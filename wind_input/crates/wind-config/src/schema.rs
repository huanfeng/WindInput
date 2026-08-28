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
    /// **方案级词库权重归一化**（`[weight_spec]`）。`None` = 不归一化（默认）。
    ///
    /// ## 为什么是方案级而不是按词库
    ///
    /// 一个方案的子词库是**同一套规则下的分组**（主库/扩展/地名/emoji），分库是为了
    /// 分组与分级，不是各自独立的权重体系。故全方案共用一个映射函数——单调映射天然保序，
    /// **库间相对关系原样保留**。
    ///
    /// ⚠️ 按词库配过一版，实测有反转：给扩展库单独配归一化后，`aaah` 下「欧莱雅」
    /// （扩展库 1170→3328）反超「葡萄牙」（主库 1485），而作者写的 `base_order = 1`
    /// 救不回来（`better()` 是 `weight 降 → base_order 升`，weight 在前）。
    /// 根因就是**两个不同的映射函数之间没有保序保证**。
    ///
    /// ## 用途：Rime 生态导入的适配层
    ///
    /// 主要场景是从 Rime 生态导入的方案——其权重常是**未归一化的原始语料词频**
    /// （虎码「的」= 10,359,470），与本仓约定的 `0~10000` 差三个数量级，
    /// 于是「短语 vs 码表」的权重比较失真。本仓自产方案（五笔等）守约，**不配**。
    ///
    /// ⚠️ **拼音方案不要配**：拼音权重刻意在另一条轴（max 1537 万），且
    /// `pinyin/mod.rs` 的 `COMPLETION_FAR_WEIGHT_FLOOR`(100) /
    /// `SENTENCE_YIELD_WEIGHT_FLOOR`(50) 是**按原始权重分布标定的绝对阈值**
    /// （该处代码注释亦有明示）。归一化后拼音 p50 从 20 抬到 target，两道闸门会
    /// **静默失效**——人人过线，等于没有。
    ///
    /// 参数建议由 `wind_input dict weight-check` 按方案实测算出，不必手填。
    #[serde(default)]
    pub weight_spec: Option<WeightSpec>,
    #[serde(default)]
    pub encoder: Option<EncoderSpec>,
    /// **方案级按键功能表**（`[key_actions]`）：按键名 → 动词。
    ///
    /// 与 `[engine]` 平级而非放在 `[engine.codetable]` 下：按键功能与引擎类型无关，
    /// 拼音方案同样需要它。值域与语义见 [`crate::BoundAction`] 与
    /// `docs/design/schema-key-actions.md`。
    ///
    /// 空表 = 不覆盖任何键，各键照常走全局引导键链。**逐键合并**，不是整段替换：
    /// 方案文件内联段与 `schema_overrides/{id}.toml` 在 toml 层由 `merge_toml` 合并，
    /// 那里只能新增/覆盖、无法删除键——故「本方案禁用某个全局绑定」必须写显式
    /// `"none"`，不能靠从 override 里删掉那一行。
    ///
    /// 用 `BTreeMap`：顺序无语义（优先级由分派插入点决定），键唯一由类型保证。
    #[serde(default)]
    pub key_actions: std::collections::BTreeMap<String, String>,
    /// **方案级会话态按键表**（`[session_actions]`）：按键名 → 动词。
    ///
    /// 与 [`Self::key_actions`] 是两张表而不是一张带状态维度的表，因为两者的到达条件不同：
    /// 本表只在**有会话时**（有编码或候选）生效，那时用户停留在处境里反复按键、有肌肉记忆；
    /// `key_actions` 则是无会话态的引导键。判据见 `docs/design/session-key-actions.md` §2。
    /// 值域与语义见 [`crate::SessionAction`]。
    ///
    /// **逐键合并**，与 `key_actions` 一致：方案只写想改的键，其余继承全局
    /// `keys.session_actions` 与四组键组配置的展开结果。⚠️ 故「本方案禁用某个全局绑定」
    /// 必须写显式 `"none"`——`merge_toml` 只能新增/覆盖，**无法表达删除**，靠从 override
    /// 里删掉那一行是删不掉 base 里那条的。
    ///
    /// ⚠️ 本表参与**可达性并集**：所有方案绑过的键都要让 TSF 转发，否则切方案后 C++ 手里
    /// 还是旧表。代价是别的方案里也转发这些键——keyup 侧无害，但本表支持减号、方括号等
    /// **可打印符号键**，它们带 `FORWARD_ONLY` 进 keydown 表，无会话时必须放行给下游按标点
    /// 处理，否则就是丢键。见 `docs/design/key-resolver-unification.md` §8 注意点 5。
    #[serde(default)]
    pub session_actions: std::collections::BTreeMap<String, String>,
    /// **overlay 激活面**（`[overlay]`）：本方案可被引导键/直达热键叠加激活时的呈现配置。
    ///
    /// **段存在即声明「我是 overlay 方案」**——这同时是实例集合的枚举依据
    /// （`EngineManager::overlay_modes`）。`None` = 普通方案，只能作 base 常驻使用。
    ///
    /// 不能复用 `[schema] hidden` 作这个判据：两者回答的是不同问题。`hidden` 是**展示**
    /// 属性（列不列进方案切换列表），本段是**行为**属性（有没有叠加进入/退出的生命周期）。
    /// 一个 overlay 方案完全可以不 hidden（作者想让它同时能常驻切换），一个 hidden 的
    /// 码表方案也可能只是 mix 成员、没有 overlay 生命周期。
    ///
    /// 见 `docs/redesign/overlay-mode-config.md`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<OverlaySpec>,
    /// **方案级标点**（`[punct]` 段）：标点态意图 `mode` + 自定义映射表 `custom_mappings`。
    ///
    /// `mode` 与 [`Self::candidate`] 共用「代际感知覆盖」：方案意图是默认值，用户在本方案
    /// 期间手动 `toggle_punct` 的值胜出，切走再切回后手动值随 `schema_generation` 失效。
    /// 见 `docs/design/schema-scoped-behavior.md` §2、§4。
    ///
    /// ⚠️ **两个字段的作用域不同**（`mode` 跟活跃方案、`custom_mappings` 跟数据归属方案），
    /// 对照表在 [`PunctSpec`] 上，动之前先读。
    ///
    /// ⛔ 仍**不含** `smart_after_digit` / `smart_list` / `follow_mode`：前两者是「数字后
    /// 智能标点」的参数、后者是切中英时的交互习惯，都属全局。段名留成 `punct` 是为了以后
    /// 能加，但不要顺手塞进来。
    #[serde(default)]
    pub punct: PunctSpec,
    /// **方案级候选呈现**（`[candidate]` 段）。默认 `Follow` = 跟随下一层。
    ///
    /// ⚠️ 与 [`OverlaySpec::candidate_layout`] **两段并存、语义不同，不要合并**：
    /// 那份是「本方案被叠加激活期间」的布局（有进入/退出生命周期），本段是「本方案作为
    /// 常驻 active 方案期间」的布局。一个方案可以两段都写，取值互不干扰。
    #[serde(default)]
    pub candidate: CandidateSpec,
    /// **方案级短语加载**（`[phrases]` 段）。默认全开。
    #[serde(default)]
    pub phrases: PhrasesSpec,
}

