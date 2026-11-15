//! 扩展子系统（服务进程侧，设计见 `docs/design/extension-hook-channel.md` §3、§6.1）。
//!
//! - 按 `[extensions]` 配置拉起扩展宿主 `wind_input_ext`（与本 exe 同目录），经它的 stdin 下发
//!   `config` 与事件，从它的 stdout 收 `status`。
//! - 协调器经 [`Tap`] 产出事件；这里一个转发线程把它们放进宿主的丢旧队列，再由写线程写管道。
//!   按键线程从不碰管道。
//! - 宿主崩溃按 [`Backoff`] 退避重启、频繁崩溃熔断；熔断后授权清零，协调器不再产出事件。
//! - 宿主随本进程生死：Windows 放进 `KILL_ON_JOB_CLOSE` 的 Job（扩展作为宿主的子进程一并在内）；
//!   其他平台宿主自设 `PR_SET_PDEATHSIG`，且 stdin EOF 即退出。
//!
//! 日志只记启停、崩溃、扩展状态变化，不记事件内容。

use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{error, info, warn};
use wind_config::{Config, ExtensionsConfig};
use wind_hook::backoff::{Backoff, Decision};
use wind_hook::frame::{Frame, read_frame, write_frame};
use wind_hook::msg::{ExtensionGrant, ExtensionStatus, Msg};
use wind_hook::queue::{DropOldest, Pop};
use wind_hook::tap::Tap;
use wind_hook::tier::Grants;

/// 协调器 → 转发线程的通道容量。
const TAP_CAP: usize = 1024;
/// 本进程 → 宿主的待写队列容量。
const QUEUE_CAP: usize = 1024;
const TICK: Duration = Duration::from_millis(200);
/// 停宿主时等它自行退出的宽限。
const STOP_GRACE: Duration = Duration::from_secs(2);
const HOST_LOG_MAX: u64 = 1024 * 1024;
/// 写宿主管道卡住这么久即判宿主卡死，结束后按崩溃处理。
const STALL: Duration = Duration::from_secs(10);

/// 宿主文件名随变体带后缀（`wind_input_ext_dev.exe`）：两个变体的宿主若同名，
/// 部署脚本按进程名停旧进程时会把另一个变体的也停掉。
fn host_exe_name() -> String {
    format!(
        "wind_input_ext{}{}",
        wind_config::variant::pipe_suffix(),
        std::env::consts::EXE_SUFFIX
    )
}

