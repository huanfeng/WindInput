//! 临时拼音里按标点：顶屏高亮候选**并**上屏该标点（按中文标点映射），退出临拼。
//!
//! 现场（实测）：五笔主方案下 `` ` `` 进临拼打 `nihao`，按 `,` 只上屏「你好」，逗号丢了。
//! 按键落在 `handle_temp_pinyin_key` 的 `_` 兜底臂，那里有候选时只调
//! `commit_temp_pinyin_selected`，标点字符本身没有任何出口；`.` `/` `[` `\` `Shift+1`
//! 同样被吞。对照快捷输入 ⑥、临时英文、主输入路 `commit_highlight_then_char`：三处都是
//! 「候选 + 标点」一起上屏。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::stats::CommitSource;

const VK_BACKTICK: u32 = 0xC0;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("schemas/pinyin.schema.toml").exists()
}

fn key(k: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 临拼打 `nihao` 后按 (`k`, `modifiers`)，返回 (上屏文本, 按后是否仍在临拼, 这一键的统计事件)。
///
/// 统计事件只捕获这一键（打码那几下不上屏、本就不产生事件）；走非 policed 入口，
/// 不含顶层兜底那一笔。
fn nihao_then(k: u32, modifiers: u32) -> (Option<String>, bool, Vec<(CommitSource, String)>) {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    coord.handle_key_event(&key(VK_BACKTICK, 0));
    assert!(coord.debug_in_temp_pinyin(), "前提：反引号进临拼");
    for ch in "nihao".chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
    let page = coord.debug_page_texts();
    assert_eq!(
        page.first().map(String::as_str),
        Some("你好"),
        "前提：首选应是「你好」，实际: {page:?}"
    );
    coord.debug_capture_stat_events();
    let out = match coord.handle_key_event(&key(k, modifiers)) {
        KeyAction::InsertText { text, .. } => Some(text),
        _ => None,
    };
    (
        out,
        coord.debug_in_temp_pinyin(),
        coord.debug_take_stat_events(),
    )
}

#[test]
fn comma_commits_candidate_and_chinese_comma() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (out, still, events) = nihao_then(0xBC, 0);
    assert_eq!(
        out.as_deref(),
        Some("你好，"),
        "`,` 应顶屏「你好」并上屏中文逗号"
    );
    assert!(!still, "顶屏后应退出临拼");
    // 候选段按临拼、标点按 `Punctuation` 各记一笔（候选段那笔由 `commit_temp_pinyin_selected` 记）。
    assert_eq!(
        events,
        vec![
            (CommitSource::TempPinyin, "你好".to_string()),
            (CommitSource::Punctuation, "，".to_string()),
        ],
        "应记「候选 + 标点」两笔统计"
    );
}

#[test]
fn period_commits_candidate_and_chinese_period() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (out, _, _) = nihao_then(0xBE, 0);
    assert_eq!(out.as_deref(), Some("你好。"));
}

#[test]
fn shifted_digit_punct_is_not_swallowed() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (out, _, _) = nihao_then(0x31, MOD_SHIFT);
    assert_eq!(out.as_deref(), Some("你好！"));
}
