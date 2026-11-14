//! 学到的数据的表格文本目标（设计 §2.4 / §2.5 / P5）。
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wind_userdata::engine::{Dirs, TextBinding, TextFormat, UserData};
use wind_userdata::record::{OpKind, Payload, Target};
use wind_userdata::text::codec::{self, TableSpec};

fn root(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wind_learned_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn open(d: &Path) -> UserData {
    UserData::open(Dirs {
        state_dir: d.join("state"),
        dir_id: "dir".into(),
    })
    .unwrap()
}

fn binding(path: &Path, spec: &'static TableSpec) -> TextBinding {
    TextBinding {
        path: path.into(),
        name: "s.x".into(),
        lowercase: false,
        syllable_codes: false,
        format: TextFormat::Table(spec),
    }
}

fn incr(ud: &UserData, t: Target, code: &str, text: &str, n: u32) {
    ud.apply(t, "s", OpKind::Incr, code, text, Payload::Count(n), 0)
        .unwrap();
}

#[test]
fn table_target_folds_counts_into_text() {
    let d = root("fold_counts");
    let ud = open(&d);
    let f = d.join("data/s/freq.table.yaml");
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    for _ in 0..3 {
        incr(&ud, Target::Freq, "a", "啊", 1);
    }
    ud.fold_text(Target::Freq, "s").unwrap();
    let s = std::fs::read_to_string(&f).unwrap();
    assert!(s.contains("a\t啊\t3\t"), "{s}");
    assert_eq!(ud.view().delta.pending(Target::Freq, "s"), 0);
    incr(&ud, Target::Freq, "a", "啊", 2);
    ud.fold_text(Target::Freq, "s").unwrap();
    assert!(std::fs::read_to_string(&f).unwrap().contains("a\t啊\t5\t"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn bind_adopts_existing_view_when_text_missing() {
    let d = root("adopt");
    let ud = open(&d);
    ud.apply(
        Target::Temp,
        "s",
        OpKind::Upsert,
        "ab",
        "阿布",
        Payload::Weight(5),
        0,
    )
    .unwrap();
    incr(&ud, Target::Temp, "ab", "阿布", 2);
    // 模拟 v5：数据只在快照里
    ud.fold_with(Target::Temp, "s", &mut |v| v).unwrap();
    let f = d.join("data/s/temp.dict.yaml");
    ud.bind_text(Target::Temp, "s", binding(&f, &codec::TEMP))
        .unwrap();
    let s = std::fs::read_to_string(&f).unwrap();
    assert!(s.contains("ab\t阿布\t5\t2\t"), "{s}");
    let e = ud.get(Target::Temp, "s", "ab", "阿布").unwrap();
    assert_eq!(
        (e.weight, e.count),
        (5, 2),
        "绑定后数据仍在，没有被「文件不存在」清空"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn adopt_includes_unfolded_delta() {
    let d = root("adopt_delta");
    let ud = open(&d);
    incr(&ud, Target::Assoc, "上文", "上文词", 4);
    let f = d.join("data/s/assoc.table.yaml");
    ud.bind_text(Target::Assoc, "s", binding(&f, &codec::ASSOC))
        .unwrap();
    assert!(
        std::fs::read_to_string(&f)
            .unwrap()
            .contains("上文\t上文词\t4\t")
    );
    assert_eq!(ud.view().delta.pending(Target::Assoc, "s"), 0);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn deleting_state_dir_rebuilds_learned_target_from_text() {
    let d = root("wipe_state");
    let f = d.join("data/s/freq.table.yaml");
    {
        let ud = open(&d);
        ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
            .unwrap();
        incr(&ud, Target::Freq, "a", "啊", 4);
        ud.fold_text(Target::Freq, "s").unwrap();
    }
    std::fs::remove_dir_all(d.join("state")).unwrap();
    let ud = Arc::new(open(&d));
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    let r = wind_userdata::runtime::Runtime::new(ud);
    assert_eq!(r.freq("s", "a", "啊").map(|x| x.count), Some(4));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn isolated_learned_text_keeps_learning_and_merges_after_fix() {
    let d = root("isolated");
    let ud = open(&d);
    let f = d.join("data/s/freq.table.yaml");
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    incr(&ud, Target::Freq, "a", "啊", 1);
    ud.fold_text(Target::Freq, "s").unwrap();
    let good = std::fs::read(&f).unwrap();
    std::fs::write(&f, b"\xff\xfe not utf8").unwrap();
    incr(&ud, Target::Freq, "a", "啊", 1);
    assert!(ud.fold_text(Target::Freq, "s").is_err());
    assert_eq!(
        std::fs::read(&f).unwrap(),
        b"\xff\xfe not utf8",
        "坏文件不被覆盖"
    );
    assert!(ud.text_isolated(Target::Freq, "s"));
    assert_eq!(
        ud.get(Target::Freq, "s", "a", "啊").map(|e| e.count),
        Some(2),
        "隔离期间照常学习、查询"
    );
    std::fs::write(&f, &good).unwrap();
    ud.fold_text(Target::Freq, "s").unwrap();
    assert!(
        std::fs::read_to_string(&f).unwrap().contains("a\t啊\t2\t"),
        "修好后合入"
    );
    assert!(!ud.text_isolated(Target::Freq, "s"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn auto_purge_never_rewrites_isolated_text() {
    let d = root("purge_isolated");
    let ud = open(&d);
    let f = d.join("data/s/draft.dict.yaml");
    ud.bind_text(Target::Draft, "s", binding(&f, &codec::DRAFT))
        .unwrap();
    ud.apply(
        Target::Draft,
        "s",
        OpKind::Upsert,
        "a",
        "啊",
        Payload::Weight(0),
        0,
    )
    .unwrap();
    ud.fold_text(Target::Draft, "s").unwrap();
    std::fs::write(&f, b"---\nbroken: [\n").unwrap();
    assert!(
        ud.fold_with(Target::Draft, "s", &mut |_| Vec::new())
            .is_err()
    );
    assert_eq!(std::fs::read(&f).unwrap(), b"---\nbroken: [\n");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn fold_with_on_table_target_rewrites_text() {
    let d = root("fold_with_text");
    let ud = open(&d);
    let f = d.join("data/s/temp.dict.yaml");
    ud.bind_text(Target::Temp, "s", binding(&f, &codec::TEMP))
        .unwrap();
    for (c, t) in [("a", "啊"), ("b", "不")] {
        ud.apply(
            Target::Temp,
            "s",
            OpKind::Upsert,
            c,
            t,
            Payload::Weight(1),
            0,
        )
        .unwrap();
    }
    ud.fold_with(Target::Temp, "s", &mut |v| {
        v.into_iter().filter(|e| e.code == "b").collect()
    })
    .unwrap();
    let s = std::fs::read_to_string(&f).unwrap();
    assert!(!s.contains("啊") && s.contains("b\t不\t1\t"), "{s}");
    assert!(ud.get(Target::Temp, "s", "a", "啊").is_none());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn explicit_replace_preserves_corrupt_file() {
    let d = root("replace_corrupt");
    let ud = open(&d);
    let f = d.join("data/s/temp.dict.yaml");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, b"\xff bad").unwrap();
    let _ = ud.bind_text(Target::Temp, "s", binding(&f, &codec::TEMP));
    ud.replace_text_entries(Target::Temp, "s", Vec::new())
        .unwrap();
    let kept: Vec<_> = std::fs::read_dir(f.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("temp.dict.yaml.corrupt-")
        })
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read(kept[0].path()).unwrap(), b"\xff bad");
    assert!(!ud.text_isolated(Target::Temp, "s"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn hand_edited_count_wins_and_pending_increments_add_on_top() {
    let d = root("hand_edit");
    let ud = open(&d);
    let f = d.join("data/s/freq.table.yaml");
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    incr(&ud, Target::Freq, "a", "啊", 1);
    ud.fold_text(Target::Freq, "s").unwrap();
    let s = std::fs::read_to_string(&f)
        .unwrap()
        .replace("a\t啊\t1\t", "a\t啊\t50\t");
    std::fs::write(&f, s).unwrap();
    incr(&ud, Target::Freq, "a", "啊", 1);
    ud.fold_text(Target::Freq, "s").unwrap();
    assert!(std::fs::read_to_string(&f).unwrap().contains("a\t啊\t51\t"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn clearing_corrupt_learned_text_keeps_the_old_file_aside() {
    let d = root("clear_corrupt");
    let ud = Arc::new(open(&d));
    let f = d.join("data/s/freq.table.yaml");
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    incr(&ud, Target::Freq, "a", "啊", 1);
    ud.fold_text(Target::Freq, "s").unwrap();
    std::fs::write(&f, b"\xff").unwrap();
    let r = wind_userdata::runtime::Runtime::new(ud);
    assert_eq!(r.clear_freq("s").unwrap(), 1);
    assert!(r.freq("s", "a", "啊").is_none());
    assert!(
        std::fs::read_dir(f.parent().unwrap())
            .unwrap()
            .flatten()
            .any(|e| e
                .file_name()
                .to_string_lossy()
                .starts_with("freq.table.yaml.corrupt-"))
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn rule_binds_target_on_maintain_and_fold_with() {
    let d = root("rule");
    let ud = open(&d);
    let data = d.join("data");
    ud.set_text_rule(Box::new(move |t, s| match t {
        Target::Freq => Some(binding(&data.join(s).join("freq.table.yaml"), &codec::FREQ)),
        Target::Draft => Some(binding(
            &data.join(s).join("draft.dict.yaml"),
            &codec::DRAFT,
        )),
        _ => None,
    }));
    ud.set_text_idle_for_test(std::time::Duration::ZERO);
    incr(&ud, Target::Freq, "a", "啊", 1);
    ud.maintain().unwrap();
    assert!(d.join("data/s/freq.table.yaml").is_file());
    ud.apply(
        Target::Draft,
        "s",
        OpKind::Upsert,
        "b",
        "不",
        Payload::Weight(0),
        0,
    )
    .unwrap();
    ud.fold_with(Target::Draft, "s", &mut |v| v).unwrap();
    assert!(d.join("data/s/draft.dict.yaml").is_file());
    assert!(
        !ud.bind_by_rule(Target::Temp, "s").unwrap(),
        "规则不管的目标"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// 审查 I-1：由快照生成文本失败 ⇒ 不登记绑定；数据仍在；修好后再绑定照常生成。
#[test]
fn failed_adopt_does_not_bind_and_keeps_snapshot() {
    let d = root("adopt_fail");
    let ud = open(&d);
    incr(&ud, Target::Freq, "a", "啊", 5);
    ud.fold_with(Target::Freq, "s", &mut |v| v).unwrap();
    std::fs::create_dir_all(d.join("data")).unwrap();
    std::fs::write(d.join("data/s"), b"blocker").unwrap(); // 方案目录被文件占住
    let f = d.join("data/s/freq.table.yaml");
    assert!(
        ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
            .is_err()
    );
    assert!(
        ud.text_binding(Target::Freq, "s").is_none(),
        "失败不登记绑定"
    );
    incr(&ud, Target::Freq, "a", "啊", 1);
    ud.fold_with(Target::Freq, "s", &mut |v| v).unwrap();
    assert_eq!(
        ud.get(Target::Freq, "s", "a", "啊").map(|e| e.count),
        Some(6)
    );
    std::fs::remove_file(d.join("data/s")).unwrap();
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    assert!(std::fs::read_to_string(&f).unwrap().contains("a\t啊\t6\t"));
    let _ = std::fs::remove_dir_all(&d);
}

/// 审查 I-1 兜底：从没归文本管过（state 无指纹）的数据，文件不在时同步不得装空段。
#[test]
fn sync_never_wipes_data_that_was_never_text_owned() {
    let d = root("sync_guard");
    let ud = open(&d);
    ud.apply(
        Target::User,
        "s",
        OpKind::Upsert,
        "a",
        "啊",
        Payload::Weight(1),
        0,
    )
    .unwrap();
    ud.fold_with(Target::User, "s", &mut |v| v).unwrap();
    let f = d.join("data/s/user.dict.yaml");
    let _ = ud.bind_text(
        Target::User,
        "s",
        TextBinding {
            path: f,
            name: "s.user".into(),
            lowercase: false,
            syllable_codes: false,
            format: TextFormat::Words,
        },
    );
    assert!(
        ud.get(Target::User, "s", "a", "啊").is_some(),
        "不由「文件不存在」推出数据为空"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// 审查 I-2：文本隔离时日志卡住裁不动 ⇒ 不再每拍重写日志、重试坏文件。
#[test]
fn stuck_journal_does_not_retrigger_maintenance_every_tick() {
    let d = root("stuck");
    let ud = open(&d);
    let f = d.join("data/s/freq.table.yaml");
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    incr(&ud, Target::Freq, "a", "啊", 1);
    ud.fold_text(Target::Freq, "s").unwrap();
    std::fs::write(&f, b"\xff").unwrap();
    for _ in 0..50 {
        incr(&ud, Target::Freq, "a", "啊", 1);
    }
    ud.compact().unwrap(); // 折不回，日志裁不动
    assert!(
        !ud.needs_maintain_with(usize::MAX, 1),
        "日志超限但裁不动、目标在退避中 ⇒ 不再起维护"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// 审查 I-4：连续写入（从不空闲）也要在待处理够久后折回文本。
#[test]
fn continuous_writes_still_fold_after_max_pending_age() {
    let d = root("max_age");
    let ud = open(&d);
    let f = d.join("data/s/freq.table.yaml");
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    ud.set_text_idle_for_test(std::time::Duration::from_secs(3600));
    ud.set_text_max_pending_for_test(std::time::Duration::ZERO);
    incr(&ud, Target::Freq, "a", "啊", 1);
    ud.maintain().unwrap();
    assert!(f.is_file(), "未空闲但待处理超龄 ⇒ 折回");
    let _ = std::fs::remove_dir_all(&d);
}

/// 外部编辑中（暂停写回）的表格文本目标：整体替换（清空 / 导入 / 还原）与带策略的整写都拒绝，
/// 文件不动；学习照常进 delta，恢复写回后落进文件。
#[test]
fn held_table_target_refuses_whole_file_writes() {
    let d = root("held");
    let ud = open(&d);
    let f = d.join("data/s/freq.table.yaml");
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    incr(&ud, Target::Freq, "a", "啊", 1);
    ud.fold_text(Target::Freq, "s").unwrap();
    let before = std::fs::read(&f).unwrap();
    ud.hold_text(Target::Freq, "s");
    let e = ud
        .replace_text_entries(Target::Freq, "s", Vec::new())
        .unwrap_err()
        .to_string();
    assert!(e.contains("正在外部编辑"), "{e}");
    assert!(ud.clear(Target::Freq, "s").is_err());
    assert!(ud.fold_with(Target::Freq, "s", &mut |v| v).is_err());
    incr(&ud, Target::Freq, "b", "吧", 1);
    ud.compact().unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), before);
    ud.release_text(Target::Freq, "s");
    assert!(std::fs::read_to_string(&f).unwrap().contains("b\t吧\t1\t"));
    assert_eq!(ud.clear(Target::Freq, "s").unwrap(), 2);
    let _ = std::fs::remove_dir_all(&d);
}

/// 暂停写回与整写互斥：整写进行中（已过「是否暂停」那道检查）开始暂停，要等这次写完才生效；
/// 生效之后的整写一律拒绝——不会出现「检查时没暂停、写的时候已经在外部编辑」。
#[test]
fn hold_waits_for_in_flight_whole_file_write() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let d = root("hold_race");
    let ud = Arc::new(open(&d));
    let f = d.join("data/s/freq.table.yaml");
    ud.bind_text(Target::Freq, "s", binding(&f, &codec::FREQ))
        .unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let held_during_write = Arc::new(AtomicBool::new(false));
    let me = std::thread::current().id();
    let hook = wind_userdata::text::fold::add_before_rename_hook(Box::new({
        let (ud, fired, held_during_write) = (ud.clone(), fired.clone(), held_during_write.clone());
        move || {
            if std::thread::current().id() != me || fired.swap(true, Ordering::SeqCst) {
                return;
            }
            let u = ud.clone();
            std::thread::spawn(move || u.hold_text(Target::Freq, "s"));
            std::thread::sleep(std::time::Duration::from_millis(150));
            held_during_write.store(ud.is_held(Target::Freq, "s"), Ordering::SeqCst);
        }
    }));
    ud.replace_text_entries(Target::Freq, "s", Vec::new())
        .unwrap();
    drop(hook);
    assert!(
        !held_during_write.load(Ordering::SeqCst),
        "整写进行中暂停不能插进来"
    );
    for _ in 0..200 {
        if ud.is_held(Target::Freq, "s") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let e = ud
        .replace_text_entries(Target::Freq, "s", Vec::new())
        .unwrap_err()
        .to_string();
    assert!(e.contains("正在外部编辑"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}
