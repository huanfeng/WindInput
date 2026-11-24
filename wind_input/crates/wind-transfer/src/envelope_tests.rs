use super::*;

const HAPPY: &str = r#"
[package]
format_version = 2
kind = "schema_text"

[schema]
id = "kf"
version = "1.00.0"

[[files]]
path = "kf.schema.toml"
content = """
[schema]
id = "kf"
"""

[[files]]
path = "flypy/12_kf.dict.yaml"
content = "a\t啊\n"
"#;

fn dest() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("schemas");
    std::fs::create_dir_all(&d).unwrap();
    (t, d)
}

#[test]
fn happy_path_previews_and_lands_two_files() {
    let (_t, d) = dest();
    let prev = preview_import_text(HAPPY, &d).unwrap();
    assert_eq!(prev.meta.package.format_version, 2);
    assert_eq!(prev.meta.schema.id, "kf");
    assert_eq!(prev.meta.schema.version, "1.00.0");
    assert_eq!(
        prev.will_add,
        vec!["kf.schema.toml", "flypy/12_kf.dict.yaml"]
    );
    assert!(prev.conflicts.is_empty());
    assert!(prev.config_patch.is_none());

    let r = import_text(HAPPY, &d, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.imported.len(), 2);
    assert_eq!(r.schema_ids, vec!["kf"], "根方案 id 从文件名主干取");
    assert_eq!(
        std::fs::read_to_string(d.join("kf.schema.toml")).unwrap(),
        "[schema]\nid = \"kf\"\n",
        "内容逐字落盘"
    );
    assert!(d.join("flypy/12_kf.dict.yaml").is_file(), "子目录自动建出");

    // 二次导入(Merge):全部计冲突,不覆盖——语义与 zip 版一致。
    let r2 = import_text(HAPPY, &d, crate::merge::Strategy::Merge).unwrap();
    assert!(r2.imported.is_empty());
    assert_eq!(r2.conflicts.len(), 2);
}

/// 缺 format_version → 硬错误(信封无 legacy),且**不带** not_schema_text 前缀:
/// 它确实是信封,只是写坏了,回落片段管线只会给出更莫名其妙的错误。
#[test]
fn missing_format_version_is_hard_error() {
    let (_t, d) = dest();
    let text =
        "[package]\nkind = \"schema_text\"\n[[files]]\npath = \"a.schema.toml\"\ncontent = \"x\"\n";
    let err = preview_import_text(text, &d).unwrap_err().to_string();
    assert!(err.contains("format_version"), "{err}");
    assert!(
        !err.starts_with(NOT_SCHEMA_TEXT),
        "已声明 kind 就不该回落片段管线: {err}"
    );
}

/// 高于当前支持的版本 → 提示升级(宽容只给过去,不给未来)。
#[test]
fn future_format_version_is_rejected() {
    let (_t, d) = dest();
    let text = "[package]\nformat_version = 3\nkind = \"schema_text\"\n[[files]]\npath = \"a.schema.toml\"\ncontent = \"x\"\n";
    let err = preview_import_text(text, &d).unwrap_err().to_string();
    assert!(err.contains("升级"), "{err}");
}

/// kind 不对 / 缺失 / 根本不是 TOML → 一律带 not_schema_text: 前缀,
/// 设置端据此回落配置片段管线。裸方案文本(有 [schema] 段)绝不能被猜成信封。
#[test]
fn non_envelope_text_is_flagged_for_fragment_fallback() {
    let (_t, d) = dest();
    for text in [
        "[package]\nkind = \"backup\"\n",
        "[package]\nformat_version = 2\n",
        "[schema]\nid = \"kf\"\n",
        "ui.candidate.per_page = 9\n",
        "= not toml =",
    ] {
        let err = preview_import_text(text, &d).unwrap_err().to_string();
        assert!(
            err.starts_with(NOT_SCHEMA_TEXT),
            "非信封文本须带回落前缀({text:?}): {err}"
        );
    }
}

/// 穿越路径逐条过 validate_entry_rel(与 zip 导入同一守卫),且拒绝后零落盘。
#[test]
fn traversal_paths_are_rejected_without_writing() {
    let (_t, d) = dest();
    for bad in [
        "../evil.toml",
        "C:evil.toml",
        "/etc/passwd",
        "a/../../b.toml",
    ] {
        let text = format!(
            "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
                 [[files]]\npath = \"ok.schema.toml\"\ncontent = \"x\"\n\
                 [[files]]\npath = \"{bad}\"\ncontent = \"x\"\n"
        );
        assert!(
            preview_import_text(&text, &d).is_err(),
            "穿越路径应拒绝: {bad}"
        );
        assert!(import_text(&text, &d, crate::merge::Strategy::Merge).is_err());
    }
    assert!(!d.join("ok.schema.toml").exists(), "拒绝的信封不得落盘");
}

