use super::*;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_compat_res_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
/// 每段文本一层（出厂 < data_custom < 用户），走生产加载路径。
fn load(layers: [&str; 3], tag: &str) -> AppCompat {
    let dirs: Vec<_> = (0..3).map(|i| tmp(&format!("{tag}{i}"))).collect();
    for (d, t) in dirs.iter().zip(layers) {
        std::fs::write(d.join(COMPAT_FILE_NAME), t).unwrap();
    }
    let c = AppCompat::load_layered(Some(&dirs[0]), Some(&dirs[1]), Some(&dirs[2]));
    for d in &dirs {
        let _ = std::fs::remove_dir_all(d);
    }
    c
}
fn one(text: &str) -> AppCompat {
    AppCompat::from_single_layer_text(text)
}
fn ctx<'a>(process: &'a str, class: &'a str, title: &'a str) -> WindowCtx<'a> {
    WindowCtx {
        process,
        class,
        title,
    }
}
fn caret_x(c: &AppCompat, w: WindowCtx) -> i32 {
    c.resolve(&w)
        .rule
        .as_ref()
        .map(|r| r.caret_offset_x)
        .unwrap_or(-1)
}

/// 菜单写回的目标：生效值来自「进程 + 窗口条件」的规则时写那条，否则写纯进程键。
#[test]
fn menu_writeback_targets_the_most_specific_process_window_rule_touching_the_field() {
    let c = one(r#"
            [[apps]]
            class = "Chrome_WidgetWin_*"
            first_show_mode = "wait"
            [[apps]]
            process = "ahk.exe"
            first_show_mode = "fast"
            [[apps]]
            process = "ahk.exe"
            class = "AutoHotkey*"
            first_show_mode = "instant"
            [[apps]]
            process = "ahk.exe"
            class = "AutoHotkeyGUI"
            auto_pair = true
            [[apps]]
            process = "ahk.exe"
            class = "AutoHotkeyGUI"
            title = "T*"
            unset = ["auto_pair"]
            [[apps]]
            process = "ahk.exe"
            class = "Off"
            first_show_mode = "wait"
            disabled = true
        "#);
    let id = |class: &str, title: &str| RuleId {
        process: "ahk.exe".into(),
        class: class.into(),
        title: title.into(),
    };
    let gui = ctx("AHK.exe", "AutoHotkeyGUI", "");
    assert_eq!(
        c.menu_writeback_target(&gui, &["first_show_mode"]),
        Some(id("autohotkey*", "")),
        "更具体的 AutoHotkeyGUI 那条没写这个字段，出处是通配那条"
    );
    assert_eq!(
        c.menu_writeback_target(&gui, &["auto_pair"]),
        Some(id("autohotkeygui", ""))
    );
    assert_eq!(
        c.menu_writeback_target(&ctx("ahk.exe", "AutoHotkeyGUI", "Title"), &["auto_pair"]),
        Some(id("autohotkeygui", "t*")),
        "unset 也是出处"
    );
    assert_eq!(
        c.menu_writeback_target(&gui, &["caret_use_top", "auto_pair"]),
        Some(id("autohotkeygui", "")),
        "多字段取任一"
    );
    assert_eq!(
        c.menu_writeback_target(&ctx("ahk.exe", "Other", ""), &["first_show_mode"]),
        None,
        "只命中纯进程规则 ⇒ 写纯进程键"
    );
    assert_eq!(
        c.menu_writeback_target(&ctx("ahk.exe", "Off", ""), &["first_show_mode"]),
        None,
        "禁用的不算"
    );
    assert_eq!(
        c.menu_writeback_target(
            &ctx("code.exe", "Chrome_WidgetWin_1", ""),
            &["first_show_mode"]
        ),
        None,
        "仅窗口条件（不限进程）的出处不改它，写纯进程键即可压过"
    );
    assert_eq!(
        c.menu_writeback_target(&ctx("", "AutoHotkeyGUI", ""), &["auto_pair"]),
        None
    );
}

/// 每级各写一条 caret_offset_x，越具体越后叠（后叠的赢）。规则在文件里**倒序**写，
/// 证明顺序来自级别而不是行序。
#[test]
fn tiers_compose_from_wildcard_to_process_plus_window() {
    let text = r#"
            [[apps]]
            process = "ahk.exe"
            class = "C"
            title = "T"
            caret_offset_x = 7
            [[apps]]
            process = "ahk.exe"
            class = "C"
            caret_offset_x = 6
            [[apps]]
            process = "ahk.exe"
            caret_offset_x = 5
            [[apps]]
            class = "C"
            title = "T"
            caret_offset_x = 4
            [[apps]]
            title = "T*"
            caret_offset_x = 3
            [[apps]]
            class = "C"
            caret_offset_x = 2
            [[apps]]
            process = "*"
            caret_offset_x = 1
        "#;
    let c = one(text);
    assert_eq!(caret_x(&c, ctx("other.exe", "", "")), 1, "T0");
    assert_eq!(caret_x(&c, ctx("other.exe", "C", "")), 2, "T1 仅类名");
    assert_eq!(caret_x(&c, ctx("other.exe", "X", "T")), 3, "T1 仅标题");
    assert_eq!(
        caret_x(&c, ctx("other.exe", "C", "Tx")),
        3,
        "仅类名、仅标题同时命中：仅标题后叠"
    );
    assert_eq!(
        caret_x(&c, ctx("other.exe", "C", "T")),
        4,
        "T1 类名+标题 压过仅标题、仅类名"
    );
    assert_eq!(caret_x(&c, ctx("AHK.exe", "C", "T")), 7, "T3 类名+标题");
    assert_eq!(caret_x(&c, ctx("ahk.exe", "C", "x")), 6, "T3 仅类名");
    assert_eq!(caret_x(&c, ctx("ahk.exe", "X", "T")), 5, "T2 压过 T1");
    assert_eq!(
        caret_x(&c, ctx("", "C", "T")),
        -1,
        "进程名未知：空名不吃通配，窗口规则也不命中"
    );
}

/// 同级内：非通配字符多的（更具体）后叠；一样具体的按层、再按行序。
#[test]
fn within_a_tier_more_literal_patterns_win_then_layer_then_row_order() {
    let c = one(r#"
            [[apps]]
            class = "Chrome_WidgetWin_1"
            caret_offset_x = 2
            [[apps]]
            class = "Chrome_*"
            caret_offset_x = 1
            [[apps]]
            class = "*_WidgetWin_1"
            caret_offset_y = 1
            [[apps]]
            class = "Chrome_W?dgetWin_1"
            caret_offset_y = 2
        "#);
    let r = c.resolve(&ctx("x.exe", "chrome_widgetwin_1", ""));
    let r = r.rule.as_ref().unwrap();
    assert_eq!(r.caret_offset_x, 2, "字面最长的那条最后叠");
    assert_eq!(r.caret_offset_y, 2, "同为 16 个字面字符时，按行序后写的赢");

    let c = load(
        [
            "[[apps]]\nclass = \"Chrome_*\"\ncaret_offset_x = 1\n",
            "[[apps]]\nclass = \"*_WidgetWin\"\ncaret_offset_x = 2\n",
            "[[apps]]\nclass = \"chrome_*\"\ncaret_offset_y = 9\n",
        ],
        "layer",
    );
    let r = c.resolve(&ctx("x.exe", "Chrome_WidgetWin", ""));
    let r = r.rule.as_ref().unwrap();
    assert_eq!(r.caret_offset_x, 2, "同样具体：data_custom 的那条后叠");
    assert_eq!(r.caret_offset_y, 9, "用户层的同身份行叠在出厂那条上");
}

/// 同级、字面字符数也相同的平手：同一层按行序（后写的赢）。
#[test]
fn a_tie_within_one_layer_goes_to_the_later_row() {
    let c = one(
        "[[apps]]\nclass = \"A*\"\ncaret_offset_x = 1\n\n[[apps]]\nclass = \"*A\"\ncaret_offset_x = 2\n\n[[apps]]\nclass = \"?A\"\ncaret_offset_y = 1\n\n[[apps]]\nclass = \"A?\"\ncaret_offset_y = 2\n",
    );
    let r = c.resolve(&ctx("x.exe", "AA", ""));
    let r = r.rule.as_ref().unwrap();
    assert_eq!((r.caret_offset_x, r.caret_offset_y), (2, 2));
}

/// 平手跨层：出厂 < data_custom < 用户，高层（该身份首次出现的层）赢。
#[test]
fn a_tie_across_layers_goes_to_the_higher_layer() {
    let c = load(
        [
            "[[apps]]\nclass = \"*A\"\ncaret_offset_x = 1\n",
            "[[apps]]\nclass = \"A*\"\ncaret_offset_x = 2\ncaret_offset_y = 2\n",
            "[[apps]]\nclass = \"A?\"\ncaret_offset_y = 3\n",
        ],
        "tie",
    );
    let r = c.resolve(&ctx("x.exe", "AA", ""));
    let r = r.rule.as_ref().unwrap();
    assert_eq!(r.caret_offset_x, 2, "data_custom 压过出厂");
    assert_eq!(r.caret_offset_y, 3, "用户层压过 data_custom");
}

/// 空名不吃通配：进程名拿不到时 `get_rule` / `resolve` 都是空结果，`"*"` 与窗口规则都不命中。
#[test]
fn an_empty_process_name_gets_no_rule_at_all() {
    let c = one(
        "[[apps]]\nprocess = \"*\"\ninitial_mode = \"english\"\n\n[[apps]]\nclass = \"C\"\nauto_pair = true\n",
    );
    assert!(c.get_rule("").is_none());
    let r = c.resolve(&ctx("", "C", "t"));
    assert!(r.rule.is_none() && r.window_matches.is_empty());
    assert_eq!(
        c.get_rule(ANY_PROCESS).and_then(|r| r.initial_mode),
        Some(InitialMode::English),
        "显式取通配那条仍然可以"
    );
    assert!(c.get_rule("x.exe").is_some());
}

/// 旧文件里手写的 `*` + 某进程 `unset` 同名字段：unset 跨级生效，把 `*` 设下的值也取消
/// （= 该进程回到跟随全局）。这是设计稿定的语义（P1 起），钉住它。
#[test]
fn a_process_unset_also_cancels_the_wildcard_value() {
    let c = load(
        [
            "[[apps]]\nprocess = \"Foo.exe\"\nfirst_show_mode = \"wait\"\n",
            "",
            "[[apps]]\nprocess = \"*\"\nfirst_show_mode = \"instant\"\nauto_pair = false\n\n[[apps]]\nprocess = \"foo.exe\"\nunset = [\"first_show_mode\"]\n",
        ],
        "unsetstar",
    );
    let foo = c.get_rule("foo.exe").unwrap();
    assert_eq!(foo.first_show_mode, None, "出厂值与通配值一并取消");
    assert_eq!(foo.auto_pair, Some(false), "没 unset 的字段照常继承通配");
    assert_eq!(
        c.get_rule("bar.exe").and_then(|r| r.first_show_mode),
        Some(FirstShowMode::Instant)
    );
}

#[test]
fn empty_class_or_title_never_hits_a_rule_with_that_condition() {
    let c = one(
        "[[apps]]\nclass = \"*\"\ncaret_use_top = true\n\n[[apps]]\ntitle = \"*\"\nauto_pair = false\n",
    );
    assert!(c.resolve(&ctx("a.exe", "", "")).rule.is_none());
    let r = c.resolve(&ctx("a.exe", "X", ""));
    let r = r.rule.as_ref().unwrap();
    assert!(r.caret_use_top);
    assert_eq!(r.auto_pair, None, "标题为空，带标题条件的规则不命中");
}

/// 行内 unset 跨级生效：更具体那条把通配设下的字段取消 = 本窗口回到全局默认。
/// disabled 的行不参与合成。
#[test]
fn unset_reaches_across_tiers_and_disabled_rows_do_not_take_part() {
    let c = one(r#"
            [[apps]]
            process = "*"
            first_show_mode = "wait"
            initial_mode = "english"
            [[apps]]
            process = "ahk.exe"
            title = "Editor*"
            unset = ["first_show_mode"]
            [[apps]]
            process = "ahk.exe"
            class = "X"
            disabled = true
            initial_mode = "chinese"
        "#);
    let r = c.resolve(&ctx("ahk.exe", "X", "Editor - 1"));
    let r = r.rule.as_ref().unwrap();
    assert_eq!(r.first_show_mode, None, "unset 取消了通配那条的值");
    assert_eq!(
        r.initial_mode,
        Some(InitialMode::English),
        "被禁用的那条不参与"
    );
    assert_eq!(
        c.get_rule("ahk.exe").and_then(|r| r.first_show_mode),
        Some(FirstShowMode::Wait),
        "没有窗口上下文时窗口规则不命中，unset 也就不生效"
    );
}

/// `process = "*"` 纳入统一合成后对**所有**字段生效；本进程规则在其上逐字段叠加。
#[test]
fn the_wildcard_rule_now_applies_to_every_field() {
    let c = one(r#"
            [[apps]]
            process = "*"
            initial_mode = "english"
            host_drawn_candidates = true
            [[apps]]
            process = "Foo.exe"
            host_drawn_candidates = false
        "#);
    let unknown = c.get_rule("nobody.exe").expect("通配对任何进程都生效");
    assert_eq!(unknown.initial_mode, Some(InitialMode::English));
    let foo = c.get_rule("foo.exe").unwrap();
    assert_eq!(
        foo.initial_mode,
        Some(InitialMode::English),
        "没写的字段从通配继承"
    );
    assert_eq!(
        foo.host_drawn_candidates,
        Some(false),
        "本进程写了的压过通配"
    );
    assert_eq!(foo.process, "Foo.exe", "合成结果的进程名取最具体那条的写法");
    assert_eq!(unknown.process, "*");
}

#[test]
fn get_rule_is_resolve_without_a_window() {
    let c = one(
        "[[apps]]\nprocess = \"a.exe\"\nauto_pair = true\n\n[[apps]]\nprocess = \"a.exe\"\nclass = \"X\"\nauto_pair = false\n",
    );
    assert_eq!(c.get_rule("A.EXE").and_then(|r| r.auto_pair), Some(true));
    assert_eq!(
        c.resolve(&ctx("a.exe", "", ""))
            .rule
            .as_ref()
            .and_then(|r| r.auto_pair),
        Some(true)
    );
    assert_eq!(
        c.resolve(&ctx("a.exe", "x", ""))
            .rule
            .as_ref()
            .and_then(|r| r.auto_pair),
        Some(false)
    );
    assert!(c.get_rule("b.exe").is_none());
}

/// 进入类字段的跨越判据只认「自己写了（或 unset 了）进入类字段」的窗口规则：只管占位、定位的
/// 窗口规则（出厂的 `Chrome_WidgetWin_*`）命中与否，不该让同进程内换窗口重算初始模式。
#[test]
fn resolve_reports_entry_window_matches_only_for_rules_touching_entry_fields() {
    let c = one(r#"
            [[apps]]
            class = "Chrome_WidgetWin_*"
            composition_placeholder = "zwsp"
            [[apps]]
            process = "a.exe"
            class = "Dlg"
            initial_mode = "english"
            [[apps]]
            process = "a.exe"
            class = "U*"
            unset = ["schema"]
            [[apps]]
            process = "a.exe"
            title = "T*"
            initial_punct = "chinese"
            [[apps]]
            process = "a.exe"
            initial_mode = "chinese"
        "#);
    let id = |p: &str, class: &str, title: &str| RuleId {
        process: p.into(),
        class: class.into(),
        title: title.into(),
    };
    let r = c.resolve(&ctx("a.exe", "Chrome_WidgetWin_1", ""));
    assert_eq!(r.window_matches, vec![id("*", "chrome_widgetwin_*", "")]);
    assert!(
        r.entry_window_matches.is_empty(),
        "只管占位的类名规则不算进入类"
    );
    let r = c.resolve(&ctx("a.exe", "Dlg", "Tab"));
    assert_eq!(
        r.entry_window_matches,
        vec![id("a.exe", "dlg", ""), id("a.exe", "", "t*")]
    );
    let r = c.resolve(&ctx("a.exe", "Up", ""));
    assert_eq!(
        r.entry_window_matches,
        vec![id("a.exe", "u*", "")],
        "unset 进入类字段同样改变进入时的结果"
    );
    assert!(
        c.resolve(&ctx("a.exe", "", ""))
            .entry_window_matches
            .is_empty(),
        "纯进程规则不进集合"
    );
}

/// P2 据此判断「命中的带窗口条件的规则集合变了没有」。
#[test]
fn resolve_reports_which_window_rules_matched_in_compose_order() {
    let c = one(
        "[[apps]]\nprocess = \"a.exe\"\ntitle = \"T*\"\nauto_pair = true\n\n[[apps]]\nclass = \"C\"\nauto_pair = false\n\n[[apps]]\nprocess = \"a.exe\"\ncaret_use_top = true\n",
    );
    let r = c.resolve(&ctx("a.exe", "c", "Title"));
    assert_eq!(
        r.window_matches,
        vec![
            RuleId {
                process: "*".into(),
                class: "c".into(),
                title: String::new()
            },
            RuleId {
                process: "a.exe".into(),
                class: String::new(),
                title: "t*".into()
            },
        ]
    );
    assert!(c.resolve(&ctx("a.exe", "", "")).window_matches.is_empty());
}

#[test]
fn host_render_only_takes_concrete_processes_without_window_conditions() {
    let c = one(r#"
            [[apps]]
            process = "*"
            host_render = true
            [[apps]]
            process = "SearchHost.exe"
            host_render = true
            [[apps]]
            process = "Foo.exe"
            class = "X"
            host_render = true
            [[apps]]
            class = "Y"
            host_render = true
        "#);
    assert_eq!(
        c.host_render_processes(),
        vec!["SearchHost.exe".to_string()]
    );
}

#[test]
fn composition_placeholder_goes_through_the_unified_composition() {
    let c = one(
        "[[apps]]\nprocess = \"*\"\ncomposition_placeholder = \"zwsp\"\n\n[[apps]]\nprocess = \"wps.exe\"\ncomposition_placeholder = \"space\"\n\n[[apps]]\nprocess = \"code.exe\"\nauto_pair = true\n",
    );
    assert_eq!(
        c.composition_placeholder_for("x.exe"),
        PlaceholderChar::Zwsp
    );
    assert_eq!(
        c.composition_placeholder_for("code.exe"),
        PlaceholderChar::Zwsp
    );
    assert_eq!(
        c.composition_placeholder_for("WPS.exe"),
        PlaceholderChar::Space
    );
    assert_eq!(
        c.composition_placeholder_for(""),
        PlaceholderChar::Space,
        "进程名未知：保持历史行为"
    );
}

#[test]
fn resolve_results_are_cached_and_the_cache_is_bounded() {
    let c = one("[[apps]]\nclass = \"C*\"\ncaret_use_top = true\n");
    let a = c.resolve(&ctx("a.exe", "Cx", "t"));
    let b = c.resolve(&ctx("A.EXE", "cX", "t"));
    assert!(std::sync::Arc::ptr_eq(&a, &b), "规范化后同键命中缓存");
    for i in 0..200 {
        let class = format!("C{i}");
        assert!(c.resolve(&ctx("a.exe", &class, "t")).rule.is_some());
    }
    assert!(c.resolve_cache_len() <= RESOLVE_CACHE_CAP);
    assert_eq!(
        c.clone().resolve_cache_len(),
        0,
        "克隆（重载）出的表缓存是空的"
    );
}

/// 缓存两级化：标题变化不新增一级条目（按 (进程, 类名) 缓存），命中同一组标题规则的标题
/// 共享同一份结果；一条标题规则都不命中的标题直接拿基底。
#[test]
fn title_changes_reuse_the_class_entry_and_share_results_by_hit_set() {
    let c = one("[[apps]]\nprocess = \"a.exe\"\ncaret_offset_x = 1\n\n\
             [[apps]]\nprocess = \"a.exe\"\ntitle = \"*Doc*\"\ncaret_offset_x = 2\n");
    let d1 = c.resolve(&ctx("a.exe", "Main", "Doc 1"));
    let d2 = c.resolve(&ctx("a.exe", "Main", "doc 2"));
    assert_eq!(d1.rule.as_ref().unwrap().caret_offset_x, 2);
    assert!(
        std::sync::Arc::ptr_eq(&d1, &d2),
        "命中同一组标题规则 ⇒ 同一份结果"
    );
    let o1 = c.resolve(&ctx("a.exe", "Main", "Other 1"));
    let o2 = c.resolve(&ctx("a.exe", "Main", "Other 2"));
    let none = c.resolve(&ctx("a.exe", "Main", ""));
    assert_eq!(o1.rule.as_ref().unwrap().caret_offset_x, 1);
    assert!(std::sync::Arc::ptr_eq(&o1, &o2), "都不命中 ⇒ 基底");
    assert!(std::sync::Arc::ptr_eq(&o1, &none), "空标题同样是基底");
    for i in 0..200 {
        let title = format!("Doc {i}");
        assert_eq!(caret_x(&c, ctx("a.exe", "Main", &title)), 2);
    }
    assert_eq!(c.resolve_cache_len(), 1, "标题轮转不占一级缓存");
}

/// 一级缓存满了淘汰最久没用的那条，而不是整表清空：刚用过的焦点窗口留在缓存里。
#[test]
fn resolve_cache_evicts_the_least_recently_used_entry() {
    let c = one("[[apps]]\nclass = \"C*\"\ncaret_use_top = true\n");
    let first = c.resolve(&ctx("a.exe", "C0", ""));
    let second = c.resolve(&ctx("a.exe", "C1", ""));
    for i in 2..RESOLVE_CACHE_CAP {
        c.resolve(&ctx("a.exe", &format!("C{i}"), ""));
    }
    assert_eq!(c.resolve_cache_len(), RESOLVE_CACHE_CAP);
    // 碰一下 C0，让 C1 成为最久没用的。
    assert!(std::sync::Arc::ptr_eq(
        &first,
        &c.resolve(&ctx("a.exe", "C0", ""))
    ));
    c.resolve(&ctx("a.exe", "Cnew", ""));
    assert_eq!(c.resolve_cache_len(), RESOLVE_CACHE_CAP);
    assert!(
        std::sync::Arc::ptr_eq(&first, &c.resolve(&ctx("a.exe", "C0", ""))),
        "刚用过的留着"
    );
    assert!(
        !std::sync::Arc::ptr_eq(&second, &c.resolve(&ctx("a.exe", "C1", ""))),
        "最久没用的被淘汰"
    );
}

/// 「存在标题规则」开关（推给 DLL 决定采不采标题）：只看启用的 `[[apps]]` 行。
#[test]
fn has_title_rules_counts_only_enabled_app_rules_with_a_title() {
    assert!(
        !one("[[apps]]\nprocess = \"a.exe\"\nclass = \"C\"\nauto_pair = false\n").has_title_rules()
    );
    assert!(one("[[apps]]\ntitle = \"*x*\"\nauto_pair = false\n").has_title_rules());
    assert!(
        !one("[[apps]]\nprocess = \"a.exe\"\ntitle = \"x\"\ndisabled = true\nauto_pair = false\n")
            .has_title_rules(),
        "禁用的不算"
    );
    assert!(
        !one("[[commit_newline]]\nprocess = \"n.exe\"\ntitle = \"x\"\nstyle = \"crlf\"\n")
            .has_title_rules(),
        "附属段的 title 是未知键，不算"
    );
    assert!(!AppCompat::default().has_title_rules());
}

/// 「本进程可能命中标题规则」：不限进程的标题规则对所有进程成立，进程标题规则只对本进程。
#[test]
fn process_may_match_title_follows_the_buckets() {
    let own = one("[[apps]]\nprocess = \"A.exe\"\ntitle = \"x*\"\nauto_pair = false\n");
    assert!(own.process_may_match_title("a.EXE"));
    assert!(!own.process_may_match_title("b.exe"), "别的进程用不上");
    assert!(!own.process_may_match_title(""), "进程名未知");
    let any = one("[[apps]]\ntitle = \"x*\"\nauto_pair = false\n");
    assert!(any.process_may_match_title("b.exe"));
    let off = one(
        "[[apps]]\nprocess = \"a.exe\"\ntitle = \"x*\"\ndisabled = true\nauto_pair = false\n\n\
             [[apps]]\nprocess = \"a.exe\"\nclass = \"C\"\nauto_pair = false\n",
    );
    assert!(
        !off.process_may_match_title("a.exe"),
        "禁用的、只有类名的都不算"
    );
}

/// 标题签名 = 命中的标题规则集合：命中同一组的标题签名相同，不命中的与空标题相同。
#[test]
fn title_signature_is_the_set_of_matching_title_rules() {
    let c = one(
        "[[apps]]\nprocess = \"a.exe\"\ntitle = \"*Doc*\"\ncaret_offset_x = 2\n\n\
             [[apps]]\ntitle = \"*Mail*\"\ncaret_offset_y = 2\n",
    );
    let sig = |t: &str| c.title_signature(&ctx("a.exe", "Main", t));
    assert_eq!(sig("Doc 1"), sig("doc 2"));
    assert_eq!(sig("Other"), sig(""));
    assert!(sig("").is_empty());
    assert_ne!(sig("Doc 1"), sig("Other"));
    assert_ne!(sig("Doc 1"), sig("Doc Mail"), "多命中一条就不同");
    let none = one("[[apps]]\nprocess = \"a.exe\"\nclass = \"Main\"\nauto_pair = false\n");
    assert!(
        none.title_signature(&ctx("a.exe", "Main", "Doc"))
            .is_empty()
    );
}

/// 附属段不支持窗口条件：写了当未知键保留，规则仍按进程名生效。
#[test]
fn side_sections_ignore_window_conditions() {
    let c = one(
        "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclass = \"Nope\"\nclasses = [\"Progman\"]\n\n[[commit_newline]]\nprocess = \"n.exe\"\ntitle = \"x\"\nstyle = \"crlf\"\n",
    );
    assert!(c.initial_mode_applies_to_window("explorer.exe", "Progman"));
    assert_eq!(c.commit_newline_for("n.exe"), Some(NewlineStyle::Crlf));
}

/// 分桶之前的参考合成：全表逐条比，匹配用公开的 `wildcard_match`（每次现折叠）。
fn reference_resolve(c: &AppCompat, ctx: &WindowCtx) -> ResolvedRule {
    let matches = |cand: &&Candidate| {
        let process_ok = cand.id.is_any_process()
            || (!ctx.process.is_empty() && cand.id.process.eq_ignore_ascii_case(ctx.process));
        let cond = |pattern: &str, value: &str| {
            pattern.is_empty() || (!value.is_empty() && wildcard_match(pattern, value))
        };
        process_ok && cond(&cand.id.class, ctx.class) && cond(&cand.id.title, ctx.title)
    };
    compose_candidates(c.candidates.iter().filter(matches))
}

fn snapshot(r: &ResolvedRule) -> (serde_json::Value, Vec<RuleId>, Vec<RuleId>) {
    (
        serde_json::to_value(&r.rule).unwrap(),
        r.window_matches.clone(),
        r.entry_window_matches.clone(),
    )
}

/// 分桶 + 预折叠匹配、按进程分桶的 `get_rule` 查找表，都与全表逐条合成的结果相同。
#[test]
fn bucketed_resolve_and_lookup_table_match_the_full_scan() {
    let c = load(
        [
            "[[apps]]\nprocess = \"*\"\ncaret_offset_x = 1\nauto_pair = true\n\n\
                 [[apps]]\nclass = \"Chrome_*\"\ncaret_offset_x = 2\n\n\
                 [[apps]]\ntitle = \"*Doc*\"\ninitial_mode = \"english\"\n\n\
                 [[apps]]\nprocess = \"A.exe\"\ncaret_offset_x = 3\nunset = [\"auto_pair\"]\n\n\
                 [[apps]]\nprocess = \"a.exe\"\nclass = \"chrome_w?\"\ncaret_offset_y = 4\n\n\
                 [[apps]]\nprocess = \"b.exe\"\nclass = \"*\"\ndisabled = true\ncaret_offset_x = 9\n\n\
                 [[apps]]\nprocess = \"b.exe\"\nfirst_show_mode = \"wait\"\n",
            "[[apps]]\nprocess = \"*\"\nfirst_show_mode = \"fast\"\n\n\
                 [[apps]]\nprocess = \"c.exe\"\nclass = \"Ǆ*\"\nauto_pair = false\n",
            "[[apps]]\nprocess = \"a.exe\"\ncaret_offset_y = 7\n\n\
                 [[apps]]\nclass = \"Chrome_*\"\ntitle = \"x?\"\ncaret_use_top = true\n",
        ],
        "bucketed",
    );
    for process in ["", "a.exe", "A.EXE", "b.exe", "c.exe", "zzz.exe", "*"] {
        for class in ["", "Chrome_W1", "CHROME_WX", "ǆq", "Other"] {
            for title in ["", "xy", "My Doc", "x"] {
                let x = ctx(process, class, title);
                let want = if process.is_empty() {
                    ResolvedRule::default()
                } else {
                    reference_resolve(&c, &x)
                };
                assert_eq!(snapshot(&c.resolve(&x)), snapshot(&want), "{x:?}");
                // 再解析一次：走缓存（一级命中 / 二级按命中集合命中）的结果同样不变。
                assert_eq!(snapshot(&c.resolve(&x)), snapshot(&want), "cached {x:?}");
            }
        }
        if !process.is_empty() {
            let want = reference_resolve(&c, &ctx(process, "", "")).rule;
            assert_eq!(
                serde_json::to_value(c.get_rule(process)).unwrap(),
                serde_json::to_value(want.as_ref()).unwrap(),
                "get_rule({process:?})"
            );
        }
    }
}
