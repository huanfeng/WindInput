//! 英文词组分词输入：分词符切段，每段只打前缀。
//!
//! 论坛 t42。`envi'deg` → `environmental degradation`，`ip'max` → `iPhone 15 Pro Max`。
//!
//! # ★ 查询读 `text`，不读 `code`
//!
//! 出厂词库里词组的编码**有两种并存的方案**（实测 787 条）：
//!
//! | 方案 | 条数 | 例 |
//! |---|---|---|
//! | 拼接式 | 498 | `Buenos Aires` → `buenosaires` |
//! | 共同前缀式 | 289 | `iPhone 15 Pro Max` → `iphone`（code 里没有后段词） |
//!
//! 共同前缀式的 code 压根不含后段词，任何「把 code 拆成各段码」的方案（如 t42 原帖设想的
//! `p1..p10` 分列）对它们直接失效。而**词边界一直明摆在 `text` 的空格里**——按 text 切分
//! 两种编码统一处理，用户自备词库的编码方案也不影响本功能。
//!
//! 代价是索引与 code 无关，不能靠 Trie 前缀剪枝，只能线性扫词组子集。出厂 787 条下这不是
//! 问题（见 [`PhraseSegIndex::search`] 的量级说明）。
//!
//! # 这个功能解决的不是「词组够不着」
//!
//! 词组**本来就能靠前缀召回**（打 `ipho` 出 7 条 iPhone 变体、`macos` 出 9 条 macOS 变体）。
//! 本功能的价值是**在共同前缀下精确定位**：选 `iPhone 15 Pro Max` 原本要翻页，
//! `ip'max` 一步到位。那 289 条共同前缀式条目是主要受益者。

use wind_candidate::{Candidate, CandidateSource};

/// 词组分词符。
///
/// 不做成可配项的理由见 `EnglishGlobal::phrase_seg` 的文档。一句话：真正的备选是 `.`
/// （t153 要拿它作模糊万能键），两者将来要一起定，在那之前多一个旋钮只是多一处要同步的真相。
pub const PHRASE_SEPARATOR: char = '\'';

use wind_dict::DictManager;

/// 索引里的一条词组。
///
/// 存 `Box<str>` 而非 `String`：本表一次建成后只读，`String` 的容量字段在这里是纯浪费，
/// 而词组条数虽小、每条又带一个词序列，省下的是每词一个 usize。
struct PhraseEntry {
    /// 预切分并小写化的词序列。**匹配只读这里**，不再每次查询重切。
    words: Vec<Box<str>>,
    /// 原文（带大小写与空格），上屏用。
    text: Box<str>,
    /// 词库里的原始编码。只为填进候选供调试段显示，匹配不读它。
    code: Box<str>,
    weight: i32,
}

/// 英文词组分词索引：只收 `text` 含空白的词条。
///
/// ⚠️ 构建是 O(全表) 的（`DictManager::for_each_entry` 自己的注释就写着「绝不能出现在
/// 按键链路上」），故由 [`LazyPhraseIndex`] 用 `OnceLock` 守着 + 后台预热。
pub struct PhraseSegIndex {
    entries: Vec<PhraseEntry>,
}

impl PhraseSegIndex {
    /// 全表扫一次，挑出词组建索引。
    pub fn build(dm: &DictManager) -> Self {
        let mut entries = Vec::new();
        dm.for_each_entry(&mut |code, text, weight| {
            // 判据是「text 里有空白」而不是「code 里有什么」：词边界只在 text 上。
            let words: Vec<Box<str>> = text
                .split_whitespace()
                .map(|w| w.to_lowercase().into_boxed_str())
                .collect();
            if words.len() < 2 {
                return;
            }
            entries.push(PhraseEntry {
                words,
                text: text.into(),
                code: code.into(),
                weight,
            });
        });
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 按分好的段查词组。`segs` 已由调用方切分并小写化，**空段须已剔除**
    /// （见 [`split_segments`]）。
    ///
    /// # 匹配规则
    ///
    /// 1. **首段锚定第一个词**：`segs[0]` 必须是 `words[0]` 的前缀。
    ///    不锚定的话 `pro` 会命中一切含 Pro 的词组 —— 候选爆炸，且与「从头打起」的心智不符。
    /// 2. **其余段保序子序列匹配**，允许跳过中间的词（2026-09-18 拍板：优先保效果）。
    ///    于是 `ip'pro` 能命中 `iPhone 15 Pro`，不必写成 `ip'15'pro`。
    /// 3. 尾部未被任何段覆盖的词**不要求匹配**——那是前缀补全语义，同打 `hel` 出 `hello`。
    ///
    /// 贪心最左匹配：对「是否存在子序列」这个判定，贪心最左与最优解等价（经典结论），
    /// 同时它给出的结束位置是所有可行匹配里最小的，正好就是排序要的紧凑度。
    ///
    /// # 量级
    ///
    /// 线性扫词组子集。出厂 787 条 × 平均 2~3 词，每条只做几次 `starts_with`——
    /// 远比 `Trie::search_prefix` 对短前缀做的整棵子树 `collect_all` + 排序便宜。
    /// 即便换上 t42 作者那份 2W 条词组的词库也只慢一个数量级，仍在亚毫秒。
    pub fn search(&self, segs: &[String], limit: usize) -> Vec<Candidate> {
        if segs.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut hits: Vec<(usize, &PhraseEntry)> = Vec::new();
        for e in &self.entries {
            if let Some(span) = match_entry(&e.words, segs) {
                hits.push((span, e));
            }
        }
        // 跨度升序（跳得越少越紧凑）→ weight 降序 → 文本序（定序，避免同分时次序随词库
        // 遍历顺序漂移，那会让候选位置在重建索引后莫名换位）。
        hits.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.weight.cmp(&a.1.weight))
                .then_with(|| a.1.text.cmp(&b.1.text))
        });
        hits.truncate(limit);
        hits.into_iter()
            .enumerate()
            .map(|(i, (_, e))| Candidate {
                text: e.text.to_string(),
                code: e.code.to_string(),
                weight: e.weight,
                natural_order: i as i32,
                source: CandidateSource::English,
                ..Default::default()
            })
            .collect()
    }
}

