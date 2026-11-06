//! 顶屏类出口（标点顶屏 / 非码元字符与小键盘顶屏 / 进模式顶屏）也是一次选中：默认造词并
//! 推进 6b（临时词晋升计数），与各路选词出口同口径；内部开关 `input.top_commit_learn`
//! 关掉时各路（含临拼）一律不造词、不推 6b。
//!
//! 现场：主路 `nihao` 先分步选「你」、再按 `,` 顶屏「好」，上屏「你好，」却什么也没学——
//! 顶屏出口只记了词频，没把被顶的候选并进已转换段，`take_committed` 直接把段清掉了。
//! 临拼的标点顶屏走 `commit_temp_pinyin_selected`，本就会学；其余出口都不学。
//!
//! 归属口径同各路选词：主路用活跃方案、临拼用目标方案、快捷输入用成员方案——拼音族
//! 的临时词都落 `pinyin` 数据桶（`PINYIN_DATA_SCHEMA`）。
//!
//! 另钉三条口径：高亮是分步候选时顶屏只推 6b 不造词；次三选键越界 `commit_and_input`
//! 也是顶屏（跟随开关）；整句候选造词后 6b 不再重复计数（`learned_code` 防 +2）。
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

const VK_BACKTICK: u32 = 0xC0;
const VK_SEMICOLON: u32 = 0xBA;
const VK_COMMA: u32 = 0xBC;
const VK_NUMPAD0: u32 = 0x60;
const VK_NEXT: u32 = 0x22;
const VK_DOWN: u32 = 0x28;
const VK_PERIOD: u32 = 0xBE;
const VK_QUOTE: u32 = 0xDE;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english", "wubi86_pinyin"]
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

fn open(
    tag: &str,
    active: &str,
    edit: impl FnOnce(&mut Config),
) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let mut c = Config::default();
    c.schema.available = vec![active.into(), "pinyin".into(), "english".into()];
    c.schema.available.dedup();
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.schema.pinyin.auto_learn.enabled = true;
    edit(&mut c);
    let db = std::env::temp_dir().join(format!(
        "wind_top_commit_learn_{tag}_{}.redb",
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

/// 打 `nihao`（入口键由调用方按过），翻页找「你」分步选中，断言剩余码高亮是「好」。
fn step_ni(coord: &Coordinator) {
    type_str(coord, "nihao");
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        assert!(!t.is_empty(), "候选页为空（找「你」）");
        if let Some(p) = t.iter().position(|x| x == "你") {
            let act = coord.handle_key_event(&key(0x31 + p as u32));
            assert!(
                !matches!(act, KeyAction::InsertText { .. }),
                "前提：选「你」应留在组合区分步，实际: {act:?}"
            );
            assert_eq!(
                coord.debug_page_texts().first().map(String::as_str),
                Some("好"),
                "前提：剩余 `hao` 首选应是「好」"
            );
            return;
        }
        coord.handle_key_event(&key(VK_NEXT));
    }
    panic!("前提：候选里应有「你」");
}

/// 按 `vk` 顶屏，断言上屏文本以「你好」开头。
fn top_commit(coord: &Coordinator, vk: u32) {
    let text = match coord.handle_key_event(&key(vk)) {
        KeyAction::InsertText { text, .. } => text,
        KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text,
        KeyAction::CommitAndHoldComposition { commit_text, .. } => commit_text,
        other => panic!("前提：顶屏应上屏，实际: {other:?}"),
    };
    assert!(
        text.starts_with("你好"),
        "前提：顶屏上屏「你好…」，实际: {text}"
    );
}

fn nihao(store: &Store) -> Option<u32> {
    store.get_temp_word("pinyin", "nihao", "你好").unwrap()
}

fn temp_rows(store: &Store) -> Vec<(String, String)> {
    store
        .search_temp_words_prefix("pinyin", "", 500)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.code, r.text))
        .collect()
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
}

#[test]
fn main_pinyin_punct_top_commit_learns() {
    skip_without_data!();
    let (coord, store, db) = open("main_py_punct", "pinyin", |_| {});
    step_ni(&coord);
    top_commit(&coord, VK_COMMA);
    assert_eq!(nihao(&store), Some(1), "拼音桶: {:?}", temp_rows(&store));
    let _ = std::fs::remove_file(&db);
}

