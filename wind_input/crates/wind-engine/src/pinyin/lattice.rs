//! 格子构建（Lattice）+ 多切分评分
//!
//! 与 Go 版本 `wind_input/internal/engine/pinyin/lattice.go` 对齐。
//! 构建词图并支持多路径评分，用于 Viterbi 解码。

use crate::pinyin::dag::{MaskCheck, SegGraph};
use crate::pinyin::fuzzy::{FuzzyConfig, FuzzyMatcher};
use wind_dict::cached::CachedDict;

/// 虚词集合（单字时轻微惩罚，对齐 Go functionWords）
fn is_function_word(w: &str) -> bool {
    matches!(
        w,
        "了" | "的"
            | "地"
            | "得"
            | "着"
            | "过"
            | "我"
            | "你"
            | "他"
            | "她"
            | "它"
            | "们"
            | "这"
            | "那"
            | "和"
            | "与"
            | "在"
            | "把"
            | "被"
            | "让"
            | "从"
            | "到"
            | "对"
            | "向"
            | "跟"
            | "不"
            | "没"
            | "也"
            | "都"
            | "就"
            | "才"
            | "还"
            | "又"
            | "再"
            | "很"
            | "太"
            | "最"
            | "是"
            | "有"
            | "会"
            | "能"
            | "要"
            | "可"
            | "去"
            | "来"
            | "做"
            | "说"
            | "看"
            | "想"
    )
}

/// V+助词尾字（多字词以此结尾时降权，对齐 Go particleSuffixes）
fn is_particle_suffix(c: char) -> bool {
    matches!(c, '了' | '的' | '着' | '过' | '得' | '地')
}

/// 每个词的固定惩罚，对应 librime `Grammar::Evaluate` 的 `kPenalty`
/// （`ref/weasel/librime/src/rime/gear/grammar.h:18-26`）。
///
/// 取值远小于 librime 的 18.42：两边**同为自然对数概率、量纲一致**（已核对），
/// 差异来自机制而非单位——librime 的 `kPenalty` 是**无语言模型时的兜底值**，有
/// grammar 时整个被替换掉；我们始终有 unigram，且 `score_node` 另有单字罚 −3.0、
/// 实词加成等项同时作用，等效的「多一个词」代价不止这一处。
pub(crate) const WORD_PENALTY: f64 = 3.0;

/// 歧义接缝上每个音节的惩罚，对应 librime `kPenaltyForAmbiguousSyllable`
/// （`ref/weasel/librime/src/rime/algo/syllabifier.cc:243-245`）。
///
/// librime 用 −23.03（1e-10）近乎硬禁，因其以隔音符号为消歧出口；我们不引入那套
/// 产品语义（§4.1 实测：Rime 中缩合音词在候选层仍是第 4/5 位），故取「恰好压过
/// 歧义拆分的收益」的量级即可。
///
/// **0.35 是一个刀刃值，改动前务必重跑 `pinyin_eval` 的定点**：≤0.30 时
/// `lianzhengtixing` 退回「李安整体性」（本次改造的原始缺陷）；≥0.5 时
/// `liandaoyan` 劣化为「连导演」。**聚合指标在 0.30~0.35 之间完全不变**，
/// 这两个定点是仅有的差异——因为它们是同一个词、同一个 `li|an` 拆分、同一个
/// 歧义接缝，切分层没有可区分二者的信息。真正的区分需要 bigram 上下文
/// （需要 bigram 上下文；旧 lm.rs 里的简化插值实现已随 unigram 合并删除，缺磁盘语料）。
pub(crate) const AMBIGUOUS_PENALTY: f64 = 0.35;

/// 词图节点的**每模糊音节**对数惩罚，`ln(2)` ⇒ 概率域每个模糊音节乘 0.5。
///
/// 与候选层的 [`super::FUZZY_WEIGHT_SCALE`]（=0.5，乘性）是**同一个量在两个域的表达**，
/// 两者必须同步改：候选层管词典直查的模糊命中，本常数管词图节点、进而管整句路径。
///
/// 取值对齐 librime `kFuzzySpellingPenalty`（`= log(0.5)`，加进 `credibility` 再进 weight）
/// 与 libime `fuzzyCost`（`= log10(0.5)`，`extraCost = fuzzies × fuzzyCost`）—— 两者都是
/// **每个模糊拼写 0.5、多个累乘**。
///
/// 此前这里写死 `-0.5`（≈ ×0.61）且不看音节数，`beijinsi`（2 个模糊音节）与单音节模糊
/// 同等对待。
pub(crate) const FUZZY_SYLLABLE_LOG_PENALTY: f64 = std::f64::consts::LN_2;

