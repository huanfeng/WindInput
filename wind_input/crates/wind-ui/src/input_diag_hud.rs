//! 输入诊断 HUD：非激活置顶浮窗，右键「高级」开关控制显隐，可拖动，双击复制。
//!
//! 复用 [`crate::window::LayeredWindow`]（默认已带 `WS_EX_NOACTIVATE | WS_EX_TOPMOST |
//! WS_EX_TOOLWINDOW | WS_EX_LAYERED`：不进任务栏、不抢焦点、透明渲染）+ [`TextRenderer`] +
//! [`crate::view::View`] 盒模型，仿 `status_tip.rs`/`toast.rs`。MVP 用固定深色半透明底 + 白字，
//! 不接主题。拖动/双击复制在窗口过程（`wnd_proc`）经 [`WindowMouse`] 处理。

use crate::text::dwrite::TextRenderer;
use crate::view::{Align, Edges, Layout, View};
use crate::window::LayeredWindow;

/// 视图数据类型与纯格式化 `format_diag_lines` 已下沉至 wind-ui-types；
/// 再导出保持 `wind_ui::input_diag_hud::*` 原路径成立（manager.rs 的链式转发也经此）。
pub use wind_ui_types::{DiagSections, InputDiagView, WindowDiagView, format_diag_lines};

/// 固定深色半透明底 + 白字（MVP，不接主题）。
const BG: [u8; 4] = [32, 32, 36, 235];
const FG: [u8; 4] = [240, 240, 245, 255];
/// 无主题兜底字号（逻辑像素）。
const FONT_PX: f32 = 14.0;
/// 初始位置距屏幕右下角边距（逻辑像素）。
const MARGIN: i32 = 24;

/// 拖动交互状态（与 `wnd_proc` 共享）：仅在 Windows 有意义，非 Windows 为死代码占位。
struct DragState {
    hwnd: crate::sys::HWND,
    /// 是否正在拖动（`WM_LBUTTONDOWN` → true，`WM_LBUTTONUP` → false）。
    dragging: bool,
    /// 按下时鼠标屏幕坐标与窗口左上角的偏移，拖动时保持该偏移。
    grab_dx: i32,
    grab_dy: i32,
    /// 上次左键按下时间与坐标，用于手动判定双击（窗口类未启用 CS_DBLCLKS）。
    last_down_ms: u64,
    last_down_x: i32,
    last_down_y: i32,
    /// 当前诊断文本（双击复制用），由 `show_or_update` 刷新。
    copy_text: String,
    /// 回送协调器的事件通道（右键请求菜单）。同 `tooltip.rs` 的做法：菜单树与动作分发
    /// 都归协调器，UI 侧只负责报告"用户在哪儿右键了"。
    events: std::sync::mpsc::Sender<crate::manager::UiEvent>,
}

impl DragState {
    fn new(
        hwnd: crate::sys::HWND,
        events: std::sync::mpsc::Sender<crate::manager::UiEvent>,
    ) -> Self {
        Self {
            hwnd,
            dragging: false,
            grab_dx: 0,
            grab_dy: 0,
            last_down_ms: 0,
            last_down_x: i32::MIN,
            last_down_y: i32::MIN,
            copy_text: String::new(),
            events,
        }
    }
}

