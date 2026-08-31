//! 拼音输入引擎
//!
//! 与 Go 版本 `wind_input/internal/engine/pinyin/` 对齐。
//!
//! 候选生成流程（对齐 Go convertCore）：
//! 1. 精确查找（完整音节 join 无空格）
//! 2. Viterbi 长句解码（>=2 音节）
//! 3. DAG 子短语查找
//! 4. 前缀查找
//! 5. 缩写/简拼匹配
//!
//! 注意：运行时词频 boost 由上层（协调器）应用，本引擎只产出基础权重候选。

pub mod dag;
pub mod fuzzy;
pub mod generate;
pub mod grammar;
pub mod interp;
pub mod lattice;
pub mod mixed_abbrev;
pub mod octagram;
pub mod parser;
pub mod scorer;
pub mod shuangpin;
pub mod syllable;
pub mod viterbi;

use crate::engine::{BoundaryResolution, ConvertOptions, ConvertResult, Engine, EngineType};
use dag::{Dag, SegGraph};
use fuzzy::FuzzyConfig;
use generate::CharPinyinIndex;
use lattice::LatticeBuilder;
use scorer::AbbrevMatcher;
use shuangpin::ShuangpinConverter;
use std::sync::{Arc, OnceLock};
use syllable::SyllableTrie;
use viterbi::{ViterbiDecoder, WordNode};
use wind_candidate::{Candidate, CandidateSource};
use wind_dict::DictManager;
use wind_dict::cached::CachedDict;

/// 整句解的**等效词频**：把 Viterbi 路径分换算回词频域，与其余候选同量纲。
///
/// ```text
/// W_eff = clamp(exp(log_prob) × DICT_TOTAL, 1 ..= i32::MAX)
/// ```
///
/// ## 为什么要同量纲（退役 `SENTENCE_WEIGHT_BASE`）
///
/// 整句原本拿 `3e7 + log_prob×1000`，而其余候选的 weight 是**词频**（1 ~ 5e5），
/// 二者差两个数量级以上、**不可比**。后果不是「整句排太前」，而是**每次想调整整句的
/// 相对位置都只能加一个二值开关**——连续的权重比较在跨轴时根本不成立。现存补丁：
/// step 1.5（反向把词典整词抬到整句量纲）、step 6.5 / 6.5b（改 weight 到 `max-1`）、
/// step 6.6（`is_sentence_contested` 摘锚定）与 `is_sentence_unanchored`、
/// `SENTENCE_YIELD_WEIGHT_FLOOR`，以及 `Candidate` 上 4 个整句相关布尔。
///
/// **清理结果**：step 1.5 / 6.6 / `is_sentence_unanchored` 已删（各自零行为差异），
/// 布尔减到 2 个。**step 6.5 / 6.5b 保留** —— 实测拆不掉，因为同量纲没有解决「整句分数
/// 系统性偏高」：① 同文合并的 `max(词典 weight, W_eff)` 把模糊惩罚旁路了；
/// ② 不归一化的 `∏f_i` 让高频字拼成的合成解天然巨大（`sishi`→「是是」压过「四十」）。
/// 详见 `docs/design/sentence-weight-same-axis.md` §3.2 的实测表。
///
/// ## 为什么这不会把整句压下去（实测，见 `docs/design/sentence-weight-same-axis.md`）
///
/// 高频整句本身就是词典词，同文合并取 `max(词典 weight, W_eff)` 后用的是词典值：
/// `gonghe` 共和 597 vs 恭贺 77、`siyuan` 寺院 491 vs 思源 245、`sixiang` 思想 26133
/// vs 四项 656、`nihao` 你好 5328 vs 拟好 64 —— 四个经典冲突场景顺序全部不变。
/// **3e7 从未起决定性作用，它只是掩盖了「词典权重本来就够用」这个事实。**
///
/// ## 公式：各词频次的**几何平均**
///
/// ```text
/// log_prob = Σ ln(f_i / T)                       （n = 词数，T = DICT_TOTAL）
/// W_eff    = exp(log_prob / n + ln T) = (∏ f_i)^(1/n)
/// ```
///
/// n = 1 时还原成 `f` 本身，与词典权重同量纲。
///
/// ### ⚠️ 为什么要除以 n（这一步是后补的，别再拿掉）
///
/// 初版按 librime 的「不归一化」写作 `exp(log_prob + n·ln T) = ∏f_i`。librime 确实不
/// 归一化（`poet.cc:216` 累加 `dict_compiler.cc:257` 写入的 `log(freq)`），但它**全程在
/// 对数域比较**，而我们要把结果落进 `i32` 的 `weight` 字段 —— `∏f_i` 必然溢出：
///
/// ```text
/// 实测 19 个常见输入，5 个饱和到 i32::MAX：
///   我是中国人 / 我们是 / 他是谁 / 我今天很开心 / 吃饭了
///   被近似 1,690,885,022（濒临）
/// ```
///
/// 饱和的后果不是「整句排太前」，而是**整句之间彻底无法区分**（都等于 i32::MAX，只能
/// 靠 `base_order` 决定先后，实质是随机）。实测 `gongzuosi` 的首选是饱和的「工作是」，
/// 而用户要的「工作室」只有 w=82。
///
/// 几何平均把结果压回词频量纲（恒 ≤ max f_i），**永不溢出，整句之间恒可比**。
///
/// ### 「没有词也强制组句」不受影响
///
/// 那个行为**不靠 weight**，靠协调器比较链 ⓪ `consumed_length`：合成整句消费的输入恒
/// 最长，在 weight 参与比较之前就赢了。weight 只决定**同 consumed 的候选之间**谁先，
/// 而那正是需要「几何平均」这种可比量纲的地方。
///
/// ⚠️ 这条依赖 ⓪ 的语义。若 `consumed_length` 不再是比较链首键，须重新评估本公式。
///
/// ⚠️ 下界仍 clamp 到 1：长整句的几何平均可能落到 0（低频字连乘再开根仍 < 1），
/// 而 0 与「无权重」候选语义混淆。
///
/// ⚠️ `log_prob` 含各类惩罚/加成（`WORD_PENALTY`、单字罚、实词加成、模糊音罚…），故
/// `exp(log_prob/n)` 不是严格的整句概率，`DICT_TOTAL` 实际充当**标定系数**。改那些惩罚
/// 常数须重新标定。
///
/// ### ⚠️ 已知偏差：`/n` 会摊薄「局部特征」类惩罚（已评估决定不修）
///
/// 除以 n 的是**整个** `log_prob`，于是两类惩罚命运不同：
///
/// | 惩罚 | 与词数的关系 | 除以 n 后 |
/// |---|---|---|
/// | `WORD_PENALTY`(3.0/词)、单字罚、虚词加成 | 总量正比于 n | **恒定，不受影响** |
/// | `AMBIGUOUS_PENALTY`(每个歧义接缝) | 与 n 无关 | **被摊薄** |
/// | 模糊音罚(每个模糊音节) | 与 n 无关 | **被摊薄** `0.5^(k/n)` |
///
/// 即 5 词整句里 1 个模糊音节，惩罚从 `0.5` 稀释成 `0.5^0.2 ≈ 0.87` —— **长句里的模糊音
/// 几乎免罚**，而候选层 [`fuzzy_penalized`] 对同一个概念扣的是 `0.5^k`，两处自相矛盾。
///
/// **不修的理由**：① 触发要同时满足「长整句 + 含模糊音 + 走得到 weight」，而长整句的
/// `consumed_length` 恒最大、在比较链 ⓪ 就赢了；② 修法（把模糊惩罚从 `log_prob` 剥离、
/// 归一化后按 `0.5^k` 重施）等于给「求平均」的映射开后门让某一项不参与平均，而
/// `AMBIGUOUS_PENALTY` 同样被摊薄 —— 下次就得再开一个后门，规则边界无原则可依。
///
/// ⚠️ **什么现象出现时回来重新考虑**：真机上「长句里含模糊音的整句不该赢却赢了」，且该
/// 场景确实走到了 weight 比较（与竞争者 `consumed_length` 相同）。
/// 完整论证与参考实现对照见 `docs/design/sentence-weight-same-axis.md` §7。
fn sentence_weight(log_prob: f64, word_count: usize) -> i32 {
    let n = word_count.max(1) as f64;
    let geometric_mean = log_prob / n + lattice::DICT_TOTAL.ln();
    geometric_mean.exp().clamp(1.0, i32::MAX as f64) as i32
}

/// 模糊音命中的权重折扣（对齐 Go `ranker.go` 的 `IsFuzzy → score -= 100`）。
///
/// **为何是惩罚而非层级**：模糊命中是「召回来源」，不是「匹配质量」——`si` 经 s↔sh 命中的
/// 「是」在音节结构上与精确命中的「四」完全对齐，两者本就该同层按权重竞争。此前 `is_fuzzy`
/// 是 `cmp_match_layers` 的首要键（等价于惩罚 ∞），真实词典下打 `si` 时「是」落在第 231 位、
/// 打 `zong` 时「中」落在第 158 位，而生产候选上限仅 50（临拼/混输）~300（拼音方案），
/// 模糊音在全部三条路径上等价于未实现。
///
/// **为何用乘性而非 Go 的加性常数**：Go 的分数是归一化后的加权和（音节对齐 +500、用户词 +300、
/// 词频仅 ×0.00001），而本侧 weight 直接就是词频量纲且跨来源差异极大（词典词 ~1e2、
/// 前缀补全可达 2e9、整句 3e7）。固定减法在不同量纲上效果天差地别，乘性折扣则量纲无关。
///
/// ## 取值 0.5，**按被模糊的音节数累乘**（对齐两个参考实现）
///
/// | | 惩罚 | 按音节累计 |
/// |---|---|---|
/// | librime `kFuzzySpellingPenalty` | `log(0.5)` 加进 `credibility` | ✅ |
/// | libime `fuzzyCost` | `log10(0.5)`，`extraCost = fuzzies × fuzzyCost` | ✅ |
/// | 本常数 | `×0.5` per 音节 | ✅ |
///
/// 两个参考实现在这一点上完全一致：**每个模糊拼写乘 0.5**，多个模糊音节累乘。
///
/// ### 曾用 0.01 一次性折扣，两处偏差
///
/// 1. **量级**：0.01 比参考实现严 50 倍。当时的标定依据是「让精确守住首选位」——汉语同音
///    字词频跨数量级（「是」1799848 vs「四」22625 = 80 倍），要让「四」守住首位，折扣必须
///    < 1/80。这是个**产品取向**选择，不是参考实现的做法；改为对齐后 `si` 会出「是」在前。
/// 2. **不按音节数**：`beijinsi`（jin→jing 且 si→shi，2 个模糊音节）与 `si`（1 个）拿同样
///    的折扣。两个参考实现都累计 —— 模糊得越多，置信度越低。
///
/// ### 作用域：候选层与词图层同轴
///
/// 词图层在 `lattice.rs` 用 `FUZZY_SYLLABLE_LOG_PENALTY`（= `ln(0.5)`）施加同一惩罚，
/// 于是**整句路径也吃到模糊惩罚**了。此前词图固定 −0.5、候选层 ×0.01，同一个「模糊」
/// 概念两个量级，且整句 weight 走 `max(词典 weight, W_eff)` 把词图那份惩罚旁路掉 ——
/// 这正是 step 6.5 的模糊降级拆不掉的根因。
const FUZZY_WEIGHT_SCALE: f64 = 0.5;

/// 对模糊命中施加权重折扣：`weight × 0.5^fuzzy_edits`，见 [`FUZZY_WEIGHT_SCALE`]。
///
/// `fuzzy_edits` 是**模糊改动处数**而非音节数：一个音节可以声母、韵母同时模糊
/// （`sen`→`sheng`），它比只错一处的 `sen`→`shen` 更不可信，该多罚一档。
/// 参考实现同样按「每个模糊拼写」逐个累加，而非按音节。
///
/// 饱和到 `>= 1`：折扣不该把候选压成 0/负权重而改变它与「无权重」候选的关系。
fn fuzzy_penalized(weight: i32, fuzzy_edits: usize) -> i32 {
    if weight <= 1 || fuzzy_edits == 0 {
        return weight;
    }
    let scale = FUZZY_WEIGHT_SCALE.powi(fuzzy_edits as i32);
    ((weight as f64) * scale).round().max(1.0) as i32
}

/// 裸声母（无完整音节，如 "m"）单字提权：使单字候选（吗/么）排在多字前缀补全词
/// （没有/目前）之前——对齐主流输入法「首字优先」。取 1e7：高于常规词频（单字基础权重上限
/// ~2e6），稳压多字词。（历史注记：此值原本还须刻意低于 freq_rerank 的 2e7 阈值以免被误当
/// 整句锚定——该阈值已改为按 `Candidate::is_sentence` 标记判定，此处不必再避让任何数值线。）
/// 提权改的是 weight，故能穿过协调器按权重的重排（否则引擎内单字优先会被 build_candidates
/// 重排冲掉）。仅裸声母（syllables 为空）时应用——完整音节输入的单字已靠 is_prefix 层级就位。
const BARE_INITIAL_SINGLE_CHAR_BOOST: i32 = 10_000_000;

// **用户/临时词**长词上浮的「距词尾」上限曾是常量 `COMPLETION_NEAR_SYLLABLES = 2`，
// 现已改为读用户配置 `completion.max_extra_syllables`（见 `should_promote_user_completion`
// 的文档：硬编码 2 会让配置调宽的用户词长词沉到候选最底、进而被 `truncate` 丢弃）。
//
// ⚠️ 它与系统词库那边的 [`COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES`] **始终是两回事**，
// 别因为都被改动过就试图合并：这边问「这个词还差几个音节打完」（用户词，可信度由用户
// 自己加词背书），那边问「这条补全预测了多少你还没输入的内容」（系统词，还要过
// [`COMPLETION_FAR_WEIGHT_FLOOR`]）。两者一度共用同一个 2，改动时连带破坏了
// `qingfengshu`→「清风输入法」（`promote_user_completion_thresholds` 当场抓到）。

/// **系统词库前缀补全**无条件上浮进完整匹配层的最大距离（候选音节数 - 已完成音节数）。
///
/// 距离 1 = 「补完手头正在输入的这个音节」，词恰好在此结束，置信度天然高；距离 ≥ 2 起
/// 就是在预测用户**尚未输入**的内容，必须过 [`COMPLETION_FAR_WEIGHT_FLOOR`] 才配上浮。
///
/// ## 为什么从 2 收到 1（实测推翻了旧注释）
///
/// 旧值 2 的理由写的是「`beijingd`→北京大学、`jisuanjik`→计算机科学 都是 +2，取 1 会
/// 直接干掉这类极常见场景」。**该判断不成立**：取 1 之后它们只是改走 FLOOR 那条路，而
/// 北京大学 w=2010、计算机科学 w=1609 都远高于门槛 100，照样上浮（实测位次不变）。
///
/// 代价则是实打实的：距离 2 整档白白豁免了 FLOOR，于是 `zhonghuar`（打「中华人民共和国」
/// 打到第 3 个字母）的首选是 **w=18 的「中华人民」**，压过整句「中华」。用户报的
/// 「候选长度来回跳动」正由此而来 —— 有残码时冷僻长词靠豁免登顶，无残码时整句 3e7 登顶，
/// 两套依据逐键切换。收到 1 后该序列的首选长度恢复单调。
const COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES: u32 = 1;

/// 远距离补全的权重门槛：超出近距离的补全属于「预测用户尚未输入的内容」，
/// 需足够高频才配上浮，否则沉回前缀补全层级（仍在候选中，只是排到精确匹配之后）。
///
/// 门槛落在实测数据的空隙里——合理项最低是「中国人民解放军」w=252（`zhongguorenm`，距离 +4）
/// 与「你好吗」w=166（距离 +1，本就走近距离豁免）；噪音项（`zhonghuarenmingongheg` 前缀下
/// 的「中华人民共和国XXX法」条文名）最高 w=60。60~166 之间取 100，双向都有余量。
///
/// 注意不能对近距离补全也套这道门槛：词库 weight_spec 的 median 仅 200，
/// 一半的词低于它，会误沉大量高频使用但低词频的日常词。
const COMPLETION_FAR_WEIGHT_FLOOR: i32 = 100;

/// step 6.5b「整句让位于用完残码的补全」的**置信度下限**（按折后权重标定）。
///
/// 缺了它，任何一条码上挂着的冷僻词都能把整句顶掉：实测 `zhonghuar` 首选变成
/// **`种花人`（w=0）**，整句「中华」被降到 **w=-1**。而同一机制下「你好吗」(原始 166)
/// 顶掉「你好」是用户明确要的 —— 二者的差别只在词频，故必须有一条线。
///
/// **为什么 librime 不需要这道门槛**：其整句由 Poet 与词条**同轴**打分，w=0 的词条自然
/// 排不上去；我们的整句拿 旧的 `SENTENCE_WEIGHT_BASE`(3e7，已退役) 跨轴置顶，「让位」只能做成二值
/// 开关 —— 一旦把连续比较压成布尔，就必须自己补回那道被丢掉的门槛。
///
/// **取值＝ [`COMPLETION_FAR_WEIGHT_FLOOR`] 的一半，两者是同一条线**：6.5b 的候选
/// distance 恒为 1（音节数 == completed + 1），折扣恒 `0.5^1`，故折后 50 等价于原始 100。
/// 代入实测：你好吗 166 ✓ / 中国人民 605 ✓ / 中华人民共和国 3113 ✓ 让位；
/// 计算机课 41 ✗ / 种花人 0 ✗ 不让位。
const SENTENCE_YIELD_WEIGHT_FLOOR: i32 = COMPLETION_FAR_WEIGHT_FLOOR / 2;

/// step 6.5b 的**反向闸门**：补全侧最强者弱于此值时，残码整句不让位（配合
/// [`SENTENCE_KEEP_MAX_COMPLETED_SYLS`] 使用）。
///
/// ## 要治的病
///
/// 6.5b 的判据是光秃秃的 `is_sentence`，写在 step 2c（残码整句）**存在之前** —— 彼时
/// 「整句」只有 step 2 那一种，语义是「只解释了 `completed`、把用户按下的残码丢掉了」，
/// 让位天经地义。step 2c 的整句**消费了整串**，它自己就是「用完残码」的一方，却一并
/// 被降级：实测 `zaim`/`zdm` 的正解「在吗」被压到 `818 = 819 - 1`，让位给「再买」(819)，
/// 而「在吗」在词库里 `w=0`（`base.dict.yaml:103576`），补全路径**根本给不出它**
/// —— 唯一的出路被自己该保护的规则堵死。
///
/// ## 判据为什么是「补全侧的绝对词频」
///
/// 要回答的问题是「**词库在这个码上给不出好答案吗**」：补全侧最强者越弱，越说明该码上的
/// 正解缺频（正是 `w=0` 的病征），此时整句解码（走单字乘积、不吃词条频率）更可信。
///
/// 实测否决过四条：
/// - **整句一律豁免**：`beijingd`→「背景的」、`zhongguorenm`→「中国人吗」当场翻上首位
///   （两条都是现有断言），高频字拼出的虚高合成解会全面翻盘；
/// - **词库中有无同名词条**：「在吗」有(w=0)、「你和」也有(w=0)，区分不开；
/// - **词条 w 是否为 0**：同上，两者都是 0；
/// - **整句/补全的倍数**：⚠️ 首版用的就是它（24×），但它要拿**整句分数**去比，而那正是
///   被语法模型平移的一轴 —— 开启万象模型(w=0.5)后 `zaim` 的「在吗」从 61.3× 掉到 14.0×，
///   被自己的闸门拦掉，真机复现为「zdm 还是在美国」。跨轴比较的病根一直在，只是
///   grammar 关闭时恰好没暴露。
///
/// **词频是单轴的、且完全不受 grammar 影响**，故改用它。
///
/// ## 取值 2500 的标定（真实词库，`completed_syls == 1` 全样本，grammar 开/关一致）
///
/// | 补全侧最强 | 场景 | 该谁赢 |
/// |---|---|---|
/// | **819** | `zaim` 再买 | **整句**（正解「在吗」w=0，补全给不出） |
/// | 6363 | `xiex` 谢谢 | 补全 |
/// | 11131 | `nih` 你会 | 补全 |
/// | 20624 | `shih` 适合 | 补全 |
/// | 31281 ~ 237059 | zhid/zenm/nim/shenm/meig/haiy/tam/wom/meiy/yinw | 补全 |
///
/// 819 与 6363 之间空着 7.8 倍，2500 落在中间、两侧各留 ~3×。同一组取值在
/// grammar 关闭与万象 w=0.5 下**都成立**，标定探针见 `grammar_ratio_calibration`。
const COMPLETION_WEAK_CEILING: i32 = 2500;

/// [`COMPLETION_WEAK_CEILING`] 的**适用范围**：只在已完成音节数 ≤ 此值时允许整句反压补全。
///
/// **这是基于实测的范围限制，不是从第一性原理推出的**，故取值写死为 1 而非做成配置：
/// 已完成 ≥ 2 个音节时，补全侧有更长的上下文约束、置信度实测更高，且现有断言
/// （`beijingd`→「北京的」、`zhongguorenm`→「中国人民」、`nihaom`→「你好吗」）
/// 全部要求补全赢。只有「1 个完整音节 + 残码」这一层是词频缺失的重灾区
/// ——上下文最短，词库一旦缺频就再没有别的信号能把正解托上来。
///
/// 放宽此值前必须重跑标定探针。**放到 2 会当场打破两条现有断言**：`beijingd` 的补全
/// 「北京的」只有 1738、`zhongguorenm` 的「中国人民」只有 605，双双低于
/// [`COMPLETION_WEAK_CEILING`]，于是错解「背景的」「中国人吗」会翻上首位。
///（`duibuq` 反而安全 —— 它的「对不起」有 3984，够不着那道门槛。）
const SENTENCE_KEEP_MAX_COMPLETED_SYLS: u32 = 1;

/// 前缀补全的**每音节权重折扣**：候选每比已输入内容多出一个音节，权重打对折。
///
/// ## 为什么要有它（改动前必读）
///
/// 「残码上浮」原本只有 [`Candidate::is_promoted_completion`] 一个布尔开关：上浮的补全整批
/// 进入完整匹配层，层内**只比裸词频**，extra=1 与 extra=3 同等对待。真实词库实测 `nih`：
/// 114 条补全全部上浮，首选「你会」(w=22262)、「你会发现」(w=13330) 第 2，而「你好」
/// (w=5328) 落到第 3、单字「你」(w=492791) 被整层压到**第 114 位**。
/// 布尔层级等价于「惩罚 = 0 或 ∞」，中间地带无法表达 —— 同款前科见
/// `Candidate::is_fuzzy` 字段文档（它曾是层级键，把「是」压到第 231 位）。
///
/// ## 取 0.5 的依据：两个成熟实现独立地选了同一个数
///
/// - **librime**（`algo/syllabifier.cc:22`）：`kCompletionPenalty = log(0.5)`，每个补全音节
///   累加进 `SpellingProperties::credibility`；而 credibility 与词频**同轴相加**
///   （`dict/dictionary.cc:155` `weight = e.weight - log(1e8) + chunk.credibility`），
///   排序时一并比较（`dict/dictionary.cc:74`）—— 不存在独立层级。
///   同族常量 `kAbbreviationPenalty` / `kFuzzySpellingPenalty` 也都是 log(0.5)。
/// - **fcitx5/libime**（`pinyin/pinyindictionary.cpp:471`）：
///   `overLengthCost = log10(0.5) * lengthDiff`，`lengthDiff` = 候选音节数 - 已输入音节数，
///   直接加进候选 cost。
///
/// 两者都在 log 概率空间做加法，等价于本仓 weight 空间的**幂次乘法**。语义是朴素先验：
/// 用户每少打一个音节，这个猜测的可信度减半。
///
/// ⚠️ 本折扣**不替代**上浮层级：「覆盖输入更长的候选优先」三家一致（librime 的
/// `phrase_->rbegin()` 按 code_length 倒序、libime 的 lattice 覆盖全长路径优先、本仓的
/// `is_partial` 层），删掉层级会让 `meiy`→「没有」重新被数百个单字「没/每/美」淹掉。
const COMPLETION_WEIGHT_DISCOUNT: f64 = 0.5;

/// 按「未输入的音节数」对前缀补全施加权重折扣，见 [`COMPLETION_WEIGHT_DISCOUNT`]。
///
/// `extra` = 候选音节数 - 已完成音节数。`boundary == 0`（无边界信息的旧词典/手输码用户词）
/// 算出 0，即不折扣 —— 与本文件其他位置「无边界信息一律降级放行」的处理一致。
///
/// 饱和到 `>= 1`：与 [`fuzzy_penalized`] 同理，折扣不该把候选压成 0/负权重，
/// 那会改变它与「无权重」候选的关系。
fn completion_penalized(weight: i32, extra: u32) -> i32 {
    if weight <= 1 || extra == 0 {
        return weight;
    }
    let scale = COMPLETION_WEIGHT_DISCOUNT.powi(extra as i32);
    ((weight as f64) * scale).round().max(1.0) as i32
}

/// 前缀补全的取数上限（见 convert 中 `completion_limit` 处的完整说明）。
///
/// 取 1000 是 `push_unique` 的 O(n²) 查重与「单字母可持续翻页」之间的平衡点：
/// 实测 1000 条约 3.5ms，5000 条则达 54ms。1000 条按每页 9 项可翻 110 页，
/// 远超实用范围；该上限只约束**补全**，精确匹配候选不受影响。
const MAX_COMPLETION_CANDIDATES: usize = 1000;

/// 混合简拼查 `AbbrevSection` 时的取码上限（step 5b）。
///
/// 比纯简拼的 10 大一截：纯简拼那边「键即答案」，按权重取前 10 条就是最终候选；混合路径
/// 拿到的码还要过一道逐段校验（`nh` 下的 `nihao`/`nanhai`/`naihe`… 只有第二段等于 `hao`
/// 的能活下来），**绝大多数会被滤掉**，取 10 条几乎必然一条不剩。
/// 索引点查本身是 DAT 前缀走位 + 定长条目读取，放宽到 64 的成本远小于「查了等于没查」。
const MIXED_ABBREV_INDEX_LIMIT: usize = 64;

/// 混合整句的**质量闸门**：路径平均每字 log_prob 低于此值就不插入候选。
///
/// 为什么需要它：整句靠 旧的 `SENTENCE_WEIGHT_BASE`(3e7，已退役) 无条件置顶，而混合整句的简拼段
/// 歧义极大——`bzd` 要在 12 个同简拼词里选，靠 unigram 常常选错。没有闸门时**错误整句
/// 100% 占据首选**，连原本可用的部分候选（分步上屏的「不知道」）都被顶掉。
/// 调 `ABBREV_NODE_PENALTY` 治不了这个：`log_offset` 那点差异撼动不了 3e7 的基准
/// （实测 1.2 / 2.5 / 4.0 三档，D 类指标几乎不动）。
///
/// **取值依据：D 类评测的受控对比（常用词口径，n=1000）**，不是估算：
///
/// | 阈值 | 整句 top-1 | 首词命中 |
/// |---|---|---|
/// | 不设闸门 | 12.10% | 0.00% |
/// | **-8.0（本值）** | **12.10%** | 0.00% |
/// | -6.5 | 10.30% | 0.50% |
/// | -5.0 | 1.10% | 3.30% |
///
/// -8.0 是**零代价点**：整句命中与不设闸门完全相同，却挡掉了最离谱的那批——
/// `nhaoma` 原本出「你会熬吗」（每字 -11.01），闸掉后正确的「你好吗」上位。
///
/// ★ 收紧到 -5.0 曾看似合理（用户真会打的组合每字落在 -3.7 ~ -4.8：`bzdhaobuhao`
/// →不知道好不好 -3.90、`wmyiqizou`→我们一起走 -4.81），但受控对比显示它**砍掉 91%
/// 的正确整句**，只换来 3.3% 的首词命中——很不划算。且整句占首位时部分候选就在第 2、3 位，
/// 按一下数字键即可选到，「首词命中 0%」并不等于分步上屏那条路断了。
///
/// ⚠️ 正确与错误的分布**在 -5 ~ -6.5 区间大幅重叠**，任何阈值都必然既误伤正解又放行错解；
/// -8.0 只保证「不误伤」。真正的区分需要上下文概率（尚无 bigram，缺语料）。
/// 改动本值必须重跑 `pinyin_eval` 的 D 类并做同样的受控对比。
///
/// ## ★ 语法模型开启后本阈值是否仍成立（2026-08-15 专项排查，结论：成立）
///
/// 起因是 `6a5fb75e`（残码整句闸门在开着语法模型时失效）留下的那句话——
/// 「跨轴比较的病根一直在，只是 grammar 关闭时没暴露」。本阈值属同一形态：
/// 拿**会被 grammar 改动**的 `log_prob` 与一个**在 grammar 关闭时标定**的常量比。
///
/// 探针 `tests/grammar_axis_shift.rs` 实测（万象 `weight=0.5`，32 个样本）：
///
/// ```text
/// 平移量  最小 -5.15   中位 +1.60   最大 +10.95   平均 +2.10
/// 因 grammar 而跌破 -8.0 的样本：无
/// pinyin_eval D 类首词命中  0.00% → 0.40%（改善）
/// ```
///
/// ★★ **平移是双向的，方向取决于搭配是否命中**，不是我一开始设想的系统性下移：
/// 打分为 `weight × (ln(频次) − baseline)`，baseline=8.34，而万象常见搭配的 ln 频次
/// 在 15~18 ⇒ 命中则**加分**（最多 +10.95）、未命中才扣分（最多 −5.15）。
/// `6a5fb75e` 的 `zaim`「在吗」之所以掉下去，正因为那个搭配未命中。
///
/// ★ **为什么下降方向没有咬到本阈值**：会被扣分的是「搭配罕见」的整句，而那类整句的
/// 词典权重本来就低、在 grammar 关闭时就已远在 -8.0 以下（实测 -10 ~ -14），
/// 扣分只是让它更低。**两个条件高度相关**，于是「原本刚好在阈值上方、被扣分后跌破」
/// 这个危险区间实际上很稀疏。
///
/// ⇒ 本阈值**无需为 grammar 调整**。但探针留下了，改本值或换模型时应重跑它。
const MIXED_SENTENCE_MIN_LOGP_PER_CHAR: f64 = -8.0;

/// 混合整句解码（step 2b）的最短击键串。
///
/// 至少要容得下「一个简拼段（2 字母）+ 一个全拼音节（2 字母）」，再短就不存在混合形态，
/// 白建一次词图。
const MIN_MIXED_SENTENCE_LEN: usize = 4;

/// 简拼族前缀回退（step 6.2）的最短击键段。与 `is_abbreviation` 的下限一致：
/// 单字母不构成简拼，退到 1 只会拖出一堆高频单字。
const MIN_ABBREV_STROKE: usize = 2;

/// 前缀回退**单个切点**的产出上限。
///
/// 逐切点限流而非只设总额，是为了保证**短切点也挤得进来**：`bzdhaobuhao` 的
/// `bzdh` 切点若把配额占满，`bzd`（→「不知道」，词频高 672 倍）就一条都进不来。
const MAX_FALLBACK_PER_CUT: usize = 6;

/// 前缀回退**参与竞争的切点数**（有产出的才计数）。
///
/// 取 2 = 「最长 + 次长」。相邻切点的竞争才是真实场景（`bzdhaobuhao` 里 `bzdh`→「表彰大会」
/// 与 `bzd`→「不知道」），再往下只会引入越来越短、越来越不相关的解释：试到 `bz` 时
/// 「标准/帮助/保证」会凭着高词频（~5 万）挤进前几位，而它们只解释了 11 键里的 2 键。
///
/// 这是**软偏好**的落点——不给短切点打折扣（那要按未解释字母数调一个乘性系数，量纲难定
/// 且依赖本轮最大消费长度、不稳定），而是限制它们**是否入场**。入场后一律同层按词频竞争。
///
/// ★ **放宽它已被评测否决，勿再尝试。** D 类（简拼+全拼混合整句）实测：
/// 取 2 → 首词召回 40.30% / 首词命中 2.30%；取 8 → **14.60% / 0.00%**，大幅恶化。
/// 原因是短切点的高频词会占据首选并把长切点的正确候选挤出 top-10
/// （`dltqbandaoer` 首选变成 `dl`→「到了」、`dqsmayou` 变成 `dq`→「地球」）。
/// 召回率要越过 40% 这个天花板，得让所有切点同时参与、由语言模型仲裁 —— 即简拼段入
/// lattice（文档 §4.8），不是调这个常量能做到的。
const MAX_FALLBACK_CUTS: usize = 2;

/// 全拼降级支路（双拼下按全拼解释击键）的召回条数上限，精确 + 子短语 + 前缀补全合计。
///
/// 远小于主路径的 `completion_limit`（30 ~ [`MAX_COMPLETION_CANDIDATES`]）是刻意的：本支路
/// 服务「偶尔来用一下的人」，产出恒沉在双拼候选之后，堆再多也只是把翻页拉长；而候选查重
/// 是 O(n²) 线性扫描（见 step4 注释的实测：5000 条 54.5ms），支路不该往这个瓶颈上加码。
const MAX_FULL_PINYIN_RECALL: usize = 24;

/// 全拼降级闸门要求的**最少完整音节数**。
///
/// 取 2 是证伪力与可用性的折中。双拼击键大量拿 v/i/u 作声母（小鹤 v=zh、i=ch、u=sh），
/// 全拼音节表根本切不动，于是 `nihc`(ni + `hc`✗)、`womf`(wo + `mf`✗)、`vsuf`(v 就不成音节)
/// 全被挡在门外，只有 `nihao`/`dianhua` 这类真·全拼串放行 —— 闸门的证伪力全在这里。
///
/// **降到 1 等于取消闸门**：任何双拼串的前两键几乎总能读成一个音节，支路会对每一次击键
/// 都启动。
///
/// 代价：全拼用户打到 `nih`（1 音节 + 残码）时支路不启动，要打满 `niha` 才出候选。这是
/// 有意的取舍；真机若嫌迟钝，调这里之前先确认不是闸门第三条（尾部须为合法音节前缀）的锅。
const FULL_PINYIN_MIN_SYLLABLES: usize = 2;

/// 全拼降级支路里**每级前缀子短语**（②）的召回上限。
///
/// ★ 没有它，单音节前缀的同音单字会把 [`MAX_FULL_PINYIN_RECALL`] 的配额整个吃光：真机
/// `zaijian` 下 `zai` 一级就有 20+ 条（在/再/载/哉/崽/宰/仔/栽/灾/甾/載/災/菑/畠/渽…），
/// 于是 ① 精确整词只挤进 4 条、③ 前缀补全一条不剩，**「再见」压根没被召回**——用户看到
/// 的那条「再见」其实来自双拼流的简拼回退，只消费 4/7 键。
///
/// 子短语的价值本就低于精确整词（只解释开头一截，且双拼流通常已给出同批单字），限得紧些
/// 不损失什么。真正需要这些单字时，用户手头的双拼候选里已经有了。
const MAX_FULL_PINYIN_SUBPHRASE: usize = 4;

