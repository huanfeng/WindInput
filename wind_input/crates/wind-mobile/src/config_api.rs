//! 移动端的配置读写门面。
//!
//! 宿主不自己读写 config.toml：[`Config::set_user_value`] 负责容错解析、损坏即备份
//! （备份失败就拒绝写）、等于默认值的键不落盘，宿主重写一份必然漏掉其中几条。
//! 这一层只做转发与类型投影，不写落盘逻辑。
//!
//! 快照用 TOML：与配置文件同一种表示，快照里的键就是文件里的键，不需要第二套映射。
//! 字段注册表一并暴露，宿主加载设置清单时可当场比对出写错的键（否则是「开关点了没反应」且不报错）。

use std::path::Path;
use std::sync::{Mutex, RwLock};

use wind_config::Config;
use wind_config::config_schema::{self, FieldType};

/// 一个配置值（跨语言边界的最小值类型集合）。
///
/// 刻意不含 Map / 结构体数组：那些在设置界面上要专门的编辑器，不是「一个值」，
/// 目前经快照读出来展示、由桌面端编辑。将来移动端要编辑它们时，应当为那一类
/// 单独设计入口（如标点映射表有自己的行编辑界面），而不是往这个枚举里塞一个
/// 万能的字符串。
#[derive(Debug, Clone)]
pub enum ConfigValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    StrList(Vec<String>),
}

impl ConfigValue {
    fn into_toml(self) -> toml::Value {
        match self {
            Self::Bool(v) => toml::Value::Boolean(v),
            Self::Int(v) => toml::Value::Integer(v),
            Self::Float(v) => toml::Value::Float(v),
            Self::Str(v) => toml::Value::String(v),
            Self::StrList(v) => {
                toml::Value::Array(v.into_iter().map(toml::Value::String).collect())
            }
        }
    }
}

/// 注册表里一个字段的类型描述（宿主据此校验清单、渲染控件、约束取值）。
#[derive(Debug, Clone)]
pub struct ConfigFieldInfo {
    /// 点分路径，如 `"ui.candidate.per_page"`
    pub key: String,
    /// `bool` / `int` / `float` / `str` / `enum` / `str_list` / `map` / `struct_list`
    pub ty: String,
    /// `enum` 的合法值集合；其余类型为空。
    ///
    /// 值域由注册表给出，不让宿主手抄：手抄的会与 core 漂移，表现为新选项看不到或「选了没反应」。
    pub options: Vec<String>,
}

fn type_name(ty: &FieldType) -> &'static str {
    match ty {
        FieldType::Bool => "bool",
        FieldType::Int => "int",
        FieldType::Float => "float",
        FieldType::Str => "str",
        FieldType::Enum(_) | FieldType::LayoutEnum(_) => "enum",
        FieldType::StrList => "str_list",
        FieldType::Map(_) => "map",
        FieldType::StructList => "struct_list",
    }
}

/// core 的配置字段注册表。
pub fn registry() -> Vec<ConfigFieldInfo> {
    config_schema::registry()
        .iter()
        .map(|f| ConfigFieldInfo {
            key: f.key.to_string(),
            ty: type_name(&f.ty).to_string(),
            // LayoutEnum 也报成 enum（见 type_name），值域同样要交出去，否则设置页画不出选择器。
            options: match f.ty {
                FieldType::Enum(vals) | FieldType::LayoutEnum(vals) => {
                    vals.iter().map(|s| s.to_string()).collect()
                }
                _ => Vec::new(),
            },
        })
        .collect()
}

/// 当前生效配置的全量快照（TOML 文本）。
///
/// 生效 = 四层合并再套上移动端强制项（[`apply_mobile_overrides`]），不是用户层那一份。
/// 设置页要按它显示，开关的显示态才与引擎实际行为一致。
pub fn snapshot(data_dir: &Path) -> String {
    let mut cfg = Config::load(Some(data_dir)).unwrap_or_else(|e| {
        tracing::warn!("配置快照加载失败，回落代码默认值: {e}");
        Config::default()
    });
    apply_mobile_overrides(&mut cfg);
    to_toml(&cfg)
}

/// 移动端对加载结果的强制改写，`MobileCore::new` 与 [`snapshot`] 共用。
///
/// 两处必须一致，否则设置页显示的是引擎没在用的值。这里改写的键不是用户可选项，
/// 不该出现在移动端设置清单里。
pub fn apply_mobile_overrides(cfg: &mut Config) {
    // 构造与每次重载都要重算：只在构造时算的话，重载读回出厂 `available` 会丢掉已装的方案。
    apply_enabled_schemas(cfg);
    // 移动端没有光标旁的候选窗，编码显示在键盘上方的编码栏，必须让协调器下发 preedit
    // （默认的 `app_inline` 不下发）。
    cfg.ui.candidate.preedit_display = "candidate_top".to_string();
    // 联想态空格是否选联想不在这里强制，归 `[mobile.association] space_commits`。
}

