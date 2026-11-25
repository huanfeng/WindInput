//! 歧义键盘（九宫格 T9）的全拼解码。设计见 `docs/design/t9-pinyin.md`。
//!
//! 查询串里每个字符代表一组字母：`2`–`9` 是九宫格键，`a`–`z` 是已锁定的字母（左栏选定的
//! 读音、或实体键盘打的字母），`'` 是分词。一键对应一个字母，所以击键偏移与全拼字母偏移
//! 一一对应：节点、音节边界、造词码都在全拼坐标里，词典按真实全拼码查。
//!
//! 双拼方案下击键两两配对，每对键取两组字母的笛卡尔积交给双拼配对规则（[`SylGraph::shuangpin`]），
//! 此时击键偏移与全拼偏移不再一一对应：节点的起止、音节边界在击键坐标，造词码仍在全拼坐标。
//!
//! 只在 [`ConvertOptions::keypad`](crate::engine::ConvertOptions::keypad) 有值时进入，
//! 电脑端恒为 `None`，不经过这里。

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use wind_candidate::{Candidate, CandidateMeta, CandidateSource};

use super::PinyinEngine;
use super::fuzzy::{FuzzyConfig, FuzzyMatcher};
use super::lattice::{
    FUZZY_SYLLABLE_LOG_PENALTY, PARTIAL_FINAL_NODE_LIMIT, PARTIAL_FINAL_PENALTY, USER_NODE_BONUS,
    USER_NODE_LIMIT, USER_NODE_WEIGHT_CAP, is_sentence_store_word, score_node,
    score_node_partial_final,
};
use super::shuangpin::{Layout, ShuangpinConverter};
use super::syllable::STANDARD_SYLLABLES;
use super::viterbi::WordNode;
use crate::engine::ConvertResult;

/// 分词键在查询串里的字符。
pub const SEPARATOR: u8 = b'\'';

/// 同一层在途组合的上限：词典前缀剪枝之后仍超出时按生成顺序截断（结果确定）。
const LAYER_BUDGET: usize = 512;

/// 整句里每个词扣的分（对数概率轴）：歧义大时高频单字容易拼满整串，偏向少而长的词。
/// 取值见 `tests/pinyin_eval.rs` 的 `pinyin_eval_t9`。
const NODE_PENALTY: f64 = 3.0;

/// 九宫格简拼最多几个键：每键 1–4 个声母，组合数按键数指数增长。
const ABBREV_MAX_KEYS: usize = 4;

/// 用户词简拼整串召回最多几个键（= 用户词最多几个字）。
const USER_ABBREV_MAX_KEYS: usize = 16;

/// 每个简拼串从索引取的全拼码条数（同一串的码按权重降序）。
const ABBREV_CODES_PER_STROKE: usize = 4;

/// 能作简拼的首字母：声母（zh/ch/sh 取首字母）与零声母音节的开头 a/e/o。i/u/v 不打头。
const ABBREV_LETTERS: &[u8] = b"abcdefghjklmnopqrstwxyz";

/// 不从开头起的节点（只进整句）每个音节码保留的条数。
const INNER_NODE_LIMIT: usize = 16;

/// 键 → 字母集合。字母键恒映射到自身（已锁定），分词键不在表内。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeypadMap {
    /// `masks[c]` 的第 i 位 = 键 `c` 含字母 `'a' + i`。
    masks: [u32; 128],
}

impl KeypadMap {
    /// 标准电话键盘：`2`=abc … `9`=wxyz。
    pub fn t9() -> Self {
        let mut masks = [0u32; 128];
        for (i, slot) in masks[b'a' as usize..=b'z' as usize].iter_mut().enumerate() {
            *slot = 1 << i;
        }
        for (key, letters) in [
            (b'2', "abc"),
            (b'3', "def"),
            (b'4', "ghi"),
            (b'5', "jkl"),
            (b'6', "mno"),
            (b'7', "pqrs"),
            (b'8', "tuv"),
            (b'9', "wxyz"),
        ] {
            masks[key as usize] = letters.bytes().fold(0, |m, b| m | 1 << (b - b'a'));
        }
        Self { masks }
    }

    fn mask(&self, key: u8) -> u32 {
        self.masks.get(key as usize).copied().unwrap_or(0)
    }

    /// `key` 能否代表字母 `letter`（小写 ASCII）。
    pub fn admits(&self, key: u8, letter: u8) -> bool {
        letter.is_ascii_lowercase() && self.mask(key) & (1 << (letter - b'a')) != 0
    }

    /// 字母所在的编码键（`n` → `6`）；本身就是编码键或不在任何键上 → 原样 / `None`。
    pub fn key_of(&self, c: u8) -> Option<u8> {
        if !c.is_ascii_lowercase() {
            return (self.mask(c) != 0).then_some(c);
        }
        (b'0'..=b'9').find(|&k| self.admits(k, c))
    }

    /// 是否是本键盘的编码字符（含分词键）。
    pub fn is_code_char(&self, c: char) -> bool {
        c.is_ascii() && (c as u8 == SEPARATOR || self.mask(c as u8) != 0)
    }

    /// 键 `key` 的字母（升序）；不是编码键时为空。
    pub fn letters(&self, key: u8) -> Vec<u8> {
        (b'a'..=b'z').filter(|&l| self.admits(key, l)).collect()
    }
}

