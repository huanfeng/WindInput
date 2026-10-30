//! 按键动词 `command:<cmdbar 表达式>`（命令直通）的端到端测试。
//!
//! 两张表都收它：`keys.key_actions`（组合键 / 纯修饰键轻敲 / 单个有字符的键三种形态）与
//! `keys.session_actions`（会话态）。执行复用工具栏自定义按钮那条通路
//! （`wrap_command_source` + `spawn_command`），故裸表达式与 `$CC(...)` 两种写法都收。
//!
//! # 探针为什么是 `ime.toggle("candwin")`
//!
//! 它只翻一个内存位（候选窗隐藏开关）：不写盘、不切方案、**不碰组合区**。会话态那条要同时
//! 断言「命令执行了」与「当前组合原样不动」，拿 `ime.schema` 当探针的话，切方案本身就会清掉
//! 组合，两件事分不开。`ime.toggle("layout")` 则会写用户配置文件，测试里不该碰。
//!
//! 命令经 `spawn_command` 起独立线程执行，结果不是同步可见的，故用轮询等待。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP, MOD_ALT, MOD_CTRL, MOD_SHIFT};

const PROBE: &str = r#"command:ime.toggle("candwin")"#;

const VK_BACK: u32 = 0x08;
const VK_F1: u32 = 0x70;
const VK_TAB: u32 = 0x09;
const VK_J: u32 = 0x4A;
const VK_RSHIFT: u32 = 0xA1;
const VK_BACKSLASH: u32 = 0xDC;
const VK_PERIOD: u32 = 0xBE;

/// 出厂 `ctrl+shift+j` 被 `keys.toggle_s2t` 占着（固定字段先注册者赢），故用三修饰键。
const CTRL_ALT_SHIFT: u32 = MOD_CTRL | MOD_ALT | MOD_SHIFT;

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

fn bind_session(key: &str, verb: &str) -> impl FnOnce(&mut Config) {
    let (key, verb) = (key.to_string(), verb.to_string());
    move |c| {
        c.keys.session_actions.insert(key, verb);
    }
}

/// 轮询等命令线程跑完：固定 sleep 要么慢、要么在忙机器上偶发失败。
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

fn assert_probe_ran(coord: &Coordinator) {
    assert!(
        wait_until(|| coord.debug_candidate_window_hidden()),
        "命令应被执行（候选窗隐藏开关应被翻转）"
    );
}

/// 「什么都没发生」要给足与正例相同量级的时间窗，否则只证明了「还没发生」。
fn assert_probe_not_ran(coord: &Coordinator) {
    std::thread::sleep(Duration::from_millis(300));
    assert!(!coord.debug_candidate_window_hidden(), "命令不应被执行");
}

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

// ───────────────────────── keys.key_actions ─────────────────────────

/// 组合键：命中即执行并吞键。
#[test]
fn combo_key_runs_command() {
    skip_without_data!();
    let coord = open_with(bind_key("ctrl+alt+shift+j", PROBE));
    assert!(!coord.debug_candidate_window_hidden(), "前提：候选窗未隐藏");

    let act = coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert!(
        matches!(act, KeyAction::Consumed),
        "组合键命中命令应吞键，实际 {act:?}"
    );
    assert_probe_ran(&coord);
}

/// ★ 命令与中英态无关：英文态下同一个组合键照样按得动。
///
/// 守的是策略位——带上 `CHINESE_ONLY` 的话 TSF 在英文态不转发，分派端也会被
/// `only_in_chinese_mode` 守卫挡掉。
#[test]
fn combo_key_runs_command_in_english_mode() {
    skip_without_data!();
    let coord = open_with(|c| {
        bind_key("ctrl+alt+shift+j", PROBE)(c);
        c.input.default.chinese_mode = false;
    });
    assert!(!coord.is_chinese_mode(), "前提：英文态");

    coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert_probe_ran(&coord);
}

