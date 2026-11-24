//! 辅助码过滤逻辑
//!
//! 核心不变量：`filter_by_aux_code` 输出的 `kept` 必须是输入候选的子序列——不加候选、
//! 不重排，被保留候选的相对顺序与原列表一致。
//!
//! - 单字：任一码以 `aux_input` 开头即保留。不做字集判断，非汉字在表里有码同样可命中。
//! - 词组：逐字首码对齐（见 `phrase_matches_per_char_prefix`），先受
//!   `max_phrase_len` 约束。
//! - `aux_input` 为空或 `table` 为空：原样放行（防御态，否则会把候选窗整个滤光）。

use wind_candidate::{Candidate, FilterOutcome};

use crate::lookup::AuxCodeLookup;

/// 辅助码过滤选项
#[derive(Debug, Clone, Default)]
pub struct AuxCodeFilterOptions {
    /// 词组长度上限：参与筛选的**词组**字数 > 此值一律排除（0 = 不限）。默认 0。
    ///
    /// 长词组（如 `meishijian` 下的「没时间看」）是整词补全，首字前缀匹配会让它们大量
    /// 残留、污染筛选结果。只作用于词组，单字恒参与匹配。
    pub max_phrase_len: usize,
}

/// 判断候选文本是否为「单字符」（非词组）。
///
/// 不判断字集：字集筛选由上游负责，这里对任何字符一视同仁按表查询，无码自然过滤。
fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        None
    } else {
        Some(first)
    }
}

/// 词组逐字首码匹配：第 i 个字须有某码以 `aux_input` 的第 i 个字符为前缀
/// （即「每字的第一个辅助码」按序对齐）。
///
/// 前缀态：辅助码少于 N 位时词组保留（边打边缩）；超过 N 位或某位不中才过滤，否则 2 字词
/// 会被 3 位辅助码静默保留、挤掉真正匹配的 3 字词。
///
/// 按键热路径，零分配。非汉字字符同样按表查询（「多啦A梦」里 A 的码是 a）。
fn phrase_matches_per_char_prefix(text: &str, table: &dyn AuxCodeLookup, aux_input: &str) -> bool {
    let mut text_chars = text.chars();
    let mut input_chars = aux_input.chars();
    // 逐字对齐：第 i 个字查第 i 位输入的首码；输入在字前耗尽 = 前缀态（位数不足 N），
    // 词组保留——这正是「前缀匹配而非精确匹配」的语义，见函数头注释。
    // 不用 `zip`：其 `next` 会同时消耗两侧，末次空轮会把 text 的余字也吞掉，
    // 导致「input 提前耗尽」漏判。
    for ch in &mut text_chars {
        match input_chars.next() {
            Some(c) => {
                if !table.any_code_starts_with_char(ch, c) {
                    return false;
                }
            }
            None => break,
        }
    }
    // 字全部对齐完，但 input 还有剩余 = 位数**超过**词字数 → 不中：多出的位没有字
    // 可对应。否则 2 字词配 3 位辅助码会被静默保留，挤掉真正匹配的 3 字词。
    input_chars.next().is_none()
}

/// 判断单个候选是否匹配辅助码条件（谓词）。
///
/// 可与 [`CandidateStore::set_filter`] 等通用筛选容器组合使用——调用方只需传入此谓词，
/// 无需了解辅助码内部的单字/词组匹配细节。
///
/// 规则：
/// - 单字符：任一码前缀匹配 `aux_input` → `true`
/// - 词组：先检查 `max_phrase_len` 长度上限，再逐字首码匹配
/// - 辅助码输入为空或码表为空时返回 `true`（passthrough 语义）
pub fn aux_code_matches(
    c: &Candidate,
    table: &dyn AuxCodeLookup,
    aux_input: &str,
    options: &AuxCodeFilterOptions,
) -> bool {
    // 空输入/空表 = passthrough（不过滤）
    if aux_input.is_empty() || table.is_empty() {
        return true;
    }
    if let Some(ch) = single_char(&c.text) {
        return table.any_code_starts_with(ch, aux_input);
    }
    // 词组：长度上限排除
    if options.max_phrase_len > 0 && c.text.chars().count() > options.max_phrase_len {
        return false;
    }
    // 词组：逐字首码匹配
    phrase_matches_per_char_prefix(&c.text, table, aux_input)
}

/// 用辅助码表和用户输入过滤候选
///
/// 输出与 `wind-candidate::filter_candidates` 同构，可直接对接翻页放宽机制：`kept` 是输入的
/// 子序列，`filtered` 是其余候选，两者都保持原相对顺序。辅助码或码表为空时原样放行。
/// `table` 可以是任何 [`AuxCodeLookup`] 实现（文件表、方案视图、多来源拼接）。
pub fn filter_by_aux_code(
    candidates: Vec<Candidate>,
    table: &dyn AuxCodeLookup,
    aux_input: &str,
    options: &AuxCodeFilterOptions,
) -> FilterOutcome {
    // 辅助码输入为空：不过滤（原样放行）。触发键进入的辅助码模式正常不会空手调用
    // 本函数，这里是防御语义——用空辅助码筛选会无差别滤光整个候选窗。
    if aux_input.is_empty() {
        tracing::debug!("辅助码输入为空，不过滤");
        return FilterOutcome {
            kept: candidates,
            filtered: Vec::new(),
        };
    }

    // 未挂载任何有效码表：视为辅助码功能未启用，不过滤（原样放行）。
    if table.is_empty() {
        tracing::debug!("辅助码表为空，不过滤");
        return FilterOutcome {
            kept: candidates,
            filtered: Vec::new(),
        };
    }

    let mut kept = Vec::new();
    let mut filtered = Vec::new();

    for cand in candidates {
        if aux_code_matches(&cand, table, aux_input, options) {
            kept.push(cand);
        } else {
            filtered.push(cand);
        }
    }

    FilterOutcome { kept, filtered }
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;
