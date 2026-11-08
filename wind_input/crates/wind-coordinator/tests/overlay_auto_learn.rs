//! 临时拼音 / 快捷输入的自动造词：闸门与归属按 **overlay 实际用的拼音方案**判，不按活跃方案。
//!
//! 现场：主方案是纯码表（五笔）时，`learn_phrase_on_commit` 开头的 `is_codetable()` 问的是
//! **活跃**方案 ⇒ 临拼 / 快捷输入里分步组出的拼音词一个也学不到，而它们产出的恰恰是拼音词。
//!
//! 同一处的归属也按活跃方案取：纯拼音主方案下快捷输入先分步选拼音「你好」、再选英文
//! `help`，会学出「你好help」写进拼音桶——各段来源不同，这条码谁也打不出来。
//!
//! - [`temp_pinyin_learns_under_codetable_active`] / [`quick_mix_learns_under_codetable_active`]：
//!   五笔主方案下两条 overlay 分步组词都要学进 `pinyin` 桶；
//! - [`quick_mix_unowned_tail_does_not_learn`]：先分步选拼音「你好」、末段选英文 `hel`，
//!   末段候选**归不到成员**（`mix_candidate_owner` 为 `None`）⇒ 不造词。它钉的只是
//!   「归不到成员不造词」这一道闸：实测同时放开成员类型闸门、删掉各段同源守卫，它照样绿。
//!   同源守卫由 `freq_learn_tests.rs` 的 `overlay_mixed_source_segments_are_not_learned`
//!   直接构造分段钉住。
//!   同配置下纯拼音分步是**正向对照**——缺了它，「快捷输入整体不造词」也能让本用例假绿。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const VK_BACKTICK: u32 = 0xC0;
const VK_SEMICOLON: u32 = 0xBA;
const VK_NEXT: u32 = 0x22;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists()
        && d.join("schemas/pinyin.schema.toml").exists()
        && d.join("schemas/english.schema.toml").exists()
}

fn key(k: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn cfg(active: &str) -> Config {
    let mut c = Config::default();
    c.schema.available = vec![active.into(), "pinyin".into(), "english".into()];
    c.schema.available.dedup();
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.schema.pinyin.auto_learn.enabled = true;
    c
}

fn open(tag: &str, c: Config) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let db = std::env::temp_dir().join(format!(
        "wind_overlay_learn_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    (coord, store, db)
}

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 翻页找候选并数字键选中，返回按键结果。
fn pick(coord: &Coordinator, want: impl Fn(&str) -> bool) -> KeyAction {
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        assert!(!t.is_empty(), "候选页为空");
        if let Some(p) = t.iter().position(|x| want(x)) {
            return coord.handle_key_event(&key(0x31 + p as u32));
        }
        coord.handle_key_event(&key(VK_NEXT));
    }
    panic!("翻 40 页没找到目标候选");
}

fn committed(act: &KeyAction) -> Option<String> {
    match act {
        KeyAction::InsertText { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn temp_texts(store: &Store, schema: &str) -> Vec<(String, String)> {
    store
        .search_temp_words_prefix(schema, "", 500)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.code, r.text))
        .collect()
}

/// 分步选「你」「好」（入口键由调用方按过、`nihao` 已打入）。
fn compose_ni_hao(coord: &Coordinator) {
    assert_eq!(
        committed(&pick(coord, |t| t == "你")),
        None,
        "前提：选「你」应留在组合区分步"
    );
    assert_eq!(
        committed(&pick(coord, |t| t == "好")).as_deref(),
        Some("你好"),
        "前提：再选「好」应整体上屏"
    );
}

#[test]
fn temp_pinyin_learns_under_codetable_active() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, db) = open("tp_wubi", cfg("wubi86"));
    coord.handle_key_event(&key(VK_BACKTICK));
    assert!(coord.debug_in_temp_pinyin(), "前提：反引号进临拼");
    type_str(&coord, "nihao");
    compose_ni_hao(&coord);
    assert_eq!(
        store.get_temp_word("pinyin", "nihao", "你好").unwrap(),
        Some(1),
        "五笔主方案下临拼分步组词应学进拼音桶；pinyin 桶: {:?}，wubi86 桶: {:?}",
        temp_texts(&store, "pinyin"),
        temp_texts(&store, "wubi86"),
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn quick_mix_learns_under_codetable_active() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, db) = open("mix_wubi", cfg("wubi86"));
    coord.handle_key_event(&key(VK_SEMICOLON));
    type_str(&coord, "nihao");
    compose_ni_hao(&coord);
    assert_eq!(
        store.get_temp_word("pinyin", "nihao", "你好").unwrap(),
        Some(1),
        "五笔主方案下快捷输入分步组词应学进拼音桶；pinyin 桶: {:?}，wubi86 桶: {:?}",
        temp_texts(&store, "pinyin"),
        temp_texts(&store, "wubi86"),
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn quick_mix_unowned_tail_does_not_learn() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    // 正向对照：同配置下纯拼音分步能学（否则下面的「没学」不说明混源守卫生效）。
    let (coord, store, db) = open("mix_py_ctrl", cfg("pinyin"));
    coord.handle_key_event(&key(VK_SEMICOLON));
    type_str(&coord, "nihao");
    compose_ni_hao(&coord);
    assert_eq!(
        store.get_temp_word("pinyin", "nihao", "你好").unwrap(),
        Some(1),
        "前提：纯拼音主方案下快捷输入分步组词应学到"
    );
    let _ = std::fs::remove_file(&db);

    let (coord, store, db) = open("mix_py_mixed", cfg("pinyin"));
    coord.handle_key_event(&key(VK_SEMICOLON));
    type_str(&coord, "nihaohel");
    assert_eq!(
        committed(&pick(&coord, |t| t == "你好")),
        None,
        "前提：选「你好」应留在组合区分步"
    );
    let out = committed(&pick(&coord, |t| {
        t.starts_with("hel") || t.starts_with("Hel")
    }))
    .expect("前提：选英文候选应整体上屏");
    assert!(out.starts_with("你好"), "前提：上屏含前缀「你好」: {out}");
    for schema in ["pinyin", "english"] {
        let bad: Vec<_> = temp_texts(&store, schema)
            .into_iter()
            .filter(|(_, t)| t.starts_with("你好") && t.chars().any(|c| c.is_ascii()))
            .collect();
        assert!(
            bad.is_empty(),
            "末段归不到成员不得造词，{schema} 桶学出了: {bad:?}"
        );
    }
    let _ = std::fs::remove_file(&db);
}
