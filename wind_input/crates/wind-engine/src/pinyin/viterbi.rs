//! Viterbi 解码
//!
//! 与 Go 版本 `wind_input/internal/engine/pinyin/viterbi.go` 对齐。
//! 使用动态规划找到最优词序列。

use crate::pinyin::grammar::Grammar;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{Level, debug, enabled, info};

/// 开启语法模型后，每多少次解码汇总一条 INFO 性能日志。
///
/// 取 50：一次按键即一次解码，打几句话就能看到一条，既够快感知、又不刷屏。
/// 关闭语法模型时**一条都不出**——没开这功能的用户不该在日志里看见它。
const PERF_LOG_EVERY: u64 = 50;

/// Viterbi 解码结果
#[derive(Debug, Clone)]
pub struct ViterbiResult {
    pub words: Vec<String>,
    pub log_prob: f64,
    /// 最优路径**实际采用**的音节边界（全输入空间的起始位 bitmask）。
    ///
    /// 多路径切分下同一串输入可有多种切法，整句是按其中哪一条拼出来的，只有解码器
    /// 知道。此前整句候选一律标 `maximum_match` 的切分——单路径时那恰好就是真相，
    /// 多路径时便成了谎报（`xianjiaotongdaxue` 实走 `xi|an|…` 却标成 `xian|…`）。
    /// 该字段供整句候选回填 `Candidate::boundary`，双拼校验与用户造词都依赖它。
    ///
    /// 0 = 无可用信息（解码失败 / 输入超 64 字节，超出 bitmask 表达范围）。
    pub boundary: u64,
}

/// 词节点（用于构建 lattice）
#[derive(Debug, Clone)]
pub struct WordNode {
    pub start: usize,
    pub end: usize,
    pub word: String,
    /// 本节点所采用切分的音节起始位 bitmask，相对 `start`（见 `LatticeNode::syl_mask`）
    pub syl_mask: u64,
    pub log_prob: f64,
}

/// 单状态 DP 的状态（**无 grammar 时**用）。
///
/// 与 [`BeamEntry`] 的差别只有一个字段：这里**没有 `prev_word`**——单状态下每个位置
/// 就一条线，回溯只需 `prev_pos`，多带一个 `String` 就是每条胜出边多一次分配与拷贝。
/// 保持它与「接语言模型之前」逐字节同构，是**功能关闭时零开销**的前提。
#[derive(Clone)]
struct DpEntry {
    log_prob: f64,
    prev_pos: usize,
    /// 前驱在 `dp[prev_pos]` 里的下标。
    ///
    /// **K=1 时恒 0，多解时才有意义**：`dp[i]` 保留 top-K 条路径后，光靠 `prev_pos`
    /// 已经定位不到「是哪一条」—— 同一个起点位置上并存着 K 条分数不同的前驱。
    /// （beam 那侧用 `prev_word` 解同一个问题，因为它按末词分组；DP 这侧按分数排名分组，
    /// 故用下标。）
    prev_idx: usize,
    word: String,
    syl_mask: u64,
}

/// 往**降序**的 top-K 列表里插入一条，超出 `k` 的丢弃。
///
/// ⚠️ 插入位置取「第一个**严格小于**它的位置」，即分数相等时新来的排在既有之后 ——
/// 这与单路径 DP 那句「严格大于才覆盖 ⇒ 相等分数保留先到的」是同一条 tie-break 规则。
/// K=1 时本函数与那句等价，这正是「K=1 逐位不变」的依据。
fn push_topk(list: &mut Vec<DpEntry>, entry: DpEntry, k: usize) {
    if k == 0 {
        return;
    }
    let pos = list
        .iter()
        .position(|x| x.log_prob < entry.log_prob)
        .unwrap_or(list.len());
    if pos >= k {
        return;
    }
    list.insert(pos, entry);
    list.truncate(k);
}

/// beam 的一条线（**有 grammar 时**用）。
#[derive(Clone)]
struct BeamEntry {
    log_prob: f64,
    prev_pos: usize,
    /// 前驱线的末词。
    ///
    /// 单状态 DP 时回溯键只需 `prev_pos`；beam 下同一位置并存多条按末词区分的线，
    /// 键必须是 `(prev_pos, prev_word)` 才能唯一定位前驱。
    /// librime 用裸指针 `const Line* predecessor`（`poet.cc:24`）绕开了这件事——
    /// 它的 Line 存活在 map 里、地址稳定；我们用键查找，正确性依赖
    /// [`ViterbiDecoder::decode_beam`] 里论证的「`dp[start]` 定稿后不再变」。
    prev_word: String,
    word: String,
    syl_mask: u64,
}

/// 每个位置最多保留几条线。对齐 librime `BeamSearch::kMaxLineCandidates`
/// （`poet.cc:136`）。
///
/// 状态键是「最后一个词」而非整条前缀，再叠加本上限，
/// **活跃状态数恒 ≤ 7，与词表大小无关**——这正是 bigram 解码不会状态爆炸的原因。
const BEAM_WIDTH: usize = 7;