/// 用户/临时词的**前缀补全**是否上浮进完整匹配层（贴合「长词打到第 3-4 个音节就给出」）。
///
/// 用户长词（如「清风输入法」qingfengshurufa，5 音节）在部分拼音下由 store 层前缀命中，
/// 但恒带 `is_prefix=true`，会被首音节一大批同音子短语（清/青/情…，`is_prefix=false`）整层
/// 压到候选最底、翻页翻不到。此判据决定何时把它提升到完整匹配层（`is_promoted_completion`）：
///
/// **尾部残码**（未成音节的声母，如 `qingfengs` 的 `s`）算作「已起头的一个音节」——用户已
/// 明确要接着打这个音节，意图强于停在整音节边界（`qingfeng`）。`started` = 完整音节数 +
/// (有残码 ? 1 : 0)：
/// - **有边界**（GUI 加词/学习词带音节真值）：`started ≥ 2` 且**距词尾 ≤ `max_extra`**
///   （用户配置的 `completion.max_extra_syllables`）才上浮。
/// - **无边界**（手输码用户词 `boundary=0`，算不出剩余）：退化为「`started ≥ 3`」门槛，
///   同样对齐「打到第 3 个音节才给」，避免 1-2 音节时被一堆冷僻长词占满前排。
///
/// # 为什么距词尾上限必须读配置，不能硬编码
///
/// 该上限一度是常量 `COMPLETION_NEAR_SYLLABLES = 2`，真机报障：用户把
/// `max_extra_syllables` 调到 10、库里有 11 音节的用户词「清风输入法内测问题反馈」，
/// 打 `qingfengshurufa`(started 5, 剩 6) **翻遍全部 16 页都找不到它**，必须打到
/// `...wen't`(started 9, 剩 2) 才出现 —— 分界点正是那个硬编码的 2。
///
/// ⚠️ 后果比「排在后面」严重得多，这是本判据最容易被低估的地方：
///
/// 不上浮 ⇒ 落进前缀补全层 ⇒ 被首音节同音子短语整层压到候选最底。而引擎侧
/// **`sort_by` 紧跟着 `truncate`**，于是「排到最底」在候选数超过上限时**等于被丢弃**
/// —— 协调器再也收不到它，`cmp_by_consumed` 那道补救也就无从谈起。实测同一条件下
/// `limit=141` 该词消失、`limit=142` 才刚好保住（它就在最后一位）。
///
/// ⇒ **「降级不销毁」的界线不由排序决定，而由候选总数有没有超过上限决定**，这是个
/// 藏在数据规模里的开关：小词库测不出来（本仓测试词库该输入恰好产出 142 条，全保留），
/// 用户的大词库产出远超 300 就必然丢。定位时四轮探针全部误报「正常」正因如此。
///
/// 读配置后语义也与召回层对齐了：召回上限是 `word_syls ≤ started + max_extra`，
/// 即 `remaining ≤ max_extra` —— **召回得进来的用户词，就该上浮得起来**，不再有
/// 「召回了却沉在必被截断的位置」这个中间态。
fn should_promote_user_completion(
    completed_syls: usize,
    trailing_partial: bool,
    boundary: u64,
    max_extra: u32,
) -> bool {
    let started = completed_syls + usize::from(trailing_partial);
    if boundary != 0 {
        let word_syls = boundary.count_ones() as usize;
        let remaining = word_syls.saturating_sub(started);
        started >= 2 && remaining <= max_extra as usize
    } else {
        started >= 3
    }
}

/// 拼音引擎配置
#[derive(Debug, Clone)]
pub struct Config {
    pub show_code_hint: bool,
    pub use_smart_compose: bool,
    /// 是否产出简拼候选（声母缩写，nh→你好）。默认 true = 历史行为（简拼此前恒开、无开关）。
    ///
    /// 混输经 `schema.mix.enable_pinyin_abbrev` 关闭它：简拼让「几乎任何字母串都可能是拼音」
    /// （`is_abbreviation` 只要求每字母是某音节首字母），而混输里有人只拿拼音做临时输入补位。
    /// 关闭时连简拼族的召回一并省掉（step5/5b/6/6.2 整条支路）。
    pub enable_abbrev: bool,
    /// 是否让**尾部残码**参与整句解码（step 2c，`buzhidaok`→「不知道看」）。
    /// 默认 true = 纯拼音方案的行为。
    ///
    /// **混输的码长内关闭它**：那一段的击键串同时是码表码，把它整串当拼音组句是过度解读。
    /// 真机现象是打五笔 `aaw`（本意 `aawt`→「工作」）时首选变成拼音「啊啊我」——残码 `w`
    /// 被补成「我」后整句消费满 3/3 键，于是跨过「消费整串」这道闸门抢走首位。
    ///
    /// ⚠️ **本字段在混输下恒 false，超码长那一侧由调用方经
    /// [`crate::engine::ConvertOptions::allow_partial_final`] 覆写为放行**：判据是「这串还可能
    /// 是码表码吗」而非「是不是混输」，定长码表之外的串不可能是码。整体关掉的代价是
    /// `zaiyebuj` 的尾字母 `j` 不参与组句、打不出「在也不就」（纯拼音一直打得出）。
    ///
    /// ⚠️ **不要复用 [`Self::enable_abbrev`] 当判据**（两者恰好都是「混输时关掉」）：
    /// 一个问「要不要把整串按声母读成简拼」，一个问「要不要把最后半个音节猜完」，
    /// 语义正交。共用一个开关的代价见 `COMPLETION_NEAR_SYLLABLES` 的文档——那次
    /// 两个不相干的功能共用一个常量，改动时连带打断了 `qingfengshu`→「清风输入法」。
    pub enable_partial_final: bool,
    /// 词组补全的音节数约束：至少输入几个音节才给词组（见 `[schema.pinyin.completion]`）。
    /// 补全词的音节数恒 ≥ 输入音节数，故未达门槛时上限收紧到输入音节数本身，
    /// 效果即「只出同音节数的候选」。取 1 = 不设限。
    pub completion_min_syllables: u32,
    /// 词组补全的音节数约束：候选最多比输入多几个音节。
    pub completion_max_extra_syllables: u32,
    /// **双拼方案下**是否额外把击键串当全拼解释一遍（`nihao` → 「你好」）。
    ///
    /// 服务「多人共用一台机器」：主力用户打双拼，偶尔来的人只会全拼。产出的候选整体沉在
    /// 双拼候选之后（[`wind_candidate::Candidate::is_fullpinyin_fallback`] 是 `cmp_match_layers`
    /// 的首键），故对双拼用户是纯增量。
    ///
    /// **只在 `shuangpin.is_some()` 时有意义**：全拼方案下击键本就是全拼，再跑一遍支路等于
    /// 把同一批候选查两次。引擎侧判据固定为二者取与，见 `full_pinyin_gate`。
    ///
    /// **混输强制关闭**（`manager.rs` 接线处），理由同 [`Self::enable_partial_final`]：混输的
    /// 击键串同时是码表码，多接一条全拼流是过度解读。且混输接双拼这个组合本身就不成立
    /// （见 `mixed::MixedEngine::pinyin_may_continue` 的「前提：混输不接双拼」）。
    pub allow_full_pinyin: bool,
}

/// 前缀补全允许的最大候选音节数 —— 词组补全两个旋钮（`[schema.pinyin.completion]`）
/// 合成的那一个数。
///
/// `started` 是**输入自身**表达的音节数（完整音节 + 残码算起头的一个）。未达
/// `min_syllables` 时收紧到 `started`：补全词的音节数恒 ≥ `started`，故这等价于
/// 「不给词组」，但表述成同一个上限，使词库层下推也只需要它。
///
/// ## 判据为什么长这样（改动前必读）
///
/// 现象起点：打 `d` 或 `dian` 时候选里混着「但是」「的时候」「电话」。它们全部来自
/// step4 前缀补全，且因残码上浮（`d` 的 `trailing_partial=true`、距离 ≤
/// [`COMPLETION_NEAR_SYLLABLES`]）被**提升进完整匹配层**与单字同层竞争，于是第 2 页就能
/// 见到 —— 不是排序没生效，是它们被主动提上来的。
///
/// ⚠️ **判据问的是「输入有几个音节」，不是「输入在这条候选的切分下占了几个音节」。**
/// 后者曾被用过一版：它对精确匹配是对的（同一个 `dian`，对「点」是 1 音节、对「堤岸」
/// 是 `di|an` 2 音节，确实没有唯一答案），但**精确匹配根本不需要这道闸门**——
/// 它不预测任何未输入的内容。把那个理由推广到前缀补全上就错了：`xia` 会因为存在
/// `xi|an`（西安）的切分而被当成 2 音节输入、整批放行词组，`ying` 同理漏出
/// `yin|guo`（因果）。对前缀补全而言输入的音节数是**唯一确定**的 —— `completed_len`
/// 是词图的性质，与走哪条切分路径无关（见 convert 中该变量的论证）。
///
/// ⚠️ **也不能拿"是不是前缀补全"当唯一判据**：`d` 的单字候选（「的」code=`de`）与词组
/// 候选（「但是」code=`danshi`）出自同一条 step4，`is_prefix` 全为 true；`d` 又短于
/// `is_abbreviation` 的 2 字母下限、简拼路径压根不启动 ⇒ 若按来源整条关掉前缀查询，
/// `d` 会是**零候选**。两者要一起用：`is_prefix` 圈定适用范围，音节数决定去留。
///
/// ⚠️ **`min_syllables` 取 3 会伤到既有定点**：`nih`→「你好」、`meiy`→「没有」是残码上浮
/// 机制存在的理由（`pinyin_completion.rs` 有断言），它们的 `started` 都恰好是 2。
///
/// ⚠️ **`max_extra` 两头够不着**（实测，改默认值前先读）：既有定点里
/// `zhonghuar`→「中华人民共和国」extra=4、`zhongguorenm`→「中国人民解放军」extra=3，
/// 而真机抱怨的 `nih`→「你会发现」extra=2、「你会怎么做」extra=3 —— 后者与前者
/// **在音节维度上完全同形**，单靠 extra 分不开；weight 也分不开（「你会发现」13330
/// 高于「中华人民共和国」3113）。真正的区别是「该前缀下有没有竞争者」，本轮没有实现
/// 这个判据，故只能由用户按口味取舍：出厂 3 保住「中国人民解放军」而放弃「中华人民
/// 共和国」，调到 1 才能消掉「你会发现」。
fn completion_syllable_cap(started: u32, min_syllables: u32, max_extra: u32) -> u32 {
    if started < min_syllables {
        started
    } else {
        started.saturating_add(max_extra)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_code_hint: false,
            use_smart_compose: true,
            enable_abbrev: true,
            enable_partial_final: true,
            // ⚠️ 与 `wind_config::PinyinCompletion::default()` 的 4 / 5 **保持同值**。
            // 真实路径总是从 wind-config 传入（`manager.rs` 的 `completion_min_syllables:
            // pg.completion.min_syllables`），本处只在纯引擎构造时兜底——但两处分叉会让
            // 「引擎单测通过、协调器行为不同」这类假绿有可乘之机，故必须一起改。
            // 取值理由（对齐 librime/fcitx5 的 4 音节门槛、4+5=9 音节上限）见 wind-config 侧。
            completion_min_syllables: 4,
            completion_max_extra_syllables: 5,
            // 默认关：全拼降级是显式开启的降级通道，不该在任何未声明的地方生效
            // （测试要覆盖支路时显式置 true，这样"支路参与了哪些用例"一目了然）。
            allow_full_pinyin: false,
        }
    }
}

/// 拼音引擎
pub struct PinyinEngine {
    /// 引擎配置（show_code_hint / use_smart_compose 等）
    config: Config,
    dict: CachedDict,
    trie: SyllableTrie,
    viterbi: ViterbiDecoder,
    lattice_builder: LatticeBuilder,
    fuzzy_config: FuzzyConfig,
    /// 用户/临时造词层（L 造词显现）：仅含 StoreUserLayer/StoreTempLayer，无系统层。
    /// 拼音候选除主词典外，按相同的码并入这些层的用户造词（None=无持久化，如纯测试）。
    store_layers: Option<Arc<DictManager>>,
    /// 造词反推用的单字读音索引（懒构建：首次 generate_word_pinyin 时从词典派生）。
    char_pinyin_idx: OnceLock<CharPinyinIndex>,
    /// 双拼转换器（None 表示全拼模式，输入原样传递）。
    shuangpin: Option<ShuangpinConverter>,
    /// 双拼布局含非字母键时的码元字符集（如微软/搜狗/紫光的 `;` = ing）。
    ///
    /// 没有它，`;` 就不是码元 → 协调器的非字母码元闸门放行 → 键一路流到次选键 /
    /// 模式引导键 / 标点流水线被吃掉，`ing` 韵母整个打不出来（引擎侧 `convert("n;")`
    /// 明明返回 `ning`，故障只在分派层）。全拼与「布局全是字母」时为 `None`，
    /// 协调器回落内置 `a-z`，与历史逐键等价。
    input_chars: Option<wind_config::CodeCharSet>,
}

impl PinyinEngine {
    /// （`with_unigram` 已随 unigram 表并回 dict 移除，见 `lattice::score_node_inner`：
    /// 词图打分改用词条自身的**词典权重**。
    ///
    /// 注意这句话只对 **unigram** 成立：`with_grammar` 挂的是 n-gram **上下文**模型，
    /// 提供的是词典里没有的信息——词与词之间的搭配强度，与那次合并不矛盾。）
    pub fn new(config: Config, dict: CachedDict) -> Self {
        Self {
            config,
            dict,
            trie: SyllableTrie::new(),
            viterbi: ViterbiDecoder::new(),
            lattice_builder: LatticeBuilder::new(),
            fuzzy_config: FuzzyConfig::default(),
            store_layers: None,
            char_pinyin_idx: OnceLock::new(),
            shuangpin: None,
            input_chars: None,
        }
    }

    /// 注入用户/临时造词层（L 造词显现）。链式 builder：构造后由 EngineManager 按 schema 挂上。
    pub fn with_store_layers(mut self, layers: Arc<DictManager>) -> Self {
        self.store_layers = Some(layers);
        self
    }

    /// 注入模糊音配置（取代 with_unigram 中的 FuzzyConfig::default()）。
    pub fn with_fuzzy(mut self, fuzzy: FuzzyConfig) -> Self {
        self.fuzzy_config = fuzzy;
        self
    }

    /// 注入上下文语言模型（链式 builder）。
    ///
    /// 挂上之后 Viterbi 会从单状态 DP 切到 beam（见 [`ViterbiDecoder`]），
    /// 每个转移额外拿一份上下文分。不挂则**逐位**维持原行为。
    pub fn with_grammar(mut self, grammar: Arc<dyn grammar::Grammar>) -> Self {
        self.viterbi = ViterbiDecoder::with_grammar(grammar);
        self
    }

    /// 注入双拼转换器（链式 builder）。注入后 convert/compute_composition 均先把输入转为全拼。
    ///
    /// 码元字符集在此一并算定（见 [`Layout::code_char_set`]）：布局是引擎的构造参数，
    /// 换布局＝重建引擎，故字段与引擎同生命周期，不会像按方案 id 缓存那样有失效时机问题。
    pub fn with_shuangpin(mut self, conv: ShuangpinConverter) -> Self {
        self.input_chars = conv.layout().code_char_set();
        self.shuangpin = Some(conv);
        self
    }

    /// 仅测试用：读取 fuzzy_config.zh_z 以验证 with_fuzzy 注入是否生效。
    #[cfg(test)]
    pub fn fuzzy_zh_z(&self) -> bool {
        self.fuzzy_config.zh_z
    }

    /// 总条目数
    pub fn entry_count(&self) -> usize {
        self.dict.len()
    }

    /// 从起始位置贪心切出连续完整音节（每步取最长匹配），返回 (音节序列, 结束字节位置)。
    /// 对齐 Go `ContiguousCompletedFromStart`：遇到无完整音节即停（残缺尾部不计入）。
    fn contiguous_completed_from_start(&self, prefix: &str) -> (Vec<String>, usize) {
        let mut syllables = Vec::new();
        let mut pos = 0;
        while pos < prefix.len() {
            // match_at 返回该位置所有完整音节，最长优先；取最长贪心推进。
            let matches = self.trie.match_at(prefix, pos);
            let Some(syl) = matches.into_iter().next() else {
                break;
            };
            pos += syl.len();
            syllables.push(syl);
        }
        (syllables, pos)
    }

    /// 计算 preedit 显示与音节信息。
    /// `full_pinyin` 必须已是全拼串（调用方负责转换），本方法不再内部做双拼→全拼转换。
    ///
    /// 含手动分隔符 `'` 时：按 `'` 分段各自组合，段间以 `'` 重新连接——保留全部手动边界
    /// （含开头 / 结尾 / 连续 `''`），使末尾 `'` 立即可见。段内仍走自动分词。
    fn compute_composition(&self, full_pinyin: &str) -> (String, Vec<String>, String) {
        if !full_pinyin.contains('\'') {
            return self.compose_segment(full_pinyin);
        }
        let mut all_syllables: Vec<String> = Vec::new();
        let mut last_partial = String::new();
        let mut rendered: Vec<String> = Vec::new();
        for seg in full_pinyin.split('\'') {
            if seg.is_empty() {
                rendered.push(String::new());
                continue;
            }
            let (seg_pre, seg_syls, seg_partial) = self.compose_segment(seg);
            rendered.push(seg_pre);
            all_syllables.extend(seg_syls);
            last_partial = seg_partial;
        }
        let preedit = rendered.join("'");
        (preedit, all_syllables, last_partial)
    }

    /// 对单个「无分隔符」片段做自动分词并组合 preedit（原 compute_composition 逻辑）。
    fn compose_segment(&self, full_pinyin: &str) -> (String, Vec<String>, String) {
        let input = full_pinyin;
        let dag = Dag::build(input, &self.trie);
        let syllables = dag.maximum_match();
        let consumed: usize = syllables.iter().map(|s| s.len()).sum();
        let partial = if consumed < input.len() {
            input[consumed..].to_string()
        } else {
            String::new()
        };

        let mut preedit = syllables.join("'");
        if !partial.is_empty() {
            if !preedit.is_empty() {
                preedit.push('\'');
            }
            preedit.push_str(&partial);
        }
        if preedit.is_empty() {
            preedit = input.to_string();
        }
        (preedit, syllables, partial)
    }

    /// 简拼（`nh`→`n'h`）与混合简拼（`nhao`→`n'hao`、`wbwn`→`w'b'w'n`）的击键分段显示。
    /// 非简拼候选 / 无边界真值 / 切不出与击键相符的串 → `None`（调用方保持原显示）。
    ///
    /// **切的是击键串，不是候选的 code。** 直接渲染 code 会显示成 `ni'hao`：用户只敲了
    /// 4 键却看到 5 个字母，退格与光标编辑立刻错位。这里用候选自带的真值音节序列去切
    /// `raw_input`，声母段吃 1 字节、音节段吃整段（见 `render_keystroke_preedit`）。
    ///
    /// 返回 `None` 而不是硬拼一个串：preedit 是显示层，宁可少一个分隔符，也不能给出与
    /// 击键长度不符的串。
    fn abbrev_keystroke_preedit(&self, cand: &Candidate, raw_input: &str) -> Option<String> {
        if !cand.is_abbrev || cand.boundary == 0 {
            return None;
        }
        let syls = mixed_abbrev::syllables_from_boundary(&cand.code, cand.boundary)?;
        let (head, used) = mixed_abbrev::render_keystroke_preedit(raw_input, &syls)?;
        if used == raw_input.len() {
            return Some(head);
        }
        if cand.consumed_length != 0 && used == cand.consumed_length {
            // 部分匹配（step 6.2 前缀回退）：余下的击键**自己再切一遍**，别整段甩上去。
            // `bzdnihaob` 选中「不知道」(消费 bzd) 后尾巴是 `nihaob`，含完整音节
            // `ni`/`hao` + 残码 `b`——整段追加会显示成 `b'z'd'nihaob`，该切的地方没切。
            // 走 compose_segment 得 `ni'hao'b`，最终 `b'z'd'ni'hao'b`。
            let (tail, _, _) = self.compose_segment(&raw_input[used..]);
            return Some(format!("{head}'{tail}"));
        }
        // 走不完且不是部分候选：模式与击键对不上，硬拼只会给出与击键长度不符的串。
        None
    }

    /// 尊重手动分隔符 `'` 的音节分段：按 `'` 切段、各段独立 DAG 最大匹配，
    /// 拼接为纯音节序列（不含 `'`）。`'` 为硬边界，任何音节不得跨越。
    /// 段内未成音节的残码（partial）不计入（仅用于 completed 音节序列）。
    fn segment_with_separators(&self, input: &str) -> Vec<String> {
        self.spans_with_separators(input)
            .into_iter()
            .map(|s| s.pinyin)
            .collect()
    }

    /// 同 [`Self::segment_with_separators`]，但保留每个音节在 raw / flat 两域的偏移。
    ///
    /// `consumed_length` 要回映射到**含 `'` 的原始输入空间**，需要的正是这些偏移；
    /// 只拿音节文本的那个版本丢了它们，此前只能靠 `map_consumed_over_separators`
    /// 逐字节数分隔符补算。两者共用同一次切分，故取 span 版本不多花开销。
    fn spans_with_separators(&self, input: &str) -> Vec<interp::SylSpan> {
        interp::spans_for_full_pinyin(input, &self.trie)
    }

    /// 由全拼码取简拼（各音节声母拼接），供用户/临时造词层的简拼**判据**使用。
    ///
    /// ⚠️ 这是**判据**，不是召回：候选集由声母索引给出（`wind_store::abbrev_index`，
    /// 键就是本函数 `boundary != 0` 分支的产物），本函数负责逐条比对。两侧对 boundary
    /// 的解释必须**逐位一致**——那边加了个这边没有的守卫，索引键就永远匹配不上查询，
    /// 表现为「简拼一条都召不回」而不是报错。
    ///
    /// 曾经这里的注释写的是「用户词库规模小，现场取声母足够快，无需建索引」——
    /// 那个假设在 19 万词时失效（实测枚举一次 172ms，而 step 6.2 逐切点还要再来十几遍）。
    ///
    /// **优先采信候选自带的 `boundary`（音节起始字节位 bitmask）**——那是造词/词库解析
    /// 期留下的真值，直接取这些位置的字符即得声母。仅当 `boundary == 0`（旧数据、
    /// 手输码、五笔码）才退回 DAG 切分去猜。
    ///
    /// ⚠️ 重猜在**歧义切分码**上必错，且既漏又错。用户词「西安宁」真值 `xi|an|ning`
    /// 应给 `xan`，而 `maximum_match` 切成 `xian|ning` 给出 `xn` —— 真简拼打不出、
    /// 假简拼反而命中。这是「`maximum_match` 不是真相」的第二次现场（第一次是整句
    /// boundary，见 `pinyin_multipath.rs`：必须用解码器实际走的那条路径）。
    ///
    /// 切分未完全覆盖 code（残码/非法拼音）时返回 None，不参与简拼匹配。
    ///
    /// ⚠️ 混合简拼另有 [`mixed_abbrev::syllables_from_boundary`]，对同一个 `boundary` 做
    /// 同一件事的另一半（那边要整段音节，这边只要首字母）。**两者对 boundary 的解释必须
    /// 一致**，改动其一时同步核对另一处。
    fn abbrev_of_code(&self, code: &str, boundary: u64) -> Option<String> {
        if boundary != 0 {
            // bit 位是**字节**偏移；拼音码为 ASCII，char_indices 的下标即字节位。
            return Some(
                code.char_indices()
                    .filter(|(i, _)| *i < 64 && (boundary >> i) & 1 == 1)
                    .map(|(_, ch)| ch)
                    .collect(),
            );
        }
        let syllables = self.segment_with_separators(code);
        let consumed: usize = syllables.iter().map(|s| s.len()).sum();
        if syllables.is_empty() || consumed != code.len() {
            return None;
        }
        Some(syllables.iter().filter_map(|s| s.chars().next()).collect())
    }

    /// 简拼族召回，但只针对击键串的一个**前缀**（step 6.2 的前缀回退用）。
    ///
    /// `stroke` = 击键串的前 `consumed` 个字节；产出的候选只消费这么多击键，余下的字母
    /// 留给下一次转换（分步上屏）。覆盖四条来源，与整串路径一一对应：
    /// 纯简拼系统词（step5）、混合简拼系统词（step5b）、以及二者的用户词版本（step6）。
    ///
    /// ⚠️ **与那三处是重复实现，改判据时必须同步。** 没有合并是有意的取舍：那三处各自
    /// 带着大段历史踩坑注释和专门的回归测试（层级一致、音节数过滤、边界豁免…），把它们
    /// 抽成公共函数的回归风险大于这里重复三十行的维护成本。判据本身很短，且都由
    /// `mixed_abbrev` 的同一套模式表达。
    fn recall_abbrev_prefix(&self, stroke: &str, consumed: usize, cands: &mut Vec<Candidate>) {
        let trie = &self.trie;
        let dict = &self.dict;
        // 本切点的产出配额（见 MAX_FALLBACK_PER_CUT：不逐切点限流，长切点会把额度占满，
        // 词频高得多的短切点一条都进不来）。
        let start = cands.len();

        // 部分候选统一形态：`is_abbrev` 归入简拼层、`is_partial` 表示只覆盖了输入前缀
        // （沉在完整匹配之后），`consumed_length` **自带击键域的消费数**——下方那个按
        // code/query 关系统一计算 consumed 的循环会跳过它们（见其注释）。
        let push =
            |cands: &mut Vec<Candidate>, text: String, code: String, w: i32, boundary: u64| {
                if cands.len() - start >= MAX_FALLBACK_PER_CUT {
                    return;
                }
                if text.is_empty() || cands.iter().any(|c| c.text == text) {
                    return;
                }
                cands.push(Candidate {
                    text,
                    code,
                    weight: w,
                    natural_order: 999999,
                    source: CandidateSource::Pinyin,
                    is_abbrev: true,
                    is_partial: true,
                    boundary,
                    consumed_length: consumed,
                    ..Default::default()
                });
            };

        // ① 纯简拼系统词（同 step5，含「音节数 == 简拼字母数」过滤，boundary=0 走 DAG 兜底）
        //
        // 本函数整体已由调用方按 `config.enable_abbrev` 把关（见 step 6.2 的入口条件），
        // 故此处只需判形态。该值下面 ③④ 复用，不重算——重算就多一次漂移的机会。
        let plain = AbbrevMatcher::is_abbreviation(stroke, trie);
        if plain {
            for abbr_code in dict.search_abbrev(stroke, 10) {
                for h in dict.search_with_boundary(&abbr_code) {
                    let eb = effective_boundary(&abbr_code, h.boundary, trie);
                    if eb != 0 && eb.count_ones() as usize != stroke.len() {
                        continue;
                    }
                    push(cands, h.text, abbr_code.clone(), h.weight, h.boundary);
                }
            }
        }

        // ② 混合简拼系统词（同 step5b）。`stroke` 已是完整音节序列时不做混合解释——
        //    那属于全拼语义，交给主路径（此处的 stroke 是前缀，主路径按整串查，故这里
        //    直接跳过即可）。
        let covered: usize = Dag::build(stroke, trie)
            .maximum_match()
            .iter()
            .map(|s| s.len())
            .sum();
        let pats = if covered >= stroke.len() {
            Vec::new()
        } else {
            mixed_abbrev::mixed_patterns(stroke, trie)
        };
        if !pats.is_empty() {
            let mut keys: Vec<&str> = pats.iter().map(|p| p.key()).collect();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                for abbr_code in dict.search_abbrev(key, MIXED_ABBREV_INDEX_LIMIT) {
                    for h in dict.search_with_boundary(&abbr_code) {
                        let Some(syls) =
                            mixed_abbrev::syllables_from_boundary(&abbr_code, h.boundary)
                        else {
                            continue;
                        };
                        if !pats.iter().any(|p| p.key() == key && p.matches(&syls)) {
                            continue;
                        }
                        push(cands, h.text, abbr_code.clone(), h.weight, h.boundary);
                    }
                }
            }
        }

