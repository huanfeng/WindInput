//! 平台基础类型/常量/光标函数的统一入口（跨平台 shim）。
//!
//! Windows 上直接复用 `windows` crate 的真实类型与 API；其它平台（Linux/macOS）
//! 提供 mock 占位，使 UI 层（候选窗/工具栏/菜单）的鼠标消息处理代码能够在非
//! Windows 平台上编译并跑测试。这些 mock 在非 Windows 上属于死代码——平台没有
//! Win32 消息泵，on_message 不会被触发——仅用于满足类型/链接。
//!
//! 真正的平台行为差异（LayeredWindow、DirectWrite 文本、剪贴板、消息循环）由各模块
//! 自身的 `#[cfg]` 分支处理，本模块只兜底“句柄/常量/光标”这类基础符号。

#[cfg(windows)]
mod imp {
    pub use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    pub use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, ReleaseCapture, SetCapture, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
    };
    pub use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowRect, HWND_TOPMOST, IDC_ARROW, IDC_SIZEALL, LoadCursorW, SW_HIDE,
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetCursor, SetWindowPos, ShowWindow,
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
        WM_SETCURSOR, WM_TIMER,
    };
    // WM_MOUSELEAVE 不在 WindowsAndMessaging 模块内，直接以字面量定义（与 Win32 一致）。
    pub const WM_MOUSELEAVE: u32 = 0x02A3;
    /// 会话结束（注销 / 关机）。
    pub const WM_ENDSESSION: u32 = 0x0016;
}

// 下面四条 allow 同源：本模块的 mock 是照抄 `windows` crate 的形状的——PascalCase 函数名、
// `VIRTUAL_KEY` 这类全大写类型名、`unsafe fn` 不写 `# Safety`（它不解引用任何指针）、错误类型
// 用 `Result<_, ()>`。把它们改成「更 Rust」的写法，本模块就失去了存在的意义：调用点在两个
// 平台上必须一字不差，否则 UI 层每个鼠标处理函数都要再分叉一次 cfg。
#[cfg(not(windows))]
#[allow(non_snake_case)]
#[allow(non_camel_case_types)]
#[allow(clippy::missing_safety_doc)]
#[allow(clippy::result_unit_err)]
mod imp {
    //! 非 Windows mock。类型/常量值尽量贴近 Win32 语义，但仅用于编译占位。

