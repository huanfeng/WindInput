//! 输入方案定义（统一富 Schema）
//!
//! 与 Go 版本 `wind_input/internal/schema/` 对齐，但合理精简：只保留实际有意义的字段，
//! tri-state 用 `Option<bool>`（区分"未设置/false"），剔除仅为临时兼容的遗留。
//!
//! 本类型是**唯一**的方案表示——取代 wind-engine 早期私有的 `SchemaFile`。
//! 字段对齐真实 `data/schemas/*.schema.toml`（码表/拼音/混输/双拼）。

use serde::{Deserialize, Serialize};

/// 完整方案定义
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Schema {
    #[serde(default)]
    pub schema: SchemaInfo,
    #[serde(default)]
    pub engine: EngineSpec,
    #[serde(default)]
    pub dictionaries: Vec<DictSpec>,
    /// 扩展词库的**发现目录**（`[[dictionary_dirs]]`）：目录里的词库文件在读方案时自动登记为
    /// 扩展库（出厂关，由用户在「方案设置 → 扩展词库」里开），不必改方案文件。
    /// 注入发生在 `EngineManager::read_schema`，见 docs/design/schema-dict-discovery.md。
    #[serde(default)]
    pub dictionary_dirs: Vec<DictDirSpec>,
    /// 方案级词库权重归一化（`[weight_spec]`）。`None` = 不归一化（默认）。
    ///
    /// 方案级而非按词库：全方案共用一个单调映射才能保住库间相对顺序。主要给 Rime 生态导入的
    /// 未归一化词频方案用；本仓自产方案不配，拼音方案不要配（拼音若干阈值按原始权重标定，归一化
    /// 后会静默失效）。依据见 `docs/design/dict-weight-normalization.md`。参数由
    /// `wind_input dict weight-check` 算出。
    #[serde(default)]
    pub weight_spec: Option<WeightSpec>,
    #[serde(default)]
    pub encoder: Option<EncoderSpec>,
    /// 方案级按键功能表（`[key_actions]`）：按键名 → 动词。与引擎类型无关，故与 `[engine]` 平级。
    /// 值域见 [`crate::BoundAction`] 与 `docs/design/schema-key-actions.md`。
    ///
    /// 逐键合并：`merge_toml` 表达不了删除，「本方案禁用某个全局绑定」须写显式 `"none"`。
    #[serde(default)]
    pub key_actions: std::collections::BTreeMap<String, String>,
    /// 方案级会话态按键表（`[session_actions]`）：按键名 → 动词，只在有会话时生效（与
    /// [`Self::key_actions`] 到达条件不同，见 `docs/design/session-key-actions.md` §2）。
    /// 值域见 [`crate::SessionAction`]。逐键合并，禁用全局绑定须写显式 `"none"`。
    ///
    /// 本表参与可达性并集：所有方案绑过的键都让 TSF 转发，其中可打印符号键带 `FORWARD_ONLY`，
    /// 无会话时必须放行给下游（见 `docs/design/key-resolver-unification.md` §8 注意点 5）。
    #[serde(default)]
    pub session_actions: std::collections::BTreeMap<String, String>,
    /// overlay 激活面（`[overlay]`）：本方案被引导键/直达热键叠加激活时的呈现配置。
    ///
    /// 段存在即声明「我是 overlay 方案」，也是 `EngineManager::overlay_modes` 的枚举依据。
    /// 不能复用 `[schema] hidden`：那是展示属性，这是行为属性，两者互不蕴含。
    /// 见 `docs/redesign/overlay-mode-config.md`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<OverlaySpec>,
    /// 方案级标点（`[punct]` 段）：标点态意图 `mode` + 自定义映射表 `custom_mappings`。
    ///
    /// `mode` 与 [`Self::candidate`] 共用「代际感知覆盖」：方案意图是默认值，本方案期间手动
    /// `toggle_punct` 胜出，切走再切回后失效（见 `docs/design/schema-scoped-behavior.md` §2、§4）。
    /// 两个字段的作用域不同，对照表在 [`PunctSpec`] 上。`smart_after_digit` 等属全局，别塞进来。
    #[serde(default)]
    pub punct: PunctSpec,
    /// 方案级候选呈现（`[candidate]` 段）。默认 `Follow` = 跟随下一层。
    ///
    /// 与 [`OverlaySpec::candidate_layout`] 并存、不要合并：那份管叠加激活期间，本段管常驻期间。
    #[serde(default)]
    pub candidate: CandidateSpec,
    /// 方案级短语加载（`[phrases]` 段）。默认全开。
    #[serde(default)]
    pub phrases: PhrasesSpec,
    /// 本次读取中没能按用户所写生效的项，人类可读，直接进 toast。
    ///
    /// 存描述而不是路径：段级降级给出段名（见 `EngineManager::salvage_schema`），字段级容错只
    /// 拿得到原值（见 [`crate::tolerant_de`]）。由 `EngineManager::read_schema` 填入。挂在返回值
    /// 上而不用全局收集器，是为了随 `Schema` 一起缓存、一起失效。
    #[serde(skip)]
    pub degraded_items: Vec<String>,
}

