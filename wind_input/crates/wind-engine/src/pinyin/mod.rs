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
/// 仅诊断用：定位偶发的 convert 长耗时落在哪一段，定位完毕可整块移除。
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

/// 整句解的等效词频：把 Viterbi 路径分换算回词频域，与其余候选同量纲。
///
/// ```text
/// W_eff = exp(log_prob / n + ln T) = (∏ f_i)^(1/n)    （n = 词数，T = DICT_TOTAL）
/// ```
///
/// 取几何平均而非 librime 的不归一化 `∏f_i`：后者落进 `i32` 会饱和，整句之间失去可比性。
/// 下界 clamp 到 1，避免与「无权重」候选混淆。`log_prob` 含各类惩罚，`DICT_TOTAL` 实为
/// 标定系数，改那些惩罚常数须重新标定。
///
/// 「没有词也强制组句」不靠 weight，靠协调器比较链首键 `consumed_length`；若它不再是首键，
/// 须重新评估本公式。已知偏差：`/n` 会摊薄模糊音罚与 `AMBIGUOUS_PENALTY`（与
/// [`fuzzy_penalized`] 的 `0.5^k` 不一致），已评估不修。依据与重新考虑的触发条件见
/// `docs/design/sentence-weight-same-axis.md` §4.3、§7.5。
fn sentence_weight(log_prob: f64, word_count: usize) -> i32 {
    let n = word_count.max(1) as f64;
    let geometric_mean = log_prob / n + lattice::DICT_TOTAL.ln();
    geometric_mean.exp().clamp(1.0, i32::MAX as f64) as i32
}

/// step 2 首位整句（同文合并后也带着这个身份）。
fn is_step2_top(c: &Candidate) -> bool {
    c.is_sentence && c.sentence_rank == 1
}

/// 简拼候选的保底配额分母：截断时至少给 `is_abbrev` 留 `max_candidates / 此值` 席
/// （`max_candidates = 300` ⇒ 30 席）。见 [`truncate_with_quotas`]。
///
/// 比混输 `PINYIN_QUOTA_DIVISOR`（5）宽松：简拼产出在上游已被 [`ABBREV_INDEX_LIMIT`]
/// 与逐段校验限流，30 席大概率打不满，而配额只补不挤空，放宽代价有限。
/// `max_candidates < 10` 时配额为 0（混输子引擎探测、临拼、repl 的小 limit），本就只取头几条。
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
/// `is_abbrev` 是 [`wind_candidate::cmp_match_layers`] 的第二键，简拼候选几乎沉底；
/// 模糊音等把候选总数推过 `max_candidates` 时会被整批截掉。沉底是对的（质量确实低于
/// 全拼精确匹配），被截掉不对，所以用配额而不是改排序。与 `mixed/engine.rs` 的
/// `truncate_with_pinyin_quota` 同构；这是按 `is_abbrev` 的单档配额，不是按消费长度分档。
///
/// 只补不挤空：尾部没有简拼候选时一条都不挤（`quota_does_not_displace_when_no_abbrev_candidate`）；
/// 由 `quota <= max` 知腾位必然腾得满，结果长度恒 `<= max`。
///
/// 补进来的候选追加在尾部、不保证有序，依赖调用方重排（主输入与临拼走
/// `candidate_display_order`）。不重排的特殊模式 / overlay 须用
/// [`ConvertOptions::no_abbrev_quota`] 关掉配额。本函数只负责「让候选进得来」。
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
    // 排在简拼之后的只有 `fp_demoted` 低置信降级候选，优先挤掉它们是有意的取舍。
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

/// 模糊音命中的权重折扣，每处模糊拼写乘一次（对齐 librime `kFuzzySpellingPenalty` 与
/// libime `fuzzyCost`，均为每个模糊拼写 ×0.5 并累乘）。
///
/// 是惩罚而非层级：模糊命中是召回来源，不是匹配质量，与精确命中同层按权重竞争；做成层级键
/// 会让模糊候选沉到截断线之外，等于未实现。用乘性而非加性：weight 是词频量纲且跨来源差异极大，
/// 固定减法在不同量级上效果天差地别。取 0.5 的代价是同音高频字可能压过精确命中（`si` 出「是」在前）。
///
/// 词图层用 `lattice.rs` 的 `FUZZY_SYLLABLE_LOG_PENALTY`（= `ln(0.5)`）施加同一惩罚，两处须同轴。
const FUZZY_WEIGHT_SCALE: f64 = 0.5;

/// 用户/临时层的简拼命中遇到同文已有候选时：合并进去，而不是整条丢弃（丢弃会让学过的词
/// 排位与没学过一样，右键删除也找不到记录）。
///
/// 整串简拼（step 6.1）与前缀回退（`recall_abbrev_prefix` ③④）共用，规则同全拼 step 6：
/// - `weight` 取 max，且只在已有候选本身在简拼层（`is_abbrev`）时才动——跨层的同文是另一种
///   解释，用户权重不该借合并越层；
/// - 来源标记按命中的实际归属置位，存储码随之带走（右键删除按标记选表、按码拼 key）。
///
/// 刻意不清 `is_synthesized`：它决定上屏后走 `learn_phrase_on_commit`（按造词码写、已存在即
/// count++），协调器 6b 对刚由造词写过的码会跳过以防 +2。保留标记，计数与边界补写就始终由
/// 造词那一路负责。
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
/// 两个标记要分别并入，写成 if/else 会让右键删除只删掉一半。两个标记都没有的命中按用户层算。
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
/// （没有/目前）之前，对齐主流输入法「首字优先」。取 1e7：高于单字基础权重上限（~2e6）。
/// 改的是 weight，才能穿过协调器按权重的重排。仅裸声母（syllables 为空）时应用——完整音节
/// 输入的单字已靠 is_prefix 层级就位。
const BARE_INITIAL_SINGLE_CHAR_BOOST: i32 = 10_000_000;

// 用户/临时词长词上浮的「距词尾」上限读配置 `completion.max_extra_syllables`（见
// `should_promote_user_completion`）。它与系统词库的 [`COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES`]
// 是两回事，别合并：这边问「用户词还差几个音节打完」，那边问「系统词补全预测了多少未输入内容」
// （还要过 [`COMPLETION_FAR_WEIGHT_FLOOR`]）。见 `promote_user_completion_thresholds`。

/// 系统词库前缀补全无条件上浮进完整匹配层的最大距离（候选音节数 - 已完成音节数）。
///
/// 距离 1 = 补完手头正在输入的这个音节，置信度天然高；距离 ≥ 2 是在预测尚未输入的内容，
/// 必须过 [`COMPLETION_FAR_WEIGHT_FLOOR`]。取 2 会让冷僻长词豁免门槛登顶，首选长度逐键跳动；
/// 依据见 docs/architecture/candidate-sorting-rules.md §4.2「上浮距离收到 1」。
const COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES: u32 = 1;

/// 近距离补全的上浮名额：`distance <= COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES` 的
/// 补全按权重降序最多放行这么多条进完整匹配层，其余沉回前缀层。
///
/// 没有名额时，一个残码音节能补出几十条同距离的词，把单字整批压到几十位之后（`meiy` 的「没」）。
/// 用名额而不是权重门槛：近距离补全按词频卡会误沉大量低词频的日常词（见
/// [`COMPLETION_FAR_WEIGHT_FLOOR`]）。取 8：占满首页仍有余量，单字最迟在第二页露面。
const COMPLETION_NEAR_PROMOTE_LIMIT: usize = 8;

/// 远距离补全的权重门槛：超出近距离的补全属于「预测用户尚未输入的内容」，
/// 需足够高频才配上浮，否则沉回前缀补全层级（仍在候选中，只是排到精确匹配之后）。
///
/// 取 100 落在合理长词与条文名类噪音之间的空隙里，依据见 docs/architecture/candidate-sorting-rules.md
/// §4.2「拼音常量的取值依据」。不能对近距离补全也套这道门槛：词库中位词频仅约 200，
/// 会误沉大量高频使用但低词频的日常词。
const COMPLETION_FAR_WEIGHT_FLOOR: i32 = 100;

/// step 6.5b「整句让位于用完残码的补全」的置信度下限（按折后权重标定）。
///
/// 缺了它，码上挂着的任何冷僻词（w=0）都能把整句顶掉。让位是二值开关，把连续比较压成布尔，
/// 就得自己补回门槛。与 [`COMPLETION_FAR_WEIGHT_FLOOR`] 是同一条线：6.5b 的候选距离恒为 1、
/// 折扣恒 `0.5^1`，故折后 50 等价于原始 100。
const SENTENCE_YIELD_WEIGHT_FLOOR: i32 = COMPLETION_FAR_WEIGHT_FLOOR / 2;

/// step 6.5b 的反向闸门：补全侧最强者弱于此值时，残码整句（step 2c，本身就消费了整串）
/// 不让位（配合 [`SENTENCE_KEEP_MAX_COMPLETED_SYLS`] 使用）。
///
/// 补全侧越弱，越说明该码上的正解在词库里缺频（如 w=0 的「在吗」，补全路径给不出），此时
/// 整句解码更可信。判据用补全侧的绝对词频，因为它单轴且不受语法模型影响；拿整句分数比倍数
/// 会被 grammar 平移而失效。被否决的判据与 2500 的标定见 docs/architecture/candidate-sorting-rules.md §4.2
/// 「拼音常量的取值依据」，标定探针 `grammar_ratio_calibration`。
const COMPLETION_WEAK_CEILING: i32 = 2500;

/// [`COMPLETION_WEAK_CEILING`] 的适用范围：只在已完成音节数 ≤ 此值时允许整句反压补全。
///
/// 经验性的范围限制：「1 个完整音节 + 残码」上下文最短，是词频缺失的重灾区；已完成 ≥ 2 个
/// 音节时补全更可信。放到 2 会打破 `beijingd`→「北京的」、`zhongguorenm`→「中国人民」
/// 两条断言（补全词频低于门槛，错解翻上首位）；放宽前须重跑标定探针。
const SENTENCE_KEEP_MAX_COMPLETED_SYLS: u32 = 1;

/// 前缀补全的每音节权重折扣：候选每比已输入内容多出一个音节，权重打对折。
///
/// 只有 [`Candidate::is_promoted_completion`] 这个布尔层级时，层内只比裸词频，多补 1 个与
/// 多补 3 个音节同等对待，冷僻长补全会压过短词与单字。0.5 对齐 librime `kCompletionPenalty`
/// 与 libime `overLengthCost`（见 docs/architecture/candidate-sorting-rules.md §4.2「补全折扣」）。
///
/// 本折扣不替代上浮层级：删掉层级会让 `meiy`→「没有」重新被数百个单字淹掉。
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

/// 前缀补全的取数上限（见 convert 中 `completion_limit` 处的说明）。
///
/// 取 1000 是 `push_unique` 的 O(n²) 查重（1000 条约 3.5ms，5000 条约 54ms）与「单字母可持续
/// 翻页」之间的平衡点。只约束补全，精确匹配候选不受影响。
const MAX_COMPLETION_CANDIDATES: usize = 1000;

