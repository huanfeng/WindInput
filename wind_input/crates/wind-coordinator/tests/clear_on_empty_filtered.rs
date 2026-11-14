//! 满码空码清空按**用户看到的列表**裁决（GH#221）：码表里有字、但全被过滤掉（检索范围
//! 「常用字」滤掉生僻字、单字输入滤掉词组）时，候选窗是空的，应与码表真没有这个码一样清空。
//!
//! 数据前提（`wubi86_jidian.dict.yaml`）：`lynn` 只有生僻字「甿」「軶」；`aaad` 只有词组「工期」；
//! `aaab` 码表里不存在。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("charsets").exists()
}

fn press_letter(coord: &Coordinator, c: char) -> KeyAction {
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
    })
}

fn coord(filter_mode: &str, single_char: bool) -> std::sync::Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = filter_mode.into();
    cfg.schema.codetable.clear_on_empty_max = true;
    cfg.schema.codetable.single_char = single_char;
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

/// 打完整个码，返回最后一键是否清空了组合，以及此时的候选。
fn type_code(c: &Coordinator, code: &str) -> (bool, Vec<String>) {
    let mut action = KeyAction::Consumed;
    for ch in code.chars() {
        action = press_letter(c, ch);
    }
    (
        matches!(action, KeyAction::ClearComposition),
        c.debug_all_candidate_texts(),
    )
}

/// 对照：码表里根本没有的码，满码即清空。
#[test]
fn absent_code_is_cleared() {
    if !has_data() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let (cleared, cands) = type_code(&coord("general", false), "aaab");
    assert!(cleared && cands.is_empty(), "实际候选 {cands:?}");
}

/// 「常用字」档把生僻字全滤掉 ⇒ 视同空码，清空。
#[test]
fn rare_only_code_is_cleared_under_common_scope() {
    if !has_data() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let (cleared, cands) = type_code(&coord("general", false), "lynn");
    assert!(cleared && cands.is_empty(), "实际候选 {cands:?}");
}

/// 对照：不滤生僻字的档位里，`lynn` 照常出字、不清空。
#[test]
fn rare_only_code_is_kept_under_full_scope() {
    if !has_data() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let (cleared, cands) = type_code(&coord("gb18030", false), "lynn");
    assert!(!cleared);
    assert!(cands.iter().any(|t| t == "甿"), "实际 {cands:?}");
}

/// 单字输入把词组全滤掉 ⇒ 同样视同空码，清空。
#[test]
fn word_only_code_is_cleared_under_single_char() {
    if !has_data() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let (_, cands) = type_code(&coord("gb18030", false), "aaad");
    assert!(
        cands.iter().any(|t| t == "工期"),
        "前提：不开单字时有词组，实际 {cands:?}"
    );

    let (cleared, cands) = type_code(&coord("gb18030", true), "aaad");
    assert!(cleared && cands.is_empty(), "实际候选 {cands:?}");
}