pub struct Extensions {
    tap: Arc<Tap>,
    /// 授权变化、宿主（重新）启动后调用：协调器据此补发当前模式、通知 DLL 要不要旁路上报按键。
    on_hook_changed: Box<dyn Fn() + Send + Sync>,
    state: Mutex<State>,
    status: Arc<Mutex<Vec<ExtensionStatus>>>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

struct State {
    /// 应当运行的扩展与授权；空 = 宿主不该运行。
    want: Vec<ExtensionGrant>,
    host: Option<Host>,
    backoff: Backoff,
    next_start: Option<Instant>,
}

struct Host {
    child: Child,
    queue: Arc<DropOldest<Msg>>,
    /// 写线程正卡在一次写上的起点。
    busy_since: Arc<Mutex<Option<Instant>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Extensions {
    pub fn start(tap: Arc<Tap>, on_hook_changed: Box<dyn Fn() + Send + Sync>) -> Arc<Extensions> {
        let rx = tap.connect(Grants::NONE, TAP_CAP);
        let p = Arc::new(Extensions {
            tap,
            on_hook_changed,
            state: Mutex::new(State {
                want: Vec::new(),
                host: None,
                backoff: Backoff::default(),
                next_start: None,
            }),
            status: Arc::new(Mutex::new(Vec::new())),
            #[cfg(windows)]
            job: job::Job::kill_on_close()
                .map_err(|e| warn!("扩展宿主的 Job 对象创建失败: {e}"))
                .ok(),
        });
        {
            let p = p.clone();
            let _ = std::thread::Builder::new()
                .name("extensions-forward".into())
                .spawn(move || {
                    while let Ok(msg) = rx.recv() {
                        if let Some(h) = &lock(&p.state).host {
                            h.queue.push(msg);
                        }
                    }
                });
        }
        {
            let p = p.clone();
            let _ = std::thread::Builder::new()
                .name("extensions-supervise".into())
                .spawn(move || {
                    loop {
                        p.tick(Instant::now());
                        std::thread::sleep(TICK);
                    }
                });
        }
        p
    }

    /// 配置变化（含启动时的初值）。授权有变时通知协调器（放锁之后）。
    pub fn apply_config(&self, cfg: &ExtensionsConfig) {
        if self.apply_config_locked(cfg) {
            (self.on_hook_changed)();
        }
    }

    /// 返回授权是否变了。
    fn apply_config_locked(&self, cfg: &ExtensionsConfig) -> bool {
        let want: Vec<ExtensionGrant> = if cfg.enabled {
            cfg.list
                .iter()
                .filter(|e| e.enabled)
                .map(|e| ExtensionGrant {
                    id: e.id.clone(),
                    granted: e.granted.clone(),
                    options: e.options.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut st = lock(&self.state);
        if want == st.want {
            return false;
        }
        st.want = want.clone();
        if want.is_empty() {
            self.tap.set_grants(Grants::NONE);
            st.next_start = None;
            st.backoff.reset();
            if let Some(h) = st.host.take() {
                info!("扩展已全部停用，关闭扩展宿主");
                stop_host(h);
            }
            lock(&self.status).clear();
            return true;
        }
        self.tap.set_grants(union(&want));
        match &st.host {
            Some(h) => {
                h.queue.push_keep(Msg::Config { extensions: want });
            }
            None => {
                // 用户改了配置 = 一次手动干预，熔断随之解除。
                st.backoff.reset();
                st.next_start = Some(Instant::now());
            }
        }
        true
    }

    fn tick(&self, now: Instant) {
        // 回调一律放锁后做：它会经 Tap 产出事件，转发线程要拿这把锁。
        if self.tick_locked(now) {
            (self.on_hook_changed)();
        }
    }

    /// 一次监护。返回是否要通知协调器（宿主刚起来，或熔断后授权清零）。
    fn tick_locked(&self, now: Instant) -> bool {
        let mut st = lock(&self.state);
        if let Some(h) = &mut st.host {
            match h.child.try_wait() {
                Ok(Some(code)) => {
                    warn!("扩展宿主意外退出: {code}");
                    h.queue.close();
                    st.host = None;
                    return self.on_host_exit(&mut st, now);
                }
                Ok(None) => {
                    let stalled = lock(&h.busy_since)
                        .is_some_and(|t| now.saturating_duration_since(t) >= STALL);
                    if stalled {
                        warn!("扩展宿主超过 {}s 不读消息，结束它", STALL.as_secs());
                        let _ = h.child.kill();
                    }
                }
                Err(e) => warn!("查询扩展宿主状态失败: {e}"),
            }
            return false;
        }
        if st.want.is_empty() || !st.next_start.is_some_and(|t| t <= now) {
            return false;
        }
        st.next_start = None;
        st.backoff.on_start(now);
        match self.spawn_host(&st.want) {
            Ok(h) => {
                info!("扩展宿主已启动（{} 个扩展）", st.want.len());
                st.host = Some(h);
                true
            }
            Err(e) => {
                error!("扩展宿主启动失败: {e}");
                self.on_host_exit(&mut st, now)
            }
        }
    }

    /// 返回是否熔断（授权随之清零，需要通知协调器）。
    fn on_host_exit(&self, st: &mut State, now: Instant) -> bool {
        match st.backoff.on_exit(now) {
            Decision::RestartAfter(d) => {
                st.next_start = Some(now + d);
                false
            }
            Decision::Tripped => {
                error!("扩展宿主频繁退出，已停止重启；修改扩展配置后会重新尝试");
                self.tap.set_grants(Grants::NONE);
                true
            }
        }
    }

    fn spawn_host(&self, want: &[ExtensionGrant]) -> Result<Host, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("取本进程路径失败: {e}"))?
            .with_file_name(host_exe_name());
        if !exe.is_file() {
            return Err(format!("找不到 {}", exe.display()));
        }
        let extensions_dir = extensions_dir().ok_or("取不到用户目录")?;
        let mut cmd = Command::new(&exe);
        cmd.arg("--extensions-dir")
            .arg(&extensions_dir)
            .arg("--core-version")
            .arg(env!("CARGO_PKG_VERSION"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(host_log());
        if let Some(dir) = Config::log_dir() {
            cmd.arg("--log-dir").arg(dir);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 {}: {e}", exe.display()))?;
        // 先入 Job 再下发配置：宿主收到配置才拉起扩展，扩展因此一定也在 Job 里。
        #[cfg(windows)]
        if let Some(job) = &self.job
            && let Err(e) = job.assign(&child)
        {
            warn!("扩展宿主加入 Job 失败（本进程崩溃时它可能残留）: {e}");
        }
        let stdin = child.stdin.take().ok_or("拿不到宿主 stdin")?;
        let stdout = child.stdout.take().ok_or("拿不到宿主 stdout")?;
        let queue = Arc::new(DropOldest::new(QUEUE_CAP));
        queue.push_keep(Msg::Config {
            extensions: want.to_vec(),
        });
        let busy_since = Arc::new(Mutex::new(None));
        spawn_writer(queue.clone(), stdin, busy_since.clone());
        self.spawn_reader(stdout);
        Ok(Host {
            child,
            queue,
            busy_since,
        })
    }

    fn spawn_reader(&self, mut stdout: ChildStdout) {
        let status = self.status.clone();
        let _ = std::thread::Builder::new()
            .name("extensions-read".into())
            .spawn(move || {
                while let Ok(Some(f)) = read_frame(&mut stdout) {
                    if let Msg::Status { extensions } = f.msg {
                        update_status(&status, extensions);
                    }
                }
            });
    }
}

/// 记下宿主上报的状态，扩展状态有变化的记一行日志。
fn update_status(status: &Mutex<Vec<ExtensionStatus>>, extensions: Vec<ExtensionStatus>) {
    let mut cur = lock(status);
    for s in &extensions {
        let changed = cur
            .iter()
            .find(|o| o.id == s.id)
            .is_none_or(|o| o.state != s.state);
        if changed {
            match &s.error {
                Some(e) => info!("扩展 {} → {}（{}）", s.id, s.state, e),
                None => info!("扩展 {} → {}", s.id, s.state),
            }
        }
    }
    *cur = extensions;
}

impl Extensions {
    /// 设置端扩展页的数据（ctrl RPC `extensions.inventory`）：扩展目录里装了哪些扩展、清单
    /// （名称、申请的档位、选项声明）或清单错误、宿主上报的运行状态。
    pub fn inventory(&self) -> serde_json::Value {
        let Some(root) = extensions_dir() else {
            return serde_json::json!({ "dir": null, "extensions": [] });
        };
        inventory(&root, &lock(&self.status))
    }
}

fn inventory(root: &std::path::Path, status: &[ExtensionStatus]) -> serde_json::Value {
    let extensions: Vec<serde_json::Value> = wind_hook::manifest::scan(root)
        .into_iter()
        .map(|(id, m)| {
            let status = status.iter().find(|s| s.id == id);
            match m {
                Ok(m) => serde_json::json!({
                    "id": id,
                    "name": m.name,
                    "want": m.want,
                    "options": m.options,
                    "status": status,
                }),
                Err(e) => serde_json::json!({ "id": id, "error": e, "status": status }),
            }
        })
        .collect();
    serde_json::json!({ "dir": root, "extensions": extensions })
}

fn union(want: &[ExtensionGrant]) -> Grants {
    want.iter().fold(Grants::NONE, |g, p| {
        g.union(Grants::parse(p.granted.iter().map(String::as_str)))
    })
}

/// 扩展目录：用户目录下的 `extensions/`。
pub fn extensions_dir() -> Option<PathBuf> {
    Config::user_config_dir().map(|d| d.join("extensions"))
}

fn host_log() -> Stdio {
    let Some(dir) = Config::log_dir() else {
        return Stdio::null();
    };
    let path = dir.join("extension_host.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > HOST_LOG_MAX) {
        let _ = std::fs::rename(&path, dir.join("extension_host.old.log"));
    }
    let _ = std::fs::create_dir_all(&dir);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null())
}

fn spawn_writer(
    queue: Arc<DropOldest<Msg>>,
    mut stdin: ChildStdin,
    busy_since: Arc<Mutex<Option<Instant>>>,
) {
    let _ = std::thread::Builder::new()
        .name("extensions-write".into())
        .spawn(move || {
            let mut seq = 0u64;
            loop {
                let (msg, dropped) = match queue.pop(Duration::from_secs(1)) {
                    Pop::Item(m, d) => (m, d),
                    Pop::Timeout => continue,
                    Pop::Closed => break,
                };
                seq += 1;
                let frame = Frame { seq, dropped, msg };
                *lock(&busy_since) = Some(Instant::now());
                let r = write_frame(&mut stdin, &frame);
                *lock(&busy_since) = None;
                if r.is_err() {
                    break;
                }
            }
            // stdin 随之关闭：宿主读到 EOF，收走扩展后退出。
        });
}

/// 发 `shutdown` 并关队列，宽限期后仍未退出就强杀。后台做，不卡调用方。
fn stop_host(mut h: Host) {
    h.queue.push_keep(Msg::Shutdown);
    h.queue.close();
    let _ = std::thread::Builder::new()
        .name("extensions-stop".into())
        .spawn(move || {
            let deadline = Instant::now() + STOP_GRACE;
            while Instant::now() < deadline {
                if matches!(h.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = h.child.kill();
            let _ = h.child.wait();
        });
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows::core::PCWSTR;

    /// 本进程退出（含崩溃）时句柄随之关闭，系统结束 Job 内全部进程。
    pub struct Job(HANDLE);

    // SAFETY: Job 句柄只用于 AssignProcessToJobObject，内核对象句柄可跨线程使用。
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn kill_on_close() -> windows::core::Result<Job> {
            // SAFETY: 标准 Win32 调用；info 在调用期间有效。
            unsafe {
                let h = CreateJobObjectW(None, PCWSTR::null())?;
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if let Err(e) = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) {
                    let _ = CloseHandle(h);
                    return Err(e);
                }
                Ok(Job(h))
            }
        }

        pub fn assign(&self, child: &Child) -> windows::core::Result<()> {
            // SAFETY: child 句柄在调用期间有效。
            unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_lists_manifests_errors_and_status() {
        let t = tempfile::tempdir().unwrap();
        let good = t.path().join("keysound");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("run"), "").unwrap();
        std::fs::write(
            good.join("extension.toml"),
            "name = \"按键音\"\nexe = \"run\"\nwant = [\"key.detail\"]\n\
             [[option]]\nkey = \"volume\"\ntype = \"number\"\ndefault = 0.6\nmin = 0.0\nmax = 1.0\n",
        )
        .unwrap();
        std::fs::create_dir_all(t.path().join("broken")).unwrap();
        let status = [ExtensionStatus {
            id: "keysound".into(),
            state: "running".into(),
            restarts: 0,
            dropped: 0,
            error: None,
        }];
        let v = inventory(t.path(), &status);
        let list = v["extensions"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["id"], "broken");
        assert!(
            list[0]["error"]
                .as_str()
                .unwrap()
                .contains("extension.toml")
        );
        assert!(list[0]["status"].is_null());
        assert_eq!(list[1]["name"], "按键音");
        assert_eq!(list[1]["want"], serde_json::json!(["key.detail"]));
        assert_eq!(list[1]["options"][0]["type"], "number");
        assert_eq!(list[1]["status"]["state"], "running");
    }
}
