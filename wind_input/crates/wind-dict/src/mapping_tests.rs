use super::*;

/// 预算与全局计数是进程级的：碰它们的测试串行，且只经 `map_file_with` 传策略，
/// 不改全局策略（其它测试默认 `Off`，不会动这些计数）。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn temp_file(tag: &str, bytes: &[u8]) -> (std::path::PathBuf, File) {
    let path = std::env::temp_dir().join(format!("wind_map_{tag}_{}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let f = File::open(&path).unwrap();
    (path, f)
}

#[test]
fn reserve_stops_at_the_budget_and_never_overshoots() {
    let c = AtomicU64::new(0);
    assert_eq!(reserve(&c, 100, 60), Some(60));
    assert_eq!(reserve(&c, 100, 40), Some(100), "刚好到顶可以");
    assert_eq!(reserve(&c, 100, 1), None);
    assert_eq!(c.load(Ordering::Relaxed), 100, "占不到时不改计数");
    assert_eq!(reserve(&c, 100, u64::MAX), None, "溢出按占不到处理");
}

#[test]
fn locked_map_keeps_content_and_returns_its_budget_on_drop() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (path, f) = temp_file("lock", b"wind pin bytes");
    let (l0, f0) = (locked_bytes(), lock_failed_bytes());
    let m = map_file_with(&f, CachePolicy::Lock).unwrap();
    assert_eq!(&m[..], b"wind pin bytes", "锁页失败只能退化，内容必须原样");
    // 配额够就记锁定，不够就记失败；两者之和恰为文件长度。
    assert_eq!(locked_bytes() - l0 + lock_failed_bytes() - f0, 14);
    drop(m);
    assert_eq!(
        (locked_bytes(), lock_failed_bytes()),
        (l0, f0),
        "释放后计数归还"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn map_over_budget_is_left_plain_and_counted() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (path, f) = temp_file("over", b"wind pin bytes");
    let (l0, o0) = (locked_bytes(), over_budget_bytes());
    set_budget(10);
    let m = map_file_with(&f, CachePolicy::Lock).unwrap();
    set_budget(u64::MAX);
    assert_eq!(&m[..], b"wind pin bytes");
    assert_eq!(locked_bytes(), l0, "超预算不锁");
    assert_eq!(over_budget_bytes() - o0, 14);
    drop(m);
    assert_eq!(over_budget_bytes(), o0, "释放后归还");
    std::fs::remove_file(&path).ok();
}

#[test]
fn preload_copies_into_memory_counts_and_returns_on_drop() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (path, f) = temp_file("pre", b"wind pin bytes");
    let p0 = preloaded_bytes();
    let m = map_file_with(&f, CachePolicy::Preload).unwrap();
    assert_eq!(&m[..], b"wind pin bytes");
    assert_eq!(preloaded_bytes() - p0, 14);
    drop(m);
    assert_eq!(preloaded_bytes(), p0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn default_budget_is_capped_at_512_mb() {
    let b = default_budget();
    assert!(b > 0 && b <= 512 << 20, "{b}");
}

#[test]
fn config_values_map_to_policies_and_unknown_falls_back_to_lock() {
    for (s, p) in [
        ("lock", CachePolicy::Lock),
        ("preload", CachePolicy::Preload),
        ("off", CachePolicy::Off),
        ("", CachePolicy::Lock),
        ("bogus", CachePolicy::Lock),
    ] {
        assert_eq!(CachePolicy::from_config(s), p, "{s:?}");
    }
    for p in [CachePolicy::Lock, CachePolicy::Preload, CachePolicy::Off] {
        assert_eq!(CachePolicy::from_config(p.as_config()), p);
        assert_eq!(CachePolicy::from_u8(p.to_u8()), p);
    }
}
