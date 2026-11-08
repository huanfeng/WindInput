//! 生僻字模式上屏要进**上屏历史**（`quick_input.repeat` / z 重复上屏的事实源）。
//!
//! 现场：`commit_special_candidate` 对生僻字模式整个跳过 `record_selection_in`——跳过词频
//! 是用户拍板的，但上屏历史也挂在那条通路上一并跳过了（注释自己写着历史不在此列）
//! ⇒ 刚打出的生僻字 `;` 重复上屏取不到。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

const VK_SEMICOLON: u32 = 0xBA;
const VK_SPACE: u32 = 0x20;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english"]
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

#[test]
fn rare_char_commit_enters_commit_history() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.codetable.z_key_action = "rare_char".into();
    // headless 无 store ⇒ 短语层空、`z` 不是活码前缀，首键直接进生僻字模式。
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    coord.handle_key_event(&key(u32::from(b'Z')));
    assert_eq!(
        coord.debug_active_mode(),
        Some("rare_char"),
        "前提：z 进生僻字模式"
    );
    coord.handle_key_event(&key(u32::from(b'G')));
    coord.handle_key_event(&key(u32::from(b'G')));
    let first = coord
        .debug_page_texts()
        .first()
        .cloned()
        .expect("前提：生僻字模式下 `gg` 应有候选");
    match coord.handle_key_event(&key(VK_SPACE)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, first, "前提：空格上屏首选"),
        other => panic!("前提：空格应上屏生僻字，实际: {other:?}"),
    }
    // 再进快捷输入、空缓冲：`quick_input.repeat` 取的正是上屏历史的最新一条。
    coord.handle_key_event(&key(VK_SEMICOLON));
    assert_eq!(
        coord.debug_page_texts().first(),
        Some(&first),
        "生僻字上屏应进上屏历史（重复上屏取得到）"
    );
}
