//! 候选词过滤逻辑
//!
//! 与 Go 版本 `wind_input/internal/candidate/filter.go` 对齐。

use crate::candidate::{Candidate, CandidateSource};

/// 过滤模式
///
/// `Default` 取 `Smart`，与 [`from_config`](Self::from_config) 的未知值回退同源，
/// 否则「没配」与「配错」会表现不一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    /// 不过滤
    Gb18030,
    /// 只保留常用词
    General,
    /// 智能过滤（同编码下有常用词则过滤非常用词）
    #[default]
    Smart,
}

impl FilterMode {
    /// 从配置值（`input.filter_mode`）解析。未知值回退 Smart：手改拼错不该让输入法失去过滤，
    /// 故不实现 `FromStr`（那要求返回 `Result`）。
    pub fn from_config(s: &str) -> Self {
        match s {
            "gb18030" => Self::Gb18030,
            "general" => Self::General,
            _ => Self::Smart,
        }
    }

    /// 配置值（`input.filter_mode`），与 [`from_config`](Self::from_config) 成对；
    /// 菜单切换要写回配置（见 `set_filter_mode`）。
    pub fn as_config(&self) -> &'static str {
        match self {
            Self::Gb18030 => "gb18030",
            Self::General => "general",
            Self::Smart => "smart",
        }
    }
}

/// 过滤结果：保留集 + 被滤集。
///
/// 被滤集是「检索范围放宽」的数据来源（见 `docs/design/smart-filter-scope-relax.md`）：
/// 智能档不足一页时从中回补，手动放宽时整体并回，无需重查词库或重排。
pub struct FilterOutcome {
    /// 按当前模式保留、正常显示的候选。
    pub kept: Vec<Candidate>,
    /// 被本次过滤剔除的候选，保持原有相对顺序。`Gb18030`（不过滤）时恒空。
    pub filtered: Vec<Candidate>,
}

/// 「常用词类」判据：常用字 / 短语 / 命令 / 分组一律豁免过滤。
///
/// 公开给 emoji 扩展的「列表末尾」档（常用候选之后、生僻候选之前），两边对「什么算常用」
/// 必须逐字一致。
pub fn is_common_like(c: &Candidate) -> bool {
    c.is_common || is_user_authored(c)
}

/// 用户**自己配出来的**候选：短语、命令、短语分组。
///
/// [`is_common_like`] 在此之上或上 `is_common`；[`single_char_admits`] 不能或上它——
/// 常用词同样 `is_common = true`，带上它单字模式就整个失效。两者别互相替代。
pub fn is_user_authored(c: &Candidate) -> bool {
    c.is_phrase || c.is_command || c.is_group
}

/// 含生僻字的多字候选（词组）在检索范围过滤下的处置。
///
/// [`crate::CommonChars::is_string_common`] 是全称判据，词库正常收录的词只因一个字不在
/// 通用规范汉字表里就整条被判非常用。出厂 [`Keep`](Self::Keep) 取「过滤只作用在单字层、
/// 多字词由词库收录背书」的语义，背景见 `docs/architecture/engine-candidate-pipeline.md`
/// 的「8.1 智能过滤」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RarePhrasePolicy {
    /// 整词放行：多字候选不受检索范围裁剪（出厂）。
    #[default]
    Keep,
    /// 与单字同判：整串每字都常用才留。
    Filter,
}

impl RarePhrasePolicy {
    /// 从配置值（`input.rare_phrase`）解析。未知值回退 [`Keep`](Self::Keep)，与 `Default`
    /// 同源，理由同 [`FilterMode::from_config`]。
    pub fn from_config(s: &str) -> Self {
        match s {
            "filter" => Self::Filter,
            _ => Self::Keep,
        }
    }

    /// 配置值（`input.rare_phrase`）。与 [`from_config`](Self::from_config) 成对。
    pub fn as_config(&self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Filter => "filter",
        }
    }
}

/// 这条候选是否吃「整词放行」豁免：[`Keep`](RarePhrasePolicy::Keep) 档下的**多字**候选。
///
/// 「多字」按字素簇（UAX #29）算，与 [`single_char_admits`] 同一把尺子，故单个 emoji
/// 不吃豁免。别退回 `chars().count() > 1` 数码位，那会把多码位 emoji 当成词放行。
///
/// 两个调用点位置不同：`filter_common_only` 排在 `is_common_like` 之后；`filter_smart`
/// 必须排在最前（`user_rare` 之前），否则「多字词含用户标生僻的字」会从放行翻成滤掉。
/// 代价是出厂档下智能档每条候选都跑一次 `nth(1)`（最多推进两簇）。
pub fn rare_phrase_admits(c: &Candidate, policy: RarePhrasePolicy) -> bool {
    use unicode_segmentation::UnicodeSegmentation;
    policy == RarePhrasePolicy::Keep && c.text.graphemes(true).nth(1).is_some()
}

