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
pub mod handle_charset;
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
pub mod handle_uielement;
pub mod handle_url;
pub mod host_services;
pub mod hotkey_match;
pub mod input_diag;
pub mod key_convert;
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
#[cfg(windows)]
pub mod tsf_profile_name;
/// UI 命令发送端：把「投递 + 唤醒 UI 线程」绑成一次操作，见模块文档。
pub mod ui_sender;
pub mod watchdog;
pub mod web_host;

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

// 全屏形态探测搬到 wind-keys：UI 线程显示浮窗前还要再判一次（最后一道闸），而
// wind-ui 不能依赖本 crate。见 `wind_keys::foreground` 模块注释。
pub(crate) use wind_keys::foreground::{FullscreenKind, foreground_fullscreen_kind};
