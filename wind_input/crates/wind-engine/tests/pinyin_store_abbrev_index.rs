//! 用户/临时造词层的简拼召回经声母索引取候选。
//!
//! 枚举全部用户词与临时词现算声母，在大词库下每次上百毫秒，step 6.2 的前缀回退还要逐切点
//! 再来十几遍，表现为「全拼不卡、一打简拼就卡」。本文件钉住换成索引后行为不变：判据全在
//! 引擎侧，索引只缩小候选集。覆盖点：纯简拼 / 混合简拼各自命中；临时词层也走索引；
//! `enable_abbrev=false` 仍关得掉；无边界词仍能召回；前缀回退（step 6.2）也走索引。
//!
//! 自带 wdat 夹具，不依赖 `build_dev/data`。

use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_store::Store;

/// 最小系统词库：只放一个 `nihao`，让系统侧不参与本文件的断言。
fn sys_dict(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_store_abbrev_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("nihao".into(), vec![("你好".into(), 5328, 0, 0b101)]);
    w.add_abbrev("nh".into(), vec![("nihao".into(), 5328)]);
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn store(tag: &str) -> Arc<Store> {
    let p = std::env::temp_dir().join(format!("wind_store_abbrev_{tag}.redb"));
    let _ = std::fs::remove_file(&p);
    Arc::new(Store::open(&p).unwrap())
}

/// 装好用户层 + 临时层的引擎（与 `EngineManager` 的注册方式一致）。
fn engine_with(tag: &str, s: Arc<Store>, cfg: PyConfig) -> PinyinEngine {
    let dm = wind_dict::manager::DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        s.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(
        s.clone(),
        "pinyin",
    )));
    PinyinEngine::new(cfg, sys_dict(tag)).with_store_layers(Arc::new(dm))
}

