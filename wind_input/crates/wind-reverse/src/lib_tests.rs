use super::*;
use std::path::PathBuf;

#[test]
fn test_strip_tone() {
    assert_eq!(strip_tone("nǐ"), "ni");
    assert_eq!(strip_tone("hǎo"), "hao");
    assert_eq!(strip_tone("lǜ"), "lv");
}

#[test]
fn test_wubi_word_code_rules() {
    let mut rl = ReverseLookup::default();
    rl.set_chaizi(vec![
        ('工', "", "aaaa"),
        ('人', "", "wwww"),
        ('大', "", "dddd"),
        ('小', "", "ihty"),
    ]);
    // 1字=全码
    assert_eq!(rl.wubi_word_code("工"), "aaaa");
    // 2字=各前2码
    assert_eq!(rl.wubi_word_code("工人"), "aaww");
    // 3字=前2字首码+末字前2码
    assert_eq!(rl.wubi_word_code("工人大"), "awdd");
    // ≥4字=前3字首码+末字首码
    assert_eq!(rl.wubi_word_code("工人大小"), "awdi");
}

fn sample_rl() -> ReverseLookup {
    let mut rl = ReverseLookup::default();
    rl.set_chaizi(vec![('好', "女子", "vbg"), ('人', "人", "w")]);
    rl.set_pinyin(vec![('好', vec!["hǎo", "hào"]), ('人', vec!["rén"])]);
    rl
}

/// 多音字读音表：「行」xíng/háng、「长」cháng/zhǎng，首项为最常用。
fn heteronym_rl() -> ReverseLookup {
    let mut rl = ReverseLookup::default();
    rl.set_pinyin(vec![
        ('行', vec!["xíng", "háng", "hàng"]),
        ('长', vec!["cháng", "zhǎng"]),
    ]);
    rl
}

/// 词条音节消歧多音字：「行长」逐字取首音会得到 `xíng cháng`，两个字都错；
/// 带上词库标注的音节 `["hang","zhang"]` 后各自筛剩唯一读音，得 `háng zhǎng`。
#[test]
fn syllables_disambiguate_heteronyms() {
    let rl = heteronym_rl();
    assert_eq!(
        rl.toned_pinyin_of("行长", None, " "),
        "xíng cháng",
        "无音节可依时只能逐字取首音（这正是要修的形态）"
    );
    assert_eq!(
        rl.toned_pinyin_of("行长", Some(&["hang", "zhang"]), " "),
        "háng zhǎng",
        "带词条音节应筛出正确读音"
    );
    // 同一个词换一组音节即换一组读音——证明筛选真的按音节走，不是碰巧。
    assert_eq!(
        rl.toned_pinyin_of("行长", Some(&["xing", "chang"]), " "),
        "xíng cháng"
    );
}

/// 声调无法从编码恢复：同音异调筛完仍剩多个，取最常用（首项）。
/// 这是数据下界而非实现缺陷 —— 拼音码里根本没有声调信息。
#[test]
fn same_syllable_different_tones_falls_back_to_most_common() {
    let rl = sample_rl(); // 好: hǎo / hào，去调都是 hao
    assert_eq!(rl.toned_pinyin_of("好", Some(&["hao"]), " "), "hǎo");
}

/// 音节数与字数对不上时整体放弃筛选、退回逐字首音，而不是按位错配——错配会给出
/// 一个看起来精确的错答案。
#[test]
fn mismatched_syllable_count_degrades_instead_of_misaligning() {
    let rl = heteronym_rl();
    // 两个字却只给一个音节：若按位取用会拿 "hang" 套到「行」、第二字越界或错位。
    assert_eq!(
        rl.toned_pinyin_of("行长", Some(&["hang"]), " "),
        "xíng cháng",
        "数量不匹配应整体降级为逐字首音"
    );
}

/// 音节与读音表全不匹配时不回退首音：不匹配意味着词库与读音表有一方过时，
/// 此时首音同样没有依据，给出它只会掩盖数据问题。
#[test]
fn unmatched_syllable_yields_nothing_for_that_char() {
    let rl = heteronym_rl();
    assert_eq!(
        rl.toned_pinyin_of("行", Some(&["zzz"]), " "),
        "",
        "读音表里没有该音节时不得拿首音充数"
    );
}

/// 简拼音节：以简拼造的词/整句，词条码里该字的音节只有声母（如「撒哈」的 `sa` + `h`），
/// 精确匹配必落空。简拼段是读音的前缀，精确不中时退而取首个以它开头的读音。
#[test]
fn abbreviated_syllable_matches_by_prefix() {
    let mut rl = ReverseLookup::default();
    rl.set_pinyin(vec![
        ('撒', vec!["sā", "sǎ"]),
        ('哈', vec!["hā", "hǎ", "hà"]),
    ]);
    assert_eq!(rl.toned_pinyin_of("撒哈", Some(&["sa", "h"]), " "), "sā hā");
    // 前缀仍按音节筛：「行」读音里只有 h 开头的 háng 能配上 `h`。
    let rl = heteronym_rl();
    assert_eq!(rl.toned_pinyin_of("行", Some(&["h"]), " "), "háng");
}

/// 反例：完整音节不走前缀回退——`an` 不得配上 `áng`。
#[test]
fn full_syllable_does_not_match_by_prefix() {
    let mut rl = ReverseLookup::default();
    rl.set_pinyin(vec![('昂', vec!["áng"])]);
    assert_eq!(rl.toned_pinyin_of("昂", Some(&["an"]), " "), "");
}

/// 无读音的字跳过（不产出空段/孤立分隔符）；整词皆无返回空串。
#[test]
fn chars_without_readings_are_skipped() {
    let rl = heteronym_rl();
    assert_eq!(rl.toned_pinyin_of("行X", None, " "), "xíng");
    assert_eq!(rl.toned_pinyin_of("XY", None, " "), "");
}

// ---------------- 注释表 ----------------

fn ct(rows: &[(&str, &str, &str)]) -> CommentTable {
    CommentTable::build(
        rows.iter()
            .map(|(t, c, k)| (t.to_string(), c.to_string(), k.to_string()))
            .collect(),
    )
}

