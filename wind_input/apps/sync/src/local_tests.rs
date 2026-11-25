use super::*;
use crate::testing::FakeCore;

fn rig() -> (tempfile::TempDir, Arc<FakeCore>, FsLocal, Echo) {
    let d = tempfile::tempdir().unwrap();
    let core = Arc::new(FakeCore::default());
    let echo = Echo::default();
    let l = FsLocal::new(d.path().to_path_buf(), core.clone(), echo.clone());
    (d, core, l, echo)
}

fn put(d: &Path, rel: &str, c: &str) {
    let p = d.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

#[test]
fn list_whole_user_dir_except_excluded_and_hidden() {
    let (d, _c, l, _) = rig();
    for (p, c) in [
        ("config.toml", "a"),
        ("compat.toml", "b"),
        ("meta.toml", "x"),
        ("schemas/pinyin.schema.toml", "s"),
        ("themes/t/theme.toml", "t"),
        ("data/pinyin/user.dict.yaml", "u"),
        ("data/pinyin/temp.dict.yaml", "no"),
        ("data/.backup/x.yaml", "no"),
        ("data/pinyin/.user.dict.yaml.wind-sync.tmp", "no"),
        ("emoji/a.txt", "e"),
        ("config.conflict-a-2026-11-24.toml", "no"),
    ] {
        put(d.path(), p, c);
    }
    assert_eq!(
        l.list().unwrap(),
        vec![
            "compat.toml",
            "config.toml",
            "data/pinyin/user.dict.yaml",
            "emoji/a.txt",
            "schemas/pinyin.schema.toml",
            "themes/t/theme.toml",
        ]
    );
    let mut off = FsLocal::new(
        d.path().to_path_buf(),
        Arc::new(FakeCore::default()),
        Echo::default(),
    );
    assert_eq!(off.list().unwrap().len(), 6);
    assert!(
        l.conflict_files()
            .contains(&"config.conflict-a-2026-11-24.toml".to_string())
    );
    assert!(off.read("../etc/passwd").is_err());
    assert_eq!(off.read("nope.toml").unwrap(), None);
    assert!(off.write_batch(&[("../x".into(), vec![])]).is_err());
}

#[test]
fn write_batch_is_atomic_and_merges_reloads() {
    let (d, core, mut l, echo) = rig();
    put(d.path(), "data/pinyin/user.dict.yaml", "old");
    put(d.path(), "data/phrases.dict.yaml", "old");
    l.write_batch(&[
        ("config.toml".into(), b"a=1\n".to_vec()),
        ("data/pinyin/user.dict.yaml".into(), b"u".to_vec()),
        ("data/phrases.dict.yaml".into(), b"p".to_vec()),
        ("themes/t/theme.toml".into(), b"t".to_vec()),
        ("themes/t/bg.png".into(), b"i".to_vec()),
        ("schemas/pinyin.schema.toml".into(), b"s".to_vec()),
        ("schemas/pinyin/x.txt".into(), b"s".to_vec()),
        ("data/charsets/gbk.txt".into(), b"c".to_vec()),
    ])
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(d.path().join("config.toml")).unwrap(),
        "a=1\n"
    );
    // 没有遗留临时文件
    for e in std::fs::read_dir(d.path()).unwrap().flatten() {
        assert!(!e.file_name().to_string_lossy().ends_with(TMP_SUFFIX));
    }
    assert_eq!(
        core.methods(),
        vec![
            "data.beginEdit pinyin/user.dict.yaml",
            "data.beginEdit phrases.dict.yaml",
            "data.endEdit pinyin/user.dict.yaml",
            "data.endEdit phrases.dict.yaml",
            "config.reload",
            "schema.reloadFiles",
            "theme.reloadFiles",
            "dict.reload",
        ]
    );
    let calls = core.calls.lock().unwrap();
    let theme = calls.iter().find(|c| c.0 == "theme.reloadFiles").unwrap();
    assert_eq!(theme.1, json!({"ids": ["t"]}));
    let schema = calls.iter().find(|c| c.0 == "schema.reloadFiles").unwrap();
    assert_eq!(schema.1, json!({}), "子目录资源被多方案共用，重载全部");
    assert_eq!(
        lock(&echo)["config.toml"],
        content_hash(b"a=1\n"),
        "写入后记 hash 供回声抑制"
    );
}

