//! 方案设置按方案取值：全拼与双拼各设一份互不影响；混输、英文、码表自动造词同理。
//! 设计见 `docs/design/scheme-settings-flatten.md`。只读方案定义，用仓库自带的 `data/` 即可。

use std::path::PathBuf;

use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data")
}

fn mgr(tag: &str, active: &str, overrides: &[(&str, &str)]) -> EngineManager {
    let root = std::env::temp_dir().join(format!("wind_pinyin_scheme_settings_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let ov = root.join("schema_overrides");
    std::fs::create_dir_all(&ov).unwrap();
    for (id, text) in overrides {
        std::fs::write(ov.join(format!("{id}.toml")), text).unwrap();
    }
    let mut cfg = Config::default();
    cfg.schema.available = [
        "pinyin",
        "shuangpin",
        "wubi86",
        "wubi86_pinyin",
        "stroke",
        "english",
    ]
    .map(String::from)
    .to_vec();
    cfg.schema.active = active.to_string();
    cfg.schema.pinyin.frequency.enabled = true;
    EngineManager::with_store_override(&cfg, Some(&data_dir()), None, Some(ov))
}

#[test]
fn full_and_shuangpin_hold_separate_values() {
    let m = mgr(
        "separate",
        "pinyin",
        &[(
            "shuangpin",
            "[engine.pinyin]\nsentence_count = 3\nsingle_char = true\n\
             [engine.pinyin.frequency]\nenabled = false\n",
        )],
    );
    let full = m.pinyin_settings_of("pinyin");
    let sp = m.pinyin_settings_of("shuangpin");
    assert_eq!(full.sentence_count, 1);
    assert!(!full.single_char);
    assert!(full.frequency.enabled);
    assert_eq!(sp.sentence_count, 3);
    assert!(sp.single_char);
    assert!(!sp.frequency.enabled);
    // 调频走的是另一条取值路径，同样要按方案分。
    assert!(m.freq_settings_for("pinyin").enabled);
    assert!(!m.freq_settings_for("shuangpin").enabled);
}

/// 「此刻起作用的拼音方案」：拼音方案是自己，混输是它的拼音成员，码表是主拼音方案。
#[test]
fn scheme_in_effect_follows_active_scheme() {
    let ov = [("shuangpin", "[engine.pinyin]\nsentence_count = 4\n")];
    assert_eq!(
        mgr("eff_sp", "shuangpin", &ov)
            .pinyin_settings()
            .sentence_count,
        4
    );
    assert_eq!(
        mgr("eff_py", "pinyin", &ov)
            .pinyin_settings()
            .sentence_count,
        1
    );
    assert_eq!(
        mgr("eff_mix", "wubi86_pinyin", &ov).pinyin_scheme_in_effect(),
        "pinyin"
    );
    assert_eq!(
        mgr("eff_ct", "wubi86", &ov).pinyin_scheme_in_effect(),
        "pinyin"
    );
}

/// 方案失效后重新读盘：设置页保存方案设置走的就是这条路。
#[test]
fn invalidate_picks_up_new_scheme_settings() {
    let m = mgr("invalidate", "pinyin", &[]);
    assert_eq!(m.pinyin_settings_of("pinyin").sentence_count, 1);
    let ov = std::env::temp_dir().join("wind_pinyin_scheme_settings_invalidate/schema_overrides");
    std::fs::write(
        ov.join("pinyin.toml"),
        "[engine.pinyin]\nsentence_count = 2\n",
    )
    .unwrap();
    m.invalidate_schema("pinyin");
    assert_eq!(m.pinyin_settings_of("pinyin").sentence_count, 2);
}

#[test]
fn mixed_english_and_auto_phrase_settings_come_from_their_scheme() {
    let m = mgr(
        "others",
        "wubi86",
        &[
            (
                "wubi86_pinyin",
                "[engine.mixed]\nmin_pinyin_length = 5\nenable_english = true\n",
            ),
            (
                "english",
                "[engine.english]\ncommit_space = true\n[engine.english.frequency]\nenabled = true\n",
            ),
            (
                "wubi86",
                "[engine.codetable.auto_phrase]\nenabled = true\nmax_phrase_len = 3\n",
            ),
        ],
    );
    let mix = m.mix_settings_of("wubi86_pinyin");
    assert_eq!(mix.min_pinyin_length, 5);
    assert!(mix.enable_english);
    // 方案文件里的成员声明不受设置项影响。
    assert_eq!(
        m.mixed_secondary_schema("wubi86_pinyin").as_deref(),
        Some("pinyin")
    );

    let en = m.english_settings();
    assert!(en.commit_space);
    assert!(m.freq_settings_for("english").enabled);

    let ap = m.codetable_settings().auto_phrase;
    assert!(ap.enabled);
    assert_eq!(ap.max_phrase_len, 3);
    // 没写的项保持基线。
    assert_eq!(
        ap.min_phrase_len,
        Config::default()
            .schema
            .codetable
            .auto_phrase
            .min_phrase_len
    );
    // 笔画方案没写，不受五笔的设置影响。
    assert!(!m.effective_codetable("stroke").auto_phrase.enabled);
}

// ── 旧全局键别名 ──

use wind_config::config_schema::SchemeKind;

fn override_text(tag: &str, id: &str) -> String {
    let p = std::env::temp_dir().join(format!(
        "wind_pinyin_scheme_settings_{tag}/schema_overrides/{id}.toml"
    ));
    std::fs::read_to_string(p).unwrap_or_default()
}

/// 别名指向当前方案；当前方案不属于该类型时指主方案，混输取对应成员。
#[test]
fn alias_target_follows_active_scheme() {
    let targets = |active: &str| {
        let m = mgr(&format!("alias_target_{active}"), active, &[]);
        [
            SchemeKind::Codetable,
            SchemeKind::Pinyin,
            SchemeKind::Mixed,
            SchemeKind::English,
        ]
        .map(|k| m.scheme_alias_target(k).unwrap_or_default())
    };
    assert_eq!(
        targets("wubi86"),
        ["wubi86", "pinyin", "wubi86_pinyin", "english"]
    );
    assert_eq!(
        targets("shuangpin"),
        ["wubi86", "shuangpin", "wubi86_pinyin", "english"]
    );
    assert_eq!(
        targets("wubi86_pinyin"),
        ["wubi86", "pinyin", "wubi86_pinyin", "english"]
    );
    assert_eq!(
        targets("stroke"),
        ["stroke", "pinyin", "wubi86_pinyin", "english"]
    );
}

#[test]
fn alias_set_lands_in_target_scheme_and_reads_back() {
    let m = mgr("alias_set", "shuangpin", &[]);
    let done = m
        .scheme_alias_set("schema.pinyin.sentence_count", toml::Value::Integer(3))
        .expect("是别名")
        .expect("写入成功");
    assert_eq!(done, "shuangpin");
    assert_eq!(m.pinyin_settings_of("shuangpin").sentence_count, 3);
    assert_eq!(
        m.pinyin_settings_of("pinyin").sentence_count,
        1,
        "全拼不受影响"
    );
    let got = m
        .scheme_alias_get("schema.pinyin.sentence_count")
        .unwrap()
        .unwrap();
    assert_eq!(got, toml::Value::Integer(3));
    assert!(override_text("alias_set", "shuangpin").contains("sentence_count = 3"));
    assert_eq!(override_text("alias_set", "pinyin"), "");

    // 码表键在拼音方案下指主码表方案；子段与辅助码各落各的段。
    m.scheme_alias_set(
        "schema.codetable.frequency.enabled",
        toml::Value::Boolean(true),
    )
    .unwrap()
    .unwrap();
    m.scheme_alias_set("schema.pinyin.aux_code.enabled", toml::Value::Boolean(true))
        .unwrap()
        .unwrap();
    assert!(m.effective_codetable("wubi86").frequency.enabled);
    assert!(override_text("alias_set", "wubi86").contains("[engine.codetable.frequency]"));
    assert!(override_text("alias_set", "shuangpin").contains("[engine.aux_code]"));
    assert_eq!(
        m.scheme_alias_get("schema.pinyin.aux_code.enabled")
            .unwrap()
            .unwrap(),
        toml::Value::Boolean(true)
    );
}

/// 写一项不能冲掉该方案设置文件里已有的其它内容。
#[test]
fn alias_set_keeps_existing_scheme_settings() {
    let m = mgr(
        "alias_keep",
        "wubi86",
        &[(
            "wubi86",
            "[engine.codetable]\nwildcard = true\n[key_actions]\nbacktick = \"temp_pinyin\"\n",
        )],
    );
    m.scheme_alias_set("schema.codetable.single_char", toml::Value::Boolean(true))
        .unwrap()
        .unwrap();
    let text = override_text("alias_keep", "wubi86");
    assert!(
        text.contains("wildcard = true") && text.contains("single_char = true"),
        "{text}"
    );
    assert!(text.contains("backtick = \"temp_pinyin\""), "{text}");
}

/// 设置文件坏了就拒绝写，而不是拿空表覆盖它。
#[test]
fn alias_set_refuses_to_overwrite_unparsable_file() {
    let m = mgr(
        "alias_bad",
        "wubi86",
        &[("wubi86", "[engine.codetable\nwildcard = ")],
    );
    let before = override_text("alias_bad", "wubi86");
    assert!(
        m.scheme_alias_set("schema.codetable.single_char", toml::Value::Boolean(true))
            .unwrap()
            .is_err()
    );
    assert_eq!(override_text("alias_bad", "wubi86"), before);
}

#[test]
fn non_alias_keys_are_not_intercepted() {
    let m = mgr("alias_none", "wubi86", &[]);
    assert!(m.scheme_alias_get("input.fuzzy.enabled").is_none());
    assert!(
        m.scheme_alias_set("ui.candidate.per_page", toml::Value::Integer(5))
            .is_none()
    );
}

// ── 用户层旧全局段搬入方案设置 ──

fn legacy(text: &str) -> toml::Table {
    toml::from_str(text).unwrap()
}

/// 搬给每个同类方案，生效值与搬之前「全局值对所有同类方案生效」时相同。
#[test]
fn legacy_settings_spread_to_every_scheme_of_the_type() {
    let m = mgr("legacy_spread", "wubi86", &[]);
    let done = m.migrate_legacy_type_settings(&legacy(
        "[codetable]\nwildcard = true\n[codetable.frequency]\nenabled = true\n\
         [pinyin]\nsentence_count = 2\n[pinyin.aux_code]\nenabled = true\n\
         [mix]\nenable_english = true\n[english]\ncommit_space = true\n",
    ));
    let ids: Vec<&str> = done.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "english",
            "pinyin",
            "shuangpin",
            "stroke",
            "wubi86",
            "wubi86_pinyin"
        ]
    );
    for id in ["wubi86", "stroke"] {
        assert!(m.effective_codetable(id).frequency.enabled, "{id}");
    }
    assert!(m.effective_codetable("wubi86").wildcard);
    for id in ["pinyin", "shuangpin"] {
        assert_eq!(m.pinyin_settings_of(id).sentence_count, 2, "{id}");
        assert!(
            override_text("legacy_spread", id).contains("[engine.aux_code]"),
            "{id}"
        );
    }
    assert!(m.mix_settings_of("wubi86_pinyin").enable_english);
    assert!(m.english_settings_of("english").commit_space);
}

