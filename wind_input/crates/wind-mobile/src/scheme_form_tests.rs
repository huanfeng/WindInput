use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use toml::Value;
use wind_config::Config;
use wind_coordinator::Coordinator;

use super::*;
use crate::config_api::ConfigValue;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    if data_dir().join("schemas/wubi86.schema.toml").exists() {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        eprintln!("跳过：缺少出厂数据（CI 环境）");
        return false;
    }
    panic!("缺少出厂数据 {}，本地不允许跳过", data_dir().display());
}

/// `root` 下独立的 redb 与覆盖目录。`stroke` 不在 available 里，充当「停用的方案」。
fn core_at(root: &Path) -> MobileCore {
    let store = Arc::new(wind_store::Store::open(root.join("s.redb")).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = ["wubi86", "pinyin", "shuangpin", "wubi86_pinyin", "english"]
        .map(String::from)
        .to_vec();
    cfg.schema.active = "pinyin".into();
    let coord = Coordinator::new_headless_with_store_override(
        cfg,
        Some(&data_dir()),
        store,
        Some(root.join("overrides")),
    );
    MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: data_dir(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    }
}

fn root(tag: &str) -> PathBuf {
    let r = std::env::temp_dir().join(format!("wind_mobile_sf_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&r);
    std::fs::create_dir_all(&r).unwrap();
    r
}

fn table(text: &str) -> toml::Table {
    toml::from_str(text).unwrap()
}

const COUNT: &str = "schema.pinyin.sentence_count";

#[test]
fn pinyin_and_shuangpin_are_independent() {
    if !has_data() {
        return;
    }
    let r = root("indep");
    let m = core_at(&r);
    let before = table(&m.scheme_form("pinyin").unwrap().effective)[COUNT].clone();
    m.scheme_form_apply(
        "shuangpin",
        vec![(COUNT.into(), ConfigValue::Int(5))],
        vec![],
    )
    .unwrap();
    let sp = table(&m.scheme_form("shuangpin").unwrap().effective);
    assert_eq!(sp[COUNT], Value::Integer(5));
    let py = table(&m.scheme_form("pinyin").unwrap().effective);
    assert_eq!(py[COUNT], before, "改双拼不能影响全拼");
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn reset_returns_to_scheme_default_and_keeps_other_overrides() {
    if !has_data() {
        return;
    }
    let r = root("reset");
    let m = core_at(&r);
    m.set_schema_setting("pinyin", "phrases.enabled", Some("false"))
        .unwrap();
    m.scheme_form_apply("pinyin", vec![(COUNT.into(), ConfigValue::Int(5))], vec![])
        .unwrap();
    m.scheme_form_apply("pinyin", vec![], vec![COUNT.into()])
        .unwrap();
    let f = m.scheme_form("pinyin").unwrap();
    assert_eq!(table(&f.effective)[COUNT], table(&f.default)[COUNT]);
    let phrases = m
        .schema_settings("pinyin")
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.key == "phrases.enabled")
        .unwrap();
    assert!(phrases.overridden, "表单恢复不能动覆盖层里别的内容");
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn builtin_lists_scheme_presets() {
    if !has_data() {
        return;
    }
    let r = root("builtin");
    let m = core_at(&r);
    let f = m.scheme_form("shuangpin").unwrap();
    let key = "schema.pinyin.english_merge.exact_position";
    assert_eq!(table(&f.effective)[key], Value::Integer(2));
    assert_eq!(table(&f.builtin)[key], Value::Integer(1));
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn disabled_scheme_still_readable_and_writable() {
    if !has_data() {
        return;
    }
    let r = root("disabled");
    let m = core_at(&r);
    let key = "schema.codetable.top_code_commit";
    let f = m.scheme_form("stroke").unwrap();
    assert!(table(&f.effective).contains_key(key));
    m.scheme_form_apply(
        "stroke",
        vec![(key.into(), ConfigValue::Bool(false))],
        vec![],
    )
    .unwrap();
    assert_eq!(
        table(&m.scheme_form("stroke").unwrap().effective)[key],
        Value::Boolean(false)
    );
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn broken_override_file_refuses_write() {
    if !has_data() {
        return;
    }
    let r = root("broken");
    let m = core_at(&r);
    let path = r.join("overrides").join("pinyin.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "this is = = not toml").unwrap();
    assert!(
        m.scheme_form_apply("pinyin", vec![(COUNT.into(), ConfigValue::Int(5))], vec![])
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "this is = = not toml"
    );
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn rejects_wrong_kind_key_and_bad_value() {
    if !has_data() {
        return;
    }
    let r = root("reject");
    let m = core_at(&r);
    assert!(
        m.scheme_form_apply(
            "pinyin",
            vec![(
                "schema.codetable.top_code_commit".into(),
                ConfigValue::Bool(true)
            )],
            vec![]
        )
        .is_err()
    );
    assert!(
        m.scheme_form_apply(
            "pinyin",
            vec![(COUNT.into(), ConfigValue::Str("x".into()))],
            vec![]
        )
        .is_err()
    );
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn mixed_scheme_names_its_members() {
    if !has_data() {
        return;
    }
    let r = root("members");
    let m = core_at(&r);
    let f = m.scheme_form("wubi86_pinyin").unwrap();
    assert_eq!(f.members, vec!["wubi86".to_string(), "pinyin".to_string()]);
    assert!(m.scheme_form("pinyin").unwrap().members.is_empty());
    let _ = std::fs::remove_dir_all(&r);
}
