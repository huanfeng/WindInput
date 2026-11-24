//! CapsLock 全局低级键盘钩子（`WH_KEYBOARD_LL`）。注释切换键选了 Alt 时同一个钩子兼管 Alt
//! （判定见 [`crate::alt_tap`]）：一个进程只装一个 LL 钩子。
//!
//! CapsLock 的锁定态在 TSF key event sink 之前就已翻转，TSF 里吃键压不住；事后回敲复原在竞态下
//! 无解。LL 钩子回调发生在按键异步状态更新之前，是用户态唯一拦得住的位置。背景见
//! docs/design/session-key-actions.md 的「CapsLock：不走 TSF，走全局低级键盘钩子」。
//!
//! # 三条硬约束（违反了都是无声故障）
//!
//! 1. 回调必须极快返回，超时后系统会静默移除钩子。故回调里只读原子量 + 一次非阻塞 `send`，
//!    不加锁、不分配、不做 IPC。唯一例外是 Alt 松开时补发屏蔽键的那次 `SendInput`（栈上数组、
//!    不阻塞）：屏蔽键必须排在 Alt 松开之前，只能在回调里补。
//! 2. 安装线程必须有消息循环（钩子靠给该线程发消息来调用），故本模块自带专用线程。
//! 3. 不能搭 UI 线程的便车：候选窗某次渲染慢过 `LowLevelHooksTimeout`，钩子就永久掉了且没有信号。
//!
//! # 安装门控
//!
//! 只有用户在 `keys.session_actions` 里配了 `capslock`、或把它勾成中英切换键时才安装（协调器侧
//! 判定）。没配的用户进程里根本不存在全局键盘钩子，这是首要的风险控制，不可省。
//!
//! 勾成切换键时空闲也要吃（[`IDLE_PID`]），另有两重保护：只在前台窗口属于本输入法所在的
//! 进程时接管；按着 Shift / Ctrl / Alt / Win 时不接管。
//!
//! Alt 那一份另算：`ui.comment.switch_mode != off` 且 `switch_key` 是 Alt 系才要。钩子 =
//! CapsLock 要 ∪ Alt 要，协调器两边都不要时卸载。闸门 [`set_alt_gate`] 只在有候选时非 0；
//! 认领时还要前台窗口属于活动客户端的进程（[`set_alt_pid`]），候选窗残留时别的程序里的 Alt 不接管。
//!
//! Alt 的已知边角：
//! - Alt+鼠标点击不算打断（钩子只看键盘），松开照样补屏蔽键、照样算单击；
//! - 比我们晚装的 LL 钩子先吃掉的组合键我们看不到，那次会被当成单击；
//! - 前台是管理员窗口时 UIPI 拦下 SendInput，补发失败走「不吃」分支：放行物理松开，那次菜单栏照常激活。

#[cfg(windows)]
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub use crate::alt_tap::AltEvent;

/// 钩子此刻是否应当吃掉 CapsLock。
///
/// 由协调器随「有没有输入会话」实时更新。为 true 的时间窗必须尽量短：钩子是全局的，标志滞留
/// 意味着用户在别的应用里按 CapsLock 也切不动大小写，比功能不生效糟糕得多。
static SHOULD_EAT: AtomicBool = AtomicBool::new(false);

/// 空闲时也吃 CapsLock：「切换按键」勾了 CapsLock 时，存本输入法激活所在的宿主进程 pid，
/// 0 = 不吃。由协调器随激活态更新。
///
/// 与 [`SHOULD_EAT`] 分开存：那个闸门在候选窗隐藏、失焦时被无条件归零（宁可少吃），
/// 这个跟着激活态走。激活态在宿主崩溃、切到不走 TSF 的程序时可能清不掉，故吃键前还要比对
/// 前台窗口的进程：对不上就放行，滞留的闸门也伤不到别的应用。
static IDLE_PID: AtomicU32 = AtomicU32::new(0);

/// 被吃掉的 CapsLock 事件。`Down` 含按住不放时的自动重复，`first` 标出这次按下的第一个；
/// `Up` 只在这次按下归我们时才有，带按住时长（毫秒，取钩子事件的系统时间戳之差），
/// `interrupted` = 按住期间按过别的键（同 Shift 的轻敲判定：那样不算单按 CapsLock）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsEvent {
    Down { first: bool },
    Up { held_ms: u32, interrupted: bool },
}

/// 钩子投递给协调器的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    Caps(CapsEvent),
    Alt(AltEvent),
}

