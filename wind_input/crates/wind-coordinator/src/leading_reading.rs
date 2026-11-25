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
    /// 记录时已分步上屏的段数。九宫格靠它认出「缓冲前部被选词切走了」：剩下的键可能碰巧与
    /// 锁定的键相同（`99999999` 选掉前四键），只比缓冲认不出来。
    pub segs: usize,
}

impl Applied {
    /// 这份选择对缓冲 `buffer` 还有没有效。
    ///
    /// 全键盘：缓冲一变就失效。九宫格：锁定的那一段（前 `orig_len` 字节）没变就仍有效——锁定后
    /// 接着按键、删掉锁定之后的键，锁定都保留；分步上屏切走了缓冲前部才失效。
    pub(crate) fn covers(&self, buffer: &str, segs: usize, keypad: bool) -> bool {
        if !keypad {
            return self.buffer == buffer;
        }
        self.segs == segs
            && self.orig_len > 0
            && buffer.get(..self.orig_len).is_some()
            && buffer.get(..self.orig_len) == self.buffer.get(..self.orig_len)
    }
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

/// 九宫格：缓冲里第 `k` 个编码键（不数分词键）的字节位；`k` ≥ 键数 → 缓冲长度。
///
/// 前 `k` 个键之后紧跟的分词键并入前面（返回的是下一个键的位置）。
pub(crate) fn key_offset(buffer: &str, k: usize) -> usize {
    let mut seen = 0;
    for (i, b) in buffer.bytes().enumerate() {
        if b == b'\'' {
            continue;
        }
        if seen == k {
            return i;
        }
        seen += 1;
    }
    buffer.len()
}

/// 九宫格：引擎在改写后的查询串上给的 `consumed_length` 换算回原始缓冲。锁定的字母与键
/// 一一对应，只有分词键的位置不同，所以按「吃掉了几个编码键」换算。0 = 整串，原样返回。
pub(crate) fn unmap_keypad_consumed(consumed: usize, query: &str, buffer: &str) -> usize {
    if consumed == 0 {
        return 0;
    }
    let keys = query.as_bytes()[..consumed.min(query.len())]
        .iter()
        .filter(|&&b| b != b'\'')
        .count();
    key_offset(buffer, keys)
}

/// 锁定读法 `head`（`ni'hao`）里的编码键数（字母数）
fn locked_keys(head: &str) -> usize {
    head.bytes().filter(|&b| b != b'\'').count()
}

impl Coordinator {
    /// 九宫格左栏逐段选拼音：已锁定部分之后那一段的读法 `(label, head, orig_len)`。
    ///
    /// 与全键盘的「首单元读法」共用一套（改写查询串、不动缓冲），差别是锁定可以**逐段累加**：
    /// 点 `ni` 后 `head = ni`；再点 `hao` 后 `head = ni'hao`、`orig_len` 覆盖 5 个键。
    /// 当前首选用的那个读法排第一。
    fn keypad_leading(
        &self,
        buffer: &str,
        applied: Option<&Applied>,
    ) -> Vec<(String, String, usize)> {
        let (head, locked) = match applied {
            Some(a) => (a.head.as_str(), a.orig_len.min(buffer.len())),
            None => ("", 0),
        };
        let after = &buffer[locked..];
        let start = locked + (after.len() - after.trim_start_matches('\'').len());
        let unit = buffer[start..].split('\'').next().unwrap_or("");
        if unit.is_empty() {
            return Vec::new();
        }
        let mut readings = self.engine_mgr.keypad_readings(unit);
        // 首选用的读法：九宫格预编辑里字母与键一一对应，跳过已锁定的键数，取到下一个隔音符
        let query = match applied {
            Some(a) => rewrite_query(buffer, &a.head, a.orig_len),
            None => buffer.to_string(),
        };
        let done = buffer[..start].bytes().filter(|&b| b != b'\'').count();
        let preedit = self.engine_mgr.convert(&query, 1).preedit_display;
        let mut letters = 0;
        let mut current = String::new();
        for c in preedit.chars() {
            if c == '\'' {
                if letters > done {
                    break;
                }
                continue;
            }
            if letters >= done {
                current.push(c);
            }
            letters += 1;
        }
        if let Some(i) = readings.iter().position(|r| *r == current) {
            let r = readings.remove(i);
            readings.insert(0, r);
        }
        readings
            .into_iter()
            .map(|syl| {
                let new_head = if head.is_empty() {
                    syl.clone()
                } else {
                    format!("{head}'{syl}")
                };
                let orig_len = key_offset(buffer, done + syl.len());
                (syl, new_head, orig_len)
            })
            .collect()
    }

