use super::*;

const L: u32 = VK_LMENU;
const R: u32 = VK_RMENU;
const TAB: u32 = 0x09;
const LSHIFT: u32 = 0xA0;
const DIGIT1: u32 = 0x31;
const MASK: u32 = crate::key_inject::VK_MENU_MASK;
const BOTH: u32 = GATE_LEFT | GATE_RIGHT;

fn ev(press: &mut Option<AltPress>, vk: u32, is_down: bool, time: u32, gate: u32) -> AltVerdict {
    decide_alt(
        press,
        vk,
        is_down,
        Origin::Physical,
        time,
        gate,
        || false,
        || true,
    )
}

const MASKED_TAP: AltVerdict = AltVerdict {
    eat: true,
    mask: true,
    event: Some(AltEvent::Tap),
};

#[test]
fn tap_passes_down_masks_up_and_reports_tap() {
    let mut p = None;
    assert_eq!(ev(&mut p, L, true, 1000, BOTH), PASS, "按下放行");
    assert!(p.is_some(), "闸门开时认领");
    assert_eq!(ev(&mut p, L, false, 1100, BOTH), MASKED_TAP);
    assert!(p.is_none());
}

#[test]
fn long_press_masks_but_is_not_tap() {
    let mut p = None;
    ev(&mut p, L, true, 1000, BOTH);
    let up = ev(&mut p, L, false, 1000 + TAP_MAX_MS + 1, BOTH);
    assert_eq!(
        up,
        AltVerdict {
            eat: true,
            mask: true,
            event: None
        },
        "长按不切换，但仍压住菜单栏"
    );
}

#[test]
fn auto_repeat_keeps_first_down_time() {
    let mut p = None;
    ev(&mut p, L, true, 1000, BOTH);
    for t in [1300, 1330, 1360] {
        assert_eq!(ev(&mut p, L, true, t, BOTH), PASS);
    }
    assert_eq!(
        ev(&mut p, L, false, 1600, BOTH).event,
        None,
        "连发不重置计时"
    );
}

#[test]
fn tap_time_survives_tick_wraparound() {
    let mut p = None;
    ev(&mut p, L, true, u32::MAX - 50, BOTH);
    assert_eq!(ev(&mut p, L, false, 49, BOTH), MASKED_TAP);
}

/// Alt+Tab / Alt+Shift / Alt+数字：被打断 ⇒ 松开放行、不补发、不切换。
#[test]
fn combos_are_interrupted_and_pass_untouched() {
    for other in [TAB, LSHIFT, DIGIT1] {
        let mut p = None;
        ev(&mut p, L, true, 1000, BOTH);
        assert_eq!(
            ev(&mut p, other, true, 1050, BOTH),
            PASS,
            "{other:#x} 按下放行"
        );
        assert_eq!(ev(&mut p, other, false, 1080, BOTH), PASS);
        assert_eq!(
            ev(&mut p, L, false, 1100, BOTH),
            PASS,
            "{other:#x}：松开原样放行"
        );
        assert!(p.is_none());
    }
}

#[test]
fn injected_other_key_also_interrupts() {
    let mut p = None;
    ev(&mut p, L, true, 1000, BOTH);
    decide_alt(
        &mut p,
        TAB,
        true,
        Origin::Injected,
        1050,
        BOTH,
        || false,
        || true,
    );
    assert_eq!(ev(&mut p, L, false, 1100, BOTH), PASS);
}

#[test]
fn other_side_alt_interrupts() {
    let mut p = None;
    ev(&mut p, L, true, 1000, BOTH);
    assert_eq!(ev(&mut p, R, true, 1020, BOTH), PASS);
    assert_eq!(ev(&mut p, R, false, 1040, BOTH), PASS, "另一侧不归我们");
    assert_eq!(ev(&mut p, L, false, 1100, BOTH), PASS);
}

#[test]
fn modifier_held_before_alt_is_not_claimed() {
    let mut p = None;
    let v = decide_alt(
        &mut p,
        L,
        true,
        Origin::Physical,
        1000,
        BOTH,
        || true,
        || true,
    );
    assert_eq!(v, PASS);
    assert!(p.is_none(), "Ctrl+Alt / Shift+Alt 不认领");
    assert_eq!(ev(&mut p, L, false, 1100, BOTH), PASS);
}

/// 前台不是活动客户端的进程（候选窗残留在别处）：不认领，松开放行、不补发。
#[test]
fn foreground_mismatch_is_not_claimed() {
    let mut p = None;
    let v = decide_alt(
        &mut p,
        L,
        true,
        Origin::Physical,
        1000,
        BOTH,
        || false,
        || false,
    );
    assert_eq!(v, PASS);
    assert!(p.is_none());
    assert_eq!(ev(&mut p, L, false, 1100, BOTH), PASS);
}

