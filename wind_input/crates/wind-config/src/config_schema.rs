//! 配置字段注册表（声明式单一真相源）。
//!
//! 每个配置叶子键在此声明其值类型。通过测试与 [`Config`](crate::config::Config) 结构体
//! **反向对照**（注册表覆盖所有键、类型与默认值一致），并与系统预置 `data/config.toml`
//! 对照（无孤立键）。CLI、core 端校验、设置 UI 均由此注册表派生，杜绝多份手写真相源漂移。
//!
//! 注：本注册表只描述 **config 类**（用户可改配置）；运行状态（state）与分发数据（data）不在此。
//! 详见仓库根 `SETTINGS_REVAMP_PLAN.md` 的"数据三分准则"，键名映射见 `docs/config-key-migration.md`。

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
    /// 携带**键名值域**：非空表示这张表的键只能取这几个（如 `ui.font.scripts` 的脚本类名），
    /// 空表示键由用户自由命名（如 `input.punct.custom_mappings`）。
    ///
    /// ⚠️ 值域为什么要进注册表：设置端的下拉/预填项若各自手抄一份，core 加了新类名而设置端
    /// 没跟上，用户就永远看不到新类别；反向漂移（设置端多出 core 不认的键）则是「填了没反应」。
    /// `ui.candidate.layout` 已经踩过这个形状（值域从 3 变 4，设置端滞后，方案里手写的第 4 个
    /// 值被静默改写回默认）。有了这一位，设置端可用 capability 做守门比对。
    Map(&'static [&'static str]),
    /// 结构体数组（如 mix_modes），整体作不透明叶子。
    StructList,
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

use FieldType::{Bool, Enum, Float, Int, Map, Str, StrList, StructList};

/// 候选无效按键三策（number_key/select_key/select_char_key 共用）。
const OVERFLOW_VALUES: &[&str] = &["ignore", "commit", "commit_and_input"];

/// 空码（缓冲非空但无候选）时终结键怎么处置这串废码——**回车/空格两键**。
/// **只有两态**：`commit` 上屏原码 / `clear` 丢弃。语义与实现见
/// `docs/design/enter-behavior-clear-semantics.md`。
///
/// ⚠️ 登记成 `Enum` 而非 `Str` 是刻意的：这三键曾因值域不受约束，让设置端 manifest 抄来了
/// [`OVERFLOW_VALUES`] 的四个选项，用户选「忽略」被静默当成「上屏编码」，界面与行为不一致
/// 且无任何测试拦得住。值域进注册表后，设置端的守门测试即可比对。
const EMPTY_CODE_BEHAVIOR_VALUES: &[&str] = &["commit", "clear"];

/// 标点键的空码处置——比回车/空格**多一态**，故不能与 [`EMPTY_CODE_BEHAVIOR_VALUES`] 共用。
///
/// ★ 这一族配置描述的行为其实是**两根轴**，而值域只有一维：
///
/// | | 键字符输出 | 键字符吞掉 |
/// |---|---|---|
/// | 废码上屏 | `commit` | （无意义，不设值） |
/// | 废码丢弃 | `clear` | `clear_no_input` |
///
/// 回车/空格的 `clear` 落在**吞键**那一格（返回 `ClearComposition`，键本身不产出任何字符），
/// 标点的 `clear` 落在**出键**那一格（标点照常上屏）——同一个字面值在第二根轴上取值相反。
/// 这不是笔误：标点是用户真正想输入的可见字符，吞掉等于吞掉用户的意图，故出键是对的默认。
/// `clear_no_input` 补的正是这一族缺的第四格：「这串码作废，这个键也当没按过」。
///
/// ⚠️ **不要**为了「三键同族」把 `clear_no_input` 也加给回车/空格：它们的 `clear` 本就是
/// 吞键态，加了会得到两个行为完全相同的值，设置页出现两个无法区分的选项。
///
/// ⚠️ 加值前先确认消费点真的分了那一支——`Coordinator::punct_empty_code_policy` 的 `match`
/// 是唯一解释器，未知值一律落到 `commit`。
const PUNCT_EMPTY_CODE_BEHAVIOR_VALUES: &[&str] = &["commit", "clear", "clear_no_input"];

/// 码表词频应用策略。
const FREQ_STRATEGY_VALUES: &[&str] = &["top", "step", "position"];

/// 前缀补全参与词频位置提升的范围（按语义单元数判定）。
const PROMOTE_PREFIX_VALUES: &[&str] = &["none", "single", "all"];

/// 英文候选的词频记账码口径（内部配置，不进 GUI）。
const ENGLISH_CODE_SCOPE_VALUES: &[&str] = &["candidate", "input"];

/// `ui.font.scripts` 的合法键名——**文字类别**，不是脚本的全集。
///
/// 与渲染端 `wind_ui::text::script::ScriptClass::key()` 是同一件事的两处表达，由
/// wind-ui 侧的 `script_class_keys_match_config_registry` 测试钉住（wind-config 依赖不到
/// wind-ui，反向才能测）。**加类别时两处一起改，否则设置页永远不会列出新类别。**
pub const FONT_SCRIPT_KEYS: &[&str] = &[
    "latin", "greek", "cyrillic", "cjk", "emoji", "digits", "punct",
];

/// 模式级候选布局意图（`LayoutIntent` 的 serde 形态）。临拼/临英/网址/加词共用；
/// mix / special 的同名字段是 StructList 条目内的属性，不在本注册表单独登记。
const LAYOUT_INTENT_VALUES: &[&str] = &["follow", "vertical", "horizontal"];

