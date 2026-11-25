//! 数据同步的 core 侧（设计 `docs/design/data-sync.md` §3、§6）。
//!
//! - 同步设置 `<local>/state/sync/settings.toml` 由本进程读写（不进 config.toml，不含任何机密）；
//!   `enabled` 决定是否拉起同步进程 `wind_input_sync`（与本 exe 同目录，带变体后缀）。
//!   exe 不存在 = 未安装，不拉起、不刷日志，状态报 `not_installed`。
//! - 拉起、退避、熔断、宽限停止复用 [`wind_supervise::Supervisor`]；同步进程只经继承的匿名管道
//!   与本进程说话（协议见 `wind-sync-ipc`），不连 ctrl / events 管道。
//! - 同步进程发来的 `Request` 只放行白名单方法，经注入的执行器（即 ctrl RPC 的分发器）执行。
//! - 设置端经 ctrl RPC `sync.*` 读写设置、查状态、发命令；命令同步等待同步进程的回执。
//!
//! 日志只记启停、崩溃与被拒的请求，不记命令参数（可能含口令）。

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracing::{error, info, warn};
use wind_config::Config;
use wind_supervise::backoff::Decision;
use wind_supervise::{Event, Job, Supervisor};
use wind_sync_ipc::{
    FromSync, Op, RoundStats, Status, SyncSettings, ToSync, read_frame, write_frame,
};

const TICK: Duration = Duration::from_millis(200);
const STOP_GRACE: Duration = Duration::from_secs(3);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// 检查「期望态是否该变」（exe 装上 / 被删）的间隔。
const RECONCILE_EVERY: Duration = Duration::from_secs(5);
const LOG_MAX: u64 = 1024 * 1024;
/// 主动同步的票等这么久还没结果就放弃。
const TICKET_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// 同步结果提示（由接线处弹 toast）。
#[derive(Debug, Clone, PartialEq)]
pub enum SyncNotice {
    Done(RoundStats),
    Failed(String),
    Busy,
    Disabled,
    BackgroundFailed(String),
}

#[allow(dead_code)] // 接线在主循环里另做
pub fn notice_text(n: &SyncNotice) -> String {
    match n {
        SyncNotice::Done(s) => {
            let mut parts = Vec::new();
            if s.downloaded > 0 {
                parts.push(format!("收到 {} 个文件", s.downloaded));
            }
            if s.uploaded > 0 {
                parts.push(format!("上传 {} 个", s.uploaded));
            }
            let mut t = "同步完成".to_string();
            if !parts.is_empty() {
                t.push('：');
                t.push_str(&parts.join("、"));
            }
            if s.conflicts > 0 {
                t.push_str(&format!("，{} 个冲突", s.conflicts));
            }
            t
        }
        SyncNotice::Failed(r) => format!("同步失败：{r}"),
        SyncNotice::Busy => "正在同步，完成后会提示".into(),
        SyncNotice::Disabled => "数据同步未启用".into(),
        SyncNotice::BackgroundFailed(r) => format!("数据同步出错：{r}"),
    }
}

/// 一次主动同步在等什么。
enum Active {
    /// 同步进程还没连上，连上后发 `SyncNow`。
    Pending { since: Instant },
    /// 已拿到票号，等 `round >= ticket`。
    Ticket { ticket: u64, since: Instant },
}

/// 提示判定的状态。
#[derive(Default)]
struct Track {
    ticket: Option<Active>,
    /// 上一次判定时 phase 是否为 `error`：只在「进入」时提示后台失败。
    was_error: bool,
}

/// 同步进程能经 core 执行的方法。其余一律拒绝：同步进程持有凭据，被攻破时不能借 core 做别的事。
pub const REQUEST_WHITELIST: &[&str] = &[
    "data.beginEdit",
    "data.endEdit",
    // 只读：写入前确认用户自己没开着编辑会话
    "data.editSessions",
    "dict.reload",
    "config.reload",
    "compat.reload",
    "schema.reloadFiles",
    "theme.reloadFiles",
    "softkeyboard.reload",
    // 替换运行中的扩展文件前后：暂停 / 恢复这些扩展
    "extensions.suspend",
    "extensions.resume",
];

