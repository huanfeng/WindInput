//! 组字中单击切换键（`ui.comment.switch_key`）切换注释详细态（`ui.comment.switch_mode`）端到端。
//!
//! - Alt：平台层只在组字中转发 Alt 的按下与松开（`0xA4` / `0xA5`，不带修饰位），吃不吃由返回值定。
//! - Ctrl：按下时有输入会话，平台如实上报按下与松开（`EVENT_MOD_DOWN` / `EVENT_MOD_UP`），服务端判单击；
//!   否则平台只送判过单击的松开（`0xA2` / `0xA3`；TSF 带 Ctrl 位，mac / Linux 不带）。
//!   撞键时中英切换、选词键组、会话绑定优先。
//!
//! 期望注释取自出厂分配：码表 默认=`code_hint`、详细=`chaizi_pinyin`，同 `comment_state.rs`。
//!
//! 词典缺失时自动跳过（判据：耗时 0.0x 秒＝假绿）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use wind_bridge::handler::{CaretData, KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::comment_lib::{CommentState, SwitchKey, SwitchMode};
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{
    EVENT_KEY_DOWN, EVENT_KEY_UP, EVENT_MOD_DOWN, EVENT_MOD_UP, MOD_ALT, MOD_CTRL, MOD_LCTRL,
    MOD_RCTRL, MOD_SHIFT, TOGGLE_MOD_NOT_TAP, caret_source,
};
use wind_ui_types::{CandidateItem, UiCommand};

const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
const VK_F1: u32 = 0x70;

const GG_DEFAULT: [&str; 3] = ["", "c", "g"];
const GG_DETAIL: [&str; 3] = [
    "\u{e003} \u{e001} \u{e06b} \u{e001} · wǔ",
    "\u{e002} \u{e002} \u{e210} \u{e1df} · pá",
    "\u{e002} \u{e002} \u{e002} \u{e002} · wáng",
];

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists()
        && d.join("schemas/wubi86/wubi86_chaizi.txt").exists()
}

fn ev(key_code: u32, modifiers: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn down(key_code: u32) -> KeyEventData {
    ev(key_code, 0, EVENT_KEY_DOWN)
}

fn up(key_code: u32) -> KeyEventData {
    ev(key_code, 0, EVENT_KEY_UP)
}

fn alt_digit(digit: u32) -> KeyEventData {
    ev(digit, MOD_ALT, EVENT_KEY_DOWN)
}

fn cfg() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.association.kind = "off".into();
    cfg.ui.comment.switch_key = SwitchKey::Alt;
    cfg
}

/// 切换键 Ctrl（左右任一）。
fn ctrl_cfg() -> Config {
    let mut cfg = cfg();
    cfg.ui.comment.switch_key = SwitchKey::Ctrl;
    cfg
}

/// TSF 送来的 Ctrl 单击松开：自带 Ctrl 位与左右位。
fn tsf_ctrl_up(vk: u32) -> KeyEventData {
    let side = if vk == VK_LCONTROL {
        MOD_LCTRL
    } else {
        MOD_RCTRL
    };
    ev(vk, MOD_CTRL | side, EVENT_KEY_UP)
}

fn ctrl_mods(vk: u32) -> u32 {
    MOD_CTRL
        | if vk == VK_LCONTROL {
            MOD_LCTRL
        } else {
            MOD_RCTRL
        }
}

/// 如实上报的 Ctrl 按下 / 松开（三端同形：修饰位带 Ctrl 与左右位）。
fn raw_down(vk: u32) -> KeyEventData {
    ev(vk, ctrl_mods(vk), EVENT_MOD_DOWN)
}

fn raw_up(vk: u32) -> KeyEventData {
    ev(vk, ctrl_mods(vk), EVENT_MOD_UP)
}

/// 平台报「看见过别的输入」的松开（透传给宿主的 Ctrl+C、Ctrl+点击）。
fn raw_up_not_tap(vk: u32) -> KeyEventData {
    KeyEventData {
        toggles: TOGGLE_MOD_NOT_TAP,
        ..raw_up(vk)
    }
}

