//! 前台全屏形态的周期复查（Windows 专属），补上「进出全屏不产生任何回调」的盲区。
//!
//! coordinator 的子模块：要读写父模块的私有态（单飞闸、`state` 里的三项合取）。
//!
//! `fullscreen_cached` 原本只由 [`Coordinator::notify_toolbar_async`] 在四个 TSF 焦点/激活事件上
//! 刷新，而进出全屏（F11、视频全屏、双击播放器）焦点不动、系统也不回调，缓存会停在旧值。
//! 是否恰好有焦点事件捎带刷新不归我们管，所以同一份代码在不同系统上表现不同。
//!
//! 常驻开销靠三道闸压到近零：
//! 1. 工具栏没显示时线程整个挂起（[`park_until_toolbar_shown`]），由 `notify_toolbar` 叫醒，
//!    另有 [`PARK_FALLBACK`] 兜底防信号漏发。
//! 2. 每拍只做本地调用：探 `foreground_covers_own_monitor` 而非 `foreground_fullscreen_kind`，
//!    后者带一次 `SHQueryUserNotificationState` 跨进程 RPC，按节拍反复问的东西不能带 RPC。
//! 3. 因此只写 `fullscreen_cached`，不碰 `fullscreen_exclusive_cached`：判据①没问就不能拿来清它。
//!    独占全屏由 UI 线程弹窗时按需问（`exclusive_fullscreen_recent`，300ms TTL）。
//!
//! 不用 WinEvent 钩子：`EVENT_SYSTEM_FOREGROUND` 在同一 HWND 进出全屏时不发；
//! `EVENT_OBJECT_LOCATIONCHANGE` 的 out-of-context 钩子会让 Chromium / Firefox 进入完整无障碍模式，
//! 整个浏览器变慢。
//!
//! 也不顺带判「宿主还在不在前台」：那由 `ITfThreadFocusSink` 负责，且不能用前台窗口 pid 兜底——
//! 多进程宿主（WebView 类）下该判据恒假，会让工具栏永不显示。见
//! `docs/architecture/tsf-docmgr-focus-semantics.md` §3.5。

use super::*;

/// 醒着时的复查周期。进出全屏是秒级动作，两拍确认后最迟 1s 跟上；每拍只有本地 Win32 调用。
#[cfg_attr(not(windows), allow(dead_code))]
const WATCH_TICK: std::time::Duration = std::time::Duration::from_millis(500);

/// 挂起时的兜底醒来间隔。正常唤醒走 [`wake`]，这条只防信号漏发：漏了最多晚 10s 恢复，
/// 代价是每 10 秒一次空醒。
#[cfg_attr(not(windows), allow(dead_code))]
const PARK_FALLBACK: std::time::Duration = std::time::Duration::from_secs(10);

/// 一次性唤醒信号：`wake` 置位，`park` 等到置位（或超时）后**消费掉**它。
///
/// 抽成类型是为了让测试各用各的实例：共用进程级 static 时并发测试会互相叫醒，
/// 「丢唤醒」的变异照样全绿。
struct WakeChannel {
    woken: std::sync::Mutex<bool>,
    cv: std::sync::Condvar,
}

impl WakeChannel {
    const fn new() -> Self {
        Self {
            woken: std::sync::Mutex::new(false),
            cv: std::sync::Condvar::new(),
        }
    }

    fn wake(&self) {
        *self.woken.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_one();
    }

    /// 等到被叫醒或 `fallback` 超时，返回前把信号消费掉。
    ///
    /// 非 Windows 下只有测试调用它，故需 `allow(dead_code)`。
    ///
    /// 消费点必须在醒来之后而非进门处：读闸与拿锁之间若到达 [`Self::wake`]，进门清零会抹掉它，
    /// 线程要等兜底才醒，而那正是工具栏刚亮、最该开工的时刻。留着标志最多多空转一圈。
    #[cfg_attr(not(windows), allow(dead_code))]
    fn park(&self, fallback: std::time::Duration) {
        let mut woken = self.woken.lock().unwrap_or_else(|e| e.into_inner());
        while !*woken {
            let (g, timeout) = self
                .cv
                .wait_timeout(woken, fallback)
                .unwrap_or_else(|e| e.into_inner());
            woken = g;
            if timeout.timed_out() {
                break;
            }
        }
        *woken = false;
    }
}

/// 生产用实例。放进程级 static 而非 Coordinator 字段：挂起中的线程不能持有
/// `Arc<Coordinator>`，否则协调器永远析构不掉。
static WAKE: WakeChannel = WakeChannel::new();

/// 叫醒复查线程：`notify_toolbar` 在闸（三项合取）成立时调，显示与隐藏两个分支都调。
///
/// 判据是闸而不是「工具栏要不要显示」：工具栏正因全屏隐藏时闸也可能刚打开，按「要显示」判
/// 就发不出信号，退出全屏后要等 [`PARK_FALLBACK`] 才恢复。见
/// `watch_gate_stays_open_while_hidden_by_fullscreen`。
pub(crate) fn wake() {
    WAKE.wake();
}

/// 挂起到「工具栏显示了」或兜底超时。不持任何 `Arc<Coordinator>`。
#[cfg(windows)]
fn park_until_toolbar_shown() {
    WAKE.park(PARK_FALLBACK);
}

