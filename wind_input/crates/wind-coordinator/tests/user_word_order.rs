//! 用户词库同码排序的端到端验证。
//!
//! `wind-store` 的单测只验到 redb 记录带正确的入库序号；从 redb 到候选列表还隔着
//! `StoreUserLayer` → `record_to_candidate` → `better()`，任一处丢掉 `natural_order`
//! 就会「库里存对了、屏幕上照样是字典序」。本文件走完整链路：预置 store → 建 `Coordinator`
//! → 真打码 → 读候选列表。
//!
//! 词典缺失时整族静默跳过（判据是耗时 0.00s，不是通过条数）。worktree 里需自备
//! `build_dev` 链接。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    // 三级：crates/wind-coordinator → crates → wind_input → 仓库根（build_dev 在仓库根）。
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_pinyin() -> bool {
    let d = data_dir();
    d.join("schemas/pinyin.schema.toml").exists() || d.join("schemas/pinyin.schema.yaml").exists()
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

fn type_code(coord: &Coordinator, s: &str) {
    for c in s.chars() {
        coord.handle_key_event(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg
}

fn row(code: &str, text: &str, weight: i32) -> wind_store::wdict::WordIo {
    wind_store::wdict::WordIo {
        code: code.into(),
        text: text.into(),
        weight,
        count: 0,
        boundary: None,
    }
}

/// 同码用户词在候选列表里按导入文件的先后排。
///
/// 三条给同一个高权重：`better()` 先比 weight，等权时才轮到 `natural_order`，不等权就退化成
/// 测权重排序；高权重也保证它们落在第一页。只断言三条的相对次序，不绑死整个候选列表。
#[test]
fn user_words_appear_in_import_order() {
    if !has_pinyin() {
        return;
    }
    let store_path = std::env::temp_dir().join("wind_e2e_user_word_order.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());

    // 刻意让导入序 ≠ 字典序：按「丙丙 乙乙 甲甲」导入。
    const W: i32 = 9_999_999;
    store
        .import_user_words(
            "pinyin",
            &[
                row("ceshi", "丙丙", W),
                row("ceshi", "乙乙", W),
                row("ceshi", "甲甲", W),
            ],
        )
        .expect("预置用户词失败");

    let coord = Coordinator::new_headless_with_store(config(), Some(&data_dir()), store);
    type_code(&coord, "ceshi");

    let texts = coord.debug_page_texts();
    let pos = |t: &str| texts.iter().position(|x| x == t);
    let (bing, yi, jia) = (pos("丙丙"), pos("乙乙"), pos("甲甲"));
    assert!(
        bing.is_some() && yi.is_some() && jia.is_some(),
        "三条用户词都应出现在候选里，实际候选: {texts:?}"
    );
    assert!(
        bing < yi && yi < jia,
        "同码用户词应按导入行序排（丙丙 → 乙乙 → 甲甲），实际候选: {texts:?}"
    );
}

/// 反向对照：把导入顺序倒过来，候选顺序必须跟着倒过来。
///
/// 上一条的导入序「丙 乙 甲」恰好就是字典升序，只靠它排除不了「恒按 text 字典序」的实现；
/// 这条导入「甲甲 乙乙 丙丙」（与字典序相反），两条合起来才能区分两种实现。
#[test]
fn user_words_follow_import_order_not_lexicographic() {
    if !has_pinyin() {
        return;
    }
    let store_path = std::env::temp_dir().join("wind_e2e_user_word_order_rev.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());

    const W: i32 = 9_999_999;
    store
        .import_user_words(
            "pinyin",
            &[
                row("ceshi", "甲甲", W),
                row("ceshi", "乙乙", W),
                row("ceshi", "丙丙", W),
            ],
        )
        .expect("预置用户词失败");

    let coord = Coordinator::new_headless_with_store(config(), Some(&data_dir()), store);
    type_code(&coord, "ceshi");

    let texts = coord.debug_page_texts();
    let pos = |t: &str| texts.iter().position(|x| x == t);
    let (jia, yi, bing) = (pos("甲甲"), pos("乙乙"), pos("丙丙"));
    assert!(
        jia.is_some() && yi.is_some() && bing.is_some(),
        "三条用户词都应出现在候选里，实际候选: {texts:?}"
    );
    assert!(
        jia < yi && yi < bing,
        "候选应跟随导入序（甲甲 → 乙乙 → 丙丙）而非 text 字典序，实际候选: {texts:?}"
    );
}
