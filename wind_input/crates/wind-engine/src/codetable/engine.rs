//! 码表引擎实现
//!
//! 与 Go 版本 `wind_input/internal/engine/codetable/` 对齐。
//!
//! 查询经 `DictManager`（CompositeDict）——系统词库 + （后续）用户/临时词层统一合并。
//! 候选生成：精确匹配 + 前缀匹配。运行时词频/shadow 不在此（见 frequency.md / dict.md）。

use crate::engine::{ConvertResult, Engine, EngineType, ExtendedEngine};
// 组合区里两段编码之间的分隔符，与码表整句、拼音音节分隔**共用**同一个字符：
// 用户不必为逆切分再学一套符号。
use super::sentence::SPLIT_SEPARATOR;
use std::collections::HashMap;
use std::sync::Arc;
use wind_candidate::{Candidate, CandidateSource, better, by_natural, cmp_exact_first};
use wind_dict::DictManager;

/// 基础排序（`[engine.codetable].base_sort`）：候选**主排序维度**。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BaseSort {
    /// 按词库权重降序（默认；等权回退 natural_order）。行为 = `candidate::better`。
    #[default]
    Weight,
    /// 纯按 natural_order（词库出现序，含 base_order 层偏移）升序，**忽略权重**。
    /// 行为 = `candidate::by_natural`。用于"设计者按文件顺序排、不用权重"的词库。
    Natural,
}

impl BaseSort {
    /// 解析配置字符串：`"natural"` → Natural，`""`/`"weight"` → Weight。
    ///
    /// 其余取值同样回退 Weight，但**会告警**：此前静默吞掉拼写错误，配置者只会观察到
    /// 「改了没生效」而拿不到任何线索。注意本项**不接受 librime 的 `by_weight`/`original`
    /// 拼法**——那是 `.dict.yaml` 里 rime 的库内同码排序键，与本项（方案级全局排序维度）
    /// 语义不同，故列为非法值而非别名，避免两套词汇被误当等价。
    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("natural") {
            Self::Natural
        } else {
            if !s.is_empty() && !s.eq_ignore_ascii_case("weight") {
                tracing::warn!(
                    value = %s,
                    "[engine.codetable].base_sort 取值无法识别，已回退 \"weight\"；合法值仅 \"weight\" / \"natural\""
                );
            }
            Self::Weight
        }
    }

    /// 该模式对应的候选比较器。
    fn cmp(self) -> fn(&Candidate, &Candidate) -> std::cmp::Ordering {
        match self {
            Self::Weight => better,
            Self::Natural => by_natural,
        }
    }
}

/// 逆切分**后段**的取数上限。
///
/// 8 的口径同 `single_code_complete` 的补全池：够协调器过滤（检索范围 / 单字 / shadow）
/// 之后仍有得选，又不至于让一个二简的重码把候选窗整屏占满。前段的取数是可配的
/// （`CommitOptions::split_front_candidates`），后段不可配——后段重码有几个是词库的事实，
/// 砍掉它就是砍掉用户本来打得出的字。
const SPLIT_BACK_LIMIT: usize = 8;

/// 逆切分**前段**取数的上界，钳制 `split_front_candidates`。
///
/// 与后段同量级，取数口径对称。没有它的话，方案作者写个
/// `split_front_candidates = 50` 就会在**按键热路径**上构造 50×8 = 400 条候选
/// （每条两次 String 分配），而紧接着的 `truncate(max_candidates)` 又把绝大多数丢掉。
/// 这个旋钮在文档里被定位成「给方案作者实测用的」，更容易被填大。
const SPLIT_FRONT_LIMIT: usize = 8;

/// 逆切分每段向词典取数的**池子**大小：候选调整（删除 / 置顶）在这个池子上做完再截到
/// 前段 / 后段各自的条数，见 [`CodeTableEngine::split_segment`]。
const SPLIT_SEGMENT_POOL: usize = 16;

/// 逆切分的**前段码长**：恒 2，即一个**二简**。
///
/// # 为什么是常量而不是「码长的一半」
///
/// 初版取 `max_code_length / 2`，四码方案下恰好也是 2，看着等价 —— 直到需求方补充了
/// 三码那一格（论坛 t11，2026-09-19）：`sma` 要切成 `sm`(什么) + `a`(啊)，是 **2+1**
/// 而不是均分。⇒ 这个功能的本质从来不是「对半切」，而是**「切出一个二简，剩下的归后段」**：
/// 二简就是 2 码，与方案的满码长无关。
///
/// 不做成配置项的理由不变：多一个自由参数只会制造「配出来不报错、打起来全是错」的状态。
const SPLIT_FRONT_LEN: usize = 2;

/// 通配结果**硬上限**（spec §3.1 / §11：常量、不开放配置，与翻页扩容上限是同一个值）。
/// 平常由协调器给的 `max_candidates` 决定条数（首批 100，翻页 ×2）；首位即通配退化成
/// 全表扫描时靠它兜底。
pub const WILDCARD_RESULT_LIMIT: usize = crate::engine::CANDIDATE_LIMIT_CAP;

/// 逆切分的触发档（`[engine.codetable].split_trigger`）：满码长时「空到什么程度」才切分。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SplitTrigger {
    /// 整串**一条候选都没有**才切（默认）。只填「现在什么都没有」的那一格，
    /// 不抢任何现有候选，回归面接近零。
    #[default]
    Empty,
    /// 整串**没有精确解**（无 `code == input` 的候选）即切，即便有前缀/补全候选。
    /// 覆盖面更大，但会改变既有候选序 ⇒ 此档产出的切分候选一律**沉底**
    /// （`candidate_display_order` 的 `is_split_composed` 层）。
    ///
    /// **本档不会自动上屏**，但挡住它的**不是**「恰一个精确匹配」那条判据——切分候选的
    /// `code` 就是整串，后段唯一时它恰好**是**唯一一条 `code == input`，那条判据是成立的。
    /// 真正挡住它的是 `decide_auto_commit` 的最后一道 `has_longer`：本档能放行就意味着
    /// 有前缀候选，也就必然存在更长后继。
    /// ⚠️ 谁放宽了 `has_longer`，本档就会静默开始自动上屏——`split_no_exact_never_auto_commits`
    /// 锁着这一点。
    NoExact,
}

impl SplitTrigger {
    /// 解析配置字符串：`"no_exact"` → NoExact，`""`/`"empty"` → Empty。
    ///
    /// 其余取值回退 Empty 并**告警**，理由同 [`BaseSort::parse`]：静默吞掉拼写错误时，
    /// 配置者只会观察到「改了没生效」而拿不到任何线索。
    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("no_exact") {
            Self::NoExact
        } else {
            if !s.is_empty() && !s.eq_ignore_ascii_case("empty") {
                tracing::warn!(
                    value = %s,
                    "[engine.codetable].split_trigger 取值无法识别，已回退 \"empty\"；合法值仅 \"empty\" / \"no_exact\""
                );
            }
            Self::Empty
        }
    }

    /// 本档下 `candidates` 是否「空到可以切分」。
    fn allows(self, candidates: &[Candidate]) -> bool {
        match self {
            Self::Empty => candidates.is_empty(),
            Self::NoExact => !candidates.iter().any(|c| c.is_exact_code),
        }
    }
}

/// 逆切分**次选**的显示形态（`[engine.codetable].split_alt_display`）。
///
/// 需求方（论坛 t11 / t231）对组合候选的理解是「组合态」：首选是**组合区**（前段 + 后段首选），
/// 次选是**等待组合**的后段重码——前段已经认定、不再参与选择，就像已经上屏了一样。
/// 于是 `hfkn` 应显示「① 很可能 ② 困难」，选 ② 上屏「很困难」。
///
/// 只动**显示**、不动上屏：候选的 `text` 恒为完整组合（词频、候选调整、上屏都按它走），
/// 后段文本放进 `Candidate::display_text`，由协调器在显示出口取用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SplitAltDisplay {
    /// 次选只显示后段（默认，需求方的形态）。
    #[default]
    Back,
    /// 次选也显示完整组合（「显示什么就上屏什么」，0.122 的形态）。
    Full,
}