/// 出厂默认配置的全量快照（TOML 文本）。
///
/// 给的是 L1⊕L2⊕L2.5（代码默认 ⊕ `data/config.toml` ⊕ 定制层），不是 `Config::default()`：
/// [`Config::set_user_value`] 的「等于默认不落盘」比对的正是这一层（`preset_for_pruning`）。
/// 拿 L1 当默认，在出厂文件改写过的键上「默认：X」会标错，「恢复默认」写回的值也剪不掉。
///
/// 取不到出厂配置时降级到 L1 并告警，这说明 `WIND_INSTALL_ROOT` 没钉对（见 `crate::mount_dirs`），
/// 核心侧的剪枝此时同样失效。
pub fn defaults(data_dir: &Path) -> String {
    let preset = match Config::system_preset_value(Some(data_dir)) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("出厂默认取不到（{e}），回落代码默认值；「恢复默认」将不会剪枝");
            return to_toml(&apply_overrides_to(Config::default()));
        }
    };
    let cfg: Config = match preset.try_into() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("出厂默认反序列化失败（{e}），回落代码默认值");
            Config::default()
        }
    };
    to_toml(&apply_overrides_to(cfg))
}

fn apply_overrides_to(mut cfg: Config) -> Config {
    apply_mobile_overrides(&mut cfg);
    cfg
}

fn to_toml(cfg: &Config) -> String {
    // 序列化失败返回空串而不是 panic：这条路在设置页打开时同步调用，
    // panic 会把整个输入法带走，而空快照只是设置页显示不出当前值。
    toml::to_string(cfg).unwrap_or_default()
}

// ── 启用的方案 ──────────────────────────────────────────────────────
//
// 移动端语义：装了的方案默认全部可用；用户在设置里关掉某些，就写用户层 `schema.available`
// （装了的里头他要的那些）。英文方案恒在其中——它是中英键切换的目标，关掉中英键就失灵；
// 也因此用户写下的列表恒含 `english`，不会与出厂列表相等而被「等于默认不落盘」剪掉。

/// 已装方案（构造时扫 assets 得到，见 [`set_installed_schemas`]；导入 / 删除方案后重扫刷新）。
/// 覆盖钩子是函数指针、捕获不了变量，故放进程级。
static INSTALLED: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// 英文方案 id：中英键切换的目标，恒启用。
pub const ENGLISH_SCHEMA: &str = "english";

/// 构造时的首装兜底方案 id（磁盘上没有它也算已装）。重扫已装方案时要按同一规则补回，
/// 否则导入 / 删除之后列表会和构造时不一致。
static FALLBACK_SCHEMA: RwLock<String> = RwLock::new(String::new());

/// 记下首装兜底方案（构造时调一次）。
pub fn set_fallback_schema(id: &str) {
    *FALLBACK_SCHEMA.write().unwrap_or_else(|e| e.into_inner()) = id.to_string();
}

