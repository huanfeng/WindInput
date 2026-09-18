//! 前台全屏形态的**周期复查**（Windows 专属）——补上「进出全屏不产生任何回调」这个
//! 结构性盲区。
//!
//! coordinator 的子模块（非平级）：要读写父模块的私有态（单飞闸、`state` 里的三项合取），
//! 归属判据同 [`super::first_show`]。
//!
//! ## 为什么需要它
//!
//! 本模块出现之前，`fullscreen_cached` / `fullscreen_exclusive_cached` 的唯一刷新者是
//! [`Coordinator::notify_toolbar_async`]，而它的调用点全集是四个 TSF 事件
//! （focus_gained / focus_lost / ime_activated / ime_deactivated）。**进出全屏不属于这四个
//! 里的任何一个**：在浏览器里点全屏播放视频、按 F11、在播放器里双击——焦点自始至终没动，
//! 系统也不回调输入法任何东西 ⇒ 缓存停在进全屏之前的值，工具栏该隐没隐（GH#134）。
//! 反向同样盲：退出全屏后工具栏该恢复，也得等下一次焦点事件。
//!
//! ⚠ GH#134 报的是「Win11 正常、Win10 恒显示」，但那**不说明两边的全屏判据不同**：判据
//! 本身（`foreground.rs` 那两条）不含任何版本分支，用到的 API 也都不分版本。真正决定
//! 结果的是「全屏前后有没有一个焦点事件恰好到达」把缓存捎带刷新掉——**有没有那个事件
//! 不归我们管**，于是同一份代码在两个系统上表现不同。
//!
//! ## 三道闸把「常驻开销」压到近乎为零
//!
//! 轮询在本仓一直是被拒绝的方案，理由是「这个功能不值得常驻 CPU」。那条反对成立，因此
//! 本模块的形状完全是围着它长的——不是「轮询 + 尽量省」，而是**默认停住、被叫醒才跑**：
//!
//! 1. **工具栏没显示时线程整个挂起**（[`park_until_toolbar_shown`]），不是空转睡醒。
//!    唤醒者是 `notify_toolbar` 决定显示工具栏的那一刻。用户没在输入框里的绝大部分时间，
//!    本线程的唤醒次数是 **0**（另有 [`PARK_FALLBACK`] 的长兜底，防信号漏发，见那里）。
//! 2. **醒着时每拍只做本地调用**：探的是 `foreground_covers_own_monitor` 而不是
//!    `foreground_fullscreen_kind` —— 后者带一次 `SHQueryUserNotificationState` 跨进程问
//!    shell，那是这组查询里唯一的 RPC。**要按固定节拍反复问的东西不能带 RPC**。剩下的
//!    GetForegroundWindow / GetWindowRect / GetMonitorInfo 都是本地调用。
//! 3. 因此本模块只写 `fullscreen_cached`（工具栏那一格），**不碰**
//!    `fullscreen_exclusive_cached`：判据①既然不问，就不能拿「没问到」去清它。独占全屏
//!    那一侧另有 UI 线程在真要弹浮窗的那一刻问的按需闸（`exclusive_fullscreen_recent`，
//!    300ms TTL），它才是候选窗的权威。
//!
//! ## 为什么不是 WinEvent 钩子（那才是真正贵的那条）
//!
//! `EVENT_SYSTEM_FOREGROUND` 恰恰不覆盖本场景——全屏前后是**同一个** HWND 在前台，它不发。
//! 能覆盖窗口尺寸变化的只有 `EVENT_OBJECT_LOCATIONCHANGE`，而给它挂 out-of-context 钩子
//! 会被 Chromium / Firefox 判定为「有辅助技术在监听」，**整个浏览器进入完整无障碍模式**
//! （已知的全局性能损失）。为了省掉每秒两次本地调用而让用户的浏览器整体变慢，方向是反的。
//!
//! ## 为什么不顺手把「宿主还在不在前台」也一起判了
//!
//! 那一路已有 `ITfThreadFocusSink` 负责（实测 Chrome 5/5、VSCode 5/5、Edge 11/11 零漏），
//! 且**不能**用「前台窗口的 pid 是不是宿主」去兜底：多进程宿主（WebView 类，前台窗口在
//! 一个进程、TSF 加载在另一个）下该判据恒假，拿它关 UI 会让工具栏在这类宿主里**永不显示**。
//! 本仓为此翻过一次车，记录见 `docs/architecture/tsf-docmgr-focus-semantics.md` §3.5。

