//! 双拼击键坐标整句图（设计见 `docs/design/aux-code-continuous.md` §5.1）。
//!
//! 与 [`LatticeBuilder::build`] 等的区别只在坐标：节点 `start/end` 是**击键偏移**，边是
//! [`ShuangpinConverter::pair_edges`](super::shuangpin::ShuangpinConverter::pair_edges) 的键对；
//! 查词仍用逐边选出的读音拼成的全拼码，`syl_mask` 是该码自身的音节掩码（与
//! `DictEntry::boundary` 同域），打分走同一个 [`score_node`]。
//!
//! ## 与旧路径对齐的几处
//!
//! - **读音**：每条边取首个读音（0 处）并按模糊配置展开（[`FuzzyMatcher::fuzzy_variants_scored`]，
//!   同键韵母 0 处、模糊组按处数），与旧路径「`convert` 只交首读音、引擎再按模糊展开」同集合。
//!   边上其余读音（对偶声母、同键韵母）都在展开结果里；出厂布局固有的同键对（luo/lo 等）旧路径
//!   刻意只出首读音，这里同样不收。
//! - **组合预算**：逐层延伸、同层超过 [`FUZZY_LAYER_BUDGET`] 时按处数截，与
//!   [`LatticeBuilder::fuzzy_spans`] 同一闸门；系统词另用 `has_longer_code` 剪掉死前缀。
//! - **歧义罚**：双拼旧路径的切分图是真值链，歧义接缝恒空，故不计。
//! - **`'`**：并入前一条边的末尾（打在开头的并入第一条边），`ni'hc` 的「你」是 `[0,3)`，
//!   图上不留空洞；与 `map_consumed_length` 把分隔符算给前一段一致。
//! - **`canon`**：恒为 `Some((全拼码, 掩码))`。所打码是击键、不能当造词码，`learn_code`
//!   须从这里取全拼。
//! - **屏蔽词**：不在这里滤，调用方照旧对节点向量调 `drop_blocked_nodes`（按 `word` 滤）。
//!
//! ## 辅码边（连续辅助码）
//!
//! 传入辅码表时边取 [`pair_edges_all`](super::shuangpin::ShuangpinConverter::pair_edges_all)，
//! 图从单条槽链变成击键偏移上的 DAG：每个音节之后可再吸收 1～2 个辅码字母。辅码只能紧跟
//! 音节的两键、全是字母且在同一 `'` 段内（音节后有 `'` 就不接辅码），辅码后紧随的 `'` 并入辅码。
//! 节点合法须：词的字数等于音节数，且每处辅码前缀都是它所跟那个字的某个辅码的开头。
//! 规范码与掩码不含辅码字母，辅码位置记在 [`AuxSpans`]。没有辅码表时与单链逐项相同。

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use wind_dict::DictManager;
use wind_dict::cached::{CachedDict, DictHit};

use super::PinyinEngine;
use super::fuzzy::{FuzzyConfig, FuzzyMatcher};
use super::lattice::{
    FUZZY_LAYER_BUDGET, FUZZY_SYLLABLE_LOG_PENALTY, LatticeBuilder, LatticeNode,
    PARTIAL_FINAL_NODE_LIMIT, PARTIAL_FINAL_PENALTY, USER_NODE_BONUS, USER_NODE_LIMIT,
    USER_NODE_WEIGHT_CAP, is_sentence_store_word, score_node, score_node_partial_final,
};
use super::shuangpin::RawEdge;
use super::viterbi::{ViterbiResult, WordNode};
use crate::engine::AuxCharLookup;
use crate::prof::{self, Counter, Stage};

/// 每个辅码字母扣的分（对数概率轴，与 [`FUZZY_SYLLABLE_LOG_PENALTY`] 同轴）：1 位扣 2.0，2 位扣 4.0。
pub(crate) const AUX_LETTER_LOG_PENALTY: f64 = 2.0;

#[cfg(not(test))]
#[inline(always)]
fn aux_letter_penalty() -> f64 {
    AUX_LETTER_LOG_PENALTY
}

#[cfg(test)]
thread_local! {
    static AUX_PENALTY: std::cell::Cell<f64> = const { std::cell::Cell::new(AUX_LETTER_LOG_PENALTY) };
}

#[cfg(test)]
fn aux_letter_penalty() -> f64 {
    AUX_PENALTY.with(std::cell::Cell::get)
}

/// 测试里覆写本线程的辅码罚分，返回旧值（用完须还原）。
#[cfg(test)]
pub(crate) fn set_aux_letter_penalty(v: f64) -> f64 {
    AUX_PENALTY.with(|c| c.replace(v))
}

/// 一种读法的辅码罚分：辅码字母各扣 λ。
fn aux_cost(s: &Shape) -> f64 {
    aux_letter_penalty() * s.aux_len as f64
}

/// 比分闸门的裕度（对数概率轴）：终点为整串、用了辅码的整句要比同终点不用辅码的最优路径
/// 高出这么多，才排到末槽整句之前。语法模型开时分数尺度整体放大，另用一档。
///
/// 两份语料（189 + 164 句）× 三布局：模型关时逐键前缀假读法分差多在 4.1 以下（另有 3 条 5.8～7.9），
/// 整串正当辅码除 2 例外最小 4.98（有 2 条恰好被 5 挡住）；两侧有重叠，5 是折中。模型开时假读法 ≤ 9.0，正当辅码除 2 例外
/// 都 ≥ 10.8（另 1 条 8.7）。
pub(crate) const AUX_GATE_MARGIN: f64 = 5.0;
/// 语法模型开、权重为 [`AUX_GATE_REF_WEIGHT`] 时的 [`AUX_GATE_MARGIN`]。
pub(crate) const AUX_GATE_MARGIN_GRAMMAR: f64 = 10.0;
/// [`AUX_GATE_MARGIN_GRAMMAR`] 标定时的 `grammar.weight`（移动端出厂值；桌面出厂不挂模型，勾上后同值）。
pub(crate) const AUX_GATE_REF_WEIGHT: f64 = 0.5;

/// 出厂裕度。`grammar`：没挂模型 = `None`；挂了 = 其权重（`Some(None)` = 不报权重，按 0.5）。
/// 模型的转移分是 `weight × (…)`，分差尺度随权重放大，裕度按权重线性缩放：权重 0 退回模型关
/// 的 5，0.5（移动端出厂）时为 10。
fn default_gate_margin(grammar: Option<Option<f64>>) -> f64 {
    match grammar {
        None => AUX_GATE_MARGIN,
        Some(w) => {
            let w = w.unwrap_or(AUX_GATE_REF_WEIGHT).max(0.0);
            AUX_GATE_MARGIN + (AUX_GATE_MARGIN_GRAMMAR - AUX_GATE_MARGIN) * w / AUX_GATE_REF_WEIGHT
        }
    }
}

#[cfg(not(test))]
#[inline(always)]
pub(super) fn aux_gate_margin(grammar: Option<Option<f64>>) -> f64 {
    default_gate_margin(grammar)
}

#[cfg(test)]
thread_local! {
    static GATE_MARGIN: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn aux_gate_margin(grammar: Option<Option<f64>>) -> f64 {
    GATE_MARGIN
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| default_gate_margin(grammar))
}

/// 测试里覆写本线程的闸门裕度（模型开 / 关同值；`None` = 出厂两档），返回旧值（用完须还原）。
#[cfg(test)]
pub(crate) fn set_aux_gate_margin(v: Option<f64>) -> Option<f64> {
    GATE_MARGIN.with(|c| c.replace(v))
}

/// 落单（末键在 P1 读法里落单、被当成末字的 1 位辅码）时的裕度。落单读法常是「下一字的声母」
/// 被借作辅码，假读法分差比非落单高。两份语料 × 三布局 × 模型关 / 开（权重 0.3～0.8）逐键前缀上，
/// 排除文字仍是原句前缀的（文字对，只吞了下一字的声母；模型关最高 12.5），假读法分差 ≤ 8.1；
/// 且不随权重放大，模型开关同取一值。
pub(crate) const AUX_DANGLING_MARGIN: f64 = 12.0;

/// 落单时的放行裕度：`None` = 一律不放行。
#[cfg(not(test))]
#[inline(always)]
pub(super) fn aux_dangling_margin() -> Option<f64> {
    Some(AUX_DANGLING_MARGIN)
}

#[cfg(test)]
thread_local! {
    /// `None` = 出厂；`Some(None)` = 一律不放行；`Some(Some(v))` = 按 v 比分。
    static DANGLING_MARGIN: std::cell::Cell<Option<Option<f64>>> = const { std::cell::Cell::new(None) };
    static GATE_GAPS: RefCell<Vec<GateGap>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(super) fn aux_dangling_margin() -> Option<f64> {
    DANGLING_MARGIN
        .with(std::cell::Cell::get)
        .unwrap_or(Some(AUX_DANGLING_MARGIN))
}

/// 测试里覆写本线程的落单裕度（`None` = 出厂，`Some(None)` = 一律不放行），返回旧值。
#[cfg(test)]
pub(crate) fn set_aux_dangling_margin(v: Option<Option<f64>>) -> Option<Option<f64>> {
    DANGLING_MARGIN.with(|c| c.replace(v))
}

/// 闸门比分记录。
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct GateGap {
    pub sentence: String,
    /// `'n'` = 终点整串的辅码整句 S_n，`'e'` = 用了辅码的末槽整句 S_e
    pub kind: char,
    pub dangling: bool,
    /// 辅码整句的分；没比分（落单一律不放行时）为 `None`
    pub score: Option<f64>,
    /// 同终点不用辅码的最优路径的分；`None` = 没有对手
    pub rival: Option<f64>,
    pub chars: usize,
    pub aux_letters: usize,
}

#[cfg(test)]
impl GateGap {
    /// 分差（有对手且比过分时）。
    pub fn gap(&self) -> Option<f64> {
        self.score.zip(self.rival).map(|(s, p)| s - p)
    }
}

#[cfg(test)]
pub(super) fn note_gate_gap(g: GateGap) {
    GATE_GAPS.with(|v| v.borrow_mut().push(g));
}

/// 取走并清空闸门比分记录。
#[cfg(test)]
pub(crate) fn take_gate_gaps() -> Vec<GateGap> {
    GATE_GAPS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// 末槽整句（终点 e）用了辅码时的闸门裕度：`None` = 不设闸。
pub(crate) const AUX_SE_GATE_MARGIN: Option<f64> = None;

#[cfg(not(test))]
#[inline(always)]
pub(super) fn aux_se_gate_margin() -> Option<f64> {
    AUX_SE_GATE_MARGIN
}

#[cfg(test)]
thread_local! {
    static SE_GATE_MARGIN: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(AUX_SE_GATE_MARGIN) };
}

#[cfg(test)]
pub(super) fn aux_se_gate_margin() -> Option<f64> {
    SE_GATE_MARGIN.with(std::cell::Cell::get)
}

/// 测试里覆写本线程的末槽整句闸门裕度，返回旧值。
#[cfg(test)]
pub(crate) fn set_aux_se_gate_margin(v: Option<f64>) -> Option<f64> {
    SE_GATE_MARGIN.with(|c| c.replace(v))
}

/// 同文合并时用了辅码的整句是否改写消费数（`mod.rs` 的 `aux_whole`）；测试可关掉以量它的贡献。
#[cfg(not(test))]
#[inline(always)]
pub(super) fn aux_whole_enabled() -> bool {
    true
}

#[cfg(test)]
thread_local! {
    static AUX_WHOLE_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static AUX_SENTENCES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(super) fn aux_whole_enabled() -> bool {
    !AUX_WHOLE_OFF.with(std::cell::Cell::get)
}

/// 测试里关掉本线程的 `aux_whole` 改写，返回旧值。
#[cfg(test)]
pub(crate) fn set_aux_whole_off(v: bool) -> bool {
    AUX_WHOLE_OFF.with(|c| c.replace(v))
}

/// 测试探针：记下用了辅码的整句文字（step 2 / 2c）。
#[cfg(test)]
pub(super) fn note_aux_sentence(text: &str) {
    AUX_SENTENCES.with(|v| v.borrow_mut().push(text.to_string()));
}

/// 取走并清空探针记下的整句。
#[cfg(test)]
pub(crate) fn take_aux_sentences() -> Vec<String> {
    AUX_SENTENCES.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// 一处辅码：(词内字序, 1～2 位小写前缀)。
pub(crate) type AuxSpan = (usize, String);

/// 带辅码节点的辅码位置：`(起点, 终点)` → `[(词, 辅码)]`。无辅码的节点不登记。
///
/// 转解码节点时每个节点都要查一次，键只是两个偏移，用 [`SpanHasher`] 而不走 SipHash。
#[derive(Default)]
pub(crate) struct AuxSpans(
    HashMap<(usize, usize), Vec<WordAux>, std::hash::BuildHasherDefault<SpanHasher>>,
);

/// [`AuxSpans`] 键的哈希：逐个整数混入、乘奇数常量。
#[derive(Default)]
pub(crate) struct SpanHasher(u64);

impl std::hash::Hasher for SpanHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(29) ^ v).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
    fn write_u32(&mut self, v: u32) {
        self.write_u64(u64::from(v));
    }
}

/// 键为整数的小表：哈希走 [`SpanHasher`]。
type CheapMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<SpanHasher>>;

/// 一个节点的 `(词, 辅码)`。
type WordAux = (String, Vec<AuxSpan>);

impl AuxSpans {
    pub(crate) fn get(&self, start: usize, end: usize, word: &str) -> Option<&[AuxSpan]> {
        self.0
            .get(&(start, end))?
            .iter()
            .find(|(w, _)| w == word)
            .map(|(_, a)| a.as_slice())
    }

    /// 登记节点的辅码（空表示无辅码，删掉旧登记）。
    fn set(&mut self, start: usize, end: usize, word: &str, aux: &[AuxSpan]) {
        if aux.is_empty() {
            if let Some(v) = self.0.get_mut(&(start, end)) {
                v.retain(|(w, _)| w != word);
            }
            return;
        }
        let v = self.0.entry((start, end)).or_default();
        match v.iter_mut().find(|(w, _)| w == word) {
            Some(slot) => slot.1 = aux.to_vec(),
            None => v.push((word.to_string(), aux.to_vec())),
        }
    }

    fn letters(&self, start: usize, end: usize, word: &str) -> u8 {
        if self.0.is_empty() {
            return 0;
        }
        self.get(start, end, word)
            .map_or(0, |a| a.iter().map(|(_, p)| p.len() as u8).sum())
    }
}

/// 单次建图内的辅码查表：按字缓存首字母掩码（[`AuxCharLookup::letter_mask`]），1 位前缀只比掩码；
/// 2 位前缀只对首字母已过的字查 `(字, 前缀)`（前缀单调：2 位命中 ⊆ 首字母命中），结果同样缓存。
///
/// 同一码的命中在接 1 / 2 位辅码的各终点上逐条再判，99% 被拒；按字取一次掩码后，这些判定
/// 大多是一次廉价哈希查找加一次位与。
pub(crate) struct AuxCtx<'a> {
    lookup: &'a dyn AuxCharLookup,
    masks: RefCell<CheapMap<char, u32>>,
    cache: RefCell<CheapMap<(char, [u8; 2]), bool>>,
    /// 测试参照：不用掩码、逐字查 `(字, 前缀)`（批量判定之前的实现）
    #[cfg(test)]
    per_char: bool,
}