impl SplitAltDisplay {
    /// 解析配置字符串：`""`/`"back"` → Back，`"full"` → Full；其余回退 Back 并告警
    /// （同 [`SplitTrigger::parse`]）。
    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("full") {
            Self::Full
        } else {
            if !s.is_empty() && !s.eq_ignore_ascii_case("back") {
                tracing::warn!(
                    value = %s,
                    "[engine.codetable].split_alt_display 取值无法识别，已回退 \"back\"；合法值仅 \"back\" / \"full\""
                );
            }
            Self::Back
        }
    }
}

/// 码表上屏策略配置（schema 的 [engine.codetable] 相关开关）。
#[derive(Clone, Copy, Debug, Default)]
pub struct CommitOptions {
    /// 全码自动上屏（含 legacy auto_commit_unique 回退，调用方解析）
    pub auto_commit_at_full: bool,
    /// 自动上屏最短码长（0 跟随 max_code_length）
    pub auto_commit_min_len: usize,
    /// 满码无候选时清空缓冲
    pub clear_on_empty_max: bool,
    /// 超过满码长时取前 N 码顶字上屏
    pub top_code_commit: bool,
    /// 显示编码提示：码表方案下,给前缀候选标注「剩余编码」(候选全码去掉已输入前缀)。
    pub show_code_hint: bool,
    /// 精确匹配模式（关闭前缀匹配，对齐 Go SingleCodeInput）。
    pub single_code_input: bool,
    /// 精确匹配空码补全：精确无候选且有更长编码时，从中取首选（对齐 Go SingleCodeComplete；不限码长）。
    pub single_code_complete: bool,
    /// 基础排序维度（weight 降序 / natural 出现序）。见 [`BaseSort`]。
    pub base_sort: BaseSort,
    /// 整句输入：超码长的串自动切分成多个编码单元并组句。
    /// 见 `docs/design/codetable-sentence-input.md` 与 [`super::sentence`]。
    ///
    /// **方案级引擎固定参数**（同 `max_code_length` / `base_sort`），不是可回落全局的
    /// 行为 tri-state：一张码表能不能整句取决于它的编码结构（定长？简码体系多深？），
    /// 是方案属性而非用户偏好。出厂关闭。
    pub sentence_input: bool,
    /// 整句 N-best：候选里**露**几条（`schema.codetable.sentence_count`）。0 按 1 处理。
    pub sentence_count: u8,
    /// 整句 N-best：解码时**算**几条（`schema.codetable.sentence_max_count`）。不足露的条数时按后者抬。
    pub sentence_max_count: u8,
    /// 逆切分（切分模式）：**恰好**满码长的串若空码，切成 2+2 两段各查词典再拼接。
    ///
    /// 语义、为什么是方案属性、与 `sentence_input` 的关系，**权威表述在**
    /// [`wind_config::schema::CodeTableSpec::split_input`]（配置面的入口）。
    /// 设计与判据见 `docs/design/codetable-split-input.md`。
    pub split_input: bool,
    /// 逆切分的**前段**取几条候选。0/缺省 = 1，上界 [`SPLIT_FRONT_LIMIT`]。
    /// 取舍理由见 [`wind_config::schema::CodeTableSpec::split_front_candidates`]。
    pub split_front_candidates: usize,
    /// 逆切分的触发档。见 [`SplitTrigger`]。
    pub split_trigger: SplitTrigger,
    /// 逆切分次选的显示形态。见 [`SplitAltDisplay`]。
    pub split_alt_display: SplitAltDisplay,
    /// 通配键（`[engine.codetable].wildcard` + `.wildcard_key` 折叠后；关闭或键非法为 `None`）。
    /// 引擎只拿它回答 [`Engine::wildcard_key`]；哪几位作通配由协调器定，见 `convert_wildcard`。
    pub wildcard: Option<char>,
    /// 通配结果只留单字（字素簇）。行内通配与反查模式共用 `wildcard_query`，二者同受约束。
    /// 见 reverse-mode spec §2。
    pub wildcard_single_only: bool,
    /// 反查模式内的通配键（`reverse_wildcard_char`，恒有值；不看 `wildcard` 主开关）。
    /// 只有码表方案的 `build_engine` 注入；`None` 表示该引擎不支持反查。见 reverse-mode spec §3.2。
    pub reverse_key: Option<char>,
}

/// 整句解码的产出，见 `CodeTableEngine::decode_sentence`。
#[derive(Default)]
struct SentenceOutput {
    /// 露在候选里的整句，按名次（≤ `sentence_count` 条）。
    shown: Vec<Candidate>,
    /// 切换键的池子（≤ `sentence_max_count` 条）。只在算的条数 > 1 时非空，同拼音。
    pool: Vec<Candidate>,
    /// 名次 1 的编码单元切分串（`ConvertResult::preedit_codetable`）。无整句时为空。
    split: String,
}

/// 码表引擎
pub struct CodeTableEngine {
    max_code_length: usize,
    opts: CommitOptions,
    dm: Arc<DictManager>,
    /// 码元字符集（`[engine.codetable].input_chars` / `.leading_chars`）。
    ///
    /// 引擎自身**不消费**它——本引擎对码元字符零假设（`convert` 是纯字符串键、
    /// 码长一律 `chars().count()`）。放在这里是因为它与 `max_code_length` 同性质：
    /// 方案级引擎固定参数，由协调器经 `EngineManager::active_input_chars()` 按方案取用。
    /// 挂在引擎上，方案切换时自然跟着换，不会像全局快照那样读到别的方案的集合。
    charset: wind_config::CodeCharSet,
    /// 本方案**声明过**的扩展词库 id（含未启用、因而没被加载的那些）。
    ///
    /// 只为 [`Engine::set_dict_enabled`] 分辨「这个 id 是不是我的」而存在。混输引擎会把
    /// 同一次调用转发给 primary / secondary / english 三个子引擎，码表子引擎照样会收到
    /// 拼音库的 id —— 没有这张表就只能靠「摘层是否命中」来猜，而「我的库但惰性加载没装」
    /// 与「压根不是我的库」摘层同样都命中不了，两者必须分开：前者是「已达成」，
    /// 后者是「不认识，该由别人处理或重建」。
    own_extra_dicts: std::collections::HashSet<String>,
    /// 整句解码器（`opts.sentence_input` 关闭时为 `None`）。
    ///
    /// 它内部两张表都是 `OnceLock` 懒构建的全表扫描——关闭的方案连这个结构都不建；
    /// 开启的方案由 [`Self::prewarm_sentence`] 在后台线程提前填好，不占按键线程。
    sentence: Option<super::sentence::CodeSentenceDecoder>,
    /// 逆切分段候选要吃的**候选调整**（shadow）：`(store, 数据归属方案 id)`。
    ///
    /// 候选调整平时在协调器那层应用（`apply_shadow_in`），而逆切分的两段是本引擎**自己**
    /// 查的——协调器只看得见拼好的整串，用户在 `n` 上删掉的「内容」照样被拼进 `kpn`
    /// → 「恐怕内容」（论坛 t231）。故段候选在这里按 `(方案, 段码)` 补吃一次。
    ///
    /// `None`（无 store 的测试 / CLI）⇒ 段候选不做调整，退回纯词库序。
    segment_shadow: Option<(Arc<wind_store::Store>, String)>,
    /// 影子层：本方案未启用的扩展词库（reverse-mode spec §4.2）。
    ///
    /// ★ **只在 [`Self::wildcard_query`] 且 `include_disabled` 时读**——即行内通配
    /// （`convert_wildcard`）与反查模式（`convert_reverse`）读它；普通 `convert`（打字候选、
    /// 活码探针、顶码、自动上屏复评）不读。读了就是把未启用库漏进打字候选，这一条靠调用方守。
    /// `None` = 开关（`input.reverse.lookup_disabled_dicts`）关 / 无未启用库 / 测试未注入。
    disabled_dicts: Option<super::DisabledDictLayers>,
}

