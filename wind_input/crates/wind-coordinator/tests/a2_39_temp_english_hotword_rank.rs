//! A2-39 观测：临时英文里用过 `translation` 后再键入 `t`，它排第几。
//!
//! 反馈（t202）：打完 `translation` 上屏，再进临英键入 `t`，`translation` 未置顶。
//! 本文件**只量不判**（`#[ignore]`，手动跑）：
//!
//! ```text
//! cargo test -p wind-coordinator --test a2_39_temp_english_hotword_rank -- --ignored --nocapture
//! ```
//!
//! 量六组：英文调频 {出厂关, 开} × 打法 {打全按空格, 打全选小写那条, 打 `tran` 翻页选}，
//! 各报选 0/1/3/5 次后 `t` 下 `translation` 的原序位次（0 起的全列表下标）与页号，末轮后
//! `tr`/`tra`/`tran` 下的位次，以及英文桶里实际记下的词频行。
//! 配置取出厂（`Config::load` = 代码默认 + `build_dev/data/config.toml` + 用户层；用例覆盖
//! `schema.available/active` 与调频开关，用户层若另改了临英/英文段会混进来，读数前先看一眼）。
//!
//! 2026-09-25 修前实测结论：
//! - 出厂调频关：不记、不排，位次恒不变——出厂值问题；
//! - 调频开、打全再上屏（空格 / 选小写）：一条都不记。打全 `translation` 时词库那条被头部
//!   原文 / 大小写变形吞掉，头部候选不是 `English` 来源，`record_temp_english_selection` 跳过；
//! - 调频开、从词库段选中：`(translation, translation)` 记到 5 次，`tr`/`tra` 101→6、
//!   `tran` 41→4；但 `t` 下它**根本不在取回的 300 条里**，重排只作用于已取回的候选，
//!   用多少次都上不来——取数上限先于词频重排，是代码问题。
//!
//! 修后（②头部候选按对应词库词记、③调频开时从词频表召回池外的词，正式断言见
//! `temp_english_hotword_recall.rs`）：调频开时三种打法一致——`t` 下 选 1/3/5 次后
//! 下标 152/39/11（第 22/6/2 页），`tr`/`tra` 6、`tran` 4；调频关逐字不变。
//! `observe_recall_other_entries_and_cost` 另量英文方案 / 快捷输入的位次与召回的每键耗时。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时跳过。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

const WORD: &str = "translation";
const VK_ESCAPE: u32 = 0x1B;
const VK_NEXT: u32 = 0x22;
const VK_SPACE: u32 = 0x20;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_english_schema() -> bool {
    let d = data_dir();
    d.join("schemas/english.schema.toml").exists() && d.join("schemas/english").is_dir()
}

