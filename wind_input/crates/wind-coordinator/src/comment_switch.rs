//! 组字中单击切换键（`ui.comment.switch_key`）切换注释态（`ui.comment.switch_mode`）的判定。
//!
//! - mac / Linux 的 Alt：平台层只在组字中转发 Alt 的按下与松开，不做计时；吃不吃由这里的 [`SwitchOutcome`] 定。
//! - Windows 的 Alt：TSF 收不到单独 Alt 的按下，改由服务进程的全局低级键盘钩子判单击 / 按住
//!   （`wind_keys::alt_tap`），这里只把它投来的事件折成动作，见 [`on_hook_alt`]。
//! - Ctrl：平台只送判过单击的松开（TSF 走 keyup 白名单，mac / Linux 无条件送），见 [`on_ctrl_up`]。
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

/// Ctrl 单击的松开（平台已判过单击）。按住档在调用前已由 `effective_mode` 降为单击档。
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
/// `mode` 是已按 [`wind_config::comment_lib::effective_mode`] 折算、注释总开关也算进去的那个。
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
mod tests {
    use super::*;
    use SwitchEffect as E;
    use SwitchOutcome as O;

    fn tap(mode: SwitchMode, held_ms: u64) -> [(SwitchOutcome, SwitchEffect); 2] {
        let t0 = Instant::now();
        let mut track = None;
        [
            on_alt_down(mode, true, &mut track, t0),
            on_alt_up(mode, true, &mut track, t0 + Duration::from_millis(held_ms)),
        ]
    }

    #[test]
    fn tap_toggles_session() {
        assert_eq!(
            tap(SwitchMode::Session, 100),
            [(O::PassThrough, E::None), (O::Consumed, E::ToggleSession)]
        );
    }

    #[test]
    fn long_press_is_not_tap() {
        assert_eq!(tap(SwitchMode::Session, 600)[1], (O::PassThrough, E::None));
    }

    #[test]
    fn combo_is_not_tap() {
        let t0 = Instant::now();
        let mut track = None;
        on_alt_down(SwitchMode::Session, true, &mut track, t0);
        on_other_key(&mut track);
        let up = on_alt_up(
            SwitchMode::Session,
            true,
            &mut track,
            t0 + Duration::from_millis(50),
        );
        assert_eq!(up, (O::PassThrough, E::None));
        assert!(track.is_none(), "松开后清掉 track");
    }

    #[test]
    fn no_candidates_passes_through() {
        for mode in [SwitchMode::Session, SwitchMode::Persist, SwitchMode::Hold] {
            let t0 = Instant::now();
            let mut track = None;
            assert_eq!(
                on_alt_down(mode, false, &mut track, t0),
                (O::PassThrough, E::None)
            );
            assert_eq!(
                on_alt_up(mode, false, &mut track, t0),
                (O::PassThrough, E::None)
            );
        }
        // 按下时有候选、松开时没了（Session 档）：放行。
        let t0 = Instant::now();
        let mut track = None;
        on_alt_down(SwitchMode::Session, true, &mut track, t0);
        assert_eq!(
            on_alt_up(SwitchMode::Session, false, &mut track, t0),
            (O::PassThrough, E::None)
        );
    }

    #[test]
    fn off_mode_passes_through() {
        assert_eq!(
            tap(SwitchMode::Off, 100),
            [(O::PassThrough, E::None), (O::PassThrough, E::None)]
        );
    }

    #[test]
    fn persist_mode_toggles_persist() {
        assert_eq!(
            tap(SwitchMode::Persist, 100)[1],
            (O::Consumed, E::TogglePersist)
        );
    }

    #[test]
    fn hold_on_off() {
        assert_eq!(
            tap(SwitchMode::Hold, 2000),
            [(O::Consumed, E::HoldOn), (O::Consumed, E::HoldOff)]
        );
        // 期间按过别的键、松开时已无候选：松开仍吃。
        let t0 = Instant::now();
        let mut track = None;
        on_alt_down(SwitchMode::Hold, true, &mut track, t0);
        on_other_key(&mut track);
        assert_eq!(
            on_alt_up(SwitchMode::Hold, false, &mut track, t0),
            (O::Consumed, E::HoldOff)
        );
    }

    #[test]
    fn other_modifier_bits_mark_combo() {
        use wind_ipc::protocol::{
            MOD_ALT, MOD_CTRL, MOD_LCTRL, MOD_LSHIFT, MOD_RCTRL, MOD_SHIFT, MOD_WIN,
        };
        for alt in [0xA4, 0xA5] {
            assert!(!carries_other_modifier(alt, 0));
            assert!(!carries_other_modifier(alt, MOD_ALT));
            for m in [MOD_SHIFT, MOD_CTRL, MOD_WIN, MOD_LSHIFT] {
                assert!(carries_other_modifier(alt, m | MOD_ALT), "{m:#x}");
            }
        }
        // Ctrl 松开自带的 Ctrl 位不算组合键。
        assert!(!carries_other_modifier(0xA2, MOD_CTRL | MOD_LCTRL));
        assert!(!carries_other_modifier(0xA3, MOD_CTRL | MOD_RCTRL));
        assert!(!carries_other_modifier(0xA2, 0));
        for m in [MOD_SHIFT, MOD_ALT, MOD_WIN, MOD_LSHIFT] {
            assert!(carries_other_modifier(0xA2, m | MOD_CTRL), "{m:#x}");
        }
    }

