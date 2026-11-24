use super::*;
use crate::codetable::CodetableDict;
use crate::datformat::WdatWriter;

/// 造一个最小可用的 wdat，返回其路径。
fn make_wdat(dir: &Path, name: &str, code: &str, text: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let mut d = CodetableDict::empty();
    d.merge_single(code.into(), text.into(), 1, 0);
    let mut w = WdatWriter::new();
    d.export_to_wdat(&mut w);
    w.write(&path).unwrap();
    path
}

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("wind-reader-pool-{}-{}", std::process::id(), tag))
}

/// 把 `path` 的 mtime 拨回 `before` 那一刻，并断言「大小 + mtime」与之逐字段相同。
///
/// 「重建前后 stamp 恰好一致」在生产上是时序赌局（同一 mtime 刻度内完成两次写入），
/// 让测试去赌会假绿（`note_replaced` 注释掉照样全过）。这里把它变成前提并当场校验，
/// 大小若变了直接报错，免得测试退化成在测 stamp 判据。
///
/// 这几条测试守的是 Windows 语义（被 mmap 的文件照样能被 rename 覆盖），本函数要对正被
/// mmap 的文件再开写句柄去 `SetFileTime`。理论上不冲突（std 带 `FILE_SHARE_WRITE`，只改
/// 元数据），但只在 Linux 上跑过；Windows 上若在此处失败，是夹具的问题而非被测逻辑的问题。
fn force_same_stamp(path: &Path, before: &std::fs::Metadata) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_accessed(before.accessed().unwrap())
                .set_modified(before.modified().unwrap()),
        )
        .unwrap();
    let after = std::fs::metadata(path).unwrap();
    assert_eq!(
        (before.len(), before.modified().unwrap()),
        (after.len(), after.modified().unwrap()),
        "前提没造出来：重建后大小或 mtime 仍有差别，本测试会退化成在测 stamp 判据"
    );
}

