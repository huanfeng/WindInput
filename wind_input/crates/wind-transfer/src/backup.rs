//! 整机备份：config、compat 兼容规则、逐表用户数据（文本）、
//! 用户方案/方案覆盖层/主题目录、可选 state，
//! 组合 bundle/merge/store 导出原语打成 kind=backup 的自描述 zip。
use crate::bundle::{BundleKind, BundleWriter, Manifest};
use std::path::{Path, PathBuf};
use wind_store::store::Store;

pub struct BackupOptions {
    pub include_stats: bool,
    pub include_state: bool,
}

pub struct BackupSources<'a> {
    pub user_config_file: Option<&'a Path>,
    /// 用户层应用兼容规则（`{user_config_dir}/compat.toml`，右键菜单管理）。
    pub compat_file: Option<&'a Path>,
    pub user_schemas_dir: Option<&'a Path>,
    /// 方案配置覆盖层目录（`{user_config_dir}/schema_overrides/<id>.toml`），
    /// 与 `user_schemas_dir`（方案文件本体）是不同目录，须分别打包。
    pub user_schema_overrides_dir: Option<&'a Path>,
    pub user_themes_dir: Option<&'a Path>,
    /// 用户层字符类定义目录（`{user_config_dir}/charsets/*.yaml`，设置页「字符集分类」
    /// 管理）。
    ///
    /// ⚠️ 与 `compat.toml` / `schema_overrides/` 同类：都是用户层配置、都在设置页里改。
    /// 漏掉它的表现是**静默的**——备份成功、还原成功，只是换了机器之后「我调过的那些
    /// 字符类」没了，而用户想不到去怀疑备份。
    pub user_charsets_dir: Option<&'a Path>,
    pub state_file: Option<&'a Path>,
}

pub struct BackupResult {
    pub path: PathBuf,
    pub entries: Vec<String>,
}

