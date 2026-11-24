use super::*;

#[test]
fn exact_key_matches_only_itself() {
    let sel = SettingsSelector::parse("key:input.temp_english.enabled").unwrap();
    assert!(sel.matches_key("input.temp_english.enabled"));
    assert!(!sel.matches_key("input.temp_english.enabled_x"));
    assert!(!sel.matches_key("input.temp_english"));
}

#[test]
fn trailing_star_is_prefix_match() {
    let sel = SettingsSelector::parse("key:input.temp_english.*").unwrap();
    assert!(sel.matches_key("input.temp_english.enabled"));
    assert!(sel.matches_key("input.temp_english.trigger_keys"));
    assert!(!sel.matches_key("input.temp_pinyin.enabled"));
}

#[test]
fn dot_star_also_matches_the_stem_and_its_views() {
    let sel = SettingsSelector::parse("key:schema.mix_modes.*").unwrap();
    assert!(sel.matches_key("schema.mix_modes"));
    assert!(sel.matches_key("schema.mix_modes#members"));
    assert!(!sel.matches_key("schema.mix_modes_x"));
    assert!(!sel.matches_key("schema.mix"));
}

/// 精确 key 连带视图项，但不外溢到子键或同前缀的兄弟键。
#[test]
fn exact_key_also_matches_its_views() {
    let sel = SettingsSelector::parse("key:schema.mix_modes").unwrap();
    assert!(sel.matches_key("schema.mix_modes"));
    assert!(sel.matches_key("schema.mix_modes#members"));
    assert!(sel.matches_key("schema.mix_modes#candidate_layout"));
    assert!(!sel.matches_key("schema.mix_modes.members"));
    assert!(!sel.matches_key("schema.mix_modes_x"));
    assert!(!sel.matches_key("schema.mix"));
}

/// 精确写视图 key 只藏那一个视图，本体与别的视图不受牵连。
#[test]
fn exact_view_key_matches_only_that_view() {
    let sel = SettingsSelector::parse("key:schema.mix_modes#members").unwrap();
    assert!(sel.matches_key("schema.mix_modes#members"));
    assert!(!sel.matches_key("schema.mix_modes"));
    assert!(!sel.matches_key("schema.mix_modes#free_input"));
}

#[test]
fn whitespace_around_parts_is_tolerated() {
    let sel = SettingsSelector::parse("  key : ui.toolbar.* ").unwrap();
    assert!(sel.matches_key("ui.toolbar.visible"));
}

#[test]
fn known_feature_parses() {
    for id in SETTINGS_FEATURES {
        assert_eq!(
            SettingsSelector::parse(&format!("feature:{id}")).unwrap(),
            SettingsSelector::Feature((*id).into())
        );
    }
}

/// 报错要把合法值列全：定制者照着改，漏列一个就等于没告诉他有这一项。
#[test]
fn unknown_feature_error_lists_every_id() {
    let err = SettingsSelector::parse("feature:nope").unwrap_err();
    for id in SETTINGS_FEATURES {
        assert!(err.contains(id), "报错里缺 `{id}`：{err}");
    }
}

#[test]
fn malformed_selectors_are_rejected() {
    for bad in [
        "input.temp_english.*",
        "key:",
        "key:*",
        "key:input.*.enabled",
        "feature:nope",
        "page:stats",
    ] {
        assert!(SettingsSelector::parse(bad).is_err(), "{bad} 应被拒");
    }
}

#[test]
fn feature_never_matches_a_key() {
    let sel = SettingsSelector::parse("feature:wizard").unwrap();
    assert!(!sel.matches_key("wizard"));
}

/// 一条坏选择器不连累其余：设置程序那边宁可少藏一项，也不能整段失效。
#[test]
fn bad_entries_are_dropped_good_ones_kept() {
    let (hide, errors) = SettingsHide::parse(&["key:ui.toolbar.*", "bogus", "", "feature:wizard"]);
    assert_eq!(errors.len(), 1);
    assert!(hide.hides_key("ui.toolbar.visible"));
    assert!(hide.hides_feature("wizard"));
    assert!(!hide.hides_feature("other"));
}

/// 清单里的 `[settings]` 段落进 `CustomManifest::settings`（与 `[schemas]` 同形）。
#[test]
fn manifest_settings_section_deserializes() {
    let m: crate::CustomManifest =
        toml::from_str("[settings]\nhide = [\"feature:wizard\"]\n").unwrap();
    assert_eq!(m.settings.hide, vec!["feature:wizard".to_string()]);
    let (hide, errors) = SettingsHide::parse(&m.settings.hide);
    assert!(errors.is_empty());
    assert!(hide.hides_feature("wizard"));
}
