//! 引擎接口定义
//!
//! 与 Go 版本 `wind_input/internal/engine/engine.go` 对齐。

use wind_candidate::Candidate;

/// 引擎类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineType {
    Pinyin,
    CodeTable,
    Mixed,
    /// 英文引擎（复用码表查询，独立类型便于英文专属路由/演化）。
    English,
}

/// 引擎转换结果
#[derive(Debug, Clone, Default)]
pub struct ConvertResult {
    /// 候选列表（已按引擎内部权重排序，未应用运行时词频 boost）
    pub candidates: Vec<Candidate>,
    /// 预编辑显示文本（拼音：含音节分隔；码表：原始编码）
    pub preedit_display: String,
    /// 拼音音节拆分形态（供「混输高亮跟随」：高亮拼音候选时显示此拆分串，高亮码表/五笔
    /// 候选时显示原始码）。拼音引擎 = preedit_display；混输引擎 = 拼音子引擎的音节拆分
    /// （**拆分串与原始输入不同时**给出，含单音节 + 尾部残码如 `nun'l`）；码表/无拼音引擎 =
    /// 空串（恒原始码）。判据与 `preedit_display` 的「≥2 完成音节」刻意不同，见
    /// `MixedEngine::pinyin_split_of`。
    pub preedit_pinyin: String,
    /// **全拼降级形态**：双拼方案下把击键串按全拼切分的串（`zaijian` → `zai'jian`）。
    ///
    /// 供协调器的高亮跟随：高亮到 [`Candidate::is_fullpinyin_fallback`] 的候选时显示它，
    /// 其余情形仍按 `preedit_display`（双拼自己的切分 `za'ij'ia'n`）。空串 = 无此形态
    /// （非双拼方案 / 开关关 / 闸门未放行）。
    ///
    /// ⚠️ **另立字段而非复用上面两个**：`preedit_display` 与 `preedit_pinyin` 的语义已被
    /// 混输的高亮跟随占住（拆分串 vs 原始码），双拼在它们之上还需要第三种形态——同一串
    /// 击键的**两种切法**。挤进老字段会连带改掉混输行为，那是两条无关的路径。
    pub preedit_fullpinyin: String,
    /// **简拼分段形态**：把击键串按简拼/混合简拼候选的真值音节序列切开（`wbwn` → `w'b'w'n`）。
    ///
    /// 供协调器的高亮跟随：高亮到 [`Candidate::is_abbrev`] 的候选时显示它。空串 = 本次转换
    /// 没有可用的简拼候选（或切不出与击键等长的串）。
    ///
    /// ⚠️ **只有双拼方案会填**。全拼下简拼分段已经就是 `preedit_display` 本身（那里的首选
    /// 一旦是简拼候选，`convert` 就地把它切好了），再填一份等于给协调器加一条恒等分支。
    /// 双拼则不同：`build_raw_preedit` 按**两键一音节**切，与简拼「每键一个声母」是同一串
    /// 击键的两种切法（`wbwn` 双拼切成 `wbwn`／`wf'wt`，简拼该切成 `w'b'w'n`／`w'f'w't`），
    /// 二者必须并存、由高亮决定用哪个——同 `preedit_fullpinyin` 的理由。
    pub preedit_abbrev: String,
    /// **码表整句的编码单元切分**（`aawtaawt` → `aawt'aawt`）。
    ///
    /// 供协调器的高亮跟随：高亮到码表整句候选时显示它，让用户看见引擎把这串码切成了
    /// 哪几段——不然一长串码配一句话，切错了也无从判断错在哪一段。
    /// 空串 = 本次没有整句解（或未开启整句）。
    ///
    /// ⚠️ **另立字段而非复用 `preedit_pinyin`**：那个的语义是「拼音音节拆分」，
    /// 协调器按 `source == Pinyin` 决定要不要用它，码表候选走不到那条分支；
    /// 且混输方案下两者会同时存在（拼音子引擎给音节切分、码表给编码单元切分），
    /// 必须由高亮候选各选各的。同 `preedit_fullpinyin` 另立的理由。
    pub preedit_codetable: String,
    /// 已完成音节（拼音 UI 高亮用）
    pub completed_syllables: Vec<String>,
    /// 末尾未完成音节（拼音）
    pub partial_syllable: String,
    /// 是否存在未完成音节
    pub has_partial: bool,
    /// 是否应自动上屏（码表满码等）
    pub should_commit: bool,
    /// 自动上屏的文本
    pub commit_text: String,
    /// 是否为空码（有输入但无候选）
    pub is_empty: bool,
    /// 满码空码时是否应清空缓冲（码表 clear_on_empty_max）
    pub should_clear: bool,
    /// 精确匹配空码补全的**备选池**（码表 `single_code_input` + `single_code_complete`）：
    /// 从更长编码取的候选，按引擎序排好，**尚未**计入 `candidates`。
    ///
    /// 补全的语义是「一条候选都没有时的兜底」，而这个「没有」必须按**最终显示列表**判定。
    /// 引擎只看得见自己这一层，看不见协调器随后叠加的短语，就地判空会在「短语已有候选」时
    /// 多冒一条无关的后续编码；反过来，引擎抢先填非空又会把协调器的短语前缀补全误压制。
    /// 故引擎只备货，采纳与否由掌握最终列表的协调器统一收口（见 `build_candidates`）。
    ///
    /// ⚠️ 是**池**不是单条：协调器要在 shadow / 检索范围过滤**之后**才择一。只备一条的话，
    /// 用户把它隐藏掉就无货可补、屏幕全空——而词库里其实还有下一条。同「从池中择 N 条
    /// 必须发生在过滤之后」，见 `Engine::browse_display_limit` 的同款教训。
    pub completion_hints: Vec<Candidate>,
    /// **候选调整（shadow）规则的归一编码**；空串 = 无归一形态，消费方落回原始击键缓冲。
    ///
    /// 存在的理由：shadow 的存储键是 `"{schema}\0{code}"`，而 `EngineManager::data_schema_id`
    /// 已把全拼与双拼折叠成同一个 schema（常量 `"pinyin"`）。若 `code` 继续取击键，
    /// 双拼的 `hc` 与全拼的 `hao` 明明是同一个音、却落成两个互不相认的键；反过来若无脑
    /// 归一，又会把候选集不同的输入并到一起。故由**引擎**给出这个域——只有引擎握着双拼
    /// 转换结果，协调器拿着击键串重猜必然出错（同 `generate_word_pinyin` 的
    /// 「已知真值不要重算」）。
    ///
    /// 填充口径（拼音引擎，见 `PinyinEngine::convert` 末尾，判据取舍与被否决的两个备选
    /// 详见那里）：
    ///
    /// | 情形 | 取值 | 理由 |
    /// |---|---|---|
    /// | 全拼 | 空串 | 恒等，存量规则零迁移、行为零变更（含手动分隔符 `'`，它是硬边界不可剥） |
    /// | 双拼·转换完整覆盖击键 | `full_pinyin` | `hc`→`hao`，与全拼共享一条规则 |
    /// | 双拼·转换未覆盖整串 | 空串 | `full_pinyin` 里混着未翻译的原始字母，不是干净的全拼域 |
    ///
    /// 与全拼降级支路（`allow_full_pinyin`）相容：那条支路的闸门要求击键串本身能切出
    /// ≥2 个完整音节，而这类串在双拼下多与击键同形（`nihao` 解释成 ni|ha|o 拼回去仍是
    /// `nihao`），归一码于是恰好等于击键串、与全拼方案落在同一个 key 上。
    ///
    /// 码表 / 混输 / 英文引擎恒为空串：它们的击键即码位，本就没有第二个域。
    pub shadow_code: String,
}

