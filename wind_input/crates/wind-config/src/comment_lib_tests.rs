use super::*;

fn shipped() -> SystemComments {
    SystemComments::embedded()
}

fn code_hint_text() -> String {
    let s = shipped();
    s.templates
        .iter()
        .find(|t| t.id == "code")
        .unwrap()
        .text
        .clone()
}

fn global_default_of(sys: &SystemComments) -> String {
    let (lib, _) = CommentLibrary::build(sys, &UiCommentConfig::default());
    lib.template_text(&scope(None), CommentState::Default, false)
        .to_string()
}

#[test]
fn missing_system_comments_falls_back_to_embedded() {
    let (sys, w) = SystemComments::load_or_embedded(None);
    assert!(w.is_some());
    assert_eq!(global_default_of(&sys), code_hint_text());
}

#[test]
fn unparsable_system_comments_falls_back_to_embedded_with_warning() {
    let p = std::env::temp_dir().join(format!("sys_comments_bad_{}.toml", std::process::id()));
    std::fs::write(&p, "templates = [[[").unwrap();
    let (sys, w) = SystemComments::load_or_embedded(Some(&p));
    let _ = std::fs::remove_file(&p);
    assert!(w.unwrap().contains("内置副本"));
    assert_eq!(global_default_of(&sys), code_hint_text());
}

#[test]
fn on_disk_system_comments_overrides_embedded() {
    let p = std::env::temp_dir().join(format!("sys_comments_ok_{}.toml", std::process::id()));
    std::fs::write(
        &p,
        "[[templates]]\nid = \"x\"\ntext = \"X\"\n[assign.global]\ndefault_horizontal = \"x\"\n",
    )
    .unwrap();
    let (sys, w) = SystemComments::load_or_embedded(Some(&p));
    let _ = std::fs::remove_file(&p);
    assert!(w.is_none());
    assert_eq!(global_default_of(&sys), "X");
}

#[test]
fn reserved_user_template_ids_are_rejected() {
    let u = UiCommentConfig {
        templates: vec![tpl("none", "N"), tpl("inherit", "I")],
        ..Default::default()
    };
    let (lib, w) = CommentLibrary::build(&sys(global_only()), &u);
    assert_eq!(
        w.iter().filter(|w| w.contains("保留字")).count(),
        2,
        "{w:?}"
    );
    assert!(
        lib.templates()
            .all(|(t, _)| t.id != "none" && t.id != "inherit")
    );
}

#[test]
fn system_comments_parses_shipped_file() {
    let s = shipped();
    let ids: Vec<&str> = s.templates.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "code",
            "code_hint",
            "code_rev",
            "shuangpin",
            "pinyin",
            "chaizi",
            "chaizi_code",
            "chaizi_pinyin",
            "zh_en",
            "en_zh",
            "pinyin_en",
            "full",
        ],
        "设计 §13.6 的内置模板与顺序"
    );
    assert_eq!(s.assign.global.default_horizontal.as_deref(), Some("code"));
    assert_eq!(
        s.assign.global.detail_horizontal.as_deref(),
        Some("pinyin_en")
    );
    let dicts: Vec<&str> = s.dicts.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(dicts, ["en_zh", "zh_en"]);
    assert_eq!(s.dicts[0].path, "comments/en_zh.txt");
    assert_eq!(s.dicts[1].path, "comments/zh_en.txt");
}

#[test]
fn shipped_assign_matches_design_table() {
    let a = shipped().assign;
    let cell = |m: &BTreeMap<String, CommentCells>, k: &str| {
        let c = &m[k];
        (c.default_horizontal.clone(), c.detail_horizontal.clone())
    };
    let s = |v: &str| Some(v.to_string());
    // 设计 §13.6 出厂分配表。
    assert_eq!(cell(&a.by_type, "codetable"), (None, s("chaizi_pinyin")));
    assert_eq!(cell(&a.by_type, "pinyin"), (s("none"), None));
    assert_eq!(cell(&a.by_type, "shuangpin"), (s("none"), None));
    assert_eq!(
        cell(&a.by_type, "mixed"),
        (s("code_rev"), s("chaizi_pinyin"))
    );
    assert_eq!(cell(&a.by_type, "english"), (s("none"), s("en_zh")));
    assert_eq!(cell(&a.mode, "temp_english"), (s("none"), s("en_zh")));
    assert_eq!(cell(&a.mode, "temp_pinyin"), (s("code"), s("chaizi_code")));
    assert_eq!(cell(&a.mode, MIX_ALL_MODE), (s("none"), s("none")));
    let all = std::iter::once(&a.global)
        .chain(a.by_type.values())
        .chain(a.schema.values())
        .chain(a.mode.values());
    for c in all {
        assert!(
            c.default_vertical.is_none() && c.detail_vertical.is_none(),
            "出厂只写横排格: {c:?}"
        );
    }
}

