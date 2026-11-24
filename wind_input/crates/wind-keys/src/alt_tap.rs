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
///   钩子里它是几次 `GetAsyncKeyState`；
/// - `foreground_ok`：前台窗口是否属于活动客户端的进程（候选窗残留时别的程序里的 Alt 不接管）。
///   同样只在要认领时才调。
///
/// 自动连发：认领后同侧 Alt 的重复按下不重置时刻。若松开丢过（钩子被系统摘掉），残留的认领
/// 会被下一次按下当成连发：那次按住时长过长不算单击，其松开照常补发，之后自愈。
#[cfg_attr(not(windows), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_alt(
    press: &mut Option<AltPress>,
    vk: u32,
    is_down: bool,
    origin: Origin,
    time: u32,
    gate: u32,
    others_held: impl FnOnce() -> bool,
    foreground_ok: impl FnOnce() -> bool,
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
                if gate & side == 0 || others_held() || !foreground_ok() {
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
#[path = "alt_tap_tests.rs"]
mod tests;