impl<'a> AuxCtx<'a> {
    pub(crate) fn new(lookup: &'a dyn AuxCharLookup) -> Self {
        Self {
            lookup,
            masks: RefCell::default(),
            cache: RefCell::default(),
            #[cfg(test)]
            per_char: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_per_char(mut self, per_char: bool) -> Self {
        self.per_char = per_char;
        self
    }

    fn hit(&self, ch: char, prefix: &str) -> bool {
        prof::count(Counter::CharChecks, 1);
        #[cfg(test)]
        if self.per_char {
            return self.hit_exact(ch, prefix);
        }
        let bit = crate::engine::aux_code_letter_bit(prefix);
        if bit == 0 {
            return self.hit_exact(ch, prefix);
        }
        let t0 = prof::fine_now();
        let mask = *self.masks.borrow_mut().entry(ch).or_insert_with(|| {
            prof::count(Counter::TraitCalls, 1);
            let t1 = prof::fine_now();
            let m = self.lookup.letter_mask(ch);
            prof::fine_add(Stage::FineTrait, t1);
            m
        });
        // 含 trait 调用，`prof::take` 时扣掉。
        prof::fine_add(Stage::FineCache, t0);
        if mask & bit == 0 {
            return false;
        }
        prefix.len() == 1 || self.hit_exact(ch, prefix)
    }

    /// `(字, 前缀)` 逐个查表（缓存结果）。
    fn hit_exact(&self, ch: char, prefix: &str) -> bool {
        let t0 = prof::fine_now();
        let b = prefix.as_bytes();
        let key = (ch, [b[0], b.get(1).copied().unwrap_or(0)]);
        let r = *self.cache.borrow_mut().entry(key).or_insert_with(|| {
            prof::count(Counter::TraitCalls, 1);
            let t1 = prof::fine_now();
            let r = self.lookup.any_code_starts_with(ch, prefix);
            prof::fine_add(Stage::FineTrait, t1);
            r
        });
        // 含 trait 调用，`prof::take` 时扣掉。
        prof::fine_add(Stage::FineCache, t0);
        r
    }

    /// 读法 `shape` 的辅码对词 `word` 是否成立（无辅码恒成立）。
    fn admits(&self, shape: &Shape, word: &str) -> bool {
        if shape.aux.is_empty() {
            return true;
        }
        prof::count(Counter::Admits, 1);
        let t0 = prof::fine_now();
        let n = word.chars().count();
        prof::fine_add(Stage::FineNth, t0);
        let ok = n == shape.syllables.len()
            && shape.aux.iter().all(|(i, p)| {
                let t0 = prof::fine_now();
                let c = word.chars().nth(*i);
                prof::fine_add(Stage::FineNth, t0);
                c.is_some_and(|c| self.hit(c, p))
            });
        if ok {
            prof::count(Counter::Admitted, 1);
        }
        ok
    }
}

/// 一个音节槽：边的击键跨度（已并入相邻 `'`）与可选读音 `(全拼音节, 处数)`。
struct Slot {
    start: usize,
    end: usize,
    /// 边自身的终点（未并 `'`）；`raw_end == end` 时音节后可接辅码
    raw_end: usize,
    options: Vec<(String, usize)>,
}

/// 从某个槽出发拼出的一种读法。音节与辅码与延伸中的组合共享（同一读法接 0/1/2 位辅码时不拷）。
struct Shape {
    /// 全拼码自身的音节掩码
    mask: u64,
    syllables: Rc<Vec<String>>,
    edits: usize,
    /// 每条边都取首读音（= 旧路径的所打码）
    exact: bool,
    /// 词内第几字后吸收了哪几位辅码
    aux: Rc<Vec<AuxSpan>>,
    /// 辅码字母总数
    aux_len: usize,
}

/// `(终点击键偏移, 去辅码全拼码)` → 拼出它的各种读法。BTreeMap：查询与建节点顺序确定。
type Spans = BTreeMap<(usize, String), Vec<Shape>>;

struct RawChain {
    slots: Vec<Slot>,
    /// `at[p]`：起点为击键偏移 `p` 的槽（每个偏移至多一个）
    at: Vec<Option<usize>>,
    /// 槽 i 的起点从 0 可达（经音节，辅码模式下也经辅码）
    reachable: Vec<bool>,
    /// 辅码模式下的击键串（小写），`None` = 不接辅码边
    aux_keys: Option<Vec<u8>>,
}

impl RawChain {
    fn new(keys: &str, edges: &[RawEdge], fuzzy: Option<&FuzzyConfig>, with_aux: bool) -> Self {
        let b = keys.as_bytes();
        let fuzzy = fuzzy.filter(|f| f.any_enabled());
        let slots: Vec<Slot> = edges
            .iter()
            .filter(|e| is_slot_edge(b, e))
            .map(|e| {
                let (start, end) = slot_span(b, e);
                let first = &e.syllables[0];
                let mut options = vec![(first.clone(), 0)];
                if let Some(f) = fuzzy {
                    options.extend(FuzzyMatcher::fuzzy_variants_scored(first, f));
                }
                Slot {
                    start,
                    end,
                    raw_end: e.end,
                    options,
                }
            })
            .collect();
        let mut at = vec![None; b.len() + 1];
        for (i, s) in slots.iter().enumerate() {
            at[s.start] = Some(i);
        }
        let aux_keys = with_aux.then(|| keys.to_ascii_lowercase().into_bytes());
        let mut chain = Self {
            slots,
            at,
            reachable: Vec::new(),
            aux_keys,
        };
        // 槽按起点升序，前驱的终点都在它之前，一遍前推即可。
        let mut pos = vec![false; b.len() + 1];
        pos[0] = true;
        let mut reachable = Vec::with_capacity(chain.slots.len());
        for i in 0..chain.slots.len() {
            let r = pos[chain.slots[i].start];
            reachable.push(r);
            if r {
                pos[chain.slots[i].end] = true;
                for (_, end) in chain.aux_ends(i) {
                    pos[end] = true;
                }
            }
        }
        chain.reachable = reachable;
        chain
    }

    /// 槽 `i` 之后可吸收的辅码：`(小写前缀, 并入其后 '` 的终点)`，1 位在前。
    fn aux_ends(&self, i: usize) -> Vec<(String, usize)> {
        let Some(k) = &self.aux_keys else {
            return Vec::new();
        };
        let s = &self.slots[i];
        if s.raw_end != s.end {
            return Vec::new();
        }
        let mut out = Vec::new();
        for a in 1..=2 {
            let r = s.raw_end + a;
            if r > k.len() || !k[s.raw_end..r].iter().all(u8::is_ascii_alphabetic) {
                break;
            }
            let end = r + k[r..].iter().take_while(|&&c| c == b'\'').count();
            out.push((String::from_utf8_lossy(&k[s.raw_end..r]).into_owned(), end));
        }
        out
    }

    /// 从槽 `i` 起、至多 `max_syllables` 个音节的全部读法，按 `(终点, 全拼码)` 归并。
    /// `can_extend(码)` 为假的组合不再延伸。
    fn spans_from(
        &self,
        i: usize,
        max_syllables: usize,
        can_extend: &dyn Fn(&str) -> bool,
    ) -> Spans {
        struct Combo {
            code: String,
            mask: u64,
            syls: Rc<Vec<String>>,
            edits: usize,
            exact: bool,
            end: usize,
            aux: Rc<Vec<AuxSpan>>,
            aux_len: usize,
        }
        let mut spans = Spans::new();
        let mut layer = vec![Combo {
            code: String::new(),
            mask: 0,
            syls: Rc::default(),
            edits: 0,
            exact: true,
            end: self.slots[i].start,
            aux: Rc::default(),
            aux_len: 0,
        }];
        for k in 0..max_syllables {
            let mut next: Vec<Combo> = Vec::new();
            for c in &layer {
                if !c.code.is_empty() && !can_extend(&c.code) {
                    continue;
                }
                let Some(j) = self.at.get(c.end).copied().flatten() else {
                    continue;
                };
                let bit = if c.code.len() < 64 {
                    1u64 << c.code.len()
                } else {
                    0
                };
                let aux_ends = self.aux_ends(j);
                for (idx, (opt, opt_edits)) in self.slots[j].options.iter().enumerate() {
                    let mut syls = Vec::with_capacity(c.syls.len() + 1);
                    syls.extend_from_slice(&c.syls);
                    syls.push(opt.clone());
                    let base = Combo {
                        code: format!("{}{opt}", c.code),
                        mask: c.mask | bit,
                        syls: Rc::new(syls),
                        edits: c.edits + opt_edits,
                        exact: c.exact && idx == 0,
                        end: self.slots[j].end,
                        aux: Rc::clone(&c.aux),
                        aux_len: c.aux_len,
                    };
                    for (prefix, end) in &aux_ends {
                        let mut aux = Vec::with_capacity(base.aux.len() + 1);
                        aux.extend_from_slice(&base.aux);
                        aux.push((k, prefix.clone()));
                        next.push(Combo {
                            code: base.code.clone(),
                            syls: Rc::clone(&base.syls),
                            end: *end,
                            aux: Rc::new(aux),
                            aux_len: base.aux_len + prefix.len(),
                            ..base
                        });
                    }
                    next.push(base);
                }
            }
            if next.is_empty() {
                break;
            }
            if next.len() > FUZZY_LAYER_BUDGET {
                // 按罚分（同 `pick_shape`）稳定排序：首读音、无辅码的组合（罚分 0、先生成）恒在最前，
                // 不会被截。无辅码时即按处数。
                next.sort_by(|a, b| {
                    let cost = |c: &Combo| {
                        FUZZY_SYLLABLE_LOG_PENALTY * c.edits as f64
                            + aux_letter_penalty() * c.aux_len as f64
                    };
                    cost(a).total_cmp(&cost(b))
                });
                next.truncate(FUZZY_LAYER_BUDGET);
            }
            for c in &next {
                spans
                    .entry((c.end, c.code.clone()))
                    .or_default()
                    .push(Shape {
                        mask: c.mask,
                        syllables: Rc::clone(&c.syls),
                        edits: c.edits,
                        exact: c.exact,
                        aux: Rc::clone(&c.aux),
                        aux_len: c.aux_len,
                    });
            }
            layer = next;
        }
        spans
    }
}

/// 成槽的边：有读音、不越界。
fn is_slot_edge(b: &[u8], e: &RawEdge) -> bool {
    !e.syllables.is_empty() && e.end <= b.len()
}

/// 槽的击键跨度：前导 `'` 并入第一条边，紧随的 `'` 并入本边。
fn slot_span(b: &[u8], e: &RawEdge) -> (usize, usize) {
    let start = if b[..e.start].iter().all(|&c| c == b'\'') {
        0
    } else {
        e.start
    };
    let end = e.end + b[e.end..].iter().take_while(|&&c| c == b'\'').count();
    (start, end)
}

/// 末槽的终点（击键偏移）；没有槽时为 `None`。
fn last_slot_end(keys: &str, edges: &[RawEdge]) -> Option<usize> {
    let b = keys.as_bytes();
    edges
        .iter()
        .rfind(|e| is_slot_edge(b, e))
        .map(|e| slot_span(b, e).1)
}

/// step 2 整句的终点：图须铺到末槽，否则整句的码与边界（按全部音节写）和消费数对不上。
fn sentence_end(reach: usize, last_slot_end: Option<usize>) -> Option<usize> {
    (last_slot_end == Some(reach)).then_some(reach)
}

/// 为命中词 `word`（词典边界 `boundary`）挑读法：同码里有首读音组合时只在它们里挑（同旧路径：
/// 所打码本身不走模糊分支）；再按边界与辅码合法性筛，取罚分（模糊处数与辅码字母各按其罚分计）
/// 最少者，平手取辅码少的。无辅码时即「处数最少」。
fn pick_shape<'s>(
    shapes: &'s [Shape],
    fits: impl Fn(&Shape) -> bool,
    word: &str,
    aux: Option<&AuxCtx>,
) -> Option<&'s Shape> {
    let any_exact = shapes.iter().any(|s| s.exact);
    let cost = |s: &Shape| FUZZY_SYLLABLE_LOG_PENALTY * s.edits as f64 + aux_cost(s);
    shapes
        .iter()
        .filter(|s| s.exact || !any_exact)
        .filter(|s| fits(s))
        .filter(|s| aux.is_none_or(|a| a.admits(s, word)))
        .min_by(|a, b| cost(a).total_cmp(&cost(b)).then(a.aux_len.cmp(&b.aux_len)))
}

/// 同词同跨度去重：`(模糊来源, 辅码字母数)` 与分数（已扣辅码罚分）→ 新节点是否替换旧节点。
/// 辅码数相同时同旧路径（精确恒胜、否则分高者）；不同时取分高者，平分留辅码少的。
fn should_replace(old: (bool, usize, f64), new: (bool, usize, f64)) -> bool {
    let ((old_fuzzy, old_aux, old_lp), (new_fuzzy, new_aux, new_lp)) = (old, new);
    if old_aux != new_aux {
        return new_lp > old_lp || (new_lp == old_lp && new_aux < old_aux);
    }
    match (old_fuzzy, new_fuzzy) {
        (true, false) => true,
        (false, true) => false,
        _ => new_lp > old_lp,
    }
}

impl LatticeBuilder {
    /// 击键坐标版 [`Self::build`]：`nodes[end]` = 在击键偏移 `end` 结束的节点，长度 `keys.len()+1`。
    ///
    /// 系统词的约束与打分同 `build`：首读音组合是精确节点，词条边界须等于其掩码（`0` 放行）；
    /// 模糊读音扣 [`FUZZY_SYLLABLE_LOG_PENALTY`] × 处数，边界须等于某读法的掩码（`0` 放行、取处数
    /// 最少者）；同词同起点精确恒胜，模糊之间取分高者。
    /// `require_reachable` 同 `build`：只在从 0 连通的槽上起节点。
    #[cfg(test)]
    pub(crate) fn build_raw(
        &self,
        keys: &str,
        edges: &[RawEdge],
        dict: &CachedDict,
        fuzzy_config: Option<&FuzzyConfig>,
        require_reachable: bool,
    ) -> Vec<Vec<LatticeNode>> {
        self.build_raw_aux(keys, edges, dict, fuzzy_config, require_reachable, None)
            .0
    }

    /// [`Self::build_raw`] 带辅码边：`aux` 为 `Some` 时 `edges` 应取 `pair_edges_all`，另返回
    /// 带辅码节点的辅码位置。`None` 时与 `build_raw` 相同、登记表为空。
    pub(crate) fn build_raw_aux(
        &self,
        keys: &str,
        edges: &[RawEdge],
        dict: &CachedDict,
        fuzzy_config: Option<&FuzzyConfig>,
        require_reachable: bool,
        aux: Option<&AuxCtx>,
    ) -> (Vec<Vec<LatticeNode>>, AuxSpans) {
        let mut nodes: Vec<Vec<LatticeNode>> = vec![Vec::new(); keys.len() + 1];
        let mut aux_spans = AuxSpans::default();
        let chain = RawChain::new(keys, edges, fuzzy_config, aux.is_some());
        // 当前 (起点, 终点) 上的词 → (在 `nodes[终点]` 的下标, 是否模糊来源, 辅码字母数)；模糊来源的
        // 节点在精确节点到来时要被顶掉。常见音节同音字上千，线性找同词会退化成平方。
        let mut seen: HashMap<String, (usize, bool, usize)> = HashMap::new();
        // 辅码模式下同一起点的同一码会出现在多个终点（接 0/1/2 位辅码），命中按码缓存、只查一次。
        let mut hit_cache: Option<HashMap<String, Vec<DictHit>>> = aux.map(|_| HashMap::new());
        for i in 0..chain.slots.len() {
            if require_reachable && !chain.reachable[i] {
                continue;
            }
            let p = chain.slots[i].start;
            if let Some(c) = &mut hit_cache {
                c.clear();
            }
            // 对精确组合也剪：只是省查询，与旧精确分支不剪枝的结果相同。
            let spans = {
                let _t = prof::span(Stage::Spans);
                chain.spans_from(i, self.max_word_len(), &|c| dict.has_longer_code(c))
            };
            let mut groups: Vec<_> = spans.iter().collect();
            // 缓存时：同一码最后用到的那一组才能把命中文本移进节点（之前的组只能拷）。按终点降序走，
            // 不带辅码的终点（最靠前、节点最多）就排在最后；终点内码序不变，`nodes[q]` 的次序同升序。
            let mut uses: HashMap<&str, usize> = HashMap::new();
            if hit_cache.is_some() {
                groups.sort_by_key(|((q, _), _)| std::cmp::Reverse(*q));
                for (_, code) in spans.keys() {
                    *uses.entry(code.as_str()).or_default() += 1;
                }
            }
            // 槽起点互不相同，同一终点的跨度在 `spans` 里相邻：换终点时清表即可。
            let mut seen_q = None;
            for ((q, code), shapes) in groups {
                let q = *q;
                if seen_q != Some(q) {
                    seen.clear();
                    seen_q = Some(q);
                }
                let mut owned;
                // 还有别的组要用这份命中时只能拷文本。
                let cached = uses.get_mut(code.as_str()).is_some_and(|n| {
                    *n -= 1;
                    *n > 0
                });
                let search = prof::span(Stage::Search);
                let hits: &mut Vec<DictHit> = match &mut hit_cache {
                    Some(c) => c
                        .entry(code.clone())
                        .or_insert_with(|| dict.search_with_boundary(code)),
                    None => {
                        owned = dict.search_with_boundary(code);
                        &mut owned
                    }
                };
                drop(search);
                let with_aux = shapes.iter().any(|s| !s.aux.is_empty());
                let _t = prof::span(if with_aux {
                    Stage::HitsAux
                } else {
                    Stage::HitsPlain
                });
                if with_aux {
                    prof::count(Counter::AuxHits, hits.len() as u64);
                }
                for hit in hits.iter_mut() {
                    let fits = |s: &Shape| hit.boundary == 0 || s.mask == hit.boundary;
                    let Some(shape) = pick_shape(shapes, fits, &hit.text, aux) else {
                        // 精确读法掩码不符时直接跳过、不再试模糊读法；边链固定，实际碰不到。
                        continue;
                    };
                    let log_prob = score_node(&hit.text, code, hit.weight)
                        - FUZZY_SYLLABLE_LOG_PENALTY * shape.edits as f64
                        - aux_cost(shape);
                    let is_fuzzy = !shape.exact;
                    if let Some((at, existing_fuzzy, existing_aux)) =
                        seen.get_mut(hit.text.as_str())
                    {
                        let existing = &mut nodes[q][*at];
                        if should_replace(
                            (*existing_fuzzy, *existing_aux, existing.log_prob),
                            (is_fuzzy, shape.aux_len, log_prob),
                        ) {
                            if *existing_aux != 0 || shape.aux_len != 0 {
                                aux_spans.set(p, q, &hit.text, &shape.aux);
                            }
                            *existing =
                                raw_node(p, q, take_text(hit, cached), code, shape, log_prob);
                            *existing_fuzzy = is_fuzzy;
                            *existing_aux = shape.aux_len;
                        }
                        continue;
                    }
                    if shape.aux_len != 0 {
                        aux_spans.set(p, q, &hit.text, &shape.aux);
                    }
                    seen.insert(hit.text.clone(), (nodes[q].len(), is_fuzzy, shape.aux_len));
                    nodes[q].push(raw_node(
                        p,
                        q,
                        take_text(hit, cached),
                        code,
                        shape,
                        log_prob,
                    ));
                }
            }
        }
        (nodes, aux_spans)
    }