/// 把若干「库」各写成一个 `.dict.yaml`，返回 `reload_comments` 用的 spec 列表
/// （顺序即优先级）。白名单一律留空 = 全部方案适用；要测白名单的用例自行改写。
fn write_libs(tag: &str, libs: &[&[(&str, &str, &str)]]) -> (PathBuf, Vec<(PathBuf, Vec<String>)>) {
    let dir = std::env::temp_dir().join(format!("wind_cmt_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let paths = libs
        .iter()
        .enumerate()
        .map(|(i, rows)| {
            let mut s = String::from("name: t\ncolumns:\n  - text\n  - comment\n  - code\n...\n");
            for (t, c, k) in rows.iter() {
                s.push_str(&format!("{t}\t{c}\t{k}\n"));
            }
            let p = dir.join(format!("lib{i}.dict.yaml"));
            std::fs::write(&p, s).unwrap();
            (p, Vec::new())
        })
        .collect();
    (dir, paths)
}

/// 用给定的若干库建反查表，两种后端各建一份：内存降级（`cache_dir=None`）与 mmap `.wcmt`。
/// 断言 mmap 那份确实走到 `CommentSource::Mmap`，否则缓存目录不可写时会静默降级成
/// 两份内存表，parity 测试什么都没验到。
fn rl_both(
    tag: &str,
    libs: &[&[(&str, &str, &str)]],
) -> (PathBuf, Vec<(&'static str, ReverseLookup)>) {
    let (dir, paths) = write_libs(tag, libs);
    let mut mem = ReverseLookup::default();
    mem.reload_comments(&paths, None);
    let mut mm = ReverseLookup::default();
    mm.reload_comments(&paths, Some(&dir.join("cache")));

    assert_eq!(mem.comments.len(), libs.len(), "每个库各一个 source");
    assert!(
        mm.comments
            .iter()
            .all(|s| matches!(s.body, CommentBody::Mmap(_))),
        "mmap 后端必须真的走 mmap，否则本测试退化成两份内存表的自比"
    );
    assert!(
        mem.comments
            .iter()
            .all(|s| matches!(s.body, CommentBody::Memory(_)))
    );
    (dir, vec![("内存", mem), ("mmap", mm)])
}

#[test]
fn declares_comment_column_needs_explicit_declaration() {
    //! 「自动把词库自带 comment 列当注释源」的准入判据。
    //!
    //! 与 `comment_columns` 不同：那个在无 `columns:` 声明时回落默认 `[text, comment]`，
    //! 准入若照抄，多数码表（`text\tcode\tweight`）会被判成自带注释、把整列编码当注释显示。
    let dir = std::env::temp_dir().join(format!("wind-decl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, head: &str| {
        let p = dir.join(name);
        std::fs::write(&p, format!("{head}...\n你好\tnihao\t100\n")).unwrap();
        p
    };

    let no_decl = write("plain.dict.yaml", "name: x\n");
    assert!(
        !declares_comment_column(&no_decl),
        "无 columns 声明 = 没有注释，绝不能回落默认列序"
    );

    let codetable = write(
        "code.dict.yaml",
        "name: x\ncolumns:\n  - text\n  - code\n  - weight\n",
    );
    assert!(!declares_comment_column(&codetable), "声明里没有 comment");

    let with_comment = write(
        "cmt.dict.yaml",
        "name: x\ncolumns:\n  - text\n  - code\n  - comment\n",
    );
    assert!(declares_comment_column(&with_comment), "块序列写法");

    let flow = write(
        "flow.dict.yaml",
        "name: x\ncolumns: [text, code, comment]\n",
    );
    assert!(declares_comment_column(&flow), "流式写法同样要认");

    let no_text = write("notext.dict.yaml", "name: x\ncolumns: [code, comment]\n");
    assert!(
        !declares_comment_column(&no_text),
        "缺 text 列时读不出词条，同样不合格"
    );

    assert!(
        !declares_comment_column(&dir.join("nope.dict.yaml")),
        "文件不存在不该 panic"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn declares_comment_column_stops_at_header() {
    //! 判据只看 YAML 头：正文里出现 `columns:` 或 `comment` 字样不算数。
    //! 这条同时钉住「不读整份文件」——正文里那行若被读进来，判定就会翻成 true。
    let dir = std::env::temp_dir().join(format!("wind-declh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("body.dict.yaml");
    std::fs::write(
        &p,
        "name: x\ncolumns: [text, code]\n...\ncolumns: [text, comment]\n",
    )
    .unwrap();
    assert!(
        !declares_comment_column(&p),
        "`...` 之后是正文，里面写什么都不影响列声明"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 基本点查：命中返回注释，未命中返回 None。
#[test]
fn comment_lookup_basic() {
    let t = ct(&[("苹果", "apple", ""), ("香蕉", "banana", "")]);
    assert_eq!(t.lookup_first("苹果"), Some("apple"));
    assert_eq!(t.lookup_first("香蕉"), Some("banana"));
    assert_eq!(t.lookup_first("梨"), None);
    assert_eq!(t.lookup_first(""), None);
}

/// 两种后端（内存降级 / mmap `.wcmt`）的查找语义必须逐条一致，否则差异只在缓存目录
/// 出问题时才暴露，难以复现。
#[test]
fn memory_and_mmap_backends_agree() {
    let (dir, both) = rl_both(
        "parity",
        &[&[
            ("行", "háng 行列", "tfhh"),
            ("行", "xíng 走路", "tfhx"),
            ("好", "hǎo 美好", ""),
            ("你好", "hello", ""),
            ("𠮷", "扩展区汉字", ""),
        ]],
    );
    for (name, rl) in &both {
        assert_eq!(rl.comment_of("行", Some("tfhh"), ""), "háng 行列", "{name}");
        assert_eq!(rl.comment_of("行", Some("tfhx"), ""), "xíng 走路", "{name}");
        assert_eq!(
            rl.comment_of("行", Some("hang"), ""),
            "háng 行列",
            "{name} 码不匹配回落首条"
        );
        assert_eq!(rl.comment_of("行", None, ""), "háng 行列", "{name}");
        assert_eq!(rl.comment_of("好", Some("vb"), ""), "hǎo 美好", "{name}");
        assert_eq!(rl.comment_of("你好", None, ""), "hello", "{name}");
        assert_eq!(rl.comment_of("𠮷", None, ""), "扩展区汉字", "{name}");
        assert_eq!(rl.comment_of("没有的词", None, ""), "", "{name}");
        assert_eq!(rl.comment_of("", None, ""), "", "{name}");
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 跨库仲裁：code 精确匹配优先于库顺序。靠前的库有这个词但 code 对不上时不得就地回落，
/// 写成「逐库各自先 code 后首条」的话第一个库会截胡。
#[test]
fn exact_code_wins_across_libraries() {
    let (dir, both) = rl_both(
        "cross",
        &[
            &[("行", "通用释义", "")],     // 靠前：无 code
            &[("行", "五笔专用", "tfhh")], // 靠后：精确 code
        ],
    );
    for (name, rl) in &both {
        assert_eq!(
            rl.comment_of("行", Some("tfhh"), ""),
            "五笔专用",
            "{name}：精确 code 必须越过靠前库的通用条目"
        );
        assert_eq!(
            rl.comment_of("行", None, ""),
            "通用释义",
            "{name}：无 code 时靠前库优先"
        );
        assert_eq!(
            rl.comment_of("行", Some("hang"), ""),
            "通用释义",
            "{name}：都对不上 code 时回落靠前库首条"
        );
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 源文件改动后必须读到新内容；未改动则复用缓存不重建。用 `.wcmt` 的 mtime 判断是否
/// 重建过——只断言查询结果区分不了「重建了」与「压根没缓存」。
#[test]
fn cache_reused_until_source_changes() {
    let (dir, paths) = write_libs("fresh", &[&[("甲", "旧释义", "")]]);
    let cache = dir.join("cache");

    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));
    assert_eq!(rl.comment_of("甲", None, ""), "旧释义");
    let wcmt = find_wcmt(&cache)
        .into_iter()
        .next()
        .expect("应生成 .wcmt 缓存");
    let stamp1 = std::fs::metadata(&wcmt).unwrap().modified().unwrap();

    // 源未变 → 复用缓存（不重写文件）
    drop(rl);
    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));
    assert_eq!(rl.comment_of("甲", None, ""), "旧释义");
    assert_eq!(
        std::fs::metadata(&wcmt).unwrap().modified().unwrap(),
        stamp1,
        "源未变时不该重建缓存"
    );

    // 源变更 → 必须重建并读到新内容。先释放映射：Windows 上被 mmap 的文件虽能被
    // rename 覆盖，但旧 view 会继续指向替换前的数据（见 reader_pool 的同名测试）。
    drop(rl);
    std::fs::write(
        &paths[0].0,
        "name: t\ncolumns:\n  - text\n  - comment\n  - code\n...\n甲\t新释义\t\n",
    )
    .unwrap();
    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));
    assert_eq!(
        rl.comment_of("甲", None, ""),
        "新释义",
        "源文件改了必须读到新内容——恒新鲜的缓存表现为「改了不生效，重启也没用」"
    );

    drop(rl);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `${dict:id}`：只在该 id 的库里查，不受挂载顺序影响；`${dict}` 仍取第一个命中的库。
/// 方案白名单对按库取同样生效；未知 id 得空串；同一文件被两个 id 指到时两个都认。
#[test]
fn comment_of_id_reads_one_library() {
    let (dir, mut paths) = write_libs(
        "byid",
        &[
            &[("好", "good well", "")],
            &[("好", "女子", ""), ("妈", "女马", "")],
        ],
    );
    paths[1].1 = vec!["pinyin".into()];
    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, None);
    rl.set_comment_ids(&[
        (paths[0].0.clone(), vec!["en".into()]),
        (paths[1].0.clone(), vec!["chaifen".into(), "chai".into()]),
    ]);

    assert_eq!(
        rl.comment_of("好", None, "pinyin"),
        "good well",
        "不带 id：首个命中"
    );
    assert_eq!(rl.comment_of_id("chaifen", "好", None, "pinyin"), "女子");
    assert_eq!(
        rl.comment_of_id("chai", "好", None, "pinyin"),
        "女子",
        "同库第二个 id"
    );
    assert_eq!(
        rl.comment_of_id("en", "妈", None, "pinyin"),
        "",
        "不跨库回落"
    );
    assert_eq!(rl.comment_of_id("nope", "好", None, "pinyin"), "");

    rl.set_named_only(&[paths[0].0.clone()]);
    assert_eq!(
        rl.comment_of("好", None, "pinyin"),
        "女子",
        "裸查跳过只按 id 的库"
    );
    assert_eq!(
        rl.comments_of("好", None, "pinyin"),
        vec!["女子".to_string()]
    );
    assert_eq!(
        rl.comment_of_id("en", "好", None, "pinyin"),
        "good well",
        "按 id 照取"
    );
    rl.set_named_only(&[]);
    assert_eq!(
        rl.comment_of_id("chaifen", "好", None, "wubi86"),
        "",
        "白名单照样生效"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ───────────── `${dict_all}`：全部适用库各出一条 ─────────────

/// 两库都有该词 → 按挂载顺序各出一条；`${dict}` 只回第一条。
#[test]
fn comments_of_collects_one_per_library_in_mount_order() {
    let (dir, both) = rl_both(
        "all-order",
        &[
            &[("configure", "v.配置", "")],
            &[("configure", "[IT]配置…", "")],
        ],
    );
    for (name, rl) in &both {
        assert_eq!(
            rl.comments_of("configure", None, ""),
            ["v.配置", "[IT]配置…"],
            "{name}"
        );
        assert_eq!(rl.comment_of("configure", None, ""), "v.配置", "{name}");
        assert!(rl.comments_of("nosuch", None, "").is_empty(), "{name}");
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 同一份内容挂两次（不同路径）只出一条：按字符串去重。
#[test]
fn comments_of_dedups_identical_text_across_libraries() {
    let (dir, both) = rl_both(
        "all-dedup",
        &[
            &[("好", "good", "")],
            &[("好", "good", "")],
            &[("好", "女子", "")],
        ],
    );
    for (name, rl) in &both {
        assert_eq!(rl.comments_of("好", None, ""), ["good", "女子"], "{name}");
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 同库同词多码：注释相同只出一条；注释不同按 code 只出对应那条，无 code 出首条。
#[test]
fn comments_of_gives_at_most_one_per_library() {
    let (dir, both) = rl_both(
        "all-percode",
        &[
            &[("了", "助词", "le"), ("了", "助词", "liao")],
            &[("行", "走", "xing"), ("行", "银行", "hang")],
        ],
    );
    for (name, rl) in &both {
        assert_eq!(rl.comments_of("了", Some("liao"), ""), ["助词"], "{name}");
        assert_eq!(rl.comments_of("行", Some("hang"), ""), ["银行"], "{name}");
        assert_eq!(rl.comments_of("行", Some("xing"), ""), ["走"], "{name}");
        assert_eq!(rl.comments_of("行", None, ""), ["走"], "{name}");
        assert_eq!(
            rl.comments_of("行", Some("zzz"), ""),
            ["走"],
            "{name}：code 对不上回落该库首条"
        );
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 白名单排除的库不出现——精确那遍与大小写回退那遍都一样。
#[test]
fn comments_of_respects_schema_whitelist_in_both_passes() {
    let (dir, mut paths) = write_libs(
        "all-wl",
        &[&[("apple", "n.苹果", "")], &[("apple", "[水果]苹果", "")]],
    );
    paths[0].1 = vec!["english".into()];
    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&dir.join("cache")));

    assert_eq!(
        rl.comments_of("apple", None, "english"),
        ["n.苹果", "[水果]苹果"]
    );
    assert_eq!(rl.comments_of("apple", None, "wubi86"), ["[水果]苹果"]);
    assert_eq!(
        rl.comments_of("APPLE", None, "wubi86"),
        ["[水果]苹果"],
        "大小写回退那一遍同样受白名单约束"
    );
    assert_eq!(
        rl.comments_of("APPLE", None, "english"),
        ["n.苹果", "[水果]苹果"]
    );
    drop(rl);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 大小写回退：全无精确命中时 `Configure` 能聚合两库；任一库有精确命中则不走回退。
#[test]
fn comments_of_case_fallback_only_without_exact_hits() {
    let (dir, both) = rl_both(
        "all-case",
        &[
            &[("configure", "v.配置", "")],
            &[("configure", "[IT]配置…", "")],
        ],
    );
    for (name, rl) in &both {
        assert_eq!(
            rl.comments_of("Configure", None, ""),
            ["v.配置", "[IT]配置…"],
            "{name}"
        );
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, both) = rl_both(
        "all-case-exact",
        &[
            &[("configure", "v.配置", "")],
            &[("Configure", "人名?", "")],
        ],
    );
    for (name, rl) in &both {
        assert_eq!(
            rl.comments_of("Configure", None, ""),
            ["人名?"],
            "{name}：有精确命中就不再按大小写变形去别的库捞"
        );
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 不变量：`comment_of` 的结果 ∈ `comments_of` 的结果（都空时同为空）。
#[test]
fn comment_of_is_always_among_comments_of() {
    let (dir, both) = rl_both(
        "all-inv",
        &[
            &[("行", "通用释义", ""), ("Us", "缩写", "")],
            &[("行", "五笔专用", "tfhh"), ("us", "我们", "")],
            &[("US", "美国", ""), ("行", "通用释义", "")],
        ],
    );
    let queries: &[(&str, Option<&str>)] = &[
        ("行", None),
        ("行", Some("tfhh")),
        ("行", Some("hang")),
        ("us", None),
        ("US", None),
        ("uS", None),
        ("Us", None),
        ("无", None),
    ];
    for (name, rl) in &both {
        for &(t, code) in queries {
            let one = rl.comment_of(t, code, "");
            let all = rl.comments_of(t, code, "");
            if one.is_empty() {
                assert!(all.is_empty(), "{name} {t} {code:?}: {all:?}");
            } else {
                assert!(all.contains(&one), "{name} {t} {code:?}: {one} ∉ {all:?}");
            }
        }
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 重载时仍在列表里的库原样复用，不重新校验指纹、不读源文件（切方案的高频路径）。
/// 判据是「把源文件删掉后仍能查」：重新加载会在 `cache_is_fresh` 读源失败而整库消失。
/// `Arc::ptr_eq` 不能当判据：即使不复用，`reader_pool` 也会因旧 Arc 存活交出同一指针。
#[test]
fn unchanged_libraries_are_reused_without_rereading_source() {
    let (dir, paths) = write_libs("reuse", &[&[("甲", "全局库", "")], &[("乙", "专属库", "")]]);
    let cache = dir.join("cache");
    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));
    assert_eq!(rl.comment_of("甲", None, ""), "全局库");

    // 源文件消失（等价于「这次重载没有去读它」的可观测代理）
    std::fs::remove_file(&paths[0].0).unwrap();

    // 去掉第二个库，模拟切到不挂它的方案
    rl.reload_comments(&paths[..1], Some(&cache));
    assert_eq!(
        rl.comment_of("甲", None, ""),
        "全局库",
        "未变动的库必须原样复用——一旦重新校验指纹，每次切方案都要重读整份源文件"
    );
    assert_eq!(rl.comment_of("乙", None, ""), "", "已卸载的库不再参与查询");

    drop(rl);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 卸载一个库后，它的 `.wcmt` 与指纹 sidecar 应被清掉，不在缓存目录里越积越多。
#[test]
fn removed_library_cache_is_pruned() {
    let (dir, paths) = write_libs("prune", &[&[("甲", "一号库", "")], &[("乙", "二号库", "")]]);
    let cache = dir.join("cache");
    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));
    assert_eq!(find_wcmt(&cache).len(), 2);

    // 只留第一个库：第二个的缓存应被清理，第一个的保留（不能连坐）
    rl.reload_comments(&paths[..1], Some(&cache));
    assert_eq!(find_wcmt(&cache).len(), 1, "已卸载库的缓存应被清掉");
    assert_eq!(rl.comment_of("甲", None, ""), "一号库", "保留库不受影响");
    assert_eq!(rl.comment_of("乙", None, ""), "");

    drop(rl);
    let _ = std::fs::remove_dir_all(&dir);
}

/// prune 递归下探时不得碰词库缓存：注释库缓存与词库缓存（`.wdat`/`.wdb`）共用一个根，
/// 扫到别人的文件必须原样放过。
#[test]
fn prune_never_touches_other_cache_files() {
    let (dir, paths) = write_libs("prune-foreign", &[&[("甲", "一号库", "")]]);
    let cache = dir.join("cache");
    std::fs::create_dir_all(cache.join("wubi86")).unwrap();
    let foreign = cache.join("wubi86").join("wubi86.wdat");
    std::fs::write(&foreign, b"<pretend wdat>").unwrap();
    let foreign_root = cache.join("unigram.wdb");
    std::fs::write(&foreign_root, b"<pretend wdb>").unwrap();

    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));
    // 再清一次（此时挂载列表为空，最激进的清理条件）
    rl.reload_comments(&[], Some(&cache));

    assert!(foreign.exists(), "词库缓存不得被注释库的清理波及");
    assert!(foreign_root.exists(), "缓存根下的其他文件同样不得被删");
    assert_eq!(find_wcmt(&cache).len(), 0, "自己的缓存该清干净");

    drop(rl);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 递归找出缓存根下所有 `.wcmt`（缓存按 schemas 下的目录链分了命名空间，不止一层）。
fn find_wcmt(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "wcmt") {
                out.push(p);
            }
        }
    }
    out
}

/// 同名但父目录不同的两个库：缓存靠命名空间隔离，不得互相覆盖——落到同一路径的话
/// 每次加载都会因指纹不符而互相重建。
#[test]
fn same_name_different_dirs_get_separate_caches() {
    let base = std::env::temp_dir().join(format!("wind_cmt_ns_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (a, b) = (base.join("libA"), base.join("libB"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let head = "name: t\ncolumns:\n  - text\n  - comment\n...\n";
    std::fs::write(a.join("x.dict.yaml"), format!("{head}词\tA 库\n")).unwrap();
    std::fs::write(b.join("x.dict.yaml"), format!("{head}词\tB 库\n")).unwrap();
    let paths = vec![
        (a.join("x.dict.yaml"), Vec::new()),
        (b.join("x.dict.yaml"), Vec::new()),
    ];
    let cache = base.join("cache");

    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));
    assert_eq!(find_wcmt(&cache).len(), 2, "同名库须各自持有缓存");
    assert_eq!(rl.comment_of("词", None, ""), "A 库", "靠前的库优先");

    drop(rl);
    let _ = std::fs::remove_dir_all(&base);
}

/// 大小写回退必须双向：英文库里既有小写词条也有大写缩写，只做 `to_lowercase()` 的话
/// `abc`→`ABC` 查不到。
#[test]
fn case_fallback_is_bidirectional() {
    let (dir, both) = rl_both(
        "case",
        &[&[
            ("apple", "n.苹果", ""), // 小写词条
            ("ABC", "字母表", ""),   // 大写缩写
            ("Beijing", "北京", ""), // 首字母大写的专名
            ("好", "hǎo", ""),       // 中文：不该被大小写逻辑碰到
        ]],
    );
    for (name, rl) in &both {
        // 小写词条 ← 各种输入形态
        assert_eq!(rl.comment_of("apple", None, ""), "n.苹果", "{name} 精确");
        assert_eq!(
            rl.comment_of("Apple", None, ""),
            "n.苹果",
            "{name} 首字母大写→小写"
        );
        assert_eq!(
            rl.comment_of("APPLE", None, ""),
            "n.苹果",
            "{name} 全大写→小写"
        );
        // 大写缩写 ← 小写输入（单向 to_lowercase 在这里必失败）
        assert_eq!(rl.comment_of("ABC", None, ""), "字母表", "{name} 精确");
        assert_eq!(
            rl.comment_of("abc", None, ""),
            "字母表",
            "{name} 小写→全大写"
        );
        assert_eq!(
            rl.comment_of("Abc", None, ""),
            "字母表",
            "{name} 首字母大写→全大写"
        );
        // 专名
        assert_eq!(
            rl.comment_of("beijing", None, ""),
            "北京",
            "{name} 小写→首字母大写"
        );
        // 中文与未命中
        assert_eq!(rl.comment_of("好", None, ""), "hǎo", "{name}");
        assert_eq!(rl.comment_of("nosuchword", None, ""), "", "{name}");
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `schemas` 白名单是查询期判据，不是挂载期判据。同时断言 ① 两个库都挂上了、
/// ② 查询按传入方案分流；只断言 ② 的话「挂载时筛掉」在单方案用例下照样绿。
#[test]
fn schema_whitelist_filters_at_query_time() {
    let (dir, mut paths) = write_libs("wl", &[&[("甲", "英文库", "")], &[("乙", "通用库", "")]]);
    paths[0].1 = vec!["english".into()];
    let cache = dir.join("cache");
    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));

    assert_eq!(rl.comments.len(), 2, "两个库都要挂上——过滤不在挂载层");
    assert_eq!(rl.comment_of("甲", None, "english"), "英文库");
    assert_eq!(
        rl.comment_of("甲", None, "wubi86"),
        "",
        "白名单外的方案查不到这份库"
    );
    assert_eq!(
        rl.comment_of("乙", None, "wubi86"),
        "通用库",
        "空白名单 = 全部方案"
    );
    assert_eq!(
        rl.comment_of("甲", None, ""),
        "英文库",
        "语境未知（空串）时放行——「到处都显示」比「哪都不显示」好查"
    );

    drop(rl);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 白名单必须施加到三遍扫描的每一遍：漏了大小写回退那遍，会从被排除的库里捞出注释。
#[test]
fn schema_whitelist_applies_to_case_fallback_scan() {
    let (dir, mut paths) = write_libs("wl-case", &[&[("apple", "n.苹果", "")]]);
    paths[0].1 = vec!["english".into()];
    let cache = dir.join("cache");
    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));

    assert_eq!(
        rl.comment_of("APPLE", None, "english"),
        "n.苹果",
        "本方案内回退照常"
    );
    assert_eq!(
        rl.comment_of("APPLE", None, "wubi86"),
        "",
        "大小写回退那一遍同样受白名单约束"
    );

    drop(rl);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 复用旧映射时必须更新白名单：只按路径认领会让改过 `schemas` 的库继续按旧
/// 白名单查，表现是「设置里改了适用方案，重启才生效」。
#[test]
fn reload_refreshes_whitelist_on_reused_source() {
    let (dir, mut paths) = write_libs("wl-reuse", &[&[("甲", "释义", "")]]);
    paths[0].1 = vec!["english".into()];
    let cache = dir.join("cache");
    let mut rl = ReverseLookup::default();
    rl.reload_comments(&paths, Some(&cache));
    assert_eq!(rl.comment_of("甲", None, "wubi86"), "", "先限定在 english");

    // 同一个路径、只改白名单 ⇒ 必然走「复用旧映射」那条分支
    paths[0].1.clear();
    rl.reload_comments(&paths, Some(&cache));
    assert_eq!(
        rl.comment_of("甲", None, "wubi86"),
        "释义",
        "复用的是映射而不是整份规格，白名单要当场跟上"
    );

    drop(rl);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 精确匹配优先于任何大小写回退：同时存在 `US`/`us` 时各取各的，不得互相顶替。
#[test]
fn exact_case_wins_over_fallback() {
    let (dir, both) = rl_both("case-exact", &[&[("US", "美国", ""), ("us", "我们", "")]]);
    for (name, rl) in &both {
        assert_eq!(rl.comment_of("US", None, ""), "美国", "{name}");
        assert_eq!(rl.comment_of("us", None, ""), "我们", "{name}");
        // 两者都不精确匹配时才回退，此处 Us→us（lower 先于 upper）
        assert_eq!(
            rl.comment_of("Us", None, ""),
            "我们",
            "{name} 回退顺序：小写在先"
        );
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `LowerOnly`：词的大小写已由词库确证时，回退只许把大写改小写，不许升格。
///
/// 小写 `asp`（毒蛇）与大写缩写 `ASP`（Active Server Pages）是两个词；双向回退会让前者
/// 贴上后者的释义。往小写走（`Configure` → `configure`）只是句首 / 大写锁定的形态差，
/// 照常放行。
#[test]
fn lower_only_fallback_never_raises_case() {
    let (dir, both) = rl_both(
        "case-lower-only",
        &[&[
            ("ASP", "[IT]动态服务器页面", ""),
            ("configure", "v.配置", ""),
        ]],
    );
    for (name, rl) in &both {
        let one = |t: &str| rl.comment_of_with(None, t, None, "", CaseFallback::LowerOnly);
        let all = |t: &str| rl.comments_of_with(t, None, "", CaseFallback::LowerOnly);
        assert_eq!(one("asp"), "", "{name}：小写词不得升格去命中大写缩写");
        assert!(all("asp").is_empty(), "{name}");
        assert_eq!(one("Asp"), "", "{name}：首字母大写同样不得升到全大写");
        assert_eq!(one("ASP"), "[IT]动态服务器页面", "{name}：精确照常");
        assert_eq!(one("Configure"), "v.配置", "{name}：往小写走照常");
        assert_eq!(all("CONFIGURE"), ["v.配置"], "{name}");
        // 双向档（默认）行为不变。
        assert_eq!(
            rl.comment_of("asp", None, ""),
            "[IT]动态服务器页面",
            "{name}"
        );
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);

    // 注释库同时收了小写原词：精确命中它，与大写那条无关。
    let (dir, both) = rl_both(
        "case-lower-only-both",
        &[&[("ASP", "[IT]动态服务器页面", ""), ("asp", "n.毒蛇", "")]],
    );
    for (name, rl) in &both {
        assert_eq!(
            rl.comment_of_with(None, "asp", None, "", CaseFallback::LowerOnly),
            "n.毒蛇",
            "{name}"
        );
        assert_eq!(
            rl.comments_of_with("asp", None, "", CaseFallback::LowerOnly),
            ["n.毒蛇"],
            "{name}"
        );
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 无 ASCII 字母的词不进大小写分支（中文是主路径，这条守卫是性能约束不是可选优化）。
#[test]
fn non_ascii_text_skips_case_fallbacks() {
    assert!(case_fallbacks("中文词").is_empty());
    assert!(case_fallbacks("１２３").is_empty());
    assert!(case_fallbacks("123").is_empty());
    assert!(case_fallbacks("!@#").is_empty());
    assert!(!case_fallbacks("a").is_empty(), "含字母才产出变形");
}

/// 挂载顺序即优先级（跨库）：同一个词在两个库里都有时取靠前那个。
#[test]
fn earlier_library_wins() {
    let (dir, both) = rl_both(
        "order",
        &[&[("苹果", "先挂载", "")], &[("苹果", "后挂载", "")]],
    );
    for (name, rl) in &both {
        assert_eq!(rl.comment_of("苹果", None, ""), "先挂载", "{name}");
    }
    drop(both);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 声明了 `code` 列、但某些行只写两列：这些行必须照常入表，code 列不计入最少列数。
#[test]
fn rows_without_code_survive_when_code_column_declared() {
    let rows =
        parse_str("columns:\n  - text\n  - comment\n  - code\n...\n甲\t有码\tlhnh\n乙\t无码\n");
    assert_eq!(
        rows,
        vec![
            ("甲".to_string(), "有码".to_string(), "lhnh".to_string()),
            ("乙".to_string(), "无码".to_string(), String::new()),
        ]
    );
}

/// 挂载顺序即优先级：同词同码时保留首次出现的那条。依赖 `build` 用稳定排序，
/// 换成 `sort_unstable_by` 的话优先级会随输入规模随机翻转。
#[test]
fn earlier_source_wins_on_duplicate() {
    let t = ct(&[("行", "第一份", ""), ("行", "第二份", "")]);
    assert_eq!(t.lookup_first("行"), Some("第一份"));
    assert_eq!(t.len(), 1, "同词同码只保留一条");
}

/// code 消歧：同词不同码各存一条，按 code 精确匹配。
#[test]
fn code_disambiguates_same_text() {
    let t = ct(&[("行", "háng 行列", "tfhh"), ("行", "xíng 走路", "tfhx")]);
    assert_eq!(t.len(), 2, "同词不同码应各存一条");
    assert_eq!(t.lookup_by_code("行", "tfhh"), Some("háng 行列"));
    assert_eq!(t.lookup_by_code("行", "tfhx"), Some("xíng 走路"));
}

/// code 对不上时回落该词首条，而不是返回空：跨方案挂同一份注释库是常态
/// （五笔码 `tfhh` 对拼音候选的 `hang` 必然不匹配）。
#[test]
fn unmatched_code_falls_back_to_first_entry() {
    let t = ct(&[("行", "háng 行列", "tfhh"), ("行", "xíng 走路", "tfhx")]);
    assert_eq!(t.lookup_by_code("行", "hang"), None, "对不上即未命中");
    assert_eq!(t.lookup_first("行"), Some("háng 行列"), "由调用方回落首条");
    assert_eq!(t.lookup_by_code("行", ""), None, "空 code 不参与消歧");
}

/// 无 code 的通用条目与带 code 的条目并存：带 code 者按码命中，其余回落首条。
#[test]
fn generic_and_coded_entries_coexist() {
    let t = ct(&[("行", "通用", ""), ("行", "五笔专用", "tfhh")]);
    assert_eq!(t.len(), 2);
    assert_eq!(t.lookup_by_code("行", "tfhh"), Some("五笔专用"));
    assert_eq!(
        t.lookup_first("行"),
        Some("通用"),
        "首条是无 code 的通用条目"
    );
}

/// 二分边界：首条 / 末条 / 排序键相邻的词都要能查到。
#[test]
fn comment_lookup_covers_binary_search_edges() {
    let t = ct(&[
        ("a", "A", ""),
        ("ab", "AB", ""),
        ("b", "B", ""),
        ("z", "Z", ""),
    ]);
    for (k, v) in [("a", "A"), ("ab", "AB"), ("b", "B"), ("z", "Z")] {
        assert_eq!(t.lookup_first(k), Some(v), "查 {k}");
    }
    assert_eq!(
        t.lookup_first("aa"),
        None,
        "落在 a 与 ab 之间的词不得误命中"
    );
    assert_eq!(t.lookup_first("zz"), None, "越过末条不得越界");
}

/// 多字节键（中文）在 arena 里按字节切分，不得切在 UTF-8 中间。
#[test]
fn multibyte_keys_are_sliced_safely() {
    let t = ct(&[("你好", "hello", "wqvb"), ("世界", "world", "")]);
    assert_eq!(t.lookup_first("你好"), Some("hello"));
    assert_eq!(t.lookup_first("世界"), Some("world"));
}

// ---------------- 注释库解析 ----------------

fn parse_str(content: &str) -> Vec<(String, String, String)> {
    // 文件名必须每次调用都唯一，不能按内容取名：样本相同的测试并行跑时会互删对方的文件。
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "wind_comment_test_{}_{}.dict.yaml",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&p, content).unwrap();
    let r = parse_comment_dict(&p).unwrap();
    let _ = std::fs::remove_file(&p);
    r
}

/// 行尾不挑食：注释库无论 LF / CRLF / 孤立 CR，解析结果一致（孤立 `\r` 不规整的话
/// 找不到正文起点、一条注释都出不来）。样本在代码里构造，别改用仓库 fixture（autocrlf 会改行尾）。
#[test]
fn line_endings_do_not_change_the_result() {
    let lf = "name: x\n...\n苹果\tapple\n香蕉\tbanana\n";
    let want = parse_str(lf);
    assert_eq!(want.len(), 2, "样本本身要能解析出两条");
    for (tag, text) in [
        ("CRLF", lf.replace('\n', "\r\n")),
        ("CR", lf.replace('\n', "\r")),
    ] {
        assert_eq!(parse_str(&text), want, "{tag}: 结果与 LF 版不同");
    }
}

/// 默认列序 `[text, comment]`（无 columns 声明）。
#[test]
fn parse_defaults_to_text_comment() {
    let rows = parse_str("name: x\n...\n苹果\tapple\n香蕉\tbanana\n");
    assert_eq!(
        rows,
        vec![
            ("苹果".into(), "apple".into(), String::new()),
            ("香蕉".into(), "banana".into(), String::new()),
        ]
    );
}

/// 显式 columns 声明（流式与块式两种写法都要认）+ code 列。
#[test]
fn parse_honors_columns_declaration() {
    let flow = parse_str("columns: [text, code, comment]\n...\n行\ttfhh\thang\n");
    assert_eq!(flow, vec![("行".into(), "hang".into(), "tfhh".into())]);

    let block = parse_str("columns:\n  - text\n  - code\n  - comment\n...\n行\ttfhh\thang\n");
    assert_eq!(block, flow, "块式与流式声明结果须一致");
}

/// 声明里缺 comment（或缺 text）→ 整库跳过。没有注释的注释库是配置错误，
/// 静默当空会让人以为是路径问题。
#[test]
fn parse_skips_library_without_comment_column() {
    assert!(parse_str("columns: [text, code]\n...\n行\ttfhh\n").is_empty());
    assert!(parse_str("columns: [comment]\n...\nx\n").is_empty());
}

/// `#` 注释行、空行、列数不足的行一律跳过；空 text / 空 comment 同样跳过。
#[test]
fn parse_skips_comments_and_incomplete_rows() {
    let rows = parse_str("...\n# 这是注释\n\n只有一列\n苹果\tapple\n\tempty_text\n梨\t\n");
    assert_eq!(rows, vec![("苹果".into(), "apple".into(), String::new())]);
}

/// 无 YAML 头的裸 TSV 也能读（容许用户直接丢一张两列表）。
#[test]
fn parse_accepts_headerless_tsv() {
    let rows = parse_str("苹果\tapple\n");
    assert_eq!(rows, vec![("苹果".into(), "apple".into(), String::new())]);
}

/// 裸表按 OpenCC 约定读：丢掉重复键本身的值首项、跳过 `---` 注释行；首项不是键本身的
/// 原样保留（「好人」不是「好」的重复）；只剩键本身的行没有注释，跳过。
#[test]
fn parse_reads_bare_opencc_tables() {
    let rows = parse_str(
        "----------格式为：输入内容\t----------格式为：输入内容 \n\
             好\t好 good well\n\
             好人\tgood person\n\
             好\t好人 的首项不是键\n\
             空\t空\n",
    );
    let got: Vec<(&str, &str)> = rows.iter().map(|r| (r.0.as_str(), r.1.as_str())).collect();
    assert_eq!(
        got,
        [
            ("好", "good well"),
            ("好人", "good person"),
            ("好", "好人 的首项不是键"),
        ]
    );
}

/// 有 `...` 的 rime 形态不套 OpenCC 约定：注释本来就可能以词本身开头。
#[test]
fn parse_keeps_rime_form_comment_verbatim() {
    let rows = parse_str("...\n好\t好 good\n");
    assert_eq!(rows[0].1, "好 good");
}

/// 注释内容含空格/标点原样保留（只按 `\t` 切列，不 trim 内容）。
#[test]
fn parse_preserves_comment_content() {
    let rows = parse_str("...\n行\txíng; 走路 / háng; 行列\n");
    assert_eq!(rows[0].1, "xíng; 走路 / háng; 行列");
}

/// 整词字根串：逐字拼接，无拆字数据的字跳过。
#[test]
fn radicals_of_joins_per_char() {
    let rl = sample_rl();
    assert_eq!(rl.radicals_of("好", " "), "女子");
    assert_eq!(rl.radicals_of("好人", " "), "女子 人");
    assert_eq!(rl.radicals_of("好X", " "), "女子", "无字根的字跳过");
    assert_eq!(rl.radicals_of("XY", " "), "");
}

/// 整词编码串：与 `radicals_of` 对称，逐字取拆字库里记录的编码而非字根，
/// 无编码的字同样跳过。
#[test]
fn codes_of_joins_per_char() {
    let rl = sample_rl();
    assert_eq!(rl.codes_of("好", " "), "vbg");
    assert_eq!(rl.codes_of("好人", " "), "vbg w");
    assert_eq!(rl.codes_of("好X", " "), "vbg", "无编码的字跳过");
    assert_eq!(rl.codes_of("XY", " "), "");
}

#[test]
fn test_chaizi_table_lookup_and_dup_last_wins() {
    // 乱序输入 + 同字重复：查询按二分命中，重复取文件靠后者。
    let t = ChaiziTable::build(vec![
        ('乙', "乙", "nnll"),
        ('甲', "田", "old"),
        ('甲', "田", "lhnh"),
        ('丙', "", "gmw"),
    ]);
    assert_eq!(t.len(), 3, "重复字应合并");
    assert_eq!(t.lookup('甲'), Some(("田", "lhnh")), "同字取靠后行");
    assert_eq!(t.code('乙'), Some("nnll"));
    assert_eq!(t.radicals('丙'), None, "空字根应视为无");
    assert_eq!(t.code('丙'), Some("gmw"));
    assert_eq!(t.lookup('丁'), None);
}

#[test]
fn test_reload_chaizi_none_clears() {
    let mut rl = sample_rl();
    assert!(!rl.chaizi.is_empty());
    rl.reload_chaizi(None);
    assert!(rl.chaizi.is_empty(), "None 应清空拆字表");
    assert!(!rl.pinyin.is_empty(), "拼音表不受影响");
    assert_eq!(rl.wubi_word_code("好"), "");
}

#[test]
fn test_gen_pinyin_uses_first_reading() {
    let mut rl = ReverseLookup::default();
    // 多音字"重"：首音 zhòng（最常用），次音 chóng
    rl.set_pinyin(vec![('重', vec!["zhòng", "chóng"]), ('要', vec!["yào"])]);
    assert_eq!(rl.gen_pinyin("重要"), "zhong yao");
}

/// `readings_of`：读音按表内顺序（最常用在前）、`max` 截断、0 = 不限；查不到给空串。
/// 悬停提示 `${readings[:N]}` 的全部口径都在这里。
#[test]
fn readings_of_orders_limits_and_joins() {
    let mut rl = ReverseLookup::default();
    rl.set_pinyin(vec![('重', vec!["zhòng", "chóng"])]);
    assert_eq!(rl.readings_of('重', 0, "/"), "zhòng/chóng");
    assert_eq!(rl.readings_of('重', 1, "/"), "zhòng");
    assert_eq!(
        rl.readings_of('重', 5, "/"),
        "zhòng/chóng",
        "N 大于读音数不补"
    );
    assert_eq!(rl.readings_of('重', 0, " "), "zhòng chóng");
    assert_eq!(rl.readings_of('无', 0, "/"), "");
}

#[test]
fn test_load_pinyin_parses_multi_reading() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("wind-reverse-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pinyin_map.txt");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "# 头部注释").unwrap();
    writeln!(f, "U+4E00: yī  # 一").unwrap();
    writeln!(f, "U+91CD: zhòng,chóng  # 重").unwrap();
    drop(f);

    let mut rl = ReverseLookup::default();
    rl.load_pinyin(&path);
    assert_eq!(
        rl.pinyin.readings('一').unwrap().iter().collect::<Vec<_>>(),
        vec!["yī"]
    );
    assert_eq!(
        rl.pinyin.readings('重').unwrap().iter().collect::<Vec<_>>(),
        vec!["zhòng", "chóng"]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_pinyin_table_lookup_and_dup_last_wins() {
    // 与拆字表对称：乱序输入 + 同字重复，查询按二分命中，重复取靠后者。
    let t = PinyinTable::build(vec![
        ('乙', vec!["yǐ"]),
        ('甲', vec!["jiǎ"]),
        ('甲', vec!["jiá", "jiǎ"]),
        ('丙', vec!["bǐng"]),
    ]);
    assert_eq!(t.entries.len(), 3, "重复字应合并");
    assert_eq!(
        t.readings('甲').unwrap().iter().collect::<Vec<_>>(),
        vec!["jiá", "jiǎ"],
        "同字取靠后行"
    );
    // 相邻条目的读音区间不串味（甲有 2 个读音，其后的乙仍应只取自己那条）
    assert_eq!(t.readings('乙').unwrap().first(), Some("yǐ"));
    assert_eq!(t.readings('乙').unwrap().len(), 1);
    assert_eq!(t.readings('丙').unwrap().first(), Some("bǐng"));
    assert!(t.readings('丁').is_none());
}
