use super::*;
use serde_json::json;

fn obj(v: Value) -> Obj {
    v.as_object().cloned().expect("测试夹具必须是对象")
}

#[test]
fn layer_fields_override_and_unwritten_fields_are_inherited() {
    let base = obj(json!({"process": "A.exe", "caret_use_top": true, "stale_probe_guard": true}));
    let layer = obj(json!({"process": "a.exe", "first_show_mode": "wait"}));
    let out = compose(&base, &layer);
    assert_eq!(out["caret_use_top"], json!(true), "没写的字段必须继承");
    assert_eq!(out["stale_probe_guard"], json!(true));
    assert_eq!(out["first_show_mode"], json!("wait"));
    assert_eq!(out["process"], json!("A.exe"), "保留底层的写法");
}

#[test]
fn explicit_false_overrides_a_true_below() {
    let base = obj(json!({"process": "A.exe", "caret_use_top": true}));
    let layer = obj(json!({"process": "A.exe", "caret_use_top": false}));
    assert_eq!(compose(&base, &layer)["caret_use_top"], json!(false));
}

#[test]
fn unset_removes_a_field_from_below_and_is_carried_upwards() {
    let base = obj(json!({"process": "A.exe", "first_show_mode": "wait", "auto_pair": true}));
    let layer = obj(json!({"process": "A.exe", "unset": ["first_show_mode"]}));
    let out = compose(&base, &layer);
    assert!(out.get("first_show_mode").is_none());
    assert_eq!(out["auto_pair"], json!(true));
    assert_eq!(
        out["unset"],
        json!(["first_show_mode"]),
        "累积上去，更高层还要读到"
    );
}

#[test]
fn a_higher_layer_can_set_a_field_that_a_lower_layer_unset() {
    let mid = obj(json!({"process": "A.exe", "unset": ["first_show_mode"]}));
    let top = obj(json!({"process": "A.exe", "first_show_mode": "fast"}));
    let out = compose(&mid, &top);
    assert_eq!(out["first_show_mode"], json!("fast"));
    assert!(
        out.get("unset").is_none(),
        "被重新设置的字段要从 unset 里拿掉"
    );
}

#[test]
fn writing_and_unsetting_the_same_field_in_one_entry_means_set() {
    let base = obj(json!({"process": "A.exe", "first_show_mode": "wait"}));
    let layer =
        obj(json!({"process": "A.exe", "first_show_mode": "fast", "unset": ["first_show_mode"]}));
    let out = compose(&base, &layer);
    assert_eq!(out["first_show_mode"], json!("fast"));
    assert!(out.get("unset").is_none());
}

#[test]
fn overlay_is_per_process_case_insensitive_and_appends_new_ones() {
    let base = vec![
        obj(json!({"process": "A.exe", "auto_pair": true})),
        obj(json!({"process": "B.exe", "auto_pair": true})),
    ];
    let layer = vec![
        obj(json!({"process": " a.EXE ", "caret_use_top": true})),
        obj(json!({"process": "C.exe", "auto_pair": false})),
    ];
    let out = overlay("apps", base, &layer);
    assert_eq!(out.len(), 3);
    assert_eq!(out[0]["auto_pair"], json!(true));
    assert_eq!(out[0]["caret_use_top"], json!(true));
    assert_eq!(process_of(&out[2]), "C.exe");
}

#[test]
fn rows_without_a_usable_process_are_dropped() {
    let layer = vec![
        obj(json!({"auto_pair": true})),
        obj(json!({"process": 1})),
        obj(json!({"process": "  "})),
    ];
    assert!(overlay("apps", Vec::new(), &layer).is_empty());
}

#[test]
fn disabled_is_an_ordinary_overlaid_field() {
    let base = obj(json!({"process": "A.exe", "disabled": true, "auto_pair": true}));
    let enabled = compose(&base, &obj(json!({"process": "A.exe", "disabled": false})));
    assert!(!is_disabled(&enabled), "更高层用 disabled = false 重新启用");
    let stays = compose(
        &base,
        &obj(json!({"process": "A.exe", "caret_use_top": true})),
    );
    assert!(is_disabled(&stays), "没写 disabled 就继承下层的禁用");
}

