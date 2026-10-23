//! 注释切换键选了 Alt 时，全局低级键盘钩子里的 Alt 判定（钩子本体见 [`crate::capslock_hook`]）。
//!
//! Windows 上 TSF 收不到单独 Alt 的按下（`WM_SYSKEYDOWN`），只收得到松开，判不出单击；
//! 而 Alt 松开放给宿主就会激活菜单栏。故整套判定挪到钩子里：
//!
//! - Alt 按下一律放行（宿主照常进入 Alt 态，Alt+Tab / Alt+Shift / Alt+数字都不受影响），
//!   闸门开时认领这次按下；
//! - 认领的这次按下没被别的键打断：吃掉物理松开，补发「屏蔽键 down/up + 同侧 Alt 松开」
//!   （宿主看到 Alt 期间按过别的键，就不激活菜单栏）；被打断则松开原样放行，不补发；
//! - 自己注入的事件（带 [`crate::key_inject::INJECT_MARK`]）一律放行、不改状态。
//!
//! 判定写成纯函数：钩子回调在 Linux 单测里跑不到，而「吃掉松开却没补发」会让 Alt 卡住，
//! 这类后果必须能脱离平台逐条断言。

/// 闸门位：左 Alt。
pub const GATE_LEFT: u32 = 1;
/// 闸门位：右 Alt。
pub const GATE_RIGHT: u32 = 2;
/// 闸门位：按住档（投递 `HoldDown` / `HoldUp`，不投递 `Tap`）。
pub const GATE_HOLD: u32 = 4;

/// 单击的最长按住时长（毫秒），同服务端 `comment_switch::ALT_TAP_MAX`。
pub const TAP_MAX_MS: u32 = 500;

const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;

/// 投递给协调器的 Alt 事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltEvent {
    /// 干净的单击（未被打断、按住不超过 [`TAP_MAX_MS`]）。
    Tap,
    /// 按住档：认领的按下。
    HoldDown,
    /// 按住档：认领的那次按下松开了（被打断也投递）。
    HoldUp,
}

/// 事件来源。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Physical,
    /// 别的程序注入的（`LLKHF_INJECTED` 且不带我们的标记）。
    Injected,
    /// 我们自己注入的（带标记）。
    Ours,
}

/// 被认领的这次 Alt 按下。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AltPress {
    vk: u32,
    down_time: u32,
    interrupted: bool,
    hold: bool,
}

/// 一次事件的裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AltVerdict {
    /// 吃掉这次事件。
    pub eat: bool,
    /// 吃掉之后补发「屏蔽键 down/up + 同侧 Alt 松开」。只与 `eat` 同时为真。
    pub mask: bool,
    pub event: Option<AltEvent>,
}

const PASS: AltVerdict = AltVerdict {
    eat: false,
    mask: false,
    event: None,
};

fn side_bit(vk: u32) -> u32 {
    match vk {
        VK_LMENU => GATE_LEFT,
        VK_RMENU => GATE_RIGHT,
        _ => 0,
    }
}

/// 对一次键盘事件裁决，并更新认领状态 `press`。
///
/// - `gate`：闸门（`GATE_*` 位，0 = 不接管），只在认领时看；
/// - `time`：钩子事件的系统时间戳（毫秒，会回绕）；
/// - `others_held`：此刻是否按着 Shift / Ctrl / Win（Ctrl+Alt 等不认领）。只在要认领时才调，
///   钩子里它是几次 `GetAsyncKeyState`。
///
/// 自动连发：认领后同侧 Alt 的重复按下不重置时刻。若松开丢过（钩子被系统摘掉），残留的认领
/// 会被下一次按下当成连发：那次按住时长过长不算单击，其松开照常补发，之后自愈。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn decide_alt(
    press: &mut Option<AltPress>,
    vk: u32,
    is_down: bool,
    origin: Origin,
    time: u32,
    gate: u32,
    others_held: impl FnOnce() -> bool,
) -> AltVerdict {
    if origin == Origin::Ours {
        return PASS;
    }
    let side = side_bit(vk);
    if side == 0 {
        // 认领期间别的键按下（含别的程序注入的）：这次就不是单击。
        if is_down && let Some(p) = press {
            p.interrupted = true;
        }
        return PASS;
    }
    if origin == Origin::Injected {
        return PASS;
    }
    if is_down {
        match press {
            Some(p) if p.vk == vk => PASS,
            Some(p) => {
                // 按住一侧时又按了另一侧。
                p.interrupted = true;
                PASS
            }
            None => {
                if gate & side == 0 || others_held() {
                    return PASS;
                }
                let hold = gate & GATE_HOLD != 0;
                *press = Some(AltPress {
                    vk,
                    down_time: time,
                    interrupted: false,
                    hold,
                });
                AltVerdict {
                    event: hold.then_some(AltEvent::HoldDown),
                    ..PASS
                }
            }
        }
    } else {
        let Some(p) = press.filter(|p| p.vk == vk) else {
            return PASS;
        };
        *press = None;
        let event = if p.hold {
            Some(AltEvent::HoldUp)
        } else if !p.interrupted && time.wrapping_sub(p.down_time) <= TAP_MAX_MS {
            Some(AltEvent::Tap)
        } else {
            None
        };
        // 认领的松开不再看闸门：按下后上屏关了窗也照样补发，否则宿主激活菜单栏。
        let clean = !p.interrupted;
        AltVerdict {
            eat: clean,
            mask: clean,
            event,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: u32 = VK_LMENU;
    const R: u32 = VK_RMENU;
    const TAB: u32 = 0x09;
    const LSHIFT: u32 = 0xA0;
    const DIGIT1: u32 = 0x31;
    const MASK: u32 = crate::key_inject::VK_MENU_MASK;
    const BOTH: u32 = GATE_LEFT | GATE_RIGHT;

    fn ev(
        press: &mut Option<AltPress>,
        vk: u32,
        is_down: bool,
        time: u32,
        gate: u32,
    ) -> AltVerdict {
        decide_alt(press, vk, is_down, Origin::Physical, time, gate, || false)
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
        decide_alt(&mut p, TAB, true, Origin::Injected, 1050, BOTH, || false);
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
        let v = decide_alt(&mut p, L, true, Origin::Physical, 1000, BOTH, || true);
        assert_eq!(v, PASS);
        assert!(p.is_none(), "Ctrl+Alt / Shift+Alt 不认领");
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
            let v = decide_alt(&mut p, L, true, origin, 1000, BOTH, || false);
            assert_eq!(v, PASS);
            assert!(p.is_none(), "{origin:?} 的 Alt 按下不认领");
        }
        ev(&mut p, L, true, 1000, BOTH);
        // 我们补发的屏蔽键与 Alt 松开：放行，不算打断、不销账。
        for (vk, down) in [(MASK, true), (MASK, false), (L, false)] {
            let v = decide_alt(&mut p, vk, down, Origin::Ours, 1050, BOTH, || false);
            assert_eq!(v, PASS);
        }
        // 别的程序注入的同侧 Alt 松开也不销账。
        decide_alt(&mut p, L, false, Origin::Injected, 1060, BOTH, || false);
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
}
