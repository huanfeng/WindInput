//! 滑窗草稿：落屏文本流 + 滑窗切分。
//!
//! 设计见 `docs/design/auto-phrase-draft-layer.md`。本模块**只做决策、不做 IO**：
//! 吐出「待写入草稿层的词」，取码/查重/落库由协调器完成。打断语义因此可以全部单测覆盖
//! （同 [`crate::auto_phrase`] 的形态）。
//!
//! # 与 `auto_phrase` 的根本差别：缓冲的是什么
//!
//! `AutoPhraseBuf` 缓冲的是**连续单字序列**——多字词上屏即终止，且该词不入缓冲。
//! 那是「猜一个词」模型下的正确选择，但它正是「正常打字不容易进入造词」的主因：
//! 正常打字里大多数词是选词组上屏的，一句话只要选中一次词组，前面攒的字当场被结算、
//! 词组不参与、之后从零开始。
//!
//! 本模块缓冲的是**最近落屏的文本流**：不论逐字打的还是选词组来的，汉字都进流。
//! 「我今天去上班」不管是怎么打出来的，都切成 `我今天 / 今天去 / 天去上 / 去上班 / …`。
//! **主因那道闸是在换缓冲对象这一步顺带消失的，不是被调松的。**
//!
//! # 杂词
//!
//! 滑窗必然产出大量杂词，这是**预期而非缺陷**：模型的重心从「猜一个词」转移到
//! 「先记一堆、用过的才留」，过滤发生在使用端（草稿层带有效期、用过才跃迁），
//! 不在产生端。这个模型敢用的前提是出厂关闭、受众是主动开启的用户。

use crate::handle_addword::is_han;
use std::time::{Duration, Instant};

/// 能进滑窗的字：汉字，外加 〇（U+3007）。
///
/// 〇 落在 CJK 符号区，[`is_han`] 不认它；但它在年份、编号里与汉字连用（「二〇二六」），五笔
/// 码表也给了码（`llll`），取码那一步（`encoder::encode_text`）已让有码的非汉字参与。若窗口仍按
/// [`is_han`] 断，〇 一上屏就把前面的字清掉，这类词永远造不出来。
///
/// 只放这一个字而不是「码表里有码就算」：窗口拿不到码表，且个别码表给标点也配了码，按有码放行
/// 会让标点两侧的字拼进同一个窗口。
pub fn is_draft_char(c: char) -> bool {
    is_han(c) || c == '〇'
}

/// 滑窗长度下界的兜底值（配置为 0 时用）。
pub const DEFAULT_MIN_WINDOW: usize = 2;
/// 滑窗长度上界的兜底值（配置为 0 时用）。
///
/// 复用 `[schema.codetable.auto_phrase].max_phrase_len` 的语义与取值。★ 它在滑窗模型下
/// 顺带取消了「超长整条序列整体丢弃」那道闸——滑窗本就只切 2–5 字，不存在「整条序列超长」。
pub const DEFAULT_MAX_WINDOW: usize = 5;
/// 两次落屏之间的最大间隔；超过则视作断流（防跨句拼出杂词）。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// 滑窗切出的一个草稿词，连同其中各字**上屏时用户实际选中的码**（GH#181）。
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
    /// 选词出口登记、**下一次落屏**消费的「这个字是用哪条码打出来的」。
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

    /// 文本落屏。返回**本次新产生的窗口**（可能为空）。
    ///
    /// `text` 含任何非汉字即视为断流：标点、英文、数字、空格都是句子边界的信号，
    /// 跨过它们切出来的窗口是纯噪声（「上班。今天」→「班今天」）。判据与
    /// `feed_auto_phrase` 的 `all_han` 一致。
    ///
    /// **逐字推进**：多字词（选词组上屏）会被拆成一个个字依次进流，每个字各自产出
    /// 以它结尾的窗口。这正是本模型与 `AutoPhraseBuf` 的分界——那边多字词是终止符。
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
    /// **UTF-16 单元**，调用方随后再把替换文本喂进 [`Self::on_commit`]。
    ///
    /// ⚠️ 量纲是 UTF-16 单元而非 `char`，与 `ReplaceBackward.count` 对齐（那是发给宿主的
    /// 删除量）。扩展区汉字占 2 个单元，按 char 数回退会多删一个字——而码表里的生僻字
    /// 大量落在扩展区，正是最需要造词的那批。
    ///
    /// 回退量超过缓冲现有内容时**清空**：那说明被改掉的文字有一部分早于本段流，
    /// 剩下的字与新文本之间的连续性已不可知，继续切窗口就是在造幽灵词。
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
    /// 语义是「流到此为止，下一段重新起」，**不是** `AutoPhraseBuf::terminate` 那种
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
