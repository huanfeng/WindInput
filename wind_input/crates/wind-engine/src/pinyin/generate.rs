//! 词语全拼编码生成（造词反推读音）。
//!
//! 为词语推断全拼编码（如"你好"→"nihao"），核心是解决多音字在词里读哪个音。
//!
//! 三级优先策略：
//!  1. 整词命中：枚举每字所有读音的笛卡尔积（按权重降序），第一个能让词典查回该词的组合即最优。
//!  2. 最长子词切分：DP 把词切成已知子词序列（如"长江三角洲"=长江+三角洲），继承子词整体读音。
//!  3. 逐字代表读音兜底：确保至少有结果。
//!
//! 单字读音索引 [`CharPinyinIndex`] 从词典本身派生（遍历标准音节查单字候选、按权重排序），
//! 不依赖 wind-reverse 的 pinyin_map.txt——代表读音 = 词典里权重最高的读音。

use std::collections::HashMap;

use wind_dict::cached::CachedDict;

use super::dag::{Dag, SegGraph};
use super::syllable::{STANDARD_SYLLABLES, SyllableTrie};

/// 整词读音消歧时笛卡尔积组合数上限（防生僻多音字长词性能塌方）。
const MAX_READING_COMBOS: usize = 64;

/// [`CharPinyinIndex::build`] 的累计统计（诊断）。
pub static BUILD_STATS: crate::text_codes::BuildStats = crate::text_codes::BuildStats::new();

/// 单字读音表的查询面：本模块的推断 / 求解只经这三个方法读读音表。
///
/// 抽成 trait 只为等价性对拍（`generate/legacy_equiv.rs` 的参照实现与 [`CharPinyinIndex`]
/// 喂同一批词）。生产只有 [`CharPinyinIndex`] 一个实现。
pub trait ReadingTable {
    /// 代表读音（权重最高者）；没收录的字为 `None`。
    fn representative(&self, c: char) -> Option<&str>;
    /// 该字的读音条数（≥ 1）；没收录的字为 `None`。
    fn reading_count(&self, c: char) -> Option<usize>;
    /// 第 `i` 个读音（按权重降序，`i < reading_count`）；越界或没收录为 `None`。
    fn reading(&self, c: char, i: usize) -> Option<&str>;
}

/// 汉字 → 读音反向索引。
///
/// 每字的读音按词典权重降序（同权重按 [`STANDARD_SYLLABLES`] 的次序），第 0 个即代表读音。
/// 由 [`CharPinyinIndex::build`] 遍历 [`STANDARD_SYLLABLES`] 查词典单字候选构建。
///
/// 紧凑存储（与 `wind-reverse` 的 `PinyinTable` 同构）：按字升序的定长条目数组 + 读音结束偏移
/// 数组 + 一整块读音文本，查询二分，没有逐条堆分配（逐字 `HashMap` 的小块分配开销见
/// `docs/design/memory-footprint.md` S5；对拍见 `generate/legacy_equiv.rs`）。
#[derive(Debug, Default)]
pub struct CharPinyinIndex {
    /// 按 `ch` 升序。本字读音在 `reading_ends` 中的下标区间 = [前一条目的 `reading_end`,
    /// 本条 `reading_end`)，首条起点为 0。
    entries: Vec<CharEntry>,
    /// 每个读音在 `arena` 中的结束偏移，按条目序连续；单条读音 = [前一项, 本项)，首项起点为 0。
    reading_ends: Vec<u32>,
    /// 全部读音文本首尾相接。
    arena: String,
}

#[derive(Debug)]
struct CharEntry {
    ch: char,
    reading_end: u32,
}

impl CharPinyinIndex {
    /// 从词典构建索引：遍历标准音节，收集单字候选及其权重，按权重降序定读音。
    pub fn build(dict: &CachedDict) -> Self {
        let t0 = std::time::Instant::now();
        let idx = Self::build_inner(dict);
        let took = t0.elapsed();
        BUILD_STATS.record(took, took);
        tracing::debug!(
            "单字读音索引构建 字数={} 用时={took:?} heap_kb={}",
            idx.char_count(),
            idx.heap_estimate().0 / 1024
        );
        idx
    }