#[test]
fn code_template_matches_legacy_default() {
    let s = shipped();
    let t = s.templates.iter().find(|t| t.id == "code").unwrap();
    assert_eq!(t.text, "${code_hint|code_rev|shuangpin}");
}

#[test]
fn template_ids_are_valid() {
    let s = shipped();
    let mut seen = std::collections::HashSet::new();
    for t in &s.templates {
        assert!(
            !t.id.is_empty()
                && t.id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "模板 id 不合法: {:?}",
            t.id
        );
        assert!(seen.insert(t.id.clone()), "模板 id 重复: {}", t.id);
    }
}

#[test]
fn ui_comment_defaults() {
    let c = UiCommentConfig::default();
    assert!(c.enabled);
    assert_eq!(c.state, CommentState::Default);
    assert_eq!(c.switch_key, SwitchKey::F1);
    assert_eq!(c.switch_mode, SwitchMode::Session);
    assert!(c.templates.is_empty());
    assert_eq!(c.assign, CommentAssign::default());
}

#[test]
fn switch_key_parses_all_values_and_matches_vk() {
    for (name, key, vks) in [
        ("none", SwitchKey::None, vec![]),
        ("f1", SwitchKey::F1, vec![]),
        ("f12", SwitchKey::F12, vec![]),
        ("ctrl", SwitchKey::Ctrl, vec![0xA2, 0xA3]),
        ("lctrl", SwitchKey::LCtrl, vec![0xA2]),
        ("rctrl", SwitchKey::RCtrl, vec![0xA3]),
        ("alt", SwitchKey::Alt, vec![0xA4, 0xA5]),
        ("lalt", SwitchKey::LAlt, vec![0xA4]),
        ("ralt", SwitchKey::RAlt, vec![0xA5]),
    ] {
        let c: UiCommentConfig = toml::from_str(&format!("switch_key = \"{name}\"")).unwrap();
        assert_eq!(c.switch_key, key, "{name}");
        for vk in 0xA2..=0xA5u32 {
            assert_eq!(key.matches_vk(vk), vks.contains(&vk), "{name} {vk:#x}");
        }
        assert_eq!(key.is_alt(), name.ends_with("alt"));
        assert_eq!(key.is_ctrl(), name.ends_with("ctrl"));
    }
    assert!(!SwitchKey::Ctrl.matches_vk(0x41));
}

#[test]
fn function_keys_name_their_session_key_and_off_is_not_a_config_value() {
    assert_eq!(SwitchKey::F1.function_key_name(), Some("f1"));
    assert_eq!(SwitchKey::F12.function_key_name(), Some("f12"));
    assert_eq!(SwitchKey::Ctrl.function_key_name(), None);
    assert_eq!(SwitchKey::None.function_key_name(), None);
    assert_eq!(SwitchKey::F1.function_vk(), Some(0x70));
    assert_eq!(SwitchKey::F12.function_vk(), Some(0x7B));
    assert_eq!(SwitchKey::Alt.function_vk(), None);
    let c: UiCommentConfig = toml::from_str("switch_mode = \"off\"").unwrap();
    assert_eq!(
        c.switch_mode,
        SwitchMode::Session,
        "off 不再是配置值，按出厂处理"
    );
}

#[test]
fn invalid_switch_values_fall_back_to_defaults() {
    let c: UiCommentConfig =
        toml::from_str("switch_key = \"bogus\"\nswitch_mode = \"nope\"").unwrap();
    assert_eq!(c.switch_key, SwitchKey::F1);
    assert_eq!(c.switch_mode, SwitchMode::Session);
}

