use super::*;
use crate::compat_overlay::parse_raw;
use serde_json::json;

const SYS: &str = "[[apps]]\nprocess = \"ahk.exe\"\nauto_pair = true\n\n[[apps]]\nprocess = \"AHK.exe\"\nclass = \"AutoHotkeyGUI\"\nfirst_show_mode = \"wait\"\ncaret_use_top = true\n\n[[apps]]\nclass = \"Chrome_WidgetWin_*\"\ncomposition_placeholder = \"zwsp\"\n";

fn layers(usr: &str) -> Layers {
    Layers {
        system: parse_raw(SYS).unwrap(),
        user: parse_raw(usr).unwrap(),
        warnings: Vec::new(),
    }
}

fn patch(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn k(p: &str, c: &str, t: &str) -> RuleKey {
    RuleKey::new(p, c, t)
}

#[test]
fn views_expose_conditions_and_a_normalized_key() {
    let l = layers("");
    let v = l
        .view_of(Section::Apps, k("ahk.EXE", " autohotkeygui ", ""))
        .unwrap();
    assert_eq!(
        (v.process.as_str(), v.class.as_str(), v.title.as_str()),
        ("AHK.exe", "AutoHotkeyGUI", ""),
        "原样写法"
    );
    assert_eq!(v.key, "ahk.exe\u{1f}autohotkeygui\u{1f}");
    let any = l
        .view_of(Section::Apps, k("*", "chrome_widgetwin_*", ""))
        .unwrap();
    assert_eq!(any.process, "*");
    assert_eq!(any.key, "*\u{1f}chrome_widgetwin_*\u{1f}");
    let s = serde_json::to_value(&any).unwrap();
    assert_eq!(s["class"], json!("Chrome_WidgetWin_*"));
    assert_eq!(s["title"], json!(""));
    assert!(s["key"].is_string());
    assert!(s.get("id").is_none());
}

#[test]
fn upsert_on_a_system_window_rule_stores_the_diff_under_the_same_identity() {
    let mut l = layers("");
    l.upsert(
        Section::Apps,
        k("ahk.exe", "autohotkeygui", ""),
        &patch(json!({"first_show_mode": "instant"})),
    )
    .unwrap();
    assert_eq!(l.user.apps.len(), 1, "{:?}", l.user.apps);
    let row = &l.user.apps[0];
    assert_eq!(row["process"], json!("AHK.exe"), "沿用系统层写法");
    assert_eq!(row["class"], json!("AutoHotkeyGUI"));
    assert_eq!(row["first_show_mode"], json!("instant"));
    assert!(row.get("caret_use_top").is_none(), "只记差异");
    let v = l
        .view_of(Section::Apps, k("ahk.exe", "AutoHotkeyGUI", ""))
        .unwrap();
    assert_eq!(v.state, RuleState::Modified);
    assert_eq!(
        l.view_of(Section::Apps, "ahk.exe").unwrap().state,
        RuleState::System,
        "纯进程那条没被碰"
    );
}

#[test]
fn upsert_creates_a_window_only_user_rule_without_a_process_key() {
    let mut l = layers("");
    l.upsert(
        Section::Apps,
        k("*", "MozillaWindowClass", ""),
        &patch(json!({"composition_placeholder": "zwsp"})),
    )
    .unwrap();
    let row = l.user.apps.last().unwrap();
    assert!(row.get("process").is_none(), "{row:?}");
    assert_eq!(row["class"], json!("MozillaWindowClass"));
    let v = l
        .view_of(Section::Apps, k("", "mozillawindowclass", ""))
        .unwrap();
    assert_eq!(v.state, RuleState::User);
    assert_eq!(v.process, "*");
}

#[test]
fn disable_reset_field_and_reset_address_the_window_rule_only() {
    let mut l = layers("");
    let key = k("ahk.exe", "AutoHotkeyGUI", "");
    l.set_disabled(Section::Apps, &key, true).unwrap();
    assert_eq!(
        l.view_of(Section::Apps, &key).unwrap().state,
        RuleState::Disabled
    );
    assert_eq!(
        l.view_of(Section::Apps, "ahk.exe").unwrap().state,
        RuleState::System
    );
    l.upsert(
        Section::Apps,
        &key,
        &patch(json!({"first_show_mode": null})),
    )
    .unwrap();
    l.reset_field(Section::Apps, &key, "first_show_mode")
        .unwrap();
    assert!(
        l.user.apps[0].get("unset").is_none(),
        "{:?}",
        l.user.apps[0]
    );
    assert!(l.reset(Section::Apps, &key));
    assert!(l.user.apps.is_empty());
    assert!(
        l.set_disabled(Section::Apps, k("ahk.exe", "Nope", ""), true)
            .is_err(),
        "不存在的窗口规则"
    );
}

#[test]
fn rename_changes_conditions_of_a_user_rule_in_place() {
    let mut l =
        layers("[[apps]]\nprocess = \"x.exe\"\nclass = \"A\"\nauto_pair = true\ncomment = \"c\"\n");
    l.rename(Section::Apps, k("x.exe", "a", ""), k("X.exe", "B*", "T?"))
        .unwrap();
    assert_eq!(l.user.apps.len(), 1);
    let row = &l.user.apps[0];
    assert_eq!(row["process"], json!("X.exe"));
    assert_eq!(row["class"], json!("B*"));
    assert_eq!(row["title"], json!("T?"));
    assert_eq!(row["auto_pair"], json!(true), "字段保留");
    assert_eq!(row["comment"], json!("c"));
    assert!(l.view_of(Section::Apps, k("x.exe", "A", "")).is_none());

    // 去掉全部窗口条件、改成不限进程 + 类名，都是合法的改法。
    l.rename(Section::Apps, k("x.exe", "b*", "t?"), k("*", "B*", ""))
        .unwrap();
    let row = &l.user.apps[0];
    assert!(
        row.get("process").is_none() && row.get("title").is_none(),
        "{row:?}"
    );
    // 只改写法（大小写 / 空白）也行。
    l.rename(Section::Apps, k("*", "b*", ""), k("*", " b* ", ""))
        .unwrap();
    assert_eq!(l.user.apps[0]["class"], json!("b*"));
}

#[test]
fn rename_refuses_factory_rules_collisions_and_bad_targets() {
    let mut l = layers(
        "[[apps]]\nprocess = \"AHK.exe\"\nclass = \"AutoHotkeyGUI\"\nauto_pair = false\n\n[[apps]]\nprocess = \"x.exe\"\nauto_pair = true\n",
    );
    let before = l.user.clone();
    let err = l
        .rename(
            Section::Apps,
            k("ahk.exe", "AutoHotkeyGUI", ""),
            k("ahk.exe", "Other", ""),
        )
        .unwrap_err();
    assert!(
        err.contains("复制为新规则"),
        "出厂规则（含被改过的）: {err}"
    );
    let err = l
        .rename(Section::Apps, "x.exe", k("ahk.exe", "", ""))
        .unwrap_err();
    assert!(err.contains("已有"), "撞上出厂规则: {err}");
    assert!(
        l.rename(Section::Apps, "x.exe", k("", "", "")).is_err(),
        "三者全空"
    );
    assert!(
        l.rename(Section::Apps, "x.exe", k("x.exe", "a\u{7}", ""))
            .is_err(),
        "控制字符"
    );
    assert!(
        l.rename(Section::Apps, "nobody.exe", "y.exe").is_err(),
        "不存在"
    );
    assert_eq!(l.user, before, "被拒绝时不改任何东西");
}

#[test]
fn copy_turns_the_effective_content_into_a_new_user_rule() {
    let mut l = layers(
        "[[apps]]\nprocess = \"AHK.exe\"\nclass = \"AutoHotkeyGUI\"\nfirst_show_mode = \"instant\"\ndisabled = true\n",
    );
    l.copy(
        Section::Apps,
        k("ahk.exe", "AutoHotkeyGUI", ""),
        k("ahk.exe", "AutoHotkeyGUI2", ""),
    )
    .unwrap();
    let v = l
        .view_of(Section::Apps, k("ahk.exe", "AutoHotkeyGUI2", ""))
        .unwrap();
    assert_eq!(v.state, RuleState::User, "新规则是用户新增、启用的");
    assert_eq!(
        v.effective["first_show_mode"],
        json!("instant"),
        "带上用户差异"
    );
    assert_eq!(v.effective["caret_use_top"], json!(true), "带上出厂字段");
    assert_eq!(
        l.view_of(Section::Apps, k("ahk.exe", "AutoHotkeyGUI", ""))
            .unwrap()
            .state,
        RuleState::Disabled,
        "源规则原样"
    );
    let err = l
        .copy(Section::Apps, "ahk.exe", k("ahk.exe", "AutoHotkeyGUI2", ""))
        .unwrap_err();
    assert!(err.contains("已有"), "{err}");
}

#[test]
fn key_validation() {
    assert!(
        validate_key(Section::Apps, &k("*", "", "")).is_ok(),
        "* = 所有应用"
    );
    assert!(validate_key(Section::Apps, &k("", "X", "")).is_ok());
    assert!(validate_key(Section::Apps, &k(" ", " ", "")).is_err());
    assert!(validate_key(Section::Apps, &k("a/b.exe", "", "")).is_err());
    assert!(validate_key(Section::Apps, &k("a.exe", "x\ny", "")).is_err());
    let long = "x".repeat(MAX_PATTERN_CHARS);
    assert!(validate_key(Section::Apps, &k("a.exe", &long, "")).is_ok());
    let longer = "字".repeat(MAX_PATTERN_CHARS + 1);
    assert!(validate_key(Section::Apps, &k("a.exe", "", &longer)).is_err());
    assert!(validate_key(Section::CommitNewline, &k("a.exe", "", "")).is_ok());
    assert!(validate_key(Section::CommitNewline, &k("*", "", "")).is_err());
    assert!(validate_key(Section::InitialModeScope, &k("a.exe", "X", "")).is_err());
    assert_eq!(
        validate_key(Section::Apps, &k(" a.exe ", " X ", " T ")).unwrap(),
        k("a.exe", "X", "T"),
        "trim"
    );
}

#[test]
fn problems_and_import_items_carry_the_conditions() {
    let (_, report) = crate::app_compat::sanitize_raw(
        &parse_raw("[[apps]]\nprocess = \"a.exe\"\nclass = \"K\"\nfirst_show_mode = \"bogus\"\n")
            .unwrap(),
    );
    assert_eq!(report.len(), 1);
    assert_eq!(
        (report[0].class.as_str(), report[0].title.as_str()),
        ("K", "")
    );
    let v = serde_json::to_value(&report[0]).unwrap();
    assert_eq!(v["class"], json!("K"));

    let l = layers("[[apps]]\nprocess = \"x.exe\"\ntitle = \"T\"\nauto_pair = true\n");
    let p = l
        .import_preview(
            "[[apps]]\nclass = \"New*\"\nauto_pair = true\n\n[[apps]]\nprocess = \"y.exe\"\nclass = \"bad\\u0007\"\nauto_pair = true\n\n[[commit_newline]]\nprocess = \"*\"\nstyle = \"cr\"\n",
            ImportMode::Replace,
        )
        .unwrap();
    let items: Vec<_> = p
        .items
        .iter()
        .map(|i| {
            (
                i.process.as_str(),
                i.class.as_str(),
                i.title.as_str(),
                i.action,
            )
        })
        .collect();
    assert_eq!(
        items,
        vec![("*", "New*", "", "add"), ("x.exe", "", "T", "remove")]
    );
    assert_eq!(p.rejected.len(), 2, "{:?}", p.rejected);
}

fn rpc_dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "wind_compat_p4_{tag}_{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let (d, u) = (base.join("data"), base.join("user"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::create_dir_all(&u).unwrap();
    std::fs::write(d.join(COMPAT_FILE_NAME), SYS).unwrap();
    (d, u)
}

