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

/// 协调器向引擎要候选的上限的上限：翻页扩容（`expand_candidates`）翻倍到此为止，
/// 码表通配的硬上限（`codetable::engine::WILDCARD_RESULT_LIMIT`）与之是同一个值
/// （spec codetable-wildcard §11）。两处各写一个 5000 的话，改一处另一处静默失配：
/// 扩容要 8000、引擎只回 5000 ⇒ `has_more` 恒假。
pub const CANDIDATE_LIMIT_CAP: usize = 5000;

/// 引擎转换结果
#[derive(Debug, Clone, Default)]
pub struct ConvertResult {
    /// 候选列表（已按引擎内部权重排序，未应用运行时词频 boost）
    pub candidates: Vec<Candidate>,
    /// 预编辑显示文本（拼音：含音节分隔；码表：原始编码）
    pub preedit_display: String,
    /// 拼音音节拆分形态（供「混输高亮跟随」：高亮拼音候选时显示此拆分串，高亮码表候选时
    /// 显示原始码）。拼音引擎 = preedit_display；混输引擎 = 拼音子引擎的音节拆分（与原始输入
    /// 不同时给出，含单音节 + 尾部残码如 `nun'l`）；码表/无拼音引擎 = 空串。判据与
    /// `preedit_display` 的「≥2 完成音节」刻意不同，见 `MixedEngine::pinyin_split_of`。
    pub preedit_pinyin: String,
    /// 全拼降级形态：双拼方案下把击键串按全拼切分的串（`zaijian` → `zai'jian`）。
    ///
    /// 高亮到 [`Candidate::is_fullpinyin_fallback`] 的候选时显示它，其余仍按 `preedit_display`。
    /// 空串 = 无此形态。另立字段是因为前两个字段的语义已被混输高亮跟随占住，挤进去会连带改掉
    /// 混输行为。
    pub preedit_fullpinyin: String,
    /// 简拼分段形态：把击键串按简拼/混合简拼候选的真值音节序列切开（`wbwn` → `w'b'w'n`）。
    ///
    /// 高亮到 [`Candidate::is_abbrev`] 的候选时显示它。空串 = 没有可用的简拼候选。只有双拼
    /// 方案会填：全拼下简拼分段就是 `preedit_display` 本身，双拼则按两键一音节切，与简拼
    /// 「每键一个声母」是同一串击键的两种切法，须并存、由高亮决定。
    pub preedit_abbrev: String,
    /// 码表整句的编码单元切分（`aawtaawt` → `aawt'aawt`）。
    ///
    /// 高亮到码表整句候选时显示它，让用户看见码被切成了哪几段。空串 = 本次没有整句解。
    /// 不复用 `preedit_pinyin`：协调器按 `source == Pinyin` 才用那个，且混输下两者同时存在、
    /// 须由高亮候选各选各的。
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
    /// 满码空码清空的资格（码表 clear_on_empty_max：满码 + 无更长后继）。不含「无候选」——
    /// 候选要经协调器过滤后才是用户所见，由协调器按最终列表裁决是否真清空。
    pub should_clear: bool,
    /// 精确匹配空码补全的备选池（码表 `single_code_input` + `single_code_complete`）：
    /// 从更长编码取的候选，按引擎序排好，尚未计入 `candidates`。
    ///
    /// 「一条候选都没有」必须按最终显示列表判定，引擎看不见协调器随后叠加的短语，故引擎只备货、
    /// 由协调器收口（见 `build_candidates`）。是池不是单条：协调器在 shadow / 检索范围过滤之后
    /// 才择一，只备一条的话用户隐藏它后就无货可补。
    pub completion_hints: Vec<Candidate>,
    /// 候选调整（shadow）规则的归一编码；空串 = 无归一形态，消费方落回原始击键缓冲。
    ///
    /// shadow 存储键是 `"{schema}\0{code}"`，而全拼与双拼已折叠成同一个 schema，双拼 `hc` 与
    /// 全拼 `hao` 要落在同一个键上。只有引擎握着双拼转换结果，故由引擎给出（协调器重猜必错）。
    ///
    /// | 情形 | 取值 |
    /// |---|---|
    /// | 全拼 | 空串（恒等，含硬边界 `'`） |
    /// | 双拼·转换完整覆盖击键 | `full_pinyin`（`hc`→`hao`） |
    /// | 双拼·转换未覆盖整串 | 空串（混着未翻译字母，不是干净的全拼域） |
    ///
    /// 码表 / 混输 / 英文引擎恒为空串：击键即码位。取舍见 `PinyinEngine::convert` 末尾。
    pub shadow_code: String,
    /// 整句 N-best 的池子：按名次排好、已按文本去重的全部整句（名次 1..=K）。
    ///
    /// 候选列表只露前 `sentence_count` 条，余下供协调器的切换键滚动、不必重新解码。只在
    /// `sentence_max_count > 1` 时非空，协调器据此判定切换键吃不吃键。目前只有拼音填。
    pub sentence_pool: Vec<Candidate>,
}

