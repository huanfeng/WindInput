//! 邮箱输入模式（`@` 后缀触发 + 后缀补全）。
//!
//! # 与 [`crate::handle_url`] 的关系
//!
//! 夺取骨架（`try_prefix_hijack` / `active_hijack_buffer` / `can_rewind` /
//! `rewind_hijack`）住在 `handle_url.rs`，本模式**共用**那一套，只在闸门里多一条判据。
//! 本文件的结构刻意与 `handle_url.rs` 逐行平行：同一类模式的按键处理不该无谓地分叉，
//! 分叉的表现是「网址模式里退格/光标/Esc 的行为和邮箱模式不一样」，而没人会想到去查。
//!
//! # 触发判据是**后缀**，不是前缀
//!
//! | | 判据 | 例 |
//! |---|---|---|
//! | 前缀夺取（url / unicode） | 缓冲 + 本键 **全等**某个前缀 | `www.` / `u+` |
//! | 本模式 | 本键是 `@` 且**缓冲非空** | `abc` + `@` |
//!
//! `abc@` 里缓冲是 `abc`，与任何前缀都不相等——照前缀式那条路抄会撞墙
//! （`docs/design/prefix-hijack-modes.md` §5.2 早就点明了）。
//!
//! **空缓冲按 `@` 刻意不触发**：那样用户每次想单独打一个 `@` 都会掉进邮箱模式，而
//! `@` 在中文模式下本就该走标点流水线（出全角＠或半角 @，随 `punct` 配置）。
//! 这也是「缓冲非空」这个条件同时承担的第二个职责——它就是用户名。
//!
//! # C++ 侧不用改
//!
//! `@` 是 Shift+2，在 `CHotkeyManager::ClassifyInputKey` 里归 `Punctuation`，中文模式
//! **无条件吃**。本模式让 Rust 在更多情形下出字，是「C++ 吃键集 ⊆ Rust 出字集」这条
//! 硬约定的安全方向。模式内续打的 `.`/`-`/`_`/数字同理（Punctuation 或 Number，后者
//! 在有 session 时吃）。

use crate::coordinator::{Coordinator, State, numpad_char, printable_char};
use crate::pipeline::{ModeKind, Rewind, RewindOrigin};
use crate::preedit_cursor;
use tracing::debug;
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_ipc::protocol::MOD_SHIFT;
use wind_keys::keymap;

/// 用户名与后缀的分隔符。
pub(crate) const EMAIL_AT: char = '@';

impl Coordinator {
    /// 进入邮箱模式：以 `用户名@` 作初始缓冲，清空普通输入/候选。
    ///
    /// 登记夺取回退：snapshot = 夺取前的正常输入（= 用户名），host_text = `用户名@`
    /// （夺取边界）。退格删到只剩 `abc@` 时再按一次，就回到正常码表输入流的 `abc`。
    pub(crate) fn enter_email_mode(&self, state: &mut State, buffer: String) -> KeyAction {
        // 夺取前的正常 input_buffer 即回退快照（`@` 是刚按下的那一键，不在快照里）。
        let snapshot = state.input_buffer.clone();
        state.input_buffer.clear();
        state.candidates.clear();
        state.active = Some(ModeKind::Email);
        state.email_buffer = buffer.clone();
        state.email_cursor = state.email_buffer.len(); // 夺取进入时缓冲已有内容，光标落末尾
        state.preedit = buffer.clone();
        state.rewind = Some(Rewind {
            snapshot,
            host_text: buffer,
            origin: RewindOrigin::Normal, // 前缀夺取抢的是正常码表输入流
        });
        self.update_email_candidates(state);
        self.notify_ui_update(state);
        let disp = state.email_buffer.clone();
        debug!("Entered email mode (len={})", disp.chars().count());
        KeyAction::UpdateComposition {
            text: disp.clone(),
            caret_pos: disp.chars().count() as u32,
        }
    }

    /// 退出邮箱模式并清空相关状态（含作废回退登记）。
    ///
    /// ⚠️ 与 `active_hijack_buffer` / `rewind_hijack` / `cancel_session` /
    /// `reset_exclusive_modes` 四处**成对**：那边认得本模式、这边漏了收尾，症状是
    /// 「退得出去但状态不对」，且只在退到边界那一次出现。
    pub(crate) fn exit_email_mode(&self, state: &mut State) {
        state.active = None;
        state.email_buffer.clear();
        state.email_cursor = 0;
        state.candidates.clear();
        // 与 `exit_url_mode` 同步：只清候选不清翻页视图的话，`current_page` /
        // `selected_index` 会带着上一次的值进入下一个会话。
        self.reset_candidate_view(state);
        state.preedit.clear();
        state.rewind = None;
    }