/// Viterbi 解码器
///
/// 节点权重（含单字惩罚/虚词加成/实体词加成）在 lattice 构建阶段由 `score_node`
/// 计算并写入 `WordNode.log_prob`，解码器只做最优路径 DP。
///
/// 可选挂载一个 [`Grammar`] 给**转移**打上下文分（见 `grammar.rs` 与
/// `docs/design/language-model-integration.md`）。`None` 时行为与挂载前逐位一致，
/// 且**不构造 context 串**——那是每条边一次的字符串分配，无模型时纯属浪费。
/// 对齐 librime 用模板在编译期分派两种策略（`poet.cc` 的 `DynamicProgramming`
/// vs `BeamSearch`）的意图，这里用一次 `match` 在运行时达到同样效果。
pub struct ViterbiDecoder {
    grammar: Option<Arc<dyn Grammar>>,
    /// 性能汇总累加器。**仅语法模型开启时写入**——关闭时一次原子操作都不做。
    perf: PerfStats,
}

/// 解码性能的滚动累加，每 [`PERF_LOG_EVERY`] 次汇总一条 INFO 后清零。
///
/// 用原子量而非 `Mutex`：`decode` 收 `&self`（引擎跨线程共享），而这里只是计数，
/// 各字段之间不需要互相一致——汇总日志差一两次采样无所谓，不值得为它上锁。
#[derive(Default)]
struct PerfStats {
    count: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
    queries: AtomicU64,
}

impl Default for ViterbiDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ViterbiDecoder {
    pub fn new() -> Self {
        Self {
            grammar: None,
            perf: PerfStats::default(),
        }
    }

    /// 挂载上下文模型。
    pub fn with_grammar(grammar: Arc<dyn Grammar>) -> Self {
        Self {
            grammar: Some(grammar),
            perf: PerfStats::default(),
        }
    }

    /// 构造某条线的上下文串：**回看两个词**。
    ///
    /// 语义逐条对齐 librime `Line::context()`（`poet.cc:52-58`）：句首为空串，
    /// 只有一个词时就是那个词，否则是「前一个词 + 当前词」的拼接。
    /// 再往前的词由具体 `Grammar` 实现决定要不要用（octagram 只取尾部 ≤3 个字，
    /// 而 bgc 实测更是只用到 1 个字）。
    ///
    /// 线自带 `prev_word`，所以这里拿到的是**这条线自己的**末两词。
    /// （P1 时按位置回溯 `dp[pos]`，只能拿到「当前最优路径的末两词」，是个近似；
    /// 改 beam 后每条线各自携带前驱，context 才真正准确。）
    fn context_of(entry: &BeamEntry) -> String {
        if entry.word.is_empty() {
            return String::new();
        }
        if entry.prev_word.is_empty() {
            return entry.word.clone();
        }
        let mut s = String::with_capacity(entry.prev_word.len() + entry.word.len());
        s.push_str(&entry.prev_word);
        s.push_str(&entry.word);
        s
    }