/// 一条词组是否匹配这组段；匹配则返回**跨度** = 最后一段落在第几个词上。
///
/// 跨度就是紧凑度：`ip'pro` 对 `iPhone 15 Pro` 跨度 2、对假想的 `iPhone Pro` 跨度 1，
/// 后者更贴合所打的两段，该排前面。
fn match_entry(words: &[Box<str>], segs: &[String]) -> Option<usize> {
    // 段比词还多 ⇒ 无论怎么跳都对不上。提前挡掉，省下后面的逐段扫。
    if segs.len() > words.len() {
        return None;
    }
    // 规则 1：首段锚定第一个词。
    if !words[0].starts_with(segs[0].as_str()) {
        return None;
    }
    // 规则 2：其余段在 words[1..] 上保序贪心最左。
    let mut wi = 1usize;
    let mut span = 0usize;
    for seg in &segs[1..] {
        loop {
            let w = words.get(wi)?;
            wi += 1;
            if w.starts_with(seg.as_str()) {
                span = wi - 1;
                break;
            }
        }
    }
    Some(span)
}

/// 按分词符切段并小写化，**剔除空段**。
///
/// 空段必须剔除而不是让它匹配失败：用户打到 `ip'` 时最后一段天然是空的，若让它参与匹配，
/// 候选会在每次按下分词符的那一刻整片消失，再打一个字母又回来——打字过程中闪烁。
/// 中间的空段（`ip''pro`，多按一下）同理，按「手滑」宽容处理。
pub fn split_segments(input: &str, sep: char) -> Vec<String> {
    input
        .split(sep)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// 懒建的词组索引 + 后台预热。
///
/// 与 `codetable/sentence.rs` 的 [`LazyTables`](crate::codetable::sentence) 同款：
/// 「懒」只解决**要不要付**这笔全表扫描，不解决**在哪条线程上付**——不预热的话它会恰好
/// 落在用户第一次按下分词符的那一刻。预热**不改变任何取值**，`OnceLock::get_or_init`
/// 保证两条线程抢到同一份结果；预热没跑完就打到了，按键线程在 `get_or_init` 上等，
/// 那是与「不预热」持平的最坏情况，不会更差。
pub struct LazyPhraseIndex {
    index: std::sync::OnceLock<PhraseSegIndex>,
}

impl Default for LazyPhraseIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl LazyPhraseIndex {
    pub fn new() -> Self {
        Self {
            index: std::sync::OnceLock::new(),
        }
    }

    pub fn get(&self, dm: &DictManager) -> &PhraseSegIndex {
        self.index.get_or_init(|| PhraseSegIndex::build(dm))
    }

    /// 把索引构建推给后台线程。由引擎构建完成时调用。
    pub fn prewarm(self: &std::sync::Arc<Self>, dm: std::sync::Arc<DictManager>) {
        let me = std::sync::Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("english-phrase-warm".into())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let n = me.get(&dm).len();
                tracing::info!(
                    ms = t0.elapsed().as_millis(),
                    phrases = n,
                    "英文词组分词：后台预热完成"
                );
            });
        if let Err(e) = spawned {
            // 不致命：索引仍会在首次分词查询时现场构建，只是那一下会卡。
            tracing::warn!("英文词组分词：预热线程启动失败（{e}），退回首次查询时现场构建");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(pairs: &[(&str, &str, i32)]) -> PhraseSegIndex {
        let entries = pairs
            .iter()
            .filter_map(|(text, code, w)| {
                let words: Vec<Box<str>> = text
                    .split_whitespace()
                    .map(|x| x.to_lowercase().into_boxed_str())
                    .collect();
                (words.len() >= 2).then(|| PhraseEntry {
                    words,
                    text: (*text).into(),
                    code: (*code).into(),
                    weight: *w,
                })
            })
            .collect();
        PhraseSegIndex { entries }
    }

    fn texts(i: &PhraseSegIndex, input: &str) -> Vec<String> {
        i.search(&split_segments(input, '\''), 20)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    /// 两种编码方案都靠 text 命中——这是整个设计的立足点。
    #[test]
    fn matches_both_encoding_schemes() {
        let i = idx(&[
            ("Buenos Aires", "buenosaires", 100), // 拼接式
            ("iPhone 15 Pro", "iphone", 100),     // 共同前缀式：code 里没有 15/Pro
        ]);
        assert_eq!(texts(&i, "bue'air"), vec!["Buenos Aires"]);
        assert_eq!(texts(&i, "ip'15"), vec!["iPhone 15 Pro"]);
    }

    /// 跳词：`ip'pro` 越过 `15` 命中 `Pro`。这是 2026-09-18 拍板的「优先保效果」。
    #[test]
    fn skips_intervening_words() {
        let i = idx(&[("iPhone 15 Pro", "iphone", 100)]);
        assert_eq!(texts(&i, "ip'pro"), vec!["iPhone 15 Pro"]);
    }

    /// 跳词不等于乱序：段序必须与词序一致。
    #[test]
    fn skipping_still_requires_order() {
        let i = idx(&[("Mac OS X Snow Leopard", "macosx", 100)]);
        assert_eq!(texts(&i, "mac'snow"), vec!["Mac OS X Snow Leopard"]);
        // `leopard` 在 `snow` 之后，反过来打就不该命中。
        assert!(
            texts(&i, "mac'leo'snow").is_empty(),
            "段序与词序相反时不得命中"
        );
    }

    /// 首段锚定第一个词：中段词不能当入口。
    #[test]
    fn first_segment_must_anchor_the_first_word() {
        let i = idx(&[("iPhone 15 Pro", "iphone", 100)]);
        assert!(
            texts(&i, "pro'").is_empty(),
            "`pro` 不是首词前缀，不得从中段进入"
        );
    }

    /// 跨度小的排前面：同样两段，跳得少的更贴合所打内容。
    ///
    /// ⚠️ 两条的 weight 必须**反向**拉开（紧凑那条更低），否则本用例对排序主键的顺序无感：
    /// weight 相等时，把跨度降到 weight 之后仍会 fallback 回跨度、结果一模一样，
    /// 于是「跨度优先」和「weight 优先」两种实现都能过——变异验证里这条实测不变红。
    #[test]
    fn tighter_span_ranks_first() {
        let i = idx(&[
            ("iPhone 15 Pro Max", "iphone", 900),
            ("iPhone Pro", "iphone", 10),
        ]);
        assert_eq!(
            texts(&i, "ip'pro"),
            vec!["iPhone Pro", "iPhone 15 Pro Max"],
            "跳 0 个词的应排在跳 1 个词的前面，哪怕它 weight 低得多"
        );
    }

    /// 跨度相同时才轮到 weight。反向对照：没有这条，「只按 weight 排」也能过上一条。
    #[test]
    fn weight_breaks_ties_within_the_same_span() {
        let i = idx(&[
            ("iPhone 15 Pro", "iphone", 10),
            ("iPhone 16 Pro", "iphone", 900),
        ]);
        assert_eq!(
            texts(&i, "ip'pro"),
            vec!["iPhone 16 Pro", "iPhone 15 Pro"],
            "同跨度下按 weight 降序"
        );
    }

    /// 末尾空段被剔除：打到 `ip'` 的那一刻候选不该整片消失。
    #[test]
    fn trailing_empty_segment_is_dropped() {
        let i = idx(&[("iPhone 15 Pro", "iphone", 100)]);
        assert_eq!(split_segments("ip'", '\''), vec!["ip".to_string()]);
        assert_eq!(texts(&i, "ip'"), vec!["iPhone 15 Pro"]);
    }

    /// 单词条目不进索引——它们走原本的 Trie 前缀匹配，不该在这里被重复召回。
    #[test]
    fn single_word_entries_are_not_indexed() {
        let i = idx(&[
            ("hello", "hello", 100),
            ("Buenos Aires", "buenosaires", 100),
        ]);
        assert_eq!(i.len(), 1);
    }

    /// 段比词多时不命中——别让 `a'b'c` 匹配上只有两个词的条目。
    #[test]
    fn more_segments_than_words_never_matches() {
        let i = idx(&[("Buenos Aires", "buenosaires", 100)]);
        assert!(texts(&i, "bue'air'x").is_empty());
    }
}
