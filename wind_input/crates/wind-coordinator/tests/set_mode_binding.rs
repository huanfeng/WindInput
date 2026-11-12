//! `set_mode:english` / `set_mode:chinese` 定向切换绑定的集成测试。
//!
//! 覆盖三条契约（spec: `.scratch/set-mode-switch-keys/spec.md`）：
//! - 全局 `keys.key_actions` 把 lshift/rshift 绑到两个新动词后，纯修饰键轻敲（keyup）
//!   定向切到目标态——而不是在当前态上翻转；
//! - 幂等：已在目标态再按不翻转、不报错；
//! - 编码中途按下：先上屏（InsertText）再切；
//! - 未绑定的一侧保持出厂 toggle 语义（绑定只盖自己那颗键）。

use std::path::PathBuf;
use std::sync::Arc;

use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP};

const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir().join("schemas/wubi86.schema.toml").exists()
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
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

fn key_up(vk: u32) -> KeyEventData {
    KeyEventData {
        event_type: EVENT_KEY_UP,
        ..key(vk, 0)
    }
}

fn open_with(edit: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    edit(&mut c);
    Coordinator::new_headless(c, Some(&data_dir()))
}

fn bind_key(key: &str, verb: &str) -> impl FnOnce(&mut Config) {
    let (key, verb) = (key.to_string(), verb.to_string());
    move |c| {
        c.keys.key_actions.insert(key, verb);
    }
}

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

/// lshift 绑 set_mode:english：中文态轻敲 → 英文态。
#[test]
fn lshift_set_english_from_chinese() {
    skip_without_data!();
    let coord = open_with(bind_key("lshift", "set_mode:english"));
    coord.handle_key_event(&key_up(VK_LSHIFT));
    assert!(!coord.is_chinese_mode(), "轻敲 lshift 应定向切到英文态");
}

/// rshift 绑 set_mode:chinese：出厂英文态起步 → 中文态。
#[test]
fn rshift_set_chinese_from_english() {
    skip_without_data!();
    let coord = open_with(|c| {
        c.input.default.chinese_mode = false;
        bind_key("rshift", "set_mode:chinese")(c);
    });
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert!(coord.is_chinese_mode(), "轻敲 rshift 应定向切到中文态");
}

/// 幂等 + 优先级：英文态再按 lshift 仍英文（不翻转回去）；未绑定的 rshift 出厂仍 toggle。
#[test]
fn set_mode_is_idempotent_and_unbound_sibling_keeps_toggle() {
    skip_without_data!();
    let coord = open_with(bind_key("lshift", "set_mode:english"));
    coord.handle_key_event(&key_up(VK_LSHIFT));
    assert!(!coord.is_chinese_mode());
    // 已在英文态，再按 = 无操作，绝不翻转回中文。
    coord.handle_key_event(&key_up(VK_LSHIFT));
    assert!(!coord.is_chinese_mode(), "已处目标态，重复轻敲不得翻转");
    // 另一颗键没绑 → 出厂 toggle 语义原样保留。
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert!(coord.is_chinese_mode(), "未绑定的 rshift 应保持出厂 toggle");
}

/// 编码中途轻敲：先上屏（InsertText）再定向切英文。
#[test]
fn mid_composition_commits_then_sets_english() {
    skip_without_data!();
    let coord = open_with(bind_key("lshift", "set_mode:english"));
    type_str(&coord, "wubi");
    let act = coord.handle_key_event(&key_up(VK_LSHIFT));
    assert!(
        matches!(
            act,
            KeyAction::InsertText {
                mode_changed: true,
                ..
            }
        ),
        "编码中切换应返回 InsertText（先上屏再切），实际 {act:?}"
    );
    assert!(!coord.is_chinese_mode(), "上屏后应落在英文态");
}
