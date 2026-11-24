use super::*;

/// redb 在同一进程里对已打开的文件拒绝再开：能另开一份 ⇒ Store 没握着它（不锁文件）。
fn redb_is_released(path: &Path) -> bool {
    Database::open(path).is_ok()
}

/// 迁移全部完成（短语迁移是最后一步）后立即放下旧库：不再锁文件。
#[test]
fn legacy_db_is_released_once_fully_migrated() {
    let path = std::env::temp_dir().join(format!("wind_store_release-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(derived_state_dir(&path));
    let _ = std::fs::remove_dir_all(derived_text_dir(&path));
    let s = Store::open(&path).unwrap();
    assert!(!redb_is_released(&path), "短语还没迁，旧库要留着");
    s.migrate_phrases_now().unwrap();
    assert!(s.migrated_p3() && s.migrated_p4());
    assert!(redb_is_released(&path), "全部迁完就放下旧库");
    drop(s);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(derived_state_dir(&path));
    let _ = std::fs::remove_dir_all(derived_text_dir(&path));
}

/// 用户目录已记为迁完（`legacy_done`，来自 meta.toml 的 format_version）时，state 里的迁移
/// 标记被删了也不再从 redb 重迁：升级后删掉的词不会被旧表带回来，redb 不开、不写；
/// 标记不补写（P5 起判据只认 format_version），其余依赖旧库链完成的判断照旧。
#[test]
fn legacy_done_skips_remigration_even_if_markers_are_deleted() {
    let path = std::env::temp_dir().join(format!("wind_store_ldone-{}.redb", std::process::id()));
    let st = std::env::temp_dir().join(format!("wind_store_ldone_state-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&st);
    let loc = |done: bool| StateLoc {
        state_dir: st.clone(),
        dir_id: "d".into(),
        text_dir: st.join(crate::DATA_DIR_NAME),
        legacy_done: done,
    };
    let del_markers = || {
        for m in crate::migrate_p3::ALL_MIGRATION_MARKERS {
            let _ = std::fs::remove_file(st.join(m));
        }
    };
    {
        let s = Store::open_with_state(&path, loc(false)).unwrap();
        s.legacy_put_user("wb", "a", "工", 1, 0, 0).unwrap();
    }
    // 旧表里的词迁成文本，然后用户在新版本里删掉它
    del_markers();
    {
        let s = Store::open_with_state(&path, loc(false)).unwrap();
        s.migrate_phrases_now().unwrap();
        assert_eq!(s.get_user_words("wb", "a").unwrap().len(), 1, "迁进来了");
        s.remove_user_word("wb", "a", "工").unwrap();
        s.flush_user_data().unwrap();
    }
    del_markers();
    let before = std::fs::read(&path).unwrap();
    let s = Store::open_with_state(&path, loc(true)).unwrap();
    assert!(
        s.get_user_words("wb", "a").unwrap().is_empty(),
        "删掉的词不被旧表带回"
    );
    assert!(redb_is_released(&path), "不打开旧库");
    assert!(
        crate::migrate_p3::ALL_MIGRATION_MARKERS
            .iter()
            .all(|m| !st.join(m).exists()),
        "判据只认 format_version：标记不再补写（P5）"
    );
    drop(s);
    assert!(std::fs::read(&path).unwrap() == before, "旧库文件不被写");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&st);
}

/// 已经迁完的库再打开：根本不碰 redb（不锁、不写——文件逐字节不变，留给回退的旧版本）。
#[test]
fn fully_migrated_store_never_opens_legacy_db() {
    let path = std::env::temp_dir().join(format!("wind_store_noopen-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(derived_state_dir(&path));
    let _ = std::fs::remove_dir_all(derived_text_dir(&path));
    {
        let s = Store::open(&path).unwrap();
        s.migrate_phrases_now().unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let s = Store::open(&path).unwrap();
    assert!(redb_is_released(&path), "迁完的库重开不再打开旧库");
    s.add_user_word("wb", "a", "工", 1, 0).unwrap();
    assert_eq!(
        s.get_user_words("wb", "a").unwrap()[0].text,
        "工",
        "照常读写"
    );
    drop(s);
    assert!(std::fs::read(&path).unwrap() == before, "旧库文件不被写");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(derived_state_dir(&path));
    let _ = std::fs::remove_dir_all(derived_text_dir(&path));
}

#[test]
fn list_data_schemas_across_tables() {
    let path = std::env::temp_dir().join("wind_store_schemas_test.redb");
    let _ = std::fs::remove_file(&path);
    let s = Store::open(&path).unwrap();
    s.add_user_word("wb", "a", "工", 1, 0).unwrap();
    s.learn_temp_word("py", "ni", "你", 1, 0).unwrap();
    s.record_freq("sp", "x", "词").unwrap();
    s.pin_shadow("wb", "aa", "恭", None, 0).unwrap();
    let mut got = s.list_data_schemas().unwrap();
    got.sort();
    assert_eq!(got, vec!["py", "sp", "wb"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_open_version_persist_and_reopen() {
    let path = std::env::temp_dir().join("wind_store_skeleton_test.redb");
    let _ = std::fs::remove_file(&path);

    // 首次打开：版本应为当前版本
    {
        let s = Store::open(&path).unwrap();
        assert_eq!(s.version().unwrap(), CURRENT_VERSION);
    }
    // 重开：版本持久化（证明写事务落盘）
    {
        let s = Store::open(&path).unwrap();
        assert_eq!(s.version().unwrap(), CURRENT_VERSION);
        // pause/resume 往返：暂停态报错，恢复后可用
        s.pause().unwrap();
        assert!(s.is_paused());
        assert!(s.version().is_err(), "暂停态读取应失败");
        s.resume().unwrap();
        assert!(!s.is_paused());
        assert_eq!(s.version().unwrap(), CURRENT_VERSION);
    }
    let _ = std::fs::remove_file(&path);
}
fn tmp_db(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_store_rt-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d.join(format!("{name}.redb"))
}

#[test]
fn open_attaches_runtime_in_derived_state_dir() {
    let p = tmp_db("derived");
    let _ = std::fs::remove_file(&p);
    let s = Store::open(&p).unwrap();
    s.runtime().learn_temp("py", "a", "甲", 1, 0).unwrap();
    assert!(derived_state_dir(&p).join("journal").is_dir());
    drop(s);
    let s = Store::open(&p).unwrap();
    assert!(
        s.runtime().temp_word("py", "a", "甲").is_some(),
        "同一库重开，引擎数据仍在"
    );
}

/// P5：学到的数据写进用户数据目录的文本（复制用户目录即带走）。
#[test]
fn learned_data_is_written_to_data_dir() {
    let p = tmp_db("learned_text");
    let _ = std::fs::remove_file(&p);
    let s = Store::open(&p).unwrap();
    s.learn_temp_word("pinyin", "nihao", "你好", 10, 0b101)
        .unwrap();
    s.record_freq("pinyin", "nihao", "你好").unwrap();
    s.record_assoc_pick("pinyin", "我们", "我们的").unwrap();
    s.flush_user_data().unwrap();
    let dir = s.user_text_dir().join("pinyin");
    let temp = std::fs::read_to_string(dir.join("temp.dict.yaml")).unwrap();
    assert!(temp.contains("ni hao\t你好\t10\t1\t"), "{temp}");
    assert!(dir.join("freq.table.yaml").is_file());
    assert!(dir.join("assoc.table.yaml").is_file());
}

/// 每类程序写出的数据文件都写一份（文件 → 说明行见 [`banner_table`]）。
fn fill_every_file(s: &Store) {
    s.add_user_word("pinyin", "nihao", "你好", 1, 0b101)
        .unwrap();
    s.learn_temp_word("pinyin", "zaijian", "再见", 1, 0b1001)
        .unwrap();
    s.add_drafts("pinyin", &[("haode".into(), "好的".into())])
        .unwrap();
    s.record_freq("pinyin", "nihao", "你好").unwrap();
    s.record_assoc_pick("pinyin", "我们", "我们的").unwrap();
    s.pin_shadow("pinyin", "ni", "你", None, 0).unwrap();
    s.add_phrase("dz", "地址", 0, 1).unwrap();
    s.record_completion(crate::completion::CompletionKind::EmailSuffix, "qq.com")
        .unwrap();
    s.set_quick_format_enabled("date", "date.lunar", false)
        .unwrap();
    s.set_common_char_override("的", false).unwrap();
    s.record_stat("2026-10-16", 3, 0).unwrap();
    s.put_stats_meta(&crate::stats::StatsMeta::default())
        .unwrap();
    s.set_phrase_sys_hash("h1").unwrap();
    s.set_user_dict_enabled("pinyin", "pinyin", "x", false)
        .unwrap();
    s.flush_user_data().unwrap();
}

fn banner_table() -> Vec<(&'static str, &'static str)> {
    use wind_userdata::text::banner;
    vec![
        ("pinyin/user.dict.yaml", banner::USER_DICT),
        ("pinyin/temp.dict.yaml", banner::TEMP),
        ("pinyin/draft.dict.yaml", banner::DRAFT),
        ("pinyin/freq.table.yaml", banner::FREQ),
        ("pinyin/assoc.table.yaml", banner::ASSOC),
        ("pinyin/shadow.dict.yaml", banner::SHADOW),
        ("pinyin/dicts.toml", banner::USER_DICTS_META),
        ("phrases.dict.yaml", banner::PHRASES),
        ("completion.table.yaml", banner::COMPLETION),
        ("quick_format.toml", banner::QUICK_FORMAT),
        ("common_chars.yaml", banner::COMMON_CHARS),
        ("stats/2026-10.toml", banner::STATS),
        ("stats/meta.toml", banner::STATS),
        ("phrases.sys_hash", banner::PHRASE_SYS_HASH),
    ]
}

/// `data/` 下程序写出的每个文本文件，第一行是各自的说明行。
#[test]
fn every_data_file_starts_with_its_banner() {
    let p = tmp_db("banner");
    let _ = std::fs::remove_file(&p);
    let s = Store::open(&p).unwrap();
    fill_every_file(&s);
    let d = s.user_text_dir();
    for (f, want) in banner_table() {
        let text = std::fs::read_to_string(d.join(f)).unwrap();
        assert_eq!(text.lines().next(), Some(want), "{f}");
    }
    assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("h1"));
    assert_eq!(s.get_daily_stat("2026-10-16").unwrap().total(), 3);
}

/// 打开时给没有说明行的数据文件补上（与整写同一行）：未折回的数据不丢；用户自放的分库、坏文件、
/// 第一行已是注释的不动；再次打开一个字节都不改。
#[test]
fn open_backfills_missing_banners_without_losing_pending_data() {
    use wind_userdata::text::banner;
    let p = tmp_db("backfill");
    let _ = std::fs::remove_file(&p);
    {
        let s = Store::open(&p).unwrap();
        fill_every_file(&s);
        s.set_charset_doc("rare", "key: rare\n").unwrap();
        // 待折回：不 flush
        s.add_user_word("pinyin", "shijie", "世界", 2, 0b1001)
            .unwrap();
        s.learn_temp_word("pinyin", "haoa", "好啊", 1, 0b101)
            .unwrap();
    }
    let d = derived_text_dir(&p);
    let mut table = banner_table();
    table.push(("charsets/rare.yaml", banner::CHARSETS));
    for (f, want) in &table {
        let t = std::fs::read_to_string(d.join(f)).unwrap();
        if let Some(rest) = t.strip_prefix(&format!("{want}\n")) {
            std::fs::write(d.join(f), rest).unwrap();
        }
    }
    let lib = d.join("pinyin/dicts/mine.dict.yaml");
    std::fs::create_dir_all(lib.parent().unwrap()).unwrap();
    let lib_text = "---\nname: mine\ncolumns: [text, code]\n...\n好\thao\n";
    std::fs::write(&lib, lib_text).unwrap();
    let bad = d.join("wubi86/user.dict.yaml");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, "---\nname: [\n").unwrap();
    let own = d.join("wubi86/shadow.dict.yaml");
    std::fs::write(
        &own,
        "# 我自己的说明\n---\ncolumns: [code, text, action]\n...\n",
    )
    .unwrap();

    let s = Store::open(&p).unwrap();
    for (f, want) in &table {
        let text = std::fs::read_to_string(d.join(f)).unwrap();
        assert_eq!(text.lines().next(), Some(*want), "{f}");
    }
    assert_eq!(
        std::fs::read_to_string(&lib).unwrap(),
        lib_text,
        "用户自放的分库不动"
    );
    assert_eq!(
        std::fs::read_to_string(&bad).unwrap(),
        "---\nname: [\n",
        "坏文件不动"
    );
    assert!(
        std::fs::read_to_string(&own)
            .unwrap()
            .starts_with("# 我自己的说明\n")
    );
    s.flush_user_data().unwrap();
    let user = std::fs::read_to_string(d.join("pinyin/user.dict.yaml")).unwrap();
    assert!(user.contains("你好") && user.contains("世界"), "{user}");
    let temp = std::fs::read_to_string(d.join("pinyin/temp.dict.yaml")).unwrap();
    assert!(temp.contains("再见") && temp.contains("好啊"), "{temp}");
    assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("h1"));
    assert!(
        s.get_charset_doc("rare")
            .unwrap()
            .unwrap()
            .contains("key: rare")
    );
    assert!(
        std::fs::read_to_string(d.join("pinyin/user.dict.yaml"))
            .unwrap()
            .starts_with(banner::USER_DICT)
    );
    drop(s);
    let snap: Vec<Vec<u8>> = table
        .iter()
        .map(|(f, _)| std::fs::read(d.join(f)).unwrap())
        .collect();
    let s = Store::open(&p).unwrap();
    for ((f, _), before) in table.iter().zip(&snap) {
        assert_eq!(&std::fs::read(d.join(f)).unwrap(), before, "{f}");
    }
    drop(s);
}

/// 删除一个数据方案的全部用户数据：目录与 `.backup/` 里的都删；待写回的操作一并清掉，之后
/// 写回（压实 / 维护）不会把文件写回来；别的方案不动；正在外部编辑时拒绝。
#[test]
fn purge_data_schema_removes_everything_of_that_schema() {
    let p = tmp_db("purge");
    let _ = std::fs::remove_file(&p);
    let s = Store::open(&p).unwrap();
    let d = s.user_text_dir().to_path_buf();
    s.add_user_word("wubi86", "a", "工", 1, 0).unwrap();
    s.flush_user_data().unwrap();
    s.add_user_word("wubi86", "aa", "式", 1, 0).unwrap(); // 未折回
    s.add_user_word("wubi86", "aa", "式", 9, 0).unwrap(); // 产生上一版
    s.flush_user_data().unwrap();
    s.add_user_word("wubi86", "b", "了", 1, 0).unwrap();
    s.learn_temp_word("wubi86", "c", "以", 1, 0).unwrap();
    s.pin_shadow("wubi86", "a", "工", None, 0).unwrap();
    s.create_user_dict("wubi86", "我的").unwrap();
    s.add_user_word("pinyin", "ni", "你", 1, 0b1).unwrap();
    s.flush_user_data().unwrap();
    s.learn_temp_word("wubi86", "d", "在", 1, 0).unwrap();
    assert!(d.join(".backup/wubi86").is_dir());

    let sess = s.begin_edit("wubi86/user.dict.yaml").unwrap();
    let e = s.purge_data_schema("wubi86").unwrap_err().to_string();
    assert!(e.contains("正在外部编辑"), "{e}");
    s.end_edit(&sess.path.to_string_lossy()).unwrap();

    let removed = s.purge_data_schema("wubi86").unwrap();
    for f in [
        "wubi86/user.dict.yaml",
        "wubi86/temp.dict.yaml",
        "wubi86/shadow.dict.yaml",
        "wubi86/dicts/我的.dict.yaml",
        ".backup/wubi86/user.dict.yaml",
    ] {
        assert!(removed.contains(&f.to_string()), "{f}: {removed:?}");
    }
    assert!(!d.join("wubi86").exists());
    assert!(!d.join(".backup/wubi86").exists());
    assert!(
        s.search_user_words_prefix("wubi86", "", 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(s.count_temp_words("wubi86").unwrap(), 0);
    s.flush_user_data().unwrap();
    s.runtime()
        .data()
        .maintain_text_idle(std::time::Duration::ZERO)
        .unwrap();
    assert!(!d.join("wubi86").exists(), "不写回");
    assert!(d.join("pinyin/user.dict.yaml").is_file(), "别的方案不动");
    assert!(s.purge_data_schema("../x").is_err());
    // 保留目录：统计 / 字符分类不是数据方案（大小写不敏感，Windows 上是同一个目录）
    std::fs::create_dir_all(d.join("stats")).unwrap();
    std::fs::write(d.join("stats/2026-10.toml"), "a = 1\n").unwrap();
    for ds in ["stats", "Stats", "charsets", "CharSets", ".backup"] {
        assert!(s.purge_data_schema(ds).is_err(), "{ds}");
    }
    assert!(d.join("stats/2026-10.toml").is_file());
}

/// 打开时把旧版留在数据文件旁边的 `.bak` / `.corrupt-<时间>` 挪进 `data/.backup/`。
#[test]
fn open_moves_legacy_backups_into_backup_dir() {
    let p = tmp_db("legacy_bak");
    let _ = std::fs::remove_file(&p);
    {
        let s = Store::open(&p).unwrap();
        s.add_user_word("pinyin", "nihao", "你好", 1, 0b101)
            .unwrap();
        s.flush_user_data().unwrap();
    }
    let d = derived_text_dir(&p);
    // 模拟旧版留下的数据目录：还没迁移过
    std::fs::remove_file(d.join(".backup/.migrated")).unwrap();
    std::fs::write(d.join("pinyin/user.dict.yaml.bak"), "old").unwrap();
    std::fs::write(d.join("phrases.dict.yaml.corrupt-20261016-120000"), "bad").unwrap();
    let s = Store::open(&p).unwrap();
    assert!(!d.join("pinyin/user.dict.yaml.bak").exists());
    assert!(!d.join("phrases.dict.yaml.corrupt-20261016-120000").exists());
    assert!(
        d.join(".backup/phrases.dict.yaml.corrupt-20261016-120000")
            .is_file()
    );
    assert!(d.join(".backup/pinyin/user.dict.yaml").is_file());
    assert!(!s.user_text_schemas().contains(".backup"));
    drop(s);
    // 迁移只跑一次：之后用户自己放的 `.bak`（哪怕与数据文件同名）重启后还在
    std::fs::write(d.join("x.bak"), "mine").unwrap();
    std::fs::write(d.join("pinyin/user.dict.yaml.bak"), "mine").unwrap();
    let _s = Store::open(&p).unwrap();
    assert_eq!(std::fs::read_to_string(d.join("x.bak")).unwrap(), "mine");
    assert!(d.join("pinyin/user.dict.yaml.bak").is_file());
}

/// 打开时，`data/` 下已有学到的数据文本（换机复制过来）⇒ 直接绑定、可查。
#[test]
fn learned_text_copied_from_another_machine_is_bound_at_open() {
    let p = tmp_db("learned_copy");
    let _ = std::fs::remove_file(&p);
    {
        let s = Store::open(&p).unwrap();
        s.learn_temp_word("pinyin", "nihao", "你好", 10, 0b101)
            .unwrap();
        s.flush_user_data().unwrap();
    }
    let text = derived_text_dir(&p).join("pinyin/temp.dict.yaml");
    let saved = std::fs::read(&text).unwrap();
    std::fs::remove_dir_all(derived_state_dir(&p)).unwrap();
    // 派生模式下 redb 在就不清；只删 state，文本留着
    std::fs::write(&text, &saved).unwrap();
    let s = Store::open(&p).unwrap();
    assert!(s.runtime().temp_word("pinyin", "nihao", "你好").is_some());
}

/// Review Focus 4：删掉 redb 再开同一路径 = 新库，派生目录里的旧引擎数据不得残留。
#[test]
fn fresh_redb_starts_with_empty_runtime() {
    let p = tmp_db("fresh");
    let _ = std::fs::remove_file(&p);
    let s = Store::open(&p).unwrap();
    s.runtime().learn_temp("py", "a", "甲", 1, 0).unwrap();
    drop(s);
    std::fs::remove_file(&p).unwrap();
    let s = Store::open(&p).unwrap();
    assert!(s.runtime().temp_word("py", "a", "甲").is_none());
}

#[test]
fn open_with_state_uses_the_given_dir_and_never_wipes() {
    let p = tmp_db("explicit");
    let _ = std::fs::remove_file(&p);
    let st = std::env::temp_dir().join(format!("wind_store_rt_state-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&st);
    let loc = || StateLoc {
        state_dir: st.clone(),
        dir_id: "dir-x".into(),
        text_dir: st.join(crate::DATA_DIR_NAME),
        legacy_done: false,
    };
    let s = Store::open_with_state(&p, loc()).unwrap();
    s.runtime().learn_temp("py", "a", "甲", 1, 0).unwrap();
    drop(s);
    std::fs::remove_file(&p).unwrap();
    let s = Store::open_with_state(&p, loc()).unwrap();
    assert!(
        s.runtime().temp_word("py", "a", "甲").is_some(),
        "显式状态目录不随 redb 清空"
    );
    assert!(!derived_state_dir(&p).exists());
}

/// 一次很长的 `with_db`（单事务导入、冷扫描）期间不许回收；空闲要从它**结束**时算起——
/// 否则它一结束回收线程就立刻关库重开（还会与批量写之后的立即回收撞在一起）。
/// （原在 `tests/scan_reclaim.rs`，借用户词遍历的回调制造长操作；P2 起用户词不经 redb，挪到
/// 这里直接用 `with_db`。）
#[test]
fn a_long_operation_is_not_idle_time() {
    use std::time::Duration;
    let tick = Duration::from_millis(20);
    let scan_idle = Duration::from_millis(100);
    let p = tmp_db("long_op");
    let _ = std::fs::remove_file(&p);
    let s = std::sync::Arc::new(Store::open(&p).unwrap());
    s.spawn_idle_cache_reclaimer(Duration::from_secs(3600), scan_idle, tick);
    s.mark_scan_pending();
    s.with_db(|_| {
        std::thread::sleep(scan_idle * 4);
        Ok(())
    })
    .unwrap();
    std::thread::sleep(scan_idle / 2);
    assert_eq!(
        s.page_cache_drops(),
        0,
        "长操作刚结束、还没空闲满短档，不该回收"
    );
    let t0 = std::time::Instant::now();
    while s.page_cache_drops() < 1 && t0.elapsed() < scan_idle * 20 {
        std::thread::sleep(tick / 2);
    }
    assert!(s.page_cache_drops() >= 1, "结束后空闲满短档才回收");
}
