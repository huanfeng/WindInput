//! **吃键判定：唯一真相源。**
//!
//! # 这个模块解决什么
//!
//! 「这个键该不该由输入法处理」这条判据，此前有**两份实现**：
//! - C++ `CKeyEventSink::OnTestKeyDown`（约 400 行，含热键白名单/切换键/配对跳出/…）
//! - 协调器内部散落的各处分支
//!
//! 它们靠注释和纪律维持同步，那些注释本身就是证据：「判据须与服务端保持单一真相源」
//! 「判据镜像 core」「漂移即『吃了再吐』丢键」。
//!
//! Android 接入后出现了**第三份**——FFI 层手写的谓词。一个开发会话里同形 bug 出现三次：
//! 空缓冲的空格/回车/退格/数字全无效、切到英文模式字母打不出。两次的形态完全一样：
//! 协调器对不该收的键返回 `Consumed`（意为「已在输入法内处理」），宿主当成消费后
//! **既不上屏也不执行默认行为**，键静默消失。
//!
//! 本模块把判据收进核心。宿主侧只做自己独有的前置（只读上下文、密码框、自注入按键
//! 跳过）与后置（TSF 的 pending toggle 计时），中间的判定一律调 [`Coordinator::should_handle_key`]。
//!
//! # 迁移状态
//!
//! - Android：已切到本模块，FFI 层的手写谓词已删除。
//! - C++ TSF：**尚未迁移**（保持现状，桌面零风险）。迁移时 C++ 只需保留
//!   TSF 特有的前置/后置，中间整段替换为一次 IPC 查询或本地镜像调用。
//! - macOS IMKit：**没有前置闸门**——`InputController.handle` 把每个 keyDown 都发给协调器，
//!   再照 `KeyAction` 决定吃不吃。于是本模块第 3 条（快捷键组合归宿主）在那边完全不生效，
//!   全靠协调器返回 `PassThrough`；而「有组合时按 ⌘C」这条返回的是 `ClearComposition`，
//!   Swift 侧一度按「已消费」处理 → 键被吞（issue #64）。现由
//!   `BridgeResponseRouter.apply(hostShortcut:)` 顶住，迁移本模块后可撤掉那个参数。

use wind_config::hotkey::KeyDownPolicy;
use wind_host::KeyProbe;
use wind_keys::keymap;

use crate::coordinator::Coordinator;

/// 修饰位：本模块内部用的规范化形式（与 `wind_ipc::protocol` 的通用位对齐）。
const MOD_SHIFT: u32 = 0x0001;
/// 宿主快捷键修饰位（Ctrl/Alt/Win，macOS 的 Command 走 Win 位）。见
/// `wind_ipc::protocol::MOD_SHORTCUT` 的说明。
const MOD_SHORTCUT: u32 = wind_ipc::protocol::MOD_SHORTCUT;

/// 与 `wind_config::hotkey` 的 `key_hash` 同构（高 16 位修饰、低 16 位键码）。
fn key_hash(modifiers: u32, vk: u32) -> u32 {
    (modifiers << 16) | (vk & 0xFFFF)
}

/// 只有**有输入会话**时才承担输入法语义的键。
///
/// 空缓冲时它们该交还宿主：空格出空格、回车换行、退格删字、方向键移光标、数字出数字。
/// 有会话时它们是上屏/取消/删码/翻页/选词。
fn is_session_only_key(vk: u32) -> bool {
    matches!(
        vk,
        keymap::VK_SPACE
            | keymap::VK_RETURN
            | keymap::VK_BACK
            | keymap::VK_ESCAPE
            | keymap::VK_LEFT
            | keymap::VK_RIGHT
            | keymap::VK_UP
            | keymap::VK_DOWN
            | keymap::VK_PRIOR
            | keymap::VK_NEXT
    ) || is_digit(vk)
}

fn is_digit(vk: u32) -> bool {
    (0x30..=0x39).contains(&vk)
}

fn is_letter(vk: u32) -> bool {
    (0x41..=0x5A).contains(&vk)
}

/// OEM 标点键（`;'`、`,.`、`-=`、`[]`、`/\`、`` ` ``）。中文模式下要转中文标点，故要吃。
fn is_punct(vk: u32) -> bool {
    matches!(
        vk,
        keymap::VK_SEMICOLON
            | keymap::VK_QUOTE
            | keymap::VK_COMMA
            | keymap::VK_PERIOD
            | keymap::VK_MINUS
            | keymap::VK_EQUAL
            | keymap::VK_LBRACKET
            | keymap::VK_RBRACKET
            | keymap::VK_SLASH
            | keymap::VK_BACKSLASH
            | keymap::VK_BACKTICK
    )
}

