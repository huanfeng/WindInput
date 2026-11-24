use super::*;
use crate::compat_overlay::parse_raw;
use serde_json::json;

fn layers(sys: &str, usr: &str) -> Layers {
    Layers {
        system: parse_raw(sys).expect("测试夹具必须是合法 TOML"),
        user: parse_raw(usr).expect("测试夹具必须是合法 TOML"),
        warnings: Vec::new(),
    }
}

fn patch(v: Value) -> Map<String, Value> {
    v.as_object().cloned().expect("补丁必须是对象")
}

fn state_of(l: &Layers, p: &str) -> RuleState {
    l.view_of(Section::Apps, p).expect("规则应存在").state
}

fn user_row(l: &Layers, sec: Section, p: &str) -> Option<Obj> {
    rows(&l.user, sec)
        .iter()
        .find(|r| rule_id(sec.as_str(), r) == Some(RuleId::process_only(p)))
        .cloned()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("wind_compat_adm_{tag}_{}", std::process::id()))
}

const SYS: &str = "\
[[apps]]
process = \"Feishu.exe\"
composition_start_pair_guard = true

[[apps]]
process = \"Weixin.exe\"
caret_use_top = true
stale_probe_guard = true

[[apps]]
process = \"Dota.exe\"
host_render = true
";

const USER_A: &str = "[[apps]]\nprocess = \"Weixin.exe\"\nauto_pair = true\n";

// ───────────── 视图 ─────────────

#[test]
fn view_marks_system_modified_user_disabled() {
    let usr = "\
[[apps]]
process = \"Weixin.exe\"
first_show_mode = \"wait\"

[[apps]]
process = \"Dota.exe\"
disabled = true

[[apps]]
process = \"Mine.exe\"
auto_pair = true
";
    let l = layers(SYS, usr);
    assert_eq!(state_of(&l, "Feishu.exe"), RuleState::System);
    assert_eq!(state_of(&l, "Weixin.exe"), RuleState::Modified);
    assert_eq!(state_of(&l, "Dota.exe"), RuleState::Disabled);
    assert_eq!(state_of(&l, "Mine.exe"), RuleState::User);
}

#[test]
fn overridden_lists_only_the_meaningful_diff_including_unset() {
    let usr = "[[apps]]\nprocess = \"Weixin.exe\"\nauto_pair = true\ncaret_use_top = true\nunset = [\"stale_probe_guard\"]\n";
    let l = layers(SYS, usr);
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert_eq!(
        v.overridden,
        vec!["auto_pair".to_string(), "stale_probe_guard".to_string()],
        "caret_use_top 与系统一致是冗余，不算差异；unset 的字段算差异"
    );
    assert!(v.system.is_some());
    assert!(
        v.effective.get("stale_probe_guard").is_none(),
        "unset 后运行时没有它"
    );
    assert_eq!(v.effective["caret_use_top"], json!(true), "没提到的继承");
}

#[test]
fn user_only_rule_reports_all_its_fields_as_overridden() {
    let l = layers(
        SYS,
        "[[apps]]\nprocess = \"Mine.exe\"\nauto_pair = true\ncaret_use_top = true\n",
    );
    let v = l.view_of(Section::Apps, "Mine.exe").unwrap();
    assert_eq!(
        v.overridden,
        vec!["auto_pair".to_string(), "caret_use_top".to_string()]
    );
    assert!(v.system.is_none());
}

/// 旧文件里与系统层完全一致的冗余条目：状态是 System，但用户层里确实有这一条（还原按钮据此判断）。
#[test]
fn a_redundant_user_entry_is_reported_as_system_but_still_has_a_user_entry() {
    let l = layers(
        SYS,
        "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_top = true\n",
    );
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert_eq!(v.state, RuleState::System);
    assert!(v.has_user_entry);
    assert!(v.overridden.is_empty());
}

#[test]
fn view_exposes_whether_a_user_entry_exists_and_its_raw_content() {
    let mut l = layers(SYS, "");
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert!(!v.has_user_entry && v.user.is_none());
    l.upsert(
        Section::Apps,
        "Weixin.exe",
        &patch(json!({"auto_pair": true})),
    )
    .unwrap();
    l.set_disabled(Section::Apps, "Weixin.exe", true).unwrap();
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert_eq!(v.state, RuleState::Disabled);
    let user = v.user.expect("禁用状态下要能看到用户层原始条目");
    assert_eq!(user["auto_pair"], json!(true), "禁用前的改写要可见");
    assert_eq!(user["disabled"], json!(true));
}

// ───────────── 差异写入 ─────────────

/// 整体原则的核心：改一个字段只写这一个字段，系统规则的其余字段继续继承。
#[test]
fn patch_on_a_system_rule_stores_only_the_diff() {
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "weixin.exe",
        &patch(json!({"first_show_mode": "wait"})),
    )
    .unwrap();
    let row = user_row(&l, Section::Apps, "Weixin.exe").unwrap();
    assert_eq!(row["first_show_mode"], json!("wait"));
    assert!(
        row.get("caret_use_top").is_none() && row.get("stale_probe_guard").is_none(),
        "不得拷贝系统字段: {row:?}"
    );
    assert_eq!(row["process"], json!("Weixin.exe"), "沿用系统层的写法");
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert_eq!(v.state, RuleState::Modified);
    assert_eq!(
        v.effective["caret_use_top"],
        json!(true),
        "系统字段照旧生效"
    );
    assert_eq!(v.effective["first_show_mode"], json!("wait"));
}

#[test]
fn patch_equal_to_the_system_value_removes_the_entry() {
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "Weixin.exe",
        &patch(json!({"auto_pair": true})),
    )
    .unwrap();
    assert_eq!(state_of(&l, "Weixin.exe"), RuleState::Modified);
    l.upsert(
        Section::Apps,
        "Weixin.exe",
        &patch(json!({"auto_pair": null})),
    )
    .unwrap();
    assert!(
        user_row(&l, Section::Apps, "Weixin.exe").is_none(),
        "回到与系统一致，条目应消失"
    );
    assert_eq!(state_of(&l, "Weixin.exe"), RuleState::System);
    l.upsert(
        Section::Apps,
        "Weixin.exe",
        &patch(json!({"caret_use_top": true})),
    )
    .unwrap();
    assert!(
        user_row(&l, Section::Apps, "Weixin.exe").is_none(),
        "写成与系统相同的值也是冗余"
    );
}

/// `null` = 清除 = 回到「跟随全局」：系统层设了它就要写 unset，否则只会继承回系统值。
#[test]
fn null_on_a_system_field_writes_unset_and_reset_field_undoes_it() {
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "Feishu.exe",
        &patch(json!({"composition_start_pair_guard": null})),
    )
    .unwrap();
    let row = user_row(&l, Section::Apps, "Feishu.exe").unwrap();
    assert_eq!(row["unset"], json!(["composition_start_pair_guard"]));
    let v = l.view_of(Section::Apps, "Feishu.exe").unwrap();
    assert_eq!(v.state, RuleState::Modified, "unset 是真正的差异");
    assert!(v.effective.get("composition_start_pair_guard").is_none());

    l.reset_field(Section::Apps, "Feishu.exe", "composition_start_pair_guard")
        .unwrap();
    assert!(
        user_row(&l, Section::Apps, "Feishu.exe").is_none(),
        "还原 = 继承出厂，痕迹全清"
    );
    assert_eq!(state_of(&l, "Feishu.exe"), RuleState::System);
}

#[test]
fn null_on_a_field_the_system_lacks_leaves_no_trace() {
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "Weixin.exe",
        &patch(json!({"auto_pair": null})),
    )
    .unwrap();
    assert!(
        user_row(&l, Section::Apps, "Weixin.exe").is_none(),
        "空操作不该留噪声条目"
    );
}

