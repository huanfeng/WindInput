//! 组合键的 `keys.key_actions`：值域与单键、修饰键同一张表（`BoundAction`）。组合键通路在按键链
//! 上位次更早（英文模式分水岭之前）、不与输入争键，本该是限制最少的一条。
//!
//! 本文件守三件事：
//!
//! 1. 能力：A/B/C 三类动词绑在组合键上都真的动作（wind-config 的
//!    `combo_keys_accept_the_whole_bound_action_domain` 只验进没进表，到不了分派端）；
//! 2. 热键上下文专有的三条：key_code=0 哨兵（不写引导符）、`chinese_mode` 守卫、已在该模式时幂等；
//! 3. 中英切换的回程：`toggle_mode` 绑组合键时不得带 `CHINESE_ONLY`，否则切到英文态后 TSF 不再
//!    转发，再也切不回来。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_ALT, MOD_CTRL, MOD_SHIFT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 返回 `false` 时全部用例直接 `return` 并报 `ok`，一条断言都没跑（`tests/` 下缺 `build_dev/data`
/// 时不红的既有约定）。故跳过时留一行痕，让「全绿但什么都没测」看得出来。
fn has_schemas() -> bool {
    let d = data_dir();
    let ok = d.join("schemas/wubi86.schema.toml").exists()
        && d.join("schemas/pinyin.schema.toml").exists();
    if !ok {
        eprintln!("SKIP: 缺 build_dev/data/schemas，本用例未执行任何断言");
    }
    ok
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

const VK_J: u32 = 0x4A;
const VK_W: u32 = 0x57;

/// 三个修饰键是刻意选的：出厂 `keys.toggle_s2t` 占着 `ctrl+shift+j`，固定字段编译在
/// `key_actions` 之前，`match_key_down` 又是 `.find()` 先注册者赢。用两修饰键写这些用例，
/// 绑定会被出厂那条静默遮蔽。选键前先查一遍出厂占用。
const CTRL_ALT_SHIFT: u32 = MOD_CTRL | MOD_ALT | MOD_SHIFT;

/// 装一条 `ctrl+alt+shift+j` → `verb` 的组合键绑定。
fn coord_with_combo(verb: &str) -> std::sync::Arc<Coordinator> {
    let mut c = cfg();
    c.keys
        .key_actions
        .insert("ctrl+alt+shift+j".into(), verb.into());
    Coordinator::new_headless(c, Some(&data_dir()))
}

/// A 类：中英文切换绑组合键（`keys.toggle_mode_keys` 的值域只有五个修饰键，组合键只能走这里）。
#[test]
fn combo_key_toggles_chinese_mode() {
    if !has_schemas() {
        return;
    }
    let coord = coord_with_combo("toggle_mode");
    assert!(coord.is_chinese_mode(), "出厂应在中文态");

    coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert!(!coord.is_chinese_mode(), "组合键应切到英文");
}

/// 回程：英文态下同一个组合键必须还能按得动。
///
/// 单独一条：只验「切过去」的话，给 `toggle_mode` 错配 `CHINESE_ONLY` 的实现也会绿，而那种实现
/// 切到英文态后 TSF 不再转发这个键，用户再也回不到中文。
#[test]
fn combo_key_toggles_back_from_english() {
    if !has_schemas() {
        return;
    }
    let coord = coord_with_combo("toggle_mode");
    coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert!(!coord.is_chinese_mode());

    coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert!(coord.is_chinese_mode(), "英文态下按同一个组合键应切回中文");
}

/// B 类：进 overlay 模式，且组合区不写引导符（key_code=0 哨兵）。热键进入没有引导符可写，
/// 写了就会在用户屏幕上凭空多一个字符。
#[test]
fn combo_key_enters_overlay_without_guide_prefix() {
    if !has_schemas() {
        return;
    }
    let coord = coord_with_combo("temp_pinyin");

    let act = coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_pinyin"),
        "组合键应进临时拼音"
    );
    match act {
        KeyAction::Consumed => {}
        KeyAction::UpdateComposition { text, .. } => {
            assert!(
                text.is_empty(),
                "直达热键不写引导符，组合区应为空，实际: {text:?}"
            )
        }
        other => panic!("进模式应吞键，实际: {other:?}"),
    }
}

