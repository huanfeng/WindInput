//! 同步进程的主循环：调度、命令处理、状态上报（设计 §3.2、§3.3、§7）。
//!
//! 主循环单线程处理命令与事件；一轮同步放到工作线程里跑，主循环在同步期间仍响应：
//! 只读命令（导出密钥、测试连接）与 `sync_now` 照常处理，会改状态 / 机密的命令立即回
//! 「正在同步」，不排队（排队的命令在 core 超时之后仍会执行，用户以为失败了其实生效）；
//! `Shutdown` 置取消标志，工作线程在写入各步之间收手并释放编辑会话。
//! 输入（core 的帧、文件事件、同步结束）都经一个 channel 进来；
//! core 对本进程请求的回执由读线程直接交给 [`crate::link::PipeLink`]，不经这里——
//! 同步进行中也要能收到回执，否则 `data.beginEdit` 会自己等死自己。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wind_sync::crypto::{Dek, KdfParams, Meta, content_hash};
use wind_sync::devices::{DeviceInfo, list_devices, put_device_info, remove_device};
use wind_sync::scope::{DeviceClass, ScopeFlags, in_scope, to_remote};
use wind_sync::state::backoff_secs;
use wind_sync::{Backend, SyncCtx, SyncError, SyncReport, SyncState, manifest, remote, sync_once};
use wind_sync_ipc::{Backend as Target, FromSync, Op, Scope, Status, SyncSettings, ToSync};

use crate::backends::BackendFactory;
use crate::errors::{backend_error, crypto_error, sync_error};
use crate::link::CoreLink;
use crate::local::{Echo, FsLocal, RETRY_TAG, lock};
use crate::log::{log, now_unix};
use crate::secrets::{SecretStore, Secrets};
use crate::statefile;
use crate::watch::{self, SharedScope};

/// 主循环的输入。
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // 一次只在 channel 里排几个，不值得 Box
pub enum Input {
    /// core 发来的帧（`Reply` 由读线程处理，不会出现在这里）。
    Core(ToSync),
    /// 范围内的文件事件（相对用户目录路径）。
    Fs(String),
    /// stdin 关闭：core 已退出。
    Eof,
    /// 工作线程跑完了一轮同步。
    RoundDone(Box<Round>),
}

/// 一轮同步的结果（工作线程 → 主循环）。
pub struct Round {
    state: SyncState,
    /// 外层 Err = 连后端都没建成（不计入退避状态）。
    result: Result<Result<SyncReport, SyncError>, String>,
    conflicts: Vec<String>,
    now: u64,
    /// 本轮开始前的回声表快照：本轮扫描之后，这些旧条目就可以清掉了。
    echo_before: HashMap<String, String>,
    failures_before: u32,
    /// 本轮成功写出的设备信息。
    info_written: Option<DeviceInfo>,
}

impl std::fmt::Debug for Round {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Round")
    }
}

/// 同步中拒绝会改状态的命令时的提示。
const BUSY_MSG: &str = "正在同步，请稍后再试";
/// 大批量删除护栏停止时 `halted` / `last_error` 里的识别标记。
const MASS_MARK: &str = "为防误删已暂停";
/// 连续这么多次「稍后重试」类的轮次后，改按普通失败退避。
const MAX_FREE_RETRIES: u32 = 5;
/// `Shutdown` 后最多等工作线程收尾这么久。
const SHUTDOWN_WAIT: Duration = Duration::from_millis(2500);

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// 启动后首次同步。
    pub first_delay: Duration,
    /// 文件变化去抖。
    pub debounce: Duration,
    /// 设置 / 机密变更后到下一轮的等待。
    pub settings_delay: Duration,
    /// 本轮因「文件在扫描后被改 / 正被用户编辑」放弃后，到下一轮的等待。
    pub retry_delay: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            first_delay: Duration::from_secs(30),
            debounce: Duration::from_secs(10),
            settings_delay: Duration::from_secs(2),
            retry_delay: Duration::from_secs(10),
        }
    }
}

