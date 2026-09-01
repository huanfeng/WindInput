//! Win32 Layered Window 封装（跨平台）
//!
//! 用于候选窗口、工具栏等浮层。Windows 上使用 UpdateLayeredWindow 实现透明渲染；
//! 非 Windows 平台提供 mock 实现（持有 BGRA 缓冲区，show/hide/update 为空操作），
//! 使上层窗口逻辑能在 Linux 上编译与跑测试。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sys::{HWND, LPARAM, LRESULT, WPARAM};

/// 系统「浅色/深色模式」已变更的置位标记。
///
/// 系统用 `SendMessageTimeout(HWND_BROADCAST, WM_SETTINGCHANGE, 0, "ImmersiveColorSet")`
/// 广播这一变更。**发送**的消息不入线程消息队列——它由系统在本线程调用 `PeekMessage`
/// 期间直接回调 `wnd_proc`，因此 UI 主循环的消息泵里根本看不到它，只能在 `wnd_proc`
/// 截获。而截获点身处对方 `SendMessage` 的同步等待中，不宜就地重解析主题（会阻塞广播方，
/// 且 wnd_proc 拿不到协调器），故仅置标记，由 UI 主循环取走后回送协调器。
static SYSTEM_COLOR_CHANGED: AtomicBool = AtomicBool::new(false);

/// 取走「系统明暗已变更」标记（读取即清零）。UI 主循环每轮调用一次。
///
/// 一次系统切换会广播给本进程的每个顶层窗口（候选窗/工具栏/气泡…），标记因而被重复置位；
/// swap 语义把这一串重复塌缩成一次事件，避免同一次切换触发多轮主题重解析。
pub fn take_system_color_changed() -> bool {
    SYSTEM_COLOR_CHANGED.swap(false, Ordering::Relaxed)
}

/// `LayeredWindow::show_z` 的 z 序意图。
///
/// 存在的理由：`show()` 历来无条件把窗口插进**置顶组**，这对候选窗/工具栏是对的
/// （它们必须盖住一切），但对「可以被用户关掉置顶」的浮窗就成了 bug ——每次刷新都把
/// 它重新提到最前，用户看到的是「置顶开关不起作用」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowZOrder {
    /// 插入置顶组（`HWND_TOPMOST`）。历史默认，`show()` 等价于本档。
    Topmost,
    /// 移出置顶组（`HWND_NOTOPMOST`，清 `WS_EX_TOPMOST`），落到非置顶组顶部。
    /// 只在**切换的那一次**用，之后要用 [`ShowZOrder::Keep`]，否则每次刷新又把它顶到
    /// 所有普通窗口之上。
    NoTopmost,
    /// 完全不动 z 序（`SWP_NOZORDER`）。别的窗口被激活时能自然盖过本窗口。
    Keep,
}

/// 浮层窗口鼠标消息处理器（由具体窗口实现，如候选窗）。
/// 返回 `Some(lresult)` 表示已处理；`None` 交回默认处理。
///
/// 非 Windows 平台上没有 Win32 消息泵，该 trait 的实现不会被调用，仅用于类型占位。
pub trait WindowMouse {
    fn on_message(
        &mut self,
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT>;
}

#[cfg(windows)]
mod platform {
    use super::{Rc, RefCell, WindowMouse};
    use std::collections::HashMap;
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;

    thread_local! {
        /// hwnd → 鼠标处理器（仅 UI 线程访问，wnd_proc 与窗口同线程）
        static MOUSE_HANDLERS: RefCell<HashMap<isize, Rc<RefCell<dyn WindowMouse>>>> =
            RefCell::new(HashMap::new());
    }

    /// `WM_SETTINGCHANGE` 的 lParam 是否为 `"ImmersiveColorSet"`（即系统明暗/强调色变更）。
    ///
    /// 该消息被系统复用于几十种设置变更（区域、字体、电源…），lParam 是唯一的区分依据；
    /// 它可能为 NULL（部分变更不带名字），也并非总以我方期望的长度收尾，故显式设上限扫描，
    /// 不用 `PCWSTR::to_string()`——后者在非法指针上会走到未定义行为。
    fn is_immersive_color_set(lparam: LPARAM) -> bool {
        if lparam.0 == 0 {
            return false;
        }
        // 逐字比对（含结尾 NUL——否则 "ImmersiveColorSetX" 之类会被前缀误判）。
        // `all` 短路：首个不符即停，不会沿非预期字符串一路读下去。
        let p = lparam.0 as *const u16;
        let expect = "ImmersiveColorSet".encode_utf16().chain(std::iter::once(0));
        unsafe { expect.enumerate().all(|(i, c)| *p.add(i) == c) }
    }

