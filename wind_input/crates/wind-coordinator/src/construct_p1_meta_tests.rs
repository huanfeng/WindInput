use super::*;

/// Review Focus 3：meta 为 1 的目录被新版打开 ⇒ 迁移后升到最新（认识 1 的旧版本随后只读）。
#[test]
fn opening_migrates_and_raises_format_version() {
    let d = std::env::temp_dir().join(format!("wind_raise_fv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("meta.toml"), "format_version = 1\ndir_id = \"d1\"\n").unwrap();
    let (store, reason) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    assert!(store.is_some() && reason.is_none());
    let text = std::fs::read_to_string(d.join("meta.toml")).unwrap();
    assert!(text.contains("format_version = 3"), "{text}");
    assert!(text.contains("d1"), "dir_id 保留: {text}");
    let state = wind_store::store::derived_state_dir(&d.join("userdata.redb"));
    assert!(state.join(wind_store::migrate_p1::MIGRATED_P1).is_file());
    drop(store);
    let _ = std::fs::remove_dir_all(&d);
}

/// 造一个「老版本写的」用户目录：meta 为 1，redb 旧表里有一条拼音临时词。
fn legacy_user_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_p1fix_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("meta.toml"), "format_version = 1\ndir_id = \"d1\"\n").unwrap();
    let p = d.join("userdata.redb");
    let s = Store::open(&p).unwrap();
    s.legacy_put_temp("py", "nihao", "你好", 800, 3, 1_700_000_000, 0b101)
        .unwrap();
    drop(s);
    std::fs::remove_dir_all(wind_store::store::derived_state_dir(&p)).unwrap();
    d
}

fn meta_version(d: &std::path::Path) -> u32 {
    let t = std::fs::read_to_string(d.join("meta.toml")).unwrap();
    toml::from_str::<wind_config::user_meta::UserMeta>(&t)
        .unwrap()
        .format_version
}

/// §7.4：旧库各表迁完 ⇒ format_version 5、旧库改名 `.legacy`、`migrated-*` 标记删除；
/// 再开不打开旧库、不重迁，数据都在。
#[test]
fn legacy_dir_reaches_final_format_and_retires_redb() {
    let d = legacy_user_dir("retire");
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    let s = store.unwrap();
    s.migrate_phrases_now().unwrap();
    crate::construct::finish_format_upgrade(&s, &d);
    drop(s);
    assert_eq!(
        meta_version(&d),
        wind_config::user_meta::KNOWN_FORMAT_VERSION
    );
    assert!(d.join("userdata.redb.legacy").is_file());
    assert!(!d.join("userdata.redb").exists());
    let state = wind_store::store::derived_state_dir(&d.join("userdata.redb"));
    assert!(
        wind_store::migrate_p3::all_migration_markers()
            .iter()
            .all(|m| !state.join(m).exists()),
        "迁完不再留 migrated-* 标记"
    );
    assert!(
        d.join("data/py/temp.dict.yaml").is_file(),
        "学到的数据落成文本"
    );
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    let s = store.unwrap();
    assert!(s.runtime().temp_word("py", "nihao", "你好").is_some());
    assert!(!d.join("userdata.redb").exists(), "不再新建 redb");
    drop(s);
    let _ = std::fs::remove_dir_all(&d);
}

