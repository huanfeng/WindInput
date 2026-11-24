//! 配置字段注册表（声明式单一真相源）。
//!
//! 每个配置叶子键在此声明其值类型，经测试与 [`Config`](crate::config::Config) 结构体及
//! `data/config.toml` 反向对照。CLI、core 端校验、设置 UI 均由此注册表派生。
//!
//! 只描述 config 类（用户可改配置），运行状态与分发数据不在此（「数据三分准则」见
//! `docs/architecture/config-design-rules.md`）；键名映射见 `docs/config-key-migration.md`。

use crate::config::Config;

/// 配置字段值类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// 布尔。
    Bool,
    /// 整数（usize/i32 等）。
    Int,
    /// 浮点（f32 等）。
    Float,
    /// 任意字符串。
    Str,
    /// 受限字符串（合法值集合）。
    Enum(&'static [&'static str]),
    /// 字符串数组。
    StrList,
    /// 键值映射表（如自定义标点 mappings）。
    ///
    /// 携带键名值域：非空表示键只能取这几个（如 `ui.font.scripts` 的脚本类名），空表示键由用户
    /// 自由命名。值域进注册表，设置端才能用 capability 做守门比对，免得两边各抄一份而漂移。
    Map(&'static [&'static str]),
    /// 结构体数组（如 mix_modes），整体作不透明叶子。
    StructList,
    /// 可按候选窗排布分档的枚举（见 `crate::by_layout::ByLayout`）。
    ///
    /// 值域与 [`Self::Enum`] 相同，但值可以是 `"h:hide v:always"` 标签式或 `{ h = …, v = … }` 表，
    /// 每一档都要落在同一个值域里。独立成变体是因为值域校验要逐档验、叶子展开要在这里停住。
    LayoutEnum(&'static [&'static str]),
}

impl FieldType {
    /// 这个类型的值在写回时是不是**整体一份**——即「写它 ＝ 用新值把旧值整个换掉」。
    ///
    /// 给写盘闸 [`crate::config::ConfigDegradation::blocks_write_back`] 用：整体一份的值，调用方
    /// 发来的必然是「读回来的旧值 ⊕ 本次编辑」，降级时读回的是出厂值，写回就抹掉用户数据；
    /// 标量是显式单值，与种子无关，不该拦。
    ///
    /// 不要只写 `Map(_)`：数组型键（`ui.toolbar.items`、`schema.mix_modes` 等）同样整份发送。
    /// 加新变体时先回答「写它是整体换掉还是只动一个值」。
    pub fn is_whole_value_leaf(&self) -> bool {
        match self {
            Self::Map(_) | Self::StrList | Self::StructList => true,
            // LayoutEnum 与 Enum 同侧：设置端写它时发的是一个标量（标签式字符串）。
            Self::Bool
            | Self::Int
            | Self::Float
            | Self::Str
            | Self::Enum(_)
            | Self::LayoutEnum(_) => false,
        }
    }
}

/// 单个配置字段的声明。
#[derive(Debug, Clone, Copy)]
pub struct ConfigField {
    /// 点分路径，如 `"ui.candidate.layout"`。
    pub key: &'static str,
    /// 值类型。
    pub ty: FieldType,
}

const fn f(key: &'static str, ty: FieldType) -> ConfigField {
    ConfigField { key, ty }
}

use FieldType::{Bool, Enum, Float, Int, LayoutEnum, Map, Str, StrList, StructList};

/// 候选无效按键三策（number_key/select_key/select_char_key 共用）。
const OVERFLOW_VALUES: &[&str] = &["ignore", "commit", "commit_and_input"];

/// 数字键越界策略：三策之外多一档 `clear_and_input`（清空输入并上屏该数字，不顶高亮候选）。
/// 只有数字键有这一档——次/三选键、以词定字键越界时那个字符本可作编码，不存在「丢码出字」的诉求。
const NUMBER_OVERFLOW_VALUES: &[&str] =
    &["ignore", "commit", "commit_and_input", "clear_and_input"];

/// 翻页键翻不动时：只有吞键与「顶高亮候选 + 输出该键符号」两档。
const PAGE_KEY_OVERFLOW_VALUES: &[&str] = &["ignore", "commit_and_input"];

/// 空码（缓冲非空但无候选）时回车/空格怎么处置这串废码：`commit` 上屏原码 / `clear` 丢弃。
/// 见 `docs/design/enter-behavior-clear-semantics.md`。登记成 `Enum` 而非 `Str`，设置端的
/// 守门测试才能比对值域。
const EMPTY_CODE_BEHAVIOR_VALUES: &[&str] = &["commit", "clear"];

/// 上屏文本的换行形式。四态，语义与「为什么必须按应用配」见
/// `app_compat::NewlineStyle`（本常量是它的值域在配置注册表里的镜像）。
///
/// 与 `NewlineStyle` 的变体集必须一致，由 `newline_style_values_match_enum` 钉住。
const NEWLINE_STYLE_VALUES: &[&str] = &["keep", "cr", "lf", "crlf"];

/// 含生僻字的词在检索范围过滤下的处置。语义见 `wind_candidate::RarePhrasePolicy`
/// （本常量是它的值域在配置注册表里的镜像）。
///
/// 与那个枚举的变体集必须一致，由 `wind-coordinator/tests/rare_phrase_contract.rs` 的
/// `registry_values_match_the_policy_enum` 钉住（本 crate 看不见 `wind_candidate`）。
const RARE_PHRASE_VALUES: &[&str] = &["keep", "filter"];

/// 标点键的空码处置，比回车/空格多一态，故不能与 [`EMPTY_CODE_BEHAVIOR_VALUES`] 共用。
/// 行为其实是两根轴，值域只有一维：
///
/// | | 键字符输出 | 键字符吞掉 |
/// |---|---|---|
/// | 废码上屏 | `commit` | （无意义，不设值） |
/// | 废码丢弃 | `clear` | `clear_no_input` |
///
/// 回车/空格的 `clear` 落在吞键格，标点的 `clear` 落在出键格（标点是用户想输入的字符）。
/// 别把 `clear_no_input` 加给回车/空格：它们的 `clear` 本就吞键，会出现两个等价选项。
/// 加值前先确认 `Coordinator::punct_empty_code_policy`（唯一解释器）分了那一支，未知值落到 `commit`。
const PUNCT_EMPTY_CODE_BEHAVIOR_VALUES: &[&str] = &["commit", "clear", "clear_no_input"];

/// `ui.comment.assign.global` 的四格键名。与 [`crate::comment_lib::CommentCells`] 的字段名逐项对齐。
pub(crate) const COMMENT_CELL_KEYS: &[&str] = &[
    "default_vertical",
    "default_horizontal",
    "detail_vertical",
    "detail_horizontal",
];

/// `ui.comment.state` 的值域。与 [`crate::comment_lib::CommentState`] 的 serde 名逐项对齐。
pub(crate) const COMMENT_STATE_VALUES: &[&str] = &["default", "detail"];

/// `ui.comment.switch_key` 的值域。与 [`crate::comment_lib::SwitchKey`] 的 serde 名逐项对齐。
pub(crate) const COMMENT_SWITCH_KEY_VALUES: &[&str] = &[
    "none", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "ctrl",
    "lctrl", "rctrl", "alt", "lalt", "ralt",
];

/// `ui.comment.switch_mode` 的值域。与 [`crate::comment_lib::SwitchMode`] 的 serde 名逐项对齐。
pub(crate) const COMMENT_SWITCH_MODE_VALUES: &[&str] = &["session", "persist", "hold"];

/// `input.alt_commit` 的值域。与 [`crate::config::AltCommit::from_config`] 逐项对齐。
pub(crate) const ALT_COMMIT_VALUES: &[&str] = &["off", "pinyin", "pinyin_plain", "comment"];

/// 码表词频应用策略。
const FREQ_STRATEGY_VALUES: &[&str] = &["top", "step", "position"];

/// 前缀补全参与词频位置提升的范围（按语义单元数判定）。
const PROMOTE_PREFIX_VALUES: &[&str] = &["none", "single", "all"];

/// 英文候选的词频记账码口径（内部配置，不进 GUI）。
const ENGLISH_CODE_SCOPE_VALUES: &[&str] = &["candidate", "input"];

/// `ui.font.scripts` 的合法键名——**文字类别**，不是脚本的全集。
///
/// 与渲染端 `wind_ui::text::script::ScriptClass::key()` 两处表达，由 wind-ui 侧的
/// `script_class_keys_match_config_registry` 钉住。加类别时两处一起改。
pub const FONT_SCRIPT_KEYS: &[&str] = &[
    "latin", "greek", "cyrillic", "cjk", "emoji", "digits", "punct",
];

/// 模式级候选布局意图（`LayoutIntent` 的 serde 形态）。临拼/临英/网址/加词共用；
/// mix / special 的同名字段是 StructList 条目内的属性，不在本注册表单独登记。
const LAYOUT_INTENT_VALUES: &[&str] = &["follow", "vertical", "horizontal"];

/// 「首候选是所打原文」的三档（`RawCandidateMode`）。反序列化侧仍认老的 `bool`（见
/// `de_raw_candidate`），但设置端只写新值域，否则写回一次就把档位降成两档。
const RAW_CANDIDATE_VALUES: &[&str] = &["always", "in_dict", "off"];