    // ---- 基础句柄/消息类型 ----
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct HWND(pub isize);
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct WPARAM(pub usize);
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct LPARAM(pub isize);
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct LRESULT(pub isize);
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct HCURSOR(pub isize);
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct HINSTANCE(pub isize);

    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct POINT {
        pub x: i32,
        pub y: i32,
    }
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct RECT {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    /// 光标资源名占位（对应 Win32 的 `PCWSTR` 资源 ID）。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CursorName(pub usize);

    /// 虚拟键码占位（对应 Win32 的 `VIRTUAL_KEY` newtype，`.0` 取值方式一致）。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VIRTUAL_KEY(pub u16);

    // ---- 常量 ----
    pub const VK_LBUTTON: VIRTUAL_KEY = VIRTUAL_KEY(0x01);
    pub const VK_RBUTTON: VIRTUAL_KEY = VIRTUAL_KEY(0x02);
    pub const VK_MBUTTON: VIRTUAL_KEY = VIRTUAL_KEY(0x04);
    pub const IDC_ARROW: CursorName = CursorName(32512);
    pub const IDC_SIZEALL: CursorName = CursorName(32646);
    pub const SW_HIDE: i32 = 0;
    pub const HWND_TOPMOST: HWND = HWND(-1);
    pub const SWP_NOSIZE: u32 = 0x0001;
    pub const SWP_NOZORDER: u32 = 0x0004;
    pub const SWP_NOACTIVATE: u32 = 0x0010;
    pub const WM_LBUTTONDOWN: u32 = 0x0201;
    pub const WM_LBUTTONUP: u32 = 0x0202;
    pub const WM_RBUTTONDOWN: u32 = 0x0204;
    pub const WM_RBUTTONUP: u32 = 0x0205;
    pub const WM_MBUTTONDOWN: u32 = 0x0207;
    pub const WM_MBUTTONUP: u32 = 0x0208;
    pub const WM_MOUSEMOVE: u32 = 0x0200;
    pub const WM_MOUSEWHEEL: u32 = 0x020A;
    pub const WM_SETCURSOR: u32 = 0x0020;
    pub const WM_MOUSELEAVE: u32 = 0x02A3;
    pub const WM_TIMER: u32 = 0x0113;
    pub const WM_ENDSESSION: u32 = 0x0016;

    // ---- 光标/窗口 API（mock：无副作用）----
    /// # Safety
    /// mock 不解引用指针；签名与 Win32 对齐以便调用点不变。
    ///
    /// Linux 外部宿主形态例外：返回 addon 最近报来的指针屏幕坐标（见 `ext_panel`），
    /// 此时 `lppoint` 须指向有效的 `POINT`（同 Win32 的约定）。
    #[cfg_attr(not(all(target_os = "linux", ext_presenter)), allow(unused_variables))]
    pub unsafe fn GetCursorPos(lppoint: *mut POINT) -> Result<(), ()> {
        #[cfg(all(target_os = "linux", ext_presenter))]
        if let Some((x, y)) = crate::ext_panel::cursor_pos()
            && !lppoint.is_null()
        {
            unsafe { *lppoint = POINT { x, y } };
        }
        Ok(())
    }
    pub unsafe fn LoadCursorW(_inst: Option<HINSTANCE>, _name: CursorName) -> Result<HCURSOR, ()> {
        Ok(HCURSOR(0))
    }
    pub unsafe fn SetCursor(_cursor: HCURSOR) -> HCURSOR {
        HCURSOR(0)
    }
    pub unsafe fn SetCapture(_hwnd: HWND) -> HWND {
        HWND(0)
    }
    pub unsafe fn ReleaseCapture() -> Result<(), ()> {
        Ok(())
    }
    /// mock：恒返回「未按下」。非 Windows 无全局按键状态可查，靠它驱动的轮询
    /// （菜单外点击）在此平台自然静默。
    pub unsafe fn GetAsyncKeyState(_vk: i32) -> i16 {
        0
    }
    /// Linux 外部宿主形态下，外部面板（`HWND(kind)`）给出服务端记下的当前矩形，见 `ext_panel`。
    #[cfg_attr(not(all(target_os = "linux", ext_presenter)), allow(unused_variables))]
    pub unsafe fn GetWindowRect(hwnd: HWND, rect: *mut RECT) -> Result<(), ()> {
        #[cfg(all(target_os = "linux", ext_presenter))]
        if let Some((left, top, right, bottom)) = crate::ext_panel::window_rect(hwnd)
            && !rect.is_null()
        {
            unsafe {
                *rect = RECT {
                    left,
                    top,
                    right,
                    bottom,
                }
            };
        }
        Ok(())
    }
    /// Linux 外部宿主形态：屏幕坐标减去外部面板的左上角（同 Win32，`WM_MOUSEWHEEL` 的 lparam
    /// 是屏幕坐标，处理器要先换回客户区）。
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub unsafe fn ScreenToClient(hwnd: HWND, lppoint: *mut POINT) -> bool {
        let Some((left, top, _, _)) = crate::ext_panel::window_rect(hwnd) else {
            return false;
        };
        if lppoint.is_null() {
            return false;
        }
        unsafe {
            (*lppoint).x -= left;
            (*lppoint).y -= top;
        }
        true
    }
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn SetWindowPos(
        _hwnd: HWND,
        _after: HWND,
        _x: i32,
        _y: i32,
        _cx: i32,
        _cy: i32,
        _flags: u32,
    ) -> Result<(), ()> {
        Ok(())
    }
    pub unsafe fn ShowWindow(_hwnd: HWND, _cmd: i32) -> bool {
        true
    }
}

pub use imp::*;

/// 按内容矩形把窗口钳制在所在显示器内——软阴影允许溢出屏幕。
///
/// 与 [`clamp_to_work_area`] 有两处刻意的差别：
///
/// 1. 按内容而非窗口矩形：窗口矩形含软阴影，按它钳制会让内容离屏幕边还有一个阴影宽度
///    就拖不过去。阴影溢出屏幕无妨。
/// 2. 钳到 `rcMonitor` 而非 `rcWork`，允许摆到任务栏上方：候选窗是临时浮层，用户可能正要
///    挪开让出正文；常驻的工具栏仍用 [`clamp_to_work_area`] 避开任务栏。
///
/// 入参与返回都是窗口左上（含阴影），`margin` = (left, top, right, bottom) 扩边。
// 非 Windows 下无显示器查询，除 x/y 外的参数仅 Windows 分支使用。
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn clamp_content_to_monitor(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    margin: (i32, i32, i32, i32),
) -> (i32, i32) {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        let (ml, mt, mr, mb) = margin;
        let (cw, ch) = (w as i32 - ml - mr, h as i32 - mt - mb);
        // 扩边大于窗口本身属异常数据，此时退回按整窗钳制，至少不会算出负尺寸。
        if cw <= 0 || ch <= 0 {
            return clamp_to_work_area(x, y, w, h);
        }
        let (cx, cy) = (x + ml, y + mt);
        unsafe {
            // 以内容中心选显示器：用左上角会让窗口刚过屏幕边界时归属跳变。
            let pt = POINT {
                x: cx + cw / 2,
                y: cy + ch / 2,
            };
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let m = mi.rcMonitor;
                return clamp_content_in_bounds(
                    x,
                    y,
                    margin,
                    (cw, ch),
                    (m.left, m.top, m.right, m.bottom),
                );
            }
        }
    }
    (x, y)
}