    /// 击键坐标版 [`Self::add_store_nodes`]：用户词 / 临时词按全拼码点查进图。
    ///
    /// 约束同原方法：只收 [`is_sentence_store_word`]；记录边界须**等于**某读法的掩码
    /// （`boundary == 0` 不进图）；模糊读法扣 [`FUZZY_SYLLABLE_LOG_PENALTY`] × 处数，再叠
    /// [`USER_NODE_WEIGHT_CAP`] / [`USER_NODE_BONUS`]；同词同起点取分高者。须在 [`Self::build_raw`] 之后调。
    #[cfg(test)]
    pub(crate) fn add_store_nodes_raw(
        &self,
        keys: &str,
        edges: &[RawEdge],
        store: &DictManager,
        fuzzy_config: Option<&FuzzyConfig>,
        require_reachable: bool,
        nodes: &mut [Vec<LatticeNode>],
    ) {
        let mut spans = AuxSpans::default();
        self.add_store_nodes_raw_aux(
            keys,
            edges,
            store,
            fuzzy_config,
            require_reachable,
            None,
            nodes,
            &mut spans,
        );
    }

    /// [`Self::add_store_nodes_raw`] 带辅码边，约定同 [`Self::build_raw_aux`]。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_store_nodes_raw_aux(
        &self,
        keys: &str,
        edges: &[RawEdge],
        store: &DictManager,
        fuzzy_config: Option<&FuzzyConfig>,
        require_reachable: bool,
        aux: Option<&AuxCtx>,
        nodes: &mut [Vec<LatticeNode>],
        aux_spans: &mut AuxSpans,
    ) {
        let chain = RawChain::new(keys, edges, fuzzy_config, aux.is_some());
        for i in 0..chain.slots.len() {
            if require_reachable && !chain.reachable[i] {
                continue;
            }
            let p = chain.slots[i].start;
            let spans = chain.spans_from(i, self.max_word_len(), &|_| true);
            for ((q, code), shapes) in &spans {
                let q = *q;
                if q >= nodes.len() {
                    continue;
                }
                for cand in store.search(code, USER_NODE_LIMIT) {
                    if !is_sentence_store_word(&cand) {
                        continue;
                    }
                    let fits = |s: &Shape| s.mask == cand.boundary;
                    let Some(shape) = pick_shape(shapes, fits, &cand.text, aux) else {
                        continue;
                    };
                    let weight = cand.weight.min(USER_NODE_WEIGHT_CAP);
                    let log_prob = score_node(&cand.text, code, weight)
                        - FUZZY_SYLLABLE_LOG_PENALTY * shape.edits as f64
                        - aux_cost(shape)
                        + USER_NODE_BONUS;
                    if let Some(existing) = nodes[q]
                        .iter_mut()
                        .find(|n| n.word == cand.text && n.start == p)
                    {
                        let old_aux = aux_spans.letters(p, q, &cand.text) as usize;
                        let replace = log_prob > existing.log_prob
                            || (log_prob == existing.log_prob && shape.aux_len < old_aux);
                        if replace {
                            if old_aux != 0 || shape.aux_len != 0 {
                                aux_spans.set(p, q, &cand.text, &shape.aux);
                            }
                            *existing = raw_node(p, q, cand.text, code, shape, log_prob);
                        }
                        continue;
                    }
                    if shape.aux_len != 0 {
                        aux_spans.set(p, q, &cand.text, &shape.aux);
                    }
                    nodes[q].push(raw_node(p, q, cand.text, code, shape, log_prob));
                }
            }
        }
    }

    /// 击键坐标版 [`Self::add_partial_final_nodes`]：尾部落单键 `[completed_len, keys.len())`
    /// 按全拼残码 `partial`（`SpConvertResult::partial_initial`，小鹤 `u` → `sh`）补出单字节点。
    ///
    /// 召回、打分同原方法；`canon` 填 `(partial, 1)`，造词码是全拼残码而非击键。
    #[cfg(test)]
    pub(crate) fn add_partial_final_raw(
        &self,
        keys: &str,
        completed_len: usize,
        partial: &str,
        dict: &CachedDict,
        nodes: &mut [Vec<LatticeNode>],
    ) {
        self.add_partial_final_raw_cached(keys, completed_len, partial, dict, nodes, &mut None);
    }

    /// [`Self::add_partial_final_raw`]，残码的前缀查询结果存进 `cache`、再调时复用
    /// （同一次 convert 里多字残码补全与 2c 查的是同一个残码）。
    fn add_partial_final_raw_cached(
        &self,
        keys: &str,
        completed_len: usize,
        partial: &str,
        dict: &CachedDict,
        nodes: &mut [Vec<LatticeNode>],
        cache: &mut Option<PartialHits>,
    ) {
        let end = keys.len();
        if completed_len >= end
            || end >= nodes.len()
            || partial.is_empty()
            || !partial.bytes().all(|b| b.is_ascii_lowercase())
        {
            return;
        }
        if cache.as_ref().is_none_or(|(p, _)| p != partial) {
            let hits = dict.search_prefix_with_boundary_syllable_capped(
                partial,
                PARTIAL_FINAL_NODE_LIMIT,
                1,
                0,
            );
            *cache = Some((partial.to_string(), hits));
        }
        let Some((_, hits)) = cache.as_ref() else {
            return;
        };
        for hit in hits {
            if hit.text.chars().count() != 1 {
                continue;
            }
            if nodes[end]
                .iter()
                .any(|n| n.word == hit.text && n.start == completed_len)
            {
                continue;
            }
            let log_prob = score_node_partial_final(&hit.text, hit.weight) - PARTIAL_FINAL_PENALTY;
            nodes[end].push(LatticeNode {
                start: completed_len,
                end,
                word: hit.text.clone(),
                syllables: vec![partial.to_string()],
                syl_mask: 1,
                log_prob,
                canon: Some((partial.to_string(), 1)),
            });
        }
    }

    /// 末键落单时的多字残码补全：对每个从 0 可达（`reach`）的起点 `p`，`[p, n-1)` 全是键对音节
    /// （各取首读音、不展开模糊）、末键按全拼残码 `partial` 补，前缀查出字数 = 音节数 + 1 的词，
    /// 节点 `[p, n)`，`canon` 为 (词码, 边界)。`p = n-1` 即单字残码节点（同 [`Self::add_partial_final_raw`]）。
    ///
    /// 与「音节 + 辅码」节点同在 `nodes[n]`、由解码比分：末键既可能是辅码也可能是下一音节的开头。
    /// 同起点同词已有节点时取分高者。`partial_hits` 见 [`Self::add_partial_final_raw_cached`]。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_tail_completion_raw(
        &self,
        keys: &str,
        edges: &[RawEdge],
        partial: &str,
        reach: &[bool],
        dict: &CachedDict,
        nodes: &mut [Vec<LatticeNode>],
        partial_hits: &mut Option<PartialHits>,
    ) {
        let n = keys.len();
        if n == 0 || nodes.len() != n + 1 || reach.len() != n + 1 {
            return;
        }
        if reach[n - 1] {
            self.add_partial_final_raw_cached(keys, n - 1, partial, dict, nodes, partial_hits);
        }
        if partial.is_empty() || !partial.bytes().all(|b| b.is_ascii_lowercase()) {
            return;
        }
        // 起点为 i 的键对 [i, i+2) 的首读音
        let mut pair: Vec<Option<&str>> = vec![None; n];
        for e in edges.iter().filter(|e| e.end == e.start + 2 && e.end <= n) {
            if let Some(s) = e.syllables.first() {
                pair[e.start] = Some(s.as_str());
            }
        }
        // 从 n-1 往前逐对扩：syls 是 [p, n-1) 的音节（正序）
        let mut syls: Vec<&str> = Vec::new();
        let mut p = n - 1;
        while p >= 2 && syls.len() + 1 < self.max_word_len() {
            let Some(s) = pair[p - 2] else {
                break;
            };
            p -= 2;
            syls.insert(0, s);
            if !reach[p] {
                continue;
            }
            let done: String = syls.concat();
            if done.len() >= 63 {
                break;
            }
            let mut expect = 1u64 << done.len();
            let mut off = 0;
            for s in &syls {
                expect |= 1 << off;
                off += s.len();
            }
            let below = (1u64 << (done.len() + 1)) - 1;
            let m = syls.len() + 1;
            for hit in dict.search_prefix_with_boundary_syllable_capped(
                &format!("{done}{partial}"),
                TAIL_COMPLETION_LIMIT,
                m as u32,
                done.len(),
            ) {
                if hit.text.chars().count() != m
                    || (hit.boundary != 0 && hit.boundary & below != expect)
                {
                    continue;
                }
                let log_prob = score_node(&hit.text, &hit.code, hit.weight) - PARTIAL_FINAL_PENALTY;
                let mask = if hit.boundary != 0 {
                    hit.boundary
                } else {
                    expect
                };
                let mut syllables: Vec<String> = syls.iter().map(|s| s.to_string()).collect();
                syllables.push(partial.to_string());
                let node = LatticeNode {
                    start: p,
                    end: n,
                    word: hit.text,
                    syllables,
                    syl_mask: mask,
                    log_prob,
                    canon: Some((hit.code, mask)),
                };
                match nodes[n]
                    .iter_mut()
                    .find(|x| x.start == p && x.word == node.word)
                {
                    Some(x) if node.log_prob > x.log_prob => *x = node,
                    Some(_) => {}
                    None => nodes[n].push(node),
                }
            }
        }
    }
}

/// 多字残码补全每个起点取几条词。
const TAIL_COMPLETION_LIMIT: usize = 20;

/// 残码单字的前缀查询结果：`(全拼残码, 命中)`。
type PartialHits = (String, Vec<DictHit>);

/// step 2 与 2c 共用的击键图底图：系统词 + 用户词、已滤屏蔽词。
pub(super) struct RawBase {
    lattice: Vec<Vec<WordNode>>,
    /// 系统词节点最远的终点（不看用户词），即从 0 连续可达的最远槽末尾。
    reach: usize,
    /// 末槽的终点，见 [`last_slot_end`]
    last_slot_end: Option<usize>,
    /// 辅码模式：各击键位置经系统词节点从 0 可达（不看用户词）。单链时为 `None`，判据照旧用 `reach`。
    node_reach: Option<Vec<bool>>,
    /// 终点为整串、末字带辅码的节点：`(起点, 词, 末字辅码字母数)`。
    tail_aux: Vec<(usize, String, u8)>,
    /// 多字残码补全查过的残码单字，2c 复用
    partial_hits: Option<PartialHits>,
    /// 带辅码节点的辅码分布（单链时为空），辅码整句据此定击键位置（[`aux_path`]）
    aux_spans: AuxSpans,
}

/// 辅码边的输入：全偏移键对、辅码表与末键落单时的全拼残码（`SpConvertResult::partial_initial`）。
#[derive(Clone, Copy)]
pub(super) struct AuxInput<'a> {
    pub edges_all: &'a [RawEdge],
    pub lookup: &'a dyn AuxCharLookup,
    pub tail_partial: Option<&'a str>,
}

impl RawBase {
    /// step 2 的整句图：底图本身，终点是 `reach`（击键数，可直接当 `consumed_length`）。
    /// `reach` 没到末槽时为 `None`（整句跳过，同旧路径末音节无词时解不出）。
    ///
    /// 辅码模式下 DAG 上的位置不再与槽一一对应：终点同 P1 取末槽终点（须可达）；末槽到不了
    /// 而整串可达（末尾是辅码）时取整串。末槽之后整串另可达的那条整句见 [`Self::tail_end`]。
    pub(super) fn sentence(&self) -> Option<(&[Vec<WordNode>], usize)> {
        let end = match &self.node_reach {
            None => sentence_end(self.reach, self.last_slot_end)?,
            Some(r) => {
                let n = r.len() - 1;
                self.last_slot_end
                    .filter(|&e| r[e])
                    .or_else(|| r[n].then_some(n))?
            }
        };
        Some((self.lattice.as_slice(), end))
    }

    /// 辅码模式：[`Self::sentence`] 取的是末槽终点、而整串经辅码读法或残码补全也可达时，
    /// 整串的终点。这条整句与 step 2 的整句并存，排位同 2c。
    pub(super) fn tail_end(&self) -> Option<usize> {
        let r = self.node_reach.as_ref()?;
        let n = r.len() - 1;
        let e = self.last_slot_end.filter(|&e| r[e])?;
        (e < n && r[n]).then_some(n)
    }

    pub(super) fn lattice(&self) -> &[Vec<WordNode>] {
        &self.lattice
    }

    pub(super) fn aux_spans(&self) -> &AuxSpans {
        &self.aux_spans
    }

    /// 去掉带辅码节点的底图（比分闸门的对手：同终点不用辅码的最优路径）。
    pub(super) fn plain_lattice(&self) -> Vec<Vec<WordNode>> {
        self.lattice
            .iter()
            .map(|v| v.iter().filter(|w| w.aux == 0).cloned().collect())
            .collect()
    }

    /// 终点为整串的节点 `(start, word)` 末字带几个辅码字母。
    pub(super) fn trailing_aux_len(&self, start: usize, word: &str) -> u8 {
        self.tail_aux
            .iter()
            .find(|(s, w, _)| *s == start && w == word)
            .map_or(0, |t| t.2)
    }
}

/// [`forward_reach`] 的解码节点版。
fn word_reach(nodes: &[Vec<WordNode>]) -> Vec<bool> {
    let mut r = vec![false; nodes.len()];
    r[0] = true;
    for e in 1..nodes.len() {
        r[e] = nodes[e].iter().any(|n| n.start < e && r[n.start]);
    }
    r
}

/// 经节点从 0 前向可达的位置。
fn forward_reach(nodes: &[Vec<LatticeNode>]) -> Vec<bool> {
    let mut r = vec![false; nodes.len()];
    r[0] = true;
    for e in 1..nodes.len() {
        r[e] = nodes[e].iter().any(|n| n.start < e && r[n.start]);
    }
    r
}

impl PinyinEngine {
    /// 开关打开时的底图：击键坐标建图、补用户词、滤屏蔽词，转成解码用的节点。
    /// 一个系统词节点都没有时为 `None`（step 2 与 2c 都跳过）。
    ///
    /// `edges` 恒为 `pair_edges`（定末槽）；`aux` 为 `Some` 时建图改用其中的全偏移键对并接辅码边。
    pub(super) fn raw_base_lattice(
        &self,
        keys: &str,
        edges: &[RawEdge],
        aux: Option<AuxInput>,
    ) -> Option<RawBase> {
        let _base = prof::span(Stage::Base);
        let fuzzy = Some(&self.fuzzy_config);
        let ctx = aux.map(|a| AuxCtx::new(a.lookup));
        let graph_edges = aux.map_or(edges, |a| a.edges_all);
        let (mut nodes, mut aux_spans) = self.lattice_builder.build_raw_aux(
            keys,
            graph_edges,
            &self.dict,
            fuzzy,
            true,
            ctx.as_ref(),
        );
        let reach = nodes.iter().rposition(|v| !v.is_empty())?;
        let node_reach = ctx.as_ref().map(|_| forward_reach(&nodes));
        let mut partial_hits = None;
        // 辅码模式：末键落单、而经辅码读法能到整串时，补多字残码补全节点与之在 `nodes[n]` 比分
        // （到不了整串时底图同 P1）。可达表在补之前算，step 2 / 2c 的终点判据不变。
        if let Some(a) = aux
            && let Some(partial) = a.tail_partial
            && let Some(r) = &node_reach
            && r[keys.len()]
        {
            let _t = prof::span(Stage::TailCompletion);
            self.lattice_builder.add_tail_completion_raw(
                keys,
                a.edges_all,
                partial,
                r,
                &self.dict,
                &mut nodes,
                &mut partial_hits,
            );
        }
        if self.config.sentence_uses_user_words
            && let Some(store) = &self.store_layers
        {
            let _t = prof::span(Stage::Store);
            self.lattice_builder.add_store_nodes_raw_aux(
                keys,
                graph_edges,
                store,
                fuzzy,
                true,
                ctx.as_ref(),
                &mut nodes,
                &mut aux_spans,
            );
        }
        let _t = prof::span(Stage::ToWordNodes);
        self.drop_blocked_nodes(&mut nodes);
        // 辅码模式才有：单链时 `aux_spans` 为空。
        let tail_aux: Vec<(usize, String, u8)> = nodes[keys.len()]
            .iter()
            .filter(|_| aux.is_some())
            .filter_map(|n| {
                let last = n.word.chars().count().checked_sub(1)?;
                let len = aux_spans
                    .get(n.start, n.end, &n.word)?
                    .iter()
                    .find(|(i, _)| *i == last)?
                    .1
                    .len() as u8;
                Some((n.start, n.word.clone(), len))
            })
            .collect();
        Some(RawBase {
            lattice: to_word_nodes(nodes, &aux_spans),
            reach,
            last_slot_end: last_slot_end(keys, edges),
            node_reach,
            tail_aux,
            partial_hits,
            aux_spans,
        })
    }

