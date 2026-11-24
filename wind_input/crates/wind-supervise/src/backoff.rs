// SPDX-License-Identifier: MPL-2.0
//! 崩溃后的退避重启与熔断。核心监护宿主、宿主监护扩展用的是同一套规则（设计 §6）：
//! 间隔 1s、2s、4s… 递增；一个窗口（60 秒）内退出满 3 次即熔断，不再重启。
//! 进程稳定运行超过一个窗口后，计数清零。
//!
//! 纯逻辑：时间由调用方传入，便于测试。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 等这么久后重启。
    RestartAfter(Duration),
    /// 熔断：不再重启。
    Tripped,
}

#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    window: Duration,
    max_failures: usize,
    failures: VecDeque<Instant>,
    started: Option<Instant>,
    tripped: bool,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff::new(Duration::from_secs(1), Duration::from_secs(60), 3)
    }
}

impl Backoff {
    pub fn new(base: Duration, window: Duration, max_failures: usize) -> Backoff {
        Backoff {
            base,
            window,
            max_failures,
            failures: VecDeque::new(),
            started: None,
            tripped: false,
        }
    }

    /// 进程（重新）启动时调用。
    pub fn on_start(&mut self, now: Instant) {
        self.started = Some(now);
    }

    /// 进程意外退出时调用，返回下一步。
    pub fn on_exit(&mut self, now: Instant) -> Decision {
        if self.tripped {
            return Decision::Tripped;
        }
        // 上一次启动后稳定跑满一个窗口：此前的失败不再算数。
        if self
            .started
            .is_some_and(|s| now.saturating_duration_since(s) >= self.window)
        {
            self.failures.clear();
        }
        while self
            .failures
            .front()
            .is_some_and(|f| now.saturating_duration_since(*f) >= self.window)
        {
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        if self.failures.len() >= self.max_failures {
            self.tripped = true;
            return Decision::Tripped;
        }
        let exp = (self.failures.len() - 1).min(16) as u32;
        Decision::RestartAfter(self.base * 2u32.pow(exp))
    }

    pub fn is_tripped(&self) -> bool {
        self.tripped
    }

    /// 熔断后用户手动重新启用时调用。
    pub fn reset(&mut self) {
        self.failures.clear();
        self.tripped = false;
    }

    /// 累计重启次数（当前窗口内）。
    pub fn recent_failures(&self) -> usize {
        self.failures.len()
    }
}

#[cfg(test)]
#[path = "backoff_tests.rs"]
mod tests;