/// 同步进程文件名随变体带后缀（`wind_input_sync_dev.exe`），理由同扩展宿主：
/// 两个变体同名的话，部署脚本按进程名停旧进程会把另一个变体的也停掉。
fn sync_exe_name() -> String {
    format!(
        "wind_input_sync{}{}",
        wind_config::variant::pipe_suffix(),
        std::env::consts::EXE_SUFFIX
    )
}

fn sync_exe_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .map(|p| p.with_file_name(sync_exe_name()))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 读设置文件。缺失用默认值；损坏记日志后用默认值（不覆盖原文件，留给用户处理）。
fn load_settings(path: &Path) -> SyncSettings {
    wind_sync_ipc::load_settings_file(path).unwrap_or_else(|e| {
        warn!("{e}，按默认值处理");
        SyncSettings::default()
    })
}

fn save_settings(path: &Path, s: &SyncSettings) -> Result<(), String> {
    wind_sync_ipc::save_settings_file(path, s)
}

struct Paths {
    settings_file: PathBuf,
    state_dir: PathBuf,
    userdata_dir: PathBuf,
    suffix: String,
}

impl Paths {
    fn production() -> Option<Paths> {
        let state_dir = Config::state_dir()?.join("sync");
        Some(Paths {
            settings_file: state_dir.join("settings.toml"),
            state_dir,
            userdata_dir: Config::user_config_dir()?,
            suffix: wind_config::variant::pipe_suffix().to_string(),
        })
    }
}

type Emit = Box<dyn Fn(Value) + Send + Sync>;
type Exec = Box<dyn Fn(&str, &Value) -> Result<Value, String> + Send + Sync>;
type Notify = Box<dyn Fn(SyncNotice) + Send + Sync>;
type EnabledHook = Box<dyn Fn(bool) + Send + Sync>;
type Clock = Box<dyn Fn() -> Instant + Send + Sync>;

struct Hooks {
    /// 广播 `sync.statusChanged`。
    emit: Emit,
    /// 执行同步进程请求的方法（ctrl RPC 分发器）。
    exec: Exec,
}

struct Link {
    tx: Sender<ToSync>,
    /// 区分前后两次拉起：旧进程的读线程迟到的消息不得写进新进程的状态。
    epoch: u64,
}

struct Inner {
    sup: Supervisor,
    settings: SyncSettings,
    link: Option<Link>,
    status: Status,
    epoch: u64,
    last_reconcile: Option<Instant>,
    track: Track,
}

pub struct SyncHost {
    paths: Option<Paths>,
    installed: Box<dyn Fn() -> bool + Send + Sync>,
    inner: Mutex<Inner>,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    hooks: OnceLock<Hooks>,
    notify: OnceLock<Notify>,
    enabled_hook: OnceLock<EnabledHook>,
    /// 测试注入的时钟；生产用 `Instant::now`。
    clock: OnceLock<Clock>,
    command_timeout: Duration,
    /// 经同步进程成功 `data.beginEdit`、尚未 `data.endEdit` 的 (进程代次, 路径)。
    /// 同步进程退出（含被强杀）时据此代为结束，否则 core 会一直暂停该文件的写回。
    held_edits: Mutex<BTreeSet<(u64, String)>>,
    /// 经同步进程成功 `extensions.suspend`、尚未 `extensions.resume` 的 (进程代次, 扩展 id)；
    /// 同步进程退出时代为恢复，否则扩展会一直停着。
    held_suspends: Mutex<BTreeSet<(u64, String)>>,
}

impl SyncHost {
    /// 生产构造：读设置、起监护线程。设置目录取不到时整体惰性（不启用，设置端写入会报错）。
    pub fn start() -> Arc<SyncHost> {
        let paths = Paths::production();
        let job = Job::kill_on_close()
            .map_err(|e| warn!("同步进程的 Job 对象创建失败: {e}"))
            .ok()
            .map(Arc::new);
        let sup = Supervisor::new(sync_command)
            .with_log(Config::log_dir().map(|d| d.join("sync.log")), LOG_MAX)
            .with_job(job);
        let host = Arc::new(SyncHost::new(
            paths,
            sup,
            Box::new(|| sync_exe_path().is_some_and(|p| p.is_file())),
            COMMAND_TIMEOUT,
        ));
        {
            let host = host.clone();
            let _ = std::thread::Builder::new()
                .name("sync-supervise".into())
                .spawn(move || {
                    loop {
                        host.tick(Instant::now());
                        std::thread::sleep(TICK);
                    }
                });
        }
        host
    }

