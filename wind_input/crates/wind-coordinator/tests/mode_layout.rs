//! 模式级候选布局的**接线**端到端测试。
//!
//! 决策矩阵（模式 × 意图 × 基线）的单测在 `src/layout.rs`；这里只验真实按键流程下
//! 覆盖是否生效、退出后是否算回基线。设计见 `docs/design/mode-candidate-layout.md`。
//!
//! 刻意选临时英文：Shift+字母进入，不依赖任何方案/词典，因此**不需要 build_dev/data**，
//! 不会像 `has_schemas()` 守卫的测试族那样在缺数据时静默跳过（判据：耗时 0.00s）。

use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::{Config, LayoutIntent};
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, FocusLostReason, MOD_SHIFT};

const VK_ESCAPE: u32 = 0x1B;

/// 基线竖排 + 临英横排：**旧的 `force_vertical: bool` 表达不了这一格**，
/// 三态改造的全部增量都在这里。
fn cfg() -> Config {
    let mut c = Config::default();
    c.input.default.chinese_mode = true;
    c.ui.candidate.layout = "vertical".into();
    c.input.temp_english.candidate_layout = LayoutIntent::Horizontal;
    c
}

fn key(vk: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// Shift+字母进入临时英文，返回进入时的 KeyAction 供调用方断言「真的进去了」。
fn enter_temp_english(coord: &Coordinator) -> KeyAction {
    coord.handle_key_event(&key(u32::from(b'A'), MOD_SHIFT))
}

#[test]
fn temp_english_overrides_vertical_baseline_then_restores() {
    let coord = Coordinator::new_headless(cfg(), None);
    assert!(
        coord.debug_desired_vertical(),
        "无模式时应跟随基线（本用例基线是竖排）"
    );

    // ★ 先证明真的进了临英再断言布局。少了这一步，「压根没进模式」与「进了但覆盖没生效」
    //   在后面的断言上无法区分，测试会静默退化成从不执行被测分支的假绿。
    let action = enter_temp_english(&coord);
    assert!(
        matches!(&action, KeyAction::UpdateComposition { text, .. } if text == "A"),
        "Shift+A 应进入临时英文并把 A 写进组合区，实际: {action:?}"
    );
    assert!(
        !coord.debug_desired_vertical(),
        "临英期间应按模式意图横排（覆盖竖排基线）"
    );

    // Esc 退出 → 无模式 → 算回基线。这一步走的是正常退出路径。
    coord.handle_key_event(&key(VK_ESCAPE, 0));
    assert!(
        coord.debug_desired_vertical(),
        "退出临英后应算回全局基线（竖排）"
    );
}

/// 自愈：**不走**任何模式自己的退出路径，直接用失焦复位，布局仍应算回基线。
///
/// 这是声明式重算相对「进入时保存 / 退出时回放」的核心收益——旧实现要求每条退出路径
/// 都手写一遍恢复（`state.active` 有 8 个清空点），漏一处就把候选窗卡在错误方向且无日志。
#[test]
fn layout_self_heals_when_mode_cleared_without_its_exit_path() {
    let coord = Coordinator::new_headless(cfg(), None);
    let action = enter_temp_english(&coord);
    assert!(
        matches!(&action, KeyAction::UpdateComposition { text, .. } if text == "A"),
        "前置条件：应已进入临时英文，实际: {action:?}"
    );
    assert!(!coord.debug_desired_vertical(), "前置条件：此刻应是横排");

    // 失焦复位（reset_exclusive_modes）——它不再包含任何布局恢复代码。
    // 用 Thread（整个应用失去前台）：CtxLost 刻意不清输入态，拿它测等于没触发被测路径。
    coord.handle_focus_lost(0, FocusLostReason::Thread);
    assert!(
        coord.debug_desired_vertical(),
        "失焦清空模式后，布局应自动算回基线，无需退出路径显式恢复"
    );
}

/// 模式意图为 Follow 时不做任何覆盖：基线是什么就是什么。
/// 守的是「Follow 不等于 Horizontal」——迁移把旧 `force_vertical = false` 映射成 Follow，
/// 若误映射成 Horizontal，全局竖排的存量用户会被强行钉在横排。
#[test]
fn follow_intent_leaves_baseline_untouched() {
    let mut c = cfg();
    c.input.temp_english.candidate_layout = LayoutIntent::Follow;
    let coord = Coordinator::new_headless(c, None);

    let action = enter_temp_english(&coord);
    assert!(
        matches!(&action, KeyAction::UpdateComposition { text, .. } if text == "A"),
        "前置条件：应已进入临时英文，实际: {action:?}"
    );
    assert!(
        coord.debug_desired_vertical(),
        "Follow 应保持基线竖排，而不是被当成横排"
    );
}