/// 全部配置字段声明（单一真相源）。与 [`Config`] 经测试反向对照，保证零漂移。
/// 域划分见 `docs/config-key-migration.md`（不做向后兼容，旧键已弃）。
///
/// # 三态键（`Option<T>`，默认 `None`）刻意不登记
///
/// 模式级注释模板 `input.{temp_english,temp_pinyin,url}.comment_template_{vertical,horizontal}`
/// 不在本表内，**这不是遗漏**：注册表是与 `Config::default()` 的序列化键集反向对照的，
/// 而这类键的出厂值恰恰是「键不存在」（= 跟随全局），序列化时压根不出现，登记即被判「多余」。
///
/// 不登记同时是**正确**的：`prune_redundant` 只清理「已登记且值等于出厂默认」的键，
/// 未登记键一律不碰——用户手写的模板因此永远不会被写回逻辑清掉。这与废弃键、
/// `Map`/`StructList` 下钻子路径归为同一类处置，理由见该函数的「两道保险」。
static REGISTRY: &[ConfigField] = &[
    // -- schema（方案 + 拼音 + 模式）--
    f("schema.active", Str),
    f("schema.available", StrList),
    f("schema.primary_codetable", Str),
    f("schema.primary_pinyin", Str),
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
    f("schema.codetable.z_key_repeat", Bool),
    // 带参数的值域（`mix:<id>` / `special:<id>`）故用 Str 而非 Enum；解析与校验见 `BoundAction`。
    f("schema.codetable.z_key_action", Str),
    // 码元字符集：范围+字面的自由文本（如 `a-x/`、`a-z0-9`），值域无法枚举故用 Str；
    // 解析与非法回落见 `CodeCharSet`。空 = 内置默认 `a-z`。
    f("schema.codetable.input_chars", Str),
    f("schema.codetable.leading_chars", Str),
    f("schema.codetable.frequency.enabled", Bool),
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
    f("schema.english.raw_candidate", Bool),
    f("schema.english.case_variants", Bool),
    f("schema.codetable.auto_phrase.enabled", Bool),
    f("schema.codetable.auto_phrase.min_phrase_len", Int),
    f("schema.codetable.auto_phrase.max_phrase_len", Int),
    f("schema.codetable.auto_phrase.promote_count", Int),
    f("schema.codetable.auto_phrase.idle_timeout_ms", Int),
    f("schema.codetable.auto_phrase.temp_max_entries", Int),
    // 全局拼音
    f("schema.pinyin.show_code_hint", Bool),
    f("schema.pinyin.use_smart_compose", Bool),
    f("schema.pinyin.separator", Str),
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
    f("schema.pinyin.frequency.enabled", Bool),
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
    // 上下文语言模型（n-gram）。weight=0 时不加载模型文件，整句结果与没有该功能时逐位相同。
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
    // 全局混输（融合策略）
    f("schema.mix.show_source_hint", Bool),
    f("schema.mix.enable_english", Bool),
    f("schema.mix.pinyin_only_overflow", Bool),
    f("schema.mix.top_code_override_pinyin", Bool),
    f("schema.mix.auto_commit_block_on_pinyin", Bool),
    f("schema.mix.auto_commit_block_on_english", Bool),
    f("schema.mix.min_pinyin_length", Int),
    f("schema.mix.min_english_length", Int),
    f("schema.mix.block_commit_on_pinyin_word", Bool),
    f("schema.mix.pinyin_word_min_weight", Int),
    f("schema.mix.enable_pinyin_abbrev", Bool),
    f("schema.mix.pinyin_partial_candidates", Bool),
    f("schema.mix.pinyin_partial_candidates_overflow", Bool),
    // 快捷输入：各候选来源的开关与优先级在 schema.mix_modes 的 members 里（有无=开关，
    // 顺序=优先级）；总开关＝把 quick_mix 的 trigger_keys 清空。此处只有全局行为项。
    f("schema.quick_input.decimal_places", Int),
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
    // 检索范围放宽（智能档增强，见 docs/design/smart-filter-scope-relax.md）
    f("input.scope_relax.page_end_key", Bool),
    f("input.scope_relax.prefix", Str),
    f("input.enter_behavior", Enum(EMPTY_CODE_BEHAVIOR_VALUES)),
    f(
        "input.space_on_empty_behavior",
        Enum(EMPTY_CODE_BEHAVIOR_VALUES),
    ),
    f(
        "input.punct_on_empty_behavior",
        Enum(PUNCT_EMPTY_CODE_BEHAVIOR_VALUES),
    ),
    f("input.numpad_behavior", Str),
    // 启动默认状态（原 general 域）
    f("input.default.remember_last_state", Bool),
    f("input.default.state_scope", Enum(&["global", "app"])),
    f("input.default.chinese_mode", Bool),
    f("input.default.full_width", Bool),
    f("input.default.chinese_punct", Bool),
    f("input.punct.follow_mode", Bool),
    f("input.punct.smart_after_digit", Bool),
    f("input.punct.smart_list", Str),
    f("input.punct.custom_enabled", Bool),
    f("input.punct.custom_mappings", Map(&[])),
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
    f("input.temp_english.enabled", Bool),
    f("input.temp_english.show_candidates", Bool),
    f(
        "input.temp_english.shift_behavior",
        Enum(&["temp_english", "direct_commit"]),
    ),
    f("input.temp_english.trigger_keys", StrList),
    f("input.temp_english.allow_symbols", Bool),
    f("input.temp_english.symbol_chars", Str),
    f("input.temp_english.space_as_input", Bool),
    f("input.temp_english.raw_candidate", Bool),
    f("input.temp_english.case_variants", Bool),
    f(
        "input.temp_english.candidate_layout",
        Enum(LAYOUT_INTENT_VALUES),
    ),
    f("input.capslock.cancel_on_mode_switch", Bool),
    f("input.temp_pinyin.enabled", Bool),
    f("input.temp_pinyin.trigger_keys", StrList),
    f("input.temp_pinyin.hotkey", Str),
    f(
        "input.temp_pinyin.candidate_layout",
        Enum(LAYOUT_INTENT_VALUES),
    ),
    f("input.url.enabled", Bool),
    f("input.url.prefixes", StrList),
    f("input.url.candidate_layout", Enum(LAYOUT_INTENT_VALUES)),
    f(
        "input.add_word.candidate_layout",
        Enum(LAYOUT_INTENT_VALUES),
    ),
    f("input.s2t.enabled", Bool),
    f("input.s2t.variant", Str),
    f("input.cmdbar.enabled", Bool),
    f("input.cmdbar.candidate_prefix", Str),
    // 短语前缀列举（原 dict.phrase）
    f("input.phrase.min_prefix", Int),
    // 顶码上屏策略
    f(
        "input.top_commit_mode",
        Enum(&["pre_confirm", "direct_commit"]),
    ),
    // 联想。kind 兼任开关与类型（"off" 即关）。本段是**桌面基线**，移动端的差异走
    // [mobile.association]——值域里不留平台哨兵。
    f("input.association.kind", Enum(&["off", "word", "smart"])),
    f("input.association.mode", Enum(&["one_shot", "continuous"])),
    f("input.association.max_count", Int),
    f("input.association.space_commits", Bool),
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
    f("keys.switch_engine", Str),
    f("keys.toggle_full_width", Str),
    f("keys.toggle_punct", Str),
    f("keys.toggle_toolbar", Str),
    f("keys.open_settings", Str),
    f("keys.add_word", Str),
    f("keys.open_add_word_dialog", Str),
    f("keys.toggle_s2t", Str),
    f("keys.activate_ime", Str),
    f("keys.pin_candidate", Str),
    f("keys.delete_candidate", Str),
    f("keys.take_screenshot", Str),
    f("keys.global_hotkeys", StrList),
    // `keys.schema_hotkeys` 已废弃并**从登记表移除**（与当年 `schema.special_modes` 同样
    // 的处置）：改写进下面的 key_actions（动词 `switch_schema:<id>`），**没有兼容折算**
    // ——加载期只由 `Config::warn_legacy_schema_hotkeys` 告警一次随后清空，残留的老配置
    // 不生效。serde 字段仍保留（`legacy_schema_hotkeys`），是为了**读得出残留值以便告警**，
    // 不是为了让它继续工作；移出登记表则让 `config.setItems`
    // 不再接受对它的写入——写旧键的客户端会收到 skipped 而不是静默成功，这正是我们想要的
    // 可见性（本仓最忌讳「写进去了、没人读」）。
    f("keys.key_actions", Map(&[])),
    f("keys.session_actions", Map(&[])),
    f("keys.select_key_groups", StrList),
    f("keys.page_keys", StrList),
    f("keys.highlight_keys", StrList),
    f("keys.select_char_keys", StrList),
    f("keys.overflow.number_key", Enum(OVERFLOW_VALUES)),
    f("keys.overflow.select_key", Enum(OVERFLOW_VALUES)),
    f("keys.overflow.select_char_key", Enum(OVERFLOW_VALUES)),
    // -- ui（外观）--
    f("ui.candidate.per_page", Int),
    f("ui.candidate.per_page_extended", Int),
    f("ui.candidate.layout", Enum(&["horizontal", "vertical"])),
    f(
        "ui.candidate.preedit_display",
        Enum(&["app_inline", "candidate_top", "candidate_inline"]),
    ),
    f("ui.candidate.hide_window", Bool),
    // 首显策略的三个内部选项（不进设置页，仅 config.toml / CLI 可调）。
    // 注册到此表是必须的：registry_covers_every_config_key 强制全键覆盖，
    // 漏注册的键会静默无法经 CLI/RPC 读写。
    f("ui.candidate.first_show_settle_ratio", Float),
    f("ui.candidate.fast_typing_window_ms", Int),
    f("ui.candidate.fast_first_show_fallback_ms", Int),
    f("ui.candidate.font_size", Float),
    f("ui.candidate.font_size_follow_theme", Bool),
    f(
        "ui.candidate.pager_bar_display",
        Enum(&["", "hide", "auto", "always"]),
    ),
    f(
        "ui.candidate.page_number_display",
        Enum(&["", "show", "hide"]),
    ),
    f("ui.candidate.max_chars", Int),
    f("ui.candidate.min_window_width_horizontal", Int),
    f("ui.candidate.min_window_width_vertical", Int),
    f("ui.candidate.min_window_height_horizontal", Int),
    f("ui.candidate.min_window_height_vertical", Int),
    f("ui.candidate.min_rows", Int),
    f("ui.candidate.comment_template_vertical", Str),
    f("ui.candidate.comment_template_horizontal", Str),
    f("ui.candidate.comment_max_chars_vertical", Int),
    f("ui.candidate.comment_max_chars_horizontal", Int),
    f("ui.comment_dicts", StructList),
    f("ui.candidate.index_labels", StrList),
    f("ui.candidate.flip_when_above", Bool),
    f("ui.candidate.swap_preedit_when_above", Bool),
    f("ui.candidate.pager_in_preedit", Bool),
    f(
        "ui.candidate.position_mode",
        Enum(&["follow_caret", "fixed"]),
    ),
    f("ui.candidate.custom_x", Int),
    f("ui.candidate.custom_y", Int),
    f("ui.font.family", Str),
    f("ui.font.path", Str),
    f("ui.font.render_mode", Enum(&["directwrite", "gdi"])),
    f("ui.font.fallback", StrList),
    // 脚本类名 → 该类的字体链。登记为 Map（叶子、不下钻）：`latin`/`cjk` 这些是**数据**
    // 不是配置项，下钻会把 `ui.font.scripts.latin` 当成注册表键去比对。同 `keys.key_actions`。
    f("ui.font.scripts", Map(FONT_SCRIPT_KEYS)),
    f("ui.theme.name", Str),
    f("ui.theme.style", Str),
    f("ui.mode_indicator.style", Enum(&["short", "full", "none"])),
    f("ui.tooltip.delay", Int),
    f("ui.tooltip.code_enabled", Bool),
    f("ui.tooltip.pinyin_enabled", Bool),
    f("ui.tooltip.pinyin_heteronyms", Bool),
    f("ui.tooltip.pinyin_max_readings", Int),
    f("ui.tooltip.chaizi_enabled", Bool),
    f("ui.tooltip.debug_enabled", Bool),
    f("ui.status.enabled", Bool),
    f("ui.status.duration", Int),
    f("ui.status.display_mode", Enum(&["temp", "always"])),
    f("ui.status.show_on_focus", Bool),
    f("ui.status.schema_name_style", Enum(&["full", "short"])),
    f("ui.status.position_mode", Enum(&["follow_caret", "fixed"])),
    f("ui.status.offset_x", Int),
    f("ui.status.offset_y", Int),
    f("ui.status.custom_x", Int),
    f("ui.status.custom_y", Int),
    f("ui.status.items", StrList),
    f("ui.toolbar.visible", Bool),
    f("ui.toolbar.hide_in_fullscreen", Bool),
    f("ui.toolbar.auto_hide", Bool),
    f("ui.toolbar.auto_hide_delay", Int),
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
    // punct_badge 用 Enum 而非 Str：写错一个词只会静默回落默认形状，而"配了没反应"
    // 是最难自查的一类；登记成员后 `config set` 与设置页都能先一步挡下。
    f(
        "ui.langbar.punct_badge",
        Enum(&[
            "none",
            "corner_triangle",
            "outer_ring",
            "bottom_bar",
            "circle_square",
            "ring_dot",
        ]),
    ),
    f("ui.langbar.punct_badge_scale", Float),
    f("ui.langbar.full_width_mark", Bool),
    f("ui.langbar.full_width_mark_scale", Float),
    f("ui.langbar.badge_alpha", Float),
    f("ui.langbar.colored", Bool),
    f("ui.langbar.punct_color_cn", Str),
    f("ui.langbar.punct_color_en", Str),
    f("ui.langbar.full_width_color", Str),
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
    // -- debug（调试）--
    f(
        "debug.log_level",
        Enum(&["trace", "debug", "info", "warn", "error"]),
    ),
    f("debug.log_max_size_mb", Int),
    f("debug.log_max_files", Int),
    // -- mobile（移动端对上面各域的覆盖；桌面构建完全无视）--
    // 只登记**当前真有平台差异**的键，别提前铺一排永远等于基线的键——每个都要在这里、
    // 预置文件、capability 快照、设置页豁免名单各占一行。详见 MobileConfig 的文档。
    f("mobile.association.kind", Enum(&["off", "word", "smart"])),
    f("mobile.association.mode", Enum(&["one_shot", "continuous"])),
    f("mobile.association.punct", Bool),
];

