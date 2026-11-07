//! 方案切换时对「未上屏编码」的处置：与中英切换**同一条策略**（`keys.commit_on_switch`）。
//!
//! 真机现场：有候选时切方案，编码既不上屏也不消失，原样残留在宿主的组合区里。
//! 根因是两层——策略层 `finish_user_schema_switch` 只做了三行裸 `clear()`（不看
//! `commit_on_switch`、不管独占模式），通道层返回的是 `StatusUpdate`，而**只有
//! `CommitText` 那条路会让 C++ 侧 `EndComposition`**，`StatusUpdate` 不会。
//!
//! ⇒ 本文件的断言分成两半，缺一不可：
//!   1. `text` 对不对（上屏原码 / 丢弃）；
//!   2. **KeyAction 的形状**对不对——丢弃分支下 `text` 恒空，此时若返回 `StatusUpdate`
//!      就等于没修：内部状态干净了，用户屏幕上那串编码还在。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL, MOD_SHIFT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("schemas/pinyin.schema.toml").exists()
}

fn cfg() -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c
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

const VK_W: u32 = 0x57;
const VK_P: u32 = 0x50;
const VK_E: u32 = 0x45;
const CTRL_SHIFT: u32 = MOD_CTRL | MOD_SHIFT;

/// 敲一个五笔码 `w`，使缓冲非空（一码不会满码自动上屏，故这一帧稳定可复现）。
fn type_pending_code(coord: &Coordinator) {
    coord.handle_key_event(&key(VK_W, 0));
}

/// `commit_on_switch = true`（出厂值）：方案直达热键切走时上屏原码。
#[test]
fn schema_switch_commits_pending_code() {
    if !has_schemas() {
        return;
    }
    let mut c = cfg();
    c.keys.commit_on_switch = true;
    c.keys
        .key_actions
        .insert("ctrl+shift+p".into(), "switch_schema:pinyin".into());
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    type_pending_code(&coord);

    let act = coord.handle_key_event(&key(VK_P, CTRL_SHIFT));
    match act {
        KeyAction::InsertText { text, .. } => assert_eq!(
            text, "w",
            "commit_on_switch 开启时切方案应上屏原码，与切英文同策略"
        ),
        other => panic!("切方案应经 CommitText 出口回给宿主，实际: {other:?}"),
    }
}

/// `commit_on_switch = false`：不上屏，但**仍须走 `InsertText`**。
///
/// ★ 这是本次修复的核心判据。空文本 + `StatusUpdate` 看起来「什么都没提交，很干净」，
/// 实际是宿主组合区里的编码没人收——正是用户报的那个残留。
#[test]
fn schema_switch_clears_pending_code_when_disabled() {
    if !has_schemas() {
        return;
    }
    let mut c = cfg();
    c.keys.commit_on_switch = false;
    c.keys
        .key_actions
        .insert("ctrl+shift+p".into(), "switch_schema:pinyin".into());
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    type_pending_code(&coord);

    let act = coord.handle_key_event(&key(VK_P, CTRL_SHIFT));
    match act {
        KeyAction::InsertText { text, .. } => assert!(
            text.is_empty(),
            "commit_on_switch 关闭时不该上屏任何文本，实际: {text:?}"
        ),
        other => panic!(
            "丢弃分支也必须走 CommitText 才能结束宿主 composition（StatusUpdate 不结束），实际: {other:?}"
        ),
    }
    assert_eq!(coord.debug_candidate_count(), 0, "候选应随编码一并清空");
}

/// 反向对照：**没有**待处理编码时不该凭空给宿主发提交。
///
/// 没有这一条，上面两个用例在「无条件返回 InsertText」的实现下也会绿，
/// 而那种实现会在每次切方案时给宿主发一个空提交（可能打断宿主自己的组合态）。
#[test]
fn schema_switch_without_pending_input_does_not_commit() {
    if !has_schemas() {
        return;
    }
    let mut c = cfg();
    c.keys
        .key_actions
        .insert("ctrl+shift+p".into(), "switch_schema:pinyin".into());
    let coord = Coordinator::new_headless(c, Some(&data_dir()));

    let act = coord.handle_key_event(&key(VK_P, CTRL_SHIFT));
    assert!(
        !matches!(act, KeyAction::InsertText { .. }),
        "无待处理编码时切方案不该给宿主发提交，实际: {act:?}"
    );
}

/// 循环切换键（`keys.switch_engine`）走的是 `dispatch_hotkey` 那张表，出口与直达热键
/// 不同源——必须单独验一条，否则「直达热键修好了、循环键还残留」。
#[test]
fn cycle_schema_hotkey_commits_pending_code() {
    if !has_schemas() {
        return;
    }
    let mut c = cfg();
    c.keys.commit_on_switch = true;
    c.keys.switch_engine = "ctrl+shift+e".into();
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    type_pending_code(&coord);

    let act = coord.handle_key_event(&key(VK_E, CTRL_SHIFT));
    match act {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "w", "循环切换方案同样按 commit_on_switch 上屏原码")
        }
        other => panic!("循环切换键也须经 CommitText 出口，实际: {other:?}"),
    }
}
