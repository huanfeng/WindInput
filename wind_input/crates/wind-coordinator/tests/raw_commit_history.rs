//! 原码类上屏也进**上屏历史**（`quick_input.repeat` / z 重复上屏的事实源）。覆盖的出口：
//!
//! - 回车上屏原码：英文方案、主输入路（拼音）、临英、快捷输入、临拼；
//! - 空格无候选上屏原码：主输入路、临英、快捷输入、临拼（`space_on_empty_behavior =
//!   "clear"` 丢弃时什么也没上屏，不记）；
//! - 标点顶掉原码（无候选）：主输入路普通出口与智能符号 hold 出口、临英。
//!
//! 现场：这些出口只记输入统计、不记历史 ⇒ `hello` 回车上屏后按 `;` 重复上屏，取到的是
//! 更早的某次选词，或者什么也没有。
//!
//! 历史记**转换前形态**（与选词出口一致）：简体、半角，含已转换前缀与归还的引导字母，
//! 不含补的空格与顶屏的标点。重复上屏时会再过一次简繁 / 全角转换，记转换后的形态会让
//! 同一段文字经选词与经回车重复出来两个样子。词频不记（原码不是词库词）。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};

const VK_BACKTICK: u32 = 0xC0;
const VK_SEMICOLON: u32 = 0xBA;
const VK_RETURN: u32 = 0x0D;
const VK_NEXT: u32 = 0x22;
const VK_COMMA: u32 = 0xBC;
const VK_PERIOD: u32 = 0xBE;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
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

fn open(active: &str) -> Arc<Coordinator> {
    open_with(active, |_| {})
}

fn open_with(active: &str, edit: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec![
        active.into(),
        "wubi86".into(),
        "pinyin".into(),
        "english".into(),
    ];
    c.schema.available.dedup();
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    edit(&mut c);
    Coordinator::new_headless(c, Some(&data_dir()))
}

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

/// 翻页找「你」分步选中（留在组合区）。
fn pick_ni(coord: &Coordinator) {
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if let Some(p) = t.iter().position(|x| x == "你") {
            let act = coord.handle_key_event(&key(0x31 + p as u32, 0));
            assert!(
                !matches!(act, KeyAction::InsertText { .. }),
                "前提：选「你」应留在组合区分步，实际: {act:?}"
            );
            return;
        }
        coord.handle_key_event(&key(VK_NEXT, 0));
    }
    panic!("前提：候选里应有「你」");
}

/// 回车上屏并断言文本，再进快捷输入（空缓冲）断言重复上屏候选就是它。
fn enter_then_repeat(coord: &Coordinator, want: &str) {
    commit_then_repeat(coord, VK_RETURN, want);
}

/// 按 `vk` 上屏并断言文本，再进快捷输入（空缓冲）断言重复上屏候选就是它。
fn commit_then_repeat(coord: &Coordinator, vk: u32, want: &str) {
    match coord.handle_key_event(&key(vk, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, want, "前提：上屏"),
        other => panic!("前提：应上屏，实际: {other:?}"),
    }
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some(want),
        "上屏的原码应进上屏历史（`;` 重复上屏取得到）"
    );
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
}

#[test]
fn english_schema_enter_enters_history() {
    skip_without_data!();
    let coord = open("english");
    type_str(&coord, "helo");
    enter_then_repeat(&coord, "helo");
}

#[test]
fn main_pinyin_enter_enters_history() {
    skip_without_data!();
    let coord = open("pinyin");
    type_str(&coord, "nihao");
    pick_ni(&coord);
    enter_then_repeat(&coord, "你hao");
}

#[test]
fn temp_english_enter_enters_history() {
    skip_without_data!();
    let coord = open("wubi86");
    coord.handle_key_event(&key(u32::from(b'H'), MOD_SHIFT));
    type_str(&coord, "elo");
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_english"),
        "前提：Shift+H 进临英"
    );
    enter_then_repeat(&coord, "Helo");
}

#[test]
fn quick_input_enter_enters_history() {
    skip_without_data!();
    let coord = open("wubi86");
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "nihao");
    pick_ni(&coord);
    enter_then_repeat(&coord, "你hao");
}

