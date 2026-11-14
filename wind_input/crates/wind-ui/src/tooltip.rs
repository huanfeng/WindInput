//! 候选悬停提示气泡：悬停候选时显示协调器按段列表渲染好的 [`TooltipDoc`]。
//!
//! 与 Go 版本 `wind_input/internal/ui/tooltip.go` 对齐（简化版）。
//! 深色圆角小气泡 + DirectWrite 文本。
//!
//! # 右键命中
//!
//! 整块文本仍是**一个**叶节点（`TooltipDoc::to_plain_text` 画出来的那串），外观与段列表
//! 引入前逐像素相同。命中不靠逐行布局，而是量出文本块的矩形（与 `View` 绘制同一套定位
//! 公式），再按行数均分——渲染器的行距钉成 UNIFORM（见 `text::dwrite` 的
//! `create_layout_with`），每行等高。点中的行经 `TooltipDoc::hit_at_line` 换算回
//! `(段, 原始行)`，随 `RequestTooltipMenu` 交给协调器。
//!
//! # 右键菜单打开期间
//!
//! 菜单一开就 `SetCapture`，同线程的气泡从此收不到鼠标消息（`WM_MOUSEMOVE` / 按键都没有），
//! 而鼠标移向菜单时 `WM_MOUSELEAVE` 已经把 `mouse_over` 清掉、离开跟踪也随之失效。所以
//! 菜单关闭时 `mouse_over` 是**陈旧**的，不能拿来判「鼠标还在不在气泡上」——点回气泡关菜单
//! 时气泡会被当成「鼠标已离开」而隐藏。几件事因此都改问真实光标（[`menu_step`] 是判据）：
//! - 菜单关闭（任何方式）：光标在气泡上就留下并重挂离开跟踪，否则隐藏；
//! - 菜单外按下落在气泡上且是右键、关掉的正是气泡的菜单：菜单轮询看见了这次按下（气泡
//!   自己收不到），转给这里按新位置重新请求菜单，相当于「重新右键」；
//! - 气泡自己的 `WM_RBUTTONDOWN` 在**任何**菜单打开期间一律不理，只认轮询那一路，免得
//!   同一次右键请求两遍菜单；工具栏 / 状态菜单开着时右键气泡也就只关菜单。
//!
//! 重挂离开跟踪会招来一条**伪** `WM_MOUSELEAVE`：菜单持有捕获期间，系统记的「鼠标所在窗口」
//! 是菜单；`ReleaseCapture` 后还没处理过一次鼠标移动时就 `TrackMouseEvent`，系统认定光标
//! 不在气泡上，当场投递离开——而此刻抑制刚解除，照旧处理就会把刚决定留下的气泡藏掉（靶机
//! 实测：左键点气泡关菜单、菜单开着时右键气泡另一处，气泡都随之消失）。故「离开」也问真实
//! 光标：光标仍在气泡上就不是离开，只清掉跟踪，等下一次真实 `WM_MOUSEMOVE` 再挂。光标若一次
//! 都没在气泡上移动就离开，那条移动永远不来，故另起复查定时器（[`RECHECK_MS`]），且跟踪没
//! 挂着时 [`Tooltip::hide`] 改问真实光标，不按 `mouse_over` 推迟。
//!
//! # 悬停宽限与强制隐藏（设计 §7.6 C）
//!
//! 真实离开气泡不当场藏，开 [`LEAVE_GRACE_MS`] 宽限：光标穿过空隙回到所属候选行时，悬停值
//! 没变、协调器不重绘，当场藏了气泡就不回来。候选窗收起走 [`Tooltip::hide_force`]，光标停在
//! 气泡上也照收。
//!
//! 「关掉的正是气泡的菜单」看 `suppress_hide`。它由协调器的 `SetTooltipMenuOpen` 维护，
//! 但协调器有几条收菜单的路（打字、失焦、切走输入法、组合被终止）走的是 `HideMenu` /
//! `HideCandidates`，不一定补发 `SetTooltipMenuOpen(false)`。故 UI 循环在这两条命令真把一个
//! 可见的菜单收掉时调 [`Tooltip::on_menu_dismissed`]，按「菜单已关闭」处理，标志不会残留。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use crate::manager::UiEvent;
use crate::sys::{
    GetCursorPos, GetWindowRect, HWND, LPARAM, LRESULT, POINT, RECT, WM_MOUSELEAVE, WM_MOUSEMOVE,
    WM_RBUTTONDOWN, WPARAM,
};
use crate::text::dwrite::{ColorRun, TextRenderer};
use crate::view::{Align, Edges, View, ViewImage, ViewLayer};
use crate::window::{LayeredWindow, WindowMouse};
use wind_theme::RvNode;
use wind_ui_types::{TooltipDoc, TooltipHit};

/// 当前显示内容的命中信息（`Tooltip` 写、`TooltipMouse` 读）。
#[derive(Default)]
struct HitState {
    /// 与协调器下发的 `CandidateItem::tooltip` 共享同一份，不深拷贝。
    doc: Arc<TooltipDoc>,
    /// 气泡属于当前页第几个候选（页内下标）。
    candidate: i32,
    /// 文本块在窗口客户区里的矩形（最近一次渲染）。
    text_box: Option<TextBox>,
}

impl HitState {
    /// 客户区（位图内）坐标 → 点中的段 / 原始行；落在文本块外为 `None`。
    fn hit_at(&self, cx: i32, cy: i32) -> Option<TooltipHit> {
        self.text_box
            .and_then(|b| b.line_at(cx, cy))
            .and_then(|i| self.doc.hit_at_line(i))
    }
}

/// 文本块矩形 + 行数。
#[derive(Debug, Clone, Copy, PartialEq)]
struct TextBox {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    lines: usize,
}

impl TextBox {
    /// 客户区坐标 → 第几行（从 0 起）。落在文本块外（内边距、阴影扩边）为 `None`。
    fn line_at(&self, x: i32, y: i32) -> Option<usize> {
        let (x, y) = (x as f32, y as f32);
        if self.lines == 0
            || x < self.x
            || x >= self.x + self.w
            || y < self.y
            || y >= self.y + self.h
        {
            return None;
        }
        let line_h = self.h / self.lines as f32;
        Some((((y - self.y) / line_h) as usize).min(self.lines - 1))
    }
}

/// `WM_*BUTTON*` 的 lParam → 客户区坐标（低 / 高 16 位，有符号）。
fn client_point(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0;
    (
        (v & 0xFFFF) as u16 as i16 as i32,
        ((v >> 16) & 0xFFFF) as u16 as i16 as i32,
    )
}

/// 右键菜单相关时刻气泡要回答的事件（见模块文档「右键菜单打开期间」）。
///
/// `on_tip` 是光标此刻是否真在气泡上——问系统得来，不是 `mouse_over`，菜单开着期间那个值
/// 是陈旧的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuEvent {
    /// 气泡自己收到 `WM_RBUTTONDOWN`。`any_menu` = 此刻有弹出菜单可见（不论是谁的）；
    /// `in_flight` = 气泡已发出的菜单请求还没回应。
    OwnRightDown { any_menu: bool, in_flight: bool },
    /// 菜单轮询到一次菜单外按下（菜单已随之关闭）。`own_menu` = 关掉的是气泡的菜单。
    OutsidePress {
        right: bool,
        own_menu: bool,
        on_tip: bool,
    },
    /// 菜单已关闭（协调器 `SetTooltipMenuOpen(false)`，或 UI 循环收掉了可见菜单）。
    Closed { on_tip: bool },
    /// 气泡收到 `WM_MOUSELEAVE`。`suppress` = 菜单打开中的隐藏抑制；`spurious` = 此前已连续
    /// 收到几条伪离开（中间没有真实 `WM_MOUSEMOVE`）。
    Leave {
        suppress: bool,
        on_tip: bool,
        spurious: u8,
    },
    /// 复查定时器到期：伪离开后的复查（[`RECHECK_MS`]），或真实离开后的宽限
    /// （[`LEAVE_GRACE_MS`]）。`suppress` 同 `Leave`：光标没动就右键气泡时，菜单请求已发出、
    /// 抑制已开，复查不该再来定去留。`on_anchor` = 光标在气泡所属的候选行上。
    Recheck {
        suppress: bool,
        on_tip: bool,
        on_anchor: bool,
    },
}

/// 气泡对 [`MenuEvent`] 的反应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuStep {
    /// 什么都不做。
    Ignore,
    /// 按命中位置请求菜单（首次右键，或菜单开着时在气泡上再右键）。
    RequestMenu,
    /// 留下气泡，并重挂离开跟踪。
    Keep,
    /// 隐藏气泡。
    Hide,
    /// 伪离开：留下气泡，清掉离开跟踪，等下一次真实 `WM_MOUSEMOVE` 或复查定时器再挂。不能
    /// 当场重挂——系统此刻仍认定光标不在气泡上，重挂只会再收一条伪离开。
    AwaitMove,
    /// 复查时光标仍在气泡上：补挂离开跟踪（不清伪离开计数）。
    Retrack,
    /// 真实离开：先不隐藏，开宽限期（[`LEAVE_GRACE_MS`]），到期再定。
    Grace,
    /// 宽限到期时光标回到了气泡所属的候选行：气泡留下，不挂跟踪——此后去留跟着候选悬停走。
    Idle,
}

/// 真实离开气泡后的宽限期（毫秒）。从气泡移回它所属的候选行要穿过两者之间的空隙
/// （阴影扩边、内边距）；一离开就藏，光标回到候选行时悬停值没变、协调器不重绘，气泡就再也
/// 不回来，看上去是「移回去气泡没了」。280ms 覆盖正常手速穿过十几到几十像素的空隙，又短到
/// 移到别处时气泡的逗留不显得拖沓（常见 hover-intent 取 200–300ms）。与候选窗那侧的
/// `candidate_window::TIP_GRACE` 同值，两侧对称。
pub(crate) const LEAVE_GRACE_MS: u32 = 280;

/// 连续伪离开的上限：到达后按真实离开处理。每条伪离开之后复查补挂一次跟踪，若系统始终
/// 认定光标不在气泡上，就会「补挂 → 伪离开 → 补挂」无限循环；宁可把气泡藏掉。
const MAX_SPURIOUS_LEAVES: u8 = 3;

/// 伪离开后的复查延时（毫秒）。伪离开的成因是系统还没处理过 `ReleaseCapture` 之后的鼠标
/// 移动；那次移动由系统在下一次取输入时合成，通常在一帧（~16ms）内就被处理掉，80ms 足以
/// 让它落定，又短到人察觉不出「移开后气泡慢半拍才消失」。复查兜的是光标一次都没在气泡上
/// 移动就离开的情形——那时气泡收不到 `WM_MOUSEMOVE`，没有别的消息会来重挂跟踪。
const RECHECK_MS: u32 = 80;