#[test]
fn user_toml_round_trips_with_type_key() {
    let c: UiCommentConfig = toml::from_str(
        "state = \"detail\"\nswitch_key = \"ralt\"\nswitch_mode = \"hold\"\n[assign.type.codetable]\ndetail_vertical = \"x\"\n[assign.mode.\"mix:q\"]\ndefault_vertical = \"none\"\n",
    )
    .unwrap();
    assert_eq!(c.state, CommentState::Detail);
    assert_eq!(c.switch_key, SwitchKey::RAlt);
    assert_eq!(c.switch_mode, SwitchMode::Hold);
    assert_eq!(
        c.assign.by_type["codetable"].detail_vertical.as_deref(),
        Some("x")
    );
    assert_eq!(
        c.assign.mode["mix:q"].default_vertical.as_deref(),
        Some("none")
    );
}

// ---- 解析链 ----

fn cells(dv: Option<&str>, dh: Option<&str>, tv: Option<&str>, th: Option<&str>) -> CommentCells {
    let o = |s: Option<&str>| s.map(str::to_string);
    CommentCells {
        default_vertical: o(dv),
        default_horizontal: o(dh),
        detail_vertical: o(tv),
        detail_horizontal: o(th),
    }
}

fn tpl(id: &str, text: &str) -> CommentTemplateDef {
    CommentTemplateDef {
        id: id.into(),
        name: id.into(),
        text: text.into(),
        ..Default::default()
    }
}

/// 内置模板：code / full / a / b / c / d，文本为 `T:<id>`。
fn sys(assign: CommentAssign) -> SystemComments {
    SystemComments {
        templates: ["code", "full", "a", "b", "c", "d"]
            .iter()
            .map(|id| tpl(id, &format!("T:{id}")))
            .collect(),
        assign,
        dicts: Vec::new(),
    }
}

fn scope<'a>(mode: Option<&'a str>) -> CommentScope<'a> {
    CommentScope {
        mode,
        schema: "wubi",
        schema_type: SchemaType::CodeTable,
    }
}

fn text(lib: &CommentLibrary, mode: Option<&str>, st: CommentState, v: bool) -> String {
    lib.template_text(&scope(mode), st, v).to_string()
}

fn global_only() -> CommentAssign {
    CommentAssign {
        global: cells(None, Some("code"), None, Some("full")),
        ..Default::default()
    }
}

#[test]
fn global_only_resolves_both_states_and_layouts() {
    let (lib, w) = CommentLibrary::build(&sys(global_only()), &UiCommentConfig::default());
    assert!(w.is_empty());
    for v in [true, false] {
        assert_eq!(text(&lib, None, CommentState::Default, v), "T:code");
        assert_eq!(text(&lib, None, CommentState::Detail, v), "T:full");
    }
}

#[test]
fn mode_beats_schema_beats_type_beats_global() {
    let mk = |id: &str| cells(None, None, None, Some(id));
    let mut a = global_only();
    a.global = mk("a");
    a.by_type.insert("codetable".into(), mk("b"));
    a.schema.insert("wubi".into(), mk("c"));
    a.mode.insert("url".into(), mk("d"));
    let u = UiCommentConfig::default();
    let check = |a: &CommentAssign, want: &str| {
        let (lib, _) = CommentLibrary::build(&sys(a.clone()), &u);
        for v in [false, true] {
            assert_eq!(text(&lib, Some("url"), CommentState::Detail, v), want);
        }
    };
    check(&a, "T:d");
    a.mode.clear();
    check(&a, "T:c");
    a.schema.clear();
    check(&a, "T:b");
    a.by_type.clear();
    check(&a, "T:a");
}

#[test]
fn none_is_an_opinion() {
    let mut a = global_only();
    a.mode
        .insert("url".into(), cells(None, Some("none"), None, None));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    for v in [false, true] {
        assert_eq!(text(&lib, Some("url"), CommentState::Default, v), "");
        assert_eq!(text(&lib, None, CommentState::Default, v), "T:code");
    }
}

