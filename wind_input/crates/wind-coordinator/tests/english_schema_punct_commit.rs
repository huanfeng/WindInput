//! 英文方案的标点顶屏**恒允许**，不读码表的 `punct_commit`。
//!
//! 现场：标点顶码开关的判据按引擎类型分流，拼音恒允许、其余一律读有效码表配置
//! `engine.codetable.punct_commit`（出厂 false）⇒ 英文方案打 `hello` 按 `.` 被吞掉、
//! 编码原样留着。那个开关是码表「标点顶字」的方案属性，英文词后接标点是最基本的用法，
//! 与它无关。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

const VK_PERIOD: u32 = 0xBE;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
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

#[test]
fn english_schema_punct_commits_regardless_of_codetable_switch() {
    if !data_dir().join("schemas/english.schema.toml").exists() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = Config::default();
    c.schema.available = vec!["english".into(), "wubi86".into()];
    c.schema.active = "english".into();
    c.input.default.chinese_mode = true;
    c.schema.codetable.punct_commit = false;
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    for ch in "hello".chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
    assert!(
        !coord.debug_page_texts().is_empty(),
        "前提：英文方案 `hello` 有候选"
    );
    match coord.handle_key_event(&key(VK_PERIOD)) {
        // 英文方案下标点按英文语义出半角（与英文方案无编码时按 `.` 同一条流水线）。
        KeyAction::InsertText { text, .. } => {
            assert_eq!(text, "hello.", "应顶屏「hello」并接上标点")
        }
        other => panic!("英文方案按 `.` 应顶屏上屏，实际: {other:?}"),
    }
}
