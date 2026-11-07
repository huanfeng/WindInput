//! 以词定字（`keys.select_char_keys`）的词频记账：按**那个字自己的编码**记，拿不到可靠
//! 编码时不记词频、只记上屏历史。
//!
//! 现场：`handle_select_char` 拿整词的码记单字（拼音下 `nihao` + 「好」），读端打 `hao`
//! 时查的是 `(hao, 好)`，那一行永远查不中——孤儿行。
//!
//! 口径：
//! - 拼音来源：候选带音节边界（词典真值 `boundary`）时按边界切出第 N 个音节记（`hao` + 「好」）；
//!   音节数与字数对不上 / 没有边界 ⇒ 不记词频。
//! - 码表来源：码表词频按**输入码位**记（码位独立），而这个字在用户打的这个码位下并不是
//!   候选；改记它自己的全码也不对——用户多半用简码打它，全码那一行照样读不到。⇒ 不记词频。
//! - 其余来源（英文 / 短语 / 无来源）同理不记词频。
//! - 上屏历史一律照记（`;` 重复上屏取得到）。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const VK_COMMA: u32 = 0xBC;
const VK_PERIOD: u32 = 0xBE;
const VK_SEMICOLON: u32 = 0xBA;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
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

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// `,` 取第 1 字、`.` 取第 2 字；两个方案都开词频。
fn open(tag: &str, active: &str) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let mut c = Config::default();
    c.schema.available = vec![active.into(), "wubi86".into(), "pinyin".into()];
    c.schema.available.dedup();
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.keys.select_char_keys = vec!["comma_period".into()];
    c.schema.pinyin.frequency.enabled = true;
    c.schema.codetable.frequency.enabled = true;
    let db = std::env::temp_dir().join(format!(
        "wind_select_char_freq_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    (coord, store, db)
}

fn inserted(act: KeyAction) -> String {
    match act {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("前提：以词定字应上屏，实际: {other:?}"),
    }
}

/// 该方案桶里所有 `text` 相同的词频行的码。
fn freq_codes_of(store: &Store, schema: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    store
        .for_each_freq(schema, "", &mut |code, t, _| {
            if t == text {
                out.push(code.to_string());
            }
            true
        })
        .unwrap();
    out
}

/// 进快捷输入（空缓冲）断言重复上屏候选就是 `want`（即它进了上屏历史）。
fn assert_repeat(coord: &Coordinator, want: &str) {
    coord.handle_key_event(&key(VK_SEMICOLON));
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some(want),
        "应进上屏历史（`;` 重复上屏取得到）"
    );
}

#[test]
fn pinyin_select_char_records_char_syllable() {
    skip_without_data!();
    let (coord, store, db) = open("py", "pinyin");
    type_str(&coord, "nihao");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("你好"),
        "前提：`nihao` 首选「你好」"
    );
    assert_eq!(inserted(coord.handle_key_event(&key(VK_PERIOD))), "好");
    assert_eq!(
        freq_codes_of(&store, "pinyin", "好"),
        vec!["hao".to_string()],
        "应按「好」自己的音节记，不是整词码 nihao"
    );
    type_str(&coord, "nihao");
    assert_eq!(inserted(coord.handle_key_event(&key(VK_COMMA))), "你");
    assert_eq!(
        freq_codes_of(&store, "pinyin", "你"),
        vec!["ni".to_string()]
    );
    let _ = std::fs::remove_file(&db);
}

#[test]
fn codetable_select_char_records_history_only() {
    skip_without_data!();
    let (coord, store, db) = open("wb", "wubi86");
    type_str(&coord, "aadg");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("工厂"),
        "前提：`aadg` 首选「工厂」"
    );
    assert_eq!(inserted(coord.handle_key_event(&key(VK_PERIOD))), "厂");
    assert!(
        freq_codes_of(&store, "wubi86", "厂").is_empty(),
        "码表下拿不到可靠的单字码位，不该记词频（整词码 aadg 下「厂」是孤儿行）"
    );
    assert_repeat(&coord, "厂");
    let _ = std::fs::remove_file(&db);
}

/// 双拼方案：候选码是全拼语义（`hao`），不是击键（小鹤 `hc`）——按全拼音节码记，
/// 读端单打「好」查的也是 `hao`。
#[test]
fn shuangpin_select_char_records_full_pinyin_syllable() {
    skip_without_data!();
    if !data_dir().join("schemas/shuangpin.schema.toml").exists() {
        eprintln!("跳过：缺双拼方案");
        return;
    }
    let (coord, store, db) = open("sp", "shuangpin");
    type_str(&coord, "nihc");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("你好"),
        "前提：小鹤 `nihc` 首选「你好」"
    );
    assert_eq!(inserted(coord.handle_key_event(&key(VK_PERIOD))), "好");
    assert_eq!(
        freq_codes_of(&store, "pinyin", "好"),
        vec!["hao".to_string()],
        "双拼下按全拼音节码记"
    );
    let _ = std::fs::remove_file(&db);
}
