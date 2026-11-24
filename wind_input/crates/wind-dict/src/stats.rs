//! 系统词库统计（设置页「词库统计」）：纯计算，输入按码分组的条目流，输出各项计数。
//!
//! 条目来源是 [`crate::cached::for_each_code_group`]：同一编码在各库里的条目合成一组送进
//! [`StatsBuilder::push_group`]，重码、同码内序位都在组内算完，不物化全表。去重集合借词库
//! 的 `&str`（mmap 上的字节），算完即随 builder 释放，只留下 [`DictStats`] 这份结果。
//!
//! # 口径（界面 ⓘ 与文档站同此）
//!
//! - **条目**：一条（编码, 文字）。各库分行时按库内原样计；合计按 (编码, 文字) 跨库去重。
//! - **单字 / 词组 / 其他**：文字去掉首尾空白后，全是汉字（CJK 统一汉字含各扩展区、兼容区、
//!   私用区与「〇」）时按字数分单字（1）与词组（≥2）；含任何别的字符（emoji、符号、字母）
//!   计「其他」，不进字数分档。英文方案不分这三类，按字母数分档。
//! - **重码组**：一个编码下有 ≥2 个不同文字。「涉及条目」= 各组大小之和，「非首选」=
//!   Σ(组大小 − 1)，「最大重码」= 最大组大小。**只算含汉字的条目**（英文方案全算）：纯
//!   emoji / 符号条目不参与重码，也不参与同码排位——五笔把 71 个表情挂在 `emoj` 一个码上，
//!   算进来「最大重码」就成了 71、重码 Top 全是表情，而它们并不跟汉字抢位。「T恤」「卡拉OK」
//!   这类含汉字的真词照算。合计跨库算一份，主库另算一份（[`DupStats::main`]）。
//! - **序位**：静态排序（`base_sort = weight` 时权重降 → 库档位升 → 自然序升，`natural`
//!   时只看库档位与自然序），**未计**出简让全、单字模式、字符集过滤、调频与候选调整。
//! - **全码**：一个字的最长码（不超过 `max_code_length`，码长按字符数）；同字有多个最长码时
//!   取序位最前者，再按码字典序。比全码短的码是简码，级别 = 码长。
//! - **动态选重率**：Σ_{c 最优码序位 > 0} f(c) / Σ f(c)，c 取字集内已收录的字，最优码 =
//!   各码中 (序位, 码长) 最小者；平均码长 = Σ f(c)·len(最优码) / Σ f(c)。

use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

use serde::Serialize;

use crate::cached::GroupEntry;
use crate::weight_norm::WeightNorm;

/// 统计算法版本：口径一变就加一，进缓存键，旧结果自然失效。
pub const STATS_ALGO_VERSION: u32 = 2;

/// 重码 Top 列表里每组最多带几个成员（拼音同音组可达上千）。
pub const TOP_MEMBERS_MAX: usize = 40;

/// 字集「未收字」最多列几个（拼音大字表下通常为 0，码表常见几百）。
pub const MISSING_CHARS_MAX: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatsKind {
    Codetable,
    Pinyin,
    English,
}

/// 同码内的静态排序口径，对应码表的 `base_sort`。拼音恒按权重。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RankOrder {
    #[default]
    Weight,
    Natural,
}

/// 一个输入库的排序参数（下标与 [`GroupEntry::layer`] 对应）。
#[derive(Clone, Copy, Debug, Default)]
pub struct StatsLayer {
    pub base_order: i32,
    pub default_weight: Option<i32>,
    pub weight_norm: Option<WeightNorm>,
}

impl StatsLayer {
    /// 与 `SystemDictLayer::effective_weight` 同一换算（不含越界告警）。
    fn effective_weight(&self, raw: i32) -> i32 {
        match (self.default_weight, &self.weight_norm) {
            (Some(w), _) => w,
            (None, Some(n)) => n.apply(raw),
            (None, None) => raw,
        }
    }
}

/// 一个参与覆盖 / 分字集统计的字集。成员是单字（多码位的簇不会是汉字，忽略即可）。
#[derive(Clone, Debug)]
pub struct Charset {
    pub key: String,
    pub label: String,
    pub chars: Vec<String>,
}