/// overlay 激活面配置（`[overlay]`）。
///
/// 装的是「这张码表被叠加使用时怎么表现」，字段语义都依赖 overlay 生命周期。
///
/// 刻意不含 `trigger_keys` / `hotkey`：入口统一在 `keys.key_actions` 与方案 `[key_actions]`，
/// 别在这里开第三个真相源（见设计文档 §2.2）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverlaySpec {
    /// overlay 类别。当前只有 `"special"`（引导键特殊模式）；空 = 按 `special` 处理。
    ///
    /// 段说「我可以被当 overlay 用」，本字段说「哪一类」。
    #[serde(default)]
    pub kind: String,
    /// 进入模式即展示候选：空编码（刚进入、尚未敲码）时枚举本方案码表首页候选
    /// （按 weight 降序），UI 按 per_page 分页浏览。默认 false（进入空白，敲码才出候选）。
    ///
    /// 面向快符/生僻字等**小符号表**的「进入即浏览」；大表会遍历全表取首 N 条、有开销，慎用。
    #[serde(default)]
    pub show_all_on_enter: bool,
    /// 进入本模式期间的候选布局（默认跟随全局）。每个 overlay 方案独立——快符表可竖排、
    /// 生僻字表可横排，互不影响。
    #[serde(default)]
    pub candidate_layout: crate::config::LayoutIntent,
    /// 已作废的注释模板键，只为给出降级提示而捕获，不生效（见 [`Schema::behavior`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_vertical: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_horizontal: Option<String>,
}

/// 方案元信息（[schema]）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchemaInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub icon_label: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    /// 隐藏方案：不在设置页「方案管理」列出，也不进循环切换。
    /// 用于内部 / 被引用的词库配置方案（如 english——仅供临时英文 / 融合英文候选懒加载）。
    #[serde(default)]
    pub hidden: bool,
    /// 方案角色。空 = 普通输入方案；`"aux"` = 主要充当辅助码来源的码表方案（如笔画）。
    ///
    /// aux 方案仍是可启用的输入方案（与 `hidden` 不同），只是设置页默认不列，勾上「显示特殊方案」
    /// 或已启用时才出现。辅助码来源选择器不看此字段。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
}

/// 图标主字的显示宽度上限（CJK 记 2、其余记 1），即一个汉字或两个拉丁字符。
///
/// 往上调必须同步扩 C++ 侧 `CLangBarItemButton::_inputTypeLabel` 的缓冲（`wcscpy_s` 超长会
/// 终止宿主进程）。单位是显示宽度而非字符数，双汉字标签才会截回首字。依据见
/// `docs/design/mode-icon-label-config.md` 第三节。
pub const ICON_LABEL_MAX_WIDTH: usize = 2;

/// 图标主字的统一截断口径：去首尾空白后，取显示宽度不超过 [`ICON_LABEL_MAX_WIDTH`] 的前缀。
/// 未配置 / 全空白返回空串，由调用方决定回落。
///
/// 方案标签、特殊模式、非中文态（[`crate::config::LabelsConfig`]）三处共用，别各写一份。
pub fn icon_label_trunc(raw: &str) -> String {
    crate::config::display_width_trunc(raw, ICON_LABEL_MAX_WIDTH)
}

/// 同 [`icon_label_trunc`]，但截断结果为空时回落 `fallback`。
///
/// 回落不能省：空标签会让语言栏图标画成一个空白方块。
pub fn icon_label_or(raw: &str, fallback: &str) -> String {
    let s = icon_label_trunc(raw);
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// 引擎配置（[engine]）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineSpec {
    /// "pinyin" / "codetable" / "mixed"（用 String 容忍未知/缺省，由 Schema 方法判定）
    #[serde(rename = "type", default)]
    pub engine_type: String,
    #[serde(default)]
    pub codetable: CodeTableSpec,
    #[serde(default)]
    pub pinyin: PinyinSpec,
    #[serde(default)]
    pub mixed: MixedSpec,
    /// 拆字（字根分解）反查与字根字体（码表方案的悬停提示用）。
    #[serde(default)]
    pub chaizi: ChaiziSpec,
    /// 辅助码码表（拼音/双拼候选的字形二次筛选用）。
    #[serde(default)]
    pub aux_code: AuxCodeSpec,
}

/// 拆字配置（[engine.chaizi]）。供悬停提示的"如何输入"反查与 PUA 字根字符渲染。
/// 路径相对 `data/schemas/`。三字段全空=该方案无拆字提示。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChaiziSpec {
    /// 拆字库路径（`字\t字根\t编码` 文本），相对 schemas 目录。
    #[serde(default)]
    pub db_path: String,
    /// 字根字体 TTF 文件路径，相对 schemas 目录（注册进 DirectWrite 自定义字体集）。
    #[serde(default)]
    pub font_path: String,
    /// 字根字体的 DirectWrite 家族名（取自 TTF name 表，如 "黑体字根"）；渲染时按此名引用。
    #[serde(default)]
    pub font_family: String,
}

impl ChaiziSpec {
    /// 是否配置了拆字（至少有库或字体路径）。
    pub fn is_configured(&self) -> bool {
        !self.db_path.is_empty() || !self.font_path.is_empty()
    }
}

/// `[engine.aux_code].files` 里「引用一个码表方案」的条目前缀：`"schema:wubi86"`。
/// 其余条目是码表文件路径（相对 schemas 目录）。见 `docs/design/aux-code-schema-source.md` §3。
pub const AUX_SCHEMA_SOURCE_PREFIX: &str = "schema:";

