//! 组字中单击 / 按住 Alt 切换注释详细态（`ui.comment.alt_switch`）端到端。
//!
//! 平台层只在组字中转发 Alt 的按下与松开（`0xA4` / `0xA5`，不带修饰位），吃不吃由返回值定。
//! 期望注释取自出厂分配：码表 默认=`code_hint`、详细=`chaizi_pinyin`，同 `comment_state.rs`。
//!
//! 词典缺失时自动跳过（判据：耗时 0.0x 秒＝假绿）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use wind_bridge::handler::{CaretData, KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::comment_lib::{AltSwitch, CommentState};
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{
    EVENT_KEY_DOWN, EVENT_KEY_UP, MOD_ALT, MOD_CTRL, MOD_SHIFT, caret_source,
};
use wind_ui_types::{CandidateItem, UiCommand};

const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;

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
    cfg
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
    cfg.ui.comment.alt_switch = AltSwitch::Hold;
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
    off.ui.comment.alt_switch = AltSwitch::Off;
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
    cfg.ui.comment.alt_switch = AltSwitch::Hold;
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
    cfg.ui.comment.alt_switch = AltSwitch::Hold;
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