/// 方案自己已有值的项不动：方案文件里写的（笔画的 wildcard = false）和用户设置里写的都算。
#[test]
fn legacy_settings_skip_what_the_scheme_already_sets() {
    let m = mgr(
        "legacy_skip",
        "wubi86",
        &[("shuangpin", "[engine.pinyin]\nsentence_count = 5\n")],
    );
    m.migrate_legacy_type_settings(&legacy(
        "[codetable]\nwildcard = true\n[pinyin]\nsentence_count = 2\n",
    ));
    assert!(
        !m.effective_codetable("stroke").wildcard,
        "方案文件声明了 false"
    );
    assert!(m.effective_codetable("wubi86").wildcard);
    assert_eq!(m.pinyin_settings_of("shuangpin").sentence_count, 5);
    assert_eq!(m.pinyin_settings_of("pinyin").sentence_count, 2);
    // 再搬一次什么都不写。
    assert!(
        m.migrate_legacy_type_settings(&legacy(
            "[codetable]\nwildcard = true\n[pinyin]\nsentence_count = 2\n"
        ))
        .is_empty()
    );
}

/// 注册表里没有的键、类型不对的值不搬：搬过去也会在折叠时被丢掉，只会往用户的方案文件里写垃圾。
#[test]
fn legacy_settings_skip_unknown_keys_and_bad_values() {
    let m = mgr("legacy_invalid", "wubi86", &[]);
    m.migrate_legacy_type_settings(&legacy(
        "[pinyin]\nallow_full_pinyin = true\nsentence_count = \"two\"\nsentence_max_count = 4\n",
    ));
    let text = override_text("legacy_invalid", "pinyin");
    assert!(!text.contains("allow_full_pinyin"), "{text}");
    assert!(!text.contains("two"), "{text}");
    assert_eq!(m.pinyin_settings_of("pinyin").sentence_max_count, 4);
}

