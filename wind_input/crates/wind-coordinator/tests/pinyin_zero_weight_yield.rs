//! 零权重读音让位：单字的**该读音**在词库里权重为 0（几乎没人这么读）时，排到同码的简拼词
//! 之后，但仍留在候选里；有权重的读音不受影响。
//!
//! ## 起因
//!
//! 标准音节表补了 `dia`/`sei`/`cei`（3445e134）。双拼里解码到这三个音节的两键组合以前解不出
//! 音节、落到简拼，补表后首选变成生僻单字：小鹤 `cw`→𤭢（原「成为」）、`sw`→塞（原「所谓」）、
//! `dx`→嗲（原「东西」）。词库权重：𤭢(cei)=0、塞(sei)=0、嗲(dia)=182 —— 前两个读音几乎
//! 没人用，后一个是嗲的本音。`nun`（嫩/嫰/黁 全 0）、`zhei`（这 0）是早就存在的同类问题。
//!
//! ## 为什么在协调器级测
//!
//! 界面序由 `candidate_display_order` 整体重排决定（引擎位次 ≠ 界面位次），简拼与精确单字的
//! 先后在那里由 `cmp_match_layers` 裁决。
//!
//! 词典缺失时自动跳过。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_pinyin() -> bool {
    data_dir()
        .join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
}

fn config(schema: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.into()];
    cfg.schema.active = schema.into();
    cfg.input.default.chinese_mode = true;
    // `Config::default()` 的词频开关是 false，与出厂不一致；词频用例要显式打开。
    cfg.schema.pinyin.frequency.enabled = true;
    cfg
}

fn type_keys(coord: &Coordinator, input: &str) -> Vec<String> {
    for c in input.chars() {
        let vk = (c.to_ascii_uppercase() as u32) & 0xFF;
        coord.handle_key_event(&KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    }
    coord.debug_all_candidate_texts()
}

/// 敲入整串，返回协调器的全部候选文本（已按显示序排好）。
fn candidates_for(schema: &str, input: &str) -> Vec<String> {
    let coord = Coordinator::new_headless(config(schema), Some(&data_dir()));
    type_keys(&coord, input)
}

fn fresh_store(tag: &str) -> Arc<Store> {
    let root = std::env::temp_dir().join(format!(
        "wind_zero_weight_yield_{tag}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    Arc::new(Store::open(root.join("user_data.db")).expect("打开 store"))
}

fn head(c: &[String]) -> Vec<&str> {
    c.iter().take(6).map(String::as_str).collect()
}

/// 小鹤两键：零权重读音的单字让位给简拼词，且仍在候选里。
#[test]
fn zero_weight_reading_yields_to_abbrev_word() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want_top, rare) in [("cw", "成为", "𤭢"), ("sw", "所谓", "塞")] {
        let cands = candidates_for("shuangpin", input);
        assert_eq!(
            cands.first().map(String::as_str),
            Some(want_top),
            "小鹤 `{input}` 首选应为简拼词「{want_top}」，实际前 6：{:?}",
            head(&cands)
        );
        let rare_pos = cands.iter().position(|t| t == rare);
        let top_pos = cands.iter().position(|t| t == want_top).unwrap();
        assert!(
            rare_pos.is_some_and(|p| p > top_pos),
            "「{rare}」应让位但仍留在候选里，实际位次 {rare_pos:?}"
        );
    }
}

/// 让位不改变「谁活过引擎截断」：小鹤 `jx`（jia）同音字多，零权重的「夾」(41448 库 w=0)
/// 在不让位时能活过 300 的上限，让位后也必须还在。
///
/// 置位若在截断之前，它沉到列表末尾，`truncate_with_abbrev_quota` 给简拼腾位时从尾部
/// 先挤掉的就是它 —— 用户完全选不到，词频也救不回（重排在截断之后）。
#[test]
fn zero_weight_reading_survives_truncation() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    // 检索范围放到全字集：出厂 smart 档本就滤掉「夾」，看不出截断丢没丢它。
    let mut cfg = config("shuangpin");
    cfg.input.filter_mode = "gb18030".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    let cands = type_keys(&coord, "jx");
    assert!(
        cands.iter().any(|t| t == "夾"),
        "小鹤 `jx` 的候选里应有「夾」（共 {} 条）",
        cands.len()
    );
}

/// 有权重的读音（嗲 dia=182）保持首选：让位只针对「这个读音几乎没人用」。
#[test]
fn nonzero_weight_reading_keeps_top() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let cands = candidates_for("shuangpin", "dx");
    assert_eq!(
        cands.first().map(String::as_str),
        Some("嗲"),
        "小鹤 `dx` 首选应仍为「嗲」，实际前 6：{:?}",
        head(&cands)
    );
    assert!(
        cands.iter().any(|t| t == "东西"),
        "简拼词「东西」应仍在候选里"
    );
}

/// 全拼：`cei`/`sei` 无同码简拼词可让，照旧首选（让位不等于沉底）。
#[test]
fn full_pinyin_rare_syllable_still_types_its_char() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    for (input, want) in [("cei", "𤭢"), ("sei", "塞")] {
        let cands = candidates_for("pinyin", input);
        let pos = cands.iter().position(|t| t == want);
        assert!(
            pos.is_some_and(|p| p < 3),
            "全拼 `{input}` 应在前列出「{want}」，实际位次 {pos:?}，前 6：{:?}",
            head(&cands)
        );
    }
}

/// 用户选过的字不让位：有词频记录 = 用户用过这个读音，**选一次**就回到首选。
///
/// 只靠位置提升不够：让位的字在简拼层末尾（小鹤 `cw` 下 𤭢 在第 140 位），减半模型要选
/// 8 次才到顶。
#[test]
fn zero_weight_reading_recovers_with_user_freq() {
    if !has_pinyin() {
        eprintln!("跳过：拼音词库不存在");
        return;
    }
    let store = fresh_store("cw");
    store.record_freq("pinyin", "cei", "𤭢").expect("写词频");
    let coord = Coordinator::new_headless_with_store(config("shuangpin"), Some(&data_dir()), store);
    let cands = type_keys(&coord, "cw");
    assert_eq!(
        cands.first().map(String::as_str),
        Some("𤭢"),
        "选过一次的「𤭢」应回到首选，实际前 6：{:?}",
        head(&cands)
    );
}