fn texts(e: &PinyinEngine, input: &str) -> Vec<String> {
    e.convert(input, 30)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

/// 纯简拼命中用户词，且索引确实在筛选：同首字母但声母串不同的词不该被捞出来。
///
/// 「西安宁」xi|an|ning 简拼 `xan`，「先拧」xian|ning 简拼 `xn`：扁平码与首字母相同，
/// 只有音节数不同，落在不同分组，`xan` 不该出「先拧」。
#[test]
fn plain_abbrev_hits_user_word_and_the_index_actually_narrows() {
    let s = store("plain");
    s.add_user_word("pinyin", "xianning", "西安宁", 500, 0b10101)
        .unwrap();
    s.add_user_word("pinyin", "xianning", "先拧", 500, 0b10001)
        .unwrap();
    let e = engine_with("plain", s, PyConfig::default());

    let t = texts(&e, "xan");
    assert!(t.contains(&"西安宁".to_string()), "xan 应命中西安宁: {t:?}");
    assert!(
        !t.contains(&"先拧".to_string()),
        "xan 不该命中先拧（它的声母串是 xn，音节数也对不上）: {t:?}"
    );

    let t2 = texts(&e, "xn");
    assert!(t2.contains(&"先拧".to_string()), "xn 应命中先拧: {t2:?}");
    assert!(!t2.contains(&"西安宁".to_string()), "xn 不该命中西安宁");
}

/// 混合简拼命中用户词：`dbluoge` = d + b + luo + ge。
///
/// `is_abbreviation("dbluoge")` 判假（`u` 不是音节首字母），这条只能由混合判据放行：验证
/// 入口条件取的是「或」，且索引查的是模式的声母投影键（`dblg`）而不是击键串本身。
#[test]
fn mixed_abbrev_hits_user_word_through_the_projection_key() {
    let s = store("mixed");
    s.add_user_word("pinyin", "daboluoge", "大菠萝哥", 500, 0b10010101)
        .unwrap();
    let e = engine_with("mixed", s, PyConfig::default());

    assert!(
        texts(&e, "dbluoge").contains(&"大菠萝哥".to_string()),
        "混合简拼应命中"
    );
    assert!(
        texts(&e, "dblg").contains(&"大菠萝哥".to_string()),
        "纯简拼同样应命中"
    );
}

/// 临时词层也要走索引，否则自动造词开着时临时词库仍被逐切点全量枚举，且其简拼静默召不回。
#[test]
fn temp_layer_words_are_recalled_too() {
    let s = store("temp");
    s.learn_temp_word("pinyin", "zaijian", "再见", 800, 0b1001)
        .unwrap();
    let e = engine_with("temp", s, PyConfig::default());

    assert!(
        texts(&e, "zj").contains(&"再见".to_string()),
        "临时词的简拼应召回"
    );
}

/// 晋升后仍召得回：`promote_temp_word` 把词从临时表搬进用户表，两张索引都要跟着搬。
/// 这条路径在 `temp_words.rs` 里，按文件名找用户词写路径容易漏。
#[test]
fn promoted_word_survives_the_move_between_layers() {
    let s = store("promote");
    s.learn_temp_word("pinyin", "zaijian", "再见", 800, 0b1001)
        .unwrap();
    assert!(s.promote_temp_word("pinyin", "zaijian", "再见").unwrap());
    let e = engine_with("promote", s, PyConfig::default());

    assert!(
        texts(&e, "zj").contains(&"再见".to_string()),
        "晋升进用户词库后简拼仍须召回"
    );
}

/// 闸门不得被绕过：`enable_abbrev=false`（混输经 `schema.mix.enable_pinyin_abbrev` 注入）时，
/// 用户词层一条简拼候选都不该产出。召回函数里重算 `is_abbreviation` 却不带 `enable_abbrev`，
/// 闸门就只剩一半。
#[test]
fn disabling_abbrev_also_disables_the_store_layer_recall() {
    let s = store("gate");
    s.add_user_word("pinyin", "xianning", "西安宁", 500, 0b10101)
        .unwrap();
    s.add_user_word("pinyin", "daboluoge", "大菠萝哥", 500, 0b10010101)
        .unwrap();
    let cfg = PyConfig {
        enable_abbrev: false,
        ..Default::default()
    };
    let e = engine_with("gate", s, cfg);

    assert!(
        !texts(&e, "xan").contains(&"西安宁".to_string()),
        "纯简拼应关掉"
    );
    assert!(
        !texts(&e, "dbluoge").contains(&"大菠萝哥".to_string()),
        "混合简拼应一并关掉"
    );
}

/// 无边界词仍能召回：手输码 / 旧库导入的词没有音节边界、算不出声母串，挂在「码首字符」
/// 兜底组里，查询时并入交给引擎用 DAG 现判。建了索引反而召不回比慢更糟。
#[test]
fn words_without_boundary_are_still_recalled_via_the_fallback_group() {
    let s = store("noboundary");
    // 无边界的用户词：boundary=0
    s.add_user_word("pinyin", "zaijian", "再见", 0, 0).unwrap();
    assert_eq!(
        s.get_user_words("pinyin", "zaijian").unwrap()[0].boundary,
        0,
        "前提：这条记录确实没有边界信息"
    );
    let e = engine_with("noboundary", s, PyConfig::default());

    assert!(
        texts(&e, "zj").contains(&"再见".to_string()),
        "无边界词的简拼须由 DAG 兜底判出"
    );
}

/// 前缀回退（step 6.2）也要走索引：整串无解时引擎逐切点退到最长可命中前缀，一次按键调用
/// 十几遍，正是全表枚举代价被放大的地方。
///
/// `zjnihaobuhao`：整串无解，退到 `zj` 命中用户词「再见」，余下字母留给下次输入。
#[test]
fn prefix_fallback_also_goes_through_the_index() {
    let s = store("fallback");
    s.add_user_word("pinyin", "zaijian", "再见", 62492, 0b1001)
        .unwrap();
    let e = engine_with("fallback", s, PyConfig::default());

    let t = texts(&e, "zjnihaobuhao");
    assert!(
        t.contains(&"再见".to_string()),
        "整串无解时应退到 zj 命中用户词: {t:?}"
    );
}

/// 全拼输入完全不碰这条路，换索引后依旧。
#[test]
fn full_pinyin_input_is_untouched() {
    let s = store("fullpy");
    s.add_user_word("pinyin", "nihao", "你好啊", 500, 0b101)
        .unwrap();
    let e = engine_with("fullpy", s, PyConfig::default());

    let t = texts(&e, "nihao");
    assert!(t.contains(&"你好".to_string()), "系统词应在: {t:?}");
    assert!(t.contains(&"你好啊".to_string()), "用户词按全拼精确命中");
}