    fn new(
        paths: Option<Paths>,
        sup: Supervisor,
        installed: Box<dyn Fn() -> bool + Send + Sync>,
        command_timeout: Duration,
    ) -> SyncHost {
        let settings = paths
            .as_ref()
            .map(|p| load_settings(&p.settings_file))
            .unwrap_or_default();
        let host = SyncHost {
            paths,
            installed,
            inner: Mutex::new(Inner {
                sup,
                settings,
                link: None,
                status: Status::default(),
                epoch: 0,
                last_reconcile: None,
                track: Track::default(),
            }),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            hooks: OnceLock::new(),
            notify: OnceLock::new(),
            enabled_hook: OnceLock::new(),
            clock: OnceLock::new(),
            command_timeout,
            held_edits: Mutex::new(BTreeSet::new()),
            held_suspends: Mutex::new(BTreeSet::new()),
        };
        host.reconcile(&mut lock(&host.inner), Instant::now(), false);
        host
    }

    /// 接上事件广播与请求执行器（RPC 服务建好之后才有，故晚于构造）。
    pub fn set_hooks(&self, emit: Emit, exec: Exec) {
        let _ = self.hooks.set(Hooks { emit, exec });
    }

    /// 接上同步结果提示（弹 toast 的接线处）。
    #[allow(dead_code)]
    pub fn set_notify(&self, f: Notify) {
        let _ = self.notify.set(f);
    }

    /// 接上启用状态回调：设置时立即以当前值调一次，之后每次变化再调。
    #[allow(dead_code)]
    pub fn set_enabled_hook(&self, f: EnabledHook) {
        let enabled = lock(&self.inner).settings.enabled;
        if self.enabled_hook.set(f).is_ok() {
            if let Some(h) = self.enabled_hook.get() {
                h(enabled);
            }
        }
    }

    #[cfg(test)]
    fn set_clock(&self, f: Clock) {
        let _ = self.clock.set(f);
    }

    fn now(&self) -> Instant {
        self.clock.get().map_or_else(Instant::now, |c| c())
    }

    /// 服务即将重启或退出：通知同步进程收尾，等不到就强杀。
    pub fn stop(&self) {
        let waiter = {
            let mut inner = lock(&self.inner);
            if let Some(link) = inner.link.take() {
                let _ = link.tx.send(ToSync::Shutdown);
            }
            inner.sup.stop_background(STOP_GRACE)
        };
        self.fail_pending("同步进程已停止");
        if let Some(w) = waiter {
            let _ = w.join();
        }
        self.release_held(None);
    }

    /// 记录 / 移除经同步链路暂停的扩展。
    fn track_suspend(&self, epoch: u64, method: &str, params: &Value, ok: bool) {
        if !ok {
            return;
        }
        let Some(ids) = params.get("ids").and_then(Value::as_array) else {
            return;
        };
        let mut held = lock(&self.held_suspends);
        for id in ids.iter().filter_map(Value::as_str) {
            match method {
                "extensions.suspend" => {
                    held.insert((epoch, id.to_string()));
                }
                "extensions.resume" => {
                    held.remove(&(epoch, id.to_string()));
                }
                _ => {}
            }
        }
    }

    /// 记录 / 移除经同步链路开的编辑会话。`ok` 为请求是否成功执行。
    fn track_edit(&self, epoch: u64, method: &str, params: &Value, ok: bool) {
        if !ok {
            return;
        }
        let Some(path) = params.get("path").and_then(Value::as_str) else {
            return;
        };
        let mut held = lock(&self.held_edits);
        match method {
            "data.beginEdit" => {
                held.insert((epoch, path.to_string()));
            }
            "data.endEdit" => {
                held.remove(&(epoch, path.to_string()));
            }
            _ => {}
        }
    }

