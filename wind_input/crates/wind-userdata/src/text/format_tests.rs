use super::*;
use std::path::Path;

fn p(s: &str, lower: bool) -> TextDict {
    parse(s.as_bytes(), Path::new("user.dict.yaml"), lower).unwrap()
}

const HEAD: &str = "---\nname: pinyin.user\ncolumns: [text, code, weight]\nwind:\n  kind: words\n  code_spacing: syllable\n  escape: true\n...\n";

#[test]
fn parses_header_entries_and_boundaries() {
    let d = p(
        &format!("{HEAD}你好\tni hao\t100\n先\txian\t5\n# 注释\n\n西安\txi an\t\n"),
        false,
    );
    assert_eq!(d.header.kind.as_deref(), Some("words"));
    assert!(d.header.syllable_codes && d.header.escape);
    let e: Vec<(&str, &str, u64, i32)> = d
        .entries
        .iter()
        .map(|e| (e.code.as_str(), e.text.as_str(), e.boundary, e.weight))
        .collect();
    assert_eq!(
        e,
        vec![
            ("nihao", "你好", 0b101, 100),
            ("xian", "先", 0b1, 5),
            ("xian", "西安", 0b101, 0)
        ],
        "单个汉字 = 单音节；空权重 = 0"
    );
    assert_eq!(d.entries[0].line, 8, "行号含头部行");
}

/// 文件头前的说明行不参与解析，行号照常含它。
#[test]
fn banner_line_before_header_is_ignored() {
    let d = p(
        &format!(
            "{}\n{HEAD}你好\tni hao\t100\n",
            crate::text::banner::USER_DICT
        ),
        false,
    );
    assert_eq!(d.header.kind.as_deref(), Some("words"));
    assert!(d.header.syllable_codes && d.header.escape);
    assert_eq!(d.entries.len(), 1);
    assert_eq!((d.entries[0].text.as_str(), d.entries[0].line), ("你好", 9));
    assert_eq!(d.bad_lines, 0);
}

#[test]
fn bad_lines_are_skipped_and_counted() {
    let d = p(
        &format!("{HEAD}你好\tni hao\t100\n坏\n缺码\t\t3\n权重坏\tquan\tabc\n好\thao\t1\n"),
        false,
    );
    let texts: Vec<&str> = d.entries.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, vec!["你好", "好"]);
    assert_eq!(d.bad_lines, 3, "列不够、码空、权重不是数字各算一行");
}

/// 单字词的单音节边界：空格载体表达不了 0b1，但一个汉字只对应一个音节——拼音文件里码无空格、
/// 文本恰为一个汉字 ⇒ 0b1。非汉字的单字符（emoji 等）码可能是多音节，仍为 0。
#[test]
fn single_han_char_without_spaces_is_one_syllable() {
    let d = p(
        &format!("{HEAD}先\txian\t5\n😊\txiaolian\t1\n西安\txian\t1\n"),
        false,
    );
    let b: Vec<u64> = d.entries.iter().map(|e| e.boundary).collect();
    assert_eq!(b, vec![0b1, 0, 0]);
    let wb = p("---\nname: wb.user\n...\n工\taaaa\t3\n", false);
    assert_eq!(wb.entries[0].boundary, 0, "非拼音文件不推断");
}

#[test]
fn bom_crlf_escape_and_no_comment() {
    let raw = format!(
        "\u{feff}{}C:\\\\note\tcn\t1\r\n# no comment\r\n#井\tjing\t2\r\n",
        HEAD.replace('\n', "\r\n")
    );
    let d = p(&raw, false);
    assert!(d.bom && d.crlf);
    assert_eq!(d.entries[0].text, "C:\\note", "escape: true 时反转义");
    assert_eq!(d.entries[1].text, "#井", "no comment 之后 # 行是数据");
}

#[test]
fn codetable_file_without_syllable_flag_has_no_boundaries() {
    let d = p("---\nname: wubi86.user\n...\n工人\taaww\t3\n", false);
    assert_eq!(d.entries[0].boundary, 0);
    assert!(!d.header.syllable_codes && !d.header.escape);
}

#[test]
fn normalize_key_flattens_spaces_and_lowercases() {
    assert_eq!(
        normalize_key("ni hao", "你好", false),
        ("nihao".into(), 0b101, "你好".into())
    );
    assert_eq!(normalize_key("Hello", "Hello", true).0, "hello");
    assert_eq!(normalize_key("Hello", "Hello", false).0, "Hello");
}

#[test]
fn broken_files_are_classified() {
    assert!(matches!(
        parse(&[0xff, 0xfe, 0x00], Path::new("x"), false),
        Err(ParseError::NotUtf8)
    ));
    assert!(matches!(
        parse(b"---\nname: [\n...\n", Path::new("x"), false),
        Err(ParseError::BadHeader(_))
    ));
    assert!(matches!(
        parse(b"just text", Path::new("x"), false),
        Err(ParseError::NoBody)
    ));
    assert!(p(HEAD, false).entries.is_empty(), "只有头 = 零条目");
}
