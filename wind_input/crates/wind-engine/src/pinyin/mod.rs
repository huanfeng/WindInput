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

pub mod correction;
pub mod dag;
pub mod fuzzy;
pub mod generate;
pub mod grammar;
pub(crate) mod hint_align;
pub mod interp;
pub mod lattice;
pub mod misread;
pub mod mixed_abbrev;
pub mod octagram;
pub mod parser;
mod raw_lattice;
pub mod scorer;
pub mod shuangpin;
pub mod spelling;
pub mod syllable;
pub mod viterbi;

use crate::engine::{BoundaryResolution, ConvertOptions, ConvertResult, Engine, EngineType};
use dag::{Dag, SegGraph};
use fuzzy::FuzzyConfig;
use generate::CharPinyinIndex;
use lattice::LatticeBuilder;
use scorer::AbbrevMatcher;
use shuangpin::ShuangpinConverter;
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use syllable::SyllableTrie;
use tracing::debug;
use viterbi::{ViterbiDecoder, WordNode};
use wind_candidate::{Candidate, CandidateSource};
use wind_dict::DictManager;
use wind_dict::cached::CachedDict;

/// convert 分段计时器：Drop 时若总耗时 ≥15ms，把各段与未命名剩余（`rest`）一次性打进日志。
/// 仅诊断用——2026-11-04 真机偶发 convert ≥500ms 但本地/靶机探针均不复现，
/// 需要分段定位瓶颈落点；定位完毕可整块移除。
pub(crate) struct ConvertStageTimer {
    tag: &'static str,
    t: std::time::Instant,
    stages: Vec<(std::string::String, std::time::Duration)>,
}

impl ConvertStageTimer {
    pub(crate) fn new() -> Self {
        Self::with_tag("")
    }

    pub(crate) fn with_tag(tag: &'static str) -> Self {
        Self {
            tag,
            t: std::time::Instant::now(),
            stages: Vec::new(),
        }
    }

    pub(crate) fn mark(&mut self, name: impl Into<String>) {
        let now = std::time::Instant::now();
        let name = name.into();
        self.stages.push((name, now - self.t));
        self.t = now;
    }
}

impl Drop for ConvertStageTimer {
    fn drop(&mut self) {
        const MIN: std::time::Duration = std::time::Duration::from_millis(15);
        let tail = self.t.elapsed();
        let named: std::time::Duration = self
            .stages
            .iter()
            .map(|(_, d)| *d)
            .sum::<std::time::Duration>();
        let total = named + tail;
        if total < MIN {
            return;
        }
        let parts: Vec<String> = self
            .stages
            .iter()
            .map(|(n, d)| format!("{n}={d:?}"))
            .collect();
        debug!(
            "convert 分段[{}]: total={total:?} {} rest={tail:?}",
            self.tag,
            parts.join(" ")
        );
    }
}

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

/// step 2 首位整句（同文合并后也带着这个身份）。
fn is_step2_top(c: &Candidate) -> bool {
    c.is_sentence && c.sentence_rank == 1
}

/// 简拼候选的**保底配额分母**：截断时至少给 `is_abbrev` 留 `max_candidates / 此值` 席
/// （生产 `max_candidates = 300` ⇒ 30 席）。见 [`truncate_with_quotas`]。
///
/// **为什么是 10 而不是混输 `PINYIN_QUOTA_DIVISOR` 的 5**：那边护的是「整个拼音块」，
/// 五笔 2 码前缀动辄数百条、要抢的席位多；这边护的是简拼这一小类，而它的产出在**上游
/// 就被限流**了 —— 纯简拼与混合共用 [`ABBREV_INDEX_LIMIT`] = 64，
/// 再经音节数与逐段校验滤掉绝大多数。真机现场要的是 **1 席**（`shengrikl` 的「生日快乐」
/// 是第 337 位那一条）。30 席是宽松上界且大概率打不满，`extra` 取多少就只挤多少
/// （见 `truncate_with_quotas` 的「只补不挤空」），故放宽的代价有限。
///
/// ⚠️ `max_candidates < 10` 时 `quota == 0`，等价于不设配额 —— 混输子引擎的
/// `sec.convert(input, 8)`、临拼的 limit=1、`apps/repl` 的 limit=9 都落在这一侧，
/// 它们本就只取头几条，给配额没有意义。
const ABBREV_QUOTA_DIVISOR: usize = 10;

/// 换位纠错候选在窗口里保底的上限条数（对齐 librime 纠错候选上限 4）。
const CORRECTION_QUOTA_MAX: usize = 4;

/// 窗口 `max_candidates` 下纠错保底几席：与简拼同按 `max / ABBREV_QUOTA_DIVISOR` 缩放、
/// 封顶 [`CORRECTION_QUOTA_MAX`]。小窗口（< 10，混输子引擎的 8 / 1、repl 的 9）为 0——
/// 引擎层没有 D6，保底一席在 limit=1 时就是把原首选挤掉。
fn correction_quota(max_candidates: usize) -> usize {
    (max_candidates / ABBREV_QUOTA_DIVISOR).min(CORRECTION_QUOTA_MAX)
}

/// 截断到 `max_candidates`，按需给两类候选保底：
///
/// 1. **换位纠错**（`is_corr`，见 [`correction_quota`]）：窗口里不足时把窗口外分最高的几条
///    挪进窗口末尾，被挤出的是尾部其余候选，相对次序不变；
/// 2. **简拼**（`abbrev_quota` 为真时，见下文）：腾位时跳过
///    纠错保底的那几席，两份保底互不抵消。
///
/// 纠错那步保持有序（挪进来的本就排在窗口之后），故 `abbrev_quota = false`（不重排的
/// 调用方）也照做。
///
/// ## 简拼保底
///
/// 保证简拼候选至少留 `max / ABBREV_QUOTA_DIVISOR` 席。
///
/// **为什么需要**：`is_abbrev` 是 [`wind_candidate::cmp_match_layers`] 的**第二**键
/// （首键是 `fp_demoted`），简拼候选因此排在除「低置信全拼降级候选」之外的全部候选之后
/// —— 它一直靠「候选总数碰巧没超过 `max_candidates`」侥幸存活，而这是**数据的性质，
/// 不是代码的保证**。
///
/// 真机现场：`shengrikl`（sheng + ri + k + l）关模糊音时候选共 121 条、「生日快乐」
/// 第 121 位，刚好活在协调器的 limit=300 之内；一开 `sh_s + en_eng`，模糊变体把总数推到
/// 337 条，「生日快乐」随之落到第 337 位 —— **整条被 `truncate` 丢掉**。用户看到的是
/// 「开了模糊音，混合简拼就不好使了」，而这两件事在功能上毫无关系。
///
/// 同型事故本仓已有先例：全拼降级候选带 `is_prefix` 被沉底、落在 595 位被同一个 300
/// 截断（见 [`wind_candidate::cmp_match_layers`] 的 `fp_demoted` 注释）。那次的修法是
/// 改层级键，本次不行 —— 简拼候选质量确实低于全拼精确匹配，把它提上来会污染正常输入的
/// 候选面。**沉底是对的，被整批截掉不对**，所以解法是配额而不是排序。
///
/// 与 `mixed/engine.rs` 的 `truncate_with_pinyin_quota` 逐条同构。
///
/// ⚠️ **这不等于截断处注释说的那件事**：那里要的是「按**消费长度**分档的配额」（为下沉
/// 协调器 P0 排序做准备），本函数给的是**按 `is_abbrev` 的单档配额**。P0 下沉仍未做。
///
/// **只补不挤空**：尾部确实没有简拼候选时 `extra` 为空，一条都不会被挤掉，行为与改动前
/// 完全一致（`quota_does_not_displace_when_no_abbrev_candidate` 守这一条）。
///
/// **腾位必然腾得满**：需腾 `quota - kept` 席，而可腾的非简拼候选有 `max - kept` 席，
/// 由 `quota <= max` 知前者恒不大于后者 ⇒ 结果长度恒 `<= max`，不会溢出上限。
///
/// ⚠️ 补进来的候选**追加在尾部、不保证有序**，依赖调用方重排。已核的调用路径：
/// - 主输入路 `handle_candidate.rs`、临拼路 `handle_temp.rs`：都走
///   `candidate_display_order`（见 candidate-sorting-rules.md §6），安全；
/// - 特殊模式 / overlay `handle_special.rs`：**不重排**，故由
///   [`ConvertOptions::no_abbrev_quota`] 显式关掉配额；
/// - `apps/repl`（limit=9）与混输的几处探测调用（limit 8/1）：`quota == 0`，走原样截断。
///
/// ⚠️ 混输主路径给子引擎传的是**满值** `max_candidates`（`mixed/engine.rs` 有六处），
/// 配额在那里照常生效 —— 混输随后自己还有一道 `truncate_with_pinyin_quota`，
/// 见那边 doc 记的已知缺口。
///
/// 本函数的职责只是「让候选进得来」，不是「排好序」。
fn truncate_with_quotas(
    cands: &mut Vec<Candidate>,
    max_candidates: usize,
    abbrev_quota: bool,
    is_corr: &dyn Fn(&Candidate) -> bool,
) {
    if cands.len() <= max_candidates {
        return;
    }
    let corr_quota = correction_quota(max_candidates);
    if corr_quota > 0 {
        let kept = cands[..max_candidates]
            .iter()
            .filter(|c| is_corr(c))
            .count();
        if kept < corr_quota {
            let picked: Vec<usize> = (max_candidates..cands.len())
                .filter(|&i| is_corr(&cands[i]))
                .take(corr_quota - kept)
                .collect();
            // 从后往前摘，下标不失效；摘出来的保持原序。
            let mut extra: Vec<Candidate> = picked.iter().rev().map(|&i| cands.remove(i)).collect();
            extra.reverse();
            let at = max_candidates - extra.len();
            cands.splice(at..at, extra);
        }
    }
    let quota = if abbrev_quota {
        max_candidates / ABBREV_QUOTA_DIVISOR
    } else {
        0
    };
    let kept = cands[..max_candidates]
        .iter()
        .filter(|c| c.is_abbrev)
        .count();
    if kept >= quota {
        cands.truncate(max_candidates);
        return;
    }
    // 被截掉的那段同样是排好序的，故前几条即权重最高的简拼候选。
    let mut extra: Vec<Candidate> = cands[max_candidates..]
        .iter()
        .filter(|c| c.is_abbrev)
        .take(quota - kept)
        .cloned()
        .collect();
    cands.truncate(max_candidates);
    // 纠错保底的那几席（窗口里前 `corr_quota` 条纠错候选）不让位。
    let mut corr_seen = 0usize;
    let protected: Vec<bool> = cands
        .iter()
        .map(|c| {
            let p = !c.is_abbrev && is_corr(c) && corr_seen < corr_quota;
            corr_seen += usize::from(p);
            p
        })
        .collect();
    // 腾位：从尾部往前挤掉等量的非简拼、非纠错保底候选（排序后尾部即层级最低的那些）。
    //
    // 排在简拼之后的只有 `fp_demoted`（双拼开「允许全拼输入」时的低置信降级候选），
    // 所以**优先被挤掉的正是它们** —— 而它们恰是 `cmp_match_layers` 的 `fp_demoted`
    // 注释里记的那次 595 位事故的主角。这是个有意的取舍：它们本就是「预测尚未输入的
    // 音节」或「只解释了开头一截」的低置信品，而配额最多挤 30 条。
    let mut to_remove = extra.len();
    let mut i = cands.len();
    while to_remove > 0 && i > 0 {
        i -= 1;
        if !cands[i].is_abbrev && !protected[i] {
            cands.remove(i);
            to_remove -= 1;
        }
    }
    // 腾不满（理论上不会：两份保底合计不超过窗口的 1/5）时少补几条，长度不越界。
    extra.truncate(extra.len() - to_remove);
    cands.extend(extra);
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

/// 用户/临时层的**简拼**命中遇到同文已有候选时：合并进去，而不是整条丢弃。
///
/// 整串简拼（step 6.1）与前缀回退（`recall_abbrev_prefix` ③④）共用。规则照搬全拼
/// step 6 的同文合并（那边有完整论证），只取与简拼相关的两条：
/// - `weight` 取 **max**，且只在已有候选**本身在简拼层**（`is_abbrev`）时才动——
///   跨层的同文（例如残码整句）描述的是另一种解释，用户权重不该借合并越层；
/// - 来源标记按命中的实际归属置位，存储码随之带走（右键删除按标记选表、按码拼 key）。
///
/// 旧行为是 `continue`：系统库已有的词（真实词库「拜城县」w=1），用户学过（w=800）后
/// 在 `bcx` 下仍排第 46 位，与没学过一模一样，右键删除也找不到它的记录。
///
/// ⚠️ 两个来源标记**分别**并入：`hit` 来自 `CompositeDict::merge_search`，同一个词同时在用户层
/// 与临时层时两条已合成一条、两个标记都为真。写成 if/else 只会置上一个，右键删除便只删掉
/// 一半、要删第二次（审查查出，同款写法也在 step 6）。
///
/// ⚠️ **刻意不清 `is_synthesized`**（已有候选是 ②b 整句时它为真）：看似「用户层有了就不算
/// 新合成」，但它决定上屏后走不走 `learn_phrase_on_commit`——那一路按造词码（规范码 +
/// 规范边界）写、已存在即 count++。协调器 6b 现按 `store_code` 点查也能命中这条记录，
/// 但对「刚由造词写过的码」刻意跳过（防同一次上屏 +2），两路只走一路。保留标记，计数与
/// 边界补写就始终由造词那一路负责；清掉则改由 6b 计数、边界退回它的推算口径——
/// 行为可以接受，但没有理由在这里悄悄换路。
///
/// `hit_is_fuzzy`：本条是否经模糊变体键命中（处数 > 0），决定它能否改写存储码，见
/// [`merge_store_origin_and_code`]。
fn merge_store_abbrev_hit(
    existing: &mut Candidate,
    hit: &Candidate,
    weight: i32,
    hit_is_fuzzy: bool,
) {
    if existing.is_abbrev {
        existing.weight = existing.weight.max(weight);
    }
    merge_store_origin_and_code(existing, hit, hit_is_fuzzy);
    existing.absorb_codes_from(hit);
}

/// store 层命中 `hit` 并入同文已有候选时：来源标记并入（[`merge_store_origin`]），存储码
/// 随之带走——除非 **`hit` 是模糊命中、且同一层先已并入过同文记录**。
///
/// 同层同文可以有两条记录（遗留的非规范码 + 规范码，或所打码精确那条 + 变体键捞回的那条）。
/// 精确那批先到；模糊那条后到时再改写存储码，右键删除删掉的就是它，用户指向的精确记录原样
/// 留着。全拼 step 6 与简拼合并（[`merge_store_abbrev_hit`]）共用这一份判据。
///
/// 存储码取 `hit.meta.store_code`，缺省用 `hit.code`：模糊命中的 `code` 已换成用户敲的码，
/// 记录码在它自己的 `store_code` 里（`search_store_fuzzy`）。两层码不同的极端情形下这里只
/// 留得住后来那个，删除侧因此仍把 `code` 作为兜底一并尝试。
fn merge_store_origin_and_code(existing: &mut Candidate, hit: &Candidate, hit_is_fuzzy: bool) {
    let same_layer_seen = if hit.meta.is_temp_dict {
        existing.meta.is_temp_dict
    } else {
        existing.meta.is_user_dict
    };
    merge_store_origin(existing, hit);
    if !(hit_is_fuzzy && same_layer_seen) {
        existing.meta.store_code = hit
            .meta
            .store_code
            .clone()
            .or_else(|| Some(hit.code.as_str().into()));
    }
}

/// 把 store 层命中 `hit` 的来源标记并入 `existing`：临时层 → `is_temp_dict`，用户层 →
/// `is_user_dict`，两层都有（`merge_search` 已把同文两条合成一条）→ 两个都置。
///
/// 两个标记都没有的命中按用户层算，与此前 if/else 的兜底一致。
fn merge_store_origin(existing: &mut Candidate, hit: &Candidate) {
    if hit.meta.is_temp_dict {
        existing.meta.is_temp_dict = true;
    }
    if hit.meta.is_user_dict || !hit.meta.is_temp_dict {
        existing.meta.is_user_dict = true;
    }
}

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

/// **近距离补全的上浮名额**：`distance <= COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES` 的
/// 补全按权重降序最多放行这么多条进完整匹配层，其余沉回前缀层。
///
/// ## 为什么无条件上浮必须有名额
///
/// 「近距离无条件上浮」本意是让「没有」这种高频补全别被数百条同音单字淹掉。但它对
/// **同距离的一大批**候选没有任何约束：`meiy` 的残码 `y` 一个音节就能补出 58 条
/// （没有/美元/每月/没用/每一/美院…），全部 distance=1、全部无条件上浮，于是
/// 单字「没」被整批压到第 59 位 —— 从「单字淹掉词」翻转成「词淹掉单字」，只是方向反过来。
///
/// 该常量的原注释已预见这个风险（「若 30 条补全全部上浮，长输入下候选 2~5 位会被该前缀下
/// 的冷僻长词占满」），但给出的约束是**距离 + 置信度**，对同距离同档的 58 条不起作用。
///
/// ## 为什么是名额而不是权重门槛
///
/// [`COMPLETION_FAR_WEIGHT_FLOOR`] 的文档写明了不能对近距离套门槛的理由：词库 weight_spec
/// 的 median 仅 200，一半的词低于它，按权重卡会误沉大量「高频使用但低词频」的日常词。
/// 名额则与词频分布无关 —— 不管这批词权重高低，只让最靠前的几条插队。
///
/// 取 8：候选窗每页 5~9 条（`ui.candidate.per_page` 出厂 5、extended 7），8 条保证高频补全
/// 占满首页仍有余量，而单字最迟在第二页露面。实测 `meiy`：单字「没」从第 59 位升到第 9 位。
const COMPLETION_NEAR_PROMOTE_LIMIT: usize = 8;

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
/// 剥除手动分隔符 `'` 后的前 `n` 个字节，对应原始输入里的字节长度（不含紧随的分隔符）。
fn raw_len_for_stripped(input: &str, n: usize) -> usize {
    let mut seen = 0usize;
    for (i, ch) in input.char_indices() {
        if ch == '\'' {
            continue;
        }
        seen += ch.len_utf8();
        if seen >= n {
            return i + ch.len_utf8();
        }
    }
    input.len()
}

const MAX_COMPLETION_CANDIDATES: usize = 1000;

/// 查 `AbbrevSection` 时的取码上限。纯简拼（step 5 / 回退①）与混合简拼（step 5b / 回退②）
/// **共用同一个值**——两条路径查的是同一张表、同一个键。
///
/// 为什么混合路径需要 64：拿到的码还要过一道逐段校验（`nh` 下的 `nihao`/`nanhai`/`naihe`…
/// 只有第二段等于 `hao` 的能活下来），**绝大多数会被滤掉**，取 10 条几乎必然一条不剩。
/// 索引点查本身是 DAT 前缀走位 + 定长条目读取，放宽的成本远小于「查了等于没查」。
///
/// ★ **纯简拼曾单独取 10，已废止。** 那个值的理由是「键即答案，按权重取前 10 就是最终
/// 候选」——该前提只在**键下词条少于 10** 时成立。真实词库里 `bcx` 有 48 条，取 10 等于
/// 宣布「这个键下只有最热门的 10 个词存在」：`bai cheng xian`（拜城县，w=1，第 48 条）
/// 在 `bcx` 下永不产生，而在 `baicx`（混合、64）下是第 77 位候选 —— **打得更省事的写法
/// 反而查得更少**。且因为截断在**召回层**，候选窗开到 300 也捞不回来，调频同样救不回来
/// （位次重排只能重排已经进了列表的候选）。两条路径的窗口必须一致，见
/// `tests/pinyin_abbrev_recall_window.rs`。
///
/// ⚠️ **已知上界：只有单键上限，没有总额。** 成本是
/// `变体键数 × 本值 × 每码的 search_with_boundary`，而 `fuzzy_abbrev_keys` 最多返回
/// `MAX_ABBREV_KEY_VARIANTS`(64) 个变体键，step 6.2 还要逐切点重来一遍。
/// 真机计时（`build_dev/data`、release、开满六个声母模糊组，见
/// `tests/pinyin_abbrev_recall_latency.rs`）最坏 `zhy` 551µs / `lll` 246µs，
/// 离按键线程的预算还远，故**不设总额**。但同样的形状在合成词库（每键塞满码）下
/// `lll` 会到 4.99ms —— 真实 `cn_dicts` 一个 3 位键下只有几十条码，这个余量靠的是
/// **数据分布而非代码保证**。词库形态变了（超大码表、机器生成的键）先重跑那个基准。
const ABBREV_INDEX_LIMIT: usize = 64;

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

/// 简拼前缀回退（step 6.2）的**内部**限额，不进用户配置（`wind-config` 不认它）。
///
/// 集中成一个结构，是为了以后调值 / 对不同来源（移动端、混输）分档时只改这一处。
/// 默认值参考主流输入法实测（2026-11）：微软拼音中间级不限条数，豆包 3 字不限、2 字 8 条。
#[derive(Debug, Clone)]
pub struct AbbrevFallbackConfig {
    /// 前缀回退**参与竞争的切点数**（有产出的才计数）。
    ///
    /// 默认 2 = 「最长 + 次长」。相邻切点的竞争才是真实场景（`bzdhaobuhao` 里 `bzdh`→「表彰大会」
    /// 与 `bzd`→「不知道」），再往下只会引入越来越短、越来越不相关的解释。
    ///
    /// ★ **整串无命中时放宽它已被评测否决。** D 类（简拼+全拼混合整句）实测：
    /// 取 2 → 首词召回 40.30% / 首词命中 2.30%；取 8 → **14.60% / 0.00%**，大幅恶化。
    /// 原因是短切点的高频词会占据首选并把长切点的正确候选挤出 top-10。整串命中时补进来的
    /// 候选都沉在整长匹配之后，没有这个风险，但同一个值两种场景共用，改它先重跑 pinyin_eval。
    pub cuts: usize,
    /// 切点长度 ≥ 3 个字母（三字词组及以上）时，单切点、**单个来源层**的产出上限。
    ///
    /// ⚠️ **系统层与 store 层各记各的**（`recall_abbrev_prefix` 的 `start` / `store_base`），
    /// 故单切点实际上限是 **2×** 本值。共用一份时用户自己造的词会被系统高频词挤出。
    /// 调值时对照候选规模（`mixed_partial_pinyin_filter.rs` 记着「219 条残码单字把简拼词压到
    /// 第 221 位」的教训）。受 `ABBREV_INDEX_LIMIT`（每个简拼键最多 64 个码）约束。
    pub per_cut_long: usize,
    /// 切点长度 = 2 个字母（两字词组）时的同类上限。两字词数量大、噪音多，取得比长切点小。
    pub per_cut_short: usize,
}

impl Default for AbbrevFallbackConfig {
    fn default() -> Self {
        Self {
            cuts: 2,
            per_cut_long: 50,
            per_cut_short: 20,
        }
    }
}

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
/// - **有边界**（GUI 加词/学习词带音节真值，或码能 DAG 切出音节）：`started ≥ 2` 且
///   **距词尾 ≤ `max_extra`**（用户配置的 `completion.max_extra_syllables`）才上浮。
/// - **算不出音节数**：退化为「`started ≥ 3`」门槛，同样对齐「打到第 3 个音节才给」，
///   避免 1-2 音节时被一堆冷僻长词占满前排。
///
/// # `word_syls` 必须是 [`effective_boundary`] 现切的结果，不能是 `boundary.count_ones()`
///
/// ⚠️ **`boundary == 0` 不等于「音节数算不出」**，这是本判据踩过的坑：手输码用户词恒
/// `boundary = 0`，而它的码本身就是合法全拼串、DAG 现切即得音节数。此前本函数直接读裸
/// `boundary` ⇒ 这批词一律落进 `started >= 3` 那条退化门槛，于是**同一个词有没有带
/// 边界字段，决定了它在 2 音节输入下能不能被看见**：5 音节的 `qingfengshurufa` 用户词在
/// `qingf`(started 2) 下，带边界那条上浮到首位，手输码那条沉在补全层。
///
/// 更要紧的是它与**召回门不同口径**：召回门早已过 `effective_boundary` 把这批词放进来了，
/// 上浮判据却拿裸 `boundary` 按「无信息」处置 —— 正是本文档末尾要消除的那个
/// 「召回了却沉在必被截断的位置」的中间态。
///
/// 入参之所以是**算好的音节数**而非 `(code, boundary, trie)`：三个调用点里有两个在紧邻
/// 几行内已经为显示序档位算过一次，传进来即可复用（`boundary == 0` 时那是一次
/// `Dag::build`，而手输码用户词恰好全是 `boundary == 0`）。顺带消掉「code 与 boundary
/// 取自不同候选」这个类型系统管不着的面 —— `d4084b8` 踩过一次「A 层的码 + B 层的边界」。
///
/// # `remaining <= max_extra` 在主路径上恒真，别拿它写护栏
///
/// step 6.3 的 `retain` 用 `syllable_cap = started + max_extra` 裁掉所有 `is_prefix`
/// 候选，而 `word_syls <= started + max_extra` 与本函数的 `remaining <= max_extra`
/// **是同一个不等式** ⇒ 能活到这里的候选必然满足它，起作用的只剩 `started >= 2`。
///
/// ⇒ **想验「距词尾太远所以不上浮」，只能在 step 6.7 全拼降级支路上做**：那条路的
/// 用户词刻意不受 `completion_syllable_cap` 约束（见 `recall_full_pinyin` 里那段
/// 「没有跟着加 completion_syllable_cap」的取舍说明）。首版护栏在主路径上写了三处
/// 「收紧 max_extra ⇒ 不该上浮」的对照，候选被召回门先一步删光、断言全部空过，
/// 把本函数整个改成 `return true` 也照样全绿。
///
/// 同理，反方向（`started >= 3` 且距词尾 > `max_extra` 时旧逻辑会误上浮）在主路径上
/// **是理论缺口而非可复现故障**。
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
    word_syls: u32,
    max_extra: u32,
) -> bool {
    let started = completed_syls + usize::from(trailing_partial);
    if word_syls != 0 {
        let remaining = (word_syls as usize).saturating_sub(started);
        started >= 2 && remaining <= max_extra as usize
    } else {
        // 音节数算不出来：退化为「打到第 3 个音节才给」。
        //
        // ⚠️ 这里只兜得住**一个音节都切不出**的码（空码、整串非拼音）。码切不满
        // （`daboluoqq` → `da|bo|luo`）走的是上面那条，拿低估的音节数判 —— 见
        // `effective_boundary` 文档里那条「一声不吭地少算」。
        started >= 3
    }
}