#[test]
fn user_inherit_skips_system_cell() {
    let mut a = global_only();
    a.mode
        .insert("temp_english".into(), cells(None, Some("none"), None, None));
    let mut u = UiCommentConfig::default();
    u.assign.mode.insert(
        "temp_english".into(),
        cells(None, Some("inherit"), None, None),
    );
    let (lib, _) = CommentLibrary::build(&sys(a), &u);
    for v in [false, true] {
        assert_eq!(
            text(&lib, Some("temp_english"), CommentState::Default, v),
            "T:code"
        );
    }
}

#[test]
fn vertical_same_as_horizontal_when_unset() {
    let mut a = global_only();
    a.global.detail_vertical = Some("b".into());
    a.schema
        .insert("wubi".into(), cells(None, None, None, Some("a")));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    assert_eq!(text(&lib, None, CommentState::Detail, true), "T:a");
    assert_eq!(text(&lib, None, CommentState::Detail, false), "T:a");
}

#[test]
fn vertical_same_as_horizontal_none() {
    let mut a = global_only();
    a.schema
        .insert("wubi".into(), cells(None, Some("none"), None, None));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    assert_eq!(text(&lib, None, CommentState::Default, true), "");
}

#[test]
fn vertical_inherit_goes_down() {
    let mut a = global_only();
    a.global.detail_vertical = Some("b".into());
    a.schema
        .insert("wubi".into(), cells(None, None, Some("inherit"), Some("a")));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    assert_eq!(text(&lib, None, CommentState::Detail, true), "T:b");
    assert_eq!(text(&lib, None, CommentState::Detail, false), "T:a");
}

#[test]
fn horizontal_inherit_goes_down_and_vertical_follows_next_layer() {
    let mut a = global_only();
    a.global.detail_vertical = Some("b".into());
    a.schema
        .insert("wubi".into(), cells(None, None, None, Some("inherit")));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    assert_eq!(text(&lib, None, CommentState::Detail, false), "T:full");
    assert_eq!(text(&lib, None, CommentState::Detail, true), "T:b");
}

#[test]
fn vertical_written_does_not_affect_horizontal() {
    let mut a = global_only();
    a.schema
        .insert("wubi".into(), cells(None, None, Some("a"), None));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    assert_eq!(text(&lib, None, CommentState::Detail, true), "T:a");
    assert_eq!(text(&lib, None, CommentState::Detail, false), "T:full");
}

#[test]
fn vertical_unset_and_horizontal_unset_follows_down() {
    let mut a = global_only();
    a.global.detail_vertical = Some("b".into());
    a.schema
        .insert("wubi".into(), cells(None, None, None, None));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    assert_eq!(text(&lib, None, CommentState::Detail, true), "T:b");
    assert_eq!(text(&lib, None, CommentState::Detail, false), "T:full");
}

#[test]
fn vertical_unset_with_horizontal_unset_applies_rule_at_next_layer() {
    // 方案层两格都不写 → 看类型层：类型层竖排也不写、横排有值 → 同类型层横排。
    let mut a = global_only();
    a.global.detail_vertical = Some("b".into());
    a.by_type
        .insert("codetable".into(), cells(None, None, None, Some("c")));
    a.schema
        .insert("wubi".into(), cells(None, None, None, None));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    assert_eq!(text(&lib, None, CommentState::Detail, true), "T:c");
    assert_eq!(text(&lib, None, CommentState::Detail, false), "T:c");
}

#[test]
fn user_inherit_on_vertical_skips_row_horizontal() {
    let mut a = global_only();
    a.global.detail_vertical = Some("b".into());
    a.by_type
        .insert("codetable".into(), cells(None, None, None, Some("c")));
    let mut u = UiCommentConfig::default();
    u.assign
        .by_type
        .insert("codetable".into(), cells(None, None, Some("inherit"), None));
    let (lib, _) = CommentLibrary::build(&sys(a), &u);
    assert_eq!(text(&lib, None, CommentState::Detail, true), "T:b");
    assert_eq!(text(&lib, None, CommentState::Detail, false), "T:c");
}