/// 调用方按**路径**给拼音引擎的取舍覆写（[`Engine::convert_with_opts`]）。
///
/// ## ⚠️ 为什么是参数而不是引擎配置
///
/// 混输的**主路径（码长内）与超码长路径共用同一个拼音子引擎实例**，而两者的取舍恰好相反。
/// 做成构造期配置就只能让一个实例表现出两种行为——那是必然出错的形态。
///
/// | | 码长内 | 超码长 |
/// |---|---|---|
/// | `require_full_match` | 随 `schema.mix.pinyin_partial_candidates`（出厂丢弃半截候选） | 随 `..._overflow`（出厂保留） |
/// | `allow_partial_final` | 强制 `false` | 强制 `true` |
///
/// 判据是**这串还可能是码表码吗**：定长码表（五笔 4 码）之外的串不可能是码，那里已是纯拼音
/// 语境，两项都该按纯拼音的规矩来。
#[derive(Clone, Default)]
pub struct ConvertOptions {
    /// **候选准入**：在引擎产出候选的那一刻逐条判定，不合格的根本不进列表。
    /// `None` = 全部放行（默认，与本字段引入前逐条一致）。
    ///
    /// # 为什么必须下推到引擎里，而不是拿到结果再 `retain`
    ///
    /// 与本结构体 [`require_full_match`](Self::require_full_match) 那条是同一个道理，
    /// 只是那里过滤的是「没消费整串的候选」，这里由调用方指定：**引擎是「产生 N 条 →
    /// 排序 → truncate(max_candidates)」，上限施加在过滤之前**。调用方事后过滤等于
    /// 在被截断过的那一段里筛——过滤再准也提不了不在场的候选。
    ///
    /// 实测（生僻字模式，拼音 `yi`）：事后过滤只剩 **4** 条，而该音实际有 1183 个
    /// 非常用字；把上限从 100 提到 1000 能取到，但单次 `convert` 从 3.5ms 涨到 29ms
    /// ——因为 `push_unique` 是 O(n²) 线性查重，而那 1000 条里绝大多数注定被丢弃。
    ///
    /// # ⚠️ 它买到的是查重成本，**不是**「配额全给合格候选」
    ///
    /// 判定发生在 `push_unique`，而词库层**已经按 `max_candidates` 取过一轮 top-N** 了
    /// ——被滤掉的名额不会被补回来。实测（下推后，拼音，`limit=100`）：
    ///
    /// | 输入 | 合格候选数 |
    /// |---|---|
    /// | `y` | **0** |
    /// | `sh` | **0** |
    /// | `n` | 3 |
    /// | `ni` | 100（装满了）|
    ///
    /// 高频音节的前 100 名全是常用字，一个都留不下。⇒ **调用方仍需「不足则加大重取」**
    /// （见 `Coordinator::refill_rare_if_short`），本字段只是让那次重取便宜得多：
    /// `y` 的端到端耗时由 148.7ms 降到 50.0ms。
    ///
    /// 要真正做到「配额全给合格候选」，得把判定再下推一层到词库查询（`dm.search`），
    /// 让它返回 N 条**合格**结果。那是 wind-dict 的改动，未做。
    ///
    /// ⚠️ **Viterbi 整句走 `insert(0)` 绕过 `push_unique`，不受本判据约束**。这不是疏漏：
    /// 整句是多字候选，任何「只要单字」类的准入都会在调用方那一侧再滤一次。
    /// ⇒ 本字段是**性能与覆盖面**的手段，不是正确性的唯一保证；调用方该有的最终过滤照留。
    ///
    /// ⚠️ 判据函数在**每条候选**上调用，且在按键热路径内——实现必须便宜。
    /// 现有调用方（`rare_admits`）是两次表查询 + 一次位运算。
    pub admit: Option<std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    /// 候选必须**消费整串输入**，否则在排序截断**之前**丢弃。
    ///
    /// ## ⚠️ 过滤必须发生在拼音引擎内部（截断之前）
    ///
    /// 拼音引擎是「一次性产生 N 条 + 排序 + `truncate(max_candidates)`」，简拼候选在
    /// `cmp_match_layers` 里是最沉的一层、恒排在部分匹配候选之后。若改由调用方拿到结果再
    /// `retain`，同音字堆满配额时**简拼词早在截断时就没了**——过滤再准也提不了不在场的候选
    /// （同 `MixedEngine::truncate_with_pinyin_quota` 的既有教训）。
    /// 实测 `gedw`：`ge` 的残码同音字 219 条，把混合简拼「各单位」压到第 221 位。
    pub require_full_match: bool,
    /// 覆写 [`crate::pinyin::Config::enable_partial_final`]（尾部残码参与整句解码，step 2c）。
    /// `None` = 不覆写，用引擎自身配置。
    ///
    /// 混输把该配置整体关掉过，理由是五笔码 `aaw`（本意 `aawt`→工作）会被读成「啊啊我」
    /// 抢首位。但**那个理由只在码长内成立**：超过码表最大码长的串不可能是码表码，关掉它的
    /// 代价是 `zaiyebuj` 的尾字母 `j` 不参与组句、打不出「在也不就」（纯拼音方案一直打得出）。
    /// ⇒ 判据从「是不是混输」改为「这串还可能是码表码吗」。
    pub allow_partial_final: Option<bool>,
}

