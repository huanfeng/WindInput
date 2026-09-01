//! 词语全拼编码生成（造词反推读音）。
//!
//! 与 Go 版 `internal/engine/pinyin` 的 `GenerateWordPinyin` 对齐：
//! 为词语推断全拼编码（如"你好"→"nihao"），核心是解决**多音字在词里读哪个音**。
//!
//! 三级优先策略：
//!  1. 整词命中：枚举每字所有读音的笛卡尔积（按权重降序），第一个能让词典查回该词的组合即最优。
//!  2. 最长子词切分：DP 把词切成已知子词序列（如"长江三角洲"=长江+三角洲），继承子词整体读音。
//!  3. 逐字代表读音兜底：确保至少有结果。
//!
//! 单字读音索引 [`CharPinyinIndex`] 从**词典本身**派生（遍历标准音节查单字候选、按权重排序），
//! 不依赖 wind-reverse 的 pinyin_map.txt——代表读音 = 词典里权重最高的读音。

use std::collections::HashMap;

use wind_dict::cached::CachedDict;

use super::dag::{Dag, SegGraph};
use super::syllable::{STANDARD_SYLLABLES, SyllableTrie};

/// 整词读音消歧时笛卡尔积组合数上限（防生僻多音字长词性能塌方）。
const MAX_READING_COMBOS: usize = 64;

/// 汉字 → 读音反向索引。
///
/// 多音字 `char_all` 按词典权重降序，`char`（代表读音）= 权重最高者。
/// 由 [`CharPinyinIndex::build`] 遍历 [`STANDARD_SYLLABLES`] 查词典单字候选构建。
#[derive(Debug, Default)]
pub struct CharPinyinIndex {
    /// 汉字 → 代表读音（权重最高）
    char: HashMap<char, String>,
    /// 汉字 → 所有读音（按权重降序），用于多音字消歧
    char_all: HashMap<char, Vec<String>>,
}

impl CharPinyinIndex {
    /// 从词典构建索引：遍历标准音节，收集单字候选及其权重，按权重降序定读音。
    pub fn build(dict: &CachedDict) -> Self {
        // 每字暂存 (读音, 权重)；同字同音节多条（异体/多源）合并取最大权重
        let mut all: HashMap<char, Vec<(String, i32)>> = HashMap::new();
        for &syl in STANDARD_SYLLABLES {
            for (text, weight, _order) in dict.search(syl) {
                let mut chars = text.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    continue; // 仅单字
                };
                let entry = all.entry(c).or_default();
                if let Some(e) = entry.iter_mut().find(|(s, _)| s == syl) {
                    if weight > e.1 {
                        e.1 = weight;
                    }
                } else {
                    entry.push((syl.to_string(), weight));
                }
            }
        }

        let mut char = HashMap::with_capacity(all.len());
        let mut char_all = HashMap::with_capacity(all.len());
        for (c, mut list) in all {
            // 按权重降序，第 0 个即代表读音
            list.sort_by_key(|(_, w)| std::cmp::Reverse(*w));
            let readings: Vec<String> = list.into_iter().map(|(s, _)| s).collect();
            char.insert(c, readings[0].clone());
            char_all.insert(c, readings);
        }
        Self { char, char_all }
    }

    fn representative(&self, c: char) -> Option<&str> {
        self.char.get(&c).map(String::as_str)
    }

    fn readings(&self, c: char) -> Option<&[String]> {
        self.char_all.get(&c).map(Vec::as_slice)
    }
}

/// 按音节/词段拼接**带空格的音节码**（`ni hao`）。
///
/// 造词本就是「逐音节拼起来」，边界是白送的——此前被 `String::push_str` 丢掉，逼得下游用
/// DAG 反猜（甚至靠 410 音节暴力反查）。
///
/// 此前的形态是「扁平 code + 手工累积的 bitmask」两个独立值，有两个固有毛病，空格表示
/// 一并消除：
/// - **二者可以不一致**：`d4084b8` 已踩过「A 层的 code + B 层的 boundary」。空格串里
///   码与边界物理上不可分离，错配连写都写不出来。
/// - **64 字节天花板**：bitmask 装不下更长的拼接，只能整体降级为「无边界」，长词因此
///   一律拿不到边界。字符串没有这个限制。
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

    /// 追加一个**已带空格的多音节词段**（如整词 `ni hao`）。段内边界随空格原样并入，
    /// 无需平移——这正是空格表示取代 bitmask 左移的地方。
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