/// emoji 扩展的触发范围（`input.emoji.scope`）。值域登记在这里，设置页从同一个数组取值。
const EMOJI_SCOPE_VALUES: &[&str] = &["off", "exact", "all"];
/// emoji 扩展的呈现形态（`input.emoji.show_as`）。理由同 [`EMOJI_SCOPE_VALUES`]。
const EMOJI_SHOW_AS_VALUES: &[&str] = &["after", "tail", "comment"];

/// 全部配置字段声明（单一真相源）。与 [`Config`] 经测试反向对照，保证零漂移。
/// 域划分见 `docs/config-key-migration.md`（不做向后兼容，旧键已弃）。
///
/// # 三态键（`Option<T>`，默认 `None`）刻意不登记
///
/// 它们的出厂值是「键不存在」，不在 `Config::default()` 的序列化键集里，登记即被判多余。
/// 不登记也让 `prune_redundant` 永远不碰用户手写的值（同废弃键与 Map/StructList 子路径）。
static REGISTRY: &[ConfigField] = &[
    // -- schema（方案 + 拼音 + 模式）--
    f("schema.active", Str),
    f("schema.available", StrList),
    f("schema.primary_codetable", Str),
    f("schema.primary_pinyin", Str),
    f("schema.keep_all_loaded", Bool),
    f(
        "schema.dict_cache_policy",
        Enum(&["lock", "preload", "off"]),
    ),
    // 全局码表（公共基线；方案经 schema_overrides 覆盖）
    f("schema.codetable.top_code_commit", Bool),
    f("schema.codetable.clear_on_empty_max", Bool),
    f("schema.codetable.auto_commit_at_full", Bool),
    f("schema.codetable.auto_commit_min_len", Int),
    f("schema.codetable.punct_commit", Bool),
    f("schema.codetable.show_code_hint", Bool),
    f("schema.codetable.single_code_input", Bool),
    f("schema.codetable.single_code_complete", Bool),
    f("schema.codetable.short_code_yield_level", Int),
    // 单字输入（码表侧）。与 `input.filter_mode` 是两根正交的轴：那个按字符常用度裁剪，
    // 这个按候选长度裁剪。⚠️ 拼音另有一份（`schema.pinyin.single_char`），刻意不共享取值。
    // 方案级同名覆盖是 `Option<bool>`（不写＝跟随全局），值域与本处同为布尔。
    f("schema.codetable.single_char", Bool),
    f("schema.codetable.z_key_repeat", Bool),
    // 英文候选混入（码表方案）。混输走 schema.mix 那三项、拼音走 schema.pinyin
    // 同名段，三者互不接管，见 `CodetableEnglishMerge` 文档。
    f("schema.codetable.english_merge.enable", Bool),
    f("schema.codetable.english_merge.min_length", Int),
    f("schema.codetable.english_merge.block_commit", Bool),
    f("schema.codetable.english_merge.complete_min_length", Int),
    f("schema.codetable.english_merge.block_commit_on_word", Bool),
    f("schema.codetable.english_merge.exact_position", Int),
    f("schema.codetable.english_merge.complete_position", Int),
    f("schema.codetable.english_merge.max_count", Int),
    // 带参数的值域（`mix:<id>` / `special:<id>`）故用 Str 而非 Enum；解析与校验见 `BoundAction`。
    f("schema.codetable.z_key_action", Str),
    // 通配输入（万能键）。键是单个字面字符，值域无法枚举故用 Str；合法性见 `parse_wildcard_key`。
    f("schema.codetable.wildcard", Bool),
    f("schema.codetable.wildcard_key", Str),
    f("schema.codetable.wildcard_single_only", Bool),
    f("schema.codetable.sentence_count", Int),
    f("schema.codetable.sentence_max_count", Int),
    f("schema.codetable.sentence_cycle_key", Str),
    f("schema.codetable.sentence_grammar_weight", Float),
    // 码元字符集：范围+字面的自由文本（如 `a-x/`、`a-z0-9`），值域无法枚举故用 Str；
    // 解析与非法回落见 `CodeCharSet`。空 = 内置默认 `a-z`。
    f("schema.codetable.input_chars", Str),
    f("schema.codetable.leading_chars", Str),
    f("schema.codetable.frequency.enabled", Bool),
    f("schema.codetable.frequency.record_always", Bool),
    f("schema.codetable.frequency.protect_top_n", Int),
    f("schema.codetable.frequency.protect_top_n_len1", Int),
    f("schema.codetable.frequency.protect_top_n_len2", Int),
    f("schema.codetable.frequency.protect_top_n_len3", Int),
    f(
        "schema.codetable.frequency.strategy",
        Enum(FREQ_STRATEGY_VALUES),
    ),
    f(
        "schema.codetable.frequency.promote_prefix",
        Enum(PROMOTE_PREFIX_VALUES),
    ),
    f("schema.codetable.frequency.half_life", Float),
    // ── 英文（[schema.english]）：不再共用码表段 ──
    f("schema.english.frequency.enabled", Bool),
    f("schema.english.frequency.record_always", Bool),
    f(
        "schema.english.frequency.strategy",
        Enum(FREQ_STRATEGY_VALUES),
    ),
    f(
        "schema.english.frequency.promote_prefix",
        Enum(PROMOTE_PREFIX_VALUES),
    ),
    f("schema.english.frequency.half_life", Float),
    f(
        "schema.english.frequency.code_scope",
        Enum(ENGLISH_CODE_SCOPE_VALUES),
    ),
    f("schema.english.commit_space", Bool),
    f("schema.english.raw_candidate", Enum(RAW_CANDIDATE_VALUES)),
    f("schema.english.case_variants", Bool),
    f("schema.english.case_follow_input", Bool),
    f("schema.english.phrase_seg", Bool),
    f("schema.english.auto_learn.enabled", Bool),
    f("schema.english.auto_learn.promote_count", Int),
    f("schema.english.auto_learn.temp_max_entries", Int),
    f("schema.codetable.auto_phrase.enabled", Bool),
    f("schema.codetable.auto_phrase.min_phrase_len", Int),
    f("schema.codetable.auto_phrase.max_phrase_len", Int),
    f("schema.codetable.auto_phrase.promote_count", Int),
    f("schema.codetable.auto_phrase.idle_timeout_ms", Int),
    f("schema.codetable.auto_phrase.temp_max_entries", Int),
    f("schema.codetable.auto_phrase.draft_ttl_hours", Int),
    f("schema.codetable.auto_phrase.draft_max_entries", Int),
    f("schema.codetable.auto_phrase.draft_flush_batch", Int),
    // 全局拼音
    f("schema.pinyin.use_smart_compose", Bool),
    // 用户词进整句词图。出厂 true，接用户词与临时词（不接草稿层）。只在 use_smart_compose
    // 开着时有效，UI 侧 enabled_when 挂在它上面。
    f("schema.pinyin.sentence_uses_user_words", Bool),
    // 整句 N-best：露几条 / 算几条（切换键的池子）。出厂 1/1 = 单条整句。
    f("schema.pinyin.sentence_count", Int),
    f("schema.pinyin.sentence_max_count", Int),
    // 整句切换键：键即开关，空串 = 关闭（出厂）。只在整句池 ≥ 2 条时夺取。
    f("schema.pinyin.sentence_cycle_key", Str),
    f("schema.pinyin.separator", Str),
    // 单字输入（拼音侧）。与码表那份是两件独立的事、不共享取值——两种引擎对「只出单字」
    // 的诉求本就不同（码表是定长盲打，拼音只是不出词）。拼音**没有**方案级覆盖。
    f("schema.pinyin.single_char", Bool),
    // 英文候选混入（拼音方案）。**没有 block_commit**：那一项否决的是满码自动上屏/顶码，
    // 拼音方案两者都没有，登记它等于教用户去配一件不成立的事。见 `PinyinEnglishMerge`。
    f("schema.pinyin.english_merge.enable", Bool),
    f("schema.pinyin.english_merge.min_length", Int),
    f("schema.pinyin.english_merge.complete_min_length", Int),
    f("schema.pinyin.english_merge.exact_position", Int),
    f("schema.pinyin.english_merge.complete_position", Int),
    f("schema.pinyin.english_merge.max_count", Int),
    f("schema.pinyin.fuzzy.enabled", Bool),
    f("schema.pinyin.fuzzy.zh_z", Bool),
    f("schema.pinyin.fuzzy.ch_c", Bool),
    f("schema.pinyin.fuzzy.sh_s", Bool),
    f("schema.pinyin.fuzzy.n_l", Bool),
    f("schema.pinyin.fuzzy.f_h", Bool),
    f("schema.pinyin.fuzzy.r_l", Bool),
    f("schema.pinyin.fuzzy.an_ang", Bool),
    f("schema.pinyin.fuzzy.en_eng", Bool),
    f("schema.pinyin.fuzzy.in_ing", Bool),
    f("schema.pinyin.fuzzy.ian_iang", Bool),
    f("schema.pinyin.fuzzy.uan_uang", Bool),
    f("schema.pinyin.fuzzy.un_ong", Bool),
    f("schema.pinyin.fuzzy.eng_ong", Bool),
    f("schema.pinyin.frequency.enabled", Bool),
    f("schema.pinyin.frequency.record_always", Bool),
    f("schema.pinyin.frequency.half_life", Float),
    f("schema.pinyin.frequency.base_scale", Float),
    f("schema.pinyin.frequency.recency_peak", Float),
    f(
        "schema.pinyin.frequency.promote_prefix",
        Enum(PROMOTE_PREFIX_VALUES),
    ),
    f("schema.pinyin.auto_learn.enabled", Bool),
    f("schema.pinyin.auto_learn.min_word_length", Int),
    f("schema.pinyin.auto_learn.max_word_length", Int),
    f("schema.pinyin.auto_learn.promote_count", Int),
    f("schema.pinyin.completion.min_syllables", Int),
    f("schema.pinyin.completion.max_extra_syllables", Int),
    f("schema.pinyin.abbrev.enabled", Bool),
    // 上下文语言模型（n-gram）。enabled=false 或 weight=0 时都不加载模型文件，整句结果与没有该功能时逐位相同。
    f("schema.pinyin.grammar.enabled", Bool),
    f("schema.pinyin.grammar.weight", Float),
    f("schema.pinyin.grammar.model", Str),
    // 双拼下的全拼降级输入（多人共用机器）。非双拼方案无效，混输次引擎强制关闭。
    // 落在 [schema.pinyin.shuangpin] 子段：它只对双拼有意义，不该混在「所有拼音方案共用」
    // 的顶层里（与 fuzzy/frequency/completion 同为子段）。
    f("schema.pinyin.shuangpin.allow_full_pinyin", Bool),
    // 辅助码（字形二次筛选）的全局基线。**出厂关闭**；方案段 `[engine.aux_code]` 可用
    // 同名字段 tri-state 覆盖（`AuxCodeGlobal::resolved`），故「双拼开、全拼关」表达得出
    // ——全拼的反引号出厂已被音节分隔符占用，绑了也进不去，两个方案必须能分别开关。
    // `files`（码表清单）不在此：它是方案属性，只住在 `[engine.aux_code]` 里。
    f("schema.pinyin.aux_code.enabled", Bool),
    f("schema.pinyin.aux_code.max_phrase_len", Int),
    // 直接辅助码（仅双拼，任一音节后 1～2 位自动当辅码）。出厂关；只在 enabled 也生效时起作用。
    // 方案段另有两个内部项不登记在此（不进设置页、无全局键）：逃生口 `continuous` 与编码栏
    // 分隔符 `preedit_separator`，见 `AuxCodeSpec`。
    f("schema.pinyin.aux_code.direct", Bool),
    // 全局混输（融合策略）
    f("schema.mix.show_source_hint", Bool),
    f("schema.mix.enable_english", Bool),
    f("schema.mix.pinyin_only_overflow", Bool),
    f("schema.mix.top_code_override_pinyin", Bool),
    f("schema.mix.auto_commit_block_on_pinyin", Bool),
    f("schema.mix.auto_commit_block_on_english", Bool),
    f("schema.mix.block_commit_on_english_word", Bool),
    f("schema.mix.min_pinyin_length", Int),
    f("schema.mix.min_english_length", Int),
    f("schema.mix.english_complete_min_length", Int),
    f("schema.mix.english_exact_position", Int),
    f("schema.mix.english_complete_position", Int),
    f("schema.mix.english_max_count", Int),
    f("schema.mix.block_commit_on_pinyin_word", Bool),
    f("schema.mix.pinyin_word_min_weight", Int),
    f("schema.mix.enable_pinyin_abbrev", Bool),
    f("schema.mix.pinyin_partial_candidates", Bool),
    f("schema.mix.pinyin_partial_candidates_overflow", Bool),
    // 快捷输入：各候选来源的开关与优先级在 schema.mix_modes 的 members 里（有无=开关，
    // 顺序=优先级）；总开关＝把 quick_mix 的 trigger_keys 清空。此处只有全局行为项。
    f("schema.quick_input.decimal_places", Int),
    f("schema.quick_input.history_max", Int),
    // 强制竖排原在此（force_vertical）。它其实是 quick_mix **实例**的显示属性，已迁往
    // mix_modes[].candidate_layout；per-instance 字段由 StructList 条目承载，不另立项
    // （一个配置键只能有一个 manifest 项）。
    f("schema.mix_modes", StructList),
    // 跨引擎的词频公共基线。与 schema.{codetable,pinyin,english}.frequency 分工见
    // `FrequencyGlobal`：那三段是各引擎的调频参数，本段是三个引擎都照办的同一条规则。
    // 值域为区块名或预设组名（"emoji"），故是 StrList 而非 Enum——块表会随 Unicode 增长。
    f("schema.frequency.exclude_blocks", StrList),
    // -- input（输入行为）--
    f("input.filter_mode", Str),
    // 含生僻字的词要不要吃 filter_mode 那一刀（与它正交，不是它的第四个档）。
    f("input.rare_phrase", Enum(RARE_PHRASE_VALUES)),
    // 上屏换行形式的全局默认档；per-app 覆盖在 compat.toml 的 [[commit_newline]] 段，
    // 不在本注册表（那是另一份文件的 schema）。
    f("input.commit_newline", Enum(NEWLINE_STYLE_VALUES)),
    f("input.english_case_cycle_key", Str),
    f("input.alt_commit", Enum(ALT_COMMIT_VALUES)),
    // 检索范围放宽（智能档增强，见 docs/design/smart-filter-scope-relax.md）
    f("input.scope_relax.page_end_key", Bool),
    f("input.scope_relax.prefix", Str),
    f("input.scope_relax.always_append", Bool),
    f("input.correction.enabled", Bool),
    f("input.correction.marker", Str),
    f("input.correction.custom", StrList),
    f("input.correction.rules.gn_ng", Bool),
    f("input.correction.rules.on_ong", Bool),
    f("input.correction.rules.vowel_swap", Bool),
    f("input.correction.rules.legacy_spelling", Bool),
    f("input.correction.rules.h_misplaced", Bool),
    f("input.correction.rules.mg_ng", Bool),
    f("input.correction.rules.n_before_g", Bool),
    f("input.correction.rules.ai_ia", Bool),
    f("input.correction.hint.enabled", Bool),
    f("input.correction.hint.fuzzy", Bool),
    f("input.correction.hint.misread", Bool),
    f("input.correction.hint.correction", Bool),
    f("input.correction.hint.tone", Bool),
    f(
        "input.correction.hint.scope",
        Enum(crate::config::HintScope::VALUES),
    ),
    f("input.enter_behavior", Enum(EMPTY_CODE_BEHAVIOR_VALUES)),
    f(
        "input.space_on_empty_behavior",
        Enum(EMPTY_CODE_BEHAVIOR_VALUES),
    ),
    f(
        "input.punct_on_empty_behavior",
        Enum(PUNCT_EMPTY_CODE_BEHAVIOR_VALUES),
    ),
    f("input.buffer_symbol_chars", Str),
    f("input.numpad_behavior", Str),
    f("input.numpad_half_width", Bool),
    // 启动默认状态（原 general 域）
    f("input.default.remember_last_state", Bool),
    f("input.default.state_scope", Enum(&["global", "app"])),
    f("input.default.chinese_mode", Bool),
    f("input.default.full_width", Bool),
    f("input.default.chinese_punct", Bool),
    f("input.punct.follow_mode", Bool),
    f("input.punct.smart_after_digit", Bool),
    f("input.punct.smart_list", Str),
    f("input.punct.smart_after_latin", Bool),
    f("input.punct.latin_smart_list", Str),
    f("input.punct.custom_enabled", Bool),
    f("input.punct.custom_mappings", Map(&[])),
    f("input.spacing.cjk_latin", Bool),
    f("input.spacing.cjk_digit", Bool),
    f("input.symbol.smart_mode", Bool),
    f("input.symbol.smart_timeout_ms", Int),
    f("input.symbol.smart_chars", Str),
    f(
        "input.symbol.smart_method",
        Enum(&["delete_replace", "hold_composition"]),
    ),
    f("input.symbol.english_punct_mode", Bool),
    f("input.symbol.english_mode", Bool),
    f("input.symbol.english_chars", Str),
    f("input.auto_pair.chinese", Bool),
    f("input.auto_pair.english", Bool),
    f("input.auto_pair.chinese_pairs", StrList),
    f("input.auto_pair.english_pairs", StrList),
    f("input.auto_pair.jump_out_keys", StrList),
    f("input.auto_pair.state_ttl_secs", Int),
    f("input.auto_pair.commit_via_composition", Bool),
    f("input.temp_english.enabled", Bool),
    f("input.temp_english.show_candidates", Bool),
    f(
        "input.temp_english.shift_behavior",
        Enum(&["temp_english", "direct_commit"]),
    ),
    f(
        "input.temp_english.shift_when_composing",
        Enum(&["buffer", "commit_enter"]),
    ),
    f("input.temp_english.trigger_keys", StrList),
    f("input.temp_english.allow_symbols", Bool),
    f("input.temp_english.symbol_chars", Str),
    f("input.temp_english.space_as_input", Bool),
    f(
        "input.temp_english.raw_candidate",
        Enum(RAW_CANDIDATE_VALUES),
    ),
    f("input.temp_english.commit_space", Bool),
    f("input.temp_english.phrase_seg", Bool),
    // 候选窗定位方式（内部配置，不进设置页 —— 见 wind-setting 的 uncovered allowlist）。
    f("input.caret.add_word_via_composition", Bool),
    f("input.caret.temp_pinyin_via_composition", Bool),
    f("input.caret.special_via_composition", Bool),
    f("input.caret.rare_char_via_composition", Bool),
    f("input.temp_english.case_variants", Bool),
    f("input.temp_english.case_follow_input", Bool),
    f(
        "input.temp_english.candidate_layout",
        Enum(LAYOUT_INTENT_VALUES),
    ),
    f("input.capslock.cancel_on_mode_switch", Bool),
    f(
        "input.capslock.mode_after_cancel",
        Enum(&["chinese", "english", "toggle"]),
    ),
    f("input.capslock.commit_upper", Bool),
    f("input.temp_pinyin.enabled", Bool),
    f("input.temp_pinyin.trigger_keys", StrList),
    f("input.temp_pinyin.hotkey", Str),
    f(
        "input.temp_pinyin.candidate_layout",
        Enum(LAYOUT_INTENT_VALUES),
    ),
    f("input.url.enabled", Bool),
    f("input.url.prefixes", StrList),
    // 网址历史：独立于 url.enabled 的第二道开关（前者只改输入行为，后者开始落盘用户
    // 打过的网址原文）。理由见 `UrlConfig::history_enabled`。
    f("input.url.history_enabled", Bool),
    f("input.url.history_max", Int),
    // 邮箱模式。与 url 共用补全候选源，但触发判据是**后缀**（`@`）而非前缀。
    f("input.email.enabled", Bool),
    f("input.email.suffixes", StrList),
    // Unicode 码点输入（`u+4e00` → 一）。与 url 同为前缀夺取式模式，故字段形态一致。
    f("input.unicode.enabled", Bool),
    f("input.unicode.prefixes", StrList),
    // 生僻字模式。刻意没有 enabled——不绑键即进不去，见 `RareCharConfig`。
    // trigger_keys 与其余五处同：normalize 折算进 keys.key_actions 后清空，
    // 留在此处是**出厂声明处** + 设置端 key_action 控件的标识符。
    f("input.rare_char.trigger_keys", StrList),
    f("input.rare_char.include_blocks", StrList),
    // 反查模式。进入方式只走 key_actions（动词 reverse），出厂不绑键；没有总开关（绑键即可用）。
    f("input.reverse.candidate_layout", Enum(LAYOUT_INTENT_VALUES)),
    f("input.reverse.lookup_disabled_dicts", Bool),
    f("input.reverse.clip_max_chars", Int),
    // Emoji 候选扩展（按候选文本查表追加，与编码域无关 ⇒ 全方案通用，故在 input 而非 schema）。
    f("input.emoji.enabled", Bool),
    f("input.emoji.scope", Enum(EMOJI_SCOPE_VALUES)),
    f("input.emoji.show_as", Enum(EMOJI_SHOW_AS_VALUES)),
    f("input.emoji.max_per_word", Int),
    f("input.emoji.max_hosts", Int),
    f("input.emoji.min_word_chars", Int),
    f("input.emoji.categories", Bool),
    f("input.emoji.block_auto_commit", Bool),
    // 附加模块开关（id → bool）。键由内置注册表与用户放进 `emoji/` 的文件共同决定，不是
    // 固定集合，故整体登记为 Map；设置端由对话框整体读写。
    f("input.emoji.modules", Map(&[])),
    f("input.url.candidate_layout", Enum(LAYOUT_INTENT_VALUES)),
    f("input.email.candidate_layout", Enum(LAYOUT_INTENT_VALUES)),
    f("input.unicode.candidate_layout", Enum(LAYOUT_INTENT_VALUES)),
    f(
        "input.add_word.candidate_layout",
        Enum(LAYOUT_INTENT_VALUES),
    ),
    f("input.s2t.enabled", Bool),
    f("input.s2t.variant", Str),
    f("input.t2s.enabled", Bool),
    f("input.cmdbar.enabled", Bool),
    f("input.cmdbar.candidate_prefix", Str),
    f("input.cmdbar.backspace_collapse_group", Bool),
    f("input.cmdbar.group_per_page", Int),
    // 短语前缀列举（原 dict.phrase）
    f("input.phrase.prefix_match", Bool),
    f("input.phrase.min_prefix", Int),
    // 顶码上屏策略
    f(
        "input.top_commit_mode",
        Enum(&["pre_confirm", "direct_commit"]),
    ),
    // 顶屏类上屏是否造词 + 推 6b（内部配置）
    f("input.top_commit_learn", Bool),
    // 密码框强制英文（内部配置，入口是高级菜单）
    f("input.password_force_english", Bool),
    // 忽略软件写 compartment 引起的中英切换（按应用覆盖见 compat.toml 同名字段）
    f("input.ignore_host_ime_switch", Bool),
    // 联想。kind 兼任开关与类型（"off" 即关）。本段是**桌面基线**，移动端的差异走
    // [mobile.association]——值域里不留平台哨兵。
    f("input.association.kind", Enum(&["off", "word", "smart"])),
    f("input.association.mode", Enum(&["one_shot", "continuous"])),
    f("input.association.max_count", Int),
    f("input.association.space_commits", Bool),
    f("input.association.select_keys_commit", Bool),
    f("input.association.enter_cancels_only", Bool),
    f("input.association.backspace_cancels_only", Bool),
    f("input.association.hide_after_ms", Int),
    f("input.association.hint", Str),
    f("input.association.history", Bool),
    f("input.association.bigram", Bool),
    f("input.association.prefix", Bool),
    f("input.association.punct", Bool),
    // -- keys（全部按键，扁平；overflow 保留一层）--
    f("keys.toggle_mode_keys", StrList),
    f("keys.commit_on_switch", Bool),
    // Ctrl+空格 中英切换开关。消费端读 `KeysConfig::ctrl_space_toggle_effective`。
    f("keys.ctrl_space_toggle", Bool),
    f("keys.switch_engine", Str),
    f("keys.toggle_full_width", Str),
    f("keys.toggle_punct", Str),
    f("keys.toggle_toolbar", Str),
    f("keys.open_settings", Str),
    f("keys.open_dictionary", Str),
    f("keys.add_word", Str),
    f("keys.open_add_word_dialog", Str),
    f("keys.toggle_s2t", Str),
    f("keys.toggle_t2s", Str),
    f("keys.activate_ime", Str),
    f("keys.pin_candidate", Str),
    f("keys.delete_candidate", Str),
    f("keys.quick_input_select", Str),
    f("keys.take_screenshot", Str),
    f("keys.softkeyboard", Str),
    f("keys.global_hotkeys", StrList),
    // `keys.schema_hotkeys` 已废弃、不登记：改写进 key_actions（`switch_schema:<id>`），无兼容
    // 折算。serde 字段 `legacy_schema_hotkeys` 只为读出残留值告警；不登记让 `config.setItems`
    // 对旧键回 skipped 而不是静默成功。
    f("keys.key_actions", Map(&[])),
    f("keys.session_actions", Map(&[])),
    f("keys.select_key_groups", StrList),
    f("keys.page_keys", StrList),
    f("keys.highlight_keys", StrList),
    f("keys.select_char_keys", StrList),
    f("keys.overflow.number_key", Enum(NUMBER_OVERFLOW_VALUES)),
    f("keys.overflow.select_key", Enum(OVERFLOW_VALUES)),
    f("keys.overflow.select_char_key", Enum(OVERFLOW_VALUES)),
    f("keys.overflow.page_key", Enum(PAGE_KEY_OVERFLOW_VALUES)),
    // 按键级覆盖（键名 → 策略），整体一张表；设置端编辑控件另案，先用配置文件。
    // -- ui（外观）--
    f("ui.candidate.per_page", Int),
    f("ui.candidate.per_page_extended", Int),
    f("ui.candidate.layout", Enum(&["horizontal", "vertical"])),
    f(
        "ui.candidate.preedit_display",
        Enum(&["app_inline", "candidate_top", "candidate_inline"]),
    ),
    f("ui.candidate.syllable_separator", Str),
    f("ui.candidate.hide_window", Bool),
    // 候选窗首显策略的全局默认档。per-app 覆盖走 compat.toml（右键菜单管理），
    // 那里的「跟随全局」一档就是回到本键。
    f(
        "ui.candidate.first_show_mode",
        Enum(&["fast", "wait", "instant"]),
    ),
    // 首显策略的三个内部选项（不进设置页，仅 config.toml / CLI 可调）。
    // 注册到此表是必须的：registry_covers_every_config_key 强制全键覆盖，
    // 漏注册的键会静默无法经 CLI/RPC 读写。
    f("ui.candidate.first_show_settle_ratio", Float),
    f("ui.candidate.fast_typing_window_ms", Int),
    f("ui.candidate.fast_first_show_fallback_ms", Int),
    f("ui.candidate.font_size", Float),
    // 这两项可按候选窗排布分档（`"h:hide v:always"`），故是 LayoutEnum 而非 Enum——
    // 值域不变，只是每一档各过一遍。
    f(
        "ui.candidate.pager_bar_display",
        LayoutEnum(&["", "hide", "auto", "always"]),
    ),
    f(
        "ui.candidate.page_number_display",
        LayoutEnum(&["", "show", "hide"]),
    ),
    f("ui.candidate.max_chars", Int),
    f("ui.candidate.min_window_width_horizontal", Int),
    f("ui.candidate.min_window_width_vertical", Int),
    f("ui.candidate.min_window_height_horizontal", Int),
    f("ui.candidate.min_window_height_vertical", Int),
    f("ui.candidate.min_rows", Int),
    f("ui.candidate.grid_expand", Bool),
    f("ui.candidate.grid_rows", Int),
    f("ui.candidate.grid_cell_max_chars", Int),
    f(
        "ui.candidate.grid_index_fill",
        Enum(&["rows", "cells", "off"]),
    ),
    f("ui.candidate.grid_pageupdown_screen", Bool),
    f("ui.candidate.grid_categories", StrList),
    f("ui.candidate.comment_max_chars_vertical", Int),
    f("ui.candidate.comment_max_chars_horizontal", Int),
    f("ui.candidate.comment_above", Bool),
    f("ui.candidate.auto_comment_dicts", Bool),
    f("ui.comment_dicts", StructList),
    f("ui.comment.enabled", Bool),
    f("ui.comment.state", Enum(COMMENT_STATE_VALUES)),
    f("ui.comment.switch_key", Enum(COMMENT_SWITCH_KEY_VALUES)),
    f("ui.comment.switch_mode", Enum(COMMENT_SWITCH_MODE_VALUES)),
    f("ui.comment.templates", StructList),
    f("ui.comment.assign.global", Map(COMMENT_CELL_KEYS)),
    f("ui.comment.assign.type", Map(&[])),
    f("ui.comment.assign.schema", Map(&[])),
    f("ui.comment.assign.mode", Map(&[])),
    f("ui.candidate.index_labels", StrList),
    f("ui.candidate.flip_when_above", Bool),
    f("ui.candidate.swap_preedit_when_above", Bool),
    f("ui.candidate.pager_in_preedit", Bool),
    f("ui.candidate.double_click_screenshot", Bool),
    f(
        "ui.candidate.position_mode",
        Enum(&["follow_caret", "fixed"]),
    ),
    f("ui.candidate.offset_x", Int),
    f("ui.candidate.offset_y", Int),
    f("ui.candidate.shadow", Enum(&["follow", "off"])),
    f("ui.candidate.custom_x", Int),
    f("ui.candidate.custom_y", Int),
    f("ui.font.family", Str),
    f("ui.font.path", Str),
    f("ui.font.render_mode", Enum(&["directwrite", "gdi"])),
    f("ui.font.fallback", StrList),
    f("ui.font.weight", Int),
    // 脚本类名 → 该类的字体链。登记为 Map（叶子、不下钻）：`latin`/`cjk` 这些是**数据**
    // 不是配置项，下钻会把 `ui.font.scripts.latin` 当成注册表键去比对。同 `keys.key_actions`。
    f("ui.font.scripts", Map(FONT_SCRIPT_KEYS)),
    f("ui.theme.name", Str),
    f("ui.theme.style", Str),
    f("ui.theme.animation", Bool),
    f("ui.mode_indicator.style", Enum(&["short", "full", "none"])),
    f("ui.tooltip.enabled", Bool),
    f("ui.tooltip.delay", Int),
    f("ui.tooltip.max_chars", Int),
    f("ui.tooltip.wrap_width", Int),
    // 悬停提示段列表：结构体数组，整体作不透明叶子（同 ui.langbar.badges）。
    // 段字段（label/template/each/promote/inline/enabled）是条目属性，不单独登记。
    f("ui.tooltip.sections", StructList),
    f("ui.status.enabled", Bool),
    f("ui.status.duration", Int),
    f("ui.status.display_mode", Enum(&["temp", "always"])),
    f("ui.status.show_on_focus", Bool),
    f("ui.status.schema_name_style", Enum(&["full", "short"])),
    f(
        "ui.status.position_mode",
        Enum(&crate::app_compat::STATUS_POSITION_MODES),
    ),
    f(
        "ui.status.fallback_position",
        Enum(&crate::app_compat::STATUS_FALLBACK_POSITIONS),
    ),
    f("ui.status.caret_side", Enum(&["below", "above"])),
    f("ui.status.offset_x", Int),
    f("ui.status.offset_y", Int),
    f("ui.status.custom_x", Int),
    f("ui.status.custom_y", Int),
    f("ui.status.items", StrList),
    f("ui.toolbar.visible", Bool),
    f("ui.toolbar.hide_in_fullscreen", Bool),
    f("ui.toolbar.fullscreen_watch", Bool),
    f("ui.toolbar.hide_in_english", Bool),
    f("ui.toolbar.auto_hide", Bool),
    f("ui.toolbar.auto_hide_delay", Int),
    f("ui.toolbar.auto_hide_hover_reveal", Bool),
    f("ui.toolbar.vertical", Bool),
    // items 的**顺序即渲染顺序**（不同于 ui.status.items 的顺序无语义）。StrList 不带值域
    // 校验，非法项由协调器的 `parse_toolbar_items` 跳过并告警——那里同时要处理顺序，
    // 值域与顺序在一处判定才不会各说各话。
    f("ui.toolbar.items", StrList),
    // 自定义按钮：结构体数组，整体作不透明叶子（同 ui.comment_dicts / schema.mix_modes）。
    // 出厂空，且**不写进 data/config.toml**——写出一行 `buttons = []` 没有任何信息量，
    // 却会把空数组冻结成 L2 值。已在豁免名单登记，理由同 ui.comment_dicts。
    f("ui.toolbar.buttons", StructList),
    // -- ui.langbar（Windows 任务栏输入指示器图标）--
    // badge 用 Enum 而非 Str：写错一个词只会静默回落「不显示」，而"配了没反应"
    // 是最难自查的一类；登记成员后 `config set` 与设置页都能先一步挡下。
    f("ui.langbar.badge", Enum(&["none", "corner"])),
    f("ui.langbar.badge_scale", Float),
    f("ui.langbar.badge_alpha", Float),
    // 角标规则表（顺序即优先级）。条目内的字段（state / corner / color_light …）
    // 是 StructList 条目的属性，不在本注册表单独登记，同 schema.mix_modes。
    f("ui.langbar.badges", StructList),
    // 主字色：Str，"" = 跟随主题（R3 空串哨兵）。色值合法性由协调器解析时判定并回落。
    f("ui.langbar.text_color_cn_light", Str),
    f("ui.langbar.text_color_cn_dark", Str),
    f("ui.langbar.text_color_en_light", Str),
    f("ui.langbar.text_color_en_dark", Str),
    // -- ui.labels（非中文态的图标主字；中文态在方案文件的 [schema] icon_label）--
    // 类型是 Str 而非 Enum：值域是"任意 ≤2 字符"，不是一组枚举值。上限由
    // `wind_config::schema::icon_label_trunc` 在读取侧统一截断，不在这里表达——
    // 注册表只描述类型，长度约束若也写一份就成了第二个真相源。
    f("ui.labels.english", Str),
    f("ui.labels.caps_lock", Str),
    // -- stats（统计，原 features.stats 升顶级）--
    f("stats.enabled", Bool),
    f("stats.track_english", Bool),
    f("stats.speed_factor", Float),
    // -- extensions（扩展程序）--
    f("extensions.enabled", Bool),
    f("extensions.list", StructList),
    // -- debug（调试）--
    // `off` = 整个日志功能关闭：主日志文件不创建，连 `startup_stage.log` 也不写
    // （见 startup_trace::disabled）。它排在最前是因为值域按"由关到详"排。
    f(
        "debug.log_level",
        Enum(&["off", "error", "warn", "info", "debug", "trace"]),
    ),
    f("debug.log_max_size_mb", Int),
    f("debug.log_max_files", Int),
    // -- system（与操作系统集成的方式：注册名等，需管理员权限才能落地）--
    f("system.dota2_compat", Bool),
    // 自由文本：值域是「Dota 2 白名单里的某一条」，那张表在游戏二进制里、会随游戏更新变，
    // 枚举不了也不该由我们替用户收窄——故 Str 而非 Enum。清洗与长度校验在写注册表那侧。
    f("system.dota2_compat_name", Str),
    // -- mobile（移动端对上面各域的覆盖；桌面构建完全无视）--
    // 只登记**当前真有平台差异**的键，别提前铺一排永远等于基线的键——每个都要在这里、
    // 预置文件、capability 快照、设置页豁免名单各占一行。详见 MobileConfig 的文档。
    f("mobile.association.kind", Enum(&["off", "word", "smart"])),
    f("mobile.association.mode", Enum(&["one_shot", "continuous"])),
    f("mobile.association.punct", Bool),
    f("mobile.association.space_commits", Bool),
    f("mobile.association.select_keys_commit", Bool),
    f("mobile.association.enter_cancels_only", Bool),
    f("mobile.association.backspace_cancels_only", Bool),
    f("mobile.grammar.enabled", Bool),
    f("mobile.grammar.weight", Float),
    f("mobile.grammar.model", Str),
];

