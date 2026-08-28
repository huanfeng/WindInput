//! wind-ui: UI 渲染层（tiny-skia 渲染、Layered Window、多种窗口类型）
//!
//! 与 Go 版本 `wind_input/internal/ui/` 对齐。
//!
//! # 跨平台与测试边界
//!
//! 本 crate 在非 Windows 平台（Linux/macOS）以 mock 编译，便于在 Linux 上跑测试。
//! 但并非所有逻辑在 Linux 都是“真实”验证——按可测性分三类：
//!
//! - **跨平台真实**（Linux 测试 == Windows 行为）：[`view`] 的盒模型布局
//!   （measure/arrange/collect_hits）与形状绘制（fill_rounded/circle/ring/shadow，
//!   基于纯 Rust 的 tiny-skia 光栅化）、[`debounce`]、[`image_cache`]。
//! - **mock 近似**（Linux 可测但数值是占位）：[`text::dwrite`] 的文本测量在非 Windows
//!   返回 `字符数 × 字号 × 0.6` 的等宽近似；真实字形宽度需 Windows + DirectWrite。
//!   含文本的布局测试因此 gate 到 `not(windows)`，以 mock 的确定尺寸做精确断言。
//! - **仅占位、Linux 测不到真实行为**（必须 Windows 回归）：[`window`] 的 Layered Window
//!   （UpdateLayeredWindow/消息分发）、[`text::dwrite`] 的实际字形渲染、popup_menu 剪贴板、
//!   manager 的 Win32 消息泵。这些在非 Windows 是空实现，其测试仅验证 mock 的 API 契约。

pub mod auto_hide;
pub mod candidate_window;
pub mod debounce;
pub mod dpi;
/// macOS 全局热键（Carbon RegisterEventHotKey）。对位 Windows 的 RegisterHotKey 分支。
#[cfg(target_os = "macos")]
pub mod global_hotkey_macos;
pub mod image_cache;
pub mod input_diag_hud;
/// macOS 输入源切换（TISSelectInputSource）。对位 Windows 的 DirectSwitchHotkeys 注册表。
#[cfg(target_os = "macos")]
pub mod input_source_macos;
/// 语言栏图标（Windows TSF 输入指示器）的离屏渲染。
///
/// 渲染逻辑本身平台无关（几何绘制 + 蒙版合成），故不整模块 cfg——非 Windows 上
/// 文本后端是 mock，字形部分为空，但角标与合成逻辑仍可被 CI 的 Linux test job 覆盖。
pub mod langbar_icon;
pub mod manager;
/// macOS host-render forwarder：把 UiCommand 光栅化进 POSIX SHM + push 推帧给 .app。
#[cfg(target_os = "macos")]
pub mod manager_macos;
pub mod popup_menu;
pub mod screenshot;
pub mod soft_keyboard;
pub mod status_tip;
pub mod sys;
/// macOS 系统明暗变更监听。对位 Windows 消息泵里的 `WM_SETTINGCHANGE`。
#[cfg(target_os = "macos")]
pub mod system_theme_macos;
pub mod text;
pub mod theme_assets;
pub mod toast;
pub mod toolbar;
pub mod toolbar_gate;
pub mod tooltip;
pub mod view;
/// UI 线程唤醒原语：消息循环据此「睡到有事发生」，取代原先的固定周期轮询。
pub mod wake;
pub mod window;

pub use manager::UiManager;
