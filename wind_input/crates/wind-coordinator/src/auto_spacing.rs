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
mod tests {
    use super::*;
    use wind_config::config::Config;

    fn coord(cjk_latin: bool, cjk_digit: bool) -> std::sync::Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.input.spacing.cjk_latin = cjk_latin;
        cfg.input.spacing.cjk_digit = cjk_digit;
        Coordinator::new_headless(cfg, None)
    }

    fn insert(t: &str) -> KeyAction {
        KeyAction::InsertText {
            text: t.to_string(),
            new_composition: None,
            has_new_composition: false,
            mode_changed: false,
            chinese_mode: true,
        }
    }

    fn text_of(a: &KeyAction) -> String {
        match a {
            KeyAction::InsertText { text, .. } => text.clone(),
            KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text.clone(),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn skin_commits(rx: &std::sync::mpsc::Receiver<wind_ui_types::UiCommand>) -> usize {
        rx.try_iter()
            .filter(|c| {
                matches!(
                    c,
                    wind_ui_types::UiCommand::SkinSignal(wind_ui_types::SkinSignal::Commit)
                )
            })
            .count()
    }

    #[test]
    fn commit_with_text_signals_skin() {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        let _ = skin_commits(&rx);
        let mut st = c.state.lock().unwrap();
        c.apply_auto_spacing(&mut st, insert("1"));
        drop(st);
        assert_eq!(skin_commits(&rx), 1);
    }

    #[test]
    fn no_text_no_skin_signal() {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        let _ = skin_commits(&rx);
        let mut st = c.state.lock().unwrap();
        c.apply_auto_spacing(&mut st, insert(""));
        c.apply_auto_spacing(&mut st, KeyAction::Consumed);
        c.apply_auto_spacing(&mut st, KeyAction::PassThrough);
        drop(st);
        assert_eq!(skin_commits(&rx), 0);
    }

    #[test]
    fn session_channel_gets_no_skin_signal() {
        let (c, events) = Coordinator::new_session(
            Config::default(),
            None,
            None,
            crate::input_profile::InputProfile::desktop(),
        );
        let _: Vec<_> = events.try_iter().collect();
        let mut st = c.state.lock().unwrap();
        let act = c.apply_auto_spacing(&mut st, insert("1"));
        drop(st);
        assert_eq!(text_of(&act), "1", "上屏本身照常");
        assert_eq!(events.try_iter().count(), 0, "中立通道没有对应事件");
    }

    #[test]
    fn prefixes_latin_after_cjk_and_cjk_after_latin() {
        let c = coord(true, false);
        let mut st = State {
            spacing_prev: CharClass::Cjk,
            ..Default::default()
        };
        let a = c.apply_auto_spacing(&mut st, insert("Python"));
        assert_eq!(text_of(&a), " Python");
        assert_eq!(st.spacing_prev, CharClass::Latin);
        let a = c.apply_auto_spacing(&mut st, insert("很好"));
        assert_eq!(text_of(&a), " 很好");
        let a = c.apply_auto_spacing(&mut st, insert("，"));
        assert_eq!(text_of(&a), "，", "标点永不前补");
        assert_eq!(st.spacing_prev, CharClass::Other);
    }

    #[test]
    fn nothing_when_disabled_or_unknown_prev() {
        let c = coord(false, false);
        let mut st = State {
            spacing_prev: CharClass::Cjk,
            ..Default::default()
        };
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("abc"))),
            "abc"
        );
        let c = coord(true, true);
        let mut st = State::default(); // spacing_prev = Other（不可知）
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("abc"))),
            "abc"
        );
    }

    #[test]
    fn digit_rule_is_separate() {
        let c = coord(true, false);
        let mut st = State {
            spacing_prev: CharClass::Digit,
            ..Default::default()
        };
        assert_eq!(text_of(&c.apply_auto_spacing(&mut st, insert("个"))), "个");
        let c = coord(false, true);
        st.spacing_prev = CharClass::Digit;
        assert_eq!(text_of(&c.apply_auto_spacing(&mut st, insert("个"))), " 个");
    }

    /// 英文词自己带了尾随空格（`commit_space`）：之后的汉字看到的前一字符是空格，不再补；
    /// 连续的英文之间一概不管。
    #[test]
    fn trailing_word_space_and_consecutive_latin() {
        let c = coord(true, false);
        let mut st = State {
            spacing_prev: CharClass::Cjk,
            ..Default::default()
        };
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("Hello "))),
            " Hello "
        );
        assert_eq!(text_of(&c.apply_auto_spacing(&mut st, insert("好"))), "好");
        st.spacing_prev = CharClass::Latin;
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("world"))),
            "world"
        );
    }

    #[test]
    fn top_commit_prefixes_committed_part_only() {
        let c = coord(true, false);
        let mut st = State {
            spacing_prev: CharClass::Latin,
            ..Default::default()
        };
        let a = c.apply_auto_spacing(
            &mut st,
            KeyAction::CommitThenDeferComposition {
                commit_text: "工".into(),
                deferred_composition: "a".into(),
                timeout_ms: 0,
            },
        );
        assert_eq!(text_of(&a), " 工");
        assert_eq!(st.spacing_prev, CharClass::Cjk);
    }

    #[test]
    fn per_app_rule_turns_off_cjk_spacing() {
        let c = coord(true, true);
        c.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_spacing = Some(false);
        let mut st = State {
            spacing_prev: CharClass::Cjk,
            ..Default::default()
        };
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("abc"))),
            "abc"
        );
        c.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_spacing = Some(true);
        st.spacing_prev = CharClass::Cjk;
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("ghi"))),
            " ghi"
        );
    }

    #[test]
    fn latch_prefers_document_whitespace_veto() {
        let c = coord(true, false);
        let mut st = State::default();
        let mut d = KeyEventData {
            key_code: 0x41,
            scan_code: 0,
            modifiers: 0,
            event_type: 0,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0x4F60,
        };
        c.latch_spacing_boundary(&mut st, &d);
        assert_eq!(st.spacing_prev, CharClass::Cjk);
        d.prev_char = ' ' as u16;
        c.latch_spacing_boundary(&mut st, &d);
        assert_eq!(st.spacing_prev, CharClass::Other, "文档里已有空格就不补");
        d.prev_char = 0;
        d.seq_char = 0;
        c.latch_spacing_boundary(&mut st, &d);
        assert_eq!(st.spacing_prev, CharClass::Other);
    }
}
