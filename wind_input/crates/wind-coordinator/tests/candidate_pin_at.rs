//! 候选调位的位置口径：写端下标与读端 `apply_shadow` 解释位置用的必须是同一个列表。
//!
//! 读端在 shadow 之后才插入简繁变体（`expand_s2t_variants`）、单字母重复上屏项与 emoji；
//! 写端读的 `state.candidates` 已含这些项，算下标时要跳过。
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

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "wubi86_pinyin", "shuangpin"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
        && d.join("schemas/pinyin/rime_frost.dict.yaml").exists()
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
}

fn type_keys(coord: &Coordinator, input: &str) {
    for ch in input.chars() {
        coord.handle_key_event_policed(&KeyEventData {
            key_code: (ch.to_ascii_uppercase() as u32) & 0xFF,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    }
}

fn config(active: &str) -> Config {
    let mut c = Config::default();
    c.schema.available = vec![
        "wubi86".into(),
        "pinyin".into(),
        "wubi86_pinyin".into(),
        "shuangpin".into(),
    ];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    c.schema.codetable.auto_commit_at_full = false;
    c.keys.select_key_groups.clear();
    c
}

fn fresh_store(name: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_pinat_{name}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

fn open(cfg: &Config, store: &Arc<Store>, s2t: bool) -> Option<Arc<Coordinator>> {
    let coord =
        Coordinator::new_headless_with_store(cfg.clone(), Some(&data_dir()), Arc::clone(store));
    if s2t && !coord.debug_set_s2t(true) {
        eprintln!("跳过：缺 opencc 数据");
        return None;
    }
    Some(coord)
}

/// 规则里 `word` 的位置（无规则 → None）。
fn pinned_pos(store: &Store, schema: &str, code: &str, word: &str) -> Option<usize> {
    store
        .get_shadow_rules(schema, code)
        .unwrap()?
        .pinned
        .iter()
        .find(|p| p.word == word)
        .map(|p| p.position)
}

/// 码表 `bm`（出）开简繁：「出」后紧跟变体「齣」（内部文本仍是「出」）。对「出」之后第二个
/// 不同的候选后移一位：规则位置按读端列表（无变体）计；关掉简繁重开，它就在那个位置上。
#[test]
fn codetable_move_down_with_s2t_variants() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("ct_s2t");
    let Some(coord) = open(&cfg, &store, true) else {
        return;
    };
    type_keys(&coord, "bm");
    let all = coord.debug_all_candidate_texts();
    let chu = all
        .iter()
        .position(|t| t == "出")
        .unwrap_or_else(|| panic!("前提：`bm` 有「出」，实际 {all:?}"));
    assert_eq!(
        all.get(chu + 1).map(String::as_str),
        Some("出"),
        "前提：「出」后紧跟变体：{all:?}"
    );
    // 读端列表 = 去掉紧随其后的那条变体。
    let mut plain = all.clone();
    plain.remove(chu + 1);
    let target_plain = chu + 1;
    let word = plain
        .get(target_plain)
        .cloned()
        .unwrap_or_else(|| panic!("前提：「出」之后还有候选：{all:?}"));
    assert!(
        plain.len() > target_plain + 1,
        "前提：后面还有可后移的位置：{all:?}"
    );
    coord.candidate_or_quick_format_op_at(CandidateOp::MoveDown, target_plain + 1);
    assert_eq!(
        pinned_pos(&store, "wubi86", "bm", &word),
        Some(target_plain + 1),
        "规则位置按无变体的列表计"
    );
    let plain_coord = open(&cfg, &store, false).unwrap();
    type_keys(&plain_coord, "bm");
    let now = plain_coord.debug_all_candidate_texts();
    assert_eq!(
        now.iter().position(|t| *t == word),
        Some(target_plain + 1),
        "重开后读端位置一致：{now:?}"
    );
}

// ── 固定到第 N 位（CandidateOp::PinAt）──────────────────────────────────────

fn coord_at(cfg: &Config, store: &Arc<Store>, input: &str) -> Arc<Coordinator> {
    let coord = open(cfg, store, false).unwrap();
    type_keys(&coord, input);
    coord
}

fn texts(cfg: &Config, store: &Arc<Store>, input: &str) -> Vec<String> {
    coord_at(cfg, store, input).debug_all_candidate_texts()
}

/// 列表里第 `from` 位起第一个与前面都不同文的候选（避开同文多条时的歧义）。
fn distinct_at(all: &[String], from: usize) -> (usize, String) {
    (from..all.len())
        .find(|&i| !all[..i].contains(&all[i]))
        .map(|i| (i, all[i].clone()))
        .unwrap_or_else(|| panic!("前提：第 {from} 位之后有候选：{all:?}"))
}

/// 打 `input`，把第 `from` 位起的某个词固定到第 `n` 位；返回那个词。
fn pin_at(cfg: &Config, store: &Arc<Store>, input: &str, from: usize, n: u8) -> String {
    let coord = coord_at(cfg, store, input);
    let (idx, word) = distinct_at(&coord.debug_all_candidate_texts(), from);
    coord.candidate_or_quick_format_op_at(CandidateOp::PinAt(n), idx);
    word
}

fn pos_of(all: &[String], word: &str) -> Option<usize> {
    all.iter().position(|t| t == word)
}

/// 子菜单项 `(文案, N, 可用)`。
type PinItem = (String, u8, bool);

/// 「固定到第 N 位」子菜单：`(父项可用, 子项)`。
fn pin_menu(coord: &Coordinator, idx: usize) -> Option<(bool, Vec<PinItem>)> {
    coord
        .candidate_menu_items(idx)?
        .into_iter()
        .find(|m| m.label == "固定到第 N 位")
        .map(|m| {
            let kids = m
                .children
                .iter()
                .map(|c| match c.kind {
                    wind_ui_types::MenuKind::Op(CandidateOp::PinAt(n)) => {
                        (c.label.clone(), n, c.enabled)
                    }
                    other => panic!("子项应是 PinAt：{other:?}"),
                })
                .collect();
            (m.enabled, kids)
        })
}

/// 全拼：固定到第 3 位写 `(pinyin, nihao, 词, 2)`，当场与重开都在第 3 位。
#[test]
fn pinyin_pin_at_3() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py3");
    let coord = coord_at(&cfg, &store, "nihao");
    let (idx, word) = distinct_at(&coord.debug_all_candidate_texts(), 5);
    coord.candidate_or_quick_format_op_at(CandidateOp::PinAt(3), idx);
    assert_eq!(pinned_pos(&store, "pinyin", "nihao", &word), Some(2));
    assert_eq!(
        pos_of(&coord.debug_all_candidate_texts(), &word),
        Some(2),
        "当场生效"
    );
    assert_eq!(
        pos_of(&texts(&cfg, &store, "nihao"), &word),
        Some(2),
        "重开一致"
    );
}

/// 菜单：拼音给「固定到第 N 位」，子项到每页条数；当前所在名次那一项灰；前移/后移仍灰。
#[test]
fn pinyin_menu_offers_pin_at() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_menu");
    let coord = coord_at(&cfg, &store, "nihao");
    let pp = coord.debug_page_texts().len();
    let (enabled, kids) = pin_menu(&coord, 1).expect("拼音应有「固定到第 N 位」");
    assert!(enabled);
    assert_eq!(kids.len(), pp.min(20), "子项列到每页条数");
    for (i, (label, n, en)) in kids.iter().enumerate() {
        assert_eq!(*n as usize, i + 1);
        assert_eq!(label, &format!("第 {} 位", i + 1));
        assert_eq!(*en, i != 1, "第 2 个候选的「第 2 位」灰，其余可用");
    }
    let items = coord.candidate_menu_items(1).unwrap();
    for op in [CandidateOp::MoveUp, CandidateOp::MoveDown] {
        let it = items
            .iter()
            .find(|m| m.kind == wind_ui_types::MenuKind::Op(op))
            .unwrap();
        assert!(!it.enabled, "拼音 {op:?} 仍灰");
    }
    // 已在该位：点了不写规则（与菜单同判据）。
    coord.candidate_or_quick_format_op_at(CandidateOp::PinAt(2), 1);
    assert!(store.get_shadow_rules("pinyin", "nihao").unwrap().is_none());
}