/// 九宫格的预编辑文字。
///
/// `read` 是前 `covered` 个击键的拼音（音节间 `'`）。全拼下字母与击键一一对应，补全过的尾音节
/// 只留已打的字母（`222` 读作 ba'ba 时显示 `ba'b`）；双拼下拼音与击键不对应，原样显示。
/// 其余读不出的击键显示该键的首字母（锁定字母显示自己），用户看到的总是字母。
fn keypad_preedit(read: &str, covered: usize, keys: &Keys, map: &KeypadMap, full: bool) -> String {
    let mut out = String::new();
    if full {
        let mut letters = 0;
        for c in read.chars() {
            if c != SEPARATOR as char {
                if letters == covered {
                    break;
                }
                letters += 1;
            }
            out.push(c);
        }
        while out.ends_with(SEPARATOR as char) {
            out.pop();
        }
    } else {
        out.push_str(read);
    }
    for k in covered..keys.len() {
        if !out.is_empty() && (k == covered || keys.forced[k]) {
            out.push(SEPARATOR as char);
        }
        if let Some(&l) = map.letters(keys.clean[k]).first() {
            out.push(l as char);
        }
    }
    out
}

/// 去掉分词键后的击键，及其与原串的坐标对应。
struct Keys<'a> {
    raw: &'a str,
    clean: Vec<u8>,
    /// `raw_start[k]`：第 k 个击键在原串里的字节偏移；`raw_start[n]` = 原串长度。
    /// 也是「消费到第 k 个击键」时原串的消费长度：紧随其后的分词键并入前一段。
    raw_start: Vec<usize>,
    /// `forced[k]`：第 k 个击键之前有分词键，音节必须从这里开始。
    forced: Vec<bool>,
}

impl<'a> Keys<'a> {
    /// 前 `k` 个击键的规范码（候选的 `code`，词频按它记账）：锁定字母还原成所在的键、去掉分词键。
    /// 左栏锁定读法后查询里是 `ni'426`，记成 `64426`，与直接按数字选的同一个词共用一份词频。
    fn digit_code(&self, k: usize, map: &KeypadMap) -> String {
        self.clean[..k]
            .iter()
            .map(|&c| map.key_of(c).unwrap_or(c) as char)
            .collect()
    }

    /// 前 `n` 个键能否整体当简拼读（每键一个声母）：分词键无妨（每键本就自成音节），锁定的
    /// 字母只能是单个声母——锁成多字母音节（左栏点了 `ma`）的两个键不能再拆成 m、a。
    fn abbrev_compatible(&self, n: usize) -> bool {
        (1..n).all(|i| {
            !(self.clean[i - 1].is_ascii_lowercase()
                && self.clean[i].is_ascii_lowercase()
                && !self.forced[i])
        })
    }

    fn parse(raw: &'a str, map: &KeypadMap) -> Option<Self> {
        let b = raw.as_bytes();
        let mut clean = Vec::with_capacity(b.len());
        let mut raw_start = Vec::with_capacity(b.len() + 1);
        let mut forced = Vec::with_capacity(b.len() + 1);
        let mut sep_before = false;
        for (i, &c) in b.iter().enumerate() {
            if c == SEPARATOR {
                sep_before = true;
                continue;
            }
            if map.mask(c) == 0 {
                return None;
            }
            // 第一个击键前的分词键不构成边界。
            forced.push(sep_before && !clean.is_empty());
            clean.push(c);
            raw_start.push(i);
            sep_before = false;
        }
        raw_start.push(b.len());
        forced.push(false);
        Some(Self {
            raw,
            clean,
            raw_start,
            forced,
        })
    }

    fn len(&self) -> usize {
        self.clean.len()
    }

    /// `[p, q)` 内部没有分词键（端点上可以有）。
    fn open_span(&self, p: usize, q: usize) -> bool {
        !self.forced[p + 1..q].iter().any(|&f| f)
    }

    /// 全拼坐标（相对 `start`）的音节掩码换成原串坐标（相对原串开头）。
    fn raw_mask(&self, start: usize, mask: u64) -> u64 {
        let mut out = 0u64;
        for i in 0..64 {
            if mask >> i & 1 == 1 {
                let r = self.raw_start[start + i];
                if r >= 64 {
                    return 0;
                }
                out |= 1 << r;
            }
        }
        out
    }
}

fn syllables() -> &'static [&'static str] {
    static ALL: OnceLock<Vec<&'static str>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<&'static str> = STANDARD_SYLLABLES.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    })
}

fn matches(map: &KeypadMap, keys: &[u8], letters: &[u8]) -> bool {
    keys.len() == letters.len() && keys.iter().zip(letters).all(|(&k, &l)| map.admits(k, l))
}

/// 标准音节表里的同名音节（`'static`，供音节图引用）；不在表内（模糊错音串等）为 `None`。
fn intern(syl: &str) -> Option<&'static str> {
    let all = syllables();
    all.binary_search(&syl).ok().map(|i| all[i])
}

/// 双拼布局是否只用字母键。九宫格只有字母，布局里有 `;` 等符号键（微软、搜狗、紫光的
/// `;` = ing）时打不全，九宫格下不支持。
pub fn shuangpin_layout_is_letter_only(layout: &Layout) -> bool {
    layout.code_char_set().is_none()
}

/// 音节图上的一条边。
#[derive(Debug, Clone, Copy)]
struct Edge {
    end: usize,
    /// 查词典用的音节（模糊变体时是变体，`94` 读 zi、开了 `zh↔z` 时另有一条 zhi）
    syl: &'static str,
    /// 按键实际拼出的音节（编码栏显示它）；非模糊边与 `syl` 相同。双拼下与 `syl` 相同。
    typed: &'static str,
    /// 模糊改动处数（声母、韵母各算一处），0 = 按键原样拼出。
    edits: usize,
}