/// 简拼节点每个音节的惩罚（混合整句解码用，见 [`LatticeBuilder::add_abbrev_nodes`]）。
///
/// **按音节数计而非固定值**：简拼段越长，「每个字母只给了一个声母」积累的不确定性越大
/// ——`bzd` 要在 12 个同简拼词里选，`bzdh` 的候选面更宽。固定罚会让长简拼段不合理地便宜。
///
/// 量纲参照同文件的 `WORD_PENALTY`(3.0) 与模糊命中的 0.5：简拼的不确定性远大于模糊音
/// （一个声母对应几十个音节 vs z↔zh 两个变体），但又不能大到让混合整句根本出不来。
/// **本值由 `pinyin_eval` 的 D 类对账定出，改动前必须重跑**（见 `pinyin-mixed-abbrev.md` §4.8）。
pub(crate) const ABBREV_NODE_PENALTY: f64 = 1.2;

/// 单个简拼跨度最多取几个词进图。
///
/// 简拼召回面宽（`bzd` 真实词库下 12 个词），全塞进去会让节点数与 Viterbi 的边数一起膨胀，
/// 而排在后面的低频词几乎不可能赢下整句路径。按权重取前 N 即可。
const ABBREV_NODE_LIMIT: usize = 8;

/// 简拼跨度的最大字母数（= 最大音节数）。与 `AbbrevMatcher::find_candidates` 的上限一致。
const MAX_ABBREV_SPAN: usize = 6;

/// 尾部残码补全节点（[`LatticeBuilder::add_partial_final_nodes`]）的对数概率罚。
///
/// 对齐 librime 的 `kCompletionPenalty = log(0.5)`（`script_translator.cc`）：残码是
/// 「用户还没打完的音节」，把它当成某个具体音节是一次**预测**，须付出确定性代价。
/// 我们的 `log_prob` 同为自然对数（`ln(weight / DICT_TOTAL)`），故数值直接取 `ln 2`。
///
/// **为何不按残码长度递减**：直觉上 `zho` 比 `z` 确定（候选音节少），但候选面收窄这件事
/// 已经由 `search_prefix_*` 的召回集合自然表达了——`z` 捞出的字横跨 za/zai/zan/…，
/// 它们要各自与整句其余部分竞争；`zho` 只剩 zhong/zhou 两族。**再按长度加一道折扣等于
/// 把同一个信息扣两次**。同类先例见 `ABBREV_NODE_PENALTY` 的反向情形：那里按音节数递增
/// 是因为简拼段的召回集合**不随段长收窄**（每个字母恒是一个声母），信息未被表达。
pub(crate) const PARTIAL_FINAL_PENALTY: f64 = std::f64::consts::LN_2;

/// 尾部残码跨度最多取几个单字进图。
///
/// 残码召回面比简拼更宽（`k` 覆盖 ka/kai/kan/kang/ke/ken/keng/kong/kou/ku/kua/… 全部单字），
/// 但真正可能赢下整句路径的只有高频字，故比 `ABBREV_NODE_LIMIT`(8) 略放宽即可。
const PARTIAL_FINAL_NODE_LIMIT: usize = 12;

/// 词频归一化基准 —— **标定系数，非精确的词库总权重**，理由见 [`score_node_inner`]。
///
/// 取值 = 合并前 `unigram.txt` 的总频次（实测 242,154,693），使 99.95% 的词在这次合并前后
/// `log_prob` **数值完全不变**。参照：`cn_dicts` 实测总权重 243,154,024，差 0.41%
/// （差额全部来自 w=0 条目、纯 ASCII 词与多音合并，w=0 的权重和本就为 0）。
///
/// ⚠️ 它与 [`WORD_PENALTY`] 共同构成「每词固定罚」这一个旋钮（`Σ ln(f/T) = Σ ln(f) − n·ln T`），
/// 换词库导致的漂移只等价于该罚值的微调（词库总权重 +50% 也只让每词罚变化 0.4，相对
/// `WORD_PENALTY`=3.0 约 13%），故不必随词库精确浮动。
pub(crate) const DICT_TOTAL: f64 = 242_154_693.0;

