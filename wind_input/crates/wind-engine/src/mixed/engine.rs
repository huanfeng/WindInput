//! 混合引擎实现（码表主 + 拼音次 + 可选英文）。
//!
//! 合并后按[截断优先级档](MixedEngine::truncation_tier)稳定排序、按文本去重、带保底配额截断；
//! 不改 weight，最终显示序由协调器重排。输入短于 `min_pinyin_length` 时仅码表；超码长走
//! `convert_overflow`。设计见 `docs/architecture/engine-candidate-pipeline.md` §7 与
//! `docs/design/mixed-source-tier-quota.md`。

use crate::engine::{ConvertOptions, ConvertResult, Engine, EngineType};
use std::sync::Arc;
use wind_candidate::{Candidate, CandidateSource};

/// 拼音候选的**保底配额分母**：截断时至少给拼音留 `max_candidates / 此值` 席
/// （生产 `max_candidates=300` ⇒ 60 席）。见 [`MixedEngine::truncate_with_pinyin_quota`]。
const PINYIN_QUOTA_DIVISOR: usize = 5;

/// [`MixedEngine::pinyin_claims_overflow`] 取拼音候选的上限：与协调器给引擎的上限同值，
/// 保证简拼层的 ②b 混合整句活过引擎截断（见该函数内注释）。
const PINYIN_CLAIM_PROBE_LIMIT: usize = 300;

/// 判定[截断优先级档](MixedEngine::truncation_tier)所需的两个判据串。
///
/// 两者都随调用路径变化，故不能从候选自身推出，必须由调用方按语境传入：
/// - `codetable_exact`：主路径 = `input`，overflow = **前 N 码前缀**；
/// - `english_exact`：恒为小写化的 `input`（与 `english_merge::lookup` 同口径）。
#[derive(Clone, Copy)]
struct TruncationCtx<'a> {
    codetable_exact: &'a str,
    english_exact: &'a str,
}

impl MixedEngine {
    /// **截断优先级档**——引擎侧「谁活得下来」的唯一真相源，值越小越先活。
    ///
    /// | 档 | 对象 |
    /// |---|---|
    /// | 0 | 码表精确全码（`code == codetable_exact`） |
    /// | 1 | 短语 —— **本引擎恒不可达，见下** |
    /// | 2 | 码表前缀补全、英文精确 |
    /// | 3 | 拼音、英文前缀 |
    ///
    /// 档位不写进 weight（真实词频与类别偏置挤在同一个 `i32` 会吃掉量程、泄漏到显示序），
    /// 见 `docs/design/mixed-source-tier-quota.md`。
    ///
    /// 档 1 在本引擎恒不可达：短语由协调器在引擎返回之后合并，引擎内无生产代码置位
    /// `is_phrase`。保留它零成本且语义正确。
    ///
    /// 档 2 混着码表前缀补全与英文精确，是旧加成常数碰撞留下的；档内不比权重，英文精确按
    /// 合并顺序落在码表之后，靠 [`MixedEngine::truncate_with_english_seats`] 保底。
    fn truncation_tier(c: &Candidate, ctx: TruncationCtx<'_>) -> u8 {
        match c.source {
            // 短语先判：短语也走码表来源，但优先于「码是否等于输入」。
            CandidateSource::CodeTable if c.is_phrase => 1,
            CandidateSource::CodeTable if c.code == ctx.codetable_exact => 0,
            CandidateSource::CodeTable => 2,
            CandidateSource::English if c.code == ctx.english_exact => 2,
            _ => 3,
        }
    }
}

/// 混输引擎的标量配置（融合策略参数）。引擎部件 primary/secondary/english 单独传入 `new`；
/// 此处仅聚合可配开关/阈值，避免 `new` 参数膨胀。字段语义见 [`MixedEngine`] 同名字段。
#[derive(Debug, Clone)]
pub struct MixConfig {
    pub min_pinyin_length: usize,
    pub pinyin_partial_candidates: bool,
    pub pinyin_partial_candidates_overflow: bool,
    pub auto_commit_block_on_pinyin: bool,
    pub pinyin_only_overflow: bool,
    pub top_code_override_pinyin: bool,
    pub show_source_hint: bool,
    pub min_english_length: usize,
    pub auto_commit_block_on_english: bool,
    /// 成词级英文否决：整串恰是英文完整单词时否决满码 / 顶码上屏。三处同源：本处 /
    /// `MixGlobal::default()` / `data/config.toml`。
    pub block_commit_on_english_word: bool,
    /// 中文侧有候选时按长度补英文前缀的门槛（0 = 不补）。见 `english_merge::select`。
    pub english_complete_min_length: usize,
    /// 中文侧有候选时英文合计至多几条（0 = 默认 2），截断时为它们保底留座。
    pub english_max_count: usize,
    pub block_commit_on_pinyin_word: bool,
    pub pinyin_word_min_weight: i32,
}

impl Default for MixConfig {
    fn default() -> Self {
        Self {
            min_pinyin_length: 2,
            // 三处同源项（见下方 auto_commit_block_on_pinyin 的说明）。
            pinyin_partial_candidates: false,
            pinyin_partial_candidates_overflow: true,
            // 三处同源：本处 / `MixGlobal::default()`（wind-config）/ `data/config.toml
            // [schema.mix]` 必须一致，否则引擎单测跑在现实中不存在的配置下。
            auto_commit_block_on_pinyin: true,
            pinyin_only_overflow: true,
            top_code_override_pinyin: false,
            show_source_hint: false,
            // 与 `manager::build_engine` 的回退值、`english_merge` 的 DEFAULT_MIN_LENGTH 同为 3。
            min_english_length: 3,
            auto_commit_block_on_english: false,
            block_commit_on_english_word: true,
            english_complete_min_length: 0,
            english_max_count: 2,
            block_commit_on_pinyin_word: true,
            pinyin_word_min_weight: 0,
        }
    }
}