/// [`ConvertOptions::admit`] 的候选准入判据。
///
/// 抽成别名是 `clippy::type_complexity` 的要求；`Arc` 而非 `Box` 是因为同一个判据要同时
/// 交给引擎和调用方的兜底过滤（见 `Coordinator::rare_admit_fn`），两侧共享同一份闭包。
pub type AdmitFn = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// 辅码表的只读视图，供击键坐标整句图加辅码边。
///
/// 引擎只要「某字是否有以 `prefix` 开头的辅码」，因此在引擎侧定义最小接口，由协调器
/// 用已加载的辅码表实现，引擎不依赖 `wind-aux-code`，输入路径上也不读盘。
pub trait AuxCharLookup: Send + Sync {
    /// `ch` 的任一辅码以 `prefix` 开头。
    fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool;

    /// 一个字都没有（未挂载）；引擎据此视同关闭。
    fn is_empty(&self) -> bool;

    /// `ch` 全部辅码首字母的掩码：bit i 置位 ⇔ 有以 `'a' + i` 开头的码（只看小写字母）。
    ///
    /// 建图时同一字要对 1 位、2 位辅码各判一次，按字取一次掩码就能先筛掉绝大多数字。
    /// 默认由 [`Self::any_code_starts_with`] 逐字母推出；实现可给零分配的快速版本（见
    /// [`aux_code_letter_bit`]），语义须与逐字母判定相同。
    fn letter_mask(&self, ch: char) -> u32 {
        let mut mask = 0;
        let mut buf = [0u8; 4];
        for (i, b) in (b'a'..=b'z').enumerate() {
            if self.any_code_starts_with(ch, char::from(b).encode_utf8(&mut buf)) {
                mask |= 1 << i;
            }
        }
        mask
    }
}

/// 一个辅码在 [`AuxCharLookup::letter_mask`] 里的位：首字节是小写字母时为该字母的位，否则 0。
pub fn aux_code_letter_bit(code: &str) -> u32 {
    match code.as_bytes().first() {
        Some(&b) if b.is_ascii_lowercase() => 1 << (b - b'a'),
        _ => 0,
    }
}