use super::*;

/// 醒着时的复查周期。
///
/// 500ms 的依据：进出全屏是秒级的用户动作，两拍确认后最迟 1s 跟上，观感上仍属「跟手」。
/// 这一拍只做几个本地 Win32 调用（见模块文档第 2 条），**没有**跨进程往返。
#[cfg_attr(not(windows), allow(dead_code))]
const WATCH_TICK: std::time::Duration = std::time::Duration::from_millis(500);

/// 挂起时的兜底醒来间隔。
///
/// 正常唤醒走 [`wake`]（`notify_toolbar` 显示工具栏时发），这条只防「信号漏发」这一类
/// 未知失效：真漏了也最多晚 10s 恢复，而不是永久瞎掉。代价是每 10 秒一次空醒
/// （读一个布尔就继续挂起），量级上等于没有。
#[cfg_attr(not(windows), allow(dead_code))]
const PARK_FALLBACK: std::time::Duration = std::time::Duration::from_secs(10);

/// 一次性唤醒信号：`wake` 置位，`park` 等到置位（或超时）后**消费掉**它。
///
/// 抽成类型而不是裸的 `(Mutex, Condvar)`，是为了让测试能各用各的实例。共用进程级 static
/// 的话，并发跑的两条测试会互相把对方叫醒——实测如此：拿「丢唤醒」那个缺陷做变异时，两条
/// 唤醒测试**双双照绿**，因为一条的 `wake()` 顺手放行了另一条的 `park()`。
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
    /// 非 Windows 下唯一的调用者是本模块的测试（生产调用点 `park_until_toolbar_shown` 整个
    /// 是 `cfg(windows)` 的），故 lib 单独编译时它无人使用 —— 而 `-D warnings` 下那就是
    /// 编译失败。与 `wind-keys` 的 `rect_covers` 同一处置：跨平台跑的测试仍要它，不能删。
    ///
    /// ⚠ 消费点在**醒来之后**，不能放在进门处。线程读闸（`toolbar_wants_display`）与走到
    /// 这里拿锁之间有一个窗口，进门清零会把落在窗口里的 [`Self::wake`] 抹掉，线程照样挂起、
    /// 要等兜底才醒——而那个窗口恰好是「用户刚点进输入框、工具栏刚亮起来」，最该立刻开工
    /// 的一刻。留着标志则最多多空转一圈（立刻返回 → 重读闸 → 再挂起）。
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

/// 生产用的那一个。进程级 static 而不是 Coordinator 的字段——挂起中的线程**不能**持有
/// `Arc<Coordinator>`，否则协调器永远析构不掉（线程就是那个多出来的强引用）。
static WAKE: WakeChannel = WakeChannel::new();

/// 叫醒复查线程：`notify_toolbar` 在**闸（三项合取）成立**时调，两个分支都调。
///
/// ⚠ 判据是闸，不是「工具栏要不要显示」。两者只差全屏那一项，而那一格恰恰是唯一要紧的：
/// 工具栏正因全屏隐藏、此刻三项合取由假转真（用户点进全屏页面里的搜索框），若按「要显示」
/// 判就发不出信号，线程要等 [`PARK_FALLBACK`] 才醒 —— 用户退出全屏后十来秒工具栏才回来。
/// 「隐藏 ⇒ 闸随后会关」这个想当然，正是 `watch_gate_stays_open_while_hidden_by_fullscreen`
/// 钉着要否掉的。
pub(crate) fn wake() {
    WAKE.wake();
}

