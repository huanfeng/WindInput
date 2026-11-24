//! `wind_input config check --custom <data_custom 目录>`：定制层体检。
//!
//! 读者是第三方定制者，不是本仓开发者：措辞里不出现「L2.5」「段级降级」「注册表」这类内部词，
//! 每条结论都写成「哪个文件的哪个键 / 会发生什么 / 该怎么改」。定制层的失败几乎全是静默的，
//! 这个命令把终端用户的反馈提前到打包之前。
//!
//! [`check_layer`] 不读环境变量、不读用户层 `%APPDATA%`、不碰 RPC：它体检的是定制包发出去之后
//! 会怎样，定制者本机的个人设置不该影响结论。data 层与当前版本号都由调用方显式传入。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use wind_config::config::{DEFAULT_PINYIN_SCHEMA, MIX_MEMBER_PRIMARY_PINYIN};
use wind_config::config_schema::{FieldType, ValidateError, field, registry, validate};
use wind_config::{BoundAction, CUSTOM_MANIFEST_NAME, Config};

/// 定制层里的配置文件名（与 data 层同名同构）。
const CONFIG_NAME: &str = "config.toml";

/// 报告级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Level {
    /// 会让功能不生效或让终端用户的配置回落出厂值。
    Error,
    /// 现在能工作，但形态不对——多半在下一次主程序升级时变成故障。
    Warn,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Error => "错误",
            Level::Warn => "警告",
        }
    }
}

/// 一条体检结论。
#[derive(Debug, Clone)]
pub(super) struct Finding {
    pub level: Level,
    /// 出问题的文件，定制者视角的相对路径（如 `custom.toml`、`config.toml`）。
    pub file: String,
    /// 具体的键 / 条目 id；整份文件的问题为 `None`。
    pub item: Option<String>,
    /// 问题是什么、会发生什么。
    pub problem: String,
    /// 该怎么改。
    pub fix: String,
}

impl Finding {
    fn new(
        level: Level,
        file: impl Into<String>,
        item: Option<String>,
        problem: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            level,
            file: file.into(),
            item,
            problem: problem.into(),
            fix: fix.into(),
        }
    }
}

/// 体检结果。
#[derive(Debug, Default)]
pub(super) struct Report {
    pub findings: Vec<Finding>,
    /// 清单里读到的定制版身份摘要（用于抬头），清单不可用时为 `None`。
    pub identity: Option<String>,
}

impl Report {
    pub fn errors(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.level == Level::Error)
            .count()
    }

    pub fn warns(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.level == Level::Warn)
            .count()
    }

    fn push(&mut self, f: Finding) {
        self.findings.push(f);
    }

    fn error(
        &mut self,
        file: impl Into<String>,
        item: Option<String>,
        problem: impl Into<String>,
        fix: impl Into<String>,
    ) {
        self.push(Finding::new(Level::Error, file, item, problem, fix));
    }

    fn warn(
        &mut self,
        file: impl Into<String>,
        item: Option<String>,
        problem: impl Into<String>,
        fix: impl Into<String>,
    ) {
        self.push(Finding::new(Level::Warn, file, item, problem, fix));
    }
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

/// 体检一个定制层目录。
///
/// - `custom_dir`：待检查的 `data_custom/` 目录（可以还没安装，指向打包目录即可）。
/// - `data_dir`：出厂 `data/` 目录。`None` 时需要出厂数据作对照的检查一律跳过并声明，不用残缺对照下结论。
/// - `app_version`：当前主程序版本，用于与清单的 `base_version` 比对。
pub(super) fn check_layer(custom_dir: &Path, data_dir: Option<&Path>, app_version: &str) -> Report {
    let mut rep = Report::default();

    if !custom_dir.is_dir() {
        rep.error(
            custom_dir.display().to_string(),
            None,
            "这个目录不存在（或不是目录）。",
            "用 --custom 指向定制包里与 data/ 同级的 data_custom 目录。",
        );
        return rep;
    }

    // ① 清单：它在场且能解析，是「本机是不是定制版」的**唯一**判据。
    let manifest = check_manifest(custom_dir, app_version, &mut rep);

    // ② 定制层自己的 config.toml：三类键问题 + 两个静默陷阱 + 冗余键。
    let custom_cfg = read_toml(&custom_dir.join(CONFIG_NAME));
    let custom_value = match &custom_cfg {
        TomlRead::Parsed(v) => {
            check_custom_config(v, data_dir, &mut rep);
            Some(v)
        }
        // 只做减法的定制包完全可以没有 config.toml。
        TomlRead::Absent => None,
        TomlRead::Unreadable(e) => {
            rep.error(
                CONFIG_NAME,
                None,
                format!(
                    "文件在，但读不出来：{e}\n\
                     整份 config.toml 会被跳过——本层写的配置差异**一条都不生效**，而程序照常\
                     启动，日志里只有一行 INFO，你和用户都看不出少了什么。\n\
                     最常见的一种：用记事本编辑过（注释里有中文），另存时选了 ANSI/GBK 而不是 \
                     UTF-8。"
                ),
                "确认文件是 **UTF-8** 编码（记事本另存为时在编码下拉里选 UTF-8），\
                 以及没有被别的程序占用、权限可读。",
            );
            None
        }
        TomlRead::BadSyntax(e) => {
            rep.error(
                CONFIG_NAME,
                None,
                format!(
                    "TOML 语法错误：{e}\n\
                     整份 config.toml 会被跳过——本层写的配置差异一条都不生效，而程序照常启动，\
                     日志里只有一行 WARN。"
                ),
                "修正语法后重跑本命令。",
            );
            None
        }
    };

    // ③ 清单的减法清单与「配置里还在引用它」的交叉核对。
    if let Some(m) = &manifest {
        check_hide_lists(custom_dir, data_dir, m, custom_value, &mut rep);
        check_settings_hide(m, &mut rep);
    }

    // ④ 简繁数据：按名逐文件覆盖，只放一两本是**正常**的；名字对不上才是问题。
    check_opencc(custom_dir, data_dir, &mut rep);

    // 错误排前面。**稳定**排序：各级别内部保持发现顺序（清单 → 配置 → 减法 → 简繁），
    // 那个顺序本身就是「先看整层活不活、再看细节」的阅读顺序。
    rep.findings.sort_by_key(|f| f.level);
    rep
}

