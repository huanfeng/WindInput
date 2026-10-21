//! 拼音「英文候选」的可选前缀补全（`schema.pinyin.english_merge.prefix_complete`）。
//!
//! 判据：中文侧有候选时，只在输入 ≥5 字母且整串切不成完整全拼音节时补，至多 2 条，
//! 不与精确命中重复；双拼下不生效。开关关着时的行为由 `english_merge_exact_only.rs` 守。
//!
//! 词库缺失时整族跳过（worktree 里要自备 `build_dev/data`，核对耗时）。

use std::path::PathBuf;
use wind_candidate::CandidateSource;
use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn ready(schema: &str) -> bool {
    let dir = data_dir();
    dir.join(format!("schemas/{schema}.schema.toml")).exists()
        && dir.join("schemas/english/en.dict.yaml").exists()
}

fn manager(schema: &str, prefix_complete: bool) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.to_string()];
    cfg.schema.active = schema.to_string();
    cfg.schema.pinyin.english_merge.enable = true;
    cfg.schema.pinyin.english_merge.prefix_complete = prefix_complete;
    EngineManager::new(&cfg, Some(&data_dir()))
}

/// 中文侧有没有候选（英文之外的来源）。没有的话走的是前缀回退那条路，测不到本开关。
fn has_chinese(mgr: &EngineManager, input: &str) -> bool {
    mgr.convert(input, 50)
        .candidates
        .iter()
        .any(|c| c.source != CandidateSource::English)
}

fn english_texts(mgr: &EngineManager, input: &str) -> Vec<String> {
    mgr.convert(input, 50)
        .candidates
        .iter()
        .filter(|c| c.source == CandidateSource::English)
        .map(|c| c.text.clone())
        .collect()
}

/// 不是完整拼音的串：开关开着补前缀，关着一条不出（中文侧有候选时原本只收精确）。
#[test]
fn completes_when_not_full_pinyin() {
    if !ready("pinyin") {
        eprintln!("跳过：pinyin 方案或英文词库不存在");
        return;
    }
    let on = manager("pinyin", true);
    let off = manager("pinyin", false);
    assert!(
        has_chinese(&on, "templa"),
        "前提：templa 中文侧有候选（否则走的是前缀回退那条路）"
    );
    assert!(
        english_texts(&off, "templa").is_empty(),
        "开关关着：中文侧有候选时不补前缀"
    );
    let en = english_texts(&on, "templa");
    assert!(
        en.iter().any(|t| t.eq_ignore_ascii_case("template")),
        "templa 应补出 template，实际 {en:?}"
    );
    assert!(en.len() <= 2, "无精确命中时至多补 2 条，实际 {en:?}");
}

/// 能切成完整全拼音节的串不补，哪怕英文库里有以它开头的词。
#[test]
fn full_pinyin_is_not_completed() {
    if !ready("pinyin") {
        eprintln!("跳过：pinyin 方案或英文词库不存在");
        return;
    }
    let on = manager("pinyin", true);
    for s in ["chang", "shang"] {
        assert!(has_chinese(&on, s), "前提：{s} 中文侧有候选");
        let en = english_texts(&on, s);
        assert!(
            en.iter().all(|t| t.eq_ignore_ascii_case(s)),
            "{s} 是完整拼音，只许出精确命中，实际 {en:?}"
        );
    }
}

/// 有精确命中时：精确那条之外至多再补 2 条，且不重复。
#[test]
fn exact_plus_at_most_two() {
    if !ready("pinyin") {
        eprintln!("跳过：pinyin 方案或英文词库不存在");
        return;
    }
    let on = manager("pinyin", true);
    // `table`：ta + ble 切不完整，拼音照样给得出候选（简拼）；英文库里有 table / tables / tablet。
    assert!(
        has_chinese(&on, "table"),
        "前提：table 中文侧有候选（否则走的是前缀回退那条路）"
    );
    let en = english_texts(&on, "table");
    assert!(
        en.iter().any(|t| t == "table"),
        "前提：table 精确命中在场，实际 {en:?}"
    );
    let extra: Vec<_> = en.iter().filter(|t| *t != "table").collect();
    assert!(
        !extra.is_empty() && extra.len() <= 2,
        "精确之外应补 1~2 条，实际 {en:?}"
    );
    let mut dedup = en.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), en.len(), "补全不得与精确命中重复：{en:?}");
}

/// 补全按英文词频排：`produc` 先补 product（rime-frost 的英文主库没有权重列，
/// 不注入词频就是字母序，先出 produce / produced）。
#[test]
fn completion_follows_word_frequency() {
    if !ready("pinyin") {
        eprintln!("跳过：pinyin 方案或英文词库不存在");
        return;
    }
    let on = manager("pinyin", true);
    assert!(has_chinese(&on, "produc"), "前提：produc 中文侧有候选");
    let en = english_texts(&on, "produc");
    assert_eq!(
        en.first().map(String::as_str),
        Some("product"),
        "produc 应先补 product，实际 {en:?}"
    );
}

/// 双拼下不生效：判据按全拼音节定义，双拼码套不上。
#[test]
fn shuangpin_ignores_prefix_complete() {
    if !ready("shuangpin") {
        eprintln!("跳过：shuangpin 方案或英文词库不存在");
        return;
    }
    let on = manager("shuangpin", true);
    assert!(
        has_chinese(&on, "templa"),
        "前提：双拼下 templa 中文侧有候选"
    );
    assert!(
        english_texts(&on, "templa").is_empty(),
        "双拼下不该补前缀，实际 {:?}",
        english_texts(&on, "templa")
    );
    // 反向对照：同一串在全拼下会补，说明上面的空不是词库缺词造成的。
    assert!(!english_texts(&manager("pinyin", true), "templa").is_empty());
}
