//! `command:ime.set("cn-en", …)` 定向中英切换的集成测试（按键绑定路径）。
//!
//! 覆盖三条契约（PR 评审约定：ime.set 的 `set` 语义 = 置到目标态，区别于 toggle 的翻转）：
//! - 绑到纯修饰键（keyup 轻敲）后，命令线程异步执行完毕时落在目标态——而不是翻转；
//! - 已在目标态：命令只提示不动作，**待编码零扰动**（ime.toggle 的老路径会把编码随
//!   翻转上屏/丢弃，`set` 的幂等语义要求它什么都不碰）；
//! - 真切换 + 有待编码：编码按 toggle 同一策略交还宿主（push 出口），切换后组合区清空。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP};

const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;

fn data_dir() -> PathBuf {
    // 相对测试 crate（crates/wind-coordinator）上三级 = 仓库根，build_dev 词库在那里。
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

/// 轮询等命令线程跑完：`command:` 动作在独立线程求值，固定 sleep 要么慢、要么在忙机器上偶发失败。
fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// 「什么都没发生」要给足与正例相同量级的时间窗，否则只证明了「还没发生」。
fn assert_unchanged(coord: &Coordinator, what: &str) {
    std::thread::sleep(Duration::from_millis(300));
    assert!(coord.is_chinese_mode(), "{what}：中英态不得变化");
    assert!(
        coord.debug_pending_composition(),
        "{what}：待编码不得被动丢掉"
    );
}

/// rshift 绑 `ime.set("cn-en","off")`：中文态轻敲 → 英文态（异步命令线程完成后）。
#[test]
fn ime_set_off_switches_to_english() {
    skip_without_data!();
    let coord = open_with(bind_key("rshift", r#"command:ime.set("cn-en","off")"#));
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert!(
        wait_until(|| !coord.is_chinese_mode()),
        "轻敲 rshift 应定向切到英文态"
    );
}

/// lshift 绑 `ime.set("cn-en","on")`：英文态起步 → 中文态。
#[test]
fn ime_set_on_switches_to_chinese() {
    skip_without_data!();
    let coord = open_with(|c| {
        c.input.default.chinese_mode = false;
        bind_key("lshift", r#"command:ime.set("cn-en","on")"#)(c);
    });
    coord.handle_key_event(&key_up(VK_LSHIFT));
    assert!(
        wait_until(|| coord.is_chinese_mode()),
        "轻敲 lshift 应定向切到中文态"
    );
}

/// 幂等：已在中文态再按 `set on`，命令只提示不动作——中英态与待编码都不得变化。
#[test]
fn ime_set_same_target_keeps_composition() {
    skip_without_data!();
    let coord = open_with(bind_key("rshift", r#"command:ime.set("cn-en","on")"#));
    type_str(&coord, "wubi");
    assert!(coord.debug_pending_composition(), "前提：待编码已就位");
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_unchanged(&coord, "已在目标态，set 只提示不动作");
}

/// 真切换 + 待编码：编码随切换交还宿主（push 出口），组合区清空、落在英文态。
#[test]
fn ime_set_switch_commits_pending_composition() {
    skip_without_data!();
    let coord = open_with(bind_key("rshift", r#"command:ime.set("cn-en","off")"#));
    type_str(&coord, "wubi");
    assert!(coord.debug_pending_composition(), "前提：待编码已就位");
    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert!(
        wait_until(|| !coord.is_chinese_mode() && !coord.debug_pending_composition()),
        "切换后应落英文态且组合区已清空（编码经 push 交还宿主）"
    );
}
