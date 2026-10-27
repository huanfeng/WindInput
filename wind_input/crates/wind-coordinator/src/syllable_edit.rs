//! 拼音「中间修正」（手机端）：看到编码被切成哪几个音节、把光标定到某个音节后面、
//! 对某一段给出可替换的拼写、替换后继续输入。
//!
//! 为什么放在核心而不是 Android：音节切分、纠错集合与候选重算都在这里，宿主只负责画音节栏、
//! 收点击；换一个宿主（iOS）照样复用。
//!
//! 只在 Android 生效（`use_mobile_overrides`），电脑端 ←/→ 的既有行为不动，理由与
//! [`Coordinator::query_cut`](crate::handle_candidate) 同。

use wind_engine::pinyin::syllable::STANDARD_SYLLABLES;
use wind_host::KeyOutcome;

use wind_bridge::handler::KeyAction;

use crate::coordinator::Coordinator;
use crate::{edit_ops, preedit_cursor};

/// 一个音节：文本与它在缓冲内的字节区间
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyllableSpan {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// 当前编码的音节视图
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyllableView {
    pub syllables: Vec<SyllableSpan>,
    /// 编码光标（缓冲内字节偏移）
    pub caret: usize,
}

/// 一条可替换的拼写：把缓冲 `[start, end)` 换成 `text`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyllableFix {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// 备选条数上限：一屏放得下，再多是噪声
const MAX_FIXES: usize = 5;

/// `a` 变成 `b` 的「一次手误」档位，越小越像真手误；不是一次编辑 → `None`。
///
/// 0 = 漏打 / 多打 / 相邻互换（fan↔fang、fnag→fang），1 = 模糊对（l/n、f/h），
/// 2 = 键盘相邻键误触。换一个字母只认后两档：不限的话 fang 会冒出 bang/cang/dang 一长串噪声。
fn edit_rank(a: &str, b: &str) -> Option<u8> {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a == b {
        return None;
    }
    match a.len().cmp(&b.len()) {
        std::cmp::Ordering::Equal => {
            let diff: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
            match diff.as_slice() {
                [i] => substitution_rank(a[*i], b[*i]),
                [i, j] if *j == *i + 1 && a[*i] == b[*j] && a[*j] == b[*i] => Some(0),
                _ => None,
            }
        }
        std::cmp::Ordering::Less => insertion_rank(&a, &b),
        std::cmp::Ordering::Greater => insertion_rank(&b, &a),
    }
}

/// 换一个字母的档位：模糊对（l/n、f/h）= 1，QWERTY 相邻 = 2，其余不算手滑
fn substitution_rank(a: char, b: char) -> Option<u8> {
    if matches!((a, b), ('l', 'n') | ('n', 'l') | ('f', 'h') | ('h', 'f')) {
        return Some(1);
    }
    // 各行按键盘错位给横坐标：下一行比上一行右移约 1/4、3/4 个键
    fn pos(c: char) -> Option<(f32, f32)> {
        for (row, (keys, off)) in [("qwertyuiop", 0.0), ("asdfghjkl", 0.25), ("zxcvbnm", 0.75)]
            .into_iter()
            .enumerate()
        {
            if let Some(i) = keys.find(c) {
                return Some((i as f32 + off, row as f32));
            }
        }
        None
    }
    match (pos(a), pos(b)) {
        (Some((x1, y1)), Some((x2, y2))) if (y1 - y2).abs() <= 1.0 && (x1 - x2).abs() <= 1.0 => {
            Some(2)
        }
        _ => None,
    }
}

/// `short` 插入一个字符能得到 `long` 时的档位：漏掉**首字母**不像手误（`ang`←`fang`）→ 2，
/// 其余位置（多打 / 漏打后面的字母）→ 0。长度差必须为 1，否则 `None`。
fn insertion_rank(short: &[char], long: &[char]) -> Option<u8> {
    if long.len() != short.len() + 1 {
        return None;
    }
    let skip = (0..short.len())
        .find(|&i| short[i] != long[i])
        .unwrap_or(short.len());
    (short[skip..] == long[skip + 1..]).then_some(if skip == 0 { 2 } else { 0 })
}

/// 第 `idx` 个音节的可替换拼写。
///
/// 备选来自标准音节表里**与这一段只差一次编辑**的音节：模糊音等价（an/ang、z/zh、l/n、f/h…）
/// 与误触、漏打、多打、相邻互换（fnag→fang）都落在这个集合里，不必单独枚举规则。
/// 一次编辑纠不了一个音节时（`fnagxiang` 被切成 `f|nag|xiang`），再试**与前 / 后一个音节合并**
/// 的整段，所以点 `nag` 也能得到把 `fnag` 整段改成 `fang` 的选项。
pub fn syllable_fixes(buffer: &str, spans: &[(usize, usize)], idx: usize) -> Vec<SyllableFix> {
    let Some(&(s, e)) = spans.get(idx) else {
        return Vec::new();
    };
    let mut ranges = vec![(s, e)];
    if idx + 1 < spans.len() {
        ranges.push((s, spans[idx + 1].1));
    }
    if idx > 0 {
        ranges.push((spans[idx - 1].0, e));
    }
    let mut out: Vec<(u8, SyllableFix)> = Vec::new();
    for (start, end) in ranges {
        let text = &buffer[start..end];
        for syl in STANDARD_SYLLABLES {
            let Some(rank) = edit_rank(text, syl) else {
                continue;
            };
            if !out
                .iter()
                .any(|(_, f)| f.start == start && f.end == end && f.text == *syl)
            {
                out.push((
                    rank,
                    SyllableFix {
                        start,
                        end,
                        text: (*syl).to_string(),
                    },
                ));
            }
        }
    }
    // 排序：先单音节（改动小）再合并区间，再按档位、字短在前
    out.sort_by_key(|(rank, f)| (f.end - f.start != e - s, *rank, f.text.len()));
    let mut out: Vec<SyllableFix> = out.into_iter().map(|(_, f)| f).collect();
    out.truncate(MAX_FIXES);
    out
}

impl Coordinator {
    /// 当前编码的音节拆分；非拼音 / 拆不回缓冲（双拼）/ 无编码 → `None`
    fn syllable_spans_now(&self) -> Option<(String, Vec<(usize, usize)>, usize)> {
        if !crate::handle_assoc::use_mobile_overrides() || !self.engine_mgr.is_pinyin() {
            return None;
        }
        let (buffer, caret) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (st.input_buffer.clone(), st.input_cursor_pos)
        };
        if buffer.is_empty() {
            return None;
        }
        let split = self.engine_mgr.convert(&buffer, 1).preedit_pinyin;
        let spans = preedit_cursor::syllable_spans(&buffer, &split)?;
        Some((buffer, spans, caret))
    }

    /// 音节视图（编码栏下方 / 展开页左栏画它）
    pub fn syllable_view(&self) -> Option<SyllableView> {
        let (buffer, spans, caret) = self.syllable_spans_now()?;
        Some(SyllableView {
            syllables: spans
                .iter()
                .map(|&(start, end)| SyllableSpan {
                    text: buffer[start..end].to_string(),
                    start,
                    end,
                })
                .collect(),
            caret,
        })
    }

    /// 第 `idx` 个音节的可替换拼写
    pub fn syllable_fixes(&self, idx: usize) -> Vec<SyllableFix> {
        match self.syllable_spans_now() {
            Some((buffer, spans, _)) => syllable_fixes(&buffer, &spans, idx),
            None => Vec::new(),
        }
    }

    /// 把编码光标放到缓冲 `caret` 处（落在字符边界、夹进 `[0, len]`），候选随之只查光标前一段。
    /// 返回编辑指令（同 `key_down`：宿主据此刷新组合区与插入符）。
    pub fn set_syllable_caret(&self, caret: usize) -> KeyOutcome {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.input_buffer.is_empty() {
            return KeyOutcome::passthrough();
        }
        let mut c = caret.min(state.input_buffer.len());
        while !state.input_buffer.is_char_boundary(c) {
            c -= 1;
        }
        state.input_cursor_pos = c;
        self.update_candidates(&mut state);
        let text = state.preedit.clone();
        let caret_pos = self.composition_caret(&state);
        self.notify_ui_update(&state);
        edit_ops::to_outcome(KeyAction::UpdateComposition { caret_pos, text })
    }

    /// 应用一条修正：缓冲 `[start, end)` 换成 `text`，光标落到替换段之后，候选按光标前一段重算。
    /// 区间越界 / 不在字符边界 / `text` 含非小写字母 → 不改，返回 `None`。
    pub fn apply_syllable_fix(&self, start: usize, end: usize, text: &str) -> Option<KeyOutcome> {
        if !crate::handle_assoc::use_mobile_overrides()
            || text.is_empty()
            || !text.bytes().all(|b| b.is_ascii_lowercase())
        {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let buf = &state.input_buffer;
        if start >= end
            || end > buf.len()
            || !buf.is_char_boundary(start)
            || !buf.is_char_boundary(end)
        {
            return None;
        }
        state.input_buffer.replace_range(start..end, text);
        // 大写影子串跟着失效：替换后长度可能不同，宁可丢大写也不错位
        state.input_buffer_cased.clear();
        state.input_cursor_pos = start + text.len();
        self.update_candidates(&mut state);
        let preedit = state.preedit.clone();
        let caret_pos = self.composition_caret(&state);
        self.notify_ui_update(&state);
        Some(edit_ops::to_outcome(KeyAction::UpdateComposition {
            caret_pos,
            text: preedit,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_rank_orders_realistic_typos_first() {
        assert_eq!(edit_rank("fnag", "fang"), Some(0), "相邻互换");
        assert_eq!(edit_rank("fan", "fang"), Some(0), "漏打");
        assert_eq!(edit_rank("fangg", "fang"), Some(0), "多打");
        assert_eq!(edit_rank("zi", "zhi"), Some(0), "平翘舌");
        assert_eq!(edit_rank("fang", "hang"), Some(1), "f/h 模糊对");
        assert_eq!(edit_rank("fsng", "fang"), Some(2), "s 与 a 键盘相邻");
        assert_eq!(edit_rank("fang", "bang"), None, "f/b 不相邻，不是手滑");
        assert_eq!(edit_rank("fang", "fang"), None, "相同不算备选");
        assert_eq!(edit_rank("fang", "xiang"), None);
    }

    #[test]
    fn fixes_offer_neighbours_of_a_syllable() {
        // fang'xiang：点 fang，fan（漏打 g）排在最前，bang 不在
        let spans = [(0, 4), (4, 9)];
        let fixes = syllable_fixes("fangxiang", &spans, 0);
        let texts: Vec<_> = fixes.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(texts.first(), Some(&"fan"), "{texts:?}");
        assert!(!texts.contains(&"bang"), "{texts:?}");
        assert!(fixes.iter().all(|f| f.text != "fang"), "不含自己");
        assert!(fixes.len() <= MAX_FIXES);
    }

    #[test]
    fn fixes_merge_with_neighbours_when_the_typo_spans_two_syllables() {
        // fnagxiang 被切成 f|nag|xiang：点 nag 时，f+nag 合并成 fnag → fang
        let spans = [(0, 1), (1, 4), (4, 9)];
        let fixes = syllable_fixes("fnagxiang", &spans, 1);
        assert!(
            fixes
                .iter()
                .any(|f| (f.start, f.end, f.text.as_str()) == (0, 4, "fang")),
            "{fixes:?}"
        );
    }

    #[test]
    fn out_of_range_index_is_empty() {
        assert!(syllable_fixes("ni", &[(0, 2)], 5).is_empty());
    }
}
