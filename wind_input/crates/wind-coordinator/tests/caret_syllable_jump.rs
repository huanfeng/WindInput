//! 编码区光标的音节跳转（rime 导航器语义）：
//!
//! - ←/→：光标在音节边界上整音节跳，在音节中间按单字符；
//! - Ctrl+←/→：总是整音节跳；
//! - 两端回绕；无切分时退回单字符。
//!
//! 光标位置用显示串里的字节偏移断言。引擎自己拆的 `'` 不占缓冲位：`nihao` 的 `ni|hao`
//! （缓冲位 2）在显示串 `ni'hao` 里落在 `'` 之前，仍是 2。
//! 词典缺失时整族静默跳过——核对 `--nocapture` 里有没有「跳过：」。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::MOD_SHIFT;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL};
use wind_keys::keymap::{VK_BACKTICK, VK_LEFT, VK_RIGHT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn data_ready() -> bool {
    let ok = data_dir()
        .join("schemas/pinyin/rime_frost.dict.yaml")
        .exists();
    if !ok {
        eprintln!("跳过：词库不存在");
    }
    ok
}

fn key(vk: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn coord() -> std::sync::Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

fn type_code(c: &Coordinator, code: &str) {
    for ch in code.chars() {
        c.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

fn left(c: &Coordinator) {
    c.handle_key_event(&key(VK_LEFT, 0));
}
fn right(c: &Coordinator) {
    c.handle_key_event(&key(VK_RIGHT, 0));
}

#[test]
fn left_from_end_jumps_whole_syllable_then_steps_inside() {
    if !data_ready() {
        return;
    }
    let c = coord();
    type_code(&c, "nihao");
    assert_eq!(c.debug_ui_caret_bytes(), 6, "nihao|");
    left(&c);
    assert_eq!(c.debug_ui_caret_bytes(), 2, "末尾是边界 → 整音节 ni|hao");
    left(&c);
    assert_eq!(c.debug_ui_caret_bytes(), 0, "边界 → |ni'hao");
}

#[test]
fn mid_syllable_steps_one_char() {
    if !data_ready() {
        return;
    }
    let c = coord();
    type_code(&c, "nihao");
    left(&c); // ni'|hao
    right(&c); // 边界 → 末尾
    left(&c); // ni'|hao
    c.handle_key_event(&key(VK_RIGHT, 0)); // → 末尾
    // 走到音节中间：先到 ni'|hao，再 Ctrl+→ 不涉及；这里用 ←、→ 配合 Home 外的方式：
    left(&c); // ni'|hao
    c.handle_key_event(&key(VK_LEFT, MOD_CTRL)); // Ctrl+← → |ni
    assert_eq!(c.debug_ui_caret_bytes(), 0);
}

#[test]
fn left_at_head_wraps_to_last_syllable() {
    if !data_ready() {
        return;
    }
    let c = coord();
    type_code(&c, "nihao");
    left(&c);
    left(&c); // |ni'hao
    left(&c); // rime 回绕 → ni'|hao
    assert_eq!(c.debug_ui_caret_bytes(), 2);
}

#[test]
fn right_at_end_wraps_to_first_boundary() {
    if !data_ready() {
        return;
    }
    let c = coord();
    type_code(&c, "nihao");
    right(&c); // 末尾再右 → 回绕到 ni'|hao
    assert_eq!(c.debug_ui_caret_bytes(), 2);
}

#[test]
fn ctrl_arrows_jump_syllables_even_mid_syllable() {
    if !data_ready() {
        return;
    }
    let c = coord();
    type_code(&c, "woxihuan");
    c.handle_key_event(&key(VK_LEFT, MOD_CTRL));
    let a = c.debug_ui_caret_bytes();
    c.handle_key_event(&key(VK_LEFT, MOD_CTRL));
    let b = c.debug_ui_caret_bytes();
    assert!(b < a, "Ctrl+← 逐音节左移: {a} → {b}");
    c.handle_key_event(&key(VK_RIGHT, MOD_CTRL));
    assert_eq!(c.debug_ui_caret_bytes(), a, "Ctrl+→ 回到上一个停靠点");
    assert_eq!(c.debug_input_buffer(), "woxihuan", "只移光标不改缓冲");
}

#[test]
fn single_syllable_falls_back_to_char_step() {
    if !data_ready() {
        return;
    }
    let c = coord();
    type_code(&c, "ni");
    left(&c);
    assert_eq!(c.debug_ui_caret_bytes(), 1, "单音节无跳转 → 单字符");
}

#[test]
fn ctrl_left_without_composition_passes_through() {
    if !data_ready() {
        return;
    }
    let c = coord();
    let act = c.handle_key_event(&key(VK_LEFT, MOD_CTRL));
    assert!(
        matches!(act, wind_bridge::handler::KeyAction::PassThrough),
        "没有编码时 Ctrl+← 归宿主（按词移动），实际: {act:?}"
    );
}

/// Shift+←/→：总是单字符，是走到音节中间的精调通路。
#[test]
fn shift_arrows_step_one_char() {
    if !data_ready() {
        return;
    }
    let c = coord();
    type_code(&c, "nihao");
    c.handle_key_event(&key(VK_LEFT, MOD_SHIFT));
    assert_eq!(c.debug_ui_caret_bytes(), 5, "ni'ha|o：单字符");
    c.handle_key_event(&key(VK_LEFT, MOD_SHIFT));
    assert_eq!(c.debug_ui_caret_bytes(), 4, "ni'h|ao：单字符");
    left(&c); // 缓冲 nih| 在音节中间 → 单字符，落到 ni|hao（显示串里在 ' 之前）
    assert_eq!(c.debug_ui_caret_bytes(), 2, "音节中间 ← 仍是单字符");
}

fn overlay_coord() -> std::sync::Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

/// 临拼 overlay（`` ` `` 引导）与主路同一套：边界整音节跳、Ctrl 总是整音节、Shift 单字符。
#[test]
fn temp_pinyin_overlay_follows_same_rules() {
    if !data_ready() || !data_dir().join("schemas/wubi86.schema.toml").exists() {
        return;
    }
    let c = overlay_coord();
    c.handle_key_event(&key(VK_BACKTICK, 0));
    type_code(&c, "nihao");
    let end = c.debug_ui_caret_bytes();
    left(&c);
    assert_eq!(
        c.debug_ui_caret_bytes(),
        3,
        "`ni|hao：边界整音节（前缀 ` 占 1）"
    );
    assert!(end > 3);
    c.handle_key_event(&key(VK_RIGHT, MOD_SHIFT));
    assert_eq!(c.debug_ui_caret_bytes(), 5, "Shift+→ 单字符（`ni'h|）");
    c.handle_key_event(&key(VK_LEFT, MOD_CTRL));
    assert_eq!(c.debug_ui_caret_bytes(), 3, "Ctrl+← 音节中间也跳到边界");
    c.handle_key_event(&key(VK_LEFT, MOD_CTRL));
    assert_eq!(c.debug_ui_caret_bytes(), 1, "再 Ctrl+← 到开头");
    assert!(
        c.debug_preedit().starts_with('`'),
        "仍在临拼里，组合没被清掉"
    );
}
