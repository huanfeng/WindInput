//! wind-coordinator: 中央协调器（按键路由、候选管理、模式切换）
//!
//! 与 Go 版本 `wind_input/internal/coordinator/` 对齐。

pub(crate) mod aux_code_source;
pub(crate) mod candidate_nav;
pub mod candidate_pull;
#[cfg(test)]
pub(crate) mod charset_test_support;
pub(crate) mod comment;
/// 设置页预览行的模板样例求值（`comment` 本身不对外，只开放这一块）。
pub use comment::preview as template_preview;
pub(crate) mod config_bundle;
pub(crate) mod construct;
pub mod coordinator;
pub(crate) mod data_needs;
pub(crate) mod debug_support;
#[cfg(windows)]
pub mod direct_switch;
pub mod draft_window;
pub mod edit_ops;
#[cfg(test)]
mod email_mode_tests;
pub(crate) mod english_candidates;
pub(crate) mod english_learn;
#[cfg(test)]
mod freq_learn_tests;
pub mod handle_addword;
pub mod handle_assoc;
pub mod handle_aux_code;
pub mod handle_candidate;
pub mod handle_charset;
pub mod handle_cmdbar;
#[cfg(ext_presenter)]
pub mod handle_cmdbar_macos;
pub mod handle_common_chars;
pub mod handle_config;
pub mod handle_direct_aux;
pub mod handle_draft;
pub mod handle_email;
pub mod handle_key;
pub mod handle_lifecycle;
pub mod handle_menu;
pub mod handle_mode;
pub mod handle_punct;
pub mod handle_quick_format;
mod handle_reverse;
mod handle_softkeyboard;
pub mod handle_special;
pub mod handle_temp;
pub mod handle_tooltip;
pub mod handle_uielement;
pub mod handle_unicode;
mod handle_url;
pub(crate) mod heap_trim;
pub mod host_services;
pub mod hotkey_match;
pub mod input_diag;
pub mod key_convert;
pub mod key_gate;
pub(crate) mod key_resolver;
pub mod layout;
pub mod mode_completion;
pub mod pipeline;
pub(crate) mod preedit_cursor;
mod quick_eval;
pub(crate) mod quick_history;
pub(crate) mod schema_scope;
pub(crate) mod short_code_yield;
pub mod stats;
pub mod theme_query;
pub mod theme_style;
pub(crate) mod tooltip;
#[cfg(windows)]
pub mod tsf_profile_name;
/// UI 命令发送端：把「投递 + 唤醒 UI 线程」绑成一次操作，见模块文档。
pub mod ui_sender;
pub mod watchdog;
pub mod web_host;
pub(crate) mod wildcard;

pub use coordinator::{Coordinator, request_restart, restart_signal, set_settings_url_provider};
pub use ui_sender::UiSender;

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

/// 当前持有键盘焦点的窗口所属进程 ID（0 = 查询失败）。
///
/// 取前台线程的 `GetGUIThreadInfo().hwndFocus`：跨进程嵌入的子窗口（WebView2 等）与前台
/// 顶层窗口共用输入队列，焦点落在子进程窗口时这里给出的是子进程——只看前台窗口会把它们
/// 全算到宿主头上。拿不到焦点窗口时退回前台窗口的进程。
#[cfg(windows)]
pub(crate) fn focus_owner_pid() -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{
        GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
    };
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return 0;
    }
    let tid = unsafe { GetWindowThreadProcessId(fg, None) };
    let mut gti = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if tid != 0 && unsafe { GetGUIThreadInfo(tid, &mut gti) }.is_ok() && !gti.hwndFocus.is_invalid()
    {
        let pid = window_pid(gti.hwndFocus);
        if pid != 0 {
            return pid;
        }
    }
    window_pid(fg)
}

#[cfg(not(windows))]
pub(crate) fn focus_owner_pid() -> u32 {
    0
}

/// `sender` 是否是后台客户端：焦点在别的进程，且那个进程正是当前活动客户端。
///
/// 只在「活动客户端 = 焦点持有者」时才判后台——焦点查询拿不准（0）、或活动客户端本身
/// 已不是焦点持有者（它可能是陈旧的）时一律不判，保持旧行为：错判的代价是真正切过去
/// 的宿主抢不到激活态，比放过一次初始化噪声严重得多。
pub(crate) fn is_background_sender(sender_pid: u32, active_pid: u32, focus_pid: u32) -> bool {
    sender_pid != 0 && focus_pid != 0 && sender_pid != focus_pid && active_pid == focus_pid
}

// 全屏形态探测搬到 wind-keys：UI 线程显示浮窗前还要再判一次（最后一道闸），而
// wind-ui 不能依赖本 crate。见 `wind_keys::foreground` 模块注释。
pub(crate) use wind_keys::foreground::{FullscreenKind, foreground_fullscreen_kind};

#[cfg(test)]
mod background_sender_tests {
    use super::is_background_sender;

    #[test]
    fn webview_child_during_searchhost_typing_is_background() {
        // 2026-10-09：SearchHost(8092) 正在输入，其 WebView2 子进程(27168) 初始化 TSF。
        assert!(is_background_sender(27168, 8092, 8092));
    }

    #[test]
    fn real_focus_switch_is_not_background() {
        // 焦点已到发送方：正常的切应用。
        assert!(!is_background_sender(27168, 8092, 27168));
        // 同进程（别的线程）也不判。
        assert!(!is_background_sender(8092, 8092, 8092));
    }

    #[test]
    fn unsure_cases_keep_legacy_behavior() {
        assert!(!is_background_sender(27168, 8092, 0), "焦点查询失败不判");
        assert!(
            !is_background_sender(27168, 0, 8092),
            "还没有活动客户端不判"
        );
        assert!(
            !is_background_sender(27168, 1234, 8092),
            "活动客户端已不持焦点（陈旧）不判"
        );
        assert!(!is_background_sender(0, 8092, 8092), "发送方 pid 未知不判");
    }
}