/// 辅助码方案段（`[engine.aux_code]`）：方案作者的码表基线 + 行为 tri-state 覆盖。
///
/// 与 [`CodeTableSpec`] 同构：`files` 是方案属性；`enabled` / `max_phrase_len` / `direct` 是
/// 用户可配行为，`None` 回落全局 `[schema.pinyin.aux_code]`。`files` 非空不等于功能开启，
/// 总闸是 `enabled`（出厂 `false`，见 [`crate::config::AuxCodeGlobal::resolved`]）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuxCodeSpec {
    /// 辅助码来源列表，**顺序即优先级**（先出现 = 高优）。两种条目：
    /// - `"aux_code/flypy_full.txt"`：码表文件（`字=码` 文本），相对 schemas 目录；
    /// - `"schema:wubi86"`：引用一个码表方案，取它的系统词库 + 用户词库里的编码
    ///   （见 [`AUX_SCHEMA_SOURCE_PREFIX`]）。只能引用 `codetable` 方案、不能引用自己。
    ///
    /// override 层写这个键即**整组替换**方案文件的基线（`merge_toml` 对数组整体替换）。
    #[serde(default)]
    pub files: Vec<String>,
    /// 本方案是否启用辅助码。`None` = 回落全局 `[schema.pinyin.aux_code].enabled`。
    ///
    /// 需要方案级覆盖，因为全拼与双拼键位预算不同：双拼符号键空闲，全拼要用符号键分隔音节，
    /// 「双拼开、全拼关」是常态需求。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 词组长度上限：字数 > 此值的**词组**一律排除、不参与辅助码筛选（0 = 不限）。
    /// `None` = 回落全局 `[schema.pinyin.aux_code].max_phrase_len`。
    ///
    /// 长词组按首字辅助码前缀匹配会大量残留、污染筛选；单字恒参与匹配。见 wind-aux-code 的
    /// `AuxCodeFilterOptions::max_phrase_len`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_phrase_len: Option<usize>,
    /// 本方案是否开直接辅助码（任一音节后 1～2 位自动当该字辅码，**仅双拼**）。`None` = 回落全局
    /// `[schema.pinyin.aux_code].direct`。只在 `enabled` 也生效时起作用；全拼方案写了
    /// `true` 不生效、日志告警一次。见 `docs/design/aux-code-direct.md` §7。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct: Option<bool>,
    /// 内部逃生口（不进设置页、无全局键）：`Some(false)` 让直接辅助码退回只做串尾，不走
    /// 连续辅助码（`docs/design/aux-code-continuous.md` §6）。`None` / `true` = 开着 `direct`
    /// 的双拼走连续模式。连续模式稳定后删除。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuous: Option<bool>,
    /// 内部项（不进设置页、无全局键）：编码栏里辅码前插的分隔符，连续辅助码的辅码候选
    /// （`ni hn'hc`）与串尾直接辅助（`ui'du p`）共用。`None` = 空格；可设空串，或由空格、`·`、
    /// `-`、`_`、`|` 组成的串，其余值不用（日志告警一次，退回空格）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preedit_separator: Option<String>,
}

impl AuxCodeSpec {
    /// 本方案是否配了码表（至少一个文件）。**不是**功能开关，见结构体文档。
    pub fn has_files(&self) -> bool {
        !self.files.is_empty()
    }
}

