use super::*;
use crate::msg::{CommitEvent, KeyEvent};

fn key() -> Msg {
    Msg::Key(KeyEvent {
        ts: 1,
        sid: 0,
        app: String::new(),
        key: Some("a".into()),
        mods: vec!["shift".into()],
        class: "letter".into(),
        handled: true,
        observed: false,
    })
}

#[test]
fn names_round_trip() {
    for t in Tier::ALL {
        assert_eq!(Tier::parse(t.name()), Some(t));
    }
    assert_eq!(Tier::parse("bogus"), None);
}

#[test]
fn candidate_provide_cannot_be_granted() {
    let g = Grants::parse(["candidate.provide", "mode"]);
    assert!(!g.has(Tier::CandidateProvide));
    assert!(g.has(Tier::Mode));
}

#[test]
fn key_class_only_strips_key_and_mods() {
    let Some(Msg::Key(k)) = trim(&key(), Grants::NONE.with(Tier::KeyClass)) else {
        panic!("key.class 应收到按键");
    };
    assert_eq!(k.key, None);
    assert!(k.mods.is_empty());
    assert_eq!(k.class, "letter");
}

#[test]
fn key_detail_implies_key_class() {
    let want = Grants::parse(["key.detail"]);
    let granted = Grants::parse(["key.class"]);
    let eff = want.intersect(granted);
    assert!(eff.has(Tier::KeyClass) && !eff.has(Tier::KeyDetail));
}

#[test]
fn key_detail_keeps_everything() {
    assert_eq!(
        trim(&key(), Grants::NONE.with(Tier::KeyDetail)),
        Some(key())
    );
}

#[test]
fn ungranted_events_are_dropped() {
    let commit = Msg::Commit(CommitEvent {
        ts: 1,
        sid: 0,
        app: String::new(),
        text: "你".into(),
        source: "candidate".into(),
        rect: None,
    });
    let g = Grants::NONE.with(Tier::KeyDetail).with(Tier::Mode);
    assert_eq!(trim(&commit, g), None);
    assert_eq!(trim(&key(), Grants::NONE.with(Tier::Mode)), None);
}

#[test]
fn state_events_need_no_tier() {
    let p = Msg::Presence(crate::msg::PresenceEvent {
        ts: 1,
        terminals: 2,
        focused: true,
    });
    assert_eq!(trim(&p, Grants::NONE), Some(p));
}

#[test]
fn control_messages_pass_through() {
    assert_eq!(trim(&Msg::Ping, Grants::NONE), Some(Msg::Ping));
}
