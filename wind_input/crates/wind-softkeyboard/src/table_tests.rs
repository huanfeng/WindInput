use super::*;

fn one_page(body: &str) -> SoftKeyboardTable {
    SoftKeyboardTable::parse(body).expect("解析失败")
}

#[test]
fn canvas_maps_tokens_by_position() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
name = "T"
rows = ["A B C", "D E F"]
"#,
    );
    let p = t.page("t").unwrap();
    assert_eq!(p.output("grave", false), Some("A"));
    assert_eq!(p.output("1", false), Some("B"));
    assert_eq!(p.output("2", false), Some("C"));
    assert_eq!(p.output("q", false), Some("D"));
    assert_eq!(p.output("w", false), Some("E"));
    assert_eq!(p.output("e", false), Some("F"));
    // 没铺到的键位是空的
    assert_eq!(p.output("3", false), None);
    assert_eq!(p.output("z", false), None);
}

#[test]
fn hole_and_escaped_dot() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["A . \\. B"]
"#,
    );
    let p = t.page("t").unwrap();
    assert_eq!(p.output("grave", false), Some("A"));
    assert_eq!(p.output("1", false), None, ". 是空键位占位");
    assert_eq!(p.output("2", false), Some("."), "\\. 是字面点");
    assert_eq!(p.output("3", false), Some("B"));
}

#[test]
fn ideographic_space_survives_tokenizing() {
    // U+3000 是 char::is_whitespace，split_whitespace 会把它吃掉。
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["A 　 B"]
"#,
    );
    let p = t.page("t").unwrap();
    assert_eq!(p.output("grave", false), Some("A"));
    assert_eq!(
        p.output("1", false),
        Some("\u{3000}"),
        "全角空格应作为符号保留"
    );
    assert_eq!(p.output("2", false), Some("B"));
}

#[test]
fn shift_layer_is_separate() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["a b"]
rows_shift = ["A B"]
"#,
    );
    let p = t.page("t").unwrap();
    assert_eq!(p.output("grave", false), Some("a"));
    assert_eq!(p.output("grave", true), Some("A"));
}

#[test]
fn missing_shift_rows_means_empty_second_layer() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["a b"]
"#,
    );
    let p = t.page("t").unwrap();
    assert_eq!(p.output("grave", true), None);
}

#[test]
fn patch_overrides_canvas() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["a b"]
keys = { grave = "Z", "shift+1" = "Y" }
"#,
    );
    let p = t.page("t").unwrap();
    assert_eq!(p.output("grave", false), Some("Z"), "补丁压过画布");
    assert_eq!(p.output("1", false), Some("b"), "没打补丁的键位不动");
    assert_eq!(p.output("1", true), Some("Y"), "补丁可以只写第二层");
}

#[test]
fn patch_can_clear_a_slot() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["a b"]
keys = { grave = "." }
"#,
    );
    assert_eq!(t.page("t").unwrap().output("grave", false), None);
}

#[test]
fn output_accepts_aliases() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["a b c d e f g h i j k l m"]
"#,
    );
    let p = t.page("t").unwrap();
    assert_eq!(p.output("`", false), Some("a"));
    assert_eq!(p.output("GRAVE", false), Some("a"));
    assert_eq!(p.output("-", false), Some("l"));
    assert_eq!(p.output("equals", false), Some("m"));
}

#[test]
fn skips_bad_pages_but_keeps_good_ones() {
    let t = one_page(
        r#"
[[pages]]
id = ""
rows = ["x"]

[[pages]]
id = "good"
rows = ["y"]

[[pages]]
id = "good"
rows = ["z"]

[[pages]]
id = "blank"
rows = [". . ."]
"#,
    );
    assert_eq!(t.len(), 1, "空 id / 重复 id / 全空面都该剔除");
    assert_eq!(t.page("good").unwrap().output("grave", false), Some("y"));
}

#[test]
fn unknown_patch_key_is_skipped_not_fatal() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["a"]
keys = { enter = "X", "ctrl+q" = "Y", w = "W" }
"#,
    );
    let p = t.page("t").unwrap();
    assert_eq!(p.output("grave", false), Some("a"));
    assert_eq!(p.output("w", false), Some("W"), "合法补丁照常生效");
}

#[test]
fn extra_rows_and_tokens_are_truncated() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = [
  "1 2 3 4 5 6 7 8 9 10 11 12 13 14 15",
  "a", "b", "c", "d",
]
"#,
    );
    let p = t.page("t").unwrap();
    assert_eq!(
        p.output("equal", false),
        Some("13"),
        "第 13 个 token 落在最后一个键位"
    );
    assert_eq!(p.output("q", false), Some("a"));
    assert_eq!(p.output("z", false), Some("c"), "第 4 行仍然有效");
}

// ── 用户覆盖三态 ──

#[test]
fn user_patch_only_when_rows_absent() {
    let mut t = one_page(
        r#"
[[pages]]
id = "t"
name = "出厂"
rows = ["a b c"]
"#,
    );
    t.merge_user(
        r#"
[[pages]]
id = "t"
keys = { "1" = "X" }
"#,
    )
    .unwrap();
    let p = t.page("t").unwrap();
    assert_eq!(p.output("grave", false), Some("a"), "基础画布不动");
    assert_eq!(p.output("1", false), Some("X"), "补丁生效");
    assert_eq!(p.output("2", false), Some("c"));
    assert_eq!(p.name, "出厂", "没给 name 就不动");
}

#[test]
fn user_rows_replace_whole_page() {
    let mut t = one_page(
        r#"
[[pages]]
id = "t"
name = "出厂"
rows = ["a b c"]
"#,
    );
    t.merge_user(
        r#"
[[pages]]
id = "t"
rows = ["X"]
"#,
    )
    .unwrap();
    let p = t.page("t").unwrap();
    assert_eq!(p.output("grave", false), Some("X"));
    assert_eq!(p.output("1", false), None, "整面替换，旧画布不残留");
    assert_eq!(p.name, "出厂", "名字缺省时保留出厂名");
}

#[test]
fn user_unknown_id_appends_page() {
    let mut t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["a"]
"#,
    );
    t.merge_user(
        r#"
[[pages]]
id = "mine"
name = "我的"
rows = ["Z"]
"#,
    )
    .unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(t.index_of("mine"), Some(1), "新增面追加在末尾");
    assert_eq!(t.page("mine").unwrap().name, "我的");
}

#[test]
fn builtin_is_parseable_and_nonempty() {
    let t = SoftKeyboardTable::builtin();
    assert!(!t.is_empty());
    assert_eq!(t.page("punct").unwrap().output("1", false), Some("！"));
}

#[test]
fn entries_lists_both_layers() {
    let t = one_page(
        r#"
[[pages]]
id = "t"
rows = ["a"]
rows_shift = ["A"]
"#,
    );
    let mut got: Vec<_> = t.page("t").unwrap().entries().collect();
    got.sort();
    assert_eq!(got, vec![("grave", false, "a"), ("grave", true, "A")]);
}
