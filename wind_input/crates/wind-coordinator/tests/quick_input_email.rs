//! 快捷输入里按 `@` 转交邮箱模式（GH#162 的快捷输入一半；临英一半见 `email_mode_tests.rs`）。
//!
//! 五笔用户名常超过四码，正常输入流里早已顶字上屏，`try_prefix_hijack` 的 `@` 触发只对
//! 短用户名可用。快捷输入能攒住一整段，在其中认 `@` 才能带完整用户名进邮箱模式。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库（快捷输入的成员方案要能加载）；缺失时**静默跳过**
//! （判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};

const VK_SEMICOLON: u32 = 0xBA;
const VK_SPACE: u32 = 0x20;
const VK_BACK: u32 = 0x08;
const VK_NEXT: u32 = 0x22;
const VK_OEM_PLUS: u32 = 0xBB;
const VK_OEM_PERIOD: u32 = 0xBE;
const VK_LEFT: u32 = 0x25;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
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

/// 五笔主方案（`;` 空码进快捷输入），`email` 控制邮箱模式开关。
fn open(email: bool) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.email.enabled = email;
    Coordinator::new_headless(c, Some(&data_dir()))
}

/// 小写字母与数字逐键敲入（不带 Shift）。
fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

fn enter_quick(coord: &Coordinator) {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
}

/// `@` = Shift+2。
fn press_at(coord: &Coordinator) -> KeyAction {
    coord.handle_key_event(&key(0x32, MOD_SHIFT))
}

