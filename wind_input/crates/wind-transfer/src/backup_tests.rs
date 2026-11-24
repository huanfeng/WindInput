use super::*;
use std::fs;

/// 空 sources/targets（只考察 store 数据域时用，文件域一概不带）。
fn empty_sources() -> BackupSources<'static> {
    BackupSources {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    }
}

fn empty_targets() -> RestoreTargets<'static> {
    RestoreTargets {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    }
}

fn open_store(dir: &std::path::Path, tag: &str) -> Store {
    Store::open(dir.join(format!("{tag}.redb"))).unwrap()
}

/// 补全学习数据（邮箱后缀 + 网址历史）必须随整机备份走一个来回。
///
/// 这两类是键不带方案的全局段，不在 `list_data_schemas()` 的逐 schema 循环里：漏登记
/// `create_backup` 时备份与还原都不报错，用户换机后才发现学的东西全没了。
#[test]
fn backup_roundtrip_carries_completion_learning_data() {
    use wind_store::completion::CompletionKind;
    let t = tempfile::tempdir().unwrap();
    let src = open_store(t.path(), "src");
    src.record_completion(CompletionKind::EmailSuffix, "mycorp.cn")
        .unwrap();
    src.record_completion(CompletionKind::EmailSuffix, "mycorp.cn")
        .unwrap();
    src.record_completion(CompletionKind::UrlHistory, "www.example.com")
        .unwrap();

    let pkg = t.path().join("b.zip");
    let r = create_backup(
        &src,
        &empty_sources(),
        &pkg,
        "0.0.0",
        "test",
        "2026-09-19T00:00:00Z",
        &BackupOptions {
            include_stats: false,
            include_state: false,
        },
    )
    .unwrap();
    assert!(
        r.entries.iter().any(|e| e == "userdata/completions.jsonl"),
        "备份包里应有补全学习数据这一段，实际条目：{:?}",
        r.entries
    );

    let dst = open_store(t.path(), "dst");
    restore_backup(
        &pkg,
        &dst,
        &empty_targets(),
        crate::merge::Strategy::Replace,
        None,
    )
    .unwrap();
    assert_eq!(
        dst.get_completion(CompletionKind::EmailSuffix, "mycorp.cn")
            .unwrap()
            .expect("邮箱后缀应被还原")
            .count,
        2,
        "次数要一并还原，否则还原后排序全乱"
    );
    assert_eq!(
        dst.list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap()
            .1,
        1,
        "网址历史也该被还原"
    );
}

/// `--sections completion` 能单独还原这一段。
///
/// 这条钉的是 `RESTORE_SECTIONS` 那张与 `create_backup` 无编译期约束的白名单：
/// 漏登记不会报错，只是 `--sections` 里写了也被静默丢弃。
#[test]
fn completion_is_selectable_as_its_own_restore_section() {
    use wind_store::completion::CompletionKind;
    let t = tempfile::tempdir().unwrap();
    let src = open_store(t.path(), "s2");
    src.record_completion(CompletionKind::UrlHistory, "www.a.com")
        .unwrap();
    let pkg = t.path().join("b2.zip");
    create_backup(
        &src,
        &empty_sources(),
        &pkg,
        "0.0.0",
        "test",
        "2026-09-19T00:00:00Z",
        &BackupOptions {
            include_stats: false,
            include_state: false,
        },
    )
    .unwrap();

    let dst = open_store(t.path(), "d2");
    let sections = vec!["completion".to_string()];
    let r = restore_backup(
        &pkg,
        &dst,
        &empty_targets(),
        crate::merge::Strategy::Replace,
        Some(&sections),
    )
    .unwrap();
    assert!(
        r.restored.iter().any(|p| p == "userdata/completions.jsonl"),
        "只选 completion 这一段时它必须被还原，实际：{:?}",
        r.restored
    );
    assert_eq!(
        dst.list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap()
            .1,
        1
    );
}