/// 查 `AbbrevSection` 时的取码上限。纯简拼（step 5 / 回退①）与混合简拼（step 5b / 回退②）
/// 共用同一个值：查的是同一张表、同一个键，窗口不一致会让打得更省事的写法反而查得更少，
/// 且截断在召回层，调频与开大候选窗都救不回来（见 `tests/pinyin_abbrev_recall_window.rs`）。
/// 混合路径的码还要过逐段校验、绝大多数被滤掉，取小了几乎一条不剩。
///
/// 已知上界：只有单键上限，没有总额（成本约 `变体键数 × 本值 × 每码查询`）。真实词库下
/// 最坏不到 1ms，余量靠数据分布而非代码保证；词库形态变了先重跑
/// `tests/pinyin_abbrev_recall_latency.rs`。
const ABBREV_INDEX_LIMIT: usize = 64;

/// 混合整句的质量闸门：路径平均每字 log_prob 低于此值就不插入候选。
///
/// 混合整句的简拼段歧义极大，靠 unigram 常常选错；没有闸门时错误整句会占据首选。
/// -8.0 是零代价点：整句命中与不设闸门相同，只挡掉最离谱的那批；正误分布在 -5 ~ -6.5
/// 大幅重叠，收紧会砍掉大量正确整句。语法模型开启后仍成立（平移双向，被扣分的整句本就远在
/// 阈值以下）。依据见 docs/architecture/candidate-sorting-rules.md §4.2「拼音常量的取值依据」；改本值须重跑
/// `pinyin_eval` D 类受控对比与 `tests/grammar_axis_shift.rs`。
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
/// 默认值参考主流输入法（微软拼音中间级不限条数，豆包 3 字不限、2 字 8 条）。
#[derive(Debug, Clone)]
pub struct AbbrevFallbackConfig {
    /// 前缀回退**参与竞争的切点数**（有产出的才计数）。
    ///
    /// 默认 2 = 「最长 + 次长」。相邻切点的竞争才是真实场景（`bzdhaobuhao` 里 `bzdh`→「表彰大会」
    /// 与 `bzd`→「不知道」），再往下只会引入越来越短、越来越不相关的解释。
    ///
    /// 整串无命中时放宽它已被评测否决：短切点的高频词会占据首选，把长切点的正确候选挤出前排
    /// （依据见 docs/architecture/candidate-sorting-rules.md §4.2「拼音常量的取值依据」）。改它先重跑 pinyin_eval。
    pub cuts: usize,
    /// 切点长度 ≥ 3 个字母（三字词组及以上）时，单切点、**单个来源层**的产出上限。
    ///
    /// 系统层与 store 层各记各的（`recall_abbrev_prefix` 的 `start` / `store_base`），故单切点
    /// 实际上限是 2× 本值；共用一份时用户自己造的词会被系统高频词挤出。调值时对照候选规模
    /// （见 `mixed_partial_pinyin_filter.rs`）。受 `ABBREV_INDEX_LIMIT` 约束。
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
/// 刻意远小于主路径的 `completion_limit`：本支路产出恒沉在双拼候选之后，堆再多只是拉长翻页，
/// 而候选查重是 O(n²)，支路不该往这个瓶颈上加码。
const MAX_FULL_PINYIN_RECALL: usize = 24;

/// 全拼降级闸门要求的最少完整音节数。
///
/// 双拼击键常拿 v/i/u 作声母，全拼音节表切不动，2 个音节足以挡掉 `nihc`/`womf` 这类双拼串；
/// 降到 1 等于取消闸门（任何双拼串的前两键几乎都能读成一个音节）。代价：全拼用户打到
/// `nih`（1 音节 + 残码）时支路不启动，要打满 `niha` 才出候选。
const FULL_PINYIN_MIN_SYLLABLES: usize = 2;

/// 全拼降级支路里每级前缀子短语（②）的召回上限。
///
/// 没有它，单音节前缀的同音单字（`zai` 一级就有 20+ 条）会吃光 [`MAX_FULL_PINYIN_RECALL`]，
/// 精确整词与前缀补全反被挤掉。子短语只解释开头一截、双拼流通常已给出同批单字，限紧些不损失什么。
const MAX_FULL_PINYIN_SUBPHRASE: usize = 4;

/// 用户/临时词的前缀补全是否上浮进完整匹配层（`is_promoted_completion`）。不上浮时它会被首音节
/// 的同音子短语整层压到候选最底。
///
/// `started` = 完整音节数 + (有残码 ? 1 : 0)，尾部残码算已起头的一个音节：
/// - 算得出音节数：`started ≥ 2` 且距词尾 ≤ `max_extra`（`completion.max_extra_syllables`）才上浮；
/// - 算不出：退化为 `started ≥ 3`，避免 1-2 音节时被冷僻长词占满前排。
///
/// `word_syls` 必须是 [`effective_boundary`] 现切的结果，不能是 `boundary.count_ones()`：
/// 手输码用户词恒 `boundary = 0`，但码能现切出音节数；读裸 `boundary` 会让它与召回门口径不一，
/// 召回了却按「无信息」沉底。传算好的音节数也让调用点复用已算的结果，并避免拿 A 层的码配
/// B 层的边界。
///
/// 距词尾上限必须读配置、与召回层同口径：不上浮的用户词沉到最底，而引擎 `sort_by` 后紧跟
/// `truncate`，候选数超过上限时等于被丢弃。召回得进来的用户词，就该上浮得起来。
///
/// 别拿 `remaining <= max_extra` 在主路径上写护栏：step 6.3 的 `retain` 用同一个不等式先裁过，
/// 主路径上它恒真。要验「距词尾太远不上浮」只能走 step 6.7 全拼降级支路（那里刻意不受
/// `completion_syllable_cap` 约束）。
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
        // 音节数算不出来：退化为「打到第 3 个音节才给」。只兜得住一个音节都切不出的码；
        // 码切不满（`daboluoqq` → `da|bo|luo`）走上面那条、拿低估的音节数判，见 `effective_boundary`。
        started >= 3
    }
}

/// 拼音引擎配置
#[derive(Debug, Clone)]
pub struct Config {
    pub use_smart_compose: bool,
    /// 是否产出简拼候选（声母缩写，nh→你好）。默认 true。
    ///
    /// 混输经 `schema.mix.enable_pinyin_abbrev` 关闭它：简拼让「几乎任何字母串都可能是拼音」
    /// （`is_abbreviation` 只要求每字母是某音节首字母），而混输里有人只拿拼音做临时输入补位。
    /// 关闭时连简拼族的召回一并省掉（step5/5b/6/6.2 整条支路）。
    pub enable_abbrev: bool,
    /// 是否让**尾部残码**参与整句解码（step 2c，`buzhidaok`→「不知道看」）。
    /// 默认 true = 纯拼音方案的行为。
    ///
    /// 混输下恒 false：码长内的击键串同时是码表码，整串当拼音组句是过度解读（五笔 `aaw`
    /// 的残码被补成「我」后整句消费满整串，抢走首位）。超码长那一侧由调用方经
    /// [`crate::engine::ConvertOptions::allow_partial_final`] 覆写为放行，判据是「这串还可能
    /// 是码表码吗」。
    ///
    /// 不要复用 [`Self::enable_abbrev`] 当判据：一个问「整串按声母读成简拼」，一个问「把最后
    /// 半个音节猜完」，语义正交，只是恰好都在混输时关。
    pub enable_partial_final: bool,
    /// 词组补全的音节数约束：至少输入几个音节才给词组（见 `[schema.pinyin.completion]`）。
    /// 补全词的音节数恒 ≥ 输入音节数，故未达门槛时上限收紧到输入音节数本身，
    /// 效果即「只出同音节数的候选」。取 1 = 不设限。
    pub completion_min_syllables: u32,
    /// 词组补全的音节数约束：候选最多比输入多几个音节。
    pub completion_max_extra_syllables: u32,
    /// 是否让用户上屏过的词（用户词 + 临时词）参与整句解码（S2）。出厂 true。
    ///
    /// 关闭时整句词图只从系统词库建，用户自造词永远不会成为整句的一段（单独打得出、
    /// 放进句子里就被打散）。已知局限：出厂只出一条整句，用户词进图是赢者通吃；会改变所有
    /// 用户的整句结果；用户词 weight 只做了上限截断（[`USER_NODE_WEIGHT_CAP`]），没有做分布对齐。
    /// 只在 `use_smart_compose` 开着时有效。
    ///
    /// 接用户词与临时词，不接草稿层：草稿「用过即转正」才是质量闸，杂词直接进图会污染整句。
    /// 临时层也收分步上屏拼接与选中的合成整句（不经草稿），分错段的一次上屏会以整节点进图
    /// 自我强化，见 `lattice::add_store_nodes` 约束 1。
    ///
    /// 全拼段点查全拼码（`lattice::add_store_nodes`），简拼整句（②b）的声母段另查用户层的
    /// 简拼索引（`lattice::add_store_abbrev_nodes`）。
    pub sentence_uses_user_words: bool,
    /// 整句 N-best：候选列表里露几条整句（step 2 主整句）。出厂 1。
    ///
    /// 调到 >1 后次优整句也进候选，协调器把整句块连续摆到最前（`sentence_rank`）。
    ///
    /// 只作用于 step 2 主整句。2b 混合整句 / 2c 残码整句仍各出一条：它们是特殊形态的
    /// 补充解读（2c 连消费长度都与主整句不同层），多解意义不大、叠在一起反而难辨认。
    pub sentence_count: u8,
    /// 整句 N-best：解码时**算**几条（切换键的池子）。`>= sentence_count`，不足时按后者抬。
    ///
    /// 算得多、露得少：候选列表里露 `sentence_count` 条，切换键在这 `sentence_max_count`
    /// 条里滚动窗口，切换时不必重新解码。
    pub sentence_max_count: u8,
    /// 简拼前缀回退的内部限额，见 [`AbbrevFallbackConfig`]。
    pub abbrev_fallback: AbbrevFallbackConfig,
    /// 双拼方案下是否额外把击键串当全拼解释一遍（`nihao` → 「你好」），服务多人共用一台机器。
    /// 产出的候选整体沉在双拼候选之后（[`wind_candidate::Candidate::is_fullpinyin_fallback`]），
    /// 对双拼用户是纯增量。只在 `shuangpin.is_some()` 时有意义。
    ///
    /// 混输强制关闭（`manager.rs` 接线处），理由同 [`Self::enable_partial_final`]；且混输不接
    /// 双拼（见 `mixed::MixedEngine::pinyin_may_continue`）。
    pub allow_full_pinyin: bool,
}

/// 前缀补全允许的最大候选音节数 —— 词组补全两个旋钮（`[schema.pinyin.completion]`）
/// 合成的那一个数。
///
/// `started` 是输入自身表达的音节数（完整音节 + 残码算起头的一个）。未达
/// `min_syllables` 时收紧到 `started`：补全词的音节数恒 ≥ `started`，故这等价于
/// 「不给词组」，但表述成同一个上限，使词库层下推也只需要它。
///
/// 判据问的是「输入有几个音节」，不是「输入在这条候选的切分下占几个音节」：后者会让 `xia`
/// 因存在 `xi|an` 切分而被当成 2 音节、整批放行词组。对前缀补全而言输入音节数唯一确定
/// （`completed_len` 是词图的性质）；精确匹配不预测未输入内容，本就不需要这道闸门。
///
/// 也不能拿「是不是前缀补全」当唯一判据：`d` 的单字与词组出自同一条 step4，按来源整条关掉
/// 会让 `d` 零候选。`is_prefix` 圈定适用范围，音节数决定去留。
///
/// `min_syllables` 取 3 会伤到 `nih`→「你好」、`meiy`→「没有」（`started` 都是 2）。
/// `max_extra` 单靠 extra 分不开想要的长补全与不想要的（音节维度同形、weight 也分不开），
/// 缺的是「该前缀下有没有竞争者」这一判据，目前只能由用户按口味取舍。
fn completion_syllable_cap(started: u32, min_syllables: u32, max_extra: u32) -> u32 {
    if started < min_syllables {
        started
    } else {
        started.saturating_add(max_extra)
    }
}