/// 双拼专有项只搬给双拼方案。
#[test]
fn legacy_shuangpin_settings_go_to_shuangpin_schemes_only() {
    let m = mgr("legacy_sp", "wubi86", &[]);
    m.migrate_legacy_type_settings(&legacy("[pinyin.shuangpin]\nallow_full_pinyin = true\n"));
    assert!(override_text("legacy_sp", "shuangpin").contains("allow_full_pinyin"));
    assert!(!override_text("legacy_sp", "pinyin").contains("shuangpin"));
}

// ── 按方案 id 读写设置（设置页走这条）──

#[test]
fn snapshot_lists_effective_and_default_under_old_key_names() {
    let m = mgr(
        "snapshot",
        "wubi86",
        &[(
            "shuangpin",
            "[engine.pinyin]\nsentence_count = 3\n[engine.aux_code]\nenabled = true\n",
        )],
    );
    let snap = m.scheme_settings_snapshot("shuangpin").unwrap().unwrap();
    let (eff, def) = (&snap.effective, &snap.default);
    assert_eq!(eff["schema.pinyin.sentence_count"].as_integer(), Some(3));
    assert_eq!(def["schema.pinyin.sentence_count"].as_integer(), Some(1));
    assert_eq!(eff["schema.pinyin.aux_code.enabled"].as_bool(), Some(true));
    assert_eq!(def["schema.pinyin.aux_code.enabled"].as_bool(), Some(false));
    assert!(
        eff.keys().all(|k| k.starts_with("schema.pinyin.")),
        "只含本类的键"
    );
    assert!(
        !eff.contains_key("schema.pinyin.fuzzy.enabled"),
        "模糊音不是方案设置"
    );
    // 方案文件自己声明的值算方案默认（笔画的 z_key_repeat = false）。
    let def = m
        .scheme_settings_snapshot("stroke")
        .unwrap()
        .unwrap()
        .default;
    assert_eq!(def["schema.codetable.z_key_repeat"].as_bool(), Some(false));
}