#[test]
fn parse_raw_keeps_explicit_false_and_reads_all_three_sections() {
    let raw = parse_raw(
        "[[apps]]\nprocess = \"a.exe\"\ncaret_use_top = false\n\n[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"X\"]\n\n[[commit_newline]]\nprocess = \"w.exe\"\nstyle = \"cr\"\n",
    )
    .unwrap();
    assert_eq!(
        raw.apps[0]["caret_use_top"],
        json!(false),
        "显式 false 必须保留"
    );
    assert_eq!(raw.initial_mode_scope[0]["classes"], json!(["X"]));
    assert_eq!(raw.commit_newline[0]["style"], json!("cr"));
}

#[test]
fn parse_raw_reports_syntax_errors_with_a_position() {
    let err = parse_raw("[[apps\nprocess=").unwrap_err();
    assert!(err.contains("line"), "{err}");
}

#[test]
fn materialize_skips_disabled_and_strips_unset_and_isolates_bad_rows() {
    use crate::app_compat::AppCompatRule;
    let rows = vec![
        obj(json!({"process": "a.exe", "caret_use_top": true, "unset": ["x"]})),
        obj(json!({"process": "b.exe", "disabled": true, "caret_use_top": true})),
    ];
    let rules: Vec<AppCompatRule> = materialize(&rows);
    assert_eq!(rules.len(), 1);
    assert!(rules[0].caret_use_top);

    use crate::app_compat::InitialModeScopeRule;
    let scopes: Vec<InitialModeScopeRule> = materialize(&[
        obj(json!({"process": "ok.exe", "classes": ["A"]})),
        obj(json!({"process": "bad.exe", "classes": "not-a-list"})),
    ]);
    assert_eq!(scopes.len(), 1, "结构错的行只丢它自己");
    assert_eq!(scopes[0].process, "ok.exe");
}

// ───────────── 写入侧 ─────────────

use crate::app_compat::AppCompatRule;

fn sys_wechat() -> Obj {
    obj(json!({"process": "Weixin.exe", "caret_use_top": true, "stale_probe_guard": true}))
}

#[test]
fn set_on_a_system_rule_stores_only_the_diff() {
    let sys = vec![sys_wechat()];
    let mut user = Vec::new();
    apply_edits::<AppCompatRule>(
        "apps",
        &sys,
        &mut user,
        "weixin.exe",
        &[FieldEdit::Set("first_show_mode".into(), json!("instant"))],
        true,
    );
    assert_eq!(user.len(), 1);
    assert_eq!(user[0]["first_show_mode"], json!("instant"));
    assert!(
        user[0].get("caret_use_top").is_none(),
        "只记差异，不拷贝系统字段: {:?}",
        user[0]
    );
    assert_eq!(user[0]["process"], json!("Weixin.exe"), "沿用系统层的写法");
}

#[test]
fn a_value_equal_to_the_system_one_is_redundant_and_the_row_disappears() {
    let sys = vec![sys_wechat()];
    let mut user = Vec::new();
    apply_edits::<AppCompatRule>(
        "apps",
        &sys,
        &mut user,
        "Weixin.exe",
        &[FieldEdit::Set("caret_use_top".into(), json!(true))],
        true,
    );
    assert!(user.is_empty(), "与系统一致就不该有用户条目: {user:?}");
}

/// 后半句 P4 改口径：没有同身份系统行的显式 false 也保留（跨级压过 `*` 等更低级规则）。
#[test]
fn explicit_false_is_kept_over_a_system_true_and_on_a_user_rule() {
    let sys = vec![sys_wechat()];
    let mut user = Vec::new();
    apply_edits::<AppCompatRule>(
        "apps",
        &sys,
        &mut user,
        "Weixin.exe",
        &[FieldEdit::Set("caret_use_top".into(), json!(false))],
        true,
    );
    assert_eq!(
        user[0]["caret_use_top"],
        json!(false),
        "显式关掉出厂打开的开关"
    );

    let mut user2 = Vec::new();
    apply_edits::<AppCompatRule>(
        "apps",
        &[],
        &mut user2,
        "New.exe",
        &[FieldEdit::Set("caret_use_top".into(), json!(false))],
        true,
    );
    assert_eq!(
        user2[0]["caret_use_top"],
        json!(false),
        "用户新增规则的显式 false 不是冗余：它压过 * 等更低级规则"
    );
}