/// [`Engine::resolve_boundary`] 的结果。见
/// `docs/design/pinyin-entry-boundary-contract.md` §3.1。
///
/// ★★ 六个变体**必须分开处置**，合并任意两个都会出错。最容易合并错的是最后两个：
/// `NoInfo` 与 `Unresolvable` 都给不出边界，但前者**合法**（照常入库，`boundary = 0`
/// 走既有降级路径），后者**非法**（拒收）。把 `NoInfo` 当非法会拒掉所有码表词库；
/// 把 `Unresolvable` 当无信息则等于这套契约白做。
///
/// `Ambiguous` 与 `Derived` 的区别只在可信度，两者都应入库——分开是为了让导入预览能
/// 如实报数，不是为了拦截。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryResolution {
    /// 词典点查到的真值。
    Exact(u64),
    /// 求解出的唯一解。
    Derived(u64),
    /// 约束筛完仍多解，已按读音权重择一。
    Ambiguous(u64),
    /// **合法、切分已定，但读音表验证不了**：`text` 含无读音字符（符号 `←`、外文、
    /// 词典单字表里没有的字），而 `code` 切得出与字数相符的音节序列。
    ///
    /// ★ 这一档是从 `Unresolvable` 里**拆出来**的。合在一起时，用户在设置页手动添加的
    /// `zuo ←`（加词路径有意放行，见 `webdata::normalize_add_code`）导出后再导入会被
    /// 判非法丢弃——同一条词条，加词放行、导出放行、导入拒收。见 issue #97。
    ///
    /// ⚠️ **不再单独记 `ambiguous`**：那个变体的语义是「多解，已按读音权重择一」，而
    /// 读音表在这里整体缺席，压根没有权重可择。两者不是正交维度，是同一维度的互斥取值。
    NoReading(u64),
    /// **合法但边界无法表达**：非拼音方案（码表码无音节语义），或码超 64 字节
    /// （bitmask 装不下，既定语义是整体降级，见 `wdict::split_spaced_code`）。
    NoInfo,
    /// **不合法**：`code` 切不出与字数相符的音节序列。这是「拿错了文件」的判据——
    /// 五笔码 `wgkq` 配单字「工」、`aaaa` 配「田」都在此被拒。
    ///
    /// ⚠️ 「含无读音字符」**不再落到这里**（那是 `NoReading`）：拦截误导入的力量全部
    /// 来自「切不出音节序列」这一条，`text` 是不是汉字与文件选没选错无关。
    Unresolvable,
}

