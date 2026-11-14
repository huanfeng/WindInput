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
    assert_eq!(
        coord_at(&sp, &fresh_store("sp3_plain"), "nihc").debug_preedit(),
        SP_PREEDIT,
        "前提：未固定时的编码栏"
    );
    assert_eq!(coord_at(&sp, &store, "nihc").debug_preedit(), SP_PREEDIT);
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
    let again = coord_at(&cfg, &store, "nihao");
    assert_eq!(pos_of(&again.debug_all_candidate_texts(), &word), Some(2));
    assert_eq!(again.debug_preedit(), "ni'hao", "编码栏仍是拼音切分");
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

/// 双拼 `nihc` 的编码栏形态（按击键切分；固定位置不改它）。
const SP_PREEDIT: &str = "ni'hc";

// ── 前移 / 后移与已有固定规则的邻居交换 ────────────────────────────────────

fn ct_top(cfg: &Config, store: &Arc<Store>, n: usize) -> Vec<String> {
    texts(cfg, store, "aa")[..n].to_vec()
}

fn ct_op(cfg: &Config, store: &Arc<Store>, op: CandidateOp, idx: usize) {
    coord_at(cfg, store, "aa").candidate_or_quick_format_op_at(op, idx);
}

/// `[a,b,c,d]` 后移 a 得 `[b,a,c,d]`，再后移 b 应换回 `[a,b,c,d]`。
#[test]
fn move_down_onto_pinned_neighbor_swaps_back() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("ct_swap");
    let base = ct_top(&cfg, &store, 4);
    ct_op(&cfg, &store, CandidateOp::MoveDown, 0);
    assert_eq!(
        ct_top(&cfg, &store, 4),
        [
            base[1].clone(),
            base[0].clone(),
            base[2].clone(),
            base[3].clone()
        ]
    );
    ct_op(&cfg, &store, CandidateOp::MoveDown, 0);
    assert_eq!(ct_top(&cfg, &store, 4), base);
}

/// 置顶 b 得 `[b,a,c]`，再后移 b 回到 `[a,b,c]`。
///
/// 对照用例：a 没有规则，不触发交换，修复前也是绿的；钉住的是「置顶后再后移」这条路不回归。
#[test]
fn move_top_then_move_down_restores() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("ct_top_down");
    let base = ct_top(&cfg, &store, 3);
    ct_op(&cfg, &store, CandidateOp::MoveTop, 1);
    ct_op(&cfg, &store, CandidateOp::MoveDown, 0);
    assert_eq!(ct_top(&cfg, &store, 3), base);
}

/// 对照（前移）：置顶 c 得 `[c,a,b,d]`，前移 a 与 c 交换得 `[a,c,b,d]`。
///
/// 对照用例：前移写的规则是最新的、同位时排在前面，本身就等于交换，修复前也是绿的。
#[test]
fn move_up_onto_pinned_neighbor_swaps() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("ct_up");
    let base = ct_top(&cfg, &store, 4);
    ct_op(&cfg, &store, CandidateOp::MoveTop, 2);
    ct_op(&cfg, &store, CandidateOp::MoveUp, 1);
    assert_eq!(
        ct_top(&cfg, &store, 4),
        [
            base[0].clone(),
            base[2].clone(),
            base[1].clone(),
            base[3].clone()
        ]
    );
}

// ── 英文数据方案不给「固定到第 N 位」──────────────────────────────────────

fn config_with_english(active: &str) -> Config {
    let mut c = config(active);
    c.schema.available.push("english".into());
    c.input.temp_english.enabled = true;
    c
}

/// 英文方案与临英：菜单没有这一项，写端也不写规则；置顶照旧可用。
#[test]
fn english_has_no_pin_at() {
    skip_without_data!();
    for (active, input) in [("english", "hel"), ("wubi86", "Hel")] {
        let cfg = config_with_english(active);
        let store = fresh_store(&format!("en_{active}"));
        let coord = open(&cfg, &store, false).unwrap();
        for ch in input.chars() {
            coord.handle_key_event_policed(&KeyEventData {
                key_code: (ch.to_ascii_uppercase() as u32) & 0xFF,
                scan_code: 0,
                modifiers: if ch.is_ascii_uppercase() {
                    wind_ipc::protocol::MOD_SHIFT
                } else {
                    0
                },
                event_type: EVENT_KEY_DOWN,
                toggles: 0,
                event_seq: 0,
                prev_char: 0,
                seq_char: 0,
            });
        }
        let all = coord.debug_all_candidate_texts();
        let idx = 3.min(all.len() - 1);
        assert!(
            pin_menu(&coord, idx).is_none(),
            "{active}: 不给「固定到第 N 位」"
        );
        let items = coord.candidate_menu_items(idx).unwrap();
        assert!(
            items
                .iter()
                .any(|m| m.kind == wind_ui_types::MenuKind::Op(CandidateOp::MoveTop) && m.enabled),
            "{active}: 置顶照旧"
        );
        coord.candidate_or_quick_format_op_at(CandidateOp::PinAt(1), idx);
        assert!(
            store.list_shadow_rules("english").unwrap().is_empty(),
            "{active}: 写端同判据不写"
        );
    }
}

