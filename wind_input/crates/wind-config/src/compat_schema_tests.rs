use super::*;

/// 抽出某个 `pub struct <name> {` 的字段名。取自源码而不是类型系统：Rust 没有字段反射，
/// 而「漏登记」正是本测试要防的。
fn struct_fields(src: &str, name: &str) -> Vec<String> {
    let start = src
        .find(&format!("pub struct {name} {{"))
        .unwrap_or_else(|| panic!("找不到结构体 {name}"));
    let body = &src[start..];
    let end = body.find("\n}").expect("找不到结构体结尾");
    body[..end]
        .lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("pub ")
                .and_then(|r| r.split_once(':'))
                .map(|(n, _)| n.trim().to_string())
        })
        .collect()
}

fn check(section: &str, struct_name: &str) {
    let src = include_str!("app_compat.rs");
    let mut fields = struct_fields(src, struct_name);
    fields.retain(|f| !crate::compat_overlay::META_KEYS.contains(&f.as_str()));
    assert!(!fields.is_empty(), "{struct_name} 字段表截获失灵");
    let registered = known_keys(section);
    for f in &fields {
        assert!(
            registered.contains(&f.as_str()),
            "{struct_name}.{f} 没登记进 COMPAT_FIELDS（新增兼容字段必须同时补元数据）"
        );
    }
    for k in &registered {
        assert!(
            fields.iter().any(|f| f == k),
            "COMPAT_FIELDS 登记了不存在的字段 {section}.{k}"
        );
    }
}

#[test]
fn every_apps_field_has_metadata() {
    check("apps", "AppCompatRule");
}

#[test]
fn every_scope_field_has_metadata() {
    check("initial_mode_scope", "InitialModeScopeRule");
}

#[test]
fn every_newline_field_has_metadata() {
    check("commit_newline", "CommitNewlineRule");
}

#[test]
fn every_field_explains_itself() {
    for f in COMPAT_FIELDS {
        assert!(
            !f.label.is_empty() && !f.summary.is_empty() && !f.problem.is_empty(),
            "{}.{} 的 label/summary/problem 不得为空",
            f.section,
            f.key
        );
    }
}

#[test]
fn field_specific_labels_override_and_tri_fields_carry_three_labels() {
    assert_eq!(
        option_label_for("status_position_mode", "fixed"),
        Some("固定（取当前位置）")
    );
    assert_eq!(
        option_label_for("candidate_position_mode", "fixed"),
        Some("固定位置")
    );
    let v = schema_json();
    for f in v["fields"].as_array().unwrap() {
        if f["kind"] == "tri_bool" {
            let l = f["triLabels"].as_array().expect("三态开关必须带 triLabels");
            assert_eq!(l.len(), 3, "{}", f["key"]);
        } else {
            assert!(f.get("triLabels").is_none(), "{}", f["key"]);
        }
    }
}

#[test]
fn every_enum_option_has_a_chinese_label() {
    for f in COMPAT_FIELDS {
        for o in f.options {
            let zh = option_label(o)
                .unwrap_or_else(|| panic!("{}.{} 的取值 {o} 没有登记中文名", f.section, f.key));
            assert!(!zh.is_ascii(), "{o} 的显示名 {zh:?} 里没有中文");
        }
    }
    let v = schema_json();
    let f = v["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "first_show_mode")
        .unwrap();
    assert_eq!(
        f["options"].as_array().unwrap().len(),
        f["optionLabels"].as_array().unwrap().len()
    );
}

#[test]
fn enum_fields_list_options_and_others_do_not() {
    for f in COMPAT_FIELDS {
        if f.kind == Kind::Enum {
            assert!(!f.options.is_empty(), "{}.{} 缺 options", f.section, f.key);
        } else {
            assert!(
                f.options.is_empty(),
                "{}.{} 不是枚举却带了 options",
                f.section,
                f.key
            );
        }
    }
}

#[test]
fn depends_on_points_at_a_real_enum_option() {
    for f in COMPAT_FIELDS {
        let Some(dep) = f.depends_on else { continue };
        let target = COMPAT_FIELDS
            .iter()
            .find(|t| t.section == f.section && t.key == dep.key)
            .unwrap_or_else(|| panic!("{}.depends_on 指向不存在的字段 {}", f.key, dep.key));
        assert!(
            target.options.contains(&dep.value),
            "{}.depends_on 的值 {} 不在 {} 的 options 里",
            f.key,
            dep.value,
            dep.key
        );
    }
}

#[test]
fn every_group_id_is_declared() {
    for f in COMPAT_FIELDS {
        assert!(
            GROUPS.iter().any(|(id, _)| *id == f.group),
            "{}.{} 的分组 {} 未在 GROUPS 里声明",
            f.section,
            f.key,
            f.group
        );
    }
}

#[test]
fn keys_are_unique_within_a_section() {
    let mut seen = std::collections::HashSet::new();
    for f in COMPAT_FIELDS {
        assert!(
            seen.insert((f.section, f.key)),
            "重复登记 {}.{}",
            f.section,
            f.key
        );
    }
}

#[test]
fn schema_json_has_groups_and_all_fields() {
    let v = schema_json();
    assert_eq!(v["fields"].as_array().unwrap().len(), COMPAT_FIELDS.len());
    assert_eq!(v["groups"].as_array().unwrap().len(), GROUPS.len());
    let first = &v["fields"][0];
    for k in [
        "section", "key", "kind", "group", "label", "summary", "problem", "options", "protocol",
    ] {
        assert!(first.get(k).is_some(), "字段元数据缺少键 {k}");
    }
}
