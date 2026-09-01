//! 混合引擎实现（码表主 + 拼音次，分层加权合并）
//!
//! 与 Go 版本 `wind_input/internal/engine/mixed/mixed.go` 对齐（核心分层）。
//!
//! 加权策略（双向夹击）：
//! - 码表：精确匹配(code==input) +CodetableWeightBoost(默认 1e7)；短语 +1M；前缀补全 +500K
//! - 拼音：weight ÷ PinyinTierScale(100) 归一化到低档（0~100K），与码表/短语严格隔离
//! - 合并后按权重排序、按文本去重；输入短于 min_pinyin_length 时仅码表
//!
//! 后置：英文候选、简拼长度惩罚（HasFullSyllable）、convertMixedOverflow 精细档。

use crate::engine::{ConvertOptions, ConvertResult, Engine, EngineType};
use wind_candidate::{Candidate, CandidateSource};

/// 拼音候选的**保底配额分母**：截断时至少给拼音留 `max_candidates / 此值` 席
/// （生产 `max_candidates=300` ⇒ 60 席）。见 [`MixedEngine::truncate_with_pinyin_quota`]。
const PINYIN_QUOTA_DIVISOR: usize = 5;

/// 判定[截断优先级档](MixedEngine::truncation_tier)所需的两个判据串。
///
/// 两者都随调用路径变化，故不能从候选自身推出，必须由调用方按语境传入：
/// - `codetable_exact`：主路径 = `input`，overflow = **前 N 码前缀**；
/// - `english_exact`：恒为小写化的 `input`（与 [`MixedEngine::english_candidates`] 同口径）。
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
    /// ## 这套档位从前写在权重里
    ///
    /// 六个加成常数（`+1e7` / `+1M` / `+500K` ×2 / `÷100` / `+0`）一直在表达同一套档位，
    /// 表达方式是**数值大小**：给候选权重加一个足够大的常数，再全局按 weight 排序，档位
    /// 就"自然"浮现。那套写法有三个代价，正是拆掉它的理由：
    ///
    /// 1. **真实词频与类别偏置挤在同一个 `i32` 里**，量程被吃掉（拼音 p50=34 被 `÷100`
    ///    整除归零）；
    /// 2. **档序依赖权重取值范围**——档 0 与档 1 之间隔着 9e6，一条权重超过 9e6 的短语就能
    ///    反超码表精确全码。真实词频到不了那个量级，但那是**数据的性质，不是代码的保证**；
    /// 3. 偏置随 `weight` 一路**泄漏到协调器**，把引擎的截断策略混进了显示序。
    ///
    /// ## ⚠️ 档 1（短语）在本引擎恒不可达
    ///
    /// `is_phrase` 在整个 `wind-engine` 里**没有任何生产代码置位**（`freq_rerank` 里的几处
    /// 全是测试 fixture，且 `apply_freq_rerank` 定义并调用于**协调器**）。短语是协调器合并进
    /// 候选列表的（`handle_candidate.rs`），发生在引擎返回**之后**，而本函数只看得见
    /// `primary.convert()`（码表引擎从不置位）、拼音、英文三路。
    ///
    /// 保留本档是因为它零成本且语义正确：真有短语流到这里时它该排在码表前缀补全之前。
    ///
    /// ## ⚠️ 档 2 混着两个来源，是历史包袱
    ///
    /// 「英文精确」与「码表前缀补全」同档，源于旧加成里 `PARTIAL_MATCH_BOOST` 与
    /// `ENGLISH_EXACT_BOOST` **数值恰好相等**（都是 500,000）——常数碰撞，不是设计。
    /// 本步维持同档，但档内不再比权重（见 [`MixedEngine::sort_dedup_truncate`]），于是英文
    /// 精确落在码表前缀补全**之后**（按合并顺序）。英文该不该有保底席位，见
    /// `docs/design/mixed-source-tier-quota.md` §3.3。
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
    pub block_commit_on_pinyin_word: bool,
    pub pinyin_word_min_weight: i32,
}

