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
use super::lattice::{
    PARTIAL_FINAL_NODE_LIMIT, PARTIAL_FINAL_PENALTY, USER_NODE_BONUS, USER_NODE_LIMIT,
    USER_NODE_WEIGHT_CAP, is_sentence_store_word, score_node, score_node_partial_final,
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

/// 击键上的音节图：每个起点出发的完整音节，以及尾部残缺音节的拼法。
struct SylGraph {
    /// `edges[p]` = `(终点, 音节)`
    edges: Vec<Vec<(usize, &'static str)>>,
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
    fn full(keys: &Keys, map: &KeypadMap) -> Self {
        let n = keys.len();
        let mut edges = vec![Vec::new(); n + 1];
        let mut tails: Vec<Vec<&'static str>> = vec![Vec::new(); n + 1];
        let mut tail_prefixes: Vec<Vec<String>> = vec![Vec::new(); n + 1];
        for p in 0..n {
            for &syl in syllables() {
                let l = syl.len();
                let s = syl.as_bytes();
                if p + l <= n {
                    if keys.open_span(p, p + l) && matches(map, &keys.clean[p..p + l], s) {
                        edges[p].push((p + l, syl));
                    }
                } else if keys.open_span(p, n) && matches(map, &keys.clean[p..], &s[..n - p]) {
                    tails[p].push(syl);
                    tail_prefixes[p].push(syl[..n - p].to_string());
                }
            }
            tail_prefixes[p].sort_unstable();
            tail_prefixes[p].dedup();
        }
        Self::finish(edges, tails, tail_prefixes, false)
    }

    /// 双拼：每段（`'` 之间）从头起两键一对，每对取两键字母的笛卡尔积，按布局配对规则得音节。
    /// 输入末尾落单的键是残缺声母（只补全，不当完整音节）；段中落单的键（其后紧跟 `'`）
    /// 不成边，图在那里断开。
    fn shuangpin(keys: &Keys, map: &KeypadMap, conv: &ShuangpinConverter) -> Self {
        let n = keys.len();
        let mut edges: Vec<Vec<(usize, &'static str)>> = vec![Vec::new(); n + 1];
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
            let mut out: Vec<(usize, &'static str)> = Vec::new();
            for l1 in map.letters(keys.clean[p]) {
                for l2 in map.letters(keys.clean[p + 1]) {
                    for syl in conv.convert_pair(l1, l2) {
                        if let Some(s) = intern(&syl)
                            && !out.iter().any(|&(_, o)| o == s)
                        {
                            out.push((p + 2, s));
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
        edges: Vec<Vec<(usize, &'static str)>>,
        tails: Vec<Vec<&'static str>>,
        tail_prefixes: Vec<Vec<String>>,
        strict_tail: bool,
    ) -> Self {
        let n = edges.len() - 1;
        let mut reachable = vec![false; n + 1];
        reachable[0] = true;
        for p in 0..n {
            if reachable[p] {
                for &(q, _) in &edges[p] {
                    reachable[q] = true;
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

/// 音节 `syl` 的声母恰是 `initial`（零声母时 `initial` 是首字母，如 `a` → a/ai/an…）。
/// `s` / `c` / `z` 不含翘舌的 `sh` / `ch` / `zh`。
fn has_initial(syl: &str, initial: &str) -> bool {
    syl.starts_with(initial)
        && !(matches!(initial, "s" | "c" | "z") && syl.as_bytes().get(1) == Some(&b'h'))
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
}

/// 从同一起点出发、逐音节延伸的在途组合。
struct Combo {
    pos: usize,
    code: String,
    mask: u64,
    key_mask: u64,
    syls: usize,
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
            None => SylGraph::full(&keys, map),
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
                    code: keys.raw.to_string(),
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
                .then(b.weight.cmp(&a.weight))
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
                code: keys.raw[..consumed].to_string(),
                weight: nd.weight,
                source: CandidateSource::Pinyin,
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
        cands.truncate(max_candidates);

        // 预编辑：读得出的部分显示拼音，读不出的尾键显示该键的首字母——任何时候都不出数字。
        // 跟着显示首选走（候选栏按 `candidate_display_order` 排，与引擎内顺序不同），编码栏与
        // 首选的读音才对得上；没有候选时退回整句路径。
        let shown = cands
            .iter()
            .min_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, keys.raw));
        let (read, covered) = match (shown, &preedit_source) {
            (None, Some(p)) => (p.clone(), n),
            (Some(top), _) => {
                let covered = keys.raw_start[..n]
                    .iter()
                    .filter(|&&s| s < top.consumed_length)
                    .count();
                (top.pinyin.clone(), covered)
            }
            (None, None) => (String::new(), 0),
        };
        let preedit = keypad_preedit(&read, covered, &keys, map, self.shuangpin.is_none());
        ConvertResult {
            is_empty: cands.is_empty(),
            candidates: cands,
            preedit_pinyin: preedit.clone(),
            preedit_display: preedit,
            ..Default::default()
        }
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
            }];
            for _ in 0..max_syls {
                let mut next: Vec<Combo> = Vec::new();
                for c in &layer {
                    // 从开头起的组合走到残缺尾巴前：补全成「已打音节 + 残缺音节」的词。
                    if p == 0 && c.syls > 0 {
                        self.add_tail_words(keys, graph, c, &mut add);
                    }
                    for &(q, syl) in &graph.edges[c.pos] {
                        next.push(Combo {
                            pos: q,
                            code: format!("{}{syl}", c.code),
                            mask: c.mask | bit_at(c.code.len()),
                            key_mask: c.key_mask | bit_at(c.pos - p),
                            syls: c.syls + 1,
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
                    for hit in hits {
                        if hit.boundary != 0 && hit.boundary != c.mask {
                            continue;
                        }
                        add(Node {
                            start: p,
                            end: c.pos,
                            log_prob: score_node(&hit.text, &c.code, hit.weight),
                            text: hit.text,
                            code: c.code.clone(),
                            mask: c.mask,
                            key_mask: c.key_mask,
                            weight: hit.weight,
                            is_user: false,
                            completed_tail: false,
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
                            log_prob: score_node(&cand.text, &c.code, weight) + USER_NODE_BONUS,
                            text: cand.text,
                            code: c.code.clone(),
                            mask: c.mask,
                            key_mask: c.key_mask,
                            weight: cand.weight,
                            is_user: true,
                            completed_tail: false,
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
                    log_prob: score_node(&hit.text, &code, hit.weight) - PARTIAL_FINAL_PENALTY,
                    text: hit.text,
                    code: code.clone(),
                    mask,
                    key_mask,
                    weight: hit.weight,
                    is_user: false,
                    completed_tail: true,
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
