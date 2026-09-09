//! Unicode 码点输入模式（前缀夺取 + 求值候选）
//!
//! `u+4e00` → 一。与 [`crate::handle_url`] 同属**前缀夺取式**，共用那边的
//! [`Coordinator::try_prefix_hijack`] 闸门与 `Rewind` 回退骨架；差别只在缓冲怎么解释——
//! url 原样累积文本、不产候选，本模式把缓冲当十六进制求值、产一条候选。
//!
//! # 两个入口，一张前缀表
//!
//! - `u+`（小写）：小写 u 进码表 `input_buffer`，`+` 在 `try_prefix_hijack` 触发夺取；
//! - `U+`（大写）：Shift+U 先被 `try_activate_mode` 截进**临时英文**（`input_buffer` 此刻
//!   是空的，夺取点看不见那个 U），由 `handle_temp.rs` 的转交分支交过来。
//!
//! 两条都查同一份 `input.unicode.prefixes`，且各自带上自己的 [`RewindOrigin`]——退格退到
//! 前缀边界时，前者回码表输入流、后者回临英缓冲。

use crate::coordinator::{Coordinator, State, numpad_char, printable_char};
use crate::pipeline::{ModeKind, Rewind, RewindOrigin};
use crate::preedit_cursor;
use tracing::debug;
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::Candidate;
use wind_ipc::protocol::MOD_SHIFT;
use wind_keys::keymap;

/// 十六进制部分的最大位数。`0x10FFFF` 是 Unicode 的最大码点，六位足够表达。
///
/// 有上限不只是防呆：没有它，`u+000000000041` 这类前导零串会一路解析成功，
/// 而用户多半是打错了——超长即判无效，比默默出一个 `A` 更可解释。
const MAX_HEX_DIGITS: usize = 6;

impl Coordinator {
    /// 探针是否恰好等于某个 Unicode 前缀。
    ///
    /// **字面比较，不做大小写归一** —— 与 [`Coordinator::is_url_prefix`] 的刻意差异，
    /// 理由见 `UnicodeConfig::prefixes`。
    pub(crate) fn is_unicode_prefix(&self, probe: &str) -> bool {
        self.rt()
            .config
            .input
            .unicode
            .prefixes
            .iter()
            .any(|p| !p.is_empty() && p == probe)
    }

    /// 缓冲开头那段前缀的字节长度；没有任何前缀匹配时为 0。
    ///
    /// 取**最长**匹配：前缀表由用户配置，出现 `u+` 与 `u+x` 这种包含关系时，短的那条会
    /// 把后一个字符错判成十六进制位。取最长与「夺取时用的是哪条」一致——夺取要求探针
    /// 与前缀**全等**，进来的缓冲就是那条前缀本身。
    fn unicode_prefix_len(&self, buffer: &str) -> usize {
        self.rt()
            .config
            .input
            .unicode
            .prefixes
            .iter()
            .filter(|p| !p.is_empty() && buffer.starts_with(p.as_str()))
            .map(|p| p.len())
            .max()
            .unwrap_or(0)
    }