/// 递归收集目录下全部文件的 (zip条目名, 绝对路径)；条目名 = prefix + 目录相对路径（`/`分隔）。
fn walk_dir(dir: &Path, prefix: &str) -> anyhow::Result<Vec<(String, PathBuf)>> {
    debug_assert!(prefix.ends_with('/'), "prefix 须带尾斜杠(如 schemas/)");
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                let rel = p
                    .strip_prefix(dir)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((format!("{prefix}{rel}"), p));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// 创建整机备份。schema 清单取 `store.list_data_schemas()`（覆盖有数据但未启用的方案）。
pub fn create_backup(
    store: &Store,
    src: &BackupSources,
    out_path: &Path,
    app_version: &str,
    platform: &str,
    created_at: &str,
    opts: &BackupOptions,
) -> anyhow::Result<BackupResult> {
    let manifest = Manifest::new(BundleKind::Backup, app_version, platform, created_at);
    let mut w = BundleWriter::new(out_path, manifest)?;
    let mut entries = Vec::new();
    let mut add = |w: &mut BundleWriter,
                   name: String,
                   data: &[u8],
                   ty: &str,
                   meta: serde_json::Value|
     -> anyhow::Result<()> {
        w.add_bytes_with(&name, data, ty, meta)?;
        entries.push(name);
        Ok(())
    };

    // 文件域：config / state
    if let Some(cfg) = src.user_config_file
        && cfg.is_file()
    {
        add(
            &mut w,
            "config/config.toml".into(),
            &std::fs::read(cfg)?,
            "config",
            serde_json::Value::Null,
        )?;
    }
    if let Some(compat) = src.compat_file
        && compat.is_file()
    {
        add(
            &mut w,
            "config/compat.toml".into(),
            &std::fs::read(compat)?,
            "compat",
            serde_json::Value::Null,
        )?;
    }
    if opts.include_state
        && let Some(st) = src.state_file
        && st.is_file()
    {
        add(
            &mut w,
            "state/state.toml".into(),
            &std::fs::read(st)?,
            "state",
            serde_json::Value::Null,
        )?;
    }

    // 数据域：逐 schema 四表 + 全局 phrases
    let schemas = store.list_data_schemas()?;
    for sc in &schemas {
        let meta = serde_json::json!({ "schema": sc });
        let words = store.export_user_words_wdict(sc, created_at)?;
        add(
            &mut w,
            format!("userdata/user_words/{sc}.wdict"),
            words.as_bytes(),
            "dict",
            meta.clone(),
        )?;
        let temp = store.export_temp_words_wdict(sc, created_at)?;
        add(
            &mut w,
            format!("userdata/temp_words/{sc}.wdict"),
            temp.as_bytes(),
            "temp",
            meta.clone(),
        )?;
        let freq = store.export_freq_jsonl(sc)?;
        add(
            &mut w,
            format!("userdata/freq/{sc}.jsonl"),
            freq.as_bytes(),
            "freq",
            meta.clone(),
        )?;
        let shadow = store.export_shadow_jsonl(sc)?;
        add(
            &mut w,
            format!("userdata/shadow/{sc}.jsonl"),
            shadow.as_bytes(),
            "shadow",
            meta,
        )?;
    }
    let phrases = store.export_user_phrases_wdict(created_at)?;
    add(
        &mut w,
        "userdata/phrases.wdict".into(),
        phrases.as_bytes(),
        "phrase",
        serde_json::Value::Null,
    )?;
    // 常用字表的用户覆盖：键不带方案，故与 phrases 一样是全局段，不进上面的逐 schema 循环。
    // 这是用户在候选上一个个点出来的数据，不备份就意味着换机重来一遍。
    let common_chars = store.export_common_chars_jsonl()?;
    add(
        &mut w,
        "userdata/common_chars.jsonl".into(),
        common_chars.as_bytes(),
        "common_chars",
        serde_json::Value::Null,
    )?;

    if opts.include_stats {
        let stats = store.export_stats_jsonl()?;
        add(
            &mut w,
            "userdata/stats.jsonl".into(),
            stats.as_bytes(),
            "stats",
            serde_json::Value::Null,
        )?;
        let meta = store.get_stats_meta()?;
        add(
            &mut w,
            "userdata/stats_meta.json".into(),
            serde_json::to_vec(&meta)?.as_slice(),
            "stats_meta",
            serde_json::Value::Null,
        )?;
    }

    // 文件域：用户方案 / 方案覆盖层 / 主题整目录
    if let Some(dir) = src.user_schemas_dir
        && dir.is_dir()
    {
        for (name, path) in walk_dir(dir, "schemas/")? {
            let data = std::fs::read(&path)?;
            add(&mut w, name, &data, "schema_file", serde_json::Value::Null)?;
        }
    }
    if let Some(dir) = src.user_schema_overrides_dir
        && dir.is_dir()
    {
        for (name, path) in walk_dir(dir, "schema_overrides/")? {
            let data = std::fs::read(&path)?;
            add(
                &mut w,
                name,
                &data,
                "schema_override_file",
                serde_json::Value::Null,
            )?;
        }
    }
    if let Some(dir) = src.user_themes_dir
        && dir.is_dir()
    {
        for (name, path) in walk_dir(dir, "themes/")? {
            let data = std::fs::read(&path)?;
            add(&mut w, name, &data, "theme_file", serde_json::Value::Null)?;
        }
    }
    if let Some(dir) = src.user_charsets_dir
        && dir.is_dir()
    {
        for (name, path) in walk_dir(dir, "charsets/")? {
            let data = std::fs::read(&path)?;
            add(&mut w, name, &data, "charset_file", serde_json::Value::Null)?;
        }
    }

    w.finish()?;
    Ok(BackupResult {
        path: out_path.to_path_buf(),
        entries,
    })
}

pub struct RestoreTargets<'a> {
    pub user_config_file: Option<&'a Path>,
    pub compat_file: Option<&'a Path>,
    pub user_schemas_dir: Option<&'a Path>,
    pub user_schema_overrides_dir: Option<&'a Path>,
    pub user_themes_dir: Option<&'a Path>,
    /// 用户层字符类定义目录（`{user_config_dir}/charsets/*.yaml`，设置页「字符集分类」
    /// 管理）。
    ///
    /// ⚠️ 与 `compat.toml` / `schema_overrides/` 同类：都是用户层配置、都在设置页里改。
    /// 漏掉它的表现是**静默的**——备份成功、还原成功，只是换了机器之后「我调过的那些
    /// 字符类」没了，而用户想不到去怀疑备份。
    pub user_charsets_dir: Option<&'a Path>,
    pub state_file: Option<&'a Path>,
}

pub struct RestoreResult {
    pub restored: Vec<String>,
    pub conflicts: Vec<String>,
    pub schemas_touched: Vec<String>,
}

/// 条目 type → section 名（sections 过滤用）。
fn section_of(ty: &str) -> &str {
    match ty {
        "schema_file" | "schema_override_file" => "schemas",
        "theme_file" => "themes",
        "stats_meta" => "stats",
        "compat" => "config",
        other => other,
    }
}