        // ③④ 用户/临时造词层的纯简拼与混合简拼（同 step6 末段）
        //
        // 经**声母索引**取候选，判据一字未动：索引只保证「声母投影对得上」，
        // 音节数、逐段全等仍在下面逐条判。见 `DictLayer::search_abbrev`。
        if let Some(store_dm) = &self.store_layers {
            for c in self.recall_store_by_abbrev(store_dm, stroke, plain, &pats) {
                let plain = self.abbrev_of_code(&c.code, c.boundary).as_deref() == Some(stroke);
                let mixed = !plain
                    && mixed_abbrev::syllables_from_boundary(&c.code, c.boundary)
                        .is_some_and(|syls| pats.iter().any(|p| p.matches(&syls)));
                if plain || mixed {
                    push(cands, c.text, c.code, c.weight, c.boundary);
                }
            }
        }
    }

    /// 从用户/临时词层按**声母索引**取简拼候选集（整串路径与前缀回退路径共用）。
    ///
    /// ## 要查哪些声母串
    ///
    /// - 纯简拼：击键串本身就是声母串（`nh` → 查 `nh`）；
    /// - 混合简拼：每条模式的**声母投影键**（`nhao` = n + hao → 投影键 `nh`）。
    ///   这正是 `mixed_abbrev` 模块设计时就为系统词库 `AbbrevSection` 准备的那个键
    ///   （见其模块文档的四步图），用户词索引沿用同一套键，故两侧完全同构。
    ///
    /// ## `plain` 必须由调用方传入，**不得在此重算**
    ///
    /// 调用方的闸门是 `config.enable_abbrev && is_abbreviation(stroke, trie)`。这里若图省事
    /// 只重算 `is_abbreviation`，就会丢掉 `enable_abbrev` 那一半：混输下用户明明关了简拼
    /// （`schema.mix.enable_pinyin_abbrev = false`），召回照样出候选。
    ///
    /// 这是「闸门长在调用点、召回搬进新函数只搬了一半」的老毛病——重算有漏项的自由度，
    /// 传参没有。`pats` 同理：调用方已按 `enable_abbrev` 决定它是否为空。
    ///
    /// 入口条件（`plain || !pats.is_empty()`）保持原样：`nih` 这类形态 `is_abbreviation`
    /// 判假（`i` 不是任何音节首字母）却有合法混合解释，故取或。
    ///
    /// ## 返回的是超集
    ///
    /// 索引只保证声母投影相等，**判据一律留给调用方**：纯简拼要比对整串、混合简拼要
    /// 逐段校验音节。这条边界是刻意的——改索引不该改变简拼的语义，只该改变取候选的代价。
    ///
    /// 去重按 (code, text)：一个词可能同时落在纯简拼键与某条混合模式的投影键下。
    fn recall_store_by_abbrev(
        &self,
        store_dm: &DictManager,
        stroke: &str,
        plain: bool,
        pats: &[mixed_abbrev::MixedPattern],
    ) -> Vec<Candidate> {
        let mut keys: Vec<&str> = Vec::new();
        if plain {
            keys.push(stroke);
        }
        keys.extend(pats.iter().map(|p| p.key()));
        keys.sort_unstable();
        keys.dedup();

        let mut out: Vec<Candidate> = Vec::new();
        for key in keys {
            for c in store_dm.search_abbrev(key, 0) {
                if !out.iter().any(|x| x.code == c.code && x.text == c.text) {
                    out.push(c);
                }
            }
        }
        out
    }

    /// 全拼降级支路的**准入闸门**：这串击键本身够不够像一串全拼。
    ///
    /// 返回 `Some(音节序列)` 即放行，顺带把切分交给调用方（召回时不必重切）。三条判据：
    /// - 从 0 起连续切出的完整音节数 ≥ [`FULL_PINYIN_MIN_SYLLABLES`]；
    /// - **首音节 ≥2 字母**——挡掉 `a`/`e`/`o` 起头的退化解析，与
    ///   `is_possible_pinyin_sequence` / `is_whole_syllable_pinyin` 同款守卫；
    /// - 尾部要么被音节吃干净，要么是**合法音节前缀**（`nihaom` 的 `m` 放行、
    ///   `nihaoxyz` 的 `xyz` 拒绝）。
    ///
    /// ⚠️ 入参必须是**原始击键**（`raw_input`），不是双拼转换后的全拼——本支路的全部意义
    /// 就是「不经双拼转换地读这串键」。传错域会让闸门恒真：转换结果本身就是合法全拼串。
    fn full_pinyin_gate(&self, stroke: &str) -> Option<Vec<String>> {
        let (syllables, end) = self.contiguous_completed_from_start(stroke);
        let first = syllables.first()?;
        if syllables.len() < FULL_PINYIN_MIN_SYLLABLES || first.len() < 2 {
            return None;
        }
        // 尾部残码须是某音节的合法前缀（`m` 可以，`xyz` 不行）。
        if end < stroke.len() && !self.trie.is_prefix(&stroke[end..]) {
            return None;
        }
        Some(syllables)
    }

    /// 全拼降级召回：把击键串当**全拼**查词典，产出候选全部标
    /// [`Candidate::is_fullpinyin_fallback`]（经 `cmp_match_layers` 首键整体沉在双拼之后）。
    ///
    /// ## 与 [`Self::recall_abbrev_prefix`] 同构
    ///
    /// 两者都是「以击键串为准、`code` 与当次击键不同域」的第二召回通道，故同样自带层级
    /// 标志、自带击键域的 `consumed_length`、自带限流。调用方须在三处双拼专属逻辑上豁免
    /// 本批候选（边界校验 / `map_consumed_length` / `build_raw_preedit`），见字段文档。
    ///
    /// ## 覆盖范围
    ///
    /// 对应主路径的 step1 / step3 / step4 / step2：精确整词、各级前缀子短语、前缀补全、
    /// 整句解码（⑤，含自带的「整句让位于精确整词」）。
    /// **模糊音跟随** `[schema.pinyin.fuzzy]`，与双拼流同一套设置（①②走 `lookup_with_fuzzy`、
    /// ⑤ 把 `fuzzy_config` 传进词图）。首版刻意不做，被真机反馈否掉——同一个人同一套模糊音
    /// 配置在两条流下表现不一致，本身就是缺陷。
    ///
    /// - **不含简拼**：简拼判据本就走 `abbr_query`（击键域），主路径已覆盖，再来一遍纯属重复；
    /// - **不含残码补全整句**（主路径 step 2c）：它让整句消费满整串从而跨过若干「消费整串」
    ///   闸门，副作用面大于收益（同一顾虑使混输的**码长内**也关着它，见
    ///   [`crate::engine::ConvertOptions::allow_partial_final`]）。
    ///
    /// ## 为什么消费长度可以直接取字节数
    ///
    /// 本支路里「全拼域」与「击键域」**是同一个域**（支路的定义就是把击键当全拼读），故
    /// `consumed_length` 直接取码的字节数。绝不可再过 `map_consumed_length`——那是双拼流
    /// 专用的全拼→击键回映射，用在这里等于二次换算，必然错位。
    fn recall_full_pinyin(&self, stroke: &str, syllables: &[String], cands: &mut Vec<Candidate>) {
        let dict = &self.dict;
        let start = cands.len();
        // 输入自身表达的音节数，口径同主路径的 `started_syllables`：完整音节 + (有残码 ? 1 : 0)。
        //
        // 本支路可以直接按字节长度判残码 —— 全拼域与击键域**是同一个域**（支路的定义就是
        // 把击键当全拼读，见函数文档「为什么消费长度可以直接取字节数」）。主路径那边两域
        // 不同，`started_syllables` 必须走双拼域，混用会静默错配（见 step 6.7 的位置说明）。
        let completed_bytes: usize = syllables.iter().map(String::len).sum();
        let trailing_partial = completed_bytes < stroke.len();
        let started = syllables.len() as u32 + u32::from(trailing_partial);
        // ④ 产出的用户/临时词前缀补全（见那里的注释），末尾据此分流上浮判据。
        let mut user_prefix_texts: Vec<String> = Vec::new();
        // 同文去重的规则**不是**「无条件保留先到者」，而是「保留解释得更多的那条」。
        //
        // ★ 真机现场（`zaijian` → 「再见」）：双拼流的简拼前缀回退（step 6.2）先用前 4 键
        // `zaij` 召回了「再见」并标 `consumed=4`，本支路随后想以 `consumed=7`（完整解释）
        // 补一条，若无条件让位，用户选中后只吃掉 4 键、缓冲里凭空剩下 `ian`。
        //
        // 「双拼优先」讲的是**双拼的完整解释**优先于全拼的完整解释（`nini` 那种两域同形的
        // 情形，consumed 相等 ⇒ 不替换 ⇒ 双拼那条留下），而不是「双拼的半截解释」也优先。
        // 判据落在 consumed 上，两件事各归各位。
        //
        // `consumed_length == 0` 表示引擎未标注（全仓约定＝消费整串），不得替换。
        let push = |cands: &mut Vec<Candidate>,
                    text: String,
                    code: String,
                    weight: i32,
                    order: i32,
                    boundary: u64,
                    is_prefix: bool,
                    consumed: usize,
                    is_fuzzy: bool| {
            if text.is_empty() {
                return;
            }
            let cand = Candidate {
                text,
                code,
                weight,
                natural_order: order,
                source: CandidateSource::Pinyin,
                // 恒 true —— 本字段是**来源标记**（「这条来自全拼降级支路」），不是排序决策。
                // 沉不沉底由 `cmp_match_layers` 按 `is_prefix`/`is_partial` 另行判定，见该函数。
                // 三处豁免（边界校验 / consumed 回映射 / preedit 跟随）认的都是这个来源标记，
                // 若按「是否沉底」来置位，高置信候选就会丢标记、三处豁免一起失灵。
                is_fullpinyin_fallback: true,
                is_fuzzy,
                is_prefix,
                // 子短语＝码短于击键（`nihao` 的「你」），与主路径 push_unique 同义。
                is_partial: !is_prefix && consumed < stroke.len(),
                boundary,
                consumed_length: consumed,
                ..Default::default()
            };
            if let Some(existing) = cands.iter_mut().find(|c| c.text == cand.text) {
                if existing.consumed_length != 0 && existing.consumed_length < cand.consumed_length
                {
                    *existing = cand;
                }
                return;
            }
            if cands.len() - start >= MAX_FULL_PINYIN_RECALL {
                return;
            }
            cands.push(cand);
        };

        // ① 精确整词：完整音节覆盖的那一段（`nihaom` 取 `nihao`，残码 `m` 留给续输）。
        //
        // 走 `lookup_with_fuzzy` 而非裸 `search_with_boundary`：**模糊音必须支持**。首版刻意
        // 不做（「降级通道不做二次放大」），但真机反馈直接否掉了那个取舍——用户在
        // `[schema.pinyin.fuzzy]` 里开了 zh_z/sh_s/an_ang…，双拼流吃这些设置而全拼流不吃，
        // 于是同一个人同一套模糊音设置在两条流下表现不一致，这本身就是缺陷。
        // 惩罚由 `lookup_with_fuzzy` 内部的 `fuzzy_penalized`（0.5^音节数）施加，与主路径同源。
        let completed: String = syllables.concat();
        for h in self.lookup_with_fuzzy(&completed, syllables) {
            let c = completed.clone();
            let n = c.len();
            push(
                cands, h.text, c, h.weight, h.order, h.boundary, false, n, h.is_fuzzy,
            );
        }

        // ② 各级前缀子短语（`nihao` → `ni`），供分段上屏。段数上限同主路径 step3。
        if syllables.len() >= 2 {
            for end in 1..syllables.len().min(6) {
                let code: String = syllables[..end].concat();
                // 逐级限流，见 MAX_FULL_PINYIN_SUBPHRASE：单音节前缀的同音单字动辄 20+ 条，
                // 不限就会吃光整个配额，把 ① 精确整词和 ③ 前缀补全挤出去。
                for h in self
                    .lookup_with_fuzzy(&code, &syllables[..end])
                    .into_iter()
                    .take(MAX_FULL_PINYIN_SUBPHRASE)
                {
                    let c = code.clone();
                    let n = c.len();
                    push(
                        cands, h.text, c, h.weight, h.order, h.boundary, false, n, h.is_fuzzy,
                    );
                }
            }
        }

        // ③ 前缀补全（码比击键长，`niha` → 「你好」）：以**整串击键**为前缀，含尾部残码。
        //
        // ⚠️ 必须与主路径 step 4 同口径地过 `completion_syllable_cap` —— 本支路一度直接用
        // 不带 cap 的 `search_prefix_with_boundary`，于是 `[schema.pinyin.completion]` 的两个
        // 旋钮对它**完全失效**：实测 `beijingd`（started 3、出厂 min_syllables=4 ⇒ 上限应
        // 收紧到 3）照样召回「北京大学」「北京地区」乃至 **7 音节的「北京大学出版社」**，
        // 而纯拼音主路径同输入下一条都不给。用户抱怨的「候选里全是我没打的音节」在这条
        // 支路上原样复现。
        let cap = completion_syllable_cap(
            started,
            self.config.completion_min_syllables,
            self.config.completion_max_extra_syllables,
        );
        for h in
            dict.search_prefix_with_boundary_syllable_capped(stroke, MAX_FULL_PINYIN_RECALL, cap)
        {
            push(
                cands,
                h.text,
                h.code,
                h.weight,
                h.order,
                h.boundary,
                true,
                stroke.len(),
                false,
            );
        }

        // ④ 用户/临时造词层：用户加过的词，换全拼打同样该出得来。
        if let Some(store_dm) = &self.store_layers {
            for c in store_dm.search(&completed, MAX_FULL_PINYIN_RECALL) {
                let n = completed.len();
                push(
                    cands,
                    c.text,
                    completed.clone(),
                    c.weight,
                    c.natural_order,
                    c.boundary,
                    false,
                    n,
                    false,
                );
            }
            for c in store_dm.search_prefix(stroke, MAX_FULL_PINYIN_RECALL) {
                // 记下文本，供末尾施加**用户词专属**的上浮判据。
                //
                // 为什么用文本名单而不是索引区间：`push` 带同文去重，本批词若与 ③ 的系统
                // 词库补全同文，会被合并到 ③ 已占的位置上（在 `user_start` 之前），按区间
                // 切会漏掉那些。名单通常只有个位数条（用户词本就不多），`contains` 的开销
                // 可以忽略。
                user_prefix_texts.push(c.text.clone());
                push(
                    cands,
                    c.text,
                    c.code,
                    c.weight,
                    c.natural_order,
                    c.boundary,
                    true,
                    stroke.len(),
                    false,
                );
            }
        }

        // ⑤ **整句解码**：让全拼降级流也能组句（`wojintianhenkaixin` → 「我今天很开心」），
        //    否则「完整的全拼也能工作」只兑现了一半——词典里没有的搭配全部落空。
        //
        //    在 `completed`（完整音节覆盖段）上建图，对应主路径的 step 2；**不做 step 2c 的
        //    残码补全**：它让整句消费满整串从而跨过若干「消费整串」闸门，副作用面比收益大，
        //    降级通道不值得冒这个险（同一顾虑使混输的码长内也关着它，见 `ConvertOptions`）。
        //
        //    切分图走 `from_dag`（多路径）而非 `from_syllables`：全拼的切分是**猜的**，词图该
        //    看到全部切法——这正是它与双拼主路径 `fixed_segmentation` 的分野，也是当初判定
        //    「不能跑两遍 convert」的根由。
        //
        //    模糊音传 `None`：与本支路其余部分一致，降级通道不做二次放大。
        if self.config.use_smart_compose && syllables.len() >= 2 {
            let trie = &self.trie;
            let seg_graph = SegGraph::from_dag(&Dag::build(&completed, trie));
            // 模糊音同 ①②：与主路径 step 2 一致地把 fuzzy_config 传进词图。
            let lattice_nodes = self.lattice_builder.build(
                &completed,
                &seg_graph,
                dict,
                Some(&self.fuzzy_config),
                true,
            );
            let input_len = completed.len();
            let mut lattice: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
            for (end_pos, nodes_at_end) in lattice_nodes.iter().enumerate() {
                if end_pos > input_len {
                    continue;
                }
                for node in nodes_at_end {
                    lattice[end_pos].push(WordNode {
                        start: node.start,
                        end: node.end,
                        word: node.word.clone(),
                        syl_mask: node.syl_mask,
                        log_prob: node.log_prob,
                    });
                }
            }
            let result = self.viterbi.decode(&lattice, input_len);
            let sentence: String = result.words.join("");
            let logp_per_char = result.log_prob / sentence.chars().count().max(1) as f64;
            // 三道闸门，逐条复刻主路径 step 2 / 2b 的既有判据：
            // - `log_prob.is_finite()`：解码失败时为 NEG_INFINITY，不能把空/错误路径塞进候选；
            // - `words.len() >= 2`：**单节点路径不算组句**。它本质就是一条普通词候选，而整句的
            //   `code` 是击键串、普通候选的 `code` 是词典码——包装成整句会让同一个词的词频记到
            //   两个互不相认的键上（step 2b 注释记着这个坑）。单节点交给 ① 出即可，那边 code 是对的；
            // - `logp_per_char`：低置信整句宁可不出。降级通道尤其如此——出一条烂整句的代价
            //   比不出高得多。
            if result.log_prob.is_finite()
                && result.words.len() >= 2
                && !sentence.is_empty()
                && logp_per_char >= MIXED_SENTENCE_MIN_LOGP_PER_CHAR
                && !cands.iter().any(|c| c.text == sentence)
            {
                // **整句让位于精确整词**，语义同主路径 6.5，但判据必须自带一份：6.5/6.5b 跑在
                // 双拼边界校验**之前**，而本支路在其之后（见 convert 里 step 6.7 的位置说明），
                // 那套逻辑根本够不着这批候选。比较范围限于 `cands[start..]`（本支路自己的产出）
                // ——跨层去跟双拼候选比权重没有意义，层级键已经把两批彻底分开了。
                let exact_max = cands[start..]
                    .iter()
                    .filter(|c| !c.is_prefix && !c.is_partial && c.code == completed)
                    .map(|c| c.weight)
                    .max();
                let w = sentence_weight(result.log_prob, result.words.len());
                let weight = exact_max.map_or(w, |m| w.min(m.saturating_sub(1)));
                cands.push(Candidate {
                    text: sentence,
                    // 码取**已完成音节段**（不含尾部残码），与 consumed 一致：`nihaom` 选整句
                    // 后残码 `m` 留在缓冲里续输，与主路径 step 2 同款。
                    code: completed.clone(),
                    weight,
                    natural_order: 0,
                    source: CandidateSource::Pinyin,
                    is_fullpinyin_fallback: true,
                    is_sentence: true,
                    // 新建整句 = 引擎合成的解读，词库无此词条（同文合并那三处刻意不设）。
                    is_synthesized: true,
                    is_partial: completed.len() < stroke.len(),
                    boundary: result.boundary,
                    consumed_length: completed.len(),
                    ..Default::default()
                });
            }
        }

        // 本支路产出的前缀补全统一补齐音节数对齐档位。放在末尾一次做完，而不是散在 ③④
        // 各自的 push 旁边：push 闭包带同文去重（可能替换已在表里的那条），逐条回填要跟
        // 去重逻辑纠缠，而本项只依赖 `boundary` 与 `started`，与谁 push 的无关。
        //
        // 漏设的后果：4 音节的「北京大学」与 3 音节的「北京的」在 `beijingd` 下同档竞争
        // （两者 extra 都是 0），协调器的 `cmp_completion_extra` 形同虚设。
        //
        // ⚠️ **上浮判据必须按来源分流，不能对整批统一施加**。`cands[start..]` 混着 ③ 的
        // 系统词库补全与 ④ 的用户/临时词，两者判据不是一套：系统词走
        // `COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES` + `COMPLETION_FAR_WEIGHT_FLOOR`，
        // 用户词走 `should_promote_user_completion`。统一施加任何一套都会误伤另一批 ——
        // 本仓已有一次两套判据因数值巧合被混用、连带打断 `qingfengshu`→「清风输入法」
        // 的前科。故这里只对 `user_prefix_texts` 点名的那批施加用户词判据。
        //
        // ## 用户词不上浮的后果：在本支路是「完全打不出」
        //
        // 双拼方案下主路径把 `qingfengshurufa` 当**双拼码**解释（每 2 键一音节，切出来是
        // 另一串音节），根本命中不到这条用户词 —— **本支路是它唯一的产出通道**，主路径
        // step 6 那道 `should_promote_user_completion` 在此场景下从未被执行到。
        //
        // 实测 11 音节用户词、`max_extra=10`、打 `qingfengshurufa`（距词尾 6）：
        // 不上浮时它落在 **603/604 位**，而协调器传的 limit 恒为 300 ⇒ 被 `truncate` 丢弃
        // ⇒ 用户看到的是「这个词根本打不出来」。纯拼音方案同输入是位次 1。
        // 与 `should_promote_user_completion` 文档里那次报障是同一病灶的另一条通道。
        for c in &mut cands[start..] {
            if !c.is_prefix {
                continue;
            }
            let word_syls = c.boundary.count_ones();
            if word_syls == 0 {
                continue; // 无边界信息：算不出，保持默认 0（同主路径对 boundary==0 的处置）
            }
            c.completion_extra_syllables =
                word_syls.saturating_sub(started).min(u8::MAX as u32) as u8;
            if !c.is_promoted_completion
                && user_prefix_texts.contains(&c.text)
                && should_promote_user_completion(
                    syllables.len(),
                    trailing_partial,
                    c.boundary,
                    self.config.completion_max_extra_syllables,
                )
            {
                c.is_promoted_completion = true;
            }
        }
    }

    /// 带模糊拼音扩展的词库查找（对齐 Go lookupWithFuzzy）。
    /// `code` 为待查询的全拼码（整串或前缀子码）；`syllables` 为该码对应的音节切分，
    /// 用于生成模糊变体。返回与 `dict.search` 相同的 `(text, weight, order)`。
    ///
    /// fuzzy 全 false 时 fuzzy_variants 返回空 → 天然退化为纯 `dict.search`（无需 enabled 判断）。
    /// 返回 `(text, weight, order, is_fuzzy)`：原 code 精确命中 is_fuzzy=false；
    /// 模糊变体命中 is_fuzzy=true（供排序时整体降到精确候选之后）。
    fn lookup_with_fuzzy(&self, code: &str, syllables: &[String]) -> Vec<LookupHit> {
        // 精确匹配：候选码即查询码 `code`，故词典 boundary 与之同域，可直接采信。
        // 注意此处必须用 search_with_boundary——拼音引擎直接持有 CachedDict、不经
        // SystemDictLayer，用 search() 会把边界丢在这里。
        let mut results: Vec<LookupHit> = self
            .dict
            .search_with_boundary(code)
            .into_iter()
            .map(|h| LookupHit {
                text: h.text,
                weight: h.weight,
                order: h.order,
                is_fuzzy: false,
                boundary: h.boundary,
            })
            .collect();
        let mut seen: std::collections::HashSet<String> =
            results.iter().map(|h| h.text.clone()).collect();

        // 模糊变体命中一律 boundary=0（不设防）：词典给的是**变体码**（如 zhongguo）的切分，
        // 而候选对外的 code 是用户实际输入的原码（zongguo）——两者不同域，位偏移对不上，
        // 直接采信会错位误杀。模糊音本就是放宽匹配，不校验边界是合理的。
        if syllables.len() <= 1 {
            // 单音节：对该音节（无切分时退化为整码）生成变体逐个查询。
            let syllable: &str = if syllables.len() == 1 {
                &syllables[0]
            } else {
                code
            };
            for (variant, edits) in
                fuzzy::FuzzyMatcher::fuzzy_variants_scored(syllable, &self.fuzzy_config)
            {
                for (text, weight, order) in self.dict.search(&variant) {
                    if seen.insert(text.clone()) {
                        results.push(LookupHit {
                            text,
                            // 单音节也可能声母、韵母同时模糊（`sen`→`sheng` 计 2 处），
                            // 故按变体自带的改动处数罚，不再恒当 1 处。
                            weight: fuzzy_penalized(weight, edits),
                            order,
                            is_fuzzy: true,
                            boundary: 0,
                        });
                    }
                }
            }
        } else {
            // 多音节：笛卡尔积展开各音节变体，拼成完整 altCode 查询。
            for (alt_code, fuzzy_count) in self.expand_code(syllables) {
                if alt_code == code {
                    continue;
                }
                for (text, weight, order) in self.dict.search(&alt_code) {
                    if seen.insert(text.clone()) {
                        results.push(LookupHit {
                            text,
                            weight: fuzzy_penalized(weight, fuzzy_count),
                            order,
                            is_fuzzy: true,
                            boundary: 0,
                        });
                    }
                }
            }
        }

        results
    }

    /// 对多音节做模糊变体笛卡尔积展开（对齐 Go `FuzzyConfig.ExpandCode`）。
    ///
    /// 实现收口在 [`fuzzy::FuzzyMatcher::expand_syllables`]，与 `lattice.rs` 的整句路径共用
    /// **同一份**逐音节展开逻辑——两处曾各写一套，且 lattice 那套对整串求变体，非首音节的
    /// 模糊永远命中不了（见该函数文档）。
    /// 返回 `(变体码, 被模糊的音节数)`——第二项供 [`fuzzy_penalized`] 按音节数累乘折扣。
    fn expand_code(&self, syllables: &[String]) -> Vec<(String, usize)> {
        fuzzy::FuzzyMatcher::expand_syllables(syllables, &self.fuzzy_config)
    }
}

/// 候选码 `code` 是否恰好落在前 k 个音节的边界上（`syllables[..k].join("") == code`）。
/// 命中返回 `Some(k)`（k>=1）；不落任何边界（如前缀补全的超长码）返回 `None`。
/// 供手动分隔符边界过滤：判断候选字数是否与所跨音节数一致。
fn syllable_span(syllables: &[String], code: &str) -> Option<usize> {
    if code.is_empty() {
        return None;
    }
    let mut acc = String::new();
    for (i, s) in syllables.iter().enumerate() {
        acc.push_str(s);
        if acc.len() > code.len() {
            break;
        }
        if acc == code {
            return Some(i + 1);
        }
    }
    None
}

/// 词典查询命中（含音节边界），供 `lookup_with_fuzzy` 返回。
struct LookupHit {
    text: String,
    weight: i32,
    order: i32,
    is_fuzzy: bool,
    /// 该候选 code 的音节边界；0=无信息（模糊变体/非拼音词库/旧数据），不参与校验。
    boundary: u64,
}

/// 按边界 bitmask 渲染 preedit：`code` 以 `'` 在各音节起点断开，尾部残码另起一段。
/// 供「预编辑区跟随首选候选」使用（见 `convert`）。
fn render_preedit(code: &str, boundary: u64, partial: &str) -> String {
    let mut out = String::with_capacity(code.len() + 8);
    for (i, ch) in code.char_indices() {
        if i > 0 && i < 64 && (boundary >> i) & 1 == 1 {
            out.push('\'');
        }
        out.push(ch);
    }
    if !partial.is_empty() {
        if !out.is_empty() {
            out.push('\'');
        }
        out.push_str(partial);
    }
    out
}

/// 由音节列表算边界 bitmask（全拼空间），只取覆盖前 `limit_len` 字节的部分。
///
/// 用于 **DAG 切分出来的**候选（Viterbi 整句、前缀子短语）——它们的 code 是把
/// `syllables` 拼起来的，故其"边界"就是这份切分本身。这与词典真值边界同域、可直接比对：
/// 双拼 `nihao` 被 DAG 重切成 `ni|hao` 拼出「你好」时，标上 DAG 的切分，正好会被
/// 双拼真值 `ni|ha|o` 拒掉——这正是我们要的。
fn syllables_boundary_mask(syllables: &[String], limit_len: usize) -> u64 {
    let mut mask = 0u64;
    let mut pos = 0usize;
    for s in syllables {
        if pos >= limit_len {
            break;
        }
        if pos >= 64 {
            return 0;
        }
        mask |= 1u64 << pos;
        pos += s.len();
    }
    mask
}

/// 候选的音节边界，`boundary` 无真值（=0）时用 DAG 对码现切一次补出。
///
/// 无真值的来源：用户手输码（`infer_boundary_for` 兜不住的那部分）、模糊变体（一律置 0，
/// 见 P2b 的候选边界归属表）、旧词典条目。现切是**猜测**，但这里本就没有真值可用——
/// 与「凡拿 flat 码现算音节 = 把已知真值扔掉重猜」那条戒律不冲突：那条针对的是
/// 手上有真值却不用，此处是真的没有。
///
/// ⚠️ 猜测走 `maximum_match`（最长匹配 ⇒ **最少**音节），故对同码多切分会偏向少的那侧：
/// `xian` 猜成 1 音节，若某条 boundary=0 的候选实为「西安」(xi|an)，会被少算一个音节。
/// 只影响没有边界真值的那一小撮，且仅在短输入档生效。
fn effective_boundary(code: &str, boundary: u64, trie: &SyllableTrie) -> u64 {
    if boundary != 0 {
        return boundary;
    }
    let syls = Dag::build(code, trie).maximum_match();
    syllables_boundary_mask(&syls, code.len())
}

/// 双拼给出的**分段边界**（全拼空间 bitmask，与候选 `boundary` 同域）。
///
/// 双拼每 2 键 = 1 段，边界免费且精确——这正是双拼相对全拼的信息优势，此前却被拼成
/// `full_pinyin` 后交给 DAG 重猜。
///
/// **回写段也算一个段起点**。`convert` 拼不出合法音节时会把两个键原样写进 full
/// （注释所谓「简拼/无效键对」）且不产生 `SylSpan`——但它照样**占据 full 的一段**、
/// 用户也确实是当一个单元敲的，故它的起点同样是真值。曾以为这类段"无从表达"而让整个
/// mask 作废（返回 0 = 不校验），结果 `nihaoya` 的「你好呀」从 step4 前缀补全漏网：
/// 校验一关，全拼命中就畅通无阻。给回写段标上起点后，`ni|ha|oy…` = {0,2,4} 与词典的
/// `ni|hao|ya` = {0,2,5} 自然不符，拒绝生效。
///
/// 返回 0 仅表示无可用信息（空输入 / 越出 64 位表达范围）。
fn sp_boundary_mask(sp: &shuangpin::SpConvertResult) -> u64 {
    let mut mask = 0u64;
    let mut cursor = 0usize;
    let mark = |pos: usize, mask: &mut u64| -> bool {
        if pos >= 64 {
            return false;
        }
        *mask |= 1u64 << pos;
        true
    };
    for s in &sp.syllables {
        // 音节之前的空隙 = 回写段（如 `omni` 的 om），其起点同样是段边界。
        if s.fp_start > cursor && !mark(cursor, &mut mask) {
            return 0;
        }
        if !mark(s.fp_start, &mut mask) {
            return 0;
        }
        cursor = s.fp_end;
    }
    // 尾部剩余：partial 声母（nihao 的 o）或回写段（nihaoya 的 oy+a）——两者都开一个新段。
    // 注：回写段内部可能不止一段（每 2 键一段），但其细分无从得知；只标首个起点即可，
    // 已足以让「跨越该点的词典切分」失配。
    if cursor < sp.full_pinyin.len() && !mark(cursor, &mut mask) {
        return 0;
    }
    mask
}

/// 候选的音节切分是否与双拼解释相容。
///
/// 双拼定死了每个音节的边界，候选（词典真值边界）必须与之吻合，否则它根本不是用户打的那串音。
/// 典型：输入 `nihao`(5键) 双拼解释为 `ni|ha|o`，而「你好」的词典边界是 `ni|hao`——两者不符，
/// 「你好」应被拒绝（该词的正确双拼是 4 键）。此前因边界信息全丢，只能靠 DAG 把
/// `nihao` 重新切成 `ni|hao`，于是 5 键也能出「你好」。
///
/// 比较窗口取 `min(候选 code 长, 全拼串长)`：
/// - 候选码更短（子短语，如 `ni`→「你」）→ 只比其覆盖的前缀范围；
/// - 候选码更长（前缀补全，输入 `ni` 补出「你好」`nihao`）→ 只比已输入的部分，
///   补全部分尚未键入、无从校验。
///
/// 任一侧无边界信息（0）即放行——降级回原有 DAG 行为，不误杀。
fn boundary_compatible(cand_boundary: u64, sp_mask: u64, code_len: usize, full_len: usize) -> bool {
    if cand_boundary == 0 || sp_mask == 0 {
        return true; // 无信息 → 不设防（用户手输码/五笔/超长码/含回写段）
    }
    let win = code_len.min(full_len);
    if win == 0 {
        return true;
    }
    let win_mask = if win >= 64 {
        u64::MAX
    } else {
        (1u64 << win) - 1
    };
    cand_boundary & win_mask == sp_mask & win_mask
}

/// Fix A：用双拼原始按键重建 preedit（按音节边界以 `'` 分隔）。
///
/// **必须完整覆盖 `raw_input` 的每个字节**：已完成音节取其 `[raw_start, raw_end)`，音节之间与尾部
/// 未被任何音节覆盖的字节原样作独立段。不可只在 `has_partial` 时补尾——无匹配键对（`convert`
/// 的 else 分支「原样回写」，如首道双拼的 `om`）既不进 `syllables` 也不置 `has_partial`，
/// 早期实现据此判尾会把它们静默吞掉：`nihaom` → `ni'ha`（om 消失）、再按 `a` 又诡异复现。
/// 分隔符与全拼自动分词一致用 `'`。双拼键均为 ASCII，字节切片安全。
fn build_raw_preedit(raw_input: &str, sp: &shuangpin::SpConvertResult) -> String {
    if raw_input.is_empty() {
        return String::new();
    }

    // 段起点集合：每个音节的 raw_start，以及音节之间／尾部未被覆盖段的起点
    //（无匹配键对的原样回写、partial 尾键）。
    let mut starts: Vec<usize> = Vec::new();
    let mut cursor = 0usize;
    for s in &sp.syllables {
        if s.raw_start > cursor {
            starts.push(cursor);
        }
        starts.push(s.raw_start);
        cursor = s.raw_end;
    }
    if cursor < raw_input.len() {
        starts.push(cursor);
    }

    // ★ 逐字节重建而非 `join("'")`：raw_input 里**可能已经有用户手打的 `'`**，join 会在它
    // 旁边再插一个（`n'hc` → `n''hc`）。判据是「这一位输出前，前面是不是已经有分隔符了」
    // ——手动的和自动的都算，于是两者天然合流，连续 `''` 与首尾 `'` 也都原样留住。
    let bytes = raw_input.as_bytes();
    let mut out = String::with_capacity(raw_input.len() + starts.len());
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'\'' {
            out.push('\'');
            continue;
        }
        if starts.contains(&i) && !out.is_empty() && !out.ends_with('\'') {
            out.push('\'');
        }
        out.push(c as char);
    }
    out
}

impl Engine for PinyinEngine {
    /// 双拼布局带非字母键时返回该布局的码元集，否则 `None`（回落内置 `a-z`）。
    /// 拼音没有「码表码元」那层配置，本集**完全由双拼布局推导**，不读 `[engine.codetable]`。
    fn input_chars(&self) -> Option<&wind_config::CodeCharSet> {
        self.input_chars.as_ref()
    }

    fn convert(&self, input: &str, max_candidates: usize) -> anyhow::Result<ConvertResult> {
        self.convert_with_opts(input, max_candidates, ConvertOptions::default())
    }