/// 当前毫秒时间戳（单调够用；仅用于双击间隔判定）。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl crate::window::WindowMouse for DragState {
    fn on_message(
        &mut self,
        _hwnd: crate::sys::HWND,
        msg: u32,
        _wparam: crate::sys::WPARAM,
        _lparam: crate::sys::LPARAM,
    ) -> Option<crate::sys::LRESULT> {
        use crate::sys::{
            HWND_TOPMOST, LRESULT, ReleaseCapture, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
            SetCapture, SetWindowPos, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN,
        };
        match msg {
            WM_RBUTTONDOWN => {
                // 拖动中右键：先收拖动，否则 capture 会留在窗口上、菜单收不到点击。
                if self.dragging {
                    self.dragging = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                let (sx, sy) = cursor_screen();
                let _ = self
                    .events
                    .send(crate::manager::UiEvent::RequestInputDiagMenu { x: sx, y: sy });
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                // 记录抓取偏移（鼠标屏幕坐标 − 窗口左上角），并 SetCapture 以持续收到移动。
                let (mx, my) = cursor_screen();
                let (wx, wy) = window_origin(self.hwnd);
                self.grab_dx = mx - wx;
                self.grab_dy = my - wy;
                self.dragging = true;
                unsafe {
                    SetCapture(self.hwnd);
                }
                // 手动双击判定：与上次按下间隔 < 400ms 且位移 < 6px → 视为双击，复制文本。
                let t = now_ms();
                let dbl = t.saturating_sub(self.last_down_ms) < 400
                    && (mx - self.last_down_x).abs() < 6
                    && (my - self.last_down_y).abs() < 6;
                if dbl {
                    self.dragging = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    if !self.copy_text.is_empty() {
                        crate::popup_menu::set_clipboard_text(&self.copy_text);
                    }
                    self.last_down_ms = 0; // 消费本次双击，避免三击连锁
                } else {
                    self.last_down_ms = t;
                    self.last_down_x = mx;
                    self.last_down_y = my;
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if self.dragging {
                    let (mx, my) = cursor_screen();
                    let nx = mx - self.grab_dx;
                    let ny = my - self.grab_dy;
                    unsafe {
                        let _ = SetWindowPos(
                            self.hwnd,
                            HWND_TOPMOST,
                            nx,
                            ny,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    return Some(LRESULT(0));
                }
                None
            }
            WM_LBUTTONUP => {
                if self.dragging {
                    self.dragging = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    return Some(LRESULT(0));
                }
                None
            }
            _ => None,
        }
    }
}

/// 取鼠标屏幕坐标（失败回退 (0,0)）。
fn cursor_screen() -> (i32, i32) {
    let mut pt = crate::sys::POINT::default();
    unsafe {
        let _ = crate::sys::GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}

/// 取窗口左上角屏幕坐标（失败回退 (0,0)）。
fn window_origin(hwnd: crate::sys::HWND) -> (i32, i32) {
    let mut r = crate::sys::RECT::default();
    unsafe {
        let _ = crate::sys::GetWindowRect(hwnd, &mut r);
    }
    (r.left, r.top)
}

/// 每次刷新时如何给窗口定位。抽成纯枚举让「选哪一档」可单测（坐标计算依赖 Win32）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PositionPlan {
    /// 首次显示：落屏幕右下角。
    InitialCorner,
    /// 已被拖出屏幕外：复位右下角，保证还能被抓回来。
    ResetCorner,
    /// 常规：以窗口当前实际位置为基准钳回工作区。
    ClampCurrent,
}

/// 定位档位决策。
///
/// `ClampCurrent` 让内容变长（分区展开）时不被屏幕边缘吞掉：保持左上角贴近原位，只把溢出
/// 部分推回来。拖动过程不经过这里，拖到哪当次就是哪，下次内容更新才钳回屏内。
fn plan_position(positioned: bool, visible: bool) -> PositionPlan {
    if !positioned {
        PositionPlan::InitialCorner
    } else if visible {
        PositionPlan::ClampCurrent
    } else {
        PositionPlan::ResetCorner
    }
}

/// 本次 `show` 该用哪一档 z 序。
///
/// `applied` = 上次已应用的置顶态（`None` = 还没显示过），`want` = 本次期望的。
///
/// 非置顶下的后续刷新必须 `Keep`：`LayeredWindow::show` 无条件 `HWND_TOPMOST`，每次都走
/// `NoTopmost` 会把窗口反复插到非置顶组顶部、盖住别的窗口。只有第一次需要真的移出置顶组。
fn plan_zorder(applied: Option<bool>, want: bool) -> crate::window::ShowZOrder {
    use crate::window::ShowZOrder;
    if want {
        // 置顶：每次都重申。候选窗/宿主窗口都可能在此期间抢到更高 z 位。
        ShowZOrder::Topmost
    } else if applied == Some(false) {
        ShowZOrder::Keep
    } else {
        // 首次显示即非置顶，或刚从置顶切过来 → 真正移出置顶组（清 WS_EX_TOPMOST）。
        ShowZOrder::NoTopmost
    }
}

/// 输入诊断 HUD 窗口
pub struct InputDiagHud {
    window: LayeredWindow,
    renderer: TextRenderer,
    scale: f32,
    /// 拖动/双击状态（注册进 `wnd_proc`）。show_or_update 刷新其 copy_text。
    state: std::rc::Rc<std::cell::RefCell<DragState>>,
    /// 最近一次 show 使用的窗口左上角屏幕坐标：首次为右下角，之后每次刷新从窗口实际位置
    /// 同步（尊重用户拖动）；仅当被拖出屏幕外时复位回右下角。
    pos: (i32, i32),
    /// 是否已定位过（避免每次 update 都重置到右下角，尊重用户拖动）。
    positioned: bool,
    /// 已应用的置顶态。`None` = 还没显示过。
    ///
    /// 用来区分「切换到非置顶的那一次」（要移出置顶组）和「后续刷新」（完全不动 z 序），
    /// 见 [`plan_zorder`]。
    applied_topmost: Option<bool>,
}

impl InputDiagHud {
    pub fn new(events: std::sync::mpsc::Sender<crate::manager::UiEvent>) -> Result<Self, String> {
        let scale = dpi_scale();
        let window = LayeredWindow::create(None, 240, 120, "WindInputDiagHud")?;
        let renderer = TextRenderer::new("Microsoft YaHei UI", FONT_PX * scale)?;
        let state = std::rc::Rc::new(std::cell::RefCell::new(DragState::new(
            window.hwnd(),
            events,
        )));
        window.register_mouse(state.clone());
        let pos = initial_bottom_right(240, 120, scale);
        Ok(Self {
            window,
            renderer,
            scale,
            state,
            pos,
            positioned: false,
            applied_topmost: None,
        })
    }

    /// 渲染 4 行诊断文本并显示/更新窗口（首次落右下角，之后保持当前位置尊重拖动）。
    pub fn show_or_update(&mut self, v: &InputDiagView) {
        let lines = format_diag_lines(v);
        // 双击复制文本：整块以换行连接。
        self.state.borrow_mut().copy_text = lines.join("\n");

        let s = self.scale;
        let mut col = View::container(Layout::Column)
            .bg(BG)
            .pad(Edges::xy(12.0 * s, 9.0 * s))
            .gap(4.0 * s);
        col.corner_radius = 8.0 * s;
        for line in &lines {
            col = col.child(View::leaf(line.clone(), FG).text_align(Align::Start));
        }
        col.layout(0.0, 0.0, &self.renderer);
        let (w_f, h_f) = col.measured_size();
        let w = (w_f.ceil() as u32).max(80);
        let h = (h_f.ceil() as u32).max(40);
        self.window.resize(w, h);
        {
            let buf = self.window.buffer_mut();
            buf.fill(0);
        }
        {
            let (bw, bh) = self.window.size();
            let buf = self.window.buffer_mut();
            col.paint(buf, bw, bh, &self.renderer);
        }
        if let Err(e) = self.window.update() {
            tracing::warn!("InputDiagHud update failed: {}", e);
        }
        // 定位三档（见 `plan_position`）：首次落右下角；拖出屏外复位右下角；其余以窗口当前
        // 实际位置为基准钳回工作区。
        let (cx, cy) = window_origin(self.window.hwnd());
        self.pos = match plan_position(
            self.positioned,
            window_visible_on_screen(cx, cy, w as i32, h as i32),
        ) {
            PositionPlan::InitialCorner | PositionPlan::ResetCorner => {
                initial_bottom_right(w, h, self.scale)
            }
            PositionPlan::ClampCurrent => crate::sys::clamp_to_work_area(cx, cy, w, h),
        };
        self.positioned = true;
        // z 序三档，非置顶时后续刷新必须 Keep（见 `plan_zorder`）。
        let plan = plan_zorder(self.applied_topmost, v.topmost);
        self.window.show_z(self.pos.0, self.pos.1, plan);
        // 刚移出置顶组时再插到当前前台窗口之后，立刻沉下去，否则要等用户点别的窗口才看得出效果。
        if plan == crate::window::ShowZOrder::NoTopmost {
            self.sink_below_foreground();
        }
        self.applied_topmost = Some(v.topmost);
    }

    /// 把窗口插到当前前台窗口之后（仅在刚取消置顶时调用）。
    ///
    /// 前台窗口若自身是置顶窗口则跳过：插到 topmost 窗口之后的窗口也会变成 topmost，
    /// 刚清掉的 `WS_EX_TOPMOST` 会当场被重新戴上。
    fn sink_below_foreground(&self) {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, SWP_NOACTIVATE, SWP_NOMOVE,
                SWP_NOSIZE, SetWindowPos, WS_EX_TOPMOST,
            };
            let fg = GetForegroundWindow();
            if fg == HWND::default() || fg == self.window.hwnd() {
                return;
            }
            let ex = GetWindowLongPtrW(fg, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TOPMOST.0 != 0 {
                return; // 前台自己是置顶窗口，插到它后面会把我们也变回置顶
            }
            let _ = SetWindowPos(
                self.window.hwnd(),
                fg,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    /// 复制当前显示的诊断文本到剪贴板（右键菜单「复制全部内容」）。
    /// 文本取自最近一次渲染的行——所见即所得，包含分区隐藏后的结果。
    pub fn copy_text(&self) {
        let text = self.state.borrow().copy_text.clone();
        if !text.is_empty() {
            crate::popup_menu::set_clipboard_text(&text);
        }
    }

    pub fn hide(&mut self) {
        self.window.hide();
    }
}

/// 系统 DPI 缩放因子（仿 status_tip.rs）。
fn dpi_scale() -> f32 {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Gdi::*;
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

/// 主屏右下角初始坐标 = 屏幕尺寸 − 窗口尺寸 − 边距（边距按 DPI 缩放）。
#[cfg_attr(not(windows), allow(unused_variables))]
fn initial_bottom_right(w: u32, h: u32, scale: f32) -> (i32, i32) {
    let margin = (MARGIN as f32 * scale).round() as i32;
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        unsafe {
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            let x = (sw - w as i32 - margin).max(0);
            let y = (sh - h as i32 - margin).max(0);
            (x, y)
        }
    }
    #[cfg(not(windows))]
    {
        (margin, margin)
    }
}

/// 窗口矩形与屏幕虚拟区域的可见交集在两个方向上是否都 ≥ `min`（纯几何，可单测）。
/// 用"可见余量"而非"任意相交"：只露极少（如 1px）也视为屏外，保证用户能重新抓到窗口。
/// 参数是两个矩形加一个阈值，展开成标量便于逐项单测，不包成 Rect 对象。
#[allow(clippy::too_many_arguments)]
/// 非 Windows 下唯一的调用者是本文件的测试（生产调用点在 `cfg(windows)` 内），
/// 故 lib 单独编译时它无人使用——测试本身仍跨平台跑，不能删。
#[cfg_attr(not(windows), allow(dead_code))]
fn rect_visible(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    vx: i32,
    vy: i32,
    vw: i32,
    vh: i32,
    min: i32,
) -> bool {
    let overlap_w = (x + w).min(vx + vw) - x.max(vx);
    let overlap_h = (y + h).min(vy + vh) - y.max(vy);
    overlap_w >= min && overlap_h >= min
}

/// 窗口是否"在屏幕内"（拖出屏外判据）：与虚拟屏（多显示器合并区域）可见交集 ≥ 24px。
#[cfg_attr(not(windows), allow(unused_variables))]
fn window_visible_on_screen(x: i32, y: i32, w: i32, h: i32) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
            SM_YVIRTUALSCREEN,
        };
        unsafe {
            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            // 指标异常（0）时保守视为屏内，避免误复位用户拖动的位置。
            if vw <= 0 || vh <= 0 {
                return true;
            }
            rect_visible(x, y, w, h, vx, vy, vw, vh, 24)
        }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_visible_keeps_fully_and_partially_onscreen() {
        // 完全在屏内
        assert!(rect_visible(100, 100, 240, 120, 0, 0, 1920, 1080, 24));
        // 部分露出但可见余量充足（左侧被切到只剩 100px 宽仍可见）
        assert!(rect_visible(1820, 100, 240, 120, 0, 0, 1920, 1080, 24));
    }

    #[test]
    fn rect_visible_resets_when_offscreen() {
        // 拖到顶部外，仅剩 10px 高可见 < 24 → 判定屏外
        assert!(!rect_visible(100, -110, 240, 120, 0, 0, 1920, 1080, 24));
        // 完全拖到右侧屏外
        assert!(!rect_visible(1920, 100, 240, 120, 0, 0, 1920, 1080, 24));
        // 完全拖到左侧屏外（负坐标）
        assert!(!rect_visible(-240, 100, 240, 120, 0, 0, 1920, 1080, 24));
    }

    #[test]
    fn rect_visible_multi_monitor_left_virtual_origin() {
        // 副屏在主屏左侧：虚拟屏原点为负；窗口在副屏内应判定屏内
        assert!(rect_visible(-1800, 200, 240, 120, -1920, 0, 3840, 1080, 24));
    }

    /// 定位三档。窗口在屏内时也要走钳制而不是原样沿用坐标，否则内容变长后会被屏幕边缘吞掉。
    #[test]
    fn position_plan_three_cases() {
        assert_eq!(plan_position(false, true), PositionPlan::InitialCorner);
        assert_eq!(plan_position(false, false), PositionPlan::InitialCorner);
        assert_eq!(plan_position(true, true), PositionPlan::ClampCurrent);
        assert_eq!(plan_position(true, false), PositionPlan::ResetCorner);
    }

    /// z 序档位。非置顶下的后续刷新必须 `Keep`，否则每次刷新都把窗口顶回普通窗口之上。
    #[test]
    fn zorder_plan_covers_switch_and_steady_state() {
        use crate::window::ShowZOrder;
        // 置顶：每次都重申（此间可能有别的窗口抢到更高 z 位）。
        assert_eq!(plan_zorder(None, true), ShowZOrder::Topmost);
        assert_eq!(plan_zorder(Some(true), true), ShowZOrder::Topmost);
        assert_eq!(plan_zorder(Some(false), true), ShowZOrder::Topmost);
        // 刚从置顶切到非置顶 / 首次即非置顶 → 真正移出置顶组。
        assert_eq!(plan_zorder(Some(true), false), ShowZOrder::NoTopmost);
        assert_eq!(plan_zorder(None, false), ShowZOrder::NoTopmost);
        // 已经是非置顶 → 放手不管，让别的窗口盖过来。
        assert_eq!(plan_zorder(Some(false), false), ShowZOrder::Keep);
    }
}
