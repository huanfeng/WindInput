use super::*;

fn extension(root: &Path, id: &str, manifest: &str, files: &[&str]) -> PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(MANIFEST_FILE), manifest).unwrap();
    for f in files {
        std::fs::write(dir.join(f), "").unwrap();
    }
    dir
}

#[test]
fn loads_minimal_manifest() {
    let t = tempfile::tempdir().unwrap();
    let dir = extension(
        t.path(),
        "keysound",
        "exe = \"run\"\nwant = [\"key.detail\"]\n",
        &["run"],
    );
    let m = load(&dir).unwrap();
    assert_eq!(m.id, "keysound");
    assert_eq!(m.name, "keysound");
    assert_eq!(m.want, vec!["key.detail"]);
    assert!(m.exe.ends_with("run"));
}

#[test]
fn per_os_exe_picks_current_platform() {
    let t = tempfile::tempdir().unwrap();
    let os = std::env::consts::OS;
    let dir = extension(
        t.path(),
        "p",
        &format!("exe = {{ {os} = \"mine\", other = \"theirs\" }}\n"),
        &["mine"],
    );
    assert!(load(&dir).unwrap().exe.ends_with("mine"));
}

#[test]
fn exe_outside_dir_is_rejected() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("evil"), "").unwrap();
    for exe in ["../evil", "/bin/sh", ""] {
        let dir = extension(t.path(), "p", &format!("exe = {exe:?}\n"), &[]);
        assert!(load(&dir).is_err(), "{exe} 应被拒绝");
    }
}

#[cfg(unix)]
#[test]
fn symlink_escaping_dir_is_rejected() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("evil"), "").unwrap();
    let dir = extension(t.path(), "p", "exe = \"link\"\n", &[]);
    std::os::unix::fs::symlink(t.path().join("evil"), dir.join("link")).unwrap();
    assert!(load(&dir).is_err());
}

#[test]
fn bad_id_and_missing_exe_are_errors() {
    let t = tempfile::tempdir().unwrap();
    let dir = extension(t.path(), "Bad Id", "exe = \"x\"\n", &["x"]);
    assert!(load(&dir).is_err());
    let dir = extension(t.path(), "ok", "exe = \"missing\"\n", &[]);
    assert!(load(&dir).is_err());
}

#[test]
fn scan_lists_dirs_sorted_and_tolerates_missing_root() {
    let t = tempfile::tempdir().unwrap();
    assert!(scan(&t.path().join("none")).is_empty());
    extension(t.path(), "b", "exe = \"x\"\n", &["x"]);
    extension(t.path(), "a", "not toml [", &[]);
    let got = scan(t.path());
    assert_eq!(
        got.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(got[0].1.is_err() && got[1].1.is_ok());
}