    #[test]
    fn ctrl_up_toggles_by_mode() {
        assert_eq!(
            on_ctrl_up(SwitchMode::Session, true, false),
            E::ToggleSession
        );
        assert_eq!(
            on_ctrl_up(SwitchMode::Persist, true, false),
            E::TogglePersist
        );
        assert_eq!(on_ctrl_up(SwitchMode::Off, true, false), E::None);
        assert_eq!(on_ctrl_up(SwitchMode::Session, false, false), E::None);
        assert_eq!(on_ctrl_up(SwitchMode::Session, true, true), E::None);
    }

    #[test]
    fn hook_alt_events_map_by_mode() {
        use wind_keys::alt_tap::AltEvent as A;
        assert_eq!(
            on_hook_alt(SwitchMode::Session, true, A::Tap),
            E::ToggleSession
        );
        assert_eq!(
            on_hook_alt(SwitchMode::Persist, true, A::Tap),
            E::TogglePersist
        );
        assert_eq!(on_hook_alt(SwitchMode::Hold, true, A::Tap), E::None);
        assert_eq!(on_hook_alt(SwitchMode::Off, true, A::Tap), E::None);
        assert_eq!(on_hook_alt(SwitchMode::Hold, true, A::HoldDown), E::HoldOn);
        assert_eq!(on_hook_alt(SwitchMode::Session, true, A::HoldDown), E::None);
        assert_eq!(on_hook_alt(SwitchMode::Hold, true, A::HoldUp), E::HoldOff);
    }

    /// 单击 / 按下投到时候选窗已关：不做；按住的松开照样收起。
    #[test]
    fn hook_alt_after_window_closed() {
        use wind_keys::alt_tap::AltEvent as A;
        assert_eq!(on_hook_alt(SwitchMode::Session, false, A::Tap), E::None);
        assert_eq!(on_hook_alt(SwitchMode::Persist, false, A::Tap), E::None);
        assert_eq!(on_hook_alt(SwitchMode::Hold, false, A::HoldDown), E::None);
        assert_eq!(on_hook_alt(SwitchMode::Hold, false, A::HoldUp), E::HoldOff);
        assert_eq!(on_hook_alt(SwitchMode::Off, false, A::HoldUp), E::HoldOff);
    }

    #[test]
    fn alt_gate_mask_by_key_and_mode() {
        use wind_config::comment_lib::SwitchKey as K;
        use wind_keys::alt_tap::{GATE_HOLD as H, GATE_LEFT as L, GATE_RIGHT as R};
        assert_eq!(alt_gate_mask(K::Alt, SwitchMode::Session), L | R);
        assert_eq!(alt_gate_mask(K::LAlt, SwitchMode::Persist), L);
        assert_eq!(alt_gate_mask(K::RAlt, SwitchMode::Hold), R | H);
        assert_eq!(alt_gate_mask(K::Alt, SwitchMode::Off), 0);
        for k in [K::Ctrl, K::LCtrl, K::RCtrl] {
            assert_eq!(alt_gate_mask(k, SwitchMode::Session), 0, "{k:?}");
        }
    }

    #[test]
    fn stale_track_is_dropped_when_not_applicable() {
        let t0 = Instant::now();
        let mut track = None;
        on_alt_down(SwitchMode::Hold, true, &mut track, t0);
        // 松开没送到；下一次按下时已无候选。
        on_alt_down(SwitchMode::Hold, false, &mut track, t0);
        assert!(track.is_none());
        assert_eq!(
            on_alt_up(SwitchMode::Hold, false, &mut track, t0),
            (O::PassThrough, E::None),
            "无候选时的松开不该因残留被吃"
        );
    }

    #[test]
    fn dirty_track_restarts_on_fresh_down() {
        let t0 = Instant::now();
        let mut track = None;
        on_alt_down(SwitchMode::Session, true, &mut track, t0);
        on_other_key(&mut track);
        // 组合键之后松开没送到，2 秒后重新单击。
        let t1 = t0 + Duration::from_secs(2);
        on_alt_down(SwitchMode::Session, true, &mut track, t1);
        assert_eq!(
            on_alt_up(
                SwitchMode::Session,
                true,
                &mut track,
                t1 + Duration::from_millis(100)
            ),
            (O::Consumed, E::ToggleSession)
        );
    }

    #[test]
    fn repeat_down_keeps_first_press_time() {
        let t0 = Instant::now();
        let mut track = None;
        on_alt_down(SwitchMode::Session, true, &mut track, t0);
        on_alt_down(
            SwitchMode::Session,
            true,
            &mut track,
            t0 + Duration::from_millis(400),
        );
        let up = on_alt_up(
            SwitchMode::Session,
            true,
            &mut track,
            t0 + Duration::from_millis(600),
        );
        assert_eq!(up, (O::PassThrough, E::None), "连发不重置计时");
    }
}