/// 码表引擎配置（[engine.codetable]）：引擎固定参数 + **方案内联行为覆盖**。
///
/// 行为字段为 tri-state `Option`：`None`=回落全局 `schema.codetable`，`Some`=覆盖该字段。
/// 方案文件与 `schema_overrides/{id}.toml` 用相同的段名/字段，后者经 `merge_toml` 深合并到
/// 前者之上，再由 `CodetableGlobal::resolved` 折叠到全局基线。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeTableSpec {
    // ── 引擎固定参数 ──
    /// 最大码长（0=未设置，构建时回退 4）
    #[serde(default)]
    pub max_code_length: usize,
    /// 基础排序："weight"（默认）/ "natural"（字根序/inner_order）。见 docs/redesign/frequency.md。
    #[serde(default)]
    pub base_sort: String,
    /// 输入码字符集，如 "a-x" / "a-x/" / "a-z0-9"。空=回退全局/默认（`a-z`）。
    #[serde(default)]
    pub input_chars: String,
    /// 可作**首码**的字符集（`input_chars` 的子集）。空=与 `input_chars` 相同。
    ///
    /// 典型用途：数字能作码元但不能起头，否则空缓冲下的数字键就不能选词或透传了。
    #[serde(default)]
    pub leading_chars: String,
    /// 整句输入：超过 `max_code_length` 的串自动切分成多个编码单元并组句。
    /// 设计与判据见 `docs/design/codetable-sentence-input.md`。
    ///
    /// 引擎固定参数而非行为 tri-state：能不能整句取决于码表的编码结构，由方案作者声明，出厂 `false`。
    ///
    /// 开启后 `top_code_commit` 自动让位（同抢超码长区间，见 `CodeTableEngine::handle_top_code`），
    /// `auto_commit_at_full` / `clear_on_empty_max` 同样让位（见 `CodeTableEngine::new`）。
    #[serde(default)]
    pub sentence_input: bool,
    /// 逆切分：恰好 `max_code_length` 长的串若空码，切成前后两段各查词典并拼成组合候选。
    /// 设计见 `docs/design/codetable-split-input.md`。引擎固定参数（同 `sentence_input`），出厂
    /// `false`，不限定方案类型。
    ///
    /// 与 `sentence_input` 占不同区间（`==` vs `>` 码长），可同开；但同开时顶码让位，逆切分的
    /// 「后码顶首选上屏」随之失效（构建时告警）。混输方案下整体不生效：切分候选的 `code` 是
    /// 整串，会压过拼音精确候选（见 `wind_engine::manager::resolve_split_input`）。
    #[serde(default)]
    pub split_input: bool,
    /// 逆切分的前段取几条候选。0/缺省 = 1。取多了同前段变体会占满候选窗；这是给方案作者调的
    /// 旋钮，不是终端用户开关。
    #[serde(default)]
    pub split_front_candidates: usize,
    /// 逆切分的触发档：`""`/`"empty"`（默认，整串零候选才切）/ `"no_exact"`（无精确解即切，
    /// 切分候选沉底且不参与自动上屏）。非法值回退 `"empty"` 并告警。
    #[serde(default)]
    pub split_trigger: String,
    /// 逆切分次选的显示形态：`""`/`"back"`（默认，次选只显示后段）/ `"full"`（显示整串）。
    /// 只影响显示，上屏恒为整串。非法值回退 `"back"` 并告警。
    #[serde(default)]
    pub split_alt_display: String,

    // ── 方案内联行为覆盖（None=回落全局 schema.codetable；Some=覆盖）──
    /// 顶码上屏（超满码长取前 N 码首选上屏）。
    #[serde(default)]
    pub top_code_commit: Option<bool>,
    /// 满码无候选时清空缓冲。
    #[serde(default)]
    pub clear_on_empty_max: Option<bool>,
    /// 满码唯一精确时自动上屏。
    #[serde(default)]
    pub auto_commit_at_full: Option<bool>,
    /// 自动上屏最短码长（隐藏参数；0=等于全码长）。
    #[serde(default)]
    pub auto_commit_min_len: Option<usize>,
    /// 标点触发上屏。
    #[serde(default)]
    pub punct_commit: Option<bool>,
    /// 显示编码提示。
    #[serde(default)]
    pub show_code_hint: Option<bool>,
    /// 精确匹配模式（关闭前缀匹配）。
    #[serde(default)]
    pub single_code_input: Option<bool>,
    /// 精确匹配空码补全。
    #[serde(default)]
    pub single_code_complete: Option<bool>,
    /// 出简让全的简码级别上限（0 关闭 / 2 一二级 / 3 全部）。
    ///
    /// 方案级可覆盖：「短码首选 = 简码」只对五笔这类前缀式简码成立。
    #[serde(default)]
    pub short_code_yield_level: Option<usize>,
    /// 本方案的单字输入覆盖。`None`（出厂）= 跟随全局 `schema.codetable.single_char`；`Some(_)`
    /// 恒压过全局。拼音侧没有对应的覆盖层；混输继承主码表方案的取值（见
    /// `EngineManager::codetable_settings`）。
    ///
    /// 内置方案不要声明这一项：方案级恒覆盖全局，用户在全局页关掉会纹丝不动（守门测试
    /// `factory_schemas_do_not_declare_single_char`）。第三方方案写它完全正当。
    ///
    /// 别挂 `tolerant_de::tolerant`：它只治字符串，布尔字段挂上去连正确值都解析失败。写错类型
    /// 走段级降级。
    #[serde(default)]
    pub single_char: Option<bool>,
    /// z 键重复输入。
    #[serde(default)]
    pub z_key_repeat: Option<bool>,
    /// z 键功能（`""`/`none` / `temp_pinyin` / `temp_english` / `mix:<id>` / `special:<id>`）。
    ///
    /// 方案级才有意义：z 能否借作引导键取决于这张码表里它是不是死码。
    /// 值域与语义见 `wind_config::config::BoundAction`。
    #[serde(default)]
    pub z_key_action: Option<String>,
    /// 通配输入开关（`None` = 跟随全局 `schema.codetable.wildcard`）。
    #[serde(default)]
    pub wildcard: Option<bool>,
    /// 通配键（`None` = 跟随全局）。合法性见 `wind_config::config::parse_wildcard_key`。
    #[serde(default)]
    pub wildcard_key: Option<String>,
    /// 通配仅单字（`None` = 跟随全局 `schema.codetable.wildcard_single_only`）。
    /// 见 `docs/design/codetable-reverse-mode.md` §2。
    #[serde(default)]
    pub wildcard_single_only: Option<bool>,
    /// 整句露几条（`None` = 跟随全局 `schema.codetable.sentence_count`）。
    #[serde(default)]
    pub sentence_count: Option<u8>,
    /// 整句算几条（`None` = 跟随全局 `schema.codetable.sentence_max_count`）。
    #[serde(default)]
    pub sentence_max_count: Option<u8>,
    /// 整句切换键（`None` = 跟随全局 `schema.codetable.sentence_cycle_key`）。
    #[serde(default)]
    pub sentence_cycle_key: Option<String>,
    /// 整句语法模型权重（`None` = 跟随全局 `schema.codetable.sentence_grammar_weight`）。
    #[serde(default)]
    pub sentence_grammar_weight: Option<f64>,
    /// 方案级调频覆盖（`[engine.codetable.frequency]`）。
    ///
    /// 缺省 = 整段跟随基线。特殊方案的基线是内置默认（不继承全局 `schema.codetable`，
    /// 见 `EngineManager::codetable_baseline`），普通方案的基线是全局段。
    #[serde(default)]
    pub frequency: Option<CodeTableFrequencySpec>,
    /// 方案级英文混入覆盖（`[engine.codetable.english_merge]`）。
    ///
    /// 缺省 = 整段跟随基线。不同码表对英文混入的诉求不同，故可按方案覆盖。
    #[serde(default)]
    pub english_merge: Option<CodeTableEnglishMergeSpec>,
}