/// 调用方按路径给拼音引擎的取舍覆写（[`Engine::convert_with_opts`]）。
///
/// 做成参数而不是引擎配置：混输的码长内路径与超码长路径共用同一个拼音子引擎实例，而两者
/// 取舍相反。判据是「这串还可能是码表码吗」：
///
/// | | 码长内 | 超码长 |
/// |---|---|---|
/// | `require_full_match` | 随 `schema.mix.pinyin_partial_candidates`（出厂丢弃半截候选） | 随 `..._overflow`（出厂保留） |
/// | `allow_partial_final` | 强制 `false` | 强制 `true` |
#[derive(Clone, Default)]
pub struct ConvertOptions {
    /// 候选准入：在引擎产出候选的那一刻逐条判定，不合格的根本不进列表。
    /// `None` = 全部放行。
    ///
    /// 必须下推到引擎：引擎先 truncate(max_candidates)，调用方事后过滤只能在截断后的那段里筛。
    /// 但它买到的只是查重成本：词库层已按 `max_candidates` 取过 top-N，高频音节（`y`、`sh`）
    /// 的合格候选仍可能为 0，调用方仍需「不足则加大重取」（`Coordinator::refill_rare_if_short`）。
    ///
    /// Viterbi 整句走 `insert(0)` 绕过 `push_unique`，不受本判据约束，故调用方的最终过滤照留。
    /// 判据在按键热路径上逐条调用，实现必须便宜。
    pub admit: Option<AdmitFn>,
    /// 候选必须消费整串输入，否则在排序截断之前丢弃。
    ///
    /// 必须在拼音引擎内部过滤：简拼候选是 `cmp_match_layers` 最沉的一层，调用方事后 `retain`
    /// 时简拼词早在截断时就没了（`gedw` 下「各单位」会被 `ge` 的同音字压出配额）。
    pub require_full_match: bool,
    /// 本次转换不要简拼保底配额（见 `pinyin::truncate_with_quotas`）。
    ///
    /// 配额把简拼候选补在尾部、不保证有序，前提是调用方随后会重排（主输入与临拼走
    /// `candidate_display_order`）。不重排的调用方（如生僻字模式）拿到的是净损失：尾部被挤掉
    /// 的正是生僻字，补进来的简拼词又会被「只出单字」删掉。不能用 [`admit`](Self::admit)
    /// 代替：简拼候选另有两条 push 路径不经 `push_unique`。
    pub no_abbrev_quota: bool,
    /// 不做「纠错音节 + 简拼尾」的二次转换（纠错召回步内部递归用，防止替换后的串再触发一次）。
    pub no_correction_tail: bool,
    /// 覆写 [`crate::pinyin::Config::enable_partial_final`]（尾部残码参与整句解码，step 2c）。
    /// `None` = 不覆写，用引擎自身配置。
    ///
    /// 混输码长内要关（五笔码 `aaw` 会被读成「啊啊我」），超码长要开（否则 `zaiyebuj` 的 `j`
    /// 不参与组句）。判据是「这串还可能是码表码吗」，不是「是不是混输」。
    pub allow_partial_final: Option<bool>,
    /// 双拼走击键坐标整句图（内部开关，默认关；环境变量 `WIND_SP_RAW_LATTICE=1` 亦可打开，
    /// 仅内部调试与评测用）。
    pub sp_raw_lattice: bool,
    /// 连续辅助码用的辅码表；`None` 或 `is_empty()` = 关闭（默认）。
    pub aux_continuous: Option<std::sync::Arc<dyn AuxCharLookup>>,
    /// 编码栏里辅码前插的分隔符（带辅码候选的 `meta.preedit_split`）；`None` =
    /// [`AUX_PREEDIT_SEPARATOR`]。只在 `aux_continuous` 生效时用。
    pub aux_preedit_separator: Option<std::sync::Arc<str>>,
}

/// 编码栏辅码前的默认分隔符：`ni hn'hc`、串尾 `ni'hc hn`（同已发布的串尾直接辅助 `ui'du p`）。
/// 方案可经内部配置 `[engine.aux_code] preedit_separator` 改成空串或别的符号。
pub const AUX_PREEDIT_SEPARATOR: &str = " ";

/// 编码栏辅码分隔符的可选字符（白名单）：空格、`·`、`-`、`_`、`|`。
const AUX_PREEDIT_SEPARATOR_CHARS: &[char] = &[' ', '·', '-', '_', '|'];

/// 编码栏辅码分隔符是否可用：空串，或只由 [`AUX_PREEDIT_SEPARATOR_CHARS`] 组成。编码栏须是
/// 「缓冲按序插入分隔符」的形态，光标换算（逐字节贪心同步）才成立；字母数字、`;` 之类会与缓冲
/// 里的键混淆，`'` 会与音节分隔混淆，故用白名单。
pub fn aux_preedit_separator_valid(sep: &str) -> bool {
    sep.chars()
        .all(|c| AUX_PREEDIT_SEPARATOR_CHARS.contains(&c))
}

#[cfg(test)]
mod aux_separator_tests {
    #[test]
    fn aux_preedit_separator_whitelist() {
        for ok in ["", " ", "·", "-", "_", "|", " · "] {
            assert!(super::aux_preedit_separator_valid(ok), "{ok:?}");
        }
        for bad in ["'", ";", "x", "1", "/", "\t", " '"] {
            assert!(!super::aux_preedit_separator_valid(bad), "{bad:?}");
        }
    }
}

