//! 暂停学习（隐私 / 无痕输入，`Coordinator::set_learning_suspended`）：暂停期间照常出候选、
//! 照常上屏，但词频、联想下一词、输入统计、码表自动造词（草稿层）一概不写；恢复后同样的
//! 输入照常学习。
//!
//! 恢复后那一半是**对照组**：同一个协调器、同一串按键，证明这几条学习通路在本夹具配置下
//! 确实会写——否则「暂停时没写」可能只是配置没开，断言形同虚设。
//!
//! 夹具：wubi86，`trwu` =「我们」、`ggfh` =「一直」、`xnn` =「幻」、`smq` =「枫」
//! （「幻」「枫」连打产出草稿 `xnsm → 幻枫`，见 `draft_recall_without_terminator.rs`）。
//! 词典缺失时自动跳过（`build_dev/data` 不存在时静默跳过）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_coordinator::web_host::WebDataHost;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
}

fn press(c: &Coordinator, vk: u32) -> KeyAction {
    // 走 `_policed`：上屏后处理（草稿流喂入）挂在这一层，与桌面真实按键链一致。
    c.handle_key_event_policed(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    })
}

/// 打码 + 空格上屏首选，返回上屏文本。
fn commit_word(c: &Coordinator, code: &str) -> String {
    for ch in code.chars() {
        press(c, (ch.to_ascii_uppercase() as u32) & 0xFF);
    }
    match press(c, 0x20) {
        KeyAction::InsertText { text, .. } => text,
        KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text,
        other => panic!("空格应上屏，实际: {other:?}"),
    }
}

fn setup(tag: &str) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let db = std::env::temp_dir().join(format!(
        "wind_learning_suspended_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    // 下一词只在智能联想档学（`learns_next_words`）。
    cfg.input.association.kind = "smart".to_string();
    cfg.input.association.mode = "continuous".to_string();
    cfg.schema.codetable.auto_commit_at_full = false;
    // `Config::default()` 不读 data/config.toml：码表调频与自动造词出厂都关，显式打开，
    // 对照组才写得出东西。
    cfg.schema.codetable.frequency.enabled = true;
    cfg.schema.codetable.auto_phrase.enabled = true;
    cfg.stats.enabled = true;
    let c = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store.clone());
    c.prewarm_indexes();
    (c, store, db)
}

/// 同一串输入：「我们一直」（词频 + 下一词）+「幻」「枫」（草稿）。
/// 顺带断言暂停与否都正常出候选、正常上屏（读路径不受影响）。
fn type_session(c: &Coordinator) {
    assert_eq!(commit_word(c, "trwu"), "我们", "前提：trwu 首选「我们」");
    assert_eq!(commit_word(c, "ggfh"), "一直", "前提：ggfh 首选「一直」");
    assert_eq!(commit_word(c, "xnn"), "幻", "前提：xnn 首选「幻」");
    assert_eq!(commit_word(c, "smq"), "枫", "前提：smq 首选「枫」");
}

fn freq_of_women(store: &Store) -> u32 {
    store
        .get_freq("wubi86", "trwu", "我们")
        .unwrap()
        .map_or(0, |r| r.count)
}

fn next_word_learned(store: &Store) -> bool {
    store
        .assoc_history("wubi86", "我们", 0)
        .unwrap()
        .iter()
        .any(|(w, _)| w == "一直")
}

fn today_chars(c: &Coordinator) -> u32 {
    c.stat_collector()
        .expect("注入了 store 就应有统计采集器")
        .get_today_stat()
        .total()
}

/// 草稿落库在后台线程上：轮询到 `deadline` 为止。
fn draft_xnsm(store: &Store, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if !store
            .search_drafts("wubi86", "xnsm", 0)
            .unwrap_or_default()
            .is_empty()
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn suspended_learns_nothing_and_resume_learns_again() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (c, store, db) = setup("main");
    assert!(!c.learning_suspended(), "默认不暂停");

    // ── 暂停：打字上屏照常，但什么都不写 ──
    c.set_learning_suspended(true);
    assert!(c.learning_suspended());
    type_session(&c);
    // 断流冲一次草稿队列：若有东西入了队，这一下就会落库，下面的等待才有意义。
    c.handle_focus_lost(1, wind_bridge::handler::FocusLostReason::Thread);
    assert_eq!(freq_of_women(&store), 0, "暂停期间不记词频");
    assert!(!next_word_learned(&store), "暂停期间不学下一词");
    assert_eq!(today_chars(&c), 0, "暂停期间不涨输入统计");
    assert!(
        !draft_xnsm(&store, Duration::from_millis(800)),
        "暂停期间不自动造词（草稿层）"
    );

    // ── 恢复（对照组）：同样的输入，四条通路都写 ──
    c.set_learning_suspended(false);
    assert!(!c.learning_suspended());
    type_session(&c);
    assert_eq!(freq_of_women(&store), 1, "恢复后选词记词频");
    assert!(next_word_learned(&store), "恢复后学「我们 → 一直」");
    assert!(today_chars(&c) > 0, "恢复后输入统计照涨");
    assert!(
        draft_xnsm(&store, Duration::from_secs(5)),
        "恢复后「幻」「枫」连打产出草稿 xnsm → 幻枫"
    );

    drop(c);
    let _ = std::fs::remove_file(&db);
}

/// 暂停只挡「学习」，不挡用户的显式操作：暂停期间经词库接口手动加的词照常入库。
#[test]
fn suspended_still_allows_explicit_user_word_add() {
    if !dict_ready() {
        return;
    }
    let (c, store, db) = setup("explicit");
    c.set_learning_suspended(true);
    wind_coordinator::user_words_api::add(&*c, "wubi86", "zzzz", "清风测词", 100).unwrap();
    assert!(
        store
            .get_user_words("wubi86", "zzzz")
            .unwrap()
            .iter()
            .any(|r| r.text == "清风测词"),
        "暂停期间手动加词仍应写入用户词库"
    );
    drop(c);
    let _ = std::fs::remove_file(&db);
}