/// 混合引擎
pub struct MixedEngine {
    /// 主引擎（码表，如五笔）
    primary: Box<dyn Engine>,
    /// 次引擎（拼音）
    secondary: Option<Box<dyn Engine>>,
    /// 拼音生效的最小输入长度
    min_pinyin_length: usize,
    /// **码长内**（输入 ≤ 主码表最大码长）是否保留「未消费整串」的拼音候选。
    ///
    /// 默认 `false`：`gedw`（五笔「青春」）下拼音会交出 `ge` 的上百条同音单字，每条只解释
    /// 一半击键，主流混输均不出这类候选。代价是失去码长内的分步上屏；前缀补全（`wanl` →
    /// 「完了」，消费整串）不受影响，见 `Engine::convert_requiring_full_match`。
    pinyin_partial_candidates: bool,
    /// **超码长**（输入 > 主码表最大码长，已切入纯拼音语境）是否保留同类候选。
    ///
    /// 默认 `true` = 保留：那里是长拼音输入的地盘，`nihaom` 选「你好」再续打的分步上屏要留着。
    pinyin_partial_candidates_overflow: bool,
    /// 全码自动上屏 / 顶码上屏 / **满码空码清空**时，若存在拼音候选则否决（保护拼音用户，
    /// 对齐 Go AutoCommitBlockOnPinyin）。**默认开**（三处同源：`MixConfig::default()` /
    /// `MixGlobal::default()` / `data/config.toml`）。粗粒度：整串只要查得出拼音候选就让路，
    /// 不看拼音成不成词；细粒度拦截另由 `block_commit_on_pinyin_word`（亦默认开）承担，两者叠加。
    ///
    /// 清空那条通路（`convert` 的 `should_clear`）除「有拼音候选」外还受 `pinyin_may_continue`
    /// （拼音还没打完）支配，二者同归本开关——关闭即「拼音一律不干预码表处置」。
    auto_commit_block_on_pinyin: bool,
    /// 输入超过码表最大码长时仅查拼音（主流混输行为，对齐 Go PinyinOnlyOverflow）。
    /// false 时走「码表前 N 码 + 拼音完整输入」混合 overflow。
    ///
    /// 「仅查拼音」有一个例外口 [`Self::codetable_owns_overflow`]：前 N 码是码表精确全码而拼音
    /// 主张不了整串时，码表候选照样回捞、顶码照样放行。它同时管着本项在 `convert_overflow` 与
    /// `handle_top_code` 两处的表现，改判据须两处一起验。
    pinyin_only_overflow: bool,
    /// 顶码歧义裁决（对齐 Go TopCodeOverridePinyin）：前缀既是完整拼音又是唯一五笔全码时，
    /// true 放行顶码倒向五笔，false（默认）维持拼音保护。
    top_code_override_pinyin: bool,
    /// 主码表最大码长（构建期由 primary.max_code_length() 注入；0 表示未知/不启用溢出分支）。
    max_code_len: usize,
    /// 候选来源标记（对齐 Go addSourceHints）：true 时给拼音候选 comment 加「拼」前缀，
    /// 帮助用户区分混输候选来源。默认 false（零回归）。
    show_source_hint: bool,
    /// 英文词库引擎（schema.mix.enable_english 开且 english 方案可加载时为 Some）。
    /// 混输各路径按精确/前缀加权混入英文候选；None = 关闭（零开销）。
    ///
    /// `Arc` 而非 `Box`：与 `engines["english"]` 是同一个实例，英文引擎带一张随用户词库
    /// 规模增长的词组分词索引，各建一份是纯浪费。
    english: Option<Arc<dyn Engine>>,
    /// 英文取用策略（最小长度 / 补全门槛 / 条数上限 / 上屏否决），与码表方案的
    /// `english_merge` 折成同一形态，经 `english_merge::select` / `has_any` 共用一套口径。
    /// `block_commit` 即 `auto_commit_block_on_english`：存在英文候选（含前缀）时否决满码 /
    /// 顶码上屏。
    english_policy: crate::english_merge::Effective,
    /// 拼音歧义拦截（词强度）：整串是强拼音词时否决五笔自动/顶码上屏，让拼音赢
    /// （wangba→网吧；aipu 无强词则放行落实）。默认开；独立于 auto_commit_block_on_pinyin。
    block_commit_on_pinyin_word: bool,
    /// 词强度权重阈值（0=仅结构判据：拼音首选须 ≥2 汉字且消费整串；预留真机调）。
    pinyin_word_min_weight: i32,
}

impl MixedEngine {
    /// 构造混输引擎：primary（码表主）/ secondary（拼音次）/ english（英文词库，可空）为引擎部件，
    /// 其余融合策略参数经 [`MixConfig`] 传入。
    pub fn new(
        primary: Box<dyn Engine>,
        secondary: Option<Box<dyn Engine>>,
        english: Option<Arc<dyn Engine>>,
        cfg: MixConfig,
    ) -> Self {
        let max_code_len = primary.max_code_length();
        Self {
            primary,
            secondary,
            min_pinyin_length: cfg.min_pinyin_length,
            pinyin_partial_candidates: cfg.pinyin_partial_candidates,
            pinyin_partial_candidates_overflow: cfg.pinyin_partial_candidates_overflow,
            auto_commit_block_on_pinyin: cfg.auto_commit_block_on_pinyin,
            pinyin_only_overflow: cfg.pinyin_only_overflow,
            top_code_override_pinyin: cfg.top_code_override_pinyin,
            max_code_len,
            show_source_hint: cfg.show_source_hint,
            english,
            english_policy: crate::english_merge::Effective {
                enable: true,
                min_length: cfg.min_english_length,
                block_commit: cfg.auto_commit_block_on_english,
                block_commit_on_word: cfg.block_commit_on_english_word,
                complete_min_length: cfg.english_complete_min_length,
                complete_only_non_pinyin: false,
                max_count: crate::english_merge::max_count_or_default(cfg.english_max_count),
            },
            block_commit_on_pinyin_word: cfg.block_commit_on_pinyin_word,
            pinyin_word_min_weight: cfg.pinyin_word_min_weight,
        }
    }

    /// 拼音词否决判据（`block_commit_on_pinyin_word` 开时生效；满码/顶码共用）。命中任一即判为
    /// 「用户意图是拼音（词）」→ 否决五笔上屏。`secondary` 为 None / 开关关时恒 false。
    ///
    /// **(b) 单音节前缀（中途态）**：前 N 码前缀恰是「1 个完整拼音音节」（如 wang）→ 用户多在打
    /// 拼音词的中途（wangb→wangba→网吧），保护拼音。≥2 音节前缀（aipu=ai+pu）已是完整多音节
    /// 单元、多为恰好像拼音的五笔码 → 不拦（放行落实）。这是区分 wang（拦）/ aipu（放）的关键。
    ///
    /// **(a) 整串强拼音词**：整串是完整拼音音节序列、且拼音首选是「≥2 汉字、消费整串」的真实词
    /// （权重 ≥ `pinyin_word_min_weight`）——借拼音引擎自身排序识别（真词排 #1 且消费整串）。
    fn is_ambiguous_pinyin_word(&self, input: &str) -> bool {
        if !self.block_commit_on_pinyin_word {
            return false;
        }
        let Some(sec) = &self.secondary else {
            return false;
        };
        // (b) 前 N 码前缀是单个完整拼音音节 → 中途打拼音词，保护拼音。
        let plen = self.max_code_len.min(input.chars().count());
        if plen >= 1 {
            let prefix: String = input.chars().take(plen).collect();
            if sec.is_whole_syllable_pinyin(&prefix) && sec.completed_syllable_count(&prefix) == 1 {
                return true;
            }
        }
        // (a) 整串是完整拼音强词。
        if !sec.is_whole_syllable_pinyin(input) {
            return false;
        }
        let Ok(r) = sec.convert(input, 8) else {
            return false;
        };
        let Some(top) = r.candidates.first() else {
            return false;
        };
        let input_len = input.chars().count();
        // consumed_length==0 表示引擎未标注（视为整串匹配）。
        let consumes_all = top.consumed_length == 0 || top.consumed_length >= input_len;
        top.text.chars().count() >= 2 && consumes_all && top.weight >= self.pinyin_word_min_weight
    }

    /// 五笔上屏拼音否决（**满码全码自动上屏 / 顶码上屏共用同一套**，保证两条通路一致）：
    /// - ① `auto_commit_block_on_pinyin` 且存在拼音候选（`has_pinyin`）→ 否决（有拼音就让路，粗粒度）；
    /// - ② `block_commit_on_pinyin_word` 且整串是强拼音词（词强度）→ 否决。
    ///
    /// `has_pinyin` 由调用方按各自可见的候选给出（满码=引擎合并前的拼音候选；顶码=对整串查拼音）。
    fn pinyin_vetoes_commit(&self, input: &str, has_pinyin: bool) -> bool {
        (self.auto_commit_block_on_pinyin && has_pinyin) || self.is_ambiguous_pinyin_word(input)
    }

