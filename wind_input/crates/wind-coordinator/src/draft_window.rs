//! 滑窗草稿：落屏文本流 + 滑窗切分。
//!
//! 设计见 `docs/design/auto-phrase-draft-layer.md`。本模块只做决策、不做 IO：吐出「待写入
//! 草稿层的词」，取码/查重/落库由协调器完成，打断语义因此可全部单测覆盖。
//!
//! 与 `auto_phrase` 的根本差别是缓冲对象：`AutoPhraseBuf` 缓冲连续单字、多字词上屏即终止，
//! 正常打字一选词组就结算；本模块缓冲最近落屏的文本流，不论逐字还是选词组，汉字都进流。
//!
//! 滑窗必然产出大量杂词，这是预期：过滤发生在使用端（草稿层带有效期、用过才跃迁），
//! 不在产生端。前提是出厂关闭、受众是主动开启的用户。

use crate::handle_addword::is_han;
use std::time::{Duration, Instant};

/// 能进滑窗的字：汉字，外加 〇（U+3007）。
///
/// 〇 不被 [`is_han`] 认，但它在年份、编号里与汉字连用（「二〇二六」）且码表有码，按 [`is_han`]
/// 断会让这类词永远造不出来。只放这一个字而不是「有码就算」：窗口拿不到码表，且个别码表给
/// 标点也配了码。
pub fn is_draft_char(c: char) -> bool {
    is_han(c) || c == '〇'
}

/// 滑窗长度下界的兜底值（配置为 0 时用）。
pub const DEFAULT_MIN_WINDOW: usize = 2;
/// 滑窗长度上界的兜底值（配置为 0 时用）。
///
/// 复用 `[schema.codetable.auto_phrase].max_phrase_len` 的语义与取值。它在滑窗模型下
/// 顺带取消了「超长整条序列整体丢弃」那道闸——滑窗本就只切 2–5 字，不存在「整条序列超长」。
pub const DEFAULT_MAX_WINDOW: usize = 5;
/// 两次落屏之间的最大间隔；超过则视作断流（防跨句拼出杂词）。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// 滑窗切出的一个草稿词，连同其中各字上屏时用户实际选中的码（GH#181）。
///
/// 取码时有提示的字按提示取位，没有的回退单字全码表（见
/// `EngineManager::encode_word_with_hints`）。提示只来自单字上屏：词组上屏时候选带的是
/// 整词的码，拆不出每个字各是哪条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftWord {
    pub text: String,
    /// `(字, 码)`，按字在词中的顺序；同一个字只记第一次。
    pub code_hints: Vec<(char, String)>,
}

/// 落屏文本流的滑窗缓冲。
///
/// 只保留最近 `max_window` 个字：再往前的字不可能参与任何新窗口，留着只是占地方。
#[derive(Debug, Default)]
pub struct DraftWindowBuf {
    /// 流里的字，各带上屏时登记的码（没有则 `None`）。
    chars: Vec<(char, Option<String>)>,
    /// 上一次落屏的时刻；`None` = 流为空。
    last_at: Option<Instant>,
    /// 选词出口登记、下一次落屏消费的「这个字是用哪条码打出来的」。
    ///
    /// 走登记而不是让落屏动作自己带码：落屏收口在 `note_commit_action`，那里只看得到
    /// `KeyAction`（只有文本）；选中的候选只在选词出口手里。两者在同一次按键内先后发生，
    /// 下一次落屏无论是什么都会把它取走，故不会跨键串到别的字上。
    pending_hint: Option<(char, String)>,
}

impl DraftWindowBuf {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    #[cfg(test)]
    fn buffered(&self) -> String {
        self.chars.iter().map(|(c, _)| *c).collect()
    }

    /// 登记：下一次落屏若恰是单字 `ch`，它是用 `code` 打出来的。
    pub fn set_code_hint(&mut self, ch: char, code: String) {
        self.pending_hint = Some((ch, code));
    }

    /// 作废未消费的登记（本次按键没有落屏）。
    pub fn clear_code_hint(&mut self) {
        self.pending_hint = None;
    }

    /// 文本落屏。返回本次新产生的窗口（可能为空）。
    ///
    /// `text` 含任何非汉字即视为断流（标点、英文、数字、空格都是句子边界），判据与
    /// `feed_auto_phrase` 的 `all_han` 一致。多字词逐字推进，每个字各自产出以它结尾的窗口
    /// ——这是与 `AutoPhraseBuf`（多字词是终止符）的分界。
    pub fn on_commit(
        &mut self,
        text: &str,
        now: Instant,
        idle_timeout: Duration,
        min_window: usize,
        max_window: usize,
    ) -> Vec<String> {
        self.on_commit_hinted(text, now, idle_timeout, min_window, max_window)
            .into_iter()
            .map(|w| w.text)
            .collect()
    }

