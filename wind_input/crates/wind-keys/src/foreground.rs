//! 前台窗口的全屏形态探测（Windows），供协调器与 UI 线程共用。
//!
//! 放在本 crate 而不是协调器里，是因为**要在两个地方判**：
//! - 协调器在焦点/激活事件时异步探一次、缓存起来（工具栏 `hide_in_fullscreen`、候选窗
//!   「D3D 独占全屏不弹」），那是事件驱动的，游戏**在激活之后**才切进独占全屏时它就过期了。
//!   同一盲区的日常形态是浏览器全屏播放视频 / 按 F11：焦点没动，系统不回调任何东西，缓存
//!   一直停在旧值。协调器为此另有一条 500ms 复查线程（`coordinator/fullscreen_watch.rs`），
//!   但它**只覆盖工具栏那一格**——开销闸是工具栏的三项合取，而全屏游戏几乎不会有 TSF 可编辑
//!   上下文，上一句那个独占全屏的盲区仍然只由下面这道 UI 线程闸兜住；
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

/// 纯几何内核：`outer` 是否完整盖住 `inner`（边界相等算盖住）。
///
/// 抽出来是为了可测——两个调用点都得先向系统取前台窗口与显示器矩形，那部分测不了，
/// 而真正要锁住的性质是「**最大化窗口不算覆盖**」：它底边止于任务栏，正属于必须继续
/// 把任务栏算进占用的那一类。与 `wind-ui` 的 `clamp_content_in_bounds` 同一手法。
///
/// DPI 无关：`rcMonitor` 与 `GetWindowRect` 在 DPI-aware 进程里同为物理像素、同一参照系，
/// 纯比较不受缩放影响。两个矩形都是 (left, top, right, bottom)。
///
/// 非 Windows 下唯一的调用者是本模块的测试（生产调用点都在 `cfg(windows)` 内），
/// 故 lib 单独编译时它无人使用——测试本身仍跨平台跑，不能删。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn rect_covers(outer: (i32, i32, i32, i32), inner: (i32, i32, i32, i32)) -> bool {
    let (ol, ot, orr, ob) = outer;
    let (il, it, ir, ib) = inner;
    ol <= il && ot <= it && orr >= ir && ob >= ib
}

/// 当前前台窗口，排除桌面与 Shell 这两个恒存在的窗口。`None` = 没有可判的前台。
#[cfg(windows)]
fn foreground_window() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetDesktopWindow, GetForegroundWindow, GetShellWindow,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == HWND::default() || hwnd == GetDesktopWindow() || hwnd == GetShellWindow() {
            return None;
        }
        Some(hwnd)
    }
}

/// `hwnd` 的矩形是否铺满 `m` 所描述的显示器 —— 判据②的**唯一实现**，两个调用点共用。
///
/// ⛔ 别再在别处重写这个谓词：几何比较只是它的一半，下面两道守卫才是它能用的原因，
/// 而那两道都是实测命中后补上的，照抄几何部分会把它们一起丢掉。
///
/// ── 两道守卫的共同前提：矩形铺满 ≠ 用户在看一个全屏应用 ──
/// 桌面上存在若干"矩形精确等于显示器"的系统窗口，它们只是壳 UI 的容器，大部分区域
/// 透明。焦点切换的一两毫秒中间态里它们可能短暂成为前台，而探测恰好在那时采样，于是
/// 每次跨窗口切换都可能被误判成全屏。
#[cfg(windows)]
fn window_covers_monitor(
    hwnd: windows::Win32::Foundation::HWND,
    m: &windows::Win32::Foundation::RECT,
) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowRect};
    unsafe {
        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return false;
        }
        if !rect_covers(
            (wr.left, wr.top, wr.right, wr.bottom),
            (m.left, m.top, m.right, m.bottom),
        ) {
            return false;
        }

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
                "window_covers_monitor=false 矩形铺满但 DWM cloaked={} class={}（隐形系统覆盖窗口，非真全屏）",
                cloaked,
                foreground_class_name(hwnd)
            );
            return false;
        }

        // 守卫②：窗口属于 shell 进程（explorer）—— 它承载的铺满窗口都是壳 UI。
        // 实测命中 XamlExplorerHostIslandWindow（Win11 开始菜单/任务视图/搜索的 XAML 岛宿主，
        // rect 精确等于显示器且**不是** cloaked，守卫①拦不住）；Progman 虽已被 foreground_window
        // 的 GetShellWindow 排除，也落在本规则内。
        // 判据取"与 GetShellWindow 同进程"而非硬编码类名——壳 UI 的类名会随 Windows 版本增删，
        // 名单永远追不齐；而"全屏应用不会由 explorer.exe 承载"这一条长期成立。
        // 代价：文件管理器按 F11 真全屏时不再隐藏工具栏，可接受。
        let shell_pid = window_pid(GetShellWindow());
        let fg_pid = window_pid(hwnd);
        if shell_pid != 0 && fg_pid == shell_pid {
            tracing::debug!(
                "window_covers_monitor=false 矩形铺满但属于 shell 进程 pid={} class={}（壳 UI，非全屏应用）",
                fg_pid,
                foreground_class_name(hwnd)
            );
            return false;
        }
        tracing::debug!(
            "window_covers_monitor=true class={} rect=({},{},{},{}) monitor=({},{},{},{})",
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

/// 前台窗口是否铺满**调用方指定的那块**显示器——等价于问「那块屏的任务栏此刻可不可见」。
///
/// ⚠ 与 [`foreground_fullscreen_kind`] 的差别**只在问哪块屏**：那个用
/// `MonitorFromWindow(前台窗口)`，答的是「前台窗口自己那块屏」；本函数由调用方给矩形，
/// 答的是「我关心的那块屏」。多屏下 A 屏的全屏游戏**不会**隐藏 B 屏的任务栏，候选窗要问的
/// 是 caret 所在那块屏，故不能用前者代替。守卫链两者共用（见 [`window_covers_monitor`]）。
///
/// 取不到前台窗口时回 `false`，即"按任务栏可见处理"——调用方据此走更保守的那条路。
#[cfg(windows)]
pub fn foreground_covers_monitor(m: &windows::Win32::Foundation::RECT) -> bool {
    foreground_window().is_some_and(|hwnd| window_covers_monitor(hwnd, m))
}

/// 前台窗口的全屏形态。
/// 对齐 Go foreground.IsForegroundFullscreen:① SHQueryUserNotificationState 报 D3D 独占/演示模式
/// ⇒ [`FullscreenKind::D3dExclusive`]; ② 前台窗口矩形 ⊇ 所在显示器物理矩形(F11/无边框全屏/
/// 远程桌面) ⇒ [`FullscreenKind::Covering`]。排除桌面/Shell 窗口。非 Windows 恒 `None`。
#[cfg(windows)]
pub fn foreground_fullscreen_kind() -> FullscreenKind {
    use windows::Win32::UI::Shell::{
        QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
    };
    let Some(hwnd) = foreground_window() else {
        return FullscreenKind::None;
    };
    unsafe {
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
        // 判据②:前台窗口矩形 ⊇ **它自己所在那块**显示器的物理矩形（含两道守卫）。
        if !window_covers_own_monitor(hwnd) {
            return FullscreenKind::None;
        }
        FullscreenKind::Covering
    }
}

/// 判据②的实现：`hwnd` 是否铺满**它自己所在那块**显示器。
#[cfg(windows)]
fn window_covers_own_monitor(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return false;
        }
        window_covers_monitor(hwnd, &mi.rcMonitor)
    }
}

