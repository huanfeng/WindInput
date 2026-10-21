//! 移动端的**配置读写门面**。
//!
//! # 为什么必须走这里，而不是让宿主自己读写文件
//!
//! 移动端宿主（Android 的 Kotlin 侧）本来就有 TOML 解析器，"自己读改写用户层
//! config.toml" 看起来是最短的路。它不成立，因为 [`Config::set_user_value`] 里压着
//! 一整套用血换来的讲究，宿主重写一份必然违反其中某几条：
//!
//! - **容错解析**：用户层语法坏了不能整份丢弃，要尽量救回；
//! - **损坏即备份，备份失败就拒绝写**——写回会永久删掉没救回的行，无备份写回等于
//!   用户亲手点的保存把自己的配置吃了；
//! - **等于默认值的键不写入**：写了就等于把当前默认"钉死"，将来默认值一改用户被留在
//!   旧行为上。`schema.mix.auto_commit_block_on_pinyin` 已经这么引爆过一次
//!   （见 `set_user_value` 的注释，那里记着 62 颗同形的雷）。
//!
//! 这些规矩属于**配置系统**，不属于某个宿主。所以这一层只做转发与类型投影，
//! 一行落盘逻辑都不自己写。
//!
//! # 快照为什么是 TOML 而不是 JSON
//!
//! 配置文件本身就是 TOML，宿主侧也已经有解析器（布局/清单都在用）。用同一种表示，
//! 「快照里的键」与「文件里的键」就是同一个东西，不需要第二套映射，也就没有第二处
//! 会漂移的地方。
//!
//! # 注册表为什么要一起暴露
//!
//! 宿主的设置清单里写的是**字符串键**。写错一个字母的后果是「这个开关点了没反应」，
//! 而且不报错——本仓最不愿意再制造的那类缺陷。把 core 的字段注册表原样交给宿主，
//! 宿主就能在加载清单时当场比对：清单里有而注册表没有的键，直接报出来。

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use wind_config::Config;
use wind_config::config_schema::{self, FieldType};

/// 一个配置值（跨语言边界的最小值类型集合）。
///
/// 刻意不含 Map / 结构体数组：那些在设置界面上要专门的编辑器，不是「一个值」，
/// 目前经快照读出来展示、由桌面端编辑。将来移动端要编辑它们时，应当为**那一类**
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
    /// 值域进注册表而不是让宿主手抄一份，是因为手抄的那份会滞后：core 加了新取值而
    /// 宿主没跟上，用户就永远看不到新选项；反向漂移（宿主多出 core 不认的值）则是
    /// 「选了没反应」。`ui.candidate.layout` 在桌面端已经踩过这个形状。
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
            // LayoutEnum 也报成 enum（见 type_name），值域同样要交出去——此前只填了 Enum，
            // LayoutEnum 键到宿主手里是「枚举但没有选项」，设置页画不出选择器。
            options: match f.ty {
                FieldType::Enum(vals) | FieldType::LayoutEnum(vals) => {
                    vals.iter().map(|s| s.to_string()).collect()
                }
                _ => Vec::new(),
            },
        })
        .collect()
}

/// **当前生效**配置的全量快照（TOML 文本）。
///
/// 生效 = 四层合并 **再套上移动端强制项**后的结果，不是用户层那一份，也不是裸的
/// 四层合并结果。设置界面要显示的正是这个：
///
/// - 用户没设过的键也得显示出它此刻实际取什么值，否则开关的显示态与引擎行为对不上；
/// - 移动端强制改写的键（见 [`apply_mobile_overrides`]）必须显示**改写后**的值，
///   否则设置页会照着磁盘上的原值显示一个引擎根本没在用的状态。
pub fn snapshot(data_dir: &Path) -> String {
    let mut cfg = Config::load(Some(data_dir)).unwrap_or_else(|e| {
        tracing::warn!("配置快照加载失败，回落代码默认值: {e}");
        Config::default()
    });
    apply_mobile_overrides(&mut cfg);
    to_toml(&cfg)
}