/// 返回配置字段注册表。
pub fn registry() -> &'static [ConfigField] {
    REGISTRY
}

/// 按点分路径查注册表条目（未登记返回 None）。
pub fn field(key: &str) -> Option<&'static ConfigField> {
    REGISTRY.iter().find(|f| f.key == key)
}

/// [`field`] 加上 [`CommentDynKey`] 的动态键。**只有 `config get/set` 与 cmdbar `config.*`
/// 这类「单项按名字读写」的入口用它**；`field` / `is_known_key` 保持只查静态表——动态键在
/// `Config` 结构体里没有同名字段，被 `config check`、patch、剪枝、设置端写回等一并认作
/// 「已登记」会让它们对一个并不存在的路径做出错误判断。
pub fn addressable_field(key: &str) -> Option<&'static ConfigField> {
    field(key).or_else(|| comment_dyn_key(key).map(CommentDynKey::field))
}

/// [`addressable_field`] 是否命中。
pub fn is_addressable_key(key: &str) -> bool {
    addressable_field(key).is_some()
}

/// `ui.comment` 下两类**按名字寻址**的动态键，不在静态注册表里（名字是用户数据），
/// 但可经 `config get/set` 与命令直通车 `config.get/config.set` 单项读写：
///
/// - 分配的一格：`ui.comment.assign.global.<格>`，或
///   `ui.comment.assign.<type|schema|mode>.<名>.<格>`；`<格>` 取 [`COMMENT_CELL_KEYS`]，
///   值为模板 id / `none` / `inherit`，**空串＝删除这一格**（回到下一层）。
/// - 模板文本：`ui.comment.template.<id>`，值为模板串；id 不存在则新建（名称取 id），
///   **空串＝删除该用户模板**。
///
/// 范围名与模板 id 里不能带 `.`；模板 id 须合 `[a-z0-9_]+` 且不是保留字 `none` / `inherit`，
/// 与注释库加载时的取舍一致（不合格的模板加载时会被丢弃，写入时就该拒绝）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentDynKey<'a> {
    Cell {
        scope: &'a str,
        name: Option<&'a str>,
        cell: &'a str,
    },
    Template {
        id: &'a str,
    },
}

