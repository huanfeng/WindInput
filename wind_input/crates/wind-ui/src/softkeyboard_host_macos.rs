//! 软键盘面板的 macOS 主线程宿主。
//!
//! 它解决的是线程问题：面板（[`SoftKeyboard`]）必须住在主线程（AppKit 只能在主线程改动，
//! 且 `SoftKeyboard` 内部是非 `Send` 的 `Rc<RefCell<..>>`），而下发命令的
//! `manager_macos::forwarder_thread` 是工作线程，既不能碰 AppKit 也不驱动面板的 `tick`。
//! 形制照抄 [`crate::global_hotkey_macos`]（同一个主线程、同一套「入队 + 唤醒源」约定）：
//!
//! 1. 转运：forwarder 线程调 [`apply`] 只把命令塞进 `PENDING` 并戳唤醒源，建窗/绘制在主线程
//!    的 perform 回调里做。
//! 2. 驱动：面板可见期间挂一个 `CFRunLoopTimer` 周期性调 `tick()`（长按重复、修饰键跟随、
//!    按键高亮熄灭），面板一关就撤掉定时器。
//!
//! [`install_on_main`] 必须由主线程在进主事件循环（`global_hotkey_macos::run_main_loop`）之前
//! 调用一次。在那之前 forwarder 发过的命令留在 `PENDING` 里，装配时 drain 一次补上。

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use core_foundation_sys::base::CFIndex;
use core_foundation_sys::date::CFAbsoluteTimeGetCurrent;
use core_foundation_sys::runloop::{
    CFRunLoopAddSource, CFRunLoopAddTimer, CFRunLoopGetMain, CFRunLoopSourceContext,
    CFRunLoopSourceCreate, CFRunLoopSourceRef, CFRunLoopSourceSignal, CFRunLoopTimerContext,
    CFRunLoopTimerCreate, CFRunLoopTimerInvalidate, CFRunLoopTimerRef, CFRunLoopWakeUp,
    kCFRunLoopCommonModes,
};

use crate::soft_keyboard::SoftKeyboard;
use wind_ui_types::{SoftKeyCap, UiEvent};

/// 面板 `tick` 的驱动周期（毫秒）。
///
/// 比面板里更慢的两个节奏（修饰键轮询 `MODIFIER_POLL_MS`、长按重复 `REPEAT_RATE_MS`）都快
/// 一档，才不会被采样节拍拖出可见迟滞。
///
/// 这是「UI 事件驱动、不做空转轮询」的有意例外，只在面板可见时；面板一关 [`sync_timer`]
/// 立刻撤掉定时器。
const TICK_MS: f64 = 0.020;

/// 转运给主线程的软键盘命令。
///
/// 刻意不直接传 `UiCommand`：本模块只关心其中几个变体，用它做队列元素会让读者以为这里
/// 还会处理别的。字段与 `UiCommand` 的对应变体逐位一致。
pub enum SkCmd {
    Show {
        pages: Vec<String>,
        current: usize,
        keys: Vec<SoftKeyCap>,
        send_keys: bool,
    },
    Hide,
    KeyState {
        slot: String,
        down: bool,
    },
    Layer {
        shift: bool,
    },
    Pinned(bool),
    /// 主题变更。
    ///
    /// 面板惰性创建，而 `SetTheme` 一般只在启动时发一次，故在本模块留住最近一份主题给它补课
    /// （同 Windows 侧 `manager.rs` 的 `last_theme`）。
    Theme(Box<wind_theme::Resolved>),
}

/// 待主线程处理的命令队列。
static PENDING: Mutex<Vec<SkCmd>> = Mutex::new(Vec::new());
/// 上行事件通道。面板的点击/翻页/关闭经它回协调器。
static EV_TX: Mutex<Option<Sender<UiEvent>>> = Mutex::new(None);

/// 裸指针的跨线程包装。
///
/// 只用于把「主线程建好的唤醒源」交给别的线程去 `signal`——`CFRunLoopSourceSignal`
/// 与 `CFRunLoopWakeUp` 本身是线程安全的（这正是 CF 给出这两个函数的理由），
/// 与 `global_hotkey_macos::SendPtr` 同源同理。
struct SendPtr<T>(*mut T);
// SAFETY: 见上。指针指向的 CFRunLoopSource 由主线程创建并持有到进程结束，
// 别的线程只对它调那两个明确声明为线程安全的函数。
unsafe impl<T> Send for SendPtr<T> {}
unsafe impl<T> Sync for SendPtr<T> {}

