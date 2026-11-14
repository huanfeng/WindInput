//! 临英残留缓冲在**模式切换**（切中英 / CapsLock / 切方案，同经
//! `take_input_on_mode_switch`）时上屏，也要记输入统计。
//!
//! 现场：临拼 / 快捷输入 / 主路的残留都按 `CommitSource::ModeSwitch` 记一笔，唯独临英那一
//! 支直接返回缓冲原文、不调 `record_commit` ⇒ 这段英文从统计里消失。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP, MOD_SHIFT};
use wind_store::stats::CommitSource;

const VK_CAPITAL: u32 = 0x14;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("schemas/english.schema.toml").exists()
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
        seq_char: 0,
    }
}

/// Shift+H 进临英并打 `ello`（缓冲 `Hello`）。
fn temp_english_hello() -> std::sync::Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    coord.handle_key_event(&key(u32::from(b'H'), MOD_SHIFT));
    for ch in "ello".chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_english"),
        "前提：Shift+H 进临英"
    );
    coord
}

#[test]
fn temp_english_residual_on_mode_switch_records_stats() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let coord = temp_english_hello();
    coord.debug_capture_stat_events();
    let (_, text) = coord.handle_toggle_mode();
    assert_eq!(text, "Hello", "前提：切英文时临英残留原文上屏");
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![(CommitSource::ModeSwitch, "Hello".to_string())],
        "临英残留应按模式切换来源记一笔统计"
    );
}

/// CapsLock 路径（keyup 状态同步分支，经按键出口）同样记这一笔。
#[test]
fn temp_english_residual_on_capslock_records_stats() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let coord = temp_english_hello();
    coord.debug_capture_stat_events();
    let act = coord.handle_key_event(&KeyEventData {
        event_type: EVENT_KEY_UP,
        toggles: 0x01, // 大写锁定已开
        ..key(VK_CAPITAL, 0)
    });
    match act {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "Hello", "前提：开大写时临英残留原文上屏")
        }
        other => panic!("前提：CapsLock 应上屏临英残留，实际: {other:?}"),
    }
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![(CommitSource::ModeSwitch, "Hello".to_string())],
        "临英残留应按模式切换来源记一笔统计"
    );
}