static DYN_CELL_FIELD: ConfigField = f("ui.comment.assign.<范围>.<格>", Str);
static DYN_TEMPLATE_FIELD: ConfigField = f("ui.comment.template.<id>", Str);

impl CommentDynKey<'_> {
    fn field(self) -> &'static ConfigField {
        match self {
            Self::Cell { .. } => &DYN_CELL_FIELD,
            Self::Template { .. } => &DYN_TEMPLATE_FIELD,
        }
    }
}

/// 解析 [`CommentDynKey`]；不是这两类形状返回 `None`。
pub fn comment_dyn_key(key: &str) -> Option<CommentDynKey<'_>> {
    if let Some(id) = key.strip_prefix("ui.comment.template.") {
        return crate::comment_lib::valid_user_template_id(id)
            .then_some(CommentDynKey::Template { id });
    }
    let rest = key.strip_prefix("ui.comment.assign.")?;
    let (scope, tail) = rest.split_once('.')?;
    let (name, cell) = match scope {
        "global" => (None, tail),
        "type" | "schema" | "mode" => {
            let (n, c) = tail.split_once('.')?;
            if n.is_empty() {
                return None;
            }
            (Some(n), c)
        }
        _ => return None,
    };
    COMMENT_CELL_KEYS
        .contains(&cell)
        .then_some(CommentDynKey::Cell { scope, name, cell })
}

