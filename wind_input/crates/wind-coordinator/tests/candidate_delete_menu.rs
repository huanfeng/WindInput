//! 候选右键菜单的删除类菜单项：拼音 / 英文数据方案下 ≥2 字的词给「屏蔽该词」（按文字屏蔽）与
//! 「在此编码下隐藏」（只写当前输入码那一行）两项；单字、码表、混输码表段只有「在此编码下隐藏」。
//! 设计见 docs/design/system-dict-patch-and-candidate-adjust.md §4a、§9。
//!
//! 菜单一律按全量下标取（`candidate_menu_items`），候选经 Coordinator 取（引擎位次 ≠ 界面位次）。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时整族静默跳过（判据是耗时 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL, MOD_SHIFT};
use wind_store::Store;
use wind_ui_types::{CandidateOp, MenuKind};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english", "wubi86_pinyin", "shuangpin"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
        && d.join("schemas/pinyin/rime_frost.dict.yaml").exists()
        && d.join("schemas/english/en.dict.yaml").exists()
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

/// 逐键敲入；大写字母带 Shift（五笔下首字母大写即进临时英文）。
fn type_keys(coord: &Coordinator, input: &str) {
    for ch in input.chars() {
        let m = if ch.is_ascii_uppercase() {
            MOD_SHIFT
        } else {
            0
        };
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF, m));
    }
}

fn config(active: &str) -> Config {
    let mut c = Config::default();
    c.schema.available = vec![
        "wubi86".into(),
        "pinyin".into(),
        "english".into(),
        "wubi86_pinyin".into(),
        "shuangpin".into(),
    ];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    c.keys.select_key_groups.clear();
    c
}