    fn build_inner(dict: &CachedDict) -> Self {
        // 音节 id = 它在 `STANDARD_SYLLABLES` 里**首次**出现的下标：同一音节串只算一个读音，
        // 且 id 序就是遍历序（「同权重保留首次遇到的次序」靠的正是它）。
        let mut first: HashMap<&'static str, u32> =
            HashMap::with_capacity(STANDARD_SYLLABLES.len());
        // 中间态只有定长三元组 (字, 音节 id, 权重)，不为读音逐条分配。
        let mut rows: Vec<(char, u32, i32)> = Vec::new();
        for (i, &syl) in STANDARD_SYLLABLES.iter().enumerate() {
            let sid = *first.entry(syl).or_insert(i as u32);
            for (text, weight, _order) in dict.search(syl) {
                let mut chars = text.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    continue; // 仅单字
                };
                rows.push((c, sid, weight));
            }
        }
        // 同字同音节多条（异体 / 多源）合并取最大权重。
        rows.sort_unstable_by_key(|&(c, sid, _)| (c, sid));
        rows.dedup_by(|b, a| {
            let same = a.0 == b.0 && a.1 == b.1;
            if same {
                a.2 = a.2.max(b.2);
            }
            same
        });
        // 字内按权重降序、同权重按音节 id（即首次出现次序）。
        rows.sort_unstable_by_key(|&(c, sid, w)| (c, std::cmp::Reverse(w), sid));

        let chars =
            rows.windows(2).filter(|w| w[0].0 != w[1].0).count() + usize::from(!rows.is_empty());
        let mut idx = CharPinyinIndex {
            entries: Vec::with_capacity(chars),
            reading_ends: Vec::with_capacity(rows.len()),
            arena: String::with_capacity(
                rows.iter()
                    .map(|r| STANDARD_SYLLABLES[r.1 as usize].len())
                    .sum(),
            ),
        };
        for (i, &(c, sid, _)) in rows.iter().enumerate() {
            idx.arena.push_str(STANDARD_SYLLABLES[sid as usize]);
            // 偏移存 u32：读音池约 0.2 MB、读音 5 万条，离上限差四个数量级。
            debug_assert!(u32::try_from(idx.arena.len()).is_ok(), "读音池超出 u32");
            idx.reading_ends.push(idx.arena.len() as u32);
            if rows.get(i + 1).is_none_or(|n| n.0 != c) {
                debug_assert!(
                    u32::try_from(idx.reading_ends.len()).is_ok(),
                    "读音条数超出 u32"
                );
                idx.entries.push(CharEntry {
                    ch: c,
                    reading_end: idx.reading_ends.len() as u32,
                });
            }
        }
        drop(rows);
        idx.entries.shrink_to_fit();
        idx.reading_ends.shrink_to_fit();
        idx.arena.shrink_to_fit();
        idx
    }

    /// 该字读音在 `reading_ends` 中的下标区间；没收录为 `None`。
    fn range_of(&self, c: char) -> Option<std::ops::Range<usize>> {
        let i = self.entries.binary_search_by_key(&c, |e| e.ch).ok()?;
        let start = if i == 0 {
            0
        } else {
            self.entries[i - 1].reading_end as usize
        };
        Some(start..self.entries[i].reading_end as usize)
    }

    /// 诊断：(堆字节, 堆分配块数)。三块连续存储，按容量计。
    pub(crate) fn heap_estimate(&self) -> (usize, usize) {
        let bytes = self.entries.capacity() * size_of::<CharEntry>()
            + self.reading_ends.capacity() * size_of::<u32>()
            + self.arena.capacity();
        (bytes, 3)
    }

    /// 诊断：收录的字数。
    pub(crate) fn char_count(&self) -> usize {
        self.entries.len()
    }

    /// 诊断：全部读音条数（多音字每音一条）。
    pub(crate) fn total_readings(&self) -> usize {
        self.reading_ends.len()
    }
}

impl ReadingTable for CharPinyinIndex {
    fn representative(&self, c: char) -> Option<&str> {
        self.reading(c, 0)
    }

    fn reading_count(&self, c: char) -> Option<usize> {
        self.range_of(c).map(|r| r.len())
    }

    fn reading(&self, c: char, i: usize) -> Option<&str> {
        let r = self.range_of(c)?;
        let k = r.start + i;
        if k >= r.end {
            return None;
        }
        let from = if k == 0 {
            0
        } else {
            self.reading_ends[k - 1] as usize
        };
        Some(&self.arena[from..self.reading_ends[k] as usize])
    }
}

/// 按音节/词段拼接带空格的音节码（`ni hao`）。
///
/// 造词本就逐音节拼接，边界是白送的，不该丢给下游用 DAG 反猜。用空格而不是「扁平 code +
/// bitmask」：码与边界物理上不可分离、无法错配，也没有 bitmask 的 64 字节上限。
///
/// 落库时由 [`wind_store::wdict::split_spaced_code`] 拆回 `flat + mask`——key 必须保持
/// 扁平，见 `docs/design/pinyin-code-domains.md` §2.2。
struct SpacedCode {
    syls: Vec<String>,
}

