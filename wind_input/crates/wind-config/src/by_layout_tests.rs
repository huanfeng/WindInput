use super::*;

#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
#[serde(default)]
struct Holder {
    x: ByLayout<String>,
    after: String,
}

fn de(s: &str) -> Holder {
    toml::from_str(s).expect("应能解析")
}

#[test]
fn scalar_is_shared_by_both_layouts() {
    let h = de(r#"x = "hide""#);
    assert_eq!(h.x.as_str(false), "hide");
    assert_eq!(h.x.as_str(true), "hide");
    assert!(!h.x.is_split());
}

#[test]
fn tagged_splits_the_two_layouts() {
    let h = de(r#"x = "h:hide v:always""#);
    assert_eq!(h.x.as_str(false), "hide");
    assert_eq!(h.x.as_str(true), "always");
    assert!(h.x.is_split());
}

#[test]
fn one_sided_leaves_the_other_at_default() {
    // 「只覆盖一侧」是分档最常见的用法，缺侧必须是默认值（＝不覆盖），
    // 不能像主题 Ld 那样回退另一侧——否则这个用法根本表达不出来。
    let h = de(r#"x = "v:always""#);
    assert_eq!(h.x.as_str(true), "always");
    assert_eq!(h.x.as_str(false), "", "横排没写 ⇒ 不覆盖，不是跟着竖排走");
}

#[test]
fn full_dim_names_are_accepted() {
    let h = de(r#"x = "horizontal:hide vertical:always""#);
    assert_eq!((h.x.as_str(false), h.x.as_str(true)), ("hide", "always"));
}

#[test]
fn table_form_is_accepted_too() {
    let h = de(r#"x = { h = "hide", v = "always" }"#);
    assert_eq!((h.x.as_str(false), h.x.as_str(true)), ("hide", "always"));
    let full = de(r#"x = { horizontal = "hide", vertical = "always" }"#);
    assert_eq!(full.x, h.x, "表写法的全称别名应与缩写等价");
}

// ── 三条防护 ──────────────────────────────────────────────

#[test]
fn partially_unknown_dim_is_an_error_not_silently_ignored() {
    // 明显是分档意图却写错一个档位名 ⇒ 本层报错（不可能是谁的合法标量值，不误伤）。
    // 忽略的话，那一档会静默变成「没配过」，而用户明明写了东西。
    let e = toml::from_str::<Holder>(r#"x = "h:hide x:always""#).unwrap_err();
    assert!(e.to_string().contains("排布档位"), "实得：{e}");
}

#[test]
fn wholly_unknown_dim_falls_through_to_value_domain_check() {
    // ★ "x:hide" 与 "C:\path" 语法同形，本层分辨不了，故一律当标量放过——
    // 它必然不在该键的 Enum 值域里，由值域校验那一层报错（见 DIM_NAMES 文档）。
    // 这条钉住的是「本层不自作聪明」，不是「拼错没人管」。
    let h = de(r#"x = "x:hide""#);
    assert!(!h.x.is_split());
    assert_eq!(h.x.as_str(false), "x:hide", "原样留给值域校验去判");
}

#[test]
fn malformed_token_is_an_error() {
    let e = toml::from_str::<Holder>(r#"x = "h:hide always""#).unwrap_err();
    assert!(e.to_string().contains("档位"), "实得：{e}");
}

#[test]
fn duplicate_dim_is_an_error() {
    let e = toml::from_str::<Holder>(r#"x = "h:hide h:always""#).unwrap_err();
    assert!(e.to_string().contains("不止一次"), "实得：{e}");
}

#[test]
fn values_containing_colons_are_not_mistaken_for_tags() {
    // ★ 歧义判据：冒号前的 token 不在档位名单里 ⇒ 整串当标量。
    for raw in [
        r#"x = "C:\\path\\to""#,
        r#"x = "18:00""#,
        r#"x = "note: see""#,
    ] {
        let h = de(raw);
        assert!(!h.x.is_split(), "不该被当成分档语法：{raw}");
    }
}

// ── 序列化：形态不漂移 ────────────────────────────────────

#[test]
fn scalar_round_trips_unchanged() {
    let h = de(r#"
x = "hide"
after = "z"
"#);
    let out = toml::to_string(&h).unwrap();
    assert!(out.contains(r#"x = "hide""#), "标量往返应原样：{out}");
    assert_eq!(de(&out).x, h.x);
}

#[test]
fn split_serializes_as_a_tag_string_not_a_subtable() {
    // 这条是本 wire format 存在的全部理由：表写法会被 to_string 写成子段
    // `[x]`，把同段后面的标量字段挤到前面去；标签式永远是标量，形态不漂移。
    let h = de(r#"
x = { h = "hide", v = "always" }
after = "z"
"#);
    let out = toml::to_string(&h).unwrap();
    assert!(!out.contains("[x]"), "不该裂出子段：{out}");
    assert!(out.contains(r#"x = "h:hide v:always""#), "实得：{out}");
    // 往返闭合
    assert_eq!(de(&out).x, h.x);
}

#[test]
fn empty_side_is_omitted_from_the_tag_string() {
    let h = de(r#"x = { v = "always" }"#);
    let out = toml::to_string(&h).unwrap();
    assert!(out.contains(r#"x = "v:always""#), "实得：{out}");
}

#[test]
fn values_with_spaces_fall_back_to_table_form() {
    // 值里有空格时标签式会自伤，改用表写法（接受子段化）。
    let h = ByLayout::Split {
        h: Some("a b".to_string()),
        v: Some("c".to_string()),
    };
    let out = toml::to_string(&Holder {
        x: h.clone(),
        after: String::new(),
    })
    .unwrap();
    assert!(out.contains("[x]"), "含空格应回退表写法：{out}");
    assert_eq!(de(&out).x, h, "表写法仍须往返闭合");
}