// ---------------------------------------------------------------------------
// ① 清单
// ---------------------------------------------------------------------------

/// 清单里所有**认识**的字段。清单刻意没有 `deny_unknown_fields`（旧程序要能忽略未来新增
/// 的段），代价就是 `[schema] hide`（少写一个 s）这类拼写错误被静默忽略——只能在这里报。
const MANIFEST_KNOWN: &[(&str, &[&str])] = &[
    ("custom", &["id", "name", "version", "base_version"]),
    ("schemas", &["hide"]),
    ("themes", &["hide"]),
    ("settings", &["hide"]),
];

fn check_manifest(
    custom_dir: &Path,
    app_version: &str,
    rep: &mut Report,
) -> Option<wind_config::CustomManifest> {
    let path = custom_dir.join(CUSTOM_MANIFEST_NAME);
    if !path.is_file() {
        rep.error(
            CUSTOM_MANIFEST_NAME,
            None,
            "缺少 custom.toml。程序判断「这台机器装的是不是定制版」只看这一个文件在不在、\
             能不能解析——它不在，整个 data_custom/ 目录就被完全忽略：你放进去的方案、主题、\
             配置一个都不会生效，而程序一切正常、日志里连 WARN 都没有。",
            "在定制层根目录建 custom.toml，最小内容：\n\
             \x20   [custom]\n\
             \x20   id = \"my-edition\"\n\
             \x20   name = \"我的定制版\"\n\
             \x20   version = \"1.0\"\n\
             \x20   base_version = \"<主程序版本>\"",
        );
        return None;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            rep.error(
                CUSTOM_MANIFEST_NAME,
                None,
                format!("读不出来：{e}。整个定制层不启用，程序回落成原版。"),
                "确认文件权限与编码（UTF-8）。",
            );
            return None;
        }
    };
    let raw: toml::Value = match text.parse() {
        Ok(v) => v,
        Err(e) => {
            rep.error(
                CUSTOM_MANIFEST_NAME,
                None,
                format!(
                    "TOML 语法错误：{e}\n\
                     清单解析不了 ⇒ **整个定制层不启用**（不是「少了 hide 清单」，是连方案、\
                     主题、配置一起回落原版）。这是最该第一时间发现的一类问题。"
                ),
                "修正语法后重跑本命令。",
            );
            return None;
        }
    };

    // 未知字段：解析得过、但一个字都不起作用。
    check_manifest_unknown_fields(&raw, rep);

    let manifest: wind_config::CustomManifest = match toml::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            rep.error(
                CUSTOM_MANIFEST_NAME,
                None,
                format!(
                    "字段类型不对：{e}\n\
                     同样是**整个定制层不启用**。最常见的是 hide 写成了字符串而不是数组。"
                ),
                "`hide` 必须是数组：`hide = [\"wubi86\"]`，只删一个也要带方括号。",
            );
            return None;
        }
    };

    // 身份：没有它，定制版用户报障时连他装的是不是定制版都判断不出来。
    let id = manifest.custom.id.trim();
    if id.is_empty() {
        rep.warn(
            CUSTOM_MANIFEST_NAME,
            Some("custom.id".into()),
            "没写定制版 id。日志、关于页、报障信息里都显示成 `<未命名>`——用户报障时你分辨\
             不出他装的是你的哪个包，甚至分辨不出他是不是装了定制版。",
            "填一个稳定不变的短标识，如 id = \"huma-edition\"。",
        );
    }
    if manifest.custom.version.trim().is_empty() {
        rep.warn(
            CUSTOM_MANIFEST_NAME,
            Some("custom.version".into()),
            "没写定制包自身的版本号，报障时无法区分你发出去的是第几版。",
            "填 version = \"1.0\" 这样的字符串，每次发包递增。",
        );
    }
    check_base_version(&manifest.custom.base_version, app_version, rep);

    rep.identity = Some(identity_line(&manifest));
    Some(manifest)
}

fn check_manifest_unknown_fields(raw: &toml::Value, rep: &mut Report) {
    let Some(table) = raw.as_table() else {
        rep.error(
            CUSTOM_MANIFEST_NAME,
            None,
            "顶层不是配置表。",
            "清单由 [custom] / [schemas] / [themes] / [settings] 这几个段构成。",
        );
        return;
    };
    for (section, value) in table {
        let Some((_, known_keys)) = MANIFEST_KNOWN.iter().find(|(s, _)| s == section) else {
            let hint = MANIFEST_KNOWN
                .iter()
                .map(|(s, _)| *s)
                .collect::<Vec<_>>()
                .join(" / ");
            rep.warn(
                CUSTOM_MANIFEST_NAME,
                Some(section.clone()),
                format!(
                    "`[{section}]` 不是清单认识的段，会被**静默忽略**——写在里面的内容一条都\
                     不生效，而清单本身解析成功、定制层照常启用，所以现象是「我明明写了 hide，\
                     可那个方案还在」。"
                ),
                format!("清单只有这几个段：{hint}。注意 `[schemas]`/`[themes]` 都是复数。"),
            );
            continue;
        };
        let Some(sub) = value.as_table() else {
            rep.warn(
                CUSTOM_MANIFEST_NAME,
                Some(section.clone()),
                format!("`{section}` 不是配置段（表），里面的内容不会被读到。"),
                format!("写成段：`[{section}]` 后面跟它的键。"),
            );
            continue;
        };
        for key in sub.keys() {
            if !known_keys.contains(&key.as_str()) {
                rep.warn(
                    CUSTOM_MANIFEST_NAME,
                    Some(format!("{section}.{key}")),
                    format!("`{section}.{key}` 不是清单认识的键，会被静默忽略。"),
                    format!("`[{section}]` 段里可用的键：{}。", known_keys.join(" / ")),
                );
            }
        }
    }
}

