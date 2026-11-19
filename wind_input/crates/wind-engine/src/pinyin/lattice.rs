//! 格子构建（Lattice）+ 多切分评分
//!
//! 与 Go 版本 `wind_input/internal/engine/pinyin/lattice.go` 对齐。
//! 构建词图并支持多路径评分，用于 Viterbi 解码。

use crate::pinyin::dag::{MaskCheck, SegGraph};
use crate::pinyin::fuzzy::{FuzzyConfig, FuzzyMatcher};
use crate::pinyin::syllable::SyllableTrie;
use std::collections::{BTreeMap, HashMap};
use wind_dict::cached::CachedDict;
use wind_dict::manager::DictManager;

/// 虚词表：**字形 → 该字作虚词时的读音**（无调全拼）。
///
/// ## ★★★ 判据必须同时问「哪个字」和「哪个读音」
///
/// 虚词优待合计 **8.0** 的量级差（`FUNCTION_WORD_BONUS` +2.0、实词
/// `SINGLE_CHAR_PENALTY` −3.0、再豁免 `WORD_PENALTY` 3.0），相当于 e^8 ≈ **2981 倍词频**。
/// 只按字形发放时，多音字的**冷僻读音**会白拿这份优待，把该音位上真正的高频字整片压掉：
///
/// ```text
/// kuai:  会(w=334)   +2.0 = −11.49   >   快(w=29689)  −6.0 = −15.01   ⇒ 「快吃饭」出成「会吃饭」
/// zhao:  着(w=263)          −11.72   >   找(w=54117)        −14.40
/// huan:  还(w=12366)         −7.88   >   换(w=18772)        −15.47
/// liao:  了(w=51)           −13.37   >   聊(w=8535)         −16.26
/// ```
///
/// 这与 `score_node_inner` 长注释里批评 `gen_unigram`「按词去重取 max、多音字冷僻读音被按
/// 常见读音计价」是**同型缺陷**，只是发生在加成层而非基础分层，故那次修复没覆盖到。
///
/// ## 取值口径
///
/// 只登记该字**作虚词/代词/助动词时**的读音；同字的实词读音一律不登记（会 kuai 会计、
/// 都 du 首都、还 huan 归还、着 zhao/zhuo、和 huo/hu、没 mo 沉没、了 liao 了解、
/// 地 di 土地、说 shui 游说）。码是**无调全拼**，故同码不同调无法区分——`得` 的 `de`
/// （结构助词）与 `dé`（得到）共码，只能一并放行，这是无调码的固有上界，不是疏漏。
///
/// 单读音的字（我/你/是/不…）登记其唯一读音即可，判据对它们等价于旧的「只问字形」。
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

/// 混合整句里把 `zh` / `ch` / `sh` **拆成两个声母**（z|h）时，每处拆分的扣分（对数域）。
///
/// 用户打 `zh` 绝大多数时候要的是翘舌声母（`zhge` = 这个），而简拼节点按「一个字母一个
/// 声母」读，会把它读成 z|h：「之后」(zhi|hou) 的简拼恰好是 `zh`，于是 `zhge` 组出
/// 「之后个」、`shzhe` 组出「生活这」、`zhrgan` 组出「之后如果按钮」。拆开读并非不合法
/// （「最后」的简拼就是 `zh`），故是**罚**不是禁。
///
/// 取值由 `pinyin_eval` 界面序扫出（2026-09-24，seed 20260721；A/B/C 不受影响，只有 ②b 用）：
///
/// | 惩罚 | E 短简拼 top-1 | 其中 R 全声母 | D 混合长串 top-1 |
/// |---|---|---|---|
/// | 0 | 64.40% | 27.27% | 12.20% |
/// | 2 | 68.20% | 52.27% | 12.20% |
/// | 4 | 70.60% | 70.45% | 12.10% |
/// | **8** | **74.40%** | **90.91%** | **12.00%** |
/// | 50（≈禁止） | 75.50% | 90.91% | 11.40% |
///
/// 8 处：E 类新增 100 条、丢 0 条；D 类丢的 2 条正是用户真把 s|h、z|h 当两个声母打的
/// （「可视化」`ksh`、「相互帮助」`xhbz` + 「黑武士」）。再往上 E 只多 11 条、D 却丢 8 条。
pub(crate) const RETROFLEX_SPLIT_PENALTY: f64 = 8.0;

