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

/// 逆切分的触发档（`[engine.codetable].split_trigger`）：满码长时「空到什么程度」才切分。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SplitTrigger {
    /// 整串**一条候选都没有**才切（默认）。只填「现在什么都没有」的那一格，
    /// 不抢任何现有候选，回归面接近零。
    #[default]
    Empty,
    /// 整串**没有精确解**（无 `code == input` 的候选）即切，即便有前缀/补全候选。
    /// 覆盖面更大，但会改变既有候选序 ⇒ 此档产出的切分候选一律**沉底**
    /// （`candidate_display_order` 的 `is_split_composed` 层）且**不参与自动上屏**
    /// （`decide_auto_commit` 的「恰一个精确匹配」判据此时本就不成立）。
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
    /// 精确匹配空码补全：精确无候选且未满码时，从更长编码取首选（对齐 Go SingleCodeComplete）。
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
    /// 逆切分（切分模式）：**恰好**满码长的串若空码，切成 2+2 两段各查词典再拼接。
    /// 见 `docs/design/codetable-split-input.md`。
    ///
    /// **方案级引擎固定参数**（同 `sentence_input`）：能不能逆切分取决于这张码表有没有
    /// 成体系的二简、二简空间是否留了余量，是编码方案的结构事实。出厂关闭。
    ///
    /// ⚠️ 与 `sentence_input` **占的是不同区间**（`== max_code_length` vs `>`），
    /// 两者可以同开、互不让位；但同开时顶码会因整句而整体让位
    /// （见 [`CodeTableEngine::handle_top_code`]），逆切分候选的「后码顶首选上屏」随之失效。
    pub split_input: bool,
    /// 逆切分的**前段**取几条候选（`[engine.codetable].split_front_candidates`）。
    ///
    /// 默认 1（前段恒取首选）：取更多会让**同前段的变体**占满候选窗——后段有 4 个重码时，
    /// 前段取 2 就是 8 条，其中后 4 条共享一个用户多半不想要的前段。
    ///
    /// 这个旋钮存在的意义是让上面那条取舍**可被实测推翻**，不是给终端用户调的。
    /// 0 与缺省等同于 1。
    pub split_front_candidates: usize,
    /// 逆切分的触发档。见 [`SplitTrigger`]。
    pub split_trigger: SplitTrigger,
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
        // 逆切分的切点由 `max_code_length` 唯一决定（见 `split_at`），奇数/过短码长下无解。
        // **就地关掉并告警**，而不是留给 `split_at()` 每次按键静默返回 None：配置者只会
        // 观察到「开了没反应」，拿不到任何线索。
        if opts.split_input && !(max_code_length >= 4 && max_code_length.is_multiple_of(2)) {
            tracing::warn!(
                max_code_length,
                "[engine.codetable] split_input 需要偶数且 ≥4 的 max_code_length（切点取其一半），\
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
        }
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

    /// 指明**整句词频**的来源目录（见 `sentence::SentenceFreq`）。词库到首次整句解码时才读。
    ///
    /// 整句未开启时是 no-op —— 没有解码器可交代。
    pub fn with_sentence_schemas_dir(mut self, dir: std::path::PathBuf) -> Self {
        if let Some(d) = self.sentence.take() {
            self.sentence = Some(d.with_schemas_dir(dir));
        }
        self
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

    /// 后台预热整句的两张懒表（简码索引 + 拼音词频）。整句未开启时是 no-op。
    ///
    /// 由构建方在引擎组装完毕、**所有 `with_*` 都已调用之后**调用一次——预热线程读的是
    /// 那时的 `freq_source`，早于 `with_sentence_schemas_dir` 就白跑一趟没词频的预热。
    /// 开销数字与「为什么必须搬到后台」见 `sentence::LazyTables`。
    pub fn prewarm_sentence(&self) {
        if let Some(d) = &self.sentence {
            d.prewarm(Arc::clone(&self.dm));
        }
    }

    /// 整句解码：产出一条覆盖整串的整句候选，或 `None`。
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
    /// `(整句候选, 编码单元切分串)`。切分串给组合区显示用
    /// （见 `ConvertResult::preedit_codetable`），无整句解时为空串。
    fn decode_sentence(
        &self,
        input: &str,
        candidates: &[Candidate],
    ) -> (Option<Candidate>, String) {
        let none = (None, String::new());
        let Some(decoder) = self.sentence.as_ref() else {
            return none;
        };
        if input.chars().count() <= self.max_code_length {
            return none;
        }
        if candidates.iter().any(|c| c.is_exact_code) {
            return none;
        }
        let Some(r) = decoder.decode(input, &self.dm) else {
            return none;
        };
        let split = r.split_code(input);
        (
            Some(Candidate {
                text: r.text,
                code: input.to_string(),
                weight: super::sentence::SENTENCE_WEIGHT_BASE,
                source: CandidateSource::CodeTable,
                is_sentence: true,
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
                // 学进用户词库，那份假边界会一路传下去。切分显示另找出口。
                boundary: 0,
                ..Default::default()
            }),
            split,
        )
    }

    /// 逆切分的切点：`max_code_length / 2`，仅当码长为**偶数且 ≥ 4** 时有效。
    ///
    /// **不做成配置项**：切点是「这张码表的二简在哪里结束」，由 `max_code_length` 唯一决定。
    /// 多给一个自由参数只会制造 `max_code_length = 4, split_at = 3` 这种配出来不报错、
    /// 打起来全是错的状态。奇数/过短码长下返回 `None`（功能整体不生效，构建期已告警）。
    fn split_at(&self) -> Option<usize> {
        (self.max_code_length >= 4 && self.max_code_length.is_multiple_of(2))
            .then_some(self.max_code_length / 2)
    }

    /// 逆切分：恰好满码长的空码串切成两段，各查一次词典后拼接成组合候选。
    ///
    /// # 四道门槛
    ///
    /// 1. **功能开启**（`opts.split_input`）且**切点有效**（见 [`Self::split_at`]）；
    /// 2. **恰好**满码长 —— 超码长那一段归顶码与整句（两者的闸门都在 `>` 上，
    ///    本功能刻意只占 `==` 这一格，与它们零重叠、不必和谁让位）；未满码时还有更长
    ///    后继可打，切分等于替用户提前认定「这串到此为止」（整句门槛的注释记着真机反例：
    ///    `aaw`（本意 `aawt`→「工作」）会被读成「啊啊我」）；
    /// 3. **`split_trigger` 定的空度**（[`SplitTrigger::allows`]）；
    /// 4. **两段都查得到** —— 切一半没有意义，半截结果只会让用户以为词库缺条目。
    ///
    /// # 排序
    ///
    /// `weight` 取两段的**较小值**：组合的可信度不高于最弱的那一段。前段恒取首选时
    /// （默认）这等价于「按后段权重序」，与构造序一致；`natural_order` 填构造序作为
    /// 同权重时的末级键，`base_sort = natural` 的方案也因此拿到同一个序。
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
        let Some(split_at) = self.split_at() else {
            return none;
        };
        let chars: Vec<char> = input.chars().collect();
        if chars.len() != self.max_code_length {
            return none;
        }
        if !self.opts.split_trigger.allows(candidates) {
            return none;
        }

        let front_code: String = chars[..split_at].iter().collect();
        let back_code: String = chars[split_at..].iter().collect();
        // ⚠️ **不得按 `c.code == 段码` 过滤**：`DictManager::search` 走的是
        // `CompositeDict::merge_search(.., Query::Exact)`，查询本身已经是精确的、不会混进
        // 前缀候选；但同一条 `merge_search` 在跨层合并时会「同 text 取最短码」——「能」若
        // 同时在 `kn` 与某个一简位上，返回条目的 `code` 会被换成那个更短的码。
        // **查询口径与 code 字段不是同一件事**，按 code 过滤会误杀正确的二简候选。
        // 本函数只读段候选的 `text` 与 `weight`。
        let front = self
            .dm
            .search(&front_code, self.opts.split_front_candidates.max(1));
        if front.is_empty() {
            return none;
        }
        let back = self.dm.search(&back_code, SPLIT_BACK_LIMIT);
        if back.is_empty() {
            return none;
        }

        let mut out = Vec::with_capacity(front.len() * back.len());
        for f in &front {
            for b in &back {
                out.push(Candidate {
                    text: format!("{}{}", f.text, b.text),
                    // 整串：`decide_auto_commit` 的判据是「恰一个 `code == input` 的候选」，
                    // 后段唯一时它恰好成立 ⇒ 原帖「前后都唯一即自动上屏」零额外判据地实现。
                    code: input.to_string(),
                    weight: f.weight.min(b.weight),
                    natural_order: out.len() as i32,
                    source: CandidateSource::CodeTable,
                    is_split_composed: true,
                    // 词库里没有以它为整体的词条 —— 自动造词据此判「值不值得学」。
                    // 打过一次下次 `hfkn` 直接有，切分从此不必再算。
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
        let split = format!("{front_code}{SPLIT_SEPARATOR}{back_code}");
        (out, split)
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
    let mut exact = candidates
        .iter()
        .filter(|c| c.code == input && !c.is_scope_filtered);
    let first = exact.next()?;
    if exact.next().is_some() {
        return None; // 多个精确匹配，不自动上屏
    }
    if has_longer {
        return None;
    }
    Some(first.text.clone())
}

impl Engine for CodeTableEngine {
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
        } else if self.opts.single_code_complete
            && candidates.is_empty()
            && input.chars().count() < self.max_code_length
        {
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
        let (sentence, sentence_split) = self.decode_sentence(input, &candidates);
        if let Some(c) = sentence {
            candidates.insert(0, c);
        }

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
        // has_longer 一次求值复用：自动上屏判定与满码空码清空共用同一「更长后继」前缀扫描，
        // 避免每次按键各查一次 search_prefix（此前经 should_auto_commit + should_clear 两次）。
        let has_longer = self.has_longer_code(input);
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
        // 满码空码清空：无候选 + 码长达满码 + 无更长后继（避免吞掉长码精确匹配）。
        let should_clear = is_empty
            && self.opts.clear_on_empty_max
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
mod tests {
    use super::*;
    use wind_candidate::Candidate;
    use wind_dict::SystemDictLayer;
    use wind_dict::cached::CachedDict;
    use wind_dict::codetable::CodetableDict;

    fn cand(code: &str, text: &str) -> Candidate {
        Candidate {
            code: code.to_string(),
            text: text.to_string(),
            ..Default::default()
        }
    }

    /// ★ 护栏：检索范围放宽补回的候选**不得**影响满码自动上屏。
    ///
    /// 今天的自动上屏有一部分是靠智能过滤滤掉同码生僻字才成立的（见
    /// `recheck_auto_commit_unique_after_filter`）。放宽（自动补充 / 手动临时切换）会把这些字
    /// 补回候选列表，若不在计数时排除，一批原本满码即上屏的字会**静默退化**成要多按一次空格，
    /// 用户只感到「上屏时灵时不灵」。设计见 docs/design/smart-filter-scope-relax.md §2.1。
    #[test]
    fn scope_filtered_candidates_do_not_block_auto_commit() {
        let mut relaxed = cand("hhnu", "愳");
        relaxed.is_scope_filtered = true; // 智能档下本应被滤，因放宽才在列表里
        let with_relaxed = [cand("hhnu", "X"), relaxed];
        assert_eq!(
            decide_auto_commit(true, 4, "hhnu", &with_relaxed, false).as_deref(),
            Some("X"),
            "放宽补回的同码生僻字不该否决满码自动上屏"
        );

        // ★ 反向对照：同样两条候选，只是不带放宽标记（即它本就通过了过滤）→ 仍须照旧否决。
        // 没有这条，一个「无条件放行」的错误实现同样能让上面那句变绿。
        let both_normal = [cand("hhnu", "X"), cand("hhnu", "愳")];
        assert_eq!(
            decide_auto_commit(true, 4, "hhnu", &both_normal, false),
            None,
            "未经放宽的两个精确同码候选仍须否决上屏（放行了说明排除条件写宽了）"
        );

        // 边界：全部候选都是放宽补回的 → 无可上屏的正常候选，不上屏
        let mut only_relaxed = cand("hhnu", "愳");
        only_relaxed.is_scope_filtered = true;
        assert_eq!(
            decide_auto_commit(true, 4, "hhnu", &[only_relaxed], false),
            None,
            "只有放宽候选时不该拿它自动上屏"
        );
    }

    #[test]
    fn decide_basic_unique_full() {
        let cands = [cand("aaaa", "工")];
        assert_eq!(
            decide_auto_commit(true, 4, "aaaa", &cands, false),
            Some("工".to_string())
        );
    }

    #[test]
    fn decide_blocked_when_disabled_or_short() {
        let cands = [cand("aaaa", "工")];
        assert_eq!(decide_auto_commit(false, 4, "aaaa", &cands, false), None);
        // 码长不足 min_len
        assert_eq!(
            decide_auto_commit(true, 4, "aaa", &[cand("aaa", "x")], false),
            None
        );
    }

    #[test]
    fn decide_blocked_when_ambiguous_or_has_longer() {
        // 两个精确匹配 → 不上屏
        let two = [cand("aaaa", "工"), cand("aaaa", "戈")];
        assert_eq!(decide_auto_commit(true, 4, "aaaa", &two, false), None);
        // 有更长后继 → 不上屏
        let one = [cand("aa", "式")];
        assert_eq!(decide_auto_commit(true, 2, "aa", &one, true), None);
    }

    fn engine_with(
        entries: &[(&str, &str, i32)],
        at_full: bool,
        min_len: usize,
    ) -> CodeTableEngine {
        engine_opts(
            entries,
            CommitOptions {
                auto_commit_at_full: at_full,
                auto_commit_min_len: min_len,
                ..Default::default()
            },
        )
    }

    /// 双词库夹具：主库 + 扩展库各一层，返回引擎与 `DictManager`（后者用于热启停扩展库）。
    fn engine_two_dicts(
        main: &[(&str, &str, i32)],
        ext: &[(&str, &str, i32)],
    ) -> (CodeTableEngine, Arc<DictManager>) {
        let build = |entries: &[(&str, &str, i32)]| {
            let mut d = CodetableDict::empty();
            for (i, (code, text, w)) in entries.iter().enumerate() {
                d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
            }
            CachedDict::Memory(d)
        };
        let dm = Arc::new(DictManager::new());
        dm.register_layer(Box::new(SystemDictLayer::new(build(main), "main")));
        dm.register_layer(Box::new(SystemDictLayer::new(build(ext), "ext")));
        let e = CodeTableEngine::new(4, CommitOptions::default(), dm.clone());
        (e, dm)
    }

    /// `set_dict_enabled` 必须先分辨「这个 id 是不是我的」，再谈处理（t107 的回归守门）。
    ///
    /// # 为什么这一步不能省
    ///
    /// `MixedEngine::set_dict_enabled` 把同一次调用转发给 primary / secondary / english
    /// 三个子引擎并取 `a || b || c`。码表子引擎因此照样会收到**拼音库**的 id。若它对任何
    /// id 都回 true（早先的写法就是这样），那个或运算恒真 ⇒ 调用方判定「已生效」⇒
    /// 真正承载该库、且**不支持**热插拔的拼音子引擎所需要的失效重建被整个吞掉：
    /// 用户禁用拼音方案的扩展库，独立方案下次生效，混输里却继续出该库的候选，直到重启。
    ///
    /// 启用方向一律回 false：惰性加载之后未启用的库根本没建层，这里无从恢复，
    /// 必须交调用方失效重建 —— 这也是为什么调用方**不能**再拿返回值给启用路径做路由。
    #[test]
    fn set_dict_enabled_only_claims_own_dicts() {
        let (e, _dm) = engine_two_dicts(&[("a", "工", 100)], &[("a", "工", 5000)]);
        let e = e.with_own_extra_dicts(["mine".to_string()]);

        assert!(
            e.set_dict_enabled("mine", false),
            "本方案声明过的库：禁用应认领（摘到与否都算达成 —— 惰性加载可能本就没装它）"
        );
        assert!(
            !e.set_dict_enabled("not-mine", false),
            "**不是**本方案的库：必须回 false，否则混输的 a||b||c 会恒真、掩盖真正承载方的重建需求"
        );
        assert!(
            !e.set_dict_enabled("mine", true),
            "启用方向一律回 false：层没建过，这里无从恢复，交调用方失效重建"
        );
        assert!(
            !e.set_dict_enabled("not-mine", true),
            "既不是我的、又是启用，更该回 false"
        );
    }

    /// 默认（未登记任何 own_extra_dicts）时对一切 id 回 false —— 退化成「失效重建」，
    /// 可能多跑一次重建，但不会给出错误答案。既有调用点与测试因此无需改动。
    #[test]
    fn set_dict_enabled_defaults_to_disowning_everything() {
        let (e, _dm) = engine_two_dicts(&[("a", "工", 100)], &[("a", "工", 5000)]);
        assert!(!e.set_dict_enabled("ext", false));
        assert!(!e.set_dict_enabled("ext", true));
    }

    /// ★★★ 跨词库同词条合并的主键是 `(code, text)`，不是 `text`。
    ///
    /// ① 两库收录**同一条**（码相同）→ 按最高权重算，这是「多个词库有同一个 code+词、
    ///    权重不同时以最高者为准」那条用户可见语义；关掉出该权重的库即回退。
    /// ② 两库里该词**码不同** → 那是**两个词条**，权重各归各的码位，不得互相继承。
    ///
    /// ② 尤其要钉住：曾经按 text 无条件取 max，于是打 `a` 时精确候选「工」带上了扩展库
    /// 全码 `ab` 那条的权重。码表方案里码长本身就是分档依据，简码条目凭空拿到全码条目的
    /// 高权重会直接改掉首选。
    ///
    /// 两条必须并存：只有 ① 时，一个「无条件跨码位取 max」的实现照样全绿；只有 ② 时，
    /// 一个「永不继承」的实现也全绿。
    #[test]
    fn cross_dict_weight_merges_by_code_and_text() {
        // ① 同码：主库 100 / 扩展库 5000 → 取 5000，来源标注指向扩展库。
        let (e, dm) = engine_two_dicts(&[("a", "工", 100)], &[("a", "工", 5000)]);
        let pick = |e: &CodeTableEngine| {
            e.convert("a", 50)
                .unwrap()
                .candidates
                .into_iter()
                .find(|c| c.text == "工")
                .expect("应有候选「工」")
        };
        let gong = pick(&e);
        assert_eq!(gong.weight, 5000, "同一词条被两库收录时按最高权重算");
        assert_eq!(
            gong.meta.weight_layer.as_deref(),
            Some("ext"),
            "权重来源须标为扩展库，否则调试段会把它记在主库头上"
        );

        // 关掉扩展库 → 回退到主库权重，来源标注一并回退（不得残留 ext）。
        assert!(dm.set_layer_enabled("ext", false));
        let gong_off = pick(&e);
        assert_eq!(gong_off.weight, 100, "扩展库关闭后回退到主库权重");
        assert_eq!(gong_off.meta.weight_layer.as_deref(), Some("main"));

        // ② 异码：主库简码 a(100)、扩展库全码 ab(5000)，打 `a`。
        let (e2, _) = engine_two_dicts(&[("a", "工", 100)], &[("ab", "工", 5000)]);
        let gong2 = pick(&e2);
        assert_eq!(gong2.code, "a", "打 a 命中的是简码那条");
        assert_eq!(
            gong2.weight, 100,
            "权重须是简码 `a` 自己的 100——`ab` 是另一个词条，它的 5000 不得漂过来"
        );
        assert_eq!(
            gong2.meta.weight_layer.as_deref(),
            Some("main"),
            "来源仍是主库：权重压根没换过"
        );
        assert!(
            gong2.merged_codes.iter().any(|c| c == "ab"),
            "被丢弃那条的**码位**仍要并入（检索范围过滤依赖它）——不继承的是权重，不是码位"
        );
    }

    fn engine_opts(entries: &[(&str, &str, i32)], opts: CommitOptions) -> CodeTableEngine {
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(
            CachedDict::Memory(d),
            "codetable-system",
        )));
        CodeTableEngine::new(4, opts, Arc::new(dm))
    }

    #[test]
    fn clear_on_empty_at_full_len() {
        // 满码(4) 无候选 + clear_on_empty_max → should_clear
        let e = engine_opts(
            &[("aaaa", "工", 100)],
            CommitOptions {
                clear_on_empty_max: true,
                ..Default::default()
            },
        );
        let r = e.convert("zzzz", 50).unwrap();
        assert!(r.is_empty && r.should_clear, "满码空码应请求清空");
        // 未满码的空码不清空
        let r2 = e.convert("zz", 50).unwrap();
        assert!(r2.is_empty && !r2.should_clear, "未满码空码不应清空");
    }

    #[test]
    fn top_code_commits_overflow_prefix() {
        // max=4，"aaaa"=工 唯一全码；输入 "aaaab"（>4，整串无匹配/无更长）→ 顶前4码"工"，余 "b"
        let e = engine_opts(
            &[("aaaa", "工", 100)],
            CommitOptions {
                top_code_commit: true,
                ..Default::default()
            },
        );
        let top = e.handle_top_code("aaaab");
        assert_eq!(top, Some(("工".to_string(), "b".to_string())));
        // 关闭开关 → None
        let e2 = engine_opts(&[("aaaa", "工", 100)], CommitOptions::default());
        assert_eq!(e2.handle_top_code("aaaab"), None);
    }

    #[test]
    fn top_code_overflow_prefix_no_char_returns_empty_top() {
        // prefix 码表无字（短语专属码场景）：仍判定溢出该顶，返回 Some(("", 余码))——
        // 「顶什么」交上层用短语显示首选兜底。原 `first()?` 短路会让顶码整个不触发。
        let e = engine_opts(
            &[("aaaa", "工", 100)],
            CommitOptions {
                top_code_commit: true,
                ..Default::default()
            },
        );
        // "bbbb" 无字，"bbbbc"(>4，无匹配/无更长后继) → Some(("", "c"))
        assert_eq!(
            e.handle_top_code("bbbbc"),
            Some((String::new(), "c".to_string())),
            "prefix 码表无字应返回空 top + 余码，而非 None"
        );
    }

    #[test]
    fn convert_sets_should_commit_for_unique_full_code() {
        // "aaaa" 唯一精确、无更长后继 → should_commit
        let e = engine_with(&[("aaaa", "工", 100)], true, 4);
        let r = e.convert("aaaa", 50).unwrap();
        assert!(r.should_commit, "唯一全码应自动上屏");
        assert_eq!(r.commit_text, "工");
    }

    #[test]
    fn convert_no_commit_when_longer_code_exists() {
        // "aaa" 精确存在，但还有更长 "aaaa" → 不自动上屏
        let e = engine_with(&[("aaa", "甲", 100), ("aaaa", "工", 90)], true, 3);
        let r = e.convert("aaa", 50).unwrap();
        assert!(!r.should_commit, "存在更长后继编码时不应自动上屏");
    }

    #[test]
    fn convert_no_commit_when_disabled() {
        let e = engine_with(&[("aaaa", "工", 100)], false, 4);
        let r = e.convert("aaaa", 50).unwrap();
        assert!(!r.should_commit);
    }

    #[test]
    fn recheck_auto_commit_unique_after_filter() {
        // 同码两个精确候选（"hhnu"→X 常用 / 愳 生僻）：引擎按未过滤候选判不唯一 → 不上屏。
        let e = engine_with(&[("hhnu", "X", 100), ("hhnu", "愳", 1)], true, 4);
        let r = e.convert("hhnu", 50).unwrap();
        assert!(!r.should_commit, "两个精确同码候选不自动上屏");
        // 模拟智能过滤后仅剩一个精确全码候选 → 复评放行。
        let filtered = [cand("hhnu", "X")];
        assert_eq!(
            e.recheck_auto_commit("hhnu", &filtered),
            Some("X".to_string()),
            "过滤后唯一精确全码应复评放行"
        );
        // 满码上屏开关关闭时复评不放行。
        let e_off = engine_with(&[("hhnu", "X", 100), ("hhnu", "愳", 1)], false, 4);
        assert_eq!(e_off.recheck_auto_commit("hhnu", &filtered), None);
    }

    #[test]
    fn single_code_input_disables_prefix() {
        // 词典：精确 "aa"→"式"，更长 "aab"→"想"。开启精确匹配后 "aa" 只应出 "式"。
        let e = engine_opts(
            &[("aa", "式", 100), ("aab", "想", 90)],
            CommitOptions {
                single_code_input: true,
                ..Default::default()
            },
        );
        let r = e.convert("aa", 50).unwrap();
        assert_eq!(r.candidates.len(), 1, "精确匹配模式不应含前缀候选");
        assert_eq!(r.candidates[0].text, "式");
    }

    #[test]
    fn single_code_complete_fills_from_longer_code() {
        // 无 "ab" 精确项；补全池应按引擎序备好更长编码候选，首条为 "abc"→"你"。
        let e = engine_opts(
            &[("abc", "你", 100), ("abd", "他", 90)],
            CommitOptions {
                single_code_input: true,
                single_code_complete: true,
                show_code_hint: true,
                ..Default::default()
            },
        );
        let r = e.convert("ab", 50).unwrap();
        // 补全候选走 `completion_hints` 旁路而**不入** `candidates`：该不该补取决于最终屏幕上
        // 有没有候选，而引擎看不见协调器随后叠加的短语，无权就地拍板（见 ConvertResult 文档）。
        assert!(r.candidates.is_empty(), "补全候选不应入引擎候选列表");
        let hint = r.completion_hints.first().expect("应备好空码补全候选");
        assert_eq!(hint.text, "你", "空码补全首选取更长编码首条");
        assert_eq!(hint.comment, "c", "补全候选应标注剩余编码");
        // 备的是**池**不是单条：协调器要在 shadow/检索范围过滤之后才择一，只备一条的话
        // 用户隐藏掉首条就无货可补、屏幕全空。
        assert!(
            r.completion_hints.iter().any(|c| c.text == "他"),
            "补全池应含次条 abd→他，实际: {:?}",
            r.completion_hints
                .iter()
                .map(|c| &c.text)
                .collect::<Vec<_>>()
        );
        assert!(!r.should_commit, "补全候选不应触发自动上屏");
    }

    #[test]
    fn single_code_complete_hint_absent_without_longer_code() {
        // 无 "ab" 精确项、也无更长后继 → 无货可备。
        let e = engine_opts(
            &[("xy", "甲", 100)],
            CommitOptions {
                single_code_input: true,
                single_code_complete: true,
                ..Default::default()
            },
        );
        let r = e.convert("ab", 50).unwrap();
        assert!(r.candidates.is_empty());
        assert!(r.completion_hints.is_empty(), "无更长编码时不应备补全候选");
    }

    #[test]
    fn exact_match_suppresses_completion_hint() {
        // 有 "ab" 精确项 → 不是空码，不该备补全（否则协调器侧判空虽拦得住，但白查一次前缀）。
        let e = engine_opts(
            &[("ab", "甲", 100), ("abc", "你", 90)],
            CommitOptions {
                single_code_input: true,
                single_code_complete: true,
                ..Default::default()
            },
        );
        let r = e.convert("ab", 50).unwrap();
        assert_eq!(r.candidates.len(), 1);
        assert!(r.completion_hints.is_empty(), "有精确候选时不备补全");
    }

    #[test]
    fn exact_match_outranks_higher_weight_prefix_words() {
        // 真实现场（古精86五笔-深海词库）：简码 usr→「新」(11777)，前缀词组 usrq→「新的」(47487)、
        // usrt→「新手」(22229)。词组权重取自词频、单字取自字频，两套量纲不可比——纯按权重排会把
        // 简码「新」挤到第三位。精确匹配须恒居首，其后的前缀候选内部仍按权重降序。
        let e = engine_opts(
            &[
                ("usr", "新", 11777),
                ("usrq", "新的", 47487),
                ("usrt", "新手", 22229),
                ("usrp", "亲近", 1861),
            ],
            CommitOptions::default(),
        );
        let r = e.convert("usr", 50).unwrap();
        let order: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            order,
            vec!["新", "新的", "新手", "亲近"],
            "精确匹配应居首、其余按权重降序"
        );
        // 该层级必须落到字段上随候选流动：协调器合并短语后会无条件重排，只在引擎内排好而
        // 不标记，下游会按纯权重把结果推翻（本 bug 的原始成因）。
        assert!(
            r.candidates[0].is_exact_code,
            "精确候选须标记 is_exact_code 供协调器重排沿用"
        );
        assert!(
            r.candidates[1..].iter().all(|c| !c.is_exact_code),
            "前缀补全候选不应被标记为精确匹配"
        );
    }

    #[test]
    fn truncate_protects_low_weight_exact_match() {
        // 精确全码 "aa"→式(权重 1) + 5 个高权重前缀词(code="aab".."aaf",权重 1000)。
        // max_candidates=3：纯按权重截断会把低权重精确「式」挤出配额丢失；分区保护须保留它。
        let e = engine_opts(
            &[
                ("aa", "式", 1),
                ("aab", "A", 1000),
                ("aac", "B", 1000),
                ("aad", "C", 1000),
                ("aae", "D", 1000),
                ("aaf", "E", 1000),
            ],
            CommitOptions::default(),
        );
        let r = e.convert("aa", 3).unwrap();
        assert_eq!(r.candidates.len(), 3, "应截断到 3 条");
        assert!(
            r.candidates.iter().any(|c| c.text == "式"),
            "低权重精确全码不应被高权重前缀词截断挤出，实际: {:?}",
            r.candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn single_code_complete_off_yields_empty() {
        let e = engine_opts(
            &[("abc", "你", 100)],
            CommitOptions {
                single_code_input: true,
                ..Default::default()
            },
        );
        let r = e.convert("ab", 50).unwrap();
        assert!(r.is_empty, "补全关闭时无精确匹配应为空");
    }

    #[test]
    fn base_sort_natural_ignores_weight_uses_appearance_order() {
        // 同码 "aa" 两候选：低权重"低"先出现（order 0）、高权重"高"后出现（order 1）。
        let entries = &[("aa", "低", 1), ("aa", "高", 100)];
        // natural：忽略权重，按出现序 → 低、高。
        let e = engine_opts(
            entries,
            CommitOptions {
                base_sort: BaseSort::Natural,
                ..Default::default()
            },
        );
        let t: Vec<String> = e
            .convert("aa", 50)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert_eq!(t, vec!["低", "高"], "natural 应按出现序、忽略权重");
        // weight（默认）：高权重在前 → 高、低。
        let e2 = engine_opts(entries, CommitOptions::default());
        let t2: Vec<String> = e2
            .convert("aa", 50)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert_eq!(t2, vec!["高", "低"], "weight 应按权重降序");
    }

    #[test]
    fn base_sort_parse_maps_strings() {
        assert_eq!(BaseSort::parse("natural"), BaseSort::Natural);
        assert_eq!(BaseSort::parse("Natural"), BaseSort::Natural);
        assert_eq!(BaseSort::parse("weight"), BaseSort::Weight);
        assert_eq!(BaseSort::parse(""), BaseSort::Weight);
        assert_eq!(BaseSort::parse("xyz"), BaseSort::Weight);
    }

    /// 构造双层码表引擎（贴近真实多词库方案）：
    /// - 主库 `codetable-system`（base_order 0，**带权重**）：同码 "aa" 两条——"主低"(w10,出现序0)、
    ///   "主高"(w100,出现序1)，故权重序与出现序**相反**（用于区分 weight/natural）。
    /// - 扩展库 `codetable-extra-x`（base_order 1，**无权重**，default_weight=50）：同码 "aa" 一条 "扩"。
    fn engine_two_layers(opts: CommitOptions) -> CodeTableEngine {
        let mut main = CodetableDict::empty();
        main.merge_single("aa".into(), "主低".into(), 10, 0);
        main.merge_single("aa".into(), "主高".into(), 100, 1);
        let mut ext = CodetableDict::empty();
        ext.merge_single("aa".into(), "扩".into(), 0, 0);

        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(
            CachedDict::Memory(main),
            "codetable-system",
        )));
        dm.register_layer(Box::new(
            SystemDictLayer::with_enabled(CachedDict::Memory(ext), "codetable-extra-x", true)
                .with_base_order(1)
                .with_default_weight(Some(50)),
        ));
        CodeTableEngine::new(4, opts, Arc::new(dm))
    }

    fn texts_of(e: &CodeTableEngine, input: &str) -> Vec<String> {
        e.convert(input, 50)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    #[test]
    fn multi_layer_weight_mode_weight_primary_default_weight_places_ext() {
        // weight 模式（默认）：权重主导 → 主高(100) > 扩(50, 由 default_weight) > 主低(10)。
        // 证明：① 权重优先于 base_order（主低虽 base_order 0 却因低权重沉底）；
        //       ② default_weight 让无权重扩展库落在 50 档（介于 100 与 10 之间）。
        let e = engine_two_layers(CommitOptions::default());
        assert_eq!(
            texts_of(&e, "aa"),
            vec!["主高", "扩", "主低"],
            "weight 模式应权重主导 + default_weight 定档"
        );
    }

    #[test]
    fn multi_layer_natural_mode_base_order_tiers_dicts_ignores_weight() {
        // natural 模式：忽略权重，按 base_order 档位分组、组内按出现序。
        // → 主库(base_order 0)整组在前：主低(出现序0)、主高(出现序1)；扩展库(base_order 1)在后：扩。
        // 证明：① base_order 分档把整个扩展库排到主库之后（与条目权重无关）；
        //       ② 组内忽略权重按出现序（主低虽权重低却因出现序靠前而在主高之前）。
        let e = engine_two_layers(CommitOptions {
            base_sort: BaseSort::Natural,
            ..Default::default()
        });
        assert_eq!(
            texts_of(&e, "aa"),
            vec!["主低", "主高", "扩"],
            "natural 模式应按 base_order 分档 + 组内出现序、忽略权重"
        );
    }

    // ───────────────────────── 整句输入（sentence_input） ─────────────────────────

    /// 五笔结构的缩微模型（与 `sentence.rs` 单测同源）：一简 / 二简 / 3 码全码 /
    /// 4 码全码 / 4 码词组俱全，权重照抄极点词库的层级带。
    const SENTENCE_ENTRIES: &[(&str, &str, i32)] = &[
        ("a", "工", 9999),
        ("g", "一", 9999),
        ("w", "人", 9999),
        ("aa", "式", 9950),
        ("wt", "何", 9950),
        ("hci", "皮", 1200),
        ("aaaa", "工", 800),
        ("ggll", "一", 700),
        ("wtgf", "人", 600),
        ("aagg", "式", 500),
        ("aawt", "工作", 1241),
    ];

    fn sentence_engine(extra: &[(&str, &str, i32)], opts: CommitOptions) -> CodeTableEngine {
        let mut entries: Vec<(&str, &str, i32)> = SENTENCE_ENTRIES.to_vec();
        entries.extend_from_slice(extra);
        engine_opts(&entries, opts)
    }

    /// 分隔符的字符串形式（测试拼串用）。
    const SEPS: &str = "'";

    fn sentence_opts() -> CommitOptions {
        CommitOptions {
            sentence_input: true,
            ..Default::default()
        }
    }

    #[test]
    fn sentence_leads_when_input_exceeds_code_length() {
        // 超码长（8 > 4）且整串无精确解 → 整句候选置顶。
        let e = sentence_engine(&[], sentence_opts());
        let r = e.convert("aawtaawt", 50).unwrap();
        let first = r.candidates.first().expect("应有候选");
        assert_eq!(first.text, "工作工作");
        assert!(first.is_sentence, "整句候选须带 is_sentence 供顶部锚定");
    }

    #[test]
    fn sentence_consumes_whole_input_and_marks_synthesized() {
        // ★ 锁住「码表候选 consumed_length 恒 0」这条全仓约定：整句只在覆盖整串时产出，
        //   故仍然消费整串。清空守护 / 词频记账 / 自动造词缓冲三处都依赖它，
        //   一旦这里改成分段上屏，那三处必须同时改（见设计文档 §7.1）。
        let e = sentence_engine(&[], sentence_opts());
        let first = e.convert("aawtaawt", 50).unwrap().candidates.remove(0);
        assert_eq!(first.consumed_length, 0, "整句须消费整串");
        assert_eq!(first.code, "aawtaawt", "整句的 code 是整串输入");
        assert!(
            first.is_synthesized,
            "词库里没有这个词条，自动造词据此判值不值得学"
        );
        // boundary 是**音节**边界（拼音域）；码表码无音节语义，必须留 0，
        // 否则整句被学进用户词库时会带一份假的音节真值。
        assert_eq!(first.boundary, 0, "码表整句不得填 boundary");
    }

    #[test]
    fn sentence_not_produced_within_code_length() {
        // ★ 真机翻车现场（`mixed/engine.rs` 有记录）：`aaw` 本意是 `aawt`→「工作」，
        //   若让它进整句路径会被读成「工工人」之类抢走首位。
        //   码长内的串本就是一个编码单元，切它没有意义。
        let e = sentence_engine(&[], sentence_opts());
        let r = e.convert("aaw", 50).unwrap();
        assert!(
            !r.candidates.iter().any(|c| c.is_sentence),
            "码长内不得产出整句候选，实得: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn sentence_off_by_default() {
        // 出厂关闭：同一串输入在默认配置下不该冒出整句。
        let e = sentence_engine(&[], CommitOptions::default());
        let r = e.convert("aawtaawt", 50).unwrap();
        assert!(!r.candidates.iter().any(|c| c.is_sentence));
    }

    #[test]
    fn sentence_skipped_when_whole_input_has_exact_match() {
        // 对齐 librime `enable_sentence_ && !translation`：整串查得到词就不走整句，
        // 否则同一个词会以两种身份进列表。这里造一条 8 码的用户长词条。
        let e = sentence_engine(&[("aawtaawt", "工作工作", 300)], sentence_opts());
        let r = e.convert("aawtaawt", 50).unwrap();
        let first = r.candidates.first().expect("应有候选");
        assert!(first.is_exact_code, "整串精确匹配应居首");
        assert!(
            !r.candidates.iter().any(|c| c.is_sentence),
            "整串有精确解时不产整句"
        );
    }

    #[test]
    fn top_code_yields_to_sentence() {
        // 顶码与整句抢同一个区间（超码长），且顶码是自动上屏、一触发用户就看不到整句。
        // 判据取「功能是否开启」，故即便本次解不出整句，顶码同样让位——
        // 否则同一串码上顶码会时灵时不灵。
        let opts = CommitOptions {
            top_code_commit: true,
            sentence_input: true,
            ..Default::default()
        };
        let e = sentence_engine(&[], opts);
        assert_eq!(e.handle_top_code("aawtaawt"), None, "整句开启时顶码让位");
        assert_eq!(e.handle_top_code("aawtzzzz"), None, "解不出整句也照样让位");

        // 对照：关掉整句，顶码恢复。
        let e2 = sentence_engine(
            &[],
            CommitOptions {
                top_code_commit: true,
                ..Default::default()
            },
        );
        let (top, rest) = e2.handle_top_code("aawtaawt").expect("顶码应触发");
        assert_eq!((top.as_str(), rest.as_str()), ("工作", "aawt"));
    }

    #[test]
    fn sentence_fills_preedit_split() {
        // 组合区切分串：一长串码配一句话时，用户要看得见引擎把它切成了哪几段。
        let e = sentence_engine(&[], sentence_opts());
        let r = e.convert("aawtaawt", 50).unwrap();
        assert_eq!(r.preedit_codetable, "aawt@aawt".replace('@', SEPS));
    }

    #[test]
    fn no_sentence_means_no_preedit_split() {
        // 码长内不产整句 ⇒ 切分串必须为空，否则组合区会显示一个不对应任何候选的切法。
        let e = sentence_engine(&[], sentence_opts());
        assert!(e.convert("aaw", 50).unwrap().preedit_codetable.is_empty());
        // 关闭整句时同理。
        let off = sentence_engine(&[], CommitOptions::default());
        assert!(
            off.convert("aawtaawt", 50)
                .unwrap()
                .preedit_codetable
                .is_empty()
        );
    }

    #[test]
    fn manual_separator_end_to_end() {
        // 手动分隔符走完整 convert：`aa'wt` 强制两个二简字，且切分串原样保留分隔符。
        let e = sentence_engine(&[], sentence_opts());
        let input = format!("aa{SEPS}wt");
        let r = e.convert(&input, 50).unwrap();
        let first = r.candidates.first().expect("应有候选");
        assert!(first.is_sentence, "分隔符输入应产出整句候选");
        assert_eq!(first.text, "式何");
        assert_eq!(r.preedit_codetable, input);
    }

    #[test]
    fn sentence_input_enabled_reports_state() {
        // 协调器据此放行分隔符键（见 `manual_separator_key`）。
        use crate::engine::Engine;
        assert!(sentence_engine(&[], sentence_opts()).sentence_input_enabled());
        assert!(!sentence_engine(&[], CommitOptions::default()).sentence_input_enabled());
    }

    #[test]
    fn sentence_uses_three_code_full_entry_end_to_end() {
        // 3 码全码「皮 hci」必须能参与整句 —— 「整句只认 4 码单元」那条捷径的反例。
        let e = sentence_engine(&[], sentence_opts());
        let r = e.convert("hciaawt", 50).unwrap();
        assert_eq!(
            r.candidates.first().map(|c| c.text.as_str()),
            Some("皮工作")
        );
    }

    // ───────────────────────── 逆切分（split_input） ─────────────────────────

    /// 音形结构的缩微模型：二简字/词 + 一个四码精确条目（对照组）。
    ///
    /// 取自论坛 t11 的原例 `hf'kn` →「很可能」，后段 `kn` 故意留两个重码。
    ///
    /// ⚠️ **不在这里放 `hfkn` 的五码扩展**：那会给 `hfkn` 添一条前缀候选，默认档的
    /// 「有候选就不切」随即把所有基本盘用例挡掉。需要它的只有 `no_exact` 档那一条用例，
    /// 由该用例自己经 `extra` 加入（[`PREFIX_ONLY_ENTRY`]）。
    const SPLIT_ENTRIES: &[(&str, &str, i32)] = &[
        ("hf", "很可", 900),
        ("kn", "能", 800),
        ("kn", "难", 500),
        ("xt", "学", 700),
        ("up", "双拼", 600),
        // 四码精确条目：默认档下「有候选就不切」的对照组。
        ("aaaa", "工", 100),
    ];

    /// 让 `hfkn` **有前缀候选却仍无精确解** —— 构造 `no_exact` 档场景的唯一条目。
    const PREFIX_ONLY_ENTRY: (&str, &str, i32) = ("hfknq", "甲", 1000);

    fn split_engine(extra: &[(&str, &str, i32)], opts: CommitOptions) -> CodeTableEngine {
        let mut entries: Vec<(&str, &str, i32)> = SPLIT_ENTRIES.to_vec();
        entries.extend_from_slice(extra);
        engine_opts(&entries, opts)
    }

    fn split_opts() -> CommitOptions {
        CommitOptions {
            split_input: true,
            ..Default::default()
        }
    }

    fn texts(r: &ConvertResult) -> Vec<&str> {
        r.candidates.iter().map(|c| c.text.as_str()).collect()
    }

    /// 基本盘：满码长空码 → 前段首选 × 后段列举，按后段权重序。
    #[test]
    fn split_composes_two_segments_on_full_length_empty_code() {
        let e = split_engine(&[], split_opts());
        let r = e.convert("hfkn", 50).unwrap();
        assert_eq!(
            texts(&r),
            vec!["很可能", "很可难"],
            "hf(很可) × kn(能/难)，后段按权重序"
        );
        assert!(
            r.candidates.iter().all(|c| c.is_split_composed),
            "组合候选须带 is_split_composed（协调器的组合区判据与沉底层都读它）"
        );
        assert!(!r.is_empty, "产出了候选，is_empty 必须为假");
    }

    /// ★ 组合候选的四条字段约定，每一条都有下游依赖，改动即回归。
    #[test]
    fn split_candidate_field_contract() {
        let e = split_engine(&[], split_opts());
        let r = e.convert("hfkn", 50).unwrap();
        let c = r.candidates.first().expect("应有组合候选");

        assert_eq!(
            c.code, "hfkn",
            "code 须是整串 —— decide_auto_commit 据此认精确"
        );
        assert_eq!(
            c.consumed_length, 0,
            "消费整串：全仓「码表候选 consumed_length 恒 0」的约定不得由本功能打破"
        );
        assert_eq!(
            c.boundary, 0,
            "boundary 是音节边界（拼音域），码表无音节语义；填切分位会让自动造词学到假真值"
        );
        assert!(
            !c.is_exact_code,
            "它不是词库里的精确解，置位会让它混进 cmp_exact_first 的精确档"
        );
        assert!(
            c.is_synthesized,
            "词库无此整体词条 ⇒ 自动造词据此判「值得学」，打过一次下次直接有"
        );
        assert_eq!(c.source, CandidateSource::CodeTable);
    }

    /// 组合区显示 `hf'kn`，与整句、拼音音节共用同一个分隔符。
    #[test]
    fn split_fills_preedit_codetable() {
        let e = split_engine(&[], split_opts());
        let r = e.convert("hfkn", 50).unwrap();
        assert_eq!(r.preedit_codetable, format!("hf{SEPS}kn"));
        // 对照：没切分时不得留下切分串，否则组合区会显示一个不对应任何候选的切法。
        let r2 = e.convert("aaaa", 50).unwrap();
        assert!(r2.preedit_codetable.is_empty());
    }

    /// 关闭时**零行为变化** —— 这是本功能能默认关着发布的全部依据。
    #[test]
    fn split_disabled_changes_nothing() {
        let e = split_engine(&[], CommitOptions::default());
        let r = e.convert("hfkn", 50).unwrap();
        assert!(
            r.candidates.is_empty() && r.is_empty,
            "关闭时满码空码仍是空"
        );
        assert!(r.preedit_codetable.is_empty());
    }

    /// 闸门只占 `== max_code_length` 这一格：未满码、超码长都不切。
    ///
    /// 超码长那一格归顶码与整句（两者的闸门都在 `>` 上）；未满码时还有更长后继可打，
    /// 切分等于替用户提前认定「这串到此为止」。
    #[test]
    fn split_only_at_exactly_full_length() {
        let e = split_engine(&[], split_opts());

        let short = e.convert("hfk", 50).unwrap();
        assert!(
            !short.candidates.iter().any(|c| c.is_split_composed),
            "3 码（未满码）不得切分，实际: {:?}",
            texts(&short)
        );

        let long = e.convert("hfknx", 50).unwrap();
        assert!(
            !long.candidates.iter().any(|c| c.is_split_composed),
            "5 码（超码长）不得切分，那是顶码与整句的区间，实际: {:?}",
            texts(&long)
        );
    }

    /// 两段缺一不产：切一半的结果只会让用户以为词库缺条目。
    #[test]
    fn split_needs_both_segments() {
        let e = split_engine(&[], split_opts());
        for input in ["hfzz", "zzkn", "zzyy"] {
            let r = e.convert(input, 50).unwrap();
            assert!(
                r.candidates.is_empty(),
                "{input}：有一段查不到就整体不产出，实际: {:?}",
                texts(&r)
            );
        }
    }

    /// 默认档（`Empty`）**不抢任何现有候选**：有精确解时一条切分候选都不出。
    #[test]
    fn split_empty_trigger_yields_to_existing_candidates() {
        let e = split_engine(&[], split_opts());
        let r = e.convert("aaaa", 50).unwrap();
        assert_eq!(texts(&r), vec!["工"], "有精确候选时默认档不切分");
    }

    /// `no_exact` 档：无精确解即切，但切分候选**排在既有候选之后**。
    ///
    /// 反向对照同在本用例里 —— 同一串码在默认档下必须一条都不产，
    /// 缺了它，一个「两档都照切」的实现同样能让上半段变绿。
    #[test]
    fn split_no_exact_trigger_appends_after_existing() {
        // `hfkn` 无精确解，但 `hfknq`（甲）是它的前缀扩展 ⇒ 有候选、无精确。
        let e = split_engine(
            &[PREFIX_ONLY_ENTRY],
            CommitOptions {
                split_trigger: SplitTrigger::NoExact,
                ..split_opts()
            },
        );
        let r = e.convert("hfkn", 50).unwrap();
        assert_eq!(
            texts(&r),
            vec!["甲", "很可能", "很可难"],
            "no_exact 档：切分候选 append 在既有候选之后"
        );

        let default_trigger = split_engine(&[PREFIX_ONLY_ENTRY], split_opts());
        let r2 = default_trigger.convert("hfkn", 50).unwrap();
        assert_eq!(
            texts(&r2),
            vec!["甲"],
            "★ 反向对照：默认档下有候选就不切，一条组合都不该出现"
        );
    }

    /// 原帖规则一/二：前后两段都唯一（或前段有重码但恒取首选）→ 适配四码唯一自动上屏。
    ///
    /// 靠的是既有 `decide_auto_commit` 的「恰一个 `code == input` 的候选」判据，
    /// 本功能**没有**为此新增任何上屏判据。
    #[test]
    fn split_unique_composition_triggers_auto_commit() {
        let e = split_engine(
            &[],
            CommitOptions {
                auto_commit_at_full: true,
                ..split_opts()
            },
        );
        let r = e.convert("xtup", 50).unwrap();
        assert!(r.should_commit, "xt(学) × up(双拼) 唯一 ⇒ 应自动上屏");
        assert_eq!(r.commit_text, "学双拼");
    }

    /// 原帖规则三前半：后段有重码 → 不自动上屏，列出候选让用户选。
    #[test]
    fn split_ambiguous_back_segment_blocks_auto_commit() {
        let e = split_engine(
            &[],
            CommitOptions {
                auto_commit_at_full: true,
                ..split_opts()
            },
        );
        let r = e.convert("hfkn", 50).unwrap();
        assert!(!r.should_commit, "后段 kn 有重码 ⇒ 不得自动上屏");
        assert_eq!(texts(&r), vec!["很可能", "很可难"]);
    }

    /// ★ 原帖规则三后半：「想要①就直接往后打」—— 第 5 个字母触发顶码，顶的是切分首选。
    ///
    /// 这条能成立是因为 `handle_top_code` 取首选走的是 `self.convert(&prefix, 1)`，
    /// 而 prefix 恰好是满码长 ⇒ 切分在那次 convert 里照常触发。**没有第四条上屏通路**。
    #[test]
    fn split_top_code_commits_first_composition() {
        use crate::engine::Engine;
        let e = split_engine(
            &[],
            CommitOptions {
                top_code_commit: true,
                ..split_opts()
            },
        );
        assert_eq!(
            e.handle_top_code("hfknx"),
            Some(("很可能".to_string(), "x".to_string())),
            "顶码须顶出切分首选，余码 x 返回续打"
        );
    }

    /// `split_front_candidates = 2`：前段也列举，得到原帖配图那种效果。
    ///
    /// 顺序是**前段外层、后段内层** —— 同前段的组合聚在一起，而不是按权重全局交错。
    #[test]
    fn split_front_candidates_expands_combinations() {
        let e = split_engine(
            &[("hf", "困", 300)],
            CommitOptions {
                split_front_candidates: 2,
                ..split_opts()
            },
        );
        let r = e.convert("hfkn", 50).unwrap();
        assert_eq!(
            texts(&r),
            vec!["很可能", "很可难", "困能", "困难"],
            "前段取 2 条时按「前段外层 × 后段内层」展开"
        );
    }

    /// 切点由 `max_code_length` 唯一决定，奇数/过短码长下功能整体关闭（构建期已告警）。
    #[test]
    fn split_disabled_on_unsupported_code_length() {
        let mut entries: Vec<(&str, &str, i32)> = SPLIT_ENTRIES.to_vec();
        entries.push(PREFIX_ONLY_ENTRY);
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(
            CachedDict::Memory(d),
            "codetable-system",
        )));
        // 码长 5：切点 5/2 无意义 ⇒ `new` 就地关掉。
        let e = CodeTableEngine::new(5, split_opts(), Arc::new(dm));
        let r = e.convert("hfknq", 50).unwrap();
        assert_eq!(
            texts(&r),
            vec!["甲"],
            "奇数码长下不得切分（只该有 hfknq 的精确解）"
        );
    }

    /// 逆切分与整句**占不同区间**，可以同开、互不干扰。
    #[test]
    fn split_and_sentence_cover_different_ranges() {
        let mut entries: Vec<(&str, &str, i32)> = SPLIT_ENTRIES.to_vec();
        entries.extend_from_slice(SENTENCE_ENTRIES);
        let e = engine_opts(
            &entries,
            CommitOptions {
                split_input: true,
                sentence_input: true,
                ..Default::default()
            },
        );

        // 4 码（== 码长）→ 逆切分
        let r4 = e.convert("hfkn", 50).unwrap();
        let c4 = r4.candidates.first().expect("4 码应有组合候选");
        assert!(c4.is_split_composed && !c4.is_sentence, "4 码那格归逆切分");
        assert_eq!(c4.text, "很可能");

        // 8 码（> 码长）→ 整句
        let r8 = e.convert("aawtaawt", 50).unwrap();
        let c8 = r8.candidates.first().expect("8 码应有整句候选");
        assert!(c8.is_sentence && !c8.is_split_composed, "超码长那格归整句");
        assert_eq!(c8.text, "工作工作");
    }
}
