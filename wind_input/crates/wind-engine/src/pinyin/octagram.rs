//! 用 librime-octagram `.gram` 模型实现 [`Grammar`]。
//!
//! 查询流程复刻 `ref/librime-octagram/src/octagram.cc::Query`：
//! 取前文尾部 ≤N 字与当前词首部 ≤N 字，把 context 从最长逐字缩短、每轮查一次，
//! 对所有命中取**最大分**。
//!
//! 分数的**量纲映射不能照抄**，见 [`OctagramConfig`] 的长注释。

use std::path::Path;

use wind_dict::gramdb::{
    self, EncodeBuf, GramDb, MAX_ENCODED_UNICODE, MAX_RESULTS, VALUE_SCALE, encode_chars,
};

use super::grammar::Grammar;

/// 打分参数。
///
/// ## ★★★ 为什么不能照抄 octagram 的常数
///
/// octagram 的语义是：**未命中** `= non_collocation_penalty (−12)`，
/// **命中** `= ln(频次) + collocation_penalty (−12)`。两者共用同一个 −12，
/// 所以那个常数实际是**每转移的固定罚**，而 `ln(频次)` 是纯增益。
///
/// 而我们这一侧 [`super::lattice::WORD_PENALTY`]（= 3.0）**已经在扮演每词固定罚**，
/// 且它是连同 `DICT_TOTAL` 一起被 `pinyin_eval` 标定出来的。直接照搬 −12，
/// 每词罚会变成 15，词数偏好整个崩掉。
///
/// 量纲也对不上：librime 的 `entry_weight = ln(freq)`，我们的
/// `log_prob = ln(freq / DICT_TOTAL)`，差着 `ln(DICT_TOTAL) ≈ 19.3`。
///
/// ## ★★★ 两条约束，以及它们为什么只能靠 `weight` 调和
///
/// 打分形态是 `weight × (ln(频次) − baseline)`，未命中时 `ln` 部分取 0，
/// 于是未命中恰为 `−weight × baseline`。这个式子要同时满足两条约束：
///
/// **① 未命中不能优于命中。** 否则奖励的是「碎片」——完全没有搭配记录的组合拿 0 分，
/// 反而赢过有记录但频次中等的正确词组。实测教训：一版让未命中返回 0 之后，
/// 「建议修改」输给了「见一修改」、「他的意思就是」输给了「他的一死就是」。
///
/// **② 不能每转移重罚。** 否则按词数惩罚长句。实测教训：`weight=1, baseline=12`
/// （即照搬 octagram 的 −12）时，`pinyin_eval` 的 D 类（简拼混合**长句**）
/// top-1 从 12.20% 崩到 3.90%，而 A/B/C（短词）纹丝不动——差异恰好落在词数上。
///
/// librime 能直接用 −12 满足①而不触发②，是因为它的 `entry_weight = ln(freq)` 是
/// **正值**（6.9~19.7），词数增加时 `Σln(freq)` 同步增长、把固定罚抵消掉；
/// 我们的 `log_prob = ln(freq / DICT_TOTAL)` 本来就是负的，抵消不了。
///
/// ⇒ 两条约束并不真正冲突，冲突的是**量级**。保持 octagram 的符号语义（①），
/// 把 `weight` 压小让每词额外罚落在 1~2 而非 12（②）。
/// `baseline` 取实测 `ln(频次)` 中位数（8.34，设计文档 §2.2.3b）。
///
/// - `weight = 0` ⇒ **完全无影响**，逐位退化回接入前。这是标定的安全起点，也是当前默认。
#[derive(Debug, Clone)]
pub struct OctagramConfig {
    /// 整体权重。**0 = 关闭**（逐位等价于不挂模型）。
    pub weight: f64,
    /// 命中时的零点：`ln(频次)` 高于它得正分、低于它得负分。
    /// 取实测中位数 8.34，使命中项的期望≈0，从而不改变词数偏好。
    pub baseline: f64,
    /// context 与 word 各取几个字，对齐 `collocation_max_length`。
    pub collocation_max_length: usize,
    /// 搭配总长不足此值且非整串匹配时，额外扣 [`Self::weak_extra_penalty`]。
    ///
    /// ⚠️ octagram 默认 3，那是给**词级**模型（bgw）用的。
    /// 我们实测 `bgc` 是纯 2-gram（设计文档 §2.2.3d），搭配总长恒为 2，
    /// 默认 3 会让**每一次命中都被判为 weak**。故这里默认 2。
    pub collocation_min_length: usize,
    /// 弱搭配的额外扣分，对齐 `weak_collocation_penalty − collocation_penalty`（= 12）。
    pub weak_extra_penalty: f64,
    /// 句末搭配的额外扣分，对齐 `rear_penalty − collocation_penalty`（= 6）。
    pub rear_extra_penalty: f64,
}

