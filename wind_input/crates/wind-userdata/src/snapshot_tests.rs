use super::*;

fn se(code: &str, text: &str, w: i32, b: u64, count: u32) -> SnapshotEntry {
    SnapshotEntry {
        code: code.into(),
        text: text.into(),
        weight: w,
        boundary: b,
        count,
        created_at: 100,
        last_used: 200,
        flags: 0,
        order: 0,
    }
}

#[test]
fn build_open_query_and_meta_roundtrip() {
    let d = std::env::temp_dir().join(format!("wind_snap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let mut es = vec![
        se("nihao", "你好", 10, 0b101, 3),
        se("ni", "你", 50, 0b1, 9),
        se("nihao", "拟好", 5, 0b101, 1),
        se("wubi", "五笔", 7, 0, 2),
    ];
    es.sort_by(|a, b| (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str())));
    let s = build(&d, "temp", 42, es.into_iter()).unwrap();
    assert_eq!(s.max_seq, 42);
    assert_eq!(s.entry_count, 4);
    drop(s);
    let s = open(&d, "temp").unwrap().expect("应能打开");
    assert_eq!(s.max_seq, 42);
    let hits = s.exact("nihao");
    assert_eq!(
        hits.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
        vec!["你好", "拟好"],
        "同码按权重降序"
    );
    assert_eq!(
        (hits[0].count, hits[0].created_at, hits[0].last_used),
        (3, 100, 200)
    );
    assert_eq!(hits[1].count, 1, "sidecar 与条目一一对齐");
    // 简拼命中的是**候选**：nihao 下两条（你好、拟好）都算
    assert_eq!(s.abbrev("nh", 10).len(), 2, "多音节简拼");
    assert_eq!(s.abbrev("nh", 1).len(), 1, "limit 截断");
    assert_eq!(s.abbrev("\u{1}w", 10).len(), 1, "无边界兜底组");
    assert_eq!(s.prefix("ni", 10).len(), 3);
    let mut seen = Vec::new();
    s.for_each(&mut |e| seen.push((e.code.clone(), e.count)));
    assert_eq!(seen.len(), 4);
    assert!(seen.contains(&("wubi".to_string(), 2)));
    assert!(
        open(&d, "nope").unwrap().is_none(),
        "没有文件 = 零段，不是错误"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// 每条的 sidecar 行按全局序号取：码多、叶长不一时，各查询入口取到的都得是自己那一行。
#[test]
fn meta_rows_line_up_across_codes_of_uneven_leaves() {
    let d = std::env::temp_dir().join(format!("wind_snap_rows-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let mut es = Vec::new();
    for c in 0..40u32 {
        for t in 0..(c % 5 + 1) {
            // count 编码 (码, 词)，权重与词序反向，叶内要重排
            es.push(se(
                &format!("k{c:03}"),
                &format!("w{c}-{t}"),
                t as i32,
                0,
                c * 10 + t,
            ));
        }
    }
    es.sort_by(|a, b| (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str())));
    build(&d, "user", 1, es.into_iter()).unwrap();
    let s = open(&d, "user").unwrap().unwrap();
    let want = |e: &SnapshotEntry| {
        let c: u32 = e.code[1..].parse().unwrap();
        let t: u32 = e.text.rsplit('-').next().unwrap().parse().unwrap();
        c * 10 + t
    };
    for c in [0u32, 7, 23, 39] {
        for e in s.exact(&format!("k{c:03}")) {
            assert_eq!(e.count, want(&e), "exact {}/{}", e.code, e.text);
        }
    }
    for e in s.prefix("k01", 100) {
        assert_eq!(e.count, want(&e), "prefix {}/{}", e.code, e.text);
    }
    let mut n = 0;
    s.for_each_prefix("k02", &mut |e| {
        assert_eq!(e.count, want(&e), "for_each_prefix {}/{}", e.code, e.text);
        n += 1;
        true
    });
    assert!(n > 10);
    let _ = std::fs::remove_dir_all(&d);
}

/// 同码异边界：xian 下「先」(x) 与「西安」(x'a) 两条，两个简拼都要召回。
#[test]
fn abbrev_keys_cover_every_boundary_under_one_code() {
    let d = std::env::temp_dir().join(format!("wind_snap_ab-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let es = vec![
        se("xian", "先", 100, 0b1, 0),
        se("xian", "西安", 50, 0b101, 0),
    ];
    let s = build(&d, "t", 1, es.into_iter()).unwrap();
    assert!(s.abbrev("x", 10).iter().any(|h| h.text == "先"));
    assert!(
        s.abbrev("xa", 10).iter().any(|h| h.text == "西安"),
        "低权重条目的边界也要建键"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// 崩溃落在「新 wdat 已写、新 sidecar 未写」之间：旧段必须原样可读。
#[test]
fn crash_between_wdat_and_meta_keeps_previous_snapshot() {
    let d = std::env::temp_dir().join(format!("wind_snap_half-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let old = vec![
        se("a", "甲", 1, 0, 1),
        se("b", "乙", 1, 0, 1),
        se("c", "丙", 1, 0, 1),
    ];
    drop(build(&d, "t", 3, old.into_iter()).unwrap());
    FAIL_BEFORE_META.with(|f| f.set(true));
    assert!(build(&d, "t", 4, vec![se("d", "丁", 1, 0, 1)].into_iter()).is_err());
    let s = open(&d, "t").unwrap().expect("旧段应仍可打开");
    assert_eq!((s.max_seq, s.entry_count), (3, 3));
    let _ = std::fs::remove_dir_all(&d);
}

/// 旧段还映射着（查询读者仍持有）时重建同名段：Windows 上不能因 sidecar 被映射而写不进。
#[test]
fn rebuild_while_previous_snapshot_is_still_mapped() {
    let d = std::env::temp_dir().join(format!("wind_snap_live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let old = build(&d, "t", 1, vec![se("a", "甲", 1, 0, 1)].into_iter()).unwrap();
    let new = build(&d, "t", 2, vec![se("b", "乙", 1, 0, 1)].into_iter()).unwrap();
    let empty = build(&d, "t", 3, std::iter::empty()).unwrap();
    assert_eq!(old.exact("a").len(), 1, "旧段仍可读");
    assert_eq!(new.exact("b").len(), 1);
    assert_eq!((empty.max_seq, empty.entry_count), (3, 0));
    let s = open(&d, "t").unwrap().expect("最新段可打开");
    assert_eq!((s.max_seq, s.entry_count), (3, 0));
    drop((old, new, empty, s));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn mismatched_sidecar_is_an_error_not_a_silent_zero() {
    let d = std::env::temp_dir().join(format!("wind_snap_bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let s = build(&d, "t", 1, vec![se("a", "甲", 1, 0, 1)].into_iter()).unwrap();
    drop(s);
    std::fs::write(d.join("t.meta"), b"WUDM").unwrap();
    assert!(open(&d, "t").is_err());
    let _ = std::fs::remove_dir_all(&d);
}

/// 零条目也是一个快照：它带着水位，清空之后 state 丢了也不会让旧记录复活。
#[test]
fn empty_build_keeps_watermark_and_drops_old_segment() {
    let d = std::env::temp_dir().join(format!("wind_snap_empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    drop(build(&d, "t", 3, vec![se("a", "甲", 1, 0, 1)].into_iter()).unwrap());
    let s = build(&d, "t", 7, std::iter::empty()).unwrap();
    assert_eq!((s.max_seq, s.entry_count), (7, 0));
    assert!(s.exact("a").is_empty());
    let wdats = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "wdat"))
        .count();
    assert_eq!(wdats, 0, "旧代号文件已清");
    let s = open(&d, "t").unwrap().expect("零条目 sidecar 也能打开");
    assert_eq!(s.max_seq, 7);
    let _ = std::fs::remove_dir_all(&d);
}
#[test]
fn order_survives_build_and_open() {
    let d = std::env::temp_dir().join(format!("wind_snap_order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let mut a = se("ni", "你", 5, 0b1, 0);
    a.order = 7;
    let mut b = se("ni", "泥", 5, 0b1, 0);
    b.order = 3;
    drop(build(&d, "t", 1, vec![a, b].into_iter()).unwrap());
    let s = open(&d, "t").unwrap().unwrap();
    let mut got: Vec<(String, u32)> = s
        .exact("ni")
        .into_iter()
        .map(|e| (e.text, e.order))
        .collect();
    got.sort();
    assert_eq!(got, vec![("你".into(), 7), ("泥".into(), 3)]);
    let _ = std::fs::remove_dir_all(&d);
}
