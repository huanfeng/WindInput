// SPDX-License-Identifier: MPL-2.0
//! 通用监护器：期望态 + 子进程 + 退避。调用方按节拍（200ms 级）调 [`Supervisor::tick`]，
//! 据返回的事件接管/清理自己的管道、队列与线程。

use std::io;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::backoff::{Backoff, Decision};
use crate::{Job, open_log};

/// 一次 [`Supervisor::tick`] 发生的事。
#[derive(Debug)]
pub enum Event {
    /// 子进程已拉起（已放入 Job），stdin/stdout 交给调用方。
    Started {
        stdin: ChildStdin,
        stdout: ChildStdout,
        /// 不致命的问题（Job 加入失败、日志打不开），由调用方记日志。
        warnings: Vec<String>,
    },
    /// 子进程退出了。`next` 是退避的结论：多久后重启，或熔断。
    Exited { status: ExitStatus, next: Decision },
    /// 拉起失败（含工厂报错），同样按一次崩溃计。
    StartFailed { error: String, next: Decision },
    /// 查询子进程状态失败（极少见）；子进程仍视为在运行。
    PollFailed(io::Error),
}

type Factory = Box<dyn Fn() -> Result<Command, String> + Send + Sync>;

pub struct Supervisor {
    make: Factory,
    log: Option<(PathBuf, u64)>,
    job: Option<Arc<Job>>,
    wanted: bool,
    child: Option<Child>,
    backoff: Backoff,
    next_start: Option<Instant>,
}

impl Supervisor {
    /// `make` 每次拉起时调一次，只需配好程序、参数、环境；stdio、`CREATE_NO_WINDOW`、
    /// Job 由监护器处理。初始期望态为「不运行」。
    pub fn new(make: impl Fn() -> Result<Command, String> + Send + Sync + 'static) -> Supervisor {
        Supervisor {
            make: Box::new(make),
            log: None,
            job: None,
            wanted: false,
            child: None,
            backoff: Backoff::default(),
            next_start: None,
        }
    }

    /// 子进程 stderr 追加到 `path`，超过 `max` 字节轮换。不设则丢弃。
    pub fn with_log(mut self, path: Option<PathBuf>, max: u64) -> Supervisor {
        self.log = path.map(|p| (p, max));
        self
    }

    /// 拉起后立即把子进程放进 Job（先于调用方向它下发任何东西）。
    pub fn with_job(mut self, job: Option<Arc<Job>>) -> Supervisor {
        self.job = job;
        self
    }

    pub fn with_backoff(mut self, backoff: Backoff) -> Supervisor {
        self.backoff = backoff;
        self
    }

    pub fn wanted(&self) -> bool {
        self.wanted
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    pub fn is_tripped(&self) -> bool {
        self.backoff.is_tripped()
    }

    /// 下次计划启动的时刻（无计划为 `None`）。
    pub fn next_start(&self) -> Option<Instant> {
        self.next_start
    }

    /// 设期望态。置真时若没在运行也没有计划，安排立即启动（熔断中不动，要 [`Self::retry`]）；
    /// 置假时取消计划并清零退避，**不碰**正在运行的子进程——调用方先通知它收尾，再 [`Self::stop`]
    /// 或 [`Self::stop_background`]。
    pub fn set_wanted(&mut self, wanted: bool, now: Instant) {
        self.wanted = wanted;
        if wanted {
            if self.child.is_none() && self.next_start.is_none() && !self.backoff.is_tripped() {
                self.next_start = Some(now);
            }
        } else {
            self.next_start = None;
            self.backoff.reset();
        }
    }

    /// 清零退避（解除熔断）。
    pub fn reset_backoff(&mut self) {
        self.backoff.reset();
    }

    /// 手动干预：清零退避，期望运行且没在跑则立即重新启动。
    pub fn retry(&mut self, now: Instant) {
        self.backoff.reset();
        if self.wanted && self.child.is_none() {
            self.next_start = Some(now);
        }
    }

    /// 强杀（调用方判定卡死时用）。不等待，下一次 [`Self::tick`] 照常收尸并按崩溃计。
    pub fn kill(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
        }
    }

    /// 取走子进程并等它退出，`grace` 内不退出就强杀。期望态同时置假。会阻塞。
    pub fn stop(&mut self, grace: Duration) {
        self.set_wanted(false, Instant::now());
        if let Some(c) = self.child.take() {
            stop_child(c, grace);
        }
    }

    /// 同 [`Self::stop`]，但在后台线程等；返回句柄供关机时等待。
    pub fn stop_background(&mut self, grace: Duration) -> Option<JoinHandle<()>> {
        self.set_wanted(false, Instant::now());
        self.child.take().map(|c| stop_child_background(c, grace))
    }

    /// 一次监护：收尸；没在运行且期望运行、到点则拉起。
    pub fn tick(&mut self, now: Instant) -> Option<Event> {
        if let Some(c) = &mut self.child {
            return match c.try_wait() {
                Ok(Some(status)) => {
                    self.child = None;
                    let next = self.on_exit(now);
                    Some(Event::Exited { status, next })
                }
                Ok(None) => None,
                Err(e) => Some(Event::PollFailed(e)),
            };
        }
        if !self.wanted || !self.next_start.is_some_and(|t| t <= now) {
            return None;
        }
        self.next_start = None;
        self.backoff.on_start(now);
        match self.spawn() {
            Ok((stdin, stdout, warnings)) => Some(Event::Started {
                stdin,
                stdout,
                warnings,
            }),
            Err(error) => {
                let next = self.on_exit(now);
                Some(Event::StartFailed { error, next })
            }
        }
    }

    fn on_exit(&mut self, now: Instant) -> Decision {
        let d = self.backoff.on_exit(now);
        if let Decision::RestartAfter(after) = d {
            self.next_start = Some(now + after);
        }
        d
    }

    fn spawn(&mut self) -> Result<(ChildStdin, ChildStdout, Vec<String>), String> {
        let mut cmd = (self.make)()?;
        let mut warnings = Vec::new();
        let stderr = match &self.log {
            Some((path, max)) => match open_log(path, *max) {
                Ok(f) => Stdio::from(f),
                Err(e) => {
                    warnings.push(format!("打开日志 {} 失败: {e}", path.display()));
                    Stdio::null()
                }
            },
            None => Stdio::null(),
        };
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 {:?} 失败: {e}", cmd.get_program()))?;
        if let Some(job) = &self.job
            && let Err(e) = job.assign(&child)
        {
            warnings.push(format!("加入 Job 失败: {e}"));
        }
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("拿不到子进程 stdio".into());
        };
        self.child = Some(child);
        Ok((stdin, stdout, warnings))
    }
}

/// 等子进程自行退出，`grace` 内没退出就强杀并收尸。
pub fn stop_child(mut child: Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// [`stop_child`] 放后台线程做。
pub fn stop_child_background(child: Child, grace: Duration) -> JoinHandle<()> {
    std::thread::spawn(move || stop_child(child, grace))
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
