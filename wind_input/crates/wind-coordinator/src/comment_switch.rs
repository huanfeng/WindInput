//! 组字中单击切换键（`ui.comment.switch_key`）切换注释态（`ui.comment.switch_mode`）的判定。
//!
//! - mac / Linux 的 Alt：平台层只在组字中转发 Alt 的按下与松开，不做计时；吃不吃由这里的 [`SwitchOutcome`] 定。
//! - Windows 的 Alt：TSF 收不到单独 Alt 的按下，改由服务进程的全局低级键盘钩子判单击 / 按住
//!   （`wind_keys::alt_tap`），这里只把它投来的事件折成动作，见 [`on_hook_alt`]。
//! - Ctrl：按下那一刻有输入会话，平台就如实上报这次按下与松开（`EVENT_MOD_DOWN` / `EVENT_MOD_UP`），
//!   单击由这里判（[`on_ctrl_raw_down`] / [`on_ctrl_raw_up`]），判出单击再按原「确认过的松开」交给
//!   中英切换、选词、会话绑定等原有消费者；没有会话时平台照旧只送判过单击的松开，见 [`on_ctrl_up`]。
//!
//! 判定是纯函数，时刻由调用方传入，便于测出长按、组合键等分支。

use crate::coordinator::AltTrack;
use std::time::{Duration, Instant};
use wind_config::comment_lib::SwitchMode;

/// 单击的最长按住时长，与 TSF 的 `TOGGLE_TAP_THRESHOLD_MS` 一致。
pub(crate) const ALT_TAP_MAX: Duration = Duration::from_millis(500);

/// 这次 Alt 事件宿主看不看得到。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwitchOutcome {
    PassThrough,
    Consumed,
}

/// 判定出的注释态动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwitchEffect {
    None,
    ToggleSession,
    TogglePersist,
    HoldOn,
    HoldOff,
}

/// 切换键事件是否带了别的修饰位，带了就不是单击。
///
/// Alt：平台把这次按住期间按过的 Shift / Ctrl / Win 粘在 Alt 事件上（桌面 TSF 不转发这几个键的
/// 按下）：Alt+Shift 是系统切换输入语言的热键。Ctrl：TSF 送来的松开自带 Ctrl 位，那是它自己的，
/// 只看 Shift / Alt / Win。
pub(crate) fn carries_other_modifier(vk: u32, modifiers: u32) -> bool {
    use wind_ipc::protocol::{
        MOD_ALT, MOD_CTRL, MOD_LCTRL, MOD_LSHIFT, MOD_RCTRL, MOD_RSHIFT, MOD_SHIFT, MOD_WIN,
    };
    let shift_win = MOD_SHIFT | MOD_WIN | MOD_LSHIFT | MOD_RSHIFT;
    let others = if is_ctrl_vk(vk) {
        shift_win | MOD_ALT
    } else {
        shift_win | MOD_CTRL | MOD_LCTRL | MOD_RCTRL
    };
    modifiers & others != 0
}

/// 左 / 右 Ctrl（0xA2 / 0xA3）。
pub(crate) fn is_ctrl_vk(vk: u32) -> bool {
    matches!(vk, 0xA2 | 0xA3)
}

/// Ctrl 单击的松开（平台已判过单击，或 [`on_ctrl_raw_up`] 判出的单击）。按住档在按下 / 松开时已处理，这里不动。
pub(crate) fn on_ctrl_up(mode: SwitchMode, has_candidates: bool, combo: bool) -> SwitchEffect {
    if !has_candidates || combo {
        return SwitchEffect::None;
    }
    match mode {
        SwitchMode::Session => SwitchEffect::ToggleSession,
        SwitchMode::Persist => SwitchEffect::TogglePersist,
        SwitchMode::Off | SwitchMode::Hold => SwitchEffect::None,
    }
}

/// 如实上报的 Ctrl 自按下起的追踪：哪一侧、按下时刻、期间是否按过别的键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CtrlTrack {
    pub vk: u32,
    pub down_at: Instant,
    pub dirty: bool,
}

/// 如实上报的 Ctrl 按下：一律重新起算。平台对同一次按住只发一次按下（连发 / 重复回调在平台层
/// 就滤掉了），再来一次按下只能是上次松开丢了（Ctrl+Alt+Del、Ctrl+点击切走），沿用旧 track
/// 只会把这次拖成长按或组合键。另一侧 Ctrl 还按着时换成这一侧，那一侧的松开就找不到 track，
/// 不算单击。`combo` = 按下时已带别的修饰位（Shift / Alt / Win）。
pub(crate) fn on_ctrl_raw_down(track: &mut Option<CtrlTrack>, vk: u32, combo: bool, now: Instant) {
    *track = Some(CtrlTrack {
        vk,
        down_at: now,
        dirty: combo,
    });
}

/// 如实上报的 Ctrl 松开：是不是单击。干净（期间没有别的键、平台也没报 `not_tap`）、同侧且按住
/// 不超过 [`ALT_TAP_MAX`] 才算；没有 track（按下没送到）不算。无论结果都清掉 track。
pub(crate) fn on_ctrl_raw_up(
    track: &mut Option<CtrlTrack>,
    vk: u32,
    not_tap: bool,
    now: Instant,
) -> bool {
    let Some(t) = track.take() else {
        return false;
    };
    t.vk == vk && !t.dirty && !not_tap && now.saturating_duration_since(t.down_at) <= ALT_TAP_MAX
}