/// 按点分键从配置 JSON 取值，兼顾 [`CommentDynKey`]：未写的格与未自定义的模板读作空串。
/// `config get` 与 cmdbar `config.get` 共用。
pub fn lookup_json_value(full: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
    use serde_json::Value;
    match comment_dyn_key(key) {
        Some(CommentDynKey::Template { id }) => {
            let text = full
                .pointer("/ui/comment/templates")
                .and_then(Value::as_array)
                .and_then(|a| a.iter().find(|t| t["id"] == id))
                .and_then(|t| t["text"].as_str())
                .unwrap_or("");
            Some(Value::String(text.to_string()))
        }
        Some(CommentDynKey::Cell { scope, name, cell }) => {
            let mut cur = full
                .pointer("/ui/comment/assign")
                .and_then(|a| a.get(scope));
            if let Some(n) = name {
                cur = cur.and_then(|c| c.get(n));
            }
            let v = cur.and_then(|c| c.get(cell)).and_then(|v| v.as_str());
            Some(Value::String(v.unwrap_or("").to_string()))
        }
        None => {
            let mut cur = full;
            for part in key.split('.') {
                cur = cur.get(part)?;
            }
            Some(cur.clone())
        }
    }
}

/// 该键是否已在注册表登记。
pub fn is_known_key(key: &str) -> bool {
    field(key).is_some()
}