/// overlay 激活面配置（`[overlay]`）。
///
/// ★ 这一段装的**不是**「这张码表是什么」（那是 `[engine.codetable]`），而是
/// 「这张码表**被叠加使用时**怎么表现」——三个字段的语义都依赖 overlay 生命周期：
/// `show_all_on_enter` 只在存在「进入这一刻」时才有意义；`candidate_layout` 的语义是
/// 「本模式期间覆盖全局、退出自动恢复」。段名 `overlay` 由此而来。
///
/// ⛔ **刻意不含 `trigger_keys` / `hotkey`**：引导键与直达热键统一住在 `keys.key_actions`
/// （全局）与方案文件 `[key_actions]`（按源方案分流）两张表里。在此再开一个入口字段
/// 就是第三个真相源，正是本轮重构要消除的东西。见设计文档 §2.2。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverlaySpec {
    /// overlay 类别。当前只有 `"special"`（引导键特殊模式）；空 = 按 `special` 处理。
    ///
    /// 留这个字段是为消歧：`overlay` 在本仓有两个粒度——运行时状态（临拼/临英/mix/URL
    /// 也都是 overlay，但它们无宿主方案、配置只能待在 `input.*`）与方案文件的这一段
    /// （仅有宿主方案者）。段说「我可以被当 overlay 用」，本字段说「哪一类」。
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
    /// 本模式期间的注释模板覆盖（竖排）。三态见 [`crate::config::CommentTemplateOverride`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_vertical: crate::config::CommentTemplateOverride,
    /// 本模式期间的注释模板覆盖（横排）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_horizontal: crate::config::CommentTemplateOverride,
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
}