#[test]
fn duplicate_paths_are_rejected() {
    let (_t, d) = dest();
    let text = "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
                    [[files]]\npath = \"a.schema.toml\"\ncontent = \"1\"\n\
                    [[files]]\npath = \"a.schema.toml\"\ncontent = \"2\"\n";
    let err = preview_import_text(text, &d).unwrap_err().to_string();
    assert!(err.contains("重复路径"), "{err}");
}

/// 根下无 *.schema.toml → 不是有效信封(与 zip 侦测规则同构)。
/// 注意 config_patch 不算方案文件:纯配置包不走信封这条路。
#[test]
fn envelope_without_root_schema_is_rejected() {
    let (_t, d) = dest();
    let nested = "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
                      [[files]]\npath = \"sub/a.schema.toml\"\ncontent = \"x\"\n";
    assert!(
        preview_import_text(nested, &d).is_err(),
        "子目录方案文件不算根"
    );
    let only_patch = format!(
        "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
             [[files]]\npath = \"{CONFIG_PATCH_NAME}\"\ncontent = \"ui.candidate.per_page = 9\"\n"
    );
    assert!(preview_import_text(&only_patch, &d).is_err());
    let empty = "[package]\nformat_version = 2\nkind = \"schema_text\"\n";
    assert!(preview_import_text(empty, &d).is_err(), "无 files 也应拒绝");
}

/// 反斜杠路径归一化后不算根条目 → 该信封缺根方案文件被拒。
/// 拿未归一化的原始切片判根会让 `sub\x.schema.toml` 冒充根方案文件。
#[test]
fn backslash_path_does_not_count_as_root_schema() {
    let (_t, d) = dest();
    let text = "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
                    [[files]]\npath = \"sub\\\\x.schema.toml\"\ncontent = \"x\"\n";
    let err = preview_import_text(text, &d).unwrap_err().to_string();
    assert!(err.contains("缺少 *.schema.toml"), "{err}");

    // 对照:同一个信封补一个真正的根方案文件 → 通过,且反斜杠条目落盘用 `/` 形态。
    let ok = "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
                  [[files]]\npath = \"r.schema.toml\"\ncontent = \"x\"\n\
                  [[files]]\npath = \"sub\\\\x.dict.yaml\"\ncontent = \"y\"\n";
    let prev = preview_import_text(ok, &d).unwrap();
    assert_eq!(prev.will_add, vec!["r.schema.toml", "sub/x.dict.yaml"]);
    import_text(ok, &d, crate::merge::Strategy::Merge).unwrap();
    assert!(d.join("sub").join("x.dict.yaml").is_file());
}

/// 超 2 MB → 硬错误(不带回落前缀),且在**解析之前**就拒掉。
/// 片段是手写配置、KB 量级,超限文本几乎必是超大信封;回落片段管线只会刷一屏
/// 「未知配置键」噪音。
#[test]
fn oversized_text_is_rejected_before_parsing() {
    let (_t, d) = dest();
    // 一形:合法信封但内容超限
    let mut big = String::from(
        "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
             [[files]]\npath = \"b.schema.toml\"\ncontent = \"",
    );
    big.push_str(&"x".repeat(MAX_TEXT_BYTES + 1));
    big.push_str("\"\n");
    let err = preview_import_text(&big, &d).unwrap_err().to_string();
    assert!(err.contains("过大"), "{err}");
    assert!(
        !err.starts_with(NOT_SCHEMA_TEXT),
        "超限是硬错误,不回落: {err}"
    );

    // 二形:纯垃圾(连 TOML 都不是)——同样按超限拒绝,证明限额先于解析。
    let garbage = "\u{4e00}".repeat(MAX_TEXT_BYTES); // 每字 3 字节,必定超限
    let err2 = preview_import_text(&garbage, &d).unwrap_err().to_string();
    assert!(err2.contains("过大"), "限额须先于 TOML 解析: {err2}");
    assert!(!err2.starts_with(NOT_SCHEMA_TEXT), "{err2}");

    // 零落盘
    assert!(import_text(&big, &d, crate::merge::Strategy::Merge).is_err());
    assert!(!d.join("b.schema.toml").exists());
}