/// [`Engine::resolve_boundary`] 的结果。见 `docs/design/pinyin-entry-boundary-contract.md` §3.1。
///
/// 六个变体必须分开处置。尤其 `NoInfo`（合法，`boundary = 0` 照常入库）与 `Unresolvable`
/// （非法，拒收）：把前者当非法会拒掉所有码表词库，把后者当无信息则契约白做。`Ambiguous`
/// 与 `Derived` 只差可信度，都入库，分开是为了导入预览如实报数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryResolution {
    /// 词典点查到的真值。
    Exact(u64),
    /// 求解出的唯一解。
    Derived(u64),
    /// 约束筛完仍多解，已按读音权重择一。
    Ambiguous(u64),
    /// 合法、切分已定，但读音表验证不了：`text` 含无读音字符（符号 `←`、外文、单字表里没有的字），
    /// 而 `code` 切得出与字数相符的音节序列。
    ///
    /// 不能并回 `Unresolvable`：加词路径放行的 `zuo ←` 导出后再导入会被拒收
    /// （见 `user_words_api::normalize_add_code`）。读音表缺席，故不再单独记 `ambiguous`。
    NoReading(u64),
    /// 合法但边界无法表达：非拼音方案（码表码无音节语义），或码超 64 字节
    /// （bitmask 装不下，既定语义是整体降级，见 `wdict::split_spaced_code`）。
    NoInfo,
    /// 不合法：`code` 切不出与字数相符的音节序列。这是「拿错了文件」的判据，五笔码 `wgkq`
    /// 配单字「工」在此被拒。含无读音字符不落到这里（那是 `NoReading`）。
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

    /// 该词条是否应当入库。只有 `Unresolvable` 被拒。
    pub fn accepted(self) -> bool {
        !matches!(self, Self::Unresolvable)
    }

    /// 入库后是否仍缺边界信息（供导入预览统计「可补但没补上」的行）。
    pub fn lacks_boundary(self) -> bool {
        matches!(self, Self::NoInfo)
    }
}

/// 诊断用：一块可自报大小的内存结构（`docs/design/memory-footprint.md` S5 打点）。
///
/// `bytes` 是按容量算的堆字节（不含分配器每块的额外开销），`allocs` 是它占的堆分配块数——
/// 小块多的结构（逐条 `String`）在真实分配器上的占用明显高于 `bytes`，两者一起看。
#[derive(Debug, Clone, Default)]
pub struct MemPart {
    pub name: String,
    pub bytes: usize,
    pub allocs: usize,
}

/// 基础引擎接口
pub trait Engine: Send + Sync {
    /// 诊断：引擎内已建好的懒建结构及其自报大小（没建的不列）。只读，不触发构建。
    fn memory_parts(&self) -> Vec<MemPart> {
        Vec::new()
    }

    /// 转换输入为候选词列表
    fn convert(&self, input: &str, max_candidates: usize) -> anyhow::Result<ConvertResult>;

    /// 同 [`Self::convert`]，但按调用路径覆写两项取舍（见 [`ConvertOptions`]）。
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

    /// 为词语生成带空格的全拼音节码（`你好` → `ni hao`；造词反推读音、多音字消歧）。
    /// 默认不支持（码表/五笔等返回 None）；拼音引擎按词典权重消歧。含无读音字符时返回 None。
    ///
    /// 空格即音节边界，落库时由 `wind_store::wdict::split_spaced_code` 拆成扁平 code + boundary，
    /// 用户自造词从诞生起即有边界。
    fn generate_word_pinyin(&self, _word: &str) -> Option<String> {
        None
    }

    /// 反查某条已知 `(code, text)` 在词典里记录的音节边界；查不到或非拼音方案返回 0
    /// （= 无边界信息，消费方降级）。
    ///
    /// 与 [`Self::generate_word_pinyin`] 不同，不做推断，拿现成的码点查真值。词频列表显示音节
    /// 格式用它（词频表是唯一不带 boundary 的持久层）。
    fn syllable_boundary_of(&self, _code: &str, _text: &str) -> u64 {
        0
    }

    /// 为待入库的 `(code, text)` 求解音节边界，兼作拼音词条合法性判据。
    ///
    /// 与 [`Self::syllable_boundary_of`]（点查真值、不推断）、[`Self::generate_word_pinyin`]
    /// （从词反推读音）三者并存，勿合并：本方法读音由 code 给定，只定边界。
    ///
    /// 默认实现返回 [`BoundaryResolution::NoInfo`]，这对非拼音方案是正确语义；绝不可改成
    /// `Unresolvable`，那会让码表词库导入被整体拒收。
    fn resolve_boundary(&self, _code: &str, _text: &str) -> BoundaryResolution {
        BoundaryResolution::NoInfo
    }