#[test]
fn explicit_false_over_a_system_true_is_kept() {
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "Feishu.exe",
        &patch(json!({"composition_start_pair_guard": false})),
    )
    .unwrap();
    assert_eq!(
        user_row(&l, Section::Apps, "Feishu.exe").unwrap()["composition_start_pair_guard"],
        json!(false)
    );
    let v = l.view_of(Section::Apps, "Feishu.exe").unwrap();
    assert_eq!(
        v.effective["composition_start_pair_guard"],
        json!(false),
        "显式关闭必须生效"
    );
    assert_eq!(v.state, RuleState::Modified);
}

#[test]
fn patch_rejects_invalid_enum_value_and_leaves_layers_untouched() {
    let mut l = layers(SYS, "");
    let err = l
        .upsert(
            Section::Apps,
            "Weixin.exe",
            &patch(json!({"first_show_mode": "bogus"})),
        )
        .unwrap_err();
    assert!(err.contains("first_show_mode"), "错误应指出字段: {err}");
    assert!(l.user.apps.is_empty());
}

#[test]
fn patch_rejects_typos_wrong_types_and_empty_enum() {
    let bad = [
        json!({"caret_use_tpo": false}),
        json!({"auto_pair": 0}),
        json!({"caret_use_top": "yes"}),
        json!({"first_show_mode": ""}),
        json!({"first_show_mode": 0}),
        json!({"caret_offset_x": "12"}),
    ];
    for p in bad {
        let mut l = layers(SYS, "");
        assert!(
            l.upsert(Section::Apps, "Weixin.exe", &patch(p.clone()))
                .is_err(),
            "补丁 {p} 应被拒绝"
        );
        assert!(l.user.apps.is_empty(), "被拒绝的补丁不得落任何东西: {p}");
    }
}

#[test]
fn patch_rejects_meta_keys() {
    for p in [
        json!({"disabled": true}),
        json!({"comment": "x"}),
        json!({"process": "O.exe"}),
        json!({"unset": ["auto_pair"]}),
    ] {
        let mut l = layers(SYS, "");
        assert!(
            l.upsert(Section::Apps, "Weixin.exe", &patch(p.clone()))
                .is_err(),
            "{p}"
        );
    }
}

/// 用户新增规则里的显式 `false` 不当冗余删掉：合成时它跨级压过 `*` / 更低级规则的 `true`
/// （「系统没有同身份行」不等于「没人设它」）。
#[test]
fn false_on_a_user_rule_is_kept_because_it_overrides_lower_tiers() {
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "Mine.exe",
        &patch(json!({"auto_pair": true, "caret_use_top": false})),
    )
    .expect("bool false 是合法值");
    let row = user_row(&l, Section::Apps, "Mine.exe").unwrap();
    assert_eq!(
        row["caret_use_top"],
        json!(false),
        "用户新增规则的显式 false 保留: {row:?}"
    );
    assert_eq!(row["auto_pair"], json!(true));
}

#[test]
fn process_validation_and_matching() {
    assert!(validate_process("").is_err());
    assert!(validate_process("   ").is_err());
    assert!(validate_process("a/b.exe").is_err());
    assert!(validate_process("a\\b.exe").is_err());
    assert!(validate_process("a\u{7}b.exe").is_err());
    assert_eq!(validate_process(" x.exe ").unwrap(), "x.exe");
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "  weixin.EXE ",
        &patch(json!({"auto_pair": true})),
    )
    .unwrap();
    assert_eq!(l.user.apps.len(), 1);
    assert_eq!(
        process_of(&l.user.apps[0]),
        "Weixin.exe",
        "沿用系统层的写法，不新增一条"
    );
}

/// 窗口类只改大小写或顺序，运行时等价，不算差异。
#[test]
fn classes_differing_only_in_case_or_order_are_not_a_modification() {
    let sys = "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"Progman\", \"WorkerW\"]\n";
    let mut l = layers(sys, "");
    l.upsert(
        Section::InitialModeScope,
        "explorer.exe",
        &patch(json!({"classes": ["workerw", "progman"]})),
    )
    .unwrap();
    assert!(
        l.user.initial_mode_scope.is_empty(),
        "{:?}",
        l.user.initial_mode_scope
    );
}

/// 元数据里登记的每个枚举可选值都必须被真实解析器接受并原样往返。
#[test]
fn every_registered_enum_option_roundtrips_through_the_real_parser() {
    use crate::compat_schema::{COMPAT_FIELDS, Kind};
    for f in COMPAT_FIELDS.iter().filter(|f| f.kind == Kind::Enum) {
        let sec = Section::parse(f.section).unwrap();
        for opt in f.options {
            let mut l = layers("", "");
            l.upsert(sec, "probe.exe", &patch(json!({ f.key: opt })))
                .unwrap_or_else(|e| panic!("{}.{} = {opt} 被拒绝: {e}", f.section, f.key));
            let v = l.view_of(sec, "probe.exe").unwrap();
            assert_eq!(
                v.effective[f.key],
                json!(opt),
                "{}.{} = {opt} 往返后不一致",
                f.section,
                f.key
            );
        }
    }
}

// ───────────── 还原 / 禁用 ─────────────

#[test]
fn reset_field_inherits_the_system_value_and_keeps_other_edits() {
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "Weixin.exe",
        &patch(json!({"caret_use_top": false, "auto_pair": true})),
    )
    .unwrap();
    l.reset_field(Section::Apps, "Weixin.exe", "caret_use_top")
        .unwrap();
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert_eq!(v.effective["caret_use_top"], json!(true), "回到系统值");
    assert_eq!(v.effective["auto_pair"], json!(true), "另一个改写保留");
    assert_eq!(v.overridden, vec!["auto_pair".to_string()]);
}

#[test]
fn reset_field_rejects_meta_keys_and_unknown_rules() {
    let mut l = layers(SYS, "");
    assert!(
        l.reset_field(Section::Apps, "Weixin.exe", "process")
            .is_err()
    );
    assert!(
        l.reset_field(Section::Apps, "Nope.exe", "auto_pair")
            .is_err()
    );
}

#[test]
fn reset_removes_the_user_entry_and_reports_whether_it_existed() {
    let mut l = layers(SYS, USER_A);
    assert!(l.reset(Section::Apps, "weixin.exe"));
    assert_eq!(state_of(&l, "Weixin.exe"), RuleState::System);
    assert!(!l.reset(Section::Apps, "Weixin.exe"));
    let mut u = layers(SYS, "[[apps]]\nprocess = \" x.exe \"\nauto_pair = true\n");
    assert!(u.reset(Section::Apps, "x.exe"), "已存的名字带空白也要能删");
}

#[test]
fn reset_all_clears_all_three_sections() {
    let usr = "[[apps]]\nprocess = \"Mine.exe\"\nauto_pair = true\n\n[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"X\"]\n\n[[commit_newline]]\nprocess = \"W.exe\"\nstyle = \"cr\"\n";
    let mut l = layers(SYS, usr);
    l.reset_all();
    let text = l.render_user().unwrap();
    assert!(!text.contains("[["), "{text}");
}

#[test]
fn set_disabled_roundtrip_restores_prior_edits() {
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "Weixin.exe",
        &patch(json!({"auto_pair": true})),
    )
    .unwrap();
    l.set_disabled(Section::Apps, "Weixin.exe", true).unwrap();
    assert_eq!(state_of(&l, "Weixin.exe"), RuleState::Disabled);
    assert_eq!(
        user_row(&l, Section::Apps, "Weixin.exe").unwrap()["auto_pair"],
        json!(true),
        "禁用期间改写还在"
    );
    l.set_disabled(Section::Apps, "Weixin.exe", false).unwrap();
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert_eq!(v.state, RuleState::Modified, "启用后回到禁用前的改写状态");
    assert_eq!(v.effective["auto_pair"], json!(true));
    assert!(
        user_row(&l, Section::Apps, "Weixin.exe")
            .unwrap()
            .get("disabled")
            .is_none()
    );
}

