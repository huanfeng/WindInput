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
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wind_sync::crypto::{Dek, Meta, content_hash};
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
use crate::learn_round;
use crate::link::CoreLink;
use crate::local::{Echo, FsLocal, RETRY_TAG, local_skips, lock};
use crate::log::{log, now_unix};
use crate::secrets::{SecretStore, Secrets};
use crate::statefile;
use crate::usage;
use crate::watch;

mod commands;
mod groups;

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
    /// 手动刷新的空间占用统计完成：(统计时的库, 结果)。
    UsageDone(Box<(String, Result<wind_sync_ipc::Usage, String>)>),
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
    /// 本轮开始时的库位置，核对结论只对它有效。
    library_id: Option<String>,
    /// 本轮成功写出的设备信息。
    info_written: Option<DeviceInfo>,
    /// 学习数据同步结果：None = 本轮没跑。
    learn: Option<Result<(), String>>,
    /// 学习数据同步成功后查到的已移除但仍有学习数据的设备。
    removed_with_data: Option<Vec<String>>,
    /// 本轮顺带的空间占用统计：(统计时的库, 结果)；None = 没算。
    usage: Option<(String, Result<wind_sync_ipc::Usage, String>)>,
}

impl std::fmt::Debug for Round {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Round")
    }
}

/// 学习数据同步的最短间隔（强制轮次不受限）。
pub const LEARN_INTERVAL_SECS: u64 = 1800;

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
    /// 学习数据上次同步成功的时间（unix 秒）。
    learn_last_ok: u64,
    /// 学习数据轮连续失败次数与下次允许自动重试的时间（unix 秒），退避同文件同步。
    learn_failures: u32,
    learn_retry_at: u64,
    /// 用户目录下属于本机目录或安装目录的前缀，扫描与监听都跳过。
    skips: Vec<String>,
    /// 已移除但远端仍有学习数据的设备号。
    removed_with_data: Vec<String>,
    /// 空间占用：缓存、统计中、最近失败原因、失败后下次自动重试的时间（unix 秒）。
    usage: Option<usage::Cached>,
    usage_busy: bool,
    usage_error: Option<String>,
    usage_retry_at: u64,
    _watch: Option<watch::Watch>,
}