/// `base_version` 与当前主程序版本的差距。
///
/// 判据取前两段（主.次）：补丁号差异是常态，每次都告警只会让人把输出当噪音。
fn check_base_version(base_version: &str, app_version: &str, rep: &mut Report) {
    let base = base_version.trim();
    if base.is_empty() {
        rep.warn(
            CUSTOM_MANIFEST_NAME,
            Some("custom.base_version".into()),
            format!(
                "没写 base_version，无从判断这份定制包是基于哪个主程序版本做的\
                 （当前主程序是 {app_version}）。"
            ),
            format!("填 base_version = \"{app_version}\"。"),
        );
        return;
    }
    match (major_minor(base), major_minor(app_version)) {
        (Some(a), Some(b)) if a == b => {}
        (Some(_), Some(_)) => rep.warn(
            CUSTOM_MANIFEST_NAME,
            Some("custom.base_version".into()),
            format!(
                "这份定制包基于 {base} 制作，当前主程序是 {app_version}，主/次版本已经不同。\
                 跨版本时配置键可能改名或退役，定制层里写着旧键就会静默失效。"
            ),
            format!(
                "按当前版本复核一遍定制层的 config.toml（本命令的其余结论已经在做这件事），\
                 确认无误后把 base_version 更新为 \"{app_version}\"。"
            ),
        ),
        _ => rep.warn(
            CUSTOM_MANIFEST_NAME,
            Some("custom.base_version".into()),
            format!("base_version = \"{base}\" 不是 `主.次.补丁` 形式的版本号，无法与当前版本 {app_version} 比对。"),
            format!("写成 base_version = \"{app_version}\" 这样的形式。"),
        ),
    }
}

fn major_minor(v: &str) -> Option<(u32, u32)> {
    let mut it = v.trim().split('.');
    let major = it.next()?.trim().parse().ok()?;
    let minor = it.next().unwrap_or("0").trim().parse().ok()?;
    Some((major, minor))
}

// ---------------------------------------------------------------------------
// ② 定制层的 config.toml
// ---------------------------------------------------------------------------

/// 定制层里不该声明的键：它们会被一次性物化进终端用户的 `%APPDATA%` 并打死版本标记，此后
/// 定制包再改这些绑定，对存量用户永远不生效且没有日志。对第三方不可发现，只能在这里报出来。
const MATERIALIZED_KEYS: &[&str] = &[
    "keys.key_actions",
    "input.temp_pinyin.trigger_keys",
    "input.temp_english.trigger_keys",
];

/// 一个待判定的配置项。
struct Leaf {
    key: String,
    value: toml::Value,
}

fn check_custom_config(custom: &toml::Value, data_dir: Option<&Path>, rep: &mut Report) {
    // 冗余检查要拿「出厂值」作对照，而出厂值只在 data/config.toml 里看得见。
    // 拿不到就**不做**这项检查（只用 L1 默认当对照会把 data 层改过的键全报成「与出厂不同」，
    // 那是在制造噪音）——同 `preset_for_pruning` 取不到 preset 就退化为不清理。
    let factory = data_dir
        .map(|d| d.join(CONFIG_NAME))
        .filter(|p| p.is_file())
        .and_then(|p| read_toml_opt(&p))
        .map(|sys| {
            let mut base = Config::default()
                .to_toml_value()
                .expect("Config::default 必须可序列化（wind-config 有守门测试）");
            merge_value(&mut base, sys);
            base
        });

    let mut leaves = Vec::new();
    collect_leaves("", custom, &mut leaves, rep);

    for Leaf { key, value } in leaves {
        let Some(fld) = field(&key) else {
            rep.warn(
                CONFIG_NAME,
                Some(key.clone()),
                "当前版本的配置项里没有这个键：多半是旧版本遗留，也可能是拼错了。它不会\
                 连累同一段里的其它键，但你以为配上的那个行为未必配上了——少数旧键还被\
                 兼容迁移读着，那是过渡措施，不保证长期有效。",
                format!(
                    "用 `wind_input config list {}` 找当前的键名，改用新键；确认没有对应的\
                     新键就删掉它。",
                    key.split('.').next().unwrap_or(&key)
                ),
            );
            continue;
        };

        match validate(&key, &value) {
            Ok(()) => {}
            Err(ValidateError::TypeMismatch { expected, got }) => {
                rep.error(
                    CONFIG_NAME,
                    Some(key.clone()),
                    format!(
                        "类型不符：应为 {expected}，这里写的是 {got}。\n\
                         后果最重的一类：加载时这个键所在的整段配置会被丢掉换成出厂默认值，\
                         而这份定制包发出去之后，**每个用户、每次启动**都会踩到——他们自己\
                         对这一段做的所有设置都跟着回落，且只有日志里一行 WARN。"
                    ),
                    format!("把值改成 {expected}。"),
                );
                continue;
            }
            Err(ValidateError::EnumOutOfRange { allowed, got }) => {
                rep.error(
                    CONFIG_NAME,
                    Some(key.clone()),
                    format!(
                        "值 \"{got}\" 不在这个键的合法取值里。合法值：{}。",
                        allowed.join(" / ")
                    ),
                    format!("改成 {} 之一。", allowed.join(" / ")),
                );
                continue;
            }
            Err(ValidateError::UnknownKey) => continue, // 上面 field() 已拦，不可达
        }

        // Map 的**键名值域**（如 `ui.font.scripts` 只认这几个文字类别）。值域为空表示
        // 键由定制者自由命名（自定义标点映射就是），此时无从校验。
        if let FieldType::Map(allowed) = fld.ty
            && !allowed.is_empty()
            && let Some(t) = value.as_table()
        {
            for k in t.keys() {
                if !allowed.contains(&k.as_str()) {
                    rep.warn(
                        CONFIG_NAME,
                        Some(format!("{key}.{k}")),
                        format!("`{k}` 不是这张表认识的项，会被静默丢弃（现象是「配了没反应」）。"),
                        format!("可用的项：{}。", allowed.join(" / ")),
                    );
                }
            }
        }

        if MATERIALIZED_KEYS.contains(&key.as_str()) {
            rep.warn(
                CONFIG_NAME,
                Some(key.clone()),
                format!(
                    "定制层里**不建议**声明 `{key}`。首次启动时，程序会把折算后的按键绑定\
                     一次性写进终端用户的个人配置并打上完成标记；此后你在定制包里再改这些\
                     绑定，对**已经装过旧版包的用户永远不生效**，而且没有任何日志、用户和你\
                     都看不出来。只有全新安装的用户才拿得到新绑定。"
                ),
                "把按键绑定留给用户自己在设置页里改；确实要改默认绑定，就随定制包附一份说明，\
                 而不是靠这个键。",
            );
        }

        // 只写差异键：与出厂值相同的键现在没有任何作用，但它会在主程序**将来改这个默认值**
        // 时把旧值顶住——那时的现象是「新版本的改进在定制版上没生效」，极难归因。
        if let Some(base) = &factory
            && let Some(bv) = get_path(base, &key)
            && toml_eq(bv, &value)
        {
            rep.warn(
                CONFIG_NAME,
                Some(key.clone()),
                "这个键的值与出厂值完全相同，现在不起任何作用。但主程序将来调整这个默认值时，\
                 定制层里的这份旧值会把新默认顶住——现象是「新版本的改进在定制版上没生效」。",
                "删掉它。定制层只写与出厂**不同**的键，差异越小，跨版本存活率越高。",
            );
        }
    }

    // 第二道判据在第一道之后跑，排除第一道已点过名的键。「已点过名」直接从 `rep` 里取，
    // 别改成手动维护的集合：新增检查时必漏，同一个键就会报两遍。
    let named_before: BTreeSet<String> = rep
        .findings
        .iter()
        .filter(|f| f.level == Level::Error && f.file == CONFIG_NAME)
        .filter_map(|f| f.item.clone())
        .collect();
    check_deserialization(factory.as_ref(), custom, &named_before, rep);
}

