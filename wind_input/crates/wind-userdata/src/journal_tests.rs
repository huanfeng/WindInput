use super::*;
use crate::record::*;

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_jnl-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    let p = d.join(name);
    let _ = std::fs::remove_file(&p);
    p
}
fn rec(seq: u64, code: &str) -> Record {
    Record {
        seq,
        ts_ms: 1,
        kind: OpKind::Upsert,
        target: Target::Temp,
        schema: "s".into(),
        code: code.into(),
        text: "t".into(),
        payload: Payload::Weight(1),
        boundary: 0,
    }
}

/// 审查 I-5：取刷盘句柄即清 dirty（锁外刷）；刷失败可放回。
#[test]
fn sync_handle_is_taken_once_and_can_be_returned() {
    let p = tmp("handle.wjl");
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    assert!(j.take_sync_handle().unwrap().is_none());
    j.append(&rec(j.next_seq(), "c")).unwrap();
    let h = j.take_sync_handle().unwrap().expect("有新写入");
    assert!(!j.is_dirty());
    h.sync_data().unwrap();
    j.mark_dirty();
    assert!(j.is_dirty());
}

#[test]
fn sync_if_dirty_only_after_append() {
    let p = tmp("dirty.wjl");
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    assert!(!j.is_dirty());
    assert!(!j.sync_if_dirty().unwrap(), "没有新写入不 fsync");
    j.append(&rec(j.next_seq(), "c")).unwrap();
    assert!(j.is_dirty());
    assert!(j.sync_if_dirty().unwrap());
    assert!(!j.is_dirty());
    assert!(!j.sync_if_dirty().unwrap());
}

#[test]
fn append_then_replay_from_watermark() {
    let p = tmp("a.wjl");
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    for i in 1..=5 {
        j.append(&rec(i, "c")).unwrap();
    }
    assert_eq!(j.next_seq(), 6);
    let got = Journal::replay(&p, 3).unwrap();
    assert_eq!(got.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![4, 5]);
    assert_eq!(Journal::tail_seq(&p).unwrap(), 5);
    drop(j);
    let j2 = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    assert_eq!(j2.next_seq(), 6, "重开续号");
}

/// Review Focus 1：尾部半条记录（断电 / SIGKILL）⇒ 截到最后一条完整记录，不 panic。
#[test]
fn truncated_tail_stops_at_last_complete_record() {
    let p = tmp("t.wjl");
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    for i in 1..=3 {
        j.append(&rec(i, "c")).unwrap();
    }
    drop(j);
    let mut bytes = std::fs::read(&p).unwrap();
    bytes.truncate(bytes.len() - 5);
    std::fs::write(&p, &bytes).unwrap();
    let got = Journal::replay(&p, 0).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(Journal::tail_seq(&p).unwrap(), 2);
    // 重开后在截断点续写，新记录可读
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    assert_eq!(j.next_seq(), 3);
    j.append(&rec(3, "d")).unwrap();
    let got = Journal::replay(&p, 0).unwrap();
    assert_eq!(got.len(), 3);
    assert_eq!(got[2].code, "d");
}

#[test]
fn prune_rewrites_without_old_head_and_keeps_header() {
    let p = tmp("p.wjl");
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    for i in 1..=6 {
        j.append(&rec(i, "c")).unwrap();
    }
    j.prune_before(4).unwrap();
    drop(j);
    let got = Journal::replay(&p, 0).unwrap();
    assert_eq!(got.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![4, 5, 6]);
    let j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    assert_eq!(j.next_seq(), 7);
    assert!(!p.with_extension("wjl.tmp").exists(), "tmp 不残留");
}

#[test]
fn bump_next_seq_only_moves_forward() {
    let p = tmp("bump.wjl");
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    j.bump_next_seq(10);
    assert_eq!(j.next_seq(), 10);
    j.bump_next_seq(3);
    assert_eq!(j.next_seq(), 10, "不往回拨");
    j.append(&rec(10, "c")).unwrap();
    assert_eq!(Journal::tail_seq(&p).unwrap(), 10);
}

/// 字段长度编码是 u16：超长的必须在写盘前拒掉，否则写成一条合法 CRC 却解不开的帧，
/// 下次 open 从它截断，之后的记录全丢。
#[test]
fn oversized_field_is_refused_before_it_reaches_disk() {
    let p = tmp("big.wjl");
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    let mut big = rec(1, "c");
    big.text = "字".repeat(30_000);
    assert!(j.append(&big).is_err(), "超过 u16 的字段要拒");
    assert_eq!(j.next_seq(), 1, "拒掉的不占号");
    j.append(&rec(1, "a")).unwrap();
    j.append(&rec(2, "b")).unwrap();
    drop(j);
    let j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    assert_eq!(j.next_seq(), 3);
    assert_eq!(Journal::replay(&p, 0).unwrap().len(), 2);
}

/// 裁剪走 rename：已打开的 Journal 若还拿着旧 fd，后续追加会写进被替换掉的旧 inode。
#[test]
fn append_after_prune_lands_in_the_pruned_file() {
    let p = tmp("pa.wjl");
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    for i in 1..=4 {
        j.append(&rec(i, "c")).unwrap();
    }
    j.prune_before(3).unwrap();
    j.append(&rec(5, "d")).unwrap();
    let got = Journal::replay(&p, 0).unwrap();
    assert_eq!(got.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
}

#[test]
fn wrong_header_is_rejected_and_out_of_order_seq_is_refused() {
    let p = tmp("h.wjl");
    std::fs::write(&p, b"NOPE").unwrap();
    assert!(Journal::open(&p, [1; 16], [2; 16]).is_err());
    let p = tmp("seq.wjl");
    let mut j = Journal::open(&p, [1; 16], [2; 16]).unwrap();
    assert!(j.append(&rec(5, "c")).is_err(), "seq 必须连续");
    assert_eq!(j.next_seq(), 1);
}