    /// 拼音引擎的**唯一**转换实现（`convert` 转发至此，传全默认的 [`ConvertOptions`]）。
    /// 两项覆写各自的落点：`require_full_match` 在排序截断**之前**丢弃未消费整串的候选；
    /// `allow_partial_final` 覆写 step 2c 的入口条件。判据说明见 [`ConvertOptions`]。
    fn convert_with_opts(
        &self,
        input: &str,
        max_candidates: usize,
        opts: ConvertOptions,
    ) -> anyhow::Result<ConvertResult> {
        let require_full_match = opts.require_full_match;
        // 未覆写时用引擎自身配置（纯拼音方案即走这条）。
        let allow_partial_final = opts
            .allow_partial_final
            .unwrap_or(self.config.enable_partial_final);
        if input.is_empty() {
            return Ok(ConvertResult::default());
        }

        // 手动音节分隔符 `'` 在双拼下同样生效：由 `ShuangpinConverter::convert` 当作配对的
        // 硬边界消化（见那里的文档），到这里之后 `input` 已是纯全拼域、不含 `'`，
        // 下面全拼路径的 `has_sep` 分支只对 shuangpin=None 的输入成立。

        // Fix A：在任何 shadow 之前保存用户实际输入的原始字符（双拼键序列或全拼）。
        // 用于重建 preedit_display（显示原始按键），以及简拼判定（见 `abbr_query`）。
        let raw_input = input;

        // 简拼判定与比对的基准串：**原始击键**，不是转换后的全拼。
        //
        // 简拼的定义是「每个字母取一个音节的声母」，这件事只跟用户敲下的字母有关，与双拼
        // 编码方案无关。而下面 `input` 会被双拼转换结果覆盖、`query` 由它派生——双拼下打
        // `xan` 得到的是「某音节 + partial 声母」，拿它去判简拼永远匹配不到用户实际敲的
        // `xan`，用户词「西安宁」的简拼在双拼下因此完全不可达。
        //
        // 全拼下 `raw_input == query`，故此改动对全拼零影响。简拼串不含 `'`
        //（`is_abbreviation` 对分隔符判假），故与手动分隔符路径也不冲突。
        let abbr_query = raw_input;

        // 混合简拼（声母段 + 音节段）枚举模式用的串。**与 `abbr_query` 分家**：
        // 纯简拼判据问的是「这串击键是不是一串声母」，只有击键域答得了（`xan`）；
        // 混合模式问的是「哪几段是音节」，击键域在双拼下根本没有音节可言。
        //
        // 仅当双拼**且用户打了分隔符**时改用全拼域——那时段结构由 `'` 定死，
        // `full_pinyin` 是唯一解释，不存在拿它去猜的风险。没有分隔符时维持原样，
        // 双拼既有行为（靠击键串碰巧在全拼域可读）一字不动。
        let use_fp_for_mixed = self.shuangpin.is_some() && raw_input.contains('\'');

        // 双拼激活时保留 SpConvertResult，以便后续用 map_consumed_length 回算消费键数。
        let sp_result: Option<shuangpin::SpConvertResult> =
            self.shuangpin.as_ref().map(|conv| conv.convert(input));
        let full_owned: String = match &sp_result {
            Some(r) if !r.full_pinyin.is_empty() => r.full_pinyin.clone(),
            Some(_) => input.to_string(),
            None => input.to_string(),
        };
        let input = full_owned.as_str();

        // 手动音节分隔符 `'` 支持（全拼路径）：
        // - `has_sep`：输入含手动分隔符，走边界感知分词 + 剥除查询。
        // - `query`：剥除 `'` 后的纯拼音串（词典查询用）；音节边界信息来自带 `'` 的分段。
        let has_sep = input.contains('\'');
        let query_owned: String = if has_sep {
            input.chars().filter(|&c| c != '\'').collect()
        } else {
            String::new()
        };
        let query: &str = if has_sep { query_owned.as_str() } else { input };

        // 见上面 `use_fp_for_mixed`：此处 `input` 已是全拼域（双拼转换后的结果）。
        let mixed_pattern_source: &str = if use_fp_for_mixed { input } else { abbr_query };

        // 纯分隔符输入（如 `'` / `''`）：无可查询拼音，仅回显分隔符，不产候选。
        if has_sep && query.is_empty() {
            let (preedit, _, _) = self.compute_composition(input);
            return Ok(ConvertResult {
                preedit_pinyin: preedit.clone(),
                preedit_display: preedit,
                is_empty: true,
                ..Default::default()
            });
        }

        let dict = &self.dict;
        let trie = &self.trie;
        let mut candidates: Vec<Candidate> = Vec::new();

        let push_unique = |cands: &mut Vec<Candidate>,
                           text: String,
                           code: String,
                           weight: i32,
                           order: i32,
                           is_fuzzy: bool,
                           is_prefix: bool,
                           boundary: u64,
                           is_promoted: bool| {
            if text.is_empty() || cands.iter().any(|c| c.text == text) {
                return;
            }
            // 子短语候选：code 是输入的真前缀（比输入短，如 baoan 的「报」(bao)）。
            // Viterbi 整句走 insert(0) 不经此闭包，故无需 weight 启发式即可排除整句。
            // 注：以剥除分隔符后的 query 为基准（无分隔符时 query==input，行为不变）。
            let is_partial = !is_prefix && code.len() < query.len() && query.starts_with(&code);
            cands.push(Candidate {
                text,
                code,
                weight,
                natural_order: order,
                source: CandidateSource::Pinyin,
                is_fuzzy,
                is_prefix,
                is_partial,
                boundary,
                is_promoted_completion: is_promoted,
                ..Default::default()
            });
        };

        // DAG 分词提前到 step1 之前：lookup_with_fuzzy 需要音节列表生成模糊变体。
        // 含手动分隔符时按 `'` 分段独立分词（`'` 为硬边界，音节不得跨越），否则整串 DAG。
        //
        // **双拼激活时用双拼自己的真值切分**，不让 DAG 对拼平后的 full_pinyin 重猜——
        // 双拼每 2 键 = 1 音节，边界免费且精确。让 DAG 重猜会造成「查询按猜测、校验按真值」
        // 两套切分打架：`hao`(3键) 双拼解释为 ha|o，DAG 却重切成 [hao] 只查了「好」，
        // 随后被真值拒掉，而真正该查的 `ha`（→「哈」）压根没查 → 候选全空。
        // 双拼激活：取**从 0 起连续覆盖**的音节前缀，遇断裂即止。
        //
        // 断裂 = 「无匹配键对原样回写」段（convert 的 else 分支，如 `oy`——o 非声母、拼不出
        // 音节）。它没有 SylSpan，其后音节的 fp 偏移也已被它污染，故断裂处之后
        // **不解释**：那本就是用户打错的键，不该产生候选。
        //
        // 不可整串退回 DAG——那等于「打错一个键对反而解锁全拼」，与 nihao(5键) 不出「你好」
        // 自相矛盾。注释里「简拼/无效键对」的**简拼**那半由 AbbrevMatcher 兜底，它走 query、
        // 不看音节切分，本就不需要 DAG（见 shuangpin_writeback_keeps_abbrev_input_intact）。
        //
        // 尾部 partial（未完成音节的声母）不是完成音节，不计入——由 step4 前缀补全承接。
        let sp_syllables: Option<Vec<String>> = sp_result.as_ref().map(|r| {
            let mut v = Vec::with_capacity(r.syllables.len());
            let mut cursor = 0usize;
            for s in &r.syllables {
                if s.fp_start != cursor {
                    break; // 断裂：其后 fp 偏移不可信，停止解释
                }
                v.push(s.pinyin.clone());
                cursor = s.fp_end;
            }
            v
        });
        // `fixed_segmentation` = 切分是**真值**、只有一条（双拼每 2 键 1 音节；`'` 是硬边界），
        // 词图必须照单全收，绝不可让 DAG 重猜。全拼则相反：切分是猜的，词图应看到**全部**
        // 候选切法（见 lattice::LatticeBuilder::build）。
        let fixed_segmentation = sp_syllables.is_some() || has_sep;
        // 带分隔符时保留 span 序列：下方 consumed_length 要靠它回映射到含 `'` 的原始空间，
        // 与双拼的 map_consumed_length 共用同一套 SylSpan 表示（见 interp 模块文档）。
        let sep_spans = if has_sep {
            self.spans_with_separators(input)
        } else {
            Vec::new()
        };
        let syllables = if let Some(v) = sp_syllables {
            v
        } else if has_sep {
            sep_spans.iter().map(|s| s.pinyin.clone()).collect()
        } else {
            Dag::build(input, trie).maximum_match()
        };

        // 完成音节覆盖的连续前缀（从起点算）。
        //
        // **多路径切分下这个值依然唯一确定**，无须在多条路径间做选择：所有切分路径都从 0
        // 连续覆盖，故「覆盖长度」恒等于「路径终点」，而 `maximum_match` 取的正是**最远
        // 可达位置**——该位置是图的性质，与走哪条路径无关。于是 `completed_len` /
        // `consumed_length`（分段上屏字节数）保持单一确定值，多路径只影响词图**内部**
        // 查哪些跨度，不影响引擎对外承诺消费多少输入。
        //
        // 尾部不成音节的残码（如「nihaom」的「m」）
        // 不参与精确匹配/整句解码——否则 lattice 到不了残码末端、Viterbi 失败、整句退化成单字，
        // 且精确层会把「nihao」当模糊变体误标 is_fuzzy 沉底被截断（bug①）。
        let completed_len: usize = syllables.iter().map(|s| s.len()).sum();
        // 含分隔符时用音节直接拼接（避免 `'` 字节位错位）；无分隔符时 query==input，等价原切片。
        let completed_owned: String;
        let completed: &str = if has_sep {
            completed_owned = syllables.join("");
            &completed_owned
        } else {
            &query[..completed_len]
        };

        // 1. 精确查找（完整匹配，含模糊扩展，对齐 Go lookupWithFuzzy）。
        //    以 completed（完成音节前缀）而非 query（可能含尾部残码）为查询码与存储 code：
        //    残码存在时（nihaom）query 非合法音节序列，search(query) 为空，而 lookup_with_fuzzy
        //    的 expand_code「全原音节」组合会命中 completed 的精确匹配——但因 `alt_code == code`
        //    守卫按 query 比较而失配，被误标 is_fuzzy=true 沉底、遭 truncate 截断（bug①）。
        //    传 completed 后守卫正确跳过全原组合（精确匹配 is_fuzzy=false）；code 存 completed 使
        //    残码输入的 consumed_length 只覆盖完成音节（nihao 消费 5 留 m 续输）。
        for h in self.lookup_with_fuzzy(completed, &syllables) {
            push_unique(
                &mut candidates,
                h.text,
                completed.to_string(),
                h.weight,
                h.order,
                h.is_fuzzy,
                false,
                h.boundary,
                false,
            );
        }

        // （step 1.5「超长词典整词兜底」已删除。它把音节数超过 `max_word_len` 的词典精确
        //  整词按 `score_node` 折算成「单节点等价整句分」并抬到整句量纲，同时标 `is_sentence`
        //  —— 因为旧整句拿 3e7 基座，词典精确命中只带原始词频（「中华人民共和国」3113），
        //  在同一个 weight 维度上必输，哪怕整句是语义碎片拼出的错误切分。
        //
        //  整句退役 3e7、改用等效词频后，这段成了 no-op：`sentence_weight(log_prob, 1)` 对
        //  单词整句还原成 `f × exp(各类惩罚) < f`，`max` 恒取原权重。而排序结果之所以不变，
        //  是因为错误拼接整句的 W_eff 本就极低（一串低频字的乘积趋近 clamp 下限 1），词典
        //  整词的真实词频天然压过它 —— 这正是同量纲要达到的效果，无需再手工抬权。
        //
        //  `pinyin_long_word::test_over_limit_long_word_falls_back_to_dict_exact` 的 5 个用例
        //  当年是按「依赖本段」挑的，删除后仍全绿，即为此事的实证。）

        // Viterbi **新合成**的整句文本（词典里没有这个词，只能由多个节点拼出来）。
        // 与词典整词同文而被合并的那一支不记入——它本身就是精确整词，不存在「让位」问题。
        // 供 step 6.5 的降级判定使用（须等 step 6 并入用户/临时层后才能定夺）。
        let mut synthesized_sentence: Option<String> = None;

        // 整串是否已被完整音节覆盖。**两条简拼路径共用**：step 2b 的混合整句与 step 5b/6.2
        // 的混合简拼都只在「输入里有成不了音节的字母」时才该启动，纯全拼一律不碰。
        //
        // 全拼下比较 `completed_len` 与击键长度；双拼下 `completed_len` 说的是转换后的全拼域，
        // 与 `abbr_query`（原始击键）不同域，故改判「转换结果覆盖了整串击键」。
        //
        // ★ 双拼这边问的是「**有没有**击键没被音节覆盖」，不是「最后一个音节到没到末尾」。
        // 后者隐含「音节连续铺满」的假设——手动分隔符第一次让中间合法地出现空洞
        //（`n'hc` 的 `n` 是简拼段，不进 `syllables`），只看末尾就会把「整串已覆盖」
        // 误判成真、把混合简拼整条路短路掉，候选一条不剩。
        // 同一个洞在开头时（`omni` 的回写段 `om`）旧判据也一样漏，只是此前没有场景踩到。
        //
        // `'` 自身不是待解释的击键，空洞里只有分隔符不算漏。
        let mixed_covered = match &sp_result {
            Some(r) => {
                let unexplained = |seg: &str| seg.bytes().any(|c| c != b'\'');
                let mut cursor = 0usize;
                let mut covered = true;
                for s in &r.syllables {
                    if unexplained(&raw_input[cursor..s.raw_start]) {
                        covered = false;
                        break;
                    }
                    cursor = s.raw_end;
                }
                covered && !unexplained(&raw_input[cursor..])
            }
            None => completed_len >= abbr_query.len(),
        };

        // 2. Viterbi 长句解码（>=2 音节，仅在完成音节前缀上跑；use_smart_compose=false 时跳过）
        if self.config.use_smart_compose && syllables.len() >= 2 {
            // 切分图：全拼取 DAG 的全部路径；双拼/手动分隔符取真值链（行为与改造前一致）。
            let seg_graph = if fixed_segmentation {
                SegGraph::from_syllables(&syllables)
            } else {
                SegGraph::from_dag(&Dag::build(completed, trie))
            };
            let lattice_nodes = self.lattice_builder.build(
                completed,
                &seg_graph,
                dict,
                Some(&self.fuzzy_config),
                true,
            );
            let input_len = completed.len();
            let mut lattice: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
            for (end_pos, nodes_at_end) in lattice_nodes.iter().enumerate() {
                if end_pos > input_len {
                    continue;
                }
                for node in nodes_at_end {
                    lattice[end_pos].push(WordNode {
                        start: node.start,
                        end: node.end,
                        word: node.word.clone(),
                        syl_mask: node.syl_mask,
                        log_prob: node.log_prob,
                    });
                }
            }
            let result = self.viterbi.decode(&lattice, input_len);
            // 仅接受有限概率的完整路径：解码失败时 log_prob 为 NEG_INFINITY，
            // 不能把这种空/错误路径强插到首选位置。
            if !result.words.is_empty() && result.log_prob.is_finite() {
                let sentence: String = result.words.join("");
                if !sentence.is_empty() {
                    // 整句优先：给予高权重置顶（log_prob 为负，原 .max(1) 会被截断淘汰）。
                    // clamp + saturating_add 防止超长低频句的 log_prob 溢出 i32 导致沉底/panic。
                    let weight = sentence_weight(result.log_prob, result.words.len());
                    if let Some(existing) = candidates.iter_mut().find(|c| c.text == sentence) {
                        // 整句与已有候选（如精确匹配 你好）同文：提升其权重置顶，
                        // 同时抹去 is_partial（step1 标了 true，但整句是完整解读并非子短语），
                        // 否则残码场景下 is_partial=true 会在排序时被 is_partial=false 的前缀补全
                        // （如「你好吗」）压下去——后者经 trailing_partial 优化也是 false。
                        existing.weight = existing.weight.max(weight);
                        existing.is_partial = false;
                        // 同文合并后它就是整句解本身，须继承整句身份，
                        // 否则 freq_rerank 会把它当普通候选而让别的整句锚定到它之上。
                        existing.is_sentence = true;
                    } else {
                        synthesized_sentence = Some(sentence.clone());
                        candidates.insert(
                            0,
                            Candidate {
                                text: sentence,
                                // 码为完成音节前缀（不含残码），使 consumed_length=completed_len，
                                // 整句上屏后残码留缓冲续输（你好m → 选你好留 m）。
                                code: completed.to_string(),
                                weight,
                                natural_order: 0,
                                source: CandidateSource::Pinyin,
                                is_sentence: true,
                                // 新建整句 = 引擎合成的解读，词库无此词条（同文合并那三处刻意不设）。
                                is_synthesized: true,
                                // 整句的边界 = 解码器**实际选中**的那条路径（多路径下同一串
                                // 输入可有多种切法，只有解码器知道走的是哪条）。回退到
                                // maximum_match 仅用于解码器给不出边界的极端情形（超 64 字节）。
                                boundary: if result.boundary != 0 {
                                    result.boundary
                                } else {
                                    syllables_boundary_mask(&syllables, completed.len())
                                },
                                ..Default::default()
                            },
                        );
                    }
                }
            }
        }

        // 2c. **残码补全整句解码**：尾部残码作为「待定音节」入图，由 LM 选出最优单字
        //     （`buzhidaok` → 「不知道」+ `k` 补成「看」→ 整句「不知道看」）。
        //
        //     ## 为什么必须另起一条路径而不是改 step 2
        //
        //     step 2 建图在 `completed` 上，`nodes` 数组长度 = `completed.len()+1`，**残码
        //     末端根本没有槽位**，Viterbi 到不了串尾（`completed_len` 处的注释记着这个约束：
        //     「否则 lattice 到不了残码末端、Viterbi 失败、整句退化成单字」）。本路径在含
        //     残码的 `query` 上重建图，槽位才够。
        //
        //     两条路径的产出**都保留**，它们是不同 `consumed_length` 层的候选：`nihaom` 既
        //     给「你好」(consumed=5，选它则残码 `m` 留缓冲续输)，也给残码整句(consumed=6)。
        //     协调器按消费长度优先排序，后者在前，但前者仍在候选中可选——这正是分步上屏
        //     依赖的行为，改成「用残码整句替换 step 2 结果」会破坏它。
        //
        //     ## 门槛
        //
        //     `syllables.len() >= 2`（与 step 2 一致）：至少两个完整音节才谈得上「组句」。
        //     `nim` 这类 1 音节 + 残码不走本路径——那种输入的正解是词库补全（你们/你没），
        //     残码整句「你吗」只会挤掉它。同 fcitx5 `partialLongWordLimit` 的精神：短输入
        //     不做激进的部分匹配。
        //
        //     双拼跳过（`sp_result.is_none()`）：`query` 是转换后的全拼、与击键不同域，
        //     残码的字节位在两个域里对不上。分隔符跳过（`!has_sep`）：`completed` 由音节
        //     `join` 得出，`completed_len` 与 `query` 的字节位不同源。
        //
        //     **混输的码长内跳过**（`allow_partial_final`，见 [`ConvertOptions`]）：那里的击键串
        //     同时是码表码，整串当拼音组句会抢走码表首位——真机 `aaw`（本意五笔 `aawt`→工作）
        //     首选变成「啊啊我」。★ 注意这不是「防线被绕过」而是**防线的前提被改掉**：
        //     `is_pinyin_exact_tier` 靠「拼音整句解释不了全部输入」把它挡在精确档外，step 2c
        //     让它真的消费满整串，那道判据便合法地放行了。加新的生成路径时，要回头查有哪些
        //     判据隐含假设了它不存在。
        //
        //     ⚠️ 判据是**「这串还可能是码表码吗」，不是「是不是混输」**：混输超码长（>码表最大
        //     码长）的串不可能是码，那里由调用方覆写为放行，否则 `zaiyebuj` 的尾字母 `j` 不参与
        //     组句、打不出「在也不就」，而纯拼音方案一直打得出。
        //
        //     ## 「1 个完整音节 + 残码」这一档为什么**延迟定夺**
        //
        //     本路径原本要求 `syllables.len() >= 2`，`zaim`/`zdm`（zai + 残码 m）够不着，
        //     正解「在吗」只能走词典补全 —— 而它在词库里 `w=0`，被一路降级到第 98 位。
        //     放开到 1 能让整句救回它，但**照原样立即插入会全面引入噪音**：实测 `wom`→「我吗」、
        //     `tam`→「他吗」、`nim`→「你吗」、`meiy`→「没也」全部挤进第 2 位，且多插的这一条
        //     还把 `meiy` 的单字「没」顶出了 400 条上限（它原本正卡在第 400 位）。
        //
        //     噪音与正解的分野要等 step 4 跑完才看得见（比的是「补全侧最强者有多弱」，
        //     见 [`SENTENCE_KEEP_RATIO`]），而本路径跑在 step 4 **之前**。故这一档先把整句
        //     存进 `short_sentence_pending`、**不进候选**，待 6.5b 之后再定夺：不够格的
        //     压根不出现，于是候选集与放开门槛前逐条一致，连截断边界都不动。
        //
        //     `completed_syls >= 2` 仍走原来的立即插入，行为逐字不变。
        let mut short_sentence_pending: Option<(String, i32, u64)> = None;
        if self.config.use_smart_compose
            && allow_partial_final
            // 原为 `syllables.len() >= 2`（至少两个完整音节才谈得上组句）。放开到 1 是为了
            // 让 `zaim`/`zdm` 这类「1 音节 + 残码」也能组句，其噪音由 6.5c 的延迟定夺挡住。
            && !syllables.is_empty()
            && completed_len < query.len()
            && !has_sep
        {
            let seg_graph = SegGraph::from_dag(&Dag::build(query, trie));
            let mut lattice_nodes =
                self.lattice_builder
                    .build(query, &seg_graph, dict, Some(&self.fuzzy_config), true);
            self.lattice_builder.add_partial_final_nodes(
                query,
                completed_len,
                dict,
                &mut lattice_nodes,
            );
            let full_len = query.len();
            let mut lattice: Vec<Vec<WordNode>> = vec![Vec::new(); full_len + 1];
            for (end_pos, nodes_at_end) in lattice_nodes.iter().enumerate() {
                if end_pos > full_len {
                    continue;
                }
                for node in nodes_at_end {
                    lattice[end_pos].push(WordNode {
                        start: node.start,
                        end: node.end,
                        word: node.word.clone(),
                        syl_mask: node.syl_mask,
                        log_prob: node.log_prob,
                    });
                }
            }
            let result = self.viterbi.decode(&lattice, full_len);
            if !result.words.is_empty() && result.log_prob.is_finite() {
                let sentence: String = result.words.join("");
                if !sentence.is_empty() {
                    let weight = sentence_weight(result.log_prob, result.words.len());
                    // 此处 `completed_syls`（step 4 才定义）尚不可见，用同源的 `syllables`
                    if syllables.len() as u32 <= SENTENCE_KEEP_MAX_COMPLETED_SYLS {
                        // 短上下文档：留到 6.5b 之后按 SENTENCE_KEEP_RATIO 定夺（见上方说明）
                        short_sentence_pending = Some((sentence, weight, result.boundary));
                    } else if let Some(existing) =
                        candidates.iter_mut().find(|c| c.text == sentence)
                    {
                        // 同文合并（`nihaom` 的残码整句「你好吗」与 step4 的前缀补全同文）：
                        // 取更强的身份，理由同 step 2 的同文合并分支。
                        existing.weight = existing.weight.max(weight);
                        existing.is_partial = false;
                        existing.is_sentence = true;
                    } else {
                        candidates.insert(
                            0,
                            Candidate {
                                text: sentence,
                                // 码取整串（含残码）⇒ `consumed_length = query.len()`：本路径
                                // 的整句**解释了全部输入**，这正是它区别于 step 2 结果之处。
                                code: query.to_string(),
                                weight,
                                natural_order: 0,
                                source: CandidateSource::Pinyin,
                                is_sentence: true,
                                // 新建整句 = 引擎合成的解读，词库无此词条（同文合并那三处刻意不设）。
                                is_synthesized: true,
                                boundary: result.boundary,
                                ..Default::default()
                            },
                        );
                    }
                }
            }
        }

        // 2b. **混合整句解码**：简拼段与全拼段在同一张词图里由 Viterbi 选路径
        //     （`bzdhaobuhao` → 「不知道」+「好不好」→ 整句「不知道好不好」）。
        //
        //     为什么不能复用 step 2：它的入口是 `syllables.len() >= 2`，而 `syllables` 是
        //     **从 0 起连续的完整音节覆盖**。`bzdhaobuhao` 的 DAG 从位置 0 就走不通（`b` 不
        //     成音节）⇒ `syllables` 为空、`completed` 为空串 ⇒ 整句解码压根不启动。step 2
        //     依赖 `completed` 来处理残码（`nihaom` 只在 `nihao` 上跑整句），不能改成整串，
        //     故这里另起一条在**整串**上建图的路径。
        //
        //     与 step 2 的三处差别：
        //     ① 在 `abbr_query`（整串击键）而非 `completed` 上建图；
        //     ② `require_reachable=false` —— 简拼段打断了音节图的可达性，而 `[3,6) hao`
        //        这些边其实都在图里，补上连接的是随后追加的简拼节点；
        //     ③ 追加 `add_abbrev_nodes`，跨度独立枚举（音节图给不出简拼段的终点）。
        //
        //     门槛：`!mixed_covered`（整串有成不了音节的字母）挡掉纯全拼输入——那种输入
        //     step 2 已经处理，再跑一遍只会重复建图并引入简拼噪音。双拼跳过：`input` 是
        //     转换后的全拼、与击键不同域，简拼判据会全部失配（文档 §5 约束 4）。
        if self.config.use_smart_compose
            && self.config.enable_abbrev
            && sp_result.is_none()
            && !has_sep
            // **未被音节覆盖的部分要够构成一个简拼段**，比 `!mixed_covered` 更严。
            // 尾部单字母残码是「还没打完」而不是简拼：`zhongguorenm` 的 `m` 也让
            // `!mixed_covered` 成立，整句便插到首位、把「中国人民解放军」挤后一格
            // （`pinyin_completion::test_useful_completions_still_float` 当场抓到）。
            // 这道闸门只加在 step 2b —— 只有它会抢首选；step 5b 的混合召回不受影响，
            // `nihaom` → 「你好吗」正该走混合模式。
            && abbr_query.len().saturating_sub(completed_len) >= MIN_ABBREV_STROKE
            && abbr_query.len() >= MIN_MIXED_SENTENCE_LEN
        {
            let graph = SegGraph::from_dag(&Dag::build(abbr_query, trie));
            let mut lattice_nodes = self.lattice_builder.build(
                abbr_query,
                &graph,
                dict,
                Some(&self.fuzzy_config),
                false,
            );
            self.lattice_builder
                .add_abbrev_nodes(abbr_query, dict, &mut lattice_nodes);

            let input_len = abbr_query.len();
            let mut lattice: Vec<Vec<WordNode>> = vec![Vec::new(); input_len + 1];
            for (end_pos, at_end) in lattice_nodes.iter().enumerate() {
                if end_pos > input_len {
                    continue;
                }
                for node in at_end {
                    lattice[end_pos].push(WordNode {
                        start: node.start,
                        end: node.end,
                        word: node.word.clone(),
                        syl_mask: node.syl_mask,
                        log_prob: node.log_prob,
                    });
                }
            }
            let result = self.viterbi.decode(&lattice, input_len);
            if !result.words.is_empty() && result.log_prob.is_finite() {
                let sentence: String = result.words.join("");
                // 质量闸门：低置信整句宁可不出，也不该顶掉可用的部分候选
                // （见 MIXED_SENTENCE_MIN_LOGP_PER_CHAR 的取值依据）。
                let logp_per_char = result.log_prob / sentence.chars().count().max(1) as f64;
                // **至少两个节点才算「组句」。** 单节点路径（`dblg` → 只有「夺不了冠」）
                // 本质就是一条简拼候选，包装成整句有实际危害：整句的 code 是**击键串**
                // （`dblg`），而简拼候选的 code 是全拼码（`duobuliaoguan`）——同一个词的
                // 词频会记到两个互不相认的键上，正是 wdat v5 改「索引存码」时修掉的那个坑。
                // 交给 step5/6.2 的简拼路径处理即可，那边的 code 是对的。
                if result.words.len() >= 2
                    && !sentence.is_empty()
                    && logp_per_char >= MIXED_SENTENCE_MIN_LOGP_PER_CHAR
                    && !candidates.iter().any(|c| c.text == sentence)
                {
                    candidates.insert(
                        0,
                        Candidate {
                            text: sentence,
                            // code = 整串**击键**。混合整句本就不是词库主键（它是解读不是词），
                            // 与 step 2 的整句一样以「本次输入」为码；消费整串由此自然成立。
                            code: abbr_query.to_string(),
                            weight: sentence_weight(result.log_prob, result.words.len()),
                            natural_order: 0,
                            source: CandidateSource::Pinyin,
                            is_sentence: true,
                            // 新建整句 = 引擎合成的解读，词库无此词条（同文合并那三处刻意不设）。
                            is_synthesized: true,
                            // 解码器实际走的那条路径。简拼段每字母一位，故回填出的
                            // preedit 是 `b'z'd'hao'bu'hao` —— 与击键同域，正是要的。
                            boundary: result.boundary,
                            // **不标 is_abbrev**：它是整句解读，不是简拼候选。标了会沉进
                            // 简拼层，被前缀回退的部分候选压在下面。
                            ..Default::default()
                        },
                    );
                }
            }
        }

        // 3. DAG 前缀子短语查找（仅 start==0）：给出输入的各级前缀词，供从左到右分段上屏
        //    （「nihao」→「你」「你好」）。不取中段/后段子串（如「hao」→「好」）——非前缀词
        //    应在前缀提交后、剩余拼音重转时才出现，否则会污染整串候选并破坏分段语义。
        if syllables.len() >= 2 {
            for end in 1..syllables.len().min(6) {
                let code: String = syllables[..end].join("");
                if code == query {
                    continue;
                }
                // 子词组 code 是输入的真前缀（比输入*短*，如 nihao 的「你」(ni)），是合法的
                // 分段上屏候选，与精确同层按权重排（不可降权——否则罕见全长词「拟好」会压过
                // 常用子词组「你」）。只有 code 比输入*长*的补全词(step4)才算前缀补全降权。
                for h in self.lookup_with_fuzzy(&code, &syllables[..end]) {
                    push_unique(
                        &mut candidates,
                        h.text,
                        code.clone(),
                        h.weight,
                        h.order,
                        h.is_fuzzy,
                        false,
                        h.boundary,
                        false,
                    );
                }
            }
        }

        // 4. 前缀查找（补全词，code 比输入长，如 si→思考）→ 前缀层级，降到精确之后。
        //
        // 尾部残码存在时（如 meiy 的 "y" 未成音节，completed="mei" ⊂ query="meiy"）：**提升进
        // 完整匹配层**（`is_promoted_completion=true`）。否则 is_prefix=true 会被协调器
        // build_candidates 的有效前缀层比较压到全部精确匹配之后，数百条单字「没/每/美/…」
        // 会淹掉「没有」（用户翻 15+ 页才见，与 "不处理" 无异）。提升后有效前缀层为 false，
        // 且 code("meiyou") 长于 query("meiy") → is_partial=false（由 push_unique 自动计算），
        // 落进精确/子短语层、浮到 is_partial=true 的精确子串（没/每）之前。
        // is_prefix 本身恒表结构真值（码更长），排序提升与结构事实拆开，见
        // `wind_candidate::Candidate::is_promoted_completion`。
        // 无残码时（meiyou）不提升，前缀补全沉在精确匹配之后（正常行为）。
        // 残码时的上浮特权不是无条件的：双拼每 2 键 1 音节，奇数键必然有残码，
        // 若 30 条补全全部上浮，长输入下候选 2~5 位会被该前缀下的冷僻长词占满
        // （`zhonghuarenmingongheg` → 一串「中华人民共和国XXX法」w≤60），且随每次
        // 按键在「整句+单字」与「整句+条文名」两种形态间反复跳动。
        //
        // 用「补全距离 + 置信度」约束：近距离（补完手头音节）无条件上浮；远距离属于
        // 预测未输入内容，需 weight 达门槛。距离**不能单独用**——实测 +4 上既有合理的
        // 「中国人民解放军」(w=252) 也有噪音「…物权法」(w=21)，判别力全在 weight。
        //
        // boundary=0（无边界信息的旧词典/用户手输码）→ 距离算作 0 → 放行，
        // 与本文件其他位置「无边界信息一律降级放行」的处理一致。
        let trailing_partial = completed != query;
        let completed_syls = syllables.len() as u32;
        // 输入**自身**表达的音节数：完整音节 + 尾部残码算起头的一个（`qingfengs` 的 `s`
        // 表示用户明确要接着打这个音节，意图强于停在整音节边界）。与
        // `should_promote_user_completion` 的 `started` 同义，也是 step 6.3 闸门的尺子。
        //
        // 用它而非「输入在候选切分下占的音节数」：后者会让 `xia` 因存在 `xi|an`（西安）
        // 的切分而被当成 2 音节、放行整批词组。见 `STRICT_SYLLABLE_MATCH_MAX`。
        let started_syllables = completed_syls + u32::from(trailing_partial);
        // 补全条数**跟随调用方请求量**，并 clamp 到 [30, MAX_COMPLETION_CANDIDATES]。
        //
        // 原先写死 30，代价是单字母输入（`b`/`y` 这类不成音节、无精确匹配的码）候选恒为 30 条、
        // 翻页翻不动。而**那个限制并没有省下成本**：wdat 前缀查询原是「遍历整棵子树 + 堆淘汰」，
        // 实测 `b` 取 30 条与取 5000 条同为 8~11ms，限制只压低了 String 分配，白白牺牲功能。
        // wdat v6 改为分支限界后（docs/design/prefix-topk-branch-and-bound.md），取数成本随条数
        // 而非子树规模增长，放开才真正划算：`b` 现在 300 条只要 0.9ms（改造前 30 条要 8ms）。
        //
        // ⚠️ **上限不能去掉**，瓶颈已从词库层转移到本函数的 `push_unique`：它按
        // `cands.iter().any(|c| c.text == text)` 线性查重，整体是 O(n²)。实测 `b`：
        // 300 条 0.9ms → 1000 条 3.5ms → 5000 条 **54.5ms**（5 倍数据量、15 倍耗时）。
        // 治本要把查重换成哈希索引，但 Viterbi 整句走 `insert(0)` 绕过该闭包、后续还有
        // `retain`，两者都会让哈希集与实际列表失同步，须单独立项处理。
        let completion_limit = max_candidates.clamp(30, MAX_COMPLETION_CANDIDATES);
        // **音节数过滤下推到词库层**（step 6.3 的闸门在此处提前施加，见
        // `STRICT_SYLLABLE_MATCH_MAX`）。若只在 6.3 那里事后 retain，`limit` 配额会被
        // 注定要丢弃的词组吃光：实测 `d` 取 1000 条补全，过闸门的只有 68 条单字，
        // 用户翻两页就没了。下推后 top-N 直接是 N 条合格条目。
        //
        // 长输入（`started ≥ 2`，如 `meiy`/`zhonghuar`）走不带上限的原路径，与改动前
        // 逐条一致。6.3 的 retain 仍然保留 —— 它还要兜住词库层判不了的
        // `boundary == 0`（DAG 现切）与 step6 并入的用户/临时词。
        let syllable_cap = completion_syllable_cap(
            started_syllables,
            self.config.completion_min_syllables,
            self.config.completion_max_extra_syllables,
        );
        for h in
            dict.search_prefix_with_boundary_syllable_capped(query, completion_limit, syllable_cap)
        {
            // 候选比已完成音节多出的音节数（`boundary == 0` → 0）。同时供「是否降级」判据
            // 与 `completion_penalized` 的折扣指数使用。
            let distance = h.boundary.count_ones().saturating_sub(completed_syls);
            let demote_to_prefix_layer = if trailing_partial {
                // ⚠️ 门槛比的是**原始** weight：COMPLETION_FAR_WEIGHT_FLOOR 按原始权重分布
                // 标定（合理项下界「中国人民解放军」252 / 噪音上界 60），拿折后值比会让这条
                // 线整体失准 —— 折扣与降级是两件正交的事。
                //
                // ★ `weight <= 0` 一律降级，**距离 1 的无条件上浮也不例外**：w≤0 是词库对
                // 「存疑 / 非标准条目」的标记（`lattice.rs::score_node` 早就对它罚 -10），
                // 而无条件上浮那条原本没有任何权重下限，于是最不可靠的词反而被提到最显眼处
                // ——`zhonghuar` 的「种花人」(w=0，距离恰好 1) 因此排到第 2，压过 w=18 的
                // 「中华人民」（后者距离 2、要过 FLOOR 而没过）。librime 用
                // `log(w > 0 ? w : DBL_EPSILON)` 在结构上避免了这类条目参与竞争。
                h.weight <= 0
                    || (distance > COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES
                        && h.weight < COMPLETION_FAR_WEIGHT_FLOOR)
            } else {
                true // 无残码：正常前缀补全，沉在精确匹配之后
            };
            // is_prefix 恒表结构事实（search_prefix_with_boundary 返回的都是码更长的补全）；
            // 「是否沉到非精确层」的排序决策由 is_promoted_completion 承接（残码上浮即提升）；
            // **层内**的远近之别由两件事共同承接：`completion_penalized` 的连续折扣（本层内
            // 的权重打折）与 `completion_extra_syllables` 的显示序档位（协调器侧，见该字段）。
            //
            // ⚠️ 两者口径不同，别把 `distance` 直接当档位用：`distance` 是「比**已完成**音节
            // 多几个」，档位要的是「比**输入自身表达的**音节数多几个」——残码位上用户已经
            // 起头了一个音节，`meiy` 的「没有」(2 音节) 对 started=2 是**恰好对齐**、不该降档，
            // 而它的 distance 是 1。折扣那边用 distance 是对的（它衡量的是权重可信度衰减，
            // 残码位那个音节确实还没打完）。
            let extra = distance.saturating_sub(u32::from(trailing_partial));
            let before = candidates.len();
            push_unique(
                &mut candidates,
                h.text,
                h.code,
                completion_penalized(h.weight, distance),
                h.order,
                false,
                true,
                h.boundary,
                !demote_to_prefix_layer,
            );
            // 同文已存在时 push_unique 不添加（如整句已 insert(0) 过），此时**不置位**——
            // 那条候选不是补全，档位保持 0 才对。
            if candidates.len() > before {
                candidates[before].completion_extra_syllables = extra.min(u8::MAX as u32) as u8;
            }
        }

        // 简拼族（纯简拼 step5 / 混合 step5b / 用户词 step6）是否命中了**整串**击键。
        // step 6.2 的前缀回退只在这三处全都落空时才启动——整串能打出词就不该降级。
        let mut abbrev_full_hit = false;

        // 整串是否**纯简拼形态**（每字母均为某音节首字母、且非完整音节序列）。
        // 提成变量供 step5 / step6 / step6.2 共用，`enable_abbrev` 仍在短路前（关闭时
        // 连 is_abbreviation 的 Dag 构建都省掉）。
        let stroke_is_plain_abbrev =
            self.config.enable_abbrev && AbbrevMatcher::is_abbreviation(abbr_query, trie);

        // 5. 简拼匹配（声母缩写，如 nh→你好）：查 wdat 预存的独立 AbbrevSection。
        //    仅当输入像简拼时才查（is_abbreviation：每字母均为某音节首字母、且非完整音节序列），
        //    避免对全拼输入做无谓查找。natural_order=999999 让简拼候选默认排在全拼之后。
        //    `enable_abbrev` 置于短路前：关闭时连 is_abbreviation 的 Dag 构建都省掉。
        //
        //    AbbrevSection 存的是**全拼码**（v5），故这里是「查索引拿码 → 走主表装配」两步。
        //    候选因此带上真实的 code 与 boundary：词频记账走 `cand_code` 取候选的 code，
        //    此前设成简拼串 `nh`，同一个词在简拼与全拼下遂走两个互不相认的计数。
        if stroke_is_plain_abbrev {
            for abbr_code in dict.search_abbrev(abbr_query, 10) {
                // 用 search_with_boundary 而非 search：拼音引擎直接持有 CachedDict、
                // 不经 SystemDictLayer，用 search() 会把边界丢在这里（P2b 踩过同款）。
                for h in dict.search_with_boundary(&abbr_code) {
                    // **音节数必须等于简拼字母数**。扁平码有损：`xian` 既是「西安」的
                    // xi|an（2 音节），也是「先」的 xian（1 音节）。索引里 `xa` 指向的是
                    // 前者的码，回查主表却会把后者一并捞出来 —— 实测 `xa` 出「先/线/弦/
                    // 现/县」一串单字。这是「存词」改「存码」引入的，存词时不会发生。
                    //
                    // boundary=0（无边界信息）**不再直接放行**：那是本判据唯一的漏网口，
                    // 手输码用户词/旧词典条目会绕过音节数约束（`nh` 出 3 音节词）。
                    // 改用 `effective_boundary` 对码现切补出音节数，与全拼侧 6.3 闸门同源。
                    let eb = effective_boundary(&abbr_code, h.boundary, trie);
                    if eb != 0 && eb.count_ones() as usize != abbr_query.len() {
                        continue;
                    }
                    let before = candidates.len();
                    push_unique(
                        &mut candidates,
                        h.text,
                        abbr_code.clone(),
                        h.weight,
                        999999,
                        false,
                        // is_prefix=false：简拼不是前缀补全，层级由 is_abbrev 表达（见下）。
                        false,
                        h.boundary,
                        false,
                    );
                    // **简拼层标记，必须与 step6 的用户词简拼一致。**
                    //
                    // 此前这里借 `is_prefix=true` 沉底，而 step6 用 `is_abbrev=true`——
                    // 二者在 `cmp_match_layers` 里是**两个不同层级**（`is_abbrev` 是第一键、
                    // 比前缀层更沉），于是用户词简拼被整层压在系统词简拼之后，怎么调频都
                    // 翻不过来（层级是硬闸门）：`dblg` 下用户词「大菠萝哥」永远排在系统词
                    // 「夺不了冠」之后。同层之后两者才能按权重/词频正常竞争。
                    if candidates.len() > before {
                        candidates[before].is_abbrev = true;
                        abbrev_full_hit = true;
                    }
                }
            }
        }

        // 5b. 混合简拼：同一串里混用声母与完整音节（`nhao` = n + hao、`nih` = ni + h）。
        //     设计文档 `docs/design/pinyin-mixed-abbrev.md`；模式表示见 `mixed_abbrev`。
        //
        //     召回复用 step5 的同一条索引：模式的**声母投影键**退化成纯简拼串（`nhao` → `nh`），
        //     AbbrevSection 的键正是这个形状，故索引一个字节都不用改。混合信息留在模式里做
        //     后置校验（按 boundary 把全拼码切回音节序列，逐段比对），因此不会像纯简拼那样
        //     把 `nh` 下的词一股脑捞出来。
        //
        //     **短路：整串已被音节完整覆盖时不进这里。** `nihao`/`xian` 这类正常全拼输入
        //     因此零开销——而它们正是绝大多数击键。判据用 `completed_len`（step1 之前已算好，
        //     不额外建 Dag），但只在全拼下可用：双拼的 `completed_len` 说的是转换后的全拼串，
        //     与 `abbr_query`（原始击键，文档 §5 约束 4）不同域。双拼下改为「转换结果覆盖了
        //     整串击键」——双拼每 2 键 1 音节，覆盖完整即无残码可作声母段。
        //
        //     ★ **双拼 + 手动分隔符时模式改从全拼域枚举**（`mixed_pattern_source`）。
        //     模式要的是「声母段 + 音节段」，而双拼击键域里没有音节：`n'hc` 的击键
        //     `nhc` 只能读成三个声母，`[n][hao]` 永远出不来。此前双拼下混合能work，
        //     靠的是击键串**碰巧**在全拼域也读得通（`xanning` = x+an+ning），是巧合
        //     不是机制——那正是「双拼下 `xan` 本身歧义、故意未处理」记的那件事。
        //     分隔符把段结构定死之后，`full_pinyin` 成了无歧义的解释，比击键域更可信。
        let mixed_pats = if self.config.enable_abbrev && !mixed_covered {
            mixed_abbrev::mixed_patterns(mixed_pattern_source, trie)
        } else {
            Vec::new()
        };
        if !mixed_pats.is_empty() {
            // 同一串的多条解释常投影到同一个键（`nhao` 的 [n][hao] 与 `nih` 的 [ni][h] 都是
            // `nh`），按键去重后每个键只点查一次索引。
            let mut keys: Vec<&str> = mixed_pats.iter().map(|p| p.key()).collect();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                // limit 比 step5 的 10 大一截：那边键即答案、取权重前 10 就够；这里拿到的
                // 码还要过一道逐段校验，**绝大多数会被滤掉**，取 10 条几乎必然一条不剩。
                for abbr_code in dict.search_abbrev(key, MIXED_ABBREV_INDEX_LIMIT) {
                    for h in dict.search_with_boundary(&abbr_code) {
                        // 无边界信息 → 判据不存在 → 不参与（不是放行，见 syllables_from_boundary）。
                        let Some(syls) =
                            mixed_abbrev::syllables_from_boundary(&abbr_code, h.boundary)
                        else {
                            continue;
                        };
                        if !mixed_pats
                            .iter()
                            .any(|p| p.key() == key && p.matches(&syls))
                        {
                            continue;
                        }
                        let before = candidates.len();
                        push_unique(
                            &mut candidates,
                            h.text,
                            abbr_code.clone(),
                            h.weight,
                            999999,
                            false,
                            false,
                            h.boundary,
                            false,
                        );
                        // **并入 is_abbrev 层，不新造层级、更不借用别的层级键**（文档 §5 约束 2）：
                        // 混合简拼与纯简拼是同质候选，分属两层就会让其中一侧被硬闸门整层压住、
                        // 词频永远翻不过来。这个标记同时让候选走在双拼边界校验的豁免侧（约束 1）
                        // ——它的 code 是词的全拼码，与当次击键根本不同域。
                        if candidates.len() > before {
                            candidates[before].is_abbrev = true;
                            abbrev_full_hit = true;
                        }
                    }
                }
            }
        }