fn fresh_store(name: &str) -> Arc<Store> {
    // 带 pid：并发跑两份测试（两个 worktree 共用 TMPDIR）时不互删库文件。
    let path =
        std::env::temp_dir().join(format!("wind_delmenu_{name}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

fn open(cfg: &Config, store: &Arc<Store>) -> Arc<Coordinator> {
    Coordinator::new_headless_with_store(cfg.clone(), Some(&data_dir()), Arc::clone(store))
}

fn has(cfg: &Config, store: &Arc<Store>, input: &str, word: &str) -> bool {
    let coord = open(cfg, store);
    type_keys(&coord, input);
    coord.debug_all_candidate_texts().iter().any(|t| t == word)
}

/// 打 `input`，返回协调器与 `want` 在全量候选里的下标。
fn locate(cfg: &Config, store: &Arc<Store>, input: &str, want: &str) -> (Arc<Coordinator>, usize) {
    let coord = open(cfg, store);
    type_keys(&coord, input);
    let all = coord.debug_all_candidate_texts();
    let idx = all
        .iter()
        .position(|t| t == want)
        .unwrap_or_else(|| panic!("前提：`{input}` 应有「{want}」，实际 {all:?}"));
    (coord, idx)
}

/// 删除 / 隐藏 / 恢复三类菜单项（按出现顺序）：`(文案, 动作, 可用)`。
fn delete_items(coord: &Coordinator, idx: usize) -> Vec<(String, CandidateOp, bool)> {
    coord
        .candidate_menu_items(idx)
        .expect("应有菜单")
        .into_iter()
        .filter_map(|m| match m.kind {
            MenuKind::Op(
                op @ (CandidateOp::Delete | CandidateOp::HideHere | CandidateOp::Reset),
            ) => Some((m.label, op, m.enabled)),
            _ => None,
        })
        .collect()
}

/// 菜单里词条操作的顺序（不含常用字标记）。
fn op_order(coord: &Coordinator, idx: usize) -> Vec<CandidateOp> {
    coord
        .candidate_menu_items(idx)
        .expect("应有菜单")
        .into_iter()
        .filter_map(|m| match m.kind {
            MenuKind::Op(CandidateOp::ToggleCommon) => None,
            MenuKind::Op(op) => Some(op),
            _ => None,
        })
        .collect()
}

fn two_items(enabled: bool, reset: bool) -> Vec<(String, CandidateOp, bool)> {
    vec![
        ("屏蔽该词".into(), CandidateOp::Delete, enabled),
        ("在此编码下隐藏".into(), CandidateOp::HideHere, enabled),
        ("恢复默认".into(), CandidateOp::Reset, reset),
    ]
}

fn one_item(label: &str, enabled: bool) -> Vec<(String, CandidateOp, bool)> {
    vec![
        (label.into(), CandidateOp::Delete, enabled),
        ("恢复默认".into(), CandidateOp::Reset, false),
    ]
}

fn blocked(store: &Store, schema: &str) -> Vec<String> {
    let mut v: Vec<String> = store
        .blocked_words(schema)
        .unwrap()
        .iter()
        .cloned()
        .collect();
    v.sort();
    v
}

fn deleted_at(store: &Store, schema: &str, code: &str) -> Option<Vec<String>> {
    store
        .get_shadow_rules(schema, code)
        .unwrap()
        .map(|r| r.deleted)
}

/// 拼音多字的系统词 / 用户词 / 临时词 / 整句：「屏蔽该词」「在此编码下隐藏」两项，顺序在调位之后、
/// 「恢复默认」之前。
#[test]
fn pinyin_multi_char_words_offer_block_and_hide_here() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_menu");
    store
        .add_user_word("pinyin", "nihao", "倪豪", 50_000_000, 0b101)
        .unwrap();
    store
        .learn_temp_word("pinyin", "nihao", "尼浩", 10, 0)
        .unwrap();
    for (input, word) in [
        ("nihao", "你好"),
        ("nihao", "倪豪"),
        ("nihao", "尼浩"),
        ("tazaijialikanshu", "他在家里看书"),
    ] {
        let (coord, idx) = locate(&cfg, &store, input, word);
        assert_eq!(
            delete_items(&coord, idx),
            two_items(true, false),
            "`{input}` 的「{word}」"
        );
        assert_eq!(
            op_order(&coord, idx),
            [
                CandidateOp::MoveTop,
                CandidateOp::MoveUp,
                CandidateOp::MoveDown,
                CandidateOp::Delete,
                CandidateOp::HideHere,
                CandidateOp::Reset,
            ],
            "`{input}` 的「{word}」菜单顺序"
        );
    }
}

/// 拼音单字只有「在此编码下隐藏」（`Delete` 本就按编码隐藏）。
#[test]
fn pinyin_single_char_only_hides_here() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_single");
    let (coord, idx) = locate(&cfg, &store, "ni", "你");
    assert_eq!(delete_items(&coord, idx), one_item("在此编码下隐藏", true));
}

/// 点「在此编码下隐藏」：只写 `(pinyin, nihao, 你好)`，不写 `*`；`nihao` 下不出，`nh` 下照出，
/// 那里「恢复默认」是灰的（那个码下没有规则）。
#[test]
fn pinyin_hide_here_writes_only_current_code() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_hide");
    assert!(has(&cfg, &store, "nh", "你好"), "前提：`nh` 有「你好」");
    let (coord, idx) = locate(&cfg, &store, "nihao", "你好");
    coord.candidate_or_quick_format_op_at(CandidateOp::HideHere, idx);
    assert!(blocked(&store, "pinyin").is_empty(), "不写屏蔽行");
    assert_eq!(
        deleted_at(&store, "pinyin", "nihao"),
        Some(vec!["你好".to_string()])
    );
    assert!(!has(&cfg, &store, "nihao", "你好"));
    let (coord, idx) = locate(&cfg, &store, "nh", "你好");
    assert_eq!(delete_items(&coord, idx), two_items(true, false));
}

/// 用户词「在此编码下隐藏」：不删用户词，只在这个码下不出，别的码下照出。
#[test]
fn pinyin_hide_here_keeps_user_word() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_hide_user");
    for code in ["nihao", "lihao"] {
        store
            .add_user_word("pinyin", code, "倪豪", 50_000_000, 0b101)
            .unwrap();
    }
    let (coord, idx) = locate(&cfg, &store, "nihao", "倪豪");
    coord.candidate_or_quick_format_op_at(CandidateOp::HideHere, idx);
    assert!(blocked(&store, "pinyin").is_empty(), "不写屏蔽行");
    for code in ["nihao", "lihao"] {
        assert!(
            store
                .get_user_words("pinyin", code)
                .unwrap()
                .iter()
                .any(|r| r.text == "倪豪"),
            "`{code}` 下的用户词仍在"
        );
    }
    assert!(!has(&cfg, &store, "nihao", "倪豪"));
    assert!(has(&cfg, &store, "lihao", "倪豪"), "别的编码照出");
}

/// 「屏蔽该词」仍是原来的按文字屏蔽。
#[test]
fn pinyin_block_item_still_blocks_by_text() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_block");
    let (coord, idx) = locate(&cfg, &store, "nihao", "你好");
    coord.candidate_or_quick_format_op_at(CandidateOp::Delete, idx);
    assert_eq!(blocked(&store, "pinyin"), vec!["你好".to_string()]);
    assert!(deleted_at(&store, "pinyin", "nihao").is_none());
    assert!(!has(&cfg, &store, "nh", "你好"));
}