/// 第二道类型判据：把合并结果真的反序列化一次（即 `Config::load` 的 `try_into`），接住注册表
/// 看不见的坏值：`Map` / `StructList` 表里的值（如自定义标点写成 `"," = "，"`，值须是数组，
/// 运行时 `input.punct` 整段回落出厂默认），以及注册表表达不了的值域（`per_page = -1`）。
///
/// 三条免责：
/// 1. 先拿 `L1⊕L2` 单独试一次作对照，出厂配置自己就反序列化不了时跳过并声明，不栽给定制层；
/// 2. 前一道判据已点名的键不重复报（那边措辞更好）；
/// 3. 这里没跑 `Config::load` 的 `migrate_*_value`（wind-config 私有）。其中会改写已注册键的两条
///    （`index_labels`、`empty_code_behavior`）是普通标量键，注册表检查先一步点名，免责 2 兜住。
///    别在 CLI 里复刻迁移名单，那是第二个真相源。
fn check_deserialization(
    factory: Option<&toml::Value>,
    custom: &toml::Value,
    named_before: &BTreeSet<String>,
    rep: &mut Report,
) {
    // 没有出厂对照就没有免责 1，宁可不做：拿 L1 默认当基准会把 data 层的既有问题算到
    // 定制层头上。同 `preset_for_pruning` 取不到 preset 就退化为不清理。
    let Some(base) = factory else {
        return;
    };
    if Config::from_toml_value(base.clone()).is_err() {
        rep.warn(
            format!("data/{CONFIG_NAME}（出厂值）"),
            None,
            "出厂配置自身就反序列化不了，无法判断定制层是否引入了新的坏值，本项检查已跳过\
             （其余检查照常）。",
            "这不是定制包的问题。请向主程序作者报告，并附上本命令的完整输出。",
        );
        return;
    }
    let mut full = base.clone();
    merge_value(&mut full, custom.clone());
    let Err(e) = Config::from_toml_value(full) else {
        return;
    };
    // ⚠️ `trim_end`：toml 的错误串自带尾随换行，直接嵌进多行 problem 会在输出里留一行
    // 只有缩进的空行。
    let msg = e.to_string().trim_end().to_string();
    // toml 0.8 的错误串自带 `` in `点分路径` ``，那是**精确到出错那个值**的定位
    // （`input.punct.custom_mappings.,`），比我们自己能算出来的任何路径都准。
    let path = extract_toml_error_path(&msg);
    if let Some(p) = &path
        && named_before
            .iter()
            .any(|k| p == k || p.starts_with(&format!("{k}.")))
    {
        return; // 前一道已经点过名（含它的子路径），措辞更好，不重复
    }
    let head = match &path {
        Some(p) => format!("`{p}` 的值加载不了。"),
        None => "定制层的某个值加载不了。".to_string(),
    };
    rep.error(
        CONFIG_NAME,
        path.clone(),
        format!(
            "{head}原始错误：{msg}\n\
             这是把「出厂配置 ⊕ 你的定制层」真的加载一遍得到的结果，与程序启动时走的是\
             同一条路。加载失败 ⇒ 这个值所在的整段配置会被丢掉换成出厂默认值，而这份\
             定制包发出去之后，**每个用户、每次启动**都会踩到。\n\
             最常见的一种：映射表的值写成了单个值而不是数组（`\",\" = \"，\"` 应为 \
             `\",\" = [\"，\"]`）——这类表的**内容**注册表检查不到，只有真加载一次才看得见。"
        ),
        match &path {
            Some(p) => format!("按上面的原始错误改 `{p}` 的值（它已经点到具体位置了）。"),
            None => "按上面的原始错误定位并修正。".to_string(),
        },
    );
}

