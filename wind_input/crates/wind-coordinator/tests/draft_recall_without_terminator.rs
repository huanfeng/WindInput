//! 刚造出来的词，**不经任何断流信号**就要能召回。
//!
//! # 现场（2026-09-22 靶机 192.168.5.30）
//!
//! 用户打「幻」(xnn) 「枫」(smq) 两个单字，再直接打 `xnsm`（五笔二字词规则 AaAbBaBb
//! ⇒ 各字全码前两位 ⇒ xn + sm），召不回「幻枫」。日志显示第一条 `draft: 落库` 直到
//! 一分半后焦点丢失断流才发生——他打 `xnsm` 的时候草稿还躺在**内存队列**里。
//!
//! 根因是 `enqueue_drafts` 当时有个「攒够 `draft_flush_batch`(64) 条才落库」的门槛：
//! 打两个单字只产出 1 条草稿，离 64 差得远。而「刚打完一个词、马上想用它」恰恰是
//! 用户最期待的时刻，功能却恰恰在这一刻不工作。
//!
//! # 这条护栏为什么必须独立存在
//!
//! `input_flow.rs` 里的草稿端到端测试**照不出**本缺陷：它们在断言之前都调了一次
//! `handle_focus_lost` 去「等落库」——那等于测试替被测系统做了它自己不做的事。
//! 本文件的要害就是**刻意不调任何断流**，与真实使用姿势一致。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    data_dir().join("schemas/wubi86.schema.toml").exists()
}

fn key_event(key_code: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

/// 打一串字母 + 空格上屏，返回上屏文本。
fn commit_code(coord: &Coordinator, code: &str) -> String {
    for ch in code.chars() {
        coord.handle_key_event_policed(&key_event(ch.to_ascii_uppercase() as u32, EVENT_KEY_DOWN));
    }
    match coord.handle_key_event_policed(&key_event(0x20, EVENT_KEY_DOWN)) {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("空格应上屏 InsertText，实际: {other:?}"),
    }
}

#[test]
fn a_fresh_draft_is_recallable_without_any_stream_terminator() {
    if !has_schemas() {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_phrase.enabled = true;

    let db = std::env::temp_dir().join("wind_draft_repro_xnsm.redb");
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), Arc::clone(&store));
    coord.prewarm_indexes();

    let a = commit_code(&coord, "xnn");
    let b = commit_code(&coord, "smq");
    eprintln!("上屏: {a:?} {b:?}");
    assert_eq!(a, "幻", "xnn 应上屏「幻」");
    assert_eq!(b, "枫", "smq 应上屏「枫」");

    // ⚠️ **刻意不调 `handle_focus_lost`**：靶机现场用户就是打完两个字直接打 xnsm 的，
    // 中间没有任何断流信号。原先的端到端测试在这里调了一次断流去「等落库」——
    // 那等于测试替被测系统做了它自己不做的事，于是恰好把本缺陷绕了过去。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut found = Vec::new();
    while std::time::Instant::now() < deadline {
        found = store.search_drafts("wubi86", "xnsm", 0).unwrap_or_default();
        if !found.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    eprintln!("草稿表 xnsm -> {found:?}");

    // 不管落没落，把 xnsm 打出来看候选
    for ch in "xnsm".chars() {
        coord.handle_key_event_policed(&key_event(ch.to_ascii_uppercase() as u32, EVENT_KEY_DOWN));
    }
    let cands = coord.candidate_window(0, 50).items;
    eprintln!("xnsm 候选: {cands:?}");

    assert!(
        !found.is_empty(),
        "打完 幻枫 两个单字后，草稿表里应有 xnsm -> 幻枫"
    );
    assert!(
        cands.iter().any(|t| t == "幻枫"),
        "打 xnsm 应能召回草稿「幻枫」，实际候选: {:?}",
        cands
    );
    let _ = std::fs::remove_file(&db);
}