#[test]
fn temp_pinyin_enter_enters_history() {
    skip_without_data!();
    let coord = open("wubi86");
    coord.handle_key_event(&key(VK_BACKTICK, 0));
    assert!(coord.debug_in_temp_pinyin(), "前提：反引号进临拼");
    type_str(&coord, "nihao");
    pick_ni(&coord);
    enter_then_repeat(&coord, "你hao");
}

// ── 空格无候选上屏原码（临拼 / 快捷输入）──

const VK_SPACE: u32 = 0x20;

/// 快捷输入去掉英文成员：它恒给出「所打原文」候选，造不出无候选。
fn without_english(c: &mut Config) {
    c.schema.mix_modes[0].members.retain(|m| m != "english");
}

fn assert_no_candidates(coord: &Coordinator) {
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提：剩余码不应有候选，实际: {:?}",
        coord.debug_page_texts()
    );
}

#[test]
fn temp_pinyin_space_raw_commit_enters_history() {
    skip_without_data!();
    let coord = open("wubi86");
    coord.handle_key_event(&key(VK_BACKTICK, 0));
    type_str(&coord, "nivvvvvvvv");
    pick_ni(&coord);
    assert_no_candidates(&coord);
    commit_then_repeat(&coord, VK_SPACE, "你vvvvvvvv");
}

#[test]
fn quick_input_space_raw_commit_enters_history() {
    skip_without_data!();
    let coord = open_with("wubi86", without_english);
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "vvv");
    assert_no_candidates(&coord);
    commit_then_repeat(&coord, VK_SPACE, "vvv");
}

/// `clear` 丢弃时什么也没上屏，不记历史。
#[test]
fn space_clear_does_not_enter_history() {
    skip_without_data!();
    let coord = open_with("wubi86", |c| {
        without_english(c);
        c.input.space_on_empty_behavior = "clear".into();
    });
    for enter in [VK_BACKTICK, VK_SEMICOLON] {
        coord.handle_key_event(&key(enter, 0));
        type_str(&coord, "vvv");
        assert_no_candidates(&coord);
        assert!(
            matches!(
                coord.handle_key_event(&key(VK_SPACE, 0)),
                KeyAction::ClearComposition
            ),
            "前提：clear 丢弃"
        );
    }
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert!(
        !coord.debug_page_texts().iter().any(|t| t.contains("vvv")),
        "clear 丢弃的原码不应进上屏历史，实际: {:?}",
        coord.debug_page_texts()
    );
}

// ── 主输入路 / 临英的空格与标点无候选出口 ──

/// 主输入路拼音 `nivvvvvvvv` 分步选「你」，剩余 `vvvvvvvv` 无候选。
fn main_ni_then_no_candidates(coord: &Coordinator) {
    type_str(coord, "nivvvvvvvv");
    pick_ni(coord);
    assert_no_candidates(coord);
}

#[test]
fn main_space_raw_commit_enters_history() {
    skip_without_data!();
    let coord = open("pinyin");
    main_ni_then_no_candidates(&coord);
    commit_then_repeat(&coord, VK_SPACE, "你vvvvvvvv");
}

/// 无候选按标点：顶掉的原码进历史，不含标点本身。`punct_on_empty_behavior` 出厂是
/// `clear`（丢原码），须拨到 `commit` 才上屏原码。
#[test]
fn main_punct_raw_commit_enters_history() {
    skip_without_data!();
    let coord = open_with("pinyin", |c| {
        c.input.punct_on_empty_behavior = "commit".into()
    });
    main_ni_then_no_candidates(&coord);
    match coord.handle_key_event(&key(VK_COMMA, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "你vvvvvvvv，", "前提"),
        other => panic!("前提：应上屏，实际: {other:?}"),
    }
    assert_repeat(&coord, "你vvvvvvvv");
}

/// 智能符号 hold 出口（`CommitAndHoldComposition`）同样。
#[test]
fn main_punct_hold_raw_commit_enters_history() {
    skip_without_data!();
    let coord = open_with("pinyin", |c| {
        c.input.punct_on_empty_behavior = "commit".into();
        c.input.symbol.smart_mode = true;
        c.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
    });
    main_ni_then_no_candidates(&coord);
    match coord.handle_key_event(&key(VK_PERIOD, 0)) {
        KeyAction::CommitAndHoldComposition { commit_text, .. } => {
            assert_eq!(commit_text, "你vvvvvvvv", "前提")
        }
        other => panic!("前提：应 hold 上屏，实际: {other:?}"),
    }
    assert_repeat(&coord, "你vvvvvvvv");
}

