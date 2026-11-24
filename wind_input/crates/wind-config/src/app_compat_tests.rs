use super::*;

/// `pair_via_comp` 三态：未配 = None（跟随全局）；配 true/false 各自生效；
/// 容错值回落 None 而不是让整份 compat.toml 失效。
#[test]
fn pair_via_comp_three_states() {
    let unset = AppCompat::from_single_layer_text("[[apps]]\nprocess = \"a.exe\"\n");
    assert_eq!(unset.get_rule("a.exe").and_then(|r| r.pair_via_comp), None);

    let on = AppCompat::from_single_layer_text(
        "[[apps]]\nprocess = \"acad.exe\"\npair_via_comp = true\n",
    );
    assert_eq!(
        on.get_rule("acad.exe").and_then(|r| r.pair_via_comp),
        Some(true)
    );
    assert_eq!(on.get_rule("other.exe").and_then(|r| r.pair_via_comp), None);

    let off = AppCompat::from_single_layer_text(
        "[[apps]]\nprocess = \"notepad.exe\"\npair_via_comp = false\n",
    );
    assert_eq!(
        off.get_rule("notepad.exe").and_then(|r| r.pair_via_comp),
        Some(false)
    );

    // 类型写错（"yes" / 1）只让本字段回落 None，不让整份 compat.toml 失效
    // （compat 没有段级降级，不容错就是整份文件静默失效）。
    let bogus = AppCompat::from_single_layer_text(
        "[[apps]]\nprocess = \"b.exe\"\npair_via_comp = \"yes\"\n",
    );
    assert_eq!(bogus.get_rule("b.exe").and_then(|r| r.pair_via_comp), None);
}

/// `assoc_placeholder` 优先于 `composition_placeholder`，都没配 = 空格；认不出的值退化为未配置。
#[test]
fn assoc_placeholder_overrides_then_follows_composition() {
    let c = AppCompat::from_single_layer_text(
        r#"
            [[apps]]
            process = "*"
            composition_placeholder = "zwsp"

            [[apps]]
            process = "a.exe"
            assoc_placeholder = "Blank"

            [[apps]]
            process = "b.exe"
            assoc_placeholder = "bogus"
            "#,
    );
    assert_eq!(c.assoc_placeholder_for("a.exe"), AssocPlaceholder::Blank);
    assert_eq!(
        c.composition_placeholder_for("a.exe"),
        PlaceholderChar::Zwsp
    );
    assert_eq!(c.assoc_placeholder_for("b.exe"), AssocPlaceholder::Zwsp);
    assert_eq!(c.assoc_placeholder_for("other.exe"), AssocPlaceholder::Zwsp);
    assert_eq!(c.assoc_placeholder_for(""), AssocPlaceholder::Space);
    let n = AppCompat::from_single_layer_text(
        "[[apps]]\nprocess = \"n.exe\"\nassoc_placeholder = \"None\"\n",
    );
    assert_eq!(n.assoc_placeholder_for("n.exe"), AssocPlaceholder::None);
    assert_eq!(AssocPlaceholder::None.as_str(), "");
    assert_eq!(
        AssocPlaceholder::from_config("none"),
        Some(AssocPlaceholder::None)
    );
}