/// 方案级英文混入覆盖（`[engine.codetable.english_merge]`），**逐字段稀疏**。
///
/// 每个字段都是 `Option`：给了就覆盖基线，没给就跟随。整段缺省 = 全部跟随。
/// 字段语义见 [`crate::config::CodetableEnglishMerge`]。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeTableEnglishMergeSpec {
    #[serde(default)]
    pub enable: Option<bool>,
    #[serde(default)]
    pub min_length: Option<usize>,
    #[serde(default)]
    pub block_commit: Option<bool>,
    #[serde(default)]
    pub complete_min_length: Option<usize>,
    #[serde(default)]
    pub block_commit_on_word: Option<bool>,
    #[serde(default)]
    pub exact_position: Option<usize>,
    #[serde(default)]
    pub complete_position: Option<usize>,
    #[serde(default)]
    pub max_count: Option<usize>,
}

/// 方案级调频覆盖（`[engine.codetable.frequency]`），**逐字段稀疏**。
///
/// 每个字段都是 `Option`：给了就覆盖基线，没给就跟随。整段缺省 = 全部跟随。
///
/// 不同码表的调频诉求不同（快符表要稳定顺序、生僻字表要学习、五笔要简码位保护）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CodeTableFrequencySpec {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 调频关闭时仍记录选词次数（见 `CodetableFrequency::record_always`）。
    #[serde(default)]
    pub record_always: Option<bool>,
    /// `"top"` / `"step"` / `"position"`。
    #[serde(default)]
    pub strategy: Option<String>,
    /// `"none"` / `"single"` / `"all"`；仅 `position` 生效。
    #[serde(default)]
    pub promote_prefix: Option<String>,
    /// 衰减半衰期（小时），`0` = 内置默认；仅 `position` 生效。
    #[serde(default)]
    pub half_life: Option<f64>,
    /// 全码位（码长 ≥ 4）首选保护。
    #[serde(default)]
    pub protect_top_n: Option<usize>,
    #[serde(default)]
    pub protect_top_n_len1: Option<usize>,
    #[serde(default)]
    pub protect_top_n_len2: Option<usize>,
    #[serde(default)]
    pub protect_top_n_len3: Option<usize>,
}

/// 拼音引擎配置（[engine.pinyin]）。
/// 注：show_code_hint / use_smart_compose / candidate_order / fuzzy 已上移为全局 [pinyin]。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinyinSpec {
    /// "full"（全拼）/ "shuangpin"（双拼）
    #[serde(default)]
    pub scheme: String,
    /// 双拼布局 id（引用 data/schemas/shuangpin/<layout>.toml）
    #[serde(default)]
    pub shuangpin: ShuangpinSpec,
    /// 本方案的手动音节分隔符策略（`auto` / `quote` / `backtick` / `none`）。
    /// `None`（及空串）= 回落全局 `[schema.pinyin].separator`。
    ///
    /// 需要方案级覆盖的理由同 [`AuxCodeSpec::enabled`]：双拼出厂把反引号给了辅助码，全局唯一的
    /// `separator` 一改就会静默夺走它。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
}

/// 双拼布局（[engine.pinyin.shuangpin]）
///
/// `layout` 引用一个布局 id（内置预置或用户自定义映射文件），引擎据此加载键位→声母/韵母
/// 映射与所用符号；**不在代码内硬编码具体方案**。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShuangpinSpec {
    #[serde(default)]
    pub layout: String,
}

/// 混输引擎配置（[engine.mixed]）。**仅引擎固定参数**（方案构成 + 内部权重基线）；
/// 融合策略（show_source_hint/enable_english/min_pinyin_length 等）已上移至全局 `schema.mix`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MixedSpec {
    #[serde(default)]
    pub primary_schema: String,
    #[serde(default)]
    pub secondary_schema: String,
}

/// 词库规格（[[dictionaries]]）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DictSpec {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// 设置端「分类词库」清单里的分组名（如「人名地名」）。只影响展示，不影响加载；
    /// 不写 = 未分组（设置端归「其他」；发现目录登记的库取词库 yaml 头里的 `group:`，没写归「扩展词库」）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default)]
    pub path: String,
    /// "rime_codetable" / "rime_pinyin" / "codetable"（空=回退 rime_codetable）
    #[serde(rename = "type", default)]
    pub dict_type: String,
    /// 主词库
    #[serde(default)]
    pub default: bool,
    /// 附加库的方案默认启用状态（tri-state，nil=未启用，见 `should_load`）
    #[serde(default)]
    pub default_enabled: Option<bool>,
    /// 用户覆盖启用（tri-state，nil=继承 default_enabled）
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 该词库的**层级基序档位**（小整数）：排序时作为独立层级（weight 之后、natural_order 之前）。
    /// 等权/`base_sort=natural` 时决定库间先后——设计者配 0/1/2…（如给扩展库配 1 排到主库 0 之后），
    /// 与词库条目数无关。默认 0。系统词库建议取 `>=0`（负值会与用户/临时词层的默认档交错）。
    #[serde(default)]
    pub base_order: i32,
    /// 默认权重（可选）：设置后**覆盖本库所有条目的权重**。用于**无权重的附加库**——与带权重
    /// 主库合并、按权重排序时让其条目落在设计者选定的权重档，而非 weight=0 全部沉底。默认 None=用自身权重。
    #[serde(default)]
    pub default_weight: Option<i32>,
}