/// 造一份最小可用的 wdat 字节流（供下面两条「替换后不得复用旧 reader」用）。
fn wdat_bytes(code: &str, text: &str) -> Vec<u8> {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("x.wdat");
    let mut d = wind_dict::codetable::CodetableDict::empty();
    d.merge_single(code.into(), text.into(), 1, 0);
    let mut w = wind_dict::datformat::WdatWriter::new();
    d.export_to_wdat(&mut w);
    w.write(&p).unwrap();
    fs::read(&p).unwrap()
}

/// 把 mtime 拨回替换前，并校验「大小 + mtime」确已逐字段相同。
///
/// 池的 `(大小, mtime)` 判据在正常时序下本就能认出替换，不把前提做成确定的，测的就只是
/// 那道判据，删掉通知照样绿（同 `wind_dict::reader_pool` 的同名夹具）。
fn force_same_stamp(path: &std::path::Path, before: &std::fs::Metadata) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_accessed(before.accessed().unwrap())
                .set_modified(before.modified().unwrap()),
        )
        .unwrap();
    let after = fs::metadata(path).unwrap();
    assert_eq!(
        (before.len(), before.modified().unwrap()),
        (after.len(), after.modified().unwrap()),
        "前提没造出来：替换后大小或 mtime 仍有差别，本测试会退化成在测 stamp 判据"
    );
}

/// 方案导入覆盖 sidecar `.wdat` 后，运行中的引擎不得再拿到旧内容。
///
/// 包里可以只带 `.wdat`（见 `collect_packs_wdat_only_dict`），那种词库直接走 `reader_pool`
/// 打开，导入时运行中的引擎手里还攥着旧映射。
#[test]
fn importing_a_pooled_wdat_invalidates_the_reader() {
    let t = tempfile::tempdir().unwrap();
    let user = t.path().join("u");
    fs::create_dir_all(user.join("wb")).unwrap();
    let target = user.join("wb/main.wdat");
    fs::write(&target, wdat_bytes("aa", "旧")).unwrap();
    let before = fs::metadata(&target).unwrap();

    let held = wind_dict::reader_pool::open_wdat(&target).unwrap();
    assert_eq!(held.search("aa").len(), 1);

    crate::scheme::write_staged(
        &[("wb/main.wdat".to_string(), wdat_bytes("bb", "新"))],
        &user,
        crate::merge::Strategy::Replace,
    )
    .unwrap();
    force_same_stamp(&target, &before);

    let fresh = wind_dict::reader_pool::open_wdat(&target).unwrap();
    assert_eq!(fresh.search("bb").len(), 1, "导入后必须读到新词库");
    assert!(fresh.search("aa").is_empty(), "旧内容不该还在");
}

/// 备份还原覆盖 sidecar `.wdat` 后同理——还原的 `schema_file` 来自
/// `walk_dir(user_schemas_dir)`，同样囊括 `.wdat`。
///
/// 导入与还原是两个写盘口、各挂各的通知，合成一条就会漏一个还能过。
#[test]
fn restoring_a_pooled_wdat_invalidates_the_reader() {
    let t = tempfile::tempdir().unwrap();
    let target = t.path().join("wb/main.wdat");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, wdat_bytes("aa", "旧")).unwrap();
    let before = fs::metadata(&target).unwrap();

    let held = wind_dict::reader_pool::open_wdat(&target).unwrap();
    assert_eq!(held.search("aa").len(), 1);

    assert!(
        write_file(
            &target,
            &wdat_bytes("bb", "新"),
            crate::merge::Strategy::Replace
        )
        .unwrap()
    );
    force_same_stamp(&target, &before);

    let fresh = wind_dict::reader_pool::open_wdat(&target).unwrap();
    assert_eq!(fresh.search("bb").len(), 1, "还原后必须读到新词库");
    assert!(fresh.search("aa").is_empty(), "旧内容不该还在");
}

fn seed_store(dir: &std::path::Path) -> wind_store::store::Store {
    let s = wind_store::store::Store::open(dir.join("t.redb")).unwrap();
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    s.learn_temp_word("wb", "ab", "临", 5, 0).unwrap();
    s.record_freq("wb", "a", "工").unwrap();
    s.pin_shadow("wb", "aa", "恭", None, 0).unwrap();
    s.add_phrase("bj", "北京", 0, 10).unwrap();
    // 常用字覆盖：全局段（键不带方案），两个方向各一条。
    s.set_common_char_override("槮", true).unwrap();
    s.set_common_char_override("的", false).unwrap();
    // 字符类用户层：全局段，value 是 yaml 文本。
    s.set_charset_doc("emoji", "key: emoji\ndefault: rare\n")
        .unwrap();
    s
}