#[test]
fn rpc_round_trip_with_window_conditions() {
    let (d, u) = rpc_dirs("rt");
    let call = |m: &str, p: Value| rpc(m, &p, Some(&d), None, Some(&u));

    let list = call("compat.list", json!({"section": "apps"}))
        .unwrap()
        .value;
    let rules = list["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 3, "窗口规则如实列出");
    assert!(
        rules
            .iter()
            .any(|r| r["class"] == json!("Chrome_WidgetWin_*"))
    );

    let out = call(
        "compat.upsert",
        json!({"section": "apps", "class": "Foo*", "patch": {"auto_pair": true}}),
    )
    .unwrap();
    assert!(out.wrote);
    assert_eq!(out.value["rule"]["process"], json!("*"));
    assert_eq!(out.value["rule"]["class"], json!("Foo*"));

    let out = call(
        "compat.rename",
        json!({"section": "apps", "process": "*", "class": "foo*", "to": {"process": "z.exe", "class": "Foo*", "title": "Doc*"}}),
    )
    .unwrap();
    assert!(out.wrote);
    assert_eq!(out.value["rule"]["title"], json!("Doc*"));
    let Err(err) = call(
        "compat.rename",
        json!({"section": "apps", "process": "ahk.exe", "to": {"process": "q.exe"}}),
    ) else {
        panic!("出厂规则不能改条件");
    };
    assert!(err.contains("复制为新规则"), "{err}");

    let out = call(
        "compat.copy",
        json!({"section": "apps", "process": "ahk.exe", "class": "AutoHotkeyGUI", "to": {"process": "ahk.exe", "class": "Other"}}),
    )
    .unwrap();
    assert_eq!(out.value["rule"]["state"], json!("user"));
    assert_eq!(
        out.value["rule"]["effective"]["first_show_mode"],
        json!("wait")
    );

    let out = call(
        "compat.setDisabled",
        json!({"section": "apps", "process": "AHK.EXE", "class": "autohotkeygui", "disabled": true}),
    )
    .unwrap();
    assert_eq!(out.value["rule"]["state"], json!("disabled"));
    assert!(
        call(
            "compat.rename",
            json!({"section": "apps", "process": "z.exe"})
        )
        .is_err(),
        "缺 to"
    );
    assert!(
        call("compat.upsert", json!({"section": "apps", "patch": {}})).is_err(),
        "缺身份"
    );
    assert!(
        call(
            "compat.upsert",
            json!({"section": "apps", "process": "a.exe", "class": 1, "patch": {}})
        )
        .is_err(),
        "类型不对"
    );
    let text = std::fs::read_to_string(u.join(COMPAT_FILE_NAME)).unwrap();
    assert!(text.contains("title = \"Doc*\""), "{text}");
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

/// 复制保真：源规则带跨级的 `unset` 与显式 `false`，复制品在同一级、同一窗口上解析结果一致。
#[test]
fn copy_keeps_unset_and_explicit_false_so_the_copy_resolves_the_same() {
    use crate::app_compat::{AppCompat, WindowCtx};
    let sys = "[[apps]]\nprocess = \"ahk.exe\"\ncaret_use_top = true\nfirst_show_mode = \"fast\"\n\n[[apps]]\nprocess = \"ahk.exe\"\nclass = \"A\"\ncaret_use_top = false\nunset = [\"first_show_mode\"]\n";
    let mut l = Layers {
        system: parse_raw(sys).unwrap(),
        user: Raw::default(),
        warnings: Vec::new(),
    };
    l.copy(Section::Apps, k("ahk.exe", "A", ""), k("ahk.exe", "B", ""))
        .unwrap();
    let row = l.user.apps.last().unwrap();
    assert_eq!(row["caret_use_top"], json!(false), "{row:?}");
    assert_eq!(row["unset"], json!(["first_show_mode"]), "{row:?}");

    let base = std::env::temp_dir().join(format!(
        "wind_compat_p4_copy_{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let (d, u) = (base.join("d"), base.join("u"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::create_dir_all(&u).unwrap();
    std::fs::write(d.join(COMPAT_FILE_NAME), sys).unwrap();
    std::fs::write(u.join(COMPAT_FILE_NAME), l.render_user().unwrap()).unwrap();
    let c = AppCompat::load_layered(Some(&d), None, Some(&u));
    let _ = std::fs::remove_dir_all(&base);
    let at = |class: &str| {
        let r = c.resolve(&WindowCtx {
            process: "ahk.exe",
            class,
            title: "",
        });
        let r = r.rule.clone().unwrap();
        (r.caret_use_top, r.first_show_mode)
    };
    assert_eq!(at("B"), at("A"), "复制品与源规则效果相同");
    assert_eq!(at("A"), (false, None));
}

/// 现存的不合规身份（手写超长 / 控制字符、附属段 `*`）能改、能禁用、能改名成合规的；
/// 只有写出新身份的一侧才校验。
#[test]
fn existing_non_conforming_rules_are_editable_and_only_new_identities_are_validated() {
    let long = "x".repeat(MAX_PATTERN_CHARS + 10);
    let usr = format!(
        "[[apps]]\nprocess = \"a.exe\"\nclass = \"{long}\"\nauto_pair = true\n\n[[commit_newline]]\nprocess = \"*\"\nstyle = \"cr\"\n"
    );
    let mut l = layers(&usr);
    let bad = k("a.exe", &long, "");
    l.upsert(Section::Apps, &bad, &patch(json!({"caret_use_top": true})))
        .expect("现存行可改");
    l.set_disabled(Section::Apps, &bad, true).expect("可禁用");
    l.reset_field(Section::Apps, &bad, "caret_use_top")
        .expect("可还原字段");
    l.set_disabled(Section::CommitNewline, "*", true)
        .expect("附属段的 * 行可禁用");
    assert!(
        l.rename(Section::Apps, &bad, k("a.exe", "x\u{7}", ""))
            .is_err(),
        "to 仍要校验"
    );
    l.rename(Section::Apps, &bad, k("a.exe", "Short", ""))
        .expect("能改名成合规的");
    assert!(l.view_of(Section::Apps, k("a.exe", "short", "")).is_some());
    assert!(
        l.upsert(
            Section::Apps,
            k("b.exe", &long, ""),
            &patch(json!({"auto_pair": true}))
        )
        .is_err(),
        "新建仍校验"
    );
}

#[test]
fn rpc_to_must_be_an_object() {
    let (d, u) = rpc_dirs("to");
    let Err(e) = rpc(
        "compat.rename",
        &json!({"section": "apps", "process": "ahk.exe", "to": "x.exe"}),
        Some(&d),
        None,
        Some(&u),
    ) else {
        panic!("to 不是对象应报错");
    };
    assert!(e.contains("to 必须是对象"), "{e}");
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}