/// 扩展词库发现目录（[[dictionary_dirs]]）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DictDirSpec {
    /// 相对 `schemas/` 的目录，各资源层合并扫描（用户目录遮蔽安装目录的同名文件）
    #[serde(default)]
    pub path: String,
    /// 目录里词库的类型，同 [`DictSpec::dict_type`]
    #[serde(rename = "type", default)]
    pub dict_type: String,
}

/// 方案级词库权重归一化（`[weight_spec]`）。语义与取舍见 [`Schema::weight_spec`]。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WeightSpec {
    /// 本方案全部词库合并后的权重**中位数**（声明值）。归一化后落到 `target`。
    #[serde(default)]
    pub median: i64,
    /// 归一化的**上锚点**。建议取 **p99 而非 max**：离群值会吃掉整个量程——虎码方案级
    /// max=1e11（12 条脏数据）而 p99=343,880，相差 30 万倍。超过本值者 clamp 到上界。
    #[serde(default)]
    pub max: i64,
    /// 保留字段（暂未消费）。
    #[serde(default)]
    pub min: i64,
    /// `"log"`（默认，推荐）/ `"linear"`。长尾分布必须用 log——线性压缩会把低段整除归零。
    #[serde(default)]
    pub mode: String,
    /// `median` 归一化后的落点。0 = 取默认 1000（与短语默认权重同量级）。
    #[serde(default)]
    pub target: i64,
}

/// 造词编码规则（[encoder]）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EncoderSpec {
    #[serde(default)]
    pub max_word_length: usize,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    #[serde(default)]
    pub rules: Vec<EncoderRule>,
}

/// 单条编码规则（[[encoder.rules]]）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EncoderRule {
    /// 精确匹配字数
    #[serde(default)]
    pub length_equal: usize,
    /// 字数范围 [min, max]
    #[serde(default)]
    pub length_in_range: Vec<usize>,
    /// 拆字公式，如 "AaAbBaBb"
    #[serde(default)]
    pub formula: String,
}

// 跨 crate 的序列化 / 反序列化一律走这几个入口，理由同 `Config::from_toml_value` 上方。
impl Schema {
    #[inline(never)]
    pub fn from_toml_value(v: toml::Value) -> Result<Self, toml::de::Error> {
        v.try_into()
    }

    #[inline(never)]
    pub fn to_toml_value(&self) -> Result<toml::Value, toml::ser::Error> {
        toml::Value::try_from(self)
    }

    /// [`crate::section_fallback::probe_and_patch`] 的 Schema 实例。
    #[inline(never)]
    pub fn probe_and_patch(
        merged: &toml::Value,
        default_v: &toml::Value,
    ) -> crate::section_fallback::Patched {
        crate::section_fallback::probe_and_patch::<Self>(merged, default_v)
    }
}

impl Schema {
    /// 是否为拼音类型引擎（engine.type 缺省时依据默认词典类型判定）
    pub fn is_pinyin(&self) -> bool {
        match self.engine.engine_type.to_lowercase().as_str() {
            "pinyin" => true,
            "codetable" | "mixed" => false,
            _ => {
                let default = self
                    .dictionaries
                    .iter()
                    .find(|d| d.default)
                    .or_else(|| self.dictionaries.first());
                matches!(default, Some(d) if d.dict_type == "rime_pinyin")
            }
        }
    }

    /// 是否为混输方案
    pub fn is_mixed(&self) -> bool {
        self.engine.engine_type.eq_ignore_ascii_case("mixed")
    }

    /// 该方案当前是否受支持（全拼/双拼均支持）
    pub fn is_supported(&self) -> bool {
        if self.is_pinyin() {
            let s = self.engine.pinyin.scheme.to_lowercase();
            return s.is_empty() || s == "full" || s == "shuangpin";
        }
        true
    }
}

impl DictSpec {
    /// 是否应加载（主词库，或 default_enabled 默认启用的附加库；enabled 用户覆盖优先）
    pub fn is_enabled(&self) -> bool {
        if let Some(e) = self.enabled {
            return e || self.default;
        }
        self.default || self.default_enabled.unwrap_or(false)
    }

    /// 词典类型（空时回退 rime_codetable）
    pub fn effective_type(&self) -> &str {
        if self.dict_type.is_empty() {
            "rime_codetable"
        } else {
            &self.dict_type
        }
    }
}

/// 方案级标点（`[punct]` 段）。
///
/// 取值词汇与 [`crate::config::LayoutIntent`] 同构（`Follow` 打头且是 default）。
///
/// # 两个字段的作用域刻意不同，不要统一
///
/// | 字段 | 归属方案取自 | 回答的是 |
/// |---|---|---|
/// | [`Self::mode`] | `active_behavior()`（活跃方案） | 此刻用中文还是英文标点，用户可见的模式态，走代际同步与 `punct_before_schema` 基线 |
/// | [`Self::custom_mappings`] | `effective_data_schema`（数据归属方案） | 这个键出什么字，是数据，与 `[phrases]` 同源 |
///
/// `custom_mappings` 跟活跃方案走，快符 / 临英就拿不到自己的符号表；`mode` 跟数据走，则要重验
/// `sync_schema_scope` 那条链。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PunctSpec {
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub mode: PunctIntent,
    /// 方案级自定义标点映射表（`[punct.custom_mappings]`），语义是整表替换：
    ///
    /// | 取值 | 含义 |
    /// |---|---|
    /// | `None`（段不写） | 跟随全局——全局的 `custom_enabled` 与整张表原样生效 |
    /// | `Some(表)` | 本方案只认这张表，全局表整份不参与（连 `custom_enabled` 一起换） |
    /// | `Some({})` | 本方案不用任何自定义映射 |
    ///
    /// 刻意没有方案级的 `custom_enabled`：「一条都不要」已由 `Some({})` 表达，再加开关只会造出
    /// 矛盾态。推论（设置页与文档站须写明）：全局页关掉「自定义标点」，声明了本表的方案仍出
    /// 自定义符号。
    ///
    /// override 走整体替换而非深合并（见 [`merge_toml`]），否则用户删不掉方案作者写的行。
    /// 列序契约与全局表一致：`[中半, 英全, 中全, 英半]`，见 [`crate::config::PunctConfig`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_mappings: Option<std::collections::HashMap<String, Vec<String>>>,
}