#[test]
fn snapshot_builtin_lists_only_what_the_scheme_file_changed() {
    let m = mgr(
        "builtin",
        "wubi86",
        &[("stroke", "[engine.codetable]\ntop_code_commit = false\n")],
    );
    // 双拼方案文件把英文位次写成 2：给出的是类型内置默认 1。
    let sp = m.scheme_settings_snapshot("shuangpin").unwrap().unwrap();
    assert_eq!(
        sp.builtin["schema.pinyin.english_merge.exact_position"].as_integer(),
        Some(1)
    );
    assert_eq!(sp.builtin.len(), 1, "{:?}", sp.builtin);
    // 笔画方案文件写的 wildcard 与内置默认相同，不算预设；用户层改的项也不算。
    let snap = m.scheme_settings_snapshot("stroke").unwrap().unwrap();
    assert!(!snap.builtin.contains_key("schema.codetable.wildcard"));
    assert!(
        !snap
            .builtin
            .contains_key("schema.codetable.top_code_commit")
    );
    assert_eq!(
        snap.effective["schema.codetable.top_code_commit"].as_bool(),
        Some(false)
    );
    // 全拼方案文件没改任何设置项。
    let py = m.scheme_settings_snapshot("pinyin").unwrap().unwrap();
    assert!(py.builtin.is_empty(), "{:?}", py.builtin);
}

#[test]
fn apply_writes_only_differences_and_reset_restores_default() {
    let m = mgr("apply", "wubi86", &[]);
    m.scheme_settings_apply(
        "pinyin",
        vec![
            (
                "schema.pinyin.sentence_count".into(),
                toml::Value::Integer(4),
            ),
            (
                "schema.pinyin.frequency.half_life".into(),
                toml::Value::Float(48.0),
            ),
        ],
        &[],
    )
    .unwrap();
    assert_eq!(m.pinyin_settings_of("pinyin").sentence_count, 4);
    assert_eq!(m.pinyin_settings_of("pinyin").frequency.half_life, 48.0);
    // 写回方案默认值 = 删掉那一项；恢复默认同理。文件里不留空表。
    m.scheme_settings_apply(
        "pinyin",
        vec![(
            "schema.pinyin.sentence_count".into(),
            toml::Value::Integer(1),
        )],
        &["schema.pinyin.frequency.half_life".to_string()],
    )
    .unwrap();
    assert_eq!(override_text("apply", "pinyin").trim(), "");
    assert_eq!(m.pinyin_settings_of("pinyin").sentence_count, 1);
}

#[test]
fn apply_rejects_keys_of_another_kind() {
    let m = mgr("apply_kind", "wubi86", &[]);
    assert!(
        m.scheme_settings_apply(
            "pinyin",
            vec![(
                "schema.codetable.wildcard".into(),
                toml::Value::Boolean(true)
            )],
            &[],
        )
        .is_err()
    );
    assert_eq!(override_text("apply_kind", "pinyin"), "");
}