    /// 同 [`Self::on_commit`]，但窗口带上各字登记过的上屏码（见 [`DraftWord`]）。
    ///
    /// 消费 [`Self::set_code_hint`] 的登记：本次落屏恰是那个单字时随字进流，否则作废。
    pub fn on_commit_hinted(
        &mut self,
        text: &str,
        now: Instant,
        idle_timeout: Duration,
        min_window: usize,
        max_window: usize,
    ) -> Vec<DraftWord> {
        if text.is_empty() {
            return Vec::new();
        }
        let hint = self.pending_hint.take();
        let mut cs = text.chars();
        let hint = match (cs.next(), cs.next(), hint) {
            (Some(c), None, Some((h, code))) if c == h => Some(code),
            _ => None,
        };
        if !text.chars().all(is_draft_char) {
            self.terminate();
            return Vec::new();
        }
        // 距上次落屏太久 ⇒ 断流，本次以新段起头。
        if self
            .last_at
            .is_some_and(|t| now.saturating_duration_since(t) > idle_timeout)
        {
            self.terminate();
        }
        self.last_at = Some(now);

        let (min, max) = normalize_bounds(min_window, max_window);
        let mut out = Vec::new();
        for ch in text.chars() {
            self.chars.push((ch, hint.clone()));
            // 只保留可能参与后续窗口的那一段。
            if self.chars.len() > max {
                let drop = self.chars.len() - max;
                self.chars.drain(..drop);
            }
            // 以本字结尾、长度 min..=max 的全部窗口。
            let n = self.chars.len();
            for len in min..=max.min(n) {
                let span = &self.chars[n - len..];
                let mut code_hints: Vec<(char, String)> = Vec::new();
                for (c, code) in span {
                    if let Some(code) = code
                        && !code_hints.iter().any(|(h, _)| h == c)
                    {
                        code_hints.push((*c, code.clone()));
                    }
                }
                out.push(DraftWord {
                    text: span.iter().map(|(c, _)| *c).collect(),
                    code_hints,
                });
            }
        }
        out
    }

    /// 回改已落屏的内容（`KeyAction::ReplaceBackward`）：先回退 `utf16_count` 个
    /// UTF-16 单元，调用方随后再把替换文本喂进 [`Self::on_commit`]。
    ///
    /// 量纲与 `ReplaceBackward.count`（发给宿主的删除量）对齐：扩展区汉字占 2 个单元，
    /// 按 char 回退会多删一个字。回退量超过缓冲现有内容时清空：剩下的字与新文本的
    /// 连续性已不可知，继续切窗口就是在造幽灵词。
    pub fn rewind(&mut self, utf16_count: usize) {
        let mut remaining = utf16_count;
        while remaining > 0 {
            match self.chars.pop() {
                Some((c, _)) => remaining = remaining.saturating_sub(c.len_utf16()),
                None => break,
            }
        }
        if self.chars.is_empty() {
            self.last_at = None;
        }
    }

    /// 断流（标点/英文/数字上屏、焦点丢失、IME 停用、模式切换、切换方案、光标移动、idle 超时）。
    ///
    /// 语义是「流到此为止，下一段重新起」，不是 `AutoPhraseBuf::terminate` 那种
    /// 「把攒的字结算成一个词」——本模型在每次落屏时就已经把窗口吐出去了，断流时没有
    /// 什么待结算的东西。
    pub fn terminate(&mut self) {
        self.chars.clear();
        self.last_at = None;
        self.pending_hint = None;
    }
}

/// 规整长度上下界：0 用兜底值，且保证 `min <= max`。
///
/// `min > max` 时取 `max = min`（只产出一种长度的窗口）而不是静默返回空：配置写反了应当
/// 表现为「窗口变少」，不该表现为「造词功能整个失灵」——后者会被当成缺陷来查很久。
fn normalize_bounds(min_window: usize, max_window: usize) -> (usize, usize) {
    let min = if min_window == 0 {
        DEFAULT_MIN_WINDOW
    } else {
        min_window
    };
    let max = if max_window == 0 {
        DEFAULT_MAX_WINDOW
    } else {
        max_window
    };
    (min, max.max(min))
}

#[cfg(test)]
#[path = "draft_window_tests.rs"]
mod tests;
