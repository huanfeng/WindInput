//! 英文用户词与同码的系统词相比恒在前，哪怕用户词权重是 0（导入的词表没有权重列）。
//!
//! 系统词按词频名次赋了权（至多 `RANK_WEIGHT_MAX`）之后，用户层若不托底，0 权重的用户词
//! 就会排到同码系统词之后——名次引入前双方都是 0，靠层级基序（用户 -3 < 系统 0）用户恒在前。
//!
//! 只测**同码精确**：前缀补全那一档另有层级键（用户/临时词层的补全带 `is_prefix`，系统码表
//! 层的不带，`cmp_match_layers` 先于权重），那里用户词本来就排在系统补全之后，与权重无关。
//! 词库缺失时整族跳过（worktree 里要自备 `build_dev/data`，核对耗时）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::RawCandidateMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

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

#[test]
fn zero_weight_user_word_leads_same_code_ranked_word() {
    let d = data_dir();
    if !d.join("schemas/english/en_freq.txt").exists() {
        eprintln!("跳过：英文词库或名次表不存在");
        return;
    }
    let path = std::env::temp_dir().join("wind_en_user_over_ranked.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    // 与系统词 product（有名次）同码、文本不同。
    store
        .add_user_word("english", "product", "ProductX", 0, 0)
        .unwrap();

    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "english".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&d), store);
    for c in "product".chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    let page = coord.debug_page_texts();
    let user = page.iter().position(|t| t == "ProductX");
    let sys = page.iter().position(|t| t == "product");
    assert!(
        user.is_some() && sys.is_some(),
        "前提：用户词与系统词都在页内，实际 {page:?}"
    );
    assert!(
        user < sys,
        "0 权重的用户词应排在同码的有名次系统词之前，实际 {page:?}"
    );
    let _ = std::fs::remove_file(&path);
}
