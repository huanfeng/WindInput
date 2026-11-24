#[test]
fn preserve_corrupt_renames_with_stamp_and_never_collides() {
    let d = std::env::temp_dir().join(format!("wind_preserve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("a.toml");
    assert!(super::preserve_corrupt(&f).unwrap().is_none());
    std::fs::write(&f, "1").unwrap();
    let p1 = super::preserve_corrupt(&f).unwrap().unwrap();
    std::fs::write(&f, "2").unwrap();
    let p2 = super::preserve_corrupt(&f).unwrap().unwrap();
    assert_ne!(p1, p2);
    assert!(!f.exists());
    let n = p1.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        n.starts_with("a.toml.corrupt-20") && n.len() == "a.toml.corrupt-".len() + 15,
        "{n}"
    );
    assert_eq!(std::fs::read_to_string(&p2).unwrap(), "2");
    let _ = std::fs::remove_dir_all(&d);
}

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn root(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wind_fold_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    register_data_root(&d);
    d
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// 数据目录下：上一版放进 `.backup/<相对路径>`，数据文件旁边不再有 `.bak`。
#[test]
fn commit_keeps_previous_version_under_backup_dir() {
    let d = root("bak");
    let f = d.join("pinyin").join("user.dict.yaml");
    commit_text(&f, b"1\n", None).unwrap();
    commit_text(&f, b"2\n", None).unwrap();
    assert_eq!(names(&d.join("pinyin")), ["user.dict.yaml"]);
    let bak = d.join(BACKUP_DIR).join("pinyin").join("user.dict.yaml");
    assert_eq!(backup_path(&f), bak);
    assert_eq!(std::fs::read_to_string(&bak).unwrap(), "1\n");
    // 没登记的目录照旧同目录 `.bak`
    let other = std::env::temp_dir().join(format!("wind_fold_unreg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&other);
    let g = other.join("a.toml");
    commit_text(&g, b"1", None).unwrap();
    commit_text(&g, b"2", None).unwrap();
    assert_eq!(
        std::fs::read_to_string(other.join("a.toml.bak")).unwrap(),
        "1"
    );
    let _ = std::fs::remove_dir_all(&other);
    remove_backup(&f);
    assert!(!bak.exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// 上一版写不出来（这里让它的位置被一个目录占着）不拦数据文件的提交。
#[test]
fn commit_proceeds_when_backup_cannot_be_written() {
    let d = root("bak_fail");
    let f = d.join("pinyin").join("user.dict.yaml");
    commit_text(&f, b"1\n", None).unwrap();
    std::fs::create_dir_all(backup_path(&f)).unwrap();
    commit_text(&f, b"2\n", None).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "2\n");
    let _ = std::fs::remove_dir_all(&d);
}

/// 数据目录下的坏文件留存进 `.backup/`，每个原文件只留最新 5 份；给用户看的是相对路径。
#[test]
fn corrupt_copies_go_to_backup_dir_and_only_five_are_kept() {
    let d = root("corrupt");
    let f = d.join("stats").join("2026-10.toml");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    let mut last = PathBuf::new();
    for i in 0..7 {
        std::fs::write(&f, format!("{i}")).unwrap();
        last = preserve_corrupt(&f).unwrap().unwrap();
    }
    let dir = d.join(BACKUP_DIR).join("stats");
    let kept = names(&dir);
    assert_eq!(kept.len(), MAX_CORRUPT_KEPT, "{kept:?}");
    assert_eq!(std::fs::read_to_string(&last).unwrap(), "6");
    assert!(kept.iter().all(|n| n.starts_with("2026-10.toml.corrupt-")));
    assert_eq!(names(&d.join("stats")), Vec::<String>::new());
    let rel = kept_as(&last);
    assert!(
        rel.starts_with(".backup/stats/2026-10.toml.corrupt-"),
        "{rel}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

fn set_mtime(p: &Path, secs: u64) {
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap();
}

/// 启动迁移：只挪程序数据文件的 `.bak`（原文件在）与 `.corrupt-<时间>`，相对路径不变；用户自己的
/// `.bak` 不动；什么都不删（新位置已有一版 ⇒ 按修改时间留新的，旧的另存或留在原处）；迁移时
/// 不按 5 份裁剪；重名的留存续号后仍是留存名；只跑一次。
#[test]
fn legacy_backups_migrate_into_backup_dir() {
    let d = root("migrate");
    std::fs::create_dir_all(d.join("pinyin")).unwrap();
    std::fs::create_dir_all(d.join("stats")).unwrap();
    std::fs::create_dir_all(d.join(".backup")).unwrap();
    let mut files = vec![
        "pinyin/user.dict.yaml",
        "pinyin/user.dict.yaml.bak",
        "pinyin/shadow.dict.yaml.bak",
        "phrases.dict.yaml",
        "phrases.dict.yaml.bak",
        "notes.txt",
        "notes.txt.bak",
        "quick_format.toml.corrupt-20261016-120000",
        "quick_format.toml.corrupt-20261016-120000-2",
        "x.corrupt-notastamp",
    ];
    let stats: Vec<String> = (0..6)
        .map(|i| format!("stats/2026-10.toml.corrupt-2026101{i}-120000"))
        .collect();
    files.extend(stats.iter().map(String::as_str));
    for f in &files {
        std::fs::write(d.join(f), f).unwrap();
    }
    // 新位置已有：pinyin 那份比旁边的新 ⇒ 旁边的留在原处；phrases 那份更旧 ⇒ 另存、新的挪进去
    std::fs::create_dir_all(d.join(".backup/pinyin")).unwrap();
    std::fs::write(d.join(".backup/pinyin/user.dict.yaml"), "newer").unwrap();
    set_mtime(&d.join("pinyin/user.dict.yaml.bak"), 1_000);
    set_mtime(&d.join(".backup/pinyin/user.dict.yaml"), 2_000);
    std::fs::write(d.join(".backup/phrases.dict.yaml"), "older").unwrap();
    set_mtime(&d.join(".backup/phrases.dict.yaml"), 1_000);
    set_mtime(&d.join("phrases.dict.yaml.bak"), 2_000);
    // 已有同名留存 ⇒ 续号
    std::fs::write(
        d.join(".backup/quick_format.toml.corrupt-20261016-120000"),
        "there",
    )
    .unwrap();
    let is_data = |rel: &[&str]| {
        matches!(
            rel,
            ["pinyin", "user.dict.yaml" | "shadow.dict.yaml"]
                | ["phrases.dict.yaml" | "quick_format.toml"]
                | ["stats", _]
        )
    };
    assert_eq!(migrate_legacy_backups(&d, is_data), 9);
    assert_eq!(
        names(&d.join("pinyin")),
        [
            "shadow.dict.yaml.bak",
            "user.dict.yaml",
            "user.dict.yaml.bak"
        ],
        "更旧的与原文件不在的不动"
    );
    assert_eq!(
        std::fs::read_to_string(d.join(".backup/pinyin/user.dict.yaml")).unwrap(),
        "newer"
    );
    assert_eq!(
        std::fs::read_to_string(d.join(".backup/phrases.dict.yaml")).unwrap(),
        "phrases.dict.yaml.bak"
    );
    let bk = names(&d.join(".backup"));
    assert!(
        bk.iter().any(|n| n != "phrases.dict.yaml"
            && n.starts_with("phrases.dict.yaml")
            && std::fs::read_to_string(d.join(".backup").join(n)).unwrap() == "older"),
        "{bk:?}"
    );
    assert!(d.join("notes.txt.bak").is_file(), "用户自己的 .bak 不动");
    assert!(d.join("x.corrupt-notastamp").is_file());
    let qf: Vec<&String> = bk
        .iter()
        .filter(|n| n.starts_with("quick_format.toml.corrupt-"))
        .collect();
    assert_eq!(qf.len(), 3, "{bk:?}");
    assert!(
        qf.iter()
            .all(|n| corrupt_key(n, "quick_format.toml").is_some()),
        "{qf:?}"
    );
    assert_eq!(names(&d.join(".backup/stats")).len(), 6, "迁移不裁剪");
    assert!(d.join(".backup").join(MIGRATED_MARK).is_file());

    // 只跑一次：之后放进来的 `.bak` 不再被收走
    std::fs::write(d.join("phrases.dict.yaml.bak"), "mine").unwrap();
    assert_eq!(migrate_legacy_backups(&d, is_data), 0);
    assert_eq!(
        std::fs::read_to_string(d.join("phrases.dict.yaml.bak")).unwrap(),
        "mine"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// 改名带走上一版。
#[test]
fn move_backup_follows_rename() {
    let d = root("move");
    let (a, b) = (d.join("p/dicts/a.dict.yaml"), d.join("p/dicts/b.dict.yaml"));
    commit_text(&a, b"1", None).unwrap();
    commit_text(&a, b"2", None).unwrap();
    std::fs::rename(&a, &b).unwrap();
    move_backup(&a, &b);
    assert!(!backup_path(&a).exists());
    assert_eq!(std::fs::read_to_string(backup_path(&b)).unwrap(), "1");
    let _ = std::fs::remove_dir_all(&d);
}

/// 钩子是进程级的，并发的测试各装各的：一个测试卸下自己的钩子，不能顺手卸掉别人的。
#[test]
fn removing_one_hook_keeps_the_others() {
    let d = std::env::temp_dir().join(format!("wind_fold_hooks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let me = std::thread::current().id();
    let (a, b) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (a2, b2) = (a.clone(), b.clone());
    let ga = add_before_rename_hook(Box::new(move || {
        if std::thread::current().id() == me {
            a2.fetch_add(1, Ordering::SeqCst);
        }
    }));
    let gb = add_before_rename_hook(Box::new(move || {
        if std::thread::current().id() == me {
            b2.fetch_add(1, Ordering::SeqCst);
        }
    }));
    drop(gb);
    commit_text(&d.join("a.yaml"), b"x\n", None).unwrap();
    drop(ga);
    commit_text(&d.join("a.yaml"), b"y\n", None).unwrap();
    assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (1, 0));
    let _ = std::fs::remove_dir_all(&d);
}
