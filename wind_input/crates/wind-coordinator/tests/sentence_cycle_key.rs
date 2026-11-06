//! 整句切换键（`schema.pinyin.sentence_cycle_key`）端到端：按键真的走 `handle_key_event`。
//!
//! 纯窗口逻辑（`cycle_sentence_window`）在 `handle_candidate.rs` 的单测里；本文件钉的是
//! **接线与守卫**——尤其是「不该吃键时一定不吃」：Tab 出厂在高亮组里（移高亮），被错吃
//! 一次，用户看到的就是「Tab 有时好用有时没反应」。
//!
//! 依赖 `build_dev/data`（真实拼音词库）；缺失时跳过。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

const VK_TAB: u32 = 0x09;
const VK_DOWN: u32 = 0x28;
const VK_BACKTICK: u32 = 0xC0;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_pinyin() -> bool {
    data_dir().join("schemas/pinyin").is_dir()
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

fn coord(count: u8, max: u8, cycle_key: &str, tag: &str) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.sentence_count = count;
    cfg.schema.pinyin.sentence_max_count = max;
    cfg.schema.pinyin.sentence_cycle_key = cycle_key.into();
    // 文件名带进程号：并发会话同时跑本文件时不互相覆盖（审查查出）。
    let path = std::env::temp_dir().join(format!(
        "wind_sentence_cycle_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    Coordinator::new_headless_with_store(
        cfg,
        Some(&data_dir()),
        Arc::new(Store::open(&path).unwrap()),
    )
}

fn type_pinyin(c: &Coordinator, s: &str) {
    for ch in s.chars() {
        c.handle_key_event(&key(0x41 + (ch as u32 - 'a' as u32)));
    }
}

/// 出厂 `sentence_max_count = 1`：配了切换键也**不吃** —— 池子恒空。
///
/// Tab 出厂在高亮组里，落回原义的可观测结局是「候选不变、高亮下移」。
#[test]
fn factory_pool_never_takes_the_key() {
    if !has_pinyin() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let c = coord(1, 1, "tab", "factory");
    type_pinyin(&c, "yougailunma");
    let before = c.debug_page_texts();
    let (_, sel_before, _) = c.debug_page_info();
    c.handle_key_event(&key(VK_TAB));
    assert_eq!(c.debug_page_texts(), before, "池子为空时不得换候选");
    assert_ne!(
        c.debug_page_info().1,
        sel_before,
        "Tab 必须落回原义（出厂 = 高亮下移），没动说明被错吃了"
    );
}

/// 露 1 算 3：同一个位置轮流换三种解读，第三次回卷。
#[test]
fn single_slot_cycles_and_wraps() {
    if !has_pinyin() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let c = coord(1, 3, "tab", "single");
    type_pinyin(&c, "yougailunma");
    let first = c.debug_page_texts()[0].clone();
    // 先把高亮挪开：切换前高亮本就在 0 的话，「高亮落在换进来的那条上」这条断言看不出任何事。
    c.handle_key_event(&key(VK_DOWN));
    assert_eq!(c.debug_page_info().1, 1, "前提：方向键把高亮移到了第 2 条");
    c.handle_key_event(&key(VK_TAB));
    let second = c.debug_page_texts()[0].clone();
    assert_ne!(
        second,
        first,
        "Tab 应换成另一种整句解读: {:?}",
        c.debug_page_texts()
    );
    assert_eq!(c.debug_page_info().1, 0, "高亮落在换进来的那条上");
    c.handle_key_event(&key(VK_TAB));
    c.handle_key_event(&key(VK_TAB));
    assert_eq!(c.debug_page_texts()[0], first, "按满一圈回卷到最优解");
}

/// 露 3 算 3：整句块占最前 3 位，Tab 让三条一起滚动。
#[test]
fn block_of_three_is_on_top_and_rotates() {
    if !has_pinyin() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let c = coord(3, 3, "tab", "block");
    type_pinyin(&c, "yougailunma");
    let top: Vec<String> = c.debug_page_texts()[..3].to_vec();
    // 整句消费整串 `you|gai|lun|ma` ⇒ 恰好四字；三条互不相同。
    // （真机实测：有概论吗 / 又概论吗 / 有概论嘛，其后才是「又该」「由该」这类部分候选。）
    assert!(
        top.iter().all(|t| t.chars().count() == 4),
        "最前 3 位应是整句块（四字）: {:?}",
        c.debug_page_texts()
    );
    let uniq: std::collections::HashSet<&String> = top.iter().collect();
    assert_eq!(uniq.len(), 3, "整句块三条互不相同: {top:?}");
    c.handle_key_event(&key(VK_TAB));
    let after: Vec<String> = c.debug_page_texts()[..3].to_vec();
    assert_eq!(after, vec![top[1].clone(), top[2].clone(), top[0].clone()]);
}

/// 键留空 = 关闭：开了 N-best 也不夺取 Tab。键即开关，没有第二道闸。
#[test]
fn empty_key_means_off_even_with_nbest() {
    if !has_pinyin() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let c = coord(3, 3, "", "off");
    type_pinyin(&c, "yougailunma");
    let before = c.debug_page_texts();
    c.handle_key_event(&key(VK_TAB));
    assert_eq!(c.debug_page_texts(), before, "没配键不得换候选");
}

/// 切换键若恰是当前方案的**音节分隔符**，一律让位（审查查出）。
///
/// 全拼出厂 `separator = "auto"`：`'` 被选词键占着时反引号就是分隔符。用户把切换键选成反引号、
/// 开了 N-best，打到 2 个音节以上（池子才非空）分隔符就按不进去了。
#[test]
fn separator_key_is_never_taken() {
    if !has_pinyin() {
        eprintln!("跳过：缺少 build_dev/data");
        return;
    }
    let c = coord(1, 3, "backtick", "separator");
    type_pinyin(&c, "yougailunma");
    let act = c.handle_key_event(&key(VK_BACKTICK));
    assert!(
        !matches!(act, wind_bridge::handler::KeyAction::Consumed),
        "反引号是分隔符，不得被整句切换吃掉，实际 {act:?}"
    );
}
