//! 英文候选混入（供单一来源引擎复用的查询与合并）
//!
//! 把「查一次英文词库，把结果混进另一个引擎的候选列表」独立出来，供纯拼音方案与纯码表方案
//! 复用。接线点在 [`crate::manager::EngineManager`]：`convert` / `recheck_auto_commit` /
//! `handle_top_code` 三条通路的共同收口，不必逐引擎改造或包转发装饰器。
//!
//! 本模块不是引擎无关的：配置按引擎分两份（`[schema.codetable.english_merge]` 与
//! `[schema.pinyin.english_merge]`），由 `manager::english_merge_cfg` 折叠成 [`Effective`]
//! 再交给本模块；新增引擎类型时那里的穷尽匹配会强制做决定。
//!
//! 混输取哪些英文同样走 [`select`] / [`has_any`]，但合并走 `MixedEngine::merge_sort_dedup`
//! （与三方截断档位仲裁耦合）。混输方案两份配置都不读，只读自己的 `schema.mix.*`，否则两套
//! 英文各混一遍。
//!
//! 本模块只管「查哪些、谁活下来」：取回的每一条都保证活过截断（纯拼音一次就能吐满
//! `max_candidates`，追加在尾部会被整片截掉）。位次由协调器 `place_english_after_common_exact`
//! 在全部排序之后统一定位。设计见 docs/architecture/engine-candidate-pipeline.md 的「6.1」。

use crate::engine::Engine;
use wind_candidate::Candidate;

/// 扫描窗口：向英文引擎取多少条原始命中，用来从中挑出精确匹配。
///
/// 英文引擎精确整串恒居首，取 8 条是给「同码多形态」（`she` / `She`）留余量，不是给前缀留的。
const EXACT_SCAN: usize = 8;

/// 无精确命中、且基础候选为空时，前缀回退至多给几条。见 [`lookup_prefix_fallback`]。
const PREFIX_FALLBACK_MAX: usize = 5;

/// 英文输入态下的最小长度下限：中文侧无候选时，`min_length` 最低压到这个值。
///
/// 与「丢弃前缀扩展」同一个理由，故在同一前提下一起放宽，否则 `wi` / `vi` / `hi` 仍是空码。
/// 不放到 1：单字母补全没有信息量，且 1 码在拼音下通常有简拼候选。
const EMPTY_BASE_MIN_LENGTH: usize = 2;

/// 精确命中至多保留几条（同码的大小写形态）。2 条足够，候选面上英文再多也没有意义。
const EXACT_MAX: usize = 2;

/// 最小触发长度的回退值（配置为 0 时）。与 `manager::build_engine` 给
/// `schema.mix.min_english_length` 的回退同值，两处口径一致。
const DEFAULT_MIN_LENGTH: usize = 3;

/// 本次转换生效的英文混入参数。
///
/// 码表侧来自 `schema.codetable.english_merge`（经方案级覆盖折叠），拼音侧来自
/// `schema.pinyin.english_merge`，混输侧来自 `schema.mix` 的 `english_*`（`MixedEngine` 构建时
/// 折叠）。本结构是它们的共同形态。拼音侧 `block_commit` 恒为 `false`：拼音方案没有满码 /
/// 顶码上屏可否决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effective {
    pub enable: bool,
    pub min_length: usize,
    /// 前缀级否决：有英文词以当前串开头即否决上屏（[`has_any`]）。
    pub block_commit: bool,
    /// 成词级否决：整串恰是一个英文完整单词时否决上屏（[`has_word`]）。前缀级开着时被它涵盖。
    pub block_commit_on_word: bool,
    /// 中文侧有候选时，输入达到几码起按长度给前缀补全（`complete_min_length`）。
    /// 0 = 不按长度补。双拼方案由管理器折成 0（见 [`Self::complete_only_non_pinyin`]）。
    pub complete_min_length: usize,
    /// 补全再叠一条拼音判据：整串切不成完整全拼音节才补（[`prefix_complete_applies`]）。
    /// 只有拼音侧为 `true`；码表 / 混输够长就补。
    pub complete_only_non_pinyin: bool,
    /// 中文侧有候选时英文合计至多几条（完整单词优先）。已解析回退值（≥1）。
    pub max_count: usize,
}