impl SpacedCode {
    fn new(cap: usize) -> Self {
        Self {
            syls: Vec::with_capacity(cap),
        }
    }

    /// 追加一个已带空格的多音节词段（如整词 `ni hao`）。段内边界随空格原样并入，无需平移。
    fn push_segment(&mut self, spaced: &str) {
        self.syls.extend(
            spaced
                .split(' ')
                .filter(|s| !s.is_empty())
                .map(String::from),
        );
    }

    /// 追加单个音节。
    fn push_syllable(&mut self, s: &str) {
        self.syls.push(s.to_string());
    }

    fn finish(self) -> String {
        self.syls.join(" ")
    }
}

/// 为词语生成带空格的全拼音节码（`你好` → `ni hao`）。含无读音字符时返回 `None`。
///
/// 空格即音节边界，与 rime 源词库同形。落库时由
/// [`wind_store::wdict::split_spaced_code`] 拆成扁平 code + boundary
/// （语义同 `wind_dict::binformat::DictEntry::boundary`），供用户自造词从诞生起就带上
/// 边界——否则用户词是块「边界空洞」，双拼校验只能对其降级。
///
/// `dict` 为拼音系统词典（提供整词验证的真值表），`index` 为单字读音索引。
pub fn generate_word_pinyin(
    dict: &CachedDict,
    index: &(impl ReadingTable + ?Sized),
    word: &str,
) -> Option<String> {
    let runes: Vec<char> = word.chars().collect();
    if runes.is_empty() {
        return None;
    }
    // 1) 整词命中
    if let Some(r) = infer_whole_word_code(dict, index, &runes, word) {
        return Some(r);
    }
    // 2) 子词切分 + 整体读音继承
    if let Some(r) = infer_by_subword_segmentation(dict, index, &runes) {
        return Some(r);
    }
    // 3) 兜底：逐字代表读音（每字一音节）
    let mut b = SpacedCode::new(runes.len());
    for &r in &runes {
        b.push_syllable(index.representative(r)?);
    }
    Some(b.finish())
}

/// 用词典真值表为整词推断读音：枚举每字读音笛卡尔积，找到第一个能查回该词的组合。
/// 每字读音按权重降序，故按字典序枚举时首个命中天然是"各字读音权重之和"最高的合理组合。
/// 单字不进入此分支（无消歧必要）。
fn infer_whole_word_code(
    dict: &CachedDict,
    index: &(impl ReadingTable + ?Sized),
    runes: &[char],
    word: &str,
) -> Option<String> {
    if runes.len() < 2 {
        return None;
    }
    // 收集每字读音列表，同时估算笛卡尔积规模
    let mut counts: Vec<usize> = Vec::with_capacity(runes.len());
    let mut combos = 1usize;
    for &r in runes {
        let n = index.reading_count(r)?;
        if n == 0 {
            return None;
        }
        combos *= n;
        if combos > MAX_READING_COMBOS {
            return None;
        }
        counts.push(n);
    }
    // 笛卡尔积枚举（按字典序，等价于按权重组合的优先级）
    let mut idxs = vec![0usize; runes.len()];
    loop {
        // 每字一音节（第 i 字选第 pos 个读音）。
        let mut b = SpacedCode::new(runes.len());
        for (i, &pos) in idxs.iter().enumerate() {
            b.push_syllable(index.reading(runes[i], pos)?);
        }
        let spaced = b.finish();
        // 查词典须用**扁平**码：词典 key 是扁平的（见 §2.2）。
        let flat = spaced.replace(' ', "");
        if dict.search(&flat).iter().any(|(text, _, _)| text == word) {
            return Some(spaced);
        }
        // 递增到下一个组合（低位满则进位）
        let mut k = runes.len();
        loop {
            if k == 0 {
                return None;
            }
            k -= 1;
            idxs[k] += 1;
            if idxs[k] < counts[k] {
                break;
            }
            idxs[k] = 0;
        }
    }
}

/// DP 节点：拼出 `word[..i]` 字段的最优方案。
#[derive(Clone)]
struct DpState {
    prev: usize,
    /// 该段为多字子词时的整体读音码，**带空格**（如「你好」→ `ni hao`）；单字过渡为空。
    /// 段内边界就在空格里，回溯拼接时直接并入即可——此前需额外存一个 `seg_mask`
    /// 并在拼接时左移到全局位置，漏平移就会丢掉长词的段内边界。
    seg: String,
    /// 已用多字子词段数（越少越优，同总字数下）。
    multi_segs: usize,
    /// 已用多字子词的总字数（越大越优）。
    total_mul: usize,
}