/// 幂等：已在目标模式时再按，安静吃掉，不重开。
///
/// 重开会清掉用户已打进模式里的编码，放行则让这个组合键泄漏给宿主，两种都不该有。
#[test]
fn combo_key_is_idempotent_inside_its_own_mode() {
    if !has_schemas() {
        return;
    }
    let coord = coord_with_combo("temp_pinyin");
    coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert_eq!(coord.debug_active_mode(), Some("temp_pinyin"));

    let act = coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert!(
        matches!(act, KeyAction::Consumed),
        "已在该模式时应安静吃掉，实际: {act:?}"
    );
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_pinyin"),
        "不得退出或重开模式"
    );
}

/// `chinese_mode` 守卫：英文态下 B 类动词不触发。
///
/// 策略位已让 TSF 在英文态不转发这个键，这里仍要判：别的路径转发进来的同一个键会在英文态
/// 凭空建一个组合区。判据取 `BoundAction::only_in_chinese_mode`，与编译期给不给
/// `CHINESE_ONLY` 位是同一个方法。
#[test]
fn combo_key_overlay_verb_is_inert_in_english_mode() {
    if !has_schemas() {
        return;
    }
    let mut c = cfg();
    c.input.default.chinese_mode = false;
    c.keys
        .key_actions
        .insert("ctrl+alt+shift+j".into(), "temp_pinyin".into());
    let coord = Coordinator::new_headless(c, Some(&data_dir()));

    coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "英文态下不得进入 overlay 模式"
    );
}

/// 门卫没过时仍然吞键，不把组合键泄漏给宿主。
///
/// 现场是正常配置：活跃方案本身就是拼音方案时 `temp_pinyin_target()` 恒为 `None`，
/// `commit_and_enter_bound_action` 的门卫返回 `None`。此时若照引导键通路「不吞键、落回按键链」，
/// `Ctrl+Alt+Shift+J` 会交给宿主执行它自己的加速键；引导键通路能那么做是因为落下去只打出一个字符。
#[test]
fn combo_key_eats_the_key_even_when_the_guard_rejects() {
    if !has_schemas() {
        return;
    }
    let mut c = cfg();
    // 活跃方案就是拼音 ⇒ 临拼没有目标方案可切。
    c.schema.active = "pinyin".into();
    c.keys
        .key_actions
        .insert("ctrl+alt+shift+j".into(), "temp_pinyin".into());
    let coord = Coordinator::new_headless(c, Some(&data_dir()));

    let act = coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert_eq!(
        coord.debug_active_mode(),
        None,
        "前提：拼音方案下临拼进不去，门卫应当拒绝"
    );
    assert!(
        matches!(act, KeyAction::Consumed),
        "门卫没过也必须吃掉这个键，否则组合键泄漏给宿主，实际: {act:?}"
    );
}

/// A 类的出口形状：切换前未上屏的编码要经本次按键应答交还宿主。
///
/// 上一条只断言 `is_chinese_mode()`，把 `run_dispatch_action` 从 `dispatch_hotkey_keyed` 改成
/// `dispatch_hotkey` 它照样绿；而 `dispatch_hotkey` 只能 push，空文本清不掉宿主的 composition，
/// 编码会挂在应用的组合区里（同类缺陷见 `schema_switch_commit.rs`）。
#[test]
fn combo_key_toggle_mode_returns_pending_code_through_the_key_reply() {
    if !has_schemas() {
        return;
    }
    let mut c = cfg();
    c.keys.commit_on_switch = true;
    c.keys
        .key_actions
        .insert("ctrl+alt+shift+j".into(), "toggle_mode".into());
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    // 敲一个五笔码使缓冲非空（一码不满码自动上屏，这一帧稳定可复现）。
    coord.handle_key_event(&key(VK_W, 0));

    let act = coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    match act {
        KeyAction::InsertText { text, .. } => assert_eq!(
            text, "w",
            "commit_on_switch 开启时切中英文应把原码经按键应答交还宿主"
        ),
        other => panic!(
            "必须走 CommitText 出口才能结束宿主 composition（StatusUpdate 不结束），实际: {other:?}"
        ),
    }
}

/// C 类：方案切换绑组合键，作为组合键分派整条链路的回归对照。
#[test]
fn combo_key_switches_schema() {
    if !has_schemas() {
        return;
    }
    let coord = coord_with_combo("switch_schema:pinyin");
    coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert_eq!(coord.active_schema_id(), "pinyin", "组合键应切到目标方案");
}