/// 击键上的音节图：每个起点出发的完整音节，以及尾部残缺音节的拼法。
struct SylGraph {
    edges: Vec<Vec<Edge>>,
    /// `tails[p]` = 从 `p` 到串尾只打了开头的完整音节
    tails: Vec<Vec<&'static str>>,
    /// `tail_prefixes[p]` = 尾部残缺音节已打出的全拼前缀（查末字用，已去重）
    tail_prefixes: Vec<Vec<String>>,
    /// 末字须是 `tails[p]` 里的音节（双拼：前缀 `s` 不能带出 `sh` 的字）
    strict_tail: bool,
    /// 从 0 出发经完整音节可达
    reachable: Vec<bool>,
}

impl SylGraph {
    /// 全拼：一键一个字母，按 414 个标准音节逐个比对键位。
    ///
    /// `fuzzy` 有值时，每个按键吻合的音节再按用户开的模糊组展开出变体边（同一击键区间，
    /// 查词典用变体）。只展开完整音节，尾部残缺音节不展开：补全本就宽，再叠模糊候选太散。
    fn full(keys: &Keys, map: &KeypadMap, fuzzy: Option<&FuzzyConfig>) -> Self {
        let n = keys.len();
        let mut edges: Vec<Vec<Edge>> = vec![Vec::new(); n + 1];
        let mut tails: Vec<Vec<&'static str>> = vec![Vec::new(); n + 1];
        let mut tail_prefixes: Vec<Vec<String>> = vec![Vec::new(); n + 1];
        for p in 0..n {
            for &syl in syllables() {
                let l = syl.len();
                let s = syl.as_bytes();
                if p + l <= n {
                    if keys.open_span(p, p + l) && matches(map, &keys.clean[p..p + l], s) {
                        edges[p].push(Edge {
                            end: p + l,
                            syl,
                            typed: syl,
                            edits: 0,
                        });
                    }
                } else if keys.open_span(p, n) && matches(map, &keys.clean[p..], &s[..n - p]) {
                    tails[p].push(syl);
                    tail_prefixes[p].push(syl[..n - p].to_string());
                }
            }
            tail_prefixes[p].sort_unstable();
            tail_prefixes[p].dedup();
            if let Some(fuzzy) = fuzzy {
                add_fuzzy_edges(&mut edges[p], fuzzy);
            }
        }
        Self::finish(edges, tails, tail_prefixes, false)
    }

    /// 双拼：每段（`'` 之间）从头起两键一对，每对取两键字母的笛卡尔积，按布局配对规则得音节。
    /// 输入末尾落单的键是残缺声母（只补全，不当完整音节）；段中落单的键（其后紧跟 `'`）
    /// 不成边，图在那里断开。
    fn shuangpin(keys: &Keys, map: &KeypadMap, conv: &ShuangpinConverter) -> Self {
        let n = keys.len();
        let mut edges: Vec<Vec<Edge>> = vec![Vec::new(); n + 1];
        let mut tails: Vec<Vec<&'static str>> = vec![Vec::new(); n + 1];
        let mut tail_prefixes: Vec<Vec<String>> = vec![Vec::new(); n + 1];
        let mut p = 0;
        while p < n {
            let lone = p + 1 >= n || keys.forced[p + 1];
            if lone {
                if p + 1 >= n {
                    for l1 in map.letters(keys.clean[p]) {
                        let initial = conv
                            .layout()
                            .initial_of(l1)
                            .map(str::to_string)
                            .unwrap_or_else(|| (l1 as char).to_string());
                        for &syl in syllables() {
                            if has_initial(syl, &initial) && !tails[p].contains(&syl) {
                                tails[p].push(syl);
                            }
                        }
                        tail_prefixes[p].push(initial);
                    }
                    tail_prefixes[p].sort_unstable();
                    tail_prefixes[p].dedup();
                }
                p += 1;
                continue;
            }
            let mut out: Vec<Edge> = Vec::new();
            for l1 in map.letters(keys.clean[p]) {
                for l2 in map.letters(keys.clean[p + 1]) {
                    for syl in conv.convert_pair(l1, l2) {
                        if let Some(s) = intern(&syl)
                            && !out.iter().any(|e| e.syl == s)
                        {
                            out.push(Edge {
                                end: p + 2,
                                syl: s,
                                typed: s,
                                edits: 0,
                            });
                        }
                    }
                }
            }
            edges[p] = out;
            p += 2;
        }
        Self::finish(edges, tails, tail_prefixes, true)
    }

    fn finish(
        edges: Vec<Vec<Edge>>,
        tails: Vec<Vec<&'static str>>,
        tail_prefixes: Vec<Vec<String>>,
        strict_tail: bool,
    ) -> Self {
        let n = edges.len() - 1;
        let mut reachable = vec![false; n + 1];
        reachable[0] = true;
        for p in 0..n {
            if reachable[p] {
                for e in &edges[p] {
                    reachable[e.end] = true;
                }
            }
        }
        Self {
            edges,
            tails,
            tail_prefixes,
            strict_tail,
            reachable,
        }
    }
}

/// 给同一起点的边追加模糊变体：`(终点, 变体)` 已有边时不重复（精确边优先，变体间取处数少的）。
fn add_fuzzy_edges(edges: &mut Vec<Edge>, fuzzy: &FuzzyConfig) {
    let exact = edges.len();
    for i in 0..exact {
        let base = edges[i];
        for (variant, edits) in FuzzyMatcher::group_variants_scored(base.syl, fuzzy) {
            let Some(syl) = intern(&variant) else {
                continue;
            };
            match edges.iter_mut().find(|e| e.end == base.end && e.syl == syl) {
                Some(old) if old.edits > edits => *old = Edge { syl, edits, ..base },
                Some(_) => {}
                None => edges.push(Edge { syl, edits, ..base }),
            }
        }
    }
}

