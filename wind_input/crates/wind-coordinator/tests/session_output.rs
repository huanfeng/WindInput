//! 中立事件通道：`Coordinator::new_session` + `InputProfile::mobile()`。
//!
//! 事件在调用线程里同步发出（按键处理期间持锁发送），故每次按键后 `try_iter` 就能取到
//! 这次按键产生的全部事件。缺出厂数据：设了 `CI` 则打印原因跳过，否则失败，本地不许悄悄跳过。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use wind_bridge::handler::{FocusData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::{CompositionView, Coordinator, InputProfile, SessionEvent, SessionStatus};
use wind_ipc::protocol::{EVENT_KEY_DOWN, caret_source};
use wind_keys::keymap::{VK_A, VK_BACK, VK_ESCAPE, VK_LEFT, VK_SPACE};
use wind_ui_types::{MenuCmd, MenuKind, UiEvent};

const VK_BACKTICK: u32 = 0xC0;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    if d.join("schemas/wubi86.schema.toml").exists()
        && d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
        && d.join("schemas/english.schema.toml").exists()
    {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        eprintln!("跳过：缺少出厂数据 {}（CI 环境）", d.display());
        return false;
    }
    panic!("缺少出厂数据 {}，本地不允许跳过", d.display());
}

fn config(chinese_mode: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = chinese_mode;
    // 英文态默认会收起工具栏，状态事件就发不出来了
    cfg.ui.toolbar.hide_in_english = false;
    // 联想默认状态不确定，测试自己决定
    cfg.input.association.kind = "off".into();
    cfg
}

fn session(cfg: Config) -> (Arc<Coordinator>, Receiver<SessionEvent>) {
    let (c, rx) = Coordinator::new_session(cfg, Some(&data_dir()), None, InputProfile::mobile());
    // 与移动端宿主一致：先获得焦点（工具栏/状态事件要 ime_active + 编辑上下文）。
    // 获焦那一批事件（含首个 Status）留在 rx 里，要看动作后事件的测试用 `session_quiet`。
    let _ = c.handle_focus_gained(&FocusData {
        x: 0,
        y: 0,
        height: 0,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: 1,
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: caret_source::TSF_SELECTION,
        bundle_id: String::new(),
        window_class: String::new(),
        window_title: String::new(),
    });
    (c, rx)
}

/// 同 [`session`]，另丢掉获焦产生的事件。
fn session_quiet(cfg: Config) -> (Arc<Coordinator>, Receiver<SessionEvent>) {
    let (c, rx) = session(cfg);
    let _ = rx.try_iter().count();
    (c, rx)
}

