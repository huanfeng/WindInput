//! 软键盘面板的 **macOS 主线程宿主**。
//!
//! # 它解决的是线程问题，不是功能问题
//!
//! 面板的一切（[`SoftKeyboard`]）都必须住在主线程：AppKit 的窗口/视图只能在主线程创建
//! 与改动，而 `SoftKeyboard` 内部又是 `Rc<RefCell<..>>`（非 `Send`），本来也过不了线程。
//! 但下发命令的 `manager_macos::forwarder_thread` 是一条**工作线程**，且它是个
//! `for cmd in rx` 的阻塞循环——既不能在那儿碰 AppKit，也没法顺带驱动面板的 `tick`。
//!
//! 于是本模块做两件事，形制照抄 [`crate::global_hotkey_macos`]（同一个主线程、同一套
//! 「入队 + 唤醒源」约定，那边已实测可用）：
//!
//! 1. **转运**：forwarder 线程调 [`apply`] 只把命令塞进 `PENDING` 并戳一下唤醒源；
//!    真正的建窗/绘制在主线程的 perform 回调里做。
//! 2. **驱动**：面板可见期间挂一个 `CFRunLoopTimer` 周期性调 `tick()`——鼠标长按重复、
//!    物理 Shift/CapsLock 跟随、按键高亮到期熄灭都靠它。**面板一关就撤掉定时器**，
//!    线程回到全静默。
//!
//! # ⚠️ 装配时机
//!
//! [`install_on_main`] 必须由**主线程**在进 `RunApplicationEventLoop` 之前调用一次
//! （见 `apps/service/src/main.rs`）。在那之前 forwarder 可能已经发过命令了——那些命令
//! 留在 `PENDING` 里，装配时主动 drain 一次补上，与 `global_hotkey_macos` 的做法一致。

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
/// 取 20ms 是为了同时盖住面板里两个更慢的节奏：物理 Shift/CapsLock 的轮询
/// （`MODIFIER_POLL_MS` = 40）与鼠标长按的重复（`REPEAT_RATE_MS` = 33）。比它们都快
/// 一档，两者才不会被采样节拍拖成肉眼可见的迟滞。
///
/// ⚠️ 这是本仓「UI 事件驱动、不做空转轮询」的一处**有意例外**，边界与 `SoftKeyboard::tick`
/// 里那条注释写的完全一致：**只在面板可见时**。面板一关，[`sync_timer`] 立刻撤掉定时器。
const TICK_MS: f64 = 0.020;

/// 转运给主线程的软键盘命令。
///
/// 刻意**不直接传 `UiCommand`**：那个枚举有 46 个变体，本模块只关心其中 4 个加一个
/// 主题，用它做队列元素等于让读者以为这里还会处理别的。字段与 `UiCommand` 的对应
/// 变体逐位一致。
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
    /// 主题变更。
    ///
    /// ★ 面板是**惰性创建**的（首次 `Show` 才建窗），而 `SetTheme` 一般只在启动时发一次。
    /// 不把最近一份主题留住，面板就会永远停在内置默认配色——与 Windows 侧 `manager.rs`
    /// 用 `last_theme` 给惰性窗口"补课"是同一件事，这里换成在本模块留底。
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
    /// 面板本体。**只在主线程访问**（`thread_local` 把这条约定焊进类型系统之外的另一层：
    /// 别的线程即使拿到本模块也只会看到一个空 `RefCell`，而不是数据竞争）。
    static PANEL: RefCell<Option<SoftKeyboard>> = const { RefCell::new(None) };
    /// 最近一份主题，给惰性建窗补课用。见 [`SkCmd::Theme`]。
    static THEME: RefCell<Option<wind_theme::Resolved>> = const { RefCell::new(None) };
    /// 面板可见期间的 tick 定时器。`None` = 未挂（面板不可见）。
    static TIMER: RefCell<Option<CFRunLoopTimerRef>> = const { RefCell::new(None) };
}

/// 从**任意线程**提交一条软键盘命令。
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
/// **必须在主线程、且在 `RunApplicationEventLoop` 之前调用一次。**
pub fn install_on_main() {
    if WAKE_SOURCE.get().is_some() {
        return;
    }
    unsafe {
        // 逐字段构造，**不能**用 `mem::zeroed()`：`perform` 是非空函数指针，全零对它是
        // 非法值，真机上会在启动时直接 abort 且 `cargo check` 看不出来。
        // 这一条是 `global_hotkey_macos` 已经踩过并写进注释的坑，此处照办。
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

/// 取走队列并逐条应用。**只在主线程调用。**
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
    }
}

/// 借出面板做一件事。借不到就**跳过**。
///
/// ⚠️ 用 `try_borrow_mut` 而不是 `borrow_mut`，理由与 `mac_panel::PanelView::dispatch`
/// 里那段完全相同：命令 drain 与 tick 定时器同在主线程，某些 AppKit 调用会重入
/// run loop，于是有可能在面板已借出时再进来一次。那一下若 panic，输入法服务整个进程
/// 就没了——而跳过的代价只是少画一帧，下一次 tick 就补回来。
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
/// 建窗失败只记一条 error 并返回 false——**不 panic**：软键盘建不出来，输入法其余部分
/// 照常工作，这是 `manager.rs` 对所有惰性窗口的一贯处置（best-effort）。
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
/// 幂等，每次处理完命令与每轮 tick 后都调一次——面板的显隐有好几条来路（协调器下发的
/// Show/Hide、面板自己的关闭按钮），让每条来路各记一次注定要漏，漏的表现是「面板关了
/// 但 CPU 一直在跑」或「面板开着却不响应长按」。
fn sync_timer() {
    let visible = PANEL.with(|p| {
        p.try_borrow()
            .is_ok_and(|p| p.as_ref().is_some_and(|k| k.is_visible()))
    });
    TIMER.with(|t| {
        // 这里用裸 `borrow_mut` 而非 `try_borrow_mut`（与本模块其余各处相反）是**有据的**：
        // 借出期间只调 CFRunLoopTimer 的创建/挂载/失效，三者都不驱动 run loop，故本函数
        // 不可能在自身借出期间被重入。改动这段时若引入任何会转一次 run loop 的调用，
        // 这条前提就没了，得跟着换成 try_borrow_mut。
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