/// 移动端对加载结果的**强制改写**——`MobileCore::new` 与 [`snapshot`] 共用这一份。
///
/// 抽出来是因为两处必须一致：构造时改了、快照时不改，设置页显示的就是一个引擎没在用
/// 的值；用户还能去"改"它，改完毫无反应。共用一个函数，新增强制项时不会漏掉快照。
///
/// ⚠ 这里改写的键**不该出现在移动端设置清单里**——它们不是用户可选项。
pub fn apply_mobile_overrides(cfg: &mut Config) {
    // 启用的方案：每次（构造、每次重载）都重算——此前只在构造时把可选方案换成「装了的全部」，
    // 重载一读回出厂 `available = ["wubi86", "wubi86_pinyin"]`，拼音、双拼、英文就从列表里没了
    // （真机：快捷面板一操作方案就少了）。
    apply_enabled_schemas(cfg);
    // 编码区归候选区自绘（移动端必须如此，非偏好）：默认的 `app_inline` 是把编码塞进
    // 宿主组合区、协调器**不下发 preedit** 给候选窗。移动端没有桌面那种浮在光标旁的
    // 候选窗，编码要显示在键盘上方的编码栏里，就必须让协调器把 preedit 发出来。
    cfg.ui.candidate.preedit_display = "candidate_top".to_string();
    // 联想态空格出空格、不选联想（手机习惯，非偏好）：豆包、微信都是收起联想出空格，汉字后
    // 双击空格出「。」也靠这一下真的是空格。桌面默认 `space_commits = true` 是实体键盘语义
    // （联想高亮在候选窗里看得见，空格选词顺手）；手机上联想挤在候选条里，空格选走它就是
    // 「我想打空格却上了个词」。此前 Android 侧先发 Esc 再出空格权宜绕过，改到这里收口。
    cfg.input.association.space_commits = false;
}

/// **出厂默认**配置的全量快照（TOML 文本）。
///
/// # 「默认」是哪一层，必须说清楚
///
/// 这里给的是 **L1⊕L2⊕L2.5**（代码默认 ⊕ `data/config.toml` ⊕ 定制层），
/// **不是** `Config::default()`。两者在「出厂 config.toml 显式写了、且与代码默认不同」
/// 的键上分叉——`schema.active` 这类只写在 L2 的键是最明显的一类。
///
/// 判据来自 [`Config::set_user_value`]：它的「等于默认就不落盘」比对的正是这一层
/// （`preset_for_pruning`）。设置端若拿 L1 当默认，在那些分叉的键上会有两个后果：
/// - 「默认：X」标错——显示的是代码默认，而引擎实际回落到的是出厂默认；
/// - **「恢复默认」失效**：写回 L1 的值不等于 L2 的默认，键留在用户层没被剪掉，
///   用户看到的是「点了恢复默认，它还在那儿」。
///
/// 取不到出厂配置时降级到 L1 并**留一条警告**。那说明 `WIND_INSTALL_ROOT` 没钉对
/// （见 `crate::mount_dirs`）；此时核心那边的剪枝也同样是关着的，两处症状同源——
/// 安卓端「恢复默认后键仍在用户层」最初就是这么来的，与本层选哪一层默认无关。
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
// 移动端语义：**装了的方案默认全部可用**；用户在设置里关掉某些，就写用户层 `schema.available`
// （装了的里头他要的那些）。英文方案恒在其中——它是中英键切换的目标，关掉中英键就失灵；
// 也因此用户写下的列表恒含 `english`，不会与出厂列表相等而被「等于默认不落盘」剪掉。

/// 已装方案（构造时扫 assets 得到，见 [`set_installed_schemas`]）。覆盖钩子是函数指针、
/// 捕获不了变量，故放进程级。
static INSTALLED: OnceLock<Vec<String>> = OnceLock::new();

/// 英文方案 id：中英键切换的目标，恒启用。
pub const ENGLISH_SCHEMA: &str = "english";

/// 记下已装方案（构造时调一次；同进程再调无效，装了什么在进程生命期内不变）。
pub fn set_installed_schemas(ids: Vec<String>) {
    let _ = INSTALLED.set(ids);
}

/// 已装方案；构造前为空。
pub fn installed_schemas() -> &'static [String] {
    INSTALLED.get().map(Vec::as_slice).unwrap_or(&[])
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
    enabled_from(installed_schemas(), user_chosen_schemas().as_deref())
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
    let mut list = enabled_from(installed, user_chosen_schemas().as_deref());
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