/// `a` 是否优于 `b`：多字子词总字数高 > 段数少（更长子词优先）。
fn better(a: &DpState, b: &DpState) -> bool {
    if a.total_mul != b.total_mul {
        a.total_mul > b.total_mul
    } else {
        a.multi_segs < b.multi_segs
    }
}

/// 用 DP 把词切成已知子词序列，继承子词整体读音（解决长词中的多音字）。
/// 找不到任何多字子词切分时返回 `None`，让调用方走逐字兜底。
fn infer_by_subword_segmentation(
    dict: &CachedDict,
    index: &(impl ReadingTable + ?Sized),
    runes: &[char],
) -> Option<String> {
    let n = runes.len();
    if n < 2 {
        return None;
    }
    let mut dp: Vec<Option<DpState>> = vec![None; n + 1];
    // dp[0].prev 永不被回溯读取（回溯条件 cur > 0），用 0 占位即可
    dp[0] = Some(DpState {
        prev: 0,
        seg: String::new(),
        multi_segs: 0,
        total_mul: 0,
    });

    for i in 0..n {
        let Some(cur) = dp[i].clone() else {
            continue;
        };
        // 长度 >=2 的子段做整词查（单字走兜底过渡）
        let mut l = 2;
        while i + l <= n {
            let sub: String = runes[i..i + l].iter().collect();
            if let Some(code) = infer_whole_word_code(dict, index, &runes[i..i + l], &sub) {
                let next = DpState {
                    prev: i,
                    seg: code,
                    multi_segs: cur.multi_segs + 1,
                    total_mul: cur.total_mul + l,
                };
                if dp[i + l].as_ref().is_none_or(|d| better(&next, d)) {
                    dp[i + l] = Some(next);
                }
            }
            l += 1;
        }
        // 单字过渡（不计入 total_mul，仅承接前缀状态）
        let next = DpState {
            prev: i,
            seg: String::new(),
            multi_segs: cur.multi_segs,
            total_mul: cur.total_mul,
        };
        if dp[i + 1].as_ref().is_none_or(|d| better(&next, d)) {
            dp[i + 1] = Some(next);
        }
    }

    let final_state = dp[n].as_ref()?;
    if final_state.total_mul == 0 {
        // 没有任何多字子词被命中，让上层走代表读音兜底
        return None;
    }
    // 回溯重建（从后往前收集各段，再反转）
    struct Span {
        from: usize,
        code: String,
    }
    let mut spans: Vec<Span> = Vec::new();
    let mut cur = n;
    while cur > 0 {
        let s = dp[cur].as_ref().expect("dp 链应连续");
        spans.push(Span {
            from: s.prev,
            code: s.seg.clone(),
        });
        cur = s.prev;
    }
    spans.reverse();

    let mut b = SpacedCode::new(n);
    for sp in &spans {
        if !sp.code.is_empty() {
            // 多字子词段：段内音节边界就在空格里，直接并入（无需平移）。
            b.push_segment(&sp.code);
        } else {
            // 单字段：用代表读音（本身即一个音节）
            b.push_syllable(index.representative(runes[sp.from])?);
        }
    }
    Some(b.finish())
}

/// 层 4 求解的路径枚举上限（见 [`super::dag::SegGraph::paths_with_edges`]）。
/// 取 16：正常词条码 ≤ 12 字节，同字数的合法切分极少超过个位数；超限即按「多解」处置。
const MAX_BOUNDARY_PATHS: usize = 16;

/// [`boundary_by_char_count`] 的求解结果。
///
/// `ambiguous` 与 `no_reading` 不是两个可以任意组合的正交布尔：`no_reading` 为真时
/// 读音表整体缺席，`ambiguous`（「多解，已按读音权重择一」）的语义不成立，调用方须先看
/// `no_reading`。分成两个字段只是因为求解过程里它们在不同步骤得出。
pub struct BoundarySolve {
    /// 音节起点 bitmask，语义同 `DictEntry::boundary`。
    pub mask: u64,
    /// 约束筛完仍多解（或路径枚举被截断），已按读音权重择一。
    pub ambiguous: bool,
    /// `text` 含无读音字符 ⇒ 读音验证整体缺席（不是「读音对不上」）。
    pub no_reading: bool,
}