impl BoundaryResolution {
    /// 取边界值；`NoInfo` / `Unresolvable` 均为 0。
    pub fn boundary(self) -> u64 {
        match self {
            Self::Exact(b) | Self::Derived(b) | Self::Ambiguous(b) | Self::NoReading(b) => b,
            Self::NoInfo | Self::Unresolvable => 0,
        }
    }

    /// 该词条是否应当入库。**只有 `Unresolvable` 被拒**。
    pub fn accepted(self) -> bool {
        !matches!(self, Self::Unresolvable)
    }

    /// 入库后是否仍缺边界信息（供导入预览统计「可补但没补上」的行）。
    pub fn lacks_boundary(self) -> bool {
        matches!(self, Self::NoInfo)
    }
}

/// 基础引擎接口
pub trait Engine: Send + Sync {
    /// 转换输入为候选词列表
    fn convert(&self, input: &str, max_candidates: usize) -> anyhow::Result<ConvertResult>;

    /// 同 [`Self::convert`]，但按**调用路径**覆写两项取舍（见 [`ConvertOptions`]）。
    ///
    /// 只有拼音引擎实现它；其余引擎的候选恒不带 `consumed_length`（视作消费整串）、
    /// 也没有整句解码，默认转发到 `convert` 即为正确语义。
    fn convert_with_opts(
        &self,
        input: &str,
        max_candidates: usize,
        _opts: ConvertOptions,
    ) -> anyhow::Result<ConvertResult> {
        self.convert(input, max_candidates)
    }

    /// 重置引擎状态
    fn reset(&self);

    /// 引擎类型
    fn engine_type(&self) -> EngineType;

    /// 顶码上屏：超过满码长时取前 N 码首选上屏，返回 (上屏文本, 剩余编码)。
    /// 默认不支持（拼音等）；码表/混输引擎按 schema 的 top_code_commit 实现。
    fn handle_top_code(&self, _input: &str) -> Option<(String, String)> {
        None
    }