/// Alt 闸门：`alt_tap::GATE_*` 位，0 = 不接管 Alt。由协调器随候选窗显隐更新。
static ALT_GATE: AtomicU32 = AtomicU32::new(0);

/// Alt 认领要求的前台进程 pid（活动客户端所在进程），0 = 不认领。随 [`ALT_GATE`] 一起同步。
static ALT_PID: AtomicU32 = AtomicU32::new(0);

/// 钩子事件的通知回调。在钩子线程里执行，实现方必须只做非阻塞投递。
type PressCallback = Box<dyn Fn(HookEvent) + Send + Sync + 'static>;
#[cfg(windows)]
static CALLBACK: OnceLock<PressCallback> = OnceLock::new();

/// 设置「当前是否拦截 CapsLock」。协调器在会话状态变化时调用，钩子未安装时也可安全调用。
pub fn set_should_eat(eat: bool) {
    SHOULD_EAT.store(eat, Ordering::Relaxed);
}

/// 设置「空闲时在哪个进程里也拦截 CapsLock」（0 = 不拦），见 [`IDLE_PID`]。钩子未安装时也可安全调用。
pub fn set_idle_pid(pid: u32) {
    IDLE_PID.store(pid, Ordering::Relaxed);
}

/// 设置 Alt 闸门（`alt_tap::GATE_*` 位，0 = 不接管）。钩子未安装时也可安全调用。
pub fn set_alt_gate(mask: u32) {
    ALT_GATE.store(mask, Ordering::Relaxed);
}

/// 设置 Alt 认领要求的前台进程 pid（0 = 不认领）。钩子未安装时也可安全调用。
pub fn set_alt_pid(pid: u32) {
    ALT_PID.store(pid, Ordering::Relaxed);
}

/// 当前拦截状态（供日志/诊断）。
pub fn should_eat() -> bool {
    SHOULD_EAT.load(Ordering::Relaxed)
}

/// 钩子对一次非注入的 CapsLock 事件的裁决：`(要不要吃掉, 配对状态的新值)`。抽成纯函数是为了
/// 脱离平台逐条断言：少吃只是「这次绑定没生效」，多吃是「用户在别的应用里 CapsLock 按不动」。
///
/// 配对状态优先于闸门：闸门管「这次按下要不要接管」，一旦接管，这次物理按下的余下事件
/// （重复的 down、最后的 up）都归我们，中途闸门关掉不改变归属。
/// - down + 闸门开 → 吃，并记下「欠一个 up」；
/// - down + 闸门关 + 已欠 up → 照吃（同一次按下的自动重复；绑定动作可能已把闸门关掉，放行会
///   每次翻转一下真正的大写锁定）；
/// - down + 闸门关 + 没欠 → 放行；
/// - up → 只看配对状态：欠着就吃并销账，没欠就放行，不问闸门。
///
/// 代价：钩子超时被摘而丢过一个 up 时，下一次按下会被多吃一次，其 up 销账后自愈。
/// 背景见 docs/design/session-key-actions.md 的「一次按下吃到底」。
///
/// 生产调用只在 `#[cfg(windows)] mod imp` 里，非 Windows 编 lib 时无人调用；别改成
/// `#[cfg(windows)]`，否则单测脱离不了平台。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn decide_eat(is_down: bool, should_eat: bool, eaten_down: bool) -> (bool, bool) {
    if is_down {
        // 不能只问 `should_eat`：闸门可能已被绑定动作关掉，重复 down 漏出去会翻转大写锁定。
        if should_eat || eaten_down {
            // 重复置位是幂等的，最后那次 up 一并销账。
            (true, true)
        } else {
            (false, false)
        }
    } else {
        (eaten_down, false)
    }
}