    /// 2c 的整句图：只在键对恰好铺到末键之前时，在底图末端补残码节点，终点为整串；否则 `None`。
    /// 另返回底图的辅码分布（见 [`RawBase::aux_spans`]）。
    ///
    /// 辅码模式下末端可能已有「音节 + 辅码」节点，判据改为末键前一位可达；残码节点与已有节点
    /// 同词同起点时取分高者。整串另可达（[`RawBase::tail_end`]）时那条整句由 step 2 出，这里
    /// 去掉带辅码的节点与 `lattice[n]` 上的辅码读法、补全节点，只解 P1 的残码整句。
    pub(super) fn raw_partial_lattice(
        &self,
        keys: &str,
        base: RawBase,
        partial: &str,
    ) -> Option<(Vec<Vec<WordNode>>, usize, AuxSpans)> {
        let plain_tail = base.tail_end().is_some();
        let RawBase {
            mut lattice,
            reach,
            last_slot_end,
            node_reach,
            mut partial_hits,
            aux_spans,
            ..
        } = base;
        let end = keys.len();
        if end == 0 {
            return None;
        }
        if plain_tail {
            lattice[end].clear();
            for v in &mut lattice {
                v.retain(|w| w.aux == 0);
            }
        }
        // 去掉辅码后的可达性要重算：末槽须恰在末键之前、且不经辅码可达（同 P1），否则白解一次。
        let start = match &node_reach {
            None if reach + 1 == end => reach,
            Some(_) if plain_tail => (last_slot_end == Some(end - 1)
                && word_reach(&lattice)[end - 1])
                .then_some(end - 1)?,
            Some(r) if r[end - 1] => end - 1,
            _ => return None,
        };
        let mut tail: Vec<Vec<LatticeNode>> = vec![Vec::new(); end + 1];
        self.lattice_builder.add_partial_final_raw_cached(
            keys,
            start,
            partial,
            &self.dict,
            &mut tail,
            &mut partial_hits,
        );
        self.drop_blocked_nodes(&mut tail);
        let extra = to_word_nodes(tail, &AuxSpans::default()).swap_remove(end);
        for n in extra {
            match lattice[end]
                .iter_mut()
                .find(|x| x.start == n.start && x.word == n.word)
            {
                Some(x) if n.log_prob > x.log_prob => *x = n,
                Some(_) => {}
                None => lattice[end].push(n),
            }
        }
        Some((lattice, end, aux_spans))
    }
}

fn to_word_nodes(nodes: Vec<Vec<LatticeNode>>, aux: &AuxSpans) -> Vec<Vec<WordNode>> {
    nodes
        .into_iter()
        .map(|at_end| {
            at_end
                .into_iter()
                .map(|n| WordNode {
                    aux: aux.letters(n.start, n.end, &n.word),
                    start: n.start,
                    end: n.end,
                    word: n.word,
                    syl_mask: n.syl_mask,
                    log_prob: n.log_prob,
                    canon: n.canon,
                })
                .collect()
        })
        .collect()
}

/// 击键图整句的造词码。节点都带 `canon`，`learn_code` 恒有值；与候选所打的全拼码、边界相同时
/// 记 `None`，与旧路径「全精确不另带」同口径。`keys` 是建图用的击键串。
pub(super) fn raw_learn_code(
    result: &ViterbiResult,
    keys: &str,
    code: &str,
    boundary: u64,
) -> Option<(String, u64)> {
    result
        .learn_code(keys)
        .filter(|(c, b)| !(c == code && *b == boundary))
}

/// 击键图整句沿路径读出的形态（见 [`aux_path`]）。
pub(super) struct AuxPath {
    /// 路径实际经过的音节（去辅码）的所打全拼码：各键对取首读音，末键落单取全拼残码
    pub code: String,
    /// `code` 的音节边界；超 64 字节为 0
    pub boundary: u64,
    /// 造词码（各节点规范码）；与 `code` / `boundary` 相同时 `None`（同 [`raw_learn_code`]）
    pub learn_code: Option<(String, u64)>,
    /// 辅码 `(击键起点, 长度)`，按起点升序；空 = 路径没用辅码
    pub aux_keys: Vec<(u16, u8)>,
    /// 路径终点（击键偏移）
    pub end: usize,
    /// 各音节（含落单残码）的击键起点
    syllable_starts: Vec<usize>,
}

/// 按解码路径逐节点走击键：音节两键（前导 `'` 跳过），其后按 [`AuxSpans`] 登记的字序吃辅码，
/// 紧随的 `'` 并入；节点末尾只剩一键时是残码（2c 残码节点、多字残码补全）。
///
/// `edges_all` 是建图用的全偏移键对（取首读音），`tail_partial` 是末键的全拼残码。路径与登记
/// 对不上（不应发生）时 `None`，调用方照旧写候选。
pub(super) fn aux_path(
    result: &ViterbiResult,
    keys: &str,
    edges_all: &[RawEdge],
    spans: &AuxSpans,
    tail_partial: Option<&str>,
) -> Option<AuxPath> {
    let Some(pieces) = result.pieces.as_ref() else {
        debug_assert!(false, "击键图整句缺路径片段：{:?}", result.words);
        return None;
    };
    if pieces.len() != result.words.len() {
        debug_assert!(false, "路径片段与词数不符：{:?}", result.words);
        return None;
    }
    if keys.len() > usize::from(u16::MAX) {
        return None;
    }
    let b = keys.as_bytes();
    let edge_at = |pos: usize| {
        let i = edges_all.binary_search_by_key(&pos, |e| e.start).ok()?;
        let e = &edges_all[i];
        (e.end == pos + 2 && e.end <= b.len())
            .then(|| e.syllables.first().map(String::as_str))
            .flatten()
    };
    let mut out = AuxPath {
        code: String::new(),
        boundary: 0,
        learn_code: None,
        aux_keys: Vec::new(),
        end: 0,
        syllable_starts: Vec::new(),
    };
    let mut boundary_ok = true;
    let mut mark = |out: &mut AuxPath, pos: usize| {
        if out.code.len() >= 64 {
            boundary_ok = false;
        } else {
            out.boundary |= 1 << out.code.len();
        }
        out.syllable_starts.push(pos);
    };
    for (piece, word) in pieces.iter().zip(&result.words) {
        let aux = spans.get(piece.start, piece.end, word).unwrap_or(&[]);
        let mut used = 0;
        let mut pos = piece.start;
        let mut k = 0;
        loop {
            while pos < piece.end && b[pos] == b'\'' {
                pos += 1;
            }
            if pos >= piece.end {
                break;
            }
            if pos + 2 <= piece.end
                && let Some(syl) = edge_at(pos)
            {
                mark(&mut out, pos);
                out.code.push_str(syl);
                pos += 2;
                if let Some((_, prefix)) = aux.iter().find(|(i, _)| *i == k) {
                    out.aux_keys.push((pos as u16, prefix.len() as u8));
                    pos += prefix.len();
                    used += 1;
                }
                k += 1;
            } else if pos + 1 == piece.end {
                mark(&mut out, pos);
                match tail_partial.filter(|_| pos + 1 == b.len()) {
                    Some(p) => out.code.push_str(p),
                    None => out.code.push(char::from(b[pos])),
                }
                pos += 1;
            } else {
                debug_assert!(
                    false,
                    "{keys} 的节点 {piece:?}（{word}）在 {pos} 处读不出音节"
                );
                return None;
            }
        }
        if used != aux.len() || pos != piece.end {
            debug_assert!(
                false,
                "{keys} 的节点 {piece:?}（{word}）与辅码登记 {aux:?} 对不上"
            );
            return None;
        }
        out.end = piece.end;
    }
    if !boundary_ok {
        out.boundary = 0;
    }
    out.learn_code = raw_learn_code(result, keys, &out.code, out.boundary);
    Some(out)
}

impl AuxPath {
    /// 把路径的身份写到候选上：消费数、码与边界、造词码、辅码位置、编码栏，五者覆盖同一段击键。
    /// 同文合并时整套替换（已有候选只留词库来源标记与权重），不留半截旧身份。
    pub(super) fn stamp(&self, c: &mut wind_candidate::Candidate, keys: &str, sep: &str) {
        c.consumed_length = self.end;
        c.code.clone_from(&self.code);
        c.boundary = self.boundary;
        c.meta.learn_code.clone_from(&self.learn_code);
        c.meta.aux_keys = Some(self.aux_keys.as_slice().into());
        c.meta.preedit_split = Some(self.preedit(keys, sep).into());
    }

    /// 编码栏形态：缓冲按序插入分隔符——音节之间 `'`，辅码前 `sep`；路径之后的剩余缓冲按两键
    /// 一段接着切。手打的 `'` 原样保留、旁边不再插（同 `build_raw_preedit`）。
    pub(super) fn preedit(&self, keys: &str, sep: &str) -> String {
        let b = keys.as_bytes();
        let mut starts = self.syllable_starts.clone();
        let mut letters = 0usize;
        for (i, &c) in b.iter().enumerate().skip(self.end) {
            if c == b'\'' {
                letters = 0;
                continue;
            }
            if letters.is_multiple_of(2) {
                starts.push(i);
            }
            letters += 1;
        }
        let mut out = String::with_capacity(keys.len() + starts.len() + self.aux_keys.len());
        for (i, &c) in b.iter().enumerate() {
            if c == b'\'' {
                out.push('\'');
                continue;
            }
            if self.aux_keys.iter().any(|&(s, _)| usize::from(s) == i) {
                out.push_str(sep);
            } else if starts.contains(&i) && !out.is_empty() && !out.ends_with('\'') {
                out.push('\'');
            }
            out.push(char::from(c));
        }
        out
    }
}

/// 缓存的命中还要给别的终点用，只能拷文本；不缓存或已是最后一次用时移走。
fn take_text(hit: &mut DictHit, cached: bool) -> String {
    if cached {
        hit.text.clone()
    } else {
        std::mem::take(&mut hit.text)
    }
}

