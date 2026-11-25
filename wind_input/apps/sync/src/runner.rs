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
use wind_sync::devices::{
    DeviceEntry, DeviceInfo, id_taken, list_devices, put_device_info, remove_device,
};
use wind_sync::identity::{self, Overwrite};
use wind_sync::ids;
use wind_sync::scope::{DeviceClass, Kind, Namespace, in_scope, kind, to_remote};
use wind_sync::state::backoff_secs;
use wind_sync::{
    Backend, LocalStore, SyncCtx, SyncError, SyncReport, SyncState, manifest, remote, sync_once,
};
use wind_sync_ipc::{Backend as Target, FromSync, Op, RoundStats, Status, SyncSettings, ToSync};

use crate::backends::BackendFactory;
use crate::backup;
use crate::errors::{backend_error, crypto_error, sync_error};
use crate::link::CoreLink;
use crate::local::{Echo, FsLocal, RETRY_TAG, lock};
use crate::log::{log, now_unix};
use crate::secrets::{SecretStore, Secrets};
use crate::statefile;
use crate::watch;

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
/// 加入同步库后、确认身份前拒绝同步的提示。
const IDENTITY_MSG: &str = "请先确认本机身份";
/// 覆盖本机设置只完成一部分时的停止态文案：只有重新加入成功才解除。
const JOIN_INCOMPLETE: &str = "加入设备组未完成，请重试";
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
    /// 绿色版：默认设备号加 `-portable`。
    pub portable: bool,
    /// 只在被要求时（`sync_now` / 确认大批量删除）才跑一轮，不按间隔与设置变更自行调度。
    /// 进程内运行的手机端由宿主决定何时同步。
    pub on_demand: bool,
}

