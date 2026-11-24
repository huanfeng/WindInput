use super::*;

fn up(ud: &UserData, code: &str, text: &str) {
    ud.apply(
        Target::Temp,
        "py",
        OpKind::Upsert,
        code,
        text,
        Payload::Weight(1),
        0,
    )
    .unwrap();
}

/// §8：日志写失败（磁盘满、目录只读）记下原因供设置页提示；恢复后清掉。
#[test]
fn journal_write_failure_is_recorded_and_cleared() {
    let (d, dirs) = dirs_for("wfail");
    let ud = UserData::open(dirs).unwrap();
    assert!(ud.write_error().is_none());
    crate::journal::FAIL_NEXT_APPEND.with(|f| f.set(true));
    assert!(
        ud.apply(
            Target::Temp,
            "py",
            OpKind::Upsert,
            "a",
            "甲",
            Payload::Weight(1),
            0
        )
        .is_err()
    );
    let e = ud.write_error().expect("写失败要记下");
    assert!(!e.contains("甲"), "不记内容：{e}");
    up(&ud, "b", "乙");
    assert!(ud.write_error().is_none(), "恢复后清掉");
    drop(ud);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn sync_journal_reports_whether_it_synced() {
    let (d, dirs) = dirs_for("syncj");
    let ud = UserData::open(dirs).unwrap();
    assert!(!ud.sync_journal().unwrap());
    up(&ud, "a", "啊");
    assert!(ud.sync_journal().unwrap());
    assert!(!ud.sync_journal().unwrap());
    drop(ud);
    let _ = std::fs::remove_dir_all(&d);
}

/// 折回写到一半崩溃（新 wdat 已写、sidecar 未写）：重开后已折回的与未折回的都在。
#[test]
fn crash_mid_fold_loses_nothing() {
    let d = std::env::temp_dir().join(format!("wind_ud_halffold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let dirs = || Dirs {
        state_dir: d.clone(),
        dir_id: "dir".into(),
    };
    let ud = UserData::open(dirs()).unwrap();
    for (c, t) in [("a", "甲"), ("b", "乙"), ("c", "丙")] {
        up(&ud, c, t);
    }
    ud.fold(Target::Temp, "py").unwrap();
    up(&ud, "d", "丁");
    crate::snapshot::FAIL_BEFORE_META.with(|f| f.set(true));
    assert!(ud.fold(Target::Temp, "py").is_err());
    drop(ud);
    let ud = UserData::open(dirs()).unwrap();
    for (c, t) in [("a", "甲"), ("b", "乙"), ("c", "丙"), ("d", "丁")] {
        assert_eq!(
            ud.query_exact(Target::Temp, "py", c),
            vec![(t.to_string(), 1)],
            "{c}"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}
fn dirs_for(name: &str) -> (std::path::PathBuf, Dirs) {
    let d = std::env::temp_dir().join(format!("wind_ud_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    (
        d.clone(),
        Dirs {
            state_dir: d,
            dir_id: "dir".into(),
        },
    )
}

/// M3：created_at 是写入时刻，折回不改它；Upsert 刷新 last_used；Incr 对不存在的键新建条目。
#[test]
fn timestamps_come_from_the_op_not_the_fold() {
    let (d, dirs) = dirs_for("ts");
    let ud = UserData::open(dirs).unwrap();
    let mut a = Op::new(
        Target::Temp,
        "py",
        OpKind::Upsert,
        "a",
        "甲",
        Payload::Weight(5),
        0,
    );
    a.ts_ms = 5_000_000;
    let mut a2 = Op::new(
        Target::Temp,
        "py",
        OpKind::Upsert,
        "a",
        "甲",
        Payload::Weight(6),
        0,
    );
    a2.ts_ms = 9_000_000;
    let mut f = Op::new(
        Target::Freq,
        "py",
        OpKind::Incr,
        "b",
        "乙",
        Payload::Count(2),
        0,
    );
    f.ts_ms = 7_000_000;
    ud.apply_batch(&[a, a2, f]).unwrap();
    ud.fold(Target::Temp, "py").unwrap();
    ud.fold(Target::Freq, "py").unwrap();
    let e = ud.get(Target::Temp, "py", "a", "甲").unwrap();
    assert_eq!((e.weight, e.created_at, e.last_used), (6, 5_000, 9_000));
    let f = ud
        .get(Target::Freq, "py", "b", "乙")
        .expect("Incr 对不存在的键新建");
    assert_eq!((f.count, f.created_at, f.last_used), (2, 7_000, 7_000));
    let _ = std::fs::remove_dir_all(&d);
}

/// M5：没有增量语义的记录不落日志、不占号。
#[test]
fn op_without_delta_semantics_is_refused_before_append() {
    let (d, dirs) = dirs_for("m5");
    let ud = UserData::open(dirs).unwrap();
    let bad = Op::new(
        Target::Temp,
        "py",
        OpKind::BatchMarker,
        "a",
        "甲",
        Payload::None,
        0,
    );
    assert!(ud.apply_batch(&[bad]).is_err());
    let seq = ud
        .apply(
            Target::Temp,
            "py",
            OpKind::Upsert,
            "a",
            "甲",
            Payload::Weight(1),
            0,
        )
        .unwrap();
    assert_eq!(seq, 1, "被拒的记录不占号");
    let _ = std::fs::remove_dir_all(&d);
}
