use super::*;

fn texts<'a>(v: Vec<UserHit<'a>>) -> Vec<(&'a str, i64)> {
    v.iter().map(|h| (h.text, h.rank)).collect()
}

fn row(t: &str, rank: i64, count: u32) -> Row {
    (t.into(), "c".into(), rank, count)
}

#[test]
fn prefix_is_strict_sorted_by_rank_and_deduped() {
    let t = TextTable::build(vec![
        row("荷载", 5, 1),
        row("荷", 99, 1),
        row("荷载效应", 7, 1),
        row("荷载", 9, 2),
        row("花卉", 100, 1),
        row("荷包", 1, 1),
    ]);
    assert_eq!(
        texts(t.with_prefix("荷", 10, |_| true)),
        vec![("荷载", 9), ("荷载效应", 7), ("荷包", 1)]
    );
    assert_eq!(
        t.with_prefix("荷", 10, |_| true)[0].count,
        3,
        "同文本多码次数取和"
    );
    assert_eq!(
        texts(t.with_prefix("荷载", 10, |_| true)),
        vec![("荷载效应", 7)]
    );
    assert_eq!(texts(t.with_prefix("荷", 1, |_| true)), vec![("荷载", 9)]);
    assert!(t.with_prefix("荷", 10, |h| h.count > 5).is_empty());
    assert!(t.with_prefix("无", 10, |_| true).is_empty());
}

fn tmp_store(tag: &str) -> wind_store::Store {
    let p = std::env::temp_dir().join(format!("wind_user_assoc_{tag}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    wind_store::Store::open(&p).unwrap()
}

/// ★ 连续选词（已有临时词 count +1）不让索引过期 ⇒ 不触发后台全表重建。
#[test]
fn repeated_picks_do_not_invalidate_index() {
    let s = tmp_store("picks");
    s.add_user_word("wb", "awfa", "荷载", 0, 0).unwrap();
    s.learn_temp_word("wb", "awgg", "荷叶田田", 800, 0).unwrap();
    let idx = UserAssocIndex::build(&s, "wb");
    for _ in 0..10 {
        s.learn_temp_word("wb", "awgg", "荷叶田田", 800, 0).unwrap();
        s.increment_temp_if_exists("wb", "awgg", "荷叶田田")
            .unwrap();
        assert!(
            !idx.is_stale(&s, COUNT_REBUILD_MIN_INTERVAL),
            "选词后按键路径不该判过期"
        );
    }
    // 但临时词 count 确实变了：节流窗口过后（这里用 0 模拟）要跟上。
    assert!(idx.is_stale(&s, Duration::ZERO));
    // 结构变化（新词）立即过期。
    let idx = UserAssocIndex::build(&s, "wb");
    s.learn_temp_word("wb", "awhh", "荷塘月色", 800, 0).unwrap();
    assert!(idx.is_stale(&s, COUNT_REBUILD_MIN_INTERVAL));
}

/// 用户词权重变化是结构变化：排序键变了。
#[test]
fn user_weight_change_invalidates_index() {
    let s = tmp_store("boost");
    s.add_user_word("wb", "awfa", "荷载", 0, 0).unwrap();
    let idx = UserAssocIndex::build(&s, "wb");
    assert!(!idx.is_stale(&s, COUNT_REBUILD_MIN_INTERVAL));
    s.update_user_word_weight("wb", "awfa", "荷载", 5).unwrap();
    assert!(idx.is_stale(&s, COUNT_REBUILD_MIN_INTERVAL), "权重变了");
}

#[test]
fn contains_is_exact() {
    let s = tmp_store("contains");
    s.add_user_word("wb", "awfa", "荷载", 0, 0).unwrap();
    s.learn_temp_word("wb", "awgg", "荷叶田田", 800, 0).unwrap();
    let idx = UserAssocIndex::build(&s, "wb");
    assert!(idx.contains("荷载") && idx.contains("荷叶田田"));
    assert!(!idx.contains("荷") && !idx.contains("荷叶"));
}

/// ★ 建索引是一次全表扫：预热与后台重建两条路建完都**只标记**，库空闲满短档后才回收
/// 一次（`docs/design/memory-footprint.md` §6）。
#[test]
fn every_build_marks_and_reclaims_after_short_idle() {
    const TICK: Duration = Duration::from_millis(20);
    const SCAN_IDLE: Duration = Duration::from_millis(150);
    let wait_drops = |s: &wind_store::Store, n: u64| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if s.page_cache_drops() >= n {
                return true;
            }
            std::thread::sleep(TICK / 2);
        }
        false
    };
    let s = Arc::new(tmp_store("reclaim"));
    let big: Vec<_> = (0..wind_store::store::SCAN_RECLAIM_MIN_ROWS)
        .map(|i| wind_store::wdict::WordIo {
            code: format!("x{i}"),
            text: format!("词{i}"),
            weight: 0,
            count: 0,
            boundary: None,
        })
        .collect();
    s.import_user_words("wb", &big).unwrap();
    s.add_user_word("wb", "awfa", "荷载", 0, 0).unwrap();
    s.spawn_idle_cache_reclaimer(Duration::from_secs(3600), SCAN_IDLE, TICK);
    let slot = SharedSlot::default();
    assert!(prewarm(&slot, &s, "wb"));
    assert_eq!(s.page_cache_drops(), 0, "预热建完不立即回收");
    assert!(wait_drops(&s, 1), "空闲满短档后回收");

    s.add_user_word("wb", "awgg", "荷叶", 0, 0).unwrap();
    get_or_refresh(&slot, &s, "wb");
    assert!(wait_drops(&s, 2), "后台重建之后同样在空闲满短档后回收");
    assert!(
        slot.lock()
            .unwrap()
            .index
            .as_ref()
            .is_some_and(|i| i.contains("荷叶")),
        "回收前重建已完成"
    );
}