    /// 拼音后续可能性（满码空码清空守护专用）：整串是否**可能**通过继续输入产生拼音候选
    /// （含残缺尾音节，如 zhon→zhong）。这是码表侧 `has_longer_code` 的拼音对偶——码表问
    /// 「有无更长后继码」，拼音问「是不是合法音节前缀」，两者共同构成「这串码还有后续」。
    ///
    /// 与上屏否决 `is_ambiguous_pinyin_word` 的分工：那个判「拼音**已经**成词」（看词典权重），
    /// 这个判「拼音**还没打完**」（只查标准音节表，不查词典）。清空发生在无候选时，正需要后者。
    /// `secondary` 为 None（纯码表混输）时恒 false。
    ///
    /// 已知取舍：不认简拼。若一并认 `is_abbreviation`，几乎任何字母串都会被守护住，清空形同
    /// 虚设；要改须连带重估清空功能本身。
    ///
    /// 前提：混输不接双拼。本判据与另三个音节判据都把入参当全拼直喂音节表；若将来接双拼，
    /// 须先给这四个判据加统一的双拼前置转换，否则会静默误判并清掉正在输入的串。
    fn pinyin_may_continue(&self, input: &str) -> bool {
        self.secondary
            .as_ref()
            .is_some_and(|sec| sec.is_possible_pinyin_sequence(input))
    }

    /// 拼音是否**主张**这个超码长串（「这串确实归拼音管」）。两条任一成立即主张：
    /// - `pinyin_may_continue`：还没打完（`youyo` = you + `yo`，`yo` 是合法音节前缀）；
    /// - 拼音首选**解释了整串**（`consumed_length` 覆盖全长；0 = 引擎未标注，按整串算，与
    ///   `is_ambiguous_pinyin_word` 同口径）。简拼串走的正是这一支——`pinyin_may_continue`
    ///   只认全拼音节前缀，对简拼恒 false（见其文档）。
    ///
    /// 反例：`yijga` 拼音只切得出 `yi`，首选「以」只消费 2/5，不该由拼音独占。
    /// 与 `is_ambiguous_pinyin_word`（成词，用于否决上屏）不同，本函数看覆盖度，用于超码长归属。
    fn pinyin_claims_overflow(&self, input: &str) -> bool {
        if self.pinyin_may_continue(input) {
            return true;
        }
        let Some(sec) = &self.secondary else {
            return false;
        };
        // 上限不能取 1：②b 混合整句带 `is_abbrev`（与整串简拼词同层按权重竞争），引擎序里
        // 排在只吃首音节的部分候选之后，limit 1 会在截断时把它丢掉。取协调器同值，保证它
        // 活过截断；上限只影响截断，不影响引擎算多少。
        let Ok(r) = sec.convert(input, PINYIN_CLAIM_PROBE_LIMIT) else {
            return false;
        };
        let input_len = input.chars().count();
        let consumes_all = |c: &Candidate| c.consumed_length == 0 || c.consumed_length >= input_len;
        // 首选吃满整串，或有整句吃满整串。后一条为 ②b 混合整句：它在简拼层、排在部分候选
        // 之后（`wobzd` 首选是只吃 `wo` 的「我」），见 `tests/mixed_overflow_abbrev_sentence.rs`。
        // 不放宽成「任一候选吃满」：整串简拼词会把本该让给码表的串也判给拼音。
        r.candidates.first().is_some_and(consumes_all)
            || r.candidates
                .iter()
                .any(|c| c.is_sentence && consumes_all(c))
    }

    /// 英文是否**主张**这个超码长串：英文词库里有**精确整串**词条（`github` 是完整英文词）。
    ///
    /// 与 [`Self::pinyin_claims_overflow`] 对称 —— 超码长归属问的是「谁解释得了整串」。码表只
    /// 解释得了前 N 码（`gith`=不算），英文却吃得下整个 `github`，归属就不该判给码表。
    ///
    /// 判据用精确整串而非前缀：英文库前缀面极大，按前缀判会让撞上英文词开头的五笔全码平白
    /// 丢掉归属。也不取候选再比对，那会被取数上限截断。
    ///
    /// 不读 `auto_commit_block_on_english`：那是上屏否决开关，本判据管候选归属，两者正交。
    fn english_claims_overflow(&self, input: &str) -> bool {
        let Some(eng) = &self.english else {
            return false;
        };
        // 英文词库 code 列已小写化（`type = "english"`），查询侧同口径小写。
        eng.has_full_input_match(&input.to_lowercase())
    }

    /// 超码长时**码表前 N 码是否比拼音/英文更有话说**：⓪ `pinyin_only_overflow` 的例外口，
    /// 顶码（`handle_top_code`）与候选装配（`convert_overflow`）共用同一判据。四条缺一不可：
    /// - 前 N 码前缀恰是码表**精确全码**（`yijg` = 唯一编码「就是」）——只有前缀确实成码才值得
    ///   让码表回来；否则捞回的全是前缀补全候选，纯属刷屏。拼音打错一个字母（`nihxo`）也靠这条
    ///   兜住：`nihx` 在五笔没有精确全码 → 仍归拼音，不会被五笔顶码截胡；
    /// - 拼音并不**主张**这一串（见 [`Self::pinyin_claims_overflow`]）；
    /// - 英文并不**主张**这一串（见 [`Self::english_claims_overflow`]）——开着英文词库时 `words`
    ///   的前 4 码 `word` 若在码表成词，码表精确（档 0）会把英文精确（档 2）整层压掉；
    /// - 拼音至少**交得出候选**（见 [`Self::pinyin_has_any`]）——这条与上面第二条方向相反，
    ///   两头夹出「还在中文语境里、但拼音接管不了整串」这个窄带。
    ///
    /// 前三条问「谁解释得了整串」，第四条问「这串还算不算中文」（`github` 连开头都读不出一个字，
    /// 判给码表毫无依据，候选留空让用户上屏原码）。第四条对顶码通路无影响。
    ///
    /// 判据落在前 N 码而非整串：定长码表（五笔 4 码封顶）里不存在更长的词条，问整串的长码特例
    /// 对五笔恒假。详见 `docs/architecture/engine-candidate-pipeline.md` §7.4。
    fn codetable_owns_overflow(&self, input: &str) -> bool {
        if self.max_code_len == 0 {
            return false;
        }
        if self.english_claims_overflow(input) {
            return false;
        }
        if !self.pinyin_has_any(input) {
            return false;
        }
        let prefix: String = input.chars().take(self.max_code_len).collect();
        self.primary.has_full_input_match(&prefix) && !self.pinyin_claims_overflow(input)
    }

    /// 拼音对这串**交得出候选**（哪怕只解释开头一小截）——「这串还在中文语境里」的最低证据。
    ///
    /// 与 [`Self::pinyin_claims_overflow`] 方向相反，两头夹出「还在中文语境、但拼音接管不了整串」
    /// 的窄带：`yijga` 出得来「以」→ 归码表；`github` 什么都出不来 → 谁都不接。
    fn pinyin_has_any(&self, input: &str) -> bool {
        self.secondary.as_ref().is_some_and(|sec| {
            sec.convert(input, 1)
                .map(|r| !r.candidates.is_empty())
                .unwrap_or(false)
        })
    }

