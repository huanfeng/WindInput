//! 手机端「首单元读法」：展开页左栏列出**最前面那一个单元**的几种读法，点一项就按该读法重算候选。
//!
//! 对标主流手机输入法（小米、搜狗一类）的做法：**不改用户打的编码**，只切换「这几个字母怎么读」。
//! 读法有三类：同一串字母的不同切分（`xian` → `xian` / `xi`）、模糊音兄弟（`fang` → `fan`）、
//! 只取声母（`wo` → `w`）。选走一段后，下一个单元自然成了首单元，所以不存在「移到前面回不去」。
//!
//! 实现上读法 = 把首单元写成指定形态、后面接一个隔音符再接剩余编码，**临时**喂给引擎
//! （引擎本就支持手动分隔符 `'`，`consumed_length` 也已回映射到含 `'` 的原始输入空间）；
//! 查完立刻还原缓冲，候选的 `consumed_length` 换算回原始缓冲，选词、续打、词频记账全走现成的分步上屏。
//! 缓冲没有被改写过，编码栏显示的始终是用户打的那串。
//!
//! 电脑端要的是「移光标」那套（[`crate::handle_candidate`] 的 `cursor_by_syllable`），与本模块无关。

use wind_bridge::handler::KeyAction;
use wind_engine::pinyin::syllable::STANDARD_SYLLABLES;
use wind_host::KeyOutcome;

use crate::coordinator::{Coordinator, InputOutcome, State};
use crate::{edit_ops, preedit_cursor};

/// 已选中的非默认读法：只对记录它时的那份缓冲有效（缓冲一变就失效，见 `State::leading`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub buffer: String,
    /// 首单元写成什么（`fan`、`xi`、`w`）
    pub head: String,
    /// 它在**原始缓冲**里占几个字节（`fang`→`fan` 时是 4，不是 3）
    pub orig_len: usize,
}

/// 一条读法
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadingReading {
    /// 左栏显示的文字，也是写进查询的首单元
    pub head: String,
    /// 占原始缓冲的字节数
    pub orig_len: usize,
    /// 当前生效的是不是它
    pub selected: bool,
}

/// 一个单元最长的拼音音节（`zhuang` / `chuang` / `shuang` 都是 6）
const MAX_SYLLABLE: usize = 6;

/// 首单元的读法列表（纯函数）。`default_len` 是引擎当前切法下首音节的长度。
///
/// 顺序：默认切法 → 其余能成音节的前缀（长的在前）→ 模糊音兄弟 → 只取声母。
/// 少于两条 = 没得选，返回空。
pub(crate) fn readings_of(
    buffer: &str,
    default_len: usize,
    siblings: &[String],
) -> Vec<(String, usize)> {
    if default_len == 0 || default_len > buffer.len() || !buffer.is_char_boundary(default_len) {
        return Vec::new();
    }
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut push = |head: &str, len: usize| {
        if !head.is_empty() && !out.iter().any(|(h, _)| h == head) {
            out.push((head.to_string(), len));
        }
    };
    push(&buffer[..default_len], default_len);
    for k in (1..=MAX_SYLLABLE.min(buffer.len())).rev() {
        // 另一种切法要站得住：前缀本身是音节，且剩下的部分能接着切出一个完整音节（`xian` 的
        // `xi'an` 成立，`xia'n` 不成立——落单的 n 不是一个单元）
        if k != default_len
            && buffer.is_char_boundary(k)
            && STANDARD_SYLLABLES.contains(&&buffer[..k])
            && starts_with_syllable(&buffer[k..])
        {
            push(&buffer[..k], k);
        }
    }
    for sib in siblings {
        push(sib, default_len);
    }
    // 只取声母：zh/ch/sh 是一个声母（两个字母），其余取首字母
    let initial_len = if buffer.len() >= 2 && matches!(&buffer[..2], "zh" | "ch" | "sh") {
        2
    } else {
        1
    };
    if initial_len <= default_len {
        push(&buffer[..initial_len], initial_len);
    }
    if out.len() < 2 {
        out.clear();
    }
    out
}