/// ★ `shift+backspace` 绑命令（t372：一键撤销刚上屏的长词组，绑 `ime.undo_commit()`）：
/// 空闲态（刚上屏完、没有组合）与组合中途都命中即执行并吞键，不落到退格的原语义。
#[test]
fn shift_backspace_runs_command_when_idle() {
    skip_without_data!();
    let coord = open_with(bind_key("shift+backspace", PROBE));
    let act = coord.handle_key_event(&key(VK_BACK, MOD_SHIFT));
    assert!(
        matches!(act, KeyAction::Consumed),
        "空闲态 Shift+Backspace 命中命令应吞键，实际 {act:?}"
    );
    assert_probe_ran(&coord);
}

/// 裸 F1 进热键表（`route_of_key_action`），空闲态也命中并吞键。
#[test]
fn bare_function_key_runs_command_when_idle() {
    skip_without_data!();
    let coord = open_with(bind_key("f1", PROBE));
    let act = coord.handle_key_event(&key(VK_F1, 0));
    assert!(
        matches!(act, KeyAction::Consumed),
        "空闲态裸 F1 命中命令应吞键，实际 {act:?}"
    );
    assert_probe_ran(&coord);
}

/// 裸 F 键在英文模式归宿主：不执行命令、不吞键（F1 帮助、F5 刷新照常到应用）。
#[test]
fn bare_function_key_passes_through_in_english_mode() {
    skip_without_data!();
    let coord = open_with(|c| {
        bind_key("f1", PROBE)(c);
        c.input.default.chinese_mode = false;
    });
    let act = coord.handle_key_event(&key(VK_F1, 0));
    assert!(
        matches!(act, KeyAction::PassThrough),
        "英文模式裸 F1 应透传，实际 {act:?}"
    );
    assert_probe_not_ran(&coord);
}

#[test]
fn shift_backspace_runs_command_mid_composition() {
    skip_without_data!();
    let coord = open_with(bind_key("shift+backspace", PROBE));
    type_str(&coord, "ab");
    coord.handle_key_event(&key(VK_BACK, MOD_SHIFT));
    assert_probe_ran(&coord);
}

/// 已写成 `$CC(...)` 的也照收（从短语那边抄过来的写法），不被包成嵌套。
#[test]
fn combo_key_accepts_cc_wrapped_expression() {
    skip_without_data!();
    let coord = open_with(bind_key(
        "ctrl+alt+shift+j",
        r#"command:$CC("", ime.toggle("candwin"))"#,
    ));
    coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert_probe_ran(&coord);
}

/// 纯修饰键轻敲（keyup）：执行命令，且**不再**落到出厂的中英切换（rshift 出厂是 toggle_mode）。
#[test]
fn modifier_tap_runs_command_instead_of_toggle_mode() {
    skip_without_data!();
    let coord = open_with(bind_key("rshift", PROBE));
    assert!(coord.is_chinese_mode(), "前提：中文态");

    coord.handle_key_event(&key_up(VK_RSHIFT));
    assert_probe_ran(&coord);
    assert!(coord.is_chinese_mode(), "绑了命令的修饰键不应再切中英文");
}

/// 单个有字符的键（引导键链）：空缓冲时执行命令并吞键，不出那个符号。
#[test]
fn leading_key_runs_command_when_idle() {
    skip_without_data!();
    let coord = open_with(bind_key("backslash", PROBE));
    let act = coord.handle_key_event(&key(VK_BACKSLASH, 0));
    assert!(
        matches!(act, KeyAction::Consumed),
        "空闲时应吞键执行命令，实际 {act:?}"
    );
    assert_probe_ran(&coord);
}