/// 复查定时器的 id（`SetTimer` 按窗口区分，气泡窗口只有这一个定时器）。
const RECHECK_TIMER: usize = 1;

/// 判据表（设计 §7.6 B）。
fn menu_step(event: MenuEvent) -> MenuStep {
    match event {
        // 菜单开着时这次右键由轮询那一路负责；两路都认会请求两遍菜单。
        // 请求在途同理：同一次右键合成的那次已经发出了请求。
        MenuEvent::OwnRightDown {
            any_menu: false,
            in_flight: false,
        } => MenuStep::RequestMenu,
        MenuEvent::OwnRightDown { .. } => MenuStep::Ignore,
        MenuEvent::OutsidePress {
            right: true,
            own_menu: true,
            on_tip: true,
        } => MenuStep::RequestMenu,
        // 左键点气泡 / 点别处 / 关掉的是别人的菜单：菜单已由轮询关掉，气泡去留等 Closed 再定。
        MenuEvent::OutsidePress { .. } => MenuStep::Ignore,
        MenuEvent::Closed { on_tip: true } => MenuStep::Keep,
        MenuEvent::Closed { on_tip: false } => MenuStep::Hide,
        // 菜单开着时的离开是移向菜单，不隐藏（气泡去留等 Closed 再定）。
        MenuEvent::Leave { suppress: true, .. } => MenuStep::Ignore,
        // 光标仍在气泡上：伪离开，见模块文档；连续太多次则按真实离开处理。
        MenuEvent::Leave {
            on_tip: true,
            spurious,
            ..
        } if spurious < MAX_SPURIOUS_LEAVES => MenuStep::AwaitMove,
        // 伪离开到上限：系统始终不认光标在气泡上，宽限只会再绕一圈，直接隐藏。
        MenuEvent::Leave { on_tip: true, .. } => MenuStep::Hide,
        // 真实离开：宽限，给「穿过空隙回到候选行 / 回到气泡」留时间。
        MenuEvent::Leave { .. } => MenuStep::Grace,
        MenuEvent::Recheck { suppress: true, .. } => MenuStep::Ignore,
        MenuEvent::Recheck { on_tip: true, .. } => MenuStep::Retrack,
        MenuEvent::Recheck {
            on_anchor: true, ..
        } => MenuStep::Idle,
        MenuEvent::Recheck { .. } => MenuStep::Hide,
    }
}

/// 鼠标跟踪器：检测鼠标是否悬停在 tooltip 上（WM_MOUSELEAVE 触发时直接隐藏窗口）；
/// 右键弹出菜单（按段 / 按行复制、上屏，复制全部，截图此窗口）。
struct TooltipMouse {
    hwnd: HWND,
    mouse_over: Rc<Cell<bool>>,
    tracking: bool,
    /// 回送协调器的鼠标事件通道（右键请求菜单）。
    events: Sender<UiEvent>,
    /// 菜单打开期间抑制 WM_MOUSELEAVE 自动隐藏：右键弹出菜单后鼠标会移到菜单窗口上，
    /// 触发 WM_MOUSELEAVE，若不抑制 tooltip 会当场消失，菜单就指向一个已不存在的窗口。
    suppress_hide: Rc<Cell<bool>>,
    hits: Rc<RefCell<HitState>>,
    /// 最近一次发出菜单请求的时刻，协调器回 `SetTooltipMenuOpen(true)` 即清。「请求在途」
    /// 期间自己的右键不理：菜单轮询合成的那次先发出请求、菜单还没出现，同一次右键的真实
    /// 消息随后才到，不挡就请求两遍、菜单重弹。超过 `MENU_REPLY_TIMEOUT` 视为请求已丢。
    requested_at: Cell<Option<std::time::Instant>>,
    /// 窗口是否显示中（与 `Tooltip` 共享）：离开隐藏在这里执行，得同步它，否则
    /// `Tooltip::hide` 的幂等判断会以为窗口还开着。
    visible: Rc<Cell<bool>>,
    /// 「屏幕点处最上层是不是这个窗口」，默认 [`crate::window::window_at`]；测试换桩。
    window_at: fn(HWND, i32, i32) -> bool,
    /// 连续伪离开次数（见 [`MAX_SPURIOUS_LEAVES`]），真实 `WM_MOUSEMOVE` / 菜单关闭留下 /
    /// 隐藏时清零。
    spurious: u8,
    /// 复查定时器是否在走（见 [`RECHECK_MS`] / [`LEAVE_GRACE_MS`]）。
    recheck_pending: bool,
    /// 气泡所属候选行的屏幕矩形 `(left, top, right, bottom)`，宽限到期时判「光标回到了
    /// 候选行」用。由候选窗在显示气泡时给出。
    anchor: Option<(i32, i32, i32, i32)>,
}

impl TooltipMouse {
    /// 光标此刻是否在气泡上（问系统，不看 `mouse_over`）。
    fn cursor_on_tip(&self) -> bool {
        let mut p = POINT::default();
        if unsafe { GetCursorPos(&mut p) }.is_err() {
            return false;
        }
        (self.window_at)(self.hwnd, p.x, p.y)
    }

    /// 是否有菜单请求在途（发出后协调器尚未回应，且未超时）。
    fn request_in_flight(&self, now: std::time::Instant) -> bool {
        self.requested_at
            .get()
            .is_some_and(|at| now.duration_since(at) < crate::popup_menu::MENU_REPLY_TIMEOUT)
    }

    #[cfg(windows)]
    fn arm_leave(&self) {
        unsafe {
            use windows::Win32::UI::Input::KeyboardAndMouse::{
                TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
            };
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut t);
        }
    }
    #[cfg(not(windows))]
    fn arm_leave(&self) {}

    /// 光标在气泡上、离开跟踪却已失效时（菜单关闭后）重新挂上，之后移出照常隐藏。
    fn rearm(&mut self) {
        self.spurious = 0;
        self.retrack();
    }

    /// 挂离开跟踪（不动伪离开计数）。
    fn retrack(&mut self) {
        self.cancel_recheck();
        self.mouse_over.set(true);
        self.tracking = true;
        self.arm_leave();
    }

    /// 光标此刻是否在气泡所属的候选行上。
    fn cursor_on_anchor(&self) -> bool {
        let Some((l, t, r, b)) = self.anchor else {
            return false;
        };
        let mut p = POINT::default();
        if unsafe { GetCursorPos(&mut p) }.is_err() {
            return false;
        }
        p.x >= l && p.x < r && p.y >= t && p.y < b
    }

    /// 启动复查定时器（`ms` 后到期，见 [`RECHECK_MS`] / [`LEAVE_GRACE_MS`]）。
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn start_recheck(&mut self, ms: u32) {
        self.recheck_pending = true;
        #[cfg(windows)]
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::SetTimer;
            SetTimer(self.hwnd, RECHECK_TIMER, ms, None);
        }
    }

    /// 撤掉复查定时器（没在走时无操作）。
    fn cancel_recheck(&mut self) {
        if !self.recheck_pending {
            return;
        }
        self.recheck_pending = false;
        #[cfg(windows)]
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::KillTimer;
            let _ = KillTimer(self.hwnd, RECHECK_TIMER);
        }
    }

    /// 气泡已藏起（或将由调用方藏起）：跟踪相关状态归位。
    fn settle_hidden(&mut self) {
        self.cancel_recheck();
        self.spurious = 0;
        self.mouse_over.set(false);
        self.tracking = false;
    }

    /// 离开 / 复查判定为离开：就地隐藏窗口，并同步共享的显示态。
    ///
    /// 同时把悬停清成「无」：光标从候选进了气泡后，候选窗的悬停一直停在原候选上（交给气泡的
    /// 离开跟踪），气泡在这里自己收起时光标既不在气泡上、也不在所属候选行上。不清的话协调器
    /// 仍记着原候选，光标移回它时悬停「没变」、不重绘，气泡再也不出来。
    fn hide_now(&mut self) {
        self.settle_hidden();
        self.visible.set(false);
        let _ = self.events.send(UiEvent::Hover(-1));
        #[cfg(windows)]
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// 按屏幕坐标 `(sx, sy)` / 客户区坐标 `(cx, cy)` 请求右键菜单。
    fn request_menu(&self, (sx, sy): (i32, i32), (cx, cy): (i32, i32)) {
        self.requested_at.set(Some(std::time::Instant::now()));
        let hits = self.hits.borrow();
        let _ = self.events.send(UiEvent::RequestTooltipMenu {
            x: sx,
            y: sy,
            candidate: hits.candidate,
            hit: hits.hit_at(cx, cy),
            doc_fingerprint: hits.doc.fingerprint(),
        });
    }
}

