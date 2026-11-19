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

use std::collections::{BTreeMap, HashMap};

use wind_dict::DictManager;
use wind_dict::cached::CachedDict;

use super::PinyinEngine;
use super::fuzzy::{FuzzyConfig, FuzzyMatcher};
use super::lattice::{
    FUZZY_LAYER_BUDGET, FUZZY_SYLLABLE_LOG_PENALTY, LatticeBuilder, LatticeNode,
    PARTIAL_FINAL_NODE_LIMIT, PARTIAL_FINAL_PENALTY, USER_NODE_BONUS, USER_NODE_LIMIT,
    USER_NODE_WEIGHT_CAP, is_sentence_store_word, score_node, score_node_partial_final,
};
use super::shuangpin::RawEdge;
use super::viterbi::{ViterbiResult, WordNode};

/// 一个音节槽：边的击键跨度（已并入相邻 `'`）与可选读音 `(全拼音节, 处数)`。
struct Slot {
    start: usize,
    end: usize,
    options: Vec<(String, usize)>,
}

/// 从某个槽出发拼出的一种读法。
struct Shape {
    /// 全拼码自身的音节掩码
    mask: u64,
    syllables: Vec<String>,
    edits: usize,
    /// 每条边都取首读音（= 旧路径的所打码）
    exact: bool,
}

/// `(终止槽, 全拼码)` → 拼出它的各种读法。BTreeMap：查询与建节点顺序确定。
type Spans = BTreeMap<(usize, String), Vec<Shape>>;

struct RawChain {
    slots: Vec<Slot>,
    /// `linked[i]`：槽 i 与槽 i+1 首尾相接
    linked: Vec<bool>,
    /// 槽 i 的起点从 0 可达
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

