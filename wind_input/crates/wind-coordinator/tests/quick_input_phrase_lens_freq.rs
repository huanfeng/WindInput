//! 快捷输入**词组透镜**（英文分词 `ip'pro`）选词要记词频，与临英同口径。
//!
//! 现场：`mix_select_at` 与标点顶屏把 `MixLens::commits_whole()` 兼作「有没有码可记」的
//! 判据。词组透镜整体上屏（`commits_whole() == true`）是对的，但它的候选是英文词库词、
//! 有码可记——被当成数字透镜只记了上屏历史，词频一条不写。临英下同一次选词写
//! `english` 桶 `("iphone", "iPhone 15 Pro")`。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::FreeInputMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

const VK_QUOTE: u32 = 0xDE;
const VK_SEMICOLON: u32 = 0xBA;
const VK_NEXT: u32 = 0x22;
const VK_COMMA: u32 = 0xBC;
const WANT: &str = "iPhone 15 Pro";

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists()
        && d.join("schemas/english.schema.toml").exists()
        && d.join("schemas/english").is_dir()
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

fn open(tag: &str) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    open_with(tag, |_| {})
}

fn open_with(tag: &str, edit: impl FnOnce(&mut Config)) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    c.input.temp_english.phrase_seg = true;
    // 英文词频出厂关闭（`Config::default()` 同），不开则两边都不记、对照失去意义。
    c.schema.english.frequency.enabled = true;
    edit(&mut c);
    let db = std::env::temp_dir().join(format!(
        "wind_quick_phrase_freq_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    (coord, store, db)
}

fn type_rest(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        if ch == '\'' {
            coord.handle_key_event(&key(VK_QUOTE));
        } else {
            coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
        }
    }
}

/// 翻页找到 `WANT` 并选中，断言整体上屏。`mouse`：临英里数字是内容不是选词键，改用点选
/// （与键盘选词同一出口 `commit_temp_english_selected`）。
fn pick_want(coord: &Coordinator, mouse: bool) {
    for _ in 0..20 {
        let t = coord.debug_page_texts();
        if let Some(p) = t.iter().position(|x| x == WANT) {
            let act = if mouse {
                coord.debug_mouse_select(p).expect("点选应带回动作")
            } else {
                coord.handle_key_event(&key(0x31 + p as u32))
            };
            match act {
                KeyAction::InsertText { text, .. } => {
                    assert!(text.starts_with(WANT), "前提：上屏 {WANT}，实际: {text}")
                }
                other => panic!("前提：选 {WANT} 应整体上屏，实际: {other:?}"),
            }
            return;
        }
        coord.handle_key_event(&key(VK_NEXT));
    }
    panic!("前提：候选里应有 {WANT}");
}

fn english_count(store: &Store) -> Option<u32> {
    store
        .get_freq("english", "iphone", WANT)
        .unwrap()
        .map(|r| r.count)
}

#[test]
fn quick_input_phrase_lens_records_freq() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, db) = open("quick");
    coord.handle_key_event(&key(VK_SEMICOLON));
    type_rest(&coord, "ip'pro");
    pick_want(&coord, false);
    assert_eq!(
        english_count(&store),
        Some(1),
        "快捷输入词组透镜选词应记英文桶 (iphone, {WANT})，同临英"
    );
    let _ = std::fs::remove_file(&db);
}

/// 对照：临英同一次选词的记账（快捷输入要对齐的口径）。
#[test]
fn temp_english_phrase_records_freq_control() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, db) = open("temp");
    coord.handle_key_event(&KeyEventData {
        key_code: u32::from(b'I'),
        modifiers: MOD_SHIFT,
        ..key(0)
    });
    type_rest(&coord, "p'pro");
    pick_want(&coord, true);
    assert_eq!(
        english_count(&store),
        Some(1),
        "前提：临英选 {WANT} 记英文桶 (iphone, {WANT})"
    );
    let _ = std::fs::remove_file(&db);
}

/// 标点顶屏（⑥，`free_input = off` 才走到）同一判据：高亮的词组候选随标点上屏也要记词频。
#[test]
fn quick_input_phrase_lens_punct_commit_records_freq() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, db) = open_with("punct", |c| {
        c.schema.mix_modes[0].free_input = FreeInputMode::Off;
    });
    coord.handle_key_event(&key(VK_SEMICOLON));
    type_rest(&coord, "ip'pro");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some(WANT),
        "前提：`;ip'pro` 首选是 {WANT}"
    );
    match coord.handle_key_event(&key(VK_COMMA)) {
        KeyAction::InsertText { text, .. } => {
            assert!(
                text.starts_with(WANT),
                "前提：顶屏上屏 {WANT}，实际: {text}"
            )
        }
        other => panic!("前提：`,` 应顶屏上屏，实际: {other:?}"),
    }
    assert_eq!(
        english_count(&store),
        Some(1),
        "词组透镜标点顶屏应记英文桶 (iphone, {WANT})"
    );
    let _ = std::fs::remove_file(&db);
}
