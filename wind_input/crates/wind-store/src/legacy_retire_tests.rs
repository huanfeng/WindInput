use super::*;
use wind_userdata::record::{OpKind, Payload, Target};

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_retire_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn loc(d: &Path) -> crate::store::StateLoc {
    crate::store::StateLoc {
        state_dir: d.join("state"),
        dir_id: "d".into(),
        text_dir: d.join(crate::DATA_DIR_NAME),
        legacy_done: true,
    }
}

#[test]
fn retire_renames_redb_and_never_clobbers_an_older_legacy() {
    let d = dir("retire");
    std::fs::write(d.join("userdata.redb"), "db1").unwrap();
    retire_legacy_db(&d);
    assert_eq!(
        std::fs::read_to_string(d.join("userdata.redb.legacy")).unwrap(),
        "db1"
    );
    assert!(!d.join("userdata.redb").exists());
    std::fs::write(d.join("userdata.redb"), "db2").unwrap();
    retire_legacy_db(&d);
    assert_eq!(
        std::fs::read_to_string(d.join("userdata.redb.legacy")).unwrap(),
        "db1",
        "已有的 .legacy 不被覆盖"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("userdata.redb.legacy.2")).unwrap(),
        "db2"
    );
    retire_legacy_db(&d); // 没有 redb：无事
}

#[test]
fn forget_markers_keeps_legacy_chain_done() {
    let d = dir("forget");
    let l = loc(&d);
    std::fs::create_dir_all(&l.state_dir).unwrap();
    for m in crate::migrate_p3::ALL_MIGRATION_MARKERS {
        std::fs::write(l.state_dir.join(m), "1\n").unwrap();
    }
    let s = Store::open_with_state(d.join("userdata.redb"), l.clone()).unwrap();
    s.forget_legacy_markers();
    assert!(
        crate::migrate_p3::ALL_MIGRATION_MARKERS
            .iter()
            .all(|m| !l.state_dir.join(m).exists())
    );
    assert_eq!(
        s.format_version_reached(),
        Some(crate::migrate_p3::FULLY_MIGRATED_VERSION)
    );
}

/// 靶机发现：标记删了之后设定系统短语不得再去读（已不打开的）旧库做短语迁移。
#[test]
fn system_phrases_after_markers_forgotten_do_not_touch_legacy_db() {
    let d = dir("phrases_after");
    let s = Store::open_with_state(d.join("userdata.redb"), loc(&d)).unwrap();
    s.forget_legacy_markers();
    s.set_system_phrases(&[]).unwrap();
    s.migrate_phrases_now().unwrap();
}

/// Review Focus 2：学到的数据只在引擎快照里、`data/` 下还没有文本 ⇒ 打开时由快照生成文本，
/// 不被「文件不存在」清空。
#[test]
fn learned_data_only_in_snapshot_is_written_out_at_open() {
    let d = dir("adopt");
    let l = loc(&d);
    {
        let ud = wind_userdata::engine::UserData::open(wind_userdata::engine::Dirs {
            state_dir: l.state_dir.clone(),
            dir_id: "d".into(),
        })
        .unwrap();
        ud.apply(
            Target::Temp,
            "pinyin",
            OpKind::Upsert,
            "nihao",
            "你好",
            Payload::Weight(7),
            0b101,
        )
        .unwrap();
        ud.apply(
            Target::Freq,
            "pinyin",
            OpKind::Incr,
            "nihao",
            "你好",
            Payload::Count(3),
            0,
        )
        .unwrap();
        ud.fold_with(Target::Temp, "pinyin", &mut |v| v).unwrap();
        ud.fold_with(Target::Freq, "pinyin", &mut |v| v).unwrap();
    }
    let s = Store::open_with_state(d.join("userdata.redb"), l.clone()).unwrap();
    assert!(l.text_dir.join("pinyin/temp.dict.yaml").is_file());
    assert!(l.text_dir.join("pinyin/freq.table.yaml").is_file());
    assert_eq!(
        s.runtime()
            .temp_word("pinyin", "nihao", "你好")
            .unwrap()
            .weight,
        7
    );
    assert_eq!(
        s.get_freq("pinyin", "nihao", "你好")
            .unwrap()
            .unwrap()
            .count,
        3
    );
}
