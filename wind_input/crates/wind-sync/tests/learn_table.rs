use wind_sync::learn::LearnKind;
use wind_sync::learn::table::{parse, render, token_of};

const FREQ: &str = "# 清风输入法 · 词频\n---\ncolumns: [code, text, count, last_used]\nwind:\n  kind: freq\n  code_spacing: syllable\n  escape: true\n...\n# no comment\n#h\ta\\tb\t3\t100\nni hao\t你好\t5\t200\n";

#[test]
fn parse_missing_or_header_only() {
    assert!(parse(LearnKind::Freq, None).unwrap().rows.is_empty());
    assert!(parse(LearnKind::Freq, Some(b"")).unwrap().rows.is_empty());
    let head_only = FREQ.lines().take(9).collect::<Vec<_>>().join("\n") + "\n";
    assert!(
        parse(LearnKind::Freq, Some(head_only.as_bytes()))
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn raw_fields_roundtrip() {
    // 键序：("h","a\\tb") 的 code 去空白后 "#h" < "nihao"
    let f = parse(LearnKind::Freq, Some(FREQ.as_bytes())).unwrap();
    assert_eq!(f.rows.len(), 2);
    assert_eq!(f.bad_lines, 0);
    assert_eq!(render(LearnKind::Freq, &f, None, false), FREQ.as_bytes());
}

#[test]
fn token_written_and_read() {
    let f = parse(LearnKind::Freq, Some(FREQ.as_bytes())).unwrap();
    let b1 = render(LearnKind::Freq, &f, Some("t1"), false);
    assert_eq!(token_of(&b1).as_deref(), Some("t1"));
    let f1 = parse(LearnKind::Freq, Some(&b1)).unwrap();
    assert_eq!(f1.rows, f.rows);
    let b2 = render(LearnKind::Freq, &f1, Some("t2"), false);
    assert_eq!(token_of(&b2).as_deref(), Some("t2"));
    assert_eq!(String::from_utf8(b2).unwrap().matches("sync:").count(), 1);
}

#[test]
fn flow_mapping_token() {
    let src = "---\ncolumns: [kind, text, count, last_used]\nwind: {kind: completion, escape: true}\n...\n# no comment\nx\ty\t1\t2\n";
    let f = parse(LearnKind::Completion, Some(src.as_bytes())).unwrap();
    let b = render(LearnKind::Completion, &f, Some("a"), false);
    assert!(String::from_utf8_lossy(&b).contains("escape: true, sync: a}"));
    assert_eq!(token_of(&b).as_deref(), Some("a"));
    let f = parse(LearnKind::Completion, Some(&b)).unwrap();
    let b = render(LearnKind::Completion, &f, Some("b"), false);
    assert_eq!(token_of(&b).as_deref(), Some("b"));
    assert_eq!(String::from_utf8(b).unwrap().matches("sync:").count(), 1);
}

#[test]
fn columns_order_respected() {
    let src = "---\ncolumns: [text, code, last_used, count]\nwind:\n  kind: freq\n  escape: true\n...\n# no comment\n你\tni\t9\t7\n";
    let f = parse(LearnKind::Freq, Some(src.as_bytes())).unwrap();
    let (k, c, fields) = f.rows.values().next().unwrap();
    assert_eq!((k.0.as_str(), k.1.as_str(), *c), ("ni", "你", 7));
    assert_eq!(fields, &vec!["9".to_string()]);
    assert_eq!(render(LearnKind::Freq, &f, None, false), src.as_bytes());
}

#[test]
fn bad_lines_and_duplicates() {
    let src = "---\ncolumns: [code, text, count, last_used]\nwind:\n  escape: true\n...\n# no comment\na\tb\n# note\nc\td\tx\t1\ne\tf\t2\t3\ne\tf\t9\t9\n";
    let f = parse(LearnKind::Freq, Some(src.as_bytes())).unwrap();
    assert_eq!(f.bad_lines, 2);
    assert_eq!(f.rows.len(), 1);
    assert_eq!(f.rows.values().next().unwrap().1, 2);
}

#[test]
fn new_file_head_per_kind() {
    let f = parse(LearnKind::Freq, None).unwrap();
    let b = String::from_utf8(render(LearnKind::Freq, &f, Some("t"), true)).unwrap();
    assert_eq!(
        b,
        "---\ncolumns: [code, text, count, last_used]\nwind:\n  kind: freq\n  code_spacing: syllable\n  escape: true\n  sync: t\n...\n# no comment\n"
    );
    let f = parse(LearnKind::Completion, None).unwrap();
    let b = String::from_utf8(render(LearnKind::Completion, &f, Some("t"), false)).unwrap();
    assert!(b.contains("wind: {kind: completion, escape: true, sync: t}\n"));
    let f = parse(LearnKind::Temp, None).unwrap();
    let b = String::from_utf8(render(LearnKind::Temp, &f, None, false)).unwrap();
    assert!(b.contains("columns: [code, text, weight, count, created_at]"));
    assert_eq!(token_of(b.as_bytes()), None);
}