#[test]
fn too_many_files_is_rejected() {
    let (_t, d) = dest();
    let mut text = String::from("[package]\nformat_version = 2\nkind = \"schema_text\"\n");
    text.push_str("[[files]]\npath = \"a.schema.toml\"\ncontent = \"x\"\n");
    for i in 0..MAX_FILES {
        text.push_str(&format!(
            "[[files]]\npath = \"d/{i}.txt\"\ncontent = \"x\"\n"
        ));
    }
    let err = preview_import_text(&text, &d).unwrap_err().to_string();
    assert!(err.contains("文件过多"), "{err}");
}

/// 信封的说明元信息:与 zip 包同名同义、同一份校验。缺省即空串(不报错)。
#[test]
fn envelope_carries_package_info() {
    let (_t, d) = dest();
    let text = "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
                    title = \"快符方案\"\n\
                    description = \"\"\"\n分号引导的符号表。\n含常用标点。\n\"\"\"\n\
                    [[files]]\npath = \"kf.schema.toml\"\ncontent = \"x\"\n";
    let prev = preview_import_text(text, &d).unwrap();
    assert_eq!(prev.meta.package.title, "快符方案");
    assert_eq!(
        prev.meta.package.description,
        "分号引导的符号表。\n含常用标点。"
    );

    // 缺省 → 空串,不报错(HAPPY 没写这两个字段)。
    let prev2 = preview_import_text(HAPPY, &d).unwrap();
    assert!(prev2.meta.package.title.is_empty());
    assert!(prev2.meta.package.description.is_empty());
}

/// 说明非法 → 硬错误(与 format_version 门禁同层),且**不带**回落前缀:
/// 它确实是信封,只是说明写坏了。零落盘。
#[test]
fn invalid_envelope_info_is_rejected() {
    let (_t, d) = dest();
    let long_title = "字".repeat(wind_config::patch::INFO_TITLE_MAX_CHARS + 1);
    for info in [
        "title = \"第一行\\n第二行\"".to_string(),
        format!("title = \"{long_title}\""),
        "description = \"说明\\u0007\"".to_string(),
    ] {
        let text = format!(
            "[package]\nformat_version = 2\nkind = \"schema_text\"\n{info}\n\
                 [[files]]\npath = \"bad.schema.toml\"\ncontent = \"x\"\n"
        );
        let err = preview_import_text(&text, &d).unwrap_err().to_string();
        assert!(err.contains("package."), "错误须点名字段: {err}");
        assert!(
            !err.starts_with(NOT_SCHEMA_TEXT),
            "已声明 kind 就不回落: {err}"
        );
        assert!(import_text(&text, &d, crate::merge::Strategy::Merge).is_err());
    }
    assert!(!d.join("bad.schema.toml").exists(), "拒绝的信封不得落盘");
}

/// 版本门禁**先于**说明校验(与 zip 路径同判据同顺序):两处都不合法时报版本过高。
#[test]
fn future_format_version_reported_before_invalid_info() {
    let (_t, d) = dest();
    let long_title = "字".repeat(wind_config::patch::INFO_TITLE_MAX_CHARS + 1);
    let text = format!(
        "[package]\nformat_version = 3\nkind = \"schema_text\"\ntitle = \"{long_title}\"\n\
             [[files]]\npath = \"v3.schema.toml\"\ncontent = \"x\"\n"
    );
    let err = preview_import_text(&text, &d).unwrap_err().to_string();
    assert!(err.contains("升级"), "应先报版本过高: {err}");
    assert!(
        !err.contains("过长"),
        "不该拿当前规则去判更高版本的说明: {err}"
    );
    assert!(import_text(&text, &d, crate::merge::Strategy::Merge).is_err());
}

/// config_patch.toml 作 files 条目:不落盘,原文随预览返回(语义同 zip 包)。
#[test]
fn config_patch_entry_is_returned_not_written() {
    let (_t, d) = dest();
    let text = format!(
        "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
             [[files]]\npath = \"kf.schema.toml\"\ncontent = \"[schema]\\nid = 'kf'\\n\"\n\
             [[files]]\npath = \"{CONFIG_PATCH_NAME}\"\ncontent = \"ui.candidate.per_page = 9\\n\"\n"
    );
    let prev = preview_import_text(&text, &d).unwrap();
    assert_eq!(prev.will_add, vec!["kf.schema.toml"], "片段不进文件清单");
    assert_eq!(
        prev.config_patch.as_deref(),
        Some("ui.candidate.per_page = 9\n")
    );
    let r = import_text(&text, &d, crate::merge::Strategy::Merge).unwrap();
    assert_eq!(r.imported, vec!["kf.schema.toml"]);
    assert!(
        !d.join(CONFIG_PATCH_NAME).exists(),
        "配置片段不是方案资源,不得落盘"
    );
}