/// 按「音节数 == 汉字数」求解 `(code, text)` 的音节边界。
///
/// 这是导入闸口的第 4 层（见 `docs/design/pinyin-entry-boundary-contract.md` §3.1）。
///
/// 不是猜：汉字词的音节数恒等于汉字数，`xianning` + 「西安宁」只有 `xi|an|ning` 一条
/// 3 音节路径，切分由启发式降为可判定问题。与 [`generate_word_pinyin`]（字 → 码，多音字
/// 靠权重猜）方向相反：这里读音已由词库作者写定，`chongqing` + 「重庆」直接切出，无需消歧。
///
/// `code.len() > 64`（合法但无边界，降级为 0）须由调用方先拦掉，到这里的 `None` 一律是非法。
/// 判据①（每字须有读音）只标记不拒收，拦误导入全靠判据②，见设计文档 §2.1。
pub fn boundary_by_char_count(
    index: &(impl ReadingTable + ?Sized),
    trie: &SyllableTrie,
    code: &str,
    text: &str,
) -> Option<BoundarySolve> {
    let runes: Vec<char> = text.chars().collect();
    if runes.is_empty() || code.is_empty() || code.len() > 64 {
        return None;
    }
    // 判据①（设计文档 §2.1）：每个字符都要有读音。只作标记，不拒收。
    // `readings` 只收单字词典条目，故这条同时也是「该字在本方案词典里存在」。
    let no_reading = runes.iter().any(|&c| index.reading_count(c).is_none());
    // 必须 `build_strict`：这里推的是真值边界。模糊拼写层多出的错音边既可能撑爆
    // `MAX_BOUNDARY_PATHS` 把唯一解判成 truncated，也会让判据② 被错音串满足。
    let graph = SegGraph::from_dag(&Dag::build_strict(code, trie));
    let paths = graph.paths_with_edges(0, code.len(), runes.len(), MAX_BOUNDARY_PATHS);
    if paths.is_empty() {
        return None; // 判据②不满足：切不出与字数相符的音节序列
    }
    // 达到上限说明还有没枚举到的路径 ⇒ 即便下面筛出唯一，也不能宣称唯一。
    let truncated = paths.len() >= MAX_BOUNDARY_PATHS;

    // 读音验证：每个音节必须是对应字的读音之一。按「各字读音下标之和」升序择优
    // （`readings` 已按词典权重降序，下标越小越常用），与 `infer_whole_word_code`
    // 的笛卡尔积按字典序枚举是同一套偏好。
    let mut scored: Vec<(usize, &Vec<usize>)> = paths
        .iter()
        .filter_map(|p| reading_score(index, &runes, code, p).map(|s| (s, p)))
        .collect();

    let (best, multi) = if scored.is_empty() {
        // 读音表不认可任何一条切分（方言音 / 词库作者用了非常用读音 / 词典单字表不全 /
        // `no_reading`：text 含符号等无读音字符）。
        // 切分本身在音节图上合法，不能因此否决——否则会把「码没错、只是读音冷门」的
        // 词条误判成非法。退回「唯一即采信、多解算歧义」。
        (&paths[0], paths.len() > 1)
    } else {
        scored.sort_by_key(|(s, _)| *s);
        (scored[0].1, scored.len() > 1)
    };
    Some(BoundarySolve {
        mask: mask_of(best),
        ambiguous: multi || truncated,
        no_reading,
    })
}

/// 一条切分的读音代价：各音节在对应字读音表中的下标之和；任一音节不是该字的读音则 `None`。
fn reading_score(
    index: &(impl ReadingTable + ?Sized),
    runes: &[char],
    code: &str,
    offsets: &[usize],
) -> Option<usize> {
    if offsets.len() != runes.len() {
        return None;
    }
    let mut score = 0usize;
    for (i, &off) in offsets.iter().enumerate() {
        let end = offsets.get(i + 1).copied().unwrap_or(code.len());
        let syl = code.get(off..end)?;
        let n = index.reading_count(runes[i])?;
        let pos = (0..n).position(|k| index.reading(runes[i], k) == Some(syl))?;
        score += pos;
    }
    Some(score)
}

/// 音节起点偏移 → boundary bitmask（语义同 `DictEntry::boundary`）。
fn mask_of(offsets: &[usize]) -> u64 {
    offsets
        .iter()
        .filter(|&&o| o < 64)
        .fold(0u64, |m, &o| m | (1u64 << o))
}

#[cfg(test)]
mod legacy_equiv;

#[cfg(test)]
#[path = "generate_tests.rs"]
mod tests;
