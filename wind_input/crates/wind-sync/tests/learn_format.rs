use std::collections::BTreeMap;
use wind_sync::learn::format::*;
use wind_sync::learn::*;

fn g(s: &str) -> Gen {
    Gen::parse(s).unwrap()
}

fn row(gen_: &str, count: u64, f: &[&str]) -> Row {
    Row {
        gen_: g(gen_),
        count,
        fields: f.iter().map(|s| s.to_string()).collect(),
    }
}

fn contrib(
    kind: LearnKind,
    epoch: &str,
    rows: &[(&str, &str, Row)],
    tombs: &[(&str, &str, &str)],
) -> Contribution {
    let mut r = BTreeMap::new();
    for (a, b, row) in rows {
        let k = Key(a.to_string(), b.to_string());
        r.insert(norm_key(kind, &k), (k, row.clone()));
    }
    let mut t = BTreeMap::new();
    for (a, b, gs) in tombs {
        let k = Key(a.to_string(), b.to_string());
        t.insert(norm_key(kind, &k), (k, g(gs)));
    }
    Contribution {
        kind,
        epoch: g(epoch),
        rows: r,
        tombs: t,
    }
}

#[test]
fn encode_decode_roundtrip() {
    let c = contrib(
        LearnKind::Freq,
        "5:a",
        &[
            ("ni", "你", row("0", 3, &["100"])),
            ("hao", "好", row("7:dev1", 9, &["200"])),
        ],
        &[("zz", "坏", "8:dev2")],
    );
    let s = encode(&c);
    assert!(s.starts_with("#wind-learn 1 freq epoch=5:a\n"));
    assert_eq!(decode(LearnKind::Freq, &s).unwrap(), c);
    assert_eq!(encode(&c), s);
}

#[test]
fn overlay_takes_delta_state_not_sum() {
    let base = contrib(
        LearnKind::Freq,
        "0",
        &[("nh", "你好", row("0", 100, &["1"]))],
        &[],
    );
    let delta = contrib(
        LearnKind::Freq,
        "0",
        &[("nh", "你好", row("1:a", 105, &["2"]))],
        &[],
    );
    let o = overlay(&base, &delta);
    let k = norm_key(LearnKind::Freq, &Key("nh".into(), "你好".into()));
    assert_eq!(o.rows[&k].1.count, 105);
}

#[test]
fn overlay_tomb_removes_base_row() {
    let base = contrib(
        LearnKind::Freq,
        "0",
        &[("nh", "你好", row("1:a", 100, &["1"]))],
        &[],
    );
    let delta = contrib(LearnKind::Freq, "0", &[], &[("nh", "你好", "2:a")]);
    let o = overlay(&base, &delta);
    assert!(o.rows.is_empty());
    assert_eq!(o.tombs.len(), 1);
}

#[test]
fn overlay_older_tomb_keeps_base_row() {
    let base = contrib(
        LearnKind::Freq,
        "0",
        &[("nh", "你好", row("3:a", 100, &["1"]))],
        &[],
    );
    let delta = contrib(LearnKind::Freq, "0", &[], &[("nh", "你好", "2:a")]);
    let o = overlay(&base, &delta);
    assert_eq!(o.rows.len(), 1);
    assert_eq!(o.tombs.len(), 1);
}

#[test]
fn overlay_epoch_takes_larger() {
    let base = contrib(LearnKind::Freq, "9:a", &[], &[]);
    let delta = contrib(LearnKind::Freq, "3:a", &[], &[]);
    assert_eq!(overlay(&base, &delta).epoch, g("9:a"));
    assert_eq!(overlay(&delta, &base).epoch, g("9:a"));
}

#[test]
fn split_then_overlay_is_identity() {
    let last = contrib(
        LearnKind::Freq,
        "0",
        &[
            ("a", "甲", row("0", 1, &["1"])),
            ("b", "乙", row("2:a", 2, &["1"])),
            ("c", "丙", row("0", 3, &["1"])),
        ],
        &[("t", "旧", "2:a")],
    );
    // 甲不变；乙改数；丙被删（有标记）；丁新增；新增标记；旧标记保留
    let full = contrib(
        LearnKind::Freq,
        "0",
        &[
            ("a", "甲", row("0", 1, &["1"])),
            ("b", "乙", row("5:b", 7, &["9"])),
            ("d", "丁", row("6:b", 1, &["1"])),
        ],
        &[("t", "旧", "2:a"), ("c", "丙", "4:b")],
    );
    let d = split(&full, &last);
    assert_eq!(d.rows.len(), 2);
    assert_eq!(d.tombs.len(), 1);
    assert_eq!(overlay(&last, &d), full);

    // 无变化 → 空增量
    let d0 = split(&last, &last);
    assert!(d0.rows.is_empty() && d0.tombs.is_empty());
    assert_eq!(overlay(&last, &d0), last);
}

