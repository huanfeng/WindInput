//! 单向事件推送通道：core 在 config/dict 变更、needsRestart 时向订阅者广播 JSON 事件。
//!
//! 设计参考 wind-bridge/push.rs：每连接一个 mpsc 发送端 + 独立 writer 线程；
//! [`EventSink`] 是广播句柄（可 clone，跨线程），core/dispatch 经它 `emit_*` 推事件。
//!
//! 传输：
//! - Windows: 单向 named pipe `\\.\pipe\wind_input{suffix}_events`（参考 push.rs）。
//! - unix(macOS/Linux): unix socket `..._events.sock`（见 transport.rs 的接线点）。
//!
//! 线路帧复用 wind-ipc 的 4 字节大端长度前缀 + JSON（[`EventMessage`]）。

use std::sync::{Arc, Mutex};

use serde_json::Value;
use wind_ipc::rpc::{EventMessage, encode_message};

/// 单个订阅者（writer 线程）的发送端。
struct Subscriber {
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
}

/// 每个订阅者的积压上限。事件只在配置/词库变更时发，正常情况下队列几乎是空的；
/// 满了说明设置端卡住不读，按断开处理：设置端断线后自动重连，积压期间的事件丢失。
const SUBSCRIBER_QUEUE_CAP: usize = 64;

/// 事件广播中心：持有所有订阅者发送端，`broadcast` 向全部投递（幂等、无副作用）。
#[derive(Clone)]
pub struct EventSink {
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
}

impl Default for EventSink {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 未接传输时的占位句柄：emit 仍可调用（注册订阅者前为 no-op）。
    /// 与 `new()` 等价，仅语义化命名（dispatch 默认值用）。
    pub fn disconnected() -> Self {
        Self::new()
    }

    /// 注册一个订阅者，返回其接收端（由传输层 writer 线程消费并写线路）。
    pub(crate) fn subscribe(&self) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(SUBSCRIBER_QUEUE_CAP);
        self.subscribers.lock().unwrap().push(Subscriber { tx });
        rx
    }

    /// 广播一条事件给所有订阅者，顺带移除投递失败的：writer 线程已退出（设置窗口关了）
    /// 或积压满了（卡住不读）。不移除的话设置窗口每开一次表里就多一个死订阅者。
    pub fn broadcast(&self, event: &str, data: Value) {
        let msg = EventMessage {
            event: event.to_string(),
            data,
        };
        let bytes = match encode_message(&msg) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("事件编码失败 {}: {}", event, e);
                return;
            }
        };
        self.subscribers
            .lock()
            .unwrap()
            .retain(|s| s.tx.try_send(bytes.clone()).is_ok());
    }

    /// 配置变更事件（setItems/reload 后）。事件名与前端约定一致："config.changed"。
    pub fn emit_config_changed(&self, data: Value) {
        self.broadcast("config.changed", data);
    }

    /// 兼容规则变更事件（`compat.*` 写方法成功后）。设置端据此刷新列表。
    ///
    /// ⚠ 只覆盖经 RPC 的写入：右键菜单的写入（`update_user_raw`）不经过 dispatch，**不会**
    /// 广播。设置页要看到菜单的改动，需在窗口重新获得焦点时主动重新拉取一次 `compat.list`。
    pub fn emit_compat_changed(&self, data: Value) {
        self.broadcast("compat.changed", data);
    }

    /// 方案配置变更事件（`schema.saveConfig` / `resetConfig` / `setDictEnabled` 等成功后，
    /// 载荷 `{id, method}`）。设置端开着该方案的设置对话框时据此重拉 `schema.getConfig`。
    pub fn emit_schema_config_changed(&self, data: Value) {
        self.broadcast("schema.configChanged", data);
    }

    /// 词库变更事件（dict.* 写操作后，宿主按需调用）。
    pub fn emit_dict_changed(&self, data: Value) {
        self.broadcast("dict.changed", data);
    }

    /// 需要重启才能完全生效的提示事件。
    pub fn emit_needs_restart(&self, data: Value) {
        self.broadcast("needsRestart", data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(sink: &EventSink) -> usize {
        sink.subscribers.lock().unwrap().len()
    }

    /// 设置窗口关掉后（接收端析构），下一次广播就把它移出表：此前只增不删。
    #[test]
    fn closed_subscriber_is_removed_on_broadcast() {
        let sink = EventSink::new();
        let live = sink.subscribe();
        drop(sink.subscribe());
        assert_eq!(count(&sink), 2);
        sink.emit_config_changed(Value::Null);
        assert_eq!(count(&sink), 1);
        assert!(live.try_recv().is_ok(), "活着的订阅者照常收到");
    }

    /// 卡住不读的订阅者积压到上限即移除，内存不随事件数增长。
    #[test]
    fn stalled_subscriber_is_removed_when_queue_fills() {
        let sink = EventSink::new();
        let stuck = sink.subscribe();
        for _ in 0..SUBSCRIBER_QUEUE_CAP + 1 {
            sink.emit_config_changed(Value::Null);
        }
        assert_eq!(count(&sink), 0);
        assert_eq!(stuck.try_iter().count(), SUBSCRIBER_QUEUE_CAP);
    }
}
