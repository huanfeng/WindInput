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
#[path = "raw_lattice_tests.rs"]
mod tests;

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
#[path = "raw_lattice_lambda_sweep_tests.rs"]
mod lambda_sweep;
