//! 模糊音「与精确拼音统一词频排序」（`input.fuzzy.equal_rank`）：开启后模糊命中不再被惩罚，
//! 模糊整句也不再被压到最强精确整词之下，高频词按词频自然靠前。
//!
//! 走协调器断言界面序（引擎位次不作数）。真实词库在 `build_dev/data`，缺失时跳过。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir()
        .join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
}

/// 前后鼻音 + 平翘舌全开、按 `equal_rank` 取值，敲入 `input` 后的全部候选文字（界面序）。
fn run(input: &str, fuzzy: bool, equal_rank: bool) -> Vec<String> {
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.input.fuzzy.enabled = fuzzy;
    c.input.fuzzy.in_ing = true;
    c.input.fuzzy.an_ang = true;
    c.input.fuzzy.en_eng = true;
    c.input.fuzzy.zh_z = true;
    c.input.fuzzy.ch_c = true;
    c.input.fuzzy.sh_s = true;
    c.input.fuzzy.equal_rank = equal_rank;
    c.input.correction.rules.gn_ng = true;
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    for ch in input.chars() {
        coord.handle_key_event(&KeyEventData {
            key_code: ch.to_ascii_uppercase() as u32,
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

fn rank(all: &[String], text: &str) -> usize {
    all.iter()
        .position(|t| t == text)
        .unwrap_or_else(|| panic!("{text} 不在候选里：{:?}", &all[..all.len().min(20)]))
}

/// `jinxin` 开模糊音后，高频「进行」被压到「金星」「精心」「尽心」之后。
/// 开关关着时保持原样（精确优先的既有行为不变）。
#[test]
fn off_keeps_exact_first_behaviour() {
    if !has_data() {
        return;
    }
    let all = run("jinxin", true, false);
    assert!(
        rank(&all, "进行") > rank(&all, "尽心"),
        "关着时「进行」仍在「尽心」之后：{:?}",
        &all[..8]
    );
}

#[test]
fn on_ranks_fuzzy_by_plain_frequency() {
    if !has_data() {
        return;
    }
    let all = run("jinxin", true, true);
    assert_eq!(
        all[0],
        "进行",
        "统一词频后高频「进行」居首：{:?}",
        &all[..8]
    );
    assert!(
        rank(&all, "尽心").max(rank(&all, "精心")) < 12,
        "精确与模糊的词都还在前排：{:?}",
        &all[..12]
    );
}

/// 不打折：原词频更高的模糊词（惊醒 370）排在精确的「尽心」（294）之前；关着时模糊词减半（185），
/// 排在它之后。钉的是词典命中的 ×0.5，与整句降级是两处独立的改动。
#[test]
fn on_removes_the_half_weight_discount() {
    if !has_data() {
        return;
    }
    let on = run("jinxin", true, true);
    assert!(rank(&on, "惊醒") < rank(&on, "尽心"), "{:?}", &on[..10]);
    let off = run("jinxin", true, false);
    assert!(rank(&off, "尽心") < rank(&off, "惊醒"), "{:?}", &off[..10]);
}

/// 开着也只改模糊音，不动没有模糊的输入：`jinxing` 的前几位与关着时一致。
#[test]
fn on_does_not_change_exact_inputs() {
    if !has_data() {
        return;
    }
    let off = run("jinxing", true, false);
    let on = run("jinxing", true, true);
    assert_eq!(off[..2], on[..2], "精确输入的首位不变");
}

/// 纠错（错拼）不属于模糊音，开着统一词频也照旧被罚：`dagnshi` 的「当时」仍在前 10。
#[test]
fn on_keeps_correction_penalty() {
    if !has_data() {
        return;
    }
    let off = run("dagnshi", true, false);
    let on = run("dagnshi", true, true);
    assert!(rank(&on, "当时") < 10, "{:?}", &on[..10]);
    // 纠错候选没有因为开关而被抬到精确候选之前
    assert_eq!(
        off.first(),
        on.first(),
        "纠错输入的首位不变：{:?} / {:?}",
        &off[..5],
        &on[..5]
    );
}