impl Default for OctagramConfig {
    fn default() -> Self {
        Self {
            // 默认关闭：接上模型但不改变任何结果，留给标定逐步放开。
            weight: 0.0,
            // 实测 ln(频次) 的中位数（设计文档 §2.2.3b）
            baseline: 8.34,
            collocation_max_length: 4,
            collocation_min_length: 2,
            weak_extra_penalty: 12.0,
            rear_extra_penalty: 6.0,
        }
    }
}

/// context 逐级缩短后在 trie 中的一个落点。
///
/// `ctx_ptr` 必须一并记住：`whole` 判据（整串匹配豁免 weak 罚）问的是
/// 「**这一级是不是最长的那一级**」，而最长级可能 traverse 失败被跳过，
/// 此时数组下标 0 已经不是最长级了。用 `ctx_ptr == 0` 判才是对的。
#[derive(Clone, Copy)]
struct CtxLevel {
    node: usize,
    /// 该级 context 的字符数，参与 `collocation_len` 计算。
    ctx_len: usize,
    /// 该级在编码串中的起始字节偏移，`0` 即最长级。
    ctx_ptr: usize,
}

/// 每线程的搭配查询缓存。
///
/// ## 为什么会有重复劳动
///
/// `decode_beam` 的循环是 `for node { for src } }`，而 [`OctagramGrammar::best_collocation`]
/// 内部天然分成互不相干的两半：
///
/// - **context 半**（取尾部 n 字 → 编码 → 逐级 traverse）只依赖 context，
///   却在该起点下的**每个 node** 上被重算一遍；
/// - **word 半**（取首部 n 字 → 编码）只依赖 `node.word`，
///   却在 beam 的**每条线**上被重算一遍。
///
/// 两者都是纯函数的结果，缓存起来**不改变任何打分**——所以本优化的验收判据是硬的：
/// 评测指标逐位不变，只有耗时下降。
///
/// ## 容量与失效
///
/// context 侧取 `BEAM_WIDTH + 1 = 8` 格：内层一轮最多轮询 beam 宽度条不同的
/// context，多一格避免边界上整轮失效。word 侧只需**单格**——内层循环里 word 恒定，
/// 命中率天然是 `(线数−1)/线数`。
///
/// `db_id` 是 [`GramDb`] 的地址：同进程可能同时活着多个 grammar 实例（不同方案），
/// 换实例时整体清空，宁可重算也不能跨模型串用落点。
struct QueryCache {
    db_id: usize,
    ctx_keys: Vec<String>,
    ctx_levels: Vec<Vec<CtxLevel>>,
    ctx_next: usize,
    word_key: String,
    word_buf: EncodeBuf,
    /// word 首部 n 字是否就是整个 word（`is_rear` 分支要用）。
    word_full: bool,
    word_valid: bool,
}

/// context 缓存格数：beam 宽度 7 + 1。与 `viterbi::BEAM_WIDTH` 无编译期耦合
/// （那是解码器的参数、这是缓存的容量），改那边不必改这边，只是命中率会变。
const CTX_SLOTS: usize = 8;

impl QueryCache {
    fn new() -> Self {
        Self {
            db_id: 0,
            ctx_keys: Vec::with_capacity(CTX_SLOTS),
            ctx_levels: Vec::with_capacity(CTX_SLOTS),
            ctx_next: 0,
            word_key: String::new(),
            word_buf: EncodeBuf::default(),
            word_full: false,
            word_valid: false,
        }
    }

