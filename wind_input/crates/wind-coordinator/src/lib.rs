//! wind-coordinator: 中央协调器（按键路由、候选管理、模式切换）
//!
//! 与 Go 版本 `wind_input/internal/coordinator/` 对齐。

pub mod auto_phrase;
pub(crate) mod candidate_nav;
pub mod candidate_pull;
#[cfg(test)]
pub(crate) mod charset_test_support;
pub(crate) mod comment;
pub(crate) mod config_bundle;
pub(crate) mod construct;
pub mod coordinator;
pub(crate) mod debug_support;
#[cfg(windows)]
pub mod direct_switch;
pub mod edit_ops;
pub(crate) mod english_candidates;
#[cfg(test)]
mod freq_learn_tests;
pub mod handle_addword;
pub mod handle_assoc;
pub mod handle_aux_code;
pub mod handle_candidate;
pub mod handle_cmdbar;
#[cfg(target_os = "macos")]
pub mod handle_cmdbar_macos;
pub mod handle_common_chars;
pub mod handle_config;
pub mod handle_key;
pub mod handle_lifecycle;
pub mod handle_menu;
pub mod handle_mode;
pub mod handle_punct;
pub mod handle_quick_format;
mod handle_softkeyboard;
pub mod handle_special;
pub mod handle_temp;
pub mod handle_tooltip;
pub mod handle_url;
pub mod host_services;
pub mod hotkey_match;
pub mod input_diag;
pub(crate) mod key_convert;
pub mod key_gate;
pub(crate) mod key_resolver;
pub mod layout;
pub mod pipeline;
pub(crate) mod preedit_cursor;
mod quick_eval;
pub(crate) mod schema_scope;
pub(crate) mod short_code_yield;
pub mod stats;
pub mod theme_query;
pub mod theme_style;
/// UI 命令发送端：把「投递 + 唤醒 UI 线程」绑成一次操作，见模块文档。
pub mod ui_sender;
pub mod watchdog;
pub mod web_host;

pub use coordinator::{Coordinator, request_restart, restart_signal, set_settings_url_provider};
pub use ui_sender::UiSender;

/// 前台窗口的类名（诊断用，最长 63 字符）。只取类名不取标题——标题常含文件名等用户信息。
#[cfg(windows)]
fn foreground_class_name(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 窗口所属进程 ID（0 = 查询失败）。
#[cfg(windows)]
fn window_pid(hwnd: windows::Win32::Foundation::HWND) -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// 当前前台窗口所属进程 ID（0 = 无前台窗口或查询失败）。
///
/// 供 `handle_client_connected` 判断「刚建立连接的这个宿主是否真的在前台」——pid 只说明
/// 哪个进程打开了管道，不代表它现在有焦点，不加这层判断会让一条无关的重连（后台窗口的
/// 管道抖动）覆盖掉真正聚焦应用的 per-app 兼容态。
#[cfg(windows)]
pub(crate) fn foreground_pid() -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    window_pid(unsafe { GetForegroundWindow() })
}

/// 前台窗口是否全屏（供工具栏 ui.toolbar.hide_in_fullscreen 判定）。
/// 对齐 Go foreground.IsForegroundFullscreen:① SHQueryUserNotificationState 报 D3D 独占/演示模式;
/// ② 前台窗口矩形 ⊇ 所在显示器物理矩形(F11/无边框全屏/远程桌面)。排除桌面/Shell 窗口。
/// 非 Windows 恒 false。
#[cfg(windows)]
pub(crate) fn is_foreground_fullscreen() -> bool {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows::Win32::UI::Shell::{
        QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetDesktopWindow, GetForegroundWindow, GetShellWindow, GetWindowRect,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == HWND::default() || hwnd == GetDesktopWindow() || hwnd == GetShellWindow() {
            return false;
        }
        // 判据①:系统通知状态(游戏 D3D 独占 / PPT 放映等系统级全屏)。
        if let Ok(state) = SHQueryUserNotificationState()
            && (state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE)
        {
            tracing::debug!(
                "is_foreground_fullscreen=true 判据①(通知状态) state={} class={}",
                state.0,
                foreground_class_name(hwnd)
            );
            return true;
        }
        // 判据②:前台窗口矩形 ⊇ 显示器物理矩形。
        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return false;
        }
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        if !(wr.left <= m.left && wr.top <= m.top && wr.right >= m.right && wr.bottom >= m.bottom) {
            return false;
        }
        // ── 以下两道守卫的共同前提：矩形铺满 ≠ 用户在看一个全屏应用 ──
        // 桌面上存在若干"矩形精确等于显示器"的系统窗口，它们只是壳 UI 的容器，大部分区域
        // 透明。焦点切换的一两毫秒中间态里它们可能短暂成为前台，而 notify_toolbar_async 的
        // 探测线程恰好在那时采样，于是 IME 每次跨窗口切换都可能被误判成全屏、隐藏工具栏。

        // 守卫①：DWM cloaked —— 窗口存在但合成器没在渲染它。
        // 实测命中：ClickToDo 的 IslandWindow(cloaked=1)、TextInputHost 的
        // Windows.UI.Core.CoreWindow(cloaked=2)。注意 IsWindowVisible 对这类窗口仍返回 true，
        // 几何上也确实铺满，只有 DWMWA_CLOAKED 能分辨。
        let mut cloaked: u32 = 0;
        let hr = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        // 查询失败（旧系统/无 DWM）时按未 cloaked 处理，保持既有行为。
        if hr.is_ok() && cloaked != 0 {
            tracing::debug!(
                "is_foreground_fullscreen=false 矩形铺满但 DWM cloaked={} class={}（隐形系统覆盖窗口，非真全屏）",
                cloaked,
                foreground_class_name(hwnd)
            );
            return false;
        }

        // 守卫②：窗口属于 shell 进程（explorer）—— 它承载的铺满窗口都是壳 UI。
        // 实测命中 XamlExplorerHostIslandWindow（Win11 开始菜单/任务视图/搜索的 XAML 岛宿主，
        // rect 精确等于显示器且**不是 cloaked**，守卫①拦不住）；Progman 虽已被函数开头的
        // GetShellWindow 排除，也落在本规则内。
        // 判据取"与 GetShellWindow 同进程"而非硬编码类名——壳 UI 的类名会随 Windows 版本增删，
        // 名单永远追不齐；而"全屏应用不会由 explorer.exe 承载"这一条长期成立。
        // 代价：文件管理器按 F11 真全屏时不再隐藏工具栏，可接受。
        let shell_pid = window_pid(GetShellWindow());
        let fg_pid = window_pid(hwnd);
        if shell_pid != 0 && fg_pid == shell_pid {
            tracing::debug!(
                "is_foreground_fullscreen=false 矩形铺满但属于 shell 进程 pid={} class={}（壳 UI，非全屏应用）",
                fg_pid,
                foreground_class_name(hwnd)
            );
            return false;
        }
        tracing::debug!(
            "is_foreground_fullscreen=true 判据②(矩形铺满) class={} rect=({},{},{},{}) monitor=({},{},{},{})",
            foreground_class_name(hwnd),
            wr.left,
            wr.top,
            wr.right,
            wr.bottom,
            m.left,
            m.top,
            m.right,
            m.bottom
        );
        true
    }
}

/// 非 Windows:无全屏检测,恒 false(工具栏不因全屏隐藏)。
#[cfg(not(windows))]
pub(crate) fn is_foreground_fullscreen() -> bool {
    false
}
