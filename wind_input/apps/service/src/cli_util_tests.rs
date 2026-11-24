use super::*;
use serde_json::json;

#[test]
fn get_path_walks_nested_objects() {
    let v = json!({ "a": { "b": { "c": 7 } } });
    assert_eq!(json_get_path(&v, "a.b.c"), Some(&json!(7)));
    assert_eq!(json_get_path(&v, "a.b"), Some(&json!({ "c": 7 })));
    assert_eq!(json_get_path(&v, "a.x.c"), None);
    assert_eq!(json_get_path(&v, "a.b.c.d"), None);
}

#[test]
fn set_path_only_replaces_existing_keys() {
    let mut v = json!({ "a": { "b": 1 }, "s": "x" });
    assert!(json_set_path(&mut v, "a.b", json!(2)));
    assert_eq!(v["a"]["b"], json!(2));
    // 不存在的键不凭空创建（防 typo 静默写进 override）。
    assert!(!json_set_path(&mut v, "a.zzz", json!(3)));
    assert!(!json_set_path(&mut v, "no.such", json!(3)));
    // 终点父级不是对象。
    assert!(!json_set_path(&mut v, "s.sub", json!(3)));
}

#[test]
fn coerce_follows_current_type() {
    assert_eq!(coerce_like(&json!(true), "off").unwrap(), json!(false));
    assert_eq!(coerce_like(&json!(3), "9").unwrap(), json!(9));
    assert_eq!(coerce_like(&json!(1.5), "2").unwrap(), json!(2.0));
    assert_eq!(coerce_like(&json!("a"), "9").unwrap(), json!("9"));
    assert_eq!(
        coerce_like(&json!(["x"]), r#"["y","z"]"#).unwrap(),
        json!(["y", "z"])
    );
    assert!(coerce_like(&json!(3), "seven").is_err());
    // 非有限浮点若放行会经 Value::from 静默变 null
    assert!(coerce_like(&json!(1.5), "nan").is_err());
    assert!(coerce_like(&json!(1.5), "inf").is_err());
    assert!(coerce_like(&json!(1.5), "1e400").is_err());
    assert!(coerce_like(&json!(["x"]), "{}").is_err());
    assert!(coerce_like(&Value::Null, "1").is_err());
}

fn fake_resolve(name: &str) -> Option<String> {
    match name {
        "USER_DATA" => Some(r"C:\Users\me\AppData\Roaming\WindInput".to_string()),
        "APP_DIR" => Some(r"C:\Program Files\WindInput".to_string()),
        _ => None,
    }
}

#[test]
fn expand_path_vars_replaces_known_and_preserves_literals() {
    assert_eq!(
        expand_with("no/vars/here.zip", fake_resolve).unwrap(),
        "no/vars/here.zip"
    );
    assert_eq!(
        expand_with(r"${USER_DATA}\bak\wind.zip", fake_resolve).unwrap(),
        r"C:\Users\me\AppData\Roaming\WindInput\bak\wind.zip"
    );
    // 多个变量与中间字面量都保留
    assert_eq!(
        expand_with("${APP_DIR}/x/${USER_DATA}/y", fake_resolve).unwrap(),
        r"C:\Program Files\WindInput/x/C:\Users\me\AppData\Roaming\WindInput/y"
    );
}

#[test]
fn expand_path_vars_rejects_unknown_and_unclosed() {
    // 未知变量报错，不静默留字面量（否则写到字面目录）
    assert!(expand_with("${NOPE}/x", fake_resolve).is_err());
    // 未闭合 ${ 报错
    assert!(expand_with("${USER_DATA/x", fake_resolve).is_err());
}

#[test]
fn format_value_unquotes_strings_only() {
    assert_eq!(format_value(&json!("vertical")), "vertical");
    assert_eq!(format_value(&json!(7)), "7");
    assert!(format_value(&json!({ "a": 1 })).contains('\n'));
}
