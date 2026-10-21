//! 拼音方案里混入的英文候选（`schema.pinyin.english_merge`）被选中上屏后，记进**英文**
//! 词频桶（`"english"`）——与英文方案、临英同一个记频口，三处学到的互通。
//!
//! 开关按归属方案取：英文桶看 `schema.english.frequency`，不看拼音的调频开关。
//! 词库缺失时整族跳过（worktree 里要自备 `build_dev/data`，核对耗时）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const VK_1: u32 = 0x31;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn ready() -> bool {
    let d = data_dir();
    d.join("schemas/pinyin.schema.toml").exists() && d.join("schemas/english/en.dict.yaml").exists()
}

fn key(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn config(english_freq: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into(), "english".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.english_merge.enable = true;
    // 拼音调频关着：英文那条要记进英文桶，与拼音开关无关。
    cfg.schema.pinyin.frequency.enabled = false;
    cfg.schema.english.frequency.enabled = english_freq;
    cfg
}

fn store_at(tag: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_py_en_freq_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

/// 打 `word`，在当前页按数字键选中文本恰为 `word` 的那条（英文精确命中）。
fn type_and_pick(coord: &Coordinator, word: &str) {
    for c in word.chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    let page = coord.debug_page_texts();
    let i = page
        .iter()
        .position(|t| t == word)
        .unwrap_or_else(|| panic!("首页应有英文候选 {word}，实际 {page:?}"));
    coord.handle_key_event(&key(VK_1 + i as u32));
}

#[test]
fn merged_english_selection_records_into_english_bucket() {
    if !ready() {
        eprintln!("跳过：缺少拼音方案或英文词库");
        return;
    }
    let store = store_at("on");
    let coord =
        Coordinator::new_headless_with_store(config(true), Some(&data_dir()), store.clone());
    type_and_pick(&coord, "hello");
    assert!(
        store
            .get_freq("english", "hello", "hello")
            .unwrap()
            .is_some(),
        "拼音里选中混入的英文 hello，应记进英文词频桶"
    );
}

/// 反向对照：英文调频关着时一条都不记（开关按英文方案取）。
#[test]
fn merged_english_selection_respects_english_switch() {
    if !ready() {
        eprintln!("跳过：缺少拼音方案或英文词库");
        return;
    }
    let store = store_at("off");
    let coord =
        Coordinator::new_headless_with_store(config(false), Some(&data_dir()), store.clone());
    type_and_pick(&coord, "hello");
    assert!(
        store
            .get_freq("english", "hello", "hello")
            .unwrap()
            .is_none(),
        "英文调频关着时不得记"
    );
}