#[test]
fn enabling_an_untouched_disabled_rule_removes_the_entry() {
    let mut l = layers(SYS, "");
    l.set_disabled(Section::Apps, "Weixin.exe", true).unwrap();
    l.set_disabled(Section::Apps, "Weixin.exe", false).unwrap();
    assert!(
        user_row(&l, Section::Apps, "Weixin.exe").is_none(),
        "与系统一致就不该留条目"
    );
    assert_eq!(state_of(&l, "Weixin.exe"), RuleState::System);
}

#[test]
fn set_disabled_on_unknown_process_errors_and_user_only_rules_are_allowed() {
    let mut l = layers(SYS, "[[apps]]\nprocess = \"Mine.exe\"\nauto_pair = true\n");
    assert_eq!(
        l.set_disabled(Section::Apps, "Nope.exe", true).unwrap_err(),
        "无此规则"
    );
    l.set_disabled(Section::Apps, "Mine.exe", true).unwrap();
    assert_eq!(state_of(&l, "Mine.exe"), RuleState::Disabled);
    l.set_disabled(Section::Apps, "Mine.exe", false).unwrap();
    assert_eq!(state_of(&l, "Mine.exe"), RuleState::User);
}

// ───────────── 段之间 ─────────────

#[test]
fn sections_are_independent() {
    let sys = "[[apps]]\nprocess = \"explorer.exe\"\nauto_pair = true\n\n[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"CabinetWClass\"]\n";
    let mut l = layers(sys, "");
    l.upsert(
        Section::Apps,
        "explorer.exe",
        &patch(json!({"caret_use_top": true})),
    )
    .unwrap();
    assert!(
        l.user.initial_mode_scope.is_empty(),
        "改 apps 不能牵连 initial_mode_scope"
    );
    assert_eq!(
        l.view_of(Section::InitialModeScope, "explorer.exe")
            .unwrap()
            .state,
        RuleState::System
    );
}

#[test]
fn scope_and_newline_sections_support_patch_and_disable() {
    let sys = "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"CabinetWClass\"]\n\n[[commit_newline]]\nprocess = \"WINWORD.EXE\"\nstyle = \"cr\"\n";
    let mut l = layers(sys, "");
    l.upsert(
        Section::InitialModeScope,
        "explorer.exe",
        &patch(json!({"classes": ["CabinetWClass", "Progman"]})),
    )
    .unwrap();
    assert_eq!(
        l.view_of(Section::InitialModeScope, "explorer.exe")
            .unwrap()
            .state,
        RuleState::Modified
    );
    l.set_disabled(Section::CommitNewline, "WINWORD.EXE", true)
        .unwrap();
    assert_eq!(
        l.view_of(Section::CommitNewline, "WINWORD.EXE")
            .unwrap()
            .state,
        RuleState::Disabled
    );
    assert!(
        l.upsert(
            Section::CommitNewline,
            "WINWORD.EXE",
            &patch(json!({"style": "bogus"}))
        )
        .is_err()
    );
}

// ───────────── 读写与文件 ─────────────

#[test]
fn strict_load_reports_syntax_error_with_path() {
    let dir = tmp("syntax");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(COMPAT_FILE_NAME), "[[apps\nprocess=").unwrap();
    let err = match Layers::load(None, None, Some(&dir)) {
        Err(e) => e,
        Ok(_) => panic!("语法错必须报错，而不是静默按空集处理"),
    };
    assert!(
        err.contains(COMPAT_FILE_NAME) && err.contains("line"),
        "应带路径与行号: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_user_file_is_an_empty_layer() {
    assert!(
        Layers::load(None, None, Some(&tmp("missing")))
            .unwrap()
            .user
            .apps
            .is_empty()
    );
}

#[test]
fn save_is_atomic_and_preserves_other_sections_and_unknown_keys() {
    let dir = tmp("save");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(COMPAT_FILE_NAME),
        "[[commit_newline]]\nprocess = \"WINWORD.EXE\"\nstyle = \"cr\"\n\n[[apps]]\nprocess = \"Old.exe\"\nfuture_key = 3\nauto_pair = true\n",
    )
    .unwrap();
    let mut l = Layers::load(None, None, Some(&dir)).unwrap();
    l.upsert(
        Section::Apps,
        "Mine.exe",
        &patch(json!({"auto_pair": true})),
    )
    .unwrap();
    l.save(&dir).unwrap();
    let text = std::fs::read_to_string(dir.join(COMPAT_FILE_NAME)).unwrap();
    for needle in [
        "[[commit_newline]]",
        "Mine.exe",
        "Old.exe",
        "future_key = 3",
    ] {
        assert!(text.contains(needle), "写回后丢了 {needle}: {text}");
    }
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "不应残留临时文件: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ───────────── 导出 / 导入 ─────────────

#[test]
fn export_user_contains_only_the_diff() {
    let l = layers(SYS, USER_A);
    let text = l.export(ExportScope::User).unwrap();
    assert!(text.contains("auto_pair = true"), "{text}");
    assert!(
        !text.contains("caret_use_top") && !text.contains("Feishu"),
        "用户导出只含差异: {text}"
    );
}

#[test]
fn export_effective_expands_the_overlay_and_drops_disabled_but_keeps_unset() {
    let usr = format!(
        "{USER_A}\n[[apps]]\nprocess = \"Dota.exe\"\ndisabled = true\n\n[[apps]]\nprocess = \"Feishu.exe\"\nunset = [\"composition_start_pair_guard\"]\n"
    );
    let l = layers(SYS, &usr);
    let text = l.export(ExportScope::Effective).unwrap();
    assert!(
        text.contains("caret_use_top = true") && text.contains("auto_pair = true"),
        "叠加后的全貌: {text}"
    );
    assert!(
        !text.contains("Dota") && !text.contains("disabled"),
        "被禁用的不导出: {text}"
    );
    assert!(
        !text.contains("composition_start_pair_guard = "),
        "unset 掉的字段不该以取值形式出现: {text}"
    );
    // 保留 unset 记号：同一套系统层上重新导入，被取消的字段不能又继承回来。
    let mut b = layers(SYS, "");
    b.import_apply(&text, ImportMode::Replace).unwrap();
    let (va, vb) = (
        l.view_of(Section::Apps, "Feishu.exe").unwrap(),
        b.view_of(Section::Apps, "Feishu.exe").unwrap(),
    );
    assert_eq!(va.effective, vb.effective, "生效导出回导后结果必须一致");
}

#[test]
fn export_then_import_replace_roundtrips() {
    let usr = format!(
        "{USER_A}\n[[apps]]\nprocess = \"Dota.exe\"\ndisabled = true\n\n[[apps]]\nprocess = \"Feishu.exe\"\nunset = [\"composition_start_pair_guard\"]\n"
    );
    let a = layers(SYS, &usr);
    let text = a.export(ExportScope::User).unwrap();
    let mut b = layers(SYS, "");
    b.import_apply(&text, ImportMode::Replace).unwrap();
    for p in ["Feishu.exe", "Weixin.exe", "Dota.exe"] {
        let (va, vb) = (
            a.view_of(Section::Apps, p).unwrap(),
            b.view_of(Section::Apps, p).unwrap(),
        );
        assert_eq!((va.state, &va.effective), (vb.state, &vb.effective), "{p}");
    }
}