/// `WM_HOTKEY` 送来的 Ctrl+数字（置顶）：普通 keydown，不经 TSF 按键回调。
fn hotkey_ctrl_digit(digit: u32) -> KeyEventData {
    ev(digit, MOD_CTRL | MOD_LCTRL, EVENT_KEY_DOWN)
}

fn raw_tap(c: &Coordinator, vk: u32) -> KeyAction {
    let act = c.handle_key_event_policed(&raw_down(vk));
    assert!(is_consumed(&act), "如实上报的按下回 Consumed，实得 {act:?}");
    c.handle_key_event_policed(&raw_up(vk))
}

fn open(cfg: Config) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    c.prewarm_indexes();
    (c, rx)
}

fn type_str(c: &Coordinator, s: &str) {
    for ch in s.chars() {
        c.handle_key_event(&down(ch.to_ascii_uppercase() as u32));
    }
}

/// 放行首显闸门后取最近一次下发的候选页的前三条注释。
fn comments(c: &Coordinator, rx: &Receiver<UiCommand>) -> Vec<String> {
    c.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    last_pushed(rx).expect("应下发候选")
}

/// 只收此前已下发的候选页（不放行闸门）：用来断言「这一下当场重绘了」。
fn last_pushed(rx: &Receiver<UiCommand>) -> Option<Vec<String>> {
    rx.try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates { candidates, .. } => Some(candidates),
            _ => None,
        })
        .last()
        .map(|items: Vec<CandidateItem>| {
            items
                .iter()
                .take(3)
                .map(|i| i.comment.as_str().to_string())
                .collect()
        })
}

fn typed_gg(cfg: Config) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    let (c, rx) = open(cfg);
    type_str(&c, "gg");
    assert_eq!(comments(&c, &rx), GG_DEFAULT, "前置条件：gg 默认态注释");
    (c, rx)
}

fn is_consumed(act: &KeyAction) -> bool {
    matches!(act, KeyAction::Consumed)
}

fn is_pass(act: &KeyAction) -> bool {
    matches!(act, KeyAction::PassThrough)
}

