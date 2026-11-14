//! 快捷输入 ⑥ 标点顶屏（`free_input = off` 才走到）也是一次选中：记词频、上屏历史、输入统计。
//!
//! 现场：`;nihao` 按 `，` 得到「你好，」，但高亮候选没经 `record_selection_cand_in`
//! ⇒ 词频不涨、上屏历史没有它（`quick_input.repeat` 重复不出来），统计只由顶层兜底按
//! 「候选」来源粗记。对照临英 ⑥（`handle_temp.rs`）与主输入路 `commit_highlight_then_char`：
//! 两处都记账。
//!
//! 主方案取五笔：词频若按活跃方案归属会落 `wubi86` 桶，断言查的是 `pinyin` 桶。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::FreeInputMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;
use wind_store::stats::CommitSource;

const VK_SEMICOLON: u32 = 0xBA;
const VK_COMMA: u32 = 0xBC;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("schemas/pinyin.schema.toml").exists()
}

fn key(k: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn open(tag: &str) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.pinyin.frequency.enabled = true;
    c.schema.mix_modes[0].free_input = FreeInputMode::Off;
    let db = std::env::temp_dir().join(format!(
        "wind_quick_punct_acct_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    (coord, store, db)
}

/// `;nihao` 后按 `,` 顶屏，返回上屏文本。
fn punct_commit(coord: &Coordinator) -> String {
    coord.handle_key_event(&key(VK_SEMICOLON));
    for ch in "nihao".chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("你好"),
        "前提：`;nihao` 首选应是「你好」，实际: {page:?}"
    );
    match coord.handle_key_event(&key(VK_COMMA)) {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("`,` 应顶屏上屏，实际: {other:?}"),
    }
}

#[test]
fn punct_commit_records_freq_in_member_bucket() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, db) = open("freq");
    let out = punct_commit(&coord);
    assert!(out.starts_with("你好"), "前提：顶屏含「你好」: {out}");
    assert!(
        store.get_freq("pinyin", "nihao", "你好").unwrap().is_some(),
        "⑥ 顶屏应按成员方案记词频"
    );
    assert!(
        store.get_freq("wubi86", "nihao", "你好").unwrap().is_none(),
        "词频不得记进活跃方案桶"
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn punct_commit_records_stats_on_its_own_path() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, _store, db) = open("stats");
    coord.debug_capture_stat_events();
    punct_commit(&coord);
    // 候选段按 `Mix`、标点按 `Punctuation` 各记一笔（逐键打码那几下不产生上屏事件）。
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![
            (CommitSource::Mix, "你好".to_string()),
            (CommitSource::Punctuation, "，".to_string()),
        ],
        "⑥ 顶屏应记「候选 + 标点」两笔统计"
    );
    // 非 policed 入口不跑顶层兜底：置位只可能来自 ⑥ 自己的 `record_commit`。
    assert!(
        coord.debug_stat_recorded(),
        "⑥ 顶屏应在本路径记输入统计（不靠顶层兜底）"
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn punct_commit_enters_commit_history() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, _store, db) = open("history");
    punct_commit(&coord);
    // 再进快捷输入、空缓冲：`quick_input.repeat` 取的正是上屏历史的最新一条。
    coord.handle_key_event(&key(VK_SEMICOLON));
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("你好"),
        "⑥ 顶屏的候选应进上屏历史（重复上屏取得到）"
    );
    let _ = std::fs::remove_file(&db);
}

/// 无高亮候选时按 `,`：标点同样显式按 `Punctuation` 记，不交给顶层兜底。
///
/// 出厂成员里英文成员恒给出「所打原文」候选，⑥ 几乎总有高亮；空缓冲按 `,` 又会作字面
/// 进缓冲、到不了 ⑥。故去掉英文成员，打一串拼音成员给不出候选的 `vvvvvvvv` 造出这一支。
#[test]
fn punct_without_candidate_records_punctuation() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.mix_modes[0].free_input = FreeInputMode::Off;
    c.schema.mix_modes[0].members.retain(|m| m != "english");
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    coord.handle_key_event(&key(VK_SEMICOLON));
    for ch in "vvvvvvvv".chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提：无英文成员时 `vvvvvvvv` 不应有候选"
    );
    coord.debug_capture_stat_events();
    match coord.handle_key_event(&key(VK_COMMA)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "，", "前提：上屏中文逗号"),
        other => panic!("`,` 应上屏标点，实际: {other:?}"),
    }
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![(CommitSource::Punctuation, "，".to_string())],
        "无候选时标点也应在本路径按 Punctuation 记"
    );
}
