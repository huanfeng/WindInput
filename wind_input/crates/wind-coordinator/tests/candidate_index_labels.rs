//! 候选序号默认标签：第 10 个写 `0`，与选它的物理键对应；用户 `index_labels` 仍优先。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use wind_bridge::handler::{CaretData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, caret_source};
use wind_ui_types::UiCommand;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn labels(index_labels: Vec<String>) -> Option<Vec<String>> {
    if !data_dir().join("schemas/pinyin.schema.toml").exists() {
        eprintln!("跳过：缺 build_dev 词库");
        return None;
    }
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.ui.candidate.per_page = 10;
    c.ui.candidate.index_labels = index_labels;
    let (coord, rx) = Coordinator::new_headless_with_ui(c, Some(&data_dir()));
    coord.handle_key_event(&KeyEventData {
        key_code: u32::from(b'S'),
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    });
    coord.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    let items = rx
        .try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates { candidates, .. } => Some(candidates),
            _ => None,
        })
        .last()
        .expect("应下发候选");
    assert!(
        items.len() >= 10,
        "前提：`s` 一页满 10 条，实得 {}",
        items.len()
    );
    Some(items.into_iter().map(|i| i.label).collect())
}

#[test]
fn tenth_default_label_is_zero() {
    let Some(l) = labels(Vec::new()) else { return };
    assert_eq!(l[0], "1");
    assert_eq!(l[8], "9");
    assert_eq!(l[9], "0", "第 10 个对应 `0` 键");
}

#[test]
fn user_index_label_overrides_tenth() {
    let user: Vec<String> = (1..=10).map(|n| format!("#{n}")).collect();
    let Some(l) = labels(user) else { return };
    assert_eq!(l[9], "#10", "用户配置优先于默认");
}
