//! 配置系统：四层合并（代码默认值 L1、系统配置 L2、定制版配置 L2.5、用户配置 L3）
//!
//! 配置文件为 TOML 格式，四层合并：默认值 → data/config.toml → data_custom/config.toml
//! → %APPDATA%/WindInput/config.toml。L2.5 见 `docs/design/data-custom-layer.md`。
//!
//! 顶级域按「正交大类」划分（见 docs/architecture/config-design-rules.md / docs/config-key-migration.md）：
//! schema(方案+pinyin+模式) / input(输入行为，含 default 启动默认 / phrase 短语) /
//! keys(全部按键) / ui(外观) / stats(统计) / debug。
//!
//! 按进程名的兼容性规则（HostRender 白名单、caret 定位等）不在这里，见
//! `app_compat.rs`（独立的 `compat.toml` 文件，字段级合并，键名不受本文件四层合并约束）。

use crate::by_layout::ByLayout;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{debug, error, info, warn};

/// 深合并两个 TOML 值：表递归合并（overlay 的键覆盖/新增），标量与数组由 overlay 整体覆盖。
/// 用于配置四层合并——overlay 中未出现的键保留 base 的值。
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

/// 在 TOML 表里按 `path` 导航（缺失则创建嵌套表），把叶子设为 `value`。
/// 路径中途若遇非表值（类型冲突）则覆盖为表。供 [`Config::set_user_value`] 部分合并用。
pub(crate) fn set_nested(table: &mut toml::Table, path: &[&str], value: toml::Value) {
    if path.len() == 1 {
        table.insert(path[0].to_string(), value);
        return;
    }
    let entry = table
        .entry(path[0].to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    match entry {
        toml::Value::Table(t) => set_nested(t, &path[1..], value),
        other => {
            let mut t = toml::Table::new();
            set_nested(&mut t, &path[1..], value);
            *other = toml::Value::Table(t);
        }
    }
}

/// 在 TOML 值里按 `path` 逐级取值（任一级缺失或非表则 `None`）。
/// 供 [`Config::set_user_value`] 与出厂默认（L1⊕L2⊕L2.5）比对用。
pub(crate) fn get_nested<'a>(root: &'a toml::Value, path: &[&str]) -> Option<&'a toml::Value> {
    let mut cur = root;
    for k in path {
        cur = cur.as_table()?.get(*k)?;
    }
    Some(cur)
}

/// 在 TOML 表里按 `path` 删除叶子，并回收因此变空的中间表（避免留下 `[schema.mix]` 这类空段）。
/// 返回是否真的删掉了东西。供用户层「与默认相同即不落盘」的收口用。
fn remove_nested(table: &mut toml::Table, path: &[&str]) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.len() == 1 {
        return table.remove(path[0]).is_some();
    }
    let Some(toml::Value::Table(t)) = table.get_mut(path[0]) else {
        return false;
    };
    let removed = remove_nested(t, &path[1..]);
    if removed && t.is_empty() {
        table.remove(path[0]);
    }
    removed
}

/// 从 `root`（用户层）删除所有与 `preset`（出厂默认 L1⊕L2⊕L2.5）取值相同的叶子键，返回删除数。
/// 纯函数，IO 由 [`Config::prune_user_config`] 负责，便于在不碰真实 `%APPDATA%` 的前提下测试。
///
/// 两道保险缺一不可：
/// 1. `is_known_key`：只碰注册表登记过的键。排除废弃键（清理它们必须走显式名单）与
///    `Map`/`StructList` 键的下钻子路径（如 `input.punct.custom_mappings."'1"`，删单条是错的语义）。
/// 2. 值必须与 preset 逐一相等（`get_nested` 两侧都取到才比）。
fn prune_redundant(root: &mut toml::Value, preset: &toml::Value) -> usize {
    let mut leaves = Vec::new();
    collect_leaf_paths(root, &mut Vec::new(), &mut leaves);
    let redundant: Vec<Vec<String>> = leaves
        .into_iter()
        .filter(|p| crate::config_schema::is_known_key(&p.join(".")))
        .filter(|p| {
            let refs: Vec<&str> = p.iter().map(|s| s.as_str()).collect();
            get_nested(root, &refs)
                .zip(get_nested(preset, &refs))
                .is_some_and(|(user, default)| user == default)
        })
        .collect();
    let toml::Value::Table(t) = root else {
        return 0;
    };
    let mut removed = 0usize;
    for p in &redundant {
        let refs: Vec<&str> = p.iter().map(|s| s.as_str()).collect();
        if remove_nested(t, &refs) {
            removed += 1;
        }
    }
    removed
}

/// 引导键物化迁移的当前版本，落在 `keys.key_actions_materialized`。
///
/// 递增即让 [`Config::materialize_key_actions`] 对所有用户再跑一次，会覆盖用户对这批键的修改；
/// 只在出厂绑定本身要变、且必须送达存量用户时才递增。
const KEY_ACTIONS_MATERIALIZE_VERSION: u32 = 1;

/// 已退役的配置键：结构体里已经没有它们，`load()` 时被 serde 静默丢弃，但会在用户层
/// `config.toml` 里永久留存（写回走原始 `toml::Value`，不经类型化结构体），让人以为还在起作用。
///
/// 只能用显式名单，不能改成「凡未登记键一律删」：`Map` 子路径（如 `input.punct.custom_mappings.<字符>`）
/// 同样不在注册表里，一刀切会删光用户的自定义映射。
const RETIRED_KEYS: &[&[&str]] = &[
    // 与 `mix_modes.members` 构成双真相源，已废弃：英文候选的开关只看 members 里有没有
    // `english`。不是 `schema.mix.enable_english` —— 那个还活着，是混输引擎
    // （`wubi86_pinyin` 这类方案）混入英文词库候选的开关，两者只是名字像。
    &["schema", "quick_input", "enable_english"],
    // 从未被任何逻辑读取过，关掉不产生任何效果（曾被误当作快捷输入的总开关）。
    // 真正的「禁用快捷输入」＝把 quick_mix 的 trigger_keys 清空。
    &["schema", "quick_input", "enabled"],
    // 随英文段独立迁至 `schema.english.frequency.code_scope`。不做值迁移：从未随发布版到用户手里，
    // 且新旧默认值都是 "candidate"。
    &["schema", "codetable", "frequency", "english_code_scope"],
    // 并入 `ui.candidate.font_size`（0 = 跟随主题）。本清单只删不迁，能登记它是因为
    // [`Config::prune_user_config`] 清本清单之前先把 [`Config::migrate_font_size_follow_theme_value`]
    // 的结果落盘。新增同类条目须照此先落盘迁移。
    &["ui", "candidate", "font_size_follow_theme"],
    // 悬停提示的六个旧开关，并入 `ui.tooltip.sections`。与上一条同一判据：
    // [`Config::prune_user_config`] 先把 [`Config::migrate_tooltip_sections_value`] 的结果
    // （`sections`）落盘、再清这六个键，清的时候它们已对生效值毫无影响。
    &["ui", "tooltip", "code_enabled"],
    &["ui", "tooltip", "pinyin_enabled"],
    &["ui", "tooltip", "pinyin_heteronyms"],
    &["ui", "tooltip", "pinyin_max_readings"],
    &["ui", "tooltip", "chaizi_enabled"],
    &["ui", "tooltip", "debug_enabled"],
    // 反查模式改造（docs/design/codetable-reverse-mode.md §9）：总开关去掉、「含未启用扩展词库」挪成
    // `input.reverse.lookup_disabled_dicts`。不做值迁移：两键从未随发布版到用户手里。
    &["input", "reverse", "enabled"],
    &["schema", "codetable", "lookup_disabled_dicts"],
    // 拆成 `_vertical` / `_horizontal`。与 `font_size_follow_theme` 同一判据：先把
    // [`Config::migrate_comment_max_chars_value`] 的结果落盘、再清。旧键常驻会让设置页改回 0 后
    // 下次 load 又被旧键补回原值。
    &["ui", "candidate", "comment_max_chars"],
    // 候选注释重构（docs/design/candidate-comment-templates.md）：注释模板、开关与三个编码来源
    // 并入 `[ui.comment]`。不做值迁移，破坏性变更登记在 docs/config-key-migration.md。
    // `schema.mix_modes[]` 条目里的同名键是数组元素、无法按路径列出，留在原处无人读取。
    &["ui", "candidate", "comment_enabled"],
    &["ui", "candidate", "comment_template_vertical"],
    &["ui", "candidate", "comment_template_horizontal"],
    &["input", "temp_english", "comment_template_vertical"],
    &["input", "temp_english", "comment_template_horizontal"],
    &["input", "temp_pinyin", "comment_template_vertical"],
    &["input", "temp_pinyin", "comment_template_horizontal"],
    &["input", "url", "comment_template_vertical"],
    &["input", "url", "comment_template_horizontal"],
    &["input", "email", "comment_template_vertical"],
    &["input", "email", "comment_template_horizontal"],
    &["input", "unicode", "comment_template_vertical"],
    &["input", "unicode", "comment_template_horizontal"],
    &["schema", "pinyin", "code_hint_source"],
    &["schema", "pinyin", "show_code_hint"],
    &["input", "temp_pinyin", "code_hint_source"],
    &["schema", "mix", "pinyin_code_hint"],
    // 按键级翻页溢出覆盖表，被翻页动作后缀取代（`page_prev:ignore` 这类写在 `keys.session_actions`
    // 里，见 docs/design/keys-overflow-per-key.md）。不做值迁移，这里只把它从用户文件里清掉。
    &["keys", "overflow", "page_key_overrides"],
];

/// 从用户层删除 [`RETIRED_KEYS`] 里的退役键，返回删除数。
///
/// 与 [`prune_redundant`] 同一条不变量：清理前后 `load()` 的结果逐键相同。幂等；
/// `remove_nested` 会顺带回收变空的父表。
fn prune_retired(root: &mut toml::Value) -> usize {
    let toml::Value::Table(t) = root else {
        return 0;
    };
    RETIRED_KEYS
        .iter()
        .filter(|path| remove_nested(t, path))
        .count()
}

/// 用户层是否已完成引导键物化（版本号 >= [`KEY_ACTIONS_MATERIALIZE_VERSION`]）。
/// 只认显式版本号，不做「key_actions 非空就算迁过」之类推断：手工配过 key_actions 的用户会被猜错，
/// 导致出厂绑定永久丢失。
fn already_materialized(root: &toml::Value) -> bool {
    get_nested(root, &["keys", "key_actions_materialized"])
        .and_then(toml::Value::as_integer)
        .unwrap_or(0)
        >= i64::from(KEY_ACTIONS_MATERIALIZE_VERSION)
}

/// 把 `bindings` 写进用户层 `keys.key_actions`、摘掉五处旧 `trigger_keys`、打上版本标记。
/// 返回摘掉的旧字段数。
///
/// 纯函数，IO 与安全闸由 [`Config::materialize_key_actions`] 负责：后者依赖 `user_config_dir()`，
/// 直接测会改写真实的用户配置。调用方须先用 [`already_materialized`] 判幂等，本函数每次都会照写。
fn materialize_into(
    root: &mut toml::Value,
    bindings: &BTreeMap<String, String>,
) -> anyhow::Result<usize> {
    let toml::Value::Table(t) = root else {
        anyhow::bail!("materialize_into: root 不是 table");
    };
    set_nested(
        t,
        &["keys", "key_actions"],
        toml::Value::try_from(bindings)?,
    );
    // 用户层里的旧字段一并清掉：它们已被物化进 key_actions，留着就是第二真相源，
    // 日后排查会再次踩「改了哪个都不对」的坑。L2 的那份不动（出厂声明处）。
    let mut dropped = 0usize;
    for path in [
        ["input", "temp_pinyin", "trigger_keys"].as_slice(),
        ["input", "temp_english", "trigger_keys"].as_slice(),
        ["input", "rare_char", "trigger_keys"].as_slice(),
    ] {
        if remove_nested(t, path) {
            dropped += 1;
        }
    }
    // `schema.mix_modes` 是结构体数组，逐元素摘掉 trigger_keys（不删元素本身：
    // id/name/members 还活着）。
    if let Some(arr) = t
        .get_mut("schema")
        .and_then(toml::Value::as_table_mut)
        .and_then(|s| s.get_mut("mix_modes"))
        .and_then(toml::Value::as_array_mut)
    {
        for m in arr.iter_mut() {
            if let Some(mt) = m.as_table_mut()
                && mt.remove("trigger_keys").is_some()
            {
                dropped += 1;
            }
        }
    }
    set_nested(
        t,
        &["keys", "key_actions_materialized"],
        toml::Value::Integer(i64::from(KEY_ACTIONS_MATERIALIZE_VERSION)),
    );
    Ok(dropped)
}

/// 收集 TOML 值里所有叶子路径（表递归；数组/标量视为叶子，不下钻）。
///
/// 数组必须当叶子：`schema.mix_modes` / `keys.page_keys` 这类整体就是一个配置项，
/// 下钻进数组元素会切出无法用 `path` 表达、也无法与出厂默认逐项比对的伪键。
fn collect_leaf_paths(v: &toml::Value, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    match v {
        toml::Value::Table(t) if !t.is_empty() => {
            for (k, sub) in t {
                prefix.push(k.clone());
                collect_leaf_paths(sub, prefix, out);
                prefix.pop();
            }
        }
        _ => out.push(prefix.clone()),
    }
}

/// 某一层配置文件没能按 TOML 语法读完的记录（见 [`parse_toml_lenient`]）。
///
/// 与 [`ConfigDegradation::sections`] 是两类不同的故障：
///
/// | | 段级降级 (`sections`) | 层级语法故障（本结构） |
/// |---|---|---|
/// | 出错位置 | 四层合并之后的 `try_into` | 合并之前的单文件 `from_str` |
/// | 文件本身 | 语法合法，只是某个键的类型不对 | 语法就不合法（重复键、缺引号、括号不配对……） |
/// | 影响范围 | 精确到一段/一个子表 | 整个文件，除非能逐行救回 |
///
/// 安全网的判据必须覆盖全部入口：语法故障若不记在这里，写盘闸、设置页横幅、CLI 报告都会判
/// 「一切正常」，而那正是用户配置被整表覆盖的时刻。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnparsableLayer {
    /// 层名：`data` / `custom` / `user`，与 [`LayerOrigin::layer`] 同域。
    pub layer: &'static str,
    /// 出问题的配置文件。
    pub path: PathBuf,
    /// 首个解析错误的原文（含 toml 自带的行列与代码片段）。后续错误多是同一处笔误的连锁，只留第一个。
    pub error: String,
    /// 被跳过的行号（1-based，升序）。空 = 一行都没救回来（整个文件作废）。
    pub skipped_lines: Vec<usize>,
    /// 救回来的顶层键数量。0 且 `skipped_lines` 为空 ⇒ 该层完全作废。
    pub salvaged_keys: usize,
    /// 文件不是 UTF-8（记事本存成 ANSI/GBK 最常见）：整层作废，与语法错误分开讲。
    pub not_utf8: bool,
}

/// 文案里最多列出几个行号，其余折叠成计数。完全不是 TOML 的文件会一路啃到
/// [`LENIENT_MAX_SKIPS`]，全列出会撑爆 toast 与横幅，且那时具体行号没有意义。
const LINES_LISTED_MAX: usize = 6;

impl UnparsableLayer {
    /// 是否救回了内容（哪怕只有一个键）。
    pub fn is_salvaged(&self) -> bool {
        self.salvaged_keys > 0
    }

    /// 行号短语（含「第…行」），最多列 [`LINES_LISTED_MAX`] 个，其余折叠成「等 N 处」。
    /// 无行号时返回空串。量词跟着内容一起给，免得调用方拼出「等 32 处行」这种语病。
    ///
    /// 供 core 内所有文案（`describe`、toast、CLI）共用。设置端另有一份等价实现，截断阈值须保持一致。
    pub fn lines_phrase(&self) -> String {
        if self.skipped_lines.is_empty() {
            return String::new();
        }
        let n = self.skipped_lines.len();
        let listed: Vec<String> = self
            .skipped_lines
            .iter()
            .take(LINES_LISTED_MAX)
            .map(|x| x.to_string())
            .collect();
        if n > LINES_LISTED_MAX {
            format!("第 {} 行等 {n} 处", listed.join("、"))
        } else {
            format!("第 {} 行", listed.join("、"))
        }
    }

    /// 给用户看的一句话：哪个文件、丢了哪几行（1-based，用户能直接跳过去改）。
    ///
    /// 判据是 [`Self::is_salvaged`] 而非 `skipped_lines` 非空：啃到 [`LENIENT_MAX_SKIPS`] 仍失败时
    /// 两者同时成立，那时一个字都没加载，不能说「加载其余内容」。
    pub fn describe(&self) -> String {
        let file = self.path.display();
        if self.not_utf8 {
            return format!("{file}：不是 UTF-8 编码，本次未能加载");
        }
        if !self.is_salvaged() {
            // 尝试过的行号仍要给（用户拿它去看那几行怎么了），但走紧凑形态：
            // 啃不动的文件会攒到 32 个行号，全列出来只是噪音。
            return if self.skipped_lines.is_empty() {
                format!("{file}：语法错误，本次未能加载")
            } else {
                format!(
                    "{file}：语法错误，本次未能加载（已尝试跳过{}仍无法解析）",
                    self.lines_phrase()
                )
            };
        }
        format!("{file}：已跳过{}后加载其余内容", self.lines_phrase())
    }
}

/// 本次加载中发生的降级记录（段级见 [`Config::deserialize_with_section_fallback`]，
/// 层级语法故障见 [`UnparsableLayer`]）。
///
/// 这不是配置项，是「这一份 `Config` 是怎么来的」的元信息。异常态，正常加载恒为空。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigDegradation {
    /// 被替换为 L1 默认的段，点分路径，按字典序排列。一层（`keys`）表示整个顶层段回落；
    /// 两层（`ui.font`）表示只有该子表回落、同段其余子表完好。
    pub sections: Vec<String>,
    /// 整份配置都回落到了 L1 默认——毒不在任何单段（例如顶层不是表）。
    /// 与 `sections` 互斥：走到这一步时 `sections` 为空，因为没能定位到任何有毒段。
    pub total_fallback: bool,
    /// 本次加载中语法不合法的配置文件，按层从低到高。正常加载恒为空。
    /// 非空时 [`Self::taints`] 对任何路径恒为真：被跳过的行可能落在任何段里。
    pub unparsable: Vec<UnparsableLayer>,
}

impl ConfigDegradation {
    /// 本次加载是否发生过降级（含层级语法故障）。
    pub fn is_degraded(&self) -> bool {
        self.total_fallback || !self.sections.is_empty() || !self.unparsable.is_empty()
    }

    /// 是否有配置文件语法不合法。与 [`Self::is_degraded`] 分开：语法故障写盘前要备份原文件，
    /// 段级降级不需要（文件本身是好的）。
    pub fn has_unparsable(&self) -> bool {
        !self.unparsable.is_empty()
    }

    /// 顶层段 `section` 是否受本次降级影响，含它的子路径。
    ///
    /// 不能写成精确相等：降级可细到子表，`keys` 段出问题时记下的可能是 `keys.key_actions`，
    /// 漏判会让本该拦下的写盘照样发生。实现与 [`Self::taints`] 共用一份。
    pub fn affects(&self, section: &str) -> bool {
        self.taints(section)
    }

    /// 写盘闸的核心判据：本次加载在 `path` 处的值是否不可信。
    ///
    /// 段级降级把 `load()` 的 `Err` 变成了「成功但某段是出厂值」，拿 `Config::load()` 的结果（或其
    /// 派生值）当种子整表写回用户层或导出的路径，落盘前必须先问这里，否则降级会变成永久数据丢失。
    ///
    /// 降级段与待写路径相等、是其祖先、或是其后代，都算不可信：只判一个方向会漏掉「写大表、坏在小格」
    /// 或「坏在大段、写小格」。
    ///
    /// `unparsable` 非空时对任何 `path` 恒为真：救回来的 Value 里，被跳过的键与从未写过的键同形，
    /// 无法判断哪些段受影响。
    pub fn taints(&self, path: &str) -> bool {
        fn is_ancestor(ancestor: &str, descendant: &str) -> bool {
            descendant
                .strip_prefix(ancestor)
                .is_some_and(|r| r.starts_with('.'))
        }
        self.total_fallback
            || !self.unparsable.is_empty()
            || self
                .sections
                .iter()
                .any(|s| s == path || is_ancestor(s, path) || is_ancestor(path, s))
    }

    /// [`Self::taints`] 的带日志版本：不可信时打一行 WARN 并返回 `true`，调用方据此什么都不做。
    /// 统一措辞，可按 `降级闸` 一次 grep 出全部因降级而跳过的写盘。`what` 描述被拦下的动作
    /// （如 `key_actions 物化`）。
    #[must_use]
    pub fn blocks_write_back(&self, path: &str, what: &str) -> bool {
        if !self.taints(path) {
            return false;
        }
        // 语法故障单独措辞：它的修法（去改那一行）与段级降级（去改那个键的类型）
        // 完全不同，共用一句话会把用户支使到错误的地方。
        if let Some(u) = self.unparsable.first() {
            warn!(
                "降级闸：配置文件语法不合法（{}），本次加载的 [{path}] 不可信，已跳过「{what}」；\
                 这不是失败，是拒绝拿残表覆盖你的配置——修好该文件的语法后即自动恢复",
                u.describe()
            );
            return true;
        }
        warn!(
            "降级闸：本次加载的 [{path}] 不可信（降级段 {:?}，整份回落={}），已跳过「{what}」；\
             这不是失败，是拒绝拿出厂残表覆盖你的配置——修好报错的配置键后即自动恢复",
            self.sections, self.total_fallback
        );
        true
    }
}

/// [`parse_toml_lenient`] 的结果。
#[derive(Debug, Clone, Default)]
pub struct LenientParse {
    /// 解析出的值。`None` = 连容错都救不回来。
    pub value: Option<toml::Value>,
    /// 首个语法错误原文；`None` = 文件本来就合法（此时 `skipped_lines` 必为空）。
    pub first_error: Option<String>,
    /// 被跳过的行号（1-based，升序）。
    pub skipped_lines: Vec<usize>,
}

impl LenientParse {
    /// 文件语法是否合法（没动过一行）。
    pub fn is_clean(&self) -> bool {
        self.first_error.is_none()
    }
}

/// 容错解析上限：最多剔除这么多处语法错误。超过基本可断定文件根本不是 TOML（编码错、被覆盖、
/// 下载到半截），继续剔只会啃出面目全非的残骸，比整份作废更危险。
const LENIENT_MAX_SKIPS: usize = 32;

/// 容错解析 TOML：严格解析失败时，逐行剔除报错处，尽量救回其余内容。配置是手写的，写错一行
/// 不该让所有设置一起失效。
///
/// 每轮用 [`toml::de::Error::span`] 定位出错行并置空（不删除）后重试，行号因此稳定，
/// 报给用户的就是编辑器里看到的行号。出错行是段头时丢弃整段：只删 `[ui]` 那一行，它下面的键会被
/// 归进上一个段（`ui.font_size` 变成 `input.font_size`），污染比丢失更危险。
///
/// 救回的值里「被跳过的键」与「从未写过的键」同形，所以 [`ConfigDegradation::taints`] 在
/// `unparsable` 非空时对任何路径都返回真。
pub fn parse_toml_lenient(src: &str) -> LenientParse {
    let first_error = match src.parse::<toml::Value>() {
        Ok(v) => {
            return LenientParse {
                value: Some(v),
                first_error: None,
                skipped_lines: Vec::new(),
            };
        }
        Err(e) => e,
    };
    let first_error_text = first_error.to_string();

    // 逐行工作副本。置空后 join 回去，行数恒定。
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();
    let mut skipped: Vec<usize> = Vec::new();
    let mut err = first_error;

    for _ in 0..LENIENT_MAX_SKIPS {
        let current = lines.join("\n");
        let Some(span) = err.span() else {
            // 没有位置信息就无从下手，整份作废。
            return LenientParse {
                value: None,
                first_error: Some(first_error_text),
                skipped_lines: skipped,
            };
        };
        // span 落在当前文本上；行数恒定 ⇒ 行索引对原文同样成立。
        let Some(head) = current.get(..span.start) else {
            return LenientParse {
                value: None,
                first_error: Some(first_error_text),
                skipped_lines: skipped,
            };
        };
        let idx = head.matches('\n').count();
        if idx >= lines.len() {
            return LenientParse {
                value: None,
                first_error: Some(first_error_text),
                skipped_lines: skipped,
            };
        }

        let killed = if lines[idx].trim_start().starts_with('[') {
            // 段头有毒 ⇒ 整段丢弃（见函数文档）。段的范围 = 本行到下一个段头之前。
            let mut end = idx + 1;
            while end < lines.len() && !lines[end].trim_start().starts_with('[') {
                end += 1;
            }
            (idx..end).collect::<Vec<_>>()
        } else {
            vec![idx]
        };
        for i in &killed {
            // 只记非空行：段内的空行/注释行本来就不承载配置，报给用户只是噪音。
            if !lines[*i].trim().is_empty() {
                skipped.push(*i + 1);
            }
            lines[*i] = String::new();
        }

        let retry = lines.join("\n");
        match retry.parse::<toml::Value>() {
            Ok(v) => {
                skipped.sort_unstable();
                skipped.dedup();
                return LenientParse {
                    value: Some(v),
                    first_error: Some(first_error_text),
                    skipped_lines: skipped,
                };
            }
            Err(e) => err = e,
        }
    }

    skipped.sort_unstable();
    skipped.dedup();
    LenientParse {
        value: None,
        first_error: Some(first_error_text),
        skipped_lines: skipped,
    }
}

/// 完整配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// 本次加载的降级记录，不是配置键：`#[serde(skip)]` 让它不进配置文件，也不进 `config.get` 的产物。
    ///
    /// 随实例而非模块级静态快照：`Config::load` 有多个并发调用方，静态快照会互相覆盖。
    /// `skip` 字段不出现在序列化产物里，故注册表守门、剪枝与 `config.get` 写回都不受影响。
    /// 它不是被禁止的 `skip_serializing_if`（那个用于让配置键退出体系），本字段从来就不是配置键。
    #[serde(skip)]
    pub degradation: ConfigDegradation,
    #[serde(default)]
    pub schema: SchemaConfig,
    #[serde(default)]
    pub input: InputConfig,
    #[serde(default)]
    pub keys: KeysConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub stats: StatsConfig,
    #[serde(default)]
    pub extensions: ExtensionsConfig,
    #[serde(default)]
    pub debug: DebugConfig,
    #[serde(default)]
    pub system: SystemConfig,
    /// 移动端对上面各域的覆盖；桌面构建完全无视。见 [`MobileConfig`]。
    #[serde(default)]
    pub mobile: MobileConfig,
}

// ──────────────── input.default（启动默认状态，原 general 域）────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputDefaultConfig {
    /// 记忆前次状态：true=启动/激活时恢复上次的中英/全半角/标点；false=每次激活重置为下方默认值。
    #[serde(default)]
    pub remember_last_state: bool,
    #[serde(default = "default_true")]
    pub chinese_mode: bool,
    #[serde(default)]
    pub full_width: bool,
    #[serde(default = "default_true")]
    pub chinese_punct: bool,
    /// 中英状态作用域："global"（全局统一，默认）| "app"（按应用独立记忆，会话级）。
    #[serde(default = "default_state_scope")]
    pub state_scope: String,
}

fn default_state_scope() -> String {
    "global".to_string()
}

impl InputDefaultConfig {
    /// 中英状态是否按应用独立记忆（state_scope == "app"）。
    pub fn per_app_scope(&self) -> bool {
        self.state_scope.eq_ignore_ascii_case("app")
    }
}

impl Default for InputDefaultConfig {
    fn default() -> Self {
        Self {
            remember_last_state: false,
            chinese_mode: true,
            full_width: false,
            chinese_punct: true,
            state_scope: default_state_scope(),
        }
    }
}

// ───────────────────────── schema（方案 + 拼音 + 模式）─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SchemaConfig {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub available: Vec<String>,
    #[serde(default)]
    pub primary_codetable: String,
    #[serde(default)]
    pub primary_pinyin: String,
    /// 方案常驻策略（出厂 `false`）。
    ///
    /// - `true`：启动后预热全部可用方案（+ 临拼目标 + 英文）并一直常驻，切换零等待。
    /// - `false`：启动只建用得着的，其余方案只在后台校验 / 重建派生缓存、建完即释放；切过去时现建，
    ///   30 分钟没用的方案引擎自动卸载。
    ///
    /// 一个布尔同时管预热与淘汰：拆开会出现「不预热但永不卸载」这类无意义组合
    /// （`docs/design/memory-footprint.md` §5）。卸载时限与节拍不进配置（R1：程序能定）。
    #[serde(default)]
    pub keep_all_loaded: bool,
    /// 词库缓存策略（出厂 `"lock"`）：词库、反查索引、注释等只读缓存文件映射后怎么处理。
    ///
    /// - `"lock"`：锁定，映射后把页锁在物理内存里，系统不能再回收。
    /// - `"preload"`：预加载，启动时把缓存整份载入内存，之后允许系统回收。
    /// - `"off"`：关闭，不做处理，页按需换入、可被系统回收。
    ///
    /// 只在桌面服务进程生效，修改后需重启服务（已映射的文件不会回头处理）；锁页失败
    /// （系统配额不够）时静默退化为普通映射，不影响输入。取值由注册表的 `Enum` 校验。
    #[serde(default = "default_dict_cache_policy")]
    pub dict_cache_policy: String,
    /// 全局码表配置（所有码表方案公共基线；方案经 schema_overrides 覆盖）。
    #[serde(default)]
    pub codetable: CodetableGlobal,
    /// 全局拼音配置（所有拼音类方案共用：全拼/双拼/混输拼音子方案/临时拼音反查）。
    #[serde(default)]
    pub pinyin: PinyinGlobalConfig,
    /// 全局混输配置（融合策略；全局唯一）。
    #[serde(default)]
    pub mix: MixGlobal,
    /// 全局英文配置（英文方案自身的行为与调频；不再共用码表那套）。
    #[serde(default)]
    pub english: EnglishGlobal,
    /// 快捷输入（日期/计算等内置类方案）配置。将随"英文/快捷做成方案"一并重构。
    #[serde(default)]
    pub quick_input: QuickInputConfig,
    /// 跨引擎的词频公共基线（[schema.frequency]）。
    ///
    /// 与 `schema.{codetable,pinyin,english}.frequency` 不同，别合并：那三段是各引擎自己的调频参数，
    /// 本段是三个引擎都该照办的同一条规则（如「emoji 不参与词频」），配三遍只会漂移。
    #[serde(default)]
    pub frequency: FrequencyGlobal,
    /// 已废弃：特殊模式的实例集合改由带 `[overlay]` 段的已安装方案定义
    /// （`EngineManager::overlay_modes`），见 `docs/redesign/overlay-mode-config.md`。
    ///
    /// 字段保留只为读出残留值以便告警（[`Config::warn_legacy_special_modes`]），删掉的话 serde 会
    /// 静默丢弃这一段。不参与任何行为，也不再写出（`skip_serializing_if`）。
    #[serde(
        rename = "special_modes",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub legacy_special_modes: Vec<toml::Value>,
    /// 临时 mix 模式列表（引导键触发，合并多个成员方案的候选）。
    #[serde(default = "default_mix_modes")]
    pub mix_modes: Vec<MixModeConfig>,
}

impl Default for SchemaConfig {
    fn default() -> Self {
        Self {
            active: String::new(),
            available: Vec::new(),
            primary_codetable: String::new(),
            primary_pinyin: String::new(),
            keep_all_loaded: false,
            dict_cache_policy: default_dict_cache_policy(),
            codetable: CodetableGlobal::default(),
            pinyin: PinyinGlobalConfig::default(),
            mix: MixGlobal::default(),
            english: EnglishGlobal::default(),
            quick_input: QuickInputConfig::default(),
            frequency: FrequencyGlobal::default(),
            legacy_special_modes: Vec::new(),
            mix_modes: default_mix_modes(),
        }
    }
}

/// 跨引擎的词频公共基线（[schema.frequency]）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FrequencyGlobal {
    /// 这些 Unicode 区块的候选不参与词频：既不记录选中，也不受已有记录影响（不重排）。
    /// 取值是字符类的 key：区块名、`"其它"`、`"符号"` 或 `"emoji"`。解析与告警见
    /// `wind_engine::charset_assembly`（装配期打 `no_freq` 标记）。
    ///
    /// 出厂为空。用途如让 emoji 不参与调频：它多是一次性点缀，被顶到前面会把常用字挤下去。
    /// 空列表即关闭，不另设 `enabled`。写端与读端必须同时照办，否则库里既有的记录仍在生效；
    /// 两端共用 `FreqSettings::excluded_from_freq`。
    #[serde(default)]
    pub exclude_blocks: Vec<String>,
}

/// 「首候选是所打原文」的三档（`schema.english.raw_candidate` / `input.temp_english.raw_candidate`，
/// 两个作用域各一份）。
///
/// 是三档而非开关：打 `hel`（词库无此词）时用户不想要原文占首位，打 `hell`（词库有）时又想要它
/// 恒在首位不被调频顶下去，[`Self::InDict`] 按「原文是不是词库词」在两档间逐次切换。
///
/// 判据是字面相同：不用 `is_exact_code`（`he'll` 的 code 也是 `hell`），不忽略大小写（打 `usa`
/// 时词库只有 `USA`，不产原文候选，想要小写仍可回车上屏原码）。口径与精确去重一致，见
/// `docs/design/schema-scoped-behavior.md` 的 `wants_raw_candidate` 一节。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RawCandidateMode {
    /// 恒是首候选（历史 `true`，出厂档）。打词库里没有的词时它是唯一能上屏的东西。
    #[default]
    Always,
    /// 仅当所打原文本身是词库里的词时才作首候选（钉最前、不受调频影响）；
    /// 否则一条都不产，列表就是纯词库候选、调频照常生效。
    InDict,
    /// 不产原文候选（历史 `false`）。想上屏所打原文走回车。
    Off,
}

/// `raw_candidate` 的兼容反序列化：老配置里它是 `bool`。
///
/// `true` → [`RawCandidateMode::Always`]、`false` → [`RawCandidateMode::Off`]。不可改成
/// 「认不出就当 Always」，那会把 `false` 也吞成开启。字符串分支把值重新喂给 derive 生成的实现
/// （同 [`crate::tolerant_de::tolerant`]），写错了回落出厂档并记进 fallback 供设置端 toast。
fn de_raw_candidate<'de, D>(d: D) -> Result<RawCandidateMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::IntoDeserializer;
    use serde::de::value::StrDeserializer;
    let v = toml::Value::deserialize(d)?;
    match v {
        toml::Value::Boolean(true) => Ok(RawCandidateMode::Always),
        toml::Value::Boolean(false) => Ok(RawCandidateMode::Off),
        toml::Value::String(raw) => {
            let sd: StrDeserializer<'_, D::Error> = raw.as_str().into_deserializer();
            match RawCandidateMode::deserialize(sd) {
                Ok(v) => Ok(v),
                Err(_) => {
                    tracing::warn!(
                        "配置值 \"{raw}\" 不在 raw_candidate 的取值范围内（always / in_dict / off，\
                         或老写法 true / false），本项回落出厂默认值"
                    );
                    crate::tolerant_de::record_fallback(&raw);
                    Ok(RawCandidateMode::default())
                }
            }
        }
        // 其余类型（数字、数组…）交给段级降级，与 `tolerant` 的分工一致：
        // 值域层只回答「这个值在不在值域里」，不吞类型错误。
        other => Err(serde::de::Error::custom(format!(
            "raw_candidate 应为 always / in_dict / off（或老写法 true / false），实际是 {}",
            other.type_str()
        ))),
    }
}

/// 全局英文配置（[schema.english]）。英文是可切换方案，行为不挂靠码表段，免得纯码表用户与
/// 英文用户互相牵连。
///
/// 不 derive `Default`：本段有非零默认值（如 `case_follow_input`、`raw_candidate`），而 serde 的
/// `default = ...` 管不着 `Config::default()`，两条路会分叉（`config-design-rules` §R4）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnglishGlobal {
    #[serde(default)]
    pub frequency: EnglishFrequency,
    /// 英文方案下上屏一个词后再补一个空格。
    ///
    /// 生效范围（消费点见 `english_appends_space` / `english_space_enabled`）：
    /// - 所有选中方式：空格 / 数字键 / 次三选键 / 修饰键选词 / 鼠标点选；
    /// - 空格上屏原码（打了词库里没有的词）；
    /// - 不含回车上屏原码（终结性动作）、标点键顶屏（会得到 `hello ,`）、顶码。
    #[serde(default)]
    pub commit_space: bool,
    /// 首候选是用户所打原文。三档，见 [`RawCandidateMode`]；出厂 [`RawCandidateMode::Always`]
    /// （＝老配置的 `true`）。
    ///
    /// 英文输入串本身就是合法上屏内容，调频把某个词顶到首位后，想上屏原文只剩回车（终结性动作）。
    /// 码表的 `aaaa` 不是可上屏文本，所以不下放给码表。与 `input.temp_english.raw_candidate` 是两个
    /// 作用域各一份。
    #[serde(default, deserialize_with = "de_raw_candidate")]
    pub raw_candidate: RawCandidateMode,
    /// 生成大小写变形候选（全小写 / 首字母大写 / 全大写）。默认关。
    ///
    /// 与临英那份（`input.temp_english.case_variants`，默认开）刻意相反：临英里人名、专有名词首字母
    /// 大写是刚需；英文方案长时打英文，每条变形吃一个候选位。合并成一个键必然改掉其中一侧。
    #[serde(default)]
    pub case_variants: bool,
    /// 词库候选跟随输入的大小写形态（逐位投影）。默认开。
    ///
    /// 打 `Hi` ⇒ `hill` 显示为 `Hill`；打 `WoW` ⇒ `wowed` 显示为 `WoWed`。单向：只投影大写，
    /// 不压掉词库自带的大写（`China` / `iPhone`）。与 [`Self::case_variants`] 不重叠：本项改的是
    /// 词库候选自身的形态，不新增条目。
    #[serde(default = "default_true")]
    pub case_follow_input: bool,
    /// 英文词组分词输入：用 `'` 切开各段，每段只打前缀。默认关。
    ///
    /// `bue'air` ⇒ `Buenos Aires`，`ip'max` ⇒ `iPhone 15 Pro Max`；允许跳词。
    ///
    /// 默认关：开启后 `'` 在英文方案下不再是第三候选键，这种可察觉的行为变化不该由升级静默施加。
    /// 分词符固定为 `'`：含撇号的词（`let's`）本就靠前缀召回、`'` 进不了缓冲，且 `convert` 是合并
    /// 而非劫持。备选是 `.`，将来要可配时与它一起定。
    #[serde(default)]
    pub phrase_seg: bool,
    /// 英文自动造词（[schema.english.auto_learn]）。见 [`EnglishAutoLearn`]。
    #[serde(default)]
    pub auto_learn: EnglishAutoLearn,
}

impl Default for EnglishGlobal {
    fn default() -> Self {
        Self {
            frequency: EnglishFrequency::default(),
            commit_space: false,
            raw_candidate: RawCandidateMode::Always,
            case_variants: false,
            case_follow_input: true,
            phrase_seg: false,
            auto_learn: EnglishAutoLearn::default(),
        }
    }
}

/// 英文自动造词（[schema.english.auto_learn]）。
///
/// 一个开关管英文方案与临时英文两处：两者共用 schema id `english` 与同一套用户 / 临时词库。
/// 快捷输入里的英文不在此列，那里的原文进快捷输入历史。
///
/// 造词信号是上屏了词库里没有的原文（选中原文候选、回车 / 空格上屏原码、标点顶屏原文）。
/// 词走两跳，与码表 / 拼音同构：先进临时词库（有上限、会淘汰），累计使用 `promote_count`
/// 次后晋升用户词库。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnglishAutoLearn {
    /// 总开关，出厂关。
    #[serde(default)]
    pub enabled: bool,
    /// 临时词晋升进用户词库所需使用次数（含造词那一次）。0 = 不晋升，一直留在临时词库。
    /// L1 与 L2 同值（3），见 [`EnglishGlobal`] 文档里的 §R4。
    #[serde(default = "default_english_promote_count")]
    pub promote_count: usize,
    /// 英文临时词库条目上限（0 = 不限）。超出后按「用得最少、造得最早」淘汰。内部字段，
    /// 设置页不开放。与码表那份分开：英文与码表的临时词各在自己的 schema 分区里淘汰。
    #[serde(default = "default_english_temp_max_entries")]
    pub temp_max_entries: usize,
}

fn default_english_promote_count() -> usize {
    3
}

fn default_english_temp_max_entries() -> usize {
    5000
}

impl Default for EnglishAutoLearn {
    fn default() -> Self {
        Self {
            enabled: false,
            promote_count: default_english_promote_count(),
            temp_max_entries: default_english_temp_max_entries(),
        }
    }
}

/// 英文调频（[schema.english.frequency]）。不 derive `Eq`：`half_life` 是 f64。
///
/// 没有 `protect_top_n*`：那是简码位首选保护，英文没有简码位，照搬只会锁死前几位不让调频。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnglishFrequency {
    #[serde(default)]
    pub enabled: bool,
    /// 调频关闭时仍记录选词次数；语义同 [`CodetableFrequency::record_always`]。
    #[serde(default)]
    pub record_always: bool,
    /// `"top"` / `"step"` / `"position"`，默认 `"position"`（与码表默认不同）：英文候选几乎全是前缀
    /// 匹配，`top`/`step` 误选一次就把词顶到显眼位置且不衰减；`position` 每次只前移一半、久不用会回落。
    #[serde(default = "default_english_freq_strategy")]
    pub strategy: String,
    /// 前缀补全候选参与位置提升的范围；仅 `strategy = "position"` 时生效。
    /// 英文默认 `"all"`：候选本来就几乎全是前缀补全，收窄到 `single` 等于把调频关掉大半。
    #[serde(default = "default_codetable_promote_prefix")]
    pub promote_prefix: String,
    /// 衰减半衰期（小时），`0` = 内置默认 72 小时；仅 `position` 策略生效。
    #[serde(default)]
    pub half_life: f64,
    /// 词频记账码口径（`"candidate"` / `"input"`，默认 `"candidate"`）。
    ///
    /// | 取值 | 打 `hel` 选 `hello` 记成 | 之后打 `he` |
    /// |---|---|---|
    /// | `"candidate"`（默认） | `(hello, hello)` | 也受益（跨码位共享） |
    /// | `"input"` | `(hel, hello)` | 不受益（码位独立） |
    ///
    /// 按候选来源生效，不按当前方案：混输里的英文候选同样读它，故 `EngineManager::freq_settings`
    /// 的每个分支都要从这里取值。
    #[serde(default = "default_english_code_scope")]
    pub code_scope: String,
}

fn default_english_freq_strategy() -> String {
    "position".to_string()
}

impl Default for EnglishFrequency {
    fn default() -> Self {
        Self {
            enabled: false,
            record_always: false,
            strategy: default_english_freq_strategy(),
            promote_prefix: default_codetable_promote_prefix(),
            half_life: 0.0,
            code_scope: default_english_code_scope(),
        }
    }
}

/// `input.alt_commit` 的值域：Alt+数字 / Alt+空格 上屏候选的什么。
///
/// 一个键兼作开关与内容选择（而不是 bool + 内容两键）：关着时内容无意义，两键组合里有一半
/// 格子是死的；设置页也只需一个下拉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AltCommit {
    /// 关（出厂）：Alt 组合一个都不登记，原样归宿主。
    #[default]
    Off,
    /// 带调拼音，音节以空格分隔（`nǐ hǎo`）。与注释变量 `${pinyin}` 同一算法。
    Pinyin,
    /// 不带调拼音（`ni hao`；ü 保留为 `ü`）。
    PinyinPlain,
    /// 候选注释段的渲染结果（当前生效的注释模板，不做显示截断）。
    Comment,
}

impl AltCommit {
    /// 认不出的值按 `Off` 处理——这是一个会抢宿主快捷键的功能，写错不该让它意外开启。
    ///
    /// match 臂必须与 `config_schema::ALT_COMMIT_VALUES` 逐项对齐。
    pub fn from_config(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "pinyin" => Self::Pinyin,
            "pinyin_plain" => Self::PinyinPlain,
            "comment" => Self::Comment,
            _ => Self::Off,
        }
    }

    /// 生效档：`comment` 档而候选注释总开关（`ui.comment.enabled`）关着时按
    /// `Off` 处理——注释恒空，认领 Alt 组合只会把宿主的加速键吞掉。热键登记与协调器分派
    /// 都取这一份，两边口径一致（TSF 吃键集 ⊆ Rust 出字集）。
    pub fn effective(cfg: &Config) -> Self {
        match Self::from_config(&cfg.input.alt_commit) {
            Self::Comment if !cfg.ui.comment.enabled => Self::Off,
            kind => kind,
        }
    }
}

/// 全局拼音配置（[schema.pinyin]）。所有拼音类方案（全拼/双拼/混输拼音子方案）共用，
/// 无方案级 override。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinyinGlobalConfig {
    #[serde(default = "default_true")]
    pub use_smart_compose: bool,
    /// 让用户上屏过的词（用户词 + 临时词）参与整句解码。出厂 true。
    ///
    /// 关闭时整句词图只从系统词库建：自造词单独打得出，在整句里却被打散，调过权重的词也不认。
    /// 已知局限：出厂只露一条整句（赢者通吃）、会改变所有老用户的整句结果（导入词也进图）、
    /// 用户词 weight 只做了上限截断。取舍是「用户造过的词该在整句里认得出」优先。
    ///
    /// 设置页里本项 `enabled_when` 挂在 `use_smart_compose`：智能组词关闭时整句建图不会跑，
    /// 本项无效，UI 须同步置灰。只接用户词与临时词，不接草稿层（「用过即转正」是草稿的质量闸）；
    /// 但临时层也收自动造词写入的分步拼接与合成整句（见 `pinyin/lattice.rs::add_store_nodes` 约束 1，
    /// 简拼段见 `add_store_abbrev_nodes`）。默认值须与 `wind_engine::pinyin::Config::default()` 同值。
    #[serde(default = "default_true")]
    pub sentence_uses_user_words: bool,
    /// 整句 N-best：候选列表里露几条整句。出厂 1 = 单条整句。>1 时整句块连续摆在候选最前，
    /// 便于看出用户词输给了什么。默认值须与 `wind_engine::pinyin::Config::default()` 同值。
    #[serde(default = "default_sentence_count")]
    pub sentence_count: u8,
    /// 整句 N-best：解码时算几条（切换键的池子）。不足 `sentence_count` 时按后者抬，
    /// 上限 8（引擎 `SENTENCE_COUNT_LIMIT`）。
    #[serde(default = "default_sentence_count")]
    pub sentence_max_count: u8,
    /// 整句切换键：在整句池（`sentence_max_count` 条）里滚动显示窗口。空串 = 关闭（出厂）。
    ///
    /// 键名取值同 `keys.session_actions`（`wind_keys::keymap::session_key_name_to_vk`，可带 `shift+`），
    /// 不认识的键名告警后按关闭处理。键即开关，与 `input.english_case_cycle_key` 同形，不另设 `enabled`。
    ///
    /// 只在整句池 ≥ 2 条时夺取，其余时候保持原语义（Tab 出厂还是高亮下移、配对跳出、加词切来源键）；
    /// 它若是音节分隔符或被方案 `[key_actions]` 绑定则一律让位。出厂 `sentence_max_count = 1` 时恒不吃。
    /// 撞车时启动体检告警（`Coordinator::warn_sentence_cycle_conflict`）。
    #[serde(default)]
    pub sentence_cycle_key: String,
    /// 拼音分隔策略（"auto" 等）。原 input.pinyin_separator 收拢至此。
    #[serde(default = "default_pinyin_separator")]
    pub separator: String,
    #[serde(default)]
    pub fuzzy: PinyinFuzzy,
    /// 英文候选混入（[schema.pinyin.english_merge]）。见 [`PinyinEnglishMerge`]。
    ///
    /// 与码表那份（[`CodetableEnglishMerge`]）是两件独立的事，不共享取值。
    #[serde(default)]
    pub english_merge: PinyinEnglishMerge,
    /// 单字输入：开启后候选只出单字。出厂关。
    ///
    /// 与码表那份（[`CodetableGlobal::single_char`]）不共享取值：码表开单字是为定长盲打（满码唯一即
    /// 上屏），拼音开了只是「不出词」，分开才能表达「五笔只出单字、拼音照常出词」。拼音没有方案级覆盖，
    /// 要加先读 [`CodeTableSpec::single_char`] 的文档。运行时热键切换不写回本项
    /// （临时态，见 `State::single_char_override`）。
    #[serde(default)]
    pub single_char: bool,
    /// 拼音调频（衰减参数；全局唯一，按引擎分——见 docs/redesign/schema-config-layering.md §3.4）。
    #[serde(default)]
    pub frequency: PinyinFrequency,
    /// 拼音自动造词（全局唯一）。
    #[serde(default)]
    pub auto_learn: AutoLearnConfig,
    /// 词组补全的音节数约束（全局唯一）。
    #[serde(default)]
    pub completion: PinyinCompletion,
    /// 简拼（只打声母）的开关（`[schema.pinyin.abbrev]`）。
    #[serde(default)]
    pub abbrev: PinyinAbbrev,
    /// 双拼相关的全局行为（`[schema.pinyin.shuangpin]`）。
    #[serde(default)]
    pub shuangpin: PinyinShuangpin,
    /// 辅助码字形二次筛选（`[schema.pinyin.aux_code]`）。出厂关闭。
    #[serde(default)]
    pub aux_code: AuxCodeGlobal,
    /// 上下文语言模型（n-gram 语法模型）。
    #[serde(default)]
    pub grammar: PinyinGrammar,
}

/// 辅助码的全局基线（`[schema.pinyin.aux_code]`）：拼音候选的字形二次筛选。
///
/// 与方案段 `[engine.aux_code]`（[`crate::schema::AuxCodeSpec`]）的分工同码表那套
/// （schema-config-layering.md §4）：那里放 `files`（方案属性），本段放行为基线，方案可用 tri-state
/// `Option` 逐字段覆盖。全部字段出厂即类型默认值（`false` / `0`），故 `derive(Default)`：
/// 出厂值必须是「什么都不发生」。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AuxCodeGlobal {
    /// 总开关，出厂 `false`。辅助码会改变已有按键的语义（双拼下反引号从标点变成进入筛选），并把
    /// 数十万条码表读进内存，不该在用户没表态时发生。方案配了 `files` 只表示推荐，不构成开启意图，
    /// 判据分离在 [`Self::resolved`]。
    #[serde(default)]
    pub enabled: bool,
    /// 词组长度上限：字数 > 此值的词组一律排除、不参与筛选（0 = 不限）。
    /// 单字恒参与匹配，不受此限。
    #[serde(default)]
    pub max_phrase_len: usize,
    /// 直接辅助码（仅双拼）：不按引导键，任一音节后直接敲 1～2 位形码当该字的辅码，长句中间也可以
    /// （`gokqk` → 国情；`docs/design/aux-code-continuous.md`）。串尾末字 1 位照旧提前
    /// （`docs/design/aux-code-direct.md`）。出厂 `false`。
    ///
    /// 只在 [`Self::enabled`] 也开时起作用。它改变了字母的语义，故出厂关。全拼方案写了也不生效
    /// （`lim` 既可能是 li+m 也可能是简拼），见 `EngineManager::aux_code_settings_of`。
    #[serde(default)]
    pub direct: bool,
}

impl AuxCodeGlobal {
    /// 折叠方案 `[engine.aux_code]` 的内联/override 到全局基线：`Some` 覆盖、`None` 回落。
    ///
    /// 与 [`CodetableGlobal::resolved`] 同构——方案内联与 `schema_overrides` 已在
    /// `read_schema` 经 `merge_toml` 合并成单个 `AuxCodeSpec`，此处只做一次折叠。
    pub fn resolved(&self, ov: Option<&crate::schema::AuxCodeSpec>) -> AuxCodeGlobal {
        let mut out = self.clone();
        let Some(o) = ov else {
            return out;
        };
        if let Some(v) = o.enabled {
            out.enabled = v;
        }
        if let Some(v) = o.max_phrase_len {
            out.max_phrase_len = v;
        }
        if let Some(v) = o.direct {
            out.direct = v;
        }
        out
    }
}

/// 上下文语言模型（`[schema.pinyin.grammar]`）。
///
/// 整句解码原本只有 unigram（词频），分不出「是想 / 思想」这类需要上下文才能定夺的解读。
/// 本段挂上一个 n-gram 搭配模型给词与词之间的转移打分。
/// 设计与实测见 `docs/design/language-model-integration.md`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinyinGrammar {
    /// 总开关，设置页的那个勾。结构体缺省 true 且只做否决：真正生效仍要 `weight` 与 `model` 都配了
    /// （见 [`PinyinGrammar::active`]）。桌面出厂写成 false、同时备好 weight / model，设置页一勾即用。
    /// 分层合并后 L2 的 false 会盖住用户层没写的这一项，手写过 weight + model 的老配置靠
    /// [`Config::migrate_grammar_enabled_value`] 补 true。
    #[serde(default = "default_grammar_enabled")]
    pub enabled: bool,
    /// 影响力权重。0 = 关闭：不加载模型文件、不占内存，整句结果与没有这个功能时逐位相同。
    /// 默认关：接上模型会大幅重排整句结果，相关常数需整套重新标定（见 `language-model-integration.md`）。
    #[serde(default = "default_grammar_weight")]
    pub weight: f64,
    /// 模型文件名，相对 `data/schemas/pinyin/grammar/`。默认空串 = 不启用。
    ///
    /// 格式是 librime-octagram 的 `.gram`（darts-clone double-array）。随包带 `wanxiang-mini.gram`
    /// （设计文档 §11）：桌面默认不启用，移动端经 [`MobileGrammarConfig`] 默认启用。
    ///
    /// 默认必须是空串：填任何文件名，用户只把 `weight` 调成非 0 就会静默启用那个模型，而实测过的
    /// 模型里有质量为负的（评测见 `docs/design/language-model-integration.md`）。必须同时显式给出
    /// weight 与 model 才生效。
    #[serde(default)]
    pub model: String,
}

fn default_grammar_weight() -> f64 {
    0.0
}

fn default_grammar_enabled() -> bool {
    true
}

impl PinyinGrammar {
    /// 引擎该不该加载模型：开关开着，且 `weight` 与 `model` 都显式配了。
    pub fn active(&self) -> bool {
        self.enabled && self.weight != 0.0 && !self.model.trim().is_empty()
    }
}

impl Default for PinyinGrammar {
    fn default() -> Self {
        Self {
            enabled: default_grammar_enabled(),
            weight: default_grammar_weight(),
            model: String::new(),
        }
    }
}

/// 双拼相关的全局行为（`[schema.pinyin.shuangpin]`）。
///
/// 方案级 `engine.pinyin.shuangpin` 放 `layout`（编码规则，属方案）；本段放「这台机器怎么用双拼」，
/// 对所有双拼方案生效。代价是不能按方案区分（`PinyinGlobalConfig` 无方案级 override），真有此需求
/// 时应整体重估这一段的归属。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinyinShuangpin {
    /// 双拼方案下是否额外把击键串当全拼解释一遍（`nihao` → 「你好」）。默认 `false`。
    ///
    /// 服务多人共用一台机器。只有低置信部分（前缀补全 / 子短语）沉底，精确整词与整句和双拼候选同层竞争，
    /// 见 [`wind_candidate::Candidate::is_fullpinyin_fallback`]。非双拼方案下无效；混输的拼音次引擎
    /// 强制关闭，理由同 `PinyinConfig::enable_partial_final`。
    #[serde(default)]
    pub allow_full_pinyin: bool,
}

/// 词组补全（前缀补全）的音节数约束（`[schema.pinyin.completion]`）。
///
/// 约束的是「码比输入长」的补全词，即引擎在预测用户尚未输入的音节；精确匹配、子短语、整句、
/// 简拼不受影响。`started` = 输入的完整音节数 + (有尾部残码 ? 1 : 0)，允许的候选音节数上限 =
/// `started < min_syllables ? started : started + max_extra_syllables`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinyinCompletion {
    /// 至少输入几个音节才给出词组候选。不足时只出同音节数的候选：`d` 与 `dian` 只出单字，
    /// 不混进「但是」「电话」。取 1 = 不设限。尾部残码算一个音节：`dianh` 已算 2 个，照常出「电话」。
    #[serde(default = "default_completion_min_syllables")]
    pub min_syllables: u32,
    /// 词组最多比输入多几个音节。0 = 只给音节数相等的词；1 = 只补下一个音节（`nih` → 你好/你会）；
    /// 4 以上才够 `zhonghuar` → 「中华人民共和国」。数值越大，引擎越敢预测还没打的内容。
    #[serde(default = "default_completion_max_extra_syllables")]
    pub max_extra_syllables: u32,
}

/// 取 4，与 librime `UserDictionary::kNumSyllablesToPredictWord` 及 fcitx5-chinese-addons
/// `LongWordLengthLimit` 的默认值一致：输入不足 4 个音节时不预测用户没打的内容。
/// 在召回层直接不给，比靠档位排序压到后面体感更好。
fn default_completion_min_syllables() -> u32 {
    4
}

/// 取 5：配合 `min_syllables = 4`，`started = 4` 时上限 9 音节，够召回「冰冻三尺非一日之寒」。
/// 两个旋钮必须配合改：`min = 4` 配 3 会把上限压到 7，9 音节的长词永远召回不到。
fn default_completion_max_extra_syllables() -> u32 {
    5
}

impl Default for PinyinCompletion {
    /// 这里是真实默认值（与 `data/config.toml` 一致），不同于 [`CodetableGlobal`] 的结构体零值：
    /// `max_extra_syllables = 0` 是合法且严格的取值，不能当「未配置」哨兵。
    fn default() -> Self {
        Self {
            min_syllables: default_completion_min_syllables(),
            max_extra_syllables: default_completion_max_extra_syllables(),
        }
    }
}

/// 简拼（只打声母，`nh` → 你好）的开关（`[schema.pinyin.abbrev]`）。管纯拼音方案（全拼 / 双拼）；
/// 混输里的拼音另有 `schema.mix.enable_pinyin_abbrev`，两者取与。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinyinAbbrev {
    /// 是否给简拼候选。关掉后纯简拼（`nh`）、混合简拼（`nhao`）、简拼整句
    /// （`bzdhaobuhao`）一并不出。尾部没打完的音节照常补全（全拼 `nihaom` → 你好吗、
    /// 搜狗双拼 `hkl` → 好了），那是前缀补全，不归它管。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for PinyinAbbrev {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for PinyinGlobalConfig {
    fn default() -> Self {
        Self {
            use_smart_compose: true,
            // 出厂开，与引擎侧 `pinyin::Config::default()` 同值；UI 侧靠 enabled_when 挂在
            // use_smart_compose 上，防止关了智能组词还显得开着。
            sentence_uses_user_words: true,
            sentence_count: default_sentence_count(),
            sentence_max_count: default_sentence_count(),
            sentence_cycle_key: String::new(),
            english_merge: PinyinEnglishMerge::default(),
            separator: default_pinyin_separator(),
            fuzzy: PinyinFuzzy::default(),
            single_char: false,
            frequency: PinyinFrequency::default(),
            auto_learn: AutoLearnConfig::default(),
            completion: PinyinCompletion::default(),
            abbrev: PinyinAbbrev::default(),
            shuangpin: PinyinShuangpin::default(),
            aux_code: AuxCodeGlobal::default(),
            grammar: PinyinGrammar::default(),
        }
    }
}

/// z 键功能（`schema.codetable.z_key_action` 的解析形态），也是 `key_actions` 的动词值域。
///
/// 方案级、且只管 z：字母能否借作引导键取决于它在这张码表里是不是死码，全局 `trigger_keys`
/// 配字母就是在所有方案里无条件抢键，故字母引导键已移除（见 `Coordinator::special_trigger_vk`）。
/// 只管 z 是因为本项与 `z_key_repeat` 是同一个键的两个身份，换成别的字母会让 repeat 状态去挡无关的键。
///
/// 值域：
/// - `""` / `"none"`：z 是普通编码字母（默认）
/// - `"temp_pinyin"`：进临时拼音
/// - `"temp_english"`：进临时英文
/// - `"rare_char"`：进生僻字模式（当前方案的编码，候选只留生僻字）
/// - `"reverse"`：进反查模式（当前方案的编码，通配可在首位）
/// - `"mix:<id>"`：进指定融合模式（`mix:quick_mix` = 内置「快捷」）
/// - `"special:<id>"`：进指定特殊模式
/// - `"toggle_schema:<id>"`：切到指定方案，再按回来
/// - `"switch_schema:<id>"`：切到指定方案，单向（见该变体说明）
/// - `"command:<cmdbar 表达式>"`：执行一段命令（同工具栏自定义按钮），见 [`BoundAction::Command`]
///
/// 未知值一律解析成 [`BoundAction::None`]（不静默变成别的功能）；指向不存在的 id 由消费端
/// 门卫拦下（`mix_members` / `ensure_schema`），并在加载期 `warn`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundAction {
    /// 不启用：z 作正常编码字母。
    None,
    /// 进临时拼音。
    TempPinyin,
    /// 进临时英文。
    TempEnglish,
    /// 进辅助码模式（拼音候选字形二次筛选；仅组码中有效）。
    AuxCode,
    /// 进生僻字模式：用当前活跃方案的编码输入，候选只留生僻字。
    ///
    /// 无载荷——它是单例，不像 [`Self::Special`] 那样一个引导键对应一份码表方案。
    RareChar,
    /// 进反查模式：输入本方案编码（含通配，任何位置都可通配），只查主码表
    /// （reverse-mode spec §3）。无载荷，单例。
    Reverse,
    /// 进指定融合模式（携带实例 id）。
    Mix(String),
    /// 进指定特殊模式（携带实例 id）。
    Special(String),
    /// 切换单字输入（开 / 关 / 切换 / 回落配置）。载荷见 [`SingleCharAction`]。
    /// 切的是内存临时态，不写配置；切方案即失效（见 `Coordinator::sync_schema_scope`）。
    SingleChar(SingleCharAction),
    /// 切候选注释的详细 / 默认态（持久，写回 `ui.comment.state`）。载荷见 [`CommentStateAction`]。
    ///
    /// 本表没有「当前候选窗」可言，故不接受 `session` 档（见 [`CommentStateAction::Session`]）。
    CommentState(CommentStateAction),
    /// 切到指定方案，再按回到来源（携带目标方案 id）。
    ///
    /// 往返语义由运行时来源兜底，不要求目标方案配对称的绑定。
    /// 见 docs/design/schema-key-actions.md §5。
    ToggleSchema(String),
    /// 切到指定方案，单向（携带目标方案 id）。原 `keys.schema_hotkeys` 的语义。
    ///
    /// 全局 `keys.key_actions` 与方案级 `[key_actions]` 都合法，作用域不同：
    /// - 全局：在所有方案下都命中，目标方案里再按走幂等分支（`restore_state_for_same_schema`）。
    /// - 方案级：只在源方案里命中；切走后这把键在目标方案里被吞掉、不动作
    ///   （`Coordinator::schema_switch_arrival` + `handle_bound_modifier_key_up` 的 `NotBound` 分支）。
    ///
    /// 方案级单向没有回程，回程交给另一把键。那条吞键兜底必须保留：少了它键会落回全局链，
    /// 而 `lshift`/`rshift` 出厂是 `toggle_mode`，就成了「配的是切方案却切了中英文」。
    SwitchSchema(String),
    /// 开关软键盘；`Some(id)` 直接切到指定面（直通车），`None` 保持/切上次那面。
    ///
    /// 不进 [`Self::DISPATCH_ACTIONS`]（同 `add_word`）：开启后要接管后续按键，得返回占位 composition
    /// 激活宿主转发，不符 `dispatch_hotkey` 的 `bool` 契约，混进去就是「按了没反应」。
    SoftKeyboard(Option<String>),
    /// A 类状态切换（`toggle_punct` / `take_screenshot` 那类）。携带动词原文，由协调器转交
    /// `dispatch_hotkey`（这批动作的既有单点）。值域见 [`Self::DISPATCH_ACTIONS`]。
    Action(String),
    /// 执行一段 cmdbar 表达式（`command:<表达式>`，如 `command:proc.run("charmap.exe")`）。
    ///
    /// 执行通路与工具栏自定义按钮同一条（`wrap_command_source` + `spawn_command`），裸表达式与
    /// `$CC(...)` 都收。载荷只 trim 首尾、保留大小写（路径与网址区分大小写）；表达式为空解析成
    /// [`Self::None`]。与中英态无关、不限修饰键。目标函数经独立线程执行（`run_command_candidate`
    /// 要求未持 `State` 锁）。
    Command(String),
}

/// `command:<表达式>` 的前缀。两张表（[`BoundAction`] / [`SessionAction`]）共用。
const COMMAND_VERB_PREFIX: &str = "command:";

/// 解析 `command:<表达式>`：不是这个动词返回 `None`；是但表达式为空返回 `Some(None)`。
/// 两张表共用：`SessionAction::parse` 会整串小写化，command 必须在那之前剥出来。
fn parse_command_verb(s: &str) -> Option<Option<String>> {
    let s = s.trim();
    let head = s.get(..COMMAND_VERB_PREFIX.len())?;
    if !head.eq_ignore_ascii_case(COMMAND_VERB_PREFIX) {
        return None;
    }
    let expr = s[COMMAND_VERB_PREFIX.len()..].trim();
    Some((!expr.is_empty()).then(|| expr.to_string()))
}

impl BoundAction {
    /// A 类可绑的状态切换动词，与 `Coordinator::dispatch_hotkey` 的分支一一对应。
    ///
    /// 白名单：写错的动词若静默通过，表现与「没绑上」同形（同 `is_supported_key_action`）。
    /// `add_word` / `open_add_word_dialog` 不在此列：它们要返回占位 composition 激活 C++ 转发，
    /// 在按键路径上单独特判（`coordinator.rs` 的热键分支）。
    pub const DISPATCH_ACTIONS: &'static [&'static str] = &[
        "toggle_mode",
        "switch_engine",
        "toggle_full_width",
        "toggle_punct",
        "toggle_s2t",
        "toggle_t2s",
        "toggle_toolbar",
        "open_settings",
        "open_dictionary",
        "take_screenshot",
    ];

    /// 该动作是否只能绑无字符键（修饰键）。
    ///
    /// 判据是「它是不是从英文态出来的手段」：有字符的键走 keydown 链，在英文模式分水岭之后，
    /// 绑在上面就切得过去、切不回来。
    ///
    /// | 动作 | 限修饰键 | why |
    /// |---|---|---|
    /// | `toggle_mode` / `switch_engine` / `toggle_schema:*` / `switch_schema:*` | 是 | 正是用来离开/返回英文态的 |
    /// | `toggle_punct` / `toggle_s2t` / `toggle_t2s` / `take_screenshot` … | 否 | 本就只在中文态有意义 |
    ///
    /// 与「键有没有字符」是正交的两问，见 docs/design/schema-key-actions.md §4.1。
    pub fn requires_modifier_key(&self) -> bool {
        match self {
            Self::ToggleSchema(_) | Self::SwitchSchema(_) => true,
            Self::Action(a) => matches!(a.as_str(), "toggle_mode" | "switch_engine"),
            _ => false,
        }
    }

    /// 该动作是否只在中文态有意义，即组合键的策略位该不该带 `HOTKEY_POLICY_CHINESE_ONLY`
    /// （`hotkey.rs` 的 `hotkey_policy_for`，对全部动词生效）。
    ///
    /// 分派端（`dispatch_bound_action_hotkey`）借它判 B 类（建 overlay 的）的进入条件，兜住从别的路径
    /// 转发进来的同一个键。A 类（`BoundAction::Action(_)`）在守卫之前就被 `is_lock_free_bound` 分流走，
    /// 这是刻意的：A 类只改状态位、不建 overlay，「英文态先切好繁简档」是合理用法；需要内检的动词
    /// 各自负责（如 `toggle_punct` 的 `effective_chinese`）。
    ///
    /// 离开英文态的手段（`toggle_mode` / `switch_engine` / `toggle_schema:` / `switch_schema:`）一律
    /// `false`；`softkeyboard` 也是 `false`，面板与中英态无关（C++ 侧有软键盘总闸 `IsSoftKeyboard()`）。
    pub fn only_in_chinese_mode(&self) -> bool {
        match self {
            // 进 overlay 的一族：中文输入中途才有意义。
            Self::TempPinyin
            | Self::TempEnglish
            | Self::AuxCode
            | Self::RareChar
            | Self::Reverse
            | Self::Mix(_)
            | Self::Special(_) => true,
            // 字词范围切的是「这一码出什么」，没有中文候选就无从谈起。
            Self::SingleChar(_) => true,
            // 注释态是持久设置，与中英态无关。
            Self::CommentState(_) => false,
            // 命令不改输入状态，英文态下照样该按得动。
            Self::None
            | Self::SoftKeyboard(_)
            | Self::ToggleSchema(_)
            | Self::SwitchSchema(_)
            | Self::Command(_) => false,
            // A 类：只有这三个是「中文态才有意义」，其余（含 `open_dictionary`，打开的是独立窗口）
            // 两模式都吃，分组与 `hotkey.rs` 固定字段那两段一致。载荷是 `String` 编译期穷举不了，
            // 由 `chinese_only_covers_every_dispatch_action` 守门。
            Self::Action(a) => matches!(a.as_str(), "toggle_punct" | "toggle_s2t" | "toggle_t2s"),
        }
    }

    /// 解析配置字符串。大小写与首尾空白不敏感；未知值 → [`Self::None`]。
    ///
    /// 例外是 `command:` 的表达式：原样保留大小写与内部空白，见 [`Self::Command`]。
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if let Some(expr) = parse_command_verb(s) {
            return expr.map_or(Self::None, Self::Command);
        }
        if let Some(id) = s.strip_prefix("mix:") {
            let id = id.trim();
            return if id.is_empty() {
                Self::None
            } else {
                Self::Mix(id.to_string())
            };
        }
        if let Some(id) = s.strip_prefix("special:") {
            let id = id.trim();
            return if id.is_empty() {
                Self::None
            } else {
                Self::Special(id.to_string())
            };
        }
        if let Some(id) = s.strip_prefix("toggle_schema:") {
            let id = id.trim();
            return if id.is_empty() {
                Self::None
            } else {
                Self::ToggleSchema(id.to_string())
            };
        }
        if let Some(id) = s.strip_prefix("switch_schema:") {
            let id = id.trim();
            return if id.is_empty() {
                Self::None
            } else {
                Self::SwitchSchema(id.to_string())
            };
        }
        // 直通车 `softkeyboard:<面 id>`。带冒号的形态先判，不带冒号的落到下面的 `lower`
        // 分支——两者不构成子集关系（前缀判据要求冒号存在），故顺序不影响结果。
        if let Some(id) = s.strip_prefix("softkeyboard:") {
            let id = id.trim();
            return if id.is_empty() {
                Self::None
            } else {
                Self::SoftKeyboard(Some(id.to_string()))
            };
        }
        // 单字输入 `single_char[:<on|off|toggle|follow>]`，裸形式 = toggle。载荷写错落 `None`
        // 而不是回落某个状态：拼错表现成「切了但方向不对」比「这个键没绑上」更难倒推。
        if s.trim().eq_ignore_ascii_case("single_char") {
            return Self::SingleChar(SingleCharAction::Toggle);
        }
        if let Some(rest) = s.strip_prefix("single_char:") {
            return match SingleCharAction::parse_payload(rest) {
                Some(a) => Self::SingleChar(a),
                None => Self::None,
            };
        }
        // 注释态 `comment_state[:<default|detail>]`；`session` 档只属于会话表，这里判无效。
        if s.eq_ignore_ascii_case("comment_state") {
            return Self::CommentState(CommentStateAction::Toggle);
        }
        if let Some(rest) = s.strip_prefix("comment_state:") {
            return match CommentStateAction::parse_payload(rest) {
                Some(CommentStateAction::Session) | None => Self::None,
                Some(a) => Self::CommentState(a),
            };
        }
        let lower = s.to_lowercase();
        match lower.as_str() {
            "temp_pinyin" => Self::TempPinyin,
            "temp_english" => Self::TempEnglish,
            "aux_code" => Self::AuxCode,
            "rare_char" => Self::RareChar,
            "reverse" => Self::Reverse,
            "softkeyboard" => Self::SoftKeyboard(None),
            a if Self::DISPATCH_ACTIONS.contains(&a) => Self::Action(a.to_string()),
            _ => Self::None,
        }
    }

    /// 是否启用（非 `None`）。
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// 翻页动作在「翻不动」时怎么办（`page_prev` / `page_next` 的后缀）。
///
/// `keys.overflow.page_key` 是全局档；要落到单个键上（如「只有逗号翻不动时上屏」），在自定义按键
/// 里给那个键绑带后缀的翻页动作。翻不动判定与闸门见 `Coordinator::page_key_overflow_to_punct`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageOverflow {
    /// 跟随全局档 `keys.overflow.page_key`（出厂 / 写 `page_prev`）。
    Global,
    /// 顶高亮候选并输出该键符号（`page_prev:commit_and_input`）。
    CommitAndInput,
    /// 吞键，全局档开了上屏也压住（`page_prev:ignore`）。
    Ignore,
}

impl PageOverflow {
    fn parse_payload(s: &str) -> Option<Self> {
        match s.trim() {
            "commit_and_input" => Some(Self::CommitAndInput),
            "ignore" => Some(Self::Ignore),
            _ => None,
        }
    }

    /// 落盘写法的后缀；跟随全局为空。
    fn as_payload(&self) -> &'static str {
        match self {
            Self::Global => "",
            Self::CommitAndInput => "commit_and_input",
            Self::Ignore => "ignore",
        }
    }
}

/// `caret_right` 在编码区光标已到末尾时的动作（macOS 式 → 键的三种落法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretEnd {
    /// 上屏高亮候选（出厂 / 写 `caret_right`）。
    Commit,
    /// 高亮移到下一个候选（`caret_right:next`，macOS「→ 切到下一个候选词」）。
    Next,
    /// 开 / 关高亮候选的悬停提示（`caret_right:tip`，等同绑 `toggle_tip`）。
    Tip,
}

impl CaretEnd {
    fn parse_payload(s: &str) -> Option<Self> {
        match s.trim() {
            "commit" => Some(Self::Commit),
            "next" => Some(Self::Next),
            "tip" => Some(Self::Tip),
            _ => None,
        }
    }

    /// 落盘写法的后缀；默认档为空。
    fn as_payload(&self) -> &'static str {
        match self {
            Self::Commit => "",
            Self::Next => "next",
            Self::Tip => "tip",
        }
    }
}

/// 会话态按键动词（`keys.session_actions` 的值域）。
///
/// 与 [`BoundAction`] 是姊妹表，分野是触发态：那张管无输入会话时，本表管正在组合一段输入时。
/// 必须是两张表：同一个键在两种态下本就是两个动作（Tab 有会话时翻页、无会话时是制表符）；
/// 且边界与 C++ 侧的按键可达性区间重合（`_IsSessionKey` 功能键有会话时免费转发、可打印符号键须带
/// `HOTKEY_POLICY_FORWARD_ONLY`、修饰键与 CapsLock 只有 keyup），见 docs/design/session-key-actions.md §3。
///
/// 判据是「有会话」不是「有候选」：`clear` 在打了码还没出候选时也要能用，且须与 C++ 的
/// `hasComp || _hasCandidates` 同构。只在有候选时才有意义的动词由消费点自己判。
///
/// 值域：
/// - `"none"`：在本态禁用该键（第三态，非「未声明」）
/// - `"page_prev"` / `"page_next"`：上一页 / 下一页（候选展开后逐行移动激活行）
/// - `"page_screen_prev"` / `"page_screen_next"`：候选展开后翻一整屏（`rows` 行），未展开时
///   与 `page_prev` / `page_next` 相同
/// - `"highlight_up"` / `"highlight_down"`：高亮上移 / 下移
/// - `"cancel"`（别名 `"clear"`）：放弃当前输入，等同 Esc
/// - `"select_candidate:N"`：选中当前页第 N 个候选（N 从 1 起、至多 10，`2` 即次选键）
/// - `"select_char:N"`：以词定字，取当前高亮候选词的第 N 个字（N 从 1 起）
/// - `"aux_code"` / `"aux_code:page_next"`：进辅助码筛选（后者与下翻页共键）
/// - `"commit_highlighted"`：上屏当前高亮候选（即空格的「选词」功能）
/// - `"commit_code"`：上屏编码（同回车上屏编码，但不看 `input.enter_behavior`）
/// - `"command:<cmdbar 表达式>"`：执行一段命令，当前组合不动（同 [`BoundAction::Command`]）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    /// 未启用 / 显式禁用。
    None,
    /// 上一页 / 下一页；载荷是翻不动时的策略（写法 `page_prev` / `page_prev:commit_and_input|ignore`）。
    PagePrev(PageOverflow),
    PageNext(PageOverflow),
    /// 展开态翻整屏；未展开时同 `PagePrev`（见 `candidate_grid::grid_page_key`）。
    PageScreenPrev,
    PageScreenNext,
    HighlightUp,
    HighlightDown,
    /// 放弃当前输入：清空编码与候选，并退出 overlay 模式，语义等同 Esc。
    ///
    /// 诉求是 Esc 的替代键，不另立「只清空不退模式」的语义（普通输入下两者无区别）。`"clear"` 收作
    /// 别名而非第二个动词，避免两个名字行为微妙不同。
    Cancel,
    /// 选中当前页第 N 个候选（N 从 1 起，至多 10，1 = 首选）。收编自 `keys.select_key_groups`。
    /// 载荷用「第几个」而非 0-based 偏移，配置是给人读的。
    SelectCandidate(u8),
    /// 切换单字输入。动词、裸形式与载荷都与 [`BoundAction::SingleChar`] 一致。
    ///
    /// 两张表都有：`key_actions` 只解析得出符号键、字母 z 与修饰键，Tab / 翻页键要靠本表（同
    /// [`Self::AuxCode`]）。只在一张表里加是错的，另一张写同一个动词会落 `None`（见 [`Self::parse`]）。
    SingleChar(SingleCharAction),
    /// 切候选注释态。动词与载荷同 [`BoundAction::CommentState`]，另多一档 `session`
    /// （只翻转当前候选窗，不写配置）。
    CommentState(CommentStateAction),
    /// 以词定字：取当前高亮候选词的第 N 个字（N 从 1 起）。收编自 `keys.select_char_keys`。
    SelectChar(u8),
    /// 进辅助码筛选：对已出的候选按字形码二次过滤。
    ///
    /// 放在这张表：辅助码是对已有候选的操作（`enter_aux_code` 无候选即拒），且 `key_actions` 只认
    /// 符号键、字母 z 与修饰键，Tab / PageDown / 方向键写进去会被静默丢弃。`key_actions` 里的
    /// `aux_code` 仍有效（双拼出厂 `backtick = "aux_code"`），两条路都通向 `enter_aux_code`。
    /// 载荷 [`AuxCodeShare`] 是这个动词的参数（共键降级目标），不是第二条绑定。
    AuxCode(AuxCodeShare),
    /// 上屏当前高亮候选，空格在有候选时做的那件事，换一个键来按。
    ///
    /// 执行体与各模式空格臂同一个函数（`Coordinator::commit_highlighted`），副作用一致。两个空格变体
    /// 取舍相反：临英 `space_as_input` 不跟（绑本动词要的就是上屏动作本身）；联想态 `space_commits = false`
    /// 跟（「别替我选联想」与用哪个键无关），此时不吞键、回落原语义。
    /// 只在有候选时生效（[`Self::requires_candidates`]），空码空格的处置不属于本动词。
    CommitHighlighted,
    /// 编码区光标左移一字符（`input_buffer` 内，已转换前缀进不去）。
    ///
    /// 交互形态是「← 从编码末尾进入编辑编码」，底层是 `preedit_cursor::BufEdit::move_left`；光标是编辑
    /// 位置，不参与引擎查询，改码后按整串重算候选。已在开头 = 吞键（交给宿主会让宿主光标与组合区错位）；
    /// 无会话 = 放行。overlay 让位：临拼 / 快捷输入等有自己的缓冲光标（`state.active` 非 `None`）。
    CaretLeft,
    /// 编码区光标右移一字符；已在末尾 = 做 [`CaretEnd`] 指定的动作，默认上屏高亮候选（macOS「→ 进入选词」）。
    /// 写法 `caret_right` / `caret_right:commit|next|tip`。
    ///
    /// 边沿语义必须内建在动词里，静态绑定表达不出「到末尾那一下改为选词」。无候选时末尾那一下纯吞键。
    /// 其余边界同 [`Self::CaretLeft`]。
    CaretRight(CaretEnd),
    /// 编码区光标整音节左跳（rime `left_by_syllable`）：落到前一个音节起点，已在开头则回绕到
    /// 最后一个音节起点。分隔符 `'` 之后算起点。无可用切分（双拼、码表单码等）时退回左移一字符。
    /// 可绑到普通键（会话键表只认键 + Shift）。Ctrl+← 是内建的另一条通路（见
    /// `Compiler` 的 Ctrl+←/→ 转发登记与协调器的 Ctrl 分支），不经本动词。
    SyllablePrev,
    /// 编码区光标整音节右跳（rime `right_by_syllable`），已在末尾则回绕到第一个音节边界。
    /// 其余同 [`Self::SyllablePrev`]。
    SyllableNext,
    /// 开 / 关高亮候选的悬停提示气泡（反查编码、拆字等），不用鼠标悬停。
    ///
    /// 走鼠标悬停同一条通路（悬停目标 = 高亮项）；再按一次收起，移动高亮 / 翻页 / 候选刷新都会收起。
    /// 需要候选；候选没有提示内容时无效果。
    ToggleTip,
    /// 上屏编码，回车在有编码时做的那件事，换一个键来按（极点 / 小鸭用 Ctrl）。
    ///
    /// 恒为上屏编码，不看 `input.enter_behavior`。执行方式是把这个键当回车重新分派并强制走上屏编码
    /// 那一支（`Coordinator::redispatch_commit_code`），不另写上屏，免得跟着各模式的回车臂逐个维护。
    /// 不要候选（空码时上屏原码恰是常用场合）；无会话、联想态时放行。
    CommitCode,
    /// 执行一段 cmdbar 表达式，当前组合原样不动（不清空、不上屏），吞键。
    /// 写法、载荷与执行通路都与 [`BoundAction::Command`] 逐字一致。
    ///
    /// 不要候选（[`Self::requires_candidates`] 为假）：打了码还没出候选时也能按；判据与
    /// `cancel` 同侧——有会话即可，空闲时放行（绑在 Tab 上，没打字时 Tab 仍是制表符）。
    Command(String),
}

/// 辅助码触发键的「共键」参数：进不去辅助码时，这个键改做什么。
///
/// 做成动词参数，而不是组合动词（`page_next_aux_code` 会先翻页再进入，名字随组合数相乘）或通用
/// 降级链（`aux_code|page_next`，可表达的组合远多于有意义的，已在 `docs/design/key-resolver-unification.md`
/// §5 否决）。一组取值只对一个动词有意义 ⇒ 它是那个动词的参数。
///
/// 顺序即优先级：`aux_code:page_next` = 先试辅助码，进不去才翻页。「进不去」复用 `enter_aux_code`
/// 门卫不过返回 `None` 的契约：
/// - 主输入路 + 辅助码可用 → 进辅助码，不翻页（首选不会被翻走）；
/// - 已在辅助码态内 → 降级翻页；
/// - 辅助码未开启 / 方案无码表 → 退化成纯翻页键；
/// - 无候选 → 两个成员都 `requires_candidates`，按键放行，Tab 仍是宿主的制表符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxCodeShare {
    /// 专用触发键（`aux_code`）：进不去就不吞键，落该键原本的语义。
    Solo,
    /// 与下翻页共键（`aux_code:page_next`）：进不去时翻下一页。只收下翻页，`page_prev` 共键没有
    /// 对应的心智；要加成员先问有没有真实用例。
    PageNext,
}

impl SessionAction {
    /// 解析配置字符串。大小写与首尾空白不敏感；未知值 → [`Self::None`]，不静默变成别的动作。
    /// 写错的动词与「没配」同形，调用方在加载期要 `warn`（见 [`Self::parse_checked`]）。
    pub fn parse(s: &str) -> Self {
        // `command:` 必须在小写化之前剥出来：表达式原样保留大小写（路径 / 网址）。
        if let Some(expr) = parse_command_verb(s) {
            return expr.map_or(Self::None, Self::Command);
        }
        let t = s.trim().to_lowercase();
        // 带载荷的两个动词，序号从 1 起。`select_candidate` 上限 10（数字键 0 选第 10 个，见
        // `handle_number_key_select`）；`select_char` 限个位数，放宽只会静默收下必不生效的配置。
        if let Some(n) = t.strip_prefix("select_candidate:") {
            return match n.trim().parse::<u8>() {
                Ok(n @ 1..=10) => Self::SelectCandidate(n),
                _ => Self::None,
            };
        }
        if let Some(n) = t.strip_prefix("select_char:") {
            return match n.trim().parse::<u8>() {
                Ok(n @ 1..=9) => Self::SelectChar(n),
                _ => Self::None,
            };
        }
        // 辅助码的共键参数（`aux_code:page_next`）。未知参数落 `None` 而不是退回专用触发键
        // ——静默降级会让 `aux_code:page_prev` 这种写错的配置表现成「共键没生效」，
        // 而那与「功能坏了」同形；落 `None` 则由 `parse_checked` 在加载期告警。
        if let Some(rest) = t.strip_prefix("aux_code:") {
            return match rest.trim() {
                "page_next" => Self::AuxCode(AuxCodeShare::PageNext),
                _ => Self::None,
            };
        }
        // 单字输入。动词、裸形式与载荷须与 `BoundAction::parse` 里那一段逐字一致，
        // 否则用户把配置从一张表挪到另一张就静默失效。
        if t == "single_char" {
            return Self::SingleChar(SingleCharAction::Toggle);
        }
        if let Some(rest) = t.strip_prefix("single_char:") {
            return match SingleCharAction::parse_payload(rest) {
                Some(a) => Self::SingleChar(a),
                None => Self::None,
            };
        }
        if t == "comment_state" {
            return Self::CommentState(CommentStateAction::Toggle);
        }
        if let Some(rest) = t.strip_prefix("comment_state:") {
            return match CommentStateAction::parse_payload(rest) {
                Some(a) => Self::CommentState(a),
                None => Self::None,
            };
        }
        for (prefix, page_next) in [("page_prev:", false), ("page_next:", true)] {
            if let Some(rest) = t.strip_prefix(prefix) {
                return match PageOverflow::parse_payload(rest) {
                    Some(o) if page_next => Self::PageNext(o),
                    Some(o) => Self::PagePrev(o),
                    None => Self::None,
                };
            }
        }
        if let Some(rest) = t.strip_prefix("caret_right:") {
            return match CaretEnd::parse_payload(rest) {
                Some(a) => Self::CaretRight(a),
                None => Self::None,
            };
        }
        match t.as_str() {
            "page_prev" => Self::PagePrev(PageOverflow::Global),
            "page_next" => Self::PageNext(PageOverflow::Global),
            "page_screen_prev" => Self::PageScreenPrev,
            "page_screen_next" => Self::PageScreenNext,
            "highlight_up" => Self::HighlightUp,
            "highlight_down" => Self::HighlightDown,
            // 与 `BoundAction::parse` 的同名动词逐字一致：同一个功能在两张表里写法不同的话，
            // 用户把配置从一张表挪到另一张就会静默失效。
            // （`key_actions` 那张表没有共键形态：它只认符号键与字母 z，翻页键压根解析不出来。）
            "aux_code" => Self::AuxCode(AuxCodeShare::Solo),
            "commit_highlighted" => Self::CommitHighlighted,
            "caret_left" => Self::CaretLeft,
            "caret_right" => Self::CaretRight(CaretEnd::Commit),
            "syllable_prev" => Self::SyllablePrev,
            "syllable_next" => Self::SyllableNext,
            "toggle_tip" => Self::ToggleTip,
            "commit_code" => Self::CommitCode,
            // `clear` 是 `cancel` 的别名（同一个动作，两种心智），见 `Cancel` 的文档。
            "cancel" | "clear" => Self::Cancel,
            _ => Self::None,
        }
    }

    /// 该动作是否只在有候选时才有意义。
    ///
    /// 导航类在没有候选时无事可做，消费点据此放行按键；`cancel` 在打了码还没出候选时必须生效
    /// （见 docs/design/session-key-actions.md §4）。判据挂在动作上而非三个消费点（主输入 / mix /
    /// 候选导航）各写一份。
    pub fn requires_candidates(&self) -> bool {
        // 注释态：只有翻转当前候选窗（`session`）依赖候选窗；改持久态随时可做。
        if let Self::CommentState(a) = self {
            return *a == CommentStateAction::Session;
        }
        // 以下与 `Cancel` 同侧，在「打了码还没出候选」时也要生效：`SingleChar`（单字模式下可能
        // 一条候选都不剩，放行就再也关不掉）、`Command`、`CommitCode`（空码上屏原码）、
        // `CaretLeft` / `CaretRight`（候选因无匹配而空时正是最想改码的时刻）。
        !matches!(
            self,
            Self::None
                | Self::Cancel
                | Self::SingleChar(_)
                | Self::Command(_)
                | Self::CommitCode
                | Self::CaretLeft
                | Self::CaretRight(_)
                | Self::SyllablePrev
                | Self::SyllableNext
        )
    }

    /// 选中第几个候选（1 起）——非选词动词返回 `None`。
    pub fn candidate_ordinal(&self) -> Option<u8> {
        match self {
            Self::SelectCandidate(n) => Some(*n),
            _ => None,
        }
    }

    /// 以词定字取第几个字（1 起）——非取字动词返回 `None`。
    pub fn char_ordinal(&self) -> Option<u8> {
        match self {
            Self::SelectChar(n) => Some(*n),
            _ => None,
        }
    }

    /// 同 [`Self::parse`]，但区分「写错的动词」与「显式 none」，供加载期告警。
    /// 返回 `None` 表示值不认识；`Some(Self::None)` 表示用户明确写了 `none`。
    pub fn parse_checked(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("none") {
            return Some(Self::None);
        }
        // 传原串而非小写化的副本：`command:` 的表达式要原样保留大小写。
        match Self::parse(t) {
            Self::None => None,
            a => Some(a),
        }
    }

    /// 是否启用（非 [`Self::None`]）。
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// 配置字符串形式（折算与写回用），与 [`SessionAction::parse`] 互为逆运算。
/// 用 `Display` 而非 `as_str()`：带载荷的动词拼不出 `'static` 串。两边对不上的话，
/// 折算出来的配置自己就读不回来。
impl std::fmt::Display for SessionAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::PagePrev(o) => match o.as_payload() {
                "" => f.write_str("page_prev"),
                p => write!(f, "page_prev:{p}"),
            },
            Self::PageNext(o) => match o.as_payload() {
                "" => f.write_str("page_next"),
                p => write!(f, "page_next:{p}"),
            },
            Self::PageScreenPrev => f.write_str("page_screen_prev"),
            Self::PageScreenNext => f.write_str("page_screen_next"),
            Self::HighlightUp => f.write_str("highlight_up"),
            Self::HighlightDown => f.write_str("highlight_down"),
            // 别名 `clear` 刻意不回写——规范名只有一个，避免同一份配置在两次保存后
            // 出现两种写法。
            Self::Cancel => f.write_str("cancel"),
            Self::SelectCandidate(n) => write!(f, "select_candidate:{n}"),
            Self::SelectChar(n) => write!(f, "select_char:{n}"),
            Self::AuxCode(AuxCodeShare::Solo) => f.write_str("aux_code"),
            Self::AuxCode(AuxCodeShare::PageNext) => f.write_str("aux_code:page_next"),
            Self::SingleChar(a) => write!(f, "single_char:{}", a.as_payload()),
            Self::CommentState(a) => match a.as_payload() {
                "" => f.write_str("comment_state"),
                p => write!(f, "comment_state:{p}"),
            },
            Self::CommitHighlighted => f.write_str("commit_highlighted"),
            Self::CaretLeft => f.write_str("caret_left"),
            Self::SyllablePrev => f.write_str("syllable_prev"),
            Self::SyllableNext => f.write_str("syllable_next"),
            Self::CaretRight(a) => match a.as_payload() {
                "" => f.write_str("caret_right"),
                p => write!(f, "caret_right:{p}"),
            },
            Self::ToggleTip => f.write_str("toggle_tip"),
            Self::CommitCode => f.write_str("commit_code"),
            Self::Command(expr) => write!(f, "{COMMAND_VERB_PREFIX}{expr}"),
        }
    }
}

/// 全局码表配置（[schema.codetable]）。所有码表方案的公共基线，方案可经
/// `schema_overrides/{id}.toml` 的 `[codetable]` 段（带 enabled 总开关）逐字段覆盖。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodetableGlobal {
    /// 英文候选混入（[schema.codetable.english_merge]）。见 [`CodetableEnglishMerge`]。
    /// 方案级覆盖须单独登记进 `SCHEMA_OVERRIDES`：`schema.codetable.` 段前缀刻意不递归到子段
    /// （见 `SchemaOverride::key`），与 `frequency.` 同理。
    #[serde(default)]
    pub english_merge: CodetableEnglishMerge,
    /// 顶码上屏（超满码长取前 N 码首选上屏）。
    #[serde(default)]
    pub top_code_commit: bool,
    /// 满码无候选时清空缓冲。
    #[serde(default)]
    pub clear_on_empty_max: bool,
    /// 满码唯一精确时自动上屏。
    #[serde(default)]
    pub auto_commit_at_full: bool,
    /// 自动上屏最短码长（隐藏参数；0=等于全码长，不在设置 UI 暴露）。
    #[serde(default)]
    pub auto_commit_min_len: usize,
    /// 标点触发上屏。
    #[serde(default)]
    pub punct_commit: bool,
    /// 显示编码提示。
    #[serde(default = "default_true")]
    pub show_code_hint: bool,
    /// 精确匹配模式（关闭前缀匹配）。
    #[serde(default)]
    pub single_code_input: bool,
    /// 精确匹配空码补全（无候选时从更长编码取首选）。
    #[serde(default)]
    pub single_code_complete: bool,
    /// z 键重复输入。
    #[serde(default)]
    pub z_key_repeat: bool,
    /// z 键功能：空缓冲按 z 且 z 在本方案是死码时，进哪个模式。见 [`BoundAction`]。
    ///
    /// 与 [`Self::z_key_repeat`] 正交（可同时开）：repeat 先手，继续打字母才轮到本项，
    /// 详见 `Coordinator::try_activate_mode` 的三重身份裁决。
    #[serde(default)]
    pub z_key_action: String,
    /// 码元字符集（哪些字符可进输入缓冲）。空=内置默认 `a-z`。
    /// 解析与回落见 [`crate::code_charset::CodeCharSet`]。
    #[serde(default)]
    pub input_chars: String,
    /// 可作首码的字符集（`input_chars` 的子集）。空=与 `input_chars` 相同。
    #[serde(default)]
    pub leading_chars: String,
    /// 通配输入（万能键）：组码时通配键代替恰好一个码元，候选显示完整编码；
    /// 通配结果不自动上屏、不顶字、不清空。见 `docs/design/codetable-wildcard.md`。
    #[serde(default)]
    pub wildcard: bool,
    /// 通配键：单个字面字符（字母或 ASCII 符号）。合法性见 [`parse_wildcard_key`]。
    #[serde(default = "default_wildcard_key")]
    pub wildcard_key: String,
    /// 通配结果只留单字（一个字素簇），行内通配与反查模式都受它管；
    /// 与 `wildcard` 主开关正交。见 reverse-mode spec §2。
    #[serde(default)]
    pub wildcard_single_only: bool,
    /// 码表整句 N-best：候选列表里露几条整句。出厂 1，上限 8。只在方案开了整句
    /// （`[engine.codetable].sentence_input`）时有消费者。与 `schema.pinyin.sentence_count` 各管各的：
    /// 两边整句首选准确率不在一个水平上。
    #[serde(default = "default_sentence_count")]
    pub sentence_count: u8,
    /// 码表整句 N-best：解码时算几条（整句切换键的池子）。不足 `sentence_count` 时按后者抬。
    #[serde(default = "default_sentence_count")]
    pub sentence_max_count: u8,
    /// 码表整句的切换键。空串 = 关闭（出厂）。键名取值、夺取规则同
    /// `schema.pinyin.sentence_cycle_key`。
    #[serde(default)]
    pub sentence_cycle_key: String,
    /// 码表整句挂语法模型的权重。0 = 不挂。出厂 0.5。
    ///
    /// 模型固定用随包的 `wanxiang-mini.gram`，不跟 `schema.pinyin.grammar`（拼音桌面端出厂不挂）：
    /// 码表整句挂上收益大得多；权重取 0.5 是因为 1.0 在逐字全码打法上反而掉分。数据见
    /// `docs/design/codetable-sentence-input.md` §12。
    #[serde(default = "default_codetable_sentence_grammar_weight")]
    pub sentence_grammar_weight: f64,
    /// 出简让全：有简码的字，在更长的码位上把首选让给词语（「路」的三简是 `kht`，那么 `khtk` 的首选
    /// 就该给「路上」之类）。值 = 参与让位的简码级别上限：
    ///
    /// - `0` 关闭（全局出厂值），候选顺序完全按词库原序
    /// - `2` 一二级简码置后
    /// - `3` 全部简码置后
    ///
    /// 判据是「当前码长 > 本值」而非「当前码长 == 全码长」，后者换到非四码方案就错位。
    #[serde(default = "default_short_code_yield_level")]
    pub short_code_yield_level: usize,
    /// 字词范围：候选里出单字、出词组，还是两者都出（`all` / `char` / `phrase`）。出厂关。
    ///
    /// 主要价值是定长盲打：五笔单字恒 ≤ 全码长，滤掉同码的词后「满码唯一即上屏」的成立率大幅提高。
    /// 与 `input.filter_mode`（检索范围）正交，同时施加时依次生效，判据是
    /// `wind_candidate::single_char_admits`。拼音另有一份（[`PinyinGlobalConfig::single_char`]），
    /// 不共享取值。运行时热键切换不写回本项，故读它得不到「此刻开没开」。
    /// 出厂关的理由同 `default_short_code_yield_level`：只出单字是定长码表才成立的手感。
    #[serde(default)]
    pub single_char: bool,
    /// 码表调频（统一开关，取代旧 user_frequency）。
    #[serde(default)]
    pub frequency: CodetableFrequency,
    /// 码表自动造词（连续单字）。
    #[serde(default)]
    pub auto_phrase: AutoPhraseConfig,
}

/// 出简让全的全局出厂值是关，由方案在 `[engine.codetable]` 里声明（内置 wubi86 声明 3）。
///
/// 全局基线是所有码表方案共用的，而「短码首选 = 简码」只对五笔这类前缀式简码成立；第三方码表的
/// 短码首选往往是作者排定的常用字，全局开会静默改掉它们的候选顺序。同 `z_key_action`：
/// 「这张码表适不适合」只有方案作者知道。
fn default_short_code_yield_level() -> usize {
    0
}

fn default_wildcard_key() -> String {
    "z".to_string()
}

/// 通配键的合法性：恰好一个字符、ASCII 可打印（不含空格）、不是数字；字母归一小写。
/// 数字排除：空缓冲时数字键到不了 core，组码中数字恒是选词键，两处都当不了通配。
pub fn parse_wildcard_key(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    if it.next().is_some() || !c.is_ascii_graphic() || c.is_ascii_digit() {
        return None;
    }
    Some(c.to_ascii_lowercase())
}

impl Default for CodetableGlobal {
    fn default() -> Self {
        Self {
            english_merge: CodetableEnglishMerge::default(),
            // 这是结构体零值，不是出厂默认（出厂值在 `data/config.toml` 的 L2，恒覆盖本处）。大量集成测试
            // 以 `Config::default()` 构造，拨成 true 会改变它们的输入行为。特殊方案的折叠基线另见
            // `EngineManager::SPECIAL_SCHEMA_BASELINE`，与这里不是同一件事，别合并。
            top_code_commit: false,
            clear_on_empty_max: false,
            auto_commit_at_full: false,
            auto_commit_min_len: 0,
            punct_commit: false,
            show_code_hint: true,
            single_code_input: false,
            single_code_complete: false,
            short_code_yield_level: default_short_code_yield_level(),
            single_char: false,
            z_key_repeat: false,
            z_key_action: String::new(),
            // 空串 = 未设置 → `CodeCharSet::new` 回落内置默认 `a-z`，与历史硬编码
            // `VK_A..=VK_Z` 逐键等价。这里刻意不写 "a-z" 字面量：让「未配置」在
            // 结构体零值与 TOML 缺省两处是同一个值，避免两套默认源不一致。
            input_chars: String::new(),
            leading_chars: String::new(),
            wildcard: false,
            wildcard_key: default_wildcard_key(),
            wildcard_single_only: false,
            sentence_count: default_sentence_count(),
            sentence_max_count: default_sentence_count(),
            sentence_cycle_key: String::new(),
            sentence_grammar_weight: default_codetable_sentence_grammar_weight(),
            frequency: CodetableFrequency::default(),
            auto_phrase: AutoPhraseConfig::default(),
        }
    }
}

impl CodetableGlobal {
    /// 折叠方案 `[engine.codetable]` 的内联/override 行为到全局基线：各 `Some(_)` 字段覆盖，
    /// `None` 回落全局。schema 内联与 `schema_overrides` 已在 `read_schema` 经 `merge_toml`
    /// 合并成单个 `CodeTableSpec`，此处只做「方案 → 全局」一次折叠。见 schema-config-layering.md §4。
    pub fn resolved(&self, ov: Option<&crate::schema::CodeTableSpec>) -> CodetableGlobal {
        let mut out = self.clone();
        let Some(o) = ov else {
            return out;
        };
        if let Some(v) = o.top_code_commit {
            out.top_code_commit = v;
        }
        if let Some(v) = o.clear_on_empty_max {
            out.clear_on_empty_max = v;
        }
        if let Some(v) = o.auto_commit_at_full {
            out.auto_commit_at_full = v;
        }
        if let Some(v) = o.auto_commit_min_len {
            out.auto_commit_min_len = v;
        }
        if let Some(v) = o.punct_commit {
            out.punct_commit = v;
        }
        if let Some(v) = o.show_code_hint {
            out.show_code_hint = v;
        }
        if let Some(v) = o.single_code_input {
            out.single_code_input = v;
        }
        if let Some(v) = o.single_code_complete {
            out.single_code_complete = v;
        }
        if let Some(v) = o.short_code_yield_level {
            out.short_code_yield_level = v;
        }
        // 字词范围：方案表了态才覆盖。`Follow`（含「没写这一项」）交出 `None`，回落全局
        // 方案没表态（`None`）就保留全局值。
        if let Some(v) = o.single_char {
            out.single_char = v;
        }
        if let Some(v) = o.z_key_repeat {
            out.z_key_repeat = v;
        }
        // 码元字符集是 `String` 而非 `Option`，故「未设置」由空串表达（与上面那些
        // tri-state 字段不同）。非空才覆盖——否则方案没写这项时会把全局基线抹成空串，
        // 落到 `CodeCharSet::new` 又被回落成 `a-z`，全局配的字符集就被静默丢弃了。
        if !o.input_chars.is_empty() {
            out.input_chars = o.input_chars.clone();
        }
        if !o.leading_chars.is_empty() {
            out.leading_chars = o.leading_chars.clone();
        }
        if let Some(v) = &o.z_key_action {
            out.z_key_action = v.clone();
        }
        if let Some(v) = o.wildcard {
            out.wildcard = v;
        }
        if let Some(v) = &o.wildcard_key {
            out.wildcard_key = v.clone();
        }
        if let Some(v) = o.wildcard_single_only {
            out.wildcard_single_only = v;
        }
        if let Some(v) = o.sentence_count {
            out.sentence_count = v;
        }
        if let Some(v) = o.sentence_max_count {
            out.sentence_max_count = v;
        }
        if let Some(v) = &o.sentence_cycle_key {
            out.sentence_cycle_key = v.clone();
        }
        if let Some(v) = o.sentence_grammar_weight {
            out.sentence_grammar_weight = v;
        }
        // 调频段逐字段折叠，整段缺省 = 全部跟随基线。消费方是 `EngineManager::freq_settings`，
        // 与上屏行为字段（`build_engine` 的 CommitOptions）不同路径；加字段时两条都要看。
        if let Some(f) = &o.frequency {
            if let Some(v) = f.enabled {
                out.frequency.enabled = v;
            }
            if let Some(v) = f.record_always {
                out.frequency.record_always = v;
            }
            if let Some(v) = &f.strategy {
                out.frequency.strategy = v.clone();
            }
            if let Some(v) = &f.promote_prefix {
                out.frequency.promote_prefix = v.clone();
            }
            if let Some(v) = f.half_life {
                out.frequency.half_life = v;
            }
            if let Some(v) = f.protect_top_n {
                out.frequency.protect_top_n = v;
            }
            if let Some(v) = f.protect_top_n_len1 {
                out.frequency.protect_top_n_len1 = v;
            }
            if let Some(v) = f.protect_top_n_len2 {
                out.frequency.protect_top_n_len2 = v;
            }
            if let Some(v) = f.protect_top_n_len3 {
                out.frequency.protect_top_n_len3 = v;
            }
        }
        // 英文混入子段：同 `frequency` 的逐字段稀疏折叠。消费方是 `EngineManager::english_merge_ctx`
        // （按活跃方案取 resolved 值），不是 `build_engine` 的 CommitOptions。
        if let Some(e) = &o.english_merge {
            if let Some(v) = e.enable {
                out.english_merge.enable = v;
            }
            if let Some(v) = e.min_length {
                out.english_merge.min_length = v;
            }
            if let Some(v) = e.block_commit {
                out.english_merge.block_commit = v;
            }
            if let Some(v) = e.complete_min_length {
                out.english_merge.complete_min_length = v;
            }
            if let Some(v) = e.block_commit_on_word {
                out.english_merge.block_commit_on_word = v;
            }
            if let Some(v) = e.exact_position {
                out.english_merge.exact_position = v;
            }
            if let Some(v) = e.complete_position {
                out.english_merge.complete_position = v;
            }
            if let Some(v) = e.max_count {
                out.english_merge.max_count = v;
            }
        }
        out
    }

    /// 生效的通配键：关闭 ⇒ `None`；键非法 ⇒ 告警并视为关闭（spec §4）。
    /// 只在构建引擎时调用一次（`EngineManager::build_engine`），协调器经
    /// `EngineManager::active_wildcard_key` 取值，免得热路径上反复告警。
    pub fn wildcard_char(&self, schema_id: &str) -> Option<char> {
        if !self.wildcard {
            return None;
        }
        let parsed = parse_wildcard_key(&self.wildcard_key);
        if parsed.is_none() {
            tracing::warn!(
                "方案 {} 的 wildcard_key = {:?} 非法（须为单个字母或 ASCII 符号），通配输入已关闭",
                schema_id,
                self.wildcard_key
            );
        }
        parsed
    }

    /// 反查模式内的通配键：取方案 `wildcard_key`，非法回落 `z`（spec §3.2）。
    ///
    /// 不看主开关 `wildcard`；也不告警——合法性告警归 [`Self::wildcard_char`]。
    pub fn reverse_wildcard_char(&self) -> char {
        parse_wildcard_key(&self.wildcard_key).unwrap_or('z')
    }
}

/// 码表调频（[schema.codetable.frequency]）。
///
/// 不 derive `Eq`：`half_life` 是 f64。与 `PinyinFrequency` 一致。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodetableFrequency {
    #[serde(default)]
    pub enabled: bool,
    /// 调频关闭时仍记录选词次数。`enabled = true` 时本项无意义。
    ///
    /// 只放开写端（`record_selection_in`），读端（主候选重排、英文热词召回、词语联想打分）仍只认
    /// `enabled`：攒一份真实常用字词的次数表供查看 / 导出，候选顺序保持固定；日后开 `enabled` 即刻参与排序。
    #[serde(default)]
    pub record_always: bool,
    /// 锁定码表原始前 N 位，兜底档：码长 ≥ 4 的深码位；简码位（码长 1/2/3）见下面三个字段。
    /// 作用域是「码表配置组」而非纯码表方案：混输也用这套值（`EngineManager::freq_settings`
    /// 按「非拼音即码表」分流）。
    #[serde(default)]
    pub protect_top_n: usize,
    /// 一简位（码长 1）保护前 N 位。五笔一简 25 个码每个都是二选一，默认保护首选。
    #[serde(default = "default_protect_len1")]
    pub protect_top_n_len1: usize,
    /// 二简位（码长 2）保护前 N 位。
    #[serde(default = "default_protect_len2")]
    pub protect_top_n_len2: usize,
    /// 三简位（码长 3）保护前 N 位。默认不保护——三简的钦定性弱于一二简。
    #[serde(default)]
    pub protect_top_n_len3: usize,
    /// 词频应用策略：`"top"`（一次到顶 MRU）/ `"step"`（逐次提升，默认）/ `"position"`（位次减半）。
    ///
    /// `top`/`step` 是布尔 used-first：用过一次即跳到档内最前，策略只决定已用过的那批怎么排。
    /// `position` 让位次连续表达强弱，适合前缀匹配为主的方案（尤其英文）。
    #[serde(default = "default_freq_strategy")]
    pub strategy: String,
    /// 前缀补全候选参与词频位置提升的范围（`"none"` / `"single"` / `"all"`）。仅 `strategy = "position"`
    /// 时生效。
    ///
    /// 码表默认 `"all"`（拼音是 `"single"`）：码表的前缀补全已由来源档位隔离，且与 `top`/`step`
    /// 对前缀补全从无限制的行为一致，存量用户的调频范围不会突然变窄。
    #[serde(default = "default_codetable_promote_prefix")]
    pub promote_prefix: String,
    /// 衰减半衰期（小时）；`0` = 内置默认 72 小时。仅 `strategy = "position"` 时生效。
    ///
    /// 与拼音段完全独立，不回落到 `schema.pinyin.frequency.half_life`：设置页上是两个独立控件，
    /// 回落链会让「改了拼音的、码表跟着变」。
    #[serde(default)]
    pub half_life: f64,
}

fn default_english_code_scope() -> String {
    "candidate".to_string()
}

fn default_codetable_promote_prefix() -> String {
    "all".to_string()
}

fn default_protect_len1() -> usize {
    1
}

fn default_protect_len2() -> usize {
    1
}

fn default_freq_strategy() -> String {
    "step".to_string()
}

impl Default for CodetableFrequency {
    fn default() -> Self {
        Self {
            enabled: false,
            record_always: false,
            protect_top_n: 0,
            protect_top_n_len1: default_protect_len1(),
            protect_top_n_len2: default_protect_len2(),
            protect_top_n_len3: 0,
            strategy: default_freq_strategy(),
            promote_prefix: default_codetable_promote_prefix(),
            half_life: 0.0, // 0 = 用内置默认 72 小时
        }
    }
}

/// 拼音调频（[schema.pinyin.frequency]）。衰减参数（0=用 store 默认）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PinyinFrequency {
    #[serde(default)]
    pub enabled: bool,
    /// 调频关闭时仍记录选词次数；语义同 [`CodetableFrequency::record_always`]。
    #[serde(default)]
    pub record_always: bool,
    /// 半衰期（小时）。
    #[serde(default)]
    pub half_life: f64,
    /// base 系数。
    #[serde(default)]
    pub base_scale: f64,
    /// 最近使用峰值。
    #[serde(default)]
    pub recency_peak: f64,
    /// 前缀补全候选参与词频位置提升的范围（默认 `"single"`）。
    ///
    /// | 取值 | 打 `d` 选「得」 | 打 `d` 选「东西」 | 打 `hel` 选 `hello` |
    /// |---|---|---|---|
    /// | `"none"` | 不提升 | 不提升 | 不提升 |
    /// | `"single"`（默认） | 提升 | 不提升 | 提升 |
    /// | `"all"` | 提升 | 提升 | 提升 |
    ///
    /// 判据是[语义单元数][wind_candidate::semantic_units]（汉字逐字计、西文词整体计 1）而非字符数，
    /// 否则英文调频全灭。默认 `"single"`：短输入撑不起一个词组，把长词组顶到高频单字前面与直觉相悖，
    /// 而单字之间的调整是合理的。只作用于有效前缀层（`is_prefix && !is_promoted_completion`），
    /// 与 `cmp_match_layers` 同口径。
    #[serde(default = "default_promote_prefix")]
    pub promote_prefix: String,
}

fn default_promote_prefix() -> String {
    "single".to_string()
}

/// 码表自动造词（[schema.codetable.auto_phrase]），滑窗草稿层模型
/// （`docs/design/auto-phrase-draft-layer.md`）。
///
/// 缓冲的是最近落屏的文本流（逐字打与选词组上屏都进流）。词走两跳才进用户词库：
/// 1. 滑窗切出 `min_phrase_len..=max_phrase_len` 字的窗口 → 全部记进草稿层
///    （`DRAFT_WORDS` 表，带 `draft_ttl_hours` 有效期）；
/// 2. 用户用某条草稿上屏过一次 → 跃迁进临时词库；
/// 3. 此后累计使用达 `promote_count` 次 → 晋升进用户词库（永久，无容量上限）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoPhraseConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 滑窗下界（字，默认 2；内部字段，设置页不开放）。
    #[serde(default = "default_phrase_min_len")]
    pub min_phrase_len: usize,
    /// 滑窗上界（字，默认 5；内部字段，设置页不开放）。超过它的连续输入照常切出
    /// `min..=max` 的各个窗口，不整段丢弃。
    #[serde(default = "default_phrase_max_len")]
    pub max_phrase_len: usize,
    /// 临时词晋升进用户词库所需使用次数。0 = 不晋升，一直留在临时词库。
    ///
    /// 这里的 `0` 是结构体零值（L1），出厂值在 `data/config.toml`（L2）是 3；只动 L2，理由同
    /// [`CodetableGlobal::default`]。拼音侧 [`AutoLearnConfig::promote_count`] 出厂仍是 0：本段出厂
    /// `enabled = false` 只影响主动开启的用户，拼音那段出厂就开着。
    #[serde(default)]
    pub promote_count: usize,
    /// 落屏文本流的最大间隔（毫秒，0=默认 5000）。超时即断流，已缓冲内容不再与后续拼接。
    /// 兜底用：其余断流信号（标点/焦点/切方案/移光标）全漏时防止跨句拼出杂词。内部字段。
    #[serde(default)]
    pub idle_timeout_ms: u32,
    /// 临时词库条目上限（0=不限）。超出后按「用得最少、造得最早」淘汰。内部字段，设置页不开放。
    #[serde(default = "default_temp_max_entries")]
    pub temp_max_entries: usize,
    /// 草稿层有效期（小时，0=永不过期）。默认 24。到期没被用过的草稿自动丢弃。内部字段。
    #[serde(default = "default_draft_ttl_hours")]
    pub draft_ttl_hours: u32,
    /// 草稿层条目上限（0=不限）。超出后按 `last_used` 升序淘汰（近似 LRU）。内部字段。
    /// 默认值来自设计稿 §7 的纸面估算上界，不是实测值，待真机数据重定。
    #[serde(default = "default_draft_max_entries")]
    pub draft_max_entries: usize,
    /// 草稿层单次批量写最多落库多少条。默认 64，待实测调参。太小则多开几次批量写，太大则一批的
    /// 取码与查重堆在一起、崩溃时丢得更多。内部字段。
    #[serde(default = "default_draft_flush_batch")]
    pub draft_flush_batch: usize,
}

fn default_draft_ttl_hours() -> u32 {
    24
}

fn default_draft_max_entries() -> usize {
    50_000
}

fn default_draft_flush_batch() -> usize {
    64
}

fn default_phrase_min_len() -> usize {
    2
}

/// 默认 5（原为 10）。五笔场景下 10 字连续序列几乎必是跨句杂词；Go 版默认亦为 5。
fn default_phrase_max_len() -> usize {
    5
}

fn default_temp_max_entries() -> usize {
    5000
}

impl Default for AutoPhraseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_phrase_len: default_phrase_min_len(),
            max_phrase_len: default_phrase_max_len(),
            promote_count: 0,
            idle_timeout_ms: 0,
            temp_max_entries: default_temp_max_entries(),
            draft_ttl_hours: default_draft_ttl_hours(),
            draft_max_entries: default_draft_max_entries(),
            draft_flush_batch: default_draft_flush_batch(),
        }
    }
}

/// 拼音自动造词（[schema.pinyin.auto_learn]）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoLearnConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 造词最小字数。
    ///
    /// 默认值必须落在设置页数值控件的 `[min, max]` 区间内（min = 2），否则控件夹值后没动过也会写盘
    /// （`untouched_settings_produce_no_diff`）。消费端的 `== 0` 回退 2 保留：老配置里可能显式写着 0。
    #[serde(default = "default_learn_min_len")]
    pub min_word_length: usize,
    /// 造词最大字数（`0` = 不限）。超出后先按「从尾部保留整段」裁剪，仍超则整体放弃：在一串汉字中间
    /// 切一刀，切出来的多半不是词。整句一次上屏时只有一段可裁，对整句等价于「超过就不学」。
    /// 默认 10：覆盖值得进词库的长词，又挡住整句解常见的跨句拼接。
    #[serde(default = "default_learn_max_len")]
    pub max_word_length: usize,
    /// 临时词晋升进用户词库所需使用次数。0 = 不晋升，一直留在临时词库（出厂值）。
    /// 与 [`AutoPhraseConfig::promote_count`] 同义、共用晋升判定（`Coordinator::maybe_promote_temp`）。
    ///
    /// 出厂值与码表侧（3）不同：本段出厂 `enabled = true`，改本项等于给全体用户的用户词库开闸，
    /// 需先有真机数据。
    #[serde(default)]
    pub promote_count: usize,
}

/// 拼音造词最小字数默认值。取 2 而非 0：0 是「回退 2」的哨兵，两者生效值相同，
/// 但只有 2 落在设置页控件的取值区间内——见 [`AutoLearnConfig::min_word_length`]。
fn default_learn_min_len() -> usize {
    2
}

/// 拼音造词最大字数默认值。比码表侧的 5 宽松——码表造词的素材是连续单字序列
/// （跨句拼接风险高），而拼音整句/分步转换的每一段都是用户明确选中的语义单元。
fn default_learn_max_len() -> usize {
    10
}

/// 手写而非 `derive(Default)`：`max_word_length`（10）与 `min_word_length`（2）的默认值不是零值，
/// derive 会让 `AutoLearnConfig::default()` 与 serde 缺键给出不同的值。
impl Default for AutoLearnConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_word_length: default_learn_min_len(),
            max_word_length: default_learn_max_len(),
            promote_count: 0,
        }
    }
}

/// 全局混输配置（[schema.mix]）。融合策略；全局唯一，无方案级 override。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MixGlobal {
    /// 显示候选来源标记。
    #[serde(default)]
    pub show_source_hint: bool,
    /// 启用英文候选。
    #[serde(default)]
    pub enable_english: bool,
    /// 超码长时仅查拼音。
    #[serde(default)]
    pub pinyin_only_overflow: bool,
    /// 顶码偏好（顶码覆盖拼音）。
    #[serde(default)]
    pub top_code_override_pinyin: bool,
    /// 满码自动上屏与顶码上屏遇拼音候选则否决（保护拼音用户）。默认开。
    ///
    /// 粗粒度一票否决：整串能查出任何拼音候选就让路，与细粒度的 `block_commit_on_pinyin_word` 任一命中
    /// 即否决（见 `pinyin_vetoes_commit`）。`top_code_commit` 出厂即开，故改动本项会直接改变混输的
    /// 顶码行为；`top_code_override_pinyin` 可无视本否决。它还兼管满码空码清空
    /// （`clear_on_empty_max`）的两道拼音守护（已有拼音候选、拼音还没打完如 zhon→zhong）。
    /// 关闭 = 拼音一律不干预码表处置。
    #[serde(default = "default_true")]
    pub auto_commit_block_on_pinyin: bool,
    /// 满码上屏遇英文候选则否决（保护正在输入英文词的用户；仅 enable_english 开时有意义）。
    /// 前缀级：有英文词以当前串开头即否决；开着时涵盖下面的成词级。
    #[serde(default)]
    pub auto_commit_block_on_english: bool,
    /// 成词级英文否决：整串恰是一个英文完整单词时否决满码 / 顶码上屏（英文临时词不算）。
    /// 默认开；与前缀级 `auto_commit_block_on_english` 的关系同拼音那对
    /// （`auto_commit_block_on_pinyin` / `block_commit_on_pinyin_word`）。
    #[serde(default = "default_true")]
    pub block_commit_on_english_word: bool,
    /// 拼音最小触发长度（0=回退 2）。
    #[serde(default)]
    pub min_pinyin_length: usize,
    /// 英文最小触发长度（0=回退 3，即 2 字符以内不查英文；预留可配）。
    #[serde(default)]
    pub min_english_length: usize,
    /// 中文侧有候选时，输入达到几码起也给英文前缀补全。0 = 不补（出厂值）。
    /// 口径同 `schema.codetable.english_merge.complete_min_length`。
    #[serde(default)]
    pub english_complete_min_length: usize,
    /// 完整单词英文最靠前排第几位（1 起；0 = 默认 1）。
    /// 口径同 `schema.codetable.english_merge.exact_position`。
    #[serde(default = "default_english_exact_position")]
    pub english_exact_position: usize,
    /// 前缀补全英文最靠前排第几位（1 起；0 = 默认 2）。
    /// 口径同 `schema.codetable.english_merge.complete_position`。
    #[serde(default = "default_english_complete_position")]
    pub english_complete_position: usize,
    /// 中文侧有候选时英文最多几条（0 = 默认 2），截断时为它们保底留座。
    /// 口径同 `schema.codetable.english_merge.max_count`。
    #[serde(default = "default_english_max_count")]
    pub english_max_count: usize,
    /// 拼音歧义拦截（词强度启发式）：整串是强拼音词时否决五笔自动/顶码上屏，让拼音赢
    /// （如 wangba→网吧；aipu 无强词则放行落实）。默认开；独立于 auto_commit_block_on_pinyin。
    #[serde(default = "default_true")]
    pub block_commit_on_pinyin_word: bool,
    /// 拼音歧义拦截的词强度权重阈值（0=仅结构判据：≥2 汉字且消费整串；预留真机调）。
    #[serde(default)]
    pub pinyin_word_min_weight: i32,
    /// 混输码长内（输入 ≤ 主码表最大码长）是否保留「未消费整串」的拼音候选。默认 `false`（丢弃）：
    /// 否则 `gedw`（五笔「青春」）下拼音会交出 `ge` 的两百多条同音单字。关掉时开着简拼仍能浮出混合简拼词；
    /// 代价是码长内没有分步上屏，正在输入中的拼音（`wanl`→「完了」）不受影响。
    #[serde(default)]
    pub pinyin_partial_candidates: bool,
    /// 混输超码长（已切入纯拼音语境）是否保留同类候选。默认 `true`（保留）：
    /// 那里正是长拼音的地盘，`nihaom` 选「你好」再续打的分步上屏要留着。
    #[serde(default = "default_true")]
    pub pinyin_partial_candidates_overflow: bool,
    /// 混输时拼音是否产出简拼候选（声母缩写，nh→你好）。出厂关：开着时超码长的顶码上屏基本不会发生
    /// （见 `data/config.toml` 同名项注释）。仅影响混输的拼音子引擎。
    /// 三处同源：serde 默认 / `Default` / `data/config.toml`，由 `mix_pinyin_abbrev_*` 测试守门。
    #[serde(default)]
    pub enable_pinyin_abbrev: bool,
}

impl Default for MixGlobal {
    fn default() -> Self {
        Self {
            show_source_hint: false,
            enable_english: false,
            // 三处同源：本处 / `MixConfig::default()`（wind-engine mixed/engine.rs）/
            // `data/config.toml [schema.mix]`，改默认须同步全部三处。
            pinyin_only_overflow: true,
            top_code_override_pinyin: false,
            auto_commit_block_on_pinyin: true,
            auto_commit_block_on_english: false,
            block_commit_on_english_word: true,
            min_pinyin_length: 0,
            min_english_length: 0,
            english_complete_min_length: 0,
            english_exact_position: default_english_exact_position(),
            english_complete_position: default_english_complete_position(),
            english_max_count: default_english_max_count(),
            block_commit_on_pinyin_word: true,
            pinyin_word_min_weight: 0,
            // 同属「三处同源」：本处 / `MixConfig::default()` / `data/config.toml`。
            pinyin_partial_candidates: false,
            pinyin_partial_candidates_overflow: true,
            // 出厂关：简拼几乎能主张任何字母串，开着时超码长归属恒判给拼音，顶码上屏基本不会发生
            // （`pinyin_only_overflow` 是隐藏项，另两个否决开关都关掉也无济于事）。
            enable_pinyin_abbrev: false,
        }
    }
}

/// 码表方案的「英文候选混入」配置（[schema.codetable.english_merge]）。
///
/// 按引擎各配一份：五笔用户常打命令行、变量名，全拼的英文词与音节大面积重叠，取舍不同。
/// 拆开后 [`Self::block_commit`] 只出现在码表这份（拼音没有满码 / 顶码），设置页的两个键也能各归其分区。
///
/// 混输（`[schema.mix]` 的 `enable_english` / `min_english_length` / `auto_commit_block_on_english`）
/// 是第三份，互不接管：它们与 `MixedEngine::truncation_tier` 的档位仲裁耦合，混输方案不读本段，
/// 否则两套英文各混一遍、档位与配额双重失真。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodetableEnglishMerge {
    /// 在候选列表里混入英文词库候选（中文侧有候选时只收精确命中，按
    /// [`Self::complete_min_length`] 才补前缀）。出厂关。
    #[serde(default)]
    pub enable: bool,
    /// 最小触发长度（0=回退 3，即 2 字符以内不查英文）。口径同 `schema.mix.min_english_length`。
    #[serde(default)]
    pub min_length: usize,
    /// 存在英文候选时否决满码自动上屏 / 顶码上屏 / 满码空码清空。默认开。前缀级：有英文词以当前串
    /// 开头即否决（`english_merge::has_any`），开着时涵盖成词级 [`Self::block_commit_on_word`]。
    /// 只看精确命中的话，打 `github` 到第 4 键 `gith` 就会被满码顶出中文。
    ///
    /// 默认值与 `schema.mix.auto_commit_block_on_english`（默认关）刻意相反：混输以码表为主、顶码要可用；
    /// 本段是用户显式打开的能力，不否决等于白开。
    #[serde(default = "default_true")]
    pub block_commit: bool,
    /// 成词级否决：整串恰是一个英文完整单词时否决满码 / 顶码上屏 / 满码空码清空（英文
    /// 临时词不算）。默认开。前缀级 [`Self::block_commit`] 关掉时，用它保住「打完一个英文词
    /// 不被顶走」，又不让英文词的前缀拦住五笔满码。两级关系同混输拼音那对。
    #[serde(default = "default_true")]
    pub block_commit_on_word: bool,
    /// 中文侧有候选时，输入达到几码起也给英文前缀补全（`gith` → github）。
    /// 0 = 不补，只在中文侧为空时补（出厂值）。
    #[serde(default)]
    pub complete_min_length: usize,
    /// 完整单词英文最靠前排第几位（1 起；0 = 默认 1）。仍先让过开头连续的完整匹配中文，
    /// 只会更靠后。由协调器 `place_english_after_common_exact` 应用。
    #[serde(default = "default_english_exact_position")]
    pub exact_position: usize,
    /// 前缀补全英文最靠前排第几位（1 起；0 = 默认 2）。同样先让过开头的完整匹配中文，且恒排在
    /// 完整单词英文之后。
    #[serde(default = "default_english_complete_position")]
    pub complete_position: usize,
    /// 中文侧有候选时英文最多几条（完整单词 + 补全合计，完整单词优先；0 = 默认 2）。
    /// 中文侧为空时不受本项约束（给一页前缀补全）。
    #[serde(default = "default_english_max_count")]
    pub max_count: usize,
}

/// 完整单词英文最靠前位次的出厂值（码表 / 混输两份同值）。
pub fn default_english_exact_position() -> usize {
    1
}

/// 前缀补全英文最靠前位次的出厂值（码表 / 混输两份同值）。
pub fn default_english_complete_position() -> usize {
    2
}

/// 中文侧有候选时英文条数上限的出厂值（码表 / 混输两份同值）。
fn default_english_max_count() -> usize {
    2
}

impl Default for CodetableEnglishMerge {
    fn default() -> Self {
        Self {
            enable: false,
            min_length: 0,
            // 见字段文档：这一项与 `schema.mix.auto_commit_block_on_english` 默认值相反是刻意的。
            block_commit: true,
            block_commit_on_word: true,
            complete_min_length: 0,
            exact_position: default_english_exact_position(),
            complete_position: default_english_complete_position(),
            max_count: default_english_max_count(),
        }
    }
}

/// 拼音方案的「英文候选混入」配置（[schema.pinyin.english_merge]）。
///
/// 与 [`CodetableEnglishMerge`] 是两件独立的事，理由见该结构文档。刻意没有 `block_commit`：
/// 拼音方案没有满码自动上屏与顶码上屏。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinyinEnglishMerge {
    /// 在候选列表里混入英文词库候选（只收精确命中，前缀扩展不入列）。出厂关。
    #[serde(default)]
    pub enable: bool,
    /// 最小触发长度（0=回退 3，即 2 字符以内不查英文）。
    #[serde(default)]
    pub min_length: usize,
    /// 中文侧有候选时，输入达到几个字母起也补英文前缀（不与精确命中重复）。
    /// 0 = 只在中文侧为空时补（出厂值）；开时建议 5。口径同码表的同名项，另加一条拼音判据：
    /// 整串任何切法都切不成完整的全拼音节序列时才补（`templa` 补 template，`shang` 不补），
    /// 见 `wind_engine::english_merge::prefix_complete_applies`。双拼方案下不生效（几乎任何串都解得开）。
    #[serde(default)]
    pub complete_min_length: usize,
    /// 完整单词英文最靠前排第几位（1 起）。0 = 按方案自动：全拼 1、双拼 2（出厂值）；非 0 则都用它。
    /// 仍先让过开头吃满整串的中文。双拼默认让一位：两键一音节，`the` / `good` 这类常用词几乎都解得出中文。
    #[serde(default)]
    pub exact_position: usize,
    /// 前缀补全英文最靠前排第几位（1 起；0 = 默认 2）。恒在完整单词英文之后。
    #[serde(default = "default_english_complete_position")]
    pub complete_position: usize,
    /// 中文侧有候选时英文最多几条（完整单词 + 补全合计，完整单词优先；0 = 默认 2）。
    /// 中文侧为空时不受本项约束（给一页前缀补全）。
    #[serde(default = "default_english_max_count")]
    pub max_count: usize,
}

impl Default for PinyinEnglishMerge {
    fn default() -> Self {
        Self {
            enable: false,
            min_length: 0,
            complete_min_length: 0,
            exact_position: 0,
            complete_position: default_english_complete_position(),
            max_count: default_english_max_count(),
        }
    }
}

/// 全局模糊音（[schema.pinyin.fuzzy]）。字段对齐引擎 FuzzyConfig。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinyinFuzzy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub zh_z: bool,
    #[serde(default)]
    pub ch_c: bool,
    #[serde(default)]
    pub sh_s: bool,
    #[serde(default)]
    pub n_l: bool,
    #[serde(default)]
    pub f_h: bool,
    #[serde(default)]
    pub r_l: bool,
    #[serde(default)]
    pub an_ang: bool,
    #[serde(default)]
    pub en_eng: bool,
    #[serde(default)]
    pub in_ing: bool,
    #[serde(default)]
    pub ian_iang: bool,
    #[serde(default)]
    pub uan_uang: bool,
    #[serde(default)]
    pub un_ong: bool,
    #[serde(default)]
    pub eng_ong: bool,
}

/// 拼音纠错与正确拼音提示（`[input.correction]`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CorrectionConfig {
    /// 纠错总开关。
    pub enabled: bool,
    /// 纠错候选的标记，加在候选前（留空 = 不加）。
    pub marker: String,
    /// 内置纠错规则开关。
    pub rules: CorrectionRuleToggles,
    /// 自定义纠错，每项 `错误=正确`。
    pub custom: Vec<String>,
    /// 正确拼音提示。
    pub hint: PinyinHintConfig,
}

impl Default for CorrectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            marker: "*".to_string(),
            rules: CorrectionRuleToggles::default(),
            custom: Vec::new(),
            hint: PinyinHintConfig::default(),
        }
    }
}

/// 内置纠错规则开关（`[input.correction.rules]`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CorrectionRuleToggles {
    pub gn_ng: bool,
    pub on_ong: bool,
    pub vowel_swap: bool,
    pub legacy_spelling: bool,
    pub h_misplaced: bool,
    pub mg_ng: bool,
    pub n_before_g: bool,
    pub ai_ia: bool,
}

impl Default for CorrectionRuleToggles {
    fn default() -> Self {
        Self {
            gn_ng: false,
            on_ong: true,
            vowel_swap: true,
            legacy_spelling: false,
            h_misplaced: false,
            mg_ng: false,
            n_before_g: false,
            ai_ia: false,
        }
    }
}

/// 正确拼音提示（`[input.correction.hint]`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PinyinHintConfig {
    /// 候选正文内嵌正确全拼（「苹(ping)果」）的总开关。
    pub enabled: bool,
    /// 模糊音命中时提示。
    pub fuzzy: bool,
    /// 易读错的字提示。
    pub misread: bool,
    /// 纠错命中时提示。
    pub correction: bool,
    /// 提示范围：first = 只标第一个带提示的候选；all = 标全部。
    #[serde(deserialize_with = "crate::tolerant_de::tolerant")]
    pub scope: HintScope,
    /// 提示带声调（「苹(píng)果」），内嵌提示与注释变量 `${correct_pinyin}` 共用。
    pub tone: bool,
}

impl Default for PinyinHintConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            fuzzy: true,
            misread: true,
            correction: false,
            scope: HintScope::First,
            tone: false,
        }
    }
}

/// 提示范围（`[input.correction.hint] scope`）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HintScope {
    /// 只标候选列表里第一个带提示的候选。
    #[default]
    First,
    /// 每个带提示的候选都标。
    All,
}

impl HintScope {
    pub const VALUES: &'static [&'static str] = &["first", "all"];
}

/// 快捷输入的全局行为配置。
///
/// 各候选来源的开关与优先级不在这里，它们是 `mix_modes.members` 的成员（`quick_input.calc` /
/// `.date` / `.number` / `.repeat` 与 `$primary_pinyin` / `english`），开关即增删、优先级即排序。
/// 没有总开关：清空 `quick_mix` 的 `trigger_keys` 即禁用。旧的 `enable_english` 与 `force_vertical`
/// 已在加载期迁移（后者迁到 [`MixModeConfig::candidate_layout`]，见 docs/design/mode-candidate-layout.md）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuickInputConfig {
    /// 计算器结果小数位数，默认 6
    #[serde(default = "default_decimal_places")]
    pub decimal_places: i32,
    /// 快捷输入历史（成员 `quick_input.history`）的条数上限，默认 1000；`0` = 不限。
    /// 超出时按「次数少、用得旧」裁掉，与 `input.url.history_max` 同一套裁剪；不做按时间过期。
    #[serde(default = "default_quick_history_max")]
    pub history_max: u32,
}

fn default_decimal_places() -> i32 {
    6
}

fn default_quick_history_max() -> u32 {
    1000
}

impl Default for QuickInputConfig {
    fn default() -> Self {
        Self {
            decimal_places: default_decimal_places(),
            history_max: default_quick_history_max(),
        }
    }
}

// ───────────────────────── 模式级显示属性（多模式共用）─────────────────────────

/// 模式级候选布局意图（设计见 docs/design/mode-candidate-layout.md）。
///
/// - `Follow`：跟随全局 `ui.candidate.layout`。
/// - `Vertical` / `Horizontal`：进入该模式期间覆盖全局方向，退出自动回到全局。
///
/// 与 `ui.candidate.layout` 共用取值词汇。不用布尔：`Follow` 与 `Vertical` 只在全局竖排时无区别，
/// 布尔也表达不了「全局竖排但本模式横排」。旋转 / 文字竖排是另一根轴（[`TextOrientation`]）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LayoutIntent {
    #[default]
    Follow,
    Vertical,
    Horizontal,
}

/// 横排时文字的排列方式（方案级 `[candidate] text_orientation`）。
///
/// | 取值 | 屏幕上 | 给谁用 |
/// |---|---|---|
/// | `Normal` | 一行候选，文字水平 | 绝大多数方案 |
/// | `Rotated` | 候选成列自左向右，整项顺时针转 90° | 蒙古文等连写的纵向书写脚本 |
/// | `Upright` | 同上，但字不转、逐字下行 | 汉字的对联式竖排 |
///
/// 术语取自 CSS `text-orientation: sideways \| upright`，`Rotated` 对应 sideways。
///
/// 独立一根轴而非 [`LayoutIntent`] 的取值：注释模板、窗口尺寸下限、`flip_when_above` 都只按横竖
/// 成对；面向所有用户的布局下拉不该混进极少数方案才用的选项；合成一根轴的话 `ime.toggle("layout")`
/// 切横竖会连旋转一起丢。竖排时本轴一律按 `Normal` 处理，归一化只在 [`Orientation::normalized`] 一处做。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextOrientation {
    /// 文字水平（出厂）。
    #[default]
    Normal,
    /// 整项顺时针旋转 90°。
    Rotated,
    /// 整项旋转，但每个字逆时针扶正、逐字下行（对联式）。按字切单元，会切断连写脚本的字形连接，
    /// 那些脚本要用 [`Self::Rotated`]。
    Upright,
}

impl TextOrientation {
    /// 配置里的字符串取值；未知值回落 `Normal`（配置文件是用户手写的，不 panic）。
    pub fn from_str_or_normal(s: &str) -> Self {
        if s.eq_ignore_ascii_case("rotated") {
            Self::Rotated
        } else if s.eq_ignore_ascii_case("upright") {
            Self::Upright
        } else {
            Self::Normal
        }
    }

    /// 回写用字符串。与 [`Self::from_str_or_normal`] round-trip。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Rotated => "rotated",
            Self::Upright => "upright",
        }
    }

    /// 供设置页/文档枚举与测试穷举。
    pub const ALL: &'static [TextOrientation] = &[Self::Normal, Self::Rotated, Self::Upright];
}

/// 候选呈现方向：竖排位 + 横排时的文字排列方式。
///
/// `vertical` 是用户随时可切的呈现偏好，`text` 是方案声明的「这套文字怎么写」，切前者不动后者。
/// 唯一的耦合是竖排时 `text` 不生效，归一化只在 [`Self::normalized`] 一处做；读侧一律经
/// [`Self::rotated`] / [`Self::upright`] 取值，不要自己判 `!vertical && text != Normal`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Orientation {
    /// 候选纵向堆叠（屏幕空间）。
    pub vertical: bool,
    /// 横排时文字的排列方式。竖排时无意义，见 [`Self::normalized`]。
    pub text: TextOrientation,
}

impl Orientation {
    pub const HORIZONTAL: Self = Self {
        vertical: false,
        text: TextOrientation::Normal,
    };
    pub const VERTICAL: Self = Self {
        vertical: true,
        text: TextOrientation::Normal,
    };
    /// 横排 + 整项旋转 90°。
    pub const ROTATED: Self = Self {
        vertical: false,
        text: TextOrientation::Rotated,
    };
    /// 横排 + 文字直立的竖排（对联式）。
    pub const UPRIGHT: Self = Self {
        vertical: false,
        text: TextOrientation::Upright,
    };

    /// 竖排时把文字排列归零。下发 UI 前必须过一次——渲染端的
    /// `list_vertical` 同时看两位，`vertical && rotated` 会让列表既按竖排堆叠又被转 90°。
    pub fn normalized(self) -> Self {
        if self.vertical { Self::VERTICAL } else { self }
    }

    /// 渲染端要的「整个列表转 90°」位。竖排时恒 false。
    pub fn rotated(self) -> bool {
        !self.vertical && self.text != TextOrientation::Normal
    }

    /// 渲染端要的「每个字扶正」位。蕴含 [`Self::rotated`]。
    pub fn upright(self) -> bool {
        !self.vertical && self.text == TextOrientation::Upright
    }

    /// 解析 `ui.candidate.layout`；未知值按横排（出厂行为）。只认横竖两个值：旋转/直立是方案属性，
    /// 混进来会让 `ime.toggle("layout")` 的写回覆盖方案意图。
    pub fn from_layout_str(s: &str) -> Self {
        if s.eq_ignore_ascii_case("vertical") {
            Self::VERTICAL
        } else {
            Self::HORIZONTAL
        }
    }

    /// 回写 `ui.candidate.layout` 用的字符串，只反映 `vertical`，于是切横竖不会碰到方案声明的文字排列。
    pub fn layout_str(self) -> &'static str {
        if self.vertical {
            "vertical"
        } else {
            "horizontal"
        }
    }
}

/// 自由输入（字面输入）模式：让 mix 能打出 `GetTestData()` / `test_data` / `<TAB>` 这类任何 member
/// 都无法接受的内容。
///
/// - `Off`：越界字符仍走「顶屏候选 + 上屏标点 + 退出」，没有 Free 透镜。例外：含英文成员的实例里
///   Shift+字母照样进缓冲、候选只给英文段，但数字 / 符号键仍是功能键（`MixLens::English`）；
///   下面第二条判据同样适用。
/// - `Auto`（默认）：由缓冲内容自动推导，见 `MixLens`。
/// - `Always`：本实例恒为自由输入，用于专做字面输入的融合模式。
///
/// 没有切换键：mix 里几乎每个键都已双重占用。判据落在输入内容本身，两条：
/// 1. 字符不在当前透镜的合法字符集内，就只能是字面内容；
/// 2. 一个候选都没有时，选词键也归字面（否则 `.txt` 这类本透镜合法却换不来候选的输入打不出）。
///    快捷输入历史的候选不算数，只有历史命中时选词键同样归字面。
///
/// 是缓冲的纯函数而非粘滞状态位：没有进入键就找不到清除时机；退格删掉越界字符即回到原透镜。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FreeInputMode {
    Off,
    #[default]
    Auto,
    Always,
}

/// 临时 mix 模式配置（overlay 激活面）。触发后对每个成员方案查询并按成员序合并候选。
///
/// `Default` 手写而非 derive：`free_input_takes_select_keys` 的 serde 缺省是 `true`，derive 会给出
/// `false`，测试夹具与真实配置就此分叉。新增带非零默认值的字段时必须同时改这里。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MixModeConfig {
    /// 实例唯一标识
    #[serde(default)]
    pub id: String,
    /// 显示名（UI 徽标 / 模式指示全称）
    #[serde(default)]
    pub name: String,
    /// 模式指示短称（空则取 name 首字）
    #[serde(default)]
    pub short_name: String,
    /// 引导键列表
    #[serde(default)]
    pub trigger_keys: Vec<String>,
    /// 成员列表：候选来源的单一真相源——有无即开关，顺序即优先级。
    ///
    /// 三类取值：
    /// - 真实方案 id（`"pinyin"` / `"english"` / 码表方案…），经其 `.schema.toml` 加载；
    /// - 占位符 [`MIX_MEMBER_PRIMARY_PINYIN`]（解析为 `schema.primary_pinyin`）；
    ///   字面 id 一律精确解释（`"pinyin"` 恒为全拼，永不被替换）；
    /// - 快捷输入内置来源（`wind_quick_input::MEMBER_*`：`quick_input.calc` / `.date` /
    ///   `.number` / `.repeat`），无对应方案文件，由协调器直接产出候选。
    ///   旧的合并值 `"quick_input"` 在加载期展开为这四项。
    #[serde(default)]
    pub members: Vec<String>,
    /// 进入本 mix 期间的候选布局（默认跟随全局）。每实例独立——两个融合模式可以
    /// 一个竖排一个横排。旧的 `schema.quick_input.force_vertical` 已迁移到这里
    /// （它本就是 quick_mix 这个实例的属性，却被存在了与实例无关的全局段里）。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub candidate_layout: LayoutIntent,
    /// 自由（字面）输入，见 [`FreeInputMode`]。默认 `Auto`。
    ///
    /// 每实例独立：一个只做日期/计算的 mix 可以关掉它，保住「打完拼音按逗号顶屏出
    /// 中文标点」；专做字面输入的实例则设 `Always`。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub free_input: FreeInputMode,
    /// 自由输入是否夺取二三候选键（`keys.select_key_groups` 的键，默认 `;` `'`）作字面输入。
    /// 默认开；`free_input = "off"` 时无意义。
    ///
    /// 文本透镜下选词键与字面字符是同一批键的两种解释。`rock'n'roll` / `for(;;)` 里的 `'` `;`
    /// 恰是默认选词键，不夺取就会先被 `select_key_offset` 吃掉。夺取无能力损失：它们只是数字键
    /// `2`/`3` 的别名。数字键 1-9 不在夺取范围（文本透镜唯一的选词通路），代价是 `utf8` 这类
    /// 「纯小写 + 数字」在有候选时仍需先打大写或符号切进自由输入（无候选时选词键本就让位，见 [`FreeInputMode`]）。
    ///
    /// 独立开关而非跟随 `free_input`：翻页键有 PageUp/PageDown 替代，让位是纯收益；选词键的取舍
    /// 因人而异，习惯 `;`/`'` 选词的用户可单独关掉。
    #[serde(default = "default_true")]
    pub free_input_takes_select_keys: bool,
    /// 数字透镜（首字符是数字/符号）下是否拿小写字母作选词键（a=首选）。默认开。
    ///
    /// 关掉后字母在数字透镜里是字面输入，`2fa`、`1.5x` 可以直接打进缓冲；序号标签随之
    /// 改回数字，选词靠 `keys.quick_input_select`（出厂 Ctrl+数字）与空格。
    #[serde(default = "default_true")]
    pub numeric_letter_select: bool,
}

impl Default for MixModeConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            short_name: String::new(),
            trigger_keys: Vec::new(),
            members: Vec::new(),
            candidate_layout: LayoutIntent::default(),
            free_input: FreeInputMode::default(),
            // 与 `#[serde(default = "default_true")]` 对齐，见结构体文档。
            free_input_takes_select_keys: default_true(),
            numeric_letter_select: default_true(),
        }
    }
}

/// 内置「快捷」融合 mix 的实例 id（`;` 触发，成员含日期/计算/拼音/英文）。
/// 设置页只暴露其 trigger_keys；其余字段（尤其 members）为内置默认值。
pub const QUICK_MIX_ID: &str = "quick_mix";

/// mix 成员占位符：解析期替换为 `schema.primary_pinyin`（空=全拼 "pinyin"）。
/// 内置「快捷」默认成员用它，使快捷输入的拼音跟随主拼音方案（双拼用户得双拼）。
/// 与字面 `"pinyin"` 严格区分——后者表示"就要全拼"，永不被替换。
pub const MIX_MEMBER_PRIMARY_PINYIN: &str = "$primary_pinyin";

/// mix 成员占位符：生僻字候选（用当前活跃方案的编码查询，只留生僻字）。
///
/// 与 [`MIX_MEMBER_PRIMARY_PINYIN`] 同为 `$` 前缀占位符：它指向「此刻的活跃方案 + 一道过滤」，
/// 无法写成字面方案 id。它不是真实方案，`ensure_schema` 对它必然失败，故 `mix_members` 要像排除
/// `quick_input.*` 那样排除它，进入门卫则要单独认它。
pub const MIX_MEMBER_RARE_CHAR: &str = "$rare_char";

/// 主拼音方案缺省回退（`schema.primary_pinyin` 为空时的目标方案）。
/// 固定全拼，不扫描 available——避免方案列表顺序静默改变拼音行为。
pub const DEFAULT_PINYIN_SCHEMA: &str = "pinyin";

fn default_mix_modes() -> Vec<MixModeConfig> {
    let mut members: Vec<String> = wind_quick_input::LEGACY_EXPANSION
        .iter()
        .map(|s| s.to_string())
        .collect();
    members.push(MIX_MEMBER_PRIMARY_PINYIN.to_string());
    members.push("english".to_string());
    vec![MixModeConfig {
        id: QUICK_MIX_ID.to_string(),
        name: "快捷".to_string(),
        short_name: "快".to_string(),
        // 出厂引导键刻意放在这里而非 keys.key_actions：`normalize` 会把它折算进 key_actions，默认值
        // 留在被折算的一侧，「清空」才能折算出空；写进 key_actions 的话清空与「从没配过」同形。
        // 见 docs/design/schema-key-actions.md 五c。
        trigger_keys: vec!["semicolon".to_string()],
        members,
        // 出厂强制竖排：快捷输入的候选是日期/算式结果等长文本，横排放不下。
        // 与旧 data/config.toml 的 `quick_input.force_vertical = true` 行为一致
        // （mix_modes 不能写进预置文件，故默认值只能落在这里，见 §迁移）。
        candidate_layout: LayoutIntent::Vertical,
        // 出厂开自动自由输入：`;` 是为特殊内容而进的模式，字面输入是它的常见用途。
        free_input: FreeInputMode::Auto,
        // 夺取 `;`/`'` 作字面：它们选第 2/3 候选只是数字键的冗余别名，而 `rock'n'roll`
        // 这类内容里的 `'` 没有别的输入通路。
        free_input_takes_select_keys: true,
        numeric_letter_select: true,
    }]
}

// `SpecialModeConfig` 已删除。特殊模式的实例集合改由带 `[overlay]` 段的已安装方案定义
// （`EngineManager::overlay_modes`），引导键与直达热键落 `keys.key_actions`（`special:<方案id>`）。
// 见 `docs/redesign/overlay-mode-config.md`；残留旧配置的告警见 `warn_legacy_special_modes`。
// ───────────────────────── input（输入行为）─────────────────────────

/// 「检索范围」智能档的放宽增强（设计见 `docs/design/smart-filter-scope-relax.md`）。
///
/// 智能档会滤掉同码位有常用字的生僻字，唯一编码被占的生僻字（如五笔「桜」sivg 与「档」同码）
/// 因而彻底打不出。PC 上出路只有一条：候选窗内向后翻页翻到底再按一次即临时放宽；
/// 刻意不做自动补充，那会改变智能档的既有观感。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeRelaxConfig {
    /// 末页再按向后翻页键即临时放宽为「全部字符」，本次组合结束后自动恢复。
    /// 三类引擎通用；候选不足一页时同样适用。
    #[serde(default = "default_true")]
    pub page_end_key: bool,
    /// 放宽放出来的候选的前缀标注（空=不标注），用于与正常候选区分。
    #[serde(default = "default_scope_relax_prefix")]
    pub prefix: String,
    /// 智能档下始终把被滤掉的候选追加在列表末尾（不必先翻到底再按翻页键）。
    ///
    /// 给没有翻页键的触屏端用，PC 出厂关。追加的候选与翻页放宽同源：带 `is_scope_filtered`、沉底、
    /// 不计入满码自动上屏、不参与词频重排。只在智能档生效，辅助码模式不适用。
    #[serde(default)]
    pub always_append: bool,
}

impl Default for ScopeRelaxConfig {
    fn default() -> Self {
        Self {
            page_end_key: true,
            prefix: default_scope_relax_prefix(),
            always_append: false,
        }
    }
}

fn default_scope_relax_prefix() -> String {
    "·".to_string()
}

/// 联想（`[input.association]`）：上屏之后按上文推荐下一个词或标点。
///
/// 与普通候选的根本差别在输入源——普通候选的输入是编码缓冲，联想的输入是刚上屏的文本。
/// 候选生成在 `wind-assoc`，何时展示/退出在协调器 `handle_assoc.rs`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssociationConfig {
    /// 哪一种联想：`"off"`（默认）/ `"word"` / `"smart"`。
    ///
    /// - `word` 词语联想：上文当前缀，出以它开头的更长的词（「中」→「国」「间」），不含标点。
    /// - `smart` 智能联想：上文当上下文，出下一个可能的词与标点，移动端刚需。
    ///
    /// 开关与类型合并成一个字段，避免「开着但类型没配」的歧义态。本段是桌面基线，移动端差异走
    /// [`MobileAssociationConfig`]，不要加 `"auto"` 之类的平台哨兵。
    #[serde(default = "default_assoc_kind")]
    pub kind: String,
    /// `"one_shot"`（默认）/ `"continuous"`。
    ///
    /// `one_shot` = 只出一次，任何非选词动作即退出；`continuous` = 选中之后拿它当新上文
    /// 接着给。桌面默认一次性：候选窗是浮层，常驻会挡住正文。
    #[serde(default = "default_assoc_mode")]
    pub mode: String,
    /// 候选总数上限。各源的配额由它按优先级分配，配额本身不开放给用户——那是调参项，
    /// 而用户没有评测手段。
    #[serde(default = "default_assoc_max_count")]
    pub max_count: usize,
    /// 空格是否上屏当前高亮的联想候选。主流输入法多数如此，故默认开；关掉则空格照常出空格
    /// （联想窗同时收起）。
    #[serde(default = "default_true")]
    pub space_commits: bool,
    /// 二三候选键（`keys.select_key_groups` / `session_actions` 里 `select_candidate:N` 的可打印键，
    /// 出厂 `;` `'`）在联想态是否选联想候选。
    ///
    /// `true`（默认）= 照常选第 2/3 条。`false` = 联想态下让出：收起联想窗，该键按「联想窗不在」处理
    /// （对标 QQ 五笔：按 `;` 想打的是分号）。只管 keydown 消费的可打印键；修饰键组（`lrshift` /
    /// `lrctrl`）与数字键不受影响。
    #[serde(default = "default_true")]
    pub select_keys_commit: bool,
    /// 联想态按回车：只收窗（吃键），还是收窗 + 把回车透传给宿主。
    ///
    /// `false`（默认）= 透传，联想窗收起同时照常换行 / 发送；`true` = 回车被吃掉。
    /// 默认透传：回车是终结性动作，不该让输入法自己弹出的建议挡住。
    #[serde(default)]
    pub enter_cancels_only: bool,
    /// 联想态按退格：只收窗（吃键），还是收窗 + 把退格透传给宿主。
    ///
    /// 语义与 [`Self::enter_cancels_only`] 对称，但默认相反（`true` = 吃键）：退格透传是删掉刚上屏的
    /// 字，不可逆，而联想窗弹出时手正停在刚打的字上，误触代价大。
    #[serde(default = "default_true")]
    pub backspace_cancels_only: bool,
    /// 联想窗自动隐藏的毫秒数；`0` = 不自动隐藏。
    ///
    /// 联想态下候选窗几乎一直挂着，长时间停留会挡住正文。主流输入法的做法是显示几秒后
    /// 自行淡出，用户若要用就在这几秒内按数字。
    #[serde(default = "default_assoc_hide_after_ms")]
    pub hide_after_ms: u64,
    /// 联想态在编码栏显示的标识；空串 = 不显示。只在候选窗自绘编码栏时可见。
    /// 联想候选与普通候选长得一样，没有标识时用户分不清自己是不是还在打字。
    #[serde(default = "default_assoc_hint")]
    pub hint: String,
    /// 个人上屏历史学到的搭配。
    #[serde(default = "default_true")]
    pub history: bool,
    /// 词→后继表（离线从 n-gram 模型蒸馏）。
    #[serde(default = "default_true")]
    pub bigram: bool,
    /// 码表词的文本前缀延伸（「北京」→「大学」）。
    #[serde(default = "default_true")]
    pub prefix: bool,
    /// 标点与符号（静态规则表打底）。
    ///
    /// 桌面默认关（移动端在 [`MobileAssociationConfig::punct`] 里开）：实体键盘上标点一键可达，
    /// 打完一个字就弹一串标点干扰远大于收益；软键盘上打标点要切层，从候选里点更省事。
    #[serde(default)]
    pub punct: bool,
}

impl Default for AssociationConfig {
    fn default() -> Self {
        Self {
            kind: default_assoc_kind(),
            mode: default_assoc_mode(),
            max_count: default_assoc_max_count(),
            space_commits: true,
            select_keys_commit: true,
            enter_cancels_only: false,
            backspace_cancels_only: true,
            hide_after_ms: default_assoc_hide_after_ms(),
            hint: default_assoc_hint(),
            history: true,
            bigram: true,
            prefix: true,
            punct: false,
        }
    }
}

// ──────────────── mobile（移动端覆盖域）────────────────

/// 取配置时套哪一层覆盖：`Base` = 只看基线段；`Mobile` = 再叠 `[mobile.*]`。
///
/// 由宿主在构造协调器时经 `InputProfile` 选定，不再由编译目标决定——这样主机上的
/// 测试也能走到移动端那一支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigOverlay {
    #[default]
    Base,
    Mobile,
}

/// `[mobile]`：移动端对桌面基线的覆盖。桌面构建完全无视本域。
///
/// 少数配置项的最优值在两个平台上不同（如联想）。不用值域哨兵（`kind = "auto"`，设置页被迫列一个
/// 语义空洞的选项），也不靠两份预置文件各写各的（安卓的是无守门测试的手工副本，同步时会被覆盖）；
/// 本域让一份文件显式说清两个平台的差异。
///
/// 只登记真有平台差异的键，没出现的键沿用基线：每登记一个都要在 REGISTRY、预置文件、
/// capability 快照、设置页豁免名单里各占一行。字段用 `String` 而非 `Option<String>`：
/// `None` + `skip_serializing_if` 会让本段序列化成空表、自己变成叶子键，注册表守门测试都得开特例。
///
/// 覆盖不在 [`Config::load`] 里合并，而在消费点由宿主决定是否取用（协调器 `handle_assoc.rs`）：
/// 合并进 `input.*` 的话，移动端设置页一保存就把移动端的值写进了桌面基线。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MobileConfig {
    #[serde(default)]
    pub association: MobileAssociationConfig,
    #[serde(default)]
    pub grammar: MobileGrammarConfig,
}

/// `[mobile.grammar]`：拼音整句语法模型的移动端取值。字段语义见 [`PinyinGrammar`]。
///
/// 移动端默认开随包的 `wanxiang-mini.gram`（挑法见 `language-model-integration.md` §10–§11）：
/// 软键盘上整句打错要逐字改，代价比桌面高。桌面基线仍关。覆盖同样不在 `Config::load` 里合并，
/// 由引擎按平台挑（[`Config::pinyin_effective`]）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileGrammarConfig {
    /// 总开关，语义见 [`PinyinGrammar::enabled`]。
    #[serde(default = "default_grammar_enabled")]
    pub enabled: bool,
    #[serde(default = "default_mobile_grammar_weight")]
    pub weight: f64,
    #[serde(default = "default_mobile_grammar_model")]
    pub model: String,
}

impl Default for MobileGrammarConfig {
    fn default() -> Self {
        Self {
            enabled: default_grammar_enabled(),
            weight: default_mobile_grammar_weight(),
            model: default_mobile_grammar_model(),
        }
    }
}

fn default_mobile_grammar_weight() -> f64 {
    0.5
}

fn default_mobile_grammar_model() -> String {
    "wanxiang-mini.gram".to_string()
}

/// `[mobile.association]`：联想的移动端取值。字段语义见 [`AssociationConfig`] 同名字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileAssociationConfig {
    /// 移动端 `"smart"`：软键盘上每多打一个字都很贵，连标点带下一个词一起猜才划算。
    #[serde(default = "default_mobile_assoc_kind")]
    pub kind: String,
    /// 移动端 `"continuous"`：软键盘上方的联想栏本就常驻，多显示一行没有额外遮挡成本。
    #[serde(default = "default_mobile_assoc_mode")]
    pub mode: String,
    /// 移动端开标点联想：软键盘上打标点要切键盘层，从候选里点走省事得多。
    /// 桌面相反（见 [`AssociationConfig::punct`]）——实体键盘上标点一键可达。
    #[serde(default = "default_true")]
    pub punct: bool,
    /// 移动端关：联想态空格出空格、收起联想，不选首个联想；汉字后双击空格出「。」也靠它。
    /// 桌面相反（[`AssociationConfig::space_commits`] 默认开）：手机上联想挤在候选条里，
    /// 空格选走它就是「想打空格却上了个词」。
    #[serde(default)]
    pub space_commits: bool,
    /// 移动端关：联想态的二三候选键（`;` `'`）不选联想，照常出标点。
    /// 手机上选联想只靠点候选；这两个键在软键盘上就是普通标点键，不该被联想截走。
    /// 桌面相反（[`AssociationConfig::select_keys_commit`] 默认开）。
    #[serde(default)]
    pub select_keys_commit: bool,
    /// 移动端关（= 透传）：联想态按回车收起联想并照常换行/发送。语义同
    /// [`AssociationConfig::enter_cancels_only`]。
    #[serde(default)]
    pub enter_cancels_only: bool,
    /// 移动端关（= 透传）：联想态按退格收起联想并照常删字。桌面默认吃键
    /// （[`AssociationConfig::backspace_cancels_only`]）；手机上退格就是要删字，吃掉一次成了「删不掉」。
    #[serde(default)]
    pub backspace_cancels_only: bool,
}

impl Default for MobileAssociationConfig {
    fn default() -> Self {
        Self {
            kind: default_mobile_assoc_kind(),
            mode: default_mobile_assoc_mode(),
            punct: true,
            space_commits: false,
            select_keys_commit: false,
            enter_cancels_only: false,
            backspace_cancels_only: false,
        }
    }
}

fn default_mobile_assoc_kind() -> String {
    "smart".to_string()
}

fn default_mobile_assoc_mode() -> String {
    "continuous".to_string()
}

fn default_assoc_kind() -> String {
    "off".to_string()
}

fn default_assoc_mode() -> String {
    "one_shot".to_string()
}

fn default_assoc_max_count() -> usize {
    9
}

fn default_assoc_hide_after_ms() -> u64 {
    5000
}

fn default_assoc_hint() -> String {
    "联想输入".to_string()
}

/// `single_char[:<载荷>]` 按键动词的载荷（[`BoundAction::SingleChar`] /
/// [`SessionAction::SingleChar`] 共用）。
///
/// 载荷：`on` / `off` / `toggle` / `follow`；裸 `single_char` 等价于 `:toggle`。
/// `follow` = 清掉临时态、回落配置层，与方案文件里「不写＝跟随全局」同义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleCharAction {
    /// 直达指定状态（`on` / `off`）。
    Set(bool),
    /// 在开 / 关之间切换（裸 `single_char` 与 `single_char:toggle`）。
    ///
    /// 无法由 `Set` 组合出来：一个键要既能开又能关，只有 toggle 表达得了。
    Toggle,
    /// 清掉临时态，回到配置层（`follow`）。`Toggle` 永远回不到「没有临时态」，三者都不冗余。
    Follow,
}

impl SingleCharAction {
    /// 解析 `single_char:` 之后的那一段；未知载荷返回 `None`（⇒ `BoundAction::None`），
    /// 不回落某个状态：写错的动词不静默变成别的功能。
    pub fn parse_payload(rest: &str) -> Option<Self> {
        match rest.trim().to_ascii_lowercase().as_str() {
            "on" | "true" => Some(Self::Set(true)),
            "off" | "false" => Some(Self::Set(false)),
            "toggle" => Some(Self::Toggle),
            "follow" => Some(Self::Follow),
            _ => None,
        }
    }

    /// 配置值（不含 `single_char:` 前缀）。与 [`Self::parse_payload`] 成对——设置页要能
    /// 把用户选的动作写回配置文本。
    pub fn as_payload(&self) -> &'static str {
        match self {
            Self::Set(true) => "on",
            Self::Set(false) => "off",
            Self::Toggle => "toggle",
            Self::Follow => "follow",
        }
    }
}

/// `comment_state[:<载荷>]` 按键动词的载荷（[`BoundAction::CommentState`] /
/// [`SessionAction::CommentState`] 共用）。
///
/// 载荷：空（裸动词）= 在持久态之间切换；`default` / `detail` = 直达该态；
/// `session` = 只翻转当前候选窗（仅会话表有意义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStateAction {
    /// 切换持久态（写回 `ui.comment.state`）。
    Toggle,
    /// 直达指定持久态。
    Set(crate::comment_lib::CommentState),
    /// 只翻转当前候选窗，不写配置。
    Session,
}

impl CommentStateAction {
    /// 解析 `comment_state:` 之后的那一段；未知载荷返回 `None`（同 [`SingleCharAction::parse_payload`]）。
    pub fn parse_payload(rest: &str) -> Option<Self> {
        use crate::comment_lib::CommentState;
        match rest.trim().to_ascii_lowercase().as_str() {
            "" => Some(Self::Toggle),
            "default" => Some(Self::Set(CommentState::Default)),
            "detail" => Some(Self::Set(CommentState::Detail)),
            "session" => Some(Self::Session),
            _ => None,
        }
    }

    /// 配置值（不含 `comment_state:` 前缀）；`Toggle` 为空串，对应裸动词。
    pub fn as_payload(&self) -> &'static str {
        use crate::comment_lib::CommentState;
        match self {
            Self::Toggle => "",
            Self::Set(CommentState::Default) => "default",
            Self::Set(CommentState::Detail) => "detail",
            Self::Session => "session",
        }
    }
}

/// 候选窗定位方式（内部配置，不进设置页）：热键直达模式激活时是否发占位 composition 取坐标。
///
/// 这些模式激活瞬间没有输入内容却要立刻显示窗口。占位 composition（`SetText(" ")` 后
/// `GetTextExt`）坐标最准，但建在当前 selection 上，用户选中着文字时会把它替换掉；
/// `false` 则完全不发 `UpdateComposition`，选中内容保留，坐标退到 `GetCaretPosition` 的回退链。
/// 逐模式开关，出厂只关加词。背景与已否定的方向见
/// `docs/architecture/caret-position-host-compat-notes.md` 的「热键直达模式的占位 composition」。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CaretPlacementConfig {
    /// 快捷加词（`keys.add_word`，出厂 Ctrl+=）。
    pub add_word_via_composition: bool,
    /// 临时拼音直达热键（`enter_temp_pinyin`）。热键进入时 `key_code=0` ⇒ 前缀为空 ⇒ 空组合区。
    /// 只管按下热键时缓冲为空的进入；打了一半再按直达热键走顶字上屏，本就不建 composition，
    /// 与本开关无关。
    pub temp_pinyin_via_composition: bool,
    /// 特殊模式直达热键（`enter_special:<id>`）。同上，`key_code=0` ⇒ 前缀空。
    pub special_via_composition: bool,
    /// 生僻字模式热键（`enter_rare_char`）。同上。
    pub rare_char_via_composition: bool,
}

impl Default for CaretPlacementConfig {
    fn default() -> Self {
        // 加词出厂为 false、其余为 true，是逐个取值而非笔误：加词弹预览窗，对坐标精度不敏感，
        // 「保住选中文字」优先；另三个弹候选列表，错位代价更大。理由见本结构体文档。
        Self {
            add_word_via_composition: false,
            temp_pinyin_via_composition: true,
            special_via_composition: true,
            rare_char_via_composition: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    #[serde(default = "default_filter_mode")]
    pub filter_mode: String,
    /// 含生僻字的词要不要吃 `filter_mode` 这一刀：`keep`（出厂，整词放行）/ `filter`（与单字同判）。
    /// 值域与调研见 `wind_candidate::RarePhrasePolicy`。
    ///
    /// 与 `filter_mode` 正交（那个决定「什么算该滤」），故不是给它加第四档：两件事要能任意组合，
    /// 如常用字档 + 整词放行。
    #[serde(default = "default_rare_phrase")]
    pub rare_phrase: String,
    /// 上屏文本里的换行用什么字符表达的全局默认档；per-app 覆盖见 compat.toml 的 `[[commit_newline]]`。
    /// 值域与「为什么必须按应用配」见 [`crate::app_compat::NewlineStyle`]：`keep`（出厂）/ `cr` / `lf` / `crlf`。
    ///
    /// 认不出的值只在全局层回落出厂档，per-app 层认不出的值等于「没配」＝跟随全局。须与 `NewlineStyle`
    /// 的 `#[default]` 一致（`commit_newline_global_default_matches_enum_default`）。仅 Windows 消费，
    /// macOS 的 IMKit 用 LF，协调器在非 Windows 上编译期即 `Keep`。
    #[serde(default = "default_commit_newline")]
    pub commit_newline: String,
    /// 英文输入态（英文方案 / 临时英文）下临时夺取哪个键来切换候选的大小写档位：
    /// 默认 → 全大写 → 全小写 → 默认，一次组合结束即复位。空串 = 关闭（出厂）。
    /// 快捷输入里有英文段时同样生效，档位只套英文段。
    ///
    /// 值域见 `Coordinator::parse_case_cycle_key`：`capslock` / `tab` / `enter` / `space` / `escape`，
    /// 不认识的键名告警后按关闭处理。是键名而非 bool：macOS 上 CapsLock 拦不到（IMKit 走
    /// `.flagsChanged`），用户需自己指定键（Windows 填 `capslock`、macOS 填 `tab`）。
    /// 键即开关，不要再加 `enabled`，两道闸串联会让配好了却没反应。
    ///
    /// 被夺取的键在英文输入期间失去原语义，启动体检会告警（`Coordinator::warn_english_case_cycle_conflict`）。
    #[serde(default)]
    pub english_case_cycle_key: String,
    /// 上屏注释 / 拼音：组合中按 Alt+数字 N 上屏第 N 个候选的拼音或注释而非候选字本身，
    /// Alt+空格 取高亮那条。键不可配，本项同时是开关与内容选择：`off`（出厂）/ `pinyin`（带调）/
    /// `pinyin_plain`（不带调）/ `comment`（注释段原文，不截断）。解析见 [`AltCommit`]。
    ///
    /// 只在「中文 + 有候选」时吃键；`keys.key_actions` 里用户已绑的 Alt 组合一律让位
    /// （排查清单见 `docs/design/alt-commit.md`）。
    #[serde(default = "default_alt_commit")]
    pub alt_commit: String,
    /// 检索范围放宽（智能档增强）。
    #[serde(default)]
    pub scope_relax: ScopeRelaxConfig,
    /// 拼音纠错与正确拼音提示。
    #[serde(default)]
    pub correction: CorrectionConfig,
    #[serde(default = "default_enter_behavior")]
    pub enter_behavior: String,
    #[serde(default = "default_space_behavior")]
    pub space_on_empty_behavior: String,
    /// 空码（缓冲非空但无候选）时按标点/符号键的处理方式。三态：
    ///
    /// - `commit` 上屏原码再接标点；
    /// - `clear`（出厂）丢弃原码，标点照常上屏；
    /// - `clear_no_input` 丢弃原码，标点本身也不上屏。
    ///
    /// 与 [`Self::enter_behavior`] / [`Self::space_on_empty_behavior`] 同族，但同一字面值 `clear`
    /// 在「键字符出不出」上刻意相反：回车/空格的 `clear` 吞键，标点的 `clear` 出键（标点是用户
    /// 真想输入的字符），`clear_no_input` 补的是标点缺的那一格。值域清单在
    /// `config_schema::PUNCT_EMPTY_CODE_BEHAVIOR_VALUES`，唯一解释器是 `Coordinator::punct_empty_code_policy`。
    /// 不要与 `schema.codetable.punct_commit` 混淆：那一项关掉是吞键并保留编码。
    #[serde(default = "default_punct_on_empty_behavior")]
    pub punct_on_empty_behavior: String,
    /// 组码中可直接进输入缓冲的符号（纯字面字符集，逐字符匹配）。出厂 `-`，留空 = 关闭。
    ///
    /// 解决 `-` 的双重身份：它既是英文标识符的高频字符（`sun-panel`），又是出厂翻页键。
    /// 只在该键此刻没有别的活身份时才入缓冲，判据单点在 `Coordinator::symbol_buffer_key_free`
    /// （会话动作、次选键、以词定字键、引导键、音节分隔符与英文分词符、数字选词，命中即让位）。
    /// 唯一例外是有候选、本次会话尚未翻过页时的「上一页」，那一格本是空转吞键；翻过页后恢复翻页身份
    /// （`State::paged` 粘滞到候选重装），对齐微软拼音。
    ///
    /// 无候选那一格不碰：那里 `-` 走标点臂按 `punct_on_empty_behavior` 甩掉废码；但缓冲里已有本闸门
    /// 放进来的符号时照收。只对组码中生效，空闲时这些键仍归宿主/标点流水线。
    ///
    /// 不复用 `input_chars` 的范围语法，理由同 [`TempEnglishConfig::symbol_chars`]。本项也不是码元：
    /// 不进 `CodeCharSet`；要让符号成为真正的码元走 `[engine.codetable] input_chars`，那条路无条件夺取
    /// 该键（见 `Coordinator::code_char_conflicts`），与本项的「让位优先」相反。
    #[serde(default = "default_buffer_symbol_chars")]
    pub buffer_symbol_chars: String,
    #[serde(default = "default_numpad_behavior")]
    pub numpad_behavior: String,
    /// 小键盘恒半角：开启后小键盘的 `0-9` 与 `. + - * /` 在全角态下仍输出 ASCII，且整条标点流水线
    /// 一并跳过（自定义映射 / 中文标点 / 全半角）。
    ///
    /// 语义是设备级：小键盘用来录数字，全角数字在表格与编号栏里几乎总是错的。与
    /// [`Self::numpad_behavior`] 正交，只有落到出字那一步才改半角。覆盖面止于单键直接上屏的路径：
    /// 临英 / URL / mix 等缓冲类模式只存字符不存来源，不受本开关影响，这是刻意的能力边界。
    #[serde(default)]
    pub numpad_half_width: bool,
    /// 启动默认状态（记住上次状态 / 默认中文 / 全角 / 中文标点；原 general 域）。
    #[serde(default)]
    pub default: InputDefaultConfig,
    /// 标点相关（随中英、智能标点、自定义映射）。
    #[serde(default)]
    pub punct: PunctConfig,
    /// 智能符号模式。
    #[serde(default)]
    pub symbol: SymbolConfig,
    /// 中英 / 中数之间自动补空格（[input.spacing]）。
    #[serde(default)]
    pub spacing: SpacingConfig,
    /// 标点配对（输入左括号自动补右括号 + 输右括号智能跳过）。
    #[serde(default)]
    pub auto_pair: AutoPairConfig,
    /// 临时英文（Shift+字母 / 触发键进入临英缓冲）。
    #[serde(default)]
    pub temp_english: TempEnglishConfig,
    #[serde(default)]
    pub capslock: CapslockConfig,
    /// 临时拼音（码表方案下临时切到拼音反查）。
    #[serde(default)]
    pub temp_pinyin: TempPinyinConfig,
    /// 生僻字模式（用当前方案的编码输入，候选只留生僻字）。
    #[serde(default)]
    pub rare_char: RareCharConfig,
    /// 反查模式（用本方案编码 + 通配查字，只查主码表）。
    #[serde(default)]
    pub reverse: ReverseConfig,
    /// Emoji 候选扩展（按候选文本查表追加，与编码域无关故所有方案通用）。
    #[serde(default)]
    pub emoji: EmojiConfig,
    /// 网址输入模式。
    #[serde(default)]
    pub url: UrlConfig,
    /// 邮箱输入模式（缓冲非空时按 `@` 触发，补全常用后缀）。与 [`Self::url`] 共用
    /// 补全候选源与夺取回退骨架，但触发判据是后缀而非前缀。
    #[serde(default)]
    pub email: EmailConfig,
    /// Unicode 码点输入模式（`u+4e00` → 一）。与 [`Self::url`] 同为前缀夺取式。
    #[serde(default)]
    pub unicode: UnicodeConfig,
    /// 快捷加词面板（目前只有候选布局一项；进入方式是 keys.add_word 热键）。
    #[serde(default)]
    pub add_word: AddWordConfig,
    /// 简繁转换（上屏文字变换）。原 features.s2t。
    #[serde(default)]
    pub s2t: S2TConfig,
    /// 繁简转换（上屏文字变换，与 [`Self::s2t`] 反向）：词库与内部候选是繁体、要出简体。
    /// 与 `s2t` 互斥（由 `Coordinator::persist_s2t_enabled` / `persist_t2s_enabled` 保证）：
    /// 内部域只有一个，同时开会转过去再转回来。
    #[serde(default)]
    pub t2s: T2SConfig,
    /// 命令栏（$CC/$SS/$AA 等命令候选）。原 features.cmdbar。
    #[serde(default)]
    pub cmdbar: CmdbarConfig,
    /// 短语前缀列举（含命令栏 $CC/$SS/$AA）。原 dict.phrase / Go input.phrase。
    #[serde(default)]
    pub phrase: PhraseConfig,
    /// 顶码上屏策略（内部/实验，默认 direct_commit 真提交时序，躲开 diff 合并与整段下划线）。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub top_commit_mode: TopCommitMode,
    /// 顶屏类上屏（标点顶屏 / 非码元字符与小键盘顶屏 / 进模式顶屏）是否也造词并推进临时词晋升计数
    /// （6b），口径与各路选词出口一致（内部配置，不进设置页）。
    ///
    /// 默认开：顶屏就是「选高亮那条」。关掉时各路（含临拼）一律不造词、不推 6b，词频与上屏历史
    /// 不受影响。次三选键越界的 `commit_and_input` 也算顶屏；高亮是分步候选时只推 6b 不造词。
    /// 例外：临拼兜底臂（非标点键被吞、选中高亮候选）是选词不是顶屏，恒造词、不读本项。
    #[serde(default = "default_true")]
    pub top_commit_learn: bool,
    /// 密码框强制英文：宿主报出的 InputScope 含密码位（掩码 bit31）时，输入闸强制英文透传，
    /// 不改 `chinese_mode` 持久值（内部配置，入口是托盘/工具栏右键的「高级」菜单）。
    ///
    /// 出厂开：真密码框里出中文候选既打不对密码、又可能把明文露在候选窗里。必须能关：有的宿主把
    /// 普通输入框也报成 `IS_DEFAULT + IS_PASSWORD`（`0x80000001`）。
    ///
    /// 关掉时两侧一起放行：协调器不再置位 `password_suppress`，DLL 侧本地吃键门控经
    /// `push_password_suppress_config` 同步关闭，只关 core 则开关形同虚设。输入诊断 HUD 的 InputScope
    /// 显示不受影响，那是判断误报的依据。
    #[serde(default = "default_true")]
    pub password_force_english: bool,
    /// 忽略软件（宿主）自己写 compartment 引起的中英切换，开和关两个方向都忽略。出厂关。
    ///
    /// 按应用的 compat.toml `ignore_host_ime_switch` 写了值就以它为准。判据（只认 compartment 来源、
    /// Ctrl 按住放行）见协调器 `host_ime_switch_ignored`。全局打开有代价：gvim 一类宿主靠 OPENCLOSE
    /// 保存/恢复中英，按应用锁定中英的外部工具（KBLSwitch、AutoHotkey 脚本）也会被忽略；更推荐在
    /// 「应用兼容性」里单独开。
    #[serde(default)]
    pub ignore_host_ime_switch: bool,
    /// 联想（上屏后按上文推荐下一个词/标点）。默认关。
    #[serde(default)]
    pub association: AssociationConfig,
    /// 候选窗定位方式（内部配置，见 [`CaretPlacementConfig`]）。
    /// `serde(default)`：旧配置文件无此段时取 [`CaretPlacementConfig::default`]。
    #[serde(default)]
    pub caret: CaretPlacementConfig,
}

impl InputConfig {
    /// 简繁两个方向的生效状态：`(简入繁出, 繁入简出)`。
    ///
    /// 两者互斥；配置里两个都 true 时以 s2t 优先：快照不带顺序，取固定优先级，简入繁出用户基数大。
    /// 「后开的赢」住在协调器 `toggle_conversion_direction` 与设置页的互斥联动里，那两处知道用户按的是哪个。
    /// 构造、reload、归一都调用这一处，别再各写一份。
    pub fn conversion_directions(&self) -> (bool, bool) {
        (self.s2t.enabled, self.t2s.enabled && !self.s2t.enabled)
    }

    /// 配置里是不是「两个方向都开」这个非法组合（该被归一掉，见协调器
    /// `normalize_conversion_exclusivity`）。
    pub fn has_conversion_conflict(&self) -> bool {
        self.s2t.enabled && self.t2s.enabled
    }
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            filter_mode: "smart".to_string(),
            rare_phrase: default_rare_phrase(),
            commit_newline: default_commit_newline(),
            english_case_cycle_key: String::new(),
            alt_commit: default_alt_commit(),
            scope_relax: ScopeRelaxConfig::default(),
            correction: CorrectionConfig::default(),
            enter_behavior: "commit".to_string(),
            space_on_empty_behavior: "commit".to_string(),
            punct_on_empty_behavior: default_punct_on_empty_behavior(),
            buffer_symbol_chars: default_buffer_symbol_chars(),
            numpad_behavior: default_numpad_behavior(),
            numpad_half_width: false,
            default: InputDefaultConfig::default(),
            punct: PunctConfig::default(),
            symbol: SymbolConfig::default(),
            spacing: SpacingConfig::default(),
            auto_pair: AutoPairConfig::default(),
            temp_english: TempEnglishConfig::default(),
            capslock: CapslockConfig::default(),
            temp_pinyin: TempPinyinConfig::default(),
            rare_char: RareCharConfig::default(),
            reverse: ReverseConfig::default(),
            emoji: EmojiConfig::default(),
            url: UrlConfig::default(),
            email: EmailConfig::default(),
            unicode: UnicodeConfig::default(),
            add_word: AddWordConfig::default(),
            s2t: S2TConfig::default(),
            t2s: T2SConfig::default(),
            cmdbar: CmdbarConfig::default(),
            phrase: PhraseConfig::default(),
            top_commit_mode: TopCommitMode::default(),
            top_commit_learn: true,
            password_force_english: true,
            ignore_host_ime_switch: false,
            association: AssociationConfig::default(),
            caret: CaretPlacementConfig::default(),
        }
    }
}

/// 标点配置（[input.punct]）：随中英、智能标点、自定义映射。
/// `custom_mappings`: key=源字符（引号用 `"1`/`"2`/`'1`/`'2` 区分左右），
/// value=`[中文半角, 英文全角, 中文全角, 英文半角]`（空串/缺列=回退默认转换）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PunctConfig {
    /// 标点随中英模式切换。
    #[serde(default)]
    pub follow_mode: bool,
    /// 数字后的标点智能直出英文。
    #[serde(default = "default_true")]
    pub smart_after_digit: bool,
    /// 参与"数字后智能英文标点"的标点集合。
    #[serde(default = "default_smart_punct_list")]
    pub smart_list: String,
    /// 英文单词后的标点智能直出英文（`Node.js`、`a/b`、`snake_case`）。默认关。
    ///
    /// 「英文后」= 插入点前一字符是拉丁字母：空缓冲按标点时取客户端输入序列记账
    /// （`KeyPayload.seq_char`），顶屏时取被顶出的那段文本的末字符。用户自己打了空格之后
    /// 不再算英文后。与数字那一对互相独立（开关与集合各一份）。
    #[serde(default)]
    pub smart_after_latin: bool,
    /// 参与「英文后智能英文标点」的标点集合（存键本身的 ASCII 标点）。
    ///
    /// 出厂刻意不含 `,;!?`：中文句子里英文词后接中文逗号很常见（「我用 Python，它……」）。
    /// `.` 在内的代价是句末英文词后的句号也出 `.`，靠智能符号连按两次换回 `。`。
    #[serde(default = "default_latin_smart_punct_list")]
    pub latin_smart_list: String,
    /// 自定义标点映射开关。
    #[serde(default)]
    pub custom_enabled: bool,
    /// 自定义标点映射表（四状态：中半/英全/中全/英半）。
    #[serde(default)]
    pub custom_mappings: HashMap<String, Vec<String>>,
}

impl Default for PunctConfig {
    fn default() -> Self {
        Self {
            follow_mode: false,
            smart_after_digit: true,
            smart_list: default_smart_punct_list(),
            smart_after_latin: false,
            latin_smart_list: default_latin_smart_punct_list(),
            custom_enabled: false,
            custom_mappings: HashMap::new(),
        }
    }
}

/// 中英 / 中数之间自动补空格（[input.spacing]）。设计见
/// `docs/design/auto-spacing-and-latin-smart-punct.md`。
///
/// 只管切换处：空格是切换后那一笔上屏的前缀（`你好` → ` Hello`），交界一侧是标点、空白或不可知时
/// 不补。「上一笔末字符」来自客户端输入序列记账（`KeyPayload.seq_char`），不看宿主光标。
/// 英文词之间的空格归 `schema.english.commit_space` / `input.temp_english.commit_space`。
/// 按应用关闭见 compat `[[apps]] auto_spacing = false`（自带空格处理的 Markdown 编辑器、终端）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpacingConfig {
    /// 汉字与拉丁字母之间补空格（`中 en 中`）。默认关。
    #[serde(default)]
    pub cjk_latin: bool,
    /// 汉字与数字之间补空格（`共 3 个`）。默认关。
    #[serde(default)]
    pub cjk_digit: bool,
}

/// 智能符号替换方案，「体感」与「兼容性」的取舍，故都保留：
/// - `DeleteReplace`（默认）：press1 直接提交中文符号，press2 删掉重打成英文。所见即所得，
///   代价是依赖对宿主做删改。
/// - `HoldComposition`：press1 开启 TSF 组合态展示中文符号，press2 替换组合提交英文；超时
///   （smart_timeout_ms）后自动提交中文。不做删改，适合对删改敏感的宿主。
// Copy + Eq：本身是无字段枚举，且要按值放进 per-app 的 `ActiveCompat`（那是个 Copy 结构）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SmartMethod {
    #[default]
    DeleteReplace,
    HoldComposition,
}

/// 顶码/顶屏的宿主上屏策略。影响顶码上屏时「已确认文字」如何落到宿主：
/// - PreConfirm：留在 TSF 组合态（_pendingCommitPrefix 聚合），延迟到最终 CommitText 才真提交。
///   diff 式宿主（终端/Chromium）不双写，但部分宿主整段画下划线、WPS 智能标点顶屏会清空。
/// - DirectCommit：顶码时真提交，余码新组合延迟到触发键 keyup 才开（照抄真实输入法时序），
///   靠隔一拍消息泵躲开 diff 合并；真提交无下划线歧义、WPS 不清空。
///
/// TODO(per-app)：后续可按宿主进程名 override（当前仅全局默认）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TopCommitMode {
    PreConfirm,
    #[default]
    DirectCommit,
}

/// 智能符号配置（[input.symbol]）：同一标点在时限内连按两次，删前一字符替换为另一形态。
///
/// 三个总开关互相独立，各管一种上下文，都默认关：
///   - `smart_mode`：中文标点状态，press1 中文 → press2 英文（数字后智能标点方向相反）。
///   - `english_punct_mode`：中文输入 + 英文标点状态，press1 英文 → press2 中文。
///   - `english_mode`：英文输入模式，同上。
///
/// 后两者是不同场景（用英文标点写中文 vs 正在打英文），很多人只想要前者。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolConfig {
    /// 智能符号模式总开关（默认 false）。
    #[serde(default)]
    pub smart_mode: bool,
    /// 判定时限（毫秒，默认 500）。三种上下文共用。
    #[serde(default = "default_smart_symbol_timeout_ms")]
    pub smart_timeout_ms: i32,
    /// 参与智能符号转换的中文标点集合（子串包含匹配，含成对/多字符标点）。
    #[serde(default = "default_smart_symbol_chars")]
    pub smart_chars: String,
    /// 替换方案：`delete_replace`（删改，默认）或 `hold_composition`（保持组合态，兼容性更好）。
    /// 三种上下文共用。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub smart_method: SmartMethod,
    /// 英文标点状态（中文输入模式 + 工具栏标点切英文）下的智能符号（默认 false）。
    #[serde(default)]
    pub english_punct_mode: bool,
    /// 英文输入模式（整个输入法切英文）下的智能符号（默认 false）。
    ///
    /// 开启会让 core 把 `english_chars` 里的键推给 DLL 吃下转发——英文半角下这些键本来是直接
    /// 透传给宿主的，不吃就永远到不了引擎。故此开关的影响面比另外两个大，默认关。
    #[serde(default)]
    pub english_mode: bool,
    /// 参与英文智能符号的源字符集合（`english_punct_mode` 与 `english_mode` 共用）。
    ///
    /// 与 `smart_chars` 存中文产物不同，这里存键本身的 ASCII 标点：推给 DLL 的吃键集必须是源字符。
    /// 不建议放配对符：英文模式下这些键被吃走后配对改由 core 处理，DLL 跳出栈为空，Tab 跳出会失效
    /// （见 `handle_english_custom_punct`）。
    #[serde(default = "default_english_smart_chars")]
    pub english_chars: String,
}

impl Default for SymbolConfig {
    fn default() -> Self {
        Self {
            smart_mode: false,
            smart_timeout_ms: default_smart_symbol_timeout_ms(),
            smart_chars: default_smart_symbol_chars(),
            smart_method: SmartMethod::default(),
            english_punct_mode: false,
            english_mode: false,
            english_chars: default_english_smart_chars(),
        }
    }
}

/// 标点配对配置（[input.auto_pair]，对齐 Go AutoPairConfig）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoPairConfig {
    /// 中文标点配对开关
    #[serde(default)]
    pub chinese: bool,
    /// 英文标点配对开关
    #[serde(default)]
    pub english: bool,
    /// 中文配对表（每项 2 字符："（）"）
    #[serde(default = "default_chinese_pairs")]
    pub chinese_pairs: Vec<String>,
    /// 英文配对表（每项 2 字符："()"）
    #[serde(default = "default_english_pairs")]
    pub english_pairs: Vec<String>,
    /// 跳出配对的按键：命中即光标越过右符号、弹出配对栈。可多选。
    ///
    /// 取值为键名（`"tab"`/`"enter"`/`"space"`/`"escape"`），外加 `"right_symbol"` = 右符号键本身。
    /// 列表里没有就是没有，不做隐式补偿。对称配对（引号）永不参与右符号跳出：按键不携带开/闭，
    /// 一律开新的一对（见 `pin_quote_left_if_paired`）。
    #[serde(default = "default_jump_out_keys")]
    pub jump_out_keys: Vec<String>,
    /// 配对状态时效，单位秒（内部项）。`0` = 不过期。
    ///
    /// 管同一输入框内的状态陈旧（鼠标点过别处、退格删掉括号，输入法都感知不到）：距最后一次按键
    /// 超过本值即跳出键不再生效；持续输入会不断刷新。跨焦点的陈旧由失焦清空处理（见 `handle_focus_lost`）。
    #[serde(default = "default_pair_state_ttl_secs")]
    pub state_ttl_secs: u32,
    /// 配对插入改走 Unicode 按键注入提交（内部项，默认关 = TSF 直写）。键名沿用最初的组合语义设计。
    ///
    /// 有的宿主（AutoCAD 表格在位编辑器）对 `ITfInsertAtSelection` 直写谎报成功却丢弃 ASCII，只有
    /// `VK_PACKET` 产生的 WM_CHAR 能上屏（见 `docs/architecture/pair-commit-host-compat-notes.md`）。
    /// 开启后 DLL 以按键注入上屏配对文本（空闲态被吃下的 ASCII 标点同理），光标回移仍走 `VK_LEFT`
    /// （修饰键按住时延后到松开）。compat 层 `pair_via_comp` 按应用覆盖本值。
    #[serde(default)]
    pub commit_via_composition: bool,
}

impl Default for AutoPairConfig {
    fn default() -> Self {
        Self {
            chinese: false,
            english: false,
            chinese_pairs: default_chinese_pairs(),
            english_pairs: default_english_pairs(),
            jump_out_keys: default_jump_out_keys(),
            state_ttl_secs: default_pair_state_ttl_secs(),
            commit_via_composition: false,
        }
    }
}

/// 默认 120 秒。够覆盖「在括号里停下来想一会儿」，又不会让状态在用户去干别的事之后
/// 仍然存活到吃掉 Tab。
fn default_pair_state_ttl_secs() -> u32 {
    120
}

/// 默认只启用右符号跳出（保持「打 `）` 跳出」这一长期行为），Tab/Enter 需用户显式勾选。
fn default_jump_out_keys() -> Vec<String> {
    vec![JUMP_OUT_RIGHT_SYMBOL.to_string()]
}

/// `jump_out_keys` 里代表「右符号键本身」的特殊值（非键名，不参与 VK 解析）。
pub const JUMP_OUT_RIGHT_SYMBOL: &str = "right_symbol";

fn default_chinese_pairs() -> Vec<String> {
    ["（）", "【】", "｛｝", "《》", "〈〉", "「」", "『』"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_english_pairs() -> Vec<String> {
    ["()", "[]", "{}"].iter().map(|s| s.to_string()).collect()
}

/// 临英符号白名单出厂值：数字 + 标识符/代码里最常用的符号。
///
/// 含 `.` 与 `-` 是刻意的（`obj.prop` / `e-mail`），代价是它们在临英下交出翻页职责，
/// 「打完英文按句号上屏」也随之失效。不需要的人从列表里删掉即可。
fn default_temp_english_symbol_chars() -> String {
    "0123456789+-_.@#/".to_string()
}

/// 临时英文配置（[input.temp_english]，原 input.shift_temp_english）。
///
/// `PartialEq` 是承重的：`Coordinator::reload_user_config` 的 `schema_dirty` 判据靠它发现本段变更、
/// 触发引擎刷新镜像，去掉它 `phrase_seg` 这类开关会变成重启才生效。同 [`TempPinyinConfig`]。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TempEnglishConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 显示英文候选（原 show_english_candidates）。
    #[serde(default = "default_true")]
    pub show_candidates: bool,
    #[serde(default = "default_shift_behavior")]
    pub shift_behavior: String,
    /// 已有编码缓冲时按 Shift+字母的行为（`shift_behavior` 只管空缓冲那一支）。
    /// 字符串枚举，值域：
    /// - `buffer`（出厂）：大写字母以小写进编码缓冲、大小写记在影子串里（现状，
    ///   英文候选套形、混输自由输入都依赖它）。
    /// - `commit_enter`：上屏高亮候选（含已转换前缀），再以该大写字母进临时英文——
    ///   对齐临时拼音「顶字进模式」。英文方案下不生效。
    #[serde(default = "default_shift_when_composing")]
    pub shift_when_composing: String,
    /// 触发键（符号键进入临时英文模式，类似临时拼音触发键）。默认空（仅 Shift+字母触发）。
    #[serde(default)]
    pub trigger_keys: Vec<String>,
    /// 允许符号与数字直接入缓冲（`C++` / `hello2` / `x64`）而非触发上屏或选词。
    /// 总开关，放行哪些字符由 [`Self::symbol_chars`] 精确决定。
    #[serde(default)]
    pub allow_symbols: bool,
    /// 允许入缓冲的符号/数字白名单（纯字面字符集，逐字符匹配）。仅 `allow_symbols` 开启时生效；
    /// 列表外的字符维持关闭时的语义（符号上屏并退出临英、`;`/`'` 选词、`-=[],.` 翻页、数字选词）。
    /// 留空 = 一个都不放行。
    ///
    /// 不复用 `input_chars` 的 `a-z` 范围语法：`+-_` 会被解析成 `0x2B..=0x5F` 一整片，填一个减号
    /// 就静默放行几十个字符。与 `symbol.english_chars` / `symbol.smart_chars` 一致，字面即全部真相。
    #[serde(default = "default_temp_english_symbol_chars")]
    pub symbol_chars: String,
    /// 空格作为输入字符入缓冲（可打出带空格的英文短句）。上屏职责随之转给回车，
    /// 且回车此时上屏高亮候选而非原文——否则该配置下没有任何选词键可用。
    #[serde(default)]
    pub space_as_input: bool,
    /// 进入临时英文期间的候选布局（默认跟随全局）。
    /// 典型用法是设 `horizontal`——英文候选一行放得下，全局竖排时反而占屏。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub candidate_layout: LayoutIntent,
    /// 首候选是用户所打原文。三档，见 [`RawCandidateMode`]；出厂 [`RawCandidateMode::Always`]
    /// （＝老配置的 `true`）。可配是因为「总走词库补全」也是合理偏好，原文占首位时常用词永远在 2 号键。
    ///
    /// 本项不出原文、[`Self::case_variants`] 也关、词库又无命中时候选为空，这不是缺陷：临英空格臂按
    /// 实际候选是否为空判断，会落到上屏缓冲原文（见 `handle_temp.rs`）。`show_candidates = false` 时
    /// 词库不被查询，`InDict` 按 `Always` 处理（见 `update_temp_english_candidates`）。
    ///
    /// 本项与 `case_variants` 也作用于快捷输入里的英文成员：头部候选落在英文段段首，占该成员的配额。
    #[serde(default, deserialize_with = "de_raw_candidate")]
    pub raw_candidate: RawCandidateMode,
    /// 生成大小写变形候选（全小写 / 首字母大写 / 全大写）。
    ///
    /// 关掉后候选只剩输入原文 + 词库匹配。变形候选每条占一个候选位，会把词库候选挤到下一页，
    /// 且没有注释、词频；只想要词库补全的人可以关掉。
    #[serde(default = "default_true")]
    pub case_variants: bool,
    /// 词库候选跟随输入的大小写形态（逐位投影）。默认开，语义同
    /// [`EnglishGlobal::case_follow_input`]（两个作用域各一份）。
    ///
    /// 临英由 Shift+字母进入，缓冲首字母恒大写，本项开着时候选恒为首字母大写形态，这是有意裁定的。
    /// 与已删除的整串套 Title case 不同，本项逐位投影且单向：只覆盖用户按了 Shift 的位，
    /// 词库自带的大写一律保留。
    ///
    /// 也作用于快捷输入里的英文：缓冲因 Shift+字母带上大写时，纯字母缓冲在原文之后追加的英文段
    /// 按本项投影（见 `Coordinator::mix_free_english_segment`）。
    #[serde(default = "default_true")]
    pub case_follow_input: bool,
    /// 临英选词上屏后自动补一个空格。中文与英文之间的空格另见 [`SpacingConfig`]。
    ///
    /// 与 `schema.english.commit_space` 是两个作用域各一份：长时打英文时补空格顺手，中文里插一个
    /// 英文词时补上的空格往往还得删掉，需求可能相反。出厂两者皆关。
    ///
    /// 也作用于快捷输入里的英文（英文词库候选与原文补，中文候选与计算结果不补，判据见
    /// `Coordinator::mix_candidate_is_english`），以及中文方案主路径里的英文词库候选（原码与短语
    /// 不补，判据见 `Coordinator::english_appends_space`）。
    #[serde(default)]
    pub commit_space: bool,
    /// 临英下的词组分词输入：用 `'` 切开各段，每段只打前缀。默认关。
    ///
    /// 与 [`EnglishGlobal::phrase_seg`] 是两个作用域各一份，语义、分词符、跳词规则一致，只是需求
    /// 可能相反故开关独立。快捷输入里的英文成员也读这一份（见 `Coordinator::mix_phrase_separator`）。
    /// 开启后 `'` 在临英下不再作二三候选键，取舍同英文方案侧。
    #[serde(default)]
    pub phrase_seg: bool,
}

impl Default for TempEnglishConfig {
    fn default() -> Self {
        Self {
            commit_space: false,
            enabled: true,
            show_candidates: true,
            shift_behavior: "temp_english".to_string(),
            shift_when_composing: default_shift_when_composing(),
            trigger_keys: Vec::new(),
            allow_symbols: false,
            symbol_chars: default_temp_english_symbol_chars(),
            space_as_input: false,
            candidate_layout: LayoutIntent::default(),
            raw_candidate: RawCandidateMode::Always,
            case_variants: true,
            case_follow_input: true,
            phrase_seg: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapslockConfig {
    #[serde(default)]
    pub cancel_on_mode_switch: bool,
    /// 切中英时取消了大写锁定之后落到哪个模式（仅 `cancel_on_mode_switch` 开着时有意义）。值域：
    ///
    /// - `chinese`（出厂）：Shift 等切换键归位中文（对齐搜狗）；Ctrl+空格 这类自带目标方向的切换
    ///   按请求方向；
    /// - `english`：两条路都进英文（输完大写想接着打小写）；
    /// - `toggle`：照常切换，Shift 翻转中英，Ctrl+空格 按请求方向。
    ///
    /// 只管用户按出来的切换；宿主写 compartment、功能菜单等非用户发起的请求恒按请求落地。
    /// 消费端见 `Coordinator::mode_after_caps_cancel`，认不出的值告警后回落 `chinese`。
    /// 另起字段而非把 `cancel_on_mode_switch` 改成枚举：改已发布键的类型要做兼容迁移，收益只是少一个键。
    #[serde(default = "default_capslock_mode_after_cancel")]
    pub mode_after_cancel: String,
    /// 打字中按 CapsLock 切换时，把正在打的编码转大写上屏（打 `wps` 得 `WPS`）：开大写那一下，
    /// 以及 CapsLock 作中英切换键切到英文那一下。
    ///
    /// 只改大小写，上不上屏仍由 `keys.commit_on_switch` 决定（临英总会上屏）；已转换前缀不转。
    /// 消费端见 `Coordinator::take_input_on_mode_switch_cased`。
    #[serde(default)]
    pub commit_upper: bool,
}

impl Default for CapslockConfig {
    fn default() -> Self {
        Self {
            cancel_on_mode_switch: false,
            mode_after_cancel: default_capslock_mode_after_cancel(),
            commit_upper: false,
        }
    }
}

fn default_capslock_mode_after_cancel() -> String {
    "chinese".to_string()
}

/// Emoji 候选扩展：打出词组后，按候选文本查表把对应 emoji 追加到候选里。
///
/// 只查文本不查码，所有方案自动支持。设计与六条硬约束见 `docs/design/emoji-suggestion.md`。
///
/// 每个非零默认值都有 `default_emoji_*` 函数，供 serde 缺键与 [`Default::default()`] 共用，
/// 免得两边字面量分叉。`scope` / `show_as` 是字符串枚举，类型层不约束；消费端遇到不认识的值
/// 必须告警后回落默认，不得静默。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmojiConfig {
    /// 总开关，出厂关。关闭时不打开数据文件（`load_or_build` 不会被调用），未启用的用户
    /// 常驻内存与启动开销为零。
    #[serde(default)]
    pub enabled: bool,
    /// 触发范围：`off` 不扩展 / `exact` 仅对精确整词命中的候选扩展 / `all` 所有候选。
    ///
    /// 出厂 `exact`：`all` 会让前缀补全、子短语之类的候选也带出 emoji，而那些候选本身就是
    /// 「猜」出来的，再挂一个 emoji 只会放大噪音。
    #[serde(default = "default_emoji_scope")]
    pub scope: String,
    /// 呈现形态：`after` 紧随宿主候选 / `tail` 追加到列表末尾 / `comment` 只在注释段灰字
    /// 显示（走 `${emoji}` 模板变量，不进候选列表）。
    ///
    /// 三档互斥，是「emoji 以什么形态出现」的单一决策点——`${emoji}` 变量因此也只在
    /// `comment` 档求值，否则配了 `after` 又在模板里写 `${emoji}` 会两处都出。
    #[serde(default = "default_emoji_show_as")]
    pub show_as: String,
    /// 一个词最多产出几个 emoji。主表 `emoji_word.txt` 平均约 1 个/键，几乎不触发；
    /// 真正要拦的是分类表（`emoji_category.txt` 一键可达几十上百个）。
    #[serde(default = "default_emoji_max_per_word")]
    pub max_per_word: usize,
    /// 对候选列表的前几个候选做扩展。
    ///
    /// 出厂 1（只扩首选）：这是把「候选序号漂移」控制住的主要手段——rime 那边的干扰感，
    /// 根源正是它对每个候选都扩。
    #[serde(default = "default_emoji_max_hosts")]
    pub max_hosts: usize,
    /// 宿主候选至少要有几个字才触发。出厂 2：单字词（「一」→ 1️⃣）噪音最大且最没用。
    #[serde(default = "default_emoji_min_word_chars")]
    pub min_word_chars: usize,
    /// 是否加载分类表（`emoji_category.txt`：「动物」→ 一大批 emoji 那种）。
    /// 出厂关：干扰几乎全来自这张表；但它是「找个动物表情」唯一好用的入口，故做成独立开关。
    #[serde(default)]
    pub categories: bool,
    /// 候选里有追加出来的 emoji / 附加模块符号时，不做全码唯一自动上屏（码表与短语两侧）。
    ///
    /// 出厂关：追加候选不是这个码检索出的词条，默认不计入唯一性判据（见
    /// `Candidate::is_emoji_suggestion`）。开了附加模块（如偏旁部首）的用户要的往往正是那条
    /// 追加候选（`rrup` 提手旁 → 扌），自动上屏会让它来不及出现。
    #[serde(default)]
    pub block_auto_commit: bool,
    /// 附加模块开关（`[input.emoji.modules]`，键 = [`EMOJI_MODULES`] 的 id）。缺的键按该模块
    /// 的出厂值（[`EmojiConfig::module_enabled`]）；不认识的键忽略。
    ///
    /// 用表而不是数组：配置分层对表按键合并、对数组整体覆盖，写成数组的话翻一个开关就把整份列表
    /// 固化进用户配置，之后新增的出厂模块对他不可见。
    #[serde(default = "default_emoji_modules")]
    pub modules: BTreeMap<String, bool>,
}

/// emoji 追加的附加模块（rime-frost 的 OpenCC 表，`docs/design/opencc-conversions.md` T3）。
///
/// rime-emoji 主表（`emoji/emoji_word.txt`，随 [`EmojiConfig::enabled`]）与分类表
/// （[`EmojiConfig::categories`]）不在此列：它们早于本机制，开关保持原样。
pub struct EmojiModule {
    /// 配置键 `input.emoji.modules.<id>`，也是缓存文件名。
    pub id: &'static str,
    /// 设置页显示名。
    pub label: &'static str,
    /// 设置页说明（举一个例子，让人知道打什么词会出什么）。
    pub description: &'static str,
    /// 相对数据根（`data/`）的源文件，OpenCC 文本词典。
    pub file: &'static str,
    /// 只取这些分组（`others.txt` 按分组拼成一份）；空 = 整表。
    pub sections: &'static [&'static str],
    /// 出厂是否开启。
    pub default_on: bool,
}

/// 附加模块注册表。顺序即查询合并顺序（同一个词多个模块都有时，靠前的排前面）。
///
/// 须与 `data/config.toml` 的 `[input.emoji.modules]`、`config_schema` 的登记、设置端清单同步。
/// `others.txt` 里「著名表演艺术家」一组构建期即剔除；「emoji集合」一组的键带 `V` 前缀，
/// 正常打字打不出宿主词，也不收。
pub const EMOJI_MODULES: &[EmojiModule] = &[
    EmojiModule {
        id: "frost_emoji",
        label: "白霜 Emoji",
        description: "补充更多词语的 emoji 与符号，如「右箭头」→ ➡️、「勾选」→ ☑️",
        file: "emoji/frost_emoji.txt",
        sections: &[],
        default_on: true,
    },
    EmojiModule {
        id: "frost_radicals",
        label: "偏旁部首",
        description: "如「提手旁」→ 扌",
        file: "emoji/frost_others.txt",
        sections: &["偏旁部首"],
        default_on: false,
    },
    EmojiModule {
        id: "frost_greek",
        label: "希腊字母",
        description: "如「阿尔法」→ alpha α Α",
        file: "emoji/frost_others.txt",
        sections: &["希腊字母"],
        default_on: false,
    },
    EmojiModule {
        id: "frost_misc",
        label: "单位与杂项",
        description: "如「平方米」→ m²",
        file: "emoji/frost_others.txt",
        sections: &["其他"],
        default_on: false,
    },
    EmojiModule {
        id: "frost_keys",
        label: "键盘符号",
        description: "如「command」→ ⌘",
        file: "emoji/frost_others.txt",
        sections: &["键盘符号"],
        default_on: false,
    },
    EmojiModule {
        id: "frost_diezi",
        label: "叠字",
        description: "如「两个火」→ 炎 炏",
        file: "emoji/frost_others.txt",
        sections: &["二叠字", "三叠字", "四叠字"],
        default_on: false,
    },
    EmojiModule {
        id: "frost_chem",
        label: "化学式",
        description: "如「高锰酸钾」→ KMnO₄",
        file: "emoji/frost_others.txt",
        sections: &["化学式"],
        default_on: false,
    },
    EmojiModule {
        id: "frost_month",
        label: "月份星期英文",
        description: "如「一月」→ Jan. January",
        file: "emoji/frost_others.txt",
        sections: &["月份星期"],
        default_on: false,
    },
    EmojiModule {
        id: "frost_sitcom",
        label: "美剧人名",
        description: "如「谢耳朵」→ Sheldon",
        file: "emoji/frost_others.txt",
        sections: &["生活大爆炸&老友记"],
        default_on: false,
    },
];

/// 用户放置 emoji / OpenCC 表的目录（相对数据根，各资源层合并扫描）。
pub const EMOJI_USER_DIR: &str = "emoji";

/// 数据根 `emoji/` 下属于内置表源文件的文件名，不当作用户模块（它们由主表 / 分类表 /
/// [`EMOJI_MODULES`] 读取；用户层放同名文件是覆盖那张内置表，不是新增模块）。
const EMOJI_BUILTIN_FILES: &[&str] = &[
    "emoji_word.txt",
    "emoji_category.txt",
    "frost_emoji.txt",
    "frost_others.txt",
];

/// 一个可开关的 emoji 附加模块：内置（[`EMOJI_MODULES`]）或用户放进 `emoji/` 的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiModuleEntry {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 相对数据根的源文件。
    pub file: String,
    pub sections: Vec<String>,
    /// 用户放置的文件（`false` = 内置模块）。
    pub user: bool,
    pub default_on: bool,
}

/// 宿主显式注入的 `(用户配置目录, 本地目录)`，见 [`Config::set_dir_override`]。
static DIR_OVERRIDE: std::sync::RwLock<Option<(PathBuf, PathBuf)>> = std::sync::RwLock::new(None);

// 跨 crate 的序列化 / 反序列化一律走下面这几个入口，别在下游直接 `value.try_into::<Config>()`
// 或 `serde_json::to_value(cfg)`：release 不共享泛型，每个调用的 crate 都会把整棵配置类型树的
// serde 代码单态化一份（一份 ~0.2 MiB）。`inline(never)` 防 LTO 把它们又内联回各调用方。
impl Config {
    #[inline(never)]
    pub fn from_toml_value(v: toml::Value) -> Result<Self, toml::de::Error> {
        v.try_into()
    }

    #[inline(never)]
    pub fn to_toml_value(&self) -> Result<toml::Value, toml::ser::Error> {
        toml::Value::try_from(self)
    }

    #[inline(never)]
    pub fn to_json_value(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

impl Config {
    /// 运行时生效的会话态按键表：[`KeysConfig::effective_session_actions`] ⊕ 注释切换键。
    ///
    /// 注释切换键选功能键（F1–F12）时在这里补一条绑定，走会话按键的现成通路；用户在
    /// `session_actions` 里绑过这个键（含 `none`）就让给用户。`hold` 档也补 `comment_state:session`，
    /// 只为让平台转发这个键，按下与松开由协调器的按住判定接走。补的这条不落盘。
    pub fn effective_session_actions(&self) -> BTreeMap<String, String> {
        use crate::comment_lib::SwitchMode;
        let mut out = self.keys.effective_session_actions();
        let c = &self.ui.comment;
        if let Some(name) = c.switch_key.function_key_name().filter(|_| c.enabled) {
            let verb = match c.switch_mode {
                SwitchMode::Off => None,
                SwitchMode::Persist => Some("comment_state"),
                SwitchMode::Session | SwitchMode::Hold => Some("comment_state:session"),
            };
            if let Some(v) = verb {
                out.entry(name.to_string()).or_insert_with(|| v.to_string());
            }
        }
        out
    }

    /// 拼音全局配置的生效取值：`Mobile` 时语法模型换成 `[mobile.grammar]`，其余照基线。
    ///
    /// 由引擎在取拼音配置的那一刻按构造时传入的 [`ConfigOverlay`] 调用，不进 `Config::load`
    /// ——理由见 [`MobileConfig`]：合并后移动端设置页一保存，就把移动端的值写进了桌面基线。
    pub fn pinyin_effective(&self, overlay: ConfigOverlay) -> PinyinGlobalConfig {
        let mut p = self.schema.pinyin.clone();
        if overlay == ConfigOverlay::Mobile {
            p.grammar = PinyinGrammar {
                enabled: self.mobile.grammar.enabled,
                weight: self.mobile.grammar.weight,
                model: self.mobile.grammar.model.clone(),
            };
        }
        p
    }

    /// 全部 emoji 附加模块：内置在前（注册表序），其后是 `emoji/` 目录里用户放置的
    /// OpenCC 表（`*.txt`，按文件名序；id = 文件名去掉 `.txt`，出厂关）。
    ///
    /// 用户文件与内置模块 id 撞名时跳过并告警：同一个开关管两张表，用户分不清。
    pub fn emoji_modules(data_dir: Option<&Path>) -> Vec<EmojiModuleEntry> {
        let mut out: Vec<EmojiModuleEntry> = EMOJI_MODULES
            .iter()
            .map(|m| EmojiModuleEntry {
                id: m.id.to_string(),
                label: m.label.to_string(),
                description: m.description.to_string(),
                file: m.file.to_string(),
                sections: m.sections.iter().map(|s| s.to_string()).collect(),
                user: false,
                default_on: m.default_on,
            })
            .collect();
        for (rel, _abs) in Self::list_data_dir(data_dir, EMOJI_USER_DIR, ".txt") {
            let name = rel.rsplit('/').next().unwrap_or(&rel);
            if EMOJI_BUILTIN_FILES.contains(&name) {
                continue;
            }
            let id = name.trim_end_matches(".txt").to_string();
            if out.iter().any(|m| m.id == id) {
                warn!("emoji 模块文件 {rel} 与内置模块同名，已跳过（请改个文件名）");
                continue;
            }
            out.push(EmojiModuleEntry {
                label: id.clone(),
                description: String::new(),
                id,
                file: rel,
                sections: Vec::new(),
                user: true,
                default_on: false,
            });
        }
        out
    }
}

fn default_emoji_modules() -> BTreeMap<String, bool> {
    EMOJI_MODULES
        .iter()
        .map(|m| (m.id.to_string(), m.default_on))
        .collect()
}

impl EmojiConfig {
    /// 模块是否开启：配置里写了按配置，没写按出厂值；不认识的 id（含用户放置的文件）为 false。
    pub fn module_enabled(&self, id: &str) -> bool {
        self.modules.get(id).copied().unwrap_or_else(|| {
            EMOJI_MODULES
                .iter()
                .find(|m| m.id == id)
                .is_some_and(|m| m.default_on)
        })
    }
}

fn default_emoji_scope() -> String {
    "exact".to_string()
}
fn default_emoji_show_as() -> String {
    "after".to_string()
}
fn default_emoji_max_per_word() -> usize {
    3
}
fn default_emoji_max_hosts() -> usize {
    1
}
fn default_emoji_min_word_chars() -> usize {
    2
}

impl Default for EmojiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            scope: default_emoji_scope(),
            show_as: default_emoji_show_as(),
            max_per_word: default_emoji_max_per_word(),
            max_hosts: default_emoji_max_hosts(),
            min_word_chars: default_emoji_min_word_chars(),
            categories: false,
            block_auto_commit: false,
            modules: default_emoji_modules(),
        }
    }
}

/// 生僻字模式配置（[input.rare_char]）。
///
/// 没有 `enabled`：不绑任何键即进不去，即关闭，无须第二个开关。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RareCharConfig {
    /// 引导键（空码时按下即进入本模式）。
    ///
    /// 不是写入路径：同其余 `trigger_keys`，`normalize()` 把它折算进 `keys.key_actions`（动词
    /// `"rare_char"`）后清空。本字段只剩出厂声明处，以及设置端 `dialog_button_key_action` 控件的
    /// 标识符（该控件只认 strlist）。出厂值必须留在被折算的这一侧，否则「用户清空」与「从没配过」
    /// 同形；本模式出厂不绑键，故是空数组。
    #[serde(default)]
    pub trigger_keys: Vec<String>,

    /// 额外纳入本模式的 Unicode 区块（区块名或预设组名，如 `"emoji"`）。出厂为空 = 只出生僻汉字。
    ///
    /// 生僻汉字本来就在（判据见 `wind_candidate::rare_admits`）；emoji、注音、假名、间架结构符等
    /// 的 `is_string_common` 恒为 true，不显式纳入就永远进不来。
    ///
    /// 「其它」是兜底档：区块表逐块列举一份仍在增长的 Unicode 区间，新块会落进「其它」，
    /// 让它落在用户可控的开关里而不是静默打不出。判据表见 `wind_candidate::charblock` 模块头。
    #[serde(default)]
    pub include_blocks: Vec<String>,
}

/// 反查模式配置（[input.reverse]，reverse-mode spec §3）。
///
/// 进入方式只由 `keys.key_actions` / 方案 `[key_actions]` / `z_key_action`（动词 `reverse`）
/// 承载，出厂不绑任何键 ⇒ 默认不可用；没有总开关，与生僻字（[`RareCharConfig`]）同。
/// 绑了键还要活跃方案有反查能力（码表 / 五笔拼音混输），门卫在协调器 `reverse_mode_available`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReverseConfig {
    /// 反查模式期间的候选布局（默认跟随全局）。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub candidate_layout: LayoutIntent,
    /// 反查模式里也查未启用的扩展词库（`DictSpec::is_enabled() == false`，如五笔「行政区域」），
    /// 结果排在已启用词库之后。只管反查模式本身：行内通配、普通打字候选与候选注释里的编码反查都不受影响。
    /// 出厂关。见 docs/design/codetable-reverse-mode.md §9。
    #[serde(default)]
    pub lookup_disabled_dicts: bool,
    /// 命令栏整串反查（`dict.rev(clip())` 省略 n）接受的最大字数（按字符计，默认 16）。
    /// 超过视为「不是一个词」，不查、返回空串，免得复制一大段文字时对全文算拼音/拆字。
    /// 内部字段，设置页不开放。
    #[serde(default = "default_clip_max_chars")]
    pub clip_max_chars: usize,
}

fn default_clip_max_chars() -> usize {
    16
}

impl Default for ReverseConfig {
    fn default() -> Self {
        Self {
            candidate_layout: LayoutIntent::default(),
            lookup_disabled_dicts: false,
            clip_max_chars: default_clip_max_chars(),
        }
    }
}

/// 临时拼音配置（[input.temp_pinyin]）。码表方案下临时切到拼音反查。全局唯一。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TempPinyinConfig {
    /// 总开关（原方案级 [engine.codetable.temp_pinyin].enabled 上移至此）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 触发键（如 "backtick" / "semicolon"），默认反引号。只认符号键——字母触发键
    /// 已迁往方案级 `schema.codetable.z_key_action`（见 `migrate_letter_trigger_keys`）。
    #[serde(default = "default_temp_pinyin_triggers")]
    pub trigger_keys: Vec<String>,
    /// 专用直达热键（如 "ctrl+shift+p"，空串=不注册）。与 `trigger_keys` 引导键共存；
    /// 热键进入时组合区不写引导符（见 docs/design/special-mode-entry-hotkey.md）。
    #[serde(default)]
    pub hotkey: String,
    /// 进入临时拼音期间的候选布局（默认跟随全局）。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub candidate_layout: LayoutIntent,
}

fn default_temp_pinyin_triggers() -> Vec<String> {
    vec!["backtick".to_string()]
}

impl Default for TempPinyinConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger_keys: default_temp_pinyin_triggers(),
            hotkey: String::new(),
            candidate_layout: LayoutIntent::default(),
        }
    }
}

/// 网址模式配置（[input.url]，原 input.url_input）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UrlConfig {
    /// 总开关（默认关闭）
    #[serde(default)]
    pub enabled: bool,
    /// 触发前缀（恰好匹配；如 "www." / "http" / "https" / "ftp."）
    #[serde(default = "default_url_prefixes")]
    pub prefixes: Vec<String>,
    /// 是否把上屏过的网址记进历史，供下次补全（默认关闭）。
    ///
    /// 独立于 [`Self::enabled`]：网址模式本身不产生持久数据，开了历史才把网址原文落盘，隐私量级
    /// 不同。邮箱后缀学习只记域名不记用户名，故跟随 [`EmailConfig::enabled`] 即可。
    #[serde(default)]
    pub history_enabled: bool,
    /// 历史条数上限，超出时按「次数少、用得旧」的顺序裁掉（0 = 不限）。
    ///
    /// 历史无界增长，而补全只显示前几条。`0` 有代价：补全每键都取出所有命中前缀的行排序再分页，
    /// 不设上限时逐键开销随使用增长。
    #[serde(default = "default_url_history_max")]
    pub history_max: u32,
    /// 进入网址模式期间的候选布局（默认跟随全局）。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub candidate_layout: LayoutIntent,
}

fn default_url_history_max() -> u32 {
    200
}

fn default_url_prefixes() -> Vec<String> {
    vec![
        "www.".to_string(),
        "http".to_string(),
        "https".to_string(),
        "ftp.".to_string(),
        "bbs.".to_string(),
    ]
}

impl Default for UrlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            prefixes: default_url_prefixes(),
            history_enabled: false,
            history_max: default_url_history_max(),
            candidate_layout: LayoutIntent::default(),
        }
    }
}

/// 邮箱输入配置（[input.email]）。
///
/// 不是前缀夺取式而是后缀触发：判据是「本键是 `@` 且缓冲非空」，缓冲整体作为用户名带进模式
/// （`docs/design/prefix-hijack-modes.md` §5.2）。与 [`UrlConfig`] 共用 `try_prefix_hijack`
/// 闸门与 `Rewind` 回退骨架，只是闸门里多一条判据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    /// 总开关（默认关闭，与 [`UrlConfig`] 同口径）。
    #[serde(default)]
    pub enabled: bool,
    /// 预置后缀表，存 `@` 之后的部分（`qq.com` 而不是 `@qq.com`）。
    ///
    /// 须与学习数据（`wind_store::completion::CompletionKind::EmailSuffix`）同域，否则去重对不上，
    /// 同一后缀出现两次。用户打出的后缀不必在表内：上屏后会被学进补全数据，带频次后很快排到前面。
    #[serde(default = "default_email_suffixes")]
    pub suffixes: Vec<String>,
    /// 进入邮箱模式期间的候选布局（默认跟随全局）。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub candidate_layout: LayoutIntent,
}

/// 出厂预置的邮箱后缀。国内常用在前、国际常用在后：只影响没有频次数据的冷启动排序。
///
/// 这是数组字段，用户删干净就是真的空表，不会被深合并把出厂值合回来（AGENTS.md「出厂预置的
/// 绑定/开关，落点必须是用户能清空的载体」）。
fn default_email_suffixes() -> Vec<String> {
    [
        // 国内
        "qq.com",
        "163.com",
        "126.com",
        "foxmail.com",
        "sina.com",
        "sina.cn",
        "sohu.com",
        "139.com",
        "189.cn",
        "aliyun.com",
        "yeah.net",
        "21cn.com",
        "tom.com",
        // 国际
        "gmail.com",
        "outlook.com",
        "hotmail.com",
        "yahoo.com",
        "icloud.com",
        "live.com",
        "me.com",
        "proton.me",
        "protonmail.com",
        "aol.com",
        "gmx.com",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            suffixes: default_email_suffixes(),
            candidate_layout: LayoutIntent::default(),
        }
    }
}

/// Unicode 码点输入配置（[input.unicode]）。
///
/// 与 [`UrlConfig`] 同为前缀夺取式模式（正常输入累积到某前缀即夺取），共用
/// `Coordinator::try_prefix_hijack` 那道闸门与 `Rewind` 回退骨架。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnicodeConfig {
    /// 总开关（默认关闭，与 [`UrlConfig`] 同口径）。
    #[serde(default)]
    pub enabled: bool,
    /// 触发前缀（恰好匹配）。
    ///
    /// 按字面匹配、不做大小写归一，与 [`UrlConfig::prefixes`] 刻意不同：`U+` 的大小写携带真实信息。
    /// 出厂两条各对应一个入口：
    /// - `u+`：小写 u 进码表 `input_buffer`，`+` 在 `try_prefix_hijack` 触发夺取；
    /// - `U+`：Shift+U 先进临时英文（缓冲为 `U`），由 `handle_temp.rs` 的转交分支交给本模式。
    ///
    /// 不要配单字符前缀：`"u"` 会夺取一切 u 起手，废掉码表与拼音的 `u`；`"U"` 在空缓冲时被临英
    /// 先截走，够不着夺取点。
    #[serde(default = "default_unicode_prefixes")]
    pub prefixes: Vec<String>,
    /// 进入 Unicode 模式期间的候选布局（默认跟随全局）。
    #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
    pub candidate_layout: LayoutIntent,
}

fn default_unicode_prefixes() -> Vec<String> {
    vec!["u+".to_string(), "U+".to_string()]
}

impl Default for UnicodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            prefixes: default_unicode_prefixes(),
            candidate_layout: LayoutIntent::default(),
        }
    }
}

/// 快捷加词配置（[input.add_word]）。加词面板是覆盖在任意输入态之上的临时面板，
/// 故其布局意图优先于底层模式（见 `Coordinator::layout_intent`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddWordConfig {
    /// 加词面板期间的候选布局。默认竖排：逐字确认的面板竖排更易读，也是本项出现前的固定行为。
    #[serde(
        default = "default_add_word_layout",
        deserialize_with = "crate::tolerant_de::tolerant"
    )]
    pub candidate_layout: LayoutIntent,
}

fn default_add_word_layout() -> LayoutIntent {
    LayoutIntent::Vertical
}

impl Default for AddWordConfig {
    fn default() -> Self {
        Self {
            candidate_layout: default_add_word_layout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S2TConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_s2t_variant")]
    pub variant: String,
}

impl Default for S2TConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            variant: default_s2t_variant(),
        }
    }
}

/// 繁入简出（[input.t2s]）：出口把繁体转成简体。
///
/// 刻意没有 `variant`：台/港变体归一要自造 OpenCC 官方没有的反转表，场景小众。
/// 要加时同时补 `wind_transform::s2t::chain_for` 的分支。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct T2SConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmdbarConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 副作用命令候选（含 ActionEffect）在候选框渲染时的前缀标注（对齐 Go,默认 "⚡"）。
    #[serde(default = "default_candidate_prefix")]
    pub candidate_prefix: String,
    /// 选中折叠组进入展开态后，Backspace 先退回折叠视图（不删码）；关闭则直接删一个码。
    #[serde(default = "default_true")]
    pub backspace_collapse_group: bool,
    /// `$SS`/`$AA` 组展开后（候选全是该组成员）每页候选数；0 = 跟随 `per_page`。
    #[serde(default)]
    pub group_per_page: usize,
}

fn default_candidate_prefix() -> String {
    "⚡".to_string()
}

impl Default for CmdbarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            candidate_prefix: default_candidate_prefix(),
            backspace_collapse_group: true,
            group_per_page: 0,
        }
    }
}

// ───────────────────────── keys（全部按键）─────────────────────────

/// 全部按键绑定（[keys]，扁平）：原 hotkeys.* + 散在 input 的选择/导航键 + overflow。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeysConfig {
    // ── 热键（原 hotkeys.*）──
    #[serde(default = "default_toggle_mode_keys")]
    pub toggle_mode_keys: Vec<String>,
    #[serde(default = "default_true")]
    pub commit_on_switch: bool,
    /// Ctrl+空格 切换中英文。出厂开。
    ///
    /// 关掉后 DLL 不吃 Ctrl+空格、不做兜底切换，键原样交给宿主（IDEA 等的代码提示才按得出）；
    /// 系统输入法开关热键翻 OPENCLOSE 时服务端拒绝并拉回（见协调器 `ctrl_space_switch_rejected`）。
    /// 系统热键本身在 msctf 层就消费了按键，那只能让用户去系统设置里关。
    ///
    /// 消费端读 [`Self::ctrl_space_toggle_effective`]，不要直接读本字段。
    #[serde(default = "default_true")]
    pub ctrl_space_toggle: bool,
    #[serde(default = "default_switch_engine")]
    pub switch_engine: String,
    #[serde(default = "default_toggle_full_width")]
    pub toggle_full_width: String,
    #[serde(default = "default_toggle_punct")]
    pub toggle_punct: String,
    #[serde(default = "default_toggle_toolbar")]
    pub toggle_toolbar: String,
    #[serde(default = "default_open_settings")]
    pub open_settings: String,
    /// 打开词库管理页快捷键。出厂不绑——与 `toggle_t2s` 同理：这是个小众直达入口，
    /// 常规路径（菜单「词库管理…」/设置页内导航）已经够用，任何默认组合都在抢用户键位。
    /// 落点见 `Coordinator::open_dictionary`：直接跳设置端 `dict` 页并带上当前方案 id。
    #[serde(default = "default_open_dictionary")]
    pub open_dictionary: String,
    #[serde(default = "default_add_word")]
    pub add_word: String,
    #[serde(default = "default_open_add_word_dialog")]
    pub open_add_word_dialog: String,
    #[serde(default = "default_toggle_s2t")]
    pub toggle_s2t: String,
    /// 繁入简出开关键。出厂不绑：功能小众，任何默认值都在抢用户键位。设置页可配。
    ///
    /// 「不绑」写 `"none"` 而不是空串：core 侧两者等价，但设置页 hotkey 控件禁用态恒写 `"none"`，
    /// 出厂值是空串会让没动过的设置页被判「有未保存的更改」（`untouched_settings_produce_no_diff`）。
    #[serde(default = "default_toggle_t2s")]
    pub toggle_t2s: String,
    #[serde(default = "default_activate_ime")]
    pub activate_ime: String,
    #[serde(default = "default_pin_candidate")]
    pub pin_candidate: String,
    #[serde(default = "default_delete_candidate")]
    pub delete_candidate: String,
    /// 快捷输入（mix）专用选词上屏键，模板值域同 [`Self::pin_candidate`]
    /// （见 `hotkey::number_template_mods`），`"none"` 不绑。
    ///
    /// 只在快捷输入里生效、且先于置顶/删除判定：快捷输入的候选无词库落点，数字键又常被算式占着。
    ///
    /// 本键自己不进 TSF 热键表，送达靠两条现成通路：
    /// - 与置顶/删除同模板（出厂即是）：借它们的 SESSION 热键送达（WM_HOTKEY → `DispatchHotkey`）。
    /// - 模板不同：快捷输入期间恒有组合，TSF 对会话中未登记的 Ctrl/Alt 组合先吃下转发。代价是不受
    ///   `RegisterHotKey` 保护，Chromium 宿主（如 QQNT）可能对同一键再触发一次加速键。
    ///
    /// 不单独登记进 SESSION 表：那会让主输入路也吃掉它，而主输入路对它没有出口。
    #[serde(default = "default_quick_input_select")]
    pub quick_input_select: String,
    #[serde(default = "default_take_screenshot")]
    pub take_screenshot: String,
    /// 软键盘面板开关热键。
    ///
    /// 出厂绑定住在本字段而不是 [`Self::key_actions`]：那张表四层深合并表达不了删除，写在 L2 的
    /// 出厂条目被用户删掉后每次 `load()` 都会合并回来。标量字段清空即禁用。
    /// 给某个面配直通车（`softkeyboard:<面 id>`）仍写 `key_actions`：本字段是「一功能一键」，那张表是
    /// 「一键一功能」，刻意并存（见 docs/design/key-resolver-unification.md §9）。
    #[serde(default = "default_softkeyboard")]
    pub softkeyboard: String,
    #[serde(default)]
    pub global_hotkeys: Vec<String>,
    /// 已废弃且不再生效：方案直达热键已并入 [`Self::key_actions`]（`switch_schema:<方案id>`）。
    /// 不做自动迁移，只在加载期告警一次（[`Config::warn_legacy_schema_hotkeys`]）。
    ///
    /// 字段保留只为读出残留值以便告警，删掉的话 serde 会静默丢弃这一段。`rename` 保住原键名，
    /// `skip_serializing_if` 让它不再写出、退出 `config_schema` 登记。新代码不要读它。
    #[serde(
        rename = "schema_hotkeys",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub legacy_schema_hotkeys: HashMap<String, String>,
    /// 按键功能表：热键串 → 动词（如 `{ "ctrl+shift+n" = "toggle_schema:english" }`）。
    ///
    /// 「键 → 干什么」的通用表，同一套动词值域也用于方案级 `[key_actions]`
    /// （见 docs/design/schema-key-actions.md）。用 `BTreeMap`：编译成热键条目时遍历顺序即冲突时的
    /// 胜者顺序，`HashMap` 会让同一份配置在不同进程里表现不同。
    #[serde(default)]
    pub key_actions: BTreeMap<String, String>,
    /// 引导键物化迁移的版本号（0 = 未物化）。见 [`Config::materialize_key_actions`]。
    ///
    /// 置位后 [`Config::migrate_trigger_keys_into_key_actions`] 不再折算，[`Self::key_actions`] 成为
    /// 唯一真相源。必须住在用户层 config.toml 里而非独立标记文件，才能跟着备份/还原走，否则还原后
    /// 会被重新折算。用版本号而非 bool，日后可递增重跑。`skip_serializing_if` 让它退出
    /// `config_schema` 登记：它是迁移账本，不是用户可配项。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub key_actions_materialized: u32,
    /// 会话态按键功能表：键名 → 动词（如 `{ tab = "page_next", capslock = "page_prev" }`）。
    ///
    /// [`Self::key_actions`] 的姊妹表，管「正在组合一段输入」时。值域见 [`SessionAction`]，键名解析见
    /// `wind_keys::keymap::session_key_name_to_vk`（支持 `shift+tab` 这样的单修饰前缀）。
    ///
    /// `page_keys` / `highlight_keys` 等键组在 [`Self::effective_session_actions`] 里展开并入，
    /// 本表显式写的键优先。默认值刻意留在键组字段一侧：写进本表的话，用户清空键组就与
    /// 「从没配过」同形，清空意图会丢失（docs/design/session-key-actions.md §6）。
    /// 用 `BTreeMap` 的理由同 `key_actions`。
    #[serde(default)]
    pub session_actions: BTreeMap<String, String>,
    // ── 选择/导航键（原 input.*）──
    #[serde(default = "default_select_key_groups")]
    pub select_key_groups: Vec<String>,
    /// 翻页键组。消费点已改为 [`Self::session_actions`]，本字段是折算来源与
    /// 默认值的家（见那边的注释）。
    #[serde(default = "default_page_keys")]
    pub page_keys: Vec<String>,
    /// 高亮移动键组。同 [`Self::page_keys`]，折算进 `session_actions` 后消费。
    #[serde(default = "default_highlight_keys")]
    pub highlight_keys: Vec<String>,
    #[serde(default)]
    pub select_char_keys: Vec<String>,
    /// 候选无效按键策略（数字键/次选三选键/以词定字键超出候选范围时的处理）。
    #[serde(default)]
    pub overflow: OverflowConfig,
}

impl KeysConfig {
    /// Ctrl+空格 中英切换实际是否生效。两个入口任一说「关」就关：
    /// 1. [`Self::ctrl_space_toggle`] 开关；
    /// 2. [`Self::key_actions`] 里把 Ctrl+空格 绑成 `none`（「全局自定义按键 → 不启用」）。
    ///
    /// 第 2 条让用户最先想到的地方真的管用。只认 `none`：绑成真动作时系统热键仍可能先翻
    /// compartment，一并拒绝会让按 Ctrl+空格 什么都不发生。
    pub fn ctrl_space_toggle_effective(&self) -> bool {
        if !self.ctrl_space_toggle {
            return false;
        }
        let target = crate::hotkey::parse_hotkey("ctrl+space");
        !self.key_actions.iter().any(|(key, verb)| {
            let verb = verb.trim();
            (verb.is_empty() || verb.eq_ignore_ascii_case("none"))
                && crate::hotkey::parse_hotkey(key) == target
        })
    }
}

/// `KeysConfig::key_actions_materialized` 的 `skip_serializing_if`。
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

// [keys] 整表在 L2 缺失时用 Default::default()，而 config.getDefaults 以 Config::default() 为 L1 基线，
// 故必须手写 Default（而非 derive 的空值），否则设置页开关后默认键丢失。
fn default_toggle_mode_keys() -> Vec<String> {
    vec!["lshift".to_string(), "rshift".to_string()]
}
fn default_switch_engine() -> String {
    "ctrl+shift+e".to_string()
}
fn default_toggle_full_width() -> String {
    "shift+space".to_string()
}
fn default_toggle_punct() -> String {
    "ctrl+.".to_string()
}
fn default_add_word() -> String {
    "ctrl+equal".to_string()
}
fn default_open_add_word_dialog() -> String {
    "ctrl+shift+equal".to_string()
}
fn default_toggle_s2t() -> String {
    "ctrl+shift+j".to_string()
}
/// 见 [`KeysConfig::toggle_t2s`]：「不绑」的规范表示是 `"none"`，不是空串。
fn default_toggle_t2s() -> String {
    "none".to_string()
}
fn default_take_screenshot() -> String {
    "ctrl+shift+f11".to_string()
}
fn default_softkeyboard() -> String {
    "ctrl+shift+k".to_string()
}
fn default_activate_ime() -> String {
    "ctrl+shift+[".to_string()
}
fn default_pin_candidate() -> String {
    "ctrl+number".to_string()
}
fn default_delete_candidate() -> String {
    "ctrl+shift+number".to_string()
}
fn default_quick_input_select() -> String {
    "ctrl+number".to_string()
}
fn default_select_key_groups() -> Vec<String> {
    vec!["semicolon_quote".to_string()]
}
fn default_page_keys() -> Vec<String> {
    vec!["pageupdown".to_string(), "minus_equal".to_string()]
}
fn default_highlight_keys() -> Vec<String> {
    vec!["arrows".to_string(), "tab".to_string()]
}

/// 翻页键组名 → 折算出的 (键名, 动词) 对。
///
/// 键名必须与 `wind_keys::keymap::session_key_name_to_vk` 认的规范名逐字一致：跨 crate 拼写契约
/// 无编译期约束，写错表现为翻页键全没了且无报错。单测 `nav_group_names_resolve` 守这条。
fn page_key_group_binds(group: &str) -> &'static [(&'static str, SessionAction)] {
    match group.trim().to_lowercase().as_str() {
        "pageupdown" => &[
            ("pageup", SessionAction::PagePrev(PageOverflow::Global)),
            ("pagedown", SessionAction::PageNext(PageOverflow::Global)),
        ],
        "minus_equal" => &[
            ("minus", SessionAction::PagePrev(PageOverflow::Global)),
            ("equal", SessionAction::PageNext(PageOverflow::Global)),
        ],
        "brackets" => &[
            ("lbracket", SessionAction::PagePrev(PageOverflow::Global)),
            ("rbracket", SessionAction::PageNext(PageOverflow::Global)),
        ],
        "comma_period" => &[
            ("comma", SessionAction::PagePrev(PageOverflow::Global)),
            ("period", SessionAction::PageNext(PageOverflow::Global)),
        ],
        "shift_tab" => &[
            ("shift+tab", SessionAction::PagePrev(PageOverflow::Global)),
            ("tab", SessionAction::PageNext(PageOverflow::Global)),
        ],
        _ => &[],
    }
}

/// 高亮移动键组名 → 折算出的 (键名, 动词) 对。约束同 [`page_key_group_binds`]。
fn highlight_key_group_binds(group: &str) -> &'static [(&'static str, SessionAction)] {
    match group.trim().to_lowercase().as_str() {
        "arrows" => &[
            ("up", SessionAction::HighlightUp),
            ("down", SessionAction::HighlightDown),
        ],
        "tab" => &[
            ("shift+tab", SessionAction::HighlightUp),
            ("tab", SessionAction::HighlightDown),
        ],
        _ => &[],
    }
}

/// 二三候选键组名 → 折算出的 (键名, 动词) 对。约束同 [`page_key_group_binds`]。
///
/// 组内第一个键选次选、第二个选三选（`select_key_vks` 的位置语义 `pos + 1`）。
/// `lrshift` / `lrctrl` 展开成纯修饰键名：它们走 keyup 轻敲通路。
fn select_key_group_binds(group: &str) -> &'static [(&'static str, SessionAction)] {
    match group.trim().to_lowercase().as_str() {
        "semicolon_quote" => &[
            ("semicolon", SessionAction::SelectCandidate(2)),
            ("quote", SessionAction::SelectCandidate(3)),
        ],
        "comma_period" => &[
            ("comma", SessionAction::SelectCandidate(2)),
            ("period", SessionAction::SelectCandidate(3)),
        ],
        "lrshift" => &[
            ("lshift", SessionAction::SelectCandidate(2)),
            ("rshift", SessionAction::SelectCandidate(3)),
        ],
        "lrctrl" => &[
            ("lctrl", SessionAction::SelectCandidate(2)),
            ("rctrl", SessionAction::SelectCandidate(3)),
        ],
        _ => &[],
    }
}

/// 以词定字键组名 → 折算出的 (键名, 动词) 对。约束同 [`page_key_group_binds`]。
/// 值域与 [`select_key_group_binds`] 不同（这里有 `brackets`、没有修饰键组），故分成两张表，
/// 混用会让 `brackets` 静默失效。
fn select_char_group_binds(group: &str) -> &'static [(&'static str, SessionAction)] {
    match group.trim().to_lowercase().as_str() {
        "comma_period" => &[
            ("comma", SessionAction::SelectChar(1)),
            ("period", SessionAction::SelectChar(2)),
        ],
        "minus_equal" => &[
            ("minus", SessionAction::SelectChar(1)),
            ("equal", SessionAction::SelectChar(2)),
        ],
        "brackets" => &[
            ("lbracket", SessionAction::SelectChar(1)),
            ("rbracket", SessionAction::SelectChar(2)),
        ],
        _ => &[],
    }
}

impl KeysConfig {
    /// 有效的会话态按键表：四组键组配置的展开结果 ⊕ `session_actions`（后者优先）。
    ///
    /// 这是消费层的视图，不是存储层的改写：四组键组（`page_keys` / `highlight_keys` /
    /// `select_key_groups` / `select_char_keys`）与 `session_actions` 在配置里各自保持原样。
    /// 不要在 `normalize()` 里折算并清空原字段：设置页读的是存储，那样出厂默认会显示为未勾选、
    /// 保存后重开又变空、删掉的折算绑定还会被折算回来。
    ///
    /// 优先级两层都必须保持：
    /// 1. 组间：折算顺序 = 主输入路径的判定顺序（以词定字 `select_char_index` → 翻页/高亮
    ///    `apply_session_action` → 二三候选 `select_key_offset`），撞键时先折的赢；
    ///    `comma_period` 同属两个键组，顺序是唯一裁决依据。
    /// 2. 显式优先：`session_actions` 里写了的键覆盖折算结果。
    pub fn effective_session_actions(&self) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        let mut folded: Vec<(&'static str, SessionAction)> = Vec::new();
        for g in &self.select_char_keys {
            folded.extend_from_slice(select_char_group_binds(g));
        }
        for g in &self.page_keys {
            folded.extend_from_slice(page_key_group_binds(g));
        }
        for g in &self.highlight_keys {
            folded.extend_from_slice(highlight_key_group_binds(g));
        }
        for g in &self.select_key_groups {
            folded.extend_from_slice(select_key_group_binds(g));
        }
        for (key, action) in folded {
            out.entry(key.to_string())
                .or_insert_with(|| action.to_string());
        }
        // 显式表最后覆盖（含显式的 `none`——那是「在打字时禁用该键」，必须能压过折算）。
        for (key, verb) in &self.session_actions {
            out.insert(key.clone(), verb.clone());
        }
        out
    }
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            toggle_mode_keys: default_toggle_mode_keys(),
            commit_on_switch: true,
            ctrl_space_toggle: true,
            switch_engine: default_switch_engine(),
            toggle_full_width: default_toggle_full_width(),
            toggle_punct: default_toggle_punct(),
            toggle_toolbar: default_toggle_toolbar(),
            open_settings: default_open_settings(),
            open_dictionary: default_open_dictionary(),
            add_word: default_add_word(),
            open_add_word_dialog: default_open_add_word_dialog(),
            toggle_s2t: default_toggle_s2t(),
            toggle_t2s: default_toggle_t2s(),
            activate_ime: default_activate_ime(),
            pin_candidate: default_pin_candidate(),
            delete_candidate: default_delete_candidate(),
            quick_input_select: default_quick_input_select(),
            take_screenshot: default_take_screenshot(),
            softkeyboard: default_softkeyboard(),
            global_hotkeys: Vec::new(),
            legacy_schema_hotkeys: HashMap::new(),
            key_actions: BTreeMap::new(),
            // 0 = 未物化：新装机器由 `materialize_key_actions` 在服务启动时置位。
            key_actions_materialized: 0,
            session_actions: BTreeMap::new(),
            select_key_groups: default_select_key_groups(),
            page_keys: default_page_keys(),
            highlight_keys: default_highlight_keys(),
            select_char_keys: Vec::new(),
            overflow: OverflowConfig::default(),
        }
    }
}

/// 候选无效按键策略（[keys.overflow]）。
/// 每项取值："ignore"（吞键无效）/ "commit"（上屏当前高亮候选）/
/// "commit_and_input"（上屏高亮候选 + 追加按键字符）。默认全 ignore。
/// 仅 `number_key` 另有 "clear_and_input"（清空输入并上屏该数字，不顶高亮候选）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverflowConfig {
    /// 数字键超出当前页候选数量时
    #[serde(default = "default_overflow_behavior")]
    pub number_key: String,
    /// 次选/三选键候选不足时
    #[serde(default = "default_overflow_behavior")]
    pub select_key: String,
    /// 以词定字键候选词长度不足时
    #[serde(default = "default_overflow_behavior")]
    pub select_char_key: String,
    /// 可打印的翻页键（`-` `=` `,` `.` `[` `]`）在主路翻不动、且本批候选从未翻过页时：
    /// "ignore"（吞键）/ "commit_and_input"（顶高亮候选并输出该键的符号，走标点通路）。
    /// 只有这两档——「只顶不出字」没有诉求。
    #[serde(default = "default_overflow_behavior")]
    pub page_key: String,
}

fn default_overflow_behavior() -> String {
    "ignore".to_string()
}

impl Default for OverflowConfig {
    fn default() -> Self {
        Self {
            number_key: default_overflow_behavior(),
            select_key: default_overflow_behavior(),
            select_char_key: default_overflow_behavior(),
            page_key: default_overflow_behavior(),
        }
    }
}

// ───────────────────────── ui（外观）─────────────────────────

fn default_per_page() -> usize {
    7
}
fn default_grid_rows() -> usize {
    5
}
fn default_grid_cell_max_chars() -> usize {
    6
}
fn default_grid_index_fill() -> String {
    "rows".to_string()
}
fn default_grid_categories() -> Vec<String> {
    ["all", "single", "phrase", "rare"]
        .into_iter()
        .map(String::from)
        .collect()
}
/// 展开态可见行数的合法范围（[`UiCandidateConfig::grid_rows`]）。
pub const GRID_ROWS_RANGE: std::ops::RangeInclusive<usize> = 2..=10;
fn default_first_show_settle_ratio() -> f32 {
    0.8
}

fn default_fast_typing_window_ms() -> u64 {
    100
}

fn default_fast_first_show_fallback_ms() -> u64 {
    25
}

/// 候选窗首显策略的全局默认档。须与 `app_compat::FirstShowMode` 的 `#[default]` 一致，
/// 由 `global_first_show_mode_matches_enum_default` 钉住。
fn default_first_show_mode() -> String {
    "fast".to_string()
}

/// UI 配置（子表结构，对齐真实 config.toml：[ui.candidate] / [ui.font] / [ui.theme]）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiConfig {
    #[serde(default)]
    pub candidate: UiCandidateConfig,
    /// 候选注释的命名模板与默认 / 详细两态（`[ui.comment]`）。
    #[serde(default)]
    pub comment: crate::comment_lib::UiCommentConfig,
    #[serde(default)]
    pub font: UiFontConfig,
    #[serde(default)]
    pub theme: UiThemeConfig,
    #[serde(default)]
    pub mode_indicator: ModeIndicatorConfig,
    #[serde(default)]
    pub tooltip: TooltipConfig,
    #[serde(default)]
    pub status: StatusIndicatorConfig,
    #[serde(default)]
    pub toolbar: ToolbarConfig,
    /// 语言栏图标（Windows 任务栏输入指示器）的呈现参数。
    #[serde(default)]
    pub langbar: LangBarConfig,
    /// 非中文态在语言栏图标 / 工具栏模式格 / 状态气泡上显示的主字。
    #[serde(default)]
    pub labels: LabelsConfig,
    /// 注释词库挂载列表（`[[ui.comment_dicts]]`），供候选注释模板的 `${dict}` 变量查询。
    /// 数组顺序即优先级：同一个词在多个库里都有注释时取靠前的。表里还可以有自动派生项的
    /// 引用条目（`auto = true`），见 [`CommentDictSpec::auto`]。
    #[serde(default)]
    pub comment_dicts: Vec<CommentDictSpec>,
}

/// 非中文态的图标主字（`[ui.labels]`）。
///
/// 没有中文态：中文态主字是方案属性（方案文件 `[schema] icon_label`），放两处会争同一个显示位。
/// 不下沉到方案：英文半角是全局运行时状态，随方案切换改标签会让同一个状态显示不同。
/// 不复用 `english.schema.toml` 的 `icon_label`：英文半角与英文方案是两件事，且英文方案可被禁用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LabelsConfig {
    /// 英文半角态（左/右 Shift 切出）的主字。空 = 回落内置「英」。
    pub english: String,
    /// 大写锁定态的主字。空 = 回落内置「A」。
    pub caps_lock: String,
}

impl Default for LabelsConfig {
    fn default() -> Self {
        Self {
            english: DEFAULT_LABEL_ENGLISH.to_string(),
            caps_lock: DEFAULT_LABEL_CAPS.to_string(),
        }
    }
}

/// 英文半角态的内置主字。
pub const DEFAULT_LABEL_ENGLISH: &str = "英";
/// 大写锁定态的内置主字。
pub const DEFAULT_LABEL_CAPS: &str = "A";

impl LabelsConfig {
    /// 英文半角态该显示的主字（已截断 + 已回落，可直接送渲染）。
    ///
    /// 截断与回落必须封在这里：四个消费面（语言栏图标 / 工具栏 / 状态气泡 / 移动端）中语言栏那条
    /// 会写进 C++ 侧 `wchar_t[4]` 缓冲，漏掉截断会让宿主进程崩溃。
    pub fn english_label(&self) -> String {
        crate::schema::icon_label_or(&self.english, DEFAULT_LABEL_ENGLISH)
    }

    /// 大写锁定态该显示的主字（已截断 + 已回落）。
    pub fn caps_label(&self) -> String {
        crate::schema::icon_label_or(&self.caps_lock, DEFAULT_LABEL_CAPS)
    }
}

/// 一个注释词库（`[[ui.comment_dicts]]`）。
///
/// 独立成表而不塞进 `[[dictionaries]]` 加 `type` 区分：注释库不参与召回，复用词库表的话词库开关、
/// `base_order`、`composite::merge_search`、造词、学习每条路径都得记得跳过它，漏一处注释库的词
/// 就变成候选。独立成表则它从不在召回的数据结构里。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommentDictSpec {
    /// 稳定标识（供设置页与日志定位；不参与查询）。
    #[serde(default)]
    pub id: String,
    /// 显示名。
    #[serde(default)]
    pub label: String,
    /// 词库路径，相对数据目录（用户目录优先，回落安装目录）。
    #[serde(default)]
    pub path: String,
    /// 是否启用。缺省视为启用 —— 用户手写一条却忘了 `enabled = true` 时，
    /// 「配了没反应」比「多加载一份」难查得多。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 自动派生项的引用条目：`true` 时本条不带自己的 `path`，按 `id` 认领一个运行时检测出的
    /// 自动注释源（词库自带 `comment` 列，见 [`UiConfig::auto_comment_dicts`]）。
    ///
    /// 留空壳是因为自动项的启用态与优先级（即本表的数组位置）要能被用户改，另立顺序键会成为
    /// 第二个真相源。空壳里只有 `id` / `enabled` / 位置是用户的，`path` 与 `schemas` 每次启动
    /// 从检测结果重取。认领不到的空壳静默跳过，那是常态而非配置错误。
    #[serde(default)]
    pub auto: bool,
    /// 随包内置项的引用条目：与 `auto` 同形，按 `id` 去认领 `system.comments.toml` 的
    /// `[[dicts]]`，只保留用户的 `enabled` 与位置；认领不到静默丢弃。
    #[serde(default)]
    pub builtin: bool,
    /// 限定生效的方案 id；留空 = 全部方案。
    ///
    /// 注释库常是方案专属的，挂在无关方案上每次输入都白查一次。留空取「全部」而非「无」：
    /// 手写一条忘了 `schemas` 时，到处显示比哪都不显示好查，与 `enabled` 缺省即启用同一取舍。
    #[serde(default)]
    pub schemas: Vec<String>,
}

impl CommentDictSpec {
    /// 本库是否适用于给定方案。
    pub fn applies_to(&self, schema_id: &str) -> bool {
        self.schemas.is_empty() || self.schemas.iter().any(|s| s == schema_id)
    }
}

/// 工具栏配置（[ui.toolbar]，对齐 Go）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolbarConfig {
    /// 是否显示常驻工具栏（启动初值；运行时可经菜单切换）。
    #[serde(default = "default_true")]
    pub visible: bool,
    /// 前台应用全屏时自动隐藏工具栏（默认 true）。
    #[serde(default = "default_true")]
    pub hide_in_fullscreen: bool,
    /// 主动检测前台全屏态（默认 true），只在 `hide_in_fullscreen` 之下有意义。
    ///
    /// 进出全屏不产生 TSF 回调（如输入框里按 F11），只靠焦点/激活事件时缓存会停在旧值、工具栏
    /// 该隐没隐；开着则工具栏显示期间按固定节拍自查。复查是一条常驻线程，故留开关由用户定，
    /// 关掉后 F11 那类例外会回来。节拍与开销只写在 `coordinator/fullscreen_watch.rs` 的 `WATCH_TICK`。
    #[serde(default = "default_true")]
    pub fullscreen_watch: bool,
    /// 英文状态下隐藏工具栏（默认关）。
    ///
    /// 「英文状态」= 中英模式处于英文（`chinese_mode == false`），与全屏否决同档：只否决显示，
    /// 切回中文时照常出现。临时英文 / 大写锁定仍属中文态，不在此列。
    #[serde(default)]
    pub hide_in_english: bool,
    /// 自动隐藏：显示后超时无交互则淡出（默认关）。
    #[serde(default)]
    pub auto_hide: bool,
    /// 自动隐藏超时（秒，默认 5；下限 1 由协调器钳制）。
    #[serde(default = "default_toolbar_auto_hide_delay")]
    pub auto_hide_delay: u32,
    /// 自动隐藏的附加选项「假隐藏」（默认 false）：淡出后窗口仍留在原位（几乎透明、
    /// 可被鼠标命中），光标移到那里即重新显示，移开后按 `auto_hide_delay` 再淡出。
    /// 关掉则淡出后真隐藏，只有状态变化（切中英等）才重新出现。仅在 `auto_hide` 下有意义。
    ///
    /// 代价：那一小块屏幕的点击落在工具栏上，但光标移过去时工具栏会先亮出来。
    #[serde(default)]
    pub auto_hide_hover_reveal: bool,
    /// 纵向排列（默认 false=横条）。纵向是横向的转置：条宽取主题 `[toolbar] height`，
    /// 每格高取 `button_width`，故同一套主题几何在两个朝向下都成立、无需另配。
    /// 属用户偏好而非视觉设计，所以落在此处而非主题。
    #[serde(default)]
    pub vertical: bool,
    /// 显示哪些条目、按什么顺序。数组顺序即渲染顺序，合法项见 [`TOOLBAR_ITEM_KEYS`]。
    ///
    /// `-` 前缀 = 「在这个位置，但不显示」（如 `"-s2t"`）：设置页关掉某格时写的就是这个，
    /// 关掉再打开仍在原位；顺序与启用态因此留在同一个数组里。
    ///
    /// 留空 = 全部显示（未配置的默认，旧配置行为不变）；整条不要用 `visible = false`。
    /// 每项都带 `-` 只在手写配置时可达，协调器回落成全部显示并告警。
    /// 顺序有语义，设置页必须用列表编辑器而非 `checkbox_group`（`config-design-rules.md` §R3）。
    #[serde(default = "default_toolbar_items")]
    pub items: Vec<String>,
    /// 自定义按钮定义（`[[ui.toolbar.buttons]]`）。显不显示、排第几由 [`Self::items`] 里的
    /// `custom:<id>` 决定，两者是引用关系。出厂空；expert 档，设置页不提供编辑器
    /// （`config-design-rules.md` §R5）。
    #[serde(default)]
    pub buttons: Vec<ToolbarButtonSpec>,
}

/// 工具栏上的一个自定义按钮。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolbarButtonSpec {
    /// 稳定标识，被 `items` 里的 `custom:<id>` 引用。空 id 的按钮无法被引用（等于禁用）。
    #[serde(default)]
    pub id: String,
    /// 格内显示的文字。见 [`toolbar_label_trunc`] 的宽度口径。
    ///
    /// 刻意没有 tooltip 字段：工具栏没有悬停提示机制，渲染端消费不了的字段是配了也没反应的旋钮。
    #[serde(default)]
    pub label: String,
    /// 点击执行的 cmdbar 表达式，如 `proc.run("charmap.exe")` / `open("https://…")`。
    ///
    /// 值域就是命令栏 / 短语动作那一套（`wind-cmdbar`），故 `web.search(…)`、
    /// `wind.cli("schema switch wubi86")`、`key.tap("Ctrl+Shift+P")` 同样可用。
    #[serde(default)]
    pub action: String,
    /// 关掉即不渲染（`items` 里那条 `custom:<id>` 留着，供设置页记住位置）。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// 工具栏按钮 label 的宽度上限，按显示宽度计（CJK 记 2、其余记 1）。
pub const TOOLBAR_LABEL_MAX_WIDTH: usize = 2;

/// 工具栏按钮 label 的截断口径：去首尾空白后，取显示宽度不超过
/// [`TOOLBAR_LABEL_MAX_WIDTH`] 的前缀。即「一个汉字」或「两个 ASCII 字符」。
///
/// 与 `schema::icon_label_trunc` 同一条规则、共用 [`display_width_trunc`]，上限各留一个常量：
/// 两处物理约束不同（这里是等宽方格，那里是 16px 位图 + C++ 侧 `wcscpy_s` 缓冲）。
pub fn toolbar_label_trunc(raw: &str) -> String {
    display_width_trunc(raw, TOOLBAR_LABEL_MAX_WIDTH)
}

/// 按显示宽度截断的共享内核：去首尾空白后，取显示宽度不超过 `max_width` 的前缀。
///
/// 宽度口径见 [`is_wide_char`]（CJK 记 2、其余记 1）。字符不可拆，故结果宽度可能小于
/// `max_width`（`"A符"` 在上限 2 下只剩 `"A"`）。
pub(crate) fn display_width_trunc(raw: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut w = 0usize;
    for c in raw.trim().chars() {
        let cw = if is_wide_char(c) { 2 } else { 1 };
        if w + cw > max_width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

/// 是否按双宽字符计。粗口径，只为决定「一格能放几个」。
///
/// 覆盖 CJK 统一表意文字及扩展 A、兼容表意文字、假名、全角/半角形式区、CJK 符号与标点、
/// 注音符号、谚文；其余（拉丁、数字、常用符号、emoji）记 1。
///
/// emoji 记 1 意味着图标主字上限 2 最坏占 4 个 UTF-16 码元，C++ 侧 `_inputTypeLabel` 的容量
/// 按这个取。要把 emoji 划进双宽，先看 `schema::icon_label_limit_counts_scalar_values`。
pub fn is_wide_char(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F      // 谚文字母
        | 0x2E80..=0x303E    // CJK 部首补充 / 康熙部首 / CJK 符号与标点
        | 0x3041..=0x33FF    // 假名 / 注音 / 谚文兼容 / CJK 兼容
        | 0x3400..=0x4DBF    // CJK 扩展 A
        | 0x4E00..=0x9FFF    // CJK 统一表意文字
        | 0xA000..=0xA4CF    // 彝文
        | 0xAC00..=0xD7A3    // 谚文音节
        | 0xF900..=0xFAFF    // CJK 兼容表意文字
        | 0xFE30..=0xFE6F    // CJK 兼容形式 / 小型变体
        | 0xFF00..=0xFF60    // 全角形式
        | 0xFFE0..=0xFFE6    // 全角符号
        | 0x20000..=0x2FFFD  // CJK 扩展 B~F
        | 0x30000..=0x3FFFD  // CJK 扩展 G+
    )
}

/// 工具栏条目键的全集，同时也是默认值（全部显示）与默认顺序。
///
/// 与 [`STATUS_ITEM_KEYS`] 的差别：那份的顺序无语义（状态气泡的渲染顺序固定在代码里），
/// 这份的顺序就是渲染顺序。
pub const TOOLBAR_ITEM_KEYS: [&str; 7] = [
    "mode",
    "punct",
    "full_width",
    "s2t",
    "t2s",
    "soft_keyboard",
    "settings",
];

/// 出厂默认排布：全集的每一项都在，关掉的写 `-` 前缀。
///
/// 与 [`TOOLBAR_ITEM_KEYS`]（合法值域）分开，往值域里加新格不会改变任何人的工具栏外观。
/// - `s2t` / `t2s` 关着：简繁转换是少数人用的功能，默认多一格是打扰；勾上后这一格就是它的
///   鼠标开关（`t2s` 没有右键菜单，这是唯一的鼠标入口）。
/// - `soft_keyboard` 开着：软键盘没有别的显眼入口，偶尔要用时得马上找到。
const DEFAULT_TOOLBAR_SHOWN: [&str; 7] = [
    "mode",
    "punct",
    "full_width",
    "-s2t",
    "-t2s",
    "soft_keyboard",
    "settings",
];

fn default_toolbar_items() -> Vec<String> {
    DEFAULT_TOOLBAR_SHOWN
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for ToolbarConfig {
    fn default() -> Self {
        Self {
            visible: true,
            hide_in_fullscreen: true,
            fullscreen_watch: true,
            hide_in_english: false,
            auto_hide: false,
            auto_hide_delay: 5,
            auto_hide_hover_reveal: false,
            vertical: false,
            items: default_toolbar_items(),
            buttons: Vec::new(),
        }
    }
}

fn default_toolbar_auto_hide_delay() -> u32 {
    5
}

/// 语言栏图标（Windows 任务栏输入指示器）的呈现参数（`[ui.langbar]`）。
///
/// 模型是总开关 + 全局参数 + 有序规则表：「什么状态、画在哪个角、什么颜色、多大」组合起来
/// 必须是一张表，摊成扁平键会铺十几个且状态集合一变就要重排。顺序即优先级：一个角落只画
/// 最靠前命中的那一条（16px 上叠两个三角会糊成一片）。
///
/// 默认值与渲染侧（`wind_ui::langbar_icon::IconRenderer`）各存一份：wind-config 不能依赖
/// wind-ui，而配置约定要求每项在 `data/config.toml` 列出具体默认值。两份由 `wind-coordinator`
/// 的 `langbar_config_defaults_match_renderer` 钉住。颜色只存字符串，解析与回退在协调器侧。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LangBarConfig {
    /// 角标总开关。取值见 `wind_ui::langbar_icon::BadgeStyle::as_id()`：
    /// `none`（默认，不显示）/ `corner`（角标）。
    ///
    /// 关掉即全部规则都不画。无法识别的取值回落 `none`、不报错，写错一个词就退回装之前的样子。
    /// 早期实验性的形状 id（`corner_triangle` / `outer_ring` …）不做兼容别名。
    #[serde(default = "default_langbar_badge")]
    pub badge: String,
    /// 角标全局大小倍率（1.0 = 基准尺寸）。单条可用 `badges[].scale` 再乘一次。
    #[serde(default = "default_one")]
    pub badge_scale: f32,
    /// 角标不透明度的全局默认（0~1）。单条可用 `badges[].alpha_light` / `alpha_dark` 覆盖。
    ///
    /// 它同时是档位开关，逐条按有效不透明度判定：`= 1.0` 走「实心 + 挖空」（切掉周围一圈主字），
    /// `< 1.0` 走「半透明 + 保留主字」，见 `wind_ui::langbar_icon::IconRenderer::badge_alpha`。
    /// 出厂共用一个值是审美统一，不是技术限制。
    #[serde(default = "default_langbar_badge_alpha")]
    pub badge_alpha: f32,
    /// 角标规则表，顺序即优先级（见本结构体的文档）。
    #[serde(default = "default_langbar_badges")]
    pub badges: Vec<LangBarBadge>,
    /// 中文态主字在浅色任务栏上的颜色：`#RRGGBB` / `#RRGGBBAA`，`""`（默认）= 跟随主题
    /// （主题 token `langbar_text_cn`，主题也没给则内置黑）。
    ///
    /// 优先级「用户非空 > 主题 > 内置」，四格各自独立回落；合并在协调器的
    /// `effective_text_colors` 一处完成。非法色值记警告并只让这一格回落主题。
    /// `en` = 英文态标签（含不可输入态、CapsLock）。light/dark 指任务栏明暗。
    #[serde(default)]
    pub text_color_cn_light: String,
    /// 中文态主字在深色任务栏上的颜色，语义同 [`Self::text_color_cn_light`]（内置白）。
    #[serde(default)]
    pub text_color_cn_dark: String,
    /// 英文态主字在浅色任务栏上的颜色，语义同 [`Self::text_color_cn_light`]。
    #[serde(default)]
    pub text_color_en_light: String,
    /// 英文态主字在深色任务栏上的颜色，语义同 [`Self::text_color_cn_light`]（内置白）。
    #[serde(default)]
    pub text_color_en_dark: String,
}

/// 一条角标规则（`[[ui.langbar.badges]]`）：某状态成立时，在某角落用某色画一个角标。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LangBarBadge {
    /// 这条规则生效与否。「某个状态不想被打扰」的落点（如只想在英文标点时被提醒）；
    /// 做成开关而非删条目，关掉仍保留颜色与位置。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 什么时候画。取值见 `wind_ui::langbar_icon::BadgeState::as_id()`：
    /// `punct_cn`（中文标点）/ `punct_en`（英文标点）/ `full_width`（全角）。
    ///
    /// 与本段其余取值不同，无法识别的状态会让整条规则被丢弃（并告警）：状态没有合理的默认值。
    #[serde(default)]
    pub state: String,
    /// 画在哪个角。取值见 `wind_ui::langbar_icon::Corner::as_id()`：
    /// `top_left` / `top_right` / `bottom_right` / `bottom_left`。未知回落 `bottom_right`。
    #[serde(default = "default_langbar_corner")]
    pub corner: String,
    /// 浅色任务栏上的颜色：`#RRGGBB`，或 `""`（与主字同色）。不透明度见 [`Self::alpha_light`]。
    ///
    /// 亮暗分两个字段：同一个色在浅色与深色任务栏上的可辨度差很远，而渲染本就按明暗两档出变体。
    /// 出厂三条亮暗同色，是按两种任务栏上都立得住挑的。解析失败只让这一项回落「与主字同色」并告警。
    ///
    /// 旧写法 `auto` / `#RRGGBBAA` 由 `Config::migrate_langbar_badge_colors_value` 迁为
    /// `""` / `#RRGGBB` + `alpha_light`；定制层里未迁移的旧写法渲染侧仍能读。
    #[serde(default = "default_langbar_color_follow")]
    pub color_light: String,
    /// 深色任务栏上的颜色，语义同 [`Self::color_light`]。
    #[serde(default = "default_langbar_color_follow")]
    pub color_dark: String,
    /// 本条在浅色任务栏上自己的不透明度（0~1）。`None` = 跟随全局 `badge_alpha`。
    /// 填 `1.0` 会把这一条切到挖空档（实心 + 切掉周围主字），见
    /// `wind_ui::langbar_icon::IconRenderer::badge_alpha`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha_light: Option<f32>,
    /// 本条在深色任务栏上自己的不透明度，语义同 [`Self::alpha_light`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha_dark: Option<f32>,
    /// 本条相对全局倍率的额外倍率（1.0 = 与其他角标同大）。全局那级答「角标整体多大」，
    /// 这级答「这一条要不要比别人大」；出厂三条都是 1.0，不替用户分主次。
    /// `0` 或负数 = 这一条不画（与 `enabled = false` 等效，后者才是给人用的表达）。
    #[serde(default = "default_one")]
    pub scale: f32,
}

fn default_one() -> f32 {
    1.0
}
fn default_langbar_badge() -> String {
    "none".to_string()
}
fn default_langbar_badge_alpha() -> f32 {
    0.88
}
fn default_langbar_corner() -> String {
    "bottom_right".to_string()
}
/// 角标色值缺省 = 与主字同色。
fn default_langbar_color_follow() -> String {
    String::new()
}

/// 出厂规则表：中文标点右下蓝、英文标点右下橙、全角右上玫红，亮暗同色、大小相同。
///
/// 两条标点规则同占右下角不冲突：它们的状态互斥（同一时刻只可能命中一条），
/// 优先级规则处理的是「同时命中」，不是「配在同一角」。
fn default_langbar_badges() -> Vec<LangBarBadge> {
    let rule = |state: &str, corner: &str, color: &str, scale: f32| LangBarBadge {
        enabled: true,
        state: state.to_string(),
        corner: corner.to_string(),
        color_light: color.to_string(),
        color_dark: color.to_string(),
        alpha_light: None,
        alpha_dark: None,
        scale,
    };
    vec![
        rule("punct_cn", "bottom_right", "#2288E0", 1.0),
        rule("punct_en", "bottom_right", "#EE9922", 1.0),
        rule("full_width", "top_right", "#E0447A", 1.0),
    ]
}

impl Default for LangBarConfig {
    fn default() -> Self {
        Self {
            badge: default_langbar_badge(),
            badge_scale: 1.0,
            badge_alpha: default_langbar_badge_alpha(),
            badges: default_langbar_badges(),
            text_color_cn_light: String::new(),
            text_color_cn_dark: String::new(),
            text_color_en_light: String::new(),
            text_color_en_dark: String::new(),
        }
    }
}

/// 状态提示气泡配置（[ui.status]，对齐 Go）：中英/标点/全半角/方案切换的瞬时气泡。
/// 样式（字号/透明度/圆角/配色）跟随主题（theme.views.status）；此处为行为与位置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusIndicatorConfig {
    /// 是否启用状态提示气泡（false=完全不显示）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 自动隐藏时长（毫秒）；display_mode="always" 时忽略（常驻不隐藏）。
    #[serde(default = "default_status_duration")]
    pub duration: i32,
    /// 显示模式："temp"（临时,duration 后隐藏,默认）| "always"（常驻:激活/获焦时显示,失焦隐藏）。
    #[serde(default = "default_status_display_mode")]
    pub display_mode: String,
    /// 焦点切换到新的输入框时，是否也强制显示一次状态气泡（默认关）。
    ///
    /// 与 `display_mode` 正交：对 `always` 无额外效果；对 `temp` 是「换个输入框也弹一次」。
    /// 显示时绕过 `show_status` 的文本去重，否则同状态下焦点切换完全不显示。
    #[serde(default)]
    pub show_on_focus: bool,
    /// 方案名显示样式："full"（全名，默认）| "short"（图标短称 icon_label，回退全名）。
    #[serde(default = "default_schema_name_style")]
    pub schema_name_style: String,
    /// 位置模式："follow_caret"（跟随光标,默认）| "fixed"（固定屏幕坐标 custom_x/custom_y）|
    /// 锚点 `screen_center` / `screen_top_left` / `screen_top_right` / `screen_bottom_left` /
    /// `screen_bottom_right`（前台窗口所在显示器工作区）、`window_center` / `window_bottom_left`
    /// （前台窗口可见边框）。值域 = [`crate::app_compat::STATUS_POSITION_MODES`]；读取请走
    /// [`Self::position`]（写错回落跟随光标）。
    #[serde(default = "default_status_position_mode")]
    pub position_mode: String,
    /// 光标坐标不可信时的兜底位置（只在 follow_caret 下生效）："last"（最近一次有效坐标,
    /// 默认,= 本项引入前的行为）| "hide"（不显示）| 上面 7 个锚点。值域 =
    /// [`crate::app_compat::STATUS_FALLBACK_POSITIONS`]；读取请走 [`Self::fallback`]（写错回落 last）。
    #[serde(default = "default_status_fallback_position")]
    pub fallback_position: String,
    /// follow_caret 下的默认位置："below"（光标下方，默认）| "above"（光标上方）。放不下翻到另一侧；
    /// 与候选窗重叠时自动避让（见 docs/design/status-tip-avoid-candidates.md）。读取请走 [`Self::caret_above`]。
    #[serde(default = "default_status_caret_side")]
    pub caret_side: String,
    /// follow_caret 下相对默认位置（光标下方、左边缘对齐光标）的水平偏移（像素，正=右）。
    #[serde(default)]
    pub offset_x: i32,
    /// follow_caret 下相对默认位置的垂直偏移（像素，正=下）。
    #[serde(default)]
    pub offset_y: i32,
    /// fixed 模式的固定屏幕 X（像素）。
    #[serde(default)]
    pub custom_x: i32,
    /// fixed 模式的固定屏幕 Y（像素）。
    #[serde(default)]
    pub custom_y: i32,
    /// 气泡显示哪些内容段（按此处顺序无关，渲染顺序固定）。合法项：
    /// `schema`（输入方案 / 中英）、`punct`（标点状态）、`full_width`（全半角）、
    /// `s2t`（简繁）、`caps`（大写锁定）。
    ///
    /// 留空 = 全部显示：既是"未配置"的合理默认，也让旧配置文件（无此键）行为不变。
    /// 用列表而非逐项 bool，是为了后续增加状态项时不必再动配置结构。
    #[serde(default = "default_status_items")]
    pub items: Vec<String>,
}

/// 状态气泡内容段的全集，同时也是默认值（全部显示）。
pub const STATUS_ITEM_KEYS: [&str; 5] = ["schema", "punct", "full_width", "s2t", "caps"];

fn default_status_items() -> Vec<String> {
    STATUS_ITEM_KEYS.iter().map(|s| s.to_string()).collect()
}

fn default_schema_name_style() -> String {
    "full".to_string()
}
fn default_status_duration() -> i32 {
    800
}
fn default_status_display_mode() -> String {
    "temp".to_string()
}
fn default_status_position_mode() -> String {
    "follow_caret".to_string()
}
fn default_status_fallback_position() -> String {
    "last".to_string()
}
fn default_status_caret_side() -> String {
    "below".to_string()
}

impl StatusIndicatorConfig {
    /// 解析后的定位方式；值域外的字符串回落出厂默认（跟随光标）——字段是自由字符串，
    /// 写错不会让配置加载失败，这里负责把它落回默认档。
    pub fn position(&self) -> crate::app_compat::StatusPositionMode {
        crate::app_compat::StatusPositionMode::from_config(&self.position_mode)
            .unwrap_or(crate::app_compat::StatusPositionMode::FollowCaret)
    }

    /// 默认位置是不是光标上方；值域外的字符串按出厂（下方）。
    pub fn caret_above(&self) -> bool {
        self.caret_side.eq_ignore_ascii_case("above")
    }

    /// 解析后的兜底位置；值域外的字符串回落出厂默认（`last`）。
    pub fn fallback(&self) -> crate::app_compat::StatusFallback {
        crate::app_compat::StatusFallback::from_config(&self.fallback_position)
            .unwrap_or(crate::app_compat::StatusFallback::Last)
    }
}

impl Default for StatusIndicatorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            duration: default_status_duration(),
            display_mode: default_status_display_mode(),
            show_on_focus: false,
            schema_name_style: default_schema_name_style(),
            position_mode: default_status_position_mode(),
            fallback_position: default_status_fallback_position(),
            caret_side: default_status_caret_side(),
            offset_x: 0,
            offset_y: 0,
            custom_x: 0,
            custom_y: 0,
            items: default_status_items(),
        }
    }
}

/// 模式指示器配置（[ui.mode_indicator]）：进入临时拼音/双拼/快捷/英文/快符等模式时的标识。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeIndicatorConfig {
    /// 显示样式："short"（短称，默认）| "full"（全称）| "none"（不显示）。
    #[serde(default = "default_mode_indicator_style")]
    pub style: String,
}

fn default_mode_indicator_style() -> String {
    "short".to_string()
}

impl Default for ModeIndicatorConfig {
    fn default() -> Self {
        Self {
            style: default_mode_indicator_style(),
        }
    }
}

/// 模式指示样式（解析自 ui.mode_indicator.style）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeIndicatorStyle {
    /// 短称（拼/双/快/英/符）。
    Short,
    /// 全称（临时拼音 等）。
    Full,
    /// 不显示。
    None,
}

impl ModeIndicatorStyle {
    pub fn from_config(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "full" => Self::Full,
            "none" => Self::None,
            _ => Self::Short,
        }
    }
}

impl ModeIndicatorConfig {
    pub fn parsed_style(&self) -> ModeIndicatorStyle {
        ModeIndicatorStyle::from_config(&self.style)
    }
}

/// 候选窗配置（[ui.candidate]）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiCandidateConfig {
    /// 候选每页显示数（默认 7，对齐 Go 版本）
    #[serde(default = "default_per_page")]
    pub per_page: usize,
    /// 扩展档每页候选数（临拼/快捷/短语等 overlay 模式用，0=与 per_page 相同）。
    #[serde(default)]
    pub per_page_extended: usize,
    #[serde(default)]
    pub layout: String,
    /// 编码（组合区）显示方式。
    /// - "app_inline"（默认）：编码内嵌应用光标处，候选窗不显示 preedit 栏
    /// - "candidate_top"：候选窗顶部独立 preedit 栏
    /// - "candidate_inline"：编码作为候选窗首单元内联
    ///
    /// 后两种依赖候选窗，宿主自绘候选的场合（TSF UI-less 全屏游戏 / SDL、D3D 独占全屏）自动降级为
    /// `app_inline`，否则编码无处显示。见 `wind-coordinator` 的 `preedit_uses_placeholder` 及
    /// `docs/design/game-compat-tsf-uielement.md` §4.4 末。
    #[serde(default = "default_preedit_display")]
    pub preedit_display: String,
    /// 拼音编码里音节之间显示的分隔符（`xiao'peng'you` 里的 `'`），默认 `'`，可留空。
    ///
    /// 只改显示：应用里的组合串与编码栏都换，引擎缓冲、手动分隔符的输入与切分不变。
    /// 用于 Unreal Engine 等不接受 `'` 的程序。只作用于主路拼音；临时英文等模式里的 `'` 是正文。
    #[serde(default = "default_syllable_separator")]
    pub syllable_separator: String,
    #[serde(default)]
    pub hide_window: bool,
    /// 候选文本字号：0（默认）= 跟随主题 `behavior.font_size`，>0 = 用户指定。
    ///
    /// 0 号字本身不合法，故可作「跟随」哨兵（R3「外观覆盖主题」）。旧的 `font_size_follow_theme`
    /// 已并入本键，迁移见 [`Config::migrate_font_size_follow_theme_value`]。
    #[serde(default)]
    pub font_size: f32,
    /// 翻页栏显示覆盖："" 跟随主题 / "hide" / "auto"(>1页) / "always"。
    ///
    /// 可按排布分档（见 [`ByLayout`]）：`"hide"` 横竖共用，`"h:hide v:always"` 分设。横排的翻页栏
    /// 挤占所有候选共享的那一行，竖排多一行代价小，两者常需不同取值。
    #[serde(default)]
    pub pager_bar_display: ByLayout<String>,
    /// 页码文字显示覆盖："" 跟随主题 / "show" / "hide"。
    #[serde(default)]
    pub page_number_display: ByLayout<String>,
    /// 首显容差系数（内部选项，不进设置页）：首帧用了非权威坐标时，随后的权威坐标与它相差在
    /// `行高 × 本系数` 以内就不再校正，十几像素不动比「跳一下修正」更稳。换行/重排的偏差通常
    /// ≥2 个行高，仍会校正。0 = 禁用容差（任何偏差都校正）。默认 0.8。
    #[serde(default = "default_first_show_settle_ratio")]
    pub first_show_settle_ratio: f32,
    /// 连续输入判定窗口（内部选项，毫秒）：两次按键间隔小于此值即视为"连续快速输入"，
    /// 此时 fast 档直接采信首条试探坐标、不再比对上一轮权威坐标。
    /// 依据是连打时光标顺序前移、不发生重排，且用户对"跟手"的敏感度远高于十几像素的偏差。
    /// 0 表示禁用该快路径。默认 100。
    #[serde(default = "default_fast_typing_window_ms")]
    pub fast_typing_window_ms: u64,
    /// fast 档的首显兜底超时（内部选项，毫秒）：等不到试探/权威坐标就用现有坐标先显示。默认 25。
    ///
    /// 必须远小于 wait 档的 150ms：有的宿主（如 Word）从不发 `OnLayoutChange`、组合坐标要上百毫秒
    /// 才到，而连打时组合只活几十毫秒，上屏即作废 timer，150ms 兜底永远到不了期，候选窗大量不显示。
    /// 取小值让 fast 在这类宿主上退化成 instant；发 `OnLayoutChange` 的宿主几毫秒就有试探坐标，不受影响。
    #[serde(default = "default_fast_first_show_fallback_ms")]
    pub fast_first_show_fallback_ms: u64,
    /// 候选窗首显策略的全局默认档："fast"（默认）/ "wait" / "instant"，「快」与「准」的取舍：
    /// - `fast`：等到坐标「可信」即放行（首帧信任门保底）；
    /// - `wait`：一律等宿主 reflow 后的权威坐标，最稳但首显有固定延迟，连打时迟钝；
    /// - `instant`：完全不等，沿用上一次坐标，光标移动过就会先错位再跳。
    ///
    /// 右键菜单「应用独立配置 ▸ 候选窗首显」可按应用覆盖；「跟随全局」= 清除覆盖。认不出的值回落默认档。
    #[serde(default = "default_first_show_mode")]
    pub first_show_mode: String,
    /// 候选文本最大显示字数，超出截断（0=不限）。
    #[serde(default)]
    pub max_chars: usize,
    /// 横排候选窗的最小宽度，单位 dp（0=关闭，跟随内容）。
    ///
    /// 下限量的是整个窗口（含翻页栏、内边距、序号列），候选照常紧凑左对齐，凑不满的宽度留在右侧，
    /// 从而能直接做到「窗口宽度不变」。单位是 dp 而非字符：窗口宽度里有与字号无关的部分；dp 随 DPI 缩放。
    /// 横竖各配一项（见 [`Self::min_window_width_vertical`]）：可用横向空间差一个数量级。
    /// 与 [`Self::max_chars`] 不冲突，后者仍封候选文字的上限。
    #[serde(default)]
    pub min_window_width_horizontal: u32,
    /// 竖排候选窗的最小宽度，单位 dp（0=关闭，跟随内容）。语义同
    /// [`Self::min_window_width_horizontal`]。与主题 `behavior.vertical_max_width` 冲突时下限优先：
    /// 用户显式配的抗抖动宽度不该被主题的裁切上限压回去。
    #[serde(default)]
    pub min_window_width_vertical: u32,
    /// 横排候选窗的最小高度，单位 dp（0=关闭，跟随内容）。窗口高度仍会随编码栏出现/消失而变，
    /// 故对横排同样有意义。
    ///
    /// 窗口翻到光标上方时，多出的高度补在顶部：上方显示时底边贴光标，空白压在下面会把候选顶离光标。
    #[serde(default)]
    pub min_window_height_horizontal: u32,
    /// 竖排候选窗的最小高度，单位 dp（0=关闭，跟随内容）。
    ///
    /// 与 [`Self::min_rows`] 可同时配、取较大者：`min_rows` 只数候选行，管不着翻页栏、编码栏的
    /// 出现/消失；本项量窗口总高。只想稳住候选区、让翻页栏照常伸缩时用 `min_rows`。
    #[serde(default)]
    pub min_window_height_vertical: u32,
    /// 竖排候选窗的最小行数（0=关闭，跟随候选数量）。
    ///
    /// 不足时补等高的透明占位行，上限钳到当前生效的每页候选数（[`Self::per_page`] /
    /// [`Self::per_page_extended`]）。横排不适用；候选为空的提示态（临拼/临英/网址刚进入）不补。
    /// 只稳住候选区，要连翻页栏、编码栏一并罩住用 [`Self::min_window_height_vertical`]。
    #[serde(default)]
    pub min_rows: usize,
    /// 候选展开（卷轴，出厂关）：候选多于一页时按翻页键，候选窗原地展开成多行网格，
    /// 一行就是原来的一页。设计见 `docs/design/candidate-grid-expand.md`。
    ///
    /// 只对横排生效：竖排 / 旋转态的自然形态是「多列并排」，不在本项范围内。
    #[serde(default)]
    pub grid_expand: bool,
    /// 展开态的可见行数（出厂 5）。生效值夹在 [`GRID_ROWS_RANGE`] 内。
    #[serde(default = "default_grid_rows")]
    pub grid_rows: usize,
    /// 展开态每格最多显示几个字（出厂 6，0 = 只受 [`Self::max_chars`] 约束）。
    ///
    /// 网格按列对齐，一个长词会把整列撑宽，故比单行的 `max_chars` 收得更紧。被截掉的
    /// 部分由悬停提示的 `${full_text}` 段补齐。
    #[serde(default = "default_grid_cell_max_chars")]
    pub grid_cell_max_chars: usize,
    /// 展开态下序号（与数字键）从高亮行起怎么往下顺延（1–0 共 10 个）：
    /// - `"rows"`（出厂）：只补整行——每页 5 个时高亮行 1–5、下一行 6–0；每页 7 个时只编高亮行。
    /// - `"cells"`：逐格编到 0 为止，可以停在半行。
    /// - `"off"`：只编高亮行。
    #[serde(default = "default_grid_index_fill")]
    pub grid_index_fill: String,
    /// 展开态下 PageUp/PageDown 是否翻一整屏（出厂开）；其余翻页键（如 `-` `=`）恒逐行移动。
    /// 关掉后 PageUp/PageDown 也逐行。
    #[serde(default = "default_true")]
    pub grid_pageupdown_screen: bool,
    /// 展开态的分类标签：`all` 全部 / `single` 单字 / `phrase` 词组 / `rare` 生僻 / `symbol` 符号。
    /// 只决定有哪些，顺序固定按上面这个次序（「全部」恒在首位）。少于两个时不显示标签栏。
    /// 未知名字忽略。
    #[serde(default = "default_grid_categories")]
    pub grid_categories: Vec<String>,
    /// 上方注释条（默认关）：开启后注释模板里的第一个字面 `\n`（写在 `{…}` 之外）把注释
    /// 拆成上下两段，上段显示在候选上方一行，下段仍在候选右侧。关闭时不拆分，行为与本项
    /// 引入前逐字节一致。
    ///
    /// 全局一份：不进模式级 / 方案级三态，也不分横竖。
    #[serde(default)]
    pub comment_above: bool,
    /// 自动把词库自带的 `comment` 列当注释源用（默认开）。
    ///
    /// 词库显式写了 `columns: [text, code, comment]` 时，那份注释是作者想让人看到的，无需用户再手工
    /// 挂一遍。判据是显式声明：无 `columns:` 的码表按默认列序读会把编码当注释，见
    /// `wind_reverse::declares_comment_column`。关掉则一条都不派生，`[[ui.comment_dicts]]` 里的引用
    /// 空壳一并失效。模板决定显示什么，本键决定去哪儿查。
    #[serde(default = "default_true")]
    pub auto_comment_dicts: bool,
    /// 竖排注释段的最大字数（0=不限），超出截断并加 `…`。
    ///
    /// 默认 0：非 0 默认会让存量用户的注释突然变短。横竖各一份：横排共享一行宽度，竖排每行独占，
    /// 预算差一个数量级。旧键 `comment_max_chars` 经 [`Config::migrate_comment_max_chars_value`]
    /// 抄进两份。开启 [`Self::comment_above`] 后上下两段各自计算。
    #[serde(default)]
    pub comment_max_chars_vertical: usize,
    /// 横排注释段的最大字数（0=不限）。见 [`Self::comment_max_chars_vertical`]；
    /// 开启 [`Self::comment_above`] 后同样按上下两段各自计算。
    #[serde(default)]
    pub comment_max_chars_horizontal: usize,
    /// 自定义序号标签，一槽一项（如 `["a","s","d"]`、`["Ⅰ","Ⅱ","Ⅲ"]`；空表=全部默认 1-9）。
    ///
    /// 每槽是字符串而非字符：`(1)`、罗马数字、ZWJ emoji 都是多字符。须与主题 `views.index.labels`
    /// 同型，由协调器 `resolve_index_label` 三级裁决。槽内空串 = 该槽让位给主题层，中间空槽有意义，
    /// 写回时不可「遇空即停」截断。
    #[serde(default)]
    pub index_labels: Vec<String>,
    /// 候选窗在光标上方时反转候选排列顺序。仅竖排生效：横排候选左右并列，
    /// 反转与窗口在上在下无关，只会把读序倒过来，故对横排一律忽略。
    ///
    /// 反转生效期间 `highlight_up` / `highlight_down`（出厂的 ↑↓ 与 Shift+Tab/Tab）
    /// 按屏幕上看到的方向走，与排列一致；翻页键不受影响（页与页之间无空间关系）。
    #[serde(default)]
    pub flip_when_above: bool,
    /// 候选窗在光标上方时交换编码栏与候选栏位置（编码区沉底贴光标）。与 flip_when_above 正交，可叠加。
    #[serde(default)]
    pub swap_preedit_when_above: bool,
    /// 翻页栏并入编码所在行、右对齐显示（竖排省一行）。两种编码形态的落点不同、表现一致：
    /// 独立编码栏（`candidate_top`）并进栏行右端，内联编码（`candidate_inline`）并进编码自己那一行。
    /// 横排下翻页栏本就落在行尾，此项无影响；蒙文旋转态维持底部独立行（并入会让箭头跟着转 90°）。
    #[serde(default)]
    pub pager_in_preedit: bool,
    /// 双击候选窗空白处（编码栏 / 内边距）把候选窗当前画面截图复制到剪贴板（默认关）。
    ///
    /// 只认空白处：候选项按下即上屏、窗口消失，翻页键会换页；空白处单击只起拖动，双击不误伤。
    #[serde(default)]
    pub double_click_screenshot: bool,
    /// 候选窗定位方式："follow_caret"（默认，跟随光标）/ "fixed"（固定屏幕坐标）。
    /// fixed 下窗口不再随光标移动，也不再上翻（flip/swap_when_above 随之失去意义）。
    #[serde(default = "default_candidate_position_mode")]
    pub position_mode: String,
    /// 候选窗相对光标的用户偏移（dp，X 正值向右，Y 正值恒为「远离光标」：下方定位向下推、
    /// 上翻定位向上推），与主题 `window.position_offset` 相加。0 = 不偏移。
    /// 只在跟随光标时生效；固定位置与拖动是显式意图，不叠加。
    #[serde(default)]
    pub offset_x: i32,
    #[serde(default)]
    pub offset_y: i32,
    /// 主题阴影覆盖："follow"（默认，用主题的）/ "off"（不画阴影）。
    ///
    /// 主题的阴影画在窗口位图里，窗口因此比内容大一圈透明边；个别宿主对这圈透明边不兼容时
    /// 用 "off" 让窗口回到与内容等大。定位补偿由窗口扩边自动派生，随之归零，不需另调。
    /// 主题自带的 `position_offset` 不受影响。仅作用于候选窗（macOS 用系统原生阴影，不经此项）。
    #[serde(default = "default_candidate_shadow")]
    pub shadow: String,
    /// 固定模式下的内容左上屏幕坐标（不含阴影扩边），仅 position_mode="fixed" 生效。
    /// 由用户拖动候选窗落盘，设置页刻意不暴露：手填绝对坐标既不直观又会与拖动互相覆盖
    /// （与 ui.status.custom_x/y 同一决策）。(0,0) 视作"尚未设定"，首次显示落到屏幕默认锚点。
    #[serde(default)]
    pub custom_x: i32,
    #[serde(default)]
    pub custom_y: i32,
}

fn default_syllable_separator() -> String {
    "'".to_string()
}

fn default_preedit_display() -> String {
    "app_inline".to_string()
}

fn default_candidate_shadow() -> String {
    "follow".to_string()
}

fn default_candidate_position_mode() -> String {
    "follow_caret".to_string()
}

/// 编码显示方式（解析自 ui.candidate.preedit_display）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreeditDisplay {
    /// 内嵌应用光标处，候选窗不显示 preedit。
    AppInline,
    /// 候选窗顶部独立 preedit 栏。
    CandidateTop,
    /// 编码作为候选窗首单元内联。
    CandidateInline,
}

impl PreeditDisplay {
    /// 解析配置字符串（空/未知 → 默认 AppInline）。
    pub fn from_config(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "candidate_top" => Self::CandidateTop,
            "candidate_inline" => Self::CandidateInline,
            _ => Self::AppInline,
        }
    }

    /// 配置字符串形式（持久化用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::AppInline => "app_inline",
            Self::CandidateTop => "candidate_top",
            Self::CandidateInline => "candidate_inline",
        }
    }

    /// 是否内嵌应用（候选窗不显示 preedit）。
    pub fn in_app(self) -> bool {
        matches!(self, Self::AppInline)
    }

    /// 是否编码内联候选首单元（对应旧 preedit_embedded）。
    pub fn embedded(self) -> bool {
        matches!(self, Self::CandidateInline)
    }

    /// 循环切换：内嵌应用 → 候选顶部 → 候选内联 → 内嵌应用。
    pub fn next(self) -> Self {
        match self {
            Self::AppInline => Self::CandidateTop,
            Self::CandidateTop => Self::CandidateInline,
            Self::CandidateInline => Self::AppInline,
        }
    }

    /// 简短中文名（状态提示用）。
    pub fn label(self) -> &'static str {
        match self {
            Self::AppInline => "编码:内嵌应用",
            Self::CandidateTop => "编码:候选顶部",
            Self::CandidateInline => "编码:候选内联",
        }
    }
}

impl Default for UiCandidateConfig {
    fn default() -> Self {
        Self {
            per_page: default_per_page(),
            first_show_settle_ratio: default_first_show_settle_ratio(),
            fast_typing_window_ms: default_fast_typing_window_ms(),
            fast_first_show_fallback_ms: default_fast_first_show_fallback_ms(),
            first_show_mode: default_first_show_mode(),
            per_page_extended: 0,
            layout: "horizontal".to_string(),
            preedit_display: default_preedit_display(),
            syllable_separator: default_syllable_separator(),
            hide_window: false,
            font_size: 0.0,
            pager_bar_display: ByLayout::default(),
            page_number_display: ByLayout::default(),
            max_chars: 16,
            // 五项下限均出厂关闭：任何非零默认都会让存量用户升级后候选窗突然变宽/变高。
            min_window_width_horizontal: 0,
            min_window_width_vertical: 0,
            min_window_height_horizontal: 0,
            min_window_height_vertical: 0,
            min_rows: 0,
            grid_expand: false,
            grid_rows: default_grid_rows(),
            grid_cell_max_chars: default_grid_cell_max_chars(),
            grid_index_fill: default_grid_index_fill(),
            grid_pageupdown_screen: true,
            grid_categories: default_grid_categories(),
            comment_above: false,
            auto_comment_dicts: true,
            comment_max_chars_vertical: 0,
            comment_max_chars_horizontal: 0,
            index_labels: Vec::new(),
            flip_when_above: false,
            swap_preedit_when_above: false,
            pager_in_preedit: false,
            double_click_screenshot: false,
            position_mode: default_candidate_position_mode(),
            offset_x: 0,
            offset_y: 0,
            shadow: default_candidate_shadow(),
            custom_x: 0,
            custom_y: 0,
        }
    }
}

impl UiCandidateConfig {
    /// 是否为固定位置模式（position_mode="fixed"）。
    pub fn is_fixed_position(&self) -> bool {
        self.position_mode.eq_ignore_ascii_case("fixed")
    }

    /// 解析后的编码显示方式。
    pub fn preedit(&self) -> PreeditDisplay {
        PreeditDisplay::from_config(&self.preedit_display)
    }

    /// 用户配置的第 `i` 个序号槽位（0 基）：仅当该槽位存在且非空时返回，否则 None。
    /// 供协调器 `resolve_index_label` 裁决「用户 > 主题 > 默认」。空串按 None 处理，
    /// 是「中间空槽让位」的落点。不提供「跳过主题直接回退数字」的方法：只有协调器持有主题槽位。
    pub fn user_index_label(&self, i: usize) -> Option<String> {
        self.index_labels.get(i).filter(|s| !s.is_empty()).cloned()
    }

    /// 叠上方案级通用补丁（`[candidate]` 里已通过 [`crate::config_schema::split_candidate_patch`]
    /// 校验的键），得到本方案下的生效配置。补丁空时等价于 `clone`。
    ///
    /// 做法是 序列化 → 覆盖同名叶子 → 反序列化：不为每个键写一行赋值，新增可覆盖键不用改这里。
    /// 任何一步失败（补丁已校验，理论上不会）都退回原值——宁可「方案没生效」也不能带出半份配置。
    pub fn folded_with(&self, patch: &toml::Table) -> Self {
        if patch.is_empty() {
            return self.clone();
        }
        let Ok(toml::Value::Table(mut tree)) = toml::Value::try_from(self) else {
            return self.clone();
        };
        for (k, v) in patch {
            tree.insert(k.clone(), v.clone());
        }
        toml::Value::Table(tree)
            .try_into()
            .unwrap_or_else(|_| self.clone())
    }

    /// 竖排最小行数的生效值：按每页候选数封顶（0=不补）。
    ///
    /// 扩展档（`per_page_extended`）更少时仍可能补出略超一页的空行，为此每次候选更新重下发不划算。
    /// 钳制放在配置层：候选窗不知道分页概念。
    pub fn effective_min_rows(&self) -> u32 {
        if self.min_rows == 0 {
            return 0;
        }
        self.min_rows.min(self.per_page.max(1)) as u32
    }

    /// 当前排布对应的注释段最大字数（0=不限）。
    pub fn comment_max_chars(&self, vertical: bool) -> usize {
        if vertical {
            self.comment_max_chars_vertical
        } else {
            self.comment_max_chars_horizontal
        }
    }

    /// 按 max_chars 截断候选显示文本（0=不限）。超出时截断并加省略号 [`TRUNCATION_MARK`]
    /// 提示"过长"（仅影响显示；上屏用完整原文，见 coordinator 候选下发）。
    ///
    /// 「字」按字素簇计（口径同悬停提示的截断与折行）：按码位切会把 emoji ZWJ 序列、
    /// 组合符、变体选择符拦腰切开，屏上冒出半个字形。
    pub fn truncate_display(&self, text: &str) -> String {
        truncate_graphemes(text, self.max_chars)
    }

    /// 展开态网格单格的显示截断：取 `max_chars` 与 `grid_cell_max_chars` 中更紧的那个
    /// （0 表示该项不限）。
    pub fn truncate_grid_cell(&self, text: &str) -> String {
        let limit = match (self.max_chars, self.grid_cell_max_chars) {
            (0, g) => g,
            (m, 0) => m,
            (m, g) => m.min(g),
        };
        truncate_graphemes(text, limit)
    }

    /// 展开态的生效可见行数（夹进 [`GRID_ROWS_RANGE`]）。
    pub fn grid_rows(&self) -> usize {
        self.grid_rows
            .clamp(*GRID_ROWS_RANGE.start(), *GRID_ROWS_RANGE.end())
    }
}

/// 按字素簇截断到 `limit` 个（0 = 不截），超出时追加 [`TRUNCATION_MARK`]。
fn truncate_graphemes(text: &str, limit: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    match text.grapheme_indices(true).nth(limit) {
        Some((cut, _)) if limit > 0 => format!("{}{TRUNCATION_MARK}", &text[..cut]),
        _ => text.to_string(),
    }
}

/// 显示截断追加的标记。候选窗（[`UiCandidateConfig::truncate_display`]）与悬停提示共用：
/// 悬停提示的逐字段要认出并跳过它（它不是候选的字），两处写成各自的字面量迟早漂移。
pub const TRUNCATION_MARK: char = '…';

/// 字体配置（[ui.font]）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiFontConfig {
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub render_mode: String,
    /// 主字体的回退链：`family` 里没有这个字时按本表顺序找下一个。最终链 = `[family] + fallback`。
    ///
    /// 刻意不把 `family` 改成数组：改类型要配 Value 迁移，漏了老用户会整段回落出厂值；
    /// 分成两个键零迁移，设置页字体控件也不用动。
    ///
    /// 不得加 `skip_serializing_if`：本仓用它表达「这个键退出配置体系」，加上后
    /// `registry_covers_every_config_key` 会静默放行，该键在 CLI、片段导入、设置页里都够不着。
    #[serde(default)]
    pub fallback: Vec<String>,
    /// 按脚本指派字体：键是脚本类名（`latin`/`greek`/`cyrillic`/`cjk`/`emoji`/`digits`/`punct`），
    /// 值是该类自己的字体链。
    ///
    /// 与 [`Self::fallback`] 缺一不可：回退链只在缺字时触发，字体自带 ASCII 时换不掉；指派是无条件的。
    /// 未列出的类是「不单独切段」，与其余文字同属默认链，省掉多余的 `SetFontFamilyName`。
    /// 用 `BTreeMap` 保证写盘键序稳定。注册表里登记为 `Map`：本身是叶子，`latin` 等是数据而非
    /// 配置项，且 patch 逐条合并。同样不得加 `skip_serializing_if`，理由见 [`Self::fallback`]。
    #[serde(default)]
    pub scripts: BTreeMap<String, Vec<String>>,
    /// 候选字体的字重（100–950，OpenType 口径；0 = 不指定，按常规 / 跟随主题）。
    ///
    /// 独立维度而非写进 `family`：DirectWrite 的 family 名不含字重，「思源宋体 SemiBold」这类
    /// GDI 名字查不到。存量的这种旧名由渲染端兼容，本键为 0 时名字里的字重生效；主题节点显式写的
    /// `font_weight` 仍胜过本键。不得加 `skip_serializing_if`，理由见 [`Self::fallback`]。
    #[serde(default)]
    pub weight: i32,
}

/// 主题配置（[ui.theme]）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiThemeConfig {
    // 字段级缺省保持空：加载用户配置缺字段时回退 theme.txt（旧版迁移），不被强制成 default。
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub style: String,
    /// 播放主题动画（皮肤装饰的动图与事件切换）。关掉时装饰停在初始状态首帧；系统关了动画
    /// 效果时同样不播。与上面两项不同，字段级缺省就是 true（没有旧版迁移要兜）。
    #[serde(default = "default_true")]
    pub animation: bool,
}

// 手写 Default：仅供 Config::default()（getDefaults/恢复本页）给出有效初值，
// 与字段级 serde 缺省（空）解耦，避免影响加载期 theme.txt 迁移回退。
impl Default for UiThemeConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            style: "system".to_string(),
            animation: true,
        }
    }
}

/// 悬停提示配置（[ui.tooltip]）：延迟 + 有序段列表。
///
/// 段列表取代了旧的六个开关（`code_enabled` / `pinyin_enabled` / `pinyin_heteronyms` /
/// `pinyin_max_readings` / `chaizi_enabled` / `debug_enabled`），旧键由
/// [`Config::migrate_tooltip_sections_value`] 迁移。设计见 `docs/design/candidate-tooltip-sections.md`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TooltipConfig {
    /// 悬停提示总开关（默认开）。关掉后不再逐候选渲染气泡，段列表也不再计入按需加载的
    /// 需求（悬停[编码]用的反查索引、用户层编码索引，拆字段用的拆字表随之可卸）。
    /// 段各自的 `enabled` 在它之下（设计 `docs/design/memory-footprint.md` §4.3）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 提示延迟显示时间（毫秒）。
    #[serde(default = "default_tooltip_delay")]
    pub delay: i32,
    /// 单行显示上限（按字素簇计，emoji 组合序列算一个），超出截断加 `…`；0 = 不限。只管显示，复制 / 上屏取原文。
    ///
    /// 气泡里最长的是「完整原文」段——长短语可达上千字，不截会把气泡撑到屏幕外。
    #[serde(default = "default_tooltip_max_chars")]
    pub max_chars: usize,
    /// 折行宽度（显示列，CJK / 全角 / emoji 计 2、其余计 1）；0 = 不折。模板字面量里写了 `\t` 的
    /// 分列段，其含 `\t` 的行不折；由变量值带进来的 `\t` 照常折。
    ///
    /// 折行在协调器里硬插换行，而不是交给渲染层：View 引擎不支持文本折行，自绘、宿主渲染、
    /// macOS 三端因此都不用改就一致。
    #[serde(default = "default_tooltip_wrap_width")]
    pub wrap_width: usize,
    /// 气泡的段，按数组顺序自上而下排列。整体是一个不透明叶子（REGISTRY `StructList`），
    /// 用户层写了就整表替换出厂列表。
    #[serde(default = "default_tooltip_sections")]
    pub sections: Vec<TooltipSection>,
}

/// 气泡的一段：段名 + 段内容，都是注释模板（语法与变量同 `ui.comment.templates`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TooltipSection {
    /// 段开关。保留「内置段关着」这种状态：设置页一键就能打开，不必删了再加。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 段名，本身也是模板（`编码{(${code_source})}`）。求值后为空 = 无标题行。
    ///
    /// 与段内容不同，段名里的字面文字不随变量全空而消失：`编码{(${code_source})}`
    /// 在直接输入时就是 `编码`，不是空标题。
    #[serde(default)]
    pub label: String,
    /// 段内容模板。结果按 `\n` 拆行、空行丢弃；全空则整段不显示。
    #[serde(default)]
    pub template: String,
    /// 求值粒度：`""` 整个候选一次；`"han"` 显示文本里每个 ≥U+3400 的字一次；
    /// `"char"` 每个非空白字符一次。逐字时每字一行，`${char}` 不计入「这行有值」。
    /// 未知取值按 `""` 处理并告警（在协调器编译段列表处）。
    #[serde(default)]
    pub each: String,
    /// 仅逐字段：按该变量（逐字上下文求值）是否非空把行稳定分成两组，非空组在前。
    /// 空 = 不分组，保持原文顺序。
    #[serde(default)]
    pub promote: String,
    /// 仅当内容恰为一行时生效：渲染成 `标签: 内容`，而非 `[标签]` 独占一行。
    #[serde(default)]
    pub inline: bool,
}

impl TooltipSection {
    fn new(label: &str, each: &str, template: &str) -> Self {
        Self {
            enabled: true,
            label: label.to_string(),
            template: template.to_string(),
            each: each.to_string(),
            promote: String::new(),
            inline: false,
        }
    }

    fn disabled(self) -> Self {
        Self {
            enabled: false,
            ..self
        }
    }
}

fn default_tooltip_max_chars() -> usize {
    200
}

fn default_tooltip_wrap_width() -> usize {
    40
}

/// 出厂段名：迁移按段名找段（见 [`Config::migrate_tooltip_sections_value`]），
/// 出厂列表与迁移共用这组常量，改名只改一处。
const TIP_CODE: &str = "编码{(${code_source})}";
const TIP_PINYIN: &str = "拼音";
const TIP_CHAIZI: &str = "拆字";
const TIP_DEBUG: &str = "调试";

/// 出厂段列表。与 `data/config.toml` 的 `[[ui.tooltip.sections]]` 逐项相同
/// （守门测试 `tooltip_sections_l1_matches_l2`）。
///
/// 「完整原文」置首：只在候选显示被截断时有值，出现时正是用户最想看的。其后复现旧出厂外观
/// （`[编码]`、`[拼音]`），拆字、调试、Unicode 关着。「释义」段出厂开：查不到时整段不显示，
/// 对没用注释库的用户零外观变化。
pub fn default_tooltip_sections() -> Vec<TooltipSection> {
    vec![
        TooltipSection::new("完整原文", "", "${full_text}"),
        TooltipSection::new(TIP_CODE, "", "${word_code}"),
        TooltipSection::new(TIP_PINYIN, "han", "${char}：${readings}"),
        TooltipSection::new(TIP_CHAIZI, "han", "${char}：${chaizi}{ [${chaizi_code}]}").disabled(),
        TooltipSection::new("释义", "", "${dict_all:\n}"),
        TooltipSection::new("Unicode", "char", "${char}：${unicode}").disabled(),
        TooltipSection::new(TIP_DEBUG, "", "${debug}").disabled(),
    ]
}

/// 「释义」段加入之前的出厂段列表，冻结，只供
/// [`Config::migrate_stale_factory_tooltip_sections_value`] 认出存量副本。出厂再变时不要改它。
fn tooltip_sections_before_dict_all() -> Vec<TooltipSection> {
    vec![
        TooltipSection::new("完整原文", "", "${full_text}"),
        TooltipSection::new("编码{(${code_source})}", "", "${word_code}"),
        TooltipSection::new("拼音", "han", "${char}：${readings}"),
        TooltipSection::new("拆字", "han", "${char}：${chaizi}{ [${chaizi_code}]}").disabled(),
        TooltipSection::new("Unicode", "char", "${char}：${unicode}").disabled(),
        TooltipSection::new("调试", "", "${debug}").disabled(),
    ]
}

/// 旧版 `[ui.tooltip]` 的六个开关（缺省值即旧出厂值）。只供迁移使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyTooltipFlags {
    pub code: bool,
    pub pinyin: bool,
    pub heteronyms: bool,
    pub max_readings: usize,
    pub chaizi: bool,
    pub debug: bool,
}

impl Default for LegacyTooltipFlags {
    fn default() -> Self {
        Self {
            code: true,
            pinyin: true,
            heteronyms: true,
            max_readings: 0,
            chaizi: false,
            debug: false,
        }
    }
}

/// 旧开关组合 → 与之外观一致的段列表（设计 §8.1）。以出厂列表为底改写，老用户也拿到
/// 「完整原文」段（开）与 Unicode 段（关）。
///
/// 独立成 pub 函数：协调器的对拍测试要用同一份映射，在测试里另写一份就只证明了两份手写映射一致。
pub fn tooltip_sections_from_legacy(f: LegacyTooltipFlags) -> Vec<TooltipSection> {
    // 读音数：旧实现 heteronyms=false 时只取首音，先于 max_readings 判定。
    let readings = if !f.heteronyms {
        "${readings:1}".to_string()
    } else if f.max_readings > 0 {
        format!("${{readings:{}}}", f.max_readings)
    } else {
        "${readings}".to_string()
    };
    let mut out = default_tooltip_sections();
    for s in &mut out {
        match s.label.as_str() {
            TIP_CODE => s.enabled = f.code,
            TIP_PINYIN => {
                s.enabled = f.pinyin;
                s.template = format!("${{char}}：{readings}");
            }
            TIP_CHAIZI => s.enabled = f.chaizi,
            TIP_DEBUG => s.enabled = f.debug,
            _ => {}
        }
    }
    if f.chaizi && f.pinyin {
        // 两段合成一段（旧 `merge_chaizi_pinyin`）：拆字部分包在可选段里——无拆字的字整段
        // 消失得 `好：hǎo`；无读音时末尾 `\t` 被空变量吞掉得 `好：女子 [vbg]`。
        // `promote = "chaizi"` 复现旧行序：有拆字的字按原文序在前，拆字库未收录的补在末尾。
        out.retain(|s| s.label != TIP_CHAIZI);
        if let Some(p) = out.iter_mut().find(|s| s.label == TIP_PINYIN) {
            *p = TooltipSection {
                enabled: true,
                label: "拆字 / 拼音".to_string(),
                template: format!("${{char}}：{{${{chaizi}}{{ [${{chaizi_code}}]}}\t}}{readings}"),
                each: "han".to_string(),
                promote: "chaizi".to_string(),
                inline: false,
            };
        }
    }
    out
}

fn default_tooltip_delay() -> i32 {
    200
}

impl Default for TooltipConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            delay: default_tooltip_delay(),
            max_chars: default_tooltip_max_chars(),
            wrap_width: default_tooltip_wrap_width(),
            sections: default_tooltip_sections(),
        }
    }
}

// ───────────────────────── input.phrase（短语前缀列举）─────────────────────────

/// 短语前缀列举配置（[input.phrase]，对齐 Go）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhraseConfig {
    /// 命令短语的前缀列举开关：敲前缀（如 `zz`、`co`）时列出以它开头的短语。关闭后只有编码
    /// 完全相同的短语才出。默认开。
    ///
    /// 码表「精确匹配」（`single_code_input`）只管纯码表方案；本开关对所有方案生效。
    /// 精确匹配的「空码补全」兜底另有开关，不受本项影响。
    #[serde(default = "default_true")]
    pub prefix_match: bool,
    /// 触发前缀导航列举的最小输入长度（原 min_prefix_length）。默认 2。
    #[serde(default = "default_phrase_min_prefix")]
    pub min_prefix: usize,
}

impl Default for PhraseConfig {
    fn default() -> Self {
        Self {
            prefix_match: true,
            min_prefix: default_phrase_min_prefix(),
        }
    }
}

fn default_phrase_min_prefix() -> usize {
    2
}

// ───────────────────────── stats（统计）─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub track_english: bool,
    /// 打字速度的修正系数（只作用于速度展示，不影响字数统计）。
    ///
    /// 采集期已修掉结构性偏差（见 `wind_store::stats` 的「速度模型」），剩下的修不掉：输入法不知道
    /// 用户是否打错，退格重打的字计两遍、耗时只算一遍，恒为正偏差，故出厂取 < 1 做经验折价。
    /// 填 ≥ 1 只会让显示值偏离真实手速。暂不进设置页。
    #[serde(default = "default_speed_factor")]
    pub speed_factor: f32,
}

fn default_speed_factor() -> f32 {
    0.85
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            track_english: true,
            speed_factor: default_speed_factor(),
        }
    }
}

// ───────────────────────── extensions（扩展程序）─────────────────────────

/// 扩展程序（设计见 `docs/design/extension-hook-channel.md`）。扩展装在
/// `<userdata>/extensions/<id>/`；这里只记总开关与每个扩展的启用状态、授予的档位。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtensionsConfig {
    /// 总开关。关闭时核心不拉起扩展宿主，也不产出任何事件。
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub list: Vec<ExtensionEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionEntry {
    /// 扩展目录名。
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 授予的档位（`key.class` / `key.detail` / `mode` / `composition` / `candidates` /
    /// `commit`）。扩展清单里申请的只是申请，以这里为准。
    #[serde(default)]
    pub granted: Vec<String>,
    /// 扩展选项里用户改过的值（声明在扩展清单里，宿主按声明校验）。没存的取清单默认值。
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub options: serde_json::Map<String, serde_json::Value>,
}

// ───────────────────────── debug ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugConfig {
    /// 日志级别。空字符串等同 `info`（生产默认）；`info` 不得包含用户输入、词条等隐私数据。
    ///
    /// `off` = 整个日志功能关闭：主日志文件不创建，`startup_stage.log` 也不写，空文件同样是痕迹。
    /// 判据唯一实现在 `startup_trace::effective_log_level`，不要在别处重算优先级链。
    #[serde(default)]
    pub log_level: String,
    /// 单个日志文件的大小上限（MB），超出后滚动。默认 10。
    #[serde(default = "default_log_max_size_mb")]
    pub log_max_size_mb: u64,
    /// 保留的旧日志文件数量上限（不含主文件）。默认 10。
    ///
    /// 服务每次启动都会滚动一次，故该值约等于「能回溯最近几次运行」。
    #[serde(default = "default_log_max_files")]
    pub log_max_files: usize,
}

impl Default for DebugConfig {
    fn default() -> Self {
        Self {
            log_level: String::new(),
            log_max_size_mb: default_log_max_size_mb(),
            log_max_files: default_log_max_files(),
        }
    }
}

fn default_log_max_size_mb() -> u64 {
    10
}

fn default_log_max_files() -> usize {
    10
}

// ──────────────── system（与操作系统集成的方式，非输入行为）────────────────

/// 系统集成配置（`[system]`）：怎样向 Windows 登记自己，而不是输入行为。
/// 改这里的键会改变系统注册结果，需要管理员权限，且对已启动的宿主进程不生效。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfig {
    /// Dota 2 兼容：把本输入法在系统里登记的名称改为 [`dota2_compat_name`] 指定的那个。
    ///
    /// [`dota2_compat_name`]: SystemConfig::dota2_compat_name
    ///
    /// Dota 2 的 `imemanager.dll` 按 TSF Profile Description 全等比对一张硬编码白名单，未命中的
    /// 输入法取不到候选，还会多出系统默认 IME 小窗。见 `docs/design/game-compat-tsf-uielement.md` §1.1。
    /// 代价是语言栏和 Windows 设置里显示的名字跟着变，所以出厂关闭，由知情的用户自己决定。
    #[serde(default)]
    pub dota2_compat: bool,

    /// 开启 [`dota2_compat`] 后要登记的名称，出厂值见 [`DEFAULT_DOTA2_ALIAS`]。
    ///
    /// 开放成自由文本而非内置表：白名单条目各自的 `product_id` 行为不同（有的会吞上屏后的第一个
    /// 退格，见 `docs/design/game-compat-tsf-uielement.md` §1.3），无法逐条替用户验证取舍。
    ///
    /// 留空 = 回落到出厂值，不是关闭；关闭走 [`dota2_compat`]。写注册表那侧还会做清洗与长度校验，
    /// 见 `wind_coordinator::tsf_profile_name`。
    ///
    /// [`dota2_compat`]: SystemConfig::dota2_compat
    #[serde(default = "default_dota2_alias")]
    pub dota2_compat_name: String,
}

/// `schema.dict_cache_policy` 的出厂值。
fn default_dict_cache_policy() -> String {
    "lock".to_string()
}

/// `system.dota2_compat_name` 的出厂值，也是 Dota 2 白名单里我们选定的那条（第 90 条，
/// `product_id=0x410000`）。
///
/// 必须与游戏内表逐字一致：比对是全等的，差一个字符就静默不命中，由 `dota2_alias_tests`
/// 钉住提取自游戏二进制的字节。选它是因为不含空格、括号、连字符（最常见的静默失效来源），
/// 且是通用类别名而非产品品牌名；选取依据见 `docs/design/game-compat-tsf-uielement.md` §1.3。
///
/// 定义在本 crate：serde 的 `default` 与常量必须是同一份。
pub const DEFAULT_DOTA2_ALIAS: &str = "拼音输入法";

fn default_dota2_alias() -> String {
    DEFAULT_DOTA2_ALIAS.to_string()
}

/// `system.dota2_compat_name` 的长度上限（UTF-16 码元数，不含结尾 NUL）。
///
/// 硬上限来自 `wind_tsf/src/Register.cpp` 的 `wchar_t[256]` 读缓冲：超过 255 码元时读不到，
/// 重装/升级会静默写回真名。取 127 留一半余量；放宽它必须先放宽 `Register.cpp` 的两个缓冲。
pub const DOTA2_ALIAS_MAX_LEN: usize = 127;

/// 清洗并校验 `system.dota2_compat_name`。放在本 crate：纯逻辑，且须与 [`DEFAULT_DOTA2_ALIAS`]
/// 同源；`wind_coordinator::tsf_profile_name` 重导出本函数。
///
/// 只做三件事，各对应一种静默失效：
/// - trim 首尾空白（游戏侧全等比对）；内部半角空格不动，白名单条目本就带。
/// - 拒绝看不见的字符：见 [`is_invisible_troublemaker`]。
/// - 长度上限：见 [`DOTA2_ALIAS_MAX_LEN`]。
///
/// 空串（或只有空白）不是错误，回落到 [`DEFAULT_DOTA2_ALIAS`]。
pub fn sanitize_dota2_alias(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(DEFAULT_DOTA2_ALIAS.to_string());
    }
    if let Some(bad) = t.chars().find(|c| is_invisible_troublemaker(*c)) {
        // 报出码位：这类字符在界面上和正常字符长得一模一样，不给码位用户无从下手。
        return Err(format!(
            "名称里有一个看不见的字符（U+{:04X}），它会让比对不命中。\
             常见来源是从网页或文档里复制——请手工重敲一遍，空格用半角空格",
            bad as u32
        ));
    }
    // 按 UTF-16 码元而非 char 计数：BMP 外的字符占两个码元，按 char 算会放过
    // 一个 Register.cpp 的固定缓冲装不下的名字。
    let len = t.encode_utf16().count();
    if len > DOTA2_ALIAS_MAX_LEN {
        return Err(format!(
            "名称过长（{len} 个字符，上限 {DOTA2_ALIAS_MAX_LEN}）"
        ));
    }
    Ok(t.to_string())
}

/// 这个字符会不会让「全等比对」静默落空。白名单条目只含普通可见字符与半角空格，
/// 而从网页或文档复制常夹带看不出来的字符：
///
/// | 字符 | 来源 | 不拦会怎样 |
/// |---|---|---|
/// | U+00A0 不断行空格 | 网页排版 | 看着是空格，比对不命中 |
/// | U+3000 全角空格 | 中文输入法 | 同上，肉眼难辨 |
/// | U+200B 零宽空格 / U+FEFF | 复制粘贴、BOM | 完全不可见，`trim()` 也不剪 |
/// | U+202A..U+202E 双向覆盖 | 混排文本 | 不可见，还会搅乱显示顺序 |
/// | Cc 控制字符 | 粘贴多行 | 写进 `REG_SZ` 后语言栏显示乱码 |
///
/// 别把这些「顺手修好」（NBSP→半角空格之类）：猜错就是替用户写了个照样不命中的名字，
/// 报错让他重敲代价更小。
fn is_invisible_troublemaker(c: char) -> bool {
    // 半角空格是唯一允许的空白：白名单里的条目就是用它分隔的。
    c.is_control()
        || (c.is_whitespace() && c != ' ')
        || matches!(c,
            '\u{00AD}'                 // 软连字符
            | '\u{200B}'..='\u{200F}'  // 零宽空格/连字/断字 + 方向标记
            | '\u{202A}'..='\u{202E}'  // 双向嵌入与覆盖
            | '\u{2060}'..='\u{2064}'  // 词连接符等不可见格式符
            | '\u{FEFF}') // BOM / 零宽不断行空格
}

/// 「全局默认字符串」与「枚举 `#[default]`」是两处独立事实，必须一致：
/// per-app 没配时走全局串，直接 `NewlineStyle::default()` 的路径走枚举默认，不一致就两条路行为不同。
#[cfg(test)]
mod global_default_guards {
    use super::*;
    use crate::app_compat::{FirstShowMode, NewlineStyle};

    #[test]
    fn commit_newline_global_default_matches_enum_default() {
        assert_eq!(
            NewlineStyle::from_config(&default_commit_newline()),
            Some(NewlineStyle::default()),
            "input.commit_newline 的出厂串与 NewlineStyle 的 #[default] 必须是同一档"
        );
    }

    /// 同一条不变式，`first_show_mode` 那一对。
    #[test]
    fn global_first_show_mode_matches_enum_default() {
        assert_eq!(
            FirstShowMode::from_config(&default_first_show_mode()),
            Some(FirstShowMode::default()),
            "ui.candidate.first_show_mode 的出厂串与 FirstShowMode 的 #[default] 必须是同一档"
        );
    }
}

#[cfg(test)]
#[path = "config_bound_action_chinese_only_tests.rs"]
mod bound_action_chinese_only_tests;

#[cfg(test)]
#[path = "config_comment_state_verb_tests.rs"]
mod comment_state_verb_tests;

#[cfg(test)]
#[path = "config_dota2_alias_tests.rs"]
mod dota2_alias_tests;

// 出厂值不全等于各字段类型的 Default（`dota2_compat_name` 非空），故手写。
// 加新键时别改回 derive：那会让本键静默变成空串，开了开关后系统里的名字被改成空，且没有任何报错。
impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            dota2_compat: false,
            dota2_compat_name: default_dota2_alias(),
        }
    }
}

// ───────────────────────── 共享 default 助手 ─────────────────────────

/// 整句 N-best 两个数量的出厂值：1 = 单条整句（N-best 之前的行为）。
/// 须与引擎 `pinyin::Config::default()` 同值。
fn default_sentence_count() -> u8 {
    1
}

fn default_codetable_sentence_grammar_weight() -> f64 {
    0.5
}

fn default_true() -> bool {
    true
}

fn default_alt_commit() -> String {
    "off".to_string()
}

fn default_numpad_behavior() -> String {
    "direct".to_string()
}

fn default_s2t_variant() -> String {
    "s2t".to_string()
}

fn default_toggle_toolbar() -> String {
    "ctrl+shift+\\".to_string()
}

fn default_open_settings() -> String {
    "ctrl+shift+]".to_string()
}

/// 出厂不绑——写 `"none"` 而不是空串，理由同 `default_toggle_t2s`：设置页的 hotkey
/// 控件在禁用态恒写 `"none"`，出厂值若是空串，用户什么都没动打开一次设置页就会被判
/// 「有未保存的更改」。
fn default_open_dictionary() -> String {
    "none".to_string()
}

fn default_smart_symbol_timeout_ms() -> i32 {
    500
}

fn default_smart_symbol_chars() -> String {
    "。，？！：；、～￥·……——".to_string()
}

/// 参与英文智能符号的源字符默认集：`smart_chars` 那批中文标点对应的 ASCII 键，去掉配对符
/// （配对符在英文模式下被吃走会让 DLL 的 Tab 跳出失效，见 `SymbolConfig::english_chars`）。
fn default_english_smart_chars() -> String {
    ".,?!:;".to_string()
}

fn default_filter_mode() -> String {
    "smart".to_string()
}

/// 含生僻字的词的出厂处置：整词放行。
///
/// 与 `wind_candidate::RarePhrasePolicy` 的 `#[default]` 及 `from_config` 未知值回落必须一致，
/// 由 `wind-coordinator/tests/rare_phrase_contract.rs` 的 `config_default_matches_policy_default`
/// 钉住（只有协调器同时依赖两个 crate）。
fn default_rare_phrase() -> String {
    "keep".to_string()
}

/// 上屏换行形式的全局默认档。`keep` = 原样透传：不知道宿主要什么时不动用户数据；
/// 富文本宿主（Word / WPS / RichEdit）由 compat.toml 的 `[[commit_newline]]` 出厂名单配 `cr`。
///
/// 与 `app_compat::NewlineStyle` 的 `#[default]` 必须一致，由
/// `commit_newline_global_default_matches_enum_default` 钉住。
fn default_commit_newline() -> String {
    "keep".to_string()
}

fn default_smart_punct_list() -> String {
    ".,:".to_string()
}

fn default_latin_smart_punct_list() -> String {
    "./_:@".to_string()
}

fn default_enter_behavior() -> String {
    "commit".to_string()
}

fn default_space_behavior() -> String {
    "commit".to_string()
}

/// 空码时按标点出厂即丢弃废码（与 `enter_behavior` / `space_on_empty_behavior` 的 `commit`
/// 刻意不同）。
///
/// 代价不对称：错判成 `commit` 会把废码插进正文、要退格好几次；错判成 `clear` 只是少上屏原码，
/// 回车仍能一键拿到原码，「获取原码」的能力没有被封掉。
fn default_punct_on_empty_behavior() -> String {
    "clear".to_string()
}

/// 出厂只放减号：它是唯一既是英文标识符高频字符、又被出厂配成翻页键的符号。
///
/// `_` `.` `/` `@` 出厂都没有会话身份，组码中按下走「顶码上屏 + 出符号」的既有行为，
/// 没有「按了没反应」的空白要填，改动它们得不偿失。需要的用户自己加，让位判据照样护着
/// 自配的键组与分隔符。
fn default_buffer_symbol_chars() -> String {
    "-".to_string()
}

fn default_pinyin_separator() -> String {
    "auto".to_string()
}

fn default_shift_behavior() -> String {
    "temp_english".to_string()
}

fn default_shift_when_composing() -> String {
    "buffer".to_string()
}

/// 用户配置目录的就绪探测结果。
///
/// 把「系统尚未就绪」与「用户确实没有配置」分开：两者若都静默跳过用户层，
/// [`Config::active_schema`] 会回退到预置方案，表现为设置好的方案重启后变回出厂。
#[derive(Debug)]
pub enum UserConfigProbe {
    /// 便携模式：路径来自 exe 同目录，不依赖 known folder，恒就绪。
    Portable(PathBuf),
    /// 用户自定义数据目录（安装向导选定，见 `variant::custom_userdata_dir`）：
    /// 是本机固定盘上的普通目录，不经漫游 known folder，故与便携同属恒就绪一类。
    CustomDir(PathBuf),
    /// `dirs::config_dir()` 解析失败——漫游 known folder 尚不可用。
    RoamingUnavailable,
    /// 漫游根解析出来了但尚不存在（用户配置文件未挂载完成）。
    RoamingMissing(PathBuf),
    /// 漫游根已就绪、但本用户的 `config.toml` 此刻还看不到，而本地标记表明它本该存在。
    /// 这是开机早期漫游 profile 尚未挂载完的竞态，必须继续等，别把「没看到」当成「没有」而退回系统预置方案。
    ConfigPending { dir: PathBuf },
    /// 漫游根已就绪。此时 `dir_exists`/`file_exists` 是确定性事实，
    /// 再等下去也不会变，故不属于需要重试的状态。
    Ready {
        dir: PathBuf,
        dir_exists: bool,
        file_exists: bool,
    },
}

impl UserConfigProbe {
    /// 是否已到达「再等也不会变」的状态。`ConfigPending` 刻意排除：它正是
    /// 「本该有、暂时没看到」的可变态，要继续轮询等漫游挂载。
    pub fn is_settled(&self) -> bool {
        matches!(
            self,
            Self::Portable(_) | Self::CustomDir(_) | Self::Ready { .. }
        )
    }
}

/// 定制版数据层的目录名，与 `data/` 同级。
pub const CUSTOM_DATA_DIR_NAME: &str = "data_custom";

/// 定制版清单文件名。它在场与否就是「本机是不是定制版」的唯一判据。
pub const CUSTOM_MANIFEST_NAME: &str = "custom.toml";

/// `data_custom/custom.toml` 的内容。
///
/// 清单只负责减法（`hide`）与身份；加法与整表替换直接把文件放进 `data_custom/` 对应位置。
///
/// 刻意不加 `deny_unknown_fields`：清单是第三方手写、随定制包分发的数据，未来新增段时旧程序
/// 必须能忽略而不是整层退场。代价是 `[schema]`（少写 s）之类拼写错误被静默忽略，
/// 由 `wind_input config check --custom` 负责报出。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomManifest {
    /// 定制版身份，见 [`CustomIdentity`]。
    #[serde(default)]
    pub custom: CustomIdentity,
    /// 方案减法清单。与 `[schema].hidden` 是正交的两个轴，不得合并：`hidden` 只是不列进
    /// 切换列表（仍可被 mix 引用）；本字段 = 这个方案在本定制版里不存在。
    #[serde(default)]
    pub schemas: CustomHideList,
    /// 主题减法清单。
    #[serde(default)]
    pub themes: CustomHideList,
    /// 设置界面减法清单：条目是选择器（`key:<配置键>`，可带尾部 `*`；`feature:<id>`），
    /// 由设置程序消费，core 只解析与校验。
    ///
    /// 语义与上面两段不同：这里只是界面上不显示，被隐藏的设置照常生效。要关掉一个功能，
    /// 定制者应在 `data_custom/config.toml` 里设好值再在这里隐藏，两件事不得合成一个开关。
    #[serde(default)]
    pub settings: CustomHideList,
}

/// 定制版身份。没有它，定制版用户报障时连他装的是不是定制版都判断不出来。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomIdentity {
    /// 稳定标识，日志 / 关于页 / 报障用（如 `huma-edition`）。
    #[serde(default)]
    pub id: String,
    /// 展示名（如 `虎码定制版`）。
    #[serde(default)]
    pub name: String,
    /// 定制包自身的版本。
    #[serde(default)]
    pub version: String,
    /// 基于哪个主程序版本定制。目前只解析和保存，不做强制版本检查（兼容性判定留给校验 CLI
    /// 与启动告警），否则定制者每次主程序小版本更新都得改清单。
    #[serde(default)]
    pub base_version: String,
}

/// 一段减法清单。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomHideList {
    /// 在本定制版里「不存在」的条目 id。
    #[serde(default)]
    pub hide: Vec<String>,
}

/// 一个资源层：层名 + 该层的根目录。见 [`Config::resource_layers_named`]。
///
/// 层名是 `user` / `custom` / `data` 三个固定字面量。它同时是日志措辞
/// （`覆盖生效[custom][schema]`）与呈现层判据（主题 builtin = 非 user 层）的来源，两处须指同一件事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLayer {
    /// 层名：`user` / `custom` / `data`。
    pub name: &'static str,
    /// 该层的根目录（`%APPDATA%\WindInput` / `<安装根>\data_custom` / `<安装根>\data`）。
    pub path: PathBuf,
}

impl ResourceLayer {
    /// 构造一层。层名只应取 `user` / `custom` / `data`。
    pub fn new(name: &'static str, path: PathBuf) -> Self {
        Self { name, path }
    }

    /// 层内子目录（`schemas` / `themes` / `opencc`），层名不变。
    /// 写成 `layers.map(|l| l.sub("themes"))`，层名就不会在 map 里丢掉。
    pub fn sub(&self, sub: &str) -> Self {
        Self {
            name: self.name,
            path: self.path.join(sub),
        }
    }

    /// 是否用户层（`%APPDATA%`）。其余层都是「随安装包分发的」，对用户不可写、不可删。
    pub fn is_user(&self) -> bool {
        self.name == "user"
    }
}

/// 四层配置的一次性快照：各层原始内容 + 一次真实加载的结果。
///
/// 把「采样」与「取键」分开，同一次采样查多个键时答案来自同一个世界。快照是采样那一刻的
/// 盘上状态，用完即弃。
///
/// 不要拿它遍历整张注册表给设置页做「每项的来源」：这是排查用的重查询；「此项可被方案覆盖」
/// 是静态事实，走 capability 快照。
pub struct OriginSnapshot {
    /// 各层：层名、配置文件路径、该层整份未合并的 TOML。从低到高。
    layers: Vec<(&'static str, Option<PathBuf>, Option<toml::Value>)>,
    /// 一次真实 `load()` 结果的序列化形态（含 `normalize` 之后）。
    effective: toml::Value,
    /// 那次加载的降级记录。
    degradation: ConfigDegradation,
}

impl OriginSnapshot {
    /// 采样四层。`data_dir` 同 [`Config::load`]。
    pub fn capture(data_dir: Option<&Path>) -> anyhow::Result<Self> {
        // L1 序列化不了属于代码 bug，与 `load()` 里同样处置——往上抛。
        let default_v = toml::Value::try_from(Config::default())?;

        // 层不存在（非定制版的 custom、漫游目录取不到的 user）与「有这层但文件缺失」
        // 要分得开：前者 path 为 None，后者 path 有值而内容为 None。呈现端据此说得出
        // 「本机不是定制版」还是「这一层没写这个键」。
        let file_layer = |name: &'static str, dir: Option<PathBuf>| {
            let Some(dir) = dir else {
                return (name, None, None);
            };
            let path = dir.join("config.toml");
            let value = Config::read_toml_value(&path);
            (name, Some(path), value)
        };

        let layers = vec![
            ("default", None, Some(default_v)),
            file_layer("data", data_dir.map(|d| d.to_path_buf())),
            file_layer("custom", Config::custom_data_dir()),
            file_layer("user", Config::user_config_dir()),
        ];

        let cfg = Config::load(data_dir)?;
        Ok(Self {
            layers,
            effective: toml::Value::try_from(&cfg)?,
            degradation: cfg.degradation,
        })
    }

    /// 追溯一个键。语义见 [`KeyOrigin`] 与 [`Config::key_origin`]。
    pub fn key(&self, key: &str) -> KeyOrigin {
        let layers: Vec<LayerOrigin> = self
            .layers
            .iter()
            .map(|(name, path, whole)| LayerOrigin {
                layer: name,
                path: path.clone(),
                value: whole.as_ref().and_then(|v| value_at_path(v, key)),
            })
            .collect();

        let effective = value_at_path(&self.effective, key);

        // 归属判定：从高到低找第一个声明了的层，其值与生效值相同才算「就是它」。
        // 值相等这步不能省：表类型深合并时最高声明层只是并集的一部分，`normalize()` 也会改写值；
        // 宁可报「指不到单独一层」，也不报一个会把人带偏的层名。
        let effective_layer = layers
            .iter()
            .rev()
            .find(|l| l.value.is_some())
            .filter(|l| l.value == effective)
            .map(|l| l.layer);

        KeyOrigin {
            layers,
            effective,
            effective_layer,
            degraded: self.degradation.taints(key),
            syntax_error: self.degradation.unparsable.first().map(|u| u.describe()),
        }
    }
}

/// 按点分路径在 TOML 值里取一个子值。中途遇到非表即视为不存在——
/// 「`ui` 被用户写成了字符串」这类情况下，`ui.font.size` 确实不存在。
fn value_at_path(v: &toml::Value, key: &str) -> Option<toml::Value> {
    let mut cur = v;
    for part in key.split('.') {
        cur = cur.as_table()?.get(part)?;
    }
    Some(cur.clone())
}

/// 一个配置键在某一层的声明情况。见 [`Config::key_origin`]。
#[derive(Debug, Clone, PartialEq)]
pub struct LayerOrigin {
    /// 层名：`default` / `data` / `custom` / `user`。后三个与 [`ResourceLayer::name`] 同名同义；
    /// `default` 是配置层独有的（代码里的默认值）。
    pub layer: &'static str,
    /// 该层配置文件的路径。`default` 层恒为 `None`；层不存在（非定制版的 `custom`、
    /// 漫游目录取不到的 `user`）时也是 `None`，与 `value: None` 一起表示「这一层不在」。
    pub path: Option<PathBuf>,
    /// 该层显式声明的值；`None` = 这一层没写这个键（或该层不存在）。
    ///
    /// 不是「该层生效后的值」：层与层之间是深合并，单层的声明可能只是最终值的一部分。
    pub value: Option<toml::Value>,
}

/// 一个配置键的来源追溯：每层各声明了什么、最终生效的是哪个。见 [`Config::key_origin`]。
#[derive(Debug, Clone, PartialEq)]
pub struct KeyOrigin {
    /// 各层声明，从低到高排列（`default` → `data` → `custom` → `user`），恒含四个元素
    /// （层不存在也占位）。与 [`Config::resource_layers_named`] 顺序相反是有意的：这里给人看，
    /// 覆盖关系从下往上叠更直观。
    pub layers: Vec<LayerOrigin>,
    /// 实际生效值，取自一次真实的 [`Config::load`]（含 `normalize` 归一化之后）。
    /// 不从 `layers` 推算：那会漏掉 `normalize()` 的改写与段级降级的回落。
    pub effective: Option<toml::Value>,
    /// 生效值来自哪一层。`None` 表示指不到单独一层，三种成因：
    /// 表类型跨层深合并（生效值是多层的并集）、`normalize()` 改写过、或该键压根不存在。
    pub effective_layer: Option<&'static str>,
    /// 本次加载中，该键是否被降级殃及（[`ConfigDegradation::taints`]）。为真时用户层的值
    /// 没有进入生效值：所在段回落了出厂默认，或所在文件语法不合法，见 [`Self::syntax_error`]。
    pub degraded: bool,
    /// `degraded` 为真且成因是文件语法不合法时，那条故障的一句话描述
    /// （[`UnparsableLayer::describe`]）；成因是段级降级时为 `None`。
    ///
    /// 呈现端须分开讲：段级降级要用户改那个键的类型，语法故障要他改那一行。
    /// 本字段非 `None` 时不要照搬 `degraded` 报「本次未生效」：语法故障让 `taints` 对任何路径
    /// 恒为真，这对写盘闸是安全的保守，对呈现却是错的（本键很可能正常生效）。应照常报
    /// `effective_layer`，把语法故障作为附加警示（见 `config describe`）。
    pub syntax_error: Option<String>,
}

/// 读取并解析定制版清单。见 [`Config::custom_manifest`]（含「解析失败 ⇒ 整层退场」的理由）。
fn load_custom_manifest() -> Option<CustomManifest> {
    let file = crate::variant::install_root()?
        .join(CUSTOM_DATA_DIR_NAME)
        .join(CUSTOM_MANIFEST_NAME);
    // 绝大多数装机走这一条：不是定制版，不打任何日志。
    if !file.is_file() {
        debug!("非定制版（无 {}）", file.display());
        return None;
    }
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            warn!(
                "定制版清单读取失败（{e}），data_custom 层整体不启用: {}",
                file.display()
            );
            return None;
        }
    };
    match toml::from_str::<CustomManifest>(&text) {
        Ok(m) => {
            // DEBUG 而非 INFO：面向用户的权威摘要是服务启动路径上的
            // `wind_rpc::custom_edition::startup_summary()`，这里再打 INFO 会像加载了两次。
            // 保留它是因为非 service 宿主（TSF DLL / 移动端 / CLI）走不到那条摘要，且首次解析的时间点本身是线索。
            debug!(
                "定制版 {} {}（基于 {}）",
                if m.custom.id.is_empty() {
                    "<未命名>"
                } else {
                    &m.custom.id
                },
                m.custom.version,
                m.custom.base_version
            );
            Some(m)
        }
        // WARN 而非 ERROR：程序照常工作，只是退回原版行为。措辞点明「整体不启用」，
        // 因为用户看到的现象是「我的定制版变回原版了」，日志必须能一眼对上。
        Err(e) => {
            warn!(
                "定制版清单解析失败（{e}），data_custom 层整体不启用（回落原版行为）: {}",
                file.display()
            );
            None
        }
    }
}

impl Config {
    /// 四层合并加载：默认值 → `data/config.toml` → `data_custom/config.toml` → 用户配置。
    ///
    /// 各层 `toml::Value` 深合并（表递归、标量/数组后者覆盖），最后一次性反序列化，新增字段无需改合并代码。
    /// 定制层只需写差异键，差异越小跨版本存活率越高。
    pub fn load(data_dir: Option<&Path>) -> anyhow::Result<Self> {
        // Layer 1: 代码默认值（序列化为 Value，保证所有字段存在）
        let mut merged = toml::Value::try_from(Self::default())?;
        // 各层的语法故障。收满后连同段级降级一起挂到 `Config::degradation` 上——
        // 写盘闸、RPC、设置页横幅、CLI 全部只认那一个出口，多一条通道就多一处会漂移的判据。
        let mut unparsable: Vec<UnparsableLayer> = Vec::new();

        // Layer 2: 系统预置配置 (data/config.toml)
        if let Some(data_dir) = data_dir {
            let sys_config = data_dir.join("config.toml");
            if let Some(v) = Self::read_toml_value_reporting(&sys_config, "data", &mut unparsable) {
                merge_value(&mut merged, v);
                info!("Loaded system config: {}", sys_config.display());
            }
        }

        // Layer 2.5: 定制版预置配置 (data_custom/config.toml)。
        // 位置固定在 L2 之后、L3 之前：定制者可以覆盖出厂值，但绝不能压过终端用户。
        if let Some(custom_dir) = Self::custom_data_dir() {
            let custom_config = custom_dir.join("config.toml");
            if let Some(mut v) =
                Self::read_toml_value_reporting(&custom_config, "custom", &mut unparsable)
            {
                Self::migrate_user_layer_value(&mut v);
                merge_value(&mut merged, v);
                info!("Loaded custom config: {}", custom_config.display());
            }
        }

        // Layer 3: 用户配置 (%APPDATA%/WindInput/config.toml)
        match Self::user_config_dir() {
            Some(user_dir) => {
                let user_config = user_dir.join("config.toml");
                if let Some(mut v) =
                    Self::read_toml_value_reporting(&user_config, "user", &mut unparsable)
                {
                    Self::migrate_user_layer_value(&mut v);
                    merge_value(&mut merged, v);
                    info!("Loaded user config: {}", user_config.display());
                }
            }
            // 漫游 known folder 解析失败：用户层被跳过，必须留下 warn，否则 schema.active 等设置失效
            // 却无任何痕迹。
            None => warn!("User config dir unavailable, user layer skipped"),
        }

        Self::migrate_enable_english_value(&mut merged);
        Self::migrate_force_vertical_value(&mut merged);
        Self::migrate_index_labels_value(&mut merged);
        Self::migrate_empty_code_behavior_value(&mut merged);
        // 层内迁移（如 `migrate_comment_max_chars_value`）不在这里，见 [`Self::migrate_user_layer_value`]：
        // 它们要问「这一层写过新键吗」，而合并结果里新键必然存在（L1 序列化而来）。
        // 这一族放在合并之后、`try_into` 之前，定制层的旧值也一并被迁移。
        // 段级降级不能顶替迁移：迁移是无损搬到新形态，降级是整段换成出厂值；已发布字段改类型
        // 仍必须写迁移，降级只是兜底。
        let mut config = Self::deserialize_with_section_fallback(merged);
        // 语法故障挂在段级降级之后：`deserialize_with_section_fallback` 会整个改写
        // `config.degradation`（unparsable 恒空），先挂会被覆盖。两者可同时发生。
        config.degradation.unparsable = unparsable;
        config.normalize();
        Ok(config)
    }

    /// 反序列化 `merged`，失败时只把有毒的那一段替换成 L1 默认，其余配置原样保留。
    ///
    /// 整份 `try_into` 是全有全无的，而调用方多为 `unwrap_or_default()`，一个类型错的键会让
    /// 方案/词库/按键/主题一起回落出厂值；段边界即功能边界，降级应只影响那一块。
    ///
    /// 定位用探针（把待测段贴到全默认骨架上试），不用「逐段替换直到成功」：后者无法区分多段
    /// 是否都有毒，会误伤。探两层：顶层段判坏后对其直接子键再探，能定位到 `ui.font` 就不降
    /// `ui` 整段；只递归这一层，再深收益递减且易把配置切成半新半旧。子键探不出则整段降级。
    ///
    /// 成功路径会深 clone `merged` 一次（几十微秒）：toml 只有按值消费的 `Deserializer`，
    /// 失败后拿不回原值做探针。相对 `load()` 的文件 IO 可忽略，不值得改结构。
    fn deserialize_with_section_fallback(merged: toml::Value) -> Config {
        let root_err = match merged.clone().try_into::<Config>() {
            Ok(config) => return config,
            Err(e) => e.to_string(),
        };

        let default_v = match toml::Value::try_from(Config::default()) {
            Ok(v) => v,
            // 默认配置自己序列化不了属于代码 bug，此处无从补救。
            Err(e) => {
                error!("Config default is not serializable ({e}); config falls back to defaults");
                return Config {
                    degradation: ConfigDegradation {
                        sections: Vec::new(),
                        total_fallback: true,
                        // 语法故障由 `load()` 在本函数返回后挂上；这里恒空。
                        unparsable: Vec::new(),
                    },
                    ..Config::default()
                };
            }
        };

        // 探毒与回落的实现共用 `section_fallback`（`schema_overrides/<id>.toml` 走同一套，
        // 见该模块头部）。这里只保留 `Config` 特有的处置：WARN 文案与 `degradation` 落点。
        let crate::section_fallback::Patched {
            value: patched,
            bad,
        } = crate::section_fallback::probe_and_patch::<Config>(&merged, &default_v);

        match patched.try_into::<Config>() {
            Ok(mut config) if !bad.is_empty() => {
                for (path, err) in &bad {
                    // WARN 而非 INFO：这是异常，不是正常降级。压成 INFO 会掩盖真实的
                    // 迁移缺失——本该写 `migrate_*_value` 的字段类型变更会在这里悄悄"自愈"。
                    warn!(
                        "配置段 [{path}] 解析失败，已回落出厂默认值（该段的用户设置本次不生效）；\
                         原始错误：{err}"
                    );
                }
                config.degradation = ConfigDegradation {
                    sections: bad.into_iter().map(|(path, _)| path).collect(),
                    total_fallback: false,
                    // 同上：`load()` 负责补语法故障，本函数只管段级降级。
                    unparsable: Vec::new(),
                };
                config
            }
            // 防御性分支：`bad` 为空时 `patched == merged`，而 `merged` 在函数开头已经失败过，
            // 走不到这里。留着是为了不把「探不出毒却又成功了」静默当成正常加载。
            Ok(config) => config,
            Err(e) => {
                error!(
                    "配置解析失败且无法定位到具体段，整份配置回落出厂默认值；\
                     原始错误：{root_err}；回落后仍失败：{e}"
                );
                Config {
                    degradation: ConfigDegradation {
                        sections: Vec::new(),
                        total_fallback: true,
                        // 语法故障由 `load()` 在本函数返回后挂上；这里恒空。
                        unparsable: Vec::new(),
                    },
                    ..Config::default()
                }
            }
        }
    }

    /// 存量迁移（须在反序列化前跑）：空码三键（`input.enter_behavior` /
    /// `space_on_empty_behavior` / `punct_on_empty_behavior`）的值域收窄为 `commit | clear`，
    /// 把设置端曾误列的 `ignore` / `commit_and_input` 归一到 `commit`。
    ///
    /// 那两个值从未实现（消费点只判 `== "clear"`），故零行为变更；不做的话设置页下拉匹配不到，
    /// 会静默弹回首项。
    fn migrate_empty_code_behavior_value(merged: &mut toml::Value) {
        const KEYS: [&str; 3] = [
            "enter_behavior",
            "space_on_empty_behavior",
            "punct_on_empty_behavior",
        ];
        let Some(input) = merged.get_mut("input").and_then(|i| i.as_table_mut()) else {
            return;
        };
        // 合法值域取自注册表，不另抄一份：三键的值域并不相同（标点多一个 `clear_no_input`），
        // 且日后还会加值，抄一份会让迁移把存量配置里的新值抹回 commit。
        let allowed = |key: &str| -> &'static [&'static str] {
            match crate::config_schema::field(&format!("input.{key}")).map(|f| &f.ty) {
                Some(crate::config_schema::FieldType::Enum(vals)) => vals,
                // 注册表里没登记或不是 Enum：宁可不迁移也不要按猜测的值域改写用户配置。
                _ => &["commit", "clear"],
            }
        };
        // 先收集再改写：`get` 的不可变借用不能与 `insert` 的可变借用同时活着。
        let illegal: Vec<(String, String)> = KEYS
            .iter()
            .filter_map(|k| {
                let v = input.get(*k)?.as_str()?;
                (!allowed(k).contains(&v)).then(|| ((*k).to_string(), v.to_string()))
            })
            .collect();
        for (key, old) in illegal {
            info!("Migrated input.{key} = {old:?} → \"commit\"（该值从未被实现，行为不变）");
            input.insert(key, toml::Value::String("commit".to_string()));
        }
    }

    /// 存量迁移（须在反序列化前跑，字段已从 [`QuickInputConfig`] 移除）：
    /// 废弃键 `schema.quick_input.enable_english = false` → 从内置 quick_mix 的 members 移除 `english`。
    ///
    /// 语义已合并到 members，关掉过英文候选的存量用户必须落成成员删除，否则升级后英文候选会冒回来。
    /// 只认 false（true 是默认值）。
    fn migrate_enable_english_value(merged: &mut toml::Value) {
        let disabled = merged
            .get("schema")
            .and_then(|s| s.get("quick_input"))
            .and_then(|q| q.get("enable_english"))
            .and_then(|v| v.as_bool())
            .is_some_and(|v| !v);
        if !disabled {
            return;
        }
        let Some(modes) = merged
            .get_mut("schema")
            .and_then(|s| s.get_mut("mix_modes"))
            .and_then(|m| m.as_array_mut())
        else {
            return;
        };
        for mode in modes.iter_mut() {
            if mode.get("id").and_then(|v| v.as_str()) != Some(QUICK_MIX_ID) {
                continue;
            }
            if let Some(members) = mode.get_mut("members").and_then(|m| m.as_array_mut()) {
                members.retain(|v| v.as_str() != Some("english"));
            }
        }
        info!("Migrated quick_input.enable_english=false into quick_mix members");
    }

    /// 存量迁移（须在反序列化前跑）：`ui.candidate.index_labels` 由「字符串，每 char 一槽」
    /// 改为「字符串数组，每项一槽」，旧值按 char 拆成数组。
    ///
    /// 不可省：类型不匹配会让该段被降级，配过此项的用户整段回落出厂值。按 `char` 而非字素簇拆
    /// 才是对旧值的无损还原。
    fn migrate_index_labels_value(merged: &mut toml::Value) {
        let Some(cand) = merged
            .get_mut("ui")
            .and_then(|u| u.get_mut("candidate"))
            .and_then(|c| c.as_table_mut())
        else {
            return;
        };
        // 只认字符串——已是数组说明是新格式（或本次默认值），不动。
        let Some(old) = cand.get("index_labels").and_then(|v| v.as_str()) else {
            return;
        };
        let split: Vec<toml::Value> = old
            .chars()
            .map(|c| toml::Value::String(c.to_string()))
            .collect();
        let n = split.len();
        cand.insert("index_labels".to_string(), toml::Value::Array(split));
        info!("Migrated ui.candidate.index_labels string → {n} 个槽位数组");
    }

    /// 存量迁移（层内、须在反序列化前跑）：`ui.candidate.comment_max_chars`（横竖共用）
    /// → `comment_max_chars_vertical` / `_horizontal` 各一份。
    ///
    /// 只在新键缺失时抄旧值（新键是更明确的意图）。抄完摘掉旧键，且写盘路径（`prune_user_config` /
    /// `set_user_value`）都先跑本函数：否则把某一侧改回 0 会删新键，下次 load 旧键又把它补回。
    fn migrate_comment_max_chars_value(merged: &mut toml::Value) {
        let Some(cand) = merged
            .get_mut("ui")
            .and_then(|u| u.get_mut("candidate"))
            .and_then(|c| c.as_table_mut())
        else {
            return;
        };
        let Some(old) = cand.remove("comment_max_chars") else {
            return;
        };
        let Some(old) = old.as_integer() else {
            return;
        };
        let mut copied = Vec::new();
        for k in ["comment_max_chars_vertical", "comment_max_chars_horizontal"] {
            if !cand.contains_key(k) {
                cand.insert(k.to_string(), toml::Value::Integer(old));
                copied.push(k);
            }
        }
        if !copied.is_empty() {
            info!("Migrated ui.candidate.comment_max_chars={old} → {copied:?}");
        }
    }

    /// 层内迁移：作用在单独一层的 `toml::Value` 上，在 `merge_value` 之前跑。
    ///
    /// 与 [`Self::load`] 里合并后那一族刻意分开：L1 由 `Config::default()` 序列化，凡仍在结构体里的
    /// 字段合并结果里都有，「新键不存在才写」的迁移放在那里永远不执行。只有在单层上问
    /// `contains_key`，答案才是「这一层的作者写过吗」；设置页写回只写它管的键，新旧两键会在
    /// 用户配置里长期并存，必须这样区分。
    ///
    /// custom 层（L2.5）也跑一遍，定制层里的旧值同样被迁移。
    fn migrate_user_layer_value(layer: &mut toml::Value) {
        Self::migrate_comment_max_chars_value(layer);
        Self::migrate_font_size_follow_theme_value(layer);
        Self::migrate_langbar_badge_colors_value(layer);
        Self::migrate_tooltip_sections_value(layer);
        Self::migrate_stale_factory_tooltip_sections_value(layer);
        Self::migrate_grammar_enabled_value(layer);
    }

    /// 存量迁移（层内）：本层手写了 `[schema.pinyin.grammar]` 的 `weight`（非 0）与 `model`（非空）、
    /// 却没写 `enabled` → 补 `enabled = true`。
    ///
    /// `enabled` 单层缺省为 true，但 L2 显式写了 `false`，分层合并后会把这类老配置的模型悄悄关掉。
    /// 已知边界：这些用户在设置页关掉开关时，`false` 等于出厂值会被剪枝，下次又被补回 true；
    /// 要关只能删掉 weight / model 两行。
    fn migrate_grammar_enabled_value(layer: &mut toml::Value) {
        let Some(g) = layer
            .get_mut("schema")
            .and_then(|s| s.get_mut("pinyin"))
            .and_then(|p| p.get_mut("grammar"))
            .and_then(toml::Value::as_table_mut)
        else {
            return;
        };
        if g.contains_key("enabled") {
            return;
        }
        let weight_set = g.get("weight").is_some_and(|v| {
            v.as_float()
                .or_else(|| v.as_integer().map(|i| i as f64))
                .is_some_and(|w| w != 0.0)
        });
        let model_set = g
            .get("model")
            .and_then(toml::Value::as_str)
            .is_some_and(|m| !m.trim().is_empty());
        if weight_set && model_set {
            g.insert("enabled".to_string(), toml::Value::Boolean(true));
            info!("schema.pinyin.grammar has weight+model but no enabled → enabled = true");
        }
    }

    /// 存量迁移（层内）：本层的 `ui.tooltip.sections` 与「释义」段加入前的旧出厂 6 段逐项相等时，
    /// 视为冗余剪掉，让这层跟随新出厂。
    ///
    /// 否则出厂多了一段后它就成了删不掉的「自定义」，用户永远拿不到新段。比较按反序列化后的段
    /// （serde 缺省补齐）；改过任何一处的不动。
    fn migrate_stale_factory_tooltip_sections_value(layer: &mut toml::Value) {
        let Some(tip) = layer
            .get_mut("ui")
            .and_then(|u| u.get_mut("tooltip"))
            .and_then(toml::Value::as_table_mut)
        else {
            return;
        };
        let stale = tip
            .get("sections")
            .and_then(|v| v.clone().try_into::<Vec<TooltipSection>>().ok())
            .is_some_and(|secs| secs == tooltip_sections_before_dict_all());
        if stale {
            tip.remove("sections");
            info!(
                "ui.tooltip.sections equals the pre-t282 factory list → dropped, following factory"
            );
        }
    }

    /// 存量迁移（层内、须在反序列化前跑）：`[ui.tooltip]` 的六个旧开关 → `sections`。
    ///
    /// 按这一层写了什么判（同 [`Self::migrate_user_layer_value`]）：
    /// - 本层没写任何旧键 → 不动；
    /// - 本层已写 `sections` → 以它为准，旧键只清不迁；
    /// - 旧键的值全等于旧出厂值 → 只清不迁；
    /// - 否则按 [`tooltip_sections_from_legacy`] 生成段列表写入本层，没写的旧键取旧出厂值。
    ///
    /// 旧键从本层移除。用户文件里的旧键由 [`Config::prune_user_config`] 先迁移落盘、再按
    /// [`RETIRED_KEYS`] 清除。已知近似：不看下层（定制层）写过什么，只在定制版与用户层都改过
    /// 这些开关时有差别；仓内没有写这些旧键的定制层，按近似接受。
    fn migrate_tooltip_sections_value(layer: &mut toml::Value) {
        const LEGACY_KEYS: [&str; 6] = [
            "code_enabled",
            "pinyin_enabled",
            "pinyin_heteronyms",
            "pinyin_max_readings",
            "chaizi_enabled",
            "debug_enabled",
        ];
        let Some(tip) = layer
            .get_mut("ui")
            .and_then(|u| u.get_mut("tooltip"))
            .and_then(toml::Value::as_table_mut)
        else {
            return;
        };
        let old: BTreeMap<&str, toml::Value> = LEGACY_KEYS
            .iter()
            .filter_map(|&k| tip.remove(k).map(|v| (k, v)))
            .collect();
        if old.is_empty() || tip.contains_key("sections") {
            return;
        }
        let d = LegacyTooltipFlags::default();
        let flag =
            |k: &str, default: bool| old.get(k).and_then(toml::Value::as_bool).unwrap_or(default);
        let flags = LegacyTooltipFlags {
            code: flag("code_enabled", d.code),
            pinyin: flag("pinyin_enabled", d.pinyin),
            heteronyms: flag("pinyin_heteronyms", d.heteronyms),
            max_readings: old
                .get("pinyin_max_readings")
                .and_then(toml::Value::as_integer)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(d.max_readings),
            chaizi: flag("chaizi_enabled", d.chaizi),
            debug: flag("debug_enabled", d.debug),
        };
        // 旧开关全是旧出厂值 = 用户从没改过气泡：只摘旧键、不写段列表，让本层跟随出厂（含 L2.5
        // 定制层）。写进去的话，set_user_value 只剪本次那一个键、会把整份出厂段列表原样落盘，
        // 下次出厂段列表一变它就成了删不掉的「自定义」。
        if flags == d {
            info!(
                "ui.tooltip legacy switches all at old defaults → dropped, following factory sections"
            );
            return;
        }
        match toml::Value::try_from(tooltip_sections_from_legacy(flags)) {
            Ok(v) => {
                tip.insert("sections".to_string(), v);
                info!("Migrated ui.tooltip legacy switches {flags:?} → sections");
            }
            // 段列表自己序列化不了属于代码 bug；旧键已摘，本层回落出厂段列表。
            Err(e) => error!("ui.tooltip 旧开关迁移失败（{e}），本层回落出厂段列表"),
        }
    }

    /// 存量迁移（层内）：`[[ui.langbar.badges]]` 的色值哨兵拆成「色值 + 不透明度」两键。
    ///
    /// 逐条、逐侧（light/dark）处理 `color_x`：
    /// - `auto`（不分大小写）→ `""`（与主字同色）；
    /// - `#RRGGBBAA`（`#` 可省）→ `#RRGGBB` + `alpha_x = AA/255`（`FF` 即 1.0，仍是挖空档）；
    /// - 其余（6 位、空串、非法值）不动，非法值留给渲染侧告警。
    ///
    /// 本层已显式写了 `alpha_x` 时不覆盖。幂等。
    fn migrate_langbar_badge_colors_value(layer: &mut toml::Value) {
        let Some(badges) = layer
            .get_mut("ui")
            .and_then(|u| u.get_mut("langbar"))
            .and_then(|l| l.get_mut("badges"))
            .and_then(toml::Value::as_array_mut)
        else {
            return;
        };
        for badge in badges.iter_mut().filter_map(toml::Value::as_table_mut) {
            for (color_key, alpha_key) in
                [("color_light", "alpha_light"), ("color_dark", "alpha_dark")]
            {
                let Some(raw) = badge.get(color_key).and_then(toml::Value::as_str) else {
                    continue;
                };
                let t = raw.trim();
                // `#` 可省：渲染侧 `parse_hex` 两种写法都认，手写配置里也都见过；
                // 只认带 `#` 的话，`2288E080` 就漏迁，落进新语义后被当非法色值。
                let hex = t.strip_prefix('#').unwrap_or(t);
                let (color, alpha) = if t.eq_ignore_ascii_case("auto") {
                    (String::new(), None)
                } else if hex.len() == 8 && hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                    let aa = u8::from_str_radix(&hex[6..], 16).expect("已校验为十六进制");
                    (format!("#{}", &hex[..6]), Some(f64::from(aa) / 255.0))
                } else {
                    continue;
                };
                info!(
                    "Migrated ui.langbar.badges.{color_key}={raw} → {color:?}, {alpha_key}={alpha:?}"
                );
                badge.insert(color_key.to_string(), toml::Value::String(color));
                if let Some(a) = alpha
                    && !badge.contains_key(alpha_key)
                {
                    badge.insert(alpha_key.to_string(), toml::Value::Float(a));
                }
            }
        }
    }

    /// 旧版 `ui.candidate.font_size_follow_theme` 的出厂值，也是缺该键时的生效值。
    const LEGACY_FONT_SIZE_FOLLOW_DEFAULT: bool = true;
    /// 旧版 `ui.candidate.font_size` 的出厂值。旧版写回会剪掉「等于出厂值」的键，
    /// 所以「关了跟随、字号保持 18」的用户文件里只剩 `follow = false`，字号要按它补回。
    const LEGACY_FONT_SIZE_DEFAULT: f64 = 18.0;

    /// 存量迁移（层内、须在反序列化前跑）：`ui.candidate.font_size_follow_theme`（bool）
    /// + `font_size` 两处表达 → 只剩 `font_size`（0 = 跟随主题，>0 = 用户指定）。
    ///
    /// 只在旧键 `font_size_follow_theme` 真的还在这一层时才动手：
    /// - `follow = true`  → `font_size = 0`；
    /// - `follow = false` → 保留本层 `font_size`；本层没写则补旧出厂值（见 [`Self::LEGACY_FONT_SIZE_DEFAULT`]）；
    /// - 旧键不存在 → 不动。
    ///
    /// 迁移后旧键从本层移除；用户文件里的旧键由 [`Config::prune_user_config`] 先迁移落盘、
    /// 再按 [`RETIRED_KEYS`] 清除。不要加「没写 follow 但写了 font_size 就清零」的分支：迁移每次
    /// 加载都重跑，新格式的自定义字号与那种旧文件字面上相同，会被反复清回 0。
    fn migrate_font_size_follow_theme_value(layer: &mut toml::Value) {
        let Some(cand) = layer
            .get_mut("ui")
            .and_then(|u| u.get_mut("candidate"))
            .and_then(toml::Value::as_table_mut)
        else {
            return;
        };
        let Some(v) = cand.remove("font_size_follow_theme") else {
            return;
        };
        let follow = v.as_bool().unwrap_or(Self::LEGACY_FONT_SIZE_FOLLOW_DEFAULT);
        let size = if follow {
            toml::Value::Float(0.0)
        } else {
            match cand.get("font_size") {
                Some(v) => v.clone(),
                None => toml::Value::Float(Self::LEGACY_FONT_SIZE_DEFAULT),
            }
        };
        info!("Migrated ui.candidate.font_size_follow_theme={follow} → font_size={size}");
        cand.insert("font_size".to_string(), size);
    }

    /// 存量迁移（须在反序列化前跑，字段已从 [`QuickInputConfig`] 移除）：
    /// 废弃键 `schema.quick_input.force_vertical` → 内置 quick_mix 的 [`MixModeConfig::candidate_layout`]。
    ///
    /// 映射刻意不对称：`true` → `"vertical"`；`false` → `"follow"` 而非 `"horizontal"`，
    /// 旧布尔的 false 是「不强制」。键不存在则不动，出厂值（Vertical）与老预置的 `true` 同义。
    fn migrate_force_vertical_value(merged: &mut toml::Value) {
        let Some(forced) = merged
            .get("schema")
            .and_then(|s| s.get("quick_input"))
            .and_then(|q| q.get("force_vertical"))
            .and_then(|v| v.as_bool())
        else {
            return;
        };
        let layout = if forced { "vertical" } else { "follow" };
        let Some(modes) = merged
            .get_mut("schema")
            .and_then(|s| s.get_mut("mix_modes"))
            .and_then(|m| m.as_array_mut())
        else {
            return;
        };
        for mode in modes.iter_mut() {
            if mode.get("id").and_then(|v| v.as_str()) != Some(QUICK_MIX_ID) {
                continue;
            }
            if let Some(t) = mode.as_table_mut() {
                t.insert(
                    "candidate_layout".to_string(),
                    toml::Value::String(layout.to_string()),
                );
            }
        }
        info!(
            "Migrated quick_input.force_vertical={forced} into quick_mix candidate_layout={layout}"
        );
    }

    /// 系统预置配置的 TOML 值：代码默认(L1) ⊕ `data/config.toml`(L2)
    /// ⊕ `data_custom/config.toml`(L2.5)，不含用户层(L3)。供 capability 的 `default` 来源。
    ///
    /// 定制层必须进入这个计算：[`preset_for_pruning`] 与 [`materialize_key_actions`] 拿它删用户层的键，
    /// 漏了定制层会把「点回定制默认」写成永久钉死的用户值；算错则静默删掉用户真实设置。
    /// 连带地，定制版设置页的「出厂默认」与「恢复默认」落点都是定制默认值，这是正确语义。
    ///
    /// [`preset_for_pruning`]: Self::preset_for_pruning
    /// [`materialize_key_actions`]: Self::materialize_key_actions
    pub fn system_preset_value(data_dir: Option<&Path>) -> anyhow::Result<toml::Value> {
        let mut merged = toml::Value::try_from(Self::default())?;
        if let Some(data_dir) = data_dir {
            let sys_config = data_dir.join("config.toml");
            if let Some(v) = Self::read_toml_value(&sys_config) {
                merge_value(&mut merged, v);
            }
        }
        if let Some(custom_dir) = Self::custom_data_dir() {
            let custom_config = custom_dir.join("config.toml");
            if let Some(mut v) = Self::read_toml_value(&custom_config) {
                // 与 `load()` 的 L2.5 同一道迁移：否则定制层里旧格式的值在这份基线里未迁移，
                // 剪枝会把用户显式设的同值当成「等于默认」删掉，违反「清理前后 load 结果逐键相同」。
                Self::migrate_user_layer_value(&mut v);
                merge_value(&mut merged, v);
            }
        }
        Ok(merged)
    }

    /// 「与默认相同即不落盘」判定所用的出厂默认（L1⊕L2⊕L2.5）。取不到则 `None`，
    /// 调用方一律退化为「照常写入 / 不清理」。
    ///
    /// 必须确认 `data/config.toml` 在场才返回 `Some`：L2 允许合法覆盖 L1（如 `schema.active`），
    /// 拿纯 L1 或残缺的 L1⊕L2.5 去比对，会把用户显式设的值误删，`load()` 时回落成另一个值。
    /// 定制层只写差异键，单独在场也必须返回 `None`；其值由 [`system_preset_value`] 自动带入。
    ///
    /// [`system_preset_value`]: Self::system_preset_value
    fn preset_for_pruning() -> Option<toml::Value> {
        let dir = Self::data_dir()?;
        if !dir.join("config.toml").is_file() {
            return None;
        }
        Self::system_preset_value(Some(&dir)).ok()
    }

    /// 清理用户层里与出厂默认（L1⊕L2⊕L2.5）相同的冗余键，返回删除的键数。
    ///
    /// 不变量：清理前后 `load()` 的结果逐键相同，只影响将来默认值变更能否到达该用户。
    /// `set_user_value` 负责不再产生新的冗余键，本函数清存量。幂等；`data/config.toml`
    /// 或用户层缺失时返回 0。
    pub fn prune_user_config() -> anyhow::Result<usize> {
        let Some(dir) = Self::user_config_dir() else {
            return Ok(0);
        };
        let file = dir.join("config.toml");
        // 严格解析，失败即什么都不做（与 `materialize_key_actions` 同类：后台自动跑的写盘）。
        // 不要改用 `parse_toml_lenient`：剪枝会整表写回，容错救回的近似值当种子会永久删掉被跳过的行。
        let mut root = match std::fs::read_to_string(&file) {
            Err(_) => return Ok(0), // 没有用户配置 = 没什么可清理的
            Ok(s) => match toml::from_str::<toml::Value>(&s) {
                Ok(v) => v,
                Err(e) => {
                    // 这条 WARN 是必要的：本函数在启动流程里排在任何一次 `load()` 之前，
                    // 静默返回的话，日志里第一条与配置有关的记录要等到 materialize 那一步。
                    warn!(
                        "降级闸：用户配置 {} 语法不合法（{e}），已跳过「陈旧键清理」；\
                         这不是失败，是拒绝拿残表覆盖你的配置——修好该文件的语法后即自动恢复",
                        file.display()
                    );
                    return Ok(0);
                }
            },
        };

        // 仍有值迁移读取的退役键，先把迁移结果落盘再清（见 RETIRED_KEYS 里该条的说明）。
        // 迁移会把旧键从 root 里摘掉，故需单独计数，否则 removed==0 时不写盘、迁移白做。
        let before = root.clone();
        Self::migrate_font_size_follow_theme_value(&mut root);
        Self::migrate_langbar_badge_colors_value(&mut root);
        Self::migrate_tooltip_sections_value(&mut root);
        Self::migrate_stale_factory_tooltip_sections_value(&mut root);
        Self::migrate_comment_max_chars_value(&mut root);
        Self::migrate_grammar_enabled_value(&mut root);
        let migrated = usize::from(root != before);
        // 退役键（[`RETIRED_KEYS`]）先清：它们与出厂默认无关，不能被 preset 取不到时的
        // 提前返回挡住——否则没装 data/config.toml 的环境永远清不掉。
        let mut removed = migrated + prune_retired(&mut root);
        // 冗余键需要出厂默认做逐键比对，取不到时跳过（安全降级为「不清理」的旧行为）。
        if let Some(preset) = Self::preset_for_pruning() {
            removed += prune_redundant(&mut root, &preset);
        }
        if removed == 0 {
            return Ok(0);
        }

        let out = toml::to_string_pretty(&root)?;
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, &file)?;
        info!("Pruned {} stale key(s) from user config", removed);
        Ok(removed)
    }

    /// 一次性物化：把折算后的引导键绑定写进用户层 `keys.key_actions`，此后停止折算。
    /// 返回物化的绑定条数（0 = 已做过 / 条件不满足）。
    ///
    /// 必须落盘：`trigger_keys` 的出厂值住在 L2，设置页只写 `key_actions`，用户层没有能压制
    /// L2 的键，「每次加载折算」会把用户删掉的绑定重新插回去。物化后 `key_actions` 是唯一真相源，
    /// L2 的 `trigger_keys` 只作「恢复默认」的出厂声明。新用户（用户层文件不存在）也照常物化，
    /// 否则删除依然会被折算复活。背景见 `docs/design/key-actions-materialization.md`。
    ///
    /// 四道安全闸，命中即什么都不做：
    /// 1. 用户配置目录不可用（漫游未挂载）：用户层看起来是空的，照做会抹掉真实自定义。
    /// 2. L2 `data/config.toml` 不在场：折算出的是残表，物化即永久丢失出厂绑定。
    /// 3. 本次 `load()` 的 `keys` 段被降级：`bindings` 只剩出厂值，而写回是整表覆盖并置版本标记，
    ///    用户自定义绑定会永久丢失。
    /// 4. 用户 config.toml 语法不合法：发生在合并之前，闸三拦不到。
    ///
    /// 幂等靠 `keys.key_actions_materialized` 版本号，不靠推断。
    pub fn materialize_key_actions() -> anyhow::Result<usize> {
        // 闸一：用户配置目录不可用。
        let Some(dir) = Self::user_config_dir() else {
            return Ok(0);
        };
        // 闸二：L2 必须在场（判据与 `preset_for_pruning` 同源——出厂值只在 L2 看得见）。
        let data_dir = Self::data_dir();
        if !data_dir
            .as_deref()
            .is_some_and(|d| d.join("config.toml").is_file())
        {
            debug!("Skip key_actions materialization: data/config.toml absent");
            return Ok(0);
        }
        let file = dir.join("config.toml");
        // 闸四：文件在盘上但语法不合法 → 不动。不能当空表继续：本次加载不生效只是内存态，
        // 拿空表当种子写回才真正删掉文件内容；且空表恒判为未物化，每次启动都会重跑。
        // 只用严格解析、不用 `parse_toml_lenient`：整表写回的种子必须与盘上内容逐字节对应，
        // 容错解析跳过的行会被永久删除。
        let mut root = match std::fs::read_to_string(&file) {
            // 文件不存在 = 新用户，按空表继续（见上文「对新用户也必须跑」）。
            Err(_) => toml::Value::Table(Default::default()),
            Ok(s) => match toml::from_str::<toml::Value>(&s) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        "降级闸：用户配置 {} 语法不合法（{e}），已跳过「key_actions 物化」；\
                         这不是失败，是拒绝拿残表覆盖你的配置——修好该文件的语法后即自动恢复",
                        file.display()
                    );
                    return Ok(0);
                }
            },
        };
        if !root.is_table() {
            root = toml::Value::Table(Default::default());
        }

        // 幂等闸：版本号已达标就退出（不必读 L2、不必 load）。
        if already_materialized(&root) {
            return Ok(0);
        }

        // 权威来源 = 一次完整 `load()`：它已跑完 normalize，`key_actions` 就是当前生效
        // 的那张表（四层合并 ⊕ 折算 ⊕ 「用户显式配过的不被覆盖」）。这里绝不能自己再抄
        // 一遍折算规则——抄一份就是第二个真相源，本次修的正是这类问题。
        let cfg = Self::load(data_dir.as_deref())?;
        // 闸三：`keys` 段本次被降级过 ⇒ `key_actions` 是出厂残表而非用户绑定。判据用 `affects`
        // （降级可细到 `keys.key_actions`），走共用的 [`ConfigDegradation::blocks_write_back`]。
        // 判 `keys` 而非 `keys.key_actions`：本函数还会摘掉 `input.*.trigger_keys`。
        if cfg
            .degradation
            .blocks_write_back("keys", "key_actions 物化")
        {
            return Ok(0);
        }
        let bindings = cfg.keys.key_actions;
        let count = bindings.len();
        let dropped = materialize_into(&mut root, &bindings)?;

        let out = toml::to_string_pretty(&root)?;
        std::fs::create_dir_all(&dir)?;
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, &file)?;
        info!(
            "Materialized {} key_action binding(s) into user config (v{}), dropped {} legacy trigger_keys field(s)",
            count, KEY_ACTIONS_MATERIALIZE_VERSION, dropped
        );
        Ok(count)
    }

    /// 读取 TOML 文件为 Value（不存在/读不了返回 None，不中断加载）。
    ///
    /// 不关心语法诊断的调用方用这个；要把语法故障报进 [`ConfigDegradation`] 的
    /// 用 [`Self::read_toml_value_reporting`]。两者共用同一套容错解析，行为一致。
    fn read_toml_value(path: &Path) -> Option<toml::Value> {
        Self::read_toml_value_reporting(path, "", &mut Vec::new())
    }

    /// [`Self::read_toml_value`] 的带诊断版本：语法不合法时尽量救回，并记进 `unparsable`
    /// （`layer` 为空则只记日志不上报）。
    ///
    /// 「文件不存在」与「文件语法错」必须走出不同的结果：下游对前者用空表是对的，
    /// 对后者用空表就是拿空表当种子覆盖用户配置。
    fn read_toml_value_reporting(
        path: &Path,
        layer: &'static str,
        unparsable: &mut Vec<UnparsableLayer>,
    ) -> Option<toml::Value> {
        if !path.exists() {
            // 「文件不存在」曾是唯一无日志的失败路径：它让「用户没有配置」与
            // 「开机早期读不到配置」在日志上完全同形，只能靠有无 `Loaded user config`
            // 反推。DEBUG 级——`load()` 在热重载/RPC 上高频调用，不能进 INFO 刷屏。
            debug!("Config file absent: {}", path.display());
            return None;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            // 编码不对是文件内容的问题，与语法错同类：必须进 `unparsable` 关上写盘闸，
            // 否则这一层按「不存在」处理，下次保存就拿默认值覆盖掉用户的文件。
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                warn!("配置文件不是 UTF-8 编码 {}：{}", path.display(), e);
                if !layer.is_empty() {
                    unparsable.push(UnparsableLayer {
                        layer,
                        path: path.to_path_buf(),
                        error: e.to_string(),
                        skipped_lines: Vec::new(),
                        salvaged_keys: 0,
                        not_utf8: true,
                    });
                }
                return None;
            }
            Err(e) => {
                // 读不出来（权限、被独占、坏扇区）不是语法问题，不进 `unparsable`：
                // 那份文件在盘上可能完好，报「语法错误」会把用户支去改一个没错的文件。
                warn!("Cannot read config {}: {}", path.display(), e);
                return None;
            }
        };
        let parsed = parse_toml_lenient(&content);
        if parsed.is_clean() {
            return parsed.value;
        }

        let salvaged_keys = parsed
            .value
            .as_ref()
            .and_then(|v| v.as_table())
            .map_or(0, |t| t.len());
        // WARN 而非 INFO：整层配置失效是异常，默认不打印的 INFO 会让它无迹可查。
        if let Some(err) = &parsed.first_error {
            warn!(
                "配置文件语法不合法 {}：{}（已跳过 {} 行，救回 {} 个顶层段）",
                path.display(),
                err,
                parsed.skipped_lines.len(),
                salvaged_keys
            );
        }
        if !layer.is_empty() {
            unparsable.push(UnparsableLayer {
                layer,
                path: path.to_path_buf(),
                error: parsed.first_error.clone().unwrap_or_default(),
                skipped_lines: parsed.skipped_lines.clone(),
                salvaged_keys,
                not_utf8: false,
            });
        }
        parsed.value
    }

    /// 归一化 + 存量迁移（如 per_page=0 视为未设置）。幂等。
    ///
    /// 必须挂在所有配置生效的必经之路上，由 `ConfigBundle::build` 统一调用，而不只在 `load()`：
    /// `refresh_config_in_memory` 直接重建 bundle，漏掉折算会让引导键在设置页保存后全部失效。
    pub fn normalize(&mut self) {
        if self.ui.candidate.per_page == 0 {
            self.ui.candidate.per_page = default_per_page();
        }
        self.migrate_quick_mix_pinyin_member();
        self.migrate_quick_input_legacy_member();
        self.migrate_letter_trigger_keys();
        // 须在 migrate_letter_trigger_keys 之后：那一步先把字母项摘干净，
        // 这里看到的 trigger_keys 已只剩符号键。
        self.migrate_trigger_keys_into_key_actions();
        self.warn_legacy_schema_hotkeys();
        // 这里不把四组键组配置折算进 `session_actions`：折算是消费层的视图，不是存储层的改写，
        // 见 `KeysConfig::effective_session_actions`。
        self.warn_legacy_special_modes();
    }

    /// 残留的 `schema.special_modes` 告警（不迁移，见 `docs/redesign/overlay-mode-config.md` §5）。
    ///
    /// 该键已废弃，实例集合改由带 `[overlay]` 段的已安装方案定义。不做自动迁移：新家在
    /// `schema_overrides/{id}.toml`，而 `normalize` 是零 IO、幂等、纯内存的，写盘型迁移代价过高。
    /// 但不能静默丢弃，warn 让「配了没反应」可见。
    fn warn_legacy_special_modes(&mut self) {
        if self.schema.legacy_special_modes.is_empty() {
            return;
        }
        let ids: Vec<String> = self
            .schema
            .legacy_special_modes
            .iter()
            .map(|v| {
                // 身份现在就是方案 id，故优先报 `schema` 字段——那才是用户要去建
                // `[overlay]` 段的那个方案文件。
                v.get("schema")
                    .or_else(|| v.get("id"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("?")
                    .to_string()
            })
            .collect();
        warn!(
            "schema.special_modes 已废弃且不再生效（残留 {} 条：{}）。\
             改法：在对应方案的 .schema.toml（或 schema_overrides/<id>.toml）加 [overlay] 段，\
             引导键与直达热键写进 keys.key_actions（如 backslash = \"special:<方案id>\"）。\
             详见 docs/redesign/overlay-mode-config.md",
            ids.len(),
            ids.join(", ")
        );
        // 清空：留着会让后续任何「有没有配过」的判断读到假信号。
        self.schema.legacy_special_modes.clear();
    }

    /// 残留的 `keys.schema_hotkeys` 告警（不迁移）。
    ///
    /// 该键已废弃，方案直达热键并入 `keys.key_actions` 的 `switch_schema:<方案id>`。不做折算：
    /// 保留兼容路径要永远维护两套形态的等价性。处置同 [`Self::warn_legacy_special_modes`]，
    /// warn 让失效可见。
    fn warn_legacy_schema_hotkeys(&mut self) {
        if self.keys.legacy_schema_hotkeys.is_empty() {
            return;
        }
        let mut pairs: Vec<String> = self
            .keys
            .legacy_schema_hotkeys
            .iter()
            .map(|(id, key)| format!("{id} = {key:?}"))
            .collect();
        // HashMap 迭代序不稳定，排序后再报：同一份配置每次启动都该给出同样的告警文本，
        // 否则日志比对时会以为配置变过。
        pairs.sort();
        warn!(
            "keys.schema_hotkeys 已废弃且不再生效（残留 {} 条：{}）。\r
             改法：写进 keys.key_actions，如 \"ctrl+shift+r\" = \"switch_schema:<方案id>\"\r
             （单向切换；要「再按一次回到来源」则用 toggle_schema:<方案id>）。\r
             也可在设置页「方案 → 选中方案 → 设置 → 进入方式」重配一次。",
            pairs.len(),
            pairs.join(", ")
        );
        // 清空：留着会让后续任何「有没有配过」的判断读到假信号。
        self.keys.legacy_schema_hotkeys.clear();
    }

    /// 存量迁移：四处 `trigger_keys` → `keys.key_actions`（设计文档五c「全局层收编」）。
    ///
    /// 只在内存里折算、不写回，与其余 `migrate_*` 同策略：回退一个版本仍能工作，旧字段在用户
    /// 下次用设置页保存时消失。
    ///
    /// 冲突处置为先到先得，顺序必须复现 `try_activate_mode` 的真实顺序：
    /// 临英 > 临拼 > special > mix（见 `handle_lifecycle.rs`），以代码为准；顺序错了，同一个键
    /// 配给多处的用户升级后会进错模式。未中选的几处也清空，免得留下静默失效的配置。
    /// 已在 `keys.key_actions` 显式配过的键不覆盖。
    ///
    /// 它让 `key_actions` 成为跨段折算的产物：来源住在 `input` 段，段级降级换掉 `input` 时，
    /// 这几条也随之变成出厂值，而只问 `keys.*` 的降级判据够不着。`wind-webdata` 的
    /// `keys_overview` 刻意不把 `input` 拉进判据，前提是本折算只对未物化的存量用户跑一次；
    /// 别把新的折算来源加成常驻的。
    fn migrate_trigger_keys_into_key_actions(&mut self) {
        // 已物化：`key_actions` 是唯一真相源，本折算必须让位，否则守卫 `contains_key` 分不清
        // 「配过别的」与「删掉了」，会复活用户删掉的绑定。仍要清空 `trigger_keys`，
        // 维持 `normalize()` 后「trigger_keys 恒为空」的后置条件。
        if self.keys.key_actions_materialized >= KEY_ACTIONS_MATERIALIZE_VERSION {
            self.clear_all_trigger_keys();
            return;
        }
        // (键名 → 动词)，按优先级先到先得。
        let mut claimed: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        // 收走一处的触发键：中选的进 claimed，未中选的丢弃并 warn。两种情况都清空原字段。
        let mut take = |keys: &mut Vec<String>, action: String, owner: &str| {
            for k in keys.drain(..) {
                let k = k.trim().to_lowercase();
                if k.is_empty() {
                    continue; // 空串项是历史遗留的占位，本就不生效
                }
                match claimed.get(&k) {
                    Some(winner) => warn!(
                        "配置迁移：{owner} 的引导键 {k:?} 与 {winner:?} 争用，\
                         按激活链已归 {winner:?}（该键在 {owner} 上原本就静默失效）"
                    ),
                    None => {
                        claimed.insert(k, action.clone());
                    }
                }
            }
        };

        take(
            &mut self.input.temp_english.trigger_keys,
            "temp_english".to_string(),
            "input.temp_english",
        );
        take(
            &mut self.input.temp_pinyin.trigger_keys,
            "temp_pinyin".to_string(),
            "input.temp_pinyin",
        );
        take(
            &mut self.input.rare_char.trigger_keys,
            "rare_char".to_string(),
            "input.rare_char",
        );
        // 特殊模式不在此折算：它的实例已不来自 `schema.special_modes`，引导键与直达
        // 热键直接写在 `keys.key_actions` 里（`special:<方案id>`）。残留旧配置由
        // `warn_legacy_special_modes` 告警，不做迁移（见设计文档 §5）。
        for m in self.schema.mix_modes.iter_mut() {
            if m.id.is_empty() {
                m.trigger_keys.clear();
                continue;
            }
            let owner = format!("schema.mix_modes[{}]", m.id);
            take(&mut m.trigger_keys, format!("mix:{}", m.id), &owner);
        }

        for (key, action) in claimed {
            // 键名解析不了的（历史脏数据）丢弃并 warn：留着也进不了任何通路。
            if crate::hotkey::route_of_key_action(&key).is_none() {
                warn!("配置迁移：引导键 {key:?} 无法解析为按键，已丢弃（动作 {action:?}）");
                continue;
            }
            if self.keys.key_actions.contains_key(&key) {
                debug!("配置迁移：{key:?} 已在 keys.key_actions 显式配置，保留用户设置");
                continue;
            }
            // debug 而非 info：本折算每次 `Config::load` 都跑且不写回，info 会让日志看起来像同一件事
            // 被反复做了 N 遍。真正异常（键名解析不了 / 多处争用）仍是 warn。
            debug!("配置迁移：引导键 {key:?} → keys.key_actions = {action:?}");
            self.keys.key_actions.insert(key, action);
        }
    }

    /// 清空六处 `trigger_keys`（内存态），维持 `normalize()` 后置条件「trigger_keys 恒为空」。
    /// 只动内存：磁盘上 L2 的 `trigger_keys` 是「恢复默认」读取的出厂声明处，必须留着。
    fn clear_all_trigger_keys(&mut self) {
        self.input.temp_english.trigger_keys.clear();
        self.input.temp_pinyin.trigger_keys.clear();
        self.input.rare_char.trigger_keys.clear();
        for m in self.schema.mix_modes.iter_mut() {
            m.trigger_keys.clear();
        }
    }

    /// 存量迁移：`trigger_keys` 里的单字母 → 方案级 [`BoundAction`]。
    ///
    /// 字母的特殊能力已收归 `schema.codetable.z_key_action`，解析端只认符号，不迁移字母就会被
    /// `filter_map` 静默丢弃。`z` 折算成对应 action，其余字母丢弃并 `warn`。
    /// 归属优先级同老的模式激活链（临拼 > 特殊模式 > mix）；已显式配过 `z_key_action` 的不覆盖。
    fn migrate_letter_trigger_keys(&mut self) {
        /// 摘掉 `keys` 里的所有单字母项，返回其中是否含 `z`。
        fn take_letters(keys: &mut Vec<String>, owner: &str) -> bool {
            let mut has_z = false;
            keys.retain(|k| {
                let k = k.trim().to_lowercase();
                let is_letter = k.len() == 1 && k.as_bytes()[0].is_ascii_lowercase();
                if !is_letter {
                    return true;
                }
                if k == "z" {
                    has_z = true;
                } else {
                    warn!(
                        "配置迁移：{} 的引导键 \"{}\" 已失效（字母不再作引导键），已移除。\
                         若需让某个字母进模式，请配 schema.codetable.z_key_action（仅支持 z）",
                        owner, k
                    );
                }
                false
            });
            has_z
        }

        let mut migrated: Option<String> = None;
        let mut claim = |action: String| {
            if migrated.is_none() {
                migrated = Some(action);
            }
        };

        if take_letters(
            &mut self.input.temp_pinyin.trigger_keys,
            "input.temp_pinyin",
        ) {
            claim("temp_pinyin".to_string());
        }
        // 特殊模式那一路已随 `schema.special_modes` 一并废弃：z 要进某个 overlay 方案，
        // 直接写 `schema.codetable.z_key_action = "special:<方案id>"`（或方案级 key_actions）。
        for m in self.schema.mix_modes.iter_mut() {
            let owner = format!("schema.mix_modes[{}]", m.id);
            if take_letters(&mut m.trigger_keys, &owner) {
                claim(format!("mix:{}", m.id));
            }
        }

        // 已显式配过则不覆盖：用户的新配置优先于存量迁移。
        if let Some(action) = migrated
            && self.schema.codetable.z_key_action.trim().is_empty()
        {
            info!(
                "配置迁移：z 引导键 → schema.codetable.z_key_action = \"{}\"",
                action
            );
            self.schema.codetable.z_key_action = action;
        }
    }

    /// 存量迁移：合并成员 `"quick_input"` → 细分来源 [`wind_quick_input::LEGACY_EXPANSION`]。
    ///
    /// 旧值在原位展开，顺序与展开序一致，存量用户的候选序不变。对所有 mix 生效：
    /// `"quick_input"` 是保留 id，只可能是这个含义。
    fn migrate_quick_input_legacy_member(&mut self) {
        for m in self.schema.mix_modes.iter_mut() {
            let Some(at) = m
                .members
                .iter()
                .position(|s| s == wind_quick_input::MEMBER_LEGACY)
            else {
                continue;
            };
            // 展开时跳过已单独写在别处的细分来源，避免重复成员。
            let expansion: Vec<String> = wind_quick_input::LEGACY_EXPANSION
                .iter()
                .filter(|e| !m.members.iter().any(|s| s == *e))
                .map(|e| e.to_string())
                .collect();
            m.members.splice(at..=at, expansion);
        }
    }

    /// 存量迁移：内置 `quick_mix` 的字面 `"pinyin"` 成员 → [`MIX_MEMBER_PRIMARY_PINYIN`] 占位符。
    ///
    /// `members` 从未开放给用户，存量配置里的字面 `"pinyin"` 只可能是设置页整组写回时带出的
    /// 旧默认值，替换是安全的。用户自定义 mix 的字面 id 一律精确解释，不动。
    fn migrate_quick_mix_pinyin_member(&mut self) {
        for m in self
            .schema
            .mix_modes
            .iter_mut()
            .filter(|m| m.id == QUICK_MIX_ID)
        {
            for s in m.members.iter_mut() {
                if s == DEFAULT_PINYIN_SCHEMA {
                    *s = MIX_MEMBER_PRIMARY_PINYIN.to_string();
                }
            }
        }
    }

    /// 应用数据目录名：正式版 `WindInput`；dev 变体 `WindInputDev`
    /// （隔离调试与正式版的配置/缓存/日志，与管道后缀同源于运行时变体探测）。
    pub fn app_dir_name() -> &'static str {
        crate::variant::app_dir_name()
    }

    /// 宿主显式注入的 `(用户配置目录, 本地目录)`；没注入为 `None`（桌面恒如此）。
    ///
    /// 给没有「漫游 / 本地」known folder 的平台用（如 Android：用户目录放在文件管理器可达处，
    /// 缓存与日志留在私有目录）。优先于便携、`datadir.conf` 与系统目录。
    fn dir_override() -> Option<(PathBuf, PathBuf)> {
        DIR_OVERRIDE.read().ok().and_then(|g| g.clone())
    }

    /// 注入用户配置目录与本地目录（见 [`Self::dir_override`]）。进程启动早期、读任何配置之前调用；
    /// 重复调用以最后一次为准（测试用）。
    pub fn set_dir_override(user: PathBuf, local: PathBuf) {
        if let Ok(mut g) = DIR_OVERRIDE.write() {
            *g = Some((user, local));
        }
    }

    /// 用户配置目录（config.toml / meta.toml / `data/` 下的全部用户数据 / 旧库 userdata.redb(.legacy)）。
    /// - 便携模式：`<exe目录>/userdata/`
    /// - 自定义数据目录（安装向导选定，落 `datadir.conf`）：该目录本身
    /// - 正常模式：漫游 `%APPDATA%\WindInput[Dev]`（随用户在多设备间同步）
    ///
    /// 三者优先级即上述顺序。注意自定义目录只影响本函数——`local_dir()` 系
    /// （cache / logs / state.toml）不跟随，详见 `variant::custom_userdata_dir`。
    pub fn user_config_dir() -> Option<PathBuf> {
        if let Some((user, _)) = Self::dir_override() {
            return Some(user);
        }
        if crate::variant::is_portable() {
            return crate::variant::portable_userdata_dir();
        }
        if let Some(d) = crate::variant::custom_userdata_dir() {
            return Some(d);
        }
        dirs::config_dir().map(|d| d.join(Self::app_dir_name()))
    }

    /// 「用户配置目录取不到」时的错误文本，写进进程已知的事实（便携与否、安装根）。
    ///
    /// [`Self::user_config_dir`] 只有两条出口返回 `None`：便携模式取不到安装根，或非便携下漫游
    /// AppData 解析失败。`datadir.conf` 非法 / 目录建不出来都回落默认、不会走到这里，
    /// 所以话术不能指向自定义数据目录。
    fn no_user_config_dir_error() -> anyhow::Error {
        if crate::variant::is_portable() {
            return anyhow::anyhow!(
                "取不到便携模式的用户数据目录（定位不到程序所在目录）。\
                 请确认程序未被移动或删除后重试"
            );
        }
        anyhow::anyhow!(
            "取不到用户配置目录：系统未能提供漫游数据目录（%APPDATA%）。\
             常见于漫游配置文件尚未挂载或账户环境异常，通常重启输入法服务\
             （或重新登录）即可恢复；若持续出现请附带本条消息反馈"
        )
    }

    /// 探测用户配置目录当前是否可用。纯查询，无副作用、不重试。
    ///
    /// 判据建在漫游根目录而非 `config.toml` 上：全新安装本就没有 config.toml，
    /// 建在文件上会让每个新用户白等一个完整超时。
    pub fn probe_user_config() -> UserConfigProbe {
        if crate::variant::is_portable() {
            return match crate::variant::portable_userdata_dir() {
                Some(d) => UserConfigProbe::Portable(d),
                None => UserConfigProbe::RoamingUnavailable,
            };
        }
        // 宿主显式指定的目录（移动端）同样不经漫游 known folder，恒就绪。
        if let Some((user, _)) = Self::dir_override() {
            return UserConfigProbe::CustomDir(user);
        }
        // 自定义目录同样绕开漫游 known folder，恒就绪——若漏了这一支，配置已指向
        // 自定义目录、探测却仍盯着漫游根，就会出现「等一个根本不用的目录」的错配。
        if let Some(d) = crate::variant::custom_userdata_dir() {
            return UserConfigProbe::CustomDir(d);
        }
        let Some(root) = dirs::config_dir() else {
            return UserConfigProbe::RoamingUnavailable;
        };
        if !root.is_dir() {
            return UserConfigProbe::RoamingMissing(root);
        }
        let dir = root.join(Self::app_dir_name());
        let file_exists = dir.join("config.toml").is_file();
        if !file_exists && Self::user_config_seen() {
            // 看不到 config.toml，但本地标记说这用户确有配置：开机早期漫游 profile 还没挂载完的竞态。
            // 继续等，别退回系统预置。
            return UserConfigProbe::ConfigPending { dir };
        }
        UserConfigProbe::Ready {
            dir_exists: dir.is_dir(),
            file_exists,
            dir,
        }
    }

    /// 本地「用户配置曾存在」标记文件路径（`%LOCALAPPDATA%\WindInput[Dev]\user_config.seen`）。
    ///
    /// 放非漫游目录：它登录即挂载，能可靠仲裁可能迟到的漫游 `config.toml`。
    /// 便携模式恒 `None`：便携不经漫游，[`Self::probe_user_config`] 永远读不到它，而
    /// [`Self::local_dir`] 指向用户可见目录，写了只会造出无人读的文件。闸门放在路径这一处，
    /// 读写两端与日后新调用点都能挡住。
    fn user_config_marker_path() -> Option<PathBuf> {
        if crate::variant::is_portable() {
            return None;
        }
        Self::local_dir().map(|d| d.join("user_config.seen"))
    }

    /// 查询本地「用户配置曾存在」标记。纯查询、无副作用、不重试——供 [`probe_user_config`]
    /// 区分「默认用户（永不等）」与「定制用户但漫游未挂载（要等）」。
    pub fn user_config_seen() -> bool {
        Self::user_config_marker_path()
            .map(|p| p.exists())
            .unwrap_or(false)
    }

    /// 若本用户当前确有 `config.toml`（即定制过设置），落下本地标记（幂等）。
    ///
    /// 只应由服务启动路径调用一次，绝不放进 [`load`](Self::load)：`load()` 在热重载/RPC 上高频
    /// 调用，且被单元测试直接执行，从中写盘会污染真实 `%LOCALAPPDATA%`。
    pub fn mark_user_config_seen_if_present() {
        // 只有确实看得到用户 config.toml 时才记；看不到就不记，避免把
        // 「漫游没挂载」误记成「用户有配置」而污染下次判断。
        let Some(user_dir) = Self::user_config_dir() else {
            return;
        };
        if !user_dir.join("config.toml").is_file() {
            return;
        }
        let Some(marker) = Self::user_config_marker_path() else {
            return;
        };
        if marker.exists() {
            return;
        }
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&marker, b"1") {
            Ok(()) => info!("Marked user-config-seen: {}", marker.display()),
            Err(e) => warn!("Failed to write user-config-seen marker: {e}"),
        }
    }

    /// 阻塞等待用户配置目录就绪，最多 `timeout`。返回是否就绪。
    ///
    /// 只应在服务启动早期、logger 初始化之后调用一次。不要放进 `load()`：热重载与 RPC
    /// 也走 `load()`，阻塞会卡住输入。超时后降级为系统预置配置继续启动，不死等。
    pub fn wait_user_config_ready(timeout: std::time::Duration) -> bool {
        Self::wait_until_settled(
            Self::probe_user_config,
            timeout,
            std::time::Duration::from_millis(250),
        )
    }

    /// [`Self::wait_user_config_ready`] 的可注入内核：探测源与轮询间隔都是参数，
    /// 以便测试开发机上走不到的重试分支。
    fn wait_until_settled(
        mut probe_fn: impl FnMut() -> UserConfigProbe,
        timeout: std::time::Duration,
        interval: std::time::Duration,
    ) -> bool {
        let start = std::time::Instant::now();
        let mut attempts = 0u32;

        loop {
            let probe = probe_fn();
            if probe.is_settled() {
                // 就绪状态也记录：dir_exists/file_exists 能直接回答
                // 「是路径没解析出来，还是配置真的不在」，无需再猜。
                info!(
                    "User config ready after {} attempt(s), {}ms: {:?}",
                    attempts,
                    start.elapsed().as_millis(),
                    probe
                );
                return true;
            }
            if start.elapsed() >= timeout {
                warn!(
                    "User config NOT ready after {}ms ({} attempts), last={:?}; \
                         falling back to system preset — user settings will be ignored",
                    start.elapsed().as_millis(),
                    attempts,
                    probe
                );
                return false;
            }
            if attempts == 0 {
                warn!("User config dir not ready, waiting: {:?}", probe);
            } else {
                debug!(
                    "User config dir still not ready (attempt {}): {:?}",
                    attempts, probe
                );
            }
            attempts += 1;
            std::thread::sleep(interval);
        }
    }

    /// 用户覆盖命中时的统一日志打点，[`Self::log_layer_override`] 的用户层专用入口，
    /// 留给自己实现多级回落的解析点（`wind-engine` 的方案/词库解析）。
    ///
    /// `kind` 是资源类别（`schema` / `dict` / `resource` / `shuangpin` / `theme` / `data`），
    /// `rel` 是方案/数据根下的相对路径。只在命中用户层时调用，未覆盖的默认安装不产生日志。
    /// 所有解析点共用 `覆盖生效` 措辞（`wind-theme` 的 `用户覆盖生效[theme]` 也含此子串），
    /// 一次 grep 即可列出全部生效的覆盖。
    ///
    /// `shadowed` 不可省：安装目录也有同名文件才是真覆盖（info）；没有时只是第三方方案资源
    /// 走用户目录（debug），否则几十个词库会把真正的覆盖淹掉。
    pub fn log_user_override(kind: &str, rel: &str, path: &Path, shadowed: bool) {
        Self::log_layer_override("user", kind, rel, path, shadowed);
    }

    /// 覆盖命中时的统一日志打点，`layer` 为命中的层名（`user` / `custom` / `data`）。
    ///
    /// 措辞统一为 `覆盖生效[层][类别]`，可按 `覆盖生效` grep 全部覆盖，或按 `覆盖生效[custom]`
    /// 只看定制版带来的差异。`layer == "data"` 不打点：那是常态，打了就刷屏，
    /// 「出现即线索」的性质也随之失效。
    pub fn log_layer_override(layer: &str, kind: &str, rel: &str, path: &Path, shadowed: bool) {
        if layer == "data" {
            return;
        }
        if shadowed {
            info!(
                "覆盖生效[{}][{}]: {} → {}",
                layer,
                kind,
                rel,
                path.display()
            );
        } else {
            debug!(
                "非覆盖资源[{}][{}]: {} → {}",
                layer,
                kind,
                rel,
                path.display()
            );
        }
    }

    /// 「靠前的层优先、逐层回落」的解析内核。`sub` 为各层共同的子目录
    /// （方案类资源传 `Some("schemas")`，数据根文件传 `None`）。层序来自
    /// [`Self::resource_layers_with`]：`user > custom > data`。
    fn resolve_overridable(
        data_dir: Option<&Path>,
        sub: Option<&str>,
        rel: &str,
        kind: &str,
    ) -> Option<PathBuf> {
        if rel.is_empty() {
            return None;
        }
        let under = |base: &Path| -> PathBuf {
            match sub {
                Some(s) => base.join(s).join(rel),
                None => base.join(rel),
            }
        };
        let layers = Self::resource_layers_named_with(data_dir);
        for (i, layer) in layers.iter().enumerate() {
            let p = under(&layer.path);
            if !p.is_file() {
                continue;
            }
            // `shadowed` = 「确实盖住了更靠后的某一层」，判据必须看全部后续层：
            // 用户层盖住 custom 层同样是「覆盖了自带数据」，只看 data 层会把它记成
            // 第三方自带资源（debug），于是定制版上最该被看见的那类覆盖反而不进日志。
            let shadowed = layers[i + 1..].iter().any(|b| under(&b.path).is_file());
            Self::log_layer_override(layer.name, kind, rel, &p, shadowed);
            return Some(p);
        }
        None
    }

    /// 解析方案附属资源（拆字库/字根字体等 `[engine.chaizi]` 相对路径）：与方案文件同规则，
    /// 按 `user > custom > data` 逐层找 `<层>/schemas/<rel>`（层序见
    /// [`Self::resource_layers_with`]）。第三方方案装在用户目录，其资源只在用户目录下
    /// ——只拼 data_dir 会永远找不到。各层均不存在返回 None（调用方自行告警）。
    pub fn resolve_schema_resource(data_dir: Option<&Path>, rel: &str) -> Option<PathBuf> {
        Self::resolve_overridable(data_dir, Some("schemas"), rel, "resource")
    }

    /// 列出 `schemas/<sub>/` 下所有以 `suffix` 结尾的文件，各层合并、靠前的层遮蔽同名者。
    ///
    /// 返回 `(相对 schemas/ 的路径, 绝对路径)`，按文件名字典序。相对路径用 `/` 分隔：它要写进
    /// 配置（`[[ui.comment_dicts]].path`）并跨平台读回，反斜杠会在 TOML 里撞转义。
    /// 与 [`Self::resolve_schema_resource`] 同一层序：那个答「这个名字在哪一层」，这个答
    /// 「一共有哪些名字」，设置页列可选项用后者。
    pub fn list_schema_resource_dir(
        data_dir: Option<&Path>,
        sub: &str,
        suffix: &str,
    ) -> Vec<(String, PathBuf)> {
        let mut out: Vec<(String, PathBuf)> = Vec::new();
        for layer in Self::resource_layers_named_with(data_dir) {
            let dir = layer.path.join("schemas").join(sub);
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.ends_with(suffix) || !entry.path().is_file() {
                    continue;
                }
                let rel = format!("{sub}/{name}");
                // 先到先得 = 靠前的层遮蔽后面的层，与 `resolve_overridable` 同语义。
                if out.iter().any(|(r, _)| *r == rel) {
                    continue;
                }
                out.push((rel, entry.path()));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// 解析数据根下的程序自带文件（`system.phrases.toml` / `pinyin_map.txt` 等）：
    /// 按 `user > custom > data` 逐层找，整体替换语义（不做键级合并）。各层均无返回 None。
    ///
    /// 合并语义只有 `config.toml`（四层）与 `compat.toml`（字段级）两处，各有专用加载器，不走本函数。
    pub fn resolve_data_file(data_dir: Option<&Path>, rel: &str) -> Option<PathBuf> {
        Self::resolve_overridable(data_dir, None, rel, "data")
    }

    /// 列出数据根 `<sub>/` 下所有以 `suffix` 结尾的文件，各层合并、靠前的层遮蔽同名者。
    /// 与 [`Self::resolve_data_file`] 同一层序，差别同 [`Self::list_schema_resource_dir`] 之于
    /// [`Self::resolve_schema_resource`]：那个回答「这个名字在哪一层」，这个回答「一共有哪些」。
    pub fn list_data_dir(
        data_dir: Option<&Path>,
        sub: &str,
        suffix: &str,
    ) -> Vec<(String, PathBuf)> {
        let mut out: Vec<(String, PathBuf)> = Vec::new();
        for layer in Self::resource_layers_named_with(data_dir) {
            let Ok(rd) = std::fs::read_dir(layer.path.join(sub)) else {
                continue;
            };
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.ends_with(suffix) || !entry.path().is_file() {
                    continue;
                }
                let rel = format!("{sub}/{name}");
                if out.iter().any(|(r, _)| *r == rel) {
                    continue;
                }
                out.push((rel, entry.path()));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// 把语法不合法的配置原件另存一份，返回备份路径。
    ///
    /// 命名 `config.toml.corrupt-<YYYYMMDD-HHMMSS>.bak`，与原文件同目录，便于用户在配置文件夹里
    /// 看到。内容由调用方传入，避免重读的 IO 与「读到的与备份的不是同一份」的竞态。
    /// 失败只告警，返回 `None`。
    fn backup_corrupt_config(file: &Path, content: &str) -> Option<PathBuf> {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let name = format!(
            "{}.corrupt-{stamp}.bak",
            file.file_name()?.to_string_lossy()
        );
        let target = file.with_file_name(name);
        match std::fs::write(&target, content) {
            Ok(()) => {
                info!("已备份语法不合法的配置：{}", target.display());
                Some(target)
            }
            Err(e) => {
                warn!("备份配置失败 {}: {e}", target.display());
                None
            }
        }
    }

    /// 把单个配置项部分合并写入用户层 `config.toml`。只改 `path` 指定的项、保留文件里其它项，
    /// 不写入未改动的默认/系统段。原子写（tmp + rename）。`path` 如 `["ui","candidate","preedit_display"]`。
    ///
    /// 值等于出厂默认（L1⊕L2⊕L2.5，见 [`preset_for_pruning`](Self::preset_for_pruning)）时删除该键
    /// 而不是写入：否则点回默认位就留下显式值，从此不再跟随默认值的后续变更。
    /// 代价是「显式选了与默认相同的值」无法与「跟随默认」区分；若将来要支持锁定某值（pin），
    /// 需另设表达方式，不要退回「照原样写入」。
    pub fn set_user_value(path: &[&str], value: toml::Value) -> anyhow::Result<()> {
        if path.is_empty() {
            anyhow::bail!("set_user_value: empty path");
        }
        let dir = Self::user_config_dir().ok_or_else(Self::no_user_config_dir_error)?;
        std::fs::create_dir_all(&dir)?;
        let file = dir.join("config.toml");

        // 读现有用户层。语法不合法时的处置与 [`Self::materialize_key_actions`] 刻意不同：
        // 那边是后台自动跑，什么都不做最安全；这边是用户主动保存，不写就是「点了没反应」。
        // 故：容错解析救回尽量多的内容 → 先备份原件 → 拿救回的部分当种子写回。
        let existing = std::fs::read_to_string(&file).unwrap_or_default();
        let parsed = parse_toml_lenient(&existing);
        if !parsed.is_clean() {
            // 备份失败就拒绝写回并报 Err：写回会永久删掉没救回的行，备份是唯一的安全网。
            // 报错而非静默，是因为用户需要知道没保存成功（磁盘满 / 只读目录是他能处理的）。
            let backup = Self::backup_corrupt_config(&file, &existing).ok_or_else(|| {
                anyhow::anyhow!(
                    "配置文件 {} 语法不合法，需要先备份原件才能保存，但备份写入失败\
                     （磁盘空间不足或目录只读？）。本次未做任何改动，你的配置文件原样保留。",
                    file.display()
                )
            })?;
            warn!(
                "用户配置 {} 语法不合法（已跳过第 {:?} 行），本次保存{}；原件已备份到 {}",
                file.display(),
                parsed.skipped_lines,
                if parsed.value.is_some() {
                    "只保留可解析的部分"
                } else {
                    // 一个键都没救回来：写回等于用「只含本次修改」的新文件替换原件。
                    "将丢弃原文件全部内容（一个键都没能解析出来）"
                },
                backup.display()
            );
        }
        let mut root = parsed
            .value
            .unwrap_or_else(|| toml::Value::Table(Default::default()));
        if !root.is_table() {
            root = toml::Value::Table(Default::default());
        }
        // 旧字号开关若还在文件里（`prune_user_config` 没跑过），必须在写新值之前迁掉：
        // 否则写入 `font_size = 0`（等于出厂值 ⇒ 被剪掉）后，残留的 `follow = false` 会在
        // 下次 load 被迁成 18，用户刚选的「跟随主题」被打回。
        Self::migrate_font_size_follow_theme_value(&mut root);
        // 气泡旧开关同理：用户把 `sections` 改回出厂值时，写入等于默认 ⇒ 被剪掉，文件里
        // 残留的旧开关（比如 `chaizi_enabled = true`）会在下次 load 被迁回一份合并段，
        // 用户刚选的出厂段列表被打回。
        Self::migrate_tooltip_sections_value(&mut root);
        // 注释字数旧键同理：竖排改回 0（出厂值 ⇒ 被剪掉）后，残留的旧键会在下次 load 补回原值。
        Self::migrate_comment_max_chars_value(&mut root);
        // 语法模型总开关：老用户只写了 weight + model 时，要在写新值之前补上 enabled = true，
        // 否则别的键的保存会把与出厂相同的 weight / model 剪掉，补 true 的依据就丢了。
        Self::migrate_grammar_enabled_value(&mut root);
        // 供落盘后通知钩子用：下方 set_nested 会 move 掉 value。
        let value_for_hook = value.clone();
        // 注释分配的单格 / 单个模板：按名字寻址，不走静态注册表（见 `CommentDynKey`）。
        // 通知必须发真实存在的路径（分配的范围表 / 模板数组）——设置端镜像直接按路径写入，
        // 虚拟键会在里面建出游离子树，之后被它整键写回覆盖。
        let joined = path.join(".");
        let mut dyn_notify: Option<(Vec<String>, toml::Value)> = None;
        if let Some(dynk) = crate::config_schema::comment_dyn_key(&joined) {
            let Some(text) = value.as_str() else {
                anyhow::bail!("{joined} 的值须为字符串");
            };
            if let toml::Value::Table(t) = &mut root {
                dyn_notify = Some(Self::apply_comment_dyn_key(t, dynk, text)?);
            }
        }
        // 出厂默认取不到时 `is_default` 恒 false → 退化为「照常写入」的旧行为（安全降级）。
        // `is_known_key` 与 `prune_redundant` 同一道保险：未登记键（废弃键 / Map 子路径）不收口。
        let is_default = crate::config_schema::is_known_key(&path.join("."))
            && Self::preset_for_pruning()
                .as_ref()
                .and_then(|p| get_nested(p, path))
                .is_some_and(|d| *d == value);
        if let toml::Value::Table(t) = &mut root
            && dyn_notify.is_none()
        {
            if is_default {
                remove_nested(t, path);
            } else {
                set_nested(t, path, value);
            }
        }
        // 角标旧色值：放在 set_nested 之后，盘上已有的与本次新传入的一并迁掉
        // （旧写法只读不写）；放在之前则新值里的 `auto` / 8 位色值会原样落盘。
        Self::migrate_langbar_badge_colors_value(&mut root);

        let out = toml::to_string_pretty(&root)?;
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, &file)?;
        // 落盘成功后通知订阅方（设置界面等）。放在 rename 之后：失败路径提前 `?`
        // 返回，不会误报。传出的是入参值——键被剪枝删除时用户层虽无此键，生效值
        // 仍是它（见上方剪枝说明），对订阅方语义一致。
        match &dyn_notify {
            Some((p, v)) => {
                let p: Vec<&str> = p.iter().map(String::as_str).collect();
                crate::change_hook::notify_changed(&p, v)
            }
            None => crate::change_hook::notify_changed(path, &value_for_hook),
        }
        Ok(())
    }

    /// 把一个 [`CommentDynKey`](crate::config_schema::CommentDynKey) 写进用户层表；
    /// 空串＝删除（格回到下一层 / 删掉该用户模板）。
    ///
    /// 返回落盘后真实存在的通知路径与该路径上的值。途经的值类型不对（`templates` 不是数组、
    /// 中间层不是表）时拒写：那是用户手改坏的内容，静默替换等于无备份销毁。
    fn apply_comment_dyn_key(
        root: &mut toml::Table,
        key: crate::config_schema::CommentDynKey<'_>,
        text: &str,
    ) -> anyhow::Result<(Vec<String>, toml::Value)> {
        use crate::config_schema::CommentDynKey;
        match key {
            CommentDynKey::Cell { scope, name, cell } => {
                let mut path = vec!["ui", "comment", "assign", scope];
                path.extend(name);
                path.push(cell);
                // 逐级核对：已存在的途经项必须是表。
                let mut cur = Some(&*root);
                for (i, k) in path[..path.len() - 1].iter().enumerate() {
                    cur = match cur.and_then(|t| t.get(*k)) {
                        Some(toml::Value::Table(t)) => Some(t),
                        Some(_) => {
                            anyhow::bail!("用户配置里 [{}] 不是表，拒绝覆盖", path[..=i].join("."))
                        }
                        None => None,
                    };
                }
                if text.is_empty() {
                    remove_nested(root, &path);
                } else {
                    set_nested(root, &path, toml::Value::String(text.to_string()));
                }
                let scope_path = vec!["ui", "comment", "assign", scope];
                let v = get_nested(&toml::Value::Table(root.clone()), &scope_path)
                    .cloned()
                    .unwrap_or_else(|| toml::Value::Table(Default::default()));
                Ok((scope_path.iter().map(|s| s.to_string()).collect(), v))
            }
            CommentDynKey::Template { id } => {
                let path = ["ui", "comment", "templates"];
                let mut list = match get_nested(&toml::Value::Table(root.clone()), &path) {
                    Some(toml::Value::Array(a)) => a.clone(),
                    None => Vec::new(),
                    Some(_) => anyhow::bail!("用户配置里 ui.comment.templates 不是数组，拒绝覆盖"),
                };
                let pos = list
                    .iter()
                    .position(|t| t.get("id").and_then(|v| v.as_str()) == Some(id));
                match (pos, text.is_empty()) {
                    (Some(i), true) => {
                        list.remove(i);
                    }
                    (Some(i), false) => {
                        if let Some(t) = list[i].as_table_mut() {
                            t.insert("text".into(), toml::Value::String(text.into()));
                        }
                    }
                    (None, false) => {
                        // 与内置同 id 的用户模板会整条替换内置，名称 / 说明沿用内置的，免得被冲成 id。
                        let sys = crate::comment_lib::SystemComments::embedded();
                        let builtin = sys.templates.iter().find(|t| t.id == id);
                        let mut t = toml::Table::new();
                        t.insert("id".into(), toml::Value::String(id.into()));
                        t.insert(
                            "name".into(),
                            toml::Value::String(builtin.map_or(id, |b| b.name.as_str()).into()),
                        );
                        t.insert("text".into(), toml::Value::String(text.into()));
                        if let Some(b) = builtin.filter(|b| !b.desc.is_empty()) {
                            t.insert("desc".into(), toml::Value::String(b.desc.clone()));
                        }
                        list.push(toml::Value::Table(t));
                    }
                    (None, true) => {}
                }
                let v = toml::Value::Array(list.clone());
                if list.is_empty() {
                    remove_nested(root, &path);
                } else {
                    set_nested(root, &path, toml::Value::Array(list));
                }
                Ok((path.iter().map(|s| s.to_string()).collect(), v))
            }
        }
    }

    /// [`set_user_value`](Self::set_user_value) 的字符串便捷形式。
    pub fn set_user_string(path: &[&str], value: &str) -> anyhow::Result<()> {
        Self::set_user_value(path, toml::Value::String(value.to_string()))
    }

    /// [`set_user_value`](Self::set_user_value) 的布尔便捷形式。
    pub fn set_user_bool(path: &[&str], value: bool) -> anyhow::Result<()> {
        Self::set_user_value(path, toml::Value::Boolean(value))
    }

    /// 本机状态目录（设计 `user-data-storage-redesign.md` §2.1）：机器写、不可再生、
    /// 不漫游。`state.toml`、设备 id、锁、运行时快照、小文件都在这里。与 `cache/`（可删）
    /// 和 `logs/` 并列，本机根下只有这三个目录。
    pub fn state_dir() -> Option<PathBuf> {
        Self::local_dir().map(|d| d.join("state"))
    }

    /// 本机状态目录（工具栏位置、日志、缓存等机器相关数据）。
    /// - 便携模式：`<exe目录>/localdata/`
    /// - 正常模式：`%LOCALAPPDATA%\WindInput[Dev]`（不随漫游同步）
    ///
    /// 便携下它与 [`Self::user_config_dir`]（`<exe目录>/userdata/`）不是同一个目录，免得备份
    /// 用户数据时连带拖走缓存与日志。理由见 [`crate::variant::portable_localdata_dir`]。
    pub fn local_dir() -> Option<PathBuf> {
        if let Some((_, local)) = Self::dir_override() {
            return Some(local);
        }
        if crate::variant::is_portable() {
            crate::variant::portable_localdata_dir()
        } else {
            dirs::data_local_dir().map(|d| d.join(Self::app_dir_name()))
        }
    }

    /// 缓存目录（%LOCALAPPDATA%\WindInput\cache，便携下 `<exe目录>/localdata/cache`）：
    /// 词库 .wdat 等可重建产物。
    pub fn cache_dir() -> Option<PathBuf> {
        Self::local_dir().map(|d| d.join("cache"))
    }

    /// 日志目录：[`Self::local_dir`] 下的 `logs`。
    /// - 便携模式：`<exe目录>/localdata/logs`
    /// - 正常模式：`%LOCALAPPDATA%\WindInput[Dev]\logs`
    ///
    /// TSF DLL 那份路径（`FileLogger::_BuildPaths`）在 C++ 里，改动须两边同步。
    pub fn log_dir() -> Option<PathBuf> {
        Self::local_dir().map(|d| d.join("logs"))
    }

    /// 获取 data 目录（安装根目录下的 `data/`，正常即可执行文件同目录）。
    ///
    /// 根走 [`crate::variant::install_root`]——那里带一个仅供测试的注入点，见其文档。
    pub fn data_dir() -> Option<PathBuf> {
        crate::variant::install_root().map(|d| d.join("data"))
    }

    /// 定制版清单（`data_custom/custom.toml`）。`None` = 本机不是定制版。
    ///
    /// 判据是清单文件可解析，不是 `data_custom/` 目录存在。清单同时充当定制版身份
    /// （日志/关于页/报障）、减法清单与版本兼容判据。解析失败则整个 custom 层不启用，
    /// 不做「半启用」：半解析的清单可能丢掉 `hide` 名单，让被删的方案冒出来，难以归因。
    ///
    /// OnceLock 缓存，进程内只读一次盘，故相关测试必须各占一个测试二进制
    /// （见 `tests/datadir_conf.rs`）。
    pub fn custom_manifest() -> Option<&'static CustomManifest> {
        static MANIFEST: OnceLock<Option<CustomManifest>> = OnceLock::new();
        MANIFEST.get_or_init(load_custom_manifest).as_ref()
    }

    /// 定制版数据层目录（`<安装根>/data_custom`）。清单在场且可解析时才返回 `Some`。
    ///
    /// 层序固定为 `data < data_custom < %APPDATA%`：`data_custom` 在安装目录下，
    /// 普通用户无写权限，只承担「随安装包分发的定制」职责；用户个人调整仍走 `%APPDATA%`。
    /// 程序对本目录只读，退役键清理、配置剪枝一律只作用于用户层。
    pub fn custom_data_dir() -> Option<PathBuf> {
        Self::custom_manifest()?;
        crate::variant::install_root().map(|d| d.join(CUSTOM_DATA_DIR_NAME))
    }

    /// 本定制版是否把方案 `id`「删掉了」（`data_custom/custom.toml` 的 `[schemas] hide`）。
    ///
    /// 与 `[schema].hidden` 是正交的两个轴，不得合并（见 [`CustomManifest::schemas`]）：`hidden`
    /// 只是不列进切换列表，仍可被 mix / `schema.active` 引用；hide 是「本定制版里不存在」。
    ///
    /// hide 是绝对的，对所有层生效，包括用户目录里的同名方案文件，否则用户层放一个同名文件
    /// 就能让被删的方案复活。代价是用户无法用被 hide 的 id 命名自己的方案，且现象与
    /// 「文件没放对」难以区分，所以定制者应只 hide 确实要删的内置 id。
    pub fn custom_hides_schema(id: &str) -> bool {
        Self::custom_manifest().is_some_and(|m| m.schemas.hide.iter().any(|h| h == id))
    }

    /// 本定制版是否把主题 `id`「删掉了」（`[themes] hide`）。绝对性同
    /// [`Self::custom_hides_schema`]：被 hide 的主题在用户层同名放一份也不会复活。
    pub fn custom_hides_theme(id: &str) -> bool {
        Self::custom_manifest().is_some_and(|m| m.themes.hide.iter().any(|h| h == id))
    }

    /// 资源层的有序列表：`[user?, custom?, data?]`，靠前者优先。
    ///
    /// 这是枚举点唯一该用的层序来源（方案 / 主题 / 双拼布局 / opencc）。各自写死目录列表的话，
    /// 漏接 custom 层会让定制版里的方案静默不见，且在开发机上复现不出来。用法：
    ///
    /// ```ignore
    /// let dirs: Vec<PathBuf> = Config::resource_layers_with(Some(data_dir))
    ///     .into_iter()
    ///     .map(|d| d.join("schemas"))
    ///     .collect();
    /// ```
    ///
    /// data 层取 [`Self::data_dir`]；调用方已有 data 目录（便携/测试）时用 [`Self::resource_layers_with`]。
    pub fn resource_layers() -> Vec<PathBuf> {
        Self::resource_layers_with(Self::data_dir().as_deref())
    }

    /// 同 [`Self::resource_layers`]，但 data 层由调用方指定（`None` = 无 data 层）。
    pub fn resource_layers_with(data_dir: Option<&Path>) -> Vec<PathBuf> {
        Self::resource_layers_named_with(data_dir)
            .into_iter()
            .map(|l| l.path)
            .collect()
    }

    /// 同 [`Self::resource_layers`]，但带层名（`user` / `custom` / `data`）。
    ///
    /// 解析点用层名打 [`Self::log_layer_override`]，枚举点用它区分内置与用户自带（主题 builtin
    /// 标记、方案可删判定）。统一由此取，免得各枚举点自己猜层名而分叉。
    pub fn resource_layers_named() -> Vec<ResourceLayer> {
        Self::resource_layers_named_with(Self::data_dir().as_deref())
    }

    /// 带层名的层序，data 层由调用方指定（`None` = 无 data 层，只剩 user/custom）。
    ///
    /// custom 层刻意不由 `data_dir` 推导：清单的 OnceLock 缓存是进程级的，随参数变的目录会与
    /// 清单失配。调用方只有 data 层的某个子目录（如 `wind-engine` 的 `schemas_dir`）时，
    /// 用 `None` 取 user/custom 再自行接上 data 层，别从子目录 `parent()` 反推数据根。
    pub fn resource_layers_named_with(data_dir: Option<&Path>) -> Vec<ResourceLayer> {
        let mut layers: Vec<ResourceLayer> = Vec::with_capacity(3);
        if let Some(u) = Self::user_config_dir() {
            layers.push(ResourceLayer::new("user", u));
        }
        if let Some(c) = Self::custom_data_dir() {
            layers.push(ResourceLayer::new("custom", c));
        }
        if let Some(d) = data_dir {
            layers.push(ResourceLayer::new("data", d.to_path_buf()));
        }
        layers
    }

    /// 追溯一个配置键的来源：四层各声明了什么、最终生效的是哪一层的值。用于回答
    /// 「我改的这个值为什么不生效」。
    ///
    /// - 层的值是「该层显式写了什么」。表类型逐键深合并，单层常只是最终值的一部分，
    ///   此时 `effective_layer` 为 `None`。
    /// - `effective` 取自真实的 [`Self::load`]，含 `normalize()` 改写；段级降级时它是出厂默认，
    ///   `degraded` 会说明用户值被丢了。
    /// - 键不在任何层时四层全 `None`；调用方应先用 `config_schema::is_known_key` 挡掉拼错的键。
    ///
    /// 每次调用做 4 次文件读 + 一次完整 `load()`，只用于排查，不要放进热路径。
    pub fn key_origin(key: &str, data_dir: Option<&Path>) -> anyhow::Result<KeyOrigin> {
        Ok(OriginSnapshot::capture(data_dir)?.key(key))
    }

    /// 获取当前激活的 schema ID
    pub fn active_schema(&self) -> &str {
        if self.schema.active.is_empty() {
            self.schema
                .available
                .first()
                .map(|s| s.as_str())
                .unwrap_or("wubi86")
        } else {
            &self.schema.active
        }
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "config_email_config_tests.rs"]
mod email_config_tests;

#[cfg(test)]
#[path = "config_status_position_tests.rs"]
mod status_position_tests;

#[cfg(test)]
mod page_overflow_tests {
    use super::*;

    /// `page_prev|page_next[:commit_and_input|ignore]` 往返；跟随全局档不带后缀；写错的载荷
    /// 不静默降级成普通翻页（那会让「配了没生效」无从察觉）。
    #[test]
    fn page_payload_round_trips() {
        for (v, want) in [
            ("page_prev", SessionAction::PagePrev(PageOverflow::Global)),
            (
                "page_prev:commit_and_input",
                SessionAction::PagePrev(PageOverflow::CommitAndInput),
            ),
            (
                "page_next:ignore",
                SessionAction::PageNext(PageOverflow::Ignore),
            ),
        ] {
            assert_eq!(SessionAction::parse(v), want, "{v}");
            assert_eq!(want.to_string(), v, "{v} 应原样写回");
        }
        assert_eq!(
            SessionAction::parse("page_prev:commit"),
            SessionAction::None
        );
        assert_eq!(SessionAction::parse("page_next:"), SessionAction::None);
    }
}

#[cfg(test)]
mod caret_right_tests {
    use super::*;

    /// `caret_right[:commit|next|tip]` 往返；默认档不带后缀；写错的载荷不静默降级。
    #[test]
    fn caret_right_payload_round_trips() {
        for (v, want) in [
            ("caret_right", CaretEnd::Commit),
            ("caret_right:commit", CaretEnd::Commit),
            ("caret_right:next", CaretEnd::Next),
            ("caret_right:tip", CaretEnd::Tip),
        ] {
            assert_eq!(
                SessionAction::parse(v),
                SessionAction::CaretRight(want),
                "{v}"
            );
        }
        assert_eq!(
            SessionAction::parse("caret_right").to_string(),
            "caret_right"
        );
        assert_eq!(
            SessionAction::parse("caret_right:tip").to_string(),
            "caret_right:tip"
        );
        assert_eq!(
            SessionAction::parse("caret_right:bogus"),
            SessionAction::None
        );
    }
}