#[test]
fn clear_writes_unset_only_when_the_system_layer_sets_that_field() {
    let sys = vec![obj(
        json!({"process": "EXCEL.EXE", "first_show_mode": "wait"}),
    )];
    let mut user = Vec::new();
    apply_edits::<AppCompatRule>(
        "apps",
        &sys,
        &mut user,
        "excel.exe",
        &[FieldEdit::Clear("first_show_mode".into())],
        true,
    );
    assert_eq!(user[0]["unset"], json!(["first_show_mode"]));

    let mut user2 = Vec::new();
    apply_edits::<AppCompatRule>(
        "apps",
        &sys,
        &mut user2,
        "excel.exe",
        &[FieldEdit::Clear("auto_pair".into())],
        true,
    );
    assert!(
        user2.is_empty(),
        "系统没设的字段清除是空操作，不该留噪声条目: {user2:?}"
    );
}

#[test]
fn clear_with_unknown_system_layer_keeps_the_unset_marker() {
    let mut user = Vec::new();
    apply_edits::<AppCompatRule>(
        "apps",
        &[],
        &mut user,
        "x.exe",
        &[FieldEdit::Clear("first_show_mode".into())],
        false,
    );
    assert_eq!(
        user[0]["unset"],
        json!(["first_show_mode"]),
        "读不到系统层时不能凭「没有」丢掉 unset"
    );
}

#[test]
fn setting_again_after_clear_removes_the_unset_marker() {
    let sys = vec![obj(
        json!({"process": "EXCEL.EXE", "first_show_mode": "wait"}),
    )];
    let mut user = Vec::new();
    apply_edits::<AppCompatRule>(
        "apps",
        &sys,
        &mut user,
        "excel.exe",
        &[FieldEdit::Clear("first_show_mode".into())],
        true,
    );
    apply_edits::<AppCompatRule>(
        "apps",
        &sys,
        &mut user,
        "excel.exe",
        &[FieldEdit::Set("first_show_mode".into(), json!("fast"))],
        true,
    );
    assert_eq!(user[0]["first_show_mode"], json!("fast"));
    assert!(user[0].get("unset").is_none());
}

#[test]
fn editing_one_field_never_disturbs_the_others_in_the_user_row() {
    let sys = vec![sys_wechat()];
    let mut user = vec![obj(
        json!({"process": "Weixin.exe", "auto_pair": true, "unknown_future_key": 1}),
    )];
    apply_edits::<AppCompatRule>(
        "apps",
        &sys,
        &mut user,
        "Weixin.exe",
        &[FieldEdit::Set("first_show_mode".into(), json!("fast"))],
        true,
    );
    assert_eq!(user[0]["auto_pair"], json!(true));
    assert_eq!(
        user[0]["unknown_future_key"],
        json!(1),
        "认不出的键要原样保留"
    );
}

#[test]
fn render_orders_keys_and_roundtrips_through_the_parser() {
    let raw = Raw {
        apps: vec![obj(json!({
            "first_show_mode": "wait", "process": "A.exe", "comment": "带\"引号\"和中文",
            "caret_use_top": false, "unset": ["auto_pair"], "unknown_z": 1, "classes_like": ["a", "b"]
        }))],
        ..Default::default()
    };
    let text = render_raw("# header\n\n", &raw).unwrap();
    let process_at = text.find("process =").unwrap();
    let mode_at = text.find("first_show_mode =").unwrap();
    assert!(process_at < mode_at, "process 必须排在字段前: {text}");
    let back = parse_raw(&text).unwrap();
    assert_eq!(
        back.apps[0], raw.apps[0],
        "渲染再解析必须与原始键值一致: {text}"
    );
    assert_eq!(
        back.apps[0]["caret_use_top"],
        json!(false),
        "显式 false 必须写出来"
    );
}