/// 置顶过的词「恢复默认」可用，撤掉后规则清空。
#[test]
fn pinyin_reset_after_pin() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_reset");
    let coord = open(&cfg, &store);
    type_keys(&coord, "nihao");
    let all = coord.debug_all_candidate_texts();
    let (idx, word) = all
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, t)| t.chars().count() >= 2)
        .map(|(i, t)| (i, t.clone()))
        .unwrap_or_else(|| panic!("前提：`nihao` 第二位之后有多字词，实际 {all:?}"));
    coord.candidate_or_quick_format_op_at(CandidateOp::MoveTop, idx);
    let (coord, idx) = locate(&cfg, &store, "nihao", &word);
    assert_eq!(idx, 0, "置顶生效");
    assert_eq!(delete_items(&coord, idx), two_items(true, true));
    coord.candidate_or_quick_format_op_at(CandidateOp::Reset, idx);
    assert!(store.get_shadow_rules("pinyin", "nihao").unwrap().is_none());
}

/// 英文多字母词：两项；点「在此编码下隐藏」只写 `(english, hel, help)`，别的前缀照出。
#[test]
fn english_word_offers_both_and_hide_here_is_per_code() {
    skip_without_data!();
    let cfg = config("english");
    let store = fresh_store("en");
    let (coord, idx) = locate(&cfg, &store, "hel", "help");
    assert_eq!(delete_items(&coord, idx), two_items(true, false));
    coord.candidate_or_quick_format_op_at(CandidateOp::HideHere, idx);
    assert!(blocked(&store, "english").is_empty(), "不写屏蔽行");
    assert_eq!(
        deleted_at(&store, "english", "hel"),
        Some(vec!["help".to_string()])
    );
    assert!(!has(&cfg, &store, "hel", "help"));
    assert!(has(&cfg, &store, "he", "help"), "别的编码照出");
}

/// 英文头部原文候选（不可删）：两项都灰，「在此编码下隐藏」也不写规则。
#[test]
fn english_head_raw_candidate_both_disabled() {
    skip_without_data!();
    let cfg = config("english");
    let store = fresh_store("en_head");
    let (coord, idx) = locate(&cfg, &store, "wxyzqk", "wxyzqk");
    assert_eq!(idx, 0, "前提：首位是原文条");
    assert_eq!(delete_items(&coord, idx), two_items(false, false));
    coord.candidate_or_quick_format_op_at(CandidateOp::HideHere, idx);
    assert!(store.list_shadow_rules("english").unwrap().is_empty());
}

/// 码表系统词只有「在此编码下隐藏」；码表用户词仍是「删除用户词」。
#[test]
fn codetable_items() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("ct");
    store
        .add_user_word("wubi86", "aaaa", "工工工工", 0, 0)
        .unwrap();
    let coord = open(&cfg, &store);
    type_keys(&coord, "aaaa");
    let all = coord.debug_all_candidate_texts();
    let sys = all
        .iter()
        .position(|t| t.chars().count() >= 2 && t != "工工工工")
        .unwrap_or_else(|| panic!("前提：`aaaa` 有多字系统词，实际 {all:?}"));
    assert_eq!(delete_items(&coord, sys), one_item("在此编码下隐藏", true));
    let user = all
        .iter()
        .position(|t| t == "工工工工")
        .unwrap_or_else(|| panic!("前提：`aaaa` 有用户词，实际 {all:?}"));
    assert_eq!(delete_items(&coord, user), one_item("删除用户词", true));
    // 写端与菜单同判据：码表词不接「在此编码下隐藏」
    coord.candidate_or_quick_format_op_at(CandidateOp::HideHere, sys);
    assert!(store.list_shadow_rules("wubi86").unwrap().is_empty());
}

/// 混输：拼音段多字词两项，码表段只有「在此编码下隐藏」。
#[test]
fn mixed_segments() {
    skip_without_data!();
    let cfg = config("wubi86_pinyin");
    let store = fresh_store("mix");
    let (coord, idx) = locate(&cfg, &store, "nihao", "你好");
    assert_eq!(delete_items(&coord, idx), two_items(true, false));
    let (coord, idx) = locate(&cfg, &store, "wqvb", "你好");
    assert_eq!(delete_items(&coord, idx), one_item("在此编码下隐藏", true));
}