/// 按住 Ctrl 期间来了别的键（含 `WM_HOTKEY` 送来的 Ctrl+数字）：之后的松开不算单击。
pub(crate) fn on_other_ctrl_key(track: &mut Option<CtrlTrack>) {
    if let Some(t) = track {
        t.dirty = true;
    }
}

/// 按住档的 Ctrl：按下立即翻转（要有候选），松开一律收起（哪怕期间上屏关了窗、按得太久、
/// 中途改了档位），别让翻转卡住。切换键不是这一侧的 Ctrl → 不动。
pub(crate) fn ctrl_hold_effect(
    mode: SwitchMode,
    key_matches: bool,
    has_candidates: bool,
    is_down: bool,
) -> SwitchEffect {
    if !key_matches {
        return SwitchEffect::None;
    }
    if !is_down {
        return SwitchEffect::HoldOff;
    }
    if mode == SwitchMode::Hold && has_candidates {
        SwitchEffect::HoldOn
    } else {
        SwitchEffect::None
    }
}

/// Alt 按下。不适用时清掉 track（上次松开没送到时留下的残留）。
///
/// 已有且没弄脏的 track 视作同一次按住的自动连发，保留首次按下的时刻；弄脏过的是组合键之后
/// 的残留（自动连发不会在别的键之后接着发 Alt），按新的一次按下算。
pub(crate) fn on_alt_down(
    mode: SwitchMode,
    has_candidates: bool,
    track: &mut Option<AltTrack>,
    now: Instant,
) -> (SwitchOutcome, SwitchEffect) {
    if mode == SwitchMode::Off || !has_candidates {
        *track = None;
        return (SwitchOutcome::PassThrough, SwitchEffect::None);
    }
    if !track.as_ref().is_some_and(|t| !t.dirty) {
        *track = Some(AltTrack {
            down_at: now,
            dirty: false,
        });
    }
    match mode {
        SwitchMode::Hold => (SwitchOutcome::Consumed, SwitchEffect::HoldOn),
        _ => (SwitchOutcome::PassThrough, SwitchEffect::None),
    }
}

/// Alt 松开。无论结果如何都清掉 track。
pub(crate) fn on_alt_up(
    mode: SwitchMode,
    has_candidates: bool,
    track: &mut Option<AltTrack>,
    now: Instant,
) -> (SwitchOutcome, SwitchEffect) {
    let Some(t) = track.take() else {
        return (SwitchOutcome::PassThrough, SwitchEffect::None);
    };
    // 按下吃了，松开也吃：按住期间上屏关了窗，松开时已无候选。
    if mode == SwitchMode::Hold {
        return (SwitchOutcome::Consumed, SwitchEffect::HoldOff);
    }
    let tap = !t.dirty && now.saturating_duration_since(t.down_at) <= ALT_TAP_MAX;
    if !has_candidates || !tap {
        return (SwitchOutcome::PassThrough, SwitchEffect::None);
    }
    match mode {
        SwitchMode::Session => (SwitchOutcome::Consumed, SwitchEffect::ToggleSession),
        SwitchMode::Persist => (SwitchOutcome::Consumed, SwitchEffect::TogglePersist),
        _ => (SwitchOutcome::PassThrough, SwitchEffect::None),
    }
}

/// Windows Alt 钩子投来的事件折成动作。事件与处理之间候选窗可能已关：单击与按下就不做，
/// 按住的松开照样收起（别让按住翻转卡住）。
pub(crate) fn on_hook_alt(
    mode: SwitchMode,
    has_candidates: bool,
    ev: wind_keys::alt_tap::AltEvent,
) -> SwitchEffect {
    use wind_keys::alt_tap::AltEvent;
    match ev {
        AltEvent::Tap if has_candidates => match mode {
            SwitchMode::Session => SwitchEffect::ToggleSession,
            SwitchMode::Persist => SwitchEffect::TogglePersist,
            SwitchMode::Off | SwitchMode::Hold => SwitchEffect::None,
        },
        AltEvent::HoldDown if has_candidates && mode == SwitchMode::Hold => SwitchEffect::HoldOn,
        AltEvent::HoldUp => SwitchEffect::HoldOff,
        _ => SwitchEffect::None,
    }
}

/// 钩子的 Alt 闸门值（`wind_keys::alt_tap::GATE_*`）：切换键不是 Alt 系或已关 → 0。
/// `mode` 是注释总开关也算进去的那个（关了即 `Off`）。
pub(crate) fn alt_gate_mask(key: wind_config::comment_lib::SwitchKey, mode: SwitchMode) -> u32 {
    use wind_keys::alt_tap::{GATE_HOLD, GATE_LEFT, GATE_RIGHT};
    if mode == SwitchMode::Off || !key.is_alt() {
        return 0;
    }
    let mut mask = 0;
    if key.matches_vk(0xA4) {
        mask |= GATE_LEFT;
    }
    if key.matches_vk(0xA5) {
        mask |= GATE_RIGHT;
    }
    if mode == SwitchMode::Hold {
        mask |= GATE_HOLD;
    }
    mask
}

/// 按住 Alt 期间来了别的键：之后的松开不算单击。
pub(crate) fn on_other_key(track: &mut Option<AltTrack>) {
    if let Some(t) = track {
        t.dirty = true;
    }
}

#[cfg(test)]
#[path = "comment_switch_tests.rs"]
mod tests;