/// 候选少于每页条数时子项只列到候选总数。
#[test]
fn pin_menu_capped_by_candidate_count() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_few");
    let coord = coord_at(&cfg, &store, "nihao");
    let all = coord.debug_all_candidate_texts();
    let pp = coord.debug_page_texts().len();
    let (_, kids) = pin_menu(&coord, 0).unwrap();
    assert_eq!(kids.len(), pp.min(20).min(all.len()));
    assert!(!kids[0].2, "首位的「第 1 位」灰");
}

/// 双拼：规则落在归一码 `nihao`（与全拼共用），全拼下也在第 3 位。
#[test]
fn shuangpin_pin_at_keys_on_normalized_code() {
    skip_without_data!();
    let sp = config("shuangpin");
    let full = config("pinyin");
    let store = fresh_store("sp3");
    let word = pin_at(&sp, &store, "nihc", 5, 3);
    assert_eq!(pinned_pos(&store, "pinyin", "nihao", &word), Some(2));
    assert!(store.get_shadow_rules("pinyin", "nihc").unwrap().is_none());
    assert_eq!(pos_of(&texts(&sp, &store, "nihc"), &word), Some(2));
    assert_eq!(pos_of(&texts(&full, &store, "nihao"), &word), Some(2));
}

/// 混输拼音段：固定到第 3 位，重开仍在第 3 位。
#[test]
fn mixed_pinyin_segment_pin_at() {
    skip_without_data!();
    let cfg = config("wubi86_pinyin");
    let store = fresh_store("mix3");
    let coord = coord_at(&cfg, &store, "nihao");
    let all = coord.debug_all_candidate_texts();
    assert!(
        pin_menu(&coord, 4).is_some_and(|(e, _)| e),
        "混输给「固定到第 N 位」"
    );
    let (idx, word) = distinct_at(&all, 4);
    coord.candidate_or_quick_format_op_at(CandidateOp::PinAt(3), idx);
    assert_eq!(pos_of(&texts(&cfg, &store, "nihao"), &word), Some(2));
}