/// 混合整句里简拼段**抢走前一全拼音节的韵尾**时的扣分（对数域）。
///
/// 用户打 `ningbr`（宁波人）时 `ning` 是一个完整音节，而简拼节点可以从它中间的 `n` 起头，把
/// 串读成 ni + n|g + b|r，组出「你能够比如」；同类还有 `zhentp` →「这钕铁硼」、`banld` →
/// 「把年龄段」。用户把一个音节打全了，就几乎不会是要把它的韵尾拆给下一个词当声母。
/// 判据见 [`steals_coda`]；与 [`RETROFLEX_SPLIT_PENALTY`] 同理，是罚不是禁。
///
/// 取值由 `pinyin_eval` 界面序扫出（2026-09-24，seed 20260721，已含翘舌拆分惩罚）：
///
/// | 惩罚 | E 短简拼 top-1 | 其中 F 首音节+声母 | D 混合长串 top-1 |
/// |---|---|---|---|
/// | 0 | 74.40% | 72.24% | 12.00% |
/// | 2 | 75.20% | 73.43% | 12.00% |
/// | 4 | 75.70% | 74.41% | 12.00% |
/// | **8** | **76.20%** | **75.20%** | **12.00%** |
/// | 16 / 50 | 76.20% | 75.20% | 12.00% |
///
/// 8 处 E 类新增 18、丢 0，此后不再变化；A~D 全程逐位不变。E 类剩余的 F 形态失败以同音 /
/// 同简拼竞争为主（就你能 / 就你那），另有零声母音节的切分歧义（`xianzw` → 西安作为），
/// 那是全拼音节图本身的问题，不归本项管。
pub(crate) const CODA_STEAL_PENALTY: f64 = 8.0;

/// 单个简拼跨度最多取几个词进图。
///
/// 简拼召回面宽（`bzd` 真实词库下 12 个词），全塞进去会让节点数与 Viterbi 的边数一起膨胀，
/// 而排在后面的低频词几乎不可能赢下整句路径。按权重取前 N 即可。
const ABBREV_NODE_LIMIT: usize = 8;

/// 单个跨度上最多取几条用户词入图（[`LatticeBuilder::add_store_nodes`]）。
///
/// 同一个 code 下的用户词通常只有一两条（用户不会给同一串拼音造十个词），8 是宽松上界；
/// 它同时是成本闸门：整句建图要遍历全部 (p, q) 跨度，每个跨度一次 redb 点查。
pub(crate) const USER_NODE_LIMIT: usize = 8;

/// 用户层简拼节点每个跨度**取数**的上限（过判据、按权重取前 [`USER_NODE_LIMIT`] 之前）。
///
/// 取数用 `search_abbrev_exact`（只查本组，无边界词不在其中）。存储层按索引键字典序数够
/// 上限就返回，**不按权重**——所以上限太小会在判据前切掉高权重词（曾是 8：同组 10 个词时
/// 「拜城县」就被切掉），太大又让按键路径随用户库规模线性变慢。实测（每个被查的两/三字母组
/// 塞 2000 个用户词、`bcxrmzf` 一次转换，关 → 开）：不设上限 +60~90ms，64 +1.7ms，32 +1.1ms。
///
/// ⚠️ 已知限制：同一声母串的用户词超过本值时，超出部分按码的字典序被截，与权重无关。
/// 要彻底解决需在索引里按权重排序，本项不做。值与引擎侧 `ABBREV_INDEX_LIMIT` 同为 64。
const USER_ABBREV_SCAN_LIMIT: usize = 64;

/// 模糊入图（系统词与用户词共用）时，从一个起点出发、**同一音节数**上最多保留几条在途的
/// （所打码切分 × 逐音节变体）组合（[`LatticeBuilder::fuzzy_spans`]）。
///
/// 超出时按改动处数升序保留，截掉的只是模糊处数多的组合（及其后继——后继的处数只增不减）。
/// 每条组合在它到达的终点上各是一次候选点查（按变体码归并后），故这也是成本闸门。
///
/// 取 256 = 旧口径「一条切分 64 种组合 × 4 条切分」：
/// 不再按切分截断之后，多切分的串（`fanganfangan…` 同音节数下有 8 条切分）要的是
/// 「每条切分都至少保住少处模糊的那些组合」，按处数排序的同层预算正好表达这一点；
/// 量级与旧上界持平，性能见 `pinyin_sentence_user_words_probe::fuzzy_store_node_cost_on_real_dict`。
pub(crate) const FUZZY_LAYER_BUDGET: usize = 256;

