//! 向 UI 线程投递命令的发送端。
//!
//! ## 为什么不是裸的 `mpsc::Sender`
//!
//! UI 线程改为事件驱动后（见 `wind_ui::wake`），它空闲时真的在睡——不再每 8ms 醒来查一遍
//! 通道。于是「投递一条命令」不再等价于「这条命令会被处理」：还得把线程叫醒。
//!
//! 协调器里有 50 余处 `ui_tx.send`，散落在按键、焦点、菜单、配置各条路径上。靠纪律在每一
//! 处补一句唤醒是不可行的——**漏掉任何一处，那条路径就变成「UI 偶尔不更新」**：它只在
//! 「恰好没有别的事把线程叫醒」时发作，既不报错也难复现，是那种要查很久的 bug。
//!
//! 故把顺序固化在本类型的 [`UiSender::send`] 里。调用方写不出「只投递不唤醒」，新增的
//! 发送点也自动获得唤醒——**这是编译器保证的，不是靠记得**。

use std::sync::Arc;
use std::sync::mpsc;
use wind_ui_types::UiCommand;

/// UI 命令投递失败：接收端已关闭（UI 线程未起来 / 已退出 / headless 的哑通道）。
///
/// 刻意**不携带**那条没送出去的命令。`mpsc::SendError` 会把它原样带回来，那是 280 字节，
/// 而全部 50 余处调用点都写作 `let _ = ...`——UI 线程都不在了，拿回命令没有任何用处。
/// 带上它只会触发 clippy 的 `result_large_err`（CI 是 `-D warnings`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiSendError;

impl std::fmt::Display for UiSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UI 命令通道已关闭")
    }
}

impl std::error::Error for UiSendError {}

/// UI 命令发送端：投递 + 唤醒，二者绑为一次操作。
///
/// Clone 廉价（内部两个 `Arc` 级句柄），可自由分发到各线程。
#[derive(Clone)]
pub struct UiSender {
    tx: mpsc::Sender<UiCommand>,
    /// 投递后唤醒 UI 线程。`None` = 对端无需唤醒，见 [`UiSender::without_wake`]。
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl UiSender {
    /// 带唤醒的发送端。`wake` 通常来自 `wind_ui::UiManager::waker`。
    pub fn new(tx: mpsc::Sender<UiCommand>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            tx,
            wake: Some(wake),
        }
    }

    /// 不带唤醒的发送端。**仅用于对端本就不需要被唤醒的场合**：
    ///
    /// - macOS 的 host-render forwarder 线程阻塞在 `recv()` 上，命令到达本身即唤醒；
    /// - 测试与 headless 构造出的哑通道，接收端当场丢弃，没有线程可唤醒。
    ///
    /// 桌面路径（Windows/Linux 的 `UiManager`）**不可**用它——那会让 UI 线程睡到下一个
    /// 计时器到期才看见命令。
    pub fn without_wake(tx: mpsc::Sender<UiCommand>) -> Self {
        Self { tx, wake: None }
    }

    /// 投递一条命令，随后唤醒 UI 线程。
    ///
    /// ⚠ **顺序不可颠倒**：先投递、后唤醒。反过来会让 UI 线程醒来时通道还是空的，转头
    /// 睡回去，那条命令要等下一次唤醒才被看见。
    ///
    /// 投递失败（接收端已关闭）时不唤醒：没有东西可处理，叫醒只是白费一次上下文切换。
    pub fn send(&self, cmd: UiCommand) -> Result<(), UiSendError> {
        self.tx.send(cmd).map_err(|_| UiSendError)?;
        if let Some(wake) = &self.wake {
            wake();
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "ui_sender_tests.rs"]
mod tests;
