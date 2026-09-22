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

/// 索引里的一条词组：**只存偏移，不存字符串**。
///
/// 字符串全部躺在 [`PhraseSegIndex`] 的三块 arena 里。这不是微优化——旧结构每条持有
/// `Vec<Box<str>>` + 两个 `Box<str>`，一条词组就是 4~6 次独立堆分配，而真机上这张表有
/// **18 万条**（用户自备英文词库，出厂只有 787 条）：约 90 万次小分配，每次都要付分配器
/// 头部与对齐填充，实测常驻 48 MB，其中真实数据不到三分之一。
///
/// 改成 arena 后整张表只有个位数次分配，字段也从「指针 + 容量」缩到定长偏移。
struct PhraseEntry {
    /// 本条小写词序列在 `lower` 中的起点（第 0 个词的起点）。
    lower_start: u32,
    /// 原文与编码在 `raw` 中的起点：原文在前，编码紧随其后，两者不留分隔。
    raw_start: u32,
    text_len: u32,
    code_len: u32,
    /// 本条各词的结束偏移在 `word_ends` 中的起点。
    words_start: u32,
    /// 词数，恒 ≥ 2（单词条目不进索引）。
    word_count: u32,
    weight: i32,
}

/// 英文词组分词索引：只收 `text` 含空白的词条。
///
/// ⚠️ 构建是 O(全表) 的（`DictManager::for_each_entry` 自己的注释就写着「绝不能出现在
/// 按键链路上」），故由 [`LazyPhraseIndex`] 守着 + 后台预热。
///
/// # 三块 arena
///
/// | | 存什么 | 谁读 |
/// |---|---|---|
/// | `lower` | 各词小写化后**首尾相接**（不留分隔符） | 匹配 |
/// | `raw` | 原文 + 编码首尾相接 | 产出候选 |
/// | `word_ends` | 每个词在 `lower` 中的结束偏移 | 切词 |
///
/// 词与词之间不留分隔符，是因为边界已由 `word_ends` 给出——再塞一个空格等于为 18 万条
/// 各付一个字节去表达一件已经表达过的事。
#[derive(Default)]
pub struct PhraseSegIndex {
    lower: String,
    raw: String,
    word_ends: Vec<u32>,
    entries: Vec<PhraseEntry>,
    /// [`Self::finish`] 调过没有。**只在 debug 构建里存在**。
    ///
    /// [`Self::search`] 的二分以「`entries` 按首词有序」为前提，而那是 `finish` 建立的；
    /// 漏调的后果是二分在无序数组上乱跳、静默少召回。
    ///
    /// 存一个布尔而不是在 `search` 里验一遍有序：后者是 O(n)，18 万条时**每次按键**都要
    /// 走一遍，正好把二分省下来的 1.48 ms 原样赔回去（靶机的 dev 变体走
    /// `[profile.dev-variant]`、`debug-assertions = false` 不受影响，但开发者本地
    /// `cargo run` / `cargo test` 是实打实地付）。有序性本身在 `finish` 末尾验一次就够。
    #[cfg(debug_assertions)]
    finished: bool,
}

impl PhraseSegIndex {
    /// 全表扫一次，挑出词组建索引。
    pub fn build(dm: &DictManager) -> Self {
        let mut me = Self::default();
        dm.for_each_entry(&mut |code, text, weight| me.push(code, text, weight));
        me.finish();
        me
    }

