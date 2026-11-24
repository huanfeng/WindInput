use super::*;

fn update() -> KeyAction {
    KeyAction::UpdateComposition {
        text: "n".into(),
        caret_pos: 1,
    }
}

fn insert(text: &str) -> KeyAction {
    KeyAction::InsertText {
        text: text.into(),
        new_composition: None,
        mode_changed: false,
        chinese_mode: true,
        has_new_composition: false,
    }
}

#[test]
fn session_spans_composition_and_ends_on_commit() {
    let mut s = HookSession::default();
    assert_eq!(s.advance(&KeyAction::PassThrough), 0);
    let sid = s.advance(&update());
    assert_ne!(sid, 0);
    assert_eq!(s.advance(&update()), sid, "同一次组合共用 sid");
    assert_eq!(s.advance(&insert("你")), sid, "上屏事件带刚结束的会话");
    assert_eq!(s.advance(&KeyAction::PassThrough), 0);
    assert_ne!(s.advance(&update()), sid, "下一次组合是新会话");
}

#[test]
fn commit_with_follow_up_composition_starts_new_session() {
    let mut s = HookSession::default();
    let first = s.advance(&update());
    let act = KeyAction::CommitThenDeferComposition {
        commit_text: "我".into(),
        deferred_composition: "q".into(),
        timeout_ms: 0,
    };
    assert_eq!(s.advance(&act), first);
    let second = s.advance(&update());
    assert_ne!(second, 0);
    assert_ne!(second, first);
}

// ── 经协调器出口的行为 ──

use std::sync::mpsc::Receiver;
use wind_bridge::handler::MessageHandler;
use wind_hook::tier::Grants;
use wind_keys::keymap::{VK_A, VK_COMMA};

fn coord(tag: &str) -> Arc<Coordinator> {
    let path = std::env::temp_dir().join(format!("wind_hook_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(wind_store::Store::open(&path).unwrap());
    Coordinator::new_headless_with_store(wind_config::Config::default(), None, store)
}

fn press(c: &Coordinator, vk: u32) -> KeyAction {
    c.handle_key_event_policed(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    })
}

fn connect(c: &Coordinator, tiers: &[&str]) -> Receiver<Msg> {
    c.hook_tap()
        .connect(Grants::parse(tiers.iter().copied()), 64)
}

fn drain(rx: &Receiver<Msg>) -> Vec<Msg> {
    rx.try_iter().collect()
}

#[test]
fn key_and_commit_events_carry_detail_and_text() {
    let c = coord("detail");
    let rx = connect(&c, &["key.detail", "commit"]);
    press(&c, VK_COMMA);
    press(&c, VK_A);
    let got = drain(&rx);
    let keys: Vec<&KeyEvent> = got
        .iter()
        .filter_map(|m| if let Msg::Key(k) = m { Some(k) } else { None })
        .collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].key.as_deref(), Some("comma"));
    assert_eq!(keys[0].class, "punct");
    assert_eq!(keys[1].key.as_deref(), Some("a"));
    assert_eq!(keys[1].class, "letter");
    assert!(
        got.iter()
            .any(|m| matches!(m, Msg::Commit(e) if e.text == "，" && e.sid == 0)),
        "空闲时逗号直接上屏中文标点，不属于任何组合: {got:?}"
    );
    assert_ne!(keys[1].sid, 0, "字母开启组合会话");
}

#[test]
fn presence_is_emitted_on_change_only() {
    let c = coord("presence");
    let rx = connect(&c, &["mode"]);
    c.hook_presence();
    c.hook_presence();
    let got = drain(&rx);
    assert!(
        matches!(got.as_slice(), [Msg::Presence(p)] if p.terminals == 0 && !p.focused),
        "无终端、无焦点；重复轮询不重发: {got:?}"
    );
    {
        let mut s = c.state.lock().unwrap();
        s.ime_active = true;
        s.has_edit_context = true;
    }
    c.hook_presence();
    let got = drain(&rx);
    assert!(
        matches!(got.as_slice(), [Msg::Presence(p)] if p.focused),
        "{got:?}"
    );
    c.state.lock().unwrap().has_edit_context = false;
    c.hook_presence();
    assert!(matches!(
        drain(&rx).as_slice(),
        [Msg::Presence(p)] if !p.focused
    ));
}

#[test]
fn hook_changed_replays_presence() {
    let c = coord("presence_replay");
    let rx = connect(&c, &["mode"]);
    c.hook_presence();
    drain(&rx);
    c.hook_changed();
    c.hook_presence();
    let n = drain(&rx)
        .iter()
        .filter(|m| matches!(m, Msg::Presence(_)))
        .count();
    assert_eq!(n, 1, "宿主重新就绪后应再发一次当前状态");
}

