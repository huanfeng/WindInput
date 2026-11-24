use super::*;

fn mem_store() -> Arc<Store> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static CNT: AtomicU32 = AtomicU32::new(0);
    let n = CNT.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("wind_collector_{}_{}.redb", std::process::id(), n));
    let _ = std::fs::remove_file(&p);
    Arc::new(Store::open(&p).unwrap())
}

/// M-3：元数据文件坏了（无可用备份）时，收集器起步不得拿零值累加写回。
#[test]
fn collector_starts_from_recalculated_meta_when_meta_file_is_corrupt() {
    let store = mem_store();
    let d = crate::stats::DailyStats {
        chinese: 300,
        ..Default::default()
    };
    store.put_daily_stat("2020-01-01", &d).unwrap();
    store.recalculate_stats_meta(1.0).unwrap();
    let mp = store.stats_dir().join("meta.toml");
    std::fs::write(&mp, "total_chars = [").unwrap();
    let _ = std::fs::remove_file(wind_userdata::text::fold::backup_path(&mp));
    let sc = StatCollector::new(store.clone(), 1.0);
    assert_eq!(sc.get_meta().total_chars, 300);
    wind_userdata::issues::clear(&mp);
}

#[test]
fn test_record_aggregates_all_dimensions() {
    let sc = StatCollector::new(mem_store(), 1.0);
    sc.record(StatEvent {
        chinese: 5,
        code_len: 2,
        candidate_pos: 0,
        source: CommitSource::Candidate,
        schema_id: "wubi86".into(),
        ..Default::default()
    });

    let today = sc.get_today_stat();
    assert_eq!(today.total(), 5);
    assert_eq!(today.commit_count, 1);
    assert_eq!(today.code_len_sum, 2);
    assert_eq!(today.code_len_count, 1);
    assert_eq!(today.cand_pos_dist[0], 1);
    assert_eq!(today.by_source[CommitSource::Candidate.index()], 5);
    let ss = today.by_schema.get("wubi86").expect("schema stats");
    assert_eq!(ss.total_chars, 5);
    assert_eq!(ss.commit_count, 1);
    assert_eq!(sc.get_meta().total_chars, 5);
}

#[test]
fn test_cross_day_flushes_old_and_starts_new() {
    let store = mem_store();
    let sc = StatCollector::new(store.clone(), 1.0);
    {
        let mut inner = sc.shared.inner.lock().unwrap();
        inner.today_date = "2026-01-01".into();
        inner.today.chinese = 100;
        inner.dirty = true;
    }
    sc.record(StatEvent {
        chinese: 3,
        source: CommitSource::Candidate,
        ..Default::default()
    });

    // 旧日落库
    let old = store.get_daily_stat("2026-01-01").unwrap();
    assert_eq!(old.total(), 100);
    // 新日只含新记录
    let today = sc.get_today_stat();
    assert_ne!(sc.shared.inner.lock().unwrap().today_date, "2026-01-01");
    assert_eq!(today.total(), 3);
    // 旧日 flush 时更新了 streak
    assert_eq!(sc.get_meta().streak_last_day, "2026-01-01");
}

#[test]
fn test_active_time_within_threshold() {
    let sc = StatCollector::new(mem_store(), 1.0);
    let base = Local::now();
    sc.record(StatEvent {
        timestamp: base,
        chinese: 1,
        source: CommitSource::Candidate,
        ..Default::default()
    });
    sc.record(StatEvent {
        timestamp: base + chrono::Duration::seconds(10),
        chinese: 1,
        source: CommitSource::Candidate,
        ..Default::default()
    });
    sc.record(StatEvent {
        timestamp: base + chrono::Duration::seconds(100),
        chinese: 1,
        source: CommitSource::Candidate,
        ..Default::default()
    });
    assert_eq!(
        sc.get_today_stat().active_seconds,
        10,
        "仅 10s 间隔计入活跃"
    );
}

/// 亚秒间隔必须累进分母。
///
/// ★ v1 用 `num_seconds()` 向零截断，这 4 次 900ms 的间隔全被记成 0 秒 ⇒ 分母恒 0、
/// 分子照涨，速度被外推。**手越快，被砍掉的比例越大**——偏差方向恒为「显示得更快」，
/// 恰好发生在最想看这个数字的用户身上。
#[test]
fn sub_second_gaps_accumulate_in_millis() {
    let sc = StatCollector::new(mem_store(), 1.0);
    let base = Local::now();
    for i in 0..5 {
        sc.record(StatEvent {
            timestamp: base + chrono::Duration::milliseconds(900 * i),
            chinese: 2,
            code_len: 4,
            source: CommitSource::Candidate,
            ..Default::default()
        });
    }
    let d = sc.get_today_stat();
    assert_eq!(d.active_millis, 3_600, "4 个 900ms 间隔");
    assert_eq!(d.active_seconds, 3, "旧字段由毫秒总量派生");
    assert_eq!(d.total(), 10, "实际字数不受速度模型影响");
}