/// **只问判据②**：前台窗口是否铺满它自己那块显示器（无边框全屏 / F11 / 浏览器全屏播放
/// 视频 / 远程桌面）。
///
/// 与 [`foreground_fullscreen_kind`] 的差别是**不问判据①** —— 那条要跨进程问 shell
/// （`SHQueryUserNotificationState`），是这组查询里唯一的 RPC，其余全是 user32/gdi32 的
/// 本地调用。周期性复查（`coordinator/fullscreen_watch.rs`）只服务工具栏那一格，而工具栏
/// 关心的全屏形态都落在判据②上，故用本函数把那次 RPC 省掉：**要按固定节拍反复问的东西，
/// 不能带着一次跨进程调用**。判据①管的是 D3D 独占 / PPT 放映，它另有 UI 线程那道
/// 按需的最后闸（[`exclusive_fullscreen_recent`]）负责。
///
/// 守卫链与 [`foreground_fullscreen_kind`] 共用同一实现（见 [`window_covers_monitor`]）。
#[cfg(windows)]
pub fn foreground_covers_own_monitor() -> bool {
    foreground_window().is_some_and(window_covers_own_monitor)
}

/// 非 Windows：无全屏检测，恒 `false`。
#[cfg(not(windows))]
pub fn foreground_covers_own_monitor() -> bool {
    false
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

#[cfg(test)]
mod tests {
    use super::rect_covers;

    /// 判据②的几何部分。锁住的性质是「最大化窗口不算覆盖」——它底边止于任务栏，正是
    /// 必须继续把任务栏算进占用的那一类；只有真铺满显示器，调用方才可以把任务栏那条
    /// 也当成可用空间。判错的后果：桌面上光标贴近屏幕底部时候选窗会压住任务栏。
    #[test]
    fn rect_covers_distinguishes_fullscreen_from_maximized() {
        const MON: (i32, i32, i32, i32) = (0, 0, 3840, 2160);
        // 无边框全屏：与显示器完全重合。
        assert!(rect_covers((0, 0, 3840, 2160), MON));
        // 最大化：底边止于任务栏上沿（实测任务栏约 48px）⇒ 不算覆盖。
        assert!(!rect_covers((0, 0, 3840, 2112), MON));
        // 某些全屏实现会溢出一两像素，仍算覆盖。
        assert!(rect_covers((-1, -1, 3841, 2161), MON));
        // 普通窗口。
        assert!(!rect_covers((100, 100, 800, 600), MON));
        // 副屏上的全屏窗口不该算作主屏的覆盖（正向偏移）。
        assert!(!rect_covers((3840, 0, 7680, 2160), MON));
    }

    /// 副屏在主屏**左侧**时整块坐标为负——本仓在 coordinator/status_tip 两处都有过
    /// 「负坐标翻车」的记录，几何判据必须在负半轴同样成立。
    #[test]
    fn rect_covers_holds_on_negative_coordinates() {
        const LEFT_MON: (i32, i32, i32, i32) = (-1920, 0, 0, 1080);
        assert!(
            rect_covers((-1920, 0, 0, 1080), LEFT_MON),
            "负坐标全屏应算覆盖"
        );
        assert!(
            !rect_covers((-1920, 0, 0, 1032), LEFT_MON),
            "负坐标最大化(底边止于任务栏)不应算覆盖"
        );
    }
}
