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
/// 目前为空。`StructList` 子树（如 `schema.mix_modes`）内的字段不必进名单：展平时随整棵
/// 子树作一个值，不会以点分键形态出现。宁缺勿滥——拿不准的键不进名单，
/// 误报「未知」可改，误放行则绕过了 REGISTRY 这道门。
///
/// 守门测试（本文件 tests）保证名单不腐烂：每个键必须 (a) 不在 REGISTRY
/// （日后登记了就该从名单删除）、(b) 真实存在于 `Config` 结构（写入后能反序列化并原值读回）。
pub const ALLOWED_UNREGISTERED_KEYS: &[&str] = &[];

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
    text.parse::<toml::Value>()
        .map_err(|e| format!("TOML 解析失败: {e}"))
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
#[path = "patch_tests.rs"]
mod tests;