/// 段首（当天首次 / 发呆超阈值后重开）的字符不进速度分子——它的耗时无从测量。
///
/// ★ 这是 v1 速度虚高的第二个来源：那次上屏的**时间被丢弃而字数被保留**，
/// 分子分母口径不对称。分段越碎（写代码时夹杂中文就是这样），虚高越离谱。
#[test]
fn segment_start_chars_stay_out_of_speed_numerator() {
    let sc = StatCollector::new(mem_store(), 1.0);
    let base = Local::now();
    // 段首：5 字，无前序间隔。
    sc.record(StatEvent {
        timestamp: base,
        chinese: 5,
        code_len: 9,
        source: CommitSource::Candidate,
        ..Default::default()
    });
    // 段内：2 秒后 3 字 —— 这一次才算得出耗时。
    sc.record(StatEvent {
        timestamp: base + chrono::Duration::seconds(2),
        chinese: 3,
        code_len: 9,
        source: CommitSource::Candidate,
        ..Default::default()
    });
    // 发呆 60 秒后重开：又是段首，4 字不进分子。
    sc.record(StatEvent {
        timestamp: base + chrono::Duration::seconds(62),
        chinese: 4,
        code_len: 9,
        source: CommitSource::Candidate,
        ..Default::default()
    });
    let d = sc.get_today_stat();
    assert_eq!(d.total(), 12, "实际字数如实计全部 12 字");
    assert_eq!(d.speed_chars, 3, "只有能测到耗时的那 3 字进分子");
    assert_eq!(d.active_millis, 2_000);
}

/// 短码出长词按击键数封顶，实际字数不受影响。
#[test]
fn short_code_long_word_caps_speed_numerator_only() {
    let sc = StatCollector::new(mem_store(), 1.0);
    let base = Local::now();
    sc.record(StatEvent {
        timestamp: base,
        chinese: 1,
        code_len: 4,
        keystrokes: 4,
        source: CommitSource::Candidate,
        ..Default::default()
    });
    // 4 键出「中华人民共和国」7 字：速度只认 4。
    sc.record(StatEvent {
        timestamp: base + chrono::Duration::seconds(1),
        chinese: 7,
        code_len: 4,
        keystrokes: 4,
        source: CommitSource::Candidate,
        ..Default::default()
    });
    // 1 键触发快捷指令出 20 字：同样只认 4。
    sc.record(StatEvent {
        timestamp: base + chrono::Duration::seconds(2),
        chinese: 20,
        keystrokes: 1,
        source: CommitSource::Mix,
        ..Default::default()
    });
    // 12 键打出的 12 字整句：不封顶。
    sc.record(StatEvent {
        timestamp: base + chrono::Duration::seconds(3),
        chinese: 12,
        code_len: 12,
        keystrokes: 12,
        source: CommitSource::Candidate,
        ..Default::default()
    });
    let d = sc.get_today_stat();
    assert_eq!(d.total(), 40, "实际字数一个不少");
    assert_eq!(
        d.speed_chars,
        4 + 4 + 12,
        "段首那 1 字不计，其余按封顶后累加"
    );
}

/// 当天中途从 v1 升级：旧半段按旧口径折算入账，不能凭空清零。
#[test]
fn resume_migrates_v1_record_of_the_same_day() {
    let store = mem_store();
    // 模拟旧版本落库的当日数据（无 speed_v2 字段）。
    store
        .put_daily_stat(
            &today_string(),
            &DailyStats {
                chinese: 300,
                active_seconds: 120,
                ..Default::default()
            },
        )
        .unwrap();
    let sc = StatCollector::new(store, 1.0);
    let d = sc.get_today_stat();
    assert!(d.speed_v2);
    assert_eq!(d.speed_chars, 300, "旧半段按旧口径折算");
    assert_eq!(d.active_millis, 120_000);
}

#[test]
fn test_streak_increment_and_reset() {
    let sc = StatCollector::new(mem_store(), 1.0);

    let flush_day = |date: &str| {
        let mut inner = sc.shared.inner.lock().unwrap();
        inner.today_date = date.into();
        inner.today.chinese = 10;
        inner.dirty = true;
        sc.shared.flush_locked(&mut inner);
    };

    flush_day("2026-04-01");
    let m = sc.get_meta();
    assert_eq!(m.streak_current, 1);
    assert_eq!(m.streak_last_day, "2026-04-01");

    flush_day("2026-04-02");
    let m = sc.get_meta();
    assert_eq!(m.streak_current, 2);
    assert_eq!(m.streak_max, 2);

    flush_day("2026-04-04");
    let m = sc.get_meta();
    assert_eq!(m.streak_current, 1, "间隔后重置");
    assert_eq!(m.streak_max, 2, "max 保留");
}

#[test]
fn test_drop_flushes_pending() {
    let store = mem_store();
    {
        let sc = StatCollector::new(store.clone(), 1.0);
        sc.record(StatEvent {
            chinese: 7,
            source: CommitSource::Candidate,
            ..Default::default()
        });
    } // Drop 应 flush
    let today = store.get_daily_stat(&today_string()).unwrap();
    assert_eq!(today.total(), 7, "Drop 时应持久化未落库数据");
}

/// 审查 I-5：还原期间收集器挂起，任何 flush（含后台 30 秒那次）都不落盘；resume 后按库里的值重载。
#[test]
fn suspended_collector_never_flushes() {
    let store = mem_store();
    let sc = StatCollector::new(store.clone(), 1.0);
    sc.suspend();
    sc.record(StatEvent {
        chinese: 100,
        ..Default::default()
    });
    sc.flush();
    assert_eq!(store.get_daily_stat(&today_string()).unwrap().chinese, 0);
    sc.resume();
    sc.flush();
    assert_eq!(store.get_daily_stat(&today_string()).unwrap().chinese, 0);
    sc.record(StatEvent {
        chinese: 1,
        ..Default::default()
    });
    sc.flush();
    assert_eq!(
        store.get_daily_stat(&today_string()).unwrap().chinese,
        1,
        "resume 后照常"
    );
}