/// ★ 单个有字符的键在**英文态**透传：那个字符照常出，命令不执行。
///
/// 引导键链在英文模式分水岭之后，英文态下有字符的键必须能出字——这是这条通路的物理
/// 约束，不是命令动词的取舍（与 A 类动词同）。设置端文案据此区分「组合键 / 修饰键两种
/// 态都能按」与「单键只在中文态」。
#[test]
fn leading_key_passes_through_in_english_mode() {
    skip_without_data!();
    let coord = open_with(|c| {
        bind_key("backslash", PROBE)(c);
        c.input.default.chinese_mode = false;
    });
    assert!(!coord.is_chinese_mode(), "前提：英文态");
    let act = coord.handle_key_event(&key(VK_BACKSLASH, 0));
    assert!(
        matches!(act, KeyAction::PassThrough),
        "英文态下单键应透传出字符，实际 {act:?}"
    );
    assert_probe_not_ran(&coord);
}

/// 打字中途按绑了命令的单键：不执行命令（与 A 类同约束——那一下多半是想输入）。
#[test]
fn leading_key_does_not_run_command_mid_composition() {
    skip_without_data!();
    let coord = open_with(bind_key("backslash", PROBE));
    type_str(&coord, "a");
    assert!(!coord.debug_page_texts().is_empty(), "前提：打字中途");
    coord.handle_key_event(&key(VK_BACKSLASH, 0));
    assert_probe_not_ran(&coord);
}

// ───────────────────────── keys.session_actions ─────────────────────────

/// ★ 会话态：执行命令，**当前组合原样不动**（不清空、不上屏），吞键。
#[test]
fn session_key_runs_command_and_keeps_composition() {
    skip_without_data!();
    let coord = open_with(bind_session("tab", PROBE));
    type_str(&coord, "a");
    let preedit = coord.debug_preedit();
    let texts = coord.debug_page_texts();
    assert!(!texts.is_empty(), "前提：应有候选");

    let act = coord.handle_key_event(&key(VK_TAB, 0));
    assert!(
        matches!(act, KeyAction::Consumed),
        "会话态命令应吞键且不动组合，实际 {act:?}"
    );
    assert_probe_ran(&coord);
    assert_eq!(coord.debug_preedit(), preedit, "组合区不应变");
    assert_eq!(coord.debug_page_texts(), texts, "候选不应变");
}

/// ★ 功能键 F1 能绑会话态动作（候选中按 F1 触发命令），组合区原样不动。
#[test]
fn session_function_key_runs_command() {
    skip_without_data!();
    let coord = open_with(bind_session("f1", PROBE));
    type_str(&coord, "a");
    let preedit = coord.debug_preedit();
    let act = coord.handle_key_event(&key(VK_F1, 0));
    assert!(
        matches!(act, KeyAction::Consumed),
        "会话态 F1 应吞键，实际 {act:?}"
    );
    assert_probe_ran(&coord);
    assert_eq!(coord.debug_preedit(), preedit, "组合区不应变");
}

/// ★ `toggle_tip`：把悬停目标设到高亮项，再按一次收起；移动高亮同鼠标离开一样收起（t372）。
#[test]
fn session_toggle_tip_follows_highlight() {
    skip_without_data!();
    let coord = open_with(bind_session("f2", "toggle_tip"));
    type_str(&coord, "a");
    assert_eq!(coord.debug_hover_target(), -1, "前提：无悬停");

    let act = coord.handle_key_event(&key(0x71, 0));
    assert!(matches!(act, KeyAction::Consumed), "应吞键，实际 {act:?}");
    assert_eq!(
        coord.debug_hover_target(),
        0,
        "悬停目标 = 高亮项（页内第 0 项）"
    );

    coord.handle_key_event(&key(0x71, 0));
    assert_eq!(coord.debug_hover_target(), -1, "再按一次收起");
}

