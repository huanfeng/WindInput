use super::*;

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

/// 临时词导入必须认 `WordIo::boundary`，而不只认 code 里的空格。
///
/// 样本特意选单音节：`join_code_by_boundary("xian", 0b1)` 仍是无空格的 `xian`，空格载体
/// 表达不了单音节边界，只认空格的实现会静默丢掉闸口求出的边界。对照组 `gong` 钉住
/// 不给显式边界时行为不变。
#[test]
fn temp_word_import_honors_explicit_boundary() {
    let path = tmp("wind_tw_explicit_b.redb");
    let s = Store::open(&path).unwrap();
    let row = |code: &str, text: &str, b: Option<u64>| wdict::WordIo {
        code: code.into(),
        text: text.into(),
        weight: 50,
        count: 1,
        boundary: b,
    };
    s.import_temp_word_rows(
        "pinyin",
        &[
            row("xian", "先", Some(0b1)),
            row("gong", "工", None),
            row("nihao", "你好", Some(0b101)),
        ],
    )
    .unwrap();
    let b = |code: &str, text: &str| {
        s.get_temp_words("pinyin", code)
            .unwrap()
            .into_iter()
            .find(|r| r.text == text)
            .unwrap()
            .boundary
    };
    assert_eq!(b("xian", "先"), 0b1, "单音节边界必须存活");
    assert_eq!(
        b("gong", "工"),
        0b1,
        "不给显式边界：拼音文本里单个汉字按单音节读（与用户词文本同一规则，P5）"
    );
    assert_eq!(b("nihao", "你好"), 0b101);
    let _ = std::fs::remove_file(&path);
}