/// 挂起到「工具栏显示了」或兜底超时。**不持任何 `Arc<Coordinator>`**。
#[cfg(windows)]
fn park_until_toolbar_shown() {
    WAKE.park(PARK_FALLBACK);
}

/// 两拍确认：连续两次采到同一个值才认，返回是否应当提交本次采样。
///
/// 为什么轮询路径需要它而事件路径不需要：判据②把「问不出前台窗口」与「前台是 shell 的
/// 壳 UI」都归为**非全屏**（`foreground.rs` 的 `foreground_window` 显式排除桌面/Shell，
/// `window_covers_monitor` 的守卫②排除 explorer 承载的铺满窗口）。那在事件驱动下是安全
/// 兜底——探测只发生在焦点刚变的那一刻，本来就要重算；但在按拍采样下它变成一个翻转源：
/// 全屏看视频时鼠标扫过任务栏、通知弹一下、切换器一闪，都可能让某一拍读到「非全屏」
/// ⇒ 工具栏弹到全屏画面上，下一拍再收回去。
///
/// 代价是真实变化要多等一拍（≤1s）。这个方向的错（晚 500ms 隐藏）比另一个方向（每隔
/// 几拍闪一下）轻得多。
#[cfg_attr(not(windows), allow(dead_code))]
fn confirmed(last: &mut Option<bool>, sample: bool) -> bool {
    let repeated = *last == Some(sample);
    *last = Some(sample);
    repeated
}

/// 复查该不该工作：它服务的是工具栏的全屏否决，故**两项都要**——用户关掉「全屏时隐藏
/// 工具栏」时，`fullscreen_cached` 根本没有生产读者（见 `notify_toolbar` 的 `hide_fullscreen`），
/// 再探就是纯粹的无效功；而设置页那一项正是按这条门控置灰的（`enabled_when`），若 core
/// 不跟着合取，用户在父项关着时既关不掉子项、线程又照跑，与开关的初衷相反。
#[cfg_attr(not(windows), allow(dead_code))]
fn watch_enabled(toolbar: &wind_config::config::ToolbarConfig) -> bool {
    toolbar.hide_in_fullscreen && toolbar.fullscreen_watch
}