/// 各音节边 `(起, 止)` 的变体表（首项恒为原音节、0 处改动）：同一条边会被许多起点、
/// 许多前缀组合反复展开，算一次即可。
pub(crate) type EdgeVariants = HashMap<(usize, usize), Vec<(String, usize)>>;

/// 模糊变体到达某终点的一种形态：(变体边界, 所打码切分（相对起点的偏移）, 改动处数)。
pub(crate) type FuzzyShape = (u64, Vec<usize>, usize);

/// 用户词入图时的**对数域加成**（[`LatticeBuilder::add_store_nodes`]）。
///
/// ## 为什么需要它
///
/// 用户词的 weight 与词频同轴，但两者**语义不同**：词频说「这个词有多常见」，用户词说
/// 「我要这个词」。同轴比大小的后果实测如下 —— 「盖伦」在 cn_dicts 里本就有（w=237），
/// 同码的「概论」w=1217，而手动加词的出厂权重 `ADD_WORD_WEIGHT = 1200`
/// （`handle_addword.rs:38`）**恰好低 17 分**：用户按设计流程走完（造词 + 开开关），
/// 整句**仍然不认**，要手工把权重调到 1300 才翻得过来。那等于这个功能对默认路径上的
/// 用户不存在。
///
/// ## 为什么加在这里，而不是抬 `ADD_WORD_WEIGHT`
///
/// 抬那个常量会连带改变**候选层**排序（它在那边的语义是「略高于系统库归一化中位 1000」），
/// 而候选层本来就工作正常 —— 为整句的需要去动候选层的标定，是拿一个正确的东西去补
/// 另一个。整句与候选本就是两套打分（前者在对数域按路径求和、后者在线性域比 weight），
/// 「用户明确造过的词在**组句时**该被优先考虑」是独立于词频的判断，就地表达即可。
///
/// ## 为什么是 +2.0，且为什么是加而不是乘
///
/// 量纲对照同文件的其余调整项（`FUNCTION_WORD_BONUS` +2.0、`SINGLE_CHAR_PENALTY` −3.0、
/// `WORD_PENALTY` 3.0），+2.0 等价于 `weight × e² ≈ ×7.4`：出厂加词的 1200 等效到 ~8900，
/// 从「压过 96.3% 的系统词」抬到 **99.5%** —— 翻得过绝大多数同码竞争者，又仍然输给真正的
/// 高频词（「这个」w=555006 ⇒ −6.08，加成后的用户词是 −10.22）。用户词不该碾压高频词，
/// 只该在势均力敌时胜出。
///
/// **加在 `score_node` 之后而不是乘在 weight 上**：`score_node_inner` 里多字词的
/// `BASE_CONTENT_WORD_BONUS` 用 `freq_factor`（由 log_prob 推出）做缩放，放大 weight 会
/// 连带把那份加成也放大一次，等于同一个意图被计了两遍、且幅度不可控。
pub(crate) const USER_NODE_BONUS: f64 = 2.0;