    /// 换了 `GramDb` 实例就整体作废——落点是相对某棵 trie 的，跨模型复用会给出错误分数。
    fn bind(&mut self, db_id: usize) {
        if self.db_id != db_id {
            self.db_id = db_id;
            self.ctx_keys.clear();
            self.ctx_levels.clear();
            self.ctx_next = 0;
            self.word_valid = false;
        }
    }
}

thread_local! {
    static QUERY_CACHE: std::cell::RefCell<QueryCache> =
        std::cell::RefCell::new(QueryCache::new());
}

/// 挂载 `.gram` 的上下文打分器。
pub struct OctagramGrammar {
    db: GramDb,
    config: OctagramConfig,
}

impl OctagramGrammar {
    pub fn open(path: &Path, config: OctagramConfig) -> anyhow::Result<Self> {
        Ok(Self {
            db: GramDb::open(path)?,
            config,
        })
    }

    pub fn unit_count(&self) -> usize {
        self.db.unit_count()
    }

    /// 把 context 的尾部 n 字编码并逐级 traverse，结果写进缓存槽并返回槽下标。
    ///
    /// 这一半只依赖 `context`，与 word 无关——分出来正是为了跨 node 复用。
    /// 逐级缩短、每级各 traverse 一次的语义原样保留（对齐 octagram.cc:125-148）；
    /// **traverse 失败的级直接不入表**，与改造前「失败则跳过该级」等价。
    fn fill_ctx_levels(&self, cache: &mut QueryCache, context: &str, n: usize) -> usize {
        if let Some(i) = cache.ctx_keys.iter().position(|k| k == context) {
            return i;
        }

        // context 取**尾部** n 字：用 char_indices().rev() 定位起点，避免收集 Vec<char>。
        let ctx_start = context
            .char_indices()
            .rev()
            .take(n)
            .last()
            .map_or(0, |(i, _)| i);
        let ctx_tail = &context[ctx_start..];

        let mut buf = EncodeBuf::default();
        let mut ctx_len = encode_chars(ctx_tail.chars(), n, &mut buf);
        let mut levels = Vec::with_capacity(n);
        let key_all = buf.as_slice();
        let mut ctx_ptr = 0usize;
        while ctx_len > 0 {
            if let Some(node) = self.db.traverse(&key_all[ctx_ptr..], 0) {
                levels.push(CtxLevel {
                    node,
                    ctx_len,
                    ctx_ptr,
                });
            }
            ctx_ptr += gramdb::encoded_char_len(key_all[ctx_ptr]);
            ctx_len -= 1;
        }

        // 环形替换：内层一轮的 context 集合是稳定的，先进先出即可维持整轮命中。
        if cache.ctx_keys.len() < CTX_SLOTS {
            cache.ctx_keys.push(context.to_string());
            cache.ctx_levels.push(levels);
            cache.ctx_keys.len() - 1
        } else {
            let i = cache.ctx_next;
            cache.ctx_keys[i].clear();
            cache.ctx_keys[i].push_str(context);
            cache.ctx_levels[i] = levels;
            cache.ctx_next = (cache.ctx_next + 1) % CTX_SLOTS;
            i
        }
    }

    /// 把 word 的首部 n 字编码进缓存的单格。这一半只依赖 `word`，与 context 无关。
    fn fill_word_key(&self, cache: &mut QueryCache, word: &str, n: usize) {
        if cache.word_valid && cache.word_key == word {
            return;
        }
        let word_end = word.char_indices().nth(n).map_or(word.len(), |(i, _)| i);
        let word_head = &word[..word_end];
        cache.word_full = word_end == word.len();
        let len = encode_chars(word_head.chars(), n, &mut cache.word_buf);
        cache.word_key.clear();
        cache.word_key.push_str(word);
        cache.word_valid = len > 0;
    }

