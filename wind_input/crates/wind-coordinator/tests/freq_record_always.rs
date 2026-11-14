//! 「仅记录次数」`schema.*.frequency.record_always`（论坛 t278）的**行为级**对照。
//!
//! 语义：调频关、本项开 ⇒ 选词照记 FREQ，但候选顺序**与没有任何记录时逐字相同**。
//! 写端判据是 `FreqSettings::records()`，读端（重排 / 英文召回 / 联想）只认 `enabled`；
//! 读端误改成 `records()` 时，单看配置字段的断言测不出来，必须拿真实词库比候选页。
//!
//! 依赖 `build_dev/data` 的 wubi86 / english 方案，缺则跳过。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::RawCandidateMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const VK_SPACE: u32 = 0x20;
const VK_ESCAPE: u32 = 0x1B;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schema(id: &str) -> bool {
    data_dir()
        .join(format!("schemas/{id}.schema.toml"))
        .exists()
}

fn key(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn type_word(coord: &Coordinator, word: &str) {
    for c in word.chars() {
        coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn store_at(tag: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_record_always_it_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

/// 用 `cfg` 打 `input`，返回候选页文本。
fn page(cfg: &Config, store: &Arc<Store>, input: &str) -> Vec<String> {
    let coord =
        Coordinator::new_headless_with_store(cfg.clone(), Some(&data_dir()), Arc::clone(store));
    type_word(&coord, input);
    let p = coord.debug_page_texts();
    coord.handle_key_event(&key(VK_ESCAPE));
    p
}

fn wubi_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    // 一简位出厂保护首选；放开，让调频有机会动首位（正向对照才测得出东西）。
    let f = &mut cfg.schema.codetable.frequency;
    f.protect_top_n_len1 = 0;
    f.strategy = "top".into();
    cfg
}

fn english_config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "english".into();
    cfg.input.default.chinese_mode = true;
    // 原文不钉首位，首选完全由词库序 + 调频决定。
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    cfg
}

/// ★ 码表：库里有足以顶到首位的记录，「仅记录次数」下候选页与空库逐字相同。
#[test]
fn codetable_record_always_does_not_rerank() {
    if !has_schema("wubi86") {
        eprintln!("跳过：缺 wubi86");
        return;
    }
    let mut off = wubi_config();
    off.schema.codetable.frequency.enabled = false;
    off.schema.codetable.frequency.record_always = true;
    let base = page(&off, &store_at("ct_base"), "a");
    assert!(base.len() >= 2, "前提：`a` 应有至少 2 个候选：{base:?}");
    // 取第 2 位：一简位只有两个精确候选，再往后是前缀补全，词频跨不出档、顶不到首位。
    let target = base[1].clone();

    let seeded = store_at("ct_seeded");
    for _ in 0..5 {
        seeded.record_freq("wubi86", "a", &target).unwrap();
    }
    assert_eq!(
        page(&off, &seeded, "a"),
        base,
        "只记不调：有记录也不得改变候选顺序"
    );

    // 正向对照：同一份记录在调频开着时确实会把它顶上去，否则上一句什么也没测。
    let mut on = off.clone();
    on.schema.codetable.frequency.enabled = true;
    assert_eq!(
        page(&on, &seeded, "a").first(),
        Some(&target),
        "前提：调频开着时这份记录足以把 {target} 顶到首位"
    );
}

/// ★ 英文：同上（英文的读端还有热词召回一条路，一并经候选页覆盖）。
#[test]
fn english_record_always_does_not_rerank() {
    if !has_schema("english") {
        eprintln!("跳过：缺 english");
        return;
    }
    let mut off = english_config();
    off.schema.english.frequency.enabled = false;
    off.schema.english.frequency.record_always = true;
    let base = page(&off, &store_at("en_base"), "hell");

    let seeded = store_at("en_seeded");
    for _ in 0..9 {
        seeded.record_freq("english", "hello", "hello").unwrap();
    }
    assert_eq!(page(&off, &seeded, "hell"), base, "只记不调：顺序不得变");

    let mut on = off.clone();
    on.schema.english.frequency.enabled = true;
    let promoted = page(&on, &seeded, "hell");
    assert_ne!(
        promoted, base,
        "前提：调频开着时这份记录确实会改变顺序：{promoted:?}"
    );
}

/// ★ 英文写端：上屏的是**头部原文**（与词库词只差大小写：打 `usa`、词库是 `USA`）也要记账。
///
/// 头部候选的记账要先经 `english_head_dict_word` 换回词库词，那一步曾只认 `enabled`，
/// 于是「仅记录次数」在这条路上一条都不记。字面完全相同的（`hello`）不走这条路——
/// 头部那格保留的就是词库词本身，见 `english_head_keeps_dict_word`。
#[test]
fn english_record_always_records_typed_word() {
    if !has_schema("english") {
        eprintln!("跳过：缺 english");
        return;
    }
    for (record_always, expect) in [(true, true), (false, false)] {
        let mut cfg = english_config();
        cfg.schema.english.raw_candidate = RawCandidateMode::Always;
        cfg.schema.english.frequency.enabled = false;
        cfg.schema.english.frequency.record_always = record_always;
        let store = store_at(&format!("en_write_{record_always}"));
        let coord =
            Coordinator::new_headless_with_store(cfg, Some(&data_dir()), Arc::clone(&store));
        type_word(&coord, "usa");
        assert_eq!(
            coord.debug_page_texts().first().map(String::as_str),
            Some("usa"),
            "前提：Always 档首位是头部原文"
        );
        coord.handle_key_event(&key(VK_SPACE));
        assert_eq!(
            store.get_freq("english", "usa", "USA").unwrap().is_some(),
            expect,
            "record_always={record_always}"
        );
    }
}
