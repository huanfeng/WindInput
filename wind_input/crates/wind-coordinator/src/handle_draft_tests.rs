use crate::coordinator::Coordinator;
use std::sync::Arc;
use wind_config::config::Config;
use wind_store::Store;

fn cfg_with_ttl(hours: u32) -> Config {
    let mut cfg = Config::default();
    cfg.schema.codetable.auto_phrase.draft_ttl_hours = hours;
    cfg
}

/// 带一个真 store 的无头协调器（`new_headless` 的 store 恒为 `None`，
/// 用它测不到启动清理真的走进了存储层）。
fn coord_with_store(tag: &str, hours: u32) -> (Arc<Coordinator>, Arc<Store>, std::path::PathBuf) {
    let db = std::env::temp_dir().join(format!("wind_draft_start_{tag}.redb"));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let c = Coordinator::new_headless_with_store(cfg_with_ttl(hours), None, Arc::clone(&store));
    (c, store, db)
}

/// 中英交界补的前缀空格不是用户输入：喂草稿流前剥掉。不剥的话草稿窗口见到非汉字就
/// 断流并丢掉整笔，`Python` 之后打的第一个字永远进不了草稿。
#[test]
fn auto_space_prefix_is_stripped_before_feeding_the_draft() {
    let mut cfg = cfg_with_ttl(24);
    cfg.schema.codetable.auto_phrase.enabled = true;
    let c = Coordinator::new_headless(cfg, None);
    assert!(c.draft_enabled(), "前提：默认方案应启用码表造词");
    let feed = |prefixed: bool| {
        c.draft_window.lock().unwrap().terminate();
        c.auto_space_prefixed
            .store(prefixed, std::sync::atomic::Ordering::Relaxed);
        c.note_draft_commit(&Coordinator::commit_action(" 你".into(), true));
        !c.draft_window.lock().unwrap().is_empty()
    };
    assert!(feed(true), "补的空格应被剥掉，「你」进草稿窗口");
    assert!(!feed(false), "用户自己打的空格照旧断流");
}

/// 单位换算：配置里是小时，存储层的 `ttl_secs` 要的是秒。
///
/// 漏掉 ×3600 不报错，只会让草稿 24 秒就过期——开关开着却永远用不上草稿；行为上难察觉，
/// 故直接钉住换算本身。
#[test]
fn draft_ttl_is_hours_in_config_but_seconds_at_the_store() {
    let c = Coordinator::new_headless(cfg_with_ttl(24), None);
    assert_eq!(c.draft_ttl_secs(), 86_400);
    let c = Coordinator::new_headless(cfg_with_ttl(1), None);
    assert_eq!(c.draft_ttl_secs(), 3_600);
    // 0 = 永不过期，启动清理据此整个跳过。
    let c = Coordinator::new_headless(cfg_with_ttl(0), None);
    assert_eq!(c.draft_ttl_secs(), 0);
}

/// 启动清理不能误杀还没过期的草稿。它跑在启动线程上、没有任何 UI 反馈，
/// 错删的表现是「昨天记下的词今天一条也召不回」，用户无从分辨是没记住还是被清了。
#[test]
fn startup_purge_keeps_drafts_that_are_still_fresh() {
    let (c, store, db) = coord_with_store("fresh", 24);
    store
        .add_drafts("wubi86", &[("aaaa".into(), "刚记的".into())])
        .unwrap();
    c.purge_drafts_on_start();
    assert_eq!(
        store.search_drafts("wubi86", "aaaa", 0).unwrap(),
        vec!["刚记的"],
        "还在有效期内的草稿不该被启动清理带走"
    );
    let _ = std::fs::remove_file(&db);
}

/// 两条早退：`TTL = 0`（永不过期）与没有 store 的宿主（测试 / 移动端裸构造）。
/// 它跑在启动线程上，这两条是它不会在那里 panic 的全部依据。
#[test]
fn startup_purge_is_a_quiet_noop_without_a_ttl_or_a_store() {
    let (c, store, db) = coord_with_store("nottl", 0);
    store
        .add_drafts("wubi86", &[("aaaa".into(), "刚记的".into())])
        .unwrap();
    c.purge_drafts_on_start();
    assert_eq!(
        store.count_drafts("wubi86").unwrap(),
        1,
        "TTL=0 ＝ 永不过期，一条都不该动"
    );
    let _ = std::fs::remove_file(&db);
    // store 为 None：只要不 panic 即可。
    Coordinator::new_headless(cfg_with_ttl(24), None).purge_drafts_on_start();
}

