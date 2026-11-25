use super::*;

#[test]
fn name_to_id() {
    assert_eq!(
        id_from_name("DESKTOP-7F3K2L9").as_deref(),
        Some("desktop-7f3k2l9")
    );
    assert_eq!(id_from_name("My  Laptop!!").as_deref(), Some("my-laptop"));
    assert_eq!(id_from_name("家里电脑"), None);
    assert_eq!(id_from_name("---"), None);
    assert_eq!(id_from_name(&"a".repeat(30)).unwrap().len(), 16);
}

#[test]
fn truncated_id_has_no_trailing_dash() {
    let id = id_from_name("aaaaaaaaaaaaaaa-bbbb").unwrap();
    assert_eq!(id, "aaaaaaaaaaaaaaa");
    assert!(valid_id(&id));
}

#[test]
fn default_id() {
    let id = default_device_id("家里电脑", false);
    assert_eq!(id.len(), 4);
    assert!(id.chars().all(|c| ALPHABET.contains(c)));
    assert_eq!(default_device_id("Pixel 8", true), "pixel-8-portable");
}

#[test]
fn validity() {
    assert!(valid_id("a"));
    assert!(valid_id("home-pc"));
    for bad in ["", "-a", "a-", "A", "a_b"] {
        assert!(!valid_id(bad), "{bad}");
    }
    assert!(!valid_id(&"a".repeat(25)));
}

#[test]
fn suffix() {
    assert_eq!(with_suffix("home-pc", 2), "home-pc-2");
    let s = with_suffix(&"a".repeat(24), 12);
    assert!(s.len() <= MAX_ID_LEN && s.ends_with("-12") && valid_id(&s));
    let s = with_suffix(&format!("{}-b", "a".repeat(21)), 12);
    assert!(s.len() <= MAX_ID_LEN && valid_id(&s));
}

#[test]
fn random_ids_valid() {
    for _ in 0..1000 {
        assert!(valid_id(&random_id()));
    }
}

#[test]
fn leading_junk_does_not_eat_name_budget() {
    let id = id_from_name(&format!("!!{}", "a".repeat(20))).unwrap();
    assert_eq!(id.len(), 16);
}

#[test]
fn long_portable_id_stays_valid() {
    let id = default_device_id("abcdefghijklmnop", true);
    assert!(valid_id(&id) && id.ends_with("-portable"));
}
