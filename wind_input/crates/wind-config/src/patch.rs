//! 配置片段（fragment）的解析、展平与校验——`config.previewPatch` / `config.applyPatch`
//! 的纯逻辑层。不碰文件系统：RPC 层负责取当前生效配置与经 `set_user_value` 落盘，
//! 本模块只回答「这段 TOML 拆成哪些键、每个键合不合法、应用后从什么值变成什么值」。
//!
//! 片段的键域 = `config_schema::REGISTRY` 登记键 ∪ [`ALLOWED_UNREGISTERED_KEYS`]。
//! 展平在这两类键处**停止下钻**：`StructList` 键（如 `schema.mix_modes`）整个子树就是一个
//! 配置值，切成伪键是错的语义——与 `prune_redundant` 用 `is_known_key` 把下钻子路径整体
//! 排除是同一道保险。
//!
//! **Map 键是唯一的例外，且只多下钻一层**（`keys.key_actions` / `keys.schema_hotkeys` /
//! `keys.session_actions` / `input.punct.custom_mappings`）：
//!
//! - 片段里 Map 键下的表**恒为逐条合并**（upsert：并入当前生效表，同名条目覆盖，其余保留）。
//!   片段**不能**整表替换、**不能**删条目——分发包带整表替换会清掉用户既有绑定，这正是本
//!   语义存在的理由。顺带消灭了「空表 = 清空」的脚枪：空表 = 无条目 = no-op。
//! - 条目名**不并入点分键**：`custom_mappings` 的条目名可以含 `.`（如 `"."`），拼进点分键
//!   就再也拆不回来。条目名由 [`PatchEntry::map_entry`] 独立承载，`key` 恒为父 Map 键。
//! - 落盘的最终键值由 [`writes`] 算出（父 Map 键一条，值 = 合并后整表）。
//!
//! 错误只有三类：整体 TOML 解析失败（[`parse_fragment`] 返回 `Err`，不产出任何条目）、
//! 未知配置键、类型或取值不合法（后两类落在条目的 `error` 字段，逐键/逐条目报告）。
//!
//! **保留顶层段 [`RESERVED_TOP_SECTION`]（`[package]`）**：片段可自带一段说明元信息
//! （`title` / `description`），供导入界面回答「这个包是干什么的」。它是片段里**唯一**的
//! 保留顶层段——展平时整段跳过，既不产出配置条目、也不报「未知配置键」；config.toml
//! 没有 `package` 域，不存在与真实配置键撞名的可能。段内未知子键一律忽略（向前兼容：
//! 分发包与文本信封把 `kind` / `format_version` 写在同一段里，旧客户端遇到新字段不能炸）。
//! 提取走 [`extract_info`]，净化与限额走 [`sanitize_info_text`]——三种载体
//! （片段 / 分发包 `package.toml` / 文本信封）共用这一份实现，wind-transfer 直接复用。

use std::collections::HashMap;

use serde::Serialize;

use crate::config::{Config, ConfigDegradation};
use crate::config_schema;

/// 合法但刻意不进 REGISTRY 的配置键白名单。
///
/// 这些是 `Option<T>` 三态字段：出厂值恰是「键不存在」（`skip_serializing_if`），
/// 不出现在 `Config::default()` 的序列化键集里，登记进 REGISTRY 会被
/// `registry_covers_every_config_key` 判「多余」——见 REGISTRY 文档
/// 「三态键（`Option<T>`，默认 `None`）刻意不登记」一节。片段校验若只认登记键，
/// 用户合法手写的这些键就会被误报「未知配置键」，故在此显式列出。
///
/// 目前唯一的家族是模式级注释模板覆盖（[`crate::config::CommentTemplateOverride`]）。
/// `mix_modes` 条目内的同名字段不在此列：它们在 `schema.mix_modes`（StructList）子树内，
/// 展平时随整棵子树作一个值，不会以点分键形态出现。宁缺勿滥——拿不准的键不进名单，
/// 误报「未知」可改，误放行则绕过了 REGISTRY 这道门。
///
/// 守门测试（本文件 tests）保证名单不腐烂：每个键必须 (a) 不在 REGISTRY
/// （日后登记了就该从名单删除）、(b) 真实存在于 `Config` 结构（写入后能反序列化并原值读回）。
pub const ALLOWED_UNREGISTERED_KEYS: &[&str] = &[
    "input.temp_english.comment_template_vertical",
    "input.temp_english.comment_template_horizontal",
    "input.temp_pinyin.comment_template_vertical",
    "input.temp_pinyin.comment_template_horizontal",
    "input.url.comment_template_vertical",
    "input.url.comment_template_horizontal",
];

/// 预览条目：片段里的一个配置键及其应用效果。
///
/// Map 键的每个条目各占一条：`key` = 父 Map 键、[`Self::map_entry`] = 条目名。
#[derive(Debug, Clone, Serialize)]
pub struct PatchEntry {
    /// 点分配置键。Map 条目取**父 Map 键**（条目名不并入，见模块文档）。
    pub key: String,
    /// Map 条目名；`None` = 本条是普通标量/整值键。
    #[serde(rename = "mapEntry", skip_serializing_if = "Option::is_none")]
    pub map_entry: Option<String>,
    /// 当前生效值（按路径从传入的当前配置树取；Map 条目取表内该条目的值，
    /// 缺席 = 新增；白名单键未设置时无值）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<toml::Value>,
    /// 片段给出的新值。
    pub next: toml::Value,
    /// 校验错误（未知配置键 / 类型或取值不合法）；`None` = 本条可应用。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 风险提示：本条**可以应用**，但导入界面须显著提示后果。`None` = 无需提示。
    ///
    /// 与 [`Self::error`] 是两回事：那个说「这条写错了、不会应用」，这个说
    /// 「这条没问题、但你得知道它意味着什么」。二者可同时为 `None`（常态），
    /// 也可只有本项（合法但有风险）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// 应用后会让**配置本身获得执行外部程序的能力**的键，及给用户看的提示语。
///
/// # 这张表为什么存在
///
/// 配置片段的门是本仓最开放的一档（`package-format.md` §5：「片段仅能改登记键」），
/// 而这个前提原本由一件事担保——**`config.toml` 里没有任何可执行内容**。能执行程序的
/// 短语属于用户数据，只进备份包、永不进分发包，风险从格式层面就被挡住了。
///
/// `ui.toolbar.buttons`（0.119）第一次打破了这个担保：它的 `action` 是 cmdbar 表达式，
/// 而 `StructList` 键在片段里是**整值覆盖**。于是「导入片段 → 工具栏多了个按钮 →
/// 用户点一下」就是一条无提示的任意程序执行路径。
///
/// 本表的处置是**提示而非阻断**：用户自己写这类配置是正当需求，拦掉等于把功能废掉；
/// 真正缺的是「你正在从别人那里接受这个」这一句话。
///
/// ⚠️ **日后再加能执行程序 / 改写启动项一类的配置键时，必须登记到这里。**
/// 判据不是「这个键危不危险」，而是「一份陌生片段写了它之后，用户的某次寻常操作
/// 会不会变成执行对方给的代码」。
const RISKY_KEYS: &[(&str, &str)] = &[(
    "ui.toolbar.buttons",
    "该片段会在工具栏上添加按钮，按钮可启动程序或打开网址——点击即执行。请确认来源可信。",
)];

/// 取某个键的风险提示（无风险返回 `None`）。
fn risk_warning(key: &str) -> Option<String> {
    RISKY_KEYS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, msg)| (*msg).to_string())
}