#[test]
fn unknown_id_follows_and_warns_once() {
    let mut a = global_only();
    a.schema
        .insert("wubi".into(), cells(None, Some("gone"), None, Some("gone")));
    let (lib, w) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("gone"));
    assert_eq!(text(&lib, None, CommentState::Default, true), "T:code");
    assert_eq!(text(&lib, None, CommentState::Detail, true), "T:full");
}

#[test]
fn user_template_overrides_builtin() {
    let mut u = UiCommentConfig::default();
    u.templates.push(tpl("code", "X"));
    let (lib, w) = CommentLibrary::build(&sys(global_only()), &u);
    assert!(w.is_empty());
    assert_eq!(text(&lib, None, CommentState::Default, true), "X");
    let origin = lib
        .templates()
        .find(|(t, _)| t.id == "code")
        .map(|(_, o)| o);
    assert_eq!(origin, Some(TemplateOrigin::UserOverridesBuiltin));
}

#[test]
fn fallback_when_global_empty() {
    let (lib, _) =
        CommentLibrary::build(&sys(CommentAssign::default()), &UiCommentConfig::default());
    assert_eq!(text(&lib, None, CommentState::Default, true), "T:code");
    assert_eq!(text(&lib, None, CommentState::Detail, false), "T:full");
}

#[test]
fn referenced_texts_covers_detail() {
    let (lib, _) = CommentLibrary::build(&sys(global_only()), &UiCommentConfig::default());
    assert!(lib.referenced_texts().contains(&"T:full"));
    assert!(lib.referenced_texts().contains(&"T:code"));
}

#[test]
fn invalid_user_template_id_skipped_and_warned() {
    let mut u = UiCommentConfig::default();
    u.templates.push(tpl("Bad-Id", "x"));
    u.templates.push(tpl("", "x"));
    u.templates.push(tpl("mine", "first"));
    u.templates.push(tpl("mine", "second"));
    let (lib, w) = CommentLibrary::build(&sys(global_only()), &u);
    assert_eq!(w.len(), 3, "{w:?}");
    let ids: Vec<&str> = lib.templates().map(|(t, _)| t.id.as_str()).collect();
    assert!(!ids.contains(&"Bad-Id") && !ids.contains(&""));
    let mine = lib.templates().find(|(t, _)| t.id == "mine").unwrap();
    assert_eq!(
        (mine.0.text.as_str(), mine.1),
        ("first", TemplateOrigin::User)
    );
}

#[test]
fn resolve_cell_reports_winner_and_layer() {
    let mut a = global_only();
    a.by_type
        .insert("codetable".into(), cells(None, None, None, Some("b")));
    a.mode
        .insert("url".into(), cells(None, Some("none"), None, None));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    let sc = scope(Some("url"));
    assert_eq!(
        lib.resolve_cell(&sc, AssignLayer::Mode, CommentState::Default, true),
        (None, AssignLayer::Mode)
    );
    assert_eq!(
        lib.resolve_cell(&sc, AssignLayer::Schema, CommentState::Default, true),
        (Some("code"), AssignLayer::Global)
    );
    assert_eq!(
        lib.resolve_cell(&sc, AssignLayer::Mode, CommentState::Detail, false),
        (Some("b"), AssignLayer::Type)
    );
    assert_eq!(
        lib.resolve_cell(&sc, AssignLayer::Global, CommentState::Detail, true),
        (Some("full"), AssignLayer::Global)
    );
}

/// 快捷输入自己没写的格落到通配 `mode.mix`，写了的覆盖它；别的模式不看通配格。
#[test]
fn mix_modes_fall_back_to_mix_all_cell() {
    let mut a = global_only();
    a.mode
        .insert(MIX_ALL_MODE.into(), cells(None, Some("b"), None, None));
    a.mode
        .insert("mix:own".into(), cells(None, None, None, Some("none")));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    for m in ["mix:own", "mix:other"] {
        assert_eq!(
            lib.resolve_cell(
                &scope(Some(m)),
                AssignLayer::Mode,
                CommentState::Default,
                false
            ),
            (Some("b"), AssignLayer::Mode),
            "{m}"
        );
    }
    assert_eq!(
        lib.resolve_cell(
            &scope(Some("mix:own")),
            AssignLayer::Mode,
            CommentState::Detail,
            false
        ),
        (None, AssignLayer::Mode)
    );
    assert_eq!(
        lib.resolve_cell(
            &scope(Some("url")),
            AssignLayer::Mode,
            CommentState::Default,
            false
        ),
        (Some("code"), AssignLayer::Global)
    );
}