/// 首装兜底方案；构造前为空。
pub fn fallback_schema() -> String {
    FALLBACK_SCHEMA
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 记下已装方案（构造时调；方案包导入 / 删除后再调一次覆盖）。
pub fn set_installed_schemas(ids: Vec<String>) {
    *INSTALLED.write().unwrap_or_else(|e| e.into_inner()) = ids;
}

/// 已装方案；构造前为空。
pub fn installed_schemas() -> Vec<String> {
    INSTALLED.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 用户层里显式写下的 `schema.available`（没写过 = `None`）。
///
/// 直接读用户层文件而不是看合并后的配置：合并后分不清「用户选的」与「出厂那两个」，
/// 而这两者语义正相反（前者照办，后者在移动端应当忽略、按装了的全部来）。
fn user_chosen_schemas() -> Option<Vec<String>> {
    let path = Config::user_config_dir()?.join("config.toml");
    let text = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    let arr = value.get("schema")?.get("available")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
    )
}

/// 当前启用的方案：装了的 ∩ 用户选的（没选过 = 全部），英文恒在。
pub fn enabled_schemas() -> Vec<String> {
    enabled_from(&installed_schemas(), user_chosen_schemas().as_deref())
}

/// 把可选方案设成 [`enabled_schemas`]；当前方案不在其中时换成第一个仍启用的中文方案。
/// 没有已装信息（构造前、测试壳）时不动。
pub fn apply_enabled_schemas(cfg: &mut Config) {
    if installed_schemas().is_empty() {
        return;
    }
    let list = enabled_schemas();
    if !list.contains(&cfg.schema.active)
        && let Some(first) = list.iter().find(|s| *s != ENGLISH_SCHEMA)
    {
        cfg.schema.active = first.clone();
    }
    cfg.schema.available = list;
}

/// 纯函数：装了的方案里，用户选了的（`None` = 全部），再补上英文；至少留一个中文方案，
/// 用户一个中文方案都没留（手改配置）时退回全部。
pub fn enabled_from(installed: &[String], chosen: Option<&[String]>) -> Vec<String> {
    let mut list: Vec<String> = match chosen {
        Some(c) => installed
            .iter()
            .filter(|s| c.contains(s))
            .cloned()
            .collect(),
        None => installed.to_vec(),
    };
    if installed.iter().any(|s| s == ENGLISH_SCHEMA) && !list.iter().any(|s| s == ENGLISH_SCHEMA) {
        list.push(ENGLISH_SCHEMA.to_string());
    }
    if !list.iter().any(|s| s != ENGLISH_SCHEMA) {
        return installed.to_vec();
    }
    list
}

/// 启用 / 停用一个已装方案（写用户层 `schema.available`）。英文与最后一个中文方案关不掉。
/// 同 [`set`]，只写盘，生效要另发重载（重载时 [`apply_enabled_schemas`] 按新列表重算）。
pub fn set_schema_enabled(id: &str, on: bool) -> Result<(), String> {
    let installed = installed_schemas();
    if !installed.iter().any(|s| s == id) {
        return Err(format!("没有装这个方案：{id}"));
    }
    if id == ENGLISH_SCHEMA && !on {
        return Err("英文方案是中英键切换的目标，不能关闭".to_string());
    }
    let _w = write_guard();
    let mut list = enabled_from(&installed, user_chosen_schemas().as_deref());
    if on {
        if !list.iter().any(|s| s == id) {
            list.push(id.to_string());
        }
    } else {
        list.retain(|s| s != id);
        if !list.iter().any(|s| s != ENGLISH_SCHEMA) {
            return Err("至少要留一个中文方案".to_string());
        }
    }
    // 按已装顺序落盘，列表稳定、diff 好读
    let ordered: Vec<toml::Value> = installed
        .iter()
        .filter(|s| list.contains(s))
        .map(|s| toml::Value::String(s.clone()))
        .collect();
    Config::set_user_value(&["schema", "available"], toml::Value::Array(ordered))
        .map_err(|e| e.to_string())
}

/// 用户层写入的进程内互斥，本模块的写口一律经这把锁。
///
/// `Config::set_user_value` 是不加锁的整份读改写；宿主把各卡片的写入各自丢到 IO 线程，
/// 并发时后写的会拿旧文件覆盖先写的（「刚打开的开关又弹回去」）。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn write_guard() -> std::sync::MutexGuard<'static, ()> {
    // 持锁方 panic 过也照常往下写：锁里没有要保护的内存状态，只是串行化文件读改写
    WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 一个 emoji 附加模块与它当前的开关（设置页「附加模块」一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiModuleState {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 用户自己放进 `emoji/` 目录的表（`false` = 内置模块）
    pub user: bool,
    pub enabled: bool,
}

/// 全部 emoji 附加模块（内置在前，其后是用户放进 `emoji/` 的表）及当前开关。
///
/// 单独开口而不走 [`set`]：`input.emoji.modules` 是按模块 id 为键的表，注册表里只登记了
/// 表本身（条数不定，用户能自己加），逐个 id 的叶子键不在注册表里，[`set`] 会当成未知键拒掉。
/// 用户没设过的模块取它的出厂开关（`default_on`），与核心查表时的口径一致。
pub fn emoji_modules(data_dir: &Path) -> Vec<EmojiModuleState> {
    let cfg = Config::load(Some(data_dir)).unwrap_or_default();
    Config::emoji_modules(Some(data_dir))
        .into_iter()
        .map(|m| EmojiModuleState {
            enabled: cfg
                .input
                .emoji
                .modules
                .get(&m.id)
                .copied()
                .unwrap_or(m.default_on),
            id: m.id,
            label: m.label,
            description: m.description,
            user: m.user,
        })
        .collect()
}

/// 打开 / 关闭一个 emoji 附加模块（写用户层 `input.emoji.modules.<id>`）。只认
/// [`emoji_modules`] 列出的 id：写一个不存在的模块不报错的话，就是「开关点了没反应」。
/// 同 [`set`]，只写盘，生效要另发重载。
pub fn set_emoji_module(data_dir: &Path, id: &str, on: bool) -> Result<(), String> {
    if !Config::emoji_modules(Some(data_dir))
        .iter()
        .any(|m| m.id == id)
    {
        return Err(format!("没有这个 emoji 模块：{id}"));
    }
    let _w = write_guard();
    Config::set_user_value(&["input", "emoji", "modules", id], toml::Value::Boolean(on))
        .map_err(|e| e.to_string())
}

/// 候选提示（`ui.tooltip.sections`）的一段与它当前的开关（设置页「长按提示」一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooltipSectionState {
    /// 段名原文（可能是模板，如 `编码{(${code_source})}`）——开关时原样回传，按它定位
    pub label: String,
    /// 给人看的段名：模板里的 `{…}` 可选组去掉（`编码{(${code_source})}` → `编码`）
    pub title: String,
    pub enabled: bool,
}