impl Effective {
    /// 上屏否决（[`has_any`]）的长度门槛：英文从几码起可能出现在候选里就从几码起否决。
    ///
    /// 完整单词从 `min_length` 起出；开了按长度补全时补全从 `complete_min_length` 起出，
    /// 取两者较小者。门槛再低就是「英文还不会露面却先挡住上屏」。
    pub fn veto_min_length(&self) -> usize {
        let base = min_length_or_default(self.min_length);
        if self.complete_min_length > 0 {
            base.min(self.complete_min_length)
        } else {
            base
        }
    }
}

/// 中文侧有候选时英文条数上限的回退值（配置为 0 时）。码表 / 混输同口径。
const DEFAULT_MAX_COUNT: usize = 2;

/// 配置值 0 视作「用回退值」，口径同 [`min_length_or_default`]。
pub fn max_count_or_default(configured: usize) -> usize {
    if configured > 0 {
        configured
    } else {
        DEFAULT_MAX_COUNT
    }
}

/// 按中文侧有无候选，取这次要进候选列表的英文。码表、拼音与混输共用这一个入口。
///
/// | 中文侧 | 给什么 |
/// |---|---|
/// | 空 | [`lookup_prefix_fallback`]：前缀补全一页（不受 `max_count` 约束） |
/// | 有 | [`lookup`] 的完整单词；补全开着且适用时再补前缀词；合计不超 `max_count` |
///
/// 补全按 `complete_min_length` 判；拼音另要求整串不是完整拼音（[`prefix_complete_applies`]）。
pub fn select(
    english: &dyn Engine,
    input: &str,
    base_empty: bool,
    cfg: &Effective,
) -> Vec<Candidate> {
    if base_empty {
        return lookup_prefix_fallback(english, input, cfg.min_length);
    }
    let by_length = cfg.complete_min_length > 0
        && input.chars().count() >= cfg.complete_min_length
        && (!cfg.complete_only_non_pinyin || prefix_complete_applies(input));
    // 按长度补全时，完整单词的门槛一并放到补全门槛：否则 `complete_min_length` 比 `min_length`
    // 小时，打 `go` 补出 good / google 却不出 go 本身。
    let exact_gate = if by_length {
        cfg.veto_min_length()
    } else {
        cfg.min_length
    };
    let mut v = lookup(english, input, exact_gate);
    let room = if by_length {
        cfg.max_count.saturating_sub(v.len())
    } else {
        0
    };
    if room > 0 {
        let extra = lookup_prefix_complete(english, input, &v, room);
        v.extend(extra);
    }
    v.truncate(cfg.max_count);
    v
}

/// 配置值 0 视作「用回退值」，口径同 `schema.mix.min_english_length`。
pub fn min_length_or_default(configured: usize) -> usize {
    if configured > 0 {
        configured
    } else {
        DEFAULT_MIN_LENGTH
    }
}

/// 查英文词库，只取精确命中（`code == 小写化输入`）。短输入不查。
///
/// 前缀扩展在中文侧有候选时是纯噪音，想打某个英文词就继续敲完它。与 [`has_any`] 的判据
/// 刻意不同：否决上屏仍须按前缀判，否则五笔满码会在 `gith` 把中文顶上屏，别「顺手统一」。
///
/// 清掉 `is_exact_code`：它是码表域标志，拼音从不设它；带进拼音列表会让英文经
/// `cmp_exact_first` 无条件压过全部中文。
///
/// 查询失败静默退化为空：英文只是捎带的增强，不该影响主候选。
pub fn lookup(english: &dyn Engine, input: &str, min_length: usize) -> Vec<Candidate> {
    if input.chars().count() < min_length_or_default(min_length) {
        return Vec::new();
    }
    let lower = input.to_lowercase();
    let Ok(r) = english.convert(&lower, EXACT_SCAN) else {
        return Vec::new();
    };
    r.candidates
        .into_iter()
        .filter(|c| c.code == lower)
        .map(|mut c| {
            // 见函数文档「清 is_exact_code」。
            c.is_exact_code = false;
            c
        })
        .take(EXACT_MAX)
        .collect()
}

