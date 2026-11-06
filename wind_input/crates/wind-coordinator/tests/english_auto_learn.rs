//! 英文自动造词（`[schema.english.auto_learn]`，GH#136 / C1-11 子项 1）。设计见
//! `src/english_learn.rs` 模块文档。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库（英文方案要能加载，「是不是词库词」要问真词库）；
//! 缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};
use wind_store::Store;

const VK_SPACE: u32 = 0x20;
const VK_RETURN: u32 = 0x0D;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "english"]
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

fn key(k: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// `active`：`"english"` = 英文方案主路；`"wubi86"` = 五笔下用临时英文。
fn open(tag: &str, active: &str, edit: impl FnOnce(&mut Config)) -> (Arc<Coordinator>, Arc<Store>) {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "english".into()];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    c.schema.english.auto_learn.enabled = true;
    edit(&mut c);
    let path = std::env::temp_dir().join(format!(
        "wind_english_learn_{}_{tag}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), store.clone());
    (coord, store)
}

/// 逐键敲入；大写字母带 Shift。
fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        let m = if ch.is_ascii_uppercase() {
            MOD_SHIFT
        } else {
            0
        };
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, m));
    }
}

fn committed(act: &KeyAction) -> String {
    match act {
        KeyAction::InsertText { text, .. } => text.trim_end().to_string(),
        other => panic!("应上屏，实际: {other:?}"),
    }
}

fn temp_count(store: &Store, text: &str) -> u32 {
    store
        .get_temp_word("english", &text.to_ascii_lowercase(), text)
        .unwrap()
        .unwrap_or(0)
}

fn in_user(store: &Store, text: &str) -> bool {
    store
        .get_user_words("english", &text.to_ascii_lowercase())
        .unwrap()
        .iter()
        .any(|r| r.text == text)
}

/// 英文方案：回车上屏词库里没有的原文 → 进临时词库，下次打前缀就能补出来。
#[test]
fn english_schema_enter_learns_and_recalls() {
    skip_without_data!();
    let (coord, store) = open("enter", "english", |_| {});
    type_str(&coord, "wxyzqk");
    assert_eq!(
        committed(&coord.handle_key_event(&key(VK_RETURN, 0))),
        "wxyzqk"
    );
    assert_eq!(temp_count(&store, "wxyzqk"), 1, "应进英文临时词库");
    type_str(&coord, "wxyz");
    let texts = coord.debug_all_candidate_texts();
    assert!(
        texts.iter().any(|t| t == "wxyzqk"),
        "打前缀应补出造的词: {texts:?}"
    );
}

/// 选中原文候选（空格，出厂首候选即原文）同样是造词信号。
#[test]
fn english_schema_selecting_raw_learns() {
    skip_without_data!();
    let (coord, store) = open("space", "english", |_| {});
    type_str(&coord, "wxyzqk");
    coord.handle_key_event(&key(VK_SPACE, 0));
    assert_eq!(temp_count(&store, "wxyzqk"), 1, "同一次上屏只记一次");
}

/// 开关关着（出厂）什么都不造。
#[test]
fn nothing_learned_when_disabled() {
    skip_without_data!();
    let (coord, store) = open("off", "english", |c| {
        c.schema.english.auto_learn.enabled = false;
    });
    type_str(&coord, "wxyzqk");
    coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(temp_count(&store, "wxyzqk"), 0);
}

/// 词库里已有的词不造（大小写不论），英文调频关着（出厂）时也一样判得出。
#[test]
fn dictionary_word_is_not_learned() {
    skip_without_data!();
    let (coord, store) = open("dict", "english", |c| {
        c.schema.english.frequency.enabled = false;
    });
    for w in ["hello", "Hello"] {
        type_str(&coord, w);
        coord.handle_key_event(&key(VK_RETURN, 0));
        assert_eq!(temp_count(&store, w), 0, "{w} 是词库词，不该造");
    }
}

/// 临时英文（五笔下 Shift 起手）：空格选中原文 → 造词，存原形、码小写。
#[test]
fn temp_english_learns_with_original_case() {
    skip_without_data!();
    let (coord, store) = open("temp", "wubi86", |_| {});
    type_str(&coord, "Immortalwrt");
    assert_eq!(coord.debug_active_mode(), Some("temp_english"), "前提");
    assert_eq!(
        committed(&coord.handle_key_event(&key(VK_SPACE, 0))),
        "Immortalwrt"
    );
    assert_eq!(temp_count(&store, "Immortalwrt"), 1, "存原形 Immortalwrt");
    // 两边共用一套词库：再进临英打前缀就补得出来。
    type_str(&coord, "Immo");
    let texts = coord.debug_all_candidate_texts();
    assert!(
        texts.iter().any(|t| t.eq_ignore_ascii_case("Immortalwrt")),
        "临英里应补出造的词: {texts:?}"
    );
}

