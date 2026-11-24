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

use std::collections::HashMap;
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
use wind_sync_ipc::{FromSync, Op, Status, SyncSettings, ToSync, read_frame, write_frame};

const TICK: Duration = Duration::from_millis(200);
const STOP_GRACE: Duration = Duration::from_secs(3);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// 检查「期望态是否该变」（exe 装上 / 被删）的间隔。
const RECONCILE_EVERY: Duration = Duration::from_secs(5);
const LOG_MAX: u64 = 1024 * 1024;

/// 同步进程能经 core 执行的方法。其余一律拒绝：同步进程持有凭据，被攻破时不能借 core 做别的事。
pub const REQUEST_WHITELIST: &[&str] = &[
    "data.beginEdit",
    "data.endEdit",
    "dict.reload",
    "config.reload",
    "compat.reload",
    "schema.reloadFiles",
    "theme.reloadFiles",
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
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SyncSettings::default(),
        Err(e) => {
            warn!("读取同步设置 {} 失败: {e}", path.display());
            return SyncSettings::default();
        }
    };
    match toml::from_str::<SyncSettings>(&text) {
        Ok(s) => s,
        Err(e) => {
            warn!("同步设置 {} 解析失败，按默认值处理: {e}", path.display());
            SyncSettings::default()
        }
    }
}

/// 同目录临时文件 + rename，写一半不会留下半个设置文件。
fn save_settings(path: &Path, s: &SyncSettings) -> Result<(), String> {
    let text = toml::to_string_pretty(s).map_err(|e| format!("序列化同步设置失败: {e}"))?;
    let dir = path.parent().ok_or("设置路径无父目录")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("替换 {} 失败: {e}", path.display()))
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
}