    /// 九宫格退格：有锁定读法时先撤掉最后锁的那一段（不删键）。没有锁定 → `None`。
    pub(crate) fn pop_keypad_lock(&self, state: &mut State) -> Option<KeyAction> {
        if !self.keypad_active() {
            return None;
        }
        let applied = state
            .leading
            .clone()
            .filter(|a| a.covers(&state.input_buffer, state.committed_segs.len(), true))?;
        // 锁定之后还有键：先删键（走普通退格），删到锁定部分才撤锁
        if state.input_buffer[applied.orig_len..]
            .bytes()
            .any(|b| b != b'\'')
        {
            return None;
        }
        state.leading = applied.head.rsplit_once('\'').map(|(rest, _)| Applied {
            buffer: state.input_buffer.clone(),
            head: rest.to_string(),
            orig_len: key_offset(&state.input_buffer, locked_keys(rest)),
            segs: state.committed_segs.len(),
        });
        self.update_candidates(state);
        let text = state.preedit.clone();
        let caret_pos = self.composition_caret(state);
        self.notify_ui_update(state);
        Some(KeyAction::UpdateComposition { caret_pos, text })
    }

    /// 首单元的全部读法 `(label, head, orig_len)`，全拼与双拼各走各的；没得选 / 不是拼音 → 空。
    fn leading_all(&self, buffer: &str, applied: Option<&Applied>) -> Vec<(String, String, usize)> {
        if buffer.is_empty() || !self.engine_mgr.is_pinyin() {
            return Vec::new();
        }
        if self.keypad_active() {
            return self.keypad_leading(buffer, applied);
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
        let (buffer, applied, segs) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.active.is_some() {
                return Vec::new();
            }
            (
                st.input_buffer.clone(),
                st.leading.clone(),
                st.committed_segs.len(),
            )
        };
        // 九宫格列的是下一段（还没选），没有「当前生效」的那条
        let keypad = self.keypad_active();
        let applied = applied.filter(|a| a.covers(&buffer, segs, keypad));
        let current = applied.as_ref().map(|a| (a.head.clone(), a.orig_len));
        self.leading_all(&buffer, applied.as_ref())
            .into_iter()
            .enumerate()
            .map(|(i, (label, head, orig_len))| LeadingReading {
                selected: !keypad
                    && match &current {
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
        // 九宫格的列表是下一段的读法，点哪条都是锁定；全键盘的第一条是默认读法，点它 = 清除
        state.leading = (idx != 0 || self.keypad_active()).then(|| Applied {
            buffer: state.input_buffer.clone(),
            head: head.to_string(),
            orig_len,
            segs: state.committed_segs.len(),
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
        let query = rewrite_query(&full, &applied.head, applied.orig_len);
        state.input_buffer = query.clone();
        state.input_buffer_cased.clear();
        let result = self.build_candidates(state, limit);
        state.input_buffer = full;
        state.input_buffer_cased = full_cased;
        let total = state.input_buffer.len();
        let keypad = self.keypad_active();
        for c in &mut state.candidates {
            c.consumed_length = if keypad {
                unmap_keypad_consumed(c.consumed_length, &query, &state.input_buffer)
            } else {
                unmap_consumed(
                    c.consumed_length,
                    applied.head.len(),
                    applied.orig_len,
                    total,
                )
            };
            // 候选自带的切分与辅码位置是按改写后的查询算的（`fan'gxiang`），不能拿来显示
            c.meta.preedit_split = None;
            c.meta.aux_keys = None;
        }
        // 九宫格的编码栏要显示锁定后的读法（`mi'hao`），就用改写查询算出的那份，不覆盖
        if keypad {
            return result;
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
