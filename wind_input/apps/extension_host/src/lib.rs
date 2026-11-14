//! 扩展宿主：拉起、监护扩展，把核心发来的事件按授权分发给它们（设计见
//! `docs/design/extension-hook-channel.md` §3、§6.2）。
//!
//! - 与核心：stdin 读核心的帧（`config` / 事件 / `shutdown`），stdout 写 `status`。
//!   stdin EOF 即核心已退出，宿主收走全部扩展后退出。
//! - 与扩展：每个扩展一对 stdio 匿名管道，各配一个写线程（丢旧队列 → 管道）和一个读线程
//!   （握手、订阅、ping）。扩展的 stderr 追加到 `<log_dir>/extension-<id>.log`。
//! - 监护：崩溃按 [`Backoff`] 退避重启、熔断；写管道卡住超过 [`STALL`] 视为卡死，结束后按崩溃计。
//! - 扩展随宿主生死：Windows 放进宿主自建的 `KILL_ON_JOB_CLOSE` Job（嵌套在核心那个 Job 里）；
//!   Linux 设 `PR_SET_PDEATHSIG`。宿主被强杀时扩展也一并结束，不靠扩展自觉处理 stdin EOF。
//!
//! 宿主自己的日志写 stderr，由核心重定向到文件。事件内容不进日志。

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wind_hook::PROTOCOL_VERSION;
use wind_hook::backoff::{Backoff, Decision};
use wind_hook::frame::{Frame, read_frame, write_frame};
use wind_hook::manifest::{self, Manifest};
use wind_hook::msg::{ExtensionGrant, ExtensionStatus, Msg};
use wind_hook::options;
use wind_hook::queue::{DropOldest, Pop};
use wind_hook::tier::{Grants, trim};

/// 每个扩展的待写队列长度。按键再快每秒也就几十条，几百条够扛住一两秒的卡顿。
const QUEUE_CAP: usize = 256;
/// 写管道卡住这么久即判卡死（扩展不读 stdin）。
pub const STALL: Duration = Duration::from_secs(10);
/// 停扩展时等它自行退出的宽限。
const STOP_GRACE: Duration = Duration::from_secs(1);
/// 监护循环的节拍。
const TICK: Duration = Duration::from_millis(100);
/// 扩展日志超过这么大就轮换一份。
const LOG_MAX: u64 = 1024 * 1024;

pub struct Opts {
    pub extensions_dir: PathBuf,
    pub log_dir: Option<PathBuf>,
    pub core_version: String,
}

macro_rules! log {
    ($($a:tt)*) => { eprintln!("{} [extension_host] {}", wind_hook::stamp::utc_stamp(), format_args!($($a)*)) };
}

/// 入口：阻塞直到核心关闭 stdin 或发来 `shutdown`。返回进程退出码。
pub fn run(opts: Opts) -> i32 {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl(PR_SET_PDEATHSIG) 只设置本进程属性，无内存安全影响。
    unsafe {
        // 核心被 SIGKILL 时 stdin 也会断，这里只是让退出更及时。
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
    let host = Arc::new(Host::new(opts, Box::new(io::stdout())));
    let sup = {
        let host = host.clone();
        std::thread::Builder::new()
            .name("supervise".into())
            .spawn(move || host.supervise())
            .expect("spawn supervisor")
    };
    let mut stdin = io::stdin().lock();
    loop {
        match read_frame(&mut stdin) {
            Ok(Some(f)) => {
                if !host.on_core(f.msg) {
                    break;
                }
            }
            Ok(None) => {
                log!("核心已关闭管道，退出");
                break;
            }
            Err(e) => {
                log!("读核心消息失败，退出: {e}");
                break;
            }
        }
    }
    host.shutdown();
    let _ = sup.join();
    0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Starting,
    Running,
    Backoff,
    Tripped,
    Invalid,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Backoff => "backoff",
            State::Tripped => "tripped",
            State::Invalid => "invalid",
        }
    }
}

