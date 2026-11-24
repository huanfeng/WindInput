//! UI 线程的唤醒原语：让消息循环「睡到有事发生」，而不是定时醒来查一遍。
//!
//! ## 为什么需要它
//!
//! 消息循环要同时等三类事件，而它们的等待原语互不相通：
//!
//! | 来源 | 等待原语 |
//! |------|---------|
//! | Win32 消息（鼠标 / 热键 / 重绘） | 消息队列，`GetMessage` 系 |
//! | `UiCommand` 到达 | `mpsc::Receiver`，`recv` 系 |
//! | 计时器到期（气泡 / 工具栏 / 防抖） | 超时 |
//!
//! `mpsc` 不暴露底层同步对象，没法塞进 `MsgWaitForMultipleObjects` 的句柄数组。故本模块
//! 反过来做：额外造一个 Win32 事件，**由发送方在 `send` 之后置位**，UI 线程等的是
//! 「消息队列 ∪ 该事件 ∪ 超时」。事件只承担唤醒，命令本身照旧走 `mpsc`。
//!
//! ⚠ **投递与唤醒的顺序不可颠倒**：先 `mpsc::send` 再 [`UiWaker::wake`]。反过来会让 UI
//! 线程醒来时队列还是空的，然后睡回去——那条命令要等到下一次唤醒才被看见。顺序由
//! `UiSender::send` 固化（见 wind-coordinator），调用方无从写反。
//!
//! ## 自动重置事件，不丢唤醒
//!
//! 事件是 auto-reset 的：等到即自动复位，无需手动 `ResetEvent`。处理命令期间到达的新命令
//! 会把它重新置位，下一轮 `wait` 立刻返回——**唤醒不会因为「UI 线程正忙」而丢失**。这正是
//! auto-reset 事件相对于「条件变量 + 已错过的 notify」的好处，也是非 Windows 分支必须自带
//! 一个 `bool` 的原因。
//!
//! ## 降级（仅 Windows）
//!
//! 建不出事件时不让 UI 失效：[`UiWaitPort::wait`] 退回固定 8ms 轮询（即本次改动之前的
//! 行为）。功能完全正常，只是 CPU 占用回到老样子。这条路径实测走不到，但它是「UI 线程一旦
//! 出问题就是全部 GUI 消失」这个后果决定的——见 `manager.rs` 中 `ui_thread` 的函数文档。
//!
//! 非 Windows 侧没有对应概念：条件变量不会「创建失败」，无从降级。

