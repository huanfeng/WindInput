//! `input.temp_english.shift_when_composing`：已有编码时按 Shift+字母的行为。
//!
//! - `buffer`（出厂）：大写字母照常进编码缓冲，不进临英（论坛 t255 的现状）。
//! - `commit_enter`：上屏高亮候选，再以该大写字母进临英。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir().join("schemas/wubi86.schema.toml").exists()
}

fn key(k: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn coord(mode: &str) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    c.input.temp_english.shift_when_composing = mode.to_string();
    Coordinator::new_headless(c, Some(&data_dir()))
}

fn type_codes(c: &Coordinator, codes: &str) {
    for ch in codes.chars() {
        c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

fn committed_text(act: &KeyAction) -> Option<&str> {
    match act {
        KeyAction::InsertText { text, .. }
        | KeyAction::CommitThenDeferComposition {
            commit_text: text, ..
        } => Some(text),
        _ => None,
    }
}

#[test]
fn buffer_default_keeps_uppercase_in_code_buffer() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let c = coord("buffer");
    type_codes(&c, "gg");
    c.handle_key_event(&key(u32::from(b'H'), MOD_SHIFT));
    assert_eq!(
        c.debug_active_mode(),
        None,
        "出厂档：大写进编码缓冲，不进临英"
    );
}

#[test]
fn commit_enter_commits_highlight_then_enters_temp_english() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let c = coord("commit_enter");
    type_codes(&c, "gg");
    let act = c.handle_key_event(&key(u32::from(b'H'), MOD_SHIFT));
    let text = committed_text(&act).unwrap_or_else(|| panic!("应先上屏高亮候选，实得 {act:?}"));
    assert!(!text.is_empty(), "上屏文本不应为空");
    assert!(
        text.chars().all(|ch| !ch.is_ascii()),
        "上屏的是中文候选而非字母：{text}"
    );
    assert_eq!(c.debug_active_mode(), Some("temp_english"), "随后进临英");
    // 临英里续打：缓冲以 H 起头
    type_codes(&c, "i");
    assert_eq!(c.debug_active_mode(), Some("temp_english"));
}

#[test]
fn commit_enter_without_candidates_still_enters_temp_english() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let c = coord("commit_enter");
    // 五笔没有以 `qqqq` 起头可上屏的码：空码时丢弃缓冲直接进临英
    type_codes(&c, "qqqq");
    let act = c.handle_key_event(&key(u32::from(b'H'), MOD_SHIFT));
    assert!(
        !matches!(act, KeyAction::PassThrough),
        "不应透传，实得 {act:?}"
    );
    assert_eq!(c.debug_active_mode(), Some("temp_english"));
}

#[test]
fn commit_enter_empty_buffer_unchanged() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let c = coord("commit_enter");
    c.handle_key_event(&key(u32::from(b'H'), MOD_SHIFT));
    assert_eq!(
        c.debug_active_mode(),
        Some("temp_english"),
        "空缓冲仍走原有进入路径"
    );
}
