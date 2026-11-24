use super::*;

const KEY: &str = "ui.candidate.pager_bar_display";

fn v(s: &str) -> toml::Value {
    toml::Value::String(s.to_string())
}

#[test]
fn scalar_value_still_validates() {
    assert!(validate(KEY, &v("hide")).is_ok());
    assert!(validate(KEY, &v("")).is_ok(), "空串＝跟随主题，是合法值");
}

#[test]
fn each_side_of_a_tagged_value_is_checked() {
    assert!(validate(KEY, &v("h:hide v:always")).is_ok());
    assert!(validate(KEY, &v("v:always")).is_ok(), "只覆盖一侧");
}

#[test]
fn a_bad_value_on_one_side_is_caught() {
    // 分档不是绕过值域的后门：任一档越界都要报出来，且报的是那一档的值。
    let e = validate(KEY, &v("h:hide v:alwys")).unwrap_err();
    assert!(
        matches!(&e, ValidateError::EnumOutOfRange { got, .. } if got == "alwys"),
        "实得：{e:?}"
    );
}

#[test]
fn misspelled_dim_falls_through_to_the_value_domain() {
    // ★ 「拼错档位名」的兜底就在这里：`"x:hide"` 在解析层与 `"C:\path"` 同形、
    // 分辨不了，于是原样当标量落到值域比对 ⇒ 报越界。错误照样看得见。
    let e = validate(KEY, &v("x:hide")).unwrap_err();
    assert!(
        matches!(&e, ValidateError::EnumOutOfRange { got, .. } if got == "x:hide"),
        "实得：{e:?}"
    );
}

#[test]
fn table_form_validates_per_side() {
    let mut t = toml::map::Map::new();
    t.insert("h".into(), v("hide"));
    t.insert("v".into(), v("always"));
    assert!(validate(KEY, &toml::Value::Table(t.clone())).is_ok());
    t.insert("v".into(), v("nope"));
    assert!(
        validate(KEY, &toml::Value::Table(t)).is_err(),
        "表写法同样逐档验"
    );
}

#[test]
fn table_form_is_a_leaf_not_drilled_into() {
    // 下钻的话 `….pager_bar_display.h` 会被当成注册表里没有的键，两个方向同时报错。
    let mut t = toml::map::Map::new();
    t.insert("h".into(), v("hide"));
    let mut out = Vec::new();
    collect_leaf_keys(KEY, &toml::Value::Table(t), &mut out);
    assert_eq!(out, vec![KEY.to_string()]);
}

#[test]
fn is_not_a_whole_value_leaf() {
    // 设置端写它时发的是一个标量（标签式），不该落进写盘闸的「整体一份」那一类。
    assert!(!field(KEY).unwrap().ty.is_whole_value_leaf());
}
