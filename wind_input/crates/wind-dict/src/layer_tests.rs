use super::*;

#[test]
fn pattern_matches_exact_length_and_prefix() {
    let w = '?';
    assert!(pattern_matches("a?c", w, "abc", false));
    assert!(!pattern_matches("a?c", w, "abd", false));
    assert!(
        !pattern_matches("a?c", w, "ab", false),
        "短于 pattern 不匹配：不做王码「末尾通配匹配更短码」"
    );
    assert!(
        !pattern_matches("a?c", w, "abcd", false),
        "更长码只在 with_prefix 时匹配"
    );
    assert!(pattern_matches("a?c", w, "abcd", true));
    assert!(pattern_matches("??", w, "zz", false), "首位通配");
    assert!(
        !pattern_matches("??", w, "z", true),
        "with_prefix 也不放行更短码"
    );
}

#[test]
fn literal_prefix_stops_at_first_wildcard() {
    let w = '?';
    assert_eq!(literal_prefix("ab?d", w), "ab");
    assert_eq!(
        literal_prefix("?b", w),
        "",
        "首位通配 ⇒ 字面前缀为空（退化全表扫描）"
    );
    assert_eq!(literal_prefix("abc", w), "abc");
}

#[test]
fn cmp_pattern_puts_equal_length_first_then_better() {
    let c = |code: &str, w: i32| Candidate {
        code: code.into(),
        weight: w,
        ..Default::default()
    };
    let mut v = [c("abcd", 9999), c("ab", 10), c("ac", 20)];
    v.sort_by(|a, b| cmp_pattern(2, a, b));
    let codes: Vec<&str> = v.iter().map(|x| x.code.as_str()).collect();
    assert_eq!(
        codes,
        ["ac", "ab", "abcd"],
        "等长档恒在前，档内按 better（权重降序）"
    );
}
