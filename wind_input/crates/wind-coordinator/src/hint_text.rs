//! 正确拼音提示的显示正文（设计 `docs/design/fuzzy-pinyin-hint.md` §5.2、
//! `docs/design/pinyin-correction-and-hints.md` §7）：把候选的 `meta.pinyin_hints`（引擎文字的
//! 字下标）按来源勾选过滤、换算到显示文字上，插成「苹(ping)果」。
//!
//! 宁漏不错：显示文字与引擎文字对不上（简繁转换改了字数、截断截掉了被标的字）就不标。

use wind_candidate::{Candidate, HintKind, PinyinHint};
use wind_config::PinyinHintConfig;
use wind_config::config::TRUNCATION_MARK;
use wind_ui_types::{SpanStyle, StyledText};

/// 来源 `k` 是否勾选（`[input.correction.hint] fuzzy / misread / correction`）；正文与模板变量共用。
pub(crate) fn hint_kind_enabled(h: &PinyinHintConfig, k: HintKind) -> bool {
    match k {
        HintKind::Fuzzy => h.fuzzy,
        HintKind::Misread => h.misread,
        HintKind::Correction => h.correction,
    }
}

/// `hints` 里来源已勾选的那些。
pub(crate) fn enabled_hints(h: &PinyinHintConfig, hints: &[PinyinHint]) -> Vec<PinyinHint> {
    hints
        .iter()
        .filter(|x| hint_kind_enabled(h, x.kind))
        .cloned()
        .collect()
}

/// 在 `display` 第 `char_index` 个字后面插入 `(correct)`，整段 `(correct)` 角色 `pinyin_hint`，
/// 其余文字无角色。没有一条提示落在 `display` 里 ⇒ 空（= 按纯文字显示）。
pub(crate) fn hint_styled_text(display: &str, hints: &[PinyinHint]) -> StyledText {
    let mut out = StyledText::new();
    let hint_style = SpanStyle {
        role: wind_ui_types::static_role("pinyin_hint"),
        ..Default::default()
    };
    let mut copied = 0;
    let mut marked = false;
    for (i, (at, ch)) in display.char_indices().enumerate() {
        let mut here = hints
            .iter()
            .filter(|h| h.char_index as usize == i)
            .peekable();
        if here.peek().is_none() {
            continue;
        }
        let end = at + ch.len_utf8();
        out.push(&display[copied..end], &SpanStyle::default());
        copied = end;
        for h in here {
            out.push(&format!("({})", h.correct), &hint_style);
            marked = true;
        }
    }
    if !marked {
        return StyledText::new();
    }
    out.push(&display[copied..], &SpanStyle::default());
    out
}

/// 引擎文字的字下标换算到显示文字：字数相同原样；显示文字更短 ⇒ 视为只显示后段（逆切分次选），
/// 扣掉前段字数、落在前段的提示丢弃；显示文字更长 ⇒ `None`。
///
/// 字数单凭两段文字分不清「只显示后段」与「转换改了字数」，后者由调用方
/// （`Coordinator::cand_display_hints`）先判掉。
pub(crate) fn remap_hints(
    engine_text: &str,
    display: &str,
    hints: &[PinyinHint],
) -> Option<Vec<PinyinHint>> {
    let (engine_len, display_len) = (engine_text.chars().count(), display.chars().count());
    let front = engine_len.checked_sub(display_len)?;
    Some(
        hints
            .iter()
            .filter(|h| h.char_index as usize >= front)
            .map(|h| PinyinHint {
                char_index: h.char_index - front as u16,
                correct: h.correct.clone(),
                kind: h.kind,
            })
            .collect(),
    )
}

/// 候选窗那一格最终文字（= 前缀标注 `prefix` + 截断后的 `disp`）的带提示正文。`hints` 已换算到
/// `full`（截断前的显示文字）。截断掉的字上的提示丢弃；前缀标注让下标整体后移。
pub(crate) fn styled_for_shown(
    full: &str,
    disp: &str,
    prefix: &str,
    hints: &[PinyinHint],
) -> StyledText {
    let disp_len = disp.chars().count();
    let kept = if disp == full {
        disp_len
    } else if disp.ends_with(TRUNCATION_MARK) {
        disp_len - 1
    } else {
        return StyledText::new();
    };
    let shift = prefix.chars().count();
    let shifted: Vec<PinyinHint> = hints
        .iter()
        .filter(|h| (h.char_index as usize) < kept)
        .map(|h| PinyinHint {
            char_index: h.char_index + shift as u16,
            correct: h.correct.clone(),
            kind: h.kind,
        })
        .collect();
    if shifted.is_empty() {
        return StyledText::new();
    }
    hint_styled_text(&format!("{prefix}{disp}"), &shifted)
}

/// 单字 `ch` 在词库音节 `syl` 下的带调读音（读音表筛音，同 `${pinyin}` 口径）；读音表里没有
/// 与 `syl` 相符的读音 ⇒ 退回 `syl` 本身（宁可少调，不可错音）。
fn toned_reading(reverse: &wind_reverse::ReverseLookup, ch: char, syl: &str) -> String {
    let t = reverse.toned_pinyin_of(ch.encode_utf8(&mut [0; 4]), Some(&[syl]), "");
    if t.is_empty() { syl.to_string() } else { t }
}

/// `hint.tone`（§9.1）：提示里的正确音节换成带调读音。`hints` 是引擎文字 `text` 的字下标。
pub(crate) fn toned_hints(
    text: &str,
    hints: &[PinyinHint],
    reverse: &wind_reverse::ReverseLookup,
) -> Vec<PinyinHint> {
    hints
        .iter()
        .map(|h| PinyinHint {
            char_index: h.char_index,
            correct: match text.chars().nth(h.char_index as usize) {
                Some(ch) => toned_reading(reverse, ch, &h.correct),
                None => h.correct.clone(),
            },
            kind: h.kind,
        })
        .collect()
}

/// `${correct_pinyin}`（§9.2）：带已勾选来源提示的候选的整词正确拼音，音节以空格分隔；`tone`
/// 有值即带调。只按来源过滤，不看 `hint.enabled` 与 `scope`。音节取引擎随提示给出的
/// `meta.hint_syllables`（与内嵌提示同一切法）；没有勾选来源的提示、或音节数与字数对不上 ⇒ 空串。
pub(crate) fn correct_pinyin_text(
    c: &Candidate,
    hint: &PinyinHintConfig,
    tone: Option<&wind_reverse::ReverseLookup>,
) -> String {
    let syls = &c.meta.hint_syllables;
    if !c
        .meta
        .pinyin_hints
        .iter()
        .any(|h| hint_kind_enabled(hint, h.kind))
        || syls.len() != c.text.chars().count()
    {
        return String::new();
    }
    match tone {
        Some(reverse) => c
            .text
            .chars()
            .zip(syls)
            .map(|(ch, s)| toned_reading(reverse, ch, s))
            .collect::<Vec<_>>()
            .join(" "),
        None => syls.join(" "),
    }
}

#[cfg(test)]
#[path = "hint_text_tests.rs"]
mod tests;