impl CodeTableEngine {
    pub fn new(max_code_length: usize, mut opts: CommitOptions, dm: Arc<DictManager>) -> Self {
        // min_len 为 0 时跟随 max_code_length（对齐 Go codetable.go:135）。
        if opts.auto_commit_min_len == 0 {
            opts.auto_commit_min_len = max_code_length;
        }
        let sentence = opts
            .sentence_input
            .then(|| super::sentence::CodeSentenceDecoder::new(max_code_length));
        // ★ 整句开启时满码上屏与满码空码清空**就地让位**（同顶码，判据看功能开没开）：
        // 两者都在第 4 码截断正在组的句子——唯一全码当场上屏、空码当场清空，而那 4 码
        // 很可能只是句子的前几个编码单元。在这里关掉，`convert` / `should_auto_commit` /
        // `recheck_auto_commit` 与清空资格（协调器只在引擎给了 `should_clear` 时才裁决）
        // 一处覆盖。见设计文档 §7.2。
        if opts.sentence_input && (opts.auto_commit_at_full || opts.clear_on_empty_max) {
            tracing::info!(
                auto_commit_at_full = opts.auto_commit_at_full,
                clear_on_empty_max = opts.clear_on_empty_max,
                "[engine.codetable] sentence_input 开启：满码上屏与满码空码清空不生效"
            );
            opts.auto_commit_at_full = false;
            opts.clear_on_empty_max = false;
        }
        // 前段恒 2 码（`SPLIT_FRONT_LEN`），后段至少 1 码 ⇒ 满码长至少 3 才切得动。
        // **就地关掉并告警**，而不是每次按键静默什么都不做：配置者只会观察到「开了没反应」，
        // 拿不到任何线索。
        //
        // ⚠️ 判据曾是「偶数且 ≥4」（那时切点取码长的一半）。改成恒切二简后奇数码长不再是
        // 障碍：5 码方案切 2+3 照样成立。
        if opts.split_input && max_code_length <= SPLIT_FRONT_LEN {
            tracing::warn!(
                max_code_length,
                "[engine.codetable] split_input 需要 max_code_length > 2（前段恒取一个二简、后段至少 1 码），\
                 本方案码长不满足，逆切分已关闭"
            );
            opts.split_input = false;
        }
        // 两者占的区间本不重叠（`==` vs `>` 码长），可以同开；但整句会让**顶码整体让位**
        // （见 `handle_top_code`），而逆切分的「后码顶首选上屏」正是靠顶码实现的。
        // 功能仍可用，只是少了那一条交互 —— 说一句，别让人以为是 bug。
        if opts.split_input && opts.sentence_input {
            tracing::warn!(
                "[engine.codetable] split_input 与 sentence_input 同时开启：整句会让顶码整体让位，\
                 逆切分候选的「继续打字母顶首选上屏」将不生效（候选窗选词不受影响）"
            );
        }
        Self {
            max_code_length,
            opts,
            dm,
            sentence,
            // 默认 `a-z`，与历史硬编码 `VK_A..=VK_Z` 逐键等价。构建方按方案配置
            // 再 `with_charset` 覆盖——如此所有既有调用点（含测试）无需改动。
            charset: wind_config::CodeCharSet::default_alpha(),
            // 默认空集 ⇒ `set_dict_enabled` 对任何 id 都回 false（「不认识」），
            // 构建方用 `with_own_extra_dicts` 按方案填。既有调用点（含测试）无需改动：
            // 空集只会让热插拔退化成「失效重建」，不会给出错误答案。
            own_extra_dicts: std::collections::HashSet::new(),
            segment_shadow: None,
            disabled_dicts: None,
        }
    }

    /// 注入逆切分段候选的候选调整来源。`schema` 须是**数据归属** id（码表即方案自身 id，
    /// 与协调器 `apply_shadow_in` 取 `data_schema_id` 同口径）。
    pub fn with_segment_shadow(mut self, store: Arc<wind_store::Store>, schema: &str) -> Self {
        self.segment_shadow = Some((store, schema.to_string()));
        self
    }

    /// 注入码元字符集。缺省即内置默认 `a-z`。
    pub fn with_charset(mut self, charset: wind_config::CodeCharSet) -> Self {
        self.charset = charset;
        self
    }

    /// 登记本方案声明过的扩展词库 id（含未启用的）。见 [`Self::own_extra_dicts`]。
    pub fn with_own_extra_dicts<I: IntoIterator<Item = String>>(mut self, ids: I) -> Self {
        self.own_extra_dicts = ids.into_iter().collect();
        self
    }

    /// 注入影子层（未启用扩展词库，只给反查模式查询用）。见 [`Self::disabled_dicts`] 字段。
    pub fn with_disabled_dicts(mut self, d: super::DisabledDictLayers) -> Self {
        self.disabled_dicts = Some(d);
        self
    }

    /// 影子层（未注入为 `None`）。
    pub fn disabled_dicts(&self) -> Option<&super::DisabledDictLayers> {
        self.disabled_dicts.as_ref()
    }

    /// 指明**整句词频**的来源目录（见 `sentence::SentenceFreq`）。词库到首次整句解码时才读。
    ///
    /// 整句未开启时是 no-op —— 没有解码器可交代。
    pub fn with_sentence_schemas_dir(mut self, dir: std::path::PathBuf) -> Self {
        if let Some(d) = self.sentence.take() {
            self.sentence = Some(d.with_schemas_dir(dir));
        }
        self
    }

    /// 整句挂语法模型（`schema.codetable.sentence_grammar_weight`）。`weight <= 0` 等于不挂。
    /// 模型文件在预热线程里打开；整句未开启时是 no-op。
    pub fn with_sentence_grammar(mut self, path: std::path::PathBuf, weight: f64) -> Self {
        if let Some(d) = self.sentence.take() {
            self.sentence = Some(d.with_grammar_file(path, weight));
        }
        self
    }

    /// 整句 `(露几条, 算几条)`，同拼音的 `Config::sentence_counts`：都夹到 `1..=上限`，
    /// 算的条数不少于露的条数。
    fn sentence_counts(&self) -> (usize, usize) {
        let limit = crate::pinyin::SENTENCE_COUNT_LIMIT;
        let show = self.opts.sentence_count.clamp(1, limit) as usize;
        let pool = (self.opts.sentence_max_count.clamp(1, limit) as usize).max(show);
        (show, pool)
    }

    /// 注入**已加载的**拼音词库作为整句词频来源。测试与探针走这个。
    pub fn with_sentence_pinyin_dict(
        mut self,
        dict: std::sync::Arc<wind_dict::cached::CachedDict>,
    ) -> Self {
        if let Some(d) = self.sentence.take() {
            self.sentence = Some(d.with_pinyin_dict(dict));
        }
        self
    }

    /// 本引擎的词典管理器。
    ///
    /// 供薄封装（[`EnglishEngine`](crate::english::EnglishEngine)）建它自己的索引——
    /// 英文词组分词索引与码表无关，但它的数据源和本引擎是同一份词库，没必要再加载一遍。
    pub fn dict_manager(&self) -> &Arc<DictManager> {
        &self.dm
    }

    /// 后台预热整句的懒表（简码索引、拼音词频与组词表、语法模型）。整句未开启时是 no-op。
    ///
    /// 由构建方在引擎组装完毕、**所有 `with_*` 都已调用之后**调用一次——预热线程读的是
    /// 那时的 `freq_source`，早于 `with_sentence_schemas_dir` 就白跑一趟没词频的预热。
    /// 开销数字与「为什么必须搬到后台」见 `sentence::LazyTables`。
    pub fn prewarm_sentence(&self) {
        if let Some(d) = &self.sentence {
            d.prewarm(Arc::clone(&self.dm));
        }
    }