    /// 代为结束残留的编辑会话：`epoch` 为 None 时全部。不持有 inner 锁调用。
    fn release_held(&self, epoch: Option<u64>) {
        let paths: Vec<String> = {
            let mut held = lock(&self.held_edits);
            let take: Vec<(u64, String)> = held
                .iter()
                .filter(|(e, _)| epoch.is_none_or(|x| x == *e))
                .cloned()
                .collect();
            for k in &take {
                held.remove(k);
            }
            take.into_iter().map(|(_, p)| p).collect()
        };
        for p in paths {
            warn!("同步进程退出时仍有编辑会话未结束，代为结束: {p}");
            if let Err(e) = self.run_request("data.endEdit", &json!({ "path": p })) {
                warn!("代为结束编辑会话 {p} 失败: {e}");
            }
        }
        let ids: Vec<String> = {
            let mut held = lock(&self.held_suspends);
            let take: Vec<(u64, String)> = held
                .iter()
                .filter(|(e, _)| epoch.is_none_or(|x| x == *e))
                .cloned()
                .collect();
            for k in &take {
                held.remove(k);
            }
            let set: BTreeSet<String> = take.into_iter().map(|(_, id)| id).collect();
            set.into_iter().collect()
        };
        if !ids.is_empty() {
            warn!("同步进程退出时仍有扩展被暂停，代为恢复: {}", ids.join("、"));
            if let Err(e) = self.run_request("extensions.resume", &json!({ "ids": ids })) {
                warn!("代为恢复扩展失败: {e}");
            }
        }
    }

    // ── 期望态 ─────────────────────────────────────────────

    /// 期望态 = 已启用且同步进程已安装。`manual` 表示用户刚改了设置：视为手动干预，解除熔断。
    fn reconcile(&self, inner: &mut Inner, now: Instant, manual: bool) {
        inner.last_reconcile = Some(now);
        if inner.settings.enabled && (self.installed)() {
            inner.sup.set_wanted(true, now);
            if manual && inner.link.is_none() {
                inner.sup.retry(now);
            }
        } else if inner.sup.wanted() || inner.link.is_some() {
            if let Some(link) = inner.link.take() {
                let _ = link.tx.send(ToSync::Shutdown);
            }
            // 宽限后仍未退出就强杀；后台做，不卡调用方。
            inner.sup.stop_background(STOP_GRACE);
            self.fail_pending("同步已停用");
        }
    }

    // ── 监护节拍 ───────────────────────────────────────────

    fn tick(self: &Arc<Self>, now: Instant) {
        let changed = {
            let mut inner = lock(&self.inner);
            if inner
                .last_reconcile
                .is_none_or(|t| now.saturating_duration_since(t) >= RECONCILE_EVERY)
            {
                self.reconcile(&mut inner, now, false);
            }
            match inner.sup.tick(now) {
                Some(Event::Started {
                    stdin,
                    stdout,
                    warnings,
                }) => {
                    for w in warnings {
                        warn!("同步进程: {w}");
                    }
                    info!("同步进程已启动");
                    self.attach(&mut inner, stdin, stdout);
                    true
                }
                Some(Event::Exited { status, next }) => {
                    warn!("同步进程退出: {status}");
                    self.on_exit(&mut inner, next)
                }
                Some(Event::StartFailed { error, next }) => {
                    error!("同步进程启动失败: {error}");
                    self.on_exit(&mut inner, next)
                }
                Some(Event::PollFailed(e)) => {
                    warn!("查询同步进程状态失败: {e}");
                    false
                }
                None => false,
            }
        };
        if changed {
            self.emit_status();
        } else {
            self.pump();
        }
    }

    fn on_exit(&self, inner: &mut Inner, next: Decision) -> bool {
        inner.link = None;
        inner.status.phase.clear();
        self.fail_pending("同步进程已退出");
        if matches!(next, Decision::Tripped) {
            error!("同步进程频繁退出，已停止重启；修改同步设置后会重新尝试");
        }
        true
    }