/// `composition_placeholder` 解析 + 查表（含 `process = "*"` 通配与具体进程压过通配）。
#[test]
fn composition_placeholder_parses_and_falls_back_to_wildcard() {
    let toml = r#"
            [[apps]]
            process = "msedge.exe"
            composition_placeholder = "zwsp"

            [[apps]]
            process = "WPS.exe"
            composition_placeholder = "space"

            [[apps]]
            process = "plain.exe"
            caret_use_top = true

            [[apps]]
            process = "typo.exe"
            composition_placeholder = "zwj"
        "#;
    let compat = AppCompat::from_rules(toml::from_str::<AppCompatFile>(toml).unwrap().apps);
    assert_eq!(
        compat
            .get_rule("MSEDGE.EXE")
            .unwrap()
            .composition_placeholder,
        Some(PlaceholderChar::Zwsp)
    );
    assert_eq!(
        compat
            .get_rule("plain.exe")
            .unwrap()
            .composition_placeholder,
        None
    );
    assert_eq!(
        compat.get_rule("typo.exe").unwrap().composition_placeholder,
        None,
        "值域外 ⇒ 回落未配置，不拖垮整份"
    );
    assert_eq!(
        compat.composition_placeholder_for("msedge.exe"),
        PlaceholderChar::Zwsp
    );
    assert_eq!(
        compat.composition_placeholder_for("plain.exe"),
        PlaceholderChar::Space
    );
    assert_eq!(
        compat.composition_placeholder_for("unknown.exe"),
        PlaceholderChar::Space
    );
    assert_eq!(
        compat.composition_placeholder_for(""),
        PlaceholderChar::Space
    );

    // 通配：没配的进程跟通配走；具体进程显式写 space 压过通配。
    let wild = r#"
            [[apps]]
            process = "*"
            composition_placeholder = "zwsp"

            [[apps]]
            process = "WPS.exe"
            composition_placeholder = "space"
        "#;
    let compat = AppCompat::from_rules(toml::from_str::<AppCompatFile>(wild).unwrap().apps);
    assert_eq!(
        compat.composition_placeholder_for("chrome.exe"),
        PlaceholderChar::Zwsp
    );
    assert_eq!(
        compat.composition_placeholder_for("wps.exe"),
        PlaceholderChar::Space
    );
    assert_eq!(
        compat.composition_placeholder_for(""),
        PlaceholderChar::Space,
        "进程未知 ⇒ 不吃通配"
    );
}

/// `syllable_separator` 解析：认不出的值退化为未配置（跟随全局）；`none` 是真值而非「没配」。
#[test]
fn syllable_separator_parses() {
    let compat = AppCompat::from_single_layer_text(
        r#"
            [[apps]]
            process = "ue.exe"
            syllable_separator = "None"
            [[apps]]
            process = "bad.exe"
            syllable_separator = "tab"
            "#,
    );
    let get = |p: &str| compat.get_rule(p).and_then(|r| r.syllable_separator);
    assert_eq!(get("ue.exe"), Some(SyllableSeparator::None));
    assert_eq!(get("bad.exe"), None);
    for v in [
        SyllableSeparator::Apostrophe,
        SyllableSeparator::None,
        SyllableSeparator::Space,
        SyllableSeparator::Dot,
    ] {
        assert_eq!(SyllableSeparator::from_config(v.as_config()), Some(v));
    }
}

/// `multiline_commit` 解析：认不出的值退化为未配置；`process = "*"` 通配与窗口条件照常生效。
#[test]
fn multiline_commit_parses_and_composes() {
    let toml = r#"
            [[apps]]
            process = "Weixin.exe"
            multiline_commit = "Paste"

            [[apps]]
            process = "typo.exe"
            multiline_commit = "clipboard"

            [[apps]]
            process = "num.exe"
            multiline_commit = 1
        "#;
    let compat = AppCompat::from_rules(toml::from_str::<AppCompatFile>(toml).unwrap().apps);
    let get = |p: &str| compat.get_rule(p).and_then(|r| r.multiline_commit);
    assert_eq!(get("weixin.exe"), Some(MultilineCommit::Paste));
    assert_eq!(get("typo.exe"), None, "值域外 ⇒ 未配置，不拖垮整份");
    assert_eq!(get("num.exe"), None, "类型错 ⇒ 未配置");
    assert_eq!(MultilineCommit::default(), MultilineCommit::Commit);
    for v in [MultilineCommit::Commit, MultilineCommit::Paste] {
        assert_eq!(MultilineCommit::from_config(v.as_config()), Some(v));
    }

    let wild = r#"
            [[apps]]
            process = "*"
            multiline_commit = "paste"

            [[apps]]
            process = "WindowsTerminal.exe"
            multiline_commit = "commit"

            [[apps]]
            process = "app.exe"
            class = "Chat*"
            multiline_commit = "commit"
        "#;
    let compat = AppCompat::from_single_layer_text(wild);
    let at = |process: &str, class: &str| {
        compat
            .resolve(&WindowCtx {
                process,
                class,
                title: "",
            })
            .rule
            .as_ref()
            .and_then(|r| r.multiline_commit)
    };
    assert_eq!(at("x.exe", ""), Some(MultilineCommit::Paste), "通配");
    assert_eq!(
        at("windowsterminal.exe", ""),
        Some(MultilineCommit::Commit),
        "具体进程压过通配"
    );
    assert_eq!(at("app.exe", "ChatWnd"), Some(MultilineCommit::Commit));
    assert_eq!(at("app.exe", "Other"), Some(MultilineCommit::Paste));
}