#[cfg(windows)]
mod imp {
    use super::{ALT_GATE, ALT_PID, CALLBACK, CapsEvent, HookEvent, IDLE_PID, SHOULD_EAT};
    use crate::alt_tap::{AltPress, Origin, decide_alt};
    use crate::key_inject::{INJECT_MARK, send_alt_release_masked};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::mpsc;
    use tracing::{debug, error, info};
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
        KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, MSG, PostThreadMessageW,
        SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
    };

    const VK_CAPITAL: u32 = 0x14;
    /// `nCode` 为该值时 wParam/lParam 才含按键信息；小于 0 时文档要求原样下传。
    const HC_ACTION: i32 = 0;

    /// 上一个被吃掉的 CapsLock keydown 是否还在等它的 keyup。
    ///
    /// 与 [`SHOULD_EAT`] 正交：闸门管「这一次按下要不要接管」，本标志管「已经接管的这一次
    /// 要吃到底」。绑定动作（选词上屏 ⇒ 候选清空）会在用户松手前把闸门归零，keyup 漏出去
    /// 就成了孤儿 up。
    ///
    /// 只由 CapsLock 的 down 置位、up 清零，另在装卸钩子时清零（否则重装后第一个 keyup 被误吃）。
    /// `notify_ui_hide` / `handle_focus_lost` 那类闸门归零不得碰它：它们收回的是接管新按下的
    /// 权限，不是撤销一次吃了一半的按键。
    static EATEN_DOWN: AtomicBool = AtomicBool::new(false);

    /// 这次被接管的按下里第一个 down 的系统时间戳（`KBDLLHOOKSTRUCT::time`，毫秒）。
    static DOWN_TIME: AtomicU32 = AtomicU32::new(0);

    /// 这次被接管的按下期间是否按过别的键。首个 down 清零，其它键的 down 置位。
    static INTERRUPTED: AtomicBool = AtomicBool::new(false);

    /// 有一次被认领的 Alt 按下还没松开（闸门关了也要接着看它的松开）。
    static ALT_OWED: AtomicBool = AtomicBool::new(false);

    thread_local! {
        /// 被认领的 Alt 按下。回调只在钩子线程上串行执行；每次安装是新线程，状态自然清空。
        static ALT_PRESS: Cell<Option<AltPress>> = const { Cell::new(None) };
    }

    /// 按着 Shift / Ctrl / Win（只在认领 Alt 时查）。AltGr 布局下右 Alt 带一个系统合成的左 Ctrl，
    /// 因而不认领——那里右 Alt 本就是字符修饰键。
    fn other_modifiers_held() -> bool {
        let held = |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| unsafe {
            GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0
        };
        held(VK_SHIFT) || held(VK_CONTROL) || held(VK_LWIN) || held(VK_RWIN)
    }

    /// Alt 侧：裁决、投递，返回是否吃掉。要补发时补发失败就不吃——宁可这次激活菜单栏，
    /// 也不能让 Alt 卡在按下态。
    fn handle_alt(info: &KBDLLHOOKSTRUCT, msg: u32) -> bool {
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let origin = if (info.flags & LLKHF_INJECTED).0 != 0 {
            Origin::Injected
        } else {
            Origin::Physical
        };
        let gate = ALT_GATE.load(Ordering::Relaxed);
        let v = ALT_PRESS.with(|c| {
            let mut press = c.get();
            let v = decide_alt(
                &mut press,
                info.vkCode,
                is_down,
                origin,
                info.time,
                gate,
                other_modifiers_held,
                || foreground_is(ALT_PID.load(Ordering::Relaxed)),
            );
            c.set(press);
            ALT_OWED.store(press.is_some(), Ordering::Relaxed);
            v
        });
        if let Some(e) = v.event
            && let Some(cb) = CALLBACK.get()
        {
            cb(HookEvent::Alt(e));
        }
        if !v.eat {
            return false;
        }
        !v.mask
            || send_alt_release_masked(
                info.vkCode,
                info.scanCode,
                (info.flags & LLKHF_EXTENDED).0 != 0,
            )
    }

    /// 空闲闸门此刻对这次按下成不成立：前台窗口属于 `pid`，且没按着组合修饰键。
    ///
    /// 只在 CapsLock 的首个 down 上调（一次物理按下一次），两个都是不阻塞的系统调用。
    fn idle_gate_open(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        // 按着修饰键（Shift+CapsLock 等）交还系统：那是用户要的组合，而且吃掉 CapsLock 会让
        // TSF 把那一下 Shift 判成干净单击，再切一次中英。
        let held = |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| unsafe {
            GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0
        };
        if held(VK_SHIFT) || held(VK_CONTROL) || held(VK_MENU) || held(VK_LWIN) || held(VK_RWIN) {
            return false;
        }
        foreground_is(pid)
    }

    /// 前台窗口属于进程 `pid`（0 恒为假）。两个都是不阻塞的系统调用。
    fn foreground_is(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        let mut fg_pid = 0u32;
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return false;
            }
            GetWindowThreadProcessId(hwnd, Some(&mut fg_pid));
        }
        fg_pid == pid
    }

    /// 已安装的钩子。Drop 即卸载（停消息泵 → 线程内 `UnhookWindowsHookEx` → join）。
    pub struct CapsLockHook {
        thread_id: u32,
        join: Option<std::thread::JoinHandle<()>>,
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // nCode 非 HC_ACTION 一律下传。五个标志都为假时立刻下传：绝大多数按键走这条路，
        // 只做五次 relaxed 原子读、不解引用 lParam。
        let caps_active = SHOULD_EAT.load(Ordering::Relaxed)
            || IDLE_PID.load(Ordering::Relaxed) != 0
            || EATEN_DOWN.load(Ordering::Relaxed);
        let alt_active = ALT_GATE.load(Ordering::Relaxed) != 0 || ALT_OWED.load(Ordering::Relaxed);
        // SAFETY: 文档保证 nCode == HC_ACTION 时 lParam 指向有效的 KBDLLHOOKSTRUCT。
        let info = (code == HC_ACTION && (caps_active || alt_active))
            .then(|| unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) })
            // 我们自己补发的事件一律放行，两侧状态都不碰。
            .filter(|info| info.dwExtraInfo != INJECT_MARK);
        if let Some(info) = info
            && alt_active
            && handle_alt(info, wparam.0 as u32)
        {
            return LRESULT(1);
        }
        if let Some(info) = info.filter(|_| caps_active) {
            if info.vkCode != VK_CAPITAL {
                // 按住 CapsLock 期间按了别的键：松开时不算单按。
                let msg = wparam.0 as u32;
                if EATEN_DOWN.load(Ordering::Relaxed) && (msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN)
                {
                    INTERRUPTED.store(true, Ordering::Relaxed);
                }
            } else {
                // 注入事件一律放行：协调器开关大写靠注入一次 CapsLock（长按切换键）；
                // AHK / 厂商热键程序的注入同理不该拦。
                if (info.flags & LLKHF_INJECTED).0 == 0 {
                    let msg = wparam.0 as u32;
                    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                    // load + store 不是原子对，但回调只在本模块的钩子线程上串行执行；
                    // `SHOULD_EAT` 是别的线程写的，这里只取一次快照。
                    let owed = EATEN_DOWN.load(Ordering::Relaxed);
                    // 空闲闸门只在一次按下的首个 down 上判（之后的事件靠配对状态归属）。
                    let gate = SHOULD_EAT.load(Ordering::Relaxed)
                        || (is_down && !owed && idle_gate_open(IDLE_PID.load(Ordering::Relaxed)));
                    let (eat, next_eaten) = super::decide_eat(is_down, gate, owed);
                    EATEN_DOWN.store(next_eaten, Ordering::Relaxed);
                    if eat {
                        let ev = if is_down {
                            if !owed {
                                DOWN_TIME.store(info.time, Ordering::Relaxed);
                                INTERRUPTED.store(false, Ordering::Relaxed);
                            }
                            CapsEvent::Down { first: !owed }
                        } else {
                            CapsEvent::Up {
                                held_ms: info.time.wrapping_sub(DOWN_TIME.load(Ordering::Relaxed)),
                                interrupted: INTERRUPTED.load(Ordering::Relaxed),
                            }
                        };
                        if let Some(cb) = CALLBACK.get() {
                            cb(HookEvent::Caps(ev));
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
        /// 安装钩子。`on_press` 在钩子线程执行，必须只做非阻塞投递（见模块文档约束 1）。
        ///
        /// 回调只能设置一次（`OnceLock`）——钩子的装卸可以反复，回调本身是进程级常量。
        pub fn install(on_press: super::PressCallback) -> anyhow::Result<Self> {
            let _ = CALLBACK.set(on_press);
            // 卸载若恰在用户按住 CapsLock 时发生，残留的配对状态会让重装后第一个 keyup 被误吃。
            EATEN_DOWN.store(false, Ordering::Relaxed);
            ALT_OWED.store(false, Ordering::Relaxed);

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
                        error!("全局键盘钩子卸载失败: {e}");
                    } else {
                        debug!("全局键盘钩子已卸载");
                    }
                })?;

            match rx.recv() {
                Ok(Ok(thread_id)) => {
                    info!("全局键盘钩子已安装 (tid={thread_id})");
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
            IDLE_PID.store(0, Ordering::Relaxed);
            // 配对状态一并清零：钩子都要没了，「还欠一个 up」的账不能留给下一次安装。
            EATEN_DOWN.store(false, Ordering::Relaxed);
            ALT_GATE.store(0, Ordering::Relaxed);
            ALT_OWED.store(false, Ordering::Relaxed);
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
#[path = "capslock_hook_tests.rs"]
mod tests;