/// 一条「这个全局项可被方案级配置覆盖」的登记。见 [`SCHEMA_OVERRIDES`]。
#[derive(Debug, Clone, Copy)]
pub struct SchemaOverride {
    /// 全局键；**以 `.` 结尾表示段前缀，且只管这一段的直接子键**。
    ///
    /// 前缀刻意不递归：覆盖能力逐字段实现，子段未必跟着（如 `schema.codetable.auto_phrase`），
    /// 递归会向用户提示一件不成立的事。要覆盖某个子段就为它再登记一条。
    pub key: &'static str,
    /// 方案文件（`.schema.toml` / `schema_overrides/<id>.toml`）里的落点段名。
    pub section: &'static str,
    /// 覆盖语义，一句话，直接进设置页的提示（写给用户，不是写给维护者）。
    pub note: &'static str,
    /// 是否由通用折叠器生效：`true` ⇒ 方案文件 `[candidate]` 下写同名叶子键即可覆盖（见
    /// [`split_candidate_patch`] 与 `UiCandidateConfig::folded_with`），目前只支持
    /// `ui.candidate.*` 的精确键。`false` 的登记是手写形态，这张表只负责提示。
    pub fold: bool,
}

/// 可被方案级配置覆盖的全局项。
///
/// 回答「全局改了，为什么某个方案里没变」，供设置页提示。它是静态事实，走 capability 快照
/// （随 `system.capabilities` 拉一次）；别为它遍历 [`crate::config::OriginSnapshot`]。
///
/// 没有编译期约束：对应关系只在各 `resolved()` 函数体里（[`crate::schema::SchemaBehavior`]、
/// [`crate::config::CodetableGlobal::resolved`]、[`crate::config::AuxCodeGlobal::resolved`]），
/// 新增方案级字段时必须回来加一行，漏了没有测试会红。
///
/// 不在表里的：`[candidate]` 的 `font_family` / `text_orientation`、`[punct]` 的 `mode`、
/// `[phrases]` 整段，它们没有对应的全局配置键。
pub const SCHEMA_OVERRIDES: &[SchemaOverride] = &[
    SchemaOverride {
        key: "schema.codetable.",
        section: "[engine.codetable]",
        note: "码表方案可逐项覆盖这里的设置；方案没写的项仍然用这里的值。",
        fold: false,
    },
    // 调频子段单独登记：段前缀不递归（见 `SchemaOverride::key`）。
    // ⚠️ 与它并列的 `auto_phrase` 子段**刻意不登记**——`CodetableGlobal::resolved` 里
    // 只折叠到 `frequency` 就返回了，码表自动造词没有方案级形态。标了它等于教用户
    // 去方案里写一段不会被读的配置。
    SchemaOverride {
        key: "schema.codetable.frequency.",
        section: "[engine.codetable.frequency]",
        note: "码表方案可逐项覆盖这里的调频设置；方案没写的项仍然用这里的值。",
        fold: false,
    },
    // 同上：段前缀不递归，英文候选子段要自己登记一条。
    SchemaOverride {
        key: "schema.codetable.english_merge.",
        section: "[engine.codetable.english_merge]",
        note: "码表方案可逐项覆盖这里的英文候选设置；方案没写的项仍然用这里的值。",
        fold: false,
    },
    SchemaOverride {
        key: "schema.pinyin.aux_code.enabled",
        section: "[engine.aux_code]",
        note: "方案可覆盖这一项；方案没写则用这里的值。",
        fold: false,
    },
    SchemaOverride {
        key: "schema.pinyin.aux_code.max_phrase_len",
        section: "[engine.aux_code]",
        note: "方案可覆盖这一项；方案没写则用这里的值。",
        fold: false,
    },
    SchemaOverride {
        key: "schema.pinyin.aux_code.direct",
        section: "[engine.aux_code]",
        note: "方案可覆盖这一项；方案没写则用这里的值。只对双拼方案生效。",
        fold: false,
    },
    SchemaOverride {
        key: "input.punct.custom_mappings",
        section: "[punct]",
        note: "声明了自己标点表的方案**整表**不用这里的表——不是逐条合并。",
        fold: false,
    },
    // ★ 这一条是 `PunctSpec::custom_mappings` 的文档点名要求「设置页与文档站必须写明」
    // 的那条推论：整表替换连开关一起换掉，所以关掉总开关，声明了自己标点表的方案
    // 照样出自定义符号。不标出来的话，用户会以为这个开关坏了。
    SchemaOverride {
        key: "input.punct.custom_enabled",
        section: "[punct]",
        note: "声明了自己标点表的方案不受这个开关约束——整表替换时连它一起换掉了。",
        fold: false,
    },
    SchemaOverride {
        key: "ui.candidate.layout",
        section: "[candidate]",
        note: "方案可覆盖候选窗排列方向；方案没写则用这里的值。",
        fold: false,
    },
    // ── 通用折叠（fold = true）：在方案 `[candidate]` 写同名叶子键即生效 ──
    // 新增一项只需加一行，并确认它的读取点走 `Coordinator::eff_candidate()`；
    // `scheme_fold_read_gates` 会拦住绕过它直接读全局值的代码。
    SchemaOverride {
        key: "ui.candidate.per_page",
        section: "[candidate]",
        note: "方案可覆盖每页候选数；方案没写则用这里的值。临拼、快捷等扩展档不受影响。",
        fold: true,
    },
];