    /// 为词语生成**带空格的全拼音节码**（`你好` → `ni hao`；造词反推读音、多音字消歧）。
    /// 默认不支持（码表/五笔等返回 None）；拼音引擎按词典权重消歧。
    /// 用于加词页自动出码、词库导入。含无读音字符时返回 None。
    ///
    /// 空格即音节边界，与 rime 源词库同形。落库时由
    /// `wind_store::wdict::split_spaced_code` 拆成扁平 code + boundary
    /// （见 `wind_dict::binformat::DictEntry::boundary`）——造词本就逐音节拼接、边界白送，
    /// 带出来使用户自造词从诞生起即有边界，否则用户词是块边界空洞、双拼校验只能对其降级。
    fn generate_word_pinyin(&self, _word: &str) -> Option<String> {
        None
    }

    /// 反查某条已知 `(code, text)` 在词典里记录的音节边界；查不到或非拼音方案返回 0
    /// （= 无边界信息，消费方降级）。
    ///
    /// 与 [`Self::generate_word_pinyin`] 的区别是**不做推断**：那个从词反推读音、多音字
    /// 靠权重消歧，可能给出与目标条目不同的码；这里是拿现成的码去词典点查、取真值边界。
    /// 词频列表要显示音节格式，用的正是这条——词频记录只有 `(code, text)`，没有边界
    /// （词频表是唯一不带 boundary 的持久层）。
    fn syllable_boundary_of(&self, _code: &str, _text: &str) -> u64 {
        0
    }

    /// 为待入库的 `(code, text)` **求解**音节边界，兼作拼音词条合法性判据。
    ///
    /// 与相邻两者的分工（三者必须并存，勿合并）：
    /// - [`Self::syllable_boundary_of`]：**点查**词典真值，查不到即 0，不推断。
    /// - [`Self::generate_word_pinyin`]：从**词**反推读音（多音字靠权重猜）。
    /// - 本方法：拿着 `(code, text)` **求解切分**——读音已由 code 给定，只需定边界。
    ///
    /// 默认实现返回 [`BoundaryResolution::NoInfo`]。★ 这对码表/五笔等非拼音方案是**正确
    /// 语义**（码表词组码没有音节概念，`boundary = 0` 本就合法），⚠️ 绝不可改成
    /// `Unresolvable`——那会让码表词库导入被整体拒收。
    fn resolve_boundary(&self, _code: &str, _text: &str) -> BoundaryResolution {
        BoundaryResolution::NoInfo
    }

    /// 运行时启停某扩展词库（按 dict id），**无需重建引擎**：直接翻 composite 中对应
    /// 系统层的 enabled 标志。返回是否命中该层。默认不支持（拼音等返回 false）；
    /// 码表/混输按 `codetable-extra-<id>` 层翻标志。用于扩展词库热插拔。
    fn set_dict_enabled(&self, _dict_id: &str, _enabled: bool) -> bool {
        false
    }

    /// 最大编码长度（码表引擎返回其码长；拼音等无意义返回 0）。
    /// 供混输引擎的超长分支（pinyin_only_overflow）与顶码裁决判断输入是否溢出。
    /// 本引擎是否开启了**整句输入**（当前只有码表引擎会返回 true）。
    ///
    /// 协调器据此决定手动分隔符键是否放行 —— 分隔符是整句的消歧手段，
    /// 没开整句时那个键该维持它原本的语义（标点 / 选词）。
    fn sentence_input_enabled(&self) -> bool {
        false
    }

    fn max_code_length(&self) -> usize {
        0
    }

    /// 码元字符集（码表引擎返回其配置集；拼音等无「码元」概念的引擎返回 `None`）。
    ///
    /// 供协调器判定一次按键是进输入缓冲还是作标点/选词/透传。`None` 表示该引擎不参与
    /// 这套判定，调用方须回落到历史行为（字母累积），**不可当成空集**——空集会让该
    /// 方案一个字也打不出来。见 `docs/design/codetable-input-chars.md`。
    fn input_chars(&self) -> Option<&wind_config::CodeCharSet> {
        None
    }