/// 没在组字（闸门关）时按下：不认领，松开放行，宿主菜单栏照常激活。
#[test]
fn gate_closed_at_down_is_not_claimed() {
    let mut p = None;
    assert_eq!(ev(&mut p, L, true, 1000, 0), PASS);
    assert!(p.is_none());
    assert_eq!(ev(&mut p, L, false, 1100, 0), PASS);
    // 按下后闸门才开：这次松开也不归我们。
    assert_eq!(ev(&mut p, L, false, 1100, BOTH), PASS);
}

/// 按下后上屏关了窗（闸门关）：松开仍吃掉并补发，否则 Alt 状态卡住或激活菜单栏。
#[test]
fn gate_closed_before_up_still_masks() {
    let mut p = None;
    ev(&mut p, L, true, 1000, BOTH);
    assert_eq!(ev(&mut p, L, false, 1100, 0), MASKED_TAP);
}

#[test]
fn injected_and_own_events_pass_without_state_change() {
    let mut p = None;
    for origin in [Origin::Injected, Origin::Ours] {
        let v = decide_alt(&mut p, L, true, origin, 1000, BOTH, || false, || true);
        assert_eq!(v, PASS);
        assert!(p.is_none(), "{origin:?} 的 Alt 按下不认领");
    }
    ev(&mut p, L, true, 1000, BOTH);
    // 我们补发的屏蔽键与 Alt 松开：放行，不算打断、不销账。
    for (vk, down) in [(MASK, true), (MASK, false), (L, false)] {
        let v = decide_alt(
            &mut p,
            vk,
            down,
            Origin::Ours,
            1050,
            BOTH,
            || false,
            || true,
        );
        assert_eq!(v, PASS);
    }
    // 别的程序注入的同侧 Alt 松开也不销账。
    decide_alt(
        &mut p,
        L,
        false,
        Origin::Injected,
        1060,
        BOTH,
        || false,
        || true,
    );
    assert_eq!(ev(&mut p, L, false, 1100, BOTH), MASKED_TAP);
}

#[test]
fn side_filter() {
    // 只认左。
    let mut p = None;
    assert_eq!(ev(&mut p, R, true, 1000, GATE_LEFT), PASS);
    assert!(p.is_none());
    assert_eq!(ev(&mut p, R, false, 1100, GATE_LEFT), PASS);
    ev(&mut p, L, true, 2000, GATE_LEFT);
    assert_eq!(ev(&mut p, L, false, 2100, GATE_LEFT), MASKED_TAP);
    // 只认右。
    assert_eq!(ev(&mut p, L, true, 3000, GATE_RIGHT), PASS);
    assert!(p.is_none());
    ev(&mut p, R, true, 4000, GATE_RIGHT);
    assert_eq!(ev(&mut p, R, false, 4100, GATE_RIGHT), MASKED_TAP);
}

#[test]
fn non_alt_keys_never_claim() {
    let mut p = None;
    for vk in [0xA2, 0xA3, 0x12, TAB] {
        assert_eq!(ev(&mut p, vk, true, 1000, BOTH | GATE_HOLD), PASS);
        assert!(p.is_none());
    }
}

#[test]
fn hold_down_up_pairing() {
    let g = BOTH | GATE_HOLD;
    let mut p = None;
    let down = ev(&mut p, L, true, 1000, g);
    assert_eq!(down.event, Some(AltEvent::HoldDown));
    assert!(!down.eat, "按下放行");
    // 连发不重复投递。
    assert_eq!(ev(&mut p, L, true, 1300, g), PASS);
    assert_eq!(
        ev(&mut p, L, false, 3000, 0),
        AltVerdict {
            eat: true,
            mask: true,
            event: Some(AltEvent::HoldUp)
        },
        "按住多久都不投 Tap；闸门关了也补发"
    );
}

#[test]
fn interrupted_hold_still_reports_hold_up() {
    let g = BOTH | GATE_HOLD;
    let mut p = None;
    ev(&mut p, L, true, 1000, g);
    ev(&mut p, DIGIT1, true, 1100, g);
    assert_eq!(
        ev(&mut p, L, false, 1200, g),
        AltVerdict {
            eat: false,
            mask: false,
            event: Some(AltEvent::HoldUp)
        }
    );
}

/// 闸门在认领时定档：按住期间改了档也按认领时的走，按下 / 松开成对。
#[test]
fn mode_is_fixed_at_claim() {
    let mut p = None;
    ev(&mut p, L, true, 1000, BOTH | GATE_HOLD);
    assert_eq!(
        ev(&mut p, L, false, 1100, BOTH).event,
        Some(AltEvent::HoldUp)
    );
    ev(&mut p, L, true, 2000, BOTH);
    assert_eq!(
        ev(&mut p, L, false, 2100, BOTH | GATE_HOLD).event,
        Some(AltEvent::Tap)
    );
}