pub struct Config {
    pub userdata_dir: std::path::PathBuf,
    pub state_dir: std::path::PathBuf,
    pub settings: SyncSettings,
    pub link: Arc<dyn CoreLink>,
    pub secrets: Box<dyn SecretStore>,
    pub factory: Box<dyn BackendFactory>,
    pub timing: Timing,
    pub kdf: KdfParams,
    /// 是否监听文件变化（测试可关；进程内运行的手机端不监听）。
    pub watch: bool,
    /// 本机设备类别：平台相关文件在远端的命名空间。
    pub class: DeviceClass,
    /// 只在被要求时（`sync_now` / 确认大批量删除）才跑一轮，不按间隔与设置变更自行调度。
    /// 进程内运行的手机端由宿主决定何时同步。
    pub on_demand: bool,
}

pub fn flags_of(s: &Scope) -> ScopeFlags {
    ScopeFlags {
        config: s.config,
        schemas: s.schemas,
        themes: s.themes,
        dicts: s.dicts,
    }
}

pub struct Runner {
    cfg: Config,
    settings: SyncSettings,
    secrets: Secrets,
    secret_error: Option<String>,
    state: SyncState,
    echo: Echo,
    scope: SharedScope,
    factory: Arc<dyn BackendFactory>,
    tx: Sender<Input>,
    cancel: Arc<AtomicBool>,
    syncing: bool,
    /// 同步进行中用户改了设置：本轮结束后再解除停止态。
    clear_halt_after_round: bool,
    force_now: bool,
    /// 用户确认了大批量删除：下一轮放行（只此一轮）。
    allow_mass_once: bool,
    free_retries: u32,
    debounce_until: Option<Instant>,
    periodic_at: Instant,
    conflicts: Vec<String>,
    last_sent: Option<Status>,
    /// 最近一次写到远端名册的本机信息；换远端、进程重启后为空（下次必写）。
    info_written: Option<DeviceInfo>,
    _watch: Option<watch::Watch>,
}

fn interval(s: &SyncSettings) -> Duration {
    Duration::from_secs(u64::from(s.interval_min.max(1)) * 60)
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// 存储位置（地址 / 桶 / 账号）是否变了。区域、前缀、寻址方式不算：凭据发往的还是同一处。
fn backend_changed(old: Option<&Target>, new: Option<&Target>) -> bool {
    match (old, new) {
        (None, None) => false,
        (Some(Target::Webdav { url: a, user: ua }), Some(Target::Webdav { url: b, user: ub })) => {
            a.trim() != b.trim() || ua != ub
        }
        (
            Some(Target::S3 {
                endpoint: ea,
                bucket: ba,
                access_key_id: ka,
                ..
            }),
            Some(Target::S3 {
                endpoint: eb,
                bucket: bb,
                access_key_id: kb,
                ..
            }),
        ) => ea.trim() != eb.trim() || ba.trim() != bb.trim() || ka != kb,
        _ => true,
    }
}

/// 把引擎的「检测到大批量删除：本次将删除 N 个…（已同步共 M 个）」改写成面向用户的说明。
fn mass_deletion_message(raw: &str) -> String {
    let nums: Vec<&str> = raw
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .collect();
    match nums.as_slice() {
        [n, m, ..] => {
            format!("本机有 {n}/{m} 个文件看似被删除，{MASS_MARK}。确认无误请点「继续同步」")
        }
        _ => raw.to_string(),
    }
}

const DAY_SECS: u64 = 86400;

fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "linux"
    }
}

/// 本机要写进名册的信息。
fn local_info(settings: &SyncSettings, now: u64) -> DeviceInfo {
    let name = settings.device_name.trim();
    DeviceInfo {
        name: if name.is_empty() {
            crate::hostname::host_name()
        } else {
            name.to_string()
        },
        platform: platform().into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        caps: vec!["files".into()],
        last_seen: now,
    }
}

/// 名册里的本机信息是否需要重写：内容变了；`daily` 时另加「last_seen 与现在不在同一个 UTC 日」。
fn info_stale(written: Option<&DeviceInfo>, want: &DeviceInfo, daily: bool) -> bool {
    let Some(w) = written else { return true };
    let same = DeviceInfo {
        last_seen: w.last_seen,
        ..want.clone()
    } == *w;
    !same || (daily && w.last_seen / DAY_SECS != want.last_seen / DAY_SECS)
}

