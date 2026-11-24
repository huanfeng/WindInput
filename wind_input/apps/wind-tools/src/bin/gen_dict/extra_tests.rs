use super::*;
use crate::config::ExtraConfig;

#[test]
fn emoji_wins_over_cjk_for_mixed_text() {
    // 关掉 emoji 库时，带中文备注的表情条目也该一起消失
    assert_eq!(classify("🐶狗"), Category::Emoji);
    assert_eq!(classify("狗"), Category::Cjk);
}

#[test]
fn english_requires_letters_not_just_ascii() {
    assert_eq!(classify("api"), Category::English);
    assert_eq!(classify("+++"), Category::Symbol, "纯符号不算英文");
    assert_eq!(classify("№"), Category::Symbol, "非 ASCII 非 CJK 落符号桶");
}

#[test]
fn custom_emoji_weights_descend_from_200() {
    let p = std::env::temp_dir().join("gen_dict_emoji_test.txt");
    std::fs::write(&p, "😂\n# 注释\n🤣\n😊\n").unwrap();
    let v = load_custom_emoji(Some(&p)).unwrap();
    assert_eq!(v.len(), 3, "注释行不计入");
    assert_eq!((v[0].weight, v[1].weight, v[2].weight), (200, 199, 198));
    assert!(v.iter().all(|e| e.code == "emoj"));
}

#[test]
fn custom_emoji_weight_never_drops_below_one() {
    let p = std::env::temp_dir().join("gen_dict_emoji_many.txt");
    let content: String = (0..250).map(|_| "😀\n").collect();
    std::fs::write(&p, content).unwrap();
    let v = load_custom_emoji(Some(&p)).unwrap();
    assert!(v.iter().all(|e| e.weight >= 1), "超过 200 条后须夹在 1");
}

#[test]
fn missing_custom_emoji_file_is_not_an_error() {
    assert!(
        load_custom_emoji(Some(Path::new("/definitely/not/here.txt")))
            .unwrap()
            .is_empty()
    );
    assert!(load_custom_emoji(None).unwrap().is_empty());
}

fn wubi_codes() -> CharCodes {
    let mut m = CharCodes::new();
    for (c, code) in [
        ('足', "khu"),
        ('球', "gfi"),
        ('哭', "kkdu"),
        ('脸', "ewgi"),
        ('鑫', "qqqf"), // 反查表里有，但下面的「饕」没有
    ] {
        m.insert(c, code.into());
    }
    m
}