pub struct SyncHost {
    paths: Option<Paths>,
    installed: Box<dyn Fn() -> bool + Send + Sync>,
    inner: Mutex<Inner>,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    hooks: OnceLock<Hooks>,
    command_timeout: Duration,
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
            }),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            hooks: OnceLock::new(),
            command_timeout,
        };
        host.reconcile(&mut lock(&host.inner), Instant::now(), false);
        host
    }

    /// 接上事件广播与请求执行器（RPC 服务建好之后才有，故晚于构造）。
    pub fn set_hooks(&self, emit: Emit, exec: Exec) {
        let _ = self.hooks.set(Hooks { emit, exec });
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

    fn view(&self, inner: &Inner) -> Value {
        let installed = (self.installed)();
        let running = inner.link.is_some();
        let (phase, last_error) = if !installed {
            ("not_installed".to_string(), None)
        } else if !inner.settings.enabled {
            ("disabled".to_string(), None)
        } else if running {
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
        };
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
        })
    }

    fn emit_status(&self) {
        let v = self.view(&lock(&self.inner));
        if let Some(h) = self.hooks.get() {
            (h.emit)(v);
        }
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
        {
            let mut inner = lock(&self.inner);
            inner.settings = s.clone();
            if let Some(l) = &inner.link {
                let _ = l.tx.send(ToSync::Settings(s));
            }
            self.reconcile(&mut inner, Instant::now(), true);
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
mod tests {
    use super::*;
    use std::io::{PipeReader, PipeWriter, pipe};
    use wind_sync_ipc::Backend;

    /// 桩同步进程：在测试里扮演管道另一头（不起真进程——测试二进制的 stdout 会混进
    /// libtest 的头部输出，污染帧流）。
    struct Stub {
        from_core: mpsc::Receiver<ToSync>,
        to_core: PipeWriter,
    }

    impl Stub {
        fn next(&self) -> ToSync {
            self.from_core
                .recv_timeout(Duration::from_secs(5))
                .expect("等不到 core 发来的帧")
        }
        fn send(&mut self, m: FromSync) {
            write_frame(&mut self.to_core, &m).unwrap();
        }
    }

    struct Rig {
        host: Arc<SyncHost>,
        stub: Stub,
        events: Arc<Mutex<Vec<Value>>>,
        executed: Arc<Mutex<Vec<String>>>,
        dir: tempfile::TempDir,
    }

    fn webdav(enabled: bool) -> SyncSettings {
        SyncSettings {
            enabled,
            backend: Some(Backend::Webdav {
                url: "https://dav.example.com/x".into(),
                user: "me".into(),
            }),
            ..Default::default()
        }
    }

    fn host_in(dir: &Path, installed: bool, timeout: Duration) -> Arc<SyncHost> {
        let paths = Paths {
            settings_file: dir.join("state/sync/settings.toml"),
            state_dir: dir.join("state/sync"),
            userdata_dir: dir.join("user"),
            suffix: "_t".into(),
        };
        let sup = Supervisor::new(|| Err("测试不拉起真进程".into()));
        Arc::new(SyncHost::new(
            Some(paths),
            sup,
            Box::new(move || installed),
            timeout,
        ))
    }

    /// 造一个已接上桩的 host；`pre` 在接上前写入设置文件。
    fn rig_with(pre: Option<SyncSettings>, timeout: Duration) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        if let Some(s) = pre {
            save_settings(&dir.path().join("state/sync/settings.toml"), &s).unwrap();
        }
        let host = host_in(dir.path(), true, timeout);
        let events = Arc::new(Mutex::new(Vec::new()));
        let executed = Arc::new(Mutex::new(Vec::new()));
        host.set_hooks(
            {
                let events = events.clone();
                Box::new(move |v| lock(&events).push(v))
            },
            {
                let executed = executed.clone();
                Box::new(move |m, _| {
                    lock(&executed).push(m.to_string());
                    Ok(json!({ "ran": m }))
                })
            },
        );
        let (child_stdin_r, child_stdin_w): (PipeReader, PipeWriter) = pipe().unwrap();
        let (child_stdout_r, child_stdout_w) = pipe().unwrap();
        let (tx, from_core) = mpsc::channel();
        std::thread::spawn(move || {
            let mut r = child_stdin_r;
            while let Ok(Some(m)) = read_frame::<_, ToSync>(&mut r) {
                if tx.send(m).is_err() {
                    break;
                }
            }
        });
        host.attach(&mut lock(&host.inner), child_stdin_w, child_stdout_r);
        Rig {
            host,
            stub: Stub {
                from_core,
                to_core: child_stdout_w,
            },
            events,
            executed,
            dir,
        }
    }

    fn rig() -> Rig {
        rig_with(Some(webdav(true)), COMMAND_TIMEOUT)
    }

    fn wait_for(mut cond: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < end, "等待条件超时");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn phase(h: &SyncHost) -> String {
        h.rpc("sync.status", &Value::Null).unwrap()["phase"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn hello_comes_first_with_paths_and_settings() {
        let r = rig();
        match r.stub.next() {
            ToSync::Hello {
                suffix,
                userdata_dir,
                state_dir,
                settings,
            } => {
                assert_eq!(suffix, "_t");
                assert!(userdata_dir.ends_with("user"));
                assert!(state_dir.ends_with("sync"));
                assert_eq!(settings, webdav(true));
            }
            m => panic!("第一帧应是 Hello: {m:?}"),
        }
    }

    #[test]
    fn status_is_cached_and_broadcast_without_secrets() {
        let mut r = rig();
        r.stub.send(FromSync::Status(Status {
            phase: "idle".into(),
            last_ok: Some(100),
            pending: 2,
            conflicts: 1,
            conflict_paths: vec!["config.conflict-a-1.toml".into()],
            has_secret: true,
            joined: true,
            ..Default::default()
        }));
        wait_for(|| phase(&r.host) == "idle");
        let st = r.host.rpc("sync.status", &Value::Null).unwrap();
        assert_eq!(st["pending"], 2);
        assert_eq!(st["conflicts"], 1);
        assert_eq!(st["joined"], true);
        assert_eq!(st["conflict_paths"][0], "config.conflict-a-1.toml");
        assert_eq!(st["last_ok"], 100);
        assert_eq!(st["running"], true);
        let ev = lock(&r.events).clone();
        assert_eq!(ev.last().unwrap()["phase"], "idle", "状态变化应广播");

        let got = r.host.rpc("sync.getSettings", &Value::Null).unwrap();
        assert_eq!(got["has_secret"], true);
        assert_eq!(got["installed"], true);
        let text = got.to_string();
        assert!(!text.contains("password") && !text.contains("secret_key"));

        // 同样的状态再发一次不重复广播。
        let n = lock(&r.events).len();
        r.stub.send(FromSync::Status(Status {
            phase: "idle".into(),
            last_ok: Some(100),
            pending: 2,
            conflicts: 1,
            conflict_paths: vec!["config.conflict-a-1.toml".into()],
            has_secret: true,
            joined: true,
            ..Default::default()
        }));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(lock(&r.events).len(), n);
    }

    #[test]
    fn requests_outside_whitelist_are_rejected() {
        let mut r = rig();
        let _hello = r.stub.next();
        r.stub.send(FromSync::Request {
            id: 1,
            method: "system.restart".into(),
            params: json!({}),
        });
        r.stub.send(FromSync::Request {
            id: 2,
            method: "dict.reload".into(),
            params: json!({}),
        });
        match r.stub.next() {
            ToSync::Reply { id: 1, error, .. } => {
                assert!(error.unwrap().contains("白名单"));
            }
            m => panic!("{m:?}"),
        }
        match r.stub.next() {
            ToSync::Reply {
                id: 2,
                result,
                error,
            } => {
                assert!(error.is_none());
                assert_eq!(result.unwrap()["ran"], "dict.reload");
            }
            m => panic!("{m:?}"),
        }
        assert_eq!(*lock(&r.executed), vec!["dict.reload".to_string()]);
    }

    #[test]
    fn every_whitelisted_method_is_executed() {
        let mut r = rig();
        let _hello = r.stub.next();
        for (i, m) in REQUEST_WHITELIST.iter().enumerate() {
            r.stub.send(FromSync::Request {
                id: i as u64,
                method: m.to_string(),
                params: json!({}),
            });
            assert!(matches!(r.stub.next(), ToSync::Reply { error: None, .. }));
        }
        assert_eq!(lock(&r.executed).len(), REQUEST_WHITELIST.len());
    }

    #[test]
    fn command_round_trip() {
        let mut r = rig();
        let _hello = r.stub.next();
        let host = r.host.clone();
        let call = std::thread::spawn(move || {
            host.rpc(
                "sync.command",
                &json!({ "op": "export_key", "args": { "x": 1 } }),
            )
        });
        let id = match r.stub.next() {
            ToSync::Command { id, op, args } => {
                assert_eq!(op, Op::ExportKey);
                assert_eq!(args, json!({ "x": 1 }));
                id
            }
            m => panic!("{m:?}"),
        };
        r.stub
            .send(FromSync::command_reply(id, Ok(json!({"key": "abc"}))));
        assert_eq!(call.join().unwrap().unwrap(), json!({"key": "abc"}));
        assert!(lock(&r.host.pending).is_empty());

        // 同步进程回错误 -> 调用方拿到错误文本。
        let host = r.host.clone();
        let call =
            std::thread::spawn(move || host.rpc("sync.command", &json!({ "op": "sync_now" })));
        let ToSync::Command { id, .. } = r.stub.next() else {
            panic!()
        };
        r.stub
            .send(FromSync::command_reply(id, Err("口令错误".into())));
        assert!(
            call.join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("口令错误")
        );
    }

    #[test]
    fn command_times_out_and_cleans_up() {
        let mut r = rig_with(Some(webdav(true)), Duration::from_millis(200));
        let _hello = r.stub.next();
        let e = r
            .host
            .rpc("sync.command", &json!({ "op": "sync_now" }))
            .unwrap_err();
        assert!(e.to_string().contains("超时"));
        assert!(lock(&r.host.pending).is_empty());
        let _ = r.stub.next(); // 命令帧
        // 超时后迟到的回执被忽略，不崩。
        r.stub.send(FromSync::command_reply(1, Ok(json!({}))));
    }

    #[test]
    fn command_errors_without_process_or_with_bad_op() {
        let dir = tempfile::tempdir().unwrap();
        let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
        let e = host
            .rpc("sync.command", &json!({ "op": "sync_now" }))
            .unwrap_err();
        assert!(e.to_string().contains("未运行"));
        assert!(host.rpc("sync.command", &json!({ "op": "nope" })).is_err());
        assert!(host.rpc("sync.command", &json!({})).is_err());
    }

    #[test]
    fn process_exit_fails_waiting_command() {
        let r = rig();
        let Rig { host, stub, .. } = r;
        let h2 = host.clone();
        let call = std::thread::spawn(move || h2.rpc("sync.command", &json!({ "op": "sync_now" })));
        wait_for(|| !lock(&host.pending).is_empty());
        drop(stub.to_core); // 同步进程 stdout 关闭
        let e = call.join().unwrap().unwrap_err();
        assert!(e.to_string().contains("已退出"));
        assert_eq!(phase(&host), "starting", "已启用但进程不在：等待重启");
    }

    #[test]
    fn disabling_pushes_settings_then_shutdown() {
        let r = rig();
        let _hello = r.stub.next();
        r.host
            .rpc("sync.setSettings", &json!({ "settings": webdav(false) }))
            .unwrap();
        assert!(matches!(r.stub.next(), ToSync::Settings(s) if !s.enabled));
        assert!(matches!(r.stub.next(), ToSync::Shutdown));
        assert!(!lock(&r.host.inner).sup.wanted());
        assert_eq!(phase(&r.host), "disabled");
        assert!(lock(&r.host.inner).link.is_none());
    }

    #[test]
    fn settings_are_validated_persisted_and_secret_free() {
        let dir = tempfile::tempdir().unwrap();
        let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
        let file = dir.path().join("state/sync/settings.toml");

        let bad = json!({ "enabled": true });
        assert!(host.rpc("sync.setSettings", &bad).is_err());
        assert!(!file.exists(), "校验失败不落盘");

        host.rpc("sync.setSettings", &json!({ "settings": webdav(true) }))
            .unwrap();
        assert_eq!(load_settings(&file), webdav(true));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.to_lowercase().contains("password"), "{text}");
        assert!(lock(&host.inner).sup.wanted(), "启用且已安装 -> 期望运行");

        // 重新构造（相当于服务重启）读回同一份设置。
        let again = host_in(dir.path(), true, COMMAND_TIMEOUT);
        assert!(lock(&again.inner).sup.wanted());
        assert_eq!(
            again.rpc("sync.getSettings", &Value::Null).unwrap()["settings"]["enabled"],
            true
        );
    }

    #[test]
    fn s3_settings_round_trip_through_toml() {
        let s = SyncSettings {
            enabled: true,
            backend: Some(Backend::S3 {
                endpoint: "https://s3.example.com".into(),
                region: "auto".into(),
                bucket: "b".into(),
                prefix: "w/".into(),
                access_key_id: "AK".into(),
                path_style: true,
            }),
            interval_min: 30,
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.toml");
        save_settings(&f, &s).unwrap();
        assert_eq!(load_settings(&f), s);
    }

    #[test]
    fn corrupt_settings_file_falls_back_to_default_without_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("state/sync/settings.toml");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "enabled = [").unwrap();
        let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
        assert!(!lock(&host.inner).settings.enabled);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "enabled = [");
    }

    #[test]
    fn not_installed_is_reported_and_never_started() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("state/sync/settings.toml");
        save_settings(&f, &webdav(true)).unwrap();
        let host = host_in(dir.path(), false, COMMAND_TIMEOUT);
        assert!(!lock(&host.inner).sup.wanted(), "未安装不设期望态");
        host.tick(Instant::now());
        assert_eq!(phase(&host), "not_installed");
        let g = host.rpc("sync.getSettings", &Value::Null).unwrap();
        assert_eq!(g["installed"], false);
    }

    #[test]
    fn unknown_sync_method_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let host = host_in(dir.path(), true, COMMAND_TIMEOUT);
        assert!(host.rpc("sync.nope", &Value::Null).is_err());
    }

    #[test]
    fn stale_link_messages_are_ignored() {
        let mut r = rig();
        // 模拟重新拉起：epoch 前进后，旧读线程的 Status 不得写进状态。
        lock(&r.host.inner).epoch += 1;
        lock(&r.host.inner).link.as_mut().unwrap().epoch += 1;
        r.stub.send(FromSync::Status(Status {
            phase: "old".into(),
            ..Default::default()
        }));
        std::thread::sleep(Duration::from_millis(150));
        assert_ne!(phase(&r.host), "old");
        let _ = &r.dir;
    }
}
