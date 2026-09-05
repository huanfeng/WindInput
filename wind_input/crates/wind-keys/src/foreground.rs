//! 前台窗口的全屏形态探测（Windows），供协调器与 UI 线程共用。
//!
//! 放在本 crate 而不是协调器里，是因为**要在两个地方判**：
//! - 协调器在焦点/激活事件时异步探一次、缓存起来（工具栏 `hide_in_fullscreen`、候选窗
//!   「D3D 独占全屏不弹」），那是事件驱动的，游戏**在激活之后**才切进独占全屏时它就过期了；
//! - UI 线程在**真正要显示某个浮窗的那一刻**再判一次（[`exclusive_fullscreen_recent`]），
//!   这是最后一道闸——独占全屏的游戏被别的进程的窗口盖一下就会被踢出独占态，处理不好
//!   的游戏直接卡死（Dota 2 实测），一帧都不能漏。
//!
//! 两处都只问「现在是不是」，不做任何状态推断。UI 线程那次带一个短 TTL 缓存，把
//! `SHQueryUserNotificationState` 这类跨进程查询的代价压到每几百毫秒一次。

/// 前台窗口的全屏形态。两种形态对浮窗的后果**不同**，故不能压成一个 bool：
///
/// - [`Self::D3dExclusive`]：D3D/DXGI **独占**全屏（判据①）。别的进程的窗口一旦盖上来，
///   系统就把游戏踢出独占态（画面闪黑、分辨率切换，处理不好的游戏直接卡死）——我们的
///   浮窗本来就显示不出来，弹出去只剩副作用，**必须不弹**。
/// - [`Self::Covering`]：窗口矩形铺满显示器（无边框全屏 / F11 / 远程桌面，判据②）。
///   普通窗口叠加没有问题，候选窗照常显示；只有工具栏按 `hide_in_fullscreen` 选择隐藏。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullscreenKind {
    None,
    D3dExclusive,
    Covering,
}

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

/// 前台窗口的全屏形态。
/// 对齐 Go foreground.IsForegroundFullscreen:① SHQueryUserNotificationState 报 D3D 独占/演示模式
/// ⇒ [`FullscreenKind::D3dExclusive`]; ② 前台窗口矩形 ⊇ 所在显示器物理矩形(F11/无边框全屏/
/// 远程桌面) ⇒ [`FullscreenKind::Covering`]。排除桌面/Shell 窗口。非 Windows 恒 `None`。
#[cfg(windows)]
pub fn foreground_fullscreen_kind() -> FullscreenKind {
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
            return FullscreenKind::None;
        }
        // 判据①:系统通知状态(游戏 D3D 独占 / PPT 放映等系统级全屏)。
        if let Ok(state) = SHQueryUserNotificationState()
            && (state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE)
        {
            tracing::debug!(
                "foreground_fullscreen_kind=D3dExclusive 判据①(通知状态) state={} class={}",
                state.0,
                foreground_class_name(hwnd)
            );
            return FullscreenKind::D3dExclusive;
        }
        // 判据②:前台窗口矩形 ⊇ 显示器物理矩形。
        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return FullscreenKind::None;
        }
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return FullscreenKind::None;
        }
        let m = mi.rcMonitor;
        if !(wr.left <= m.left && wr.top <= m.top && wr.right >= m.right && wr.bottom >= m.bottom) {
            return FullscreenKind::None;
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
                "foreground_fullscreen_kind=None 矩形铺满但 DWM cloaked={} class={}（隐形系统覆盖窗口，非真全屏）",
                cloaked,
                foreground_class_name(hwnd)
            );
            return FullscreenKind::None;
        }

        // 守卫②：窗口属于 shell 进程（explorer）—— 它承载的铺满窗口都是壳 UI。
        // 实测命中 XamlExplorerHostIslandWindow（Win11 开始菜单/任务视图/搜索的 XAML 岛宿主，
        // rect 精确等于显示器且**不是** cloaked，守卫①拦不住）；Progman 虽已被函数开头的
        // GetShellWindow 排除，也落在本规则内。
        // 判据取"与 GetShellWindow 同进程"而非硬编码类名——壳 UI 的类名会随 Windows 版本增删，
        // 名单永远追不齐；而"全屏应用不会由 explorer.exe 承载"这一条长期成立。
        // 代价：文件管理器按 F11 真全屏时不再隐藏工具栏，可接受。
        let shell_pid = window_pid(GetShellWindow());
        let fg_pid = window_pid(hwnd);
        if shell_pid != 0 && fg_pid == shell_pid {
            tracing::debug!(
                "foreground_fullscreen_kind=None 矩形铺满但属于 shell 进程 pid={} class={}（壳 UI，非全屏应用）",
                fg_pid,
                foreground_class_name(hwnd)
            );
            return FullscreenKind::None;
        }
        tracing::debug!(
            "foreground_fullscreen_kind=Covering 判据②(矩形铺满) class={} rect=({},{},{},{}) monitor=({},{},{},{})",
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
        FullscreenKind::Covering
    }
}

/// 非 Windows:无全屏检测,恒 None(工具栏不因全屏隐藏、候选窗不因独占全屏抑制)。
#[cfg(not(windows))]
pub fn foreground_fullscreen_kind() -> FullscreenKind {
    FullscreenKind::None
}

/// UI 线程显示浮窗前的最后一道闸：前台**现在**是不是 D3D 独占全屏。
///
/// 带 [`EXCLUSIVE_PROBE_TTL`] 的缓存——浮窗显示是成串的（候选窗每键一次、气泡与工具栏
/// 常同时来），不必每次都跨进程问 shell。TTL 之内返回上一次的答案。
///
/// 与协调器那份事件驱动的缓存并存而不是取代它：那份决定「要不要**发**显示命令」，本函数
/// 决定「收到命令后要不要**真显示**」。前者过期了（游戏在激活后才切独占全屏）由后者兜住；
/// 后者只在 UI 线程可用。
pub fn exclusive_fullscreen_recent() -> bool {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let now = Instant::now();
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, v)) = *guard
        && now.duration_since(at) < EXCLUSIVE_PROBE_TTL
    {
        return v;
    }
    let v = foreground_fullscreen_kind() == FullscreenKind::D3dExclusive;
    // 只在翻转时记一条 info：这是排查「游戏里弹没弹窗」的直接证据，而每次显示都记会刷屏。
    if guard.map(|(_, prev)| prev) != Some(v) {
        tracing::info!(
            "UI 线程判前台 D3D 独占全屏={v}（{}）",
            if v {
                "浮窗一律不显示"
            } else {
                "浮窗恢复显示"
            }
        );
    }
    *guard = Some((now, v));
    v
}

/// [`exclusive_fullscreen_recent`] 的缓存有效期。取值依据：游戏进出独占全屏是秒级的用户
/// 操作，几百毫秒内的陈旧答案不会错过它；而候选窗连打时每键都会显示一次，TTL 太短就退化
/// 成每键一次跨进程查询。
pub const EXCLUSIVE_PROBE_TTL: std::time::Duration = std::time::Duration::from_millis(300);
