use wind_sync::learn::{Gen, Key, LearnKind, norm_key};

fn g(ms: u64, dev: &str) -> Gen {
    Gen {
        ms,
        dev: dev.into(),
    }
}

#[test]
fn next_is_monotonic_with_slow_clock() {
    assert_eq!(Gen::next(&g(5000, "b"), 1000, "a").ms, 5001);
}

#[test]
fn next_uses_clock_when_ahead() {
    assert_eq!(Gen::next(&g(5000, "b"), 9000, "a"), g(9000, "a"));
}

#[test]
fn tie_breaks_by_device() {
    assert!(g(5, "b") > g(5, "a"));
    assert!(Gen::initial() < g(0, "a"));
    assert!(Gen::initial() < g(1, ""));
}

#[test]
fn render_parse_roundtrip() {
    assert_eq!(Gen::initial().render(), "0");
    assert_eq!(Gen::parse("0"), Some(Gen::initial()));
    let x = g(1764123461000, "pc");
    assert_eq!(x.render(), "1764123461000:pc");
    assert_eq!(Gen::parse(&x.render()), Some(x));
    assert_eq!(Gen::parse("abc"), None);
    assert_eq!(Gen::parse("x:pc"), None);
}

#[test]
fn key_ignores_code_spaces() {
    let k = |a: &str, b: &str| Key(a.into(), b.into());
    assert_eq!(
        norm_key(LearnKind::Freq, &k("ni hao", "你好")),
        norm_key(LearnKind::Freq, &k("nihao", "你好"))
    );
    assert_ne!(
        norm_key(LearnKind::Assoc, &k("你 好", "吗")),
        norm_key(LearnKind::Assoc, &k("你好", "吗"))
    );
}