#[test]
fn placeholder_char_config_roundtrip() {
    for v in [
        PlaceholderChar::Space,
        PlaceholderChar::Zwsp,
        PlaceholderChar::Blank,
    ] {
        assert_eq!(PlaceholderChar::from_config(v.as_config()), Some(v));
    }
    assert_eq!(PlaceholderChar::Space.as_str(), " ");
    assert_eq!(PlaceholderChar::Zwsp.as_str(), "\u{200B}");
    assert_eq!(
        PlaceholderChar::default(),
        PlaceholderChar::Space,
        "不写 = 现状（空格）"
    );
    assert_eq!(
        PlaceholderChar::from_config("ZWSP"),
        Some(PlaceholderChar::Zwsp)
    );
    assert_eq!(PlaceholderChar::from_config("nbsp"), None);
}

/// 钉钉在线表格一类宿主：选中单元格直接打字时，靠「出现了内容」才进入编辑态——ZWSP 被当成
/// 没有内容，空格又会被受控输入 trim 掉。`blank`（U+2800）两头都躲开：有宽度、非空白。
#[test]
fn blank_placeholder_is_non_whitespace_with_width() {
    let toml = r#"
            [[apps]]
            process = "DingTalk.exe"
            composition_placeholder = "Blank"
        "#;
    let compat = AppCompat::from_rules(toml::from_str::<AppCompatFile>(toml).unwrap().apps);
    assert_eq!(
        compat.composition_placeholder_for("dingtalk.exe"),
        PlaceholderChar::Blank
    );
    assert_eq!(
        PlaceholderChar::from_config(PlaceholderChar::Blank.as_config()),
        Some(PlaceholderChar::Blank)
    );
    assert_eq!(PlaceholderChar::Blank.as_config(), "blank");
    assert_eq!(PlaceholderChar::Blank.as_str(), "\u{2800}");
    let ch = PlaceholderChar::Blank.as_str().chars().next().unwrap();
    assert_eq!(PlaceholderChar::Blank.as_str().chars().count(), 1);
    // JS `trim()` / `\s` 的空白集 ⊂ Unicode White_Space（外加 U+FEFF）；它不在其中。
    assert!(!ch.is_whitespace(), "U+2800 不得是空白，否则会被 trim 掉");
    assert_ne!(ch, '\u{FEFF}');
    assert_eq!(
        toml::Value::try_from(PlaceholderChar::Blank).unwrap(),
        toml::Value::String("blank".into()),
        "序列化写回 compat.toml 的取值"
    );
}

/// `auto_pair` / `smart_method` 未配置必须是 `None`：那是「跟随全局」与「显式关掉」的分界，
/// 退化成 bool 等于给所有未配规则的应用关掉功能。
#[test]
fn parse_per_app_pair_symbol_and_offset() {
    let toml = r#"
            [[apps]]
            process = "EXCEL.EXE"
            auto_pair = false
            caret_offset_x = -2
            caret_offset_y = 3

            [[apps]]
            process = "Tabby.exe"
            smart_method = "hold_composition"

            [[apps]]
            process = "plain.exe"
            caret_use_top = true

            [[apps]]
            process = "QQ.exe"
            composition_start_pair_guard = true
        "#;
    let compat = AppCompat::from_rules(toml::from_str::<AppCompatFile>(toml).unwrap().apps);

    let excel = compat.get_rule("excel.exe").unwrap();
    assert_eq!(excel.auto_pair, Some(false));
    assert_eq!((excel.caret_offset_x, excel.caret_offset_y), (-2, 3));
    assert_eq!(excel.smart_method, None, "未配 = 跟随全局");

    let tabby = compat.get_rule("TABBY.EXE").unwrap();
    assert_eq!(tabby.smart_method, Some(SmartMethod::HoldComposition));
    assert_eq!(tabby.auto_pair, None, "未配 = 跟随全局");

    // 只配了别的字段的规则，三个新字段必须全部是"不干预"，不能被默认值污染。
    let plain = compat.get_rule("plain.exe").unwrap();
    assert_eq!(plain.auto_pair, None);
    assert_eq!(plain.smart_method, None);
    assert_eq!((plain.caret_offset_x, plain.caret_offset_y), (0, 0));

    let qq = compat.get_rule("qq.exe").unwrap();
    assert_eq!(qq.composition_start_pair_guard, Some(true));
}