#[test]
fn compact_thresholds() {
    assert!(should_compact(100, 26, 0, 0));
    assert!(!should_compact(100, 25, 0, 0));
    assert!(should_compact(100, 1, 262145, 0));
    assert!(should_compact(100, 1, 0, 604801));
    assert!(!should_compact(100, 1, 262144, 604800));
}

#[test]
fn newer_version_rejected() {
    assert_eq!(
        decode(LearnKind::Freq, "#wind-learn 2 freq epoch=0\n"),
        Err(FormatError::Version)
    );
}

#[test]
fn malformed_rejected() {
    assert!(matches!(
        decode(LearnKind::Freq, "garbage\n"),
        Err(FormatError::Parse(_))
    ));
    assert!(matches!(
        decode(LearnKind::Freq, "#wind-learn 1 freq epoch=0\n+\ta\tb\t0\n"),
        Err(FormatError::Parse(_))
    ));
    assert!(matches!(
        decode(LearnKind::Freq, "#wind-learn 1 temp epoch=0\n"),
        Err(FormatError::Parse(_))
    ));
}

#[test]
fn deflate_roundtrip() {
    let data = "你好 hello ".repeat(200).into_bytes();
    let z = deflate(&data);
    assert!(z.len() < data.len());
    assert_eq!(inflate(&z).unwrap(), data);
    assert_eq!(inflate(b"not deflate data"), Err(FormatError::Inflate));
}

#[test]
fn epoch_raise_overlay_drops_old_rows() {
    let base = contrib(
        LearnKind::Freq,
        "0",
        &[
            ("a", "甲", row("0", 1, &["1"])),
            ("b", "乙", row("9:a", 2, &["1"])),
        ],
        &[("t", "旧", "3:a")],
    );
    let delta = contrib(LearnKind::Freq, "5:a", &[], &[]);
    let o = overlay(&base, &delta);
    assert_eq!(o.epoch, g("5:a"));
    assert_eq!(o.rows.len(), 1);
    assert!(o.tombs.is_empty());
}

#[test]
fn needs_rebase_on_reclaimed_tomb() {
    let last = contrib(LearnKind::Freq, "0", &[], &[("t", "旧", "3:a")]);
    let full = contrib(LearnKind::Freq, "0", &[], &[]);
    assert!(needs_rebase(&full, &last));
    // 行消失且无标记、无纪元提升
    let last2 = contrib(
        LearnKind::Freq,
        "0",
        &[("a", "甲", row("1:a", 1, &["1"]))],
        &[],
    );
    assert!(needs_rebase(&full, &last2));
}

#[test]
fn needs_rebase_false_for_normal_changes() {
    let last = contrib(
        LearnKind::Freq,
        "0",
        &[
            ("a", "甲", row("0", 1, &["1"])),
            ("c", "丙", row("0", 3, &["1"])),
        ],
        &[("t", "旧", "2:a")],
    );
    let full = contrib(
        LearnKind::Freq,
        "0",
        &[
            ("a", "甲", row("5:b", 2, &["1"])),
            ("d", "丁", row("6:b", 1, &["1"])),
        ],
        &[("t", "旧", "2:a"), ("c", "丙", "4:b")],
    );
    assert!(!needs_rebase(&full, &last));
}

#[test]
fn split_then_overlay_identity_with_epoch_raise() {
    let last = contrib(
        LearnKind::Freq,
        "0",
        &[
            ("a", "甲", row("0", 1, &["1"])),
            ("b", "乙", row("2:a", 2, &["1"])),
        ],
        &[],
    );
    // 纪元提升到 5:a：甲、乙随之失效；丙是新行
    let full = contrib(
        LearnKind::Freq,
        "5:a",
        &[("c", "丙", row("6:b", 1, &["1"]))],
        &[],
    );
    assert!(!needs_rebase(&full, &last));
    let d = split(&full, &last);
    assert_eq!(overlay(&last, &d), full);
}