// ───────────── 审查（第二轮）补的回归 ─────────────

#[test]
fn parse_raw_preserves_unknown_sections_and_flags_unreadable_content() {
    let raw = parse_raw("[future]\nk = 1\n\n[apps]\nprocess = \"x.exe\"\n\n[[commit_newline]]\nprocess = \"w.exe\"\n").unwrap();
    assert_eq!(
        raw.extra["future"]["k"].as_integer(),
        Some(1),
        "不认识的顶层段原样保留"
    );
    assert_eq!(
        raw.skipped.len(),
        1,
        "`[apps]` 单括号读不进来，必须被记下: {:?}",
        raw.skipped
    );
    assert!(raw.apps.is_empty());
    assert_eq!(raw.commit_newline.len(), 1);
    let raw = parse_raw("apps = [1, 2]\n").unwrap();
    assert!(!raw.skipped.is_empty(), "数组里不是表的元素也要记下");
}

#[test]
fn parse_raw_merges_duplicate_rows_of_one_process_in_order() {
    let raw = parse_raw(
        "[[apps]]\nprocess = \"x.exe\"\na = 1\nb = 2\n\n[[apps]]\nprocess = \" X.EXE \"\na = 9\n",
    )
    .unwrap();
    assert_eq!(raw.apps.len(), 1);
    assert_eq!(raw.apps[0]["a"], json!(9), "后写的赢");
    assert_eq!(raw.apps[0]["b"], json!(2), "先写的其它字段保留");
}

#[test]
fn datetime_values_survive_as_text_and_the_file_stays_valid() {
    let raw = parse_raw("[[apps]]\nprocess = \"x.exe\"\nwhen = 2020-01-02T03:04:05Z\n").unwrap();
    assert_eq!(raw.apps[0]["when"], json!("2020-01-02T03:04:05Z"));
    assert!(render_raw("", &raw).is_ok(), "写回必须仍是合法 TOML");
}

/// 渲染结果自检：内存里的内容无法无损写成 TOML 时必须拒绝落盘，而不是写一份失真的文件。
#[test]
fn render_refuses_content_it_cannot_write_faithfully() {
    let mut row = Obj::new();
    row.insert("process".into(), json!("x.exe"));
    row.insert("bad".into(), Value::Null);
    let raw = Raw {
        apps: vec![row],
        ..Default::default()
    };
    assert!(render_raw("", &raw).is_err());
}

#[test]
fn render_quotes_keys_that_are_not_bare() {
    let mut row = Obj::new();
    row.insert("process".into(), json!("x.exe"));
    row.insert("备注".into(), json!("说明"));
    row.insert("a.b".into(), json!(1));
    row.insert("with space".into(), json!(true));
    let raw = Raw {
        apps: vec![row],
        ..Default::default()
    };
    let text = render_raw("", &raw).unwrap();
    assert_eq!(parse_raw(&text).unwrap().apps, raw.apps, "{text}");
}

#[test]
fn sanitize_drops_invalid_values_and_reports_them_but_keeps_unknown_keys() {
    use crate::app_compat::AppCompatRule;
    let rows = vec![obj(json!({
        "process": "a.exe", "first_show_mode": "wiat", "auto_pair": true, "caret_offset_x": "12",
        "future_key": [1, 2], "disabled": "yes", "unset": "auto_pair"
    }))];
    let (clean, report) = sanitize::<AppCompatRule>("apps", &rows);
    let c = &clean[0];
    assert!(
        c.get("first_show_mode").is_none()
            && c.get("caret_offset_x").is_none()
            && c.get("disabled").is_none()
    );
    assert_eq!(c["auto_pair"], json!(true));
    assert_eq!(c["future_key"], json!([1, 2]), "认不出的键不归清理管");
    assert_eq!(
        c["unset"],
        json!(["auto_pair"]),
        "单个字符串形态的 unset 归一成数组"
    );
    assert_eq!(report.len(), 3, "{report:?}");
    assert_eq!(rows[0]["first_show_mode"], json!("wiat"), "原始行不被改动");
}
