use super::*;

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_mf_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn num(b: &[u8]) -> anyhow::Result<u32> {
    Ok(std::str::from_utf8(b)?.trim().parse()?)
}

#[test]
fn good_file_loads_and_missing_is_none() {
    let d = dir("good");
    let f = d.join("a");
    assert_eq!(load_machine_file(&f, &num).unwrap(), None);
    std::fs::write(&f, "7").unwrap();
    assert_eq!(load_machine_file(&f, &num).unwrap(), Some(7));
}

#[test]
fn corrupt_file_heals_from_bak_and_is_kept_aside() {
    let d = dir("heal");
    let f = d.join("a");
    std::fs::write(&f, "x").unwrap();
    std::fs::write(d.join("a.bak"), "5").unwrap();
    assert_eq!(load_machine_file(&f, &num).unwrap(), Some(5));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "5");
    let kept: Vec<_> = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("a.corrupt-"))
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read_to_string(kept[0].path()).unwrap(), "x");
    assert!(matches!(
        issues::list_under(&f)[0].kind,
        IssueKind::Rebuilt { .. }
    ));
    issues::clear(&f);
}

/// 数据目录下：先用 `.backup/` 里的上一版，没有再用旧版旁边的 `.bak`；坏文件留存进 `.backup/`。
#[test]
fn heals_from_backup_dir_before_legacy_bak() {
    let d = dir("backup_dir");
    fold::register_data_root(&d);
    let f = d.join("stats").join("a");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "x").unwrap();
    std::fs::write(d.join("stats/a.bak"), "9").unwrap();
    std::fs::create_dir_all(d.join(".backup/stats")).unwrap();
    std::fs::write(d.join(".backup/stats/a"), "5").unwrap();
    assert_eq!(load_machine_file(&f, &num).unwrap(), Some(5));
    let IssueKind::Rebuilt { kept_as } = issues::list_under(&f)[0].kind.clone() else {
        panic!("要登记 Rebuilt");
    };
    assert!(kept_as.starts_with(".backup/stats/a.corrupt-"), "{kept_as}");
    std::fs::remove_file(d.join(".backup/stats/a")).unwrap();
    std::fs::write(&f, "y").unwrap();
    assert_eq!(
        load_machine_file(&f, &num).unwrap(),
        Some(9),
        "回退到旧 .bak"
    );
    issues::clear(&f);
}

#[test]
fn corrupt_file_without_bak_starts_empty() {
    let d = dir("empty");
    let f = d.join("a");
    std::fs::write(&f, "x").unwrap();
    std::fs::write(d.join("a.bak"), "y").unwrap();
    assert_eq!(load_machine_file(&f, &num).unwrap(), None);
    assert!(!f.exists());
    issues::clear(&f);
}