    /// Layered Window 封装
    pub struct LayeredWindow {
        hwnd: HWND,
        width: u32,
        height: u32,
        /// BGRA 像素缓冲区
        buffer: Vec<u8>,
        /// 窗口类名，仅用于诊断日志区分是哪一个浮层。
        class_name: String,
        /// 首次成功 show 后是否已记录实测几何。
        ///
        /// 「代码走到了 show()」与「像素出现在屏幕上」之间隔着好几层，而此前一层都没测量：
        /// `SetWindowPos` 的结果被丢弃、最终落点无日志、窗口归属哪个桌面也无从得知。
        /// 只记第一次——`show()` 在候选刷新时每帧调用，不能进热路径。
        geometry_logged: std::cell::Cell<bool>,
    }

    /// 当前线程所属桌面名（如 `Default`、`Winlogon`）。取不到返回 `?`。
    ///
    /// 浮层归属哪个桌面在窗口创建时就定死了，事后无法从窗口自身看出来，
    /// 故须在创建时记下——它是「服务一切正常却全屏无 GUI」的候选成因之一。
    fn current_desktop_name() -> String {
        use windows::Win32::System::StationsAndDesktops::GetThreadDesktop;
        unsafe {
            match GetThreadDesktop(windows::Win32::System::Threading::GetCurrentThreadId()) {
                Ok(h) => user_object_name(h.0),
                Err(_) => "?".to_string(),
            }
        }
    }

    /// 当前进程所属窗口站名（交互式会话通常为 `WinSta0`）。取不到返回 `?`。
    fn current_window_station_name() -> String {
        use windows::Win32::System::StationsAndDesktops::GetProcessWindowStation;
        unsafe {
            match GetProcessWindowStation() {
                Ok(h) => user_object_name(h.0),
                Err(_) => "?".to_string(),
            }
        }
    }

