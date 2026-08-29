//! wind-rpc: core(wind_input) 的本地控制 / 配置 JSON-RPC 服务（命名管道 / unix socket）。
//!
//! 从内嵌 HTTP webapi 回退而来：去掉 axum/CORS/PNA/token/端口发现，本地授权靠 OS ACL。
//!
//! 模块：
//! - [`dispatch`]：传输无关的 JSON-RPC 分发（system.*/config.* + 转发 [`CoreRpc`]）。
//! - [`events`]：单向事件推送通道（config/dict 变更广播）。
//! - [`server`]：[`RpcServer`] + 传输抽象（windows pipe / unix socket）。
//!
//! 复用 wind-ipc 的 JSON-RPC 协议（Request/Response/EventMessage + 4 字节大端长度前缀帧）。

/// capability descriptor 生成。**公开**是为了让消费方（wind-setting 检入的
/// `capabilities.snapshot.json`）能在测试里直接对照生成结果，而不是靠人工同步一份副本
/// ——手抄的镜像必然漂移，此前该快照已积累 4 处默认值偏离、缺 2 键、多 1 键。
pub mod capabilities;
pub mod client;
/// 定制版身份的对外暴露（启动日志摘要 + `system.info` 字段）。**公开**是为了让
/// service 的启动路径与 dispatch 共用同一份文案与空值处置，见模块文档。
pub mod custom_edition;
mod dispatch;
mod events;
mod security;
mod server;

pub(crate) use dispatch::APP_VERSION;

pub use dispatch::{CoreRpc, DispatchState, dispatch};
pub use events::EventSink;
pub use server::{RpcServer, ctrl_endpoint, events_endpoint};