fn key(vk: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn press(c: &Coordinator, vk: u32) {
    c.handle_key_event_policed(&key(vk));
}

fn letters(c: &Coordinator, s: &str) {
    for ch in s.chars() {
        press(c, VK_A + (ch as u32 - 'a' as u32));
    }
}

fn menu(c: &Coordinator, cmd: MenuCmd) {
    c.inject_ui_event(UiEvent::MenuAction(MenuKind::Command(cmd)));
}

fn drain(rx: &Receiver<SessionEvent>) -> Vec<SessionEvent> {
    rx.try_iter().collect()
}

fn last_view(evs: &[SessionEvent]) -> CompositionView {
    evs.iter()
        .rev()
        .find_map(|e| match e {
            SessionEvent::View(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("没有 View 事件: {evs:?}"))
}

fn last_status(evs: &[SessionEvent]) -> SessionStatus {
    evs.iter()
        .rev()
        .find_map(|e| match e {
            SessionEvent::Status(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("没有 Status 事件: {evs:?}"))
}

#[test]
fn typing_emits_composing_view() {
    if !has_data() {
        return;
    }
    let (c, rx) = session_quiet(config(true));
    letters(&c, "w");
    let v = last_view(&drain(&rx));
    assert_eq!(v.preedit, "w");
    assert!(v.composing);
    assert!(v.has_candidates);
    assert_eq!((v.page, v.selected), (1, 0));
    assert!(c.is_composing());
}

#[test]
fn clearing_buffer_emits_hide() {
    if !has_data() {
        return;
    }
    let (c, rx) = session_quiet(config(true));
    letters(&c, "wq");
    let _ = drain(&rx);
    press(&c, VK_BACK);
    press(&c, VK_BACK);
    let evs = drain(&rx);
    assert_eq!(evs.last(), Some(&SessionEvent::Hide), "{evs:?}");
    assert!(!c.is_composing());
}

#[test]
fn association_frame_is_not_composing() {
    if !has_data() {
        return;
    }
    let mut cfg = config(true);
    cfg.input.association.kind = "word".into();
    cfg.input.association.mode = "one_shot".into();
    // 编码嵌在宿主里（默认 app_inline）时联想不给标识；要看到标识得让编码归候选窗
    cfg.ui.candidate.preedit_display = "candidate_top".into();
    let (c, rx) = session_quiet(cfg);
    c.prewarm_indexes();
    letters(&c, "q");
    press(&c, VK_SPACE);
    assert!(!c.debug_assoc_texts().is_empty(), "前提：上屏后进了联想态");
    let v = last_view(&drain(&rx));
    assert!(!v.preedit.is_empty(), "联想态 preedit 是联想标识: {v:?}");
    assert!(!v.composing, "联想标识不算打字中: {v:?}");
    assert_eq!(v.preedit_caret, 0);
    assert!(v.has_candidates);
    assert!(!c.is_composing());
}

#[test]
fn caret_at_start_is_still_composing() {
    if !has_data() {
        return;
    }
    let (c, rx) = session_quiet(config(true));
    letters(&c, "wq");
    let _ = drain(&rx);
    press(&c, VK_LEFT);
    press(&c, VK_LEFT);
    assert_eq!(c.debug_ui_caret_bytes(), 0, "前提：插入符已移到编码开头");
    let evs = drain(&rx);
    let v = last_view(&evs);
    assert_eq!((v.preedit.as_str(), v.preedit_caret), ("wq", 0), "{v:?}");
    assert!(v.composing, "插入符在开头仍是打字中: {v:?}");
    assert!(c.is_composing());
}

#[test]
fn temp_mode_prefix_is_composing() {
    if !has_data() {
        return;
    }
    let (c, rx) = session_quiet(config(true));
    press(&c, VK_BACKTICK); // wubi86 下反引号进临时拼音，缓冲为空、编码栏只有引导符
    let v = last_view(&drain(&rx));
    assert_eq!(v.preedit, "`", "{v:?}");
    assert!(v.composing, "引导符算打字中: {v:?}");
    assert!(c.is_composing());
}

#[test]
fn latin_input_true_in_english_schema() {
    if !has_data() {
        return;
    }
    let (c, rx) = session_quiet(config(true));
    menu(&c, MenuCmd::TogglePunct);
    assert!(
        !last_status(&drain(&rx)).latin_input,
        "前提：中文方案不是拉丁直出"
    );
    assert!(!c.latin_input());
    menu(&c, MenuCmd::SchemaEnglish);
    let s = last_status(&drain(&rx));
    assert!(s.latin_input, "{s:?}");
    assert!(c.latin_input());
}

#[test]
fn latin_input_true_when_chinese_mode_off() {
    if !has_data() {
        return;
    }
    let (c, rx) = session(config(false));
    let s = last_status(&drain(&rx));
    assert!(!s.chinese_mode);
    assert!(s.latin_input, "{s:?}");
    assert!(c.latin_input());
}

#[test]
fn latin_input_ignores_custom_label() {
    if !has_data() {
        return;
    }
    let mut cfg = config(false);
    cfg.ui.labels.english = "A".into();
    let (c, rx) = session(cfg);
    let s = last_status(&drain(&rx));
    assert_eq!(s.icon_label, "A", "前提：英文标签已被改成 A: {s:?}");
    assert!(s.latin_input, "判据不看 icon_label: {s:?}");
    assert!(c.latin_input());
}

#[test]
fn toast_and_status_tip_become_notice() {
    if !has_data() {
        return;
    }
    let (c, rx) = session_quiet(config(true));
    // 切方案会弹状态提示/toast，两者在中立通道里都是 Notice
    menu(&c, MenuCmd::SchemaEnglish);
    let evs = drain(&rx);
    assert!(
        evs.iter()
            .any(|e| matches!(e, SessionEvent::Notice(t) if !t.is_empty())),
        "应至少有一条 Notice: {evs:?}"
    );
    press(&c, VK_ESCAPE);
}