/// 候选提示的全部段（顺序即显示顺序）及当前开关。
///
/// 单独开口而不走 [`set`]：段列表是结构体数组，[`ConfigValue`] 表达不了「改第几项的某个字段」，
/// 照 [`set_emoji_module`] 的先例按段名开关。只列有段名的段（无名段在设置里没法称呼）。
pub fn tooltip_sections(data_dir: &Path) -> Vec<TooltipSectionState> {
    let cfg = Config::load(Some(data_dir)).unwrap_or_default();
    cfg.ui
        .tooltip
        .sections
        .iter()
        .filter(|s| !s.label.is_empty())
        .map(|s| TooltipSectionState {
            title: section_title(&s.label),
            label: s.label.clone(),
            enabled: s.enabled,
        })
        .collect()
}

/// 段名模板 → 显示名：去掉 `{…}` 可选组（组里是「有值才出现」的变量部分）。
fn section_title(label: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in label.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// 打开 / 关闭名为 `label` 的提示段：整份段列表写回用户层（改回与出厂一致时由
/// `set_user_value` 剪掉，不留残余）。没有这一段 → Err。同 [`set`]，只写盘，生效要另发重载。
///
/// 已知取舍（与电脑版设置页的段列表对话框相同）：开关过任意一段后，用户层存的是整份
/// 列表，此后出厂对段模板的修改到不了这位用户——直到他把列表改回与出厂一致。出厂段列表
/// 变更时由 `Config::migrate_stale_factory_tooltip_sections_value` 认出存量副本迁移。
pub fn set_tooltip_section_enabled(data_dir: &Path, label: &str, on: bool) -> Result<(), String> {
    // 读与写都在锁里：load 到 set_user_value 之间别处写进来的，会被这里整份写回冲掉
    let _w = write_guard();
    let mut cfg = Config::load(Some(data_dir)).map_err(|e| e.to_string())?;
    // 整份段列表写回：降级时这份是出厂值，写回会抹掉用户原有的段设置
    if cfg
        .degradation
        .blocks_write_back("ui.tooltip.sections", "移动端提示段开关")
    {
        return Err("配置文件有问题，本次未写入；修好配置文件后再试".to_string());
    }
    let sec = cfg
        .ui
        .tooltip
        .sections
        .iter_mut()
        .find(|s| s.label == label)
        .ok_or_else(|| format!("没有这一段提示：{label}"))?;
    sec.enabled = on;
    let value = toml::Value::try_from(&cfg.ui.tooltip.sections).map_err(|e| e.to_string())?;
    Config::set_user_value(&["ui", "tooltip", "sections"], value).map_err(|e| e.to_string())
}

// ── 快捷输入的候选来源（quick_mix 的 members）──────────────────────────
//
// `schema.mix_modes` 是结构体数组，[`ConfigValue`] 表达不了「改第几项的某个字段」，
// 照 [`set_tooltip_section_enabled`] 的先例单独开口。members 是候选来源的单一真相源：
// 有无即开关、顺序即优先级（见 `MixModeConfig::members`）。界面行序、保留界面不认识的
// 成员这些规则在宿主侧（与电脑版设置的 `mix_members.rs` 同一套），这里只管读与整列写回。

/// 内置「快捷」融合实例 id（同 `wind_config::config::QUICK_MIX_ID`）。
const QUICK_MIX_ID: &str = wind_config::config::QUICK_MIX_ID;

/// 快捷输入候选来源：当前生效的成员序与出厂成员序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickMixMembers {
    /// 当前生效（加载期迁移之后）的成员；配置里没有 quick_mix 实例时为 `None`
    pub current: Option<Vec<String>>,
    /// 出厂（L1⊕L2⊕L2.5）的成员，「恢复默认」用它
    pub defaults: Vec<String>,
}