/// 混输（五笔+拼音）方案的拼音分步：码表侧的 `punct_commit` 出厂关，须拨开才顶屏。
#[test]
fn main_wubi_pinyin_punct_top_commit_learns() {
    skip_without_data!();
    let (coord, store, db) = open("main_wp_punct", "wubi86_pinyin", |c| {
        c.schema.codetable.punct_commit = true;
    });
    step_ni(&coord);
    top_commit(&coord, VK_COMMA);
    assert_eq!(nihao(&store), Some(1), "拼音桶: {:?}", temp_rows(&store));
    let _ = std::fs::remove_file(&db);
}

/// 非码元字符 / 小键盘顶屏（`commit_highlight_then_char`）。
#[test]
fn main_pinyin_numpad_top_commit_learns() {
    skip_without_data!();
    let (coord, store, db) = open("main_py_numpad", "pinyin", |_| {});
    step_ni(&coord);
    top_commit(&coord, VK_NUMPAD0);
    assert_eq!(nihao(&store), Some(1), "拼音桶: {:?}", temp_rows(&store));
    let _ = std::fs::remove_file(&db);
}

/// 6b：已有临时词被顶屏时推进计数。
#[test]
fn main_pinyin_punct_top_commit_bumps_temp_word() {
    skip_without_data!();
    let (coord, store, db) = open("main_py_6b", "pinyin", |_| {});
    store
        .learn_temp_word("pinyin", "nihao", "你好", 800, 0)
        .unwrap();
    type_str(&coord, "nihao");
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some("你好"),
        "前提：`nihao` 首选「你好」"
    );
    top_commit(&coord, VK_COMMA);
    assert_eq!(nihao(&store), Some(2), "顶屏应推进同一条临时词");
    let _ = std::fs::remove_file(&db);
}

/// 进模式顶屏（`take_committed_with_highlight`）：混输方案下分步选「你」后按 `` ` `` 进临拼。
#[test]
fn enter_mode_top_commit_learns() {
    skip_without_data!();
    let (coord, store, db) = open("enter_mode", "wubi86_pinyin", |_| {});
    step_ni(&coord);
    top_commit(&coord, VK_BACKTICK);
    assert!(coord.debug_in_temp_pinyin(), "前提：顶屏后进临拼");
    assert_eq!(nihao(&store), Some(1), "拼音桶: {:?}", temp_rows(&store));
    let _ = std::fs::remove_file(&db);
}

/// 快捷输入 ⑥ 标点顶屏（`free_input = off` 才走到）：归属取成员方案。
#[test]
fn quick_input_punct_top_commit_learns() {
    skip_without_data!();
    let (coord, store, db) = open("quick_punct", "wubi86", |c| {
        c.schema.mix_modes[0].free_input = FreeInputMode::Off;
    });
    coord.handle_key_event(&key(VK_SEMICOLON));
    step_ni(&coord);
    top_commit(&coord, VK_COMMA);
    assert_eq!(nihao(&store), Some(1), "拼音桶: {:?}", temp_rows(&store));
    let _ = std::fs::remove_file(&db);
}

/// 临拼小键盘顶屏同样学。
#[test]
fn temp_pinyin_numpad_top_commit_learns() {
    skip_without_data!();
    let (coord, store, db) = open("temp_numpad", "wubi86", |_| {});
    coord.handle_key_event(&key(VK_BACKTICK));
    step_ni(&coord);
    top_commit(&coord, VK_NUMPAD0);
    assert_eq!(nihao(&store), Some(1), "拼音桶: {:?}", temp_rows(&store));
    let _ = std::fs::remove_file(&db);
}