/// `rest` 是否以一个完整的标准音节开头（空串也算：前缀正好吃完整串）
fn starts_with_syllable(rest: &str) -> bool {
    rest.is_empty()
        || (1..=MAX_SYLLABLE.min(rest.len()))
            .any(|j| rest.is_char_boundary(j) && STANDARD_SYLLABLES.contains(&&rest[..j]))
}

/// 把首单元换成 `head` 后的查询串：`head` + `'` + 原始缓冲里首单元之后的部分（已有 `'` 不重复加）
pub(crate) fn rewrite_query(buffer: &str, head: &str, orig_len: usize) -> String {
    let rest = &buffer[orig_len.min(buffer.len())..];
    if rest.is_empty() || rest.starts_with('\'') {
        format!("{head}{rest}")
    } else {
        format!("{head}'{rest}")
    }
}

/// 把引擎在改写后的查询串上给的 `consumed_length`（含隔音符）换算回原始缓冲的字节数。
/// 0 = 引擎未标注（约定为整串）原样返回。
pub(crate) fn unmap_consumed(
    consumed: usize,
    head_len: usize,
    orig_len: usize,
    orig_total: usize,
) -> usize {
    if consumed == 0 {
        return 0;
    }
    if consumed <= head_len {
        return orig_len.min(orig_total);
    }
    // 超出首单元的部分要扣掉隔音符那一个字节
    (orig_len + consumed.saturating_sub(head_len + 1)).min(orig_total)
}

impl Coordinator {
    /// 引擎当前切法下首音节的长度；非全拼 / 拆分串对不回缓冲（双拼）→ `None`
    fn leading_default_len(&self, buffer: &str) -> Option<usize> {
        if !self.engine_mgr.is_pinyin() || buffer.is_empty() {
            return None;
        }
        let split = self.engine_mgr.convert(buffer, 1).preedit_pinyin;
        preedit_cursor::syllable_spans(buffer, &split)?
            .first()
            .map(|&(_, end)| end)
    }