    /// 候选排序是否**忽略权重**（`[engine.codetable].base_sort = "natural"`）：码表引擎在 natural
    /// 模式下返回 true。供协调器合并短语后按**同一维度**重排——否则协调器仍以 weight 优先，会与
    /// 引擎的 `candidate::by_natural`（纯 base_order→natural_order、忽略权重）发散。其余引擎默认
    /// false（按权重排，对齐 `candidate::better`）。
    fn base_sort_ignores_weight(&self) -> bool {
        false
    }

    /// `input` 是否存在精确（code==input）匹配（码表引擎实现；其余默认 false）。
    fn has_full_input_match(&self, _input: &str) -> bool {
        false
    }

    /// 是否存在比 `input` 更长的后继编码（码表引擎实现；其余默认 false）。
    fn has_longer_code(&self, _input: &str) -> bool {
        false
    }

    /// 空码枚举：列出词典首 `limit` 条候选（按引擎内部序），供特殊模式「进入即展示」浏览。
    /// 码表引擎返回其码表首页（按 weight 降序）；拼音等无浏览语义的引擎返回空。
    ///
    /// ⚠️ **这里只取数、不施加呈现策略**。「精确匹配模式只展示一条」由
    /// [`Self::browse_display_limit`] 声明、**由调用方在过滤之后**施加——早年在此直接按
    /// `single_code_input` 截到 1 条，结果用户隐藏掉那一条后整屏空白（截断发生在候选调整
    /// 之前，池子里明明还有下一条）。取数与截断之间隔着过滤，两者不能揉在一处。
    fn enumerate(&self, _limit: usize) -> Vec<Candidate> {
        Vec::new()
    }

    /// 「进入即展示」浏览态的**呈现上限**：`Some(n)` = 过滤后最多显示 n 条；`None` = 不限。
    /// 码表引擎在精确匹配模式（`single_code_input`）下返回 `Some(1)`，语义与空码补全的
    /// 「取首位后续码」一致。调用方须在 shadow/过滤**之后**才施加它。
    fn browse_display_limit(&self) -> Option<usize> {
        None
    }

    /// 前缀是否构成「合法拼音序列」（含残缺尾音节前缀，用于保护正在输入的拼音）。
    /// 拼音引擎实现（对齐 Go isPossiblePinyinSequence）；其余默认 false。
    fn is_possible_pinyin_sequence(&self, _prefix: &str) -> bool {
        false
    }

    /// 前缀是否「恰好由完整拼音音节构成」（切在音节边界、无残缺尾音节）。
    /// 拼音引擎实现（对齐 Go isWholeSyllablePinyin）；其余默认 false。
    fn is_whole_syllable_pinyin(&self, _prefix: &str) -> bool {
        false
    }

    /// 前缀的连续完整音节解析中是否存在「非首位单字母音节」（a/e/o，退化解析特征）。
    /// 拼音引擎实现（对齐 Go hasNonInitialSingleLetterSyllable）；其余默认 false。
    fn has_non_initial_single_letter_syllable(&self, _prefix: &str) -> bool {
        false
    }

    /// 前缀从起始连续解析出的完整拼音音节数（拼音引擎实现；其余默认 0）。
    /// 拼音词否决用：前缀恰 1 个完整音节（如 wang）多为「正在打拼音词的中途」→ 保护拼音；
    /// ≥2 音节（如 aipu=ai+pu）已是完整多音节单元 → 多为恰好像拼音的五笔码。
    fn completed_syllable_count(&self, _prefix: &str) -> usize {
        0
    }

    /// 满码自动上屏「显示态」复评（对齐 Go recheckAutoCommit）：给定已过滤/重排/shadow 的
    /// 显示候选，若满码上屏开、存在唯一精确全码码表候选且无更长后继 → 返回上屏文本。
    /// 引擎按未过滤候选判唯一时可能因生僻同码字被否决，智能过滤后据显示候选复评放行。
    /// 码表/混输引擎实现；其余默认 None。
    fn recheck_auto_commit(&self, _input: &str, _candidates: &[Candidate]) -> Option<String> {
        None
    }
}

/// 扩展引擎接口（码表引擎特有）
pub trait ExtendedEngine: Engine {
    /// 获取最大编码长度
    fn max_code_length(&self) -> usize;

    /// 判断是否应自动上屏
    fn should_auto_commit(&self, input: &str, candidates: &[Candidate]) -> Option<String>;

    /// 处理空编码
    fn handle_empty_code(&self, input: &str) -> (bool, bool, String);
}