impl WindowMouse for TooltipMouse {
    fn on_message(
        &mut self,
        _hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match msg {
            WM_MOUSEMOVE => {
                // 真实移动：系统已认得光标在气泡上，伪离开的复查不再需要。
                self.cancel_recheck();
                self.spurious = 0;
                self.mouse_over.set(true);
                if !self.tracking {
                    self.tracking = true;
                    self.arm_leave();
                }
                None
            }
            WM_MOUSELEAVE => {
                // 跟踪随这条消息失效；下一次 WM_MOUSEMOVE 重挂。
                self.tracking = false;
                let event = MenuEvent::Leave {
                    suppress: self.suppress_hide.get(),
                    on_tip: self.cursor_on_tip(),
                    spurious: self.spurious,
                };
                let step = menu_step(event);
                if !self.suppress_hide.get() {
                    tracing::info!("[tip-leave] 离开 {event:?} → {step:?}");
                }
                match step {
                    MenuStep::AwaitMove => {
                        self.spurious += 1;
                        self.mouse_over.set(true);
                        self.start_recheck(RECHECK_MS);
                    }
                    MenuStep::Grace => {
                        self.spurious = 0;
                        self.mouse_over.set(false);
                        self.start_recheck(LEAVE_GRACE_MS);
                    }
                    // 鼠标离开时直接隐藏（对齐 Go TooltipWindow WM_MOUSELEAVE 行为）。
                    MenuStep::Hide => self.hide_now(),
                    _ => self.mouse_over.set(false),
                }
                None
            }
            crate::sys::WM_TIMER if wparam.0 == RECHECK_TIMER => {
                if !self.recheck_pending {
                    return Some(LRESULT(0));
                }
                self.cancel_recheck();
                let event = MenuEvent::Recheck {
                    suppress: self.suppress_hide.get(),
                    on_tip: self.cursor_on_tip(),
                    on_anchor: self.cursor_on_anchor(),
                };
                let step = menu_step(event);
                tracing::info!(
                    "[tip-leave] 复查到期 {event:?} spurious={} → {step:?}",
                    self.spurious
                );
                match step {
                    MenuStep::Retrack => self.retrack(),
                    MenuStep::Hide => self.hide_now(),
                    MenuStep::Idle => {
                        self.mouse_over.set(false);
                        self.spurious = 0;
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN => {
                let event = MenuEvent::OwnRightDown {
                    any_menu: crate::popup_menu::menu_visible(),
                    in_flight: self.request_in_flight(std::time::Instant::now()),
                };
                if menu_step(event) != MenuStep::RequestMenu {
                    return None;
                }
                self.suppress_hide.set(true);
                let (sx, sy) = unsafe {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    (p.x, p.y)
                };
                self.request_menu((sx, sy), client_point(lparam));
                None
            }
            _ => None,
        }
    }
}

const FONT_PX: f32 = 13.0;

/// 候选基准字号的参照值（逻辑像素）：候选字号 = 它时气泡就是 [`FONT_PX`]。
/// 候选字号每比它大 / 小 1，气泡跟着大 / 小 1（相对偏移，不等于候选字号）。
const REF_CANDIDATE_PX: f32 = 18.0;
const BG: [u8; 4] = wind_theme::fallback::TOOLTIP_BG; // 深灰底（RGBA）
const FG: [u8; 4] = wind_theme::fallback::TOOLTIP_TEXT;

/// 提示气泡窗口
pub struct Tooltip {
    window: LayeredWindow,
    renderer: TextRenderer,
    scale: f32,
    /// 基准字号（逻辑像素）：`FONT_PX` + 主题 tooltip 节点的 `font_size` 偏移。
    base_logical: f32,
    /// 候选基准字号相对 [`REF_CANDIDATE_PX`] 的偏移，随候选字号设置变化。
    candidate_delta: f32,
    /// 窗口是否显示中（与 `TooltipMouse` 共享：离开隐藏在那边执行）。
    visible: Rc<Cell<bool>>,
    /// 最近一次显示的内容盒屏幕矩形 `(left, top, right, bottom)`，见 [`Self::content_rect`]。
    placed: Cell<(i32, i32, i32, i32)>,
    bg: [u8; 4],
    fg: [u8; 4],
    /// 主题位图背景 + z 层（jidian tooltip 吃九宫格 panel + 角标水印）。
    bg_image: Option<ViewImage>,
    bg_gradient: Option<crate::view::ViewGradient>,
    layers: Vec<ViewLayer>,
    /// 主题配置的软投影 / 边框 / 圆角（与候选窗一致化）。
    shadow: Option<crate::view::SoftShadow>,
    border: Option<([u8; 4], f32)>,
    radius: Option<f32>,
    /// 已应用主题（DPI 变化时按新缩放重解析几何）。
    theme: Option<wind_theme::Resolved>,
    /// 鼠标是否正悬停在 tooltip 上（由 TooltipMouse 更新）。
    /// hide() 遇到此标志时推迟隐藏，待 WM_MOUSELEAVE 自动触发后真正隐藏。
    mouse_over: Rc<Cell<bool>>,
    /// 右键菜单是否打开中（与 TooltipMouse 共享，供 set_menu_open 写入）。
    suppress_hide: Rc<Cell<bool>>,
    /// 当前内容的命中信息（与 TooltipMouse 共享）。
    hits: Rc<RefCell<HitState>>,
    /// 鼠标处理器本体：菜单关闭时要重挂它的离开跟踪、菜单外右键要借它请求菜单。
    mouse: Rc<RefCell<TooltipMouse>>,
    /// 当前内容的分段颜色（[`Self::set_doc`] 按当前主题现求，等于正文色的已丢弃）。
    runs: Vec<ColorRun>,
}

impl Tooltip {
    pub fn new(events: Sender<UiEvent>) -> Result<Self, String> {
        let scale = dpi_scale();
        let window = LayeredWindow::create(None, 120, 40, "WindInputTooltip")?;
        let renderer = TextRenderer::new("Microsoft YaHei UI", FONT_PX * scale)?;
        let mouse_over = Rc::new(Cell::new(false));
        let suppress_hide = Rc::new(Cell::new(false));
        let hits = Rc::new(RefCell::new(HitState::default()));
        let visible = Rc::new(Cell::new(false));
        // 注册鼠标跟踪：鼠标进入 tooltip 时保持可见；WM_MOUSELEAVE 触发时自动隐藏；右键弹出菜单。
        let mouse = Rc::new(RefCell::new(TooltipMouse {
            hwnd: window.hwnd(),
            mouse_over: mouse_over.clone(),
            tracking: false,
            events,
            suppress_hide: suppress_hide.clone(),
            hits: hits.clone(),
            requested_at: Cell::new(None),
            visible: visible.clone(),
            window_at: crate::window::window_at,
            spurious: 0,
            recheck_pending: false,
            anchor: None,
        }));
        window.register_mouse(mouse.clone());
        Ok(Self {
            window,
            renderer,
            scale,
            base_logical: FONT_PX,
            candidate_delta: 0.0,
            visible,
            placed: Cell::new((0, 0, 0, 0)),
            bg: BG,
            fg: FG,
            bg_image: None,
            bg_gradient: None,
            layers: Vec::new(),
            shadow: None,
            border: None,
            radius: None,
            theme: None,
            mouse_over,
            suppress_hide,
            hits,
            mouse,
            runs: Vec::new(),
        })
    }

    /// DPI 动态化：按显示点所在显示器实时取缩放，变化则更新字号并按新缩放重解析主题几何。
    fn ensure_scale(&mut self, x: i32, y: i32) {
        let sc = crate::dpi::scale_for_point(x, y);
        if (sc - self.scale).abs() > 0.01 {
            self.scale = sc;
            self.renderer.set_base_size(self.base_logical * sc);
            if let Some(t) = self.theme.clone() {
                self.set_theme(&t);
            }
        }
    }

    /// 加载拆字字根字体（PUA 字根字符渲染）。`family` 为 DWrite 家族名。失败仅日志，不影响普通提示。
    pub fn set_chaizi_font(&mut self, path: &str, family: &str) {
        if let Err(e) = self.renderer.set_chaizi_font(path, family) {
            tracing::warn!("加载拆字字根字体失败 ({path}): {e}");
        }
    }

    /// 候选基准字号（逻辑像素）变化时调用：气泡字号按它相对 [`REF_CANDIDATE_PX`] 的差值同步增减。
    pub fn set_candidate_font(&mut self, candidate_px: f32) {
        let delta = candidate_px - REF_CANDIDATE_PX;
        if (delta - self.candidate_delta).abs() < f32::EPSILON {
            return;
        }
        self.candidate_delta = delta;
        match self.theme.clone() {
            Some(t) => self.set_theme(&t),
            None => {
                self.base_logical = (FONT_PX + delta).max(8.0);
                self.renderer.set_base_size(self.base_logical * self.scale);
            }
        }
    }

    /// 应用主题（tooltip 底色/文字色 + 位图背景/层）。
    pub fn set_theme(&mut self, theme: &wind_theme::Resolved) {
        self.theme = Some(theme.clone());
        // 字号：基准 13 + 候选字号相对偏移 + 主题节点偏移。
        let node_off = theme.views.tooltip.as_ref().map_or(0.0, |n| n.font_size);
        self.base_logical = (FONT_PX + self.candidate_delta + node_off).max(8.0);
        self.renderer.set_base_size(self.base_logical * self.scale);
        // palette 兜底 → tooltip 节点覆盖（节点色已在 resolve 阶段合成 palette 默认）。
        self.bg = theme.color("tooltip_bg", BG);
        self.fg = theme.color("tooltip_text", FG);
        if let Some(node) = &theme.views.tooltip {
            if let Some(c) = node.bg_color {
                self.bg = c;
            }
            if let Some(c) = node.text_color {
                self.fg = c;
            }
            let s = self.scale;
            self.bg_image = crate::theme_assets::rv_image(theme, node.bg_image.as_ref(), s);
            self.bg_gradient = crate::theme_assets::rv_gradient(node.bg_gradient.as_ref());
            self.layers = crate::theme_assets::rv_layers(theme, &node.layers, s);
            self.shadow = crate::view::SoftShadow::build(
                node.shadow_offset_x,
                node.shadow_offset_y,
                node.shadow_blur,
                node.shadow_spread,
                node.shadow_spread_offset_x,
                node.shadow_spread_offset_y,
                node.shadow_color,
                s,
            );
            self.border = node.border_color.map(|c| {
                (
                    c,
                    node.border_width
                        .map(|d| d.resolve(s, 0.0))
                        .unwrap_or(s)
                        .max(1.0),
                )
            });
            self.radius = node.border_radius.map(|d| d.resolve(s, 0.0));
        } else {
            self.bg_image = None;
            self.bg_gradient = None;
            self.layers = Vec::new();
            self.shadow = None;
            self.border = None;
            self.radius = None;
        }
    }

    /// 渲染到 BGRA Vec（离屏化，不依赖 LayeredWindow）。
    /// 返回 `(bgra, w, h, cw, ch, ml, mt, mr, mb, has_shadow)`。
    fn render_to_bgra(
        &mut self,
        text: &str,
    ) -> (Vec<u8>, u32, u32, u32, u32, u32, u32, u32, u32, bool) {
        let s = self.scale;
        let mut tip = View::leaf(text, self.fg)
            .color_runs(self.runs.clone())
            .bg(self.bg)
            .pad(Edges::xy(8.0 * s, 4.0 * s))
            .text_align(Align::Center);
        if let Some((bc, bw)) = self.border {
            tip = tip.border(bc, bw);
        }
        tip.corner_radius = self.radius.unwrap_or(5.0 * s);
        if let Some(g) = &self.bg_gradient {
            tip = tip.bg_gradient(g.clone());
        }
        if let Some(img) = &self.bg_image {
            tip = tip.bg_image(img.clone());
        }
        if !self.layers.is_empty() {
            tip = tip.layers(self.layers.clone());
        }
        let (ml, mt, mr, mb) = self
            .shadow
            .as_ref()
            .map(|sh| sh.margins())
            .unwrap_or((0, 0, 0, 0));
        tip.layout(ml as f32, mt as f32, &self.renderer);
        let (w_f, h_f) = tip.measured_size();
        self.hits.borrow_mut().text_box = Some(text_box(&self.renderer, text, ml, mt, w_f, h_f));
        let cw = (w_f.ceil() as u32).max(24);
        let ch = (h_f.ceil() as u32).max(20);
        let w = cw + ml + mr;
        let h = ch + mt + mb;
        let n = (w * h * 4) as usize;
        let mut buf = vec![0u8; n];
        if let Some(sh) = &self.shadow {
            sh.paint(
                &mut buf,
                w,
                h,
                ml as f32,
                mt as f32,
                cw as f32,
                ch as f32,
                tip.corner_radius,
            );
        }
        tip.paint(&mut buf, w, h, &self.renderer);
        let has_shadow = self.shadow.is_some();
        (buf, w, h, cw, ch, ml, mt, mr, mb, has_shadow)
    }

    /// 渲染 golden 用：按显示路径画一帧，返回 `(缓冲, 宽, 高, 绘制调用记录)`。
    #[cfg(all(test, mock_text))]
    pub(crate) fn golden_frame(
        &mut self,
        doc: &Arc<TooltipDoc>,
    ) -> (Vec<u8>, u32, u32, Vec<String>) {
        let text = self.set_doc(doc, 0);
        let _ = self.renderer.take_draw_log();
        let (buf, w, h, ..) = self.render_to_bgra(&text);
        (buf, w, h, self.renderer.take_draw_log())
    }

    /// 渲染文本到窗口缓冲，返回内容尺寸和阴影 margin。
    /// 返回 `(cw, ch, ml, mt, mr, mb)`；失败返回 None（text 为空时调用方已拦截）。
    fn render_to_window(&mut self, text: &str) -> (u32, u32, u32, u32, u32, u32) {
        let (buf, w, h, cw, ch, ml, mt, mr, mb, _) = self.render_to_bgra(text);
        self.window.resize(w, h);
        {
            let wbuf = self.window.buffer_mut();
            wbuf[..(w * h * 4) as usize].copy_from_slice(&buf);
        }
        let _ = self.window.update();
        (cw, ch, ml, mt, mr, mb)
    }

    /// 横排模式：在候选行下方显示提示，下方不足时上翻到候选行上方。
    /// `anchor_top`/`anchor_bottom` 为候选行的屏幕上/下边界。
    ///
    /// `candidate` 是气泡所属候选的页内下标，右键时随菜单请求带回协调器。
    pub fn show(
        &mut self,
        doc: &Arc<TooltipDoc>,
        candidate: i32,
        x: i32,
        anchor_top: i32,
        anchor_bottom: i32,
    ) {
        let text = self.set_doc(doc, candidate);
        if text.is_empty() {
            self.hide();
            return;
        }
        self.ensure_scale(x, anchor_bottom);
        let (cw, ch, ml, mt, ..) = self.render_to_window(&text);
        // 内容盒按工作区钳位（下方优先，不足上翻到候选行上方）；窗口原点 = 内容锚点 − 左/上 margin。
        let (px, py) = clamp_to_work_area(x, anchor_top, anchor_bottom, cw, ch);
        self.window.show(px - ml as i32, py - mt as i32);
        self.placed.set((px, py, px + cw as i32, py + ch as i32));
        self.visible.set(true);
    }

    /// 竖排模式：在候选窗右侧显示提示，右侧空间不足时改显示在左侧。
    /// `win_left`/`win_right` 为候选窗左右边界（含阴影）屏幕坐标。
    /// `row_top`/`row_bottom` 为悬停候选行的屏幕上/下边界，tooltip 纵向对齐候选行。
    #[allow(clippy::too_many_arguments)]
    pub fn show_beside(
        &mut self,
        doc: &Arc<TooltipDoc>,
        candidate: i32,
        win_left: i32,
        win_right: i32,
        row_top: i32,
        row_bottom: i32,
    ) {
        let text = self.set_doc(doc, candidate);
        if text.is_empty() {
            self.hide();
            return;
        }
        self.ensure_scale(win_right, row_top);
        let (cw, ch, ml, mt, ..) = self.render_to_window(&text);
        let (px, py) = clamp_beside(win_left, win_right, row_top, row_bottom, cw, ch);
        self.window.show(px - ml as i32, py - mt as i32);
        self.placed.set((px, py, px + cw as i32, py + ch as i32));
        self.visible.set(true);
    }

    pub fn hide(&mut self) {
        if self.mouse_over.get() {
            // 鼠标正悬停在 tooltip 上，不立即隐藏；WM_MOUSELEAVE 触发后 TooltipMouse 会自动隐藏窗口。
            // 但跟踪没挂着时（伪离开后等移动），没有 WM_MOUSELEAVE 会来——光标一次都没在
            // 气泡上移动就离开，`mouse_over` 就永远是 true，气泡随候选窗收起也藏不掉。此时
            // 问真实光标。
            let mut m = self.mouse.borrow_mut();
            if m.tracking || m.cursor_on_tip() {
                return;
            }
            m.settle_hidden();
        } else {
            self.mouse.borrow_mut().cancel_recheck();
        }
        if self.visible.get() {
            self.window.hide();
            self.visible.set(false);
        }
    }

    /// 无条件隐藏：候选窗收起（组合结束、Esc、上屏、失焦、切走输入法、宿主渲染分流）后
    /// 气泡失去依附对象，光标停在它上面也得收。[`Self::hide`] 的推迟是给「悬停变了」这类
    /// 隐藏用的，这里连同跟踪、复查 / 宽限定时器一并归位。
    pub fn hide_force(&mut self) {
        self.mouse.borrow_mut().settle_hidden();
        if self.visible.get() {
            self.window.hide();
            self.visible.set(false);
        }
    }

    /// 气泡所属候选行的屏幕矩形 `(left, top, right, bottom)`（宽限到期判「回到了候选行」）。
    pub fn set_anchor(&mut self, rect: (i32, i32, i32, i32)) {
        self.mouse.borrow_mut().anchor = Some(rect);
    }

    /// 「是否显示中」的共享标志：候选窗的悬停宽限读 `tip_rect` 之前先问它（气泡自己收起时
    /// 候选窗那边不知道）。
    pub(crate) fn visible_flag(&self) -> Rc<Cell<bool>> {
        self.visible.clone()
    }

    /// 显示中的内容盒屏幕矩形 `(left, top, right, bottom)`；没显示为 `None`。候选窗的悬停
    /// 宽限据此判「光标正朝气泡去」。
    pub fn content_rect(&self) -> Option<(i32, i32, i32, i32)> {
        self.visible.get().then(|| self.placed.get())
    }

    /// 光标此刻是否在气泡上（窗口可见时才可能为真）。候选窗的悬停宽限据此判「已进入气泡」。
    pub fn cursor_on_tip_now(&self) -> bool {
        self.visible.get() && self.cursor_on_tip()
    }

    /// 本地窗口是否处于显示态（测试用；Windows 上问系统见 [`Self::is_visible`]）。
    #[cfg(test)]
    pub(crate) fn shown(&self) -> bool {
        self.visible.get()
    }

    /// 测试用：光标「在不在气泡上」的桩。
    #[cfg(test)]
    pub(crate) fn stub_on_tip(&self, on_tip: bool) {
        self.mouse.borrow_mut().window_at = if on_tip {
            |_, _, _| true
        } else {
            |_, _, _| false
        };
    }

    /// 测试用：模拟光标在气泡上移动（挂上离开跟踪、`mouse_over` 置位）。
    #[cfg(test)]
    pub(crate) fn simulate_move(&self) {
        self.mouse
            .borrow_mut()
            .on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
    }

    /// 是否处于「菜单打开中」的隐藏抑制（测试用）。
    #[cfg(test)]
    pub(crate) fn menu_suppressed(&self) -> bool {
        self.suppress_hide.get()
    }

    /// 记下当前内容（命中换算要用），求好分段颜色，返回要画的纯文本。
    ///
    /// 颜色在这里按**当前**主题现求：片段带的是角色 / 颜色引用，换主题、切明暗后重画即取到
    /// 新颜色。正文色兜底与 [`Self::set_theme`] 同源（palette `tooltip_text` → 节点文字色）。
    fn set_doc(&mut self, doc: &Arc<TooltipDoc>, candidate: i32) -> String {
        // 伪离开后等复查期间换了内容（光标移到别的候选，协调器重绘直接 show 新气泡，不经
        // `hide`）：光标已不在气泡上，复查到期会把新气泡当「离开」藏掉——就此归位。光标
        // 仍在气泡上、只是内容原地刷新的，复查照旧。
        {
            let mut m = self.mouse.borrow_mut();
            if m.recheck_pending && !m.cursor_on_tip() {
                m.settle_hidden();
            }
        }
        let styled = doc.to_styled();
        self.runs = match &self.theme {
            Some(t) => crate::span_runs::color_runs(
                t,
                t.views.tooltip.as_ref().unwrap_or(&RvNode::default()),
                true,
                wind_theme::TextState::Normal,
                t.color("tooltip_text", FG),
                &styled,
            ),
            None => Vec::new(),
        };
        let mut h = self.hits.borrow_mut();
        h.doc = Arc::clone(doc);
        h.candidate = candidate;
        h.text_box = None;
        styled.into_string()
    }

    /// 将当前渲染帧保存为 PNG 文件（截图用）。
    pub fn capture_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        self.window.capture_to_file(path)
    }

    /// 将当前渲染帧复制到剪贴板（截图用）。
    pub fn capture_to_clipboard(&self) -> Result<(), String> {
        self.window.capture_to_clipboard()
    }

    /// 窗口当前是否可见（查询 Win32 IsWindowVisible）。
    pub fn is_visible(&self) -> bool {
        #[cfg(windows)]
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(self.window.hwnd()).as_bool()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// 设置右键菜单打开状态：开启时抑制 WM_MOUSELEAVE 自动隐藏；关闭时光标仍在气泡上
    /// 就留下并重挂离开跟踪，否则立即隐藏（避免菜单关掉后 tooltip 永久赖着不走）。
    ///
    /// 「在不在气泡上」问真实光标而不看 `mouse_over`：菜单开着期间气泡收不到鼠标消息，
    /// 那个值停在鼠标移向菜单时的 false，用它判会把「点回气泡关菜单」误当成离开。
    ///
    /// 关闭是幂等的：抑制已解除时直接返回。协调器对同一次关闭可能下发不止一条
    /// `SetTooltipMenuOpen(false)`（`MenuClose` 与收菜单的其它路各发一条），每条都重挂一次离开
    /// 跟踪毫无意义；别人的菜单（工具栏 / 状态）关闭时也不该来动气泡。
    pub fn set_menu_open(&mut self, open: bool) {
        if !open && !self.suppress_hide.get() {
            return;
        }
        self.suppress_hide.set(open);
        if open {
            // 协调器已回应这次请求（菜单随后就到）。菜单开着期间气泡去留由菜单关闭时再定，
            // 伪离开的复查若还在走就撤掉——它到期时光标在菜单上，会被当成离开。
            let mut m = self.mouse.borrow_mut();
            m.requested_at.set(None);
            m.cancel_recheck();
            return;
        }
        // 已被收起（候选窗先收、菜单后收）的气泡不可能在光标下；桩测试里 window_at 不看
        // 可见性，这里显式挡住。
        let on_tip = self.visible.get() && self.cursor_on_tip();
        match menu_step(MenuEvent::Closed { on_tip }) {
            MenuStep::Keep => self.mouse.borrow_mut().rearm(),
            _ => {
                self.mouse_over.set(false);
                self.hide();
            }
        }
    }

    /// UI 循环收掉了一个可见菜单（协调器的 `HideMenu` / `HideCandidates`）：若开着的是气泡
    /// 的菜单，按「菜单已关闭」处理。协调器有几条收菜单的路不补发
    /// `SetTooltipMenuOpen(false)`，不在这里收口 `suppress_hide` 就会一直是 true——气泡从此
    /// 移出不隐藏、再右键也被当成「菜单开着」。
    ///
    /// 不会抢在「截图此窗口」之前把气泡藏掉：点菜单项时菜单已由它自己的 tick 收起，协调器
    /// 随后的 `HideMenu` 落在一个不可见的菜单上，UI 循环不会调到这里。
    pub fn on_menu_dismissed(&mut self) {
        if self.suppress_hide.get() {
            self.set_menu_open(false);
        }
    }

    /// 菜单轮询到的一次菜单外按下（菜单已随之关闭）：落在气泡上的右键 = 重新右键，
    /// 按新命中位置请求菜单；返回是否接手了这次按下。须在本线程处理协调器回来的
    /// `SetTooltipMenuOpen(false)` 之前调用——此时 `suppress_hide` 还是 true，它就是
    /// 「关掉的正是气泡的菜单」的证据。
    pub fn on_menu_outside_press(&mut self, x: i32, y: i32, right: bool) -> bool {
        let step = menu_step(MenuEvent::OutsidePress {
            right,
            own_menu: self.suppress_hide.get(),
            on_tip: (self.mouse.borrow().window_at)(self.window.hwnd(), x, y),
        });
        if step != MenuStep::RequestMenu {
            return false;
        }
        self.mouse_over.set(true);
        let (ox, oy) = self.window_origin();
        self.mouse.borrow().request_menu((x, y), (x - ox, y - oy));
        true
    }

    /// 光标此刻是否在气泡上。
    fn cursor_on_tip(&self) -> bool {
        self.mouse.borrow().cursor_on_tip()
    }

    /// 窗口左上角屏幕坐标（分层窗口无非客户区，客户区原点即窗口原点）。
    fn window_origin(&self) -> (i32, i32) {
        let mut r = RECT::default();
        match unsafe { GetWindowRect(self.window.hwnd(), &mut r) } {
            Ok(()) => (r.left, r.top),
            Err(_) => (0, 0),
        }
    }

    /// 横排 host-render：渲染到 BGRA buffer + 计算屏幕坐标，不操作 LayeredWindow。
    /// 返回 `(bgra, w, h, screen_x, screen_y, software_shadow)`；text 为空返回 None。
    #[cfg(windows)]
    pub fn render_frame(
        &mut self,
        doc: &Arc<TooltipDoc>,
        candidate: i32,
        x: i32,
        anchor_top: i32,
        anchor_bottom: i32,
    ) -> Option<(Vec<u8>, u32, u32, i32, i32, bool)> {
        let text = self.set_doc(doc, candidate);
        if text.is_empty() {
            return None;
        }
        self.ensure_scale(x, anchor_bottom);
        let (buf, w, h, cw, ch, ml, mt, _mr, _mb, has_shadow) = self.render_to_bgra(&text);
        let (px, py) = clamp_to_work_area(x, anchor_top, anchor_bottom, cw, ch);
        Some((buf, w, h, px - ml as i32, py - mt as i32, has_shadow))
    }

    /// 竖排 host-render：渲染到 BGRA buffer + 计算候选窗右侧/左侧坐标，不操作 LayeredWindow。
    #[cfg(windows)]
    #[allow(clippy::too_many_arguments)]
    pub fn render_frame_beside(
        &mut self,
        doc: &Arc<TooltipDoc>,
        candidate: i32,
        win_left: i32,
        win_right: i32,
        row_top: i32,
        row_bottom: i32,
    ) -> Option<(Vec<u8>, u32, u32, i32, i32, bool)> {
        let text = self.set_doc(doc, candidate);
        if text.is_empty() {
            return None;
        }
        self.ensure_scale(win_right, row_top);
        let (buf, w, h, cw, ch, ml, mt, _mr, _mb, has_shadow) = self.render_to_bgra(&text);
        let (px, py) = clamp_beside(win_left, win_right, row_top, row_bottom, cw, ch);
        Some((buf, w, h, px - ml as i32, py - mt as i32, has_shadow))
    }
}

impl Tooltip {
    /// Linux 外部宿主：渲染位图 + 落位规则（首选 / 备选点，addon 按工作区选定）。
    /// `row` 为悬停候选行的屏幕 `(left, top, right, bottom)`；`beside` 对应竖排 / 旋转的
    /// [`Self::render_frame_beside`]，否则对应横排的 [`Self::render_frame`]。
    #[cfg(ext_presenter)]
    pub(crate) fn render_overlay(
        &mut self,
        doc: &Arc<TooltipDoc>,
        candidate: i32,
        row: (i32, i32, i32, i32),
        beside: bool,
    ) -> Option<crate::overlay_host::Overlay> {
        use crate::overlay_host as ol;
        let text = self.set_doc(doc, candidate);
        if text.is_empty() {
            return None;
        }
        self.ensure_scale(row.0, row.3);
        let (buf, w, h, cw, ch, ml, mt, _mr, _mb, has_shadow) = self.render_to_bgra(&text);
        Some(ol::Overlay {
            buf,
            width: w,
            height: h,
            content_x: ml,
            content_y: mt,
            content_w: cw,
            content_h: ch,
            software_shadow: has_shadow,
            place: ol::tooltip_place(
                row,
                (
                    ol::to_host(cw as f32, self.scale) as u32,
                    ol::to_host(ch as f32, self.scale) as u32,
                ),
                beside,
            ),
            scale: self.scale,
        })
    }

    /// Linux 外部宿主：addon 报来「右键点在提示位图内 `(cx, cy)`」，按最近一次渲染换算成
    /// `RequestTooltipMenu` 要带的 `(候选页内下标, 命中, 指纹)`——与 Windows 气泡自己收到
    /// `WM_RBUTTONDOWN` 时（`TooltipMouse::request_menu`）同一套命中。
    #[cfg(ext_presenter)]
    pub(crate) fn menu_request_at(&self, cx: i32, cy: i32) -> (i32, Option<TooltipHit>, u64) {
        let hits = self.hits.borrow();
        (hits.candidate, hits.hit_at(cx, cy), hits.doc.fingerprint())
    }
}

/// 文本块在窗口里的矩形：与 `View::paint` 画叶节点文本的定位公式同一套——内容盒内
/// 水平居中（`Align::Center`）、垂直居中，左右 / 上下内边距对称，故内边距不必出现在式中。
/// `(ml, mt)` 是叶节点的排布原点（软阴影扩边），`(w_f, h_f)` 是叶节点测得尺寸。
fn text_box(renderer: &TextRenderer, text: &str, ml: u32, mt: u32, w_f: f32, h_f: f32) -> TextBox {
    let m = renderer.measure_text(text);
    let (x0, y0) = (ml as f32, mt as f32);
    TextBox {
        x: (x0 + (w_f - m.width) * 0.5).max(x0),
        y: (y0 + (h_f - m.height) * 0.5).max(y0),
        w: m.width,
        h: m.height,
        lines: text.split('\n').count(),
    }
}

fn dpi_scale() -> f32 {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGPIXELSY, ReleaseDC};
        unsafe {
            let hdc = GetDC(HWND::default());
            let dpi = GetDeviceCaps(hdc, LOGPIXELSY);
            ReleaseDC(HWND::default(), hdc);
            if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 }
        }
    }
    #[cfg(not(windows))]
    {
        1.0
    }
}