#[test]
fn import_preview_classifies_add_override_disable_unchanged() {
    let incoming = "\
[[apps]]
process = \"Brand.exe\"
auto_pair = true

[[apps]]
process = \"Weixin.exe\"
auto_pair = true

[[apps]]
process = \"Dota.exe\"
disabled = true

[[apps]]
process = \"Feishu.exe\"
composition_start_pair_guard = true
";
    let p = layers(SYS, "")
        .import_preview(incoming, ImportMode::Merge)
        .unwrap();
    let act = |n: &str| p.items.iter().find(|i| i.process == n).map(|i| i.action);
    assert_eq!(act("Brand.exe"), Some("add"));
    assert_eq!(act("Weixin.exe"), Some("override"));
    assert_eq!(act("Dota.exe"), Some("disable"));
    assert_eq!(
        act("Feishu.exe"),
        Some("unchanged"),
        "与系统完全一致 = 无变化"
    );
}

#[test]
fn import_preview_does_not_mutate_layers() {
    let l = layers(SYS, USER_A);
    let before: Vec<_> = l
        .view(Section::Apps)
        .into_iter()
        .map(|v| (v.process, v.state))
        .collect();
    let _ = l
        .import_preview(
            "[[apps]]\nprocess = \"Brand.exe\"\nauto_pair = true\n",
            ImportMode::Replace,
        )
        .unwrap();
    let after: Vec<_> = l
        .view(Section::Apps)
        .into_iter()
        .map(|v| (v.process, v.state))
        .collect();
    assert_eq!(before, after);
}

/// 导入按字段叠加：稀疏片段不得让系统规则里的其它字段丢失，显式 false 能覆盖出厂的 true。
#[test]
fn import_sparse_snippet_keeps_the_system_fields_and_explicit_false_overrides() {
    let mut l = layers(SYS, "");
    l.import_apply(
        "[[apps]]\nprocess = \"weixin.exe\"\nfirst_show_mode = \"wait\"\n",
        ImportMode::Merge,
    )
    .unwrap();
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert_eq!(v.effective["caret_use_top"], json!(true), "出厂修复不能丢");
    assert_eq!(v.effective["stale_probe_guard"], json!(true));
    assert_eq!(v.effective["first_show_mode"], json!("wait"));
    l.import_apply(
        "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_top = false\n",
        ImportMode::Merge,
    )
    .unwrap();
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert!(
        v.effective.get("caret_use_top").is_none(),
        "显式 false 应生效: {:?}",
        v.effective
    );
    assert_eq!(
        v.effective["first_show_mode"],
        json!("wait"),
        "之前导入的字段仍在（叠加而非替换）"
    );
}

#[test]
fn import_understands_unset() {
    let mut l = layers(SYS, "");
    l.import_apply(
        "[[apps]]\nprocess = \"Weixin.exe\"\nunset = [\"caret_use_top\"]\n",
        ImportMode::Merge,
    )
    .unwrap();
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert!(v.effective.get("caret_use_top").is_none());
    assert_eq!(v.state, RuleState::Modified);
}

#[test]
fn import_merge_keeps_unmentioned_user_entries_and_replace_drops_them() {
    let mut m = layers(SYS, "[[apps]]\nprocess = \"E.exe\"\nauto_pair = true\n");
    m.import_apply(
        "[[apps]]\nprocess = \"C.exe\"\nauto_pair = true\n",
        ImportMode::Merge,
    )
    .unwrap();
    assert!(
        m.view_of(Section::Apps, "E.exe").is_some() && m.view_of(Section::Apps, "C.exe").is_some()
    );
    let mut r = layers(SYS, "[[apps]]\nprocess = \"E.exe\"\nauto_pair = true\n");
    r.import_apply(
        "[[apps]]\nprocess = \"C.exe\"\nauto_pair = true\n",
        ImportMode::Replace,
    )
    .unwrap();
    assert!(
        r.view_of(Section::Apps, "E.exe").is_none(),
        "Replace 要先清空用户层"
    );
}

#[test]
fn import_reports_unknown_keys_and_sections() {
    let text =
        "[[apps]]\nprocess = \"A.exe\"\nbogus_key = 1\nauto_pair = true\n\n[[mystery]]\nx = 1\n";
    let p = layers(SYS, "")
        .import_preview(text, ImportMode::Merge)
        .unwrap();
    assert!(
        p.ignored_keys.contains(&"apps.bogus_key".to_string()),
        "{:?}",
        p.ignored_keys
    );
    assert!(
        p.ignored_keys.contains(&"mystery".to_string()),
        "{:?}",
        p.ignored_keys
    );
}

/// 无效取值只丢这一个键并报告，且不能原样落进用户层文件。
#[test]
fn import_drops_invalid_values_reports_them_and_never_persists_them() {
    let text = "[[apps]]\nprocess = \"A.exe\"\nfirst_show_mode = \"bogus\"\nauto_pair = true\n";
    let mut l = layers(SYS, "");
    let p = l.import_apply(text, ImportMode::Merge).unwrap();
    assert!(
        p.fallbacks.iter().any(|f| f.contains("bogus")),
        "{:?}",
        p.fallbacks
    );
    let row = user_row(&l, Section::Apps, "A.exe").unwrap();
    assert!(
        row.get("first_show_mode").is_none(),
        "无效值不得落盘: {row:?}"
    );
    assert_eq!(row["auto_pair"], json!(true), "同一条里的有效字段照常导入");
}

#[test]
fn import_rejects_invalid_process_names_and_reports_them() {
    let mut l = layers(SYS, "");
    let p = l.import_apply("[[apps]]\nprocess = \" a/b.exe \"\nauto_pair = true\n\n[[apps]]\nprocess = \"ok.exe\"\nauto_pair = true\n", ImportMode::Merge).unwrap();
    assert_eq!(p.rejected.len(), 1, "{:?}", p.rejected);
    assert!(l.user.apps.iter().all(|r| !process_of(r).contains('/')));
    assert!(l.view_of(Section::Apps, "ok.exe").is_some());
}

#[test]
fn import_syntax_error_is_an_error_not_silent() {
    let err = layers(SYS, "")
        .import_preview("[[apps\nprocess=", ImportMode::Merge)
        .unwrap_err();
    assert!(err.contains("line"), "应带行号: {err}");
}

#[test]
fn importing_the_same_content_twice_is_idempotent() {
    let text = "[[apps]]\nprocess = \"Weixin.exe\"\nauto_pair = true\n";
    let mut l = layers(SYS, "");
    assert_eq!(
        l.import_apply(text, ImportMode::Merge).unwrap().items[0].action,
        "override"
    );
    assert_eq!(
        l.import_apply(text, ImportMode::Merge).unwrap().items[0].action,
        "unchanged"
    );
}

// ───────────── RPC ─────────────

/// 用户目录里所有 `compat.toml.bak-*` 备份，按名字排序。
fn backups(dir: &Path) -> Vec<std::path::PathBuf> {
    let prefix = format!("{COMPAT_FILE_NAME}.bak-");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(&prefix))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn rpc_dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (d, u) = (tmp(&format!("{tag}_d")), tmp(&format!("{tag}_u")));
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&u);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(COMPAT_FILE_NAME), SYS).unwrap();
    (d, u)
}

fn call(m: &str, p: Value, d: &Path, u: &Path) -> Result<RpcOutcome, String> {
    rpc(m, &p, Some(d), None, Some(u))
}

fn cleanup(d: &Path, u: &Path) {
    let _ = std::fs::remove_dir_all(d);
    let _ = std::fs::remove_dir_all(u);
}

#[test]
fn rpc_list_returns_views_and_warnings() {
    let (d, u) = rpc_dirs("list");
    let r = call("compat.list", json!({"section": "apps"}), &d, &u).unwrap();
    assert!(!r.wrote);
    assert_eq!(r.value["rules"].as_array().unwrap().len(), 3);
    assert!(r.value["warnings"].is_array());
    cleanup(&d, &u);
}

