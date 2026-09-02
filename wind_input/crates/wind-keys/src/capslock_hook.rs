//! CapsLock 全局低级键盘钩子（`WH_KEYBOARD_LL`）。
//!
//! # 为什么非它不可
//!
//! CapsLock / NumLock / ScrollLock 的锁定态由系统在**输入线程状态机**里维护，位置在 TSF
//! key event sink **之前**——TSF 里 `pfEaten = TRUE` 只表示「这个键事件我处理了」，**不是**
//! 「这个键没发生过」，压不住锁定态翻转（2026-08-11 真机实测）。
//!
//! 先前尝试过「让它翻转，再 `SendInput` 回敲复原」，真机撞到两个无解的问题：快速连按时
//! 物理事件与注入事件在队列里的相对顺序无法保证，大写会卡住；且那次真实的状态变化会被
//! 厂商 OSD 工具（联想等）观测到并弹窗。**事后修正在竞态下没有正确解**，只能在它发生之前
//! 阻止它发生。
//!
//! `LowLevelKeyboardProc` 是用户态唯一做得到的位置，MS 文档原文：
//! > the callback function is called **before the asynchronous state of the key is updated**
//! > ...it may return a nonzero value to prevent the system from passing the message to the
//! > rest of the hook chain or the target window procedure.
//!
//! # 三条硬约束（都来自文档，违反了都是无声故障）
//!
//! 1. **回调必须极快返回**。超时后 Win7+ 会把钩子**静默移除**，且「there is no way for the
//!    application to know whether the hook is removed」。故回调里只读一个原子量 + 一次
//!    非阻塞 `send`，**不加锁、不分配、不做 IPC**。
//! 2. **安装线程必须有消息循环**（钩子是靠给该线程发消息来调用的）。故本模块自带专用线程。
//! 3. ★ **专用线程，不能搭 UI 线程的便车**。UI 线程要渲染候选窗（LayeredWindow 位图合成），
//!    某次渲染慢过 `LowLevelHooksTimeout`（Win10 1709+ 上限 1000ms）钩子就永久掉了，而且
//!    没有任何信号——本仓最难排查的故障全是这一类。
//!
//! # 安装门控
//!
//! ★ **只有用户在 `keys.session_actions` 里真的配了 `capslock` 才安装**（协调器侧判定）。
//! 没配的用户进程里根本不存在全局键盘钩子——这是本功能唯一的风险控制手段，不可省。

#[cfg(windows)]
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// 钩子此刻是否应当吃掉 CapsLock。
///
/// 由协调器随「有没有输入会话」实时更新。★ 它为 true 的时间窗必须尽量短：钩子是**全局**的，
/// 这个标志滞留就意味着用户在**别的应用**里按 CapsLock 也切不动大小写——那比功能不生效
/// 糟糕得多，属于必须优先避免的故障方向。
static SHOULD_EAT: AtomicBool = AtomicBool::new(false);

/// 按下 CapsLock 时的通知回调。在钩子线程里执行，实现方必须只做非阻塞投递。
type PressCallback = Box<dyn Fn() + Send + Sync + 'static>;
#[cfg(windows)]
static CALLBACK: OnceLock<PressCallback> = OnceLock::new();

/// 设置「当前是否拦截 CapsLock」。协调器在会话状态变化时调用，钩子未安装时也可安全调用。
pub fn set_should_eat(eat: bool) {
    SHOULD_EAT.store(eat, Ordering::Relaxed);
}

/// 当前拦截状态（供日志/诊断）。
pub fn should_eat() -> bool {
    SHOULD_EAT.load(Ordering::Relaxed)
}

/// 钩子对一次**非注入的** CapsLock 事件的裁决：`(要不要吃掉, 配对状态的新值)`。
///
/// 抽成纯函数有两个理由：钩子回调在单测里根本跑不到（要真装 LL 钩子，CI 的 Linux 上更没有），
/// 而这段逻辑的两个方向后果**极不对称**——少吃只是「这次绑定没生效」，多吃是「用户在**别的
/// 应用**里 CapsLock 按不动」。这种判据必须能脱离平台逐条断言。
///
/// 规则只有三条：
/// - down + 闸门开 → 吃，并记下「欠一个 up」；
/// - down + 闸门关 → 放行，**不动**配对状态（此时没有欠账，也不该清掉别人的）；
/// - up → 只看配对状态：欠着就吃并销账，没欠就放行。**刻意不问闸门**，因为绑定动作自己
///   就可能在 down 与 up 之间把闸门关掉（选词上屏 ⇒ 候选清空 ⇒ 无会话）。
///
/// ⚠️ 唯一的生产使用者在 `#[cfg(windows)] mod imp` 里，单测又只在 `cfg(test)` 下存在，
/// 故非 Windows 目标编 **lib** target 时它确实无人调用（CI 的 darwin clippy 会拦）。
/// ⛔ 别改成 `#[cfg(windows)]`：本函数抽成纯函数就是为了能脱离平台逐条断言。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn decide_eat(is_down: bool, should_eat: bool, eaten_down: bool) -> (bool, bool) {
    if is_down {
        if should_eat {
            // 按住不放时系统重复发 keydown：重复置位是幂等的，最后那次 up 一并销账。
            (true, true)
        } else {
            (false, eaten_down)
        }
    } else {
        (eaten_down, false)
    }
}