/// 图标主字的**显示宽度**上限（CJK 记 2、其余记 1）——即「一个汉字」或「两个拉丁字符」。
///
/// 定在 2 的两条依据，**缺一都不足以定案**：
/// ① 渲染：语言栏图标画布只有 16px（DPI 缩放后 20/24/32）。两个 ASCII 字母（`En`）
///    宽度约等于一个汉字，是"还认得出"的上限；再宽只能靠回缩字号塞进去，糊成一团。
/// ② 安全：C++ 侧 `CLangBarItemButton::_inputTypeLabel` 走 `wcscpy_s`——**超长不是
///    静默截断，是触发 invalid parameter handler 终止进程**，而那段代码跑在
///    Word / QQ 等宿主进程里。宽度 2 蕴含**最多 2 个标量值**（每个字符至少占 1 宽），
///    最坏 2 个 emoji = 4 wchar + NUL = 5，缓冲现为 `wchar_t[8]`。
///
/// ⚠️ 改这个值之前先读上面第 ②：往上调必须同步扩 C++ 侧那个缓冲，否则是在给
/// 用户配置留一条崩宿主进程的路。
///
/// ## 为什么单位是显示宽度而不是字符数（issue #85）
///
/// 上限刚放宽到 2 时按**字符数**计，于是 `icon_label = "虎单"` 这类第三方方案的双汉字
/// 标签被整个放行，渲染端为了不裁切只能把字号回缩到约一半——用户没改配置，升级后
/// 任务栏图标从清晰的「虎」变成认不出的「虎单」。
///
/// 按宽度计同时满足两侧：`En` 仍完整显示（2×1），双汉字截回首字（2 已满）。
/// 与工具栏按钮 [`crate::config::toolbar_label_trunc`] 现在是**同一条规则、不同上限常量**，
/// 共用 [`crate::config::display_width_trunc`] 一份实现。
pub const ICON_LABEL_MAX_WIDTH: usize = 2;

/// 图标主字的**统一截断口径**：去首尾空白后，取显示宽度不超过
/// [`ICON_LABEL_MAX_WIDTH`] 的前缀。未配置 / 全空白返回空串，由调用方决定回落成什么。
///
/// ## 为什么必须是共享函数
///
/// 这条口径有三个调用点（方案标签 `schema_icon_label`、特殊模式 `overlay_modes`、
/// 非中文态 [`crate::config::LabelsConfig`]）。此前前两处各写了一份 `.chars().next()`，
/// 其中一处的注释还写着"与 schema_icon_label 同口径"——**一个自我声明的耦合，
/// 编译器不管**。只改一处的表现是"方案切换显 `Wb`、进特殊模式显 `符`"这种局部
/// 不一致，且没有任何测试会发现。
pub fn icon_label_trunc(raw: &str) -> String {
    crate::config::display_width_trunc(raw, ICON_LABEL_MAX_WIDTH)
}

/// 同 [`icon_label_trunc`]，但截断结果为空时回落 `fallback`。
///
/// ⚠️ 回落**不能省**：空标签会让语言栏图标画出一个没有主字的空白方块，
/// 用户既看不出当前模式、也无从理解发生了什么。
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