/// 码表共用：五笔 `aa` 固定到第 3 位。
#[test]
fn codetable_pin_at() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("ct3");
    let word = pin_at(&cfg, &store, "aa", 5, 3);
    assert_eq!(pinned_pos(&store, "wubi86", "aa", &word), Some(2));
    assert_eq!(pos_of(&texts(&cfg, &store, "aa"), &word), Some(2));
}

/// 同码钉三个词到第 1/2/3 位（顺序打乱着钉）：重开后前三位正是它们，怎么调频都不动。
#[test]
fn pinyin_three_words_pinned_to_top_three() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py123");
    let all = texts(&cfg, &store, "nihao");
    let (_, a) = distinct_at(&all, 6);
    let (_, b) = distinct_at(&all, 8);
    let (_, c) = distinct_at(&all, 4);
    for (w, n) in [(&b, 2u8), (&c, 3), (&a, 1)] {
        let coord = coord_at(&cfg, &store, "nihao");
        let idx = pos_of(&coord.debug_all_candidate_texts(), w).unwrap();
        coord.candidate_or_quick_format_op_at(CandidateOp::PinAt(n), idx);
    }
    let now = texts(&cfg, &store, "nihao");
    assert_eq!(now[..3], [a, b, c], "{now:?}");
}

/// 整句与被钉的词同列：位置计数含整句（整句在第 1 位，被钉的词在第 3 位）。
#[test]
fn pinyin_pin_counts_sentence() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_sent");
    let input = "tazaijialikanshu";
    let before = texts(&cfg, &store, input);
    assert_eq!(before[0], "他在家里看书", "前提：首位是整句：{before:?}");
    let word = pin_at(&cfg, &store, input, 4, 3);
    let now = texts(&cfg, &store, input);
    assert_eq!(now[0], "他在家里看书", "{now:?}");
    assert_eq!(pos_of(&now, &word), Some(2), "{now:?}");
}

/// 目标不在列表里：规则不生效，列表不变。
#[test]
fn pin_rule_for_absent_word_is_inert() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_absent");
    let before = texts(&cfg, &store, "nihao");
    store
        .pin_shadow("pinyin", "nihao", "不在列表里的词", None, 2)
        .unwrap();
    assert_eq!(texts(&cfg, &store, "nihao"), before);
}

/// 被 `*` 屏蔽的词即使有位置规则也不出候选；位置规则不清屏蔽。
#[test]
fn blocked_word_with_pin_stays_hidden() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_blocked");
    let coord = coord_at(&cfg, &store, "nihao");
    let idx = pos_of(&coord.debug_all_candidate_texts(), "你好").unwrap();
    coord.candidate_or_quick_format_op_at(CandidateOp::Delete, idx);
    store
        .pin_shadow("pinyin", "nihao", "你好", None, 2)
        .unwrap();
    assert!(store.blocked_words("pinyin").unwrap().contains("你好"));
    assert!(pos_of(&texts(&cfg, &store, "nihao"), "你好").is_none());
}

/// 固定位置清掉同码同词的隐藏行。
#[test]
fn pin_clears_same_word_deleted() {
    skip_without_data!();
    let cfg = config("pinyin");
    let store = fresh_store("py_undel");
    store.delete_shadow("pinyin", "nihao", "你好").unwrap();
    assert!(pos_of(&texts(&cfg, &store, "nihao"), "你好").is_none());
    store
        .pin_shadow("pinyin", "nihao", "你好", None, 2)
        .unwrap();
    let rec = store.get_shadow_rules("pinyin", "nihao").unwrap().unwrap();
    assert!(rec.deleted.is_empty());
    assert_eq!(pos_of(&texts(&cfg, &store, "nihao"), "你好"), Some(2));
}

/// 临时拼音按**它自己的列表**应用 pinyin 桶里 N>0 的规则，不报错：先在临拼里看清目标词，
/// 写下第 3 位的规则，再进临拼，它就在临拼列表第 3 位。
#[test]
fn temp_pinyin_applies_pin_on_its_own_list() {
    skip_without_data!();
    let mut cfg = config("wubi86");
    cfg.schema.primary_pinyin = "pinyin".into();
    let store = fresh_store("tp3");
    let enter = |store: &Arc<Store>| {
        let coord = open(&cfg, store, false).unwrap();
        coord.handle_key_event(&KeyEventData {
            key_code: wind_keys::keymap::VK_BACKTICK,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
        assert!(coord.debug_in_temp_pinyin(), "前提：反引号进临拼");
        type_keys(&coord, "nihao");
        coord.debug_all_candidate_texts()
    };
    let before = enter(&store);
    let (_, word) = distinct_at(&before, 5);
    store.pin_shadow("pinyin", "nihao", &word, None, 2).unwrap();
    let now = enter(&store);
    assert_eq!(pos_of(&now, &word), Some(2), "{now:?}");
}