    /// 同步进程已拉起：先排好 Hello，再起写、读线程。
    fn attach(
        self: &Arc<Self>,
        inner: &mut Inner,
        stdin: impl Write + Send + 'static,
        stdout: impl Read + Send + 'static,
    ) {
        inner.epoch += 1;
        let epoch = inner.epoch;
        let (tx, rx) = mpsc::channel::<ToSync>();
        if let Some(p) = &self.paths {
            let _ = tx.send(ToSync::Hello {
                suffix: p.suffix.clone(),
                userdata_dir: p.userdata_dir.to_string_lossy().into_owned(),
                state_dir: p.state_dir.to_string_lossy().into_owned(),
                settings: inner.settings.clone(),
                portable: wind_config::variant::is_portable(),
            });
        }
        inner.link = Some(Link { tx, epoch });
        inner.status = Status::default();
        let mut stdin = stdin;
        let _ = std::thread::Builder::new()
            .name("sync-write".into())
            .spawn(move || {
                // 发送端都丢了（停用 / 退出）即收尾；stdin 随之关闭，同步进程读到 EOF 退出。
                while let Ok(m) = rx.recv() {
                    if write_frame(&mut stdin, &m).is_err() {
                        break;
                    }
                }
            });
        if matches!(inner.track.ticket, Some(Active::Pending { .. })) {
            let me = self.clone();
            let _ = std::thread::Builder::new()
                .name("sync-now".into())
                .spawn(move || me.send_sync_now(true));
        }
        let me = self.clone();
        let mut stdout = stdout;
        let _ = std::thread::Builder::new()
            .name("sync-read".into())
            .spawn(move || {
                while let Ok(Some(msg)) = read_frame::<_, FromSync>(&mut stdout) {
                    me.on_message(epoch, msg);
                }
                me.on_link_closed(epoch);
            });
    }

    // ── 同步进程 → core ────────────────────────────────────

    fn on_message(self: &Arc<Self>, epoch: u64, msg: FromSync) {
        match msg {
            FromSync::Status(s) => {
                let changed = {
                    let mut inner = lock(&self.inner);
                    if inner.link.as_ref().map(|l| l.epoch) != Some(epoch) || inner.status == s {
                        false
                    } else {
                        inner.status = s;
                        true
                    }
                };
                if changed {
                    self.emit_status();
                }
            }
            FromSync::Request { id, method, params } => {
                let result = self.run_request(&method, &params);
                self.track_edit(epoch, &method, &params, result.is_ok());
                self.track_suspend(epoch, &method, &params, result.is_ok());
                let inner = lock(&self.inner);
                if let Some(l) = inner.link.as_ref().filter(|l| l.epoch == epoch) {
                    let _ = l.tx.send(ToSync::reply(id, result));
                }
            }
            FromSync::CommandReply { id, result, error } => {
                let r = match error {
                    Some(e) => Err(e),
                    None => Ok(result.unwrap_or(Value::Null)),
                };
                if let Some(tx) = lock(&self.pending).remove(&id) {
                    let _ = tx.send(r);
                }
            }
        }
    }

    fn run_request(&self, method: &str, params: &Value) -> Result<Value, String> {
        if !REQUEST_WHITELIST.contains(&method) {
            warn!("拒绝同步进程的请求（不在白名单）: {method}");
            return Err(format!("方法不在白名单: {method}"));
        }
        match self.hooks.get() {
            Some(h) => (h.exec)(method, params),
            None => Err("core 尚未就绪".into()),
        }
    }

    fn on_link_closed(&self, epoch: u64) {
        let was_current = {
            let mut inner = lock(&self.inner);
            if inner.link.as_ref().map(|l| l.epoch) == Some(epoch) {
                inner.link = None;
                inner.status.phase.clear();
                true
            } else {
                false
            }
        };
        self.release_held(Some(epoch));
        if was_current {
            self.fail_pending("同步进程已退出");
            self.emit_status();
        }
    }

    fn fail_pending(&self, why: &str) {
        for (_, tx) in lock(&self.pending).drain() {
            let _ = tx.send(Err(why.to_string()));
        }
    }

    // ── 状态 ───────────────────────────────────────────────

    fn phase_of(&self, inner: &Inner) -> (String, Option<String>) {
        if !(self.installed)() {
            ("not_installed".to_string(), None)
        } else if !inner.settings.enabled {
            ("disabled".to_string(), None)
        } else if inner.link.is_some() {
            let p = if inner.status.phase.is_empty() {
                "starting".to_string()
            } else {
                inner.status.phase.clone()
            };
            (p, inner.status.last_error.clone())
        } else if inner.sup.is_tripped() {
            (
                "error".to_string(),
                Some("同步进程频繁退出，已停止重启".to_string()),
            )
        } else {
            ("starting".to_string(), inner.status.last_error.clone())
        }
    }