/// 拼音引擎配置
#[derive(Debug, Clone)]
pub struct Config {
    // 注：曾有 `show_code_hint: bool`。**已删** —— 引擎侧从未读过它，编码提示的门控
    // 早在 387ffa60「移除死状态 code_hint_cache」那次就改成协调器直接读全局配置了
    // （后来编码来源整个并入注释分配，见设计 candidate-comment-templates §13.6），这个字段是那次重构漏下的残留：
    // manager 每次建引擎都老实赋值，而引擎内部一个消费点都没有。
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
    /// 是否让**用户上屏过的词**（用户词 + 临时词）参与整句解码（S2）。**出厂 true**
    /// （2026-09-28 起；此前出厂 false，理由见下）。
    ///
    /// 关闭时整句词图只从系统词库建，用户自造词永远不会成为整句的一段：
    /// 「盖伦」单独打得出、「有盖伦吗」却被打散（t134），手动调过权重的词在整句里
    /// 同样不认（GH#93）——那不是联想弱也不是词频没学到，是它**根本没参与整句分词**。
    ///
    /// ★ 曾经出厂 false，理由是三条结构事实：
    /// - 出厂只出**一条**整句（`sentence_count = 1`）⇒ 用户词进图是**赢者通吃**，
    ///   赢了整句就变、输了什么都看不见。调大 N-best 才看得到它输给了谁；
    /// - 它会改变**所有**老用户的整句结果，包括从未造过词的人（wdict 导入词也进图）；
    /// - 用户词的 weight 轴与词频轴同源但语义不同（前者是「我要这个词」，后者是频次），
    ///   标定只做了上限截断（见 [`USER_NODE_WEIGHT_CAP`]），没有做分布对齐。
    ///
    /// 这三条仍是已知局限，2026-09-28 起不再作为出厂值的否决理由，翻成出厂 true。
    /// 它只在 `use_smart_compose`（智能组词）开着时才有效——设置页据此把它的
    /// `enabled_when` 挂在智能组词上，关闭智能组词时本项在引擎侧是死代码。
    ///
    /// ⚠️ **接用户词与临时词，不接草稿层**。S5 滑窗会造出大量杂词，它的「用过即转正」
    /// （草稿 → 临时）才是质量闸；杂词若直接进整句词图，污染的是所有人的整句。临时词曾经
    /// 也被挡，结果系统库没有的词（「拜城县」）手打一次后整句仍不认，要用够晋升次数才行。
    /// ⚠️ 临时层也收自动造词直接写入的**分步上屏拼接**与**选中的合成整句**（不经草稿），
    /// 分错段的一次上屏会以整节点进图自我强化，见 `lattice::add_store_nodes` 约束 1。
    ///
    /// 全拼段点查全拼码（`lattice::add_store_nodes`），简拼整句（②b）的声母段另查用户层的
    /// 简拼索引（`lattice::add_store_abbrev_nodes`）。
    pub sentence_uses_user_words: bool,
    /// 整句 N-best：候选列表里**露**几条整句（step 2 主整句）。出厂 1 = 现在的行为。
    ///
    /// 起因是可验证性：整句只出一条、赢者通吃，用户词（S2）赢了才看得见、输了什么线索都
    /// 没有。调到 >1 后次优整句也进候选，协调器把整句块连续摆到最前（`sentence_rank`）。
    ///
    /// ⚠️ 只作用于 step 2 主整句。2b 混合整句 / 2c 残码整句仍各出一条：它们是特殊形态的
    /// 补充解读（2c 连消费长度都与主整句不同层），多解意义不大、叠在一起反而难辨认。
    pub sentence_count: u8,
    /// 整句 N-best：解码时**算**几条（切换键的池子）。`>= sentence_count`，不足时按后者抬。
    ///
    /// 算得多、露得少：候选列表里露 `sentence_count` 条，切换键在这 `sentence_max_count`
    /// 条里滚动窗口，切换时不必重新解码。
    pub sentence_max_count: u8,
    /// 简拼前缀回退的内部限额，见 [`AbbrevFallbackConfig`]。
    pub abbrev_fallback: AbbrevFallbackConfig,
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

/// step 4 的词库前缀补全在本次输入下**注定只剩精确匹配**，可整段跳过（看板 A3-14）。
///
/// 成立条件：整音节输入（无残码）且 `syllable_cap <= 1`（出厂 `min_syllables = 4` 下的
/// **单音节**输入：`ni`/`xi`/`hu`/`zhong`，双拼的一个键对）。此时词库层
/// `prefix_entry_keep` 的两半互相矛盾：对齐判据要求补全在位 0 之外、在 `completed_len`
/// （= 整串长度）处**再开一个音节**，即至少 2 个音节；音节数判据又不许超过 1 —— 能同时
/// 满足的只有码 == 输入的精确匹配（`code_len <= completed_len` 那条放行），而它们
/// step 1 已全数给出（`push_unique` 按文去重，step 4 再推一遍是空操作）。
/// 词库层放行的 `boundary == 0` 条目到 6.3 由 `effective_boundary` 补出边界后同样
/// 过不了这两条；唯一漏口是码长 ≥ 64 字节（掩码表达不了、补出 0）的无边界条目——那种
/// 十几个音节的「补全」本就违背 cap，真实词库里也一条没有。
///
/// 跳过省下的是整棵前缀子树的分支限界扫描：`xi` 约 10 万状态、38ms，`ni`/`hu` 同量级，
/// 而结果恒为空。对拍（1~3 位字母穷举 + 全部单音节 + 多音节整词 + 手动分隔符 + 双拼，
/// 出厂与模糊音全开）逐字段一致，见 `step4_skip_tests`。
///
/// ⚠️ **不能放宽到「`cap == started` 的多音节整音节输入」**，那里不是注定为空：音节切分
/// 来自手动分隔符或双拼（真值、可以比词条的切分更细）时，词条切分更粗的补全能同时过
/// 两条判据 —— `xi'an` 下 `xian|ren`（先人）、`xian|dai`（现代）都是 2 音节且在位 4
/// 开新音节，改动前照样产出。多音节输入的前缀子树本就小，跳过也省不了什么。
///
/// ⚠️ 残码位（`trailing_partial`）绝不能跳：前缀补全存在的意义就在那里（`meiy`→「没有」）。
/// `cap > 1`（用户把 `min_syllables` 调到 1）时单音节补全合法地多出音节，也不能跳。
fn prefix_completion_is_exact_only(trailing_partial: bool, syllable_cap: u32) -> bool {
    let skip = !trailing_partial && syllable_cap <= 1;
    #[cfg(test)]
    let skip = skip && !test_hooks::never_skip_step4();
    skip
}

/// 仅测试用：观察 / 关闭 step 4 的跳过，供对拍与计数护栏使用（线程局部，互不串扰）。
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::Cell;

    thread_local! {
        static STEP4_PREFIX_SCANS: Cell<u32> = const { Cell::new(0) };
        static NEVER_SKIP_STEP4: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn note_step4_prefix_scan() {
        STEP4_PREFIX_SCANS.with(|c| c.set(c.get() + 1));
    }

    /// 取出并清零本线程 step 4 实际发起的词库前缀扫描次数。
    pub(crate) fn take_step4_prefix_scans() -> u32 {
        STEP4_PREFIX_SCANS.with(|c| c.replace(0))
    }

    pub(crate) fn never_skip_step4() -> bool {
        NEVER_SKIP_STEP4.with(Cell::get)
    }

    /// `true` = 回到改动前的行为（每次都扫），用作对拍的基线。
    pub(crate) fn set_never_skip_step4(v: bool) {
        NEVER_SKIP_STEP4.with(|c| c.set(v));
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
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
            // 默认开（2026-09-28 起）。字段文档三条结构事实仍是已知局限，但不再是出厂关的理由。
            // ⚠️ 与 `wind_config::PinyinGlobalConfig` 那份默认值**保持同值**，同 completion 两项。
            sentence_uses_user_words: true,
            // 出厂 1/1 = 现在的行为（单条整句），K-best 在 K=1 时与单路径 DP 逐位相同。
            // ⚠️ 与 `wind_config::PinyinGlobalConfig` 那两项**保持同值**。
            sentence_count: 1,
            sentence_max_count: 1,
            abbrev_fallback: AbbrevFallbackConfig::default(),
        }
    }
}

/// 整句 N-best 两个数量的上限。
///
/// 解码代价随 K 线性增长（`dp[i]` 每个位置 top-K），而候选窗一页通常 5~9 条，
/// 整句块再大就把普通候选挤出首页了。8 是「一页装得下、再多也看不过来」的界。
pub const SENTENCE_COUNT_LIMIT: u8 = 8;

impl Config {
    /// 规整后的 (露几条, 算几条)：两者都夹在 `1..=SENTENCE_COUNT_LIMIT`，且算的不少于露的。
    ///
    /// 放在这里而不是配置加载处：引擎也可能被直接构造（测试、REPL），规整只写一处。
    pub fn sentence_counts(&self) -> (usize, usize) {
        let show = self.sentence_count.clamp(1, SENTENCE_COUNT_LIMIT) as usize;
        let pool = (self.sentence_max_count.clamp(1, SENTENCE_COUNT_LIMIT) as usize).max(show);
        (show, pool)
    }
}

