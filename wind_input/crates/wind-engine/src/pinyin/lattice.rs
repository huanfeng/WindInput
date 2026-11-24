// Portions derived from librime / librime-octagram, Copyright (c) 2014 RIME Developers, BSD-3-Clause (see NOTICE.md).
//! 格子构建（Lattice）+ 多切分评分
//!
//! 构建词图并支持多路径评分，用于 Viterbi 解码。

use crate::pinyin::dag::{MaskCheck, SegGraph};
use crate::pinyin::fuzzy::{FuzzyConfig, FuzzyMatcher};
use crate::pinyin::syllable::SyllableTrie;
use std::collections::{BTreeMap, HashMap};
use wind_dict::cached::CachedDict;
use wind_dict::manager::DictManager;

/// 虚词表：**字形 → 该字作虚词时的读音**（无调全拼）。
///
/// 判据必须同时问字和读音：虚词优待合计约 8.0 nat（≈2981 倍词频），只按字形发放时多音字
/// 的冷僻读音会白拿优待、压掉该音位上的高频字（kuai 下「会」压「快」）。数据见
/// candidate-sorting-rules.md「拼音常量的取值依据」。
///
/// 只登记该字作虚词/代词/助动词时的读音，实词读音不登记（会 kuai、还 huan、了 liao…）。
/// 码是无调全拼，同码不同调无法区分（`得` de），这是无调码的固有上界。
fn function_word_readings(w: &str) -> Option<&'static [&'static str]> {
    Some(match w {
        // —— 助词 ——
        "了" => &["le"],
        "的" => &["de"],
        "地" => &["de"],
        "得" => &["de", "dei"],
        "着" => &["zhe"],
        "过" => &["guo"],
        // —— 代词 ——
        "我" => &["wo"],
        "你" => &["ni"],
        "他" => &["ta"],
        "她" => &["ta"],
        "它" => &["ta"],
        "们" => &["men"],
        "这" => &["zhe", "zhei"],
        "那" => &["na", "nei"],
        // —— 连词 / 介词 ——
        "和" => &["he"],
        "与" => &["yu"],
        "在" => &["zai"],
        "把" => &["ba"],
        "被" => &["bei"],
        "让" => &["rang"],
        "从" => &["cong"],
        "到" => &["dao"],
        "对" => &["dui"],
        "向" => &["xiang"],
        "跟" => &["gen"],
        // —— 副词 ——
        "不" => &["bu"],
        "没" => &["mei"],
        "也" => &["ye"],
        "都" => &["dou"],
        "就" => &["jiu"],
        "才" => &["cai"],
        "还" => &["hai"],
        "又" => &["you"],
        "再" => &["zai"],
        "很" => &["hen"],
        "太" => &["tai"],
        "最" => &["zui"],
        // —— 高频动词 / 助动词 ——
        "是" => &["shi"],
        "有" => &["you"],
        "会" => &["hui"],
        "能" => &["neng"],
        "要" => &["yao"],
        "可" => &["ke"],
        "去" => &["qu"],
        "来" => &["lai"],
        "做" => &["zuo"],
        "说" => &["shuo"],
        "看" => &["kan"],
        "想" => &["xiang"],
        _ => return None,
    })
}