#[test]
fn create_backup_covers_all_sections() {
    let t = tempfile::tempdir().unwrap();
    let s = seed_store(t.path());
    // 文件域 fixtures
    let cfg = t.path().join("config.toml");
    fs::write(&cfg, "[ui]\n").unwrap();
    let compat = t.path().join("compat.toml");
    fs::write(&compat, "[[apps]]\nprocess = \"Weixin.exe\"\n").unwrap();
    let schemas = t.path().join("schemas");
    fs::create_dir_all(schemas.join("my")).unwrap();
    fs::write(schemas.join("my.schema.toml"), "[schema]\nid=\"my\"\n").unwrap();
    fs::write(schemas.join("my/d.yaml"), "d").unwrap();
    let overrides = t.path().join("schema_overrides");
    fs::create_dir_all(&overrides).unwrap();
    fs::write(overrides.join("my.toml"), "auto_pair = false\n").unwrap();
    let themes = t.path().join("themes");
    fs::create_dir_all(themes.join("dark")).unwrap();
    fs::write(themes.join("dark/theme.toml"), "[meta]\nname=\"dark\"\n").unwrap();
    let state = t.path().join("state.toml");
    fs::write(&state, "[toolbar]\n").unwrap();

    let out = t.path().join("backup.zip");
    let src = BackupSources {
        user_config_file: Some(&cfg),
        compat_file: Some(&compat),
        user_schemas_dir: Some(&schemas),
        user_schema_overrides_dir: Some(&overrides),
        user_themes_dir: Some(&themes),
        state_file: Some(&state),
    };
    let r = create_backup(
        &s,
        &src,
        &out,
        "1.0.0",
        "windows",
        "t",
        &BackupOptions {
            include_stats: true,
            include_state: true,
        },
    )
    .unwrap();

    let m = crate::bundle::read_manifest(&out).unwrap();
    assert_eq!(m.kind, crate::bundle::BundleKind::Backup);
    let types: Vec<&str> = m.contents.iter().map(|e| e.r#type.as_str()).collect();
    for ty in [
        "config",
        "dict",
        "temp",
        "phrase",
        "freq",
        "shadow",
        "common_chars",
        "charset",
        "stats",
        "stats_meta",
        "schema_file",
        "schema_override_file",
        "theme_file",
        "state",
        "compat",
    ] {
        assert!(types.contains(&ty), "缺 {ty} 条目; got {types:?}");
    }
    // dict 条目路径与 meta.schema
    let dict = m.contents.iter().find(|e| e.r#type == "dict").unwrap();
    assert_eq!(dict.path, "userdata/user_words/wb.wdict");
    assert_eq!(dict.meta.get("schema").and_then(|v| v.as_str()), Some("wb"));
    // schema_file 递归含子目录文件
    assert!(m.contents.iter().any(|e| e.path == "schemas/my/d.yaml"));
    // schema_override_file 路径前缀正确
    assert!(
        m.contents
            .iter()
            .any(|e| e.path == "schema_overrides/my.toml")
    );
    // 载荷可取
    let bytes = crate::bundle::extract_entry(&out, "config/config.toml").unwrap();
    assert_eq!(bytes, b"[ui]\n");
    let compat_bytes = crate::bundle::extract_entry(&out, "config/compat.toml").unwrap();
    assert_eq!(compat_bytes, b"[[apps]]\nprocess = \"Weixin.exe\"\n");
    assert!(!r.entries.is_empty());
}

#[test]
fn restore_roundtrip_full() {
    let t = tempfile::tempdir().unwrap();
    let s = seed_store(t.path());
    let cfg = t.path().join("config.toml");
    std::fs::write(&cfg, "[ui]\n").unwrap();
    let compat = t.path().join("compat.toml");
    std::fs::write(&compat, "[[apps]]\nprocess = \"Weixin.exe\"\n").unwrap();
    let overrides = t.path().join("schema_overrides");
    std::fs::create_dir_all(&overrides).unwrap();
    std::fs::write(overrides.join("my.toml"), "auto_pair = false\n").unwrap();
    let out = t.path().join("b.zip");
    let src = BackupSources {
        user_config_file: Some(&cfg),
        compat_file: Some(&compat),
        user_schemas_dir: None,
        user_schema_overrides_dir: Some(&overrides),
        user_themes_dir: None,
        state_file: None,
    };
    create_backup(
        &s,
        &src,
        &out,
        "1.0.0",
        "windows",
        "t",
        &BackupOptions {
            include_stats: true,
            include_state: false,
        },
    )
    .unwrap();

    // 全新目标环境
    let t2 = tempfile::tempdir().unwrap();
    let s2 = wind_store::store::Store::open(t2.path().join("t2.redb")).unwrap();
    let cfg2 = t2.path().join("config.toml");
    let compat2 = t2.path().join("compat.toml");
    let overrides2 = t2.path().join("schema_overrides");
    let targets = RestoreTargets {
        user_config_file: Some(&cfg2),
        compat_file: Some(&compat2),
        user_schemas_dir: None,
        user_schema_overrides_dir: Some(&overrides2),
        user_themes_dir: None,
        state_file: None,
    };
    let r = restore_backup(&out, &s2, &targets, crate::merge::Strategy::Merge, None).unwrap();
    assert!(r.conflicts.is_empty());
    assert!(r.schemas_touched.contains(&"wb".to_string()));
    assert_eq!(std::fs::read(&cfg2).unwrap(), b"[ui]\n");
    assert_eq!(
        std::fs::read(&compat2).unwrap(),
        b"[[apps]]\nprocess = \"Weixin.exe\"\n"
    );
    assert_eq!(
        std::fs::read(overrides2.join("my.toml")).unwrap(),
        b"auto_pair = false\n"
    );
    assert_eq!(s2.get_user_words("wb", "a").unwrap()[0].weight, 100);
    assert_eq!(s2.get_temp_word("wb", "ab", "临").unwrap(), Some(1));
    assert_eq!(s2.get_freq("wb", "a", "工").unwrap().unwrap().count, 1);
    assert_eq!(s2.list_shadow_rules("wb").unwrap().len(), 1);
    assert!(s2.list_phrases().unwrap().iter().any(|p| p.code == "bj"));
    // 常用字覆盖：两个方向都要还原（只还原一个方向的话，用户会发现「我降级过的字
    // 回来了、升级过的还在」这种半吊子状态）。
    assert_eq!(s2.get_common_char_override("槮").unwrap(), Some(true));
    assert_eq!(s2.get_common_char_override("的").unwrap(), Some(false));
}

#[test]
fn restore_sections_filter_and_merge_conflict() {
    let t = tempfile::tempdir().unwrap();
    let s = seed_store(t.path());
    let cfg = t.path().join("config.toml");
    std::fs::write(&cfg, "[ui]\n").unwrap();
    let out = t.path().join("b.zip");
    let src = BackupSources {
        user_config_file: Some(&cfg),
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    create_backup(
        &s,
        &src,
        &out,
        "1.0.0",
        "windows",
        "t",
        &BackupOptions {
            include_stats: false,
            include_state: false,
        },
    )
    .unwrap();

    let t2 = tempfile::tempdir().unwrap();
    let s2 = wind_store::store::Store::open(t2.path().join("t2.redb")).unwrap();
    let cfg2 = t2.path().join("config.toml");
    std::fs::write(&cfg2, "LOCAL").unwrap();
    let targets = RestoreTargets {
        user_config_file: Some(&cfg2),
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    // 只还原 config；Merge 下本地已存在 → conflict，内容不变
    let sections = vec!["config".to_string()];
    let r = restore_backup(
        &out,
        &s2,
        &targets,
        crate::merge::Strategy::Merge,
        Some(&sections),
    )
    .unwrap();
    assert_eq!(r.conflicts, vec!["config/config.toml"]);
    assert_eq!(std::fs::read(&cfg2).unwrap(), b"LOCAL");
    assert!(
        s2.get_user_words("wb", "a").unwrap().is_empty(),
        "dict 未在 sections，不还原"
    );
    // Replace 覆盖
    let r2 = restore_backup(
        &out,
        &s2,
        &targets,
        crate::merge::Strategy::Replace,
        Some(&sections),
    )
    .unwrap();
    assert!(r2.conflicts.is_empty());
    assert_eq!(std::fs::read(&cfg2).unwrap(), b"[ui]\n");
}

/// 联想历史（t185 方案 B）随「freq」段备份 / 还原：选了 `freq` 段就带上它。
#[test]
fn assoc_history_roundtrips_under_freq_section() {
    let t = tempfile::tempdir().unwrap();
    let s = seed_store(t.path());
    s.record_assoc_pick("wb", "荷", "荷枪实弹").unwrap();
    s.record_assoc_pick("wb", "荷", "荷枪实弹").unwrap();
    let out = t.path().join("b.zip");
    let src = BackupSources {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    let opts = BackupOptions {
        include_stats: false,
        include_state: false,
    };
    create_backup(&s, &src, &out, "1.0.0", "windows", "t", &opts).unwrap();

    let t2 = tempfile::tempdir().unwrap();
    let s2 = wind_store::store::Store::open(t2.path().join("t2.redb")).unwrap();
    s2.record_assoc_pick("wb", "荷", "荷花").unwrap(); // 本地旧记录
    let targets = RestoreTargets {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    let sections = vec!["freq".to_string()];
    restore_backup(
        &out,
        &s2,
        &targets,
        crate::merge::Strategy::Replace,
        Some(&sections),
    )
    .unwrap();
    let got = s2.assoc_history("wb", "荷", 0).unwrap();
    assert_eq!(got.len(), 1, "Replace 清掉本地旧记录：{got:?}");
    assert_eq!((got[0].0.as_str(), got[0].1.count), ("荷枪实弹", 2));
}

/// 备份里**没有**联想历史条目（源库无历史 / 旧备份）时，Replace 还原 freq 段也清空本地历史。
#[test]
fn freq_replace_clears_assoc_history_even_without_entry() {
    let t = tempfile::tempdir().unwrap();
    let s = seed_store(t.path());
    let out = t.path().join("b.zip");
    let src = BackupSources {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    let opts = BackupOptions {
        include_stats: false,
        include_state: false,
    };
    create_backup(&s, &src, &out, "1.0.0", "windows", "t", &opts).unwrap();

    let t2 = tempfile::tempdir().unwrap();
    let s2 = wind_store::store::Store::open(t2.path().join("t2.redb")).unwrap();
    s2.record_assoc_pick("wb", "荷", "荷花").unwrap(); // 本地旧记录
    let targets = RestoreTargets {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    let sections = vec!["freq".to_string()];
    restore_backup(
        &out,
        &s2,
        &targets,
        crate::merge::Strategy::Replace,
        Some(&sections),
    )
    .unwrap();
    assert_eq!(s2.get_freq("wb", "a", "工").unwrap().unwrap().count, 1);
    let got = s2.assoc_history("wb", "荷", 0).unwrap();
    assert!(got.is_empty(), "Replace freq 段应清掉本地联想历史：{got:?}");
}

#[test]
fn restore_replace_clears_data_domain() {
    let t = tempfile::tempdir().unwrap();
    let s = seed_store(t.path());
    let out = t.path().join("b.zip");
    let src = BackupSources {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    create_backup(
        &s,
        &src,
        &out,
        "1.0.0",
        "windows",
        "t",
        &BackupOptions {
            include_stats: false,
            include_state: false,
        },
    )
    .unwrap();

    let t2 = tempfile::tempdir().unwrap();
    let s2 = wind_store::store::Store::open(t2.path().join("t2.redb")).unwrap();
    s2.add_user_word("wb", "zz", "杂", 1, 0).unwrap(); // 本地杂词
    let targets = RestoreTargets {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    let sections = vec!["dict".to_string()];
    restore_backup(
        &out,
        &s2,
        &targets,
        crate::merge::Strategy::Replace,
        Some(&sections),
    )
    .unwrap();
    let all = s2.search_user_words_prefix("wb", "", 0).unwrap();
    assert_eq!(all.len(), 1, "Replace 清掉杂词只剩备份内容");
    assert_eq!(all[0].code, "a");
}

#[test]
fn create_backup_options_exclude() {
    let t = tempfile::tempdir().unwrap();
    let s = seed_store(t.path());
    let out = t.path().join("b2.zip");
    let src = BackupSources {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    create_backup(
        &s,
        &src,
        &out,
        "1.0.0",
        "windows",
        "t",
        &BackupOptions {
            include_stats: false,
            include_state: false,
        },
    )
    .unwrap();
    let m = crate::bundle::read_manifest(&out).unwrap();
    let types: Vec<&str> = m.contents.iter().map(|e| e.r#type.as_str()).collect();
    assert!(!types.contains(&"stats"), "include_stats=false 不含 stats");
    assert!(!types.contains(&"state"));
    assert!(!types.contains(&"config"), "无 config 源则无 config 条目");
    assert!(!types.contains(&"compat"), "无 compat 源则无 compat 条目");
    assert!(types.contains(&"dict"), "store 数据域始终导出");
}

#[test]
fn section_of_maps_compat_and_schema_override_under_existing_sections() {
    // compat/schema_override_file 不是独立的还原范围勾选项，而是并入既有的
    // 「配置」「用户方案」段——否则设置页勾了「配置」却漏还原 compat.toml。
    assert_eq!(section_of("compat"), "config");
    assert_eq!(section_of("schema_override_file"), "schemas");
}

/// 字符类的用户层进备份包、还原回库：换机器之后用户调过的类不能静默丢失。
///
/// 它在库里而不是目录里，故走 store 的 list / set 而不是 `user_*_dir`。文本原样进出。
#[test]
fn quick_format_is_backed_up_and_restored() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("q.zip");
    let store = open_store(t.path(), "q1");
    store.add_quick_format("date", "my", "yyyy").unwrap();
    let r = create_backup(
        &store,
        &empty_sources(),
        &out,
        "0",
        "test",
        "2026-10-15T00:00:00Z",
        &BackupOptions {
            include_stats: false,
            include_state: false,
        },
    )
    .unwrap();
    assert!(
        r.entries.iter().any(|e| e == "userdata/quick_format.yaml"),
        "{:?}",
        r.entries
    );
    let store2 = open_store(t.path(), "q2");
    restore_backup(
        &out,
        &store2,
        &empty_targets(),
        crate::merge::Strategy::Replace,
        Some(&["quick_format".to_string()]),
    )
    .unwrap();
    assert_eq!(
        store2.get_quick_format("date").unwrap().unwrap().added[0].id,
        "my"
    );
}

#[test]
fn charsets_are_backed_up_and_restored() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("b.zip");
    let store = Store::open(t.path().join("s.redb")).unwrap();
    let text =
        "# 头注释\n---\nkey: 我的符号\nname: 我的符号\nranges: [U+2600-U+26FF]\n...\n★\n-☯\n";
    store.set_charset_doc("我的符号", text).unwrap();
    store
        .set_charset_doc("emoji", "key: emoji\ndefault: rare\n")
        .unwrap();
    let src = BackupSources {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    let r = create_backup(
        &store,
        &src,
        &out,
        "0",
        "test",
        "2026-09-05T00:00:00Z",
        &BackupOptions {
            include_stats: false,
            include_state: false,
        },
    )
    .unwrap();
    assert!(
        r.entries.iter().any(|e| e == "charsets/我的符号.yaml"),
        "字符类没进备份包：{:?}",
        r.entries
    );

    let store2 = Store::open(t.path().join("s2.redb")).unwrap();
    // 目标库里先放一条「本机的」调整：Replace 模式该把它清掉。
    store2
        .set_charset_doc("local_only", "key: local_only\n")
        .unwrap();
    let targets = RestoreTargets {
        user_config_file: None,
        compat_file: None,
        user_schemas_dir: None,
        user_schema_overrides_dir: None,
        user_themes_dir: None,
        state_file: None,
    };
    restore_backup(
        &out,
        &store2,
        &targets,
        crate::merge::Strategy::Replace,
        None,
    )
    .unwrap();
    assert_eq!(
        store2.get_charset_doc("我的符号").unwrap().as_deref(),
        Some(text),
        "文本必须原样还原"
    );
    assert!(store2.get_charset_doc("emoji").unwrap().is_some());
    assert_eq!(
        store2.get_charset_doc("local_only").unwrap(),
        None,
        "Replace 模式先清空再还原"
    );
}

/// 用户分库与开关文件原样进包（归 dict 段），还原写回 `data/` 下同一位置。
#[test]
fn user_dict_libs_roundtrip_as_files() {
    let t = tempfile::tempdir().unwrap();
    let s = open_store(t.path(), "src");
    s.create_user_dict("pinyin", "成语").unwrap();
    s.set_user_dict_enabled("pinyin", "shuangpin", "成语", false)
        .unwrap();
    let root = s.user_text_dir().to_path_buf();
    let lib = root.join("pinyin/dicts/成语.dict.yaml");
    let lib_bytes = fs::read(&lib).unwrap();
    let meta_bytes = fs::read(root.join("pinyin/dicts.toml")).unwrap();
    // `.backup/` 里的上一版 / 坏文件留存不进包
    fs::create_dir_all(root.join(".backup/dicts")).unwrap();
    fs::write(root.join(".backup/dicts.toml"), "x").unwrap();
    fs::write(root.join(".backup/dicts/坏.dict.yaml"), "---\n...\n").unwrap();
    let pkg = t.path().join("b.zip");
    create_backup(
        &s,
        &empty_sources(),
        &pkg,
        "0",
        "test",
        "t",
        &BackupOptions {
            include_stats: false,
            include_state: false,
        },
    )
    .unwrap();
    let m = crate::bundle::read_manifest(&pkg).unwrap();
    let entry = |p: &str| m.contents.iter().find(|e| e.path == p).cloned();
    let e = entry("data/pinyin/dicts/成语.dict.yaml").expect("分库进包");
    assert_eq!(e.r#type, "user_dict_file");
    assert_eq!(
        e.meta.get("schema").and_then(|v| v.as_str()),
        Some("pinyin")
    );
    assert_eq!(
        entry("data/pinyin/dicts.toml").unwrap().r#type,
        "user_dicts_meta"
    );
    assert!(
        m.contents.iter().all(|e| !e.path.contains(".backup")),
        "{:?}",
        m.contents.iter().map(|e| &e.path).collect::<Vec<_>>()
    );
    assert_eq!(section_of("user_dict_file"), "dict");
    assert_eq!(section_of("user_dicts_meta"), "dict");

    // 只还原 temp 段：分库不动
    let t2 = tempfile::tempdir().unwrap();
    let s2 = open_store(t2.path(), "dst");
    let root2 = s2.user_text_dir().to_path_buf();
    let only_temp = vec!["temp".to_string()];
    restore_backup(
        &pkg,
        &s2,
        &empty_targets(),
        crate::merge::Strategy::Merge,
        Some(&only_temp),
    )
    .unwrap();
    assert!(!root2.join("pinyin/dicts/成语.dict.yaml").exists());

    let r = restore_backup(
        &pkg,
        &s2,
        &empty_targets(),
        crate::merge::Strategy::Merge,
        None,
    )
    .unwrap();
    assert_eq!(
        fs::read(root2.join("pinyin/dicts/成语.dict.yaml")).unwrap(),
        lib_bytes
    );
    assert_eq!(
        fs::read(root2.join("pinyin/dicts.toml")).unwrap(),
        meta_bytes
    );
    assert!(
        r.restored
            .contains(&"data/pinyin/dicts/成语.dict.yaml".to_string())
    );

    // 合并：已有同名文件算冲突、不覆盖；替换：覆盖
    fs::write(root2.join("pinyin/dicts/成语.dict.yaml"), "mine").unwrap();
    let r = restore_backup(
        &pkg,
        &s2,
        &empty_targets(),
        crate::merge::Strategy::Merge,
        None,
    )
    .unwrap();
    assert!(
        r.conflicts
            .contains(&"data/pinyin/dicts/成语.dict.yaml".to_string())
    );
    assert_eq!(
        fs::read(root2.join("pinyin/dicts/成语.dict.yaml")).unwrap(),
        b"mine"
    );
    restore_backup(
        &pkg,
        &s2,
        &empty_targets(),
        crate::merge::Strategy::Replace,
        None,
    )
    .unwrap();
    assert_eq!(
        fs::read(root2.join("pinyin/dicts/成语.dict.yaml")).unwrap(),
        lib_bytes
    );
}

/// 分库条目的数据方案段也要是合法方案 id。
#[test]
fn user_dict_rel_checks_schema_segment() {
    assert!(is_user_dict_rel("pinyin/dicts.toml", "user_dicts_meta"));
    assert!(is_user_dict_rel(
        "wb/dicts/我的.dict.yaml",
        "user_dict_file"
    ));
    for bad in [".x/dicts.toml", "x /dicts.toml", "x./dicts.toml"] {
        assert!(!is_user_dict_rel(bad, "user_dicts_meta"), "{bad}");
    }
    assert!(!is_user_dict_rel(".x/dicts/a.dict.yaml", "user_dict_file"));
}

/// 还原遇到正在外部编辑的文件：记为冲突跳过、不整写（编辑器还开着），其余照常还原。
#[test]
fn restore_skips_files_under_external_edit() {
    let t = tempfile::tempdir().unwrap();
    let s = seed_store(t.path());
    s.create_user_dict("pinyin", "成语").unwrap();
    s.set_user_dict_enabled("pinyin", "pinyin", "成语", false)
        .unwrap();
    let pkg = t.path().join("b.zip");
    create_backup(
        &s,
        &empty_sources(),
        &pkg,
        "0",
        "test",
        "t",
        &BackupOptions {
            include_stats: false,
            include_state: false,
        },
    )
    .unwrap();
    let m = crate::bundle::read_manifest(&pkg).unwrap();
    let charset_entry = m
        .contents
        .iter()
        .find(|e| e.r#type == "charset")
        .unwrap()
        .path
        .clone();

    let t2 = tempfile::tempdir().unwrap();
    let s2 = open_store(t2.path(), "dst");
    s2.add_user_word("wb", "zz", "杂", 1, 0).unwrap();
    s2.create_user_dict("pinyin", "成语").unwrap();
    s2.set_charset_doc("emoji", "key: emoji\ndefault: common\n")
        .unwrap();
    for f in [
        "wb/user.dict.yaml",
        "wb/shadow.dict.yaml",
        "phrases.dict.yaml",
        "common_chars.yaml",
        "charsets/emoji.yaml",
        "pinyin/dicts/成语.dict.yaml",
        "pinyin/dicts.toml",
    ] {
        s2.begin_edit(f).unwrap();
    }
    let words_file = s2.user_text_path("wb");
    let words_before = fs::read(&words_file).unwrap();
    let r = restore_backup(
        &pkg,
        &s2,
        &empty_targets(),
        crate::merge::Strategy::Replace,
        None,
    )
    .unwrap();
    for p in [
        "userdata/user_words/wb.wdict",
        "userdata/shadow/wb.jsonl",
        "userdata/phrases.wdict",
        "userdata/common_chars.jsonl",
        charset_entry.as_str(),
        "data/pinyin/dicts/成语.dict.yaml",
        "data/pinyin/dicts.toml",
    ] {
        assert!(r.conflicts.iter().any(|c| c == p), "{p}: {:?}", r.conflicts);
    }
    assert_eq!(
        fs::read(&words_file).unwrap(),
        words_before,
        "编辑中的文件不整写"
    );
    assert!(
        r.restored
            .iter()
            .any(|p| p == "userdata/temp_words/wb.wdict"),
        "其余照常还原：{:?}",
        r.restored
    );
}