/// 写单个文件（tmp+rename；Merge 已存在→false 表示冲突跳过；Replace 先删旧）。
fn write_file(
    target: &Path,
    bytes: &[u8],
    strategy: crate::merge::Strategy,
) -> anyhow::Result<bool> {
    if target.exists() && strategy == crate::merge::Strategy::Merge {
        return Ok(false);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("windinput.tmp");
    std::fs::write(&tmp, bytes)?;
    if target.exists() {
        std::fs::remove_file(target)?;
    }
    std::fs::rename(&tmp, target)?;
    Ok(true)
}

/// 还原整机备份。sections=None 还原全部；数据域 Replace 先清对应表，文件域 Merge 跳过已存在。
pub fn restore_backup(
    package: &Path,
    store: &Store,
    targets: &RestoreTargets,
    strategy: crate::merge::Strategy,
    sections: Option<&[String]>,
) -> anyhow::Result<RestoreResult> {
    let manifest = crate::bundle::read_manifest(package)?;
    if manifest.kind != BundleKind::Backup {
        anyhow::bail!("不是整机备份(kind={:?})", manifest.kind);
    }
    let wanted = |ty: &str| -> bool {
        match sections {
            None => true,
            Some(ss) => ss.iter().any(|s| s == section_of(ty)),
        }
    };
    let replace = strategy == crate::merge::Strategy::Replace;
    let mut restored = Vec::new();
    let mut conflicts = Vec::new();
    let mut schemas_touched: std::collections::BTreeSet<String> = Default::default();
    // Replace 的数据域清库只做一次（phrases/stats 全局；四表按 schema 首次遇到时清）。
    let mut cleared: std::collections::HashSet<String> = Default::default();

    for e in &manifest.contents {
        if !wanted(&e.r#type) {
            continue;
        }
        let bytes = crate::bundle::extract_entry(package, &e.path)?;
        let text = || String::from_utf8_lossy(&bytes).into_owned();
        let schema = e.meta.get("schema").and_then(|v| v.as_str()).unwrap_or("");
        match e.r#type.as_str() {
            "config" => {
                if let Some(target) = targets.user_config_file {
                    if write_file(target, &bytes, strategy)? {
                        restored.push(e.path.clone());
                    } else {
                        conflicts.push(e.path.clone());
                    }
                }
            }
            "compat" => {
                if let Some(target) = targets.compat_file {
                    if write_file(target, &bytes, strategy)? {
                        restored.push(e.path.clone());
                    } else {
                        conflicts.push(e.path.clone());
                    }
                }
            }
            "state" => {
                if let Some(target) = targets.state_file {
                    if write_file(target, &bytes, strategy)? {
                        restored.push(e.path.clone());
                    } else {
                        conflicts.push(e.path.clone());
                    }
                }
            }
            "dict" if !schema.is_empty() => {
                if replace && cleared.insert(format!("dict:{schema}")) {
                    store.clear_user_words(schema)?;
                }
                store.import_user_words_wdict(schema, &text())?;
                schemas_touched.insert(schema.to_string());
                restored.push(e.path.clone());
            }
            "temp" if !schema.is_empty() => {
                if replace && cleared.insert(format!("temp:{schema}")) {
                    store.clear_temp_words(schema)?;
                }
                store.import_temp_words_wdict(schema, &text())?;
                schemas_touched.insert(schema.to_string());
                restored.push(e.path.clone());
            }
            "freq" if !schema.is_empty() => {
                if replace && cleared.insert(format!("freq:{schema}")) {
                    store.clear_freq(schema)?;
                }
                store.import_freq_jsonl(schema, &text())?;
                schemas_touched.insert(schema.to_string());
                restored.push(e.path.clone());
            }
            "shadow" if !schema.is_empty() => {
                if replace && cleared.insert(format!("shadow:{schema}")) {
                    store.clear_shadow(schema)?;
                }
                store.import_shadow_jsonl(schema, &text())?;
                schemas_touched.insert(schema.to_string());
                restored.push(e.path.clone());
            }
            "phrase" => {
                if replace && cleared.insert("phrase".into()) {
                    store.reset_user_phrases()?;
                }
                store.import_user_phrases_wdict(&text())?;
                restored.push(e.path.clone());
            }
            "common_chars" => {
                if replace && cleared.insert("common_chars".into()) {
                    store.clear_common_char_overrides()?;
                }
                store.import_common_chars_jsonl(&text())?;
                restored.push(e.path.clone());
            }
            "stats" => {
                if replace && cleared.insert("stats".into()) {
                    store.clear_stats()?;
                }
                store.import_stats_jsonl(&text(), replace)?;
                restored.push(e.path.clone());
            }
            "stats_meta" => {
                if replace {
                    let meta: wind_store::stats::StatsMeta = serde_json::from_slice(&bytes)?;
                    store.put_stats_meta(&meta)?;
                    restored.push(e.path.clone());
                }
                // Merge：保留本地 meta（streak 等本机累积），跳过不计冲突。
            }
            "schema_file" => {
                if let Some(dir) = targets.user_schemas_dir {
                    let rel = crate::bundle::validate_entry_rel(&e.path, "schemas/")?;
                    if write_file(&dir.join(rel), &bytes, strategy)? {
                        restored.push(e.path.clone());
                    } else {
                        conflicts.push(e.path.clone());
                    }
                }
            }
            "schema_override_file" => {
                if let Some(dir) = targets.user_schema_overrides_dir {
                    let rel = crate::bundle::validate_entry_rel(&e.path, "schema_overrides/")?;
                    if write_file(&dir.join(rel), &bytes, strategy)? {
                        restored.push(e.path.clone());
                    } else {
                        conflicts.push(e.path.clone());
                    }
                }
            }
            "charset_file" => {
                if let Some(dir) = targets.user_charsets_dir {
                    let rel = crate::bundle::validate_entry_rel(&e.path, "charsets/")?;
                    if write_file(&dir.join(rel), &bytes, strategy)? {
                        restored.push(e.path.clone());
                    } else {
                        conflicts.push(e.path.clone());
                    }
                }
            }
            "theme_file" => {
                if let Some(dir) = targets.user_themes_dir {
                    let rel = crate::bundle::validate_entry_rel(&e.path, "themes/")?;
                    if write_file(&dir.join(rel), &bytes, strategy)? {
                        restored.push(e.path.clone());
                    } else {
                        conflicts.push(e.path.clone());
                    }
                }
            }
            _ => {} // 未知/空 schema 条目：静默忽略（向前兼容）
        }
    }
    Ok(RestoreResult {
        restored,
        conflicts,
        schemas_touched: schemas_touched.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
        // 字符类：用户在设置页调过的那些，与 compat.toml 同属用户层配置。
        let charsets = t.path().join("charsets");
        fs::create_dir_all(&charsets).unwrap();
        fs::write(
            charsets.join("emoji.yaml"),
            "---
key: emoji
default: rare
",
        )
        .unwrap();
        let state = t.path().join("state.toml");
        fs::write(&state, "[toolbar]\n").unwrap();

        let out = t.path().join("backup.zip");
        let src = BackupSources {
            user_config_file: Some(&cfg),
            compat_file: Some(&compat),
            user_schemas_dir: Some(&schemas),
            user_schema_overrides_dir: Some(&overrides),
            user_themes_dir: Some(&themes),
            user_charsets_dir: Some(&charsets),
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
            user_charsets_dir: None,
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
            user_charsets_dir: None,
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
            user_charsets_dir: None,
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
            user_charsets_dir: None,
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
            user_charsets_dir: None,
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
            user_charsets_dir: None,
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
            user_charsets_dir: None,
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

    /// ★ 用户层的字符类定义要进备份包，还原时落回去。
    ///
    /// ⚠️ 漏掉它的表现是**静默的**：备份成功、还原成功，只是换了机器之后「我调过的
    /// 那些字符类」没了——而用户想不到去怀疑备份。同类的 compat.toml /
    /// schema_overrides/ 早就在包里，charsets/ 是后加的一档，最容易漏。
    #[test]
    fn charsets_are_backed_up_and_restored() {
        let t = tempfile::tempdir().unwrap();
        let src_dir = t.path().join("src");
        let charsets = src_dir.join("charsets");
        std::fs::create_dir_all(&charsets).unwrap();
        std::fs::write(
            charsets.join("emoji.yaml"),
            "---
key: emoji
default: rare
",
        )
        .unwrap();

        let out = t.path().join("b.zip");
        let store = Store::open(t.path().join("s.redb")).unwrap();
        let src = BackupSources {
            user_config_file: None,
            compat_file: None,
            user_schemas_dir: None,
            user_schema_overrides_dir: None,
            user_themes_dir: None,
            user_charsets_dir: Some(&charsets),
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
            r.entries.iter().any(|e| e.contains("charsets/emoji.yaml")),
            "字符类定义没进备份包：{:?}",
            r.entries
        );

        let dst = t.path().join("dst").join("charsets");
        let store2 = Store::open(t.path().join("s2.redb")).unwrap();
        let targets = RestoreTargets {
            user_config_file: None,
            compat_file: None,
            user_schemas_dir: None,
            user_schema_overrides_dir: None,
            user_themes_dir: None,
            user_charsets_dir: Some(&dst),
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
        let back = std::fs::read_to_string(dst.join("emoji.yaml")).expect("还原后该有这份文件");
        assert!(back.contains("key: emoji"), "内容对不上：{back}");
    }
}
