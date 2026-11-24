use super::*;

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn freq_jsonl_roundtrip_merge_max() {
    let path = tmp("wind_freq_io.redb");
    let s = Store::open(&path).unwrap();
    s.record_freq("wb", "a", "工").unwrap();
    s.record_freq("wb", "a", "工").unwrap(); // count=2
    let text = s.export_freq_jsonl("wb").unwrap();
    assert!(text.contains("\"count\":2"));

    // 导入到已有更高 count 的库：Merge 取 max，不回退
    let path2 = tmp("wind_freq_io2.redb");
    let s2 = Store::open(&path2).unwrap();
    for _ in 0..5 {
        s2.record_freq("wb", "a", "工").unwrap(); // count=5
    }
    let (imported, skipped) = s2.import_freq_jsonl("wb", &text).unwrap();
    assert_eq!((imported, skipped), (1, 0));
    assert_eq!(
        s2.get_freq("wb", "a", "工").unwrap().unwrap().count,
        5,
        "max 合并不回退"
    );

    // 导入到空库：原值落库
    let path3 = tmp("wind_freq_io3.redb");
    let s3 = Store::open(&path3).unwrap();
    s3.import_freq_jsonl("wb", &text).unwrap();
    assert_eq!(s3.get_freq("wb", "a", "工").unwrap().unwrap().count, 2);
    // 坏行跳过
    let (_, sk) = s3.import_freq_jsonl("wb", "not json\n").unwrap();
    assert_eq!(sk, 1);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
    let _ = std::fs::remove_file(&path3);
}