fn raw_node(
    p: usize,
    q: usize,
    word: String,
    code: &str,
    shape: &Shape,
    log_prob: f64,
) -> LatticeNode {
    LatticeNode {
        start: p,
        end: q,
        word,
        syllables: shape.syllables.to_vec(),
        syl_mask: shape.mask,
        log_prob,
        canon: Some((code.to_string(), shape.mask)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::PinyinEngine;
    use super::super::lattice::{LatticeBuilder, LatticeNode};
    use super::super::shuangpin::{Layout, ShuangpinConverter};
    use crate::engine::Engine;
    use std::collections::HashSet;
    use std::sync::Arc;
    use wind_dict::DictManager;
    use wind_dict::cached::CachedDict;
    use wind_dict::codetable::CodetableDict;
    use wind_store::Store;

    /// 闸门裕度护栏：没挂模型走模型关那档；挂了按权重缩放，权重 0.5 时仍是标定值 10，权重调高
    /// 裕度随之放大（转移分整体放大）；模型不报权重时按 0.5。
    #[test]
    fn gate_margin_guard() {
        use super::super::viterbi::ViterbiDecoder;
        assert_eq!(super::default_gate_margin(None), 5.0);
        assert_eq!(super::default_gate_margin(Some(Some(0.5))), 10.0);
        assert_eq!(super::default_gate_margin(Some(None)), 10.0);
        assert_eq!(super::default_gate_margin(Some(Some(0.0))), 5.0);
        assert!((super::default_gate_margin(Some(Some(0.8))) - 13.0).abs() < 1e-9);
        assert!((super::default_gate_margin(Some(Some(0.3))) - 8.0).abs() < 1e-9);
        // 引擎从解码器读：没挂模型 = None，挂了不报权重的 = Some(None)。
        assert_eq!(ViterbiDecoder::new().grammar_weight(), None);
        let null = ViterbiDecoder::with_grammar(Arc::new(super::super::grammar::NullGrammar));
        assert_eq!(null.grammar_weight(), Some(None));
    }

    fn xiaohe() -> ShuangpinConverter {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin/xiaohe.toml");
        ShuangpinConverter::new(Layout::from_toml(&p).expect("小鹤布局"))
    }

    fn dict(words: &[(&str, &str, i32)]) -> CachedDict {
        let mut raw = CodetableDict::empty();
        for (i, (code, text, w)) in words.iter().enumerate() {
            raw.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        CachedDict::Memory(raw)
    }

    fn find<'a>(nodes: &'a [Vec<LatticeNode>], word: &str) -> Option<&'a LatticeNode> {
        nodes.iter().flatten().find(|n| n.word == word)
    }

    const WORDS: &[(&str, &str, i32)] = &[
        ("ni", "你", 9000),
        ("hao", "好", 9000),
        ("nihao", "你好", 50000),
        ("ma", "吗", 8000),
    ];

    /// 节点跨度是击键偏移，音节掩码与规范码在全拼域。
    #[test]
    fn raw_nodes_cover_keystroke_span() {
        let conv = xiaohe();
        let keys = "nihc";
        let edges = conv.pair_edges(keys);
        let b = LatticeBuilder::new();
        let nodes = b.build_raw(keys, &edges, &dict(WORDS), None, true);
        assert_eq!(nodes.len(), keys.len() + 1);
        let n = find(&nodes, "你好").expect("你好");
        assert_eq!((n.start, n.end), (0, 4));
        assert_eq!(n.syl_mask, 0b101, "ni|hao 全拼域");
        assert_eq!(n.syllables, vec!["ni", "hao"]);
        assert_eq!(n.canon, Some(("nihao".to_string(), 0b101)));
        let h = find(&nodes, "好").unwrap();
        assert_eq!((h.start, h.end), (2, 4));
    }

    /// `'` 不断词，归到前一节点的末尾，图上不留空洞。
    #[test]
    fn raw_nodes_absorb_separator() {
        let conv = xiaohe();
        let keys = "ni'hc";
        let edges = conv.pair_edges(keys);
        let nodes = LatticeBuilder::new().build_raw(keys, &edges, &dict(WORDS), None, true);
        let n = find(&nodes, "你好").unwrap();
        assert_eq!((n.start, n.end), (0, 5));
        let ni = find(&nodes, "你").unwrap();
        assert_eq!((ni.start, ni.end), (0, 3));
        let h = find(&nodes, "好").unwrap();
        assert_eq!((h.start, h.end), (3, 5));
    }

    /// 用户词按全拼码查进图；屏蔽词由调用方的 `drop_blocked_nodes` 统一滤掉。
    #[test]
    fn raw_nodes_include_user_and_blocked_filtered() {
        // 文件名带进程号，并发跑测试互不踩；用完删掉。
        let p = std::env::temp_dir().join(format!(
            "wind_pinyin_raw_lattice_user_{}.redb",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        let store = Arc::new(Store::open(&p).unwrap());
        store
            .add_user_word("pinyin", "gailun", "盖伦", 500, 0b1001)
            .unwrap();
        // 无边界的用户词不进整句图（同 add_store_nodes 约束 2）。
        store
            .add_user_word("pinyin", "gailun", "该轮", 500, 0)
            .unwrap();
        let dm = DictManager::new();
        dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
            store.clone(),
            "pinyin",
        )));

        let conv = xiaohe();
        // 小鹤：gd = gai，ly = lun
        let keys = "gdly";
        let edges = conv.pair_edges(keys);
        let words = &[
            ("gai", "该", 9000),
            ("lun", "论", 9000),
            ("gailun", "概论", 1217),
        ];
        let b = LatticeBuilder::new();
        let mut nodes = b.build_raw(keys, &edges, &dict(words), None, true);
        b.add_store_nodes_raw(keys, &edges, &dm, None, true, &mut nodes);
        let u = find(&nodes, "盖伦").expect("用户词进图");
        assert_eq!((u.start, u.end, u.syl_mask), (0, 4, 0b1001));
        assert!(find(&nodes, "该轮").is_none(), "无边界用户词不进图");

        assert!(find(&nodes, "概论").is_some(), "前提：系统词在图里");
        let eng = PinyinEngine::new(Default::default(), dict(&[]));
        eng.set_blocked_words(Arc::new(HashSet::from(["概论".to_string()])));
        eng.drop_blocked_nodes(&mut nodes);
        assert!(find(&nodes, "概论").is_none(), "屏蔽词不进图");
        assert!(find(&nodes, "盖伦").is_some());
        drop(dm);
        drop(store);
        let _ = std::fs::remove_file(&p);
    }

    /// 尾部落单键补出残码节点，跨度是那一个击键。
    #[test]
    fn raw_nodes_tail_key_partial() {
        let conv = xiaohe();
        let keys = "nihcm";
        let r = conv.convert(keys);
        let edges = conv.pair_edges(keys);
        let b = LatticeBuilder::new();
        let mut nodes = b.build_raw(keys, &edges, &dict(WORDS), None, true);
        let partial = r.partial_initial.expect("尾键残码");
        b.add_partial_final_raw(keys, 4, &partial, &dict(WORDS), &mut nodes);
        let m = find(&nodes, "吗").expect("残码节点");
        assert_eq!((m.start, m.end, m.syl_mask), (4, 5, 1));
        assert_eq!(m.canon, Some(("m".to_string(), 1)));
        // 小鹤 `u` 作声母是 sh：残码按全拼声母补。
        let keys = "nihcu";
        let mut nodes = b.build_raw(keys, &conv.pair_edges(keys), &dict(WORDS), None, true);
        let words = &[("shi", "是", 9000)];
        b.add_partial_final_raw(keys, 4, "sh", &dict(words), &mut nodes);
        assert!(find(&nodes, "是").is_some());
    }

    /// 整句终点须恰为末槽终点；末槽含紧随的 `'`。
    #[test]
    fn sentence_end_requires_last_slot() {
        let conv = xiaohe();
        assert_eq!(
            super::last_slot_end("nihcmd", &conv.pair_edges("nihcmd")),
            Some(6)
        );
        assert_eq!(
            super::last_slot_end("nihc'", &conv.pair_edges("nihc'")),
            Some(5)
        );
        assert_eq!(super::last_slot_end("", &[]), None);
        assert_eq!(super::sentence_end(6, Some(6)), Some(6));
        assert_eq!(super::sentence_end(4, Some(6)), None);
        assert_eq!(super::sentence_end(4, None), None);
    }

    /// 模糊音读音按处数扣分，所选读音写进规范码。
    #[test]
    fn raw_nodes_fuzzy_reading_penalized() {
        let mut conv = xiaohe();
        let fz = super::super::fuzzy::FuzzyConfig {
            zh_z: true,
            ..Default::default()
        };
        conv.set_fuzzy(&fz);
        // 小鹤 zs = zong；开 z=zh 时边上另有 zhong
        let keys = "zsgo";
        let edges = conv.pair_edges(keys);
        let words = &[("zhongguo", "中国", 90000), ("zongguo", "宗国", 90000)];
        let nodes = LatticeBuilder::new().build_raw(keys, &edges, &dict(words), Some(&fz), true);
        let exact = find(&nodes, "宗国").unwrap();
        let fuzzy = find(&nodes, "中国").unwrap();
        assert_eq!((fuzzy.start, fuzzy.end), (0, 4));
        assert_eq!(fuzzy.canon.as_ref().unwrap().0, "zhongguo");
        assert!(
            (exact.log_prob - fuzzy.log_prob - std::f64::consts::LN_2).abs() < 1e-9,
            "同权重时模糊那条恰多扣一处"
        );
    }

    // ===== 辅码边 =====

    struct FakeAux(Vec<(char, &'static str)>);

    impl crate::engine::AuxCharLookup for FakeAux {
        fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
            self.0
                .iter()
                .any(|(c, code)| *c == ch && code.starts_with(prefix))
        }
        fn is_empty(&self) -> bool {
            self.0.is_empty()
        }
    }

    /// 辅码模式建图：全偏移键对 + 辅码表。
    fn build_aux(
        keys: &str,
        words: &[(&str, &str, i32)],
        table: &FakeAux,
        fuzzy: Option<&super::super::fuzzy::FuzzyConfig>,
    ) -> (Vec<Vec<LatticeNode>>, super::AuxSpans) {
        let mut conv = xiaohe();
        if let Some(f) = fuzzy {
            conv.set_fuzzy(f);
        }
        let ctx = super::AuxCtx::new(table);
        LatticeBuilder::new().build_raw_aux(
            keys,
            &conv.pair_edges_all(keys),
            &dict(words),
            fuzzy,
            true,
            Some(&ctx),
        )
    }

    fn at<'a>(
        nodes: &'a [Vec<LatticeNode>],
        word: &str,
        span: (usize, usize),
    ) -> Option<&'a LatticeNode> {
        nodes
            .iter()
            .flatten()
            .find(|n| n.word == word && (n.start, n.end) == span)
    }

    fn aux_of(spans: &super::AuxSpans, word: &str, span: (usize, usize)) -> Vec<(usize, String)> {
        spans.get(span.0, span.1, word).unwrap_or_default().to_vec()
    }

    const SHI: &[(&str, &str, i32)] =
        &[("shi", "是", 9000), ("ni", "你", 9000), ("hao", "好", 9000)];

    /// 小鹤 `ui` = shi，`uu` 作「是」的辅码：「是」跨 `[0,4)`，规范码不含辅码。
    #[test]
    fn aux_edge_builds_char_with_aux() {
        let (nodes, spans) = build_aux("uiuu", SHI, &FakeAux(vec![('是', "uu")]), None);
        let n = at(&nodes, "是", (0, 4)).expect("带辅码的「是」");
        assert_eq!(n.canon, Some(("shi".to_string(), 1)));
        assert_eq!(n.syllables, vec!["shi"]);
        assert_eq!(n.syl_mask, 1);
        assert_eq!(aux_of(&spans, "是", (0, 4)), vec![(0, "uu".to_string())]);
        // 1 位前缀同样合法。
        assert!(at(&nodes, "是", (0, 3)).is_some());
        assert_eq!(aux_of(&spans, "是", (0, 3)), vec![(0, "u".to_string())]);
        assert!(at(&nodes, "是", (0, 2)).is_some(), "无辅码读法仍在");
        assert!(spans.get(0, 2, "是").is_none());

        // 不给辅码表：只有无辅码读法。
        let conv = xiaohe();
        let plain = LatticeBuilder::new().build_raw(
            "uiuu",
            &conv.pair_edges("uiuu"),
            &dict(SHI),
            None,
            true,
        );
        let spans: Vec<_> = plain
            .iter()
            .flatten()
            .filter(|n| n.word == "是")
            .map(|n| (n.start, n.end))
            .collect();
        assert_eq!(spans, vec![(0, 2)]);
    }

    /// 辅码字母不在该字任一码的开头上 → 不建该节点。
    #[test]
    fn aux_edge_requires_table_hit() {
        let table = FakeAux(vec![('是', "or")]);
        let (nodes, spans) = build_aux("uiuu", SHI, &table, None);
        assert!(at(&nodes, "是", (0, 3)).is_none());
        assert!(at(&nodes, "是", (0, 4)).is_none());
        let (nodes, spans2) = build_aux("uio", SHI, &table, None);
        assert!(at(&nodes, "是", (0, 3)).is_some(), "前缀 o 命中 or");
        assert_eq!(aux_of(&spans2, "是", (0, 3)), vec![(0, "o".to_string())]);
        // 多字词要求字数 = 音节数，且每处辅码都核对它所跟的那个字。
        let words = &[("nihao", "你好", 50000)];
        let table = FakeAux(vec![('你', "rx"), ('好', "nz")]);
        let (nodes, spans3) = build_aux("nirhc", words, &table, None);
        assert!(at(&nodes, "你好", (0, 5)).is_some(), "词中辅码");
        assert_eq!(aux_of(&spans3, "你好", (0, 5)), vec![(0, "r".to_string())]);
        let (nodes, _) = build_aux("ninhc", words, &table, None);
        assert!(at(&nodes, "你好", (0, 5)).is_none(), "n 不是「你」的辅码");
        assert!(spans.0.is_empty());
    }

    /// 辅码不跨 `'`：辅码须紧跟音节两键、同在一个 `'` 段内；辅码后的 `'` 并入辅码。
    #[test]
    fn aux_not_across_separator() {
        let words = &[("ni", "你", 9000), ("hao", "好", 9000), ("ao", "奥", 9000)];
        let table = FakeAux(vec![('你', "hc"), ('好', "ao")]);
        // `ni'h'ao`：h 与 ni 之间隔着 `'`，不当辅码；`ao` 前也有 `'`。
        let (nodes, spans) = build_aux("ni'h'ao", words, &table, None);
        assert!(spans.0.is_empty(), "不应有辅码节点");
        assert!(at(&nodes, "你", (0, 3)).is_some());
        assert!(nodes.iter().flatten().all(|n| n.word != "你" || n.end == 3));
        // `nih'ao`：h 紧跟 ni，是辅码，其后的 `'` 并入，「奥」接在 4 上。
        let (nodes, spans) = build_aux("nih'ao", words, &table, None);
        assert_eq!(aux_of(&spans, "你", (0, 4)), vec![(0, "h".to_string())]);
        assert!(at(&nodes, "奥", (4, 6)).is_some(), "辅码后的 ' 不留空洞");
        // 两位辅码不能夹 `'`。
        let (_, spans) = build_aux("nih'c", words, &table, None);
        assert!(spans.get(0, 5, "你").is_none());
        // 音节后紧跟 `'` 的不接辅码：`hc'ao` 的「好」不吃 ao。
        let (_, spans) = build_aux("hc'ao", words, &table, None);
        assert!(spans.0.is_empty());
    }

    /// 用户词、模糊读法与辅码并存时不丢节点。
    #[test]
    fn aux_with_user_word_and_fuzzy() {
        let p = std::env::temp_dir().join(format!(
            "wind_pinyin_raw_lattice_aux_user_{}.redb",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        let store = Arc::new(Store::open(&p).unwrap());
        store
            .add_user_word("pinyin", "gailun", "盖伦", 500, 0b1001)
            .unwrap();
        let dm = DictManager::new();
        dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
            store.clone(),
            "pinyin",
        )));
        let words = &[
            ("gai", "该", 9000),
            ("lun", "论", 9000),
            ("gailun", "概论", 1217),
        ];
        let table = FakeAux(vec![('伦', "ab"), ('论', "yz")]);
        let ctx = super::AuxCtx::new(&table);
        let conv = xiaohe();
        let keys = "gdlya";
        let edges = conv.pair_edges_all(keys);
        let b = LatticeBuilder::new();
        let (mut nodes, mut spans) =
            b.build_raw_aux(keys, &edges, &dict(words), None, true, Some(&ctx));
        b.add_store_nodes_raw_aux(
            keys,
            &edges,
            &dm,
            None,
            true,
            Some(&ctx),
            &mut nodes,
            &mut spans,
        );
        assert!(at(&nodes, "盖伦", (0, 5)).is_some(), "用户词带辅码");
        assert_eq!(aux_of(&spans, "盖伦", (0, 5)), vec![(1, "a".to_string())]);
        assert!(at(&nodes, "盖伦", (0, 4)).is_some(), "无辅码用户词仍在");
        assert!(at(&nodes, "概论", (0, 4)).is_some(), "系统词仍在");
        assert!(at(&nodes, "概论", (0, 5)).is_none(), "a 不是「论」的辅码");
        drop(dm);
        drop(store);
        let _ = std::fs::remove_file(&p);

        // 模糊：小鹤 zs = zong，开 z=zh 时另有 zhong；辅码打在词中字后。
        let fz = super::super::fuzzy::FuzzyConfig {
            zh_z: true,
            ..Default::default()
        };
        let words = &[("zhongguo", "中国", 90000), ("zongguo", "宗国", 90000)];
        let table = FakeAux(vec![('中', "xk"), ('国', "gw")]);
        let (nodes, spans) = build_aux("zsxgo", words, &table, Some(&fz));
        let n = at(&nodes, "中国", (0, 5)).expect("模糊 + 词中辅码");
        assert_eq!(n.canon.as_ref().unwrap().0, "zhongguo");
        assert_eq!(aux_of(&spans, "中国", (0, 5)), vec![(0, "x".to_string())]);
        assert!(at(&nodes, "宗国", (0, 5)).is_none(), "「宗」无辅码 x");
        let (nodes, _) = build_aux("zsgo", words, &table, Some(&fz));
        assert!(at(&nodes, "中国", (0, 4)).is_some() && at(&nodes, "宗国", (0, 4)).is_some());
    }

    /// 同词同跨度去重：辅码数不同时取分高者，平分留辅码少的；辅码数相同时同旧路径。
    #[test]
    fn aux_dedupe_prefers_plain_on_tie() {
        use super::should_replace;
        assert!(
            !should_replace((false, 0, -1.0), (false, 1, -1.0)),
            "平分留无辅码"
        );
        assert!(should_replace((false, 1, -1.0), (false, 0, -1.0)));
        assert!(
            should_replace((false, 0, -2.0), (false, 1, -1.0)),
            "辅码只在严格高分时胜"
        );
        assert!(!should_replace((false, 0, -1.0), (true, 1, -2.0)));
        assert!(
            should_replace((true, 0, -1.0), (false, 0, -5.0)),
            "同辅码数：精确恒胜"
        );
        assert!(!should_replace((false, 0, -5.0), (true, 0, -1.0)));
    }

    /// 选读法按罚分比：3 处模糊（≈2.08）轻于 2 位辅码（4.0），不按「处数 + 字母数」计数比。
    #[test]
    fn pick_shape_orders_by_penalty() {
        let shape = |edits, aux_len| super::Shape {
            mask: 0,
            syllables: Default::default(),
            edits,
            exact: false,
            aux: Default::default(),
            aux_len,
        };
        let shapes = [shape(0, 2), shape(3, 0)];
        let picked = super::pick_shape(&shapes, |_| true, "", None).unwrap();
        assert_eq!((picked.edits, picked.aux_len), (3, 0));
        // 同罚分时取辅码少的。
        let old = super::set_aux_letter_penalty(super::FUZZY_SYLLABLE_LOG_PENALTY);
        let shapes = [shape(0, 1), shape(1, 0)];
        let picked = super::pick_shape(&shapes, |_| true, "", None).unwrap();
        super::set_aux_letter_penalty(old);
        assert_eq!((picked.edits, picked.aux_len), (1, 0));
    }

    /// 辅码模式底图：引擎 + 全偏移键对 + 表。
    fn aux_base(keys: &str, eng: &PinyinEngine, table: &FakeAux) -> super::RawBase {
        let conv = xiaohe();
        let edges = conv.pair_edges(keys);
        let all = conv.pair_edges_all(keys);
        let partial = conv.convert(keys).partial_initial;
        let aux = super::AuxInput {
            edges_all: &all,
            lookup: table,
            tail_partial: partial.as_deref(),
        };
        eng.raw_base_lattice(keys, &edges, Some(aux)).expect("底图")
    }

    const NIHAO_H: &[(&str, &str, i32)] = &[
        ("ni", "你", 9000),
        ("hao", "好", 9000),
        ("nihao", "你好", 50000),
        ("he", "和", 8000),
    ];

    /// 辅码模式 step 2 终点同 P1 取末槽终点；整串另经辅码可达时由 `tail_end` 给出整串。
    #[test]
    fn aux_sentence_end_is_last_slot_with_tail() {
        let eng = PinyinEngine::new(Default::default(), dict(NIHAO_H));
        let table = FakeAux(vec![('好', "hn")]);
        let ends = |keys: &str| {
            let b = aux_base(keys, &eng, &table);
            (b.sentence().map(|(_, e)| e), b.tail_end())
        };
        // m 不是「好」的辅码，5 不可达：只有末槽 `hc` 的终点 4。
        assert_eq!(ends("nihcm"), (Some(4), None));
        // 偶数长：`pl` 不成音节，6 不可达。
        assert_eq!(ends("nihcpl"), (Some(4), None));
        // 整串经辅码可达：step 2 仍取末槽，整串另给（奇 / 偶）。
        assert_eq!(ends("nihch"), (Some(4), Some(5)));
        // `hn`（hiao）不成音节，末槽仍是 `hc`。
        assert_eq!(ends("nihchn"), (Some(4), Some(6)));
    }

    /// 奇数长 2c：整串另可达时那条整句归 step 2，2c 的图只剩无辅码节点与残码节点（同 P1）。
    #[test]
    fn aux_partial_lattice_is_plain_beside_tail() {
        let eng = PinyinEngine::new(Default::default(), dict(NIHAO_H));
        let table = FakeAux(vec![('好', "hn")]);
        let keys = "nihch";
        let partial = xiaohe().convert(keys).partial_initial.expect("尾键残码");
        assert_eq!(partial, "h");
        let base = aux_base(keys, &eng, &table);
        let (lattice, end, _) = eng
            .raw_partial_lattice(keys, base, &partial)
            .expect("2c 底图");
        assert_eq!(end, 5);
        let at_end: Vec<(usize, &str, u8)> = lattice[5]
            .iter()
            .map(|n| (n.start, n.word.as_str(), n.aux))
            .collect();
        assert!(
            !at_end.is_empty() && at_end.iter().all(|&(st, _, aux)| st == 4 && aux == 0),
            "只剩残码节点：{at_end:?}"
        );
        assert!(lattice.iter().flatten().all(|n| n.aux == 0));
    }

    /// 同起点同终点同词只留一个节点，辅码分布随它登记：`nihhhx` 的「你行」可读作「你(h)行 + x」
    /// 或「你 行(hx)」，图里只有一条，`trailing_aux_len` 与它登记的分布一致。
    #[test]
    fn trailing_aux_len_follows_kept_node() {
        let words: &[(&str, &str, i32)] = &[
            ("ni", "你", 9000),
            ("hang", "行", 9000),
            ("nihang", "你行", 50000),
        ];
        let eng = PinyinEngine::new(Default::default(), dict(words));
        let table = FakeAux(vec![('你', "h"), ('行', "hx"), ('行', "x")]);
        let keys = "nihhhx";
        let base = aux_base(keys, &eng, &table);
        let at_end: Vec<_> = base.lattice[6]
            .iter()
            .filter(|n| n.start == 0 && n.word == "你行")
            .collect();
        assert_eq!(at_end.len(), 1, "{at_end:?}");
        assert_eq!(at_end[0].aux, 2);
        let conv = xiaohe();
        let ctx = super::AuxCtx::new(&table);
        let (_, spans) = eng.lattice_builder.build_raw_aux(
            keys,
            &conv.pair_edges_all(keys),
            &eng.dict,
            None,
            true,
            Some(&ctx),
        );
        let dist = spans.get(0, 6, "你行").expect("登记了辅码").to_vec();
        let last = dist
            .iter()
            .find(|(i, _)| *i == 1)
            .map_or(0, |(_, p)| p.len());
        assert!(
            dist == vec![(0, "h".to_string()), (1, "x".to_string())]
                || dist == vec![(1, "hx".to_string())],
            "{dist:?}"
        );
        assert_eq!(base.trailing_aux_len(0, "你行") as usize, last, "{dist:?}");
    }

    const QIHOU: &[(&str, &str, i32)] = &[
        ("qi", "启", 9000),
        ("qi", "气", 9000),
        ("qihou", "气候", 50000),
        ("he", "和", 8000),
        ("wo", "我", 9000),
    ];

    /// 末键落单：多字残码补全节点与「音节 + 1 位辅码」节点同在 `lattice[n]`。
    #[test]
    fn tail_completion_alongside_aux_nodes() {
        let eng = PinyinEngine::new(Default::default(), dict(QIHOU)).with_shuangpin(xiaohe());
        let table = FakeAux(vec![('启', "h")]);
        let base = aux_base("qih", &eng, &table);
        let at_end: Vec<(usize, &str, u8)> = base.lattice[3]
            .iter()
            .map(|n| (n.start, n.word.as_str(), n.aux))
            .collect();
        assert!(at_end.contains(&(0, "启", 1)), "音节 + 辅码：{at_end:?}");
        assert!(at_end.contains(&(0, "气候", 0)), "补全节点：{at_end:?}");
        let n = base.lattice[3].iter().find(|n| n.word == "气候").unwrap();
        assert_eq!(n.canon, Some(("qihou".to_string(), 0b101)));
        // 补全不改终点判据：step 2 取末槽 2，整串仍由辅码读法可达。
        assert_eq!(base.sentence().map(|(_, e)| e), Some(2));
        assert_eq!(base.tail_end(), Some(3));
        // 起点逐个往前退：`woqih` 从 2 起补出「气候」，从 0 起要 3 字词（没有）。
        let base = aux_base("woqih", &eng, &table);
        let starts: Vec<(usize, &str)> = base.lattice[5]
            .iter()
            .filter(|n| n.aux == 0 && n.end - n.start > 1)
            .map(|n| (n.start, n.word.as_str()))
            .collect();
        assert_eq!(starts, vec![(2, "气候")]);
    }

    /// 只在「经辅码读法到得了整串、且按键对真值切分末键落单」时补：到不了整串时不补；
    /// 辅码把奇偶错开才落单的末键（真值切分是偶数长）不补。
    #[test]
    fn tail_completion_only_beside_trailing_aux() {
        let eng = PinyinEngine::new(Default::default(), dict(QIHOU)).with_shuangpin(xiaohe());
        let multi = |base: &super::RawBase, n: usize| -> Vec<String> {
            base.lattice[n]
                .iter()
                .filter(|x| x.aux == 0 && x.word.chars().count() > 1)
                .map(|x| x.word.clone())
                .collect()
        };
        let base = aux_base("woqih", &eng, &FakeAux(vec![('我', "x")]));
        assert!(multi(&base, 5).is_empty(), "整串不可达");
        let table = FakeAux(vec![('我', "h"), ('启', "h")]);
        let base = aux_base("wohqih", &eng, &table);
        assert!(
            base.lattice[6].iter().any(|x| x.word == "启" && x.aux == 1),
            "前提：辅码读法到整串"
        );
        assert!(multi(&base, 6).is_empty(), "{:?}", multi(&base, 6));
    }

    /// 补全节点只在辅码模式下进图：不给表时底图与 P1 相同。
    #[test]
    fn tail_completion_absent_without_aux() {
        let eng = PinyinEngine::new(Default::default(), dict(QIHOU)).with_shuangpin(xiaohe());
        let conv = xiaohe();
        let base = eng
            .raw_base_lattice("woqih", &conv.pair_edges("woqih"), None)
            .expect("底图");
        assert!(base.lattice[5].is_empty(), "{:?}", base.lattice[5]);
    }

    /// 2c 补残码节点时与 `lattice[n]` 上已有的同起点同词节点去重：新节点分高才替换。
    #[test]
    fn partial_lattice_dedupes_residual_by_score() {
        let eng = PinyinEngine::new(Default::default(), dict(WORDS));
        let node = |log_prob| crate::pinyin::viterbi::WordNode {
            start: 4,
            end: 5,
            word: "吗".to_string(),
            syl_mask: 1,
            log_prob,
            canon: Some(("m".to_string(), 1)),
            aux: 0,
        };
        let base = |lp| {
            let mut lattice = vec![Vec::new(); 6];
            lattice[5].push(node(lp));
            super::RawBase {
                lattice,
                reach: 4,
                // 没有末槽 ⇒ 无 `tail_end`，`lattice[n]` 不清空，走去重这一支。
                last_slot_end: None,
                node_reach: Some(vec![true; 6]),
                tail_aux: Vec::new(),
                partial_hits: None,
                aux_spans: Default::default(),
            }
        };
        let ma = |lattice: &[Vec<crate::pinyin::viterbi::WordNode>]| -> Vec<f64> {
            lattice[5]
                .iter()
                .filter(|n| n.word == "吗" && n.start == 4)
                .map(|n| n.log_prob)
                .collect()
        };
        let (low, ..) = eng.raw_partial_lattice("nihcm", base(-1e9), "m").unwrap();
        let fresh = ma(&low);
        assert_eq!(fresh.len(), 1, "同起点同词只留一条");
        assert!(fresh[0] > -1e9, "分低的旧节点被替换：{fresh:?}");
        let (high, ..) = eng.raw_partial_lattice("nihcm", base(1e9), "m").unwrap();
        assert_eq!(ma(&high), vec![1e9], "分高的旧节点保留");
    }

    /// 一个词带多处辅码：每处核对它所跟的那个字。
    #[test]
    fn aux_multiple_spans_in_one_word() {
        let words = &[("nihaoma", "你好吗", 50000)];
        let table = FakeAux(vec![('你', "rx"), ('吗', "xy")]);
        let (nodes, spans) = build_aux("nirhcmax", words, &table, None);
        assert!(at(&nodes, "你好吗", (0, 8)).is_some());
        assert_eq!(
            aux_of(&spans, "你好吗", (0, 8)),
            vec![(0, "r".to_string()), (2, "x".to_string())]
        );
        let (nodes, spans) = build_aux("nirxhcmaxy", words, &table, None);
        assert_eq!(
            aux_of(&spans, "你好吗", (0, 10)),
            vec![(0, "rx".to_string()), (2, "xy".to_string())]
        );
        assert!(at(&nodes, "你好吗", (0, 10)).is_some());
        // 第二处不合法（z 不是「吗」的辅码）→ 整个节点不成立。
        let (nodes, _) = build_aux("nirhcmaz", words, &table, None);
        assert!(at(&nodes, "你好吗", (0, 8)).is_none());
        // 辅码打在「好」后：表里没有「好」→ 不成立。
        let (nodes, _) = build_aux("nihcrma", words, &table, None);
        assert!(at(&nodes, "你好吗", (0, 7)).is_none());
    }

    /// 批量判定（首字母掩码）与逐字判定产出的节点与辅码位置逐项相同：P2 的小词库夹具 + 多处辅码。
    #[test]
    fn aux_admits_batch_equals_per_char() {
        let fz = super::super::fuzzy::FuzzyConfig {
            zh_z: true,
            n_l: true,
            ..Default::default()
        };
        let multi: &[(&str, &str, i32)] = &[
            ("nihaoma", "你好吗", 50000),
            ("ni", "你", 9000),
            ("ni", "泥", 3000),
            ("hao", "好", 9000),
            ("ma", "吗", 8000),
            ("ma", "妈", 7000),
        ];
        let table = FakeAux(vec![
            ('是', "uu"),
            ('是', "ri"),
            ('你', "rx"),
            ('泥', "sh"),
            ('吗', "xy"),
            ('妈', "nm"),
            ('好', "nz"),
            ('好', "zn"),
            ('气', "qr"),
            ('启', "hu"),
            ('候', "rh"),
            ('和', "hk"),
        ]);
        type Case = (&'static str, &'static [(&'static str, &'static str, i32)]);
        let cases: &[Case] = &[
            ("uiuu", SHI),
            ("uiu", SHI),
            ("uio", SHI),
            ("nihcz", NIHAO_H),
            ("nizhcn", NIHAO_H),
            ("nirhcmax", multi),
            ("nirxhcmaxy", multi),
            ("nirhcmaz", multi),
            ("nishcnmamn", multi),
            ("uir", SHI),
            ("qiqhz", QIHOU),
            ("qihzr", QIHOU),
            ("qiqrhzrh", QIHOU),
            ("qihhz", QIHOU),
        ];
        for &(keys, words) in cases {
            for fuzzy in [None, Some(&fz)] {
                let mut conv = xiaohe();
                if let Some(f) = fuzzy {
                    conv.set_fuzzy(f);
                }
                let edges = conv.pair_edges_all(keys);
                let d = dict(words);
                let run = |per_char: bool| {
                    let ctx = super::AuxCtx::new(&table).with_per_char(per_char);
                    LatticeBuilder::new().build_raw_aux(keys, &edges, &d, fuzzy, true, Some(&ctx))
                };
                let (want, want_spans) = run(true);
                let (got, got_spans) = run(false);
                assert_eq!(format!("{got:?}"), format!("{want:?}"), "{keys} {fuzzy:?}");
                for n in want.iter().flatten() {
                    assert_eq!(
                        aux_of(&got_spans, &n.word, (n.start, n.end)),
                        aux_of(&want_spans, &n.word, (n.start, n.end)),
                        "{keys} {}",
                        n.word
                    );
                }
            }
        }
    }

    /// 无辅码表时节点与 P1 单链逐项相同（`p1_reference` 是改造前的实现）。
    #[test]
    fn aux_none_equals_p1() {
        let fz = super::super::fuzzy::FuzzyConfig {
            zh_z: true,
            n_l: true,
            ..Default::default()
        };
        let words: &[(&str, &str, i32)] = &[
            ("ni", "你", 9000),
            ("li", "里", 8000),
            ("hao", "好", 9000),
            ("nihao", "你好", 50000),
            ("lihao", "里好", 100),
            ("ma", "吗", 8000),
            ("shi", "是", 9000),
            ("shu", "书", 9000),
            ("zhongguo", "中国", 90000),
            ("zongguo", "宗国", 90000),
            ("guo", "国", 9000),
            ("zhong", "中", 9000),
        ];
        let d = dict(words);
        let b = LatticeBuilder::new();
        for fuzzy in [None, Some(&fz)] {
            let mut conv = xiaohe();
            if let Some(f) = fuzzy {
                conv.set_fuzzy(f);
            }
            for keys in [
                "nihc", "ni'hc", "nihcm", "nihcma", "uiuu", "zsgo", "nihc'", "'nihc", "nipl",
                "plnihc", "nihcuiuu",
            ] {
                let edges = conv.pair_edges(keys);
                let (new, spans) = b.build_raw_aux(keys, &edges, &d, fuzzy, true, None);
                let old = p1_reference::build_raw(&b, keys, &edges, &d, fuzzy, true);
                assert!(spans.0.is_empty());
                assert_eq!(
                    fmt_nodes(&new),
                    fmt_nodes(&old),
                    "{keys} fuzzy={}",
                    fuzzy.is_some()
                );
                assert!(
                    !fmt_nodes(&new).is_empty() || keys == "nipl" || keys == "plnihc",
                    "{keys}"
                );
            }
        }
    }

    fn fmt_nodes(nodes: &[Vec<LatticeNode>]) -> Vec<String> {
        nodes
            .iter()
            .enumerate()
            .flat_map(|(q, v)| {
                v.iter().map(move |n| {
                    format!(
                        "{q}:{}-{} {} {:?} {:b} {} {:?}",
                        n.start,
                        n.end,
                        n.word,
                        n.syllables,
                        n.syl_mask,
                        n.log_prob.to_bits(),
                        n.canon
                    )
                })
            })
            .collect()
    }

    /// 改造前（单条槽链）的 `build_raw`，只作 `aux_none_equals_p1` 的对照。
    mod p1_reference {
        use super::super::super::fuzzy::{FuzzyConfig, FuzzyMatcher};
        use super::super::super::lattice::{
            FUZZY_LAYER_BUDGET, FUZZY_SYLLABLE_LOG_PENALTY, LatticeBuilder, LatticeNode, score_node,
        };
        use super::super::super::shuangpin::RawEdge;
        use super::super::{is_slot_edge, slot_span};
        use std::collections::{BTreeMap, HashMap};
        use wind_dict::cached::CachedDict;

        struct Slot {
            start: usize,
            end: usize,
            options: Vec<(String, usize)>,
        }
        struct Shape {
            mask: u64,
            syllables: Vec<String>,
            edits: usize,
            exact: bool,
        }
        type Spans = BTreeMap<(usize, String), Vec<Shape>>;
        struct RawChain {
            slots: Vec<Slot>,
            linked: Vec<bool>,
            reachable: Vec<bool>,
        }

        impl RawChain {
            fn new(keys: &str, edges: &[RawEdge], fuzzy: Option<&FuzzyConfig>) -> Self {
                let b = keys.as_bytes();
                let fuzzy = fuzzy.filter(|f| f.any_enabled());
                let slots: Vec<Slot> = edges
                    .iter()
                    .filter(|e| is_slot_edge(b, e))
                    .map(|e| {
                        let (start, end) = slot_span(b, e);
                        let first = &e.syllables[0];
                        let mut options = vec![(first.clone(), 0)];
                        if let Some(f) = fuzzy {
                            options.extend(FuzzyMatcher::fuzzy_variants_scored(first, f));
                        }
                        Slot {
                            start,
                            end,
                            options,
                        }
                    })
                    .collect();
                let linked: Vec<bool> = slots.windows(2).map(|w| w[0].end == w[1].start).collect();
                let mut reachable = Vec::with_capacity(slots.len());
                for i in 0..slots.len() {
                    reachable.push(if i == 0 {
                        slots[0].start == 0
                    } else {
                        reachable[i - 1] && linked[i - 1]
                    });
                }
                Self {
                    slots,
                    linked,
                    reachable,
                }
            }

            fn spans_from(&self, i: usize, max: usize, can_extend: &dyn Fn(&str) -> bool) -> Spans {
                type Combo = (String, u64, Vec<String>, usize, bool);
                let mut spans = Spans::new();
                let mut layer: Vec<Combo> = vec![(String::new(), 0, Vec::new(), 0, true)];
                for k in 0..max {
                    let j = i + k;
                    if j >= self.slots.len() || (k > 0 && !self.linked[j - 1]) {
                        break;
                    }
                    let mut next: Vec<Combo> = Vec::new();
                    for (code, mask, syls, edits, exact) in &layer {
                        if !code.is_empty() && !can_extend(code) {
                            continue;
                        }
                        let bit = if code.len() < 64 {
                            1u64 << code.len()
                        } else {
                            0
                        };
                        for (idx, (opt, opt_edits)) in self.slots[j].options.iter().enumerate() {
                            let mut s = syls.clone();
                            s.push(opt.clone());
                            next.push((
                                format!("{code}{opt}"),
                                mask | bit,
                                s,
                                edits + opt_edits,
                                *exact && idx == 0,
                            ));
                        }
                    }
                    if next.is_empty() {
                        break;
                    }
                    if next.len() > FUZZY_LAYER_BUDGET {
                        next.sort_by_key(|c| c.3);
                        next.truncate(FUZZY_LAYER_BUDGET);
                    }
                    for (code, mask, syls, edits, exact) in &next {
                        spans.entry((j, code.clone())).or_default().push(Shape {
                            mask: *mask,
                            syllables: syls.clone(),
                            edits: *edits,
                            exact: *exact,
                        });
                    }
                    layer = next;
                }
                spans
            }
        }

        fn node(p: usize, q: usize, word: String, code: &str, s: &Shape, lp: f64) -> LatticeNode {
            LatticeNode {
                start: p,
                end: q,
                word,
                syllables: s.syllables.clone(),
                syl_mask: s.mask,
                log_prob: lp,
                canon: Some((code.to_string(), s.mask)),
            }
        }

        pub(super) fn build_raw(
            b: &LatticeBuilder,
            keys: &str,
            edges: &[RawEdge],
            dict: &CachedDict,
            fuzzy_config: Option<&FuzzyConfig>,
            require_reachable: bool,
        ) -> Vec<Vec<LatticeNode>> {
            let mut nodes: Vec<Vec<LatticeNode>> = vec![Vec::new(); keys.len() + 1];
            let chain = RawChain::new(keys, edges, fuzzy_config);
            let mut seen: HashMap<String, (usize, bool)> = HashMap::new();
            for i in 0..chain.slots.len() {
                if require_reachable && !chain.reachable[i] {
                    continue;
                }
                let p = chain.slots[i].start;
                let spans = chain.spans_from(i, b.max_word_len(), &|c| dict.has_longer_code(c));
                let mut seen_j = None;
                for ((j, code), shapes) in &spans {
                    if seen_j != Some(*j) {
                        seen.clear();
                        seen_j = Some(*j);
                    }
                    let q = chain.slots[*j].end;
                    let exact = shapes.iter().find(|s| s.exact);
                    for hit in dict.search_with_boundary(code) {
                        let shape = match exact {
                            Some(s) if hit.boundary == 0 || hit.boundary == s.mask => s,
                            Some(_) => continue,
                            None => match shapes
                                .iter()
                                .filter(|s| hit.boundary == 0 || s.mask == hit.boundary)
                                .min_by_key(|s| s.edits)
                            {
                                Some(s) => s,
                                None => continue,
                            },
                        };
                        let log_prob = score_node(&hit.text, code, hit.weight)
                            - FUZZY_SYLLABLE_LOG_PENALTY * shape.edits as f64;
                        let is_fuzzy = !shape.exact;
                        if let Some((at, existing_fuzzy)) = seen.get_mut(hit.text.as_str()) {
                            let existing = &mut nodes[q][*at];
                            let replace = match (*existing_fuzzy, is_fuzzy) {
                                (true, false) => true,
                                (false, true) => false,
                                _ => log_prob > existing.log_prob,
                            };
                            if replace {
                                *existing = node(p, q, hit.text, code, shape, log_prob);
                                *existing_fuzzy = is_fuzzy;
                            }
                            continue;
                        }
                        seen.insert(hit.text.clone(), (nodes[q].len(), is_fuzzy));
                        nodes[q].push(node(p, q, hit.text, code, shape, log_prob));
                    }
                }
            }
            nodes
        }
    }
}