fn write_tmp(name: &str, content: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn named_emoji_reverses_code_and_ranks_tts_above_keyword() {
    let p = write_tmp(
        "gen_dict_named_ok.txt",
        "# 注释\n⚽\t足球\ttts\n⚽\t球\tkw\n",
    );
    let mut log = |_: String| {};
    let v = load_named_emoji(Some(&p), &wubi_codes(), &mut log).unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!((v[0].code.as_str(), v[0].weight), ("khgf", 130), "tts 档");
    assert_eq!(
        (v[1].code.as_str(), v[1].weight),
        ("gfi", 110),
        "keyword 档"
    );
    assert!(
        v[0].weight > v[1].weight,
        "同码内 tts 必须排在 keyword 之前"
    );
}

#[test]
fn named_emoji_skips_unreversible_names_without_producing_wrong_code() {
    // 「饕餮」不在反查表里 → 整条跳过，绝不产出错码
    let p = write_tmp("gen_dict_named_skip.txt", "🍖\t饕餮\ttts\n⚽\t足球\ttts\n");
    let mut msgs = Vec::new();
    let mut log = |s: String| msgs.push(s);
    let v = load_named_emoji(Some(&p), &wubi_codes(), &mut log).unwrap();
    assert_eq!(v.len(), 1, "只剩可反查的那条");
    assert_eq!(v[0].text, "⚽");
    assert!(
        msgs.iter().any(|m| m.contains("无法反查编码")),
        "跳过必须留下日志，否则静默丢词无从察觉"
    );
}

#[test]
fn named_emoji_tolerates_malformed_lines() {
    let p = write_tmp(
        "gen_dict_named_bad.txt",
        "只有一列\n\t足球\ttts\n⚽\t\ttts\n⚽\t足球\ttts\n",
    );
    let mut log = |_: String| {};
    let v = load_named_emoji(Some(&p), &wubi_codes(), &mut log).unwrap();
    assert_eq!(v.len(), 1, "三种残行都跳过，只留合法行");
}

#[test]
fn named_emoji_missing_file_is_not_an_error() {
    let mut log = |_: String| {};
    assert!(
        load_named_emoji(
            Some(Path::new("/definitely/not/here.txt")),
            &wubi_codes(),
            &mut log
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        load_named_emoji(None, &wubi_codes(), &mut log)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn dedup_keeps_highest_weight_and_normalizes_vs16() {
    // ⚽(U+26BD) 与 ⚽️(U+26BD FE0F) 渲染完全相同，必须视为同一条
    let mut list = vec![
        Entry::new("\u{26BD}\u{FE0F}".into(), "khgf".into(), 100, 0), // 上游
        Entry::new("\u{26BD}".into(), "khgf".into(), 130, 1),         // named tts
    ];
    let removed = dedup_emoji_entries(&mut list);
    assert_eq!(removed, 1);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].weight, 130, "保留权重高的");
    assert_eq!(list[0].text, "\u{26BD}", "胜出者保留自己的规范形态");
}

#[test]
fn dedup_removes_upstream_self_duplicates() {
    // 上游 extra 本身带完全重复的行（实测 53 条），同权时只留一条
    let mut list = vec![
        Entry::new("😭".into(), "ddkk".into(), 100, 0),
        Entry::new("😭".into(), "ddkk".into(), 100, 1),
    ];
    assert_eq!(dedup_emoji_entries(&mut list), 1);
    assert_eq!(list.len(), 1);
}

#[test]
fn dedup_keeps_distinct_emoji_under_same_code() {
    // 同码不同 emoji 是正常的多候选，不能被去重误伤
    let mut list = vec![
        Entry::new("⚽".into(), "gfi".into(), 110, 0),
        Entry::new("🏀".into(), "gfi".into(), 110, 1),
        Entry::new("⚽".into(), "khgf".into(), 130, 2),
    ];
    assert_eq!(dedup_emoji_entries(&mut list), 0);
    assert_eq!(list.len(), 3);
}

#[test]
fn output_path_derives_sibling_names() {
    let p = Path::new("/out/wubi86_jidian.dict.yaml");
    assert_eq!(
        extra_output_path(p, "wubi86_jidian", "emoji"),
        Path::new("/out/wubi86_jidian_emoji.dict.yaml")
    );
}

/// 分档本身（压缩关闭时的原始语义）。
///
/// 必须单独验一次：开启压缩后所有权重都落进 `[1,119]`，档位差被压得只剩几个点，
/// 拿压缩后的值断言分档，等于让压缩掩盖掉分档写错——两件事要各测各的。
#[test]
fn cjk_bucket_falls_back_by_priority_when_unigram_misses() {
    let cfg = Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        extra: ExtraConfig {
            weight_max: 0, // 0 = 不压缩，看分档原值
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buckets = vec![(
        Category::Cjk,
        vec![
            Entry::new("罕见词".into(), "abcd".into(), 30, 0), // 有原始优先级
            Entry::new("无权重".into(), "abce".into(), 0, 1),  // 无原始权重
        ],
    )];
    assign_weights(&mut buckets, &Unigram::new(), 3.0, &cfg);
    assert_eq!(buckets[0].1[0].weight, 180, "priority_30 档");
    assert_eq!(buckets[0].1[1].weight, 120, "无原始权重落 priority_10");
}

/// 扩展库整库必须落在主库最低档之下——这是「扩展库排在主库之后」的**唯一**保证。
/// 引擎侧的 `base_order` 排在 `weight` 之后，只是等权 tiebreaker，压不住高权重词条。
#[test]
fn cjk_bucket_compressed_below_main_dict_floor() {
    let cfg = Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        ..Default::default()
    };
    let mut buckets = vec![(
        Category::Cjk,
        vec![
            Entry::new("高频".into(), "abcd".into(), 30, 0),
            Entry::new("中频".into(), "abce".into(), 20, 1),
            Entry::new("低频".into(), "abcf".into(), 0, 2),
        ],
    )];
    assign_weights(&mut buckets, &Unigram::new(), 3.0, &cfg);
    let ws: Vec<i64> = buckets[0].1.iter().map(|e| e.weight).collect();
    for (e, w) in buckets[0].1.iter().zip(&ws) {
        assert!(
            *w <= cfg.extra.weight_max && *w < cfg.fallback.priority_10,
            "{} 权重 {w} 必须低于主库最低档 {}",
            e.text,
            cfg.fallback.priority_10
        );
        assert!(*w >= 1, "{} 权重 {w} 不得压到 0 以下", e.text);
    }
    // 压缩是线性的：相对次序必须原样保留，否则库内排序就被压坏了。
    assert!(ws[0] > ws[1] && ws[1] > ws[2], "相对次序应保持: {ws:?}");
}

/// 全库同权时不得除零，且整体落在带顶（相对序此时本就由文件序承载）。
#[test]
fn cjk_bucket_uniform_weights_do_not_divide_by_zero() {
    let cfg = Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        ..Default::default()
    };
    let mut buckets = vec![(
        Category::Cjk,
        vec![
            Entry::new("甲".into(), "abcd".into(), 0, 0),
            Entry::new("乙".into(), "abce".into(), 0, 1),
        ],
    )];
    assign_weights(&mut buckets, &Unigram::new(), 3.0, &cfg);
    assert_eq!(buckets[0].1[0].weight, cfg.extra.weight_max);
    assert_eq!(buckets[0].1[1].weight, cfg.extra.weight_max);
}

#[test]
fn non_cjk_buckets_keep_original_weight() {
    let cfg = Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        ..Default::default()
    };
    let mut buckets = vec![(
        Category::English,
        vec![
            Entry::new("api".into(), "abcd".into(), 555, 0),
            Entry::new("sdk".into(), "abce".into(), 0, 1),
        ],
    )];
    assign_weights(&mut buckets, &Unigram::new(), 3.0, &cfg);
    assert_eq!(buckets[0].1[0].weight, 555, "有原权重则保留");
    assert_eq!(buckets[0].1[1].weight, 100, "无权重给 extra.default_weight");
}
