//! 换位纠错候选在界面上看得见：按罚过的分与其余候选穿插、窗口内保底 4 条。
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

/// 敲入 `input` 后的全部候选文字（界面序，不含标记）。
fn all_texts(input: &str) -> Vec<String> {
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.input.correction.rules.gn_ng = true; // 出厂关；用例是 dagnshi / gognping / xign
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

fn rank(all: &[String], text: &str) -> Option<usize> {
    all.iter().position(|t| t == text)
}

#[test]
fn corrected_candidates_are_visible() {
    if !has_data() {
        return;
    }
    for (input, text, within) in [
        ("zhonguo", "中国", 10),
        ("dagnshi", "当时", 10),
        ("gognping", "公平", 5),
    ] {
        let all = all_texts(input);
        let r = rank(&all, text);
        assert!(
            r.is_some_and(|r| r < within),
            "{input}: {text} 在 {r:?}：{all:?}"
        );
    }
}

#[test]
fn xign_shows_correction_and_keeps_abbrev() {
    if !has_data() {
        return;
    }
    let all = all_texts("xign");
    assert!(rank(&all, "型").is_some(), "{all:?}");
    assert!(rank(&all, "习惯").is_some(), "{all:?}");
}