        // 6. 用户/临时造词层（L：让拼音造的词显现）。查询与主词典相同的码——整串精确 +
        //    各前缀子码（你好 coded「nihao」当输入「nihaoma」时部分消费）+ 前缀补全——
        //    并入候选（dedup by text，已在系统词典出现的不重复加）。weight 由 store 记录给出，
        //    随后统一按 weight 排序；词频上浮交协调器 apply_freq_rerank（衰减软置前，frequency.md §4）。
        if let Some(store_dm) = &self.store_layers {
            let limit = max_candidates.max(50);
            let mut store_cands: Vec<Candidate> = store_dm.search(query, limit);
            if syllables.len() >= 2 {
                for end in 1..syllables.len().min(6) {
                    let code: String = syllables[..end].join("");
                    if code == query {
                        continue;
                    }
                    store_cands.extend(store_dm.search(&code, limit));
                }
            }
            store_cands.extend(store_dm.search_prefix(query, limit));

            // 用户长词上浮的**封顶基准**：提升后的补全不得越过「本次输入的最佳完整解」——
            // 码 == completed 的顶层候选（精确整词 / Viterbi 整句，均在此前步骤产出）。取其最大
            // 权重 - 1，与 step 6.5 整句降级同款手法，给出可预期的「就在最佳解之后」位置。
            // 无此类候选（如 qingfengshu 无精确词/整句）→ None → 不封顶，用户词落顶层按自身权重排。
            let completed_syls = syllables.len();
            let promotion_cap = candidates
                .iter()
                .filter(|c| {
                    !c.is_fuzzy
                        && (!c.is_prefix || c.is_promoted_completion)
                        && !c.is_partial
                        && c.code == completed
                })
                .map(|c| c.weight)
                .max()
                .map(|w| w.saturating_sub(1));

            for mut c in store_cands {
                if c.text.is_empty() {
                    continue;
                }
                // 同文时**合并**而非整条丢弃。
                //
                // 旧行为（`any(|x| x.text == c.text) → continue`）让用户词在系统词典已有同文时
                // 完全失声：用户把「自激」配到 w=2_000_000_000 也纹丝不动，最终 weight 仍是系统的
                // 18 —— 用户词的 weight **从不参与比较**，「提权」这个动作在词已存在时无效。
                //
                // 合并规则：
                // - `weight` 取 **max**：用户配高权重即生效；用户权重更低时保留系统值，
                //   因为用户加词的意图是「提权」而非「降权」（降权应由词频/屏蔽机制表达）。
                // - `code` / `boundary` **保留已有候选的**，不换成用户词的。二者必须同进同出
                //   （`d4084b8` 已踩过此坑：composite 去重换 code 时 boundary 未跟随，配出
                //   「A 层的 code + B 层的 boundary」）。用户手输码常无边界信息（boundary=0），
                //   换过去等于把系统词典的真值边界抹成未知。
                // - 置 `meta.is_user_dict = true` 使来源可追溯（该字段目前无比较器读取，
                //   仅供诊断/UI）。
                // - 其余层级标志（is_fuzzy/is_prefix/is_partial/is_exact_code）**一律不动**：
                //   它们描述的是「这条候选相对本次输入处在哪一层」，由已有候选的来源路径决定，
                //   与用户是否也收录了同一个词无关。
                if let Some(existing) = candidates.iter_mut().find(|x| x.text == c.text) {
                    // ⚠️ 用户权重的生效条件必须与下面「新增」分支**对称**，否则用户词会借合并
                    // 路径绕过 `should_promote_user_completion` 与 `promotion_cap`：
                    //
                    // 系统前缀补全放开条数后（`completion_limit`），用户词与系统候选同文的概率
                    // 大增。此处若无条件 `max`，一个**远距离**补全词会被抬到用户权重并留在
                    // 补全提升层——实测单字母 `s` 配上 w=2e9 的用户词「筛选」，它直接夺走首选，
                    // 把「是」「上」这些高频单字挤到第 2 位起。补全只取 30 条时该词进不了系统
                    // 候选、走的是新增分支、被判据正确拦下，于是问题只在放开后显形。
                    //
                    // 分层处理，与新增分支一一对应：
                    // - 精确匹配（`is_prefix=false`）：用户提权全效，这是「加词提权」的本义。
                    // - 前缀补全：须满足同一个提升判据，且同样受 `promotion_cap` 封顶。
                    // - 不满足判据的远距离补全：保留系统权重，沉在补全层（不是丢弃）。
                    if !existing.is_prefix {
                        existing.weight = existing.weight.max(c.weight);
                    } else if should_promote_user_completion(
                        completed_syls,
                        trailing_partial,
                        existing.boundary,
                        self.config.completion_max_extra_syllables,
                    ) {
                        let w = existing.weight.max(c.weight);
                        existing.weight = promotion_cap.map_or(w, |cap| w.min(cap));
                    }
                    existing.meta.is_user_dict = true;
                    continue;
                }
                c.source = CandidateSource::Pinyin;
                // 与 push_unique 一致：store 层的前缀子码命中也是子短语，降到完整匹配之后。
                c.is_partial =
                    !c.is_prefix && c.code.len() < query.len() && query.starts_with(&c.code);
                // 用户/临时词的前缀补全（is_prefix=true，码更长）：打到词尾附近就提升进完整
                // 匹配层，否则被首音节同音子短语整层淹没（长词打到第 3-4 音节才给的根因）。
                // is_prefix 保持结构真值不动，排序提升由 is_promoted_completion 承接。
                if c.is_prefix
                    && should_promote_user_completion(
                        completed_syls,
                        trailing_partial,
                        c.boundary,
                        self.config.completion_max_extra_syllables,
                    )
                {
                    c.is_promoted_completion = true;
                    if let Some(cap) = promotion_cap {
                        c.weight = c.weight.min(cap);
                    }
                }
                // 显示序档位，口径同 step4（`boundary == 0` ⇒ count_ones() 为 0 ⇒ 档位 0，
                // 即手输码用户词不降档 —— 与全仓「无边界信息一律降级放行」一致）。
                if c.is_prefix {
                    c.completion_extra_syllables =
                        c.boundary
                            .count_ones()
                            .saturating_sub(started_syllables)
                            .min(u8::MAX as u32) as u8;
                }
                candidates.push(c);
            }

            // 简拼匹配（用户/临时造词层）：经**声母索引**取候选集。
            //
            // 此前这里是「枚举该 schema 下全部用户/临时词、按各词自带的边界现算声母比对」，
            // 注释写的理由是「规模小，现算即可」——19 万词后该假设失效，实测 172ms/次，
            // 且 step6.2 还要逐切点再来十几遍。索引把候选集从全层缩到一个声母组，
            // **判据一字未动**（见 recall_store_by_abbrev）。
            //
            // natural_order 对齐 step5 系统简拼候选，同样排在全拼之后。
            {
                for mut c in self.recall_store_by_abbrev(
                    store_dm,
                    abbr_query,
                    stroke_is_plain_abbrev,
                    &mixed_pats,
                ) {
                    if c.text.is_empty() || candidates.iter().any(|x| x.text == c.text) {
                        continue;
                    }
                    // 比对基准是原始击键（见 `abbr_query`）：双拼下 query 已是转换结果，
                    // 拿它比对永远匹配不上用户敲的简拼。
                    let plain =
                        self.abbrev_of_code(&c.code, c.boundary).as_deref() == Some(abbr_query);
                    // 混合简拼：按 boundary 切回音节序列逐段比对（无边界 → 无判据 → 不参与）。
                    // 与系统词侧走同一批 `mixed_pats`，判据完全一致，只是这边不经索引——
                    // 用户词规模小，现算即可（与 `abbrev_of_code` 那条注释同理）。
                    let mixed = !plain
                        && mixed_abbrev::syllables_from_boundary(&c.code, c.boundary)
                            .is_some_and(|syls| mixed_pats.iter().any(|p| p.matches(&syls)));
                    if !plain && !mixed {
                        continue;
                    }
                    c.source = CandidateSource::Pinyin;
                    // **保留全拼码**（连同同域的 boundary），不覆盖成简拼串。
                    //
                    // 词频记账走 `cand_code`（取候选的 code），覆盖成 `xan` 会让同一个词在
                    // 简拼与全拼下走两个互不相认的计数——用简拼练熟的词切回全拼一点不认。
                    //
                    // `consumed_length` 不受影响：它的判据是 `query.starts_with(&c.code)`，
                    // 简拼下 `xan` 不以 `xianning` 开头 ⇒ 落 else 分支取 `query.len()`，
                    // 仍是「消费整串」，与覆盖时同值。
                    c.is_prefix = false;
                    c.is_partial = false;
                    // 简拼层标记。此前借 `is_fuzzy` 沉底——那是模糊音的「召回来源」标记，
                    // 与简拼无关；`is_fuzzy` 退出 `cmp_match_layers` 后借用会把简拼一起放上来。
                    c.is_abbrev = true;
                    c.natural_order = 999999;
                    candidates.push(c);
                    abbrev_full_hit = true;
                }
            }
        }

        // 6.2 简拼族**前缀回退**：整串一无所获时，退到最长的能命中的前缀，
        //     余下字母作残码留给下一次输入（分步上屏）。
        //
        //     真机现场（连打 `bzdnihaobuhao`）：输到 `bzdha` **整串空码**。`bzd` 明明能出
        //     「不知道」，但简拼族的召回一直是「全串或无」——索引按完整简拼串点查、混合
        //     模式要求覆盖整串，于是简拼一旦长过任何单个词就彻底没有候选。全拼下有 step3
        //     子短语与 step4 前缀补全兜着（`nihaobu` 照样出「你好」且只消费 5 字节），
        //     简拼下此前没有任何对应机制。**这不是混合简拼引入的，纯简拼一直如此。**
        //
        //     **只在整串一无所获时降级**，故整串能命中的情形与改动前逐字节一致（零回归）。
        //
        //     ★★ **不做「最长匹配、一有产出即停」——多个切点的候选共存，同层按词频竞争。**
        //     首版那样 break 掉更短的切点，等于把「切点长短」做成了**硬闸门（惩罚 ∞）**：
        //     `bzdhaobuhao` 的 `bzdh` 恰好是「表彰大会」的简拼（w=93），于是它把 `h` 抢走、
        //     短切点 `bzd`（→「不知道」，**w=62492，高 672 倍**）一条都进不来，剩下的
        //     `aobuhao` 成了垃圾。一个结构性偏好压过了三个数量级的词频差异。
        //     切点长短是**来源差异**、不是结构质量差异，只配走 weight，不配做布尔层级键
        //     （同款教训见模糊拼音 `is_fuzzy` 从层级键改惩罚的那一轮）。
        //     代价是候选变多，用 `MAX_FALLBACK_PER_CUT` / `MAX_FALLBACK_TOTAL` 两道限流兜住。
        //
        //     ⚠️ 这仍是**启发式**，不是智能组句：简拼段不进 lattice/Viterbi，`bzd|haobuhao`
        //     与 `bzdh|aobuhao` 之间没有语言模型仲裁，只靠词频。真正的解法是把简拼段作为
        //     词图节点纳入整句解码（见 §4.8 待办）。
        //
        //     ⚠️ 门槛问的必须是「这串里**有成不了音节的部分**吗」（`!mixed_covered`），
        //     而不是「整串本身是不是一条合法的简拼/混合模式」。两处都栽过：
        //
        //     - 只看 `!abbrev_full_hit`（简拼族没命中）⇒ **纯全拼也被拖进降级**：`meiyou`
        //       的 is_abbreviation 因 `i` 判假、整串又被音节完整覆盖使混合模式为空，于是
        //       「一无所获」成立 ⇒ 退到 `meiy` 后 `[mei][y]` 命中「没有」，凭空多出
        //       `is_abbrev` 候选、破坏「前缀补全排在精确匹配之后」的层级
        //       （`test_pinyin_trailing_partial_prefix_floats_above_exact` 当场抓到）。
        //     - 改用「整串是简拼族形态」又**把长串挡在门外**：`bzdnihaobuh` 最少需要
        //       `[b][z][d][ni][hao][bu][h]` 七段、超过 `MAX_SEGMENTS`，于是整串模式为空、
        //       门槛不过 ⇒ 连打到第 11 键**彻底空码**（真机报的正是这个）。
        //
        //     `!mixed_covered` 两边都对：`meiyou`/`nihao` 被音节完整覆盖 ⇒ 不降级；
        //     凡含成不了音节的字母（`bzdha`、`bzdnihaobuh`）⇒ 允许降级，且**逐前缀的严格
        //     判定在 `recall_abbrev_prefix` 内部**（每个 stroke 各自过 is_abbreviation /
        //     mixed_patterns），门槛只负责挡掉纯全拼、不负责替前缀把关。
        if self.config.enable_abbrev
            && !abbrev_full_hit
            && !mixed_covered
            && abbr_query.len() > MIN_ABBREV_STROKE
        {
            let mut cuts_with_hits = 0usize;
            for take in (MIN_ABBREV_STROKE..abbr_query.len()).rev() {
                if !abbr_query.is_char_boundary(take) {
                    continue;
                }
                let before = candidates.len();
                self.recall_abbrev_prefix(&abbr_query[..take], take, &mut candidates);
                if candidates.len() > before {
                    cuts_with_hits += 1;
                    if cuts_with_hits >= MAX_FALLBACK_CUTS {
                        break;
                    }
                }
            }
        }

        // 手动分隔符边界过滤：用户以 `'` 强制音节边界后，凡「码恰好落在某音节边界、但字数
        // 与所跨音节数不符」的候选被剔除——如 xi'an 强制 [xi,an]，则单字「先」(code=xian,
        // 跨 2 音节却仅 1 字) 不该出现；「西」(code=xi,1 字 1 音节)、整句「西安」(2 字 2 音节) 保留。
        // 码不落在任何边界的候选（如前缀补全）不受影响。
        if has_sep {
            let syls = &syllables;
            candidates.retain(|c| match syllable_span(syls, &c.code) {
                Some(k) if k >= 1 => c.text.chars().count() == k,
                _ => true,
            });
        }

        // 6.3 音节数匹配闸门（短输入档）：输入自身只表达了 ≤ STRICT_SYLLABLE_MATCH_MAX 个
        //     音节时，**前缀补全**候选的音节数不得超过输入的音节数 —— 即 `d`/`dian`/`xia`
        //     不出「电话」「西安」，对齐主流拼音输入法。判据与阈值见
        //     [`STRICT_SYLLABLE_MATCH_MAX`]，尺子是 `started_syllables`（输入的属性）。
        //
        //     **只约束 `is_prefix`（码比输入长的补全词）**，其余一律放行 —— 这不是白名单，
        //     而是判据的适用边界：闸门要挡的是「预测用户尚未输入的音节」，而只有前缀补全
        //     在做这件事。于是三类候选天然免疫：
        //     - 精确匹配（`dian` 的「堤岸」，code == query 但切分为 `di|an`）：它完整解释
        //       了输入，音节数是几与「预测」无关；
        //     - 子短语（code 短于 query，step3）与整句：同理；
        //     - 简拼族（step5/5b/6/6.2）：`is_prefix=false`，且其 code 与击键不在同一编码域
        //       （`nh` → `nihao`），音节数判据在各自源头按**击键字母数**施加，这里不能插手。
        //
        //     长输入（`started_syllables ≥ 2`）整体跳过：`meiy`→「没有」、`nih`→「你好」、
        //     `zhonghuar`→「中华人民共和国」全部不受影响。
        //
        //     位置必须在 step 6.2 之后：用户/临时词到 step 6 才并入，简拼前缀回退到 6.2；
        //     且必须在排序/截断**之前**（P2b 同款教训：先截断再过滤会把该出的候选挤掉）。
        candidates.retain(|c| {
            if !c.is_prefix {
                return true;
            }
            // 判据与词库层下推的那道**共用同一个函数**（`wind_dict::cached`），避免两处
            // 各写一份日后漂移。差别只在入参：这里先用 `effective_boundary` 把无真值的
            // boundary 用 DAG 补出来，词库层做不到这一步（拿不到 trie），只能放行。
            let b = effective_boundary(&c.code, c.boundary, trie);
            wind_dict::cached::prefix_syllable_keep(b, syllable_cap)
        });

        // 裸声母（无完整音节，如 "m"/"zh"）单字优先：候选全为前缀补全词（is_prefix=true），
        // 纯按词频排会让高频多字词（没有/目前）压过单字（吗/么）——不合直觉。给单字提权使其
        // 排在多字词之前（对齐主流输入法首字优先，见 BARE_INITIAL_SINGLE_CHAR_BOOST）。
        // 仅此情形——完整音节输入的单字已靠精确层级(is_prefix=false)就位，无需提权。
        if syllables.is_empty() {
            for c in candidates.iter_mut() {
                if c.text.chars().count() == 1 {
                    c.weight = c.weight.saturating_add(BARE_INITIAL_SINGLE_CHAR_BOOST);
                }
            }
        }

        // 6.5 整句让位于精确整词：**降级，不销毁**
        //
        // 输入 `lianzhengtixing` 时用户词「廉政提醒」严格覆盖整串，而 Viterbi 拼出的
        // 「连整体性」靠 旧的 SENTENCE_WEIGHT_BASE(30M，已退役) 的量纲优势恒占首位——30M 碾压一切词频，
        // 用户把词加进词库、配再高的权重也换不回首选。
        //
        // 早先试过的「有精确整词就不构造整句」是**销毁**：整句连候选都不在，用户想选也
        // 选不到，代价不可挽回。这里改为降级——整句仍在候选里，只是排到精确整词之后，
        // 代价是「多按一次」。
        //
        // 位置必须在 step 6 之后：用户/临时层的词到 step 6 才并进 `candidates`，
        // 而「用户加词」正是本功能要服务的场景，放在 step 2 旁边会看不见用户词。
        //
        // 只降 **Viterbi 新合成** 的整句（`synthesized_sentence`）。与词典整词同文而合并的
        // 那一支（nihao→你好、gonghe→共和）本身就是精确整词，无处可让。
        //
        // ## 为什么用「相对权重」而不是固定的降级基数
        //
        // 精确整词的权重是原始词频，量纲跨度极大（系统词条 1~2e6，用户词可配到 2e9）。
        // 任何固定常数都会在某一侧翻车：偏高则用户词压不住整句（原问题复发），偏低则整句
        // 沉到普通候选里。取相对值使结果与词频数值无关。
        //
        // ## 为什么是 `max - 1` 而不是 `min - 1`
        //
        // 取 `max`：整句只让位给**最强的那个**精确整词，恒定停在它之后。这给用户一个
        // 可预测的位置（「整句就在第二条」），而 `min - 1` 会让 w=8 的冷僻同码词也压过
        // 引擎对整串输入的最优解读——说不通，且名次随该输入下同码词条数浮动、无从预期。
        //
        // **多个精确整词并列于 max 时，整句排在它们全部之后**：并列者权重皆为 `max`，
        // 严格大于 `max - 1`，由算式保证，无需额外判据（见
        // `demoted_sentence_falls_below_all_max_weight_exact_words`）。
        //
        // ## `max - 1` 与其它候选在 weight 上并列时
        //
        // 同层内**只有**精确整词与整句本身：子短语（`is_partial`）、前缀补全（`is_prefix`）、
        // 模糊命中（`is_fuzzy`）由 `cmp_match_layers` 整体压在下一层，与权重无关；协调器侧
        // 的短语（`is_prefix=true`）同理，引导键导航候选与码表精确候选则由
        // `cmp_exact_first` 挡在上一层——两者都在 `candidate_display_order` 中先于权重比较。
        // 故权重并列只可能发生在整句与**另一个精确整词**之间，此时落到 base_order /
        // natural_order 决定谁先，无论结果如何都不破坏「整句在普通候选之前」这条不变量。
        //
        // 三个排序器（本函数、协调器 `candidate_display_order`、`freq_rerank`）都以
        // `cmp_match_layers` 为首要键，故这个位置在整条链路上一致。
        //
        // `is_sentence` 不清：它表达「引擎对整串输入的最优解读」这个**来源**语义，
        // 降级是**排序**决策，另立 `is_sentence_demoted` 表达（`freq_rerank` 的顶部锚定
        // 据此豁免，否则那里不看 weight，本处降权会被整个顶回去）。
        // 两类整句需要让位于精确整词：
        // ① Viterbi **新合成**的整句（词典无此词，由多节点拼出）；
        // ② **模糊命中**的整句（词典有此词，但经模糊变体召回——如 `sixiang` 经 s↔sh 命中
        //    词条「是想」，在词图里成为覆盖全串的单节点被 Viterbi 选中）。
        //
        // ② 必须走这条路而非 `fuzzy_penalized` 的比例折扣：整句拿的是旧的 `SENTENCE_WEIGHT_BASE`（已退役）
        // (3e7) 基准分，与词典词的词频量纲（1e2~1e6）差几个数量级，任何比例折扣都压不下来
        // （0.01 折扣后仍有 3e5，照样碾过「思想」的 26133）。而「降到精确整词之下」在语义上
        // 恰好对：模糊解读让位于精确解读。
        //
        // 该判据还天然区分了两种场景，无需额外条件：`sixiang` 存在精确整词「思想」故
        // 「是想」让位；`zongguo` 下没有以 zongguo 为码的精确整词（exact_max=None），
        // 「中国」照常居首——这正是模糊音想要的效果。
        let mut demote_targets: Vec<String> = synthesized_sentence.iter().cloned().collect();
        for c in candidates.iter() {
            if c.is_sentence && c.is_fuzzy && !demote_targets.contains(&c.text) {
                demote_targets.push(c.text.clone());
            }
        }
        for sent in demote_targets {
            // 「精确整词」判据：码恰好等于已消费输入 `completed`，且非模糊命中、
            // 不在前缀补全/子短语层。含系统词库与 step 6 并入的用户/临时层。
            let exact_max = candidates
                .iter()
                .filter(|c| {
                    c.text != sent
                        && !c.is_fuzzy
                        && !c.is_prefix
                        && !c.is_partial
                        && c.code == completed
                })
                .map(|c| c.weight)
                .max();
            if let Some(max_w) = exact_max
                && let Some(c) = candidates.iter_mut().find(|c| c.text == sent)
            {
                c.weight = max_w.saturating_sub(1);
                c.is_sentence_demoted = true;
            }
        }

        // 6.5b 整句让位于「恰好用完残码的补全」
        //
        // ## 现象
        //
        // 打 `nihaom`（`m` 是残码）时首选恒是整句「你好」—— 它只解释了 `nihao`，**把用户
        // 已经按下的 `m` 丢掉了**；真正响应了 `m` 的「你好吗」屈居第 2。实测该规律与音节数
        // 无关，2/3/4/6 音节一律如此：`zhongguor`→中国(丢 r)、`zhongguorenm`→中国人(丢 m)、
        // `zhonghuarenmingongheg`→中华人民共和(丢 g)。根因是整句拿着
        // 旧的 `SENTENCE_WEIGHT_BASE`(3e7，已退役) 无条件置顶，而补全只有真实词频（个位数 ~ 1e4），
        // 差 4~7 个数量级，永远翻不过去。
        //
        // ## 判据来自 librime，且比「按 extra 一刀切」更准
        //
        // librime `gear/script_translator.cc:387`：**存在覆盖完整输入的精确词条时，根本不
        // 生成整句**（`if (!has_exact_match_phrase(...)) sentence_ = MakeSentence(...)`）。
        // 其音节级 completion 会把残码 `m` 补成 `ma/mai/mei…`，于是 `ni|hao|ma` 覆盖全部
        // 输入、「你好吗」成为 exact match ⇒ 不做整句。fcitx5/libime 同理：不完整拼音是
        // lattice 里的合法节点，覆盖全长的路径天然优先。
        //
        // 我们的残码被排除在音节图之外（见 step 2 附近注释：否则 lattice 到不了残码末端），
        // 「你好吗」只能以**补全**身份出现，故复刻其语义而非其实现：
        // **补全词音节数 == 已完成音节数 + 1** —— 残码补成一个完整音节后，这个词恰好用完。
        //
        // ★ 这个判据自带过滤，无须再给低频词打补丁（实测代入）：
        //
        // | 输入 | 候选 | 音节数 vs completed+1 | 结果 |
        // |---|---|---|---|
        // | `nihaom` | 你好吗 | 3 == 2+1 | 让位 ✓ |
        // | `zhongguor` | 中国人 | 3 == 2+1 | 让位 ✓ |
        // | `zhongguorenm` | 中国人民 | 4 == 3+1 | 让位 ✓ |
        // | `zhonghuarenmingongheg` | 中华人民共和国 | 7 == 6+1 | 让位 ✓ |
        // | `beijingdaxuex` | 北京大学校长 | 6 ≠ 4+1 | **不让位**（w=4 的冷僻预测词） |
        //
        // 换成「extra ≤ 2」这类一刀切，最后一行就会放进来，w=4 的「北京大学校长」顶掉
        // 「北京大学」。**判据选对，坏例子不需要额外条件挡。**
        //
        // ## 手法与 6.5 一致：降级，不销毁
        //
        // 降到该批补全的 `max - 1`（理由同 6.5 的相对权重论证），整句仍在候选里、就在其后。
        // 取 `min` 是因为 6.5 可能已经降过一次 —— 两次让位取更低者，不能把已让的位抬回来。
        //
        // 同层性：残码补全经上浮已是 `is_promoted_completion=true` ⇒ `cmp_match_layers` 的
        // `eff_prefix` 为 false，与整句**同层**，故降 weight 即可换位（若不同层则跨层不比权重，
        // 降多少都没用 —— 这正是 6.5 注释里那条不变量的另一面）。
        //
        // `boundary == 0`（无边界信息）的候选 `count_ones()` 恒为 0，永不满足判据 ⇒ 自动
        // 排除，与本文件「无边界信息一律降级放行」一致（此处「放行」= 不触发让位 = 保守）。
        let exhausting_completion_max = if trailing_partial {
            candidates
                .iter()
                .filter(|c| {
                    c.is_prefix
                        && !c.is_fuzzy
                        && c.boundary.count_ones() == completed_syls + 1
                        // 置信度下限，见 SENTENCE_YIELD_WEIGHT_FLOOR：没有它，
                        // `zhonghuar` 的「种花人」(w=0) 就能把整句「中华」顶掉并压成 -1。
                        && c.weight >= SENTENCE_YIELD_WEIGHT_FLOOR
                })
                .map(|c| c.weight)
                .max()
        } else {
            None
        };
        if let Some(max_w) = exhausting_completion_max {
            let target = max_w.saturating_sub(1);
            for c in candidates.iter_mut().filter(|c| c.is_sentence) {
                if c.weight > target {
                    c.weight = target;
                    c.is_sentence_demoted = true;
                }
            }
        }

        // 6.5c 「1 音节 + 残码」残码整句的**延迟定夺**（step 2c 短上下文档，见那里的长注释）。
        //
        // 判据与标定见 [`SENTENCE_KEEP_RATIO`]：整句要强过「最强恰好用完残码的补全」若干倍
        // 才配进候选 —— 倍数大说明词库在这个码上给不出好答案（`zaim` 的正解「在吗」w=0，
        // 补全侧最强者只是「再买」819，比值 61×），此时整句解码走单字乘积、不吃词条频率，
        // 反而更可信。不够格的**根本不插入**，故候选集与放开门槛前逐条一致。
        //
        // ★ 放在 6.5b **之后**是有意的：本档整句不参与那轮让位。6.5b 的语义是「整句丢掉了
        // 残码、该让位给用完残码的补全」，而本档整句消费了整串，压根不是它要治的对象；
        // 若放在之前，它会被无条件降到 `补全max - 1`，正是「在吗」被压到 818 的原因。
        if let Some((text, weight, boundary)) = short_sentence_pending {
            // 补全侧没有「恰好用完残码」的答案（None）时无从比较，放行 —— 与本文件
            // 「无信息一律放行」一致，且那种情况下整句本就是唯一的整串解释。
            // 补全侧没有「恰好用完残码」的答案（None）⇒ 词库在这个码上一无所有，放行。
            // 有答案时：只有它**弱到不像正解**（见 COMPLETION_WEAK_CEILING）才让整句上位，
            // 且整句本身要真的更强 —— 后者挡住「补全弱、整句更弱」的双差场景。
            let strong_enough = exhausting_completion_max
                .is_none_or(|max_w| max_w < COMPLETION_WEAK_CEILING && weight > max_w);
            if strong_enough {
                if let Some(existing) = candidates.iter_mut().find(|c| c.text == text) {
                    // 同文合并：step 4 已把它当前缀补全召回过（`zaim` 的「在吗」w=0）。
                    // ★ 必须一并提层：w=0 让它吃了 `demote_to_prefix_layer`，
                    // `is_promoted_completion=false` ⇒ `cmp_match_layers` 的 `eff_prefix` 为真、
                    // 整条压在前缀层，**跨层不比权重**，只抬 weight 是抬不上来的。
                    existing.weight = existing.weight.max(weight);
                    existing.is_partial = false;
                    existing.is_sentence = true;
                    existing.is_promoted_completion = true;
                } else {
                    candidates.insert(
                        0,
                        Candidate {
                            text,
                            // 码取整串（含残码）⇒ consumed_length = query.len()，理由同 step 2c
                            code: query.to_string(),
                            weight,
                            natural_order: 0,
                            source: CandidateSource::Pinyin,
                            is_sentence: true,
                            // 新建整句 = 引擎合成的解读，词库无此词条（同文合并那三处刻意不设）。
                            is_synthesized: true,
                            boundary,
                            ..Default::default()
                        },
                    );
                }
            }
        }

        // （step 6.6「整句有同码竞争者 → 标 `is_sentence_contested` 摘词频锚定」已删除。
        //  它的唯一作用是在 `freq_rerank` 的顶部锚定上凿一个洞，好让 `siyuan` 的「思源」、
        //  `gonghe` 的「恭贺」能靠词频反超同码整句。整句锚定本身已随「整句 weight 与词库
        //  同量纲」一并移除，这个洞便失去了要凿的墙 —— 实测拆除前后 45 个真机场景逐条一致。
        //  字段 `Candidate::is_sentence_contested` 同批回收。）

        // 引擎内部排序（层级对齐 Go：完整匹配 >> 子短语 >> 前缀补全 >> 模糊）：
        // ① 非模糊优先于模糊（is_fuzzy=false 在前）；② 完整匹配/子短语(is_prefix=false)优先于
        // 前缀补全(is_prefix=true)；③ 完整匹配(is_partial=false)优先于子短语(is_partial=true)
        // ——对齐 Go coverage 分层，避免高频单字「报/宝」插进完整词「保安」「报案」之间；
        // ④ 同层内按权重降序、自然序升序。
        // 使输入 si 时：精确单字「四/死」> 前缀补全「思考/似乎」> 模糊命中「是」；
        // 输入 baoan 时：完整词「保安」「报案」> 子短语单字「报/宝」。
        // 双拼真值边界校验：双拼把音节边界定死了，候选的词典边界必须与之吻合。
        // 在排序/截断**之前**过滤——否则会先截断再过滤，把该出的候选挤掉。
        // 词典无边界信息的候选（用户手输码/五笔/旧数据）boundary=0，一律放行（降级回 DAG 行为）。
        if let Some(r) = &sp_result {
            let sp_mask = sp_boundary_mask(r);
            if sp_mask != 0 {
                let full_len = r.full_pinyin.len();
                candidates.retain(|c| {
                    // **简拼候选豁免。** 它的 code 是词的全拼码（`xianning`），与当次击键
                    // （`xan`）根本不同域，边界比较无意义且必然不符——双拼下打简拼会被整批
                    // 误杀。原设计里简拼候选 boundary 恒为 0，靠「任一侧为 0 即放行」自然
                    // 豁免；wdat v5 让简拼改走主表装配、带上了真实边界，那条隐式豁免随之
                    // 失效，故须显式写出。同理于模糊变体——那边至今仍靠 boundary=0 豁免。
                    // 全拼降级候选（step 6.7）同理豁免，且理由与简拼**完全一样**：它的
                    // `code` 是词的全拼码，而 `sp_mask` 说的是双拼把这串键切成了什么样，
                    // 两者根本不同域，比出来必然不符 —— 不豁免就是整批静默滤光。
                    c.is_abbrev
                        || c.is_fullpinyin_fallback
                        || boundary_compatible(c.boundary, sp_mask, c.code.len(), full_len)
                });
            }
        }

        // 6.7 **全拼降级支路**（双拼方案 + `allow_full_pinyin`）：把击键串当全拼再读一遍，
        //     服务「多人共用一台机器」——主力打双拼，偶尔来的人只会全拼。
        //
        //     ★★ **必须排在双拼边界校验之后**，这是本支路最反直觉的一处顺序约束。
        //
        //     双拼转换的结果常常与击键串**同形**：`nihao`(5 键) 双拼解释 ni|ha|o 拼起来还是
        //     "nihao"，于是 step1 早就精确命中了「你好」并放进候选——只不过它的词典边界
        //     ni|hao 与双拼解释 ni|ha|o 不符，会被上面那道 retain 删掉（见
        //     `boundary_compatible_rules` 的「这正是 5 键出「你好」的病灶」）。
        //
        //     若把本支路放在校验**之前**：支路查 "nihao" 得到「你好」，却因同文查重让位给
        //     step1 那条双拼候选，而后者紧接着被校验删除 —— **两条都没了**，用户打 `nihao`
        //     依旧一无所获，且开关看起来完全没生效。放在校验之后，被删的坑正好由支路补上。
        //
        //     顺带满足另两条约束：① 在 6.3 音节数闸门之后——那道 retain 的尺子
        //     `syllable_cap` 由 `started_syllables`（**双拼域**音节数）算出，与全拼域对不上，
        //     用它裁全拼候选是判据跨域复用；② 在所有双拼候选产出之后——支路的 push 靠
        //     「同文时保留已有候选」实现「双拼优先」，提前会让双拼版本反被挡掉。
        //
        //     ⚠️ 因此本支路的位置由**三条**约束共同钉死，挪动前请逐条复核。
        if self.config.allow_full_pinyin
            && sp_result.is_some()
            && let Some(fp_syls) = self.full_pinyin_gate(raw_input)
        {
            self.recall_full_pinyin(raw_input, &fp_syls, &mut candidates);
        }

        // 分段上屏所需：标注每个候选实际消费的输入字节数。
        //
        // 本块原在 `sort_by` + `truncate` **之后**，现提到其前。当前排序并不消费它（理由见
        // 下方 sort_by 注释），提前纯粹是为消除一个隐患：日后若有人想让排序用上
        // `consumed_length`，在原位置它恒为 0，改动会静默失效且无诊断。位置提前不改变任何
        // 现有行为（计算只依赖 `c.code`/`query`/`sp_result`，三者此刻均已就绪）。
        // code 为 input（全拼）的前缀（如 "ni" ⊂ "nihao"）→ 只消费该前缀，选中后保留剩余拼音续转；
        // 否则（整句/前缀补全/非前缀子串）消费整串。0 表示未知（由调用方按整串处理）。
        // 双拼激活时：全拼字节数需通过 map_consumed_length 回算为双拼原始键数，
        // 否则变长音节（2键→3字节，如 zh/ch/sh）会错误消费/越界双拼键缓冲区。
        for c in candidates.iter_mut() {
            // 简拼族前缀回退（step 6.2）的候选**自带击键域的消费数**，必须跳过这套计算。
            //
            // 简拼的 code 是词的全拼码（`buzhidao`），不以 query（`bzdha`）开头 ⇒ 会落到
            // else 分支取 `query.len()`，即「消费整串」——选中「不知道」后余下的 `ha` 会被
            // 一起吃掉，分步上屏当场失效。整串简拼候选的 consumed_length 保持 0、照旧走
            // 这条计算（消费整串正是它要的），故此判据不影响它们。
            //
            // 双拼下同样跳过：简拼的判据本就走 raw_input（击键域），consumed_length 的语义
            // 就是「消费多少击键」，无须再过 map_consumed_length 换算。
            if c.is_abbrev && c.consumed_length != 0 {
                continue;
            }
            // 全拼降级候选（step 6.7）同样自带击键域的消费数。**不可再过
            // `map_consumed_length`**：那是双拼流专用的「全拼字节 → 双拼键数」回映射，而本
            // 支路的码本身就是击键串的前缀（全拼域 ≡ 击键域），再换算一次即错位——
            // `nihao` 选「你好」会被回映射成 4（双拼 4 键的量），凭空吞掉一个键。
            if c.is_fullpinyin_fallback {
                continue;
            }
            // 以剥除分隔符后的 query 为基准计算消费长度（无分隔符时 query==input）。
            let fp_consumed = if !c.code.is_empty() && query.starts_with(&c.code) {
                c.code.len()
            } else {
                query.len()
            };
            c.consumed_length = match &sp_result {
                Some(r) => r.map_consumed_length(fp_consumed),
                // 全拼含手动分隔符：query 是剥除 `'` 的串，需回映射到含 `'` 的原始输入空间，
                // 否则协调器按含 `'` 缓冲切片时会残留尾字符（xi'an 选「西安」残 "n"）。
                // 与双拼分支同一套 SylSpan 表示，只是 span 来源不同（切分 vs 双拼转换）。
                None if has_sep => interp::map_fp_to_raw(&sep_spans, fp_consumed, input),
                None => fp_consumed,
            };
        }

