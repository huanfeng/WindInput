//! 用户词库同码排序的**端到端**验证（t80）。
//!
//! `wind-store` 里那几条单测只验到「redb 记录带正确的入库序号」，而从 redb 到用户眼前的
//! 候选列表之间还隔着 `StoreUserLayer` → `record_to_candidate` → `better()` 三道。
//! 那段没人看着，就可能出现「库里存对了、屏幕上照样是字典序」——这正是缺陷期的形态
//! （`natural_order` 取 `Default` 恒 0）。本文件走完整链路：
//! 预置 store → 建 `Coordinator` → 真打码 → 读候选列表。
//!
//! ⚠️ 词典缺失时整族静默跳过（判据是**耗时 0.00s**，不是通过条数）。worktree 里需自备
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

/// 同码用户词在**候选列表里**按导入文件的先后排（t80 的真正验收标准）。
///
/// 三条词按「丙丙 → 乙乙 → 甲甲」导入，而它们的 text 字典序恰好相反
/// （甲 U+7532 > 乙 U+4E59 > 丙 U+4E19，故字典序是 丙 < 乙 < 甲……
/// 注意这里要的是**导入序与字典序不同**，下面的断言只认导入序）。
///
/// 三条给**同一个高权重**：`better()` 的第一档是 weight 降序，只有等权时才轮到
/// `natural_order`——不等权的话这条测试就退化成在测权重排序，什么也没验到。
/// 高权重同时保证它们落在第一页，不必翻页。
///
/// 判据只看这三条彼此的**相对次序**，不管周围有多少系统词：真实词库的内容会随版本变，
/// 绑死整个候选列表只会让测试在无关改动上误报。
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
/// 没有这一条，「恒按 text 字典序」的实现也能让上面那条通过——上面用的
/// 「丙 乙 甲」恰好**就是**字典升序。这一条用「甲甲 乙乙 丙丙」导入，
/// 期望候选也是「甲甲 乙乙 丙丙」，与字典序相反，两条合起来才能同时排除
/// 「按导入序」与「按字典序」两种实现。
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