#[cfg(windows)]
mod imp {
    use std::sync::Arc;
    use std::time::Duration;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent};
    use windows::Win32::UI::WindowsAndMessaging::{
        MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, QS_ALLINPUT,
    };

    /// 降级轮询周期：与本次改动前 `manager.rs` 循环末尾的固定休眠一致。见模块文档「降级」。
    const FALLBACK_POLL: Duration = Duration::from_millis(8);

    /// 持有所有权的事件句柄，Drop 时关闭。
    struct OwnedEvent(HANDLE);

    // 事件句柄跨线程置位 / 等待是 Win32 明确支持的用法（内核对象是进程级的，不带线程
    // 亲和性）。这正是本类型存在的意义：发送方在任意线程置位，UI 线程等待。
    unsafe impl Send for OwnedEvent {}
    unsafe impl Sync for OwnedEvent {}

    impl Drop for OwnedEvent {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// 发送侧句柄：置位事件，唤醒 UI 线程。Clone 廉价（`Arc`）。
    #[derive(Clone)]
    pub struct UiWaker(Option<Arc<OwnedEvent>>);

    /// UI 线程侧句柄：等待「消息 ∪ 命令 ∪ 超时」。
    pub struct UiWaitPort(Option<Arc<OwnedEvent>>);

    /// 造一对唤醒端 / 等待端。事件建不出时两端都降级（见模块文档）。
    pub fn channel() -> (UiWaker, UiWaitPort) {
        // 无安全属性、bManualReset=FALSE（自动重置）、bInitialState=FALSE、匿名。
        match unsafe { CreateEventW(None, false, false, None) } {
            Ok(h) => {
                let shared = Arc::new(OwnedEvent(h));
                (UiWaker(Some(shared.clone())), UiWaitPort(Some(shared)))
            }
            Err(e) => {
                tracing::error!("UI 唤醒事件创建失败，退回 {FALLBACK_POLL:?} 轮询: {e}");
                (UiWaker(None), UiWaitPort(None))
            }
        }
    }

    impl UiWaker {
        /// 置位事件。**必须在命令投递进 `mpsc` 之后调用**（见模块文档）。
        pub fn wake(&self) {
            if let Some(ev) = &self.0 {
                unsafe {
                    let _ = SetEvent(ev.0);
                }
            }
        }
    }

    impl UiWaitPort {
        /// 阻塞至下列任一发生：唤醒事件置位、消息队列来了新输入、`timeout` 到期。
        ///
        /// `timeout` 为 `None` = 无限等待，只有在确认没有任何待到期计时器时才可传。
        pub fn wait(&self, timeout: Option<Duration>) {
            let Some(ev) = &self.0 else {
                // 降级路径：退回固定周期轮询，且不超过调用方要求的超时。
                std::thread::sleep(timeout.unwrap_or(FALLBACK_POLL).min(FALLBACK_POLL));
                return;
            };
            let ms = match timeout {
                // 亚毫秒余量会被 as_millis 截成 0，那等于忙等；进位到 1ms。
                Some(d) => u32::try_from(d.as_millis()).unwrap_or(u32::MAX).max(1),
                None => INFINITE,
            };
            unsafe {
                // MWMO_INPUTAVAILABLE 不可省：本轮 `PeekMessage` 之后、进入等待之前到达的
                // 消息，若无此标志会被判为「早就在队列里、不算新到达」而等不醒，要拖到超时
                // 或下一条消息才被处理。加上它，「队列非空」本身即满足唤醒条件。
                MsgWaitForMultipleObjectsEx(Some(&[ev.0]), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    /// 非 Windows 没有消息队列要一起等，条件变量足够。
    ///
    /// 那个 `bool` 是**必需**的，不能只靠 `Condvar`：投递发生在 `wait` 之前时纯 `notify`
    /// 会丢失，UI 线程得睡到超时才看见那条命令。置位后由 `wait` 消费，语义与 Windows 侧的
    /// auto-reset 事件一致。
    struct Shared {
        signaled: Mutex<bool>,
        cv: Condvar,
    }

    /// 发送侧句柄：置位并通知。Clone 廉价（`Arc`）。
    #[derive(Clone)]
    pub struct UiWaker(Arc<Shared>);

    /// UI 线程侧句柄：等待「命令 ∪ 超时」。
    pub struct UiWaitPort(Arc<Shared>);

    /// 造一对唤醒端 / 等待端。
    pub fn channel() -> (UiWaker, UiWaitPort) {
        let shared = Arc::new(Shared {
            signaled: Mutex::new(false),
            cv: Condvar::new(),
        });
        (UiWaker(shared.clone()), UiWaitPort(shared))
    }

    impl UiWaker {
        /// 置位并通知。**必须在命令投递进 `mpsc` 之后调用**（见模块文档）。
        pub fn wake(&self) {
            let mut g = self.0.signaled.lock().unwrap_or_else(|e| e.into_inner());
            *g = true;
            self.0.cv.notify_one();
        }
    }

    impl UiWaitPort {
        /// 阻塞至唤醒置位或 `timeout` 到期。`None` = 无限等待。
        pub fn wait(&self, timeout: Option<Duration>) {
            let mut g = self.0.signaled.lock().unwrap_or_else(|e| e.into_inner());
            if !*g {
                // 无限等待用 `wait`（`wait_timeout` 不接受「无超时」），有超时走 wait_timeout。
                g = match timeout {
                    Some(d) => {
                        let (next, _) = self
                            .0
                            .cv
                            .wait_timeout(g, d)
                            .unwrap_or_else(|e| e.into_inner());
                        next
                    }
                    None => self.0.cv.wait(g).unwrap_or_else(|e| e.into_inner()),
                };
            }
            *g = false; // 消费信号，对齐 auto-reset 语义
        }
    }
}

pub use imp::{UiWaitPort, UiWaker, channel};

#[cfg(test)]
#[path = "wake_tests.rs"]
mod tests;
