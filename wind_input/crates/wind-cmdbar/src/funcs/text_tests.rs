use super::*;
use crate::context::MemoryContext;

fn call(f: crate::registry::EvalFn, args: &[&str]) -> String {
    let ctx = MemoryContext::new();
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    f(&ctx, &a).unwrap()
}

#[test]
fn basic_text() {
    assert_eq!(call(fn_len, &["héllo"]), "5");
    assert_eq!(call(fn_upper, &["aá"]), "AÁ");
    assert_eq!(call(fn_reverse, &["abc"]), "cba");
    assert_eq!(call(fn_concat, &["a", "b", "c"]), "abc");
    assert_eq!(call(fn_default, &["", "x"]), "x");
    assert_eq!(call(fn_default, &["y", "x"]), "y");
}

#[test]
fn sub_1based_inclusive_negative() {
    assert_eq!(call(fn_sub, &["abcde", "2"]), "bcde");
    assert_eq!(call(fn_sub, &["abcde", "2", "4"]), "bcd");
    assert_eq!(call(fn_sub, &["abcde", "-2"]), "de");
    assert_eq!(call(fn_sub, &["abcde", "1", "-1"]), "abcde");
    assert_eq!(call(fn_sub, &["abc", "5"]), "");
}

#[test]
fn split_replace_regex() {
    assert_eq!(call(fn_split, &["a,b,c", ",", "2"]), "b");
    assert_eq!(call(fn_split, &["a,b,c", ",", "-1"]), "c");
    assert_eq!(call(fn_replace, &["aXaXa", "X", "-"]), "a-a-a");
    assert_eq!(call(fn_regex, &["a1b22c", "\\d+", "#"]), "a#b#c");
}

/// 文档写明 `\w` `\d` 可用，用户拿它处理中文剪贴板：类按 Unicode 算（汉字是 `\w`、
/// 全角数字是 `\d`）。钉的是 regex 的 `unicode` feature——换 regex-lite 或关 unicode
/// 会让这两类退成 ASCII。
#[test]
fn regex_classes_are_unicode_aware() {
    assert_eq!(call(fn_regex, &["清风 input", "\\w+", "W"]), "W W");
    assert_eq!(call(fn_regex, &["电话１２３", "\\d+", "#"]), "电话#");
}

#[test]
fn encoders() {
    assert_eq!(call(fn_url, &["a b&c"]), "a+b%26c");
    assert_eq!(call(fn_html, &["<a>&'\""]), "&lt;a&gt;&amp;&#39;&#34;");
    assert_eq!(call(fn_json, &["a\"b"]), "\"a\\\"b\"");
    assert_eq!(call(fn_base64, &["hi"]), "aGk=");
}
