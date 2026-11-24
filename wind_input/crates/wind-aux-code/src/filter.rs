//! 辅助码过滤逻辑
//!
//! 核心不变量（请严格遵守，后续扩展也不能打破）：
//! > **`filter_by_aux_code` 输出的 `kept` 必须是输入候选的子序列**——不会凭空加候选、
//! > 不会丢弃后重排。被保留候选的相对顺序与原列表完全一致（不额外按权重重排）。
//!
//! 过滤规则：
//! - 单字候选：查所有辅助码（多表已在 [`AuxCodeTable::merge`] 阶段合并为一张），
//!   **任一码前缀匹配 `aux_input`** → 保留；否则过滤。字符一律按表查询、**不做字集
//!   判断**——字集（简繁/字符集等）由输入法自身的相关选项在上游决定，出现在候选里的
//!   字符即用户意图；非汉字单字在表里有码（如 `A`→`a`）同样可命中，无码则自然过滤
//! - 词组候选：逐字首码匹配（`PerCharPrefix`），顺序输入每字的**第一个辅助码**——
//!   第 i 位须命中第 i 字任一码的首字符。辅助码**可以短于** N 位——输入尚未打满即前缀态，
//!   词组保留（边打边缩，如「时间」打 `o` 时保留，因为 时=oc 以 o 开头）；**超过** N 位
//!   （字全部对齐后仍有剩余）或某位不中 → 过滤。如小鹤双拼「魔法少女」打 `gdxv`
//!   （广氵小乙，乙=折）。字符一律按表查询（不区分是否汉字，表里有码即可参与匹配，
//!   如「多啦A梦」里 A 的码是 a；表里无码则自然过滤）
//! - **词组长度上限**（`AuxCodeFilterOptions::max_phrase_len`，默认 0，0 = 不限）：字数
//!   > 该值的**词组**一律排除、不参与辅助码筛选。长词组（整词补全/组合词，如 `meishijian`
//!   > 下的「没时间看/没时间做」）首字辅助码前缀匹配会让它们大量残留、污染逐字词筛选，
//!   > 而辅助码字形筛选的目标是短字词；单字恒参与匹配，不受此限
//! - `aux_input` 为空串 **或** `table` 为空（未挂任何码表）：**不过滤**（原样放行）。
//!   辅助码模式由触发键进入，正常流程不会带着空辅助码调用本函数；空输入 / 空表只是
//!   防御态——用空辅助码或空表去筛选会把候选窗整个滤光。

use wind_candidate::{Candidate, FilterOutcome};

use crate::lookup::AuxCodeLookup;

/// 辅助码过滤选项
#[derive(Debug, Clone, Default)]
pub struct AuxCodeFilterOptions {
    /// 词组长度上限：参与筛选的**词组**字数 > 此值一律排除（0 = 不限）。默认 0。
    ///
    /// 目的：长词组（如 `meishijian` 下的「没时间看/没时间做」）是整词补全/组合词，
    /// 首字辅助码前缀匹配会让它们大量残留、污染逐字词筛选的结果；而辅助码字形筛选的
    /// 目标是短字词。**只作用于词组**，单字恒参与匹配。0 = 不设限（保留全部词组）。
    pub max_phrase_len: usize,
}

/// 判断候选文本是否为「单字符」（非词组）。
///
/// **字集不在此处判断**：输入法引擎产出的候选即用户意图，字集筛选（简繁/字符集等）
/// 由输入法自身的相关选项在上游决定；这里对汉字/字母/数字/标点一视同仁、按表查询，
/// 表里无码自然过滤。
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
/// **前缀态语义**：辅助码可以**少于** N 位——输入在字前耗尽 = 尚未打满，词组保留
/// （边打边缩，用户打 1 位「时间」还在、打满 `om` 才精确锁定）。只有**超过** N 位
/// （字全部对齐后仍有剩余）或某位不中才过滤——多出的位没有字可对应，词组配不上
/// 这段辅助码（否则 2 字词会被 3 位辅助码静默保留、挤掉真正匹配的 3 字词）。
///
/// **零分配**：逐字对齐走字符迭代器 + `any_code_starts_with_char`，不构造
/// `Vec<char>`/前缀串，是按键热路径。
///
/// 不做纯汉字判断：含英文/数字/标点的字符同样按表查询——表里有码（如「多啦A梦」里
/// A 的码是 a）则可参与匹配；表里无码则查表未收录、自然过滤。判断交给查表结果，
/// 无需显式拦截。
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
/// 输出 `FilterOutcome` 与 `wind-candidate::filter_candidates` 完同构，
/// 下游的「翻页放宽机制」可直接对接：
/// - `kept`：通过筛选的候选，是输入列表的**子序列**（保持原相对顺序）
/// - `filtered`：其余候选，保持原相对顺序
///
/// 辅助码输入为空、或码表为空（未挂载任何辅助码）时**不过滤**——原样放行全部候选。
/// 辅助码模式由触发键进入，正常流程不会空手调用本函数；这里是防御语义，避免误用把
/// 候选窗整个滤空。
///
/// 词组候选在按逐字首码匹配裁决前先受 **`max_phrase_len`** 长度上限约束
/// （字数 > 上限的句子/长组合词直接排除，见 [`AuxCodeFilterOptions`]）。
///
/// 参数 `table` 任何 [`AuxCodeLookup`] 实现均可（文件表、方案视图、按序拼接的多来源）。
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
