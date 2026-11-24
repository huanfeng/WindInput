use super::*;

#[test]
fn unknown_type_decodes_as_unknown() {
    let m: Msg = serde_json::from_str(r#"{"t":"suggest","sid":3,"items":[]}"#).unwrap();
    assert_eq!(m, Msg::Unknown);
}

#[test]
fn unknown_fields_are_ignored() {
    let m: Msg =
        serde_json::from_str(r#"{"t":"mode","ts":5,"chinese":true,"full_width":false,"x":1}"#)
            .unwrap();
    assert!(matches!(
        m,
        Msg::Mode(ModeEvent {
            ts: 5,
            chinese: true,
            ..
        })
    ));
}

#[test]
fn presence_and_lifecycle_are_events() {
    let p = Msg::Presence(PresenceEvent {
        ts: 1,
        terminals: 0,
        focused: false,
    });
    let l = Msg::Lifecycle(LifecycleEvent {
        ts: 2,
        event: lifecycle::STOPPING.into(),
    });
    assert_eq!(p.event_kind(), Some("presence"));
    assert_eq!(l.event_kind(), Some("lifecycle"));
    let s = serde_json::to_string(&p).unwrap();
    assert_eq!(
        s,
        r#"{"t":"presence","ts":1,"terminals":0,"focused":false}"#
    );
    let back: Msg = serde_json::from_str(&s).unwrap();
    assert_eq!(back, p);
}

#[test]
fn key_event_omits_absent_detail() {
    let m = Msg::Key(KeyEvent {
        ts: 1,
        sid: 0,
        app: String::new(),
        key: None,
        mods: vec![],
        class: "space".into(),
        handled: false,
        observed: false,
    });
    let s = serde_json::to_string(&m).unwrap();
    assert!(s.starts_with(r#"{"t":"key""#), "{s}");
    assert!(!s.contains("\"key\":null") && !s.contains("mods"), "{s}");
}
