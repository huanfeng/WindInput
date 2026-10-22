//! 组字中 Alt 单击 / 按住切换注释态（`ui.comment.alt_switch`）的判定。
//!
//! 平台层只在组字中转发 Alt 的按下与松开，不做计时；吃不吃由这里的 [`AltOutcome`] 定。
//! 判定是纯函数，时刻由调用方传入，便于测出长按、组合键等分支。

use crate::coordinator::AltTrack;
use std::time::{Duration, Instant};
use wind_config::comment_lib::AltSwitch;

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

/// Alt 按下。重复按下（自动连发）保留首次按下的时刻。
pub(crate) fn on_alt_down(
    mode: AltSwitch,
    has_candidates: bool,
    track: &mut Option<AltTrack>,
    now: Instant,
) -> (AltOutcome, AltEffect) {
    if mode == AltSwitch::Off || !has_candidates {
        return (AltOutcome::PassThrough, AltEffect::None);
    }
    track.get_or_insert(AltTrack {
        down_at: now,
        dirty: false,
    });
    match mode {
        AltSwitch::Hold => (AltOutcome::Consumed, AltEffect::HoldOn),
        _ => (AltOutcome::PassThrough, AltEffect::None),
    }
}

/// Alt 松开。无论结果如何都清掉 track。
pub(crate) fn on_alt_up(
    mode: AltSwitch,
    has_candidates: bool,
    track: &mut Option<AltTrack>,
    now: Instant,
) -> (AltOutcome, AltEffect) {
    let Some(t) = track.take() else {
        return (AltOutcome::PassThrough, AltEffect::None);
    };
    // 按下吃了，松开也吃：按住期间上屏关了窗，松开时已无候选。
    if mode == AltSwitch::Hold {
        return (AltOutcome::Consumed, AltEffect::HoldOff);
    }
    let tap = !t.dirty && now.saturating_duration_since(t.down_at) <= ALT_TAP_MAX;
    if !has_candidates || !tap {
        return (AltOutcome::PassThrough, AltEffect::None);
    }
    match mode {
        AltSwitch::Session => (AltOutcome::Consumed, AltEffect::ToggleSession),
        AltSwitch::Persist => (AltOutcome::Consumed, AltEffect::TogglePersist),
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

    fn tap(mode: AltSwitch, held_ms: u64) -> [(AltOutcome, AltEffect); 2] {
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
            tap(AltSwitch::Session, 100),
            [(O::PassThrough, E::None), (O::Consumed, E::ToggleSession)]
        );
    }

    #[test]
    fn long_press_is_not_tap() {
        assert_eq!(tap(AltSwitch::Session, 600)[1], (O::PassThrough, E::None));
    }

    #[test]
    fn combo_is_not_tap() {
        let t0 = Instant::now();
        let mut track = None;
        on_alt_down(AltSwitch::Session, true, &mut track, t0);
        on_other_key(&mut track);
        let up = on_alt_up(
            AltSwitch::Session,
            true,
            &mut track,
            t0 + Duration::from_millis(50),
        );
        assert_eq!(up, (O::PassThrough, E::None));
        assert!(track.is_none(), "松开后清掉 track");
    }

    #[test]
    fn no_candidates_passes_through() {
        for mode in [AltSwitch::Session, AltSwitch::Persist, AltSwitch::Hold] {
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
        on_alt_down(AltSwitch::Session, true, &mut track, t0);
        assert_eq!(
            on_alt_up(AltSwitch::Session, false, &mut track, t0),
            (O::PassThrough, E::None)
        );
    }

    #[test]
    fn off_mode_passes_through() {
        assert_eq!(
            tap(AltSwitch::Off, 100),
            [(O::PassThrough, E::None), (O::PassThrough, E::None)]
        );
    }

    #[test]
    fn persist_mode_toggles_persist() {
        assert_eq!(
            tap(AltSwitch::Persist, 100)[1],
            (O::Consumed, E::TogglePersist)
        );
    }

    #[test]
    fn hold_on_off() {
        assert_eq!(
            tap(AltSwitch::Hold, 2000),
            [(O::Consumed, E::HoldOn), (O::Consumed, E::HoldOff)]
        );
        // 期间按过别的键、松开时已无候选：松开仍吃。
        let t0 = Instant::now();
        let mut track = None;
        on_alt_down(AltSwitch::Hold, true, &mut track, t0);
        on_other_key(&mut track);
        assert_eq!(
            on_alt_up(AltSwitch::Hold, false, &mut track, t0),
            (O::Consumed, E::HoldOff)
        );
    }

    #[test]
    fn repeat_down_keeps_first_press_time() {
        let t0 = Instant::now();
        let mut track = None;
        on_alt_down(AltSwitch::Session, true, &mut track, t0);
        on_alt_down(
            AltSwitch::Session,
            true,
            &mut track,
            t0 + Duration::from_millis(400),
        );
        let up = on_alt_up(
            AltSwitch::Session,
            true,
            &mut track,
            t0 + Duration::from_millis(600),
        );
        assert_eq!(up, (O::PassThrough, E::None), "连发不重置计时");
    }
}
