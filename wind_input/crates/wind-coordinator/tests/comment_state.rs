//! 注释「默认 / 详细」两态：持久态与「仅本次候选窗」翻转。
//!
//! 期望值取自出厂 `system.comments.toml` 的分配：码表方案 默认=`code_hint`、
//! 详细=`chaizi_pinyin`（`${chaizi}{ · ${pinyin}}`）；临时拼音 详细=`chaizi_code`
//! （`${chaizi} ${chaizi_code}`）。拆字取自 `wubi86_chaizi.txt`（字根是 PUA 字符）。
//!
//! 词典缺失时自动跳过（判据：耗时 0.0x 秒＝假绿）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use wind_bridge::handler::{CaretData, KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::comment_lib::CommentState;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_ALT, caret_source};
use wind_ui_types::{CandidateItem, UiCommand};

/// `gg` 前三条候选（五 / 琶 / 王）在码表默认态的注释：首选无余码，其余是补全余码。
const GG_DEFAULT: [&str; 3] = ["", "c", "g"];
/// 同上，详细态：拆字 + 带调拼音。
const GG_DETAIL: [&str; 3] = [
    "\u{e003} \u{e001} \u{e06b} \u{e001} · wǔ",
    "\u{e002} \u{e002} \u{e210} \u{e1df} · pá",
    "\u{e002} \u{e002} \u{e002} \u{e002} · wáng",
];

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists()
        && d.join("schemas/wubi86/wubi86_chaizi.txt").exists()
        && d.join("schemas/pinyin.schema.toml").exists()
}

fn key_mod(key_code: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn key(key_code: u32) -> KeyEventData {
    key_mod(key_code, 0)
}

fn cfg() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.association.kind = "off".into();
    cfg
}

fn open(cfg: Config) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    // 拼音变量与联想都读反查索引，生产里由后台预热，headless 要自己建。
    c.prewarm_indexes();
    (c, rx)
}

fn type_str(c: &Coordinator, s: &str) {
    for ch in s.chars() {
        c.handle_key_event(&key(ch.to_ascii_uppercase() as u32));
    }
}

/// 放行首显闸门后取最近一次下发的候选页。
fn shown(c: &Coordinator, rx: &Receiver<UiCommand>) -> Vec<CandidateItem> {
    c.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    rx.try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates { candidates, .. } => Some(candidates),
            _ => None,
        })
        .last()
        .expect("应下发候选")
}

fn first_comments(items: &[CandidateItem], n: usize) -> Vec<String> {
    items
        .iter()
        .take(n)
        .map(|i| i.comment.as_str().to_string())
        .collect()
}

fn gg_comments(c: &Coordinator, rx: &Receiver<UiCommand>) -> Vec<String> {
    let items = shown(c, rx);
    let texts: Vec<_> = items.iter().take(3).map(|i| i.text.as_str()).collect();
    assert_eq!(texts, ["五", "琶", "王"], "前置条件：gg 前三条候选");
    first_comments(&items, 3)
}

#[test]
fn session_flip_shows_detail_then_resets_on_commit() {
    if !has_schemas() {
        return;
    }
    let (c, rx) = open(cfg());
    type_str(&c, "gg");
    assert_eq!(gg_comments(&c, &rx), GG_DEFAULT);
    c.debug_toggle_comment_session();
    assert_eq!(gg_comments(&c, &rx), GG_DETAIL, "翻转后应显示详细");

    c.handle_key_event(&key(0x20));
    assert_eq!(c.debug_candidate_count(), 0, "前置条件：空格上屏结束会话");
    type_str(&c, "gg");
    assert_eq!(gg_comments(&c, &rx), GG_DEFAULT, "上屏后翻转应复位");
}

#[test]
fn session_flip_resets_before_association() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    cfg.input.association.kind = "word".into();
    cfg.input.association.mode = "one_shot".into();
    let (c, rx) = open(cfg);
    type_str(&c, "gg");
    c.debug_toggle_comment_session();
    assert_eq!(gg_comments(&c, &rx), GG_DETAIL);

    // 上屏后立刻弹联想，其间没有新按键。
    c.handle_key_event(&key(0x20));
    let assoc = c.debug_assoc_texts();
    assert_eq!(
        assoc.iter().take(3).map(String::as_str).collect::<Vec<_>>(),
        ["五一", "五笔", "五官"],
        "前置条件：上屏「五」后进联想"
    );
    // 详细态下这三条是 `· wǔ yī` 等；持久态（默认）下码表默认模板对联想候选为空。
    assert_eq!(
        first_comments(&shown(&c, &rx), 3),
        ["", "", ""],
        "联想窗应按持久态显示"
    );
    assert_eq!(c.debug_effective_comment_state(), CommentState::Default);
}

#[test]
fn session_flip_survives_entering_temp_pinyin() {
    if !has_schemas() {
        return;
    }
    let (c, rx) = open(cfg());
    type_str(&c, "gg");
    c.debug_toggle_comment_session();
    assert_eq!(gg_comments(&c, &rx), GG_DETAIL);
    c.handle_key_event(&key(0xC0)); // 反引号进入临拼
    assert!(c.debug_in_temp_pinyin(), "前置条件：反引号应进入临时拼音");
    type_str(&c, "wang");
    let items = shown(&c, &rx);
    let first = items.first().expect("临拼 wang 应有候选");
    assert_eq!(first.text, "往", "前置条件：临拼 wang 首选");
    assert_eq!(
        first.comment.as_str(),
        "\u{e0d1} \u{e147} \u{e002} \u{e000} tygg",
        "翻转应保留，取临拼的详细格（拆字+编码）"
    );
}

#[test]
fn persist_detail_then_session_flip_shows_default() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    cfg.ui.comment.state = CommentState::Detail;
    let (c, rx) = open(cfg);
    type_str(&c, "gg");
    assert_eq!(gg_comments(&c, &rx), GG_DETAIL, "持久态为详细");
    c.debug_toggle_comment_session();
    assert_eq!(gg_comments(&c, &rx), GG_DEFAULT, "再翻转看到的是默认");
}

#[test]
fn alt_commit_uses_effective_state() {
    if !has_schemas() {
        return;
    }
    let mut cfg = cfg();
    cfg.input.alt_commit = "comment".into();
    let (c, rx) = open(cfg);
    type_str(&c, "gg");
    c.debug_toggle_comment_session();
    assert_eq!(gg_comments(&c, &rx), GG_DETAIL);
    let act = c.handle_key_event(&key_mod(0x31, MOD_ALT));
    match act {
        KeyAction::InsertText { text, .. } => assert_eq!(text, GG_DETAIL[0]),
        other => panic!("Alt+1 应上屏注释，实得 {other:?}"),
    }
}

#[test]
fn flip_does_not_requery() {
    if !has_schemas() {
        return;
    }
    let (c, rx) = open(cfg());
    type_str(&c, "gg");
    c.handle_key_event(&key(0x28)); // 高亮挪到第二条，使下标比较有意义
    let texts = c.debug_all_candidate_texts();
    let page = c.debug_page_info();
    assert_ne!(page.1, 0, "前置条件：高亮已离开首条");
    let before: Vec<_> = shown(&c, &rx).into_iter().map(|i| i.text).collect();

    c.debug_toggle_comment_session();
    assert_eq!(c.debug_all_candidate_texts(), texts, "候选列表与顺序不变");
    assert_eq!(c.debug_page_info(), page, "页与高亮不变");
    let after: Vec<_> = shown(&c, &rx).into_iter().map(|i| i.text).collect();
    assert_eq!(after, before, "下发的候选页不变");
}