#[test]
fn newly_synced_files_trigger_their_reloads() {
    let (d, core, mut l, _echo) = rig();
    put(d.path(), "system.softkeyboard.toml", "old");
    l.write_batch(&[
        ("schema_overrides/wubi86.toml".into(), b"a".to_vec()),
        ("emoji/a.txt".into(), b"e".to_vec()),
        ("data/pinyin_misread.yaml".into(), b"m".to_vec()),
        ("system.softkeyboard.toml".into(), b"k".to_vec()),
        ("wind_setting.toml".into(), b"w".to_vec()),
    ])
    .unwrap();
    assert_eq!(
        core.methods(),
        vec!["config.reload", "schema.reloadFiles", "softkeyboard.reload"]
    );
    let calls = core.calls.lock().unwrap();
    let schema = calls.iter().find(|c| c.0 == "schema.reloadFiles").unwrap();
    assert_eq!(schema.1, json!({"ids": ["wubi86"]}));
}

#[test]
fn schema_override_with_odd_id_reloads_all_schemas() {
    let (_d, core, mut l, _echo) = rig();
    l.write_batch(&[("schema_overrides/a b.toml".into(), b"a".to_vec())])
        .unwrap();
    let calls = core.calls.lock().unwrap();
    assert_eq!(calls[0], ("schema.reloadFiles".to_string(), json!({})));
}

#[test]
fn begin_edit_failure_writes_nothing_and_releases_sessions() {
    let (d, core, mut l, _) = rig();
    put(d.path(), "data/pinyin/user.dict.yaml", "old");
    put(d.path(), "data/wubi86/user.dict.yaml", "old");
    core.fail
        .lock()
        .unwrap()
        .push("data.beginEdit data/x".into());
    core.fail
        .lock()
        .unwrap()
        .push("data.beginEdit wubi86/user.dict.yaml".into());
    let r = l.write_batch(&[
        ("config.toml".into(), b"a".to_vec()),
        ("data/pinyin/user.dict.yaml".into(), b"u".to_vec()),
        ("data/wubi86/user.dict.yaml".into(), b"w".to_vec()),
    ]);
    assert!(r.unwrap_err().0.contains("拒绝开始编辑"));
    assert!(!d.path().join("config.toml").exists());
    assert_eq!(
        std::fs::read_to_string(d.path().join("data/pinyin/user.dict.yaml")).unwrap(),
        "old"
    );
    let m = core.methods();
    assert!(m.contains(&"data.endEdit pinyin/user.dict.yaml".to_string()));
    assert!(!m.contains(&"config.reload".to_string()));
}

#[test]
fn delete_goes_through_edit_session_too() {
    let (d, core, mut l, echo) = rig();
    put(d.path(), "data/pinyin/user.dict.yaml", "u");
    put(d.path(), "compat.toml", "c");
    l.delete(&["data/pinyin/user.dict.yaml".into(), "compat.toml".into()])
        .unwrap();
    assert!(!d.path().join("compat.toml").exists());
    assert_eq!(
        core.methods(),
        vec![
            "data.beginEdit pinyin/user.dict.yaml",
            "data.endEdit pinyin/user.dict.yaml",
            "compat.reload",
        ]
    );
    assert_eq!(lock(&echo)["compat.toml"], "");
}

#[test]
fn conflict_copies_are_found_even_out_of_scope() {
    let (d, _c, l, _) = rig();
    put(d.path(), "config.conflict-dev1-2026-11-24.toml", "x");
    put(d.path(), "themes/t/a.conflict-d-1.toml", "x");
    put(d.path(), "config.toml", "x");
    let set_age = |rel: &str, secs: u64| {
        let f = std::fs::File::options()
            .write(true)
            .open(d.path().join(rel))
            .unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    };
    set_age("config.conflict-dev1-2026-11-24.toml", 1000);
    set_age("themes/t/a.conflict-d-1.toml", 2000);
    // 新的在前
    assert_eq!(
        l.conflict_files(),
        vec![
            "themes/t/a.conflict-d-1.toml",
            "config.conflict-dev1-2026-11-24.toml",
        ]
    );
    // 用户删掉后下一次收集即消失
    std::fs::remove_file(d.path().join("themes/t/a.conflict-d-1.toml")).unwrap();
    assert_eq!(
        l.conflict_files(),
        vec!["config.conflict-dev1-2026-11-24.toml"]
    );
}

#[test]
fn edit_targets() {
    assert_eq!(
        edit_target("data/pinyin/user.dict.yaml"),
        Some("pinyin/user.dict.yaml")
    );
    assert_eq!(edit_target("data/charsets/a.yaml"), Some("charsets/a.yaml"));
    assert_eq!(edit_target("data/charsets/a.txt"), None);
    assert_eq!(
        edit_target("data/pinyin/dicts/x.dict.yaml"),
        Some("pinyin/dicts/x.dict.yaml")
    );
    assert_eq!(edit_target("config.toml"), None);
}