/// step 4 的词库前缀补全在本次输入下注定只剩精确匹配，可整段跳过。
///
/// 成立条件：整音节输入（无残码）且 `syllable_cap <= 1`（出厂下的单音节输入）。此时词库层
/// `prefix_entry_keep` 的两半互相矛盾：对齐判据要求在 `completed_len` 处再开一个音节（至少
/// 2 个），音节数判据又不许超过 1，能同时满足的只有 step 1 已给出的精确匹配。唯一漏口是码长
/// ≥ 64 字节的无边界条目，本就违背 cap。省下的是整棵前缀子树的扫描（`xi` 约数十毫秒），
/// 对拍见 `step4_skip_tests`。
///
/// 不能放宽到多音节整音节输入：切分来自手动分隔符或双拼时可比词条更细，`xi'an` 下
/// `xian|ren` 这类补全能同时过两条判据。残码位与 `cap > 1` 时单音节补全合法，也不能跳。
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

    /// `true` = 每次都扫，用作对拍的基线。
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
            // 与 `wind_config::PinyinCompletion::default()` 保持同值：真实路径从 wind-config 传入，
            // 本处只在纯引擎构造时兜底，两处分叉会让引擎单测假绿。取值理由见 wind-config 侧。
            completion_min_syllables: 4,
            completion_max_extra_syllables: 5,
            // 默认关：全拼降级是显式开启的降级通道，不该在任何未声明的地方生效
            // （测试要覆盖支路时显式置 true，这样"支路参与了哪些用例"一目了然）。
            allow_full_pinyin: false,
            // 与 `wind_config::PinyinGlobalConfig` 的默认值保持同值，同 completion 两项。
            sentence_uses_user_words: true,
            // 出厂 1/1 = 单条整句，K-best 在 K=1 时与单路径 DP 逐位相同。
            // 与 `wind_config::PinyinGlobalConfig` 那两项保持同值。
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
    /// 引擎配置（use_smart_compose 等）
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
    /// 没有它，`;` 不算码元，会被协调器分派给次选键 / 引导键 / 标点流水线，`ing` 打不出来。
    /// 全拼与「布局全是字母」时为 `None`，协调器回落内置 `a-z`。
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
    /// 词图打分用词条自身的词典权重（见 `lattice::score_node_inner`）；词间搭配另由
    /// [`Self::with_grammar`] 挂的上下文模型提供。
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

    /// 注入模糊音配置（取代构造时的 `FuzzyConfig::default()`）。
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
    /// 闸门判据落在音节 Trie 上，不先归一化（`xv`→`xu`），混输下这串键就进不到拼音引擎。
    ///
    /// 与 `full_pinyin_gate` 有意不同：双拼击键里的 `v` 是布局键，归一化会毁掉它，故按
    /// `shuangpin` 短路，而不是把归一化下沉进 `contiguous_completed_from_start`。
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
    /// 切的是击键串，不是候选的 code：渲染 code 会多出用户没敲的字母，退格与光标编辑随之错位。
    /// 用候选自带的真值音节序列去切 `raw_input`（见 `render_keystroke_preedit`）。宁可返回
    /// `None` 少一个分隔符，也不给出与击键长度不符的串。
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
            // 部分匹配（step 6.2 前缀回退）：余下的击键自己再切一遍，别整段追加
            // （`bzdnihaob` → `b'z'd'ni'hao'b`，而不是 `b'z'd'nihaob`）。
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
        // 按所打码切分显示成 `gogn'ping`；关纠错时这一支结构上不可达。末尾半截纠错（`konglon`
        // 的「恐龙」，不标纠错）同样码即所打码，不跟上会显示成 `kong'lo'n`。
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
            // 混合整句（step 2b）：code 是整串击键、boundary 也在击键空间（简拼段每字母一位），
            // 得 `b'z'd'hao'bu'hao`。这类输入从位置 0 就切不出完整音节，`completed` 恒为空。
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
    /// `consumed_length` 要回映射到含 `'` 的原始输入空间，需要的正是这些偏移。
    fn spans_with_separators(&self, input: &str) -> Vec<interp::SylSpan> {
        interp::spans_for_full_pinyin(input, &self.trie)
    }

    /// 由全拼码取简拼（各音节声母拼接），供用户/临时造词层的简拼**判据**使用。
    ///
    /// 这是判据，不是召回：候选集由声母索引给出（`wind_store::abbrev_index`，键就是本函数
    /// `boundary != 0` 分支的产物）。两侧对 boundary 的解释必须逐位一致，否则索引键永远
    /// 匹配不上查询，表现为简拼一条都召不回而不是报错。
    ///
    /// 优先采信候选自带的 `boundary`（音节起始字节位 bitmask，造词/词库解析期的真值）。
    /// 仅当 `boundary == 0`（旧数据、手输码）才退回切分去猜，而重猜在歧义切分码上必错
    /// （「西安宁」`xi|an|ning` 应给 `xan`，`maximum_match` 给出 `xn`）。
    /// 切分未完全覆盖 code 时返回 None。
    ///
    /// 混合简拼的 [`mixed_abbrev::syllables_from_boundary`] 对同一个 `boundary` 做另一半
    /// （要整段音节），两者解释必须一致，改动其一时同步核对另一处。
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

    /// S2：把用户词与临时词追加进词图（`sentence_uses_user_words`）。三条整句通路共用这一个入口，
    /// 开关判定收在这里，新增通路时才不会漏判。
    ///
    /// 必须在 `LatticeBuilder::build` 之后调：同词同起点时要拿用户词的分去更新既有节点（见
    /// `add_store_nodes` 的第三条约束）。`require_reachable` 传与同一张图的 `build` 相同的值。
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

    /// 简拼族召回，但只针对击键串的一个前缀（step 6.2 的前缀回退用）。
    ///
    /// `stroke` = 击键串的前 `consumed` 个字节；产出的候选只消费这么多击键，余下的留给下一次
    /// 转换（分步上屏）。四条来源与整串路径一一对应：纯简拼系统词（step5）、混合简拼系统词
    /// （step5b）、二者的用户词版本（step6）。
    ///
    /// 与那三处是重复实现，改判据时必须同步。不合并是有意的取舍：那三处各有专门的回归测试，
    /// 抽成公共函数的回归风险大于重复三十行的维护成本。
    fn recall_abbrev_prefix(&self, stroke: &str, consumed: usize, cands: &mut Vec<Candidate>) {
        let trie = &self.trie;
        let dict = &self.dict;
        // 本切点的产出配额：不逐切点限流，长切点会把额度占满。系统层 ①② 与用户/临时层 ③④
        // 各记各的基准（`start` / `store_base`）：共用一份时系统高频词占满席位，用户词在长输入
        // （正是分步上屏、自动造词的场景）下静默出局。见 `tests/pinyin_abbrev_recall_window.rs`。
        let start = cands.len();

        // 部分候选统一形态：`is_abbrev` 归入简拼层、`is_partial` 表示只覆盖了输入前缀，
        // `consumed_length` 自带击键域的消费数（统一计算 consumed 的循环会跳过它们）。
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

        // ① 纯简拼系统词（同 step5，含「音节数 == 简拼字母数」过滤，boundary=0 走 DAG 兜底）。
        // `enable_abbrev` 已由调用方把关，此处只判形态；`plain` 下面 ③④ 复用，不重算。
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

        // ② 混合简拼系统词（同 step5b）。`stroke` 已是完整音节序列时属全拼语义，跳过。
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
                        // 同一个键下常有多条解释命中，取模糊处数最少的那条计罚（按最有利的解释算）。
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

        // ③④ 用户/临时造词层的纯简拼与混合简拼（同 step6 末段）。声母索引只保证投影对得上，
        // 音节数、逐段全等仍在下面逐条判。见 `DictLayer::search_abbrev`。
        if let Some(store_dm) = &self.store_layers {
            // 用户/临时层自己的配额基准（见 `start` 处的说明）：不与系统层 ①② 抢同一份。
            let store_base = cands.len();
            for c in self.recall_store_by_abbrev(store_dm, stroke, plain, &pats) {
                let plain_edits = self.abbrev_matches_stroke(&c.code, c.boundary, stroke);
                // 只有纯简拼精确命中（`Some(0)`）才能跳过混合解释：变体键命中带着处数，
                // 混合解释可能更优。
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
                // 取处数最少的解释计罚（同 step5b）；不能用 `or`，它取的是左边而非更优的那个。
                // 全拼下 min 恒等于 `plain_edits`，但双拼 + 手动分隔符下 plain 与 mixed 不同域、
                // 段与击键位不再一一对应，那条推理不成立，故保留 min。
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
    /// - 混合简拼：每条模式的声母投影键（`nhao` = n + hao → 投影键 `nh`），与系统词库
    ///   `AbbrevSection` 同一套键（见 `mixed_abbrev` 模块文档）。
    ///
    /// `plain` 与 `pats` 必须由调用方传入、不得在此重算：调用方的闸门含 `config.enable_abbrev`，
    /// 只重算 `is_abbreviation` 会让关了简拼的混输照样出候选。`plain` 判假时仍可能有混合解释
    /// （`nih`），故入口条件取或。
    ///
    /// 返回的是超集：索引只保证声母投影相等，判据一律留给调用方（纯简拼比对整串、混合简拼
    /// 逐段校验），改索引不该改变简拼的语义。
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
    /// 入参必须是原始击键（`raw_input`），不是双拼转换后的全拼：传错域会让闸门恒真。
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
    /// 整句解码（⑤，含自带的「整句让位于精确整词」）。模糊音跟随 `[schema.pinyin.fuzzy]`，
    /// 与双拼流同一套设置（两条流表现不一致本身就是缺陷）。
    ///
    /// - 不含简拼：简拼判据本就走击键域，主路径已覆盖；
    /// - 不含残码补全整句（主路径 step 2c）：它让整句消费满整串、跨过若干「消费整串」闸门，
    ///   副作用面大于收益（同见 [`crate::engine::ConvertOptions::allow_partial_final`]）。
    ///
    /// 本支路的全拼域与击键域是同一个域，`consumed_length` 直接取码的字节数；绝不可再过
    /// `map_consumed_length`（双拼流专用的回映射），否则二次换算必然错位。
    fn recall_full_pinyin(&self, stroke: &str, syllables: &[String], cands: &mut Vec<Candidate>) {
        let dict = &self.dict;
        // 双拼同键音节是**键位**的属性，全拼打法没有键位：本支路只吃用户开的模糊组。
        let fz = self.fuzzy_config.without_shared_key();
        let start = cands.len();
        // 输入自身表达的音节数，口径同主路径的 `started_syllables`：完整音节 + (有残码 ? 1 : 0)。
        // 本支路两域相同，可直接按字节长度判残码；主路径两域不同，不能照搬。
        let completed_bytes: usize = syllables.iter().map(String::len).sum();
        let trailing_partial = completed_bytes < stroke.len();
        let started = syllables.len() as u32 + u32::from(trailing_partial);
        // ④ 产出的用户/临时词前缀补全（见那里的注释），末尾据此分流上浮判据。
        let mut user_prefix_texts: Vec<String> = Vec::new();
        // 同文去重保留解释得更多的那条，而不是先到者：双拼流的简拼前缀回退可能先以半截
        // consumed 占位（`zaijian` 的「再见」只吃 4 键），让位会让选中后缓冲里凭空剩下残码。
        // consumed 相等时双拼那条留下。`consumed_length == 0` 表示消费整串，不得替换。
        //
        // `meta` 原样带进候选：系统词模糊命中的造词码（同主路径 step 1/3），store 层命中的来源
        // 标记 / 存储码 / 造词码（同 step 6），否则分步组词按模糊原码造词、用户词右键删不掉。
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
                // 恒 true：这是来源标记，不是排序决策（沉不沉底由 `cmp_match_layers` 另判）。
                // 三处豁免（边界校验 / consumed 回映射 / preedit 跟随）都认它，按「是否沉底」
                // 置位会让高置信候选丢标记、豁免一起失灵。
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
        // 走 `lookup_with_fuzzy` 以支持模糊音，惩罚由其内部的 `fuzzy_penalized` 施加，与主路径同源。
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

        // ③ 前缀补全（码比击键长，`niha` → 「你好」）：以整串击键为前缀，含尾部残码。
        // 必须与主路径 step 4 同口径地过 `completion_syllable_cap`，否则 `[schema.pinyin.completion]`
        // 对本支路失效。
        let cap = completion_syllable_cap(
            started,
            self.config.completion_min_syllables,
            self.config.completion_max_extra_syllables,
        );
        // 边界对齐判据同主路径 step 4：不该把 `shen` 补成 `sheng`。前缀用整串击键 `stroke`
        // （含残码），对齐位取 `completed.len()`（不含残码），两者刻意不同。
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
            // 模糊音同 ①：用户词记录的是规范码，模糊打法只能靠变体码撞上。
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
                // 音节边界对齐必须在本支路自己判一次：本支路在 step 6.3 的 retain 之后（那把尺子
                // 是双拼域的），不判的话用户词会跨越已打完的音节（`shaixu` 出「筛选」），与 ③
                // 的系统词行为不一致。另加 `effective_boundary` 兜底无边界真值的手输码。
                //
                // 有意不加 `completion_syllable_cap`：本路径的用户词一直不受音节数上限约束
                // （用户自己加的长词理应更容易召回），要动它得单独立项并配回归。
                if !wind_dict::cached::prefix_syllable_aligned(
                    effective_boundary(&c.code, c.boundary, &self.trie),
                    c.code.len(),
                    completed_bytes,
                ) {
                    continue;
                }
                // 记下文本，供末尾施加用户词专属的上浮判据。用名单而不是索引区间：`push` 的
                // 同文去重可能把本条合并到 ③ 已占的位置上，按区间切会漏掉。
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

        // ⑤ 整句解码：让全拼降级流也能组句（`wojintianhenkaixin` → 「我今天很开心」）。
        // 在 `completed` 上建图，对应主路径 step 2；不做 step 2c 的残码补全（理由见函数文档）。
        // 切分图走 `from_dag`（多路径）：全拼的切分是猜的，词图该看到全部切法，这是它与双拼
        // 主路径 `fixed_segmentation` 的分野。S2 的用户词节点也不接：降级通道收益极小，用户词
        // 进图会改变唯一一条整句结果；双拼下用自造词组句的正途是主路径 step 2。
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
            // 三道闸门，复刻主路径 step 2 / 2b 的判据：
            // - `log_prob.is_finite()`：解码失败时为 NEG_INFINITY；
            // - `words.len() >= 2`：单节点路径不算组句，包装成整句会让同一个词的词频记到两个互不
            //   相认的键上，交给 ① 出即可；
            // - `logp_per_char`：低置信整句宁可不出。
            if result.log_prob.is_finite()
                && result.words.len() >= 2
                && !sentence.is_empty()
                && logp_per_char >= MIXED_SENTENCE_MIN_LOGP_PER_CHAR
                && !cands.iter().any(|c| c.text == sentence)
            {
                // 整句让位于精确整词，语义同主路径 6.5，但须自带一份：本支路在 6.5/6.5b 之后运行，
                // 那套逻辑够不着这批候选。比较范围限于本支路自己的产出 `cands[start..]`。
                let exact_max = cands[start..]
                    .iter()
                    .filter(|c| !c.is_prefix && !c.is_partial && c.code == completed)
                    .map(|c| c.weight)
                    .max();
                let w = sentence_weight(result.log_prob, result.words.len());
                let weight = exact_max.map_or(w, |m| w.min(m.saturating_sub(1)));
                cands.push(Candidate {
                    text: sentence,
                    // 码取已完成音节段（不含尾部残码），与 consumed 一致，残码留在缓冲里续输。
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

        // 本支路产出的前缀补全统一补齐音节数对齐档位（漏设则 `cmp_completion_extra` 形同虚设）。
        // 放在末尾一次做完：push 带同文去重，逐条回填要跟去重逻辑纠缠。
        //
        // 上浮判据必须按来源分流：③ 的系统词走 `COMPLETION_UNCONDITIONAL_FLOAT_SYLLABLES` +
        // `COMPLETION_FAR_WEIGHT_FLOOR`，④ 的用户词走 `should_promote_user_completion`，统一施加
        // 任何一套都会误伤另一批。双拼方案下本支路是全拼码用户词唯一的产出通道，不上浮就会沉到
        // 截断线之外、完全打不出。
        let trie = &self.trie;
        for c in &mut cands[start..] {
            if !c.is_prefix {
                continue;
            }
            // 口径同主路径 step4/step6：`boundary == 0` 先过 `effective_boundary` 现切，
            // 而不是当「算不出」跳过（手输码用户词恒 boundary=0）。
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

    /// 简拼键的召回集：原键 + 首字母模糊变体（`n↔l` / `f↔h` / `r↔l`），带每个键的模糊处数。
    ///
    /// 简拼索引的键是各音节首字母串，改首字母的模糊组在召回侧就断了（`nqc` 捞不到挂在 `lqc`
    /// 下的「篮球场」），所以必须召回侧枚举键 + 校验侧放宽 `Initial` 一起做。`sh↔s` 等组首字母
    /// 相同，是恒等变换。变体键与原键等长，按键长做的音节数过滤不受影响。上限见
    /// [`fuzzy::fuzzy_abbrev_keys`]。
    ///
    /// 整句词图的简拼节点（`lattice::add_abbrev_nodes`）没接：那里的模糊罚分要并进词图打分轴，
    /// 属另一笔改动，故混合整句在开 `n_l` 后仍召不回 `lqc` 的节点。
    ///
    /// 只给纯简拼路径与 [`Self::abbrev_matches_stroke`] 用；混合路径走
    /// [`Self::abbrev_recall_keys_only`]（那里有分工表）。
    fn abbrev_recall_keys(&self, key: &str) -> Vec<(String, usize)> {
        fuzzy::fuzzy_abbrev_keys(key, &self.fuzzy_config)
    }

    /// 同 [`Self::abbrev_recall_keys`]，只要键、丢掉处数。刻意实现成前者的投影而非另写一份
    /// 枚举，否则两份分叉后召回面会不一致，且看起来像模式枚举的毛病。
    ///
    /// 五个召回点分两类，用错哪一类都不会报错：
    ///
    /// | 路径 | 该用 | 用错的后果 |
    /// |---|---|---|
    /// | step5 纯简拼、step6.2 前缀回退① | [`Self::abbrev_recall_keys`] | 漏罚，退回「模糊解排到精确解前面」 |
    /// | step5b 混合、前缀回退②、`recall_store_by_abbrev` | 本函数 | 罚两遍 |
    ///
    /// 混合路径丢掉键处数不会漏罚：键第 i 位变了，该段的 `matches_exact` 必然失败，校验侧
    /// 必计处数，且比键更准（`sen → sheng` 计 2 处）。
    fn abbrev_recall_keys_only(&self, key: &str) -> Vec<String> {
        self.abbrev_recall_keys(key)
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    }

    /// 词条的声母串与用户敲的简拼 `stroke` 的匹配**处数**：`Some(0)` = 精确，
    /// `Some(n)` = 经 n 处首字母模糊匹配上，`None` = 不匹配。
    ///
    /// 不能只做精确比较：召回侧经变体键捞回的词会在这里被扔掉。返回处数而不是 `bool`：
    /// 调用方要施加 [`fuzzy_penalized`] 并标 `is_fuzzy`，否则模糊命中会被当精确命中送进候选。
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

    /// 混合简拼段校验的音节比较：精确相等返回 `Some(0)`，模糊变体命中返回
    /// `Some(改动处数)`，不匹配返回 `None`。
    ///
    /// `seg` 是用户敲的那一段、`syl` 是词典里的音节，方向不可颠倒（与 `fuzzy_codes` 同向）。
    /// 返回处数而不是 `bool`：调用方要施加 [`fuzzy_penalized`] 并标 `is_fuzzy`，否则精确命中会被
    /// 权重更高的模糊命中压过（模糊恒低精确一档是全仓不变量，见 [`FUZZY_WEIGHT_SCALE`]）。
    ///
    /// 已知边界：模糊命中拿不到击键分段 preedit（`render_keystroke_preedit` 精确对齐击键），
    /// 退回通用切分，失败是安全的。`Initial` 段只对共享首字母的声母组等价于模糊放宽。
    /// 段校验几乎每条待校验候选都会触发变体展开，开销随键下条目数线性增长（最坏约 0.3ms），
    /// 可把变体预算进 `MixedPattern` 消掉，尚未做。模糊音全关时由 `any_enabled()` 短路。
    fn seg_matches_fuzzy(&self, seg: &mixed_abbrev::AbbrevSeg, syl: &str) -> Option<usize> {
        use mixed_abbrev::AbbrevSeg;
        if seg.matches_exact(syl) {
            return Some(0);
        }
        if !self.fuzzy_config.any_enabled() {
            return None;
        }
        match seg {
            // 声母段只约束首字母，判据是首字母的模糊等价集，计 1 处（声母段最多错一处，与
            // `Syllable` 段同量纲，两者相加后一起喂 `fuzzy_penalized`）。
            AbbrevSeg::Initial(c) => {
                let first = syl.chars().next()?;
                fuzzy::initials_fuzzy_equal(*c, first, &self.fuzzy_config).then_some(1)
            }
            // 双字母声母段：放宽成「只比首字母」仅在对应卷舌组开着时成立。用
            // `initials_fuzzy_equal` 会因 zh 与 z 首字母相同而恒真、开关形同虚设。同样计 1 处。
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

        // 模糊变体命中对外一律 boundary=0：词典给的是变体码的切分，候选对外的 code 是用户输入的
        // 原码，两者不同域。词典边界只拿来和同域的变体边界核对（[`fuzzy_shape_edits`]）。
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

    /// 用户/临时造词层的模糊音召回：拿 `code` 的模糊变体码逐个查 store。
    ///
    /// 造词写库用的是规范读音码（`learn_code`），故用户按模糊打法敲出来的串在 store 里
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
    /// 在 `convert` 收尾对最终候选统一做一次，而不是散在各产出点：此时各路径的同文合并都已
    /// 落定，提示与 `learn_code` 天然出自同一条。
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
    /// 词典里登记的那个码与它自己的真值边界；`None` = 与候选 `code` 同一个（精确命中）。
    ///
    /// 模糊命中时两者不同（敲 `senri`，词典是 `shengri`）。候选对外的 `code` 必须留用户那份
    /// （`consumed_length`、preedit 跟随、词频记账都绑在它上面），造词则要词典那份及其同域边界。
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
/// 无真值的来源：用户手输码、模糊变体（一律置 0）、旧词典条目。现切是猜测，但这里本就
/// 没有真值可用（与「有真值却扔掉重猜」的戒律不冲突）。
///
/// 猜测走 `maximum_match`（最少音节），同码多切分时偏向少的那侧（`xian` 猜成 1 音节）。
/// 码切不满时同样少算且不报错：`daboluoqq` 切成 `da|bo|luo`，尾部 `qq` 被丢掉。本函数刻意
/// 不做覆盖校验（加闸会同时改变全部调用点，须单独立项），故调用方拿到的音节数对这类码是
/// 低估值而非 0，`word_syls == 0` 只挡得住空码和整串切不出的码。
fn effective_boundary(code: &str, boundary: u64, trie: &SyllableTrie) -> u64 {
    if boundary != 0 {
        return boundary;
    }
    let syls = Dag::build(code, trie).maximum_match();
    syllables_boundary_mask(&syls, code.len())
}

/// 双拼给出的**分段边界**（全拼空间 bitmask，与候选 `boundary` 同域）。
///
/// 双拼每 2 键 = 1 段，边界免费且精确，不该交给 DAG 重猜。
///
/// 回写段（拼不出合法音节、原样写进 full 的键对，不产生 `SylSpan`）也算一个段起点：它照样
/// 占据 full 的一段。让整个 mask 作废会关掉校验，`nihaoya` 的「你好呀」就从前缀补全漏网。
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
/// 典型：输入 `nihao`(5键) 双拼解释为 `ni|ha|o`，而「你好」的词典边界是 `ni|hao`，应被拒绝。
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

/// 用双拼原始按键重建 preedit（按音节边界以 `'` 分隔）。
///
/// 必须完整覆盖 `raw_input` 的每个字节：音节之间与尾部未被覆盖的字节原样作独立段。不可只在
/// `has_partial` 时补尾：原样回写的无匹配键对既不进 `syllables` 也不置 `has_partial`，会被吞掉。
/// 双拼键均为 ASCII，字节切片安全。
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

    // 逐字节重建而非 `join("'")`：raw_input 里可能已有用户手打的 `'`，join 会再插一个。
    // 判据是「前面是不是已经有分隔符」，手动与自动的天然合流，连续 `''` 与首尾 `'` 原样留住。
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

    /// 拼音引擎的唯一转换实现（`convert` 转发至此，传全默认的 [`ConvertOptions`]）。
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

        // ü 拼写归一化（`xv` → `xu`、`nue` → `nve`，见 [`spelling`]）。落在 `raw_input` 之前，
        // 让归一化形态成为唯一真相：全拼下「`raw_input == query`」是下游多处判据的前提。两条
        // 规则都是等长替换，preedit 重建与 consumed_length 不受影响。
        //
        // 仅全拼：双拼的 `v` 是布局键，换成 `u` 会毁掉双拼输入（对端在 `shuangpin::normalize_pinyin`）。
        let normalized = self
            .shuangpin
            .is_none()
            .then(|| spelling::normalize_u_umlaut(input));
        let input: &str = normalized.as_deref().unwrap_or(input);

        // 手动音节分隔符 `'` 在双拼下同样生效：由 `ShuangpinConverter::convert` 当作配对的
        // 硬边界消化（见那里的文档），到这里之后 `input` 已是纯全拼域、不含 `'`，
        // 下面全拼路径的 `has_sep` 分支只对 shuangpin=None 的输入成立。

        // 在任何 shadow 之前保存用户实际输入的原始字符（双拼键序列或全拼）。
        // 用于重建 preedit_display（显示原始按键），以及简拼判定（见 `abbr_query`）。
        let raw_input = input;

        // 用户是否打了手动分隔符。下面几处判据要据此换域：击键串含 `'`，而音节、模式、
        // 长度比较全都活在不含 `'` 的域里。
        let has_manual_sep = raw_input.contains('\'');

        // 简拼判定与比对的基准串：原始击键，不是转换后的全拼。简拼只跟用户敲下的字母有关，
        // 双拼转换后的 `query` 永远匹配不到用户实际敲的声母串。分隔符要剥掉：`'` 是边界声明，
        // 留着会让 `b'z'd` 一条候选都没有；剥除后仍在击键域。
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

        // 混合简拼（声母段 + 音节段）枚举模式用的串，与 `abbr_query` 分家：混合模式问的是
        // 「哪几段是音节」，要在音节存在的域里问。打了分隔符就换到 `query`（全拼与双拼下都是
        // 不含 `'` 的全拼域串）；没打时维持 `abbr_query`，双拼既有行为不变。
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
            // 调用方指定的准入（`ConvertOptions::admit`）在这里判而不是事后 retain：上限施加在
            // 过滤之前，事后过滤只能在被截断过的那一段里筛。
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
        // 含手动分隔符时按 `'` 分段独立分词（`'` 为硬边界），否则整串 DAG。
        //
        // 双拼激活时用双拼自己的真值切分，不让 DAG 重猜：否则「查询按猜测、校验按真值」两套
        // 切分打架，`hao`(3键) 只查了「好」又被真值拒掉，候选全空。取从 0 起连续覆盖的音节前缀，
        // 遇原样回写的断裂段即止（其后 fp 偏移已被污染，那本就是打错的键）。不可整串退回 DAG，
        // 那等于打错一个键对反而解锁全拼；简拼那半由 AbbrevMatcher 兜底（见
        // `shuangpin_writeback_keeps_abbrev_input_intact`）。尾部 partial 由 step4 前缀补全承接。
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
        // `fixed_segmentation` = 切分是真值、只有一条（双拼；`'` 是硬边界），词图必须照单全收。
        // 全拼则相反：切分是猜的，词图应看到全部候选切法（见 lattice::LatticeBuilder::build）。
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
            // 只收从起点连续的音节段：`completed` 的契约是「从 0 开始的连续覆盖」，而手动分隔符
            // 让开头的段可以不是音节（`n'hao` 的 `n`）。照单全收会让 `hao` 的单字以精确匹配身份
            // 占满前排。段间只允许隔着分隔符。
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

        // 完成音节覆盖的连续前缀（从起点算）。多路径切分下它依然唯一确定：`maximum_match` 取的
        // 是最远可达位置，那是图的性质，与走哪条路径无关；多路径只影响词图内部查哪些跨度。
        //
        // 尾部不成音节的残码（`nihaom` 的 `m`）不参与精确匹配/整句解码，否则 lattice 到不了
        // 残码末端、整句退化成单字。
        let completed_len: usize = syllables.iter().map(|s| s.len()).sum();
        // 含分隔符时用音节直接拼接（避免 `'` 字节位错位）；无分隔符时 query==input，等价原切片。
        let completed_owned: String;
        let completed: &str = if has_sep {
            completed_owned = syllables.join("");
            &completed_owned
        } else {
            &query[..completed_len]
        };

        // 1. 精确查找（完整匹配，含模糊扩展）。以 completed 而非 query（可能含尾部残码）为查询码
        //    与存储 code：按 query 查会让 completed 那份精确匹配被当成模糊变体命中、误标 is_fuzzy；
        //    code 存 completed 使残码输入的 consumed_length 只覆盖完成音节（`nihaom` 留 m 续输）。
        for h in self.lookup_with_fuzzy(completed, &syllables, fixed_segmentation) {
            // 词典规范码另走 `learn_code`，不覆盖候选的 `code`。附加信息走事后赋值，用 `before`
            // 判断这次是否真的新增了候选（同文去重时不新增）。
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

        // Viterbi 新合成的整句文本（词典里没有这个词，只能由多个节点拼出来）。
        // 与词典整词同文而被合并的那一支不记入——它本身就是精确整词，不存在「让位」问题。
        // 供 step 6.5 的降级判定使用（须等 step 6 并入用户/临时层后才能定夺）。
        let mut synthesized_sentence: Option<String> = None;
        // 整句 N-best 的池子，随结果带给协调器（见 `ConvertResult::sentence_pool`）。
        let mut sentence_pool: Vec<Candidate> = Vec::new();

        // 整串是否已被完整音节覆盖。step 2b 的混合整句与 step 5b/6.2 的混合简拼共用：都只在
        // 「输入里有成不了音节的字母」时才该启动。
        //
        // 双拼下 `completed_len` 与击键不同域，改判「有没有击键没被音节覆盖」，而不是「最后一个
        // 音节到没到末尾」：手动分隔符与回写段会让中间或开头出现空洞（`n'hc` 的 `n`）。
        // 空洞里只有分隔符不算漏。
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
            // 比的是 `query`：`completed_len` 与它同源。换成 `abbr_query.len()` 目前碰巧无害，
            // 但跨域比长度本身是错的。
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
            // 整句 N-best（见 `Config::sentence_counts`）。出厂 (1, 1) 时 `decode_nbest` 即单路径
            // `decode`，下面的循环只转一圈、只进 `rank == 1` 分支。
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
                // 等效词频，见 `sentence_weight`。
                let weight = sentence_weight(result.log_prob, result.words.len());
                // 整句的边界 = 解码器实际选中的那条路径（多路径下只有解码器知道走的是哪条）。
                // 回退到 maximum_match 仅用于解码器给不出边界的极端情形（超 64 字节）。
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
                        // 备选（rank ≥ 2）只在「露得出来」时才合并：露不出来的只进池子，
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
                    // 备选紧跟在已插入的整句之后。位置只是初值，真正把整句块连续摆到最前的是
                    // 协调器的 `place_sentence_block`。
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

        // 2c. 残码补全整句解码：尾部残码作为「待定音节」入图，由 LM 选出最优单字
        //     （`buzhidaok` → 「不知道看」）。必须另起一条路径：step 2 的图建在 `completed` 上，
        //     残码末端没有槽位。两条路径的产出都保留（不同 `consumed_length` 层），分步上屏依赖
        //     这一点，改成「用残码整句替换 step 2 结果」会破坏它。见 docs/architecture/candidate-sorting-rules.md §4.2。
        //
        //     双拼不跳过（旧图在转换后的全拼上重切、再回映射；开关打开时走击键图）；分隔符跳过
        //     （`completed` 与 `query` 字节位不同源）。混输的码长内跳过（`allow_partial_final`，
        //     判据是「这串还可能是码表码吗」）：step 2c 让整句真的消费满整串，会让隐含假设
        //     「拼音解释不了全部输入」的判据合法放行。加新的生成路径时，要回头查这类隐含假设。
        //
        //     「1 个完整音节 + 残码」这一档延迟定夺：立即插入会让 `wom`→「我吗」这类噪音挤进前排，
        //     而噪音与正解（`zaim`→「在吗」）的分野要等 step 4 跑完才看得见（见
        //     [`COMPLETION_WEAK_CEILING`]）。故先存进 `short_sentence_pending`、不进候选，待 6.5c
        //     再定夺；`completed_syls >= 2` 照常立即插入。
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
            // 放开到 1 个音节，让 `zaim` 这类也能组句，噪音由 6.5c 的延迟定夺挡住。
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
                // S2：残码整句同样认用户词（`yougailunm` = 有 + 盖伦 + 残码 m）。残码那一段由
                // partial_final 节点负责，两者在同一张图上各管各的跨度。
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
                        // 短上下文档：留到 6.5c 按 COMPLETION_WEAK_CEILING 定夺（见上方说明）
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
                            // 码取整串（含残码）：本路径的整句解释了全部输入，这正是它区别于
                            // step 2 结果之处。
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

        // 2b. 混合整句解码：简拼段与全拼段在同一张词图里由 Viterbi 选路径
        //     （`bzdhaobuhao` → 「不知道好不好」）。不能复用 step 2：这类输入从位置 0 就切不出
        //     完整音节，`completed` 为空；step 2 又依赖 `completed` 处理残码，不能改成整串。
        //
        //     与 step 2 的三处差别：
        //     ① 在 `abbr_query`（整串击键）而非 `completed` 上建图；
        //     ② `require_reachable=false` —— 简拼段打断了音节图的可达性，而 `[3,6) hao`
        //        这些边其实都在图里，补上连接的是随后追加的简拼节点；
        //     ③ 追加 `add_abbrev_nodes`，跨度独立枚举（音节图给不出简拼段的终点）。
        //
        //     双拼跳过：`input` 是转换后的全拼、与击键不同域，简拼判据会全部失配。
        if self.config.use_smart_compose
            && self.config.enable_abbrev
            && sp_result.is_none()
            && !has_sep
            // 未被音节覆盖的部分要够构成一个简拼段：尾部单字母残码是「还没打完」而不是简拼
            // （`zhongguorenm`，见 `pinyin_completion::test_useful_completions_still_float`）。
            // 只加在 step 2b（只有它会抢首选），step 5b 的混合召回不受影响。
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
            // S2：混合整句里全拼那几段也认用户词（`bzdgailun` 的 `gailun` 段）……
            self.maybe_add_store_nodes(abbr_query, &graph, false, &mut lattice_nodes);
            // ……声母段另查用户层的简拼索引：按全拼码点查在 `bcx` 这种声母串上必然落空。
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
                // 质量闸门（见 MIXED_SENTENCE_MIN_LOGP_PER_CHAR）。只看节点分：阈值按词频标定，
                // 挂上语法模型后 `log_prob` 里多了转移分（见 `ViterbiResult::lexical_log_prob`）。
                let logp_per_char =
                    result.lexical_log_prob / sentence.chars().count().max(1) as f64;
                // 至少两个节点才算组句：单节点路径本质是一条简拼候选，包装成整句会让 code 变成
                // 击键串，同一个词的词频记到两个互不相认的键上。交给 step5/6.2 即可。
                if result.words.len() >= 2
                    && !sentence.is_empty()
                    && logp_per_char >= MIXED_SENTENCE_MIN_LOGP_PER_CHAR
                    && !candidates.iter().any(|c| c.text == sentence)
                {
                    candidates.insert(
                        0,
                        Candidate {
                            text: sentence,
                            // code = 整串击键：混合整句是解读不是词，以本次输入为码，消费整串由此成立。
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
                            // 标 is_abbrev：与整串简拼词同层、按权重竞争。不标就落在全拼层、整层压过
                            // 所有简拼候选（`zhge` 的「之后个」压过「这个」）。前缀回退候选带
                            // `is_partial`，同层内排在完整匹配之后，不会压住它。依据见
                            // docs/architecture/candidate-sorting-rules.md §4.2「拼音常量的取值依据」与
                            // `tests/pinyin_mixed_sentence_layer.rs`。
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
        // 尾部残码存在时（`meiy`）提升进完整匹配层（`is_promoted_completion`），否则数百条
        // 单字会淹掉「没有」。`is_prefix` 恒表结构真值，排序提升与结构事实拆开。无残码时不提升。
        // 上浮特权不是无条件的：用「补全距离 + 置信度 + 名额」约束（见 `COMPLETION_*` 常量），
        // 距离不能单独用，判别力在 weight。boundary=0 → 距离算作 0 → 放行。
        let trailing_partial = completed != query;
        let completed_syls = syllables.len() as u32;
        // 输入自身表达的音节数：完整音节 + 尾部残码算起头的一个。与
        // `should_promote_user_completion` 的 `started` 同义，也是 step 6.3 闸门的尺子。
        // 不用「输入在候选切分下占的音节数」，见 `completion_syllable_cap`。
        let started_syllables = completed_syls + u32::from(trailing_partial);
        // 补全条数跟随调用方请求量，并 clamp 到 [30, MAX_COMPLETION_CANDIDATES]，让单字母输入
        // 也能持续翻页。词库层分支限界下取数成本随条数增长（见
        // docs/design/prefix-topk-branch-and-bound.md），过滤不参与剪枝，合格条目凑不满时会
        // 退化成整棵子树遍历，靠码长上限剪枝与 `prefix_completion_is_exact_only` 兜住。
        //
        // 上限不能去掉：`push_unique` 线性查重，整体 O(n²)。治本要换哈希索引，但整句走
        // `insert(0)` 绕过该闭包、后续还有 `retain`，会让哈希集与列表失同步，须单独立项。
        let completion_limit = max_candidates.clamp(30, MAX_COMPLETION_CANDIDATES);
        // 音节数过滤下推到词库层（step 6.3 的闸门在此提前施加）：只在 6.3 事后 retain 的话，
        // `limit` 配额会被注定要丢弃的词组吃光。6.3 的 retain 仍保留，兜住词库层判不了的
        // `boundary == 0` 与 step6 并入的用户/临时词。
        let syllable_cap = completion_syllable_cap(
            started_syllables,
            self.config.completion_min_syllables,
            self.config.completion_max_extra_syllables,
        );
        // 音节边界对齐同样下推到词库层（`completed_len` 入参）：补全只能从下一个音节开始，
        // 不能把已打完的音节拉长（`shen` 不召回 `sheng`），`syllable_cap` 拦不住这类。判据见
        // `wind_dict::cached::prefix_syllable_aligned`。传 `completed_len` 而非 `query.len()`：
        // 残码位要的正是「把当前音节补完」；裸声母下它为 0，判据自动让开、退化为字符前缀匹配。
        //
        // 先收集再定名额：名额要发给权重最高的那几条（见 `COMPLETION_NEAR_PROMOTE_LIMIT`）。
        // 下面按权重排序目前与上游返回序重复，但「名额按权重发」是本逻辑的要求，不寄存在上游
        // 的实现细节上。
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
            // `distance` 已在上方经 `effective_boundary` 算好：`boundary == 0` 时直接
            // `count_ones()` 会让多音节词冒充「完全对齐」。
            let demote_to_prefix_layer = if trailing_partial {
                // 门槛比的是原始 weight：COMPLETION_FAR_WEIGHT_FLOOR 按原始权重分布标定，折扣与
                // 降级是正交的两件事。
                //
                // `weight <= 0` 一律降级，距离 1 也不例外：w≤0 是词库对存疑条目的标记
                // （`lattice.rs::score_node` 同样重罚），不该被无条件上浮提到最显眼处。
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
            // is_prefix 恒表结构事实；是否沉到非精确层由 is_promoted_completion 承接；层内的远近
            // 之别由 `completion_penalized` 的连续折扣与 `completion_extra_syllables` 档位共同承接。
            //
            // 两者口径不同：折扣用 `distance`（比已完成音节多几个，衡量可信度衰减），档位要的是
            // 比输入自身表达的音节数多几个（`meiy` 的「没有」恰好对齐、不该降档）。
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
            // 同文已存在时 push_unique 不添加，此时不置位：那条候选不是补全，档位保持 0。
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

        // 5. 简拼匹配（声母缩写，如 nh→你好）：查 wdat 预存的独立 AbbrevSection，仅当输入像
        //    简拼时才查。natural_order=999999 让简拼候选默认排在全拼之后。
        //
        //    AbbrevSection 存的是全拼码，故是「查索引拿码 → 走主表装配」两步，候选带上真实的
        //    code 与 boundary，同一个词在简拼与全拼下才共用一份词频计数。
        if stroke_is_plain_abbrev {
            for (key, edits) in self.abbrev_recall_keys(abbr_query) {
                // `edits` 由 `fuzzy_abbrev_keys` 逐位计，与校验侧 `Initial` 段计 1 处同口径；
                // 精确键为 0、不罚。不罚会让模糊解压过精确解：简拼层内仍按 weight 降序，折扣是
                // 层内唯一区分精确键与变体键的机制（混合路径见
                // `fuzzy_mixed_abbrev_is_penalized_and_marked`）。
                for abbr_code in dict.search_abbrev(&key, ABBREV_INDEX_LIMIT) {
                    // 用 search_with_boundary 而非 search：拼音引擎直接持有 CachedDict、
                    // 不经 SystemDictLayer，用 search() 会把边界丢在这里。
                    for h in dict.search_with_boundary(&abbr_code) {
                        // 音节数必须等于简拼字母数。扁平码有损：`xa` 指向「西安」的码 `xian`，
                        // 回查主表会把单音节的「先/线」一并捞出来。boundary=0 不直接放行（否则
                        // 绕过音节数约束），用 `effective_boundary` 现切，与 6.3 闸门同源。
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
                        // 简拼层标记必须与 step6 的用户词简拼一致：层级不同时用户词会被整层压在
                        // 系统词之后，怎么调频都翻不过来。
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
        //     召回复用 step5 的索引：模式的声母投影键就是纯简拼串（`nhao` → `nh`）；混合信息
        //     留在模式里做后置校验（按 boundary 把全拼码切回音节序列逐段比对）。整串已被音节
        //     完整覆盖时短路（见 `mixed_covered`），正常全拼输入零开销。双拼 + 手动分隔符时
        //     模式改从全拼域枚举（`mixed_pattern_source`）：击键域里没有音节。
        let mixed_pats = if self.config.enable_abbrev && !mixed_covered {
            mixed_abbrev::mixed_patterns(mixed_pattern_source, trie)
        } else {
            Vec::new()
        };
        timer.mark("abbrevs5");

        // 5b. 混合简拼：同一串里混用声母与完整音节（`nhao` = n + hao、`nih` = ni + h）。
        if !mixed_pats.is_empty() {
            // 多条解释常投影到同一个键，每条模式再展开首字母模糊变体，合并去重后逐键点查。
            let mut keys: Vec<String> = mixed_pats
                .iter()
                .flat_map(|p| self.abbrev_recall_keys_only(p.key()))
                .collect();
            keys.sort_unstable();
            keys.dedup();
            timer.mark("a5keys");
            for key in &keys {
                // 与 step5 共用 `ABBREV_INDEX_LIMIT`（见其文档）。
                for abbr_code in dict.search_abbrev(key, ABBREV_INDEX_LIMIT) {
                    for h in dict.search_with_boundary(&abbr_code) {
                        // 无边界信息 → 判据不存在 → 不参与（不是放行，见 syllables_from_boundary）。
                        let Some(syls) =
                            mixed_abbrev::syllables_from_boundary(&abbr_code, h.boundary)
                        else {
                            continue;
                        };
                        // 取模糊处数最少的解释计罚。不按 `p.key() == key` 预筛：召回键可能是
                        // 某条模式的模糊变体，预筛会把刚捞回来的词又滤掉；模式数有封顶，代价很小。
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
                        // 并入 is_abbrev 层，不新造层级（设计文档 §5 约束 2）：与纯简拼是同质候选，
                        // 分属两层会让一侧被整层压住。这个标记同时让候选走在双拼边界校验的豁免侧
                        // （约束 1），它的 code 与当次击键不同域。
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
            // 借同文合并给已有候选提权；不下推则名额被它们吃掉，大用户词库还会整棵子树遍历。
            store_cands.extend(store_dm.search_prefix_capped(
                query,
                limit,
                syllable_cap,
                completed_len,
            ));
            // 模糊音：整串与各前缀子码的变体码，与系统词 `lookup_with_fuzzy` 同源；前缀补全不做
            // 模糊（系统词那路也不做）。排在全部精确结果之后：下面按 text 合并、先到者占位。
            // 整串只在无残码时展开：有残码时精确那路也只查 `query`（与系统词 step 1 查
            // `completed` 不同，是既有口径，两路要改须一起改）。
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

            // 用户长词上浮的封顶基准：提升后的补全不得越过本次输入的最佳完整解（码 == completed
            // 的顶层候选），取其最大权重 - 1。无此类候选时不封顶。
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
                // 同文时合并而非整条丢弃，否则用户词的 weight 在词已存在时从不参与比较。
                // - `weight` 取 max：用户加词的意图是提权而非降权（降权由词频/屏蔽机制表达）。
                // - `code` / `boundary` 保留已有候选的，二者必须同进同出；用户手输码常无边界，
                //   换过去等于把真值边界抹成未知。
                // - 来源标记按 `c` 的实际归属置位，并把 `c.code` 记进 `meta.store_code`：右键删除
                //   按标记选表、按码拼 key，盖错标记或码会静默删个空。
                // - 其余层级标志一律不动：它们描述候选相对本次输入处在哪一层，与用户是否收录无关。
                if let Some(existing) = candidates.iter_mut().find(|x| x.text == c.text) {
                    // 用户权重的生效条件必须与下面「新增」分支对称，否则远距离补全词会借合并路径
                    // 绕过 `should_promote_user_completion` 与 `promotion_cap` 夺走首选：
                    // - 精确匹配（`is_prefix=false`）：用户提权全效，这是「加词提权」的本义。
                    // - 前缀补全：须满足同一个提升判据，且同样受 `promotion_cap` 封顶。
                    // - 不满足判据的远距离补全：保留系统权重，沉在补全层（不是丢弃）。
                    if !existing.is_prefix {
                        existing.weight = existing.weight.max(c.weight);
                    } else if should_promote_user_completion(
                        completed_syls,
                        trailing_partial,
                        // 码与边界必须取自同一条候选（此处都是 `existing` 的）。
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
                // 编码手填成简拼的用户词（`rq` → 「$Y年$M月$D日」）：码与击键逐字相同，会被当全拼
                // 精确命中放进完整匹配层、权重 0 也恒居首选。击键是纯简拼时它本身就是简拼码，
                // 归入简拼层按权重竞争。比 `abbr_query` 而非 `query`（双拼下不同域）。
                // `natural_order` 不改：它承载同码用户词的导入序。
                //
                // 击键本身是音节（序列）的前缀时不降层（`sh`/`zho`，判据即
                // `is_possible_pinyin_sequence`）：主解释是全拼，降层会沉到整批前缀补全之下、
                // 常常出首页。只在击键无法读作全拼（`rq`/`bzd`）时降层。
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
                    // 上浮判据与显示序档位共用同一次现切（`boundary == 0` 下每次都是一趟
                    // `Dag::build`）。档位口径同 step4。
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

            // 简拼匹配（用户/临时造词层）：经声母索引取候选集，判据在下面逐条判（见
            // recall_store_by_abbrev）。natural_order 对齐 step5，同样排在全拼之后。
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
                    // 混合简拼：按 boundary 切回音节序列逐段比对（无边界 → 不参与），与系统词侧
                    // 同一批 `mixed_pats`。短路只在纯简拼精确命中时成立。
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
                    // 取处数最少的解释计罚，不能用 `or`（理由见 `recall_abbrev_prefix` ③④）。
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
                    // 保留全拼码（连同同域的 boundary），不覆盖成简拼串，否则同一个词在简拼与
                    // 全拼下走两个互不相认的词频计数。`consumed_length` 不受影响，仍是消费整串。
                    c.is_prefix = false;
                    c.is_partial = false;
                    // 简拼层标记（别借 `is_fuzzy` 之类的来源标记沉底）。
                    c.is_abbrev = true;
                    c.natural_order = 999999;
                    // 模糊命中的折扣与来源标记：纯简拼也可能是变体键命中，不罚会压过同一投影键下
                    // 精确命中的用户词。
                    if edits > 0 {
                        c.weight = fuzzy_penalized(c.weight, edits);
                        c.is_fuzzy = true;
                    }
                    candidates.push(c);
                }
            }
        }

        // 6.2 简拼族前缀回退：退到更短的前缀，余下字母作残码留给下一次输入（分步上屏）。
        //     简拼族召回本是「全串或无」，长过任何单个词就彻底空码；全拼有 step3/step4 兜着，
        //     简拼靠这里。整串命中时也照样补（对齐微软拼音：整长匹配 → 词组前缀 → 首声母单字），
        //     补进来的都是 `is_partial`，沉在整长匹配之后，不改变首选。
        //
        //     不做「最长匹配、一有产出即停」：多个切点的候选共存、同层按词频竞争。切点长短是
        //     来源差异，只配走 weight，做成硬闸门会让 `bzdh`（表彰大会）挡掉词频高几百倍的
        //     `bzd`（不知道）。候选变多由 `AbbrevFallbackConfig` 的限额兜住。这仍是启发式：
        //     简拼段之间没有语言模型仲裁。
        //
        //     门槛问的必须是「这串里有成不了音节的部分吗」（`!mixed_covered`）：只看「简拼族
        //     没命中」会把纯全拼拖进降级（`meiyou`，见
        //     `test_pinyin_trailing_partial_prefix_floats_above_exact`）；看「整串是简拼族形态」
        //     会因段数超过 `MAX_SEGMENTS` 把长串挡在门外。逐前缀的严格判定在
        //     `recall_abbrev_prefix` 内部。
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
        // 简拼族（code 与击键不同域）不需要豁免：它们出场时起点那段成不了音节、`syllables`
        // 为空，`syllable_span` 恒为 None。若将来放开 step 2b 的 `!has_sep` 门槛，简拼候选可能
        // 与非空 `syllables` 共存，届时要加 `is_abbrev` 豁免并配用例。
        if has_sep {
            let syls = &syllables;
            candidates.retain(|c| match syllable_span(syls, &c.code) {
                Some(k) if k >= 1 => c.text.chars().count() == k,
                _ => true,
            });
        }

        // 6.3 音节数与边界对齐闸门：前缀补全候选的音节数不得超过 `syllable_cap`（见
        //     `completion_syllable_cap`，尺子是输入自身的 `started_syllables`），且只能从下一个
        //     音节开始补。只约束 `is_prefix`：只有前缀补全在预测未输入的音节；精确匹配、子短语、
        //     整句不预测，简拼族的音节数判据在各自源头按击键字母数施加。
        //
        //     位置必须在 step 6.2 之后（用户/临时词与简拼前缀回退到那时才并入），且在排序/截断
        //     之前（先截断再过滤会把该出的候选挤掉）。
        candidates.retain(|c| {
            if !c.is_prefix {
                return true;
            }
            // 与词库层下推的那道共用同一个函数（`wind_dict::cached`），只是这里先用
            // `effective_boundary` 补出无真值的 boundary（词库层拿不到 trie，只能放行）。
            // 在此复查是为了词库层够不着的候选：step 6 并入的用户/临时词与 `boundary == 0` 的条目。
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

        // 6.5 整句让位于精确整词：降级，不销毁（整句仍在候选里，只排到精确整词之后）。
        //
        // 两类整句需要让位：① Viterbi 新合成的整句（`synthesized_sentence`；与词典整词同文
        // 合并的那支本身就是精确整词）；② 模糊命中的整句（`sixiang` 经 s↔sh 命中「是想」）。
        // ② 不能只靠 `fuzzy_penalized`：同文合并取 `max(词典 weight, W_eff)` 会旁路模糊惩罚。
        // 没有同码精确整词时（`zongguo`）不触发，模糊解照常居首。
        //
        // 位置必须在 step 6 之后：用户/临时词到那时才并入，而用户加词正是要服务的场景。
        //
        // 降到 `max - 1`：相对值与词频量纲无关；取 max 让整句恒定停在最强精确整词之后，并列于
        // max 的全部在它之前（`demoted_sentence_falls_below_all_max_weight_exact_words`）。同层内
        // 只有精确整词与整句（其余由 `cmp_match_layers` / `cmp_exact_first` 分层），三个排序器都以
        // `cmp_match_layers` 为首要键，位置在整条链路上一致。
        //
        // `is_sentence` 不清（它是来源语义），降级另由 `is_sentence_demoted` 表达，`freq_rerank`
        // 的顶部锚定据此豁免，否则本处降权会被整个顶回去。
        let mut demote_targets: Vec<String> = synthesized_sentence.iter().cloned().collect();
        for c in candidates.iter() {
            if c.is_sentence && c.is_fuzzy && !demote_targets.contains(&c.text) {
                demote_targets.push(c.text.clone());
            }
        }
        for sent in demote_targets {
            // 「精确整词」：码恰好等于 `completed`，非模糊、不在前缀补全/子短语层，含 step 6
            // 并入的用户/临时层。排除引擎合成的 N-best 备选（不是词条，算进来会名次倒挂）。
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

        // 6.5b 整句让位于「恰好用完残码的补全」：有残码时整句只解释了 `completed`、把用户按下的
        //     残码丢掉了（`nihaom` 的「你好」），应让位给「你好吗」。判据复刻 librime
        //     `has_exact_match_phrase` 的语义：补全词音节数 == 已完成音节数 + 1（残码补成一个音节
        //     后恰好用完），自带过滤（`beijingdaxuex` 的冷僻长词不触发）。详见
        //     docs/architecture/candidate-sorting-rules.md §4.2「整句让位于用完残码的补全」。
        //
        //     手法同 6.5：降到该批补全的 `max - 1`，只往下压、不把 6.5 已让的位抬回来。残码补全经
        //     上浮与整句同层，降 weight 才能换位。`boundary == 0` 的候选永不满足判据（保守不触发）。
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

        // 6.5c 「1 音节 + 残码」残码整句的延迟定夺（step 2c 短上下文档）。判据见
        // [`COMPLETION_WEAK_CEILING`]：补全侧弱到不像正解时整句才配进候选，不够格的根本不插入。
        //
        // 放在 6.5b 之后是有意的：本档整句消费了整串，不是 6.5b 要治的对象，放在之前会被
        // 无条件降到补全之下。
        if let Some((text, weight, boundary, learn_code, aux_path)) = short_sentence_pending {
            // 补全侧没有「恰好用完残码」的答案（None）⇒ 词库在这个码上一无所有，放行。
            // 有答案时：只有它弱到不像正解才让整句上位，且整句本身要真的更强（挡住双差场景）。
            let strong_enough = exhausting_completion_max
                .is_none_or(|max_w| max_w < COMPLETION_WEAK_CEILING && weight > max_w);
            if strong_enough {
                if let Some(existing) = candidates.iter_mut().find(|c| c.text == text) {
                    // 同文合并：step 4 已把它当前缀补全召回过（w=0 被压在前缀层）。必须一并提层：
                    // 跨层不比权重，只抬 weight 抬不上来。
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
                        // 码取整串（含残码），理由同 step 2c
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

        // 双拼真值边界校验：双拼把音节边界定死了，候选的词典边界必须与之吻合。
        // 在排序/截断**之前**过滤——否则会先截断再过滤，把该出的候选挤掉。
        // 词典无边界信息的候选（用户手输码/五笔/旧数据）boundary=0，一律放行（降级回 DAG 行为）。
        if let Some(r) = &sp_result {
            let sp_mask = sp_boundary_mask(r);
            if sp_mask != 0 {
                let full_len = r.full_pinyin.len();
                candidates.retain(|c| {
                    // 与双拼切分不同域的候选显式豁免，否则必然不符、整批被静默滤光：简拼（code
                    // 是词的全拼码，带真实边界）、全拼降级候选（step 6.7）、连续辅助码的辅码候选
                    // （码去掉了辅码，双拼转换却把辅码字母读成了音节）。模糊变体靠 boundary=0 豁免。
                    c.is_abbrev
                        || c.is_fullpinyin_fallback
                        || c.meta.aux_keys.is_some()
                        || boundary_compatible(c.boundary, sp_mask, c.code.len(), full_len)
                });
            }
        }

        // 6.7 全拼降级支路（双拼方案 + `allow_full_pinyin`）：把击键串当全拼再读一遍。
        //     位置由三条约束钉死，挪动前逐条复核：
        //     - 必须在双拼边界校验之后：双拼转换结果常与击键同形（`nihao` → ni|ha|o），step1 的
        //       「你好」会被校验删掉；放在之前，支路那条因同文查重让位给它，随后两条都没了；
        //     - 在 6.3 闸门之后：那道 retain 的尺子是双拼域音节数，不能拿来裁全拼候选；
        //     - 在所有双拼候选产出之后：支路靠「同文时保留已有候选」实现双拼优先。
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

        // 分段上屏所需：标注每个候选实际消费的输入字节数。放在 `sort_by` + `truncate` 之前，
        // 日后排序要用 `consumed_length` 时才不会读到 0。
        // code 为 input（全拼）的前缀（如 "ni" ⊂ "nihao"）→ 只消费该前缀，选中后保留剩余拼音续转；
        // 否则（整句/前缀补全/非前缀子串）消费整串。0 表示未知（由调用方按整串处理）。
        // 双拼激活时：全拼字节数需通过 map_consumed_length 回算为双拼原始键数，
        // 否则变长音节（2键→3字节，如 zh/ch/sh）会错误消费/越界双拼键缓冲区。
        for c in candidates.iter_mut() {
            // 简拼族前缀回退（step 6.2）的候选自带击键域的消费数，必须跳过这套计算：简拼的
            // code 是全拼码、不以 query 开头，会落到「消费整串」，分步上屏当场失效。整串简拼候选
            // 的 consumed_length 为 0、照旧走这条计算。双拼下同样跳过（已是击键域）。
            if c.is_abbrev && c.consumed_length != 0 {
                // 含手动分隔符时，回退给的是剥除 `'` 之后的字节数，换算回原始输入空间，
                // 否则选中词组后协调器按含 `'` 的缓冲切片会残留错位（`b'z'd` 选 `bz` 的词组）。
                if has_sep {
                    c.consumed_length = raw_len_for_stripped(input, c.consumed_length);
                }
                continue;
            }
            // 全拼降级候选（step 6.7）同样自带击键域的消费数，不可再过 `map_consumed_length`
            // （全拼域 ≡ 击键域，再换算一次即错位）。
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
        // 必须在紧随其后的 `sort_by` + `truncate` 之前：部分匹配的同音字动辄数百条，简拼候选
        // 又最沉，放到调用方去 `retain` 的话简拼词在截断时就已经没了。
        //
        // 判据只能是 `consumed_length`，不能用 `!is_partial` 代替（整句走 `insert(0)` 不经算
        // `is_partial` 的闭包，同文合并还会把它置 false）。0 = 按整串算。判据切在「解释完整度」
        // 而非候选类型上：前缀补全（`wanl` → 「完了」）消费整串，与残码单字方向相反，不能一并禁用。
        //
        // 基准取 `raw_input.len()`：`consumed_length` 刚回映射到原始输入空间；`query` 与 `input`
        // （双拼下是转换后的全拼）都与它不同域。
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

        // 引擎侧刻意不用「消费长度优先」排序（那是协调器 `candidate_display_order` 的事）：紧随
        // 其后的 `truncate` 让排序同时决定去留，消费更少的子短语/单字会被整批丢弃，分段上屏失效
        // （见 `separator_two_step_segmentation`）。引擎侧层级保证「高价值候选活过截断」，与协调器
        // 的显示顺序不可互相替代。按消费长度分档的保底配额尚未做，`truncate_with_quotas` 只护简拼。
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
        // 判据只认词库原值：模糊命中、整句、补全、子短语、全拼降级支路一律不算；用户词 / 临时词
        // 里有这个字 = 用户用过这个读音，不让位。用户选过的字由
        // `freq_rerank::rerank_pinyin_positional` 摘掉标记。
        //
        // 必须在截断之后置位、再稳定重排：截断前置位的话，腾位时从尾部先挤掉的就是它们，词频
        // 重排救不回来。位置也须在下方 preedit 读 `candidates.first()` 之前。
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

        // 预编辑区跟随首选候选：多路径切分下 `maximum_match` 不必然是首选走的那条
        // （`xianjiaotongdaxue` 的「西安交通大学」走 `xi|an|…`），显示 mm 会与候选自相矛盾。
        // 候选自带 `boundary`，据此还原切分即可。双拼的 preedit 另有 build_raw_preedit 负责，
        // 分隔符段的 `'` 是用户打的硬边界不容改写，无边界信息则保持 mm 显示。简拼与击键不同域，
        // 另走一支，见 `candidate_preedit_split`。
        //
        // 切分按每条候选各算一份（`CandidateMeta::preedit_split`），协调器在高亮移动时换用
        // 高亮那条的；首选那份即这里的 preedit_display。
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

        // 双拼激活时，preedit 改为显示用户实际输入的原始按键（按双拼音节边界以 `'` 分隔），
        // 而非转换后的全拼。仅覆盖 preedit_display；候选/completed_syllables/partial_syllable/
        // consumed_length 仍保持全拼语义不变。
        // 双拼自身的音节切分恒为**默认**显示形态。
        let mut preedit_fullpinyin = String::new();
        let mut preedit_abbrev = String::new();
        // 双拼分段形态：非简拼候选的显示串，同时是 `preedit_pinyin` 交出去的那一份。它必须始终
        // 是双拼切法，否则高亮从简拼候选移回时协调器无处取回原形态。
        let mut sp_body = String::new();
        if let Some(r) = &sp_result {
            sp_body = build_raw_preedit(raw_input, r);
            preedit_display = sp_body.clone();
            // 简拼/混合简拼候选的**击键分段**（`wbwn` → `w'b'w'n`），供协调器按高亮切换
            // （见 `ConvertResult::preedit_abbrev`）。
            //
            // 双拼切法与简拼的每键一声母切法都成立，只能由高亮候选来选。取首个简拼候选而非
            // `candidates.first()`：这一份是给「高亮到简拼候选时」用的；同码简拼候选切法一致。
            preedit_abbrev = candidates
                .iter()
                .find_map(|c| self.abbrev_keystroke_preedit(c, raw_input))
                .unwrap_or_default();
            // 首选就是简拼候选 ⇒ 初显直接用简拼分段（overlay 模式如快捷输入/临拼只读
            // `preedit_display`，不走协调器的高亮跟随，靠的就是这一步）。
            if candidates.first().is_some_and(|c| c.is_abbrev) && !preedit_abbrev.is_empty() {
                preedit_display = preedit_abbrev.clone();
            }
            // 支路有产出时额外给出全拼切分（见 `ConvertResult::preedit_fullpinyin`）。不能在这里按
            // 首选就地改 preedit_display：引擎只在按键时 convert 一次，高亮之后才移动。形态选择
            // 交给协调器的 `effective_preedit_body`，引擎只负责把两种形态都交出去。
            if candidates.iter().any(|c| c.is_fullpinyin_fallback) {
                let (fp, fp_syllables, fp_partial) = self.compose_segment(raw_input);
                // 全拼降级候选也各带自己的切分（整串最大匹配不一定是它走的那条）。简拼标记同真的
                // 留给协调器的简拼形态（那边优先）。
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
                // 与 `sp_body` 比而不是 `preedit_display`（后者可能已被换成简拼分段）：问的是
                // 全拼切法与双拼切法是否不同。
                if fp != sp_body {
                    preedit_fullpinyin = fp;
                }
            }
        }

        timer.mark("tail");
        let has_partial = !partial_syllable.is_empty();
        let is_empty = candidates.is_empty();

        // 候选调整（shadow）规则的归一编码，契约见 `ConvertResult::shadow_code`。
        //
        // 双拼分支只看 `mixed_covered`（转换结果完整覆盖整串击键，`full_pinyin` 是干净的全拼域）。
        // 不用 `!stroke_is_plain_abbrev`：双拼两键击键绝大多数也判得出简拼形态，会把常态整体挡在
        // 归一之外。不用「简拼是否真的命中」：它依赖词库内容，加删一个词会让 key 来回摇摆、
        // 规则静默失效；稳定性优先，key 只取决于双拼布局。代价是一处窄串扰：既用全拼又用双拼的
        // 用户在全拼 `nang` 下置顶过的词，会在双拼敲 `nh` 时被顶上来（见
        // `tests/pinyin_shadow_code_domain.rs::sp_abbrev_and_full_coexist_in_one_keystroke`）。
        //
        // 全拼恒空串（落回击键）：不可剥 `'`，`xi'an` 与 `xian` 候选集不同。
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
            // 拼音恒为拆分形态（供混输高亮跟随）。双拼下取 `sp_body` 而非 `preedit_display`
            // （后者可能已换成简拼分段）：两种切法必须各有一个稳定的落点。
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
    /// 层序是层 2 → 层 4 → 层 3，与设计文档 §3.1 的可信度排序有意不同：层 3
    /// （`generate_word_pinyin`）枚举读音笛卡尔积，最贵；层 4 便宜且唯一解时同样确定，故只在
    /// 它给出多解时才请层 3 消歧。
    ///
    /// 层 4 无解即判非法、不再试层 3：若推导码 flat 后等于 `code`，其音节序列本身就是一条
    /// 「音节数 == 字数」的合法路径，与层 4 无解矛盾。
    fn resolve_boundary(&self, code: &str, text: &str) -> BoundaryResolution {
        // ü 拼写归一：用户手打 `xv` 当编码时应等价于 `xu`（规则见 [`spelling`]），否则层 2 与
        // 层 4 都会落空、判 Unresolvable。这只保证求解正确；落库的 key 由调用侧决定，码归一须由
        // 调用侧另做一次（wind-coordinator 的 `user_words_api::normalize_add_code`），否则加得进、
        // 打不出。有意不按 `shuangpin` 短路：入参是词典码域（恒为全拼），不是击键域。
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
        // 读音表整体缺席（text 含无读音字符）⇒ 切分已定但验证不了。必须先于 `ambiguous` 判：
        // 那一档的语义是「已按读音权重择一」，这里没有权重可择（层 3 也必返回 None）。
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
