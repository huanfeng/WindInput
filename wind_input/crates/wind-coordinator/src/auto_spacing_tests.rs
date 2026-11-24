use super::*;
use wind_config::config::Config;

fn coord(cjk_latin: bool, cjk_digit: bool) -> std::sync::Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.input.spacing.cjk_latin = cjk_latin;
    cfg.input.spacing.cjk_digit = cjk_digit;
    Coordinator::new_headless(cfg, None)
}

fn insert(t: &str) -> KeyAction {
    KeyAction::InsertText {
        text: t.to_string(),
        new_composition: None,
        has_new_composition: false,
        mode_changed: false,
        chinese_mode: true,
    }
}

fn text_of(a: &KeyAction) -> String {
    match a {
        KeyAction::InsertText { text, .. } => text.clone(),
        KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text.clone(),
        other => panic!("unexpected {other:?}"),
    }
}

fn skin_commits(rx: &std::sync::mpsc::Receiver<wind_ui_types::UiCommand>) -> usize {
    rx.try_iter()
        .filter(|c| {
            matches!(
                c,
                wind_ui_types::UiCommand::SkinSignal(wind_ui_types::SkinSignal::Commit)
            )
        })
        .count()
}

#[test]
fn commit_with_text_signals_skin() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    let _ = skin_commits(&rx);
    let mut st = c.state.lock().unwrap();
    c.apply_auto_spacing(&mut st, insert("1"));
    drop(st);
    assert_eq!(skin_commits(&rx), 1);
}

#[test]
fn no_text_no_skin_signal() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    let _ = skin_commits(&rx);
    let mut st = c.state.lock().unwrap();
    c.apply_auto_spacing(&mut st, insert(""));
    c.apply_auto_spacing(&mut st, KeyAction::Consumed);
    c.apply_auto_spacing(&mut st, KeyAction::PassThrough);
    drop(st);
    assert_eq!(skin_commits(&rx), 0);
}

#[test]
fn session_channel_gets_no_skin_signal() {
    let (c, events) = Coordinator::new_session(
        Config::default(),
        None,
        None,
        crate::input_profile::InputProfile::desktop(),
    );
    let _: Vec<_> = events.try_iter().collect();
    let mut st = c.state.lock().unwrap();
    let act = c.apply_auto_spacing(&mut st, insert("1"));
    drop(st);
    assert_eq!(text_of(&act), "1", "上屏本身照常");
    assert_eq!(events.try_iter().count(), 0, "中立通道没有对应事件");
}

#[test]
fn prefixes_latin_after_cjk_and_cjk_after_latin() {
    let c = coord(true, false);
    let mut st = State {
        spacing_prev: CharClass::Cjk,
        ..Default::default()
    };
    let a = c.apply_auto_spacing(&mut st, insert("Python"));
    assert_eq!(text_of(&a), " Python");
    assert_eq!(st.spacing_prev, CharClass::Latin);
    let a = c.apply_auto_spacing(&mut st, insert("很好"));
    assert_eq!(text_of(&a), " 很好");
    let a = c.apply_auto_spacing(&mut st, insert("，"));
    assert_eq!(text_of(&a), "，", "标点永不前补");
    assert_eq!(st.spacing_prev, CharClass::Other);
}

#[test]
fn nothing_when_disabled_or_unknown_prev() {
    let c = coord(false, false);
    let mut st = State {
        spacing_prev: CharClass::Cjk,
        ..Default::default()
    };
    assert_eq!(
        text_of(&c.apply_auto_spacing(&mut st, insert("abc"))),
        "abc"
    );
    let c = coord(true, true);
    let mut st = State::default(); // spacing_prev = Other（不可知）
    assert_eq!(
        text_of(&c.apply_auto_spacing(&mut st, insert("abc"))),
        "abc"
    );
}

#[test]
fn digit_rule_is_separate() {
    let c = coord(true, false);
    let mut st = State {
        spacing_prev: CharClass::Digit,
        ..Default::default()
    };
    assert_eq!(text_of(&c.apply_auto_spacing(&mut st, insert("个"))), "个");
    let c = coord(false, true);
    st.spacing_prev = CharClass::Digit;
    assert_eq!(text_of(&c.apply_auto_spacing(&mut st, insert("个"))), " 个");
}

/// 英文词自己带了尾随空格（`commit_space`）：之后的汉字看到的前一字符是空格，不再补；
/// 连续的英文之间一概不管。
#[test]
fn trailing_word_space_and_consecutive_latin() {
    let c = coord(true, false);
    let mut st = State {
        spacing_prev: CharClass::Cjk,
        ..Default::default()
    };
    assert_eq!(
        text_of(&c.apply_auto_spacing(&mut st, insert("Hello "))),
        " Hello "
    );
    assert_eq!(text_of(&c.apply_auto_spacing(&mut st, insert("好"))), "好");
    st.spacing_prev = CharClass::Latin;
    assert_eq!(
        text_of(&c.apply_auto_spacing(&mut st, insert("world"))),
        "world"
    );
}

#[test]
fn top_commit_prefixes_committed_part_only() {
    let c = coord(true, false);
    let mut st = State {
        spacing_prev: CharClass::Latin,
        ..Default::default()
    };
    let a = c.apply_auto_spacing(
        &mut st,
        KeyAction::CommitThenDeferComposition {
            commit_text: "工".into(),
            deferred_composition: "a".into(),
            timeout_ms: 0,
        },
    );
    assert_eq!(text_of(&a), " 工");
    assert_eq!(st.spacing_prev, CharClass::Cjk);
}

#[test]
fn per_app_rule_turns_off_cjk_spacing() {
    let c = coord(true, true);
    c.active_compat
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .auto_spacing = Some(false);
    let mut st = State {
        spacing_prev: CharClass::Cjk,
        ..Default::default()
    };
    assert_eq!(
        text_of(&c.apply_auto_spacing(&mut st, insert("abc"))),
        "abc"
    );
    c.active_compat
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .auto_spacing = Some(true);
    st.spacing_prev = CharClass::Cjk;
    assert_eq!(
        text_of(&c.apply_auto_spacing(&mut st, insert("ghi"))),
        " ghi"
    );
}

#[test]
fn latch_prefers_document_whitespace_veto() {
    let c = coord(true, false);
    let mut st = State::default();
    let mut d = KeyEventData {
        key_code: 0x41,
        scan_code: 0,
        modifiers: 0,
        event_type: 0,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0x4F60,
    };
    c.latch_spacing_boundary(&mut st, &d);
    assert_eq!(st.spacing_prev, CharClass::Cjk);
    d.prev_char = ' ' as u16;
    c.latch_spacing_boundary(&mut st, &d);
    assert_eq!(st.spacing_prev, CharClass::Other, "文档里已有空格就不补");
    d.prev_char = 0;
    d.seq_char = 0;
    c.latch_spacing_boundary(&mut st, &d);
    assert_eq!(st.spacing_prev, CharClass::Other);
}