/// 宿主与一个运行中扩展之间的共享状态（读写线程与监护线程共用）。
struct Link {
    queue: DropOldest<Msg>,
    /// 核心配置授予的档位。
    config_grants: AtomicU8,
    /// 扩展握手时申请的档位；握手前为 `None`。
    want: Mutex<Option<Grants>>,
    /// 扩展收窄后的订阅；`None` 表示全部。
    subscribed: Mutex<Option<HashSet<String>>>,
    /// 写线程正卡在一次写上的起点；用于判卡死。
    busy_since: Mutex<Option<Instant>>,
    /// 发给扩展的选项取值（已校验、补齐默认值）。
    options: Mutex<Options>,
}

type Options = serde_json::Map<String, serde_json::Value>;

impl Link {
    fn new(grants: Grants, options: Options) -> Link {
        Link {
            queue: DropOldest::new(QUEUE_CAP),
            config_grants: AtomicU8::new(grants.bits()),
            want: Mutex::new(None),
            subscribed: Mutex::new(None),
            busy_since: Mutex::new(None),
            options: Mutex::new(options),
        }
    }

    /// 握手完成时发一次选项。与 [`Self::update_options`] 都在持锁时入队：否则握手读到旧值、
    /// 更新线程先入队新值，旧值后到会盖掉新值。
    fn send_options(&self) {
        let opts = lock(&self.options);
        if !opts.is_empty() {
            self.queue.push_keep(Msg::Options {
                values: opts.clone(),
            });
        }
    }

    /// 选项变了就换上并发给扩展（握手前不发，握手时自会发）。扩展不重启。
    fn update_options(&self, new: Options) {
        let mut opts = lock(&self.options);
        if *opts == new {
            return;
        }
        *opts = new;
        if lock(&self.want).is_some() {
            self.queue.push_keep(Msg::Options {
                values: opts.clone(),
            });
        }
    }

    fn config_grants(&self) -> Grants {
        Grants::from_bits(self.config_grants.load(Ordering::Relaxed))
    }

    /// 实际生效的档位：配置授予 ∩ 扩展申请。握手前为 `None`（不发事件）。
    fn effective(&self) -> Option<Grants> {
        lock(&self.want).map(|w| self.config_grants().intersect(w))
    }

    fn stalled(&self, now: Instant) -> bool {
        lock(&self.busy_since).is_some_and(|t| now.saturating_duration_since(t) >= STALL)
    }
}

struct Running {
    child: Child,
    link: Arc<Link>,
}

struct Slot {
    manifest: Option<Manifest>,
    error: Option<String>,
    grants: Grants,
    /// 核心配置里用户设过的选项（未校验）。
    options: Options,
    state: State,
    backoff: Backoff,
    restarts: u32,
    next_start: Option<Instant>,
    run: Option<Running>,
}

impl Slot {
    /// 按清单校验、补齐默认值后的选项取值。
    fn effective_options(&self) -> Options {
        self.manifest
            .as_ref()
            .map(|m| options::resolve(&m.options, &self.options))
            .unwrap_or_default()
    }
}