/// 顶码也算「用过」：顶码上屏不经选择键，但词确实上屏了；少了这一跳，顶着草稿上屏的人
/// 永远打不出自己造的词（如双拼 `yivi` 空码造出「一直」，再打 `yivi` 由顶码上屏）。
///
/// 钉的是出口接线（`commit_top_text` 对 `promote_draft_on_commit` 的调用）：lib 单测无词库、
/// 造不出真候选，故直接传 `draft_boundary: Some(_)` 模拟「顶出的首选是草稿候选」。
#[test]
fn auto_committed_top_candidate_promotes_the_draft() {
    let db = std::env::temp_dir().join("wind_draft_auto_commit_promote.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let mut cfg = cfg_with_ttl(24);
    cfg.schema.codetable.auto_phrase.enabled = true;
    let c = Coordinator::new_headless_with_store(cfg, None, Arc::clone(&store));
    // 与 `promote_draft_on_commit` 内部同一条归因规则：草稿存进哪个方案库，转正就得
    // 从哪个方案库取——两边写死不同的字符串，测试会以「没转正」的形态失败。
    let schema = c
        .engine_mgr
        .write_data_schema_id(
            &c.engine_mgr.active_schema_id(),
            wind_candidate::CandidateSource::CodeTable,
        )
        .expect("默认配置应能归因出写入方案");
    store
        .add_drafts(&schema, &[("yivi".into(), "一直".into())])
        .unwrap();
    assert_eq!(
        store.search_drafts(&schema, "yivi", 0).unwrap(),
        vec!["一直"],
        "前置：草稿应在库里"
    );

    let action = {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "yivi".into();
        st.input_buffer_cased = "yivi".into();
        st.preedit = "yivi".into();
        c.commit_top_text(
            &mut st,
            "yivi",
            "一直".into(),
            None,
            None,
            "",
            wind_candidate::CandidateSource::CodeTable,
            Some(0),
        )
    };
    assert!(
        matches!(action, wind_bridge::handler::KeyAction::InsertText { .. }),
        "无余码时顶码该插入文本，实际: {action:?}"
    );
    assert!(
        store.search_drafts(&schema, "yivi", 0).unwrap().is_empty(),
        "自动上屏后草稿应已跃迁离开草稿层"
    );
    assert_eq!(
        store.get_temp_word(&schema, "yivi", "一直").unwrap(),
        Some(1),
        "跃迁记 count=1；若记成 2 说明「6b 临时词使用累积」没被跳过，晋升进度会快一倍"
    );
    let _ = std::fs::remove_file(&db);
}

/// 满码自动上屏 / 空码顶出同样算「用过」：两条 `AutoCommit` 消费点都经过
/// `promote_draft_on_auto_commit`，本条钉该 helper 的判据（按上屏文本找到草稿候选）。
///
/// 不跑完整 `AutoCommit` 流程（需真词库与唯一码判定），只在 `state.candidates` 放一条
/// 文本与本次上屏一致的 `is_draft` 候选，直接调 helper。
#[test]
fn auto_commit_outcome_promotes_the_draft_candidate() {
    let db = std::env::temp_dir().join("wind_draft_auto_commit_helper.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let mut cfg = cfg_with_ttl(24);
    cfg.schema.codetable.auto_phrase.enabled = true;
    let c = Coordinator::new_headless_with_store(cfg, None, Arc::clone(&store));
    let schema = c
        .engine_mgr
        .write_data_schema_id(
            &c.engine_mgr.active_schema_id(),
            wind_candidate::CandidateSource::CodeTable,
        )
        .expect("默认配置应能归因出写入方案");
    store
        .add_drafts(&schema, &[("aaaa".into(), "自动的".into())])
        .unwrap();
    {
        let mut st = c.state.lock().unwrap();
        st.candidates = vec![wind_candidate::Candidate {
            text: "自动的".into(),
            code: "aaaa".into(),
            is_draft: true,
            ..Default::default()
        }];
        c.promote_draft_on_auto_commit(&st, "自动的", "aaaa");
    }
    assert!(
        store.search_drafts(&schema, "aaaa", 0).unwrap().is_empty(),
        "自动上屏后草稿应已跃迁离开草稿层"
    );
    assert_eq!(
        store.get_temp_word(&schema, "aaaa", "自动的").unwrap(),
        Some(1),
        "跃迁记 count=1；若记成 2 说明同一次上屏被记了两遍"
    );
    let _ = std::fs::remove_file(&db);
}
