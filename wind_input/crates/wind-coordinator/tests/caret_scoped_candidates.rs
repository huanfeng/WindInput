//! `input.caret_scoped_candidates` 端到端：光标在编码中间时，候选只按光标前缀查询；
//! 选词只上屏前缀，余码留在编码栏。默认关闭；开启后仅普通拼音/双拼生效。
//!
//! 期望行为：
//! - `input.caret_scoped_candidates`默认 false；开关关时移光标不重算候选，开时按前缀重算，两个实例互不影响；
//! - 光标严格在中间字符边界时，候选与「只输入前缀」一致；Home/End 回到两端恢复整串；
//! - 裸 ←、Ctrl+←、caret_left/right、syllable_prev/next、toggle_tip 都触发重算；
//! - 双拼允许停在键对中间。例如 `vsg|o` 按 `vsg` 查，选词后剩 `o`；
//! - 整句 N-best 池切换后，池候选的消费长度夹到前缀、编码栏保留尾段，选词不吞尾码；
//! - 翻页扩容后仍只查前缀，不退回整串。
//!
//! 走 headless Coordinator 的 `handle_key_event`，用 `debug_*` 观察候选、缓冲与光标；
//! 不测 UI，也不覆盖 overlay、码表/混输、配置热重载。依赖 `build_dev/data` 的真实
//! 拼音词库；缺失时整族静默跳过（`--nocapture` 可见「跳过」），不能把 0 失败当成已覆盖。

use std::{path::PathBuf, sync::Arc};
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL, MOD_SHIFT};
use wind_keys::keymap::{VK_END, VK_HOME, VK_LEFT, VK_NEXT, VK_RIGHT, VK_SPACE, VK_TAB};

/// 创建指定开关与方案的配置。
fn config(enabled: bool, schema: &str) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec![schema.into()];
    cfg.schema.active = schema.into();
    cfg.input.default.chinese_mode = true;
    cfg.input.caret_scoped_candidates = enabled;
    cfg
}

/// 有真实词库时创建无头协调器，否则打印跳过原因。
fn coord(cfg: Config) -> Option<Arc<Coordinator>> {
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/pinyin/rime_frost.dict.yaml").exists() {
        eprintln!("跳过：缺少 build_dev/data 的真实拼音词库");
        return None;
    }
    Some(Coordinator::new_headless(cfg, Some(&data)))
}

/// 派发一次带修饰位的按键。
fn tap(c: &Coordinator, key_code: u32, modifiers: u32) -> KeyAction {
    c.handle_key_event(&KeyEventData {
        key_code,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        scan_code: 0,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    })
}

/// 输入 ASCII 编码。
fn type_code(c: &Coordinator, code: &str) {
    for ch in code.bytes() {
        tap(c, u32::from(ch.to_ascii_uppercase()), 0);
    }
}

/// 经 Home 与 Shift+右键把光标精确移到原始编码位置。
fn move_to(c: &Coordinator, pos: usize) {
    tap(c, VK_HOME, 0);
    for _ in 0..pos {
        tap(c, VK_RIGHT, MOD_SHIFT);
    }
}

#[test]
fn default_and_instances_are_independent() {
    assert!(!Config::default().input.caret_scoped_candidates);
    let Some(off) = coord(config(false, "pinyin")) else {
        return;
    };
    let Some(on) = coord(config(true, "pinyin")) else {
        return;
    };
    type_code(&off, "fangxiang");
    type_code(&on, "fangxiang");
    let full = off.debug_candidate_triples();
    assert!(!full.is_empty());
    tap(&off, VK_LEFT, 0);
    tap(&on, VK_LEFT, 0);
    assert_eq!(off.debug_candidate_triples(), full);
    assert_ne!(on.debug_candidate_triples(), full);
    assert!(
        on.debug_candidate_consumed()
            .iter()
            .all(|(_, n, _)| *n > 0 && *n <= 4)
    );
}

#[test]
fn prefix_selection_keeps_tail_and_home_end_restore_full_query() {
    let Some(c) = coord(config(true, "pinyin")) else {
        return;
    };
    let Some(prefix) = coord(config(false, "pinyin")) else {
        return;
    };
    type_code(&prefix, "fang");
    type_code(&c, "fangxiang");
    let full = c.debug_candidate_triples();
    assert!(!full.is_empty(), "前提：整串 fangxiang 应有候选");
    tap(&c, VK_LEFT, 0);
    assert_eq!(
        c.debug_candidate_triples(),
        prefix.debug_candidate_triples()
    );
    tap(&c, VK_HOME, 0);
    assert_eq!(c.debug_candidate_triples(), full);
    tap(&c, VK_END, 0);
    assert_eq!(c.debug_candidate_triples(), full);
    tap(&c, VK_LEFT, 0);
    let page = c.debug_page_texts();
    assert!(!page.is_empty(), "前提：前缀 fang 应有候选");
    let selected = page[0].clone();
    assert!(matches!(
        tap(&c, VK_SPACE, 0),
        KeyAction::UpdateComposition { .. }
    ));
    assert_eq!(c.debug_input_buffer(), "xiang");
    assert!(c.debug_preedit().starts_with(&selected));
    assert_eq!(c.debug_ui_caret_bytes(), c.debug_preedit().len());
}