/// 用户词入图时的 weight 上限（[`LatticeBuilder::add_store_nodes`]）。
///
/// 用户词与词典 weight 同轴，默认档位本就落在合理位置（手动加词 1200 压过 96.3% 的系统词、
/// 造词晋升 800 压过 94.7%），故不做分布重映射，**只防极端值**：导入的 wdict 可以带任意
/// 权重，`w = 2e9` 时 `ln(w/DICT_TOTAL) = +2.11`，而系统最大词（15,378,475）才 −2.76
/// —— 差近 5 个自然对数单位（≈148 倍），足以让整句变成用户词拼接秀。
///
/// 取 1e6（`ln = −5.49`，约 p99.99+）：够强 —— 几乎压过所有系统词，又仍在词频轴内侧，
/// 不会跨到对数正值那一段去碾压一切。
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
pub(crate) const PARTIAL_FINAL_NODE_LIMIT: usize = 12;

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
/// unigram）。`code` 是**取到这条词条所用的词典码**，供单字虚词判据核对读音——不是用户
/// 原始击键，两者在模糊/简拼路径上并不相同，见 [`is_function_word`]。
///
/// ⚠️ 因此本函数要求 `code` 落在**无调全拼**域。当前三个 `LatticeBuilder::build` 调用点
/// 传的都是全拼（双拼在进 lattice 前已转换），`add_abbrev_nodes` 传的是回查出的全码。
/// 哪天放开闸门让**双拼击键**直接进词图，`code` 会变成 `ui` 这类击键串，全表虚词一次性
/// 掉权 8.0 nat —— 那是静默的整体劣化，不会有任何编译错误。
pub(crate) fn score_node(word: &str, code: &str, weight: i32) -> f64 {
    score_node_inner(word, code, weight, true)
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

    // 虚词优待是**一件事的两半**（加成 + 豁免每词罚），此前分两处各算一次判据，
    // 改判据时漏改一处就会退化成「加了成还照罚」这类半开状态。绑一次，两处共用。
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
            // 原判据是 `ug.contains(word)`（词是否在 unigram 表内）。二者等价：
            // `gen_unigram` 只过滤 `freq<=0` 与纯 ASCII 词 ⇒ 凡 weight>0 的中文词都在表内。
            let freq_factor = ((log_prob - LOG_PROB_MIN) / LOG_PROB_RANGE).clamp(0.0, 1.0);
            log_prob += BASE_CONTENT_WORD_BONUS * (char_count as f64).sqrt() * freq_factor;
        }
    }
    // Phase 4：每词固定罚。Viterbi 的路径分是各节点 log_prob 之和，故「每节点减 W」
    // 等价于「按路径词数罚 k·W」——把低频词打碎成两个高频片段不再免费。
    //
    // **虚词（是/的/了…）豁免每词罚**：WORD_PENALTY 意在阻止「把低频词打碎成高频
    // 片段」的投机拆分，而单字虚词随内容词出现是语法黏着、不是碎片。unigram 的独立性
    // 假设对 P(内容词)·P(虚词) 双重扣了 ln(total)（每词一份），一个低频 3 字整词
    // （填鸭式 w=152）便能压过「天涯+是」这种 2 词正解——这正是 bigram P(是|天涯)
    // 该解决而 unigram 解决不了的（缺磁盘语料，尚无 bigram）。豁免虚词
    // 的每词罚是对该缺陷的近似补偿：不让「虚词自成一词」这件语法必然的事付投机拆分的代价。
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
        // 10 而非 6：6 会把「中华人民共和国」(7 音节) 挡在词图外，却放行它的语义碎片
        // 「中华人民共和」(freq=2，法律条文名切出来的残片)，于是 Viterbi 只能在
        // 「中华人民共和」+「过」之类的错误切分里挑最优。上限须覆盖常见长专名。
        //
        // 超过上限的词典整词（「冠状动脉粥样硬化性心脏病」12 音节）仍进不了词图，但不需要
        // 额外兜底：整句改用等效词频后，错误拼接整句的 W_eff 是一串低频字的乘积、趋近
        // clamp 下限，词典整词的真实词频天然压过它。守门测试见 `tests/pinyin_long_word.rs`。
        Self { max_word_len: 10 }
    }

    /// 最大词长（音节数）。
    pub(crate) fn max_word_len(&self) -> usize {
        self.max_word_len
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
            // **词条边界不能拿去比所打码的偏移**：词典返回的 boundary 是**变体码**空间的偏移
            // （zhongguo 的 {0,5}），而本跨度在用户**原码**空间（zong|guo 的 {0,4}）。z→zh
            // 这类变体改变码长，两者位偏移不同域，直接比对会把正确的模糊命中整片误杀
            // （mod.rs 对模糊命中对外一律置 boundary=0 也是这个理由）。
            //
            // 它只和**同域的变体边界**比。变体按图上**每条**切分逐音节展开（[`Self::fuzzy_spans`]）：
            // `pinan` 的平安只在 `pin|an` 上，只取一条切分会整片漏掉。词条有边界时必须等于某个
            // 形态的变体边界（`xi|an → xi|ang` 拼出的 `xiang` 撞上单音节的「想」就该拒），并取那个
            // 形态的所打码切分；无边界时放行、取改动最少的形态。
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
                        // 切分走与用户侧（`add_store_abbrev_nodes`）同一个函数：boundary==0 与
                        // bit0 未置位的畸形边界切不出来，直接跳过——混合整句的每个节点都要求真值
                        // 切分。（越出码长的高位被该函数忽略、不计音节，与 `count_ones` 不同。）
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
                            // 回查出的词条码与它的真值边界：整句造词写完整全拼，而不是击键
                            // 字母。见 `LatticeNode::canon`。边界必非 0——上面已经过
                            // `syllables_from_boundary` 校验且音节数 = 字母数（≥ 2）。
                            canon: Some((abbr_code.clone(), hit.boundary)),
                        });
                    }
                }
            }
        }
    }

    /// 在已建好的词图上**追加用户词节点**（S2），让自造词参与整句解码。
    ///
    /// ## 解决什么
    ///
    /// 整句词图此前只从 [`CachedDict`]（系统词库）建，用户层挂在 `PinyinEngine` 的另一个
    /// 字段上、两者从不相交 ⇒ 用户自造的词**根本没参与整句分词**：「盖伦」单独打得出，
    /// 「有盖伦吗」却被打散（t134）；手动调过权重的词在整句里同样不认（GH#93）。
    ///
    /// ## 三条约束（都不是「防御性编程」，各有具体代价）
    ///
    /// 1. **只收用户上屏过的词**（用户词 + 临时词，见 [`is_sentence_store_word`]）。草稿层
    ///    不收 —— 滑窗草稿会造出大量杂词，它的「用过即转正」（草稿 → 临时）才是质量闸；
    ///    杂词直接进整句词图，污染的是所有人的整句。
    ///    ⚠️ 临时词**曾经**也被挡（误把质量闸放在了临时 → 用户那一跳）：系统库没有的词
    ///    （「拜城县」）手打一次只会进临时库，要再用够 `promote_count` 次才晋升，而在那之前
    ///    整句永远不认它——用户看到的就是「刚打过的词，连着打就没了」。
    ///    ⚠️ 临时层并不只有草稿跃迁来的词：自动造词（`learn_phrase_on_commit`）会把**分步上屏
    ///    的拼接**与**选中的合成整句**直接写进临时层，不经草稿。分错段的一次上屏因此也会以
    ///    整节点进图、带着 [`USER_NODE_BONUS`] 自我强化——这是开关出厂关的理由之一。
    /// 2. **`boundary == 0` 不进图**（与 [`Self::add_abbrev_nodes`] 同、与 [`Self::build`]
    ///    的降级放行**相反**）。整句的每个节点都要求真值切分：手输码用户词没有可信边界，
    ///    放进去等于让 Viterbi 按猜出来的切分组句。代价是隐性造词（无边界）不参与整句，
    ///    这是有意的 —— 宁可不进，不可乱切。
    /// 3. **同词同起点已在图中时取 `log_prob` 较大者**，而不是像简拼节点那样直接跳过。
    ///    GH#93 要的正是这个：用户把一个**系统词库已有**的词加进用户库并调高权重，
    ///    整句里也得认这个新权重。直接跳过的话，用户词对所有已在系统库里的词完全无效。
    ///
    /// ## 权重标定：两头都要管
    ///
    /// 用户词 weight 与词典 weight 同轴（`ln(w / DICT_TOTAL)`），但两者**语义不同**
    /// （词频 vs「我要这个词」），故同轴比大小在两头都会出事，各有一个常量对应：
    ///
    /// - **下不去**：出厂加词 1200 翻不过同码的中频词（实测「概论」1217，差 17 分）⇒
    ///   [`USER_NODE_BONUS`] 在对数域补 +2.0；
    /// - **上不封顶**：导入 wdict 可带 `w=2e9`，`ln(w/T) = +2.11` 比系统最大词还高近 5 个
    ///   自然对数单位（≈148 倍）⇒ [`USER_NODE_WEIGHT_CAP`] 截到 1e6。
    ///
    /// 中间不做分布重映射：默认档位本就落在合理位置（造词晋升 800 压过 94.7% 的系统词）。
    ///
    /// ## 模糊音（`fuzzy_config` 开了任一组时）
    ///
    /// 记录存的是**规范码** + 规范码坐标下的边界，按所打码点查，翘舌打法 `chaijiu` 永远撞不上
    /// `caijiu`。每个起点另走 [`Self::add_store_fuzzy_nodes`]，打分与 [`Self::build`] 里的
    /// 系统词模糊节点同口径（每改动一处扣 [`FUZZY_SYLLABLE_LOG_PENALTY`]），再叠用户词的截顶
    /// 与加成。模糊全关时一次额外查询都没有。
    ///
    /// ⚠️ 本方法由 `Config::sentence_uses_user_words` 把关，**出厂开启**（2026-09-28 起）；
    /// 曾经出厂关闭的三条理由（赢者通吃 / 影响全体老用户 / 标定只做了截断）见该字段文档，
    /// 现仍是已知局限但不再是出厂关的理由。且它只在 `use_smart_compose` 开着时才会被调用。
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
    /// ## 为什么逐边展开而不取一条切分
    ///
    /// 同一跨度常有多条切分：`pinan` 是 `pi|nan` 也是 `pin|an`，平安只在后一条上
    /// （`pin→ping`）。只按一条切分展开，另一条上的变体永远查不到。做法：沿图的音节边从 `p`
    /// 逐层延伸，每走一条边就地展开该音节的变体（[`FuzzyMatcher::fuzzy_variants_scored`]，
    /// 与 step 6 同一份判据），组合携带（变体码、**变体码自己的**音节边界、所打码切分、改动处数）。
    ///
    /// **不先枚举路径再展开**：那样要给路径条数设上限，而截断是按图的遍历顺序截的、没有语义，
    /// 唯一对得上的那条切分可能正好被截掉（`fanganfangan…` 同音节数下 8 条切分）。
    /// 逐层延伸时各切分共享前缀，截断只发生在同层组合超出 [`FUZZY_LAYER_BUDGET`] 时，
    /// 且按改动处数截——少处模糊的组合在任何切分上都先保住。
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
                // 稳定排序后截断：结果确定，但**不是**「每层全局改动最少」的精确保证——
                // 同改动数内部按遍历生成顺序截（先到的边、先到的变体先留），且只在本层比较：
                // 被截掉的父组合，其后代可能比下一层留下的组合改动更少。实际会触发：11 组全开、
                // 6 音节 4 条切分时，≤2 处改动的组合约 292 条，会在同改动数内部被截。
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
    /// - 取数用 `search_abbrev_exact`（只查本组，上限 [`USER_ABBREV_SCAN_LIMIT`]），**过完判据
    ///   再按权重取前 [`USER_NODE_LIMIT`]**。曾用带上限的 `search_abbrev`：存储层按索引键字典序数够就返回、
    ///   且连带扫无边界组，手动加过几个同首字母的词就把名额占满，「拜城县」进不了图；
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
                    // 不会破坏既有节点的上屏 / consumed：节点的起止字节不变，而 consumed 与上屏
                    // 由起止决定。换进来的是该简拼词自己的逐字母切分（击键域，每字母一位）——
                    // 被替换的若是全拼节点，其切分未必与之同形（`any_path` 取最少音节时，两字的
                    // 「西昂」可以配单音节 `xian`），但整句回填的边界本就该跟胜出的那条解释走。
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
mod coda_steal_tests {
    //! [`steals_coda`] 的三种形态：只有**简拼段**抢前一音节的韵尾才算，全拼零声母节点不算。