    /// 整句解码：产出覆盖整串的整句候选（N-best，按 `sentence_count` / `sentence_max_count`）。
    ///
    /// # 三道门槛
    ///
    /// 1. **功能开启**（`opts.sentence_input`）；
    /// 2. **超码长**——码长内的串本就是一个编码单元，切它没有意义，且真机上正是那个
    ///    区间最容易出事：`aaw`（本意 `aawt`→「工作」）会被读成「工工人」之类。
    ///    这条门槛与混输侧 `in_code_len_opts()` 关掉拼音残码整句的判据是同一条
    ///    （「这串还可能是码表码吗」）；
    /// 3. **整串无精确解**——对齐 librime `table_translator` 的
    ///    `if (enable_sentence_ && !translation)`：整串在码表里查得到词就不进整句路径，
    ///    否则同一个词会以两种身份进列表再被去重逻辑合并。
    ///
    /// # 返回
    ///
    /// [`SentenceOutput`]：露在候选里的整句（按名次）、切换键用的池子、名次 1 的切分串。
    fn decode_sentence(&self, input: &str, candidates: &[Candidate]) -> SentenceOutput {
        let mut out = SentenceOutput::default();
        let Some(decoder) = self.sentence.as_ref() else {
            return out;
        };
        if input.chars().count() <= self.max_code_length {
            return out;
        }
        if candidates.iter().any(|c| c.is_exact_code) {
            return out;
        }
        let (show_n, pool_k) = self.sentence_counts();
        let nbest = show_n > 1 || pool_k > 1;
        for (i, r) in decoder
            .decode_nbest(input, &self.dm, pool_k)
            .into_iter()
            .enumerate()
        {
            let rank = i + 1;
            let split = r.split_code(input);
            if rank == 1 {
                out.split = split.clone();
            }
            let cand = Candidate {
                text: r.text,
                code: input.to_string(),
                // 名次靠后的权重略低：引擎内部排序与名次同向（协调器另按名次摆整句块）。
                weight: super::sentence::SENTENCE_WEIGHT_BASE - i as i32,
                source: CandidateSource::CodeTable,
                is_sentence: true,
                sentence_rank: rank as u8,
                // 词库里没有以它为整体的词条 —— 自动造词据此判「值不值得学」
                // （见 `Candidate::is_synthesized` 文档：不能用 `is_sentence` 代替）。
                is_synthesized: true,
                // ⚠️ `consumed_length` 留 0（= 消费整串）：整句只在覆盖整串时才产出
                // （见 `CodeSentenceDecoder::decode`），故不打破全仓「码表候选
                // consumed_length 恒 0」的约定。分段上屏留到后续阶段。
                consumed_length: 0,
                // ⚠️ `boundary` 也留 0：该字段是**音节**边界，域是拼音；码表码没有音节
                // 语义（`BoundaryResolution::NoInfo` 的既定含义就是「非拼音方案」）。
                // 填编码单元的切分位会让它在入库契约里被当成音节真值 —— 整句若被自动造词
                // 学进用户词库，那份假边界会一路传下去。切分显示走 `meta.preedit_split`。
                boundary: 0,
                meta: wind_candidate::CandidateMeta {
                    // 每条整句各带自己的切法：备选与名次 1 的切分往往不同，高亮到谁就显示谁的。
                    // ⚠️ 只在 N-best 开着时带：带了它协调器就会在高亮移动时重算组合区，出厂 1/1
                    // 下那会让组合区随方向键在切分形态与原始码之间来回变——不是这次要改的行为。
                    preedit_split: (nbest && !split.is_empty()).then(|| split.into()),
                    ..Default::default()
                },
                ..Default::default()
            };
            if pool_k > 1 {
                out.pool.push(cand.clone());
            }
            if rank <= show_n {
                out.shown.push(cand);
            }
        }
        out
    }

    /// 逆切分：恰好满码长的空码串切成两段，各查一次词典后拼接成组合候选。
    ///
    /// # 四道门槛
    ///
    /// 1. **功能开启**（`opts.split_input`）；
    /// 2. **码长落在 `(SPLIT_FRONT_LEN, max_code_length]`**，且**未满码时必须无更长后继**：
    ///    - 超码长那一段归顶码与整句（两者的闸门都在 `>` 上），本功能不碰；
    ///    - 满码长是主场（`hfkn` → `hf` + `kn`）；
    ///    - **未满码**原本整个排除，理由是「还有更长后继可打，切分等于替用户提前认定这串
    ///      到此为止」（整句门槛记着真机反例：`aaw` 本意 `aawt`→「工作」，被读成「啊啊我」）。
    ///      需求方补充的「**且无后续编码**」恰好补上那个缺口：没有更长后继时用户已经打不
    ///      下去了，这串确实就是到此为止 ⇒ `sma` 无后继时切成 `sm`(什么) + `a`(啊)。
    /// 3. **`split_trigger` 定的空度**（[`SplitTrigger::allows`]）；
    /// 4. **两段都有编码** —— 切一半没有意义。字、词、符号一视同仁（段码上编的是符号也切），
    ///    被用户在候选调整里删光的段视同空码，见 [`Self::split_segment`]。
    ///
    /// # 排序
    ///
    /// `weight` 取两段的**较小值**：组合的可信度不高于最弱的那一段。随后再沿构造序压成
    /// **单调不增**（后一条不高于前一条），`natural_order` 填构造序作同权重时的末级键——
    /// 协调器按 `candidate_display_order` 重排（权重先于出现序）后仍得到构造序。
    ///
    /// ⚠️ 单调化不是修饰，构造序承载着两件协调器看不见的事：
    /// - **段内的候选调整**：`split_segment` 按置顶改了段内次序，而段权重还是词库原值。
    ///   不压的话，用户在 `kn` 上置顶的「难」到协调器又被按权重排回第二（审查实测）；
    /// - **同前段相邻**：前段多取（`split_front_candidates` > 1）时，按原始权重会把不同
    ///   前段的行交错，而次选只显示后段——「② 很能 ③ 难」读起来是「很难」，上屏却是「很可难」。
    ///
    /// # 返回
    ///
    /// `(组合候选, 编码单元切分串)`。切分串给组合区显示（`hf'kn`，见
    /// `ConvertResult::preedit_codetable`），无产出时为空串。
    fn decode_split(&self, input: &str, candidates: &[Candidate]) -> (Vec<Candidate>, String) {
        let none = (Vec::new(), String::new());
        if !self.opts.split_input {
            return none;
        }
        let chars: Vec<char> = input.chars().collect();
        let len = chars.len();
        // 后段至少 1 码 ⇒ 总长必须长于前段；超过满码长归顶码与整句。
        if len <= SPLIT_FRONT_LEN || len > self.max_code_length {
            return none;
        }
        if !self.opts.split_trigger.allows(candidates) {
            return none;
        }
        // 问后继放在空度判据**之后**，让绝大多数按键根本走不到这次索引查询。
        //
        // 未满码时恒问。满码长那一格**默认档也要问**（论坛 t302）：前缀匹配下有更长后继
        // 必然带出前缀候选，上面的空度判据早已拦下，这一问只对**精确匹配模式**
        // （`single_code_input`，前缀枚举被关）有效——那里「列表空」不等于「空码」，码表收了
        // 超码长的词条时满码长照样有后续可打（小鹤音形实测），切了就抢掉了「空码提示后续
        // 编码」那一格。`no_exact` 档不问：它的定义就是「有前缀/后续也切」。
        let ask_longer =
            len < self.max_code_length || self.opts.split_trigger == SplitTrigger::Empty;
        if ask_longer && self.has_longer_code(input) {
            return none;
        }
        let split_at = SPLIT_FRONT_LEN;

        let front_code: String = chars[..split_at].iter().collect();
        let back_code: String = chars[split_at..].iter().collect();
        // `DictManager::search` 走 `CompositeDict::merge_search(.., Query::Exact)`，
        // 查询本身已经是精确的，不会混进前缀候选 ⇒ **不需要**再按 `c.code == 段码` 过滤。
        // （该函数的「同 text 取最短码」改写只对 `Query::Prefix` 生效，见 `composite.rs`
        // 里 `let is_prefix = matches!(kind, Query::Prefix)` 那一处；Exact 路径上 code
        // 不会被换掉。此前这里的注释把那条写成了对两种查询都成立，是错的。）
        //
        // 本函数只读段候选的 `text` 与 `weight`，不读 `code`。
        let front = self.split_segment(
            &front_code,
            self.opts.split_front_candidates.clamp(1, SPLIT_FRONT_LIMIT),
        );
        if front.is_empty() {
            return none;
        }
        let back = self.split_segment(&back_code, SPLIT_BACK_LIMIT);
        if back.is_empty() {
            return none;
        }

        let mut out = Vec::with_capacity(front.len() * back.len());
        let alt_back_only = self.opts.split_alt_display == SplitAltDisplay::Back;
        for (fi, f) in front.iter().enumerate() {
            for b in &back {
                out.push(Candidate {
                    text: format!("{}{}", f.text, b.text),
                    // 次选只显示后段（[`SplitAltDisplay::Back`]）：前段取的是**段首选**的那一行，
                    // 前段就是「已认定」的，只剩后段待选。前段非首选的行（`split_front_candidates`
                    // > 1 才有）照样显示整串，否则两行显示同一个后段、分不出前段差在哪。
                    //
                    // 后段首选那一条也填：「谁是首选」要到协调器排完序才知道（词频重排、候选调整
                    // 都可能把次选顶上来），首条组合显示整串由协调器的显示出口判定
                    // （`cand_display_text`）。
                    display_text: if alt_back_only && fi == 0 {
                        b.text.clone()
                    } else {
                        String::new()
                    },
                    // 整串：`decide_auto_commit` 的判据是「恰一个 `code == input` 的候选」，
                    // 后段唯一时它恰好成立 ⇒ 原帖「前后都唯一即自动上屏」零额外判据地实现。
                    code: input.to_string(),
                    weight: f.weight.min(b.weight),
                    natural_order: out.len() as i32,
                    source: CandidateSource::CodeTable,
                    is_split_composed: true,
                    // 「词库里没有以它为整体的词条」这个事实本身为真，故如实标注。
                    //
                    // ⚠️ **当前没有任何消费者**：唯一读它的 `learn_phrase_on_commit` 对
                    // 纯码表方案在入口就 `if self.engine_mgr.is_codetable() { return None }`，
                    // 混输侧另有一道 `first == CodeTable` 的排除；码表自己那条造词路
                    // （`feed_auto_phrase` 的连续单字缓冲）把多字词上屏当作**终止符**、
                    // 本词不入缓冲。⇒ 切分产物**不会**进用户词库，「打过一次下次直接有」
                    // 不成立（词频表 `record_freq` 只记频次、不造词条）。
                    //
                    // 留着它而不是删掉：这是候选的**来源事实**，将来若给码表接上合成词造词，
                    // 判据现成。删了下次要重新推一遍「这条词库里到底有没有」。
                    is_synthesized: true,
                    // ⚠️ **不置 `is_exact_code`**：它不是词库里的精确解。置位会让它混进
                    // `cmp_exact_first` 的精确档，越过真正的精确候选（`no_exact` 档下可见）。
                    is_exact_code: false,
                    // ⚠️ `consumed_length` 留 0（= 消费整串）：组合候选本就是一个整体，
                    // 没有分段上屏的需求，故不打破全仓「码表候选 consumed_length 恒 0」的约定
                    // （同整句，见 `decode_sentence`）。
                    consumed_length: 0,
                    // ⚠️ `boundary` 也留 0：该字段是**音节**边界、域是拼音，码表码没有音节
                    // 语义。填编码单元的切分位会让组合候选被自动造词学进用户词库时带一份
                    // 假的音节真值。切分显示另有出口（`preedit_codetable`）。
                    boundary: 0,
                    ..Default::default()
                });
            }
        }
        for i in 1..out.len() {
            out[i].weight = out[i].weight.min(out[i - 1].weight);
        }
        let split = format!("{front_code}{SPLIT_SEPARATOR}{back_code}");
        (out, split)
    }