#[test]
fn all_main_movement_entries_refresh_prefix_candidates() {
    let Some(prefix) = coord(config(false, "pinyin")) else {
        return;
    };
    type_code(&prefix, "fang");
    let expected = prefix.debug_candidate_triples();
    assert!(!expected.is_empty(), "前提：fang 应有候选");
    for (action, right, steps) in [
        ("", false, 1),
        ("ctrl", false, 1),
        ("caret_left", false, 5),
        ("caret_right", true, 4),
        ("syllable_prev", false, 1),
        ("syllable_next", true, 1),
        ("toggle_tip", true, 4),
    ] {
        let mut cfg = config(true, "pinyin");
        let vk = if right { VK_RIGHT } else { VK_LEFT };
        if !action.is_empty() && action != "ctrl" {
            cfg.keys
                .session_actions
                .insert(if right { "right" } else { "left" }.into(), action.into());
        }
        let Some(c) = coord(cfg) else {
            return;
        };
        type_code(&c, "fangxiang");
        if right {
            tap(&c, VK_HOME, 0);
        }
        for _ in 0..steps {
            tap(&c, vk, if action == "ctrl" { MOD_CTRL } else { 0 });
        }
        assert_eq!(c.debug_candidate_triples(), expected, "入口 {action}");
        if action.is_empty() {
            tap(&c, VK_RIGHT, 0);
            assert!(c.debug_candidate_consumed().iter().any(|(_, n, _)| *n > 4));
        }
    }
}

#[test]
fn shuangpin_mid_pair_queries_prefix_and_keeps_last_key() {
    let Some(c) = coord(config(true, "shuangpin")) else {
        return;
    };
    let Some(prefix) = coord(config(false, "shuangpin")) else {
        return;
    };
    type_code(&prefix, "vsg");
    type_code(&c, "vsgo");
    let expected = prefix.debug_candidate_triples();
    assert!(!expected.is_empty(), "前提：vsg 应有候选");
    move_to(&c, 3);
    assert_eq!(c.debug_candidate_triples(), expected);
    let consumed = c.debug_candidate_consumed();
    assert!(!consumed.is_empty(), "前提：vsgo 的前缀应有候选");
    assert_eq!(consumed[0].1, 3);
    assert!(matches!(
        tap(&c, VK_SPACE, 0),
        KeyAction::UpdateComposition { .. }
    ));
    assert_eq!(c.debug_input_buffer(), "o");
    assert_eq!(c.debug_ui_caret_bytes(), c.debug_preedit().len());
}

#[test]
fn sentence_cycle_preserves_prefix_consumption_and_full_preedit() {
    let mut cfg = config(true, "pinyin");
    cfg.schema.pinyin.sentence_max_count = 3;
    cfg.schema.pinyin.sentence_count = 1;
    cfg.schema.pinyin.sentence_cycle_key = "tab".into();
    let Some(c) = coord(cfg) else {
        return;
    };
    type_code(&c, "yougailunmahao");
    move_to(&c, "yougailunma".len());
    let before = c.debug_page_texts();
    assert!(!before.is_empty(), "前提：整句样例应有候选");
    let first = before[0].clone();
    tap(&c, VK_TAB, 0);
    let after = c.debug_page_texts();
    assert!(!after.is_empty(), "前提：切换后仍有候选");
    assert_ne!(after[0], first, "前提：确实切换了 N-best 整句");
    assert!(c.debug_preedit().ends_with("hao"), "{}", c.debug_preedit());
    let consumed = c.debug_candidate_consumed();
    assert!(!consumed.is_empty(), "前提：切换后的整句候选应有消费长度");
    assert_eq!(consumed[0].1, "yougailunma".len());
    assert!(matches!(
        tap(&c, VK_SPACE, 0),
        KeyAction::UpdateComposition { .. }
    ));
    assert_eq!(c.debug_input_buffer(), "hao");
}

#[test]
fn paging_expansion_keeps_cut() {
    let Some(c) = coord(config(true, "pinyin")) else {
        return;
    };
    type_code(&c, "fangxiang");
    tap(&c, VK_LEFT, 0);
    let per_page = c.debug_per_page(false);
    assert!(per_page > 0, "前提：每页候选数应为正数");
    let before = c.debug_candidate_consumed().len();
    assert!(before > 0, "前提：前缀 fang 应有候选");
    // 扩容由末页再按下一页触发；此时 has_more 未必为真，按实际页大小翻到末页。
    for _ in 0..before.div_ceil(per_page) {
        tap(&c, VK_NEXT, 0);
    }
    let after = c.debug_candidate_consumed();
    assert!(after.len() > before, "前提：翻页实际扩容了候选池");
    assert!(after.iter().all(|(_, n, _)| *n > 0 && *n <= 4));
}