        // 「候选须消费整串」过滤（混输经 `schema.mix.pinyin_partial_candidates*` 注入）。
        //
        // **位置是本过滤的要害：必须在紧随其后的 `sort_by` + `truncate` 之前。** 部分匹配的
        // 同音字动辄数百条（`gedw` 里 `code=ge`、`consumed_length=2` 的有 219 条），而简拼候选
        // 在 `cmp_match_layers` 里是最沉的一层 —— 放到调用方去 `retain` 的话，配额被残码占满时
        // 简拼词在截断那一步就已经没了（实测「各单位」被压到第 221 位）。
        //
        // 判据只能是 `consumed_length`，**不能用 `!is_partial` 代替**：Viterbi 整句走
        // `insert(0)` 不经算 `is_partial` 的闭包（`aaw` → 「啊啊」consumed=2 而 is_partial=false），
        // 同文合并还会主动把它置 false。0 = 引擎未标注 ⇒ 按整串算（全仓约定，整串简拼即此形态）。
        //
        // ★ 判据切在「解释完整度」而非「候选类型」上，是本过滤成立的全部理由：前缀补全
        // （预测尚未输入的音节，`wanl` → `wanle`「完了」consumed=4=整串）与残码单字（放弃了
        // 已经输入的字母）在类型上同为「不精确」，方向却相反。按类型禁用会把正在输入中的
        // `wanl` 一并打死 —— 实测过滤后 `wanl` 仍有 151 条候选，正是这条判据的价值。
        //
        // 基准取 `input.len()` 而非 `query.len()`：上一段刚把 `consumed_length` 回映射到
        // **原始输入空间**（双拼 → 击键数、含分隔符 → 含 `'` 的串），`query`（剥除分隔符后）
        // 与它不同域。无分隔符的全拼下二者相等，取错只在双拼/分隔符场景静默失效。
        if require_full_match {
            candidates.retain(|c| c.consumed_length == 0 || c.consumed_length >= input.len());
        }

        // ⚠️ **引擎侧刻意不用「消费长度优先」排序**（协调器 `candidate_display_order` 用）。
        //
        // 试过，会破坏分段上屏：紧随其后的 `truncate` 使排序决定谁**活过截断**，而消费更少
        // 的候选（`xi'an` 的子短语「西」、`nihao` 的单字「你」）会被整批丢弃 —— 不是排到
        // 后面翻页可见，是根本不在列表里。实测红 10 条，其中 `separator_two_step_segmentation`
        // / `mouse_select_two_step_segmentation` 是真回归。
        //
        // 根因是架构差异：librime 的 `Translation` 惰性流式、按需产生、从不全局截断，
        // 排序键只影响顺序；我们「一次性产生 N 条 + 截断」，排序键同时决定了去留。
        // ⇒ 引擎侧的匹配层级（含 `is_promoted_completion` 上浮）保证的是「高价值候选活过
        // 截断」，与协调器 P0 的「显示顺序」**不是同一件事，不可互相替代**。
        // 要下沉 P0 必须先给 `truncate` 配一套按消费长度分档的保底配额（参考混输
        // `PINYIN_QUOTA_DIVISOR`），本轮未做。
        candidates.sort_by(|a, b| {
            wind_candidate::cmp_match_layers(a, b)
                .then(b.weight.cmp(&a.weight))
                .then(a.natural_order.cmp(&b.natural_order))
        });
        candidates.truncate(max_candidates);

        let (mut preedit_display, completed_syllables, partial_syllable) =
            self.compute_composition(input);

        // 预编辑区**跟随首选候选**（用户拍板的策略）。
        //
        // 多路径切分后，`maximum_match` 那条不再必然是首选候选走的那条：`xianjiaotongdaxue`
        // 首选「西安交通大学」实走 `xi|an|jiao|tong|da|xue`，而 mm 给的是 `xian|jiao|…`。
        // 显示 mm 就与用户看到的候选自相矛盾。候选自带 `boundary`（整句由解码器回填真实
        // 路径，词典命中则是词库真值），据此还原其切分即可，无须另建通道。
        //
        // 只在「无双拼、无手动分隔符、首选覆盖已完成音节前缀且带边界信息」时接管——
        // 双拼的 preedit 另有 build_raw_preedit 负责（下方覆盖），分隔符段的 `'` 是用户
        // 亲手打的硬边界不容改写，无边界信息则无从跟随。其余情形保持 mm 显示不变。
        //
        // 简拼/混合简拼另走一支：它们的 code 与击键**不同域**，故 `top.code == completed`
        // 恒不成立（`nhao` 一个完整音节都切不出，`completed` 是空串），此前因此完全没有
        // 分隔显示——`nhao` 原样显示为 `nhao`，`nh` 显示为 `nh`。见下方分支。
        if sp_result.is_none()
            && !has_sep
            && let Some(top) = candidates.first()
        {
            if top.boundary != 0 && top.code == completed && !completed.is_empty() {
                preedit_display = render_preedit(completed, top.boundary, &partial_syllable);
            } else if top.is_sentence && top.boundary != 0 && top.code == raw_input {
                // 混合整句（step 2b）：它的 code 是整串**击键**、boundary 也在击键空间
                // （简拼段每字母一位），故直接渲染击键串即可，得 `b'z'd'hao'bu'hao`。
                // 走不到上一支是因为 `completed` 对这类输入恒为空串——`bzdhaobuhao`
                // 从位置 0 就切不出完整音节。
                preedit_display = render_preedit(raw_input, top.boundary, "");
            } else if let Some(s) = self.abbrev_keystroke_preedit(top, raw_input) {
                preedit_display = s;
            }
        }

        // Fix A：双拼激活时，preedit 改为显示用户实际输入的原始按键（按双拼音节边界以 `'` 分隔），
        // 而非转换后的全拼。仅覆盖 preedit_display；候选/completed_syllables/partial_syllable/
        // consumed_length 仍保持全拼语义不变。
        // 双拼自身的音节切分恒为**默认**显示形态。
        let mut preedit_fullpinyin = String::new();
        let mut preedit_abbrev = String::new();
        // 双拼分段形态：非简拼候选（也就是绝大多数情形）的显示串，同时是 `preedit_pinyin`
        // 交出去的那一份——它必须**始终**是双拼切法，否则高亮从简拼候选移回双拼候选时
        // 协调器无处取回原形态。
        let mut sp_body = String::new();
        if let Some(r) = &sp_result {
            sp_body = build_raw_preedit(raw_input, r);
            preedit_display = sp_body.clone();
            // 简拼/混合简拼候选的**击键分段**（`wbwn` → `w'b'w'n`），供协调器按高亮切换
            // （见 `ConvertResult::preedit_abbrev`）。
            //
            // 双拼的 `build_raw_preedit` 按两键一音节切，对简拼击键给出的是无意义的分段
            // （`wbwn` 一段都切不出、`wfwt` 切成 `wf'wt`）——它答的是「这串按双拼怎么读」，
            // 而用户打的是每键一个声母。两种切法都成立，只能由高亮候选来选。
            //
            // 取**首个**简拼候选而非 `candidates.first()`：这一份是给「高亮到简拼候选时」
            // 用的，首选是不是简拼与它无关。同码的简拼候选彼此切法一致（同一条 mixed
            // pattern），故一份足够。
            preedit_abbrev = candidates
                .iter()
                .find_map(|c| self.abbrev_keystroke_preedit(c, raw_input))
                .unwrap_or_default();
            // 首选就是简拼候选 ⇒ 初显直接用简拼分段（overlay 模式如快捷输入/临拼只读
            // `preedit_display`，不走协调器的高亮跟随，靠的就是这一步）。
            if candidates.first().is_some_and(|c| c.is_abbrev) && !preedit_abbrev.is_empty() {
                preedit_display = preedit_abbrev.clone();
            }
            // 支路有产出时额外给出**全拼切分**，供协调器按高亮候选切换
            // （见 `ConvertResult::preedit_fullpinyin`）。
            //
            // ⚠️ **不能**在这里按「首选是不是全拼候选」就地把 preedit_display 改掉——首版即
            // 如此，真机现象是「翻页/移动光标到双拼候选后，编码栏还停在全拼拆分」：引擎只在
            // 按键时 convert 一次，而高亮是之后才移动的，就地算定的形态根本不会跟着变。
            // 形态选择必须交给协调器的 `effective_preedit_body`（由 `sync_preedit_to_highlight`
            // 在每次高亮变化时重算），引擎只负责把两种形态都交出去。
            if candidates.iter().any(|c| c.is_fullpinyin_fallback) {
                let fp = self.compose_segment(raw_input).0;
                // 与 `sp_body` 比而不是 `preedit_display`：这一步答的是「全拼切法与**双拼
                // 切法**是否不同」，而 preedit_display 上一步可能已被换成简拼分段。拿它作
                // 基准的话，`nihao` 这类两种切法本就相同的串会因为简拼覆盖而白给一份 fp。
                if fp != sp_body {
                    preedit_fullpinyin = fp;
                }
            }
        }

        let has_partial = !partial_syllable.is_empty();
        let is_empty = candidates.is_empty();

        // 候选调整（shadow）规则的归一编码。契约与各分支理由见 `ConvertResult::shadow_code`；
        // 这里只解释**双拼分支的两个条件为何缺一不可**：
        //
        // - `mixed_covered`：双拼转换结果完整覆盖了整串击键。不成立说明串里有拼不出音节的
        //   键（无匹配键对原样回写），`full_pinyin` 里混着未翻译的原始字母，不是干净的全拼域。
        //
        // 判据**只有** `mixed_covered` 这一条，两个看似更精细的备选都已被否掉：
        //
        // ⛔ `!stroke_is_plain_abbrev`（击键是不是简拼形态）—— 实测当场推翻。双拼两键一音节，
        //    韵母键的字母本身往往也是合法声母（小鹤 `c`=ao，而 `c` 是声母），`is_abbreviation`
        //    于是对绝大多数双拼两键击键判真，`hc` 首当其冲。用它会把双拼常态整体挡在归一之外，
        //    功能等于没做。那个变量答的是「要不要去查简拼索引」，不是「查到了没有」。
        //
        // ⛔ `!abbrev_full_hit`（简拼是否真的命中整串）—— 语义上更准，能彻底杜绝下面说的串扰，
        //    但它**依赖词库内容**：用户加一个简拼恰为 `hc` 的词，`hc` 的 key 就从 `hao` 摇摆成
        //    `hc`，此前那条置顶规则静默失效，删词又摇回来。双拼击键与用户词简拼同为两字母串，
        //    碰撞频繁。2026-08-11 用户拍板：**稳定性优先**——key 只取决于双拼布局，词库怎么变
        //    都不动。
        //
        // 由此接受一处已知的窄串扰：`nh` 归一为 `nang`，而双拼 `nh` 同时出简拼候选「你好」
        // （简拼索引按原始击键查，见 `abbr_query`）。若用户**既用全拼又用双拼**、且在全拼
        // `nang` 下置顶过词，那个词会在双拼敲 `nh` 时被顶上来。纯双拼用户不受影响：pin 与读取
        // 都落在同一个全拼 key 上，行为正确。实证见
        // `tests/pinyin_shadow_code_domain.rs::sp_abbrev_and_full_coexist_in_one_keystroke`。
        //
        // 全拼恒空串（＝落回击键，恒等变换）：不可剥 `'`，它是硬边界，`xi'an` 与 `xian`
        // 候选集不同，合并即变更全拼存量行为。
        let shadow_code = match &sp_result {
            Some(r) if mixed_covered => r.full_pinyin.clone(),
            _ => String::new(),
        };

