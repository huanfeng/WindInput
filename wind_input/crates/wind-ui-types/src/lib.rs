//! **wind-ui-types：协调器 ↔ 任意前端的表现层协议（纯数据）。**
//!
//! 正向 [`UiCommand`]（协调器 → 渲染端）、反向 [`UiEvent`]（渲染端 → 协调器）
//! 及其载荷类型。此处 "ui" 指**表现层边界**，不特指 wind-ui crate——消费者包括
//! 桌面渲染线程（wind-ui）、macOS `.app`（经 host-render 转发）、Android Kotlin 壳
//! （经 FFI 回调）。
//!
//! 这是**进程内契约，非线协议**（线协议见 wind-ipc）：值经 `mpsc` 通道流转，
//! 从不编码上线。本 crate 不含任何渲染实现与平台调用；唯一的平台例外是
//! `#[cfg(windows)]` 的 [`UiCommand::SetHostRender`]（Windows 宿主渲染注入，
//! 载荷经 target-specific 依赖引入，别的 target 下不存在）。
//!
//! 约束（新增内容前先读 AGENTS.md）：仅纯数据与纯映射逻辑；零 IO、零日志、零 serde；
//! 新增依赖必须通过 `aarch64-linux-android` 的 `cargo check`。

pub mod candidate;
pub mod command;
pub mod diag;
pub mod event;
pub mod menu;
pub mod softkeyboard;
pub mod toast;
pub mod toolbar;

pub use candidate::*;
pub use command::*;
pub use diag::*;
pub use event::*;
pub use menu::*;
pub use softkeyboard::*;
pub use toast::*;
pub use toolbar::*;