    /// 给拼音子引擎的取舍：**码长内**（输入 ≤ 主码表最大码长）。
    ///
    /// `allow_partial_final: Some(false)`：这一段的击键串同时是码表码，残码整句会抢走码表首位
    /// （`aaw` 首选变成「啊啊我」）。
    fn in_code_len_opts(&self) -> ConvertOptions {
        ConvertOptions {
            require_full_match: !self.pinyin_partial_candidates,
            allow_partial_final: Some(false),
            // 混输不筛候选：它的两侧配额与截断另有一套（`merge_sort_dedup`）。
            admit: None,
            // 保留子引擎的简拼配额：协调器会重排，补在尾部是安全的。刻意列全字段，见 `overflow_opts`。
            no_abbrev_quota: false,
            sp_raw_lattice: false,
            aux_continuous: None,
            aux_preedit_separator: None,
            no_correction_tail: false,
        }
    }

    /// 给拼音子引擎的取舍：**超码长**（输入 > 主码表最大码长）。
    ///
    /// `allow_partial_final: Some(true)`：超码长已是纯拼音语境，按纯拼音规矩组句。
    ///
    /// 两侧各写一个方法、列全字段而不传 bool：两套取值表摆在一起才看得出差异（见 [`ConvertOptions`]）。
    fn overflow_opts(&self) -> ConvertOptions {
        ConvertOptions {
            require_full_match: !self.pinyin_partial_candidates_overflow,
            allow_partial_final: Some(true),
            admit: None,
            no_abbrev_quota: false,
            sp_raw_lattice: false,
            aux_continuous: None,
            aux_preedit_separator: None,
            no_correction_tail: false,
        }
    }

    /// 合并（码表在前、拼音在后）→ 按档位稳定排序 → 按文本去重 → 带拼音保底配额截断。
    fn merge_sort_dedup(
        mut codetable: Vec<Candidate>,
        pinyin: Vec<Candidate>,
        max_candidates: usize,
        ctx: TruncationCtx<'_>,
    ) -> Vec<Candidate> {
        codetable.extend(pinyin);
        Self::sort_dedup_truncate(&mut codetable, max_candidates, ctx);
        codetable
    }

    /// 按**截断优先级档**稳定排序 → 按文本去重 → 带拼音保底配额截断
    /// （`convert` 主路径与 overflow 共用）。
    ///
    /// 排序键只有「档」：稳定排序 + 各段按子引擎排好的序拼接 ⇒ 组内保持子引擎原序。
    /// 档内不可再按 weight 重排，否则拼音的层级键（布尔、等价于惩罚 ∞）被抹掉，高词频简拼
    /// 会反超低词频精确候选（candidate-sorting-rules.md §4.3）。
    ///
    /// 本函数只决定谁活下来，显示序由协调器 `candidate_display_order` 重排。
    fn sort_dedup_truncate(
        cands: &mut Vec<Candidate>,
        max_candidates: usize,
        ctx: TruncationCtx<'_>,
    ) {
        cands.sort_by_key(|c| Self::truncation_tier(c, ctx));
        // 按 text 去重，并把被丢弃那条的码位并进幸存者（`Candidate::merged_codes`），否则
        // 「检索范围」过滤按 (source, code) 分组时会丢掉「该码位下有常用字」。跨来源由
        // `absorb_codes_from` 自行挡掉（编码不同域）。
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut deduped: Vec<Candidate> = Vec::with_capacity(cands.len());
        for c in std::mem::take(cands) {
            if let Some(&idx) = seen.get(&c.text) {
                deduped[idx].absorb_codes_from(&c);
                continue;
            }
            seen.insert(c.text.clone(), deduped.len());
            deduped.push(c);
        }
        *cands = deduped;
        Self::truncate_with_english_seats(cands, max_candidates);
    }

    /// 截断时**英文全员保底**，其余候选走 [`Self::truncate_with_pinyin_quota`]。
    ///
    /// 英文已由 `english_merge::select` 限好条数，故全员保底即至多 `english_max_count`。
    /// 不保底的话码表前缀补全洪水会把同在档 2、排在其后的英文精确整片挤掉
    /// （`english_survives_codetable_flood`）。追加在尾部，显示序由协调器重排。
    fn truncate_with_english_seats(cands: &mut Vec<Candidate>, max_candidates: usize) {
        let is_en = |c: &Candidate| c.source == CandidateSource::English;
        if cands.len() <= max_candidates || !cands.iter().any(is_en) {
            Self::truncate_with_pinyin_quota(cands, max_candidates);
            return;
        }
        let (mut english, mut rest): (Vec<Candidate>, Vec<Candidate>) =
            std::mem::take(cands).into_iter().partition(is_en);
        english.truncate(max_candidates);
        Self::truncate_with_pinyin_quota(&mut rest, max_candidates - english.len());
        rest.extend(english);
        *cands = rest;
    }

    /// 截断到 `max_candidates`，但**保证拼音候选至少留 `max/PINYIN_QUOTA_DIVISOR` 席**。
    ///
    /// 与 `pinyin/mod.rs` 的 `truncate_with_quotas` 同构，只差谓词与分母；刻意保持两份，
    /// 改腾位/补位语义前先确认那边是否要跟。
    ///
    /// 为什么需要：码表恒在拼音之前的档，而五笔 2 码前缀的候选量常超过整个上限，那种输入下
    /// 拼音一条都进不来，协调器的拼音精确档无从下手（依据见 candidate-sorting-rules.md §4.3）。
    /// 只补不挤空：截掉的部分没有拼音时一条码表候选都不会被挤掉。
    ///
    /// 已知缺口：取的是被截段里最前面的拼音，而简拼候选（含 ②b 混合整句）沉在拼音块末尾，
    /// 码表候选超上限时会被再截一次。只在开了 `enable_pinyin_abbrev`（出厂关）时可见；
    /// 彻底修法是 extra 选取也给 `is_abbrev` 留一席。
    ///
    /// 补进来的拼音追加在尾部、不保证有序，依赖协调器无条件重排。
    fn truncate_with_pinyin_quota(cands: &mut Vec<Candidate>, max_candidates: usize) {
        if cands.len() <= max_candidates {
            return;
        }
        let quota = max_candidates / PINYIN_QUOTA_DIVISOR;
        let is_py = |c: &Candidate| c.source == CandidateSource::Pinyin;
        if quota == 0 {
            cands.truncate(max_candidates);
            return;
        }
        let kept = cands[..max_candidates].iter().filter(|c| is_py(c)).count();
        if kept >= quota {
            cands.truncate(max_candidates);
            return;
        }
        // 从被截掉的部分按序取拼音补足（拼音引擎已按其排序链排好，前几条即精确候选优先）。
        let extra: Vec<Candidate> = cands[max_candidates..]
            .iter()
            .filter(|c| is_py(c))
            .take(quota - kept)
            .cloned()
            .collect();
        cands.truncate(max_candidates);
        // 腾位：从尾部往前挤掉等量的非拼音候选（权重最低的那些）。
        let mut to_remove = extra.len();
        let mut i = cands.len();
        while to_remove > 0 && i > 0 {
            i -= 1;
            if !is_py(&cands[i]) {
                cands.remove(i);
                to_remove -= 1;
            }
        }
        cands.extend(extra);
    }