/// 节点对数概率打分（对齐 Go lattice calcLogProb + 惩罚/加成）。
///
/// 基础分由词条自身的**词典权重**算出（见 [`score_node_inner`] 的长注释：为何不再查
/// unigram）。对 crate 内可见：`PinyinEngine::convert` 用它给「覆盖全部输入的词典精确整词」
/// 算单节点等价分，使其与 Viterbi 整句在同一量纲比较（见 mod.rs step 1.5）。
pub(crate) fn score_node(word: &str, weight: i32) -> f64 {
    score_node_inner(word, weight, true)
}

/// **尾部残码待定音节**专用打分：与 [`score_node`] 相同，但**不给单字虚词优待**。
///
/// ## 为什么必须去掉
///
/// 虚词优待合计 **8.0** 的量级差（`FUNCTION_WORD_BONUS` +2.0、实词 `SINGLE_CHAR_PENALTY`
/// −3.0、再豁免 `WORD_PENALTY` 3.0），足以碾压任何 unigram 差距。落在残码位上的后果：
/// `zhonghuar` 补出「中华**让**」而非「中华人」、`nihaom` 补出「你好**们**」而非「你好吗」
/// ——「让」「们」都在虚词表里，「人」「吗」不在。
///
/// ## 为什么这不是给残码开特例
///
/// 虚词优待的**前提**是「虚词随内容词出现是语法黏着，不该付投机拆分的代价」（见
/// [`score_node`] 内 `WORD_PENALTY` 处的长注释）。那个前提描述的是**整句内部**已经成形的
/// 搭配。残码位是「用户打到一半的那个音节」——它是虚词的先验并不比实词高，语法黏着
/// 无从谈起。**同一条加成，在两个位置的前提不同**，故按位置区分而非按词性区分。
pub(crate) fn score_node_partial_final(word: &str, weight: i32) -> f64 {
    score_node_inner(word, weight, false)
}

