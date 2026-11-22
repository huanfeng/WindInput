//! 换位纠错候选在界面上看得见：按罚过的分与其余候选穿插、窗口内保底 4 条。
//!
//! 走协调器断言界面序（引擎位次不作数）。真实词库在 `build_dev/data`，缺失时跳过。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir()
        .join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
}

/// 敲入 `input` 后的全部候选文字（界面序，不含标记）。
fn all_texts(input: &str) -> Vec<String> {
    run(input).0
}

/// 敲入 `input` 后的（全部候选文字，编码栏）。
fn run(input: &str) -> (Vec<String>, String) {
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.input.correction.rules.gn_ng = true; // 出厂关；用例是 dagnshi / gognping / xign
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    for ch in input.chars() {
        coord.handle_key_event(&KeyEventData {
            key_code: ch.to_ascii_uppercase() as u32,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    }
    (coord.debug_all_candidate_texts(), coord.debug_preedit())
}

fn rank(all: &[String], text: &str) -> Option<usize> {
    all.iter().position(|t| t == text)
}

#[test]
fn corrected_candidates_are_visible() {
    if !has_data() {
        return;
    }
    for (input, text, within) in [
        ("zhonguo", "中国", 10),
        ("dagnshi", "当时", 10),
        ("gognping", "公平", 5),
    ] {
        let all = all_texts(input);
        let r = rank(&all, text);
        assert!(
            r.is_some_and(|r| r < within),
            "{input}: {text} 在 {r:?}：{all:?}"
        );
    }
}

#[test]
fn xign_shows_correction_and_keeps_abbrev() {
    if !has_data() {
        return;
    }
    let all = all_texts("xign");
    assert!(rank(&all, "型").is_some(), "{all:?}");
    assert!(rank(&all, "习惯").is_some(), "{all:?}");
}

/// 非首字的纠错读法吃满整串，不被同文的简拼读法（`bu`+`y`）去重挡掉：
/// 简拼开（出厂）时「不用」「恐龙」也要排在简拼候选之前，而不是沉到第 3 位。
#[test]
fn corrected_full_reading_beats_same_text_abbrev_reading() {
    if !has_data() {
        return;
    }
    for (input, text) in [("buyogn", "不用"), ("konglon", "恐龙")] {
        let all = all_texts(input);
        assert_eq!(rank(&all, text), Some(0), "{input}: {text} 应居首：{all:?}");
    }
}

/// 首选是吃满整串的纠错候选时，编码栏按所打码切分：`konglon` 的「恐龙」显示 `kong'lon`，
/// 不能退回主切分的 `kong'lo'n`（候选吃满 7 键、编码栏却拆成三段）。
#[test]
fn preedit_follows_a_trailing_partial_correction_candidate() {
    if !has_data() {
        return;
    }
    let (all, preedit) = run("konglon");
    assert_eq!(rank(&all, "恐龙"), Some(0), "{all:?}");
    assert_eq!(preedit, "kong'lon");
}

/// 纠错音节后接简拼尾：`yogny`（yogn→yong + 声母 y）出「拥有」，编码栏按所打击键切成 `yogn'y`；
/// 纠错音节在中间（`buyognx`）同样可用（纠错不占首选，合法的整句读法仍在前）。
#[test]
fn corrected_syllable_followed_by_abbrev_tail() {
    if !has_data() {
        return;
    }
    for (input, text, preedit) in [("yogny", "拥有", "yogn'y"), ("gognp", "公平", "gogn'p")] {
        let (all, pre) = run(input);
        assert_eq!(rank(&all, text), Some(0), "{input}: {text} 应居首：{all:?}");
        assert_eq!(pre, preedit, "{input} 的编码栏");
    }
    let all = all_texts("buyognx");
    assert!(rank(&all, "不用心").is_some_and(|r| r < 5), "{all:?}");
}