    /// 把一条新线并入某位置的候选集，维持「按末词去重 + 分数降序 + 至多 `width` 条」。
    ///
    /// ★ **tie-break 必须与单状态 DP 时一致**，否则无模型时结果就会漂移：
    /// 改造前是单槽 `if total_prob > dp[end].log_prob`，即**相等分数保留先到的**。
    /// 这里靠三处复刻它——同末词时要求**严格大于**才替换；候选集已满时同样要求
    /// 严格优于最差的一条才挤进去；不同末词时依赖 `sort_by` 的**稳定性**，
    /// 让相等分数保持插入顺序，于是 top-1 恒是「最高分中最早到达的那条」。
    ///
    /// ★ **参数是 `&WordNode` 而非造好的 `DpEntry`**：字符串 clone 被推迟到
    /// 「确定要留下」之后。改造前只在胜出时才 clone，若这里每条候选都先造 entry
    /// 再丢弃，长输入下会白白分配大量 String（实测是解码耗时翻倍的主因之一）。
    fn push_state(
        states: &mut Vec<BeamEntry>,
        log_prob: f64,
        prev_pos: usize,
        prev_word: &str,
        node: &WordNode,
    ) {
        if let Some(slot) = states.iter_mut().find(|e| e.word == node.word) {
            if log_prob <= slot.log_prob {
                return;
            }
            slot.log_prob = log_prob;
            slot.prev_pos = prev_pos;
            slot.prev_word.clear();
            slot.prev_word.push_str(prev_word);
            slot.syl_mask = node.syl_mask;
        } else {
            // 已满且不严格优于最差的一条：直接丢弃，不付 clone 的代价。
            if states.len() >= BEAM_WIDTH
                && states
                    .last()
                    .is_some_and(|worst| log_prob <= worst.log_prob)
            {
                return;
            }
            states.push(BeamEntry {
                log_prob,
                prev_pos,
                prev_word: prev_word.to_string(),
                word: node.word.clone(),
                syl_mask: node.syl_mask,
            });
        }
        states.sort_by(|a, b| {
            b.log_prob
                .partial_cmp(&a.log_prob)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        states.truncate(BEAM_WIDTH);
    }

    /// Viterbi 解码：找到最优词序列
    ///
    /// 输入：
    /// - `nodes`: 按 endPos 索引的词节点列表（nodes[endPos] = 所有在 endPos 结束的词）
    /// - `input_len`: 输入字符串长度
    ///
    /// 输出：最优词序列
    /// ★ **按有无语言模型分派到两套实现**，对齐 librime `Poet::MakeSentence`
    /// （`poet.cc:245-252`：无 grammar 走 `DynamicProgramming`、有才走 `BeamSearch`）。
    ///
    /// 这不是图省事，而是为了让**功能关闭时真正零开销**：beam 那套要给每条线携带
    /// `prev_word`、给每个位置持有一个 `Vec`，无模型时这些全是白付的代价。
    ///
    /// 两份实现的一致性由单测 `null_grammar_does_not_change_result` 守着：
    /// 它对比「不挂 grammar」与「挂恒 0 的 `NullGrammar`」，那恰好就是这两条路径。
    pub fn decode(&self, nodes: &[Vec<WordNode>], input_len: usize) -> ViterbiResult {
        if input_len == 0 {
            return ViterbiResult {
                words: Vec::new(),
                log_prob: 0.0,
                boundary: 0,
            };
        }

        // ★ 计时本身也要守「关闭时零开销」：`Instant::now` 有几十纳秒成本，
        // 关闭语法模型且未开 DEBUG 时一次都不取——那条路径的存在意义就是
        // 「没开这功能的用户一分钱都不用付」，不能被诊断代码破坏。
        // 开启时无条件计时：都已经付了搭配查询的钱，这点开销不值一提，
        // 而 INFO 汇总需要它。
        let on = self.grammar.is_some();
        let started = (on || enabled!(Level::DEBUG)).then(std::time::Instant::now);

        let mut queries = 0u64;
        let result = match self.grammar.as_deref() {
            None => Self::decode_dp(nodes, input_len),
            Some(g) => Self::decode_beam(nodes, input_len, g, &mut queries),
        };

        if let Some(t0) = started {
            let us = t0.elapsed().as_micros() as u64;
            self.log_perf(on, input_len, nodes, &result, queries, us);
        }
        result
    }

    /// 同 [`Self::decode`]，但返回至多 `k` 条解（**按分数降序，未按文本去重**）。
    ///
    /// `k <= 1` 时等价于 `decode`，且与单路径 DP **逐位相同** —— 出厂配置走的就是这条，
    /// 这是 N-best 改造的零回归依据（见 `k_of_one_matches_single_path_dp`）。
    ///
    /// ⚠️ **未去重**：不同切分可能拼出同一串字，调用方要连同「凑够 N 条」一起做，
    /// 因为去重后条数会少于 `k`。
    ///
    /// ⚠️ **有语法模型时只返回一条**。beam 那侧的 `dp[i]` 虽然也并存多条，但它是按
    /// **末词**分组保留的（`BEAM_WIDTH` 条不同末词），前 k 条之间往往只差最后一个词、
    /// 而不是「另一种解读」——拿来当整句备选是误导。要给它做真正的 N-best 得先想清楚
    /// 「按末词分组」与「按整句排名」怎么对齐，不在本次范围内。语法模型出厂关闭且不随
    /// 安装包分发模型，故这条路暂时影响不到任何人。
    pub fn decode_nbest(
        &self,
        nodes: &[Vec<WordNode>],
        input_len: usize,
        k: usize,
    ) -> Vec<ViterbiResult> {
        if input_len == 0 {
            return Vec::new();
        }
        if k <= 1 || self.grammar.is_some() {
            let one = self.decode(nodes, input_len);
            return if one.words.is_empty() {
                Vec::new()
            } else {
                vec![one]
            };
        }
        Self::decode_dp_nbest(nodes, input_len, k)
    }

    /// 记一次解码的性能。**只记数量与耗时，绝不记输入串或候选文本**
    /// （日志隐私：INFO 及以下不得出现用户输入内容）。
    ///
    /// 两级分工：
    /// - `DEBUG` 每次一条明细，用于定位「哪一次解码慢」；
    /// - `INFO` 每 [`PERF_LOG_EVERY`] 次汇总一条，**仅语法模型开启时**，
    ///   让人不必开 DEBUG 就能感知开启它的代价。
    fn log_perf(
        &self,
        on: bool,
        input_len: usize,
        nodes: &[Vec<WordNode>],
        result: &ViterbiResult,
        queries: u64,
        us: u64,
    ) {
        if enabled!(Level::DEBUG) {
            debug!(
                grammar = on,
                input_len,
                nodes = nodes.iter().map(Vec::len).sum::<usize>(),
                queries,
                words = result.words.len(),
                us,
                "viterbi 解码"
            );
        }
        if !on {
            return;
        }
        self.perf.total_us.fetch_add(us, Ordering::Relaxed);
        self.perf.queries.fetch_add(queries, Ordering::Relaxed);
        self.perf.max_us.fetch_max(us, Ordering::Relaxed);
        // fetch_add 返回旧值，故 `+1` 后才是本次序号。
        let n = self.perf.count.fetch_add(1, Ordering::Relaxed) + 1;
        if !n.is_multiple_of(PERF_LOG_EVERY) {
            return;
        }
        // 取完即清零，让每条汇总覆盖独立的一段窗口（累计均值会把变化抹平）。
        let total = self.perf.total_us.swap(0, Ordering::Relaxed);
        let qs = self.perf.queries.swap(0, Ordering::Relaxed);
        let max = self.perf.max_us.swap(0, Ordering::Relaxed);
        self.perf.count.store(0, Ordering::Relaxed);
        info!(
            decodes = PERF_LOG_EVERY,
            avg_us = total / PERF_LOG_EVERY,
            max_us = max,
            avg_queries = qs / PERF_LOG_EVERY,
            "语法模型解码性能（近 {PERF_LOG_EVERY} 次）"
        );
    }

    /// 单状态 DP：每个位置只保留一条最优路径。
    ///
    /// **与接语言模型之前逐字节同构**——`dp` 是一次性分配的连续数组、状态不含
    /// `prev_word`、胜出才 `clone`。改这里之前先想清楚：它的存在意义就是
    /// 「没开这个功能的用户一分钱都不用付」。
    fn decode_dp(nodes: &[Vec<WordNode>], input_len: usize) -> ViterbiResult {
        Self::decode_dp_nbest(nodes, input_len, 1)
            .into_iter()
            .next()
            .unwrap_or(ViterbiResult {
                words: Vec::new(),
                log_prob: f64::NEG_INFINITY,
                boundary: 0,
            })
    }

    /// K-best DP：每个位置保留 top-K 条路径，回溯出至多 `k` 条整句解。
    ///
    /// **`k == 1` 时与单路径 DP 逐位相同**（tie-break 见 [`push_topk`]），这是整个
    /// N-best 改造的零回归依据：出厂 `sentence_max_count = 1` 走的就是这条。
    ///
    /// 返回按分数降序，**未按文本去重** —— 不同切分可能拼出同一串字
    /// （`xi'an` 的 xi|an 与 xian 都可能落到同一个词上），去重要在调用方连同
    /// 「凑够 N 条」一起做，因为去重后条数会少于 k。
    fn decode_dp_nbest(nodes: &[Vec<WordNode>], input_len: usize, k: usize) -> Vec<ViterbiResult> {
        let k = k.max(1);
        // dp[i] = 到达位置 i 的 top-K 路径（降序）。空 = 该位置不可达
        // （单路径版本用 log_prob == NEG_INFINITY 表达同一件事）。
        let mut dp: Vec<Vec<DpEntry>> = vec![Vec::new(); input_len + 1];
        dp[0].push(DpEntry {
            log_prob: 0.0,
            prev_pos: 0,
            prev_idx: 0,
            word: String::new(),
            syl_mask: 0,
        });

        // nodes[end_pos] = 所有在字节位置 end_pos 结束的词（与 LatticeBuilder::build 的
        // 存储约定一致：node 存入 nodes[char_end]）。此前误读 nodes[end_pos-1] 导致
        // 整段 Viterbi 长句解码恒为空（差一 bug）。
        for end_pos in 1..=input_len {
            if end_pos >= nodes.len() {
                continue;
            }
            for node in &nodes[end_pos] {
                let start_pos = node.start;
                // 前驱的**每一条**都要试：次优整句往往正是「某一段换了个词」，
                // 只接前驱的最优线就退化回单路径了。
                for idx in 0..dp[start_pos].len() {
                    let prev = &dp[start_pos][idx];
                    if prev.log_prob == f64::NEG_INFINITY {
                        continue;
                    }
                    let entry = DpEntry {
                        log_prob: prev.log_prob + node.log_prob,
                        prev_pos: start_pos,
                        prev_idx: idx,
                        word: node.word.clone(),
                        syl_mask: node.syl_mask,
                    };
                    push_topk(&mut dp[end_pos], entry, k);
                }
            }
        }

        // 从最远可达位置回溯（同单路径版本：整串走不通时退而求其次）。
        let mut end = input_len;
        while end > 0 && dp[end].is_empty() {
            end -= 1;
        }
        if end == 0 {
            return Vec::new();
        }

        let expressible = input_len <= 64;
        // ★ **整串走不通时，分数一律 NEG_INFINITY**，哪怕 words 有内容。
        //
        // 这不是笔误，是单路径版本一直以来的行为（它取 `dp[input_len].log_prob` 而非回溯
        // 起点 `dp[pos]` 的分），而且那个「看起来像 bug」的取值**承担着压制部分解的功能**：
        // 回溯起点退到 `end < input_len` 意味着这条解没有解释完整串输入，把它的真实分数
        // 放出去，它就会以正常权重去和完整解竞争。实测 `bzdhaobuhao` 一类输入下
        // 「不知道哈」（多吞一个字的部分解）会就此抢走首选，压过词频高 672 倍的「不知道」
        // —— `competing_cuts_coexist_and_are_ranked_by_frequency` 当场抓到。
        //
        // 改这里之前先想清楚谁来接替这道压制，不要只看「words 非空却报 -inf 很奇怪」。
        let full_reach = end == input_len;
        (0..dp[end].len())
            .map(|rank| {
                // 回溯第 rank 条：沿 (prev_pos, prev_idx) 往回走。
                let mut words = Vec::new();
                let mut boundary = 0u64;
                let (mut pos, mut idx) = (end, rank);
                while pos > 0 {
                    let entry = &dp[pos][idx];
                    if entry.word.is_empty() {
                        break;
                    }
                    words.push(entry.word.clone());
                    if expressible {
                        boundary |= entry.syl_mask << entry.prev_pos;
                    }
                    (pos, idx) = (entry.prev_pos, entry.prev_idx);
                }
                words.reverse();
                ViterbiResult {
                    words,
                    log_prob: if full_reach {
                        dp[end][rank].log_prob
                    } else {
                        f64::NEG_INFINITY
                    },
                    boundary: if expressible { boundary } else { 0 },
                }
            })
            .collect()
    }

    /// beam search：每个位置按末词保留至多 [`BEAM_WIDTH`] 条线，转移时叠加上下文分。
    fn decode_beam(
        nodes: &[Vec<WordNode>],
        input_len: usize,
        grammar: &dyn Grammar,
        queries: &mut u64,
    ) -> ViterbiResult {
        // dp[i] = 到达位置 i 的候选线集合（按末词区分，至多 BEAM_WIDTH 条，分数降序）。
        // 空集合 = 该位置不可达（单状态那侧用 log_prob == NEG_INFINITY 表达同一件事）。
        let mut dp: Vec<Vec<BeamEntry>> = vec![Vec::new(); input_len + 1];
        // 起点是一条「空线」，对齐 librime `BeamSearch::Initiate` 的
        // `initial_state.emplace("", Line::kEmpty)`（`poet.cc:138-140`）。
        dp[0].push(BeamEntry {
            log_prob: 0.0,
            prev_pos: 0,
            prev_word: String::new(),
            word: String::new(),
            syl_mask: 0,
        });

        // 前向 DP
        // nodes[end_pos] = 所有在字节位置 end_pos 结束的词（与 LatticeBuilder::build 的
        // 存储约定一致：node 存入 nodes[char_end]）。此前误读 nodes[end_pos-1] 导致
        // 整段 Viterbi 长句解码恒为空（差一 bug）。
        //
        // ★ 按 end_pos 升序遍历，是 beam 正确性的前提：扩展 end_pos 时所有
        // `start_pos < end_pos` 的 dp 都已**定稿**、之后只被读取。回溯按
        // `(prev_pos, prev_word)` 查前驱，靠的正是这条——被扩展出的线，其前驱
        // 必然还在那个位置的保留集里，不会被后来的插入挤掉。
        // （librime 按 start_pos 遍历词图，是因为它的 beam 要「从某状态向前推」；
        //  我们按 end 索引同样满足该前提，故循环骨架无需翻转。）
        for end_pos in 1..=input_len {
            if end_pos >= nodes.len() {
                continue;
            }
            let is_rear = end_pos == input_len;
            for node in &nodes[end_pos] {
                let start_pos = node.start;
                // 防御：零长/逆向节点会让 split_at_mut 的前提失效
                if start_pos >= end_pos {
                    continue;
                }
                // 源与目标是同一个 Vec 的两段，用 split_at_mut 分开借用。
                let (left, right) = dp.split_at_mut(end_pos);
                let src_states = &left[start_pos];
                if src_states.is_empty() {
                    continue;
                }
                let target = &mut right[0];

                // 从该位置保留的每条线各扩展一次（beam 宽度即为线数上限）。
                *queries += src_states.len() as u64;
                for src in src_states.iter() {
                    // 对齐 librime `Grammar::Evaluate` 的加法形态（`grammar.h:18-26`）。
                    let ctx_score = grammar.query(&Self::context_of(src), &node.word, is_rear);
                    let total_prob = src.log_prob + node.log_prob + ctx_score;
                    Self::push_state(target, total_prob, start_pos, &src.word, node);
                }
            }
        }

        // 整句分取终点的最优线；终点不可达时维持改造前的 NEG_INFINITY 语义
        // （注意：即便如此下面仍会从更早的可达位置回溯，words 可以非空——
        //  这是改造前就有的行为，原样保留）。
        let final_log_prob = dp[input_len]
            .first()
            .map_or(f64::NEG_INFINITY, |e| e.log_prob);

        // 回溯
        let mut words = Vec::new();
        let mut pos = input_len;

        // 从最远可达位置回溯
        while pos > 0 && dp[pos].is_empty() {
            pos -= 1;
        }

        // 回溯的同时把各节点的音节 mask 平移到全输入空间累加，得到整句的真实边界。
        // 输入超 64 字节时 bitmask 表达不下，一律给 0（= 无信息，下游降级放行）。
        //
        // 键从 `pos` 变成 `(pos, word)`：先取该位置的最优线（`first()`，集合按分数降序），
        // 其后每步都按前驱的末词在前一位置里定位。
        let mut boundary = 0u64;
        let expressible = input_len <= 64;
        let mut cursor: Option<&BeamEntry> = dp[pos].first();
        while pos > 0 {
            let Some(entry) = cursor else {
                break;
            };
            if entry.word.is_empty() {
                break;
            }
            words.push(entry.word.clone());
            if expressible {
                boundary |= entry.syl_mask << entry.prev_pos;
            }
            let prev_pos = entry.prev_pos;
            let prev_word = entry.prev_word.as_str();
            // 前驱必然还在（见前向 DP 处关于「定稿」的论证）；找不到只可能是
            // 该不变量被破坏，此时宁可截断也不要给出错乱的路径。
            cursor = dp[prev_pos].iter().find(|e| e.word == prev_word);
            pos = prev_pos;
        }

        words.reverse();

        ViterbiResult {
            words,
            log_prob: final_log_prob,
            boundary: if expressible { boundary } else { 0 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锁定 nodes[end_pos] 的索引约定（C1 差一回归）。
    /// 输入 "nihao"（5 字节），节点在 char_end=5 结束。若 decode 误读
    /// nodes[end_pos-1]，则永远取不到结束于 5 的节点，words 为空。
    #[test]
    fn test_decode_index_convention() {
        let input_len = 5usize; // "nihao"
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
        // 单个双字词 "你好" 覆盖 [0,5]，结束于位置 5
        nodes[5].push(WordNode {
            start: 0,
            end: 5,
            word: "你好".to_string(),
            syl_mask: 0b101, // ni|hao
            log_prob: 10.0,
        });
        let decoder = ViterbiDecoder::new();
        let result = decoder.decode(&nodes, input_len);
        assert_eq!(result.words, vec!["你好".to_string()], "应解码出 你好");
        assert!(result.log_prob.is_finite());
        assert_eq!(result.boundary, 0b101, "整句边界应回填节点自身的切分");
    }

    /// 两段路径：ni(0..2) + hao(2..5)，供多个用例复用。
    fn two_segment_lattice() -> (Vec<Vec<WordNode>>, usize) {
        let input_len = 5usize;
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
        nodes[2].push(WordNode {
            start: 0,
            end: 2,
            word: "你".to_string(),
            syl_mask: 0b1,
            log_prob: 3.0,
        });
        nodes[5].push(WordNode {
            start: 2,
            end: 5,
            word: "好".to_string(),
            syl_mask: 0b1,
            log_prob: 3.0,
        });
        (nodes, input_len)
    }

    /// 两段路径：验证多节点拼接。
    #[test]
    fn test_decode_two_segments() {
        let (nodes, input_len) = two_segment_lattice();
        let decoder = ViterbiDecoder::new();
        let result = decoder.decode(&nodes, input_len);
        assert_eq!(result.words, vec!["你".to_string(), "好".to_string()]);
        // 两个单音节节点分别起于 0 与 2 → {0,2}
        assert_eq!(result.boundary, 0b101);
    }

    fn node(start: usize, end: usize, word: &str, log_prob: f64) -> WordNode {
        WordNode {
            start,
            end,
            word: word.to_string(),
            syl_mask: 0b1,
            log_prob,
        }
    }

    /// ★★ **beam 的存在意义**：让「前缀较差、但搭配更好」的路径能够胜出。
    ///
    /// 单状态 DP 下 `dp[2]` 只留分高的「甲」，「乙」当场被丢弃，此后无论上下文分
    /// 怎么打都**不可能**再选出「乙丙」。这条测试守的就是那个差异——
    /// 它同时也证明了多状态回溯（按 `(prev_pos, prev_word)` 找前驱）是对的，
    /// 因为选出「乙丙」要求回溯准确落到非最优的那条前缀线上。
    #[test]
    fn beam_lets_a_weaker_prefix_win_on_context() {
        let input_len = 4usize;
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
        nodes[2].push(node(0, 2, "甲", 4.0));
        nodes[2].push(node(0, 2, "乙", 3.0)); // 前缀劣 1.0
        nodes[4].push(node(2, 4, "丙", 1.0));

        let plain = ViterbiDecoder::new().decode(&nodes, input_len);
        assert_eq!(plain.words, vec!["甲".to_string(), "丙".to_string()]);

        struct PreferYiBing;
        impl Grammar for PreferYiBing {
            fn query(&self, context: &str, word: &str, _is_rear: bool) -> f64 {
                if context == "乙" && word == "丙" {
                    5.0
                } else {
                    0.0
                }
            }
        }
        let ctx = ViterbiDecoder::with_grammar(Arc::new(PreferYiBing)).decode(&nodes, input_len);
        assert_eq!(
            ctx.words,
            vec!["乙".to_string(), "丙".to_string()],
            "beam 应保留次优前缀，让上下文分翻盘"
        );
        // 两个节点分别起于 0 与 2，回溯若走错线，boundary 也会跟着错。
        assert_eq!(ctx.boundary, 0b101);
    }

    /// 每个位置的保留线数不超过 [`BEAM_WIDTH`]。
    ///
    /// 用 grammar 的调用次数间接观测：位置 2 上放 `BEAM_WIDTH + 3` 个不同末词，
    /// 扩展 `2..4` 那条边时，只会从**保留下来的**那几条各调一次。
    #[test]
    fn beam_width_caps_expansion() {
        let input_len = 4usize;
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
        for i in 0..(BEAM_WIDTH + 3) {
            nodes[2].push(node(0, 2, &format!("w{i}"), i as f64));
        }
        nodes[4].push(node(2, 4, "尾", 0.0));

        let rec = Arc::new(Recorder {
            calls: std::sync::Mutex::new(Vec::new()),
        });
        let r = ViterbiDecoder::with_grammar(rec.clone()).decode(&nodes, input_len);

        let calls = rec.calls.lock().unwrap().clone();
        let tail_calls = calls.iter().filter(|c| c.1 == "尾").count();
        assert_eq!(tail_calls, BEAM_WIDTH, "只应从保留的 {BEAM_WIDTH} 条线扩展");
        // 截断按分数降序，最高分那条必须留下
        assert_eq!(
            r.words,
            vec![format!("w{}", BEAM_WIDTH + 2), "尾".to_string()]
        );
    }

    /// 同分时保留**先到的**一条，与改造前单槽 `total > dp[end]` 的 tie-break 一致。
    /// 这条一旦破掉，无模型时的整句结果就会相对基线漂移。
    #[test]
    fn equal_scores_keep_the_earliest() {
        let input_len = 2usize;
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
        nodes[2].push(node(0, 2, "先", 1.0));
        nodes[2].push(node(0, 2, "后", 1.0));
        let r = ViterbiDecoder::new().decode(&nodes, input_len);
        assert_eq!(r.words, vec!["先".to_string()]);
    }

    /// K=1 的 K-best 必须与单路径 DP **逐位相同** —— 这是整个 N-best 改造的零回归依据。
    ///
    /// 同分那条尤其要看住：`push_topk` 取「第一个严格小于」的位置插入，等价于旧版的
    /// 「严格大于才覆盖」。写成 `<=` 就会让后到的同分项顶掉先到的，整句结果相对基线漂移。
    #[test]
    fn k_of_one_matches_single_path_dp() {
        let input_len = 4usize;
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
        nodes[2].push(node(0, 2, "先", 1.0));
        nodes[2].push(node(0, 2, "后", 1.0)); // 同分，须保留「先」
        nodes[4].push(node(2, 4, "来", 1.0));
        nodes[4].push(node(0, 4, "先来", 1.5)); // 整词分更高，但两段合计 2.0 更优

        let single = ViterbiDecoder::decode_dp(&nodes, input_len);
        let nbest = ViterbiDecoder::decode_dp_nbest(&nodes, input_len, 1);
        assert_eq!(nbest.len(), 1);
        assert_eq!(nbest[0].words, single.words);
        assert_eq!(nbest[0].log_prob, single.log_prob);
        assert_eq!(nbest[0].boundary, single.boundary);
        assert_eq!(single.words, vec!["先".to_string(), "来".to_string()]);
    }

    /// K>1 时次优解要出得来，且**按分数降序**。
    ///
    /// 次优整句的典型形态正是「某一段换了个词」，所以转移时必须对前驱的**每一条**都试 ——
    /// 只接前驱最优线的话，`dp[end]` 里堆的全是同一条前缀的延伸，K 条里看不到真正的第二解。
    #[test]
    fn nbest_returns_ranked_alternatives() {
        let input_len = 4usize;
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
        // 前段两个互斥的词，分数不同
        nodes[2].push(node(0, 2, "盖", 2.0));
        nodes[2].push(node(0, 2, "概", 1.0));
        // 后段唯一
        nodes[4].push(node(2, 4, "伦", 1.0));

        let r = ViterbiDecoder::decode_dp_nbest(&nodes, input_len, 3);
        assert_eq!(r.len(), 2, "只有两条完整路径");
        assert_eq!(r[0].words, vec!["盖".to_string(), "伦".to_string()]);
        assert_eq!(r[1].words, vec!["概".to_string(), "伦".to_string()]);
        assert!(
            r[0].log_prob > r[1].log_prob,
            "必须按分数降序: {:?}",
            r.iter().map(|x| x.log_prob).collect::<Vec<_>>()
        );
    }

    /// 每条解的 `boundary` 要跟着**自己那条路径**走，不能都取最优解的。
    ///
    /// 下游拿它回填 `Candidate::boundary`，双拼校验与自动造词都依赖它；
    /// 串了就是「选了第 2 条整句，学进词库的却是第 1 条的切分」。
    #[test]
    fn each_alternative_carries_its_own_boundary() {
        let input_len = 4usize;
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
        // 路径甲：两段各 2 字节 ⇒ 位 0、2
        nodes[2].push(node(0, 2, "甲", 2.0));
        nodes[4].push(node(2, 4, "乙", 2.0));
        // 路径乙：整段一个词、单音节 ⇒ 只有位 0
        nodes[4].push(node(0, 4, "丙", 3.0));

        let r = ViterbiDecoder::decode_dp_nbest(&nodes, input_len, 3);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].words, vec!["甲".to_string(), "乙".to_string()]);
        assert_eq!(r[0].boundary, 0b101, "两段: 位 0 与位 2");
        assert_eq!(r[1].words, vec!["丙".to_string()]);
        assert_eq!(r[1].boundary, 0b1, "一段: 只有位 0");
    }

    /// **整串走不通的解，分数必须是 `-inf`**，哪怕 `words` 有内容。
    ///
    /// 这条看着反直觉，却是单路径版本一直以来的行为，而且承担着**压制部分解**的功能：
    /// 放出真实分数后，只解释了前半截的解会以正常权重去和完整解竞争。
    /// 真机现场是 `bzdhaobuhao` 下「不知道哈」抢走首选、压过词频高 672 倍的「不知道」。
    #[test]
    fn partial_reach_paths_report_neg_infinity() {
        let input_len = 6usize;
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
        // 只能走到位置 4，末尾两字节无人认领。
        nodes[2].push(node(0, 2, "甲", 2.0));
        nodes[4].push(node(2, 4, "乙", 2.0));

        let r = ViterbiDecoder::decode_dp_nbest(&nodes, input_len, 3);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].words, vec!["甲".to_string(), "乙".to_string()]);
        assert_eq!(
            r[0].log_prob,
            f64::NEG_INFINITY,
            "部分解不得带正常分数出去竞争"
        );
        // 单路径版本同样如此 —— 两侧必须一致。
        assert_eq!(
            ViterbiDecoder::decode_dp(&nodes, input_len).log_prob,
            f64::NEG_INFINITY
        );
    }

    /// 记录每次 `query` 的入参，用来验证 context / is_rear 是怎么构造的。
    struct Recorder {
        calls: std::sync::Mutex<Vec<(String, String, bool)>>,
    }

    impl Grammar for Recorder {
        fn query(&self, context: &str, word: &str, is_rear: bool) -> f64 {
            self.calls
                .lock()
                .unwrap()
                .push((context.to_string(), word.to_string(), is_rear));
            0.0
        }
    }

    /// **P1 的核心验收**：挂恒 0 的 `NullGrammar` 与完全不挂，结果必须**逐位相同**。
    ///
    /// 这条守的是 `grammar.rs` 里「为什么 `NullGrammar` 必须返回 0.0」那段论证：
    /// 若它返回 librime 那样的 `kPenalty`，整句分会平白多出 `词数 × 常数`，
    /// 词数不同的路径之间的比较随之改变——本测试会立刻变红。
    #[test]
    fn null_grammar_does_not_change_result() {
        let (nodes, input_len) = two_segment_lattice();
        let plain = ViterbiDecoder::new().decode(&nodes, input_len);
        let with_null = ViterbiDecoder::with_grammar(Arc::new(crate::pinyin::grammar::NullGrammar))
            .decode(&nodes, input_len);
        assert_eq!(plain.words, with_null.words);
        // 逐位相等而非近似：加 0.0 对任何有限值都不改变位模式。
        assert_eq!(plain.log_prob, with_null.log_prob);
        assert_eq!(plain.boundary, with_null.boundary);
    }

    /// context 按「回看两个词」构造，`is_rear` 只在落到整句末尾时为真。
    ///
    /// 没有这条，P1 就只证明了「没接模型时没坏」，没证明「接上去的通路是通的」——
    /// 两件事都得验。
    #[test]
    fn grammar_receives_context_and_rear_flag() {
        let (nodes, input_len) = two_segment_lattice();
        let rec = Arc::new(Recorder {
            calls: std::sync::Mutex::new(Vec::new()),
        });
        ViterbiDecoder::with_grammar(rec.clone()).decode(&nodes, input_len);

        let calls = rec.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "两条边各打一次分");
        // 按 end_pos 升序遍历：先「你」(end=2) 后「好」(end=5)。
        assert_eq!(
            calls[0],
            (String::new(), "你".to_string(), false),
            "句首无上文，且不在末尾"
        );
        assert_eq!(
            calls[1],
            ("你".to_string(), "好".to_string(), true),
            "上文是前一个词，且落在整句末尾"
        );
    }
}