/// 辅助码方案段（`[engine.aux_code]`）：**方案作者的码表基线 + 行为 tri-state 覆盖**。
///
/// 与 [`CodeTableSpec`] 同构（见 schema-config-layering.md §4）：
/// - `files` 是方案属性（全拼配笔画、双拼配小鹤形码——换表不换方案），留在方案文件；
/// - `enabled` / `max_phrase_len` 是用户可配行为，tri-state `Option`：
///   `Some` 覆盖、`None` 回落全局 `[schema.pinyin.aux_code]`。
///
/// ⚠️ **`files` 非空不等于功能开启**。总闸是 `enabled`（出厂 `false`），
/// 判据见 [`crate::config::AuxCodeGlobal::resolved`]——方案文件里配了推荐码表，
/// 只是「这个方案该用哪张表」，不代表用户要用它。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuxCodeSpec {
    /// 辅助码文件列表（`字=码` 文本，每行一条），相对 schemas 目录。
    /// 多份按**顺序** merge 成一张表（先出现 = 高优，见 wind-aux-code 的 merge 语义）。
    #[serde(default)]
    pub files: Vec<String>,
    /// 本方案是否启用辅助码。`None` = 回落全局 `[schema.pinyin.aux_code].enabled`。
    ///
    /// ★ **为什么需要方案级覆盖**：全拼与双拼在这个功能上不是「偏好不同」而是
    /// **键位预算不同**——双拼把韵母塞进字母键、符号键全空闲；全拼的音节边界要靠
    /// 符号表达，反引号出厂即被 `manual_separator_key` 占用。故「双拼开、全拼关」
    /// 是常态需求，一个全局开关表达不了。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 词组长度上限：字数 > 此值的**词组**一律排除、不参与辅助码筛选（0 = 不限）。
    /// `None` = 回落全局 `[schema.pinyin.aux_code].max_phrase_len`。
    ///
    /// 长词组（整词补全/组合词，如 `meishijian` 下的「没时间看/没时间做」）首字辅助码
    /// 前缀匹配会让它们大量残留、污染逐字词筛选，而辅助码字形筛选的目标是短字词；
    /// 单字恒参与匹配，不受此限。见 wind-aux-code 的 `AuxCodeFilterOptions::max_phrase_len`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_phrase_len: Option<usize>,
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
/// schema 文件与 `schema_overrides/{id}.toml` 用**完全相同的段名/字段**表达行为——前者是作者
/// 内联基线，后者（设置页写入）经 `read_schema` 的 `merge_toml` 深合并到前者之上，最终由
/// `CodetableGlobal::resolved` 折叠到全局基线。故不再有独立的 `SchemeOverride` 平行路径
/// （见 docs/redesign/schema-config-layering.md）。
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
    /// 典型用途：数字要能作码元（打得出 `Win10`），但不能起头——空缓冲下的数字键是
    /// 选词/透传，若它同时是首码，用户就永远选不了「第 1 个候选」也拿不回原生数字输入。
    #[serde(default)]
    pub leading_chars: String,
    /// 整句输入：超过 `max_code_length` 的串自动切分成多个编码单元并组句。
    /// 设计与判据见 `docs/design/codetable-sentence-input.md`。
    ///
    /// **引擎固定参数而非行为 tri-state**：一张码表能不能整句取决于它的编码结构
    /// （定长与否、简码体系多深），是方案属性，不是「用户偏好」——故不设全局回落，
    /// 由方案作者在 `.schema.toml` 里声明。出厂 `false`。
    ///
    /// ⚠️ 开启后 `top_code_commit` 自动让位：两者抢同一个区间（超码长），
    /// 而顶码是自动上屏、一触发用户就看不到整句候选（见 `CodeTableEngine::handle_top_code`）。
    #[serde(default)]
    pub sentence_input: bool,

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
    /// 方案级可覆盖：不同码表的简码体系深浅不同，且「短码首选 = 简码」这个等式只对
    /// 五笔这类前缀式简码成立，别的码表可以在方案里关掉。
    #[serde(default)]
    pub short_code_yield_level: Option<usize>,
    /// z 键重复输入。
    #[serde(default)]
    pub z_key_repeat: Option<bool>,
    /// z 键功能（`""`/`none` / `temp_pinyin` / `temp_english` / `mix:<id>` / `special:<id>`）。
    ///
    /// 方案级才有意义：z 能否借作引导键取决于这张码表里它是不是死码。
    /// 值域与语义见 `wind_config::config::BoundAction`。
    #[serde(default)]
    pub z_key_action: Option<String>,
    /// 方案级调频覆盖（`[engine.codetable.frequency]`）。
    ///
    /// 缺省 = 整段跟随基线。特殊方案的基线是内置默认（不继承全局 `schema.codetable`，
    /// 见 `EngineManager::codetable_baseline`），普通方案的基线是全局段。
    #[serde(default)]
    pub frequency: Option<CodeTableFrequencySpec>,
}