/// 批量查询必须与逐个 `get_freq` **逐项等价**，且保持入参顺序。
///
/// 改批量是为了性能（五笔单字母 78+ 候选 = 78 次读事务），但顺序或缺失项对不齐就会让
/// 词频张冠李戴——调用方是按下标 zip 回候选文本的。
#[test]
fn freq_batch_matches_individual_lookups() {
    let path = tmp("wind_freq_batch.redb");
    let s = Store::open(&path).unwrap();
    s.record_freq("wb", "de", "有").unwrap();
    s.record_freq("wb", "def", "有").unwrap();
    s.record_freq("wb", "def", "有").unwrap(); // count=2
    s.record_freq("py", "de", "的").unwrap();

    // 含命中、未命中、跨 schema、重复项，且顺序刻意打乱
    let keys: Vec<(String, String, String)> = [
        ("wb", "def", "有"),
        ("wb", "zzz", "无"),
        ("py", "de", "的"),
        ("wb", "de", "有"),
        ("py", "de", "有"), // 同 code 不同 schema：不得串
    ]
    .iter()
    .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
    .collect();

    let batch = s.get_freq_batch(&keys).unwrap();
    assert_eq!(batch.len(), keys.len(), "必须逐项对齐，缺一不可");
    for (i, (sc, code, text)) in keys.iter().enumerate() {
        let one = s.get_freq(sc, code, text).unwrap();
        assert_eq!(
            batch[i].map(|r| r.count),
            one.map(|r| r.count),
            "第 {i} 项 ({sc},{code},{text}) 批量与逐个结果不一致"
        );
    }
    // 钉住具体值，防止「两边同时错」
    assert_eq!(batch[0].unwrap().count, 2);
    assert!(batch[1].is_none(), "未命中项必须留 None 占位，不能被跳过");
    assert_eq!(batch[2].unwrap().count, 1);
    assert_eq!(batch[3].unwrap().count, 1);
    assert!(batch[4].is_none(), "schema 隔离：py 下没有「有」");

    assert!(
        s.get_freq_batch(&[]).unwrap().is_empty(),
        "空入参不得开事务"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_redb_record_and_get_freq() {
    let path = tmp("wind_freq_redb.redb");
    let s = Store::open(&path).unwrap();
    assert!(s.get_freq("wb", "a", "工").unwrap().is_none());
    s.record_freq("wb", "a", "工").unwrap();
    s.record_freq("wb", "a", "工").unwrap();
    let r = s.get_freq("wb", "a", "工").unwrap().unwrap();
    assert_eq!(r.count, 2);
    assert!(r.last_used > 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_freq_list_delete_clear() {
    let path = tmp("wind_freq_listops.redb");
    let s = Store::open(&path).unwrap();
    s.record_freq("py", "de", "的").unwrap();
    s.record_freq("py", "shi", "是").unwrap();
    s.record_freq("py", "shi", "是").unwrap();
    s.record_freq("wb", "a", "工").unwrap(); // 另一方案隔离

    let (page, total) = s.list_freq_paged("py", "", 0, 50).unwrap();
    assert_eq!(total, 2);
    assert_eq!(page.len(), 2);
    // 前缀过滤
    let (page2, total2) = s.list_freq_paged("py", "sh", 0, 50).unwrap();
    assert_eq!(total2, 1);
    assert_eq!(page2[0].1, "是");
    assert_eq!(page2[0].2.count, 2);
    // 分页
    let (page3, _) = s.list_freq_paged("py", "", 1, 1).unwrap();
    assert_eq!(page3.len(), 1);

    // 删除
    s.delete_freq("py", "de", "的").unwrap();
    assert_eq!(s.list_freq_paged("py", "", 0, 0).unwrap().1, 1);
    // 清空（按方案隔离，wb 不受影响）
    assert_eq!(s.clear_freq("py").unwrap(), 1);
    assert_eq!(s.list_freq_paged("py", "", 0, 0).unwrap().1, 0);
    assert_eq!(s.list_freq_paged("wb", "", 0, 0).unwrap().1, 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn recency_peak_boosts_fresh_record() {
    let base = FreqProfile {
        base_scale: 100.0,
        half_life_hours: 72.0,
        recency_peak: 0.0,
    };
    let peaked = FreqProfile {
        recency_peak: 500.0,
        ..base
    };
    let now = 1_000_000i64;
    let fresh = FreqRecord {
        count: 1,
        last_used: now,
    };
    let diff = peaked.pinyin_score(&fresh, now) - base.pinyin_score(&fresh, now);
    assert!(
        (diff - 500.0).abs() < 1e-6,
        "刚用过的记录应获得完整峰值加成，got {diff}"
    );

    let stale = FreqRecord {
        count: 1,
        last_used: now - 30 * 24 * 3600,
    };
    let stale_diff = peaked.pinyin_score(&stale, now) - base.pinyin_score(&stale, now);
    assert!(stale_diff < 1.0, "峰值加成应随衰减消退，got {stale_diff}");
}

#[test]
fn test_pinyin_decay_score() {
    let p = FreqProfile::default();
    let now = 1_000_000_000i64;
    let fresh = FreqRecord {
        count: 9,
        last_used: now,
    };
    let old = FreqRecord {
        count: 9,
        last_used: now - 72 * 3600,
    }; // 一个半衰期前
    let s_fresh = p.pinyin_score(&fresh, now);
    let s_old = p.pinyin_score(&old, now);
    assert!(s_fresh > 0.0);
    // 一个半衰期 → 约半衰
    assert!((s_old / s_fresh - 0.5).abs() < 0.05, "半衰期处应≈半衰");
    assert_eq!(
        p.pinyin_score(
            &FreqRecord {
                count: 0,
                last_used: now
            },
            now
        ),
        0.0
    );
}

#[test]
fn test_freq_tracker_save_load_roundtrip() {
    let tmp = tmp("wind_freq_legacy.tsv");
    let a = FreqTracker::new();
    a.record_selection("你好");
    a.record_selection("你好");
    a.save_to_file(&tmp).unwrap();
    let b = FreqTracker::new();
    b.load_from_file(&tmp).unwrap();
    assert_eq!(b.get_count("你好"), 2);
    assert!(b.get_boost("你好") > 0.0);
    let _ = std::fs::remove_file(&tmp);
}
/// §10 退出判据：词频记录 / 批量查、联想、草稿读写都不经过 redb Mutex。
#[test]
fn draft_freq_assoc_hot_paths_do_not_touch_redb() {
    let p = std::env::temp_dir().join(format!("wind_freq_hot-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let s = std::sync::Arc::new(Store::open(&p).unwrap());
    s.pause().unwrap();
    s.record_freq("py", "nihao", "你好").unwrap();
    let got = s
        .get_freq_batch(&[("py".into(), "nihao".into(), "你好".into())])
        .unwrap();
    assert_eq!(got[0].map(|r| r.count), Some(1));
    s.record_assoc_pick("py", "天", "气").unwrap();
    assert_eq!(s.assoc_history("py", "天", 0).unwrap().len(), 1);
    s.add_drafts("wb", &[("ggtt".into(), "盖伦".into())])
        .unwrap();
    assert_eq!(s.search_drafts("wb", "ggtt", 86_400).unwrap(), vec!["盖伦"]);
    use wind_dict::DictLayer;
    let l = crate::StoreDraftLayer::new(s.clone(), "wb", 86_400);
    assert_eq!(l.search("ggtt", 0).len(), 1);
}
