use super::*;

fn rows(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(t, c)| (t.to_string(), c.to_string()))
        .collect()
}

/// 清空后，旧一轮重建的写回与 `building` 复位都不该落到新槽上。
#[test]
fn clear_fences_off_in_flight_rebuild() {
    let slots: SharedSlots = Default::default();
    let old_epoch = slots.lock().unwrap().epoch;
    slots.lock().unwrap().touch("wb", 0).building = true;
    clear(&slots);
    // 清空后新一轮重建已起。
    slots.lock().unwrap().touch("wb", 0).building = true;
    let idx = Arc::new(UserTextIndex::from_rows("wb", (0, 0), rows(&[("工", "a")])));
    install(&slots, "wb", old_epoch, idx);
    drop(BuildingGuard(slots.clone(), "wb".into(), old_epoch));
    let g = slots.lock().unwrap();
    let s = g.map.get("wb").unwrap();
    assert!(s.index.is_none(), "旧线程的结果不该写进清空后的新槽");
    assert!(s.building, "旧线程不该复位新一轮重建的单飞标记");
}

#[test]
fn groups_codes_by_text_sorted_by_length() {
    let idx = UserTextIndex::from_rows(
        "wb",
        (0, 0),
        rows(&[
            ("工", "aaaa"),
            ("我", "q"),
            ("工", "a"),
            ("工", "aaaa"),
            ("工程", "aakg"),
        ]),
    );
    assert_eq!(
        idx.codes_of("工").collect::<Vec<_>>(),
        vec!["a", "aaaa"],
        "码长升序、去重"
    );
    assert_eq!(idx.codes_of("我").collect::<Vec<_>>(), vec!["q"]);
    assert_eq!(
        idx.codes_of("工程").collect::<Vec<_>>(),
        vec!["aakg"],
        "词组也收"
    );
    assert_eq!(idx.codes_of("无").count(), 0);
    assert_eq!(idx.codes_of("").count(), 0);
}