    fn view(&self, inner: &Inner) -> Value {
        let installed = (self.installed)();
        let running = inner.link.is_some();
        let (phase, last_error) = self.phase_of(inner);
        json!({
            "installed": installed,
            "enabled": inner.settings.enabled,
            "running": running,
            "phase": phase,
            "last_ok": inner.status.last_ok,
            "last_error": last_error,
            "pending": inner.status.pending,
            "conflicts": inner.status.conflicts,
            "conflict_paths": inner.status.conflict_paths,
            "has_secret": inner.status.has_secret,
            "joined": inner.status.joined,
            "device": inner.status.device,
            "group": inner.status.group,
            "group_name": inner.status.group_name,
            "needs_identity": inner.status.needs_identity,
        })
    }

    fn emit_status(&self) {
        let v = self.view(&lock(&self.inner));
        if let Some(h) = self.hooks.get() {
            (h.emit)(v);
        }
        self.pump();
    }

    // ── 同步结果提示 ───────────────────────────────────────

    /// 按当前状态判定该弹哪些提示；回调放锁外。
    fn pump(&self) {
        let notices = self.evaluate(&mut lock(&self.inner));
        if let Some(f) = self.notify.get() {
            for n in notices {
                f(n);
            }
        }
    }

    fn evaluate(&self, inner: &mut Inner) -> Vec<SyncNotice> {
        let mut out = Vec::new();
        let (phase, last_error) = self.phase_of(inner);
        let linked = inner.link.is_some();
        let reason = || {
            last_error
                .clone()
                .or_else(|| {
                    inner
                        .status
                        .last_round
                        .as_ref()
                        .and_then(|r| r.error.clone())
                })
                .unwrap_or_else(|| phase.clone())
        };
        match &inner.track.ticket {
            Some(Active::Ticket { ticket, since }) => {
                if linked && inner.status.round >= *ticket {
                    out.push(match &inner.status.last_round {
                        Some(r) if !r.ok => SyncNotice::Failed(
                            r.error.clone().unwrap_or_else(|| "未知错误".to_string()),
                        ),
                        Some(r) => SyncNotice::Done(r.clone()),
                        None => SyncNotice::Done(RoundStats {
                            ok: true,
                            ..Default::default()
                        }),
                    });
                    inner.track.ticket = None;
                } else if self.gave_up(inner, &phase, *since) {
                    out.push(SyncNotice::Failed(reason()));
                    inner.track.ticket = None;
                } else if !linked {
                    out.push(SyncNotice::Failed("同步进程已退出".into()));
                    inner.track.ticket = None;
                }
            }
            Some(Active::Pending { since }) => {
                if self.gave_up(inner, &phase, *since) || (!linked && phase == "error") {
                    out.push(SyncNotice::Failed(reason()));
                    inner.track.ticket = None;
                }
            }
            None => {}
        }
        // 有主动同步在等时，进入失败归它管，不当后台失败。
        let entering = phase == "error" && !inner.track.was_error;
        if entering && inner.track.ticket.is_none() && out.is_empty() {
            out.push(SyncNotice::BackgroundFailed(reason()));
        }
        inner.track.was_error = phase == "error";
        out
    }

    /// 等票期间同步已不可能出结果：停用 / 未安装 / 停摆 / 待确认身份 / 超时。
    fn gave_up(&self, inner: &Inner, phase: &str, since: Instant) -> bool {
        matches!(phase, "disabled" | "not_installed" | "halted")
            || inner.status.needs_identity
            || self.now().saturating_duration_since(since) >= TICKET_TIMEOUT
    }

    fn notify_now(&self, n: SyncNotice) {
        if let Some(f) = self.notify.get() {
            f(n);
        }
    }

    /// 向同步进程发 `SyncNow` 并登记票号；`from_pending` 表示这是连上后补发的那张。
    fn send_sync_now(&self, from_pending: bool) -> Value {
        let since = self.now();
        match self.command(Op::SyncNow, Value::Null) {
            Ok(v) => {
                let ticket = v.get("ticket").and_then(Value::as_u64).unwrap_or(0);
                let busy = v.get("busy").and_then(Value::as_bool).unwrap_or(false);
                if busy {
                    self.notify_now(SyncNotice::Busy);
                }
                lock(&self.inner).track.ticket = Some(Active::Ticket { ticket, since });
                // 回执可能比登记还晚到：登记后立即按当前状态判一次。
                self.pump();
                json!({ "outcome": if busy { "busy" } else { "queued" }, "ticket": ticket })
            }
            Err(e) => {
                if from_pending {
                    lock(&self.inner).track.ticket = None;
                }
                self.notify_now(SyncNotice::Failed(e.clone()));
                json!({ "outcome": "failed", "error": e })
            }
        }
    }