static WAKE_SOURCE: OnceLock<SendPtr<c_void>> = OnceLock::new();

thread_local! {
    /// 面板本体。只在主线程访问（`thread_local` 把这条约定焊进类型系统之外的另一层：
    /// 别的线程即使拿到本模块也只会看到一个空 `RefCell`，而不是数据竞争）。
    static PANEL: RefCell<Option<SoftKeyboard>> = const { RefCell::new(None) };
    /// 最近一份主题，给惰性建窗补课用。见 [`SkCmd::Theme`]。
    static THEME: RefCell<Option<wind_theme::Resolved>> = const { RefCell::new(None) };
    /// 面板可见期间的 tick 定时器。`None` = 未挂（面板不可见）。
    static TIMER: RefCell<Option<CFRunLoopTimerRef>> = const { RefCell::new(None) };
}

/// 从任意线程提交一条软键盘命令。
///
/// 只入队 + 唤醒，不碰 AppKit。主线程尚未 [`install_on_main`] 时也可调用——命令留在
/// `PENDING` 里，等装配后的第一次 drain 一并处理。
pub fn apply(cmd: SkCmd, ev_tx: &Sender<UiEvent>) {
    {
        let mut tx = EV_TX.lock().unwrap_or_else(|e| e.into_inner());
        if tx.is_none() {
            *tx = Some(ev_tx.clone());
        }
    }
    {
        let mut q = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        // 连续的 Show 只留最后一条：长按翻页会连发，中间那些帧画出来也立刻被盖掉。
        // 与 `manager.rs` 主循环合并连续 `UpdateCandidates` 是同一个理由。
        if matches!(cmd, SkCmd::Show { .. }) {
            q.retain(|c| !matches!(c, SkCmd::Show { .. }));
        }
        q.push(cmd);
    }
    if let Some(src) = WAKE_SOURCE.get() {
        unsafe {
            CFRunLoopSourceSignal(src.0 as CFRunLoopSourceRef);
            CFRunLoopWakeUp(CFRunLoopGetMain());
        }
    }
}

/// 主线程装配：建唤醒源、挂上主 run loop，并补做一次 drain。
///
/// 必须在主线程、且在进主事件循环之前调用一次。
pub fn install_on_main() {
    if WAKE_SOURCE.get().is_some() {
        return;
    }
    unsafe {
        // 逐字段构造，不能用 `mem::zeroed()`：`perform` 是非空函数指针，全零是非法值，
        // 运行时直接 abort 且编译看不出来。
        let mut ctx = CFRunLoopSourceContext {
            version: 0 as CFIndex,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: wake_perform,
        };
        let src = CFRunLoopSourceCreate(std::ptr::null(), 0 as CFIndex, &mut ctx);
        if src.is_null() {
            tracing::error!("软键盘: CFRunLoopSourceCreate 失败，面板将无法显示");
            return;
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), src, kCFRunLoopCommonModes);
        let _ = WAKE_SOURCE.set(SendPtr(src as *mut c_void));
    }
    tracing::info!("软键盘: 主线程宿主已装配");
    // 装配前 forwarder 可能已经发过命令（协调器构造期就会推一次配置）。
    drain_pending();
}

/// 唤醒源回调（主线程）。
extern "C" fn wake_perform(_info: *const c_void) {
    drain_pending();
}

/// tick 定时器回调（主线程）。
extern "C" fn tick_fire(_timer: CFRunLoopTimerRef, _info: *mut c_void) {
    with_panel(|k| k.tick());
    // `tick` 里的点击派发可能触发关闭（面板上的 × 或 Esc 对应的上行事件走的是协调器，
    // 但换面/关闭最终会回到本模块），故每轮都核一次可见性。
    sync_timer();
}

/// 取走队列并逐条应用。只在主线程调用。
fn drain_pending() {
    loop {
        let batch: Vec<SkCmd> = {
            let mut q = PENDING.lock().unwrap_or_else(|e| e.into_inner());
            if q.is_empty() {
                break;
            }
            std::mem::take(&mut *q)
        };
        for cmd in batch {
            handle(cmd);
        }
    }
    sync_timer();
}

