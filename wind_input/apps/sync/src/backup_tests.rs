use super::*;

fn put(root: &Path, rel: &str, c: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

#[test]
fn keeps_latest_three_and_copies_settings_only() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    put(root, "config.toml", "a = 1\n");
    put(root, "data/x/dicts.toml", "d = 1\n");
    put(root, "data/x/user.dict.yaml", "词\n");
    let paths: Vec<String> = ["config.toml", "data/x/dicts.toml", "data/x/user.dict.yaml"]
        .map(String::from)
        .to_vec();
    let t0 = 1_795_564_800; // 2026-11-25 00:00:00 UTC
    let mut last = PathBuf::new();
    for i in 0..4 {
        last = backup_settings(root, &paths, t0 + i * 61).unwrap();
    }
    let base = root.join("data/.backup/sync");
    let mut dirs: Vec<String> = std::fs::read_dir(&base)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    assert_eq!(
        dirs,
        ["20261125-000101", "20261125-000202", "20261125-000303"]
    );
    assert_eq!(last, base.join("20261125-000303"));
    assert_eq!(
        std::fs::read_to_string(last.join("config.toml")).unwrap(),
        "a = 1\n"
    );
    assert!(last.join("data/x/dicts.toml").is_file());
    assert!(!last.join("data/x/user.dict.yaml").exists(), "数据不进备份");
}

#[test]
fn same_second_backups_get_numbered_dirs() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    let paths = vec!["config.toml".to_string()];
    let t0 = 1_795_564_800;
    let mut got = Vec::new();
    for i in 0..4 {
        put(root, "config.toml", &format!("v = {i}\n"));
        got.push(backup_settings(root, &paths, t0).unwrap());
    }
    let base = root.join("data/.backup/sync");
    assert_eq!(got[1], base.join("20261125-000000-2"));
    let mut dirs: Vec<String> = std::fs::read_dir(&base)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    assert_eq!(
        dirs,
        [
            "20261125-000000-2",
            "20261125-000000-3",
            "20261125-000000-4"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(got[3].join("config.toml")).unwrap(),
        "v = 3\n"
    );
}
