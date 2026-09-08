//! 英文混入**只收精确命中**：两条路（全拼 `english_merge` / 混输 `schema.mix`）都不得
//! 把前缀扩展放进候选列表。
//!
//! 真机现场（本文件的由来）：
//! - 全拼打 `hen` 混进 5 条 —— hen / hence / henceforth / Henderson / Hendrix，后 4 条纯噪音；
//! - 混输打 `github` 混进 8 条 —— GitHub 加 7 个 GitHub 开头的长词组，把候选面整个占满。
//!
//! 英文候选**排第几**不在本文件测——那由协调器 `place_english_after_common_exact` 在
//! 所有排序（含自动调频）之后统一定位，守门测试在 `wind-coordinator` 的
//! `english_placement_tests`。

use std::path::PathBuf;
use wind_candidate::CandidateSource;
use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn ready(dir: &std::path::Path, schema: &str) -> bool {
    dir.join(format!("schemas/{schema}.schema.toml")).exists()
        && dir.join("schemas/english/en.dict.yaml").exists()
}

fn make_config(schemas: &[&str]) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = schemas.iter().map(|s| s.to_string()).collect();
    cfg.schema.active = schemas[0].to_string();
    cfg
}

fn english_texts(r: &wind_engine::ConvertResult) -> Vec<String> {
    r.candidates
        .iter()
        .filter(|c| c.source == CandidateSource::English)
        .map(|c| c.text.clone())
        .collect()
}

/// 全拼（`schema.english_merge`）：`hen` 只混入精确的 `hen`，前缀扩展一条不留。
#[test]
fn pinyin_merge_keeps_only_exact() {
    let dir = data_dir();
    if !ready(&dir, "pinyin") {
        eprintln!("跳过：pinyin 方案或英文词库不存在");
        return;
    }

    // 反向对照：出厂默认（enable=false）不混英文——否则下面的断言可能测的是别的东西。
    let off = EngineManager::new(&make_config(&["pinyin"]), Some(&dir));
    assert!(
        english_texts(&off.convert("hen", 50)).is_empty(),
        "出厂默认不该混英文"
    );

    let mut cfg = make_config(&["pinyin"]);
    cfg.schema.english_merge.enable = true;
    let mgr = EngineManager::new(&cfg, Some(&dir));

    let r = mgr.convert("hen", 50);
    let en = english_texts(&r);
    assert!(
        !en.is_empty(),
        "前提不成立：精确命中 hen 都没混进来，下面测不到前缀过滤"
    );
    for t in &en {
        assert!(
            t.eq_ignore_ascii_case("hen"),
            "前缀扩展进了列表：{en:?}（Henderson/hence 之流必须整片丢弃）"
        );
    }

    // ★ 英文候选不得带着码表域的 is_exact_code —— 带着它会在协调器
    //   `cmp_exact_first`（位置在 by_weight 之前）无条件压过全部中文。
    assert!(
        r.candidates
            .iter()
            .filter(|c| c.source == CandidateSource::English)
            .all(|c| !c.is_exact_code),
        "英文候选仍带着 is_exact_code"
    );
}

/// 全拼：无精确命中时一条都不出（用户该继续敲完，而不是从前缀词里挑）。
#[test]
fn pinyin_merge_silent_without_exact_hit() {
    let dir = data_dir();
    if !ready(&dir, "pinyin") {
        eprintln!("跳过：pinyin 方案或英文词库不存在");
        return;
    }
    let mut cfg = make_config(&["pinyin"]);
    cfg.schema.english_merge.enable = true;
    let mgr = EngineManager::new(&cfg, Some(&dir));

    // `githu` 是 github 的前缀，词库里没有这个词本身。
    assert!(
        english_texts(&mgr.convert("githu", 50)).is_empty(),
        "无精确命中时不得拿前缀词顶上"
    );
    // 对照：敲完就有。
    assert!(
        !english_texts(&mgr.convert("github", 50)).is_empty(),
        "敲完整词后应有精确命中——否则上面那条断言是空转"
    );
}

/// 混输（`schema.mix.enable_english`）：`github` 从 8 条降到只剩精确的那条。
///
/// ⚠️ `enable_english` 必须显式打开：出厂 false 在 L1（`Config::default()`），
/// 而实际发布的出厂值与用户配置都在 L2/L3。不设这行本用例整个空转、却照样 `ok`。
#[test]
fn mixed_keeps_only_exact() {
    let dir = data_dir();
    if !ready(&dir, "wubi86_pinyin") {
        eprintln!("跳过：wubi86_pinyin 方案或英文词库不存在");
        return;
    }
    let mut cfg = make_config(&["wubi86_pinyin"]);
    cfg.schema.mix.enable_english = true;
    cfg.schema.mix.min_english_length = 3;
    let mgr = EngineManager::new(&cfg, Some(&dir));

    let en = english_texts(&mgr.convert("github", 50));
    assert_eq!(en.len(), 1, "混输下 github 应只剩精确命中一条，实际 {en:?}");
    assert!(en[0].eq_ignore_ascii_case("github"));

    let en_hen = english_texts(&mgr.convert("hen", 50));
    for t in &en_hen {
        assert!(
            t.eq_ignore_ascii_case("hen"),
            "混输下前缀扩展进了列表：{en_hen:?}"
        );
    }
}
