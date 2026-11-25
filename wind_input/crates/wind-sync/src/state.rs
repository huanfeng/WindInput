//! 本机同步状态（可序列化；持久化由 S4 负责）与重试退避。

use crate::error::SyncError;
use crate::manifest::{Entry, Manifest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const BACKOFF_BASE_SECS: u64 = 30;
pub const BACKOFF_MAX_SECS: u64 = 30 * 60;

/// 失败 n 次（n ≥ 1）后的等待秒数：30s 起倍增，上限 30min。
pub fn backoff_secs(failures: u32) -> u64 {
    let shift = failures.saturating_sub(1).min(16);
    (BACKOFF_BASE_SECS << shift).min(BACKOFF_MAX_SECS)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryState {
    pub failures: u32,
    /// 下次允许自动重试的 unix 秒。
    pub next_at: u64,
    /// 不可自动恢复的错误（认证/口令/格式）：停在这里等用户处理。
    pub halted: Option<String>,
    pub last_error: Option<String>,
}

impl RetryState {
    pub fn record_failure(&mut self, err: &SyncError, now: u64) {
        self.failures = self.failures.saturating_add(1);
        self.last_error = Some(err.to_string());
        if err.retryable() {
            self.next_at = now + backoff_secs(self.failures);
        } else {
            self.halted = Some(err.to_string());
        }
    }
    pub fn record_success(&mut self) {
        *self = RetryState::default();
    }
    /// 用户处理完（改了凭据/口令）后解除停止态。
    pub fn clear_halt(&mut self) {
        self.halted = None;
        self.next_at = 0;
    }
    pub fn due(&self, now: u64) -> bool {
        self.halted.is_none() && now >= self.next_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// 同步专用的随机设备号（进远端键，字母数字）。
    pub device: String,
    #[serde(default)]
    pub lamport: u64,
    /// 本设备最近一次写出的清单代数。
    #[serde(default)]
    pub gen_: u64,
    /// 本设备最近一份已提交清单的 nonce。
    #[serde(default)]
    pub last_nonce: String,
    /// 路径 → 上次同步后的条目（含删除标记）。
    #[serde(default)]
    pub files: BTreeMap<String, Entry>,
    /// 其它设备清单的缓存（按 gen 判断是否需重新下载）。
    #[serde(default)]
    pub remote_cache: BTreeMap<String, Manifest>,
    /// 最近一次已发布清单内容的摘要，用来跳过无变化的清单写入。
    #[serde(default)]
    pub published_digest: String,
    /// 尚未确认传完的路径（待传队列；失败后供状态展示，重跑时由扫描重新得出）。
    #[serde(default)]
    pub pending: Vec<String>,
    #[serde(default)]
    pub retry: RetryState,
    /// 最近一次成功同步的 unix 秒（由同步进程维护，供状态展示）。
    #[serde(default)]
    pub last_ok: Option<u64>,
}

impl SyncState {
    pub fn new(device: impl Into<String>) -> Self {
        SyncState {
            device: device.into(),
            lamport: 0,
            gen_: 0,
            last_nonce: String::new(),
            files: BTreeMap::new(),
            remote_cache: BTreeMap::new(),
            published_digest: String::new(),
            pending: Vec::new(),
            retry: RetryState::default(),
            last_ok: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendError;

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_secs(1), 30);
        assert_eq!(backoff_secs(2), 60);
        assert_eq!(backoff_secs(3), 120);
        assert_eq!(backoff_secs(7), 1800);
        assert_eq!(backoff_secs(500), 1800);
    }

    #[test]
    fn halting_errors_do_not_retry() {
        let mut r = RetryState::default();
        r.record_failure(
            &SyncError::Backend(BackendError::Transient("x".into())),
            100,
        );
        assert_eq!(r.next_at, 130);
        assert!(r.halted.is_none() && !r.due(129) && r.due(130));
        r.record_failure(&SyncError::Backend(BackendError::Auth("x".into())), 200);
        assert!(r.halted.is_some() && !r.due(u64::MAX));
        r.clear_halt();
        assert!(r.due(0));
        r.record_success();
        assert_eq!(r, RetryState::default());
    }

    #[test]
    fn state_serde_roundtrip() {
        let mut s = SyncState::new("abc");
        s.lamport = 3;
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<SyncState>(&j).unwrap(), s);
    }
}