/// 用户层写入的进程内互斥。
///
/// `Config::set_user_value` 是「读整份 config.toml → 改一键 → 整份写回」，本身不加锁；
/// [`set_tooltip_section_enabled`] 更是先 load 再整份写回段列表。宿主的设置页把各卡片的写入
/// 各自丢到 IO 线程上，连点两个开关就是两条并发的读改写——后写的拿着旧文件覆盖先写的，
/// 界面上表现为「刚打开的开关又弹回去」。本模块的写口一律经这把锁。
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
/// 单独开口而不走 [`set`]：`input.emoji.modules` 是**按模块 id 为键的表**，注册表里只登记了
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
/// ⚠ 已知取舍（与电脑版设置页的段列表对话框相同）：开关过任意一段后，用户层存的是整份
/// 列表，此后出厂对段模板的修改到不了这位用户——直到他把列表改回与出厂一致。出厂段列表
/// 变更时由 `Config::migrate_stale_factory_tooltip_sections_value` 认出存量副本迁移。
pub fn set_tooltip_section_enabled(data_dir: &Path, label: &str, on: bool) -> Result<(), String> {
    // 读与写都在锁里：load 到 set_user_value 之间别处写进来的，会被这里整份写回冲掉
    let _w = write_guard();
    let mut cfg = Config::load(Some(data_dir)).map_err(|e| e.to_string())?;
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

/// 写一个配置键到用户层。
///
/// **只写盘，不重载引擎**——重载走 [`crate::MobileCommand::ReloadConfig`]。分开是为了
/// 让宿主能把「连改若干项」合成一次重载：重载会重建引擎集，逐项重载会让设置页每点
/// 一下卡一下。
///
/// @return 失败原因（磁盘只读、用户层语法坏且备份失败…）。这些必须让用户看见：
/// 都是他能处理的问题，而「点了没反应」他处理不了。
pub fn set(key: &str, value: ConfigValue) -> Result<(), String> {
    if key.is_empty() {
        return Err("配置键为空".to_string());
    }
    // 只认注册表里的键：设置清单写错键名时当场报错，而不是落盘一个引擎永远不读的值
    // ——那样的症状是「这个开关点了没反应」，且什么都不提示。
    if !config_schema::is_known_key(key) {
        return Err(format!("未登记的配置键：{key}"));
    }
    let path: Vec<&str> = key.split('.').collect();
    let _w = write_guard();
    Config::set_user_value(&path, value.into_toml()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 移动端强制项在快照里可见：设置页、引擎读到的必须是同一个值。
    #[test]
    fn overrides_reach_snapshot_defaults() {
        let cfg = apply_overrides_to(Config::default());
        assert!(!cfg.input.association.space_commits, "联想态空格不选联想");
        assert_eq!(cfg.ui.candidate.preedit_display, "candidate_top");
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// 没选过 = 装了的全部；选过 = 交集 + 英文；一个中文都没留 = 全部
    #[test]
    fn enabled_schemas_rules() {
        let installed = ids(&["english", "pinyin", "shuangpin", "wubi86"]);
        assert_eq!(enabled_from(&installed, None), installed, "没选过：全部");
        assert_eq!(
            enabled_from(&installed, Some(&ids(&["wubi86", "pinyin", "gone"]))),
            ids(&["pinyin", "wubi86", "english"]),
            "交集（没装的丢掉）、英文补上"
        );
        assert_eq!(
            enabled_from(&installed, Some(&ids(&["english"]))),
            installed,
            "一个中文方案都没留：退回全部"
        );
        // 出厂列表 ["wubi86", "wubi86_pinyin"] 在移动端不该起作用——它不在用户层，
        // 这里只验纯函数：用户层若真写了它，也照交集办（不含英文时补上）
        let with_mix = ids(&["english", "wubi86", "wubi86_pinyin"]);
        assert!(
            enabled_from(&with_mix, Some(&ids(&["wubi86", "wubi86_pinyin"])))
                .contains(&"english".to_string())
        );
    }

    /// 段名模板的显示名：可选组整个去掉，字面文字留下
    #[test]
    fn tooltip_section_title_strips_optional_groups() {
        assert_eq!(section_title("编码{(${code_source})}"), "编码");
        assert_eq!(section_title("拆字"), "拆字");
        assert_eq!(section_title("{${x}}"), "");
    }

    /// 不存在的段名当场拒绝（不落盘）
    #[test]
    fn unknown_tooltip_section_is_rejected() {
        let dir = std::env::temp_dir();
        assert!(set_tooltip_section_enabled(&dir, "没有这一段", true).is_err());
    }

    /// 不存在的模块 id 当场拒绝（不落盘）
    #[test]
    fn unknown_emoji_module_is_rejected() {
        let dir = std::env::temp_dir();
        assert!(set_emoji_module(&dir, "no_such_module", true).is_err());
    }

    /// 内置模块都在列表里，没设过的取出厂开关
    #[test]
    fn builtin_emoji_modules_listed_with_factory_defaults() {
        let dir = std::env::temp_dir().join(format!("wind_mobile_emoji_{}", std::process::id()));
        let mods = emoji_modules(&dir);
        for m in wind_config::EMOJI_MODULES {
            let got = mods
                .iter()
                .find(|s| s.id == m.id)
                .expect("内置模块应在列表里");
            assert!(!got.user);
            assert_eq!(got.label, m.label);
        }
    }

    /// 注册表里报成 enum 的键都要带值域：设置页拿空选项画不出选择器（LayoutEnum 曾漏填）。
    #[test]
    fn every_enum_field_has_options() {
        let missing: Vec<_> = registry()
            .into_iter()
            .filter(|f| f.ty == "enum" && f.options.is_empty())
            .map(|f| f.key)
            .collect();
        assert!(missing.is_empty(), "以下枚举键没有选项：{missing:?}");
    }
}