/// 两拍确认：连续两次采到同一个值才认，返回是否应当提交本次采样。
///
/// 判据②把「问不出前台窗口」和「前台是 shell 壳 UI」都归为非全屏，事件驱动下无害，按拍采样下
/// 却会因任务栏、通知、切换器一闪而让工具栏弹到全屏画面上。代价是真实变化多等一拍（≤1s），
/// 比反复闪一下轻得多。
#[cfg_attr(not(windows), allow(dead_code))]
fn confirmed(last: &mut Option<bool>, sample: bool) -> bool {
    let repeated = *last == Some(sample);
    *last = Some(sample);
    repeated
}

/// 复查该不该工作：两项都要。父项「全屏时隐藏工具栏」关着时 `fullscreen_cached` 没有读者，
/// 设置页也按父项把本项置灰（`enabled_when`），core 须同样合取。
#[cfg_attr(not(windows), allow(dead_code))]
fn watch_enabled(toolbar: &wind_config::config::ToolbarConfig) -> bool {
    toolbar.hide_in_fullscreen && toolbar.fullscreen_watch
}

#[cfg(windows)]
impl Coordinator {
    /// 懒启动全屏复查线程，重复调用只起一条。
    ///
    /// 起点在第一次焦点/激活事件（[`Coordinator::notify_toolbar_async`]）而非构造函数：
    /// 命令行子命令与 headless 用法没有焦点事件，不该为它们留线程。
    pub(crate) fn ensure_fullscreen_watch(&self) {
        // 运行时关 → 线程下一拍自行退出；开 → 热重载再调本函数，线程退出时 `Alive` 也补一次。
        if !watch_enabled(&self.rt().config.ui.toolbar) {
            return;
        }
        if self
            .fullscreen_watch_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        // 拿不到自身 Weak 说明还在构造中途：放回标志，下一次事件再试。
        let Some(weak) = self.self_weak.get().cloned() else {
            self.fullscreen_watch_started
                .store(false, std::sync::atomic::Ordering::Release);
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("fullscreen-watch".into())
            .spawn(move || {
                // 线程任何退出路径都放回「已启动」标志，并按最新配置补起一条：热重载那次
                // `ensure_fullscreen_watch` 若撞上本线程正在退出，会看到标志仍为 true 而返回。
                // 不覆盖 panic（release 是 `panic = "abort"`，Drop 不执行）。
                struct Alive(std::sync::Weak<Coordinator>);
                impl Drop for Alive {
                    fn drop(&mut self) {
                        if let Some(c) = self.0.upgrade() {
                            c.fullscreen_watch_started
                                .store(false, std::sync::atomic::Ordering::Release);
                            c.ensure_fullscreen_watch();
                        }
                    }
                }
                let _alive = Alive(weak.clone());
                // 两拍确认用的上一拍采样，见 `confirmed`。
                let mut last: Option<bool> = None;
                // 闸的开合只在翻转时记一行，诊断用且不刷屏。
                let mut gate_open: Option<bool> = None;
                loop {
                    // 每拍先放掉 Arc 再挂起/睡眠，否则协调器析构不掉；两个 `upgrade` 作用域刻意收紧。
                    let open = match weak.upgrade() {
                        Some(c) => {
                            // 运行时被关掉则退出，`Alive` 负责收口。
                            if !watch_enabled(&c.rt().config.ui.toolbar) {
                                debug!("全屏复查已关闭（开关或父项），线程退出");
                                return;
                            }
                            c.toolbar_wants_display()
                        }
                        None => break, // 协调器已析构 ⇒ 线程退出
                    };
                    if gate_open != Some(open) {
                        gate_open = Some(open);
                        debug!(
                            "全屏复查{}（工具栏三项合取）",
                            if open { "开始" } else { "挂起" }
                        );
                    }
                    if !open {
                        last = None; // 挂起期间的历史作废，重新开工后从头确认两拍
                        park_until_toolbar_shown();
                        continue;
                    }
                    let sample = wind_keys::foreground::foreground_covers_own_monitor();
                    if confirmed(&mut last, sample) {
                        let Some(c) = weak.upgrade() else { break };
                        // 单飞：焦点事件那条探测在途时跳过本拍，两者写同一组缓存。
                        if c.try_take_probe_gate() {
                            let _gate = c.adopt_probe_gate();
                            c.commit_fullscreen_covering(sample);
                        }
                    }
                    std::thread::sleep(WATCH_TICK);
                }
                // 只有 `weak.upgrade()` 失败会走到这里；配置关闭那条路已在上面 `return`。
                debug!("fullscreen-watch 线程退出（协调器已析构）");
            });
        if spawned.is_err() {
            // 线程没起来就把标志放回去，否则此后永远不再尝试启动。
            self.fullscreen_watch_started
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

/// 非 Windows：空实现（同名函数并列 cfg，见 AGENTS.md 平台分层）。
#[cfg(not(windows))]
impl Coordinator {
    pub(crate) fn ensure_fullscreen_watch(&self) {}
}

#[cfg(test)]
#[path = "fullscreen_watch_tests.rs"]
mod tests;