    use super::*;

    fn node(start: usize, syllables: &[&str]) -> LatticeNode {
        let mut mask = 0u64;
        let mut off = 0;
        for s in syllables {
            mask |= 1 << off;
            off += s.len();
        }
        LatticeNode {
            start,
            end: start + off,
            word: String::new(),
            syllables: syllables.iter().map(|s| s.to_string()).collect(),
            syl_mask: mask,
            log_prob: 0.0,
            canon: None,
        }
    }

    #[test]
    fn consonant_abbrev_segment_steals() {
        // `ningbr` 读成 ni + n|g：`nin` 是音节。
        assert!(steals_coda(
            "ningbr",
            &SyllableTrie::new(),
            &node(2, &["n", "g"])
        ));
    }

    #[test]
    fn vowel_abbrev_segment_steals() {
        // `yuedzh` 读成 yu + 耳朵(e|d)：`yue` 是音节。
        assert!(steals_coda(
            "yuedzh",
            &SyllableTrie::new(),
            &node(2, &["e", "d"])
        ));
    }

    #[test]
    fn full_pinyin_zero_initial_node_does_not_steal() {
        // `ji` + 阿姨(a|yi)：`jia` 虽是音节，但「阿」是零声母全拼音节，不是简拼声母。
        assert!(!steals_coda(
            "jiayi",
            &SyllableTrie::new(),
            &node(2, &["a", "yi"])
        ));
    }
}