/// 从 toml 的反序列化错误串里抠出 `` in `点分路径` `` 的那个路径。
///
/// 抠不出来就返回 `None`，调用方原样报整条错误，绝不猜。`in` 前面的空白可能是换行
/// （`expected usize\nin `ui.candidate.per_page``），不能只认空格。
fn extract_toml_error_path(msg: &str) -> Option<String> {
    let needle = "in `";
    let mut from = 0;
    while let Some(rel) = msg[from..].find(needle) {
        let at = from + rel;
        let preceded_by_space = at > 0
            && msg[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        if preceded_by_space {
            let start = at + needle.len();
            if let Some(end) = msg[start..].find('`') {
                let path = msg[start..start + end].trim();
                if !path.is_empty() {
                    return Some(path.to_string());
                }
            }
        }
        from = at + needle.len();
    }
    None
}

/// 把定制层的 config.toml 展开成待判定的配置项。
///
/// 两条停止判据：① 键登记为不透明叶子（`input.punct.custom_mappings`、`keys.key_actions` 这类，
/// 子路径是定制者的数据），下钻会把映射表逐条报成「未知键」；② 前缀底下一个已登记的键都没有
/// （如整段 `[ui.oldsection]`），整段报一次。
fn collect_leaves(prefix: &str, value: &toml::Value, out: &mut Vec<Leaf>, rep: &mut Report) {
    if !prefix.is_empty() {
        // ① ⟹ ②（① 是 ② 的子集），当前行为上 ① 被 ② 完全覆盖；② 由
        // `removed_section_is_reported_once` 钉住。① 的判据必须是 `Map | StructList` 而非「登记过」：
        // 将来若同时登记 `ui.font` 与 `ui.font.family`，「登记过」会把整张表当叶子吞掉、`family`
        // 不再被校验（同 `config_schema::collect_leaf_keys` 只认 `Map(_)`）。
        if is_opaque_leaf(prefix) {
            out.push(Leaf {
                key: prefix.to_string(),
                value: value.clone(),
            });
            return;
        }
        if !has_registered_descendant(prefix) {
            out.push(Leaf {
                key: prefix.to_string(),
                value: value.clone(),
            });
            return;
        }
        // 前缀底下有已登记的键 ⇒ 它是配置段，必须是表，否则加载时整段回落出厂默认。
        if !value.is_table() {
            rep.error(
                CONFIG_NAME,
                Some(prefix.to_string()),
                format!(
                    "`{prefix}` 是一个配置**段**，这里却给了一个 {} 值。加载时这一段会整段\
                     回落出厂默认，本定制包的每个用户都会踩到。",
                    toml_type_name(value)
                ),
                format!("写成 `[{prefix}]` 段，把具体的键写在段里。"),
            );
            return;
        }
    }
    match value {
        toml::Value::Table(t) => {
            for (k, v) in t {
                let child = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                collect_leaves(&child, v, out, rep);
            }
        }
        _ => out.push(Leaf {
            key: prefix.to_string(),
            value: value.clone(),
        }),
    }
}

/// 判据①：这个键登记为不透明叶子，整张表 / 整个数组就是一个配置项，里面是定制者的数据。
///
/// 判据必须是 `Map | StructList`，不能放宽成「登记过」（见 `collect_leaves`），由
/// `is_opaque_leaf_only_matches_opaque_types` 钉住。
fn is_opaque_leaf(prefix: &str) -> bool {
    matches!(
        field(prefix).map(|f| f.ty),
        Some(FieldType::Map(_) | FieldType::StructList)
    )
}

fn has_registered_descendant(prefix: &str) -> bool {
    let with_dot = format!("{prefix}.");
    registry().iter().any(|f| f.key.starts_with(&with_dot))
}

// ---------------------------------------------------------------------------
// ③' 设置界面减法（`[settings] hide`）
// ---------------------------------------------------------------------------

/// 只校验**选择器语法**与 `feature:` 的取值。`key:` 是否真的命中某个设置项，这里判不了：
/// 设置项清单住在设置程序里（含 `local.*` 这类不进 config 的条目），拿 config 注册表去比会
/// 把合法的条目报成错。命中与否由设置程序启动时检查，一条都没命中的打 WARN 进设置日志。
fn check_settings_hide(manifest: &wind_config::CustomManifest, rep: &mut Report) {
    let (_, errors) = wind_config::SettingsHide::parse(&manifest.settings.hide);
    for e in errors {
        rep.warn(
            CUSTOM_MANIFEST_NAME,
            Some("settings.hide".into()),
            format!("{e}\n这一条会被忽略，对应的设置照常显示；其余条目不受影响。"),
            "例：hide = [\"key:input.temp_english.*\", \"feature:wizard\"]。\
             注意隐藏只影响设置界面，功能照常生效——要关掉功能，请在 data_custom/config.toml \
             里把值设好。",
        );
    }
}

// ---------------------------------------------------------------------------
// ③ 减法清单与「还在被引用」
// ---------------------------------------------------------------------------

fn check_hide_lists(
    custom_dir: &Path,
    data_dir: Option<&Path>,
    manifest: &wind_config::CustomManifest,
    custom_cfg: Option<&toml::Value>,
    rep: &mut Report,
) {
    let hidden_schemas: BTreeSet<&str> = manifest
        .schemas
        .hide
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let hidden_themes: BTreeSet<&str> = manifest
        .themes
        .hide
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if hidden_schemas.is_empty() && hidden_themes.is_empty() {
        return;
    }

    // hide 的目标在盘上存在吗？拼错的 id 完全无害地什么都不做，最难发现。
    match data_dir {
        Some(dd) => {
            let schemas = scan_schema_ids(&[dd.join("schemas"), custom_dir.join("schemas")]);
            for id in &hidden_schemas {
                if !schemas.contains(*id) {
                    rep.warn(
                        CUSTOM_MANIFEST_NAME,
                        Some(format!("schemas.hide → {id}")),
                        format!(
                            "`{id}` 在 data/schemas 和 data_custom/schemas 里都找不到对应的 \
                             {id}.schema.toml，这条 hide 什么都不会删掉（多半是拼错了）。"
                        ),
                        format!(
                            "核对方案文件名：hide 里写的是**方案 id**，也就是 `<id>.schema.toml` \
                             去掉 `.schema.toml` 之后那一截。盘上现有：{}。",
                            join_preview(&schemas)
                        ),
                    );
                }
            }
            let themes = scan_theme_ids(&[dd.join("themes"), custom_dir.join("themes")]);
            for id in &hidden_themes {
                if !themes.contains(*id) {
                    rep.warn(
                        CUSTOM_MANIFEST_NAME,
                        Some(format!("themes.hide → {id}")),
                        format!(
                            "`{id}` 在 data/themes 和 data_custom/themes 里都没有对应的主题目录，\
                             这条 hide 什么都不会删掉（多半是拼错了）。"
                        ),
                        format!(
                            "主题 id 就是 themes/ 下那个含 theme.toml 的目录名。盘上现有：{}。",
                            join_preview(&themes)
                        ),
                    );
                }
            }
        }
        None => rep.warn(
            CUSTOM_MANIFEST_NAME,
            None,
            "找不到出厂 data/ 目录，「hide 的方案/主题是否真的存在」这项检查已跳过。",
            "用 --data <目录> 指向定制包里的 data/，或把本命令放在安装目录下跑。",
        ),
    }

    // 还在被引用的 hide 目标。引用可能来自出厂 data/config.toml，故合并出「这个定制版
    // 生效的配置」再查——只看定制层自己的 config.toml 会漏掉绝大多数真实情况。
    // 刻意**不含用户层**：体检的是这个包发出去之后的样子。
    let merged = merged_config(data_dir, custom_cfg);
    for r in collect_references(&merged) {
        let hidden = match r.kind {
            RefKind::Schema => hidden_schemas.contains(r.id.as_str()),
            RefKind::Theme => hidden_themes.contains(r.id.as_str()),
        };
        if !hidden {
            continue;
        }
        let origin = origin_of(&r.key, custom_cfg, data_dir);
        rep.error(
            origin.file,
            Some(r.key.clone()),
            format!(
                "{} `{}` 已经被 custom.toml 的 hide 删掉了，但 `{}` 还指着它。{}",
                r.kind.noun(),
                r.id,
                r.key,
                r.consequence
            ),
            format!(
                "二选一：把 `{}` 从 hide 名单里拿掉；或者{}（{}）。",
                r.id, r.fix_hint, origin.how
            ),
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RefKind {
    Schema,
    Theme,
}

impl RefKind {
    fn noun(self) -> &'static str {
        match self {
            RefKind::Schema => "方案",
            RefKind::Theme => "主题",
        }
    }
}

/// 一处「配置里指名道姓提到某个 id」的引用点。
struct Reference {
    kind: RefKind,
    /// 点分配置键（`schema.mix_modes` 这类数组会带上下标与成员名，好让人一眼定位）。
    key: String,
    id: String,
    /// 用户实际会看到的现象。
    consequence: &'static str,
    fix_hint: &'static str,
}

fn push(
    out: &mut Vec<Reference>,
    kind: RefKind,
    key: String,
    id: &str,
    consequence: &'static str,
    fix_hint: &'static str,
) {
    let id = id.trim();
    if !id.is_empty() {
        out.push(Reference {
            kind,
            key,
            id: id.to_string(),
            consequence,
            fix_hint,
        });
    }
}

fn collect_references(merged: &toml::Value) -> Vec<Reference> {
    let mut out = Vec::new();

    if let Some(s) = get_path(merged, "schema.active").and_then(toml::Value::as_str) {
        push(
            &mut out,
            RefKind::Schema,
            "schema.active".into(),
            s,
            "启动时活跃方案会被降级到别的方案：用户装上定制版，工具栏上显示的不是他期望的\
             那个方案，只有日志里一行 WARN。",
            "把 schema.active 改成定制版里真实存在的方案",
        );
    }
    if let Some(arr) = get_path(merged, "schema.available").and_then(toml::Value::as_array) {
        for (i, v) in arr.iter().enumerate() {
            if let Some(s) = v.as_str() {
                push(
                    &mut out,
                    RefKind::Schema,
                    format!("schema.available[{i}]"),
                    s,
                    "它会从方案切换列表里消失（这一条本身无害），但如果列表因此变空，\
                     方案菜单会是空的、循环切换键毫无反应。",
                    "把它从 schema.available 里删掉",
                );
            }
        }
    }
    for key in ["schema.primary_pinyin", "schema.primary_codetable"] {
        if let Some(s) = get_path(merged, key).and_then(toml::Value::as_str) {
            push(
                &mut out,
                RefKind::Schema,
                key.into(),
                s,
                "凡是靠这个「主方案」派生的功能（快捷输入里的拼音、混输的码表来源）都会落空。",
                "把它改成定制版里真实存在的方案",
            );
        }
    }
    let primary_pinyin = get_path(merged, "schema.primary_pinyin")
        .and_then(toml::Value::as_str)
        .unwrap_or_default();
    if let Some(arr) = get_path(merged, "schema.mix_modes").and_then(toml::Value::as_array) {
        for (i, m) in arr.iter().enumerate() {
            let mode_id = m
                .get("id")
                .and_then(toml::Value::as_str)
                .unwrap_or("")
                .to_string();
            let Some(members) = m.get("members").and_then(toml::Value::as_array) else {
                continue;
            };
            for member in members.iter().filter_map(toml::Value::as_str) {
                // `$primary_pinyin` 是占位符，解析成 schema.primary_pinyin（空=全拼）。
                let resolved = if member == MIX_MEMBER_PRIMARY_PINYIN {
                    if primary_pinyin.is_empty() {
                        DEFAULT_PINYIN_SCHEMA
                    } else {
                        primary_pinyin
                    }
                } else {
                    member
                };
                push(
                    &mut out,
                    RefKind::Schema,
                    format!("schema.mix_modes[{i}]({mode_id}).members → {member}"),
                    resolved,
                    "这个成员会被静默跳过：用户进这个融合模式时，少了一整类候选，没有任何提示。",
                    "把这个成员从 members 里删掉",
                );
            }
        }
    }
    // 按键绑定里的 `special:` / `toggle_schema:` / `switch_schema:`。
    if let Some(t) = get_path(merged, "keys.key_actions").and_then(toml::Value::as_table) {
        for (k, v) in t {
            if let Some(s) = v.as_str() {
                push_bound_action(&mut out, &format!("keys.key_actions.{k}"), s);
            }
        }
    }
    if let Some(s) = get_path(merged, "schema.codetable.z_key_action").and_then(toml::Value::as_str)
    {
        push_bound_action(&mut out, "schema.codetable.z_key_action", s);
    }
    if let Some(s) = get_path(merged, "ui.theme.name").and_then(toml::Value::as_str) {
        push(
            &mut out,
            RefKind::Theme,
            "ui.theme.name".into(),
            s,
            "候选窗会落到兜底主题上：用户看到的配色不是定制版设定的那一套。",
            "把 ui.theme.name 改成定制版里保留的主题",
        );
    }
    out
}

fn push_bound_action(out: &mut Vec<Reference>, key: &str, raw: &str) {
    let (id, consequence, fix_hint) = match BoundAction::parse(raw) {
        BoundAction::Special(id) => (
            id,
            "按下这个键什么都不会发生——特殊模式进不去，按键落回普通输入照常出字符，\
             用户完全看不出「有个功能没了」。",
            "把这条绑定删掉或改指别的模式",
        ),
        BoundAction::ToggleSchema(id) | BoundAction::SwitchSchema(id) => (
            id,
            "按下这个键切不过去，方案切换热键静默失效。",
            "把这条绑定改指定制版里保留的方案",
        ),
        _ => return,
    };
    out.push(Reference {
        kind: RefKind::Schema,
        key: key.to_string(),
        id,
        consequence,
        fix_hint,
    });
}

/// 引用点来自哪一层——决定了定制者该去改哪个文件。
struct Origin {
    file: String,
    how: &'static str,
}

fn origin_of(key: &str, custom_cfg: Option<&toml::Value>, data_dir: Option<&Path>) -> Origin {
    // `schema.mix_modes[0](quick_mix).members → english` 这类展示用的键要先还原成真实路径。
    let path = key.split(['[', ' ']).next().unwrap_or(key);
    if custom_cfg.and_then(|v| get_path(v, path)).is_some() {
        return Origin {
            file: CONFIG_NAME.into(),
            how: "改定制层自己的 config.toml",
        };
    }
    let in_data = data_dir
        .map(|d| d.join(CONFIG_NAME))
        .and_then(|p| read_toml_opt(&p))
        .and_then(|v| get_path(&v, path).cloned())
        .is_some();
    if in_data {
        Origin {
            file: format!("data/{CONFIG_NAME}（出厂值）"),
            how: "在 data_custom/config.toml 里写这个键把出厂值盖掉——别去改 data/",
        }
    } else {
        Origin {
            file: "（内置默认值）".into(),
            how: "在 data_custom/config.toml 里写这个键把内置默认值盖掉",
        }
    }
}

// ---------------------------------------------------------------------------
// ④ 简繁数据
// ---------------------------------------------------------------------------

/// 简繁转换链按文件名跨层取，定制层只放一两本 `.octrie` 是被支持的用法，不报错。真正会失效的是
/// 名字对不上，两种后果不同、措辞要分开：完全对不上（`MyDict`）在任何平台都取不到；只差大小写
/// （`stphrases`）在大小写不敏感的卷（Windows、macOS 默认）上会被 `resolve_overridable` 命中，
/// 只在 Linux 取不到。
fn check_opencc(custom_dir: &Path, data_dir: Option<&Path>, rep: &mut Report) {
    let dir = custom_dir.join("opencc");
    if !dir.is_dir() {
        return;
    }
    let Some(dd) = data_dir else { return };
    let factory: BTreeSet<String> = list_file_names(&dd.join("opencc"), ".octrie");
    if factory.is_empty() {
        return;
    }
    for name in list_file_names(&dir, ".octrie") {
        if factory.contains(&name) {
            continue;
        }
        // 只差大小写？出厂那本的正确拼法一并给出来。
        let case_twin = factory
            .iter()
            .find(|f| f.eq_ignore_ascii_case(&name))
            .cloned();
        let problem = match &case_twin {
            Some(correct) => format!(
                "大小写与出厂的 {correct}.octrie 不一致。简繁转换链是按**文件名**逐本跨层取的，\
                 而它按出厂那个拼法（{correct}.octrie）去找：在 Windows 与 macOS 这类\
                 大小写不敏感的卷上，你这本**现在能被取到**——但那是在赌文件系统的行为。\
                 换到区分大小写的地方（Linux、开了大小写敏感的 NTFS 目录、某些打包/解包链路）\
                 就取不到了，届时程序照常用出厂那本工作，现象是「同一个包在这台机器上换了词表、\
                 在那台上一个字都没变」。"
            ),
            None => "出厂 data/opencc/ 里没有叫这个名字的。简繁转换链是按**文件名**逐本跨层\
                     取的，链里不认识这个名字 ⇒ 这本在**任何**平台上都永远不会被加载，程序\
                     照常用出厂的那几本工作，现象是「换了词表却一个字都没变」。"
                .to_string(),
        };
        let fix = match &case_twin {
            Some(correct) => format!(
                "把它改名成 {correct}.octrie（逐字对齐大小写）。\
                 只覆盖其中一两本是正常用法，没放的那几本会自动用出厂的。"
            ),
            None => format!(
                "对照 data/opencc/ 的文件名改名（大小写要逐字一致）。现有：{}。\
                 只覆盖其中一两本是正常用法，没放的那几本会自动用出厂的。",
                join_preview(&factory)
            ),
        };
        rep.warn(format!("opencc/{name}.octrie"), None, problem, fix);
    }
}

// ---------------------------------------------------------------------------
// 输出
// ---------------------------------------------------------------------------

/// 渲染报告到 stdout，返回进程退出码（0 = 无错误）。
pub(super) fn render(rep: &Report, custom_dir: &Path, data_dir: Option<&Path>, app_version: &str) {
    println!("定制层：{}", custom_dir.display());
    match &rep.identity {
        Some(s) => println!("定制版：{s}"),
        None => println!("定制版：<清单不可用>"),
    }
    println!("当前主程序版本：{app_version}");
    match data_dir {
        Some(d) => println!("出厂数据：{}", d.display()),
        None => println!("出厂数据：<未找到，部分检查已跳过>"),
    }
    println!();

    if rep.findings.is_empty() {
        println!("✓ 没有发现问题。");
        return;
    }
    for (i, f) in rep.findings.iter().enumerate() {
        let head = match &f.item {
            Some(item) => format!("{} · {item}", f.file),
            None => f.file.clone(),
        };
        println!("[{}] {} {}", f.level.label(), i + 1, head);
        print_block("问题", &f.problem);
        print_block("改法", &f.fix);
        println!();
    }
    println!("共 {} 条错误、{} 条警告。", rep.errors(), rep.warns());
    if rep.errors() > 0 {
        println!("错误必须改：它们会让定制内容不生效，或让每个终端用户的配置回落出厂值。");
    }
}

/// 带标签的多行块：标签只出现在第一行，续行对齐缩进。每行都顶一个「问题：」会读成
/// 三条独立结论，而它们是同一句话的换行。
fn print_block(label: &str, text: &str) {
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            println!("      {label}：{line}");
        } else {
            println!("            {line}");
        }
    }
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 读 TOML 文件。`Ok(None)` = 文件不存在。
fn read_toml(path: &Path) -> TomlRead {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        // 不能把读失败一律当成文件不存在：另存为 GBK 的 config.toml 不是合法 UTF-8，整层配置
        // 不生效，当成「没这个文件」就等于体检报全绿（清单那条路径同理，见 `check_manifest`）。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return TomlRead::Absent,
        Err(e) => return TomlRead::Unreadable(e),
    };
    match text.parse() {
        Ok(v) => TomlRead::Parsed(v),
        Err(e) => TomlRead::BadSyntax(e),
    }
}

/// 读一份**只作对照用**的 TOML（出厂 `data/config.toml`）。任何读不到/读不出的形态
/// 都退化为 `None`，由调用方跳过那项检查——出厂层的毛病不该栽给定制者。
fn read_toml_opt(path: &Path) -> Option<toml::Value> {
    match read_toml(path) {
        TomlRead::Parsed(v) => Some(v),
        _ => None,
    }
}

/// 读一份 TOML 的四种结局。把「不存在」与「读不出来」分开是 M2 的全部内容。
enum TomlRead {
    /// 文件不存在——对定制层的 `config.toml` 是完全正常的（只做减法的包不必有它）。
    Absent,
    Parsed(toml::Value),
    /// 存在但读不出来：编码不是 UTF-8、权限、被占用。
    Unreadable(std::io::Error),
    BadSyntax(toml::de::Error),
}

/// 深合并，与 `wind_config` 的四层合并同语义（表递归，标量与数组整体覆盖）。
fn merge_value(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(bv) => merge_value(bv, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// 这个定制包发出去之后**生效**的配置：内置默认 ⊕ data/config.toml ⊕ data_custom/config.toml。
/// 刻意不含用户层。
fn merged_config(data_dir: Option<&Path>, custom_cfg: Option<&toml::Value>) -> toml::Value {
    let mut merged = Config::default()
        .to_toml_value()
        .expect("Config::default 必须可序列化（wind-config 有守门测试）");
    if let Some(sys) = data_dir
        .map(|d| d.join(CONFIG_NAME))
        .and_then(|p| read_toml_opt(&p))
    {
        merge_value(&mut merged, sys);
    }
    if let Some(c) = custom_cfg {
        merge_value(&mut merged, c.clone());
    }
    merged
}

fn get_path<'a>(root: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    let mut cur = root;
    for part in path.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

/// 值相等判定。整数与浮点跨类型比较（TOML 里 `18` 与 `18.0` 是同一个设定值，
/// 而设置页写回时恰好会把整值浮点写成整数）。
fn toml_eq(a: &toml::Value, b: &toml::Value) -> bool {
    match (a, b) {
        (toml::Value::Integer(x), toml::Value::Float(y))
        | (toml::Value::Float(y), toml::Value::Integer(x)) => (*x as f64) == *y,
        _ => a == b,
    }
}

fn toml_type_name(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::Boolean(_) => "布尔",
        toml::Value::Integer(_) => "整数",
        toml::Value::Float(_) => "小数",
        toml::Value::String(_) => "字符串",
        toml::Value::Array(_) => "数组",
        toml::Value::Table(_) => "表",
        toml::Value::Datetime(_) => "日期",
    }
}

/// 各层 `schemas/` 目录里的方案 id（`<id>.schema.toml` 去掉后缀）。
fn scan_schema_ids(dirs: &[PathBuf]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".schema.toml")
                && !id.is_empty()
            {
                out.insert(id.to_string());
            }
        }
    }
    out
}

