use super::*;

#[test]
fn lf_only_is_borrowed() {
    let s = "a\nb\nc";
    assert!(matches!(normalize_input(s), Cow::Borrowed("a\nb\nc")));
}

#[test]
fn no_newline_at_all_is_borrowed() {
    assert!(matches!(normalize_input("abc"), Cow::Borrowed("abc")));
}

#[test]
fn bom_is_stripped() {
    assert_eq!(normalize_input("\u{feff}a\nb"), "a\nb");
}

#[test]
fn crlf_is_left_alone_and_borrowed() {
    // `lines()` 本来就把 \r\n 当一个换行，没有理由为它复制一份
    let s = "a\r\nb\r\n";
    assert!(matches!(normalize_input(s), Cow::Borrowed(x) if x == s));
}

#[test]
fn crlf_still_splits_into_the_same_lines_as_lf() {
    // 不改写不等于不生效：下游按 lines() 读到的东西必须一致
    let (a, b) = (normalize_input("a\r\nb\r\n"), normalize_input("a\nb\n"));
    assert_eq!(a.lines().collect::<Vec<_>>(), b.lines().collect::<Vec<_>>());
}

#[test]
fn lone_cr_becomes_newline() {
    assert_eq!(normalize_input("a\rb\rc"), "a\nb\nc");
}

#[test]
fn mixed_endings_all_become_lf() {
    // 只要存在一个孤立 \r 就整份改写：混排文件里 \r\n 一并折成 \n，
    // 结果仍是「每行一条」，不会因为 \r\n 被动过而多出空行
    assert_eq!(normalize_input("a\r\nb\rc\nd"), "a\nb\nc\nd");
}

#[test]
fn trailing_lone_cr() {
    assert_eq!(normalize_input("a\r"), "a\n");
}

#[test]
fn cr_then_crlf_are_two_lines_not_three() {
    // \r 后面跟的是 \r\n：前一个 \r 自己成行，后面的 \r\n 折成一个
    assert_eq!(normalize_input("a\r\r\nb"), "a\n\nb");
}

#[test]
fn bom_with_cr_endings() {
    assert_eq!(normalize_input("\u{feff}a\rb"), "a\nb");
}