    /// 读取 USER 对象（桌面/窗口站）名称。
    fn user_object_name(handle: *mut std::ffi::c_void) -> String {
        use windows::Win32::System::StationsAndDesktops::{GetUserObjectInformationW, UOI_NAME};
        unsafe {
            let mut buf = [0u16; 128];
            let mut needed = 0u32;
            let ok = GetUserObjectInformationW(
                windows::Win32::Foundation::HANDLE(handle),
                UOI_NAME,
                Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
                std::mem::size_of_val(&buf) as u32,
                Some(&mut needed),
            )
            .is_ok();
            if !ok {
                return "?".to_string();
            }
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..len])
        }
    }

    impl LayeredWindow {
        pub fn create(
            parent: Option<HWND>,
            width: u32,
            height: u32,
            class_name: &str,
        ) -> Result<Self, String> {
            unsafe {
                let instance =
                    GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {}", e))?;

                let class_wide: Vec<u16> = class_name
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();

                // 加载箭头光标（避免鼠标繁忙状态）
                let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();

                let wnd_class = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(Self::wnd_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: instance.into(),
                    hbrBackground: HBRUSH::default(),
                    lpszMenuName: windows::core::PCWSTR::null(),
                    lpszClassName: windows::core::PCWSTR(class_wide.as_ptr()),
                    hIcon: HICON::default(),
                    hIconSm: HICON::default(),
                    hCursor: cursor,
                };

                RegisterClassExW(&wnd_class);

                let style = WS_POPUP;
                let ex_style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;

                let hwnd = CreateWindowExW(
                    ex_style,
                    windows::core::PCWSTR(class_wide.as_ptr()),
                    windows::core::PCWSTR(class_wide.as_ptr()),
                    style,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    parent.unwrap_or_default(),
                    HMENU::default(),
                    instance,
                    None,
                )
                .map_err(|e| format!("CreateWindowExW: {}", e))?;

                let buffer = vec![0u8; (width * height * 4) as usize];

                // 记录窗口所属的桌面 / 窗口站。开机自启时服务可能跑在用户交互桌面就绪之前，
                // 此时建出的窗口不属于用户当前桌面：服务逻辑全对、管道照常通信、TSF 是 in-proc
                // 所以打字正常，而所有浮层在用户眼里都不存在——且 kill 重启即恢复。
                // 这是「所有 GUI 元素同时不可见」少数能一次性解释的成因，故在创建时就留证。
                wind_config::startup_trace::stage(&format!(
                    "win-create {} hwnd={:?} desktop={} winsta={}",
                    class_name,
                    hwnd.0,
                    current_desktop_name(),
                    current_window_station_name(),
                ));

                Ok(Self {
                    hwnd,
                    width,
                    height,
                    buffer,
                    class_name: class_name.to_string(),
                    geometry_logged: std::cell::Cell::new(false),
                })
            }
        }

        pub fn hwnd(&self) -> HWND {
            self.hwnd
        }

        /// 注册鼠标处理器（绑定到本窗口 hwnd）
        pub fn register_mouse(&self, handler: Rc<RefCell<dyn WindowMouse>>) {
            let key = self.hwnd.0 as isize;
            MOUSE_HANDLERS.with(|m| {
                m.borrow_mut().insert(key, handler);
            });
        }

        pub fn buffer(&self) -> &[u8] {
            &self.buffer
        }

        pub fn buffer_mut(&mut self) -> &mut [u8] {
            &mut self.buffer
        }

        pub fn resize(&mut self, width: u32, height: u32) {
            self.width = width;
            self.height = height;
            self.buffer.resize((width * height * 4) as usize, 0);
        }

        pub fn update(&self) -> Result<(), String> {
            self.update_with_alpha(255)
        }

        /// 以整窗常数 alpha 提交当前 buffer（淡出动画用）：
        /// SourceConstantAlpha 与每像素 alpha（AC_SRC_ALPHA）按 UpdateLayeredWindow 语义叠乘，
        /// 圆角/半透明背景不受影响；无需重绘像素。
        pub fn update_with_alpha(&self, alpha: u8) -> Result<(), String> {
            unsafe {
                let hdc_screen = GetDC(HWND::default());
                // 两者此前都未做空值检查就往下用。开机早期或 GDI 句柄耗尽时它们会返回空，
                // 后续调用便在空 DC 上静默失败，最终表现为「一切成功但什么都没画出来」。
                if hdc_screen.is_invalid() {
                    return Err("GetDC(screen) returned null".to_string());
                }
                let hdc_mem = CreateCompatibleDC(hdc_screen);
                if hdc_mem.is_invalid() {
                    ReleaseDC(HWND::default(), hdc_screen);
                    return Err("CreateCompatibleDC returned null".to_string());
                }

                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: self.width as i32,
                        biHeight: -(self.height as i32),
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        biSizeImage: 0,
                        biXPelsPerMeter: 0,
                        biYPelsPerMeter: 0,
                        biClrUsed: 0,
                        biClrImportant: 0,
                    },
                    ..std::mem::zeroed()
                };

                let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
                let hbitmap =
                    CreateDIBSection(hdc_mem, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
                        .map_err(|e| format!("CreateDIBSection: {}", e))?;

                std::ptr::copy_nonoverlapping(
                    self.buffer.as_ptr(),
                    bits_ptr as *mut u8,
                    self.buffer.len(),
                );

                let old_bmp = SelectObject(hdc_mem, hbitmap);

                let size = SIZE {
                    cx: self.width as i32,
                    cy: self.height as i32,
                };
                let source = POINT { x: 0, y: 0 };

                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: alpha,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };

                let result = UpdateLayeredWindow(
                    self.hwnd,
                    hdc_screen,
                    None,
                    Some(&size),
                    hdc_mem,
                    Some(&source),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );

                SelectObject(hdc_mem, old_bmp);
                let _ = DeleteObject(hbitmap);
                let _ = DeleteDC(hdc_mem);
                ReleaseDC(HWND::default(), hdc_screen);

                if result.is_err() {
                    return Err(format!("UpdateLayeredWindow: {:?}", result));
                }

                Ok(())
            }
        }

        pub fn show(&self, x: i32, y: i32) {
            self.show_z(x, y, super::ShowZOrder::Topmost)
        }

        /// 带 z 序意图的显示。语义见 [`super::ShowZOrder`]。
        pub fn show_z(&self, x: i32, y: i32, z: super::ShowZOrder) {
            use super::ShowZOrder;
            let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
            // Keep 档下 hWndInsertAfter 被 SWP_NOZORDER 忽略，传什么都不生效。
            let insert_after = match z {
                ShowZOrder::Topmost => HWND_TOPMOST,
                ShowZOrder::NoTopmost => HWND_NOTOPMOST,
                ShowZOrder::Keep => {
                    flags |= SWP_NOZORDER;
                    HWND_TOPMOST
                }
            };
            unsafe {
                let r = SetWindowPos(
                    self.hwnd,
                    insert_after,
                    x,
                    y,
                    self.width as i32,
                    self.height as i32,
                    flags,
                );
                // 这是唯一让窗口现身的调用，其结果此前被整个丢弃。
                if let Err(e) = r {
                    tracing::warn!("{}: SetWindowPos failed: {}", self.class_name, e);
                }

                // 首次显示后回读系统的真实认知：请求坐标未必等于落点，
                // 而 layered 窗口即便 UpdateLayeredWindow 成功、IsWindowVisible 为真，
                // 也可能因落在所有显示器之外或不属于当前桌面而看不见。
                // 只记一次，避免进候选刷新的热路径。
                if !self.geometry_logged.get() {
                    self.geometry_logged.set(true);
                    let mut rect = RECT::default();
                    let got = GetWindowRect(self.hwnd, &mut rect).is_ok();
                    wind_config::startup_trace::stage(&format!(
                        "win-show {} req=({x},{y}) {}x{} rect={} visible={}",
                        self.class_name,
                        self.width,
                        self.height,
                        if got {
                            format!(
                                "({},{})-({},{})",
                                rect.left, rect.top, rect.right, rect.bottom
                            )
                        } else {
                            "GetWindowRect-FAILED".to_string()
                        },
                        IsWindowVisible(self.hwnd).as_bool(),
                    ));
                }
            }
        }

        pub fn hide(&self) {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }

        pub fn clear(&mut self) {
            self.buffer.fill(0);
        }

        pub fn size(&self) -> (u32, u32) {
            (self.width, self.height)
        }

        /// 将当前 BGRA buffer 保存为 PNG 文件（截图用）。
        pub fn capture_to_file(&self, path: &std::path::Path) -> Result<(), String> {
            crate::screenshot::save_bgra_to_png(&self.buffer, self.width, self.height, path)
        }

        /// 将当前 BGRA buffer 复制到剪贴板（截图用）。
        pub fn capture_to_clipboard(&self) -> Result<(), String> {
            crate::screenshot::copy_bgra_to_clipboard(&self.buffer, self.width, self.height)
        }

        unsafe extern "system" fn wnd_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            // 系统浅色/深色模式切换：置标记后仍需交回默认处理（本消息不属于我们，别吞）。
            if msg == WM_SETTINGCHANGE && is_immersive_color_set(lparam) {
                super::SYSTEM_COLOR_CHANGED.store(true, super::Ordering::Relaxed);
            }
            // 不抢焦点：点击浮层不激活窗口，目标应用保持前台
            if msg == WM_MOUSEACTIVATE {
                return LRESULT(MA_NOACTIVATE as isize);
            }
            // 命中测试：返回 HTCLIENT 才能收到鼠标消息
            if msg == WM_NCHITTEST {
                return LRESULT(HTCLIENT as isize);
            }
            // 鼠标相关消息派发给已注册处理器（先取出 Rc 释放注册表借用，避免重入冲突）
            if matches!(
                msg,
                WM_LBUTTONDOWN
                    | WM_LBUTTONUP
                    | WM_RBUTTONDOWN
                    // 中键：工具栏用它在中英格上一键切方案。⚠️ 这份名单是**必经的闸门**
                    // ——处理器里写好分支也照样收不到消息，且编译与测试全绿，
                    // 表现只是「点了没反应」。加新鼠标交互时先看这里。
                    | crate::sys::WM_MBUTTONUP
                    | WM_MOUSEMOVE
                    | crate::sys::WM_MOUSELEAVE
                    | WM_MOUSEWHEEL
                    | WM_SETCURSOR
            ) {
                let key = hwnd.0 as isize;
                let handler = MOUSE_HANDLERS.with(|m| m.borrow().get(&key).cloned());
                if let Some(h) = handler
                    && let Some(lr) = h.borrow_mut().on_message(hwnd, msg, wparam, lparam)
                {
                    return lr;
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }

    impl Drop for LayeredWindow {
        fn drop(&mut self) {
            let key = self.hwnd.0 as isize;
            MOUSE_HANDLERS.with(|m| {
                m.borrow_mut().remove(&key);
            });
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{HWND, Rc, RefCell, WindowMouse};

    /// Layered Window 的非 Windows mock：持有 BGRA 缓冲区，窗口操作为空实现。
    pub struct LayeredWindow {
        width: u32,
        height: u32,
        buffer: Vec<u8>,
        /// 保留注册的鼠标处理器以维持 API 一致（非 Windows 下永不触发）。
        _mouse: RefCell<Option<Rc<RefCell<dyn WindowMouse>>>>,
    }

    impl LayeredWindow {
        pub fn create(
            _parent: Option<HWND>,
            width: u32,
            height: u32,
            _class_name: &str,
        ) -> Result<Self, String> {
            Ok(Self {
                width,
                height,
                buffer: vec![0u8; (width * height * 4) as usize],
                _mouse: RefCell::new(None),
            })
        }

        pub fn hwnd(&self) -> HWND {
            HWND::default()
        }

        pub fn register_mouse(&self, handler: Rc<RefCell<dyn WindowMouse>>) {
            *self._mouse.borrow_mut() = Some(handler);
        }

        pub fn buffer(&self) -> &[u8] {
            &self.buffer
        }

        pub fn buffer_mut(&mut self) -> &mut [u8] {
            &mut self.buffer
        }

        pub fn resize(&mut self, width: u32, height: u32) {
            self.width = width;
            self.height = height;
            self.buffer.resize((width * height * 4) as usize, 0);
        }

        pub fn update(&self) -> Result<(), String> {
            Ok(())
        }

        pub fn update_with_alpha(&self, _alpha: u8) -> Result<(), String> {
            Ok(())
        }

        pub fn show(&self, _x: i32, _y: i32) {}

        pub fn show_z(&self, _x: i32, _y: i32, _z: super::ShowZOrder) {}

        pub fn hide(&self) {}

        pub fn clear(&mut self) {
            self.buffer.fill(0);
        }

        pub fn size(&self) -> (u32, u32) {
            (self.width, self.height)
        }

        pub fn capture_to_file(&self, path: &std::path::Path) -> Result<(), String> {
            crate::screenshot::save_bgra_to_png(&self.buffer, self.width, self.height, path)
        }

        pub fn capture_to_clipboard(&self) -> Result<(), String> {
            crate::screenshot::copy_bgra_to_clipboard(&self.buffer, self.width, self.height)
        }
    }
}

pub use platform::LayeredWindow;

// 非 Windows mock 的冒烟测试：仅验证 mock 的缓冲区契约（尺寸/resize/clear）。
// 边界：真实 Layered Window 行为（UpdateLayeredWindow 透明渲染、show/hide 定位、
// wnd_proc 鼠标消息分发）在非 Windows 是空实现，**不在此覆盖，须 Windows 实测**。
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn mock_window_buffer_matches_size() {
        let mut w = LayeredWindow::create(None, 10, 4, "test").unwrap();
        assert_eq!(w.size(), (10, 4));
        assert_eq!(w.buffer().len(), 10 * 4 * 4);

        w.resize(20, 5);
        assert_eq!(w.size(), (20, 5));
        assert_eq!(w.buffer().len(), 20 * 5 * 4);

        // buffer_mut 写入后 clear 应清零
        w.buffer_mut()[0] = 0xAB;
        assert_eq!(w.buffer()[0], 0xAB);
        w.clear();
        assert!(w.buffer().iter().all(|&b| b == 0));
    }

    #[test]
    fn mock_window_show_hide_update_are_noops() {
        let w = LayeredWindow::create(None, 2, 2, "test").unwrap();
        w.show(1, 1);
        w.hide();
        assert!(w.update().is_ok());
    }
}