/// step 6.5e：不过闸的辅码整句（`aux_tail` 里 `pass = false` 的）在界面序下的排位。按界面排序键
/// （权重之前的各键）找同档对手，压到其中最强者之下；有更靠前一档的候选时不动；一个对手都没有时
/// 它在界面上必居首，删掉。
fn place_failed_aux_tail(
    candidates: &mut Vec<Candidate>,
    aux_tail: &[(String, usize, bool)],
    raw_input: &str,
) {
    for (text, n, pass) in aux_tail {
        if *pass {
            continue;
        }
        let Some(i) = candidates
            .iter()
            .position(|c| c.is_sentence && &c.text == text && c.consumed_length == *n)
        else {
            continue;
        };
        let mut probe = candidates[i].clone();
        let mut ahead = false;
        let mut rival_w: Option<i32> = None;
        for (j, c) in candidates.iter().enumerate() {
            if j == i {
                continue;
            }
            // 权重与两级基序取对方的值：比出来的就是权重之前那几键的胜负。
            probe.weight = c.weight;
            probe.base_order = c.base_order;
            probe.natural_order = c.natural_order;
            match wind_candidate::candidate_display_order(c, &probe, false, false, raw_input) {
                std::cmp::Ordering::Less => ahead = true,
                std::cmp::Ordering::Equal => rival_w = rival_w.max(Some(c.weight)),
                std::cmp::Ordering::Greater => {}
            }
        }
        match rival_w {
            Some(w) => candidates[i].weight = candidates[i].weight.min(w.saturating_sub(1)),
            None if !ahead => {
                candidates.remove(i);
            }
            None => {}
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
    /// 「屏蔽这个词」的文字（≥2 字），由 EngineManager 推入。空 = 无屏蔽，各处零额外开销。
    blocked_words: std::sync::RwLock<Arc<HashSet<String>>>,
}

/// 整句恰为被屏蔽的文字时，N-best 在原有条数上多取几条找替补。
const BLOCKED_SENTENCE_EXTRA: usize = 3;

/// 整句文字是否被屏蔽（解码失败的空路径不算）。
fn sentence_blocked(blocked: &HashSet<String>, words: &[String]) -> bool {
    !blocked.is_empty() && !words.is_empty() && blocked.contains(&words.concat())
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
            blocked_words: Default::default(),
        }
    }

    /// 当前屏蔽集合的快照。
    fn blocked_words(&self) -> Arc<HashSet<String>> {
        self.blocked_words
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 被屏蔽的文字不作为词节点进整句图——系统、用户、临时、简拼各来源节点都在这之前进图，
    /// 一处挡全。由更小的块拼出同文的情形由 [`Self::decode_unblocked`] 兜。
    fn drop_blocked_nodes(&self, nodes: &mut [Vec<lattice::LatticeNode>]) {
        let blocked = self.blocked_words();
        if blocked.is_empty() {
            return;
        }
        for at in nodes.iter_mut() {
            at.retain(|n| !blocked.contains(&n.word));
        }
    }

    /// 单路解码；整句文字恰为被屏蔽的文字（由更小的块拼成）时用 N-best 多取几条、取第一条
    /// 不是的，全被屏蔽则按解码失败返回。无屏蔽或没撞上时就是 `decode`，不多解一次。
    /// 替补不看语法模型（挂了模型时 `decode_nbest` 只回 1 条）。
    fn decode_unblocked(&self, lattice: &[Vec<WordNode>], len: usize) -> viterbi::ViterbiResult {
        let result = self.viterbi.decode(lattice, len);
        let blocked = self.blocked_words();
        if !sentence_blocked(&blocked, &result.words) {
            return result;
        }
        self.viterbi
            .decode_nbest_lexical(lattice, len, 1 + BLOCKED_SENTENCE_EXTRA)
            .into_iter()
            .find(|r| !sentence_blocked(&blocked, &r.words))
            .unwrap_or(viterbi::ViterbiResult {
                words: Vec::new(),
                log_prob: f64::NEG_INFINITY,
                lexical_log_prob: f64::NEG_INFINITY,
                boundary: 0,
                pieces: None,
            })
    }

    /// 注入用户/临时造词层（L 造词显现）。链式 builder：构造后由 EngineManager 按 schema 挂上。
    pub fn with_store_layers(mut self, layers: Arc<DictManager>) -> Self {
        self.store_layers = Some(layers);
        self
    }

    /// 注入模糊音配置（取代 with_unigram 中的 FuzzyConfig::default()）。
    ///
    /// 同时把「错音串」注册进切分层：模糊变体是在切分**之后**逐音节展开的，`tin`
    /// （想打 `ting`）这类本身不成音节的串若在图上没有边，`tinzhi` 最远只能切到 `ti`，
    /// 整条模糊链路一次都不会被执行。见 [`fuzzy::fuzzy_spellings`]。
    ///
    /// 双拼同键音节（[`FuzzyConfig::shared_key`]）属于布局、由 [`Self::with_shuangpin`] 注入，
    /// 用户模糊配置里没有这项：传入的为空时沿用已注入的那份，两个 builder 谁先调都不丢。
    ///
    /// 双拼方案不启用换位纠错：已注入双拼转换器时丢掉 [`FuzzyConfig::correction`]。
    pub fn with_fuzzy(mut self, mut fuzzy: FuzzyConfig) -> Self {
        if self.shuangpin.is_some() {
            fuzzy.correction = Default::default();
        }
        self.trie
            .load_fuzzy_spellings(&fuzzy::fuzzy_spellings(&fuzzy));
        // 同一拼写也是模糊错音串时（自定义 `in=ian` 的 `din` 也是 in/ing 的 `ding`）不标纠错边，
        // 否则主切分连模糊那条也一起丢了。
        let fuzzy_only = fuzzy::fuzzy_spellings(&FuzzyConfig {
            correction: Default::default(),
            ..fuzzy.clone()
        });
        self.trie.set_correction_spellings(
            fuzzy
                .correction
                .spellings()
                .filter(|s| fuzzy_only.binary_search_by(|f| f.as_str().cmp(s)).is_err()),
        );
        if fuzzy.shared_key.is_empty() {
            fuzzy.shared_key = std::mem::take(&mut self.fuzzy_config.shared_key);
        }
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
    ///
    /// 同键韵母的等价音节也在此推出（见 [`ShuangpinConverter::shared_key_syllables`]），
    /// 并进模糊音展开、零罚分。全拼方案不走这里，故不产生这些对。
    pub fn with_shuangpin(mut self, conv: ShuangpinConverter) -> Self {
        self.input_chars = conv.layout().code_char_set();
        self.fuzzy_config.shared_key = conv.shared_key_syllables();
        // 双拼不启用换位纠错（规则是全拼字母层的）；先注入的纠错拼写要从切分层摘掉。
        if !self.fuzzy_config.correction.is_empty() {
            self.fuzzy_config.correction = Default::default();
            self.trie
                .load_fuzzy_spellings(&fuzzy::fuzzy_spellings(&self.fuzzy_config));
            self.trie.set_correction_spellings([]);
        }
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

    /// 混输闸门四件套（`is_possible_pinyin_sequence` / `is_whole_syllable_pinyin` /
    /// `has_non_initial_single_letter_syllable` / `completed_syllable_count`）的公共入口归一。
    ///
    /// 这四个方法回答的都是「这串击键像不像拼音」，判据直接落在音节 Trie 上。用户打 `xv`
    /// 时 Trie 走不通 ⇒ 闸门判假 ⇒ 混输方案下这串键**根本进不到拼音引擎**，
    /// [`convert_with_opts`](Engine::convert) 里的那次归一化再对也没用。
    ///
    /// ⚠️ 与 `full_pinyin_gate` **有意不同**：那是双拼下的全拼降级支路，入参是双拼击键，
    /// 其中的 `v` 是布局键（小鹤 `v` = zh），归一化会毁掉它。故此处按 `shuangpin` 短路，
    /// 而不是把归一化下沉进 `contiguous_completed_from_start`（那会同时污染两条路）。
    fn normalize_gate_input<'a>(&self, prefix: &'a str) -> std::borrow::Cow<'a, str> {
        if self.shuangpin.is_some() {
            return std::borrow::Cow::Borrowed(prefix);
        }
        spelling::normalize_u_umlaut(prefix)
    }

    /// 从起始位置贪心切出连续完整音节（每步取最长匹配），返回 (音节序列, 结束字节位置)。
    /// 对齐 Go `ContiguousCompletedFromStart`：遇到无完整音节即停（残缺尾部不计入）。
    ///
    /// 换位纠错的错误拼写（`hagn`、`zhon`）**不算**音节：这几道闸门答的是「这串键像不像拼音」，
    /// 纠错边只为切得动，不该让混输把五笔码 `hagn` 当成打到一半的拼音、拦下满码上屏。
    fn contiguous_completed_from_start(&self, prefix: &str) -> (Vec<String>, usize) {
        let correction = &self.fuzzy_config.correction;
        let mut syllables = Vec::new();
        let mut pos = 0;
        while pos < prefix.len() {
            // match_at 返回该位置所有完整音节，最长优先；取最长贪心推进。
            let matches = self.trie.match_at(prefix, pos);
            let Some(syl) = matches
                .into_iter()
                .find(|s| correction.corrections_of(s).is_empty())
            else {
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

    /// 一条候选的编码栏切分（`CandidateMeta::preedit_split`）。只在全拼、无手动分隔符时调用
    /// （双拼下只对全拼降级候选，传的是击键串按全拼切出的那一份），此时 `completed + partial == raw_input`。
    ///
    /// | 候选 | 切分 |
    /// |---|---|
    /// | 覆盖全部完整音节（整句 / 精确词 / 模糊命中） | 自己的边界，残码另起一段 |
    /// | 混合整句（code 即整串击键） | 自己的击键域边界 |
    /// | 简拼 / 混合简拼 | 击键分段，见 [`Self::abbrev_keystroke_preedit`] |
    /// | 只覆盖前缀（子短语、单字、残码单字） | 自己的边界 + 尾部最大匹配 |
    /// | 其余（补全、无边界真值） | `None`，协调器退回首选那份 |
    ///
    /// 边界取 `boundary`；模糊命中的 `boundary` 恒 0（与所打码不同域），取 `fuzzy_typed_boundary`。
    /// 尾部切法与 `abbrev_keystroke_preedit` 的部分匹配同口径：选走这条后剩下的码再切一遍。
    fn candidate_preedit_split(
        &self,
        c: &Candidate,
        raw_input: &str,
        completed: &str,
        partial: &str,
        cache: &mut PreeditSplitCache,
    ) -> Option<Arc<str>> {
        // 纠错音节 + 简拼尾：边界本就在所打击键域（含尾巴起点），整串直接渲染。
        if c.meta.tail_typed_boundary != 0 {
            return Some(
                cache.get((4, c.meta.tail_typed_boundary, raw_input.len()), || {
                    render_preedit(raw_input, c.meta.tail_typed_boundary, "")
                }),
            );
        }
        let boundary = if c.boundary != 0 {
            c.boundary
        } else {
            c.meta.fuzzy_typed_boundary
        };
        if boundary != 0 && c.code == completed && !completed.is_empty() {
            return Some(cache.get((0, boundary, completed.len()), || {
                render_preedit(completed, boundary, partial)
            }));
        }
        // 纠错召回的整串候选：主切分不走纠错边，`completed` 覆盖不到它（`gognping` → 空串），
        // 按所打码切分显示成 `gogn'ping`。
        // 只认纠错候选：关纠错时这一支结构上不可达。
        // 末尾半截纠错（`konglon` 的「恐龙」，`lon` 对齐判作前缀、不标纠错）同样是纠错召回的整串候选，
        // 码即所打码、边界在击键空间，不跟上就退回主切分显示成 `kong'lo'n`，与它吃满整串自相矛盾。
        let recalled_by_correction =
            c.is_corrected || (c.is_fuzzy && c.meta.fuzzy_typed_boundary != 0);
        if recalled_by_correction
            && boundary != 0
            && c.code == raw_input
            && !c.is_abbrev
            && !c.is_sentence
        {
            return Some(cache.get((3, boundary, raw_input.len()), || {
                render_preedit(raw_input, boundary, "")
            }));
        }
        if c.is_sentence && c.boundary != 0 && c.code == raw_input {
            // 混合整句（step 2b）：code 是整串**击键**、boundary 也在击键空间（简拼段每字母
            // 一位），得 `b'z'd'hao'bu'hao`。走不到上一支是因为 `completed` 对这类输入恒为
            // 空串——`bzdhaobuhao` 从位置 0 就切不出完整音节。
            return Some(cache.get((1, c.boundary, raw_input.len()), || {
                render_preedit(raw_input, c.boundary, "")
            }));
        }
        if c.is_abbrev {
            return self.abbrev_keystroke_preedit(c, raw_input).map(Arc::from);
        }
        let len = c.code.len();
        if boundary != 0 && len > 0 && len < raw_input.len() && raw_input.starts_with(&c.code) {
            return Some(cache.get((2, boundary, len), || {
                let (tail, _, _) = self.compose_segment(&raw_input[len..]);
                format!("{}'{tail}", render_preedit(&c.code, boundary, ""))
            }));
        }
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
    /// S2：把已晋升的用户词追加进词图（出厂关）。三条整句通路共用这一个入口。
    ///
    /// ⚠️ **必须在 `LatticeBuilder::build` 之后调**：同词同起点时它要拿用户词的分去更新
    /// 既有节点（见 `add_store_nodes` 的第三条约束），那个节点得先在图里。
    ///
    /// 开关与 `store_layers` 的判定收在这里，调用点只写一行 —— 三处各写一遍 `if` 的话，
    /// 新增通路时漏掉判定就是「开关对那条路不生效」，而那种错在测试里长得和「功能没接」
    /// 一模一样。
    /// `require_reachable` 传与同一张图的 `build` 相同的值。
    fn maybe_add_store_nodes(
        &self,
        input: &str,
        graph: &SegGraph,
        require_reachable: bool,
        nodes: &mut [Vec<lattice::LatticeNode>],
    ) {
        if self.config.sentence_uses_user_words
            && let Some(store_dm) = &self.store_layers
        {
            self.lattice_builder.add_store_nodes(
                input,
                graph,
                store_dm,
                Some(&self.fuzzy_config),
                require_reachable,
                nodes,
            );
        }
    }

    /// 串的开头已经是完整音节（`baxzd` 的 `ba`、`buxzd` 的 `bu`），或两字母就是声母
    /// （`sh`/`zh`/`ch`）：此时首字母不是用户要的单字声母，不补首声母单字。
    fn first_letters_start_a_syllable(&self, abbr_query: &str) -> bool {
        let max = abbr_query.len().min(6);
        (2..=max).any(|n| abbr_query.is_char_boundary(n) && self.trie.is_syllable(&abbr_query[..n]))
            || (abbr_query.len() == 2 && self.is_possible_pinyin_sequence(abbr_query))
    }

    /// 前缀回退的末一档：只消费首个声母，召回以它开头的高频单字（沉在词组之后）。
    ///
    /// 与 `recall_abbrev_prefix` 的 `MIN_ABBREV_STROKE` 下限分开：单字母本不构成简拼，
    /// 这里不是在解释「简拼」，而是把首字母当作单字的声母；产出一律 `is_partial`，
    /// 排在所有整长匹配与词组前缀之后。条数**不套** `AbbrevFallbackConfig::per_cut_*`：用户期望
    /// 和直接打首字母一样能挑任意单字，取数量与主路径 step 4 同口径（`limit` 由调用方给）。
    fn recall_first_initial_chars(&self, initial: &str, limit: usize, cands: &mut Vec<Candidate>) {
        // 音节数上限 1 ⇒ 只取单音节词条（单字），边界对齐位 0 ⇒ 退化为字符前缀匹配。
        for (idx, h) in self
            .dict
            .search_prefix_with_boundary_syllable_capped(initial, limit, 1, 0)
            .into_iter()
            .enumerate()
        {
            if h.text.chars().count() != 1 || cands.iter().any(|c| c.text == h.text) {
                continue;
            }
            cands.push(Candidate {
                text: h.text,
                code: h.code,
                // 单字权重（字频，量级远高于词组）若原样带入，引擎序里会压过词组，候选上限一收紧
                // 词组先被截掉。权重置 0、用词库返回序（已按权重降序）当 natural_order 保持单字间先后；
                // 界面序仍由消费长度决定。也不吃下方裸声母单字提权（同理）。
                weight: 0,
                natural_order: idx as i32,
                source: CandidateSource::Pinyin,
                is_abbrev: true,
                is_partial: true,
                boundary: h.boundary,
                consumed_length: initial.len(),
                ..Default::default()
            });
        }
    }

    fn recall_abbrev_prefix(&self, stroke: &str, consumed: usize, cands: &mut Vec<Candidate>) {
        let trie = &self.trie;
        let dict = &self.dict;
        // 本切点的产出配额（见 `AbbrevFallbackConfig::per_cut_long`：不逐切点限流，长切点会把额度占满，
        // 词频高得多的短切点一条都进不来）。
        //
        // ★ **系统层与 store 层各记各的基准**：配额按 `base` 分段计，系统层 ①② 用 `start`，
        // 用户/临时层 ③④ 另起一个（见下方 `store_base`）。共用一份时 ③④ 排在 ①② 之后，
        // 系统的高频词占满 6 席 ⇒ 用户自己造的词在**长输入**下静默出局，而短输入（整串走
        // step 5/6，不经本函数）照常出得来 —— 真机观感是「短的打得出、长的就没了」，
        // 恰恰砸在最需要分段上屏的场景上（一次输入长串连续部分上屏，是拼音自动造词唯一
        // 走得通的路径：整串上屏会 `reset_pinyin_composition` 清空 `committed_segs`，
        // 而 `learn_phrase_on_commit` 要求 ≥2 段）。见 `tests/pinyin_abbrev_recall_window.rs`。
        let start = cands.len();

        // 部分候选统一形态：`is_abbrev` 归入简拼层、`is_partial` 表示只覆盖了输入前缀
        // （沉在完整匹配之后），`consumed_length` **自带击键域的消费数**——下方那个按
        // code/query 关系统一计算 consumed 的循环会跳过它们（见其注释）。
        let per_cut = if consumed >= 3 {
            self.config.abbrev_fallback.per_cut_long
        } else {
            self.config.abbrev_fallback.per_cut_short
        };
        let push = |cands: &mut Vec<Candidate>,
                    base: usize,
                    text: String,
                    code: String,
                    w: i32,
                    boundary: u64,
                    is_fuzzy: bool| {
            if cands.len() - base >= per_cut {
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
                is_fuzzy,
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
            for (key, edits) in self.abbrev_recall_keys(stroke) {
                // 模糊处数与折扣，口径同 step5（见那里的论证）。
                for abbr_code in dict.search_abbrev(&key, ABBREV_INDEX_LIMIT) {
                    for h in dict.search_with_boundary(&abbr_code) {
                        let eb = effective_boundary(&abbr_code, h.boundary, trie);
                        if eb != 0 && eb.count_ones() as usize != stroke.len() {
                            continue;
                        }
                        push(
                            cands,
                            start,
                            h.text,
                            abbr_code.clone(),
                            fuzzy_penalized(h.weight, edits),
                            h.boundary,
                            edits > 0,
                        );
                    }
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
            let mut keys: Vec<String> = pats
                .iter()
                .flat_map(|p| self.abbrev_recall_keys_only(p.key()))
                .collect();
            keys.sort_unstable();
            keys.dedup();
            for key in &keys {
                for abbr_code in dict.search_abbrev(key, ABBREV_INDEX_LIMIT) {
                    for h in dict.search_with_boundary(&abbr_code) {
                        let Some(syls) =
                            mixed_abbrev::syllables_from_boundary(&abbr_code, h.boundary)
                        else {
                            continue;
                        };
                        // 同一个键下常有多条解释命中，取**模糊处数最少**的那条计罚
                        // ——按最有利的解释算，与 `fuzzy_codes` 侧「变体自带处数」同口径。
                        // 不按 `p.key() == key` 预筛，理由同 step 5b。
                        let Some(edits) = pats
                            .iter()
                            .filter_map(|p| {
                                p.matches_with(&syls, |seg, syl| self.seg_matches_fuzzy(seg, syl))
                            })
                            .min()
                        else {
                            continue;
                        };
                        push(
                            cands,
                            start,
                            h.text,
                            abbr_code.clone(),
                            fuzzy_penalized(h.weight, edits),
                            h.boundary,
                            edits > 0,
                        );
                    }
                }
            }
        }

        // ③④ 用户/临时造词层的纯简拼与混合简拼（同 step6 末段）
        //
        // 经**声母索引**取候选，判据一字未动：索引只保证「声母投影对得上」，
        // 音节数、逐段全等仍在下面逐条判。见 `DictLayer::search_abbrev`。
        if let Some(store_dm) = &self.store_layers {
            // 用户/临时层自己的配额基准（见 `start` 处的说明）：不与系统层 ①② 抢同一份。
            let store_base = cands.len();
            for c in self.recall_store_by_abbrev(store_dm, stroke, plain, &pats) {
                let plain_edits = self.abbrev_matches_stroke(&c.code, c.boundary, stroke);
                // 旧代码 `if plain { None }` 的短路前提是「纯简拼命中恒精确 ⇒ 已是最小」。
                // 召回侧放宽后这个前提**只在 `Some(0)` 时还成立** —— 纯简拼也可能是变体键
                // 命中、带着处数，那时混合解释可能更优（处数更少），不能跳过不算。
                let mixed_edits = if plain_edits == Some(0) {
                    None
                } else {
                    mixed_abbrev::syllables_from_boundary(&c.code, c.boundary).and_then(|syls| {
                        pats.iter()
                            .filter_map(|p| {
                                p.matches_with(&syls, |seg, syl| self.seg_matches_fuzzy(seg, syl))
                            })
                            .min()
                    })
                };
                // 取**处数最少**的解释计罚，与 mod.rs 上方 step5b「同一个键下多条解释取 min」
                // 同口径 —— `plain` 与 `mixed` 就是同一个词的两种解释。
                // ⚠️ **不能用 `or`**：它在两边都有值时返回左边，取的是更差的那个。
                //
                // 全拼下 `plain_edits <= mixed_edits` 恒成立：两者同时命中 ⇒ 模式段数 ==
                // 击键长度 ⇒ 每段恰好 1 字母 ⇒ `Initial` 段与 plain 的键逐位走**同一条**
                // 判据；单字母 `Syllable` 段（只可能是 a/e/o）精确命中时 plain 该位亦为 0。
                // 故 min 在全拼下恒等于 `plain_edits`。
                //
                // **保留 min 是因为双拼 + 手动分隔符下这条证明不成立**：那时
                // `mixed_pattern_source` 取 `query`（转换后的全拼）而 plain 用 `abbr_query`
                // （击键），不同域也不同长，段与击键位不再一一对应，逐位论证失去前提。
                // 这不是「防御未知」，是不把一条**只在全拼域成立**的推理写进代码形状 ——
                // 文档 §5 约束 4 与「双拼下 query 已是转换结果」那条注释立的就是这条规矩。
                let Some(edits) = [plain_edits, mixed_edits].into_iter().flatten().min() else {
                    continue;
                };
                let w = fuzzy_penalized(c.weight, edits);
                // 同文已在（本切点或更早切点的系统词）：合并，不丢弃（见 merge_store_abbrev_hit）。
                if let Some(existing) = cands.iter_mut().find(|x| x.text == c.text) {
                    merge_store_abbrev_hit(existing, &c, w, edits > 0);
                    continue;
                }
                let before = cands.len();
                push(
                    cands,
                    store_base,
                    c.text.clone(),
                    c.code.clone(),
                    w,
                    c.boundary,
                    edits > 0,
                );
                // `push` 新建的候选不带来源：补上，与 step 6.1 新增的用户词同样可删、可辨。
                if let Some(pushed) = cands.get_mut(before) {
                    pushed.meta = c.meta;
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
        // 键与系统词侧同样要展开首字母模糊变体，否则用户词里的「篮球场」在 `nqc` 下召不回。
        let mut keys: Vec<String> = Vec::new();
        if plain {
            keys.extend(self.abbrev_recall_keys_only(stroke));
        }
        keys.extend(
            pats.iter()
                .flat_map(|p| self.abbrev_recall_keys_only(p.key())),
        );
        keys.sort_unstable();
        keys.dedup();

        let mut out: Vec<Candidate> = Vec::new();
        for key in &keys {
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
        // 双拼同键音节是**键位**的属性，全拼打法没有键位：本支路只吃用户开的模糊组。
        let fz = self.fuzzy_config.without_shared_key();
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
        //
        // `meta` 原样带进候选：系统词模糊命中的造词码（`learn_code` = 词典码 + 边界，同主路径
        // step 1/3），store 层命中的来源标记 / 存储码 / 造词码（同 step 6）。此前这里一律
        // `Default`，分步组词按模糊原码造词、用户词右键删不掉。
        let push = |cands: &mut Vec<Candidate>,
                    text: String,
                    code: String,
                    weight: i32,
                    order: i32,
                    boundary: u64,
                    is_prefix: bool,
                    consumed: usize,
                    is_fuzzy: bool,
                    meta: wind_candidate::CandidateMeta| {
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
                meta,
                ..Default::default()
            };
            if let Some(existing) = cands.iter_mut().find(|c| c.text == cand.text) {
                if existing.consumed_length != 0 && existing.consumed_length < cand.consumed_length
                {
                    *existing = cand;
                } else if cand.meta.is_user_dict || cand.meta.is_temp_dict {
                    // 后到的 store 命中整条让位，但来源标记与存储码并入先占位的那条
                    // （同主路径 step 6）：系统词先占位时，右键删除才找得到用户记录。
                    merge_store_origin_and_code(existing, &cand, cand.is_fuzzy);
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
        // 模糊命中带 `learn_code`（词典码 + 边界），同主路径 step 1：本支路的候选码是击键，
        // 而击键在这里就是全拼（同一个域），分步组词时造词侧拼的正是这份规范码。
        for h in self.lookup_with_fuzzy_in(&completed, syllables, false, &fz, false) {
            let c = completed.clone();
            let n = c.len();
            let meta = wind_candidate::CandidateMeta {
                learn_code: h.dict,
                ..Default::default()
            };
            push(
                cands, h.text, c, h.weight, h.order, h.boundary, false, n, h.is_fuzzy, meta,
            );
        }

        // ② 各级前缀子短语（`nihao` → `ni`），供分段上屏。段数上限同主路径 step3。
        if syllables.len() >= 2 {
            for end in 1..syllables.len().min(6) {
                let code: String = syllables[..end].concat();
                // 逐级限流，见 MAX_FULL_PINYIN_SUBPHRASE：单音节前缀的同音单字动辄 20+ 条，
                // 不限就会吃光整个配额，把 ① 精确整词和 ③ 前缀补全挤出去。
                for h in self
                    .lookup_with_fuzzy_in(&code, &syllables[..end], false, &fz, false)
                    .into_iter()
                    .take(MAX_FULL_PINYIN_SUBPHRASE)
                {
                    let c = code.clone();
                    let n = c.len();
                    let meta = wind_candidate::CandidateMeta {
                        learn_code: h.dict,
                        ..Default::default()
                    };
                    push(
                        cands, h.text, c, h.weight, h.order, h.boundary, false, n, h.is_fuzzy, meta,
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
        // 边界对齐判据同主路径 step 4（`completed` 是本支路已完成音节段，见 ①）：混输降级
        // 到全拼这条流一样不该把 `shen` 补成 `sheng`。⚠️ 这里前缀用的是**整串击键**
        // `stroke`（含残码），而对齐位取 `completed.len()`（不含残码），两者刻意不同。
        for h in dict.search_prefix_with_boundary_syllable_capped(
            stroke,
            MAX_FULL_PINYIN_RECALL,
            cap,
            completed_bytes,
        ) {
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
                Default::default(),
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
                    c.meta,
                );
            }
            // 模糊音同 ①（t215）：用户词记录的是规范码，模糊打法只能靠变体码撞上。
            // 放在精确那路之后，同文时 `push` 的去重让精确那条占位。
            for c in self.search_store_fuzzy(
                store_dm,
                &completed,
                syllables,
                false,
                MAX_FULL_PINYIN_RECALL,
                &fz,
            ) {
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
                    true,
                    c.meta,
                );
            }
            for c in store_dm.search_prefix(stroke, MAX_FULL_PINYIN_RECALL) {
                // ★ 音节边界对齐必须**在本支路自己判一次**：`recall_full_pinyin` 被刻意放在
                // step 6.3 那道 retain **之后**（6.3 的尺子 `syllable_cap` 由双拼域音节数算出，
                // 拿来裁全拼候选是判据跨域复用），所以本支路的候选一条都过不了那道闸门。
                // 漏掉这里，用户词就会在这条路上跨越已打完的音节（`shaixu` 照旧出「筛选」），
                // 而系统词（③）却已经在词库层被挡住 —— 同一输入下两类词行为不一致。
                //
                // 判据函数与 ③ 同源，另加 `effective_boundary` 的 DAG 兜底：用户手输码常常
                // 没有边界真值（`boundary == 0`），词库层判不了、只能放行，这里补上。
                //
                // ⚠️ 只补**对齐**这一道，没有跟着加 `completion_syllable_cap` —— 本路径的
                // 用户词一直不受音节数上限约束（`store_dm.search_prefix` 无 cap 入参），那是
                // 既有取舍（用户自己加的长词理应比系统词更容易召回），与本次无关；要动它得
                // 单独立项并配回归，顺手加会静默改掉用户词的可达范围。
                if !wind_dict::cached::prefix_syllable_aligned(
                    effective_boundary(&c.code, c.boundary, &self.trie),
                    c.code.len(),
                    completed_bytes,
                ) {
                    continue;
                }
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
                    c.meta,
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
        //    **S2 的用户词节点同样不接**，判据与上面拒绝 2c 残码补全的是同一条：本支路是
        //    「双拼用户偶尔打一次全拼」的降级通道，产出恒沉在双拼候选之后，而用户词进图会
        //    改变整句这唯一一条结果（没有 N-best 兜底）。收益极小、却要多维护一处判据面。
        //    真要在双拼下用自造词组句，正途是那个方案本身的 step 2（用户词的 code 就是全拼，
        //    双拼主路径转换后同域），不是这条降级支路。
        if self.config.use_smart_compose && syllables.len() >= 2 {
            let trie = &self.trie;
            let seg_graph = SegGraph::from_dag(&Dag::build(&completed, trie));
            // 模糊音同 ①②：与主路径 step 2 一致地把 fuzzy_config 传进词图。
            let mut lattice_nodes =
                self.lattice_builder
                    .build(&completed, &seg_graph, dict, Some(&fz), true);
            self.drop_blocked_nodes(&mut lattice_nodes);
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
                        canon: node.canon.clone(),
                        aux: 0,
                    });
                }
            }
            let result = self.decode_unblocked(&lattice, input_len);
            let sentence: String = result.words.join("");
            // 按词频标定的阈值只看节点分，见 `ViterbiResult::lexical_log_prob`
            let logp_per_char = result.lexical_log_prob / sentence.chars().count().max(1) as f64;
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
                    // 模糊整句的造词码（规范码），全精确时为 None。见 `ViterbiResult::learn_code`。
                    meta: wind_candidate::CandidateMeta {
                        learn_code: result.learn_code(&completed),
                        ..Default::default()
                    },
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
        let trie = &self.trie;
        for c in &mut cands[start..] {
            if !c.is_prefix {
                continue;
            }
            // 口径同主路径 step4/step6：`boundary == 0` 先过 `effective_boundary` 现切，
            // 而不是当「算不出」跳过 —— 用户词里手输码条目恒 boundary=0，直接跳过等于
            // 这批词的档位永远是 0，与 4 音节真值词同档竞争。
            let word_syls = effective_boundary(&c.code, c.boundary, trie).count_ones();
            if word_syls == 0 {
                // 一个音节都切不出（空码 / 整串非拼音）：确实算不出，保持默认 0。
                // 码切不满的那类拿到的是低估值而非 0，走不到这里（见 `effective_boundary`）。
                continue;
            }
            c.completion_extra_syllables =
                word_syls.saturating_sub(started).min(u8::MAX as u32) as u8;
            if !c.is_promoted_completion
                && user_prefix_texts.contains(&c.text)
                && should_promote_user_completion(
                    syllables.len(),
                    trailing_partial,
                    word_syls, // 上一行档位刚算过，复用
                    self.config.completion_max_extra_syllables,
                )
            {
                c.is_promoted_completion = true;
            }
        }
    }

    /// 混合简拼段校验的音节比较：精确相等返回 `Some(0)`，模糊变体命中返回
    /// `Some(改动处数)`，不匹配返回 `None`。
    ///
    /// `seg` 是**用户敲的那一段**、`syl` 是**词典里的音节**，方向不可颠倒 —— 与
    /// [`Self::lookup_with_fuzzy`] 的 `fuzzy_codes` 同向（对输入扩展去撞词典）。
    ///
    /// **返回处数而不是 `bool`**：调用方要拿它施加 [`fuzzy_penalized`] 折扣并标 `is_fuzzy`。
    /// 「模糊命中恒低精确命中一档」是全仓不变量（见 [`FUZZY_WEIGHT_SCALE`] 那段论证，
    /// 词图侧还有同轴的 `FUZZY_SYLLABLE_LOG_PENALTY`），本路径不是例外 —— 若这里不罚，
    /// 同一个声母投影键下一条**精确**命中的词会被权重更高的模糊命中压过去。
    ///
    /// ⚠️ **放宽只到段校验为止**：`mixed_abbrev::render_keystroke_preedit` 用
    /// `raw[pos..].starts_with(syl)` 精确对齐击键与词典音节，模糊命中必然对不上 ⇒ 返回
    /// `None`，于是这类候选拿不到击键分段 preedit（`shengrikl` 显示 `sheng'ri'k'l`，
    /// `senrikl` 只能退回通用切分 `sen'ri'kl`）。失败是安全的（不显示错误分段），记为已知边界。
    ///
    /// 只有 `Syllable` 段经过这里。`Initial` 段比的是首字母，其宽松程度由
    /// [`mixed_abbrev::MixedPattern::matches_with`] 的 `starts_with` 决定 —— 注意那只对
    /// **共享首字母**的声母组（`sh↔s`、`zh↔z`、`ch↔c`）等价于模糊放宽；`n↔l`、`f↔h`、
    /// `r↔l` 三组首字母不同，声母段仍是严格的，属已知限制。
    ///
    /// **开销**：段校验跑在 `keys × abbr_code × hit` 的三重循环里，而扫描**非目标**词条时
    /// 失配才是常态 —— 即「大多数段精确相等所以不展开」并不成立，几乎每条待校验候选都会
    /// 触发一次变体展开。实测最坏形状（`srkl` 键下 64 码 × 8 词 = 512 条待校验，
    /// sh_s + en_eng）：段校验净增约 310 µs，合 0.6 µs/条，随该键下条目数线性增长。
    /// 单次按键 0.3 ms 仍在预算内，但**可以消掉**：`Syllable` 段最多 `MAX_SEGMENTS` 个、
    /// 模式最多 `MAX_PATTERNS` 条，把变体预算进 `MixedPattern` 或在 `convert` 开头建一张
    /// 表，512 次展开可塌成 ≤6 次。尚未做。
    ///
    /// `any_enabled()` 那道短路是有效的：模糊音全关时整条链路一次都不执行。
    /// 简拼键的**召回集**：原键 + 首字母模糊变体（`n↔l` / `f↔h` / `r↔l`）。
    ///
    /// 简拼索引的键是各音节首字母拼成的串，改首字母的模糊组因此在**召回侧**就断了：
    /// 用户打 `nqc`，「篮球场」挂在 `lqc` 下，`search_abbrev` 一条都捞不到，后面的逐段
    /// 校验根本没机会表态。所以 `Initial` 段的模糊与 `Syllable` 段的模糊**是同一个缺口
    /// 的两面**，必须召回侧枚举键 + 校验侧放宽 `Initial` 一起做，缺一条都不出结果。
    ///
    /// `sh↔s` / `zh↔z` / `ch↔c` 在这里是恒等变换（首字母同为 s/z/c），返回单元素 —— 这
    /// 正是它们一直能用而另外三组不能的原因。模糊音全关时同样返回单元素，零额外点查。
    ///
    /// 变体键与原键**等长**，故调用方按键长度做的音节数过滤（`eb.count_ones() == key.len()`）
    /// 不受影响。上限与截断策略见 [`fuzzy::fuzzy_abbrev_keys`]。
    ///
    /// ## 用了它的五条路，与**没用它的第六条**
    ///
    /// 已接：step5 纯简拼、step5b 混合、step6.2 前缀回退的纯简拼与混合、
    /// `recall_store_by_abbrev`（用户/临时词层）。
    ///
    /// ⚠️ **整句词图的简拼节点没接**：`lattice::add_abbrev_nodes` 里那处
    /// `dict.search_abbrev(stroke, ABBREV_NODE_LIMIT)` 仍按精确键查，于是
    /// `nqchaobuhao` 这类「混合整句」在开 `n_l` 后仍召不回 `lqc` 的节点。
    /// 不是疏漏而是**范围外**：那条路的候选走 `log_prob` 与 `ABBREV_NODE_PENALTY`，
    /// 模糊罚分要并进词图打分轴（与 `FUZZY_SYLLABLE_LOG_PENALTY` 同轴），
    /// 不是在这里乘个 `fuzzy_penalized` 就完事的，属另一笔改动。
    /// `Lattice::build` 本就接 `fuzzy_config`（全拼节点支持模糊、简拼节点不支持），
    /// 这个不对称在本次改动后变成「整串支持、前缀回退支持、整句不支持」。
    /// ⚠️ 只给**纯简拼**路径与 [`Self::abbrev_matches_stroke`] 用。混合路径走
    /// [`Self::abbrev_recall_keys_only`]，那里有两类的分工表与用错的后果。
    fn abbrev_recall_keys(&self, key: &str) -> Vec<(String, usize)> {
        fuzzy::fuzzy_abbrev_keys(key, &self.fuzzy_config)
    }

    /// 同 [`Self::abbrev_recall_keys`]，只要键、丢掉处数。**刻意实现成前者的投影**
    /// 而非另写一份枚举：两份枚举日后改了一个忘了另一个，召回面就会分叉 ——
    /// 表现为「某个词在 `nanqc` 下出不来但 `nqc` 下出得来」，看着像模式枚举的毛病，
    /// 排查会走错方向。比罚分错更隐蔽。
    ///
    /// ## 五个召回点分两类，用错哪一类都**不会报错**
    ///
    /// | 路径 | 该用 | 用错的后果 |
    /// |---|---|---|
    /// | step5 纯简拼、step6.2 前缀回退① | [`Self::abbrev_recall_keys`] | 漏罚，退回「模糊解排到精确解前面」 |
    /// | step5b 混合、前缀回退②、`recall_store_by_abbrev` | 本函数 | 罚两遍 |
    ///
    /// **混合路径丢掉键处数不会漏罚**：键第 i 位变了 ⇒ 词第 i 个音节首字母变了 ⇒
    /// 该段的 `matches_exact` 必然失败（`Initial` 比首字母、`Syllable` 比全等，都钉死首字母）
    /// ⇒ 校验侧必走模糊分支、必计处数。而校验侧**比键更准**：`Syllable` 段的
    /// `sen → sheng` 是 2 处（声母 + 韵母），键位只看首字母只能给 1。
    /// 取键处数反而罚轻，取两者之和才是罚两遍。
    ///
    /// ⚠️ [`Self::abbrev_matches_stroke`] 必须用**带处数**的那个（它要自己算出 plain 的
    /// 处数），别顺手写成本函数。
    fn abbrev_recall_keys_only(&self, key: &str) -> Vec<String> {
        self.abbrev_recall_keys(key)
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    }

    /// 词条的声母串与用户敲的简拼 `stroke` 的匹配**处数**：`Some(0)` = 精确，
    /// `Some(n)` = 经 n 处首字母模糊匹配上，`None` = 不匹配。
    ///
    /// 取代 `abbrev_of_code(..) == Some(stroke)` 的精确比较：召回侧放宽后，`lqc` 下的词
    /// 会经 `nqc` 的变体键被捞回来，这里再按老判据比一次就又把它扔了。
    ///
    /// **返回处数而不是 `bool`**：调用方要拿它施加 [`fuzzy_penalized`] 折扣并标 `is_fuzzy`。
    /// 此前返回 `bool`，调用方那句 `plain ⇒ edits 为 0 ⇒ 不罚` 便把模糊命中当精确命中送进
    /// 候选 —— 而用户词权重普遍高于系统词，压过精确解比系统词侧更容易。
    fn abbrev_matches_stroke(&self, code: &str, boundary: u64, stroke: &str) -> Option<usize> {
        let ab = self.abbrev_of_code(code, boundary)?;
        if ab == stroke {
            return Some(0);
        }
        if !self.fuzzy_config.any_enabled() {
            return None;
        }
        // 处数取构造侧给的那一份，不在这里重算（口径统一见 `fuzzy_abbrev_keys`）。
        self.abbrev_recall_keys(stroke)
            .into_iter()
            .find(|(k, _)| *k == ab)
            .map(|(_, edits)| edits)
    }

    fn seg_matches_fuzzy(&self, seg: &mixed_abbrev::AbbrevSeg, syl: &str) -> Option<usize> {
        use mixed_abbrev::AbbrevSeg;
        if seg.matches_exact(syl) {
            return Some(0);
        }
        if !self.fuzzy_config.any_enabled() {
            return None;
        }
        match seg {
            // 声母段只约束首字母 ⇒ 判据是首字母的模糊等价集，计 **1 处**改动。
            // 不能套 `fuzzy_variants_scored`：那个要完整音节，而这里只有一个字母。
            //
            // 「1 处」与 `Syllable` 段**同量纲**：那边 `sen → sheng` 计 2（声母 + 韵母各一），
            // 而声母段本就只表达了一个声母、最多错一处，给 1 恰好是它能错的全部。
            // 两者相加后一起喂 `fuzzy_penalized`，故量纲必须一致。
            AbbrevSeg::Initial(c) => {
                let first = syl.chars().next()?;
                fuzzy::initials_fuzzy_equal(*c, first, &self.fuzzy_config).then_some(1)
            }
            // 双字母声母段：精确语义是「以 zh 开头」，走到这里说明音节**不**以 zh 开头。
            // 放宽成「只比首字母」仅在对应的卷舌组（zh_z / ch_c / sh_s）开着时成立 ——
            // 用 `initials_fuzzy_equal` 会因 zh 与 z 首字母相同而恒真，等于把这个段降级
            // 回单字母声母、开关形同虚设。同样计 **1 处**，与 `Initial` 段同量纲。
            AbbrevSeg::Retroflex(c) => {
                let first = syl.chars().next()?;
                (first == *c && fuzzy::retroflex_relaxed(*c, &self.fuzzy_config)).then_some(1)
            }
            AbbrevSeg::Syllable(s) => {
                fuzzy::FuzzyMatcher::fuzzy_variants_scored(s, &self.fuzzy_config)
                    .into_iter()
                    .find(|(variant, _)| variant == syl)
                    .map(|(_, edits)| edits)
            }
        }
    }

    /// 带模糊拼音扩展的词库查找（对齐 Go lookupWithFuzzy）。
    /// `code` 为待查询的全拼码（整串或前缀子码）；`syllables` 为该码对应的音节切分，
    /// `fixed` 说它是不是真值切分（见 [`Self::fuzzy_codes`]）。
    /// 返回与 `dict.search` 相同的 `(text, weight, order)`。
    ///
    /// fuzzy 全 false 时 fuzzy_codes 返回空 → 天然退化为纯 `dict.search`（无需 enabled 判断）。
    /// 返回 `(text, weight, order, is_fuzzy)`：原 code 精确命中 is_fuzzy=false；
    /// 模糊变体命中 is_fuzzy=true、按处数打折扣；双拼同键音节（0 处）命中不罚也不标。
    fn lookup_with_fuzzy(&self, code: &str, syllables: &[String], fixed: bool) -> Vec<LookupHit> {
        self.lookup_with_fuzzy_in(code, syllables, fixed, &self.fuzzy_config, false)
    }

    /// 换位纠错召回：同 [`Self::lookup_with_fuzzy`]，但变体展开走纠错边。
    fn lookup_corrected(&self, code: &str, syllables: &[String]) -> Vec<LookupHit> {
        self.lookup_with_fuzzy_in(code, syllables, false, &self.fuzzy_config, true)
    }

    /// [`Self::lookup_with_fuzzy`] 的本体，模糊配置由调用方给：全拼降级支路传去掉双拼同键
    /// 音节的那份（[`FuzzyConfig::without_shared_key`]）。
    fn lookup_with_fuzzy_in(
        &self,
        code: &str,
        syllables: &[String],
        fixed: bool,
        fz: &FuzzyConfig,
        with_correction: bool,
    ) -> Vec<LookupHit> {
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
                typed_boundary: 0,
                // 精确命中：候选码即词典码，无需另带。
                dict: None,
            })
            .collect();
        let mut seen: std::collections::HashSet<String> =
            results.iter().map(|h| h.text.clone()).collect();

        // 模糊变体命中对外一律 boundary=0（不设防）：词典给的是**变体码**（如 zhongguo）的切分，
        // 而候选对外的 code 是用户实际输入的原码（zongguo）——两者不同域，位偏移对不上，
        // 直接采信会错位误杀。词典边界只拿来和同域的变体边界核对（[`fuzzy_shape_edits`]）。
        let fuzzy = self.fuzzy_codes(code, syllables, fixed, fz, with_correction, &|v| {
            self.dict.has_longer_code(v)
        });
        for (variant, shapes) in fuzzy {
            // 走 `search_with_boundary` 而非 `search`：变体码与它自己的边界同域，
            // 那份真值正是造词要的（候选对外仍报 boundary=0，见上）。
            for h in self.dict.search_with_boundary(&variant) {
                let Some((edits, typed_boundary)) = fuzzy_shape_edits(&shapes, h.boundary) else {
                    continue;
                };
                if seen.insert(h.text.clone()) {
                    results.push(LookupHit {
                        text: h.text,
                        // 按变体自带的改动处数罚：单音节也可能声母、韵母同时模糊
                        // （`sen`→`sheng` 计 2 处），多音节按各音节处数累加。
                        weight: fuzzy_penalized(h.weight, edits),
                        order: h.order,
                        // 双拼同键音节计 0 处：不罚、也不标模糊（各处「非模糊」准入照常放行）。
                        is_fuzzy: edits > 0,
                        boundary: 0,
                        typed_boundary,
                        dict: Some((variant.clone(), h.boundary)),
                    });
                }
            }
        }

        results
    }

    /// `code` 的全部模糊变体码，各附它的形态 `(变体边界, 所打码边界, 改动处数)`，**不含** `code` 本身；
    /// 按最少改动处数、再按码排序（同文去重时先到者占位，改动少的先到）。
    ///
    /// 系统词库（[`Self::lookup_with_fuzzy`]）与用户/临时造词层（[`Self::search_store_fuzzy`]）
    /// 共用这一份判据 —— 两处各展开一遍的话，迟早一边多认一种变体、一边少认。展开本身与整句
    /// 词图同一份（[`LatticeBuilder::fuzzy_spans`]）：全拼按 `code` 的**全部**切分展开
    /// （`jinan` 是 `ji|nan` 也是 `jin|an`，静安只在后一条上），`fixed`（双拼 / 手动分隔符）时
    /// 只按真值切分 `syllables`。`can_extend` 同那边：系统词传词典的前缀存在性判据，用户层传恒真。
    ///
    /// 一个形态的变体边界与词典 / store 的 `boundary` 同域，命中时用 [`fuzzy_shape_edits`] 核对。
    /// 模糊音全关时返回空，连切分都不求。
    fn fuzzy_codes(
        &self,
        code: &str,
        syllables: &[String],
        fixed: bool,
        fz: &FuzzyConfig,
        // 走不走换位纠错边：只有纠错召回步传 true（见 `SegGraph::from_dag`）。
        with_correction: bool,
        can_extend: &dyn Fn(&str) -> bool,
    ) -> Vec<(String, Vec<FuzzyCodeShape>)> {
        if !fz.any_enabled() {
            return Vec::new();
        }
        let graph = if fixed {
            SegGraph::from_syllables(syllables)
        } else if with_correction {
            SegGraph::from_dag_with_correction(&Dag::build(code, &self.trie))
        } else {
            SegGraph::from_dag(&Dag::build(code, &self.trie))
        };
        let mut out: Vec<(String, Vec<FuzzyCodeShape>)> = self
            .lattice_builder
            .fuzzy_spans(
                0,
                code,
                &graph,
                fz,
                &mut lattice::EdgeVariants::new(),
                code.len(),
                // 整码不受词图的 `max_word_len` 限制：超过 10 音节的词典整词照样要有模糊召回。
                code.len(),
                can_extend,
            )
            .into_iter()
            .filter(|((q, _), _)| *q == code.len())
            .map(|((_, variant), shapes)| {
                let shapes = shapes.into_iter().map(|(mask, offsets, edits)| {
                    let typed = offsets
                        .iter()
                        .filter(|&&o| o < 64)
                        .fold(0u64, |m, &o| m | 1u64 << o);
                    (mask, typed, edits)
                });
                (variant, shapes.collect())
            })
            .collect();
        out.sort_by_cached_key(|(variant, shapes)| {
            let edits = shapes.iter().map(|(_, _, e)| *e).min();
            (edits, variant.clone())
        });
        out
    }

    /// 用户/临时造词层的模糊音召回：拿 `code` 的模糊变体码逐个查 store（t215）。
    ///
    /// 造词写库用的是**规范读音码**（`learn_code`），故用户按模糊打法敲出来的串在 store 里
    /// 永远查不到原码 —— 必须像系统词库那样对输入展开变体去撞。模糊音全关时不查任何东西。
    ///
    /// 返回的候选已按系统词模糊命中的口径改写（同 [`LookupHit::dict`] 的理由）：
    /// - `code` 换成用户敲的 `code`（`consumed_length` / 分步上屏 / 词频记账绑在它上面），
    ///   `boundary` 置 0（记录的边界在变体码域，与原码位偏移对不上）；
    /// - 记录自身的码与边界放进 `meta.store_code`（右键删除按它拼 key）与
    ///   `meta.learn_code`（造词写库，写回规范码而不是模糊原码）；
    /// - `weight` 按改动处数打 [`fuzzy_penalized`] 折扣并标 `is_fuzzy`。
    fn search_store_fuzzy(
        &self,
        store_dm: &DictManager,
        code: &str,
        syllables: &[String],
        fixed: bool,
        limit: usize,
        fz: &FuzzyConfig,
    ) -> Vec<Candidate> {
        let mut out = Vec::new();
        for (variant, shapes) in self.fuzzy_codes(code, syllables, fixed, fz, false, &|_| true) {
            for mut c in store_dm.search(&variant, limit) {
                // 手输码的记录没有边界：按码现切一次再核对，否则任何切法都能借到它。
                let boundary = effective_boundary(&variant, c.boundary, &self.trie);
                let Some((edits, typed_boundary)) = fuzzy_shape_edits(&shapes, boundary) else {
                    continue;
                };
                c.meta.fuzzy_typed_boundary = typed_boundary;
                c.meta.store_code = Some(c.code.as_str().into());
                c.meta.learn_code = Some((std::mem::take(&mut c.code), c.boundary));
                c.code = code.to_string();
                c.boundary = 0;
                c.weight = fuzzy_penalized(c.weight, edits);
                c.is_fuzzy = edits > 0;
                out.push(c);
            }
        }
        out
    }

    /// 给候选填正确拼音提示（`meta.pinyin_hints`，模糊与纠错来源，再按误读表覆盖）并判 `is_corrected`；
    /// `hint` 关且无纠错规则时调用方不进来。
    ///
    /// `is_corrected` 判在最终候选上：同文合并后留下的那条若有非纠错来源（精确命中无
    /// `learn_code`、或读音对齐得上），就不算经纠错。
    ///
    /// 在 `convert` 收尾对**最终**候选统一做一次，而不是散在各产出点：词库读音只认
    /// `learn_code`，各路径的同文合并（替换 / 继承 `learn_code`）都已落定，提示与
    /// `learn_code` 因此天然出自同一条，不必在每个合并点各维护一遍。
    ///
    /// 用户敲的串（`typed`）按候选所在的域取：
    /// - 全拼降级支路：`code` 是击键、即全拼；末尾判据对整串击键。
    /// - 双拼主路径：`code` 是 `query` 的前缀（转换后的全拼），逐音节换回本义读音
    ///   （[`shuangpin::SylSpan::literal`]）。`code` 不是 `query` 前缀的（击键域的简拼候选）
    ///   无从还原，跳过。双拼两键即一个完整音节，不存在「末音节没打完」，末尾判据恒假
    ///   ——否则自然码 `dn` 读成的 `din` 会被当成 `ding` 的半截而漏标。
    /// - 全拼主路径：`code` 本身；末尾判据为 `code` 吃到 `query` 末尾。
    ///
    /// 词库读音取 `learn_code`；双拼对偶声母读出的候选没有它（转换后的全拼就是词典码），
    /// 用候选自身的码。只填提示、不动 `is_fuzzy`：`hint` 开关不得改变排序（D7）。
    fn fill_pinyin_hints(
        &self,
        cands: &mut [Candidate],
        query: &str,
        raw_input: &str,
        sp: Option<&shuangpin::SpConvertResult>,
    ) {
        for c in cands.iter_mut() {
            self.fill_inferred_hint(c, query, raw_input, sp);
            self.apply_misread_hint(c, query, sp);
        }
    }

    /// 自动推断的提示（模糊 / 纠错来源）与 `is_corrected`，见 [`Self::fill_pinyin_hints`]。
    fn fill_inferred_hint(
        &self,
        c: &mut Candidate,
        query: &str,
        raw_input: &str,
        sp: Option<&shuangpin::SpConvertResult>,
    ) {
        let sp_main = sp.filter(|_| !c.is_fullpinyin_fallback);
        // 连续辅助码的辅码候选：码已是解码路径的所打码（去辅码），与双拼转换结果不同域，直接拿它
        // 对齐（对偶声母的本义读音暂不换）。
        let aux_typed = sp_main.is_some() && c.meta.aux_keys.is_some();
        if let Some(sp) = sp_main.filter(|_| !aux_typed) {
            if c.code.is_empty() || !query.starts_with(c.code.as_str()) {
                return;
            }
            // 没有 `learn_code` 的（精确命中）只有换过本义读音才可能带提示：先零分配地判掉。
            if c.meta.learn_code.is_none()
                && !sp
                    .syllables
                    .iter()
                    .any(|s| s.literal.is_some() && s.fp_end <= c.code.len())
            {
                return;
            }
        } else if c.meta.learn_code.is_none() {
            return;
        }
        let (typed, ends_here) = match sp_main {
            Some(_) if aux_typed => (c.code.clone(), false),
            Some(sp) => (literal_typed(sp, query, c.code.len()), false),
            None if c.is_fullpinyin_fallback => {
                (normalized_typed(&c.code), c.code.len() == raw_input.len())
            }
            None => (normalized_typed(&c.code), c.code.len() == query.len()),
        };
        let (dict_code, dict_boundary) = match &c.meta.learn_code {
            Some((code, b)) => (code.as_str(), *b),
            None => (c.code.as_str(), c.boundary),
        };
        let mask = effective_boundary(dict_code, dict_boundary, &self.trie);
        let Some(syllables) = mixed_abbrev::syllables_from_boundary(dict_code, mask) else {
            return;
        };
        // 一字一音节；对不上（儿化、含英文的词条）整条不标——宁漏不错。
        if syllables.len() != c.text.chars().count() {
            return;
        }
        let a = hint_align::align(&typed, &syllables, &self.fuzzy_config, ends_here);
        c.is_corrected = a.corrected;
        if a.hints.is_empty() {
            return;
        }
        c.meta.pinyin_hints = a.hints;
        c.meta.hint_syllables = syllables.iter().map(|s| s.to_string()).collect();
    }

    /// 误读表优先（设计 §6）：文字 + 用户所打读音命中表项时，提示换成表里的（只标读错的字，
    /// `HintKind::Misread`）。误读召回的候选按表重标、不算纠错。
    fn apply_misread_hint(
        &self,
        c: &mut Candidate,
        query: &str,
        sp: Option<&shuangpin::SpConvertResult>,
    ) {
        let Some(table) = self.fuzzy_config.misread.as_deref() else {
            return;
        };
        // 简拼 / 前缀补全的码是词库全码、不是用户打的：`yishn` 带出的「一刹那」不算打了 `yishana`。
        if !table.has_text(&c.text) || c.is_abbrev || c.is_prefix {
            return;
        }
        let typed = match sp.filter(|_| !c.is_fullpinyin_fallback) {
            Some(_) if c.meta.aux_keys.is_some() => c.code.clone(),
            Some(sp) if query.starts_with(c.code.as_str()) => {
                literal_typed(sp, query, c.code.len())
            }
            Some(_) => return,
            None => normalized_typed(&c.code),
        };
        let Some(e) = table.lookup(&c.text, &[typed.as_str()]) else {
            return;
        };
        if c.is_misread_recall {
            c.is_corrected = false;
        }
        c.meta.pinyin_hints = misread_hints(e);
        c.meta.hint_syllables = e.correct.clone();
    }
}

/// 双拼：`query[..len]` 里各音节换成用户本义读音（[`shuangpin::SylSpan::literal`]），
/// 回写段与残码原样保留。`query` 是转换后的全拼（不含 `'`、无需 ü 归一化）。
fn literal_typed(sp: &shuangpin::SpConvertResult, query: &str, len: usize) -> String {
    let mut out = String::with_capacity(len + 2);
    let mut cursor = 0usize;
    for s in &sp.syllables {
        if s.fp_end > len {
            break;
        }
        out.push_str(&query[cursor..s.fp_start]);
        out.push_str(s.literal.as_deref().unwrap_or(&query[s.fp_start..s.fp_end]));
        cursor = s.fp_end;
    }
    out.push_str(&query[cursor..len]);
    out
}

/// 音节序列的边界掩码（bit i = 第 i 字节起一个音节，同 `Candidate::boundary`）。
fn misread_boundary(syllables: &[String]) -> u64 {
    let mut mask = 0u64;
    let mut at = 0usize;
    for s in syllables {
        if at >= 64 {
            return 0;
        }
        mask |= 1 << at;
        at += s.len();
    }
    mask
}

/// 误读条目的逐字提示：只标错误读音与正确读音不同的字。
fn misread_hints(e: &misread::MisreadEntry) -> Vec<wind_candidate::PinyinHint> {
    e.wrong
        .iter()
        .zip(&e.correct)
        .enumerate()
        .filter(|(_, (w, c))| w != c)
        .map(|(i, (_, c))| wind_candidate::PinyinHint {
            char_index: i as u16,
            correct: c.clone(),
            kind: wind_candidate::HintKind::Misread,
        })
        .collect()
}

/// 全拼域的候选码规范成 [`hint_align::align`] 要的形态：去 `'`、ü 归一化。
///
/// 防御性的：当前各路径交来的码已是这个形态（全拼主路径入口已归一、`completed` 与整句码
/// 不含 `'`；降级支路的闸门遇 `'` 即拒，且词库用 `lve` 拼写时 `lue` 击键在支路里查不到词），
/// 但对齐对拼写差一个字母就整条漏标，入口约定一变就静默失效，故在这里兜一次。
fn normalized_typed(code: &str) -> String {
    let stripped: std::borrow::Cow<str> = if code.contains('\'') {
        code.chars()
            .filter(|&ch| ch != '\'')
            .collect::<String>()
            .into()
    } else {
        code.into()
    };
    spelling::normalize_u_umlaut(&stripped).into_owned()
}

/// 变体码的一条命中（词条边界 `boundary`，与变体边界同域）该按哪个形态算：边界对上的形态里
/// 改动最少的那个；边界为 0（没有可用信息）时不设防、取全部形态里最少的。用户层的手输码
/// 记录先经 [`effective_boundary`] 现切，到这里才是 0 的只剩切不出的码。对不上任何形态返回 `None`
/// ——切法不同的词撞上了同一个码（`xi|an → xi|ang` 拼出的 `xiang` 不是单音节的「想」）。
///
/// 一并返回该形态的**所打码边界**（供 [`Candidate`] 的 `meta.fuzzy_typed_boundary`）；
/// 边界为 0 时不知道是哪个形态，给 0。
fn fuzzy_shape_edits(shapes: &[FuzzyCodeShape], boundary: u64) -> Option<(usize, u64)> {
    if boundary == 0 {
        return shapes.iter().map(|(_, _, edits)| (*edits, 0)).min();
    }
    shapes
        .iter()
        .filter(|(mask, _, _)| *mask == boundary)
        .map(|(_, typed, edits)| (*edits, *typed))
        .min()
}

/// 变体码的一个形态：(变体边界, 所打码边界, 改动处数)，见 [`PinyinEngine::fuzzy_codes`]。
type FuzzyCodeShape = (u64, u64, usize);

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
    /// 模糊命中走的那条**所打码**切分，见 `CandidateMeta::fuzzy_typed_boundary`；精确命中为 0。
    typed_boundary: u64,
    /// **词典里登记的那个码**与它自己的真值边界；`None` = 与候选 `code` 同一个（精确命中）。
    ///
    /// 模糊命中时两者不同：用户敲 `senri`，词典里的词是 `shengri`。候选对外的 `code`
    /// 必须留用户那份 —— `consumed_length` 的判据是 `query.starts_with(&c.code)`，换成
    /// 词典码会落到「消费整串」分支、分步上屏当场失效（preedit 跟随与词频记账同理）。
    ///
    /// 但**造词要的恰恰是词典那份**：拼出来的码得是用户下次真能打出来的。顺带解决
    /// boundary —— 模糊命中的 `boundary` 恒 0（与原码不同域、位偏移对不上），
    /// 而词典码与它自己的边界天然同域，真值可以直接用。
    dict: Option<(String, u64)>,
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

/// 一次 `convert` 里各候选编码栏切分的共享表：同一切分只渲染一次，候选间共用一份 `Arc`
/// （`pinan` 下几百个 `pi` 单字都是 `pi'nan`）。键 = (情形, 边界, 候选覆盖长度)。
#[derive(Default)]
struct PreeditSplitCache(Vec<((u8, u64, usize), Arc<str>)>);

impl PreeditSplitCache {
    fn get(&mut self, key: (u8, u64, usize), render: impl FnOnce() -> String) -> Arc<str> {
        if let Some((_, s)) = self.0.iter().find(|(k, _)| *k == key) {
            return s.clone();
        }
        let s: Arc<str> = render().into();
        self.0.push((key, s.clone()));
        s
    }
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
///
/// ⚠️⚠️ **码切不满时同样少算，而且一声不吭**。`Dag::maximum_match` 从最远可达位置回溯，
/// 码里混进非拼音串就只返回能切的那段前缀：`daboluoqq` 切成 `da|bo|luo`（3 音节），
/// 尾部 `qq` 被无声丢掉 —— 实测该词在 `dabo` 下拿到 `extra=1`（按 3 音节算）并上浮，
/// 而它本该是「音节数不可知」。命中面是 `dict import` 的脏数据、码表码误入 pinyin
/// schema、手输码打错。
///
/// 本函数**刻意不做覆盖校验**（对比：同文件的 `abbrev_of_code` 有
/// `consumed != code.len() → None` 这道闸）。加这道闸会同时改变全部调用点的行为
/// （召回门、step4/step6/6.7 档位、上浮判据），得单独立项配回归 —— 在那之前，
/// 调用方拿到的「音节数」对这类码是**低估值而非 0**，`word_syls == 0` 这个判据
/// 只挡得住空码和整串切不出的码。
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
    fn set_blocked_words(&self, words: Arc<HashSet<String>>) {
        // 按键路径上每次都会推：同一份只读锁比一次指针。
        if Arc::ptr_eq(&self.blocked_words(), &words) {
            return;
        }
        *self
            .blocked_words
            .write()
            .unwrap_or_else(|e| e.into_inner()) = words;
    }

    fn memory_parts(&self) -> Vec<crate::engine::MemPart> {
        self.char_pinyin_idx
            .get()
            .map(|idx| {
                let (bytes, allocs) = idx.heap_estimate();
                crate::engine::MemPart {
                    name: format!(
                        "CharPinyinIndex(字{}/读音{})",
                        idx.char_count(),
                        idx.total_readings()
                    ),
                    bytes,
                    allocs,
                }
            })
            .into_iter()
            .collect()
    }

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
        let abbrev_quota = !opts.no_abbrev_quota;
        // 击键图开关：选项或环境变量任一为真即开。环境变量只读一次，按键路径不反复取。
        static RAW_LATTICE_ENV: OnceLock<bool> = OnceLock::new();
        let use_raw = opts.sp_raw_lattice
            || *RAW_LATTICE_ENV
                .get_or_init(|| std::env::var("WIND_SP_RAW_LATTICE").is_ok_and(|v| v == "1"));
        // 未覆写时用引擎自身配置（纯拼音方案即走这条）。
        let allow_partial_final = opts
            .allow_partial_final
            .unwrap_or(self.config.enable_partial_final);
        let mut timer = ConvertStageTimer::new();
        if input.is_empty() {
            return Ok(ConvertResult::default());
        }

        // ü 拼写归一化（`xv` → `xu`、`nue` → `nve`，两条规则与安全性论证见 [`spelling`]）。
        //
        // 落在 `raw_input` **之前**，让归一化形态成为本次转换的唯一真相：全拼下
        // 「`raw_input == query`」是下游多处判据的前提（整句 preedit 的 `top.code == raw_input`、
        // 简拼基准串 `abbr_query`），只归一 query 会让它们对含 `v` 的串静默失效。
        // 两条规则都是**等长**替换，故 preedit 逐字节重建与 consumed_length 全不受影响；
        // 用户看到的编码栏是归一化形态（打 `xv` 显示 `xu`），与主流输入法一致。
        //
        // ⚠️ 仅全拼：双拼的 `v` 是布局键（小鹤 `v` = zh 声母），拿到这里会被换成 `u`、
        // 毁掉整个双拼输入；双拼路径的对端实现在 `shuangpin::normalize_pinyin`。
        let normalized = self
            .shuangpin
            .is_none()
            .then(|| spelling::normalize_u_umlaut(input));
        let input: &str = normalized.as_deref().unwrap_or(input);

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
        // 用户是否打了手动分隔符。下面几处判据要据此换域——击键串含 `'`，
        // 而音节、模式、长度比较全都活在不含 `'` 的域里。
        let has_manual_sep = raw_input.contains('\'');

        // ★ **剥掉分隔符**：`'` 是边界声明，不是待解释的字母。留着它，
        // `is_abbreviation` 一见非字母就判假 ⇒ `b'z'd`（用户明确写出三个声母）
        // 反而**一条候选都没有**，而同样意思的 `bzd` 好好地出「不知道」。
        //
        // 剥除后仍在**击键域**，约束 4（判据用击键不用 query）照旧成立：双拼下
        // `n'hc` 剥成 `nhc`，与不打分隔符时逐位相同，双拼纯简拼行为一字不动。
        let abbr_query_owned: String;
        let abbr_query: &str = if has_manual_sep {
            abbr_query_owned = raw_input.chars().filter(|&c| c != '\'').collect();
            &abbr_query_owned
        } else {
            raw_input
        };

        // 双拼激活时保留 SpConvertResult，以便后续用 map_consumed_length 回算消费键数。
        let sp_result: Option<shuangpin::SpConvertResult> =
            self.shuangpin.as_ref().map(|conv| conv.convert(input));
        // 击键图分支（开关开且双拼）：step 2 / 2c 改在击键坐标建图，整句自带击键域的消费数。
        let raw_edges: Option<Vec<shuangpin::RawEdge>> = if use_raw {
            self.shuangpin
                .as_ref()
                .map(|conv| conv.pair_edges(raw_input))
        } else {
            None
        };
        // 连续辅助码：击键图开着且辅码表非空时另取全偏移键对，建图时接辅码边。
        let aux_lookup = opts
            .aux_continuous
            .as_deref()
            .filter(|a| raw_edges.is_some() && !a.is_empty());
        let raw_edges_all: Option<Vec<shuangpin::RawEdge>> = aux_lookup
            .and(self.shuangpin.as_ref())
            .map(|conv| conv.pair_edges_all(raw_input));
        let raw_aux = aux_lookup
            .zip(raw_edges_all.as_deref())
            .map(|(lookup, edges_all)| raw_lattice::AuxInput {
                edges_all,
                lookup,
                tail_partial: sp_result
                    .as_ref()
                    .and_then(|r| r.partial_initial.as_deref()),
            });
        // 辅码整句沿解码路径读出的形态（辅码位置、编码栏）；没用辅码时 `None`。
        let aux_sep: &str = opts
            .aux_preedit_separator
            .as_deref()
            .unwrap_or(crate::engine::AUX_PREEDIT_SEPARATOR);
        let aux_path_of = |result: &viterbi::ViterbiResult, spans: &raw_lattice::AuxSpans| {
            let a = raw_aux?;
            raw_lattice::aux_path(result, raw_input, a.edges_all, spans, a.tail_partial)
                .filter(|p| !p.aux_keys.is_empty())
        };
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

        // 混合简拼（声母段 + 音节段）枚举模式用的串。**与 `abbr_query` 分家**：
        // 纯简拼判据问的是「这串击键是不是一串声母」，只有击键域答得了（双拼下 `xan`
        // 靠它才命中「西安宁」）；混合模式问的是「哪几段是音节」，那要在**音节存在的域**
        // 里问——`abbr_query` 含 `'`，模式枚举遇到它直接判假，整条混合路走不到。
        //
        // ★ 打了分隔符就换到 `query`，它恰好在两条路上都是「不含 `'` 的全拼域串」：
        // 全拼下是剥除 `'` 的查询串；双拼下 `'` 已被 `convert` 消化，`query == input`
        // 即转换后的 full_pinyin。故这一行同时修好两边，不必按引擎类型分叉。
        //
        // 没打分隔符时维持 `abbr_query` 原样：双拼既有行为（靠击键串碰巧在全拼域可读，
        // 如 `xanning` = x+an+ning）一字不动。
        let mixed_pattern_source: &str = if has_manual_sep { query } else { abbr_query };

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
            // 调用方指定的准入（`ConvertOptions::admit`）：**在这里判而不是让调用方事后
            // retain**，理由见该字段文档——上限施加在过滤之前，事后过滤只能在被截断过的
            // 那一段里筛。放在查重之后：不合格的候选连查重表都不该占位，而查重本身是
            // O(n²) 的成本大头。
            if let Some(f) = opts.admit.as_deref()
                && !f(&text)
            {
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
            // ★ 只收**从起点连续**的音节段。下面 `completed` 的契约是「从 0 开始的连续
            // 覆盖」，而手动分隔符第一次让**开头的段可以不是音节**（`n'hao` 的 `n` 是
            // 简拼声母）。照单全收会把不接在起点上的 `hao` 当成 completed，于是
            // `lookup_with_fuzzy("hao")` 让「好」「号」「薅」以**精确匹配**身份占满前排
            // ——它们根本没解释前半段，却把真正解释了整串的混合简拼候选压到一百多位。
            //
            // 段间只允许隔着分隔符；隔着成不了音节的字母就断在那里。
            let mut out: Vec<String> = Vec::new();
            let mut cursor = 0usize;
            for s in &sep_spans {
                if input[cursor..s.raw_start].bytes().any(|c| c != b'\'') {
                    break;
                }
                out.push(s.pinyin.clone());
                cursor = s.raw_end;
            }
            out
        } else {
            Dag::build(input, trie).maximum_match()
        };
        timer.mark("seg");

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
        //    残码存在时（nihaom）query 非合法音节序列，search(query) 为空；而变体展开只排除
        //    与查询码相同的「全原音节」组合，按 query 查的话 completed 那份精确匹配会当成模糊
        //    变体命中，被误标 is_fuzzy=true 沉底、遭 truncate 截断（bug①）。传 completed 后全原
        //    组合正确排除（精确匹配 is_fuzzy=false）；code 存 completed 使残码输入的
        //    consumed_length 只覆盖完成音节（nihao 消费 5 留 m 续输）。
        for h in self.lookup_with_fuzzy(completed, &syllables, fixed_segmentation) {
            // 词典规范码另走 `learn_code`，不覆盖候选的 `code`（理由见该字段文档）。
            // 与 `is_abbrev` 那几处同款：`push_unique` 的参数已经很长，附加信息走事后赋值，
            // 用 `before` 判断这次是否真的新增了候选（同文去重时不新增）。
            let learn = h.dict.clone();
            let typed = h.typed_boundary;
            let before = candidates.len();
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
            if candidates.len() > before {
                candidates[before].meta.learn_code = learn;
                candidates[before].meta.fuzzy_typed_boundary = typed;
            }
        }
        timer.mark("exact");

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
        // 整句 N-best 的池子，随结果带给协调器（见 `ConvertResult::sentence_pool`）。
        let mut sentence_pool: Vec<Candidate> = Vec::new();

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
            // ⚠️ 比的是 `query`：`completed_len` 数的是音节字节数，与 `query` 同源。
            // ⓘ 变异验证测不出与 `abbr_query.len()` 的差别（`abbr_query` 现已剥掉 `'`，
            // 全拼下两者相等；双拼下 `has_manual_sep` 时长度虽差一截，但下游的
            // 模式校验兜住了噪音）。仍按 `query` 写，是因为**跨域比长度本身是错的**，
            // 留一个碰巧无害的错误判据，下次有人依赖它时就不无害了。
            None => completed_len >= query.len(),
        };

        // 2. Viterbi 长句解码（>=2 音节，仅在完成音节前缀上跑；use_smart_compose=false 时跳过）
        // 击键图：`raw_step2` 是 (lattice, 终点击键数)；建不出（含终点没铺到末音节）时整句跳过，
        // 不回落旧图。底图 step 2 与 2c 共用、用到时才建：外层 `None` = 未建，`Some(None)` = 建不出。
        let mut raw_base: Option<Option<raw_lattice::RawBase>> = None;
        // 辅码模式：过了闸门比分的辅码整句 `(文字, 消费数, 是否过闸)`；排位在 6.5c 之后定（见 6.5d）。
        let mut aux_tail: Vec<(String, usize, bool)> = Vec::new();
        // 辅码模式：辅码字母与下一键可能拼成非法键对，P1 的两键音节链在第 1 个音节后就断了。
        // 这时整句改按击键图本身可达（`RawBase::sentence` / `tail_end`）来解，只出两字以上的。
        // 用了辅码的整句（各出处同此）码、边界、造词码与消费数一律按解码路径（`AuxPath::stamp`），
        // 不取 P1 的 `completed` / `syllables`——那里面混着辅码字母的读音。
        let aux_entry = raw_aux.is_some() && syllables.len() == 1;
        let raw_step2 = match &raw_edges {
            Some(edges) if self.config.use_smart_compose && (syllables.len() >= 2 || aux_entry) => {
                Some(
                    raw_base
                        .get_or_insert_with(|| self.raw_base_lattice(raw_input, edges, raw_aux))
                        .as_ref()
                        .and_then(raw_lattice::RawBase::sentence),
                )
            }
            _ => None,
        };
        if self.config.use_smart_compose
            && (syllables.len() >= 2 || (aux_entry && matches!(raw_step2, Some(Some(_)))))
            && !matches!(raw_step2, Some(None))
        {
            let (lattice, input_len, raw_consumed) = if let Some(Some((lattice, end))) = raw_step2 {
                (Cow::Borrowed(lattice), end, end)
            } else {
                // 切分图：全拼取 DAG 的全部路径；双拼/手动分隔符取真值链（行为与改造前一致）。
                let seg_graph = if fixed_segmentation {
                    SegGraph::from_syllables(&syllables)
                } else {
                    SegGraph::from_dag(&Dag::build(completed, trie))
                };
                let mut lattice_nodes = self.lattice_builder.build(
                    completed,
                    &seg_graph,
                    dict,
                    Some(&self.fuzzy_config),
                    true,
                );
                self.maybe_add_store_nodes(completed, &seg_graph, true, &mut lattice_nodes);
                self.drop_blocked_nodes(&mut lattice_nodes);
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
                            canon: node.canon.clone(),
                            aux: 0,
                        });
                    }
                }
                (Cow::Owned(lattice), input_len, 0)
            };
            // 整句 N-best（见 `Config::sentence_counts`）。出厂 (1, 1) 时 `decode_nbest`
            // 走的就是原来的单路径 `decode`，下面的循环只转一圈、只进 `rank == 1` 分支，
            // 与改动前逐位相同。
            let (show_n, pool_k) = self.config.sentence_counts();
            let mut seen: Vec<String> = Vec::new();
            let step2_t = crate::prof::span(crate::prof::Stage::Step2Decode);
            let mut results = self.viterbi.decode_nbest(&lattice, input_len, pool_k);
            drop(step2_t);
            // 整句恰为被屏蔽的文字（由更小的块拼成）：多取几条，剔掉它，首选仍覆盖整串。
            // 没撞上屏蔽词时不多解一次，循环与原来逐条相同。
            let blocked = self.blocked_words();
            if results.iter().any(|r| sentence_blocked(&blocked, &r.words)) {
                // 不看语法模型：挂了模型时 `decode_nbest` 只回 1 条，没有替补可取。
                results = self.viterbi.decode_nbest_lexical(
                    &lattice,
                    input_len,
                    pool_k + BLOCKED_SENTENCE_EXTRA,
                );
                results.retain(|r| !sentence_blocked(&blocked, &r.words));
            }
            let two_chars = |r: &viterbi::ViterbiResult| {
                r.words.iter().map(|w| w.chars().count()).sum::<usize>() >= 2
            };
            // 辅码模式：末槽整句用了辅码时同样比分（对手 = 同终点不用辅码的最优路径）。不过闸时对手
            // 排第 1（同关表），辅码整句另插在它之后（6.5d 压到末槽整句之下）。出厂不设闸：两份语料上
            // 假读法分差到 11（我拍了 → 我 + 辅码 pd + 了），正当辅码低到 0.1，设闸的代价大于收益。
            let mut aux_last_failed: Option<viterbi::ViterbiResult> = None;
            if raw_aux.is_some()
                && (cfg!(test) || raw_lattice::aux_se_gate_margin().is_some())
                && raw_consumed != 0
                && let Some(base) = raw_base.as_ref().and_then(Option::as_ref)
                && let Some(i) = results.iter().position(|r| {
                    !r.words.is_empty() && r.log_prob.is_finite() && (!aux_entry || two_chars(r))
                })
                && results[i].pieces.iter().flatten().any(|p| p.aux > 0)
            {
                let plain_lattice = {
                    let _t = crate::prof::span(crate::prof::Stage::GatePlainLattice);
                    base.plain_lattice()
                };
                let plain = {
                    let _t = crate::prof::span(crate::prof::Stage::GateDecode);
                    self.decode_unblocked(&plain_lattice, input_len)
                };
                let rival = !plain.words.is_empty() && plain.log_prob.is_finite();
                #[cfg(test)]
                raw_lattice::note_gate_gap(raw_lattice::GateGap {
                    sentence: results[i].words.concat(),
                    kind: 'e',
                    dangling: false,
                    score: Some(results[i].log_prob),
                    rival: rival.then_some(plain.log_prob),
                    chars: results[i].words.concat().chars().count(),
                    aux_letters: results[i]
                        .pieces
                        .iter()
                        .flatten()
                        .map(|p| p.aux as usize)
                        .sum(),
                });
                let plain_text = plain.words.concat();
                if let Some(m) = raw_lattice::aux_se_gate_margin()
                    && rival
                    && results[i].log_prob <= plain.log_prob + m
                    && plain_text != results[i].words.concat()
                {
                    aux_last_failed = Some(results.remove(i));
                    results.retain(|r| r.words.concat() != plain_text);
                    results.insert(0, plain);
                }
            }
            for result in results {
                if seen.len() >= pool_k {
                    break;
                }
                // 仅接受有限概率的完整路径：解码失败时 log_prob 为 NEG_INFINITY，
                // 不能把这种空/错误路径强插到首选位置。
                if result.words.is_empty() || !result.log_prob.is_finite() {
                    continue;
                }
                let sentence: String = result.words.join("");
                // `decode_nbest` 已按文本去重；这里再挡一次（beam 路径、或哈希碰撞）。
                if sentence.is_empty() || seen.contains(&sentence) {
                    continue;
                }
                let uses_aux = result.pieces.iter().flatten().any(|p| p.aux > 0);
                // 链断入口只为辅码开：没用辅码的整句（P1 本就解不出）不出。
                if aux_entry && (!two_chars(&result) || !uses_aux) {
                    continue;
                }
                let path = raw_base
                    .as_ref()
                    .and_then(Option::as_ref)
                    .filter(|_| uses_aux && raw_consumed != 0)
                    .and_then(|b| aux_path_of(&result, b.aux_spans()));
                seen.push(sentence.clone());
                let rank = seen.len() as u8;
                // 整句优先：给予高权重置顶（log_prob 为负，原 .max(1) 会被截断淘汰）。
                // clamp + saturating_add 防止超长低频句的 log_prob 溢出 i32 导致沉底/panic。
                let weight = sentence_weight(result.log_prob, result.words.len());
                // 整句的边界 = 解码器**实际选中**的那条路径（多路径下同一串输入可有多种切法，
                // 只有解码器知道走的是哪条）。回退到 maximum_match 仅用于解码器给不出边界的
                // 极端情形（超 64 字节）。
                //
                // 击键图的 `result.boundary` 是击键偏移与全拼掩码混拼的，不可用。整句走满真值键对，
                // 所打全拼码 `completed` 上的边界就是真值切分。
                let boundary = if raw_consumed != 0 {
                    syllables_boundary_mask(&syllables, completed.len())
                } else if result.boundary != 0 {
                    result.boundary
                } else {
                    syllables_boundary_mask(&syllables, completed.len())
                };
                #[cfg(test)]
                if uses_aux {
                    raw_lattice::note_aux_sentence(&sentence);
                }
                let aux_whole =
                    raw_consumed == raw_input.len() && uses_aux && raw_lattice::aux_whole_enabled();
                let merged =
                    if let Some(existing) = candidates.iter_mut().find(|c| c.text == sentence) {
                        // 整句与已有候选（如精确匹配 你好）同文：提升其权重置顶，
                        // 同时抹去 is_partial（step1 标了 true，但整句是完整解读并非子短语），
                        // 否则残码场景下 is_partial=true 会在排序时被 is_partial=false 的前缀补全
                        // （如「你好吗」）压下去——后者经 trailing_partial 优化也是 false。
                        //
                        // ⚠️ 备选（rank ≥ 2）只在「露得出来」时才合并：露不出来的只进池子，
                        // 不该去改一条普通候选的权重与身份。
                        if rank == 1 || rank as usize <= show_n {
                            existing.weight = existing.weight.max(weight);
                            existing.is_partial = false;
                            // 同文合并后它就是整句解本身，须继承整句身份，
                            // 否则 freq_rerank 会把它当普通候选而让别的整句锚定到它之上。
                            existing.is_sentence = true;
                            existing.sentence_rank = rank;
                            // 用了辅码且消费整串：整套换成整句的身份（消费数含辅码字母，
                            // 否则上屏后滞留缓冲；码与造词码去辅码）。
                            if aux_whole {
                                match &path {
                                    Some(p) => p.stamp(existing, raw_input, aux_sep),
                                    None => {
                                        existing.consumed_length = raw_consumed;
                                        existing.meta.learn_code = raw_lattice::raw_learn_code(
                                            &result,
                                            raw_input,
                                            &existing.code,
                                            existing.boundary,
                                        );
                                    }
                                }
                            }
                        }
                        true
                    } else {
                        false
                    };
                let mut fresh = Candidate {
                    text: sentence.clone(),
                    // 码为完成音节前缀（不含残码），使 consumed_length=completed_len，
                    // 整句上屏后残码留缓冲续输（你好m → 选你好留 m）。
                    code: completed.to_string(),
                    weight,
                    natural_order: 0,
                    source: CandidateSource::Pinyin,
                    is_sentence: true,
                    // 新建整句 = 引擎合成的解读，词库无此词条（同文合并那三处刻意不设）。
                    is_synthesized: !merged,
                    boundary,
                    sentence_rank: rank,
                    // 模糊整句的造词码（规范码），全精确时为 None。见 `ViterbiResult::learn_code`。
                    meta: wind_candidate::CandidateMeta {
                        learn_code: if raw_consumed != 0 {
                            raw_lattice::raw_learn_code(&result, raw_input, completed, boundary)
                        } else {
                            result.learn_code(completed)
                        },
                        ..Default::default()
                    },
                    // 击键图整句自带击键数，出口不再换算（0 = 旧路径，照旧回映射）。
                    consumed_length: raw_consumed,
                    ..Default::default()
                };
                if let Some(p) = &path {
                    p.stamp(&mut fresh, raw_input, aux_sep);
                }
                if pool_k > 1 {
                    sentence_pool.push(fresh.clone());
                }
                if merged {
                    continue;
                }
                if rank == 1 {
                    synthesized_sentence = Some(sentence);
                    candidates.insert(0, fresh);
                } else if rank as usize <= show_n {
                    // 备选紧跟在已插入的整句之后。位置在这里只是初值 —— 引擎随后按层级/
                    // 权重重排、协调器再按消费长度重排，真正把整句块连续摆到最前的是
                    // 协调器的 `place_sentence_block`（仅 `sentence_count > 1` 时生效）。
                    let at = candidates
                        .iter()
                        .position(|c| c.sentence_rank == 0)
                        .unwrap_or(candidates.len());
                    candidates.insert(at, fresh);
                }
            }
            // 辅码模式：末槽之后整串另可达（末尾是辅码或残码补全）时另解终点为整串的整句，排位同 2c。
            // 只出用了辅码的：没用时首选不该变（2c 照 P1 只解残码整句，补全节点只当辅码读法的对手）。
            // 比分闸门：要比同终点不用辅码的最优路径高出裕度才放行；末键在 P1 读法里落单、这里被当成
            // 1 位辅码（可能是下一音节的声母）的用 `aux_dangling_margin`，其余用 `aux_gate_margin`。
            // 放行的排到末槽整句之前、同文合并时取整串消费数；不放行的排在末槽整句之后，不改写同文候选。
            if let Some(base) = raw_base.as_ref().and_then(Option::as_ref)
                && let Some(n) = base.tail_end()
            {
                let result = {
                    let _t = crate::prof::span(crate::prof::Stage::SnDecode);
                    self.decode_unblocked(base.lattice(), n)
                };
                if !result.words.is_empty()
                    && result.log_prob.is_finite()
                    && let Some(pieces) = &result.pieces
                    && pieces.iter().any(|p| p.aux > 0)
                    && (!aux_entry || two_chars(&result))
                {
                    let sentence: String = result.words.join("");
                    #[cfg(test)]
                    raw_lattice::note_aux_sentence(&sentence);
                    let path = aux_path_of(&result, base.aux_spans());
                    // 落单：P1 的读法里末槽之后只剩一个字母（可带 `'`），这里把它当末字的 1 位辅码。
                    let dangling = base.sentence().is_some_and(|(_, e)| {
                        raw_input[e..n].bytes().filter(|&b| b != b'\'').count() == 1
                    }) && pieces
                        .last()
                        .zip(result.words.last())
                        .is_some_and(|(p, w)| base.trailing_aux_len(p.start, w) == 1);
                    let margin = if dangling {
                        raw_lattice::aux_dangling_margin()
                    } else {
                        Some(raw_lattice::aux_gate_margin(self.viterbi.grammar_weight()))
                    };
                    #[cfg(test)]
                    let gap = |score: Option<f64>, rival: Option<f64>| raw_lattice::GateGap {
                        sentence: sentence.clone(),
                        kind: 'n',
                        dangling,
                        score,
                        rival,
                        chars: sentence.chars().count(),
                        aux_letters: pieces.iter().map(|p| p.aux as usize).sum(),
                    };
                    #[cfg(test)]
                    if margin.is_none() {
                        raw_lattice::note_gate_gap(gap(None, None));
                    }
                    let pass = margin.is_some_and(|m| {
                        let plain_lattice = {
                            let _t = crate::prof::span(crate::prof::Stage::GatePlainLattice);
                            base.plain_lattice()
                        };
                        let plain = {
                            let _t = crate::prof::span(crate::prof::Stage::GateDecode);
                            self.decode_unblocked(&plain_lattice, n)
                        };
                        let rival = !plain.words.is_empty() && plain.log_prob.is_finite();
                        #[cfg(test)]
                        raw_lattice::note_gate_gap(gap(
                            Some(result.log_prob),
                            rival.then_some(plain.log_prob),
                        ));
                        !rival || result.log_prob > plain.log_prob + m
                    });
                    let weight = sentence_weight(result.log_prob, result.words.len());
                    let boundary = syllables_boundary_mask(&syllables, completed.len());
                    // 末槽整句（step 2 首位）在不在：被屏蔽词滤光时没有排位基准，不放行的就不出。
                    let has_top = candidates.iter().any(is_step2_top);
                    // 同文合并按文字找（与 step 2 合并同口径，Task 3 裁决：并入同文候选、取整串消费数）。
                    if let Some(existing) = candidates.iter_mut().find(|c| c.text == sentence) {
                        if pass {
                            existing.weight = existing.weight.max(weight);
                            existing.is_partial = false;
                            existing.is_sentence = true;
                            if raw_lattice::aux_whole_enabled() {
                                match &path {
                                    Some(p) => p.stamp(existing, raw_input, aux_sep),
                                    None => {
                                        existing.consumed_length = n;
                                        existing.meta.learn_code = raw_lattice::raw_learn_code(
                                            &result,
                                            raw_input,
                                            &existing.code,
                                            existing.boundary,
                                        );
                                    }
                                }
                            }
                            aux_tail.push((sentence, n, true));
                        }
                    } else if pass || has_top {
                        let mut c = Candidate {
                            text: sentence.clone(),
                            code: completed.to_string(),
                            weight,
                            natural_order: 0,
                            source: CandidateSource::Pinyin,
                            is_sentence: true,
                            is_synthesized: true,
                            boundary,
                            meta: wind_candidate::CandidateMeta {
                                learn_code: raw_lattice::raw_learn_code(
                                    &result, raw_input, completed, boundary,
                                ),
                                ..Default::default()
                            },
                            consumed_length: n,
                            ..Default::default()
                        };
                        if let Some(p) = &path {
                            p.stamp(&mut c, raw_input, aux_sep);
                        }
                        candidates.insert(0, c);
                        aux_tail.push((sentence, n, pass));
                    }
                }
            }
            // 不过闸的末槽辅码整句：排在末槽整句之后，同文时不碰已有候选（同上面 S_n 不放行）。
            if let Some(result) = aux_last_failed {
                let sentence = result.words.concat();
                if !candidates.iter().any(|c| c.text == sentence)
                    && candidates.iter().any(is_step2_top)
                {
                    let boundary = syllables_boundary_mask(&syllables, completed.len());
                    let mut c = Candidate {
                        text: sentence.clone(),
                        code: completed.to_string(),
                        weight: sentence_weight(result.log_prob, result.words.len()),
                        natural_order: 0,
                        source: CandidateSource::Pinyin,
                        is_sentence: true,
                        is_synthesized: true,
                        boundary,
                        meta: wind_candidate::CandidateMeta {
                            learn_code: raw_lattice::raw_learn_code(
                                &result, raw_input, completed, boundary,
                            ),
                            ..Default::default()
                        },
                        consumed_length: input_len,
                        ..Default::default()
                    };
                    if let Some(p) = raw_base
                        .as_ref()
                        .and_then(Option::as_ref)
                        .and_then(|b| aux_path_of(&result, b.aux_spans()))
                    {
                        p.stamp(&mut c, raw_input, aux_sep);
                    }
                    candidates.insert(0, c);
                    aux_tail.push((sentence, input_len, false));
                }
            }
        }

        timer.mark("sent");

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
        //     双拼**并不跳过**：门条件不排除双拼（`query` 含残码声母），旧图在转换后的全拼上
        //     用 DAG 重切，码与消费数再经 `map_consumed_length` 换回击键；开关打开时改走击键图。
        //     分隔符跳过（`!has_sep`）：`completed` 由音节
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
        // (整句文本, 权重, 所打码边界, 造词码, 辅码整句的路径形态)
        type PendingSentence = (
            String,
            i32,
            u64,
            Option<(String, u64)>,
            Option<raw_lattice::AuxPath>,
        );
        let mut short_sentence_pending: Option<PendingSentence> = None;
        // 击键图：在真值键对后补残码节点。消费数（0 = 旧路径）在 6.5c 延迟插入时同样带上。
        let mut raw_partial_consumed = 0usize;
        let mut raw_partial_spans = raw_lattice::AuxSpans::default();
        let raw_step2c = match (&raw_edges, &sp_result) {
            (Some(edges), Some(r))
                if self.config.use_smart_compose
                    && allow_partial_final
                    && !syllables.is_empty()
                    && completed_len < query.len()
                    && !has_sep =>
            {
                Some(r.partial_initial.as_deref().and_then(|partial| {
                    let base = raw_base
                        .get_or_insert_with(|| self.raw_base_lattice(raw_input, edges, raw_aux))
                        .take()?;
                    let _t = crate::prof::span(crate::prof::Stage::Step2c);
                    self.raw_partial_lattice(raw_input, base, partial)
                }))
            }
            _ => None,
        };
        if self.config.use_smart_compose
            && allow_partial_final
            // 原为 `syllables.len() >= 2`（至少两个完整音节才谈得上组句）。放开到 1 是为了
            // 让 `zaim`/`zdm` 这类「1 音节 + 残码」也能组句，其噪音由 6.5c 的延迟定夺挡住。
            && !syllables.is_empty()
            && completed_len < query.len()
            && !has_sep
            && !matches!(raw_step2c, Some(None))
        {
            let (lattice, full_len) = if let Some(Some((lattice, end, spans))) = raw_step2c {
                raw_partial_consumed = end;
                raw_partial_spans = spans;
                (lattice, end)
            } else {
                let seg_graph = SegGraph::from_dag(&Dag::build(query, trie));
                let mut lattice_nodes = self.lattice_builder.build(
                    query,
                    &seg_graph,
                    dict,
                    Some(&self.fuzzy_config),
                    true,
                );
                self.lattice_builder.add_partial_final_nodes(
                    query,
                    completed_len,
                    dict,
                    &mut lattice_nodes,
                );
                // S2：残码整句同样认用户词 —— 前半句用自造词、末尾还没打完，是最常见的形态
                // （`yougailunm` = 有 + 盖伦 + 残码 m）。残码那一段由上面的 partial_final
                // 节点负责，两者在同一张图上各管各的跨度。
                self.maybe_add_store_nodes(query, &seg_graph, true, &mut lattice_nodes);
                self.drop_blocked_nodes(&mut lattice_nodes);
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
                            canon: node.canon.clone(),
                            aux: 0,
                        });
                    }
                }
                (lattice, full_len)
            };
            let result = {
                let _t = crate::prof::span(crate::prof::Stage::Step2c);
                self.decode_unblocked(&lattice, full_len)
            };
            if !result.words.is_empty() && result.log_prob.is_finite() {
                let sentence: String = result.words.join("");
                #[cfg(test)]
                if result.pieces.iter().flatten().any(|p| p.aux > 0) {
                    raw_lattice::note_aux_sentence(&sentence);
                }
                if !sentence.is_empty() {
                    let weight = sentence_weight(result.log_prob, result.words.len());
                    // 击键图：边界与造词码换回全拼域（真值切分 + 残码一位），不用 `result.boundary`。
                    let (sent_boundary, sent_learn) = if raw_partial_consumed != 0 {
                        let b = if completed_len < 64 {
                            syllables_boundary_mask(&syllables, completed_len) | 1 << completed_len
                        } else {
                            0
                        };
                        (b, raw_lattice::raw_learn_code(&result, raw_input, query, b))
                    } else {
                        (result.boundary, result.learn_code(query))
                    };
                    let path = if raw_partial_consumed != 0 {
                        aux_path_of(&result, &raw_partial_spans)
                    } else {
                        None
                    };
                    // 此处 `completed_syls`（step 4 才定义）尚不可见，用同源的 `syllables`
                    if syllables.len() as u32 <= SENTENCE_KEEP_MAX_COMPLETED_SYLS {
                        // 短上下文档：留到 6.5b 之后按 SENTENCE_KEEP_RATIO 定夺（见上方说明）
                        short_sentence_pending =
                            Some((sentence, weight, sent_boundary, sent_learn, path));
                    } else if let Some(existing) =
                        candidates.iter_mut().find(|c| c.text == sentence)
                    {
                        // 同文合并（`nihaom` 的残码整句「你好吗」与 step4 的前缀补全同文）：
                        // 取更强的身份，理由同 step 2 的同文合并分支。用了辅码时整套换成整句身份。
                        existing.weight = existing.weight.max(weight);
                        existing.is_partial = false;
                        existing.is_sentence = true;
                        if let Some(p) = &path {
                            p.stamp(existing, raw_input, aux_sep);
                        }
                    } else {
                        let mut c = Candidate {
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
                            boundary: sent_boundary,
                            meta: wind_candidate::CandidateMeta {
                                learn_code: sent_learn,
                                ..Default::default()
                            },
                            consumed_length: raw_partial_consumed,
                            ..Default::default()
                        };
                        if let Some(p) = &path {
                            p.stamp(&mut c, raw_input, aux_sep);
                        }
                        candidates.insert(0, c);
                    }
                }
            }
        }

        timer.mark("sentp");

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
            // S2：混合整句里**全拼那几段**也认用户词（`bzdgailun` 的 `gailun` 段）……
            self.maybe_add_store_nodes(abbr_query, &graph, false, &mut lattice_nodes);
            // ……**声母段**另查用户层的简拼索引：上面那条按全拼码点查，在 `bcx` 这种声母串上
            // 必然落空，用户造的「拜城县」于是永远进不了 `bcxrmzf` 的整句。
            if self.config.sentence_uses_user_words
                && let Some(store_dm) = &self.store_layers
            {
                self.lattice_builder.add_store_abbrev_nodes(
                    abbr_query,
                    store_dm,
                    &mut lattice_nodes,
                );
            }

            // zh/ch/sh 被拆成两个声母的读法扣分（`zhge` ≠ 之后 + 个）。放在全部节点
            // （含用户层）进图之后，理由见 `RETROFLEX_SPLIT_PENALTY`。
            lattice::penalize_retroflex_splits(abbr_query, &mut lattice_nodes);
            // 简拼段抢走前一全拼音节韵尾的读法扣分（`ningbr` ≠ 你 + 能够 + 比如），见
            // `CODA_STEAL_PENALTY`。
            lattice::penalize_coda_steals(abbr_query, trie, &mut lattice_nodes);
            self.drop_blocked_nodes(&mut lattice_nodes);
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
                        canon: node.canon.clone(),
                        aux: 0,
                    });
                }
            }
            let result = self.decode_unblocked(&lattice, input_len);
            if !result.words.is_empty() && result.log_prob.is_finite() {
                let sentence: String = result.words.join("");
                // 质量闸门：低置信整句宁可不出，也不该顶掉可用的部分候选
                // （见 MIXED_SENTENCE_MIN_LOGP_PER_CHAR 的取值依据）。
                // 只看节点分：阈值按词频标定，挂上语法模型后 `log_prob` 里多了转移分
                // （见 `ViterbiResult::lexical_log_prob`）
                let logp_per_char =
                    result.lexical_log_prob / sentence.chars().count().max(1) as f64;
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
                            // **标 is_abbrev**：与整串简拼词同层、按权重竞争。
                            //
                            // 曾经刻意不标（理由：「沉进简拼层会被前缀回退的部分候选压住」），
                            // 结果它落在全拼层，被 `cmp_match_layers` 的「全拼优先于简拼」整层
                            // 顶在所有简拼候选之上、权重不参与比较 —— `zhge` 把 zh 读成 z|h
                            // 组出「之后个」（w=3263），压过「这个」（w=555006）。那条顾虑现已
                            // 不成立：前缀回退候选都带 `is_partial`，同层内排在完整匹配之后，
                            // 协调器又以消费长度为首键。
                            //
                            // 真实词库界面层实测：短简拼 5063 条首选命中 68.48% → 85.50%，
                            // 长串混合整句（pinyin_eval D 类）1000 条逐条零差异。
                            // 见 `tests/pinyin_mixed_sentence_layer.rs`。
                            is_abbrev: true,
                            // 简拼节点带完整规范码（回查词条的全拼 + 规范边界），故混合简拼整句的
                            // 造词码是完整全拼（`bzdgailun` → `buzhidaogailun`）；全拼段的模糊节点
                            // 同样贡献词典码。全部节点都是精确全拼时为 None。
                            meta: wind_candidate::CandidateMeta {
                                learn_code: result.learn_code(abbr_query),
                                ..Default::default()
                            },
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
                for h in self.lookup_with_fuzzy(&code, &syllables[..end], fixed_segmentation) {
                    // 同 step1：词典规范码另走 `learn_code`，不覆盖候选的 `code`。
                    let learn = h.dict.clone();
                    let typed = h.typed_boundary;
                    let before = candidates.len();
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
                    if candidates.len() > before {
                        candidates[before].meta.learn_code = learn;
                        candidates[before].meta.fuzzy_typed_boundary = typed;
                    }
                }
            }
        }

        timer.mark("mixed");

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
        // 而非子树规模增长，放开才真正划算：`b` 当时 300 条只要 0.9ms（改造前 30 条要 8ms）。
        //
        // ⚠️ **那个 0.9ms 已不成立**（2026-09-29 复测）：它量的是不带过滤的 `search_prefix`。
        // 本步现走 `..._syllable_capped`，过滤不参与剪枝（见 `WdatReader::search_prefix_inner`），
        // 合格条目凑不满 `limit` 时分支限界退化成整棵子树遍历 —— 裸声母 `b`/`sh` 曾约 30ms，
        // 词库层按码长上限剪枝后约 1~3ms（`prefix_code_len_cap`）。单音节整音节输入（`xi` 曾
        // 38ms）结果注定只剩精确匹配，已由下方 `prefix_completion_is_exact_only` 整段跳过。
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
        // **音节边界对齐同样下推到词库层**（`completed_len` 那个入参）：补全只能从下一个
        // 音节开始，不能把用户已经打完的那个音节继续拉长。缺了它，扁平码的字符前缀匹配会
        // 让 `shen` 召回 `sheng` 的字（前 20 条里 8 条）、`fen` 召回 `feng`、`ni` 召回
        // nin/ning/niu/nie/nian/niang/niao —— 而 `syllable_cap` 一条都拦不住，那些字的
        // 音节数完全合格。判据与两处放行见 `wind_dict::cached::prefix_syllable_aligned`。
        //
        // ⚠️ 传 `completed_len` 而非 `query.len()`：残码位（`shenm`）要的正是「把当前音节
        // 补完」，判据位落在残码**之前**才对。裸声母（`zh`）下 `completed_len == 0`，位 0
        // 恒置位 ⇒ 整条判据自动让开，退化为纯字符前缀匹配 —— 这也是主流全拼输入法的行为：
        // `fe` 不成音节故前缀匹配，`fen` 是合法音节故按音节匹配、不含 `feng`。
        // **先收集再定名额**：名额要发给权重最高的那几条（见 `COMPLETION_NEAR_PROMOTE_LIMIT`）。
        //
        // ⚠️ 下面那行按权重排序**当前是冗余的** —— 实测 `search_prefix_with_boundary_syllable_capped`
        // 返回的就是权重降序，边遍历边计数结果相同（变异验证：删掉排序，两条护栏测试照样绿）。
        // 保留它是因为「名额按权重发」是本逻辑的**要求**，而「上游按权重返回」是上游的
        // **当前实现**，把要求寄存在别处的实现细节上，改那边的人不会知道这里依赖它。
        // 代价只有一次 ≤ completion_limit 的排序。
        // 单音节整音节输入下本步注定只剩精确匹配（step 1 已给），整段跳过，见该函数。
        let prefix_hits: Vec<_> = if prefix_completion_is_exact_only(trailing_partial, syllable_cap)
        {
            Vec::new()
        } else {
            #[cfg(test)]
            test_hooks::note_step4_prefix_scan();
            dict.search_prefix_with_boundary_syllable_capped(
                query,
                completion_limit,
                syllable_cap,
                completed_len,
            )
            .into_iter()
            .map(|h| {
                let distance = effective_boundary(&h.code, h.boundary, trie)
                    .count_ones()
                    .saturating_sub(completed_syls);
                (h, distance)
            })
            .collect()
        };
        // 够得着无条件上浮的那批（`w > 0` 那条门槛见下方 `demote_to_prefix_layer`），
        // 按权重降序取前 N 个名额；其余沉回前缀层。
        let near_promoted: std::collections::HashSet<usize> = {
            let mut idx: Vec<usize> = prefix_hits
                .iter()
                .enumerate()
                .filter(|(_, (h, d))| {
                    h.weight > 0 && *d <= COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES
                })
                .map(|(i, _)| i)
                .collect();
            idx.sort_by_key(|&i| std::cmp::Reverse(prefix_hits[i].0.weight));
            idx.into_iter()
                .take(COMPLETION_NEAR_PROMOTE_LIMIT)
                .collect()
        };
        for (hit_idx, (h, distance)) in prefix_hits.into_iter().enumerate() {
            // `distance`（候选比已完成音节多出的音节数）已在上方连同 `effective_boundary`
            // 一起算好 —— 那里有一条要害注释：`boundary == 0`（导入词库 / 手输码 / 旧数据）
            // 时 `count_ones()` 也是 0，不过 `effective_boundary` 就会让多音节词冒充「完全对齐」。
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
                    || if distance > COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES {
                        h.weight < COMPLETION_FAR_WEIGHT_FLOOR
                    } else {
                        // 近距离：无条件上浮**限名额**，没抢到的沉回前缀层。
                        !near_promoted.contains(&hit_idx)
                    }
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

        // 整串是否**纯简拼形态**（每字母均为某音节首字母、且非完整音节序列）。
        // 提成变量供 step5 / step6 / step6.2 共用，`enable_abbrev` 仍在短路前（关闭时
        // 连 is_abbreviation 的 Dag 构建都省掉）。
        timer.mark("prefix");
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
            for (key, edits) in self.abbrev_recall_keys(abbr_query) {
                // `edits` 由 `fuzzy_abbrev_keys` 在构造期给出（逐位计），与校验侧
                // `Initial` 段计 1 处**同口径**（简拼键的每一位就是一个 `Initial` 段）。
                // 精确键 ⇒ 0 ⇒ `fuzzy_penalized` 原样返回、`is_fuzzy` 为 false，
                // 与改动前逐字节一致。
                //
                // ⚠️ **不罚会让模糊解压过精确解**：`is_abbrev` 只把简拼整层压到全拼之后，
                // **层内仍按 weight 降序**。折扣是层内唯一区分「精确键」与「变体键」的机制。
                // 实测（`lqc`→「篮球场」1191 / `nqc`→「南区菜」800，开 n_l 打 `nqc`）：
                // 不罚时模糊解排第 1、精确解第 2；罚一处后 1191×0.5=595 < 800，次序才对。
                // 同一个不变量在混合路径已由 `fuzzy_mixed_abbrev_is_penalized_and_marked`
                // 守着，纯简拼这条当时漏了。
                for abbr_code in dict.search_abbrev(&key, ABBREV_INDEX_LIMIT) {
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
                            fuzzy_penalized(h.weight, edits),
                            999999,
                            edits > 0,
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
                        }
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
        timer.mark("abbrevs5");

        // 5b. 混合简拼：同一串里混用声母与完整音节（`nhao` = n + hao、`nih` = ni + h）。
        if !mixed_pats.is_empty() {
            // 同一串的多条解释常投影到同一个键（`nhao` 的 [n][hao] 与 `nih` 的 [ni][h] 都是
            // `nh`），按键去重后每个键只点查一次索引。
            // 每条模式再展开首字母模糊变体（见 `abbrev_recall_keys`），合并去重后逐键点查。
            let mut keys: Vec<String> = mixed_pats
                .iter()
                .flat_map(|p| self.abbrev_recall_keys_only(p.key()))
                .collect();
            keys.sort_unstable();
            keys.dedup();
            timer.mark("a5keys");
            for key in &keys {
                // 与 step5 共用 `ABBREV_INDEX_LIMIT`（两条路径查的是同一张表、同一个键）。
                // 这里拿到的码还要过一道逐段校验，**绝大多数会被滤掉**，窗口小了几乎一条不剩。
                for abbr_code in dict.search_abbrev(key, ABBREV_INDEX_LIMIT) {
                    for h in dict.search_with_boundary(&abbr_code) {
                        // 无边界信息 → 判据不存在 → 不参与（不是放行，见 syllables_from_boundary）。
                        let Some(syls) =
                            mixed_abbrev::syllables_from_boundary(&abbr_code, h.boundary)
                        else {
                            continue;
                        };
                        // 取**模糊处数最少**的解释计罚（同 `recall_abbrev_prefix` ②）。
                        // **不再按 `p.key() == key` 预筛**：召回键现在可能是某条模式的
                        // 模糊变体，与该模式的原键并不相等，预筛会把刚捞回来的词又滤掉。
                        // 判据本就该是「有没有任一条模式解释得通这个词」，键只是召回手段；
                        // 模式数有 `MAX_PATTERNS` 封顶，且 `matches_with` 首先比音节数，
                        // 去掉预筛的代价是每条候选多几次整数比较。
                        let Some(edits) = mixed_pats
                            .iter()
                            .filter_map(|p| {
                                p.matches_with(&syls, |seg, syl| self.seg_matches_fuzzy(seg, syl))
                            })
                            .min()
                        else {
                            continue;
                        };
                        let before = candidates.len();
                        push_unique(
                            &mut candidates,
                            h.text,
                            abbr_code.clone(),
                            fuzzy_penalized(h.weight, edits),
                            999999,
                            edits > 0,
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
                        }
                    }
                }
            }
            timer.mark("a5search");
        }

        timer.mark("abbrevs");

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
            // 音节数 / 边界对齐 / 码长过滤同 step 4 下推到词库层：超限的补全过不了 6.3，也不该
            // 借同文合并给已有候选提权；不下推则名额被它们吃掉，权重全打平的大用户词库还会
            // 整棵子树遍历（19 万条导入词库下 `sh` 约 7ms）。
            store_cands.extend(store_dm.search_prefix_capped(
                query,
                limit,
                syllable_cap,
                completed_len,
            ));
            // 模糊音（t215）：整串与各前缀子码的变体码，变体生成与系统词 `lookup_with_fuzzy`
            // 同源（`fuzzy_codes`）；前缀补全那路系统词也不做模糊，这里同样不做。
            // 排在全部精确结果**之后**：下面按 text 合并、先到者占位，同文时须以精确那条为准。
            // 整串只在无残码时展开：有残码时（`chaijiuduolianm`）精确那路也只查 `query`、
            // 查不到 `completed` 这一段 —— 与系统词 step 1 查 `completed` 不同，是既有口径，
            // 两路要改须一起改。
            // 关模糊音时整段跳过（`search_store_fuzzy` 本也立即返回，但前缀循环的 join 白分配）。
            let fuzzy_on = self.fuzzy_config.any_enabled();
            if fuzzy_on && completed == query {
                store_cands.extend(self.search_store_fuzzy(
                    store_dm,
                    query,
                    &syllables,
                    fixed_segmentation,
                    limit,
                    &self.fuzzy_config,
                ));
            }
            if fuzzy_on && syllables.len() >= 2 {
                for end in 1..syllables.len().min(6) {
                    let code: String = syllables[..end].join("");
                    if code == query {
                        continue;
                    }
                    store_cands.extend(self.search_store_fuzzy(
                        store_dm,
                        &code,
                        &syllables[..end],
                        fixed_segmentation,
                        limit,
                        &self.fuzzy_config,
                    ));
                }
            }

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
                // - 来源标记**按 `c` 的实际归属置位**（用户层→`is_user_dict`，临时层→
                //   `is_temp_dict`），并把 `c.code` 记进 `meta.store_code`。
                //
                //   ⚠️ 这两件事都不是「诊断/UI 用」的装饰：右键删除按标记选表、按码拼 key。
                //   2026-09-03 实测——自动造词写进**临时**层的「再也不好」与整句层同文，
                //   合并时被无条件盖成 `is_user_dict=true`，于是删除跑去 `user_words` 删一个
                //   不存在的 key（`redb` 静默成功），临时词纹丝不动、点几次都无作用。
                //   同理 `code` 保留已有那条的（见上一条），删除必须改读 `store_code`，
                //   否则用户词码为 `nihaoa`、候选码为 `nihao` 时同样删了个空。
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
                        // 码与边界必须取自**同一条**候选（此处都是 `existing` 的，
                        // 合并分支刻意保留已有候选的这两项，理由见上方注释）。
                        effective_boundary(&existing.code, existing.boundary, trie).count_ones(),
                        self.config.completion_max_extra_syllables,
                    ) {
                        let w = existing.weight.max(c.weight);
                        existing.weight = promotion_cap.map_or(w, |cap| w.min(cap));
                    }
                    // 标记按来源分流：`c` 来自 StoreTempLayer 就是临时词，不能盖成用户词。
                    // 两层都有同文记录时两个标记都置，删除侧据此把两张表都删掉。存储码随标记
                    // 一起带走（`code` 字段仍归已有候选），同层已并入过时模糊那条不改写。
                    merge_store_origin_and_code(existing, &c, c.is_fuzzy);
                    continue;
                }
                c.source = CandidateSource::Pinyin;
                // 与 push_unique 一致：store 层的前缀子码命中也是子短语，降到完整匹配之后。
                c.is_partial =
                    !c.is_prefix && c.code.len() < query.len() && query.starts_with(&c.code);
                // 编码手填成简拼的用户词（`rq` → 「$Y年$M月$D日」，GH#177）：码与击键逐字相同，
                // 上面的精确查找会把它当**全拼精确命中**放进完整匹配层；而这串击键的系统候选
                // 全在简拼层（`is_abbrev`，层级里最沉的一层），层级是硬闸门 ⇒ 权重 0 也恒居首选。
                // 击键被判为纯简拼时（`is_abbreviation`：非完整音节序列），码 == 击键的用户词
                // 本身就是简拼码，归入同一简拼层按权重 / 词频竞争（与 step5 / 下方简拼召回同层）。
                // 比 `abbr_query` 而非 `query`：双拼下 `query` 是转换后的全拼，与击键不同域，
                // 码恰为那段全拼的用户词是真正的全拼命中，不能降层。
                // `natural_order` 不改：它承载同码用户词的导入序（t80），层内等权时仍按它排。
                //
                // ⚠️ **击键本身是音节（序列）的前缀时不降层**（`sh`/`zh`/`zho`/`yans`，判据即
                // 混输闸门 `is_possible_pinyin_sequence`）。`is_abbreviation` 对这类「未打完的
                // 音节」同样判真，但它们的主解释是全拼：系统有一大批前缀补全（是 / 上…），
                // 而简拼层排在前缀层、子短语层之后（`cmp_match_layers`）。此时降层就不是
                // 「与系统简拼同层竞争」，而是沉到整批补全之下、常常出首页——用户给 `sh`
                // 配的词凭空打不出来了。故只在击键**无法**读作全拼（`rq`/`bzd`）时降层，
                // 这时它的同串对手只剩简拼候选，降层恰好落在它们之间。
                if stroke_is_plain_abbrev
                    && !c.is_prefix
                    && c.code == abbr_query
                    && !self.is_possible_pinyin_sequence(abbr_query)
                {
                    c.is_abbrev = true;
                }
                // 用户/临时词的前缀补全（is_prefix=true，码更长）：打到词尾附近就提升进完整
                // 匹配层，否则被首音节同音子短语整层淹没（长词打到第 3-4 音节才给的根因）。
                // is_prefix 保持结构真值不动，排序提升由 is_promoted_completion 承接。
                if c.is_prefix {
                    // 上浮判据与显示序档位**共用同一次现切**：两者都要「这个词几个音节」，
                    // 而 `boundary == 0`（手输码用户词的常态）下每次都是一趟 `Dag::build`。
                    //
                    // 档位口径同 step4，连同那条「boundary == 0 要过 `effective_boundary`
                    // 现切」一起（理由见 step4 的 `distance`）。用户词里手输码条目最多，
                    // 这一处漏掉的话，导入词库绕过分档的路就没堵上。
                    let word_syls = effective_boundary(&c.code, c.boundary, trie).count_ones();
                    if should_promote_user_completion(
                        completed_syls,
                        trailing_partial,
                        word_syls,
                        self.config.completion_max_extra_syllables,
                    ) {
                        c.is_promoted_completion = true;
                        if let Some(cap) = promotion_cap {
                            c.weight = c.weight.min(cap);
                        }
                    }
                    c.completion_extra_syllables = word_syls
                        .saturating_sub(started_syllables)
                        .min(u8::MAX as u32)
                        as u8;
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
                    if c.text.is_empty() {
                        continue;
                    }
                    // 比对基准是原始击键（见 `abbr_query`）：双拼下 query 已是转换结果，
                    // 拿它比对永远匹配不上用户敲的简拼。
                    let plain_edits = self.abbrev_matches_stroke(&c.code, c.boundary, abbr_query);
                    // 混合简拼：按 boundary 切回音节序列逐段比对（无边界 → 无判据 → 不参与）。
                    // 与系统词侧走同一批 `mixed_pats`，判据完全一致，只是这边不经索引——
                    // 用户词规模小，现算即可（与 `abbrev_of_code` 那条注释同理）。
                    // 同上：短路只在纯简拼**精确**命中时成立。
                    let mixed_edits = if plain_edits == Some(0) {
                        None
                    } else {
                        mixed_abbrev::syllables_from_boundary(&c.code, c.boundary).and_then(
                            |syls| {
                                mixed_pats
                                    .iter()
                                    .filter_map(|p| {
                                        p.matches_with(&syls, |seg, syl| {
                                            self.seg_matches_fuzzy(seg, syl)
                                        })
                                    })
                                    .min()
                            },
                        )
                    };
                    // 取**处数最少**的解释计罚（同 step5b）。⚠️ 不能用 `or`：两边都有值时
                    // 它返回左边，等于取了更差的解释。全拼下已证 `plain <= mixed`、min 恒等于
                    // plain；保留 min 是因为**这一处**正是那条证明失效的地方 —— plain 用
                    // `abbr_query`（击键域）而 `mixed_pats` 来自 `mixed_pattern_source`，
                    // 双拼带手动分隔符时后者是转换后的全拼，两者不同域。详见前缀回退那处的论证。
                    let Some(edits) = [plain_edits, mixed_edits].into_iter().flatten().min() else {
                        continue;
                    };
                    // 同文已在（系统简拼候选）：合并，不丢弃。判据要先过——没过判据的命中
                    // 不是这串击键的解释，不该给同文候选提权。
                    if let Some(existing) = candidates.iter_mut().find(|x| x.text == c.text) {
                        let w = fuzzy_penalized(c.weight, edits);
                        merge_store_abbrev_hit(existing, &c, w, edits > 0);
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
                    // 模糊命中的折扣与来源标记。**纯简拼命中不再恒精确** —— 召回侧放宽后
                    // 它也可能是变体键命中（`lqc` 下的词经 `nqc` 捞回），处数由
                    // `abbrev_matches_stroke` 给出。不罚的话，同一投影键下精确命中的
                    // 用户词会被压过去，而用户词权重普遍高于系统词，压过去比系统词侧更容易。
                    if edits > 0 {
                        c.weight = fuzzy_penalized(c.weight, edits);
                        c.is_fuzzy = true;
                    }
                    candidates.push(c);
                }
            }
        }

        // 6.2 简拼族**前缀回退**：退到更短的前缀，余下字母作残码留给下一次输入（分步上屏）。
        //
        //     ★ **整串命中时也照样补**（对齐微软拼音：整长匹配 → 词组前缀 → 首声母单字）。
        //     补进来的都是 `is_partial`，沉在整长匹配之后，不改变首选；此前整串一命中就
        //     不再降级，`tymh` 这类只有一个整长词的输入候选只剩 1 条（t371）。
        //     下面各段「整串一无所获才降级」的论证是**旧设计**，保留作历史背景：它们
        //     论证的风险（短切点高频词抢首选）在整串命中时由层级键挡住。
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
        //     代价是候选变多，用 `AbbrevFallbackConfig` 的限额兜住。
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
        if self.config.enable_abbrev && !mixed_covered && abbr_query.len() >= MIN_ABBREV_STROKE {
            let mut cuts_with_hits = 0usize;
            for take in (MIN_ABBREV_STROKE..abbr_query.len()).rev() {
                if !abbr_query.is_char_boundary(take) {
                    continue;
                }
                let before = candidates.len();
                self.recall_abbrev_prefix(&abbr_query[..take], take, &mut candidates);
                if candidates.len() > before {
                    cuts_with_hits += 1;
                    if cuts_with_hits >= self.config.abbrev_fallback.cuts {
                        break;
                    }
                }
            }
            // 最末一档：首声母对应的单字（微软拼音的「整串 → 词组 → 单字」）。
            // 只对纯简拼形态做：`buxzd` 这类首个音节已打全的串，首字母不是用户的意图。
            if stroke_is_plain_abbrev && !self.first_letters_start_a_syllable(abbr_query) {
                self.recall_first_initial_chars(
                    &abbr_query[..1],
                    max_candidates.clamp(30, MAX_COMPLETION_CANDIDATES),
                    &mut candidates,
                );
            }
        }

        // 手动分隔符边界过滤：用户以 `'` 强制音节边界后，凡「码恰好落在某音节边界、但字数
        // 与所跨音节数不符」的候选被剔除——如 xi'an 强制 [xi,an]，则单字「先」(code=xian,
        // 跨 2 音节却仅 1 字) 不该出现；「西」(code=xi,1 字 1 音节)、整句「西安」(2 字 2 音节) 保留。
        // 码不落在任何边界的候选（如前缀补全）不受影响。
        //
        // ⓘ 曾在此加过一条 `is_abbrev` 豁免（简拼族的 code 是全拼码、与击键不同域，
        // 拿手动分段去量必然不符）。**撤掉了**：`syllables` 改成只收从起点连续的音节段
        // 之后，简拼族出场的前提恰恰是「起点那段成不了音节」⇒ 那时 `syllables` 为空，
        // `syllable_span` 一律返回 None 走 `_ => true`，豁免永远不会被用到。
        // 变异验证时把它去掉，7 条用例全绿——没有可达路径能证明它必要，故不留。
        // 若将来放开 step 2b 的 `!has_sep` 门槛（`ni'hm` 那类三段混合），
        // 简拼候选就可能与非空 `syllables` 共存，届时再加回来并配一条用例。
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
            //
            // ★ 边界对齐一并在此复查，为的是词库层够不着的两批候选：step 6 并入的
            // 用户/临时词（走 `store_dm.search_prefix`，那条 API 没有 cap 入参），
            // 以及 `boundary == 0` 被词库层放行、在这里才由 DAG 补出真值的条目。
            let b = effective_boundary(&c.code, c.boundary, trie);
            wind_dict::cached::prefix_entry_keep(b, c.code.len(), syllable_cap, completed_len)
        });

        // 裸声母（无完整音节，如 "m"/"zh"）单字优先：候选全为前缀补全词（is_prefix=true），
        // 纯按词频排会让高频多字词（没有/目前）压过单字（吗/么）——不合直觉。给单字提权使其
        // 排在多字词之前（对齐主流输入法首字优先，见 BARE_INITIAL_SINGLE_CHAR_BOOST）。
        // 仅此情形——完整音节输入的单字已靠精确层级(is_prefix=false)就位，无需提权。
        if syllables.is_empty() {
            for c in candidates.iter_mut() {
                // 简拼前缀回退补的首声母单字（6.2 末档）不提权：要排在词组之后。
                if c.text.chars().count() == 1 && !(c.is_abbrev && c.is_partial) {
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
            // 排除引擎合成的其它整句（N-best 备选）：它不是词条，算进来会让第 1 名降到
            // 第 2 名之下、名次倒挂。并入了用户/临时词的那条是词条，照算。
            let exact_max = candidates
                .iter()
                .filter(|c| {
                    c.text != sent
                        && !c.is_fuzzy
                        && !c.is_prefix
                        && !c.is_partial
                        && c.code == completed
                        && !(c.is_synthesized && !c.meta.is_user_dict && !c.meta.is_temp_dict)
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
        if let Some((text, weight, boundary, learn_code, aux_path)) = short_sentence_pending {
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
                    // 用了辅码时整套换成整句身份（同 step 2 / S_n 的同文合并）。
                    if let Some(p) = &aux_path {
                        p.stamp(existing, raw_input, aux_sep);
                    }
                } else {
                    let mut c = Candidate {
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
                        meta: wind_candidate::CandidateMeta {
                            learn_code,
                            ..Default::default()
                        },
                        consumed_length: raw_partial_consumed,
                        ..Default::default()
                    };
                    if let Some(p) = &aux_path {
                        p.stamp(&mut c, raw_input, aux_sep);
                    }
                    candidates.insert(0, c);
                }
            }
        }

        // 6.5d 辅码整串整句的排位（step 2 比分闸门的结论）：放行的排到末槽整句与 2c 残码整句
        // 之前，不放行的排在末槽整句之后。放在各轮让位之后，按它们最终的权重定。整句按身份找：
        // 末槽整句是 `sentence_rank == 1` 的整句，辅码整句是同文且消费整串的整句。
        // 不过闸的先压、过闸的后抬：抬的那条要高过压完后的全部其它整句。
        aux_tail.sort_by_key(|t| t.2);
        for (text, n, pass) in &aux_tail {
            let is_tail =
                |c: &Candidate| c.is_sentence && &c.text == text && c.consumed_length == *n;
            let top_w = candidates
                .iter()
                .find(|c| is_step2_top(c))
                .map(|c| c.weight);
            let others_w = candidates
                .iter()
                .filter(|c| c.is_sentence && !is_tail(c))
                .map(|c| c.weight)
                .max();
            if let Some(c) = candidates.iter_mut().find(|c| is_tail(c)) {
                if *pass {
                    if let Some(w) = others_w {
                        c.weight = c.weight.max(w.saturating_add(1));
                    }
                } else if let Some(w) = top_w {
                    c.weight = c.weight.min(w.saturating_sub(1));
                }
            }
        }
        // 整句池纳入过闸的整串整句：界面上它居首，池里也排第 1（同文的末槽整句让给它），其余顺延；
        // 候选里带名次的整句按新池重编，切换键按名次换位才与界面一致。不过闸的不进池。
        // 池里文字互不相同，候选按文字对名次（同文合并的普通候选此刻还没有击键域的消费数）。
        let pool_k = self.config.sentence_counts().1;
        if pool_k > 1
            && let Some((text, n)) = aux_tail.iter().filter(|t| t.2).find_map(|(text, n, _)| {
                candidates
                    .iter()
                    .any(|c| c.is_sentence && &c.text == text && c.consumed_length == *n)
                    .then(|| (text.clone(), *n))
            })
        {
            let is_sn = |c: &Candidate| c.is_sentence && c.text == text && c.consumed_length == n;
            if let Some(sn) = candidates.iter().find(|c| is_sn(c)) {
                sentence_pool.retain(|p| p.text != text);
                sentence_pool.insert(0, sn.clone());
                sentence_pool.truncate(pool_k);
            }
            for (i, p) in sentence_pool.iter_mut().enumerate() {
                p.sentence_rank = (i + 1) as u8;
            }
            // 顺延后仍按「露 N」封顶：露不出来的新建整句只留池里，同文合并的普通候选名次清 0。
            let show_n = self.config.sentence_counts().0;
            const DROP: u8 = u8::MAX;
            for c in candidates
                .iter_mut()
                .filter(|c| c.sentence_rank > 0 || is_sn(c))
            {
                let rank = sentence_pool
                    .iter()
                    .position(|p| p.text == c.text)
                    .map_or(0, |i| i + 1);
                c.sentence_rank = match rank {
                    1.. if rank <= show_n => rank as u8,
                    _ if c.is_synthesized => DROP,
                    _ => 0,
                };
            }
            candidates.retain(|c| c.sentence_rank != DROP);
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
                    // 连续辅助码的辅码候选（`aux_keys`）同理：码按解码路径去掉了辅码，而双拼转换
                    // 把辅码字母也读成了音节，两者不同域。
                    c.is_abbrev
                        || c.is_fullpinyin_fallback
                        || c.meta.aux_keys.is_some()
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

        // 换位纠错召回：主切分不走纠错边（见 `Dag::maximum_match`），纠错词在这里按走纠错边的
        // 切分另查——整串与含纠错音节的各级前缀，同 step 1 / step 3 的查法。放在全部召回之后：
        // `push_unique` 同文先到者占位，同一文字有非纠错来源时留非纠错那条。
        // 经本步新增的文字：截断时只占非纠错候选用剩的名额（见下方截断处）。不能只看
        // `is_corrected`——末尾打字途中的半截（`zhon`）对齐判作前缀、不标纠错。
        let mut correction_added: std::collections::HashSet<String> = Default::default();
        // 经「纠错音节 + 简拼尾」并入的文字：`fill_pinyin_hints` 按候选自身的码判纠错、会把它们判成
        // 非纠错，收尾处要补标回来（界面的 `*` 前缀、低置信待遇都看 `is_corrected`）。
        let mut tail_corrected: std::collections::HashSet<String> = Default::default();
        if !self.fuzzy_config.correction.is_empty() && sp_result.is_none() && !has_sep {
            let corr_syls = Dag::build(query, trie).maximum_match_with_correction();
            let first_corr = corr_syls
                .iter()
                .position(|s| trie.is_correction_spelling(s));
            if let Some(first) = first_corr {
                for end in (first + 1)..=corr_syls.len().min(6) {
                    let code: String = corr_syls[..end].concat();
                    for h in self.lookup_corrected(&code, &corr_syls[..end]) {
                        let learn = h.dict.clone();
                        let typed = h.typed_boundary;
                        // 同文的简拼读法只吃了前几个键（`buyogn` 的「不用」= `bu`+`y`），纠错读法吃满
                        // 整段：换掉它，否则纠错读法被 `push_unique` 的同文占位挡掉，想打的词沉在
                        // 简拼层。同文的非简拼候选（精确读法）仍照旧优先。
                        candidates.retain(|c| {
                            !(c.text == h.text
                                && c.is_abbrev
                                && c.consumed_length != 0
                                && c.consumed_length < code.len())
                        });
                        let before = candidates.len();
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
                        if candidates.len() > before {
                            candidates[before].meta.learn_code = learn;
                            candidates[before].meta.fuzzy_typed_boundary = typed;
                            correction_added.insert(candidates[before].text.clone());
                        }
                    }
                }
            }
            // 纠错音节后接简拼尾（`yogny` = yogn + y）：整串没有音节解释、上面只查得到完整音节，
            // 尾巴没人认。把纠错拼写换成正确音节（`yongy`），交给同一套转换（混合简拼）再走一遍，
            // 只收吃满整串的候选（混合简拼、前缀补全），按纠错计罚并入。替换后的串不再含纠错拼写，二次转换里
            // 用 `no_correction_tail` 兜底防递归。
            let prefix_len: usize = corr_syls.iter().map(String::len).sum();
            if !opts.no_correction_tail
                && first_corr.is_some()
                && prefix_len < query.len()
                && query.starts_with(&corr_syls.concat())
            {
                let fixed: Option<String> = corr_syls
                    .iter()
                    .map(|s| {
                        if trie.is_correction_spelling(s) {
                            self.fuzzy_config
                                .correction
                                .corrections_of(s)
                                .first()
                                .cloned()
                        } else {
                            Some(s.clone())
                        }
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(|v| v.concat() + &query[prefix_len..]);
                // 各段起点（含尾巴起点）的击键域 bitmask，供编码栏切分。
                let tail_boundary: u64 = corr_syls
                    .iter()
                    .scan(0usize, |pos, s| {
                        *pos += s.len();
                        Some(*pos)
                    })
                    .filter(|&p| p < 64)
                    .fold(0u64, |m, p| m | (1u64 << p));
                if let Some(fixed) = fixed {
                    let mut sub = opts.clone();
                    sub.no_correction_tail = true;
                    if let Ok(r) = self.convert_with_opts(&fixed, max_candidates, sub) {
                        for mut c in r.candidates {
                            if c.is_partial || c.consumed_length != fixed.len() {
                                continue;
                            }
                            // 同文已有吃满整串的候选就不换（精确读法优先）；只有更短的才被它顶掉。
                            if candidates.iter().any(|e| {
                                e.text == c.text
                                    && (!e.is_abbrev
                                        || e.consumed_length == 0
                                        || e.consumed_length >= query.len())
                            }) {
                                continue;
                            }
                            candidates.retain(|e| e.text != c.text);
                            c.weight = fuzzy_penalized(c.weight, correction::CORRECTION_EDITS);
                            c.is_fuzzy = true;
                            // 并入简拼层，与合法的简拼候选按（罚过的）权重竞争：前缀补全层本身压着
                            // 简拼层，纠错尾巴凭它会越过用户真打的简拼词（`nagnd` 的「那个年代」）。
                            c.is_abbrev = true;
                            c.is_prefix = false;
                            c.consumed_length = query.len();
                            c.meta.tail_typed_boundary = tail_boundary;
                            correction_added.insert(c.text.clone());
                            tail_corrected.insert(c.text.clone());
                            candidates.push(c);
                        }
                    }
                }
            }
        }

        // 误读召回（设计 §6）：整串恰为表里某条的错误读音（`xinxin` → 「莘莘」）⇒ 词库读不出
        // 也放进候选。权重取同页最低再低一档，标 `is_misread_recall`（不占首选、不计自动上屏，
        // 截断时与纠错共用保底）。词库已有同文字候选时不召回，那条由收尾的提示照标。
        if let Some(table) = self.fuzzy_config.misread.clone() {
            // 拼音 weight 恒非负（负 weight 在 `better_natural` 里另有「非精确」含义），故夹到 0。
            let lowest = candidates.iter().map(|c| c.weight).min().unwrap_or(1);
            let floor = (lowest - 1).max(0);
            for e in table.recall(query) {
                if candidates.iter().any(|c| c.text == e.text)
                    || opts.admit.as_deref().is_some_and(|f| !f(&e.text))
                {
                    continue;
                }
                let correct = &e.correct;
                let order = candidates.len() as i32;
                candidates.push(Candidate {
                    text: e.text.clone(),
                    code: query.to_string(),
                    weight: floor,
                    natural_order: order,
                    source: CandidateSource::Pinyin,
                    boundary: misread_boundary(&e.wrong),
                    is_misread_recall: true,
                    meta: wind_candidate::CandidateMeta {
                        learn_code: Some((correct.concat(), misread_boundary(correct))),
                        pinyin_hints: misread_hints(e),
                        hint_syllables: correct.clone(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
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
                // 含手动分隔符时，回退给的是剥除 `'` 之后的字节数，换算回原始输入空间，
                // 否则选中词组后协调器按含 `'` 的缓冲切片会残留错位（`b'z'd` 选 `bz` 的词组）。
                if has_sep {
                    c.consumed_length = raw_len_for_stripped(input, c.consumed_length);
                }
                continue;
            }
            // 全拼降级候选（step 6.7）同样自带击键域的消费数。**不可再过
            // `map_consumed_length`**：那是双拼流专用的「全拼字节 → 双拼键数」回映射，而本
            // 支路的码本身就是击键串的前缀（全拼域 ≡ 击键域），再换算一次即错位——
            // `nihao` 选「你好」会被回映射成 4（双拼 4 键的量），凭空吞掉一个键。
            if c.is_fullpinyin_fallback {
                continue;
            }
            // 击键图整句（step 2 / 2c）同样自带击键数。
            if raw_edges.is_some() && c.is_sentence && c.consumed_length != 0 {
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
        // 基准取 `raw_input.len()`：上一段刚把 `consumed_length` 回映射到**原始输入空间**
        // （双拼 → 击键数、含分隔符 → 含 `'` 的串）。`query`（剥除分隔符后）与它不同域；
        // ⚠️ `input` 也不行——双拼下它早被换成转换后的全拼（`nihc` → `nihao`，5 > 4 键），
        // 曾因此把双拼的整串候选全部当成半截丢光。全拼下 `raw_input == input`，行为不变。
        if require_full_match {
            candidates.retain(|c| c.consumed_length == 0 || c.consumed_length >= raw_input.len());
        }

        // 「屏蔽这个词」：任何来源（系统 / 用户 / 临时 / 模糊 / 简拼 / 纠错 / 误读召回）的同文候选
        // 一律不出。放在截断之前，空出的名额由后面的候选补上。
        let blocked = self.blocked_words();
        if !blocked.is_empty() {
            candidates.retain(|c| !blocked.contains(&c.text));
        }

        // 6.5e 不过闸的辅码整句在界面序下的排位（承 6.5d）。界面先按消费数分档：整串整句（S_n）
        // 消费 n，与残码整句、补全同档，6.5d 把它压到末槽整句（消费 e < n）之下管不到这一档。
        // 这里按界面排序键（权重之前的各键）找同档对手，压到其中最强者之下；有更靠前一档的候选时
        // 它本就不居首，不动；一个对手都没有时它在界面上必居首，只好不出。
        // 放在消费数回映射之后（各候选此时才有击键域的消费数）、截断之前。
        place_failed_aux_tail(&mut candidates, &aux_tail, raw_input);

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
        // 保底配额现已有**一档**：`truncate_with_quotas`，判据是 `is_abbrev`。
        // 但下沉 P0 要的是按**消费长度**分档（参考混输 `PINYIN_QUOTA_DIVISOR`），那件仍未做
        // —— 单档只护住了简拼这一类，消费长度短的候选仍会被长候选整批挤出截断窗口。
        //
        // 零权重读音让位的重排（截断之后）用的也是它，两处必须同一个比较器。
        let engine_order = |a: &Candidate, b: &Candidate| {
            wind_candidate::cmp_match_layers(a, b)
                .then(b.weight.cmp(&a.weight))
                .then(a.natural_order.cmp(&b.natural_order))
        };
        // 换位纠错：截断前先判出纠错候选（`is_corrected`）、去掉同文的纠错那条。纠错候选按
        // 罚过的 weight（`0.5^CORRECTION_EDITS`，与模糊同量纲）与其余候选同一比较器穿插；
        // 窗口里保底见 [`correction_quota`]（对齐 librime 纠错上限 4），挤出去的只是窗口尾部。
        let correction_on = !self.fuzzy_config.correction.is_empty();
        if correction_on {
            self.fill_pinyin_hints(&mut candidates, query, raw_input, sp_result.as_ref());
            for c in candidates.iter_mut() {
                if tail_corrected.contains(&c.text) {
                    c.is_corrected = true;
                }
            }
            wind_candidate::drop_corrected_duplicates(&mut candidates);
        }
        candidates.sort_by(engine_order);
        // 纠错召回步新增的也算：末尾半截（`zhon`）对齐判作前缀、不标纠错。
        // 误读召回同样只占保底名额（`is_misread_recall`）。
        let is_corr = |c: &Candidate| {
            c.is_misread_recall
                || (correction_on && (c.is_corrected || correction_added.contains(&c.text)))
        };
        // `abbrev_quota = false`：调用方声明了不重排（见 `ConvertOptions::no_abbrev_quota`），
        // 简拼补位在那边是净损失；纠错保底保持有序，照做。
        truncate_with_quotas(&mut candidates, max_candidates, abbrev_quota, &is_corr);

        // 零权重读音让位：精确单字的**这个读音**词库权重为 0（𤭢 cei、塞 sei、嫩 nun、这 zhei）
        // ⇒ 并入简拼层按 weight 竞争（`Candidate::is_zero_weight_reading`）。双拼两键 `cw`
        // 首选回到简拼词「成为」，𤭢 仍在候选里；嗲(dia=182) 这类有权重的读音不动。
        //
        // 判据只认**词库原值**：模糊命中（权重是折扣后的、读音也不是这个）、整句、补全、
        // 子短语、全拼降级支路一律不算；用户词 / 临时词里有这个字 = 用户用过这个读音，不让位。
        // 用户选过的字由 `freq_rerank::rerank_pinyin_positional` 摘掉标记。
        //
        // ⚠️ **必须在截断之后置位、再稳定重排**：标记把字沉到简拼层末尾，截断前置位的话
        // `truncate_with_quotas` 腾位时从尾部先挤掉的就是它们（小鹤 `jx` 的夾/挟/袷整条
        // 消失，8 个布局两键共 584 个输入掉字），而词频重排在截断之后、救不回来。
        // 截断按改动前的次序做 ⇒ 活过截断的集合与改前逐条一致，变的只有次序。
        // 位置也须在下方 preedit 读 `candidates.first()` 之前。
        let mut any_zero_weight = false;
        for c in candidates.iter_mut() {
            c.is_zero_weight_reading = c.weight == 0
                && c.code == completed
                && c.text.chars().count() == 1
                && !c.is_abbrev
                && !c.is_prefix
                && !c.is_partial
                && !c.is_sentence
                && !c.is_fuzzy
                && !c.is_fullpinyin_fallback
                && !c.meta.is_user_dict
                && !c.meta.is_temp_dict;
            any_zero_weight |= c.is_zero_weight_reading;
        }
        if any_zero_weight {
            candidates.sort_by(engine_order);
        }

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
        // 分隔显示——`nhao` 原样显示为 `nhao`，`nh` 显示为 `nh`。见 `candidate_preedit_split`。
        //
        // 切分按**每条候选**各算一份（`CandidateMeta::preedit_split`），协调器在高亮移动时
        // 换用高亮那条的；首选那份即这里的 preedit_display。
        if sp_result.is_none() && !has_sep {
            let mut cache = PreeditSplitCache::default();
            for c in candidates.iter_mut().chain(sentence_pool.iter_mut()) {
                c.meta.preedit_split = self.candidate_preedit_split(
                    c,
                    raw_input,
                    completed,
                    &partial_syllable,
                    &mut cache,
                );
            }
            if let Some(s) = candidates
                .first()
                .and_then(|c| c.meta.preedit_split.as_deref())
            {
                preedit_display = s.to_string();
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
                let (fp, fp_syllables, fp_partial) = self.compose_segment(raw_input);
                // 全拼降级候选也各带自己的切分：支路里击键即全拼码，与全拼主路同一套渲染。
                // `preedit_fullpinyin` 是整串最大匹配，`xiandaxue` 的「西安大学」走的却是
                // `xi|an|da|xue`。简拼标记同真的留给协调器的简拼形态（那边优先）。
                let fp_completed = fp_syllables.concat();
                let mut cache = PreeditSplitCache::default();
                for c in candidates
                    .iter_mut()
                    .filter(|c| c.is_fullpinyin_fallback && !c.is_abbrev)
                {
                    c.meta.preedit_split = self.candidate_preedit_split(
                        c,
                        raw_input,
                        &fp_completed,
                        &fp_partial,
                        &mut cache,
                    );
                }
                // 与 `sp_body` 比而不是 `preedit_display`：这一步答的是「全拼切法与**双拼
                // 切法**是否不同」，而 preedit_display 上一步可能已被换成简拼分段。拿它作
                // 基准的话，`nihao` 这类两种切法本就相同的串会因为简拼覆盖而白给一份 fp。
                if fp != sp_body {
                    preedit_fullpinyin = fp;
                }
            }
        }

        timer.mark("tail");
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

        // 开纠错时 `candidates` 已在截断前标过（见上）。
        if self.fuzzy_config.hint.get() && !correction_on {
            self.fill_pinyin_hints(&mut candidates, query, raw_input, sp_result.as_ref());
        }
        if self.fuzzy_config.hint.get() || correction_on {
            self.fill_pinyin_hints(&mut sentence_pool, query, raw_input, sp_result.as_ref());
            wind_candidate::drop_corrected_duplicates(&mut sentence_pool);
        }

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
            sentence_pool,
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
        // ü 拼写归一：用户手打 `xv` 当编码时应等价于 `xu`（规则见 [`spelling`]）。
        //
        // ⚠️ 须用**归一化后的码**做层 2 点查——词典存的是正字法形态 `xu`，拿 `xv` 查
        // `search_with_boundary` 必落空、掉进层 4，而层 4 建图用的 Trie 同样走不通
        // `xv` ⇒ 判 Unresolvable。
        //
        // ★ 这一层归一只保证**求解正确**，**不足以**让词条可用：本方法只返回 boundary，
        // 落库的 key 由调用侧那个 code 决定。key 若仍是 `xv`，而查询侧
        // （`convert_with_opts`）已把击键归一成 `xu`，就成了「加得进、打不出」。
        // 落库前的码归一因此必须由调用侧另做一次——见 wind-coordinator 的
        // `user_words_api::normalize_add_code`。
        //
        // ⚠️ 此处**有意不按 `shuangpin` 短路**（与 `normalize_gate_input` 相反）：
        // 入参 `code` 是**词典码域**（恒为全拼），不是击键域。双拼方案的词典码同样是
        // `xu`，故双拼引擎在这条路上也要归一。
        let code = spelling::normalize_u_umlaut(code);
        let code = code.as_ref();
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
        let Some(sol) = generate::boundary_by_char_count(idx, &self.trie, code, text) else {
            return BoundaryResolution::Unresolvable;
        };
        // 读音表整体缺席（text 含符号/外文等无读音字符）⇒ 切分已定但验证不了。
        // ★ 必须先于 `ambiguous` 判：那一档的语义是「已按读音权重择一」，这里没有权重可择。
        // 层 3 在此也走不到——`generate_word_pinyin` 对含无读音字符的 text 必返回 None。
        if sol.no_reading {
            return BoundaryResolution::NoReading(sol.mask);
        }
        if !sol.ambiguous {
            return BoundaryResolution::Derived(sol.mask);
        }
        // 层 3：仅在多解时出场——推导码与目标码逐字相同，则其切分是确定的。
        if let Some(spaced) = generate::generate_word_pinyin(&self.dict, idx, text) {
            let (flat, derived) = wind_store::wdict::split_spaced_code(&spaced);
            if flat == code && derived != 0 {
                return BoundaryResolution::Derived(derived);
            }
        }
        BoundaryResolution::Ambiguous(sol.mask)
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
        let prefix = &self.normalize_gate_input(prefix);
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
        let prefix = &self.normalize_gate_input(prefix);
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
        let (completed, _) =
            self.contiguous_completed_from_start(&self.normalize_gate_input(prefix));
        completed.iter().skip(1).any(|s| s.len() == 1)
    }

    fn completed_syllable_count(&self, prefix: &str) -> usize {
        self.contiguous_completed_from_start(&self.normalize_gate_input(prefix))
            .0
            .len()
    }

    fn shuangpin_units(&self, keys: &str) -> Vec<(String, usize, usize)> {
        self.shuangpin
            .as_ref()
            .map(|c| {
                c.convert(keys)
                    .syllables
                    .into_iter()
                    .map(|s| (s.pinyin, s.raw_start, s.raw_end))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn fuzzy_siblings(&self, syllable: &str) -> Vec<String> {
        if !self.fuzzy_config.any_enabled() {
            return Vec::new();
        }
        fuzzy::FuzzyMatcher::fuzzy_variants(syllable, &self.fuzzy_config)
            .into_iter()
            .filter(|v| v != syllable && syllable::STANDARD_SYLLABLES.contains(&v.as_str()))
            .collect()
    }

    fn shuangpin_full_syllable_count(&self, keys: &str) -> Option<usize> {
        self.shuangpin.as_ref()?.full_syllable_count(keys)
    }
}

#[cfg(test)]
mod step4_skip_tests;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
