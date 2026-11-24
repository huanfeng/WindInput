use crate::Store;
use crate::completion::CompletionKind;
use crate::quick_format::QuickFormatRecord;
use crate::stats::{DailyStats, StatsMeta};

fn db(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("wind_mp4_{name}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_dir_all(crate::store::derived_state_dir(&p));
    let _ = std::fs::remove_dir_all(crate::store::derived_text_dir(&p));
    p
}

/// 造旧库：P4 各表只在 redb 里；删掉新文件与标记，模拟升级前。
fn legacy(name: &str) -> std::path::PathBuf {
    let p = db(name);
    {
        let s = Store::open(&p).unwrap();
        let mut qf = QuickFormatRecord::default();
        qf.add_entry("my", "yyyy");
        s.legacy_put_quick_format("date", &qf).unwrap();
        s.legacy_put_common_char("槮", true).unwrap();
        s.legacy_put_common_char("的", false).unwrap();
        s.legacy_put_charset("rare", "key: rare\n").unwrap();
        s.legacy_put_completions(&[(CompletionKind::UrlHistory, "example.com".into(), 3, 9)])
            .unwrap();
        s.legacy_put_daily_stat(
            "2026-10-14",
            &DailyStats {
                chinese: 7,
                ..Default::default()
            },
        )
        .unwrap();
        s.legacy_put_stats_meta(&StatsMeta {
            total_chars: 7,
            ..Default::default()
        })
        .unwrap();
        s.legacy_put_phrase_sys_hash("h0").unwrap();
    }
    let st = crate::store::derived_state_dir(&p);
    let tx = crate::store::derived_text_dir(&p);
    for f in [
        st.join(super::MIGRATED_P4),
        tx.join("completion.table.yaml"),
        tx.join("phrases.sys_hash"),
        tx.join("quick_format.toml"),
        tx.join("common_chars.yaml"),
    ] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(tx.join("stats"));
    let _ = std::fs::remove_dir_all(tx.join("charsets"));
    p
}

/// 旧表各类数据迁成文件：快捷格式、常用字、字符集、补全、统计、meta、phrase_sys_hash。
#[test]
fn p4_tables_migrate_to_files() {
    let p = legacy("all");
    let s = Store::open(&p).unwrap();
    assert!(s.state_dir().join(super::MIGRATED_P4).is_file());
    assert_eq!(
        s.get_quick_format("date").unwrap().unwrap().added[0].id,
        "my"
    );
    assert_eq!(s.get_common_char_override("的").unwrap(), Some(false));
    assert_eq!(
        s.get_charset_doc("rare").unwrap().as_deref(),
        Some("key: rare\n")
    );
    let c = s
        .get_completion(CompletionKind::UrlHistory, "example.com")
        .unwrap()
        .unwrap();
    assert_eq!((c.count, c.last_used), (3, 9));
    assert!(s.completion_path().is_file());
    assert_eq!(s.get_daily_stat("2026-10-14").unwrap().chinese, 7);
    assert_eq!(s.get_stats_meta().unwrap().total_chars, 7);
    assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("h0"));
}

/// 用户目录里已有手写的 common_chars.yaml ⇒ 这一类跳过，不拿旧表盖掉它。
#[test]
fn p4_migration_keeps_existing_files() {
    let p = legacy("keep");
    let f = crate::store::derived_text_dir(&p).join("common_chars.yaml");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "common: [甲]\nuncommon: []\n").unwrap();
    let s = Store::open(&p).unwrap();
    // 打开时只补了说明行（banner_backfill），内容没被迁移盖掉
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        format!(
            "{}\ncommon: [甲]\nuncommon: []\n",
            wind_userdata::text::banner::COMMON_CHARS
        )
    );
    assert_eq!(s.get_common_char_override("的").unwrap(), None);
    assert_eq!(
        s.get_quick_format("date").unwrap().unwrap().added[0].id,
        "my",
        "别的类照迁"
    );
}

/// 审查 M-5（升为 Important）：统计迁移中途失败后重试，已写的月份保留、余下的补齐。
#[test]
fn partial_stats_migration_completes_on_retry() {
    let p = legacy("partial");
    {
        let s = Store::open(&p).unwrap();
        s.legacy_put_daily_stat(
            "2026-09-01",
            &DailyStats {
                chinese: 3,
                ..Default::default()
            },
        )
        .unwrap();
    }
    let st = crate::store::derived_state_dir(&p);
    let _ = std::fs::remove_file(st.join(super::MIGRATED_P4));
    let stats = crate::store::derived_text_dir(&p).join("stats");
    let _ = std::fs::remove_dir_all(&stats);
    // 上一轮只写成了 10 月（且本地这天已有新数据）
    std::fs::create_dir_all(&stats).unwrap();
    std::fs::write(
        stats.join("2026-10.toml"),
        "version = 1\n[days.2026-10-14]\nchinese = 99\n",
    )
    .unwrap();
    let s = Store::open(&p).unwrap();
    assert_eq!(
        s.get_daily_stat("2026-09-01").unwrap().chinese,
        3,
        "余下的月份补齐"
    );
    assert_eq!(
        s.get_daily_stat("2026-10-14").unwrap().chinese,
        99,
        "已有的日以文件为准"
    );
}

/// 审查 I-3：重试时已有的统计元数据（运行期间累加过）不被旧表倒回。
#[test]
fn retry_does_not_roll_back_existing_stats_meta() {
    let p = legacy("meta_retry");
    {
        let s = Store::open(&p).unwrap();
        s.legacy_put_stats_meta(&crate::stats::StatsMeta {
            total_chars: 7,
            ..Default::default()
        })
        .unwrap();
    }
    let st = crate::store::derived_state_dir(&p);
    let _ = std::fs::remove_file(st.join(super::MIGRATED_P4));
    let stats = crate::store::derived_text_dir(&p).join("stats");
    std::fs::create_dir_all(&stats).unwrap();
    std::fs::write(stats.join("meta.toml"), "total_chars = 500\n").unwrap();
    let s = Store::open(&p).unwrap();
    assert_eq!(s.get_stats_meta().unwrap().total_chars, 500);
}
