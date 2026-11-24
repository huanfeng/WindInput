//! 智能档「始终追加」（`input.scope_relax.always_append`）端到端。
//!
//! 背景：智能档会滤掉「同码位已有常用字」的生僻字，PC 靠「末页再按一次翻页键」把它们调回来；
//! 手机没有翻页键，那条出路不存在，于是这些字彻底打不出（实测：98 五笔导入后 22% 的单字
//! 出不来）。本开关让被滤的候选**直接追加在末尾**（带 `is_scope_filtered`，沉底、不参与
//! 满码自动上屏计数、不参与词频重排，与翻页放宽出来的完全同源）。
//!
//! 装置沿用 `common_chars_override.rs`：五笔 `sivg` 上坐着常用的「档」与生僻的「桜」。
//!
//! ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**（判据是耗时：正常秒级）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready(d: &std::path::Path) -> bool {
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
}

fn key_event(key_code: u32) -> KeyEventData {
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

fn store(tag: &str) -> Arc<wind_store::Store> {
    let p = std::env::temp_dir().join(format!(
        "wind_always_append_{}_{}.redb",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_file(&p);
    Arc::new(wind_store::Store::open(&p).unwrap())
}

fn coord(tag: &str, mode: &str, always_append: bool) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = mode.into();
    cfg.input.scope_relax.always_append = always_append;
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store(tag))
}

fn cands_of(coord: &Coordinator, code: &str) -> Vec<String> {
    for _ in 0..12 {
        coord.handle_key_event_policed(&key_event(0x08));
    }
    for c in code.chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
    coord.debug_all_candidate_texts()
}

/// 默认关：智能档的既有表现纹丝不动（PC 出厂就是这个）。
#[test]
fn off_by_default_smart_list_is_unchanged() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    let c = coord("off", "smart", false);
    let list = cands_of(&c, "sivg");
    assert!(list.contains(&"档".to_string()), "前置：{list:?}");
    assert!(
        !list.contains(&"桜".to_string()),
        "未开启时智能档不该自动带出生僻字，实际 {list:?}"
    );
}

/// 开启后：被滤的生僻字出现，且**沉底**——常用字的位置与顺序不变。
#[test]
fn on_appends_filtered_chars_after_the_normal_ones() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    let base = cands_of(&coord("base", "smart", false), "sivg");
    let list = cands_of(&coord("on", "smart", true), "sivg");
    assert!(
        list.contains(&"桜".to_string()),
        "开启后被滤的生僻字应直接出现，实际 {list:?}"
    );
    assert_eq!(
        &list[..base.len()],
        &base[..],
        "正常候选的位置与顺序必须原样保留，生僻字只追加在后面"
    );
    assert_eq!(
        list.last().map(String::as_str),
        Some("桜"),
        "追加在末尾，实际 {list:?}"
    );
}

/// 常用字档要的就是「稳定只出常用字」；全部字符档本就不过滤。开关两头都不该碰。
#[test]
fn general_and_gb18030_modes_ignore_the_switch() {
    if !dict_ready(&data_dir()) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    let general = cands_of(&coord("gen", "general", true), "sivg");
    assert!(
        !general.contains(&"桜".to_string()),
        "常用字档不该因本开关放出生僻字，实际 {general:?}"
    );
    let all_off = cands_of(&coord("gb_off", "gb18030", false), "sivg");
    let all_on = cands_of(&coord("gb_on", "gb18030", true), "sivg");
    assert_eq!(
        all_off, all_on,
        "全部字符档开不开本开关结果应一致（不得重复追加）"
    );
}
