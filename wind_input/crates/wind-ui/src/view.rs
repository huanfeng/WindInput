//! View 盒模型已迁入 wind-scene（`wind_scene::view`），这里原路径再导出，调用方不改。
//!
//! 留在本 crate 的只有等命令时顺带回收绘制缓存的 mpsc 辅助：它属于窗口侧的线程模型，
//! 不是渲染。

pub use wind_scene::view::*;

/// 等下一条命令，期间到点就回收绘制缓存；返回 None = 发送端已全部断开。
///
/// 存在的理由是 **macOS**：那边没有 `manager.rs` 的消息循环（`ui_thread` 是
/// `cfg(not(macos))`），只有 `manager_macos::forwarder_thread` 一个纯阻塞 `recv`，
/// 于是闲置回收永远轮不到执行——而候选窗在 macOS 同样走本地绘制（`candidate_window`
/// 的 `render_frame` → `root.paint`），缓存照样在涨。
///
/// 逻辑放在这里而不是 `manager_macos`：那个模块 `cfg(target_os = "macos")`，在开发机上
/// 连编译都不参与，写进去的代码没有任何本机可验证性。做成与命令类型无关的泛型后，它在
/// 所有平台参与编译、在开发机上可测，macOS 那侧只剩一行调用。
pub fn recv_evicting_idle<T>(rx: &std::sync::mpsc::Receiver<T>) -> Option<T> {
    match recv_evicting_idle_until(rx, None) {
        Recv::Cmd(cmd) => Some(cmd),
        // 没给截止时刻就不会超时；按断开处理只是让 match 穷尽。
        Recv::Timeout | Recv::Disconnected => None,
    }
}

/// [`recv_evicting_idle_until`] 的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum Recv<T> {
    Cmd(T),
    /// 到了调用方给的截止时刻，期间没有命令。
    Timeout,
    /// 发送端已全部断开。
    Disconnected,
}

/// 同 [`recv_evicting_idle`]，但最多等到 `until`：调用方自己也有计时器要推进时用
/// （Linux forwarder 的工具栏迟滞 / 自动隐藏、软键盘连发）。`None` = 只等命令。
///
/// 闲置回收照旧挂在等待上：到期时刻取「调用方的 `until`」与「缓存回收时刻」中较早者，
/// 缓存先到就收一次再接着等，不会因此提前返回给调用方。
pub fn recv_evicting_idle_until<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    until: Option<std::time::Instant>,
) -> Recv<T> {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        let now = std::time::Instant::now();
        paint_cache_evict_if_idle(now);
        if until.is_some_and(|u| now >= u) {
            return Recv::Timeout;
        }
        // 回收之后才问到期时刻：刚清空的缓存不再要求唤醒，于是退化为纯阻塞 recv，
        // 空闲时线程零开销地停住。
        let Some(deadline) = [paint_cache_next_deadline(), until]
            .into_iter()
            .flatten()
            .min()
        else {
            return match rx.recv() {
                Ok(cmd) => Recv::Cmd(cmd),
                Err(_) => Recv::Disconnected,
            };
        };
        match rx.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(cmd) => return Recv::Cmd(cmd),
            // 到点了：回到顶部收一次（或发现是调用方的截止时刻而返回）。不会忙等——
            // 缓存那一轮的回收必定成功，于是不再要求唤醒。
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Recv::Disconnected,
        }
    }
}

#[cfg(test)]
#[path = "view_idle_evict_tests.rs"]
mod idle_evict_tests;