    /// 逆切分一段的候选：精确查词典 → 吃候选调整 → 剔除特殊语法 → 截到 `limit` 条。
    ///
    /// # 为什么先多取再截
    ///
    /// 候选调整要在**截断之前**做：前段默认只取 1 条，若先截再删，用户删掉的恰是段首选时
    /// 前段就空了、整个组合消失，而不是像正常输入那样由次选补位；置顶同理——被置顶的候选
    /// 不在截断后的那 1 条里就无从上位。取数池 [`SPLIT_SEGMENT_POOL`] 与协调器按段码看到的
    /// 列表同量级，置顶位次超出池子的（极少见）不生效。
    ///
    /// # 符号照样参与（论坛 t11，需求方 2026-09-20 两次澄清）
    ///
    /// 「有实际编码就切，不论字、词、符」：`xkok` → `xk`(行) + `ok`(👌)。0.122 曾把「只查到
    /// 符号的段」当空码，是误读了需求方 09-19 那条（那条说的是二简位**没有编码**时按空码
    /// 处理，符号不算空码）。
    ///
    /// 唯独剔除**特殊语法**值（`$CC(..)` 命令 / `$SS` 组 / `{date}` 之类求值模板）：它们要到
    /// 协调器的 `finalize_candidates` 才展开，拼进组合就是把源码当文本上屏。判据与那里的
    /// 快路径同一个（含 `$` 或 `{`），代价是字面就是 `$`、`{` 的符号条目也不参与切分。
    fn split_segment(&self, code: &str, limit: usize) -> Vec<Candidate> {
        let mut v = self.dm.search(code, SPLIT_SEGMENT_POOL);
        if let Some((store, schema)) = &self.segment_shadow
            && let Ok(Some(rec)) = store.get_shadow_rules(schema, code)
        {
            let pinned: Vec<wind_candidate::ShadowPinRule> = rec
                .pinned
                .iter()
                .map(|p| wind_candidate::ShadowPinRule {
                    word: p.word.clone(),
                    cand_id: p.cand_id.clone(),
                    position: p.position,
                })
                .collect();
            wind_candidate::apply_shadow(&mut v, &rec.deleted, &pinned);
        }
        // 特殊语法不进逆切分；纯文本直通 `$CC(.., type("字"))` 换成上屏文本（与整句同判据）。
        let mut v: Vec<Candidate> = v
            .into_iter()
            .filter_map(|mut c| {
                let plain = super::sentence::sentence_text(&c.text)?;
                if let Some(t) = plain {
                    c.text = t;
                }
                Some(c)
            })
            .collect();
        v.truncate(limit);
        v
    }

    /// 整句解的**切分分段**（诊断/探针用）。`None` = 未开启整句或解不出。
    ///
    /// 存在的理由：只看整句首选文本看不出错在哪一段，而「切错了」与「同一条边上同码
    /// 选错了」是两类完全不同的问题，修法也完全不同。
    pub fn sentence_segments(&self, input: &str) -> Option<Vec<String>> {
        Some(self.sentence.as_ref()?.decode(input, &self.dm)?.words)
    }

    /// 是否存在比 `input` 更长的后继编码（避免把长码精确匹配的前缀误当全码上屏）。
    ///
    /// 走 `DictManager::has_longer_code` 直接问各层有序索引，而非「`search_prefix(input, 64)`
    /// 再 `.any(code 更长)`」——后者为一个 bool 遍历整棵前缀子树（`ok` 拼字这类单前缀
    /// 8.8 万条的词库上单次 20ms 级），且其判据经权重截断与跨层「同 text 取最短码」两道
    /// 变形，长码候选权重偏低时会漏判成 false，反而让不该自动上屏的情形上了屏。
    fn has_longer_code(&self, input: &str) -> bool {
        self.dm.has_longer_code(input)
    }

    /// `input` 是否存在精确（code==input）匹配。
    fn has_full_input_match(&self, input: &str) -> bool {
        !self.dm.search(input, 1).is_empty()
    }
}

/// 全码自动上屏纯判定（对齐 Go checkAutoCommit）：
/// 开关开 + 码长达 min_len + 恰一个精确匹配（code==input）+ 无更长后继 → 上屏该候选文本。
fn decide_auto_commit(
    at_full: bool,
    min_len: usize,
    input: &str,
    candidates: &[Candidate],
    has_longer: bool,
) -> Option<String> {
    if !at_full || input.chars().count() < min_len {
        return None;
    }
    // ⚠️ **排除检索范围放宽补进来的候选**（`is_scope_filtered`）。今天的满码自动上屏，一部分
    // 正是靠智能过滤滤掉了同码生僻字才成立（见 `recheck_auto_commit_unique_after_filter`：
    // `hhnu` 下常用「X」+生僻「愳」判不唯一不上屏，滤掉「愳」后复评才放行）。放宽把它们补回
    // 列表却不在此排除，会让一批原本满码即上屏的字退化成要多按一次空格——**而且是静默退化**，
    // 用户只觉得「上屏时灵时不灵」。排除后，自动上屏口径在自动补充/手动放宽下均与放宽前一致。
    //
    // ⚠️ **也排除整句候选**（`is_sentence`）：它的 `code` 是整串，超码长时又常是唯一一条
    // `code == input`、且超码长串没有更长后继 ⇒ 不排除的话，开了满码上屏后每多打一码
    // 整句就把自己上屏了（`trntw` → 「我人」），句子永远组不起来（论坛 t350）。
    let mut exact = candidates.iter().filter(|c| {
        c.code == input && !c.is_scope_filtered && !c.is_low_confidence() && !c.is_sentence
    });
    let first = exact.next()?;
    if exact.next().is_some() {
        return None; // 多个精确匹配，不自动上屏
    }
    if has_longer {
        return None;
    }
    Some(first.text.clone())
}