impl Runner {
    pub fn new(mut cfg: Config, tx: Sender<Input>) -> Runner {
        let (secrets, secret_error) = match Secrets::load(&*cfg.secrets) {
            Ok(s) => (s, None),
            Err(e) => {
                log!("{e}");
                (Secrets::default(), Some(e))
            }
        };
        let state = statefile::load(&cfg.state_dir);
        let settings = cfg.settings.clone();
        let scope = SharedScope::new(flags_of(&settings.scope));
        let watch = cfg.watch.then(|| {
            watch::start(&cfg.userdata_dir, scope.clone(), tx.clone())
                .map_err(|e| log!("{e}；仅靠定时同步"))
                .ok()
        });
        let periodic_at = Instant::now() + cfg.timing.first_delay;
        let factory: Arc<dyn BackendFactory> = Arc::from(std::mem::replace(
            &mut cfg.factory,
            Box::new(crate::backends::NoFactory),
        ));
        Runner {
            cfg,
            settings,
            secrets,
            secret_error,
            state,
            echo: Arc::new(Mutex::new(Default::default())),
            scope,
            factory,
            tx,
            cancel: Arc::new(AtomicBool::new(false)),
            syncing: false,
            clear_halt_after_round: false,
            force_now: false,
            allow_mass_once: false,
            free_retries: 0,
            debounce_until: None,
            periodic_at,
            conflicts: Vec::new(),
            last_sent: None,
            info_written: None,
            _watch: watch.flatten(),
        }
    }

    // ── 状态 ───────────────────────────────────────────────

    fn configured(&self) -> bool {
        self.settings.enabled && self.settings.backend.is_some()
    }

    fn ready(&self) -> bool {
        self.secrets.password.is_some() && self.secrets.dek.is_some()
    }

    /// 能自动调度：已配置、机密齐全、没停在错误态。
    fn runnable(&self) -> bool {
        self.configured() && self.ready() && self.state.retry.halted.is_none()
    }

    pub fn status(&self) -> Status {
        let phase = if !self.configured() {
            "not_configured"
        } else if !self.ready() {
            "needs_setup"
        } else if self.syncing {
            "syncing"
        } else if self.state.retry.halted.is_some() {
            "halted"
        } else if self.state.retry.last_error.is_some() {
            "error"
        } else {
            "idle"
        };
        Status {
            phase: phase.into(),
            last_ok: self.state.last_ok.map(|t| t as i64),
            last_error: self
                .secret_error
                .clone()
                .or_else(|| self.state.retry.last_error.clone()),
            pending: self.state.pending.len() as u32,
            conflicts: self.conflicts.len() as u32,
            conflict_paths: self
                .conflicts
                .iter()
                .take(wind_sync_ipc::MAX_CONFLICT_PATHS)
                .cloned()
                .collect(),
            has_secret: self.ready(),
            joined: self.secrets.dek.is_some(),
        }
    }

    fn report(&mut self) {
        let s = self.status();
        if self.last_sent.as_ref() != Some(&s) {
            self.cfg.link.send(FromSync::Status(s.clone()));
            self.last_sent = Some(s);
        }
    }

    fn save_state(&self) {
        if let Err(e) = statefile::save(&self.cfg.state_dir, &self.state) {
            log!("{e}");
        }
    }

    fn save_secrets(&mut self) -> Result<(), String> {
        self.secrets.save(&*self.cfg.secrets)?;
        self.secret_error = None;
        Ok(())
    }

    // ── 主循环 ─────────────────────────────────────────────

    /// 下一次要跑同步的时刻；不可调度返回 None。
    fn next_wake(&self, now: Instant) -> Option<Instant> {
        if self.syncing || !self.runnable() {
            return None;
        }
        if self.force_now {
            return Some(now);
        }
        if self.cfg.on_demand {
            return None;
        }
        let mut t = self.periodic_at;
        if let Some(d) = self.debounce_until {
            t = t.min(d);
        }
        // 退避：不早于上次失败后约定的重试时刻
        let wait = self.state.retry.next_at.saturating_sub(now_unix());
        Some(t.max(now + Duration::from_secs(wait)))
    }