const UD: &str = "data/pinyin/user.dict.yaml";
const UD_EDIT: &str = "pinyin/user.dict.yaml";

/// 扫描后、写入前，core 在 beginEdit 里把待处理学词折回了文件：不得以旧内容覆盖，
/// 且下一轮重新扫描能把折回的内容纳入。
#[test]
fn content_folded_by_begin_edit_is_not_overwritten_and_next_round_sees_it() {
    let (d, core, mut l, _) = rig();
    put(d.path(), UD, "old");
    assert_eq!(l.read(UD).unwrap().unwrap(), b"old");
    let path = d.path().join(UD);
    *core.on_call.lock().unwrap() = Some(Box::new(move |m, _| {
        if m == "data.beginEdit" {
            std::fs::write(&path, "folded").unwrap();
        }
    }));
    let e = l
        .write_batch(&[(UD.into(), b"remote".to_vec())])
        .unwrap_err();
    assert!(e.0.starts_with(RETRY_TAG), "{e:?}");
    assert_eq!(
        std::fs::read_to_string(d.path().join(UD)).unwrap(),
        "folded"
    );
    assert!(core.methods().contains(&format!("data.endEdit {UD_EDIT}")));

    // 下一轮：重新扫描读到折回后的内容，写入放行
    *core.on_call.lock().unwrap() = None;
    let mut l2 = FsLocal::new(d.path().to_path_buf(), core.clone(), Echo::default());
    assert_eq!(l2.read(UD).unwrap().unwrap(), b"folded");
    l2.write_batch(&[(UD.into(), b"merged".to_vec())]).unwrap();
    assert_eq!(
        std::fs::read_to_string(d.path().join(UD)).unwrap(),
        "merged"
    );
}

#[test]
fn new_local_file_skips_begin_edit_and_triggers_rescan() {
    let (d, core, mut l, _) = rig();
    // beginEdit 对不存在的文件会被拒：即使这样整批也要成功
    core.fail_on(&format!("data.beginEdit {UD_EDIT}"));
    l.write_batch(&[
        ("config.toml".into(), b"a=1\n".to_vec()),
        (UD.into(), b"u".to_vec()),
    ])
    .unwrap();
    assert_eq!(std::fs::read_to_string(d.path().join(UD)).unwrap(), "u");
    assert_eq!(core.methods(), vec!["config.reload", "dict.reload"]);
}

#[test]
fn user_opened_session_is_left_alone_and_batch_is_retried_later() {
    let (d, core, mut l, _) = rig();
    put(d.path(), UD, "mine");
    core.user_sessions.lock().unwrap().push(UD_EDIT.into());
    let e = l
        .write_batch(&[
            ("config.toml".into(), b"a".to_vec()),
            (UD.into(), b"remote".to_vec()),
        ])
        .unwrap_err();
    assert!(e.0.starts_with(RETRY_TAG) && e.0.contains(UD_EDIT), "{e:?}");
    assert!(
        core.methods().is_empty(),
        "不 begin、不 end：{:?}",
        core.methods()
    );
    assert_eq!(std::fs::read_to_string(d.path().join(UD)).unwrap(), "mine");
    assert!(!d.path().join("config.toml").exists());
}

#[test]
fn begin_edit_timeout_also_tries_to_end_the_session() {
    let (d, core, mut l, _) = rig();
    put(d.path(), UD, "old");
    core.timeout
        .lock()
        .unwrap()
        .push(format!("data.beginEdit {UD_EDIT}"));
    assert!(l.write_batch(&[(UD.into(), b"x".to_vec())]).is_err());
    assert_eq!(
        core.methods(),
        vec![
            format!("data.beginEdit {UD_EDIT}"),
            format!("data.endEdit {UD_EDIT}")
        ]
    );
}

#[test]
fn end_edit_is_retried_once() {
    let (d, core, mut l, _) = rig();
    put(d.path(), UD, "old");
    core.fail_once
        .lock()
        .unwrap()
        .push(format!("data.endEdit {UD_EDIT}"));
    l.write_batch(&[(UD.into(), b"x".to_vec())]).unwrap();
    let ends = core
        .methods()
        .iter()
        .filter(|m| m.starts_with("data.endEdit"))
        .count();
    assert_eq!(ends, 2);
}

#[test]
fn cancel_after_begin_ends_session_and_writes_nothing() {
    let (d, core, l, _) = rig();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut l = l.with_cancel(cancel.clone());
    put(d.path(), UD, "old");
    *core.on_call.lock().unwrap() = Some(Box::new(move |m, _| {
        if m == "data.beginEdit" {
            cancel.store(true, Ordering::SeqCst);
        }
    }));
    assert!(l.write_batch(&[(UD.into(), b"x".to_vec())]).is_err());
    assert_eq!(std::fs::read_to_string(d.path().join(UD)).unwrap(), "old");
    assert!(core.methods().contains(&format!("data.endEdit {UD_EDIT}")));
}

