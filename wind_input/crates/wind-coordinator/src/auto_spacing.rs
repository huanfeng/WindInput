//! 中英 / 中数**切换处**补空格的统一出口（设计见 `docs/design/auto-spacing-and-latin-smart-punct.md`）。
//!
//! 空格是**下一笔上屏的前缀**：上屏点只管产出文本，补不补由这里按交界（上一笔末字符 →
//! 本笔首字符）一次性决定。上屏点有几十个，散点接线必漏——同 `handle_key_event_policed`
//! 出口那几道收口的理由。
//!
//! 英文词的词间空格**不归这里**：开了 `commit_space` 的英文上屏照旧立即补尾随空格（可预期：
//! 打完 `hello` 就看得见后面有没有空格），尾随空格之后的中文看到的前一字符是空格，自然不再补。
//! 连续的英文之间这里一概不管。
//!
//! 两个出口调到这里：桌面按键（`handle_key_event_policed`）与候选点选（`select_candidate_at`，
//! 鼠标与移动端候选栏）。移动端按键路径不经前者，客户端也不报 `seq_char`，本功能在那里不触发。

use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_punct::spacing::{CharClass, SpacingRules, classify, classify_unit, needs_space};

use crate::coordinator::{Coordinator, State};

/// 动作里**真写进文档**的那段文本（可改写前缀）。组合区、待定标点不算。
fn committed_text_mut(action: &mut KeyAction) -> Option<&mut String> {
    match action {
        KeyAction::InsertText { text, .. } | KeyAction::InsertTextWithCursor { text, .. } => {
            Some(text)
        }
        KeyAction::CommitAndHoldComposition { commit_text, .. }
        | KeyAction::CommitThenDeferComposition { commit_text, .. } => Some(commit_text),
        _ => None,
    }
}

impl Coordinator {
    /// 无输入会话时的 keydown：按客户端上报的输入序列重新锁存「插入点前一字符」。
    ///
    /// 宿主读得回文档、且光标前已是空白时按 `Other` 处理——只会少补不会多补，防的是
    /// Markdown 编辑器插件自己补了空格（输入序列看不见别人写进文档的东西）。
    /// 客户端不知道（`0` 旧客户端、`SEQ_CHAR_UNKNOWN`）一律 `Other`：宁可少补。
    pub(crate) fn latch_spacing_boundary(&self, state: &mut State, data: &KeyEventData) {
        let doc_space =
            char::from_u32(data.prev_char as u32).is_some_and(|c| c != '\0' && c.is_whitespace());
        state.spacing_prev = if doc_space {
            CharClass::Other
        } else {
            classify_unit(data.seq_char)
        };
    }

    /// 本应用是否允许中英 / 中数交界补空格（compat `auto_spacing = false` 关闭）。
    pub(crate) fn auto_spacing_allowed_here(&self) -> bool {
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_spacing
            != Some(false)
    }

    /// 空闲的数字键（主键盘，或 direct 档的小键盘）是否因「汉字后补空格」由输入法出字。
    ///
    /// Windows：DLL 只在它判定要补空格时才把空闲数字吃下交过来（`NumberKeyPolicy.h` 的
    /// `SpaceBeforeDigit`，条件与这里逐条对应），这里**不看**开关恒出字——DLL 手上的开关可能
    /// 慢一拍，吃了键这边却不出字就是丢键（「C++ 吃键集 ⊆ Rust 出字集」）。
    /// macOS / Linux：客户端每个键都发过来、按回复决定透传，必须看开关与应用规则——否则开关
    /// 关着时汉字后的数字也被输入法接管，不再交给宿主（数字当快捷键的场景）。
    pub(crate) fn digit_after_cjk_outputs(&self, state: &State, data: &KeyEventData) -> bool {
        data.modifiers == 0
            && state.chinese_mode
            && !state.full_width
            && !state.caps_lock
            && !Self::has_input_session(state)
            && wind_punct::spacing::classify_unit(data.seq_char)
                == wind_punct::spacing::CharClass::Cjk
            && (cfg!(windows)
                || (self.rt().config.input.spacing.cjk_digit && self.auto_spacing_allowed_here()))
    }

    /// 统一出口：按交界给本笔上屏文本补前缀空格，并把「插入点前一字符」推进到本笔末尾。
    /// 上屏文本非空时顺带发皮肤装饰的 `commit` 事件（同一个收口，见 [`Self::emit_skin_commit`]）。
    pub(crate) fn apply_auto_spacing(&self, state: &mut State, mut action: KeyAction) -> KeyAction {
        if let KeyAction::ReplaceBackward { text, .. }
        | KeyAction::CommitReplacingHeld { text, .. } = &action
        {
            // 智能符号替换：光标前换成了替换文本，不补空格（只发生在标点上）。
            // 纯删除（撤销上屏）：光标前换成了什么不知道。
            state.spacing_prev = text.chars().last().map_or(CharClass::Other, classify);
            return action;
        }
        let hold_tail = match &action {
            KeyAction::CommitAndHoldComposition { hold_text, .. } => hold_text.chars().last(),
            _ => None,
        };
        let Some(text) = committed_text_mut(&mut action) else {
            return action;
        };
        if !text.is_empty() {
            self.emit_skin_commit();
        }
        let mut prev = state.spacing_prev;
        self.space_committed_text(&mut prev, text);
        state.spacing_prev = prev;
        // 先提交再待定一个中文标点：光标前是那个标点（落定与否都是它）。
        if let Some(c) = hold_tail {
            state.spacing_prev = classify(c);
        }
        action
    }

    /// 文本级的同一判据：按交界补前缀，再把 `prev` 推进到本笔末字符。空文本什么都不动。
    ///
    /// 不经 `KeyAction` 的上屏出口（barrier 的 `CommitResultData`）直接调这个。
    pub(crate) fn space_committed_text(&self, prev: &mut CharClass, text: &mut String) {
        let Some(head) = text.chars().next() else {
            return;
        };
        let spacing = &self.rt().config.input.spacing;
        let allowed = (spacing.cjk_latin || spacing.cjk_digit) && self.auto_spacing_allowed_here();
        let rules = SpacingRules {
            cjk_latin: allowed && spacing.cjk_latin,
            cjk_digit: allowed && spacing.cjk_digit,
        };
        let add = needs_space(*prev, classify(head), rules);
        self.auto_space_prefixed
            .store(add, std::sync::atomic::Ordering::Relaxed);
        if add {
            text.insert(0, ' ');
        }
        if let Some(last) = text.chars().last() {
            *prev = classify(last);
        }
    }
}

#[cfg(test)]
#[path = "auto_spacing_tests.rs"]
mod tests;