/// 各层 `themes/` 目录里的主题 id（含 `theme.toml` 的子目录名）。
/// `_` 打头的是给别的主题继承用的基底片段，不是可选主题，与列表侧判据一致。
fn scan_theme_ids(dirs: &[PathBuf]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            if !e.path().is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with('_') && e.path().join("theme.toml").is_file() {
                out.insert(name);
            }
        }
    }
    out
}

/// 目录里指定后缀的文件名（已去掉后缀）。
fn list_file_names(dir: &Path, suffix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(suffix)
            && !stem.is_empty()
        {
            out.insert(stem.to_string());
        }
    }
    out
}

fn join_preview(items: &BTreeSet<String>) -> String {
    if items.is_empty() {
        return "（一个都没有）".into();
    }
    items.iter().cloned().collect::<Vec<_>>().join(" / ")
}

/// 身份摘要（抬头用）。
pub(super) fn identity_line(m: &wind_config::CustomManifest) -> String {
    let id = if m.custom.id.trim().is_empty() {
        "<未命名>"
    } else {
        m.custom.id.trim()
    };
    let name = m.custom.name.trim();
    let version = if m.custom.version.trim().is_empty() {
        "<无版本>"
    } else {
        m.custom.version.trim()
    };
    let base = if m.custom.base_version.trim().is_empty() {
        "<未声明>"
    } else {
        m.custom.base_version.trim()
    };
    if name.is_empty() {
        format!("{id} {version}（基于 {base}）")
    } else {
        format!("{name} {id} {version}（基于 {base}）")
    }
}

#[cfg(test)]
#[path = "custom_check_tests.rs"]
mod tests;