/// `function_word_credit`：是否给单字虚词优待，见 [`score_node_partial_final`]。
fn score_node_inner(word: &str, weight: i32, function_word_credit: bool) -> f64 {
    const SINGLE_CHAR_PENALTY: f64 = -3.0;
    const FUNCTION_WORD_BONUS: f64 = 2.0; // 虚词加成（Go 原名 functionWordPenalty，值为正）
    const VERB_PARTICLE_PENALTY: f64 = -1.0;
    const BASE_CONTENT_WORD_BONUS: f64 = 3.0;
    const LOG_PROB_MIN: f64 = -15.0;
    const LOG_PROB_RANGE: f64 = 12.0;

    let chars: Vec<char> = word.chars().collect();
    let char_count = chars.len();

    // 基础 logProb 直接由**该词条自己的**词典权重算出，不再查 unigram 表。
    //
    // ## 为什么不查 unigram（2026-08-08 合并）
    //
    // unigram.txt 由 `gen_unigram` 从**同一批 cn_dicts** 生成，实测 608,446/608,754
    // (99.95%) 的取值与 dict weight 完全相等、缺失 0 条 —— 它是一份副本，不是独立数据。
    // 而剩下的 0.05%（308 条）是**副本引入的缺陷**：`gen_unigram` 按词去重时取 `max`，
    // 于是多音字的冷僻读音被按常见读音计价：
    //
    //   说(shui) 真实 w=4  → 按 267,892 计价（虚高 66,973 倍）
    //   了(liao) 真实 w=51 → 按 1,758,342 计价（虚高 34,477 倍）
    //
    // 而调用方（`LatticeBuilder::build`）手上的 `hit.weight` 正是**该读音**的真实权重。
    // 对齐 librime：`dict_compiler.cc:257` 编译期 `log(w > 0 ? w : DBL_EPSILON)`，词图打分
    // 直接用词条自带 weight，**没有独立的 unigram 表**。（fcitx5 确有第二个数据源，但那是
    // 独立语料训练的 KenLM —— 提供词典没有的信息，与本副本性质不同。）
    //
    // ⚠️ [`DICT_TOTAL`] 不是精确的词库总权重，而是**标定系数**：整句分
    // `Σ ln(freq_i/TOTAL) = Σ ln(freq_i) − n·ln(TOTAL)`，其中 `−n·ln(TOTAL)` 与
    // `WORD_PENALTY` 的 `−n×3.0` 是同一种「每词固定罚」。二者共同构成一个已被实测标定的
    // 旋钮，故 TOTAL 无需随词库精确浮动；取当前值是为了让 99.95% 的词 log_prob **数值不变**，
    // 把改动的扰动面压到最小。
    let mut log_prob = if weight > 0 {
        (weight as f64 / DICT_TOTAL).ln()
    } else {
        // w ≤ 0 = 词库对「存疑 / 非标准读音」的标记（如 那→ne 方言读法）。等价于「频次 0.5」，
        // 与 librime 用 `DBL_EPSILON` 让这类条目排不上去同一思路。
        //
        // 旧实现是「各字平均(char_based_score) + CHAR_BASED_PENALTY，再减 10.0」，实测
        // 「种花人」为 −20.29，本式为 −19.99，**数值连续**；且不再依赖各字频率，
        // 由高频字组成的存疑词不会再借「各字平均」拿到虚高基础分。
        (0.5 / DICT_TOTAL).ln()
    };

    if char_count == 1 {
        if function_word_credit && is_function_word(word) {
            log_prob += FUNCTION_WORD_BONUS;
        } else {
            log_prob += SINGLE_CHAR_PENALTY;
        }
    } else if char_count > 1 {
        if chars
            .last()
            .map(|c| is_particle_suffix(*c))
            .unwrap_or(false)
        {
            log_prob += VERB_PARTICLE_PENALTY;
        } else if weight > 0 {
            // 原判据是 `ug.contains(word)`（词是否在 unigram 表内）。二者等价：
            // `gen_unigram` 只过滤 `freq<=0` 与纯 ASCII 词 ⇒ 凡 weight>0 的中文词都在表内。
            let freq_factor = ((log_prob - LOG_PROB_MIN) / LOG_PROB_RANGE).clamp(0.0, 1.0);
            log_prob += BASE_CONTENT_WORD_BONUS * (char_count as f64).sqrt() * freq_factor;
        }
    }
    // Phase 4：每词固定罚。Viterbi 的路径分是各节点 log_prob 之和，故「每节点减 W」
    // 等价于「按路径词数罚 k·W」——把低频词打碎成两个高频片段不再免费。
    // 也施加于 mod.rs step 1.5 的「单节点等价整句分」（那是一句一词，罚一次，量纲一致）。
    //
    // **虚词（是/的/了…）豁免每词罚**：WORD_PENALTY 意在阻止「把低频词打碎成高频
    // 片段」的投机拆分，而单字虚词随内容词出现是语法黏着、不是碎片。unigram 的独立性
    // 假设对 P(内容词)·P(虚词) 双重扣了 ln(total)（每词一份），一个低频 3 字整词
    // （填鸭式 w=152）便能压过「天涯+是」这种 2 词正解——这正是 bigram P(是|天涯)
    // 该解决而 unigram 解决不了的（缺磁盘语料，尚无 bigram）。豁免虚词
    // 的每词罚是对该缺陷的近似补偿：不让「虚词自成一词」这件语法必然的事付投机拆分的代价。
    if !(function_word_credit && char_count == 1 && is_function_word(word)) {
        log_prob -= WORD_PENALTY;
    }
    log_prob
}

/// 格子节点
#[derive(Debug, Clone)]
pub struct LatticeNode {
    pub start: usize,
    pub end: usize,
    pub word: String,
    pub syllables: Vec<String>,
    /// 本节点所采用切分的音节起始位 bitmask，**相对节点自身的 code 起点**
    /// （与词典 `DictEntry::boundary` 同域）。多路径下同一跨度可有多种切法，
    /// 故必须逐节点记录：Viterbi 选中哪条节点，整句的真实边界就是哪条。
    pub syl_mask: u64,
    pub log_prob: f64,
}