    /// 阻塞运行直到 `Shutdown` / stdin EOF。
    pub fn run(&mut self, rx: Receiver<Input>) {
        self.report();
        loop {
            let now = Instant::now();
            let wake = self.next_wake(now);
            if wake.is_some_and(|w| w <= now) {
                self.start_round();
                continue;
            }
            let timeout = wake.map_or(Duration::from_secs(3600), |w| w - now);
            match rx.recv_timeout(timeout) {
                Ok(Input::Core(ToSync::Shutdown)) | Ok(Input::Eof) => {
                    self.wind_down(&rx);
                    break;
                }
                Ok(i) => self.handle(i),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.save_state();
        log!("同步进程退出");
    }

    /// 退出前：叫停进行中的一轮，等它释放编辑会话（等不到就不等了，core 会代为收尾）。
    fn wind_down(&mut self, rx: &Receiver<Input>) {
        self.cancel.store(true, Ordering::SeqCst);
        let end = Instant::now() + SHUTDOWN_WAIT;
        while self.syncing {
            let left = end.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Input::RoundDone(r)) => self.finish_round(*r),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    fn handle(&mut self, i: Input) {
        match i {
            Input::RoundDone(r) => self.finish_round(*r),
            Input::Core(ToSync::Settings(s)) => self.on_settings(s),
            Input::Core(ToSync::Command { id, op, args }) => {
                let r = self.command(op, &args);
                self.cfg.link.send(FromSync::command_reply(id, r));
                self.report();
            }
            Input::Fs(p) => self.on_fs(&p),
            Input::Core(_) | Input::Eof => {}
        }
    }

    fn on_settings(&mut self, s: SyncSettings) {
        if s == self.settings {
            return;
        }
        self.scope.set(flags_of(&s.scope));
        // 存储位置变了，已存的密码不能再发往新地址（防凭据被转发到别处）
        if backend_changed(self.settings.backend.as_ref(), s.backend.as_ref())
            && self.secrets.password.take().is_some()
        {
            log!("存储后端已变更，已清除保存的存储凭据");
            if let Err(e) = self.save_secrets() {
                log!("{e}");
            }
        }
        let moved = backend_changed(self.settings.backend.as_ref(), s.backend.as_ref());
        self.settings = s;
        if moved {
            self.info_written = None;
        }
        // 用户改了设置：解除停止态，稍后按新设置跑一轮
        if self.syncing {
            self.clear_halt_after_round = true;
        } else {
            self.state.retry.clear_halt();
            self.save_state();
        }
        self.kick();
        self.publish_info(false);
        self.report();
    }

    /// 稍后（`settings_delay`）跑一轮。
    fn kick(&mut self) {
        self.periodic_at = Instant::now() + self.cfg.timing.settings_delay;
    }

    /// 文件事件：与已同步内容或我们刚写的内容一致的（回声）忽略。
    fn on_fs(&mut self, rel: &str) {
        if !self.runnable() || !in_scope(rel, &flags_of(&self.settings.scope)) {
            return;
        }
        let hash = std::fs::read(self.abs(rel))
            .ok()
            .map(|b| content_hash(&b))
            .unwrap_or_default();
        if lock(&self.echo).get(rel) == Some(&hash) {
            return;
        }
        let synced = self
            .state
            .files
            .get(&to_remote(rel, self.cfg.class))
            .and_then(|e| e.value())
            .unwrap_or_default();
        if synced == hash {
            return;
        }
        self.debounce_until = Some(Instant::now() + self.cfg.timing.debounce);
    }

    fn abs(&self, rel: &str) -> std::path::PathBuf {
        rel.split('/')
            .fold(self.cfg.userdata_dir.clone(), |p, s| p.join(s))
    }

    // ── 同步 ───────────────────────────────────────────────

    /// 起工作线程跑一轮；结果经 `Input::RoundDone` 回主循环。
    fn start_round(&mut self) {
        let force = std::mem::take(&mut self.force_now);
        let allow_mass = std::mem::take(&mut self.allow_mass_once);
        self.debounce_until = None;
        let dek = match self.dek() {
            Ok(d) => d,
            Err(msg) => {
                self.round_failed(&msg);
                return;
            }
        };
        self.syncing = true;
        self.report();
        let now = now_unix();
        let scope = flags_of(&self.settings.scope);
        let mut ctx = SyncCtx::new(scope, now);
        ctx.class = self.cfg.class;
        ctx.force = force;
        ctx.gc = true;
        ctx.allow_mass_delete = allow_mass;
        let echo_before = lock(&self.echo).clone();
        let want = local_info(&self.settings, now);
        let info_stale_now = info_stale(self.info_written.as_ref(), &want, true);
        let mut state = self.state.clone();
        let failures_before = state.retry.failures;
        let (factory, settings, password) = (
            self.factory.clone(),
            self.settings.clone(),
            self.secrets.password.clone().unwrap_or_default(),
        );
        let mut local = FsLocal::new(
            self.cfg.userdata_dir.clone(),
            scope,
            self.cfg.link.clone(),
            self.echo.clone(),
        )
        .with_cancel(self.cancel.clone());
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new()
            .name("sync-round".into())
            .spawn(move || {
                let mut info_written = None;
                let result = match factory.build(&settings, &password) {
                    Err(m) => Err(m),
                    Ok(backend) => {
                        let r = sync_once(&*backend, &mut local, &mut state, &dek, &ctx);
                        // 本机 info 被其他设备「移除」后清单可能重新出现，info 却要等跨日才写回，
                        // 期间别人看到的是「无法解密」：每轮 stat 一次，缺了就补写。
                        let missing = || {
                            matches!(
                                backend.stat(&wind_sync::manifest::device_info_key(&state.device)),
                                Ok(None)
                            )
                        };
                        if let Ok(rep) = &r
                            && !rep.skipped
                            && (info_stale_now || missing())
                        {
                            let info = want;
                            match put_device_info(
                                &*backend,
                                &dek.devinfo_key(),
                                &state.device,
                                &info,
                            ) {
                                Ok(()) => info_written = Some(info),
                                Err(e) => log!("写设备信息失败: {}", sync_error(&e)),
                            }
                        }
                        Ok(r)
                    }
                };
                let round = Round {
                    info_written,
                    conflicts: local.conflict_files(),
                    state,
                    result,
                    now,
                    echo_before,
                    failures_before,
                };
                let _ = tx.send(Input::RoundDone(Box::new(round)));
            });
        if let Err(e) = spawned {
            self.syncing = false;
            self.round_failed(&format!("无法启动同步线程: {e}"));
        }
    }

    /// 没跑成的一轮（建后端失败等）：只记原因与下次时间，不动退避状态。
    fn round_failed(&mut self, msg: &str) {
        log!("同步失败: {msg}");
        let backoff = Duration::from_secs(backoff_secs(self.state.retry.failures));
        self.periodic_at = Instant::now() + backoff.min(interval(&self.settings));
        self.state.retry.last_error = Some(msg.to_string());
        self.save_state();
        self.report();
    }

    fn finish_round(&mut self, r: Round) {
        self.syncing = false;
        self.state = r.state;
        if r.info_written.is_some() {
            self.info_written = r.info_written;
        }
        if std::mem::take(&mut self.clear_halt_after_round) {
            self.state.retry.clear_halt();
        }
        self.conflicts = r.conflicts;
        // 本轮扫描已完成：上一轮写下的回声记录用完，清掉（本轮重新写过的保留）
        lock(&self.echo).retain(|k, v| r.echo_before.get(k) != Some(v));
        let now = Instant::now();
        let outcome = match r.result {
            Err(msg) => {
                self.round_failed(&msg);
                return;
            }
            Ok(o) => o,
        };
        match outcome {
            Ok(rep) if rep.skipped => {
                log!("处于退避等待期，本轮跳过");
                self.periodic_at = now + interval(&self.settings);
            }
            Ok(rep) => {
                for w in &rep.warnings {
                    log!("警告: {w}");
                }
                self.state.last_ok = Some(r.now);
                self.state.retry.last_error = None;
                self.free_retries = 0;
                log!(
                    "同步完成：上传 {}，下载 {}，合并 {}，冲突 {}",
                    rep.uploaded.len(),
                    rep.downloaded.len(),
                    rep.merged.len(),
                    rep.conflicts.len()
                );
                self.periodic_at = now + interval(&self.settings);
            }
            Err(e) => self.on_sync_error(e, r.failures_before, now),
        }
        self.save_state();
        self.publish_info(false);
        self.report();
    }

    /// 把本机信息写进远端名册。`daily` 为假时只在内容变了（改名等）才写；失败只记日志，下一轮会重试。
    fn publish_info(&mut self, daily: bool) {
        if self.syncing || !self.configured() || !self.ready() {
            return;
        }
        let want = local_info(&self.settings, now_unix());
        if !info_stale(self.info_written.as_ref(), &want, daily) {
            return;
        }
        let (Ok(dek), Ok(b)) = (self.dek(), self.fresh_backend(None)) else {
            return;
        };
        match put_device_info(&*b, &dek.devinfo_key(), &self.state.device, &want) {
            Ok(()) => self.info_written = Some(want),
            Err(e) => log!("写设备信息失败: {}", sync_error(&e)),
        }
    }

    fn on_sync_error(&mut self, e: SyncError, failures_before: u32, now: Instant) {
        // 稍后重试类：本批什么都没写，下一轮重新扫描即可，不算失败、不退避
        if let SyncError::Local(m) = &e
            && m.starts_with(RETRY_TAG)
            && self.free_retries < MAX_FREE_RETRIES
        {
            self.free_retries += 1;
            let msg = m.trim_start_matches(RETRY_TAG).to_string();
            log!("本轮放弃，稍后重试: {msg}");
            self.state.retry.failures = failures_before;
            self.state.retry.next_at = 0;
            self.state.retry.last_error = Some(msg);
            self.periodic_at = now + self.cfg.timing.retry_delay;
            return;
        }
        let msg = if e.is_mass_deletion() {
            mass_deletion_message(&e.to_string())
        } else {
            sync_error(&e)
        };
        log!("同步失败: {msg}");
        // 状态里存可读文案：重启后设置页直接展示
        if self.state.retry.halted.is_some()
            && (e.is_mass_deletion() || !matches!(e, SyncError::Halted(_)))
        {
            self.state.retry.halted = Some(msg.clone());
        }
        self.state.retry.last_error = Some(msg);
        // 重试按退避；停止态不排期（runnable 为假）
        let backoff = Duration::from_secs(backoff_secs(self.state.retry.failures));
        self.periodic_at = now + backoff.min(interval(&self.settings));
    }

    fn dek(&self) -> Result<Dek, String> {
        let b64 = self.secrets.dek.as_deref().ok_or("本机还没有同步密钥")?;
        Dek::from_base64(b64).map_err(|e| crypto_error(&e))
    }

    // ── 命令 ───────────────────────────────────────────────

    fn command(&mut self, op: Op, args: &Value) -> Result<Value, String> {
        let mutating = matches!(
            op,
            Op::SetSecret
                | Op::SetPassphrase
                | Op::ImportKey
                | Op::ResetRemote
                | Op::ConfirmMassDelete
                | Op::RemoveDevice
        );
        if self.syncing && mutating {
            return Err(BUSY_MSG.into());
        }
        match op {
            Op::ConfirmMassDelete => self.cmd_confirm_mass_delete(),
            Op::SetSecret => self.cmd_set_secret(args),
            Op::TestConnection => self.cmd_test_connection(args),
            Op::SetPassphrase => self.cmd_set_passphrase(args),
            Op::ImportKey => self.cmd_import_key(args),
            Op::ExportKey => self.cmd_export_key(),
            Op::SyncNow => self.cmd_sync_now(),
            Op::ResetRemote => self.cmd_reset_remote(args),
            Op::ListDevices => self.cmd_list_devices(),
            Op::RemoveDevice => self.cmd_remove_device(args),
        }
    }

    /// 用户处理完问题（改凭据 / 口令 / 密钥）后：解除停止态、丢旧连接、稍后同步。
    fn after_user_fix(&mut self) {
        self.state.retry.clear_halt();
        self.save_state();
        self.kick();
    }

    fn cmd_set_secret(&mut self, args: &Value) -> Result<Value, String> {
        match args.get("password") {
            Some(Value::String(p)) => self.secrets.password = Some(p.clone()),
            Some(Value::Null) => self.secrets.password = None,
            _ => return Err("缺少参数 password（字符串；null 表示清除）".into()),
        }
        self.save_secrets()?;
        self.after_user_fix();
        Ok(json!({ "ok": true }))
    }

    /// 不经缓存、可带临时密码地建后端：测试连接与改口令都用它。
    fn fresh_backend(&self, password: Option<&str>) -> Result<Box<dyn Backend>, String> {
        if self.settings.backend.is_none() {
            return Err("尚未配置存储后端".into());
        }
        let pw = password
            .map(str::to_string)
            .or_else(|| self.secrets.password.clone())
            .ok_or("尚未保存存储凭据（密码或访问密钥）")?;
        self.factory.build(&self.settings, &pw)
    }

    fn read_meta(b: &dyn Backend) -> Result<Option<Meta>, String> {
        match b
            .get(&manifest::meta_key())
            .map_err(|e| backend_error(&e))?
        {
            None => Ok(None),
            Some(raw) => Meta::from_json(&raw)
                .map(Some)
                .map_err(|e| crypto_error(&e)),
        }
    }

    fn cmd_test_connection(&mut self, args: &Value) -> Result<Value, String> {
        let b = self.fresh_backend(str_arg(args, "password"))?;
        b.test_connection().map_err(|e| backend_error(&e))?;
        let meta = Self::read_meta(&*b)?;
        let joined = match (&meta, self.dek()) {
            (Some(m), Ok(d)) => m.check_dek(&d),
            _ => false,
        };
        Ok(json!({ "ok": true, "remote_initialized": meta.is_some(), "joined": joined }))
    }

    /// DEK 变了说明换了同步库：旧的清单基线对新远端没有意义，重建（设备号也换新）。
    fn adopt_dek(&mut self, dek: &Dek) -> Result<(), String> {
        let b64 = dek.to_base64();
        let changed = self.secrets.dek.as_deref() != Some(b64.as_str());
        self.secrets.dek = Some(b64);
        self.save_secrets()?;
        if changed {
            self.state = SyncState::new_random();
            self.info_written = None;
        }
        self.after_user_fix();
        self.publish_info(false);
        Ok(())
    }

    /// 远端无 meta → 初始化；有 meta：已加入（持有匹配的 DEK 或给了旧口令）→ 改口令，否则用口令加入。
    fn cmd_set_passphrase(&mut self, args: &Value) -> Result<Value, String> {
        let pass = str_arg(args, "passphrase").unwrap_or_default();
        if pass.is_empty() {
            return Err("口令不能为空".into());
        }
        let b = self.fresh_backend(None)?;
        let kdf = self.cfg.kdf;
        let se = |e: SyncError| sync_error(&e);
        let (dek, mode) = match Self::read_meta(&*b)? {
            None => (remote::init_remote(&*b, pass, kdf).map_err(se)?, "init"),
            Some(meta) => {
                let old = str_arg(args, "old_passphrase").filter(|s| !s.is_empty());
                let held = self.dek().ok().filter(|d| meta.check_dek(d));
                if let Some(old) = old {
                    remote::change_passphrase(&*b, old, pass, kdf).map_err(se)?;
                    (
                        remote::join_with_passphrase(&*b, pass).map_err(se)?,
                        "change",
                    )
                } else if let Some(d) = held {
                    remote::rewrap_passphrase(&*b, &d, pass, kdf).map_err(se)?;
                    (d, "change")
                } else {
                    (remote::join_with_passphrase(&*b, pass).map_err(se)?, "join")
                }
            }
        };
        self.adopt_dek(&dek)?;
        log!("口令设置完成（{mode}）");
        Ok(json!({ "ok": true, "mode": mode }))
    }

    fn cmd_import_key(&mut self, args: &Value) -> Result<Value, String> {
        let key = str_arg(args, "key").unwrap_or_default();
        if key.trim().is_empty() {
            return Err("密钥不能为空".into());
        }
        let b = self.fresh_backend(None)?;
        let dek = remote::join_with_key(&*b, key).map_err(|e| sync_error(&e))?;
        self.adopt_dek(&dek)?;
        Ok(json!({ "ok": true }))
    }

    fn cmd_export_key(&self) -> Result<Value, String> {
        let dek = self
            .dek()
            .map_err(|_| "本机尚未加入加密同步，没有可导出的密钥".to_string())?;
        Ok(json!({ "key": dek.to_base64() }))
    }

    fn cmd_sync_now(&mut self) -> Result<Value, String> {
        if !self.configured() {
            return Err("同步未启用或尚未配置存储后端".into());
        }
        if self.secrets.password.is_none() {
            return Err("尚未保存存储凭据".into());
        }
        if self.secrets.dek.is_none() {
            return Err("尚未设置同步口令或导入密钥".into());
        }
        if self.syncing {
            // 进行中的这一轮跑完后再来一轮
            self.clear_halt_after_round = true;
        } else {
            self.state.retry.clear_halt();
        }
        self.force_now = true;
        Ok(json!({ "queued": true }))
    }

    /// 大批量删除护栏停止后，用户确认「这些删除是真的」：解除停止态，下一轮放行一次。
    fn cmd_confirm_mass_delete(&mut self) -> Result<Value, String> {
        let pending = self.state.retry.halted.as_deref().is_some_and(|h| {
            h.contains(MASS_MARK) || h.starts_with(wind_sync::error::MASS_DELETION_PREFIX)
        });
        if !pending {
            return Err("当前没有等待确认的大批量删除".into());
        }
        self.state.retry.clear_halt();
        self.allow_mass_once = true;
        self.force_now = true;
        self.save_state();
        Ok(json!({ "queued": true }))
    }

    fn cmd_list_devices(&self) -> Result<Value, String> {
        let dek = self.dek().map_err(|_| "本机尚未加入加密同步".to_string())?;
        let b = self.fresh_backend(None)?;
        let list = list_devices(&*b, &dek.devinfo_key()).map_err(|e| sync_error(&e))?;
        let devices: Vec<Value> = list
            .into_iter()
            .map(|(id, info)| {
                let is_self = id == self.state.device;
                match info {
                    Ok(i) => json!({
                        "id": id, "name": i.name, "platform": i.platform,
                        "app_version": i.app_version, "caps": i.caps,
                        "last_seen": i.last_seen, "is_self": is_self, "readable": true,
                    }),
                    Err(_) => json!({
                        "id": id, "name": "", "platform": "", "app_version": "",
                        "caps": [], "last_seen": 0, "is_self": is_self, "readable": false,
                    }),
                }
            })
            .collect();
        Ok(json!({ "devices": devices }))
    }

    /// 删除某设备在远端的清单与设备信息。它若仍在运行，下次同步会重新出现。
    fn cmd_remove_device(&mut self, args: &Value) -> Result<Value, String> {
        if args.get("confirm").and_then(Value::as_bool) != Some(true) {
            return Err("移除设备须明确确认（confirm=true）".into());
        }
        let dev = str_arg(args, "dev")
            .filter(|d| !d.is_empty())
            .ok_or("缺少参数 dev")?;
        if dev == self.state.device {
            return Err("不能移除本机".into());
        }
        let b = self.fresh_backend(None)?;
        let removed = remove_device(&*b, dev).map_err(|e| sync_error(&e))?;
        log!("已移除设备 {dev}（{removed} 个对象）");
        Ok(json!({ "ok": true, "removed": removed }))
    }

    /// 清空远端后以本机为准重新上传。需 `confirm: true` 与新口令；本机有 DEK 则沿用（已导出的密钥仍有效）。
    fn cmd_reset_remote(&mut self, args: &Value) -> Result<Value, String> {
        if args.get("confirm").and_then(Value::as_bool) != Some(true) {
            return Err("重置远端会清空其中全部同步数据，须明确确认（confirm=true）".into());
        }
        let pass = str_arg(args, "passphrase").unwrap_or_default();
        if pass.is_empty() {
            return Err("重置后需要设置新口令".into());
        }
        let b = self.fresh_backend(None)?;
        let dek = self.dek().unwrap_or_else(|_| Dek::generate());
        remote::reset_remote(&*b, &dek, pass, self.cfg.kdf).map_err(|e| sync_error(&e))?;
        self.secrets.dek = Some(dek.to_base64());
        self.save_secrets()?;
        // 远端已空：旧基线作废，所有本地文件当作新增上传
        self.state = SyncState::new_random();
        self.info_written = None;
        self.after_user_fix();
        self.publish_info(false);
        log!("远端已重置");
        Ok(json!({ "ok": true }))
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