pub struct Host {
    opts: Opts,
    out: Mutex<Box<dyn Write + Send>>,
    slots: Mutex<BTreeMap<String, Slot>>,
    /// 核心最近发来的模式事件。模式不常变，后连上（或重启后）的扩展握手完立即补发一份，
    /// 否则它要等到用户下次切中英才知道当前状态。
    last_mode: Arc<Mutex<Option<Msg>>>,
    stop: AtomicBool,
    #[cfg(windows)]
    job: Option<job::Job>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Host {
    pub fn new(opts: Opts, out: Box<dyn Write + Send>) -> Host {
        Host {
            opts,
            out: Mutex::new(out),
            slots: Mutex::new(BTreeMap::new()),
            last_mode: Arc::new(Mutex::new(None)),
            stop: AtomicBool::new(false),
            #[cfg(windows)]
            job: job::Job::kill_on_close()
                .map_err(|e| log!("创建 Job 对象失败（宿主被强杀时扩展可能残留）: {e}"))
                .ok(),
        }
    }

    /// 处理核心的一条消息。返回 `false` 表示该退出了。
    fn on_core(&self, msg: Msg) -> bool {
        match msg {
            Msg::Config { extensions } => self.apply_config(extensions),
            Msg::Shutdown => return false,
            Msg::Key(_)
            | Msg::Mode(_)
            | Msg::Composition(_)
            | Msg::Candidates(_)
            | Msg::Commit(_) => self.dispatch(&msg),
            _ => {}
        }
        true
    }

    fn dispatch(&self, msg: &Msg) {
        if matches!(msg, Msg::Mode(_)) {
            *lock(&self.last_mode) = Some(msg.clone());
        }
        for slot in lock(&self.slots).values() {
            if let Some(run) = &slot.run
                && trim(msg, run.link.config_grants()).is_some()
            {
                run.link.queue.push(msg.clone());
            }
        }
    }

    fn apply_config(&self, extensions: Vec<ExtensionGrant>) {
        let mut slots = lock(&self.slots);
        let wanted: BTreeMap<String, (Grants, Options)> = extensions
            .into_iter()
            .map(|p| {
                let g = Grants::parse(p.granted.iter().map(String::as_str));
                (p.id, (g, p.options))
            })
            .collect();
        let removed: Vec<String> = slots
            .keys()
            .filter(|id| !wanted.contains_key(*id))
            .cloned()
            .collect();
        for id in removed {
            if let Some(slot) = slots.remove(&id) {
                log!("停用扩展 {id}");
                if let Some(run) = slot.run {
                    stop_running(run);
                }
            }
        }
        for (id, (grants, opts)) in wanted {
            match slots.get_mut(&id) {
                // 无效或已熔断的扩展：配置变了视为用户干预过，从头再试（重新读清单、清零熔断）。
                Some(slot) if matches!(slot.state, State::Invalid | State::Tripped) => {
                    *slot = self.new_slot(&id, grants, opts);
                }
                Some(slot) => {
                    slot.grants = grants;
                    slot.options = opts;
                    if let Some(run) = &slot.run {
                        run.link
                            .config_grants
                            .store(grants.bits(), Ordering::Relaxed);
                        run.link.update_options(slot.effective_options());
                    }
                }
                None => {
                    let slot = self.new_slot(&id, grants, opts);
                    slots.insert(id, slot);
                }
            }
        }
        drop(slots);
        self.send_status();
    }

    fn new_slot(&self, id: &str, grants: Grants, options: Options) -> Slot {
        let (manifest, error, state) = if !manifest::valid_id(id) {
            (None, Some(format!("扩展 id `{id}` 不合法")), State::Invalid)
        } else {
            match manifest::load(&self.opts.extensions_dir.join(id)) {
                Ok(m) => (Some(m), None, State::Starting),
                Err(e) => (None, Some(e), State::Invalid),
            }
        };
        if let Some(e) = &error {
            log!("扩展 {id} 无效: {e}");
        }
        Slot {
            manifest,
            error,
            grants,
            options,
            state,
            backoff: Backoff::default(),
            restarts: 0,
            next_start: (state == State::Starting).then(Instant::now),
            run: None,
        }
    }

    fn supervise(&self) {
        while !self.stop.load(Ordering::Relaxed) {
            if self.tick(Instant::now()) {
                self.send_status();
            }
            std::thread::sleep(TICK);
        }
    }

    /// 一次监护：收尸、判卡死、到点重启。返回状态是否有变化。
    fn tick(&self, now: Instant) -> bool {
        let mut changed = false;
        let mut slots = lock(&self.slots);
        for (id, slot) in slots.iter_mut() {
            if let Some(run) = &mut slot.run {
                match run.child.try_wait() {
                    Ok(Some(status)) => {
                        log!("扩展 {id} 退出: {status}");
                        run.link.queue.close();
                        slot.run = None;
                        self.on_exit(id, slot, now);
                        changed = true;
                    }
                    Ok(None) => {
                        if run.link.stalled(now) {
                            log!("扩展 {id} 超过 {}s 不读事件，结束它", STALL.as_secs());
                            let _ = run.child.kill();
                        }
                    }
                    Err(e) => log!("查询扩展 {id} 状态失败: {e}"),
                }
            } else if slot.next_start.is_some_and(|t| t <= now) {
                slot.next_start = None;
                changed = true;
                match self.spawn(id, slot) {
                    Ok(run) => {
                        log!("扩展 {id} 已启动");
                        slot.run = Some(run);
                        slot.state = State::Running;
                        slot.error = None;
                        slot.backoff.on_start(now);
                    }
                    Err(e) => {
                        log!("扩展 {id} 启动失败: {e}");
                        slot.error = Some(e);
                        slot.backoff.on_start(now);
                        self.on_exit(id, slot, now);
                    }
                }
            }
        }
        changed
    }

    fn on_exit(&self, id: &str, slot: &mut Slot, now: Instant) {
        match slot.backoff.on_exit(now) {
            Decision::RestartAfter(d) => {
                slot.state = State::Backoff;
                slot.restarts += 1;
                slot.next_start = Some(now + d);
            }
            Decision::Tripped => {
                log!("扩展 {id} 频繁退出，已熔断");
                slot.state = State::Tripped;
                slot.next_start = None;
                if slot.error.is_none() {
                    slot.error = Some("短时间内多次异常退出，已停止重启".into());
                }
            }
        }
    }

    fn spawn(&self, id: &str, slot: &Slot) -> Result<Running, String> {
        let m = slot.manifest.as_ref().ok_or("没有清单")?;
        let mut cmd = Command::new(&m.exe);
        cmd.args(&m.args)
            .current_dir(&m.dir)
            .env("WIND_EXTENSION_ID", id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(self.extension_log(id));
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: pre_exec 闭包里只调异步信号安全的 prctl。
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 {:?} 失败: {e}", m.exe))?;
        #[cfg(windows)]
        if let Some(job) = &self.job
            && let Err(e) = job.assign(&child)
        {
            log!("扩展 {id} 加入 Job 失败: {e}");
        }
        let stdin = child.stdin.take().ok_or("拿不到扩展 stdin")?;
        let stdout = child.stdout.take().ok_or("拿不到扩展 stdout")?;
        let link = Arc::new(Link::new(slot.grants, slot.effective_options()));
        spawn_writer(id, link.clone(), stdin);
        spawn_reader(
            id,
            link.clone(),
            stdout,
            self.opts.core_version.clone(),
            m.protocol.clone(),
            self.last_mode.clone(),
        );
        Ok(Running { child, link })
    }

    fn extension_log(&self, id: &str) -> Stdio {
        let Some(dir) = &self.opts.log_dir else {
            return Stdio::null();
        };
        let path = dir.join(format!("extension-{id}.log"));
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX) {
            let _ = std::fs::rename(&path, dir.join(format!("extension-{id}.old.log")));
        }
        match open_append(&path) {
            Ok(f) => Stdio::from(f),
            Err(e) => {
                log!("打开扩展日志 {path:?} 失败: {e}");
                Stdio::null()
            }
        }
    }

    fn send_status(&self) {
        let extensions = lock(&self.slots)
            .iter()
            .map(|(id, s)| ExtensionStatus {
                id: id.clone(),
                state: s.state.name().into(),
                restarts: s.restarts,
                dropped: s.run.as_ref().map_or(0, |r| r.link.queue.total_dropped()),
                error: s.error.clone(),
            })
            .collect();
        let mut out = lock(&self.out);
        if let Err(e) = write_frame(&mut *out, &Frame::new(Msg::Status { extensions })) {
            log!("写状态给核心失败: {e}");
        }
    }

    fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let runs: Vec<Running> = lock(&self.slots)
            .values_mut()
            .filter_map(|s| s.run.take())
            .collect();
        let handles: Vec<_> = runs.into_iter().map(stop_running).collect();
        for h in handles {
            let _ = h.join();
        }
    }
}

