use super::*;
use crate::record::Target;

fn e(seq: u64, code: &str, text: &str, op: DeltaOp) -> Entry {
    Entry {
        ts_ms: 0,
        seq,
        target: Target::Temp,
        schema: "py".into(),
        code: code.into(),
        text: text.into(),
        op,
        boundary: 0b101,
    }
}

/// Review Focus 3：同键多操作按 seq 顺序保留，不合并。
#[test]
fn same_key_ops_apply_in_seq_order() {
    let d = Delta::default()
        .with(e(
            1,
            "nihao",
            "你好",
            DeltaOp::Upsert {
                weight: 10,
                kind: 0,
            },
        ))
        .with(e(2, "nihao", "你好", DeltaOp::Delete))
        .with(e(
            3,
            "nihao",
            "你好",
            DeltaOp::Upsert {
                weight: 20,
                kind: 1,
            },
        ));
    let ops: Vec<u64> = d
        .ops_for(Target::Temp, "py", "nihao", "你好")
        .iter()
        .map(|x| x.seq)
        .collect();
    assert_eq!(ops, vec![1, 2, 3]);
}

#[test]
fn tail_overflow_freezes_and_queries_still_find_everything() {
    let mut d = Delta::default();
    for i in 0..300u64 {
        d = d.with(e(
            i + 1,
            &format!("ni{i}"),
            "x",
            DeltaOp::Upsert { weight: 1, kind: 0 },
        ));
    }
    assert_eq!(d.len(), 300);
    assert_eq!(d.prefix(Target::Temp, "py", "ni").len(), 300);
    assert_eq!(d.exact(Target::Temp, "py", "ni7").len(), 1);
    // 边界 0b101 下 "ni7" 的分组是 "n7"：ni7 与 ni70..ni79 共 11 条
    assert_eq!(
        d.by_group(Target::Temp, "py", &group_of("ni7", 0b101))
            .len(),
        11
    );
    // 无边界兜底组：按码首字符分组，全部 300 条
    let mut nb = Delta::default();
    for i in 0..300u64 {
        nb = nb.with(Entry {
            boundary: 0,
            ..e(
                i + 1,
                &format!("ni{i}"),
                "x",
                DeltaOp::Upsert { weight: 1, kind: 0 },
            )
        });
    }
    assert_eq!(nb.by_group(Target::Temp, "py", "\u{1}n").len(), 300);
    assert!(
        d.exact(Target::User, "py", "ni7").is_empty(),
        "别的目标看不到"
    );
}

#[test]
fn drop_folded_keeps_newer_and_other_targets_and_schemas() {
    let d = Delta::default()
        .with(e(1, "a", "x", DeltaOp::Delete))
        .with(Entry {
            target: Target::Freq,
            ..e(2, "a", "x", DeltaOp::Incr(1))
        })
        .with(Entry {
            schema: "wubi".into(),
            ..e(3, "a", "x", DeltaOp::Delete)
        })
        .with(e(4, "b", "y", DeltaOp::Delete));
    let d = d.drop_folded_for_schema(Target::Temp, "py", 3);
    assert_eq!(
        d.ops_for(Target::Temp, "py", "b", "y")
            .iter()
            .map(|x| x.seq)
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert!(d.ops_for(Target::Temp, "py", "a", "x").is_empty());
    assert_eq!(d.len(), 3, "别的目标与别的 schema 不动");
}

#[test]
fn group_of_matches_store_rule() {
    assert_eq!(group_of("nihao", 0b101), "nh");
    assert_eq!(group_of("nihao", 0), "\u{1}n");
    assert_eq!(group_of("", 0), "");
}

fn many() -> Delta {
    let mut d = Delta::default();
    let mut seq = 0;
    for i in 0..600u32 {
        seq += 1;
        let t = if i % 3 == 0 {
            Target::Freq
        } else {
            Target::Temp
        };
        let mut x = e(
            seq,
            &format!("c{}", i % 37),
            &format!("t{}", i % 11),
            DeltaOp::Incr(1),
        );
        x.target = t;
        d = d.with(x);
    }
    d
}

/// M4 的特征测试：二分版 ops_for 与线性过滤结果逐条相同（冻结层 + 尾巴都覆盖到）。
#[test]
fn ops_for_matches_linear_filter() {
    let d = many();
    for c in 0..37 {
        for t in 0..11 {
            let (code, text) = (format!("c{c}"), format!("t{t}"));
            let mut want: Vec<u64> = d
                .all()
                .filter(|e| {
                    e.target == Target::Temp && e.schema == "py" && e.code == code && e.text == text
                })
                .map(|e| e.seq)
                .collect();
            want.sort();
            let got: Vec<u64> = d
                .ops_for(Target::Temp, "py", &code, &text)
                .iter()
                .map(|e| e.seq)
                .collect();
            assert_eq!(got, want, "{code}/{text}");
        }
    }
}

#[test]
fn pending_targets_and_min_seq() {
    let d = many();
    assert_eq!(d.pending(Target::Freq, "py"), 200);
    assert_eq!(d.pending(Target::Temp, "py"), 400);
    assert_eq!(d.pending(Target::Temp, "other"), 0);
    let ts: Vec<(Target, String)> = d.targets().into_iter().collect();
    assert_eq!(
        ts,
        vec![(Target::Temp, "py".into()), (Target::Freq, "py".into())]
    );
    assert_eq!(d.min_seq(), Some(1));
    assert_eq!(Delta::default().min_seq(), None);
}