    /// 通配合并（spec §10）：主码表通配结果 ⊕ 字面混输结果。
    ///
    /// 次序即**存活优先级**（截断时谁先活；显示序由协调器 `candidate_display_order` 重排）：
    /// 通配等长 → 字面混输结果（其内部已按 `truncation_tier` 排好）→ 通配更长补全。
    ///
    /// 去重分两种口径：码表候选之间按 `(text, code)`（同字不同码各留，学码用，同 spec §3.1）；
    /// 拼音 / 英文候选与**任一**码表候选同字即丢（沿用 `sort_dedup_truncate`「码表留下」）。
    /// ⚠️ 跨来源去重只能在这里做：协调器通配下按 `(text, code)` 去重，拼音「蒸」与码表「蒸」
    /// 码不同，到那里两条都留。码表文本须**先收齐**再过滤：通配更长补全排在字面之后，
    /// 边走边收会漏掉与它同字的拼音。
    fn merge_wildcard(
        wildcard: Vec<Candidate>,
        literal: Vec<Candidate>,
        max_candidates: usize,
    ) -> Vec<Candidate> {
        let (equal, longer): (Vec<Candidate>, Vec<Candidate>) =
            wildcard.into_iter().partition(|c| c.is_exact_code);
        let ct_texts: std::collections::HashSet<String> = equal
            .iter()
            .chain(&literal)
            .chain(&longer)
            .filter(|c| c.source == CandidateSource::CodeTable)
            .map(|c| c.text.clone())
            .collect();
        let mut ct_keys: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut merged: Vec<Candidate> =
            Vec::with_capacity(equal.len() + literal.len() + longer.len());
        for c in equal.into_iter().chain(literal).chain(longer) {
            let keep = if c.source == CandidateSource::CodeTable {
                ct_keys.insert((c.text.clone(), c.code.clone()))
            } else {
                !ct_texts.contains(&c.text)
            };
            if keep {
                merged.push(c);
            }
        }
        Self::truncate_with_pinyin_quota(&mut merged, max_candidates);
        merged
    }

    /// 组合区**默认**形态（`preedit_display`）：≥2 完成音节且有 preedit 时才用拼音拆分串。
    /// 单音节不拆——纯五笔码更不该被拆（cang 显示 cang，不是 cang）。
    fn pinyin_preedit_of(py: &ConvertResult) -> Option<String> {
        if py.completed_syllables.len() >= 2 && !py.preedit_display.is_empty() {
            Some(py.preedit_display.clone())
        } else {
            None
        }
    }

    /// 拼音拆分形态（`preedit_pinyin` → 协调器 `preedit_split_body`），供**高亮跟随**取用：
    /// 高亮拼音候选时显示它，高亮码表候选时显示原始码（`effective_preedit_body`）。
    ///
    /// 判据是「拆分串与原始输入不同」，比 `pinyin_preedit_of` 宽：单音节 + 残码（`nunl` =
    /// `nun` + `l`，「嫩」只消费 3 字符）也要给，否则编码栏显示整串，上屏残留的 `l` 显得无由来。
    ///
    /// 拆分串等于原始输入或为空时返回 None，协调器据此用原始码。
    fn pinyin_split_of(py: &ConvertResult, input: &str) -> Option<String> {
        if py.preedit_display.is_empty() || py.preedit_display == input {
            return None;
        }
        Some(py.preedit_display.clone())
    }

    /// 来源标记（对齐 Go addSourceHints）：给拼音候选 comment 加「拼」前缀，助用户区分混输来源。
    fn add_source_hints(candidates: &mut [Candidate]) {
        for c in candidates.iter_mut() {
            if c.source == CandidateSource::Pinyin {
                if c.comment.is_empty() {
                    c.comment = "拼".to_string();
                } else {
                    c.comment = format!("拼|{}", c.comment);
                }
            }
        }
    }

    /// 进候选列表的英文（enable_english 开时）。`base_empty` = 码表与拼音都没给出候选。
    ///
    /// 取舍与码表方案的英文混入**同一个入口** [`crate::english_merge::select`]：中文侧有候选时
    /// 只收完整单词（够 `english_complete_min_length` 再补前缀词），合计不超
    /// `english_max_count`；中文侧为空时给一页前缀补全。取舍理由见该模块的 `lookup` 文档。
    fn english_for_display(&self, input: &str, base_empty: bool) -> Vec<Candidate> {
        match &self.english {
            Some(eng) => {
                crate::english_merge::select(eng.as_ref(), input, base_empty, &self.english_policy)
            }
            None => Vec::new(),
        }
    }

    /// 英文守护：满码上屏、显示态复评、顶码、满码空码清空共用，判据即
    /// [`crate::english_merge::vetoes_commit`]（与码表方案同口径的两级）：
    ///
    /// - 前缀级 `auto_commit_block_on_english`：有英文词以当前串开头即否决（用户打到 `gith`
    ///   时精确命中还不存在，否决若也收窄成精确，满码就会在第 4 键把中文顶上屏）；
    /// - 成词级 `block_commit_on_english_word`：整串恰是英文完整单词时否决。
    ///
    /// 与 [`Self::english_for_display`]（「显示哪些」）**不要合并**：这里问的是「有没有」。
    /// `shown` 为这次已在列表里的候选（顶码没有列表，传空），英文临时词两半都不算。
    fn english_blocks_commit(&self, input: &str, shown: &[Candidate]) -> bool {
        self.english.as_ref().is_some_and(|eng| {
            crate::english_merge::vetoes_commit(eng.as_ref(), input, shown, &self.english_policy)
        })
    }

