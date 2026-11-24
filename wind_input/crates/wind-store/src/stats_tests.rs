use super::*;

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn stats_jsonl_roundtrip_skip_existing() {
    let path = tmp("wind_st_io.redb");
    let s = Store::open(&path).unwrap();
    let d = DailyStats {
        chinese: 42,
        ..Default::default()
    };
    s.put_daily_stat("2026-07-01", &d).unwrap();
    let text = s.export_stats_jsonl().unwrap();
    assert!(text.contains("2026-07-01"));

    let path2 = tmp("wind_st_io2.redb");
    let s2 = Store::open(&path2).unwrap();
    let local = DailyStats {
        chinese: 7,
        ..Default::default()
    };
    s2.put_daily_stat("2026-07-01", &local).unwrap();
    // overwrite=false：已存在日跳过
    let (imp, _) = s2.import_stats_jsonl(&text, false).unwrap();
    assert_eq!(imp, 0);
    assert_eq!(s2.get_daily_stat("2026-07-01").unwrap().chinese, 7);
    // overwrite=true：覆盖
    let (imp2, _) = s2.import_stats_jsonl(&text, true).unwrap();
    assert_eq!(imp2, 1);
    assert_eq!(s2.get_daily_stat("2026-07-01").unwrap().chinese, 42);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

#[test]
fn test_record_and_daily() {
    let path = tmp("wind_stats_daily.redb");
    let s = Store::open(&path).unwrap();
    s.record_stat("2026-06-18", 10, 2).unwrap();
    s.record_stat("2026-06-18", 5, 0).unwrap(); // 累加
    s.record_stat("2026-06-20", 3, 7).unwrap();

    assert_eq!(s.get_daily_stat("2026-06-18").unwrap().total(), 17);
    let range = s.daily_stats("2026-06-18", "2026-06-20").unwrap();
    assert_eq!(range.len(), 2);
    assert_eq!(range[0].0, "2026-06-18");
    assert_eq!(range[1].1.total(), 10);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_summary_and_streak() {
    let path = tmp("wind_stats_summary.redb");
    let s = Store::open(&path).unwrap();
    s.record_stat("2026-06-20", 100, 0).unwrap(); // today
    s.record_stat("2026-06-19", 50, 0).unwrap();
    s.record_stat("2026-06-18", 30, 0).unwrap();
    s.record_stat("2026-06-01", 5, 0).unwrap();

    let sum = s.stats_summary("2026-06-20").unwrap();
    assert_eq!(sum.today, 100);
    assert_eq!(sum.week, 180, "近7日=100+50+30");
    assert_eq!(sum.month, 185, "近30日含06-01");
    assert_eq!(sum.total, 185);
    assert_eq!(sum.streak, 3, "06-18~06-20 连续");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_classify_chars() {
    let (zh, en) = classify_chars("你好abc，123");
    assert_eq!(zh, 2);
    assert_eq!(en, 3);
}

// ── Stage 1 新增 ──

#[test]
fn test_speed_per_minute_ms() {
    assert_eq!(speed_per_minute_ms(252, 6_000, 1.0), 2520);
    assert_eq!(speed_per_minute_ms(252, 120_000, 1.0), 126);
    assert_eq!(
        speed_per_minute_ms(60, 3_000, 1.0),
        720,
        "极短时间触发 5s 下限"
    );
    assert_eq!(speed_per_minute_ms(0, 60_000, 1.0), 0);
    assert_eq!(speed_per_minute_ms(100, 0, 1.0), 0);
    // 亚秒间隔不得截断成 0（`num_seconds()` 会把 900ms 记成 0 秒），否则分母消失、速度被外推偏高。
    assert_eq!(speed_per_minute_ms(2, 900, 1.0), 24, "分母走 5s 下限");
}

/// 修正系数直接乘在结果上，且写坏的值不得把速度清零。
#[test]
fn speed_factor_scales_result_and_tolerates_garbage() {
    assert_eq!(speed_per_minute_ms(300, 60_000, 1.0), 300);
    assert_eq!(speed_per_minute_ms(300, 60_000, 0.85), 255);
    for bad in [0.0f32, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            speed_per_minute_ms(300, 60_000, bad),
            300,
            "非法系数 {bad} 应按 1.0 处理而非清零"
        );
    }
}

/// 短码出长词/一键出一串：速度分子封顶，实际字数不受影响（那由调用方另计）。
#[test]
fn speed_chars_capped_only_for_short_keystrokes() {
    // 4 键出 7 字（「中华人民共和国」）→ 只计 4。
    assert_eq!(speed_chars_of(7, 4), 4);
    // 1 键触发快捷指令出 30 字 → 只计 4。
    assert_eq!(speed_chars_of(30, 1), 4);
    // 击键数超过阈值 → 不封顶（正常整句输入不该被削）。
    assert_eq!(speed_chars_of(12, 5), 12);
    // 封顶只取下限，不会把短内容拔高。
    assert_eq!(speed_chars_of(1, 1), 1);
    assert_eq!(speed_chars_of(2, 3), 2);
    // 击键数未知（0）一律不封顶：宁可漏封也不误伤 TSF 英文那种批量 1:1 上报。
    assert_eq!(speed_chars_of(30, 0), 30);
}

/// v1 记录没有速度专用量，须回退到旧口径而不是当成「0 字符」。
#[test]
fn speed_parts_falls_back_for_v1_records() {
    let v1 = DailyStats {
        chinese: 240,
        active_seconds: 120,
        ..Default::default()
    };
    assert_eq!(v1.speed_parts(), (240, 120_000));

    // v2 记录即使分子为 0（当天全是段首上屏）也不能回退——那会读成 240。
    let v2 = DailyStats {
        chinese: 240,
        active_seconds: 120,
        speed_v2: true,
        speed_chars: 0,
        active_millis: 0,
        ..Default::default()
    };
    assert_eq!(v2.speed_parts(), (0, 0));

    let v2b = DailyStats {
        chinese: 240,
        speed_v2: true,
        speed_chars: 180,
        active_millis: 90_500,
        ..Default::default()
    };
    assert_eq!(v2b.speed_parts(), (180, 90_500));
}

/// 小样本不得刷新「历史最快」：`speed_per_minute` 在分母小时给的是外推值
/// （252 字 / 6 秒 → 2520 字每分），展示当日速度无妨，写进永久的 max_speed 就是污染。
#[test]
fn small_samples_never_qualify_for_max_speed() {
    // 典型污染场景：当天开头，第一次上屏的字不计时间，第二次上屏才攒出几秒。
    assert!(
        !qualifies_for_max_speed(125, 5_000),
        "125字/5秒 外推 1500，须挡掉"
    );
    assert!(!qualifies_for_max_speed(252, 6_000));
    // 时长够但字数太少 → 仍不算（几十个字的偶发爆发不代表稳定速度）。
    assert!(!qualifies_for_max_speed(50, 300_000));
    // 字数够但时长太短 → 不算。
    assert!(!qualifies_for_max_speed(5000, 30_000));
    // 两者都够 → 计入。
    assert!(qualifies_for_max_speed(100, 60_000));
    assert!(qualifies_for_max_speed(6000, 600_000));
}

#[test]
fn test_classify_chars_full() {
    let (zh, en, pu, ot) = classify_chars_full("你好abc，123");
    assert_eq!(zh, 2);
    assert_eq!(en, 3);
    assert_eq!(pu, 1, "，为全角标点");
    assert_eq!(ot, 3, "123 计入其他");
}

#[test]
fn test_classify_fullwidth_digits_as_other() {
    // 全角数字 U+FF10-FF19 对齐 Go：unicode.IsPunct/IsSymbol 均为 false → other
    let (zh, en, pu, ot) = classify_chars_full("０１２３４５６７８９");
    assert_eq!(pu, 0, "全角数字不应计为标点");
    assert_eq!(ot, 10, "全角数字应计为其他");
    assert_eq!(zh, 0);
    assert_eq!(en, 0);
}

#[test]
fn test_classify_fullwidth_letters_as_other() {
    // 全角字母 U+FF21-FF3A/FF41-FF5A 对齐 Go：非 ASCII alpha、非 IsPunct → other
    let (zh, en, pu, ot) = classify_chars_full("ＡＢＣａｂｃ");
    assert_eq!(pu, 0, "全角字母不应计为标点");
    assert_eq!(ot, 6, "全角字母应计为其他");
    assert_eq!(zh, 0);
    assert_eq!(en, 0);
}

#[test]
fn test_daily_stats_backward_compat() {
    // 旧库只含 {chinese, english}，新字段应回落默认值。
    let rec: DailyStats = serde_json::from_str(r#"{"chinese":10,"english":2}"#).unwrap();
    assert_eq!(rec.chinese, 10);
    assert_eq!(rec.english, 2);
    assert_eq!(rec.total(), 12);
    assert_eq!(rec.hours, [0u32; 24]);
    assert_eq!(rec.active_seconds, 0);
    assert!(rec.by_schema.is_empty());
}

#[test]
fn test_put_get_daily_full() {
    let path = tmp("wind_stats_put_full.redb");
    let s = Store::open(&path).unwrap();
    let mut stat = DailyStats {
        chinese: 50,
        english: 10,
        punct: 5,
        other: 3,
        commit_count: 20,
        code_len_sum: 80,
        code_len_count: 20,
        active_seconds: 300,
        ..Default::default()
    };
    stat.hours[9] = 30;
    stat.code_len_dist[3] = 12;
    stat.cand_pos_dist[0] = 18;
    stat.by_source = vec![0; CommitSource::COUNT];
    stat.by_source[CommitSource::Candidate.index()] = 60;
    stat.by_schema.insert(
        "wubi86".into(),
        SchemaStats {
            total_chars: 60,
            commit_count: 20,
            ..Default::default()
        },
    );

    s.put_daily_stat("2026-06-20", &stat).unwrap();
    let got = s.get_daily_stat("2026-06-20").unwrap();
    assert_eq!(got, stat, "完整字段往返一致");
    assert_eq!(got.total(), 68);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_meta_put_get() {
    let path = tmp("wind_stats_meta.redb");
    let s = Store::open(&path).unwrap();
    // 默认空
    assert_eq!(s.get_stats_meta().unwrap(), StatsMeta::default());
    let meta = StatsMeta {
        total_chars: 12345,
        first_day: "2026-01-01".into(),
        streak_current: 7,
        streak_max: 30,
        streak_last_day: "2026-06-20".into(),
        max_speed: 420,
    };
    s.put_stats_meta(&meta).unwrap();
    assert_eq!(s.get_stats_meta().unwrap(), meta);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_clear_resets_meta() {
    let path = tmp("wind_stats_clear_meta.redb");
    let s = Store::open(&path).unwrap();
    s.record_stat("2026-06-10", 1, 0).unwrap();
    s.record_stat("2026-06-20", 1, 0).unwrap();
    s.put_stats_meta(&StatsMeta {
        total_chars: 2,
        ..Default::default()
    })
    .unwrap();

    assert_eq!(s.clear_stats().unwrap(), 2);
    assert_eq!(s.daily_stats("2026-01-01", "2026-12-31").unwrap().len(), 0);
    assert_eq!(
        s.get_stats_meta().unwrap(),
        StatsMeta::default(),
        "clear 应同时重置元数据"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_recalculate_stats_meta_after_prune() {
    let path = tmp("wind_stats_recalc.redb");
    let s = Store::open(&path).unwrap();
    for (date, total, active) in [
        ("2026-04-20", 100u32, 60u32),
        ("2026-04-21", 200, 120),
        ("2026-04-23", 300, 60),
    ] {
        let stat = DailyStats {
            chinese: total,
            active_seconds: active,
            ..Default::default()
        };
        s.put_daily_stat(date, &stat).unwrap();
    }

    assert_eq!(s.prune_stats_before("2026-04-21").unwrap(), 1);
    let meta = s.recalculate_stats_meta(1.0).unwrap();
    assert_eq!(meta.total_chars, 500);
    assert_eq!(meta.first_day, "2026-04-21");
    assert_eq!(meta.streak_current, 1);
    assert_eq!(meta.streak_max, 1);
    assert_eq!(meta.streak_last_day, "2026-04-23");
    assert_eq!(meta.max_speed, 300, "300字/60s = 300字/分");
    let _ = std::fs::remove_file(&path);
}

/// 重算时同样要挡掉小样本，且它是修正历史污染值的入口：虚高的 max_speed 重算一次即回落。
#[test]
fn recalculate_ignores_small_samples_for_max_speed() {
    let path = std::env::temp_dir().join("wind_recalc_speed_test.redb");
    let _ = std::fs::remove_file(&path);
    let s = Store::open(&path).unwrap();

    // 一天正常（240字/120秒 = 120字/分），一天是"开头窗口"式的小样本
    // （125字/5秒，外推 1500 字/分）。
    for (date, total, active) in [("2026-04-21", 240u32, 120u32), ("2026-04-22", 125, 5)] {
        let stat = DailyStats {
            chinese: total,
            active_seconds: active,
            ..Default::default()
        };
        s.put_daily_stat(date, &stat).unwrap();
    }
    // 先人为写入被污染的历史值，模拟旧版本留下的数据。
    s.put_stats_meta(&StatsMeta {
        max_speed: 1500,
        ..Default::default()
    })
    .unwrap();

    let meta = s.recalculate_stats_meta(1.0).unwrap();
    assert_eq!(
        meta.max_speed, 120,
        "小样本那天应被忽略，且旧的污染值 1500 被重算覆盖"
    );
    let _ = std::fs::remove_file(&path);
}

/// 每日统计按月落用户数据目录的 TOML；prune 整月删文件；meta 单独一个文件。
#[test]
fn daily_stats_live_in_monthly_data_files() {
    let s = Store::open(tmp("wind_stats_files.redb")).unwrap();
    let d = DailyStats {
        chinese: 12,
        ..Default::default()
    };
    s.put_daily_stat("2026-10-14", &d).unwrap();
    s.put_daily_stat("2026-09-30", &d).unwrap();
    assert!(s.stats_dir().join("2026-10.toml").is_file());
    assert!(s.stats_dir().join("2026-09.toml").is_file());
    assert_eq!(s.daily_stats("2026-09-01", "2026-10-31").unwrap().len(), 2);
    assert_eq!(s.get_daily_stat("2026-10-14").unwrap().chinese, 12);
    assert_eq!(s.prune_stats_before("2026-10-01").unwrap(), 1);
    assert!(!s.stats_dir().join("2026-09.toml").exists());
    s.put_stats_meta(&StatsMeta {
        total_chars: 99,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(s.get_stats_meta().unwrap().total_chars, 99);
    s.pause().unwrap();
    assert_eq!(
        s.get_daily_stat("2026-10-14").unwrap().chinese,
        12,
        "不碰 redb"
    );
    s.resume().unwrap();
    assert_eq!(s.clear_stats().unwrap(), 1);
    assert_eq!(s.get_stats_meta().unwrap().total_chars, 0);
}

fn sample_day() -> DailyStats {
    let mut d = DailyStats {
        chinese: 100,
        commit_count: 30,
        active_millis: 60_000,
        speed_v2: true,
        by_source: vec![1, 2],
        ..Default::default()
    };
    d.hours[9] = 100;
    d.code_len_dist[1] = 4;
    d.by_schema.insert(
        "pinyin".into(),
        SchemaStats {
            total_chars: 100,
            ..Default::default()
        },
    );
    d
}

fn corrupt_names(dir: &std::path::Path, prefix: &str) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .count()
}

#[test]
fn month_file_is_toml_in_data_dir_and_round_trips() {
    let s = Store::open(tmp("wind_stats_toml.redb")).unwrap();
    s.put_daily_stat("2026-10-15", &sample_day()).unwrap();
    assert!(s.stats_dir().starts_with(s.user_text_dir()));
    let p = s.stats_dir().join("2026-10.toml");
    let v: toml::Value = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert!(v.get("days").is_some());
    let got = s.get_daily_stat("2026-10-15").unwrap();
    assert_eq!(got.hours[9], 100);
    assert_eq!(got.by_schema["pinyin"].total_chars, 100);
    assert_eq!(got.by_source, vec![1, 2]);
    s.put_stats_meta(&StatsMeta {
        total_chars: 5,
        ..Default::default()
    })
    .unwrap();
    assert!(s.stats_dir().join("meta.toml").is_file());
}

#[test]
fn corrupt_month_is_rebuilt_and_writes_keep_working() {
    let s = Store::open(tmp("wind_stats_corrupt_month.redb")).unwrap();
    let p = s.stats_dir().join("2026-10.toml");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "days = [[[").unwrap();
    s.put_daily_stat("2026-10-15", &sample_day()).unwrap();
    assert_eq!(s.get_daily_stat("2026-10-15").unwrap().chinese, 100);
    assert_eq!(
        corrupt_names(
            &s.user_text_dir().join(".backup/stats"),
            "2026-10.toml.corrupt-"
        ),
        1
    );
    wind_userdata::issues::clear(&p);
}

/// 元数据坏了且没有可用备份 ⇒ 由各月数据重算，不当成全 0 写回。
#[test]
fn corrupt_stats_meta_is_recalculated_not_zeroed() {
    let s = Store::open(tmp("wind_stats_meta_recalc.redb")).unwrap();
    s.put_daily_stat("2026-10-14", &sample_day()).unwrap();
    let before = s.recalculate_stats_meta(1.0).unwrap();
    assert!(before.total_chars > 0);
    let mp = s.stats_dir().join("meta.toml");
    std::fs::write(&mp, "total_chars = \"x").unwrap();
    let _ = std::fs::remove_file(wind_userdata::text::fold::backup_path(&mp));
    let after = s.stats_meta_or_recalc(1.0).unwrap();
    assert_eq!(after.total_chars, before.total_chars);
    assert_eq!(
        corrupt_names(
            &s.user_text_dir().join(".backup/stats"),
            "meta.toml.corrupt-"
        ),
        1
    );
    wind_userdata::issues::clear(&mp);
}

#[test]
fn missing_meta_with_month_data_is_recalculated() {
    let s = Store::open(tmp("wind_stats_meta_missing.redb")).unwrap();
    s.put_daily_stat("2026-10-14", &sample_day()).unwrap();
    let _ = std::fs::remove_file(s.stats_dir().join("meta.toml"));
    assert_eq!(s.stats_meta_or_recalc(1.0).unwrap().total_chars, 100);
}

#[test]
fn range_query_skips_unreadable_month() {
    let s = Store::open(tmp("wind_stats_range_skip.redb")).unwrap();
    s.put_daily_stat("2026-09-01", &sample_day()).unwrap();
    s.put_daily_stat("2026-10-01", &sample_day()).unwrap();
    let p = s.stats_dir().join("2026-09.toml");
    std::fs::write(&p, "days = [[[").unwrap();
    let _ = std::fs::remove_file(wind_userdata::text::fold::backup_path(&p));
    let v = s.daily_stats("2026-09-01", "2026-10-31").unwrap();
    assert!(v.iter().any(|(d, _)| d == "2026-10-01"));
    wind_userdata::issues::clear(&p);
}

/// 非 ASCII / 畸形日期不得 panic（release 下 panic = abort 会带走整个服务）。
#[test]
fn malformed_dates_never_panic() {
    let s = Store::open(tmp("wind_stats_bad_dates.redb")).unwrap();
    let line = "{\"date\":\"２０２６-10-14\",\"stat\":{}}\n{\"date\":\"2026-1\",\"stat\":{}}\n";
    let (n, skipped) = s.import_stats_jsonl(line, true).unwrap();
    assert_eq!((n, skipped), (0, 2));
    assert!(s.daily_stats("２０２６-01-01", "2026-12-31").is_ok());
    assert_eq!(s.get_daily_stat("２０２６-10-14").unwrap().total(), 0);
    assert!(
        s.put_daily_stat("２０２６-10-14", &DailyStats::default())
            .is_err()
    );
    assert!(s.record_stat("x", 1, 0).is_err());
    assert_eq!(s.prune_stats_before("２０２６-01-01").unwrap(), 0);
}