    /// 首单元的读法列表（展开页左栏画它）；没得选 / 不是全拼 → 空
    pub fn leading_readings(&self) -> Vec<LeadingReading> {
        let (buffer, applied) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.active.is_some() {
                return Vec::new();
            }
            (st.input_buffer.clone(), st.leading.clone())
        };
        let Some(default_len) = self.leading_default_len(&buffer) else {
            return Vec::new();
        };
        let siblings = self.engine_mgr.fuzzy_siblings(&buffer[..default_len]);
        let list = readings_of(&buffer, default_len, &siblings);
        let current = applied
            .filter(|a| a.buffer == buffer)
            .map(|a| (a.head, a.orig_len));
        list.iter()
            .enumerate()
            .map(|(i, (head, len))| LeadingReading {
                head: head.clone(),
                orig_len: *len,
                selected: match &current {
                    Some((h, l)) => h == head && l == len,
                    None => i == 0,
                },
            })
            .collect()
    }

    /// 选定一条读法：候选按该读法重算，编码不动。选默认读法 = 清除选择。
    /// 读法不在当前列表里（缓冲已变）→ `None`，什么都没改。
    pub fn set_leading_reading(&self, head: &str, orig_len: usize) -> Option<KeyOutcome> {
        let list = self.leading_readings();
        let idx = list
            .iter()
            .position(|r| r.head == head && r.orig_len == orig_len)?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.leading = (idx != 0).then(|| Applied {
            buffer: state.input_buffer.clone(),
            head: head.to_string(),
            orig_len,
        });
        self.update_candidates(&mut state);
        let text = state.preedit.clone();
        let caret_pos = self.composition_caret(&state);
        self.notify_ui_update(&state);
        Some(edit_ops::to_outcome(KeyAction::UpdateComposition {
            caret_pos,
            text,
        }))
    }

    /// [`Self::build_candidates`] 的「首单元读法」版本：按改写后的查询串问引擎，还原缓冲，
    /// 把 `consumed_length` 换算回原始缓冲，并让编码栏继续显示用户打的那串。
    pub(crate) fn build_candidates_with_leading(
        &self,
        state: &mut State,
        limit: usize,
        applied: &Applied,
    ) -> (usize, InputOutcome) {
        let full = state.input_buffer.clone();
        let full_cased = state.input_buffer_cased.clone();
        state.input_buffer = rewrite_query(&full, &applied.head, applied.orig_len);
        state.input_buffer_cased.clear();
        let result = self.build_candidates(state, limit);
        state.input_buffer = full;
        state.input_buffer_cased = full_cased;
        let total = state.input_buffer.len();
        for c in &mut state.candidates {
            c.consumed_length = unmap_consumed(
                c.consumed_length,
                applied.head.len(),
                applied.orig_len,
                total,
            );
            // 候选自带的切分是按改写后的查询算的（`fan'gxiang`），不能拿来显示
            c.meta.preedit_split = None;
        }
        let body = self
            .engine_mgr
            .convert(&state.input_buffer, 1)
            .preedit_pinyin;
        if !body.is_empty() {
            state.preedit_split_body = body;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heads(v: &[(String, usize)]) -> Vec<&str> {
        v.iter().map(|(h, _)| h.as_str()).collect()
    }

    #[test]
    fn readings_list_default_then_other_prefixes_then_initial() {
        // xian：引擎默认 xian；另一种切法 xi'an；再加只取声母 x
        let r = readings_of("xian", 4, &[]);
        assert_eq!(heads(&r), ["xian", "xi", "x"]);
        assert_eq!(r[1], ("xi".to_string(), 2));
    }

    #[test]
    fn readings_include_fuzzy_siblings_with_original_length() {
        let r = readings_of("fangxiang", 4, &["fan".to_string()]);
        assert_eq!(heads(&r), ["fang", "fan", "f"]);
        assert_eq!(
            r[1],
            ("fan".to_string(), 4),
            "模糊兄弟仍占原始缓冲的 4 个字节"
        );
    }

    #[test]
    fn digraph_initials_are_one_unit() {
        let r = readings_of("zhongguo", 5, &[]);
        assert_eq!(heads(&r), ["zhong", "zh"]);
    }

    #[test]
    fn a_single_reading_means_no_choice() {
        assert!(readings_of("a", 1, &[]).is_empty());
        assert!(readings_of("xian", 0, &[]).is_empty(), "拆不出首音节");
    }

    #[test]
    fn rewrite_inserts_one_separator_after_the_head() {
        assert_eq!(rewrite_query("xianzai", "xi", 2), "xi'anzai");
        assert_eq!(rewrite_query("fangxiang", "fan", 4), "fan'xiang");
        assert_eq!(rewrite_query("nihao", "n", 1), "n'ihao");
        assert_eq!(rewrite_query("ni", "ni", 2), "ni", "没有后半截不加隔音符");
        assert_eq!(
            rewrite_query("ni'hao", "ni", 2),
            "ni'hao",
            "已有隔音符不重复"
        );
    }

    #[test]
    fn consumed_maps_back_over_the_separator() {
        // fang|xiang 读成 fan：查询 fan'xiang（8 字节含 '），原始缓冲 fangxiang（9）
        assert_eq!(
            unmap_consumed(3, 3, 4, 9),
            4,
            "只吃首单元 → 原始缓冲里的 4 字节"
        );
        assert_eq!(unmap_consumed(4, 3, 4, 9), 4, "吃到隔音符 → 仍是首单元");
        assert_eq!(
            unmap_consumed(8, 3, 4, 9),
            8,
            "fan'xian（8）→ 原始 fangxian（8）"
        );
        assert_eq!(unmap_consumed(9, 3, 4, 9), 9, "整串");
        assert_eq!(unmap_consumed(0, 3, 4, 9), 0, "0 = 未标注，约定为整串");
    }
}