/// 开关关掉：临拼的标点顶屏（修前本就会学的那一路）也不学。
#[test]
fn switch_off_temp_pinyin_punct_top_commit_does_not_learn() {
    skip_without_data!();
    // 正向对照：开关默认开时这一路学得到（否则「没学」证明不了开关生效）。
    let (coord, store, db) = open("temp_punct_on", "wubi86", |_| {});
    coord.handle_key_event(&key(VK_BACKTICK));
    step_ni(&coord);
    top_commit(&coord, VK_COMMA);
    assert_eq!(nihao(&store), Some(1), "前提：开关开时临拼标点顶屏造词");
    let _ = std::fs::remove_file(&db);

    let (coord, store, db) = open("temp_punct_off", "wubi86", |c| {
        c.input.top_commit_learn = false;
    });
    coord.handle_key_event(&key(VK_BACKTICK));
    step_ni(&coord);
    top_commit(&coord, VK_COMMA);
    assert_eq!(
        nihao(&store),
        None,
        "开关关时顶屏不造词；拼音桶: {:?}",
        temp_rows(&store)
    );
    let _ = std::fs::remove_file(&db);
}

/// 开关关掉：主路标点顶屏不推 6b。
#[test]
fn switch_off_main_punct_top_commit_does_not_bump() {
    skip_without_data!();
    let (coord, store, db) = open("main_6b_off", "pinyin", |c| {
        c.input.top_commit_learn = false;
    });
    store
        .learn_temp_word("pinyin", "nihao", "你好", 800, 0)
        .unwrap();
    type_str(&coord, "nihao");
    top_commit(&coord, VK_COMMA);
    assert_eq!(nihao(&store), Some(1), "开关关时顶屏不推 6b");
    let _ = std::fs::remove_file(&db);
}

/// 智能符号 hold 分支（`CommitAndHoldComposition`）是标点顶屏的另一条独立出口：同样造词。
#[test]
fn smart_symbol_hold_top_commit_learns() {
    skip_without_data!();
    let (coord, store, db) = open("hold_learn", "pinyin", |c| {
        c.input.symbol.smart_mode = true;
        c.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
    });
    step_ni(&coord);
    top_commit(&coord, VK_PERIOD);
    assert_eq!(nihao(&store), Some(1), "拼音桶: {:?}", temp_rows(&store));
    let _ = std::fs::remove_file(&db);
}

/// 智能符号 hold 分支同样推 6b。
#[test]
fn smart_symbol_hold_top_commit_bumps_temp_word() {
    skip_without_data!();
    let (coord, store, db) = open("hold_6b", "pinyin", |c| {
        c.input.symbol.smart_mode = true;
        c.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
    });
    store
        .learn_temp_word("pinyin", "nihao", "你好", 800, 0)
        .unwrap();
    type_str(&coord, "nihao");
    top_commit(&coord, VK_PERIOD);
    assert_eq!(nihao(&store), Some(2), "hold 顶屏应推进同一条临时词");
    let _ = std::fs::remove_file(&db);
}

/// 高亮是分步候选（`好` 只消费 `haoma` 的前缀）时顶屏不造词、只推 6b。
#[test]
fn partial_highlight_top_commit_bumps_without_learning() {
    skip_without_data!();
    let (coord, store, db) = open("partial", "pinyin", |_| {});
    store
        .learn_temp_word("pinyin", "hao", "好", 800, 0)
        .unwrap();
    type_str(&coord, "nihaoma");
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if let Some(p) = t.iter().position(|x| x == "你") {
            coord.handle_key_event(&key(0x31 + p as u32));
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
    match coord.handle_key_event(&key(VK_COMMA)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "你好，", "前提：顶屏「你好，」"),
        other => panic!("前提：顶屏应上屏，实际: {other:?}"),
    }
    assert_eq!(
        nihao(&store),
        None,
        "分步候选顶屏不应造词；拼音桶: {:?}",
        temp_rows(&store)
    );
    assert_eq!(
        store.get_temp_word("pinyin", "hao", "好").unwrap(),
        Some(2),
        "分步候选顶屏仍推 6b"
    );
    let _ = std::fs::remove_file(&db);
}

