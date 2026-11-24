use super::*;

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_meta_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 旁边已有旧版 `userdata.redb`：初次生成的 meta 记 1，等迁移完成再升（否则迁移还没跑就先
/// 写成最新版本号，迁移失败后回退的旧版本被误判成只读）。
#[test]
fn fresh_meta_beside_legacy_redb_starts_at_1() {
    let d = dir("legacy_redb");
    std::fs::write(d.join("userdata.redb"), b"x").unwrap();
    match load_or_init(&d) {
        MetaCheck::Ok(m) => assert_eq!(m.format_version, 1),
        other => panic!("{other:?}"),
    }
}

/// 旧库已改名 `.legacy` 后 meta.toml 被删：没有 redb 可迁，记最新（记 1 会新建空 redb）。
#[test]
fn retired_legacy_redb_alone_is_not_a_migration_source() {
    let d = dir("legacy_retired");
    std::fs::write(d.join("userdata.redb.legacy"), b"x").unwrap();
    match load_or_init(&d) {
        MetaCheck::Ok(m) => assert_eq!(m.format_version, KNOWN_FORMAT_VERSION),
        other => panic!("{other:?}"),
    }
}

#[test]
fn missing_meta_is_initialised_with_known_version_and_random_dir_id() {
    let d = dir("init");
    let MetaCheck::Ok(m) = load_or_init(&d) else {
        panic!("应初始化")
    };
    assert_eq!(m.format_version, KNOWN_FORMAT_VERSION);
    assert_eq!(m.dir_id.len(), 32);
    let MetaCheck::Ok(again) = load_or_init(&d) else {
        panic!()
    };
    assert_eq!(again.dir_id, m.dir_id, "第二次读到同一个 dir_id");
}

/// 第一行是说明行，读照常；升版本整写时说明行还在。
#[test]
fn meta_file_starts_with_banner() {
    let d = dir("banner");
    assert!(matches!(load_or_init(&d), MetaCheck::Ok(_)));
    let first = |d: &Path| {
        std::fs::read_to_string(d.join("meta.toml"))
            .unwrap()
            .lines()
            .next()
            .map(str::to_string)
    };
    assert_eq!(first(&d).as_deref(), Some(META_BANNER));
    assert!(read(&d).is_some());
    std::fs::write(
        d.join("meta.toml"),
        "format_version = 1\ndir_id = \"abc\"\n",
    )
    .unwrap();
    raise_format_version(&d, 2).unwrap();
    assert_eq!(first(&d).as_deref(), Some(META_BANNER));
    assert!(
        META_BANNER.starts_with("# 清风输入法 · ")
            && META_BANNER.ends_with("。由程序自动维护，请勿手动修改。")
    );
}

#[test]
fn newer_version_is_incompatible_and_file_is_left_untouched() {
    let d = dir("newer");
    let text = "format_version = 99\ndir_id = \"abc\"\n";
    std::fs::write(d.join("meta.toml"), text).unwrap();
    assert!(matches!(
        load_or_init(&d),
        MetaCheck::Incompatible {
            found: 99,
            known: KNOWN_FORMAT_VERSION
        }
    ));
    assert_eq!(
        std::fs::read_to_string(d.join("meta.toml")).unwrap(),
        text,
        "只读：一字不改"
    );
}

#[test]
fn unparsable_meta_is_reported_not_overwritten() {
    let d = dir("bad");
    std::fs::write(d.join("meta.toml"), "not toml ==").unwrap();
    assert!(matches!(load_or_init(&d), MetaCheck::Unreadable(_)));
    assert_eq!(
        std::fs::read_to_string(d.join("meta.toml")).unwrap(),
        "not toml =="
    );
}

#[test]
fn raise_format_version_only_moves_up_and_keeps_dir_id() {
    let d = dir("raise");
    std::fs::write(
        d.join("meta.toml"),
        "format_version = 1\ndir_id = \"abc\"\n",
    )
    .unwrap();
    raise_format_version(&d, 2).unwrap();
    let MetaCheck::Ok(m) = load_or_init(&d) else {
        panic!("应可读")
    };
    assert_eq!((m.format_version, m.dir_id.as_str()), (2, "abc"));
    raise_format_version(&d, 1).unwrap();
    let MetaCheck::Ok(m) = load_or_init(&d) else {
        panic!("应可读")
    };
    assert_eq!(m.format_version, 2, "不往回改");
}