/// `password_force_english` 三态解析：未配 = `None`（跟随全局），显式 true/false 原样保留。
#[test]
fn parse_password_force_english_tristate() {
    let toml = r#"
            [[apps]]
            process = "misreport.exe"
            password_force_english = false

            [[apps]]
            process = "strict.exe"
            password_force_english = true

            [[apps]]
            process = "plain.exe"
            caret_use_top = true
        "#;
    let compat = AppCompat::from_rules(toml::from_str::<AppCompatFile>(toml).unwrap().apps);
    assert_eq!(
        compat
            .get_rule("MISREPORT.EXE")
            .unwrap()
            .password_force_english,
        Some(false)
    );
    assert_eq!(
        compat
            .get_rule("strict.exe")
            .unwrap()
            .password_force_english,
        Some(true)
    );
    assert_eq!(
        compat.get_rule("plain.exe").unwrap().password_force_english,
        None,
        "未配 = 跟随全局，不能被 bool 默认值污染"
    );
}

/// 类型写错只让本字段回落 `None`，不得让整份 compat.toml 失效：`load_file` 没有
/// 段级降级，一个 `"yes"` 就会连带所有应用的所有规则静默消失。
#[test]
fn password_force_english_wrong_type_does_not_sink_the_file() {
    for bad in [r#""yes""#, "1", "[true]"] {
        let toml = format!(
            r#"
                [[apps]]
                process = "typo.exe"
                password_force_english = {bad}

                [[apps]]
                process = "other.exe"
                auto_pair = false
                "#
        );
        let file = toml::from_str::<AppCompatFile>(&toml)
            .unwrap_or_else(|e| panic!("{bad}: 类型写错不得让整份失败：{e}"));
        let compat = AppCompat::from_rules(file.apps);
        assert_eq!(
            compat.get_rule("typo.exe").unwrap().password_force_english,
            None,
            "{bad}: 认不出 = 没配过"
        );
        assert_eq!(
            compat.get_rule("other.exe").unwrap().auto_pair,
            Some(false),
            "{bad}: 同文件其它规则必须照常生效"
        );
    }
}

// ── 状态气泡定位 ────────────────────────────────────────────

/// 两个值域表与解析/回写同源：表里每一项都能解析、且回写成同一个串。
#[test]
fn status_value_tables_roundtrip() {
    for v in STATUS_POSITION_MODES {
        let m = StatusPositionMode::from_config(v).unwrap_or_else(|| panic!("{v}"));
        assert_eq!(m.as_config(), v);
    }
    for v in STATUS_FALLBACK_POSITIONS {
        let f = StatusFallback::from_config(v).unwrap_or_else(|| panic!("{v}"));
        assert_eq!(f.as_config(), v);
    }
    for a in StatusAnchor::ALL {
        assert!(STATUS_POSITION_MODES.contains(&a.as_config()));
        assert!(STATUS_FALLBACK_POSITIONS.contains(&a.as_config()));
    }
    // 两张表只有前两档不同：定位方式没有 last/hide，兜底没有 follow_caret/fixed。
    assert_eq!(StatusPositionMode::from_config("last"), None);
    assert_eq!(StatusFallback::from_config("fixed"), None);
}

#[test]
fn status_rule_fields_parse() {
    let c = parse_rules(
        "[[apps]]\nprocess = \"ai.exe\"\nstatus_position_mode = \"window_bottom_left\"\n\
             status_fallback_position = \"screen_top_right\"\n\n\
             [[apps]]\nprocess = \"fx.exe\"\nstatus_position_mode = \"fixed\"\n\
             status_x = 120\nstatus_y = -40\n",
    );
    let ai = c.get_rule("ai.exe").unwrap();
    assert_eq!(
        ai.status_position_mode,
        Some(StatusPositionMode::Anchor(StatusAnchor::WindowBottomLeft))
    );
    assert_eq!(
        ai.status_fallback_position,
        Some(StatusFallback::Anchor(StatusAnchor::ScreenTopRight))
    );
    let fx = c.get_rule("fx.exe").unwrap();
    assert_eq!(fx.status_position_mode, Some(StatusPositionMode::Fixed));
    assert_eq!((fx.status_x, fx.status_y), (120, -40));
    assert_eq!(fx.status_fallback_position, None);
}