/// P5 退出判据：删掉整个本机状态目录后重启，学到的数据、补全、统计全在。
#[test]
fn wiping_state_dir_keeps_all_user_data() {
    let d = std::env::temp_dir().join(format!("wind_p5_wipe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    {
        let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
        let s = store.unwrap();
        s.learn_temp_word("pinyin", "nihao", "你好", 10, 0b101)
            .unwrap();
        s.record_freq("pinyin", "nihao", "你好").unwrap();
        s.record_assoc_pick("pinyin", "我们", "我们的").unwrap();
        s.record_completion(
            wind_store::completion::CompletionKind::EmailSuffix,
            "qq.com",
        )
        .unwrap();
        s.put_daily_stat(
            "2026-10-15",
            &wind_store::stats::DailyStats {
                chinese: 5,
                ..Default::default()
            },
        )
        .unwrap();
        s.flush_user_data().unwrap();
    }
    assert_eq!(
        meta_version(&d),
        wind_config::user_meta::KNOWN_FORMAT_VERSION,
        "全新目录直接是最新格式"
    );
    std::fs::remove_dir_all(wind_store::store::derived_state_dir(
        &d.join("userdata.redb"),
    ))
    .unwrap();
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    let s = store.unwrap();
    assert!(s.runtime().temp_word("pinyin", "nihao", "你好").is_some());
    assert_eq!(
        s.get_freq("pinyin", "nihao", "你好")
            .unwrap()
            .unwrap()
            .count,
        1
    );
    assert!(!s.runtime().assoc_history("pinyin", "我们", 10).is_empty());
    assert!(
        s.get_completion(
            wind_store::completion::CompletionKind::EmailSuffix,
            "qq.com"
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(s.get_daily_stat("2026-10-15").unwrap().chinese, 5);
    drop(s);
    let _ = std::fs::remove_dir_all(&d);
}

/// 审查 I3：迁移失败时不得升 format_version（旧版本回退后仍可写、旧表仍是完整的真源）。
#[test]
fn failed_migration_does_not_raise_format_version() {
    let d = legacy_user_dir("noraise");
    let state = wind_store::store::derived_state_dir(&d.join("userdata.redb"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("py"), b"x").unwrap(); // 拼音的状态子目录被占 ⇒ 迁移失败
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    assert!(store.is_some(), "迁移失败不阻断启动");
    drop(store);
    let text = std::fs::read_to_string(d.join("meta.toml")).unwrap();
    assert!(
        text.contains("format_version = 1"),
        "迁移没成功就不升版本: {text}"
    );
    std::fs::remove_file(state.join("py")).unwrap();
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    drop(store);
    let text = std::fs::read_to_string(d.join("meta.toml")).unwrap();
    assert!(text.contains("format_version = 3"), "迁移成功后升: {text}");
    let _ = std::fs::remove_dir_all(&d);
}

/// P1、P2 都迁完才升到 3；只迁完 P1 停在 2，P2 迁完后再升。
#[test]
fn format_version_follows_migration_progress() {
    let d = legacy_user_dir("p2progress");
    let p = d.join("userdata.redb");
    {
        let s = Store::open(&p).unwrap();
        s.legacy_put_user("pinyin", "nihao", "你好", 9, 0b101, 1)
            .unwrap();
    }
    std::fs::remove_dir_all(wind_store::store::derived_state_dir(&p)).unwrap();
    // 用户词文本的路径被一个目录占住 ⇒ P2 写不出文本、迁移失败；P1（学到的数据写同目录下
    // 别的文件）照常
    let blocker = d
        .join(wind_store::DATA_DIR_NAME)
        .join("pinyin")
        .join("user.dict.yaml");
    std::fs::create_dir_all(&blocker).unwrap();
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    drop(store);
    let text = std::fs::read_to_string(d.join("meta.toml")).unwrap();
    assert!(text.contains("format_version = 2"), "只迁完 P1: {text}");
    std::fs::remove_dir_all(&blocker).unwrap();
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    let s = store.unwrap();
    assert!(s.migrated_p2());
    assert_eq!(s.get_user_words("pinyin", "nihao").unwrap()[0].text, "你好");
    drop(s);
    let text = std::fs::read_to_string(d.join("meta.toml")).unwrap();
    assert!(text.contains("format_version = 3"), "P2 迁完: {text}");
    let _ = std::fs::remove_dir_all(&d);
}

/// P2b 审查 M8：派生模式（移动端、测试）的用户词文本同样在 `<用户目录>/user/`；redb 不在时
/// 只清派生状态目录，真源文本不动、重开即读回。
#[test]
fn derived_mode_keeps_user_text_in_user_dir() {
    let d = std::env::temp_dir().join(format!("wind_p2b_m8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    let s = store.unwrap();
    s.add_user_word("wb", "a", "工", 7, 0).unwrap();
    s.flush_user_data().unwrap();
    assert!(
        d.join(wind_store::DATA_DIR_NAME)
            .join("wb")
            .join("user.dict.yaml")
            .is_file()
    );
    drop(s);
    // 全新目录本就不建 redb；有的话也删掉——两种情形文本都要原样读回
    let _ = std::fs::remove_file(d.join("userdata.redb"));
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    let s = store.unwrap();
    assert_eq!(s.get_user_words("wb", "a").unwrap()[0].weight, 7);
    drop(s);
    let _ = std::fs::remove_dir_all(&d);
}

/// 全新目录（meta 直接是最新格式）根本不建 redb：派生模式不能因「redb 不在」就把它当新库、
/// 每次打开都清掉运行时状态（移动端走派生模式，那等于每次启动丢掉学到的词）。
#[test]
fn derived_mode_fresh_dir_keeps_learned_data_across_reopen() {
    let d = std::env::temp_dir().join(format!("wind_fresh_keep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    let s = store.unwrap();
    assert!(!d.join("userdata.redb").exists(), "全新目录不建旧库");
    s.learn_temp_word("py", "ni", "你", 1, 0).unwrap();
    drop(s);
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    let s = store.unwrap();
    assert!(
        !s.get_temp_words("py", "ni").unwrap().is_empty(),
        "重开后学到的词还在"
    );
    drop(s);
    let _ = std::fs::remove_dir_all(&d);
}

/// P3 迁完（短语迁移要等 set_system_phrases）后升到最新（P4a 在打开时已迁完 ⇒ 5）。
#[test]
fn format_version_reaches_latest_after_phrase_migration() {
    let d = legacy_user_dir("p3fv");
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    let s = store.unwrap();
    assert!(!s.migrated_p3(), "短语迁移要等系统短语");
    s.set_system_phrases(&[]).unwrap();
    assert!(s.migrated_p3());
    assert_eq!(
        s.format_version_reached(),
        Some(wind_config::user_meta::KNOWN_FORMAT_VERSION)
    );
    drop(s);
    let (store, _) = Coordinator::open_user_store_checked(Some(&d), StateChoice::Derived);
    drop(store);
    assert_eq!(
        meta_version(&d),
        wind_config::user_meta::KNOWN_FORMAT_VERSION
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// 审查 I2：运行时状态按用户目录（dir_id）分开——换目录后新目录的旧表照常迁移，
/// 旧目录学到的词不混进来。
#[test]
fn runtime_state_is_per_user_dir() {
    let base = std::env::temp_dir().join(format!("wind_p1fix_base-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let a = std::env::temp_dir().join(format!("wind_p1fix_a-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&a);
    let sa = Store::open_with_state(
        a.join("userdata.redb"),
        runtime_state_loc(&base, "dir-A", &a, false),
    )
    .unwrap();
    sa.learn_temp_word("py", "jiade", "甲的", 800, 0).unwrap();
    drop(sa);
    let b = legacy_user_dir("perdir");
    let sb = Store::open_with_state(
        b.join("userdata.redb"),
        runtime_state_loc(&base, "dir-B", &b, false),
    )
    .unwrap();
    assert!(sb.migrated_p1());
    assert_eq!(
        sb.get_temp_word("py", "nihao", "你好").unwrap(),
        Some(3),
        "B 的旧表迁入"
    );
    assert!(
        sb.get_temp_word("py", "jiade", "甲的").unwrap().is_none(),
        "A 的词不混进 B"
    );
    drop(sb);
    for d in [&base, &a, &b] {
        let _ = std::fs::remove_dir_all(d);
    }
}