#[test]
fn alt_tap_rerenders_detail() {
    if !has_schemas() {
        return;
    }
    let (c, rx) = typed_gg(cfg());
    let act = c.handle_key_event(&down(VK_LMENU));
    assert!(is_pass(&act), "Session 档按下应放行，实得 {act:?}");
    let act = c.handle_key_event(&up(VK_LMENU));
    assert!(is_consumed(&act), "单击松开应吃掉，实得 {act:?}");
    assert_eq!(
        last_pushed(&rx).as_deref(),
        Some(&GG_DETAIL.map(String::from)[..]),
        "松开应当场重绘为详细"
    );
    // 右 Alt 同样认，再单击一次翻回。
    c.handle_key_event(&down(VK_RMENU));
    let act = c.handle_key_event(&up(VK_RMENU));
    assert!(is_consumed(&act), "右 Alt 单击，实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

/// 出厂切换键 F1：有候选时按下即切到详细并当场重绘；再按切回；没有候选时放行（还给宿主的帮助键）。
#[test]
fn f1_toggles_session_and_passes_through_without_candidates() {
    if !has_schemas() {
        return;
    }
    let mut f1 = cfg();
    f1.ui.comment.switch_key = SwitchKey::F1;
    let (idle, _rx) = open(f1.clone());
    let act = idle.handle_key_event(&down(VK_F1));
    assert!(is_pass(&act), "无候选应放行，实得 {act:?}");

    let (c, rx2) = typed_gg(f1);
    let act = c.handle_key_event(&down(VK_F1));
    assert!(is_consumed(&act), "有候选应吃掉，实得 {act:?}");
    assert_eq!(
        last_pushed(&rx2).as_deref(),
        Some(&GG_DETAIL.map(String::from)[..]),
        "按下应当场重绘为详细"
    );
    c.handle_key_event(&down(VK_F1));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

/// 按住档的功能键：按下显示详细，自动连发不翻回，松开恢复；没有候选时放行。
#[test]
fn f1_hold_shows_detail_until_release() {
    if !has_schemas() {
        return;
    }
    let mut hold = cfg();
    hold.ui.comment.switch_key = SwitchKey::F1;
    hold.ui.comment.switch_mode = SwitchMode::Hold;
    let (idle, _rx) = open(hold.clone());
    assert!(
        is_pass(&idle.handle_key_event(&down(VK_F1))),
        "无候选应放行"
    );

    let (c, _rx) = typed_gg(hold);
    assert!(is_consumed(&c.handle_key_event(&down(VK_F1))));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Detail);
    assert!(is_consumed(&c.handle_key_event(&down(VK_F1))), "连发");
    assert_eq!(
        c.debug_effective_comment_state(),
        CommentState::Detail,
        "连发不翻回"
    );
    assert!(is_consumed(&c.handle_key_event(&up(VK_F1))));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn alt_digit_does_not_toggle() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    cfg.input.alt_commit = "pinyin".into();
    let (c, _rx) = typed_gg(cfg);
    c.handle_key_event(&down(VK_LMENU));
    match c.handle_key_event(&alt_digit(0x31)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "wǔ"),
        other => panic!("Alt+1 应上屏首选拼音，实得 {other:?}"),
    }
    let act = c.handle_key_event(&up(VK_LMENU));
    assert!(is_pass(&act), "组合键之后的松开不是单击，实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

/// 候选窗没关的组合键（Alt+0 超出本页）：松开时仍有候选，单看 `dirty` 判出不是单击。
#[test]
fn alt_combo_keeping_window_does_not_toggle() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    cfg.input.alt_commit = "pinyin".into();
    cfg.ui.candidate.per_page = 5;
    let (c, _rx) = typed_gg(cfg);
    let before = c.debug_candidate_count();
    c.handle_key_event(&down(VK_LMENU));
    let act = c.handle_key_event(&alt_digit(0x30));
    assert!(is_consumed(&act), "Alt+0 超出本页应吞键，实得 {act:?}");
    assert_eq!(c.debug_candidate_count(), before, "前置条件：候选窗仍开着");
    let act = c.handle_key_event(&up(VK_LMENU));
    assert!(is_pass(&act), "实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

/// 平台把按住期间按过的 Shift / Ctrl 粘在 Alt 松开上（桌面 TSF 不转发它们的按下）。
fn sticky_modifier_is_not_tap(modifier: u32) {
    if !has_schemas() {
        return;
    }
    let (c, _rx) = typed_gg(cfg());
    c.handle_key_event(&down(VK_LMENU));
    let act = c.handle_key_event(&ev(VK_LMENU, modifier, EVENT_KEY_UP));
    assert!(is_pass(&act), "实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn alt_shift_is_not_tap() {
    sticky_modifier_is_not_tap(MOD_SHIFT);
}

#[test]
fn alt_ctrl_is_not_tap() {
    sticky_modifier_is_not_tap(MOD_CTRL);
}

/// Hold 档带粘滞修饰位照样在松开时复位。
#[test]
fn hold_with_sticky_modifier_still_restores() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    cfg.ui.comment.switch_mode = SwitchMode::Hold;
    cfg.ui.comment.switch_key = SwitchKey::Alt;
    let (c, _rx) = typed_gg(cfg);
    c.handle_key_event(&down(VK_LMENU));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Detail);
    let act = c.handle_key_event(&ev(VK_LMENU, MOD_SHIFT, EVENT_KEY_UP));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn alt_without_candidates_passes_through() {
    if !has_schemas() {
        return;
    }
    let (c, _rx) = open(cfg());
    assert!(is_pass(&c.handle_key_event(&down(VK_LMENU))));
    assert!(is_pass(&c.handle_key_event(&up(VK_LMENU))));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn alt_switch_off_or_comment_disabled_passes_through() {
    if !has_schemas() {
        return;
    }
    let mut off = cfg();
    off.ui.comment.switch_key = SwitchKey::None;
    let mut disabled = cfg();
    disabled.ui.comment.enabled = false;
    for cfg in [off, disabled] {
        let (c, _rx) = open(cfg);
        type_str(&c, "gg");
        assert!(is_pass(&c.handle_key_event(&down(VK_LMENU))));
        assert!(is_pass(&c.handle_key_event(&up(VK_LMENU))));
        assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
    }
}

#[test]
fn hold_alt_digit_commits_detail_comment() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    cfg.ui.comment.switch_mode = SwitchMode::Hold;
    cfg.ui.comment.switch_key = SwitchKey::Alt;
    cfg.input.alt_commit = "comment".into();
    let (c, rx) = typed_gg(cfg);
    let act = c.handle_key_event(&down(VK_LMENU));
    assert!(is_consumed(&act), "Hold 档按下应吃掉，实得 {act:?}");
    assert_eq!(
        last_pushed(&rx).as_deref(),
        Some(&GG_DETAIL.map(String::from)[..]),
        "按住即显示详细"
    );
    match c.handle_key_event(&alt_digit(0x31)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, GG_DETAIL[0]),
        other => panic!("Alt+1 应上屏详细态注释，实得 {other:?}"),
    }
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
    let act = c.handle_key_event(&up(VK_LMENU));
    assert!(is_consumed(&act), "按下吃了，松开也吃，实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn hold_release_restores_default() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    cfg.ui.comment.switch_mode = SwitchMode::Hold;
    cfg.ui.comment.switch_key = SwitchKey::Alt;
    let (c, rx) = typed_gg(cfg);
    c.handle_key_event(&down(VK_LMENU));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Detail);
    let act = c.handle_key_event(&up(VK_LMENU));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(
        last_pushed(&rx).as_deref(),
        Some(&GG_DEFAULT.map(String::from)[..]),
        "松开当场重绘回默认"
    );
}

/// 裸 `lalt` / `ralt` 在两张按键表里都绑不上（键名表不收），故无「用户绑定让位」可言。
#[test]
fn bare_alt_bindings_are_not_compiled() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    for name in ["lalt", "ralt"] {
        cfg.keys
            .key_actions
            .insert(name.into(), "toggle_full_width".into());
        cfg.keys
            .session_actions
            .insert(name.into(), "page_next".into());
    }
    let (c, _rx) = typed_gg(cfg);
    assert!(
        !c.debug_key_up_hotkeys()
            .iter()
            .any(|h| matches!(h & 0xFFFF, VK_LMENU | VK_RMENU)),
        "裸 Alt 不应进 key_up 转发表"
    );
    let page = c.debug_page_info();
    c.handle_key_event(&down(VK_LMENU));
    let act = c.handle_key_event(&up(VK_LMENU));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Detail);
    assert_eq!(c.debug_page_info(), page, "绑定未生效：没有翻页");
}

// ── 按左右过滤 Alt ──

#[test]
fn ralt_config_ignores_left_alt() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    cfg.ui.comment.switch_key = SwitchKey::RAlt;
    let (c, _rx) = typed_gg(cfg);
    assert!(is_pass(&c.handle_key_event(&down(VK_LMENU))));
    assert!(is_pass(&c.handle_key_event(&up(VK_LMENU))), "左 Alt 不切");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
    c.handle_key_event(&down(VK_RMENU));
    let act = c.handle_key_event(&up(VK_RMENU));
    assert!(is_consumed(&act), "右 Alt 照切，实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Detail);
}

#[test]
fn ctrl_config_ignores_alt() {
    if !has_schemas() {
        return;
    }
    let mut hold = ctrl_cfg();
    // 按住档配 Ctrl，Alt 照样不认。
    hold.ui.comment.switch_mode = SwitchMode::Hold;
    for cfg in [ctrl_cfg(), hold] {
        let (c, _rx) = typed_gg(cfg);
        for vk in [VK_LMENU, VK_RMENU] {
            assert!(is_pass(&c.handle_key_event(&down(vk))));
            assert!(is_pass(&c.handle_key_event(&up(vk))));
        }
        assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
    }
}

// ── Ctrl 单击 ──

#[test]
fn ctrl_tap_toggles_session_and_rerenders() {
    if !has_schemas() {
        return;
    }
    let (c, rx) = typed_gg(ctrl_cfg());
    let act = c.handle_key_event(&tsf_ctrl_up(VK_LCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(
        last_pushed(&rx).as_deref(),
        Some(&GG_DETAIL.map(String::from)[..]),
        "松开应当场重绘为详细"
    );
    // mac / Linux 送的松开不带修饰位；右 Ctrl 同样认，翻回默认。
    let act = c.handle_key_event(&up(VK_RCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(
        last_pushed(&rx).as_deref(),
        Some(&GG_DEFAULT.map(String::from)[..])
    );
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn ctrl_key_up_is_whitelisted_for_tsf() {
    if !has_schemas() {
        return;
    }
    let (c, _rx) = open(ctrl_cfg());
    let ups = c.debug_key_up_hotkeys();
    for vk in [VK_LCONTROL, VK_RCONTROL] {
        assert!(
            ups.iter().any(|h| h & 0xFFFF == vk && h & 0x8000_0000 != 0),
            "{vk:#x} 应带 SESSION 位进 key_up 转发表：{ups:x?}"
        );
    }
    let mut off = ctrl_cfg();
    off.ui.comment.switch_key = SwitchKey::None;
    let (c, _rx) = open(off);
    assert!(
        !c.debug_key_up_hotkeys()
            .iter()
            .any(|h| matches!(h & 0xFFFF, VK_LCONTROL | VK_RCONTROL)),
        "切换键为无不登记"
    );
}

#[test]
fn lctrl_config_ignores_right_ctrl() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.ui.comment.switch_key = SwitchKey::LCtrl;
    let (c, _rx) = typed_gg(cfg);
    assert!(is_pass(&c.handle_key_event(&tsf_ctrl_up(VK_RCONTROL))));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn ctrl_with_other_modifier_is_not_tap() {
    if !has_schemas() {
        return;
    }
    let (c, _rx) = typed_gg(ctrl_cfg());
    let act = c.handle_key_event(&ev(
        VK_LCONTROL,
        MOD_CTRL | MOD_LCTRL | MOD_SHIFT,
        EVENT_KEY_UP,
    ));
    assert!(is_pass(&act), "实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn ctrl_without_candidates_falls_back() {
    if !has_schemas() {
        return;
    }
    // 没配别的功能：放行。
    let (c, _rx) = open(ctrl_cfg());
    assert!(is_pass(&c.handle_key_event(&tsf_ctrl_up(VK_LCONTROL))));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
    // lctrl 兼作中英切换键：照旧切中英。
    let mut cfg = ctrl_cfg();
    cfg.keys.toggle_mode_keys = vec!["lctrl".into()];
    let (c, _rx) = open(cfg);
    assert!(c.is_chinese_mode(), "前置条件：中文");
    c.handle_key_event(&tsf_ctrl_up(VK_LCONTROL));
    assert!(!c.is_chinese_mode(), "无候选时 lctrl 照旧切英文");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn ctrl_yields_to_toggle_mode_key() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.keys.toggle_mode_keys = vec!["lctrl".into()];
    let (c, _rx) = typed_gg(cfg);
    c.handle_key_event(&tsf_ctrl_up(VK_LCONTROL));
    assert!(!c.is_chinese_mode(), "有候选时 lctrl 仍切中英");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn ctrl_yields_to_commit_code_binding() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.keys
        .session_actions
        .insert("lctrl".into(), "commit_code".into());
    let (c, _rx) = typed_gg(cfg);
    // `commit_code` 在外层 `handle_key_event_policed` 重新分派。
    match c.handle_key_event_policed(&tsf_ctrl_up(VK_LCONTROL)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "gg"),
        other => panic!("lctrl 应上屏编码，实得 {other:?}"),
    }
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn ctrl_yields_to_lrctrl_select_group() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.keys.select_key_groups = vec!["lrctrl".into()];
    let (c, _rx) = typed_gg(cfg);
    let second = c.debug_page_texts()[1].clone();
    match c.handle_key_event(&tsf_ctrl_up(VK_LCONTROL)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, second),
        other => panic!("lctrl 应选二候选，实得 {other:?}"),
    }
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

// ── Ctrl 如实上报（候选显示时）──

#[test]
fn raw_ctrl_tap_toggles_session() {
    if !has_schemas() {
        return;
    }
    let (c, rx) = typed_gg(ctrl_cfg());
    let act = raw_tap(&c, VK_LCONTROL);
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(
        last_pushed(&rx).as_deref(),
        Some(&GG_DETAIL.map(String::from)[..]),
        "松开应当场重绘为详细"
    );
    raw_tap(&c, VK_RCONTROL);
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn raw_ctrl_long_press_does_not_toggle() {
    if !has_schemas() {
        return;
    }
    let (c, _rx) = typed_gg(ctrl_cfg());
    c.handle_key_event(&raw_down(VK_LCONTROL));
    std::thread::sleep(std::time::Duration::from_millis(550));
    let act = c.handle_key_event(&raw_up(VK_LCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

/// Ctrl+C 在服务端看不见（透传给宿主）：平台在松开上报 `TOGGLE_MOD_NOT_TAP`。
#[test]
fn raw_ctrl_c_passthrough_does_not_toggle() {
    if !has_schemas() {
        return;
    }
    let (c, _rx) = typed_gg(ctrl_cfg());
    c.handle_key_event(&raw_down(VK_LCONTROL));
    let act = c.handle_key_event(&raw_up_not_tap(VK_LCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

/// Ctrl+C 送到了服务端（清掉组合交还宿主）：服务端自己记账，之后的松开不算单击——
/// lctrl 兼作中英切换键时不顺带切英文。
#[test]
fn raw_ctrl_c_seen_by_service_is_not_tap() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.keys.toggle_mode_keys = vec!["lctrl".into()];
    let (c, _rx) = typed_gg(cfg);
    c.handle_key_event(&raw_down(VK_LCONTROL));
    c.handle_key_event_policed(&ev(0x43, MOD_CTRL | MOD_LCTRL, EVENT_KEY_DOWN));
    assert_eq!(c.debug_candidate_count(), 0, "前置条件：Ctrl+C 清了组合");
    let act = c.handle_key_event(&raw_up(VK_LCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert!(c.is_chinese_mode(), "Ctrl+C 之后的松开不切中英");
}

/// 原漏洞：Ctrl+数字走 `WM_HOTKEY`，TSF 的待判单击作废不了。服务端判定下它弄脏 track。
#[test]
fn raw_ctrl_digit_via_hotkey_does_not_toggle() {
    if !has_schemas() {
        return;
    }
    let (c, _rx) = typed_gg(ctrl_cfg());
    c.handle_key_event(&raw_down(VK_LCONTROL));
    c.handle_key_event_policed(&hotkey_ctrl_digit(0x31));
    let act = c.handle_key_event(&raw_up(VK_LCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn raw_ctrl_tap_keeps_toggle_mode_priority() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.keys.toggle_mode_keys = vec!["lctrl".into()];
    // Ctrl+1 不切中英。
    let (c, _rx) = typed_gg(cfg.clone());
    c.handle_key_event(&raw_down(VK_LCONTROL));
    c.handle_key_event_policed(&hotkey_ctrl_digit(0x31));
    c.handle_key_event(&raw_up(VK_LCONTROL));
    assert!(c.is_chinese_mode(), "Ctrl+1 之后的松开不是单击");
    // 单击照切，且注释不切（中英切换优先）。
    let (c, _rx) = typed_gg(cfg);
    let act = raw_tap(&c, VK_LCONTROL);
    assert!(
        !c.is_chinese_mode(),
        "有候选时 lctrl 单击仍切英文，实得 {act:?}"
    );
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn raw_ctrl_tap_keeps_commit_code_binding() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.keys
        .session_actions
        .insert("lctrl".into(), "commit_code".into());
    let (c, _rx) = typed_gg(cfg);
    match raw_tap(&c, VK_LCONTROL) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "gg"),
        other => panic!("lctrl 单击应上屏编码，实得 {other:?}"),
    }
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn raw_ctrl_tap_keeps_lrctrl_select_group() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.keys.select_key_groups = vec!["lrctrl".into()];
    let (c, _rx) = typed_gg(cfg.clone());
    let second = c.debug_page_texts()[1].clone();
    match raw_tap(&c, VK_LCONTROL) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, second),
        other => panic!("lctrl 单击应选二候选，实得 {other:?}"),
    }
    // 长按 / 组合键不选。
    let (c, _rx) = typed_gg(cfg);
    let before = c.debug_page_texts();
    c.handle_key_event(&raw_down(VK_LCONTROL));
    let act = c.handle_key_event(&raw_up_not_tap(VK_LCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(c.debug_page_texts(), before, "候选未动");
}

#[test]
fn raw_ctrl_hold_flips_on_down_restores_on_up() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.ui.comment.switch_mode = SwitchMode::Hold;
    let (c, rx) = typed_gg(cfg);
    let act = c.handle_key_event(&raw_down(VK_LCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(
        last_pushed(&rx).as_deref(),
        Some(&GG_DETAIL.map(String::from)[..]),
        "按下即显示详细"
    );
    // 按得再久，松开也恢复；单击判定不另切。
    std::thread::sleep(std::time::Duration::from_millis(550));
    c.handle_key_event(&raw_up(VK_LCONTROL));
    assert_eq!(
        last_pushed(&rx).as_deref(),
        Some(&GG_DEFAULT.map(String::from)[..]),
        "松开当场重绘回默认"
    );
    // 短按同样：按下翻、松开回，不留 session 翻转。
    raw_tap(&c, VK_RCONTROL);
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn raw_ctrl_hold_combo_restores() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.ui.comment.switch_mode = SwitchMode::Hold;
    let (c, _rx) = typed_gg(cfg);
    c.handle_key_event(&raw_down(VK_LCONTROL));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Detail);
    c.handle_key_event_policed(&hotkey_ctrl_digit(0x31));
    c.handle_key_event(&raw_up_not_tap(VK_LCONTROL));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

/// 按住期间候选窗关了（Esc 取消）：不残留翻转，下一次组字仍是默认态。
#[test]
fn raw_ctrl_hold_window_closed_leaves_no_flip() {
    if !has_schemas() {
        return;
    }
    let mut cfg = ctrl_cfg();
    cfg.ui.comment.switch_mode = SwitchMode::Hold;
    let (c, rx) = typed_gg(cfg);
    c.handle_key_event(&raw_down(VK_LCONTROL));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Detail);
    c.handle_key_event(&down(0x1B));
    assert_eq!(c.debug_candidate_count(), 0, "前置条件：候选窗已关");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
    // 松开在关窗之后到：照样收起、不切。
    let act = c.handle_key_event(&raw_up(VK_LCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    type_str(&c, "gg");
    assert_eq!(comments(&c, &rx), GG_DEFAULT, "新一次组字是默认态");
}

/// 松开丢了（失焦）：下一次按下重新起算，不被残留拖成长按。
#[test]
fn raw_ctrl_lost_up_then_fresh_tap() {
    if !has_schemas() {
        return;
    }
    let (c, _rx) = typed_gg(ctrl_cfg());
    c.handle_key_event(&raw_down(VK_LCONTROL));
    c.handle_key_event_policed(&hotkey_ctrl_digit(0x31));
    // 松开没送到；过一会儿重新单击。
    std::thread::sleep(std::time::Duration::from_millis(550));
    raw_tap(&c, VK_LCONTROL);
    assert_eq!(c.debug_effective_comment_state(), CommentState::Detail);
}

/// 一次按住只走一条路：如实上报的这次按住之后，服务端不会再从别处收到同一下的松开；
/// 反过来，没走如实上报的按住（按下时无会话）只送确认过的松开。服务端两条各自成立。
#[test]
fn raw_and_legacy_paths_are_independent() {
    if !has_schemas() {
        return;
    }
    let (c, _rx) = typed_gg(ctrl_cfg());
    // 确认过的松开不碰如实上报的 track：先有一次组合键弄脏的按住……
    c.handle_key_event(&raw_down(VK_LCONTROL));
    c.handle_key_event_policed(&hotkey_ctrl_digit(0x31));
    c.handle_key_event(&raw_up(VK_LCONTROL));
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
    // ……之后旧路的一次单击照常切。
    let act = c.handle_key_event(&tsf_ctrl_up(VK_LCONTROL));
    assert!(is_consumed(&act), "实得 {act:?}");
    assert_eq!(c.debug_effective_comment_state(), CommentState::Detail);
}
