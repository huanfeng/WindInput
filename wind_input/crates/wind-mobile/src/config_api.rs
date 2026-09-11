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

/// core 的配置字段注册表（248 个键）。
pub fn registry() -> Vec<ConfigFieldInfo> {
    config_schema::registry()
        .iter()
        .map(|f| ConfigFieldInfo {
            key: f.key.to_string(),
            ty: type_name(&f.ty).to_string(),
            options: match f.ty {
                FieldType::Enum(vals) => vals.iter().map(|s| s.to_string()).collect(),
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
    // 编码区归候选区自绘（移动端必须如此，非偏好）：默认的 `app_inline` 是把编码塞进
    // 宿主组合区、协调器**不下发 preedit** 给候选窗。移动端没有桌面那种浮在光标旁的
    // 候选窗，编码要显示在键盘上方的编码栏里，就必须让协调器把 preedit 发出来。
    cfg.ui.candidate.preedit_display = "candidate_top".to_string();
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
    let path: Vec<&str> = key.split('.').collect();
    Config::set_user_value(&path, value.into_toml()).map_err(|e| e.to_string())
}