impl CodeTableEngine {
    /// 通配查询内核（行内通配与反查模式共用）。语义见 `convert_wildcard`。
    ///
    /// `include_disabled`：是否并查影子层（未启用扩展库）。`false` ⇒ 连 `search_pattern` 都不调
    /// （不触发影子层加载），结果与没挂影子层时逐条相同。行内通配与反查模式都传 `true`。
    fn wildcard_query(
        &self,
        input: &str,
        pattern: &str,
        max_candidates: usize,
        include_disabled: bool,
    ) -> ConvertResult {
        let n = pattern.chars().count();
        let with_prefix = !self.opts.single_code_input;
        // 按协调器给的上限查（spec §11），硬上限兜底。⚠️ 永不向 `search_pattern` 传 0：
        // Composite 把 0 当「不限」、各层把 0 当「空」，两边语义不一致。
        let limit = max_candidates.min(WILDCARD_RESULT_LIMIT);
        let mut hits: Vec<Candidate> = Vec::new();
        if limit > 0 {
            // ★ 词库层自己按 `fetch` 截断（各层两档取额），过滤只能在它之后。仅单字时不够就 ×2 重取，
            // 直到够数 / 取尽 / 硬上限（同生僻字模式的 refill）。关着时只查一轮 ⇒ 与原实现逐条相同。
            let mut fetch = limit;
            loop {
                let got =
                    self.dm
                        .search_pattern(pattern, wind_dict::WILDCARD_SLOT, fetch, with_prefix);
                // 影子层（未启用扩展库，spec §4.2）：只在这里、且只为反查模式读；
                // 行内通配与普通 convert 不看它。
                let disabled_hits = self
                    .disabled_dicts
                    .as_ref()
                    .filter(|_| include_disabled)
                    .map(|d| {
                        d.search_pattern(pattern, wind_dict::WILDCARD_SLOT, fetch, with_prefix)
                    })
                    .unwrap_or_default();
                // 两边都没取满才算取尽；没有影子层时 `0 < fetch` 恒真 ⇒ 与原判据相同。
                let exhausted = got.len() < fetch && disabled_hits.len() < fetch;
                hits = merge_disabled_hits(got, disabled_hits);
                if !self.opts.wildcard_single_only {
                    break;
                }
                hits.retain(|c| wind_candidate::single_markable_char(&c.text).is_some());
                if hits.len() >= limit || exhausted || fetch >= WILDCARD_RESULT_LIMIT {
                    break;
                }
                fetch = fetch.saturating_mul(2).min(WILDCARD_RESULT_LIMIT);
            }
        }
        let mut candidates: Vec<Candidate> = hits
            .into_iter()
            .map(|mut c| {
                c.source = CandidateSource::CodeTable;
                c.is_exact_code = c.code.chars().count() == n;
                c.comment = c.code.clone();
                // 未启用库的命中在注释前加 `*`，让人看出它不在当前启用的词库里。
                if c.from_disabled_dict {
                    c.comment.insert(0, '*');
                }
                c.is_wildcard = true;
                c
            })
            .collect();
        let base_cmp = self.opts.base_sort.cmp();
        // 未启用库的键放在 `cmp_exact_first` 之后（与 `candidate_display_order` 同位）：
        // 等长 / 更长两档不变，档内先启用后未启用；两条非未启用候选间恒 Equal ⇒ 既有次序不变。
        candidates.sort_by(|a, b| {
            cmp_exact_first(a, b)
                .then(a.from_disabled_dict.cmp(&b.from_disabled_dict))
                .then_with(|| base_cmp(a, b))
        });
        candidates.truncate(limit);
        let is_empty = candidates.is_empty();
        ConvertResult {
            candidates,
            preedit_display: input.to_string(),
            is_empty,
            ..Default::default()
        }
    }
}

/// 通配结果合并影子层命中（spec §4.2）：以 `(text, code)` 为键先收已启用的，再追加键未出现过的
/// 影子层命中并置 `from_disabled_dict`——同键留已启用那条（沿用 Composite 的去重语义）。
///
/// ⚠️ 去重只对两边**本轮取到的**条目有效。开启仅单字且加倍重取一直取到硬上限时，主层可能仍被
/// 截断，某条同时存在于主层被截段与影子层的候选会被当作影子命中，「未启用」标记在这种极端情形下
/// 可能打错（不会产生重复候选，可接受）。
fn merge_disabled_hits(enabled: Vec<Candidate>, disabled: Vec<Candidate>) -> Vec<Candidate> {
    if disabled.is_empty() {
        return enabled;
    }
    let mut seen: std::collections::HashSet<(String, String)> = enabled
        .iter()
        .map(|c| (c.text.clone(), c.code.clone()))
        .collect();
    let mut out = enabled;
    for mut c in disabled {
        if seen.insert((c.text.clone(), c.code.clone())) {
            c.from_disabled_dict = true;
            out.push(c);
        }
    }
    out
}

impl Engine for CodeTableEngine {
    fn memory_parts(&self) -> Vec<crate::engine::MemPart> {
        self.sentence
            .as_ref()
            .map(|d| d.memory_parts())
            .unwrap_or_default()
    }

    /// 热插拔扩展词库。**禁用摘层、启用交给重建**，两边不对称，各有理由：
    ///
    /// **禁用 → 从 composite 摘掉 `codetable-extra-<id>` 层**，而不是翻它的 enabled 标志。
    /// 翻标志只让它不再出候选，`CachedDict` 的 `Arc` 仍挂在层上 —— Windows 下 wdat 被 mmap
    /// 期间删不掉（见 `wind_dict::reader_pool` 的模块注释），用户禁用了词库却依然清不掉
    /// cache 里那个文件（t107）。摘层会 drop 掉该层，`Arc` 计数随之下降；若无其它方案引用
    /// 同一个缓存文件（`reader_pool` 存的是 `Weak`，不会拖住），mmap 即刻解除、文件可删。
    ///
    /// **启用 → 返回 false**。未启用的词库压根没加载（见
    /// `EngineManager::load_codetable_layers` 的惰性加载），层不存在，这里无从恢复；
    /// 调用方 `set_dict_enabled_live` 收到 false 会失效整个方案，下次使用时重建 ——
    /// 那一趟才真正去读文件、必要时建 wdat。代价是启用大词库有一次可感知的重建延迟，
    /// 这是「禁用即不占资源」换来的，取舍见 t107。
    fn set_dict_enabled(&self, dict_id: &str, enabled: bool) -> bool {
        // ★ 先问「这个 id 是不是我的」。混输会把同一次调用转发给三个子引擎，码表子引擎
        // 照样会收到拼音库的 id；不先筛掉，下面那个 `true` 就会冒充「我处理了」，
        // 让 `MixedEngine` 的 `a || b || c` 恒真 —— 真正承载该库的那个子引擎（比如拼音，
        // 它不支持热插拔）明明需要失效重建，却被这个假阳性掩盖掉。
        if !self.own_extra_dicts.contains(dict_id) {
            return false;
        }
        if enabled {
            // 惰性加载下该层压根没建，这里无从恢复 —— 交调用方失效重建。
            return false;
        }
        // 走到这里：**本方案的**扩展库，且要禁用它。返回值语义是「目标态是否已达成」
        // 而非「是否摘到了层」—— 本来就没加载（惰性加载跳过了它）同样算达成。
        // 若这里回 false，`set_dict_enabled_live` 会判成「翻不动」而失效整个方案，
        // 重建时把刚摘掉的其它扩展层一并装回来：实测踩过，wubi86 三个扩展库里 xzqy
        // 未启用，关闭三者后「甘蓝菜」仍在。
        self.dm
            .unregister_layer(&format!("codetable-extra-{dict_id}"));
        // 它从此是「未启用库」：进影子层（反查模式仍可查到）。
        if let Some(d) = &self.disabled_dicts {
            d.mark_disabled(dict_id);
        }
        true
    }