#[test]
fn config_reload_emits_lifecycle_only_when_extensions_run() {
    let c = coord("lifecycle");
    c.notify_extensions_config();
    let rx = connect(&c, &["mode"]);
    c.notify_extensions_config();
    let got = drain(&rx);
    assert!(
        matches!(got.as_slice(), [Msg::Lifecycle(l)] if l.event == "config_reloaded"),
        "{got:?}"
    );
}

#[test]
fn in_memory_config_refresh_emits_config_reloaded() {
    let c = coord("refresh_lifecycle");
    let rx = connect(&c, &["mode"]);
    c.refresh_config_in_memory(|cfg| cfg.input.s2t.enabled = true);
    let got = drain(&rx);
    assert!(
        got.iter()
            .any(|m| matches!(m, Msg::Lifecycle(l) if l.event == "config_reloaded")),
        "托盘 / 菜单改配置也要通知扩展: {got:?}"
    );
}

#[test]
fn key_class_grant_hides_key_name() {
    let c = coord("class");
    let rx = connect(&c, &["key.class"]);
    press(&c, VK_A);
    let got = drain(&rx);
    assert!(
        matches!(got.as_slice(), [Msg::Key(k)] if k.key.is_none() && k.class == "letter"),
        "{got:?}"
    );
}

#[test]
fn password_field_hides_key_name_and_commit() {
    let c = coord("password");
    let rx = connect(&c, &["key.detail", "commit"]);
    c.apply_input_diag(1, false, 2, 1 << 31);
    press(&c, VK_A);
    press(&c, VK_COMMA);
    c.hook_commit("secret", "command");
    let got = drain(&rx);
    assert!(!got.is_empty());
    for m in &got {
        match m {
            Msg::Key(k) => assert!(k.key.is_none() && k.mods.is_empty(), "{k:?}"),
            other => panic!("密码框里不该有 {other:?}"),
        }
    }
}

#[test]
fn non_key_origins_reach_extensions_through_push_exit() {
    use crate::session_exit::ActionOrigin;
    let c = coord("origins");
    let rx = connect(&c, &["commit"]);
    c.push_commit_text_from(ActionOrigin::SoftKeyboardClick, "软");
    c.push_commit_text_from(ActionOrigin::Command, "命");
    let got: Vec<(String, String)> = drain(&rx)
        .into_iter()
        .filter_map(|m| match m {
            Msg::Commit(e) => Some((e.text, e.source)),
            _ => None,
        })
        .collect();
    let want =
        [("软", "softkeyboard"), ("命", "command")].map(|(t, s)| (t.to_string(), s.to_string()));
    assert_eq!(got, want);
}

#[test]
fn idle_tap_produces_nothing() {
    let c = coord("idle");
    press(&c, VK_A);
    let rx = connect(&c, &["mode"]);
    assert!(drain(&rx).is_empty());
}

#[test]
fn observed_key_is_marked_and_unhandled() {
    let c = coord("observed");
    let rx = connect(&c, &["key.detail"]);
    c.hook_observed_key(VK_A, wind_ipc::protocol::MOD_SHIFT);
    let got = drain(&rx);
    assert!(
        matches!(got.as_slice(), [Msg::Key(k)]
            if k.observed && !k.handled && k.key.as_deref() == Some("a") && k.mods == ["shift"]),
        "{got:?}"
    );
}

#[test]
fn observed_key_in_password_field_hides_name() {
    let c = coord("observed_pw");
    let rx = connect(&c, &["key.detail"]);
    c.apply_input_diag(1, false, 2, 1 << 31);
    c.hook_observed_key(VK_A, 0);
    let got = drain(&rx);
    assert!(
        matches!(got.as_slice(), [Msg::Key(k)] if k.key.is_none() && k.observed),
        "{got:?}"
    );
}

#[test]
fn observed_key_without_key_grant_is_dropped() {
    let c = coord("observed_none");
    let rx = connect(&c, &["commit"]);
    c.hook_observed_key(VK_A, 0);
    assert!(drain(&rx).is_empty());
}

#[test]
fn mode_event_is_deduplicated() {
    let c = coord("mode");
    let rx = connect(&c, &["mode"]);
    c.build_status();
    c.build_status();
    let got = drain(&rx);
    assert!(matches!(got.as_slice(), [Msg::Mode(_)]), "{got:?}");
}

#[test]
fn modifiers_are_named() {
    assert_eq!(mod_names(MOD_SHIFT | MOD_CTRL), ["shift", "ctrl"]);
    assert!(mod_names(0).is_empty());
}