/// 竖排模式：tooltip 显示在候选窗**右侧**（空间不足时改左侧），纵向对齐悬停候选行。
/// `win_left`/`win_right` 为候选窗左右边界（含阴影）；`row_top`/`row_bottom` 为候选行上下边界。
#[cfg_attr(not(windows), allow(unused_variables, unused_mut))]
fn clamp_beside(
    win_left: i32,
    win_right: i32,
    row_top: i32,
    _row_bottom: i32,
    w: u32,
    h: u32,
) -> (i32, i32) {
    let gap = 4;
    let (mut px, mut py) = (win_right + gap, row_top);
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        unsafe {
            let pt = POINT {
                x: win_right,
                y: row_top,
            };
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let wa = mi.rcWork;
                let (wi, hi) = (w as i32, h as i32);
                // 右侧放不下则改左侧
                if px + wi > wa.right {
                    px = win_left - gap - wi;
                }
                // 左侧也越界则贴左边
                if px < wa.left {
                    px = wa.left;
                }
                // 纵向：对齐候选行顶，下方越界时上移
                if py + hi > wa.bottom {
                    py = wa.bottom - hi;
                }
                if py < wa.top {
                    py = wa.top;
                }
                return (px, py);
            }
        }
    }
    (px, py)
}

/// 钳位 tooltip 到工作区：默认候选行下方（anchor_bottom + gap）；下方放不下则上翻到候选行
/// **上方**（anchor_top − gap − h，让出整行高度避免遮挡候选）；左右越界贴边。
#[cfg_attr(not(windows), allow(unused_variables, unused_mut))]
fn clamp_to_work_area(x: i32, anchor_top: i32, anchor_bottom: i32, w: u32, h: u32) -> (i32, i32) {
    let gap = 2;
    let (mut nx, mut ny) = (x, anchor_bottom + gap);
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        unsafe {
            let pt = POINT {
                x,
                y: anchor_bottom,
            };
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let wa = mi.rcWork;
                let (wi, hi) = (w as i32, h as i32);
                // 下方放不下 → 上翻到候选行上方（让出整行高度，不遮候选）
                if ny + hi > wa.bottom {
                    ny = (anchor_top - gap - hi).max(wa.top);
                }
                if nx + wi > wa.right {
                    nx = wa.right - wi;
                }
                if nx < wa.left {
                    nx = wa.left;
                }
                if ny < wa.top {
                    ny = wa.top;
                }
                return (nx, ny);
            }
        }
    }
    (nx, ny)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wind_ui_types::{TooltipLine, TooltipSection};

    fn doc() -> TooltipDoc {
        let line = |t: &str, raw| TooltipLine {
            text: t.into(),
            raw,
        };
        TooltipDoc {
            sections: vec![
                TooltipSection {
                    title: Some("完整原文".into()),
                    inline: false,
                    // 一条原始行折成两条显示行
                    lines: vec![line("一二三四五", 0), line("六七", 0)],
                },
                TooltipSection {
                    title: Some("拼音".into()),
                    inline: false,
                    lines: vec![line("你：nǐ", 0), line("好：hǎo/hào", 1)],
                },
            ],
        }
    }

    fn tooltip() -> Tooltip {
        let (tx, _rx) = std::sync::mpsc::channel();
        Tooltip::new(tx).expect("创建气泡窗口（非 Windows 下是内存桩）")
    }

    /// ★ 外观零回归的约束在「画的是同一串文本」：渲染路径没改（仍是 `render_to_bgra(&str)`
    /// 画一个叶节点），故只要 `TooltipDoc` 扁平化出的文本与旧版逐字节相同，像素就相同。
    /// 像素本身不在这里比——同一串文本自己比自己证明不了任何事。
    #[test]
    fn doc_flattens_to_the_legacy_string() {
        let legacy = "[完整原文]\n一二三四五\n六七\n[拼音]\n你：nǐ\n好：hǎo/hào";
        assert_eq!(doc().to_plain_text(), legacy);
    }

    /// 命中换算「按行数均分」的前提：多行文本的高度 = 行数 × 单行高度，含 CJK 与 emoji 行
    /// （渲染器行距钉成 UNIFORM，回退字体再高也不撑高行框）。只有真 DirectWrite 能验证；
    /// mock 后端的高度本就是按行数算的。
    #[cfg(windows)]
    #[test]
    fn multiline_height_is_lines_times_line_height() {
        let r = TextRenderer::new("Microsoft YaHei UI", FONT_PX).expect("DirectWrite");
        let one = r.measure_text("A").height;
        for text in [
            "A\n你好\n😀",
            "😀\nA",
            "你\n好\n吗\n👨\u{200D}👩\u{200D}👧",
            "[拼音]\n你：nǐ\n好：hǎo/hào",
        ] {
            let n = text.split('\n').count() as f32;
            let h = r.measure_text(text).height;
            assert!(
                (h - one * n).abs() < 0.5,
                "{text:?}: 高 {h}，应为 {n} × {one}"
            );
        }
    }

    /// 命中换算：标题行 / 内容行 / 折行后的第二条显示行 / 内边距。
    ///
    /// 在非 Windows（mock 渲染器）下只验证换算的算术：矩形、按行均分、行号到 `(段, 原始行)`
    /// 的映射。「每行真的等高」由上面的 `multiline_height_is_lines_times_line_height` 在
    /// Windows 上兜底。
    #[test]
    fn hit_maps_client_point_to_section_and_raw_line() {
        let mut t = tooltip();
        let text = t.set_doc(&Arc::new(doc()), 3);
        let _ = t.render_to_bgra(&text);
        let h = t.hits.borrow();
        let b = h.text_box.expect("渲染后应记下文本块");
        assert_eq!(b.lines, 6);
        let line_h = b.h / 6.0;
        let at = |i: usize| {
            let y = (b.y + line_h * (i as f32 + 0.5)) as i32;
            let x = (b.x + b.w * 0.5) as i32;
            b.line_at(x, y).and_then(|l| h.doc.hit_at_line(l))
        };
        let hit = |section, raw_line| Some(TooltipHit { section, raw_line });
        assert_eq!(at(0), hit(0, None), "标题行");
        assert_eq!(at(1), hit(0, Some(0)));
        assert_eq!(at(2), hit(0, Some(0)), "折行的第二条显示行指回同一原始行");
        assert_eq!(at(3), hit(1, None));
        assert_eq!(at(5), hit(1, Some(1)));
        // 内边距：文本块上方、左侧都不算命中。
        assert_eq!(b.line_at(b.x as i32, (b.y - 1.0) as i32), None);
        assert_eq!(b.line_at((b.x - 1.0) as i32, (b.y + 1.0) as i32), None);
        assert_eq!(b.line_at(b.x as i32, (b.y + b.h + 1.0) as i32), None);
        assert_eq!(h.candidate, 3);
    }

    /// 菜单交互时序的判据表（设计 §7.6 B）。
    #[test]
    fn menu_step_table() {
        use MenuEvent::*;
        use MenuStep::*;
        let press = |right, own_menu, on_tip| OutsidePress {
            right,
            own_menu,
            on_tip,
        };
        // 首次右键：请求菜单；任何菜单开着时自己收到的右键不理（轮询那一路负责）。
        let own = |any_menu, in_flight| OwnRightDown {
            any_menu,
            in_flight,
        };
        assert_eq!(menu_step(own(false, false)), RequestMenu);
        assert_eq!(menu_step(own(true, false)), Ignore);
        assert_eq!(menu_step(own(false, true)), Ignore, "请求在途");
        // 气泡菜单开着、右键点在气泡另一处：重新请求菜单。
        assert_eq!(menu_step(press(true, true, true)), RequestMenu);
        // 左键点气泡：只关菜单，气泡去留由 Closed 决定。
        assert_eq!(menu_step(press(false, true, true)), Ignore);
        // 右键点在气泡外 / 关掉的是工具栏、状态等别人的菜单：不归气泡管。
        assert_eq!(menu_step(press(true, true, false)), Ignore);
        assert_eq!(menu_step(press(true, false, true)), Ignore);
        // 菜单关闭：光标在气泡上就留下，否则隐藏。
        assert_eq!(menu_step(Closed { on_tip: true }), Keep);
        assert_eq!(menu_step(Closed { on_tip: false }), Hide);
        // 离开：菜单开着时不理；光标仍在气泡上是伪离开；否则隐藏。
        let leave = |suppress, on_tip, spurious| Leave {
            suppress,
            on_tip,
            spurious,
        };
        assert_eq!(menu_step(leave(true, true, 0)), Ignore);
        assert_eq!(menu_step(leave(true, false, 0)), Ignore);
        assert_eq!(menu_step(leave(false, true, 0)), AwaitMove);
        assert_eq!(menu_step(leave(false, false, 0)), Grace, "真实离开先宽限");
        // 伪离开连续到上限：按真实离开处理，免得「补挂 → 伪离开」无限循环。
        assert_eq!(
            menu_step(leave(false, true, MAX_SPURIOUS_LEAVES - 1)),
            AwaitMove
        );
        assert_eq!(menu_step(leave(false, true, MAX_SPURIOUS_LEAVES)), Hide);
        // 复查定时器到期：光标仍在气泡上就补挂跟踪，否则隐藏。
        let recheck = |suppress, on_tip, on_anchor| Recheck {
            suppress,
            on_tip,
            on_anchor,
        };
        assert_eq!(menu_step(recheck(false, true, false)), Retrack);
        assert_eq!(menu_step(recheck(false, true, true)), Retrack);
        assert_eq!(menu_step(recheck(false, false, false)), Hide);
        // 宽限到期时光标回到了气泡所属的候选行：留下。
        assert_eq!(menu_step(recheck(false, false, true)), Idle);
        // 抑制中（光标没动就右键了气泡，菜单请求已发出）：复查不定去留。
        assert_eq!(menu_step(recheck(true, true, false)), Ignore);
        assert_eq!(menu_step(recheck(true, false, false)), Ignore);
    }

    fn tooltip_with_rx(on_tip: bool) -> (Tooltip, std::sync::mpsc::Receiver<UiEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let t = Tooltip::new(tx).expect("mock 气泡");
        t.mouse.borrow_mut().window_at = if on_tip {
            |_, _, _| true
        } else {
            |_, _, _| false
        };
        (t, rx)
    }

    fn own_right_down(t: &Tooltip) {
        t.mouse
            .borrow_mut()
            .on_message(HWND::default(), WM_RBUTTONDOWN, WPARAM(0), LPARAM(0));
    }

    fn menu_requested(rx: &std::sync::mpsc::Receiver<UiEvent>) -> bool {
        rx.try_iter()
            .any(|e| matches!(e, UiEvent::RequestTooltipMenu { .. }))
    }

    /// 任何菜单可见时气泡自己的 `WM_RBUTTONDOWN` 不再请求菜单：若 Windows 真把这次按下投到
    /// 气泡上，它与菜单轮询转来的那次是同一次右键，两路都认会请求两遍菜单。
    #[test]
    fn own_right_down_requests_menu_only_without_visible_menu() {
        let (t, rx) = tooltip_with_rx(true);
        crate::popup_menu::set_menu_visible(false);
        own_right_down(&t);
        assert!(menu_requested(&rx));
        assert!(t.suppress_hide.get(), "右键即抑制离开隐藏");
        // 协调器回应（请求不再在途），菜单随后可见。
        t.mouse.borrow().requested_at.set(None);
        crate::popup_menu::set_menu_visible(true);
        own_right_down(&t);
        crate::popup_menu::set_menu_visible(false);
        assert!(!menu_requested(&rx), "菜单开着时不再请求");
    }

    /// 请求在途时自己的右键不理：菜单轮询合成的那次已发出请求、菜单还没出现，同一次右键的
    /// 真实消息随后才到。协调器回应（`SetTooltipMenuOpen(true)`）或超时后恢复。
    #[test]
    fn own_right_down_ignored_while_request_in_flight() {
        let (mut t, rx) = tooltip_with_rx(true);
        crate::popup_menu::set_menu_visible(false);
        t.set_menu_open(true);
        assert!(t.on_menu_outside_press(10, 10, true), "合成那一路发出请求");
        assert!(menu_requested(&rx));
        own_right_down(&t);
        assert!(!menu_requested(&rx), "同一次右键的真实消息不得再请求");
        // 协调器回应 → 在途清掉；菜单关闭后再右键照常。
        t.set_menu_open(true);
        t.set_menu_open(false);
        own_right_down(&t);
        assert!(menu_requested(&rx));
        // 请求被丢：超过回应时限后不再挡。
        let stale = std::time::Instant::now() - crate::popup_menu::MENU_REPLY_TIMEOUT * 2;
        t.mouse.borrow().requested_at.set(Some(stale));
        own_right_down(&t);
        assert!(menu_requested(&rx), "超时后恢复");
    }

    /// 气泡菜单开着时在气泡上再右键（气泡自己收不到，由菜单轮询转来）：按新位置重新请求。
    #[test]
    fn outside_right_press_on_tip_reopens_own_menu() {
        let (mut t, rx) = tooltip_with_rx(true);
        t.set_menu_open(true);
        assert!(t.on_menu_outside_press(10, 10, true));
        assert!(menu_requested(&rx));
        // 左键：只关菜单，不请求。
        assert!(!t.on_menu_outside_press(10, 10, false));
        assert!(!menu_requested(&rx));
    }

    /// 开着的是工具栏 / 状态等别人的菜单：右键气泡只关那个菜单，不弹气泡菜单。
    #[test]
    fn outside_right_press_with_foreign_menu_is_ignored() {
        let (mut t, rx) = tooltip_with_rx(true);
        assert!(!t.on_menu_outside_press(10, 10, true));
        assert!(!menu_requested(&rx));
    }

    /// 气泡菜单被打字 / 失焦等路径收掉（协调器只发 `HideMenu`）：`on_menu_dismissed` 收口
    /// 抑制标志，此后气泡右键照常有效，别人的菜单开着时也不再被误认成气泡的菜单。
    #[test]
    fn dismissed_menu_clears_suppress_and_right_click_still_works() {
        let (mut t, rx) = tooltip_with_rx(true);
        t.visible.set(true);
        crate::popup_menu::set_menu_visible(false);
        own_right_down(&t);
        t.set_menu_open(true);
        let _ = menu_requested(&rx);
        t.on_menu_dismissed();
        assert!(!t.suppress_hide.get(), "抑制标志不得残留");
        assert!(t.mouse_over.get(), "光标在气泡上：留下并重挂跟踪");
        own_right_down(&t);
        assert!(menu_requested(&rx), "再右键仍然请求菜单");
        // 模拟随后换成工具栏菜单开着：标志若残留，这次右键会被当成重开气泡菜单。
        t.on_menu_dismissed();
        assert!(!t.on_menu_outside_press(10, 10, true));
    }

    /// 菜单关闭时光标不在气泡上：`mouse_over` 即便残留 true 也要隐藏，并解除抑制。
    #[test]
    fn menu_closed_off_tip_hides_even_with_stale_mouse_over() {
        let (mut t, _rx) = tooltip_with_rx(false);
        t.visible.set(true);
        t.set_menu_open(true);
        t.mouse_over.set(true);
        t.set_menu_open(false);
        assert!(!t.suppress_hide.get());
        assert!(!t.mouse_over.get());
        assert!(!t.shown());
    }

    /// 菜单关闭时光标在气泡上：留下。
    #[test]
    fn menu_closed_on_tip_keeps_it() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        assert!(t.shown() && t.mouse_over.get());
    }

    fn mouse_leave(t: &Tooltip) {
        t.mouse
            .borrow_mut()
            .on_message(HWND::default(), WM_MOUSELEAVE, WPARAM(0), LPARAM(0));
    }

    /// 菜单关闭、留下气泡并重挂跟踪后，系统当场投来的离开是伪的（光标仍在气泡上）：气泡
    /// 留下，跟踪清掉等下一次移动重挂。靶机实测左键点气泡关菜单时正是这条把气泡藏掉。
    #[test]
    fn spurious_leave_after_menu_close_keeps_tip() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        assert!(t.shown(), "伪离开不隐藏");
        assert!(t.mouse_over.get(), "光标仍在气泡上");
        assert!(!t.mouse.borrow().tracking, "跟踪失效，等下一次移动重挂");
        // 下一次真实移动重挂跟踪。
        t.mouse
            .borrow_mut()
            .on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
        assert!(t.mouse.borrow().tracking);
    }

    /// 真离开（光标已不在气泡上）：先宽限、不当场藏；到期光标既不在气泡也不在候选行上则
    /// 隐藏，且 `visible` 同步为 false——此后 `hide()` 按「已隐藏」幂等跳过，再显示时照常
    /// 置回 true。
    #[test]
    fn real_leave_hides_and_syncs_visible() {
        let (mut t, _rx) = tooltip_with_rx(false);
        t.visible.set(true);
        t.mouse_over.set(true);
        mouse_leave(&t);
        assert!(t.shown(), "宽限期内不藏");
        assert!(t.mouse.borrow().recheck_pending, "离开即开宽限");
        recheck_fires(&t);
        assert!(!t.shown(), "离开隐藏须同步 visible");
        assert!(!t.mouse_over.get());
        assert!(
            matches!(_rx.try_recv(), Ok(UiEvent::Hover(-1))),
            "气泡自己收起时悬停清成「无」，移回原候选才会重新出气泡"
        );
        t.hide();
        assert!(!t.shown());
        t.show(&Arc::new(doc()), 0, 10, 10, 20);
        assert!(t.shown());
    }

    /// 菜单开着时的离开（移向菜单）：不隐藏，光标在不在气泡上都一样。
    #[test]
    fn leave_while_menu_open_is_ignored() {
        for on_tip in [false, true] {
            let (mut t, _rx) = tooltip_with_rx(on_tip);
            t.visible.set(true);
            t.set_menu_open(true);
            mouse_leave(&t);
            assert!(t.shown());
            assert!(!t.mouse_over.get());
        }
    }

    fn set_on_tip(t: &Tooltip, on_tip: bool) {
        t.mouse.borrow_mut().window_at = if on_tip {
            |_, _, _| true
        } else {
            |_, _, _| false
        };
    }

    fn recheck_fires(t: &Tooltip) {
        t.mouse.borrow_mut().on_message(
            HWND::default(),
            crate::sys::WM_TIMER,
            WPARAM(RECHECK_TIMER),
            LPARAM(0),
        );
    }

    /// 菜单关闭留下气泡、随即伪离开，而用户没在气泡上移动一下就把光标移走了：气泡收不到
    /// `WM_MOUSEMOVE`，`mouse_over` 停在 true。此后候选窗收起 / 悬停换走调 `hide()` 必须
    /// 问真实光标把它藏掉，不能按「鼠标在上面」一直推迟。
    #[test]
    fn hide_after_spurious_leave_checks_real_cursor() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        assert!(t.shown() && t.mouse_over.get(), "伪离开留下");
        // 光标仍在气泡上：照旧推迟。
        t.hide();
        assert!(t.shown(), "光标还在气泡上，推迟隐藏");
        set_on_tip(&t, false);
        t.hide();
        assert!(!t.shown(), "光标已离开，hide 不得被陈旧的 mouse_over 挡住");
        assert!(!t.mouse_over.get());
        assert!(!t.mouse.borrow().recheck_pending, "复查随隐藏撤掉");
    }

    /// 伪离开即启动复查；到期时光标仍在气泡上 → 补挂跟踪、气泡留下。
    #[test]
    fn recheck_on_tip_retracks() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        assert!(t.mouse.borrow().recheck_pending, "伪离开启动复查");
        recheck_fires(&t);
        let m = t.mouse.borrow();
        assert!(m.tracking && !m.recheck_pending);
        assert!(t.shown());
    }

    /// 复查到期时光标已不在气泡上 → 隐藏（这条路不依赖任何人再调 `hide()`）。
    #[test]
    fn recheck_off_tip_hides() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        set_on_tip(&t, false);
        recheck_fires(&t);
        assert!(!t.shown());
        assert!(!t.mouse_over.get());
    }

    /// 真实移动先到：复查撤掉，到期消息（若已在队列里）也不再动气泡。
    #[test]
    fn real_move_cancels_recheck() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        t.mouse
            .borrow_mut()
            .on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
        assert!(!t.mouse.borrow().recheck_pending);
        set_on_tip(&t, false);
        recheck_fires(&t);
        assert!(t.shown(), "已撤掉的复查不得再隐藏");
    }

    /// 系统始终认定光标不在气泡上（补挂一次、伪离开一次）：到上限后按真实离开隐藏，不无限循环。
    #[test]
    fn spurious_leaves_are_capped() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        for i in 0..MAX_SPURIOUS_LEAVES {
            mouse_leave(&t);
            assert!(t.shown(), "第 {} 条伪离开仍留下", i + 1);
            recheck_fires(&t);
            assert!(t.mouse.borrow().tracking, "复查补挂");
        }
        mouse_leave(&t);
        assert!(!t.shown(), "超过上限按真实离开处理");
    }

    /// 气泡菜单再次打开（菜单开着时右键气泡另一处）：伪离开的复查撤掉——它到期时光标在菜单上。
    #[test]
    fn menu_reopen_cancels_recheck() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        t.set_menu_open(true);
        assert!(!t.mouse.borrow().recheck_pending);
        set_on_tip(&t, false);
        recheck_fires(&t);
        assert!(t.shown());
    }

    /// 伪离开等复查期间光标移到另一个候选，协调器重绘直接 `show` 新内容（不经 `hide`）：
    /// 复查到期时光标在候选窗上，不得把刚显示的新气泡当「离开」藏掉。
    #[test]
    fn probe_show_after_spurious_leave_survives_recheck() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        set_on_tip(&t, false);
        t.show(&Arc::new(doc()), 1, 10, 10, 20);
        recheck_fires(&t);
        assert!(t.shown(), "新气泡不得被旧复查藏掉");
    }

    /// 内容原地刷新（光标仍在气泡上）：复查照旧在走。
    #[test]
    fn show_on_tip_keeps_recheck() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        t.show(&Arc::new(doc()), 0, 10, 10, 20);
        assert!(t.mouse.borrow().recheck_pending);
    }

    /// 光标没动就右键气泡：请求发出、抑制已开，协调器回应晚于复查时复查不得把气泡藏掉。
    #[test]
    fn recheck_during_menu_request_is_ignored() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        crate::popup_menu::set_menu_visible(false);
        own_right_down(&t);
        set_on_tip(&t, false);
        recheck_fires(&t);
        assert!(t.shown());
    }

    /// 伪离开计数在真实移动时清零（不经菜单关闭的 rearm）：此后又能再容忍满额的伪离开。
    #[test]
    fn real_move_resets_spurious_count() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        t.mouse
            .borrow_mut()
            .on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), LPARAM(0));
        for i in 0..MAX_SPURIOUS_LEAVES {
            mouse_leave(&t);
            assert!(t.shown(), "移动后第 {} 条伪离开仍留下", i + 1);
            recheck_fires(&t);
        }
    }

    /// `mouse_over` 已清（如菜单关闭判定隐藏）时 `hide()` 也撤掉复查，免得它到期后再动气泡。
    #[test]
    fn hide_without_mouse_over_cancels_recheck() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        assert!(t.mouse.borrow().recheck_pending);
        t.mouse_over.set(false);
        t.hide();
        assert!(!t.mouse.borrow().recheck_pending);
    }

    /// 抑制未开（关闭的是工具栏 / 状态等别人的菜单）时 `SetTooltipMenuOpen(false)` 不动气泡：
    /// 光标不在气泡上也不隐藏、不改 `mouse_over` / 跟踪。
    #[test]
    fn foreign_menu_close_leaves_tip_alone() {
        let (mut t, _rx) = tooltip_with_rx(false);
        t.visible.set(true);
        t.mouse_over.set(true);
        t.mouse.borrow_mut().tracking = true;
        t.set_menu_open(false);
        assert!(t.shown());
        assert!(t.mouse_over.get());
        assert!(t.mouse.borrow().tracking);
    }

    /// 从气泡移回它所属的候选行（穿过空隙）：宽限到期时光标在候选行上 → 气泡留下，不挂
    /// 跟踪（此后去留跟着候选悬停走）。mock 光标恒在 (0,0)。
    #[test]
    fn grace_expiry_on_anchor_keeps_tip() {
        let (mut t, _rx) = tooltip_with_rx(false);
        t.visible.set(true);
        t.set_anchor((-5, -5, 5, 5));
        t.mouse_over.set(true);
        mouse_leave(&t);
        recheck_fires(&t);
        assert!(t.shown(), "回到候选行：气泡留下");
        assert!(_rx.try_recv().is_err(), "回到候选行不清悬停");
        let m = t.mouse.borrow();
        assert!(!t.mouse_over.get() && !m.tracking && !m.recheck_pending);
    }

    /// 宽限期内候选侧决定换走（悬停变了 → `hide()`）：当场隐藏并撤掉宽限。
    #[test]
    fn hide_during_grace_hides_now() {
        let (mut t, _rx) = tooltip_with_rx(false);
        t.visible.set(true);
        t.mouse_over.set(true);
        mouse_leave(&t);
        t.hide();
        assert!(!t.shown());
        assert!(!t.mouse.borrow().recheck_pending);
    }

    /// 强制隐藏：光标就在气泡上、跟踪挂着也收；跟踪 / 复查 / 计数一并归位。对照 `hide()`
    /// 在同样条件下推迟。
    #[test]
    fn hide_force_ignores_cursor_on_tip() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.simulate_move();
        t.hide();
        assert!(t.shown(), "对照：悬停类隐藏推迟");
        t.set_menu_open(true);
        t.set_menu_open(false);
        mouse_leave(&t);
        assert!(t.mouse.borrow().recheck_pending);
        t.hide_force();
        assert!(!t.shown());
        let m = t.mouse.borrow();
        assert!(!t.mouse_over.get() && !m.tracking && !m.recheck_pending && m.spurious == 0);
    }

    /// 候选窗先收（强制隐藏）、菜单后收（`HideCandidates` 的顺序）：已收起的气泡不再被
    /// 「光标在气泡上 → 留下」重挂跟踪。
    #[test]
    fn menu_close_after_force_hide_does_not_rearm() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.hide_force();
        t.on_menu_dismissed();
        assert!(!t.shown());
        assert!(!t.mouse_over.get() && !t.mouse.borrow().tracking);
    }

    /// 同一次关闭收到第二条 `SetTooltipMenuOpen(false)`：幂等，不再重挂跟踪。
    #[test]
    fn repeated_menu_close_does_not_rearm() {
        let (mut t, _rx) = tooltip_with_rx(true);
        t.visible.set(true);
        t.set_menu_open(true);
        t.set_menu_open(false);
        assert!(t.mouse.borrow().tracking);
        // 伪离开清掉跟踪；随后到达的重复关闭不得再挂（再挂只会再招一条伪离开）。
        mouse_leave(&t);
        t.set_menu_open(false);
        assert!(!t.mouse.borrow().tracking, "重复关闭不重挂");
        assert!(t.shown());
    }

    /// 主题 `[tooltip] font_size` 是相对基准 13 的有符号偏移（与主题编辑器预览同语义）。
    #[test]
    fn theme_font_size_offset_applies_to_tooltip() {
        let mut t = tooltip();
        let base = t.renderer.base_size();
        let mut theme = wind_theme::Resolved::default();
        theme.views.tooltip = Some(wind_theme::RvNode {
            font_size: 4.0,
            ..Default::default()
        });
        t.set_theme(&theme);
        assert_eq!(t.renderer.base_size(), base + 4.0 * t.scale);
        // 换回无偏移的主题要回到基准，不能把上一份主题的字号粘住。
        t.set_theme(&wind_theme::Resolved::default());
        assert_eq!(t.renderer.base_size(), base);
    }

    /// 候选字号比参照大 / 小多少，气泡就跟着大 / 小多少，且与主题节点偏移叠加。
    #[test]
    fn candidate_font_delta_applies_to_tooltip() {
        let mut t = tooltip();
        let base = t.renderer.base_size();
        t.set_candidate_font(REF_CANDIDATE_PX + 6.0);
        assert_eq!(t.renderer.base_size(), base + 6.0 * t.scale);
        let mut theme = wind_theme::Resolved::default();
        theme.views.tooltip = Some(wind_theme::RvNode {
            font_size: -2.0,
            ..Default::default()
        });
        t.set_theme(&theme);
        assert_eq!(t.renderer.base_size(), base + 4.0 * t.scale);
        t.set_candidate_font(REF_CANDIDATE_PX);
        t.set_theme(&wind_theme::Resolved::default());
        assert_eq!(t.renderer.base_size(), base);
    }

    #[test]
    fn client_point_is_signed_16_bit_pairs() {
        assert_eq!(client_point(LPARAM((20 << 16) | 10)), (10, 20));
        assert_eq!(client_point(LPARAM(0xFFFF_FFFF)), (-1, -1));
    }

    /// 片段按主题求色后接到气泡叶子：内联色那段走 draw_runs，气泡里名字先查 `tooltip_<名>`。
    #[cfg(mock_text)]
    #[test]
    fn inline_color_reaches_the_tooltip_leaf() {
        use wind_ui_types::{SpanStyle, StyledText};
        let mut t = tooltip();
        let mut theme = wind_theme::Resolved::default();
        theme.palette.insert("error".into(), [1, 1, 1, 255]);
        theme.palette.insert("tooltip_error".into(), [2, 2, 2, 255]);
        t.set_theme(&theme);
        let mut text = StyledText::from("你：");
        text.push(
            "nǐ",
            &SpanStyle {
                color: Some(std::sync::Arc::new(wind_theme::InlineColor::parse("error"))),
                ..Default::default()
            },
        );
        let doc = TooltipDoc {
            sections: vec![TooltipSection {
                title: None,
                inline: false,
                lines: vec![TooltipLine { text, raw: 0 }],
            }],
        };
        let (_, _, _, log) = t.golden_frame(&Arc::new(doc));
        assert_eq!(log.len(), 1);
        assert!(
            log[0].starts_with("draw_runs ")
                && log[0].contains("ColorRun { start: 6, end: 9, rgba: [2, 2, 2, 255] }"),
            "{}",
            log[0]
        );
    }

    /// 非出厂测试主题的气泡角色色：段名装饰与字面走 title、段名里的变量回落 title、readings 自有色。
    #[cfg(mock_text)]
    #[test]
    fn theme_roles_color_the_tooltip() {
        use wind_ui_types::{SpanStyle, StyledText};
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let theme = wind_theme::load_resolved_dirs(
            &[
                root.join("../wind-theme/testdata/themes"),
                root.join("../../../data/themes"),
            ],
            "span-roles",
            false,
        )
        .unwrap();
        let mut t = tooltip();
        t.set_theme(&theme);
        let title_role = |s: &str, role| {
            let mut x = StyledText::new();
            x.push(
                s,
                &SpanStyle {
                    role: Some(role),
                    in_title: true,
                    ..Default::default()
                },
            );
            x
        };
        let mut title = title_role("编码(", "title");
        title.append(&title_role("五笔", "code_source"));
        // `_base` 没配 dict：段名里的它回落 title——渲染层把 in_title 透传到求色的端到端覆盖。
        title.append(&title_role("释", "dict"));
        let mut line = StyledText::new();
        line.push(
            "hǎo",
            &SpanStyle {
                role: Some("readings"),
                ..Default::default()
            },
        );
        let doc = TooltipDoc {
            sections: vec![TooltipSection {
                title: Some(title),
                inline: false,
                lines: vec![TooltipLine { text: line, raw: 0 }],
            }],
        };
        let (_, _, _, log) = t.golden_frame(&Arc::new(doc));
        let accent = theme.palette["accent"];
        let info = theme.palette["tooltip_info"];
        // 整块文字是 "[编码(五笔释]\nhǎo"（样例段名只拼了「编码(」「五笔」「释」，没有右括号）：
        // 装饰 `[` 与字面 `编码(` 同为 title 角色、合成一段；`五笔` 是段名里的变量，有自己的角色色
        // （继承 `_base` 的 code_source = tooltip_info，§18）就不回落 title；`释`（dict，没配色）
        // 回落 title；装饰 `]` 是 title；换行不着色；readings 用本主题自有色（压过 `_base` 的）。
        let want = format!(
            "runs=[ColorRun {{ start: 0, end: 8, rgba: {accent:?} }}, ColorRun {{ start: 8, end: 14, rgba: {info:?} }}, \
             ColorRun {{ start: 14, end: 17, rgba: {accent:?} }}, ColorRun {{ start: 17, end: 18, rgba: {accent:?} }}, \
             ColorRun {{ start: 19, end: 23, rgba: [154, 208, 255, 255] }}]"
        );
        assert!(log[0].contains(&want), "{}\n期望含 {want}", log[0]);
    }
}

