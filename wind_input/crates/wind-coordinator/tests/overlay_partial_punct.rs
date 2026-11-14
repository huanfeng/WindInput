//! 临拼 / 快捷输入高亮停在**分步候选**（只消费缓冲前缀）时按标点：与主路拼音同口径
//! （`top_commit_learn.rs::partial_highlight_top_commit_bumps_without_learning` 钉的那条）——
//! 顶屏「已选段 + 高亮候选」+ 标点，剩余码丢弃，退出模式；记词频与输入统计；
//! 不造词、只推 6b（同 091b8f0f 的 `learn_on_top_commit`）。
//!
//! 现场：临拼兜底标点臂对分步候选刻意排除，走 `commit_temp_pinyin_selected` 只确认那一段、
//! 留在临拼——标点被吞。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::FreeInputMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;
use wind_store::stats::CommitSource;

const VK_BACKTICK: u32 = 0xC0;
const VK_SEMICOLON: u32 = 0xBA;
const VK_COMMA: u32 = 0xBC;
const VK_NEXT: u32 = 0x22;
const VK_DOWN: u32 = 0x28;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("schemas/pinyin.schema.toml").exists()
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

/// 五笔主方案 + 拼音；快捷输入关自由输入（开着时标点在 ⑤ 作字面进缓冲）。
fn open(tag: &str) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.pinyin.auto_learn.enabled = true;
    c.schema.pinyin.frequency.enabled = true;
    c.schema.mix_modes[0].free_input = FreeInputMode::Off;
    let db = std::env::temp_dir().join(format!(
        "wind_overlay_partial_punct_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    (coord, store, db)
}

/// 打 `nihaoma`，分步选「你」，再把高亮移到分步候选「好」（只消费 `haoma` 的 `hao`）。
fn highlight_partial_hao(coord: &Coordinator) {
    type_str(coord, "nihaoma");
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if let Some(p) = t.iter().position(|x| x == "你") {
            let act = coord.handle_key_event(&key(0x31 + p as u32));
            assert!(
                !matches!(act, KeyAction::InsertText { .. }),
                "前提：选「你」应留在组合区分步，实际: {act:?}"
            );
            break;
        }
        coord.handle_key_event(&key(VK_NEXT));
    }
    let page = coord.debug_page_texts();
    let p = page
        .iter()
        .position(|x| x == "好")
        .unwrap_or_else(|| panic!("前提：`haoma` 当页应有分步候选「好」，实际: {page:?}"));
    for _ in 0..p {
        coord.handle_key_event(&key(VK_DOWN));
    }
}

fn assert_partial_top_commit(
    coord: &Coordinator,
    store: &Store,
    source: CommitSource,
    still_active: impl Fn(&Coordinator) -> bool,
) {
    coord.debug_capture_stat_events();
    match coord.handle_key_event(&key(VK_COMMA)) {
        KeyAction::InsertText { text, .. } => {
            assert_eq!(
                text, "你好，",
                "顶屏「已选段 + 高亮」+ 标点，剩余码丢弃（同主路）"
            )
        }
        other => panic!("分步高亮按标点应顶屏上屏，实际: {other:?}"),
    }
    assert!(!still_active(coord), "顶屏后应退出模式");
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![
            (source, "好".to_string()),
            (CommitSource::Punctuation, "，".to_string()),
        ],
        "被顶的高亮候选按本模式来源记一段，标点单独记"
    );
    assert_eq!(
        store.get_temp_word("pinyin", "nihao", "你好").unwrap(),
        None,
        "分步候选顶屏不应造词"
    );
    assert_eq!(
        store.get_temp_word("pinyin", "hao", "好").unwrap(),
        Some(2),
        "分步候选顶屏仍推 6b"
    );
}

#[test]
fn temp_pinyin_partial_highlight_punct_top_commits() {
    skip_without_data!();
    let (coord, store, db) = open("temp_py");
    store
        .learn_temp_word("pinyin", "hao", "好", 800, 0)
        .unwrap();
    coord.handle_key_event(&key(VK_BACKTICK));
    assert!(coord.debug_in_temp_pinyin(), "前提：反引号进临拼");
    highlight_partial_hao(&coord);
    assert_partial_top_commit(&coord, &store, CommitSource::TempPinyin, |c| {
        c.debug_in_temp_pinyin()
    });
    let _ = std::fs::remove_file(&db);
}

/// 快捷输入 ⑥ 同类情况（本就与主路一致，钉住口径）。
#[test]
fn quick_input_partial_highlight_punct_top_commits() {
    skip_without_data!();
    let (coord, store, db) = open("quick");
    store
        .learn_temp_word("pinyin", "hao", "好", 800, 0)
        .unwrap();
    coord.handle_key_event(&key(VK_SEMICOLON));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
    highlight_partial_hao(&coord);
    assert_partial_top_commit(&coord, &store, CommitSource::Mix, |c| {
        c.debug_active_mode().is_some()
    });
    let _ = std::fs::remove_file(&db);
}