impl Default for MixConfig {
    fn default() -> Self {
        Self {
            min_pinyin_length: 2,
            // ⚠️ 同为「三处同源」项（见下方 auto_commit_block_on_pinyin 的说明）。
            pinyin_partial_candidates: false,
            pinyin_partial_candidates_overflow: true,
            // ⚠️ 三处同源：本处 / `MixGlobal::default()`（wind-config）/ `data/config.toml
            // [schema.mix]` 必须一致，改默认须同步全部三处。出厂默认以 L1⊕L2⊕L2.5 为准（靠后层覆盖靠前层），
            // 即 data/config.toml 里的值。本处曾长期为 false 而另两处为 true，导致引擎单测跑在一个
            // 现实中不存在的配置下（测试全绿但保护实际是开着的）。
            auto_commit_block_on_pinyin: true,
            pinyin_only_overflow: true,
            top_code_override_pinyin: false,
            show_source_hint: false,
            min_english_length: 2,
            auto_commit_block_on_english: false,
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
    /// 默认 `false`：`gedw`（五笔精确码「青春」）下拼音把 `ge` 的 219 条同音单字全交出来，
    /// 每条只解释 4 键中的 2 键 —— 主流混输实现均不出这类候选。关掉后 `gedw` 只剩「青春」，
    /// 开着简拼时还能让混合简拼词「各单位」从第 221 位浮到第 2 位（同一根因的两面）。
    ///
    /// ⚠️ 代价是**失去码长内的分步上屏**（选一条部分候选先上屏、剩余码续输）。正在输入中的
    /// 拼音不受影响 —— 那是前缀补全（`wanl` → 「完了」，code=`wanle`、消费整串），与残码
    /// 候选方向相反，见 `Engine::convert_requiring_full_match` 的判据说明。
    pinyin_partial_candidates: bool,
    /// **超码长**（输入 > 主码表最大码长，已切入纯拼音语境）是否保留同类候选。
    ///
    /// 默认 `true` = 保留：那里正是长拼音输入的地盘，`nihaom` 选「你好」再续打 `ma` 的分步
    /// 上屏必须留着。与上一项分设两个开关是用户明确要求的取舍（2026-08-14）。
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
    english: Option<Box<dyn Engine>>,
    /// 英文最小触发长度：输入短于此值时不查英文（2 字符以内不匹配 → 默认 3）。
    min_english_length: usize,
    /// 满码自动上屏时若存在英文候选（含前缀）则否决（保护正在输入英文词的用户）。
    auto_commit_block_on_english: bool,
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
        english: Option<Box<dyn Engine>>,
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
            min_english_length: cfg.min_english_length,
            auto_commit_block_on_english: cfg.auto_commit_block_on_english,
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
    /// **已知取舍：不认简拼**（`schema.mix.enable_pinyin_abbrev` 开时）。本判据只认全拼音节前缀，
    /// 故简拼中途态若暂无候选仍可能被清空。未做联动是有意的——若一并认 `is_abbreviation`，由于它
    /// 只要求每字母是某音节首字母，几乎任何字母串都会被守护住，清空将形同虚设。现有多个上屏阻止
    /// 选项已能覆盖大部分场景，待真机反馈再定；届时勿只改本函数，须连带重估清空功能的存在意义。
    ///
    /// **前提：混输不接双拼**（码长太接近，产品上不支持）。`is_possible_pinyin_sequence` 与另三个
    /// 音节判据一样，把入参当全拼直喂音节表、不走 `ShuangpinConverter`（不同于 `convert()`）。
    /// 若将来给混输接入双拼，此处会**静默**误判：如小鹤 `nihc`(=ni+hao) 判为「无后续」→ 清空吞掉
    /// 用户正在输入的串。届时须先给这四个判据加统一的双拼前置转换，勿只改本函数。
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
    /// 反面即「拼音打岔了」：`yijga`（五笔全码 `yijg`=就是 再多打一个字母）拼音只切得出 `yi`、
    /// 余下 `jga` 连音节前缀都不是，首选「以」只消费 2/5 —— 这种串不该由拼音独占。
    ///
    /// 与 `is_ambiguous_pinyin_word` 的分工：那个判「拼音**已经**成词」（看词典权重，用于否决
    /// 上屏），本函数判「拼音**够不够格接管整串**」（看覆盖度，用于超码长归属）。
    fn pinyin_claims_overflow(&self, input: &str) -> bool {
        if self.pinyin_may_continue(input) {
            return true;
        }
        let Some(sec) = &self.secondary else {
            return false;
        };
        let Ok(r) = sec.convert(input, 1) else {
            return false;
        };
        let input_len = input.chars().count();
        r.candidates
            .first()
            .is_some_and(|c| c.consumed_length == 0 || c.consumed_length >= input_len)
    }

    /// 英文是否**主张**这个超码长串：英文词库里有**精确整串**词条（`github` 是完整英文词）。
    ///
    /// 与 [`Self::pinyin_claims_overflow`] 对称 —— 超码长归属问的是「谁解释得了整串」。码表只
    /// 解释得了前 N 码（`gith`=不算），英文却吃得下整个 `github`，归属就不该判给码表。
    ///
    /// ⚠️ 判据刻意用**精确整串**而非 `english_candidates` 的「有候选（含前缀）」：英文库 21918 条，
    /// 前缀面极大，按前缀判会让一堆恰好撞上某英文词开头的五笔全码平白丢掉归属。也不走
    /// `english_candidates` 取候选再比对 —— 那会被 `max_candidates` 截断，精确词未必在前几条。
    ///
    /// ⚠️ **不读 `auto_commit_block_on_english`**：那是**上屏否决**开关（出厂 `false`），本判据决定的
    /// 是**候选归属/排序**，两者正交。若受其支配，默认配置的用户照样会看到 `github` 首选是「不算」。
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
    ///   的前 4 码 `word` 若在码表成词，码表精确 `+1e7` 会把英文精确档 `+500K` 整层压掉；
    /// - 拼音至少**交得出候选**（见 [`Self::pinyin_has_any`]）——这条与上面第二条方向相反，
    ///   两头夹出「还在中文语境里、但拼音接管不了整串」这个窄带。
    ///
    /// ⚠️ **前三条的判据是「谁解释得了整串」，第四条问的却是「这串还算不算中文」**，别把它们当成
    /// 一类。真机回归 `github`（英文词库关着）四条里前三条全放行：`gith` 在五笔主库确是精确全码
    /// 「不算」（1822）、`gi` 不成音节所以拼音主张不了、英文引擎压根不在场 —— 于是归属判给码表，
    /// 首选变成「不算」，空格上屏还把整个缓冲吃掉。可这串连开头都解释不出一个字，判给码表毫无
    /// 依据（对比 `yijga` 至少出得来「以」）。第四条即为此而设，落回 249f486 之前的行为：候选
    /// 保持为空，用户空格/回车直接上屏原码。
    ///
    /// ⚠️ 第四条对**顶码通路无影响**：⓪ 的判据是 `pinyin_only_overflow && has_pinyin && !ct_owns`，
    /// `has_pinyin=false` 时整条本就不成立。顶码侧的英文场景另由 ③ `auto_commit_block_on_english`
    /// 负责（出厂 `false`），两者是不同维度，勿混。
    ///
    /// ⚠️ 判据落在**前 N 码前缀**而非整串，这是本函数存在的全部理由：`convert_overflow` 原有的
    /// 逃生口 `has_full_input_match(input) || has_longer_code(input)` 问的是**整串**，而定长码表
    /// （五笔 4 码封顶）里根本不存在 5 码词条 —— 那个条件对五笔恒假，等于没有逃生口，于是
    /// `yijg` + **任意**字母都被拼音「以」整串接管，且关掉全部上屏否决开关也无济于事
    /// （①②③ 与 ⓪ 是独立通路）。真机实测即由此而来。
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
    /// 与 [`Self::pinyin_claims_overflow`] 是**方向相反**的一对：那个问「拼音够不够格接管整串」
    /// （够格就别让码表插手），这个问「拼音是不是连一个字都读不出来」（读不出来说明这串根本不是
    /// 中文码，码表也别硬解释）。`yijga` 出得来「以」→ 归码表；`github` 什么都出不来 → 谁都不接，
    /// 候选留空让用户上屏原码。
    fn pinyin_has_any(&self, input: &str) -> bool {
        self.secondary.as_ref().is_some_and(|sec| {
            sec.convert(input, 1)
                .map(|r| !r.candidates.is_empty())
                .unwrap_or(false)
        })
    }

    /// 给拼音子引擎的取舍：**码长内**（输入 ≤ 主码表最大码长）。
    ///
    /// `allow_partial_final: Some(false)` —— 这一段的击键串同时是码表码，让尾部残码参与整句
    /// 解码会抢走码表首位（真机 `aaw`，本意五笔 `aawt`→「工作」，首选变成「啊啊我」）。
    fn in_code_len_opts(&self) -> ConvertOptions {
        ConvertOptions {
            require_full_match: !self.pinyin_partial_candidates,
            allow_partial_final: Some(false),
            // 混输不筛候选：它的两侧配额与截断另有一套（`merge_sort_dedup`）。
            admit: None,
        }
    }

    /// 给拼音子引擎的取舍：**超码长**（输入 > 主码表最大码长）。
    ///
    /// `allow_partial_final: Some(true)` —— 定长码表之外的串不可能是码表码，这里已是纯拼音
    /// 语境，就该按纯拼音的规矩组句（`zaiyebuj` → 「在也不就」，此前尾字母 `j` 被丢下）。
    ///
    /// ⚠️ 两侧**各写一个方法而不是给同一个函数传 bool**：它们是两套取值表，摆在一起才看得出
    /// 「同一维度在两侧取值相反」（对照表见 [`ConvertOptions`]）。
    fn overflow_opts(&self) -> ConvertOptions {
        ConvertOptions {
            require_full_match: !self.pinyin_partial_candidates_overflow,
            allow_partial_final: Some(true),
            admit: None,
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
    /// ## 排序键只有「档」一个，这是刻意的
    ///
    /// `sort_by_key` 是**稳定排序**，同档候选保持原有相对次序。而候选是按
    /// `码表 → 拼音 → 英文` 拼接的，**每一段内部已经是子引擎排好的序**
    /// （码表 `cmp_exact_first().then(base_cmp)`，拼音
    /// `cmp_match_layers().then(weight).then(natural_order)`）。
    /// ⇒ 只按档位稳定排序 = 档位分组 + **组内保持子引擎原序**，无须额外记录任何索引。
    ///
    /// ## ⚠️ 档内绝不可再按 weight 重排
    ///
    /// 曾经的写法是全局按（带加成的）weight 排序，那会把子引擎的排序链整个抹掉。代价
    /// 具体是：拼音的 `cmp_match_layers` 里，层级键（简拼 / 前缀补全 / 子短语 / 全拼降级）
    /// 是**布尔的、等价于惩罚 ∞**，weight 表达不了——于是一条高词频简拼候选会在这里
    /// 反超低词频的精确候选，而子引擎明明把精确排在了前面。
    ///
    /// 同一条教训另有两处记载：`candidate-sorting-rules.md` 红线，以及协调器
    /// `candidate_display_order` 的「层级键必须原样复用 `cmp_match_layers`，不要另写一份」。
    ///
    /// ## 与协调器的分工
    ///
    /// 本函数只决定**谁活下来**。最终显示序由协调器 `candidate_display_order` 无条件重排
    /// 全部候选决定（`candidate-sorting-rules.md` §6），故这里的组间次序不影响用户所见。
    fn sort_dedup_truncate(
        cands: &mut Vec<Candidate>,
        max_candidates: usize,
        ctx: TruncationCtx<'_>,
    ) {
        cands.sort_by_key(|c| Self::truncation_tier(c, ctx));
        // 按 text 去重，并把被丢弃那条所占的码位并进幸存者（`Candidate::merged_codes`）：
        // 否则「检索范围」过滤按 (source, code) 分组时会丢掉「该码位下有常用字」这一事实，
        // 同一个字打前缀出、打全码反而不出。跨来源（码表 vs 拼音）由 `absorb_codes_from`
        // 自行挡掉——两套编码不同域，并入会造出假的同码关系。
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
        Self::truncate_with_pinyin_quota(cands, max_candidates);
    }

    /// 截断到 `max_candidates`，但**保证拼音候选至少留 `max/PINYIN_QUOTA_DIVISOR` 席**。
    ///
    /// **为什么需要**：码表候选落在档 0/档 2、拼音在档 3，于是截断时码表恒排在前。
    /// 而五笔 2 码前缀的候选量常常超过整个配额——实测 **52 个 2 码前缀条目数 > 300**
    /// （最多 `kh` 663 条），其中 `pu`（495 条）正是「既是五笔 2 码、又是完整拼音音节」的交集。
    /// 那种输入下拼音候选**一条都进不了列表**，下游协调器的拼音精确档
    /// （`cmp_pinyin_exact_first`）就无从下手——提档提不了不在场的候选。
    ///
    /// **只补不挤空**：尾部确实没有拼音候选时（纯五笔溢出串、`kh` 这类非音节码）`extra` 为空，
    /// 一条码表候选都不会被挤掉，行为与改动前完全一致。
    ///
    /// ⚠️ 补进来的拼音候选**追加在尾部、不保证有序**——这依赖协调器
    /// `candidate_display_order` 会**无条件重排全部候选**（见 candidate-sorting-rules.md §6）。
    /// 本函数的职责只是「让候选进得来」，不是「排好序」。
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
    /// 判据是「**拆分串与原始输入不同**」，而非 `pinyin_preedit_of` 的「≥2 完成音节」。
    /// 后者是「默认显示什么」的保守取舍，套到本字段上会漏掉**单音节 + 尾部残码**：
    /// `nunl` = 完成音节 `nun`（稀有音节，见 `syllable.rs` 末尾）+ 残码 `l`，拼音候选「嫩」
    /// 只消费 3 个字符，而编码栏却显示整串 `nunl` —— 候选按 `nun|l` 算、显示按整串算，
    /// 用户看不出引擎已把 `l` 划到音节外，空格上屏残留一个 `l` 便显得无由来。
    ///
    /// 拆分串 == 原始输入时返回 None（`nun`、纯五笔码 `aaaa` 的 `a'a'a'a` 除外——那确实不同，
    /// 但它只在高亮到拼音候选时才显示，那时拆分正是该候选的真实解读）。空串同样返回 None：
    /// 空 = 「无拆分形态」，协调器据此恒用原始码。
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

    /// 英文候选（enable_english 开时）：查英文词库，供混入合并。
    ///
    /// `english` 为 None（关闭）时返回空。输入小写化以匹配英文词库（code 列已小写化）。
    ///
    /// 精确/前缀的分野现由 [`MixedEngine::truncation_tier`] 表达（档 2 / 档 3），不再改 weight
    /// ——`weight` 从此只承载真实词频。
    fn english_candidates(&self, input: &str, max_candidates: usize) -> Vec<Candidate> {
        let Some(eng) = &self.english else {
            return Vec::new();
        };
        // 英文最小长度：短输入（默认 2 字符以内）不查英文，避免短前缀刷屏（对齐拼音 min 思路）。
        if input.chars().count() < self.min_english_length {
            return Vec::new();
        }
        let lower = input.to_lowercase();
        let Ok(r) = eng.convert(&lower, max_candidates) else {
            return Vec::new();
        };
        r.candidates
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
            // 超码长走**另一个**开关（默认保留部分候选：这里已是纯拼音语境，长拼音的分步
            // 上屏要留着）。上方三处判据函数（`is_ambiguous_pinyin_word` /
            // `pinyin_claims_overflow` / `pinyin_has_any`）**刻意仍走 `convert`**：它们问的是
            // 「拼音够不够格接管这一串 / 这串还算不算中文」，与「哪些候选该显示」正交。
            let py = sec
                .convert_with_opts(input, max_candidates, self.overflow_opts())
                .unwrap_or_default();
            let pinyin_preedit = Self::pinyin_preedit_of(&py);
            let pinyin_split = Self::pinyin_split_of(&py, input);
            let pinyin = py.candidates;
            // 英文候选（enable_english 开时）：与拼音/码表统一混入（对齐 Go 各路径处理英文）。
            let english = self.english_candidates(input, max_candidates);
            // 码表回捞（两条互补的口子，任一成立即把码表候选并回来；档位隔离由
            // `truncation_tier` 负责，不再靠给拼音 ÷100 来避免档位重叠）：
            // - 长码特例 `has_full_or_longer`：**整串**在码表有精确匹配/更长后继。只有码长可变
            //   的码表够得着——五笔这类定长码表恒假（4 码封顶，不存在 5 码词条）。
            // - `codetable_owns_overflow`：**前 N 码**是精确全码而拼音并不主张这一串
            //   （`yijg`+任意字母）。这条才是定长码表的逃生口，与顶码 ⓪ 共用判据。
            let ct_owns = self.codetable_owns_overflow(input);
            // 截断档位的判据串必须与紧邻的 `boost_codetable` 逐字同源——两个分支的
            // 「视作精确全码」口径不同（整串 vs 前 N 码前缀），取错会让档 0/档 2 整体错位。
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
                    // 前 N 码前缀候选：前缀视作精确全码加权（同混合 overflow 分支的口径），
                    // 但 `is_exact_code` 归一到**完整输入** —— 前缀恒短于 input，故一律 false，
                    // 免得下游（协调器 `candidate_display_order` / `freq_rerank`）把只匹配
                    // 前缀的候选当成本次输入的精确匹配提拔进精确档。
                    let prefix: String = input.chars().take(self.max_code_len).collect();
                    let mut pre = self
                        .primary
                        .convert(&prefix, max_candidates)
                        .unwrap_or_default()
                        .candidates;
                    for c in &mut pre {
                        c.is_exact_code = false;
                        // ★ 这条候选只解释得了**前 N 码**，必须如实标注消费长度。不标（码表候选
                        // 恒 0）的话协调器 `commit_selected` 的
                        // `partial = consumed > 0 && consumed < total` 恒为 false ⇒ 按「消费整串」
                        // 处理，选中即把没解释的尾码一并吃掉（`yijga` 选「就是」→ 尾巴上的 `a`
                        // 凭空消失；`github` 选「不算」→ `ub` 消失）。
                        //
                        // ⚠️ 这是**码表候选带 `consumed_length` 的唯一出口**。协调器侧有两处判据
                        // 原本依赖「码表恒 0 ⇒ 永不部分匹配」这个不变量，已随本改动一并对齐：
                        // `build_candidates` 的分段续转（改看最后一段来源）与
                        // `learn_phrase_on_commit`（混输下跳过码表段）。
                        //
                        // 字节长度：协调器按字节切缓冲（`input_buffer[consumed..]` +
                        // `is_char_boundary`），而输入缓冲在此恒为 ASCII 码字符，与字符数相等。
                        c.consumed_length = prefix.len();
                    }
                    (pre, prefix)
                };
                ct.extend(english);
                let ctx = TruncationCtx {
                    codetable_exact: &ct_exact,
                    english_exact: &english_exact,
                };
                Self::merge_sort_dedup(ct, pinyin, max_candidates, ctx)
            } else if !english.is_empty() {
                // 纯拼音 + 英文：英文精确进档 2、其余与拼音同档 3。
                // 本分支无码表候选，`codetable_exact` 取什么都不影响档位。
                let ctx = TruncationCtx {
                    codetable_exact: input,
                    english_exact: &english_exact,
                };
                Self::merge_sort_dedup(english, pinyin, max_candidates, ctx)
            } else {
                pinyin
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
            // `is_exact_code` 归一到**完整输入**：上面两次 convert 分别以 prefix 和 input 为输入，
            // 码表引擎按各自的输入串置位，于是同一个 Vec 里混着两种「精确」定义。而下游一律以
            // 完整输入为准（协调器 `candidate_display_order`、`freq_rerank::freq_tier` 的
            // `code == input`），不归一会让只匹配前缀的候选被提拔进精确档。
            for c in &mut codetable {
                c.is_exact_code = c.code == input;
            }
            // 英文候选（enable_english 开时）：并入码表位，与拼音一同竞争。
            codetable.extend(self.english_candidates(input, max_candidates));
            // 超码长走**另一个**开关（默认保留部分候选：这里已是纯拼音语境，长拼音的分步
            // 上屏要留着）。上方三处判据函数（`is_ambiguous_pinyin_word` /
            // `pinyin_claims_overflow` / `pinyin_has_any`）**刻意仍走 `convert`**：它们问的是
            // 「拼音够不够格接管这一串 / 这串还算不算中文」，与「哪些候选该显示」正交。
            let py = sec
                .convert_with_opts(input, max_candidates, self.overflow_opts())
                .unwrap_or_default();
            let pinyin_preedit = Self::pinyin_preedit_of(&py);
            let pinyin_split = Self::pinyin_split_of(&py, input);
            let pinyin = py.candidates;
            // 本分支「视作精确全码」的是**前 N 码前缀**，不是 `input`（后者是 `is_exact_code`
            // 的口径，两者刻意不同，见上面那处 `is_exact_code` 归一的说明）。
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
    /// 码元字符集取**主码表子引擎**的。
    ///
    /// ⚠️ 必须显式代理，不能沿用 trait 默认的 `None`：默认值会让协调器回落历史行为，
    /// 于是混输方案里配的 `input_chars` 完全不生效，且**毫无报错**——正是本仓
    /// 「配置就位、消费点不可达」那一类静默失效。
    ///
    /// 只取 primary 是刻意的：`input_chars` 约束的是「哪些键进码表缓冲」，拼音次引擎
    /// 有自己的音节合法性判定，不受本集约束（对齐 `max_code_len` 同样只取 primary）。
    ///
    /// ⚠️ **已知边界**：拼音引擎现在也会产出码元集（双拼布局带非字母韵母键时，如微软
    /// 双拼的 `;` = ing，见 `PinyinEngine::input_chars`），而次引擎的集在此被丢弃。
    /// 内置混输方案的 `secondary_schema` 是全拼 `pinyin`，无影响；若有人把它指向双拼方案，
    /// 那个 `;` 在混输下仍进不了缓冲。合并两侧的集须先想清「码表侧的非码元字符要不要因
    /// 次引擎而放行」，不是加个并集就完事，故留待真有此需求时再定。
    fn input_chars(&self) -> Option<&wind_config::CodeCharSet> {
        self.primary.input_chars()
    }

    /// 热插拔扩展词库：转发到主/次子引擎（码表子引擎承载 codetable-extra 层）。
    fn set_dict_enabled(&self, dict_id: &str, enabled: bool) -> bool {
        let a = self.primary.set_dict_enabled(dict_id, enabled);
        let b = self
            .secondary
            .as_ref()
            .is_some_and(|s| s.set_dict_enabled(dict_id, enabled));
        a || b
    }

    fn convert(&self, input: &str, max_candidates: usize) -> anyhow::Result<ConvertResult> {
        if input.is_empty() {
            return Ok(ConvertResult::default());
        }
        let input_len = input.chars().count();

        // 超长分支（对齐 Go ConvertEx）：输入超过码表最大码长时，按 pinyin_only_overflow 分流，
        // 不再走下方「码表+拼音等长合并」路径。
        //
        // 注：此分支**有意不产生 `should_clear`**（`convert_overflow` 恒返回 false）。超长即已切入
        // 纯拼音语境，「码表满码却无候选」这个前提不再成立，此时清空会打断正常的长拼音输入。
        // 故满码空码清空仅在 `input_len == max_code_len` 生效，勿按「缺口」补齐。
        if self.max_code_len > 0 && input_len > self.max_code_len {
            return Ok(self.convert_overflow(input, max_candidates));
        }

        // 1. 码表候选 + 加权
        let ct = self.primary.convert(input, max_candidates)?;
        // 主码表的全码自动上屏意向（下方按拼音守护 + 合并存活性复核后才放行）。
        let ct_should_commit = ct.should_commit;
        let ct_commit_text = ct.commit_text.clone();
        let ct_should_clear = ct.should_clear;
        // 主码表的精确空码补全备选原样上浮：混输合并后仍可能一条候选都没有（拼音也未命中），
        // 那时才由协调器采纳。此处若就地并入 `codetable` 会重蹈引擎自行判空的覆辙——拼音候选
        // 尚未合入，这一层的「空」同样不是最终的空。见 `ConvertResult::completion_hints`。
        let ct_completion_hints = ct.completion_hints;
        // ⚠️ 码表候选**不再改 weight**：精确/前缀补全的分野由 `truncation_tier` 表达。
        // 从前这里给精确 +1e7、前缀补全 +500K，于是类别偏置与真实词频挤在同一个 i32 里，
        // 还随候选一路泄漏到协调器，把引擎的截断策略混进了显示序。
        let codetable: Vec<Candidate> = ct.candidates;

        // 2. 拼音候选（输入达到最小长度）
        let mut pinyin: Vec<Candidate> = Vec::new();
        // 多音节拼音的组合区分隔显示（如 "ni hao"）：仅当拼音解析出 ≥2 完成音节时采用，
        // 否则保持原始码（单音节如 "cang" 无需分隔，纯五笔码更不应被拆）。
        let mut pinyin_preedit: Option<String> = None;
        // 高亮跟随用的拆分形态：判据比上面宽（见 `pinyin_split_of`），单音节 + 残码也提供。
        let mut pinyin_split: Option<String> = None;
        // ⚠️ 走 `convert_with_opts` 而非 `convert`：两项覆写都必须在拼音引擎**内部**生效
        // （半截过滤要在截断之前，残码整句是生成期的事），在这里拿结果再加工都来不及。
        if input_len >= self.min_pinyin_length
            && let Some(sec) = &self.secondary
            && let Ok(py) = sec.convert_with_opts(input, max_candidates, self.in_code_len_opts())
        {
            pinyin_preedit = Self::pinyin_preedit_of(&py);
            pinyin_split = Self::pinyin_split_of(&py, input);
            // ⚠️ 拼音候选**不再 ÷100**：与码表的隔离由 `truncation_tier` 表达。
            // 那个除法是整数除法，拼音词频中位数 34 会被**整除归零**——量程被偏置
            // 吃掉的最直接后果。
            pinyin = py.candidates;
        }

        // 3. 合并 → 按截断档位稳定排序 → 按文本去重
        //
        // ⚠️ 拼接顺序即**组内次序**：`sort_dedup_truncate` 只按档位稳定排序，同档保持原有
        // 相对位置，故各段内部子引擎排好的序原样保留。改动此处的顺序会改变同档内的去留。
        let has_pinyin = !pinyin.is_empty();
        let mut merged = codetable;
        merged.extend(pinyin);
        merged.extend(self.english_candidates(input, max_candidates));
        // 排序 → 去重 → 带拼音保底配额截断（与 overflow 路径共用，见 `sort_dedup_truncate`）。
        // 判据串与上面那段内联加成逐字同源：码表看 `code == input`，英文看 `code == 小写 input`。
        let ctx = TruncationCtx {
            codetable_exact: input,
            english_exact: &input.to_lowercase(),
        };
        Self::sort_dedup_truncate(&mut merged, max_candidates, ctx);
        if self.show_source_hint {
            Self::add_source_hints(&mut merged);
        }

        // 英文守护（对齐拼音守护）：满码上屏时若存在英文候选（含前缀），说明用户可能正在
        // 输入更长的英文词，否决自动上屏留给用户选择。仅 auto_commit_block_on_english 开时生效。
        let has_english = self.auto_commit_block_on_english
            && merged.iter().any(|c| c.source == CandidateSource::English);

        // 全码自动上屏重评（对齐 Go recheckAutoCommit）：取主码表意向，但若英文守护命中、或
        // 拼音否决①②命中（`pinyin_vetoes_commit`，与顶码同一套）则否决（输入可能是拼音/英文，
        // 留给用户选）；并复核上屏目标在合并结果中仍存活。
        // `pinyin_vetoes_commit` 经短路仅在码表确有满码上屏意向时求值（避免每键多跑一次转换）。
        let (should_commit, commit_text) = if ct_should_commit
            && !ct_commit_text.is_empty()
            && !has_english
            && !self.pinyin_vetoes_commit(input, has_pinyin)
            && merged.iter().any(|c| c.text == ct_commit_text)
        {
            (true, ct_commit_text)
        } else {
            (false, String::new())
        };

        // 满码空码清空：主码表请求清空 + 拼音守护未拦截。
        //
        // 两道守护，**同受 `auto_commit_block_on_pinyin` 支配**（这是第四条「拼音让路」通路，
        // 与 `convert` 满码上屏 / `recheck_auto_commit` 显示态复评 / `handle_top_code` 顶码同源）：
        // - `has_pinyin`：拼音此刻已出候选 → 留给拼音（粗粒度，且合并后非空，协调器亦会复核）；
        // - `pinyin_may_continue`：拼音**还没打完** → 保护中途态。这一项才是无候选时的关键守护：
        //   如 zhon（码表满码无候选无后继、拼音此刻也无候选）合并结果为空，协调器的
        //   `state.candidates.is_empty()` 复核挡不住，若不看后续可能性就会把用户正在输入的
        //   zhong 吞掉。经 `&&` 短路，仅在码表确有清空意向且守护开时才查音节表。
        //
        // ⚠️ 两道**必须一起**受开关支配，只放开 `has_pinyin` 等于没放开：`nunl` 这类
        // 「完整音节 + 单个声母字母」串即便词库里一条候选都没有，`pinyin_may_continue` 仍判
        // 「还没打完」（单字母恒是某音节前缀）而独立拦住清空。见
        // `clear_still_vetoed_even_without_the_nun_entry`。
        //
        // 关闭该开关**不会**牺牲「拼音还没打完」的中途态——那由协调器的第三道门
        // （`clear_blocked_by_candidates`）按候选实际形态兜住，比本处的音节表推测精确得多：
        // 真实词库下 `wanl` 出的是前缀补全候选（code=`wanle`，消费整串）→ 拦住清空，
        // 用户接着打 `wanle` 不会被吞；`zhon`(→zhong 系列) 同理。真正会被清空的只有
        // 「候选全是部分匹配」的串（`nunl` 的「嫩」只解释了 `nun`），即确实打岔了的那些。
        // 实测见 `input_flow.rs` 的 `..._clears_when_only_partial_pinyin` /
        // `..._keeps_prefix_completion_candidates` 单一变量对照。
        let pinyin_guards_clear =
            self.auto_commit_block_on_pinyin && (has_pinyin || self.pinyin_may_continue(input));
        let should_clear = ct_should_clear && !pinyin_guards_clear;

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
    /// ⚠️ 返回 true **不等于**混输下整句会触发：`convert` 超码长直接走 `convert_overflow`，
    /// 只有在没配拼音子引擎（退化为纯码表）时才会经过主引擎。如实转发是为了让协调器的
    /// 分隔符键在那种退化配置下也能放行，以及让「默认关闭」这件事在外部可观测。
    fn sentence_input_enabled(&self) -> bool {
        self.primary.sentence_input_enabled()
    }

    fn engine_type(&self) -> EngineType {
        EngineType::Mixed
    }

    /// 满码自动上屏「显示态」复评：先按**与 should_commit 同一套**拼音①②/英文守护否决
    /// （避免复评绕过否决——修"满码全码唯一自动上屏时不否决"），再在**码表来源**候选中判唯一
    /// 精确全码（拼音/英文不参与满码上屏）委托主码表复评。智能过滤掉生僻同码字后剩唯一精确全码
    /// 时放行。`has_pinyin`/`has_english` 按显示候选来源判定（与所见一致）。
    fn recheck_auto_commit(&self, input: &str, candidates: &[Candidate]) -> Option<String> {
        let has_pinyin = candidates
            .iter()
            .any(|c| c.source == CandidateSource::Pinyin);
        let has_english = self.auto_commit_block_on_english
            && candidates
                .iter()
                .any(|c| c.source == CandidateSource::English);
        if has_english || self.pinyin_vetoes_commit(input, has_pinyin) {
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
    /// - ③ `auto_commit_block_on_english` 且整串有英文候选 → 抑制顶码（github→GitHub，见下）；
    /// - `top_code_override_pinyin` 开启 = 顶码优先，**无视**上述全部否决强制倒向五笔。
    ///   （该名字只提 pinyin 属历史局限，它实际是顶码总开关，⓪①②③ 一律受其压制。）
    ///
    /// ⓪ 与 [`Self::convert`] 的超长分流**共用同一个判据**（`input_len > max_code_len` +
    /// `pinyin_only_overflow`）。此前本函数完全不读 `pinyin_only_overflow`，于是同一次按键里
    /// `convert` 判「切入纯拼音语境」、`handle_top_code` 却委托纯码表顶掉前 N 码；而协调器
    /// （`coordinator.rs` 字母键臂）让顶码**先于**候选刷新执行 → 顶码恒赢，`convert_overflow`
    /// 的纯拼音分支只在拼音否决①②恰好命中时才够得着。混输下打 `youyoud`（悠悠的）在第 5 键
    /// 被顶出「变凉」+ 余码 `oud` 即此漏的实例。
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
            // ③ 英文守护：与满码上屏（`convert`）/ 显示态复评（`recheck_auto_commit`）**同一个
            // 开关**，补齐第三条上屏通路。此前 `auto_commit_block_on_english` 全仓只有那两个
            // 使用点，顶码一个都没有 —— 用户开了「有英文候选时否决上屏」，打 github 到第 5 键
            // 仍被顶出五笔词「不算」（`gith` 在主码表有词），与 ⓪ 同构的漏。
            //
            // 自带防卡死，无需 ⓪ 那样的额外条件：判据要求「英文确有候选」，而它与
            // `convert_overflow` 调的是同一个 `english_candidates`、同一个 `input` ——
            // 拦下顶码后 overflow 必然交得出那批候选。
            //
            // ⚠️ 刻意放在下方 `Some(sec)` 块**之外**：英文守护与拼音子引擎无关，
            // 纯码表 + 英文的混输（secondary=None）同样该生效。
            if self.auto_commit_block_on_english && !self.english_candidates(input, 1).is_empty() {
                return None;
            }
            if let Some(sec) = &self.secondary {
                // ①的 has_pinyin：整串是否有拼音候选（与满码"合并前拼音候选非空"同义）。
                let has_pinyin = sec
                    .convert(input, 1)
                    .map(|r| !r.candidates.is_empty())
                    .unwrap_or(false);
                // ⓪ 超码长仅查拼音：本串已归拼音管，只要拼音真给得出候选，顶码就不该抢。
                //
                // **必须叠 `has_pinyin`，不可只看开关**：纯五笔溢出串（aaaab 之类，拼音一条
                // 候选都没有）若也禁顶码，`convert_overflow` 的纯拼音分支同样交不出候选——
                // 用户会卡在一个既不上屏、又没候选的长串上，没有出口。
                //
                // **例外口 `codetable_owns_overflow`**：前 N 码是精确全码而拼音只解释得了开头
                // 一小截（`yijg`=就是，再打任意字母 → 拼音只切出 `yi`，余下连音节前缀都不是）。
                // 这种串归码表，放行顶码。没有这个例外时 ⓪ 是一票独占：用户把 ①②③ 全关也
                // 改变不了结果——它们是彼此独立的通路，⓪ 只受 `top_code_override_pinyin` 压制。
                // 不必担心卡死：例外成立⇒前缀确有精确全码，码表侧顶码必给得出结果；即便码表侧
                // 因整串有更长后继而返回 None，`convert_overflow` 的长码特例也照样交得出候选。
                //
                // 与 ① 的分工：① 不限超码长（满码时同样生效）、由 `auto_commit_block_on_pinyin`
                // 驱动；⓪ 只在本函数成立（此处已确认 `input_len > max_code_len`）、由
                // `pinyin_only_overflow` 驱动。两者独立配置，任一命中即否决。
                //
                // ★ 归属结论由 ⓪① **共用**：`codetable_owns_overflow` 一旦成立，① 也不得再以
                // 「有拼音候选」为由否决顶码。此前它只豁免 ⓪，于是同一次按键里候选侧已把码表词
                // 回捞到首位、顶码侧却仍被 ① 拦下 —— 两处对同一个归属问题给出相反处置。真机
                // `cety`（唯一全码「通往」）+ 第 5 键即此漏：`ce` 是合法音节但 `ty` 连音节前缀
                // 都不是，拼音只解释 2/5 键，归属明明在码表。
                //
                // ⚠️ 这**反转**了「⓪ 的例外口不是上屏放行口」那条旧决定（原
                // `default_guards_reclaim_candidates_but_still_block_commit` 及其注释）。旧决定把
                // 「出厂 ① 开 ⇒ 回捞但不上屏」记为默认安全，而用户报的正是这一格：顶码在出厂
                // 配置下等于失效。反转依据是语义 —— ① 是「让路给拼音」，让路的前提是拼音确实
                // 接得住这一串，而 `codetable_owns_overflow` 已判定它接不住。
                //
                // ② `block_commit_on_pinyin_word` **不**豁免：它判的是「整串是强拼音词」（词强度），
                // 与「归属」正交，且两者判据基本互斥（成词要求整串是完整音节序列，本例外口却要求
                // 拼音主张不了整串）。故只削弱 `pinyin_vetoes_commit` 的 ① 那一半。
                //
                // `has_pinyin == false` 时本结论恒为 false（第四条判据 `pinyin_has_any` 与
                // `has_pinyin` 同口径），故短路求值 —— 纯五笔溢出串上省掉一次码表 + 拼音查询。
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
mod tests {
    use super::*;
    use crate::codetable::{CodeTableEngine, CommitOptions};
    use std::sync::Arc;
    use wind_dict::cached::CachedDict;
    use wind_dict::codetable::CodetableDict;
    use wind_dict::{DictManager, SystemDictLayer};

    /// 构建一个内存码表引擎（可选开启全码自动上屏）。
    fn ct_engine(entries: &[(&str, &str, i32)], at_full: bool) -> Box<dyn Engine> {
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
        let opts = CommitOptions {
            auto_commit_at_full: at_full,
            auto_commit_min_len: 4,
            ..Default::default()
        };
        Box::new(CodeTableEngine::new(4, opts, Arc::new(dm)))
    }

    // ── 截断的拼音保底配额（`truncate_with_pinyin_quota`）──

    fn ct_cand(text: &str, weight: i32) -> Candidate {
        Candidate {
            text: text.into(),
            weight,
            source: CandidateSource::CodeTable,
            ..Default::default()
        }
    }

    fn py_cand(text: &str, weight: i32) -> Candidate {
        Candidate {
            text: text.into(),
            weight,
            source: CandidateSource::Pinyin,
            ..Default::default()
        }
    }

    /// 复刻 `pu`（495 条码表 + 拼音）现场：码表候选多到吃满整个配额，拼音一条都进不来。
    /// 保底后拼音应拿到 `max/PINYIN_QUOTA_DIVISOR` 席。
    #[test]
    fn pinyin_gets_minimum_quota_when_codetable_floods() {
        let mut cands: Vec<Candidate> = (0..20)
            .map(|i| ct_cand(&format!("码{i}"), 500_000))
            .collect();
        cands.extend((0..5).map(|i| py_cand(&format!("拼{i}"), 100 - i)));
        MixedEngine::truncate_with_pinyin_quota(&mut cands, 10);
        assert_eq!(cands.len(), 10, "总数仍受 max_candidates 约束");
        let py = cands
            .iter()
            .filter(|c| c.source == CandidateSource::Pinyin)
            .count();
        assert_eq!(py, 2, "10/5=2 席保底（否则协调器的拼音精确档无候选可提）");
        // 挤掉的是权重最低的码表候选，码表仍占多数。
        assert_eq!(cands.len() - py, 8);
    }

    /// ★ 反向锁：尾部**没有**拼音候选时（`kh` 663 条这类非音节码、纯五笔溢出串），
    /// 一条码表候选都不许被挤掉 —— 行为与改动前完全一致。
    #[test]
    fn no_pinyin_means_no_codetable_is_evicted() {
        let mut cands: Vec<Candidate> = (0..20)
            .map(|i| ct_cand(&format!("码{i}"), 500_000))
            .collect();
        MixedEngine::truncate_with_pinyin_quota(&mut cands, 10);
        assert_eq!(cands.len(), 10);
        assert!(
            cands.iter().all(|c| c.source == CandidateSource::CodeTable),
            "无拼音可补时不得腾位"
        );
        assert_eq!(cands[0].text, "码0", "顺序不应被打乱");
    }

    /// 未超上限时原样不动（不触发任何腾位逻辑）。
    #[test]
    fn under_limit_is_untouched() {
        let mut cands = vec![ct_cand("码", 500_000), py_cand("拼", 69)];
        MixedEngine::truncate_with_pinyin_quota(&mut cands, 10);
        assert_eq!(cands.len(), 2);
    }

    /// 拼音本就够席位时不额外腾位（避免把配额当成"必须凑满"的硬指标）。
    #[test]
    fn existing_pinyin_above_quota_needs_no_eviction() {
        let mut cands: Vec<Candidate> = (0..5)
            .map(|i| py_cand(&format!("拼{i}"), 900 - i))
            .collect();
        cands.extend((0..20).map(|i| ct_cand(&format!("码{i}"), 100)));
        MixedEngine::truncate_with_pinyin_quota(&mut cands, 10);
        let py = cands
            .iter()
            .filter(|c| c.source == CandidateSource::Pinyin)
            .count();
        assert_eq!(py, 5, "前 10 条里已有 5 条拼音 ≥ 配额 2，不动");
        assert_eq!(cands.len(), 10);
    }

    /// 接线验证：配额逻辑必须真的挂在 `convert` 的截断上，不能只是个没人调的函数
    /// （「函数写对了但生产端不调」是本仓反复出现的欠账形态）。
    #[test]
    fn convert_applies_pinyin_quota() {
        // 码表 20 条同码候选（权重高，会吃满配额）；拼音 5 条。
        let entries: Vec<(String, String, i32)> = (0..20)
            .map(|i| ("aa".to_string(), format!("码{i}"), 9000 - i))
            .collect();
        let refs: Vec<(&str, &str, i32)> = entries
            .iter()
            .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
            .collect();
        let e = MixedEngine::new(
            ct_engine(&refs, false),
            Some(Box::new(FakePinyinMulti { n: 5 })),
            None,
            MixConfig::default(),
        );
        let r = e.convert("aa", 10).unwrap();
        assert_eq!(r.candidates.len(), 10);
        let py = r
            .candidates
            .iter()
            .filter(|c| c.source == CandidateSource::Pinyin)
            .count();
        assert_eq!(py, 2, "convert 必须走带配额的截断");
    }

    // ── 截断的**存活集合**契约（`sort_dedup_truncate`）──
    //
    // 本组测试是「拆权重加成、改来源分组配额」（`docs/design/mixed-source-tier-quota.md`）
    // 的第 1 步：**在改动前锁住现状**。上面那组只断言了各来源的候选**数**，而数相等不等于
    // 集合相等 —— 置顶（shadow pin）在协调器应用、位于引擎截断之后，被截掉的候选规则读得到
    // 却没有目标可顶，表现为「置顶了但没反应」且完全静默。故判据须是集合。
    //
    // 标注 `[改动后必须仍然通过]` 的，是新方案**必须复现**的性质；
    // 标注 `[后续步骤会改]` 的，锁的是当前实现的顺序，改完应当变红——那正是要看见的信号。

    /// 带编码的码表候选（去重与并码位测试用）。
    fn ct_cand_coded(text: &str, code: &str, weight: i32) -> Candidate {
        Candidate {
            code: code.into(),
            ..ct_cand(text, weight)
        }
    }

    fn py_cand_coded(text: &str, code: &str, weight: i32) -> Candidate {
        Candidate {
            code: code.into(),
            ..py_cand(text, weight)
        }
    }

    /// 测试用截断上下文。两个判据串默认取一个**不会命中任何候选**的串，使全部候选落到
    /// 非精确档——想测档 0 / 档 2 的分野时显式传入对应的码。
    fn tctx<'a>(codetable_exact: &'a str, english_exact: &'a str) -> TruncationCtx<'a> {
        TruncationCtx {
            codetable_exact,
            english_exact,
        }
    }

    /// 取 `(source, text)` 集合——**存活集合**判据的统一写法。
    ///
    /// 用 `{:?}` 而非直接排序 `CandidateSource`：给共享 crate 的枚举加 `Ord` 只为一个测试
    /// 排序不值当，且会凭空造出一个「来源有大小」的语义——本设计恰恰在说来源之间不可比。
    fn survivor_set(cands: &[Candidate]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = cands
            .iter()
            .map(|c| (format!("{:?}", c.source), c.text.clone()))
            .collect();
        v.sort();
        v
    }

    /// **[改动后必须仍然通过]** 存活的是**哪些**候选，不只是几条。
    ///
    /// 码表 12 条（权重递减）+ 拼音 5 条，`max=10` ⇒ 拼音保底 2 席、码表 8 席。
    /// 码表留权重最高的 8 条（码0..码7），拼音从被截掉的尾部按序补 2 条（拼0、拼1）。
    #[test]
    fn survivor_set_is_the_contract_not_the_count() {
        let mut cands: Vec<Candidate> = (0..12)
            .map(|i| ct_cand(&format!("码{i}"), 9000 - i))
            .collect();
        cands.extend((0..5).map(|i| py_cand(&format!("拼{i}"), 100 - i)));
        MixedEngine::sort_dedup_truncate(&mut cands, 10, tctx("zzzz", "zzzz"));

        let mut expect: Vec<(String, String)> = (0..8)
            .map(|i| ("CodeTable".to_string(), format!("码{i}")))
            .collect();
        expect.extend((0..2).map(|i| ("Pinyin".to_string(), format!("拼{i}"))));
        expect.sort();
        assert_eq!(survivor_set(&cands), expect);
    }

    /// **[改动后必须仍然通过]** 同 `text` 跨来源时保留**码表**那条。
    ///
    /// 现状靠加成保证（码表精确 +1e7 vs 拼音 ÷100，排序后码表在前，去重保留第一条）。
    /// 拆掉加成后这个隐含保证消失，必须显式定义跨来源保留优先级——本测试就是那条定义。
    /// 用拼音「的」（真实词频 15,378,475，全表最高之一）作对手：没有档位它会碾压码表。
    #[test]
    fn same_text_across_sources_keeps_codetable() {
        let mut cands = vec![
            // 真实词频，不带任何加成——拼音「的」是全表最高之一，若无档位它会碾压码表。
            py_cand_coded("的", "de", 15_378_475),
            ct_cand_coded("的", "r", 9950),
        ];
        MixedEngine::sort_dedup_truncate(&mut cands, 10, tctx("r", "zzzz"));
        assert_eq!(cands.len(), 1, "同文只留一条");
        assert_eq!(
            cands[0].source,
            CandidateSource::CodeTable,
            "跨来源同文须保留码表那条"
        );
        assert_eq!(cands[0].code, "r");
    }

    /// **[改动后必须仍然通过]** 同来源同文并码位；跨来源**不并**（两套编码不同域）。
    ///
    /// 反向锁的理由见 `Candidate::absorb_codes_from`：跨来源并入会给码表凭空造出
    /// 「拼音码位有常用字」的假事实，反过来误滤同码的码表生僻字。
    #[test]
    fn dedup_merges_codes_within_source_only() {
        let mut same = vec![
            ct_cand_coded("的", "r", 900),
            ct_cand_coded("的", "rqto", 100),
        ];
        MixedEngine::sort_dedup_truncate(&mut same, 10, tctx("r", "zzzz"));
        assert_eq!(same.len(), 1);
        assert_eq!(same[0].merged_codes, vec!["rqto"], "同来源并码位");

        let mut cross = vec![
            ct_cand_coded("的", "r", 900),
            py_cand_coded("的", "de", 100),
        ];
        MixedEngine::sort_dedup_truncate(&mut cross, 10, tctx("r", "zzzz"));
        assert_eq!(cross.len(), 1);
        assert!(
            cross[0].merged_codes.is_empty(),
            "跨来源不并码位——并了会造出假的同码关系"
        );
    }

    /// **[改动后必须仍然通过]** 短语必须活过码表洪水。
    ///
    /// 短语在档 1、码表前缀补全在档 2 ⇒ 短语权重再低也先活。短语量本来就极少，被高冲突码
    /// 挤掉是纯粹的功能缺失。
    ///
    /// ⚠️ 本引擎目前**收不到**短语候选（`is_phrase` 无生产置位点，见 `truncation_tier`），
    /// 故这条锁的是函数契约而非生产行为。
    #[test]
    fn phrase_survives_codetable_flood() {
        let mut cands: Vec<Candidate> = (0..20)
            .map(|i| ct_cand(&format!("码{i}"), 1000 - i))
            .collect();
        cands.push(Candidate {
            is_phrase: true,
            weight: 1,
            ..ct_cand("短语正文", 0)
        });
        MixedEngine::sort_dedup_truncate(&mut cands, 5, tctx("zzzz", "zzzz"));
        assert!(
            cands.iter().any(|c| c.is_phrase),
            "短语权重虽为 1，靠 +1M 档位活下来"
        );
    }

    /// **[改动后必须仍然通过]** 码表精确全码不被高频前缀补全挤掉（生产路径 `convert`）。
    ///
    /// 这是加成在承担的**真正职责**：精确 `code == input` 权重可能远低于某条前缀补全，
    /// 靠 +1e7 vs +500K 的档差活下来并排在前。新方案必须复现。
    #[test]
    fn exact_code_outranks_high_weight_prefix_completion() {
        let mut entries: Vec<(String, String, i32)> = vec![("aa".into(), "精确低频".into(), 1)];
        entries.extend((0..8).map(|i| (format!("aab{i}"), format!("补全{i}"), 9000 - i)));
        let refs: Vec<(&str, &str, i32)> = entries
            .iter()
            .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
            .collect();
        let e = MixedEngine::new(ct_engine(&refs, false), None, None, MixConfig::default());
        let r = e.convert("aa", 3).unwrap();
        assert_eq!(
            r.candidates[0].text, "精确低频",
            "精确全码权重仅 1，仍须压过权重 9000 的前缀补全"
        );
    }

    /// **[改动后必须仍然通过]** 码表精确/补全的分野由**档位**保证，不再靠权重量级。
    ///
    /// 档位显式化之后，这条在函数层就成立了——此前只有生产路径成立（靠 +1e7 与 +500K
    /// 的差），直接调本函数时 weight 说了算。
    #[test]
    fn exact_beats_completion_at_function_level_not_only_via_boosts() {
        // 模拟码表子引擎的输出：精确在前（cmp_exact_first），但权重低得多、且**不带加成**。
        let mut cands = vec![
            ct_cand_coded("精确低频", "aa", 1),
            ct_cand_coded("补全", "aab", 9000),
        ];
        MixedEngine::sort_dedup_truncate(&mut cands, 10, tctx("aa", "zzzz"));
        assert_eq!(
            cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["精确低频", "补全"],
            "档 0 恒先于档 2，与权重取值范围无关"
        );
    }

    /// ★ **档内保持子引擎原序**——层级链不再被 weight 抹掉。
    ///
    /// 拼音候选全部落进档 3。若档内按 weight 重排，拼音引擎自己的排序链
    /// （`cmp_match_layers().then(weight)`）就会被抹掉——其中层级键（简拼 / 前缀补全 /
    /// 子短语 / 全拼降级）是**布尔的、等价于惩罚 ∞**，weight 表达不了。高词频简拼会因此
    /// 反超低词频精确候选，而子引擎明明把精确排在了前面。
    ///
    /// 稳定排序 + 只按档位排 ⇒ 同档保持传入次序 = 子引擎原序。
    #[test]
    fn within_tier_keeps_sub_engine_order() {
        let abbrev = Candidate {
            is_abbrev: true,
            ..py_cand_coded("简拼高频", "n", 9000)
        };
        // 子引擎给出的序：精确在前（cmp_match_layers 首键 is_abbrev 判负）。
        let mut cands = vec![py_cand_coded("精确", "ni", 1), abbrev];
        MixedEngine::sort_dedup_truncate(&mut cands, 10, tctx("zzzz", "zzzz"));
        assert_eq!(
            cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["精确", "简拼高频"],
            "档内须保持子引擎原序：词频 9000 的简拼不得反超词频 1 的精确候选"
        );
    }

    /// ⚠️ **英文没有任何保底席位**：码表洪水下被整片挤掉，与词频无关。
    ///
    /// 拼音有 `max/PINYIN_QUOTA_DIVISOR` 保底，**英文一点没有**。英文精确虽与码表前缀补全
    /// 同档（档 2，历史包袱见 [`MixedEngine::truncation_tier`]），但档内按合并顺序，
    /// 码表先入 ⇒ 英文恒排其后。
    ///
    /// ## ⚠️ 这是拆加成时**唯一一处刻意的行为变化**
    ///
    /// 拆之前，`ENGLISH_EXACT_BOOST` 与 `PARTIAL_MATCH_BOOST` 数值恰好相等，于是英文精确
    /// 与码表前缀补全**拼真实词频**——词频高的英文词能赢。那是常数碰撞的产物：两者的词频
    /// 根本不同量纲，比较本身没有意义，没有任何文档说过它们应当平起平坐。
    ///
    /// 影响面有限：**显示序**由协调器 `source_tier` 决定（英文与拼音其余同档），本档位只
    /// 决定**截断存活**。英文该不该有保底席位，见 `mixed-source-tier-quota.md` §3.3。
    #[test]
    fn english_has_no_quota_under_codetable_flood() {
        // 码表 20 条前缀补全（真实词频 9000 递减）。
        let entries: Vec<(String, String, i32)> = (0..20)
            .map(|i| (format!("hel{i}"), format!("码{i}"), 9000 - i))
            .collect();
        let refs: Vec<(&str, &str, i32)> = entries
            .iter()
            .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
            .collect();
        let build = |ct: &[(&str, &str, i32)], english: Box<dyn Engine>| {
            MixedEngine::new(
                ct_engine(ct, false),
                None,
                Some(english),
                MixConfig {
                    auto_commit_block_on_pinyin: false,
                    ..Default::default()
                },
            )
        };
        let has_english = |r: &ConvertResult| {
            r.candidates
                .iter()
                .any(|c| c.source == CandidateSource::English)
        };

        // ★ 正向对照必须先立：没有洪水时英文**在场**。
        //   缺了它，下面三条「不在场」可能只是英文引擎压根没产出，测试变成空转。
        let calm = build(&[("hel", "好", 100)], english_engine(&[("hel", "hel", 1)]));
        assert!(
            has_english(&calm.convert("hel", 5).unwrap()),
            "无洪水时英文候选应当在场——否则下面的断言测不到东西"
        );

        // ① 英文精确、词频远低于码表 ⇒ 挤掉。
        let low = build(&refs, english_engine(&[("hel", "hel", 1)]));
        assert!(!has_english(&low.convert("hel", 5).unwrap()));

        // ② 英文精确、词频**高于**码表 ⇒ 照样挤掉。档内按合并顺序，词频不参与。
        let high = build(&refs, english_engine(&[("hel", "hel", 9999)]));
        assert!(
            !has_english(&high.convert("hel", 5).unwrap()),
            "英文无保底：词频 9999 也拿不到席位（拆加成前这条会赢，见文档注释）"
        );

        // ③ 英文前缀落在档 3，更靠后。
        let prefix = build(&refs, english_engine(&[("hello", "hello", 9999)]));
        assert!(!has_english(&prefix.convert("hel", 5).unwrap()));
    }

    /// 产出多条 `source=Pinyin` 候选的假拼音引擎（`FakePinyin` 只给一条，测不了配额）。
    struct FakePinyinMulti {
        n: usize,
    }
    impl Engine for FakePinyinMulti {
        fn convert(&self, input: &str, _max: usize) -> anyhow::Result<ConvertResult> {
            let candidates = (0..self.n)
                .map(|i| Candidate {
                    text: format!("拼{i}"),
                    code: input.to_string(),
                    weight: 100 - i as i32,
                    source: CandidateSource::Pinyin,
                    ..Default::default()
                })
                .collect();
            Ok(ConvertResult {
                candidates,
                ..Default::default()
            })
        }
        fn reset(&self) {}
        fn engine_type(&self) -> EngineType {
            EngineType::Pinyin
        }
    }

    #[test]
    fn mixed_propagates_auto_commit_without_pinyin() {
        // 主码表唯一全码自动上屏；无次引擎 → 无拼音候选 → 放行。
        let primary = ct_engine(&[("aaaa", "工", 100)], true);
        let e = MixedEngine::new(primary, None, None, MixConfig::default());
        let r = e.convert("aaaa", 50).unwrap();
        assert!(r.should_commit, "无拼音候选时应放行全码上屏");
        assert_eq!(r.commit_text, "工");
    }

    #[test]
    fn mixed_blocks_auto_commit_when_pinyin_present() {
        // 次引擎对同一输入也产出候选（模拟拼音命中）+ 守护①显式开 → 否决上屏。
        let primary = ct_engine(&[("aaaa", "工", 100)], true);
        let secondary = ct_engine(&[("aaaa", "啊啊", 50)], false);
        let e = MixedEngine::new(
            primary,
            Some(secondary),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: true,
                ..Default::default()
            },
        );
        let r = e.convert("aaaa", 50).unwrap();
        assert!(!r.should_commit, "有拼音候选且守护开时应否决全码上屏");
    }

    #[test]
    fn mixed_allows_auto_commit_when_guard_off() {
        // 守护关 → 即便有拼音候选也放行。
        let primary = ct_engine(&[("aaaa", "工", 100)], true);
        let secondary = ct_engine(&[("aaaa", "啊啊", 50)], false);
        let e = MixedEngine::new(
            primary,
            Some(secondary),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                ..Default::default()
            },
        );
        let r = e.convert("aaaa", 50).unwrap();
        assert!(r.should_commit, "守护关时应放行");
        assert_eq!(r.commit_text, "工");
    }

    /// 构建开启顶码上屏的码表引擎（max_code_len=4）。
    fn ct_engine_topcode(entries: &[(&str, &str, i32)]) -> Box<dyn Engine> {
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
        let opts = CommitOptions {
            top_code_commit: true,
            ..Default::default()
        };
        Box::new(CodeTableEngine::new(4, opts, Arc::new(dm)))
    }

    /// 可配假拼音引擎：`word`="" 表示无候选（has_pinyin=false）；`syllables` 同时驱动
    /// is_whole_syllable_pinyin(=`syllables>0`) 与 completed_syllable_count(=`syllables`)——
    /// 用于单测顶码/满码共用的拼音①②否决（含 ②(b) 单音节前缀保护）。
    struct FakePinyin {
        word: &'static str,
        syllables: usize,
    }
    impl Engine for FakePinyin {
        fn convert(&self, input: &str, _max: usize) -> anyhow::Result<ConvertResult> {
            let candidates = if self.word.is_empty() {
                vec![]
            } else {
                vec![Candidate {
                    text: self.word.to_string(),
                    code: input.to_string(),
                    weight: 1000,
                    consumed_length: input.chars().count(),
                    source: CandidateSource::Pinyin,
                    ..Default::default()
                }]
            };
            Ok(ConvertResult {
                candidates,
                ..Default::default()
            })
        }
        fn reset(&self) {}
        fn engine_type(&self) -> EngineType {
            EngineType::Pinyin
        }
        fn is_whole_syllable_pinyin(&self, _prefix: &str) -> bool {
            self.syllables > 0
        }
        fn completed_syllable_count(&self, _prefix: &str) -> usize {
            self.syllables
        }
    }

    // ── 满码空码清空：拼音「后续可能性」守护 ──

    /// 构建开启「满码空码清空」的码表引擎（max_code_len=4）。
    fn ct_engine_clear(entries: &[(&str, &str, i32)]) -> Box<dyn Engine> {
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
        let opts = CommitOptions {
            clear_on_empty_max: true,
            ..Default::default()
        };
        Box::new(CodeTableEngine::new(4, opts, Arc::new(dm)))
    }

    /// 清空守护专用假拼音：**恒无候选**（has_pinyin=false，把协调器的候选非空复核排除在外），
    /// 仅可配「整串是否为合法拼音前缀」——正是本守护要验的那一位。
    struct FakePinyinPrefix {
        may_continue: bool,
    }
    impl Engine for FakePinyinPrefix {
        fn convert(&self, _input: &str, _max: usize) -> anyhow::Result<ConvertResult> {
            Ok(ConvertResult::default())
        }
        fn reset(&self) {}
        fn engine_type(&self) -> EngineType {
            EngineType::Pinyin
        }
        fn is_possible_pinyin_sequence(&self, _prefix: &str) -> bool {
            self.may_continue
        }
    }

    fn mixed_with_prefix_pinyin(may_continue: bool) -> MixedEngine {
        MixedEngine::new(
            ct_engine_clear(&[("aaaa", "工", 100)]),
            Some(Box::new(FakePinyinPrefix { may_continue })),
            None,
            MixConfig::default(),
        )
    }

    #[test]
    fn clear_fires_when_pinyin_cannot_continue() {
        // 满码(4) 码表无候选无后继 + 拼音无候选且非合法前缀 → 清空。
        let r = mixed_with_prefix_pinyin(false).convert("qqqq", 50).unwrap();
        assert!(r.candidates.is_empty(), "前置：此输入确无候选");
        assert!(r.should_clear, "拼音无后续可能时应清空");
    }

    #[test]
    fn clear_vetoed_when_pinyin_may_continue() {
        // 同上，但拼音判「还没打完」（zhon→zhong 中途态）→ 守护住，不得清空。
        // 合并候选为空，协调器的 `state.candidates.is_empty()` 复核挡不住——只能靠这一位。
        let r = mixed_with_prefix_pinyin(true).convert("zhon", 50).unwrap();
        assert!(r.candidates.is_empty(), "前置：此刻确无候选");
        assert!(
            !r.should_clear,
            "拼音仍可能有后续时不得清空，否则吞掉中途输入"
        );
    }

    /// 开关关 → 拼音「还没打完」不再拦清空。用户明确关掉「有拼音候选时否决上屏」即表态
    /// 不要拼音干预，此时满码无候选就该清空（真机诉求：nunl 打满 4 码不清空）。
    #[test]
    fn clear_fires_when_pinyin_guard_disabled() {
        let e = MixedEngine::new(
            ct_engine_clear(&[("aaaa", "工", 100)]),
            Some(Box::new(FakePinyinPrefix { may_continue: true })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                ..Default::default()
            },
        );
        let r = e.convert("zhon", 50).unwrap();
        assert!(
            r.should_clear,
            "① 关时拼音后续可能性不得再拦清空（用户已表态不要拼音干预）"
        );
    }

    /// 开关关 + 拼音**确有候选** → 同样清空。锁住「两道守护一起受开关支配」，
    /// 只放开其中一道等于没放开（nunl 即便无候选也会被 may_continue 拦住）。
    #[test]
    fn clear_fires_when_guard_disabled_even_with_pinyin_candidates() {
        let e = MixedEngine::new(
            ct_engine_clear(&[("aaaa", "工", 100)]),
            Some(Box::new(FakePinyin {
                word: "嫩",
                syllables: 1,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                ..Default::default()
            },
        );
        let r = e.convert("nunl", 50).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "嫩"),
            "前置：拼音此刻确有候选"
        );
        assert!(r.should_clear, "① 关时有拼音候选也不得拦清空");
    }

    /// 反向锁：开关**开**（出厂默认）时两道守护照常拦住，勿把上面两例误改成无条件清空。
    #[test]
    fn clear_still_vetoed_when_guard_enabled() {
        let e = MixedEngine::new(
            ct_engine_clear(&[("aaaa", "工", 100)]),
            Some(Box::new(FakePinyinPrefix { may_continue: true })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: true,
                ..Default::default()
            },
        );
        assert!(
            !e.convert("zhon", 50).unwrap().should_clear,
            "① 开时中途态必须守住（zhon→zhong 不得被吞）"
        );
    }

    #[test]
    fn overflow_never_clears() {
        // 超长（>max_code_len）**有意**不清空：已切入纯拼音语境，「码表满码无候选」前提不成立。
        let r = mixed_with_prefix_pinyin(false)
            .convert("qqqqq", 50)
            .unwrap();
        assert!(!r.should_clear, "overflow 分支不得产生清空");
    }

    // ── 顶码上屏：与满码全码自动上屏**共用同一套**拼音①②否决 ──

    #[test]
    fn topcode_vetoed_by_pinyin_candidate() {
        // ① auto_commit_block_on_pinyin 显式开（默认关）+ 整串有拼音候选 → 抑制顶码。
        let primary = ct_engine_topcode(&[("wang", "王", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "网",
                syllables: 0,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: true,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("wangb"),
            None,
            "① 开 + 有拼音候选应抑制顶码"
        );
    }

    #[test]
    fn topcode_allowed_when_no_pinyin_candidate() {
        // 纯五笔溢出（整串无拼音候选）→ 顶码正常上屏（② 默认开也不拦）。
        // 默认下 ⓪ 亦为开，此例同时守着它的 `has_pinyin` 前提（详见
        // `topcode_allowed_when_overflow_has_no_pinyin`）。
        let primary = ct_engine_topcode(&[("aaaa", "工", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "",
                syllables: 0,
            })),
            None,
            MixConfig::default(),
        );
        assert_eq!(
            e.handle_top_code("aaaab"),
            Some(("工".to_string(), "b".to_string())),
            "无拼音候选时顶码应正常上屏"
        );
    }

    #[test]
    fn topcode_vetoed_by_pinyin_word_when_block_on_pinyin_off() {
        // ① 关、② 开：整串是强拼音词（网吧）→ 仍抑制顶码。
        let primary = ct_engine_topcode(&[("wang", "王", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "网吧",
                syllables: 2,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                ..Default::default()
            },
        );
        assert_eq!(e.handle_top_code("wangba"), None, "② 强拼音词应抑制顶码");
    }

    #[test]
    fn topcode_allowed_when_both_guards_off() {
        // ①② 都关：即便整串像拼音也顶码倒向五笔（王 + 余码 ba）。
        // ⓪ 须显式关以隔离变量——`MixConfig::default()` 的 pinyin_only_overflow 为 true，
        // 而本串有拼音候选，不关掉的话拦截来自 ⓪ 而非被测的 ①②。
        let primary = ct_engine_topcode(&[("wang", "王", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "网吧",
                syllables: 2,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                block_commit_on_pinyin_word: false,
                pinyin_only_overflow: false,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("wangba"),
            Some(("王".to_string(), "ba".to_string())),
            "①② 都关时顶码倒向五笔"
        );
    }

    #[test]
    fn topcode_override_ignores_pinyin_veto() {
        // top_code_override_pinyin 开 = 顶码优先，无视拼音①②否决，强制倒向五笔。
        let primary = ct_engine_topcode(&[("wang", "王", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "网吧",
                syllables: 2,
            })),
            None,
            MixConfig {
                top_code_override_pinyin: true,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("wangba"),
            Some(("王".to_string(), "ba".to_string())),
            "顶码优先应无视拼音否决"
        );
    }

    #[test]
    fn topcode_vetoed_by_single_syllable_prefix_when_block_on_pinyin_off() {
        // ① 关、② 开：前缀 "wang" 是单个完整拼音音节（中途打拼音词 wangba）→ 抑制顶码，
        // 即便 "wangb" 尚未构成完整拼音词（用户实测：① 关时 wangb 仍顶 佢 的 bug）。
        let primary = ct_engine_topcode(&[("wang", "王", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "网",
                syllables: 1,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("wangb"),
            None,
            "① 关 + ② 开：单音节前缀（中途打拼音词）应抑制顶码"
        );
    }

    #[test]
    fn topcode_allowed_for_multi_syllable_prefix_when_block_on_pinyin_off() {
        // ① 关、② 开：前缀 "aipu"=ai+pu 是完整多音节单元、无强词 → 放行顶码倒向五笔（落实）。
        // ⓪ 须显式关以隔离变量（理由同 `topcode_allowed_when_both_guards_off`）。
        let primary = ct_engine_topcode(&[("aipu", "落实", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "矮",
                syllables: 2,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                pinyin_only_overflow: false,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("aipux"),
            Some(("落实".to_string(), "x".to_string())),
            "① 关 + ② 开：多音节前缀无强词应放行顶码"
        );
    }

    // ── ⓪ pinyin_only_overflow：超码长归拼音管，顶码不得抢 ──

    #[test]
    fn topcode_vetoed_by_pinyin_only_overflow() {
        // 与 `topcode_allowed_when_both_guards_off` 构成**单一变量对照**：同样的码表/假拼音/
        // 输入串、同样 ①② 都关，唯一差别是 ⓪ 开 → 拦截只可能来自 ⓪。
        let primary = ct_engine_topcode(&[("wang", "王", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "网吧",
                syllables: 2,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                block_commit_on_pinyin_word: false,
                pinyin_only_overflow: true,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("wangba"),
            None,
            "⓪ 开 + 有拼音候选：超码长归拼音管，顶码不得抢"
        );
    }

    #[test]
    fn topcode_pinyin_only_overflow_protects_youyoud() {
        // 真机回归（用户实测）：混输下打 `youyoud`（悠悠的），第 5 键 `o` 使缓冲 "youyo" 超 4 码
        // → 旧实现顶出 `youy` 的首选「变凉」+ 余码 `oud`。
        //
        // 本例精确复刻当时 ①② 双双落空的判据状态，故只有 ⓪ 能救：
        // - ① 关（用户层 auto_commit_block_on_pinyin=false 覆盖了系统层的 true）；
        // - ②(b) 落空：前 4 码 "youy" = you + 残尾 y，不是完整音节（syllables=2 使
        //   completed_syllable_count != 1）；
        // - ②(a) 落空：整串 "youyo" 拼不出「≥2 汉字」的强词（word 只 1 字）。
        let primary = ct_engine_topcode(&[("youy", "变凉", 864)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "悠",
                syllables: 2,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                pinyin_only_overflow: true,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("youyo"),
            None,
            "①② 落空时 ⓪ 应兜住：超码长的拼音串不得被五笔顶码截胡"
        );
    }

    #[test]
    fn topcode_allowed_when_overflow_has_no_pinyin() {
        // ⓪ 开但整串**无**拼音候选（纯五笔溢出）→ 必须放行顶码。
        // 一刀切禁顶会让用户卡死：convert_overflow 此时只查拼音，同样交不出候选，
        // 那串既不上屏也没候选，没有出口。
        let primary = ct_engine_topcode(&[("aaaa", "工", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "",
                syllables: 0,
            })),
            None,
            MixConfig {
                pinyin_only_overflow: true,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("aaaab"),
            Some(("工".to_string(), "b".to_string())),
            "⓪ 开但无拼音候选：纯五笔溢出应正常顶码"
        );
    }

    #[test]
    fn topcode_override_beats_pinyin_only_overflow() {
        // top_code_override_pinyin 是总开关，压过 ⓪ 与 ①②。
        let primary = ct_engine_topcode(&[("wang", "王", 100)]);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "网吧",
                syllables: 2,
            })),
            None,
            MixConfig {
                pinyin_only_overflow: true,
                top_code_override_pinyin: true,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("wangba"),
            Some(("王".to_string(), "ba".to_string())),
            "顶码优先应无视 ⓪"
        );
    }

    // ── ③ auto_commit_block_on_english：有英文候选时顶码不得抢 ──

    /// 真机场景：`gith` 在五笔主码表是「不算」，英文词库有 GitHub。打 github 到第 5 键 `u`
    /// 时缓冲 `githu` 超 4 码，旧实现顶出「不算」+ 余码 `u`。
    /// **secondary=None 是刻意的**：一并锁住「③ 必须在 `Some(sec)` 块之外」——英文守护与
    /// 拼音子引擎无关，纯码表 + 英文的混输同样该生效。
    #[test]
    fn topcode_vetoed_by_english_candidate() {
        let primary = ct_engine_topcode(&[("gith", "不算", 1822)]);
        let english = english_engine(&[("github", "GitHub", 100)]);
        let e = MixedEngine::new(
            primary,
            None,
            Some(english),
            MixConfig {
                auto_commit_block_on_english: true,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("githu"),
            None,
            "③ 开 + 有英文候选：顶码不得抢（且无拼音子引擎时也须生效）"
        );
    }

    #[test]
    fn topcode_allowed_when_no_english_candidate() {
        // ③ 开但整串无英文候选 → 顶码正常（判据要求英文确有候选，不是开关一开就禁）。
        let primary = ct_engine_topcode(&[("gith", "不算", 1822)]);
        let english = english_engine(&[("hello", "hello", 50)]);
        let e = MixedEngine::new(
            primary,
            None,
            Some(english),
            MixConfig {
                auto_commit_block_on_english: true,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("githu"),
            Some(("不算".to_string(), "u".to_string())),
            "③ 开但无英文候选：顶码应正常"
        );
    }

    #[test]
    fn topcode_english_guard_off_allows_topcode() {
        // ③ 关（出厂默认）→ 即便有英文候选也顶码，保持零回归。
        let primary = ct_engine_topcode(&[("gith", "不算", 1822)]);
        let english = english_engine(&[("github", "GitHub", 100)]);
        let e = MixedEngine::new(
            primary,
            None,
            Some(english),
            MixConfig {
                auto_commit_block_on_english: false,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("githu"),
            Some(("不算".to_string(), "u".to_string())),
            "③ 关时应保持旧行为"
        );
    }

    #[test]
    fn topcode_override_beats_english_guard() {
        // top_code_override_pinyin 是顶码总开关，压过 ③（名字只提 pinyin 属历史局限）。
        let primary = ct_engine_topcode(&[("gith", "不算", 1822)]);
        let english = english_engine(&[("github", "GitHub", 100)]);
        let e = MixedEngine::new(
            primary,
            None,
            Some(english),
            MixConfig {
                auto_commit_block_on_english: true,
                top_code_override_pinyin: true,
                ..Default::default()
            },
        );
        assert_eq!(
            e.handle_top_code("githu"),
            Some(("不算".to_string(), "u".to_string())),
            "顶码优先应无视 ③"
        );
    }

    #[test]
    fn mixed_recheck_auto_commit_after_filter() {
        // 引擎按未过滤候选（含生僻同码字）判不唯一而否决满码上屏；智能过滤后只剩唯一精确
        // 全码码表候选 → 复评据显示候选放行（bug: 显示只剩一个却不上屏）。
        let primary = ct_engine(&[("hhnu", "X", 100), ("hhnu", "愳", 1)], true);
        let e = MixedEngine::new(primary, None, None, MixConfig::default());
        // 原始转换：两个精确 hhnu → 不唯一，引擎不给上屏意向。
        let r = e.convert("hhnu", 50).unwrap();
        assert!(!r.should_commit, "两个精确同码候选时引擎不自动上屏");
        // 模拟智能过滤后仅剩一个码表精确全码候选 → 复评放行。
        let filtered = vec![Candidate {
            text: "X".into(),
            code: "hhnu".into(),
            source: CandidateSource::CodeTable,
            ..Default::default()
        }];
        assert_eq!(
            e.recheck_auto_commit("hhnu", &filtered),
            Some("X".to_string()),
            "过滤后唯一精确全码应复评放行"
        );
        // 拼音/英文来源不参与满码自动上屏：即便过滤后剩一个拼音候选也不放行。
        let py_only = vec![Candidate {
            text: "往".into(),
            code: "hhnu".into(),
            source: CandidateSource::Pinyin,
            ..Default::default()
        }];
        assert_eq!(e.recheck_auto_commit("hhnu", &py_only), None);
    }

    #[test]
    fn mixed_blocks_auto_commit_when_pinyin_word() {
        // 主码表 mama 唯一全码本会自动上屏；① 关但整串是强拼音词 妈妈（②）→ 否决满码上屏。
        let primary = ct_engine(&[("mama", "X", 100)], true);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "妈妈",
                syllables: 2,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                ..Default::default()
            },
        );
        let r = e.convert("mama", 50).unwrap();
        assert!(!r.should_commit, "整串是强拼音词时应否决满码上屏");
    }

    #[test]
    fn mixed_allows_auto_commit_when_pinyin_word_guard_off() {
        // ①② 都关 → 即便整串是强拼音词也放行满码上屏（零回归）。
        let primary = ct_engine(&[("mama", "X", 100)], true);
        let e = MixedEngine::new(
            primary,
            Some(Box::new(FakePinyin {
                word: "妈妈",
                syllables: 2,
            })),
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                block_commit_on_pinyin_word: false,
                ..Default::default()
            },
        );
        let r = e.convert("mama", 50).unwrap();
        assert!(r.should_commit, "①② 都关时应放行满码上屏");
        assert_eq!(r.commit_text, "X");
    }

    #[test]
    fn source_hint_marks_pinyin_candidates() {
        let mut cands = vec![
            Candidate {
                text: "工".into(),
                source: CandidateSource::CodeTable,
                ..Default::default()
            },
            Candidate {
                text: "你好".into(),
                source: CandidateSource::Pinyin,
                ..Default::default()
            },
            Candidate {
                text: "拟".into(),
                source: CandidateSource::Pinyin,
                comment: "ni".into(),
                ..Default::default()
            },
        ];
        MixedEngine::add_source_hints(&mut cands);
        assert_eq!(cands[0].comment, "", "码表候选不标记");
        assert_eq!(cands[1].comment, "拼");
        assert_eq!(cands[2].comment, "拼|ni", "已有 comment 时前置拼接");
    }

    /// 内存英文引擎（EnglishEngine 包码表；code=小写英文词，前缀匹配）。
    fn english_engine(entries: &[(&str, &str, i32)]) -> Box<dyn Engine> {
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "en")));
        let ct = CodeTableEngine::new(32, CommitOptions::default(), Arc::new(dm));
        Box::new(crate::english::EnglishEngine::new(ct))
    }

    #[test]
    fn mixed_mixes_english_when_enabled() {
        // enable_english（english=Some）：混输主路径应混入英文词库候选（前缀匹配）。
        let primary = ct_engine(&[("hao", "好", 100)], false);
        let english = english_engine(&[("hello", "hello", 50), ("help", "help", 40)]);
        let e = MixedEngine::new(
            primary,
            None,
            Some(english),
            MixConfig {
                auto_commit_block_on_pinyin: false,
                ..Default::default()
            },
        );
        let r = e.convert("hel", 50).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == "hello"),
            "开启英文时混输应含英文候选 hello，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
        assert!(
            r.candidates
                .iter()
                .filter(|c| c.text == "hello" || c.text == "help")
                .all(|c| c.source == CandidateSource::English),
            "英文候选来源应标记 English"
        );
    }

    #[test]
    fn mixed_no_english_when_disabled() {
        // english=None：不混入英文候选（零回归）。
        let primary = ct_engine(&[("hao", "好", 100)], false);
        let e = MixedEngine::new(
            primary,
            None,
            None,
            MixConfig {
                auto_commit_block_on_pinyin: false,
                ..Default::default()
            },
        );
        let r = e.convert("hel", 50).unwrap();
        assert!(
            !r.candidates.iter().any(|c| c.text == "hello"),
            "关闭英文时不应有英文候选"
        );
    }

    #[test]
    fn mixed_english_respects_min_length() {
        // min_english_length=3：2 字符以内不查英文，3 字符起才混入。
        let primary = ct_engine(&[("x", "叉", 100)], false);
        let english = english_engine(&[("hello", "hello", 50)]);
        let e = MixedEngine::new(
            primary,
            None,
            Some(english),
            MixConfig {
                auto_commit_block_on_pinyin: false,
                min_english_length: 3,
                ..Default::default()
            },
        );
        let r2 = e.convert("he", 50).unwrap();
        assert!(
            !r2.candidates.iter().any(|c| c.text == "hello"),
            "2 字符（< min 3）不应出英文候选"
        );
        let r3 = e.convert("hel", 50).unwrap();
        assert!(
            r3.candidates.iter().any(|c| c.text == "hello"),
            "3 字符（>= min 3）应出英文候选"
        );
    }

    #[test]
    fn mixed_blocks_auto_commit_when_english_present() {
        // 主码表唯一全码本会自动上屏；开英文守护 + 有英文候选 → 否决（留给用户选英文）。
        let primary = ct_engine(&[("good", "工", 100)], true);
        let english = english_engine(&[("good", "good", 50), ("goodbye", "goodbye", 40)]);
        let e = MixedEngine::new(
            primary,
            None,
            Some(english),
            MixConfig {
                auto_commit_block_on_pinyin: false,
                auto_commit_block_on_english: true,
                ..Default::default()
            },
        );
        let r = e.convert("good", 50).unwrap();
        assert!(!r.should_commit, "开英文守护且有英文候选时应否决全码上屏");
        assert!(
            r.candidates.iter().any(|c| c.text == "good"),
            "应含英文候选 good"
        );
    }

    #[test]
    fn mixed_allows_auto_commit_when_english_guard_off() {
        // 英文守护关 → 即便有英文候选也放行全码上屏（零回归）。
        let primary = ct_engine(&[("good", "工", 100)], true);
        let english = english_engine(&[("good", "good", 50)]);
        let e = MixedEngine::new(
            primary,
            None,
            Some(english),
            MixConfig {
                auto_commit_block_on_pinyin: false,
                ..Default::default()
            },
        );
        let r = e.convert("good", 50).unwrap();
        assert!(r.should_commit, "英文守护关时应放行全码上屏");
        assert_eq!(r.commit_text, "工");
    }
}
