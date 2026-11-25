//! 扩展子系统（服务进程侧，设计见 `docs/design/extension-hook-channel.md` §3、§6.1）。
//!
//! - 按 `[extensions]` 配置拉起扩展宿主 `wind_input_ext`（与本 exe 同目录），经它的 stdin 下发
//!   `config` 与事件，从它的 stdout 收 `status`。
//! - 协调器经 [`Tap`] 产出事件；这里一个转发线程把它们放进宿主的丢旧队列，再由写线程写管道。
//!   按键线程从不碰管道。
//! - 宿主崩溃按 [`wind_supervise::backoff::Backoff`] 退避重启、频繁崩溃熔断；熔断后授权清零，协调器不再产出事件。
//! - 宿主随本进程生死：Windows 放进 `KILL_ON_JOB_CLOSE` 的 Job（扩展作为宿主的子进程一并在内）；
//!   其他平台宿主自设 `PR_SET_PDEATHSIG`，且 stdin EOF 即退出。
//!
//! 日志只记启停、崩溃、扩展状态变化，不记事件内容。

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{ChildStdin, ChildStdout, Command};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::{error, info, warn};
use wind_config::{Config, ExtensionsConfig};
use wind_hook::frame::{Frame, read_frame, write_frame};
use wind_hook::msg::{ExtensionGrant, ExtensionStatus, Msg};
use wind_hook::queue::{DropOldest, Pop};
use wind_hook::tap::Tap;
use wind_hook::tier::Grants;
use wind_supervise::backoff::Decision;
use wind_supervise::{Event, Job, Supervisor};

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
    /// 扩展首次从同组设备到达并启用时的提示（弹 toast 的接线处）。
    notify: OnceLock<Box<dyn Fn(String) + Send + Sync>>,
}

struct State {
    /// 应当运行的扩展与授权；空 = 宿主不该运行。
    want: Vec<ExtensionGrant>,
    host: Option<Host>,
    /// 宿主进程的监护（期望态、退避、熔断）；`host` 是进程在跑时的管道侧状态。
    sup: Supervisor,
    /// 最近一次应用的配置：暂停 / 恢复时据此重算期望集。
    cfg: ExtensionsConfig,
    /// 被同步临时暂停的扩展（文件正在被替换）：不在期望集里。
    suspended: BTreeSet<String>,
    /// 暂停时磁盘上还没有 `extension.toml` 的扩展：恢复后出现即视为首次到达。
    absent_at_suspend: BTreeSet<String>,
    /// 扩展目录。
    root: Option<PathBuf>,
}