    /// 超长输入（input_len > max_code_len）分支：按 pinyin_only_overflow 分流。
    /// - true（默认）：仅查拼音；长码特例下（完整 input 有精确/更长后继）追加码表候选。
    /// - false：码表取前 N 码（+ 长码特例追加完整 input）+ 拼音完整输入，混合竞争。
    fn convert_overflow(&self, input: &str, max_candidates: usize) -> ConvertResult {
        let Some(sec) = &self.secondary else {
            // 无拼音子引擎：退化为码表查完整输入（保持有候选）。
            return self
                .primary
                .convert(input, max_candidates)
                .unwrap_or_default();
        };
        let has_full_or_longer =
            self.primary.has_full_input_match(input) || self.primary.has_longer_code(input);

        if self.pinyin_only_overflow {
            // 超码长用 `overflow_opts`。三处判据函数（`is_ambiguous_pinyin_word` /
            // `pinyin_claims_overflow` / `pinyin_has_any`）刻意仍走 `convert`：它们问归属，
            // 与「哪些候选该显示」正交。
            let py = sec
                .convert_with_opts(input, max_candidates, self.overflow_opts())
                .unwrap_or_default();
            let pinyin_preedit = Self::pinyin_preedit_of(&py);
            let pinyin_split = Self::pinyin_split_of(&py, input);
            let pinyin = py.candidates;
            // 码表回捞（两条互补的口子，任一成立即把码表候选并回来）：
            // - 长码特例 `has_full_or_longer`：**整串**在码表有精确匹配/更长后继。只有码长可变
            //   的码表够得着——五笔这类定长码表恒假（4 码封顶，不存在 5 码词条）。
            // - `codetable_owns_overflow`：**前 N 码**是精确全码而拼音并不主张这一串
            //   （`yijg`+任意字母）。这条才是定长码表的逃生口，与顶码 ⓪ 共用判据。
            let ct_owns = self.codetable_owns_overflow(input);
            // 截断档位的判据串随分支而异（整串 vs 前 N 码前缀），取错会让档 0/档 2 整体错位。
            let english_exact = input.to_lowercase();
            let mut merged = if has_full_or_longer || ct_owns {
                // 与候选一同返回本分支的判据串，就地成对产出，不在别处重算一遍。
                let (mut ct, ct_exact) = if has_full_or_longer {
                    let full = self
                        .primary
                        .convert(input, max_candidates)
                        .unwrap_or_default()
                        .candidates;
                    (full, input.to_string())
                } else {
                    // 前 N 码前缀候选：截断档位上视作精确全码（同混合 overflow 分支的口径），
                    // 但 `is_exact_code` 归一到完整输入（恒 false），免得下游提拔进精确档。
                    let prefix: String = input.chars().take(self.max_code_len).collect();
                    let mut pre = self
                        .primary
                        .convert(&prefix, max_candidates)
                        .unwrap_or_default()
                        .candidates;
                    for c in &mut pre {
                        c.is_exact_code = false;
                        // 只解释得了前 N 码，须如实标消费长度，否则协调器按消费整串上屏、吃掉尾码。
                        // 这是码表候选带 `consumed_length` 的唯一出口（协调器侧对应见
                        // engine-candidate-pipeline.md §7.4）。字节长度：缓冲恒为 ASCII。
                        c.consumed_length = prefix.len();
                    }
                    (pre, prefix)
                };
                // 英文候选（enable_english 开时）：与拼音/码表统一混入（对齐 Go 各路径处理英文）。
                let english = self.english_for_display(input, ct.is_empty() && pinyin.is_empty());
                ct.extend(english);
                let ctx = TruncationCtx {
                    codetable_exact: &ct_exact,
                    english_exact: &english_exact,
                };
                Self::merge_sort_dedup(ct, pinyin, max_candidates, ctx)
            } else {
                let english = self.english_for_display(input, pinyin.is_empty());
                if english.is_empty() {
                    pinyin
                } else {
                    // 纯拼音 + 英文：英文精确进档 2、其余与拼音同档 3。
                    // 本分支无码表候选，`codetable_exact` 取什么都不影响档位。
                    let ctx = TruncationCtx {
                        codetable_exact: input,
                        english_exact: &english_exact,
                    };
                    Self::merge_sort_dedup(english, pinyin, max_candidates, ctx)
                }
            };
            if self.show_source_hint {
                Self::add_source_hints(&mut merged);
            }
            let is_empty = merged.is_empty();
            ConvertResult {
                candidates: merged,
                preedit_pinyin: pinyin_split.unwrap_or_default(),
                preedit_display: pinyin_preedit.unwrap_or_else(|| input.to_string()),
                is_empty,
                ..Default::default()
            }
        } else {
            // 混合 overflow：码表前 N 码 + 拼音完整输入。
            let prefix: String = input.chars().take(self.max_code_len).collect();
            let mut codetable = self
                .primary
                .convert(&prefix, max_candidates)
                .unwrap_or_default()
                .candidates;
            if has_full_or_longer {
                let full = self
                    .primary
                    .convert(input, max_candidates)
                    .unwrap_or_default();
                codetable.extend(full.candidates);
            }
            // `is_exact_code` 归一到完整输入：两次 convert 的输入不同，混着两种「精确」定义，
            // 而下游一律以完整输入为准，不归一会让只匹配前缀的候选被提拔进精确档。
            for c in &mut codetable {
                c.is_exact_code = c.code == input;
            }
            // 超码长用 `overflow_opts`（判据函数仍走 `convert`，理由同上一分支）。
            let py = sec
                .convert_with_opts(input, max_candidates, self.overflow_opts())
                .unwrap_or_default();
            let pinyin_preedit = Self::pinyin_preedit_of(&py);
            let pinyin_split = Self::pinyin_split_of(&py, input);
            let pinyin = py.candidates;
            // 英文候选（enable_english 开时）：并入码表位，与拼音一同竞争。
            let english =
                self.english_for_display(input, codetable.is_empty() && pinyin.is_empty());
            codetable.extend(english);
            // 本分支截断档位「视作精确全码」的是前 N 码前缀，与 `is_exact_code` 的口径刻意不同。
            let ctx = TruncationCtx {
                codetable_exact: &prefix,
                english_exact: &input.to_lowercase(),
            };
            let mut merged = Self::merge_sort_dedup(codetable, pinyin, max_candidates, ctx);
            if self.show_source_hint {
                Self::add_source_hints(&mut merged);
            }
            let is_empty = merged.is_empty();
            ConvertResult {
                candidates: merged,
                preedit_pinyin: pinyin_split.unwrap_or_default(),
                preedit_display: pinyin_preedit.unwrap_or_else(|| input.to_string()),
                is_empty,
                ..Default::default()
            }
        }
    }
}

impl Engine for MixedEngine {
    /// 子引擎是混输自己建的（不与独立方案共享），故逐个带前缀列出。
    fn memory_parts(&self) -> Vec<crate::engine::MemPart> {
        let sub = |tag: &str, e: &dyn Engine| {
            e.memory_parts().into_iter().map({
                let tag = tag.to_string();
                move |mut p| {
                    p.name = format!("{tag}/{}", p.name);
                    p
                }
            })
        };
        let mut v: Vec<_> = sub("primary", self.primary.as_ref()).collect();
        if let Some(s) = &self.secondary {
            v.extend(sub("secondary", s.as_ref()));
        }
        v
    }

    /// 码元字符集取**主码表子引擎**的。
    ///
    /// 必须显式代理：沿用 trait 默认的 `None` 会让混输方案配的 `input_chars` 静默失效。
    /// 只取 primary：它约束哪些键进码表缓冲，拼音有自己的音节判定。
    ///
    /// 已知边界：次引擎若是带非字母韵母键的双拼（微软双拼 `;`），它的码元集在此被丢弃；
    /// 内置混输的次引擎是全拼，无影响。合并两侧的集须先想清码表侧要不要因此放行。
    fn input_chars(&self) -> Option<&wind_config::CodeCharSet> {
        self.primary.input_chars()
    }

    /// 混输代理主码表的通配键（spec §10：码长内两路同查）。哪几位作通配、何时整串字面
    /// 由协调器裁决（首位 / 超码长 / 拼音分段续转，见 `wind-coordinator/src/wildcard.rs`）。
    fn wildcard_key(&self) -> Option<char> {
        self.primary.wildcard_key()
    }

    /// 反查模式的通配键：代理主码表（spec §3.2）。
    fn reverse_wildcard_key(&self) -> Option<char> {
        self.primary.reverse_wildcard_key()
    }

    /// 影子层：代理主码表（只有反查模式读它，而反查只查主码表）。
    fn disabled_dict_layers(&self) -> Option<&crate::codetable::DisabledDictLayers> {
        self.primary.disabled_dict_layers()
    }

    /// 反查模式转换：只查主码表（字面 + 通配），拼音 / 英文不参与（spec §3.2）。
    fn convert_reverse(
        &self,
        input: &str,
        pattern: &str,
        max_candidates: usize,
    ) -> Option<ConvertResult> {
        self.primary.convert_reverse(input, pattern, max_candidates)
    }

