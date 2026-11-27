use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::PathBuf;

use wind_sync::learn::state::{GroupState, LearnState, Pending, Resolved};

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "wind_sync_learn_state_{}_{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn gs(tag: &str) -> GroupState {
    GroupState {
        contrib: format!("c-{tag}"),
        base: format!("b-{tag}"),
        last_base: format!("l-{tag}"),
        base_at: 7,
        peer_seen: seen(&[("d1", if tag.starts_with("new") { 5 } else { 1 })]),
    }
}

fn seen(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

/// 已提交 old 状态、待定 new 状态的两组（a、b）。
fn with_pending() -> LearnState {
    let mut s = LearnState::default();
    s.groups.insert("a".into(), gs("old-a"));
    s.groups.insert("b".into(), gs("old-b"));
    s.pending = Some(Pending {
        token: "T".into(),
        groups: BTreeMap::from([("a".into(), gs("new-a")), ("b".into(), gs("new-b"))]),
    });
    s
}

fn token_all(name: &str) -> Option<String> {
    let _ = name;
    Some("T".into())
}

#[test]
fn missing_file_is_default() {
    let d = tmp_dir("missing");
    let s = LearnState::load(&d.join("state.json")).unwrap();
    assert_eq!(s.version, 1);
    assert!(s.groups.is_empty() && s.pending.is_none());
}

#[test]
fn save_load_roundtrip() {
    let d = tmp_dir("roundtrip");
    let p = d.join("state.json");
    let mut s = with_pending();
    s.library = "lib".into();
    s.last_ok = 9;
    s.index_gen = 3;
    s.save(&p).unwrap();
    let back = LearnState::load(&p).unwrap();
    assert_eq!(back.library, "lib");
    assert_eq!(back.groups["a"].contrib, "c-old-a");
    assert_eq!(back.groups["b"].base_at, 7);
    assert_eq!((back.last_ok, back.index_gen), (9, 3));
    assert_eq!(back.groups["a"].peer_seen, seen(&[("d1", 1)]));
    assert_eq!(
        back.pending.unwrap().groups["a"].peer_seen,
        seen(&[("d1", 5)])
    );
    assert!(!d.join(".state.json.tmp").exists());
}

#[test]
fn pending_adopted_when_token_matches() {
    let mut s = with_pending();
    let r = s.resolve_pending(token_all);
    assert_eq!(r.adopted, vec!["a", "b"]);
    assert!(r.dropped.is_empty());
    assert_eq!(s.groups["a"].contrib, "c-new-a");
    assert_eq!(s.groups["b"].contrib, "c-new-b");
    assert_eq!(s.groups["a"].peer_seen, seen(&[("d1", 5)]));
    assert!(s.pending.is_none());
}

#[test]
fn pending_dropped_when_token_differs() {
    let mut s = with_pending();
    let r = s.resolve_pending(|g| (g == "a").then(|| "T".to_string()));
    assert_eq!(r.adopted, vec!["a"]);
    assert_eq!(r.dropped, vec!["b"]);
    assert_eq!(s.groups["a"].contrib, "c-new-a");
    assert_eq!(s.groups["a"].peer_seen, seen(&[("d1", 5)]));
    assert_eq!(s.groups["b"].contrib, "c-old-b");
    assert_eq!(s.groups["b"].peer_seen, seen(&[("d1", 1)]));
    assert!(s.pending.is_none());
}

#[test]
fn missing_token_is_dropped() {
    let mut s = with_pending();
    let r = s.resolve_pending(|_| None);
    assert!(r.adopted.is_empty());
    assert_eq!(r.dropped, vec!["a", "b"]);
    assert_eq!(s.groups["a"].contrib, "c-old-a");
}

#[test]
fn no_pending_is_noop() {
    let mut s = LearnState::default();
    assert_eq!(s.resolve_pending(token_all), Resolved::default());
}

#[test]
fn unknown_version_is_error() {
    let d = tmp_dir("version");
    let p = d.join("state.json");
    std::fs::write(&p, r#"{"version":2}"#).unwrap();
    let e = LearnState::load(&p).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::InvalidData);
}
