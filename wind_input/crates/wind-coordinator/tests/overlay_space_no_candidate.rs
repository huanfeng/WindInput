//! 临拼 / 快捷输入剩余码**无候选**时按空格：与主路空码空格同一套语义。
//!
//! - 默认（`input.space_on_empty_behavior = "commit"`）：上屏「引导字母 + 已选段 + 剩余原码」。
//!   现场一：临拼 `` ` `` 后打 `nivvvvvvvv`、选「你」、按空格走的是 `ClearComposition` ⇒
//!   已选的「你」连同剩余码一起丢掉，统计也没记。
//!   现场二：码表方案 z 夺取进临拼 / 快捷输入（`z_key_action`），残余码无候选按空格，
//!   字母引导符 `z` 被吞——回车与切中英都经 `guide_to_return` 归还，空格这一臂漏了。
//! - `"clear"`：退出并 `ClearComposition`，连已选段一起丢（同主路）。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::stats::CommitSource;

const VK_BACKTICK: u32 = 0xC0;
const VK_SEMICOLON: u32 = 0xBA;
const VK_SPACE: u32 = 0x20;
const VK_NEXT: u32 = 0x22;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("schemas/pinyin.schema.toml").exists()
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

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 五笔主方案 + 拼音成员；快捷输入去掉英文成员（它恒给出「所打原文」候选，造不出无候选）。
fn cfg(edit: impl FnOnce(&mut Config)) -> Config {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.mix_modes[0].members.retain(|m| m != "english");
    edit(&mut c);
    c
}

fn open(edit: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
    Coordinator::new_headless(cfg(edit), Some(&data_dir()))
}

/// 真机出厂 `zz*` 短语的最小替身：装上后首键 z 是活码前缀、让位，第二键破前缀才夺取。
fn install_zz(coord: &Coordinator) {
    coord.debug_install_phrases(vec![wind_phrase::PhraseSeed {
        code: "zzbd".into(),
        text: "、".into(),
        weight: 0,
        position: 0,
        is_system: true,
        category: String::new(),
    }]);
}

/// 翻页找「你」并数字键选中（分步，留在组合区）。
fn pick_ni(coord: &Coordinator) {
    for _ in 0..40 {
        let t = coord.debug_page_texts();
        if let Some(p) = t.iter().position(|x| x == "你") {
            let act = coord.handle_key_event(&key(0x31 + p as u32));
            assert!(
                !matches!(act, KeyAction::InsertText { .. }),
                "前提：选「你」应留在组合区分步，实际: {act:?}"
            );
            return;
        }
        coord.handle_key_event(&key(VK_NEXT));
    }
    panic!("前提：候选里应有「你」");
}

fn assert_no_candidates(coord: &Coordinator) {
    assert!(
        coord.debug_page_texts().is_empty(),
        "前提：剩余码不应有候选，实际: {:?}",
        coord.debug_page_texts()
    );
}

/// 按空格，返回上屏文本（`ClearComposition` 为 `None`）。
fn space(coord: &Coordinator) -> Option<String> {
    match coord.handle_key_event(&key(VK_SPACE)) {
        KeyAction::InsertText { text, .. } => Some(text),
        KeyAction::ClearComposition => None,
        other => panic!("无候选按空格应上屏或清空，实际: {other:?}"),
    }
}

#[test]
fn temp_pinyin_space_commits_selected_segments_and_raw_tail() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let coord = open(|_| {});
    coord.handle_key_event(&key(VK_BACKTICK));
    assert!(coord.debug_in_temp_pinyin(), "前提：反引号进临拼");
    type_str(&coord, "nivvvvvvvv");
    pick_ni(&coord);
    assert_no_candidates(&coord);
    coord.debug_capture_stat_events();
    assert_eq!(
        space(&coord).as_deref(),
        Some("你vvvvvvvv"),
        "应上屏已选段 + 剩余原码"
    );
    // 已选段在选词时已记过，此处只记剩余原码（与快捷输入同一口径）。
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![(CommitSource::TempPinyin, "vvvvvvvv".to_string())],
        "剩余原码应按临拼来源记一笔统计"
    );
    assert!(!coord.debug_in_temp_pinyin(), "上屏后应退出临拼");
}

/// 无已选段、只剩无候选原码：默认配置上屏原码（符号引导符不归还）。
#[test]
fn temp_pinyin_space_commits_raw_code_without_segments() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let coord = open(|_| {});
    coord.handle_key_event(&key(VK_BACKTICK));
    type_str(&coord, "vvv");
    assert_no_candidates(&coord);
    assert_eq!(space(&coord).as_deref(), Some("vvv"));
    assert!(!coord.debug_in_temp_pinyin(), "上屏后应退出临拼");
}

#[test]
fn temp_pinyin_space_returns_letter_guide() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let coord = open(|c| c.schema.codetable.z_key_action = "temp_pinyin".into());
    install_zz(&coord);
    type_str(&coord, "zvvv");
    assert!(coord.debug_in_temp_pinyin(), "前提：`zv` 破前缀夺取进临拼");
    assert_no_candidates(&coord);
    coord.debug_capture_stat_events();
    assert_eq!(
        space(&coord).as_deref(),
        Some("zvvv"),
        "字母引导符 z 应归还（同回车）"
    );
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![(CommitSource::TempPinyin, "zvvv".to_string())],
        "统计记引导符 + 剩余原码"
    );
}

#[test]
fn quick_input_space_returns_letter_guide() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let coord = open(|c| c.schema.codetable.z_key_action = "mix:quick_mix".into());
    install_zz(&coord);
    type_str(&coord, "zvvv");
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`zv` 破前缀夺取进快捷输入"
    );
    assert_no_candidates(&coord);
    coord.debug_capture_stat_events();
    assert_eq!(
        space(&coord).as_deref(),
        Some("zvvv"),
        "字母引导符 z 应归还（同回车）"
    );
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![(CommitSource::Mix, "zvvv".to_string())],
        "统计记引导符 + 剩余原码"
    );
}

#[test]
fn temp_pinyin_space_clear_drops_everything() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let coord = open(|c| c.input.space_on_empty_behavior = "clear".into());
    coord.handle_key_event(&key(VK_BACKTICK));
    type_str(&coord, "nivvvvvvvv");
    pick_ni(&coord);
    assert_no_candidates(&coord);
    assert_eq!(space(&coord), None, "clear：连已选段一起丢（同主路）");
    assert!(!coord.debug_in_temp_pinyin(), "应退出临拼");
}

#[test]
fn quick_input_space_clear_drops_everything() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let coord = open(|c| c.input.space_on_empty_behavior = "clear".into());
    coord.handle_key_event(&key(VK_SEMICOLON));
    type_str(&coord, "nivvvvvvvv");
    pick_ni(&coord);
    assert_no_candidates(&coord);
    assert_eq!(space(&coord), None, "clear：连已选段一起丢（同主路）");
    assert_eq!(coord.debug_active_mode(), None, "应退出快捷输入");
}