#[test]
fn echo_keeps_only_latest_hash_per_path() {
    let (_d, _core, mut l, echo) = rig();
    l.write_batch(&[("config.toml".into(), b"1".to_vec())])
        .unwrap();
    l.write_batch(&[("config.toml".into(), b"2".to_vec())])
        .unwrap();
    let e = lock(&echo);
    assert_eq!(e.len(), 1);
    assert_eq!(e["config.toml"], content_hash(b"2"));
}

#[test]
fn temp_names_are_unique_per_write() {
    let (d, _core, mut l, _) = rig();
    // 旧实现用固定名：残留的同名临时文件会被覆盖/误删；新实现不碰它
    put(d.path(), ".config.toml.wind-sync.tmp", "foreign");
    l.write_batch(&[("config.toml".into(), b"1".to_vec())])
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(d.path().join(".config.toml.wind-sync.tmp")).unwrap(),
        "foreign"
    );
}

fn ext_calls(core: &FakeCore) -> Vec<(String, Value)> {
    core.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m.starts_with("extensions."))
        .cloned()
        .collect()
}

#[test]
fn extension_writes_are_bracketed_by_suspend_and_resume() {
    let (_d, core, mut l, _) = rig();
    l.write_batch(&[
        ("extensions/keysound/keysound.exe".into(), b"MZ".to_vec()),
        ("extensions/keysound/extension.toml".into(), b"x".to_vec()),
        ("extensions/other/run".into(), b"y".to_vec()),
        ("config.toml".into(), b"a=1\n".to_vec()),
    ])
    .unwrap();
    let ids = json!({ "ids": ["keysound", "other"] });
    assert_eq!(
        ext_calls(&core),
        vec![
            ("extensions.suspend".to_string(), ids.clone()),
            ("extensions.resume".to_string(), ids)
        ]
    );
    // 先暂停、写完才恢复
    let all = core.methods();
    let pos = |m: &str| all.iter().position(|x| x == m).unwrap();
    assert!(pos("extensions.suspend") < pos("config.reload"));
    assert!(pos("config.reload") <= pos("extensions.resume"));
}

#[test]
fn extension_resume_is_sent_even_when_write_fails() {
    let (d, core, mut l, _) = rig();
    // 目标路径是目录：写入失败
    std::fs::create_dir_all(d.path().join("extensions/keysound/keysound.exe")).unwrap();
    let r = l.write_batch(&[("extensions/keysound/keysound.exe".into(), b"MZ".to_vec())]);
    assert!(r.is_err());
    let ids = json!({ "ids": ["keysound"] });
    assert_eq!(
        ext_calls(&core),
        vec![
            ("extensions.suspend".to_string(), ids.clone()),
            ("extensions.resume".to_string(), ids)
        ]
    );
}

#[test]
fn extension_delete_is_also_bracketed() {
    let (d, core, mut l, _) = rig();
    put(d.path(), "extensions/keysound/old.dll", "x");
    l.delete(&["extensions/keysound/old.dll".into()]).unwrap();
    assert_eq!(ext_calls(&core).len(), 2);
}

#[test]
fn no_extension_paths_means_no_suspend() {
    let (_d, core, mut l, _) = rig();
    l.write_batch(&[("config.toml".into(), b"a=1\n".to_vec())])
        .unwrap();
    assert!(ext_calls(&core).is_empty());
}

#[test]
fn refused_suspend_aborts_the_batch_without_writing() {
    let (d, core, mut l, _) = rig();
    core.fail_on("extensions.suspend");
    let r = l.write_batch(&[("extensions/keysound/keysound.exe".into(), b"MZ".to_vec())]);
    assert!(r.is_err());
    assert!(!d.path().join("extensions/keysound/keysound.exe").exists());
}

#[cfg(unix)]
#[test]
fn exec_bit_is_set_and_read_back_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let (d, _c, mut l, _) = rig();
    l.write_batch(&[("extensions/k/run".into(), b"#!/bin/sh\n".to_vec())])
        .unwrap();
    assert!(!l.is_exec("extensions/k/run").unwrap());
    l.set_exec(&["extensions/k/run".into()]).unwrap();
    let mode = std::fs::metadata(d.path().join("extensions/k/run"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o100, 0);
    // 其余位保留
    assert_eq!(mode & 0o044, 0o044);
    assert!(l.is_exec("extensions/k/run").unwrap());
}
