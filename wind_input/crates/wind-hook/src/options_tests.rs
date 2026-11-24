use super::*;
use serde_json::json;

fn specs() -> Vec<OptionSpec> {
    #[derive(Deserialize)]
    struct M {
        option: Vec<OptionSpec>,
    }
    let m: M = toml::from_str(
        r#"
            [[option]]
            key = "theme"
            label = "音色"
            type = "choice"
            default = "click"
            choices = [{ value = "click", label = "机械键盘" }, { value = "piano", label = "钢琴" }]

            [[option]]
            key = "volume"
            type = "number"
            default = 0.6
            min = 0.0
            max = 1.0
            step = 0.05

            [[option]]
            key = "mute_in_games"
            type = "bool"
            default = false
            "#,
    )
    .unwrap();
    m.option
}

#[test]
fn parses_and_validates() {
    let s = specs();
    assert_eq!(s.len(), 3);
    assert!(validate(&s).is_ok());
    assert_eq!(s[0].label, "音色");
}

#[test]
fn defaults_fill_missing_values() {
    let v = resolve(&specs(), &Map::new());
    assert_eq!(
        Value::Object(v),
        json!({"theme": "click", "volume": 0.6, "mute_in_games": false})
    );
}

#[test]
fn bad_values_fall_back_and_numbers_clamp() {
    let user = json!({"theme": "organ", "volume": 3.0, "mute_in_games": "yes", "extra": 1});
    let v = resolve(&specs(), user.as_object().unwrap());
    assert_eq!(
        Value::Object(v),
        json!({"theme": "click", "volume": 1.0, "mute_in_games": false})
    );
    let user = json!({"theme": "piano", "volume": 0.2, "mute_in_games": true});
    let v = resolve(&specs(), user.as_object().unwrap());
    assert_eq!(
        Value::Object(v),
        json!({"theme": "piano", "volume": 0.2, "mute_in_games": true})
    );
}

#[test]
fn broken_declarations_are_rejected() {
    let mut s = specs();
    s[1].key = "theme".into();
    assert!(validate(&s).unwrap_err().contains("重复"));
    let mut s = specs();
    if let OptionKind::Choice { default, .. } = &mut s[0].kind {
        *default = "organ".into();
    }
    assert!(validate(&s).is_err());
    let mut s = specs();
    if let OptionKind::Number { default, .. } = &mut s[1].kind {
        *default = 2.0;
    }
    assert!(validate(&s).is_err());
}

#[test]
fn serializes_for_the_settings_page() {
    let v = serde_json::to_value(&specs()[1]).unwrap();
    assert_eq!(
        v,
        json!({"key": "volume", "label": "", "type": "number", "default": 0.6, "min": 0.0, "max": 1.0, "step": 0.05})
    );
}
