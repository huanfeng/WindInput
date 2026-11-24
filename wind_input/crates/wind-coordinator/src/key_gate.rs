//! 吃键判定：唯一真相源。
//!
//! 「这个键该不该由输入法处理」曾在 C++ `CKeyEventSink::OnTestKeyDown`、协调器各处分支、
//! Android FFI 手写谓词里各有一份，漂移的表现是协调器对不该收的键返回 `Consumed`、宿主既不
//! 上屏也不执行默认行为，键静默消失。本模块把判据收进核心：宿主侧只做自己独有的前置（只读
//! 上下文、密码框、自注入按键跳过）与后置（TSF 的 pending toggle 计时），中间的判定一律调
//! [`Coordinator::should_handle_key`]。
//!
//! # 迁移状态
//!
//! - Android：已切到本模块。
//! - C++ TSF：尚未迁移。迁移时 C++ 只保留 TSF 特有的前置/后置，中间整段替换。
//! - macOS IMKit：没有前置闸门，每个 keyDown 都发给协调器，本模块第 3 条（快捷键组合归宿主）
//!   在那边不生效；「有组合时按 ⌘C」返回的 `ClearComposition` 由
//!   `BridgeResponseRouter.apply(hostShortcut:)` 顶住，迁移本模块后可撤掉那个参数。

use wind_config::hotkey::KeyDownPolicy;
use wind_host::KeyProbe;
use wind_keys::keymap;

use crate::coordinator::Coordinator;
use crate::key_convert::punct_char;

/// 修饰位：本模块内部用的规范化形式（与 `wind_ipc::protocol` 的通用位对齐）。
const MOD_SHIFT: u32 = 0x0001;
/// 宿主快捷键修饰位（Ctrl/Alt/Win，macOS 的 Command 走 Win 位）。见
/// `wind_ipc::protocol::MOD_SHORTCUT` 的说明。
const MOD_SHORTCUT: u32 = wind_ipc::protocol::MOD_SHORTCUT;

/// 与 `wind_config::hotkey` 的 `key_hash` 同构（高 16 位修饰、低 16 位键码）。
fn key_hash(modifiers: u32, vk: u32) -> u32 {
    (modifiers << 16) | (vk & 0xFFFF)
}

/// 只有有输入会话时才承担输入法语义的键。
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
    /// 该键是否交给输入法处理。
    ///
    /// 返回 `false` 时宿主必须执行默认行为，且不要再调用 `handle_key_event`。
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
                // 仅转发：有会话时吃；无会话继续往下按常规分类判
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
        //
        // 先问「这个键该不该透传」，它横跨上挡符号与 OEM 标点两类，故提在分类之前。产物不变的
        // 标点不能吃了再原样吐回：非 TSF-aware 宿主会把字符码当 VK 解释（见
        // docs/architecture/smart-symbol-compat-notes.md「产物不变的标点要透传」）。
        //
        // 集合取 `{cn,en}_passthrough_punct_chars`，与推给 DLL 的那两份同源，两侧漂移就是老问题。
        // 两道动态闸门叠在集合之外（是当下状态不是配置）：有输入会话 ⇒ 顶码语义必须吃；
        // 全角 ⇒ `#` 要出 `＃`，必须吃。
        if !session && let Some(ch) = punct_char(probe.vk, mods & MOD_SHIFT != 0) {
            let (full_width, chinese_punct) = {
                let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                (s.full_width, s.chinese_punct)
            };
            let rt = self.rt();
            // 按当下标点态二选一。英文标点态那份是中文那份的超集：那个态不走中文标点
            // 表，`,` `.` `;` 这些的产物也是原样 ASCII，撞码还更凶（`.`→VK_DELETE 吞字符）。
            let set = if chinese_punct {
                &rt.cn_passthrough_punct_chars
            } else {
                &rt.en_passthrough_punct_chars
            };
            if !full_width && set.contains(&ch) {
                return false;
            }
        }

        // Shift+数字是上挡标点（`!@#$%^&*()`），要走标点转换，空缓冲时也得吃。必须排在下面那条
        // 之前：[`is_session_only_key`] 只看 VK、不看修饰位，会把 Shift+1 判成「数字，空缓冲交还
        // 宿主」，表现为上挡标点只在空缓冲时直出半角。
        if mods & MOD_SHIFT != 0 && is_digit(probe.vk) {
            return true;
        }
        if is_session_only_key(probe.vk) {
            return session;
        }
        is_letter(probe.vk) || is_punct(probe.vk)
    }

    /// 当前是否有活跃输入会话（编码缓冲非空 或 有候选）。
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

#[cfg(test)]
mod tests {
    use super::*;
    use wind_config::Config;

    fn coord() -> std::sync::Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.input.symbol.smart_mode = false;
        Coordinator::new_headless(cfg, None)
    }

    /// 透传用哪份集合，取决于当下标点态。必须写在 crate 内部：`tests/` 够不着 `state`，在那边
    /// 写就是永远走中文态分支的假护栏。用 `'` 判：中文态转 `‘`/`’` 必须吃，英文态该透传。
    #[test]
    fn passthrough_set_follows_punct_mode() {
        let c = coord();
        let quote = KeyProbe::new(keymap::VK_QUOTE);

        c.state.lock().unwrap().chinese_punct = true;
        assert!(
            c.should_handle_key(&quote),
            "中文标点态下 `'` 要转中文引号，必须吃"
        );

        c.state.lock().unwrap().chinese_punct = false;
        assert!(
            !c.should_handle_key(&quote),
            "英文标点态下 `'` 产物即原样，吃了也只能原样吐回，应透传"
        );
    }

    /// 全角闸门优先于集合：两个标点态下都必须吃。
    #[test]
    fn full_width_overrides_passthrough_in_both_punct_modes() {
        let c = coord();
        let quote = KeyProbe::new(keymap::VK_QUOTE);
        for punct in [true, false] {
            {
                let mut s = c.state.lock().unwrap();
                s.chinese_punct = punct;
                s.full_width = true;
            }
            assert!(
                c.should_handle_key(&quote),
                "全角下 `'` 要出全角形，必须吃（chinese_punct={punct}）"
            );
        }
    }

    /// 没有前置闸门的宿主（Linux / macOS）把每个键都送进 `handle_key_event`：空闲 Esc 必须
    /// 透传，否则浏览器 Vimium 一类收不到（GH#209）；有编码时仍是取消组合。
    #[test]
    fn escape_passes_through_without_a_session() {
        use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
        let esc = KeyEventData {
            key_code: keymap::VK_ESCAPE,
            scan_code: 0,
            modifiers: 0,
            event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        };
        let c = coord();
        let act = c.handle_key_event(&esc);
        assert!(
            matches!(act, KeyAction::PassThrough),
            "空闲 Esc 应透传，实际: {act:?}"
        );

        c.state.lock().unwrap().input_buffer = "ni".into();
        let act = c.handle_key_event(&esc);
        assert!(
            matches!(act, KeyAction::ClearComposition),
            "有编码时 Esc 应取消组合，实际: {act:?}"
        );
        assert!(c.state.lock().unwrap().input_buffer.is_empty());
    }
}
