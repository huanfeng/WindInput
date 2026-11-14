//! 临时拼音 / 快捷输入里选中**已有临时词**，要推进晋升计数（6b）——与主输入路同一套。
//!
//! 现场：主路 `commit_selected` 选中临时词会 `learn_temp_word` 一次、达阈值晋升进用户词库；
//! 临拼整体上屏与快捷输入 `mix_select_at` 没有这一步 ⇒ 在 overlay 里选多少次都不涨计数，
//! 词永远晋升不了。
//!
//! 主方案刻意取五笔：6b 的归属若按活跃方案取（`wubi86` 桶），点查拼音临时词必然落空，
//! 用例才分得出「按 overlay 方案」与「按活跃方案」。
//!
//! 晋升仍经 `maybe_promote_temp`（含全拼校验）——对照 `pinyin_learn_code.rs` 的
//! `legacy_mixed_code_is_not_auto_promoted`，这里只验计数推进与合法全拼的晋升。
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
/// `ni|hao` 的音节起点位（0、2）。
const NIHAO_BOUNDARY: u64 = 0b101;
const PROMOTE: usize = 3;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("schemas/pinyin.schema.toml").exists()
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

fn cfg() -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.pinyin.auto_learn.enabled = true;
    c.schema.pinyin.auto_learn.promote_count = PROMOTE;
    c
}

/// 预置 `count = seeded` 的拼音临时词「你好」，返回 (coord, store, 库路径)。
fn open(tag: &str, seeded: usize) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let db = std::env::temp_dir().join(format!(
        "wind_overlay_promote_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    for _ in 0..seeded {
        store
            .learn_temp_word("pinyin", "nihao", "你好", 800, NIHAO_BOUNDARY)
            .unwrap();
    }
    let coord = Coordinator::new_headless_with_store(cfg(), Some(&data_dir()), Arc::clone(&store));
    (coord, store, db)
}

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn pick(coord: &Coordinator, want: &str) -> Option<String> {
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        assert!(!t.is_empty(), "候选页为空");
        if let Some(p) = t.iter().position(|x| x == want) {
            return match coord.handle_key_event(&key(0x31 + p as u32)) {
                KeyAction::InsertText { text, .. } => Some(text),
                _ => None,
            };
        }
        coord.handle_key_event(&key(VK_NEXT));
    }
    panic!("翻 40 页没找到「{want}」");
}

fn promoted(store: &Store) -> bool {
    store
        .get_user_words("pinyin", "nihao")
        .unwrap()
        .iter()
        .any(|r| r.text == "你好")
}

/// `enter` = 入口键；选一次「你好」整体上屏。
fn select_once(tag: &str, enter: u32, seeded: usize) -> (Arc<Store>, PathBuf) {
    let (coord, store, db) = open(tag, seeded);
    coord.handle_key_event(&key(enter));
    type_str(&coord, "nihao");
    assert_eq!(
        pick(&coord, "你好").as_deref(),
        Some("你好"),
        "前提：整体上屏"
    );
    (store, db)
}

#[test]
fn temp_pinyin_select_bumps_temp_count() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (store, db) = select_once("tp_bump", VK_BACKTICK, 1);
    assert_eq!(
        store.get_temp_word("pinyin", "nihao", "你好").unwrap(),
        Some(2),
        "临拼选中已有临时词应推进计数"
    );
    assert!(!promoted(&store), "前提：未达阈值不晋升");
    let _ = std::fs::remove_file(&db);
}

#[test]
fn temp_pinyin_select_promotes_at_threshold() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (store, db) = select_once("tp_promo", VK_BACKTICK, PROMOTE - 1);
    assert!(promoted(&store), "临拼选中推到阈值应晋升进拼音用户词库");
    let _ = std::fs::remove_file(&db);
}

#[test]
fn quick_mix_select_bumps_temp_count() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (store, db) = select_once("mix_bump", VK_SEMICOLON, 1);
    assert_eq!(
        store.get_temp_word("pinyin", "nihao", "你好").unwrap(),
        Some(2),
        "快捷输入选中已有临时词应推进计数"
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn quick_mix_select_promotes_at_threshold() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (store, db) = select_once("mix_promo", VK_SEMICOLON, PROMOTE - 1);
    assert!(promoted(&store), "快捷输入选中推到阈值应晋升进拼音用户词库");
    let _ = std::fs::remove_file(&db);
}

/// **+2 防护**：选中的是引擎新合成的整句（`is_synthesized`），造词与 6b 点查同一个键。
/// 造词已 +1，6b 必须跳过（`learned_code` 比对），否则一次上屏计数 +2。
///
/// 整句取词库里没有的一句，保证首选确是合成整句而非词典词；预置的临时词与它同码同文。
#[test]
fn temp_pinyin_synthesized_sentence_counts_once() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    const CODE: &str = "wojintianqushangban";
    const SENT: &str = "我今天去上班";
    let db = std::env::temp_dir().join(format!(
        "wind_overlay_promote_sent_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    store.learn_temp_word("pinyin", CODE, SENT, 800, 0).unwrap();
    let coord = Coordinator::new_headless_with_store(cfg(), Some(&data_dir()), Arc::clone(&store));
    coord.handle_key_event(&key(VK_BACKTICK));
    type_str(&coord, CODE);
    assert_eq!(
        pick(&coord, SENT).as_deref(),
        Some(SENT),
        "前提：整句整体上屏"
    );
    assert_eq!(
        store.get_temp_word("pinyin", CODE, SENT).unwrap(),
        Some(2),
        "同一次上屏只能 +1（造词与 6b 不得各记一次）"
    );
    let _ = std::fs::remove_file(&db);
}