/// 方案级调频覆盖（`[engine.codetable.frequency]`），**逐字段稀疏**。
///
/// 每个字段都是 `Option`：给了就覆盖基线，没给就跟随。整段缺省 = 全部跟随。
///
/// 存在的理由是「同一台机器上不同码表的调频诉求本就不同」——快符表要的是稳定顺序
/// （作者精心排过），生僻字表要的是学习，五笔要的是简码位保护。此前这些只有一份全局值。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CodeTableFrequencySpec {
    #[serde(default)]
    pub enabled: Option<bool>,
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
    #[serde(default)]
    pub path: String,
    /// "rime_codetable" / "rime_pinyin" / "codetable"（空=回退 rime_codetable）
    #[serde(rename = "type", default)]
    pub dict_type: String,
    /// 主词库
    #[serde(default)]
    pub default: bool,
    /// 非默认但默认启用的附加库（tri-state，nil=true）
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
/// 取值词汇刻意与 [`crate::config::LayoutIntent`] 同构（`Follow` 打头且是 default）：
/// 让「方案级」与「全局」在用户眼里是同一件事的两个层级，而不是两套发明出来的词。
///
/// # ★★ 两个字段的作用域**刻意不同**，不要「顺手统一」
///
/// | 字段 | 归属方案取自 | 因为它回答的是 |
/// |---|---|---|
/// | [`Self::mode`] | `active_behavior()`（活跃方案） | 「我此刻该用中文还是英文标点」——**用户可见的模式态**，与语言栏图标、`toggle_punct` 同一层，走代际同步与 `punct_before_schema` 基线 |
/// | [`Self::custom_mappings`] | `effective_data_schema`（数据归属方案） | 「这个键出什么字」——是**数据**，与 `[phrases]` 同源；临英归 `english`、快符归快符方案 |
///
/// 把 `custom_mappings` 改成跟活跃方案走，快符/临英就永远拿不到自己的符号表（而那正是
/// 「特殊方案本质就是码表方案、该有自己的一套」的直接延伸）；把 `mode` 改成跟数据走，则要
/// 重跑那条已真机修过一轮的链（`Follow` ≠「什么都不做」，见 `sync_schema_scope`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PunctSpec {
    #[serde(default)]
    pub mode: PunctIntent,
    /// **方案级自定义标点映射表**（`[punct.custom_mappings]`）。语义是**整表替换**：
    ///
    /// | 取值 | 含义 |
    /// |---|---|
    /// | `None`（段不写） | 跟随全局——全局的 `custom_enabled` 与整张表原样生效 |
    /// | `Some(表)` | 本方案只认这张表，全局表**整份不参与**（连 `custom_enabled` 一起换） |
    /// | `Some({})` | 本方案不用任何自定义映射 |
    ///
    /// # ⛔ 刻意**没有**方案级的 `custom_enabled`
    ///
    /// 三态是「跟随 / 用我的表 / 一条都不要」，而第三态已由 `Some({})` 表达。两个字段的代价
    /// 不是多一个字段，是**矛盾态**（`enabled=None` 配非空表、`=true` 配空表都得再规定一次
    /// 语义，且规定完没人记得）。同型见 `[phrases]` 的 `categories`——那里的 `Option<Vec>`
    /// 三态也是因为「一条都不要」已被 `enabled=false` 表达而塌成了普通 `Vec`。
    ///
    /// ⇒ 推论（**设置页与文档站必须写明**）：全局页关掉「自定义标点」总开关，声明了本表的
    /// 方案**仍然出自定义符号**——那个开关管的是全局表，整表替换连开关一起换掉了。
    ///
    /// # ⚠️ override 走整体替换，不是深合并
    ///
    /// 见 [`merge_toml`] 里的 `custom_mappings` 例外。深合并（HashMap 逐键）会让用户在设置页
    /// **删不掉方案作者写的行**——override 层没有「删除」的表达。
    ///
    /// 列序契约与全局表**完全一致**：`[中半, 英全, 中全, 英半]`，引号用 `"1`/`"2`/`'1`/`'2`
    /// 区分左右形。见 [`crate::config::PunctConfig`]。
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
    /// 与 `LayoutIntent` 的 `Follow` 一样表示「跟随下一层」，故返回 `Option` 而不是
    /// 默认值——调用方据此决定「要不要写」，而不是「写什么」。
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
///
/// ## 注释模板：与 `[overlay]` 那两份**并存且语义不同**
///
/// 本段这两份是「本方案作为**常驻 active 方案**期间」的注释模板，
/// [`OverlaySpec::comment_template_vertical`] 那两份是「本方案**被叠加激活期间**」的。
/// 与 `layout` / `candidate_layout` 的并存关系完全同构——一个方案可以两段都写，
/// 取值互不干扰。见 `docs/design/candidate-comment-layering.md` §1.2。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateSpec {
    #[serde(default)]
    pub layout: crate::config::LayoutIntent,
    /// 本方案的注释模板覆盖（竖排）。三态见 [`crate::config::CommentTemplateOverride`]：
    /// 键缺失 = 跟随全局、非空 = 覆盖、空串 = 本方案不显示注释。
    ///
    /// 字段名与全局 `ui.candidate.comment_template_vertical`、与 `[overlay]` 同名字段
    /// **逐字一致**：让「全局 / 方案 / 模式」在用户眼里是同一件事的三个层级，
    /// 而不是三套发明出来的键名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_vertical: crate::config::CommentTemplateOverride,
    /// 本方案的注释模板覆盖（横排）。见 [`Self::comment_template_vertical`]。
    ///
    /// 横竖**各自独立三态**：只覆盖竖排、横排仍跟随全局是合法且常见的配置
    /// （竖排每行独占，横排全部候选共享一行宽度，能放什么本就不是同一个答案）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_horizontal: crate::config::CommentTemplateOverride,
}

