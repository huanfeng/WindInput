use crate::Store;

fn db(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("wind_mig_{name}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_dir_all(crate::store::derived_state_dir(&p));
    p
}

/// 造一个「老版本写的库」：先开一次、往旧表直接写行，再删掉派生状态目录（老版本没有它）。
fn legacy_db(name: &str) -> std::path::PathBuf {
    let p = db(name);
    let s = Store::open(&p).unwrap();
    s.legacy_put_temp("py", "nihao", "你好", 800, 3, 1_700_000_000, 0b101)
        .unwrap();
    s.legacy_put_temp("py", "xian", "西安", 500, 1, 1_700_000_001, 0b101)
        .unwrap();
    s.legacy_put_draft("wb", "ggtt", "盖伦", crate::draft_words::now_for_test())
        .unwrap();
    s.legacy_put_freq("py", "nihao", "你好", 7, 1_700_000_100)
        .unwrap();
    s.legacy_put_assoc("py", "天", "气", 2, 1_700_000_200)
        .unwrap();
    drop(s);
    std::fs::remove_dir_all(crate::store::derived_state_dir(&p)).unwrap();
    p
}

/// Review Focus 1：四张表全部迁入、简拼可召回；第二次打开不再迁移。
#[test]
fn legacy_tables_are_migrated_once() {
    let p = legacy_db("once");
    let s = std::sync::Arc::new(Store::open(&p).unwrap());
    let w = s.get_temp_words("py", "nihao").unwrap();
    assert_eq!(
        (w[0].weight, w[0].count, w[0].created_at, w[0].boundary),
        (800, 3, 1_700_000_000, 0b101)
    );
    use wind_dict::DictLayer;
    let l = crate::StoreTempLayer::new(s.clone(), "py");
    assert!(
        l.search_abbrev("xa", 0).iter().any(|c| c.text == "西安"),
        "简拼可召回"
    );
    assert_eq!(s.search_drafts("wb", "ggtt", 86_400).unwrap(), vec!["盖伦"]);
    assert_eq!(
        s.get_freq("py", "nihao", "你好")
            .unwrap()
            .map(|r| (r.count, r.last_used)),
        Some((7, 1_700_000_100))
    );
    assert_eq!(s.assoc_history("py", "天", 0).unwrap()[0].1.count, 2);
    let marker = crate::store::derived_state_dir(&p).join(super::MIGRATED_P1);
    assert!(marker.is_file());
    // 迁移后改一笔，再开：不得被旧表覆盖回去
    s.record_freq("py", "nihao", "你好").unwrap();
    drop(l);
    drop(s);
    let s = Store::open(&p).unwrap();
    assert_eq!(s.get_freq("py", "nihao", "你好").unwrap().unwrap().count, 8);
}

/// Review Focus 2：标记没写成（崩溃）⇒ 重跑，结果与一次成功相同，不翻倍。
#[test]
fn rerun_after_crash_does_not_double_count() {
    let p = legacy_db("rerun");
    let s = Store::open(&p).unwrap();
    drop(s);
    std::fs::remove_file(crate::store::derived_state_dir(&p).join(super::MIGRATED_P1)).unwrap();
    let s = Store::open(&p).unwrap();
    assert_eq!(s.get_temp_word("py", "nihao", "你好").unwrap(), Some(3));
    assert_eq!(s.get_freq("py", "nihao", "你好").unwrap().unwrap().count, 7);
    assert_eq!(s.count_temp_words("py").unwrap(), 2);
}

#[test]
fn empty_legacy_db_writes_marker_only() {
    let p = db("empty");
    let s = Store::open(&p).unwrap();
    assert!(
        crate::store::derived_state_dir(&p)
            .join(super::MIGRATED_P1)
            .is_file()
    );
    assert_eq!(s.count_temp_words("py").unwrap(), 0);
}

/// 审查 I1：迁移失败（某目标折回不了）后照常运行，这一轮的写入不得在下次重做时被旧表覆盖。
#[test]
fn failed_migration_keeps_writes_made_before_retry() {
    let p = legacy_db("fail");
    let state = crate::store::derived_state_dir(&p);
    std::fs::create_dir_all(&state).unwrap();
    // 普通文件占住 wb 的状态子目录：草稿 wb 的折回建不了目录 ⇒ 迁移失败
    std::fs::write(state.join("wb"), b"x").unwrap();
    let s = Store::open(&p).unwrap();
    assert!(!s.migrated_p1(), "迁移失败：标记不写");
    s.learn_temp_word("py", "xinci", "新词", 800, 0).unwrap();
    s.learn_temp_word("py", "nihao", "你好", 800, 0b101)
        .unwrap();
    drop(s);
    std::fs::remove_file(state.join("wb")).unwrap();
    let s = Store::open(&p).unwrap();
    assert!(s.migrated_p1());
    assert_eq!(
        s.get_temp_word("py", "xinci", "新词").unwrap(),
        Some(1),
        "失败那轮学的词还在"
    );
    assert_eq!(
        s.get_temp_word("py", "nihao", "你好").unwrap(),
        Some(4),
        "失败那轮的 count+1 不被旧表的 3 覆盖"
    );
    assert_eq!(
        s.search_drafts("wb", "ggtt", 86_400).unwrap(),
        vec!["盖伦"],
        "重做补上失败的目标"
    );
    assert_eq!(s.get_freq("py", "nihao", "你好").unwrap().unwrap().count, 7);
}
