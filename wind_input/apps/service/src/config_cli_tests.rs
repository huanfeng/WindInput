use super::*;

#[test]
fn parse_value_typed_by_registry() {
    assert_eq!(
        parse_value("ui.candidate.per_page", "9").unwrap(),
        toml::Value::Integer(9)
    );
    assert_eq!(
        parse_value("ui.candidate.hide_window", "true").unwrap(),
        toml::Value::Boolean(true)
    );
    assert_eq!(
        parse_value("ui.candidate.hide_window", "off").unwrap(),
        toml::Value::Boolean(false)
    );
    assert_eq!(
        parse_value("ui.candidate.font_size", "18").unwrap(),
        toml::Value::Float(18.0)
    );
    assert_eq!(
        parse_value("ui.candidate.layout", "vertical").unwrap(),
        toml::Value::String("vertical".into())
    );
    // 字符串列表按逗号拆分
    let list = parse_value("schema.available", "wubi86, wubi86_pinyin").unwrap();
    assert_eq!(
        list,
        toml::Value::Array(vec![
            toml::Value::String("wubi86".into()),
            toml::Value::String("wubi86_pinyin".into()),
        ])
    );
}

#[test]
fn parse_value_rejects_bad_input() {
    assert!(parse_value("ui.candidate.per_page", "seven").is_err());
    assert!(parse_value("ui.candidate.hide_window", "maybe").is_err());
    assert!(parse_value("no.such.key", "x").is_err());
}

#[test]
fn parse_value_enum_passes_raw_then_validate_catches_range() {
    // parse 不校验枚举成员（交给 validate）；越界值先解析成字符串
    let v = parse_value("ui.candidate.layout", "diagonal").unwrap();
    assert_eq!(v, toml::Value::String("diagonal".into()));
    assert!(validate("ui.candidate.layout", &v).is_err());
}

#[test]
fn format_value_unquotes_string() {
    assert_eq!(format_value(&json!("vertical")), "vertical");
    assert_eq!(format_value(&json!(7)), "7");
    assert_eq!(format_value(&json!(true)), "true");
}
