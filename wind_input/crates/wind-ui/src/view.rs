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
mod idle_evict_tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    /// 把线程局部的那份缓存清干净，让用例从确定状态起步。
    fn reset() {
        paint_cache_evict_if_idle(Instant::now() + crate::image_cache::IDLE_EVICT_AFTER * 2);
    }

    /// 往缓存里留下点驻留并把最后取用时刻挪到过去。解码失败的源同样会占表并打点，
    /// 用它就不必在本模块再搭一份图片编码的脚手架。
    fn stage_stale_entry() {
        with_image_cache_for_test(|cache| {
            assert!(cache.src_size("data:image/png;base64,####").is_none());
            let stale = Instant::now()
                .checked_sub(crate::image_cache::IDLE_EVICT_AFTER + Duration::from_secs(1))
                .expect("单调时钟回拨");
            cache.set_last_use_for_test(stale);
        });
    }

    /// macOS 的 forwarder 线程只有一个阻塞 recv，闲置回收得挂在**等待命令**这件事上。
    /// 等待期间到点就收——收不掉的话，那个平台的内存永远不落。
    #[test]
    fn recv_evicting_idle_reclaims_while_waiting() {
        reset();
        stage_stale_entry();
        assert!(
            paint_cache_next_deadline().is_some(),
            "布置的驻留应当要求唤醒"
        );

        let (tx, rx) = channel::<u32>();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            let _ = tx.send(7);
        });

        assert_eq!(recv_evicting_idle(&rx), Some(7));
        assert_eq!(
            paint_cache_next_deadline(),
            None,
            "等命令的这段时间里就该把缓存收掉"
        );
        sender.join().expect("发送线程");
    }

    /// 缓存空着时退化成普通 recv：既不忙等，也要如实报告发送端断开。
    #[test]
    fn recv_evicting_idle_is_plain_recv_when_cache_empty() {
        reset();
        assert_eq!(paint_cache_next_deadline(), None);

        let (tx, rx) = channel::<u32>();
        tx.send(1).expect("发送");
        assert_eq!(recv_evicting_idle(&rx), Some(1));
        drop(tx);
        assert_eq!(recv_evicting_idle(&rx), None, "发送端断开该返回 None");
    }

    /// 带截止时刻：没命令就按时返回 `Timeout`，不早不晚；闲置回收照旧在等待里发生。
    #[test]
    fn recv_until_times_out_and_still_reclaims() {
        reset();
        stage_stale_entry();
        let (tx, rx) = channel::<u32>();
        let wait = Duration::from_millis(30);
        let start = Instant::now();
        assert_eq!(
            recv_evicting_idle_until(&rx, Some(start + wait)),
            Recv::Timeout
        );
        assert!(start.elapsed() >= wait, "不能提前返回");
        assert_eq!(paint_cache_next_deadline(), None, "等待期间该把缓存收掉");

        tx.send(3).expect("发送");
        assert_eq!(
            recv_evicting_idle_until(&rx, Some(Instant::now() + wait)),
            Recv::Cmd(3)
        );
        drop(tx);
        assert_eq!(
            recv_evicting_idle_until(&rx, Some(Instant::now() + wait)),
            Recv::Disconnected
        );
    }
}
