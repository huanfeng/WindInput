use super::*;

fn key(key_code: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        modifiers,
        scan_code: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

#[test]
fn add_word_ctrl_enter_consumes_key_after_closing_panel() {
    let c = Coordinator::new_headless(wind_config::Config::default(), None);
    c.state.lock().unwrap().add_word_active = true;
    let action = c.handle_key_event(&key(keymap::VK_RETURN, wind_ipc::protocol::MOD_CTRL));
    assert!(matches!(action, KeyAction::ClearComposition));
    assert!(!c.state.lock().unwrap().add_word_active);
    assert!(crate::edit_ops::to_outcome(action).consumed);
}

#[test]
fn add_word_other_ctrl_combos_remain_consumed() {
    let c = Coordinator::new_headless(wind_config::Config::default(), None);
    c.state.lock().unwrap().add_word_active = true;
    let action = c.handle_key_event(&key(
        keymap::key_name_to_vk_with_letters("s").unwrap(),
        wind_ipc::protocol::MOD_CTRL,
    ));
    assert!(matches!(action, KeyAction::Consumed));
    assert!(c.state.lock().unwrap().add_word_active);
}

#[test]
fn host_shortcuts_explicitly_release_key_after_cleanup() {
    for vk in [
        keymap::key_name_to_vk_with_letters("c").unwrap(),
        keymap::key_name_to_vk_with_letters("s").unwrap(),
    ] {
        let c = Coordinator::new_headless(wind_config::Config::default(), None);
        c.state.lock().unwrap().committed_text = "你".into();
        let action = c.handle_key_event(&key(vk, wind_ipc::protocol::MOD_CTRL));
        assert!(
            matches!(action, KeyAction::ClearCompositionThenPassThrough),
            "{action:?}"
        );
        assert!(!crate::edit_ops::to_outcome(action).consumed);
        assert!(c.state.lock().unwrap().committed_text.is_empty());
    }
}

#[test]
fn overlay_host_shortcut_explicitly_releases_key() {
    let c = Coordinator::new_headless(wind_config::Config::default(), None);
    let mut state = c.state.lock().unwrap();
    state.committed_text = "你".into();
    let action = c
        .overlay_ctrl_alt_guard(
            &mut state,
            &key(
                keymap::key_name_to_vk_with_letters("c").unwrap(),
                wind_ipc::protocol::MOD_CTRL,
            ),
            true,
            |_, st| st.committed_text.clear(),
        )
        .unwrap();
    assert!(matches!(action, KeyAction::ClearCompositionThenPassThrough));
    assert!(!crate::edit_ops::to_outcome(action).consumed);
    assert!(state.committed_text.is_empty());
}