fn tmp_store(tag: &str) -> std::sync::Arc<wind_store::Store> {
    let p = std::env::temp_dir().join(format!("wind_text_codes_{tag}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    std::sync::Arc::new(wind_store::Store::open(&p).unwrap())
}

#[test]
fn prewarm_then_get_reads_user_words_of_that_schema_only() {
    let s = tmp_store("read");
    s.add_user_word("wb", "zzzz", "嗨", 0, 0).unwrap();
    s.add_user_word("pinyin", "hai", "嗨", 0, 0).unwrap();
    let slots = SharedSlots::default();
    assert!(prewarm(&slots, &s, "wb", 0));
    let idx = get_or_refresh(&slots, &s, "wb", 0, None).expect("预热后就绪");
    assert_eq!(
        idx.codes_of("嗨").collect::<Vec<_>>(),
        vec!["zzzz"],
        "不串方案"
    );
    assert!(!prewarm(&slots, &s, "wb", 0), "已是最新则不重建");
}

/// ★ 别的方案写入不让本方案过期（依赖 store 的按方案代次）。
#[test]
fn other_schema_write_keeps_index_fresh() {
    let s = tmp_store("fresh");
    let slots = SharedSlots::default();
    prewarm(&slots, &s, "wb", 0);
    s.add_user_word("pinyin", "nihao", "你好", 0, 0).unwrap();
    assert!(
        !prewarm(&slots, &s, "wb", 0),
        "拼音写入后五笔的索引仍是最新"
    );
    s.add_user_word("wb", "wqvb", "你好", 0, 0).unwrap();
    assert!(prewarm(&slots, &s, "wb", 0), "本方案写入后必须重建");
}

#[test]
fn get_or_refresh_returns_none_before_first_build() {
    let s = tmp_store("cold");
    let slots = SharedSlots::default();
    // 冷启动：本次拿不到（后台去建），调用方按「这一层没就绪」处理。
    assert!(get_or_refresh(&slots, &s, "wb", 0, None).is_none());
}

/// 轮询到「该槽有索引且不在重建中」为止（上限 5 秒），返回那份索引。
fn wait_built(slots: &SharedSlots, key: &str) -> Arc<UserTextIndex> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        {
            let g = slots.lock().unwrap();
            if let Some(s) = g.map.get(key)
                && !s.building
                && let Some(i) = &s.index
            {
                return i.clone();
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "后台重建 5 秒内没有完成"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn building(slots: &SharedSlots, key: &str) -> bool {
    slots
        .lock()
        .unwrap()
        .map
        .get(key)
        .is_some_and(|s| s.building)
}

/// ★ 后台重建的结果会被**下一次调用**看到：冷启动那次返回 None 并起重建，建好后
/// `get_or_refresh` 拿到新表、单飞标记已复位且不再起重建；之后本方案写入 → 本次照返回
/// 旧表 + 后台重建 → 下一次拿到新内容。
#[test]
fn background_rebuild_is_seen_by_next_call() {
    let s = tmp_store("bg");
    s.add_user_word("wb", "zzzz", "嗨", 0, 0).unwrap();
    let slots = SharedSlots::default();
    assert!(
        get_or_refresh(&slots, &s, "wb", 0, None).is_none(),
        "冷启动本次拿不到"
    );
    let built = wait_built(&slots, "wb");
    assert_eq!(built.codes_of("嗨").collect::<Vec<_>>(), vec!["zzzz"]);
    let got = get_or_refresh(&slots, &s, "wb", 0, None).expect("建好后下一次调用拿得到");
    assert!(Arc::ptr_eq(&got, &built));
    assert!(!building(&slots, "wb"), "已是最新，不再起重建");

    s.add_user_word("wb", "aaaa", "嗨", 0, 0).unwrap();
    let stale = get_or_refresh(&slots, &s, "wb", 0, None).expect("过期时照返回旧表");
    assert_eq!(
        stale.codes_of("嗨").collect::<Vec<_>>(),
        vec!["zzzz"],
        "本次是旧内容"
    );
    // `building` 在 get_or_refresh 返回前已于锁内置位，故这里等到的必是新表。
    let fresh = wait_built(&slots, "wb");
    assert!(!Arc::ptr_eq(&fresh, &stale));
    assert_eq!(
        fresh.codes_of("嗨").collect::<Vec<_>>(),
        vec!["aaaa", "zzzz"]
    );
    let again = get_or_refresh(&slots, &s, "wb", 0, None).unwrap();
    assert!(Arc::ptr_eq(&again, &fresh), "下一次调用拿到新表");
    assert!(!building(&slots, "wb"));
}

/// 刚好到标记门槛的一份用户词（小库扫完不标记，见 `SCAN_RECLAIM_MIN_ROWS`）。
fn big_dict() -> Vec<wind_store::wdict::WordIo> {
    (0..wind_store::store::SCAN_RECLAIM_MIN_ROWS)
        .map(|i| wind_store::wdict::WordIo {
            code: format!("x{i}"),
            text: format!("词{i}"),
            weight: 0,
            count: 0,
            boundary: None,
        })
        .collect()
}

const TICK: std::time::Duration = std::time::Duration::from_millis(20);
const SCAN_IDLE: std::time::Duration = std::time::Duration::from_millis(150);
const NEVER: std::time::Duration = std::time::Duration::from_secs(3600);

/// 轮询到 `drops >= n`（上限 5 秒）。
fn wait_drops(s: &wind_store::Store, n: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if s.page_cache_drops() >= n {
            return true;
        }
        std::thread::sleep(TICK / 2);
    }
    false
}

/// 小库：建完不标记，短档过后也不回收（门槛之下，留给空闲回收的长档）。
#[test]
fn small_build_keeps_the_page_cache() {
    let s = tmp_store("small");
    s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
    s.add_user_word("wb", "zzzz", "嗨", 0, 0).unwrap();
    let slots = SharedSlots::default();
    assert!(prewarm(&slots, &s, "wb", 0));
    std::thread::sleep(SCAN_IDLE * 4);
    assert_eq!(s.page_cache_drops(), 0);
}

/// ★ 建表是一次全表扫：预热（阻塞）与后台重建两条路建完都**只标记**，库空闲满短档后
/// 才回收一次（`docs/design/memory-footprint.md` §6）——建完当场关库重开会让按键线程等。
#[test]
fn every_build_marks_and_reclaims_after_short_idle() {
    let s = tmp_store("reclaim");
    s.import_user_words("wb", &big_dict()).unwrap();
    s.add_user_word("wb", "zzzz", "嗨", 0, 0).unwrap();
    s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
    let slots = SharedSlots::default();
    assert!(prewarm(&slots, &s, "wb", 0));
    assert_eq!(s.page_cache_drops(), 0, "预热建完不立即回收");
    assert!(wait_drops(&s, 1), "空闲满短档后回收");

    s.add_user_word("wb", "aaaa", "嗨", 0, 0).unwrap();
    get_or_refresh(&slots, &s, "wb", 0, None);
    let fresh = wait_built(&slots, "wb");
    assert_eq!(
        fresh.codes_of("嗨").collect::<Vec<_>>(),
        vec!["aaaa", "zzzz"]
    );
    assert!(wait_drops(&s, 2), "后台重建之后同样在空闲满短档后回收");
    std::thread::sleep(SCAN_IDLE * 3);
    assert_eq!(s.page_cache_drops(), 2, "每次扫描只回收一次");
}

/// ★ 模拟打字（自动造词）：持续访问库、其间不断写新词触发后台重建——一次都不回收；
/// 停手满短档后回收**一次**。
#[test]
fn rebuilds_while_typing_never_reclaim_until_typing_stops() {
    let s = tmp_store("typing");
    s.import_user_words("wb", &big_dict()).unwrap();
    s.spawn_idle_cache_reclaimer(NEVER, SCAN_IDLE, TICK);
    let slots = SharedSlots::default();
    let mut builds = 0;
    for i in 0..8 {
        // 「打字」：间隔远小于短档地查库；每轮造一个新词，令索引过期、后台重建。
        s.add_user_word("wb", &format!("y{i}"), &format!("新{i}"), 0, 0)
            .unwrap();
        get_or_refresh(&slots, &s, "wb", 0, None);
        let t0 = std::time::Instant::now();
        while t0.elapsed() < SCAN_IDLE / 2 {
            let _ = s.search_user_words_prefix("wb", "x1", 5).unwrap();
            std::thread::sleep(TICK / 4);
        }
        if wait_built(&slots, "wb")
            .codes_of(&format!("新{i}"))
            .next()
            .is_some()
        {
            builds += 1;
        }
    }
    assert!(builds >= 4, "前提：打字期间确实重建过多次（{builds}）");
    assert_eq!(s.page_cache_drops(), 0, "持续打字期间不该回收");
    assert!(wait_drops(&s, 1), "停手满短档后回收");
    std::thread::sleep(SCAN_IDLE * 3);
    assert_eq!(s.page_cache_drops(), 1, "只回收一次");
}

/// 闲置清扫只放「闲够久且不在重建中」的槽：在建的删了，重建线程白跑、单飞标记也丢。
#[test]
fn evict_idle_spares_building_and_recently_used_slots() {
    let slots = SharedSlots::default();
    {
        let mut g = slots.lock().unwrap();
        g.touch("old", 0);
        g.touch("busy", 0).building = true;
        g.touch("recent", 900);
    }
    assert_eq!(evict_idle(&slots, 1000, 500), vec!["old".to_string()]);
    let g = slots.lock().unwrap();
    assert!(g.map.contains_key("busy") && g.map.contains_key("recent"));
}

#[test]
fn slots_are_capped() {
    let s = tmp_store("cap");
    let slots = SharedSlots::default();
    for id in ["a", "b", "c", "d", "e", "f"] {
        prewarm(&slots, &s, id, 0);
    }
    assert!(slots.lock().unwrap().map.len() <= MAX_SLOTS);
}
