use super::*;

#[test]
fn parses_marker_title_and_entries() {
    let doc = parse_phrase_text("wind:p1 我的直通车\nkx (＾▽＾)\nzw 早上好呀\n").unwrap();
    assert_eq!(doc.title, "我的直通车");
    assert_eq!(doc.entries.len(), 2);
    assert_eq!(doc.entries[0].code, "kx");
    assert_eq!(doc.entries[0].text, "(＾▽＾)");
    assert_eq!(doc.entries[1].line, 3, "行号是源行号（1-based）");
    assert!(doc.problems.is_empty());
}

#[test]
fn title_is_optional() {
    let doc = parse_phrase_text("wind:p1\nkx (＾▽＾)\n").unwrap();
    assert_eq!(doc.title, "");
    assert_eq!(doc.entries.len(), 1);
}

/// 这个格式存在的理由：命令短语里有引号、括号、逗号、花括号、反斜杠和空格，
/// 全部必须原样穿过。
#[test]
fn command_phrase_survives_verbatim() {
    let cmd = r#"$CC("知典超精 {sub(reverse(last(1)), 1, 1)}", proc.run("D:\\Program Files\\知典超精\\知典.exe", "--query", "{sub(reverse(last(1)), 1, 1)}"))"#;
    let doc = parse_phrase_text(&format!("wind:p1\nzd {cmd}\n")).unwrap();
    assert_eq!(doc.entries.len(), 1);
    assert_eq!(doc.entries[0].text, cmd, "正文必须逐字保留");
}

#[test]
fn text_may_contain_spaces() {
    let doc = parse_phrase_text("wind:p1\nem 我 的 邮箱 是 a@b.com\n").unwrap();
    assert_eq!(doc.entries[0].text, "我 的 邮箱 是 a@b.com");
}

#[test]
fn multiple_separator_spaces_are_collapsed_only_at_split() {
    let doc = parse_phrase_text("wind:p1\nkx    (＾▽＾)\n").unwrap();
    assert_eq!(doc.entries[0].code, "kx");
    assert_eq!(doc.entries[0].text, "(＾▽＾)");
}

#[test]
fn crlf_and_indentation_are_tolerated() {
    let doc = parse_phrase_text("wind:p1\r\n  kx (＾▽＾)  \r\n\r\nzw 早\r\n").unwrap();
    assert_eq!(doc.entries.len(), 2);
    assert_eq!(doc.entries[0].text, "(＾▽＾)");
}

#[test]
fn missing_marker_is_fatal() {
    assert!(parse_phrase_text("kx (＾▽＾)\n").is_err());
}

/// 被聊天软件压扁成一行时整段解析失败——失败方式是安全的：报错，而不是
/// 把一长串当成一条短语装进去。
#[test]
fn flattened_message_fails_loudly() {
    let doc = parse_phrase_text("wind:p1 标题 kx (＾▽＾) zw 早上好").unwrap();
    assert!(doc.entries.is_empty(), "全被当成标题，没有条目");
}

#[test]
fn future_version_is_rejected() {
    let err = parse_phrase_text("wind:p2\nkx x\n").unwrap_err();
    assert!(err.contains("请升级"), "{err}");
}

#[test]
fn marker_without_version_is_rejected() {
    assert!(parse_phrase_text("wind:p\nkx x\n").is_err());
}

/// `wind:p1x` 不能被当成版本 1——否则尾巴上的内容会被静默吞掉。
#[test]
fn marker_with_trailing_garbage_is_rejected() {
    assert!(parse_phrase_text("wind:p1x\nkx x\n").is_err());
}

#[test]
fn bad_lines_are_skipped_not_fatal() {
    let doc = parse_phrase_text("wind:p1\nkx (＾▽＾)\n没有空格的一行\nzw 早\n").unwrap();
    assert_eq!(doc.entries.len(), 2, "坏行不牵连其余条目");
    assert_eq!(doc.problems.len(), 1);
    assert_eq!(doc.problems[0].line, 3);
    assert_eq!(doc.problems[0].reason, ProblemReason::MissingSeparator);
}

#[test]
fn non_ascii_code_is_rejected() {
    let doc = parse_phrase_text("wind:p1\n编码 内容\n").unwrap();
    assert!(doc.entries.is_empty());
    assert_eq!(doc.problems[0].reason, ProblemReason::BadCode);
}

#[test]
fn duplicates_within_one_document_are_reported() {
    let doc = parse_phrase_text("wind:p1\nkx 好\nkx 好\n").unwrap();
    assert_eq!(doc.entries.len(), 1);
    assert_eq!(doc.problems[0].reason, ProblemReason::Duplicate);
}

/// 同 code 不同内容是合法的（短语主键是 `(code, text)`）。
#[test]
fn same_code_different_text_both_kept() {
    let doc = parse_phrase_text("wind:p1\nkx 甲\nkx 乙\n").unwrap();
    assert_eq!(doc.entries.len(), 2);
    assert!(doc.problems.is_empty());
}

#[test]
fn check_reports_syntax_errors_and_hints() {
    let bs = char::from_u32(92).unwrap();
    let q = char::from_u32(34).unwrap();
    let texts = vec![
        "(＾▽＾)".to_string(),
        format!("$CC({q}跑{q}, proc.run({q}D:{bs}{bs}notes{bs}{bs}a.exe{q}))"),
        format!("$CC({q}跑{q}, proc.run({q}D:{bs}notes{q}))"),
        format!("$CC({q}坏"),
    ];
    let c = check_entries(&texts);

    assert!(c[0].is_importable() && c[0].hints.is_empty(), "纯文本");
    assert!(
        c[1].is_importable() && c[1].hints.is_empty(),
        "命令短语本身不是问题"
    );
    assert!(c[2].is_importable(), "笔误不阻止导入");
    assert_eq!(c[2].hints.len(), 1, "单写反斜杠要提示");
    assert!(!c[3].is_importable(), "语法错误不可导入");
}

#[test]
fn detection_is_prefix_based() {
    assert!(is_phrase_text("wind:p1\nkx x\n"));
    assert!(is_phrase_text("\n\n  wind:p1 标题\nkx x\n"));
    assert!(!is_phrase_text("[package]\nkind = \"schema_text\"\n"));
    assert!(!is_phrase_text("wind_dict:\n  version: 1\n"));
    assert!(!is_phrase_text(""));
}
