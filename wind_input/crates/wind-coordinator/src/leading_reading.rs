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
    /// 左栏显示的文字。全拼下与 [`Self::head`] 相同；双拼下显示拼音（`deng`），写进查询的是击键（`dg`）
    pub label: String,
    /// 写进查询的首单元形态（全拼是音节 / 声母，双拼是对应的击键）
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

/// 双拼首单元的读法（纯函数）。首单元是一个击键对，没有「另一种切法」，所以只有三类：
/// 默认（拼音显示、击键写查询）→ 模糊音兄弟（显示兄弟音节、查询用它对应的击键）→ 只取声母。
///
/// - `first_pinyin`：首个击键对转出的全拼音节（`deng`）
/// - `keys`：原始缓冲，`raw_end`：该音节占的击键字节数
/// - `siblings`：`(兄弟音节, 它对应的击键)`，击键查不到的调用方已丢掉
///
/// 返回 `(label, head, orig_len)`；少于两条 = 没得选，返回空。
pub(crate) fn sp_readings_of(
    first_pinyin: &str,
    keys: &str,
    raw_end: usize,
    siblings: &[(String, String)],
) -> Vec<(String, String, usize)> {
    if raw_end == 0 || raw_end > keys.len() || !keys.is_char_boundary(raw_end) {
        return Vec::new();
    }
    let default_head = &keys[..raw_end];
    let mut out: Vec<(String, String, usize)> =
        vec![(first_pinyin.to_string(), default_head.to_string(), raw_end)];
    for (label, head) in siblings {
        if head != default_head && !out.iter().any(|(_, h, _)| h == head) {
            out.push((label.clone(), head.clone(), raw_end));
        }
    }
    // 只取声母：零声母音节（a/o/e 开头）没有声母可取；声母占一个击键（zh/ch/sh 也是一个键）
    let initial_len = if first_pinyin.len() >= 2 && matches!(&first_pinyin[..2], "zh" | "ch" | "sh")
    {
        2
    } else {
        1
    };
    let starts_with_vowel = first_pinyin.starts_with(['a', 'o', 'e']);
    if raw_end > 1 && !starts_with_vowel && first_pinyin.is_char_boundary(initial_len) {
        out.push((
            first_pinyin[..initial_len].to_string(),
            keys[..1].to_string(),
            1,
        ));
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
    /// 首单元的全部读法 `(label, head, orig_len)`，全拼与双拼各走各的；没得选 / 不是拼音 → 空。
    fn leading_all(&self, buffer: &str) -> Vec<(String, String, usize)> {
        if buffer.is_empty() || !self.engine_mgr.is_pinyin() {
            return Vec::new();
        }
        if self.engine_mgr.is_shuangpin() {
            // 双拼：首单元是第一个击键对。拆分串在双拼下是击键切分，不是拼音，不能拿来查音节表
            let units = self.engine_mgr.shuangpin_units(buffer);
            let Some((pinyin, start, end)) = units.into_iter().next() else {
                return Vec::new();
            };
            if start != 0 {
                return Vec::new();
            }
            let siblings: Vec<(String, String)> = self
                .engine_mgr
                .fuzzy_siblings(&pinyin)
                .into_iter()
                .filter_map(|sib| {
                    let keys = self
                        .engine_mgr
                        .shuangpin_code_of_syllables(&[sib.as_str()])?;
                    Some((sib, keys))
                })
                .collect();
            return sp_readings_of(&pinyin, buffer, end, &siblings);
        }
        let split = self.engine_mgr.convert(buffer, 1).preedit_pinyin;
        let Some(default_len) = preedit_cursor::syllable_spans(buffer, &split)
            .and_then(|spans| spans.first().map(|&(_, end)| end))
        else {
            return Vec::new();
        };
        let siblings = self.engine_mgr.fuzzy_siblings(&buffer[..default_len]);
        readings_of(buffer, default_len, &siblings)
            .into_iter()
            .map(|(head, len)| (head.clone(), head, len))
            .collect()
    }

    /// 首单元的读法列表（展开页左栏画它）；没得选 / 不是拼音 → 空
    pub fn leading_readings(&self) -> Vec<LeadingReading> {
        let (buffer, applied) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.active.is_some() {
                return Vec::new();
            }
            (st.input_buffer.clone(), st.leading.clone())
        };
        let current = applied
            .filter(|a| a.buffer == buffer)
            .map(|a| (a.head, a.orig_len));
        self.leading_all(&buffer)
            .into_iter()
            .enumerate()
            .map(|(i, (label, head, orig_len))| LeadingReading {
                selected: match &current {
                    Some((h, l)) => *h == head && *l == orig_len,
                    None => i == 0,
                },
                label,
                head,
                orig_len,
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
            // 候选自带的切分与辅码位置是按改写后的查询算的（`fan'gxiang`），不能拿来显示
            c.meta.preedit_split = None;
            c.meta.aux_keys = None;
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
#[path = "leading_reading_tests.rs"]
mod tests;