impl Coordinator {
    /// **该键是否交给输入法处理。**
    ///
    /// 返回 `false` 时宿主必须执行默认行为，且**不要**再调用 `handle_key_event`。
    ///
    /// 判定顺序镜像 TSF `OnTestKeyDown`（顺序本身有语义，热键优先于常规分类）：
    /// 1. 宿主只读 → 一律放行
    /// 2. key_down 热键白名单（四种策略）
    /// 3. Ctrl/Alt/Cmd 组合 → 归宿主快捷键
    /// 4. 配对跳出：配对栈非空时吃跳出键（跨中英模式统一闸门）
    /// 5. 英文模式 → 字母/数字/标点全放行（配对由上一条兜住）
    /// 6. 中文模式 → 字母、标点（含 Shift+数字的上挡标点）吃；会话键有会话才吃
    pub fn should_handle_key(&self, probe: &KeyProbe) -> bool {
        if probe.host_readonly {
            return false;
        }

        let mods = probe.modifiers.0 & (MOD_SHIFT | MOD_SHORTCUT);
        let hash = key_hash(mods, probe.vk);
        let chinese = self.is_chinese_mode();
        let session = self.has_active_session();

        // ── 2. 热键白名单 ──
        if let Some(policy) = self.rt().compiled_hotkeys.key_down_policy(hash) {
            match policy {
                KeyDownPolicy::Always => return true,
                KeyDownPolicy::ChineseOnly => return chinese,
                KeyDownPolicy::Session => return chinese && session,
                // 仅转发：有会话时吃；无会话**继续往下**按常规分类判
                // （中文模式下 `-=` `;'` 要当标点处理，直接放行会丢标点）
                KeyDownPolicy::ForwardOnly => {
                    if session {
                        return true;
                    }
                }
            }
        }

        // ── 3. Ctrl/Alt/Cmd 组合归宿主 ──
        // 未命中热键白名单的快捷键组合一律不碰，否则会吃掉宿主的 Ctrl+C / ⌘C 之类。
        if mods & MOD_SHORTCUT != 0 {
            return false;
        }

        // ── 4. 配对跳出（跨模式统一闸门）──
        // 栈非空本身就蕴含「开过配对、尚未跳出」，没配对时一个 Tab/Enter 都不会被吃。
        if self.has_pending_pair() && is_jump_out_key(probe.vk) {
            return true;
        }

        // ── 5. 英文模式 ──
        // 字母/数字/标点全部放行给宿主直出。这条是「切到英文打不出字」的修复点：
        // 此前照送进核心，核心返回 Consumed 而无文本，宿主两头落空。
        if !chinese {
            return false;
        }

        // ── 6. 中文模式 ──
        // Shift+数字是**上挡标点**（`!@#$%^&*()`），不是数字键：它要走标点转换，
        // 空缓冲时也得吃。必须排在下面那条之前——[`is_session_only_key`] 只看 VK、
        // 不看修饰位，会把 Shift+1 一并判成「数字，空缓冲交还宿主」。
        //
        // 漏掉这一条的表现：中文标点态下上挡整排标点**绕过标点层直出半角**
        // （`!` 而不是 `！`），而且只在空缓冲时如此——有编码时它们又是全角，
        // 于是看起来像「有时候转有时候不转」。安卓端实测就是这个形状。
        if mods & MOD_SHIFT != 0 && is_digit(probe.vk) {
            return true;
        }
        if is_session_only_key(probe.vk) {
            return session;
        }
        is_letter(probe.vk) || is_punct(probe.vk)
    }

    /// 当前是否有活跃输入会话（编码缓冲非空 **或** 有候选）。
    ///
    /// 判据必须含候选：顶码/联想等场景下缓冲已清空但候选仍在，此时回车/退格仍是
    /// 输入法语义。只看缓冲会让这些键在候选未消时漏给宿主。
    pub fn has_active_session(&self) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::has_input_session(&s)
    }

    /// 配对栈是否有待跳出的右半（`(` 已插入、`)` 尚未跳过）。
    pub fn has_pending_pair(&self) -> bool {
        !self
            .pair_tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

/// 配对跳出键：把光标移出已插入的右半。
fn is_jump_out_key(vk: u32) -> bool {
    matches!(vk, keymap::VK_TAB | keymap::VK_RETURN | keymap::VK_RIGHT)
}
