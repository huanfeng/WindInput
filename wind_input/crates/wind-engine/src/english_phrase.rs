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
    /// # 排序
    ///
    /// `weight 降序 → 跨度升序 → 文本序`。主键是 weight 而非跨度，理由见函数体里的长注释
    /// （一句话：协调器会按 weight 统一重排，引擎内不改 weight 的排序会被冲掉）。
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
        // ★ **weight 降序 → 跨度升序 → 文本序**。跨度是次级键，不是主键。
        //
        // 主键必须是 weight，这是 AGENTS.md「跨组件硬约定」里的一条：**候选排序必须落到
        // weight，引擎内部只调顺序、不改 weight 的排序会被协调器的统一重排冲掉**。
        // `EnglishEngine` 没有覆写 `base_sort_ignores_weight()`（默认 false），于是英文
        // 方案那条路上 `candidate_display_order` 的键序是
        // `cmp_exact_first → by_weight → base_order → natural_order` —— 跨度一个都不在里面。
        //
        // 本模块**曾把跨度当主键**，实测的后果是两个作用域顺序不一致：`ip'pro` 在引擎侧
        // 首位是跨度 1 的 `iPad Pro`，到了英文方案首页却被 weight 序挤出去了；而快捷输入
        // 那条路（`update_mix_candidates`）完全不排序、原样透传，跨度序在那边还活着。
        // 同一串输入两处不同序，且单测测的是用户看不到的中间态。
        //
        // 另外两条路都不可行：`base_sort_ignores_weight() -> true` 会对**全部**英文候选
        // 生效（普通英文输入的词频排序一起变）；把跨度折进 `weight` 与
        // `mixed/engine.rs` 的「`weight` 只承载真实词频」相冲。
        //
        // 于是承认 weight 优先就是最终口径。跨度仍然有用——同权重时它决定谁更贴合所打的
        // 那几段，而词库里同权重的条目成片存在（出厂词组大量 weight 相同）。
        // 文本序兜底是为了定序：同分时次序不能随词库遍历顺序漂移，否则候选位置会在重建
        // 索引后莫名换位。
        hits.sort_by(|a, b| {
            b.1.weight
                .cmp(&a.1.weight)
                .then_with(|| a.0.cmp(&b.0))
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
    /// `RwLock<Option<..>>` 而非 `OnceLock`：**索引必须能被作废**。
    ///
    /// 关闭某本英文词库走的是**热摘**（`CodeTableEngine::set_dict_enabled` →
    /// `DictManager::unregister_layer`，返回 true = 目标已达成 ⇒ 引擎不重建）。索引若只建
    /// 一次且没有失效通路，就会继续召回已禁用词库里的词组——出厂 `en_ext` 一本就带 779/787
    /// 条，而同一串输入走原路径已经查不到它们了。症状是本仓反复记着的那种
    /// 「关了没反应，顺手改别的设置又好了」（改别的设置会触发 `reload_from_config` →
    /// `engines.clear()` → 引擎连同索引一起重建）。
    ///
    /// 用 `Arc` 包内层是为了让读取方**不必持锁**：查询在按键路径上，持读锁跑完整个线性扫
    /// 会和后台预热的写锁互相等。取一次 `Arc::clone` 就放锁。
    index: std::sync::RwLock<Option<std::sync::Arc<PhraseSegIndex>>>,
}

impl Default for LazyPhraseIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl LazyPhraseIndex {
    pub fn new() -> Self {
        Self {
            index: std::sync::RwLock::new(None),
        }
    }

    /// 取索引，必要时现场构建。
    ///
    /// 两段式取锁（先读后写）而不是全程持写锁：读路径在按键链路上，绝大多数调用都会命中
    /// 已建好的索引、只付一次读锁。竞态下两条线程可能各建一次，结果等价——比让按键线程
    /// 排在写锁后面便宜。
    pub fn get(&self, dm: &DictManager) -> std::sync::Arc<PhraseSegIndex> {
        if let Some(idx) = self
            .index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return std::sync::Arc::clone(idx);
        }
        let built = std::sync::Arc::new(PhraseSegIndex::build(dm));
        let mut w = self.index.write().unwrap_or_else(|e| e.into_inner());
        // 期间别的线程已经建好就用它的，保证同一时刻只有一份索引在被引用。
        if let Some(existing) = w.as_ref() {
            return std::sync::Arc::clone(existing);
        }
        *w = Some(std::sync::Arc::clone(&built));
        built
    }

    /// 作废索引，下次查询时重建。
    ///
    /// 调用点＝词库启用状态变更（`EnglishEngine::set_dict_enabled`）。词库热摘不重建引擎，
    /// 这是索引跟上词库的唯一通路。
    pub fn invalidate(&self) {
        *self.index.write().unwrap_or_else(|e| e.into_inner()) = None;
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

    /// 跨度是**次级**键：同权重时跳得少的排前面。
    ///
    /// ⚠️ 两条 weight 必须**相等**，本用例才测得到跨度。weight 不等的话主键就分出了胜负，
    /// 「有没有跨度这一级」根本看不出来。
    #[test]
    fn tighter_span_breaks_ties_within_the_same_weight() {
        let i = idx(&[
            ("iPhone 15 Pro Max", "iphone", 100),
            ("iPhone Pro", "iphone", 100),
        ]);
        assert_eq!(
            texts(&i, "ip'pro"),
            vec!["iPhone Pro", "iPhone 15 Pro Max"],
            "同权重下跳 0 个词的应排在跳 1 个词的前面"
        );
    }

    /// ★ 而 weight 是**主键**：权重更高的排前面，哪怕它跨度更大。
    ///
    /// 这条钉的是 AGENTS.md 那条硬约定的落地——协调器会按 weight 统一重排，引擎内序
    /// 若以跨度为主键，到了用户眼前就是另一个顺序（两个作用域还会各不相同）。
    /// 与上一条构成对照的两半：缺了它，「跨度优先」的旧实现同样能过上一条。
    #[test]
    fn weight_outranks_span() {
        let i = idx(&[
            ("iPhone 15 Pro Max", "iphone", 900),
            ("iPhone Pro", "iphone", 10),
        ]);
        assert_eq!(
            texts(&i, "ip'pro"),
            vec!["iPhone 15 Pro Max", "iPhone Pro"],
            "weight 是主键：高权重的排前面，跨度只在同权重时才说话"
        );
    }

    /// weight 主键在同跨度的条目之间同样生效（与 `weight_outranks_span` 互补：那条跨度
    /// 不同、这条跨度相同）。
    #[test]
    fn weight_orders_entries_of_equal_span() {
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
