use super::*;

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("wind_compat_dis_{tag}_{}", std::process::id()))
}
fn write(dir: &std::path::Path, text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(COMPAT_FILE_NAME), text).unwrap();
}

#[test]
fn user_disabled_entry_removes_the_system_rule() {
    let (sys, usr) = (tmp("a_sys"), tmp("a_usr"));
    write(
        &sys,
        "[[apps]]\nprocess = \"Feishu.exe\"\ncomposition_start_pair_guard = true\n\
             [[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_top = true\n",
    );
    write(
        &usr,
        "[[apps]]\nprocess = \"feishu.exe\"\ndisabled = true\n",
    );
    let c = AppCompat::load_layered(Some(&sys), None, Some(&usr));
    assert!(
        c.get_rule("Feishu.exe").is_none(),
        "被禁用的进程不应再有规则"
    );
    assert!(c.get_rule("Weixin.exe").is_some(), "禁用只影响指名的进程");
    let _ = std::fs::remove_dir_all(&sys);
    let _ = std::fs::remove_dir_all(&usr);
}

#[test]
fn disabled_is_omitted_when_false_and_written_when_true() {
    let off = toml::to_string(&AppCompatRule {
        process: "a.exe".into(),
        caret_use_top: true,
        ..Default::default()
    })
    .unwrap();
    assert!(!off.contains("disabled"), "false 不应落盘: {off}");
    let on = toml::to_string(&AppCompatRule {
        process: "a.exe".into(),
        disabled: true,
        ..Default::default()
    })
    .unwrap();
    assert!(on.contains("disabled = true"), "true 必须落盘: {on}");
}

#[test]
fn wrong_typed_disabled_falls_back_to_enabled() {
    let (sys, usr) = (tmp("c_sys"), tmp("c_usr"));
    write(
        &sys,
        "[[apps]]\nprocess = \"Feishu.exe\"\ncaret_use_top = true\n",
    );
    write(
        &usr,
        "[[apps]]\nprocess = \"Feishu.exe\"\ndisabled = \"yes\"\ncaret_use_top = true\n",
    );
    let c = AppCompat::load_layered(Some(&sys), None, Some(&usr));
    assert!(
        c.get_rule("Feishu.exe").is_some(),
        "写错类型按未禁用处理，不能整条丢"
    );
    let _ = std::fs::remove_dir_all(&sys);
    let _ = std::fs::remove_dir_all(&usr);
}

#[test]
fn disabled_applies_to_scope_and_newline_sections() {
    let (sys, usr) = (tmp("d_sys"), tmp("d_usr"));
    write(
        &sys,
        "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"CabinetWClass\"]\n\
             [[commit_newline]]\nprocess = \"WINWORD.EXE\"\nstyle = \"cr\"\n",
    );
    write(
        &usr,
        "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\ndisabled = true\n\
             [[commit_newline]]\nprocess = \"WINWORD.EXE\"\ndisabled = true\n",
    );
    let c = AppCompat::load_layered(Some(&sys), None, Some(&usr));
    assert!(c.commit_newline_for("WINWORD.EXE").is_none());
    // 作用域规则被禁用 = 进程不在表内 = 不受限制
    assert!(c.initial_mode_applies_to_window("explorer.exe", "AnyOtherClass"));
    let _ = std::fs::remove_dir_all(&sys);
    let _ = std::fs::remove_dir_all(&usr);
}