/// 音节 `syl` 的声母恰是 `initial`（零声母时 `initial` 是首字母，如 `a` → a/ai/an…）。
/// `s` / `c` / `z` 不含翘舌的 `sh` / `ch` / `zh`。
fn has_initial(syl: &str, initial: &str) -> bool {
    syl.starts_with(initial)
        && !(matches!(initial, "s" | "c" | "z") && syl.as_bytes().get(1) == Some(&b'h'))
}

/// 候选 `top` 按键实际拼出的读音（音节间 `'`），只在它含模糊读出的音节时返回。
///
/// 按候选的音节边界（原串坐标）把击键切段，逐段在音节图里找「同终点、查词音节等于候选
/// 该音节」的边，换成它的 `typed`；尾部补全的残缺音节没有边，原样保留。
fn typed_reading(graph: &SylGraph, keys: &Keys, top: &Candidate) -> Option<String> {
    let canon: Vec<&str> = top.pinyin.split(SEPARATOR as char).collect();
    let starts: Vec<usize> = (0..keys.len())
        .filter(|&k| {
            let r = keys.raw_start[k];
            r < 64 && top.boundary >> r & 1 == 1
        })
        .collect();
    if starts.len() != canon.len() {
        return None;
    }
    let covered = keys.raw_start[..keys.len()]
        .iter()
        .filter(|&&s| s < top.consumed_length)
        .count();
    let mut out: Vec<&str> = Vec::with_capacity(canon.len());
    let mut fuzzy = false;
    for (i, (&s, &syl)) in starts.iter().zip(&canon).enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(covered);
        match graph.edges[s].iter().find(|e| e.end == end && e.syl == syl) {
            Some(e) => {
                fuzzy |= e.edits > 0;
                out.push(e.typed);
            }
            None => out.push(syl),
        }
    }
    fuzzy.then(|| out.join("'"))
}

/// 一个词节点（全拼坐标）。
#[derive(Debug, Clone)]
struct Node {
    start: usize,
    end: usize,
    text: String,
    /// 词条的全拼码与音节掩码（全拼坐标，相对码起点）
    code: String,
    mask: u64,
    /// 音节起点的击键掩码（击键坐标，相对 `start`）。全拼下等于 `mask`。
    key_mask: u64,
    weight: i32,
    log_prob: f64,
    is_user: bool,
    /// 末音节是补全出来的（尾部残缺音节）
    completed_tail: bool,
    /// 模糊改动处数（各音节累加）
    fuzzy_edits: usize,
}

/// 从同一起点出发、逐音节延伸的在途组合。
struct Combo {
    pos: usize,
    code: String,
    mask: u64,
    key_mask: u64,
    syls: usize,
    fuzzy_edits: usize,
}

fn bit_at(len: usize) -> u64 {
    if len < 64 { 1 << len } else { 0 }
}

/// 按掩码把全拼码切成 `ni'hao`。
fn split_pinyin(code: &str, mask: u64) -> String {
    let mut out = String::with_capacity(code.len() + 4);
    for (i, ch) in code.char_indices() {
        if i > 0 && i < 64 && mask >> i & 1 == 1 {
            out.push('\'');
        }
        out.push(ch);
    }
    out
}

impl PinyinEngine {
    /// 本方案能否用九宫格输入：全拼恒可；双拼只限纯字母布局（见 [`shuangpin_layout_is_letter_only`]）。
    pub fn keypad_supported(&self) -> bool {
        self.shuangpin
            .as_ref()
            .is_none_or(|c| shuangpin_layout_is_letter_only(c.layout()))
    }

    /// 九宫格左栏逐段选拼音的读法：落在 `unit` 开头那几个键上的完整音节，加上首键当声母的
    /// 单字母（`64` → ni / mi / mo / o / n / m）。只做全拼：双拼的拼音与击键不一一对应。
    ///
    /// 顺序：音节按该音节最常用单字的权重降序（长短不论，`yi` 排在 `zhi` 前），单字母声母殿后。
    pub fn keypad_readings(&self, unit: &str, map: &KeypadMap) -> Vec<String> {
        if self.shuangpin.is_some() {
            return Vec::new();
        }
        let keys = unit.as_bytes();
        let Some(&first) = keys.first() else {
            return Vec::new();
        };
        let fits = |syl: &str| {
            syl.len() <= keys.len() && syl.bytes().zip(keys).all(|(l, &k)| map.admits(k, l))
        };
        let mut syllables: Vec<(String, i32)> = STANDARD_SYLLABLES
            .iter()
            .filter(|s| fits(s))
            .map(|s| {
                let w = self
                    .dict
                    .search_with_boundary(s)
                    .into_iter()
                    .filter(|h| h.text.chars().count() == 1)
                    .map(|h| h.weight)
                    .max()
                    .unwrap_or(i32::MIN);
                (s.to_string(), w)
            })
            .collect();
        syllables.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut out: Vec<String> = syllables.into_iter().map(|(s, _)| s).collect();
        for l in map.letters(first) {
            let s = (l as char).to_string();
            if ABBREV_LETTERS.contains(&l) && !out.contains(&s) {
                out.push(s);
            }
        }
        out
    }