#[cfg(windows)]
impl Coordinator {
    /// 懒启动全屏复查线程，重复调用只起一条。
    ///
    /// 起点选在第一次焦点/激活事件（[`Coordinator::notify_toolbar_async`]）而不是构造函数：
    /// 命令行子命令与 headless 用法从头到尾没有焦点事件，不该为它们留一条线程；而只要
    /// 发生过一次焦点事件，此后就一直需要它（多数时间它挂着，见模块文档）。
    pub(crate) fn ensure_fullscreen_watch(&self) {
        // 关着就连线程都不起（判据见 `watch_enabled`）。运行时改配置的两个方向都要能生效：
        // 关 → 线程在下一拍读到配置后自行退出；开 → 配置热重载那条路会再调一次本函数，
        // 而线程退出时 `Alive` 还会补一次（堵住「热重载那次撞上线程正在退出」的窄竞态）。
        if !watch_enabled(&self.rt().config.ui.toolbar) {
            return;
        }
        if self
            .fullscreen_watch_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        // 拿不到自身 Weak 说明还在构造中途——本函数的调用点在焦点事件上，那时必定已就绪；
        // 真取不到就把标志放回去，下一次事件再试。
        let Some(weak) = self.self_weak.get().cloned() else {
            self.fullscreen_watch_started
                .store(false, std::sync::atomic::Ordering::Release);
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("fullscreen-watch".into())
            .spawn(move || {
                // 线程结束（协调器析构、配置关掉、任何早退路径）都把「已启动」标志放回去，
                // 否则此后再没有复查；放回去之后还要按**最新**配置补起一条 —— 堵的是
                // OFF→ON 的窄竞态：热重载那次 `ensure_fullscreen_watch` 若撞上本线程正在
                // 退出，会看到标志还是 true 而提前返回，于是开关显示「开」却没有线程在跑。
                //
                // ⚠ 不覆盖 panic：release 是 `panic = "abort"`，栈不展开、`Drop` 不执行。
                // 这里管的是正常退出路径的收口，不是 panic 恢复。
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
                // 闸的开合只在**翻转时**记一行：既能回答「那台机器上闸到底开没开」
                // （诊断「改了没效果」时的第一个岔路口），又不会刷屏。
                let mut gate_open: Option<bool> = None;
                loop {
                    // ⚠ 每一拍都**先放掉** Arc 再挂起/睡眠：挂起中的线程若攥着强引用，
                    // 协调器就永远析构不掉。下面两个 `upgrade` 各自的作用域刻意收得很紧。
                    let open = match weak.upgrade() {
                        Some(c) => {
                            // 运行时被关掉 ⇒ 线程退出（而不是留一条挂着的空线程）。
                            // `Alive` 的 Drop 会把「已启动」标志放回去并按最新配置补起一条。
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
                        // 单飞：焦点事件那条探测在途时跳过本拍。探的是同一个全局前台
                        // 状态，重复查没有意义，且两者都会写同一组缓存。
                        if c.try_take_probe_gate() {
                            let _gate = c.adopt_probe_gate();
                            c.commit_fullscreen_covering(sample);
                        }
                    }
                    std::thread::sleep(WATCH_TICK);
                }
                // 只有 `weak.upgrade()` 失败那一臂会走到这里；配置关闭那条路在上面直接
                // `return`，各打各的原因，免得一次退出打出两条互相矛盾的日志。
                debug!("fullscreen-watch 线程退出（协调器已析构）");
            });
        if spawned.is_err() {
            // 线程没起来就把标志放回去，否则此后永远不再尝试启动。
            self.fullscreen_watch_started
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

/// 非 Windows：无全屏探测可复查，空实现。
///
/// 写成同名空函数而不是让调用点带 `#[cfg]`，是照 AGENTS.md 平台分层表那条
/// 「同名函数并列 cfg + 兜底，不把 cfg 散进函数体」（先例 `foreground_fullscreen_kind`）。
#[cfg(not(windows))]
impl Coordinator {
    pub(crate) fn ensure_fullscreen_watch(&self) {}
}

#[cfg(test)]
mod tests {
    use super::{WakeChannel, confirmed, watch_enabled};

    /// 测试用的兜底时限：远小于生产的 `PARK_FALLBACK`，免得一条测试跑满十秒。
    const T: std::time::Duration = std::time::Duration::from_millis(400);

    /// ★ 唤醒通道的第一条性质：`wake()` 之后 `park` 必须**立刻**返回。
    ///
    /// 这条断的是「工具栏亮起来了、线程却还在睡」。没有它的话，把 `wake()` 写成空函数、
    /// 或把 `notify_toolbar` 那处调用删掉，都只表现为「偶尔慢十秒」，测试全绿。
    #[test]
    fn a_wake_makes_park_return_immediately() {
        let ch = WakeChannel::new();
        ch.wake();
        let t0 = std::time::Instant::now();
        ch.park(T);
        assert!(
            t0.elapsed() < T / 2,
            "已经叫过了就不该再睡，实测睡了 {:?}",
            t0.elapsed()
        );
    }

    /// ★★ 第二条：**挂起之前**到达的唤醒不得丢失。
    ///
    /// 线程读闸与拿 `WAKE` 锁之间有一个窗口，信号可能落在那里。若把标志的消费放在进门处
    /// （`*woken = false` 写在 `wait` 之前），这个信号会被抹掉、线程照睡到兜底 —— 而那个
    /// 窗口恰好是「用户刚点进输入框」。这里模拟的就是那一格：先 `wake()`（模拟窗口内到达），
    /// 再 `park()`。
    #[test]
    fn a_wake_arriving_before_park_is_not_lost() {
        let ch = WakeChannel::new();
        ch.wake();
        ch.wake(); // 重复叫不叠加，仍是一次性信号
        let t0 = std::time::Instant::now();
        ch.park(T);
        assert!(t0.elapsed() < T / 2, "挂起前到达的信号被吞了");

        // 而且消费掉之后不得有残留：这一次 park 必须真的等下去。
        let t1 = std::time::Instant::now();
        std::thread::scope(|sc| {
            sc.spawn(|| {
                std::thread::sleep(T / 4);
                ch.wake();
            });
            ch.park(T);
        });
        assert!(
            t1.elapsed() >= T / 5,
            "上一次的信号有残留，park 没有真的等，实测只等了 {:?}",
            t1.elapsed()
        );
        assert!(t1.elapsed() < T, "不该等到兜底超时 —— 叫醒信号没送到");
    }

    /// ★ 开关判据必须**两项合取**：父项「全屏时隐藏工具栏」关着时也不该开工。
    ///
    /// 只看 `fullscreen_watch` 的实现会让「父项关掉」这一格照跑线程，而那时
    /// `fullscreen_cached` 根本没有生产读者 —— 纯无效功，且设置页把子项置灰了，用户关不掉。
    #[test]
    fn watching_requires_both_the_parent_and_its_own_switch() {
        let both_on = |hide, watch| wind_config::config::ToolbarConfig {
            hide_in_fullscreen: hide,
            fullscreen_watch: watch,
            ..Default::default()
        };
        let mut t = both_on(true, true);
        assert!(watch_enabled(&t), "两项都开 ⇒ 开工");
        t.fullscreen_watch = false;
        assert!(!watch_enabled(&t), "自己关掉 ⇒ 不开工");
        t.fullscreen_watch = true;
        t.hide_in_fullscreen = false;
        assert!(
            !watch_enabled(&t),
            "父项关掉 ⇒ 同样不开工（设置页正是按这条置灰的）"
        );
        t.fullscreen_watch = false;
        assert!(!watch_enabled(&t), "都关 ⇒ 不开工");
    }

    /// 两拍确认的三条性质：首次采样不认、连续同值才认、中途换值要重新攒。
    ///
    /// 第三条是真正要钉的——只写「连续两次相同」而漏掉「换值时重新计数」的实现，在
    /// 「全屏 / 非全屏 / 全屏 / 非全屏」这种交替抖动下会每隔一拍就认一次，去抖等于没做。
    #[test]
    fn a_flip_needs_two_identical_samples_in_a_row() {
        let mut last = None;
        assert!(!confirmed(&mut last, true), "首次采到的值没有对照，不能认");
        assert!(confirmed(&mut last, true), "连着两拍同值才认");
        assert!(
            !confirmed(&mut last, false),
            "换了值要重新攒两拍，单拍抖动不得通过"
        );
        assert!(
            !confirmed(&mut last, true),
            "又换回去同样不算——交替抖动一拍都不该认"
        );
        assert!(confirmed(&mut last, true), "稳定下来之后照常认");
    }

    /// 挂起期间历史作废（线程里 `last = None`）之后，重新开工的第一拍同样不认——
    /// 否则「挂起前最后一拍」会与「重新开工第一拍」凑成两拍，而这两拍之间隔着整段
    /// 挂起时间，前台早就换了。
    #[test]
    fn history_is_discarded_across_a_park() {
        let mut last = Some(true);
        assert!(confirmed(&mut last, true), "挂起前：两拍已凑齐");
        last = None; // 线程挂起时做的事
        assert!(!confirmed(&mut last, true), "跨挂起不得与挂起前那拍凑对");
        assert!(confirmed(&mut last, true));
    }
}