/// 锚点拼错 / fallback 拼错 / 类型写错：只让该项回落「跟随全局」，同条规则的其它字段与
/// 同文件其它规则照常生效。
#[test]
fn status_rule_typos_only_drop_that_field() {
    for (field, bad) in [
        ("status_position_mode", r#""screen_centre""#),
        ("status_position_mode", "3"),
        ("status_position_mode", r#""last""#),
        ("status_fallback_position", r#""hidden""#),
        ("status_fallback_position", "true"),
        ("status_fallback_position", r#""fixed""#),
    ] {
        let text = format!(
            "[[apps]]\nprocess = \"typo.exe\"\n{field} = {bad}\nstatus_x = 5\n\
                 initial_mode = \"english\"\n\n\
                 [[apps]]\nprocess = \"other.exe\"\nstatus_position_mode = \"screen_center\"\n"
        );
        let file = toml::from_str::<AppCompatFile>(&text)
            .unwrap_or_else(|e| panic!("{field} = {bad}: 不得整份失败：{e}"));
        let c = AppCompat::from_rules(file.apps);
        let typo = c.get_rule("typo.exe").unwrap();
        assert_eq!(typo.status_position_mode, None, "{field} = {bad}");
        assert_eq!(typo.status_fallback_position, None, "{field} = {bad}");
        assert_eq!(typo.status_x, 5, "{field} = {bad}: 同条其它字段照常");
        assert_eq!(typo.initial_mode, Some(InitialMode::English));
        assert_eq!(
            c.get_rule("other.exe").unwrap().status_position_mode,
            Some(StatusPositionMode::Anchor(StatusAnchor::ScreenCenter)),
            "{field} = {bad}: 同文件其它规则必须照常生效"
        );
    }
}

/// 最初的现场：`auto_pair = "yes"` 曾让整份 compat.toml 静默失效。
#[test]
fn auto_pair_wrong_type_does_not_sink_the_file() {
    let file = toml::from_str::<AppCompatFile>(
        "[[apps]]\nprocess = \"et.exe\"\nauto_pair = \"yes\"\n\n\
             [[apps]]\nprocess = \"other.exe\"\ncaret_use_top = true\n",
    )
    .expect("auto_pair 写错不得让整份失败");
    let compat = AppCompat::from_rules(file.apps);
    assert_eq!(compat.get_rule("et.exe").unwrap().auto_pair, None);
    assert!(compat.get_rule("other.exe").unwrap().caret_use_top);
}

/// 另两段（`[[initial_mode_scope]]` / `[[commit_newline]]`）的 `process` / `comment` 同理：
/// 类型写错只让那一条作废（或那一项忽略），不得让整份 compat.toml 失效。
#[test]
fn side_sections_process_wrong_type_voids_only_that_rule() {
    let file = toml::from_str::<AppCompatFile>(
        "[[apps]]\nprocess = \"other.exe\"\nauto_pair = false\n\n\
             [[initial_mode_scope]]\nprocess = 1\nclasses = [\"X\"]\n\n\
             [[initial_mode_scope]]\ncomment = 2\nprocess = \"explorer.exe\"\nclasses = [\"Progman\"]\n\n\
             [[commit_newline]]\nprocess = true\nstyle = \"cr\"\n\n\
             [[commit_newline]]\nprocess = \"WINWORD.EXE\"\ncomment = [1]\nstyle = \"cr\"\n",
    )
    .expect("process / comment 类型写错不得让整份失败");
    let compat = AppCompat::from_parts(file.apps, file.initial_mode_scope)
        .with_commit_newline(file.commit_newline);
    assert_eq!(compat.get_rule("other.exe").unwrap().auto_pair, Some(false));
    assert!(compat.initial_mode_applies_to_window("explorer.exe", "Progman"));
    assert!(!compat.initial_mode_applies_to_window("explorer.exe", "X"));
    assert_eq!(
        compat.commit_newline_for("winword.exe"),
        Some(NewlineStyle::Cr)
    );
    assert_eq!(compat.commit_newline_for("true"), None);
}

fn parse_rules(toml: &str) -> AppCompat {
    AppCompat::from_single_layer_text(toml)
}

/// `schema` 三态：未配 = 跟随全局；具体 id = 固定；`@remember` = 记住上次。
#[test]
fn parse_app_schema_fixed_and_remember() {
    let compat = parse_rules(
        r#"
            [[apps]]
            process = "code.exe"
            schema = "english"

            [[apps]]
            process = "weixin.exe"
            schema = "@remember"

            [[apps]]
            process = "plain.exe"
            auto_pair = false
            "#,
    );
    assert_eq!(
        compat.get_rule("CODE.EXE").unwrap().app_schema(),
        Some(AppSchema::Fixed("english"))
    );
    assert_eq!(
        compat.get_rule("weixin.exe").unwrap().app_schema(),
        Some(AppSchema::Remember)
    );
    assert_eq!(compat.get_rule("plain.exe").unwrap().schema, None);
    assert_eq!(compat.get_rule("plain.exe").unwrap().app_schema(), None);
}

/// `@` 前缀是保留标记空间：只认 `@remember`，其余（拼错、将来的新标记）按未配置处理；
/// 空串同理。同文件其它规则必须照常生效。
#[test]
fn unknown_app_schema_marker_degrades_to_none() {
    for bad in [r#""@remeber""#, r#""@global""#, r#""""#, r#""   ""#] {
        let compat = parse_rules(&format!(
            r#"
                [[apps]]
                process = "typo.exe"
                schema = {bad}

                [[apps]]
                process = "other.exe"
                auto_pair = false
                "#
        ));
        assert_eq!(
            compat.get_rule("typo.exe").and_then(|r| r.schema.clone()),
            None,
            "{bad}: 认不出 = 没配过"
        );
        assert_eq!(
            compat.get_rule("other.exe").unwrap().auto_pair,
            Some(false),
            "{bad}: 同文件其它规则必须照常生效"
        );
    }
}

/// 类型写错（`schema = 1` / 数组）不得让整份 compat.toml 失效：`load_file` 没有段级降级。
#[test]
fn app_schema_wrong_type_does_not_sink_the_file() {
    for bad in ["1", "true", r#"["pinyin"]"#] {
        let text = format!(
            r#"
                [[apps]]
                process = "typo.exe"
                schema = {bad}

                [[apps]]
                process = "other.exe"
                schema = "pinyin"
                "#
        );
        let file = toml::from_str::<AppCompatFile>(&text)
            .unwrap_or_else(|e| panic!("{bad}: 类型写错不得让整份失败：{e}"));
        let compat = AppCompat::from_rules(file.apps);
        assert_eq!(
            compat.get_rule("typo.exe").and_then(|r| r.schema.clone()),
            None
        );
        assert_eq!(
            compat.get_rule("other.exe").unwrap().app_schema(),
            Some(AppSchema::Fixed("pinyin"))
        );
    }
}

#[test]
fn parse_apps_array_and_lookup_case_insensitive() {
    let toml = r#"
            [[apps]]
            process = "Weixin.exe"
            comment = "微信"
            caret_use_top = true
        "#;
    let file: AppCompatFile = toml::from_str(toml).unwrap();
    let compat = AppCompat::from_rules(file.apps);

    // 进程名匹配不区分大小写。
    let rule = compat
        .get_rule("weixin.exe")
        .expect("应命中 Weixin.exe 规则");
    assert!(rule.caret_use_top);
    assert!(compat.get_rule("WEIXIN.EXE").unwrap().caret_use_top);
    // 未配置的进程无规则。
    assert!(compat.get_rule("notepad.exe").is_none());
}

#[test]
fn caret_use_top_defaults_false_when_absent() {
    let toml = r#"
            [[apps]]
            process = "Foo.exe"
        "#;
    let file: AppCompatFile = toml::from_str(toml).unwrap();
    let compat = AppCompat::from_rules(file.apps);
    let rule = compat.get_rule("foo.exe").unwrap();
    assert!(!rule.caret_use_top);
    assert_eq!(rule.composition_start_pair_guard, None);
    // 缺字段 = 不干预（跟随全局），与 initial_mode 同语义。若它退化成 Some(默认档)，
    // 等于给所有未配置的应用都写死了一份 per-app 覆盖，用户改全局默认时全部失效。
    assert_eq!(rule.first_show_mode, None);
    // 缺字段 = 不干预。若这两个退化成 Some(English)，等于给所有未配置的应用
    // 都配上了「初始英文」——这正是字段必须用 Option 而非 bool 的原因。
    assert_eq!(rule.initial_mode, None);
    assert_eq!(rule.initial_punct, None);
}

#[test]
fn initial_mode_parses_both_values() {
    let toml = r#"
            [[apps]]
            process = "Everything.exe"
            initial_mode = "english"
            initial_punct = "chinese"
        "#;
    let file: AppCompatFile = toml::from_str(toml).unwrap();
    let compat = AppCompat::from_rules(file.apps);
    let rule = compat.get_rule("everything.exe").unwrap();
    assert_eq!(rule.initial_mode, Some(InitialMode::English));
    assert_eq!(rule.initial_punct, Some(InitialMode::Chinese));
    assert!(!rule.initial_mode.unwrap().is_chinese());
    assert!(rule.initial_punct.unwrap().is_chinese());
}

/// 单字段拼错只让该字段退化为「不干预」，不得连累同规则的其它字段、
/// 也不得让整份 compat.toml 解析失败（`load_file` 失败会静默跳过整个文件）。
#[test]
fn unknown_initial_mode_degrades_to_none_without_killing_the_file() {
    let toml = r#"
            [[apps]]
            process = "Everything.exe"
            initial_mode = "englsh"
            caret_use_top = true

            [[apps]]
            process = "Weixin.exe"
            initial_mode = "chinese"
        "#;
    let file: AppCompatFile = toml::from_str(toml).expect("拼错的值不得让整份文件解析失败");
    let compat = AppCompat::from_rules(file.apps);
    let bad = compat.get_rule("everything.exe").unwrap();
    assert_eq!(bad.initial_mode, None, "无法识别 → 不干预");
    assert!(bad.caret_use_top, "同规则的其它字段不受牵连");
    // 后续规则完整存活。
    assert_eq!(
        compat.get_rule("weixin.exe").unwrap().initial_mode,
        Some(InitialMode::Chinese)
    );
}

#[test]
fn mode_parses_from_config_and_rejects_unknown() {
    assert_eq!(
        FirstShowMode::from_config("fast"),
        Some(FirstShowMode::Fast)
    );
    assert_eq!(
        FirstShowMode::from_config(" INSTANT "),
        Some(FirstShowMode::Instant)
    );
    assert_eq!(
        FirstShowMode::from_config("wait"),
        Some(FirstShowMode::Wait)
    );
    // 未知值 = None。在 per-app 层它等于「没配」＝跟随全局；在全局层由调用点
    // `.unwrap_or_default()` 回落到默认档——回落只发生在一处，不再有两处独立事实。
    assert_eq!(FirstShowMode::from_config("turbo"), None);
    assert_eq!(FirstShowMode::Fast.as_config(), "fast");
}

/// 拼错的档位名不得让整份 compat.toml 失效，也不得固化成显式覆盖。
#[test]
fn unknown_mode_in_toml_degrades_to_follow_global() {
    let toml = r#"
            [[apps]]
            process = "Foo.exe"
            first_show_mode = "turbo"
            caret_use_top = true
        "#;
    let file: AppCompatFile = toml::from_str(toml).expect("单字段拼错不得整份解析失败");
    assert_eq!(file.apps[0].first_show_mode, None, "认不出 = 跟随全局");
    assert!(file.apps[0].caret_use_top, "同一条规则的其它字段仍须生效");
}

/// 默认档位是产品决策，改动时必须显式过这一关；取舍见
/// `docs/redesign/candidate-window-positioning.md` 第 6 层。
#[test]
fn default_mode_is_fast() {
    assert_eq!(FirstShowMode::default(), FirstShowMode::Fast);
}

/// 枚举 `#[default]`（认不出的值回落到它）与出厂 `ui.candidate.first_show_mode` 必须一致，
/// 分叉没有编译信号。顺带验出厂串是合法档位名。
#[test]
fn global_default_config_value_matches_enum_default() {
    let factory = crate::config::UiCandidateConfig::default().first_show_mode;
    assert_eq!(
        FirstShowMode::from_config(&factory),
        Some(FirstShowMode::default()),
        "config.toml 出厂档 {factory:?} 与枚举 #[default] 不一致"
    );
}

/// 缺字段 = 不干预。若退化成 `Some(默认档)`，等于给所有未配置的应用都写死了一份
/// per-app 覆盖，用户改全局 `ui.candidate.position_mode` 时会全部失效。
#[test]
fn new_per_app_fields_default_to_follow_global() {
    let toml = r#"
            [[apps]]
            process = "Foo.exe"
        "#;
    let file: AppCompatFile = toml::from_str(toml).unwrap();
    let compat = AppCompat::from_rules(file.apps);
    let rule = compat.get_rule("foo.exe").unwrap();
    assert_eq!(rule.candidate_position_mode, None);
    assert_eq!(rule.ignore_host_ime_switch, None);
    assert_eq!((rule.candidate_x, rule.candidate_y), (0, 0));
}

/// 认不出的定位方式退化为 `None`（跟随全局），且不拖垮同文件其它规则——
/// 整份 compat.toml 因一个拼错的值静默失效是本仓反复记过的形态。
#[test]
fn bad_candidate_position_mode_degrades_to_none() {
    let toml = r#"
            [[apps]]
            process = "Foo.exe"
            candidate_position_mode = "fixedd"

            [[apps]]
            process = "Bar.exe"
            candidate_position_mode = "fixed"
        "#;
    let file: AppCompatFile = toml::from_str(toml).expect("单字段拼错不得让整份失效");
    let compat = AppCompat::from_rules(file.apps);
    assert_eq!(
        compat.get_rule("foo.exe").unwrap().candidate_position_mode,
        None
    );
    assert_eq!(
        compat.get_rule("bar.exe").unwrap().candidate_position_mode,
        Some(CandidatePositionMode::Fixed)
    );
}

// ── [[initial_mode_scope]] ──

fn sample_scope() -> Vec<InitialModeScopeRule> {
    vec![InitialModeScopeRule {
        process: "explorer.exe".into(),
        comment: String::new(),
        disabled: false,
        classes: vec!["Progman".into(), "WorkerW".into()],
    }]
}

#[test]
fn mode_scope_match_is_case_insensitive_on_both_keys() {
    let c = AppCompat::from_parts(Vec::new(), sample_scope());
    assert!(c.initial_mode_applies_to_window("explorer.exe", "Progman"));
    assert!(c.initial_mode_applies_to_window("EXPLORER.EXE", "progman"));
    // 作用域外：任务栏 / Alt+Tab / 溢出区 —— 保持现状
    assert!(!c.initial_mode_applies_to_window("explorer.exe", "Shell_TrayWnd"));
    assert!(!c.initial_mode_applies_to_window("explorer.exe", "ForegroundStaging"));
}

/// 未配作用域的进程完全不受影响：白名单判据不得把所有进程都关进作用域。
#[test]
fn process_without_scope_entry_is_unrestricted() {
    let c = AppCompat::from_parts(Vec::new(), sample_scope());
    assert!(c.initial_mode_applies_to_window("notepad.exe", "Notepad"));
    // 连窗口类都拿不到时也照常生效——没配作用域就没有任何限制
    assert!(c.initial_mode_applies_to_window("notepad.exe", ""));
    // 空进程名同理（macOS / 取名失败）
    assert!(c.initial_mode_applies_to_window("", ""));
}

/// 作用域内的进程，窗口类为空时必须保持现状（explorer 首个 focus_gained 拿不到窗口类），
/// 见 docs/architecture/app-compat-host-notes.md 的「初始模式作用域」。
#[test]
fn empty_window_class_stays_outside_scope() {
    let c = AppCompat::from_parts(Vec::new(), sample_scope());
    assert!(!c.initial_mode_applies_to_window("explorer.exe", ""));
}

/// 空 classes = 该进程的初始模式规则在任何窗口上都不重算（不是"不受限"）。
#[test]
fn empty_class_list_blocks_everything_for_that_process() {
    let c = AppCompat::from_parts(
        Vec::new(),
        vec![InitialModeScopeRule {
            process: "explorer.exe".into(),
            comment: String::new(),
            disabled: false,
            classes: Vec::new(),
        }],
    );
    assert!(!c.initial_mode_applies_to_window("explorer.exe", "Progman"));
    assert!(c.initial_mode_applies_to_window("notepad.exe", "Notepad"));
}