/// 英文输入态的查询：中文侧一条候选都没有时用它，给前缀补全。
///
/// 调用方只按「中文侧有没有候选」二选一，不能再叠「有没有精确命中」：两个判据会在不同步的
/// 那一键上让候选数突变。长度门槛同时放宽到 [`EMPTY_BASE_MIN_LENGTH`]。见
/// docs/architecture/engine-candidate-pipeline.md 的「6.1.1」。
pub fn lookup_prefix_fallback(
    english: &dyn Engine,
    input: &str,
    min_length: usize,
) -> Vec<Candidate> {
    // 取「配置值」与「英文输入态下限」的较小者：用户把 min_length 调大是为了打中文时
    // 少受打扰，不该连带把「中文侧本来就没候选」时的补全也卡掉。
    let gate = min_length_or_default(min_length).min(EMPTY_BASE_MIN_LENGTH);
    if input.chars().count() < gate {
        return Vec::new();
    }
    let lower = input.to_lowercase();
    let Ok(r) = english.convert(&lower, PREFIX_FALLBACK_MAX) else {
        return Vec::new();
    };
    // 与输入相同（大小写不敏感）的词排最前，其余保持英文引擎的次序。英文引擎本身已把精确
    // 整串排在最前，这里只是不依赖它。
    let (mut out, rest): (Vec<Candidate>, Vec<Candidate>) = r
        .candidates
        .into_iter()
        .map(|mut c| {
            // 同 `lookup`：码表域标志不跨来源带，理由见该函数文档。
            c.is_exact_code = false;
            c
        })
        .partition(|c| c.text.to_lowercase() == lower);
    out.extend(rest);
    out.truncate(PREFIX_FALLBACK_MAX);
    // 清了 `is_exact_code` 后协调器按 weight 排，精确词要抬到页内最高权重之上才能排最前；
    // 没有精确词时不动。码表 `base_sort = "natural"` 时协调器不看 weight，靠英文引擎原序。
    let n_exact = out
        .iter()
        .take_while(|c| c.text.to_lowercase() == lower)
        .count();
    if let Some(top) = out[n_exact..].iter().map(|c| c.weight).max() {
        for c in &mut out[..n_exact] {
            c.weight = c.weight.max(top.saturating_add(1));
        }
    }
    out
}

/// 拼音方案中文侧有候选时，这串输入可不可以补英文前缀（长度门槛另由 [`select`] 判）。
///
/// 判据：整串任何切法都切不成完整的全拼音节序列（`templa` 补，`shang` 不补）。音节表用
/// [`crate::pinyin::syllable::is_full_pinyin_code`]，不含模糊音拼写（宁可多给两条英文，
/// 不为可选开关读引擎状态）。含非字母的串不补。只对全拼成立，双拼由调用方排除。
pub fn prefix_complete_applies(input: &str) -> bool {
    if input.is_empty() || !input.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    !crate::pinyin::syllable::is_full_pinyin_code(&input.to_ascii_lowercase(), 0)
}

/// 中文侧有候选时的英文前缀补全：至多 `max` 条，跳过精确命中（`exact` 即 [`lookup`] 的
/// 结果，调用方会把两者拼在一起）。是否该补、补几条由 [`select`] 定。
///
/// 次序取英文引擎的前缀排序（按词频权重，见 `wind_dict::freq_rank`），故 `produc` 先补
/// product。查询失败静默退化为空，口径同 [`lookup`]。
pub fn lookup_prefix_complete(
    english: &dyn Engine,
    input: &str,
    exact: &[Candidate],
    max: usize,
) -> Vec<Candidate> {
    let lower = input.to_lowercase();
    let Ok(r) = english.convert(&lower, EXACT_SCAN + max) else {
        return Vec::new();
    };
    r.candidates
        .into_iter()
        .filter(|c| c.code != lower && !exact.iter().any(|e| e.text == c.text))
        .map(|mut c| {
            // 同 `lookup`：码表域标志不跨来源带。
            c.is_exact_code = false;
            // 协调器据此把它排在整串拼音真词之后（见该字段文档）。
            c.meta.from_prefix_complete = true;
            c
        })
        .take(max)
        .collect()
}