/// 该单字节点是否够格拿虚词优待：字形在表内，**且**本节点的码正是它作虚词时的读音。
///
/// `code` 是取到这个词条所用的**词典码**（模糊命中传变体码、简拼节点传回查出的全码），
/// 不是用户原始击键——虚词性是「这个字这次读什么」的属性，跟着码走才对得上。
fn is_function_word(w: &str, code: &str) -> bool {
    function_word_readings(w).is_some_and(|readings| readings.contains(&code))
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
/// 0.35 是刀刃值，改动前务必重跑 `pinyin_eval` 的定点（`lianzhengtixing` / `liandaoyan`），
/// 依据见 `docs/design/pinyin-boundary-aware-lattice.md`。
pub(crate) const AMBIGUOUS_PENALTY: f64 = 0.35;

/// 词图节点的**每模糊音节**对数惩罚，`ln(2)` ⇒ 概率域每个模糊音节乘 0.5。
///
/// 与候选层的 [`super::FUZZY_WEIGHT_SCALE`]（=0.5，乘性）是**同一个量在两个域的表达**，
/// 两者必须同步改：候选层管词典直查的模糊命中，本常数管词图节点、进而管整句路径。
///
/// 取值对齐 librime `kFuzzySpellingPenalty`（`log(0.5)`）与 libime `fuzzyCost`（`log10(0.5)`）：
/// 每个模糊拼写 0.5、多个累乘。
pub(crate) const FUZZY_SYLLABLE_LOG_PENALTY: f64 = std::f64::consts::LN_2;

/// 简拼节点每个音节的惩罚（混合整句解码用，见 [`LatticeBuilder::add_abbrev_nodes`]）。
///
/// 按音节数计而非固定值：简拼段越长不确定性越大，固定罚会让长段不合理地便宜。
/// 由 `pinyin_eval` 的 D 类对账定出，改动前必须重跑（见 `pinyin-mixed-abbrev.md` §4.8）。
pub(crate) const ABBREV_NODE_PENALTY: f64 = 1.2;

/// 混合整句里把 `zh` / `ch` / `sh` **拆成两个声母**（z|h）时，每处拆分的扣分（对数域）。
///
/// 用户打 `zh` 多半要翘舌声母，读成 z|h 会让 `zhge` 组出「之后个」。拆开读并非不合法
/// （「最后」的简拼就是 `zh`），故是罚不是禁。取值由 `pinyin_eval` 扫参定出，见
/// candidate-sorting-rules.md「拼音常量的取值依据」。
pub(crate) const RETROFLEX_SPLIT_PENALTY: f64 = 8.0;

/// 混合整句里简拼段**抢走前一全拼音节的韵尾**时的扣分（对数域）。
///
/// `ningbr`（宁波人）被读成 ni + n|g + b|r 组出「你能够比如」：用户打全了一个音节，就几乎
/// 不会是要把韵尾拆给下一个词当声母。判据见 [`steals_coda`]；同 [`RETROFLEX_SPLIT_PENALTY`]，
/// 是罚不是禁。取值依据见 candidate-sorting-rules.md「拼音常量的取值依据」。
pub(crate) const CODA_STEAL_PENALTY: f64 = 8.0;

/// 单个简拼跨度最多取几个词进图。
///
/// 全塞进去会让节点与边数膨胀，而排在后面的低频词几乎赢不下整句路径。
const ABBREV_NODE_LIMIT: usize = 8;

/// 单个跨度上最多取几条用户词入图（[`LatticeBuilder::add_store_nodes`]）。
///
/// 同一个 code 下的用户词通常只有一两条（用户不会给同一串拼音造十个词），8 是宽松上界；
/// 它同时是成本闸门：整句建图要遍历全部 (p, q) 跨度，每个跨度一次 redb 点查。
pub(crate) const USER_NODE_LIMIT: usize = 8;

/// 用户层简拼节点每个跨度**取数**的上限（过判据、按权重取前 [`USER_NODE_LIMIT`] 之前）。
///
/// 存储层按索引键字典序数够上限就返回、不按权重：太小会在判据前切掉高权重词，太大让按键
/// 路径随用户库规模变慢（延迟数据见 candidate-sorting-rules.md「拼音常量的取值依据」）。
///
/// 已知限制：同一声母串的用户词超过本值时，超出部分按码的字典序被截，与权重无关。
/// 值与引擎侧 `ABBREV_INDEX_LIMIT` 同为 64。
const USER_ABBREV_SCAN_LIMIT: usize = 64;

/// 模糊入图（系统词与用户词共用）时，从一个起点出发、**同一音节数**上最多保留几条在途的
/// （所打码切分 × 逐音节变体）组合（[`LatticeBuilder::fuzzy_spans`]）。
///
/// 超出时按改动处数升序保留，截掉的只是模糊处数多的组合（及其后继——后继的处数只增不减）。
/// 每条组合在它到达的终点上各是一次候选点查（按变体码归并后），故这也是成本闸门。
///
/// 取 256（「64 种组合 × 4 条切分」的量级），性能见
/// `pinyin_sentence_user_words_probe::fuzzy_store_node_cost_on_real_dict`。
pub(crate) const FUZZY_LAYER_BUDGET: usize = 256;

/// 各音节边 `(起, 止)` 的变体表（首项恒为原音节、0 处改动）：同一条边会被许多起点、
/// 许多前缀组合反复展开，算一次即可。
pub(crate) type EdgeVariants = HashMap<(usize, usize), Vec<(String, usize)>>;

/// 模糊变体到达某终点的一种形态：(变体边界, 所打码切分（相对起点的偏移）, 改动处数)。
pub(crate) type FuzzyShape = (u64, Vec<usize>, usize);

/// 用户词入图时的**对数域加成**（[`LatticeBuilder::add_store_nodes`]）。
///
/// 用户词与词频同轴但语义不同（「我要这个词」）：出厂加词权重恰好翻不过同码中频词，整句
/// 仍不认。不抬 `ADD_WORD_WEIGHT`，那会动候选层的标定。+2.0 ≈ ×7.4，翻得过多数同码竞争者，
/// 仍输给真正的高频词（数据见 candidate-sorting-rules.md「拼音常量的取值依据」）。
///
/// 加在 `score_node` 之后而不乘在 weight 上：`BASE_CONTENT_WORD_BONUS` 由 log_prob 缩放，
/// 放大 weight 会把那份加成也放大一次。
pub(crate) const USER_NODE_BONUS: f64 = 2.0;

/// 用户词入图时的 weight 上限（[`LatticeBuilder::add_store_nodes`]）。
///
/// 默认档位本就合理，不做分布重映射，只防导入 wdict 带来的极端值（`w = 2e9` 会让整句变成
/// 用户词拼接秀）。1e6 约 p99.99+，几乎压过所有系统词又仍在词频轴内侧。
pub(crate) const USER_NODE_WEIGHT_CAP: i32 = 1_000_000;

/// 用户层里**哪些词进整句词图**：用户词与临时词（都是用户上屏过的），不收草稿。
///
/// 草稿在拼音侧目前根本不挂层（只码表造词挂），`is_draft` 这一条是防将来挂上时静默漏进来。
pub(crate) fn is_sentence_store_word(c: &wind_candidate::Candidate) -> bool {
    (c.meta.is_user_dict || c.meta.is_temp_dict) && !c.is_draft
}

/// 简拼跨度的最大字母数（= 最大音节数）。与 `AbbrevMatcher::find_candidates` 的上限一致。
const MAX_ABBREV_SPAN: usize = 6;

/// 尾部残码补全节点（[`LatticeBuilder::add_partial_final_nodes`]）的对数概率罚。
///
/// 对齐 librime `kCompletionPenalty = log(0.5)`：把残码当成某个具体音节是一次预测。
///
/// 不按残码长度递减：候选面收窄已由召回集合表达，再折扣等于扣两次。`ABBREV_NODE_PENALTY`
/// 按音节数递增恰是反面：简拼段的召回集合不随段长收窄。
pub(crate) const PARTIAL_FINAL_PENALTY: f64 = std::f64::consts::LN_2;

/// 尾部残码跨度最多取几个单字进图。
///
/// 残码召回面比简拼更宽，但能赢下整句路径的只有高频字，故比 `ABBREV_NODE_LIMIT` 略放宽。
pub(crate) const PARTIAL_FINAL_NODE_LIMIT: usize = 12;

/// 词频归一化基准 —— **标定系数，非精确的词库总权重**，理由见 [`score_node_inner`]。
///
/// 取值是旧 unigram 表的总频次，与 `cn_dicts` 总权重差 0.41%。它与 [`WORD_PENALTY`] 共同构成
/// 「每词固定罚」一个旋钮（`Σ ln(f/T) = Σ ln(f) − n·ln T`），换词库的漂移只等价于该罚值微调，
/// 故不必随词库精确浮动。见 `docs/design/sentence-weight-same-axis.md` §4.1。
pub(crate) const DICT_TOTAL: f64 = 242_154_693.0;

/// 节点对数概率打分（对齐 Go lattice calcLogProb + 惩罚/加成）。
///
/// 基础分由词条自身的**词典权重**算出（见 [`score_node_inner`] 的长注释：为何不再查
/// unigram）。`code` 是**取到这条词条所用的词典码**，供单字虚词判据核对读音——不是用户
/// 原始击键，两者在模糊/简拼路径上并不相同，见 [`is_function_word`]。
///
/// 因此 `code` 必须落在无调全拼域。若让双拼击键直接进词图，全表虚词会静默掉权 8.0 nat。
pub(crate) fn score_node(word: &str, code: &str, weight: i32) -> f64 {
    score_node_inner(word, code, weight, true)
}

/// **尾部残码待定音节**专用打分：与 [`score_node`] 相同，但**不给单字虚词优待**。
///
/// 虚词优待（约 8.0 nat）的前提是整句内部已成形的语法黏着；残码位是用户打到一半的音节，
/// 前提不成立，否则 `nihaom` 补出「你好们」。按位置区分而非按词性区分，见
/// candidate-sorting-rules.md §4.2。
pub(crate) fn score_node_partial_final(word: &str, weight: i32) -> f64 {
    // 不给虚词优待 ⇒ 读音无从参与判据，码传空串即可（`is_function_word` 恒 false）。
    score_node_inner(word, "", weight, false)
}

/// `function_word_credit`：是否给单字虚词优待，见 [`score_node_partial_final`]。
fn score_node_inner(word: &str, code: &str, weight: i32, function_word_credit: bool) -> f64 {
    const SINGLE_CHAR_PENALTY: f64 = -3.0;
    const FUNCTION_WORD_BONUS: f64 = 2.0; // 虚词加成（Go 原名 functionWordPenalty，值为正）
    const VERB_PARTICLE_PENALTY: f64 = -1.0;
    const BASE_CONTENT_WORD_BONUS: f64 = 3.0;
    const LOG_PROB_MIN: f64 = -15.0;
    const LOG_PROB_RANGE: f64 = 12.0;

    let chars: Vec<char> = word.chars().collect();
    let char_count = chars.len();

    // 基础 logProb 直接由该词条（该读音）自己的词典权重算出，不查独立的 unigram 表：
    // 那是同一批词库的副本，按词取 max 会让多音字冷僻读音按常见读音计价。对齐 librime
    // （词图打分直接用词条 weight）。见 `docs/design/sentence-weight-same-axis.md`。
    let mut log_prob = if weight > 0 {
        (weight as f64 / DICT_TOTAL).ln()
    } else {
        // w ≤ 0 = 词库对存疑 / 非标准读音的标记，等价于「频次 0.5」（同 librime `DBL_EPSILON`）。
        // 不按各字平均估分：由高频字组成的存疑词会借此拿到虚高基础分。
        (0.5 / DICT_TOTAL).ln()
    };

    // 虚词优待是一件事的两半（加成 + 豁免每词罚），判据绑一次、两处共用，免得改一处漏一处。
    let function_word = function_word_credit && char_count == 1 && is_function_word(word, code);

    if char_count == 1 {
        if function_word {
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
            // 等价于旧判据「词在 unigram 表内」（见 sentence-weight-same-axis.md）。
            let freq_factor = ((log_prob - LOG_PROB_MIN) / LOG_PROB_RANGE).clamp(0.0, 1.0);
            log_prob += BASE_CONTENT_WORD_BONUS * (char_count as f64).sqrt() * freq_factor;
        }
    }
    // 每词固定罚：路径分是节点之和，「每节点减 W」即「按词数罚」，把低频词打碎成高频片段
    // 不再免费。
    //
    // 单字虚词豁免：它随内容词出现是语法黏着而非碎片；unigram 独立性假设会让低频整词
    // （填鸭式）压过「天涯+是」这类正解，豁免是缺 bigram 时的近似补偿。
    if !function_word {
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
    /// **造词用的规范码**：(词条码, 该码自身坐标下的音节边界)，所打码不是规范码时才填：
    /// - 模糊命中：命中的词条码；
    /// - 简拼节点（系统 / 用户）：回查出的完整全拼与它的真值边界——击键字母 `bzd` 不是
    ///   能再打出来的码；
    /// - 精确命中为 `None`（规范码即所打码 `input[start..end]` 那一段，边界即 `syl_mask`，
    ///   零分配）；残码节点也为 `None`（补出的字读什么是预测，造词码保留所打字母）。
    ///
    /// 只服务一件事：整句候选的 `Candidate::meta.learn_code`。整句的 `code` 必须是所打码
    /// （`consumed_length` / 分步上屏绑在它上面），而单段整句上屏造词时要写的是用户下次真能
    /// 打出来的规范码——`chaijiuduolian` 的整句若按所打码落库，下次同样打法零罚精确命中这条
    /// 非规范记录。Viterbi 回溯把它透传出来（`ViterbiResult::learn_code`）。
    pub canon: Option<(String, u64)>,
}

/// 词图里把 `zh` / `ch` / `sh` **拆成两个声母**（z|h）的次数：某个 `h` 紧跟在 z/c/s 之后、
/// 且这个 `h` 是一个新音节的起点。
///
/// 两种形态用同一条判据覆盖：
/// - 节点内部：简拼节点「之后」(`zh` = z|h) —— `syl_mask` 在 `h` 那一位置位；
/// - 跨节点：节点从 `h` 起、前一字节是 z/c/s —— 音节不可能以 z/c/s 结尾，前一段必是以该
///   字母作声母的简拼段，于是同样是拆开的。
fn retroflex_splits(input: &[u8], node: &LatticeNode) -> usize {
    let is_zcs = |b: u8| matches!(b, b'z' | b'c' | b's');
    let starts_syllable = |i: usize| (node.syl_mask >> (i - node.start)) & 1 == 1;
    (node.start..node.end)
        .filter(|&i| input[i] == b'h' && i > 0 && is_zcs(input[i - 1]) && starts_syllable(i))
        .count()
}

/// 节点是否**抢了前一音节的韵尾**：节点的第一段是单字母声母（起点 `p`），而 `p` 前面有一个
/// 音节把 `input[p]` 吸收进去后仍是合法音节（`ningbr` 读成 ni + n|g|… 时，`nin` / `ning`
/// 都是音节）。
///
/// 只看「`p` 前面**存在**这样一个音节」，不看 Viterbi 实际选的前驱是谁——节点打分时还不知道
/// 前驱。于是前驱若是个恰好以同一字母收尾的简拼段，也会被一并扣分；评测里 D 类（简拼词 +
/// 全拼词长串）一条未变，说明这种误伤在实际输入里罕见。
fn steals_coda(input: &str, trie: &SyllableTrie, node: &LatticeNode) -> bool {
    let p = node.start;
    if p == 0 || node.end <= p {
        return false;
    }
    // a/e/o 起头时要分清两种节点：
    // - **全拼节点**的零声母音节（「阿姨」a|yi）：不是抢韵尾，`ji` + 阿姨 不该因 `jia` 是音节
    //   而被罚（审查查出）；
    // - **简拼节点**的元音声母（e = er / en…、o = ou…）：照样是抢，`yuedzh` 读成 yu + 耳朵(e|d)
    //   组出「与耳朵之后」、`naoxh` 读成 na + 偶像(o|x) 组出「那偶像化」。一律豁免 a/e/o 时
    //   评测 E 类正好丢这两条。
    // 简拼节点的特征是每段都是单个字母（`add_abbrev_nodes` / `add_store_abbrev_nodes` 按击键
    // 逐字母填 `syllables`），且至少两段。
    let is_abbrev_node = node.syllables.len() >= 2 && node.syllables.iter().all(|s| s.len() == 1);
    if matches!(input.as_bytes()[p], b'a' | b'e' | b'o') && !is_abbrev_node {
        return false;
    }
    // 第一段长度：syl_mask 里 bit0 之后的下一个置位，或到节点末尾。
    let seg_len = (1..node.end - p)
        .find(|&i| (node.syl_mask >> i) & 1 == 1)
        .unwrap_or(node.end - p);
    if seg_len != 1 {
        return false;
    }
    (p.saturating_sub(6)..p).any(|q| {
        input.is_char_boundary(q)
            && trie.is_syllable(&input[q..p])
            && trie.is_syllable(&input[q..=p])
    })
}

/// 对抢了前一音节韵尾的节点扣 [`CODA_STEAL_PENALTY`]（判据见 [`steals_coda`]）。
///
/// 与 [`penalize_retroflex_splits`] 同处调用：②b 全部节点进图之后。
pub fn penalize_coda_steals(input: &str, trie: &SyllableTrie, nodes: &mut [Vec<LatticeNode>]) {
    for at_end in nodes.iter_mut() {
        for node in at_end.iter_mut() {
            if steals_coda(input, trie, node) {
                node.log_prob -= CODA_STEAL_PENALTY;
            }
        }
    }
}

/// 对把 zh/ch/sh 拆成两个声母的节点扣分：每处 [`RETROFLEX_SPLIT_PENALTY`]（判据见
/// [`retroflex_splits`]）。
///
/// 须在**所有节点都进图之后**调用（系统简拼节点、用户层节点都会拆），且只用于混合整句
/// （②b）：纯全拼词图里音节不会以 z/c/s 结尾，`h` 前面不可能是拆开的声母，调用是空操作。
pub fn penalize_retroflex_splits(input: &str, nodes: &mut [Vec<LatticeNode>]) {
    let penalty = RETROFLEX_SPLIT_PENALTY;
    let bytes = input.as_bytes();
    for at_end in nodes.iter_mut() {
        for node in at_end.iter_mut() {
            let n = retroflex_splits(bytes, node);
            if n > 0 {
                node.log_prob -= penalty * n as f64;
            }
        }
    }
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
        // 上限须覆盖常见长专名（「中华人民共和国」7 音节），否则只放行其碎片。更长的词典整词
        // 进不了词图也无需兜底：错误拼接整句的等效词频趋近下限，压不过它（`tests/pinyin_long_word.rs`）。
        Self { max_word_len: 10 }
    }

    /// 最大词长（音节数）。
    pub(crate) fn max_word_len(&self) -> usize {
        self.max_word_len
    }

    /// 构建格子（**多路径切分**）
    ///
    /// 枚举字节跨度 `(p, q)` 而非音节路径：跨度对至多 O(n²)，而切分路径可指数增长
    /// （`tests/pinyin_path_scale.rs`）。词是否按合法路径敲出，靠把词条 `boundary` 当一条待验证
    /// 路径逐段查图（`SegGraph::mask_path`），路径爆炸在结构上不可能发生。于是「西安交通大学」
    /// 按真值 `xi|an|…` 入图，而「李安」（`li|an`）进不了单音节边 `lian`。
    ///
    /// `graph` 的形状决定切分来源：全拼用 `SegGraph::from_dag`（多路径），
    /// 双拼/手动分隔符用 `SegGraph::from_syllables`（真值链，行为与改造前完全一致）。
    /// `require_reachable`：是否只在「音节图上从 0 可达」的位置建节点。
    ///
    /// 常规路径传 `true`——不可达的位置上建节点纯属浪费，Viterbi 的 dp 永远到不了那里。
    /// 混合整句路径必须传 `false`：简拼段会打断音节图的可达性（`bzdhaobuhao` 位置 3 从 0
    /// 不可达），连接由随后追加的简拼节点（[`Self::add_abbrev_nodes`]）补上。
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
        let mut edge_variants = EdgeVariants::new();

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
                    let log_prob = score_node(&hit.text, code, hit.weight)
                        - AMBIGUOUS_PENALTY * graph.ambiguous_count(p, q, &offsets) as f64;
                    nodes[q].push(LatticeNode {
                        start: p,
                        end: q,
                        word: hit.text,
                        syllables: slice_syllables(code, &offsets),
                        syl_mask: offsets_mask(&offsets),
                        log_prob,
                        canon: None,
                    });
                }
            }

            // 模糊拼音变体
            //
            // 词条边界在变体码空间，不能拿去比所打码的偏移（z→zh 改变码长），只和同域的变体
            // 边界比。变体按图上每条切分逐音节展开（[`Self::fuzzy_spans`]）。词条有边界时须等于
            // 某个形态的变体边界（`xi|an → xiang` 撞上单音节「想」就该拒），无边界时放行、取改动
            // 最少的形态。
            let Some(fuzzy) = fuzzy_config.filter(|f| f.any_enabled()) else {
                continue;
            };
            let spans = self.fuzzy_spans(
                p,
                input,
                graph,
                fuzzy,
                &mut edge_variants,
                input_len,
                self.max_word_len,
                &|v| dict.has_longer_code(v),
            );
            for ((q, variant), shapes) in &spans {
                let q = *q;
                let code = &input[p..q];
                for hit in dict.search_with_boundary(variant) {
                    let Some((variant_mask, offsets, fuzzy_edits)) = shapes
                        .iter()
                        .filter(|(mask, _, _)| hit.boundary == 0 || *mask == hit.boundary)
                        .min_by_key(|(_, _, edits)| *edits)
                    else {
                        continue;
                    };
                    // 模糊命中同样按所选切分计歧义罚：惩罚是**切分**的属性（该路径是否踩在
                    // 歧义接缝上），与词条来源无关。
                    let log_prob = score_node(&hit.text, variant, hit.weight)
                        - FUZZY_SYLLABLE_LOG_PENALTY * *fuzzy_edits as f64
                        - AMBIGUOUS_PENALTY * graph.ambiguous_count(p, q, offsets) as f64;
                    // 规范边界取变体边界：词条有边界时上面已验过两者相等，没有时只能用它。
                    let canon_mask = *variant_mask;
                    // 同词同起点：精确节点恒胜（不带模糊罚）；模糊节点之间取分高者，
                    // 切分与规范码随胜者一起换（多个变体码可命中同一个词）。
                    if let Some(existing) = nodes[q]
                        .iter_mut()
                        .find(|n| n.word == hit.text && n.start == p)
                    {
                        if existing.canon.is_some() && log_prob > existing.log_prob {
                            existing.log_prob = log_prob;
                            existing.syllables = slice_syllables(code, offsets);
                            existing.syl_mask = offsets_mask(offsets);
                            existing.canon = Some((variant.clone(), canon_mask));
                        }
                        continue;
                    }
                    nodes[q].push(LatticeNode {
                        start: p,
                        end: q,
                        word: hit.text,
                        syllables: slice_syllables(code, offsets),
                        syl_mask: offsets_mask(offsets),
                        log_prob,
                        canon: Some((variant.clone(), canon_mask)),
                    });
                }
            }
        }

        nodes
    }

    /// 在已建好的词图上**追加简拼节点**，供混合整句解码（`bzd` + `haobuhao` → 不知道好不好）。
    ///
    /// 独立枚举跨度（任意 `(p, q)`，`q - p ∈ [2, MAX_ABBREV_SPAN]`）：简拼段在音节图上不可达、
    /// 也没有终点。节点按字节跨度与全拼节点天然可串，Viterbi 无需改动。
    ///
    /// 音节标注在击键空间：每个字母一个音节位，整句回填出 `b'z'd'hao'bu'hao`（见 `mixed_abbrev`
    /// 模块文档）。
    ///
    /// `input` 必须是原始击键串；双拼下须跳过本方法（`pinyin-mixed-abbrev.md` §5 约束 4）。
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
                        // 音节数必须等于简拼字母数：扁平码有损，`xa` → `xian` 会捞出 1 音节的「先」。
                        // 与用户侧同一个切分函数，畸形边界切不出来即跳过（整句节点要真值切分）。
                        if crate::pinyin::mixed_abbrev::syllables_from_boundary(
                            &abbr_code,
                            hit.boundary,
                        )
                        .map(|s| s.len())
                            != Some(span)
                        {
                            continue;
                        }
                        if nodes[q].iter().any(|n| n.word == hit.text && n.start == p) {
                            continue;
                        }
                        let log_prob = score_node(&hit.text, &abbr_code, hit.weight)
                            - ABBREV_NODE_PENALTY * span as f64;
                        nodes[q].push(LatticeNode {
                            start: p,
                            end: q,
                            word: hit.text,
                            // 击键空间：每个字母一个音节位
                            syllables: stroke.chars().map(|c| c.to_string()).collect(),
                            syl_mask: (0..span).fold(0u64, |m, i| m | (1u64 << i)),
                            log_prob,
                            // 回查出的全拼与真值边界（必非 0，上面已校验），见 `LatticeNode::canon`。
                            canon: Some((abbr_code.clone(), hit.boundary)),
                        });
                    }
                }
            }
        }
    }

    /// 在已建好的词图上**追加用户词节点**（S2），让自造词参与整句解码。
    ///
    /// 系统词库与用户层本不相交，不补这一步自造词就不参与整句分词。设计见
    /// engine-candidate-pipeline.md「S2：用户词进整句词图」。三条约束：
    ///
    /// 1. 只收用户上屏过的词（用户词 + 临时词，[`is_sentence_store_word`]），草稿不收。临时层
    ///    也含自动造词直接写入的分步上屏拼接，分错段的一次上屏会带 [`USER_NODE_BONUS`] 自我强化。
    /// 2. `boundary == 0` 不进图（与 [`Self::build`] 的降级放行相反）：整句节点要真值切分，
    ///    宁可不进，不可乱切。
    /// 3. 同词同起点取 `log_prob` 较大者而非跳过，否则用户调高系统已有词的权重在整句里无效。
    ///
    /// 权重两头都管：[`USER_NODE_BONUS`] 补下限，[`USER_NODE_WEIGHT_CAP`] 截上限。
    ///
    /// 模糊音另走 [`Self::add_store_fuzzy_nodes`]（记录存规范码，按所打码点查撞不上）。
    ///
    /// 由 `Config::sentence_uses_user_words` 把关（出厂开），且只在 `use_smart_compose` 开着时调用。
    ///
    /// `require_reachable` 与同一张图的 [`Self::build`] 传同值：不可达起点上的节点 Viterbi
    /// 永远走不到（`build` 在那里一个节点都没建，本方法的节点又只沿图边延伸），跳过它们
    /// 不改变解码结果，只省点查。
    #[allow(clippy::too_many_arguments)]
    pub fn add_store_nodes(
        &self,
        input: &str,
        graph: &SegGraph,
        store: &DictManager,
        fuzzy_config: Option<&FuzzyConfig>,
        require_reachable: bool,
        nodes: &mut [Vec<LatticeNode>],
    ) {
        let fuzzy = fuzzy_config.filter(|f| f.any_enabled());
        // 各音节边的变体表：同一条边会被许多起点、许多前缀组合反复展开，算一次即可。
        let mut edge_variants = EdgeVariants::new();
        let input_len = input.len();
        for p in 0..input_len.min(graph.len()) {
            if require_reachable && !graph.is_reachable(p) {
                continue;
            }
            for q in graph.ends_within(p, self.max_word_len) {
                if q > input_len || q >= nodes.len() {
                    continue;
                }
                let code = &input[p..q];
                for cand in store.search(code, USER_NODE_LIMIT) {
                    // 约束 1：只认用户上屏过的词。
                    if !is_sentence_store_word(&cand) {
                        continue;
                    }
                    // 约束 2：必须有真值切分，且该切分是本跨度上的一条合法路径。
                    let offsets = match graph.mask_path(p, q, cand.boundary) {
                        MaskCheck::Path(syl_count) => {
                            if syl_count > self.max_word_len {
                                continue;
                            }
                            mask_offsets(cand.boundary, q - p)
                        }
                        // 与 build 的降级放行相反：整句节点不接受无边界的词。
                        MaskCheck::NoInfo | MaskCheck::Reject => continue,
                    };
                    let weight = cand.weight.min(USER_NODE_WEIGHT_CAP);
                    let log_prob = score_node(&cand.text, code, weight)
                        - AMBIGUOUS_PENALTY * graph.ambiguous_count(p, q, &offsets) as f64
                        + USER_NODE_BONUS;
                    // 约束 3：同词同起点取更优的那个分，而不是先到先得。赢了连切分一起换：
                    // 被替换的切分同样是图上的合法路径，新切分与本条用户记录逐音节对应。
                    if let Some(existing) = nodes[q]
                        .iter_mut()
                        .find(|n| n.word == cand.text && n.start == p)
                    {
                        if log_prob > existing.log_prob {
                            existing.log_prob = log_prob;
                            existing.syllables = slice_syllables(code, &offsets);
                            existing.syl_mask = offsets_mask(&offsets);
                            existing.canon = None;
                        }
                        continue;
                    }
                    nodes[q].push(LatticeNode {
                        start: p,
                        end: q,
                        word: cand.text,
                        syllables: slice_syllables(code, &offsets),
                        syl_mask: offsets_mask(&offsets),
                        log_prob,
                        canon: None,
                    });
                }
            }
            if let Some(fuzzy) = fuzzy {
                self.add_store_fuzzy_nodes(
                    p,
                    input,
                    graph,
                    store,
                    fuzzy,
                    &mut edge_variants,
                    nodes,
                );
            }
        }
    }

    /// 从起点 `p` 出发、终点不超过 `end_limit`、至多 `max_syllables` 个音节的全部模糊变体码，
    /// 按（终点, 变体码）归并，各附到达它的全部形态（变体边界、所打码切分、改动处数）。
    /// 系统词（[`Self::build`]）与用户词（[`Self::add_store_fuzzy_nodes`]）的模糊节点、词候选的
    /// 模糊变体码（`PinyinEngine::fuzzy_codes`）共用这一份。
    ///
    /// 逐边展开而不取一条切分：`pinan` 的平安只在 `pin|an` 上。沿音节边逐层延伸、就地展开
    /// 变体（[`FuzzyMatcher::fuzzy_variants_scored`]），不先枚举路径再展开：按条数截断没有语义，
    /// 会截掉唯一对得上的切分。同层超出 [`FUZZY_LAYER_BUDGET`] 时按改动处数截。
    ///
    /// 全原音节组合（= 所打码本身）不收：那是精确命中，由各自的精确分支处理。
    ///
    /// `can_extend(变体码)` 为假的组合不再往后延伸（没有任何更长的码以它开头）。剪枝发生在
    /// 本层预算截断**之后**、延伸下一层之前：被剪掉的组合仍占过本层预算（它自己还要点查），
    /// 省下的是它的后代。系统词传词典的前缀存在性判据，用户层没有这一查询，传恒真。
    ///
    /// ## 成本
    ///
    /// 变体码按（终点, 码）归并（`xi|an` 与 `xian` 都能展开出 `xiang`），调用方每个只点查一次；
    /// 上界 = 层数(≤ `max_syllables`) × [`FUZZY_LAYER_BUDGET`]，扣掉全原音节的组合。词图传
    /// `max_word_len`（更长的词本就不进图）；词候选查的是整码，传码长当上界（层数实际止于
    /// 最长切分的音节数，图上走不动了即停）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fuzzy_spans(
        &self,
        p: usize,
        input: &str,
        graph: &SegGraph,
        fuzzy: &FuzzyConfig,
        edge_variants: &mut EdgeVariants,
        end_limit: usize,
        max_syllables: usize,
        can_extend: &dyn Fn(&str) -> bool,
    ) -> BTreeMap<(usize, String), Vec<FuzzyShape>> {
        /// 在途组合：(当前位置, 变体码, 变体边界, 所打码切分（相对 p 的起点偏移）, 改动处数)
        type Combo = (usize, String, u64, Vec<usize>, usize);
        // BTreeMap：点查与建节点的顺序确定，节点次序不随哈希漂移。
        let mut spans: BTreeMap<(usize, String), Vec<FuzzyShape>> = BTreeMap::new();
        let mut layer: Vec<Combo> = vec![(p, String::new(), 0, Vec::new(), 0)];
        for _ in 0..max_syllables {
            let mut next: Vec<Combo> = Vec::new();
            for (pos, variant, mask, offsets, edits) in &layer {
                if !variant.is_empty() && !can_extend(variant) {
                    continue;
                }
                let bit = if variant.len() < 64 {
                    1u64 << variant.len()
                } else {
                    0
                };
                let mut offs = offsets.clone();
                offs.push(pos - p);
                for &e in graph.edges_from(*pos) {
                    if e > end_limit {
                        continue;
                    }
                    let opts = edge_variants.entry((*pos, e)).or_insert_with(|| {
                        let syl = &input[*pos..e];
                        // 首项恒为原音节（0 处改动）。
                        let mut v = vec![(syl.to_string(), 0)];
                        v.extend(FuzzyMatcher::fuzzy_variants_scored(syl, fuzzy));
                        v
                    });
                    for (opt, opt_edits) in opts.iter() {
                        next.push((
                            e,
                            format!("{variant}{opt}"),
                            mask | bit,
                            offs.clone(),
                            edits + opt_edits,
                        ));
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            if next.len() > FUZZY_LAYER_BUDGET {
                // 稳定排序后截断：结果确定，但不是「每层全局改动最少」的精确保证——同改动数内
                // 按生成顺序截，且只在本层比较。模糊组全开的长串上会实际触发。
                next.sort_by_key(|c| c.4);
                next.truncate(FUZZY_LAYER_BUDGET);
            }
            for (q, variant, mask, offsets, edits) in &next {
                // 全原音节组合 = 所打码本身，已由精确分支处理（且带真值边界校验）。
                // 按码比而不按处数：双拼同键音节计 0 处，却不是所打码。
                if variant != &input[p..*q] {
                    spans.entry((*q, variant.clone())).or_default().push((
                        *mask,
                        offsets.clone(),
                        *edits,
                    ));
                }
            }
            layer = next;
        }
        spans
    }

    /// [`Self::add_store_nodes`] 的模糊分支：从起点 `p` 出发的全部用户词模糊节点。
    ///
    /// 变体码与切分来自 [`Self::fuzzy_spans`]（与系统词模糊节点同一份）。命中记录的 `boundary`
    /// 必须**恰好等于**某个形态的变体边界——于是记录的每个音节都一一对应到所打码的一个音节上。
    /// 节点的 `syllables` / `syl_mask` 用那条**所打码**切分，与精确节点同域（下游逐步上屏 /
    /// consumed 都按所打码走）。
    ///
    /// 比 [`Self::build`] 的系统词模糊节点**更严**：那边 `boundary == 0` 降级放行；
    /// 这边沿用精确分支的约束 2（整句节点要真值切分，`boundary == 0` 天然对不上任何变体边界）。
    ///
    /// ## 打分
    ///
    /// 与 [`Self::build`] 的系统词模糊节点同口径：`score_node(变体码)` − 每处改动
    /// [`FUZZY_SYLLABLE_LOG_PENALTY`] − 歧义罚，再叠 [`USER_NODE_WEIGHT_CAP`] / [`USER_NODE_BONUS`]。
    /// 同词同起点仍取较大者（约束 3）。精确优先由罚分保证：同权重时模糊那条恒多扣
    /// ln2 × 改动处数，压不过所打码的精确命中。同一个词两份记录（精确码、规范码各一）时
    /// 出哪个词不变，但**分高者决定整句按哪个码造词**：替换时 `canon` / 切分随胜者一起换
    /// （精确胜者清成 `None`、模糊胜者写记录自己的码与边界），整句的 `learn_code` 由此拼出。
    #[allow(clippy::too_many_arguments)]
    fn add_store_fuzzy_nodes(
        &self,
        p: usize,
        input: &str,
        graph: &SegGraph,
        store: &DictManager,
        fuzzy: &FuzzyConfig,
        edge_variants: &mut EdgeVariants,
        nodes: &mut [Vec<LatticeNode>],
    ) {
        let end_limit = input.len().min(nodes.len().saturating_sub(1));
        let spans = self.fuzzy_spans(
            p,
            input,
            graph,
            fuzzy,
            edge_variants,
            end_limit,
            self.max_word_len,
            &|_| true,
        );

        for ((q, variant), shapes) in &spans {
            let q = *q;
            let code = &input[p..q];
            for cand in store.search(variant, USER_NODE_LIMIT) {
                if !is_sentence_store_word(&cand) {
                    continue;
                }
                // 边界逐音节对上的那条切分；多条都对上时取改动最少的。
                let Some((_, offsets, edits)) = shapes
                    .iter()
                    .filter(|(mask, _, _)| *mask == cand.boundary)
                    .min_by_key(|(_, _, edits)| *edits)
                else {
                    continue;
                };
                let weight = cand.weight.min(USER_NODE_WEIGHT_CAP);
                let log_prob = score_node(&cand.text, variant, weight)
                    - FUZZY_SYLLABLE_LOG_PENALTY * *edits as f64
                    - AMBIGUOUS_PENALTY * graph.ambiguous_count(p, q, offsets) as f64
                    + USER_NODE_BONUS;
                // 约束 3。赢了连切分一起换：被替换的切分同样是图上的合法路径，
                // 新切分与本条用户记录逐音节对应。
                if let Some(existing) = nodes[q]
                    .iter_mut()
                    .find(|n| n.word == cand.text && n.start == p)
                {
                    if log_prob > existing.log_prob {
                        existing.log_prob = log_prob;
                        existing.syllables = slice_syllables(code, offsets);
                        existing.syl_mask = offsets_mask(offsets);
                        existing.canon = Some((variant.clone(), cand.boundary));
                    }
                    continue;
                }
                nodes[q].push(LatticeNode {
                    start: p,
                    end: q,
                    word: cand.text,
                    syllables: slice_syllables(code, offsets),
                    syl_mask: offsets_mask(offsets),
                    log_prob,
                    // 记录自己的码与边界：边界已逐音节验过等于变体边界。
                    canon: Some((variant.clone(), cand.boundary)),
                });
            }
        }
    }

    /// 在简拼整句（step ②b）的词图上**追加用户词的简拼节点**：[`Self::add_abbrev_nodes`]
    /// 的用户层版本。
    ///
    /// [`Self::add_store_nodes`] 按 `&input[p..q]` 点查全拼码，在简拼段上（`bcx` 是声母串）
    /// 必然落空；系统词在这里走的是 wdat 的简拼索引，用户层一直缺这一半 ⇒ 打 `bcxrmzf`
    /// 时用户造的「拜城县」进不了整句，哪怕已经晋升。
    ///
    /// 约束与两个兄弟方法对齐：
    /// - 词源同 [`Self::add_store_nodes`] 约束 1（[`is_sentence_store_word`]）；
    /// - **音节数 = 简拼字母数**，且**逐段首字母 = 对应字母**：层接口明说返回超集，判据由
    ///   调用方做；`boundary == 0` 的词在 `syllables_from_boundary` 这一步被拒——与
    ///   [`Self::add_store_nodes`] 约束 2 同理；
    /// - 取数用 `search_abbrev_exact`（只查本组，上限 [`USER_ABBREV_SCAN_LIMIT`]），过完判据
    ///   再按权重取前 [`USER_NODE_LIMIT`]，免得名额被无边界词或字典序占满；
    /// - 打分 = 简拼节点的罚分（`ABBREV_NODE_PENALTY × 字母数`）+ 用户词的截顶与加成，
    ///   与同位置的系统简拼节点公平竞争；
    /// - 同词同起点取 `log_prob` 较大者，切分与 `canon` 随胜者换（同 [`Self::add_store_nodes`]
    ///   约束 3）。
    ///
    /// zh/ch/sh 按一个字母计（`z`），与 `add_abbrev_nodes` 相同——整句简拼域目前就是
    /// 「一个音节 = 一个字母」。
    pub fn add_store_abbrev_nodes(
        &self,
        input: &str,
        store: &DictManager,
        nodes: &mut [Vec<LatticeNode>],
    ) {
        let input_len = input.len();
        let bytes = input.as_bytes();
        for p in 0..input_len {
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
                // 先过判据、再按权重取前 N：截断若在判据之前做（带上限查询），名额会被必然
                // 拒收的无边界词占满、或按索引键字典序切掉高权重词（见 `search_abbrev_exact`）。
                let mut hits: Vec<_> = store
                    .search_abbrev_exact(stroke, USER_ABBREV_SCAN_LIMIT)
                    .into_iter()
                    .filter(|cand| {
                        is_sentence_store_word(cand)
                            && crate::pinyin::mixed_abbrev::syllables_from_boundary(
                                &cand.code,
                                cand.boundary,
                            )
                            .is_some_and(|syls| {
                                syls.len() == span
                                    && syls
                                        .iter()
                                        .zip(stroke.bytes())
                                        .all(|(syl, b)| syl.as_bytes().first() == Some(&b))
                            })
                    })
                    .collect();
                hits.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.text.cmp(&b.text)));
                hits.truncate(USER_NODE_LIMIT);
                for cand in hits {
                    let weight = cand.weight.min(USER_NODE_WEIGHT_CAP);
                    let log_prob = score_node(&cand.text, &cand.code, weight)
                        - ABBREV_NODE_PENALTY * span as f64
                        + USER_NODE_BONUS;
                    // 同词同起点取较大者，赢了切分与 `canon` 随胜者一起换（同两个兄弟方法）。
                    //
                    // 起止字节不变，consumed 与上屏不受影响；切分换成胜者的逐字母切分，整句回填的
                    // 边界本就该跟胜出的那条解释走。
                    if let Some(existing) = nodes[q]
                        .iter_mut()
                        .find(|n| n.word == cand.text && n.start == p)
                    {
                        if log_prob > existing.log_prob {
                            existing.log_prob = log_prob;
                            existing.syllables = stroke.chars().map(|c| c.to_string()).collect();
                            existing.syl_mask = (0..span).fold(0u64, |m, i| m | (1u64 << i));
                            existing.canon = Some((cand.code.clone(), cand.boundary));
                        }
                        continue;
                    }
                    nodes[q].push(LatticeNode {
                        start: p,
                        end: q,
                        word: cand.text,
                        // 击键空间：每个字母一个音节位（同 add_abbrev_nodes）
                        syllables: stroke.chars().map(|c| c.to_string()).collect(),
                        syl_mask: (0..span).fold(0u64, |m, i| m | (1u64 << i)),
                        log_prob,
                        // 记录自己的码与边界（`syllables_from_boundary` 已保证边界非 0）。
                        canon: Some((cand.code.clone(), cand.boundary)),
                    });
                }
            }
        }
    }

    /// 在已建好的词图上**追加尾部残码节点**，供残码补全整句解码
    /// （`buzhidaok` → 「不知道」+ 残码 `k` 补成「看」→ 整句「不知道看」）。
    ///
    /// 残码作为一个待定音节进格子（对齐 librime `enable_completion`、fcitx5「不完整拼音」）。
    /// 不能让 [`Self::add_abbrev_nodes`] 兼职：简拼节点会把已完成音节也重切（`buzhidaok` →
    /// 「不直达欧卡」），残码补全只展开最后那一段，见 candidate-sorting-rules.md §4.2。
    ///
    /// 候选 = 以残码为前缀、音节数为 1 的单字；跨度恒为 `[completed_len, input_len)`，记一个音节位。
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
        // 对齐位传 0：残码相对自己没有已完成音节，边界判据整条让开、保持纯字符前缀匹配，
        // 这是 `prefix_syllable_aligned` 为残码场景留的退化通道。
        for hit in dict.search_prefix_with_boundary_syllable_capped(
            partial,
            PARTIAL_FINAL_NODE_LIMIT,
            1,
            0,
        ) {
            // 只收单字：单音节多字词（儿化）会让一个待定音节兑出两个字。
            if hit.text.chars().count() != 1 {
                continue;
            }
            if nodes[input_len]
                .iter()
                .any(|n| n.word == hit.text && n.start == completed_len)
            {
                continue;
            }
            // 残码位不给虚词优待，见 `score_node_partial_final`。
            let log_prob = score_node_partial_final(&hit.text, hit.weight) - PARTIAL_FINAL_PENALTY;
            nodes[input_len].push(LatticeNode {
                start: completed_len,
                end: input_len,
                word: hit.text,
                syllables: vec![partial.to_string()],
                syl_mask: 1,
                log_prob,
                // 残码位不填：见 `LatticeNode::canon`，残码整句的造词口径不在本字段范围。
                canon: None,
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

#[cfg(test)]
#[path = "lattice_coda_steal_tests.rs"]
mod coda_steal_tests;