#[cfg(test)]
mod gradient_tests {
    use super::Tooltip;
    use crate::theme_assets::gradient_fixture as fx;

    /// tooltip 节点的 `background.gradient` 叠在底色上（口径同候选窗节点）。
    /// 取样点同 status 气泡：左内边距、垂直居中（上内边距会被 macOS 的字形墨迹压到）。
    #[test]
    fn tooltip_paints_bg_gradient() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut t = Tooltip::new(tx).expect("非 Windows 下是内存桩");
        let mut theme = wind_theme::Resolved::default();
        theme.views.tooltip = Some(fx::node());
        t.set_theme(&theme);
        let (buf, w, _, _, ch, ml, mt, ..) = t.render_to_bgra("中");
        assert_eq!(
            fx::rgb(&buf, w, ml + 3, mt + ch / 2),
            fx::RED,
            "tooltip 渐变没画"
        );

        // 换到同节点、但没配渐变的主题：渐变必须清掉，露出底色。
        let mut plain = wind_theme::Resolved::default();
        plain.views.tooltip = Some(wind_theme::RvNode {
            bg_gradient: None,
            ..fx::node()
        });
        t.set_theme(&plain);
        let (buf, w, _, _, ch, ml, mt, ..) = t.render_to_bgra("中");
        assert_eq!(
            fx::rgb(&buf, w, ml + 3, mt + ch / 2),
            fx::BLUE,
            "旧主题的渐变残留"
        );
    }
}
