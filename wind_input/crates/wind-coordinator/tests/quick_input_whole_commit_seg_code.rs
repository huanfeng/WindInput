//! 快捷输入**整体上屏**那一段的造词码：取候选码（`cand_code`），不取原始缓冲。
//!
//! 现场：`;nih` 先选「你」（分步，段码 `ni`），剩余缓冲 `h` 再选「好」整体上屏。末段往
//! `committed_segs` 放的 `code` 是整个剩余缓冲 `h` ⇒ 造出 `("nih", "你好")`——这条码谁也打不
//! 出来，随后 `;nihao` 选「你好」时 6b（`bump_selected_temp_word`，点查用 `cand_code`）也对
//! 不上它。主路与临拼的整体上屏都取 `cand_code`；双拼下缓冲是击键（`hc`），更是与全拼码
//! 不同域。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const VK_SEMICOLON: u32 = 0xBA;
const VK_NEXT: u32 = 0x22;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english", "shuangpin"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
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

fn cfg(pinyin: &str) -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), pinyin.into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.schema.primary_pinyin = pinyin.into();
    c.input.default.chinese_mode = true;
    c.schema.pinyin.auto_learn.enabled = true;
    c
}

fn open(tag: &str, c: Config) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let db = std::env::temp_dir().join(format!(
        "wind_quick_seg_code_{tag}_{}.redb",
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

/// 翻页找候选并数字键选中，返回上屏文本（留在组合区时为 `None`）。
fn pick(coord: &Coordinator, want: &str) -> Option<String> {
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        assert!(!t.is_empty(), "候选页为空（找「{want}」）");
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

fn temp_rows(store: &Store) -> Vec<(String, String)> {
    store
        .search_temp_words_prefix("pinyin", "", 500)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.code, r.text))
        .collect()
}

/// `;{code}` 分步选「你」「好」。
fn compose(coord: &Coordinator, code: &str) {
    coord.handle_key_event(&key(VK_SEMICOLON));
    type_str(coord, code);
    assert_eq!(pick(coord, "你"), None, "前提：选「你」应留在组合区分步");
    assert_eq!(
        pick(coord, "好").as_deref(),
        Some("你好"),
        "前提：再选「好」应整体上屏"
    );
}

fn check(tag: &str, pinyin: &str, code: &str, whole: &str) {
    let (coord, store, db) = open(tag, cfg(pinyin));
    compose(&coord, code);
    assert_eq!(
        store.get_temp_word("pinyin", "nihao", "你好").unwrap(),
        Some(1),
        "`;{code}` 分步组词应按全拼码学成 (nihao, 你好)；拼音桶: {:?}",
        temp_rows(&store),
    );
    // 6b：再 `;nihao`（双拼 `;nihc`）选「你好」，推进的应是同一条临时词。
    coord.handle_key_event(&key(VK_SEMICOLON));
    type_str(&coord, whole);
    assert_eq!(
        pick(&coord, "你好").as_deref(),
        Some("你好"),
        "前提：选「你好」上屏"
    );
    assert_eq!(
        store.get_temp_word("pinyin", "nihao", "你好").unwrap(),
        Some(2),
        "再选「你好」应经 6b 推进同一条临时词；拼音桶: {:?}",
        temp_rows(&store),
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn whole_commit_seg_uses_candidate_code() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    check("qp", "pinyin", "nih", "nihao");
}

/// 出厂 `shuangpin` 是小鹤布局：`hc` = hao。整体上屏段的码须是全拼 `hao`，不是击键 `hc`。
#[test]
fn whole_commit_seg_uses_full_pinyin_code_under_shuangpin() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    check("sp", "shuangpin", "nihc", "nihc");
}