        Ok(ConvertResult {
            candidates,
            // 拼音恒为拆分形态（供混输高亮跟随：高亮拼音候选时取此串）。
            //
            // 双拼下取 `sp_body`（双拼切法）**而非 `preedit_display`**：首选是简拼候选时
            // 后者已被换成简拼分段，若跟着一起换，高亮从简拼候选移回双拼候选时协调器就
            // 取不回双拼形态了——两种切法必须各有一个稳定的落点。
            preedit_pinyin: if sp_body.is_empty() {
                preedit_display.clone()
            } else {
                sp_body
            },
            preedit_display,
            preedit_fullpinyin,
            preedit_abbrev,
            // 码表整句专用，拼音引擎恒空（见 ConvertResult::preedit_codetable）。
            preedit_codetable: String::new(),
            completed_syllables,
            partial_syllable,
            has_partial,
            should_commit: false,
            commit_text: String::new(),
            is_empty,
            should_clear: false,
            // 拼音无「全码/空码补全」概念（`single_code_*` 是码表专属）。
            completion_hints: Vec::new(),
            shadow_code,
        })
    }

    fn reset(&self) {}

    fn engine_type(&self) -> EngineType {
        EngineType::Pinyin
    }

    /// 反查 `(code, text)` 在词典里的音节边界（点查，不做推断）。查不到返回 0。
    fn syllable_boundary_of(&self, code: &str, text: &str) -> u64 {
        self.dict
            .search_with_boundary(code)
            .into_iter()
            .find(|h| h.text == text)
            .map(|h| h.boundary)
            .unwrap_or(0)
    }

    /// 求解 `(code, text)` 的音节边界，兼作拼音词条合法性判据。
    ///
    /// ★★ 层序是 **层 2 → 层 4 → 层 3**，与设计文档 §3.1 的可信度排序**有意不同**：
    /// 层 3（`generate_word_pinyin`）要枚举读音笛卡尔积再回查词典，是三者里最贵的；
    /// 层 4 只在一个短码上建图，便宜得多，且唯一解时同样确定。故让层 4 先跑，
    /// **只在它给出多解时才请层 3 消歧** —— 绝大多数词条因此根本不触发层 3。
    ///
    /// ⚠️ 层 4 无解即判非法、不再试层 3。这是可证的而非偷懒：若推导码 flat 后等于
    /// `code`，其音节序列本身就是一条「音节数 == 字数」的合法路径，与层 4 无解矛盾。
    fn resolve_boundary(&self, code: &str, text: &str) -> BoundaryResolution {
        // 层 2：词典真值点查——最便宜也最权威。
        let exact = self.syllable_boundary_of(code, text);
        if exact != 0 {
            return BoundaryResolution::Exact(exact);
        }
        // bitmask 装不下 ⇒ 合法但无边界（既定降级语义），既不求解也不拒收。
        if code.len() > 64 {
            return BoundaryResolution::NoInfo;
        }
        let idx = self
            .char_pinyin_idx
            .get_or_init(|| CharPinyinIndex::build(&self.dict));
        // 层 4：字数约束求解。
        let Some((solved, ambiguous)) =
            generate::boundary_by_char_count(idx, &self.trie, code, text)
        else {
            return BoundaryResolution::Unresolvable;
        };
        if !ambiguous {
            return BoundaryResolution::Derived(solved);
        }
        // 层 3：仅在多解时出场——推导码与目标码逐字相同，则其切分是确定的。
        if let Some(spaced) = generate::generate_word_pinyin(&self.dict, idx, text) {
            let (flat, derived) = wind_store::wdict::split_spaced_code(&spaced);
            if flat == code && derived != 0 {
                return BoundaryResolution::Derived(derived);
            }
        }
        BoundaryResolution::Ambiguous(solved)
    }

    /// 为词语生成带空格的全拼音节码（多音字按词典权重消歧）。
    /// 单字读音索引按词典懒构建并缓存。含无读音字符时返回 `None`。
    fn generate_word_pinyin(&self, word: &str) -> Option<String> {
        let idx = self
            .char_pinyin_idx
            .get_or_init(|| CharPinyinIndex::build(&self.dict));
        generate::generate_word_pinyin(&self.dict, idx, word)
    }

    fn is_possible_pinyin_sequence(&self, prefix: &str) -> bool {
        // 条件1：整个前缀本身是某合法音节的前缀（如 zhon→zhong），长度 >=2 过滤单字母简拼。
        if prefix.len() >= 2 && self.trie.is_prefix(prefix) {
            return true;
        }
        // 条件2：从起始连续完整音节 + 合法尾部前缀。首音节须非单字母。
        let (completed, end_pos) = self.contiguous_completed_from_start(prefix);
        if completed.is_empty() || completed[0].len() < 2 {
            return false;
        }
        if end_pos >= prefix.len() {
            return true;
        }
        self.trie.is_prefix(&prefix[end_pos..])
    }

    fn is_whole_syllable_pinyin(&self, prefix: &str) -> bool {
        // 整体即单个完整音节（wang/shen 等填满码长的场景）。
        if self.trie.is_syllable(prefix) {
            return true;
        }
        // 多音节：连续完整音节恰好覆盖整个前缀，且首音节非单字母简拼。
        let (completed, end_pos) = self.contiguous_completed_from_start(prefix);
        if completed.is_empty() || completed[0].len() < 2 {
            return false;
        }
        end_pos == prefix.len()
    }

    fn has_non_initial_single_letter_syllable(&self, prefix: &str) -> bool {
        let (completed, _) = self.contiguous_completed_from_start(prefix);
        completed.iter().skip(1).any(|s| s.len() == 1)
    }

    fn completed_syllable_count(&self, prefix: &str) -> usize {
        self.contiguous_completed_from_start(prefix).0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
    use wind_dict::codetable::CodetableDict;
    use wind_store::Store;

    fn empty_engine() -> PinyinEngine {
        PinyinEngine::new(
            Config::default(),
            CachedDict::Memory(CodetableDict::empty()),
        )
    }

    /// 把补全召回门槛设回旧出厂值 2 / 3 的配置。
    ///
    /// 出厂 `completion_min_syllables` 现为 4（对齐 librime/fcitx5），语义是「输入不足
    /// 4 个音节时上限收紧到 `started` 本身，不预测用户没打的内容」。用 2~3 音节输入验证
    /// **排序 / 上浮**的用例因此会在召回层就拿不到样本，断言以「候选缺失」失败，而要验
    /// 的逻辑一次都没执行到。
    ///
    /// ⇒ 凡是「已召回之后谁排前面」的用例都该用本函数，把门槛这个变量从用例里摘掉。
    /// 门槛自身的出厂行为由 `wind-coordinator` 的 `pinyin_completion_recall_gate` 守。
    fn relaxed_completion_config() -> Config {
        Config {
            completion_min_syllables: 2,
            completion_max_extra_syllables: 3,
            ..Config::default()
        }
    }

    // ── 音节分析（顶码歧义裁决用；对齐 Go isPossiblePinyinSequence / isWholeSyllablePinyin /
    //    hasNonInitialSingleLetterSyllable）。trie 为封闭标准音节集，不依赖词典。──

    #[test]
    fn possible_pinyin_sequence_matches_go_cases() {
        let e = empty_engine();
        // 完整音节 / 音节前缀 / 完整音节+尾部前缀 → true
        assert!(e.is_possible_pinyin_sequence("wang")); // 单完整音节
        assert!(e.is_possible_pinyin_sequence("zhon")); // zhong 的前缀
        assert!(e.is_possible_pinyin_sequence("yans")); // yan + 尾部前缀 s
        assert!(e.is_possible_pinyin_sequence("naap")); // na + a + 前缀 p
        // 非拼音 / 首音节单字母 → false
        assert!(!e.is_possible_pinyin_sequence("rcqn")); // 无完整音节也非前缀
        assert!(!e.is_possible_pinyin_sequence("gggg")); // g 非合法音节/前缀
        assert!(!e.is_possible_pinyin_sequence("abcd")); // 首音节 a 为单字母
    }

    #[test]
    fn whole_syllable_pinyin_matches_go_cases() {
        let e = empty_engine();
        assert!(e.is_whole_syllable_pinyin("wang")); // 单完整音节
        assert!(e.is_whole_syllable_pinyin("aipu")); // ai+pu 恰好覆盖
        assert!(!e.is_whole_syllable_pinyin("zhon")); // 残缺前缀（非完整音节）
        assert!(!e.is_whole_syllable_pinyin("yans")); // yan + 残缺 s
        assert!(!e.is_whole_syllable_pinyin("abcd")); // 首音节单字母简拼
    }

    #[test]
    fn non_initial_single_letter_syllable_matches_go_cases() {
        let e = empty_engine();
        assert!(e.has_non_initial_single_letter_syllable("naap")); // na + a（第二音节单字母）
        assert!(!e.has_non_initial_single_letter_syllable("yans")); // yan + 残缺 s（残缺不计）
        assert!(!e.has_non_initial_single_letter_syllable("aipu")); // ai + pu 皆双字母
        assert!(!e.has_non_initial_single_letter_syllable("abcd")); // 首位单字母不算「非首位」
    }

    /// 构造含指定 (code, text) 词条的最小拼音引擎（每条 weight=100，order 递增）。
    fn engine_with_words(words: &[(&str, &str)]) -> PinyinEngine {
        let mut raw = CodetableDict::empty();
        for (i, (code, text)) in words.iter().enumerate() {
            raw.merge_single(code.to_string(), text.to_string(), 100, i as i32);
        }
        PinyinEngine::new(Config::default(), CachedDict::Memory(raw))
    }

    /// Task 8 Step 2：手动分隔符强制音节硬边界。
    /// 词典含 "xian"→"先" 与 "xi"/"an" 单字；带分隔符 xi'an 强制切分 [xi,an]，
    /// 跨界单音节词「先」(code=xian 却仅 1 字) 不得出现；preedit 保留手动 `'`。
    #[test]
    fn separator_forces_syllable_boundary() {
        let e = engine_with_words(&[("xian", "先"), ("xi", "西"), ("an", "安")]);
        let r = e.convert("xi'an", 50).unwrap();
        assert!(
            !r.candidates.iter().any(|c| c.text == "先"),
            "分隔符应阻止跨界音节 xian 匹配，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
        assert!(
            r.preedit_display.contains('\''),
            "preedit 应保留手动分隔符，实际: {:?}",
            r.preedit_display
        );
        assert!(
            !r.candidates.is_empty(),
            "分隔符切分后仍应有候选（如「西」）"
        );
    }

    /// Task 8 Step 2：末尾分隔符必须立即显示，且不清空候选。
    #[test]
    fn trailing_separator_kept_in_preedit() {
        let e = engine_with_words(&[("ni", "你")]);
        let r = e.convert("ni'", 50).unwrap();
        assert!(
            r.preedit_display.ends_with('\''),
            "末尾分隔符必须立即显示，实际: {:?}",
            r.preedit_display
        );
        assert!(!r.candidates.is_empty(), "末尾分隔符不应清空候选");
    }

    /// Task 8 自审：五个边界（空段/开头 '/连续 ''/纯 '/末尾 '）均不 panic，
    /// 且手动分隔符在 preedit 中原样保留。
    #[test]
    fn separator_edge_cases_no_panic() {
        let e = engine_with_words(&[("ni", "你"), ("hao", "好"), ("xi", "西"), ("an", "安")]);

        // 开头 '：xi 段仍应产候选，preedit 以 ' 起头
        let r = e.convert("'xi", 20).unwrap();
        assert!(r.preedit_display.starts_with('\''), "开头分隔符应保留");
        assert!(r.candidates.iter().any(|c| c.text == "西"));

        // 连续 ''：等价单边界，preedit 保留双分隔符
        let r = e.convert("ni''hao", 20).unwrap();
        assert!(r.preedit_display.contains("''"), "连续分隔符应原样保留");
        assert!(r.candidates.iter().any(|c| c.text == "你"));

        // 纯 ' / 连续纯 '：无拼音可查，无候选、仅回显分隔符，不 panic
        for pure in ["'", "''", "'''"] {
            let r = e.convert(pure, 20).unwrap();
            assert!(r.candidates.is_empty(), "纯分隔符输入 {pure:?} 不应有候选");
            assert_eq!(r.preedit_display, pure, "纯分隔符应原样回显");
        }
    }

    fn tmp_store(name: &str) -> Arc<Store> {
        let p = std::env::temp_dir().join(format!("wind_pinyin_{name}.redb"));
        let _ = std::fs::remove_file(&p);
        Arc::new(Store::open(&p).unwrap())
    }

    /// L 造词显现：挂上用户/临时层后，拼音造的词应进入候选（即便主词典为空）。
    #[test]
    fn store_layer_words_appear_in_candidates() {
        let store = tmp_store("layer_show");
        store
            .add_user_word("pinyin", "nihao", "你好", 500, 0)
            .unwrap();
        store
            .learn_temp_word("pinyin", "lanshou", "蓝瘦", 800, 0)
            .unwrap();
        let dm = DictManager::new();
        dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
            store.clone(),
            "pinyin",
        )));
        dm.register_layer(Box::new(wind_dict::StoreTempLayer::new(
            store.clone(),
            "pinyin",
        )));
        let engine = empty_engine().with_store_layers(Arc::new(dm));

        // 整串精确命中用户词
        let r = engine.convert("nihao", 20).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "你好"),
            "用户造词「你好」应出现在拼音候选"
        );
        // 临时词同样显现
        let r2 = engine.convert("lanshou", 20).unwrap();
        let shou = r2.candidates.iter().find(|c| c.text == "蓝瘦");
        assert!(shou.is_some(), "临时造词「蓝瘦」应出现在拼音候选");
        assert_eq!(
            shou.unwrap().source,
            CandidateSource::Pinyin,
            "来源应标为拼音"
        );
    }

    /// 无 store 层时行为不变（不 panic、空词典无候选）。
    #[test]
    fn no_store_layer_is_inert() {
        let engine = empty_engine();
        let r = engine.convert("nihao", 20).unwrap();
        assert!(r.candidates.is_empty(), "空词典无 store 层应无候选");
    }

    /// 部分消费：用户词码是输入的前缀（nihao ⊂ nihaoma）→ consumed_length 标为前缀长度，
    /// 选中后保留剩余拼音续转（分段上屏）。
    #[test]
    fn store_word_prefix_marks_partial_consumption() {
        let store = tmp_store("layer_partial");
        store
            .add_user_word("pinyin", "nihao", "你好", 500, 0)
            .unwrap();
        let dm = DictManager::new();
        dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
            store.clone(),
            "pinyin",
        )));
        let engine = empty_engine().with_store_layers(Arc::new(dm));
        let r = engine.convert("nihaoma", 20).unwrap();
        let c = r.candidates.iter().find(|c| c.text == "你好");
        assert!(c.is_some(), "前缀用户词应作为分段候选出现");
        assert_eq!(
            c.unwrap().consumed_length,
            "nihao".len(),
            "应只消费前缀 nihao"
        );
    }

    /// 构造「带 qing 同音字洪泛的系统词典 + 用户长词」的引擎（复用于长词上浮系列测试）。
    fn engine_with_qing_flood_and_user_word(store_name: &str) -> PinyinEngine {
        let mut raw = CodetableDict::empty();
        for (i, ch) in ["清", "青", "情", "请", "轻", "晴", "倾", "氢", "卿", "顷"]
            .iter()
            .enumerate()
        {
            raw.merge_single(
                "qing".to_string(),
                ch.to_string(),
                1000 - i as i32,
                i as i32,
            );
        }
        raw.merge_single("feng".to_string(), "风".to_string(), 900, 0);
        raw.merge_single("qingfeng".to_string(), "清风".to_string(), 800, 0);

        let store = tmp_store(store_name);
        // boundary=0：模拟手输码用户词（无音节真值）→ 走 completed_syls>=3 兜底门槛。
        store
            .add_user_word("pinyin", "qingfengshurufa", "清风输入法", 5000, 0)
            .unwrap();
        let dm = DictManager::new();
        dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(store, "pinyin")));
        // 门槛设回 2：本组用例用 `qingfengshu`(3 音节) 验用户长词上浮，出厂的 4 会挡掉样本。
        PinyinEngine::new(relaxed_completion_config(), CachedDict::Memory(raw))
            .with_store_layers(Arc::new(dm))
    }

    /// 【核心回归】用户长词「清风输入法」在打到第 3-4 音节时应上浮到同音子短语之上，
    /// 而非被压到候选最底（本次修复的用户反馈现场：打到完整全拼才出现）。
    #[test]
    fn user_long_word_surfaces_at_partial_pinyin() {
        let engine = engine_with_qing_flood_and_user_word("long_word_surface");

        for input in ["qingfengshu", "qingfengshuruf"] {
            let r = engine.convert(input, 300).unwrap();
            let pos_word = r
                .candidates
                .iter()
                .position(|c| c.text == "清风输入法")
                .unwrap_or_else(|| panic!("{input}: 清风输入法 应在候选中"));
            let pos_qing = r
                .candidates
                .iter()
                .position(|c| c.text == "清")
                .expect("清 子短语应存在");
            assert!(
                pos_word < pos_qing,
                "{input}: 用户长词应上浮到同音子短语「清」之上，实际 word@{pos_word} qing@{pos_qing}: {:?}",
                r.candidates
                    .iter()
                    .take(5)
                    .map(|c| &c.text)
                    .collect::<Vec<_>>()
            );
            assert!(
                r.candidates[pos_word].is_promoted_completion,
                "{input}: 上浮的用户长词应标 is_promoted_completion"
            );
            // is_prefix 结构真值保持不变（码确实更长）。
            assert!(
                r.candidates[pos_word].is_prefix,
                "{input}: is_prefix 结构事实（码更长）不应被抹掉"
            );
        }

        // 完整全拼：精确命中，本就在首位（is_prefix=false，非提升）。
        let r_full = engine.convert("qingfengshurufa", 300).unwrap();
        assert_eq!(
            r_full.candidates[0].text, "清风输入法",
            "完整全拼应精确命中首位"
        );
        assert!(
            !r_full.candidates[0].is_prefix,
            "完整全拼是精确匹配，非补全"
        );
        assert!(
            !r_full.candidates[0].is_promoted_completion,
            "精确命中不经上浮通道"
        );
    }

    /// 【边界守卫】音节太少时不上浮：`qing`(1 音节) / `qingfeng`(2 音节) 下用户长词
    /// 仍沉在补全层，且精确词「清风」在 qingfeng 下仍居首——不被用户长词越过。
    #[test]
    fn user_long_word_not_promoted_when_too_few_syllables() {
        let engine = engine_with_qing_flood_and_user_word("long_word_guard");

        // qing：1 音节，boundary=0 兜底门槛 completed_syls>=3 未达 → 不上浮。
        let r1 = engine.convert("qing", 300).unwrap();
        if let Some(p) = r1.candidates.iter().position(|c| c.text == "清风输入法") {
            assert!(
                !r1.candidates[p].is_promoted_completion,
                "qing(1 音节)不应上浮用户长词"
            );
        }

        // qingfeng：2 音节，未达门槛 → 不上浮；精确「清风」应排在用户长词之前。
        let r2 = engine.convert("qingfeng", 300).unwrap();
        let pos_qf = r2.candidates.iter().position(|c| c.text == "清风");
        let pos_word = r2.candidates.iter().position(|c| c.text == "清风输入法");
        if let Some(pw) = pos_word {
            assert!(
                !r2.candidates[pw].is_promoted_completion,
                "qingfeng(2 音节)不应上浮用户长词"
            );
            if let Some(pqf) = pos_qf {
                assert!(pqf < pw, "qingfeng 下精确「清风」应排在用户长词之前");
            }
        }
    }

    /// 上浮判据单测：距词尾 ≤ `max_extra`（有边界）/ 已打 ≥3 音节（无边界）才上浮。
    #[test]
    fn promote_user_completion_thresholds() {
        // 5 音节词（boundary 五个音节起始位；此处只关心 count_ones()=5）。
        let b5: u64 = 0b11111; // 5 个置位（count_ones=5，模拟 5 音节词）
        assert_eq!(b5.count_ones(), 5);
        // 以 max_extra = 2 复核历史档位（本判据长期硬编码的那个值）。
        // 无残码：completed_syls 即 started。
        assert!(
            !should_promote_user_completion(2, false, b5, 2),
            "5 音节词打 2 音节剩 3 > 2，不上浮"
        );
        assert!(
            should_promote_user_completion(3, false, b5, 2),
            "5 音节词打 3 音节剩 2 = 2，上浮"
        );
        assert!(
            should_promote_user_completion(4, false, b5, 2),
            "5 音节词打 4 音节剩 1，上浮"
        );
        assert!(
            !should_promote_user_completion(1, false, b5, 2),
            "1 音节 < 2，无条件不上浮"
        );
        // 尾部残码算作已起头的一个音节：qingfengs = 2 完整音节 + 残码 → started 3 → 上浮。
        assert!(
            should_promote_user_completion(2, true, b5, 2),
            "2 完整音节 + 残码（started 3, 剩 2）应上浮"
        );
        assert!(
            !should_promote_user_completion(1, true, b5, 2),
            "1 完整音节 + 残码（started 2, 剩 3 > 2）不上浮"
        );
        // 无边界兜底：started>=3，与 max_extra 无关（算不出剩余）。
        for max_extra in [0, 2, 10] {
            assert!(
                !should_promote_user_completion(2, false, 0, max_extra),
                "无边界 2 音节不上浮（max_extra={max_extra} 不参与）"
            );
            assert!(
                should_promote_user_completion(3, false, 0, max_extra),
                "无边界 3 音节上浮（max_extra={max_extra} 不参与）"
            );
            assert!(
                should_promote_user_completion(2, true, 0, max_extra),
                "无边界 2 音节 + 残码（started 3）上浮"
            );
        }
    }

    /// 距词尾上限**跟着配置走**：这是「设置对用户词库长词无效」那条报障的判据。
    ///
    /// 真机现场：11 音节的用户词、`max_extra_syllables = 10`，打 `qingfengshurufa`
    /// （started 5，剩 6）翻遍 16 页找不到，必须打到剩 2 才出现 —— 分界点正是旧的
    /// 硬编码 2。剩 6 在 `max_extra = 10` 下必须上浮。
    ///
    /// ⚠️ 不上浮的后果不止「排在后面」：引擎 `sort_by` 紧跟 `truncate`，沉到最底
    /// 在候选数超上限时**等于被丢弃**。所以这条断言守的是可见性，不是次序。
    #[test]
    fn promote_user_completion_follows_max_extra_config() {
        // 11 音节词（count_ones = 11）。
        let b11: u64 = (1 << 11) - 1;
        assert_eq!(b11.count_ones(), 11);

        // started = 5（qingfengshurufa），剩 6。
        assert!(
            !should_promote_user_completion(5, false, b11, 2),
            "max_extra=2：剩 6 > 2，不上浮（旧硬编码行为，报障现场）"
        );
        assert!(
            should_promote_user_completion(5, false, b11, 10),
            "max_extra=10：剩 6 ≤ 10，必须上浮 —— 用户把设置调宽就是这个意思"
        );
        assert!(
            should_promote_user_completion(5, false, b11, 6),
            "max_extra=6：剩 6 恰好等于上限，边界值取闭区间（与召回层同口径）"
        );
        assert!(
            !should_promote_user_completion(5, false, b11, 5),
            "max_extra=5：剩 6 > 5，不上浮"
        );

        // 与召回层同口径：召回上限是 word_syls ≤ started + max_extra，
        // 即 remaining ≤ max_extra。召回得进来的，就该上浮得起来。
        for started in 2..=11usize {
            let remaining = 11usize.saturating_sub(started);
            let max_extra = 10u32;
            let recalled = 11 <= started + max_extra as usize;
            let promoted = should_promote_user_completion(started, false, b11, max_extra);
            assert_eq!(
                promoted,
                recalled && started >= 2,
                "started={started} 剩 {remaining}：召回与上浮判据须同口径"
            );
        }
    }

    /// 补全折扣：每多一个未输入音节，权重减半（对齐 librime `kCompletionPenalty`
    /// 与 fcitx5 `overLengthCost`，见 [`COMPLETION_WEIGHT_DISCOUNT`]）。
    #[test]
    fn completion_penalty_halves_per_extra_syllable() {
        // extra=0（候选音节数 == 已完成音节数，或 boundary=0 算不出）：原样不动。
        assert_eq!(completion_penalized(5328, 0), 5328);
        // 「你好」nih 下 extra=1。
        assert_eq!(completion_penalized(5328, 1), 2664);
        // 「你会发现」nih 下 extra=3：13330 × 0.125 = 1666.25 → 1666，
        // 于是落到「你好」2664 之后 —— 这正是本机制要达成的效果。
        assert_eq!(completion_penalized(13330, 3), 1666);
        // 饱和到 >=1：折扣不该把候选压成 0 而改变它与「无权重」候选的关系。
        assert_eq!(completion_penalized(4, 10), 1);
        // weight <= 1 短路，不因折扣变负/变 0。
        assert_eq!(completion_penalized(1, 5), 1);
        assert_eq!(completion_penalized(0, 5), 0);
    }

    /// 用户造词的简拼应可命中（现算，非离线索引）：用户新增「菜鸟驿站」（全拼
    /// cainiaoyizhan），键入简拼 cnyz 应能查到；临时词同理。
    #[test]
    fn store_layer_words_match_abbreviation() {
        let store = tmp_store("layer_abbrev");
        store
            .add_user_word("pinyin", "cainiaoyizhan", "菜鸟驿站", 500, 0)
            .unwrap();
        store
            .learn_temp_word("pinyin", "lanshoubing", "蓝瘦蘑菇", 800, 0)
            .unwrap();
        let dm = DictManager::new();
        dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
            store.clone(),
            "pinyin",
        )));
        dm.register_layer(Box::new(wind_dict::StoreTempLayer::new(
            store.clone(),
            "pinyin",
        )));
        let engine = empty_engine().with_store_layers(Arc::new(dm));

        let r = engine.convert("cnyz", 20).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "菜鸟驿站"),
            "简拼 cnyz 应命中用户造词「菜鸟驿站」"
        );

        let r2 = engine.convert("lsb", 20).unwrap();
        assert!(
            r2.candidates.iter().any(|c| c.text == "蓝瘦蘑菇"),
            "简拼 lsb 应命中临时造词「蓝瘦蘑菇」"
        );

        // 全拼整串输入仍应正常命中（无回归）
        let r3 = engine.convert("cainiaoyizhan", 20).unwrap();
        assert!(r3.candidates.iter().any(|c| c.text == "菜鸟驿站"));
    }

    /// `enable_abbrev=false`（混输经 schema.mix.enable_pinyin_abbrev 注入）时不产简拼候选，
    /// 但全拼一切照旧。与上一个用例同构，只翻转开关——用于锁住「关掉的是简拼、不是拼音」。
    #[test]
    fn abbrev_disabled_suppresses_abbrev_candidates_only() {
        let store = tmp_store("layer_abbrev_off");
        store
            .add_user_word("pinyin", "cainiaoyizhan", "菜鸟驿站", 500, 0)
            .unwrap();
        let dm = DictManager::new();
        dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
            store.clone(),
            "pinyin",
        )));
        let engine = PinyinEngine::new(
            Config {
                enable_abbrev: false,
                ..Default::default()
            },
            CachedDict::Memory(CodetableDict::empty()),
        )
        .with_store_layers(Arc::new(dm));

        let r = engine.convert("cnyz", 20).unwrap();
        assert!(
            !r.candidates.iter().any(|c| c.text == "菜鸟驿站"),
            "关闭简拼后 cnyz 不应命中「菜鸟驿站」"
        );

        // 全拼不受影响——这一条是关键：开关关的是简拼，不是拼音本身。
        let r2 = engine.convert("cainiaoyizhan", 20).unwrap();
        assert!(
            r2.candidates.iter().any(|c| c.text == "菜鸟驿站"),
            "关闭简拼不得影响全拼命中"
        );
    }

    /// C1：query→原始输入空间的 consumed 回映射。无 `'` 恒等；边界紧跟 `'` 归入已消费侧；
    /// 连续 `''` 一并吸收；越过分隔符时正确计数；nih'ao 段内残码边界不 panic。
    /// Task 1.4 TDD：with_fuzzy builder 注入的配置应被引擎持有（探针验证）。
    #[test]
    fn engine_applies_fuzzy_config() {
        let dict = CachedDict::Memory(CodetableDict::empty());
        let fz = FuzzyConfig {
            zh_z: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), dict).with_fuzzy(fz);
        assert!(eng.fuzzy_zh_z(), "with_fuzzy 注入的 zh_z=true 应被引擎持有");
    }

    /// Task 4.1 TDD Step 2：多音节双拼——consumed_length 必须回算为双拼键数，
    /// compute_composition 不能对已是全拼的串再做一次双拼转换。
    /// 输入小鹤双拼 "nihc"（ni+hc → 全拼 "nihao"），词典含「你好」。
    #[test]
    fn pinyin_engine_shuangpin_multisyllable_consumed_length() {
        // 构造含 "nihao"->"你好" 的最小词典
        let mut raw = CodetableDict::empty();
        raw.merge_single("nihao".to_string(), "你好".to_string(), 200, 0);
        raw.merge_single("ni".to_string(), "你".to_string(), 100, 1);
        let dict = CachedDict::Memory(raw);

        let schema_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
        let conv = ShuangpinConverter::new(layout);

        let eng = PinyinEngine::new(Config::default(), dict).with_shuangpin(conv);
        // 小鹤双拼 "nihc" → 全拼 "nihao"
        let r = eng.convert("nihc", 10).unwrap();

        // 1. 候选含「你好」
        assert!(
            r.candidates.iter().any(|c| c.text == "你好"),
            "双拼输入 \"nihc\" 应包含候选「你好」，实际候选: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );

        // 2. 「你好」的 consumed_length 必须是双拼键数 4，而非全拼字节数 5
        let nihao = r.candidates.iter().find(|c| c.text == "你好").unwrap();
        assert_eq!(
            nihao.consumed_length, 4,
            "「你好」consumed_length 应为双拼键数 4（\"nihc\" 的长度），实际为 {}",
            nihao.consumed_length
        );
    }

    // ===== 全拼降级支路（双拼方案下允许全拼输入，`allow_full_pinyin`）=====

    /// 构造「小鹤双拼 + 可控开关」的引擎。`entries` 为 `(文本, 空格分隔的音节码, 权重)`。
    ///
    /// ⚠️⚠️ **必须走 rime 源格式，不可用 `merge_single`**：后者造出的条目 `boundary` 恒 0，
    /// 而双拼边界校验遇 0 一律放行 —— 拿它做夹具等于把本组用例要验的那道校验整个关掉。
    /// 首版即栽于此：`nihao` 在开关**关闭**时照样出「你好」（双拼流的命中没被校验拦下），
    /// 于是「支路补上了被校验删掉的候选」这件事根本没被测到，4 条用例全是假前提。
    ///
    /// `tag` 用于隔离临时文件名——多个用例并发跑，共用一个文件名会互相截断。
    fn sp_fp_engine(tag: &str, entries: &[(&str, &str, i32)], allow: bool) -> PinyinEngine {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("wind_fp_fallback_{tag}.dict.yaml"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: py\n...").unwrap();
            for (text, code, w) in entries {
                writeln!(f, "{text}\t{code}\t{w}").unwrap();
            }
        }
        let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
        let _ = std::fs::remove_file(&path);

        let schema_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
        let cfg = Config {
            allow_full_pinyin: allow,
            ..Config::default()
        };
        PinyinEngine::new(cfg, dict).with_shuangpin(ShuangpinConverter::new(layout))
    }

    fn texts(r: &ConvertResult) -> Vec<&str> {
        r.candidates.iter().map(|c| c.text.as_str()).collect()
    }

    /// ★★ 核心用例，同时**锁住支路的位置**。
    ///
    /// 双拼下按全拼打 `nihao`（5 键）应出「你好」。这条之所以是位置的守卫：`nihao` 的双拼
    /// 解释 ni|ha|o 拼起来**与击键同形**，step1 早就精确命中了「你好」，只是随后被边界校验
    /// 删掉。支路若排在校验之前，会因同文查重让位给那条注定被删的候选 ⇒ 最终一条都不剩，
    /// 且现象是「开关像没生效」。
    #[test]
    fn full_pinyin_recalls_word_that_shuangpin_boundary_check_rejects() {
        let eng = sp_fp_engine(
            "recall",
            &[("你好", "ni hao", 200), ("你", "ni", 100)],
            true,
        );
        let r = eng.convert("nihao", 20).unwrap();
        let c = r
            .candidates
            .iter()
            .find(|c| c.text == "你好")
            .unwrap_or_else(|| panic!("应出「你好」，实际候选: {:?}", texts(&r)));
        assert!(c.is_fullpinyin_fallback, "应带全拼支路的来源标记");
        assert_eq!(
            c.consumed_length, 5,
            "consumed 必须是**击键域**的 5，不可再过 map_consumed_length 回映射成双拼键数"
        );
    }

    /// 反向对照：开关关闭时行为与改动前完全一致（`nihao` 的正确双拼打法是 `nihc`）。
    #[test]
    fn full_pinyin_off_keeps_shuangpin_only_behavior() {
        let eng = sp_fp_engine("off", &[("你好", "ni hao", 200), ("你", "ni", 100)], false);
        let r = eng.convert("nihao", 20).unwrap();
        assert!(
            !r.candidates.iter().any(|c| c.text == "你好"),
            "开关关闭时不该出「你好」，实际候选: {:?}",
            texts(&r)
        );
    }

    /// 闸门：双拼击键串不得触发支路——这是「双拼体验不被污染」的结构性保证。
    #[test]
    fn full_pinyin_gate_rejects_shuangpin_strokes() {
        let eng = sp_fp_engine("gate", &[("你好", "ni hao", 200)], true);
        // `nihc` = 「你好」的小鹤双拼码：ni + `hc`，而 `hc` 既不成音节也不是音节前缀
        // ⇒ 只切得出 1 个音节 ⇒ 不足 FULL_PINYIN_MIN_SYLLABLES。
        assert!(
            eng.full_pinyin_gate("nihc").is_none(),
            "nihc（双拼码）不该触发全拼支路"
        );
        assert!(
            eng.full_pinyin_gate("womf").is_none(),
            "womf（=women 的双拼码）不该触发"
        );
        // 尾部残码须是合法音节前缀：`m` 可以，`xyz` 不行。
        assert!(eng.full_pinyin_gate("nihaom").is_some(), "nihaom 应放行");
        assert!(
            eng.full_pinyin_gate("nihaoxyz").is_none(),
            "尾部 xyz 不是音节前缀，应拒绝"
        );
        // 真·全拼串放行。
        assert!(eng.full_pinyin_gate("nihao").is_some());
    }

    /// 支路不得干扰双拼流：`nihc` 走双拼正路，候选不带降级标志、consumed 仍是键数。
    #[test]
    fn full_pinyin_branch_does_not_disturb_shuangpin_path() {
        let eng = sp_fp_engine(
            "undisturbed",
            &[("你好", "ni hao", 200), ("你", "ni", 100)],
            true,
        );
        let r = eng.convert("nihc", 20).unwrap();
        let c = r
            .candidates
            .iter()
            .find(|c| c.text == "你好")
            .unwrap_or_else(|| panic!("双拼正确打法应出「你好」，实际: {:?}", texts(&r)));
        assert!(
            !c.is_fullpinyin_fallback,
            "这条由双拼流产出，不该带降级标志"
        );
        assert_eq!(c.consumed_length, 4, "双拼流的 consumed 仍是键数 4");
    }

    /// 两条流都解释得出同一个词时，**留下双拼那条**（不带降级标志、不沉底）。
    /// `nini`：双拼 n+i / n+i → "nini"，全拼 ni|ni 也是 "nini"，两域同形同码。
    #[test]
    fn shuangpin_candidate_wins_when_both_domains_agree() {
        let eng = sp_fp_engine("agree", &[("妮妮", "ni ni", 150)], true);
        let r = eng.convert("nini", 20).unwrap();
        let hits: Vec<_> = r.candidates.iter().filter(|c| c.text == "妮妮").collect();
        assert_eq!(hits.len(), 1, "同文候选不该重复，实际: {:?}", texts(&r));
        assert!(
            !hits[0].is_fullpinyin_fallback,
            "双拼也解释得出时，留下的必须是双拼那条"
        );
    }

    /// 层级：**低置信**全拼候选（前缀补全）沉底，**高置信**（精确整词 + 消费整串）不沉底。
    ///
    /// ⚠️ 这条曾断言「全拼候选一律排在双拼候选之后」，随真机反馈反转 —— `zaijian` 的正解
    /// 「再见」当时被整层压到第 8 位，沉底沉掉的不是噪音而是答案本身。「双拼优先」现由
    /// 同文去重那一层保住（consumed 相同则留双拼那条），不再靠层级键一刀切。
    #[test]
    fn full_pinyin_low_confidence_sinks_but_exact_word_does_not() {
        let eng = sp_fp_engine(
            "sink",
            &[
                ("你好", "ni hao", 200),
                ("你好吗", "ni hao ma", 5000),
                ("你", "ni", 100),
            ],
            true,
        );
        let r = eng.convert("nihao", 20).unwrap();
        let exact = r
            .candidates
            .iter()
            .find(|c| c.text == "你好")
            .unwrap_or_else(|| panic!("应出精确整词「你好」，实际: {:?}", texts(&r)));
        assert!(exact.is_fullpinyin_fallback, "来源标记：来自支路");
        assert!(
            !exact.is_prefix && !exact.is_partial,
            "精确整词＝高置信（cmp_match_layers 据此免除沉底）"
        );

        // 「你好吗」码更长＝前缀补全＝在预测用户还没打的音节 ⇒ 低置信，沉底。
        // 它的 weight(5000) 远高于「你好」(200)，若不沉底就会靠权重反超——正是要防的。
        if let Some(compl) = r.candidates.iter().find(|c| c.text == "你好吗") {
            assert!(
                compl.is_fullpinyin_fallback && compl.is_prefix,
                "前缀补全＝低置信，cmp_match_layers 据此沉底（否则高权重补全会盖过精确整词）"
            );
            let p_exact = r.candidates.iter().position(|c| c.text == "你好").unwrap();
            let p_compl = r
                .candidates
                .iter()
                .position(|c| c.text == "你好吗")
                .unwrap();
            assert!(
                p_exact < p_compl,
                "精确整词须先于前缀补全，实际: {:?}",
                texts(&r)
            );
        }
    }

    /// 同文去重取「解释得更多的那条」，而非无条件保留先到者。
    ///
    /// 真机 `zaijian`：双拼流的简拼前缀回退先以 `consumed=4` 放入「再见」，支路随后以
    /// `consumed=7` 完整解释同一个词——若让位给先到者，用户选中后缓冲会凭空剩下 `ian`。
    #[test]
    fn full_pinyin_replaces_partial_duplicate_with_complete_one() {
        let eng = sp_fp_engine("dedup", &[("再见", "zai jian", 2837)], true);
        let r = eng.convert("zaijian", 20).unwrap();
        let c = r
            .candidates
            .iter()
            .find(|c| c.text == "再见")
            .unwrap_or_else(|| panic!("应出「再见」，实际: {:?}", texts(&r)));
        assert_eq!(
            c.consumed_length,
            "zaijian".len(),
            "必须是完整解释（7 键）那条；留下半截解释会让上屏后残留余码"
        );
        assert_eq!(
            r.candidates.iter().filter(|c| c.text == "再见").count(),
            1,
            "同文不得重复"
        );
    }

    /// 模糊音：支路与双拼流吃同一套 `[schema.pinyin.fuzzy]` 设置。
    ///
    /// 首版刻意不做（「降级通道不做二次放大」），被真机反馈否掉——同一个人同一套模糊音配置
    /// 在两条流下表现不一致本身就是缺陷。惩罚由 `lookup_with_fuzzy` 的 `fuzzy_penalized` 施加。
    #[test]
    fn full_pinyin_honors_fuzzy_settings() {
        let mut eng = sp_fp_engine("fuzzy", &[("中国", "zhong guo", 9000)], true);
        // zh↔z：打 `zongguo` 应经模糊命中「中国」。
        eng.fuzzy_config.zh_z = true;
        let r = eng.convert("zongguo", 20).unwrap();
        let c = r
            .candidates
            .iter()
            .find(|c| c.text == "中国")
            .unwrap_or_else(|| panic!("开 zh_z 后 zongguo 应出「中国」，实际: {:?}", texts(&r)));
        assert!(c.is_fuzzy, "应标记为模糊命中（据此施加权重折扣）");
    }

    /// preedit 跟随首选：首选是全拼候选时，编码栏按**全拼**切分显示。
    /// 词典只留 `nihao`，双拼流一条候选都产不出，支路的「你好」即首选。
    #[test]
    fn engine_exposes_both_preedit_forms() {
        let eng = sp_fp_engine("preedit", &[("你好", "ni hao", 200)], true);
        let r = eng.convert("nihao", 20).unwrap();
        assert_eq!(
            r.preedit_display, "ni'ha'o",
            "默认形态恒为**双拼自己的**切分，不因首选是谁而变"
        );
        assert_eq!(
            r.preedit_fullpinyin, "ni'hao",
            "另行交出全拼切分，供协调器在高亮到全拼候选时取用"
        );
    }

    /// 支路无产出时不给全拼形态——省得协调器凭空多一个可选项。
    #[test]
    fn engine_omits_full_pinyin_preedit_when_branch_silent() {
        let eng = sp_fp_engine("preedit_off", &[("你好", "ni hao", 200)], false);
        let r = eng.convert("nihao", 20).unwrap();
        assert!(
            r.preedit_fullpinyin.is_empty(),
            "开关关闭时不该有全拼形态，实际: {:?}",
            r.preedit_fullpinyin
        );
    }

    /// 整句解码（⑤）：全拼降级流也要能组句，否则「完整的全拼也能工作」只兑现一半——
    /// 词典里没有的搭配全部落空。
    #[test]
    fn full_pinyin_composes_sentence() {
        let eng = sp_fp_engine(
            "sentence",
            &[
                ("我", "wo", 10000),
                ("今天", "jin tian", 5000),
                ("很", "hen", 3000),
                ("开心", "kai xin", 2000),
            ],
            true,
        );
        let r = eng.convert("wojintianhenkaixin", 20).unwrap();
        let c = r
            .candidates
            .iter()
            .find(|c| c.text == "我今天很开心")
            .unwrap_or_else(|| panic!("全拼长串应组出整句，实际候选: {:?}", texts(&r)));
        assert!(c.is_sentence, "应标为整句");
        assert!(c.is_fullpinyin_fallback, "整句同样带支路来源标记");
        assert_eq!(
            c.consumed_length,
            "wojintianhenkaixin".len(),
            "整句消费完整音节段（本例无残码，即整串）"
        );
    }

    /// 整句让位于精确整词：支路自带一份判据，因为主路径 6.5/6.5b 跑在双拼边界校验之前、
    /// 够不着本支路的候选。
    ///
    /// `nihao` 下词典既有精确整词「你好」(w=200)，Viterbi 又能用「你」+「好」拼出「你好」——
    /// 同文会被查重挡掉。故改用一个**词典没有的**组合：`nihen` → 「你很」由两个单字拼出，
    /// 而 `ni hen` 这个码上另有精确词「妮痕」压着，整句须排在它之后。
    #[test]
    fn full_pinyin_sentence_yields_to_exact_word() {
        let eng = sp_fp_engine(
            "yield",
            &[
                ("妮痕", "ni hen", 9000),
                ("你", "ni", 500),
                ("很", "hen", 400),
            ],
            true,
        );
        let r = eng.convert("nihen", 20).unwrap();
        let exact = r.candidates.iter().find(|c| c.text == "妮痕");
        let sentence = r.candidates.iter().find(|c| c.text == "你很");
        if let (Some(e), Some(s)) = (exact, sentence) {
            assert!(
                s.weight < e.weight,
                "整句「你很」({}) 须让位于精确整词「妮痕」({})",
                s.weight,
                e.weight
            );
        } else {
            // 词图未拼出该整句时不强求（Viterbi 行为随词典权重浮动），但精确词必须在。
            assert!(
                exact.is_some(),
                "精确整词「妮痕」必须在，实际: {:?}",
                texts(&r)
            );
        }
    }

    /// 非双拼（纯全拼）方案下开关无效——全拼本就是主路径，支路会把同一批候选查两遍。
    #[test]
    fn full_pinyin_flag_is_noop_without_shuangpin() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("nihao".to_string(), "你好".to_string(), 200, 0);
        let cfg = Config {
            allow_full_pinyin: true,
            ..Config::default()
        };
        let eng = PinyinEngine::new(cfg, CachedDict::Memory(raw));
        let r = eng.convert("nihao", 20).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "你好"),
            "全拼方案照常出「你好」"
        );
        assert!(
            r.candidates.iter().all(|c| !c.is_fullpinyin_fallback),
            "全拼方案下不该有任何候选被标成降级候选"
        );
    }

    /// 边界相容判定：双拼定死音节边界，候选的词典边界须与之吻合。
    #[test]
    fn boundary_compatible_rules() {
        // 输入 nihao(5键) 双拼解释 ni|ha|o → 全拼 "nihao"，边界 {0,2,4}
        let sp = 0b10101u64;
        // 「你好」词典边界 ni|hao = {0,2}，与解释不符 → 拒绝（这正是 5 键出「你好」的病灶）
        assert!(
            !boundary_compatible(0b101, sp, 5, 5),
            "ni|hao 不该匹配 ni|ha|o"
        );
        // 「你」code=ni(2B) 边界 {0} → 只比前 2 字节窗口 → 相容
        assert!(boundary_compatible(0b1, sp, 2, 5));
        // 「你哈」code=niha(4B) 边界 {0,2} → 前 4 字节窗口相容
        assert!(boundary_compatible(0b101, sp, 4, 5));

        // 正确双拼 nihc(4键,小鹤) 解释 ni|hao → 全拼 "nihao"，边界 {0,2}
        let sp2 = 0b101u64;
        assert!(
            boundary_compatible(0b101, sp2, 5, 5),
            "ni|hao 应匹配 ni|hao"
        );

        // 前缀补全：输入 ni（全拼串仅 2B），候选「你好」code=nihao(5B) 边界 {0,2}
        // → 窗口取 min(5,2)=2，只比已输入部分 → 相容（补全部分尚未键入，无从校验）
        assert!(boundary_compatible(0b101, 0b1, 5, 2));

        // 任一侧无信息 → 放行（用户手输码/五笔/模糊变体/含回写段）
        assert!(boundary_compatible(0, sp, 5, 5));
        assert!(boundary_compatible(0b101, 0, 5, 5));
    }

    /// 双拼分段边界：音节、尾部 partial、**以及无匹配键对回写段**，各开一个段起点。
    #[test]
    fn sp_boundary_mask_rules() {
        use crate::pinyin::shuangpin::{SpConvertResult, SylSpan};
        let syl = |p: &str, fs, fe| SylSpan {
            pinyin: p.to_string(),
            raw_start: 0,
            raw_end: 0,
            fp_start: fs,
            fp_end: fe,
        };
        // ni|ha + partial o → full "nihao"，边界 {0,2,4}
        // 注：has_partial 与 partial_initial 须同设——真实 convert 二者恒同时写入，
        // 只设其一是不可能出现的状态（fixture 造假会测出假结论）。
        let sp = SpConvertResult {
            syllables: vec![syl("ni", 0, 2), syl("ha", 2, 4)],
            has_partial: true,
            partial_initial: Some("o".into()),
            full_pinyin: "nihao".into(),
            ..Default::default()
        };
        assert_eq!(sp_boundary_mask(&sp), 0b10101);
        // ni|hao 恰好覆盖，无尾部 → {0,2}
        let sp2 = SpConvertResult {
            syllables: vec![syl("ni", 0, 2), syl("hao", 2, 5)],
            has_partial: false,
            full_pinyin: "nihao".into(),
            ..Default::default()
        };
        assert_eq!(sp_boundary_mask(&sp2), 0b101);
        // 回写段夹在音节**中间**（omni 的 om 占 0..2）：其起点同样是段边界 → {0,2}
        let sp3 = SpConvertResult {
            syllables: vec![syl("ni", 2, 4)],
            full_pinyin: "omni".into(),
            ..Default::default()
        };
        assert_eq!(
            sp_boundary_mask(&sp3),
            0b101,
            "回写段在中间时其起点也是边界"
        );
        // 回写段在**尾部**（nihaoya 的 oy+a 占 4..7）→ {0,2,4}。
        // 位 4 的存在正是关键：它让词典的 ni|hao|ya({0,2,5}) 失配。曾在此弃用为 0
        // （以为回写段"无从表达"），校验被整个关掉，「你好呀」就从 step4 前缀补全漏网。
        let sp4 = SpConvertResult {
            syllables: vec![syl("ni", 0, 2), syl("ha", 2, 4)],
            has_partial: true,
            partial_initial: Some("a".into()),
            full_pinyin: "nihaoya".into(),
            ..Default::default()
        };
        assert_eq!(
            sp_boundary_mask(&sp4),
            0b10101,
            "尾部回写段须标起点，否则校验失效"
        );
        assert!(
            !boundary_compatible(0b100101, 0b10101, 7, 7),
            "词典 ni|hao|ya 应与双拼 ni|ha|oy… 失配"
        );
        // 全是回写段（如 oy）→ 仅首段起点 {0}
        let sp5 = SpConvertResult {
            full_pinyin: "oy".into(),
            ..Default::default()
        };
        assert_eq!(sp_boundary_mask(&sp5), 0b1);
        // 空输入 → 无信息
        assert_eq!(sp_boundary_mask(&SpConvertResult::default()), 0);
    }

    /// 回归：无匹配键对（convert 的「原样回写」分支）既不进 syllables 也不置 has_partial，
    /// build_raw_preedit 必须仍覆盖它们，否则编码被静默吞掉。
    /// 真机现象（首道双拼）：nihaom → 显示 niha（om 消失），再按 a → ni'ha'oma 又复现。
    #[test]
    fn build_raw_preedit_covers_unmatched_pairs() {
        use crate::pinyin::shuangpin::{SpConvertResult, SylSpan};
        let syl = |raw_start, raw_end| SylSpan {
            pinyin: String::new(), // build_raw_preedit 只用 sp 区间切原始串，不读 pinyin
            raw_start,
            raw_end,
            fp_start: 0,
            fp_end: 0,
        };

        // ① 尾部无匹配键对（om）：has_partial=false，早期实现漏掉尾巴 → "ni'ha"。
        let sp = SpConvertResult {
            syllables: vec![syl(0, 2), syl(2, 4)],
            has_partial: false,
            ..Default::default()
        };
        assert_eq!(
            build_raw_preedit("nihaom", &sp),
            "ni'ha'om",
            "尾部无匹配键对不得被吞"
        );

        // ② 尾部 partial 单键（o）：has_partial=true，行为与早期实现一致。
        let sp = SpConvertResult {
            syllables: vec![syl(0, 2), syl(2, 4)],
            has_partial: true,
            ..Default::default()
        };
        assert_eq!(build_raw_preedit("nihao", &sp), "ni'ha'o");

        // ③ 无匹配键对在中间（om 在前）：音节前的空隙也须原样保留。
        let sp = SpConvertResult {
            syllables: vec![syl(2, 4), syl(4, 6)],
            has_partial: true,
            ..Default::default()
        };
        assert_eq!(
            build_raw_preedit("omnihao", &sp),
            "om'ni'ha'o",
            "音节之间的无匹配段不得被吞"
        );

        // ④ 全无音节：原样返回。
        let sp = SpConvertResult::default();
        assert_eq!(build_raw_preedit("xq", &sp), "xq");
        assert_eq!(build_raw_preedit("", &sp), "");
    }

    /// **双拼真值边界校验（本功能的验收点）**：双拼把音节边界定死了，候选的词典边界必须吻合。
    ///
    /// 真机现象：双拼下打 5 键 `nihao` 出「你好」。那是巧合——双拼解释为 `ni|ha|o`，拼成
    /// `full_pinyin="nihao"` 恰好撞上全拼的 nihao，DAG 再把它重切成 `ni|hao` 查到「你好」。
    /// 而「你好」的正确双拼是 4 键（`nihc`）。
    ///
    /// 注意必须用 rime 源构造词典：`merge_single` 造的条目 boundary 恒 0（无信息→放行），
    /// 用它根本测不出校验。
    #[test]
    fn shuangpin_rejects_mismatched_syllable_split() {
        use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
        use std::io::Write;
        let path = std::env::temp_dir().join("wind_sp_boundary_check.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: py\n...").unwrap();
            writeln!(f, "你好\tni hao\t2000").unwrap(); // 边界 ni|hao = {0,2}
            writeln!(f, "你\tni\t900").unwrap();
            writeln!(f, "哈\tha\t500").unwrap();
            writeln!(f, "哦\to\t300").unwrap();
        }
        let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
        let _ = std::fs::remove_file(&path);

        let schema_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
        let eng = PinyinEngine::new(Config::default(), dict)
            .with_shuangpin(ShuangpinConverter::new(layout));

        // 5 键 nihao → 双拼 ni|ha|o（o 为 partial），与「你好」的 ni|hao 不符 → 拒绝。
        let r = eng.convert("nihao", 20).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        assert!(
            !texts.contains(&&"你好".to_string()),
            "5 键 nihao 解释为 ni|ha|o，不该出「你好」（其双拼是 4 键 nihc），实际: {texts:?}"
        );
        // 与解释相容的候选仍在：ni → 「你」
        assert!(
            texts.contains(&&"你".to_string()),
            "「你」(ni) 与 ni|ha|o 的首音节相容，应保留，实际: {texts:?}"
        );

        // 4 键 nihc → 双拼 ni|hao，与「你好」的词典边界一致 → 正常出。
        let r2 = eng.convert("nihc", 20).unwrap();
        assert!(
            r2.candidates.iter().any(|c| c.text == "你好"),
            "4 键 nihc 解释为 ni|hao，应出「你好」，实际: {:?}",
            r2.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    /// 回归（真机报告）：双拼下 `nihao` 选「你」后，剩余 `hao`(3键) 变成空候选。
    ///
    /// 病灶不在校验本身，而在「查询仍按 DAG 的猜测、校验却按双拼的真值」——两套切分打架：
    /// 双拼解释 `hao` = `ha`+`o`(partial)，而 DAG 把 `full="hao"` 重切成 `[hao]` 只查了
    /// 「好」，随后被真值 {0,2} 拒掉；而双拼真正该查的 `ha`（→「哈」）压根没被查。
    /// 于是 DAG 查来的被拒、双拼该查的没查 → 空。
    #[test]
    fn shuangpin_uses_own_split_for_lookup_not_dag() {
        use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
        use std::io::Write;
        let path = std::env::temp_dir().join("wind_sp_lookup_split.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: py\n...").unwrap();
            writeln!(f, "好\thao\t2000").unwrap(); // 单音节，边界 {0}
            writeln!(f, "哈\tha\t900").unwrap();
            writeln!(f, "哦\to\t300").unwrap();
        }
        let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
        let _ = std::fs::remove_file(&path);

        let schema_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
        let eng = PinyinEngine::new(Config::default(), dict)
            .with_shuangpin(ShuangpinConverter::new(layout));

        // 3 键 hao → 双拼 ha|o：应按**双拼自己的切分**去查，出「哈」(ha)。
        let r = eng.convert("hao", 20).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        assert!(
            texts.contains(&&"哈".to_string()),
            "ha 是双拼解释出的完成音节，应查到「哈」，实际: {texts:?}（空候选=查询仍按 DAG 猜）"
        );
        // 「好」的双拼是 2 键 hc，3 键 h/a/o 不该出它。
        assert!(
            !texts.contains(&&"好".to_string()),
            "3 键 hao 解释为 ha|o，不该出「好」（其双拼是 hc），实际: {texts:?}"
        );

        // 2 键 hc → 双拼 hao 单音节 → 正常出「好」。
        let r2 = eng.convert("hc", 20).unwrap();
        assert!(
            r2.candidates.iter().any(|c| c.text == "好"),
            "2 键 hc 解释为 hao，应出「好」，实际: {:?}",
            r2.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    /// 含「无匹配键对」时**仍按双拼语义**：取从 0 起连续的音节前缀，断裂处之后不解释。
    ///
    /// `oy`（o 非声母，拼不出音节）属 convert 注释里的「无效键对」——用户打错了，
    /// 它及其后的内容不该产生候选。曾误把这里当成「整串降级回全拼 DAG」，于是 `nihaoya`
    /// 出了「你好呀」——那与 `nihao`(5键) 不出「你好」自相矛盾：同是双拼下打全拼串，
    /// 一个拒一个收，反倒是**打错一个键对就解锁了全拼**。
    ///
    /// 注释里的「简拼」指的是另一半：`nh` 这种 per-串简拼由 AbbrevMatcher 兜底（走 query，
    /// 不依赖 syllables），无需退回 DAG 也照常工作——见 shuangpin_abbrev_still_works。
    #[test]
    fn shuangpin_keeps_own_semantics_with_unmatched_pair() {
        use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
        use std::io::Write;
        let path = std::env::temp_dir().join("wind_sp_writeback_strict.dict.yaml");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nname: py\n...").unwrap();
            writeln!(f, "你好呀\tni hao ya\t2000").unwrap();
            writeln!(f, "你好\tni hao\t1500").unwrap();
            writeln!(f, "你\tni\t900").unwrap();
            writeln!(f, "哈\tha\t500").unwrap();
        }
        let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
        let _ = std::fs::remove_file(&path);

        let schema_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
        let conv = ShuangpinConverter::new(layout);

        // 前提：nihaoya 在小鹤下确有「无匹配键对」（oy）——即音节未覆盖到 full 末尾、
        // 且缺口大于 partial 声母。否则本测试没意义。
        let sp = conv.convert("nihaoya");
        let covered: usize = sp.syllables.last().map_or(0, |s| s.fp_end);
        let partial_len = sp.partial_initial.as_ref().map_or(0, |s| s.len());
        assert!(
            sp.full_pinyin.len() - covered > partial_len,
            "前提失效：nihaoya 应含无匹配回写段，实际 syllables={:?} full={:?}",
            sp.syllables.iter().map(|s| &s.pinyin).collect::<Vec<_>>(),
            sp.full_pinyin
        );

        let eng = PinyinEngine::new(Config::default(), dict).with_shuangpin(conv);
        let r = eng.convert("nihaoya", 20).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        // 断裂前的 ni|ha 照常出候选。
        assert!(
            texts.contains(&&"你".to_string()),
            "连续前缀 ni 应出「你」，实际: {texts:?}"
        );
        // 不得把整串当全拼——那会与「nihao 不出你好」自相矛盾。
        assert!(
            !texts.contains(&&"你好呀".to_string()) && !texts.contains(&&"你好".to_string()),
            "oy 是无效键对，不该整串降级成全拼解释，实际: {texts:?}"
        );
    }

    /// 无匹配键对**原样回写**进 full_pinyin（不产 SylSpan），输入不被吞——
    /// 这是简拼兜底的前提：AbbrevMatcher 走 `query`（即 full_pinyin），**不看音节切分**，
    /// 故双拼真值切分不影响它。
    ///
    /// 这也是「含回写段须退回 DAG」的反证：保住简拼根本不需要退回 DAG。
    /// （简拼表只存在于 wdat AbbrevSection，端到端查询由 wind-dict 侧覆盖。）
    #[test]
    fn shuangpin_writeback_keeps_input_intact() {
        use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
        let schema_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
        let conv = ShuangpinConverter::new(layout);
        // oy：o 非声母，拼不出合法音节 → 整对原样回写。
        let sp = conv.convert("oy");
        assert!(
            sp.syllables.is_empty(),
            "oy 拼不出音节，不该产出 SylSpan，实际 {:?}",
            sp.syllables.iter().map(|s| &s.pinyin).collect::<Vec<_>>()
        );
        assert_eq!(sp.full_pinyin, "oy", "无匹配键对须原样回写，输入不得被吞");
    }

    /// Fix A TDD：双拼 preedit 应显示用户实际输入的原始按键（按音节边界以 `'` 分隔，
    /// 与全拼自动分词一致），而非转换后的全拼。输入小鹤 "nihc"（→全拼 nihao）应显示
    /// "ni'hc"，候选仍含「你好」。
    #[test]
    fn shuangpin_preedit_shows_raw_keys() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("nihao".to_string(), "你好".to_string(), 200, 0);
        raw.merge_single("ni".to_string(), "你".to_string(), 100, 1);
        let dict = CachedDict::Memory(raw);

        let schema_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
        let conv = ShuangpinConverter::new(layout);
        let eng = PinyinEngine::new(Config::default(), dict).with_shuangpin(conv);

        // 完整双音节：preedit 为原始键按音节 ' 分隔 "ni'hc"（而非全拼 "ni'hao"）
        let r = eng.convert("nihc", 10).unwrap();
        assert_eq!(
            r.preedit_display, "ni'hc",
            "双拼 preedit 应显示原始按键并按音节 ' 分隔，实际: {:?}",
            r.preedit_display
        );
        // 候选仍走全拼语义，含「你好」
        assert!(
            r.candidates.iter().any(|c| c.text == "你好"),
            "候选仍应含「你好」，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );

        // 单音节：ni → "ni"
        let r2 = eng.convert("ni", 10).unwrap();
        assert_eq!(r2.preedit_display, "ni", "单音节 preedit 应为 \"ni\"");

        // 含 partial：nih（ni 完成 + h 未配对）→ "ni'h"
        let r3 = eng.convert("nih", 10).unwrap();
        assert_eq!(
            r3.preedit_display, "ni'h",
            "含 partial 的双拼 preedit 应为 \"ni'h\"，实际: {:?}",
            r3.preedit_display
        );
    }

    /// Fix B TDD：fuzzy 应接入精确/单音节查询。词典含 "shi"→"是"，
    /// fuzzy sh_s=true 时，输入单音节全拼 "si" 应能命中「是」（sh↔s 模糊）。
    #[test]
    fn fuzzy_lookup_single_syllable() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("shi".to_string(), "是".to_string(), 100, 0);
        let dict = CachedDict::Memory(raw);
        let fz = FuzzyConfig {
            sh_s: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), dict).with_fuzzy(fz);

        let r = eng.convert("si", 10).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "是"),
            "fuzzy sh_s 开启时，单音节 \"si\" 应命中「是」，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );

        // 反向：词典 "si"→"四"，输入 "shi" 应命中「四」
        let mut raw2 = CodetableDict::empty();
        raw2.merge_single("si".to_string(), "四".to_string(), 100, 0);
        let fz2 = FuzzyConfig {
            sh_s: true,
            ..Default::default()
        };
        let eng2 = PinyinEngine::new(Config::default(), CachedDict::Memory(raw2)).with_fuzzy(fz2);
        let r2 = eng2.convert("shi", 10).unwrap();
        assert!(
            r2.candidates.iter().any(|c| c.text == "四"),
            "fuzzy sh_s 开启时，\"shi\" 应命中「四」，实际: {:?}",
            r2.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    /// 模糊命中受 `×0.5` 折扣，但**不是硬闸门**：词频差距足够大时模糊仍可胜出。
    ///
    /// ## ⚠️ 断言方向已随「对齐 librime/fcitx5」反转
    ///
    /// 原语义是「精确必须排在模糊之前，即便模糊词词频更高」，靠 `FUZZY_WEIGHT_SCALE=0.01`
    /// 实现——那个取值是为「让精确守住首选位」标定的（汉语同音字词频常差 1~2 个量级，
    /// 折扣必须 < 1/80 才压得住），**不是参考实现的做法**。
    ///
    /// 两个参考实现都用 `log(0.5)`：librime `kFuzzySpellingPenalty`、libime `fuzzyCost`。
    /// 折扣只表达「模糊召回的置信度低一档」，压不压得住由词频差距自己决定 ——
    /// 「是」(1799848) 打 `si` 时排在「四」(22625) 前面，正是这两个实现的行为。
    ///
    /// 本用例改为守**折扣本身存在且按音节累乘**，那才是与实现无关的不变量。
    #[test]
    fn fuzzy_hit_is_discounted_but_not_gated() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("si".to_string(), "四".to_string(), 100, 0);
        raw.merge_single("shi".to_string(), "是".to_string(), 9000, 0);
        let dict = CachedDict::Memory(raw);
        let fz = FuzzyConfig {
            sh_s: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), dict).with_fuzzy(fz);

        let r = eng.convert("si", 10).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        let si = r.candidates.iter().find(|c| c.text == "四");
        let shi = r.candidates.iter().find(|c| c.text == "是");
        let si = si.unwrap_or_else(|| panic!("精确候选「四」应存在，实际: {texts:?}"));
        let shi = shi.unwrap_or_else(|| panic!("模糊候选「是」应存在，实际: {texts:?}"));

        assert!(!si.is_fuzzy, "「四」应为非模糊");
        assert!(shi.is_fuzzy, "「是」应为模糊命中");
        // 折扣确实施加了：9000 → 4500（单音节 ⇒ 0.5^1）。
        assert_eq!(
            shi.weight, 4500,
            "模糊命中须按 0.5^1 折扣，实际 {}",
            shi.weight
        );
        assert_eq!(si.weight, 100, "精确命中权重不受影响");
        // 90 倍的词频差距压过一档折扣 —— 折扣不是硬闸门。
        assert_eq!(r.candidates[0].text, "是", "实际: {texts:?}");
    }

    /// 配对：**词频相当时折扣说了算**，精确胜出。
    ///
    /// 与上一条合看才说明 0.5 是个真折扣：少了这条，「模糊完全不打折」也能让上一条通过。
    #[test]
    fn fuzzy_loses_to_exact_at_equal_weight() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("si".to_string(), "四".to_string(), 9000, 0);
        raw.merge_single("shi".to_string(), "是".to_string(), 9000, 0);
        let fz = FuzzyConfig {
            sh_s: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

        let r = eng.convert("si", 10).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        assert_eq!(
            r.candidates[0].text, "四",
            "同权重下模糊被折扣压住，精确胜出，实际: {texts:?}"
        );
    }

    /// 层级 TDD：精确词应优先于高频前缀补全词。词典 "sikao"→"思考"(weight 100,精确) 与
    /// "sikaozhe"→"思考者"(weight 9000,补全词，code 比输入长)。输入 "sikao" 时「思考」
    /// (精确,code==输入) 必须排在「思考者」(前缀补全)之前——即便后者词频高得多。
    /// 对齐 Go Exact>>Partial。
    ///
    /// ⚠️ **本例刻意用 2 音节输入**。原版用 `si` → 期望「思考」作为前缀补全出现，
    /// 该场景已被 6.3 音节数闸门（[`STRICT_SYLLABLE_MATCH_MAX`]）关掉：单音节输入下
    /// 前缀补全整体不产出，`si` 不再出「思考」，正如 `dian` 不再出「电话」。
    /// 要验证的层级序本身没变，只能移到不进严格档的输入长度上（used=2）。
    /// 单音节侧的新语义由 `single_syllable_drops_longer_completions` 守卫。
    #[test]
    fn exact_ranks_above_prefix_completion() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("sikao".to_string(), "思考".to_string(), 100, 0);
        raw.merge_single("sikaozhe".to_string(), "思考者".to_string(), 9000, 0);
        let dict = CachedDict::Memory(raw);
        // 门槛设回 2：`sikao` 只有 2 音节，出厂的 4 会让「思考者」在召回层就没了。
        let eng = PinyinEngine::new(relaxed_completion_config(), dict);

        let r = eng.convert("sikao", 10).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        let pos_kao = texts.iter().position(|t| *t == "思考");
        let pos_zhe = texts.iter().position(|t| *t == "思考者");
        assert!(pos_kao.is_some(), "精确「思考」应存在，实际: {texts:?}");
        assert!(pos_zhe.is_some(), "补全「思考者」应存在，实际: {texts:?}");
        assert!(
            pos_kao < pos_zhe,
            "精确「思考」应优先于高频前缀补全「思考者」，实际: {texts:?}"
        );
        assert!(
            !r.candidates[pos_kao.unwrap()].is_prefix,
            "「思考」应为精确(非前缀)"
        );
        assert!(
            r.candidates[pos_zhe.unwrap()].is_prefix,
            "「思考者」应为前缀补全"
        );
    }

    /// 6.3 音节数闸门：**输入只表达 1 个音节时，音节数更多的前缀补全一律不产出**。
    ///
    /// 这是上面那条测试原场景的新语义版本，也是本闸门的核心断言：`si` 只出单字「四」，
    /// 不出 2 音节的「思考」——对齐主流拼音输入法（`dian` 不出「电话」）。
    ///
    /// ⚠️ `merge_single` 造的词典 **boundary 恒为 0**（P2b 记录在案），故本例同时覆盖了
    /// `effective_boundary` 的 DAG 兜底路径：「思考」的 2 音节是现切 `si|kao` 得来的。
    #[test]
    fn single_syllable_drops_longer_completions() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("si".to_string(), "四".to_string(), 100, 0);
        raw.merge_single("sikao".to_string(), "思考".to_string(), 9000, 0);
        let dict = CachedDict::Memory(raw);
        let eng = PinyinEngine::new(Config::default(), dict);

        let texts: Vec<String> = eng
            .convert("si", 10)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert!(
            texts.iter().any(|t| t == "四"),
            "同音节数的精确候选必须保留，实际: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t == "思考"),
            "单音节输入下 2 音节补全词不该出现，实际: {texts:?}"
        );
    }

    /// **反向对照**：多打一个字母（`sik`，残码使 used=2）后，同一个补全词必须回来。
    ///
    /// 缺了这条，上面那个断言可以靠「把前缀补全整个废掉」来满足 —— 那是另一个更严重的
    /// 回归（`meiy`→「没有」、`nih`→「你好」全灭）。
    #[test]
    fn trailing_partial_restores_completions() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("si".to_string(), "四".to_string(), 100, 0);
        raw.merge_single("sikao".to_string(), "思考".to_string(), 9000, 0);
        let dict = CachedDict::Memory(raw);
        let eng = PinyinEngine::new(Config::default(), dict);

        let texts: Vec<String> = eng
            .convert("sik", 10)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert!(
            texts.iter().any(|t| t == "思考"),
            "残码使输入占到 2 个音节，补全词必须回来，实际: {texts:?}"
        );
    }

    /// 完整匹配优先于子短语（对齐 Go coverage 分层）：输入完整音节 "nihao" 时，
    /// 全长精确词「拟好」(code==nihao) 即便权重远低于子短语「你」(code=ni)，也应排在「你」之前。
    /// 「你」只覆盖部分输入(ni)，是分段上屏候选(is_partial)，整体降到完整词之后。
    ///
    /// 注：此前 `subphrase_not_demoted_below_rare_exact` 断言相反（子词组不降权 → 你 > 拟好），
    /// 那是刻意偏离 Go 的旧设计，正是 baoan→报案 被高频单字埋没的根因。现改为对齐 Go：
    /// `score = exp(词频) + initialQuality + coverage`，完整词(cov=1,iq=4) 恒先于子短语单字(cov=.5,iq=2.5)。
    #[test]
    fn full_word_ranks_above_subphrase_singlechar() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("nihao".to_string(), "你好".to_string(), 200, 0);
        raw.merge_single("nihao".to_string(), "拟好".to_string(), 10, 1); // 罕见全长精确词
        raw.merge_single("ni".to_string(), "你".to_string(), 5000, 0); // 常用子短语
        let dict = CachedDict::Memory(raw);
        let eng = PinyinEngine::new(Config::default(), dict);

        let r = eng.convert("nihao", 10).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        let pos_ni = texts.iter().position(|t| *t == "你");
        let pos_nihao_rare = texts.iter().position(|t| *t == "拟好");
        assert!(
            pos_ni.is_some() && pos_nihao_rare.is_some(),
            "候选缺失（子短语「你」仍应存在，供分段上屏）: {texts:?}"
        );
        assert!(
            pos_nihao_rare < pos_ni,
            "完整词「拟好」应优先于子短语单字「你」(对齐 Go coverage 分层)，实际: {texts:?}"
        );
        // 「你」是子短语(is_partial)，不是前缀补全(is_prefix)——分段上屏机制不受影响
        let ni = &r.candidates[pos_ni.unwrap()];
        assert!(!ni.is_prefix, "子短语「你」不应是前缀补全");
        assert!(ni.is_partial, "子短语「你」应标记 is_partial");
    }

    /// baoan 回归（用户报告场景）：输入 "baoan" 时，完整 bao'an 词「保安」「报案」必须
    /// 聚集在前，不被高频子短语单字「报」(bao) 插开。修复前「报」(高权重) 会塞进
    /// 「保安」「报案」之间，把「报案」挤到后面几页。
    #[test]
    fn baoan_full_words_group_above_subphrase() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("baoan".to_string(), "保安".to_string(), 3513, 0);
        raw.merge_single("baoan".to_string(), "报案".to_string(), 1374, 1);
        raw.merge_single("bao".to_string(), "报".to_string(), 9000, 0); // 高频单字，权重高于「报案」
        raw.merge_single("an".to_string(), "安".to_string(), 5000, 0);
        let dict = CachedDict::Memory(raw);
        let eng = PinyinEngine::new(Config::default(), dict);

        let r = eng.convert("baoan", 20).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        let pos = |t: &str| texts.iter().position(|x| *x == t);
        let p_baoan = pos("保安").expect("「保安」应存在");
        let p_baoan2 = pos("报案").expect("「报案」应存在");
        let p_bao = pos("报").expect("子短语「报」应存在（供分段上屏）");
        assert!(
            p_baoan < p_bao && p_baoan2 < p_bao,
            "完整词「保安」({p_baoan})「报案」({p_baoan2}) 都应排在高频子短语单字「报」({p_bao}) 之前，实际: {texts:?}"
        );
    }

    /// Fix B TDD：fuzzy 应接入多音节整串查询（expand_code 笛卡尔积）。
    /// 词典只存 eng 形式 "shengri"→"生日"，用户输入 en 形式 "shenri"（DAG 切分 shen+ri），
    /// fuzzy en_eng=true 时应通过 expand_code 生成 "shengri" 反查命中「生日」。
    #[test]
    fn fuzzy_lookup_multi_syllable() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("shengri".to_string(), "生日".to_string(), 100, 0);
        let dict = CachedDict::Memory(raw);
        let fz = FuzzyConfig {
            en_eng: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), dict).with_fuzzy(fz);

        let r = eng.convert("shenri", 10).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "生日"),
            "fuzzy en_eng 开启时，\"shenri\" 应模糊命中「生日」(shengri)，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    /// Fix B TDD：fuzzy 全 false 时 lookup_with_fuzzy 退化为纯精确查找（不引入多余候选）。
    #[test]
    fn fuzzy_disabled_no_extra_candidates() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("shi".to_string(), "是".to_string(), 100, 0);
        let dict = CachedDict::Memory(raw);
        // 无 with_fuzzy → FuzzyConfig::default() 全 false
        let eng = PinyinEngine::new(Config::default(), dict);
        let r = eng.convert("si", 10).unwrap();
        assert!(
            !r.candidates.iter().any(|c| c.text == "是"),
            "fuzzy 关闭时 \"si\" 不应命中「是」，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    /// 模糊命中的权重折扣：同一输入下，精确命中恒优先于**同词频**的模糊命中。
    #[test]
    fn fuzzy_penalty_keeps_exact_ahead_at_equal_weight() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("si".to_string(), "四".to_string(), 1000, 0);
        raw.merge_single("shi".to_string(), "是".to_string(), 1000, 1);
        let fz = FuzzyConfig {
            sh_s: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

        let r = eng.convert("si", 10).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        let pos_exact = texts.iter().position(|t| *t == "四").expect("「四」应存在");
        let pos_fuzzy = texts.iter().position(|t| *t == "是").expect("「是」应存在");
        assert!(
            pos_exact < pos_fuzzy,
            "同词频时精确命中须在模糊命中之前（折扣生效），实际: {texts:?}"
        );
    }

    /// **本次修复的回归守卫（原 bug 的直接复现）**：模糊命中不得被大量**前缀补全**挤出候选。
    ///
    /// 原实现把 `is_fuzzy` 当 `cmp_match_layers` 的首要键，所有非模糊候选（含码更长的前缀
    /// 补全）无条件排在模糊命中之前。真实词库下 `si` 的前缀补全有 230 条，把「是」顶到第
    /// 231 位，而生产候选上限仅 50~300 —— 模糊音整体失效。
    ///
    /// 此处用 40 条 `si*` 前缀补全模拟那堵墙：**上限取 20**（小于补全总数），若模糊命中仍
    /// 被整层压在补全之后，它必然落在截断线外。这正是「迷你词典单测全绿、真机全废」的
    /// 那道缺口——测试数据的**规模**本身就是被测条件的一部分。
    #[test]
    fn fuzzy_hit_survives_a_wall_of_prefix_completions() {
        let mut raw = CodetableDict::empty();
        // 一堵前缀补全的墙：码比输入长（is_prefix=true），非模糊，权重普通。
        for i in 0..40 {
            raw.merge_single(format!("si{i:02}"), format!("思{i:02}"), 500, i);
        }
        // 模糊命中：码 shi，经 s↔sh 由输入 si 召回；词频显著高于补全（真实词库中
        // 「是」正是高频字），折扣后仍应有竞争力。
        raw.merge_single("shi".to_string(), "是".to_string(), 900_000, 99);
        let fz = FuzzyConfig {
            sh_s: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

        const LIMIT: usize = 20;
        let r = eng.convert("si", LIMIT).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        assert!(
            texts.iter().any(|t| *t == "是"),
            "模糊命中须能挤进前 {LIMIT} 条，不得被 40 条前缀补全整层压到截断线外，实际: {texts:?}"
        );
    }

    /// 多音节整词的模糊命中（**step1 `lookup_with_fuzzy` 路径**，一直走逐音节 `expand_code`）：
    /// `beijinsi` → 「北京市」(beijingshi) 需要第 2 音节 in→ing **且** 第 3 音节 s→sh。
    ///
    /// 注意本例**测不到 lattice 路径**：词典存有覆盖整串的词条，step1 直接命中。
    /// lattice 的逐音节展开由 `fuzzy_non_initial_initial_via_lattice_sentence` 覆盖。
    #[test]
    fn fuzzy_hits_non_initial_syllables_via_lookup() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("beijingshi".to_string(), "北京市".to_string(), 5000, 0);
        let fz = FuzzyConfig {
            in_ing: true,
            sh_s: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

        let r = eng.convert("beijinsi", 20).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "北京市"),
            "第 2、3 音节同时模糊时应命中「北京市」，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    /// **lattice 逐音节展开的专项回归守卫**（本次修复的核心路径）。
    ///
    /// 设计要点，缺一条就测不到真东西：
    /// - 用**非首音节的声母**变体（`zou`→`zhou`，第 2 音节）。声母规则是 `starts_with`，
    ///   整串调用只能改首音节；而韵母规则是 `find`，第一处匹配常恰好落在非首音节上，
    ///   用韵母做判据会让整串调用也「碰巧」通过（`beijin`→`beijing` 正是如此）。
    /// - 词典**不含**覆盖整串的词条，迫使候选只能由 Viterbi 多节点拼接产生，
    ///   从而必经 lattice；否则 step1 的 `lookup_with_fuzzy` 会先命中，测不到 lattice。
    ///
    /// 把 lattice 改回对整串 `code` 求变体，本测试即挂。
    #[test]
    fn fuzzy_non_initial_initial_via_lattice_sentence() {
        let mut raw = CodetableDict::empty();
        // 覆盖前两音节的词（其码 zhongzhou 需由 zhong|zou 经第 2 音节 z→zh 得到）
        raw.merge_single("zhongzhou".to_string(), "中州".to_string(), 5000, 0);
        // 覆盖末音节的字，供 Viterbi 拼出整句
        raw.merge_single("ming".to_string(), "明".to_string(), 5000, 1);
        let fz = FuzzyConfig {
            zh_z: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

        // zhong|zou|ming：词典无覆盖整串的词条 → 只能靠词图拼接
        let r = eng.convert("zhongzouming", 20).unwrap();
        // **必须断言整句**（`is_sentence`），不能只断言「中州」出现：后者由 step3 的子短语
        // 查询命中（`partial=true, code=zhongzou`，同样走逐音节 `lookup_with_fuzzy`），
        // 在旧实现下**照样存在**——拿它做判据测不到 lattice，是一条会永远通过的假测试。
        // 只有整句「中州明」需要「中州」先作为**词图节点**存在，才必经 lattice 的模糊展开。
        let dump: Vec<String> = r
            .candidates
            .iter()
            .map(|c| format!("{}(sent={})", c.text, c.is_sentence))
            .collect();
        assert!(
            r.candidates
                .iter()
                .any(|c| c.is_sentence && c.text.contains("中州")),
            "第 2 音节 zou→zhou 须能进入词图，使 Viterbi 拼出整句「中州明」，实际: {dump:?}"
        );
    }

    /// 模糊命中的**整句**让位于精确整词：整句带 3e7 基准分，比例折扣压不下来，
    /// 故走 `is_sentence_demoted` 降级。
    #[test]
    fn fuzzy_sentence_yields_to_exact_word() {
        let mut raw = CodetableDict::empty();
        // 精确整词（码 == 输入）
        raw.merge_single("sixiang".to_string(), "思想".to_string(), 26_000, 0);
        // 模糊命中的整词（码 shixiang，经 s↔sh 由 sixiang 召回）
        raw.merge_single("shixiang".to_string(), "是想".to_string(), 30_000, 1);
        let fz = FuzzyConfig {
            sh_s: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

        let r = eng.convert("sixiang", 10).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        let pos_exact = texts.iter().position(|t| *t == "思想");
        assert_eq!(
            pos_exact,
            Some(0),
            "存在精确整词时它必须居首，模糊整句让位，实际: {texts:?}"
        );
    }

    /// 反面：**没有**精确整词竞争时，模糊命中的整句照常居首——这正是模糊音要的效果
    /// （`zongguo` → 「中国」）。与上一条共用同一判据，二者必须同时成立。
    #[test]
    fn fuzzy_sentence_leads_when_no_exact_word() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("zhongguo".to_string(), "中国".to_string(), 30_000, 0);
        // zongguo 下只有子短语单字，没有码 == zongguo 的精确整词
        raw.merge_single("zong".to_string(), "总".to_string(), 20_000, 1);
        let fz = FuzzyConfig {
            zh_z: true,
            ..Default::default()
        };
        let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

        let r = eng.convert("zongguo", 10).unwrap();
        let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
        assert_eq!(
            texts.first().map(|s| s.as_str()),
            Some("中国"),
            "无精确整词竞争时模糊整句应居首，实际: {texts:?}"
        );
    }

    /// Bug 复现：双拼下用户词（存储在 "pinyin" 共享 schema）应出现在候选中。
    /// 小鹤双拼 "dabologe" → 全拼 "daboluoge"；store 中有该用户词时应能命中。
    #[test]
    fn shuangpin_store_user_word_appears_in_candidates() {
        let store = tmp_store("sp_userdict");
        store
            .add_user_word("pinyin", "daboluoge", "大菠萝哥", 0, 0)
            .unwrap();

        let dm = DictManager::new();
        dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
            store.clone(),
            "pinyin",
        )));

        let schema_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
        let conv = ShuangpinConverter::new(layout);

        // 先确认转换正确：dabologe → daboluoge
        let sp_result = conv.convert("dabologe");
        assert_eq!(
            sp_result.full_pinyin(),
            "daboluoge",
            "小鹤双拼 dabologe 应转换为全拼 daboluoge，实际: {:?}",
            sp_result.full_pinyin()
        );

        let eng = empty_engine()
            .with_shuangpin(conv)
            .with_store_layers(Arc::new(dm));

        let r = eng.convert("dabologe", 20).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "大菠萝哥"),
            "双拼输入 \"dabologe\" 应命中用户词「大菠萝哥」，实际候选: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    // ── use_smart_compose 开关 ──────────────────────────────────────────────────

    /// 构造带单字词典的拼音引擎（供整句/Viterbi 相关测试使用）：
    /// 词典含 "ni"→"你"、"hao"→"好"，但无 "nihao"→"你好" 整词。
    /// 任何 "你好" 候选只能来自 Viterbi 整句路径。
    /// ⚠️ 权重取 `cn_dicts` 的真实值（你 492791 / 好 124546），不要随手编：节点打分是
    /// `ln(weight / DICT_TOTAL)`，各类闸门与惩罚常数都在真实权重分布上标定。原值 100 会让
    /// 每个节点落到 -14.7，整句被质量闸门挡下 —— 测出的是另一个世界的行为。
    /// （2026-08-08 之前 `score_node` 对无 unigram 的引擎走 `weight/100_000` 线性回退，
    /// 与真实路径不在同一数轴，编造值因此「碰巧」可用。）
    fn engine_for_sentence_tests(config: Config) -> PinyinEngine {
        let mut raw = CodetableDict::empty();
        raw.merge_single("ni".to_string(), "你".to_string(), 492_791, 0);
        raw.merge_single("hao".to_string(), "好".to_string(), 124_546, 1);
        PinyinEngine::new(config, CachedDict::Memory(raw))
    }

    /// 判断候选是否为 Viterbi 合成整句（按来源标记，不看权重数值）。
    fn is_viterbi_sentence(c: &Candidate) -> bool {
        c.is_sentence
    }

    // ── 词典整词被 Viterbi 选中时的同文合并 ─────────────────────────────────────

    /// 词典整词恰好被 Viterbi 选为最优解时，经 step 2 的**同文合并**分支继承整句身份，
    /// 且 weight 取 `max(词典 weight, W_eff)`。
    ///
    /// 构造要点：**给整词高权重、单字低权重**，迫使 Viterbi 选「你好」这个单节点，从而
    /// 走同文合并分支（与 `demoted_sentence_falls_below_all_max_weight_exact_words` 的
    /// 构造正好相反 —— 那里要的是多节点合成整句）。
    ///
    /// 这个结构曾是 step 6.6 `is_sentence_contested` 的现场（`siyuan` 寺院/思源：整句
    /// 锚定是硬闸门，同码词灌到 count=5000 都翻不动）。锚定与该字段均已移除，本用例
    /// 保留的是同文合并本身的三条事实。
    #[test]
    fn dict_word_selected_by_viterbi_inherits_sentence_identity() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("ni".to_string(), "你".to_string(), 100, 0);
        raw.merge_single("hao".to_string(), "好".to_string(), 100, 1);
        raw.merge_single("nihao".to_string(), "你好".to_string(), 50_000, 2);
        raw.merge_single("nihao".to_string(), "拟好".to_string(), 200, 3);
        let e = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));
        let r = e.convert("nihao", 50).unwrap();

        let c = r
            .candidates
            .iter()
            .find(|c| c.text == "你好")
            .expect("候选中应有「你好」");
        assert!(c.is_sentence, "词典整词被 Viterbi 选中 → 继承整句身份");
        assert!(
            !c.is_sentence_demoted,
            "它本身即精确整词，无处可让，不该走 6.5 降级"
        );
        // ★ 同文合并**不得**置 `is_synthesized`：这条候选是词库里实打实的词条，
        // 只是恰好被 Viterbi 选中而继承了整句身份。协调器的自动造词以该字段为准
        // ——若在这里跟着置真，打 `nihao` 选「你好」就会把系统词一条条抄进临时词库
        //（每次上屏一次 redb 写，还会被 promote_count 批量「晋升」进用户词库）。
        // 这正是 `is_sentence` 不能直接用作造词判据的原因，见该字段文档。
        assert!(
            !c.is_synthesized,
            "词库已有此词条 ⇒ 不是引擎新合成，不得置 is_synthesized"
        );
        // 同文合并取 `max(词典 weight, W_eff)`，50_000 是构造里「你好」的词典值。
        assert!(
            c.weight >= 50_000,
            "同文合并不得压低词典权重，实际 {}",
            c.weight
        );
        assert_eq!(r.candidates[0].text, "你好", "无词频记录时整句居首");
    }

    /// 对照组：同码**没有**别的精确整词时，整句身份照样成立。
    ///
    /// 与上一条合看，说明「继承整句身份」取决于 Viterbi 选没选中它，与同码有无竞争者无关。
    #[test]
    fn dict_word_sentence_without_peer_still_is_sentence() {
        let mut raw = CodetableDict::empty();
        raw.merge_single("ni".to_string(), "你".to_string(), 100, 0);
        raw.merge_single("hao".to_string(), "好".to_string(), 100, 1);
        raw.merge_single("nihao".to_string(), "你好".to_string(), 50_000, 2);
        let e = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));
        let r = e.convert("nihao", 50).unwrap();

        let c = r
            .candidates
            .iter()
            .find(|c| c.text == "你好")
            .expect("候选中应有「你好」");
        assert!(c.is_sentence, "仍是整句");
        assert!(c.weight >= 50_000, "同文合并同样取 max，实际 {}", c.weight);
    }

    // ── 整句让位于精确整词（step 6.5 降级）─────────────────────────────────────

    /// 用户要求的那条保证：**多个精确整词并列于最大权重时，整句排在它们全部之后**。
    ///
    /// `max - 1` 在算术上蕴含它（并列者皆为 `max`），但这是要靠实测确认的那一类断言，
    /// 不是靠推理就算数的 —— 并列走的是 `better`/`candidate_display_order` 的后续键
    /// （base_order / natural_order），只有真跑一遍才知道整句没混进并列组里。
    #[test]
    fn demoted_sentence_falls_below_all_max_weight_exact_words() {
        let mut raw = CodetableDict::empty();
        // 单字给高权重，确保 Viterbi 选「你+好」而非把某个 nihao 词条当单节点整句
        // （那样会走同文合并分支，压根不触发降级，测试就测空了）。
        // ⚠️ 权重取真实量级，理由同 `demoted_sentence_still_precedes_ordinary_candidates`：
        // 整句是概率**连乘**，编造的 (1e5, 1e5) vs 同码词 5000 会让 Viterbi 直接选同码词、
        // 整句根本不产生。
        raw.merge_single("ni".to_string(), "你".to_string(), 492_791, 0);
        raw.merge_single("hao".to_string(), "好".to_string(), 124_546, 1);
        // 三个精确整词，权重并列且同为最大
        raw.merge_single("nihao".to_string(), "拟好".to_string(), 64, 2);
        raw.merge_single("nihao".to_string(), "泥好".to_string(), 64, 3);
        raw.merge_single("nihao".to_string(), "尼好".to_string(), 64, 4);
        let e = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));
        let r = e.convert("nihao", 50).unwrap();

        let pos = |t: &str| {
            r.candidates
                .iter()
                .position(|c| c.text == t)
                .unwrap_or_else(|| {
                    panic!(
                        "候选中找不到 {t}，实际: {:?}",
                        r.candidates
                            .iter()
                            .map(|c| (&c.text, c.weight))
                            .collect::<Vec<_>>()
                    )
                })
        };
        let sent = pos("你好");
        let sc = &r.candidates[sent];
        assert!(sc.is_sentence, "「你好」应是合成整句");
        assert!(sc.is_sentence_demoted, "存在精确整词时整句须降级");
        // ★ 正向对照（与 `dict_word_selected_by_viterbi_inherits_sentence_identity` 里那条
        // 反向断言配对）：这里的「你好」是「你」+「好」两个节点**拼**出来的，词库里没有
        // `nihao → 你好` 这个词条，故必须置 `is_synthesized` —— 协调器的自动造词只认它。
        // 少了这条对照，哪怕引擎再也不置该字段（功能整个失效），反向断言照样全绿。
        assert!(
            sc.is_synthesized,
            "多节点合成、词库无此词条 ⇒ 必须置 is_synthesized，否则自动造词全线失效"
        );
        // 64 = 上面三个同码精确整词的权重（取自 cn_dicts 的「拟好」真实值）。
        // 写成 `64 - 1` 而非硬编码 63：本断言要守的是「降到 max(同码词) - 1」这个关系，
        // 跟着构造走；写死数值会在下次调整夹具权重时变成一个无从追溯的魔数。
        assert_eq!(sc.weight, 64 - 1, "权重须为 max(同码精确整词) - 1");
        for w in ["拟好", "泥好", "尼好"] {
            assert!(
                pos(w) < sent,
                "并列于 max 的精确整词「{w}」(rank {}) 须排在整句(rank {sent})之前，实际: {:?}",
                pos(w),
                r.candidates
                    .iter()
                    .map(|c| (&c.text, c.weight))
                    .collect::<Vec<_>>()
            );
        }
    }

    /// 不变量：降级整句仍须在**普通候选**之前，无论后者权重多高。
    ///
    /// 守的是 `max - 1` 的权重并列风险 —— 位置靠 `cmp_match_layers` 的层级键保证，
    /// 而非靠权重数值，故权重再离谱也不该翻转。
    #[test]
    fn demoted_sentence_still_precedes_ordinary_candidates() {
        let mut raw = CodetableDict::empty();
        // ⚠️ 单字与「拟好」取 cn_dicts 真实权重。节点打分是 `ln(w / DICT_TOTAL)`，整句为各词
        // **概率连乘**，故两字拼句天然弱于同码整词：编造的 (你 1e5, 好 1e5, 拟好 5000) 下
        // P(拟好)=2.07e-5 远高于 P(你)·P(好)=1.7e-7，Viterbi 会直接选「拟好」而不产生
        // 「你好」整句，本用例的前提就没了。真实权重下 P(你)·P(好)=1.05e-6 > P(拟好)=2.6e-7，
        // 整句成立。（2026-08-08 前无 unigram 的引擎走 `weight/100_000` 线性相加，没有连乘
        // 衰减、系统性偏好多词切分，编造值因此「碰巧」可用。）
        raw.merge_single("ni".to_string(), "你".to_string(), 492_791, 0);
        raw.merge_single("hao".to_string(), "好".to_string(), 124_546, 1);
        raw.merge_single("nihao".to_string(), "拟好".to_string(), 64, 2);
        // 前缀补全（码比输入长）：权重顶到 2e9，仍应留在整句之后
        raw.merge_single(
            "nihaoma".to_string(),
            "你好吗".to_string(),
            2_000_000_000,
            3,
        );
        let e = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));
        let r = e.convert("nihao", 50).unwrap();

        let sent = r.candidates.iter().position(|c| c.text == "你好").unwrap();
        assert!(r.candidates[sent].is_sentence_demoted, "整句须已降级");
        // 整句之前只允许出现精确整词（码 == 输入且不在下层）
        for (i, c) in r.candidates.iter().enumerate().take(sent) {
            assert!(
                c.code == "nihao" && !c.is_fuzzy && !c.is_prefix && !c.is_partial,
                "整句(rank {sent})之前只应有精确整词，却出现 {}(rank {i}, w={}, code={})",
                c.text,
                c.weight,
                c.code
            );
        }
    }

    /// TDD：use_smart_compose=false 时多音节输入不产生 Viterbi 合成整句候选。
    #[test]
    fn smart_compose_off_skips_viterbi_sentence() {
        let e = engine_for_sentence_tests(Config {
            use_smart_compose: false,
            ..Config::default()
        });
        let r = e.convert("nihao", 50).unwrap();
        assert!(
            !r.candidates.iter().any(|c| {
                c.text.chars().count() >= 2
                    && c.source == CandidateSource::Pinyin
                    && is_viterbi_sentence(c)
            }),
            "关闭智能组句后不应有 Viterbi 合成整句，实际候选: {:?}",
            r.candidates
                .iter()
                .map(|c| (&c.text, c.weight))
                .collect::<Vec<_>>()
        );
    }

    /// 回归：use_smart_compose=true（默认）时整句候选仍产生。
    #[test]
    fn smart_compose_on_produces_viterbi_sentence() {
        let e = engine_for_sentence_tests(Config::default()); // use_smart_compose 默认 true
        let r = e.convert("nihao", 50).unwrap();
        assert!(
            r.candidates.iter().any(|c| {
                c.text.chars().count() >= 2
                    && c.source == CandidateSource::Pinyin
                    && is_viterbi_sentence(c)
            }),
            "启用智能组句时应有 Viterbi 合成整句，实际候选: {:?}",
            r.candidates
                .iter()
                .map(|c| (&c.text, c.weight))
                .collect::<Vec<_>>()
        );
    }

    /// Task 4.1 TDD Step 1：双拼端到端——装配小鹤双拼 converter 后，
    /// 输入双拼键 "ni" 应返回含「你」的候选。
    #[test]
    fn pinyin_engine_shuangpin_input() {
        // 构造含 "ni"->"你" 的最小词典
        let mut raw = CodetableDict::empty();
        raw.merge_single("ni".to_string(), "你".to_string(), 100, 0);
        let dict = CachedDict::Memory(raw);

        // 小鹤双拼：ni → ni（声母 n + 韵母 i=i，即全拼 "ni"，保持不变）
        let schema_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
        let conv = ShuangpinConverter::new(layout);

        let eng = PinyinEngine::new(Config::default(), dict).with_shuangpin(conv);
        let r = eng.convert("ni", 10).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "你"),
            "双拼输入 \"ni\" 经转换后应返回含「你」的候选，实际候选: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    /// 探针（临时）：双拼下手动分隔符。
    #[test]
    fn probe_shuangpin_separator_current() {
        let eng = sp_fp_engine(
            "probe_sep",
            &[
                ("你好", "ni hao", 200),
                ("你", "ni", 100),
                ("西安", "xi an", 150),
                ("好", "hao", 90),
            ],
            false,
        );
        for input in [
            "nhc", "n'hc", "nihc", "xiaj", "xi'aj", "ni'", "'ni", "n''hc",
        ] {
            let sp = eng.shuangpin.as_ref().unwrap().convert(input);
            let r = eng.convert(input, 10).unwrap();
            println!(
                "IN {:?} | full={:?} pre={:?} partial={:?} syl={:?}",
                input,
                sp.full_pinyin,
                sp.preedit_display,
                sp.partial_initial,
                sp.syllables
                    .iter()
                    .map(|s| (
                        s.pinyin.clone(),
                        s.raw_start,
                        s.raw_end,
                        s.fp_start,
                        s.fp_end
                    ))
                    .collect::<Vec<_>>(),
            );
            println!(
                "   -> eng.preedit={:?} disp={:?} cands={:?}",
                r.preedit_pinyin,
                r.preedit_display,
                r.candidates
                    .iter()
                    .take(4)
                    .map(|c| (c.text.clone(), c.consumed_length))
                    .collect::<Vec<_>>()
            );
        }
    }
}