pub struct StatsOptions<'o> {
    pub kind: StatsKind,
    pub rank: RankOrder,
    /// 全码上限（码表的 `max_code_length`）；0 = 不设。
    pub max_code_length: usize,
    /// 重码 Top 列表取几组；0 = 不取。
    pub top_n: usize,
    pub layers: Vec<StatsLayer>,
    /// 主库在 `layers` 里的下标；给了就另出一份只看主库的重码（[`DupStats::main`]）。
    pub main_layer: Option<usize>,
    /// 字频（单字 → 频次）；`None` 时加权指标不出。
    pub freq: Option<&'o HashMap<char, u64>>,
    pub charsets: &'o [Charset],
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LayerCounts {
    pub entries: u64,
    pub chars: u64,
    pub words: u64,
    pub other: u64,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TotalCounts {
    pub entries: u64,
    pub unique_chars: u64,
    pub unique_words: u64,
    /// 按字数（英文按字母数）分档的条目数，键 `"1"`..`"4"`、`"5+"`。
    pub by_len: BTreeMap<String, u64>,
    pub other: u64,
    /// 有 ≥2 个不同编码的单字数：拼音即多音字，码表即多码字。
    pub multi_code_chars: u64,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodeLenDist {
    /// 单字条目按码长，键 `"1"`..`"4"`、`"5+"`。
    pub char: BTreeMap<String, u64>,
    pub word: BTreeMap<String, u64>,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShortCodes {
    /// 各级简码的字数（键 = 码长）：一个字在某级有简码即计 1。
    pub levels: BTreeMap<String, u64>,
    pub chars_with_short: u64,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CharFullDup {
    pub groups: u64,
    pub chars: u64,
    pub max: u64,
    /// 按全码分组的前 `top_n` 组（组大小降、码升），成员按该码下的序位。
    pub top: Vec<DupGroup>,
}

/// 只看主库那一层的重码（口径同合计，组内按文字去重）。
#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MainDup {
    pub groups: u64,
    pub entries: u64,
    pub max: u64,
    pub max_code: String,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DupGroup {
    pub code: String,
    pub size: u64,
    /// 按静态序位排列，最多 [`TOP_MEMBERS_MAX`] 个。
    pub texts: Vec<String>,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DupStats {
    pub groups: u64,
    pub entries: u64,
    pub non_first: u64,
    pub max: u64,
    pub max_code: String,
    /// 只看单字全码（码表）。
    pub char_full: Option<CharFullDup>,
    /// 只看主库（给了 [`StatsOptions::main_layer`] 时）。
    pub main: Option<MainDup>,
    pub top: Vec<DupGroup>,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CharsetStats {
    pub key: String,
    pub label: String,
    pub size: u64,
    pub missing: u64,
    /// 未收字（按字集顺序），最多 [`MISSING_CHARS_MAX`] 个。
    pub missing_chars: Vec<String>,
    /// 码表：字集内按全码分组的非首选字数（静态重码）。
    pub static_dup: Option<u64>,
    pub dup_groups: Option<u64>,
    /// 码表且有字频：动态选重率（0..1）。
    pub dyn_rate: Option<f64>,
    pub avg_code_len: Option<f64>,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PinyinExtra {
    pub syllables: u64,
    pub zero_weight: u64,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DictStats {
    /// 与 [`StatsOptions::layers`] 同序。
    pub layers: Vec<LayerCounts>,
    pub total: TotalCounts,
    pub code_len: Option<CodeLenDist>,
    pub short_codes: Option<ShortCodes>,
    pub dup: DupStats,
    pub charsets: Vec<CharsetStats>,
    pub pinyin: Option<PinyinExtra>,
}

/// 是否计作汉字：CJK 统一汉字（含扩展 A 与 B 起各补充平面区段）、兼容区、私用区（码表常把
/// 字根放在这里）、「〇」。
pub fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x3007
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xE000..=0xF8FF
        | 0xF900..=0xFAFF
        | 0x20000..=0x3FFFF
        | 0xF0000..=0x10FFFF)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TextClass {
    Char,
    Word(usize),
    Other,
}

fn classify(kind: StatsKind, text: &str) -> TextClass {
    let t = text.trim();
    let n = t.chars().count();
    if kind == StatsKind::English {
        return if n == 0 {
            TextClass::Other
        } else {
            TextClass::Word(n)
        };
    }
    if n == 0 || !t.chars().all(is_han) {
        TextClass::Other
    } else if n == 1 {
        TextClass::Char
    } else {
        TextClass::Word(n)
    }
}

/// 该条目是否参与重码与同码排位：英文方案全算，其余要含汉字。
fn ranked(kind: StatsKind, text: &str) -> bool {
    kind == StatsKind::English || text.chars().any(is_han)
}

/// 分档下标：1、2、3、4、≥5。
fn bucket(n: usize) -> usize {
    n.clamp(1, 5) - 1
}

const BUCKET_KEYS: [&str; 5] = ["1", "2", "3", "4", "5+"];

fn bucket_map(counts: &[u64; 5], keep_zero: bool) -> BTreeMap<String, u64> {
    counts
        .iter()
        .enumerate()
        .filter(|(_, n)| keep_zero || **n > 0)
        .map(|(i, n)| (BUCKET_KEYS[i].to_string(), *n))
        .collect()
}

/// 去重集合用的轻量哈希（FxHash 同式）：拼音全表要往集合里塞 60 万个词，默认的 SipHash
/// 在这里占了可观的一截；键都是词库里的文字，不涉及抗碰撞攻击。
#[derive(Default)]
struct FxHasher(u64);

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type FxBuild = BuildHasherDefault<FxHasher>;

/// 同码内的排序键（越小越靠前）：权重模式 `(−w, 档位, 自然序)`，自然模式 `(0, 档位, 自然序)`。
type RankKey = (i64, i32, i32);

/// 单字在各码下的序位与码（码表的全码/简码、加权指标都从这里出）。
#[derive(Default)]
struct CharInfo {
    /// (码, 该码下的序位)，码不重复。
    codes: Vec<(String, u32)>,
}

/// Top 列表的堆元素：按 (组大小, 码倒序) 排，堆顶是最该被挤掉的那组。
#[derive(PartialEq, Eq)]
struct TopItem(DupGroup);

impl Ord for TopItem {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        // 小顶堆语义靠 Reverse 包装：这里定义「更好」= 更大的组、同大小码更小。
        self.0
            .size
            .cmp(&o.0.size)
            .then_with(|| o.0.code.cmp(&self.0.code))
    }
}

impl PartialOrd for TopItem {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

pub struct StatsBuilder<'a, 'o> {
    opts: StatsOptions<'o>,
    layers: Vec<LayerCounts>,
    total: TotalCounts,
    by_len: [u64; 5],
    code_len_char: [u64; 5],
    code_len_word: [u64; 5],
    chars: HashMap<&'a str, CharInfo, FxBuild>,
    words: HashSet<&'a str, FxBuild>,
    dup: DupStats,
    top: BinaryHeap<std::cmp::Reverse<TopItem>>,
    syllables: HashSet<String, FxBuild>,
    zero_weight: u64,
    /// 组内去重 / 排序的复用缓冲：(文字, 排序键, 原始权重, 分类)。
    buf: Vec<(&'a str, RankKey, i32, TextClass)>,
    /// `buf` 里参与重码与排位的那部分（见 [`ranked`]）。
    rbuf: Vec<(&'a str, RankKey, i32, TextClass)>,
    /// 主库那一层参与重码的文字（组内去重用）。
    main_texts: Vec<&'a str>,
    main_dup: MainDup,
    /// 组内已处理过的音节边界（同码各条的边界通常相同）。
    seen_boundaries: Vec<u64>,
}

impl<'a, 'o> StatsBuilder<'a, 'o> {
    pub fn new(opts: StatsOptions<'o>) -> Self {
        let n = opts.layers.len();
        Self {
            opts,
            layers: vec![LayerCounts::default(); n],
            total: TotalCounts::default(),
            by_len: [0; 5],
            code_len_char: [0; 5],
            code_len_word: [0; 5],
            chars: HashMap::default(),
            words: HashSet::default(),
            dup: DupStats::default(),
            top: BinaryHeap::new(),
            syllables: HashSet::default(),
            zero_weight: 0,
            buf: Vec::new(),
            rbuf: Vec::new(),
            main_texts: Vec::new(),
            main_dup: MainDup::default(),
            seen_boundaries: Vec::new(),
        }
    }

    fn rank_key(&self, e: &GroupEntry<'_>) -> RankKey {
        let layer = self.opts.layers.get(e.layer).copied().unwrap_or_default();
        let primary = match self.opts.rank {
            RankOrder::Weight => -(layer.effective_weight(e.weight) as i64),
            RankOrder::Natural => 0,
        };
        (primary, layer.base_order, e.order)
    }

    /// 预留去重集合的容量（`n` 取词库条目数的估计即可），省掉大词库上逐级扩容的重哈希。
    pub fn reserve(&mut self, n: usize) {
        self.words.reserve(n);
    }

    /// 送入一个编码的全部条目（各库合在一起）。同一编码只能送一次。
    pub fn push_group(&mut self, code: &str, entries: &[GroupEntry<'a>]) {
        let kind = self.opts.kind;
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        for e in entries {
            let class = classify(kind, e.text);
            if let Some(l) = self.layers.get_mut(e.layer) {
                l.entries += 1;
                match class {
                    TextClass::Char => l.chars += 1,
                    TextClass::Word(_) => l.words += 1,
                    TextClass::Other => l.other += 1,
                }
            }
            // 去重与集合都按去掉首尾空白的文字：「 工」与「工」是同一条。
            buf.push((e.text.trim(), self.rank_key(e), e.weight, class));
        }

        // 组内按文字去重（跨库同 (码, 文字) 只算一条，取排序最靠前的那条），再按序位排。
        buf.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)));
        buf.dedup_by(|b, a| a.0 == b.0);
        buf.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));

        let code_len = code.chars().count();
        self.total.entries += buf.len() as u64;
        for &(text, _, raw_w, class) in &buf {
            if raw_w == 0 {
                self.zero_weight += 1;
            }
            match class {
                TextClass::Char => {
                    self.by_len[0] += 1;
                    self.code_len_char[bucket(code_len)] += 1;
                }
                TextClass::Word(k) => {
                    self.by_len[bucket(k)] += 1;
                    self.code_len_word[bucket(code_len)] += 1;
                    self.words.insert(text);
                }
                TextClass::Other => self.total.other += 1,
            }
        }

        // 重码与排位只看含汉字的条目：纯 emoji / 符号既不占位、也不凑重码组。
        let mut rbuf = std::mem::take(&mut self.rbuf);
        rbuf.clear();
        rbuf.extend(buf.iter().filter(|x| ranked(kind, x.0)).copied());
        for (rank, &(text, _, _, class)) in rbuf.iter().enumerate() {
            if class == TextClass::Char {
                let info = self.chars.entry(text).or_default();
                if !info.codes.iter().any(|(c, _)| c == code) {
                    info.codes.push((code.to_string(), rank as u32));
                }
            }
        }
        let n = rbuf.len();

        if kind == StatsKind::Pinyin {
            self.seen_boundaries.clear();
            for e in entries {
                if e.boundary != 0 && !self.seen_boundaries.contains(&e.boundary) {
                    self.seen_boundaries.push(e.boundary);
                    self.add_syllables(code, e.boundary);
                }
            }
        }

        if n >= 2 {
            self.dup.groups += 1;
            self.dup.entries += n as u64;
            self.dup.non_first += (n - 1) as u64;
            if n as u64 > self.dup.max {
                self.dup.max = n as u64;
                self.dup.max_code = code.to_string();
            }
            self.offer_top(code, &rbuf);
        }
        if let Some(main) = self.opts.main_layer {
            self.main_texts.clear();
            for e in entries.iter().filter(|e| e.layer == main) {
                let t = e.text.trim();
                if ranked(kind, t) && !self.main_texts.contains(&t) {
                    self.main_texts.push(t);
                }
            }
            let m = self.main_texts.len() as u64;
            if m >= 2 {
                let d = &mut self.main_dup;
                d.groups += 1;
                d.entries += m;
                if m > d.max {
                    d.max = m;
                    d.max_code = code.to_string();
                }
            }
        }
        self.buf = buf;
        self.rbuf = rbuf;
    }

    fn add_syllables(&mut self, code: &str, boundary: u64) {
        let len = code.len().min(64);
        let mut start = None;
        for i in 0..=len {
            if i < len && boundary & (1u64 << i) == 0 {
                continue;
            }
            let Some(s) = start.replace(i) else { continue };
            let end = if i == len { code.len() } else { i };
            // 只认小写字母段：拼音词库里夹带的大写字母码（「A股」那类）不是音节。
            if let Some(syl) = code.get(s..end)
                && !syl.is_empty()
                && syl.bytes().all(|b| b.is_ascii_lowercase())
                && !self.syllables.contains(syl)
            {
                self.syllables.insert(syl.to_string());
            }
        }
    }

    fn offer_top(&mut self, code: &str, buf: &[(&'a str, RankKey, i32, TextClass)]) {
        let top_n = self.opts.top_n;
        if top_n == 0 {
            return;
        }
        let size = buf.len() as u64;
        if self.top.len() >= top_n
            && let Some(std::cmp::Reverse(worst)) = self.top.peek()
        {
            let beats =
                size > worst.0.size || (size == worst.0.size && code < worst.0.code.as_str());
            if !beats {
                return;
            }
            self.top.pop();
        }
        self.top.push(std::cmp::Reverse(TopItem(DupGroup {
            code: code.to_string(),
            size,
            texts: buf
                .iter()
                .take(TOP_MEMBERS_MAX)
                .map(|(t, ..)| t.to_string())
                .collect(),
        })));
    }

    /// 字的全码：码长不超上限者中最长，同长取序位小、再取码小。
    fn full_code(info: &CharInfo, max_len: usize) -> Option<&(String, u32)> {
        info.codes
            .iter()
            .filter(|(c, _)| max_len == 0 || c.chars().count() <= max_len)
            .min_by(|a, b| {
                b.0.chars()
                    .count()
                    .cmp(&a.0.chars().count())
                    .then(a.1.cmp(&b.1))
                    .then(a.0.cmp(&b.0))
            })
    }

    /// 字的最优码：(序位, 码长) 最小。
    fn best_code(info: &CharInfo) -> Option<&(String, u32)> {
        info.codes
            .iter()
            .min_by_key(|(c, r)| (*r, c.chars().count()))
    }

    pub fn finish(mut self) -> DictStats {
        let kind = self.opts.kind;
        self.total.unique_chars = self.chars.len() as u64;
        self.total.unique_words = self.words.len() as u64;
        self.total.multi_code_chars =
            self.chars.values().filter(|i| i.codes.len() >= 2).count() as u64;
        self.total.by_len = bucket_map(&self.by_len, true);

        let max_len = self.opts.max_code_length;
        let codetable = kind == StatsKind::Codetable;
        let full: HashMap<&str, &str> = if codetable {
            self.chars
                .iter()
                .filter_map(|(t, i)| Self::full_code(i, max_len).map(|(c, _)| (*t, c.as_str())))
                .collect()
        } else {
            HashMap::new()
        };

        let (code_len, short_codes) = if codetable {
            let mut levels: BTreeMap<usize, u64> = BTreeMap::new();
            let mut with_short = 0u64;
            for (t, info) in &self.chars {
                let Some(fc) = full.get(t) else { continue };
                let mut lens: Vec<usize> = info
                    .codes
                    .iter()
                    .map(|(c, _)| c.chars().count())
                    .filter(|l| *l < fc.chars().count())
                    .collect();
                lens.sort_unstable();
                lens.dedup();
                if !lens.is_empty() {
                    with_short += 1;
                }
                for l in lens {
                    *levels.entry(l).or_default() += 1;
                }
            }
            (
                Some(CodeLenDist {
                    char: bucket_map(&self.code_len_char, false),
                    word: bucket_map(&self.code_len_word, false),
                }),
                Some(ShortCodes {
                    levels: levels
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v))
                        .collect(),
                    chars_with_short: with_short,
                }),
            )
        } else {
            (None, None)
        };

        if codetable {
            // 全码 → [(该码下的序位, 字)]
            let mut by_full: HashMap<&str, Vec<(u32, &str)>> = HashMap::new();
            for (t, c) in &full {
                let rank = self.chars[t]
                    .codes
                    .iter()
                    .find(|(x, _)| x == c)
                    .map_or(u32::MAX, |(_, r)| *r);
                by_full.entry(*c).or_default().push((rank, *t));
            }
            let mut groups: Vec<(&str, Vec<(u32, &str)>)> =
                by_full.into_iter().filter(|(_, v)| v.len() >= 2).collect();
            groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
            let top = groups
                .iter()
                .take(self.opts.top_n)
                .map(|(code, members)| {
                    let mut m = members.clone();
                    m.sort();
                    DupGroup {
                        code: code.to_string(),
                        size: m.len() as u64,
                        texts: m
                            .into_iter()
                            .take(TOP_MEMBERS_MAX)
                            .map(|(_, t)| t.to_string())
                            .collect(),
                    }
                })
                .collect();
            self.dup.char_full = Some(CharFullDup {
                groups: groups.len() as u64,
                chars: groups.iter().map(|(_, v)| v.len() as u64).sum(),
                max: groups.first().map_or(0, |(_, v)| v.len() as u64),
                top,
            });
        }
        if self.opts.main_layer.is_some() {
            self.dup.main = Some(std::mem::take(&mut self.main_dup));
        }

        let charsets = self
            .opts
            .charsets
            .iter()
            .map(|cs| self.charset_stats(cs, &full))
            .collect();

        let mut top: Vec<DupGroup> = self.top.drain().map(|r| r.0.0).collect();
        top.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.code.cmp(&b.code)));
        self.dup.top = top;

        DictStats {
            layers: self.layers,
            total: self.total,
            code_len,
            short_codes,
            dup: self.dup,
            charsets,
            pinyin: (kind == StatsKind::Pinyin).then_some(PinyinExtra {
                syllables: self.syllables.len() as u64,
                zero_weight: self.zero_weight,
            }),
        }
    }

    fn charset_stats(&self, cs: &Charset, full: &HashMap<&str, &str>) -> CharsetStats {
        let codetable = self.opts.kind == StatsKind::Codetable;
        let mut out = CharsetStats {
            key: cs.key.clone(),
            label: cs.label.clone(),
            size: cs.chars.len() as u64,
            ..Default::default()
        };
        let mut by_full: HashMap<&str, u64> = HashMap::new();
        let (mut tot, mut sel, mut len_sum) = (0u64, 0u64, 0u64);
        for ch in &cs.chars {
            let Some(info) = self.chars.get(ch.as_str()) else {
                out.missing += 1;
                if out.missing_chars.len() < MISSING_CHARS_MAX {
                    out.missing_chars.push(ch.clone());
                }
                continue;
            };
            if !codetable {
                continue;
            }
            if let Some(fc) = full.get(ch.as_str()) {
                *by_full.entry(fc).or_default() += 1;
            }
            if let (Some(freq), Some((code, rank)), Some(c)) =
                (self.opts.freq, Self::best_code(info), ch.chars().next())
            {
                let f = freq.get(&c).copied().unwrap_or(0);
                tot += f;
                if *rank > 0 {
                    sel += f;
                }
                len_sum += f * code.chars().count() as u64;
            }
        }
        if codetable {
            let dups = by_full.values().filter(|n| **n >= 2);
            out.static_dup = Some(dups.clone().map(|n| n - 1).sum());
            out.dup_groups = Some(dups.count() as u64);
            if self.opts.freq.is_some() && tot > 0 {
                out.dyn_rate = Some(sel as f64 / tot as f64);
                out.avg_code_len = Some(len_sum as f64 / tot as f64);
            }
        }
        out
    }
}

/// rime 词库正文的起点（`...` 之后）；没有文件头时从头算。
fn rime_body(text: &str) -> &str {
    match crate::codetable::rime_body_layout(text) {
        Some((off, _)) => &text[off..],
        None => text,
    }
}

/// 把白霜字表（`文字\t拼音\t权重`）的单字权重累加进 `out`：同字各读音之和即字频。
pub fn add_char_freq(text: &str, out: &mut HashMap<char, u64>) {
    let text = wind_utils::text::normalize_input(text);
    for line in rime_body(&text).lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        let (Some(t), _, Some(w)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let mut it = t.chars();
        let (Some(c), None) = (it.next(), it.next()) else {
            continue;
        };
        if let Ok(w) = w.trim().parse::<u64>() {
            *out.entry(c).or_default() += w;
        }
    }
}

/// 按 `# 一级` / `# 二级` / `# 三级` 分节读 `charsets/common_han.yaml`，返回三级各自的字。
///
/// 只认单字行（`-` 前缀的移除行、多码位簇都跳过）。分节标题就是文件里的那三行注释——
/// 名单本身不带级别字段，这是唯一的级别来源；读出来不是 3 段时调用方应当放弃字集统计。
pub fn parse_common_han_levels(text: &str) -> Vec<Vec<String>> {
    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut body = false;
    let text = wind_utils::text::normalize_input(text);
    for line in text.lines() {
        let s = line.trim();
        if !body {
            body = s == "...";
            continue;
        }
        if let Some(h) = s.strip_prefix('#') {
            let h = h.trim_start();
            if ["一级", "二级", "三级"].iter().any(|p| h.starts_with(p)) {
                levels.push(Vec::new());
            }
            continue;
        }
        let Some(cur) = levels.last_mut() else {
            continue;
        };
        let mut it = s.chars();
        if let (Some(c), None) = (it.next(), it.next())
            && c != '-'
        {
            cur.push(c.to_string());
        }
    }
    levels
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
