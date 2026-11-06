//! 主路（拼音 / 码表 / 英文方案）无候选按标点、`punct_on_empty_behavior = "commit"` 时，
//! 输入统计与回车上屏原码同口径：原码记一笔（来源 `RawInput`、码长 = 原码长，已转换前缀
//! 选词时已记过、不重复），标点另记一笔 `Punctuation`。普通出口与智能符号 hold 出口都覆盖；
//! `clear` / `clear_no_input` 不上屏原码，也不记原码。
//!
//! 现场：两个出口只记了标点，上屏的原码在统计里消失——临拼 / 快捷输入 / 临英同状态都记了。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::stats::CommitSource;

const VK_COMMA: u32 = 0xBC;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
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

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn open(active: &str, policy: &str, edit: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec![active.into()];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.input.punct_on_empty_behavior = policy.into();
    // 混输 / 码表的标点顶屏开关出厂关；本文件测的是标点出口本身。
    c.schema.codetable.punct_commit = true;
    c.schema.english.raw_candidate = wind_config::config::RawCandidateMode::Off;
    edit(&mut c);
    Coordinator::new_headless(c, Some(&data_dir()))
}

/// 打 `code`（须无候选）后按 `,`，返回（上屏文本，统计事件）。
fn comma_events(coord: &Coordinator, code: &str) -> (String, Vec<(CommitSource, String)>) {
    type_str(coord, code);
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提：`{code}` 无候选，实际: {:?}",
        coord.debug_page_texts()
    );
    coord.debug_capture_stat_events();
    let text = match coord.handle_key_event(&key(VK_COMMA)) {
        KeyAction::InsertText { text, .. } => text,
        KeyAction::CommitAndHoldComposition {
            commit_text,
            hold_text,
            ..
        } => format!("{commit_text}{hold_text}"),
        KeyAction::ClearComposition => String::new(),
        other => panic!("应上屏或清空，实际: {other:?}"),
    };
    (text, coord.debug_take_stat_events())
}

fn cases() -> [(&'static str, &'static str, &'static str); 3] {
    // (方案, 无候选的码, 该方案下 `,` 的产物)
    [
        ("pinyin", "vvv", "，"),
        ("wubi86", "vvvx", "，"),
        ("english", "qzxv", ","),
    ]
}

#[test]
fn commit_policy_records_raw_then_punct() {
    skip_without_data!();
    for (schema, code, punct) in cases() {
        let coord = open(schema, "commit", |_| {});
        let (text, events) = comma_events(&coord, code);
        assert_eq!(text, format!("{code}{punct}"), "{schema}：前提");
        assert_eq!(
            events,
            vec![
                (CommitSource::RawInput, code.to_string()),
                (CommitSource::Punctuation, punct.to_string()),
            ],
            "{schema}：原码同回车口径记一笔，标点另记"
        );
    }
}

/// 智能符号 hold 出口（`CommitAndHoldComposition`）同口径。
#[test]
fn commit_policy_hold_exit_records_raw_then_punct() {
    skip_without_data!();
    for (schema, code, punct) in cases() {
        if schema == "english" {
            continue; // 英文方案出英文标点，不参与中文智能符号集合
        }
        let coord = open(schema, "commit", |c| {
            c.input.symbol.smart_mode = true;
            c.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
        });
        let (text, events) = comma_events(&coord, code);
        assert_eq!(text, format!("{code}{punct}"), "{schema}：前提");
        assert_eq!(
            events,
            vec![
                (CommitSource::RawInput, code.to_string()),
                (CommitSource::Punctuation, punct.to_string()),
            ],
            "{schema}：hold 出口同口径"
        );
    }
}

/// `clear` 只出标点、只记标点；`clear_no_input` 什么都不出、什么都不记。
#[test]
fn clear_policies_record_no_raw() {
    skip_without_data!();
    for (schema, code, punct) in cases() {
        let coord = open(schema, "clear", |_| {});
        let (text, events) = comma_events(&coord, code);
        assert_eq!(text, punct, "{schema}：clear 前提");
        assert_eq!(
            events,
            vec![(CommitSource::Punctuation, punct.to_string())],
            "{schema}：clear 不记原码"
        );
        let coord = open(schema, "clear_no_input", |_| {});
        let (text, events) = comma_events(&coord, code);
        assert!(text.is_empty(), "{schema}：clear_no_input 前提");
        assert!(
            events.is_empty(),
            "{schema}：clear_no_input 什么都不记：{events:?}"
        );
    }
}