/// 临英关掉词库候选、原文候选与大小写变形 ⇒ 恒无候选。
fn no_temp_english_candidates(c: &mut Config) {
    c.input.temp_english.show_candidates = false;
    c.input.temp_english.raw_candidate = wind_config::config::RawCandidateMode::Off;
    c.input.temp_english.case_variants = false;
}

/// 临英无候选时空格上屏原文。
#[test]
fn temp_english_space_raw_commit_enters_history() {
    skip_without_data!();
    let coord = open_with("wubi86", no_temp_english_candidates);
    temp_english_helo(&coord);
    assert_no_candidates(&coord);
    commit_then_repeat(&coord, VK_SPACE, "Helo");
}

/// 临英无候选按标点对齐英文方案主路、读 `punct_on_empty_behavior`（出厂 `clear` 丢原文），
/// 故显式取 `commit` 档——同上方主路那两条。
#[test]
fn temp_english_punct_raw_commit_enters_history() {
    skip_without_data!();
    let coord = open_with("wubi86", |c| {
        no_temp_english_candidates(c);
        c.input.punct_on_empty_behavior = "commit".into();
    });
    temp_english_helo(&coord);
    assert_no_candidates(&coord);
    match coord.handle_key_event(&key(VK_COMMA, 0)) {
        KeyAction::InsertText { text, .. } => assert!(text.starts_with("Helo"), "前提: {text}"),
        other => panic!("前提：应上屏，实际: {other:?}"),
    }
    assert_repeat(&coord, "Helo");
}

/// Shift+H 进临英并打 `elo`。
fn temp_english_helo(coord: &Coordinator) {
    coord.handle_key_event(&key(u32::from(b'H'), MOD_SHIFT));
    type_str(coord, "elo");
    assert_eq!(
        coord.debug_active_mode(),
        Some("temp_english"),
        "前提：Shift+H 进临英"
    );
}

/// 进快捷输入（空缓冲）断言重复上屏候选。
fn assert_repeat(coord: &Coordinator, want: &str) {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some(want),
        "上屏的原码应进上屏历史（`;` 重复上屏取得到）"
    );
}

// ── 历史口径：转换前形态 ──

/// 在快捷输入里空格重复上屏，返回上屏文本。
fn repeat_commit(coord: &Coordinator) -> (String, String) {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    let shown = coord
        .debug_page_texts()
        .first()
        .cloned()
        .expect("前提：有重复上屏候选");
    match coord.handle_key_event(&key(VK_SPACE, 0)) {
        KeyAction::InsertText { text, .. } => (shown, text),
        other => panic!("前提：空格重复上屏，实际: {other:?}"),
    }
}

/// 全角态下临英**选词**与**回车**上屏同一串，重复上屏的形态必须一致：历史记转换前的
/// 半角形，重复上屏时只转一次。记全角形的话回车那条重复出来是「选词记半角 / 回车记全角」
/// 两个样子（候选窗里显示的也不同）。
#[test]
fn full_width_repeat_form_matches_between_select_and_enter() {
    skip_without_data!();
    let coord = open_with("wubi86", |c| c.input.default.full_width = true);
    // 选词：空格上屏高亮（首候选 = 原文）。
    temp_english_helo(&coord);
    match coord.handle_key_event(&key(VK_SPACE, 0)) {
        KeyAction::InsertText { .. } => {}
        other => panic!("前提：临英空格选词上屏，实际: {other:?}"),
    }
    let by_select = repeat_commit(&coord);
    // 回车：上屏原文。
    temp_english_helo(&coord);
    match coord.handle_key_event(&key(VK_RETURN, 0)) {
        KeyAction::InsertText { .. } => {}
        other => panic!("前提：临英回车上屏，实际: {other:?}"),
    }
    let by_enter = repeat_commit(&coord);
    assert_eq!(by_select.0, "Helo", "前提：选词路历史记半角原文");
    assert_eq!(
        by_enter, by_select,
        "回车与选词的重复上屏（候选显示, 上屏文本）应一致"
    );
}