    /// 运行时启停某扩展词库（按 dict id），无需重建引擎：直接翻 composite 中对应
    /// 系统层的 enabled 标志。返回是否命中该层。默认不支持（拼音等返回 false）；
    /// 码表/混输按 `codetable-extra-<id>` 层翻标志。用于扩展词库热插拔。
    fn set_dict_enabled(&self, _dict_id: &str, _enabled: bool) -> bool {
        false
    }

    /// 拼音「屏蔽这个词」的文字集合（候选调整里 `code = *` 的删除行）。只有拼音引擎认：
    /// 被屏蔽的文字不进整句图、整句恰为它时换下一条（见 `PinyinEngine`）。由调用方在规则
    /// 变化后推入，同一份集合（指针相同）重复推入是空操作。
    fn set_blocked_words(&self, _words: std::sync::Arc<std::collections::HashSet<String>>) {}

    /// 本引擎是否开启了整句输入（当前只有码表引擎会返回 true）。
    ///
    /// 协调器据此决定手动分隔符键是否放行：没开整句时那个键维持原语义（标点 / 选词）。
    fn sentence_input_enabled(&self) -> bool {
        false
    }

    /// 最大编码长度（码表引擎返回其码长；拼音等无意义返回 0）。
    /// 供混输引擎的超长分支（pinyin_only_overflow）与顶码裁决判断输入是否溢出。
    fn max_code_length(&self) -> usize {
        0
    }

    /// 码元字符集（码表引擎返回其配置集；拼音等无「码元」概念的引擎返回 `None`）。
    ///
    /// 供协调器判定一次按键是进输入缓冲还是作标点/选词/透传。`None` 须回落到字母累积，
    /// 不可当成空集（空集会让该方案一个字也打不出来）。见 `docs/design/codetable-input-chars.md`。
    fn input_chars(&self) -> Option<&wind_config::CodeCharSet> {
        None
    }

    /// 通配键（`[engine.codetable].wildcard_key`；开关关闭或键非法时 `None`）。
    /// 只有码表引擎返回 `Some`，混输代理主码表。见 `docs/design/codetable-wildcard.md`。
    fn wildcard_key(&self) -> Option<char> {
        None
    }

    /// 通配转换。`pattern` 是把作通配的那几位替换成 [`wind_dict::WILDCARD_SLOT`] 后的输入，
    /// 哪几位作通配由协调器裁决；`input` 是原始缓冲，只用于组合区显示。不支持通配的引擎返回 `None`。
    ///
    /// 与 [`Self::convert`] 分开：`convert` 还被当作「活码前缀」探针，那里的 `z` 必须是字面码元，
    /// 按内容自判会把 `zh` 当 `?h` 判活，`z_key_action` 的夺取静默失效。
    fn convert_wildcard(
        &self,
        _input: &str,
        _pattern: &str,
        _max_candidates: usize,
    ) -> Option<ConvertResult> {
        None
    }

    /// 反查模式内的通配键（reverse-mode spec §3）。只有码表引擎返回 `Some`，混输代理主码表。
    fn reverse_wildcard_key(&self) -> Option<char> {
        None
    }

    /// 影子层（未启用扩展词库，只给反查模式用；行内通配不读影子层）。只有挂了它的码表引擎返回 `Some`
    /// （开关开且方案有扩展库），混输代理主码表。管理器据此在后台预热，见
    /// `EngineManager::warm_disabled_dicts_async`。
    fn disabled_dict_layers(&self) -> Option<&crate::codetable::DisabledDictLayers> {
        None
    }

    /// 反查模式转换（reverse-mode spec §3）：与 [`Self::convert_wildcard`] 同一查询内核，
    /// 但不看 `wildcard` 主开关；`pattern` 由协调器把每个通配键位替换成 `WILDCARD_SLOT`。
    fn convert_reverse(
        &self,
        _input: &str,
        _pattern: &str,
        _max_candidates: usize,
    ) -> Option<ConvertResult> {
        None
    }