pub struct Runner {
    cfg: Config,
    settings: SyncSettings,
    secrets: Secrets,
    secret_error: Option<String>,
    state: SyncState,
    echo: Echo,
    factory: Arc<dyn BackendFactory>,
    tx: Sender<Input>,
    cancel: Arc<AtomicBool>,
    syncing: bool,
    /// 同步进行中用户改了设置：本轮结束后再解除停止态。
    clear_halt_after_round: bool,
    force_now: bool,
    /// 已结束的轮次数与最近一轮结果。
    round_no: u64,
    last_round: Option<RoundStats>,
    /// 进行中的这一轮是否由强制触发。
    round_forced: bool,
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

/// 本机显示名：设置里填了用它，否则取主机名。
fn display_name(settings: &SyncSettings) -> String {
    let name = settings.device_name.trim();
    if name.is_empty() {
        crate::hostname::host_name()
    } else {
        name.to_string()
    }
}

/// 本机所在组号；状态里为空按单机组（= 设备号）。
fn group_of(state: &SyncState) -> &str {
    if state.group.is_empty() {
        &state.device
    } else {
        &state.group
    }
}

/// 本机要写进名册的信息。
fn local_info(settings: &SyncSettings, state: &SyncState, now: u64) -> DeviceInfo {
    DeviceInfo {
        name: display_name(settings),
        platform: platform().into(),
        app_version: env!("WIND_APP_VERSION").into(),
        caps: vec!["files".into()],
        last_seen: now,
        group: group_of(state).to_string(),
        group_name: state.group_name.clone(),
        group_name_at: state.group_name_at,
    }
}

/// 名册里同组他人的组名比本机新则采用（只改本机状态）。
fn adopt_group_name(state: &mut SyncState, roster: &[DeviceEntry]) {
    let group = group_of(state).to_string();
    let newest = roster
        .iter()
        .filter(|(dev, _)| *dev != state.device)
        .filter_map(|(_, r)| r.as_ref().ok())
        .filter(|i| i.group == group && i.group_name_at > state.group_name_at)
        .max_by_key(|i| i.group_name_at);
    if let Some(i) = newest {
        state.group_name = i.group_name.clone();
        state.group_name_at = i.group_name_at;
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

/// 首个空闲组号：`base`、`base-2`…；不等于 `taken`，也不被名册里 `skip` 以外的成员使用。
fn free_group(
    roster: &[DeviceEntry],
    base: &str,
    skip: &[&str],
    taken: Option<&str>,
) -> Result<String, String> {
    let used = |g: &str| {
        taken == Some(g)
            || roster
                .iter()
                .any(|(d, r)| !skip.contains(&d.as_str()) && r.as_ref().is_ok_and(|i| i.group == g))
    };
    (1..1000)
        .map(|n| ids::with_suffix(base, n))
        .find(|g| !used(g))
        .ok_or_else(|| "找不到可用的组号".to_string())
}

/// 重新加入成功：解除「加入设备组未完成」的停止态与错误文案。
fn clear_join_halt(state: &mut SyncState) {
    if state.retry.halted.as_deref() == Some(JOIN_INCOMPLETE) {
        state.retry.last_error = None;
    }
    state.retry.clear_halt();
}

/// 名册里的平台对应的设备类别。
fn class_of_platform(platform: &str) -> DeviceClass {
    if platform == "android" {
        DeviceClass::Mobile
    } else {
        DeviceClass::Desktop
    }
}

/// 删除改号前的旧号目录：删成功或已归别处的移出待删列表，失败的留待下一轮。
fn retire_old_ids(b: &dyn Backend, dek: &Dek, state: &mut SyncState) {
    let device = state.device.clone();
    state.retired_ids.retain(|r| {
        if r.id == device {
            return false;
        }
        match identity::retire_device(b, dek, r) {
            Ok(true) => {
                log!("已删除旧设备号 {}", r.id);
                false
            }
            Ok(false) => {
                log!("旧设备号 {} 已被别处继续使用，不删除", r.id);
                false
            }
            Err(e) => {
                log!("删除旧设备号 {} 失败: {}", r.id, sync_error(&e));
                true
            }
        }
    });
}

impl Runner {
    fn ns(&self) -> Namespace {
        Namespace::new(self.cfg.class, group_of(&self.state))
    }

    pub fn new(mut cfg: Config, tx: Sender<Input>) -> Runner {
        let (secrets, secret_error) = match Secrets::load(&*cfg.secrets) {
            Ok(s) => (s, None),
            Err(e) => {
                log!("{e}");
                (Secrets::default(), Some(e))
            }
        };
        let default_id = ids::default_device_id(&display_name(&cfg.settings), cfg.portable);
        let state = statefile::load(&cfg.state_dir, &default_id);
        let settings = cfg.settings.clone();
        let watch = cfg.watch.then(|| {
            watch::start(&cfg.userdata_dir, tx.clone())
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
            factory,
            tx,
            cancel: Arc::new(AtomicBool::new(false)),
            syncing: false,
            clear_halt_after_round: false,
            force_now: false,
            round_no: 0,
            last_round: None,
            round_forced: false,
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

    /// 能自动调度：已配置、机密齐全、身份已确认、没停在错误态。
    fn runnable(&self) -> bool {
        self.configured()
            && self.ready()
            && self.state.identity_confirmed
            && self.state.retry.halted.is_none()
    }

    /// 已加入同步库但尚未确认身份。
    fn needs_identity(&self) -> bool {
        self.secrets.dek.is_some() && !self.state.identity_confirmed
    }

    /// 覆盖本机设置只完成一部分：此时同步会把半成品当本机修改传进旧组。
    fn join_incomplete(&self) -> bool {
        self.state.retry.halted.as_deref() == Some(JOIN_INCOMPLETE)
    }

    /// 解除停止态；加入组未完成的停止态除外。
    fn clear_halt(&mut self) {
        if !self.join_incomplete() {
            self.state.retry.clear_halt();
        }
    }

    /// 停在「另一台设备在用本机号」上。
    fn id_conflict(&self) -> bool {
        let msg = sync_error(&SyncError::DeviceIdConflict(self.state.device.clone()));
        self.state.retry.halted.as_deref() == Some(msg.as_str())
    }

    pub fn status(&self) -> Status {
        let phase = if !self.configured() {
            "not_configured"
        } else if !self.ready() {
            "needs_setup"
        } else if self.syncing {
            "syncing"
        } else if self.id_conflict() {
            "error"
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
            device: self.state.device.clone(),
            group: group_of(&self.state).to_string(),
            group_name: self.state.group_name.clone(),
            needs_identity: self.needs_identity(),
            round: self.round_no,
            last_round: self.last_round.clone(),
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
            self.clear_halt();
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
        if !self.runnable() || !in_scope(rel) {
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
            .get(&to_remote(rel, &self.ns()))
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
        self.round_forced = force;
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
        let mut ctx = SyncCtx::new(now, self.ns());
        ctx.force = force;
        ctx.gc = true;
        ctx.allow_mass_delete = allow_mass;
        let echo_before = lock(&self.echo).clone();
        let want = local_info(&self.settings, &self.state, now);
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
                        if let Ok(rep) = &r
                            && !rep.skipped
                        {
                            retire_old_ids(&*backend, &dek, &mut state);
                        }
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
        self.end_round(false, None, Some(msg.to_string()));
        self.save_state();
        self.report();
    }

    /// 一轮结束：编号 +1 并记下统计。
    fn end_round(&mut self, ok: bool, rep: Option<&SyncReport>, error: Option<String>) {
        self.round_no += 1;
        self.last_round = Some(RoundStats {
            ok,
            downloaded: rep.map_or(0, |r| r.downloaded.len() as u32),
            uploaded: rep.map_or(0, |r| r.uploaded.len() as u32),
            conflicts: rep.map_or(0, |r| {
                r.conflicts.iter().filter(|c| c.copy.is_some()).count() as u32
            }),
            error,
            forced: self.round_forced,
        });
    }

    fn finish_round(&mut self, r: Round) {
        self.syncing = false;
        self.state = r.state;
        if r.info_written.is_some() {
            self.info_written = r.info_written;
        }
        if std::mem::take(&mut self.clear_halt_after_round) {
            self.clear_halt();
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
                self.end_round(false, None, Some("处于退避等待期".into()));
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
                self.end_round(true, Some(&rep), None);
            }
            Err(e) => {
                self.on_sync_error(e, r.failures_before, now);
                let msg = self.state.retry.last_error.clone();
                self.end_round(false, None, msg);
            }
        }
        self.save_state();
        self.publish_info(false);
        self.report();
    }

    /// 把本机信息写进远端名册。`daily` 为假时只在内容变了（改名等）才写；失败只记日志，下一轮会重试。
    fn publish_info(&mut self, daily: bool) {
        // 未确认身份时本机号可能是别人的，不写名册
        if self.syncing || !self.configured() || !self.ready() || !self.state.identity_confirmed {
            return;
        }
        let want = local_info(&self.settings, &self.state, now_unix());
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
                | Op::RenameDevice
                | Op::SetGroupName
                | Op::JoinGroup
                | Op::LeaveGroup
                | Op::ClaimDevice
                | Op::ConfirmIdentity
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
            Op::RenameDevice => self.cmd_rename_device(args),
            Op::SetGroupName => self.cmd_set_group_name(args),
            Op::JoinGroup => self.cmd_join_group(args),
            Op::LeaveGroup => self.cmd_leave_group(args),
            Op::ClaimDevice => self.cmd_claim_device(args),
            Op::ConfirmIdentity => self.cmd_confirm_identity(),
        }
    }

    /// 用户处理完问题（改凭据 / 口令 / 密钥）后：解除停止态、丢旧连接、稍后同步。
    fn after_user_fix(&mut self) {
        self.clear_halt();
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

    /// DEK 变了说明换了同步库：旧的清单基线对新远端没有意义，重建（沿用设备号，回到单机组）。
    /// 密钥变了：只有初始化（`init`）直接确认身份，其余一律按加入处理、待确认；密钥没变则保持原有身份。
    fn adopt_dek(&mut self, dek: &Dek, init: bool) -> Result<(), String> {
        let b64 = dek.to_base64();
        let changed = self.secrets.dek.as_deref() != Some(b64.as_str());
        self.secrets.dek = Some(b64);
        self.save_secrets()?;
        if changed {
            let device = self.state.device.clone();
            self.state = SyncState::new(device.clone());
            self.state.group = device;
            self.state.identity_confirmed = init;
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
        self.adopt_dek(&dek, mode == "init")?;
        log!("口令设置完成（{mode}）");
        Ok(json!({ "ok": true, "mode": mode }))
    }

    fn cmd_import_key(&mut self, args: &Value) -> Result<Value, String> {
        let key = str_arg(args, "key").unwrap_or_default();
        if key.trim().is_empty() {
            return Err("密钥不能为空".into());
        }
        // 先在本地校验格式，免得被说成「远端格式不认识」
        wind_sync::crypto::Dek::from_base64(key).map_err(|e| {
            let d = match e {
                wind_sync::crypto::CryptoError::Format(d) => d,
                e => e.to_string(),
            };
            format!("密钥格式不对（{d}），请粘贴完整的导出密钥")
        })?;
        let b = self.fresh_backend(None)?;
        let dek = remote::join_with_key(&*b, key).map_err(|e| sync_error(&e))?;
        self.adopt_dek(&dek, false)?;
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
        if !self.state.identity_confirmed {
            return Err(IDENTITY_MSG.into());
        }
        if self.join_incomplete() {
            return Err(JOIN_INCOMPLETE.into());
        }
        let ticket = self.round_no + 1;
        if self.syncing {
            // 进行中的这一轮结束即算完成，不再另排一轮
            self.clear_halt_after_round = true;
            return Ok(json!({ "queued": false, "busy": true, "ticket": ticket }));
        }
        self.clear_halt();
        self.force_now = true;
        Ok(json!({ "queued": true, "ticket": ticket }))
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

    fn cmd_list_devices(&mut self) -> Result<Value, String> {
        let dek = self.dek().map_err(|_| "本机尚未加入加密同步".to_string())?;
        let b = self.fresh_backend(None)?;
        let list = list_devices(&*b, &dek.devinfo_key()).map_err(|e| sync_error(&e))?;
        let before = self.state.group_name_at;
        adopt_group_name(&mut self.state, &list);
        if self.state.group_name_at != before {
            self.save_state();
        }
        let devices: Vec<Value> = list
            .iter()
            .map(|(id, info)| {
                let is_self = *id == self.state.device;
                match info {
                    Ok(i) => json!({
                        "id": id, "name": i.name, "platform": i.platform,
                        "app_version": i.app_version, "caps": i.caps,
                        "last_seen": i.last_seen, "is_self": is_self, "readable": true,
                        "group": i.group,
                        "group_name": wind_sync::devices::group_name_of(&list, &i.group)
                            .unwrap_or_default(),
                    }),
                    Err(_) => json!({
                        "id": id, "name": "", "platform": "", "app_version": "",
                        "caps": [], "last_seen": 0, "is_self": is_self, "readable": false,
                        "group": "", "group_name": "",
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

    // ── 设备组与身份 ───────────────────────────────────────

    /// 身份操作的前提：已持有密钥与存储凭据。
    fn identity_ctx(&self) -> Result<(Dek, Box<dyn Backend>), String> {
        let dek = self.dek().map_err(|_| "本机尚未加入加密同步".to_string())?;
        Ok((dek, self.fresh_backend(None)?))
    }

    /// 身份变更之后：存状态、重写名册、解除停止态、稍后同步。
    fn after_identity_change(&mut self) {
        self.info_written = None;
        self.after_user_fix();
        self.publish_info(false);
    }

    /// 覆盖本机设置用的本地存储：与 `join_group` 一致。
    fn local_all(&self) -> FsLocal {
        FsLocal::new(
            self.cfg.userdata_dir.clone(),
            self.cfg.link.clone(),
            self.echo.clone(),
        )
    }

    /// 用组的设置覆盖本机前备份本机设置。
    fn backup_local(&self, local: &FsLocal) -> Result<std::path::PathBuf, String> {
        let paths = local.list().map_err(|e| e.0)?;
        let dir = backup::backup_settings(&self.cfg.userdata_dir, &paths, now_unix())?;
        log!("已备份本机设置");
        Ok(dir)
    }

    /// 本机设置文件的（路径, 内容摘要），判断覆盖是否已经动过本机。
    fn settings_digest(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .local_all()
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| kind(p) == Kind::Setting)
            .map(|p| {
                let h = std::fs::read(self.abs(&p))
                    .map(|b| content_hash(&b))
                    .unwrap_or_default();
                (p, h)
            })
            .collect();
        v.sort();
        v
    }

    /// 覆盖本机设置失败：本机已被改动一部分则进停止态，免得半成品被当本机修改同步进旧组。
    fn join_failed(&mut self, before: &[(String, String)], msg: String) -> String {
        if self.settings_digest() == before {
            return msg;
        }
        log!("加入设备组中途失败，本机设置已部分改写: {msg}");
        self.state.retry.halted = Some(JOIN_INCOMPLETE.into());
        self.state.retry.last_error = Some(JOIN_INCOMPLETE.into());
        self.save_state();
        format!("{JOIN_INCOMPLETE}（{msg}）")
    }

    /// 本机号已被远端他人占用时改用首个空闲的 `<号>-<n>`。只用于未确认身份时：本机从未以这个号发布过。
    fn ensure_free_id(&mut self, b: &dyn Backend) -> Result<(), String> {
        let se = |e: SyncError| sync_error(&e);
        let base = self.state.device.clone();
        if !id_taken(b, &base).map_err(se)? {
            return Ok(());
        }
        for n in 2..1000 {
            let id = ids::with_suffix(&base, n);
            if !id_taken(b, &id).map_err(se)? {
                identity::rename_device(b, &mut self.state, &id).map_err(se)?;
                // 旧号属于别的设备，不能当本机旧号删除
                self.state.retired_ids.retain(|x| x.id != base);
                log!("设备号 {base} 已被占用，本机改用 {id}");
                return Ok(());
            }
        }
        Err(format!("找不到空闲的设备号（{base}-n）"))
    }

    /// 以当前设备号新建单机组并确认身份。
    fn cmd_confirm_identity(&mut self) -> Result<Value, String> {
        // 已确认则不动（不把已在组里的设备拉回单机组）
        if self.state.identity_confirmed {
            let (device, group) = (self.state.device.clone(), group_of(&self.state));
            return Ok(json!({ "ok": true, "device": device, "group": group }));
        }
        let (dek, b) = self.identity_ctx()?;
        self.ensure_free_id(&*b)?;
        let device = self.state.device.clone();
        let roster = list_devices(&*b, &dek.devinfo_key()).map_err(|e| sync_error(&e))?;
        let group = free_group(&roster, &device, &[&device], None)?;
        identity::leave_group(&mut self.state, &group);
        self.state.group_name.clear();
        self.state.group_name_at = 0;
        self.state.identity_confirmed = true;
        self.after_identity_change();
        log!("已确认本机身份（单机组 {group}）");
        Ok(json!({ "ok": true, "device": device, "group": group }))
    }

    fn cmd_rename_device(&mut self, args: &Value) -> Result<Value, String> {
        let id = str_arg(args, "id")
            .filter(|s| !s.is_empty())
            .ok_or("缺少参数 id")?;
        if id == self.state.device {
            return Err("新设备号与当前相同".into());
        }
        let (_, b) = self.identity_ctx()?;
        let old = self.state.device.clone();
        // 撞号时旧号另有主人，未确认身份时本机从未以旧号发布过：这两种情况旧号都不能删
        let retire = self.state.identity_confirmed && !self.id_conflict();
        if self.state.group.is_empty() {
            self.state.group = old.clone();
        }
        identity::rename_device(&*b, &mut self.state, id).map_err(|e| sync_error(&e))?;
        if !retire {
            self.state.retired_ids.retain(|x| x.id != old);
        }
        self.after_identity_change();
        log!("设备号已改为 {id}");
        Ok(json!({ "ok": true, "device": id }))
    }

    fn cmd_set_group_name(&mut self, args: &Value) -> Result<Value, String> {
        let name = str_arg(args, "name").ok_or("缺少参数 name")?.trim();
        if name.chars().count() > wind_sync_ipc::MAX_DEVICE_NAME_CHARS {
            return Err(format!(
                "组名不能超过 {} 个字符",
                wind_sync_ipc::MAX_DEVICE_NAME_CHARS
            ));
        }
        if self.state.group.is_empty() {
            self.state.group = self.state.device.clone();
        }
        self.state.group_name = name.to_string();
        // 时钟回拨时也要比本机旧值新，才能在名册里胜出
        self.state.group_name_at = now_unix().max(self.state.group_name_at + 1);
        self.save_state();
        self.publish_info(false);
        Ok(json!({ "ok": true }))
    }

    fn cmd_join_group(&mut self, args: &Value) -> Result<Value, String> {
        let group = str_arg(args, "group").unwrap_or_default();
        if !ids::valid_id(group) {
            return Err(format!("组号 {group} 不可用"));
        }
        let mode = match str_arg(args, "mode") {
            Some("theirs") => Overwrite::Theirs,
            Some("mine") => Overwrite::Mine,
            _ => return Err("参数 mode 须为 theirs 或 mine".into()),
        };
        let (dek, b) = self.identity_ctx()?;
        let roster = list_devices(&*b, &dek.devinfo_key()).map_err(|e| sync_error(&e))?;
        let class = self.cfg.class;
        // 未确认身份时本机从未发布过，名册里同号的条目是别人的
        let me = self
            .state
            .identity_confirmed
            .then_some(self.state.device.as_str());
        let exists = roster.iter().any(|(d, r)| {
            Some(d.as_str()) != me
                && r.as_ref()
                    .is_ok_and(|i| i.group == group && class_of_platform(&i.platform) == class)
        });
        if !exists {
            return Err(format!(
                "设备组 {group} 不存在，或其成员与本机不是同一类设备（桌面与手机不能同组）"
            ));
        }
        if !self.state.identity_confirmed {
            self.ensure_free_id(&*b)?;
        }
        let mut local = self.local_all();
        if mode == Overwrite::Theirs {
            self.backup_local(&local)?;
        }
        let before = self.settings_digest();
        let ns = Namespace::new(self.cfg.class, group);
        let mut state = self.state.clone();
        let rep = match identity::join_group(&*b, &mut local, &mut state, &dek, &ns, mode) {
            Ok(r) => r,
            Err(e) => return Err(self.join_failed(&before, sync_error(&e))),
        };
        state.group_name.clear();
        state.group_name_at = 0;
        adopt_group_name(&mut state, &roster);
        state.identity_confirmed = true;
        clear_join_halt(&mut state);
        self.state = state;
        self.after_identity_change();
        log!(
            "已加入设备组 {group}：写入 {}，删除 {}",
            rep.written.len(),
            rep.deleted.len()
        );
        Ok(json!({
            "ok": true, "group": group,
            "written": rep.written.len(), "deleted": rep.deleted.len(),
        }))
    }

    /// 以本机设置新建单机组；组号取设备号，已被占用（含当前组）则加序号。
    fn cmd_leave_group(&mut self, args: &Value) -> Result<Value, String> {
        if !self.state.identity_confirmed {
            return Err(IDENTITY_MSG.into());
        }
        let name = str_arg(args, "name").map(str::trim).unwrap_or_default();
        if name.chars().count() > wind_sync_ipc::MAX_DEVICE_NAME_CHARS {
            return Err(format!(
                "组名不能超过 {} 个字符",
                wind_sync_ipc::MAX_DEVICE_NAME_CHARS
            ));
        }
        let (dek, b) = self.identity_ctx()?;
        let roster = list_devices(&*b, &dek.devinfo_key()).map_err(|e| sync_error(&e))?;
        let device = self.state.device.clone();
        let current = group_of(&self.state).to_string();
        let group = free_group(&roster, &device, &[&device], Some(&current))?;
        identity::leave_group(&mut self.state, &group);
        self.state.group_name = name.to_string();
        self.state.group_name_at = if name.is_empty() { 0 } else { now_unix() };
        self.after_identity_change();
        log!("已退出设备组 {current}，新建单机组 {group}");
        Ok(json!({ "ok": true, "group": group }))
    }

    /// 认领已有设备：沿用它的号与组，用组的设置覆盖本机（先备份）。
    fn cmd_claim_device(&mut self, args: &Value) -> Result<Value, String> {
        let dev = str_arg(args, "dev")
            .filter(|d| !d.is_empty())
            .ok_or("缺少参数 dev")?;
        if self.state.identity_confirmed && dev == self.state.device {
            return Err("不能认领本机".into());
        }
        let (dek, b) = self.identity_ctx()?;
        let se = |e: SyncError| sync_error(&e);
        let mut local = self.local_all();
        self.backup_local(&local)?;
        let roster = list_devices(&*b, &dek.devinfo_key()).map_err(se)?;
        let mut state = self.state.clone();
        let mut group = identity::claim_device(&*b, &mut state, &dek, dev).map_err(se)?;
        let from_roster = roster
            .iter()
            .any(|(d, r)| d == dev && r.as_ref().is_ok_and(|i| i.group == group));
        if from_roster {
            let before = self.settings_digest();
            let ns = Namespace::new(self.cfg.class, group.clone());
            if let Err(e) =
                identity::join_group(&*b, &mut local, &mut state, &dek, &ns, Overwrite::Theirs)
            {
                return Err(self.join_failed(&before, sync_error(&e)));
            }
        } else {
            // 名册没给出组号（缺失或不合法）：回退的组号与他人查重；组未知就不覆盖本机设置
            let fallback = free_group(&roster, dev, &[dev], None)?;
            if fallback == group {
                let before = self.settings_digest();
                let ns = Namespace::new(self.cfg.class, group.clone());
                if let Err(e) =
                    identity::join_group(&*b, &mut local, &mut state, &dek, &ns, Overwrite::Theirs)
                {
                    return Err(self.join_failed(&before, sync_error(&e)));
                }
            } else {
                identity::leave_group(&mut state, &fallback);
                group = fallback;
            }
        }
        state.identity_confirmed = true;
        clear_join_halt(&mut state);
        // 已确认过身份的设备改认领别的号：原号与待删旧号都归入待删（撞号时原号另有主人，不删）
        if self.state.identity_confirmed && !self.id_conflict() {
            state.retired_ids = std::mem::take(&mut self.state.retired_ids);
            state.retired_ids.push(wind_sync::state::RetiredId {
                id: self.state.device.clone(),
                nonce: self.state.last_nonce.clone(),
                gen_: self.state.gen_,
            });
        }
        self.state = state;
        self.after_identity_change();
        log!("已认领设备 {dev}（组 {group}）");
        Ok(json!({ "ok": true, "device": dev, "group": group }))
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
        // 远端已空：旧基线作废，所有本地文件当作新增上传；本机就是新库的首台设备
        let mut fresh = SyncState::new(self.state.device.clone());
        fresh.group = group_of(&self.state).to_string();
        fresh.group_name = std::mem::take(&mut self.state.group_name);
        fresh.group_name_at = self.state.group_name_at;
        fresh.identity_confirmed = true;
        self.state = fresh;
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
