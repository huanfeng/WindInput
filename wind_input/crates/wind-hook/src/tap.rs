//! 核心热路径上的事件出口。
//!
//! 协调器在按键、上屏、模式切换处调 [`Tap::wants`] 问一句「有没有人要这类事件」——
//! 没有扩展启用时它只是一次原子读，事件对象根本不构造。要的话构造 [`Msg`] 交给
//! [`Tap::emit`]：`try_send` 进有界通道，满了丢新事件并计数，绝不阻塞按键处理。
//! 序列化与写管道都在通道另一头的线程里做。

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::msg::Msg;
use crate::tier::{Grants, Tier};

pub struct Tap {
    /// 所有已启用扩展的授权并集；0 表示没人订阅。
    grants: AtomicU8,
    tx: Mutex<Option<SyncSender<Msg>>>,
    dropped: AtomicU64,
}

impl Default for Tap {
    fn default() -> Self {
        Tap::new()
    }
}

impl Tap {
    pub const fn new() -> Tap {
        Tap {
            grants: AtomicU8::new(0),
            tx: Mutex::new(None),
            dropped: AtomicU64::new(0),
        }
    }

    pub fn wants(&self, t: Tier) -> bool {
        self.grants().has(t)
    }

    /// 是否要按键事件（任一按键档）。
    pub fn wants_key(&self) -> bool {
        self.grants().has_key()
    }

    pub fn grants(&self) -> Grants {
        Grants::from_bits(self.grants.load(Ordering::Relaxed))
    }

    /// 接上消费端：返回接收端，之后按 `grants` 产出事件。重复调用会顶掉旧的接收端。
    pub fn connect(&self, grants: Grants, cap: usize) -> Receiver<Msg> {
        let (tx, rx) = sync_channel(cap);
        *self.tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        self.grants.store(grants.bits(), Ordering::Relaxed);
        rx
    }

    pub fn set_grants(&self, grants: Grants) {
        self.grants.store(grants.bits(), Ordering::Relaxed);
    }

    pub fn disconnect(&self) {
        self.grants.store(0, Ordering::Relaxed);
        *self.tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn emit(&self, msg: Msg) {
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else {
            return;
        };
        match tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                drop(guard);
                self.disconnect();
            }
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// 事件时间戳：进程内单调时钟的毫秒数。扩展只拿它比先后、算间隔，不当墙钟用。
pub fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::ModeEvent;

    fn mode() -> Msg {
        Msg::Mode(ModeEvent {
            ts: 0,
            chinese: true,
            full_width: false,
            schema: String::new(),
        })
    }

    #[test]
    fn idle_tap_wants_nothing_and_swallows_emits() {
        let tap = Tap::new();
        assert!(!tap.wants(Tier::Mode));
        tap.emit(mode());
        assert_eq!(tap.dropped(), 0);
    }

    #[test]
    fn connected_tap_delivers_and_counts_overflow() {
        let tap = Tap::new();
        let rx = tap.connect(Grants::NONE.with(Tier::Mode), 1);
        assert!(tap.wants(Tier::Mode) && !tap.wants_key());
        tap.emit(mode());
        tap.emit(mode());
        assert_eq!(rx.try_recv().unwrap(), mode());
        assert_eq!(tap.dropped(), 1);
    }

    #[test]
    fn dropped_receiver_disconnects() {
        let tap = Tap::new();
        drop(tap.connect(Grants::NONE.with(Tier::KeyClass), 1));
        tap.emit(mode());
        assert!(!tap.wants_key());
    }
}
