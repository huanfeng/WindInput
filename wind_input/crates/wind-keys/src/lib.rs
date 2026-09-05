//! wind-keys: 键名映射与按键注入（平台层）
//!
//! 从 wind-coordinator 抽出，便于纯逻辑（键名→VK 映射、导航键分类、combo 解析）原生测试。
//! - [`keymap`]：纯键名/VK 映射 + 导航键分类（无平台依赖）。
//! - [`key_inject`]：combo 解析（纯）+ 平台按键注入（`SysKeys`，Win32 SendInput）。
//!
//! **平台对接**：按键注入按 `cfg` 分平台——Windows 用 Win32 SendInput；macOS 待补
//! （CGEvent，见 key_inject 的 `cfg(target_os = "macos")` 桩）；其他 Unix 为 no-op。
pub mod capslock_hook;
pub mod foreground;
pub mod key_inject;
pub mod keymap;