/// 片段里唯一的保留顶层段名（见模块文档）。展平时整段跳过，说明元信息由
/// [`extract_info`] 单独提取。
pub const RESERVED_TOP_SECTION: &str = "package";

/// `title` 的字符数上限（不是字节数——CJK 说明按字符算才是用户看到的长度）。
pub const INFO_TITLE_MAX_CHARS: usize = 200;

/// `description` 的字符数上限。
pub const INFO_DESCRIPTION_MAX_CHARS: usize = 4000;

/// 分发内容的说明元信息，来自保留段 `[package]`。
///
/// 两字段都缺省（或全空白）时整个结构不产生——[`extract_info`] 返回 `None`，
/// RPC 响应里就没有 `info` 字段。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PatchInfo {
    /// 单行标题。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 多行说明。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 解析片段文本。失败即整体错误，调用方不应再走展平/校验。
pub fn parse_fragment(text: &str) -> Result<toml::Value, String> {
    toml::from_str::<toml::Value>(text).map_err(|e| format!("TOML 解析失败: {e}"))
}

/// 从片段的 `[package]` 段提取说明元信息。
///
/// - 段不存在、或 `title`/`description` 都缺省（含全空白）→ `Ok(None)`；
/// - 段内**未知子键一律忽略**（向前兼容，`kind` / `format_version` 也在此被忽略）；
/// - 段不是表、字段不是字符串、内容违反 [`sanitize_info_text`] 的限额与净化规则 → `Err`。
///
/// 错误是**整片段错误**（与 TOML 解析失败同级），不是逐条目的 `error`：说明本身写坏了，
/// 逐键 diff 再漂亮也没有意义，且预览与应用必须同判据（分发前该自测，不静默截断）。
pub fn extract_info(fragment: &toml::Value) -> Result<Option<PatchInfo>, String> {
    let Some(section) = fragment.get(RESERVED_TOP_SECTION) else {
        return Ok(None);
    };
    let Some(table) = section.as_table() else {
        return Err(format!("[{RESERVED_TOP_SECTION}] 应为表"));
    };
    let title = info_field(table, "title", sanitize_title)?;
    let description = info_field(table, "description", sanitize_description)?;
    if title.is_none() && description.is_none() {
        return Ok(None);
    }
    Ok(Some(PatchInfo { title, description }))
}

/// 取 `[package]` 段内一个说明字段：缺省 → `None`；非字符串 → 报错（不静默忽略——
/// 忽略就成了「写了没生效」）；净化后全空白 → `None`（视同没写）。
fn info_field(
    table: &toml::map::Map<String, toml::Value>,
    name: &str,
    sanitize: fn(&str) -> Result<String, String>,
) -> Result<Option<String>, String> {
    let Some(v) = table.get(name) else {
        return Ok(None);
    };
    let Some(raw) = v.as_str() else {
        return Err(format!(
            "{RESERVED_TOP_SECTION}.{name} 应为字符串（实际为 {}）",
            v.type_str()
        ));
    };
    let text = sanitize(raw)?;
    Ok((!text.is_empty()).then_some(text))
}

/// `title` 的净化入口：单行、上限 [`INFO_TITLE_MAX_CHARS`]。
///
/// 「哪个字段配哪套限额」只在这里定一次——三种载体各自调 [`sanitize_info_text`] 传参，
/// 迟早有一处把 `allow_newline` 传反。
pub fn sanitize_title(raw: &str) -> Result<String, String> {
    sanitize_info_text(
        &format!("{RESERVED_TOP_SECTION}.title"),
        raw,
        INFO_TITLE_MAX_CHARS,
        false,
    )
}

/// `description` 的净化入口：允许换行、上限 [`INFO_DESCRIPTION_MAX_CHARS`]。
pub fn sanitize_description(raw: &str) -> Result<String, String> {
    sanitize_info_text(
        &format!("{RESERVED_TOP_SECTION}.description"),
        raw,
        INFO_DESCRIPTION_MAX_CHARS,
        true,
    )
}

/// 说明文本的净化与限额校验——**三种载体唯一的一份实现**（片段 / 分发包 `package.toml` /
/// 文本信封都走这里，wind-transfer 复用，不得另写）。说明是分发者提供的任意文本、会直接
/// 渲染进 UI，故：
///
/// - `\r\n` 与孤立 `\r` 归一为 `\n`（分发者的平台行尾差异不该算「非法字符」）；
/// - 除 `\t` 与 `\n` 外的 C0 控制字符（U+0000–U+001F）与 U+007F 一律拒绝，错误点名字段；
/// - `allow_newline = false` 时，**trim 之后**仍含 `\n` 即拒绝；
/// - 超过 `max_chars` **字符**（不是字节）即拒绝，错误写明上限与实际长度——分发前该自测，
///   静默截断只会让分发者以为写全了；
/// - 返回 `trim()` 后的文本，调用方按「空串 = 没写」处理。
///
/// **换行判定在 trim 之后**（长度同）：TOML 多行字符串是写 title 的合法语法，尾随换行是
/// 该语法的固有产物而非分发者的错误——
/// ```toml
/// title = """
/// 快符方案
/// """
/// ```
/// 的值是 `"快符方案\n"`，拒掉它只会让人困惑「我明明只写了一行」。「不含换行」要挡的是
/// **title 得是单行**，即 `"第一行\n第二行"`，那种仍然拒绝。
///
/// **控制字符判定则在 trim 之前**：那类字符 trim 不掉（U+0007 之流不是空白），
/// 而 VT/FF 这些既是控制字符又算空白的，先 trim 就会被悄悄放行——不该宽容。
pub fn sanitize_info_text(
    field: &str,
    raw: &str,
    max_chars: usize,
    allow_newline: bool,
) -> Result<String, String> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    for ch in normalized.chars() {
        if ch == '\t' || ch == '\n' {
            continue;
        }
        let cp = ch as u32;
        if cp < 0x20 || cp == 0x7f {
            return Err(format!("{field} 含控制字符 U+{cp:04X}"));
        }
    }
    let trimmed = normalized.trim();
    if !allow_newline && trimmed.contains('\n') {
        return Err(format!("{field} 不能包含换行"));
    }
    let chars = trimmed.chars().count();
    if chars > max_chars {
        return Err(format!("{field} 过长（{chars} 字符，上限 {max_chars}）"));
    }
    Ok(trimmed.to_string())
}

