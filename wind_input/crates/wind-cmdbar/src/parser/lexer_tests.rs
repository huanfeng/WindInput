use super::*;

fn kinds(src: &str) -> Vec<TokenKind> {
    Lexer::new(src)
        .tokenize()
        .unwrap()
        .into_iter()
        .map(|t| t.kind)
        .collect()
}

#[test]
fn lex_call() {
    use TokenKind::*;
    assert_eq!(kinds("len(code)"), vec![Ident, LParen, Ident, RParen, Eof]);
}

#[test]
fn lex_namespaced_call_with_string() {
    use TokenKind::*;
    assert_eq!(
        kinds(r#"clip.copy("hi")"#),
        vec![Ident, Dot, Ident, LParen, String, RParen, Eof]
    );
}

#[test]
fn lex_number_negative_and_float() {
    let toks = Lexer::new("-3, 2.5").tokenize().unwrap();
    assert_eq!(toks[0].kind, TokenKind::Number);
    assert_eq!(toks[0].number, -3.0);
    assert_eq!(toks[2].number, 2.5);
}

#[test]
fn lex_bare_minus_errors() {
    assert!(Lexer::new("- 3").tokenize().is_err());
}

#[test]
fn lex_object_bag() {
    use TokenKind::*;
    assert_eq!(
        kinds("{prefix: true}"),
        vec![LBrace, Ident, Colon, Ident, RBrace, Eof]
    );
}

#[test]
fn string_parts_literal_and_interp() {
    let toks = Lexer::new(r#""a{len(x)}b""#).tokenize().unwrap();
    assert_eq!(toks[0].kind, TokenKind::String);
    let parts = &toks[0].parts;
    assert_eq!(parts.len(), 3);
    assert!(matches!(&parts[0], RawStringPart::Lit { text, .. } if text == "a"));
    assert!(matches!(&parts[1], RawStringPart::Interp { raw, .. } if raw == "len(x)"));
    assert!(matches!(&parts[2], RawStringPart::Lit { text, .. } if text == "b"));
}

#[test]
fn string_escapes() {
    let toks = Lexer::new(r#""a\nb\{c\}\\""#).tokenize().unwrap();
    match &toks[0].parts[0] {
        RawStringPart::Lit { text, .. } => assert_eq!(text, "a\nb{c}\\"),
        _ => panic!("expected lit"),
    }
}

#[test]
fn interp_with_nested_braces_and_inner_string() {
    // 内层字符串里的 `}` 不应提前闭合插值
    let toks = Lexer::new(r#""{default(x, "}")}""#).tokenize().unwrap();
    let parts = &toks[0].parts;
    assert_eq!(parts.len(), 1);
    assert!(matches!(&parts[0], RawStringPart::Interp { raw, .. } if raw == r#"default(x, "}")"#));
}

#[test]
fn unknown_escape_preserved() {
    let toks = Lexer::new(r#""a\xb""#).tokenize().unwrap();
    match &toks[0].parts[0] {
        RawStringPart::Lit { text, .. } => assert_eq!(text, "a\\xb"),
        _ => panic!(),
    }
}

#[test]
fn escape_before_multibyte_no_panic() {
    // 回归：反斜杠紧贴多字节字符曾致 `self.pos += 2` 落进字符中间 → 切片 panic。
    // 未知转义 `\文` 保留反斜杠 + 完整字符；`\\文` 转义反斜杠后中文完整。
    let toks = Lexer::new(r#""E:\文档\\市""#).tokenize().unwrap();
    match &toks[0].parts[0] {
        RawStringPart::Lit { text, .. } => assert_eq!(text, r"E:\文档\市"),
        _ => panic!("expected lit"),
    }
}

#[test]
fn utf8_char_len_covers_ranges() {
    assert_eq!(utf8_char_len(b'A'), 1);
    assert_eq!(utf8_char_len("é".as_bytes()[0]), 2);
    assert_eq!(utf8_char_len("文".as_bytes()[0]), 3);
    assert_eq!(utf8_char_len("𝄞".as_bytes()[0]), 4);
}

#[test]
fn no_infinite_loop_on_unicode() {
    // 全角空格起始：不是 ASCII ident 起始 → 立即报错而非死循环
    assert!(Lexer::new("　").tokenize().is_err());
}