    /// 九宫格用的模糊组：只取用户开的声母/韵母组。换位纠错针对 26 键的手误，九宫格上
    /// 同键位的错拼本就读得出，不叠；双拼同键音节（`shared_key`）全拼方案下恒空。
    /// 全关时为 `None`，音节图不展开。
    fn keypad_fuzzy(&self) -> Option<FuzzyConfig> {
        let cfg = FuzzyConfig {
            shared_key: HashMap::new(),
            correction: Default::default(),
            misread: None,
            ..self.fuzzy_config.clone()
        };
        cfg.any_enabled().then_some(cfg)
    }

    /// 歧义键盘输入的转换（全拼）。见模块文档。
    pub(crate) fn convert_keypad(
        &self,
        input: &str,
        max_candidates: usize,
        map: &KeypadMap,
    ) -> ConvertResult {
        if !self.keypad_supported() {
            return ConvertResult::default();
        }
        let Some(keys) = Keys::parse(input, map) else {
            return ConvertResult::default();
        };
        let n = keys.len();
        if n == 0 {
            return ConvertResult::default();
        }
        let graph = match &self.shuangpin {
            Some(conv) => SylGraph::shuangpin(&keys, map, conv),
            None => SylGraph::full(&keys, map, self.keypad_fuzzy().as_ref()),
        };
        let mut nodes = self.keypad_nodes(&keys, &graph, max_candidates.max(1));
        let blocked = self.blocked_words();
        if !blocked.is_empty() {
            nodes.retain(|nd| !blocked.contains(&nd.text));
        }

        let mut cands: Vec<Candidate> = Vec::new();
        let mut preedit_source: Option<String> = None;

        // 整句：所有节点上的 Viterbi，至少两个词才算组句。
        let mut lattice: Vec<Vec<WordNode>> = vec![Vec::new(); n + 1];
        for nd in &nodes {
            lattice[nd.end].push(WordNode {
                start: nd.start,
                end: nd.end,
                word: nd.text.clone(),
                syl_mask: nd.key_mask,
                log_prob: nd.log_prob - NODE_PENALTY,
                canon: Some((nd.code.clone(), nd.mask)),
                aux: 0,
            });
        }
        let clean = String::from_utf8_lossy(&keys.clean).into_owned();
        let result = self.decode_unblocked(&lattice, n);
        if result.log_prob.is_finite()
            && !result.words.is_empty()
            && let Some((code, mask)) = result.learn_code(&clean)
        {
            preedit_source = Some(split_pinyin(&code, mask));
            if result.words.len() >= 2 {
                cands.push(Candidate {
                    text: result.words.concat(),
                    pinyin: split_pinyin(&code, mask),
                    code: keys.digit_code(keys.len(), map),
                    weight: super::sentence_weight(result.log_prob, result.words.len()),
                    source: CandidateSource::Pinyin,
                    is_sentence: true,
                    is_synthesized: true,
                    boundary: keys.raw_mask(0, result.boundary),
                    consumed_length: keys.raw.len(),
                    meta: CandidateMeta {
                        learn_code: Some((code, mask)),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
        }

        // 从开头起的词：吃得多的在前，同长按权重。
        let mut heads: Vec<&Node> = nodes.iter().filter(|nd| nd.start == 0).collect();
        heads.sort_by(|a, b| {
            b.end
                .cmp(&a.end)
                .then(a.completed_tail.cmp(&b.completed_tail))
                .then(
                    super::fuzzy_penalized(b.weight, b.fuzzy_edits)
                        .cmp(&super::fuzzy_penalized(a.weight, a.fuzzy_edits)),
                )
                .then(a.text.cmp(&b.text))
        });
        let mut seen: HashSet<&str> = cands.iter().map(|c| c.text.as_str()).collect();
        let mut words: Vec<Candidate> = Vec::new();
        for nd in heads {
            if cands.len() + words.len() >= max_candidates {
                break;
            }
            if !seen.insert(nd.text.as_str()) {
                continue;
            }
            let consumed = keys.raw_start[nd.end];
            words.push(Candidate {
                text: nd.text.clone(),
                pinyin: split_pinyin(&nd.code, nd.mask),
                code: keys.digit_code(nd.end, map),
                weight: super::fuzzy_penalized(nd.weight, nd.fuzzy_edits),
                source: CandidateSource::Pinyin,
                is_fuzzy: nd.fuzzy_edits > 0,
                is_prefix: nd.completed_tail,
                is_partial: nd.end < n,
                boundary: keys.raw_mask(0, nd.key_mask),
                consumed_length: consumed,
                meta: CandidateMeta {
                    is_user_dict: nd.is_user,
                    learn_code: Some((nd.code.clone(), nd.mask)),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        // 整句让位于吃满整串的词典词（同全拼的整句让位）：九宫格下高频单字很容易凑满整串。
        let best_whole = words
            .iter()
            .filter(|c| !c.is_partial && !c.is_prefix)
            .map(|c| c.weight)
            .max();
        if let (Some(m), Some(s)) = (best_whole, cands.first_mut().filter(|c| c.is_sentence))
            && s.weight >= m
        {
            s.weight = m.saturating_sub(1);
            s.is_sentence_demoted = true;
        }
        cands.extend(words);
        if self.shuangpin.is_none() {
            let n = keys.len();
            if n <= ABBREV_MAX_KEYS {
                self.add_keypad_abbrevs(&keys, map, n, max_candidates, &mut cands);
            }
            // 没有吃满整串的候选（`99999999` 读不成音节）：从开头起按简拼分步选——前 2..=4 键的
            // 简拼词、首键的单字，选掉一段后剩下的键接着解。读得通的串不走这里，不多花时间。
            let step = !cands.iter().any(|c| c.consumed_length == keys.raw.len());
            // 用户造过的词（分步选完自动造的 `真正意义主要原因`）按简拼整串召回，放在分步候选
            // 之前判出 `step`：有了用户词仍给分步候选，用户想换一种分法时还有得选。
            self.add_keypad_user_abbrevs(&keys, map, max_candidates, &mut cands);
            if step {
                for l in (2..=ABBREV_MAX_KEYS.min(n.saturating_sub(1))).rev() {
                    self.add_keypad_abbrevs(&keys, map, l, max_candidates, &mut cands);
                }
                self.add_leading_initial_chars(&keys, map, max_candidates, &mut cands);
            }
        }
        cands.truncate(max_candidates);

        // 预编辑：读得出的部分显示拼音，读不出的尾键显示该键的首字母——任何时候都不出数字。
        // 跟着显示首选走（候选栏按 `candidate_display_order` 排，与引擎内顺序不同），编码栏与
        // 首选的读音才对得上；没有候选时退回整句路径。
        let shown = cands
            .iter()
            .min_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, keys.raw));
        let (read, covered) = match (shown, &preedit_source) {
            (None, Some(p)) => (p.clone(), n),
            // 简拼首选：每键一个声母，显示声母（`w'm`）而不是整个词的全拼
            (Some(top), _) if top.is_abbrev => {
                let initials: Vec<&str> = top
                    .pinyin
                    .split(SEPARATOR as char)
                    .filter_map(|syl| syl.get(..1))
                    .collect();
                let covered = initials.len();
                (initials.join("'"), covered)
            }
            (Some(top), _) => {
                let covered = keys.raw_start[..n]
                    .iter()
                    .filter(|&&s| s < top.consumed_length)
                    .count();
                (top.pinyin.clone(), covered)
            }
            (None, None) => (String::new(), 0),
        };
        // 模糊读出的首选（`94` 出「知」）编码栏仍显示按键拼出的 `zi`，不显示 `zh`。
        let read = shown
            .and_then(|top| typed_reading(&graph, &keys, top))
            .unwrap_or(read);
        let preedit = keypad_preedit(&read, covered, &keys, map, self.shuangpin.is_none());
        ConvertResult {
            is_empty: cands.is_empty(),
            candidates: cands,
            preedit_pinyin: preedit.clone(),
            preedit_display: preedit,
            ..Default::default()
        }
    }

    /// 简拼（全拼九宫格）：前 `len` 个键（2..=[`ABBREV_MAX_KEYS`]）每键读作一个声母时的词，
    /// 如 `96` → w'm → 我们。`len` 小于键数时候选只吃这一段（分步上屏，剩下的键接着解）。
    ///
    /// 每个键取它能代表的声母首字母（`9` → w/x/y/z），组合后查简拼索引（同 26 键的纯简拼
    /// step5：索引给全拼码，回主表装配，音节数必须等于键数）。候选标 `is_abbrev`：显示排序里
    /// 排在同长度的全拼读法之后。这一段里有分词键或锁定字母时不走这里。
    fn add_keypad_abbrevs(
        &self,
        keys: &Keys,
        map: &KeypadMap,
        len: usize,
        max_candidates: usize,
        cands: &mut Vec<Candidate>,
    ) {
        let n = len;
        if !self.config.enable_abbrev
            || !(2..=ABBREV_MAX_KEYS).contains(&n)
            || n > keys.len()
            || !keys.abbrev_compatible(n)
        {
            return;
        }
        let consumed = keys.raw_start[n];
        let per_key: Vec<Vec<u8>> = keys.clean[..n]
            .iter()
            .map(|&k| {
                map.letters(k)
                    .into_iter()
                    .filter(|l| ABBREV_LETTERS.contains(l))
                    .collect()
            })
            .collect();
        if per_key.iter().any(Vec::is_empty) {
            return;
        }
        let dict = &self.dict;
        let all_syllables = (1u64 << n) - 1;
        let mut seen: HashSet<String> = cands.iter().map(|c| c.text.clone()).collect();
        let mut found: Vec<Candidate> = Vec::new();
        let mut combo = vec![0usize; n];
        loop {
            let stroke: String = combo
                .iter()
                .zip(&per_key)
                .map(|(&i, ls)| ls[i] as char)
                .collect();
            for code in dict.search_abbrev(&stroke, ABBREV_CODES_PER_STROKE) {
                for h in dict.search_with_boundary(&code) {
                    let eb = super::effective_boundary(&code, h.boundary, &self.trie);
                    if eb.count_ones() as usize != n || !seen.insert(h.text.clone()) {
                        continue;
                    }
                    found.push(Candidate {
                        pinyin: split_pinyin(&code, eb),
                        text: h.text,
                        code: keys.digit_code(n, map),
                        weight: h.weight,
                        source: CandidateSource::Pinyin,
                        is_abbrev: true,
                        is_partial: consumed < keys.raw.len(),
                        boundary: keys.raw_mask(0, all_syllables),
                        consumed_length: consumed,
                        meta: CandidateMeta {
                            learn_code: Some((code.clone(), eb)),
                            ..Default::default()
                        },
                        ..Default::default()
                    });
                }
            }
            // 下一个组合（逐位进位）
            let mut i = 0;
            loop {
                if i == n {
                    found.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.text.cmp(&b.text)));
                    found.truncate(max_candidates);
                    cands.extend(found);
                    return;
                }
                combo[i] += 1;
                if combo[i] < per_key[i].len() {
                    break;
                }
                combo[i] = 0;
                i += 1;
            }
        }
    }

    /// 用户词 / 临时词的简拼整串召回：每个音节的首字母都落在对应的键上、音节数等于键数。
    ///
    /// 用户层的简拼索引只能按完整声母串精确查，8 个键的声母组合有几万种，逐个查不现实；
    /// 改按首键的几个字母各做一次前缀查询，再逐条核对。用户词库规模有限，开销可控。
    /// 系统词的简拼另走 [`Self::add_keypad_abbrevs`]（有独立索引，且限 4 键）。
    fn add_keypad_user_abbrevs(
        &self,
        keys: &Keys,
        map: &KeypadMap,
        max_candidates: usize,
        cands: &mut Vec<Candidate>,
    ) {
        let n = keys.len();
        let Some(store) = self.store_layers.as_deref() else {
            return;
        };

        if !self.config.enable_abbrev
            || !(2..=USER_ABBREV_MAX_KEYS).contains(&n)
            || !keys.abbrev_compatible(n)
        {
            return;
        }
        let mut seen: HashSet<String> = cands.iter().map(|c| c.text.clone()).collect();
        let mut found: Vec<Candidate> = Vec::new();
        for first in map.letters(keys.clean[0]) {
            let prefix = (first as char).to_string();
            // 不限条数（0）：临时词权重低，截断会先截掉它们。
            let all = store.search_prefix(&prefix, 0);
            for c in all {
                if !is_sentence_store_word(&c) || c.text.chars().count() != n {
                    continue;
                }
                let eb = super::effective_boundary(&c.code, c.boundary, &self.trie);
                let initials: Vec<u8> = c
                    .code
                    .bytes()
                    .enumerate()
                    .filter(|(i, _)| *i < 64 && (eb >> i) & 1 == 1)
                    .map(|(_, b)| b)
                    .collect();
                if initials.len() != n
                    || initials
                        .iter()
                        .zip(&keys.clean)
                        .any(|(&l, &k)| !map.admits(k, l))
                    || !seen.insert(c.text.clone())
                {
                    continue;
                }
                found.push(Candidate {
                    pinyin: split_pinyin(&c.code, eb),
                    text: c.text,
                    code: keys.digit_code(n, map),
                    weight: c.weight,
                    source: CandidateSource::Pinyin,
                    is_abbrev: true,
                    boundary: keys.raw_mask(0, (1u64 << n) - 1),
                    consumed_length: keys.raw.len(),
                    meta: CandidateMeta {
                        is_user_dict: c.meta.is_user_dict,
                        is_temp_dict: c.meta.is_temp_dict,
                        learn_code: Some((c.code, eb)),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
        }
        found.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.text.cmp(&b.text)));
        found.truncate(max_candidates);
        cands.extend(found);
    }

    /// 首键当声母的单字（分步上屏的兜底）：整串读不通时至少能先选掉第一个字。
    fn add_leading_initial_chars(
        &self,
        keys: &Keys,
        map: &KeypadMap,
        max_candidates: usize,
        cands: &mut Vec<Candidate>,
    ) {
        let Some(&k) = keys.clean.first() else {
            return;
        };
        let consumed = keys.raw_start[1];
        let mut seen: HashSet<String> = cands.iter().map(|c| c.text.clone()).collect();
        let mut found: Vec<Candidate> = Vec::new();
        for l in map.letters(k) {
            let prefix = (l as char).to_string();
            for hit in self.dict.search_prefix_with_boundary_syllable_capped(
                &prefix,
                PARTIAL_FINAL_NODE_LIMIT,
                1,
                0,
            ) {
                if hit.text.chars().count() != 1 || !seen.insert(hit.text.clone()) {
                    continue;
                }
                found.push(Candidate {
                    pinyin: hit.code.clone(),
                    text: hit.text,
                    code: keys.digit_code(1, map),
                    weight: hit.weight,
                    source: CandidateSource::Pinyin,
                    is_prefix: true,
                    is_partial: consumed < keys.raw.len(),
                    boundary: keys.raw_mask(0, 1),
                    consumed_length: consumed,
                    meta: CandidateMeta {
                        learn_code: Some((hit.code, 1)),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
        }
        found.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.text.cmp(&b.text)));
        found.truncate(max_candidates);
        cands.extend(found);
    }

    /// 全部词节点：完整音节的词、尾部残缺音节的单字与（从开头起的）补全词。
    fn keypad_nodes(&self, keys: &Keys, graph: &SylGraph, max_candidates: usize) -> Vec<Node> {
        let n = keys.len();
        let max_syls = self.lattice_builder.max_word_len();
        let store = self
            .store_layers
            .as_deref()
            .filter(|_| self.config.sentence_uses_user_words);
        let mut by_key: HashMap<(usize, usize, String), Node> = HashMap::new();
        let mut add = |nd: Node| {
            let key = (nd.start, nd.end, nd.text.clone());
            match by_key.get_mut(&key) {
                Some(old) if old.log_prob >= nd.log_prob => {}
                Some(old) => *old = nd,
                None => {
                    by_key.insert(key, nd);
                }
            }
        };

        for p in 0..n {
            if !graph.reachable[p] {
                continue;
            }
            let mut layer = vec![Combo {
                pos: p,
                code: String::new(),
                mask: 0,
                key_mask: 0,
                syls: 0,
                fuzzy_edits: 0,
            }];
            for _ in 0..max_syls {
                let mut next: Vec<Combo> = Vec::new();
                for c in &layer {
                    // 从开头起的组合走到残缺尾巴前：补全成「已打音节 + 残缺音节」的词。
                    if p == 0 && c.syls > 0 {
                        self.add_tail_words(keys, graph, c, &mut add);
                    }
                    for e in &graph.edges[c.pos] {
                        next.push(Combo {
                            pos: e.end,
                            code: format!("{}{}", c.code, e.syl),
                            mask: c.mask | bit_at(c.code.len()),
                            key_mask: c.key_mask | bit_at(c.pos - p),
                            syls: c.syls + 1,
                            fuzzy_edits: c.fuzzy_edits + e.edits,
                        });
                    }
                }
                if next.is_empty() {
                    break;
                }
                next.truncate(LAYER_BUDGET);
                for c in &next {
                    let mut hits = self.dict.search_with_boundary(&c.code);
                    // 不从开头起的节点只进整句，同一个码取权重最高的几条就够。
                    // 从开头起的节点是候选，每个码超出候选上限的部分不可能排进列表。
                    let cap = if p > 0 {
                        INNER_NODE_LIMIT
                    } else {
                        max_candidates
                    };
                    if hits.len() > cap {
                        hits.select_nth_unstable_by(cap, |a, b| b.weight.cmp(&a.weight));
                        hits.truncate(cap);
                    }
                    let fuzzy_cost = FUZZY_SYLLABLE_LOG_PENALTY * c.fuzzy_edits as f64;
                    for hit in hits {
                        if hit.boundary != 0 && hit.boundary != c.mask {
                            continue;
                        }
                        add(Node {
                            start: p,
                            end: c.pos,
                            log_prob: score_node(&hit.text, &c.code, hit.weight) - fuzzy_cost,
                            text: hit.text,
                            code: c.code.clone(),
                            mask: c.mask,
                            key_mask: c.key_mask,
                            weight: hit.weight,
                            is_user: false,
                            completed_tail: false,
                            fuzzy_edits: c.fuzzy_edits,
                        });
                    }
                    let Some(store) = store else { continue };
                    for cand in store.search(&c.code, USER_NODE_LIMIT) {
                        if !is_sentence_store_word(&cand) || cand.boundary != c.mask {
                            continue;
                        }
                        let weight = cand.weight.min(USER_NODE_WEIGHT_CAP);
                        add(Node {
                            start: p,
                            end: c.pos,
                            log_prob: score_node(&cand.text, &c.code, weight) + USER_NODE_BONUS
                                - fuzzy_cost,
                            text: cand.text,
                            code: c.code.clone(),
                            mask: c.mask,
                            key_mask: c.key_mask,
                            weight: cand.weight,
                            is_user: true,
                            completed_tail: false,
                            fuzzy_edits: c.fuzzy_edits,
                        });
                    }
                }
                next.retain(|c| self.dict.has_longer_code(&c.code));
                layer = next;
            }

            // 尾部残缺音节的单字（整句的末字）：按已打的那几个字母前缀查。
            for prefix in &graph.tail_prefixes[p] {
                for hit in self.dict.search_prefix_with_boundary_syllable_capped(
                    prefix,
                    PARTIAL_FINAL_NODE_LIMIT,
                    1,
                    0,
                ) {
                    if hit.text.chars().count() != 1 {
                        continue;
                    }
                    if graph.strict_tail && !graph.tails[p].contains(&hit.code.as_str()) {
                        continue;
                    }
                    add(Node {
                        start: p,
                        end: n,
                        log_prob: score_node_partial_final(&hit.text, hit.weight)
                            - PARTIAL_FINAL_PENALTY,
                        text: hit.text,
                        mask: if hit.boundary == 0 { 1 } else { hit.boundary },
                        key_mask: 1,
                        code: hit.code,
                        weight: hit.weight,
                        is_user: false,
                        completed_tail: true,
                        fuzzy_edits: 0,
                    });
                }
            }
        }

        let mut out: Vec<Node> = by_key.into_values().collect();
        // HashMap 的次序不确定：固定下来，保证候选与整句可复现。
        out.sort_by(|a, b| {
            (a.start, a.end, &a.text)
                .cmp(&(b.start, b.end, &b.text))
                .then(b.log_prob.total_cmp(&a.log_prob))
        });
        out
    }

    /// 组合 `c`（从开头起、已走到 `c.pos`）后接尾部残缺音节的多音节词。
    ///
    /// 逐个补全成完整音节再精确查：按前缀扫描要遍历整棵子树（`xian…` 下的词极多）。
    fn add_tail_words(&self, keys: &Keys, graph: &SylGraph, c: &Combo, add: &mut impl FnMut(Node)) {
        let n = keys.len();
        if c.pos >= n {
            return;
        }
        let mask = c.mask | bit_at(c.code.len());
        let key_mask = c.key_mask | bit_at(c.pos);
        for &syl in &graph.tails[c.pos] {
            let code = format!("{}{syl}", c.code);
            for hit in self.dict.search_with_boundary(&code) {
                if hit.boundary != mask {
                    continue;
                }
                add(Node {
                    start: 0,
                    end: n,
                    log_prob: score_node(&hit.text, &code, hit.weight)
                        - PARTIAL_FINAL_PENALTY
                        - FUZZY_SYLLABLE_LOG_PENALTY * c.fuzzy_edits as f64,
                    text: hit.text,
                    code: code.clone(),
                    mask,
                    key_mask,
                    weight: hit.weight,
                    is_user: false,
                    completed_tail: true,
                    fuzzy_edits: c.fuzzy_edits,
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "t9_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "t9_sp_tests.rs"]
mod sp_tests;