/// 绑在 → 上：编码区光标不在末尾时照旧右移光标，到末尾才开关提示（t372）。
#[test]
fn toggle_tip_on_right_moves_caret_before_end() {
    skip_without_data!();
    let coord = open_with(|c| {
        for (k, v) in [("left", "caret_left"), ("right", "toggle_tip")] {
            c.keys.session_actions.insert(k.into(), v.into());
        }
    });
    type_str(&coord, "ab");
    coord.handle_key_event(&key(0x25, 0)); // ← 光标退到 a|b
    assert_eq!(coord.debug_hover_target(), -1, "前提：无悬停");

    let act = coord.handle_key_event(&key(0x27, 0)); // → 光标回到末尾，不开提示
    // 宿主里的组合区插入符要跟着动：回 UpdateComposition（带新位置），不是裸 Consumed。
    assert!(
        matches!(act, KeyAction::UpdateComposition { caret_pos: 2, .. }),
        "应更新组合区插入符到末尾，实际 {act:?}"
    );
    assert_eq!(coord.debug_hover_target(), -1, "光标没到末尾前 → 只移光标");

    coord.handle_key_event(&key(0x27, 0)); // 已在末尾 → 开提示
    assert_eq!(coord.debug_hover_target(), 0, "末尾的 → 开提示");
}

/// `caret_right:tip`：光标没到末尾右移，到末尾开关提示（macOS 式 → 与悬停提示合并写法）。
#[test]
fn caret_right_tip_moves_then_toggles_tip() {
    skip_without_data!();
    let coord = open_with(|c| {
        for (k, v) in [("left", "caret_left"), ("right", "caret_right:tip")] {
            c.keys.session_actions.insert(k.into(), v.into());
        }
    });
    type_str(&coord, "ab");
    coord.handle_key_event(&key(0x25, 0)); // ← a|b
    coord.handle_key_event(&key(0x27, 0)); // → 回到末尾
    assert_eq!(coord.debug_hover_target(), -1, "未到末尾前不开提示");
    coord.handle_key_event(&key(0x27, 0)); // 末尾 → 开提示
    assert_eq!(coord.debug_hover_target(), 0);
}

/// `caret_right:next`：到末尾后 → 把高亮移到下一个候选（macOS「→ 切到下一个候选词」）。
#[test]
fn caret_right_next_moves_highlight_at_end() {
    skip_without_data!();
    let coord = open_with(|c| {
        c.keys
            .session_actions
            .insert("right".into(), "caret_right:next".into());
    });
    type_str(&coord, "a");
    assert!(coord.debug_page_texts().len() > 1, "前提：至少两个候选");
    let before = coord.debug_page_info().1;
    coord.handle_key_event(&key(0x27, 0));
    assert_eq!(coord.debug_page_info().1, before + 1, "高亮应下移一位");
}

/// 会话态命令**不要候选**：打了码没候选时也能按（网址模式是「有会话、无候选」的天然样本）。
#[test]
fn session_key_runs_command_without_candidates() {
    skip_without_data!();
    let coord = open_with(|c| {
        bind_session("tab", PROBE)(c);
        c.input.url.enabled = true;
    });
    type_str(&coord, "www");
    coord.handle_key_event(&key(VK_PERIOD, 0));
    assert_eq!(coord.debug_active_mode(), Some("url"), "前提：应进网址模式");
    assert!(coord.debug_page_texts().is_empty(), "前提：网址模式无候选");

    coord.handle_key_event(&key(VK_TAB, 0));
    assert_probe_ran(&coord);
    assert_eq!(coord.debug_active_mode(), Some("url"), "不应退出网址模式");
}

/// 无会话：Tab 归宿主（制表符），命令不执行。
#[test]
fn session_key_is_inert_without_session() {
    skip_without_data!();
    let coord = open_with(bind_session("tab", PROBE));
    let act = coord.handle_key_event(&key(VK_TAB, 0));
    assert!(
        matches!(act, KeyAction::PassThrough),
        "空闲时 Tab 应透传，实际 {act:?}"
    );
    assert_probe_not_ran(&coord);
}

/// 表达式为空 → 不是合法绑定：组合键不进热键表，按下去不吞键、不执行。
#[test]
fn empty_expression_is_not_a_binding() {
    skip_without_data!();
    let coord = open_with(bind_key("ctrl+alt+shift+j", "command:   "));
    let act = coord.handle_key_event(&key(VK_J, CTRL_ALT_SHIFT));
    assert!(
        !matches!(act, KeyAction::Consumed),
        "空表达式不应吞键，实际 {act:?}"
    );
    assert_probe_not_ran(&coord);
}