/// 是否存在英文候选（按前缀判），供上屏否决用（否决必须叠「对方确有候选」，见
/// `wind-engine/AGENTS.md`）。不复用 [`lookup`] 的精确收窄，见该函数文档。
///
/// 英文临时词不算：误打一次就会进临时词，不能让它永久改变主方案的上屏行为。临时词排在同档
/// 系统词之前，故多取几条再筛；取满仍全是临时词按「没有」处理，宁可放行上屏。
pub fn has_any(english: &dyn Engine, input: &str, min_length: usize) -> bool {
    /// 筛掉临时词后仍要有一条：取数上限。
    const PROBE: usize = 8;
    if input.chars().count() < min_length_or_default(min_length) {
        return false;
    }
    let lower = input.to_lowercase();
    match english.convert(&lower, PROBE) {
        Ok(r) => r.candidates.iter().any(counts_for_veto),
        Err(_) => false,
    }
}

/// 英文候选算不算数（否决用）：英文临时词不算，用户词（含已晋升的）照算。理由见 [`has_any`]。
fn counts_for_veto(c: &Candidate) -> bool {
    !c.meta.is_temp_dict || c.meta.is_user_dict
}

/// 整串是否恰是一个英文完整单词（成词级否决用）。门槛与临时词口径同 [`has_any`]。
pub fn has_word(english: &dyn Engine, input: &str, min_length: usize) -> bool {
    /// 精确整串恒居首，同码多形态留余量；同 [`EXACT_SCAN`]。
    const PROBE: usize = EXACT_SCAN;
    if input.chars().count() < min_length_or_default(min_length) {
        return false;
    }
    let lower = input.to_lowercase();
    match english.convert(&lower, PROBE) {
        Ok(r) => r
            .candidates
            .iter()
            .any(|c| c.code == lower && counts_for_veto(c)),
        Err(_) => false,
    }
}

/// 英文要不要否决这次上屏：满码上屏、显示态复评、顶码、满码空码清空共用，码表与混输同口径。
///
/// - 前缀级 `block_commit`：列表里有英文，或有英文词以当前串开头（[`has_any`]）；
/// - 成词级 `block_commit_on_word`：列表里有与整串同码的英文，或整串恰是英文单词（[`has_word`]）。
///
/// `shown` 是这次已进候选列表的候选（没有就传空），英文临时词都不算。长度门槛取
/// [`Effective::veto_min_length`]。
pub fn vetoes_commit(
    english: &dyn Engine,
    input: &str,
    shown: &[Candidate],
    cfg: &Effective,
) -> bool {
    use wind_candidate::CandidateSource;
    let real = |c: &&Candidate| c.source == CandidateSource::English && counts_for_veto(c);
    let gate = cfg.veto_min_length();
    if cfg.block_commit && (shown.iter().any(|c| real(&c)) || has_any(english, input, gate)) {
        return true;
    }
    if cfg.block_commit_on_word {
        let lower = input.to_lowercase();
        return shown.iter().filter(real).any(|c| c.code == lower)
            || has_word(english, input, gate);
    }
    false
}

/// 把英文候选混入 `base`，保证它们活过截断。
///
/// 次序不做保证：本函数只决定「谁活下来」，最终显示序由协调器
/// `candidate_display_order` 无条件重排全部候选决定（candidate-sorting-rules.md §6）。
/// 同 `MixedEngine::truncate_with_pinyin_quota` 的分工。
pub fn merge(base: &mut Vec<Candidate>, english: Vec<Candidate>, max_candidates: usize) {
    if english.is_empty() || max_candidates == 0 {
        return;
    }
    // 同文本时保留基础候选（它才是本方案的正主），把英文那条的码位并进去。
    // `absorb_codes_from` 自行挡掉跨来源合并（两套编码不同域），故此处无需再判 source。
    let mut kept: Vec<Candidate> = Vec::with_capacity(english.len());
    for c in english {
        match base.iter_mut().find(|b| b.text == c.text) {
            Some(existing) => existing.absorb_codes_from(&c),
            None => kept.push(c),
        }
    }
    if kept.is_empty() {
        return;
    }
    // ★ 先腾座、后追加。基础候选通常已经恰好装满 `max_candidates`，
    // 「先 extend 再 truncate」会把英文原样截回去——那正是混输里英文无保底的现场。
    if base.len() + kept.len() > max_candidates {
        base.truncate(max_candidates.saturating_sub(kept.len()));
    }
    base.extend(kept);
}

#[cfg(test)]
#[path = "english_merge_tests.rs"]
mod tests;