/// 为词语生成**带空格的全拼音节码**（`你好` → `ni hao`）。含无读音字符时返回 `None`。
///
/// 空格即音节边界，与 rime 源词库同形。落库时由
/// [`wind_store::wdict::split_spaced_code`] 拆成扁平 code + boundary
/// （语义同 `wind_dict::binformat::DictEntry::boundary`），供用户自造词从诞生起就带上
/// 边界——否则用户词是块「边界空洞」，双拼校验只能对其降级。
///
/// `dict` 为拼音系统词典（提供整词验证的真值表），`index` 为单字读音索引。
pub fn generate_word_pinyin(
    dict: &CachedDict,
    index: &CharPinyinIndex,
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
    index: &CharPinyinIndex,
    runes: &[char],
    word: &str,
) -> Option<String> {
    if runes.len() < 2 {
        return None;
    }
    // 收集每字读音列表，同时估算笛卡尔积规模
    let mut readings: Vec<&[String]> = Vec::with_capacity(runes.len());
    let mut combos = 1usize;
    for &r in runes {
        let rs = index.readings(r)?;
        if rs.is_empty() {
            return None;
        }
        combos *= rs.len();
        if combos > MAX_READING_COMBOS {
            return None;
        }
        readings.push(rs);
    }
    // 笛卡尔积枚举（按字典序，等价于按权重组合的优先级）
    let mut idxs = vec![0usize; runes.len()];
    loop {
        // 每字一音节（readings[i][pos] 即第 i 字选中的读音）。
        let mut b = SpacedCode::new(runes.len());
        for (i, &pos) in idxs.iter().enumerate() {
            b.push_syllable(&readings[i][pos]);
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
            if idxs[k] < readings[k].len() {
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
    index: &CharPinyinIndex,
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
/// `ambiguous` 与 `no_reading` **不是**两个可以任意组合的正交布尔：`no_reading` 为真时
/// 读音表整体缺席，`ambiguous`（「多解，已按读音权重择一」）的语义不成立，调用方须先看
/// `no_reading`。分成两个字段只是因为求解过程里它们在不同步骤得出。
pub struct BoundarySolve {
    /// 音节起点 bitmask，语义同 `DictEntry::boundary`。
    pub mask: u64,
    /// 约束筛完仍多解（或路径枚举被截断），已按读音权重择一。
    pub ambiguous: bool,
    /// `text` 含无读音字符 ⇒ **读音验证整体缺席**（不是「读音对不上」）。
    pub no_reading: bool,
}

/// 按「音节数 == 汉字数」求解 `(code, text)` 的音节边界。
///
/// 这是**导入闸口的第 4 层**（见 `docs/design/pinyin-entry-boundary-contract.md` §3.1）。
///
/// ## 为什么这不是猜
///
/// [`super::dag::Dag::maximum_match`] 只看 code，在等长路径间无从取舍（`xian` 切成
/// `xi|an` 还是 `xian`，覆盖字符数都是 4），故只能算猜。而这里手上有 `text`，汉字词的
/// **音节数恒等于汉字数**——`xianning` + 「西安宁」(3 字) 只有 `xi|an|ning` 一条 3 音节
/// 路径，`xian|ning` 因只有 2 音节被约束直接排除。切分由此从启发式降为可判定问题。
///
/// ## 与 [`generate_word_pinyin`] 的关系：方向相反，互补
///
/// 那个是「字 → 码」（多音字必须靠权重猜），这个是「码 → 切分」（读音已由词库作者写定，
/// 只需切分）。两者共用 [`CharPinyinIndex`]。**恰恰是最难的多音字，在有 code 的这条路上
/// 不构成问题**：`chongqing` + 「重庆」直接切出 `chong|qing`，无需知道「重」是多音字。
///
/// ## 返回 `None` 的两种含义，调用方必须分开处置
///
/// 本函数只管求解，不区分「非法」与「码太长装不下 bitmask」——后者由调用方在进来之前
/// 按 `code.len() > 64` 拦掉（那是**合法但无边界**，既定语义是降级为 0，见
/// [`wind_store::wdict::split_spaced_code`] 的同款契约），到这里的 `None` 一律是非法。
///
/// ## 判据①（每字须有读音）为什么只标记、不拒收
///
/// 它曾是一条早退：`text` 里但凡有一个字符查不到读音就整行判非法。这误伤了**完全合法**
/// 的一类词条——`zuo ←`、`dengyu ＝`（拼音码 → 符号候选），符号在拼音词典里当然没有
/// 单字读音，可 `zuo` 本身是个正经音节、`←` 恰好一个字符，边界 `0b1` 是**确定**的，
/// 根本不需要读音来定。见 issue #97。
///
/// 拦截「码表词库误导入拼音方案」的力量全部在**判据②**（`paths.is_empty()`）：`wgkq`
/// 切不出 1 个音节、`aaaa` 切不出 1 段，两个既有测试样例都是判据② 挡下的，判据① 对它们
/// 是多余的。判据① 唯一独占的战果是「中英/符号混排词条」，而那恰恰是上面那类合法用法。
///
/// ⚠️ 降级后逻辑是**自洽**的，不是绕过：无读音时 [`reading_score`] 里的
/// `index.readings(runes[i])?` 本就会让每条路径都不计分，`scored` 自然为空，于是落进
/// 下面那个早已存在、注释也早已写明「切分本身合法就不能否决」的降级分支。
pub fn boundary_by_char_count(
    index: &CharPinyinIndex,
    trie: &SyllableTrie,
    code: &str,
    text: &str,
) -> Option<BoundarySolve> {
    let runes: Vec<char> = text.chars().collect();
    if runes.is_empty() || code.is_empty() || code.len() > 64 {
        return None;
    }
    // 判据①（设计文档 §2.1）：每个字符都要有读音。**只作标记，不再拒收**（见上方说明）。
    // ⚠️ `readings` 只收**单字词典条目**，故这条同时也是「该字在本方案词典里存在」。
    let no_reading = runes.iter().any(|&c| index.readings(c).is_none());
    // **必须 `build_strict`**：这里推的是词条的**真值边界**（哪几个字节属于哪个字的读音），
    // 下面还要拿每个音节去比对该字的 readings。带模糊拼写层会凭空多出「用户错音」那些边，
    // 既可能撑爆 `MAX_BOUNDARY_PATHS` 把本来唯一的解判成 truncated，也让判据②
    // 「切得出与字数相符的音节序列」被错音串满足。见 `fuzzy::fuzzy_spellings`。
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
        // ★ 切分本身在音节图上合法，不能因此否决——否则会把「码没错、只是读音冷门」的
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
    index: &CharPinyinIndex,
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
        let pos = index.readings(runes[i])?.iter().position(|r| r == syl)?;
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
mod tests {
    use super::*;
    use wind_dict::codetable::CodetableDict;

    /// 用 (code, text, weight) 三元组建内存拼音词典。
    fn dict_from(entries: &[(&str, &str, i32)]) -> CachedDict {
        let mut d = CodetableDict::empty();
        for (code, text, weight) in entries {
            d.merge_single(code.to_string(), text.to_string(), *weight, 0);
        }
        CachedDict::Memory(d)
    }

    /// 扁平码（不关心边界的断言用）。
    fn gen_py(entries: &[(&str, &str, i32)], word: &str) -> Option<String> {
        gen_py_spaced(entries, word).map(|s| s.replace(' ', ""))
    }

    /// 引擎的原始产出：**带空格的音节码**。空格即边界，是本模块的真相源。
    fn gen_py_spaced(entries: &[(&str, &str, i32)], word: &str) -> Option<String> {
        let dict = dict_from(entries);
        let idx = CharPinyinIndex::build(&dict);
        generate_word_pinyin(&dict, &idx, word)
    }

    /// 落库口径：按 `wind_store::wdict::split_spaced_code` 拆成 `(flat, boundary)`，
    /// 即用户词表里最终存下的形态。
    fn gen_py_stored(entries: &[(&str, &str, i32)], word: &str) -> Option<(String, u64)> {
        gen_py_spaced(entries, word).map(|s| wind_store::wdict::split_spaced_code(&s))
    }

    /// 造词须同时产出音节边界——用户自造词的边界从此有来源，不再是「空洞」。
    /// 三条产码路径（整词消歧 / 子词切分 / 逐字兜底）都要带边界。
    ///
    /// 断言直接落在**带空格的产出**上：空格表示让切分肉眼可读，也免去了「bitmask 与 code
    /// 各存一份、可以不一致」的老毛病（`d4084b8` 踩过）。
    #[test]
    fn generate_word_pinyin_carries_boundary() {
        let entries = &[
            ("ni", "你", 100),
            ("hao", "好", 100),
            ("nihao", "你好", 500),
            ("chong", "重", 50),
            ("zhong", "重", 900),
            ("qing", "庆", 100),
            ("chongqing", "重庆", 800),
        ];
        // 整词命中
        assert_eq!(gen_py_spaced(entries, "你好").as_deref(), Some("ni hao"));
        // 整词消歧（重庆读 chongqing 而非 zhongqing）
        assert_eq!(
            gen_py_spaced(entries, "重庆").as_deref(),
            Some("chong qing")
        );
        // 单字：整串一个音节，无内部边界可标
        assert_eq!(gen_py_spaced(entries, "你").as_deref(), Some("ni"));
        // 逐字兜底（整词不在词典）
        assert_eq!(gen_py_spaced(entries, "你重").as_deref(), Some("ni zhong"));

        // 落库形态：key 扁平、边界随 value 存下
        assert_eq!(
            gen_py_stored(entries, "你好"),
            Some(("nihao".into(), 0b101))
        );
        assert_eq!(
            gen_py_stored(entries, "重庆"),
            Some(("chongqing".into(), 0b100001))
        );
        assert_eq!(
            gen_py_stored(entries, "你重"),
            Some(("nizhong".into(), 0b101))
        );
        // ⚠️ 单音节的 `0b1` 不经文本往返（见 wdict::single_syllable_boundary_is_lossy_by_design）。
        // 单音节无切分歧义，且 0 在消费端一律是「放行」，不会误杀。
        assert_eq!(gen_py_stored(entries, "你"), Some(("ni".into(), 0)));
    }

    /// 子词切分路径：段自身是多音节整词时，段内边界须并入全局，不能把整段当一个音节。
    ///
    /// 空格表示下这是纯拼接（`push_segment` 按空格展开）；此前是 bitmask 左移 `base`，
    /// 漏平移就会把长词的段内边界丢掉。
    #[test]
    fn subword_segmentation_preserves_inner_boundary() {
        let entries = &[
            ("ni", "你", 100),
            ("hao", "好", 100),
            ("nihao", "你好", 500),
            ("a", "啊", 100),
        ];
        // 「你好啊」整词不在词典 → 子词切分：段「你好」(ni hao) + 单字「啊」(a)。
        assert_eq!(
            gen_py_spaced(entries, "你好啊").as_deref(),
            Some("ni hao a")
        );
        assert_eq!(
            gen_py_stored(entries, "你好啊"),
            Some(("nihaoa".into(), 0b100101))
        );
    }

    /// 超长词（拼接 >64 字节）：bitmask 装不下，落库时整体降级为 0；但**带空格的产出
    /// 本身不受限**——这正是空格表示相对 bitmask 的增量，边界在传输/导出层面得以保全。
    #[test]
    fn overlong_word_keeps_spaces_though_mask_degrades() {
        let entries = &[("zhuang", "装", 100)];
        let word: String = std::iter::repeat_n('装', 12).collect(); // 12*6=72B
        let spaced = gen_py_spaced(entries, &word).expect("应能逐字兜底产码");
        assert_eq!(spaced.split(' ').count(), 12, "12 个音节全部保留");
        assert_eq!(
            gen_py_stored(entries, &word).unwrap().1,
            0,
            "落库时超 64B 整体降级为无边界（bitmask 的固有上限）"
        );
    }

    /// 多音字按权重择优：费→fei(1000) 而非 bi(50)，强→qiang(1000) 而非 jiang(80)。
    #[test]
    fn multi_pron_by_weight() {
        let entries = &[
            ("fei", "费", 1000),
            ("bi", "费", 50),
            ("qiang", "强", 1000),
            ("jiang", "强", 80),
            ("xiao", "晓", 1000),
        ];
        assert_eq!(gen_py(entries, "费").as_deref(), Some("fei"));
        assert_eq!(gen_py(entries, "强").as_deref(), Some("qiang"));
        // 整词不在词典 → 逐字代表读音兜底
        assert_eq!(gen_py(entries, "费晓强").as_deref(), Some("feixiaoqiang"));
    }

    /// 整词命中覆盖逐字代表读音：重→代表音 zhong，但"重庆"整词读 chongqing。
    #[test]
    fn whole_word_overrides_per_char() {
        let entries = &[
            ("zhong", "重", 1000),
            ("chong", "重", 80),
            ("qing", "庆", 1000),
            ("chongqing", "重庆", 500),
        ];
        assert_eq!(gen_py(entries, "重庆").as_deref(), Some("chongqing"));
    }

    /// 长词子词切分继承读音：长→代表音 zhang，但经"长江"+"三角洲"得 changjiangsanjiaozhou。
    #[test]
    fn subword_segmentation() {
        let entries = &[
            ("zhang", "长", 1000),
            ("chang", "长", 500),
            ("jiang", "江", 1000),
            ("san", "三", 1000),
            ("jiao", "角", 1000),
            ("zhou", "洲", 1000),
            ("changjiang", "长江", 600),
            ("sanjiaozhou", "三角洲", 500),
        ];
        assert_eq!(
            gen_py(entries, "长江三角洲").as_deref(),
            Some("changjiangsanjiaozhou")
        );
    }

    /// 简单整词命中。
    #[test]
    fn simple_whole_word() {
        let entries = &[
            ("ni", "你", 1000),
            ("hao", "好", 1000),
            ("nihao", "你好", 800),
        ];
        assert_eq!(gen_py(entries, "你好").as_deref(), Some("nihao"));
    }

    /// 含无读音字符 → None。
    #[test]
    fn unknown_char_returns_none() {
        let entries = &[("ni", "你", 1000)];
        // "你X"：X 无任何读音
        assert_eq!(gen_py(entries, "你X"), None);
    }
}