/// 读快捷输入候选来源（当前与出厂）。
pub fn quick_mix_members(data_dir: &Path) -> QuickMixMembers {
    let cfg = Config::load(Some(data_dir)).unwrap_or_default();
    let defaults = Config::system_preset_value(Some(data_dir))
        .ok()
        .and_then(|v| v.try_into::<Config>().ok())
        .unwrap_or_default();
    let members_of = |c: &Config| {
        c.schema
            .mix_modes
            .iter()
            .find(|m| m.id == QUICK_MIX_ID)
            .map(|m| m.members.clone())
    };
    QuickMixMembers {
        current: members_of(&cfg),
        defaults: members_of(&defaults).unwrap_or_default(),
    }
}

/// 整列写回快捷输入的候选来源（写用户层 `schema.mix_modes`）。只写盘，生效要另发重载。
///
/// 不能拿 `Config::load` 的结果整份写回：加载期 `normalize` 已把 `trigger_keys` 折算进
/// `keys.key_actions` 并清空，写回就等于清空了触发键。所以在原始值上改：用户层写过
/// `schema.mix_modes` 就改那份，没写过就改出厂那份（与出厂一致时被剪掉），其余字段原样保留。
pub fn set_quick_mix_members(data_dir: &Path, members: Vec<String>) -> Result<(), String> {
    let _w = write_guard();
    let cfg = Config::load(Some(data_dir)).map_err(|e| e.to_string())?;
    // 整个数组写回：降级时读到的不可信，写回会抹掉用户原有的融合实例设置
    if cfg
        .degradation
        .blocks_write_back("schema.mix_modes", "移动端快捷候选来源")
    {
        return Err("配置文件有问题，本次未写入；修好配置文件后再试".to_string());
    }
    let preset = Config::system_preset_value(Some(data_dir)).map_err(|e| e.to_string())?;
    let base = user_mix_modes()
        .or_else(|| mix_modes_of(&preset))
        .unwrap_or_default();
    let patched = patch_quick_mix_members(base, &members)?;
    Config::set_user_value(&["schema", "mix_modes"], toml::Value::Array(patched))
        .map_err(|e| e.to_string())
}

/// 用户层里显式写下的 `schema.mix_modes`（没写过 = `None`）。语法坏的情形已被调用方的降级闸挡掉。
fn user_mix_modes() -> Option<Vec<toml::Value>> {
    let path = Config::user_config_dir()?.join("config.toml");
    let text = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    mix_modes_of(&value)
}

fn mix_modes_of(root: &toml::Value) -> Option<Vec<toml::Value>> {
    root.get("schema")?.get("mix_modes")?.as_array().cloned()
}

/// 纯函数：把 `mix_modes` 原始数组里 quick_mix 那一项的 members 换成 `members`，其余原样。
/// 没有 quick_mix 实例 → Err（不静默新建实例）；成员去重、去空白项。
fn patch_quick_mix_members(
    mut mix_modes: Vec<toml::Value>,
    members: &[String],
) -> Result<Vec<toml::Value>, String> {
    let mut seen = std::collections::HashSet::new();
    let list: Vec<toml::Value> = members
        .iter()
        .map(|m| m.trim())
        .filter(|m| !m.is_empty() && seen.insert(m.to_string()))
        .map(|m| toml::Value::String(m.to_string()))
        .collect();
    let entry = mix_modes
        .iter_mut()
        .filter_map(toml::Value::as_table_mut)
        .find(|t| t.get("id").and_then(toml::Value::as_str) == Some(QUICK_MIX_ID))
        .ok_or_else(|| "配置里没有快捷输入（quick_mix）实例".to_string())?;
    entry.insert("members".to_string(), toml::Value::Array(list));
    Ok(mix_modes)
}

/// 写一个配置键到用户层。
///
/// 只写盘，不重载引擎；重载走 [`crate::MobileCommand::ReloadConfig`]，宿主可把连改的
/// 若干项合成一次重载（重载会重建引擎集）。
///
/// 失败原因（磁盘只读、用户层语法坏且备份失败等）要展示给用户。
pub fn set(key: &str, value: ConfigValue) -> Result<(), String> {
    if key.is_empty() {
        return Err("配置键为空".to_string());
    }
    // 只认注册表里的键：清单写错键名时当场报错，而不是落盘一个引擎永远不读的值。
    if !config_schema::is_known_key(key) {
        return Err(format!("未登记的配置键：{key}"));
    }
    let path: Vec<&str> = key.split('.').collect();
    let _w = write_guard();
    Config::set_user_value(&path, value.into_toml()).map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "config_api_tests.rs"]
mod tests;
