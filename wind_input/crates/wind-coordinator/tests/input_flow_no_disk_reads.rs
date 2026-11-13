//! 输入流程零读盘（docs/architecture/feature-placement-guide.md §6）：预热后在同一线程上
//! 打字、分隔、选词、上屏，不应再读任何方案文件。计数按线程（`schema_reads_on_current_thread`），
//! 后台预热线程的读盘不计入。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时整族静默跳过（判据是耗时 0.0x s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_engine::manager::schema_reads_on_current_thread;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_keys::keymap;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "wubi86_pinyin"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
}

fn key(k: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 字母按 VK 发；`'` 发分隔符键，` ` 发空格（选首选上屏），`!` 发 Esc。
fn type_keys(coord: &Coordinator, input: &str) {
    for ch in input.chars() {
        let vk = match ch {
            '\'' => keymap::VK_QUOTE,
            ' ' => keymap::VK_SPACE,
            '!' => keymap::VK_ESCAPE,
            c => c.to_ascii_uppercase() as u32,
        };
        coord.handle_key_event_policed(&key(vk));
    }
}

fn open(active: &str) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "wubi86_pinyin".into()];
    c.schema.active = active.into();
    c.input.default.chinese_mode = true;
    let path = std::env::temp_dir().join(format!("wind_no_disk_reads_{active}.redb"));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    Coordinator::new_headless_with_store(c, Some(&data_dir()), store)
}

/// 同一段输入打两遍：第一遍允许懒加载（建引擎、填缓存），第二遍一次方案文件也不许读。
fn assert_second_pass_reads_nothing(active: &str, script: &str) {
    assert_second_pass_reads_nothing_with(active, script, |c| type_keys(c, script));
}

fn assert_second_pass_reads_nothing_with(active: &str, what: &str, pass: impl Fn(&Coordinator)) {
    let coord = open(active);
    pass(&coord);
    let warmed = schema_reads_on_current_thread();
    pass(&coord);
    assert_eq!(
        schema_reads_on_current_thread(),
        warmed,
        "{active}：预热后 {what} 仍读了方案文件"
    );
}

/// 拼音段分段上屏：`nihao` 选单字「你」，剩余 `hao` 留在缓冲里继续转换（混输下这条路取
/// 拼音成员方案），再打一键让它重建候选。
fn segmented_commit(c: &Coordinator) {
    type_keys(c, "nihao");
    let texts = c.debug_all_candidate_texts();
    let idx = texts
        .iter()
        .position(|t| t == "你")
        .unwrap_or_else(|| panic!("前提：nihao 的候选里有「你」：{texts:?}"));
    assert!(idx < 9, "前提：「你」在首页：{texts:?}");
    c.debug_mouse_select(idx);
    assert_eq!(
        c.debug_input_buffer(),
        "hao",
        "前提：分段上屏后留有剩余编码"
    );
    type_keys(c, "m!");
}

#[test]
fn mixed_scheme_typing_reads_no_schema_files() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    // 码表码、拼音整句、分隔符键；分段上屏后的剩余编码（走 mixed_secondary_schema）单独一段。
    assert_second_pass_reads_nothing("wubi86_pinyin", "wqvb !nihaoma !womenshi  !xi'an !");
    assert_second_pass_reads_nothing_with("wubi86_pinyin", "分段上屏后续打", segmented_commit);
}

#[test]
fn pinyin_scheme_typing_reads_no_schema_files() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    assert_second_pass_reads_nothing("pinyin", "nihao !xi'an !womenshi  !");
}

#[test]
fn codetable_scheme_typing_reads_no_schema_files() {
    if !has_schemas() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    assert_second_pass_reads_nothing("wubi86", "wqvb !ggtt !a !");
}
