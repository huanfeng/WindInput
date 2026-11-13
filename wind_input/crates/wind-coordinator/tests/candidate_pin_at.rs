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