    /// 剥掉前缀后剩下的十六进制部分（可能为空 = 用户刚进来还没打码点）。
    fn unicode_hex_part<'a>(&self, buffer: &'a str) -> &'a str {
        &buffer[self.unicode_prefix_len(buffer)..]
    }

    /// 十六进制串 → 字符。无效一律 `None`，调用方据此决定「不出候选」。
    ///
    /// `char::from_u32` 已经替我们挡住了两类非法码点：代理区 `D800..=DFFF`（它们只在
    /// UTF-16 编码内部有意义，不是字符）与 `> 0x10FFFF`。故这里只需再管长度与字符集。
    fn unicode_decode(hex: &str) -> Option<char> {
        if hex.is_empty() || hex.len() > MAX_HEX_DIGITS {
            return None;
        }
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        char::from_u32(u32::from_str_radix(hex, 16).ok()?)
    }

    /// 进入 Unicode 模式：以触发前缀作初始缓冲，清空普通输入/候选。
    ///
    /// `origin` 决定回退目标（见 [`RewindOrigin`]），同时决定快照从哪个缓冲取——两者
    /// 必须同源，否则回退会把 A 流的内容放回 B 流。
    pub(crate) fn enter_unicode_mode(
        &self,
        state: &mut State,
        buffer: String,
        origin: RewindOrigin,
    ) -> KeyAction {
        let snapshot = match origin {
            // 夺取前的正常输入（前缀的最后一字符是刚补全的那一键，故快照即当前 input_buffer）。
            RewindOrigin::Normal => state.input_buffer.clone(),
            // 临英转交：快照是临英缓冲里已有的那部分（`U`），`+` 尚未进任何缓冲。
            RewindOrigin::TempEnglish => state.temp_english_buffer.clone(),
        };
        state.input_buffer.clear();
        state.temp_english_buffer.clear();
        state.temp_english_prefix.clear();
        state.temp_english_cursor = 0;
        state.candidates.clear();
        state.active = Some(ModeKind::Unicode);
        state.unicode_buffer = buffer.clone();
        state.unicode_cursor = state.unicode_buffer.len(); // 进入时缓冲已有前缀，光标落末尾
        state.rewind = Some(Rewind {
            snapshot,
            host_text: buffer,
            origin,
        });
        self.update_unicode_candidates(state);
        let disp = state.preedit.clone();
        self.notify_ui_update(state);
        debug!(
            "Entered Unicode mode (buffer={}, origin={:?})",
            state.unicode_buffer, origin
        );
        KeyAction::UpdateComposition {
            text: disp.clone(),
            caret_pos: disp.chars().count() as u32,
        }
    }

    /// 退出 Unicode 模式并清空相关状态（含作废回退登记）。
    pub(crate) fn exit_unicode_mode(&self, state: &mut State) {
        state.active = None;
        state.unicode_buffer.clear();
        state.unicode_cursor = 0;
        state.candidates.clear();
        state.preedit.clear();
        state.rewind = None;
    }

    /// 刷新候选：缓冲的十六进制部分解析成功则出一条，否则**不出候选**。
    ///
    /// # ★ 无效码点为什么不出「提示行」候选
    ///
    /// 初版设计想在无效时摆一条「码点无效」的提示候选，免得候选窗空着像卡死。但候选窗里
    /// 的东西是**能被空格上屏的**——用户打错一位再按空格，屏幕上就会出现「码点无效」四个
    /// 字。空列表 + 模式徽标已经足够表达「这串不是有效码点」，且与网址模式（恒无候选）
    /// 的观感一致。无效时按空格上屏的是缓冲原文（见 [`Self::handle_unicode_key`]）。
    pub(crate) fn update_unicode_candidates(&self, state: &mut State) {
        state.candidates.clear();
        self.reset_candidate_view(state);
        state.preedit = state.unicode_buffer.clone();
        let hex = self.unicode_hex_part(&state.unicode_buffer);
        let Some(ch) = Self::unicode_decode(hex) else {
            return;
        };
        // 注释里的区块名与候选右键的「类型」列同源（`wind_candidate::block_of`），
        // 免得同一个字符在两处被叫成不同的名字。
        let comment = format!("U+{:04X} {}", ch as u32, wind_candidate::block_of(ch).name);
        state.candidates.push(Candidate {
            text: ch.to_string(),
            comment,
            ..Default::default()
        });
    }

    /// Unicode 模式按键处理：可见 ASCII 累积；空格/回车上屏；退格删空退出；Esc 放弃。
    ///
    /// 结构与 [`Coordinator::handle_url_key`] 平行（同一类模式，行为不该无谓地分叉），
    /// 唯一的实质差异在空格/回车臂：那边恒上屏原文，这边优先上屏解析出的字符。
    pub(crate) fn handle_unicode_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        let refresh = |this: &Self, state: &mut State| -> KeyAction {
            this.update_unicode_candidates(state);
            this.notify_ui_update(state);
            KeyAction::UpdateComposition {
                text: state.unicode_buffer.clone(),
                caret_pos: this.overlay_caret(state),
            }
        };
        // Ctrl/Alt 组合守卫：必须最先，否则组合键会落到下方 `printable_char` 臂被当成
        // 码点字符（`Ctrl+V` 粘贴时凭空多一个 v）。同 `handle_url_key`。
        if let Some(act) =
            self.overlay_ctrl_alt_guard(state, data, !state.unicode_buffer.is_empty(), |s, st| {
                s.exit_unicode_mode(st)
            })
        {
            return act;
        }
        // 会话态按键绑定（`keys.session_actions`）。网址模式当年漏接这条，`cancel` 动词
        // 一加进来就变成「Tab 按了没反应」——新 overlay 一律先接上，别等动词来了才补。
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
                // 退到**前缀边界**再退一次会走 `rewind_hijack`（message_handler 里那道统一
                // 闸门早于本函数），故这里只处理边界之内的删除。
                let backward = data.key_code == keymap::VK_BACK;
                let removed = {
                    let mut ed = preedit_cursor::BufEdit::new(
                        &mut state.unicode_buffer,
                        &mut state.unicode_cursor,
                    );
                    if backward {
                        ed.backspace()
                    } else {
                        ed.delete()
                    }
                };
                if state.unicode_buffer.is_empty() && (removed || backward) {
                    self.exit_unicode_mode(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                } else if removed {
                    refresh(self, state)
                } else {
                    KeyAction::Consumed
                }
            }
            keymap::VK_SPACE | keymap::VK_RETURN => {
                // 有候选（码点有效）→ 上屏那个字符；无候选 → 上屏缓冲原文。
                //
                // 后者与网址模式同口径「打什么上屏什么」：`u+zzz` 这种打错的串，把原文还
                // 给用户比吞掉它好——用户至少能看见自己打了什么，改一位重来即可。
                let text = state
                    .candidates
                    .first()
                    .map(|c| c.text.clone())
                    .unwrap_or_else(|| state.unicode_buffer.clone());
                // 统计来源用 `RawInput`（原始编码上屏）而**不新增枚举值**：`CommitSource`
                // 带显式判别值且 `COUNT` 是 `by_source` 数组的长度，加一项会动到已落盘的
                // 统计结构。语义上也站得住——码点是用户以原始形式直接指定的，不是从词库
                // 选出来的候选。
                self.record_commit(&text, 0, -1, wind_store::stats::CommitSource::RawInput);
                self.exit_unicode_mode(state);
                self.notify_ui_hide();
                if text.is_empty() {
                    KeyAction::ClearComposition
                } else {
                    Self::commit_action(text, true)
                }
            }
            _ => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                // 小键盘（direct 语义）回退 `numpad_char`：十六进制含 0-9，与主键盘同待遇。
                if let Some(ch) =
                    printable_char(data.key_code, shift).or_else(|| numpad_char(data.key_code))
                {
                    preedit_cursor::BufEdit::new(
                        &mut state.unicode_buffer,
                        &mut state.unicode_cursor,
                    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_accepts_valid_code_points() {
        assert_eq!(Coordinator::unicode_decode("4e00"), Some('一'));
        assert_eq!(Coordinator::unicode_decode("4E00"), Some('一')); // 大小写十六进制都收
        assert_eq!(Coordinator::unicode_decode("41"), Some('A'));
        assert_eq!(Coordinator::unicode_decode("1F600"), Some('😀')); // BMP 之外
        assert_eq!(
            Coordinator::unicode_decode("10FFFF"),
            char::from_u32(0x10FFFF)
        );
    }

    #[test]
    fn decode_rejects_invalid_input() {
        assert_eq!(Coordinator::unicode_decode(""), None, "空串没有码点可言");
        assert_eq!(Coordinator::unicode_decode("zz"), None, "非十六进制字符");
        assert_eq!(Coordinator::unicode_decode("4e00 "), None, "尾随空格不容忍");
        assert_eq!(
            Coordinator::unicode_decode("D800"),
            None,
            "代理区不是字符——靠 char::from_u32 挡住，别自己再写一遍范围判定"
        );
        assert_eq!(Coordinator::unicode_decode("110000"), None, "超出 0x10FFFF");
        assert_eq!(
            Coordinator::unicode_decode("0000041"),
            None,
            "七位：超长即判无效，不做前导零裁剪"
        );
    }
}