// ── 「第 N 位」是界面名次（开简繁时含变体）────────────────────────────────

/// 五笔 `bm` 开简繁：`[…, 出, 齣, X, Y, …]`。把 Y 固定到界面第 N 位，重开（开简繁）后它就在
/// 界面第 N 位；紧跟「出」的那一格（变体占着）灰、写端不写。
#[test]
fn pin_at_counts_ui_slots_with_s2t_variants() {
    skip_without_data!();
    let cfg = config("wubi86");
    let probe = fresh_store("s2t_probe");
    let Some(coord) = open(&cfg, &probe, true) else {
        return;
    };
    type_keys(&coord, "bm");
    let all = coord.debug_all_candidate_texts();
    let chu = pos_of(&all, "出").unwrap_or_else(|| panic!("前提：`bm` 有「出」：{all:?}"));
    assert_eq!(
        all.get(chu + 1).map(String::as_str),
        Some("出"),
        "前提：变体：{all:?}"
    );
    let (y, word) = distinct_at(&all, chu + 3);
    // 变体那一格（界面第 chu+2 位）落不到。
    let (_, kids) = pin_menu(&coord, y).unwrap();
    let k = kids
        .get(chu + 1)
        .unwrap_or_else(|| panic!("前提：子菜单列到变体那一格（第 {} 位）：{kids:?}", chu + 2));
    assert!(!k.2, "变体那一格应灰：{kids:?}");
    coord.candidate_or_quick_format_op_at(CandidateOp::PinAt((chu + 2) as u8), y);
    assert!(
        probe.get_shadow_rules("wubi86", "bm").unwrap().is_none(),
        "落不到的格子不写"
    );

    let mut targets = vec![chu + 2];
    if chu > 0 {
        targets.push(0);
    }
    for t in targets {
        let store = fresh_store(&format!("s2t_{t}"));
        let c = open(&cfg, &store, true).unwrap();
        type_keys(&c, "bm");
        let idx = pos_of(&c.debug_all_candidate_texts(), &word).unwrap();
        c.candidate_or_quick_format_op_at(CandidateOp::PinAt((t + 1) as u8), idx);
        let again = open(&cfg, &store, true).unwrap();
        type_keys(&again, "bm");
        let now = again.debug_all_candidate_texts();
        assert_eq!(pos_of(&now, &word), Some(t), "界面第 {} 位：{now:?}", t + 1);
    }
}

/// 两条规则同在第 2 位（Y 新），界面 `[a,Y,X,…]`；前移 X 得 `[a,X,Y,…]`。
///
/// 前移写下的规则最新、同位时排在前面，本身就是交换的效果，故这一条**不依赖**交换代码
/// （去掉交换实测仍绿）；真正依赖交换的是后移，见 `move_down_onto_pinned_neighbor_swaps_back`。
#[test]
fn move_up_past_same_position_pin() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("ct_up_same");
    let base = ct_top(&cfg, &store, 5);
    let (x, y) = (&base[3], &base[4]);
    store.pin_shadow("wubi86", "aa", x, None, 1).unwrap();
    store.pin_shadow("wubi86", "aa", y, None, 1).unwrap();
    assert_eq!(
        ct_top(&cfg, &store, 3),
        [base[0].clone(), y.clone(), x.clone()]
    );
    ct_op(&cfg, &store, CandidateOp::MoveUp, 2);
    assert_eq!(
        ct_top(&cfg, &store, 3),
        [base[0].clone(), x.clone(), y.clone()]
    );
}

/// 本码下有一条按短语 id 定位的规则，它记的旧求值文本恰好与邻居同文：后移时邻居（没有那个
/// id）不算「有规则」，那条 id 规则原样保留，不被交换改写或清掉。
#[test]
fn swap_ignores_id_rule_with_same_stale_text() {
    skip_without_data!();
    let cfg = config("wubi86");
    let store = fresh_store("ct_swap_id");
    let base = ct_top(&cfg, &store, 3);
    store
        .pin_shadow("wubi86", "aa", &base[1], Some("phrase:aa:stale"), 7)
        .unwrap();
    ct_op(&cfg, &store, CandidateOp::MoveDown, 0);
    let rec = store.get_shadow_rules("wubi86", "aa").unwrap().unwrap();
    let id_rule = rec
        .pinned
        .iter()
        .find(|p| p.cand_id.as_deref() == Some("phrase:aa:stale"))
        .unwrap_or_else(|| panic!("id 规则被清掉：{:?}", rec.pinned));
    assert_eq!(id_rule.position, 7, "id 规则不被改写");
    assert!(
        !rec.pinned
            .iter()
            .any(|p| p.cand_id.is_none() && p.word == base[1]),
        "不给邻居新写按词规则：{:?}",
        rec.pinned
    );
    assert_eq!(ct_top(&cfg, &store, 2), [base[1].clone(), base[0].clone()]);
}