#[cfg(windows)]
mod imp {
    use super::{CALLBACK, SHOULD_EAT};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use tracing::{debug, error, info};
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PostThreadMessageW,
        SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
    };

    const VK_CAPITAL: u32 = 0x14;
    /// `nCode` 为该值时 wParam/lParam 才含按键信息；小于 0 时文档要求原样下传。
    const HC_ACTION: i32 = 0;

    /// 上一个被吃掉的 CapsLock keydown 是否还在等它的 keyup。
    ///
    /// ★★ 与 [`SHOULD_EAT`] **正交**：闸门管「这一次按下要不要接管」，本标志管「已经接管
    /// 的这一次要吃到底」。
    ///
    /// 模块文档那句「down 和 up 都要吃」是对的，但它默认了一个**没写出来的前提**——闸门在
    /// down 与 up 之间不会变。而本功能的动作**自己就会破坏这个前提**：`select_candidate`
    /// 选词上屏后候选清空 ⇒ `has_input_session` 为假 ⇒ `notify_ui_update` 把闸门归零，
    /// 而那时用户还没松手。于是 keyup 漏出去，系统与宿主收到一个**没有 down 的孤儿 up**，
    /// TSF 还会把它当成「用户切了大写」的状态通知转发给服务端。
    ///
    /// 真机现象（2026-08-31）：功能完全正常，却在第一次操作时闪一下状态提示泡——因为
    /// 服务端的 `show_status()` 拿空的 `last_status_text` 去比，首次必然弹一次。翻页碰不到
    /// 是因为翻完页候选还在，闸门不会归零。
    ///
    /// ⚠️ 只由 CapsLock 的 down 置位、up 清零，另在装钩子时清零（装卸之间若卡着一次未配对
    /// 的 down，重装后第一个 keyup 会被误吃——那正是「用户在别的应用里 CapsLock 按不动」
    /// 的方向，必须避免）。`notify_ui_hide` / `handle_focus_lost` 那类闸门归零**不得**碰它：
    /// 它们要收回的是「接管新按下」的权限，不是撤销一次已经吃了一半的按键。
    static EATEN_DOWN: AtomicBool = AtomicBool::new(false);

    /// 已安装的钩子。Drop 即卸载（停消息泵 → 线程内 `UnhookWindowsHookEx` → join）。
    pub struct CapsLockHook {
        thread_id: u32,
        join: Option<std::thread::JoinHandle<()>>,
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // 文档：nCode < 0 必须直接下传；这里连 HC_ACTION 之外的一律下传。
        // 两个标志都为假时立刻下传——绝大多数按键走的是这条路径，必须最短（两次
        // relaxed 原子读，不解引用 lParam、不进任何分支）。
        if code == HC_ACTION
            && (SHOULD_EAT.load(Ordering::Relaxed) || EATEN_DOWN.load(Ordering::Relaxed))
        {
            // SAFETY: 文档保证 nCode == HC_ACTION 时 lParam 指向有效的 KBDLLHOOKSTRUCT。
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if info.vkCode == VK_CAPITAL {
                // 注入事件一律放行。本模块自己不注入 CapsLock，但别的工具（AHK / 厂商热键
                // 程序）可能会——拦下它们既无意义，又会让那些工具行为异常。
                if (info.flags & LLKHF_INJECTED).0 == 0 {
                    let msg = wparam.0 as u32;
                    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                    // 裁决抽成纯函数（见其文档）。load + store 不是原子对，但钩子回调由
                    // 系统在**本模块自己的那一个线程**上串行调用，不存在并发进入；
                    // `SHOULD_EAT` 是别的线程写的，这里只取一次快照。
                    let (eat, next_eaten) = super::decide_eat(
                        is_down,
                        SHOULD_EAT.load(Ordering::Relaxed),
                        EATEN_DOWN.load(Ordering::Relaxed),
                    );
                    EATEN_DOWN.store(next_eaten, Ordering::Relaxed);
                    if eat {
                        if is_down && let Some(cb) = CALLBACK.get() {
                            cb();
                        }
                        return LRESULT(1);
                    }
                }
            }
        }
        // SAFETY: 转交钩子链的标准调用；参数原样下传。
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    impl CapsLockHook {
        /// 安装钩子。`on_press` 在钩子线程执行，**必须只做非阻塞投递**（见模块文档约束 1）。
        ///
        /// 回调只能设置一次（`OnceLock`）——钩子的装卸可以反复，回调本身是进程级常量。
        pub fn install(on_press: super::PressCallback) -> anyhow::Result<Self> {
            let _ = CALLBACK.set(on_press);
            // 装卸之间若卡着一次未配对的 down（卸载恰好发生在用户按住 CapsLock 时），
            // 重装后第一个 keyup 会被误吃 ⇒ 用户在别的应用里 CapsLock 按不动。清零。
            EATEN_DOWN.store(false, Ordering::Relaxed);

            let (tx, rx) = mpsc::channel::<Result<u32, String>>();
            let join = std::thread::Builder::new()
                .name("capslock-hook".into())
                .spawn(move || unsafe {
                    let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                        Ok(h) => h,
                        Err(e) => {
                            let _ = tx.send(Err(format!("SetWindowsHookExW 失败: {e}")));
                            return;
                        }
                    };
                    let tid = windows::Win32::System::Threading::GetCurrentThreadId();
                    let _ = tx.send(Ok(tid));

                    // 钩子靠「给本线程发消息」来调用，故必须有消息泵。这里只等 WM_QUIT，
                    // 泵本身不做任何事——线程越空闲，钩子回调的响应越不可能超时。
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {}

                    if let Err(e) = UnhookWindowsHookEx(hook) {
                        error!("CapsLock 钩子卸载失败: {e}");
                    } else {
                        debug!("CapsLock 钩子已卸载");
                    }
                })?;

            match rx.recv() {
                Ok(Ok(thread_id)) => {
                    info!("CapsLock 全局钩子已安装 (tid={thread_id})");
                    Ok(Self {
                        thread_id,
                        join: Some(join),
                    })
                }
                Ok(Err(e)) => anyhow::bail!("{e}"),
                Err(e) => anyhow::bail!("钩子线程未回报安装结果: {e}"),
            }
        }
    }

    impl Drop for CapsLockHook {
        fn drop(&mut self) {
            // 卸载期间先停止拦截：PostThreadMessage 到线程真正退出之间仍会有回调进来。
            SHOULD_EAT.store(false, Ordering::Relaxed);
            // 配对状态一并清零：钩子都要没了，「还欠一个 up」的账不能留给下一次安装。
            EATEN_DOWN.store(false, Ordering::Relaxed);
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    /// 非 Windows 平台的空壳：安装恒失败，调用方按「功能不可用」降级。
    pub struct CapsLockHook;

    impl CapsLockHook {
        pub fn install(_on_press: super::PressCallback) -> anyhow::Result<Self> {
            anyhow::bail!("低级键盘钩子仅 Windows 可用")
        }
    }
}

pub use imp::CapsLockHook;

#[cfg(test)]
mod tests {
    use super::decide_eat;

    /// 正常一次：闸门开着按下、松手，两半都吃掉，账也平了。
    #[test]
    fn paired_down_and_up_are_both_eaten() {
        let (eat_down, eaten) = decide_eat(true, true, false);
        assert!(eat_down, "闸门开时 down 必须吃");
        assert!(eaten, "吃了 down 就要记下欠一个 up");
        let (eat_up, eaten) = decide_eat(false, true, eaten);
        assert!(eat_up, "配对的 up 必须吃");
        assert!(!eaten, "销账");
    }

    /// ★ 回归 2026-08-31：动作**自己**在 down 与 up 之间把闸门关掉（选词上屏 ⇒ 候选清空
    /// ⇒ 无会话）。up 必须照吃——否则系统与宿主收到孤儿 up，TSF 还会把它当成「用户切了
    /// 大写」的状态通知转发给服务端（真机现象：功能正常，却闪一下状态提示泡）。
    #[test]
    fn up_is_eaten_even_after_gate_closed_mid_press() {
        let (_, eaten) = decide_eat(true, true, false);
        let (eat_up, eaten_after) = decide_eat(false, /* 闸门已关 */ false, eaten);
        assert!(eat_up, "闸门中途关掉，配对的 up 仍必须吃");
        assert!(!eaten_after);
    }

    /// 反方向的守卫：没吃过 down 的 up 一律放行。多吃的后果是「用户在别的应用里
    /// CapsLock 按不动」，比功能不生效严重得多，故这条不能有例外——闸门开着也不例外
    /// （闸门刚在别处置位、而这次按下发生在置位之前，正是那个竞态窗口）。
    #[test]
    fn orphan_up_is_never_eaten() {
        assert_eq!(decide_eat(false, false, false), (false, false));
        assert_eq!(decide_eat(false, true, false), (false, false));
    }

    /// 闸门关时 down 放行，且**不动**配对状态：那时没有欠账，也不该清掉别人的。
    #[test]
    fn down_with_gate_closed_passes_through_and_keeps_pairing_state() {
        assert_eq!(decide_eat(true, false, false), (false, false));
        assert_eq!(decide_eat(true, false, true), (false, true));
    }

    /// 按住不放：系统重复发 keydown，重复置位幂等，最后一次 up 一并销账。
    #[test]
    fn repeated_down_is_idempotent() {
        let mut eaten = false;
        for _ in 0..5 {
            let (eat, next) = decide_eat(true, true, eaten);
            assert!(eat);
            eaten = next;
        }
        assert!(eaten);
        assert_eq!(decide_eat(false, true, eaten), (true, false));
    }
}