/// 标点态三态。`Follow` = 本方案不干预，沿用用户当前状态 / 全局配置。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PunctIntent {
    #[default]
    Follow,
    Chinese,
    English,
}

impl PunctIntent {
    /// 折成布尔目标态；`Follow` → `None`（不干预）。
    ///
    /// 返回 `Option` 让调用方决定「要不要写」，而不是「写什么」。
    pub fn resolve(self) -> Option<bool> {
        match self {
            Self::Follow => None,
            Self::Chinese => Some(true),
            Self::English => Some(false),
        }
    }
}

/// 方案级候选呈现（`[candidate]` 段）。
///
/// 字段叫 `layout` 而不是 `candidate_layout`：段名已含 `candidate`，再写就是
/// `candidate.candidate_layout`（违反 `config-design-rules` §R3 的路径冗余）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CandidateSpec {
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub layout: crate::config::LayoutIntent,
    /// 已作废的注释模板键，只为给出降级提示而捕获，不生效（见 [`Schema::behavior`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_vertical: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_horizontal: Option<String>,
    /// 本方案候选文字节点的字族覆盖（如蒙古文方案）。空 = 不覆盖（走主题 / 全局 `ui.font`）。
    ///
    /// 只作用于 text 节点：序号、注释等是拉丁/数字，按字符指派字体用全局 `ui.font.scripts`。
    /// 归属取 `effective_data_schema`（与 [`Self::layout`] 不同），随输入语境逐次按键变化，
    /// 须在 `UpdateCandidates` 发送前重算（见 `Coordinator::sync_candidate_font`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub font_family: String,
    /// 本方案横排时文字的排列方式（默认 / 旋转 90° / 文字竖排）。是「这套文字怎么写」的方案属性，
    /// 不是界面偏好，故住在方案里。
    ///
    /// 归属取 `effective_data_schema`，与 [`Self::font_family`] 同源（临英期间该随 `english`），
    /// 与 [`Self::layout`] 不同，取值时不要复用同一个 `behavior_for` 结果。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub text_orientation: crate::config::TextOrientation,
    /// 通用方案级覆盖：本段里不属于强类型字段的键原样收下，键名与全局 `ui.candidate.*` 相同。
    ///
    /// 是否生效由 [`crate::config_schema::SCHEMA_OVERRIDES`] 里 `fold = true` 的登记决定，值由
    /// `Schema::behavior` 逐项校验，不合法的只进降级清单。新增可覆盖键只需登记那一行。
    #[serde(flatten, default, skip_serializing_if = "toml::Table::is_empty")]
    pub ui: toml::Table,
}

/// [`CandidateSpec`] 里有强类型字段的键——它们不进通用补丁。
/// 与结构体字段同步；`candidate_typed_keys_are_not_foldable` 守着它们不被登记成通用键。
#[cfg(test)]
pub(crate) const CANDIDATE_TYPED_KEYS: [&str; 5] = [
    "layout",
    "comment_template_vertical",
    "comment_template_horizontal",
    "font_family",
    "text_orientation",
];

/// 方案级短语加载（`[phrases]` 段）。
///
/// 刻意没有三态：「一条都不要」已由 `enabled = false` 表达，故两个列表都用朴素 `Vec`，
/// 空 = 不施加这一项限制。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhrasesSpec {
    /// 本方案是否加载短语。`false` ⇒ 短语层对本方案整体关闭（六个消费点全部短路）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 分类白名单。空 = 不施加这一项限制（全部分类），不是「一条都不要」。
    ///
    /// 空串 `""` 匹配未分类短语（store 里 `category` 的默认值就是空串，不引入映射名）。
    /// ⚠️ 分类 UI 落地之前所有存量短语都是未分类，此时任何非空白名单都会把短语全部滤掉。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// 分类黑名单。空 = 不排除；在 [`Self::categories`] 之后再减。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_categories: Vec<String>,
}

impl Default for PhrasesSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            categories: Vec::new(),
            exclude_categories: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// 方案级行为覆盖三段的合订快照（`EngineManager::active_behavior` 的返回体）。