    /// 通配转换（spec §10）。无拼音子引擎 ⇒ 代理主码表；有 ⇒ 字面 `convert(input)`
    /// ⊕ 主码表通配，见 [`Self::merge_wildcard`]。
    ///
    /// 前提：协调器只在整串 ≤ 主码表码长时给 pattern（超码长整串字面），故这里的字面
    /// `convert` 恒走码长内分支。上屏 / 清空意向一律不给（§3.2）；组合区显示原始缓冲，
    /// 拼音拆分形态沿用字面结果，供高亮拼音候选时的「高亮跟随」。
    fn convert_wildcard(
        &self,
        input: &str,
        pattern: &str,
        max_candidates: usize,
    ) -> Option<ConvertResult> {
        let wc = self
            .primary
            .convert_wildcard(input, pattern, max_candidates)?;
        if self.secondary.is_none() {
            return Some(wc);
        }
        let lit = self.convert(input, max_candidates).unwrap_or_default();
        let candidates = Self::merge_wildcard(wc.candidates, lit.candidates, max_candidates);
        let is_empty = candidates.is_empty();
        Some(ConvertResult {
            candidates,
            preedit_pinyin: lit.preedit_pinyin,
            preedit_display: input.to_string(),
            is_empty,
            ..Default::default()
        })
    }

    fn wildcard_code_length(&self) -> usize {
        self.max_code_len
    }

    fn wildcard_mixes_pinyin(&self) -> bool {
        self.secondary.is_some()
    }

    /// 热插拔扩展词库：转发到主/次/英文子引擎，三个都要转，漏掉哪个，那个扩展库开关在
    /// 混输下就看着生效实际没有。
    fn set_dict_enabled(&self, dict_id: &str, enabled: bool) -> bool {
        let a = self.primary.set_dict_enabled(dict_id, enabled);
        let b = self
            .secondary
            .as_ref()
            .is_some_and(|s| s.set_dict_enabled(dict_id, enabled));
        let c = self
            .english
            .as_ref()
            .is_some_and(|s| s.set_dict_enabled(dict_id, enabled));
        a || b || c
    }

    /// 拼音「屏蔽这个词」转给拼音子引擎：混输拼音段与拼音方案共用 `pinyin` 桶的屏蔽集（建图排除、
    /// 整句换替补都在子引擎里）。码表段不受影响——码表不按文字屏蔽。
    fn set_blocked_words(&self, words: std::sync::Arc<std::collections::HashSet<String>>) {
        if let Some(s) = &self.secondary {
            s.set_blocked_words(words);
        }
    }

    fn convert(&self, input: &str, max_candidates: usize) -> anyhow::Result<ConvertResult> {
        if input.is_empty() {
            return Ok(ConvertResult::default());
        }
        let mut timer = crate::pinyin::ConvertStageTimer::new();
        let input_len = input.chars().count();

        // 超长分支：按 pinyin_only_overflow 分流。有意不产生 `should_clear`：超长已是纯拼音
        // 语境，清空会打断长拼音输入，故满码空码清空仅在 `input_len == max_code_len` 生效。
        if self.max_code_len > 0 && input_len > self.max_code_len {
            return Ok(self.convert_overflow(input, max_candidates));
        }

        // 1. 码表候选
        timer.mark("mx");
        let ct = self.primary.convert(input, max_candidates)?;
        timer.mark("mxct");
        // 主码表的全码自动上屏意向（下方按拼音守护 + 合并存活性复核后才放行）。
        let ct_should_commit = ct.should_commit;
        let ct_commit_text = ct.commit_text.clone();
        let ct_should_clear = ct.should_clear;
        // 精确空码补全备选原样上浮，由协调器在最终为空时采纳；不能就地并入，拼音尚未合入，
        // 这一层的「空」不是最终的空。见 `ConvertResult::completion_hints`。
        let ct_completion_hints = ct.completion_hints;
        // 不改 weight：精确/前缀补全的分野由 `truncation_tier` 表达。
        let codetable: Vec<Candidate> = ct.candidates;

        // 2. 拼音候选（输入达到最小长度）
        let mut pinyin: Vec<Candidate> = Vec::new();
        // 多音节拼音的组合区分隔显示（如 "ni hao"）：仅当拼音解析出 ≥2 完成音节时采用，
        // 否则保持原始码（单音节如 "cang" 无需分隔，纯五笔码更不应被拆）。
        let mut pinyin_preedit: Option<String> = None;
        // 高亮跟随用的拆分形态：判据比上面宽（见 `pinyin_split_of`），单音节 + 残码也提供。
        let mut pinyin_split: Option<String> = None;
        // 走 `convert_with_opts`：半截过滤要在截断之前、残码整句是生成期的事，拿结果再加工来不及。
        if input_len >= self.min_pinyin_length
            && let Some(sec) = &self.secondary
            && let Ok(py) = sec.convert_with_opts(input, max_candidates, self.in_code_len_opts())
        {
            pinyin_preedit = Self::pinyin_preedit_of(&py);
            pinyin_split = Self::pinyin_split_of(&py, input);
            // 不改 weight：与码表的隔离由 `truncation_tier` 表达。
            pinyin = py.candidates;
        }
        timer.mark("mxpy");

        // 3. 合并 → 按截断档位稳定排序 → 按文本去重
        //
        // 拼接顺序即组内次序：`sort_dedup_truncate` 只按档位稳定排序，同档保持原有
        // 相对位置，故各段内部子引擎排好的序原样保留。改动此处的顺序会改变同档内的去留。
        let has_pinyin = !pinyin.is_empty();
        // 上屏否决只数非低置信的拼音候选：满码唯一的五笔字不该被一条打错才凑出来的拼音候选拦下。
        let pinyin_vetoes = pinyin.iter().any(|c| !c.is_low_confidence());
        let english = self.english_for_display(input, codetable.is_empty() && !has_pinyin);
        let mut merged = codetable;
        merged.extend(pinyin);
        merged.extend(english);
        // 排序 → 去重 → 带拼音保底配额截断（与 overflow 路径共用，见 `sort_dedup_truncate`）。
        // 码表看 `code == input`，英文看 `code == 小写 input`。
        let ctx = TruncationCtx {
            codetable_exact: input,
            english_exact: &input.to_lowercase(),
        };
        Self::sort_dedup_truncate(&mut merged, max_candidates, ctx);
        timer.mark("mxmerge");

        if self.show_source_hint {
            Self::add_source_hints(&mut merged);
        }

        // 全码自动上屏重评（对齐 Go recheckAutoCommit）：取主码表意向，但若英文守护命中、或
        // 拼音否决①②命中（`pinyin_vetoes_commit`，与顶码同一套）则否决（输入可能是拼音/英文，
        // 留给用户选）；并复核上屏目标在合并结果中仍存活。
        // 英文守护（`english_blocks_commit`，前缀级 / 成词级两级）：用户可能正在输入更长的
        // 英文词（`gith` → github）或已打完一个英文词，否决自动上屏。两道否决经短路仅在码表
        // 确有满码上屏意向时求值（避免每键多跑一次转换）。
        let (should_commit, commit_text) = if ct_should_commit
            && !ct_commit_text.is_empty()
            && !self.english_blocks_commit(input, &merged)
            && !self.pinyin_vetoes_commit(input, pinyin_vetoes)
            && merged.iter().any(|c| c.text == ct_commit_text)
        {
            (true, ct_commit_text)
        } else {
            (false, String::new())
        };

        // 满码空码清空：主码表请求清空 + 拼音守护未拦截。
        //
        // 两道守护同受 `auto_commit_block_on_pinyin` 支配（第四条「拼音让路」通路）：
        // - `has_pinyin`：拼音此刻已出候选；
        // - `pinyin_may_continue`：拼音还没打完（`zhon` 合并结果为空，协调器复核挡不住）。
        // 两道必须一起受开关支配，只放开前者等于没放开（`clear_still_vetoed_even_without_the_nun_entry`）。
        // 关闭开关不牺牲中途态：协调器的 `clear_blocked_by_candidates` 按候选实际形态兜住，
        // 见 engine-candidate-pipeline.md §7.3「空码清空」。
        let pinyin_guards_clear =
            self.auto_commit_block_on_pinyin && (has_pinyin || self.pinyin_may_continue(input));
        // 英文守护同样拦清空，否则会抹掉正在打的英文词。短路：仅在码表确有清空意向时才查。
        let should_clear =
            ct_should_clear && !pinyin_guards_clear && !self.english_blocks_commit(input, &merged);

        let is_empty = merged.is_empty();
        Ok(ConvertResult {
            candidates: merged,
            // 组合区：多音节拼音用音节分隔（ni'hao），否则原始码（五笔为主，简明）。
            // 拼音拆分形态单独留存，供协调器「按高亮候选类型」选择显示原始码 / 拆分串——
            // 它的判据比 preedit_display 宽（单音节 + 残码也给），见 `pinyin_split_of`。
            preedit_pinyin: pinyin_split.unwrap_or_default(),
            preedit_display: pinyin_preedit.unwrap_or_else(|| input.to_string()),
            is_empty,
            should_commit,
            commit_text,
            should_clear,
            completion_hints: ct_completion_hints,
            ..Default::default()
        })
    }