/// 临时词的每一条写路径都要维护索引：主表写了、索引没写，该词简拼就静默召不回。
#[test]
fn every_temp_write_path_maintains_the_index() {
    let p = tmp("wind_temp_abbrev_paths.redb");
    let s = Store::open(&p).unwrap();
    let hit = |ab: &str| -> Vec<String> {
        s.search_temp_words_by_abbrev("py", ab, 0)
            .unwrap()
            .into_iter()
            .map(|r| r.text)
            .collect()
    };

    // ① learn_temp_word
    s.learn_temp_word("py", "nihao", "你好", 500, 0b101)
        .unwrap();
    assert_eq!(hit("nh"), vec!["你好"], "learn 应建索引");

    // ② import_temp_word_rows
    s.import_temp_word_rows(
        "py",
        &[wdict::WordIo {
            code: "xi an ning".into(),
            text: "西安宁".into(),
            weight: 700,
            count: 3,
            boundary: None,
        }],
    )
    .unwrap();
    assert_eq!(hit("xan"), vec!["西安宁"], "import 应建索引");

    // ③ remove_temp_word
    s.remove_temp_word("py", "xianning", "西安宁").unwrap();
    assert!(hit("xan").is_empty(), "remove 应删索引");

    // ④ evict_temp_words：淘汰排序主键是 count。让"你好"多用一次，结果就不依赖两次
    // learn 是否落在同一秒（created_at 打平时退到 weight，慢 CI 跨秒会偶发翻车）。
    s.learn_temp_word("py", "nihao", "你好", 0, 0b101).unwrap();
    s.learn_temp_word("py", "haoya", "好呀", 10, 0b1001)
        .unwrap();
    assert_eq!(hit("hy"), vec!["好呀"]);
    s.evict_temp_words("py", 1).unwrap();
    assert!(hit("hy").is_empty(), "evict 应同步删索引");
    assert_eq!(hit("nh"), vec!["你好"], "count 更高者留下，索引也应留下");

    // ⑤ promote_temp_word：临时索引删、**用户索引建**
    assert!(s.promote_temp_word("py", "nihao", "你好").unwrap());
    assert!(hit("nh").is_empty(), "晋升后不该还留在临时索引里");
    assert_eq!(
        s.search_user_words_by_abbrev("py", "nh", 0)
            .unwrap()
            .into_iter()
            .map(|r| r.text)
            .collect::<Vec<_>>(),
        vec!["你好"],
        "晋升写的是用户词表，用户索引必须跟着建——这条路径住在 temp_words.rs 里，最易漏"
    );

    // ⑥ clear_temp_words
    s.learn_temp_word("py", "zaijian", "再见", 500, 0b1001)
        .unwrap();
    s.clear_temp_words("py").unwrap();
    assert!(hit("zj").is_empty(), "clear 应清索引");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn temp_words_wdict_roundtrip_and_clear() {
    let path = tmp("wind_tw_io.redb");
    let s = Store::open(&path).unwrap();
    s.learn_temp_word("wb", "ab", "临时", 50, 0).unwrap();
    s.learn_temp_word("py", "ni", "你", 10, 0).unwrap();
    let text = s.export_temp_words_wdict("wb", "t").unwrap();
    assert!(text.contains("--- !words"));

    let path2 = tmp("wind_tw_io2.redb");
    let s2 = Store::open(&path2).unwrap();
    let (imported, skipped) = s2.import_temp_words_wdict("wb", &text).unwrap();
    assert_eq!((imported, skipped), (1, 0));
    assert_eq!(s2.get_temp_word("wb", "ab", "临时").unwrap(), Some(1));

    assert_eq!(s.clear_temp_words("wb").unwrap(), 1);
    assert!(s.search_temp_words_prefix("wb", "", 0).unwrap().is_empty());
    assert_eq!(
        s.search_temp_words_prefix("py", "", 0).unwrap().len(),
        1,
        "其它 schema 不受影响"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

#[test]
fn learn_count_reaches_threshold_then_promote() {
    let path = tmp("wind_tw_promote_thresh.redb");
    let s = Store::open(&path).unwrap();
    for i in 1..=3u32 {
        let n = s.learn_temp_word("wubi86", "abcd", "测试", 100, 0).unwrap();
        assert_eq!(n, i);
    }
    assert_eq!(
        s.get_temp_word("wubi86", "abcd", "测试").unwrap(),
        Some(3),
        "3 次学习后 count 应为 3"
    );
    assert!(s.promote_temp_word("wubi86", "abcd", "测试").unwrap());
    assert_eq!(
        s.get_temp_word("wubi86", "abcd", "测试").unwrap(),
        None,
        "晋升后临时层应删除"
    );
    // 不存在的词返回 None
    assert_eq!(s.get_temp_word("wubi86", "xxxx", "无").unwrap(), None);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_learn_count_only_weight_unchanged() {
    let path = tmp("wind_tw_learn.redb");
    let s = Store::open(&path).unwrap();
    assert_eq!(s.learn_temp_word("wb", "a", "工", 800, 0).unwrap(), 1);
    assert_eq!(s.learn_temp_word("wb", "a", "工", 800, 0).unwrap(), 2);
    let r = s.get_temp_words("wb", "a").unwrap();
    assert_eq!(r[0].count, 2);
    assert_eq!(r[0].weight, 800, "权重不再随复选累加，保持写入初值");
    // 初值上限
    let _ = s.learn_temp_word("wb", "b", "戈", 99999, 0).unwrap();
    assert_eq!(
        s.get_temp_words("wb", "b").unwrap()[0].weight,
        TEMP_WORD_MAX_WEIGHT
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_increment_if_exists() {
    let path = tmp("wind_tw_inc.redb");
    let s = Store::open(&path).unwrap();
    assert_eq!(
        s.increment_temp_if_exists("wb", "a", "工").unwrap(),
        (false, 0)
    );
    s.learn_temp_word("wb", "a", "工", 100, 0).unwrap();
    assert_eq!(
        s.increment_temp_if_exists("wb", "a", "工").unwrap(),
        (true, 2)
    );
    assert_eq!(
        s.get_temp_words("wb", "a").unwrap()[0].weight,
        100,
        "计数不再驱动权重变化"
    );
    let _ = std::fs::remove_file(&path);
}

/// 用过的词不能和没用过的词一起被随机淘汰。
///
/// 三条词权重全同、且字典序与使用次数相反（用得最多的是 "a"）：排序键一旦退回纯 weight，
/// 实际顺序就是 redb 的 key 字典序，用了 3 次的词会被删掉。
#[test]
fn evict_keeps_the_used_word_when_weights_are_all_equal() {
    let path = tmp("wind_tw_evict_count.redb");
    let s = Store::open(&path).unwrap();
    for _ in 0..3 {
        s.learn_temp_word("wb", "a", "常用", 800, 0).unwrap(); // count=3
    }
    s.learn_temp_word("wb", "b", "没用过一", 800, 0).unwrap(); // count=1
    s.learn_temp_word("wb", "c", "没用过二", 800, 0).unwrap(); // count=1
    assert_eq!(s.evict_temp_words("wb", 1).unwrap(), 2);
    assert!(
        !s.get_temp_words("wb", "a").unwrap().is_empty(),
        "count=3 的词必须留下"
    );
    assert!(s.get_temp_words("wb", "b").unwrap().is_empty());
    assert!(s.get_temp_words("wb", "c").unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

/// count 与 weight 都相同时按 `created_at` 升序淘汰（先造的先走）。
///
/// 自动造词产出的 `count=1` + `weight=800` 那批在前两个键上全等，只剩 `created_at` 定序。
/// `learn_temp_word` 取当下秒数且沿用已有记录的旧值，故直接构造隔秒的两条。
#[test]
fn evict_falls_back_to_created_at_when_count_and_weight_tie() {
    let path = tmp("wind_tw_evict_ca.redb");
    let s = Store::open(&path).unwrap();
    // 该淘汰的 zz_old 字典序在后：排序键漏掉 created_at 时，删掉的会变成 aa_new。
    use wind_userdata::engine::Op;
    use wind_userdata::record::{OpKind, Payload, Target};
    let mut ops = Vec::new();
    for (code, text, ca) in [("aa_new", "新造", 100i64), ("zz_old", "老词", 50)] {
        for (kind, payload) in [
            (OpKind::Upsert, Payload::Weight(800)),
            (OpKind::Incr, Payload::Count(1)),
        ] {
            let mut op = Op::new(Target::Temp, "wb", kind, code, text, payload, 0);
            op.ts_ms = ca * 1000;
            ops.push(op);
        }
    }
    s.runtime().data().apply_batch(&ops).unwrap();
    assert_eq!(s.evict_temp_words("wb", 1).unwrap(), 1);
    assert!(
        s.get_temp_words("wb", "zz_old").unwrap().is_empty(),
        "created_at 更早的那条该被淘汰"
    );
    assert!(!s.get_temp_words("wb", "aa_new").unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

/// 没删到词就不能动代次：代次一变，`UserTextIndex` / 联想索引就整表重扫用户词。
/// 协调器每 64 次造词调一次淘汰，多数时候词数远在上限内，空提交照样 bump 就等于
/// 每 64 次造词白白重建一轮。
#[test]
fn evict_without_deletion_keeps_words_generation() {
    let path = tmp("wind_tw_evict_nogen.redb");
    let s = Store::open(&path).unwrap();
    s.learn_temp_word("wb", "a", "甲", 800, 0).unwrap();
    let before = s.words_generation_of("wb");
    assert_eq!(s.evict_temp_words("wb", 10).unwrap(), 0);
    assert_eq!(s.words_generation_of("wb"), before, "未删词却 bump 了代次");
    assert_eq!(s.evict_temp_words("wb", 0).unwrap(), 1);
    assert_ne!(s.words_generation_of("wb"), before, "删了词必须 bump 代次");
    let _ = std::fs::remove_file(&path);
}

/// 晋升落空（临时词已不在）同样不能动代次，理由同上。
#[test]
fn promote_missing_temp_word_keeps_words_generation() {
    let path = tmp("wind_tw_promote_nogen.redb");
    let s = Store::open(&path).unwrap();
    let before = s.words_generation_of("wb");
    assert!(!s.promote_temp_word("wb", "zz", "无").unwrap());
    assert_eq!(s.words_generation_of("wb"), before);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_evict_lowest_weight() {
    let path = tmp("wind_tw_evict.redb");
    let s = Store::open(&path).unwrap();
    s.learn_temp_word("wb", "a", "低", 10, 0).unwrap();
    s.learn_temp_word("wb", "b", "中", 50, 0).unwrap();
    s.learn_temp_word("wb", "c", "高", 90, 0).unwrap();
    // 保留 2 → 删除权重最低的 1 条（"低"）
    assert_eq!(s.evict_temp_words("wb", 2).unwrap(), 1);
    assert!(s.get_temp_words("wb", "a").unwrap().is_empty());
    assert!(!s.get_temp_words("wb", "c").unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_promote_uses_fixed_weight() {
    let path = tmp("wind_tw_promote.redb");
    let s = Store::open(&path).unwrap();
    s.learn_temp_word("wb", "a", "工", 800, 0).unwrap();
    s.learn_temp_word("wb", "a", "工", 800, 0).unwrap(); // count=2
    assert!(s.promote_temp_word("wb", "a", "工").unwrap());
    // 临时库已删
    assert!(s.get_temp_words("wb", "a").unwrap().is_empty());
    // 晋升权重统一取 PROMOTED_WEIGHT（不沿用临时权重）；用户词 P2 起没有 count
    let u = s.get_user_words("wb", "a").unwrap();
    assert_eq!(u[0].weight, PROMOTED_WEIGHT);
    // 已存在更高权重的手动加词：晋升不应下调，取 max
    s.add_user_word("wb", "b", "戈", 1200, 0).unwrap();
    s.learn_temp_word("wb", "b", "戈", 800, 0).unwrap();
    assert!(s.promote_temp_word("wb", "b", "戈").unwrap());
    assert_eq!(s.get_user_words("wb", "b").unwrap()[0].weight, 1200);
    // 不存在的临时词晋升返回 false
    assert!(!s.promote_temp_word("wb", "z", "无").unwrap());
    let _ = std::fs::remove_file(&path);
}

/// Review Focus 5：门面委托后代次推进时机与旧实现一致。
#[test]
fn temp_writes_bump_generations_like_before() {
    let s = Store::open(tmp("wind_tw_gens.redb")).unwrap();
    let (w0, c0) = (s.words_generation(), s.words_count_generation());
    s.learn_temp_word("py", "a", "甲", 800, 0).unwrap();
    assert!(s.words_generation() > w0, "新词推进结构代次");
    let (w1, c1) = (s.words_generation(), s.words_count_generation());
    assert_eq!(c1, c0);
    s.learn_temp_word("py", "a", "甲", 800, 0).unwrap();
    assert_eq!(s.words_generation(), w1, "count+1 不推进结构代次");
    assert!(s.words_count_generation() > c1);
    s.remove_temp_word("py", "a", "甲").unwrap();
    assert!(s.words_generation() > w1);
}

/// §10 退出判据：按键路径（学习 / 点查 / 临时词层查询）不经过 redb Mutex。
#[test]
fn temp_hot_path_does_not_touch_redb() {
    let s = std::sync::Arc::new(Store::open(tmp("wind_tw_hot.redb")).unwrap());
    s.pause().unwrap(); // redb 暂停：with_db 一律报错
    s.learn_temp_word("py", "nihao", "你好", 800, 0b101)
        .unwrap();
    assert_eq!(s.get_temp_word("py", "nihao", "你好").unwrap(), Some(1));
    let layer = crate::StoreTempLayer::new(s.clone(), "py");
    use wind_dict::DictLayer;
    assert_eq!(layer.search("nihao", 0).len(), 1);
    assert_eq!(layer.search_abbrev("nh", 0).len(), 1);
}