/// 方案级短语加载（`[phrases]` 段）。
///
/// ## ★ 这里刻意**没有**三态
///
/// 设计稿曾把 `categories` 写成 `Option<Vec<String>>`，用「键缺失 = 全部 / 空数组 =
/// 一条都不要」表达三态。作废理由：**「一条都不要」已经由 `enabled = false` 表达**，
/// `enabled = true` + `categories = []` 是一个语义重复的状态。既然重复，就不必区分
/// 缺失与空 ⇒ 两个字段都用朴素 `Vec`，语义完全对称：**空 = 不施加这一项限制**。
///
/// 判据可复用：**给一族过滤器加维度前，先问「这个三态里有没有一态已经被别的字段表达了」**。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhrasesSpec {
    /// 本方案是否加载短语。`false` ⇒ 短语层对本方案整体关闭（六个消费点全部短路）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 分类白名单。**空 = 不施加这一项限制**（全部分类），不是「一条都不要」。
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
/// 三段合成一个缓存条目而不是各存各的：它们同源（一次 `read_schema`）、同批失效，
/// 分三个缓存等于同一个方案读三次盘、加三个失效点——`key_actions` / `session_actions`
/// 那两个缓存已经因为「同批失效要接两处」在注释里互相提醒过一次。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaBehavior {
    pub punct: PunctIntent,
    /// 方案级自定义标点表（整表替换，`None` = 跟随全局）。见 [`PunctSpec::custom_mappings`]。
    ///
    /// ★ **刻意与 [`Self::punct`] 分成两个字段而不是打包成一个 `PunctSpec`**：两者虽同住
    /// `[punct]` 段，取归属方案的判据却不同（前者数据归属、后者活跃方案）。打包成一个结构体
    /// 会让下一个人理所当然地用同一个 `behavior_for(id)` 结果去喂两条路——而那正是错的。
    pub punct_custom_mappings: Option<std::collections::HashMap<String, Vec<String>>>,
    pub candidate_layout: crate::config::LayoutIntent,
    /// 方案级注释模板（竖排）。三态，见 [`CandidateSpec::comment_template_vertical`]。
    pub comment_template_vertical: crate::config::CommentTemplateOverride,
    /// 方案级注释模板（横排）。
    pub comment_template_horizontal: crate::config::CommentTemplateOverride,
    pub phrases: PhrasesSpec,
}

impl Schema {
    /// 抽出方案级行为覆盖三段。
    pub fn behavior(&self) -> SchemaBehavior {
        SchemaBehavior {
            punct: self.punct.mode,
            punct_custom_mappings: self.punct.custom_mappings.clone(),
            candidate_layout: self.candidate.layout,
            comment_template_vertical: self.candidate.comment_template_vertical.clone(),
            comment_template_horizontal: self.candidate.comment_template_horizontal.clone(),
            phrases: self.phrases.clone(),
        }
    }
}

/// 深合并 TOML：`over` 覆盖到 `base` 之上。两侧皆为 table 时逐键递归；否则 over 整体替换。
/// 数组按整体替换（如 encoder.rules 覆盖即替换全表）。
///
/// 这是 `schema_overrides/{id}.toml` 折叠到方案文件之上的**唯一**合并实现：
/// 引擎加载（wind-engine `read_schema`）与方案包导出（wind-transfer）共用，
/// 两边对「定制后的方案长什么样」必须同源，否则导出的包与实际打字行为不一致。
///
/// # 两个例外
///
/// - **`dictionaries`** 走 [`merge_dict_overrides`] 的按 id 稀疏合并——词库的
///   path/label/base_order 等结构定义必须始终以方案文件为准，override 层只表达用户开关。
/// - **`custom_mappings`**（方案级自定义标点表）走**整体替换**。默认的逐键深合并会让用户
///   在设置页**删不掉方案作者写的那一行**：override 层没有「删除」的表达，写空只能覆盖成
///   空串。失效形态是本仓反复栽的那种——界面上删掉了、保存重开又回来，或界面没了打字还在。
///   整表替换与该字段本身的语义（[`PunctSpec::custom_mappings`]）也一致：方案表是一整份，
///   不与任何东西逐行混。
///
/// ⚠️ 两个例外都按**键名**匹配、与所在路径无关。方案文件里 `custom_mappings` 只出现在
/// `[punct]` 段下；将来若别处再出现同名键，它会一并按整体替换处理——那时要么改成判路径，
/// 要么给新键换个名字。
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

