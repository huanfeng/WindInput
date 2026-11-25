//! 扩展宿主：拉起、监护扩展，把核心发来的事件按授权分发给它们（设计见
//! `docs/design/extension-hook-channel.md` §3、§6.2）。
//!
//! - 与核心：stdin 读核心的帧（`config` / 事件 / `shutdown`），stdout 写 `status`。
//!   stdin EOF 即核心已退出，宿主收走全部扩展后退出。
//! - 与扩展：每个扩展一对 stdio 匿名管道，各配一个写线程（丢旧队列 → 管道）和一个读线程
//!   （握手、订阅、ping）。扩展的 stderr 追加到 `<log_dir>/extension-<id>.log`。
//! - 监护：崩溃按 [`wind_supervise::backoff::Backoff`] 退避重启、熔断；写管道卡住超过 [`STALL`] 视为卡死，结束后按崩溃计。
//! - 扩展随宿主生死：Windows 放进宿主自建的 `KILL_ON_JOB_CLOSE` Job（嵌套在核心那个 Job 里）；
//!   Linux 设 `PR_SET_PDEATHSIG`。宿主被强杀时扩展也一并结束，不靠扩展自觉处理 stdin EOF。
//!
//! 宿主自己的日志写 stderr，由核心重定向到文件。事件内容不进日志。

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, ChildStdout, Command};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wind_hook::PROTOCOL_VERSION;
use wind_hook::frame::{Frame, read_frame, write_frame};
use wind_hook::manifest::{self, Manifest};
use wind_hook::msg::{ExtensionGrant, ExtensionStatus, LifecycleEvent, Msg, lifecycle};
use wind_hook::options;
use wind_hook::queue::{DropOldest, Pop};
use wind_hook::tap::now_ms;
use wind_hook::tier::{Grants, trim};
use wind_supervise::backoff::Decision;
use wind_supervise::{Event, Job, Supervisor};

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
    // 核心被 SIGKILL 时 stdin 也会断，这里只是让退出更及时。
    wind_supervise::die_with_parent();
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

struct Slot {
    manifest: Option<Manifest>,
    error: Option<String>,
    grants: Grants,
    /// 核心配置里用户设过的选项（未校验）。
    options: Options,
    state: State,
    restarts: u32,
    /// 清单有效才有：监护扩展进程（期望态、退避、熔断）。
    sup: Option<Supervisor>,
    /// 进程在跑时的管道侧状态。
    link: Option<Arc<Link>>,
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
    /// 核心最近发来的终端在线/焦点状态，补发理由同 `last_mode`。
    last_presence: Arc<Mutex<Option<Msg>>>,
    stop: AtomicBool,
    /// 宿主被强杀时系统据此结束全部扩展（Windows）。
    job: Option<Arc<Job>>,
}