fn handle(cmd: SkCmd) {
    match cmd {
        SkCmd::Theme(t) => {
            THEME.with(|c| *c.borrow_mut() = Some((*t).clone()));
            with_panel(|k| k.set_theme(&t));
        }
        SkCmd::Show {
            pages,
            current,
            keys,
            send_keys,
        } => {
            if !ensure_panel() {
                return;
            }
            with_panel(move |k| k.show(pages, current, keys, send_keys));
        }
        SkCmd::Hide => with_panel(|k| k.hide()),
        SkCmd::KeyState { slot, down } => with_panel(|k| k.set_key_down(&slot, down)),
        SkCmd::Layer { shift } => with_panel(|k| k.set_layer(shift)),
        SkCmd::Pinned(pinned) => with_panel(|k| k.set_pinned(pinned)),
    }
}

/// 借出面板做一件事。借不到就跳过。
///
/// 用 `try_borrow_mut`（同 `mac_panel::PanelView::dispatch`）：某些 AppKit 调用会重入
/// run loop，可能在面板已借出时再进来；panic 会带走整个服务，跳过只少画一帧。
fn with_panel(f: impl FnOnce(&mut SoftKeyboard)) {
    PANEL.with(|p| match p.try_borrow_mut() {
        Ok(mut g) => {
            if let Some(k) = g.as_mut() {
                f(k);
            }
        }
        Err(_) => tracing::debug!("软键盘: 面板正被占用，本次操作跳过（run loop 重入）"),
    });
}

/// 惰性建窗。返回面板是否可用。
///
/// 建窗失败只记 error 并返回 false，不 panic（同 `manager.rs` 对惰性窗口的 best-effort）。
fn ensure_panel() -> bool {
    if PANEL.with(|p| p.try_borrow().is_ok_and(|p| p.is_some())) {
        return true;
    }
    let Some(tx) = EV_TX.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        tracing::error!("软键盘: 事件通道尚未就绪，本次显示请求已丢弃");
        return false;
    };
    match SoftKeyboard::new(tx) {
        Ok(mut k) => {
            // 惰性建窗补课：见 `SkCmd::Theme`。
            THEME.with(|c| {
                if let Some(t) = c.borrow().as_ref() {
                    k.set_theme(t);
                }
            });
            PANEL.with(|p| match p.try_borrow_mut() {
                Ok(mut g) => {
                    *g = Some(k);
                    true
                }
                Err(_) => {
                    tracing::warn!("软键盘: 面板正被占用，建窗结果已丢弃（run loop 重入）");
                    false
                }
            })
        }
        Err(e) => {
            tracing::error!("软键盘: 面板窗口创建失败: {e}");
            false
        }
    }
}

/// 按面板当前可见性挂上/撤掉 tick 定时器。
///
/// 幂等，每次处理完命令与每轮 tick 后都调一次：显隐有好几条来路，让每条来路各记一次
/// 注定要漏。
fn sync_timer() {
    let visible = PANEL.with(|p| {
        p.try_borrow()
            .is_ok_and(|p| p.as_ref().is_some_and(|k| k.is_visible()))
    });
    TIMER.with(|t| {
        // 这里可用裸 `borrow_mut`：借出期间只调 CFRunLoopTimer 的创建/挂载/失效，都不驱动
        // run loop，不会重入。若引入会转 run loop 的调用，得换成 try_borrow_mut。
        let mut t = t.borrow_mut();
        match (visible, *t) {
            (true, None) => unsafe {
                let mut ctx = CFRunLoopTimerContext {
                    version: 0 as CFIndex,
                    info: std::ptr::null_mut(),
                    retain: None,
                    release: None,
                    copyDescription: None,
                };
                let timer = CFRunLoopTimerCreate(
                    std::ptr::null(),
                    CFAbsoluteTimeGetCurrent() + TICK_MS,
                    TICK_MS,
                    0,
                    0,
                    tick_fire,
                    &mut ctx,
                );
                if timer.is_null() {
                    tracing::error!("软键盘: tick 定时器创建失败，长按与切层将不跟手");
                    return;
                }
                CFRunLoopAddTimer(CFRunLoopGetMain(), timer, kCFRunLoopCommonModes);
                *t = Some(timer);
            },
            (false, Some(timer)) => unsafe {
                // Invalidate 会把它从 run loop 摘掉并释放我们持有的那一份引用。
                CFRunLoopTimerInvalidate(timer);
                *t = None;
            },
            _ => {}
        }
    });
}
