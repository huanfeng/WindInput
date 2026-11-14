//! 头部原文候选（及其大小写变形）与**英文词库词**字面相同时，保留词库那条（带来源 / 编码 /
//! 释义），让它占据头部那一格（首位钉住、不受调频），不再另出一条无来源的原文。
//! 英文方案主路、临英、快捷输入英文成员三路同口径（共用 `english_candidates`）。
//!
//! 现场：头部候选按字面去重把词库同名词删掉，候选窗里那一格只剩一条无来源、无编码、
//! 无释义的原文——词库词的属性整条丢了。
//!
//! 观测面取 UI 下发的 `CandidateItem.code`：头部原文恒无码，词库词带码。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use wind_bridge::handler::{CaretData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT, caret_source};
use wind_ui_types::{CandidateItem, UiCommand};

const VK_SEMICOLON: u32 = 0xBA;

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

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        let m = if ch.is_ascii_uppercase() {
            MOD_SHIFT
        } else {
            0
        };
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, m));
    }
}

fn open(active: &str, edit: impl FnOnce(&mut Config)) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    let mut c = Config::default();
    c.schema.available = vec![active.into(), "wubi86".into(), "pinyin".into()];
    c.schema.available.dedup();
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    edit(&mut c);
    Coordinator::new_headless_with_ui(c, Some(&data_dir()))
}

/// 放行首显闸门后取最近一次下发的候选页。
fn shown(coord: &Coordinator, rx: &Receiver<UiCommand>) -> Vec<CandidateItem> {
    coord.handle_caret_update(&CaretData {
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

fn with_text<'a>(items: &'a [CandidateItem], text: &str) -> Vec<&'a CandidateItem> {
    items.iter().filter(|c| c.text == text).collect()
}

#[test]
fn english_schema_head_keeps_dict_word() {
    skip_without_data!();
    let (coord, rx) = open("english", |c| {
        c.schema.english.case_variants = false;
    });
    type_str(&coord, "hello");
    let items = shown(&coord, &rx);
    assert_eq!(items[0].text, "hello", "首位仍是所打原文那一格");
    assert_eq!(
        items[0].code, "hello",
        "那一格应是词库词本身（带编码），不是无码的原文"
    );
    assert_eq!(with_text(&items, "hello").len(), 1, "不再另出一条同名原文");
}

#[test]
fn temp_english_head_keeps_dict_word() {
    skip_without_data!();
    let (coord, rx) = open("wubi86", |_| {});
    // Shift+A 进临英，再 Shift+B / Shift+C：缓冲 `ABC`，词库里正有 `ABC`。
    type_str(&coord, "ABC");
    assert_eq!(coord.debug_active_mode(), Some("temp_english"), "前提");
    let items = shown(&coord, &rx);
    assert_eq!(items[0].text, "ABC", "首位仍是所打原文那一格");
    assert_eq!(items[0].code, "abc", "那一格应是词库词 `ABC` 本身");
    assert_eq!(with_text(&items, "ABC").len(), 1);
    // 小写变形 `abc` 仍只有一条（临英出厂 `case_follow_input` 开着，词库的 `abc` 已被
    // 投影成 `ABC`、由上面那一格吸收，变形 `abc` 于是是无来源的那条）。
    assert_eq!(with_text(&items, "abc").len(), 1);
}

#[test]
fn quick_input_head_keeps_dict_word() {
    skip_without_data!();
    // 只留英文成员：拼音成员的 `he…` 分步候选会占满首页，英文段落不到首页。
    let (coord, rx) = open("wubi86", |c| {
        c.schema.mix_modes[0].members.retain(|m| m == "english");
    });
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
    type_str(&coord, "hello");
    let items = shown(&coord, &rx);
    let hello = with_text(&items, "hello");
    assert_eq!(hello.len(), 1, "不再另出一条同名原文：{items:?}");
    assert_eq!(hello[0].code, "hello", "那一格应是词库词本身（带编码）");
}