/// GH#254：Ctrl+= 开了快捷加词预览后切到英文，加词会话必须随之结束——
/// 否则 `hotkey_session` 留着，英文模式下 Ctrl+- 乃至单键 `-` 都被吞。
#[test]
fn mode_switch_ends_add_word_session() {
    skip_without_data!();
    let coord = open_with(|_| {});
    let minus = |m| coord.handle_key_event(&key(0xBD, m));
    assert!(
        matches!(minus(MOD_CTRL), KeyAction::PassThrough),
        "前提：中文态 Ctrl+- 透传"
    );
    assert!(
        matches!(
            coord.handle_key_event(&key(0xBB, MOD_CTRL)),
            KeyAction::Consumed
        ),
        "Ctrl+= 默认是加词热键"
    );
    // 轻敲左 Shift 切英文
    coord.handle_key_event(&key(0xA0, 0));
    coord.handle_key_event(&key_up(0xA0));
    assert!(
        matches!(minus(MOD_CTRL), KeyAction::PassThrough),
        "切英文后 Ctrl+- 应透传给宿主"
    );
    assert!(
        matches!(minus(0), KeyAction::PassThrough),
        "切英文后单键 - 应透传"
    );
}

/// GH#267：`ui.candidate.syllable_separator` 只换显示——应用里的组合串不再带 `'`，引擎缓冲与切分不变。
#[test]
fn syllable_separator_is_display_only() {
    skip_without_data!();
    let run = |sep: &str| {
        let sep = sep.to_string();
        let mut c = Config::default();
        c.schema.available = vec!["wubi86".into(), "pinyin".into()];
        c.schema.active = "pinyin".into();
        c.input.default.chinese_mode = true;
        c.ui.candidate.syllable_separator = sep;
        let coord = Coordinator::new_headless(c, Some(&data_dir()));
        let mut last = None;
        for ch in "xiaopeng".chars() {
            last = Some(coord.handle_key_event_policed(&key(ch.to_ascii_uppercase() as u32, 0)));
        }
        (last.unwrap(), coord.debug_preedit())
    };
    let text_of = |a: &KeyAction| match a {
        KeyAction::UpdateComposition { text, .. } => text.clone(),
        other => panic!("应是组合更新，实际 {other:?}"),
    };
    let (act, _) = run("'");
    assert!(
        text_of(&act).contains('\''),
        "默认仍带 '：{}",
        text_of(&act)
    );
    let (act, internal) = run("");
    assert_eq!(text_of(&act), "xiaopeng", "留空后应用里没有分隔符");
    assert!(internal.contains('\''), "引擎内部仍分词：{internal}");
    let (act, _) = run("·");
    assert!(text_of(&act).contains('·') && !text_of(&act).contains('\''));
    let (act, _) = run(" ");
    assert!(
        text_of(&act).contains("xiao peng") && !text_of(&act).contains('\''),
        "空格分隔：{}",
        text_of(&act)
    );
}

/// 只把 → 绑成 `toggle_tip`（← 不绑，走默认移光标）：← 退一格后 → 能回到末尾，且回 UpdateComposition。
#[test]
fn toggle_tip_alone_on_right_keeps_caret_movable() {
    skip_without_data!();
    let coord = open_with(|c| {
        c.keys
            .session_actions
            .insert("right".into(), "toggle_tip".into());
    });
    type_str(&coord, "ab");
    coord.handle_key_event(&key(0x25, 0)); // 默认 ← 退一格
    let act = coord.handle_key_event(&key(0x27, 0));
    assert!(
        matches!(act, KeyAction::UpdateComposition { caret_pos: 2, .. }),
        "实际 {act:?}"
    );
    assert_eq!(coord.debug_hover_target(), -1);
}
