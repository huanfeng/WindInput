use super::*;
use std::path::PathBuf;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wind_doc_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn modify_rereads_and_commits() {
    let d = tmpdir("modify");
    let doc = TextDoc::open(d.join("a.yaml"));
    assert!(doc.get().is_none());
    doc.modify(&mut |cur| {
        assert!(cur.is_none());
        Ok(Some("x: 1\n".into()))
    })
    .unwrap();
    std::fs::write(d.join("a.yaml"), "x: 2\n").unwrap(); // 手改
    doc.modify(&mut |cur| {
        assert_eq!(cur, Some("x: 2\n"));
        Ok(Some(format!("{}y: 3\n", cur.unwrap())))
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(d.join("a.yaml")).unwrap(),
        "x: 2\ny: 3\n"
    );
    assert_eq!(
        doc.get().as_deref().map(String::as_str),
        Some("x: 2\ny: 3\n")
    );
}

#[test]
fn concurrent_hand_edit_during_commit_is_refused() {
    let d = tmpdir("race");
    let p = d.join("a.yaml");
    let doc = TextDoc::open(p.clone());
    doc.modify(&mut |_| Ok(Some("v: 1\n".into()))).unwrap();
    let p2 = p.clone();
    // 钩子是进程级的：只在本测试线程上动手，别的测试并发提交时不碰
    let me = std::thread::current().id();
    let hook = crate::text::fold::add_before_rename_hook(Box::new(move || {
        if std::thread::current().id() == me {
            std::fs::write(&p2, "v: hand\n").unwrap();
        }
    }));
    let r = doc.modify(&mut |_| Ok(Some("v: prog\n".into())));
    drop(hook);
    assert!(r.is_err());
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "v: hand\n",
        "手改不被覆盖"
    );
}

/// M-1：闭包报错（文件解析不了）⇒ 不提交：文件、mtime、`.bak` 都不动。
#[test]
fn modify_error_leaves_file_and_bak_untouched() {
    let d = tmpdir("moderr");
    let p = d.join("x.toml");
    std::fs::write(&p, "a = 1\n").unwrap();
    let doc = TextDoc::open(p.clone());
    doc.modify(&mut |_| Ok(Some("a = 2\n".into()))).unwrap(); // .bak = "a = 1"
    std::fs::write(&p, "a = [\n").unwrap();
    let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(doc.modify(&mut |_| Err(anyhow::anyhow!("坏"))).is_err());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "a = [\n");
    assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), mtime);
    assert_eq!(
        std::fs::read_to_string(d.join("x.toml.bak")).unwrap(),
        "a = 1\n",
        "好的 .bak 不被坏内容覆盖"
    );
}

#[test]
fn none_deletes_and_sync_sees_external_changes() {
    let d = tmpdir("sync");
    let p = d.join("a.yaml");
    let doc = TextDoc::open(p.clone());
    doc.modify(&mut |_| Ok(Some("a\n".into()))).unwrap();
    std::fs::write(&p, "b\n").unwrap();
    assert!(doc.sync());
    assert!(!doc.sync());
    doc.modify(&mut |_| Ok(None)).unwrap();
    assert!(!p.exists() && doc.get().is_none());
    doc.replace(Some("c\n")).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "c\n");
}
