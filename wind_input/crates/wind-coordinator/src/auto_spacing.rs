//! 中英 / 中数间按需补空格的统一出口（设计见 `docs/design/auto-spacing-and-latin-smart-punct.md`）。
//!
//! 空格是**下一笔上屏的前缀**：上屏点只管产出文本（英文词出口另外打一个「这是一个英文词」
//! 的标记），补不补、补什么由这里按交界一次性决定。上屏点有几十个，散点接线必漏——同
//! `handle_key_event_policed` 出口那几道收口的理由。
//!
//! 两个出口调到这里：桌面按键（`handle_key_event_policed`）与候选点选（`select_candidate_at`，
//! 鼠标与移动端候选栏）。移动端按键路径不经前者：移动端有自己的一套英文自动空格
//! （`wind-mobile` 的 `auto_space`），客户端也不报 `seq_char`，本功能在那里不触发。

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

    /// 统一出口：按交界给本笔上屏文本补前缀空格，并把「插入点前一字符」推进到本笔末尾。
    ///
    /// 无论补不补都要清掉 `spacing_word_commit`：它只描述**这一次**的上屏。
    pub(crate) fn apply_auto_spacing(&self, state: &mut State, mut action: KeyAction) -> KeyAction {
        let word = std::mem::take(&mut state.spacing_word_commit);
        if let KeyAction::ReplaceBackward { text, .. }
        | KeyAction::CommitReplacingHeld { text, .. } = &action
        {
            // 智能符号替换：光标前换成了替换文本，不补空格（只发生在标点上）。
            if let Some(c) = text.chars().last() {
                state.spacing_prev = classify(c);
            }
            return action;
        }
        let full_width = state.full_width;
        let hold_tail = match &action {
            KeyAction::CommitAndHoldComposition { hold_text, .. } => hold_text.chars().last(),
            _ => None,
        };
        let Some(text) = committed_text_mut(&mut action) else {
            return action;
        };
        self.space_committed_text(&mut state.spacing_prev, full_width, word, text);
        // 先提交再待定一个中文标点：光标前是那个标点（落定与否都是它）。
        if let Some(c) = hold_tail {
            state.spacing_prev = classify(c);
        }
        action
    }

    /// 文本级的同一判据：按交界补前缀，再把 `prev` 推进到本笔末字符。空文本什么都不动。
    ///
    /// 不经 `KeyAction` 的上屏出口（barrier 的 `CommitResultData`）直接调这个。
    pub(crate) fn space_committed_text(
        &self,
        prev: &mut CharClass,
        full_width: bool,
        word: bool,
        text: &mut String,
    ) {
        let Some(head) = text.chars().next() else {
            return;
        };
        let spacing = &self.rt().config.input.spacing;
        let allowed = (spacing.cjk_latin || spacing.cjk_digit) && self.auto_spacing_allowed_here();
        let rules = SpacingRules {
            cjk_latin: allowed && spacing.cjk_latin,
            cjk_digit: allowed && spacing.cjk_digit,
            word_space: word,
        };
        let next = classify(head);
        if needs_space(*prev, next, rules) {
            // 全角字母之间的词间空格补全角空格，与旧的立即补口径一致。
            let sp = if full_width && next == CharClass::WideLatin {
                "\u{3000}"
            } else {
                " "
            };
            text.insert_str(0, sp);
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

    #[test]
    fn word_commit_flag_is_one_shot() {
        let c = coord(false, false);
        let mut st = State {
            spacing_prev: CharClass::Latin,
            spacing_word_commit: true,
            ..Default::default()
        };
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("world"))),
            " world"
        );
        assert!(!st.spacing_word_commit);
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("again"))),
            "again"
        );
    }

    #[test]
    fn full_width_word_space() {
        let c = coord(false, false);
        let mut st = State {
            full_width: true,
            spacing_prev: CharClass::WideLatin,
            spacing_word_commit: true,
            ..Default::default()
        };
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("ｗｏｒｌｄ"))),
            "\u{3000}ｗｏｒｌｄ"
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
    fn per_app_rule_turns_off_cjk_spacing_but_not_word_space() {
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
        // 词间空格属英文输入本身，不归应用规则管。
        st.spacing_word_commit = true;
        assert_eq!(
            text_of(&c.apply_auto_spacing(&mut st, insert("def"))),
            " def"
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
