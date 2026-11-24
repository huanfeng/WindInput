use super::*;

fn repo_data() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../data"))
}

#[test]
fn every_registry_key_present_with_type() {
    let caps = generate(None).expect("生成 capability");
    let arr = caps["configKeys"].as_array().expect("configKeys 应为数组");
    // 每个 entry 的 type 非空、default 非 null：守护「default 无空洞」——
    // 若将来某 Map 配了非空默认值，leaf_entries 会下钻导致该键 default 静默变 null，此处拦截。
    for e in arr {
        let key = e["key"].as_str().expect("key 应为字符串");
        assert!(
            e["type"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
            "{key}: type 应为非空字符串"
        );
        assert!(!e["default"].is_null(), "{key}: default 不应为 null");
    }
    let keys: std::collections::BTreeSet<&str> =
        arr.iter().map(|e| e["key"].as_str().unwrap()).collect();
    for f in config_schema::registry() {
        assert!(
            keys.contains(f.key),
            "capability 缺 registry key: {}",
            f.key
        );
    }
    assert_eq!(
        keys.len(),
        config_schema::registry().len(),
        "capability 键数应与 registry 一致"
    );
}

#[test]
fn enum_fields_carry_values() {
    let caps = generate(None).expect("生成 capability");
    let entry = caps["configKeys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "ui.candidate.layout")
        .expect("应含 ui.candidate.layout");
    assert_eq!(entry["type"], "enum");
    assert_eq!(
        entry["values"],
        serde_json::json!(["horizontal", "vertical"])
    );
}

/// 键名受限的 Map 也带 `values`——设置端靠它预填字体类别，缺了就只能手抄一份。
///
/// ★ 同时断言**自由命名的 Map 不带** `values`：两者若都带，设置端无从区分
/// 「类别固定」与「用户自由命名」，会给自定义标点表也铺一堆预填行。
#[test]
fn keyed_map_carries_key_domain_and_open_map_does_not() {
    let caps = generate(None).expect("生成 capability");
    let arr = caps["configKeys"].as_array().unwrap();
    let find = |k: &str| {
        arr.iter()
            .find(|e| e["key"] == k)
            .expect("键应存在")
            .clone()
    };

    let scripts = find("ui.font.scripts");
    assert_eq!(scripts["type"], "map");
    assert_eq!(
        scripts["values"],
        serde_json::json!([
            "latin", "greek", "cyrillic", "cjk", "emoji", "digits", "punct"
        ])
    );

    let punct = find("input.punct.custom_mappings");
    assert_eq!(punct["type"], "map");
    assert!(punct.get("values").is_none(), "自由命名的 Map 不该带键名域");
}

#[test]
fn defaults_reflect_preset_overrides() {
    let caps = generate(Some(repo_data())).expect("生成 capability");
    let arr = caps["configKeys"].as_array().unwrap();
    let find = |k: &str| {
        arr.iter()
            .find(|e| e["key"] == k)
            .unwrap_or_else(|| panic!("缺 key {k}"))
            .clone()
    };
    // L2 预置覆盖（code default 是空串，data/config.toml 覆盖成 wubi86）
    assert_eq!(
        find("schema.active")["default"],
        serde_json::json!("wubi86")
    );
    // 配对跳出键默认只含右符号：设置界面的默认勾选态直接取自这里，漂了就会与内核不一致。
    assert_eq!(
        find("input.auto_pair.jump_out_keys")["default"],
        serde_json::json!(["right_symbol"])
    );
    // 普通默认值
    assert_eq!(
        find("ui.candidate.per_page")["default"],
        serde_json::json!(7)
    );
}