fn key(key_code: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// Shift+首字母进临英，再打剩余字母。
fn enter(coord: &Coordinator, word: &str) {
    let mut chars = word.chars();
    let first = chars.next().unwrap();
    coord.handle_key_event(&key((first.to_ascii_uppercase() as u32) & 0xFF, MOD_SHIFT));
    for c in chars {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

/// 比对一律忽略大小写：出厂 `case_follow_input` 开着，临英缓冲首字母恒大写，候选面文本
/// 被投影成 `Translation`。
///
/// 前缀 `prefix` 下 `translation` 的全列表下标（None = 不在列表里）、候选总数、首页条数。
fn rank_at(coord: &Coordinator, prefix: &str) -> (Option<usize>, usize, usize) {
    enter(coord, prefix);
    let all = coord.debug_all_candidate_texts();
    let page = coord.debug_page_texts().len().max(1);
    let r = all.iter().position(|t| t.eq_ignore_ascii_case(WORD));
    coord.handle_key_event(&key(VK_ESCAPE, 0));
    (r, all.len(), page)
}

/// 翻页找到 `translation`（任意大小写形）并数字键选中；找不到（不在候选列表里）就 Esc 退出、返回 false。
fn pick_word(coord: &Coordinator) -> bool {
    if !coord
        .debug_all_candidate_texts()
        .iter()
        .any(|t| t.eq_ignore_ascii_case(WORD))
    {
        coord.handle_key_event(&key(VK_ESCAPE, 0));
        return false;
    }
    for _ in 0..200 {
        let page = coord.debug_page_texts();
        if let Some(p) = page.iter().position(|t| t.eq_ignore_ascii_case(WORD)) {
            match coord.handle_key_event(&key(0x31 + p as u32, 0)) {
                KeyAction::InsertText { .. } => return true,
                other => panic!("选中应上屏，实际: {other:?}"),
            }
        }
        coord.handle_key_event(&key(VK_NEXT, 0));
    }
    panic!("列表里有 {WORD}，翻 200 页却没翻到");
}

fn fmt_rank((r, total, page): (Option<usize>, usize, usize)) -> String {
    let pos = r.map_or("不在列表".to_string(), |i| {
        format!("下标 {i}（第 {} 页第 {} 位）", i / page + 1, i % page + 1)
    });
    format!("{pos}，共 {total} 条")
}

/// 一次「用过 translation」的三种打法。
#[derive(Clone, Copy)]
enum Via {
    /// 打全 `translation` 按空格：上屏的是首条原文 `Translation`（出厂 `raw_candidate = always`）。
    FullSpace,
    /// 打全 `translation` 选页上第一条小写 `translation`（出厂 `case_variants` 开时是大小写变形）。
    FullPickLower,
    /// 打 `tran` 从列表里翻页选 `translation`（必是词库候选）。
    TranPick,
}

impl Via {
    fn name(self) -> &'static str {
        match self {
            Via::FullSpace => "打全 translation 按空格",
            Via::FullPickLower => "打全 translation 选小写那条",
            Via::TranPick => "打 tran 翻页选",
        }
    }
    fn select(self, coord: &Coordinator) -> bool {
        match self {
            Via::FullSpace => {
                enter(coord, WORD);
                matches!(
                    coord.handle_key_event(&key(VK_SPACE, 0)),
                    KeyAction::InsertText { .. }
                )
            }
            Via::FullPickLower => {
                enter(coord, WORD);
                let page = coord.debug_page_texts();
                match page.iter().position(|t| t == WORD) {
                    Some(p) => matches!(
                        coord.handle_key_event(&key(0x31 + p as u32, 0)),
                        KeyAction::InsertText { .. }
                    ),
                    None => {
                        coord.handle_key_event(&key(VK_ESCAPE, 0));
                        false
                    }
                }
            }
            Via::TranPick => {
                enter(coord, "tran");
                pick_word(coord)
            }
        }
    }
}

/// 按调频开关与打法跑 5 轮，打印选 0/1/3/5 次后 `t` 下的位次；末了再量 `tr` / `tra` /
/// `tran` 下的位次（`t` 下不在列表时看它从哪个前缀起出现），并列出英文桶里记下的词频行。
fn observe(freq: bool, via: Via) {
    let mut cfg = Config::load(Some(&data_dir())).unwrap_or_default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.english.frequency.enabled = freq;
    let db = std::env::temp_dir().join(format!(
        "wind_a2_39_{}_{}_{}.redb",
        freq,
        via as u8,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), Arc::clone(&store));
    let tag = format!(
        "调频={} | {}",
        if freq { "开" } else { "关(出厂)" },
        via.name()
    );
    eprintln!(
        "A2-39 {tag} | 选 0 次后 `t`: {}",
        fmt_rank(rank_at(&coord, "t"))
    );
    for i in 1..=5 {
        if !via.select(&coord) {
            eprintln!("A2-39 {tag} | 第 {i} 次：选不到 {WORD}");
            break;
        }
        if matches!(i, 1 | 3 | 5) {
            eprintln!(
                "A2-39 {tag} | 选 {i} 次后 `t`: {}",
                fmt_rank(rank_at(&coord, "t"))
            );
        }
    }
    for prefix in ["tr", "tra", "tran"] {
        eprintln!(
            "A2-39 {tag} | 末轮后 `{prefix}`: {}",
            fmt_rank(rank_at(&coord, prefix))
        );
    }
    let mut rows = Vec::new();
    let _ = store.for_each_freq("english", "", &mut |code, text, rec| {
        rows.push(format!("({code}, {text}, count={})", rec.count));
        true
    });
    eprintln!("A2-39 {tag} | english 桶词频行: {rows:?}");
    let _ = std::fs::remove_file(&db);
}

#[test]
#[ignore = "A2-39 观测，手动跑：--ignored --nocapture"]
fn observe_translation_rank_under_t() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    for freq in [false, true] {
        for via in [Via::FullSpace, Via::FullPickLower, Via::TranPick] {
            observe(freq, via);
        }
    }
}

/// 召回在英文方案 / 快捷输入上的位次，以及 `t` 这一键的耗时（召回开 vs 调频关）。
///
/// 词频表直接种 5 次 `(translation, translation)` 与 20 个别的 `t` 开头词频行（模拟一个用过
/// 不少 t 词的用户：召回要逐条扫描 + 验证），`t` 各打 200 次取均值。
#[test]
#[ignore = "A2-39 观测，手动跑：--ignored --nocapture"]
fn observe_recall_other_entries_and_cost() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let db = std::env::temp_dir().join(format!("wind_a2_39_cost_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    for _ in 0..5 {
        store.record_freq("english", WORD, WORD).unwrap();
    }
    // 20 个 t 开头、原序多在池外的词（取自出厂 en.dict.yaml）。
    for w in [
        "tackling",
        "taxation",
        "technicians",
        "temperatures",
        "tentative",
        "territorial",
        "textbook",
        "theology",
        "thinking",
        "threatening",
        "throwing",
        "timeshares",
        "tolerated",
        "townhouse",
        "tradesman",
        "tranquil",
        "transferred",
        "transition",
        "transmit",
        "teaspoon",
    ] {
        store.record_freq("english", w, w).unwrap();
    }
    let build = |freq: bool, active: &str| {
        let mut cfg = Config::load(Some(&data_dir())).unwrap_or_default();
        cfg.schema.available = vec!["wubi86".into(), "english".into()];
        cfg.schema.active = active.into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.english.frequency.enabled = freq;
        Coordinator::new_headless_with_store(cfg, Some(&data_dir()), Arc::clone(&store))
    };
    // 英文方案：主输入路直接打 `t`。
    for freq in [false, true] {
        let coord = build(freq, "english");
        coord.handle_key_event(&key(b'T' as u32, 0));
        let all = coord.debug_all_candidate_texts();
        let r = all.iter().position(|t| t.eq_ignore_ascii_case(WORD));
        eprintln!(
            "A2-39 英文方案 调频={} | `t`: {}",
            if freq { "开" } else { "关" },
            fmt_rank((r, all.len(), coord.debug_page_texts().len().max(1)))
        );
    }
    // 快捷输入：`;t`。
    for freq in [false, true] {
        let coord = build(freq, "wubi86");
        coord.handle_key_event(&key(0xBA, 0));
        coord.handle_key_event(&key(b'T' as u32, 0));
        let all = coord.debug_all_candidate_texts();
        let r = all.iter().position(|t| t.eq_ignore_ascii_case(WORD));
        let first_en = all.iter().position(|t| t.is_ascii());
        eprintln!(
            "A2-39 快捷输入 调频={} | `;t`: {}（英文段首下标 {first_en:?}）",
            if freq { "开" } else { "关" },
            fmt_rank((r, all.len(), coord.debug_page_texts().len().max(1)))
        );
    }
    // 耗时：临英 Shift+T 一键（含进模式与候选构建），200 次均值。
    for freq in [false, true] {
        let coord = build(freq, "wubi86");
        enter(&coord, "t");
        coord.handle_key_event(&key(VK_ESCAPE, 0));
        let n = 200;
        let t0 = Instant::now();
        for _ in 0..n {
            enter(&coord, "t");
            coord.handle_key_event(&key(VK_ESCAPE, 0));
        }
        eprintln!(
            "A2-39 耗时 调频={} | 临英 `t`（含 Esc）每次 {:.1} µs",
            if freq {
                "开（召回 21 行）"
            } else {
                "关"
            },
            t0.elapsed().as_secs_f64() * 1e6 / n as f64
        );
    }
    let _ = std::fs::remove_file(&db);
}