fn lifecycle_msg(event: &str) -> Msg {
    Msg::Lifecycle(LifecycleEvent {
        ts: now_ms(),
        event: event.into(),
    })
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
            last_presence: Arc::new(Mutex::new(None)),
            stop: AtomicBool::new(false),
            job: Job::kill_on_close()
                .map_err(|e| log!("创建 Job 对象失败（宿主被强杀时扩展可能残留）: {e}"))
                .ok()
                .map(Arc::new),
        }
    }

    /// 处理核心的一条消息。返回 `false` 表示该退出了。
    fn on_core(&self, msg: Msg) -> bool {
        match msg {
            Msg::Config { extensions } => self.apply_config(extensions),
            Msg::Shutdown => {
                // 先告知再收走：队列关闭时会把已入队的消息写完，扩展有机会收尾。
                self.broadcast_keep(lifecycle_msg(lifecycle::STOPPING));
                return false;
            }
            Msg::Key(_)
            | Msg::Mode(_)
            | Msg::Composition(_)
            | Msg::Candidates(_)
            | Msg::Commit(_)
            | Msg::Presence(_)
            | Msg::Lifecycle(_) => self.dispatch(&msg),
            _ => {}
        }
        true
    }

    fn dispatch(&self, msg: &Msg) {
        match msg {
            Msg::Mode(_) => *lock(&self.last_mode) = Some(msg.clone()),
            Msg::Presence(_) => *lock(&self.last_presence) = Some(msg.clone()),
            _ => {}
        }
        for slot in lock(&self.slots).values() {
            if let Some(link) = &slot.link
                && trim(msg, link.config_grants()).is_some()
            {
                link.queue.push(msg.clone());
            }
        }
    }

    /// 给全部运行中的扩展发一条不可淘汰的状态消息（经写线程按授权/订阅过滤）。
    fn broadcast_keep(&self, msg: Msg) {
        for slot in lock(&self.slots).values() {
            if let Some(link) = &slot.link {
                link.queue.push_keep(msg.clone());
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
                if let Some(link) = &slot.link {
                    link.queue.close();
                }
                if let Some(mut sup) = slot.sup {
                    sup.stop_background(STOP_GRACE);
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
                    if let Some(link) = &slot.link {
                        link.config_grants.store(grants.bits(), Ordering::Relaxed);
                        link.update_options(slot.effective_options());
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
        let sup = manifest.as_ref().map(|m| {
            let mut sup = self.supervisor(id, m);
            sup.set_wanted(true, Instant::now());
            sup
        });
        Slot {
            manifest,
            error,
            grants,
            options,
            state,
            restarts: 0,
            sup,
            link: None,
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
            let Some(sup) = &mut slot.sup else {
                continue;
            };
            match sup.tick(now) {
                Some(Event::Exited { status, next }) => {
                    log!("扩展 {id} 退出: {status}");
                    if let Some(link) = slot.link.take() {
                        link.queue.close();
                    }
                    self.on_exit(id, slot, next);
                    changed = true;
                }
                Some(Event::PollFailed(e)) => log!("查询扩展 {id} 状态失败: {e}"),
                Some(Event::Started {
                    stdin,
                    stdout,
                    warnings,
                }) => {
                    for w in warnings {
                        log!("扩展 {id} {w}");
                    }
                    log!("扩展 {id} 已启动");
                    self.attach(slot, stdin, stdout);
                    slot.state = State::Running;
                    slot.error = None;
                    changed = true;
                }
                Some(Event::StartFailed { error, next }) => {
                    log!("扩展 {id} 启动失败: {error}");
                    slot.error = Some(error);
                    self.on_exit(id, slot, next);
                    changed = true;
                }
                None => {
                    if slot.link.as_ref().is_some_and(|l| l.stalled(now)) {
                        log!("扩展 {id} 超过 {}s 不读事件，结束它", STALL.as_secs());
                        sup.kill();
                    }
                }
            }
        }
        changed
    }

    fn on_exit(&self, id: &str, slot: &mut Slot, next: Decision) {
        match next {
            Decision::RestartAfter(_) => {
                slot.state = State::Backoff;
                slot.restarts += 1;
            }
            Decision::Tripped => {
                log!("扩展 {id} 频繁退出，已熔断");
                slot.state = State::Tripped;
                if slot.error.is_none() {
                    slot.error = Some("短时间内多次异常退出，已停止重启".into());
                }
            }
        }
    }

    /// 扩展进程的监护器：命令、日志、Job 都在这里配好（Job 在监护器里先于调用方下发任何东西）。
    fn supervisor(&self, id: &str, m: &Manifest) -> Supervisor {
        let (id, exe, args, dir) = (id.to_string(), m.exe.clone(), m.args.clone(), m.dir.clone());
        let log = self
            .opts
            .log_dir
            .as_ref()
            .map(|d| d.join(format!("extension-{id}.log")));
        Supervisor::new(move || {
            if let Err(e) = ensure_owner_exec(&exe) {
                log!("给扩展 {id} 的本体补可执行位失败: {e}");
            }
            let mut cmd = Command::new(&exe);
            cmd.args(&args)
                .current_dir(&dir)
                .env("WIND_EXTENSION_ID", &id);
            wind_supervise::die_with_parent_on(&mut cmd);
            Ok(cmd)
        })
        .with_log(log, LOG_MAX)
        .with_job(self.job.clone())
    }

    /// 扩展进程已拉起：建链路、起读写线程。
    fn attach(&self, slot: &mut Slot, stdin: ChildStdin, stdout: ChildStdout) {
        let Some(m) = slot.manifest.as_ref() else {
            return;
        };
        let link = Arc::new(Link::new(slot.grants, slot.effective_options()));
        spawn_writer(&m.id, link.clone(), stdin);
        spawn_reader(
            &m.id,
            link.clone(),
            stdout,
            self.opts.core_version.clone(),
            m.protocol.clone(),
            self.last_mode.clone(),
            self.last_presence.clone(),
        );
        slot.link = Some(link);
    }

    fn send_status(&self) {
        let extensions = lock(&self.slots)
            .iter()
            .map(|(id, s)| ExtensionStatus {
                id: id.clone(),
                state: s.state.name().into(),
                restarts: s.restarts,
                dropped: s.link.as_ref().map_or(0, |l| l.queue.total_dropped()),
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
        let handles: Vec<_> = lock(&self.slots)
            .values_mut()
            .filter_map(|s| {
                // 关队列让写线程退出（扩展随之读到 EOF），宽限期后仍未退出就强杀。
                if let Some(link) = s.link.take() {
                    link.queue.close();
                }
                s.sup.as_mut().and_then(|p| p.stop_background(STOP_GRACE))
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }
    }
}

/// Unix 上给扩展本体补 owner 可执行位：从 Windows 同步来的文件不带位，否则拉不起来。
#[cfg(unix)]
fn ensure_owner_exec(exe: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(exe)?.permissions().mode();
    if mode & 0o100 == 0 {
        std::fs::set_permissions(exe, std::fs::Permissions::from_mode(mode | 0o100))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_exec(_exe: &std::path::Path) -> io::Result<()> {
    Ok(())
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
    last_presence: Arc<Mutex<Option<Msg>>>,
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
                        if let Some(m) = lock(&last_presence).clone() {
                            link.queue.push_keep(m);
                        }
                        link.send_options();
                        // 最后发 started：此时扩展已拿到授权、当前状态与选项，可以开始工作。
                        link.queue.push_keep(lifecycle_msg(lifecycle::STARTED));
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