    /// 用户主动「立即同步」。未启用不拉起进程；进程还在启动则连上后补发。
    pub fn sync_now_active(&self) -> Value {
        {
            let mut inner = lock(&self.inner);
            if !inner.settings.enabled {
                drop(inner);
                self.notify_now(SyncNotice::Disabled);
                return json!({ "outcome": "disabled" });
            }
            if !(self.installed)() {
                drop(inner);
                let e = "同步组件未安装".to_string();
                self.notify_now(SyncNotice::Failed(e.clone()));
                return json!({ "outcome": "failed", "error": e });
            }
            if inner.link.is_none() {
                inner.track.ticket = Some(Active::Pending { since: self.now() });
                return json!({ "outcome": "starting" });
            }
        }
        self.send_sync_now(false)
    }

    // ── ctrl RPC ───────────────────────────────────────────

    /// `sync.*` 的入口。绝不返回任何机密。
    pub fn rpc(&self, method: &str, params: &Value) -> anyhow::Result<Value> {
        match method {
            "sync.getSettings" => {
                let inner = lock(&self.inner);
                Ok(json!({
                    "settings": inner.settings,
                    "installed": (self.installed)(),
                    "has_secret": inner.status.has_secret,
                }))
            }
            "sync.setSettings" => {
                let raw = params.get("settings").unwrap_or(params);
                let s: SyncSettings = serde_json::from_value(raw.clone())
                    .map_err(|e| anyhow::anyhow!("同步设置格式不对: {e}"))?;
                self.set_settings(s).map_err(anyhow::Error::msg)?;
                Ok(json!({ "ok": true }))
            }
            "sync.status" => Ok(self.view(&lock(&self.inner))),
            "sync.now" => Ok(self.sync_now_active()),
            "sync.command" => {
                let op = params
                    .get("op")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("缺少参数 op"))?;
                let op = Op::parse(op).ok_or_else(|| anyhow::anyhow!("未知的同步命令: {op}"))?;
                let args = params.get("args").cloned().unwrap_or(Value::Null);
                self.command(op, args).map_err(anyhow::Error::msg)
            }
            _ => anyhow::bail!("unknown method: {method}"),
        }
    }

    fn set_settings(&self, s: SyncSettings) -> Result<(), String> {
        s.validate()?;
        let paths = self.paths.as_ref().ok_or("取不到同步状态目录")?;
        save_settings(&paths.settings_file, &s)?;
        let now_enabled = s.enabled;
        let was_enabled = {
            let mut inner = lock(&self.inner);
            let was = inner.settings.enabled;
            inner.settings = s.clone();
            if let Some(l) = &inner.link {
                let _ = l.tx.send(ToSync::Settings(s));
            }
            self.reconcile(&mut inner, Instant::now(), true);
            was
        };
        if was_enabled != now_enabled {
            if let Some(h) = self.enabled_hook.get() {
                h(now_enabled);
            }
        }
        self.emit_status();
        Ok(())
    }

    /// 转发命令并同步等回执。不记 `args`（可能含口令）。
    fn command(&self, op: Op, args: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        // 先登记再发送：回执可能比发送调用返回得还早。
        lock(&self.pending).insert(id, tx);
        let sent = {
            let inner = lock(&self.inner);
            match &inner.link {
                Some(l) => l.tx.send(ToSync::Command { id, op, args }).is_ok(),
                None => false,
            }
        };
        if !sent {
            lock(&self.pending).remove(&id);
            return Err("同步进程未运行".into());
        }
        match rx.recv_timeout(self.command_timeout) {
            Ok(r) => r,
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err("等待同步进程回应超时".into())
            }
        }
    }
}

/// 拉起同步进程的命令（stdio、日志、Job 由监护器配）。
fn sync_command() -> Result<Command, String> {
    let exe = sync_exe_path().ok_or("取本进程路径失败")?;
    if !exe.is_file() {
        return Err(format!("找不到 {}", exe.display()));
    }
    let mut cmd = Command::new(exe);
    wind_supervise::die_with_parent_on(&mut cmd);
    Ok(cmd)
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