    /// 空码枚举：空前缀查询从根遍历整表（datformat::search_prefix），已按 weight 降序 +
    /// order 升序排好并截断。标 CodeTable 来源供协调器统一处理。
    /// 注：大表会在字典层 materialize 全部条目再截断，仅宜用于小符号表的「进入即浏览」。
    ///
    /// 精确匹配模式的「只展示一条」**不在此施加**——它是呈现策略，经
    /// [`Engine::browse_display_limit`] 声明、由调用方在过滤之后施加。
    fn enumerate(&self, limit: usize) -> Vec<Candidate> {
        // 全量取数，**不在此按 `single_code_input` 截断**——精确匹配模式的「只展示一条」
        // 经 `browse_display_limit` 交给调用方在 shadow 之后施加（见 trait 文档）。
        // 代价为零：`search_prefix` 无 early-stop，n=1 与 n=limit 同样是全表 materialize
        // 后截断，取多取少的遍历量一样。
        self.dm
            .search_prefix("", limit)
            .into_iter()
            .map(|mut c| {
                c.source = CandidateSource::CodeTable;
                c
            })
            .collect()
    }

    fn browse_display_limit(&self) -> Option<usize> {
        // 精确匹配模式（关前缀枚举）下浏览态只展示一条，与空码补全「取首位后续码」同语义。
        self.opts.single_code_input.then_some(1)
    }

    fn convert(&self, input: &str, max_candidates: usize) -> anyhow::Result<ConvertResult> {
        if input.is_empty() {
            return Ok(ConvertResult::default());
        }

        let limit = max_candidates.max(50);
        let mut candidates: Vec<Candidate> = Vec::new();
        // text -> 已入列候选的下标。**不能退回 `HashSet`**：同文本重复命中时要把被丢弃那条
        // 的码位并进幸存者（`absorb_codes_from`），否则「检索范围」过滤按 (source, code) 分组
        // 时会丢掉「该码位下有常用字」这一事实，见 `Candidate::merged_codes`。
        let mut seen: HashMap<String, usize> = HashMap::new();

        // 精确匹配优先（完整编码）
        for mut c in self.dm.search(input, limit) {
            // ⚠️ source 必须**先于** `absorb_codes_from` 赋值：该方法跨来源直接 return，
            // 而 `dm` 返回的候选 source 还是 `None`，晚一步赋值会让归并静默失效。
            c.source = CandidateSource::CodeTable;
            if let Some(&idx) = seen.get(&c.text) {
                candidates[idx].absorb_codes_from(&c);
                continue;
            }
            seen.insert(c.text.clone(), candidates.len());
            // 精确层级随候选流动，供协调器重排时沿用（见 `cmp_exact_first`）。
            c.is_exact_code = c.code == input;
            candidates.push(c);
        }

        // has_longer 一次求值复用：空码补全的前置判据、自动上屏判定与满码空码清空共用同一
        // 「更长后继」索引查询（直问有序索引，不物化候选），避免每次按键各查一次。
        let has_longer = self.has_longer_code(input);

        // 前缀匹配补充（精确匹配模式下跳过）
        let mut completion_hints: Vec<Candidate> = Vec::new();
        if !self.opts.single_code_input {
            for mut c in self.dm.search_prefix(input, limit) {
                // source 须先于 absorb 赋值，理由同上面的精确循环。
                c.source = CandidateSource::CodeTable;
                if let Some(&idx) = seen.get(&c.text) {
                    // 简码字在此被吃掉：打 `siv` 时「档」已由精确循环以 code="siv" 入列，
                    // 这条 code="sivg" 的同字条目被丢弃 —— 但 sivg 码位确实被一个常用字占着，
                    // 该事实必须留给「检索范围」过滤，否则同码位的生僻字（桜）会当孤儿码放行。
                    //
                    // ⚠️ **不要在此继承被丢弃那条的权重**：它是另一个码位的词条（这里丢的
                    // 正是 code 更长的那条），权重属于 `(code, text)` 而非「字」。曾经加过，
                    // 结果让精确候选带上了全码条目的权重——见 `merge_search` 里同一条原则。
                    candidates[idx].absorb_codes_from(&c);
                    continue;
                }
                seen.insert(c.text.clone(), candidates.len());
                // 前缀扫描也会命中输入自身（"usr".starts_with("usr")）。正常情况该条已被
                // 上面的精确循环占位去重，此处按 code 判定只为不依赖循环先后顺序。
                c.is_exact_code = c.code == input;
                candidates.push(c);
            }
        } else if self.opts.single_code_complete && candidates.is_empty() && has_longer {
            // 不设码长闸门（论坛 t302）：码表可以收超码长的词条（小鹤音形实测），满码长乃至
            // 超码长空码时照样有后续编码可提示，直到打全。判据只剩「有更长后继」，无后继时
            // 连前缀扫描都不做。超码长那一段与顶码、整句的关系：有后继时顶码不触发；开整句时
            // 若整句可解，屏上是整句、补全不显示（协调器按最终列表判空后才采纳），整句解不出
            // 才轮到补全。
            // 空码补全：从更长编码备一小池候选作提示。
            // limit=8：够协调器过滤后仍有得选，又避免全量前缀扫描开销。
            //
            // 只备货、不入列：`candidates.is_empty()` 在这一层只代表「码表没货」，而补全该不该
            // 出的判据是「最终屏幕上一条都没有」——协调器随后还要叠短语。就地 push 会在短语
            // 已命中时多冒一条后续编码。交由协调器按最终列表定夺，见 `ConvertResult::completion_hints`。
            //
            // ⚠️ **备池而非择一**（此前 `.find()` 只取首条）：协调器要在 shadow / 检索范围
            // 过滤之后才择一，只给一条的话用户隐藏掉它就无货可补、屏幕全空，而词库里其实
            // 还有下一条——「从池中择 N 条必须发生在过滤之后」。
            completion_hints = self
                .dm
                .search_prefix(input, 8)
                .into_iter()
                .filter(|c| c.code != input)
                .map(|mut c| {
                    c.source = CandidateSource::CodeTable;
                    c
                })
                .collect();
        }

        // 排序：精确匹配（code==input）优先，其内按基础维度 weight（默认，better）或
        // natural（by_natural，纯出现序、忽略权重）。
        //
        // 精确优先必须是**常驻主键**而非仅截断时的临时分区：词组权重取自词频、单字权重取自
        // 字频，两套量纲不可比，纯按权重排会让简码字沉底——如「新的」(usrq, 47487) 与
        // 「新手」(usrt, 22229) 双双压过简码「新」(usr, 11777)，把它挤到第三位。
        //
        // 该层级同时落在 `Candidate::is_exact_code` 上随候选流动：协调器合并短语后会用
        // `candidate_display_order` 无条件重排全部候选，只在此处排好而不落字段，下游重排即
        // 按纯权重推翻本层结果（此前的实际行为）。两处共用 `cmp_exact_first` 这一个键。
        let base_cmp = self.opts.base_sort.cmp();
        candidates.sort_by(|a, b| cmp_exact_first(a, b).then_with(|| base_cmp(a, b)));

        // 整句：超码长且整串无精确解时，把这串码切成多个编码单元组句。
        //
        // **排序之后 insert(0)，而不是混进排序**：`base_sort=natural` 的方案忽略权重，
        // 整句再高的 weight 也排不到前面去（同拼音侧 step ② 的 `insert(0)` 做法）。
        let sentence = self.decode_sentence(input, &candidates);
        let sentence_split = sentence.split;
        candidates.splice(0..0, sentence.shown);

        // 逆切分：**恰好**满码长且空码时，把这串码切成两段、各查词典后拼接成组合候选。
        //
        // **append 而非 `insert(0)`**：默认档（`SplitTrigger::Empty`）下列表本就是空的，
        // append 即全部；`NoExact` 档下则自然排在既有候选之后。两档共用一套代码、零分支。
        // 跨过协调器重排后的沉底由 `candidate_display_order` 的 `is_split_composed` 层保证。
        //
        // ⚠️ 必须在 `truncate` **之前**：`handle_top_code` 以 `convert(prefix, 1)` 取顶码
        // 首选（原帖「后码顶首选上屏」那条规则靠它成立），放到 truncate 之后会让那次调用
        // 拿回超过 limit 条候选，破坏 `max_candidates` 契约。
        //
        // ⚠️ 也必须在下面 `is_empty` 求值**之前**：`should_clear`（满码空码清空）读的正是
        // 那个 `is_empty`。写反了就是「切分出候选了但缓冲被清空」——用户看到候选一闪即逝。
        let (split, split_code) = self.decode_split(input, &candidates);
        candidates.extend(split);

        // 精确匹配已居首，截断不会再把它挤出配额（此前需一次临时分区保护：单字母等短输入下
        // 前缀候选可达数百，纯按基础序截断会让低权重简码字丢失，此后协调器再排也找不回）。
        candidates.truncate(max_candidates);

        // 编码提示(码表自身):前缀候选标注「剩余编码」=候选全码去掉已输入前缀(对齐 Go codetable.go)。
        // 精确候选(code==input)剩余为空 → 不标注。已有 comment 的候选不覆盖。
        if self.opts.show_code_hint {
            let input_len = input.chars().count();
            // 补全备选一并标注：它们已移出 `candidates`（见上方 completion_hints），若不接进本
            // 循环，协调器采纳后会缺「剩余编码」注释——而它恰恰是全场最需要该提示的候选（码更长）。
            for c in candidates.iter_mut().chain(completion_hints.iter_mut()) {
                if c.comment.is_empty() && c.code.chars().count() > input_len {
                    c.comment = c.code.chars().skip(input_len).collect();
                }
            }
        }

        let is_empty = candidates.is_empty();
        let (should_commit, commit_text) = match decide_auto_commit(
            self.opts.auto_commit_at_full,
            self.opts.auto_commit_min_len,
            input,
            &candidates,
            has_longer,
        ) {
            Some(text) => (true, text),
            None => (false, String::new()),
        };
        // 满码空码清空的**资格**：码长达满码 + 无更长后继（避免吞掉长码精确匹配）。
        //
        // 不看本处候选是否为空：检索范围、单字输入等过滤在协调器里才做，`lynn` 这里有
        // 「甿」「軶」，常用字档下用户看到的却是空列表。空不空由协调器按最终列表裁决
        // （`clear_blocked_by_candidates`）。
        let should_clear = self.opts.clear_on_empty_max
            && input.chars().count() >= self.max_code_length
            && !has_longer;
        Ok(ConvertResult {
            candidates,
            preedit_display: input.to_string(),
            is_empty,
            should_commit,
            commit_text,
            should_clear,
            completion_hints,
            // 整句与逆切分的闸门互斥（`> max_code_length` vs `==`），两个切分串不可能同时
            // 非空；真撞上时以整句为准——超码长那一格本就是它的。
            preedit_codetable: if sentence_split.is_empty() {
                split_code
            } else {
                sentence_split
            },
            sentence_pool: sentence.pool,
            ..Default::default()
        })
    }