    /// 从槽 `i` 起、至多 `max_syllables` 个音节的全部读法，按 `(终止槽, 全拼码)` 归并。
    /// `can_extend(码)` 为假的组合不再延伸。
    fn spans_from(
        &self,
        i: usize,
        max_syllables: usize,
        can_extend: &dyn Fn(&str) -> bool,
    ) -> Spans {
        /// 在途组合：(全拼码, 掩码, 音节, 处数, 全取首读音)
        type Combo = (String, u64, Vec<String>, usize, bool);
        let mut spans = Spans::new();
        let mut layer: Vec<Combo> = vec![(String::new(), 0, Vec::new(), 0, true)];
        for k in 0..max_syllables {
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
                // 稳定排序：首读音组合（处数 0、先生成）恒在最前，不会被截。
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

/// 同码里有首读音组合时只认它（同旧路径：所打码本身不走模糊分支）。
fn exact_shape(shapes: &[Shape]) -> Option<&Shape> {
    shapes.iter().find(|s| s.exact)
}

impl LatticeBuilder {
    /// 击键坐标版 [`Self::build`]：`nodes[end]` = 在击键偏移 `end` 结束的节点，长度 `keys.len()+1`。
    ///
    /// 系统词的约束与打分同 `build`：首读音组合是精确节点，词条边界须等于其掩码（`0` 放行）；
    /// 模糊读音扣 [`FUZZY_SYLLABLE_LOG_PENALTY`] × 处数，边界须等于某读法的掩码（`0` 放行、取处数
    /// 最少者）；同词同起点精确恒胜，模糊之间取分高者。
    /// `require_reachable` 同 `build`：只在从 0 连通的槽上起节点。
    pub(crate) fn build_raw(
        &self,
        keys: &str,
        edges: &[RawEdge],
        dict: &CachedDict,
        fuzzy_config: Option<&FuzzyConfig>,
        require_reachable: bool,
    ) -> Vec<Vec<LatticeNode>> {
        let mut nodes: Vec<Vec<LatticeNode>> = vec![Vec::new(); keys.len() + 1];
        let chain = RawChain::new(keys, edges, fuzzy_config);
        // 当前 (起点, 终点) 上的词 → (在 `nodes[终点]` 的下标, 是否模糊来源)；模糊来源的节点
        // 在精确节点到来时要被顶掉。常见音节同音字上千，线性找同词会退化成平方。
        let mut seen: HashMap<String, (usize, bool)> = HashMap::new();
        for i in 0..chain.slots.len() {
            if require_reachable && !chain.reachable[i] {
                continue;
            }
            let p = chain.slots[i].start;
            // 对精确组合也剪：只是省查询，与旧精确分支不剪枝的结果相同。
            let spans = chain.spans_from(i, self.max_word_len(), &|c| dict.has_longer_code(c));
            // 槽起点互不相同，同一终点槽的跨度在 `spans` 里相邻：换终点槽时清表即可。
            let mut seen_j = None;
            for ((j, code), shapes) in &spans {
                if seen_j != Some(*j) {
                    seen.clear();
                    seen_j = Some(*j);
                }
                let q = chain.slots[*j].end;
                let exact = exact_shape(shapes);
                for hit in dict.search_with_boundary(code) {
                    let shape = match exact {
                        Some(s) if hit.boundary == 0 || hit.boundary == s.mask => s,
                        // 精确读法掩码不符时直接跳过、不再试模糊读法；边链固定，实际碰不到。
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
                            *existing = raw_node(p, q, hit.text, code, shape, log_prob);
                            *existing_fuzzy = is_fuzzy;
                        }
                        continue;
                    }
                    seen.insert(hit.text.clone(), (nodes[q].len(), is_fuzzy));
                    nodes[q].push(raw_node(p, q, hit.text, code, shape, log_prob));
                }
            }
        }
        nodes
    }

    /// 击键坐标版 [`Self::add_store_nodes`]：用户词 / 临时词按全拼码点查进图。
    ///
    /// 约束同原方法：只收 [`is_sentence_store_word`]；记录边界须**等于**某读法的掩码
    /// （`boundary == 0` 不进图）；模糊读法扣 [`FUZZY_SYLLABLE_LOG_PENALTY`] × 处数，再叠
    /// [`USER_NODE_WEIGHT_CAP`] / [`USER_NODE_BONUS`]；同词同起点取分高者。须在 [`Self::build_raw`] 之后调。
    pub(crate) fn add_store_nodes_raw(
        &self,
        keys: &str,
        edges: &[RawEdge],
        store: &DictManager,
        fuzzy_config: Option<&FuzzyConfig>,
        require_reachable: bool,
        nodes: &mut [Vec<LatticeNode>],
    ) {
        let chain = RawChain::new(keys, edges, fuzzy_config);
        for i in 0..chain.slots.len() {
            if require_reachable && !chain.reachable[i] {
                continue;
            }
            let p = chain.slots[i].start;
            let spans = chain.spans_from(i, self.max_word_len(), &|_| true);
            for ((j, code), shapes) in &spans {
                let q = chain.slots[*j].end;
                if q >= nodes.len() {
                    continue;
                }
                let exact = exact_shape(shapes);
                for cand in store.search(code, USER_NODE_LIMIT) {
                    if !is_sentence_store_word(&cand) {
                        continue;
                    }
                    let shape = match exact {
                        Some(s) if s.mask == cand.boundary => s,
                        Some(_) => continue,
                        None => match shapes
                            .iter()
                            .filter(|s| s.mask == cand.boundary)
                            .min_by_key(|s| s.edits)
                        {
                            Some(s) => s,
                            None => continue,
                        },
                    };
                    let weight = cand.weight.min(USER_NODE_WEIGHT_CAP);
                    let log_prob = score_node(&cand.text, code, weight)
                        - FUZZY_SYLLABLE_LOG_PENALTY * shape.edits as f64
                        + USER_NODE_BONUS;
                    if let Some(existing) = nodes[q]
                        .iter_mut()
                        .find(|n| n.word == cand.text && n.start == p)
                    {
                        if log_prob > existing.log_prob {
                            *existing = raw_node(p, q, cand.text, code, shape, log_prob);
                        }
                        continue;
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
    pub(crate) fn add_partial_final_raw(
        &self,
        keys: &str,
        completed_len: usize,
        partial: &str,
        dict: &CachedDict,
        nodes: &mut [Vec<LatticeNode>],
    ) {
        let end = keys.len();
        if completed_len >= end
            || end >= nodes.len()
            || partial.is_empty()
            || !partial.bytes().all(|b| b.is_ascii_lowercase())
        {
            return;
        }
        for hit in dict.search_prefix_with_boundary_syllable_capped(
            partial,
            PARTIAL_FINAL_NODE_LIMIT,
            1,
            0,
        ) {
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
                word: hit.text,
                syllables: vec![partial.to_string()],
                syl_mask: 1,
                log_prob,
                canon: Some((partial.to_string(), 1)),
            });
        }
    }
}

/// step 2 与 2c 共用的击键图底图：系统词 + 用户词、已滤屏蔽词。
pub(super) struct RawBase {
    lattice: Vec<Vec<WordNode>>,
    /// 系统词节点最远的终点（不看用户词），即从 0 连续可达的最远槽末尾。
    reach: usize,
    /// 末槽的终点，见 [`last_slot_end`]
    last_slot_end: Option<usize>,
}

impl RawBase {
    /// step 2 的整句图：底图本身，终点是 `reach`（击键数，可直接当 `consumed_length`）。
    /// `reach` 没到末槽时为 `None`（整句跳过，同旧路径末音节无词时解不出）。
    pub(super) fn sentence(&self) -> Option<(&[Vec<WordNode>], usize)> {
        sentence_end(self.reach, self.last_slot_end).map(|end| (self.lattice.as_slice(), end))
    }
}

impl PinyinEngine {
    /// 开关打开时的底图：击键坐标建图、补用户词、滤屏蔽词，转成解码用的节点。
    /// 一个系统词节点都没有时为 `None`（step 2 与 2c 都跳过）。
    pub(super) fn raw_base_lattice(&self, keys: &str, edges: &[RawEdge]) -> Option<RawBase> {
        let fuzzy = Some(&self.fuzzy_config);
        let mut nodes = self
            .lattice_builder
            .build_raw(keys, edges, &self.dict, fuzzy, true);
        let reach = nodes.iter().rposition(|v| !v.is_empty())?;
        if self.config.sentence_uses_user_words
            && let Some(store) = &self.store_layers
        {
            self.lattice_builder
                .add_store_nodes_raw(keys, edges, store, fuzzy, true, &mut nodes);
        }
        self.drop_blocked_nodes(&mut nodes);
        Some(RawBase {
            lattice: to_word_nodes(nodes),
            reach,
            last_slot_end: last_slot_end(keys, edges),
        })
    }

    /// 2c 的整句图：只在键对恰好铺到末键之前时，在底图末端补残码节点，终点为整串；否则 `None`。
    pub(super) fn raw_partial_lattice(
        &self,
        keys: &str,
        base: RawBase,
        partial: &str,
    ) -> Option<(Vec<Vec<WordNode>>, usize)> {
        let RawBase {
            mut lattice, reach, ..
        } = base;
        let end = keys.len();
        if reach + 1 != end {
            return None;
        }
        // 底图节点都止于完整键对，末键上为空，残码节点与它们不必去重。
        debug_assert!(lattice[end].is_empty());
        let mut tail: Vec<Vec<LatticeNode>> = vec![Vec::new(); end + 1];
        self.lattice_builder
            .add_partial_final_raw(keys, reach, partial, &self.dict, &mut tail);
        self.drop_blocked_nodes(&mut tail);
        let extra = to_word_nodes(tail).swap_remove(end);
        lattice[end].extend(extra);
        Some((lattice, end))
    }
}

fn to_word_nodes(nodes: Vec<Vec<LatticeNode>>) -> Vec<Vec<WordNode>> {
    nodes
        .into_iter()
        .map(|at_end| {
            at_end
                .into_iter()
                .map(|n| WordNode {
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
        syllables: shape.syllables.clone(),
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
}