/// 通用折叠键的叶子名（`ui.candidate.` 之后的部分）→ 该登记。非折叠键返回 `None`。
pub fn candidate_fold_entry(leaf: &str) -> Option<&'static SchemaOverride> {
    let full = format!("ui.candidate.{leaf}");
    SCHEMA_OVERRIDES.iter().find(|o| o.fold && o.key == full)
}

/// 把方案 `[candidate]` 里的通用补丁拆成「可生效的子集」和「问题清单」。
///
/// 判据三层，缺一即整条丢弃并进问题清单（写给用户看，所以带键名）：
/// 1. 是登记过的折叠键（白名单，见 [`SchemaOverride::fold`]）；
/// 2. 值过注册表校验（类型 / 枚举值域）；
/// 3. 注册表表达不了的值域在 [`value_in_domain`] 补一刀。
///
/// 「写错的值 = 没写」：错值混进补丁会把全局的好值盖掉。
pub fn split_candidate_patch(raw: &toml::Table) -> (toml::Table, Vec<String>) {
    let mut ok = toml::Table::new();
    let mut problems = Vec::new();
    for (leaf, value) in raw {
        let Some(entry) = candidate_fold_entry(leaf) else {
            let why = if is_known_key(&format!("ui.candidate.{leaf}")) {
                "该项暂不支持方案级覆盖"
            } else {
                "不是可识别的键"
            };
            problems.push(format!("[candidate] {leaf}（{why}）"));
            continue;
        };
        if let Err(e) = validate(entry.key, value) {
            problems.push(format!("[candidate] {leaf}（{e}）"));
            continue;
        }
        if let Some(why) = value_in_domain(leaf, value) {
            problems.push(format!("[candidate] {leaf}（{why}）"));
            continue;
        }
        ok.insert(leaf.clone(), value.clone());
    }
    (ok, problems)
}

/// 注册表只有类型 / 枚举，表达不了的数值下限在此补。返回 `Some(原因)` 表示越界。
/// 仅对 [`SchemaOverride::fold`] 的键调用；没有特殊值域的键不用登记。
fn value_in_domain(leaf: &str, value: &toml::Value) -> Option<&'static str> {
    match leaf {
        // 全局侧 0 会被 normalize 成默认值；方案侧「0」没有意义，也不该悄悄变成 7。
        "per_page" if value.as_integer().is_some_and(|n| n < 1) => Some("至少为 1"),
        _ => None,
    }
}

/// 查一个全局键是否可被方案级配置覆盖。段前缀登记（`key` 以 `.` 结尾）对该段下所有键成立。
pub fn schema_override_of(key: &str) -> Option<&'static SchemaOverride> {
    SCHEMA_OVERRIDES.iter().find(|o| {
        match o.key.strip_suffix('.') {
            // 段前缀只管直接子键：剩余部分必须是 `.名字` 且名字里不再有点（见 `key` 的文档）。
            Some(prefix) => key
                .strip_prefix(prefix)
                .and_then(|r| r.strip_prefix('.'))
                .is_some_and(|leaf| !leaf.is_empty() && !leaf.contains('.')),
            None => o.key == key,
        }
    })
}

/// 这份方案（**合并后**：方案文件 ⊕ override 层）覆盖了哪些全局配置键。
///
/// 回答全局设置页「已被 N 个方案覆盖」的反查（R8.6）。键名与 [`SCHEMA_OVERRIDES`] 的登记
/// 一一对应（前缀型登记展开成直接子键），每个键至多出现一次。
///
/// 判据逐字段与各 `resolved()` 的折叠条件一致；「写了但等于全局值」也算覆盖（此后改全局
/// 对它已无影响）。新增方案级字段时与 [`SCHEMA_OVERRIDES`] 两处都要加一行，护栏是
/// `overridden_keys_covers_every_schema_override_entry`。
///
/// `[punct] mode` 不在内：它没有对应的全局键（见 [`SCHEMA_OVERRIDES`] 的「不在表里」一节）。
pub fn schema_overridden_keys(schema: &crate::schema::Schema) -> Vec<&'static str> {
    let ct = &schema.engine.codetable;
    let freq = ct.frequency.clone().unwrap_or_default();
    let em = ct.english_merge.clone().unwrap_or_default();
    let aux = &schema.engine.aux_code;
    let punct_table = schema.punct.custom_mappings.is_some();
    let cand = &schema.candidate;
    let checks = [
        // ── [engine.codetable] ⇄ schema.codetable.* ──
        (
            "schema.codetable.top_code_commit",
            ct.top_code_commit.is_some(),
        ),
        (
            "schema.codetable.clear_on_empty_max",
            ct.clear_on_empty_max.is_some(),
        ),
        (
            "schema.codetable.auto_commit_at_full",
            ct.auto_commit_at_full.is_some(),
        ),
        (
            "schema.codetable.auto_commit_min_len",
            ct.auto_commit_min_len.is_some(),
        ),
        ("schema.codetable.punct_commit", ct.punct_commit.is_some()),
        (
            "schema.codetable.show_code_hint",
            ct.show_code_hint.is_some(),
        ),
        (
            "schema.codetable.single_code_input",
            ct.single_code_input.is_some(),
        ),
        (
            "schema.codetable.single_code_complete",
            ct.single_code_complete.is_some(),
        ),
        (
            "schema.codetable.short_code_yield_level",
            ct.short_code_yield_level.is_some(),
        ),
        ("schema.codetable.single_char", ct.single_char.is_some()),
        ("schema.codetable.z_key_repeat", ct.z_key_repeat.is_some()),
        ("schema.codetable.z_key_action", ct.z_key_action.is_some()),
        ("schema.codetable.wildcard", ct.wildcard.is_some()),
        ("schema.codetable.wildcard_key", ct.wildcard_key.is_some()),
        (
            "schema.codetable.wildcard_single_only",
            ct.wildcard_single_only.is_some(),
        ),
        (
            "schema.codetable.sentence_count",
            ct.sentence_count.is_some(),
        ),
        (
            "schema.codetable.sentence_max_count",
            ct.sentence_max_count.is_some(),
        ),
        (
            "schema.codetable.sentence_cycle_key",
            ct.sentence_cycle_key.is_some(),
        ),
        (
            "schema.codetable.sentence_grammar_weight",
            ct.sentence_grammar_weight.is_some(),
        ),
        // 码元字符集用空串表达「未设置」（见 `CodetableGlobal::resolved`）。
        ("schema.codetable.input_chars", !ct.input_chars.is_empty()),
        (
            "schema.codetable.leading_chars",
            !ct.leading_chars.is_empty(),
        ),
        // ── [engine.codetable.frequency] ──
        ("schema.codetable.frequency.enabled", freq.enabled.is_some()),
        (
            "schema.codetable.frequency.record_always",
            freq.record_always.is_some(),
        ),
        (
            "schema.codetable.frequency.strategy",
            freq.strategy.is_some(),
        ),
        (
            "schema.codetable.frequency.promote_prefix",
            freq.promote_prefix.is_some(),
        ),
        (
            "schema.codetable.frequency.half_life",
            freq.half_life.is_some(),
        ),
        (
            "schema.codetable.frequency.protect_top_n",
            freq.protect_top_n.is_some(),
        ),
        (
            "schema.codetable.frequency.protect_top_n_len1",
            freq.protect_top_n_len1.is_some(),
        ),
        (
            "schema.codetable.frequency.protect_top_n_len2",
            freq.protect_top_n_len2.is_some(),
        ),
        (
            "schema.codetable.frequency.protect_top_n_len3",
            freq.protect_top_n_len3.is_some(),
        ),
        // ── [engine.codetable.english_merge] ──
        ("schema.codetable.english_merge.enable", em.enable.is_some()),
        (
            "schema.codetable.english_merge.min_length",
            em.min_length.is_some(),
        ),
        (
            "schema.codetable.english_merge.block_commit",
            em.block_commit.is_some(),
        ),
        (
            "schema.codetable.english_merge.complete_min_length",
            em.complete_min_length.is_some(),
        ),
        (
            "schema.codetable.english_merge.block_commit_on_word",
            em.block_commit_on_word.is_some(),
        ),
        (
            "schema.codetable.english_merge.exact_position",
            em.exact_position.is_some(),
        ),
        (
            "schema.codetable.english_merge.complete_position",
            em.complete_position.is_some(),
        ),
        (
            "schema.codetable.english_merge.max_count",
            em.max_count.is_some(),
        ),
        // ── [engine.aux_code] ⇄ schema.pinyin.aux_code.* ──
        ("schema.pinyin.aux_code.enabled", aux.enabled.is_some()),
        (
            "schema.pinyin.aux_code.max_phrase_len",
            aux.max_phrase_len.is_some(),
        ),
        ("schema.pinyin.aux_code.direct", aux.direct.is_some()),
        // ── [punct]：整表替换连总开关一起换掉，两条登记由同一个字段判定 ──
        ("input.punct.custom_mappings", punct_table),
        ("input.punct.custom_enabled", punct_table),
        // ── [candidate] ──
        (
            "ui.candidate.layout",
            cand.layout != crate::config::LayoutIntent::Follow,
        ),
    ];
    let mut keys: Vec<&'static str> = checks
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(k, _)| k)
        .collect();
    // ── 通用折叠键：补丁里有（且值合法）就算覆盖；判据与 `Schema::behavior` 同源 ──
    let (patch, _) = split_candidate_patch(&cand.ui);
    keys.extend(
        patch
            .keys()
            .filter_map(|leaf| candidate_fold_entry(leaf).map(|o| o.key)),
    );
    keys
}