/// 格子构建器
pub struct LatticeBuilder {
    /// 最大词长（音节数）
    max_word_len: usize,
}

impl Default for LatticeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl LatticeBuilder {
    pub fn new() -> Self {
        // 10 而非 6：6 会把「中华人民共和国」(7 音节) 挡在词图外，却放行它的语义碎片
        // 「中华人民共和」(freq=2，法律条文名切出来的残片)，于是 Viterbi 只能在
        // 「中华人民共和」+「过」之类的错误切分里挑最优。上限须覆盖常见长专名。
        //
        // 超过上限的词典整词（「冠状动脉粥样硬化性心脏病」12 音节）仍进不了词图，但不需要
        // 额外兜底：整句改用等效词频后，错误拼接整句的 W_eff 是一串低频字的乘积、趋近
        // clamp 下限，词典整词的真实词频天然压过它。守门测试见 `tests/pinyin_long_word.rs`。
        Self { max_word_len: 10 }
    }

    /// 构建格子（**多路径切分**）
    ///
    /// 枚举的是**字节跨度** `(p, q)` 而非音节路径。这是本次改造的核心决策：
    ///
    /// 1. 音节恒为输入的连续子串，故查询码只由跨度决定 —— `input[p..q]`。
    ///    跨度对至多 O(n²)，而完整切分路径条数可指数增长（实测见
    ///    `tests/pinyin_path_scale.rs`）。
    /// 2. 「这个词是不是按某条合法路径敲出来的」不靠枚举路径回答，而是把词条自带的
    ///    `boundary` **当作一条待验证的路径**逐段查图（`SegGraph::mask_path`），
    ///    代价 O(音节数)。**路径爆炸因此在结构上不可能发生，无需剪枝。**
    ///
    /// 于是「西安交通大学」以真值 `xi|an|jiao|tong|da|xue` 合法入图，
    /// 而「李安」（真值 `li|an`）仍进不了单音节边 `lian` —— 前者是 Phase 1 里
    /// 被边界校验误杀的 4362 个词，后者是原始缺陷。两者第一次可以同时成立。
    ///
    /// `graph` 的形状决定切分来源：全拼用 `SegGraph::from_dag`（多路径），
    /// 双拼/手动分隔符用 `SegGraph::from_syllables`（真值链，行为与改造前完全一致）。
    /// `require_reachable`：是否只在「音节图上从 0 可达」的位置建节点。
    ///
    /// 常规路径传 `true`——不可达的位置上建节点纯属浪费，Viterbi 的 dp 永远到不了那里。
    /// **混合整句路径必须传 `false`**：简拼段会打断音节图的可达性（`bzdhaobuhao` 里
    /// b/z/d 都不成音节，位置 3 从 0 不可达），而 `[3,6) hao`、`[6,8) bu` 这些边其实
    /// 都在图里、只是被这道守卫挡住了；补上连接的是随后追加的简拼节点
    /// （见 [`Self::add_abbrev_nodes`]），那是音节图看不见的。
    pub fn build(
        &self,
        input: &str,
        graph: &SegGraph,
        dict: &CachedDict,
        fuzzy_config: Option<&FuzzyConfig>,
        require_reachable: bool,
    ) -> Vec<Vec<LatticeNode>> {
        let input_len = input.len();

        // nodes[end_pos] = 所有在 end_pos 结束的节点
        let mut nodes: Vec<Vec<LatticeNode>> = vec![Vec::new(); input_len + 1];

        for p in 0..input_len.min(graph.len()) {
            if require_reachable && !graph.is_reachable(p) {
                continue;
            }
            for q in graph.ends_within(p, self.max_word_len) {
                if q > input_len {
                    continue;
                }
                let code = &input[p..q];

                for hit in dict.search_with_boundary(code) {
                    // 词条真值边界必须是本跨度上的一条合法切分路径，否则该词根本不是
                    // 用户按这串键敲出来的：「李安」真值 li|an 与单音节边 lian 不符。
                    // boundary == 0（五笔码 / code 超 64 字节 / 旧格式）降级放行 ——
                    // 不设防好过误杀（与全仓其余边界判据一致）。
                    let offsets = match graph.mask_path(p, q, hit.boundary) {
                        MaskCheck::Path(syl_count) => {
                            if syl_count > self.max_word_len {
                                continue;
                            }
                            mask_offsets(hit.boundary, q - p)
                        }
                        MaskCheck::NoInfo => match graph.any_path(p, q, self.max_word_len) {
                            Some(o) => o,
                            None => continue,
                        },
                        MaskCheck::Reject => continue,
                    };
                    let log_prob = score_node(&hit.text, hit.weight)
                        - AMBIGUOUS_PENALTY * graph.ambiguous_count(p, q, &offsets) as f64;
                    nodes[q].push(LatticeNode {
                        start: p,
                        end: q,
                        word: hit.text,
                        syllables: slice_syllables(code, &offsets),
                        syl_mask: offsets_mask(&offsets),
                        log_prob,
                    });
                }

                // 模糊拼音变体
                //
                // **刻意不做边界校验**：词典返回的 boundary 是**变体码**空间的偏移
                // （zhongguo 的 {0,5}），而本跨度在用户**原码**空间（zong|guo 的
                // {0,4}）。z→zh 这类变体改变码长，两者位偏移不同域，直接比对会把正确的
                // 模糊命中整片误杀。这与 mod.rs 对模糊变体一律置 boundary=0
                // 的既有决策一致，是已记录的永久缺口（待跨域偏移映射），本阶段不碰。
                // 音节标注取图上任意一条最短路径——模糊命中没有可信真值切分，
                // 但节点仍需一个自洽的标注供整句边界回填。
                if let Some(fuzzy) = fuzzy_config.filter(|f| f.any_enabled()) {
                    // **先取切分，再逐音节展开变体**。此前这里对整串 `code` 调
                    // `fuzzy_variants`，而其声母规则是 `starts_with`、韵母规则是 `find`，
                    // 对多音节串只能改到首音节声母与第一处韵母——`zhongzou`→`zhongzhou`
                    // （中州）这类非首音节模糊整片丢失。切分本就在下面 `slice_syllables`
                    // 里用着，只是没回头喂给变体生成（同 P1 记的「信息拿在手上，用完即弃」）。
                    let Some(offsets) = graph.any_path(p, q, self.max_word_len) else {
                        continue;
                    };
                    let syls = slice_syllables(code, &offsets);
                    for (variant, fuzzy_edits) in FuzzyMatcher::expand_syllables(&syls, fuzzy) {
                        // 全原音节组合 == 原码，属精确命中，已由上面的 search_with_boundary
                        // 循环加入（且带真值边界校验），不可在此重复添加为模糊节点。
                        if variant == code {
                            continue;
                        }
                        for (text, weight, _order) in &dict.search(&variant) {
                            // 去重
                            if nodes[q].iter().any(|n| n.word == *text && n.start == p) {
                                continue;
                            }
                            // 模糊命中同样按图上那条标注路径计歧义罚：惩罚是**切分**的
                            // 属性（该路径是否踩在歧义接缝上），与词条来源无关。
                            let log_prob = score_node(text, *weight)
                                - FUZZY_SYLLABLE_LOG_PENALTY * fuzzy_edits as f64
                                - AMBIGUOUS_PENALTY * graph.ambiguous_count(p, q, &offsets) as f64;
                            nodes[q].push(LatticeNode {
                                start: p,
                                end: q,
                                word: text.clone(),
                                syllables: slice_syllables(code, &offsets),
                                syl_mask: offsets_mask(&offsets),
                                log_prob,
                            });
                        }
                    }
                }
            }
        }

        nodes
    }