    /// 通配用的码长：协调器据此判「满码后通配键按字面」与混输「超码长整串字面」。
    /// 默认即 [`Self::max_code_length`]；混输返回主码表的码长。
    ///
    /// 与 `max_code_length` 分开：混输的 `max_code_length` 刻意为 0，代理过去会连带改变依赖
    /// 它的判据（见 `EngineManager::active_full_code_length`）。
    fn wildcard_code_length(&self) -> usize {
        self.max_code_length()
    }

    /// 通配与拼音共用这串输入（五笔拼音混输且有拼音子引擎）。协调器据此把首位、整串超码长、
    /// 拼音分段续转三种情形判为字面（spec §10）。
    fn wildcard_mixes_pinyin(&self) -> bool {
        false
    }

    /// 候选排序是否忽略权重（`[engine.codetable].base_sort = "natural"`）：码表引擎在 natural
    /// 模式下返回 true。供协调器合并短语后按同一维度重排——否则协调器仍以 weight 优先，会与
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
    /// 只取数、不施加呈现策略：「精确匹配模式只展示一条」由 [`Self::browse_display_limit`]
    /// 声明、由调用方在过滤之后施加，否则用户隐藏那一条后整屏空白。
    fn enumerate(&self, _limit: usize) -> Vec<Candidate> {
        Vec::new()
    }

    /// 「进入即展示」浏览态的呈现上限：`Some(n)` = 过滤后最多显示 n 条；`None` = 不限。
    /// 码表引擎在精确匹配模式（`single_code_input`）下返回 `Some(1)`，语义与空码补全的
    /// 「取首位后续码」一致。调用方须在 shadow/过滤之后才施加它。
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

    /// 双拼下 `keys` 恰好切成完整音节时返回音节数；非双拼、切不完整（含 `'`、尾部落单键、
    /// 不成音节的键对）返回 `None`。直接辅助码的前缀闸门（`docs/design/aux-code-direct.md` §4）。
    /// 双拼拼音引擎实现；其余（含混输）默认 `None`。
    fn shuangpin_full_syllable_count(&self, _keys: &str) -> Option<usize> {
        None
    }

    /// 把一个标准拼音音节按当前模糊音配置展开成等价读音（`fang` → `fan`、`zi` → `zhi`）。
    /// 没开模糊音 / 非拼音引擎 → 空。只返回标准音节表里真实存在的读音（纠错用的错误拼写不算）。
    /// 手机端展开页「首单元读法」列模糊兄弟用。
    fn fuzzy_siblings(&self, _syllable: &str) -> Vec<String> {
        Vec::new()
    }

    /// 双拼下把击键串按击键对拆成音节：`(全拼音节, 击键起, 击键止)`（击键域字节位，含 `'`）。
    /// 非双拼 / 拆不出 → 空。手机端展开页「首单元读法」在双拼下用它定首单元，
    /// 全拼的拆分串在双拼下是击键切分，不是拼音。
    fn shuangpin_units(&self, _keys: &str) -> Vec<(String, usize, usize)> {
        Vec::new()
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

#[cfg(test)]
mod aux_char_lookup_tests {
    use super::*;
    use std::sync::Arc;

    struct FakeAux(Vec<(char, &'static str)>);

    impl AuxCharLookup for FakeAux {
        fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
            self.0
                .iter()
                .any(|(c, code)| *c == ch && code.starts_with(prefix))
        }
        fn is_empty(&self) -> bool {
            self.0.is_empty()
        }
    }

    #[test]
    fn default_opts_have_no_aux_continuous() {
        assert!(ConvertOptions::default().aux_continuous.is_none());
    }

    #[test]
    fn fake_aux_satisfies_trait_through_arc_dyn() {
        let aux: Arc<dyn AuxCharLookup> = Arc::new(FakeAux(vec![('好', "hn"), ('是', "uu")]));
        assert!(aux.any_code_starts_with('好', "h"));
        assert!(aux.any_code_starts_with('好', "hn"));
        assert!(!aux.any_code_starts_with('好', "u"));
        assert!(!aux.any_code_starts_with('无', "h"));
        assert!(!aux.is_empty());
        assert!(FakeAux(vec![]).is_empty());
        let opts = ConvertOptions {
            aux_continuous: Some(aux),
            ..Default::default()
        };
        assert!(opts.clone().aux_continuous.is_some());
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