#[test]
fn resolve_cell_falls_back_to_builtin_ids() {
    let (lib, _) =
        CommentLibrary::build(&sys(CommentAssign::default()), &UiCommentConfig::default());
    assert_eq!(
        lib.resolve_cell(&scope(None), AssignLayer::Type, CommentState::Detail, false),
        (Some("full"), AssignLayer::Global)
    );
}

#[test]
fn effective_table_lists_fixed_and_assigned_rows() {
    let mut a = global_only();
    a.schema
        .insert("wubi".into(), cells(None, None, None, Some("a")));
    a.mode
        .insert("url".into(), cells(None, Some("none"), None, None));
    let (lib, _) = CommentLibrary::build(&sys(a), &UiCommentConfig::default());
    let t = lib.effective_table("wubi", |_| SchemaType::CodeTable);
    assert_eq!(t.len(), (1 + 5 + 1 + 1) * 4);
    let find = |scope: &str, st: CommentState, v: bool| {
        t.iter()
            .find(|c| c.scope == scope && c.state == st && c.vertical == v)
            .unwrap()
            .clone()
    };
    let c = find("schema:wubi", CommentState::Detail, false);
    assert_eq!(
        (c.template_id.as_deref(), c.from_scope.as_str()),
        (Some("a"), "schema:wubi")
    );
    let c = find("mode:url", CommentState::Detail, true);
    assert_eq!(
        (c.template_id.as_deref(), c.from_scope.as_str()),
        (Some("a"), "schema:wubi"),
        "模式行的下层取当前方案"
    );
    let c = find("mode:url", CommentState::Default, true);
    assert_eq!((c.template_id, c.from_scope.as_str()), (None, "mode:url"));
}

/// 出厂分配在两种排布下的实际结果（横排为主改写前后一致）。
#[test]
fn shipped_effective_table_snapshot() {
    let (lib, w) = CommentLibrary::build(&shipped(), &UiCommentConfig::default());
    assert!(w.is_empty(), "{w:?}");
    let t = lib.effective_table("wubi86", |_| SchemaType::CodeTable);
    let got = |scope: &str, st: CommentState| {
        let pick = |v: bool| {
            t.iter()
                .find(|c| c.scope == scope && c.state == st && c.vertical == v)
                .map(|c| (c.template_id.clone(), c.from_scope.clone()))
                .unwrap()
        };
        let (h, v) = (pick(false), pick(true));
        assert_eq!(h, v, "{scope} {st:?}: 竖排应同横排");
        (h.0.unwrap_or_else(|| "none".into()), h.1)
    };
    use CommentState::{Default as D, Detail as T};
    for (scope, st, id, from) in [
        ("global", D, "code", "global"),
        ("global", T, "pinyin_en", "global"),
        ("type:codetable", D, "code", "global"),
        ("type:codetable", T, "chaizi_pinyin", "type:codetable"),
        ("type:pinyin", D, "none", "type:pinyin"),
        ("type:pinyin", T, "pinyin_en", "global"),
        ("type:shuangpin", T, "pinyin_en", "global"),
        ("type:mixed", D, "code_rev", "type:mixed"),
        ("type:mixed", T, "chaizi_pinyin", "type:mixed"),
        ("type:english", D, "none", "type:english"),
        ("type:english", T, "en_zh", "type:english"),
        ("mode:temp_english", D, "none", "mode:temp_english"),
        ("mode:temp_english", T, "en_zh", "mode:temp_english"),
        ("mode:temp_pinyin", D, "code", "mode:temp_pinyin"),
        ("mode:temp_pinyin", T, "chaizi_code", "mode:temp_pinyin"),
        ("mode:mix", D, "none", "mode:mix"),
        ("mode:mix", T, "none", "mode:mix"),
    ] {
        assert_eq!(
            got(scope, st),
            (id.to_string(), from.to_string()),
            "{scope} {st:?}"
        );
    }
}
