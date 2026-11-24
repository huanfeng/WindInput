//! 与 core 的连接：发状态 / 命令回执，向 core 发受限请求并等回执。

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::Value;
use wind_sync_ipc::{FromSync, write_frame};

/// 向 core 发请求的超时。重载词库可能较慢，但不应无限等。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub trait CoreLink: Send + Sync {
    /// 发白名单请求并等 core 的回执。
    fn request(&self, method: &str, params: Value) -> Result<Value, String>;
    /// 发状态 / 命令回执。
    fn send(&self, msg: FromSync);
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 真实连接：stdout 写帧；读线程把 `Reply` 交给 [`PipeLink::deliver`]。
pub struct PipeLink {
    out: Mutex<Box<dyn Write + Send>>,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl PipeLink {
    pub fn new(out: Box<dyn Write + Send>, timeout: Duration) -> Arc<PipeLink> {
        Arc::new(PipeLink {
            out: Mutex::new(out),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            timeout,
        })
    }

    /// core 的回执。没人等的（已超时）丢弃。
    pub fn deliver(&self, id: u64, r: Result<Value, String>) {
        if let Some(tx) = lock(&self.pending).remove(&id) {
            let _ = tx.send(r);
        }
    }

    /// 连接断了：所有等待者立刻失败，不必等超时。
    pub fn fail_all(&self, why: &str) {
        for (_, tx) in lock(&self.pending).drain() {
            let _ = tx.send(Err(why.to_string()));
        }
    }
}

impl CoreLink for PipeLink {
    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        // 先登记再发送：回执可能比 write 返回得还早。
        lock(&self.pending).insert(id, tx);
        let sent = write_frame(
            &mut *lock(&self.out),
            &FromSync::Request {
                id,
                method: method.to_string(),
                params,
            },
        );
        if let Err(e) = sent {
            lock(&self.pending).remove(&id);
            return Err(format!("与核心的连接已断开: {e}"));
        }
        match rx.recv_timeout(self.timeout) {
            Ok(r) => r,
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err("等待核心回应超时".into())
            }
        }
    }

    fn send(&self, msg: FromSync) {
        let _ = write_frame(&mut *lock(&self.out), &msg);
    }
}