    /// 缓冲里 `@` **之后**的部分（已打的后缀片段）。没有 `@` 时返回空串。
    ///
    /// 取**最后一个** `@`：邮箱本体不该含第二个 `@`，但用户手滑打出 `a@b@` 时，按最后
    /// 一个切分才能让他继续把后半截打完，按第一个切则会拿 `b@` 去查后缀、一条也匹配不上。
    pub(crate) fn email_suffix_part(buffer: &str) -> &str {
        match buffer.rfind(EMAIL_AT) {
            Some(i) => &buffer[i + EMAIL_AT.len_utf8()..],
            None => "",
        }
    }

    /// 缓冲里 `@` **之前**的部分（用户名）。没有 `@` 时返回整串。
    pub(crate) fn email_user_part(buffer: &str) -> &str {
        match buffer.rfind(EMAIL_AT) {
            Some(i) => &buffer[..i],
            None => buffer,
        }
    }

    /// 邮箱模式按键处理：可见 ASCII 原样累积；空格/回车上屏；退格删空退出；Esc 放弃。
    ///
    /// 结构与 [`Coordinator::handle_url_key`] 平行，差异只在候选来源与上屏时记什么学习
    /// 数据两处。
    pub(crate) fn handle_email_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        // 缓冲变化后：重算候选 + 同步 preedit + 刷新候选窗，再返回组合区动作。
        let refresh = |this: &Self, state: &mut State| -> KeyAction {
            this.update_email_candidates(state);
            this.notify_ui_update(state);
            KeyAction::UpdateComposition {
                text: state.email_buffer.clone(),
                caret_pos: this.overlay_caret(state),
            }
        };
        // Ctrl/Alt 组合守卫（见 `overlay_ctrl_alt_guard`）：必须最先，否则组合键会落到
        // 下方 `printable_char` 臂被当成邮箱字符（`Ctrl+V` 粘贴时凭空多一个 v）。
        if let Some(act) =
            self.overlay_ctrl_alt_guard(state, data, !state.email_buffer.is_empty(), |s, st| {
                s.exit_email_mode(st)
            })
        {
            return act;
        }
        // 会话态按键绑定（`keys.session_actions`）+ 候选导航。网址模式当年漏接这条，
        // `cancel` 动词一加进来就变成「Tab 按了没反应」——新 overlay 一律先接上。
        if let Some(act) = self.handle_candidate_nav(state, data) {
            return act;
        }
        // 编码区光标移动（左右 / Home / End）
        if let Some(act) = self.overlay_cursor_key(state, data) {
            return act;
        }
        match data.key_code {
            // Esc：放弃退出（无上屏），实现收口在 `cancel_session`。
            keymap::VK_ESCAPE => self.cancel_session(state),
            keymap::VK_BACK | keymap::VK_DELETE => {
                // 退格删光标前 / Delete 删光标后。缓冲被删空 → 退出模式（无论前删后删，
                // 否则会留下空组合区）；本就空缓冲时只有退格退出，Delete 只吃键。
                //
                // 注：退到夺取边界（`abc@`）再按退格走的是 `rewind_hijack`，在
                // `message_handler` 里先于本函数拦截，够不到这里。
                let backward = data.key_code == keymap::VK_BACK;
                let removed = {
                    let mut ed = preedit_cursor::BufEdit::new(
                        &mut state.email_buffer,
                        &mut state.email_cursor,
                    );
                    if backward {
                        ed.backspace()
                    } else {
                        ed.delete()
                    }
                };
                if state.email_buffer.is_empty() && (removed || backward) {
                    self.exit_email_mode(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                } else if removed {
                    refresh(self, state)
                } else {
                    // 退格时光标已在最左 / Delete 时已在末尾：吃掉不透传。
                    KeyAction::Consumed
                }
            }
            // 空格选候选、回车上屏原文（分工见 `mode_completion.rs` 文件头）。
            // ⛔ 不要把这两个键并回一条 —— 那样打了一半的邮箱就再也上不了屏。
            keymap::VK_SPACE => self.commit_email(state, true),
            keymap::VK_RETURN => self.commit_email(state, false),
            _ => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                // 小键盘键（direct 语义）回退 numpad_char：数字/`.`/`-` 都是合法邮箱内容
                // → 与主键盘同样入缓冲（follow_main 时键已在入口归一化）。同网址模式。
                if let Some(ch) =
                    printable_char(data.key_code, shift).or_else(|| numpad_char(data.key_code))
                {
                    preedit_cursor::BufEdit::new(&mut state.email_buffer, &mut state.email_cursor)
                        .insert(ch);
                    refresh(self, state)
                } else {
                    // 其它非可打印键：消费但不改缓冲
                    KeyAction::Consumed
                }
            }
        }
    }
}