#[test]
fn rpc_unknown_method_and_bad_section_error() {
    let (d, u) = rpc_dirs("bad");
    assert!(call("compat.bogus", json!({}), &d, &u).is_err());
    assert!(call("compat.list", json!({"section": "x"}), &d, &u).is_err());
    assert!(call("compat.list", json!({}), &d, &u).is_err());
    cleanup(&d, &u);
}

#[test]
fn rpc_write_methods_report_wrote_true_and_persist() {
    let (d, u) = rpc_dirs("write");
    let r = call(
        "compat.upsert",
        json!({"section": "apps", "process": "Weixin.exe", "patch": {"auto_pair": true}}),
        &d,
        &u,
    )
    .unwrap();
    assert!(r.wrote);
    assert_eq!(r.value["rule"]["state"], json!("modified"));
    assert!(
        std::fs::read_to_string(u.join(COMPAT_FILE_NAME))
            .unwrap()
            .contains("auto_pair")
    );

    let r = call(
        "compat.setDisabled",
        json!({"section": "apps", "process": "Feishu.exe", "disabled": true}),
        &d,
        &u,
    )
    .unwrap();
    assert!(r.wrote);
    assert_eq!(r.value["rule"]["state"], json!("disabled"));

    let r = call(
        "compat.resetField",
        json!({"section": "apps", "process": "Weixin.exe", "key": "auto_pair"}),
        &d,
        &u,
    )
    .unwrap();
    assert!(r.wrote);
    assert_eq!(r.value["rule"]["state"], json!("system"));

    let r = call(
        "compat.reset",
        json!({"section": "apps", "process": "Feishu.exe"}),
        &d,
        &u,
    )
    .unwrap();
    assert!(r.wrote);
    assert_eq!(r.value["removed"], json!(true));

    // 只读方法不写
    for (m, p) in [
        ("compat.list", json!({"section": "apps"})),
        ("compat.schema", json!({})),
        ("compat.export", json!({"scope": "effective"})),
    ] {
        assert!(!call(m, p, &d, &u).unwrap().wrote, "{m} 不应报告写入");
    }
    cleanup(&d, &u);
}

#[test]
fn rpc_upsert_rejects_bad_value_and_does_not_touch_disk() {
    let (d, u) = rpc_dirs("reject");
    let err = call(
        "compat.upsert",
        json!({"section": "apps", "process": "Weixin.exe", "patch": {"first_show_mode": "bogus"}}),
        &d,
        &u,
    )
    .err()
    .expect("非法取值必须被拒绝");
    assert!(err.contains("first_show_mode"), "{err}");
    assert!(
        !u.join(COMPAT_FILE_NAME).exists(),
        "被拒绝的写入不应产生文件"
    );
    cleanup(&d, &u);
}

#[test]
fn rpc_write_without_user_dir_errors() {
    let (d, u) = rpc_dirs("nodir");
    let r = rpc(
        "compat.upsert",
        &json!({"section": "apps", "process": "A.exe", "patch": {"auto_pair": true}}),
        Some(&d),
        None,
        None,
    );
    assert_eq!(r.err().as_deref(), Some("无用户配置目录"));
    cleanup(&d, &u);
}

// ───────────── 旧键 ignore_host_ime_close（读取别名）经设置端全链路 ─────────────

const LEGACY_SYS: &str = "[[apps]]\nprocess = \"X60.exe\"\nignore_host_ime_switch = true\n";
const LEGACY_USER: &str = "[[apps]]\nprocess = \"X60.exe\"\nignore_host_ime_close = false\nfuture_key = 3\n\n\
                               [[apps]]\nprocess = \"Old.exe\"\nignore_host_ime_close = true\n";

fn legacy_dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (d, u) = (tmp(&format!("{tag}_d")), tmp(&format!("{tag}_u")));
    for (dir, text) in [(&d, LEGACY_SYS), (&u, LEGACY_USER)] {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(COMPAT_FILE_NAME), text).unwrap();
    }
    (d, u)
}

