//! 组合键的 `keys.key_actions`：值域与单键、修饰键**同一张表**（`BoundAction`）。
//!
//! 2026-09-18 之前组合键只认六个动词（`hotkey_action_entry` 那份逐条追加的白名单），
//! 用户在设置端「自定义按键」里选「组合键」，能配的功能比选「单个按键」少一大截——
//! 而组合键这条通路在按键链上位次**更早**（英文模式分水岭之前）、不与输入争键、可全局
//! 拦截，本该是限制最少的一条。那份白名单不是设计论证的产物，是演进残留。
//!
//! 本文件守的是合流后的三件事：
//!
//! 1. **能力** —— A/B/C 三类动词绑在组合键上都真的动作（编译期的对照见 wind-config 的
//!    `combo_keys_accept_the_whole_bound_action_domain`，那条只验进没进表，到不了分派端）；
//! 2. **热键上下文专有的三条** —— key_code=0 哨兵（不写引导符）、`chinese_mode` 守卫、
//!    已在该模式时幂等；
//! 3. **中英切换的回程** —— `toggle_mode` 绑组合键时**不得**带 `CHINESE_ONLY`，
//!    否则切到英文态后 TSF 不再转发，再也切不回来（单程票）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_ALT, MOD_CTRL, MOD_SHIFT};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// ⚠️ 返回 `false` 时全部用例直接 `return` 并报 `ok`——**一条断言都没跑**。这是本仓
/// `tests/` 下的既有约定（缺 `build_dev/data` 时不红），但它意味着「6/6 通过」本身
/// 不构成断言已执行的证据。故跳过时留一行痕，让「全绿但什么都没测」看得出来。
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
    }
}

const VK_J: u32 = 0x4A;
const VK_W: u32 = 0x57;

/// ⚠️ **三个修饰键不是随手写的**：出厂 `keys.toggle_s2t` 占着 `ctrl+shift+j`，而固定字段
/// 那几段编译在 `key_actions` **之前**，`match_key_down` 又是 `.find()` 先注册者赢——
/// 用两修饰键写这些用例，绑定会被出厂那条静默遮蔽，全部用例红在「功能没生效」上，
/// 而实现其实是对的。选键前先查一遍出厂占用。
const CTRL_ALT_SHIFT: u32 = MOD_CTRL | MOD_ALT | MOD_SHIFT;

/// 装一条 `ctrl+alt+shift+j` → `verb` 的组合键绑定。
fn coord_with_combo(verb: &str) -> std::sync::Arc<Coordinator> {
    let mut c = cfg();
    c.keys
        .key_actions
        .insert("ctrl+alt+shift+j".into(), verb.into());
    Coordinator::new_headless(c, Some(&data_dir()))
}

/// A 类：中英文切换绑组合键。**这是合流最实际的收获**——在此之前 `toggle_mode` 只能绑
/// `lshift`/`rshift`/`lctrl`/`rctrl`/`capslock` 五个修饰键（`keys.toggle_mode_keys` 的全部
/// 值域），想配成 Ctrl+Alt+Shift+J 这种组合键，全系统没有任何一条路。
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

/// ★ 回程：英文态下同一个组合键必须**还能按得动**。
///
/// 单独一条而不是并进上面那个用例：只验「切过去」的话，给 `toggle_mode` 错配上
/// `CHINESE_ONLY` 策略位的实现也会绿——那种实现切到英文态后 TSF 不再转发这个键，
/// 用户再也回不到中文，而这恰恰是本仓在 `toggle_schema` 上踩过的那个坑。
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

/// B 类：进 overlay 模式，且**组合区不写引导符**（key_code=0 哨兵）。
///
/// 哨兵那半是这条通路专有的：引导键进入要把引导符显示在组合区里，热键进入没有引导符
/// 可写，写了就会在用户屏幕上凭空多一个字符。
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
/// 重开会把用户已经打进模式里的编码清掉，放行则让这个组合键泄漏给宿主——「再按一次」
/// 两种结果都不该有。
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
/// 策略位已让 TSF 在英文态不转发这个键，这里仍要判——别的路径转发进来的同一个键，
/// 会在英文态凭空建一个组合区。判据取 `BoundAction::only_in_chinese_mode`，
/// 与编译期给不给 `CHINESE_ONLY` 位是同一个方法。
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

/// ★★ 门卫没过时**仍然吞键**，不把组合键泄漏给宿主。
///
/// 现场用的是一种**正常配置**而非错误路径：活跃方案本身就是拼音方案时，
/// `temp_pinyin_target()` 恒为 `None`（临拼是「码表方案里临时切拼音」，拼音方案下无意义），
/// 于是 `commit_and_enter_bound_action` 的门卫返回 `None`。
///
/// 此时若按引导键通路的策略「不吞键、落回按键链」，`Ctrl+Alt+Shift+J` 就会原样交给宿主，
/// 宿主按自己的加速键执行——用户配的是「进临时拼音」，得到的是别的功能。引导键通路能
/// 那么做是因为它落下去只是打出一个字符。
///
/// ⚠️ 这条守的是 2026-09-18 合流时差点丢掉的语义：被删掉的旧手写链里写着「中文模式下
/// 一律吞键（不放行，避免把该组合键泄漏给宿主）」，合流初版把它换成了引导键通路的
/// 「门卫没过返回 None」，代码审查抓了出来。
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

/// ★ A 类的**出口形状**：切换前未上屏的编码要经本次按键应答交还宿主。
///
/// 单独一条的理由与 `combo_key_toggles_back_from_english` 同类——上面那条只断言
/// `is_chinese_mode()`，把 `run_dispatch_action` 从 `dispatch_hotkey_keyed` 改成
/// `dispatch_hotkey`（正是那种「顺手统一一下」的改动）它照样绿，而真机症状是
/// 「切了中英文，编码还挂在应用的组合区里」——`dispatch_hotkey` 只能 push，
/// 而 push 的空文本清不掉宿主的 composition。`schema_switch_commit.rs` 整个文件
/// 就是为这一类缺陷建的。
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

/// C 类：方案切换绑组合键（合流前就支持，作为回归对照留着）。
///
/// 与上面几条同在一个文件里跑，是为了在「组合键分派整条链路」被改动时一起红——
/// 它是这条链路上唯一一条合流前就绿的用例。
#[test]
fn combo_key_switches_schema() {
    if !has_schemas() {
        return;
    }
    let coord = coord_with_combo("switch_schema:pinyin");
    coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert_eq!(coord.active_schema_id(), "pinyin", "组合键应切到目标方案");
}
