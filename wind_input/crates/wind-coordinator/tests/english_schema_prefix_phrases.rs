//! 英文方案开了短语时，前缀命中的短语不得把英文候选块拆开。
//!
//! 协调器 `place_english_after_common_exact` 只让拼音「英文前缀补全」补出来的英文给拼音真词
//! 让位。曾经按「code 比输入长」认英文、又不限让位对象的来源，短语（`consumed` 为 0，被当成
//! 吃满整串）就成了让位对象：英文方案打 `da` 被拆成「精确英文 → 短语 → 前缀英文」。
//!
//! 词库缺失时整族跳过（worktree 里要自备 `build_dev/data`，核对耗时）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_phrase::PhraseSeed;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
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
    }
}

/// 英文方案出厂不加载短语，用方案级 override 打开。
fn override_with_phrases() -> PathBuf {
    let dir = std::env::temp_dir().join("wind_en_prefix_phrases_ov");
    std::fs::create_dir_all(&dir).expect("建 override 目录失败");
    std::fs::write(dir.join("english.toml"), "[phrases]\nenabled = true\n")
        .expect("写 override 失败");
    dir
}

#[test]
fn phrases_do_not_split_english_block() {
    let d = data_dir();
    if !d.join("schemas/english/en.dict.yaml").exists() {
        eprintln!("跳过：英文词库不存在");
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["english".into()];
    cfg.schema.active = "english".into();
    // 首条原文会占头部一格，与本用例无关，关掉让英文块只剩词库候选。
    cfg.schema.english.raw_candidate = wind_config::config::RawCandidateMode::Off;
    let coord =
        Coordinator::new_headless_with_override(cfg, Some(&d), Some(override_with_phrases()));
    // 码比输入长的前缀短语：不在精确档，按规则排在英文块之后。
    coord.debug_install_phrases(vec![PhraseSeed {
        code: "date".into(),
        text: "前缀短语".into(),
        weight: 5000,
        position: 0,
        is_system: true,
        category: String::new(),
    }]);
    for c in "da".chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    let all = coord.debug_all_candidate_texts();
    let phrase = all
        .iter()
        .position(|t| t == "前缀短语")
        .unwrap_or_else(|| panic!("前提：打开短语后 `da` 应前缀命中短语，实际 {all:?}"));
    let last_english = all
        .iter()
        .rposition(|t| t.is_ascii())
        .expect("应有英文候选");
    assert!(
        phrase > last_english,
        "短语插进了英文块（英文块应整块在前、块内原序）：{:?}",
        &all[..(phrase + 3).min(all.len())]
    );
}