/// 展平片段并逐条校验、取当前值。`current` 为当前生效配置的 TOML 值树
/// （RPC 层从 `Config::load` 序列化得到）。条目顺序 = 片段遍历顺序。
///
/// 点分键与嵌套表两种写法在 TOML 解析层就已归一为同一棵表树，本函数天然视其等价。
pub fn preview(fragment: &toml::Value, current: &toml::Value) -> Vec<PatchEntry> {
    let mut entries = Vec::new();
    flatten("", fragment, &mut entries);
    for e in &mut entries {
        // 风险提示在**这个单点**补，而不是在 flatten 的三个 PatchEntry 构造处各填一次
        // ——那样加第四个构造点时必然漏，而漏的表现是「提示没出现」，无人会发现。
        //
        // 放在 error 判断之前：一条写错了的危险键**照样要提示**。用户看到「这条有错」
        // 往往会去改对它再导入一次，那时提示就该已经说过。
        e.warning = risk_warning(&e.key);
        // 未知键在展平期已定性，无当前值可取。
        if e.error.is_some() {
            continue;
        }
        let path: Vec<&str> = e.key.split('.').collect();
        match &e.map_entry {
            // Map 条目：校验「父键 = 只含本条目的单元素表」，当前值取表内同名条目。
            Some(name) => {
                // 空条目名（`"" = "x"`）在 TOML 里合法，语义上却没有任何键能对应它
                // （按键名/方案 id/源字符各自都不可能是空）。放行等于往用户配置里写一条
                // 永远匹配不上、也删不掉的死条目，故当校验错误报出。
                e.error = if name.is_empty() {
                    Some("条目名不能为空".to_string())
                } else {
                    validate_map_entry(&e.key, name, &e.next).err()
                };
                e.current = crate::config::get_nested(current, &path)
                    .and_then(|v| v.as_table())
                    .and_then(|t| t.get(name))
                    .cloned();
            }
            None => {
                e.error = validate_value(&e.key, &e.next).err();
                e.current = crate::config::get_nested(current, &path).cloned();
            }
        }
    }
    entries
}

/// 该键是否登记为 `Map`（片段里其下的表逐条合并，见模块文档）。
fn is_map_key(key: &str) -> bool {
    matches!(
        config_schema::field(key).map(|f| f.ty),
        Some(config_schema::FieldType::Map(_))
    )
}

/// 递归展平：走到路径 `prefix` 时，登记键/白名单键 → 整子树为一个值、停止下钻
/// （Map 键除外：再下钻一层，逐条目产出）；否则表继续下钻；叶子而不是任何已知键 →
/// 记「未知配置键」。未知路径上的空表（如孤零零一行 `[input.foo]`）没有叶子可报，
/// 静默不产出条目；Map 键下的空表同理不产出条目（空表 = no-op，不是「清空」）。
///
/// 顶层保留段 [`RESERVED_TOP_SECTION`] 整段跳过：它是说明元信息，不是配置
/// （由 [`extract_info`] 单独提取），报「未知配置键」就成了误报。
fn flatten(prefix: &str, value: &toml::Value, out: &mut Vec<PatchEntry>) {
    let is_patch_key = !prefix.is_empty()
        && (config_schema::is_known_key(prefix) || ALLOWED_UNREGISTERED_KEYS.contains(&prefix));
    if is_patch_key {
        // Map 键 + 表 → 逐条目。非表值（如 `keys.key_actions = 5`）落回单条，
        // 由 validate 按 Map 类型报「类型应为 table」。
        if is_map_key(prefix)
            && let toml::Value::Table(t) = value
        {
            for (name, v) in t {
                out.push(PatchEntry {
                    key: prefix.to_string(),
                    map_entry: Some(name.clone()),
                    current: None,
                    next: v.clone(),
                    error: None,
                    // 三个构造点一律置 None，由 `preview` 单点按键补——见那里的注释。
                    warning: None,
                });
            }
            return;
        }
        out.push(PatchEntry {
            key: prefix.to_string(),
            map_entry: None,
            current: None,
            next: value.clone(),
            error: None,
            warning: None,
        });
        return;
    }
    match value {
        toml::Value::Table(t) => {
            for (k, v) in t {
                let child = if prefix.is_empty() {
                    // 保留段整段跳过（含 `package = 5` 这种写坏的形态：类型问题由
                    // extract_info 报，展平层不该把它当配置键）。
                    if k == RESERVED_TOP_SECTION {
                        continue;
                    }
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&child, v, out);
            }
        }
        _ => out.push(PatchEntry {
            key: prefix.to_string(),
            map_entry: None,
            current: None,
            next: value.clone(),
            error: Some("未知配置键".to_string()),
            warning: None,
        }),
    }
}

/// 校验单个键值。登记键复用 `config.setItems` 的注册表校验；白名单键没有类型声明，
/// 唯一的真相是 `Config` 结构本身——把值合并进默认配置再整体反序列化，
/// 类型不符时 serde 会带字段路径报错。
fn validate_value(key: &str, value: &toml::Value) -> Result<(), String> {
    if config_schema::is_known_key(key) {
        return config_schema::validate(key, value).map_err(|e| e.to_string());
    }
    let mut base =
        toml::Value::try_from(Config::default()).map_err(|e| format!("默认配置序列化失败: {e}"))?;
    let path: Vec<&str> = key.split('.').collect();
    if let toml::Value::Table(t) = &mut base {
        crate::config::set_nested(t, &path, value.clone());
    }
    base.try_into::<Config>()
        .map(|_| ())
        .map_err(|e| format!("类型或取值不合法: {e}"))
}

/// 校验 Map 键下的单个条目值。REGISTRY 只声明父键是 `Map`（`value.is_table()`），
/// 对条目值类型一无所知，唯一的真相是 `Config` 结构本身——把「只含本条目的单元素表」
/// 并进默认配置的该 Map 键再整体反序列化，serde 会带字段路径报错。与
/// [`validate_value`] 的白名单分支同技巧。
fn validate_map_entry(key: &str, name: &str, value: &toml::Value) -> Result<(), String> {
    let mut base =
        toml::Value::try_from(Config::default()).map_err(|e| format!("默认配置序列化失败: {e}"))?;
    let mut one = toml::map::Map::new();
    one.insert(name.to_string(), value.clone());
    let path: Vec<&str> = key.split('.').collect();
    if let toml::Value::Table(t) = &mut base {
        crate::config::set_nested(t, &path, toml::Value::Table(one));
    }
    base.try_into::<Config>()
        .map(|_| ())
        .map_err(|e| format!("类型或取值不合法: {e}"))
}

