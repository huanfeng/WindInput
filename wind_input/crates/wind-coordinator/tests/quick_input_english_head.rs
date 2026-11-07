//! 快捷输入（`quick_mix` 的 english 成员）的原文候选与大小写变形，对齐临时英文（A2-3b 批 3）。
//!
//! 读临英那两份开关：`input.temp_english.raw_candidate` / `case_variants`（英文方案那两份在
//! 各用例里刻意置反）。落点是**英文段段首**，不是全表首——mix「成员顺序即候选优先级」，
//! 排在前面的拼音成员照旧在前；且头部候选**占英文成员的配额**（`MIX_MEMBER_QUOTA` = 50）。
//!
//! mix 的文本缓冲恒小写，故变形只有首字母大写 / 全大写两条（`hel` → `Hel` / `HEL`）。
//!
//! ⚠️ 词典缺失时整族静默跳过（判据是耗时而非通过条数），worktree 需自备 `build_dev`。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::RawCandidateMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const VK_SEMICOLON: u32 = 0xBA;
/// 与 `handle_mode.rs` 的 `MIX_MEMBER_QUOTA` 同值。
const MIX_MEMBER_QUOTA: usize = 50;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_english_schema() -> bool {
    let d = data_dir();
    d.join("schemas/english.schema.toml").exists() && d.join("schemas/english").is_dir()
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
        seq_char: 0,
    }
}

/// 临英两份开关按参数给，英文方案那两份置反（取值相同时读错开关也全绿）。
fn quick_config(raw: RawCandidateMode, variants: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.temp_english.raw_candidate = raw;
    cfg.input.temp_english.case_variants = variants;
    cfg.schema.english.raw_candidate = if raw == RawCandidateMode::Off {
        RawCandidateMode::Always
    } else {
        RawCandidateMode::Off
    };
    cfg.schema.english.case_variants = !variants;
    cfg
}

fn coord_with(cfg: Config, tag: &str) -> Arc<Coordinator> {
    let path = std::env::temp_dir().join(format!("wind_quick_en_head_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    Coordinator::new_headless_with_store(
        cfg,
        Some(&data_dir()),
        Arc::new(Store::open(&path).unwrap()),
    )
}

fn enter_quick(coord: &Coordinator, word: &str) {
    coord.handle_key_event(&key(VK_SEMICOLON));
    for ch in word.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn all_after(raw: RawCandidateMode, variants: bool, word: &str, tag: &str) -> Vec<String> {
    let c = coord_with(quick_config(raw, variants), tag);
    enter_quick(&c, word);
    c.debug_all_candidate_texts()
}

/// ★ `;hel`：拼音成员在前（成员顺序即优先级），英文段依次 hel / Hel / HEL / 词库词。
#[test]
fn head_candidates_lead_english_segment() {
    if !has_english_schema() {
        return;
    }
    let all = all_after(RawCandidateMode::Always, true, "hel", "seg");
    let i = all
        .iter()
        .position(|t| t == "hel")
        .unwrap_or_else(|| panic!("`;hel` 应有原文候选 hel，实际: {all:?}"));
    assert!(
        i > 0,
        "头部候选落在英文段段首而不是全表首，拼音应在前: {all:?}"
    );
    assert!(
        !all[..i].iter().any(|t| t.is_ascii()),
        "hel 之前应全是拼音成员的中文候选: {:?}",
        &all[..i]
    );
    assert_eq!(
        &all[i..i + 3],
        &["hel", "Hel", "HEL"],
        "英文段段首应是原文 + 两条变形: {all:?}"
    );
    assert!(
        all[i + 3].to_ascii_lowercase().starts_with("hel") && all[i + 3].len() > 3,
        "变形之后应是词库词: {all:?}"
    );
}

/// 反向对照：临英两份开关都关（英文方案那两份开着）时一条都不产。
#[test]
fn switches_off_produce_no_head() {
    if !has_english_schema() {
        return;
    }
    let all = all_after(RawCandidateMode::Off, false, "vqzx", "off");
    assert!(
        all.is_empty(),
        "raw_candidate=off、case_variants=false 时 `;vqzx` 不该有候选，实际: {all:?}"
    );
}

/// 只关变形：原文仍在，变形不产。
#[test]
fn variants_follow_their_own_switch() {
    if !has_english_schema() {
        return;
    }
    let all = all_after(RawCandidateMode::Always, false, "vqzx", "novar");
    assert_eq!(all, vec!["vqzx".to_string()]);
}

/// `in_dict`：原文字面是英文词库词才产（`very` 是、`vqzx` 不是）。
///
/// 命中那半打开变形：词库里的 `very` 本来就排首位，只看首条分不出「仅词库词」与「关闭」
/// 两档。打开变形后两档可分：原文不产时变形仍排最前、词库的 `very` 退到其后（实测 `off`
/// 档得 `Very / VERY / very`），前三条恰为 `very / Very / VERY` 才说明原文确实作为头部
/// 候选产出了。
#[test]
fn in_dict_mode_checks_english_member() {
    if !has_english_schema() {
        return;
    }
    let hit = all_after(RawCandidateMode::InDict, true, "very", "indict_hit");
    assert_eq!(
        &hit[..3],
        &["very", "Very", "VERY"],
        "词库里有 very，应产原文 + 变形: {hit:?}"
    );
    let miss = all_after(RawCandidateMode::InDict, false, "vqzx", "indict_miss");
    assert!(miss.is_empty(), "词库里没有 vqzx，不该产原文: {miss:?}");
}

/// 按精确文本去重：词库里的 `Vim` 被变形 `Vim` 吃掉，只剩一条，且位于变形位。
#[test]
fn dedup_by_exact_text() {
    if !has_english_schema() {
        return;
    }
    let all = all_after(RawCandidateMode::Always, true, "vim", "dedup");
    assert_eq!(&all[..3], &["vim", "Vim", "VIM"], "实际: {all:?}");
    assert_eq!(
        all.iter().filter(|t| *t == "Vim").count(),
        1,
        "Vim 只能出现一次: {all:?}"
    );
}

/// ★ 头部候选占英文成员的配额：`;vi` 拼音成员给不出候选，整表都是英文段，
/// 加上头部三条后总数仍不超过配额。
#[test]
fn head_counts_against_member_quota() {
    if !has_english_schema() {
        return;
    }
    let all = all_after(RawCandidateMode::Always, true, "vi", "quota");
    assert_eq!(&all[..3], &["vi", "Vi", "VI"], "实际: {all:?}");
    assert_eq!(
        all.len(),
        MIX_MEMBER_QUOTA,
        "英文段（含头部）应恰好截在配额上，实际 {} 条",
        all.len()
    );
}

/// 选中变形同样补空格（与批 2 联动：变形是所打原码的大小写形态，算英文）。
#[test]
fn selecting_variant_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = quick_config(RawCandidateMode::Always, true);
    cfg.input.temp_english.commit_space = true;
    cfg.schema.english.commit_space = false;
    let c = coord_with(cfg, "variant_space");
    enter_quick(&c, "vqzx");
    let page = c.debug_page_texts();
    let i = page
        .iter()
        .position(|t| t == "VQZX")
        .unwrap_or_else(|| panic!("前提：页内应有 VQZX，实际: {page:?}"));
    match c.debug_mouse_select(i).expect("鼠标点选应带回上屏动作") {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "VQZX "),
        other => panic!("期望上屏动作，实际: {other:?}"),
    }
}
