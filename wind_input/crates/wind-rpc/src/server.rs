//! RPC server：传输抽象 [`RpcTransport`] + 控制通道（请求-响应）+ 事件通道（单向广播）。
//!
//! 同步线程模型（与 wind-bridge 一致，不引入 tokio 到控制路径）：
//! - 控制 server：listen → accept → 每连接一线程 → 循环 [读长度帧 Request → dispatch → 写 Response]。
//! - 事件 server：listen → accept → 注册订阅者 → writer 线程消费广播队列写线路。
//!
//! 平台传输：
//! - Windows: named pipe（`..._ctrl` / `..._events`），见 [`windows_pipe`]。
//! - unix(macOS/Linux): unix socket（`..._ctrl.sock` / `..._events.sock`），见 [`unix_socket`]。
//!
//! dispatch/协议层平台无关；windows-only 代码 `#[cfg(windows)]`，unix 用 `#[cfg(unix)]`。

use std::sync::Arc;

use wind_ipc::rpc::{Request, Response, encode_message};

use crate::dispatch::{CoreRpc, DispatchState};
use crate::events::EventSink;

/// 控制通道管道/套接字名（含变体后缀）。
pub fn ctrl_endpoint(suffix: &str) -> String {
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\wind_input_ctrl{}", suffix)
    }
    #[cfg(unix)]
    {
        unix_endpoint(suffix, "ctrl")
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = suffix;
        String::new()
    }
}

/// 事件通道管道/套接字名（含变体后缀）。
pub fn events_endpoint(suffix: &str) -> String {
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\wind_input{}_events", suffix)
    }
    #[cfg(unix)]
    {
        unix_endpoint(suffix, "events")
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = suffix;
        String::new()
    }
}

/// unix socket 路径：`$XDG_RUNTIME_DIR/wind_input{suffix}_{kind}.sock`，回退 /tmp。
/// macOS 无 XDG_RUNTIME_DIR，落到 /tmp（接线点：如需改用 ~/Library，在此调整）。
#[cfg(unix)]
fn unix_endpoint(suffix: &str, kind: &str) -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    format!("{}/wind_input{}_{}.sock", dir, suffix, kind)
}

/// RPC server 句柄：持有 dispatch 状态 + 事件广播句柄 + 变体后缀。
pub struct RpcServer {
    state: Arc<DispatchState>,
    events: EventSink,
    suffix: String,
}

impl RpcServer {
    /// 构造 server：注入 core 实现，内部建好事件广播通道并接入 dispatch。
    pub fn new(
        core: Arc<dyn CoreRpc>,
        variant: &'static str,
        suffix: &str,
    ) -> anyhow::Result<Self> {
        let events = EventSink::new();
        let state = Arc::new(DispatchState::with_events(core, variant, events.clone())?);
        Ok(Self {
            state,
            events,
            suffix: suffix.to_string(),
        })
    }

    /// 事件广播句柄（core 在 dict 变更等处调用 `emit_*` 推事件）。
    pub fn event_sink(&self) -> EventSink {
        self.events.clone()
    }

    /// 启动控制 + 事件两个 server（各自后台线程，立即返回）。
    pub fn start(&self) -> anyhow::Result<()> {
        self.start_ctrl()?;
        self.start_events()?;
        Ok(())
    }

    /// 启动控制 server（请求-响应）。
    pub fn start_ctrl(&self) -> anyhow::Result<()> {
        let endpoint = ctrl_endpoint(&self.suffix);
        let state = self.state.clone();
        tracing::info!("RPC ctrl server starting on {:?}", endpoint);
        std::thread::Builder::new()
            .name("rpc-ctrl-server".into())
            .spawn(move || transport::run_ctrl_server(&endpoint, state))?;
        Ok(())
    }

    /// 启动事件 server（单向广播）。
    pub fn start_events(&self) -> anyhow::Result<()> {
        let endpoint = events_endpoint(&self.suffix);
        let sink = self.events.clone();
        tracing::info!("RPC events server starting on {:?}", endpoint);
        std::thread::Builder::new()
            .name("rpc-events-server".into())
            .spawn(move || transport::run_events_server(&endpoint, sink))?;
        Ok(())
    }
}

/// 处理一帧请求字节，返回响应帧字节（4 字节大端长度前缀 + JSON）。
/// dispatch 平台无关，供各传输实现复用；`trusted` 由传输层按连接对端核验。
pub(crate) fn handle_frame(state: &DispatchState, frame: &[u8], trusted: bool) -> Vec<u8> {
    let resp = match serde_json::from_slice::<Request>(frame) {
        Ok(req) => crate::dispatch::dispatch_as(state, req, trusted),
        Err(e) => {
            tracing::warn!("RPC 请求解析失败: {}", e);
            // id 未知，返回 id=0 的错误响应。
            Response::error(0, format!("invalid request: {e}"))
        }
    };
    encode_message(&resp).unwrap_or_else(|e| {
        tracing::error!("RPC 响应编码失败: {}", e);
        Vec::new()
    })
}

// ──────────────────────────────────────────────
// 传输实现：按平台二选一
// ──────────────────────────────────────────────

#[cfg(windows)]
#[path = "transport_windows.rs"]
mod transport;

#[cfg(all(unix, not(windows)))]
#[path = "transport_unix.rs"]
mod transport;

#[cfg(not(any(windows, unix)))]
mod transport {
    use super::*;
    pub(crate) fn run_ctrl_server(_endpoint: &str, _state: Arc<DispatchState>) {
        tracing::warn!("RPC ctrl server not supported on this platform");
    }
    pub(crate) fn run_events_server(_endpoint: &str, _sink: EventSink) {
        tracing::warn!("RPC events server not supported on this platform");
    }
}