/// **降级闸**：把「合并种子不可信」的 Map 条目就地标成条目级 `error`。
///
/// # 为什么必须有这一道
///
/// [`writes`] 拿**当前生效配置**当 Map 键的合并种子。段级降级之后，「生效配置」在坏段处
/// 是出厂值——`input.punct` 一降级，`custom_mappings` 的种子就是出厂空表，于是应用一个
/// 只加一条映射的片段，会把用户**全部**已有的自定义标点整表抹掉，永久且无痕。
///
/// P1 之后这条路可由**第三方**触发：定制者在 `data_custom/config.toml` 里把该键写成错
/// 类型，该定制版的每个用户每次 `load()` 都降级，此后任何一次配置导入都会引爆。
///
/// # 为什么标成 error 而不是在应用时 bail
///
/// 预览与应用共走同一批 `entries`，判据必须只有一套——「预览放行、应用才拒绝」是分发者
/// 最难自查的一类不一致（本模块的 `patch_entries` 文档已经立过这条规矩）。标成 error 之后
/// 预览界面直接显示原因，应用侧既有的「任何一条有错即整体拒绝」自动生效，无需第二处判断。
///
/// # 只管 Map 条目
///
/// 标量键的落盘值是片段里的**显式新值**，与种子无关，降级不影响它的正确性；连它一起拦
/// 会把「导入一份只改了几个开关的配置」在降级期间整个堵死，代价远大于收益。
pub fn mark_degraded_seeds(entries: &mut [PatchEntry], degradation: &ConfigDegradation) {
    for e in entries.iter_mut() {
        if e.map_entry.is_none() || e.error.is_some() || !degradation.taints(&e.key) {
            continue;
        }
        e.error = Some(format!(
            "本次配置加载中 [{}] 所在段解析失败并回落了出厂默认值，\
             当前表不是你的真实配置；此时合并写回会把已有条目整表抹掉，故拒绝应用。\
             请先修好报错的配置键（见日志 WARN）再导入。",
            e.key
        ));
    }
}