struct Host {
    queue: Arc<DropOldest<Msg>>,
    /// 写线程正卡在一次写上的起点。
    busy_since: Arc<Mutex<Option<Instant>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Extensions {
    pub fn start(
        tap: Arc<Tap>,
        on_hook_changed: Box<dyn Fn() + Send + Sync>,
        on_poll: Box<dyn Fn() + Send + Sync>,
    ) -> Arc<Extensions> {
        let rx = tap.connect(Grants::NONE, TAP_CAP);
        let job = Job::kill_on_close()
            .map_err(|e| warn!("扩展宿主的 Job 对象创建失败: {e}"))
            .ok()
            .map(Arc::new);
        let sup = Supervisor::new(host_command)
            .with_log(
                Config::log_dir().map(|d| d.join("extension_host.log")),
                HOST_LOG_MAX,
            )
            .with_job(job);
        let p = Arc::new(Extensions {
            tap,
            on_hook_changed,
            state: Mutex::new(State {
                want: Vec::new(),
                host: None,
                sup,
                cfg: ExtensionsConfig::default(),
                suspended: BTreeSet::new(),
                absent_at_suspend: BTreeSet::new(),
                root: extensions_dir(),
            }),
            status: Arc::new(Mutex::new(Vec::new())),
            notify: OnceLock::new(),
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
                        on_poll();
                        std::thread::sleep(TICK);
                    }
                });
        }
        p
    }

    /// 服务即将重启或退出：关宿主，扩展会先收到 `lifecycle: stopping`（宿主发），有机会收尾。
    /// 最多等 `STOP_GRACE`，宿主退出即返回；没有宿主时立即返回。
    pub fn stop(&self) {
        let waiter = {
            let mut st = lock(&self.state);
            st.want.clear();
            if let Some(h) = st.host.take() {
                // 宿主收到 shutdown 会先向各扩展发 `lifecycle: stopping`，再收走它们。
                h.queue.push_keep(Msg::Shutdown);
                h.queue.close();
            }
            st.sup.stop_background(STOP_GRACE)
        };
        if let Some(w) = waiter {
            let _ = w.join();
        }
    }

    /// 配置变化（含启动时的初值）。授权有变时通知协调器（放锁之后）。
    pub fn apply_config(&self, cfg: &ExtensionsConfig) {
        lock(&self.state).cfg = cfg.clone();
        self.reapply();
    }

    /// 暂停这些扩展（同步要替换它们的文件）：移出期望集，宿主随之停掉它们。
    pub fn suspend(&self, ids: &[String]) {
        {
            let mut st = lock(&self.state);
            let root = st.root.clone();
            for id in ids {
                let present = root
                    .as_ref()
                    .is_some_and(|r| r.join(id).join("extension.toml").is_file());
                if !present {
                    st.absent_at_suspend.insert(id.clone());
                }
            }
            st.suspended.extend(ids.iter().cloned());
        }
        self.reapply();
    }

    /// 接上「扩展首次到达」提示。
    #[allow(dead_code)] // 接线在主循环里另做
    pub fn set_notify(&self, f: Box<dyn Fn(String) + Send + Sync>) {
        let _ = self.notify.set(f);
    }

    /// 恢复被暂停的扩展，按当前配置重配。
    pub fn resume(&self, ids: &[String]) {
        let mut arrived = Vec::new();
        {
            let mut st = lock(&self.state);
            for id in ids {
                st.suspended.remove(id);
                if !st.absent_at_suspend.remove(id) {
                    continue;
                }
                let enabled =
                    st.cfg.enabled && st.cfg.list.iter().any(|e| e.enabled && e.id == *id);
                let dir = st.root.as_ref().map(|r| r.join(id));
                if let Some(dir) = dir.filter(|d| enabled && d.join("extension.toml").is_file()) {
                    let name = wind_hook::manifest::load(&dir)
                        .map(|m| m.name)
                        .unwrap_or_else(|_| id.clone());
                    arrived.push(name);
                }
            }
        }
        self.reapply();
        if let Some(f) = self.notify.get() {
            for name in arrived {
                f(format!("扩展 {name} 已从同组设备同步并启用"));
            }
        }
    }

    /// 当前期望集里的扩展 id（测试用）。
    #[cfg(test)]
    fn want_ids(&self) -> Vec<String> {
        lock(&self.state)
            .want
            .iter()
            .map(|g| g.id.clone())
            .collect()
    }

    fn reapply(&self) {
        if self.apply_config_locked() {
            (self.on_hook_changed)();
        }
    }

    /// 按已存的配置与暂停集重算期望集。返回授权是否变了。
    fn apply_config_locked(&self) -> bool {
        let mut st = lock(&self.state);
        let cfg = &st.cfg;
        let want: Vec<ExtensionGrant> = if cfg.enabled {
            cfg.list
                .iter()
                .filter(|e| e.enabled && !st.suspended.contains(&e.id))
                .map(|e| ExtensionGrant {
                    id: e.id.clone(),
                    granted: e.granted.clone(),
                    options: e.options.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };
        if want == st.want {
            return false;
        }
        st.want = want.clone();
        if want.is_empty() {
            self.tap.set_grants(Grants::NONE);
            if let Some(h) = st.host.take() {
                info!("扩展已全部停用，关闭扩展宿主");
                h.queue.push_keep(Msg::Shutdown);
                h.queue.close();
            }
            // 宽限期后仍未退出就强杀；后台做，不卡调用方。
            st.sup.stop_background(STOP_GRACE);
            lock(&self.status).clear();
            return true;
        }
        self.tap.set_grants(union(&want));
        let now = Instant::now();
        st.sup.set_wanted(true, now);
        match &st.host {
            Some(h) => {
                h.queue.push_keep(Msg::Config { extensions: want });
            }
            None => {
                // 用户改了配置 = 一次手动干预，熔断随之解除。
                st.sup.retry(now);
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
        match st.sup.tick(now) {
            Some(Event::Exited { status, next }) => {
                warn!("扩展宿主意外退出: {status}");
                if let Some(h) = st.host.take() {
                    h.queue.close();
                }
                self.on_host_exit(next)
            }
            Some(Event::PollFailed(e)) => {
                warn!("查询扩展宿主状态失败: {e}");
                false
            }
            Some(Event::StartFailed { error, next }) => {
                error!("扩展宿主启动失败: {error}");
                self.on_host_exit(next)
            }
            Some(Event::Started {
                stdin,
                stdout,
                warnings,
            }) => {
                for w in warnings {
                    warn!("扩展宿主: {w}");
                }
                info!("扩展宿主已启动（{} 个扩展）", st.want.len());
                let host = self.attach_host(&st.want, stdin, stdout);
                st.host = Some(host);
                true
            }
            None => {
                if let Some(h) = &st.host {
                    let stalled = lock(&h.busy_since)
                        .is_some_and(|t| now.saturating_duration_since(t) >= STALL);
                    if stalled {
                        warn!("扩展宿主超过 {}s 不读消息，结束它", STALL.as_secs());
                        st.sup.kill();
                    }
                }
                false
            }
        }
    }

    /// 返回是否熔断（授权随之清零，需要通知协调器）。
    fn on_host_exit(&self, next: Decision) -> bool {
        match next {
            Decision::RestartAfter(_) => false,
            Decision::Tripped => {
                error!("扩展宿主频繁退出，已停止重启；修改扩展配置后会重新尝试");
                self.tap.set_grants(Grants::NONE);
                true
            }
        }
    }

    /// 宿主进程已拉起（监护器已把它放进 Job）：配好队列、写线程、读线程，先下发配置。
    fn attach_host(&self, want: &[ExtensionGrant], stdin: ChildStdin, stdout: ChildStdout) -> Host {
        let queue = Arc::new(DropOldest::new(QUEUE_CAP));
        queue.push_keep(Msg::Config {
            extensions: want.to_vec(),
        });
        let busy_since = Arc::new(Mutex::new(None));
        spawn_writer(queue.clone(), stdin, busy_since.clone());
        self.spawn_reader(stdout);
        Host { queue, busy_since }
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

/// 拉起宿主的命令（stdio、日志、Job 由监护器配）。
fn host_command() -> Result<Command, String> {
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
        .arg(env!("CARGO_PKG_VERSION"));
    if let Some(dir) = Config::log_dir() {
        cmd.arg("--log-dir").arg(dir);
    }
    Ok(cmd)
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

#[cfg(test)]
mod tests {
    use super::*;
    use wind_config::ExtensionEntry;

    fn entry(id: &str) -> ExtensionEntry {
        ExtensionEntry {
            id: id.into(),
            enabled: true,
            granted: vec!["key.detail".into()],
            options: Default::default(),
        }
    }

    fn started(ids: &[&str]) -> Arc<Extensions> {
        let ext = Extensions::start(Arc::new(Tap::new()), Box::new(|| {}), Box::new(|| {}));
        ext.apply_config(&ExtensionsConfig {
            enabled: true,
            list: ids.iter().map(|i| entry(i)).collect(),
        });
        ext
    }

    #[test]
    fn suspended_extensions_leave_the_want_set_until_resumed() {
        let ext = started(&["a", "b"]);
        assert_eq!(ext.want_ids(), ["a", "b"]);
        ext.suspend(&["a".to_string()]);
        assert_eq!(ext.want_ids(), ["b"]);
        ext.resume(&["a".to_string()]);
        assert_eq!(ext.want_ids(), ["a", "b"]);
    }

    #[test]
    fn config_change_while_suspended_keeps_it_out() {
        let ext = started(&["a"]);
        ext.suspend(&["a".to_string()]);
        assert!(ext.want_ids().is_empty());
        ext.apply_config(&ExtensionsConfig {
            enabled: true,
            list: vec![entry("a"), entry("c")],
        });
        assert_eq!(ext.want_ids(), ["c"]);
        ext.resume(&["a".to_string()]);
        assert_eq!(ext.want_ids(), ["a", "c"]);
    }

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

    fn manifest(dir: &std::path::Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("run"), "").unwrap();
        std::fs::write(
            dir.join("extension.toml"),
            format!("name = \"{name}\"\nexe = \"run\"\nwant = [\"key.detail\"]\n"),
        )
        .unwrap();
    }

    fn with_root(ids: &[&str]) -> (Arc<Extensions>, tempfile::TempDir, Arc<Mutex<Vec<String>>>) {
        let ext = started(ids);
        let t = tempfile::tempdir().unwrap();
        lock(&ext.state).root = Some(t.path().to_path_buf());
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        ext.set_notify(Box::new(move |m| lock(&g).push(m)));
        (ext, t, got)
    }

    #[test]
    fn extension_arriving_during_suspend_is_announced_once() {
        let (ext, t, got) = with_root(&["a"]);
        let ids = ["a".to_string()];
        ext.suspend(&ids);
        manifest(&t.path().join("a"), "按键音");
        ext.resume(&ids);
        assert_eq!(*lock(&got), ["扩展 按键音 已从同组设备同步并启用"]);
        ext.suspend(&ids);
        ext.resume(&ids);
        assert_eq!(lock(&got).len(), 1, "再次暂停恢复不重复提示");
    }

    #[test]
    fn extension_already_present_is_not_announced() {
        let (ext, t, got) = with_root(&["a"]);
        manifest(&t.path().join("a"), "按键音");
        let ids = ["a".to_string()];
        ext.suspend(&ids);
        ext.resume(&ids);
        assert!(lock(&got).is_empty());
    }

    #[test]
    fn arriving_extension_not_enabled_in_config_is_not_announced() {
        let (ext, t, got) = with_root(&[]);
        let ids = ["a".to_string()];
        ext.suspend(&ids);
        manifest(&t.path().join("a"), "按键音");
        ext.resume(&ids);
        assert!(lock(&got).is_empty());
    }

    #[test]
    fn arriving_extension_without_readable_name_falls_back_to_id() {
        let (ext, t, got) = with_root(&["a"]);
        let ids = ["a".to_string()];
        ext.suspend(&ids);
        std::fs::create_dir_all(t.path().join("a")).unwrap();
        std::fs::write(t.path().join("a/extension.toml"), "坏的 = [").unwrap();
        ext.resume(&ids);
        assert_eq!(*lock(&got), ["扩展 a 已从同组设备同步并启用"]);
    }
}