/// `dictionaries` 的 override 合并：**按 `id` 匹配，且只接受 `enabled` 字段**。
///
/// 方案文件是词库结构（顺序/path/label/base_order/type…）的唯一权威，override 层仅记录
/// 用户在设置页翻的开关。这么定的两个原因：
/// 1. 数组整体替换会让 override 冻结整份词库定义——方案升级后新增的词库透不过来、
///    改过的 path 仍指向旧文件、顺序也停在写快照那一刻。
/// 2. 字段白名单顺带**净化历史遗留的整表快照**：老 override 里那些 path/label 副本
///    会被直接忽略，无需单独写迁移代码。
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
mod tests {
    use super::*;

    #[test]
    fn shuangpin_is_supported() {
        let mut s = Schema::default();
        s.engine.engine_type = "pinyin".into();
        s.engine.pinyin.scheme = "shuangpin".into();
        assert!(s.is_supported());
    }

    #[test]
    fn pinyin_spec_ignores_removed_fields() {
        let toml_str = r#"
scheme = "shuangpin"
show_code_hint = true
fuzzy = { enabled = true, zh_z = true }
[shuangpin]
layout = "xiaohe"
"#;
        let spec: PinyinSpec = toml::from_str(toml_str).unwrap();
        assert_eq!(spec.scheme, "shuangpin");
        assert_eq!(spec.shuangpin.layout, "xiaohe");
    }

    /// 截断的两个方向都要成立：**该截的截、不该截的别动**。
    ///
    /// 只测"长的被截短"会让一个恒返回首字符的实现也绿——那是 issue #85 之前的旧行为，
    /// 它的表现是用户配了 `En` 只显示 `E`，而单看截断断言完全正常。
    /// 反过来只测 `En` 不动，则一个"按字符数取 2"的实现也全绿——那正是 issue #85
    /// 报的缺陷：双汉字整个放行、渲染端只能回缩字号到认不出。**两个方向缺一不可。**
    #[test]
    fn icon_label_truncates_by_display_width() {
        assert_eq!(icon_label_trunc("英"), "英", "一个汉字宽 2，恰好满格");
        assert_eq!(icon_label_trunc("En"), "En", "两个拉丁字符合计宽 2，不许动");
        assert_eq!(icon_label_trunc("English"), "En", "超宽截到前两个拉丁字符");
        assert_eq!(
            icon_label_trunc("符号"),
            "符",
            "双汉字宽 4 → 截回首字（#85）"
        );
        assert_eq!(icon_label_trunc("符号表"), "符", "更长也一样只留首字");
        assert_eq!(icon_label_trunc("Ｅｎ"), "Ｅ", "全角字母按 CJK 宽度算");
        assert_eq!(icon_label_trunc("A符"), "A", "混排：A 占 1，再加汉字就超");
    }

    /// 首尾空白必须吃掉：设置页输入框里带出来的空格若原样落进标签，
    /// 会挤掉本就只有 16px 的绘制宽度，且用户在界面上看不出多了个空格。
    #[test]
    fn icon_label_trims_whitespace() {
        assert_eq!(icon_label_trunc("  En  "), "En");
        assert_eq!(icon_label_trunc("   "), "", "全空白视同未配置");
        assert_eq!(icon_label_trunc(""), "");
    }

    /// 回落方向：空 → fallback，非空 → 不许被 fallback 顶掉。
    ///
    /// 后半条是反向对照。缺了它，一个"永远返回 fallback"的实现能让前半条全绿，
    /// 而那种缺陷的表现是用户怎么配都不生效。
    #[test]
    fn icon_label_or_falls_back_only_when_empty() {
        assert_eq!(icon_label_or("", "英"), "英");
        assert_eq!(icon_label_or("   ", "英"), "英");
        assert_eq!(icon_label_or("En", "英"), "En", "配了就用配的");
        assert_eq!(icon_label_or("English", "英"), "En", "截断后仍是配的");
    }

