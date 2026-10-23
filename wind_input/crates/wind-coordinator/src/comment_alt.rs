//! 组字中 Alt 单击 / 按住切换注释态（`ui.comment.switch_mode`）的判定。
//!
//! 平台层只在组字中转发 Alt 的按下与松开，不做计时；吃不吃由这里的 [`AltOutcome`] 定。
//! 判定是纯函数，时刻由调用方传入，便于测出长按、组合键等分支。

use crate::coordinator::AltTrack;
use std::time::{Duration, Instant};
use wind_config::comment_lib::SwitchMode;

/// 单击的最长按住时长，与 TSF 的 `TOGGLE_TAP_THRESHOLD_MS` 一致。
pub(crate) const ALT_TAP_MAX: Duration = Duration::from_millis(500);

/// 这次 Alt 事件宿主看不看得到。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AltOutcome {
    PassThrough,
    Consumed,
}

/// 判定出的注释态动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AltEffect {
    None,
    ToggleSession,
    TogglePersist,
    HoldOn,
    HoldOff,
}

/// Alt 事件是否带了别的修饰位。平台把这次按住期间按过的 Shift / Ctrl / Win 粘在 Alt 事件上
/// （桌面 TSF 不转发这几个键的按下），带了就不是单击：Alt+Shift 是系统切换输入语言的热键。
pub(crate) fn carries_other_modifier(modifiers: u32) -> bool {
    use wind_ipc::protocol::{
        MOD_CTRL, MOD_LCTRL, MOD_LSHIFT, MOD_RCTRL, MOD_RSHIFT, MOD_SHIFT, MOD_WIN,
    };
    modifiers & (MOD_SHIFT | MOD_CTRL | MOD_WIN | MOD_LSHIFT | MOD_RSHIFT | MOD_LCTRL | MOD_RCTRL)
        != 0
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
) -> (AltOutcome, AltEffect) {
    if mode == SwitchMode::Off || !has_candidates {
        *track = None;
        return (AltOutcome::PassThrough, AltEffect::None);
    }
    if !track.as_ref().is_some_and(|t| !t.dirty) {
        *track = Some(AltTrack {
            down_at: now,
            dirty: false,
        });
    }
    match mode {
        SwitchMode::Hold => (AltOutcome::Consumed, AltEffect::HoldOn),
        _ => (AltOutcome::PassThrough, AltEffect::None),
    }
}

/// Alt 松开。无论结果如何都清掉 track。
pub(crate) fn on_alt_up(
    mode: SwitchMode,
    has_candidates: bool,
    track: &mut Option<AltTrack>,
    now: Instant,
) -> (AltOutcome, AltEffect) {
    let Some(t) = track.take() else {
        return (AltOutcome::PassThrough, AltEffect::None);
    };
    // 按下吃了，松开也吃：按住期间上屏关了窗，松开时已无候选。
    if mode == SwitchMode::Hold {
        return (AltOutcome::Consumed, AltEffect::HoldOff);
    }
    let tap = !t.dirty && now.saturating_duration_since(t.down_at) <= ALT_TAP_MAX;
    if !has_candidates || !tap {
        return (AltOutcome::PassThrough, AltEffect::None);
    }
    match mode {
        SwitchMode::Session => (AltOutcome::Consumed, AltEffect::ToggleSession),
        SwitchMode::Persist => (AltOutcome::Consumed, AltEffect::TogglePersist),
        _ => (AltOutcome::PassThrough, AltEffect::None),
    }
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
    use AltEffect as E;
    use AltOutcome as O;

    fn tap(mode: SwitchMode, held_ms: u64) -> [(AltOutcome, AltEffect); 2] {
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
        use wind_ipc::protocol::{MOD_ALT, MOD_CTRL, MOD_LSHIFT, MOD_SHIFT, MOD_WIN};
        assert!(!carries_other_modifier(0));
        assert!(!carries_other_modifier(MOD_ALT));
        for m in [MOD_SHIFT, MOD_CTRL, MOD_WIN, MOD_LSHIFT] {
            assert!(carries_other_modifier(m | MOD_ALT), "{m:#x}");
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