fn open_append(path: &Path) -> io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// 关队列让写线程退出（扩展随之读到 EOF），宽限期后仍未退出就强杀。在后台线程里做，
/// 不卡调用方；返回句柄供关机时等待。
fn stop_running(mut run: Running) -> std::thread::JoinHandle<()> {
    run.link.queue.close();
    std::thread::spawn(move || {
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if matches!(run.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = run.child.kill();
        let _ = run.child.wait();
    })
}

fn spawn_writer(id: &str, link: Arc<Link>, mut stdin: ChildStdin) {
    let id = id.to_string();
    let _ = std::thread::Builder::new()
        .name(format!("extension-w-{id}"))
        .spawn(move || {
            let mut seq = 0u64;
            let mut dropped = 0u64;
            let mut welcomed = false;
            loop {
                let (msg, d) = match link.queue.pop(Duration::from_secs(1)) {
                    Pop::Item(m, d) => (m, d),
                    Pop::Timeout => continue,
                    Pop::Closed => break,
                };
                dropped += d;
                let msg = if msg.event_kind().is_some() {
                    // 握手前的事件丢弃：扩展还没声明要什么。
                    let Some(eff) = link.effective().filter(|_| welcomed) else {
                        continue;
                    };
                    let subscribed = lock(&link.subscribed)
                        .as_ref()
                        .is_none_or(|s| msg.event_kind().is_some_and(|k| s.contains(k)));
                    match trim(&msg, eff) {
                        Some(m) if subscribed => m,
                        _ => continue,
                    }
                } else {
                    if matches!(msg, Msg::Welcome { .. }) {
                        welcomed = true;
                    }
                    msg
                };
                seq += 1;
                let frame = Frame {
                    seq,
                    dropped: std::mem::take(&mut dropped),
                    msg,
                };
                *lock(&link.busy_since) = Some(Instant::now());
                let r = write_frame(&mut stdin, &frame);
                *lock(&link.busy_since) = None;
                if r.is_err() {
                    break;
                }
            }
            // 走到这里 stdin 随之关闭，扩展读到 EOF。
        });
}

fn spawn_reader(
    id: &str,
    link: Arc<Link>,
    mut stdout: ChildStdout,
    core_version: String,
    manifest_protocol: Vec<u32>,
    last_mode: Arc<Mutex<Option<Msg>>>,
) {
    let id = id.to_string();
    let _ = std::thread::Builder::new()
        .name(format!("extension-r-{id}"))
        .spawn(move || {
            loop {
                let msg = match read_frame(&mut stdout) {
                    Ok(Some(f)) => f.msg,
                    Ok(None) => break,
                    Err(e) => {
                        log!("扩展 {id} 发来坏帧，不再读取: {e}");
                        break;
                    }
                };
                match msg {
                    Msg::Hello { protocol, want } => {
                        let supported = if protocol.is_empty() {
                            &manifest_protocol
                        } else {
                            &protocol
                        };
                        if !supported.is_empty() && !supported.contains(&PROTOCOL_VERSION) {
                            log!("扩展 {id} 不支持协议 v{PROTOCOL_VERSION}: {supported:?}");
                        }
                        let want = Grants::parse(want.iter().map(String::as_str));
                        *lock(&link.want) = Some(want);
                        let granted = link.config_grants().intersect(want);
                        link.queue.push_keep(Msg::Welcome {
                            protocol: PROTOCOL_VERSION,
                            granted: granted.names(),
                            core: core_version.clone(),
                        });
                        // 补发当前模式（写线程按授权裁剪，没授 mode 的扩展收不到）。
                        if let Some(m) = lock(&last_mode).clone() {
                            link.queue.push_keep(m);
                        }
                        link.send_options();
                    }
                    Msg::Subscribe { events } => {
                        *lock(&link.subscribed) = Some(events.into_iter().collect());
                    }
                    Msg::Ping => {
                        link.queue.push(Msg::Pong);
                    }
                    _ => {}
                }
            }
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

    /// 本进程退出（含被强杀）时句柄随之关闭，系统结束 Job 内全部扩展。
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