    /// 截断以 **Unicode 标量值**为单位推进，不是 UTF-16 code unit、也不是字节。
    ///
    /// 这条约束 C++ 侧的缓冲容量：宽度上限 2 蕴含**最多 2 个标量值**（每个字符至少
    /// 占 1 宽），而 emoji 在 [`crate::config`] 的粗口径里记 1 宽却占 2 个 wchar，
    /// 故最坏是 4 wchar + NUL = 5，`_inputTypeLabel` 必须 ≥ 5（已改为 8）。
    ///
    /// ⚠️ 这条同时钉住"emoji 记 1 宽"：哪天把 emoji 划进双宽字符，本断言会红，
    /// 提醒去重算那个缓冲的最坏值（会降到 3，缓冲富余但结论要重写）。
    #[test]
    fn icon_label_limit_counts_scalar_values() {
        let two_emoji = icon_label_trunc("🙂🙃🙂");
        assert_eq!(two_emoji.chars().count(), 2, "按标量值计数");
        assert_eq!(
            two_emoji.encode_utf16().count(),
            4,
            "两个标量值可占四个 wchar——C++ 侧缓冲按这个上限取容量"
        );
    }

    // ── 方案级自定义标点表 ──────────────────────────────────────────────

    /// 三态必须由**同一个字段**区分开：不写 = 跟随全局、空表 = 一条都不要、
    /// 非空表 = 整表替换。`None` 与 `Some({})` 混为一谈的话，「本方案不要自定义标点」
    /// 就没法表达，只能退回去加一个方案级 `custom_enabled`。
    #[test]
    fn schema_punct_custom_mappings_three_states() {
        let parse = |s: &str| toml::from_str::<Schema>(s).expect("方案应解析成功").punct;

        assert_eq!(
            parse("[punct]\nmode = \"chinese\"").custom_mappings,
            None,
            "段里不写 custom_mappings ⇒ 跟随全局"
        );
        assert_eq!(
            parse("[punct]\n[punct.custom_mappings]").custom_mappings,
            Some(Default::default()),
            "空表 ⇒ 本方案一条自定义都不要（与「跟随」必须可区分）"
        );

        let filled =
            parse("[punct]\n[punct.custom_mappings]\n\".\" = [\"。\", \"．\", \"。\", \".\"]");
        let table = filled.custom_mappings.expect("非空表应是 Some");
        assert_eq!(
            table.get("."),
            Some(&vec![
                "。".to_string(),
                "．".to_string(),
                "。".to_string(),
                ".".to_string()
            ]),
            "列序契约与全局表一致：[中半, 英全, 中全, 英半]"
        );
    }

    /// ★★ `custom_mappings` 在 override 合并里走**整体替换**，不是深合并。
    ///
    /// 失效形态：深合并（HashMap 逐键）时 override 层没有「删除」的表达 ⇒ 用户在设置页
    /// 删掉方案作者写的行，保存后那行还在。这条测试直接问「作者的行还在不在」，
    /// 而不是问「我写的行生效没有」——后者在两种合并策略下都通过，测不出差别。
    #[test]
    fn custom_mappings_override_replaces_whole_table() {
        let mut base = toml::from_str::<toml::Value>(
            "[punct.custom_mappings]\n\
             \".\" = [\"。\"]\n\
             \",\" = [\"，\"]\n",
        )
        .unwrap();
        // 用户在设置页只留下了 `.` 那一行（删掉了作者写的 `,`）。
        let over =
            toml::from_str::<toml::Value>("[punct.custom_mappings]\n\".\" = [\"·\"]\n").unwrap();
        merge_toml(&mut base, over);

        let table = base["punct"]["custom_mappings"].as_table().unwrap();
        assert_eq!(
            table.get(".").and_then(|v| v[0].as_str()),
            Some("·"),
            "用户改的那行要生效"
        );
        assert!(
            !table.contains_key(","),
            "★ 作者写的 `,` 必须随整表替换被删掉——深合并时它会残留，\
             表现为「设置页删了、保存重开又回来」"
        );
    }

    /// 反向对照：同一份 override，换成**别的**键名就该走深合并（作者的行保留）。
    /// 没有这条的话，把上面那条的整体替换误写成「所有 table 都整体替换」也照样通过，
    /// 而那会连带毁掉 `[engine]` 等所有段的合并语义。
    #[test]
    fn non_custom_mappings_tables_still_deep_merge() {
        let mut base =
            toml::from_str::<toml::Value>("[engine]\ntype = \"codetable\"\nfoo = 1\n").unwrap();
        let over = toml::from_str::<toml::Value>("[engine]\nfoo = 2\n").unwrap();
        merge_toml(&mut base, over);

        assert_eq!(base["engine"]["foo"].as_integer(), Some(2), "覆盖的键生效");
        assert_eq!(
            base["engine"]["type"].as_str(),
            Some("codetable"),
            "未提及的键必须保留——这才是深合并"
        );
    }
}