/// `learned_code` 防 +2：整句候选（`is_synthesized`）顶屏时造词会命中已有的同码同文临时词
/// 并计数 +1，6b 不得再推一次。
#[test]
fn synthesized_top_commit_counts_once() {
    skip_without_data!();
    const CODE: &str = "mingtianqubeijing";
    const TEXT: &str = "明天去北京";
    // 前提：该整句顶屏会被造词（走的是整句单段放行，不是 6b）。
    let (coord, store, db) = open("syn_ctrl", "pinyin", |_| {});
    type_str(&coord, CODE);
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some(TEXT),
        "前提：首选是整句「{TEXT}」"
    );
    top_commit_text(&coord, VK_COMMA);
    assert_eq!(
        store.get_temp_word("pinyin", CODE, TEXT).unwrap(),
        Some(1),
        "前提：整句顶屏造词"
    );
    let _ = std::fs::remove_file(&db);

    let (coord, store, db) = open("syn_once", "pinyin", |_| {});
    store.learn_temp_word("pinyin", CODE, TEXT, 800, 0).unwrap();
    type_str(&coord, CODE);
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some(TEXT),
        "前提：首选是「{TEXT}」"
    );
    top_commit_text(&coord, VK_COMMA);
    assert_eq!(
        store.get_temp_word("pinyin", CODE, TEXT).unwrap(),
        Some(2),
        "同一次顶屏只能 +1（造词已命中，6b 不再推）"
    );
    let _ = std::fs::remove_file(&db);
}

/// 按 `vk` 顶屏，只断言上屏了（文本不限）。
fn top_commit_text(coord: &Coordinator, vk: u32) {
    match coord.handle_key_event(&key(vk)) {
        KeyAction::InsertText { .. } => {}
        other => panic!("前提：顶屏应上屏，实际: {other:?}"),
    }
}

/// 码表主方案（五笔）打 `wqvb`：高亮候选文本（首选）。
fn wubi_top(coord: &Coordinator) -> String {
    type_str(coord, "wqvb");
    coord
        .debug_page_texts()
        .first()
        .cloned()
        .expect("前提：`wqvb` 有候选")
}

/// 反向：码表主方案的标点顶屏不造词（造词是拼音分步的事），只推 6b。
#[test]
fn codetable_punct_top_commit_bumps_without_learning() {
    skip_without_data!();
    let (coord, store, db) = open("wubi_punct", "wubi86", |c| {
        c.schema.codetable.punct_commit = true;
    });
    store
        .learn_temp_word("wubi86", "wqvb", "你好", 800, 0)
        .unwrap();
    assert_eq!(wubi_top(&coord), "你好", "前提：`wqvb` 首选「你好」");
    top_commit_text(&coord, VK_COMMA);
    assert_eq!(
        store.get_temp_word("wubi86", "wqvb", "你好").unwrap(),
        Some(2),
        "码表标点顶屏推 6b"
    );
    let wubi_rows = store
        .search_temp_words_prefix("wubi86", "", 50)
        .unwrap_or_default()
        .len();
    assert_eq!(wubi_rows, 1, "码表标点顶屏不造新词");
    assert!(temp_rows(&store).is_empty(), "拼音桶不应有新词");
    let _ = std::fs::remove_file(&db);
}

/// 次三选键越界 `commit_and_input`（每页 1 条，`'` 要第三条）是顶屏：推 6b 跟随开关。
#[test]
fn overflow_select_key_commit_and_input_follows_switch() {
    skip_without_data!();
    for (on, want) in [(true, 2), (false, 1)] {
        let (coord, store, db) = open(&format!("overflow_{on}"), "wubi86", |c| {
            c.ui.candidate.per_page = 1;
            c.keys.overflow.select_key = "commit_and_input".into();
            c.input.top_commit_learn = on;
        });
        store
            .learn_temp_word("wubi86", "wqvb", "你好", 800, 0)
            .unwrap();
        assert_eq!(wubi_top(&coord), "你好", "前提：`wqvb` 首选「你好」");
        match coord.handle_key_event(&key(VK_QUOTE)) {
            KeyAction::InsertText { text, .. } => assert!(
                text.starts_with("你好") && text.chars().count() == 3,
                "前提：越界顶屏「你好」+ 引号，实际: {text}"
            ),
            other => panic!("前提：越界应顶屏，实际: {other:?}"),
        }
        assert_eq!(
            store.get_temp_word("wubi86", "wqvb", "你好").unwrap(),
            Some(want),
            "top_commit_learn = {on}"
        );
        let _ = std::fs::remove_file(&db);
    }
}