    fn reset(&self) {}

    fn sentence_input_enabled(&self) -> bool {
        self.sentence.is_some()
    }

    fn engine_type(&self) -> EngineType {
        EngineType::CodeTable
    }

    fn max_code_length(&self) -> usize {
        self.max_code_length
    }

    fn input_chars(&self) -> Option<&wind_config::CodeCharSet> {
        Some(&self.charset)
    }

    fn wildcard_key(&self) -> Option<char> {
        self.opts.wildcard
    }

    /// 通配转换（spec §5.2）：只查词库（系统 / 用户 / 临时；草稿层不参与），不走整句、
    /// 逆切分、英文混入；「精确」改判「等长」（落在 `is_exact_code` 上，协调器重排沿用）；
    /// 注释恒为完整编码（学码价值所在，不受 `show_code_hint` 门控）；
    /// `should_commit` / `should_clear` 恒 false（spec §3.2）。
    /// 影子层（未启用扩展库）挂着就并查，命中注释带 `*`；挂不挂由 `input.reverse.lookup_disabled_dicts` 定。
    fn convert_wildcard(
        &self,
        input: &str,
        pattern: &str,
        max_candidates: usize,
    ) -> Option<ConvertResult> {
        self.opts.wildcard?;
        Some(self.wildcard_query(input, pattern, max_candidates, true))
    }

    fn reverse_wildcard_key(&self) -> Option<char> {
        self.opts.reverse_key
    }

    fn disabled_dict_layers(&self) -> Option<&super::DisabledDictLayers> {
        self.disabled_dicts.as_ref()
    }

    /// 反查模式查询（reverse-mode spec §3.2）：与 `convert_wildcard` 共用 `wildcard_query`，
    /// 但不看 `wildcard` 主开关；仅单字随内核自动生效。并查影子层（`include_disabled = true`）：
    /// 影子层只在开关 `input.reverse.lookup_disabled_dicts` 开时才由 `build_engine` 挂上，
    /// 挂了就查，引擎不另存开关。
    fn convert_reverse(
        &self,
        input: &str,
        pattern: &str,
        max_candidates: usize,
    ) -> Option<ConvertResult> {
        self.opts.reverse_key?;
        Some(self.wildcard_query(input, pattern, max_candidates, true))
    }

    /// natural 模式（`base_sort = "natural"`）忽略权重：协调器据此对齐 `by_natural` 重排。
    fn base_sort_ignores_weight(&self) -> bool {
        matches!(self.opts.base_sort, BaseSort::Natural)
    }

    fn has_full_input_match(&self, input: &str) -> bool {
        CodeTableEngine::has_full_input_match(self, input)
    }

    fn has_longer_code(&self, input: &str) -> bool {
        CodeTableEngine::has_longer_code(self, input)
    }

    /// 顶码上屏（对齐 Go HandleTopCode）：超过满码长 + 整串无精确匹配 + 无更长后继时，
    /// 取前 max_code_length 码的首选上屏，返回 (上屏文本, 剩余编码)。
    fn recheck_auto_commit(&self, input: &str, candidates: &[Candidate]) -> Option<String> {
        decide_auto_commit(
            self.opts.auto_commit_at_full,
            self.opts.auto_commit_min_len,
            input,
            candidates,
            self.has_longer_code(input),
        )
    }

    fn handle_top_code(&self, input: &str) -> Option<(String, String)> {
        if !self.opts.top_code_commit {
            return None;
        }
        // ★ 整句与顶码抢的是同一个区间（超码长），且顶码是**自动上屏**——它一触发，
        // 用户根本看不到整句候选。两者语义直接冲突，故整句开启时顶码让位。
        //
        // 判据取「功能是否开启」而非「本次有没有解出整句」：后者会让顶码在同一串码上
        // 时灵时不灵（多打一个字母解出整句就不顶了），是最难排查的那种不一致。
        if self.sentence.is_some() {
            return None;
        }
        if input.chars().count() <= self.max_code_length {
            return None;
        }
        // 整串若仍是精确匹配或有更长后继，说明不是「溢出顶字」，交回正常流程。
        if self.has_full_input_match(input) || self.has_longer_code(input) {
            return None;
        }
        let prefix: String = input.chars().take(self.max_code_length).collect();
        let remainder: String = input.chars().skip(self.max_code_length).collect();
        // 码表首选文本；prefix 码表无字（短语专属码如 date/zzbd）时留空，由上层用显示首选
        // （短语/命令）兜底顶码。此处**只判定溢出该顶**（超满码长 + 无全码匹配 + 无更长后继），
        // 「顶什么」交上层——原 `first()?` 短路会让码表无字时顶码整个不触发（短语顶不了）。
        let top = self
            .convert(&prefix, 1)
            .ok()
            .and_then(|r| r.candidates.first().map(|c| c.text.clone()))
            .unwrap_or_default();
        Some((top, remainder))
    }
}

impl ExtendedEngine for CodeTableEngine {
    fn max_code_length(&self) -> usize {
        self.max_code_length
    }

    fn should_auto_commit(&self, input: &str, candidates: &[Candidate]) -> Option<String> {
        decide_auto_commit(
            self.opts.auto_commit_at_full,
            self.opts.auto_commit_min_len,
            input,
            candidates,
            self.has_longer_code(input),
        )
    }

    fn handle_empty_code(&self, _input: &str) -> (bool, bool, String) {
        (true, false, String::new())
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
