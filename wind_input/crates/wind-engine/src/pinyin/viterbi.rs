// Portions derived from librime / librime-octagram, Copyright (c) 2014 RIME Developers, BSD-3-Clause (see NOTICE.md).
//! Viterbi 解码：用动态规划在词图上找最优词序列。

use crate::pinyin::grammar::Grammar;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{Level, debug, enabled, info};

/// 开启语法模型后，每多少次解码汇总一条 INFO 性能日志。
///
/// 一次按键即一次解码，50 次约是打几句话，既够快感知又不刷屏。关闭语法模型时一条都不出。
const PERF_LOG_EVERY: u64 = 50;

/// Viterbi 解码结果
#[derive(Debug, Clone)]
pub struct ViterbiResult {
    pub words: Vec<String>,
    pub log_prob: f64,
    /// 同一条路径上只算节点自身的分（各节点 `log_prob` 之和），不含语法模型的转移分；
    /// 无语法模型时与 `log_prob` 相等。
    ///
    /// 给「这条路径像不像话」类的闸门用：那些阈值按词频标定，混入转移分会把正解闸掉。
    /// 模型只该重排，不该决定出不出。
    pub lexical_log_prob: f64,
    /// 最优路径实际采用的音节边界（全输入空间的起始位 bitmask）。
    ///
    /// 多路径切分下整句按哪种切法拼出只有解码器知道（`xianjiaotongdaxue` 实走 `xi|an|…`），
    /// 不能拿 `maximum_match` 的切分代替。供整句候选回填 `Candidate::boundary`，双拼校验与
    /// 用户造词都依赖它。
    ///
    /// 0 = 无可用信息（解码失败 / 输入超 64 字节，超出 bitmask 表达范围）。
    pub boundary: u64,
    /// 路径上至少一个节点带规范码（`WordNode::canon`）时，按路径顺序列出各节点；
    /// 全部精确命中时为 `None`，不做任何分配。供 [`Self::learn_code`] 拼造词码。
    pub pieces: Option<Vec<SentencePiece>>,
}

/// 整句路径上的一个节点，供拼造词码（见 [`ViterbiResult::learn_code`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct SentencePiece {
    /// 所打码上的字节跨度
    pub start: usize,
    pub end: usize,
    /// 节点在所打码坐标下的音节边界（相对 `start`）
    pub syl_mask: u64,
    /// 同 `WordNode::canon`
    pub canon: Option<(String, u64)>,
    /// 同 `WordNode::aux`
    pub aux: u8,
}

impl ViterbiResult {
    /// 整句的造词码：各节点规范码按路径顺序拼接，边界按累积的规范码长度平移后合并。
    /// 精确节点取所打码 `input[start..end]` 那一段与它自己的 `syl_mask`。
    ///
    /// 全精确（`pieces == None`）返回 `None`：造词码与整句候选的 `code` / `boundary` 相同，
    /// 语义同 `Candidate::meta.learn_code` 的 `None`。拼出的码超 64 字节、或任一段边界缺失时，
    /// 边界整体置 0（与协调层 `learn_phrase_on_commit` 拼多段时的口径一致：半截边界比没有更糟）。
    ///
    /// `input` 必须是建图所用的那串（step 2 的 `completed`、2b 的整串、2c 含残码的整串）。
    pub fn learn_code(&self, input: &str) -> Option<(String, u64)> {
        let pieces = self.pieces.as_ref()?;
        // 传错串（如 2c 该传含残码的整串却传了 `completed`）时，下面的 `input.get(..)?` 会
        // 静默返回 None、整句不带造词码——在测试里就让它响。
        debug_assert!(
            pieces.last().is_none_or(|p| p.end <= input.len()),
            "learn_code 的 input（{} 字节）短于路径终点 {:?}",
            input.len(),
            pieces.last().map(|p| p.end)
        );
        let mut code = String::new();
        let mut boundary = 0u64;
        let mut boundary_ok = true;
        for piece in pieces {
            let (c, b) = match &piece.canon {
                Some((c, b)) => (c.as_str(), *b),
                None => (input.get(piece.start..piece.end)?, piece.syl_mask),
            };
            if b == 0 || code.len() + c.len() > 64 {
                boundary_ok = false;
            } else {
                boundary |= b << code.len();
            }
            code.push_str(c);
        }
        Some((code, if boundary_ok { boundary } else { 0 }))
    }
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
    /// 造词用规范码，见 `LatticeNode::canon`。`None` = 所打码那段本身。
    pub canon: Option<(String, u64)>,
    /// 节点吸收的辅码字母总数（击键图连续辅助码；其余路径恒 0）。
    pub aux: u8,
}