    /// 收完词条后的收尾：压实 arena + **按首词排序**。
    ///
    /// 排序是 [`Self::first_word_range`] 的前提，也就是 [`Self::search`] 的前提。单独成
    /// 函数而不是写在 `build` 里，是为了让测试夹具能走**同一条**收尾——夹具自己补一句
    /// `sort` 就又是一份会漂移的复制品（本模块的 `idx()` 夹具上一次就栽在这里：它自带
    /// 一份「≥2 个词才进索引」的判据，与 `push` 并存）。
    ///
    /// 忘了调它的后果是 `search` 静默少召回（二分在无序数组上乱跳），故 `search` 里挂了
    /// `debug_assert`——测试下必爆，不靠人记得。
    fn finish(&mut self) {
        // arena 按翻倍扩容，18 万条下尾部空洞可达数 MB，而本表建成后只读。
        self.lower.shrink_to_fit();
        self.raw.shrink_to_fit();
        self.word_ends.shrink_to_fit();
        self.entries.shrink_to_fit();
        // 比较闭包要读 `self.lower` / `self.word_ends`，而 `self.entries` 同时被可变借出。
        // 取出来排完再放回是最直白的解法：`word()` 不碰 `entries`，语义完全等价。
        let mut entries = std::mem::take(&mut self.entries);
        entries.sort_by(|a, b| self.word(a, 0).cmp(self.word(b, 0)));
        self.entries = entries;
        debug_assert!(
            self.first_words_are_sorted(),
            "排序之后 entries 仍不是按首词有序 —— 比较键写错了"
        );
        #[cfg(debug_assertions)]
        {
            self.finished = true;
        }
    }

    /// 首词以 `prefix` 开头的那一段。`entries` 按首词有序，故这些条目必然连续。
    ///
    /// 匹配规则 1 要求首段是第一个词的前缀（见 [`Self::search`]），于是**区间之外的条目
    /// 一条都不可能命中**，不必看。真机 18 万条词组下这是从「全表逐条 `starts_with`」
    /// 降到「两次二分 + 扫命中段」。
    fn first_word_range(&self, prefix: &str) -> &[PhraseEntry] {
        let lo = self.entries.partition_point(|e| self.word(e, 0) < prefix);
        // `lo` 起的条目首词都 ≥ prefix；以 prefix 开头的那些排在最前面（字典序下前缀
        // 恒小于任何以它开头的更长串），一个 `partition_point` 就切出上界。
        let rest = &self.entries[lo..];
        let n = rest.partition_point(|e| self.word(e, 0).starts_with(prefix));
        &rest[..n]
    }

    /// `entries` 是否按首词非降序。O(n)，只在 [`Self::finish`] 末尾验一次。
    fn first_words_are_sorted(&self) -> bool {
        self.entries
            .windows(2)
            .all(|w| self.word(&w[0], 0) <= self.word(&w[1], 0))
    }

    /// 收一条词条。**非词组（不足两个词）原样回滚**，不留痕迹。
    ///
    /// 判据是「text 里有空白」而不是「code 里有什么」：词边界只在 text 上（见模块文档
    /// 「查询读 text，不读 code」那一节）。
    ///
    /// 先写 arena 再回滚，而不是先数词数——数词数要先切一遍，切完还得再走一遍才能写进
    /// arena，等于对**全表**每条都多切一次。回滚只对被丢弃的那些条目付代价，而那是少数。
    fn push(&mut self, code: &str, text: &str, weight: i32) {
        let lower_start = self.lower.len() as u32;
        let words_start = self.word_ends.len() as u32;
        let mut word_count = 0u32;
        for w in text.split_whitespace() {
            // 逐字符写进 arena：`w.to_lowercase()` 会为每个词造一个临时 String，
            // 而这里每条词条有 2~5 个词、全表 18 万条。
            for ch in w.chars() {
                for lc in ch.to_lowercase() {
                    self.lower.push(lc);
                }
            }
            self.word_ends.push(self.lower.len() as u32);
            word_count += 1;
        }
        if word_count < 2 {
            self.lower.truncate(lower_start as usize);
            self.word_ends.truncate(words_start as usize);
            return;
        }
        let raw_start = self.raw.len() as u32;
        self.raw.push_str(text);
        self.raw.push_str(code);
        self.entries.push(PhraseEntry {
            lower_start,
            raw_start,
            text_len: text.len() as u32,
            code_len: code.len() as u32,
            words_start,
            word_count,
            weight,
        });
    }