#[test]
fn same_path_shares_one_reader() {
    let dir = temp_dir("share");
    let p = make_wdat(&dir, "a.wdat", "a", "啊");

    let r1 = open_wdat(&p).unwrap();
    let r2 = open_wdat(&p).unwrap();
    assert!(
        Arc::ptr_eq(&r1, &r2),
        "同一路径必须复用同一个 reader（这正是本池的目的）"
    );
    assert_eq!(Arc::strong_count(&r1), 2, "两个持有者");
    // 复用的 reader 功能正常
    assert_eq!(r2.search("a").len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

/// single-flight 的核心契约：同一路径的构建区间互斥。
/// 用「同时进入临界区的最大并发数」来验证——它必须恒为 1。
#[test]
fn file_lock_serializes_same_path() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let path = temp_dir("lock-same").join("f.wdat");
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let hs: Vec<_> = (0..8)
        .map(|_| {
            let (path, inside, peak) = (path.clone(), inside.clone(), peak.clone());
            std::thread::spawn(move || {
                let lock = file_lock(&path);
                let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
                let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(5));
                inside.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "同一路径的构建区间必须互斥，否则冷启动会并发 rename 同一个缓存文件"
    );
}

/// 不同路径不得互相阻塞——否则一个大词库的重建会拖住所有其他词库。
#[test]
fn file_lock_does_not_block_different_paths() {
    let dir = temp_dir("lock-distinct");
    let (a, b) = (dir.join("a.wdat"), dir.join("b.wdat"));
    let la = file_lock(&a);
    let _ga = la.lock().unwrap_or_else(|e| e.into_inner());
    // a 已被本线程持有；另一线程锁 b 应立刻拿到
    let done = std::thread::spawn(move || {
        let lb = file_lock(&b);
        let _gb = lb.lock().unwrap_or_else(|e| e.into_inner());
    });
    done.join().expect("锁不同路径不应被阻塞");
}

/// 并发加载同一份 yaml：无论谁先建好缓存，最终所有调用方都应拿到同一个 reader。
/// 若 single-flight 失效，多个线程会各自重建、rename 互撞，落到不同文件上。
#[test]
fn concurrent_load_converges_to_one_reader() {
    let dir = temp_dir("concurrent-load");
    std::fs::create_dir_all(&dir).unwrap();
    let yaml = dir.join("c.dict.yaml");
    // 正文须在独占一行的 `...` 之后，否则解析出零条目、退化成 Memory 分支
    std::fs::write(&yaml, "name: c\n...\n啊\taa\t1\n再\tzz\t1\n").unwrap();
    let cache = dir.join("cache").join("c.wdat");

    let hs: Vec<_> = (0..6)
        .map(|_| {
            let (yaml, cache) = (yaml.clone(), cache.clone());
            std::thread::spawn(move || {
                let d = crate::cached::CachedDict::load_at_with(&yaml, &cache, false).unwrap();
                match d {
                    crate::cached::CachedDict::Mmap(r) => Some(r),
                    // 缓存写入失败会退化成 Memory，这里不该发生（单文件加载也不会是 Layered）
                    _ => None,
                }
            })
        })
        .collect();
    let readers: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();

    let first = readers[0].clone().expect("应走 mmap 路径");
    for r in &readers {
        let r = r.clone().expect("每个线程都应拿到 mmap reader");
        assert!(
            Arc::ptr_eq(&first, &r),
            "并发加载同一词库须收敛到同一个 reader（single-flight + 池）"
        );
    }

    drop(readers);
    drop(first);
    std::fs::remove_dir_all(&dir).ok();
}

/// 词库在仍被持有时被重建，随后的取用必须拿到新内容，不得复用陈旧 reader。
///
/// 池化后若只按路径命中，就会把仍指向替换前数据的 reader 交出去，表现为「改了词库不生效，
/// 重启才行」。前提（本测试同时锁定）：rename 覆盖一个正被 mmap 的文件会成功，旧 view
/// 继续看到旧数据，所以不能指望「重建失败」来兜底。
///
/// 重建后用 `force_same_stamp` 把 mtime 拨回，让 stamp 判据的两个字段双双失效，只剩替换
/// 代数能救；靠时序制造「stamp 相同」会假绿。
#[test]
fn rebuilt_file_is_not_served_from_stale_entry() {
    let dir = temp_dir("rebuild");
    let p = make_wdat(&dir, "r.wdat", "aa", "旧");
    let before = std::fs::metadata(&p).unwrap();

    let held = open_wdat(&p).unwrap(); // 模拟另一个方案的引擎仍持有
    assert_eq!(held.search("aa").len(), 1);

    // 持有期间重建该词库（WdatWriter 内部走 tmp + rename）。
    // 码长与字数与旧内容一致 → 文件大小不变 → 判据的「大小」一栏先失效。
    let mut d = CodetableDict::empty();
    d.merge_single("bb".into(), "新".into(), 1, 0);
    let mut w = WdatWriter::new();
    d.export_to_wdat(&mut w);
    w.write(&p)
        .expect("被 mmap 持有不影响 rename 覆盖（Windows 亦然）");

    // 再把 mtime 拨回替换前，让「修改时间」一栏也失效——这是生产上「同一刻度内两次
    // 写入」的确定化复现，不是人造的极端场景。
    force_same_stamp(&p, &before);

    // 旧持有者继续看旧数据——这是 OS 语义，不是缺陷
    assert_eq!(held.search("aa").len(), 1, "旧 view 应继续指向替换前的数据");
    assert!(held.search("bb").is_empty());

    // 关键：新的取用必须反映重建后的内容
    let fresh = open_wdat(&p).unwrap();
    assert!(
        !Arc::ptr_eq(&held, &fresh),
        "文件已被替换，绝不能复用旧 reader"
    );
    assert_eq!(fresh.search("bb").len(), 1, "新取用须读到重建后的内容");
    assert!(fresh.search("aa").is_empty());

    drop(held);
    drop(fresh);
    std::fs::remove_dir_all(&dir).ok();
}

/// 注释库同理：`write_comment_wcmt` 也必须通知本池，否则挂载着注释库的方案重建后
/// 仍供出旧释义。三个写盘口各挂各的钩子，漏一个就得单独测出来。
#[test]
fn rebuilt_comment_dict_is_not_served_from_stale_entry() {
    let dir = temp_dir("rebuild-wcmt");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("c.wcmt");
    let row = |c: &str| vec![("啊".to_string(), c.to_string(), String::new())];

    crate::commentdict::write_comment_wcmt(&p, &row("旧释")).unwrap();
    let before = std::fs::metadata(&p).unwrap();
    let held = open_comment(&p).unwrap();
    assert_eq!(held.lookup_first("啊"), Some("旧释"));

    // 释义字数相同 → 文件大小不变；再拨回 mtime → stamp 两栏双双失效。
    crate::commentdict::write_comment_wcmt(&p, &row("新释")).unwrap();
    force_same_stamp(&p, &before);

    let fresh = open_comment(&p).unwrap();
    assert_eq!(
        fresh.lookup_first("啊"),
        Some("新释"),
        "注释库重建后必须读到新释义"
    );
    drop((held, fresh));
    std::fs::remove_dir_all(&dir).ok();
}

/// emoji 表同理：功能开关一关一开会重新解析并 rename 覆盖，`write_emoji_wemj`
/// 同样得通知本池。
#[test]
fn rebuilt_emoji_dict_is_not_served_from_stale_entry() {
    let dir = temp_dir("rebuild-wemj");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("e.wemj");
    let row = |e: &str| vec![("你好".to_string(), vec![e.to_string()])];

    crate::emojidict::write_emoji_wemj(&p, &row("😊")).unwrap();
    let before = std::fs::metadata(&p).unwrap();
    let held = open_emoji(&p).unwrap();
    assert_eq!(held.lookup("你好"), Some("😊"));

    // 两个 emoji 都是 4 字节 → 文件大小不变。
    crate::emojidict::write_emoji_wemj(&p, &row("👋")).unwrap();
    force_same_stamp(&p, &before);

    let fresh = open_emoji(&p).unwrap();
    assert_eq!(
        fresh.lookup("你好"),
        Some("👋"),
        "emoji 表重建后必须读到新内容"
    );
    drop((held, fresh));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn different_paths_do_not_share() {
    let dir = temp_dir("distinct");
    let p1 = make_wdat(&dir, "b.wdat", "b", "波");
    let p2 = make_wdat(&dir, "c.wdat", "c", "此");

    let r1 = open_wdat(&p1).unwrap();
    let r2 = open_wdat(&p2).unwrap();
    assert!(!Arc::ptr_eq(&r1, &r2));

    std::fs::remove_dir_all(&dir).ok();
}

/// 池存 Weak 的核心契约：持有者全部释放后 mmap 必须解除，否则 Windows 上词库重建
/// 的 rename 会永久失败。用「释放后能否覆写该文件」来验证映射确实断开了。
#[test]
fn dropping_all_holders_releases_the_mapping() {
    let dir = temp_dir("release");
    let p = make_wdat(&dir, "d.wdat", "d", "的");

    let r = open_wdat(&p).unwrap();
    assert_eq!(Arc::strong_count(&r), 1);
    drop(r);

    // 全部持有者已释放 → 文件不再被映射 → 可覆写（Windows 上映射未解除时这里会失败）
    let mut d = CodetableDict::empty();
    d.merge_single("dd".into(), "地".into(), 1, 0);
    let mut w = WdatWriter::new();
    d.export_to_wdat(&mut w);
    w.write(&p).expect("持有者释放后必须能覆写词库文件");

    // 失效条目不会被复用：重新打开应拿到覆写后的新内容
    let r2 = open_wdat(&p).unwrap();
    assert_eq!(r2.search("dd").len(), 1, "应读到覆写后的新内容");
    assert_eq!(r2.search("d").len(), 0, "旧内容不应再出现");

    drop(r2);
    std::fs::remove_dir_all(&dir).ok();
}