/// 用够 `promote_count` 次（含造词那一次）晋升用户词库；选中临时词也算一次。
#[test]
fn promotes_after_enough_uses() {
    skip_without_data!();
    let (coord, store) = open("promote", "english", |c| {
        c.schema.english.auto_learn.promote_count = 3;
    });
    for _ in 0..2 {
        type_str(&coord, "wxyzqk");
        coord.handle_key_event(&key(VK_RETURN, 0));
    }
    assert_eq!(temp_count(&store, "wxyzqk"), 2, "前提：两次回车");
    assert!(!in_user(&store, "wxyzqk"), "两次还不该晋升");
    // 第三次：选中候选里的那条临时词（首格即它——原文格被同名临时词占据）。
    type_str(&coord, "wxyzqk");
    coord.handle_key_event(&key(VK_SPACE, 0));
    assert!(in_user(&store, "wxyzqk"), "第三次应晋升用户词库");
}

/// 快捷输入里的英文不走英文造词（那里的原文进快捷输入历史）。
#[test]
fn quick_input_is_out_of_scope() {
    skip_without_data!();
    let (coord, store) = open("quick", "wubi86", |_| {});
    coord.handle_key_event(&key(0xBA, 0)); // `;`
    type_str(&coord, "Wxyzqk");
    coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(temp_count(&store, "Wxyzqk"), 0);
}

/// 临时英文里选中临时词同样推进晋升计数（与英文方案主路同一函数）。
#[test]
fn temp_english_selection_promotes() {
    skip_without_data!();
    let (coord, store) = open("temp_promote", "wubi86", |c| {
        c.schema.english.auto_learn.promote_count = 2;
    });
    type_str(&coord, "Wxyzqk");
    coord.handle_key_event(&key(VK_SPACE, 0));
    assert_eq!(temp_count(&store, "Wxyzqk"), 1, "前提：造词");
    type_str(&coord, "Wxyzqk");
    coord.handle_key_event(&key(VK_SPACE, 0));
    assert!(
        in_user(&store, "Wxyzqk"),
        "第二次选中应晋升（promote_count = 2）"
    );
}

/// 英文方案标点顶屏原文同样是造词信号。
#[test]
fn english_schema_punct_top_commit_learns() {
    skip_without_data!();
    let (coord, store) = open("punct", "english", |_| {});
    type_str(&coord, "wxyzqk");
    coord.handle_key_event(&key(0xBC, 0)); // `,`
    assert_eq!(temp_count(&store, "wxyzqk"), 1);
}

/// 临时英文里**打前缀补出**自造词再选中，同样推进晋升（临时词记在全码下，不能拿缓冲去查）。
#[test]
fn temp_english_prefix_selection_promotes() {
    skip_without_data!();
    let (coord, store) = open("temp_prefix", "wubi86", |c| {
        c.schema.english.auto_learn.promote_count = 2;
    });
    type_str(&coord, "Wxyzqk");
    coord.handle_key_event(&key(VK_SPACE, 0));
    assert_eq!(temp_count(&store, "Wxyzqk"), 1, "前提：造词");
    type_str(&coord, "Wxy");
    let texts = coord.debug_all_candidate_texts();
    let pos = texts
        .iter()
        .position(|t| t == "Wxyzqk")
        .unwrap_or_else(|| panic!("打前缀应补出: {texts:?}"));
    coord.handle_key_event(&key(0x31 + pos as u32, 0));
    assert!(
        in_user(&store, "Wxyzqk"),
        "前缀选中也应晋升（promote_count = 2）"
    );
}

/// 已有临时词 `Wxyzqk`，在英文方案里回车小写 `wxyzqk`：不另造，但给那一条计数。
#[test]
fn other_case_commit_counts_toward_existing_temp_word() {
    skip_without_data!();
    let (coord, store) = open("case_count", "english", |_| {});
    type_str(&coord, "Wxyzqk");
    coord.handle_key_event(&key(VK_RETURN, 0));
    type_str(&coord, "wxyzqk");
    coord.handle_key_event(&key(VK_RETURN, 0));
    assert_eq!(temp_count(&store, "wxyzqk"), 0, "不另造小写那条");
    assert_eq!(temp_count(&store, "Wxyzqk"), 2, "计到已有那条上");
}
