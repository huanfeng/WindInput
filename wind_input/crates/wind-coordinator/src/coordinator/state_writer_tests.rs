use super::*;

/// 空实现：不起线程、`schedule` 不写盘，也不 panic。
///
/// 这条钉的是测试夹具的安全性——协调器的单测会构造出成百上千个实例，
/// 任何一个真写了盘，跑一次 `cargo test` 就改掉开发者本机的 `state.toml`，
/// 还会与正在运行的服务抢同一个文件。
#[test]
fn disabled_writer_is_a_noop() {
    let dir = std::env::temp_dir().join("wind-state-writer-noop-must-not-exist");
    let w = StateWriter::new(false, Some(dir.clone()));
    w.schedule("toolbar_anchors", |rs| {
        rs.toolbar_anchors.insert("k".into(), (1, 2));
    });
    drop(w);
    assert!(
        !dir.join("state.toml").exists(),
        "禁用的写入器不得落盘: {}",
        dir.display()
    );
}

/// 同种类的多次 `schedule` 只落**最后一次**；不同种类并存。
///
/// 前半条是防抖的目的本身（连续微调不该写 N 次盘），后半条是它的边界：
/// 合并绝不能把别的字段一起吞掉——那正是本模块要根治的"丢更新"。
#[test]
fn same_kind_collapses_and_other_kinds_survive() {
    let dir = tempdir();
    {
        let w = StateWriter::new(true, Some(dir.clone()));
        for i in 1..=5 {
            w.schedule("toolbar_anchors", move |rs| {
                rs.toolbar_anchors.insert("m".into(), (i, i));
            });
        }
        w.schedule("softkeyboard_anchors", |rs| {
            rs.softkeyboard_anchors.insert("m".into(), (9, 9));
        });
        // Drop 里 flush：不必等满一个防抖窗口。
    }
    let rs = RuntimeState::load(&dir);
    assert_eq!(rs.toolbar_anchors.get("m"), Some(&(5, 5)), "该留最后一次");
    assert_eq!(
        rs.softkeyboard_anchors.get("m"),
        Some(&(9, 9)),
        "异种不得被吞"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 关机 flush：`Drop` 返回时防抖窗口内的变更必须已经落盘。
///
/// 没有这一条，"拖完立刻关机"就丢——而那恰恰是用户最在意的那一次拖动。
#[test]
fn drop_flushes_pending_edit() {
    let dir = tempdir();
    {
        let w = StateWriter::new(true, Some(dir.clone()));
        w.schedule("toolbar_anchors", |rs| {
            rs.toolbar_anchors.insert("bye".into(), (7, 8));
        });
    }
    assert_eq!(
        RuntimeState::load(&dir).toolbar_anchors.get("bye"),
        Some(&(7, 8)),
        "Drop 返回时待落变更应已写盘"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 多次 flush（`Drop` 之外还手动调过）不得 panic 或重复 join。
#[test]
fn flush_is_idempotent() {
    let dir = tempdir();
    let mut w = StateWriter::new(true, Some(dir.clone()));
    w.schedule("toolbar_anchors", |rs| {
        rs.toolbar_anchors.insert("x".into(), (1, 1));
    });
    w.flush_blocking();
    w.flush_blocking();
    drop(w);
    assert_eq!(
        RuntimeState::load(&dir).toolbar_anchors.get("x"),
        Some(&(1, 1))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 停止之后再来的 `schedule` 被丢弃，而不是让 worker 永远等下去。
#[test]
fn schedule_after_stop_is_dropped() {
    let dir = tempdir();
    let mut w = StateWriter::new(true, Some(dir.clone()));
    w.flush_blocking();
    w.schedule("toolbar_anchors", |rs| {
        rs.toolbar_anchors.insert("late".into(), (3, 3));
    });
    drop(w);
    assert!(
        !RuntimeState::load(&dir)
            .toolbar_anchors
            .contains_key("late"),
        "停止后的变更不该落盘"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn tempdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "wind-state-writer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建临时目录");
    d
}
