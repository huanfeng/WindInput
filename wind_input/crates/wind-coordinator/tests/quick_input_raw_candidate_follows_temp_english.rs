//! 快捷输入大写英文（Free 透镜英文段）的「原文候选」跟随临英开关
//! `input.temp_english.raw_candidate`（论坛 t235 三楼，0.124.0）。
//!
//! 现场：楼主把临英的原文候选关了（`/Mar` 下热词 `Markdown` 排首位），快捷输入里打 `;Mar`
//! 首位却恒是 `Mar`——`mix_free_english_segment` 把原文写死成「恒在」，不读开关；词库同名词
//! `mar` 又占住了那一格（`merge_head_with_dict`），热词只能排第二。词频重排只作用于词库段，
//! 钉在前面的头部格不参与。
//!
//! 对齐原则：快捷输入的英文段对齐临英（`english_candidates.rs` 文件头「配置分开、实现共用」），
//! 同一串大写输入在两处给出的首位必须一致。
//!
//! 原文不出时「打什么上屏什么」由回车兜底；词库也无命中时列表补回原文一条，空格同样上屏它。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::RawCandidateMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;
use wind_store::completion::CompletionKind;

const VK_SEMICOLON: u32 = 0xBA;
const VK_SPACE: u32 = 0x20;
const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;
const VK_DOWN: u32 = 0x28;
const HOT: &str = "Markdown";

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

fn open(tag: &str, mode: RawCandidateMode) -> (Arc<Coordinator>, PathBuf) {
    let (coord, _store, db) = open_with(tag, mode, |_| {});
    (coord, db)
}