    fn reset(&self) {
        self.primary.reset();
        if let Some(s) = &self.secondary {
            s.reset();
        }
    }

    /// 如实转发主引擎的状态。
    ///
    /// 构建方只在没配拼音子引擎时才给主引擎开整句（见 `EngineManager` 构建混输处），故返回
    /// true 即整句会生效；配着拼音子引擎时恒 false。
    fn sentence_input_enabled(&self) -> bool {
        self.primary.sentence_input_enabled()
    }

    fn engine_type(&self) -> EngineType {
        EngineType::Mixed
    }

    /// 满码自动上屏「显示态」复评：先按与 should_commit 同一套拼音①②/英文守护否决
    /// （复评不得绕过否决），再在**码表来源**候选中判唯一
    /// 精确全码（拼音/英文不参与满码上屏）委托主码表复评。智能过滤掉生僻同码字后剩唯一精确全码
    /// 时放行。`has_pinyin`/`has_english` 按显示候选来源判定（与所见一致）。
    fn recheck_auto_commit(&self, input: &str, candidates: &[Candidate]) -> Option<String> {
        let has_pinyin = candidates
            .iter()
            .any(|c| c.source == CandidateSource::Pinyin && !c.is_low_confidence());
        if self.english_blocks_commit(input, candidates)
            || self.pinyin_vetoes_commit(input, has_pinyin)
        {
            return None;
        }
        let ct: Vec<Candidate> = candidates
            .iter()
            .filter(|c| c.source == CandidateSource::CodeTable)
            .cloned()
            .collect();
        self.primary.recheck_auto_commit(input, &ct)
    }

    /// 顶码裁决（对齐 Go HandleTopCode）：超码长时**用与满码全码自动上屏完全相同的拼音①②否决**
    /// （`pinyin_vetoes_commit`），未被否决才委托主码表顶码。两条上屏通路同一套判据，杜绝
    /// "满码不否决、顶码却否决"的不一致。
    ///
    /// - ⓪ `pinyin_only_overflow` 且整串有拼音候选 → 超码长即纯拼音语境，抑制顶码（见下）。
    ///   例外：`codetable_owns_overflow`（前 N 码是精确全码 + 拼音主张不了整串）时放行；
    /// - ① `auto_commit_block_on_pinyin` 且整串有拼音候选 → 抑制顶码（打开时 wangba/aipu 等含拼音
    ///   读法的串都让路拼音）。**与 ⓪ 共用例外口 `codetable_owns_overflow`**：归属已判给码表时
    ///   不再让路（`cety` + 第 5 键 → 顶出「通往」，`ty` 构不成音节、拼音只解释 2/5 键）；
    /// - ② `block_commit_on_pinyin_word` 且整串是强拼音词（wangba→网吧）→ 抑制顶码；
    /// - ③ 英文守护（前缀级 `auto_commit_block_on_english` / 成词级 `block_commit_on_english_word`）
    ///   命中 → 抑制顶码（github→GitHub，见下）；
    /// - `top_code_override_pinyin` 开启 = 顶码优先，无视上述全部否决（名字只提 pinyin 是历史
    ///   局限，它实际是顶码总开关）。
    ///
    /// ⓪ 与 [`Self::convert`] 的超长分流共用同一判据：协调器让顶码先于候选刷新执行，两处不一致
    /// 时顶码恒赢（`youyoud` 第 5 键被顶出「变凉」）。
    ///
    /// 与码表侧判据天然互补，不重复拦截：`CodeTableEngine::handle_top_code` 仅在整串**既无精确
    /// 匹配也无更长后继**时才返回 Some，而 `convert_overflow` 的「长码特例」（`has_full_or_longer`）
    /// 恰是它的补集 —— 顶码想触发的那些串，在 overflow 侧走的正是纯拼音分支。
    fn handle_top_code(&self, input: &str) -> Option<(String, String)> {
        let input_len = input.chars().count();
        if self.max_code_len == 0 || input_len <= self.max_code_len {
            return self.primary.handle_top_code(input);
        }
        // 顶码优先开关关闭时，应用 ⓪③ 与满码同一套拼音①②否决。
        if !self.top_code_override_pinyin {
            // ③ 英文守护：与满码上屏 / 显示态复评同一个判据。自带防卡死：判据要求英文确有
            // 前缀词条，拦下后 overflow 必然交得出候选。放在 `Some(sec)` 块之外：纯码表 + 英文
            // 的混输同样该生效。
            if self.english_blocks_commit(input, &[]) {
                return None;
            }
            if let Some(sec) = &self.secondary {
                // ①的 has_pinyin：整串是否有拼音候选（与满码"合并前拼音候选非空"同义）。
                let has_pinyin = sec
                    .convert(input, 1)
                    .map(|r| !r.candidates.is_empty())
                    .unwrap_or(false);
                // ⓪ 超码长仅查拼音：拼音给得出候选时顶码不该抢。必须叠 `has_pinyin`：纯五笔
                // 溢出串若也禁顶码，用户会卡在既不上屏、又没候选的长串上。
                //
                // 例外口 `codetable_owns_overflow`（前 N 码是精确全码、拼音只解释开头一小截）
                // 放行顶码，且 ⓪① 共用这个归属结论：① 是「让路给拼音」，前提是拼音接得住，
                // 否则候选侧已回捞码表词、顶码侧却被 ① 拦下（`cety` + 第 5 键）。② 不豁免：
                // 词强度与归属正交。见 engine-candidate-pipeline.md §7.3 / §7.4。
                //
                // `has_pinyin == false` 时归属结论恒 false（与 `pinyin_has_any` 同口径），短路省查询。
                let codetable_owns = has_pinyin && self.codetable_owns_overflow(input);
                if self.pinyin_only_overflow && has_pinyin && !codetable_owns {
                    return None;
                }
                if self.pinyin_vetoes_commit(input, has_pinyin && !codetable_owns) {
                    return None;
                }
            }
        }
        self.primary.handle_top_code(input)
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