/// 单状态 DP 的状态（无 grammar 时用）。
///
/// 与 [`BeamEntry`] 相比没有 `prev_word`：回溯靠 `prev_pos` + `prev_idx`，多带一个 `String`
/// 就是每条胜出边多一次分配。这是语法模型关闭时零开销的前提。
#[derive(Clone)]
struct DpEntry {
    log_prob: f64,
    prev_pos: usize,
    /// 前驱在 `dp[prev_pos]` 里的下标。K=1 时恒 0；多解时同一起点并存 K 条前驱，
    /// 光靠 `prev_pos` 定位不到是哪一条（beam 那侧按末词分组，故用 `prev_word`）。
    prev_idx: usize,
    word: String,
    syl_mask: u64,
    /// 见 `WordNode::canon`（精确节点为 `None`，克隆不分配）。
    canon: Option<(String, u64)>,
    aux: u8,
    /// 起点到此处已拼出文本的哈希（与切分无关），K=1 时恒 0。
    text_key: u64,
}

/// 在 `prev` 之后接上 `word` 的文本哈希：逐字节 FNV-1a，故「西安|穿袜子」与「西安穿袜子」同值。
fn extend_text_key(prev: u64, word: &str) -> u64 {
    word.bytes()
        .fold(prev, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

const TEXT_KEY_SEED: u64 = 0xcbf2_9ce4_8422_2325;

/// 往降序的 top-K 列表里插入一条，超出 `k` 的丢弃。
///
/// 插入位置取「第一个严格小于它的位置」，即分数相等时保留先到的。K=1 时这与单路径 DP
/// 「严格大于才覆盖」等价，是 K=1 逐位不变的依据。
///
/// 同一位置已拼文本相同的两条只留分高者：往后怎么接都是同一句，留两条只会占掉名额
/// （学过的整句与它的拆分版会把备选挤掉）。
fn push_topk(list: &mut Vec<DpEntry>, entry: DpEntry, k: usize) {
    if k == 0 {
        return;
    }
    if let Some(i) = list.iter().position(|x| x.text_key == entry.text_key) {
        if list[i].log_prob >= entry.log_prob {
            return;
        }
        list.remove(i);
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

/// beam 的一条线（有 grammar 时用）。
#[derive(Clone)]
struct BeamEntry {
    log_prob: f64,
    prev_pos: usize,
    /// 前驱线的末词。beam 下同一位置并存多条按末词区分的线，回溯键必须是
    /// `(prev_pos, prev_word)`；正确性依赖 [`ViterbiDecoder::decode_beam`] 里的
    /// 「`dp[start]` 定稿后不再变」（librime 用裸指针 `Line* predecessor` 绕开此事）。
    prev_word: String,
    word: String,
    syl_mask: u64,
    /// 见 `WordNode::canon`（精确节点为 `None`，克隆不分配）。
    canon: Option<(String, u64)>,
    aux: u8,
}

/// 每个位置最多保留几条线，对齐 librime `BeamSearch::kMaxLineCandidates`。
/// 状态键是末词而非整条前缀，活跃状态数恒 ≤ 7、与词表大小无关，故不会状态爆炸。
const BEAM_WIDTH: usize = 7;

/// Viterbi 解码器
///
/// 节点权重（含单字惩罚/虚词加成/实体词加成）在 lattice 构建阶段由 `score_node`
/// 计算并写入 `WordNode.log_prob`，解码器只做最优路径 DP。
///
/// 可选挂载一个 [`Grammar`] 给转移打上下文分（见 `grammar.rs` 与
/// `docs/design/language-model-integration.md`）。`None` 时走单状态 DP、不构造 context 串，
/// 对应 librime `DynamicProgramming` / `BeamSearch` 两种策略的分派。
pub struct ViterbiDecoder {
    grammar: Option<Arc<dyn Grammar>>,
    /// 性能汇总累加器。仅语法模型开启时写入，关闭时一次原子操作都不做。
    perf: PerfStats,
}

/// 解码性能的滚动累加，每 [`PERF_LOG_EVERY`] 次汇总一条 INFO 后清零。
///
/// 用原子量而非 `Mutex`：`decode` 收 `&self`，各字段不需要互相一致，汇总差一两次采样无所谓。
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

    /// 是否挂了上下文模型。
    pub fn has_grammar(&self) -> bool {
        self.grammar.is_some()
    }

    /// 挂着的上下文模型的尺度系数（[`Grammar::weight`]）；没挂 = `None`，挂了但不报系数 =
    /// `Some(None)`。
    pub fn grammar_weight(&self) -> Option<Option<f64>> {
        self.grammar.as_ref().map(|g| g.weight())
    }

    /// 构造某条线的上下文串：回看两个词，对齐 librime `Line::context()`。句首为空串，
    /// 只有一个词时就是那个词，否则是「前一个词 + 当前词」。取多少由 `Grammar` 实现决定
    /// （octagram 只取尾部 ≤3 个字）。
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
    /// tie-break 必须与单状态 DP 一致（相等分数保留先到的），三处复刻：同末词严格大于才替换；
    /// 已满时严格优于最差一条才挤进去；不同末词靠 `sort_by` 的稳定性保持插入顺序。
    ///
    /// 参数取 `&WordNode` 而非造好的 entry：字符串 clone 推迟到确定要留下之后，否则长输入下
    /// 白白分配大量 String。
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
            slot.canon.clone_from(&node.canon);
            slot.aux = node.aux;
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
                canon: node.canon.clone(),
                aux: node.aux,
            });
        }
        states.sort_by(|a, b| {
            b.log_prob
                .partial_cmp(&a.log_prob)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        states.truncate(BEAM_WIDTH);
    }

    /// Viterbi 解码：找到最优词序列。`nodes[end]` = 所有在字节位置 `end` 结束的词，
    /// `input_len` 为输入字节长度。
    ///
    /// 按有无语言模型分派到两套实现（对齐 librime `Poet::MakeSentence`），让功能关闭时零开销。
    /// 两份实现的一致性见 `null_grammar_does_not_change_result`。
    pub fn decode(&self, nodes: &[Vec<WordNode>], input_len: usize) -> ViterbiResult {
        if input_len == 0 {
            return ViterbiResult {
                words: Vec::new(),
                log_prob: 0.0,
                lexical_log_prob: 0.0,
                boundary: 0,
                pieces: None,
            };
        }

        // 计时也守「关闭时零开销」：关闭语法模型且未开 DEBUG 时不取 `Instant::now`。
        // 开启时无条件计时，INFO 汇总需要它。
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

    /// 同 [`Self::decode`]，但返回至多 `k` 条解（按分数降序、文本互不相同，见 [`push_topk`]）。
    ///
    /// `k <= 1` 时与单路径 DP 逐位相同，出厂配置走的就是这条（见 `k_of_one_matches_single_path_dp`）。
    ///
    /// 有语法模型时只返回一条：beam 的 `dp[i]` 按末词分组，前 k 条往往只差最后一个词，
    /// 不是另一种解读，拿来当整句备选是误导。
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

    /// 不看语法模型的 N-best（单状态 DP 的前 k 条，文本互不相同）。给「首选整句被屏蔽、要找替补」
    /// 用：[`Self::decode_nbest`] 挂了语法模型时只回 1 条，没有替补可取。无语法模型且 `k > 1` 时
    /// 与 `decode_nbest` 相同。
    pub fn decode_nbest_lexical(
        &self,
        nodes: &[Vec<WordNode>],
        input_len: usize,
        k: usize,
    ) -> Vec<ViterbiResult> {
        if input_len == 0 {
            return Vec::new();
        }
        Self::decode_dp_nbest(nodes, input_len, k)
    }

    /// 记一次解码的性能。只记数量与耗时，绝不记输入串或候选文本（日志隐私）。
    ///
    /// `DEBUG` 每次一条明细；`INFO` 每 [`PERF_LOG_EVERY`] 次汇总一条，仅语法模型开启时。
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
    /// 语法模型关闭时走这里，必须零额外开销：状态不含 `prev_word`、不构造 context 串。
    fn decode_dp(nodes: &[Vec<WordNode>], input_len: usize) -> ViterbiResult {
        Self::decode_dp_nbest(nodes, input_len, 1)
            .into_iter()
            .next()
            .unwrap_or(ViterbiResult {
                words: Vec::new(),
                log_prob: f64::NEG_INFINITY,
                lexical_log_prob: f64::NEG_INFINITY,
                boundary: 0,
                pieces: None,
            })
    }

    /// K-best DP：每个位置保留 top-K 条路径，回溯出至多 `k` 条整句解。
    ///
    /// `k == 1` 时与单路径 DP 逐位相同（tie-break 见 [`push_topk`]），出厂
    /// `sentence_max_count = 1` 走的就是这条。
    ///
    /// 返回按分数降序、文本互不相同的至多 `k` 条：不同切分拼出同一串字时
    /// （`xi'an` 的 xi|an 与 xian、学过的整句与它的拆分）只留分高的那条，见 [`push_topk`]。
    fn decode_dp_nbest(nodes: &[Vec<WordNode>], input_len: usize, k: usize) -> Vec<ViterbiResult> {
        let k = k.max(1);
        // dp[i] = 到达位置 i 的 top-K 路径（降序）。空 = 该位置不可达。
        let mut dp: Vec<Vec<DpEntry>> = vec![Vec::new(); input_len + 1];
        dp[0].push(DpEntry {
            log_prob: 0.0,
            prev_pos: 0,
            prev_idx: 0,
            word: String::new(),
            syl_mask: 0,
            canon: None,
            aux: 0,
            text_key: if k > 1 { TEXT_KEY_SEED } else { 0 },
        });

        // nodes[end_pos] = 所有在字节位置 end_pos 结束的词（与 LatticeBuilder::build 的
        // 存储约定一致），不是 nodes[end_pos-1]。
        for end_pos in 1..=input_len {
            if end_pos >= nodes.len() {
                continue;
            }
            for node in &nodes[end_pos] {
                let start_pos = node.start;
                // 前驱的每一条都要试：次优整句往往正是「某一段换了个词」，
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
                        canon: node.canon.clone(),
                        aux: node.aux,
                        text_key: if k > 1 {
                            extend_text_key(prev.text_key, &node.word)
                        } else {
                            0
                        },
                    };
                    push_topk(&mut dp[end_pos], entry, k);
                }
            }
        }

        // 从最远可达位置回溯（整串走不通时退而求其次）。
        let mut end = input_len;
        while end > 0 && dp[end].is_empty() {
            end -= 1;
        }
        if end == 0 {
            return Vec::new();
        }

        let expressible = input_len <= 64;
        // 整串走不通时分数一律 NEG_INFINITY，哪怕 words 有内容：这是压制部分解的机制，
        // 放出真实分数会让没解释完整串的解与完整解竞争、抢走首选（见
        // `competing_cuts_coexist_and_are_ranked_by_frequency`）。改之前先想好谁接替这道压制。
        let full_reach = end == input_len;
        (0..dp[end].len())
            .map(|rank| {
                // 回溯第 rank 条：沿 (prev_pos, prev_idx) 往回走。
                let mut words = Vec::new();
                let mut boundary = 0u64;
                let mut has_canon = false;
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
                    has_canon |= entry.canon.is_some();
                    (pos, idx) = (entry.prev_pos, entry.prev_idx);
                }
                words.reverse();
                // 只有路径上真有模糊节点才再走一遍收集片段：全精确时零分配。
                let pieces = has_canon.then(|| {
                    let mut out = Vec::with_capacity(words.len());
                    let (mut pos, mut idx) = (end, rank);
                    while pos > 0 {
                        let entry = &dp[pos][idx];
                        if entry.word.is_empty() {
                            break;
                        }
                        out.push(SentencePiece {
                            start: entry.prev_pos,
                            end: pos,
                            syl_mask: entry.syl_mask,
                            canon: entry.canon.clone(),
                            aux: entry.aux,
                        });
                        (pos, idx) = (entry.prev_pos, entry.prev_idx);
                    }
                    out.reverse();
                    out
                });
                let log_prob = if full_reach {
                    dp[end][rank].log_prob
                } else {
                    f64::NEG_INFINITY
                };
                ViterbiResult {
                    words,
                    log_prob,
                    // 无模型：路径分就只有节点分
                    lexical_log_prob: log_prob,
                    boundary: if expressible { boundary } else { 0 },
                    pieces,
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
        // 空集合 = 该位置不可达。
        let mut dp: Vec<Vec<BeamEntry>> = vec![Vec::new(); input_len + 1];
        // 起点是一条「空线」，对齐 librime `BeamSearch::Initiate`。
        dp[0].push(BeamEntry {
            log_prob: 0.0,
            prev_pos: 0,
            prev_word: String::new(),
            word: String::new(),
            syl_mask: 0,
            canon: None,
            aux: 0,
        });

        // 前向 DP（nodes 索引约定同 `decode_dp_nbest`）。
        //
        // 按 end_pos 升序遍历是 beam 正确性的前提：扩展 end_pos 时所有 `start_pos < end_pos`
        // 的 dp 都已定稿、之后只读。回溯按 `(prev_pos, prev_word)` 查前驱靠的正是这条：
        // 被扩展出的线，其前驱必然还在那个位置的保留集里。
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
                    // 对齐 librime `Grammar::Evaluate` 的加法形态。
                    let ctx_score = grammar.query(&Self::context_of(src), &node.word, is_rear);
                    let total_prob = src.log_prob + node.log_prob + ctx_score;
                    Self::push_state(target, total_prob, start_pos, &src.word, node);
                }
            }
        }

        // 整句分取终点的最优线；终点不可达时为 NEG_INFINITY，但仍从更早的可达位置回溯，
        // words 可以非空（同 DP 侧的部分解压制）。
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
        // 先取该位置的最优线，其后每步按前驱的末词在前一位置里定位。
        let mut boundary = 0u64;
        let expressible = input_len <= 64;
        let mut cursor: Option<&BeamEntry> = dp[pos].first();
        // beam 本就只回溯一条，片段随路收集（语法模型出厂关闭，这里不追零分配）；
        // 全精确时丢弃，结果与 DP 一侧同形。
        let mut pieces = Vec::new();
        let mut lexical = 0.0;
        while pos > 0 {
            let Some(entry) = cursor else {
                break;
            };
            if entry.word.is_empty() {
                break;
            }
            // 线上不存节点分，回到词图里按（跨度，词）取；同跨度同词有多个节点（模糊变体等）
            // 时前向 DP 选的必是分最高那个——转移分与节点无关，同一前驱下高分节点恒胜。
            lexical += nodes
                .get(pos)
                .into_iter()
                .flatten()
                .filter(|n| n.start == entry.prev_pos && n.word == entry.word)
                .map(|n| n.log_prob)
                .fold(f64::NEG_INFINITY, f64::max);
            words.push(entry.word.clone());
            if expressible {
                boundary |= entry.syl_mask << entry.prev_pos;
            }
            pieces.push(SentencePiece {
                start: entry.prev_pos,
                end: pos,
                syl_mask: entry.syl_mask,
                canon: entry.canon.clone(),
                aux: entry.aux,
            });
            let prev_pos = entry.prev_pos;
            let prev_word = entry.prev_word.as_str();
            // 前驱必然还在（见前向 DP 处关于「定稿」的论证）；找不到只可能是
            // 该不变量被破坏，此时宁可截断也不要给出错乱的路径。
            cursor = dp[prev_pos].iter().find(|e| e.word == prev_word);
            pos = prev_pos;
        }

        words.reverse();
        pieces.reverse();
        let pieces = pieces.iter().any(|p| p.canon.is_some()).then_some(pieces);

        ViterbiResult {
            words,
            log_prob: final_log_prob,
            lexical_log_prob: if final_log_prob.is_finite() {
                lexical
            } else {
                f64::NEG_INFINITY
            },
            boundary: if expressible { boundary } else { 0 },
            pieces,
        }
    }
}

#[cfg(test)]
#[path = "viterbi_tests.rs"]
mod tests;