/// 把预览条目折算成**实际落盘的键值**（`set_user_value` 的入参）。
///
/// - 标量/整值条目：原样 `(key, next)`。
/// - Map 条目：按父键分组，值 = 当前生效表 ∪ 片段条目（同名覆盖、其余保留），
///   每个父键**只产出一条**（整表写回是 `set_user_value` 唯一能表达的形态）。
///
/// 顺序 = 条目首次出现顺序。调用方须先确认无 `error` 条目（半应用不被允许）。
///
/// ⚠️ **调用前须先跑 [`mark_degraded_seeds`] 并确认无 `error` 条目**：种子取自生效配置，
/// 而段级降级会让生效配置在坏段处变成出厂值，那种表拿来合并就是整表抹掉用户数据。
///
/// ⚠️ **合并种子是「四层合并后的生效表」（L1⊕L2⊕L2.5⊕L3），不是「用户层已有的表」**
/// ——这是当前 4 个 Map 键
/// 出厂值恒为空表（`= {}`）时唯一可行的取法，但它埋着一颗休眠的雷：若将来工厂层（L1/L2）
/// 给这些键提供了非空默认条目，本函数会把那些默认条目连同片段条目一起写进用户层，
/// 从此**冻结、不再跟随工厂层的后续更新**——与
/// [`Config::set_user_value`](crate::config::Config::set_user_value) 文档里
/// `schema.mix.auto_commit_block_on_pinyin` 那颗已引爆的雷同一机理（值等于出厂默认却照写，
/// 用户就被永久钉死在旧默认上），只是那里靠 prune 收口，Map 整表写回没有等价收口。
///
/// 真要给这些键加工厂默认条目时，正确做法是让种子只取**用户层**的表、并对合并结果做
/// 条目级 prune（等于工厂值的条目不写），而**不是**继续拿生效表当种子。
pub fn writes(entries: &[PatchEntry], current: &toml::Value) -> Vec<(String, toml::Value)> {
    let mut out: Vec<(String, toml::Value)> = Vec::new();
    // 父 Map 键 → out 中的下标，保证同一 Map 键的多个条目并进同一张表。
    let mut slot: HashMap<&str, usize> = HashMap::new();
    for e in entries {
        let Some(name) = &e.map_entry else {
            out.push((e.key.clone(), e.next.clone()));
            continue;
        };
        let idx = match slot.get(e.key.as_str()) {
            Some(i) => *i,
            None => {
                // 种子 = 当前生效表：合并的「其余条目保留」全靠这一步。
                let path: Vec<&str> = e.key.split('.').collect();
                let base = crate::config::get_nested(current, &path)
                    .and_then(|v| v.as_table())
                    .cloned()
                    .unwrap_or_default();
                out.push((e.key.clone(), toml::Value::Table(base)));
                slot.insert(e.key.as_str(), out.len() - 1);
                out.len() - 1
            }
        };
        if let toml::Value::Table(t) = &mut out[idx].1 {
            t.insert(name.clone(), e.next.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认配置序列化成的值树，充当测试里的「当前生效配置」。
    fn default_tree() -> toml::Value {
        toml::Value::try_from(Config::default()).expect("serialize default config")
    }

    fn preview_text(text: &str) -> Vec<PatchEntry> {
        let fragment = parse_fragment(text).expect("片段应能解析");
        preview(&fragment, &default_tree())
    }

    #[test]
    fn parse_failure_is_whole_fragment_error() {
        assert!(parse_fragment("= not toml =").is_err());
        assert!(parse_fragment("[unclosed\n").is_err());
    }

    /// ★ 降级闸：Map 条目的合并种子来自生效配置，坏段处那张表是出厂值 ⇒ 必须拒绝应用，
    /// 否则「加一条映射」会把用户已有的整表抹掉（永久、无痕）。标量键则不受牵连。
    #[test]
    fn degraded_map_seed_is_rejected_but_scalars_still_apply() {
        let mut entries = preview_text(
            "[input.punct.custom_mappings]\n\"'1\" = [\"①\"]\n\n[ui.candidate]\nper_page = 9\n",
        );
        assert!(
            entries.iter().all(|e| e.error.is_none()),
            "前置：这份片段本身合法，后面的 error 只能来自降级闸"
        );

        // `input.punct` 整段降级 ⇒ `custom_mappings` 的当前表是出厂空表。
        let deg = ConfigDegradation {
            sections: vec!["input.punct".into()],
            total_fallback: false,
        };
        mark_degraded_seeds(&mut entries, &deg);

        let map_entry = entries
            .iter()
            .find(|e| e.key == "input.punct.custom_mappings")
            .expect("Map 条目应在");
        assert!(
            map_entry.error.is_some(),
            "★ 种子不可信的 Map 条目必须报错——放行它就是把用户已有的映射整表抹掉"
        );

        // ★ 标量键**不受牵连**：它的落盘值是片段里的显式新值，与种子无关。
        // 连它一起拦会让降级期间「导入一份只改几个开关的配置」整个堵死。
        let scalar = entries
            .iter()
            .find(|e| e.key == "ui.candidate.per_page")
            .expect("标量条目应在");
        assert!(scalar.error.is_none(), "标量键不该被降级闸拦下");

        // 对照：降级发生在**别处**时，同一个 Map 条目必须放行（否则闸恒真、上面那条是假绿）。
        let mut entries2 = preview_text("[input.punct.custom_mappings]\n\"'1\" = [\"①\"]\n");
        mark_degraded_seeds(
            &mut entries2,
            &ConfigDegradation {
                sections: vec!["ui.font".into()],
                total_fallback: false,
            },
        );
        assert!(
            entries2[0].error.is_none(),
            "无关段降级不得牵连 Map 条目，实得 {:?}",
            entries2[0].error
        );

        // 整份回落 ⇒ 一切种子不可信。
        let mut entries3 = preview_text("[input.punct.custom_mappings]\n\"'1\" = [\"①\"]\n");
        mark_degraded_seeds(
            &mut entries3,
            &ConfigDegradation {
                sections: Vec::new(),
                total_fallback: true,
            },
        );
        assert!(entries3[0].error.is_some(), "整份回落时必须拦下");
    }

    /// 已有 error 的条目不被降级闸改写：先报的那个错才是作者要看的根因，
    /// 覆盖掉等于把「你这个键写错了」换成「系统降级了」，排查方向被带偏。
    #[test]
    fn existing_error_is_not_overwritten() {
        let mut entries = preview_text("[input.punct.custom_mappings]\n\"'1\" = 42\n");
        let first = entries[0].error.clone();
        assert!(first.is_some(), "前置：值类型不合法本就该报错");
        mark_degraded_seeds(
            &mut entries,
            &ConfigDegradation {
                sections: vec!["input.punct".into()],
                total_fallback: false,
            },
        );
        assert_eq!(entries[0].error, first, "原有 error 必须原样保留");
    }

    /// Map 键（custom_mappings）逐条目产出：`key` 恒为父 Map 键，条目名走 `map_entry`，
    /// 值是条目自身的值（不是整表）。
    #[test]
    fn map_key_flattens_per_entry() {
        let entries =
            preview_text("[input.punct.custom_mappings]\n\"'1\" = [\"①\"]\n\"'2\" = [\"②\"]\n");
        assert_eq!(entries.len(), 2, "两个条目应产出两条");
        for e in &entries {
            assert_eq!(e.key, "input.punct.custom_mappings", "key 恒为父 Map 键");
            assert!(e.error.is_none(), "{:?}", e.error);
        }
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e.map_entry.as_deref().expect("Map 条目须带 map_entry"))
            .collect();
        assert_eq!(names, vec!["'1", "'2"]);
        assert_eq!(
            entries[0].next,
            toml::Value::Array(vec![toml::Value::String("①".into())]),
            "next 是条目自身的值,不是整表"
        );
    }

    /// 条目名可以含 `.`（`custom_mappings` 的源字符就是标点）。条目名并进点分键
    /// 就再也拆不回来,故必须由 `map_entry` 独立承载。
    #[test]
    fn map_entry_name_with_dot_survives() {
        let entries = preview_text("[input.punct.custom_mappings]\n\".\" = [\"。\"]\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "input.punct.custom_mappings");
        assert_eq!(entries[0].map_entry.as_deref(), Some("."));
        assert!(entries[0].error.is_none(), "{:?}", entries[0].error);
    }

    /// 条目值类型错 → 错误归到该条目（不是整表报错,也不牵连同表的合法条目）。
    #[test]
    fn map_entry_type_error_is_attributed_to_that_entry() {
        let entries = preview_text("[keys.key_actions]\nbacktick = \"english\"\nf4 = 5\n");
        assert_eq!(entries.len(), 2);
        let ok = entries
            .iter()
            .find(|e| e.map_entry.as_deref() == Some("backtick"))
            .unwrap();
        assert!(ok.error.is_none(), "合法条目不应被牵连: {:?}", ok.error);
        let bad = entries
            .iter()
            .find(|e| e.map_entry.as_deref() == Some("f4"))
            .unwrap();
        let err = bad.error.as_deref().expect("整数条目值应被拒绝");
        assert!(err.contains("类型或取值不合法"), "{err}");
    }

    /// 空条目名报错而不是当正常条目:没有任何按键名/方案 id/源字符会是空串,
    /// 放行只会写进一条永远匹配不上、也删不掉的死条目。
    #[test]
    fn empty_map_entry_name_is_rejected() {
        let entries = preview_text("[keys.key_actions]\n\"\" = \"english\"\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].map_entry.as_deref(), Some(""));
        assert_eq!(entries[0].error.as_deref(), Some("条目名不能为空"));
    }

    /// Map 键下的空表 = no-op（不产出条目）。「空表 = 清空」是脚枪,片段没有删条目的语义。
    #[test]
    fn empty_map_table_is_noop() {
        assert!(preview_text("[keys.key_actions]\n").is_empty());
        assert!(preview_text("keys.key_actions = {}\n").is_empty());
    }

    /// Map 键给了非表值 → 落回单条,按 Map 类型报「类型应为 table」。
    #[test]
    fn map_key_with_non_table_value_reports_type_error() {
        let entries = preview_text("keys.key_actions = 5\n");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].map_entry.is_none());
        let err = entries[0].error.as_deref().expect("整数应被拒绝");
        assert!(err.contains("类型应为 table"), "{err}");
    }

    /// Map 条目的 current 取自当前表内的同名条目：缺席 = None（新增）,在场 = 原值。
    #[test]
    fn map_entry_current_comes_from_existing_table() {
        let mut tree = default_tree();
        if let toml::Value::Table(t) = &mut tree {
            let mut m = toml::map::Map::new();
            m.insert("backtick".into(), toml::Value::String("english".into()));
            crate::config::set_nested(t, &["keys", "key_actions"], toml::Value::Table(m));
        }
        let fragment =
            parse_fragment("[keys.key_actions]\nbacktick = \"半角\"\nf4 = \"english\"\n").unwrap();
        let entries = preview(&fragment, &tree);
        let old = entries
            .iter()
            .find(|e| e.map_entry.as_deref() == Some("backtick"))
            .unwrap();
        assert_eq!(
            old.current.as_ref().and_then(|v| v.as_str()),
            Some("english"),
            "已有条目应报当前值"
        );
        let new = entries
            .iter()
            .find(|e| e.map_entry.as_deref() == Some("f4"))
            .unwrap();
        assert!(new.current.is_none(), "缺席条目 = 新增,无当前值");
    }

    // ── writes()：落盘键值折算 ──

    /// Map 合并：当前表既有条目保留,同名条目被片段覆盖,父键只产出一条。
    #[test]
    fn writes_merges_map_keeping_existing_entries() {
        let mut tree = default_tree();
        if let toml::Value::Table(t) = &mut tree {
            let mut m = toml::map::Map::new();
            m.insert("backtick".into(), toml::Value::String("english".into()));
            m.insert("f2".into(), toml::Value::String("半角".into()));
            crate::config::set_nested(t, &["keys", "key_actions"], toml::Value::Table(m));
        }
        let fragment =
            parse_fragment("[keys.key_actions]\nbacktick = \"全角\"\nf4 = \"english\"\n").unwrap();
        let entries = preview(&fragment, &tree);
        let w = writes(&entries, &tree);
        assert_eq!(w.len(), 1, "同一 Map 父键只落一条,实际: {w:?}");
        assert_eq!(w[0].0, "keys.key_actions");
        let t = w[0].1.as_table().expect("Map 落盘值须是整表");
        assert_eq!(
            t.get("f2").and_then(|v| v.as_str()),
            Some("半角"),
            "未提及的条目保留"
        );
        assert_eq!(
            t.get("backtick").and_then(|v| v.as_str()),
            Some("全角"),
            "同名条目覆盖"
        );
        assert_eq!(
            t.get("f4").and_then(|v| v.as_str()),
            Some("english"),
            "新条目并入"
        );
        assert_eq!(t.len(), 3);
    }

    /// 当前树里该 Map 键缺席（或是空表）时,合并结果 = 片段条目本身。
    #[test]
    fn writes_seeds_empty_map_from_fragment_only() {
        let tree = default_tree();
        let fragment = parse_fragment("[keys.key_actions]\nf4 = \"english\"\n").unwrap();
        let entries = preview(&fragment, &tree);
        let w = writes(&entries, &tree);
        assert_eq!(w.len(), 1);
        let t = w[0].1.as_table().unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t.get("f4").and_then(|v| v.as_str()), Some("english"));
    }

    /// 标量与多个 Map 键混合：标量原样一条,每个 Map 父键各合并成一条,顺序 = 首次出现顺序。
    #[test]
    fn writes_groups_scalars_and_multiple_maps() {
        let tree = default_tree();
        let fragment = parse_fragment(
            "ui.candidate.per_page = 9\n\
             [keys.key_actions]\nf4 = \"english\"\nf5 = \"半角\"\n\
             [keys.session_actions]\nf6 = \"english\"\n",
        )
        .unwrap();
        let entries = preview(&fragment, &tree);
        assert_eq!(entries.len(), 4, "条目仍逐条,实际: {entries:?}");
        let w = writes(&entries, &tree);
        let keys: Vec<&str> = w.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "keys.key_actions",
                "keys.session_actions",
                "ui.candidate.per_page"
            ],
            "每个 Map 父键一条 + 标量一条（顺序 = TOML 表遍历序）"
        );
        let ka = w
            .iter()
            .find(|(k, _)| k == "keys.key_actions")
            .unwrap()
            .1
            .as_table()
            .unwrap();
        assert_eq!(ka.len(), 2, "同一 Map 的两个条目并进同一张表");
        let per_page = w
            .iter()
            .find(|(k, _)| k == "ui.candidate.per_page")
            .unwrap();
        assert_eq!(per_page.1.as_integer(), Some(9), "标量原样落盘");
    }

    /// StructList 键（mix_modes）同理：数组整体是一个值，元素不展开。
    #[test]
    fn struct_list_stops_flatten() {
        let entries = preview_text("[[schema.mix_modes]]\nid = \"m1\"\nname = \"测试\"\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "schema.mix_modes");
        assert!(entries[0].error.is_none(), "{:?}", entries[0].error);
    }

    #[test]
    fn unknown_key_reported_per_entry() {
        let entries = preview_text("[input.foo]\nbar = 1\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "input.foo.bar");
        assert_eq!(entries[0].error.as_deref(), Some("未知配置键"));
        assert!(entries[0].current.is_none(), "未知键无当前值");
    }

    #[test]
    fn allowlist_key_passes_and_has_no_default_current() {
        let entries =
            preview_text("[input.temp_english]\ncomment_template_vertical = \"${dict}\"\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].key,
            "input.temp_english.comment_template_vertical"
        );
        assert!(entries[0].error.is_none(), "{:?}", entries[0].error);
        // 出厂值恰是「键不存在」，默认树里取不到当前值。
        assert!(entries[0].current.is_none());
    }

    #[test]
    fn allowlist_key_rejects_wrong_type() {
        let entries = preview_text("[input.temp_pinyin]\ncomment_template_vertical = 5\n");
        assert_eq!(entries.len(), 1);
        let err = entries[0].error.as_deref().expect("整数值应被拒绝");
        assert!(err.contains("类型或取值不合法"), "{err}");
    }

    #[test]
    fn registered_key_rejects_type_mismatch_and_enum_out_of_range() {
        let entries = preview_text("[ui.candidate]\nper_page = \"seven\"\nlayout = \"diagonal\"\n");
        assert_eq!(entries.len(), 2);
        let per_page = entries
            .iter()
            .find(|e| e.key == "ui.candidate.per_page")
            .unwrap();
        assert!(per_page.error.as_deref().unwrap().contains("类型应为"));
        let layout = entries
            .iter()
            .find(|e| e.key == "ui.candidate.layout")
            .unwrap();
        assert!(layout.error.as_deref().unwrap().contains("不在允许集合"));
    }

    /// diff 正确性：current 取自传入的当前值树，next 取自片段。
    #[test]
    fn diff_reports_current_and_next() {
        let entries = preview_text("[ui.candidate]\nper_page = 9\n");
        assert_eq!(entries.len(), 1);
        let default_per_page =
            crate::config::get_nested(&default_tree(), &["ui", "candidate", "per_page"]).cloned();
        assert_eq!(entries[0].current, default_per_page);
        assert_eq!(entries[0].next.as_integer(), Some(9));
        assert!(entries[0].error.is_none());
    }

    /// 点分键与嵌套表两种写法产出完全相同的条目集（TOML 解析层已归一）。
    #[test]
    fn dotted_and_nested_forms_are_equivalent() {
        let dotted = preview_text("ui.candidate.per_page = 9\ninput.auto_pair.chinese = false\n");
        let nested =
            preview_text("[ui.candidate]\nper_page = 9\n[input.auto_pair]\nchinese = false\n");
        let mut a: Vec<(String, toml::Value)> =
            dotted.into_iter().map(|e| (e.key, e.next)).collect();
        let mut b: Vec<(String, toml::Value)> =
            nested.into_iter().map(|e| (e.key, e.next)).collect();
        a.sort_by(|x, y| x.0.cmp(&y.0));
        b.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(a, b);
    }

    /// 未知路径上的空表不产出条目（无叶子可报），空片段同理。
    #[test]
    fn empty_fragment_and_empty_unknown_table_yield_no_entries() {
        assert!(preview_text("").is_empty());
        assert!(preview_text("[input.foo]\n").is_empty());
    }

    // ── `[package]` 保留段与说明元信息 ──

    fn info_of(text: &str) -> Option<PatchInfo> {
        let fragment = parse_fragment(text).expect("片段应能解析");
        extract_info(&fragment).expect("说明元信息应合法")
    }

    fn info_err(text: &str) -> String {
        let fragment = parse_fragment(text).expect("片段应能解析");
        extract_info(&fragment).expect_err("说明元信息应被拒绝")
    }

    /// 保留段整段跳过：不产出配置条目,也不报「未知配置键」。同片段里的真配置键照常展平。
    #[test]
    fn reserved_section_is_skipped_by_flatten() {
        let entries = preview_text(
            "[package]\ntitle = \"演示\"\ndescription = \"说明\"\n\
             [ui.candidate]\nper_page = 9\n",
        );
        assert_eq!(entries.len(), 1, "只应剩真配置键,实际: {entries:?}");
        assert_eq!(entries[0].key, "ui.candidate.per_page");
        assert!(entries[0].error.is_none(), "{:?}", entries[0].error);
    }

    /// 只有 `[package]` 段的片段 → entries 为空,「片段为空」的既有语义不变
    /// （applyPatch 视作成功 no-op）。
    #[test]
    fn fragment_with_only_reserved_section_has_no_entries() {
        assert!(preview_text("[package]\ntitle = \"只有说明\"\n").is_empty());
        // 段内子键叫得跟配置域一模一样也照样跳过（`package.ui` 不是 `ui`）。
        assert!(preview_text("[package.ui]\ncandidate = 9\n").is_empty());
    }

    /// title / description 正常提取,description 可多行。
    #[test]
    fn extract_info_reads_title_and_multiline_description() {
        let info = info_of(
            "[package]\ntitle = \"快符方案\"\ndescription = \"\"\"\n第一行\n第二行\n\"\"\"\n",
        )
        .expect("两字段都写了应返回 Some");
        assert_eq!(info.title.as_deref(), Some("快符方案"));
        assert_eq!(
            info.description.as_deref(),
            Some("第一行\n第二行"),
            "多行说明保留内部换行,首尾空白 trim"
        );
    }

    /// 段不存在 / 两字段都缺省 → None（RPC 响应里就没有 info 字段）。
    #[test]
    fn extract_info_absent_yields_none() {
        assert!(info_of("ui.candidate.per_page = 9\n").is_none());
        assert!(info_of("[package]\n").is_none());
        assert!(
            info_of("[package]\nkind = \"schema_text\"\n").is_none(),
            "只有未知子键 = 没写说明"
        );
    }

    /// 段内未知子键一律忽略（向前兼容）：`kind` / `format_version` 是信封与包的字段,
    /// 出现在同一段里不该报错,也不该影响 title/description 的提取。
    #[test]
    fn extract_info_ignores_unknown_subkeys() {
        let info = info_of(
            "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
             future_field = { nested = true }\ntitle = \"带未知子键\"\n",
        )
        .expect("有 title 就该返回 Some");
        assert_eq!(info.title.as_deref(), Some("带未知子键"));
        assert!(info.description.is_none());
    }

    /// title 必须是单行:内部含换行即拒。归一后的 `\r\n` 同样算换行——归一是为了统一判据,
    /// 不是为了放行。
    #[test]
    fn title_with_inner_newline_is_rejected() {
        for text in [
            "[package]\ntitle = \"第一行\\n第二行\"\n",
            "[package]\ntitle = \"第一行\\r\\n第二行\"\n",
        ] {
            let err = info_err(text);
            assert!(err.contains("package.title"), "错误须点名字段: {err}");
            assert!(err.contains("换行"), "{err}");
        }
    }

    /// 首尾换行**放行**:TOML 多行字符串是写 title 的合法语法,尾随换行是该语法的固有产物,
    /// 不是分发者的错误。「不含换行」挡的是「title 得是单行」,不是「字节里不许出现 \n」。
    #[test]
    fn title_with_surrounding_newline_is_accepted() {
        let info = info_of("[package]\ntitle = \"\"\"\n快符方案\n\"\"\"\n")
            .expect("TOML 多行字符串写的单行标题应放行");
        assert_eq!(info.title.as_deref(), Some("快符方案"));
        let info = info_of("[package]\ntitle = \"尾随换行\\r\\n\"\n").unwrap();
        assert_eq!(info.title.as_deref(), Some("尾随换行"));
    }

    /// 限额按**字符**不是字节：200 个 CJK 字符（600 字节）的 title 合法,201 个即拒。
    /// description 同理 4000/4001。
    #[test]
    fn length_limit_counts_chars_not_bytes() {
        let ok_title = "字".repeat(INFO_TITLE_MAX_CHARS);
        assert_eq!(ok_title.len(), INFO_TITLE_MAX_CHARS * 3, "CJK 每字 3 字节");
        let info = info_of(&format!("[package]\ntitle = \"{ok_title}\"\n")).unwrap();
        assert_eq!(info.title.as_deref(), Some(ok_title.as_str()));

        let long_title = "字".repeat(INFO_TITLE_MAX_CHARS + 1);
        let err = info_err(&format!("[package]\ntitle = \"{long_title}\"\n"));
        assert!(err.contains("package.title"), "{err}");
        assert!(
            err.contains(&(INFO_TITLE_MAX_CHARS + 1).to_string())
                && err.contains(&INFO_TITLE_MAX_CHARS.to_string()),
            "错误须写明实际长度与上限: {err}"
        );

        let ok_desc = "说".repeat(INFO_DESCRIPTION_MAX_CHARS);
        assert!(info_of(&format!("[package]\ndescription = \"{ok_desc}\"\n")).is_some());
        let long_desc = "说".repeat(INFO_DESCRIPTION_MAX_CHARS + 1);
        let err = info_err(&format!("[package]\ndescription = \"{long_desc}\"\n"));
        assert!(err.contains("package.description"), "{err}");
    }

    /// C0 控制字符与 DEL 拒绝;`\t` 放行;`\r\n` 与孤立 `\r` 归一为 `\n`。
    #[test]
    fn control_chars_rejected_tab_allowed_crlf_normalized() {
        for bad in ["\\u0000", "\\u0007", "\\u001B", "\\u007F"] {
            let err = info_err(&format!("[package]\ndescription = \"说明{bad}\"\n"));
            assert!(err.contains("package.description"), "{err}");
            assert!(err.contains("控制字符"), "{err}");
        }
        let info = info_of("[package]\ntitle = \"制表\\t符\"\n").expect("\\t 应放行");
        assert_eq!(info.title.as_deref(), Some("制表\t符"));

        let info = info_of("[package]\ndescription = \"甲\\r\\n乙\\r丙\"\n").unwrap();
        assert_eq!(
            info.description.as_deref(),
            Some("甲\n乙\n丙"),
            "\\r\\n 与孤立 \\r 都归一为 \\n"
        );
    }

    /// 类型不对 → 报错,不静默忽略（忽略就成了「写了没生效」）。段本身不是表同理。
    #[test]
    fn wrong_type_is_rejected() {
        let err = info_err("[package]\ntitle = 5\n");
        assert!(err.contains("package.title"), "{err}");
        assert!(err.contains("字符串"), "{err}");
        let err = info_err("[package]\ndescription = [\"a\"]\n");
        assert!(err.contains("package.description"), "{err}");
        let err = info_err("package = 5\n");
        assert!(err.contains("package"), "{err}");
        assert!(
            preview_text("package = 5\n").is_empty(),
            "写坏的保留段也不该冒充配置键"
        );
    }

    /// 全空白视同没写（trim 后为空 → None）,不是「一个空标题」。
    /// 全角空格 U+3000 同样算空白（Rust `trim()` 认它）——中文分发者最容易误打的正是它。
    #[test]
    fn all_whitespace_is_treated_as_absent() {
        assert!(info_of("[package]\ntitle = \"   \"\n").is_none());
        assert!(
            info_of("[package]\ntitle = \"\u{3000}\u{3000}\"\n").is_none(),
            "全角空格也是空白,视同没写"
        );
        assert_eq!(
            info_of("[package]\ntitle = \"\u{3000}有标题\u{3000}\"\n")
                .unwrap()
                .title
                .as_deref(),
            Some("有标题"),
            "全角空格同样被 trim 掉"
        );
        assert!(info_of("[package]\ndescription = \"\\n\\n  \"\n").is_none());
        let info = info_of("[package]\ntitle = \"  有标题  \"\ndescription = \"  \"\n").unwrap();
        assert_eq!(info.title.as_deref(), Some("有标题"), "首尾空白 trim");
        assert!(info.description.is_none(), "全空白的说明视同没写");
    }

    /// [`sanitize_info_text`] 是三种载体共用的入口,直接调用时的语义与经 extract_info
    /// 走一遍一致（wind-transfer 复用的正是这条路）。
    #[test]
    fn sanitize_info_text_is_reusable_directly() {
        assert_eq!(
            sanitize_title("  标题\r\n\r\n").unwrap(),
            "标题",
            "首尾换行 trim 掉即可,只有内部换行才算多行"
        );
        assert!(sanitize_title("甲\r\n乙").unwrap_err().contains("换行"));
        assert_eq!(sanitize_description("甲\r\n乙").unwrap(), "甲\n乙");
        assert_eq!(sanitize_description("   ").unwrap(), "", "空串 = 没写");
        let err = sanitize_info_text("自定字段", "a\u{1}b", 10, true).unwrap_err();
        assert!(
            err.starts_with("自定字段"),
            "错误须点名调用方给的字段名: {err}"
        );
    }

    // ── ALLOWED_UNREGISTERED_KEYS 守门：名单不许腐烂 ──

    /// (a) 名单键必须不在 REGISTRY——日后登记了就该从名单删除，否则同一个键有两条校验路径。
    #[test]
    fn allowlist_keys_stay_out_of_registry() {
        for key in ALLOWED_UNREGISTERED_KEYS {
            assert!(
                !config_schema::is_known_key(key),
                "{key} 已进 REGISTRY，应从 ALLOWED_UNREGISTERED_KEYS 移除"
            );
        }
    }

    /// (b) 名单键必须真实存在于 `Config` 结构：写入样例值后能反序列化，且序列化回来
    /// 原值可读——`Config` 不 deny 未知字段，光「不报错」证明不了字段存在，
    /// 回读同值才能证明。样例值按当前名单全员 `Option<String>` 取字符串；
    /// 若日后加入其他类型的键，需为其单独给样例。
    #[test]
    fn allowlist_keys_deserialize_and_round_trip() {
        for key in ALLOWED_UNREGISTERED_KEYS {
            let mut base = toml::Value::try_from(Config::default()).unwrap();
            let path: Vec<&str> = key.split('.').collect();
            if let toml::Value::Table(t) = &mut base {
                crate::config::set_nested(t, &path, toml::Value::String("样例".into()));
            }
            let cfg: Config = base
                .try_into()
                .unwrap_or_else(|e| panic!("{key} 应能反序列化: {e}"));
            let back = toml::Value::try_from(cfg).unwrap();
            assert_eq!(
                crate::config::get_nested(&back, &path).and_then(|v| v.as_str()),
                Some("样例"),
                "{key} 写入后未能原值读回——名单里可能是不存在的键"
            );
        }
    }

    // ── 风险提示（RISKY_KEYS）────────────────────────────────────

    /// 危险键在预览里必须带提示，且**照常可应用**（提示不是阻断）。
    #[test]
    fn risky_key_is_flagged_but_still_applicable() {
        let frag: toml::Value = toml::from_str(
            r#"
            [[ui.toolbar.buttons]]
            id = "x"
            label = "符"
            action = 'proc.run("evil.exe")'
            "#,
        )
        .unwrap();
        let cur = toml::Value::try_from(Config::default()).unwrap();
        let entries = preview(&frag, &cur);
        let e = entries
            .iter()
            .find(|e| e.key == "ui.toolbar.buttons")
            .expect("应产出该键的条目");
        assert!(e.error.is_none(), "合法片段不该报错：{:?}", e.error);
        let w = e.warning.as_deref().expect("危险键必须带提示");
        assert!(w.contains("启动程序"), "提示要说清后果，实际：{w}");
    }

    /// 寻常键不带提示——否则提示遍地都是，等于没有提示。
    #[test]
    fn ordinary_key_has_no_warning() {
        let frag: toml::Value = toml::from_str("[ui.candidate]\nper_page = 9\n").unwrap();
        let cur = toml::Value::try_from(Config::default()).unwrap();
        let entries = preview(&frag, &cur);
        assert!(entries.iter().all(|e| e.warning.is_none()));
    }

    /// 写错了的危险键**照样提示**：用户多半会改对再导入一次，那时提示就该已经说过。
    #[test]
    fn risky_key_warns_even_when_invalid() {
        // StructList 键给了标量 → 类型不合法。
        let frag: toml::Value = toml::from_str(r#"ui.toolbar.buttons = 5"#).unwrap();
        let cur = toml::Value::try_from(Config::default()).unwrap();
        let entries = preview(&frag, &cur);
        let e = entries
            .iter()
            .find(|e| e.key == "ui.toolbar.buttons")
            .expect("应产出该键的条目");
        assert!(e.error.is_some(), "标量给 StructList 键应报类型错");
        assert!(e.warning.is_some(), "写错了也要提示");
    }

    /// 名单里的每个键都必须是**真的登记键**——否则那条提示永远不会触发，
    /// 而「提示没出现」没有任何信号。同 ALLOWED_UNREGISTERED_KEYS 的守门思路。
    #[test]
    fn risky_keys_are_all_registered() {
        for (key, msg) in RISKY_KEYS {
            assert!(
                config_schema::is_known_key(key),
                "{key} 不在 REGISTRY 里，这条风险提示永远不会触发"
            );
            assert!(!msg.trim().is_empty(), "{key} 的提示语不能为空");
        }
    }
}