/// λ 扫参工具（`#[ignore]`，不是门禁）：189 句语料上比 λ ∈ {1,2,3}。
///
/// ```text
/// cargo test -p wind-engine --release --lib lambda_sweep -- --ignored --nocapture --test-threads=1
/// ```
///
/// k=0：逐键前缀上引擎首选 `(文本, 消费数)` 与辅码表 `None` 不同的条数；k=1/2/3：在随机 k 个字后
/// 插该字首个辅码的 1～2 位（插法同 `spike/aux-joint` 的 `build_samples`），报引擎首选与界面首选
/// 等于目标句的条数。依赖 `build_dev` 的真实词库与小鹤辅码表。
#[cfg(test)]
mod lambda_sweep {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::super::dag::Dag;
    use super::super::shuangpin::{Layout, ShuangpinConverter};
    use super::super::syllable::SyllableTrie;
    use crate::engine::AuxCharLookup;
    use crate::{ConvertOptions, EngineManager};

    struct Table(HashMap<char, Vec<String>>);

    impl AuxCharLookup for Table {
        fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
            self.0
                .get(&ch)
                .is_some_and(|v| v.iter().any(|c| c.starts_with(prefix)))
        }
        fn is_empty(&self) -> bool {
            self.0.is_empty()
        }
    }

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    fn load_table(dir: &Path) -> Option<Table> {
        let src = std::fs::read_to_string(dir.join("schemas/aux_code/flypy_full.txt")).ok()?;
        let mut m: HashMap<char, Vec<String>> = HashMap::new();
        for line in src.lines().filter(|l| !l.starts_with('#')) {
            let Some((w, c)) = line.split_once('=') else {
                continue;
            };
            let mut it = w.chars();
            let c = c.trim();
            if let (Some(ch), None) = (it.next(), it.next())
                && !c.is_empty()
                && c.bytes().all(|b| b.is_ascii_lowercase())
            {
                m.entry(ch).or_default().push(c.to_string());
            }
        }
        Some(Table(m))
    }

    /// `grammar_sentence_eval.rs` 的 `("拼音", "汉字")` 行。
    fn sentences() -> Vec<(String, String)> {
        let src = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/grammar_sentence_eval.rs"),
        )
        .expect("读语料");
        let mut v = Vec::new();
        for line in src.lines() {
            let Some(rest) = line.trim_start().strip_prefix("(\"") else {
                continue;
            };
            let Some((py, rest)) = rest.split_once("\", \"") else {
                continue;
            };
            let Some((hz, _)) = rest.split_once('"') else {
                continue;
            };
            if py.bytes().all(|b| b.is_ascii_lowercase()) {
                v.push((py.to_string(), hz.to_string()));
            }
        }
        v
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// (击键, 汉字)：筛法同 `shuangpin_raw_parity` 的 `corpus`；另要求每音节两键。
    fn corpus(conv: &ShuangpinConverter) -> Vec<(Vec<String>, String)> {
        corpus_of(conv, sentences())
    }

    /// 第二份语料 `tests/aux_continuous_corpus_b.txt`（`拼音<TAB>汉字`）。
    fn sentences_b() -> Vec<(String, String)> {
        let src = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/aux_continuous_corpus_b.txt"),
        )
        .expect("读语料 B");
        src.lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once('\t'))
            .map(|(py, hz)| (py.trim().to_string(), hz.trim().to_string()))
            .collect()
    }

    fn corpus_of(
        conv: &ShuangpinConverter,
        raw: Vec<(String, String)>,
    ) -> Vec<(Vec<String>, String)> {
        let rev = conv.build_reverse();
        let trie = SyllableTrie::new();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (py, hz) in raw {
            if !seen.insert(hz.clone()) {
                continue;
            }
            let n = hz.chars().count();
            let syls = Dag::build(&py, &trie).maximum_match();
            if n < 4 || syls.len() != n {
                continue;
            }
            let syls: Vec<&str> = syls.iter().map(String::as_str).collect();
            let Some(keys) = rev.encode_all(&syls) else {
                continue;
            };
            assert_eq!(keys.len(), 2 * n, "{hz}");
            let pairs = (0..n).map(|i| keys[2 * i..2 * i + 2].to_string()).collect();
            out.push((pairs, hz));
        }
        out
    }

    /// (击键, 目标, k)：插法同 spike 的 `build_samples`（同种子、同顺序）。
    fn samples(corpus: &[(Vec<String>, String)], table: &Table) -> Vec<(String, String, usize)> {
        let mut rng = Rng(20261118);
        let mut out = Vec::new();
        for (pairs, hz) in corpus {
            let chars: Vec<char> = hz.chars().collect();
            if chars.iter().any(|c| !table.0.contains_key(c)) {
                continue;
            }
            for k in 1..=3usize {
                let mut pos: Vec<usize> = Vec::new();
                while pos.len() < k {
                    let p = rng.below(chars.len());
                    if !pos.contains(&p) {
                        pos.push(p);
                    }
                }
                let mut raw = String::new();
                for (i, kk) in pairs.iter().enumerate() {
                    raw.push_str(kk);
                    if pos.contains(&i) {
                        let code = &table.0[&chars[i]][0];
                        let len = if code.len() >= 2 && rng.below(2) == 1 {
                            2
                        } else {
                            1
                        };
                        raw.push_str(&code[..len]);
                    }
                }
                out.push((raw, hz.clone(), k));
            }
        }
        out
    }

    /// `WIND_AUX_GATE_MARGIN=<裕度>` 时覆写本线程的闸门裕度（扫参用）。
    fn gate_margin_from_env() {
        if let Some(m) = std::env::var("WIND_AUX_GATE_MARGIN")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
        {
            super::set_aux_gate_margin(Some(m));
            eprintln!("# gate margin={m}");
        }
    }

    /// 界面序首选（同 `pinyin_eval::ui_candidates`）。
    fn ui_top(mut c: Vec<wind_candidate::Candidate>, input: &str) -> Option<String> {
        ui_sort(&mut c, input);
        c.into_iter().next().map(|c| c.text)
    }

    fn ui_sort(c: &mut Vec<wind_candidate::Candidate>, input: &str) {
        wind_candidate::drop_corrected_duplicates(c);
        c.sort_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, input));
        wind_candidate::protect_top_from_corrected(c, input.len());
    }

    /// k=0 逐键前缀上开辅码表后首选 `(文字, 消费数)` 与不给表时不同的前缀，按奇偶、文字是否相同、
    /// 首选是否用了辅码分桶；另报 λ 极大（结构性差异）与关掉 `aux_whole` 改写时的差异数。
    /// `WIND_AUX_K0_DUMP=<文件>` 时把 λ=2 的差异逐条写成 TSV。
    #[test]
    #[ignore = "k=0 差异分类：依赖 build_dev 真实词库与辅码表"]
    fn k0_classify() {
        let dir = root().join("build_dev/data");
        if !dir.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
            eprintln!("跳过：拼音词库不存在");
            return;
        }
        let Some(table) = load_table(&dir) else {
            eprintln!("跳过：辅码表不存在");
            return;
        };
        let conv = ShuangpinConverter::new(
            Layout::from_toml(&root().join("data/schemas/shuangpin/xiaohe.toml")).unwrap(),
        );
        gate_margin_from_env();
        let corpus = corpus(&conv);
        let table: Arc<dyn AuxCharLookup> = Arc::new(table);
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec!["shuangpin".to_string()];
        cfg.schema.active = "shuangpin".to_string();
        let mgr = EngineManager::new(&cfg, Some(&dir));
        // (前缀, 来源句, 是否整句)
        let mut seen = HashSet::new();
        let mut prefixes: Vec<(String, String, bool)> = Vec::new();
        for (p, hz) in &corpus {
            let k = p.concat();
            for n in 1..=k.len() {
                if seen.insert(k[..n].to_string()) {
                    prefixes.push((k[..n].to_string(), hz.clone(), n == k.len()));
                }
            }
        }
        // 首选 (文字, 消费数, 是否用了辅码)
        let run = |keys: &str, aux: bool| {
            let opts = ConvertOptions {
                sp_raw_lattice: true,
                aux_continuous: aux.then(|| table.clone()),
                ..Default::default()
            };
            super::take_aux_sentences();
            let c = mgr
                .convert_with_opts("shuangpin", keys, 300, opts)
                .candidates;
            let used = super::take_aux_sentences();
            c.first().map(|c| {
                (
                    c.text.clone(),
                    c.consumed_length,
                    c.is_sentence && used.contains(&c.text),
                )
            })
        };
        let base: Vec<_> = prefixes.iter().map(|(k, _, _)| run(k, false)).collect();
        let key = |t: &Option<(String, usize, bool)>| t.as_ref().map(|t| (t.0.clone(), t.1));
        let differ = |lambda: f64, whole_off: bool| {
            let old = super::set_aux_letter_penalty(lambda);
            let old_w = super::set_aux_whole_off(whole_off);
            let out: Vec<_> = prefixes
                .iter()
                .zip(&base)
                .filter_map(|((k, hz, full), b)| {
                    let on = run(k, true);
                    (key(&on) != key(b)).then(|| (k.clone(), hz.clone(), *full, b.clone(), on))
                })
                .collect();
            super::set_aux_letter_penalty(old);
            super::set_aux_whole_off(old_w);
            out
        };
        let total = prefixes.len();
        let d2 = differ(2.0, false);
        let d_big = differ(1e6, false);
        let d2_off = differ(2.0, true);
        let d_big_off = differ(1e6, true);
        eprintln!("# prefixes={total}");
        eprintln!("# λ=2 differ={}", d2.len());
        eprintln!("# λ=1e6 differ={}", d_big.len());
        eprintln!("# λ=2 aux_whole off differ={}", d2_off.len());
        eprintln!("# λ=1e6 aux_whole off differ={}", d_big_off.len());
        let odd_total = prefixes.iter().filter(|p| p.0.len() % 2 == 1).count();
        eprintln!("# odd prefixes={odd_total} even={}", total - odd_total);
        for (name, d) in [("λ=2", &d2), ("λ=1e6", &d_big), ("λ=2 whole_off", &d2_off)] {
            // (奇偶, 文字同/异, 首选用辅码) → 条数
            let mut buckets: std::collections::BTreeMap<(&str, &str, &str), usize> =
                Default::default();
            for (k, _, _, b, on) in d.iter() {
                let parity = if k.len() % 2 == 1 { "odd" } else { "even" };
                let same = match (b, on) {
                    (Some(b), Some(o)) if b.0 == o.0 => "same_text",
                    _ => "diff_text",
                };
                let aux = if on.as_ref().is_some_and(|o| o.2) {
                    "aux_used"
                } else {
                    "no_aux"
                };
                *buckets.entry((parity, same, aux)).or_default() += 1;
            }
            for ((p, s, a), n) in buckets {
                eprintln!("# {name} bucket {p} {s} {a} = {n}");
            }
        }
        if let Ok(path) = std::env::var("WIND_AUX_K0_DUMP") {
            let fmt = |t: &Option<(String, usize, bool)>| {
                t.as_ref()
                    .map_or("-".to_string(), |t| format!("{}\t{}", t.0, t.1))
            };
            let mut s = String::from(
                "keys\tlen\tfull\tsource\toff_text\toff_consumed\ton_text\ton_consumed\ton_aux\n",
            );
            for (k, hz, full, b, on) in &d2 {
                s.push_str(&format!(
                    "{k}\t{}\t{full}\t{hz}\t{}\t{}\t{}\n",
                    k.len(),
                    fmt(b),
                    fmt(on),
                    on.as_ref().is_some_and(|o| o.2)
                ));
            }
            std::fs::write(&path, s).expect("写 TSV");
            eprintln!("# dump → {path}");
        }
    }

    /// 逐条看候选：`WIND_AUX_PROBE=键1,键2` 时打印开 / 关辅码表的前 5 条。
    #[test]
    #[ignore = "探针：依赖 build_dev 真实词库与辅码表"]
    fn k0_probe() {
        let dir = root().join("build_dev/data");
        let (Ok(keys), Some(table)) = (std::env::var("WIND_AUX_PROBE"), load_table(&dir)) else {
            eprintln!("跳过：未给 WIND_AUX_PROBE 或缺辅码表");
            return;
        };
        let table: Arc<dyn AuxCharLookup> = Arc::new(table);
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec!["shuangpin".to_string()];
        cfg.schema.active = "shuangpin".to_string();
        let mgr = EngineManager::new(&cfg, Some(&dir));
        for k in keys.split(',') {
            for aux in [false, true] {
                let opts = ConvertOptions {
                    sp_raw_lattice: true,
                    aux_continuous: aux.then(|| table.clone()),
                    ..Default::default()
                };
                super::take_aux_sentences();
                let c = mgr.convert_with_opts("shuangpin", k, 300, opts).candidates;
                let used = super::take_aux_sentences();
                let top: Vec<String> = c
                    .iter()
                    .take(5)
                    .map(|c| {
                        format!(
                            "{}/{}{}{}",
                            c.text,
                            c.consumed_length,
                            if c.is_sentence { "/S" } else { "" },
                            if used.contains(&c.text) { "/A" } else { "" }
                        )
                    })
                    .collect();
                eprintln!(
                    "{k} {} {:?} aux_sent={used:?}",
                    if aux { "ON " } else { "OFF" },
                    top
                );
            }
        }
    }

    #[test]
    #[ignore = "λ 扫参：慢，依赖 build_dev 真实词库与辅码表"]
    fn lambda_sweep() {
        let dir = root().join("build_dev/data");
        if !dir.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
            eprintln!("跳过：拼音词库不存在");
            return;
        }
        let Some(table) = load_table(&dir) else {
            eprintln!("跳过：辅码表不存在");
            return;
        };
        let conv = ShuangpinConverter::new(
            Layout::from_toml(&root().join("data/schemas/shuangpin/xiaohe.toml")).unwrap(),
        );
        gate_margin_from_env();
        let corpus = corpus(&conv);
        let samples = samples(&corpus, &table);
        let table: Arc<dyn AuxCharLookup> = Arc::new(table);
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec!["shuangpin".to_string()];
        cfg.schema.active = "shuangpin".to_string();
        let mgr = EngineManager::new(&cfg, Some(&dir));
        let mut seen = HashSet::new();
        let prefixes: Vec<String> = corpus
            .iter()
            .map(|(p, _)| p.concat())
            .flat_map(|k| (1..=k.len()).map(move |n| k[..n].to_string()))
            .filter(|p| seen.insert(p.clone()))
            .collect();
        let run = |keys: &str, aux: bool| {
            let opts = ConvertOptions {
                sp_raw_lattice: true,
                aux_continuous: aux.then(|| table.clone()),
                ..Default::default()
            };
            mgr.convert_with_opts("shuangpin", keys, 300, opts)
                .candidates
        };
        let top = |c: &[wind_candidate::Candidate]| {
            c.first().map(|c| (c.text.clone(), c.consumed_length))
        };
        let base: Vec<_> = prefixes.iter().map(|k| top(&run(k, false))).collect();
        let n_k = |k| samples.iter().filter(|s| s.2 == k).count();
        eprintln!(
            "# sentences={} prefixes={} samples k1/k2/k3={}/{}/{}",
            corpus.len(),
            prefixes.len(),
            n_k(1),
            n_k(2),
            n_k(3)
        );
        // 无辅码表时的参照（k≥1 的辅码字母只能当音节或残码读）。
        let mut cfgs: Vec<Option<f64>> = vec![None];
        cfgs.extend([1.0, 2.0, 3.0].map(Some));
        for lambda in cfgs {
            let old = lambda.map(super::set_aux_letter_penalty);
            let differ = match lambda {
                None => 0,
                Some(_) => prefixes
                    .iter()
                    .zip(&base)
                    .filter(|(k, b)| top(&run(k, true)) != **b)
                    .count(),
            };
            let mut line = format!(
                "# λ={} k=0 top1 differs vs None={differ}/{}",
                lambda.map_or("None".to_string(), |l| l.to_string()),
                prefixes.len()
            );
            for k in 1..=3 {
                let (mut eng, mut ui, mut n) = (0, 0, 0);
                for (raw, target, _) in samples.iter().filter(|s| s.2 == k) {
                    let c = run(raw, lambda.is_some());
                    n += 1;
                    eng += usize::from(c.first().is_some_and(|c| &c.text == target));
                    ui += usize::from(ui_top(c, raw).as_ref() == Some(target));
                }
                line.push_str(&format!(" | k={k} engine {eng}/{n} ui {ui}/{n}"));
            }
            eprintln!("{line}");
            if let Some(o) = old {
                super::set_aux_letter_penalty(o);
            }
        }
    }

    fn env_list(name: &str, default: &str) -> Vec<String> {
        std::env::var(name)
            .unwrap_or_else(|_| default.to_string())
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// 内置 `shuangpin` 方案经 override 换布局；`grammar` 为模型权重时挂 mini 模型。
    fn sweep_manager(dir: &Path, layout: &str, grammar: Option<f64>) -> EngineManager {
        let ov = std::env::temp_dir().join(format!(
            "wind_aux_sweep_{}/ov_{layout}_{grammar:?}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&ov);
        std::fs::create_dir_all(&ov).unwrap();
        std::fs::write(
            ov.join("shuangpin.toml"),
            format!("[engine.pinyin.shuangpin]\nlayout = \"{layout}\"\n"),
        )
        .unwrap();
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec!["shuangpin".to_string()];
        cfg.schema.active = "shuangpin".to_string();
        if let Some(w) = grammar {
            cfg.schema.pinyin.grammar.enabled = true;
            cfg.schema.pinyin.grammar.weight = w;
            cfg.schema.pinyin.grammar.model = "wanxiang-mini.gram".to_string();
        }
        let mgr = EngineManager::with_store_override(&cfg, Some(dir), None, Some(ov));
        assert_eq!(mgr.shuangpin_layout_of("shuangpin"), layout);
        mgr
    }

    /// P2 扫参：语料 × 布局 × 模型开 / 关 × λ × 裕度 × 落单放行裕度。每格报 k=0 逐键前缀差异
    /// （按来源句归类）、k=1/2/3 引擎首选命中，与闸门分差：k=0 前缀上过了落单判据的辅码整句
    /// （全是假读法）的最大分差，k≥1 整串上拼对目标句的辅码整句的最小分差。
    ///
    /// ```text
    /// WIND_AUX_SWEEP_CORPUS=a|b WIND_AUX_SWEEP_LAYOUTS=xiaohe,ziranma,mspy WIND_AUX_SWEEP_GRAMMAR=off,on \
    /// WIND_AUX_SWEEP_LAMBDAS=1,2,3 WIND_AUX_SWEEP_MARGINS=none,5 WIND_AUX_SWEEP_DANGLING=none,5 \
    /// cargo test -p wind-engine --release --lib p2_sweep -- --ignored --nocapture --test-threads=1
    /// ```
    ///
    /// `WIND_AUX_SWEEP_MARGINS`：`none` = 出厂两档（模型关 / 开各用各的，默认），数字 = 两档同取该值。
    /// `WIND_AUX_SWEEP_DANGLING`：`default` = 出厂，`none` = 落单一律不放行，数字 = 落单时按该裕度比分。
    /// `WIND_AUX_SWEEP_UI=1`：首选按界面序（协调器同款排序）取。
    /// `WIND_AUX_SWEEP_SE`：末槽辅码整句的闸门裕度（`none` = 不设闸）；
    /// `WIND_AUX_SWEEP_GAPS=<文件>`：逐条写出闸门比分（两侧都写）；
    /// `WIND_AUX_SWEEP_WEIGHTS`：模型开时的 `grammar.weight`（默认 `0.5`，可列多个）。
    #[test]
    #[ignore = "P2 扫参：慢，依赖 build_dev 真实词库、辅码表与语法模型"]
    fn p2_sweep() {
        let dir = root().join("build_dev/data");
        if !dir.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists() {
            eprintln!("跳过：拼音词库不存在");
            return;
        }
        let Some(table) = load_table(&dir) else {
            eprintln!("跳过：辅码表不存在");
            return;
        };
        let nums = |name: &str, default: &str| -> Vec<f64> {
            env_list(name, default)
                .iter()
                .map(|v| v.parse().expect("数字"))
                .collect()
        };
        let lambdas = nums("WIND_AUX_SWEEP_LAMBDAS", "2");
        let opt_list = |name: &str| -> Vec<Option<f64>> {
            env_list(name, "none")
                .iter()
                .map(|v| (v != "none").then(|| v.parse().expect("数字")))
                .collect()
        };
        let margins = opt_list("WIND_AUX_SWEEP_MARGINS");
        // `default` = 出厂，`none` = 一律不放行，数字 = 按该裕度比分。
        let danglings: Vec<Option<Option<f64>>> = env_list("WIND_AUX_SWEEP_DANGLING", "default")
            .iter()
            .map(|v| match v.as_str() {
                "default" => None,
                "none" => Some(None),
                v => Some(Some(v.parse().expect("数字"))),
            })
            .collect();
        let ses = opt_list("WIND_AUX_SWEEP_SE");
        let mut configs = Vec::new();
        for &l in &lambdas {
            for &m in &margins {
                for &d in &danglings {
                    for &se in &ses {
                        configs.push((l, m, d, se));
                    }
                }
            }
        }
        let gaps_out = std::env::var("WIND_AUX_SWEEP_GAPS").ok();
        let mut gap_rows = String::new();
        let corpus_id = std::env::var("WIND_AUX_SWEEP_CORPUS").unwrap_or_else(|_| "a".into());
        let raw = match corpus_id.as_str() {
            "a" => sentences(),
            "b" => sentences_b(),
            other => panic!("未知语料 {other}"),
        };
        let dump = std::env::var("WIND_AUX_SWEEP_DUMP").ok();
        let ui = std::env::var("WIND_AUX_SWEEP_UI").is_ok_and(|v| v == "1");
        let mut dump_rows = String::new();
        let table_arc: Arc<dyn AuxCharLookup> = Arc::new(Table(table.0.clone()));
        for layout in env_list("WIND_AUX_SWEEP_LAYOUTS", "xiaohe") {
            let conv = ShuangpinConverter::new(
                Layout::from_toml(&root().join(format!("data/schemas/shuangpin/{layout}.toml")))
                    .unwrap(),
            );
            let corpus = corpus_of(&conv, raw.clone());
            assert!(corpus.len() > 100, "语料只有 {} 句", corpus.len());
            let samples = samples(&corpus, &table);
            let mut seen = HashSet::new();
            let prefixes: Vec<(String, String, bool)> = corpus
                .iter()
                .flat_map(|(p, hz)| {
                    let k = p.concat();
                    (1..=k.len())
                        .map(|n| (k[..n].to_string(), hz.clone(), n == k.len()))
                        .collect::<Vec<_>>()
                })
                .filter(|(p, _, _)| seen.insert(p.clone()))
                .collect();
            let weights = nums("WIND_AUX_SWEEP_WEIGHTS", "0.5");
            let grams: Vec<(String, Option<f64>)> = env_list("WIND_AUX_SWEEP_GRAMMAR", "off")
                .into_iter()
                .flat_map(|g| {
                    if g == "on" {
                        weights
                            .iter()
                            .map(|&w| (format!("on{w}"), Some(w)))
                            .collect()
                    } else {
                        vec![(g, None)]
                    }
                })
                .collect();
            for (gram, weight) in grams {
                let mgr = sweep_manager(&dir, &layout, weight);
                let top = |keys: &str, aux: bool| {
                    let opts = ConvertOptions {
                        sp_raw_lattice: true,
                        aux_continuous: aux.then(|| table_arc.clone()),
                        ..Default::default()
                    };
                    let mut c = mgr
                        .convert_with_opts("shuangpin", keys, 300, opts)
                        .candidates;
                    if ui {
                        ui_sort(&mut c, keys);
                    }
                    c.first().map(|c| (c.text.clone(), c.consumed_length))
                };
                let base: Vec<_> = prefixes.iter().map(|p| top(&p.0, false)).collect();
                let ok = |t: &Option<(String, usize)>, src: &str, full: bool| {
                    t.as_ref().is_some_and(|(t, _)| {
                        if full {
                            t == src
                        } else {
                            !t.is_empty() && src.starts_with(t.as_str())
                        }
                    })
                };
                for &(lambda, margin, dangling, se) in &configs {
                    let old_l = super::set_aux_letter_penalty(lambda);
                    let old_m = super::set_aux_gate_margin(margin);
                    let old_d = super::set_aux_dangling_margin(dangling);
                    let old_s = super::set_aux_se_gate_margin(se);
                    let cfg =
                        format!("λ={lambda} margin={margin:?} dangling={dangling:?} se={se:?}");
                    let gap_row = |side: &str, keys: &str, g: &super::GateGap, ok: bool| {
                        format!(
                            "{corpus_id}\t{layout}\t{gram}\t{cfg}\t{side}\t{keys}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                            g.kind,
                            g.sentence,
                            u8::from(g.dangling),
                            g.score.map_or("-".into(), |v| format!("{v:.3}")),
                            g.rival.map_or("-".into(), |v| format!("{v:.3}")),
                            g.gap().map_or("-".into(), |v| format!("{v:.3}")),
                            g.chars,
                            g.aux_letters,
                            u8::from(ok)
                        )
                    };
                    let (mut differ, mut better, mut worse, mut neither) = (0, 0, 0, 0);
                    // [kind 'n' / 'e'] → (有对手的条数, 最大分差)
                    let mut k0_max = [(0usize, f64::NEG_INFINITY); 2];
                    let kind_i = |c: char| usize::from(c == 'e');
                    super::take_gate_gaps();
                    for ((k, hz, full), b) in prefixes.iter().zip(&base) {
                        let on = top(k, true);
                        for g in super::take_gate_gaps() {
                            if let Some(v) = g.gap() {
                                let m = &mut k0_max[kind_i(g.kind)];
                                m.0 += 1;
                                m.1 = m.1.max(v);
                            }
                            if gaps_out.is_some() {
                                gap_rows.push_str(&gap_row("k0", k, &g, false));
                            }
                        }
                        if on == *b {
                            continue;
                        }
                        differ += 1;
                        let v = match (ok(b, hz, *full), ok(&on, hz, *full)) {
                            (false, true) => {
                                better += 1;
                                "new_better"
                            }
                            (true, false) => {
                                worse += 1;
                                "new_worse"
                            }
                            _ => {
                                neither += 1;
                                "neither"
                            }
                        };
                        if dump.is_some() {
                            let f = |t: &Option<(String, usize)>| {
                                t.as_ref()
                                    .map_or("-\t0".to_string(), |t| format!("{}\t{}", t.0, t.1))
                            };
                            dump_rows.push_str(&format!(
                                "{corpus_id}\t{layout}\t{gram}\t{cfg}\t{k}\t{hz}\t{}\t{}\t{v}\n",
                                f(b),
                                f(&on)
                            ));
                        }
                    }
                    let mut line = format!(
                        "# corpus={corpus_id} layout={layout} gram={gram} {cfg} | k0 differ={differ}/{} better={better} worse={worse} neither={neither} | k0 false S_n rival={} max={:.2} S_e rival={} max={:.2}",
                        prefixes.len(),
                        k0_max[0].0,
                        k0_max[0].1,
                        k0_max[1].0,
                        k0_max[1].1
                    );
                    // k≥1：拼对目标句的辅码整句的最小分差（按 S_n / S_e 分开）。
                    let mut legit_min = [(0usize, f64::INFINITY); 2];
                    for k in 1..=3 {
                        let (mut hit, mut n) = (0, 0);
                        for (raw, target, _) in samples.iter().filter(|s| s.2 == k) {
                            n += 1;
                            super::take_gate_gaps();
                            hit += usize::from(top(raw, true).is_some_and(|t| &t.0 == target));
                            for g in super::take_gate_gaps() {
                                let legit = &g.sentence == target;
                                if legit && let Some(v) = g.gap() {
                                    let m = &mut legit_min[kind_i(g.kind)];
                                    m.0 += 1;
                                    m.1 = m.1.min(v);
                                }
                                if gaps_out.is_some() {
                                    gap_rows.push_str(&gap_row(&format!("k{k}"), raw, &g, legit));
                                }
                            }
                        }
                        line.push_str(&format!(" | k={k} {hit}/{n}"));
                    }
                    line.push_str(&format!(
                        " | legit S_n rival={} min={:.2} S_e rival={} min={:.2}",
                        legit_min[0].0, legit_min[0].1, legit_min[1].0, legit_min[1].1
                    ));
                    eprintln!("{line}");
                    super::set_aux_letter_penalty(old_l);
                    super::set_aux_gate_margin(old_m);
                    super::set_aux_dangling_margin(old_d);
                    super::set_aux_se_gate_margin(old_s);
                }
            }
        }
        if let Some(p) = dump {
            std::fs::write(&p, dump_rows).expect("写 TSV");
            eprintln!("# dump → {p}");
        }
        if let Some(p) = gaps_out {
            std::fs::write(&p, gap_rows).expect("写分差 TSV");
            eprintln!("# gaps → {p}");
        }
        let _ = std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("wind_aux_sweep_{}", std::process::id())),
        );
    }
}