fn listed_rule(d: &Path, u: &Path, process: &str) -> Value {
    let r = call("compat.list", json!({"section": "apps"}), d, u).unwrap();
    r.value["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["process"] == json!(process))
        .unwrap_or_else(|| panic!("列表里没有 {process}"))
        .clone()
}

/// 用户文件写旧键：列表在新键下看到用户值，也不把旧键报成不认识的键。
#[test]
fn rpc_list_shows_legacy_key_under_the_new_key() {
    let (d, u) = legacy_dirs("lg_list");
    let x60 = listed_rule(&d, &u, "X60.exe");
    let f = &x60["fields"]["ignore_host_ime_switch"];
    assert_eq!(f["userValue"], json!(false), "{x60}");
    assert_eq!(f["systemValue"], json!(true), "{x60}");
    assert_eq!(x60["unknownKeys"], json!(["future_key"]), "{x60}");
    assert!(x60["fields"].get("ignore_host_ime_close").is_none());
    let old = listed_rule(&d, &u, "Old.exe");
    assert_eq!(
        old["fields"]["ignore_host_ime_switch"]["userValue"],
        json!(true),
        "{old}"
    );
    cleanup(&d, &u);
}

/// 对写了旧键的规则清除（patch null）/ 还原（resetField）：结果按新键算，文件里不残留旧键，
/// 不认识的键原样保留。
#[test]
fn rpc_clear_and_reset_field_on_a_legacy_key_leave_only_the_new_key() {
    let (d, u) = legacy_dirs("lg_clear");
    let r = call(
        "compat.upsert",
        json!({"section": "apps", "process": "X60.exe", "patch": {"ignore_host_ime_switch": null}}),
        &d,
        &u,
    )
    .unwrap();
    assert!(r.wrote);
    assert_eq!(
        r.value["rule"]["fields"]["ignore_host_ime_switch"]["source"],
        json!("cleared"),
        "{}",
        r.value
    );
    let text = std::fs::read_to_string(u.join(COMPAT_FILE_NAME)).unwrap();
    assert!(!text.contains("ignore_host_ime_close"), "{text}");
    assert!(
        text.contains("unset = [\"ignore_host_ime_switch\"]"),
        "{text}"
    );
    assert!(text.contains("future_key = 3"), "{text}");
    cleanup(&d, &u);

    let (d, u) = legacy_dirs("lg_reset");
    let r = call(
        "compat.resetField",
        json!({"section": "apps", "process": "X60.exe", "key": "ignore_host_ime_switch"}),
        &d,
        &u,
    )
    .unwrap();
    assert!(r.wrote);
    let f = &r.value["rule"]["fields"]["ignore_host_ime_switch"];
    assert_eq!(f["source"], json!("system"), "{}", r.value);
    assert_eq!(f["value"], json!(true), "还原 = 继承出厂");
    let text = std::fs::read_to_string(u.join(COMPAT_FILE_NAME)).unwrap();
    assert!(!text.contains("ignore_host_ime_close"), "{text}");
    assert!(text.contains("future_key = 3"), "{text}");
    assert!(
        text.contains("ignore_host_ime_switch = true"),
        "别的规则里的旧键换成新键、值不变: {text}"
    );
    cleanup(&d, &u);
}

/// 导入含旧键的片段：不报不认识的键，落盘用新键。
#[test]
fn import_of_a_legacy_key_is_understood_and_persisted_as_the_new_key() {
    let text = "[[apps]]\nprocess = \"New.exe\"\nignore_host_ime_close = true\n";
    let mut l = layers(LEGACY_SYS, "");
    let p = l.import_preview(text, ImportMode::Merge).unwrap();
    assert!(p.ignored_keys.is_empty(), "{:?}", p.ignored_keys);
    assert!(p.fallbacks.is_empty(), "{:?}", p.fallbacks);
    l.import_apply(text, ImportMode::Merge).unwrap();
    let row = user_row(&l, Section::Apps, "New.exe").expect("导入后应有用户条目");
    assert_eq!(row.get("ignore_host_ime_switch"), Some(&json!(true)));
    assert!(!row.contains_key("ignore_host_ime_close"));
    assert!(
        !l.render_user().unwrap().contains("ignore_host_ime_close"),
        "落盘文本不残留旧键"
    );
}

#[test]
fn rpc_import_dry_run_does_not_write() {
    let (d, u) = rpc_dirs("dry");
    let r = call(
        "compat.import",
        json!({"content": "[[apps]]\nprocess = \"New.exe\"\nauto_pair = true\n", "dryRun": true}),
        &d,
        &u,
    )
    .unwrap();
    assert!(!r.wrote);
    assert_eq!(r.value["applied"], json!(false));
    assert_eq!(r.value["items"][0]["action"], json!("add"));
    assert!(!u.join(COMPAT_FILE_NAME).exists());
    cleanup(&d, &u);
}

#[test]
fn rpc_import_backs_up_existing_user_file() {
    let (d, u) = rpc_dirs("bak");
    std::fs::create_dir_all(&u).unwrap();
    let original = "[[apps]]\nprocess = \"Old.exe\"\nauto_pair = true\n";
    std::fs::write(u.join(COMPAT_FILE_NAME), original).unwrap();
    let r = call(
        "compat.import",
        json!({"content": "[[apps]]\nprocess = \"New.exe\"\nauto_pair = true\n", "mode": "replace"}),
        &d,
        &u,
    )
    .unwrap();
    assert!(r.wrote);
    assert_eq!(r.value["applied"], json!(true));
    let found = backups(&u);
    assert_eq!(found.len(), 1, "应恰好留下一份备份: {found:?}");
    assert_eq!(
        std::fs::read_to_string(&found[0]).unwrap(),
        original,
        "备份必须是导入前的原文"
    );
    assert!(
        r.value["backup"].as_str().is_some(),
        "响应里要告诉界面备份文件名: {}",
        r.value
    );
    assert!(
        std::fs::read_to_string(u.join(COMPAT_FILE_NAME))
            .unwrap()
            .contains("New.exe")
    );
    cleanup(&d, &u);
}

#[test]
fn rpc_reset_all_backs_up_and_clears() {
    let (d, u) = rpc_dirs("all");
    std::fs::create_dir_all(&u).unwrap();
    let original = "[[apps]]\nprocess = \"Old.exe\"\nauto_pair = true\n";
    std::fs::write(u.join(COMPAT_FILE_NAME), original).unwrap();
    let r = call("compat.resetAll", json!({}), &d, &u).unwrap();
    assert!(r.wrote);
    let found = backups(&u);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(std::fs::read_to_string(&found[0]).unwrap(), original);
    assert!(
        !std::fs::read_to_string(u.join(COMPAT_FILE_NAME))
            .unwrap()
            .contains("Old.exe")
    );
    cleanup(&d, &u);
}

#[test]
fn rpc_write_on_broken_user_file_errors_and_keeps_it() {
    let (d, u) = rpc_dirs("broken");
    std::fs::create_dir_all(&u).unwrap();
    let broken = "[[apps\nprocess=";
    std::fs::write(u.join(COMPAT_FILE_NAME), broken).unwrap();
    let err = call(
        "compat.upsert",
        json!({"section": "apps", "process": "A.exe", "patch": {"auto_pair": true}}),
        &d,
        &u,
    )
    .err()
    .expect("用户层语法错必须报错");
    assert!(err.contains("line"), "{err}");
    assert_eq!(
        std::fs::read_to_string(u.join(COMPAT_FILE_NAME)).unwrap(),
        broken,
        "出错时不得改写（更不能按空集重写）用户层"
    );
    cleanup(&d, &u);
}

// ───────────── 边界回归 ─────────────

/// data_custom 层禁用的规则：视图与运行时一致（运行时没有它），且可以在用户层重新启用。
#[test]
fn rule_disabled_in_custom_layer_is_disabled_in_view_and_at_runtime() {
    let (d, c) = (tmp("cust_d"), tmp("cust_c"));
    for dir in [&d, &c] {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(d.join(COMPAT_FILE_NAME), SYS).unwrap();
    std::fs::write(
        c.join(COMPAT_FILE_NAME),
        "[[apps]]\nprocess = \"Weixin.exe\"\ndisabled = true\n",
    )
    .unwrap();
    let mut l = Layers::load(Some(&d), Some(&c), None).unwrap();
    assert_eq!(
        state_of(&l, "Weixin.exe"),
        RuleState::Disabled,
        "视图必须反映它被禁用"
    );
    assert!(
        crate::app_compat::AppCompat::load_layered(Some(&d), Some(&c), None)
            .get_rule("Weixin.exe")
            .is_none(),
        "运行时确实没有它"
    );
    l.set_disabled(Section::Apps, "Weixin.exe", false).unwrap();
    assert_eq!(
        state_of(&l, "Weixin.exe"),
        RuleState::Modified,
        "用户层显式 disabled = false 重新启用"
    );
    assert!(
        user_row(&l, Section::Apps, "Weixin.exe")
            .unwrap()
            .get("disabled")
            == Some(&json!(false))
    );
    cleanup(&d, &c);
}

/// 运行时接受的写法（别名、带空白）不得被误拒。
#[test]
fn patch_accepts_aliases_and_padded_text() {
    let mut l = layers(SYS, "");
    l.upsert(
        Section::Apps,
        "Mine.exe",
        &patch(json!({"initial_mode": "en"})),
    )
    .expect("别名 en 是合法写法");
    l.upsert(
        Section::Apps,
        "Mine.exe",
        &patch(json!({"schema": " wubi86 "})),
    )
    .expect("带空白的方案 id 运行时会 trim，应接受");
}

/// replace 模式的预览要让用户看到「哪些会被删掉」。
#[test]
fn replace_preview_lists_entries_that_will_be_removed() {
    let l = layers(SYS, "[[apps]]\nprocess = \"Mine.exe\"\nauto_pair = true\n");
    let p = l
        .import_preview(
            "[[apps]]\nprocess = \"Other.exe\"\nauto_pair = true\n",
            ImportMode::Replace,
        )
        .unwrap();
    assert!(
        p.items
            .iter()
            .any(|i| i.process == "Mine.exe" && i.action == "remove"),
        "{:?}",
        p.items
    );
}

/// 用户层语法坏了，界面仍要有恢复手段：resetAll 与 replace 导入不依赖解析用户层。
#[test]
fn reset_all_and_replace_import_recover_from_a_broken_user_file() {
    let (d, u) = rpc_dirs("recover");
    std::fs::create_dir_all(&u).unwrap();
    let broken = "[[apps\nprocess=";
    std::fs::write(u.join(COMPAT_FILE_NAME), broken).unwrap();
    let r = call("compat.resetAll", json!({}), &d, &u).expect("resetAll 必须能恢复");
    assert!(r.wrote);
    assert!(
        std::fs::read_dir(&u)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| std::fs::read_to_string(e.path()).ok().as_deref() == Some(broken)),
        "损坏的原文件必须被备份，而不是被抹掉"
    );

    std::fs::write(u.join(COMPAT_FILE_NAME), broken).unwrap();
    let r = call(
        "compat.import",
        json!({"content": "[[apps]]\nprocess = \"New.exe\"\nauto_pair = true\n", "mode": "replace"}),
        &d,
        &u,
    )
    .expect("replace 导入必须能恢复");
    assert!(r.wrote);
    assert!(
        std::fs::read_to_string(u.join(COMPAT_FILE_NAME))
            .unwrap()
            .contains("New.exe")
    );

    // 合并导入不能：它要保留用户层已有内容，而那份内容读不出来。
    std::fs::write(u.join(COMPAT_FILE_NAME), broken).unwrap();
    assert!(
        call(
            "compat.import",
            json!({"content": "[[apps]]\nprocess = \"New.exe\"\nauto_pair = true\n", "mode": "merge"}),
            &d,
            &u
        )
        .is_err()
    );
    cleanup(&d, &u);
}

/// 并发写不得互相覆盖（读-改-写整段持锁）。
#[test]
fn concurrent_writes_do_not_lose_updates() {
    let (d, u) = rpc_dirs("race");
    let (d, u) = (std::sync::Arc::new(d), std::sync::Arc::new(u));
    let handles: Vec<_> = (0..12)
        .map(|i| {
            let (d, u) = (d.clone(), u.clone());
            std::thread::spawn(move || {
                call(
                    "compat.upsert",
                    json!({"section": "apps", "process": format!("P{i}.exe"), "patch": {"auto_pair": true}}),
                    &d,
                    &u,
                )
                .expect("并发写不应报错");
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let r = call("compat.list", json!({"section": "apps"}), &d, &u).unwrap();
    let users = r.value["rules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["state"] == json!("user"))
        .count();
    assert_eq!(users, 12, "12 次并发写应全部留下，丢了 {} 条", 12 - users);
    cleanup(&d, &u);
}

/// `reset` 什么也没删时不该报告写入（否则白白写盘、重载、广播）。
#[test]
fn reset_of_nonexistent_user_entry_reports_no_write() {
    let (d, u) = rpc_dirs("noop_reset");
    let r = call(
        "compat.reset",
        json!({"section": "apps", "process": "Weixin.exe"}),
        &d,
        &u,
    )
    .unwrap();
    assert!(!r.wrote);
    assert_eq!(r.value["removed"], json!(false));
    cleanup(&d, &u);
}

/// 备份最多留 5 份，且是最新的 5 份：连续多次导入后，「导入前的样子」仍能找回。
#[test]
fn backups_are_rotated_and_keep_the_newest() {
    let (d, u) = rpc_dirs("rotate");
    std::fs::create_dir_all(&u).unwrap();
    for i in 0..8 {
        std::fs::write(
            u.join(COMPAT_FILE_NAME),
            format!("[[apps]]\nprocess = \"V{i}.exe\"\nauto_pair = true\n"),
        )
        .unwrap();
        call(
            "compat.import",
            json!({"content": "[[apps]]\nprocess = \"X.exe\"\nauto_pair = true\n", "mode": "replace"}),
            &d,
            &u,
        )
        .unwrap();
    }
    let found = backups(&u);
    assert_eq!(found.len(), BACKUP_KEEP, "{found:?}");
    let newest = std::fs::read_to_string(found.last().unwrap()).unwrap();
    assert!(
        newest.contains("V7.exe"),
        "最新一份应是最后一次导入前的内容: {newest}"
    );
    cleanup(&d, &u);
}

/// dryRun 传了非布尔值必须报错，不能悄悄当成 false 而真的写入。
#[test]
fn non_boolean_dry_run_is_an_error_not_a_write() {
    let (d, u) = rpc_dirs("dryrun_type");
    let err = call(
        "compat.import",
        json!({"content": "[[apps]]\nprocess = \"N.exe\"\nauto_pair = true\n", "dryRun": "true"}),
        &d,
        &u,
    )
    .err()
    .expect("dryRun 类型不对必须报错");
    assert!(err.contains("dryRun"), "{err}");
    assert!(!u.join(COMPAT_FILE_NAME).exists());
    cleanup(&d, &u);
}

// ───────────── 视图与告警 ─────────────

/// `compat.list` 的 warnings 要报出每一层的无效值（加载不经反序列化，回落记录要单独收集）。
#[test]
fn warnings_report_invalid_values_from_every_layer() {
    let sys = "[[apps]]\nprocess = \"A.exe\"\nfirst_show_mode = \"wiat\"\n";
    let usr = "[[apps]]\nprocess = \"A.exe\"\ninitial_mode = \"englsh\"\n";
    let (d, u) = (tmp("warn_d"), tmp("warn_u"));
    for (dir, text) in [(&d, sys), (&u, usr)] {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(COMPAT_FILE_NAME), text).unwrap();
    }
    let l = Layers::load(Some(&d), None, Some(&u)).unwrap();
    let w = l.warnings();
    assert_eq!(w.len(), 2, "{w:?}");
    let keys: Vec<_> = w
        .iter()
        .map(|p| (p.section.as_str(), p.process.as_str(), p.key.as_str()))
        .collect();
    assert!(
        keys.contains(&("apps", "A.exe", "first_show_mode")),
        "带上段 / 进程 / 键: {keys:?}"
    );
    assert!(
        keys.contains(&("apps", "A.exe", "initial_mode")),
        "{keys:?}"
    );
    assert!(w.iter().all(|p| !p.message.is_empty()));
    let r = rpc(
        "compat.list",
        &json!({"section": "apps"}),
        Some(&d),
        None,
        Some(&u),
    )
    .unwrap();
    assert_eq!(
        r.value["warnings"].as_array().unwrap().len(),
        2,
        "{}",
        r.value
    );
    cleanup(&d, &u);
}

/// 用户层写错的值在视图里也当成没写：不能盖掉下层的好值。
#[test]
fn an_invalid_user_value_does_not_hide_the_system_value_in_the_view() {
    let l = layers(
        "[[apps]]\nprocess = \"A.exe\"\nfirst_show_mode = \"wait\"\n",
        "[[apps]]\nprocess = \"A.exe\"\nfirst_show_mode = \"wiat\"\n",
    );
    let v = l.view_of(Section::Apps, "A.exe").unwrap();
    assert_eq!(v.effective["first_show_mode"], json!("wait"));
    assert_eq!(v.state, RuleState::System, "写错的值不算差异");
}

/// 认不出的键不该让规则显示成「已修改」，也不进 overridden；单独列在 unknown_keys 里。
#[test]
fn unknown_keys_do_not_make_a_rule_modified() {
    let l = layers(
        SYS,
        "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_tpo = true\n",
    );
    let v = l.view_of(Section::Apps, "Weixin.exe").unwrap();
    assert_eq!(v.state, RuleState::System);
    assert!(v.overridden.is_empty(), "{:?}", v.overridden);
    assert_eq!(v.unknown_keys, vec!["caret_use_tpo".to_string()]);
}

/// 下层禁用、用户层显式 `disabled = false` 重新启用：状态是 Modified，overridden 里要有 disabled。
#[test]
fn re_enabling_a_lower_layer_disable_is_listed_as_an_override() {
    let l = layers(
        "[[apps]]\nprocess = \"A.exe\"\ndisabled = true\ncaret_use_top = true\n",
        "[[apps]]\nprocess = \"A.exe\"\ndisabled = false\n",
    );
    let v = l.view_of(Section::Apps, "A.exe").unwrap();
    assert_eq!(v.state, RuleState::Modified);
    assert_eq!(v.overridden, vec!["disabled".to_string()]);
}

/// 用户层有读不进来的内容（`[apps]` 单括号）：写操作 / 列表报错，恢复类操作照常。
#[test]
fn unreadable_user_content_is_an_error_but_reset_all_recovers() {
    let (d, u) = rpc_dirs("skipped");
    std::fs::create_dir_all(&u).unwrap();
    let text = "[apps]\nprocess = \"lost.exe\"\n";
    std::fs::write(u.join(COMPAT_FILE_NAME), text).unwrap();
    let err = call("compat.list", json!({"section": "apps"}), &d, &u)
        .err()
        .expect("必须报错");
    assert!(err.contains("无法读入"), "{err}");
    assert!(
        call(
            "compat.upsert",
            json!({"section": "apps", "process": "A.exe", "patch": {"auto_pair": true}}),
            &d,
            &u
        )
        .is_err()
    );
    call("compat.resetAll", json!({}), &d, &u).expect("resetAll 必须能恢复");
    assert!(
        backups(&u)
            .iter()
            .any(|p| std::fs::read_to_string(p).ok().as_deref() == Some(text)),
        "原文件必须被备份"
    );
    cleanup(&d, &u);
}

/// 用户目录与系统预置目录相同（拿不到用户配置目录的兜底）时拒绝写入，免得删掉出厂字段。
#[test]
fn writes_are_refused_when_the_user_dir_is_the_system_dir() {
    let (d, u) = rpc_dirs("samedir");
    let err = rpc(
        "compat.upsert",
        &json!({"section": "apps", "process": "Weixin.exe", "patch": {"auto_pair": true}}),
        Some(&d),
        None,
        Some(&d),
    )
    .err()
    .expect("必须拒绝");
    assert!(err.contains("相同"), "{err}");
    assert_eq!(
        std::fs::read_to_string(d.join(COMPAT_FILE_NAME)).unwrap(),
        SYS,
        "系统预置文件一个字节都不能动"
    );
    // 只读方法不受限。
    assert!(
        rpc(
            "compat.list",
            &json!({"section": "apps"}),
            Some(&d),
            None,
            Some(&d)
        )
        .is_ok()
    );
    cleanup(&d, &u);
}

// ───────────── 逐字段视图 fields（设置端渲染的唯一依据） ─────────────

fn field_of(l: &Layers, process: &str, key: &str) -> FieldView {
    l.view_of(Section::Apps, process)
        .unwrap_or_else(|| panic!("规则 {process} 应存在"))
        .fields
        .get(key)
        .unwrap_or_else(|| panic!("fields 缺少 {key}"))
        .clone()
}

#[test]
fn fields_source_system_shows_factory_value_and_nothing_from_the_user() {
    let l = layers(SYS, "");
    let f = field_of(&l, "Weixin.exe", "caret_use_top");
    assert_eq!(f.source, FieldSource::System);
    assert_eq!(f.value, json!(true));
    assert_eq!(f.system_value, json!(true));
    assert_eq!(f.user_value, Value::Null);
}

#[test]
fn fields_source_user_when_the_user_really_differs() {
    let l = layers(
        SYS,
        "[[apps]]\nprocess = \"Weixin.exe\"\nfirst_show_mode = \"wait\"\n",
    );
    let f = field_of(&l, "Weixin.exe", "first_show_mode");
    assert_eq!(f.source, FieldSource::User);
    assert_eq!(
        (f.value, f.user_value, f.system_value),
        (json!("wait"), json!("wait"), Value::Null)
    );
}

/// 用户显式关掉出厂打开的开关：`effective` 里这个键会消失，`fields` 必须仍然说清「用户设为关」。
#[test]
fn fields_distinguish_user_off_from_user_cleared() {
    let l = layers(
        SYS,
        "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_top = false\nunset = [\"stale_probe_guard\"]\n",
    );
    let off = field_of(&l, "Weixin.exe", "caret_use_top");
    assert_eq!(off.source, FieldSource::User, "显式关 = 用户改写");
    assert_eq!(off.value, json!(false));
    assert_eq!(off.user_value, json!(false));
    assert_eq!(off.system_value, json!(true));
    let cleared = field_of(&l, "Weixin.exe", "stale_probe_guard");
    assert_eq!(cleared.source, FieldSource::Cleared, "unset = 取消出厂设定");
    assert_eq!(
        cleared.value,
        json!(false),
        "Bool 的生效值是类型化默认值 false，不是 null"
    );
    assert_eq!(cleared.system_value, json!(true));
    assert_eq!(cleared.user_value, Value::Null);
}

#[test]
fn fields_source_default_when_nobody_set_it_and_values_are_typed() {
    let l = layers(SYS, "");
    let sec_fields = l.view_of(Section::Apps, "Weixin.exe").unwrap().fields;
    let f = &sec_fields["first_show_mode"];
    assert_eq!(
        (f.source, &f.value),
        (FieldSource::Default, &Value::Null),
        "Enum 未设为 null"
    );
    assert_eq!(
        sec_fields["caret_offset_x"].value,
        json!(0),
        "Int 的默认值是 0"
    );
    assert_eq!(
        sec_fields["auto_pair"].value,
        Value::Null,
        "TriBool 未设为 null"
    );
    assert_eq!(
        sec_fields["host_render"].value,
        json!(false),
        "Bool 的默认值是 false"
    );
    let scope = layers(
        "",
        "[[initial_mode_scope]]\nprocess = \"x.exe\"\nclasses = [\"A\"]\n",
    );
    assert_eq!(
        scope
            .view_of(Section::InitialModeScope, "x.exe")
            .unwrap()
            .fields["classes"]
            .value,
        json!(["A"])
    );
}

#[test]
fn fields_ignore_invalid_and_redundant_user_values() {
    let l = layers(
        SYS,
        "[[apps]]\nprocess = \"Weixin.exe\"\nfirst_show_mode = \"wiat\"\ncaret_use_top = true\n",
    );
    assert_eq!(
        field_of(&l, "Weixin.exe", "first_show_mode").source,
        FieldSource::Default,
        "写错的值当没写"
    );
    let same = field_of(&l, "Weixin.exe", "caret_use_top");
    assert_eq!(
        same.source,
        FieldSource::System,
        "与系统一致的冗余不算用户改写"
    );
    assert_eq!(same.user_value, Value::Null);
}

/// 每条规则的 `fields` 必须包含本段全部已登记字段，界面据此逐行渲染，不能漏。
#[test]
fn fields_cover_every_registered_field_of_the_section() {
    let l = layers(SYS, USER_A);
    for sec in [
        Section::Apps,
        Section::InitialModeScope,
        Section::CommitNewline,
    ] {
        let want: BTreeSet<_> = known_keys(sec.as_str())
            .into_iter()
            .map(str::to_string)
            .collect();
        for v in l.view(sec) {
            let got: BTreeSet<_> = v.fields.keys().cloned().collect();
            assert_eq!(got, want, "{} / {}", sec.as_str(), v.process);
        }
    }
    let l2 = layers(
        "",
        "[[commit_newline]]\nprocess = \"W.exe\"\nstyle = \"cr\"\n",
    );
    assert!(l2.view_of(Section::CommitNewline, "W.exe").is_some());
}

/// 对外 JSON 统一 camelCase（与导入预览、`dryRun` 参数一致）：界面不必猜每个键是哪种写法。
#[test]
fn view_and_schema_serialize_in_camel_case() {
    let l = layers(SYS, USER_A);
    let v = serde_json::to_value(l.view_of(Section::Apps, "Weixin.exe").unwrap()).unwrap();
    for k in [
        "hasUserEntry",
        "unknownKeys",
        "fields",
        "overridden",
        "effective",
    ] {
        assert!(v.get(k).is_some(), "缺少 {k}: {v}");
    }
    assert!(
        v.get("has_user_entry").is_none() && v.get("unknown_keys").is_none(),
        "不得留下 snake_case: {v}"
    );
    assert!(
        v["fields"]["caret_use_top"].get("systemValue").is_some(),
        "{v}"
    );
    let schema = crate::compat_schema::schema_json();
    let dep = schema["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "candidate_x")
        .unwrap();
    assert!(
        dep.get("dependsOn").is_some() && dep.get("depends_on").is_none(),
        "{dep}"
    );
}