///
/// 合成一个缓存条目：它们同源（一次 `read_schema`）、同批失效，分开缓存就要多处失效点。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchemaBehavior {
    pub punct: PunctIntent,
    /// 方案级自定义标点表（整表替换，`None` = 跟随全局）。见 [`PunctSpec::custom_mappings`]。
    ///
    /// 刻意与 [`Self::punct`] 分成两个字段：两者取归属方案的判据不同（数据归属 vs 活跃方案），
    /// 打包会诱使人用同一个 `behavior_for(id)` 结果喂两条路。
    pub punct_custom_mappings: Option<std::collections::HashMap<String, Vec<String>>>,
    pub candidate_layout: crate::config::LayoutIntent,
    /// 方案级通用 UI 覆盖（`[candidate]` 里已登记且值合法的键，键名同全局 `ui.candidate.*`）。
    /// 空 = 本方案不覆盖任何项。见 [`CandidateSpec::ui`]；折叠方法是
    /// [`crate::config::UiCandidateConfig::folded_with`]。
    pub candidate_ui: toml::Table,
    /// 方案级候选文字字族，见 [`CandidateSpec::font_family`]。归属判据与 `candidate_layout` 不同
    /// （数据方案 vs 活跃方案），取值时不要复用同一个 behavior。
    pub candidate_font_family: String,
    /// 方案级「横排时文字排列」。见 [`CandidateSpec::text_orientation`]——归属判据与
    /// `candidate_font_family` 同源（数据方案），与 `candidate_layout` 不同。
    pub candidate_text_orientation: crate::config::TextOrientation,
    pub phrases: PhrasesSpec,
    /// 这次读取里没能按用户所写生效的项（见 [`Schema::degraded_items`]）。与行为同源同批失效，
    /// 故搭 `behavior_cache` 这趟车，不另开缓存。
    pub degraded_items: Vec<String>,
}

impl Schema {
    /// 抽出方案级行为覆盖三段。
    pub fn behavior(&self) -> SchemaBehavior {
        let (candidate_ui, ui_problems) =
            crate::config_schema::split_candidate_patch(&self.candidate.ui);
        let mut degraded_items = self.degraded_items_with_retired();
        degraded_items.extend(ui_problems);
        SchemaBehavior {
            punct: self.punct.mode,
            punct_custom_mappings: self.punct.custom_mappings.clone(),
            candidate_layout: self.candidate.layout,
            candidate_ui,
            candidate_font_family: self.candidate.font_family.clone(),
            candidate_text_orientation: self.candidate.text_orientation,
            phrases: self.phrases.clone(),
            degraded_items,
        }
    }

    /// [`Self::degraded_items`] 加上方案文件里残留的注释模板键：方案文件不再配置注释。
    fn degraded_items_with_retired(&self) -> Vec<String> {
        let mut items = self.degraded_items.clone();
        let mut retired = |section: &str, v: &Option<String>, h: &Option<String>| {
            if v.is_some() || h.is_some() {
                items.push(format!(
                    "[{section}] comment_template_*（方案文件不再配置注释，请到「设置 → 候选注释」配置）"
                ));
            }
        };
        let c = &self.candidate;
        retired(
            "candidate",
            &c.comment_template_vertical,
            &c.comment_template_horizontal,
        );
        if let Some(o) = &self.overlay {
            retired(
                "overlay",
                &o.comment_template_vertical,
                &o.comment_template_horizontal,
            );
        }
        items
    }
}

/// 深合并 TOML：`over` 覆盖到 `base` 之上。两侧皆为 table 时逐键递归；否则 over 整体替换。
/// 数组按整体替换（如 encoder.rules 覆盖即替换全表）。
///
/// 这是 `schema_overrides/{id}.toml` 折叠到方案文件之上的唯一合并实现，引擎加载与方案包导出
/// 共用，否则导出的包与实际打字行为不一致。
///
/// 两个例外：
/// - `dictionaries` 走 [`merge_dict_overrides`] 的按 id 稀疏合并，结构定义以方案文件为准。
/// - `custom_mappings` 走整体替换：逐键深合并表达不了删除，用户会删不掉方案作者写的行。
///
/// 两个例外都按键名匹配、与路径无关；别处若再出现同名键，要么改成判路径，要么换个键名。
pub fn merge_toml(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, ov) in o {
                match b.get_mut(&k) {
                    Some(bv) if k == "dictionaries" => merge_dict_overrides(bv, &ov),
                    Some(bv) if k == "custom_mappings" => *bv = ov,
                    Some(bv) => merge_toml(bv, ov),
                    // base 无 dictionaries 时不接纳 override 的稀疏项（它们无 path，凭空
                    // 造不出可用词库）；其余键正常新增。
                    None if k == "dictionaries" => {}
                    None => {
                        b.insert(k, ov);
                    }
                }
            }
        }
        (b, ov) => *b = ov,
    }
}

/// `dictionaries` 的 override 合并：按 `id` 匹配，且只接受 `enabled` 字段。
///
/// 方案文件是词库结构的唯一权威，override 层只记用户的开关；整体替换会让方案升级后的新词库、
/// 新 path 透不过来。字段白名单顺带忽略老 override 里的整表快照。
///
/// override 里 id 在方案文件中找不到（词库已被方案删除）的条目静默丢弃。
fn merge_dict_overrides(base: &mut toml::Value, over: &toml::Value) {
    let (Some(base_arr), Some(over_arr)) = (base.as_array_mut(), over.as_array()) else {
        return;
    };
    for ov in over_arr {
        let (Some(id), Some(enabled)) = (
            ov.get("id").and_then(|v| v.as_str()),
            ov.get("enabled").and_then(|v| v.as_bool()),
        ) else {
            continue;
        };
        for b in base_arr.iter_mut() {
            if b.get("id").and_then(|v| v.as_str()) == Some(id)
                && let Some(t) = b.as_table_mut()
            {
                t.insert("enabled".to_string(), toml::Value::Boolean(enabled));
            }
        }
    }
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
