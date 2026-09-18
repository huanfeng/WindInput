//! 「重复上屏」候选（`quick_input.repeat`）**选词臂**的记账口径。
//!
//! `inject_mix_repeat_candidate` 的文档写着：这条候选与输入缓冲没有对应关系（码为空），
//! 「选词记录、造词、标点顶屏三条路径都必须绕开它」。空格臂与⑥标点臂都显式绕了，
//! **选词臂漏了** —— `mix_select` 一路走到 `record_selection_cand_in`，往词频库写一条
//! `code = ""` 的行，正是联想候选那条注释说的「读端按候选码永远查不中的孤儿键」。
//!
//! ⚠️ 两条容易写出「假绿」的地方，都在本文件里显式挡住：
//! 1. **词频出厂是关的**，不打开就是在测一个关着的功能，怎么写都"通过"；
//! 2. **必须用没有表达式类成员的 mix**，否则空缓冲按数字会走数字透镜的①作表达式输入，
//!    根本到不了选词臂。
//!
//! # 本文件**没有**覆盖的一半
//!
//! 记账还有一条「击键数按 1 封顶」（`record_commit_ks(text, 0, 1, 0, …)`，不封的话一键
//! 几十字会把速度统计顶穿）。本文件只查词频库，量不到它 —— 把那个 `1` 改回 `0` 两条测试
//! 都绿。之所以不补：两条臂现在共用 `commit_mix_repeat` 这**一个**方法，封顶写在里面，
//! 已经不存在「一处改了另一处没改」的漂移面；真要测它得读统计库，机械量与收益不相称。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("schemas/pinyin.schema.toml").exists()
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
    }
}

fn store_at(tag: &str) -> Arc<Store> {
    let path = std::env::temp_dir().join(format!("wind_mix_repeat_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    Arc::new(Store::open(&path).unwrap())
}

/// 只留真实方案 + `quick_input.repeat`：去掉计算/日期/数字三个表达式类来源，
/// 使 `mix_has_quick_numeric` 为假 —— 空缓冲按数字键才会落到**文本透镜的选词臂**。
fn config() -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.frequency.enabled = true; // 出厂 false，不开等于没测
    cfg.schema.english.frequency.enabled = true;
    cfg.schema.pinyin.frequency.enabled = true;
    cfg.schema.mix_modes[0]
        .members
        .retain(|m| !wind_quick_input::is_quick_member(m) || m == wind_quick_input::MEMBER_REPEAT);
    cfg
}

/// 库里所有方案下 `code` 为空的词频行。
fn empty_code_rows(store: &Store) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for schema in ["wubi86", "pinyin", "english", "wubi86_pinyin"] {
        let Ok((rows, _)) = store.list_freq_paged(schema, "", 0, 0) else {
            continue;
        };
        for (code, text, _) in rows {
            if code.is_empty() {
                out.push((schema.to_string(), code, text));
            }
        }
    }
    out
}

/// ★ 用**选词键**取走重复上屏候选，不得往词频库写空码行。
///
/// 对照的是空格臂：它走 `record_commit_ks(text, 0, 1, 0, …)`，压根不碰词频。
#[test]
fn repeat_candidate_picked_by_select_key_records_no_empty_code_freq() {
    if !has_schemas() {
        return;
    }
    let store = store_at("select");
    let coord = Coordinator::new_headless_with_store(config(), Some(&data_dir()), store.clone());

    // 先正常上屏一次，喂出上屏历史（重复候选的数据源）。
    coord.handle_key_event(&key(0xBA)); // ;
    for vk in [0x47, 0x47] {
        coord.handle_key_event(&key(vk)); // gg
    }
    coord.handle_key_event(&key(0x20)); // 空格上屏
    let base: Vec<_> = empty_code_rows(&store);
    assert!(
        base.is_empty(),
        "前提：正常选词不该留下空码行，实际 {base:?}"
    );

    // 再进快捷输入：空缓冲 → 重复候选。
    coord.handle_key_event(&key(0xBA)); // ;
    let texts = coord.debug_page_texts();
    assert_eq!(
        texts.len(),
        1,
        "前提：空缓冲应只有一条重复候选，实际 {texts:?}"
    );

    // 用**数字选词键**取它（而不是空格）—— 这一条通路此前没有绕开记账。
    coord.handle_key_event(&key(0x31)); // 1

    let stray = empty_code_rows(&store);
    assert!(
        stray.is_empty(),
        "重复上屏候选没有编码，不得记词频（空码行永远查不中）：{stray:?}"
    );
}

/// ★反向对照：同一条候选用**空格**取走，本就不记 —— 证明上面那条断言不是「这个配置下
/// 根本写不进词频」。没有它，把 `record_freq` 整个删掉两条都会绿。
#[test]
fn repeat_candidate_picked_by_space_records_nothing_either() {
    if !has_schemas() {
        return;
    }
    let store = store_at("space");
    let coord = Coordinator::new_headless_with_store(config(), Some(&data_dir()), store.clone());
    coord.handle_key_event(&key(0xBA));
    for vk in [0x47, 0x47] {
        coord.handle_key_event(&key(vk));
    }
    coord.handle_key_event(&key(0x20));
    // 前提：这个配置下词频**确实写得进**（否则上面那条测试是空过的）。
    let mut any = false;
    for schema in ["wubi86", "pinyin", "english", "wubi86_pinyin"] {
        if let Ok((rows, total)) = store.list_freq_paged(schema, "", 0, 0) {
            any |= total > 0 || !rows.is_empty();
        }
    }
    assert!(any, "前提：正常选词须真的写进了词频，否则本族测试恒绿");

    coord.handle_key_event(&key(0xBA));
    coord.handle_key_event(&key(0x20)); // 空格重复上屏
    assert!(
        empty_code_rows(&store).is_empty(),
        "空格臂本就不记（它是选词臂该对齐的那条）"
    );
}