/// 返回配置字段注册表。
pub fn registry() -> &'static [ConfigField] {
    REGISTRY
}

/// 按点分路径查注册表条目（未登记返回 None）。
pub fn field(key: &str) -> Option<&'static ConfigField> {
    REGISTRY.iter().find(|f| f.key == key)
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
    /// ★ 前缀刻意**不递归**到更深的子段。覆盖能力是逐字段实现的（`resolved()` 里一个
    /// `if let Some` 一行），子段未必跟着——`schema.codetable.` 可覆盖，它下面的
    /// `auto_phrase` 子段却整个不折叠。递归前缀会把 `auto_phrase.*` 一并标成「可被方案
    /// 覆盖」，而那是**告诉用户一件不成立的事**：他会去方案里写这一段，写了没反应，
    /// 然后怀疑是自己写错了。要覆盖某个子段就为它再登记一条。
    pub key: &'static str,
    /// 方案文件（`.schema.toml` / `schema_overrides/<id>.toml`）里的落点段名。
    pub section: &'static str,
    /// 覆盖语义，一句话，直接进设置页的提示——**写给用户，不是写给维护者**。
    pub note: &'static str,
}

/// 可被方案级配置覆盖的全局项。
///
/// # 这张表回答的问题
///
/// 「我在全局改了标点行为，为什么某个方案里没变」——因为那个方案自己声明了。这是本仓
/// 配置模型里用户最常撞到的一层，而全局设置页此前**没有任何地方提示过它的存在**。
///
/// ★ 它是**静态事实**：某项能不能被方案覆盖由配置模型决定，与用户机器上有什么文件无关。
/// 因此走 capability 快照（启动时随 `system.capabilities` 拉一次），**零运行时成本**。
/// ⛔ 不要为它去遍历 [`crate::config::OriginSnapshot`]——那是排查用的重查询，
/// 回答的也是另一个问题（「实际来自哪一层」）。
///
/// # ⚠️ 没有编译期约束，加方案级字段时必须回来加一行
///
/// 「方案的 `[codetable].top_code_commit` 覆盖全局的 `schema.codetable.top_code_commit`」
/// 这层对应关系只存在于 `resolved()` 的函数体里，类型系统看不见。新增方案级字段而漏了
/// 这张表，后果是设置页那一行**不再提示可被覆盖**——没有任何测试会红。
/// 各方案级段的权威定义：[`crate::schema::SchemaBehavior`]（punct/candidate/phrases）、
/// [`crate::config::CodetableGlobal::resolved`]、[`crate::config::AuxCodeGlobal::resolved`]。
///
/// # 不在表里的方案级字段
///
/// `[candidate]` 的 `font_family` 与 `text_orientation`、`[punct]` 的 `mode`、`[phrases]`
/// 整段：它们**没有对应的全局配置键**（回落目标分别是主题、运行时中英标点态、方案自身），
/// 全局设置页上根本没有那一行，无从标记。
pub const SCHEMA_OVERRIDES: &[SchemaOverride] = &[
    SchemaOverride {
        key: "schema.codetable.",
        section: "[codetable]",
        note: "码表方案可逐项覆盖这里的设置；方案没写的项仍然用这里的值。",
    },
    // 调频子段单独登记：段前缀不递归（见 `SchemaOverride::key`）。
    // ⚠️ 与它并列的 `auto_phrase` 子段**刻意不登记**——`CodetableGlobal::resolved` 里
    // 只折叠到 `frequency` 就返回了，码表自动造词没有方案级形态。标了它等于教用户
    // 去方案里写一段不会被读的配置。
    SchemaOverride {
        key: "schema.codetable.frequency.",
        section: "[codetable.frequency]",
        note: "码表方案可逐项覆盖这里的调频设置；方案没写的项仍然用这里的值。",
    },
    SchemaOverride {
        key: "schema.pinyin.aux_code.enabled",
        section: "[aux_code]",
        note: "方案可覆盖这一项；方案没写则用这里的值。",
    },
    SchemaOverride {
        key: "schema.pinyin.aux_code.max_phrase_len",
        section: "[aux_code]",
        note: "方案可覆盖这一项；方案没写则用这里的值。",
    },
    SchemaOverride {
        key: "input.punct.custom_mappings",
        section: "[punct]",
        note: "声明了自己标点表的方案**整表**不用这里的表——不是逐条合并。",
    },
    // ★ 这一条是 `PunctSpec::custom_mappings` 的文档点名要求「设置页与文档站必须写明」
    // 的那条推论：整表替换连开关一起换掉，所以关掉总开关，声明了自己标点表的方案
    // 照样出自定义符号。不标出来的话，用户会以为这个开关坏了。
    SchemaOverride {
        key: "input.punct.custom_enabled",
        section: "[punct]",
        note: "声明了自己标点表的方案不受这个开关约束——整表替换时连它一起换掉了。",
    },
    SchemaOverride {
        key: "ui.candidate.layout",
        section: "[candidate]",
        note: "方案可覆盖候选窗排列方向；方案没写则用这里的值。",
    },
    SchemaOverride {
        key: "ui.candidate.comment_template_vertical",
        section: "[candidate]",
        note: "方案可覆盖注释模板；方案没写则用这里的值。",
    },
    SchemaOverride {
        key: "ui.candidate.comment_template_horizontal",
        section: "[candidate]",
        note: "方案可覆盖注释模板；方案没写则用这里的值。",
    },
];

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