/// 双拼（小鹤 `nihc`）点「在此编码下隐藏」：规则落在归一后的全拼码 `nihao` 下（与读端同形），
/// 不落击键码；双拼与全拼的 `nihao` 都不出，`nh` 照出。
#[test]
fn shuangpin_hide_here_keys_on_normalized_code() {
    skip_without_data!();
    let sp = config("shuangpin");
    let full = config("pinyin");
    let store = fresh_store("sp_hide");
    let (coord, idx) = locate(&sp, &store, "nihc", "你好");
    assert_eq!(delete_items(&coord, idx), two_items(true, false));
    coord.candidate_or_quick_format_op_at(CandidateOp::HideHere, idx);
    assert!(blocked(&store, "pinyin").is_empty(), "不写屏蔽行");
    assert_eq!(
        deleted_at(&store, "pinyin", "nihao"),
        Some(vec!["你好".to_string()])
    );
    assert!(
        deleted_at(&store, "pinyin", "nihc").is_none(),
        "不按击键码落键"
    );
    assert!(!has(&sp, &store, "nihc", "你好"), "双拼当前码下不出");
    assert!(!has(&full, &store, "nihao", "你好"), "全拼同码共用");
    assert!(has(&full, &store, "nh", "你好"), "别的编码照出");
}

/// 混输拼音段点「在此编码下隐藏」：`nihao` 下不出，五笔码 `wqvb` 下照出，不写屏蔽行。
#[test]
fn mixed_pinyin_hide_here_is_per_code() {
    skip_without_data!();
    let cfg = config("wubi86_pinyin");
    let store = fresh_store("mix_hide");
    let (coord, idx) = locate(&cfg, &store, "nihao", "你好");
    coord.candidate_or_quick_format_op_at(CandidateOp::HideHere, idx);
    assert!(blocked(&store, "pinyin").is_empty(), "不写屏蔽行");
    assert!(!has(&cfg, &store, "nihao", "你好"), "当前码下不出");
    assert!(has(&cfg, &store, "wqvb", "你好"), "别的编码照出");
}

/// 临英 `Hel`（大小写跟随，`help` 显示成 `Help`）：置顶与「在此编码下隐藏」都按词库原文 `help`
/// 落在 `hel` 下；置顶后菜单「恢复默认」可用（读写同一把键）；隐藏后当场重建不再出，`He` 下照出。
#[test]
fn temp_english_hide_here_keys_on_dict_text() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("te_hide");
    let (coord, idx) = locate(&cfg, &store, "Hel", "Help");
    assert!(idx > 1, "前提：Help 不在原文之后的第一格");
    assert_eq!(delete_items(&coord, idx), two_items(true, false));
    coord.candidate_or_quick_format_op_at(CandidateOp::MoveTop, idx);
    let (coord, idx) = locate(&cfg, &store, "Hel", "Help");
    let rec = store.get_shadow_rules("english", "hel").unwrap().unwrap();
    assert_eq!(rec.pinned[0].word, "help", "置顶规则键是词库原文");
    assert_eq!(
        delete_items(&coord, idx),
        two_items(true, true),
        "恢复默认可用"
    );
    coord.candidate_or_quick_format_op_at(CandidateOp::HideHere, idx);
    assert_eq!(
        deleted_at(&store, "english", "hel"),
        Some(vec!["help".to_string()]),
        "隐藏规则键是词库原文"
    );
    assert!(blocked(&store, "english").is_empty(), "不写屏蔽行");
    let now = coord.debug_all_candidate_texts();
    assert_eq!(
        now.first().map(String::as_str),
        Some("Hel"),
        "原文条仍在：{now:?}"
    );
    assert!(!now.iter().any(|t| t == "Help"), "当场不再出：{now:?}");
    assert!(!has(&cfg, &store, "Hel", "Help"));
    assert!(has(&cfg, &store, "He", "Help"), "别的编码照出");
}

/// 删除热键（Ctrl+Shift+数字）触发的是「屏蔽该词」：写 `*` 行，不写按编码的行。
#[test]
fn delete_hotkey_still_blocks_word() {
    skip_without_data!();
    let mut cfg = config("pinyin");
    cfg.keys.delete_candidate = "ctrl+shift+number".into();
    let store = fresh_store("hotkey");
    let coord = open(&cfg, &store);
    type_keys(&coord, "nihao");
    let page = coord.debug_page_texts();
    let p = page
        .iter()
        .position(|t| t == "你好")
        .unwrap_or_else(|| panic!("前提：首页有「你好」，实际 {page:?}"));
    coord.handle_key_event_policed(&key(0x31 + p as u32, MOD_CTRL | MOD_SHIFT));
    assert_eq!(blocked(&store, "pinyin"), vec!["你好".to_string()]);
    assert!(
        deleted_at(&store, "pinyin", "nihao").is_none(),
        "不写按编码的行"
    );
}
