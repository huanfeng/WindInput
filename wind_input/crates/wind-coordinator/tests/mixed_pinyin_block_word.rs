//! 混输（五笔拼音）的拼音段与拼音方案共用「屏蔽这个词」：混输里删拼音来源的多字词写进 `pinyin`
//! 桶的按文字屏蔽，混输与纯拼音方案都不再出；码表段出的同文词不受影响。拼音方案里屏蔽的词，
//! 混输拼音段同样不出。设计见 docs/design/system-dict-patch-and-candidate-adjust.md §8 第 12 条。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时整族静默跳过（判据是耗时 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;
use wind_ui_types::CandidateOp;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "wubi86_pinyin"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
        && d.join("schemas/pinyin/rime_frost.dict.yaml").exists()
}

macro_rules! skip_without_data {
    () => {
        if !has_schemas() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
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

fn type_keys(coord: &Coordinator, input: &str) {
    for ch in input.chars() {
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn config(active: &str) -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "wubi86_pinyin".into()];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.keys.select_key_groups.clear();
    c
}

fn fresh_store(name: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_mixblock_{name}.redb"));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

fn open(cfg: &Config, store: &Arc<Store>) -> Arc<Coordinator> {
    Coordinator::new_headless_with_store(cfg.clone(), Some(&data_dir()), Arc::clone(store))
}

fn candidates(cfg: &Config, store: &Arc<Store>, input: &str) -> Vec<String> {
    let coord = open(cfg, store);
    type_keys(&coord, input);
    coord.debug_all_candidate_texts()
}

fn has(cfg: &Config, store: &Arc<Store>, input: &str, word: &str) -> bool {
    candidates(cfg, store, input).iter().any(|t| t == word)
}

fn blocked(store: &Store) -> Vec<String> {
    let mut v: Vec<String> = store
        .blocked_words("pinyin")
        .unwrap()
        .iter()
        .cloned()
        .collect();
    v.sort();
    v
}

/// ★ 混输打 `nihao`（超码长，拼音段）删「你好」：写 `pinyin` 桶的屏蔽行，混输与纯拼音都不再出，
/// 混输里码表段的「你好」（五笔 `wqvb`）照出。编码栏仍是拼音切分。
#[test]
fn mixed_pinyin_delete_blocks_in_pinyin_bucket() {
    skip_without_data!();
    let mix = config("wubi86_pinyin");
    let py = config("pinyin");
    let store = fresh_store("del");
    assert!(
        has(&mix, &store, "nihao", "你好"),
        "前提：混输 nihao 有「你好」"
    );
    assert!(
        has(&mix, &store, "wqvb", "你好"),
        "前提：五笔码 wqvb 有「你好」"
    );
    let coord = open(&mix, &store);
    type_keys(&coord, "nihao");
    let page = coord.debug_page_texts();
    let idx = page
        .iter()
        .position(|t| t == "你好")
        .unwrap_or_else(|| panic!("前提：首页应有「你好」，实际 {page:?}"));
    coord.debug_candidate_op(CandidateOp::Delete, idx);
    assert_eq!(blocked(&store), vec!["你好".to_string()]);
    assert!(
        store.list_shadow_rules("wubi86_pinyin").unwrap().is_empty(),
        "不写混输自己桶的按编码规则"
    );
    let coord = open(&mix, &store);
    type_keys(&coord, "nihao");
    assert!(
        !coord
            .debug_all_candidate_texts()
            .iter()
            .any(|t| t == "你好"),
        "混输 nihao 不再出"
    );
    assert_eq!(coord.debug_preedit(), "ni'hao", "编码栏仍是拼音切分");
    assert!(!has(&py, &store, "nihao", "你好"), "纯拼音同样不出");
    assert!(!has(&py, &store, "nh", "你好"), "纯拼音简拼同样不出");
    assert!(has(&mix, &store, "wqvb", "你好"), "码表段的同文词照出");
}

/// 拼音方案里屏蔽的词，混输拼音段也不出（引擎建图排除）；码表段不受影响。
#[test]
fn pinyin_block_applies_to_mixed_pinyin_segment() {
    skip_without_data!();
    let mix = config("wubi86_pinyin");
    let store = fresh_store("from_py");
    assert!(has(&mix, &store, "shijian", "时间"), "前提");
    store.block_word("pinyin", "时间").unwrap();
    assert!(!has(&mix, &store, "shijian", "时间"), "混输拼音段不出");
    assert!(
        !has(&mix, &store, "shijiandaole", "时间"),
        "更长的混输拼音输入里也不作为词节点出现"
    );
    assert!(
        has(&mix, &store, "jfuj", "时间"),
        "五笔码 jfuj 的「时间」照出"
    );
}

/// 混输拼音段删多字用户词：按拼音桶真删所有编码条目并屏蔽。
#[test]
fn mixed_pinyin_user_word_purged_and_blocked() {
    skip_without_data!();
    let mix = config("wubi86_pinyin");
    let store = fresh_store("user");
    store
        .add_user_word("pinyin", "qingfengshuru", "清风输入", 0, 0)
        .unwrap();
    store
        .add_user_word("pinyin", "qfsr", "清风输入", 0, 0)
        .unwrap();
    assert!(has(&mix, &store, "qingfengshuru", "清风输入"), "前提");
    let coord = open(&mix, &store);
    type_keys(&coord, "qingfengshuru");
    let page = coord.debug_page_texts();
    let idx = page.iter().position(|t| t == "清风输入").unwrap();
    coord.debug_candidate_op(CandidateOp::Delete, idx);
    assert!(
        store
            .get_user_words("pinyin", "qingfengshuru")
            .unwrap()
            .is_empty()
    );
    assert!(store.get_user_words("pinyin", "qfsr").unwrap().is_empty());
    assert_eq!(blocked(&store), vec!["清风输入".to_string()]);
    assert!(!has(&mix, &store, "qingfengshuru", "清风输入"));
}

/// 整句恰为被屏蔽的词：混输的拼音子引擎同样换下一条**覆盖整串**的整句（建图排除 + N-best 兜底），
/// 首选不退成只覆盖前半截的「时间到」，空格上屏后缓冲清空。
#[test]
fn mixed_blocked_sentence_top_still_covers_whole_input() {
    skip_without_data!();
    let mix = config("wubi86_pinyin");
    let store = fresh_store("sentence");
    let before = candidates(&mix, &store, "shijiandaole");
    assert_eq!(before.first().map(String::as_str), Some("时间到了"), "前提");
    store.block_word("pinyin", "时间到了").unwrap();
    let coord = open(&mix, &store);
    type_keys(&coord, "shijiandaole");
    let top = coord.debug_all_candidate_texts()[0].clone();
    assert_ne!(top, "时间到了");
    assert_eq!(top.chars().count(), 4, "首选覆盖四个音节，实际「{top}」");
    let text = match coord.handle_key_event_policed(&key(0x20)) {
        wind_bridge::handler::KeyAction::InsertText { text, .. } => text,
        other => panic!("空格应上屏，实际 {other:?}"),
    };
    assert_eq!(text, top);
    assert!(coord.debug_input_buffer().is_empty(), "整串上屏");
}

/// 翻页找候选并选中，返回上屏文本（None = 仍在组合区）。
fn pick(coord: &Coordinator, want: &str) -> Option<String> {
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if t.is_empty() {
            return None;
        }
        if let Some(p) = t.iter().position(|x| x == want) {
            return match coord.handle_key_event_policed(&key(0x31 + p as u32)) {
                wind_bridge::handler::KeyAction::InsertText { text, .. } => Some(text),
                _ => None,
            };
        }
        coord.handle_key_event_policed(&key(0x22)); // PageDown
    }
    None
}

/// 混输拼音段分段上屏拼出被屏蔽的词：上屏照常、不学进拼音桶。
#[test]
fn mixed_segmented_commit_of_blocked_word_is_not_learned() {
    skip_without_data!();
    let mut cfg = config("wubi86_pinyin");
    cfg.schema.pinyin.auto_learn.enabled = true;
    cfg.schema.pinyin.auto_learn.promote_count = 1;
    let learned = |s: &Store| {
        s.get_user_words("pinyin", "nihao")
            .unwrap()
            .iter()
            .chain(s.get_temp_words("pinyin", "nihao").unwrap().iter())
            .any(|r| r.text == "你好")
    };
    // 对照：没屏蔽时分段上屏「你」「好」学得进去
    let store = fresh_store("seg");
    let coord = open(&cfg, &store);
    type_keys(&coord, "nihao");
    assert!(pick(&coord, "你").is_none(), "选「你」应留在组合区分步");
    assert_eq!(pick(&coord, "好").as_deref(), Some("你好"));
    assert!(learned(&store), "前提：混输拼音段分步组词会学进拼音桶");

    let store = fresh_store("seg_blocked");
    store.block_word("pinyin", "你好").unwrap();
    let coord = open(&cfg, &store);
    type_keys(&coord, "nihao");
    assert!(pick(&coord, "你").is_none());
    assert_eq!(pick(&coord, "好").as_deref(), Some("你好"), "上屏照常");
    assert!(!learned(&store), "屏蔽的词不学");
}
