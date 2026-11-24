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
        // 联想历史（t185 方案 B）归「freq」段（见 `section_of`）：同是「用出来的」使用数据，
        // 设置端的备份段落不必多一项。
        let assoc = store.export_assoc_history_jsonl(sc)?;
        if !assoc.is_empty() {
            add(
                &mut w,
                format!("userdata/assoc_history/{sc}.jsonl"),
                assoc.as_bytes(),
                "assoc_history",
                meta.clone(),
            )?;
        }
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

    // 快捷格式调整（调序 / 停用 / 自定义条目）：全局段。包内是 yaml 序列化，与 data/ 下的 toml 真源不同。
    let quick = store.export_quick_format_yaml()?;
    if !quick.is_empty() {
        add(
            &mut w,
            "userdata/quick_format.yaml".into(),
            quick.as_bytes(),
            "quick_format",
            serde_json::Value::Null,
        )?;
    }

    // 补全学习数据（邮箱后缀词频 + 网址历史）：同样是键不带方案的全局段。
    // 一个文件装两类（jsonl 每行自带 kind），而不是两个文件——备份包里多一个文件就多
    // 一处要在 create / restore / RESTORE_SECTIONS 三处同步登记的地方，而 common_chars
    // 漏登记 RESTORE_SECTIONS 的前科说明那是条真会被漏的路。
    let completions = store.export_completions_jsonl()?;
    add(
        &mut w,
        "userdata/completions.jsonl".into(),
        completions.as_bytes(),
        "completion",
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
    // 字符类的用户层：与 common_chars 一样是全局段（键不带方案）。value 就是 yaml 文本，
    // 原样进包——用户解开包就能读，也能直接拿去设置页「从文件加载」。
    //
    // ⚠️ 进包的是**稀疏 diff**（只含用户改过的），不是完整视图：还原到升过版的出厂表上，
    // 完整视图会把出厂新加的字全记成「用户移除」。
    for (key, text) in store.list_charset_docs()? {
        add(
            &mut w,
            format!("charsets/{key}.yaml"),
            text.as_bytes(),
            "charset",
            serde_json::Value::Null,
        )?;
    }

    // 用户分库与开关文件：原样进包（`data/<数据方案>/…`），还原时写回同一位置。
    let text_dir = store.user_text_dir();
    let mut data_schemas: Vec<String> = std::fs::read_dir(text_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.')) // `.backup/` 不进包
        .collect();
    data_schemas.sort();
    for ds in &data_schemas {
        let meta = serde_json::json!({ "schema": ds });
        let switches = wind_store::user_dicts::meta_path(text_dir, ds);
        if switches.is_file() {
            add(
                &mut w,
                format!("data/{ds}/{}", wind_store::user_dicts::META_FILE),
                &std::fs::read(&switches)?,
                "user_dicts_meta",
                meta.clone(),
            )?;
        }
        for l in wind_store::user_dicts::scan(text_dir, ds) {
            add(
                &mut w,
                format!(
                    "data/{ds}/{}/{}.dict.yaml",
                    wind_store::user_dicts::LIBS_DIR,
                    l.name
                ),
                &std::fs::read(&l.path)?,
                "user_dict_file",
                meta.clone(),
            )?;
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
        "assoc_history" => "freq",
        "user_dict_file" | "user_dicts_meta" => "dict",
        other => other,
    }
}

/// 分库条目只能落在 `<数据方案>/dicts/<名>.dict.yaml` 或 `<数据方案>/dicts.toml`（数据方案段须是合法方案 id）。
fn is_user_dict_rel(rel: &str, ty: &str) -> bool {
    use wind_store::user_dicts::{LIBS_DIR, META_FILE, validate_name};
    let ds_ok = |ds: &str| wind_store::check_schema_id(ds).is_ok();
    match (ty, rel.split('/').collect::<Vec<_>>().as_slice()) {
        ("user_dicts_meta", [ds, f]) => ds_ok(ds) && *f == META_FILE,
        ("user_dict_file", [ds, d, f]) => {
            ds_ok(ds)
                && *d == LIBS_DIR
                && f.strip_suffix(".dict.yaml")
                    .is_some_and(|n| validate_name(n).is_ok())
        }
        _ => false,
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
    // 还原的 schema_file 来自 `walk_dir(user_schemas_dir)`,同样囊括 sidecar `.wdat`
    // ——那种词库直接走 `wind_dict::reader_pool` 打开,运行中的引擎可能还攥着旧映射。
    // 与 `scheme::write_staged` 同一个理由,守卫圈住「先删后 rename」整段。
    let replacing = wind_dict::reader_pool::is_pooled(target)
        .then(|| wind_dict::reader_pool::replacing(target));
    let renamed = (|| {
        if target.exists() {
            std::fs::remove_file(target)?;
        }
        std::fs::rename(&tmp, target)
    })();
    drop(replacing);
    renamed?;
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
        // 正在外部编辑的文件不整写（编辑器还开着，写了会被它的保存盖掉）：记为冲突跳过
        let editing = match e.r#type.as_str() {
            "dict" if !schema.is_empty() => store.user_words_held(schema),
            "shadow" if !schema.is_empty() => store.is_editing(&store.shadow_path(schema)),
            "phrase" => store.is_editing(&store.phrases_path()),
            "common_chars" => store.is_editing(&store.common_chars_path()),
            "quick_format" => store.is_editing(&store.quick_format_path()),
            // 替换要先清空全部字符类：有一个在编辑就整段跳过，不清到一半
            "charset" => store
                .list_charset_docs()?
                .iter()
                .filter(|(k, _)| {
                    replace
                        || wind_config::charset_def::parse_doc(&text())
                            .is_ok_and(|d| d.def.key == *k)
                })
                .any(|(k, _)| store.charset_path(k).is_ok_and(|p| store.is_editing(&p))),
            "user_dict_file" | "user_dicts_meta" => {
                crate::bundle::validate_entry_rel(&e.path, "data/")
                    .is_ok_and(|rel| store.is_editing(&store.user_text_dir().join(rel)))
            }
            _ => false,
        };
        if editing {
            conflicts.push(e.path.clone());
            continue;
        }
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
                // 联想历史归「freq」段：替换词频时一并清掉，**备份里没有它的条目也清**
                // （旧备份不带 assoc_history）——否则还原后留着本机的旧历史，与 FREQ 不同源。
                // 与下面 `assoc_history` 臂共用去重键：谁先到谁清，后到的只导入不再清。
                if replace && cleared.insert(format!("assoc_history:{schema}")) {
                    store.clear_assoc_history(schema)?;
                }
                store.import_freq_jsonl(schema, &text())?;
                schemas_touched.insert(schema.to_string());
                restored.push(e.path.clone());
            }
            "assoc_history" if !schema.is_empty() => {
                if replace && cleared.insert(format!("assoc_history:{schema}")) {
                    store.clear_assoc_history(schema)?;
                }
                store.import_assoc_history_jsonl(schema, &text())?;
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
            "quick_format" => {
                store.import_quick_format_yaml(&text(), replace)?;
                restored.push(e.path.clone());
            }
            "completion" => {
                if replace && cleared.insert("completion".into()) {
                    // 所有分区都清：备份包里的这一个文件装的就是全部类别，只清一部分会让
                    // 其余变成「本地残留 ∪ 包里内容」，那不是 Replace 的语义。
                    for kind in wind_store::completion::CompletionKind::ALL {
                        store.clear_completions(kind)?;
                    }
                }
                store.import_completions_jsonl(&text())?;
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
            "charset" => {
                if replace && cleared.insert("charset".into()) {
                    store.clear_charset_docs()?;
                }
                // 键取文本里的 `key`，不取文件名：文件名只是投影，文本才是真相。
                // 解析失败不让整次还原失败（与 common_chars 坏行同一条纪律），但也不
                // 静默——记进 conflicts，用户在结果里看得到「哪条没还原」。本 crate
                // 零日志依赖（见 scheme.rs），warn 不是选项。
                let t = text();
                match wind_config::charset_def::parse_doc(&t) {
                    Ok(doc) => {
                        store.set_charset_doc(&doc.def.key, &t)?;
                        restored.push(e.path.clone());
                    }
                    Err(_) => conflicts.push(e.path.clone()),
                }
            }
            "user_dict_file" | "user_dicts_meta" => {
                let rel = crate::bundle::validate_entry_rel(&e.path, "data/")?;
                if !is_user_dict_rel(&rel, &e.r#type) {
                    conflicts.push(e.path.clone());
                } else if write_file(&store.user_text_dir().join(&rel), &bytes, strategy)? {
                    restored.push(e.path.clone());
                } else {
                    conflicts.push(e.path.clone());
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
#[path = "backup_tests.rs"]
mod tests;