fn composition(act: &KeyAction) -> Option<String> {
    match act {
        KeyAction::UpdateComposition { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn committed(act: &KeyAction) -> Option<String> {
    match act {
        KeyAction::InsertText { text, .. } => Some(text.clone()),
        _ => None,
    }
}

#[test]
fn at_sign_carries_the_whole_username() {
    skip_without_data!();
    let coord = open(true);
    enter_quick(&coord);
    type_str(&coord, "zhangsan");
    let act = press_at(&coord);
    assert_eq!(coord.debug_active_mode(), Some("email"), "实际 {act:?}");
    assert_eq!(composition(&act).as_deref(), Some("zhangsan@"));
    let act = coord.handle_key_event(&key(VK_SPACE, 0));
    assert_eq!(committed(&act).as_deref(), Some("zhangsan@qq.com"));
}

/// 大写用户名：`auto` 下落 Free 透镜，照样转交，且保留大小写。
#[test]
fn uppercase_username_is_kept() {
    skip_without_data!();
    let coord = open(true);
    enter_quick(&coord);
    coord.handle_key_event(&key(u32::from(b'Z'), MOD_SHIFT));
    type_str(&coord, "hangsan");
    let act = press_at(&coord);
    assert_eq!(coord.debug_active_mode(), Some("email"), "实际 {act:?}");
    assert_eq!(composition(&act).as_deref(), Some("Zhangsan@"));
}

/// 纯数字用户名（QQ 邮箱）：数字开头落数字透镜，不能因此挡掉。
#[test]
fn numeric_username_is_accepted() {
    skip_without_data!();
    let coord = open(true);
    enter_quick(&coord);
    type_str(&coord, "123456");
    let act = press_at(&coord);
    assert_eq!(coord.debug_active_mode(), Some("email"), "实际 {act:?}");
    assert_eq!(composition(&act).as_deref(), Some("123456@"));
}

/// 退格回到边界：回到快捷输入（同一实例、前缀 `;` 还在），而不是码表缓冲。
#[test]
fn backspace_at_boundary_returns_to_quick_input() {
    skip_without_data!();
    let coord = open(true);
    enter_quick(&coord);
    coord.handle_key_event(&key(u32::from(b'A'), 0));
    // 组合区由快捷输入自己渲染（文本透镜下可能是拼音切分 `a'b`），回退后应逐字相同。
    let before = composition(&coord.handle_key_event(&key(u32::from(b'B'), 0)));
    assert!(
        before.as_deref().is_some_and(|t| t.starts_with(';')),
        "前提：{before:?}"
    );
    press_at(&coord);
    let act = coord.handle_key_event(&key(VK_BACK, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "回退应放回快捷输入缓冲，实际 {act:?}"
    );
    assert_eq!(composition(&act), before, "前缀与缓冲都应原样回来");
    // 回来后仍是正常的快捷输入：能继续打字、再按 @ 还能再进。
    type_str(&coord, "c");
    let act = press_at(&coord);
    assert_eq!(composition(&act).as_deref(), Some("abc@"));
}

#[test]
fn untouched_when_email_disabled() {
    skip_without_data!();
    let coord = open(false);
    enter_quick(&coord);
    type_str(&coord, "ab");
    let act = press_at(&coord);
    assert_ne!(coord.debug_active_mode(), Some("email"));
    assert_at_kept(&act);
}

/// 缓冲不像用户名（带 `+`）：`@` 维持原语义，不转交。
#[test]
fn non_username_buffer_keeps_at_literal() {
    skip_without_data!();
    let coord = open(true);
    enter_quick(&coord);
    type_str(&coord, "a");
    coord.handle_key_event(&key(VK_OEM_PLUS, MOD_SHIFT)); // `+`
    type_str(&coord, "b");
    let act = press_at(&coord);
    assert_ne!(coord.debug_active_mode(), Some("email"));
    assert_at_kept(&act);
}

/// `@` 没被转交时也不能被吞：要么字面进了缓冲（组合区里有），要么顶屏上了。
fn assert_at_kept(act: &KeyAction) {
    let text = composition(act)
        .or_else(|| committed(act))
        .unwrap_or_default();
    assert!(text.contains('@'), "`@` 应照旧字面或顶屏，实际 {act:?}");
}

/// 数字透镜里带 `.`/`-` 的缓冲是算式或日期，`@` 原本就是字面，不抢。
#[test]
fn numeric_expression_keeps_at_literal() {
    skip_without_data!();
    let coord = open(true);
    enter_quick(&coord);
    type_str(&coord, "1");
    coord.handle_key_event(&key(VK_OEM_PERIOD, 0));
    type_str(&coord, "5");
    let act = press_at(&coord);
    assert_ne!(coord.debug_active_mode(), Some("email"));
    assert_at_kept(&act);
}

/// 光标不在末尾时 `@` 是插进中间的，整个缓冲当用户名就错了。
#[test]
fn not_when_cursor_is_inside_buffer() {
    skip_without_data!();
    let coord = open(true);
    enter_quick(&coord);
    type_str(&coord, "abc");
    coord.handle_key_event(&key(VK_LEFT, 0));
    press_at(&coord);
    assert_ne!(coord.debug_active_mode(), Some("email"));
}

/// `free_input = always` 的实例专做字面输入，`@` 照字面。
#[test]
fn not_in_always_literal_instance() {
    skip_without_data!();
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.email.enabled = true;
    c.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Always;
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    enter_quick(&coord);
    type_str(&coord, "ab");
    let act = press_at(&coord);
    assert_ne!(coord.debug_active_mode(), Some("email"));
    assert_at_kept(&act);
}

/// 已有分步上屏的段（`committed_text` 非空）时不转交：转交不处置已转换前缀。
#[test]
fn not_after_partial_commit() {
    skip_without_data!();
    let coord = open(true);
    enter_quick(&coord);
    type_str(&coord, "nihao");
    let mut picked = false;
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if let Some(p) = t.iter().position(|x| x == "你") {
            coord.handle_key_event(&key(0x31 + p as u32, 0));
            picked = true;
            break;
        }
        coord.handle_key_event(&key(VK_NEXT, 0));
    }
    assert!(picked, "前提：候选里应有「你」");
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：分步后仍在快捷输入"
    );
    press_at(&coord);
    assert_ne!(coord.debug_active_mode(), Some("email"));
}