/// 把命令行/词条来源的原始字符串按注册表类型解析为 TOML 值（CLI `config set` 与
/// cmdbar `config.set` 共用；解析不校验枚举成员与范围，交给 [`validate`]）。
///
/// - Bool 认 true/1/yes/on 与 false/0/no/off（大小写不敏感）
/// - Float 拒绝 nan/inf/溢出（`Value::from(非有限 f64)` 下游会静默变 null）
/// - StrList 按逗号拆分；Map/StructList 需 JSON 文本
pub fn parse_str_value(key: &str, raw: &str) -> Result<toml::Value, String> {
    let fld = addressable_field(key).ok_or(ValidateError::UnknownKey.to_string())?;
    let v = match fld.ty {
        FieldType::Bool => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => toml::Value::Boolean(true),
            "false" | "0" | "no" | "off" => toml::Value::Boolean(false),
            _ => return Err(format!("'{raw}' 不是布尔值（true/false）")),
        },
        FieldType::Int => raw
            .trim()
            .parse::<i64>()
            .map(toml::Value::Integer)
            .map_err(|_| format!("'{raw}' 不是整数"))?,
        FieldType::Float => match raw.trim().parse::<f64>() {
            Ok(f) if f.is_finite() => toml::Value::Float(f),
            _ => return Err(format!("'{raw}' 不是有限数字")),
        },
        // LayoutEnum 同 Enum：命令行给的就是那一串（可能是 `"h:hide v:always"`），
        // 原样收下，值域与语法由 `validate` 判。
        FieldType::Str | FieldType::Enum(_) | FieldType::LayoutEnum(_) => {
            toml::Value::String(raw.to_string())
        }
        FieldType::StrList => toml::Value::Array(
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| toml::Value::String(s.to_string()))
                .collect(),
        ),
        FieldType::Map(_) | FieldType::StructList => {
            let jv: serde_json::Value =
                serde_json::from_str(raw).map_err(|e| format!("复杂值需为 JSON: {e}"))?;
            toml::Value::try_from(jv).map_err(|e| format!("无法转为配置值: {e}"))?
        }
    };
    Ok(v)
}

/// 配置值校验错误（按 registry 校验 setItems / CLI 写入时用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidateError {
    /// 键未在注册表登记。
    UnknownKey,
    /// 值类型与声明不符。
    TypeMismatch {
        expected: &'static str,
        got: &'static str,
    },
    /// 枚举值不在允许集合内。
    EnumOutOfRange {
        allowed: &'static [&'static str],
        got: String,
    },
}

impl std::fmt::Display for ValidateError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidateError::UnknownKey => write!(fmt, "未登记的配置键"),
            ValidateError::TypeMismatch { expected, got } => {
                write!(fmt, "类型应为 {expected}，实为 {got}")
            }
            ValidateError::EnumOutOfRange { allowed, got } => {
                write!(fmt, "值 {got:?} 不在允许集合 {allowed:?}")
            }
        }
    }
}

impl std::error::Error for ValidateError {}

fn toml_type_name(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::Boolean(_) => "bool",
        toml::Value::Integer(_) => "int",
        toml::Value::Float(_) => "float",
        toml::Value::String(_) => "string",
        toml::Value::Array(_) => "array",
        toml::Value::Table(_) => "table",
        toml::Value::Datetime(_) => "datetime",
    }
}

fn type_label(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Bool => "bool",
        FieldType::Int => "int",
        FieldType::Float => "float",
        FieldType::Str => "string",
        FieldType::Enum(_) => "string(enum)",
        FieldType::LayoutEnum(_) => "string(enum, 可按排布分档)",
        FieldType::StrList => "string[]",
        FieldType::Map(_) => "table",
        FieldType::StructList => "array",
    }
}

/// 按注册表校验"键+值"。未登记键、类型不符、枚举越界均返回结构化错误。
/// 宽松点：`Float` 字段接受整数值（用户常输 18 而非 18.0）。
pub fn validate(key: &str, value: &toml::Value) -> Result<(), ValidateError> {
    let f = addressable_field(key).ok_or(ValidateError::UnknownKey)?;
    let type_ok = match f.ty {
        FieldType::Bool => value.is_bool(),
        FieldType::Int => value.is_integer(),
        FieldType::Float => value.is_float() || value.is_integer(),
        FieldType::Str => value.is_str(),
        FieldType::Enum(allowed) => {
            let s = value.as_str().ok_or(ValidateError::TypeMismatch {
                expected: "string",
                got: toml_type_name(value),
            })?;
            if !allowed.contains(&s) {
                return Err(ValidateError::EnumOutOfRange {
                    allowed,
                    got: s.to_string(),
                });
            }
            true
        }
        // 可分档枚举：把值摊成每一档的实际取值，逐档过同一个值域。拼错档位名（`"x:hide"`）会
        // 原样落成标量到这里而报 EnumOutOfRange。
        FieldType::LayoutEnum(allowed) => {
            let parts = crate::by_layout::domain_parts(value).ok_or({
                ValidateError::TypeMismatch {
                    expected: "string",
                    got: toml_type_name(value),
                }
            })?;
            for s in parts {
                if !allowed.contains(&s.as_str()) {
                    return Err(ValidateError::EnumOutOfRange { allowed, got: s });
                }
            }
            true
        }
        FieldType::StrList => value
            .as_array()
            .map(|a| a.iter().all(|e| e.is_str()))
            .unwrap_or(false),
        FieldType::Map(_) => value.is_table(),
        FieldType::StructList => value.is_array(),
    };
    if type_ok {
        // 分配格的值：空串（删除）/ none / inherit / 形状合法的模板 id。
        if let (Some(CommentDynKey::Cell { .. }), Some(v)) = (comment_dyn_key(key), value.as_str())
            && !v.is_empty()
            && !matches!(v, "none" | "inherit")
            && !crate::comment_lib::valid_user_template_id(v)
        {
            return Err(ValidateError::EnumOutOfRange {
                allowed: &["none", "inherit", "<模板 id：小写字母 数字 下划线>"],
                got: v.to_string(),
            });
        }
        Ok(())
    } else {
        Err(ValidateError::TypeMismatch {
            expected: type_label(f.ty),
            got: toml_type_name(value),
        })
    }
}

/// 把 TOML 值展开为点分叶子键列表。
///
/// 规则：递归进入**非空表**；标量、数组、**空表**（如空 HashMap）均视为叶子。
/// 故 `[ui.candidate]`（非空表）会下钻，而 `input.punct.custom_mappings = {}`（空表）作叶子保留。
fn collect_leaf_keys(prefix: &str, value: &toml::Value, out: &mut Vec<String>) {
    match value {
        // 登记为 Map 的键就是叶子，不再下钻：Map 的内容是数据（如 `keys.key_actions` 的键名 → 动词），
        // 下钻会把它当成注册表键比对，两个方向同时报错。
        toml::Value::Table(_)
            if !prefix.is_empty()
                && matches!(
                    field(prefix).map(|f| f.ty),
                    // LayoutEnum 的表写法（`{ h = …, v = … }`）同理：下钻会把 `.h` / `.v`
                    // 当成注册表键去比对，两个方向同时报错（孤立键 + 本键缺失）。
                    Some(FieldType::Map(_) | FieldType::LayoutEnum(_))
                ) =>
        {
            out.push(prefix.to_string())
        }
        toml::Value::Table(t) if !t.is_empty() => {
            for (k, v) in t {
                let child = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                collect_leaf_keys(&child, v, out);
            }
        }
        _ => out.push(prefix.to_string()),
    }
}

/// 默认配置（[`Config::default`]）序列化后的全部叶子键（已排序去重）。
pub fn config_leaf_keys() -> Vec<String> {
    let value = toml::Value::try_from(Config::default()).expect("serialize default config");
    let mut out = Vec::new();
    collect_leaf_keys("", &value, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_leaf_entries(prefix: &str, value: &toml::Value, out: &mut Vec<(String, toml::Value)>) {
    match value {
        toml::Value::Table(t) if !t.is_empty() => {
            for (k, v) in t {
                let child = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                collect_leaf_entries(&child, v, out);
            }
        }
        _ => out.push((prefix.to_string(), value.clone())),
    }
}

/// 把任意 TOML 表展开为 `(点分键, 叶子值)` 列表（叶子规则同 [`config_leaf_keys`]）。
/// 供 `config import` 把一份 TOML 拍平成逐字段 setItems。
pub fn leaf_entries(value: &toml::Value) -> Vec<(String, toml::Value)> {
    let mut out = Vec::new();
    collect_leaf_entries("", value, &mut out);
    out
}

#[cfg(test)]
#[path = "config_schema_tests.rs"]
mod tests;

/// 可分档枚举（`LayoutEnum`）的校验：每一档都要落在同一个值域里。
#[cfg(test)]
#[path = "config_schema_layout_enum_tests.rs"]
mod layout_enum_tests;

#[cfg(test)]
#[path = "config_schema_overridden_keys_tests.rs"]
mod overridden_keys_tests;