    /// 查出「`context` 之后接 `word`」的最佳搭配分（自然对数域的 `ln(频次)`）。
    /// 没有任何命中时返回 0.0。
    ///
    /// 打分逻辑与改造前逐条对应，差别只在 context/word 两侧的编码与 trie 定位
    /// 改为走 [`QueryCache`]（纯函数结果的复用，不影响分数）。
    fn best_collocation(&self, context: &str, word: &str, is_rear: bool) -> f64 {
        let n = self
            .config
            .collocation_max_length
            .saturating_sub(1)
            .min(MAX_ENCODED_UNICODE);
        if n == 0 || context.is_empty() {
            return 0.0;
        }

        QUERY_CACHE.with(|c| {
            let mut cache = c.borrow_mut();
            cache.bind(std::ptr::from_ref(&self.db) as usize);

            self.fill_word_key(&mut cache, word, n);
            if !cache.word_valid {
                return 0.0;
            }
            let slot = self.fill_ctx_levels(&mut cache, context, n);
            if cache.ctx_levels[slot].is_empty() {
                // 一级都没走通 ⇒ 与改造前「while 循环全程 traverse 失败」同义。
                // 但 is_rear 那一查只依赖 word，仍须照做。
                //
                // ★ 必须再 `max(0.0)`：句末分是 `ln(频次) − rear_penalty`，**可能为负**，
                // 而改造前它是在初值为 0 的 `best` 上做 max 的，负值取不走。
                // 这里直接 return 会把负分泄出去——两处都得保住那个 0 下界。
                return 0.0f64.max(self.rear_only(&cache, is_rear));
            }

            let mut best = 0.0f64;
            let mut results = [(0i32, 0usize); MAX_RESULTS];
            let word_key = cache.word_buf.as_slice();

            for lvl in &cache.ctx_levels[slot] {
                let found = self
                    .db
                    .common_prefix_search(word_key, lvl.node, &mut results);
                for &(val, byte_len) in results.iter().take(found) {
                    let match_chars = GramDb::encoded_unicode_len(word_key, byte_len);
                    let collocation_len = lvl.ctx_len + match_chars;
                    // 整串匹配（context 用满 + word 全中）豁免 weak 罚，
                    // 对齐 `matches_whole_query`（octagram.cc:99-105）。
                    let whole = lvl.ctx_ptr == 0 && byte_len == word_key.len();
                    let penalty = if collocation_len >= self.config.collocation_min_length || whole
                    {
                        0.0
                    } else {
                        -self.config.weak_extra_penalty
                    };
                    best = best.max(val as f64 / VALUE_SCALE + penalty);
                }
            }

            best.max(self.rear_only(&cache, is_rear))
        })
    }

    /// 句末追加查一次 `word + "$"`（`$` 是 octagram 的句末标记）。
    /// 仅在 word 未被截断时进行，对齐 octagram.cc:149-158。
    ///
    /// 只依赖 word，故与 context 各级的结果取 max 即可——与改造前
    /// 「在同一个 `best` 上继续 max」等价。
    fn rear_only(&self, cache: &QueryCache, is_rear: bool) -> f64 {
        if !is_rear || !cache.word_full {
            return 0.0;
        }
        let word_key = cache.word_buf.as_slice();
        let Some(node) = self.db.traverse(word_key, 0) else {
            return 0.0;
        };
        let mut results = [(0i32, 0usize); MAX_RESULTS];
        let found = self.db.common_prefix_search(b"$", node, &mut results);
        if found > 0 {
            results[0].0 as f64 / VALUE_SCALE - self.config.rear_extra_penalty
        } else {
            0.0
        }
    }
}

impl Grammar for OctagramGrammar {
    fn query(&self, context: &str, word: &str, is_rear: bool) -> f64 {
        if self.config.weight == 0.0 {
            return 0.0;
        }
        let hit = self.best_collocation(context, word, is_rear);
        // 未命中时 `hit == 0`，于是本式给出 `−weight × baseline`——**未命中必须是负的**，
        // 理由见类型注释「两条约束」。分寸完全由 `weight` 控制。
        self.config.weight * (hit - self.config.baseline)
    }

    fn weight(&self) -> Option<f64> {
        Some(self.config.weight)
    }
}
