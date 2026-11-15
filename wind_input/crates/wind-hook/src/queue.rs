//! 满了丢最旧的有界队列。
//!
//! 与 `wind-rpc` 事件通道的「满即断开」相反：那边事件极少，满了说明对端卡死；这边按键高频，
//! 一次短暂卡顿不该把对端踢掉。丢掉的条数随下一条取出的元素一并交给消费者，写进帧的
//! `dropped` 字段告诉对端。
//!
//! 控制消息（握手、配置、关停）用 [`DropOldest::push_keep`] 入队：它们不计容量、也不会被
//! 淘汰——丢一条 `Config` 宿主就一直用旧授权，丢一条 `Welcome` 扩展就永远收不到事件。

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

pub struct DropOldest<T> {
    cap: usize,
    state: Mutex<State<T>>,
    cv: Condvar,
}

struct State<T> {
    /// (元素, 是否不可淘汰)。
    q: VecDeque<(T, bool)>,
    /// 上次取出以来丢掉的条数。
    pending_dropped: u64,
    /// 累计丢掉的条数（状态上报用）。
    total_dropped: u64,
    closed: bool,
}

#[derive(Debug, PartialEq)]
pub enum Pop<T> {
    /// 取到一条，附带此前丢掉的条数。
    Item(T, u64),
    Timeout,
    Closed,
}

impl<T> DropOldest<T> {
    pub fn new(cap: usize) -> DropOldest<T> {
        DropOldest {
            cap: cap.max(1),
            state: Mutex::new(State {
                q: VecDeque::new(),
                pending_dropped: 0,
                total_dropped: 0,
                closed: false,
            }),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 入队；满了先丢最旧的一条可淘汰元素。已关闭返回 `false`。
    pub fn push(&self, item: T) -> bool {
        self.enqueue(item, false)
    }

    /// 入队一条不可淘汰的元素（控制消息）：不受容量限制，也不会被后来者挤掉。
    pub fn push_keep(&self, item: T) -> bool {
        self.enqueue(item, true)
    }

    fn enqueue(&self, item: T, keep: bool) -> bool {
        let mut s = self.lock();
        if s.closed {
            return false;
        }
        if !keep
            && s.q.len() >= self.cap
            && let Some(i) = s.q.iter().position(|(_, k)| !k)
        {
            s.q.remove(i);
            s.pending_dropped += 1;
            s.total_dropped += 1;
        }
        s.q.push_back((item, keep));
        drop(s);
        self.cv.notify_one();
        true
    }

    /// 阻塞取一条，最多等 `timeout`。关闭后先把剩下的取完，再返回 `Closed`。
    pub fn pop(&self, timeout: Duration) -> Pop<T> {
        let mut s = self.lock();
        loop {
            if let Some((item, _)) = s.q.pop_front() {
                let dropped = std::mem::take(&mut s.pending_dropped);
                return Pop::Item(item, dropped);
            }
            if s.closed {
                return Pop::Closed;
            }
            let (g, r) = self
                .cv
                .wait_timeout(s, timeout)
                .unwrap_or_else(|e| e.into_inner());
            s = g;
            if r.timed_out() && s.q.is_empty() && !s.closed {
                return Pop::Timeout;
            }
        }
    }

    pub fn close(&self) {
        self.lock().closed = true;
        self.cv.notify_all();
    }

    pub fn len(&self) -> usize {
        self.lock().q.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn total_dropped(&self) -> u64 {
        self.lock().total_dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_millis(10);

    #[test]
    fn full_queue_drops_oldest_and_reports_count() {
        let q = DropOldest::new(2);
        for i in 0..5 {
            q.push(i);
        }
        assert_eq!(q.pop(T), Pop::Item(3, 3));
        assert_eq!(q.pop(T), Pop::Item(4, 0));
        assert_eq!(q.total_dropped(), 3);
    }

    #[test]
    fn kept_items_are_never_evicted() {
        let q = DropOldest::new(2);
        q.push_keep(100);
        for i in 0..5 {
            q.push(i);
        }
        assert_eq!(q.pop(T), Pop::Item(100, 4), "控制消息留在队首");
        assert_eq!(q.pop(T), Pop::Item(4, 0));
        assert_eq!(q.pop(T), Pop::Timeout);
    }

    #[test]
    fn empty_pop_times_out() {
        let q: DropOldest<u8> = DropOldest::new(1);
        assert_eq!(q.pop(T), Pop::Timeout);
    }

    #[test]
    fn close_drains_then_reports_closed() {
        let q = DropOldest::new(4);
        q.push(1);
        q.close();
        assert!(!q.push(2));
        assert_eq!(q.pop(T), Pop::Item(1, 0));
        assert_eq!(q.pop(T), Pop::Closed);
    }

    #[test]
    fn pop_wakes_on_push_from_other_thread() {
        let q = std::sync::Arc::new(DropOldest::new(1));
        let q2 = q.clone();
        let h = std::thread::spawn(move || q2.pop(Duration::from_secs(5)));
        std::thread::sleep(T);
        q.push(7);
        assert_eq!(h.join().unwrap(), Pop::Item(7, 0));
    }
}