    /// 第 `j` 个词的小写形态。`j` 必须 `< e.word_count`。
    ///
    /// 第 0 个词从 `lower_start` 起，其余从前一个词的结束偏移起——词在 arena 里首尾相接，
    /// 所以「上一个的 end」就是「这一个的 start」，不必另存起点。
    fn word(&self, e: &PhraseEntry, j: usize) -> &str {
        let ws = e.words_start as usize;
        let start = if j == 0 {
            e.lower_start as usize
        } else {
            self.word_ends[ws + j - 1] as usize
        };
        &self.lower[start..self.word_ends[ws + j] as usize]
    }

    /// 原文（带大小写与空格），上屏用。
    fn text(&self, e: &PhraseEntry) -> &str {
        let s = e.raw_start as usize;
        &self.raw[s..s + e.text_len as usize]
    }

    /// 词库里的原始编码。只为填进候选供调试段显示，匹配不读它。
    fn code(&self, e: &PhraseEntry) -> &str {
        let s = e.raw_start as usize + e.text_len as usize;
        &self.raw[s..s + e.code_len as usize]
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 本索引占的堆字节数（四块 arena 的容量之和）。
    ///
    /// ★ 打进预热日志，是这张表在真机上**唯一**的观测出口。
    /// 2026-09-22 查一次「服务占 100 MB」花了六轮 A/B 才定位到这里——当时日志只报条数
    /// （`phrases=180998`），而条数说明不了内存，恰恰是条数看着正常时内存最吓人。
    ///
    /// 容量而非长度：arena 按翻倍扩容，`build` 末尾虽已 `shrink_to_fit`，但报「实际占了
    /// 多少」才是观测该回答的问题。
    pub fn heap_bytes(&self) -> usize {
        self.lower.capacity()
            + self.raw.capacity()
            + self.word_ends.capacity() * size_of::<u32>()
            + self.entries.capacity() * size_of::<PhraseEntry>()
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
    /// **两次二分定位首词区间，然后只扫区间内**（见 [`Self::first_word_range`]）。规则 1
    /// 把首段钉死在第一个词上，区间外的条目连看都不用看。
    ///
    /// 这一步不是为出厂那 787 条做的——那点量怎么扫都行。真机上用户挂自备英文词库后
    /// 这张表是 **18 万条**，而查询在按键链路上：每按一个字母全表扫一遍，`ip` 这样的
    /// 短前缀尤其吃亏。二分之后扫描量只剩首词真正匹配的那几百条。
    pub fn search(&self, segs: &[String], limit: usize) -> Vec<Candidate> {
        if segs.is_empty() || limit == 0 {
            return Vec::new();
        }
        // O(1)：只问「finish 调过没有」。验有序是 O(n)，不能放在按键路径上，
        // 那一条在 `finish` 末尾做（见 `finished` 字段的文档）。
        #[cfg(debug_assertions)]
        assert!(
            self.finished,
            "建完索引忘了 finish() —— 二分会在无序数组上乱跳，静默少召回"
        );
        let mut hits: Vec<(usize, &PhraseEntry)> = Vec::new();
        for e in self.first_word_range(&segs[0]) {
            if let Some(span) = self.match_entry(e, segs) {
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
                .then_with(|| self.text(a.1).cmp(self.text(b.1)))
        });
        hits.truncate(limit);
        hits.into_iter()
            .enumerate()
            .map(|(i, (_, e))| Candidate {
                text: self.text(e).to_string(),
                code: self.code(e).to_string(),
                weight: e.weight,
                natural_order: i as i32,
                source: CandidateSource::English,
                ..Default::default()
            })
            .collect()
    }
}

impl PhraseSegIndex {
    /// 一条词组是否匹配这组段；匹配则返回**跨度** = 最后一段落在第几个词上。
    ///
    /// 跨度就是紧凑度：`ip'pro` 对 `iPhone 15 Pro` 跨度 2、对假想的 `iPhone Pro` 跨度 1，
    /// 后者更贴合所打的两段，该排前面。
    fn match_entry(&self, e: &PhraseEntry, segs: &[String]) -> Option<usize> {
        let n = e.word_count as usize;
        // 段比词还多 ⇒ 无论怎么跳都对不上。提前挡掉，省下后面的逐段扫。
        if segs.len() > n {
            return None;
        }
        // 规则 1：首段锚定第一个词。
        //
        // 走 `search` 进来时这条恒成立（`first_word_range` 已按它切过区间），**仍然保留**：
        // 它是本函数自身的契约，删掉的话函数就只在「调用方恰好先筛过」时才正确。
        // 代价是区间内每条多一次短前缀比较，与省下的 18 万次不在一个量级。
        if !self.word(e, 0).starts_with(segs[0].as_str()) {
            return None;
        }
        // 规则 2：其余段在 words[1..] 上保序贪心最左。
        let mut wi = 1usize;
        let mut span = 0usize;
        for seg in &segs[1..] {
            loop {
                if wi >= n {
                    return None;
                }
                let w = self.word(e, wi);
                wi += 1;
                if w.starts_with(seg.as_str()) {
                    span = wi - 1;
                    break;
                }
            }
        }
        Some(span)
    }
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

    /// 索引是否已经建出来了。
    ///
    /// 供「关着词组分词就不该付这笔内存」的守门测试用（`english.rs` 的
    /// `a_disabled_feature_never_builds_the_index`）。真机上这张表 12.7 MB，
    /// 而它是否存在只取决于两个开关的**或**，判据必须能被断言，不能只写在注释里。
    pub fn is_built(&self) -> bool {
        self.index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
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
                let idx = me.get(&dm);
                tracing::info!(
                    ms = t0.elapsed().as_millis(),
                    phrases = idx.len(),
                    // 条数说明不了内存：出厂 787 条与用户自备词库的 18 万条差两个数量级，
                    // 而后者曾在真机上常驻 48 MB。把字节数一并报出来。
                    heap_kb = idx.heap_bytes() / 1024,
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

    /// ⚠️ 夹具走**生产同一条** `push` + `finish`，不再自己复制一份「≥2 个词才进索引」的
    /// 判据、也不自己补排序——那种复制品曾与 `build` 并存，是典型的漂移隐患
    /// （改了一处另一处静默过期）。
    fn idx(pairs: &[(&str, &str, i32)]) -> PhraseSegIndex {
        let mut me = PhraseSegIndex::default();
        for (text, code, w) in pairs {
            me.push(code, text, *w);
        }
        me.finish();
        me
    }

    fn texts(i: &PhraseSegIndex, input: &str) -> Vec<String> {
        i.search(&split_segments(input, '\''), 20)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    /// 真机量级（18 万条）下二分窗口与全表扫的耗时对比。手动跑：
    /// `cargo test -p wind-engine --release --lib -- --ignored --nocapture bench_`
    ///
    /// 2026-09-22 本机实测：**窗口 5.1 µs / 全表 1.48 ms**，290 倍。1.48 ms 落在按键
    /// 链路上，每多打一个字母就再付一次——这才是做这一步的理由，不是「显得快一点」。
    ///
    /// `#[ignore]` 是因为它是**基准不是判据**：机器一换数字就变，拿它当回归门会变成
    /// 随机红。正确性由 `the_binary_search_window_returns_exactly_what_a_full_scan_would`
    /// 守，「有没有真的少扫」由 `the_first_word_window_is_exactly_the_prefix_block` 守。
    #[test]
    #[ignore = "基准，不参与常规回归"]
    fn bench_window_vs_full_scan() {
        let mut me = PhraseSegIndex::default();
        for i in 0..180_000u32 {
            let text = format!("word{i:06} beta gamma{i:04}");
            me.push(&format!("w{i}"), &text, (i % 1000) as i32);
        }
        me.finish();
        let segs = split_segments("word0123'gam", PHRASE_SEPARATOR);
        let t0 = std::time::Instant::now();
        for _ in 0..200 {
            std::hint::black_box(me.search(&segs, 20));
        }
        let windowed = t0.elapsed() / 200;
        let t1 = std::time::Instant::now();
        for _ in 0..200 {
            let mut hits = 0usize;
            for e in &me.entries {
                if me.match_entry(e, &segs).is_some() {
                    hits += 1;
                }
            }
            std::hint::black_box(hits);
        }
        let full = t1.elapsed() / 200;
        println!(
            "窗口 {windowed:?} / 全表 {full:?}  窗口条目数={}",
            me.first_word_range(&segs[0]).len()
        );
    }

    /// 首词有公共前缀的一族 —— 二分边界最容易错的地方（`ip` 的区间必须刚好收住
    /// `ipad`/`iphone`/`ipod`，既不漏 `ipod` 也不吃进 `internet`）。
    fn prefix_family() -> PhraseSegIndex {
        idx(&[
            ("iPad Pro", "ipadpro", 100),
            ("iPhone 15 Pro Max", "iphone", 100),
            ("iPhone 15 Pro", "iphone", 90),
            ("iPod Touch", "ipod", 80),
            ("Internet Explorer", "ie", 70),
            ("Buenos Aires", "buenosaires", 60),
            ("Mac OS X", "macosx", 50),
            ("Zulu Time", "zulu", 40),
            ("北京 大学", "bjdx", 30),
        ])
    }

    /// 朴素全表扫，只回答「命中哪些」。
    ///
    /// 刻意**不复制排序逻辑**：顺序自有 `weight_outranks_span` 那几条用例守着，这里再抄
    /// 一份三级比较器只会多一个会漂移的副本。用集合比对，测的是「二分有没有漏/多」。
    fn linear_hits(i: &PhraseSegIndex, input: &str) -> std::collections::BTreeSet<String> {
        let segs = split_segments(input, PHRASE_SEPARATOR);
        if segs.is_empty() {
            return Default::default();
        }
        i.entries
            .iter()
            .filter(|e| i.match_entry(e, &segs).is_some())
            .map(|e| i.text(e).to_string())
            .collect()
    }

    fn search_hits(i: &PhraseSegIndex, input: &str) -> std::collections::BTreeSet<String> {
        i.search(&split_segments(input, PHRASE_SEPARATOR), 100)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    /// ★ 二分窗口必须与全表扫召回同一批条目。
    ///
    /// 反向验证（变异）：删掉 `finish()` 里那句 `sort_by`，本用例在 `i'pro` / `ipo'touch`
    /// 这类落在区间边界的输入上立刻红（debug 构建还会先撞上 `search` 的 `debug_assert`）。
    #[test]
    fn the_binary_search_window_returns_exactly_what_a_full_scan_would() {
        let i = prefix_family();
        for input in [
            "i'pro",     // 区间跨 ipad/iphone/ipod 三族
            "ip'pro",    //
            "ipa'pro",   // 只剩 iPad
            "iph'max",   // 只剩一条
            "ipo'touch", // 区间**右端**那条，最容易被上界切掉
            "int'exp",   // 区间左邻，不得被吃进来
            "b'air",     // 全表最前
            "z'time",    // 全表最后（ASCII 段）
            "北'大",     // 多字节首词，排在全部 ASCII 之后
            "zz'x",      // 首词无人匹配 ⇒ 空区间
            "'",         // 空段全被剔除 ⇒ 空结果
        ] {
            assert_eq!(
                search_hits(&i, input),
                linear_hits(&i, input),
                "输入 {input:?} 上二分窗口与全表扫不一致"
            );
        }
    }

    /// ★ 窗口本身的边界：`ip` 收住三族、不吃 `internet`。
    ///
    /// 与上一条的分工：那条测「结果对不对」，这条测「少看了多少」——窗口若退化成全表，
    /// 结果照样正确，而本功能（把按键路径上的 18 万条扫描降下来）就白做了。
    #[test]
    fn the_first_word_window_is_exactly_the_prefix_block() {
        let i = prefix_family();
        let win: std::collections::BTreeSet<String> = i
            .first_word_range("ip")
            .iter()
            .map(|e| i.text(e).to_string())
            .collect();
        assert_eq!(
            win,
            [
                "iPad Pro",
                "iPhone 15 Pro",
                "iPhone 15 Pro Max",
                "iPod Touch"
            ]
            .into_iter()
            .map(String::from)
            .collect::<std::collections::BTreeSet<_>>()
        );
        assert!(
            i.first_word_range("zz").is_empty(),
            "无人匹配的首词该给出空窗口"
        );
        assert_eq!(
            i.first_word_range("").len(),
            i.entries.len(),
            "空前缀是全表 —— 用户刚打下分词符那一刻走的就是它"
        );
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

    /// ★ **每条词组的堆开销上界**。这是本模块唯一的内存护栏。
    ///
    /// 缘起：真机上这张表有 18 万条（用户自备英文词库，出厂只有 787 条），旧结构每条
    /// 持有 `Vec<Box<str>>` + 两个 `Box<str>`，约 90 万次小分配，实测常驻 **48 MB**，
    /// 而它被建了两份（english 方案 + 混输的 english 子引擎）⇒ 96 MB。
    ///
    /// 上界取 120 字节/条：arena 版实测约 78（entry 28 + word_ends 10 + lower 15 + raw 25），
    /// 留出的余量够容纳词长分布的波动，但**挡得住退回 `Box<str>`**——那一版光
    /// `Vec` + 两个 `Box` 的头部就已经是 56 字节，加上每词一次分配的分配器开销必然超线。
    ///
    /// ⚠️ 样本必须**足够多且带多词条目**：条数太少时 arena 的翻倍扩容尾巴会摊到分母上，
    /// 测出来的是扩容策略而不是结构本身。
    #[test]
    fn heap_cost_per_phrase_stays_within_budget() {
        let pairs: Vec<(String, String, i32)> = (0..2000)
            .map(|i| {
                (
                    format!("iPhone {i} Pro Max Ultra"),
                    format!("iphone{i}"),
                    100,
                )
            })
            .collect();
        let mut me = PhraseSegIndex::default();
        for (text, code, w) in &pairs {
            me.push(code, text, *w);
        }
        me.lower.shrink_to_fit();
        me.raw.shrink_to_fit();
        me.word_ends.shrink_to_fit();
        me.entries.shrink_to_fit();

        // ★ 这一条堵的是上面那条护栏的**漏网口**：`heap_bytes` 只统计四块 arena，
        // 谁要是往 `PhraseEntry` 里加回一个 `Box<str>`（16 B）或 `Vec<Box<str>>`（24 B），
        // 那份堆内存**不会被 `heap_bytes` 统计到**，上面的预算断言照样绿。
        // 用 `size_of` 直接钉住「条目里不许出现指针」，这是编译期事实，绕不过去。
        assert!(
            size_of::<PhraseEntry>() <= 28,
            "PhraseEntry 涨到 {} 字节——加了指针字段？条目必须只存定长偏移",
            size_of::<PhraseEntry>()
        );
        assert_eq!(me.len(), 2000, "全部应进索引（每条 5 个词）");
        let per = me.heap_bytes() / me.len();
        assert!(
            per < 120,
            "每条词组堆开销 {per} 字节，超出预算 120——退回 per-entry 堆分配了？\n             （总计 {} KB / {} 条）",
            me.heap_bytes() / 1024,
            me.len()
        );
    }

    /// 多字节字符不能把偏移算错：arena 存的是**字节**偏移，切片边界必须落在字符边界上。
    /// 旧结构各词独立成串，天然不会切错；arena 把它们首尾相接之后这就成了真实风险。
    #[test]
    fn multibyte_words_slice_on_char_boundaries() {
        let i = idx(&[("Café Noir Über", "cafe", 100)]);
        assert_eq!(texts(&i, "caf'noir"), vec!["Café Noir Über"]);
        // 小写化后 Ü → ü，段用小写打
        assert_eq!(texts(&i, "caf'üb"), vec!["Café Noir Über"]);
    }

    /// 段比词多时不命中——别让 `a'b'c` 匹配上只有两个词的条目。
    #[test]
    fn more_segments_than_words_never_matches() {
        let i = idx(&[("Buenos Aires", "buenosaires", 100)]);
        assert!(texts(&i, "bue'air'x").is_empty());
    }
}