    /// 在已建好的词图上**追加简拼节点**，供混合整句解码（`bzd` + `haobuhao` → 不知道好不好）。
    ///
    /// ## 为什么必须独立枚举跨度
    ///
    /// [`Self::build`] 的跨度来自 `graph.ends_within(p, ..)` —— 音节图的合法终点，且开头
    /// 有 `graph.is_reachable(p)` 守卫。简拼段两条都不满足：`bzdhaobuhao` 的 b/z/d 都不成
    /// 音节，位置 0 在音节图上根本不可达，从它出发也没有任何终点。所以简拼节点走独立枚举：
    /// 任意 `(p, q)` 且 `q - p ∈ [2, MAX_ABBREV_SPAN]`。
    ///
    /// ## 与全拼节点的兼容性
    ///
    /// [`LatticeNode`] 的 `start`/`end` 是**字节跨度**、Viterbi 的 dp 也按字节位置推进，
    /// 故简拼节点与全拼节点在同一张图里天然可串：`[0,3)` 的「不知道」接上 `[3,6)` 的
    /// 「好」，dp 一路推到串尾。**Viterbi 一行都不用改。**
    ///
    /// ## 音节标注
    ///
    /// 简拼段里**每个字母就是一个音节的位置**（击键空间），故 `syllables` 逐字母切、
    /// `syl_mask` 每字母一位。整句 boundary 由此回填出 `b'z'd'hao'bu'hao` 这样的显示，
    /// 与击键串同域——这正是简拼候选一贯的做法（见 `mixed_abbrev` 模块文档）。
    ///
    /// ⚠️ `input` 必须是**原始击键串**。双拼下 `input` 是转换后的全拼、与击键不同域，
    /// 简拼判据会全部失配（文档 §5 约束 4），故调用方须在双拼下跳过本方法。
    pub fn add_abbrev_nodes(&self, input: &str, dict: &CachedDict, nodes: &mut [Vec<LatticeNode>]) {
        let input_len = input.len();
        let bytes = input.as_bytes();
        for p in 0..input_len {
            // 简拼段每个字母都必须是小写 ASCII（声母），一遇到非法字符即可停止从此处出发
            if !bytes[p].is_ascii_lowercase() {
                continue;
            }
            for span in 2..=MAX_ABBREV_SPAN {
                let q = p + span;
                if q > input_len || q >= nodes.len() {
                    break;
                }
                if !bytes[p..q].iter().all(|b| b.is_ascii_lowercase()) {
                    break;
                }
                let stroke = &input[p..q];
                for abbr_code in dict.search_abbrev(stroke, ABBREV_NODE_LIMIT) {
                    for hit in dict.search_with_boundary(&abbr_code) {
                        // **音节数必须等于简拼字母数**（同 mod.rs step5 的过滤）：扁平码有损，
                        // `xa` 指向的 `xian` 回查主表会把 1 音节的「先」一并捞出来。
                        // boundary==0 无从校验，直接跳过——混合整句的每个节点都要求真值切分。
                        if hit.boundary.count_ones() as usize != span {
                            continue;
                        }
                        if nodes[q].iter().any(|n| n.word == hit.text && n.start == p) {
                            continue;
                        }
                        let log_prob =
                            score_node(&hit.text, hit.weight) - ABBREV_NODE_PENALTY * span as f64;
                        nodes[q].push(LatticeNode {
                            start: p,
                            end: q,
                            word: hit.text,
                            // 击键空间：每个字母一个音节位
                            syllables: stroke.chars().map(|c| c.to_string()).collect(),
                            syl_mask: (0..span).fold(0u64, |m, i| m | (1u64 << i)),
                            log_prob,
                        });
                    }
                }
            }
        }
    }

