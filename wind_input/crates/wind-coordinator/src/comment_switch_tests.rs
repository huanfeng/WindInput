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

// ── Ctrl 如实上报 ──

fn ctrl_press(held_ms: u64, between: impl FnOnce(&mut Option<CtrlTrack>), not_tap: bool) -> bool {
    let t0 = Instant::now();
    let mut track = None;
    on_ctrl_raw_down(&mut track, 0xA2, false, t0);
    between(&mut track);
    let tap = on_ctrl_raw_up(
        &mut track,
        0xA2,
        not_tap,
        t0 + Duration::from_millis(held_ms),
    );
    assert!(track.is_none(), "松开后清掉 track");
    tap
}

#[test]
fn ctrl_raw_clean_short_press_is_tap() {
    assert!(ctrl_press(100, |_| {}, false));
    assert!(ctrl_press(500, |_| {}, false), "500ms 恰在阈值内");
}

#[test]
fn ctrl_raw_long_press_is_not_tap() {
    assert!(!ctrl_press(501, |_| {}, false));
}

#[test]
fn ctrl_raw_other_key_is_not_tap() {
    // Ctrl+C / Ctrl+数字（WM_HOTKEY）送到服务端的那一键。
    assert!(!ctrl_press(50, on_other_ctrl_key, false));
}

#[test]
fn ctrl_raw_platform_not_tap_flag() {
    // 平台看见过服务端看不见的键或鼠标。
    assert!(!ctrl_press(50, |_| {}, true));
}

#[test]
fn ctrl_raw_combo_modifier_at_down_is_not_tap() {
    let t0 = Instant::now();
    let mut track = None;
    on_ctrl_raw_down(&mut track, 0xA2, true, t0);
    assert!(!on_ctrl_raw_up(&mut track, 0xA2, false, t0));
}

#[test]
fn ctrl_raw_up_without_down_is_not_tap() {
    let mut track = None;
    assert!(!on_ctrl_raw_up(&mut track, 0xA2, false, Instant::now()));
}

#[test]
fn ctrl_raw_second_down_restarts() {
    // 上次松开丢了：第二次按下重新起算，不被拖成长按。
    let t0 = Instant::now();
    let mut track = None;
    on_ctrl_raw_down(&mut track, 0xA2, false, t0);
    let t1 = t0 + Duration::from_millis(400);
    on_ctrl_raw_down(&mut track, 0xA2, false, t1);
    assert!(on_ctrl_raw_up(
        &mut track,
        0xA2,
        false,
        t1 + Duration::from_millis(200)
    ));
}

#[test]
fn ctrl_raw_other_side_takes_over() {
    let t0 = Instant::now();
    let mut track = None;
    on_ctrl_raw_down(&mut track, 0xA2, false, t0);
    on_ctrl_raw_down(&mut track, 0xA3, false, t0);
    assert!(
        !on_ctrl_raw_up(&mut track, 0xA2, false, t0),
        "左 Ctrl 的松开找不到 track"
    );
    on_ctrl_raw_down(&mut track, 0xA3, false, t0);
    assert!(on_ctrl_raw_up(&mut track, 0xA3, false, t0));
}

#[test]
fn ctrl_raw_dirty_track_restarts_on_fresh_down() {
    let t0 = Instant::now();
    let mut track = None;
    on_ctrl_raw_down(&mut track, 0xA2, false, t0);
    on_other_ctrl_key(&mut track);
    // 组合键之后松开没送到，2 秒后重新单击。
    let t1 = t0 + Duration::from_secs(2);
    on_ctrl_raw_down(&mut track, 0xA2, false, t1);
    assert!(on_ctrl_raw_up(
        &mut track,
        0xA2,
        false,
        t1 + Duration::from_millis(80)
    ));
}

#[test]
fn ctrl_hold_flips_on_down_and_always_restores() {
    assert_eq!(
        ctrl_hold_effect(SwitchMode::Hold, true, true, true),
        E::HoldOn
    );
    assert_eq!(
        ctrl_hold_effect(SwitchMode::Hold, true, false, true),
        E::None,
        "无候选不翻"
    );
    for mode in [SwitchMode::Hold, SwitchMode::Session, SwitchMode::Off] {
        for cands in [true, false] {
            assert_eq!(
                ctrl_hold_effect(mode, true, cands, false),
                E::HoldOff,
                "{mode:?}"
            );
        }
    }
    assert_eq!(
        ctrl_hold_effect(SwitchMode::Session, true, true, true),
        E::None
    );
    assert_eq!(
        ctrl_hold_effect(SwitchMode::Hold, false, true, true),
        E::None,
        "另一侧"
    );
    assert_eq!(
        ctrl_hold_effect(SwitchMode::Hold, false, true, false),
        E::None
    );
}