/// 单条候选在**单字输入模式**（`single_char`）下是否放行。
///
/// 只出单字能大幅提高码表「满码唯一即上屏」的成立率。只有开 / 关一个开关，关时调用方
/// 短路不调本函数（设计见 `docs/design/single-char-mode.md`）。
///
/// 短语 / 命令 / 分组是用户自己配的，照常显示；判据走 [`is_user_authored`]，不是
/// [`is_common_like`]（会连常用词一起豁免）。「一个字」按字素簇（UAX #29）算，别退回
/// `chars().count() == 1` 数码位。
pub fn single_char_admits(c: &Candidate) -> bool {
    is_user_authored(c) || crate::single_markable_char(&c.text).is_some()
}

/// 按模式过滤候选词。`phrase` 是含生僻字的**词**怎么处置，见 [`RarePhrasePolicy`]——
/// 它与 `mode` 正交：`mode` 决定「什么算该滤」，`phrase` 决定「词要不要吃这一刀」。
pub fn filter_candidates(
    candidates: Vec<Candidate>,
    mode: FilterMode,
    phrase: RarePhrasePolicy,
) -> FilterOutcome {
    match mode {
        FilterMode::Gb18030 => FilterOutcome {
            kept: candidates,
            filtered: Vec::new(),
        },
        FilterMode::General => filter_common_only(candidates, phrase),
        FilterMode::Smart => filter_smart(candidates, phrase),
    }
}

/// 只保留常用词、短语、命令、分组（外加 [`Keep`](RarePhrasePolicy::Keep) 档下的多字候选）
fn filter_common_only(candidates: Vec<Candidate>, phrase: RarePhrasePolicy) -> FilterOutcome {
    let (kept, filtered) = candidates
        .into_iter()
        .partition(|c| is_common_like(c) || rare_phrase_admits(c, phrase));
    FilterOutcome { kept, filtered }
}

/// 智能档的分组键：按 `(来源, 码)`，通配结果除外。
///
/// 必须带来源：混输下码表与拼音候选常共用同一 code 串，只按 code 分组会让拼音常用字误滤
/// 同码的码表生僻字。通配结果（[`Candidate::is_wildcard`]）整份一组：用户没打出那些码，
/// 按码分组会把只含生僻字的码位当孤儿放行（见 `docs/design/codetable-wildcard.md` §11）。
#[derive(Clone, PartialEq, Eq, Hash)]
enum SmartGroup {
    Code(CandidateSource, String),
    Wildcard(CandidateSource),
}

impl SmartGroup {
    fn of(c: &Candidate) -> Self {
        if c.is_wildcard {
            Self::Wildcard(c.source)
        } else {
            Self::Code(c.source, c.code.clone())
        }
    }
}

/// 统计每个 [`SmartGroup`]「是否存在常用词」。
fn build_has_common(candidates: &[Candidate]) -> std::collections::HashMap<SmartGroup, bool> {
    use std::collections::HashMap;
    let mut has_common: HashMap<SmartGroup, bool> = HashMap::new();
    for c in candidates {
        let group = SmartGroup::of(c);
        // 先建组（哪怕非常用），使「该码位下无常用词」与「该码位没出现过」区分开。
        has_common.entry(group.clone()).or_insert(false);
        if !is_common_like(c) {
            continue;
        }
        has_common.insert(group, true);
        // 通配候选只写自己那一组，否则同码位的非通配生僻字会被它遮蔽。
        if c.is_wildcard {
            continue;
        }
        // 去重吃掉的同文本码位一并遮蔽（见 `Candidate::merged_codes`），否则全码组只剩生僻字而被
        // 当孤儿放行：同一个字打简码出、打全码反而不出。非常用候选不遮蔽任何人，无需还原。
        for code in &c.merged_codes {
            has_common.insert(SmartGroup::Code(c.source, code.clone()), true);
        }
    }
    has_common
}

/// 智能过滤：同一组（来源+编码；通配结果整份一组，见 [`SmartGroup`]）下有常用词则过滤非常用词
fn filter_smart(candidates: Vec<Candidate>, phrase: RarePhrasePolicy) -> FilterOutcome {
    // 「该组有没有常用词」的统计不吃词豁免：放行的词若也算常用，会遮蔽同码位的生僻单字，
    // 把孤儿码位保底顶掉。
    let has_common = build_has_common(&candidates);
    let (kept, filtered) = candidates.into_iter().partition(|c| {
        // 整词放行（出厂 keep）排在 `user_rare` 之前：词里含被标生僻的字照样放行，
        // `user_rare` 因此只管单字。
        if rare_phrase_admits(c, phrase) {
            return true;
        }
        // 用户亲手标生僻的字无条件滤掉，不吃孤儿编码保底：否则降级同码位唯一的常用字后，保底
        // 又把它原样放回第一位，表现为「设了没反应」。它落进 `filtered`，仍可放宽调出。
        if c.user_rare {
            return false;
        }
        let common_exists = has_common.get(&SmartGroup::of(c)).copied().unwrap_or(false);
        // 同组存在常用词则只保留常用词；否则保留全部（孤儿编码 / 全无常用字的通配结果）
        !common_exists || is_common_like(c)
    });
    FilterOutcome { kept, filtered }
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;
