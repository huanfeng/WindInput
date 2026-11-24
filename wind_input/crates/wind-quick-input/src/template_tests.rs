use super::*;

fn fixed(name: &str) -> Option<String> {
    match name {
        "Y" => Some("2026".into()),
        "M" => Some("6".into()),
        "MM" => Some("06".into()),
        "YC" => Some("二〇二六".into()),
        _ => None,
    }
}

#[test]
fn expands_bare_and_braced() {
    assert_eq!(expand("$Y年$M月", fixed).unwrap(), "2026年6月");
    assert_eq!(expand("${Y}-${MM}", fixed).unwrap(), "2026-06");
    assert_eq!(expand("$YC年", fixed).unwrap(), "二〇二六年");
}

#[test]
fn adjacent_variables_do_not_merge() {
    // 出厂表里的 $Y$MM 紧邻形态：不能把 YMM 当成一个变量名
    assert_eq!(expand("$Y$MM", fixed).unwrap(), "202606");
}

#[test]
fn bare_name_stops_at_non_alpha() {
    // $name 只吃字母：`$M2` 是 $M 后跟字面 2（要取名为 M2 的变量得写 ${M2}）
    assert_eq!(expand("$M2", fixed).unwrap(), "62");
}

#[test]
fn escape_and_lone_dollar() {
    assert_eq!(expand("$$Y", fixed).unwrap(), "$Y");
    assert_eq!(expand("价格$ 5", fixed).unwrap(), "价格$ 5");
}

#[test]
fn unknown_variable_kills_whole_template() {
    assert!(expand("$Y年$NOPE月", fixed).is_none());
    // 未闭合的 ${ 同样作废
    assert!(expand("${Y", fixed).is_none());
}

#[test]
fn multibyte_literal_is_preserved() {
    assert_eq!(expand("〇年—$M", fixed).unwrap(), "〇年—6");
}

#[test]
fn bare_brace_detection_separates_the_two_paths() {
    // 变量模板：`${}` 的括号不算裸括号
    assert!(!has_bare_brace("$Y年$M月"));
    assert!(!has_bare_brace("${Y}-${MM}"));
    // 表达式模板
    assert!(has_bare_brace("{year()}"));
    assert!(has_bare_brace("{amt(unit='圆')}"));
    // 混用（加载期会拒绝，但判据本身必须认出来）
    assert!(has_bare_brace("${Y}年{month()}月"));
    // $$ 转义后紧跟的 { 仍是裸括号
    assert!(has_bare_brace("$${x}"));
    // 未闭合的 ${ 不算表达式，留给 expand 报语法错
    assert!(!has_bare_brace("${Y"));
}

#[test]
fn has_variable_ignores_literals() {
    assert!(has_variable("$Y年"));
    assert!(has_variable("${Y}"));
    assert!(!has_variable("$$Y"));
    assert!(!has_variable("价格$ 5"));
    assert!(!has_variable("2026年"));
}