fn open_with(
    tag: &str,
    mode: RawCandidateMode,
    edit: impl FnOnce(&mut Config),
) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    c.input.temp_english.raw_candidate = mode;
    // 楼主截图里没有 `mar` / `MAR` 变形：变形候选也关着。
    c.input.temp_english.case_variants = false;
    c.schema.english.frequency.enabled = true;
    edit(&mut c);
    let db = std::env::temp_dir().join(format!(
        "wind_quick_raw_follow_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    (coord, store, db)
}

/// 临英里打 `Mar`（Shift+M 进临英，缓冲首字母恒大写）。
fn temp_mar(coord: &Coordinator) -> Vec<String> {
    type_str(coord, "Mar");
    coord.debug_all_candidate_texts()
}

/// 快捷输入里打 `Mar`（Shift+M 让缓冲带大写，进 Free 透镜的英文段）。
fn quick_mar(coord: &Coordinator) -> Vec<String> {
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
    type_str(coord, "Mar");
    coord.debug_all_candidate_texts()
}

/// 在临英里打全 `Markdown` 选中若干次，让它成为热词。英文调频是「位次减半」策略
/// （`FreqStrategy::Position`），几次才爬得到首位。
fn make_hot(coord: &Coordinator) {
    for _ in 0..8 {
        let page = {
            type_str(coord, HOT);
            coord.debug_page_texts()
        };
        let p = page
            .iter()
            .position(|t| t == HOT)
            .unwrap_or_else(|| panic!("前提：首页应有 `{HOT}`：{page:?}"));
        // 不用数字键：列表只剩原文一条时临英把数字当输入。
        for _ in 0..p {
            coord.handle_key_event(&key(VK_DOWN, 0));
        }
        match coord.handle_key_event(&key(VK_SPACE, 0)) {
            KeyAction::InsertText { text, .. } => assert_eq!(text.trim_end(), HOT),
            other => panic!("空格应上屏 `{HOT}`，实际: {other:?}"),
        }
    }
}

/// 楼主的配置：临英原文候选关。热词在两处都排首位。
#[test]
fn raw_off_hot_word_leads_in_quick_input_like_temp_english() {
    skip_without_data!();
    let (coord, db) = open("off_hot", RawCandidateMode::Off);
    make_hot(&coord);
    let temp = temp_mar(&coord);
    assert_eq!(
        temp.first().map(String::as_str),
        Some(HOT),
        "对照：临英原文候选关时热词首位：{temp:?}"
    );
    coord.handle_key_event(&key(VK_ESCAPE, 0));
    let quick = quick_mar(&coord);
    assert_eq!(
        quick.first().map(String::as_str),
        Some(HOT),
        "快捷输入应与临英同首位，不该把原文 / 同名词 `Mar` 钉在热词前面：{quick:?}"
    );
    let _ = std::fs::remove_file(&db);
}

/// 出厂档 Always：原文仍钉首位（行为不变，两处一致）。
#[test]
fn raw_always_still_pins_raw_first() {
    skip_without_data!();
    let (coord, db) = open("always", RawCandidateMode::Always);
    make_hot(&coord);
    let temp = temp_mar(&coord);
    coord.handle_key_event(&key(VK_ESCAPE, 0));
    let quick = quick_mar(&coord);
    assert_eq!(temp.first().map(String::as_str), Some("Mar"), "{temp:?}");
    assert_eq!(quick.first().map(String::as_str), Some("Mar"), "{quick:?}");
    let _ = std::fs::remove_file(&db);
}

/// 原文候选关、词库也无命中：列表只剩补位的原文，回车 / 空格都上屏它。
#[test]
fn raw_off_unknown_word_still_commits_raw() {
    skip_without_data!();
    for (k, name) in [(VK_RETURN, "回车"), (VK_SPACE, "空格")] {
        let (coord, db) = open("off_unknown", RawCandidateMode::Off);
        coord.handle_key_event(&key(VK_SEMICOLON, 0));
        type_str(&coord, "Qzxv");
        // Free 透镜的底线：整段为空时补回原文一条（不算钉首位）。
        assert_eq!(coord.debug_all_candidate_texts(), vec!["Qzxv".to_string()]);
        match coord.handle_key_event(&key(k, 0)) {
            KeyAction::InsertText { text, .. } => {
                assert_eq!(text.trim_end(), "Qzxv", "{name}应上屏原文")
            }
            other => panic!("{name}应上屏原文，实际: {other:?}"),
        }
        let _ = std::fs::remove_file(&db);
    }
}

/// 候选关着（`show_candidates = false`）、原文关、变形开（出厂）：变形也不出，同临英
/// `want_variants`——否则首格成了 `qzxv`，空格上屏一个没打的形态。
#[test]
fn raw_off_without_dict_query_commits_raw_not_variant() {
    skip_without_data!();
    let (coord, _store, db) = open_with("off_noquery", RawCandidateMode::Off, |c| {
        c.input.temp_english.show_candidates = false;
        c.input.temp_english.case_variants = true;
    });
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "Qzxv");
    assert_eq!(coord.debug_all_candidate_texts(), vec!["Qzxv".to_string()]);
    match coord.handle_key_event(&key(VK_SPACE, 0)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text.trim_end(), "Qzxv"),
        other => panic!("空格应上屏原文，实际: {other:?}"),
    }
    let _ = std::fs::remove_file(&db);
}

/// 原文关、变形开、有历史命中：首格是无来源的变形 `qzxv`，不是原文，不得被当原文丢掉。
#[test]
fn raw_off_with_history_keeps_first_case_variant() {
    skip_without_data!();
    let (coord, store, db) = open_with("off_hist_variant", RawCandidateMode::Off, |c| {
        c.input.temp_english.case_variants = true;
        let members = &mut c.schema.mix_modes[0].members;
        members.insert(0, wind_quick_input::MEMBER_HISTORY.to_string());
    });
    store
        .record_completion(CompletionKind::QuickHistory, "QzxvAbc")
        .unwrap();
    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    type_str(&coord, "Qzxv");
    let all = coord.debug_all_candidate_texts();
    assert_eq!(all.first().map(String::as_str), Some("QzxvAbc"), "{all:?}");
    for v in ["qzxv", "QZXV"] {
        assert!(all.iter().any(|t| t == v), "变形 `{v}` 应保留：{all:?}");
    }
    assert!(
        !all.iter().any(|t| t == "Qzxv"),
        "原文关时不单列原文：{all:?}"
    );
    let _ = std::fs::remove_file(&db);
}
