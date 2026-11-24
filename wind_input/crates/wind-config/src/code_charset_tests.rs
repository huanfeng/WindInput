use super::*;

fn set_of(spec: &str) -> String {
    CodeCharSet::parse(spec)
        .unwrap()
        .chars()
        .into_iter()
        .collect()
}

#[test]
fn default_alpha_is_26_letters() {
    let s = CodeCharSet::default_alpha();
    assert_eq!(s.chars().len(), 26);
    assert!(s.contains('a') && s.contains('z'));
    assert!(!s.contains('0') && !s.contains('/'));
    assert!(s.is_default_alpha());
    assert!(!s.has_non_leading(), "默认集的首码集应等于全集");
}

#[test]
fn parses_range() {
    assert_eq!(set_of("a-x"), "abcdefghijklmnopqrstuvwx");
    assert_eq!(set_of("a-c"), "abc");
}

/// 注意输出恒按 ASCII 升序，与规格里的书写顺序无关（`/` 是 0x2F，排在字母之前）。
#[test]
fn parses_range_plus_literal() {
    assert_eq!(set_of("a-x/"), "/abcdefghijklmnopqrstuvwx");
    // 字面写在范围前后，结果相同。
    assert_eq!(set_of("/a-c"), "/abc");
    assert_eq!(set_of("a-c/"), "/abc");
}

#[test]
fn parses_multiple_ranges() {
    assert_eq!(set_of("a-c0-2"), "012abc");
}

#[test]
fn parses_pure_literals() {
    assert_eq!(set_of("abc"), "abc");
    assert_eq!(set_of(";'"), "';"); // 输出按 ASCII 升序
}

/// `-` 在首位或末位作字面，与正则字符类惯例一致。
#[test]
fn dash_as_literal_at_edges() {
    assert_eq!(set_of("-a-c"), "-abc");
    assert_eq!(set_of("a-c-"), "-abc");
    assert_eq!(set_of("-"), "-");
}

/// 大小写一律归一为小写——`input_buffer` 恒存小写，不同域则集内大写永不命中。
#[test]
fn normalizes_to_lowercase() {
    assert_eq!(set_of("A-Z"), set_of("a-z"));
    assert_eq!(set_of("ABC"), "abc");
    let s = CodeCharSet::parse("A-Z").unwrap();
    assert!(s.contains('a'));
    assert!(s.is_default_alpha(), "A-Z 归一后应等价于内置默认");
}

#[test]
fn rejects_bad_input() {
    assert_eq!(
        CodeCharSet::parse("z-a"),
        Err(CodeCharSetError::BadRange('z', 'a'))
    );
    assert_eq!(CodeCharSet::parse("中"), Err(CodeCharSetError::NonAscii));
    assert_eq!(CodeCharSet::parse(""), Err(CodeCharSetError::Empty));
    assert!(matches!(
        CodeCharSet::parse("a b"),
        Err(CodeCharSetError::NonPrintable(' '))
    ));
}

/// ★ 非法输入必须回落 `a-z`，**绝不能变成空集**——空集＝该方案一个字也打不出。
#[test]
fn invalid_falls_back_to_alpha_never_empty() {
    for bad in ["", "   ", "z-a", "中文", "a b"] {
        let s = CodeCharSet::new(bad, "", "test");
        assert!(
            s.is_default_alpha(),
            "非法输入 {:?} 应回落 a-z，实际 {:?}",
            bad,
            s.chars()
        );
        assert!(s.contains('a'), "回落后必须能打字，{:?}", bad);
    }
}

/// `default_contains` 是 `default_alpha().contains` 的免构造版本，两者必须恒等——
/// 它们分别服务于「引擎无码元集」与「引擎有码元集」两条路径，漂移就会让同一个字符
/// 在两条路上得到相反的判定。
#[test]
fn default_contains_matches_default_alpha() {
    let s = CodeCharSet::default_alpha();
    for c in 0u8..128 {
        let ch = c as char;
        assert_eq!(
            CodeCharSet::default_contains(ch),
            s.contains(ch),
            "字符 {:?} 的两种默认判定不一致",
            ch
        );
    }
    assert!(!CodeCharSet::default_contains('中'));
}

#[test]
fn contains_rejects_non_ascii() {
    let s = CodeCharSet::parse("a-z").unwrap();
    assert!(!s.contains('中'));
    assert!(!s.contains('\u{1F600}'));
    assert!(!s.contains_leading('中'));
}

/// 子集与含符号集都不得被误判为默认集——`is_default_alpha` 是各消费点的快捷路径开关，
/// 误判会让配了码元集的方案走进「与历史等价」的老路，表现为配置完全没生效。
#[test]
fn non_default_sets_are_not_default_alpha() {
    assert!(!CodeCharSet::parse("a-x").unwrap().is_default_alpha());
    assert!(!CodeCharSet::parse("a-z/").unwrap().is_default_alpha());
    assert!(!CodeCharSet::new("a-z0-9", "a-z", "t").is_default_alpha());
}

// ── 首码集 ──

/// 主场景：数字可作码元（`Win10`）但不可作首码。
#[test]
fn digits_as_code_but_not_leading() {
    let s = CodeCharSet::new("a-z0-9", "a-z", "test");
    assert!(s.contains('1'), "数字应是码元");
    assert!(!s.contains_leading('1'), "数字不应可作首码");
    assert!(s.contains('w') && s.contains_leading('w'));
    assert!(s.has_non_leading());
}

/// 未配首码集时，首码集 = 全集。
#[test]
fn leading_defaults_to_all() {
    let s = CodeCharSet::new("a-z0-9", "", "test");
    assert!(s.contains('1') && s.contains_leading('1'));
    assert!(!s.has_non_leading());
    assert_eq!(s.chars(), s.leading_chars());
}

/// ★ 首码集恒为全集子集：配了全集外的字符要被丢掉，否则该字符会在空缓冲时进缓冲、
/// 下一键却因不是码元而非法，形成走不出去的状态。
#[test]
fn leading_is_intersected_with_all() {
    let s = CodeCharSet::new("a-c", "a-z", "test");
    assert_eq!(s.leading_chars(), vec!['a', 'b', 'c']);
    assert!(!s.contains_leading('x'), "全集外的首码字符必须被丢弃");
}

/// 首码集与全集无交集 → 回落为全集，而不是留下一个「无法起头」的死方案。
#[test]
fn leading_disjoint_falls_back_to_all() {
    let s = CodeCharSet::new("a-c", "x-z", "test");
    assert_eq!(s.leading_chars(), vec!['a', 'b', 'c']);
    assert!(s.contains_leading('a'), "无交集时必须仍能起头");
}

/// 首码集非法 → 回落为全集（保守：宁可多允许，不可让方案打不出字）。
#[test]
fn invalid_leading_falls_back_to_all() {
    let s = CodeCharSet::new("a-z0-9", "z-a", "test");
    assert!(s.contains_leading('0'), "首码集非法时应回落为全集");
    assert!(!s.has_non_leading());
}