fn interval(s: &SyncSettings) -> Duration {
    Duration::from_secs(u64::from(s.interval_min.max(1)) * 60)
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// 存储位置（地址 / 桶 / 账号）的指纹，不含机密。区域、前缀、寻址方式不算：凭据发往的还是同一处。
fn backend_fingerprint(t: Option<&Target>) -> Option<String> {
    match t? {
        Target::Webdav { url, user } => Some(format!("webdav\n{}\n{user}", url.trim())),
        Target::S3 {
            endpoint,
            bucket,
            access_key_id,
            ..
        } => Some(format!(
            "s3\n{}\n{}\n{access_key_id}",
            endpoint.trim(),
            bucket.trim()
        )),
    }
}

/// 同步库的位置：在凭据指纹之外再加 S3 前缀（WebDAV 的路径已在 URL 里）。
fn library_id(t: Option<&Target>) -> Option<String> {
    let base = backend_fingerprint(t)?;
    match t? {
        Target::S3 { prefix, .. } => Some(format!("{base}\n{}", prefix.trim().trim_matches('/'))),
        _ => Some(base),
    }
}

/// 学习数据私有状态的库标识：库位置 + 同步密钥校验值。
fn learn_library(settings: &SyncSettings, secrets: &Secrets) -> Option<String> {
    let lib = library_id(settings.backend.as_ref())?;
    let dek = Dek::from_base64(secrets.dek.as_deref()?).ok()?;
    Some(learn_round::library_key(&lib, &Meta::new(&dek).check))
}

/// 存储位置是否变了。
fn backend_changed(old: Option<&Target>, new: Option<&Target>) -> bool {
    backend_fingerprint(old) != backend_fingerprint(new)
}

/// 已存的存储密码只属于保存它时的存储位置；对不上（含没有指纹）就丢弃，返回是否丢了。
/// 实时改设置与进程启动（停用期间改过设置）共用这一处。
fn drop_password_if_backend_moved(secrets: &mut Secrets, backend: Option<&Target>) -> bool {
    if secrets.password.is_some() && secrets.backend_id != backend_fingerprint(backend) {
        secrets.password = None;
        secrets.backend_id = None;
        return true;
    }
    false
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
        let mut secrets = secrets;
        if drop_password_if_backend_moved(&mut secrets, cfg.settings.backend.as_ref()) {
            log!("存储后端已变更，已清除保存的存储凭据");
            if let Err(e) = secrets.save(&*cfg.secrets) {
                log!("{e}");
            }
        }
        let default_id = ids::default_device_id(&display_name(&cfg.settings), cfg.portable);
        let state = statefile::load(&cfg.state_dir, &default_id);
        let learn_last_ok = learn_library(&cfg.settings, &secrets)
            .map_or(0, |lib| learn_round::load_last_ok(&cfg.state_dir, &lib));
        let settings = cfg.settings.clone();
        let cached_usage = usage::load(&cfg.state_dir);
        // 打字统计在本进程没运行时被关掉
        if !settings.content.stats {
            learn_round::clear_peer_stats(&cfg.userdata_dir);
        }
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let skips = local_skips(&cfg.userdata_dir, &cfg.state_dir, exe_dir.as_deref());
        if !skips.is_empty() {
            log!("用户目录下的本机目录不同步: {skips:?}");
        }
        let watch = cfg.watch.then(|| {
            watch::start(&cfg.userdata_dir, skips.clone(), tx.clone())
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
            learn_last_ok,
            learn_failures: 0,
            learn_retry_at: 0,
            removed_with_data: Vec::new(),
            usage: cached_usage,
            usage_busy: false,
            usage_error: None,
            usage_retry_at: 0,
            skips,
            _watch: watch.flatten(),
        }
    }

    // ── 状态 ───────────────────────────────────────────────

    fn configured(&self) -> bool {
        self.settings.enabled && self.settings.backend.is_some()
    }

    /// 已持有同步密钥，且它在当前存储位置核对通过过；换了存储位置后要重新核对。
    fn joined(&self) -> bool {
        let id = library_id(self.settings.backend.as_ref());
        self.secrets.dek.is_some() && id.is_some() && self.secrets.dek_backend_id == id
    }

    /// 同步密钥在 `lib`（核对时的库位置）核对通过：仅当它仍是当前位置才记下。
    fn mark_verified(&mut self, lib: Option<String>) {
        if self.secrets.dek.is_some()
            && lib.is_some()
            && lib == library_id(self.settings.backend.as_ref())
            && self.secrets.dek_backend_id != lib
        {
            self.secrets.dek_backend_id = lib;
            if let Err(e) = self.save_secrets() {
                log!("{e}");
            }
        }
    }

    /// 远端没有库或密钥对不上：之前的核对结论作废。
    fn invalidate_verified(&mut self) {
        if self.secrets.dek_backend_id.take().is_some()
            && let Err(e) = self.save_secrets()
        {
            log!("{e}");
        }
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
            joined: self.joined(),
            device: self.state.device.clone(),
            group: group_of(&self.state).to_string(),
            group_name: self.state.group_name.clone(),
            needs_identity: self.needs_identity(),
            round: self.round_no,
            last_round: self.last_round.clone(),
            removed_with_data: self.removed_with_data.clone(),
            usage: self.current_usage().cloned(),
            usage_busy: self.usage_busy,
            usage_error: self.usage_error.clone(),
        }
    }

    /// 属于当前同步库的空间占用缓存。
    fn current_usage(&self) -> Option<&wind_sync_ipc::Usage> {
        let lib = learn_library(&self.settings, &self.secrets)?;
        self.usage
            .as_ref()
            .filter(|c| c.library == lib)
            .map(|c| &c.usage)
    }

    /// 本轮是否顺带统计 `lib` 的空间占用。
    fn usage_due(&self, lib: &str, now: u64) -> bool {
        usage::due(self.usage.as_ref(), lib, now, self.usage_retry_at)
    }

    /// 记下一次统计结果。失败只记原因、推迟自动重试，不动缓存与统计时间。
    fn apply_usage(&mut self, lib: String, r: Result<wind_sync_ipc::Usage, String>, now: u64) {
        match r {
            Ok(u) => {
                let c = usage::Cached {
                    library: lib,
                    usage: u,
                };
                if let Err(e) = usage::save(&self.cfg.state_dir, &c) {
                    log!("{e}");
                }
                self.usage = Some(c);
                self.usage_error = None;
                self.usage_retry_at = 0;
            }
            Err(e) => {
                log!("统计空间占用失败: {e}");
                self.usage_error = Some(e);
                self.usage_retry_at = now.saturating_add(usage::RETRY_SECS);
            }
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
        if self.syncing || self.usage_busy || !self.runnable() {
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
            Input::UsageDone(d) => {
                let (lib, r) = *d;
                self.usage_busy = false;
                self.apply_usage(lib, r, now_unix());
                self.report();
            }
            Input::Core(ToSync::Settings(s)) => self.on_settings(s),
            Input::Core(ToSync::Command { id, op, args }) => {
                let r = self.command(op, &args);
                // 先报状态再回执：core 收到回执时已见到命令带来的状态变化
                self.report();
                self.cfg.link.send(FromSync::command_reply(id, r));
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
        if drop_password_if_backend_moved(&mut self.secrets, s.backend.as_ref()) {
            log!("存储后端已变更，已清除保存的存储凭据");
            if let Err(e) = self.save_secrets() {
                log!("{e}");
            }
        }
        let moved = backend_changed(self.settings.backend.as_ref(), s.backend.as_ref());
        let old_library = learn_library(&self.settings, &self.secrets);
        let stats_off = self.settings.content.stats && !s.content.stats;
        self.settings = s;
        if moved {
            self.info_written = None;
        }
        if learn_library(&self.settings, &self.secrets) != old_library {
            self.reload_learn_clock();
        }
        // 关掉打字统计：他机统计不再更新，删掉免得统计页把陈旧数据当作当前合计；重新打开后下一轮重新下载
        if stats_off {
            learn_round::clear_peer_stats(&self.cfg.userdata_dir);
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

    /// 本轮是否该同步学习数据：强制，或距上次成功已满 30 分钟且不在失败退避期。
    fn learn_due(&self, now: u64, force: bool) -> bool {
        force || (now >= self.learn_last_ok + LEARN_INTERVAL_SECS && now >= self.learn_retry_at)
    }

    /// 学习数据轮的最终判定：三项内容任一开启，且到期。
    fn learn_wanted(&self, now: u64, force: bool) -> bool {
        self.settings.content.any() && self.learn_due(now, force)
    }

    /// 起工作线程跑一轮；结果经 `Input::RoundDone` 回主循环。
    fn start_round(&mut self) {
        let force = std::mem::take(&mut self.force_now);
        self.round_forced |= force;
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
        let now = now_unix();
        let mut ctx = SyncCtx::new(now, self.ns());
        ctx.force = force;
        ctx.gc = true;
        ctx.allow_mass_delete = allow_mass;
        let learn_due = self.learn_wanted(now, force);
        let usage_lib =
            learn_library(&self.settings, &self.secrets).filter(|lib| self.usage_due(lib, now));
        self.usage_busy = usage_lib.is_some();
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
        .with_cancel(self.cancel.clone())
        .with_skips(self.skips.clone());
        let library_id = library_id(self.settings.backend.as_ref());
        // 被退避跳过或停止态的轮次不跑学习数据（§4.5 第 1 步）
        let learn_lib = learn_library(&self.settings, &self.secrets).filter(|_| {
            learn_due && state.retry.halted.is_none() && (force || state.retry.due(now))
        });
        let (userdata, state_dir, link) = (
            self.cfg.userdata_dir.clone(),
            self.cfg.state_dir.clone(),
            self.cfg.link.clone(),
        );
        let content = self.settings.content;
        let cancel = self.cancel.clone();
        let tx = self.tx.clone();
        self.report();
        let spawned = std::thread::Builder::new()
            .name("sync-round".into())
            .spawn(move || {
                let mut info_written = None;
                let mut learn: Option<Result<(), String>> = None;
                let mut removed_with_data = None;
                let mut usage = None;
                let device = state.device.clone();
                let lctx = learn_lib.as_deref().map(|lib| {
                    learn_round::LearnCtx::new(
                        &userdata, &state_dir, &*link, &device, now, content, lib,
                    )
                });
                let result = match factory.build(&settings, &password) {
                    Err(m) => Err(m),
                    Ok(backend) => {
                        // 第 2 步在文件同步之前；私有状态缺失时先按 §4.7 初始化
                        let mut lstate = None;
                        if let Some(c) = &lctx {
                            match learn_round::ensure_state(c, &*backend, &dek.files_key())
                                .and_then(|mut st| {
                                    learn_round::count_in(c, &mut st).map(|skip| (st, skip))
                                }) {
                                Ok(st) => lstate = Some(st),
                                Err(e) => learn = Some(Err(e)),
                            }
                        }
                        let r = sync_once(&*backend, &mut local, &mut state, &dek, &ctx);
                        // 第 3、5 步在文件同步成功之后
                        if let (Some(c), Some((st, skip)), Ok(rep)) = (&lctx, lstate.as_mut(), &r)
                            && !rep.skipped
                            && !cancel.load(Ordering::SeqCst)
                        {
                            let mut devs = None;
                            learn = Some(
                                learn_round::exchange_and_materialize(
                                    c,
                                    &*backend,
                                    &dek.files_key(),
                                    st,
                                    skip,
                                )
                                .map(|rep| {
                                    for w in &rep.warnings {
                                        log!("学习数据: {w}");
                                    }
                                    devs = Some(rep.devices);
                                }),
                            );
                            if let Some(devs) = devs {
                                match list_devices(&*backend, &dek.devinfo_key()) {
                                    Ok(r) => {
                                        removed_with_data = Some(
                                            learn_round::removed_devices_in(&devs, &r)
                                                .into_iter()
                                                .filter(|d| *d != device)
                                                .collect(),
                                        )
                                    }
                                    Err(e) => log!("查询已移除设备失败: {}", sync_error(&e)),
                                }
                            }
                        }
                        if let Ok(rep) = &r
                            && !rep.skipped
                        {
                            retire_old_ids(&*backend, &dek, &mut state);
                        }
                        // 空间占用在本轮写完之后统计，不影响本轮成败
                        if let (Some(lib), Ok(rep)) = (usage_lib, &r)
                            && !rep.skipped
                            && !cancel.load(Ordering::SeqCst)
                        {
                            usage = Some((lib, usage::measure(&*backend, &dek)));
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
                    library_id,
                    learn,
                    removed_with_data,
                    usage,
                };
                let _ = tx.send(Input::RoundDone(Box::new(round)));
            });
        if let Err(e) = spawned {
            self.syncing = false;
            self.usage_busy = false;
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
            forced: std::mem::take(&mut self.round_forced),
        });
    }

    fn finish_round(&mut self, r: Round) {
        self.syncing = false;
        self.usage_busy = false;
        if let Some((lib, u)) = r.usage {
            self.apply_usage(lib, u, r.now);
        }
        self.state = r.state;
        if r.info_written.is_some() {
            self.info_written = r.info_written;
        }
        if std::mem::take(&mut self.clear_halt_after_round) {
            self.clear_halt();
        }
        self.conflicts = r.conflicts;
        if let Some(v) = r.removed_with_data {
            self.removed_with_data = v;
        }
        match &r.learn {
            Some(Ok(())) => {
                self.learn_last_ok = r.now;
                self.learn_failures = 0;
                self.learn_retry_at = 0;
            }
            Some(Err(e)) => {
                self.learn_failures = self.learn_failures.saturating_add(1);
                let wait = backoff_secs(self.learn_failures);
                self.learn_retry_at = r.now.saturating_add(wait);
                log!("学习数据同步失败（{wait} 秒后重试）: {e}");
            }
            None => {}
        }
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
                self.mark_verified(r.library_id.clone());
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
                if matches!(
                    e,
                    SyncError::Crypto(wind_sync::crypto::CryptoError::BadKey)
                        | SyncError::NotInitialized
                ) {
                    self.invalidate_verified();
                }
                // 稍后重试类不算一轮结束：票号等真正完成的那一轮
                if self.on_sync_error(e, r.failures_before, now) {
                    let msg = self.state.retry.last_error.clone();
                    self.end_round(false, None, msg);
                }
            }
        }
        self.save_state();
        self.publish_info(false);
        self.report();
    }

    /// 把本机信息写进远端名册。`daily` 为假时只在内容变了（改名等）才写；失败只记日志，下一轮会重试。
    fn publish_info(&mut self, daily: bool) {
        // 未确认身份时本机号可能是别人的，不写名册
        if self.syncing
            || !self.configured()
            || !self.ready()
            || !self.joined()
            || !self.state.identity_confirmed
        {
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

    /// 返回这一轮是否真正结束（稍后重试类返回假）。
    fn on_sync_error(&mut self, e: SyncError, failures_before: u32, now: Instant) -> bool {
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
            return false;
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
        true
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
                | Op::CreateKey
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
                | Op::PurgeDevice
                | Op::RefreshUsage
        );
        // 统计中再点刷新：不算冲突，回「进行中」
        let busy = self.syncing || (self.usage_busy && op != Op::RefreshUsage);
        if busy && mutating {
            return Err(BUSY_MSG.into());
        }
        match op {
            Op::ConfirmMassDelete => self.cmd_confirm_mass_delete(),
            Op::SetSecret => self.cmd_set_secret(args),
            Op::TestConnection => self.cmd_test_connection(args),
            Op::CreateKey => self.cmd_create_key(),
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
            Op::PurgeDevice => self.cmd_purge_device(args),
            Op::RefreshUsage => self.cmd_refresh_usage(),
        }
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
