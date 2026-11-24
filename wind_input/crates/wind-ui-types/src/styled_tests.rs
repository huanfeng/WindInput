use super::*;

fn role(r: &'static str) -> SpanStyle {
    SpanStyle {
        role: Some(r),
        ..Default::default()
    }
}

fn ranges(t: &StyledText) -> Vec<(u32, u32, Option<&str>)> {
    t.spans().iter().map(|s| (s.start, s.end, s.role)).collect()
}

#[test]
fn push_merges_adjacent_same_style_and_skips_plain() {
    let mut t = StyledText::new();
    t.push("ab", &role("pinyin"));
    t.push("c", &role("pinyin"));
    t.push("-", &SpanStyle::default());
    t.push("你", &role("chaizi"));
    assert_eq!(t.as_str(), "abc-你");
    assert_eq!(
        ranges(&t),
        vec![(0, 3, Some("pinyin")), (4, 7, Some("chaizi"))]
    );
}

/// 分隔符与所在变量同角色，但不能并进同一区间：主题配了 `separator` 时要分开求色。
#[test]
fn separator_does_not_merge_with_its_variable() {
    let sep = SpanStyle {
        separator: true,
        ..role("chaizi_all")
    };
    let mut t = StyledText::new();
    t.push("亻尔", &role("chaizi_all"));
    t.push("／", &sep);
    t.push("女子", &role("chaizi_all"));
    let got: Vec<(u32, u32, bool)> = t
        .spans()
        .iter()
        .map(|s| (s.start, s.end, s.separator))
        .collect();
    assert_eq!(got, vec![(0, 6, false), (6, 9, true), (9, 15, false)]);
    assert_eq!(t.style_at(6), sep);
}

#[test]
fn pop_shrinks_and_drops_emptied_span() {
    let mut t = StyledText::new();
    t.push("a", &role("x"));
    t.push(" ", &role("literal"));
    assert!(t.pop_if(|c| c == ' '));
    assert_eq!(ranges(&t), vec![(0, 1, Some("x"))]);
    assert!(!t.pop_if(|c| c == ' '));
}

#[test]
fn append_offsets_spans() {
    let mut a = StyledText::from("x");
    let mut b = StyledText::new();
    b.push("好", &role("char"));
    b.push("：", &SpanStyle::default());
    a.append(&b);
    assert_eq!(a.as_str(), "x好：");
    assert_eq!(ranges(&a), vec![(1, 4, Some("char"))]);
}

#[test]
fn trim_and_truncate_keep_styles() {
    let mut t = StyledText::new();
    t.push("  ", &role("literal"));
    t.push("你好吗", &role("pinyin"));
    t.push(" ", &role("literal"));
    let trimmed = t.trim();
    assert_eq!(trimmed.as_str(), "你好吗");
    assert_eq!(ranges(&trimmed), vec![(0, 9, Some("pinyin"))]);
    let cut = trimmed.truncate_chars(2, "…");
    assert_eq!(cut.as_str(), "你好…");
    assert_eq!(
        ranges(&cut),
        vec![(0, 9, Some("pinyin"))],
        "… 继承前一个字的样式"
    );
    assert_eq!(trimmed.truncate_chars(3, "…"), trimmed, "没超出不动");
}

#[test]
fn slice_and_style_at() {
    let mut t = StyledText::new();
    t.push("ab", &role("a"));
    t.push("cd", &role("b"));
    let s = t.slice(1, 3);
    assert_eq!(s.as_str(), "bc");
    assert_eq!(ranges(&s), vec![(0, 1, Some("a")), (1, 2, Some("b"))]);
    assert_eq!(t.style_at(2), role("b"));
    assert_eq!(StyledText::from("x").style_at(0), SpanStyle::default());
}

#[test]
fn join_offsets_every_part() {
    let mut a = StyledText::new();
    a.push("[", &role("title"));
    let j = StyledText::join(&[a.clone(), a], "\n");
    assert_eq!(j.as_str(), "[\n[");
    assert_eq!(
        ranges(&j),
        vec![(0, 1, Some("title")), (2, 3, Some("title"))]
    );
}

#[test]
fn replace_ascii_keeps_spans() {
    let mut t = StyledText::new();
    t.push("a\nb", &role("dict"));
    t.push("\n", &SpanStyle::default());
    t.push("拆", &role("chaizi"));
    let r = t.replace_ascii('\n', ' ');
    assert_eq!(r.as_str(), "a b 拆");
    assert_eq!(
        ranges(&r),
        vec![(0, 3, Some("dict")), (4, 7, Some("chaizi"))]
    );
}