/// 把命令行/词条来源的原始字符串按注册表类型解析为 TOML 值（CLI `config set` 与
/// cmdbar `config.set` 共用；解析不校验枚举成员与范围，交给 [`validate`]）。
///
/// - Bool 认 true/1/yes/on 与 false/0/no/off（大小写不敏感）
/// - Float 拒绝 nan/inf/溢出（`Value::from(非有限 f64)` 下游会静默变 null）
/// - StrList 按逗号拆分；Map/StructList 需 JSON 文本
pub fn parse_str_value(key: &str, raw: &str) -> Result<toml::Value, String> {
    let fld = field(key).ok_or(ValidateError::UnknownKey.to_string())?;
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
        FieldType::Str | FieldType::Enum(_) => toml::Value::String(raw.to_string()),
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
        FieldType::StrList => "string[]",
        FieldType::Map(_) => "table",
        FieldType::StructList => "array",
    }
}

/// 按注册表校验"键+值"。未登记键、类型不符、枚举越界均返回结构化错误。
/// 宽松点：`Float` 字段接受整数值（用户常输 18 而非 18.0）。
pub fn validate(key: &str, value: &toml::Value) -> Result<(), ValidateError> {
    let f = field(key).ok_or(ValidateError::UnknownKey)?;
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
        FieldType::StrList => value
            .as_array()
            .map(|a| a.iter().all(|e| e.is_str()))
            .unwrap_or(false),
        FieldType::Map(_) => value.is_table(),
        FieldType::StructList => value.is_array(),
    };
    if type_ok {
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
        // ★ 注册表里登记为 Map 的键**就是叶子**，不再下钻。
        //
        // Map 的内容是**数据**（`keys.key_actions` 的键名 → 动词、`schema_hotkeys` 的
        // 方案 id → 热键），不是配置项。下钻会把 `keys.key_actions.backtick` 这种数据
        // 当成注册表键去比对——两个方向同时报错：注册表里没有它（"孤立键"），而
        // `keys.key_actions` 本身又因为没出现过被算作"缺失"。
        //
        // 此前没暴露只是因为出厂配置里所有 Map 都是空表（`= {}`），没有子键可下钻。
        toml::Value::Table(_)
            if !prefix.is_empty()
                && matches!(field(prefix).map(|f| f.ty), Some(FieldType::Map(_))) =>
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
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// 跨仓契约：`Float` 键必须吃得下 **TOML 整数**。
    ///
    /// wind-setting 的 number 控件写回时会做 `if v.fract() == 0.0 { Value::from(v as i64) }`
    /// ——用户在设置页把半衰期填 `72`，落到 config.toml 里就是整数 `72` 而非 `72.0`。
    /// 而 core 侧这些字段是 `f64`。两边隔着一个进程和一次文件往返，**没有任何编译期约束**
    /// 能发现不匹配；真出问题的表现是「设置页填了值，重启后回到默认」这种最难查的静默失效。
    #[test]
    fn float_fields_accept_integer_toml_values() {
        use crate::config::{CodetableFrequency, PinyinFrequency};
        let ct: CodetableFrequency =
            toml::from_str("half_life = 72").expect("码表 half_life 必须接受 TOML 整数");
        assert_eq!(ct.half_life, 72.0);
        let py: PinyinFrequency =
            toml::from_str("half_life = 72").expect("拼音 half_life 必须接受 TOML 整数");
        assert_eq!(py.half_life, 72.0);
        // 反向对照：小数形式当然也要能读，否则上面那条可能只是「两边都坏」。
        let ct2: CodetableFrequency = toml::from_str("half_life = 4.5").unwrap();
        assert_eq!(ct2.half_life, 4.5);
    }

    /// L1↔L2 同源：`ui.toolbar.items` 的出厂值在 Rust 默认值与 `data/config.toml` 里
    /// 必须逐项相同（`config-design-rules.md` §R4）。
    ///
    /// 这个键的顺序**有语义**（数组顺序即工具栏渲染顺序），所以不能只比集合。两侧任一
    /// 处改了顺序或成员而另一处没跟，新装用户与老用户看到的工具栏就不一样——而这种
    /// 差异不会有任何其它测试报出来。
    ///
    /// 放在本 crate 是因为**只有这里读得到 L2**（`data_config_toml()`）；协调器侧那条
    /// 同名意图的测试只能覆盖 L1，两条不重复。
    #[test]
    fn toolbar_items_l1_matches_l2() {
        let l1 = crate::Config::default().ui.toolbar.items;
        let l2: Vec<String> = data_config_toml()
            .get("ui")
            .and_then(|u| u.get("toolbar"))
            .and_then(|t| t.get("items"))
            .and_then(|v| v.as_array())
            .expect("data/config.toml 缺少 ui.toolbar.items")
            .iter()
            .map(|v| v.as_str().expect("items 元素须为字符串").to_string())
            .collect();
        assert_eq!(l1, l2, "ui.toolbar.items 的 L1 默认值与 L2 出厂文件不一致");
        // 两侧都必须是登记过的键，否则出厂配置里就躺着一个会被解析跳过的条目。
        for k in &l2 {
            assert!(
                crate::TOOLBAR_ITEM_KEYS.contains(&k.as_str()),
                "出厂 items 含未登记条目 {k:?}"
            );
        }
    }

    /// 解析仓库内系统预置 `data/config.toml`。
    /// ★★★ 出厂 `keys.key_actions` 里的动词必须真的编译得进热键表。
    ///
    /// 动词值域散在**三处**：`BoundAction::parse`（引导键链 / 方案级表）、
    /// `hotkey::hotkey_action_entry`（组合键白名单）、协调器的分派臂。只加第一处的症状是
    /// ——出厂热键按下去什么都不发生，与「这个键根本没绑」完全同形，唯一线索是日志里
    /// 一行 `组合键不支持动词 ... 忽略`。软键盘的 `ctrl+shift+k` 就是这么漏的，
    /// 部署到真机翻日志才发现。
    #[test]
    fn factory_combo_key_actions_are_accepted_by_the_hotkey_compiler() {
        let Some(actions) = data_config_toml()
            .get("keys")
            .and_then(|k| k.get("key_actions"))
            .and_then(|v| v.as_table())
            .cloned()
        else {
            return; // 出厂没配 key_actions 时无可校验
        };
        for (key, action) in &actions {
            let action = action.as_str().expect("key_actions 的值须为字符串").trim();
            if action.is_empty() {
                continue;
            }
            // 只校验走组合键通路的那些；单键与纯修饰键有各自的消费者。
            if crate::hotkey::route_of_key_action(key)
                != Some(crate::hotkey::KeyActionRoute::Hotkey)
            {
                continue;
            }
            assert!(
                crate::hotkey::hotkey_action_entry(action).is_some(),
                "出厂 keys.key_actions 的 {key:?} = {action:?} 不被组合键白名单接受，                 按下去会静默无反应（见 hotkey_action_entry）"
            );
        }
    }

    fn data_config_toml() -> toml::Value {
        // CARGO_MANIFEST_DIR = <repo>/wind_input/crates/wind-config
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../data/config.toml");
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("读取 {path} 失败（Stage1 注册表对照需要）: {e}"));
        toml::from_str(&content).expect("data/config.toml 解析失败")
    }

    fn leaf_keys_of(value: &toml::Value) -> Vec<String> {
        let mut out = Vec::new();
        collect_leaf_keys("", value, &mut out);
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn leaf_keys_drills_tables_and_keeps_arrays_maps_as_leaves() {
        let keys: BTreeSet<String> = config_leaf_keys().into_iter().collect();
        // 标量叶子
        assert!(keys.contains("ui.candidate.per_page"), "应含标量叶子");
        // 嵌套标量
        assert!(
            keys.contains("keys.overflow.number_key"),
            "应含嵌套标量叶子"
        );
        // 空 HashMap 作叶子保留
        assert!(
            keys.contains("input.punct.custom_mappings"),
            "空表(custom_mappings)应作叶子保留"
        );
        // 数组作叶子（不下钻元素）
        assert!(keys.contains("schema.available"), "数组应作叶子");
        assert!(
            keys.contains("schema.mix_modes"),
            "结构体数组应作单一叶子，不展开元素"
        );
        // 中间表不应作为叶子出现
        assert!(!keys.contains("ui.candidate"), "中间表不应是叶子");
        assert!(!keys.contains("ui"), "顶层表不应是叶子");
    }

    #[test]
    fn leaf_entries_flattens_table_to_key_value_pairs() {
        let v: toml::Value = toml::from_str(
            "[ui.candidate]\nper_page = 9\nlayout = \"vertical\"\n[input.auto_pair]\nchinese = false\n",
        )
        .unwrap();
        let entries = leaf_entries(&v);
        assert!(
            entries
                .iter()
                .any(|(k, val)| k == "ui.candidate.per_page" && val.as_integer() == Some(9))
        );
        assert!(
            entries
                .iter()
                .any(|(k, val)| k == "ui.candidate.layout" && val.as_str() == Some("vertical"))
        );
        assert!(
            entries
                .iter()
                .any(|(k, val)| k == "input.auto_pair.chinese" && val.as_bool() == Some(false))
        );
        // 不应出现中间表键
        assert!(!entries.iter().any(|(k, _)| k == "ui.candidate"));
    }

    #[test]
    fn registry_covers_every_config_key() {
        let struct_keys: BTreeSet<String> = config_leaf_keys().into_iter().collect();
        let registry_keys: BTreeSet<String> =
            registry().iter().map(|f| f.key.to_string()).collect();

        let missing: Vec<&String> = struct_keys.difference(&registry_keys).collect();
        let extra: Vec<&String> = registry_keys.difference(&struct_keys).collect();

        assert!(
            missing.is_empty() && extra.is_empty(),
            "注册表与 Config 不一致：\n  注册表缺失({}): {:?}\n  注册表多余({}): {:?}",
            missing.len(),
            missing,
            extra.len(),
            extra
        );
    }

    /// 刻意不写进 `data/config.toml` 的键（配套 [`data_config_toml_covers_registry`]）。
    ///
    /// 二者均为**结构体数组**（`StructList`）。本文件写出的数组是整体覆盖而非合并，
    /// 一旦列进预置文件就会把当时的定义冻结成快照，日后改代码侧默认值全被静默遮蔽。
    /// 故其真相源留在 `config.rs` 的 `Default`，预置文件只在注释里说明。
    ///
    /// **加条目前请三思**：豁免一个键 = 它的默认值从此不在说明书里，
    /// 只有「写进去会造成实际危害」才够格，「懒得写」不够格。
    const ABSENT_FROM_DATA_CONFIG: &[&str] = &[
        "schema.mix_modes",
        // 注释词库挂载列表：**出厂为空数组**（不随附任何注释词库——词典内容多有版权，
        // 由用户自行放置）。写进预置文件除了一行 `comment_dicts = []` 没有任何信息量，
        // 而它一旦以数组表形态出现，用户增删条目就会与预置层的整表覆盖语义纠缠。
        // 格式与示例在文档站 customize/candidate-comment。
        "ui.comment_dicts",
        // 工具栏自定义按钮：同上，**出厂为空数组**。写出一行 `buttons = []` 没有信息量，
        // 却会把空数组冻结成 L2 值；而它一旦以数组表形态出现，用户增删按钮就要与预置层
        // 的整表覆盖语义纠缠。格式与示例写在 `[ui.toolbar]` 段的注释里（不是数组表，
        // 只是注释），用户照抄即可。
        "ui.toolbar.buttons",
    ];

    /// `data/config.toml` 必须显式列出注册表里的每一个键（豁免名单除外）。
    ///
    /// 与 [`data_config_toml_has_no_orphan_keys`] 互为反向：那个测试管「不许多」，
    /// 这个管「不许少」。缺了这一侧，新增配置项时漏写预置文件不会被任何测试发现——
    /// 本测试补上前，预置文件已积压 31 个缺键（模糊音 10 项、自动造词 5 项、
    /// 拼音调频衰减 3 项、截图热键、日志滚动 2 项等）。
    #[test]
    fn data_config_toml_covers_registry() {
        let toml_keys: BTreeSet<String> = leaf_keys_of(&data_config_toml()).into_iter().collect();
        let missing: Vec<&str> = registry()
            .iter()
            .map(|f| f.key)
            .filter(|k| !toml_keys.contains(*k) && !ABSENT_FROM_DATA_CONFIG.contains(k))
            .collect();
        assert!(
            missing.is_empty(),
            "data/config.toml 缺少 {} 个已登记键（预置文件应完整列出全部可配置项，\
             既是出厂默认值也是说明书）: {:?}",
            missing.len(),
            missing
        );
    }

    /// 豁免名单自身不能腐烂：里面的键必须仍在注册表中，且确实没写进预置文件。
    /// 否则删掉某个字段后，名单会留下一条永远为真的死条目。
    #[test]
    fn absent_allowlist_stays_accurate() {
        let toml_keys: BTreeSet<String> = leaf_keys_of(&data_config_toml()).into_iter().collect();
        for key in ABSENT_FROM_DATA_CONFIG {
            assert!(is_known_key(key), "豁免名单含未登记键（已删字段？）: {key}");
            assert!(
                !toml_keys.contains(*key),
                "{key} 已写进 data/config.toml，应从豁免名单移除"
            );
        }
    }

    #[test]
    fn data_config_toml_has_no_orphan_keys() {
        let struct_keys: BTreeSet<String> = config_leaf_keys().into_iter().collect();
        let toml_keys = leaf_keys_of(&data_config_toml());
        let orphans: Vec<&String> = toml_keys
            .iter()
            .filter(|k| !struct_keys.contains(*k))
            .collect();
        assert!(
            orphans.is_empty(),
            "data/config.toml 含 {} 个孤立键（struct 无对应字段，会被静默丢弃）: {:?}",
            orphans.len(),
            orphans
        );
    }

    #[test]
    fn field_lookup_finds_registered_key_and_rejects_unknown() {
        let f = field("ui.candidate.layout").expect("已登记键应查到");
        assert_eq!(f.key, "ui.candidate.layout");
        assert!(matches!(f.ty, FieldType::Enum(_)));
        assert!(is_known_key("keys.overflow.number_key"));
        assert!(!is_known_key("ui.candidate.bogus"), "未登记键应返回 None");
        assert!(!is_known_key("totally.made.up"));
    }

    /// 守卫：core 内部 setter 硬编码的 key 路径必须都在注册表中（防拼写/漂移）。
    /// 新增 `Config::set_user_*` 调用点时，把其路径加到这里。
    #[test]
    fn internal_setter_paths_are_registered() {
        const INTERNAL_PATHS: &[&str] = &[
            "schema.active",
            "ui.theme.style",
            "ui.theme.name",
            "ui.candidate.preedit_display",
            "ui.toolbar.visible",
        ];
        for p in INTERNAL_PATHS {
            assert!(is_known_key(p), "内部 setter 路径未在注册表登记: {p}");
        }
    }

    #[test]
    fn validate_accepts_correct_types() {
        assert!(validate("ui.candidate.per_page", &toml::Value::Integer(9)).is_ok());
        assert!(validate("ui.candidate.hide_window", &toml::Value::Boolean(true)).is_ok());
        assert!(
            validate(
                "ui.candidate.layout",
                &toml::Value::String("vertical".into())
            )
            .is_ok()
        );
        // Float 字段接受整数值（宽松）
        assert!(validate("ui.candidate.font_size", &toml::Value::Integer(18)).is_ok());
        assert!(validate("ui.candidate.font_size", &toml::Value::Float(18.0)).is_ok());
        // Enum 允许空串成员（pager_bar_display 含 ""）
        assert!(
            validate(
                "ui.candidate.pager_bar_display",
                &toml::Value::String("".into())
            )
            .is_ok()
        );
        // 数组 / 表
        assert!(
            validate(
                "schema.available",
                &toml::Value::Array(vec![toml::Value::String("wubi86".into())])
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_rejects_unknown_key() {
        assert_eq!(
            validate("ui.candidate.bogus", &toml::Value::Integer(1)),
            Err(ValidateError::UnknownKey)
        );
    }

    #[test]
    fn validate_rejects_type_mismatch() {
        let r = validate(
            "ui.candidate.per_page",
            &toml::Value::String("seven".into()),
        );
        assert!(
            matches!(r, Err(ValidateError::TypeMismatch { .. })),
            "{r:?}"
        );
        let r2 = validate("ui.candidate.hide_window", &toml::Value::Integer(1));
        assert!(
            matches!(r2, Err(ValidateError::TypeMismatch { .. })),
            "{r2:?}"
        );
    }

    #[test]
    fn validate_rejects_enum_out_of_range() {
        let r = validate(
            "ui.candidate.layout",
            &toml::Value::String("diagonal".into()),
        );
        assert!(
            matches!(r, Err(ValidateError::EnumOutOfRange { .. })),
            "{r:?}"
        );
    }

    #[test]
    fn validate_rejects_strlist_with_non_string_element() {
        let r = validate(
            "schema.available",
            &toml::Value::Array(vec![toml::Value::Integer(1)]),
        );
        assert!(
            matches!(r, Err(ValidateError::TypeMismatch { .. })),
            "{r:?}"
        );
    }

    #[test]
    fn registry_types_match_default_values() {
        let default = toml::Value::try_from(Config::default()).unwrap();
        for field in registry() {
            let value = navigate(&default, field.key)
                .unwrap_or_else(|| panic!("默认配置缺少注册表声明的键: {}", field.key));
            assert!(
                type_matches(field.ty, value),
                "键 {} 声明类型 {:?} 与默认值实际类型不符: {:?}",
                field.key,
                field.ty,
                value
            );
        }
    }

    /// 按点分路径在 TOML 表中导航取值。
    fn navigate<'a>(root: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
        let mut cur = root;
        for part in key.split('.') {
            cur = cur.as_table()?.get(part)?;
        }
        Some(cur)
    }

    fn type_matches(ty: FieldType, value: &toml::Value) -> bool {
        match ty {
            FieldType::Bool => value.is_bool(),
            FieldType::Int => value.is_integer(),
            FieldType::Float => value.is_float(),
            FieldType::Str | FieldType::Enum(_) => value.is_str(),
            FieldType::StrList | FieldType::StructList => value.is_array(),
            FieldType::Map(_) => value.is_table(),
        }
    }

    /// data/config.toml 每个已登记叶子键的值，必须通过 registry 校验（类型 / enum 合法）。
    /// 注意：config.toml 作为系统预置可合法覆盖 code default，故此处只校验「合法」而非「等于默认」。
    #[test]
    fn data_config_toml_values_pass_validation() {
        let toml_val = data_config_toml();
        let mut bad = Vec::new();
        for (key, value) in leaf_entries(&toml_val) {
            // 未登记键由 data_config_toml_has_no_orphan_keys 守护，这里只校验已登记项的值
            if field(&key).is_none() {
                continue;
            }
            if let Err(e) = validate(&key, &value) {
                bad.push(format!("{key}: {e}"));
            }
        }
        assert!(
            bad.is_empty(),
            "data/config.toml 含非法值（类型/enum 不符 registry）:\n{}",
            bad.join("\n")
        );
    }

    /// [`SCHEMA_OVERRIDES`] 里的每条登记都必须指到真实存在的键（或非空的段）。
    ///
    /// 这道闸拦的是**键名漂移**：全局键被改名或删掉之后，表里那条会静默失配，
    /// 设置页那一行就不再提示可被方案覆盖——而这种缺失没有任何别的东西看得见。
    ///
    /// ⚠️ 它拦不住反方向（core 新增了方案级字段却没往表里加一条）：那层对应关系只存在于
    /// `resolved()` 的函数体里，类型系统与本测试都看不见。见 `SCHEMA_OVERRIDES` 的文档。
    #[test]
    fn schema_overrides_point_at_real_keys() {
        for o in SCHEMA_OVERRIDES {
            match o.key.strip_suffix('.') {
                // 段前缀：该段下至少要有一个已登记的叶子键。
                Some(prefix) => {
                    let n = registry()
                        .iter()
                        .filter(|f| {
                            f.key
                                .strip_prefix(prefix)
                                .is_some_and(|r| r.starts_with('.'))
                        })
                        .count();
                    assert!(
                        n > 0,
                        "SCHEMA_OVERRIDES 的段前缀 `{}` 在注册表里一个键都没匹配到——\
                         多半是那一段被改名或删了",
                        o.key
                    );
                }
                None => assert!(
                    is_known_key(o.key),
                    "SCHEMA_OVERRIDES 登记了未知键 `{}`（改名或已删除？）",
                    o.key
                ),
            }
        }
    }

    /// 前缀匹配不能误伤同前缀的兄弟段，也不能把段名本身当成叶子键。
    #[test]
    fn schema_override_prefix_matches_only_that_section() {
        // 段内叶子命中。
        assert!(schema_override_of("schema.codetable.top_code_commit").is_some());
        // 段名本身不是叶子键，不该命中。
        assert!(schema_override_of("schema.codetable").is_none());
        // 同前缀的兄弟段不该被误伤（`schema.codetableX` 与 `schema.codetable` 只差一个字符）。
        assert!(schema_override_of("schema.codetableX.foo").is_none());
        // 精确登记项。
        assert!(schema_override_of("input.punct.custom_enabled").is_some());
        // 同段但没登记的键不该命中（`smart_list` 不在方案级下放范围）。
        assert!(schema_override_of("input.punct.smart_list").is_none());
    }

    /// 段前缀**不递归**：子段要么自己登记，要么不该被标记。
    ///
    /// 现场：第一版用递归前缀，`schema.codetable.auto_phrase.*` 六个键被一并标成
    /// 「可被方案覆盖」，而 `CodetableGlobal::resolved` 折叠到 `frequency` 就返回了，
    /// 根本不读 auto_phrase。标错比漏标更糟——用户会去方案里写一段不会被读的配置，
    /// 写了没反应还以为是自己写错了。
    #[test]
    fn section_prefix_does_not_leak_into_subsections() {
        // 登记了的子段：命中，且命中的是**子段那一条**（提示文案要说 [codetable.frequency]）。
        let hit = schema_override_of("schema.codetable.frequency.half_life")
            .expect("frequency 子段已单独登记，应命中");
        assert_eq!(hit.section, "[codetable.frequency]");
        // 没登记的子段：绝不能因为父段登记了就跟着被标。
        for k in [
            "schema.codetable.auto_phrase.enabled",
            "schema.codetable.auto_phrase.min_phrase_len",
            "schema.codetable.auto_phrase.temp_max_entries",
        ] {
            assert!(
                schema_override_of(k).is_none(),
                "{k} 没有方案级形态（resolved 不折叠 auto_phrase），不该被标记"
            );
        }
    }
}
