//! wind-hook：扩展程序通道的协议与纯逻辑（设计见 `docs/design/extension-hook-channel.md`）。
//!
//! 三层进程：核心 → 扩展宿主（`wind_input_ext`）→ 扩展，全程走继承的匿名管道（子进程
//! stdio），帧是 4 字节大端长度前缀 + JSON。本 crate 只放三方共用的东西：
//!
//! - [`msg`]：线上消息（握手、事件、控制）。未知类型与未知字段一律忽略，保证前向兼容。
//! - [`frame`]：帧读写。
//! - [`tier`]：授权档位与按档位裁剪事件。
//! - [`manifest`]：扩展清单 `extension.toml` 的解析与校验。
//! - [`options`]：扩展在清单里声明的选项：校验、补齐默认值。
//! - [`backoff`]：崩溃后的退避重启与熔断。
//! - [`queue`]：满了丢最旧的有界队列（高频按键不能因为一次卡顿就断开对端）。
//! - [`tap`]：核心热路径上的事件出口：无人订阅时只是一次原子读。
//! - [`stamp`]：宿主与扩展日志行的时间戳。
//!
//! 零平台依赖。进程拉起、Job 对象这些平台相关的事由宿主与服务进程自己做。

pub mod backoff;
pub mod frame;
pub mod manifest;
pub mod msg;
pub mod options;
pub mod queue;
pub mod stamp;
pub mod tap;
pub mod tier;

/// 当前协议版本。握手时双方取共同支持的最高版本。
pub const PROTOCOL_VERSION: u32 = 1;