/// [`clamp_content_to_monitor`] 的纯几何内核：把内容矩形钳进 `bounds`，返回窗口左上。
///
/// 抽出来是为了可测，要锁住的性质是「内容贴边时窗口左上可以为负」（阴影允许溢出屏幕）。
///
/// `bounds` = (left, top, right, bottom)；`content` = (宽, 高)。
///
/// 非 Windows 下只有测试调用它（生产调用点在 `cfg(windows)` 内），不能删。
#[cfg_attr(not(windows), allow(dead_code))]
fn clamp_content_in_bounds(
    x: i32,
    y: i32,
    margin: (i32, i32, i32, i32),
    content: (i32, i32),
    bounds: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (ml, mt, _, _) = margin;
    let (cw, ch) = content;
    let (bl, bt, br, bb) = bounds;
    let (cx, cy) = (x + ml, y + mt);
    // 先按右/下边界回拉，再按左/上边界兜底：内容比屏幕还大时保证左上角可见。
    let nx = (cx.min(br - cw)).max(bl);
    let ny = (cy.min(bb - ch)).max(bt);
    (nx - ml, ny - mt)
}

/// [`clamp_to_work_area`] 的纯几何内核：把 `w×h` 的窗口钳进 `bounds`，返回窗口左上。
///
/// `bounds` = 工作区 (left, top, right, bottom)。钳制有损且不可逆：拿错误的 `w/h` 钳一次，
/// 正确坐标就再也回不来了（测试 `stale_size_destroys_a_flush_corner_position`）。
///
/// 也是状态气泡锚点落点（`status_tip::place_anchor`）的夹回步骤。
pub(crate) fn clamp_rect_in_bounds(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    bounds: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (bl, bt, br, bb) = bounds;
    let (wi, hi) = (w as i32, h as i32);
    let mut nx = x;
    let mut ny = y;
    // 先按右/下回拉，再按左/上兜底：窗口比工作区还大时保证左上角可见。
    if nx + wi > br {
        nx = br - wi;
    }
    if ny + hi > bb {
        ny = bb - hi;
    }
    if nx < bl {
        nx = bl;
    }
    if ny < bt {
        ny = bt;
    }
    (nx, ny)
}

/// 将 (x,y,w,h) 钳制到所在（或最近）显示器工作区内，保证窗口完整可见。
/// 用于拖动窗口时防止拖出桌面/拖入任务栏，以及切换显示器 / 远程连接后旧坐标落到屏外时拉回。
/// 多显示器下 `MonitorFromPoint(NEAREST)` 会随光标过界切到目标显示器。
///
/// `w`/`h` 必须是当前真实尺寸：错误尺寸会把合法坐标钳走且无法复原（跨 DPI 换屏时用上一块屏
/// 的尺寸，贴边位置会被永久拉离边缘）。调用方须确保已按目标屏排版（见 `Toolbar::set_pos`）。
// 非 Windows 下无显示器工作区查询，w/h 仅 Windows 分支使用。
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn clamp_to_work_area(x: i32, y: i32, w: u32, h: u32) -> (i32, i32) {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        unsafe {
            let pt = POINT { x, y };
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let wa = mi.rcWork;
                return clamp_rect_in_bounds(x, y, w, h, (wa.left, wa.top, wa.right, wa.bottom));
            }
        }
    }
    (x, y)
}

/// 给定屏幕坐标所在显示器（`MONITOR_DEFAULTTONEAREST`）的工作区 `(left, top, right, bottom)`。
/// 与 [`clamp_to_work_area`] 同源；非 Windows 恒 `None`。
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn work_area_at(x: i32, y: i32) -> Option<(i32, i32, i32, i32)> {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        unsafe {
            let pt = POINT { x, y };
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let wa = mi.rcWork;
                return Some((wa.left, wa.top, wa.right, wa.bottom));
            }
        }
    }
    None
}

/// 给定屏幕坐标所在显示器的工作区宽度（设备 px），作候选窗内容宽度的屏幕安全上限。
/// 与 [`clamp_to_work_area`] 同源，但给的是宽度上限而非位置。
///
/// 非 Windows 返回 `None`，调用方自给兜底值（见 [`CandidateWindow::screen_safety_max_width_px`]）。
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn monitor_work_area_width_at(x: i32, y: i32) -> Option<u32> {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        unsafe {
            let pt = POINT { x, y };
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let wa = mi.rcWork;
                return Some((wa.right - wa.left).max(0) as u32);
            }
        }
    }
    None
}

/// 同 [`monitor_work_area_width_at`]，取工作区高度。
///
/// 旋转态的候选列沿屏幕纵向延伸，其长度上限要问高度而不是宽度——拿宽度当上限，
/// 长候选会长到屏幕外，而那道钳制恒生效正是为了防这件事。
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn monitor_work_area_height_at(x: i32, y: i32) -> Option<u32> {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        unsafe {
            let pt = POINT { x, y };
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let wa = mi.rcWork;
                return Some((wa.bottom - wa.top).max(0) as u32);
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "sys_clamp_rect_tests.rs"]
mod clamp_rect_tests;

#[cfg(test)]
#[path = "sys_clamp_tests.rs"]
mod clamp_tests;
