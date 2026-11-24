use super::*;
use tempfile;

#[test]
fn manifest_roundtrip_and_validate() {
    let m = Manifest::new(
        BundleKind::Backup,
        "1.2.3",
        "windows",
        "2026-07-11T00:00:00+08:00",
    );
    assert_eq!(m.format, FORMAT_TAG);
    assert_eq!(m.spec_version, SPEC_VERSION);
    m.validate().unwrap();

    let json = serde_json::to_string(&m).unwrap();
    assert!(
        json.contains("\"kind\":\"backup\""),
        "kind 序列化为小写字符串"
    );
    let back: Manifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back.app_version, "1.2.3");
}

#[test]
fn validate_rejects_future_spec_and_bad_format() {
    let mut m = Manifest::new(BundleKind::Scheme, "1.0.0", "darwin", "t");
    m.spec_version = SPEC_VERSION + 1;
    assert!(m.validate().is_err(), "更高 spec_version 应拒绝");

    let mut m2 = Manifest::new(BundleKind::Scheme, "1.0.0", "darwin", "t");
    m2.format = "wrong".into();
    assert!(m2.validate().is_err(), "format 不匹配应拒绝");
}

#[test]
fn bundle_write_read_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let zip_path = dir.path().join("t.zip");

    let manifest = Manifest::new(BundleKind::Backup, "1.0.0", "windows", "t");
    let mut w = BundleWriter::new(&zip_path, manifest).unwrap();
    w.add_bytes("userdata/user_words.wdict", b"hello-words")
        .unwrap();
    w.finish().unwrap();

    // 免解压读 manifest
    let m = read_manifest(&zip_path).unwrap();
    assert_eq!(m.kind, BundleKind::Backup);
    // 取单个条目
    let data = extract_entry(&zip_path, "userdata/user_words.wdict").unwrap();
    assert_eq!(data, b"hello-words");
}

/// 返回值必须是**归一化后**的路径:调用方拿它判根(`contains('/')`)、取 id、落盘。
/// 若原样返回带 `\` 的切片,`sub\evil.schema.toml` 会被判成根方案文件。
#[test]
fn validate_entry_rel_returns_normalized_separators() {
    assert_eq!(
        validate_entry_rel("sub\\x.schema.toml", "").unwrap(),
        "sub/x.schema.toml"
    );
    assert_eq!(
        validate_entry_rel("schemas/a\\b\\c.yaml", "schemas/").unwrap(),
        "a/b/c.yaml"
    );
    // 归一化后不再是根条目
    assert!(
        validate_entry_rel("sub\\x.schema.toml", "")
            .unwrap()
            .contains('/'),
        "反斜杠路径不得被判成根条目"
    );
    // 无分隔符的条目原样返回
    assert_eq!(
        validate_entry_rel("x.schema.toml", "").unwrap(),
        "x.schema.toml"
    );
}

/// 穿越守卫本身不受归一化影响:反斜杠形态的 `..` 一样拦。
#[test]
fn validate_entry_rel_still_rejects_traversal_in_backslash_form() {
    assert!(validate_entry_rel("..\\evil.toml", "").is_err());
    assert!(validate_entry_rel("a\\..\\..\\evil.toml", "").is_err());
    assert!(validate_entry_rel("C:evil.toml", "").is_err());
}

#[test]
fn validate_entry_rel_rejects_names_windows_cannot_keep() {
    for bad in [
        "CON",
        "nul",
        "NUL.png",
        "assets/com1.txt",
        "lpt9",
        "assets/aux.tar.gz",
        "frame.png.",
        "frame.png ",
        "assets./x.png",
        "a//b",
        "CONIN$",
        "assets/conout$.txt",
        "assets/COM².png",
    ] {
        assert!(validate_entry_rel(bad, "").is_err(), "{bad} 该被拒");
    }
    for good in [
        "console.png",
        "COM0",
        "com10.png",
        "nul_.png",
        "a.b/c.png",
        ".hidden",
    ] {
        assert!(validate_entry_rel(good, "").is_ok(), "{good} 不该被拒");
    }
}

#[test]
fn read_manifest_rejects_bad_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.zip");
    // 手写一个不含 manifest.toml 的 zip
    let mut w = zip::ZipWriter::new(std::fs::File::create(&bad).unwrap());
    w.start_file("foo.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    use std::io::Write;
    w.write_all(b"x").unwrap();
    w.finish().unwrap();
    assert!(read_manifest(&bad).is_err(), "缺 manifest.toml 应报错");
}

#[test]
fn typed_entries_and_refs_in_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let zip_path = dir.path().join("typed.zip");
    let manifest = Manifest::new(BundleKind::Scheme, "1.0.0", "windows", "t");
    let mut w = BundleWriter::new(&zip_path, manifest).unwrap();
    w.add_bytes_with(
        "schemas/my.schema.toml",
        b"[schema]\nid=\"my\"\n",
        "schema",
        serde_json::json!({ "id": "my" }),
    )
    .unwrap();
    w.add_ref(
        "system_ref",
        "wubi86/wubi86_jidian.dict.yaml",
        serde_json::Value::Null,
    );
    w.finish().unwrap();

    let m = read_manifest(&zip_path).unwrap();
    assert_eq!(m.contents.len(), 2);
    let schema_entry = &m.contents[0];
    assert_eq!(schema_entry.r#type, "schema");
    assert_eq!(
        schema_entry.meta.get("id").and_then(|v| v.as_str()),
        Some("my")
    );
    let ref_entry = &m.contents[1];
    assert_eq!(ref_entry.r#type, "system_ref");
    // ref 条目无 zip 载荷
    assert!(extract_entry(&zip_path, "wubi86/wubi86_jidian.dict.yaml").is_err());
    // 带载荷条目可取
    assert!(extract_entry(&zip_path, "schemas/my.schema.toml").is_ok());
}
