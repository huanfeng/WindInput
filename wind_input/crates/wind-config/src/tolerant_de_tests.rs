use super::*;
use crate::config::TextOrientation;

#[derive(Debug, Deserialize, PartialEq)]
struct Holder {
    #[serde(default, deserialize_with = "tolerant")]
    v: TextOrientation,
}

#[derive(Debug, Deserialize, PartialEq)]
struct OptHolder {
    #[serde(default, deserialize_with = "tolerant_opt")]
    v: Option<TextOrientation>,
}

#[test]
fn valid_value_still_parses() {
    let h: Holder = toml::from_str("v = \"upright\"").unwrap();
    assert_eq!(h.v, TextOrientation::Upright);
}

/// 本次事故的最小复现：空串曾让**整个方案**读不出来。
#[test]
fn empty_string_falls_back_instead_of_failing() {
    let h: Holder = toml::from_str("v = \"\"").expect("空串不得让整份解析失败");
    assert_eq!(h.v, TextOrientation::Normal);
}

#[test]
fn misspelled_value_falls_back() {
    let h: Holder = toml::from_str("v = \"uprigth\"").expect("拼错不得让整份解析失败");
    assert_eq!(h.v, TextOrientation::Normal);
}

/// ★ `Option` 版回落 `None` 而不是 `Some(默认值)`——见 [`tolerant_opt`] 的文档。
#[test]
fn option_version_falls_back_to_none_not_default() {
    let h: OptHolder = toml::from_str("v = \"nonsense\"").expect("不得整份失败");
    assert_eq!(h.v, None, "认不出 = 没配过，不是「配了默认档」");
}

#[test]
fn option_version_keeps_valid_value() {
    let h: OptHolder = toml::from_str("v = \"rotated\"").unwrap();
    assert_eq!(h.v, Some(TextOrientation::Rotated));
}

/// 类型写错（不是字符串）仍照旧报错，交给段级降级——见模块头部「只治字符串写错」。
#[test]
fn wrong_type_still_errors() {
    assert!(toml::from_str::<Holder>("v = 3").is_err());
}

/// `Option` 版只服务 compat.toml（无段级降级），类型错也回落 `None`，不得整份失败。
#[test]
fn option_version_swallows_wrong_type() {
    for bad in ["3", "true", "[\"upright\"]", "{ a = 1 }"] {
        let h: OptHolder = toml::from_str(&format!("v = {bad}"))
            .unwrap_or_else(|e| panic!("v = {bad} 不得整份失败：{e}"));
        assert_eq!(h.v, None, "v = {bad}");
    }
}

#[derive(Debug, Deserialize, PartialEq)]
struct OptBoolHolder {
    #[serde(default, deserialize_with = "tolerant_opt_bool")]
    v: Option<bool>,
    #[serde(default)]
    other: i32,
}

#[test]
fn opt_bool_keeps_booleans_and_absence() {
    let h: OptBoolHolder = toml::from_str("v = false").unwrap();
    assert_eq!(h.v, Some(false));
    let h: OptBoolHolder = toml::from_str("v = true").unwrap();
    assert_eq!(h.v, Some(true));
    let h: OptBoolHolder = toml::from_str("other = 1").unwrap();
    assert_eq!(h.v, None);
}

/// 非布尔值回落 `None`（= 没配过），且不牵连同表其它字段。
#[test]
fn opt_bool_non_boolean_falls_back_to_none() {
    for bad in [r#""yes""#, "1", "1.5", "[true]", "{ a = 1 }"] {
        let h: OptBoolHolder = toml::from_str(&format!("v = {bad}\nother = 7"))
            .unwrap_or_else(|e| panic!("{bad}: 不得整份失败：{e}"));
        assert_eq!(h.v, None, "{bad}");
        assert_eq!(h.other, 7, "{bad}: 同表其它字段照常");
    }
}

#[derive(Debug, Deserialize, PartialEq)]
struct I32Holder {
    #[serde(default, deserialize_with = "tolerant_i32")]
    v: i32,
    #[serde(default)]
    other: i32,
}

#[test]
fn i32_keeps_integers_and_absence() {
    let h: I32Holder = toml::from_str("v = -12").unwrap();
    assert_eq!(h.v, -12);
    let h: I32Holder = toml::from_str("other = 1").unwrap();
    assert_eq!(h.v, 0);
}

/// 非整数与越界整数回落 `0`（= 没配过），且不牵连同表其它字段。
#[test]
fn i32_wrong_type_or_overflow_falls_back_to_zero() {
    for bad in [r#""12""#, "1.5", "true", "[1]", "{ a = 1 }", "4294967296"] {
        let h: I32Holder = toml::from_str(&format!("v = {bad}\nother = 7"))
            .unwrap_or_else(|e| panic!("{bad}: 不得整份失败：{e}"));
        assert_eq!(h.v, 0, "{bad}");
        assert_eq!(h.other, 7, "{bad}: 同表其它字段照常");
    }
}