    /// 在已建好的词图上**追加尾部残码节点**，供残码补全整句解码
    /// （`buzhidaok` → 「不知道」+ 残码 `k` 补成「看」→ 整句「不知道看」）。
    ///
    /// ## 这条通路解决什么
    ///
    /// 尾部残码（未成音节的声母/半音节）原本被完全排除在整句解码之外——`convert` 只在
    /// `completed`（完整音节前缀）上建图，`buzhidaok` 的整句止步于「不知道」，末尾的 `k`
    /// 无人认领。主流输入法（librime `enable_completion`、fcitx5 的「不完整拼音」）都会把
    /// 残码当作一个**待定音节**放进格子，由 LM 选出最优单字。本方法补上这条通路。
    ///
    /// ## 为什么不能让 [`Self::add_abbrev_nodes`] 兼职
    ///
    /// 二者都是「给词图补音节图给不出的节点」，长得几乎一样，但**约束强弱不同**：简拼节点
    /// 会把整串重新按声母序列切分，已完成的音节也会被重切。实测放开简拼闸门让残码入图，
    /// `buzhidaok` 产出的是「不直达欧卡」、`nihaom` 是「你黑暗欧美」——`bu zhi dao` 被拆回
    /// b/u/zh/i/d/a/o 去凑简拼了。残码补全要的是**保留已完成音节、只展开最后那一段**，
    /// 是简拼组句的严格子集约束，故必须独立成路。
    ///
    /// ## 召回与音节标注
    ///
    /// 候选 = 以残码为前缀、**音节数为 1** 的词条（`syllable_capped(.., 1)` 把过滤下推到
    /// 词库层），再筛出单字。跨度恒为 `[completed_len, input_len)` 的一整段，`syl_mask` 记
    /// 一个音节位——残码在击键空间就是「一个还没打完的音节」，与全拼节点在同一字节空间里
    /// 天然可串（理由同 [`Self::add_abbrev_nodes`] 的「与全拼节点的兼容性」）。
    ///
    /// ⚠️ 调用方须保证 `nodes.len() > input_len`，即词图建在**含残码的整串**上而非
    /// `completed` 上——否则残码末端没有槽位，Viterbi 永远到不了串尾。
    ///
    /// ⚠️ `input` 必须是**原始击键串**，双拼下须跳过（理由同 [`Self::add_abbrev_nodes`]）。
    pub fn add_partial_final_nodes(
        &self,
        input: &str,
        completed_len: usize,
        dict: &CachedDict,
        nodes: &mut [Vec<LatticeNode>],
    ) {
        let input_len = input.len();
        if completed_len >= input_len || input_len >= nodes.len() {
            return;
        }
        let partial = &input[completed_len..];
        // 残码必须整段是小写 ASCII 字母：分隔符/数字/大写都不是「没打完的音节」。
        if !partial.bytes().all(|b| b.is_ascii_lowercase()) {
            return;
        }
        // 对齐位传 0：`partial` 本身就是「还没打完的那个音节」，它相对自己没有任何已完成
        // 音节，位 0 恒置位 ⇒ 边界判据整条让开，保持纯字符前缀匹配（`m` → 吗/么/没）。
        // 这正是 `prefix_syllable_aligned` 为残码场景留的退化通道，不是遗漏。
        for hit in dict.search_prefix_with_boundary_syllable_capped(
            partial,
            PARTIAL_FINAL_NODE_LIMIT,
            1,
            0,
        ) {
            // 只收单字：残码补的是「正在打的那一个字」。单音节多字词（儿化/连绵词）落到
            // 这里会让一个待定音节兑出两个字，与用户已敲的音节数对不上。
            if hit.text.chars().count() != 1 {
                continue;
            }
            if nodes[input_len]
                .iter()
                .any(|n| n.word == hit.text && n.start == completed_len)
            {
                continue;
            }
            // ⚠️ 用 `score_node_partial_final` 而非 `score_node`：残码位不给虚词优待，
            // 否则「让」「们」这类虚词凭 8.0 的量级差碾压「人」「吗」。见该函数文档。
            let log_prob = score_node_partial_final(&hit.text, hit.weight) - PARTIAL_FINAL_PENALTY;
            nodes[input_len].push(LatticeNode {
                start: completed_len,
                end: input_len,
                word: hit.text,
                syllables: vec![partial.to_string()],
                syl_mask: 1,
                log_prob,
            });
        }
    }
}

/// 由 bitmask 还原音节起始偏移列表（升序，恒以 0 开头）。
fn mask_offsets(mask: u64, len: usize) -> Vec<usize> {
    let mut out = Vec::new();
    for i in 0..len.min(64) {
        if (mask >> i) & 1 == 1 {
            out.push(i);
        }
    }
    out
}

fn offsets_mask(offsets: &[usize]) -> u64 {
    let mut m = 0u64;
    for &o in offsets {
        if o < 64 {
            m |= 1u64 << o;
        }
    }
    m
}

/// 按起始偏移把 code 切成音节串。
fn slice_syllables(code: &str, offsets: &[usize]) -> Vec<String> {
    let mut out = Vec::with_capacity(offsets.len());
    for (i, &o) in offsets.iter().enumerate() {
        let end = offsets.get(i + 1).copied().unwrap_or(code.len());
        if o <= end && end <= code.len() {
            out.push(code[o..end].to_string());
        }
    }
    out
}
