//! 配置系统：四层合并（代码默认值 L1、系统配置 L2、定制版配置 L2.5、用户配置 L3）
//!
//! 与 Go 版本 `wind_input/pkg/config/config.go` 对齐。
//! 配置文件为 TOML 格式，四层合并：默认值 → data/config.toml → data_custom/config.toml
//! → %APPDATA%/WindInput/config.toml。L2.5 见 `docs/design/data-custom-layer.md`。
//!
//! 顶级域（"正交大类"准则，详见 SETTINGS_REVAMP_PLAN.md / docs/config-key-migration.md）：
//! schema(方案+pinyin+模式) / input(输入行为，含 default 启动默认 / phrase 短语) /
//! keys(全部按键) / ui(外观) / stats(统计) / debug。
//!
//! 按进程名的兼容性规则（HostRender 白名单、caret 定位等）不在这里，见
//! `app_compat.rs`（独立的 `compat.toml` 文件，字段级合并，键名不受本文件四层合并约束）。

use anyhow::Context;
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
///
/// 纯函数、不碰文件系统：[`Config::prune_user_config`] 负责 IO，本函数负责判定，
/// 单测得以在不触碰真实 `%APPDATA%` 的前提下验证「清理前后合并结果不变」这条不变量。
///
/// **两道保险，缺一不可**：
/// 1. `is_known_key` —— 只碰注册表登记过的键。这排除掉两类绝不能删的东西：**废弃键**（清理它们
///    是另一件事，必须走显式名单，绝不能靠「preset 里没有」来推断）、以及 `Map`/`StructList`
///    类型键的**下钻子路径**（`input.punct.custom_mappings` 整体才是一个配置项，
///    `collect_leaf_paths` 却会切出 `...custom_mappings."'1"` 这种伪键——删单条是错的语义）。
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

/// **已退役的配置键**：结构体里已经没有它们，`load()` 时被 serde 静默丢弃。
///
/// 但它们会在用户层 `config.toml` 里**永久留存**——写回走的是原始 `toml::Value`
/// （见 [`Config::set_user_value`]），从不经过类型化结构体，未知键因此既不会被读取、
/// 也不会被删除。留着只有坏处：用户翻开配置看见 `enable_english = true`，会以为它还在
/// 起作用，而实际早已无人读取。
///
/// **只能用显式名单**，不能改成「凡未登记键一律删」——`input.punct.custom_mappings.<字符>`
/// 这类 `Map` 子路径同样不在注册表里，一刀切会把用户的自定义标点映射删光。这也正是
/// [`prune_redundant`] 用 `is_known_key` 把未登记键整体排除在外的原因。
/// 引导键物化迁移的当前版本，落在 `keys.key_actions_materialized`。
///
/// 递增即让 [`Config::materialize_key_actions`] 对所有用户再跑一次。只有在「出厂绑定
/// 本身要变、且必须送达存量用户」时才递增——那会**覆盖用户对这批键的修改**，属于
/// 破坏性操作，不是加个新绑定就该做的事。
const KEY_ACTIONS_MATERIALIZE_VERSION: u32 = 1;

const RETIRED_KEYS: &[&[&str]] = &[
    // 与 `mix_modes.members` 构成双真相源，已废弃：英文候选的开关只看 members 里有没有
    // `english`。⚠️ **不是** `schema.mix.enable_english` —— 那个还活着，是混输引擎
    // （`wubi86_pinyin` 这类方案）混入英文词库候选的开关，两者只是名字像。
    &["schema", "quick_input", "enable_english"],
    // 从未被任何逻辑读取过，关掉不产生任何效果（曾被误当作快捷输入的总开关）。
    // 真正的「禁用快捷输入」＝把 quick_mix 的 trigger_keys 清空。
    &["schema", "quick_input", "enabled"],
    // 随英文段独立迁至 `schema.english.frequency.code_scope`。**不做值迁移**：该键
    // 从未随任何版本发布到用户手里（接进设置页的改动与本次迁移在同一个未发布版本内），
    // 且新旧默认值都是 "candidate"，能读到它的只有开发期配置。
    &["schema", "codetable", "frequency", "english_code_scope"],
    // ⛔ `ui.candidate.comment_max_chars`（已拆成 `_vertical` / `_horizontal`）**刻意不登记**。
    //
    // 本清单的不变量是上一段那句「删掉不改变任何生效值」，而该键**仍在被读取**——
    // [`Config::migrate_comment_max_chars_value`] 每次 load 都拿它补两个新键。
    // 而 `prune_user_config` 是在**用户文件**上跑的（服务启动 D2 步），迁移只改**内存**、
    // 从不落盘 ⇒ 登记进来的时序必然是「先把文件里的旧键删掉，下次启动再也迁不到」，
    // 用户配的截断值静默归 0。
    //
    // ⇒ ★ 判据：**一个键只要还有值迁移在读它，就不能进本清单**；反过来，进本清单的前提是
    // 它已经对生效值毫无影响。旧键留在用户文件里不算误导——它确实还在生效（经迁移）。
];

/// 从用户层删除 [`RETIRED_KEYS`] 里的退役键，返回删除数。
///
/// 与 [`prune_redundant`] 同一条不变量：**清理前后 `load()` 的结果逐键完全相同**——
/// 这些键本来就已经被 serde 丢弃，删掉不改变任何生效值。
/// 幂等；`remove_nested` 会顺带回收变空的父表（`[schema.quick_input]` 只剩这两个键时整段消失）。
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
///
/// 判据只认这一个显式版本号，**不做「看起来像迁移过了」的推断**（如「key_actions 非空
/// 就算迁过」）——那种推断在用户手工配过 key_actions 但从没迁移过时会直接猜错，
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
/// 纯函数、不碰文件系统：[`Config::materialize_key_actions`] 负责 IO 与两道安全闸，本函数
/// 负责改写。拆开的理由与 [`prune_redundant`] 相同——`materialize_key_actions` 依赖
/// `user_config_dir()`，直接测就会写用户真实的 `%APPDATA%\WindInput\config.toml`
/// （本仓已有前科：`cargo test -p wind-coordinator` 曾真写 `schema.active`）。
/// **本函数会删键，直接测的代价比那次更大。**
///
/// 调用方须先用 [`already_materialized`] 判幂等：本函数每次都会照写，不自带幂等。
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
/// 数组**必须**当叶子：`schema.mix_modes` / `keys.page_keys` 这类整体就是一个配置项，
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

/// 段级降级的**单次探针**：把 `value` 贴到全默认骨架 `default_v` 的 `path` 处再整体
/// 反序列化。失败即说明毒在这条路径底下，返回该路径**自己的**错误文本。
///
/// 骨架用默认值而不是用户值，是这套机制正确性的来源：其余部分恒定合法，于是失败只可能
/// 来自贴上去的那一段，判定互不干扰。
fn probe_section(default_v: &toml::Value, path: &[&str], value: &toml::Value) -> Option<String> {
    let mut probe = default_v.clone();
    let (last, parents) = path.split_last()?;
    let mut cur = &mut probe;
    for seg in parents {
        // 骨架里没有这条路径 = 未登记键。serde 会忽略它，探不出毒，也不该降级任何东西。
        cur = cur.get_mut(*seg)?;
    }
    cur.as_table_mut()?
        .insert((*last).to_string(), value.clone());
    probe.try_into::<Config>().err().map(|e| e.to_string())
}

/// 对**已判定为坏**的顶层段再探一层：逐个直接子键做探针，返回 `(段.子键, 该子键的错误)`。
///
/// 返回空表示无法细化（该段在用户值或默认值里不是表、或毒不在任何单个子键上），调用方
/// 退回整段降级。**只探这一层**，不再往下递归。
fn narrow_bad_section(
    default_v: &toml::Value,
    section: &str,
    section_value: &toml::Value,
) -> Vec<(String, String)> {
    let (Some(sub), Some(_)) = (
        section_value.as_table(),
        default_v.get(section).and_then(|v| v.as_table()),
    ) else {
        return Vec::new();
    };
    sub.iter()
        .filter_map(|(key, value)| {
            probe_section(default_v, &[section, key], value)
                .map(|err| (format!("{section}.{key}"), err))
        })
        .collect()
}

/// 把 `root` 里 `path`（点分）处的值换成 `default_v` 同路径的默认值；默认值里没有则删除。
///
/// 删除而非保留：路径在默认值里不存在意味着它不是配置键，而它又被探针判成了毒——
/// 带进最终值只会让 `try_into` 再失败一次。
fn reset_path_to_default(root: &mut toml::Value, default_v: &toml::Value, path: &str) {
    let segs: Vec<&str> = path.split('.').collect();
    let Some((last, parents)) = segs.split_last() else {
        return;
    };
    let mut cur = root;
    for seg in parents {
        let Some(next) = cur.get_mut(*seg) else {
            return;
        };
        cur = next;
    }
    let Some(table) = cur.as_table_mut() else {
        return;
    };
    let mut def = default_v;
    for seg in &segs {
        match def.get(*seg) {
            Some(v) => def = v,
            None => {
                table.remove(*last);
                return;
            }
        }
    }
    table.insert((*last).to_string(), def.clone());
}

/// 本次加载中发生的**段级降级**记录（见 [`Config::deserialize_with_section_fallback`]）。
///
/// 这不是配置项，是「这一份 `Config` 是怎么来的」的元信息：哪些段因为反序列化失败
/// 被换成了 L1 默认。异常态，正常加载恒为空。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigDegradation {
    /// 被替换为 L1 默认的段，**点分路径**，按字典序排列。
    ///
    /// 一层（`keys`）表示整个顶层段回落；两层（`ui.font`）表示只有该子表回落、同段其余
    /// 子表的用户值完好。探针能定位到哪一层就记到哪一层——`ui` 一段就有 99 个键，
    /// 整段回落离「一切归零」并不远，而缩小爆炸半径正是这套机制存在的理由。
    pub sections: Vec<String>,
    /// 整份配置都回落到了 L1 默认——毒不在任何单段（例如顶层不是表）。
    /// 与 `sections` 互斥：走到这一步时 `sections` 为空，因为没能定位到任何有毒段。
    pub total_fallback: bool,
}

impl ConfigDegradation {
    /// 本次加载是否发生过降级。
    pub fn is_degraded(&self) -> bool {
        self.total_fallback || !self.sections.is_empty()
    }

    /// 顶层段 `section` 是否受本次降级影响——**含它的子路径**。
    ///
    /// ⚠️ 判据不能写成「`sections` 里精确等于 `section`」：降级粒度可以细到子表，
    /// `keys` 段出问题时记下的可能是 `keys.key_actions` 而不是 `keys`，精确相等会漏判，
    /// 而漏判的后果是本该拦下的写盘照样发生（见 [`Config::materialize_key_actions`] 闸三）。
    ///
    /// 传顶层段名时与 [`Self::taints`] 等价（顶层名不可能有更短的祖先）；本方法只是
    /// 那个更一般判据的一个习惯叫法，实现共用一份。
    pub fn affects(&self, section: &str) -> bool {
        self.taints(section)
    }

    /// **写盘闸的核心判据**：本次加载在 `path` 处的值是否**不可信**。
    ///
    /// # 这个判据存在的理由
    ///
    /// 段级降级把 `load()` 的 `Err` 变成了「成功但某段是出厂值」。原先靠 `?` 保护的下游
    /// 因此拿到**残缺表**并当成用户的真实配置去写盘或导出——降级本身就变成了磁盘上的
    /// 永久数据丢失。本仓已发现**四条**同形状的路径（`materialize_key_actions`、
    /// `cmd_export`、`patch::writes`/`applyPatch`、`setItems` 的 Map 键），逐条打地鼠
    /// 显然会漏下一条，故把判据固化成这一个函数：
    ///
    /// > 凡是拿 `Config::load()` 的结果（或其派生值）当**种子**，再整表写回用户层
    /// > 或导出给用户的路径，落盘前必须先问这里。
    ///
    /// # 两个方向都要判
    ///
    /// `sections` 是点分路径，与待写路径的关系有三种，**都算不可信**：
    ///
    /// - 相等：`keys.key_actions` 降级，要写 `keys.key_actions`；
    /// - 降级段是待写路径的**祖先**：`input.punct` 降级 ⇒ 它下面的
    ///   `input.punct.custom_mappings` 也是出厂值；
    /// - 待写路径是降级段的**祖先**：要整表写 `keys`，而 `keys.key_actions` 降级过
    ///   ⇒ 这张表里有一块是假的。
    ///
    /// 只判前两种会漏掉「写大表、坏在小格」，只判后两种会漏掉「坏在大段、写小格」。
    pub fn taints(&self, path: &str) -> bool {
        fn is_ancestor(ancestor: &str, descendant: &str) -> bool {
            descendant
                .strip_prefix(ancestor)
                .is_some_and(|r| r.starts_with('.'))
        }
        self.total_fallback
            || self
                .sections
                .iter()
                .any(|s| s == path || is_ancestor(s, path) || is_ancestor(path, s))
    }

    /// [`Self::taints`] 的带日志版本：不可信时打一行 WARN 并返回 `true`，
    /// 调用方据此**什么都不做**（与 `preset_for_pruning` 取不到就退化为不清理同构）。
    ///
    /// 统一在这里打点，是为了让四条闸的日志措辞一致、可按 `降级闸` 一次 grep 出全部
    /// 「因为降级而没做的写盘」——排查「我的设置怎么没保存」时，这一行是唯一的线索。
    /// `what` 描述被拦下的动作（如 `key_actions 物化`），用于把日志对上现象。
    #[must_use]
    pub fn blocks_write_back(&self, path: &str, what: &str) -> bool {
        if !self.taints(path) {
            return false;
        }
        warn!(
            "降级闸：本次加载的 [{path}] 不可信（降级段 {:?}，整份回落={}），已跳过「{what}」；\
             这不是失败，是拒绝拿出厂残表覆盖你的配置——修好报错的配置键后即自动恢复",
            self.sections, self.total_fallback
        );
        true
    }
}

/// 完整配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// 本次加载的降级记录，**不是配置键**：`#[serde(skip)]` 让它既不进配置文件、
    /// 也不进 `config.get` 的序列化产物，跟着这一份 `Config` 走完生命周期。
    ///
    /// ★ 选「随实例的字段」而不是「模块级静态快照」：`Config::load` 有多个并发调用方
    /// （RPC dispatch、协调器热重载、CLI、构造期），静态快照会互相覆盖，消费点读到的
    /// 可能是**别人那次**加载的降级结果，而这恰好是最需要可信的场合。
    ///
    /// 已核实的影响面（`skip` 不触碰其中任何一条）：
    /// - `Config` 未派生 `PartialEq`，全仓无 `impl PartialEq for Config`，不存在整体相等性比较；
    /// - `config_schema::config_leaf_keys()` 由 `toml::Value::try_from(Config::default())`
    ///   推导叶子路径，`skip` 字段不出现在序列化产物里 ⇒ `registry_covers_every_config_key`
    ///   的差集不变，注册表无需登记；
    /// - `prune_redundant` / `prune_retired` / `set_user_value` 走的都是那同一套叶子路径，
    ///   同理不受影响；
    /// - `config.get`（`wind-rpc/dispatch.rs`、`wind-webdata/lib.rs`）把 `Config` 序列化
    ///   回 `toml::Value` 再比对/写回，`skip` 字段不出现 ⇒ 不会被当成用户配置写进 config.toml。
    ///
    /// ⚠️ 这里的 `skip` **不是**被禁止的那个 `skip_serializing_if`：后者用于表达
    /// 「某个**配置键**退出配置体系」，会让守门测试静默放行一个用户够不着的键；
    /// 本字段从来就不是配置键。
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
    pub debug: DebugConfig,
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
    /// **跨引擎**的词频公共基线（[schema.frequency]）。
    ///
    /// ⚠️ 与 `schema.{codetable,pinyin,english}.frequency` **是不同的东西，别合并**：
    /// 那三段是各引擎自己的调频参数（策略、保护位数、半衰期），值可以互不相同；本段装的是
    /// 「三个引擎都该照办的同一条规则」。判据是**用户会不会想给不同引擎配不同的值**——
    /// 「emoji 不参与词频」在码表里成立、在拼音里就不成立是说不通的，配三遍只会漂移。
    #[serde(default)]
    pub frequency: FrequencyGlobal,
    /// **已废弃**：特殊模式的实例集合改由「带 `[overlay]` 段的已安装方案」定义
    /// （`EngineManager::overlay_modes`），见 `docs/redesign/overlay-mode-config.md`。
    ///
    /// 字段保留只为**读得出残留值以便告警**（[`Config::warn_legacy_special_modes`]）——
    /// 删掉的话 serde 会静默丢弃这一段，用户改了半天配置没反应还查不到原因。
    /// 不参与任何行为，也不再写出（`skip_serializing_if`）。
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
    /// 这些 Unicode 区块的候选**不参与词频**：既不记录选中（不学习），也不受已有记录影响
    /// （不重排）。取值为区块名（`"表情符号"`）或预设组名（`"emoji"`），
    /// 解析见 `wind_candidate::BlockMask::from_config`。
    ///
    /// 出厂**为空**（＝行为与改动前完全一致）。诉求来自「emoji 在正常输入时不要参与词频
    /// 调整」：emoji 多是一次性的点缀，被它顶到前面会把常用字挤下去，而用户下次多半又想
    /// 打回那个字。
    ///
    /// ★ **空列表即关闭，故不另设 `enabled` 开关**——「开着但一个区块都没选」是个无意义
    /// 状态，两个键就要多解释一次它们的组合（配置设计规则 R3 的「枚举当开关」同款判据）。
    ///
    /// ⚠️ **写端与读端必须同时照办**：只跳过记录的话，用户库里既有的 emoji 词频记录仍在
    /// 生效，开关看起来像没反应。两端共用 `FreqSettings::excluded_from_freq` 一个判据。
    #[serde(default)]
    pub exclude_blocks: Vec<String>,
}

/// 全局英文配置（[schema.english]）。
///
/// 英文自 0.114 起是可切换方案，行为不再挂靠码表段——那是历史包袱：英文引擎复用了
/// 码表的重排路径，配置就顺手挂在了 `schema.codetable` 下，于是纯码表用户的「上屏行为」
/// 里混着只对英文生效的项，而英文用户改调频策略又会连带改掉五笔的。
///
/// ⚠️ **不 derive `Default`**：本段有默认 `true` 的字段，而 `derive(Default)` 只会给 bool
/// 零值。serde 的 `#[serde(default = "default_true")]` 只在**反序列化缺键**时生效，
/// 管不着 `Config::default()` 这条路——两条路不一致的后果是「出厂配置文件里写着 true、
/// 代码里的默认值却是 false」，而这种分叉只有端到端测试才看得见
/// （见 `config-design-rules` §R4「L1 与 L2 必须一致」）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnglishGlobal {
    #[serde(default)]
    pub frequency: EnglishFrequency,
    /// 英文方案下上屏一个词后**再补一个空格**。
    ///
    /// 英文是词间带空格的语言，连续打词时每次上屏都要多按一次空格。开启后由输入法补上。
    ///
    /// 生效范围（消费点见 `english_appends_space` / `english_space_enabled`）：
    /// - **所有选中方式**——空格 / 数字键 / 次三选键 / 修饰键选词 / 鼠标点选；
    /// - **空格上屏原码**（打了词库里没有的词）；
    /// - **不含**回车上屏原码（终结性动作）、标点键顶屏（会得到 `hello ,`）、顶码。
    #[serde(default)]
    pub commit_space: bool,
    /// 首候选是用户所打原文（英文方案下的「输入即内容」保证）。**默认开**。
    ///
    /// 英文引擎的特殊性：输入串本身就是合法上屏内容。而调频一旦把某个词顶到首位，
    /// 想上屏所打原文就只剩回车这一条路，而回车是终结性动作、会打断连续输入流。
    /// 码表方案没有这个问题——`aaaa` 不是可上屏文本，所以这条**不下放给码表**。
    ///
    /// 与 `input.temp_english.raw_candidate` 是**两个作用域各一份**，不是两个真相源：
    /// 用户对「中文里插一个英文词」与「长时打英文」的需求本就可能相反。
    #[serde(default = "default_true")]
    pub raw_candidate: bool,
    /// 生成大小写变形候选（全小写 / 首字母大写 / 全大写）。**默认关**。
    ///
    /// ★ 与临英那份（`input.temp_english.case_variants`，默认**开**）默认值刻意相反，
    /// 这正是「两个作用域场景不同」的证据：临英是「中文里插一个英文词」，人名与专有名词
    /// 要首字母大写是刚需；英文方案是长时打英文，每条变形吃一个候选位，每页 5 条时吃掉
    /// 一半。⛔ 合并成一个键必然改掉其中一侧的既有行为。
    #[serde(default)]
    pub case_variants: bool,
}

impl Default for EnglishGlobal {
    fn default() -> Self {
        Self {
            frequency: EnglishFrequency::default(),
            commit_space: false,
            raw_candidate: true,
            case_variants: false,
        }
    }
}

/// 英文调频（[schema.english.frequency]）。
///
/// 不 derive `Eq`：`half_life` 是 f64。与 `CodetableFrequency` / `PinyinFrequency` 一致。
///
/// **没有 `protect_top_n*`**：那组是「简码位首选保护」，判据是本次输入的码长——英文
/// 没有简码位这回事，一个 `a` 后面跟的是几万个词而不是钦定首选，照搬过来只会锁死
/// 前几位不让调频。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnglishFrequency {
    #[serde(default)]
    pub enabled: bool,
    /// `"top"` / `"step"` / `"position"`，默认 `"position"`。
    ///
    /// **与码表默认不同**：英文候选几乎全是前缀匹配，`top`/`step` 那种「用过一次即整体
    /// 跳到没用过的那批之前」在这里过于激进——误选一次就把词顶到很显眼的位置且不衰减。
    /// `position` 每次只前移一半、久不用会回落，更适合前缀为主的场景。
    #[serde(default = "default_english_freq_strategy")]
    pub strategy: String,
    /// 前缀补全候选参与位置提升的范围；**仅 `strategy = "position"` 时生效**。
    ///
    /// 英文默认 `"all"`：它的候选**本来就几乎全是前缀补全**（打 `hel` 出 `hello`），
    /// 收窄到 `single` 等于把调频关掉大半。
    #[serde(default = "default_codetable_promote_prefix")]
    pub promote_prefix: String,
    /// 衰减半衰期（小时），`0` = 内置默认 72 小时；仅 `position` 策略生效。
    #[serde(default)]
    pub half_life: f64,
    /// **词频记账码口径**（`"candidate"` / `"input"`，默认 `"candidate"`）。
    ///
    /// 原 `schema.codetable.frequency.english_code_scope`，随英文段独立迁到这里。
    ///
    /// | 取值 | 打 `hel` 选 `hello` 记成 | 之后打 `he` |
    /// |---|---|---|
    /// | `"candidate"`（默认） | `(hello, hello)` | **也受益**（跨码位共享） |
    /// | `"input"` | `(hel, hello)` | 不受益（码位独立） |
    ///
    /// ⚠️ 本项**按候选来源生效，不按当前方案**——混输方案里混进来的英文候选同样读它。
    /// 故 `EngineManager::freq_settings` 的**每个分支**都要从这里取值，不能只在
    /// 「当前是英文方案」时读。
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
            strategy: default_english_freq_strategy(),
            promote_prefix: default_codetable_promote_prefix(),
            half_life: 0.0,
            code_scope: default_english_code_scope(),
        }
    }
}

/// 全局拼音配置（[schema.pinyin]）。所有拼音类方案共用，无方案级 override。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinyinGlobalConfig {
    #[serde(default = "default_true")]
    pub show_code_hint: bool,
    #[serde(default = "default_true")]
    pub use_smart_compose: bool,
    /// 拼音分隔策略（"auto" 等）。原 input.pinyin_separator 收拢至此。
    #[serde(default = "default_pinyin_separator")]
    pub separator: String,
    #[serde(default)]
    pub fuzzy: PinyinFuzzy,
    /// 拼音调频（衰减参数；全局唯一，按引擎分——见 docs/redesign/schema-config-layering.md §3.4）。
    #[serde(default)]
    pub frequency: PinyinFrequency,
    /// 拼音自动造词（全局唯一）。
    #[serde(default)]
    pub auto_learn: AutoLearnConfig,
    /// 词组补全的音节数约束（全局唯一）。
    #[serde(default)]
    pub completion: PinyinCompletion,
    /// 双拼相关的全局行为（`[schema.pinyin.shuangpin]`）。
    #[serde(default)]
    pub shuangpin: PinyinShuangpin,
    /// 辅助码字形二次筛选（`[schema.pinyin.aux_code]`）。**出厂关闭**。
    #[serde(default)]
    pub aux_code: AuxCodeGlobal,
    /// 上下文语言模型（n-gram 语法模型）。
    #[serde(default)]
    pub grammar: PinyinGrammar,
}

/// 辅助码的**全局基线**（`[schema.pinyin.aux_code]`）：拼音候选的字形二次筛选。
///
/// 与方案段 `[engine.aux_code]`（[`crate::schema::AuxCodeSpec`]）的分工同码表那套
/// （schema-config-layering.md §4）：那里放 `files`（这个方案配哪张码表，属方案属性），
/// 本段放「这台机器怎么用辅助码」的行为基线，方案可用 tri-state `Option` 逐字段覆盖。
///
/// 两个字段的出厂值恰好都是类型默认值（`false` / `0`），故用 `derive(Default)`
/// 而非手写 `impl`——**这不是巧合而是设计**：出厂值必须是「什么都不发生」。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AuxCodeGlobal {
    /// 总开关。**出厂 `false`**。
    ///
    /// ## ★ 为什么默认必须是关
    ///
    /// 同 [`PinyinGrammar::model`] 那条：默认值必须是「什么都不发生」。辅助码会
    /// **改变一个已有按键的语义**（双拼下反引号从标点变成进入筛选模式），并把
    /// 数十万条码表读进内存。这两件事都不该在用户没表态时发生。
    ///
    /// 方案文件里配了 `files` 只表示「这个方案推荐这张表」，不构成开启意图——
    /// 判据分离在 [`Self::resolved`]。
    #[serde(default)]
    pub enabled: bool,
    /// 词组长度上限：字数 > 此值的**词组**一律排除、不参与筛选（0 = 不限）。
    /// 单字恒参与匹配，不受此限。
    #[serde(default)]
    pub max_phrase_len: usize,
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
        out
    }
}

/// 上下文语言模型（`[schema.pinyin.grammar]`）。
///
/// 整句解码原本只有 unigram（词频），分不出「是想 / 思想」这类需要上下文才能定夺的解读。
/// 本段挂上一个 n-gram 搭配模型给**词与词之间的转移**打分。
/// 设计与实测见 `docs/design/language-model-integration.md`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinyinGrammar {
    /// 影响力权重。**0 = 关闭**：不加载模型文件、不占内存，且整句结果与没有这个
    /// 功能时**逐位相同**。
    ///
    /// 之所以默认关闭：接上模型会大幅重排整句结果（实测跨词边界命中率 88%、
    /// 搭配分跨度 12.76 nat，是每词固定罚 `WORD_PENALTY`=3.0 的四倍以上），
    /// 相关常数需要整套重新标定后才谈得上默认开启。
    #[serde(default = "default_grammar_weight")]
    pub weight: f64,
    /// 模型文件名，相对 `data/schemas/pinyin/grammar/`。**默认空串 = 不启用**。
    ///
    /// 格式是 librime-octagram 的 `.gram`（darts-clone double-array）。
    /// 模型数据**不随安装包分发**（许可与体积，见设计文档 §5），需用户自行获取。
    ///
    /// ## ★ 为什么默认是空串而不是某个具体模型名
    ///
    /// 默认值必须是「什么都不发生」。填任何具体文件名都意味着：一旦用户
    /// **只**把 `weight` 调成非 0（很自然的做法——文档就是这么写的），
    /// 就会静默启用那个默认模型。而我们实测过的三个模型里：
    ///
    /// - `zh-hans-bgc`（字级）在 192 条整句评测上是 **−6**
    /// - `zh-hans-bgw`（词级）是 **−4**——它曾长期是本字段的默认值
    /// - 只有 `wanxiang-lts-zh-hans`（万象，420MB）是正的（+5）
    ///
    /// 也就是说，旧的默认值会把用户静默导向一个**质量为负**的模型。
    /// 空串则让「没配模型」直接落到不启用，用户必须**同时**显式给出 weight 与 model
    /// 才会生效——两个字段都写过一遍，就不存在「不知道自己开了什么」。
    #[serde(default)]
    pub model: String,
}

fn default_grammar_weight() -> f64 {
    0.0
}

impl Default for PinyinGrammar {
    fn default() -> Self {
        Self {
            weight: default_grammar_weight(),
            model: String::new(),
        }
    }
}

/// 双拼相关的**全局**行为（`[schema.pinyin.shuangpin]`）。
///
/// 与**方案级** `engine.pinyin.shuangpin` 的分工：那里放 `layout`（＝这个方案的编码规则，
/// 换布局就是换方案）；本段放「这台机器怎么用双拼」的偏好，一次配置对所有双拼方案生效。
///
/// 判据即本仓那条「配置落点看**实例身份从哪来**」：布局的身份来自方案，故归方案级；而
/// 「允不允许别人用全拼打字」跟装了哪个双拼方案无关，归全局。
///
/// 代价是**不能按方案区分**（`PinyinGlobalConfig` 全体无方案级 override）——做不到
/// 「小鹤允许、自然码不允许」。真有此需求时应整体重估这一段的归属，而不是单独挪一个键。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinyinShuangpin {
    /// 双拼方案下是否额外把击键串当全拼解释一遍（`nihao` → 「你好」）。
    ///
    /// 服务「多人共用一台机器」：主力用户打双拼，偶尔来的人只会全拼。产出的候选里只有
    /// **低置信**那部分沉底（前缀补全 / 子短语），精确整词与整句和双拼候选同层竞争，
    /// 见 [`wind_candidate::Candidate::is_fullpinyin_fallback`]。
    ///
    /// 非双拼方案下本项无效（引擎侧判据是 `shuangpin.is_some() && 本项`）；混输的拼音
    /// 次引擎强制关闭，理由同 `PinyinConfig::enable_partial_final`。
    ///
    /// 默认 `false`：新功能，不打扰既有用户。
    #[serde(default)]
    pub allow_full_pinyin: bool,
}

/// 词组补全（前缀补全）的音节数约束（`[schema.pinyin.completion]`）。
///
/// 约束的是「码比输入长」的补全词——即引擎在**预测用户尚未输入的音节**。精确匹配、
/// 子短语、整句、简拼都不受这两项影响（它们不预测任何东西）。
///
/// 判据的尺子是 `started` = 输入的完整音节数 + (有尾部残码 ? 1 : 0)，是**输入自身的
/// 属性**。允许的候选音节数上限 = `started < min_syllables ? started : started + max_extra_syllables`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinyinCompletion {
    /// 至少输入几个音节才给出词组候选。
    ///
    /// 补全词的音节数恒 ≥ 输入音节数，故 `started < min_syllables` 时上限收紧到
    /// `started` 本身，效果就是「只出同音节数的候选」——单字母 `d` 与单音节 `dian`
    /// 只出单字，不再混进「但是」「电话」。取 1 = 不设限（回到历史行为）。
    ///
    /// 尾部残码算作起头的一个音节：`dianh`(dian + h) 已经算 2 个，故它照常出「电话」。
    #[serde(default = "default_completion_min_syllables")]
    pub min_syllables: u32,
    /// 词组最多比输入多几个音节。
    ///
    /// 0 = 只给音节数与输入相等的词；1 = 只补下一个音节（`nih` → 你好/你会，
    /// 不给 4 音节的「你会发现」）；4 以上才够 `zhonghuar` → 「中华人民共和国」
    /// （输入 3 音节、词 7 音节）。数值越大，引擎越敢预测你还没打的内容。
    #[serde(default = "default_completion_max_extra_syllables")]
    pub max_extra_syllables: u32,
}

/// 取 4，与两个参考实现独立选定的门槛一致：librime 的
/// `UserDictionary::kNumSyllablesToPredictWord = 4`、fcitx5-chinese-addons 的
/// `LongWordLengthLimit` 默认 4。语义都是「输入不足 4 个音节时不预测用户没打的内容」。
///
/// 旧值 2 会让 `zaim`（2 音节）混进「在美国」「在没有」这类 3 音节候选——档位排序
/// （`cmp_completion_extra`）能把它们压到后面，但压不掉「候选列表里全是没打的音节」
/// 这个体感。参考实现是从**召回**层面直接不给。
fn default_completion_min_syllables() -> u32 {
    4
}

/// 取 5：与 `min_syllables = 4` 配合，`started = 4` 时上限 4 + 5 = 9 音节，恰好够
/// 「冰冻三尺非一日之寒」这类 9 音节长成语在打到第 4 个音节时被召回。
///
/// ⚠️ **两个旋钮必须配合改**。上限 = `started < min ? started : started + max_extra`，
/// 故 `min = 4` 配旧值 3 会把上限压到 7，9 音节的长词在任何输入长度下都召回不到
/// （报障用户正是自己把本项调到 6 才补上的）。
fn default_completion_max_extra_syllables() -> u32 {
    5
}

impl Default for PinyinCompletion {
    /// ⚠️ 与 [`CodetableGlobal`] 那种「结构体零值」不同，这里给的是**真实默认值**，
    /// 与 `data/config.toml` 的出厂值一致。零值在此没有意义：`min_syllables = 0`
    /// 等于不设限，而 `max_extra_syllables = 0` 是一个合法且很严格的取值，
    /// 没法拿来当「未配置」的哨兵。
    fn default() -> Self {
        Self {
            min_syllables: default_completion_min_syllables(),
            max_extra_syllables: default_completion_max_extra_syllables(),
        }
    }
}

impl Default for PinyinGlobalConfig {
    fn default() -> Self {
        Self {
            show_code_hint: true,
            use_smart_compose: true,
            separator: default_pinyin_separator(),
            fuzzy: PinyinFuzzy::default(),
            frequency: PinyinFrequency::default(),
            auto_learn: AutoLearnConfig::default(),
            completion: PinyinCompletion::default(),
            shuangpin: PinyinShuangpin::default(),
            aux_code: AuxCodeGlobal::default(),
            grammar: PinyinGrammar::default(),
        }
    }
}

/// 全局码表配置（[schema.codetable]）。所有码表方案的公共基线，方案可经
/// `schema_overrides/{id}.toml` 的 `[codetable]` 段（带 enabled 总开关）逐字段覆盖。
/// z 键功能（`schema.codetable.z_key_action` 的解析形态）。
///
/// # 为什么是方案级、且只管 z
///
/// 字母天然是编码键，能否借作引导键取决于**这张码表里它是不是死码**（五笔 86 的 z 是，
/// 别的码表未必）。这是方案的属性，全局 `trigger_keys` 无从表达——那里配了字母就是无条件
/// 抢键，该字母在所有方案里都打不出编码。故字母引导键已从 `trigger_keys` 移除
/// （见 `Coordinator::special_trigger_vk`），能力收归本项。
///
/// 只管 z 而不做「任意字母可配」：本项与 `z_key_repeat` 是同一个键的两个身份，裁决链要在
/// 二者之间选。若本项可配成别的字母，`z_key_repeat` 的状态就会去挡一个与它无关的键
/// （旧实现正是如此：配 `u` 作触发键时，按 u 会被 z 的 repeat 历史挡住不进模式）。
/// 严格同域才自洽。将来真有换字母的需求，改这一处即可。
///
/// # 值域
///
/// - `""` / `"none"`：z 是普通编码字母（默认）
/// - `"temp_pinyin"`：进临时拼音
/// - `"temp_english"`：进临时英文
/// - `"mix:<id>"`：进指定融合模式（`mix:quick_mix` = 内置「快捷」）
/// - `"special:<id>"`：进指定特殊模式
/// - `"toggle_schema:<id>"`：切到指定方案，再按回来
/// - `"switch_schema:<id>"`：切到指定方案，单向（仅全局 `keys.key_actions`，见该变体说明）
///
/// 未知值一律解析成 [`BoundAction::None`]（不静默变成别的功能）；指向不存在的 id 由消费端
/// 的门卫拦下（`mix_members` / `ensure_schema`），并在加载期 `warn`。
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
    /// 进指定融合模式（携带实例 id）。
    Mix(String),
    /// 进指定特殊模式（携带实例 id）。
    Special(String),
    /// 切到指定方案，**再按回到来源**（携带目标方案 id）。
    ///
    /// 往返语义由**运行时来源**兜底，不要求目标方案配对称的绑定。
    /// 见 docs/design/schema-key-actions.md §5。
    ToggleSchema(String),
    /// 切到指定方案，**单向**（携带目标方案 id）。原 `keys.schema_hotkeys` 的语义。
    ///
    /// 全局 `keys.key_actions` 与方案级 `[key_actions]` **都合法**，但作用域不同：
    ///
    /// - **全局**：在所有方案下都命中。目标方案里再按走幂等分支
    ///   （`restore_state_for_same_schema`：把中英态/CapsLock 归位到能用这个方案打字）。
    /// - **方案级**：只在源方案里命中。切走之后目标方案没有这条绑定，这把键在那里
    ///   **被吞掉、不动作**（`Coordinator::schema_switch_arrival` 记录 +
    ///   `handle_bound_modifier_key_up` 的 `NotBound` 分支）。
    ///
    /// ⚠️ **方案级单向没有回程**——这是它与 [`Self::ToggleSchema`] 的全部区别，也正是
    /// 「切过去就完事、不留状态」这个诉求要的东西；回程请交给另一把键。
    ///
    /// ★ 2026-08-30 之前方案级单向被整条禁掉（`bound_key_decision` 让位并 warn），理由是
    /// 「单向切走就回不来了」。那描述的是**这把键**按不动，而回程本可以由别的键负责，
    /// 禁令因此挡掉了合法配法。放开的同时必须保留那条吞键兜底：少了它，键会落回全局链，
    /// 而 `lshift`/`rshift` 出厂就是 `toggle_mode` 键 ⇒「配的是切方案却切了中英文」。
    SwitchSchema(String),
    /// 开关软键盘；`Some(id)` 直接切到指定面（直通车），`None` 保持/切上次那面。
    ///
    /// ⚠️ **不进 [`Self::DISPATCH_ACTIONS`]**，与 `add_word` 同类：软键盘开启后要接管
    /// 全部主键区按键，得返回占位 composition 激活宿主转发，不符 `dispatch_hotkey`
    /// 的 `bool` 契约。混进那份白名单的症状是「按了没反应」。
    /// 判据：**开启后是否需要接管后续按键**——`toggle_toolbar` 不接管，本动作接管。
    SoftKeyboard(Option<String>),
    /// A 类状态切换（`toggle_punct` / `take_screenshot` 那类）。
    ///
    /// 携带动词原文，由协调器转交 `dispatch_hotkey` ——那里是这批动作的既有单点，
    /// 复制一份实现只会让两处慢慢漂移。值域见 [`Self::DISPATCH_ACTIONS`]。
    Action(String),
}

impl BoundAction {
    /// A 类可绑的状态切换动词，与 `Coordinator::dispatch_hotkey` 的分支一一对应。
    ///
    /// 白名单而非「解析得动就收」：写错的动词若静默通过，按下时分发端匹配不上、
    /// 什么都不发生，与「没绑上」完全同形，用户无从分辨（同 `is_supported_key_action`）。
    ///
    /// `add_word` / `open_add_word_dialog` **不在此列**：它们要返回占位 composition
    /// 来激活 C++ 转发全部按键，不符 `dispatch_hotkey` 的 `bool` 契约，在按键路径上
    /// 是单独特判的（`coordinator.rs` 的热键分支）。混进来会变成「按了没反应」。
    pub const DISPATCH_ACTIONS: &'static [&'static str] = &[
        "toggle_mode",
        "switch_engine",
        "toggle_full_width",
        "toggle_punct",
        "toggle_s2t",
        "toggle_toolbar",
        "open_settings",
        "take_screenshot",
    ];

    /// 该动作是否**只能绑无字符键**（修饰键）。
    ///
    /// 判据是「它是不是**从英文态出来**的手段」——是的话，绑在有字符的键上就等于
    /// 单程票：有字符的键走 keydown 链，那条链在英文模式分水岭之后，英文态根本到不了，
    /// 于是切过去就再也切不回来。
    ///
    /// | 动作 | 限修饰键 | why |
    /// |---|---|---|
    /// | `toggle_mode` / `switch_engine` / `toggle_schema:*` / `switch_schema:*` | 是 | 正是用来离开/返回英文态的 |
    /// | `toggle_punct` / `toggle_s2t` / `take_screenshot` … | 否 | 本就只在中文态有意义（全局那份也带 `CHINESE_ONLY`） |
    ///
    /// 这与「键有没有字符」是**正交的两问**，合起来才定得了插入点，见
    /// docs/design/schema-key-actions.md §4.1。
    pub fn requires_modifier_key(&self) -> bool {
        match self {
            Self::ToggleSchema(_) | Self::SwitchSchema(_) => true,
            Self::Action(a) => matches!(a.as_str(), "toggle_mode" | "switch_engine"),
            _ => false,
        }
    }

    /// 解析配置字符串。大小写与首尾空白不敏感；未知值 → [`Self::None`]。
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
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
        let lower = s.to_lowercase();
        match lower.as_str() {
            "temp_pinyin" => Self::TempPinyin,
            "temp_english" => Self::TempEnglish,
            "aux_code" => Self::AuxCode,
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

/// **会话态**按键动词（`keys.session_actions` 的值域）。
///
/// 与 [`BoundAction`] 是同构的姊妹表，分野是**触发态**：那张表管「无输入会话时这个键干
/// 什么」（进模式 / 切方案 / 状态切换），本表管「**正在组合一段输入**时这个键干什么」。
///
/// # 为什么必须是两张表，而不是一张带条件的表
///
/// 因为诉求本身就是「同一个键在两种态下是两个动作」：Tab 有会话时翻页、无会话时该是
/// 宿主的制表符。合表就只能往动词里长出条件维度，分发端迟早要拆回来。
///
/// 更硬的一条是**可达性是物理约束**：C++ 侧把键分成三个区间——`_IsSessionKey` 里的功能键
/// （Tab / Esc / 方向 / PgUp…）有会话时**免费转发**、可打印符号键须带
/// `HOTKEY_POLICY_FORWARD_ONLY` 显式登记、修饰键与 CapsLock 只有 keyup。两张表的边界与
/// 这个区间划分重合，不是巧合。详见 docs/design/session-key-actions.md §3。
///
/// # 判据是「有会话」，不是「有候选」
///
/// 初版拟名 `candidate_actions`、判据取 `!candidates.is_empty()`，**已否决**：`clear` 这类
/// 动词在「打了码还没出候选」时同样要能用，而 C++ 的 `FORWARD_ONLY` 闸门判据本来就是
/// `hasComp || _hasCandidates`。两侧判据必须同构，否则「C++ 吃键集 ⊆ Rust 出字集」这条
/// 不变量守不住。
///
/// 「有候选才有意义」的动词（导航类）由**消费点**自己守一行空候选判据，不靠表结构表达
/// ——状态维度进分发端是加法，进表结构是乘法。
///
/// # 值域
///
/// - `"none"`：在本态禁用该键（第三态，非「未声明」）
/// - `"page_prev"` / `"page_next"`：上一页 / 下一页
/// - `"highlight_up"` / `"highlight_down"`：高亮上移 / 下移
/// - `"cancel"`（别名 `"clear"`）：放弃当前输入，等同 Esc
/// - `"select_candidate:N"`：选中当前页第 N 个候选（N 从 1 起，`2` 即次选键）
/// - `"select_char:N"`：以词定字，取当前高亮候选词的第 N 个字（N 从 1 起）
/// - `"aux_code"` / `"aux_code:page_next"`：进辅助码筛选（后者与下翻页共键）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    /// 未启用 / 显式禁用。
    None,
    PagePrev,
    PageNext,
    HighlightUp,
    HighlightDown,
    /// 放弃当前输入：清空编码与候选，并退出 overlay 模式。**语义完全等同 Esc**。
    ///
    /// # 为什么不另立一个「只清空不退模式」的 `clear`
    ///
    /// 用户诉求原话是「支持自定义输入清空的热键，因为 ESC 在日常输入时有些太远」——
    /// 要的是 **Esc 的替代键**，不是一个新语义。而「清空但留在模式里」在普通输入下与
    /// 本动作**完全无区别**（普通输入没有模式可退），只有 overlay 模式下才分得出来，
    /// 属于没有人要、却要额外定义五个模式各自边界的语义分叉。
    ///
    /// `"clear"` 因此收作**别名**而非第二个动词：用户按「清空」的心智去写照样能用，
    /// 但内核只有一种行为，不会出现「两个名字行为微妙不同」这种最难查的配置陷阱。
    Cancel,
    /// 选中当前页第 N 个候选（**N 从 1 起**，1 = 首选）。收编自 `keys.select_key_groups`。
    ///
    /// 载荷用「第几个」而非内部的 0-based 偏移：配置是给人读的，`select_candidate:2`
    /// 一眼就是「次选键」。转换成偏移在消费点做一次即可。
    SelectCandidate(u8),
    /// 以词定字：取当前高亮候选词的第 N 个字（**N 从 1 起**）。收编自 `keys.select_char_keys`。
    SelectChar(u8),
    /// 进辅助码筛选：对已出的候选按字形码二次过滤。
    ///
    /// # 为什么它在**这张**表而不是 `key_actions`
    ///
    /// 辅助码是**对已有候选的操作**——`enter_aux_code` 第一件事就是查
    /// `state.candidates.is_empty()`，空了直接拒。这正是本表的定义性质
    /// （见 [`Self::requires_candidates`]），而 `key_actions` 那张表管的是「给这个键指定
    /// 一个功能」，不以有会话为前提。
    ///
    /// 实际后果：`key_actions` 只认符号键、字母 z 与四个修饰键
    /// （`key_action_name_to_vk` 的值域），Tab / PageDown / 方向键那一批**根本解析不出来**，
    /// 写进去会被静默丢弃。本表认得它们，于是「Tab 进辅助码」才表达得出来。
    ///
    /// ⚠️ `key_actions` 里的 `aux_code` 仍然有效且保留（双拼出厂就是 `backtick = "aux_code"`）。
    /// 两条路都通向同一个 `enter_aux_code`，差别只在**哪些键名解析得出来**。
    ///
    /// 载荷 [`AuxCodeShare`] 是**这个动词的参数**（共键降级目标），不是第二条绑定，
    /// 理由见那里。
    AuxCode(AuxCodeShare),
}

/// 辅助码触发键的「共键」参数：**进不去辅助码时，这个键改做什么**。
///
/// # 为什么是参数，而不是 `page_next_aux_code` 这种组合动词或 `a|b` 通用链
///
/// 需求是「Tab 既翻页又进辅助码」。三种表达形态里：
///
/// 1. **组合动词**（`page_next_aux_code`）：语义是「两个都做」——先翻页再进入，于是进入
///    瞬间停在第 2 页、首选被翻走，还得在进入路径上保存/恢复 `current_page` 去补救。
///    且每多一个组合就多一个动词，名字随组合数相乘。
/// 2. **通用降级链**（`aux_code|page_next`）：要给每个动词定义「不适用」，而那些条件多是
///    实现细节（`page_next` 的「失败」是已在末页 ⇒ 语法允许的 `page_next|cancel` 会在末页
///    取消整段输入）。可表达的组合远多于有意义的组合，终点是 DSL。**已在
///    `docs/design/key-resolver-unification.md` §5「不做通用降级链」否决过**。
/// 3. **动词参数**（本形态）：值域封闭在 `aux_code` 上，配不出无意义的组合，
///    与 `select_candidate:N` / `mix:id` / `special:id` 同一套 `verb:arg` 写法。
///
/// 判据是本仓已用过三次的那条：**一组取值只对一个动词有意义 ⇒ 它是那个动词的参数**。
/// 「与翻页共键」只对 `aux_code` 成立（没有别的动词需要它），故取第 3 种。
///
/// # 语义：顺序即优先级
///
/// `aux_code:page_next` = **先试辅助码，进不去才翻页**。「进不去」不需要新定义——
/// `enter_aux_code` 本就是「门卫没过返回 `None` 不吞键」的契约，四道门卫
/// （已在别的 overlay / 未启用 / 无码表 / 无候选）直接复用。于是这几种情形全部天然成立：
///
/// - 主输入路 + 辅助码可用 → 进辅助码，**不翻页**（首选不会被翻走）；
/// - 已在辅助码态内 → `active.is_some()` 拒 ⇒ 降级翻页（模式内继续翻页）；
/// - 辅助码未开启 / 方案无码表 → 拒 ⇒ 退化成纯翻页键；
/// - 无候选 → 两个成员都 `requires_candidates` ⇒ 按键放行，Tab 仍是宿主的制表符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxCodeShare {
    /// 专用触发键（`aux_code`）：进不去就不吞键，落该键原本的语义。
    Solo,
    /// 与下翻页共键（`aux_code:page_next`）：进不去时翻下一页。
    ///
    /// **只收下翻页**：`page_prev` 共键没有对应的心智（「翻回上一页」与「进筛选」不构成
    /// 同一个递进动作），放开只是让值域变大。要加成员先问它有没有真实用例。
    PageNext,
}

impl SessionAction {
    /// 解析配置字符串。大小写与首尾空白不敏感；未知值 → [`Self::None`]。
    ///
    /// 未知值不静默变成别的动作，与 `BoundAction::parse` 同策略。写错的动词落 `None`
    /// 后表现为「这个键在会话态没绑定」，与「没配」同形——所以调用方在加载期要 `warn`
    /// （见 [`Self::parse_checked`]）。
    pub fn parse(s: &str) -> Self {
        let t = s.trim().to_lowercase();
        // 带载荷的两个动词。序号从 1 起且限个位数——页内候选与词长都远不到两位数，
        // 放宽只会让 `select_candidate:99` 这种一定不生效的配置被静默收下。
        if let Some(n) = t.strip_prefix("select_candidate:") {
            return match n.trim().parse::<u8>() {
                Ok(n @ 1..=9) => Self::SelectCandidate(n),
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
        match t.as_str() {
            "page_prev" => Self::PagePrev,
            "page_next" => Self::PageNext,
            "highlight_up" => Self::HighlightUp,
            "highlight_down" => Self::HighlightDown,
            // 与 `BoundAction::parse` 的同名动词逐字一致：同一个功能在两张表里写法不同的话，
            // 用户把配置从一张表挪到另一张就会静默失效。
            // （`key_actions` 那张表没有共键形态：它只认符号键与字母 z，翻页键压根解析不出来。）
            "aux_code" => Self::AuxCode(AuxCodeShare::Solo),
            // `clear` 是 `cancel` 的别名（同一个动作，两种心智），见 `Cancel` 的文档。
            "cancel" | "clear" => Self::Cancel,
            _ => Self::None,
        }
    }

    /// 该动作是否**只在有候选时**才有意义。
    ///
    /// 导航类（翻页 / 移高亮）在没有候选时无事可做，消费点据此放行按键、回落原语义；
    /// 而 `cancel` 在「打了码还没出候选」时恰恰**必须**生效——那正是判据从「有候选」
    /// 放宽到「有会话」的理由（见 docs/design/session-key-actions.md §4）。
    ///
    /// ★ 判据挂在**动作**上而不是写在消费点：消费点有三个（主输入 / mix / 候选导航），
    /// 写在那里就要维护三份一致的守卫，而这类「三处必须一致」的约束本仓已栽过四次。
    pub fn requires_candidates(&self) -> bool {
        !matches!(self, Self::None | Self::Cancel)
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

    /// 同 [`Self::parse`]，但把「写错的动词」与「显式 none」区分开，供加载期告警。
    ///
    /// 返回 `None` 表示**值不认识**；`Some(Self::None)` 表示用户明确写了 `none`。
    /// 静默忽略拼写错误与「功能坏了」完全同形，用户无从分辨——同
    /// `is_supported_key_action` 立的口径。
    pub fn parse_checked(s: &str) -> Option<Self> {
        let t = s.trim().to_lowercase();
        if t.is_empty() || t == "none" {
            return Some(Self::None);
        }
        match Self::parse(&t) {
            Self::None => None,
            a => Some(a),
        }
    }

    /// 是否启用（非 [`Self::None`]）。
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// 配置字符串形式（折算与写回用）。与 [`SessionAction::parse`] 互为逆运算。
///
/// 用 `Display` 而非 `as_str() -> &'static str`：带载荷的动词（`select_candidate:2`）
/// 拼不出 `'static` 串。**一个真相源**——写回与解析对不上的话，折算出来的配置自己就
/// 读不回来，而那是启动时才暴露、且看起来像「配置丢了」的一类问题。
impl std::fmt::Display for SessionAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::PagePrev => f.write_str("page_prev"),
            Self::PageNext => f.write_str("page_next"),
            Self::HighlightUp => f.write_str("highlight_up"),
            Self::HighlightDown => f.write_str("highlight_down"),
            // 别名 `clear` 刻意不回写——规范名只有一个，避免同一份配置在两次保存后
            // 出现两种写法。
            Self::Cancel => f.write_str("cancel"),
            Self::SelectCandidate(n) => write!(f, "select_candidate:{n}"),
            Self::SelectChar(n) => write!(f, "select_char:{n}"),
            Self::AuxCode(AuxCodeShare::Solo) => f.write_str("aux_code"),
            Self::AuxCode(AuxCodeShare::PageNext) => f.write_str("aux_code:page_next"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodetableGlobal {
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
    /// 与 [`Self::z_key_repeat`] **正交**（可同时开）：repeat 先手，继续打字母才轮到本项，
    /// 详见 `Coordinator::try_activate_mode` 的三重身份裁决。
    #[serde(default)]
    pub z_key_action: String,
    /// 码元字符集（哪些字符可进输入缓冲）。空=内置默认 `a-z`。
    /// 解析与回落见 [`crate::code_charset::CodeCharSet`]。
    #[serde(default)]
    pub input_chars: String,
    /// 可作**首码**的字符集（`input_chars` 的子集）。空=与 `input_chars` 相同。
    #[serde(default)]
    pub leading_chars: String,
    /// 出简让全：有简码的字，在更长的码位上把首选让给词语（「路」的三简是 `kht`，
    /// 那么 `khtk` 的首选就该给「路上」之类）。值 = **参与让位的简码级别上限**：
    ///
    /// - `0` 关闭（**全局出厂值**），候选顺序完全按词库原序
    /// - `2` 一二级简码置后
    /// - `3` 全部简码置后
    ///
    /// 判据是「当前码长 > 本值」而不是「当前码长 == 全码长」——后者要知道方案有几码，
    /// 换到非四码方案就错位。
    ///
    /// 此前这件事由 `gen_dict` 在词库生成阶段做（`[demotion]` 段，已退役），判定烘进权重、
    /// 用户关不掉，且触发条件是有条件降权而非标准的出简让全语义。
    #[serde(default = "default_short_code_yield_level")]
    pub short_code_yield_level: usize,
    /// 码表调频（统一开关，取代旧 user_frequency）。
    #[serde(default)]
    pub frequency: CodetableFrequency,
    /// 码表自动造词（连续单字）。
    #[serde(default)]
    pub auto_phrase: AutoPhraseConfig,
}

/// 出简让全的**全局**出厂值是关。
///
/// 0.118 曾默认开到三级（理由是与它取代的 `gen_dict` `[demotion]` 同量级，实测 239 vs
/// 205 个码，升级后手感延续）。但全局基线是**所有码表方案**共用的，而「短码首选 = 简码」
/// 这个前提只对五笔这类前缀式简码成立——第三方码表（词频码、二三重码表等）里短码首选
/// 往往就是作者精心排定的常用字，让位纯属破坏。全局默认开会静默改掉这些方案的候选顺序，
/// 而用户根本不知道有这么一项。
///
/// 故改为：**全局关，由方案自己在 `[engine.codetable]` 里声明**（内置 wubi86 已声明 3）。
/// 这与 `z_key_action` 是同一个模式——「这张码表适不适合」只有方案作者知道。
fn default_short_code_yield_level() -> usize {
    0
}

impl Default for CodetableGlobal {
    fn default() -> Self {
        Self {
            // ⚠️ 这是**结构体零值**，不是「出厂默认」——出厂值在 `data/config.toml`（L2 层，
            // 恒覆盖本处）。大量集成测试以 `Config::default()` 构造，把这些拨成 true 会连带
            // 改变它们的输入行为（顶码/标点上屏都会生效）。
            //
            // 特殊方案的折叠基线**另有定义**，见 `EngineManager::SPECIAL_SCHEMA_BASELINE`——
            // 那是「特殊方案该长什么样」，与本处的「结构体零值」不是同一件事，别合并。
            top_code_commit: false,
            clear_on_empty_max: false,
            auto_commit_at_full: false,
            auto_commit_min_len: 0,
            punct_commit: false,
            show_code_hint: true,
            single_code_input: false,
            single_code_complete: false,
            short_code_yield_level: default_short_code_yield_level(),
            z_key_repeat: false,
            z_key_action: String::new(),
            // 空串 = 未设置 → `CodeCharSet::new` 回落内置默认 `a-z`，与历史硬编码
            // `VK_A..=VK_Z` 逐键等价。这里刻意不写 "a-z" 字面量：让「未配置」在
            // 结构体零值与 TOML 缺省两处是同一个值，避免两套默认源不一致。
            input_chars: String::new(),
            leading_chars: String::new(),
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
        if let Some(v) = o.z_key_repeat {
            out.z_key_repeat = v;
        }
        // 码元字符集是 `String` 而非 `Option`，故「未设置」由**空串**表达（与上面那些
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
        // 调频段逐字段折叠。整段缺省 = 全部跟随基线。
        //
        // ⚠️ 这一段的消费方是 `EngineManager::freq_settings`，与上面那些上屏行为字段的
        // 消费方（`build_engine` 的 CommitOptions）不是同一条路径。加字段时两条都要看：
        // 光在这里折叠、`freq_settings` 仍读全局镜像的话，方案文件里写了也没人读。
        if let Some(f) = &o.frequency {
            if let Some(v) = f.enabled {
                out.frequency.enabled = v;
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
        out
    }
}

/// 码表调频（[schema.codetable.frequency]）。
///
/// 不 derive `Eq`：`half_life` 是 f64。与 `PinyinFrequency` 一致。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodetableFrequency {
    #[serde(default)]
    pub enabled: bool,
    /// 锁定码表原始前 N 位——**兜底档**：码长 ≥ 4 的深码位。
    /// 简码位（码长 1/2/3）另有分级值，见下面三个字段。
    ///
    /// ⚠️ 作用域是「码表配置组」而非「纯码表方案」：混输走的也是这套值
    /// （`EngineManager::freq_settings` 按"非拼音即码表"分流）。
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
    /// 词频应用策略：`"top"`（一次到顶 MRU）/ `"step"`（逐次提升，默认）/
    /// `"position"`（位次减半）。原 freq_strategy 迁入。
    ///
    /// `top`/`step` 是**布尔 used-first**——用过一次即整体跳到档内最前，策略只决定「已用过
    /// 的那批内部怎么排」。`position` 让位次连续表达强弱，没有「用过 / 没用过」这道台阶，
    /// 适合**前缀匹配为主**的方案（英文尤甚，其候选几乎全是前缀匹配）。
    #[serde(default = "default_freq_strategy")]
    pub strategy: String,
    /// 前缀补全候选参与词频位置提升的范围（`"none"` / `"single"` / `"all"`）。
    ///
    /// **仅 `strategy = "position"` 时生效**；`top`/`step` 走布尔 used-first，不读本项。
    ///
    /// 码表默认 `"all"`（与拼音的 `"single"` 不同）：码表的前缀补全已由来源档位隔离、
    /// 跨不到精确档之前，无需再按语义单元收窄；且这与 `top`/`step` 的历史行为一致
    /// （那两者对前缀补全从无限制），避免升级后存量用户的调频范围突然变窄。
    #[serde(default = "default_codetable_promote_prefix")]
    pub promote_prefix: String,
    /// **衰减半衰期（小时）**；`0` = 用内置默认 72 小时。**与拼音段完全独立，不回落到它。**
    ///
    /// **仅 `strategy = "position"` 时生效**；`top`/`step` 直接比 `count`/`last_used`，不读衰减。
    ///
    /// 曾做成「`0` 回落到 `schema.pinyin.frequency.half_life`」，已否决：设置页上这是两个
    /// 独立控件，回落链会让用户「把码表的留在 0、改了拼音的、发现码表跟着变」。**回落链只在
    /// 配置层不可见时才是便利，一旦两端都有 GUI 就变成了陷阱。**
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
    /// 半衰期（小时）。
    #[serde(default)]
    pub half_life: f64,
    /// base 系数。
    #[serde(default)]
    pub base_scale: f64,
    /// 最近使用峰值。
    #[serde(default)]
    pub recency_peak: f64,
    /// **前缀补全候选参与词频位置提升的范围**（默认 `"single"`）。
    ///
    /// | 取值 | 打 `d` 选「得」 | 打 `d` 选「东西」 | 打 `hel` 选 `hello` |
    /// |---|---|---|---|
    /// | `"none"` | 不提升 | 不提升 | 不提升 |
    /// | `"single"`（默认） | **提升** | 不提升 | **提升** |
    /// | `"all"` | 提升 | 提升 | 提升 |
    ///
    /// 判据是[语义单元数][wind_candidate::semantic_units]（汉字逐字计、西文词整体计 1），
    /// **不是字符数**——英文候选 `hello` 有 5 个 char，按字符数会被「只提升单个」挡死，
    /// 而英文所有候选都是前缀匹配，那等于英文调频全灭。
    ///
    /// 默认 `"single"` 的理由：短输入下用户给出的信息量撑不起一个词组，把长词组靠词频顶到
    /// 高频单字前面与直觉相悖（微软拼音实测「只对全码生效」是同一取舍的更强版本）。而单字
    /// 之间的调整（「的」/「得」）是合理的，`"none"` 会连它一起挡掉。
    ///
    /// 只作用于**有效前缀层**（`is_prefix && !is_promoted_completion`），与 `cmp_match_layers`
    /// 同口径：被引擎主动提升进完整匹配层的候选是结构决策，不该被本项误伤。
    #[serde(default = "default_promote_prefix")]
    pub promote_prefix: String,
}

fn default_promote_prefix() -> String {
    "single".to_string()
}

/// 码表自动造词（[schema.codetable.auto_phrase]）。
///
/// 语义：连续单字上屏累积成序列，遇终止符（标点/回车/空格/焦点切换/光标移动/多字词上屏）
/// 或超过 `idle_timeout_ms` 未继续时，按方案 `[[encoder.rules]]` 为整个序列算词组编码并
/// 写入**临时词库**（立即可作为候选）；累计使用达 `promote_count` 次才晋升进用户词库。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoPhraseConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 造词最小字数（默认 2；内部字段，设置页不开放）。
    #[serde(default = "default_phrase_min_len")]
    pub min_phrase_len: usize,
    /// 造词最大字数（默认 5；内部字段，设置页不开放）。**超长序列整体放弃**，不截取末尾
    /// N 字——在连续多字中间切一刀，切出来的多半不是词，是杂词的主要来源。
    #[serde(default = "default_phrase_max_len")]
    pub max_phrase_len: usize,
    /// 临时词晋升进用户词库所需使用次数。**0 = 不晋升**，一直留在临时词库（默认）。
    #[serde(default)]
    pub promote_count: usize,
    /// 连续单字之间的最大间隔（毫秒，0=默认 5000）。超过则把已累积序列视作终止。
    /// 兜底用：终止信号全漏时防止跨句拼出「加好加好」这类杂词。内部字段，设置页不开放。
    #[serde(default)]
    pub idle_timeout_ms: u32,
    /// 临时词库条目上限（0=不限）。超出后淘汰权重最低者。内部字段，设置页不开放。
    #[serde(default = "default_temp_max_entries")]
    pub temp_max_entries: usize,
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
        }
    }
}

/// 拼音自动造词（[schema.pinyin.auto_learn]）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoLearnConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 造词最小字数（默认 0=回退 2）。
    #[serde(default)]
    pub min_word_length: usize,
    /// 造词最大字数（`0` = 不限）。超出后先按「从尾部保留整段」裁剪，仍超则**整体放弃**
    /// ——在一串汉字中间切一刀，切出来的多半不是词（同 `AutoPhraseConfig::max_phrase_len`）。
    ///
    /// 整句一次上屏时只有一段可裁，故本项对整句等价于「超过就不学」。默认 10：既覆盖
    /// 「今天天气不错」这类值得进词库的长词，又挡住整句解常见的跨句拼接。
    #[serde(default = "default_learn_max_len")]
    pub max_word_length: usize,
    /// 临时词晋升所需使用次数（原 learning.temp_promote_count）。
    #[serde(default)]
    pub promote_count: usize,
}

/// 拼音造词最大字数默认值。比码表侧的 5 宽松——码表造词的素材是**连续单字序列**
/// （跨句拼接风险高），而拼音整句/分步转换的每一段都是用户明确选中的语义单元。
fn default_learn_max_len() -> usize {
    10
}

/// ⚠️ 手写而非 `derive(Default)`：`max_word_length` 的默认值是 10 而非零值，
/// derive 会让**代码默认**（`AutoLearnConfig::default()`）给 0=不限、而**配置默认**
/// （serde 反序列化缺键）给 10，同一个语义在两条路上分叉。
impl Default for AutoLearnConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_word_length: 0,
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
    /// 满码自动上屏 **与顶码上屏**遇拼音候选则否决（保护拼音用户）。默认开。
    ///
    /// 这是**粗粒度**一票否决：整串只要能查出任何拼音候选就让路拼音，不看拼音成不成词。
    /// 与细粒度的 `block_commit_on_pinyin_word`（按词强度判，默认开）叠加生效，
    /// 二者任一命中即否决（见 `pinyin_vetoes_commit`）。
    /// 注意作用面覆盖顶码上屏，而 `schema.codetable.top_code_commit` 出厂即开——
    /// 混输方案下改动本项会直接改变顶码行为；`top_code_override_pinyin` 可无视本否决。
    ///
    /// 它还兼管**满码空码清空**（`schema.codetable.clear_on_empty_max`）的两道拼音守护：
    /// 「已有拼音候选」与「拼音还没打完」（`is_possible_pinyin_sequence`，如 zhon→zhong）。
    /// 关闭本项 = 拼音一律不干预码表处置，满码无匹配即清空/上屏。
    #[serde(default = "default_true")]
    pub auto_commit_block_on_pinyin: bool,
    /// 满码上屏遇英文候选则否决（保护正在输入英文词的用户；仅 enable_english 开时有意义）。
    #[serde(default)]
    pub auto_commit_block_on_english: bool,
    /// 拼音最小触发长度（0=回退 2）。
    #[serde(default)]
    pub min_pinyin_length: usize,
    /// 英文最小触发长度（0=回退 3，即 2 字符以内不查英文；预留可配）。
    #[serde(default)]
    pub min_english_length: usize,
    /// 拼音歧义拦截（词强度启发式）：整串是强拼音词时否决五笔自动/顶码上屏，让拼音赢
    /// （如 wangba→网吧；aipu 无强词则放行落实）。默认开；独立于 auto_commit_block_on_pinyin。
    #[serde(default = "default_true")]
    pub block_commit_on_pinyin_word: bool,
    /// 拼音歧义拦截的词强度权重阈值（0=仅结构判据：≥2 汉字且消费整串；预留真机调）。
    #[serde(default)]
    pub pinyin_word_min_weight: i32,
    /// 混输**码长内**（输入 ≤ 主码表最大码长）是否保留「未消费整串」的拼音候选。
    /// 默认 `false`（丢弃）：`gedw`（五笔「青春」）下拼音会把 `ge` 的 219 条同音单字全交出来，
    /// 每条只解释 4 键中的 2 键。关掉后候选只剩五笔精确码，开着简拼时混合简拼词也能浮上来。
    /// 代价是码长内没有分步上屏；正在输入中的拼音（`wanl`→「完了」）不受影响。
    #[serde(default)]
    pub pinyin_partial_candidates: bool,
    /// 混输**超码长**（已切入纯拼音语境）是否保留同类候选。默认 `true`（保留）：
    /// 那里正是长拼音的地盘，`nihaom` 选「你好」再续打的分步上屏要留着。
    #[serde(default = "default_true")]
    pub pinyin_partial_candidates_overflow: bool,
    /// 混输时拼音是否产出简拼候选（声母缩写，nh→你好）。默认开=历史行为（此前恒开无开关）。
    /// 关闭后混输里的拼音只认全拼，适合「只把拼音当临时输入补位、不用简拼」的用户；
    /// 简拼会让几乎任何字母串都可能是拼音，关掉可让候选更干净。仅影响混输的拼音子引擎，
    /// 纯拼音方案不受影响。
    #[serde(default = "default_true")]
    pub enable_pinyin_abbrev: bool,
}

impl Default for MixGlobal {
    fn default() -> Self {
        Self {
            show_source_hint: false,
            enable_english: false,
            // ⚠️ 三处同源：本处 / `MixConfig::default()`（wind-engine mixed/engine.rs）/
            // `data/config.toml [schema.mix]` 必须一致，改默认须同步全部三处。
            pinyin_only_overflow: true,
            top_code_override_pinyin: false,
            auto_commit_block_on_pinyin: true,
            auto_commit_block_on_english: false,
            min_pinyin_length: 0,
            min_english_length: 0,
            block_commit_on_pinyin_word: true,
            pinyin_word_min_weight: 0,
            // ⚠️ 同属「三处同源」：本处 / `MixConfig::default()` / `data/config.toml`。
            pinyin_partial_candidates: false,
            pinyin_partial_candidates_overflow: true,
            // 出厂**关**：简拼几乎能主张任何字母串，开着时超码长归属恒判给拼音，
            // 五笔顶码上屏基本不会发生（`pinyin_only_overflow` 独立拦下，且它是隐藏项，
            // 用户把两个否决开关都关掉也无济于事）。混输用户以码表为主，默认让顶码可用；
            // 需要简拼的用户显式打开即可。详见 `data/config.toml` 同名项注释。
            enable_pinyin_abbrev: false,
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
}

/// 快捷输入的**全局**行为配置。
///
/// 各候选来源的开关与优先级**不在这里**——它们是 `mix_modes.members` 的成员
/// （`quick_input.calc` / `.date` / `.number` / `.repeat` 与 `$primary_pinyin` / `english`），
/// 开关即增删、优先级即排序。本结构只留与来源无关的全局项。
///
/// 曾有 `enable_english` 与 `members` 并存，构成双真相源（协调器两处各过滤一遍
/// english 成员）。已废弃并在加载期迁移：旧值 false → 从 quick_mix 的 members 移除 english。
/// 快捷输入的**全局**行为配置。
///
/// 没有总开关：想禁用就把 `quick_mix` 的 `trigger_keys` 清空——没有触发键自然进不去，
/// 一件事只有一种表达。（曾有 `enabled` 字段，但它从未被任何逻辑读取，关掉不产生任何效果。）
///
/// 曾有 `force_vertical`（强制竖排），但它的判定条件是「**这个 mix 实例**含 quick 成员」，
/// 属于实例的显示属性却被存在与实例无关的全局段里。已迁移到
/// [`MixModeConfig::candidate_layout`]（见 docs/design/mode-candidate-layout.md）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuickInputConfig {
    /// 计算器结果小数位数，默认 6
    #[serde(default = "default_decimal_places")]
    pub decimal_places: i32,
}

fn default_decimal_places() -> i32 {
    6
}

impl Default for QuickInputConfig {
    fn default() -> Self {
        Self {
            decimal_places: default_decimal_places(),
        }
    }
}

// ───────────────────────── 模式级显示属性（多模式共用）─────────────────────────

/// 模式级候选布局意图（设计见 docs/design/mode-candidate-layout.md）。
///
/// - `Follow`：跟随全局 `ui.candidate.layout`——用户改全局，本模式跟着改。
/// - `Vertical` / `Horizontal`：进入该模式期间覆盖全局方向，退出自动回到全局。
///
/// 刻意与 `ui.candidate.layout` 共用取值词汇（"vertical"/"horizontal"），让「模式级设置」
/// 与「全局设置」在用户眼里是同一件事的两个层级，而不是两套发明出来的开关名。
///
/// **为什么不是布尔**：`Follow` 与 `Vertical` 只在全局本身是竖排时才有区别——前者跟着
/// 全局变、后者恒定竖排。布尔（旧 `quick_input.force_vertical`）把这两种意图压成同一个
/// `true`，且表达不了「全局竖排但本模式横排」（临英候选一行放得下，竖排反而占屏）。
///
/// ⛔ **旋转 90° / 文字竖排不在本枚举里**，它们是另一根轴
/// （[`TextOrientation`]，方案级 `[candidate] text_orientation`）。
/// 曾把它们当成第三、第四种「排列」，作废理由见该类型的文档。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LayoutIntent {
    #[default]
    Follow,
    Vertical,
    Horizontal,
}

/// **横排时**文字的排列方式（方案级 `[candidate] text_orientation`）。
///
/// | 取值 | 屏幕上 | 给谁用 |
/// |---|---|---|
/// | `Normal` | 一行候选，文字水平 | 绝大多数方案 |
/// | `Rotated` | 候选成列自左向右，**整项**顺时针转 90° | 蒙古文等连写的纵向书写脚本 |
/// | `Upright` | 同上，但字不转、逐字下行 | 汉字的对联式竖排 |
///
/// 术语取自 CSS `text-orientation: sideways \| upright`，`Rotated` 对应 sideways。
///
/// # ★★ 为什么是**独立一根轴**，而不是 [`LayoutIntent`] 的第三、第四个取值
///
/// 曾按后者实现过，作废理由有三条，每条单独都足够：
///
/// 1. **注释模板只认横竖**。它在全局/方案/overlay/模式**四层**各有一对
///    `_vertical`/`_horizontal`，窗口尺寸下限有四个字段，`flip_when_above` 只对竖排成立。
///    把旋转塞进同一个枚举，这些对子要么变三元组四元组（组合级代价），要么被迫「旋转态
///    走横排那一支」——后者能跑，但它恰恰说明**旋转本来就不是一种排列**。
/// 2. **值域挤在同一个下拉里**。外观页那个控件面向所有用户，而旋转是极少数方案才用的；
///    四个平铺选项既难选，也在暗示它们是同一类东西。
/// 3. ★ **两根轴合成一根就切不开了**：蒙古文用户想在自己的方案里切横排/竖排时，
///    `ime.toggle("layout")` 只能把方向整个换掉、连旋转一起丢。拆成两根轴之后，
///    切换只动 `vertical`，`text_orientation` 是方案属性，纹丝不动。
///
/// # ⚠️ 竖排时不生效
///
/// 键名里的「横排时」是字面意思：`vertical = true` 时本轴一律按 `Normal` 处理
/// （竖排再转 90° 就是横排，没有第二种解释）。归一化只在
/// [`Orientation::normalized`] 一处做。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextOrientation {
    /// 文字水平（出厂）。
    #[default]
    Normal,
    /// 整项顺时针旋转 90°。
    Rotated,
    /// 整项旋转，但每个字逆时针扶正、逐字下行（对联式）。
    ///
    /// ⚠️ 它按**字**切单元，故会切断连写脚本（阿拉伯文、蒙古文）的字形连接；
    /// 那些脚本要的是 [`Self::Rotated`]。本项是给汉字这类等宽独立字形用的。
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
/// # 两根轴，且**真的正交**
///
/// `vertical` 是用户随时可切的呈现偏好（外观页、命令栏 `ime.toggle("layout")`、模式级覆盖）；
/// `text` 是方案声明的「这套文字怎么写」（`[candidate] text_orientation`）。
/// 切前者不动后者——这正是拆成两根轴的主要收益，见 [`TextOrientation`] 的第 3 条理由。
///
/// 唯一的耦合是**竖排时 `text` 不生效**（竖排再转 90° 就是横排）。归一化只在
/// [`Self::normalized`] 一处做，读侧一律通过 [`Self::rotated`] / [`Self::upright`] 取值，
/// 不要自己判 `!vertical && text != Normal`——那等于把归一化复制到第二处。
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

    /// 竖排时把文字排列归零。**下发 UI 前必须过一次**——渲染端的
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

    /// 解析 `ui.candidate.layout`；未知值按横排（出厂行为）。
    ///
    /// ⚠️ 本函数**只认横竖两个值**。旋转/直立不从这个键来——它们是方案属性，
    /// 混进来会让 `ime.toggle("layout")` 的写回把方案意图覆盖掉。
    pub fn from_layout_str(s: &str) -> Self {
        if s.eq_ignore_ascii_case("vertical") {
            Self::VERTICAL
        } else {
            Self::HORIZONTAL
        }
    }

    /// 回写 `ui.candidate.layout` 用的字符串——**只反映 `vertical`**。
    ///
    /// ★ 于是 `ime.toggle("layout")` 写盘时不会碰到方案声明的文字排列：
    /// 蒙古文用户切一次横竖，回来还是旋转态。
    pub fn layout_str(self) -> &'static str {
        if self.vertical {
            "vertical"
        } else {
            "horizontal"
        }
    }
}

/// 模式级注释模板覆盖（三态）。见 `crate::comment::template_for` 的决策点说明。
///
/// - **键缺失**（`None`）= 跟随全局同方向的模板（默认，零回归）
/// - **非空** = 本模式期间改用该模板
/// - **空串** = 本模式期间不显示注释
///
/// # 为什么是 `Option<String>` 而不是 `String`
///
/// 用空串表达「跟随全局」的话，「本模式不要注释」就没法表达了——而这恰恰是本功能最主要的
/// 用途（反查类模式信息太多、干扰正常输入）。三态里「缺失」与「空」必须是两件事。
///
/// # 横竖各一份，与全局同构
///
/// 字段名与 `ui.candidate.comment_template_vertical` / `_horizontal` 刻意保持一致，
/// 且**两个方向各自独立三态**——只覆盖竖排、横排仍跟随全局是合法且常见的配置。
/// 与 [`LayoutIntent`] 的取值词汇复用同一个理由：让「模式级」与「全局」在用户眼里
/// 是同一件事的两个层级，而不是两套发明出来的键名。
pub type CommentTemplateOverride = Option<String>;

/// 自由输入（字面输入）模式：让 mix 能打出 `GetTestData()` / `test_data` / `<TAB>`
/// 这类**任何 member 都无法接受**的内容。
///
/// - `Off`：完全维持既有行为（越界字符仍走「顶屏候选 + 上屏标点 + 退出」）。
/// - `Auto`（**默认**）：由缓冲内容自动推导，见 `MixLens`。
/// - `Always`：本实例恒为自由输入——用于新建一个专做字面输入的融合模式。
///
/// # 为什么没有切换键
///
/// mix 里几乎每个键都已双重占用（文本透镜：数字选词、标点顶屏；数字透镜：字母选词），
/// 挑不出一个真正空闲的可打印键；而非可打印键（Tab / 方向键 / PgUp）又都是可配置的
/// 导航键组。于是判据落在**输入内容本身**：一个字符若不在当前透镜的合法字符集内，
/// 它就不可能是编码，只能是字面内容。
///
/// # 为什么是缓冲的纯函数而非粘滞状态位
///
/// 没有进入键就没有退出键，粘滞态找不到可解释的清除时机。纯函数下退格删掉最后一个
/// 越界字符就自然回到原透镜，所见即所得。
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
/// ⚠️ **`Default` 手写而非 derive**：`free_input_takes_select_keys` 的 serde 缺省是 `true`，
/// 而 derive 出来的 `bool::default()` 是 `false`——两条路径会给出相反的默认值，测试夹具
/// （`..Default::default()`）与真实配置的行为就此分叉。新增带非零默认值的字段时，
/// **必须同时改这里**（与 `TempEnglishConfig` 手写 `Default` 同因）。
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
    /// 成员列表：**候选来源的单一真相源**——有无即开关，顺序即优先级。
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
    #[serde(default)]
    pub candidate_layout: LayoutIntent,
    /// 本 mix 期间的注释模板覆盖（竖排），见 [`CommentTemplateOverride`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_vertical: CommentTemplateOverride,
    /// 本 mix 期间的注释模板覆盖（横排），见 [`CommentTemplateOverride`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_horizontal: CommentTemplateOverride,
    /// 自由（字面）输入，见 [`FreeInputMode`]。默认 `Auto`。
    ///
    /// 每实例独立：一个只做日期/计算的 mix 可以关掉它，保住「打完拼音按逗号顶屏出
    /// 中文标点」；专做字面输入的实例则设 `Always`。
    #[serde(default)]
    pub free_input: FreeInputMode,
    /// 自由输入是否**夺取二三候选键**（`keys.select_key_groups` 的键，默认 `;` `'`）
    /// 作字面输入。默认开；`free_input = "off"` 时本项无意义。
    ///
    /// # 为什么需要它
    ///
    /// 文本透镜下「选词键」与「字面字符」是同一批物理键的两种解释，无法两全。而
    /// `rock'n'roll` / `don't` / `for(;;)` 这类内容里的 `'` `;` 恰好就是默认选词键：
    /// 不夺取的话它们在第④步就被 `select_key_offset` 吃掉，根本走不到第⑤步的字面输入
    /// （实测 `;rock` 按 `'` 会选走第 3 候选「日欧」并触发分步确认，输入被打散）。
    ///
    /// 夺取的代价是**零能力损失**：`;`/`'` 选第 2/3 候选只是数字键 `2`/`3` 的冗余别名，
    /// 数字键仍在。**数字键 1-9 刻意不在夺取范围内**——它们是文本透镜唯一的选词通路，
    /// 让位就没有选词键了。代价是 `utf8` / `mp3` / `x64` 这类「纯小写字母 + 数字」仍需
    /// 先打一个大写字母或符号切进自由输入。
    ///
    /// # 为什么是独立开关而非跟随 `free_input`
    ///
    /// 翻页键（`-`/`=`）的让位是跟着 `free_input` 走的，本项刻意不对称：翻页有
    /// PageUp/PageDown 作等价替代，让位是纯收益；而选词键的取舍因人而异——习惯用
    /// `;`/`'` 选词的用户可以单独关掉本项，保住手感，同时仍享有大写字母与其它符号
    /// 触发的自由输入。
    #[serde(default = "default_true")]
    pub free_input_takes_select_keys: bool,
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
            comment_template_vertical: None,
            comment_template_horizontal: None,
            free_input: FreeInputMode::default(),
            // 与 `#[serde(default = "default_true")]` 对齐，见结构体文档的 ⚠️。
            free_input_takes_select_keys: default_true(),
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
        // ★ 出厂引导键**刻意仍放在这里**（而不是 data/config.toml 的 keys.key_actions）。
        //
        // 收编后它由 `normalize` 折算进 `keys.key_actions`。默认值必须留在**被折算的
        // 那一侧**，折算结果才能如实反映用户意图：没配过→折算出默认；改成别的键→
        // 折算出新值；**清空→折算出空**。若把默认值直接写进 key_actions，第三种就废了
        // ——合并后 `trigger_keys = []` 与「从没配过」同形，折算跳过、默认绑定仍在，
        // 用户清空了个寂寞。见 docs/design/schema-key-actions.md 五c。
        trigger_keys: vec!["semicolon".to_string()],
        members,
        // 出厂强制竖排：快捷输入的候选是日期/算式结果等长文本，横排放不下。
        // 与旧 data/config.toml 的 `quick_input.force_vertical = true` 行为一致
        // （mix_modes 不能写进预置文件，故默认值只能落在这里，见 §迁移）。
        candidate_layout: LayoutIntent::Vertical,
        // None = 跟随全局注释模板（内置 quick_mix 不预设覆盖）
        comment_template_vertical: None,
        comment_template_horizontal: None,
        // 出厂开自动自由输入：`;` 是为特殊内容而进的模式，字面输入是它的常见用途。
        free_input: FreeInputMode::Auto,
        // 夺取 `;`/`'` 作字面：它们选第 2/3 候选只是数字键的冗余别名，而 `rock'n'roll`
        // 这类内容里的 `'` 没有别的输入通路。
        free_input_takes_select_keys: true,
    }]
}

// ⛔ `SpecialModeConfig` 已删除。特殊模式的实例集合改由「带 `[overlay]` 段的已安装方案」
// 定义（`EngineManager::overlay_modes`）：呈现配置落方案文件的 `[overlay]` 段，引导键与
// 直达热键落 `keys.key_actions`（`special:<方案id>`）。
// 见 `docs/redesign/overlay-mode-config.md`。残留旧配置的告警见 `warn_legacy_special_modes`。
// ───────────────────────── input（输入行为）─────────────────────────

/// 「检索范围」智能档的放宽增强（设计见 `docs/design/smart-filter-scope-relax.md`）。
///
/// 智能档会滤掉同码位有常用字的生僻字，代价是**唯一编码被占的生僻字彻底打不出**（如五笔
/// 「桜」sivg 与常用「档」同码），用户只能整体切「全部字符」并常忘记切回。
///
/// 出路只有**一条**：候选窗内按向后翻页键翻到底，再按一次即临时放宽。刻意不做任何自动
/// 行为——曾实现过「候选不足一页自动补充」，实测平白改变了智能档的既有观感，已删除。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeRelaxConfig {
    /// 末页再按向后翻页键即临时放宽为「全部字符」，本次组合结束后自动恢复。
    ///
    /// 三类引擎通用且唯一的入口——用户找生僻字本就会一路翻页，翻到底即是明确的放宽意图。
    /// 候选**不足一页**时同样适用：那时只有一页，按翻页键一样翻不动，落到同一条路径。
    #[serde(default = "default_true")]
    pub page_end_key: bool,
    /// 放宽放出来的候选的前缀标注（空=不标注），用于与正常候选区分。
    #[serde(default = "default_scope_relax_prefix")]
    pub prefix: String,
}

impl Default for ScopeRelaxConfig {
    fn default() -> Self {
        Self {
            page_end_key: true,
            prefix: default_scope_relax_prefix(),
        }
    }
}

fn default_scope_relax_prefix() -> String {
    "·".to_string()
}

/// 联想（`[input.association]`）：上屏之后按**上文**推荐下一个词或标点。
///
/// 与普通候选的根本差别在输入源——普通候选的输入是编码缓冲，联想的输入是刚上屏的文本。
/// 候选生成在 `wind-assoc`，何时展示/退出在协调器 `handle_assoc.rs`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssociationConfig {
    /// 哪一种联想：`"off"`（默认）/ `"word"` / `"smart"`。
    ///
    /// - `word` **词语联想**：上文当**前缀**，出词库里以它开头的更长的词。打完「中」给
    ///   「中国」「中间」，选中只补出「国」「间」。PC 输入法说的「联想」就是这个，
    ///   **不含标点**。
    /// - `smart` **智能联想**：上文当**上下文**，出下一个可能的词与标点。移动端刚需
    ///   ——软键盘上每多打一个字都很贵。
    ///
    /// ★ **开关与类型合并成这一个字段**。拆成 `enabled` + `kind` 会立刻产生「开着但类型
    /// 没配」的歧义状态，而那个状态没有正确答案。
    ///
    /// ⚠️ **本段是桌面基线，移动端的差异走 [`MobileAssociationConfig`]**，不要在这里加
    /// `"auto"` 之类的平台哨兵——那会把平台知识塞进值域，设置界面被迫列一个语义空洞的
    /// 选项，用户看不出选了会得到什么。
    #[serde(default = "default_assoc_kind")]
    pub kind: String,
    /// `"one_shot"`（默认）/ `"continuous"`。
    ///
    /// `one_shot` = 只出一次，任何非选词动作即退出；`continuous` = 选中之后拿它当新上文
    /// 接着给。桌面默认一次性：候选窗是浮层，常驻会挡住正文。
    #[serde(default = "default_assoc_mode")]
    pub mode: String,
    /// 候选总数上限。各源的配额由它按优先级分配，**配额本身不开放给用户**——那是调参项，
    /// 而用户没有评测手段。
    #[serde(default = "default_assoc_max_count")]
    pub max_count: usize,
    /// 空格是否上屏当前高亮的联想候选。
    ///
    /// 主流输入法多数如此，故默认开。关掉则空格照常出空格（联想窗同时收起）。
    /// 这一项没有「更对」的答案——它取决于用户把联想当「顺手就选」还是「别挡我打字」。
    #[serde(default = "default_true")]
    pub space_commits: bool,
    /// 联想态按**回车**：只收窗（吃键），还是收窗 + 把回车**透传**给宿主。
    ///
    /// `false`（默认）= 透传：联想窗收起，同时回车照常换行 / 发送消息。
    /// `true` = 仅取消联想：回车被吃掉，要按第二次才生效。
    ///
    /// 默认透传，是因为回车是**终结性动作**——用户按它是要发送或换行，而联想窗是输入法
    /// 自己弹出来的、用户并没有在选词。让它吞掉一次回车，等于让一个「建议」挡住了正事。
    #[serde(default)]
    pub enter_cancels_only: bool,
    /// 联想态按**退格**：只收窗（吃键），还是收窗 + 把退格**透传**给宿主。
    ///
    /// 取值语义与 [`Self::enter_cancels_only`] 完全对称，但**默认相反**（`true` = 仅取消
    /// 联想，保持吃键）。
    ///
    /// 两者默认值相反是刻意的（2026-08-20 用户拍板）：回车的透传是「把正事办了」，而退格
    /// 的透传是**删掉刚上屏的字**——一个不可逆的破坏性动作。联想窗弹出时用户的手正停在
    /// 刚打完的字上，误触退格若直接删字，比多按一次键的代价大得多。要这个行为的人可以开。
    #[serde(default = "default_true")]
    pub backspace_cancels_only: bool,
    /// 联想窗自动隐藏的毫秒数；`0` = 不自动隐藏。
    ///
    /// 联想态下候选窗几乎一直挂着，长时间停留会挡住正文。主流输入法的做法是显示几秒后
    /// 自行淡出，用户若要用就在这几秒内按数字。
    #[serde(default = "default_assoc_hide_after_ms")]
    pub hide_after_ms: u64,
    /// 联想态在**编码栏**显示的标识；空串 = 不显示。
    ///
    /// 只在「编码不嵌入宿主」（候选窗自绘编码栏）时可见——嵌入模式下编码栏本身不存在。
    /// 它回答的是「候选窗为什么还开着、这批候选是哪来的」：联想候选与普通候选长得一样，
    /// 没有标识时用户分不清自己是不是还在打字。
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
    /// ⚠️ **桌面默认关**（移动端在 [`MobileAssociationConfig::punct`] 里开）。桌面上打完
    /// 一个字就弹一串标点，干扰远大于收益——标点在实体键盘上本来就一键可达，而候选窗是
    /// 浮层、还占着数字键。移动端相反：软键盘上打标点要切键盘层，从候选里点走省事得多。
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

/// `[mobile]`：**移动端对桌面基线的覆盖**。桌面构建完全无视本域。
///
/// # 为什么单独开一个顶层域
///
/// 少数配置项的最优值在两个平台上就是不同的（联想是第一例：PC 上候选窗是浮层、会占用
/// 数字键，移动端软键盘本就常驻、每多打一个字都很贵）。表达这种差异有三条路，前两条都
/// 试过并被否决：
///
/// 1. ⛔ **值域哨兵**（`kind = "auto"` 由宿主按 `cfg!(target_os)` 解释）——把平台知识塞进
///    了值里。后果是设置界面被迫列一个语义空洞的「自动」选项，用户看不出选了会得到什么
///    （2026-08-16 用户否决）。
/// 2. ⛔ **两份预置文件各写各的**——`data/config.toml` 与安卓仓 `assets/data/config.toml`
///    确实是两份，但后者是**手工副本且无守门测试**（实测已滞后 89 行）。把平台差异寄托在
///    「两份文件恰好不一样」上，下次同步就被无脑覆盖回去了。
/// 3. ✅ **本域**：一份文件同时说清两个平台，差异是显式的、可读的、同步不会丢。
///
/// # 只登记真有平台差异的键
///
/// 本域里出现的键，移动端就用这里的值；**没出现的键沿用基线**。所以「移动端与基线相同」
/// 的键根本不该被登记进来——每登记一个都要在 REGISTRY、预置文件、capability 快照、设置页
/// 豁免名单里各占一行，改基线时还得记得改另一边。
///
/// ⚠️ 字段用 `String` 而不是 `Option<String>`：`Option` 看着更贴「覆盖」的字面意思，但
/// `None` + `skip_serializing_if` 会让本段在默认配置里序列化成**空表**，于是
/// `mobile.association` 自己成了叶子键，三道注册表守门测试全都要为它开特例
/// （2026-08-16 实测撞上）。而那份「稀疏」能力本就是多余的——见上一段。
///
/// ⚠️ 覆盖**不在 [`Config::load`] 里合并**，而是在消费点由宿主决定是否取用
/// （协调器 `handle_assoc.rs`）。理由：合并进 `input.*` 之后，移动端设置页读到的是合并
/// 后的值，一保存就把移动端的值写进了桌面基线。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MobileConfig {
    #[serde(default)]
    pub association: MobileAssociationConfig,
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
    /// 移动端**开**标点联想：软键盘上打标点要切键盘层，从候选里点走省事得多。
    /// 桌面相反（见 [`AssociationConfig::punct`]）——实体键盘上标点一键可达。
    #[serde(default = "default_true")]
    pub punct: bool,
}

impl Default for MobileAssociationConfig {
    fn default() -> Self {
        Self {
            kind: default_mobile_assoc_kind(),
            mode: default_mobile_assoc_mode(),
            punct: true,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    #[serde(default = "default_filter_mode")]
    pub filter_mode: String,
    /// 检索范围放宽（智能档增强）。
    #[serde(default)]
    pub scope_relax: ScopeRelaxConfig,
    #[serde(default = "default_enter_behavior")]
    pub enter_behavior: String,
    #[serde(default = "default_space_behavior")]
    pub space_on_empty_behavior: String,
    /// 空码（缓冲非空但无候选）时按标点/符号键的处理方式。**三态**：
    ///
    /// - `commit` 上屏原码再接标点；
    /// - `clear`（出厂）丢弃原码，标点照常上屏；
    /// - `clear_no_input` 丢弃原码，标点本身也不上屏——整个按键当没按过。
    ///
    /// ★ 与 [`Self::enter_behavior`] / [`Self::space_on_empty_behavior`] 同族但**值域多一态**。
    /// 这一族描述的行为其实有两根轴：「废码上不上屏」与「键字符本身出不出」。回车/空格的
    /// `clear` 落在**吞键**那一格（返回 `ClearComposition`），标点的 `clear` 落在**出键**那一
    /// 格——同一字面值在第二根轴上取值相反，是刻意的（标点是用户真想输入的可见字符）。
    /// `clear_no_input` 补的是标点缺的那一格。值域清单在
    /// `config_schema::PUNCT_EMPTY_CODE_BEHAVIOR_VALUES`，唯一解释器是
    /// `Coordinator::punct_empty_code_policy`。
    ///
    /// **不要与 `schema.codetable.punct_commit` 混淆**：那一项关掉是「吞键、**保留**编码」，
    /// 编码留在组合区继续输入；`clear_no_input` 是「吞键、**丢弃**编码」。
    #[serde(default = "default_punct_on_empty_behavior")]
    pub punct_on_empty_behavior: String,
    #[serde(default = "default_numpad_behavior")]
    pub numpad_behavior: String,
    /// 启动默认状态（记住上次状态 / 默认中文 / 全角 / 中文标点；原 general 域）。
    #[serde(default)]
    pub default: InputDefaultConfig,
    /// 标点相关（随中英、智能标点、自定义映射）。
    #[serde(default)]
    pub punct: PunctConfig,
    /// 智能符号模式。
    #[serde(default)]
    pub symbol: SymbolConfig,
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
    /// 网址输入模式。
    #[serde(default)]
    pub url: UrlConfig,
    /// 快捷加词面板（目前只有候选布局一项；进入方式是 keys.add_word 热键）。
    #[serde(default)]
    pub add_word: AddWordConfig,
    /// 简繁转换（上屏文字变换）。原 features.s2t。
    #[serde(default)]
    pub s2t: S2TConfig,
    /// 命令栏（$CC/$SS/$AA 等命令候选）。原 features.cmdbar。
    #[serde(default)]
    pub cmdbar: CmdbarConfig,
    /// 短语前缀列举（含命令栏 $CC/$SS/$AA）。原 dict.phrase / Go input.phrase。
    #[serde(default)]
    pub phrase: PhraseConfig,
    /// 顶码上屏策略（内部/实验，默认 direct_commit 真提交时序，躲开 diff 合并与整段下划线）。
    #[serde(default)]
    pub top_commit_mode: TopCommitMode,
    /// 联想（上屏后按上文推荐下一个词/标点）。默认关。
    #[serde(default)]
    pub association: AssociationConfig,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            filter_mode: "smart".to_string(),
            scope_relax: ScopeRelaxConfig::default(),
            enter_behavior: "commit".to_string(),
            space_on_empty_behavior: "commit".to_string(),
            punct_on_empty_behavior: default_punct_on_empty_behavior(),
            numpad_behavior: default_numpad_behavior(),
            default: InputDefaultConfig::default(),
            punct: PunctConfig::default(),
            symbol: SymbolConfig::default(),
            auto_pair: AutoPairConfig::default(),
            temp_english: TempEnglishConfig::default(),
            capslock: CapslockConfig::default(),
            temp_pinyin: TempPinyinConfig::default(),
            url: UrlConfig::default(),
            add_word: AddWordConfig::default(),
            s2t: S2TConfig::default(),
            cmdbar: CmdbarConfig::default(),
            phrase: PhraseConfig::default(),
            top_commit_mode: TopCommitMode::default(),
            association: AssociationConfig::default(),
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
            custom_enabled: false,
            custom_mappings: HashMap::new(),
        }
    }
}

/// 智能符号替换方案。两者是「体感」与「兼容性」的取舍，故都保留：
/// - `DeleteReplace`（**默认**）：press1 直接提交中文符号，press2 删掉重打成英文。
///   所见即所得、无预览态中间状态，实际体感更好；代价是依赖对宿主做删改
///   （早期的 Office 500ms 重复、SendInput 自重入、prevChar 读不到致完全不触发
///   三处已修）。
/// - `HoldComposition`：press1 开启 TSF 组合态展示中文符号，press2 替换组合提交英文；
///   超时（smart_timeout_ms）后自动提交中文。全程不做删改，**兼容性更好**，
///   适合对删改敏感、DeleteReplace 下表现异常的宿主。
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
/// 三个总开关**互相独立**，各管一种上下文，都默认关：
///   - `smart_mode`：中文标点状态 —— press1 中文 → press2 英文（数字后智能标点方向相反）。
///   - `english_punct_mode`：中文输入 + 英文标点状态 —— press1 英文 → press2 中文。
///   - `english_mode`：英文输入模式 —— 同上，但发生在整个输入法切英文时。
///
/// 后两者拆成两个开关而非一个，是因为它们是**不同场景**：前者是「用英文标点写中文、偶尔要个
/// 中文句号」，后者是「正在打英文」。很多人只想要前者，英文态保持纯净。
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
    #[serde(default)]
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
    /// 参与英文智能符号的**源字符**集合（`english_punct_mode` 与 `english_mode` 共用）。
    ///
    /// 与 `smart_chars` 存中文产物不同，这里存的是**键本身的 ASCII 标点**（`.` 而非 `。`）：
    /// 英文侧的产物通常就等于源字符，而推给 DLL 的吃键集必须是源字符——按源字符判定，
    /// 两边同源、无需从产物反推。
    ///
    /// **不建议放配对符**（`([{"'` 等）：英文模式下这些键被吃走后，配对改由 core 处理，而
    /// DLL 的跳出栈是空的，Tab 跳出会失效（见 `handle_english_custom_punct` 的已知限制）。
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
    /// 取值为键名（`"tab"`/`"enter"`/`"space"`/`"escape"`），外加一个特殊值
    /// **`"right_symbol"` = 右符号键本身**（打 `）` 跳出已插入的 `（）`）。右符号跳出曾是
    /// 无条件行为，现收敛为本列表的一项——**列表里没有它就是没有，不做隐式补偿**，故旧配置
    /// 若只写了 `["tab"]`，右符号跳出即关闭（用户可在设置界面重新勾选）。
    ///
    /// 对称配对（引号）**永不参与右符号跳出**，与本项无关：按键不携带「开/闭」这一位，
    /// 无从判断跳出还是嵌套，故一律开新的一对（见 `pin_quote_left_if_paired`）。
    #[serde(default = "default_jump_out_keys")]
    pub jump_out_keys: Vec<String>,
    /// 配对状态时效，单位秒（内部项，设置界面不暴露）。`0` = 不过期。
    ///
    /// 管的是**同一个输入框内**的状态陈旧：用户中途用鼠标点过别处、滚过页、把括号退格删掉
    /// ——这些输入法都感知不到，没有时效的话陈旧状态会一直存活到吃掉用户的 Tab。
    /// 距**最后一次按键**超过本值即视为陈旧，跳出键不再生效；从最后一次按键算起而非从插入
    /// 配对算起，因此持续输入会不断刷新，在括号里打多久都不会误过期。
    ///
    /// 跨焦点的陈旧不归它管——失焦一律清空配对状态（见 `handle_focus_lost`）。
    #[serde(default = "default_pair_state_ttl_secs")]
    pub state_ttl_secs: u32,
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
/// 含 `.` 与 `-` 是刻意的（`obj.prop` / `e-mail` / `snake_case` 都要打得出），代价是这
/// 两个键在临英下交出各自的翻页职责——`comma_period` / `minus_equal` 两个键组各被劈掉
/// 一半，「上一页」只剩 ↑ 与 PgUp；「打完英文顺手按句号上屏」这条通路也随之失效。
/// 不需要代码场景的人把这两个字符从列表里删掉即可拿回。
fn default_temp_english_symbol_chars() -> String {
    "0123456789+-_.@#/".to_string()
}

/// 临时英文配置（[input.temp_english]，原 input.shift_temp_english）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempEnglishConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 显示英文候选（原 show_english_candidates）。
    #[serde(default = "default_true")]
    pub show_candidates: bool,
    #[serde(default = "default_shift_behavior")]
    pub shift_behavior: String,
    /// 触发键（符号键进入临时英文模式，类似临时拼音触发键）。默认空（仅 Shift+字母触发）。
    #[serde(default)]
    pub trigger_keys: Vec<String>,
    /// 允许符号与数字直接入缓冲（`C++` / `hello2` / `x64`）而非触发上屏或选词。
    /// 总开关，放行哪些字符由 [`Self::symbol_chars`] 精确决定。
    #[serde(default)]
    pub allow_symbols: bool,
    /// 允许入缓冲的符号/数字白名单（**纯字面字符集**，逐字符匹配）。仅 `allow_symbols`
    /// 开启时生效；**列表外的字符一律维持关闭时的语义**——符号仍「上屏高亮候选 +
    /// 转换后标点 → 退出临英」，`;`/`'` 仍选第 2/3 候选，`-=[],.` 仍翻页，数字键 1-9
    /// 仍选词。即：白名单只把选中的字符从这套语义里摘出来改成入缓冲。
    ///
    /// 留空 = 一个字符都不放行（等价于关掉 `allow_symbols`）。
    ///
    /// ★ 刻意**不**复用码元集 `input_chars` 的 `a-z` 范围语法：`-` 在符号集里是高频
    /// 字符（`e-mail`），那套语法下 `+-_` 会被解析成 `0x2B..=0x5F` 一整片（含全部大写
    /// 字母与 `:;<=>?@[\]^`），用户在设置页填一个减号就静默放行几十个字符。与同类的
    /// `symbol.english_chars` / `symbol.smart_chars` 一致，字面即全部真相。
    #[serde(default = "default_temp_english_symbol_chars")]
    pub symbol_chars: String,
    /// 空格作为输入字符入缓冲（可打出带空格的英文短句）。上屏职责随之转给回车，
    /// 且回车此时上屏**高亮候选**而非原文——否则该配置下没有任何选词键可用。
    #[serde(default)]
    pub space_as_input: bool,
    /// 进入临时英文期间的候选布局（默认跟随全局）。
    /// 典型用法是设 `horizontal`——英文候选一行放得下，全局竖排时反而占屏。
    #[serde(default)]
    pub candidate_layout: LayoutIntent,
    /// 临英期间的注释模板覆盖（竖排），见 [`CommentTemplateOverride`]。
    /// 典型用法 `"${dict}"`——只在打英文时显示挂载的英汉释义，中文输入不受影响。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_vertical: CommentTemplateOverride,
    /// 临英期间的注释模板覆盖（横排），见 [`CommentTemplateOverride`]。
    /// 临英常设 `candidate_layout = "horizontal"`，此时生效的是本项而非竖排那份。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_horizontal: CommentTemplateOverride,
    /// 首候选是用户所打原文（保证能上屏自己输入的内容）。
    ///
    /// **默认开 = 保持既有行为**：此前这条是硬编码、不可配的。开成配置项是因为「打英文时
    /// 总是走词库补全」也是一种合理偏好——原文占掉首位，常用词就永远在 2 号键。
    ///
    /// ⚠️ 与 [`Self::case_variants`] **同时关闭**且词库无命中时，候选列表会是空的。
    /// 那不是缺陷：临英空格臂的判据是「实际候选是否为空」而非本配置，空候选会正确落到
    /// 「上屏缓冲原文」的兜底分支。见 `handle_temp.rs` 空格臂。
    #[serde(default = "default_true")]
    pub raw_candidate: bool,
    /// 生成大小写变形候选（全小写 / 首字母大写 / 全大写）。
    ///
    /// 关掉后候选只剩输入原文 + 词库匹配。变形候选的代价是**每条都占一个候选位**：
    /// 每页 5 条时它们能吃掉一半，把真正的词库候选挤到下一页；且它们与词库候选交错，
    /// 注释、词频这些附加信息在变形项上都是空的，列表看起来参差。
    /// 需要大小写变换的人默认开着，只想要词库补全的人可以关掉。
    #[serde(default = "default_true")]
    pub case_variants: bool,
}

impl Default for TempEnglishConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_candidates: true,
            shift_behavior: "temp_english".to_string(),
            trigger_keys: Vec::new(),
            allow_symbols: false,
            symbol_chars: default_temp_english_symbol_chars(),
            space_as_input: false,
            candidate_layout: LayoutIntent::default(),
            raw_candidate: true,
            case_variants: true,
            comment_template_vertical: None,
            comment_template_horizontal: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapslockConfig {
    #[serde(default)]
    pub cancel_on_mode_switch: bool,
}

/// 临时拼音配置（[input.temp_pinyin]）。码表方案下临时切到拼音反查。全局唯一。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TempPinyinConfig {
    /// 总开关（原方案级 [engine.codetable.temp_pinyin].enabled 上移至此）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 触发键（如 "backtick" / "semicolon"），默认反引号。**只认符号键**——字母触发键
    /// 已迁往方案级 `schema.codetable.z_key_action`（见 `migrate_letter_trigger_keys`）。
    #[serde(default = "default_temp_pinyin_triggers")]
    pub trigger_keys: Vec<String>,
    /// 专用直达热键（如 "ctrl+shift+p"，空串=不注册）。与 `trigger_keys` 引导键共存；
    /// 热键进入时组合区不写引导符（见 docs/design/special-mode-entry-hotkey.md）。
    #[serde(default)]
    pub hotkey: String,
    /// 进入临时拼音期间的候选布局（默认跟随全局）。
    #[serde(default)]
    pub candidate_layout: LayoutIntent,
    /// 临拼期间的注释模板覆盖（竖排），见 [`CommentTemplateOverride`]。
    /// 反查场景的典型用法是设 `"${code}"` 只留编码，或设 `""` 什么都不显示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_vertical: CommentTemplateOverride,
    /// 临拼期间的注释模板覆盖（横排），见 [`CommentTemplateOverride`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_horizontal: CommentTemplateOverride,
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
            comment_template_vertical: None,
            comment_template_horizontal: None,
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
    /// 进入网址模式期间的候选布局（默认跟随全局）。
    #[serde(default)]
    pub candidate_layout: LayoutIntent,
    /// 网址模式期间的注释模板覆盖（竖排），见 [`CommentTemplateOverride`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_vertical: CommentTemplateOverride,
    /// 网址模式期间的注释模板覆盖（横排），见 [`CommentTemplateOverride`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_template_horizontal: CommentTemplateOverride,
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
            candidate_layout: LayoutIntent::default(),
            comment_template_vertical: None,
            comment_template_horizontal: None,
        }
    }
}

/// 快捷加词配置（[input.add_word]）。加词面板是覆盖在任意输入态之上的临时面板，
/// 故其布局意图优先于底层模式（见 `Coordinator::layout_intent`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddWordConfig {
    /// 加词面板期间的候选布局。默认竖排——逐字确认的面板竖排更易读。
    /// 此前是**无条件硬编码**强制竖排、连开关都没有；本项只是给它一个出口，
    /// 默认值保持原行为不变。
    #[serde(default = "default_add_word_layout")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmdbarConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 副作用命令候选（含 ActionEffect）在候选框渲染时的前缀标注（对齐 Go,默认 "⚡"）。
    #[serde(default = "default_candidate_prefix")]
    pub candidate_prefix: String,
}

fn default_candidate_prefix() -> String {
    "⚡".to_string()
}

impl Default for CmdbarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            candidate_prefix: default_candidate_prefix(),
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
    #[serde(default = "default_add_word")]
    pub add_word: String,
    #[serde(default = "default_open_add_word_dialog")]
    pub open_add_word_dialog: String,
    #[serde(default = "default_toggle_s2t")]
    pub toggle_s2t: String,
    #[serde(default = "default_activate_ime")]
    pub activate_ime: String,
    #[serde(default = "default_pin_candidate")]
    pub pin_candidate: String,
    #[serde(default = "default_delete_candidate")]
    pub delete_candidate: String,
    #[serde(default = "default_take_screenshot")]
    pub take_screenshot: String,
    #[serde(default)]
    pub global_hotkeys: Vec<String>,
    /// **已废弃且不再生效**：方案直达热键已并入 [`Self::key_actions`]，动词
    /// `switch_schema:<方案id>`。**不做自动迁移**，只在加载期告警一次
    /// （[`Config::warn_legacy_schema_hotkeys`]）。
    ///
    /// 字段保留只为**读得出残留值以便告警**——删掉的话 serde 会静默丢弃这一段，用户
    /// 的热键失效且查不到原因。处置与 `schema.legacy_special_modes` 同构：`rename` 保住
    /// TOML/JSON 里的原键名，`skip_serializing_if` 让它不再被写出，于是它也自动退出
    /// `config_schema` 的登记表对应关系（那道守卫按序列化产物比对）。
    ///
    /// ⚠️ **不要**在新代码里读它。告警之后本表恒为空；消费端一律读 `key_actions`。
    #[serde(
        rename = "schema_hotkeys",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub legacy_schema_hotkeys: HashMap<String, String>,
    /// **按键功能表**：热键串 → 动词（如 `{ "ctrl+shift+n" = "toggle_schema:english" }`）。
    ///
    /// 与上面那批「一个功能一个字段」的热键不同，这是「键 → 干什么」的通用表——同一套
    /// 动词值域将来也用于方案级 `[key_actions]`（见 docs/design/schema-key-actions.md）。
    /// 当前只接 `toggle_schema:<id>`，其余动词随后续阶段接入。
    ///
    /// 用 `BTreeMap` 而非 `HashMap`：编译成热键条目时遍历顺序即冲突时的胜者顺序，
    /// `HashMap` 会让同一份配置在不同进程里表现不同（`schema_hotkeys` 为此要显式排序）。
    #[serde(default)]
    pub key_actions: BTreeMap<String, String>,
    /// 引导键**物化**迁移的版本号（0 = 未物化）。见 [`Config::materialize_key_actions`]。
    ///
    /// 置位后 [`Config::migrate_trigger_keys_into_key_actions`] 不再折算——五处
    /// `trigger_keys` 从「每次加载都灌一遍的真相源」降级为**出厂声明处**（只供设置页
    /// 「恢复默认」读），[`Self::key_actions`] 成为唯一真相源。
    ///
    /// ⚠️ **必须住在用户层 config.toml 里，不能改成独立标记文件**：标记要能跟着
    /// 备份/还原走。否则 A 机删掉的绑定，在 B 机还原后会因「看起来没迁移过」被重新
    /// 折算灌回去——那正是本次要修的报障现场。
    ///
    /// 用版本号而非 bool：日后若要再物化一批键，递增即可重跑，bool 没有第二次机会。
    /// `skip_serializing_if` 让它默认时不出现在序列化产物里，于是自动退出
    /// `config_schema` 的登记表对应关系（同 [`Self::legacy_schema_hotkeys`] 的处置）——
    /// 它是迁移账本，不是用户可配项。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub key_actions_materialized: u32,
    /// **会话态按键功能表**：键名 → 动词（如 `{ tab = "page_next", capslock = "page_prev" }`）。
    ///
    /// 与 [`Self::key_actions`] 同构的姊妹表，分野是触发态：那张管无会话态，本表管
    /// 「正在组合一段输入」时。值域见 [`SessionAction`]，键名解析见
    /// `wind_keys::keymap::session_key_name_to_vk`（认功能键、修饰键与符号键，支持
    /// `shift+tab` 这样的单修饰前缀）。
    ///
    /// **`page_keys` / `highlight_keys` 在 `normalize()` 里折算进本表**（组名 → 具体键），
    /// 用户显式写在这里的键优先、不被折算覆盖。⚠️ 默认值刻意**留在那两个字段**一侧：
    /// 若把出厂绑定直接写进本表，`page_keys = []`（用户清空）就与「从没配过」同形，
    /// 折算跳过而默认绑定仍在 —— 用户清空的意图会静默丢失。这是五c 折算 `trigger_keys`
    /// 时用血换来的教训，见 docs/design/session-key-actions.md §6。
    ///
    /// 用 `BTreeMap` 而非 `HashMap`：理由同 `key_actions`——遍历顺序即冲突时的胜者顺序，
    /// `HashMap` 会让同一份配置在不同进程里表现不同。
    #[serde(default)]
    pub session_actions: BTreeMap<String, String>,
    // ── 选择/导航键（原 input.*）──
    #[serde(default = "default_select_key_groups")]
    pub select_key_groups: Vec<String>,
    /// 翻页键组。**消费点已改为 [`Self::session_actions`]**，本字段是折算来源与
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

/// `KeysConfig::key_actions_materialized` 的 `skip_serializing_if`。
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

// 热键默认值对齐 Go 版 DefaultConfig.Hotkeys（wind_input/pkg/config/config.go）。
// 关键：config.getDefaults 以 Config::default() 为 L1 基线（再叠 data/config.toml），
// [keys] 整表在 L2 缺失时用 Default::default()，故必须手写 Default（而非 derive 的
// 空值），否则设置页"开关后默认键丢失"。
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
fn default_take_screenshot() -> String {
    "ctrl+shift+f11".to_string()
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
fn default_select_key_groups() -> Vec<String> {
    vec!["semicolon_quote".to_string()]
}
fn default_page_keys() -> Vec<String> {
    vec!["pageupdown".to_string(), "minus_equal".to_string()]
}
fn default_highlight_keys() -> Vec<String> {
    vec!["arrows".to_string(), "tab".to_string()]
}

/// 翻页键**组名** → 折算出的 (键名, 动词) 对。组名值域与旧 `NavKeys::from_config` 一一对应。
///
/// ⚠️ 键名必须与 `wind_keys::keymap::session_key_name_to_vk` 认的规范名逐字一致。这是一条
/// **无编译期约束**的跨 crate 拼写契约：写错了这里的键会解析不出 VK，表现为「升级后翻页键
/// 全没了」而无任何报错。`config.rs` 的单测 `nav_group_names_resolve` 守这条。
fn page_key_group_binds(group: &str) -> &'static [(&'static str, SessionAction)] {
    match group.trim().to_lowercase().as_str() {
        "pageupdown" => &[
            ("pageup", SessionAction::PagePrev),
            ("pagedown", SessionAction::PageNext),
        ],
        "minus_equal" => &[
            ("minus", SessionAction::PagePrev),
            ("equal", SessionAction::PageNext),
        ],
        "brackets" => &[
            ("lbracket", SessionAction::PagePrev),
            ("rbracket", SessionAction::PageNext),
        ],
        "comma_period" => &[
            ("comma", SessionAction::PagePrev),
            ("period", SessionAction::PageNext),
        ],
        "shift_tab" => &[
            ("shift+tab", SessionAction::PagePrev),
            ("tab", SessionAction::PageNext),
        ],
        _ => &[],
    }
}

/// 高亮移动键**组名** → 折算出的 (键名, 动词) 对。约束同 [`page_key_group_binds`]。
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

/// 二三候选键**组名** → 折算出的 (键名, 动词) 对。约束同 [`page_key_group_binds`]。
///
/// 组内第一个键选**次选**（第 2 个候选）、第二个选**三选**——这是 `select_key_vks` 的
/// 位置语义（`pos + 1`），折算成显式序号后就不再依赖数组下标。
///
/// `lrshift` / `lrctrl` 展开成纯修饰键名：它们走 keyup 轻敲通路，而 `session_key_name_to_vk`
/// 与 `hotkey::session_key_to_vk` 都认这四个名字。
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

/// 以词定字键**组名** → 折算出的 (键名, 动词) 对。约束同 [`page_key_group_binds`]。
///
/// ⚠️ 值域与 [`select_key_group_binds`] **不同**：这里有 `brackets`、没有修饰键组。
/// 两者曾被张冠李戴过一次（用选词键组的解析器去解以词定字配置，`brackets` 静默失效），
/// 所以分成两张表而不是一张带参数的。
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
    /// **有效**的会话态按键表：四组键组配置的展开结果 ⊕ `session_actions`（后者优先）。
    ///
    /// # ★ 这是消费层的视图，不是存储层的改写
    ///
    /// 四组键组（`page_keys` / `highlight_keys` / `select_key_groups` / `select_char_keys`）
    /// 与 `session_actions` 在配置文件里**各自保持原样**，只在这里合并成运行时的单一真相。
    ///
    /// 曾经的做法是在 `normalize()` 里折算并 `clear()` 掉四个原字段，后果是**存储层被视图
    /// 吃掉**（2026-08-11 用户报「感觉有些乱」时查实）：
    ///
    /// - 设置页读 `config.get` → `Config::load` → `normalize`，四项恒为空 ⇒ 出厂默认
    ///   （`page_keys` 等三项非空）在界面上全显示为未勾选，**每个用户都会遇到**。
    /// - 用户勾选后保存，重开设置页又变空，像是没保存。
    /// - 在自定义表里删掉一条折算来的绑定，下次启动又被折算回来，**删不掉**。
    ///
    /// ⇒ 判据：**折算属于「怎么解释配置」，不属于「配置是什么」。** 把视图写回存储，就丢掉了
    /// 用户的原始意图，而设置页读的正是存储。
    ///
    /// # 优先级（两层，都必须保持）
    ///
    /// 1. **组间**：折算顺序 = 消费点的判定顺序，撞键时先折的赢。主输入路径上是
    ///    以词定字（`select_char_index`）→ 翻页/高亮（`apply_session_action`）
    ///    → 二三候选（`select_key_offset`），故这里必须同序。`comma_period` 同时是选词键组
    ///    与以词定字键组的合法值，顺序就是唯一的裁决依据——搞反了的表现是「一直用的 `,`
    ///    突然从取字变成选次选」，而用户什么都没改。
    /// 2. **显式优先**：`session_actions` 里写了的键覆盖折算结果。用户在高级表里改的就该赢。
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
            switch_engine: default_switch_engine(),
            toggle_full_width: default_toggle_full_width(),
            toggle_punct: default_toggle_punct(),
            toggle_toolbar: default_toggle_toolbar(),
            open_settings: default_open_settings(),
            add_word: default_add_word(),
            open_add_word_dialog: default_open_add_word_dialog(),
            toggle_s2t: default_toggle_s2t(),
            activate_ime: default_activate_ime(),
            pin_candidate: default_pin_candidate(),
            delete_candidate: default_delete_candidate(),
            take_screenshot: default_take_screenshot(),
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

/// 候选无效按键策略（[keys.overflow]，对齐 Go OverflowConfig）。
/// 每项取值："ignore"（吞键无效）/ "commit"（上屏当前高亮候选）/
/// "commit_and_input"（上屏高亮候选 + 追加按键字符）。默认全 ignore。
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
        }
    }
}

// ───────────────────────── ui（外观）─────────────────────────

fn default_per_page() -> usize {
    7
}
fn default_first_show_settle_ratio() -> f32 {
    0.8
}

fn default_fast_typing_window_ms() -> u64 {
    100
}

fn default_fast_first_show_fallback_ms() -> u64 {
    25
}

/// UI 配置（子表结构，对齐真实 config.toml：[ui.candidate] / [ui.font] / [ui.theme]）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiConfig {
    #[serde(default)]
    pub candidate: UiCandidateConfig,
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
    ///
    /// **数组顺序即优先级**：同一个词在多个库里都有注释时，取靠前那个库的。
    #[serde(default)]
    pub comment_dicts: Vec<CommentDictSpec>,
}

/// 非中文态的图标主字（`[ui.labels]`）。
///
/// ## 这里为什么**没有**中文态
///
/// 中文态的主字是**方案的属性**，配在方案文件的 `[schema] icon_label`（「五」「拼」）。
/// 把它也搬来这里就成了两个来源争同一个显示位，切方案时该听谁的没有答案。
///
/// ## 为什么是全局段而不是下沉到方案
///
/// 英文半角是 `chinese_mode` 这个**全局运行时状态**，与当前是哪个方案无关。下沉到
/// 方案文件后，同一个英文态会随方案切换而改标签（五笔下显 `E`、拼音下显 `En`），
/// 而用户按 Shift 切出去的是同一个状态。
///
/// ## 为什么不复用 `english.schema.toml` 的 `icon_label`
///
/// 「英文半角」与「英文方案」是本仓明确区分的两件事：前者按键原样透传、不出候选；
/// 后者是 active 方案切换、走常规候选路径。而且英文方案可以被用户从方案列表里禁用，
/// 复用它的标签等于让一个可禁用对象持有全局状态的呈现。
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
    /// ⚠️ **截断与回落封在这里，不能留给调用方各做一遍**——这个值有四个消费面
    /// （语言栏图标 / 工具栏 / 状态气泡 / 移动端），其中语言栏那条路会把标签写进
    /// C++ 侧一个 `wchar_t[4]` 的缓冲，漏掉截断的后果不是显示错乱而是宿主进程崩溃。
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
/// # 为什么是独立配置表，而不是塞进 `[[dictionaries]]`
///
/// 🔴 **注释库不参与召回**。若复用词库表加个 `type = "comment"` 区分，那么词库开关、
/// `base_order`、`composite::merge_search`、造词、加词、词频学习 —— 每一条路径都得**记得**
/// 跳过它，漏一处的表现是注释库里的词变成候选。独立成表意味着它从来就不在召回的数据结构里，
/// 不需要任何一处记得跳过。
///
/// 这也是本仓反复出现的教训（见候选调整按来源分流、密码框抑制的分层）：**「加个标志位区分」
/// 要求所有消费点同步，而消费点的数量只增不减。**
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
    /// 限定生效的方案 id；**留空 = 全部方案**。
    ///
    /// 注释库常常是方案专属的：一份大英汉词典只在英文方案下有意义，挂在五笔方案上
    /// 每次输入都要多走一次二分且注定查不到。留空之所以是「全部」而非「无」——
    /// 用户手写一条却没写 `schemas` 时，「到处都显示」比「哪都不显示」好查得多，
    /// 与 `enabled` 缺省即启用同一取舍。
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
    /// 自动隐藏：显示后超时无交互则淡出（默认关）。
    #[serde(default)]
    pub auto_hide: bool,
    /// 自动隐藏超时（秒，默认 5；下限 1 由协调器钳制）。
    #[serde(default = "default_toolbar_auto_hide_delay")]
    pub auto_hide_delay: u32,
    /// 纵向排列（默认 false=横条）。纵向是横向的转置：条宽取主题 `[toolbar] height`，
    /// 每格高取 `button_width`，故同一套主题几何在两个朝向下都成立、无需另配。
    /// 属用户偏好而非视觉设计，所以落在此处而非主题。
    #[serde(default)]
    pub vertical: bool,
    /// 显示哪些条目、按什么顺序。**数组顺序即渲染顺序**，合法项见 [`TOOLBAR_ITEM_KEYS`]。
    ///
    /// **`-` 前缀 = 「在这个位置，但不显示」**（如 `"-s2t"`）：设置页关掉某格时写的就是
    /// 这个，于是关掉再打开它还在原位。若关闭的项直接从数组里删掉，位置信息就丢了，
    /// 重开只能补在声明序位——用户体感是「排好的顺序，关一下再开就乱了」。
    /// 顺序与启用态因此留在**同一个数组**里，不必拆成两个键去同步。
    ///
    /// **留空 = 全部显示**：既是「未配置」的合理默认，也让旧配置文件（无此键）行为不变。
    /// 想整条不要的正确表达是 `visible = false`——那才是「不要工具栏」这个意图的落点。
    /// 设置页另有一道「至少留一格」的闸门，故「每项都带 `-`」只在手写配置时可达
    /// （此时协调器回落成全部显示并告警）。
    ///
    /// ⚠️ 顺序是有语义的，故设置页必须用**列表编辑器**（拖拽排序）而非 `checkbox_group`
    /// ——后者恒按声明顺序写回，会静默改写用户手排的顺序（`config-design-rules.md` §R3）。
    #[serde(default = "default_toolbar_items")]
    pub items: Vec<String>,
    /// 自定义按钮定义（`[[ui.toolbar.buttons]]`）。**定义在此，显不显示 / 排第几由
    /// [`Self::items`] 里的 `custom:<id>` 决定**——两者是引用关系，不是同一语义的两张表。
    ///
    /// 出厂空。这是 expert 档配置：动作是 cmdbar 表达式，设置页不提供编辑器
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
    /// ⚠️ **刻意没有 tooltip 字段**：工具栏至今没有悬停提示机制（`wind-ui` 的 tooltip
    /// 窗口绑在候选窗上，是编码反查用的）。加一个渲染端消费不了的字段，等于给用户一个
    /// 配了永远没反应的旋钮——比没有更糟。要做提示得先给工具栏做悬停窗口，那是独立的
    /// 一件事，不该顺带塞进按钮定义里。
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

/// 工具栏按钮 label 的宽度上限，按**显示宽度**计（CJK 记 2、其余记 1）。
pub const TOOLBAR_LABEL_MAX_WIDTH: usize = 2;

/// 工具栏按钮 label 的截断口径：去首尾空白后，取显示宽度不超过
/// [`TOOLBAR_LABEL_MAX_WIDTH`] 的前缀。即「一个汉字」或「两个 ASCII 字符」。
///
/// # 与 `schema::icon_label_trunc` 的关系（issue #85 后已合并口径）
///
/// 那条口径原先是**字符数** ≤ 2，与这条**显示宽度** ≤ 2 对「符号」判断相反（前者放行、
/// 后者截成「符」）。当时留的话是"若日后要合并两条口径，得先确认语言栏那三个调用点
/// 接受「符号」被截成「符」"——issue #85 就是这个确认：第三方方案的 `icon_label = "虎单"`
/// 在 16px 图标里被回缩到认不出，用户明确要求按显示宽度收。
///
/// 现在两者**同一条规则、不同上限常量**，共用 [`display_width_trunc`]。上限仍各留一个
/// 常量：两处的物理约束不同（这里是等宽方格的长宽比，那里是 16px 位图 + C++ 侧
/// `wcscpy_s` 缓冲），日后其中一处要动不该被另一处绑住。
pub fn toolbar_label_trunc(raw: &str) -> String {
    display_width_trunc(raw, TOOLBAR_LABEL_MAX_WIDTH)
}

/// 按**显示宽度**截断的共享内核：去首尾空白后，取显示宽度不超过 `max_width` 的前缀。
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

/// 是否按双宽字符计。粗口径够用：这里只为决定「一格能放几个」，不是排版引擎。
///
/// 覆盖 CJK 统一表意文字及扩展 A、兼容表意文字、假名、全角/半角形式区、CJK 符号与标点、
/// 注音符号、谚文——即用户会拿来当按钮标签 / 图标主字的那些。其余（拉丁、数字、常用符号、
/// **emoji**）记 1。
///
/// ⚠️ emoji 记 1 是有代价的：它占 2 个 UTF-16 code unit，图标主字上限 2 因此蕴含
/// 「最坏 4 wchar」，C++ 侧 `_inputTypeLabel` 的容量按这个最坏值取。若要把 emoji
/// 划进双宽，先看 `schema::icon_label_limit_counts_scalar_values` 那条断言。
fn is_wide_char(c: char) -> bool {
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

/// 工具栏条目键的全集，同时也是默认值（全部显示）与**默认顺序**。
///
/// 与 [`STATUS_ITEM_KEYS`] 的差别：那份的顺序无语义（状态气泡的渲染顺序固定在代码里），
/// 这份的顺序**就是**渲染顺序。
pub const TOOLBAR_ITEM_KEYS: [&str; 6] = [
    "mode",
    "punct",
    "full_width",
    "s2t",
    "soft_keyboard",
    "settings",
];

/// 出厂**默认显示**的条目。
///
/// ★ 刻意不是 [`TOOLBAR_ITEM_KEYS`] 的全部：那份是「合法值域」，这份是「默认显示哪些」。
/// 软键盘格属于值域却不默认显示——它已有热键与主菜单两个入口，而给所有老用户的工具栏
/// 凭空多一格是打扰。想要它就在 `items` 里写上 `"soft_keyboard"`。
///
/// 两份分开之后，往值域里加新格不再自动改变任何人的工具栏外观。
const DEFAULT_TOOLBAR_SHOWN: [&str; 5] = ["mode", "punct", "full_width", "s2t", "settings"];

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
            auto_hide: false,
            auto_hide_delay: 5,
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
/// # 默认值在这里也写了一份，靠测试防漂移
///
/// 渲染侧（`wind_ui::langbar_icon::IconRenderer`）本来就有一套默认常量，理想情况下
/// 默认值只该有一个出处。但 wind-config 不能反向依赖 wind-ui（层次颠倒），而本仓的
/// 配置约定又要求**每个可配置项都有具体默认值并在 `data/config.toml` 里完整列出**
/// （那份预置文件同时是出厂默认与说明书，有守门测试强制）。
///
/// 于是两处各存一份，用 `wind-coordinator` 的 `langbar_config_defaults_match_renderer`
/// 把它们钉在一起——那个 crate 同时依赖两边，是唯一能做这件比对的地方。漂移的症状
/// （「装设置页看到的默认值与实际渲染不一致」）不会自己暴露，必须靠测试拦。
///
/// 颜色只存字符串，解析与回退发生在协调器侧（同样是因为不能依赖渲染侧的类型）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LangBarConfig {
    /// 标点角标形状。取值见 `wind_ui::langbar_icon::BadgeShape::as_id()`：
    /// `none`（默认，不显示）/ `corner_triangle` / `outer_ring` / `bottom_bar` /
    /// `circle_square` / `ring_dot`。
    ///
    /// 设置页只列前两项；其余靠改本文件——它们要么已被真机否决（圆方、环点），
    /// 要么仍待评估（外圈、底部横条），放进设置页等于把已知的坏选择交给用户。
    /// 无法识别的取值一律回落默认，不报错：配置文件是手写的，写错一个词不该让图标消失。
    #[serde(default = "default_langbar_punct_badge")]
    pub punct_badge: String,
    /// 标点角标大小倍率（1.0 = 形状自带的基准尺寸）。
    #[serde(default = "default_one")]
    pub punct_badge_scale: f32,
    /// 全角标记（右上角三角）总开关。默认关。
    #[serde(default)]
    pub full_width_mark: bool,
    /// 全角标记大小倍率。
    #[serde(default = "default_one")]
    pub full_width_mark_scale: f32,
    /// 两个标记**共用**的不透明度（0~1）。
    ///
    /// 它同时是档位开关：`= 1.0` 走「实心 + 挖空」（标记周围切掉一圈主字），
    /// `< 1.0` 走「半透明 + 保留主字」（笔画从标记里透出来）。两者是互斥的分离手段，
    /// 详见 `wind_ui::langbar_icon::IconRenderer::badge_alpha`。
    #[serde(default = "default_langbar_badge_alpha")]
    pub badge_alpha: f32,
    /// 标记是否用配色。`false` = 一律与主字同色并跟随明暗主题。
    ///
    /// 一个开关同时管两个标记：分开切会出现「关了彩色但右上角还是玫红」，
    /// 而这个开关在用户看来只有一个意思。
    #[serde(default = "default_true")]
    pub colored: bool,
    /// 中文标点角标色，`#RRGGBB`。解析失败回落内置默认并记一条警告。
    #[serde(default = "default_langbar_color_cn")]
    pub punct_color_cn: String,
    /// 英文标点角标色，`#RRGGBB`。
    #[serde(default = "default_langbar_color_en")]
    pub punct_color_en: String,
    /// 全角标记色，`#RRGGBB`。
    #[serde(default = "default_langbar_color_fw")]
    pub full_width_color: String,
}

fn default_one() -> f32 {
    1.0
}
fn default_langbar_punct_badge() -> String {
    "none".to_string()
}
fn default_langbar_badge_alpha() -> f32 {
    0.88
}
fn default_langbar_color_cn() -> String {
    "#2288E0".to_string()
}
fn default_langbar_color_en() -> String {
    "#EE9922".to_string()
}
fn default_langbar_color_fw() -> String {
    "#E0447A".to_string()
}

impl Default for LangBarConfig {
    fn default() -> Self {
        Self {
            punct_badge: default_langbar_punct_badge(),
            punct_badge_scale: 1.0,
            full_width_mark: false,
            full_width_mark_scale: 1.0,
            badge_alpha: default_langbar_badge_alpha(),
            colored: true,
            punct_color_cn: default_langbar_color_cn(),
            punct_color_en: default_langbar_color_en(),
            full_width_color: default_langbar_color_fw(),
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
    /// 与 `display_mode` 正交：`always` 本就在获焦时显示，本项对它无额外效果；真正改变行为的是
    /// `temp`——原本只有用户主动切换中英/标点/全半角时才弹，开启后**换个输入框也弹一次**，
    /// 用来提示「你现在切到的这个框，输入法是什么状态」。
    ///
    /// ⚠ 显示时会绕过 `show_status` 的文本去重：焦点切换恰恰是「状态文本没变但仍要重弹」的场景，
    /// 走去重路径会让它在同状态下**完全不显示**。
    #[serde(default)]
    pub show_on_focus: bool,
    /// 方案名显示样式："full"（全名，默认）| "short"（图标短称 icon_label，回退全名）。
    #[serde(default = "default_schema_name_style")]
    pub schema_name_style: String,
    /// 位置模式："follow_caret"（跟随光标,默认）| "fixed"（固定屏幕坐标 custom_x/custom_y）。
    #[serde(default = "default_status_position_mode")]
    pub position_mode: String,
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
    /// **留空 = 全部显示**：既是"未配置"的合理默认，也让旧配置文件（无此键）行为不变。
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

impl Default for StatusIndicatorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            duration: default_status_duration(),
            display_mode: default_status_display_mode(),
            show_on_focus: false,
            schema_name_style: default_schema_name_style(),
            position_mode: default_status_position_mode(),
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
    /// 编码（组合区）显示方式。单一权威配置，取代旧的 inline_preedit + preedit_mode 组合。
    /// - "app_inline"（默认）：编码内嵌应用光标处，候选窗不显示 preedit 栏
    /// - "candidate_top"：候选窗顶部独立 preedit 栏
    /// - "candidate_inline"：编码作为候选窗首单元内联
    #[serde(default = "default_preedit_display")]
    pub preedit_display: String,
    #[serde(default)]
    pub hide_window: bool,
    /// 候选文本字号（默认 18；0 亦表示跟随主题 behavior.font_size）。
    #[serde(default)]
    pub font_size: f32,
    /// 字号跟随主题（默认开）：true 时忽略 font_size，用主题 behavior.font_size。
    #[serde(default)]
    pub font_size_follow_theme: bool,
    /// 翻页栏显示覆盖："" 跟随主题 / "hide" / "auto"(>1页) / "always"。
    #[serde(default)]
    pub pager_bar_display: String,
    /// 页码文字显示覆盖："" 跟随主题 / "show" / "hide"。
    #[serde(default)]
    pub page_number_display: String,
    /// 首显容差系数（**内部选项**，不进设置页）：首帧用了非权威坐标时，随后到达的权威
    /// 坐标与它相差在 `行高 × 本系数` 以内就**不再校正**——校正本身才是抖动的观感来源，
    /// 十几像素的偏差不动比"跳一下修正"更稳（多数输入法也是这么做的）。
    /// 换行/重排的偏差通常 ≥2 个行高，远超此阈值，仍会正常校正。
    /// 0 表示禁用该容差（任何偏差都校正，即旧行为）。默认 0.8。
    #[serde(default = "default_first_show_settle_ratio")]
    pub first_show_settle_ratio: f32,
    /// 连续输入判定窗口（**内部选项**，毫秒）：两次按键间隔小于此值即视为"连续快速输入"，
    /// 此时 fast 档直接采信首条试探坐标、不再比对上一轮权威坐标。
    /// 依据是连打时光标顺序前移、不发生重排，且用户对"跟手"的敏感度远高于十几像素的偏差。
    /// 0 表示禁用该快路径。默认 100。
    #[serde(default = "default_fast_typing_window_ms")]
    pub fast_typing_window_ms: u64,
    /// fast 档的首显兜底超时（**内部选项**，毫秒）：等不到试探/权威坐标就用现有坐标先显示。
    ///
    /// 为什么必须远小于 wait 档的 150ms：实测 Word 从不发 `OnLayoutChange`（试探坐标无从产生），
    /// 其组合坐标要 60~190ms 才到，而连打时组合只活 27~57ms——上屏即 `reset_first_show()` 作废
    /// timer，150ms 兜底**永远等不到自己到期**，fast 档就此退化成 wait 档，候选窗 57/70 轮不显示。
    /// 取小值让 fast 在这类宿主上退化成 instant（用旧坐标 + 放宽容差）而非干等。
    /// 发 `OnLayoutChange` 的宿主（EverEdit/WPS）试探坐标 3~10ms 就到，不受本值影响。
    /// 默认 25。
    #[serde(default = "default_fast_first_show_fallback_ms")]
    pub fast_first_show_fallback_ms: u64,
    /// 候选文本最大显示字数，超出截断（0=不限）。
    #[serde(default)]
    pub max_chars: usize,
    /// **横排**候选窗的最小宽度，单位 dp（逻辑像素；0=关闭，跟随内容）。
    ///
    /// 下限量的是**整个窗口**，不是单个候选：候选照常按内容紧凑排列并左对齐，凑不满的
    /// 宽度留在窗口右侧空着。翻页栏、内边距、序号列这些窗口内的其它部件一并计入，故
    /// 「窗口宽度不变」是可以直接达成的——这正是本项取代旧 `min_width_chars_*` 的理由：
    /// 旧项把下限打在每个候选格上，横排时每格都被撑宽、格间距成倍放大，量的对象错了。
    ///
    /// 横竖两种排布各配一项（见 [`Self::min_window_width_vertical`]）：可用横向空间差一个
    /// 数量级（竖排每行独占，横排全部候选共享一行），合理值不是同一个答案。
    ///
    /// **单位是 dp 不是字符**：本项量的是窗口而非文字，窗口宽度里还有序号列、各级内边距、
    /// 翻页栏等与字号无关的部分，用字符数换算不出来。dp 随 DPI 缩放（×scale），在高分屏上
    /// 自动等比放大。与 [`Self::max_chars`] 不冲突：后者仍封候选文字的上限。
    #[serde(default)]
    pub min_window_width_horizontal: u32,
    /// **竖排**候选窗的最小宽度，单位 dp（0=关闭，跟随内容）。
    ///
    /// 语义、单位、衡量对象与 [`Self::min_window_width_horizontal`] 完全一致，仅作用排布
    /// 不同——为什么分开配置见该字段文档。
    ///
    /// 与主题 `behavior.vertical_max_width`（竖排宽度上限）冲突时**下限优先**：用户显式配的
    /// 抗抖动宽度不该被主题的裁切上限压回去。
    #[serde(default)]
    pub min_window_width_vertical: u32,
    /// **横排**候选窗的最小高度，单位 dp（0=关闭，跟随内容）。
    ///
    /// 与宽度同为窗口级下限，凑不满的高度留空。横排候选虽只有一行，窗口高度仍会随编码栏
    /// 出现/消失而变，故本项对横排同样有意义。
    ///
    /// 窗口被翻到光标**上方**时，多出的高度补在**顶部**：窗口上方显示时底边贴光标，空白
    /// 压在下面会把候选整体顶离光标，位置反而随内容抖动——正是本项要消除的东西。
    #[serde(default)]
    pub min_window_height_horizontal: u32,
    /// **竖排**候选窗的最小高度，单位 dp（0=关闭，跟随内容）。
    ///
    /// 与 [`Self::min_rows`] 是两种量法，可同时配、取两者较大者：`min_rows` 只数候选行，
    /// 翻页栏、编码栏的出现/消失它管不着（真机反馈的正是翻页栏这一处）；本项量的是窗口
    /// 总高，把这些一并罩住。只想稳住候选区、让翻页栏照常伸缩时仍该用 `min_rows`。
    #[serde(default)]
    pub min_window_height_vertical: u32,
    /// **竖排**候选窗的最小行数（0=关闭，跟随候选数量）。
    ///
    /// 不足此数时补足等高的透明占位行，使窗口高度在候选数变化时保持不变。上限自动
    /// 钳到当前生效的每页候选数（[`Self::per_page`] / [`Self::per_page_extended`]）——
    /// 补出比一页还多的空行只会得到一个大半空白的窗口。
    ///
    /// 横排不适用（候选并列于一行，高度本就恒定）；候选为空的提示态（临拼/临英/网址
    /// 刚进入）也不补足，那时窗口本就只有一行提示，撑成满高更突兀。
    ///
    /// 只稳住**候选区**：翻页栏、编码栏的出现/消失不在其量程内，窗口总高仍会跟着跳。
    /// 要连这些一并罩住用 [`Self::min_window_height_vertical`]（两项可同时配，取较大者）。
    #[serde(default)]
    pub min_rows: usize,
    /// **竖排**候选的注释段（候选右侧灰字）模板。语法见 `wind_coordinator::comment`。
    ///
    /// 横竖各持一份模板、互不影响：两种排布的可用横向空间差一个数量级（竖排每行独占，
    /// 横排全部候选共享一行宽度），能放什么本就不是同一个答案。共用一份的结果是
    /// 「为竖排配的拼音把横排候选窗撑爆」或「为横排收着配的注释让竖排一片空白」。
    #[serde(default = "default_comment_template")]
    pub comment_template_vertical: String,
    /// **横排**候选的注释段模板。见 [`Self::comment_template_vertical`]。
    #[serde(default = "default_comment_template")]
    pub comment_template_horizontal: String,
    /// **竖排**注释段的最大字数（0=不限），超出截断并加 `…`。
    ///
    /// 默认 0：本项引入前注释段从无长度限制，非 0 的默认值会让存量用户的注释突然变短。
    ///
    /// 横竖各一份，与模板同理：横排全部候选共享一行宽度，竖排每行独占，长度预算差一个
    /// 数量级。共用一份的话，为竖排放宽必然把横排也放宽。旧键 `comment_max_chars`
    /// （横竖共用）已退役，值经 [`Config::migrate_comment_max_chars_value`] 抄进两份。
    #[serde(default)]
    pub comment_max_chars_vertical: usize,
    /// **横排**注释段的最大字数（0=不限）。见 [`Self::comment_max_chars_vertical`]。
    #[serde(default)]
    pub comment_max_chars_horizontal: usize,
    /// 自定义序号标签，一槽一项（如 `["a","s","d"]`、`["Ⅰ","Ⅱ","Ⅲ"]`；空表=全部默认 1-9）。
    ///
    /// **每槽是一个字符串而非一个字符**：序号标签本就有多字符形态（`(1)`、罗马数字、
    /// 带 ZWJ 的组合 emoji），按 `char` 切会把它们拆散。这与主题侧
    /// `views.index.labels` 同型——两者由协调器 `resolve_index_label` 三级裁决，
    /// 类型必须一致，否则用户层永远表达不出主题层已支持的形态。
    ///
    /// 槽内**空串 = 该槽让位**（落到主题层，主题也没有才回退数字），故中间空槽有意义，
    /// 不可在写回时按「遇空即停」截断。
    #[serde(default)]
    pub index_labels: Vec<String>,
    /// 候选窗在光标上方时反转候选排列顺序。**仅竖排生效**：横排候选左右并列，
    /// 反转与窗口在上在下无关，只会把读序倒过来，故对横排一律忽略。
    ///
    /// 反转生效期间 `highlight_up` / `highlight_down`（出厂的 ↑↓ 与 Shift+Tab/Tab）
    /// 按**屏幕上看到的方向**走，与排列一致；翻页键不受影响（页与页之间无空间关系）。
    #[serde(default)]
    pub flip_when_above: bool,
    /// 候选窗在光标上方时交换编码栏与候选栏位置（编码区沉底贴光标）。与 flip_when_above 正交，可叠加。
    #[serde(default)]
    pub swap_preedit_when_above: bool,
    /// 翻页栏并入编码栏行、右对齐显示（竖排省一行）。仅"非嵌入编码"（有独立编码栏）时生效。
    #[serde(default)]
    pub pager_in_preedit: bool,
    /// 候选窗定位方式："follow_caret"（默认，跟随光标）/ "fixed"（固定屏幕坐标）。
    /// fixed 下窗口不再随光标移动，也不再上翻（flip/swap_when_above 随之失去意义）。
    #[serde(default = "default_candidate_position_mode")]
    pub position_mode: String,
    /// 固定模式下的**内容左上**屏幕坐标（不含阴影扩边），仅 position_mode="fixed" 生效。
    /// 由用户拖动候选窗落盘，设置页刻意不暴露：手填绝对坐标既不直观又会与拖动互相覆盖
    /// （与 ui.status.custom_x/y 同一决策）。(0,0) 视作"尚未设定"，首次显示落到屏幕默认锚点。
    #[serde(default)]
    pub custom_x: i32,
    #[serde(default)]
    pub custom_y: i32,
}

fn default_preedit_display() -> String {
    "app_inline".to_string()
}

fn default_candidate_position_mode() -> String {
    "follow_caret".to_string()
}

/// 注释段默认模板：`${code_hint|code}` **精确等价于本功能引入前的硬编码行为**
/// （引擎产的剩余编码优先，为空则回退到拼音候选的主码表反查码）。
///
/// 出厂默认能用模板原样表达，是 `${a|b}` 回退语法存在的主要理由 —— 没有它，出厂行为
/// 就得留在代码里作特例，模板便不再是注释内容的唯一真相源。
fn default_comment_template() -> String {
    "${code_hint|code}".to_string()
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
            per_page_extended: 0,
            layout: "horizontal".to_string(),
            preedit_display: default_preedit_display(),
            hide_window: false,
            font_size: 18.0,
            font_size_follow_theme: true,
            pager_bar_display: String::new(),
            page_number_display: String::new(),
            max_chars: 16,
            // 五项下限均出厂关闭：任何非零默认都会让存量用户升级后候选窗突然变宽/变高。
            min_window_width_horizontal: 0,
            min_window_width_vertical: 0,
            min_window_height_horizontal: 0,
            min_window_height_vertical: 0,
            min_rows: 0,
            comment_template_vertical: default_comment_template(),
            comment_template_horizontal: default_comment_template(),
            comment_max_chars_vertical: 0,
            comment_max_chars_horizontal: 0,
            index_labels: Vec::new(),
            flip_when_above: false,
            swap_preedit_when_above: false,
            pager_in_preedit: false,
            position_mode: default_candidate_position_mode(),
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

    /// 用户配置的第 `i` 个序号槽位（0 基）：仅当该槽位存在**且非空**时返回，否则 None。
    /// 供协调器 `resolve_index_label` 裁决「用户 > 主题 > 默认」（None 时主题层接手）。
    ///
    /// 空串按 None 处理，与主题层同判据——这是「中间空槽让位」的落点：用户只想改第 1、
    /// 第 4 槽时写 `["a","","","f"]`，中间两槽仍走主题。
    ///
    /// 此处刻意**不提供**「跳过主题直接回退数字」的便捷方法：那会让调用方绕开主题层，
    /// 而三级裁决只有协调器持有主题槽位，是唯一有资格作答的地方。
    pub fn user_index_label(&self, i: usize) -> Option<String> {
        self.index_labels.get(i).filter(|s| !s.is_empty()).cloned()
    }

    /// 竖排最小行数的**生效值**：按每页候选数封顶（0=不补）。
    ///
    /// 补出比一页还多的空行只会得到一个大半空白的窗口，故以 `per_page` 为上限。扩展档
    /// （`per_page_extended`）若比主档更少，那一档仍可能补出略超一页的空行——为此在每次
    /// 候选更新时重下发一遍不划算，两档差异通常只有一两条。
    ///
    /// 钳制放在配置层而不是 UI 层：候选窗只收到一串候选，不知道也不该知道分页概念。
    pub fn effective_min_rows(&self) -> u32 {
        if self.min_rows == 0 {
            return 0;
        }
        self.min_rows.min(self.per_page.max(1)) as u32
    }

    /// 当前排布对应的注释模板（`vertical` 为 true 取竖排那份）。
    pub fn comment_template(&self, vertical: bool) -> &str {
        if vertical {
            &self.comment_template_vertical
        } else {
            &self.comment_template_horizontal
        }
    }

    /// 当前排布对应的注释段最大字数（0=不限）。与 [`Self::comment_template`] 同构：
    /// 取哪一份由排布决定，调用方不必各自 `if vertical`。
    pub fn comment_max_chars(&self, vertical: bool) -> usize {
        if vertical {
            self.comment_max_chars_vertical
        } else {
            self.comment_max_chars_horizontal
        }
    }

    /// 按 max_chars 截断候选显示文本（0=不限）。超出时截断并加省略号 `…`
    /// 提示"过长"（仅影响显示；上屏用完整原文，见 coordinator 候选下发）。
    pub fn truncate_display(&self, text: &str) -> String {
        if self.max_chars == 0 {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= self.max_chars {
            text.to_string()
        } else {
            let head: String = chars[..self.max_chars].iter().collect();
            format!("{head}…")
        }
    }
}

/// 字体配置（[ui.font]）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiFontConfig {
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub render_mode: String,
    /// 主字体的**回退链**：`family` 里没有这个字时，按本表顺序找下一个。
    ///
    /// ★ 刻意保持 `family: String` 不变、把链尾单独放一个数组，而不是把 `family` 改成数组：
    /// 改类型要配一层 Value 迁移，漏了就让老用户整份配置静默回落出厂值
    /// （见 `docs/design/config-design-rules.md` 与该坑的历史）。分成两个键则**零迁移**，
    /// 设置页现有的字体控件也不用动。最终链 = `[family] + fallback`。
    ///
    /// ⛔ **不得加 `skip_serializing_if`**：本仓用它表达「这个键**退出**配置体系」
    /// （见 `keys.key_actions_materialized` 与本文件里那几个废弃键）。加上之后
    /// `Config::default()` 的序列化产物里没有它 ⇒ `registry_covers_every_config_key`
    /// 的差集为空 ⇒ **守门测试静默放行**，而该键在 CLI、配置片段导入、设置页里全都够不着。
    #[serde(default)]
    pub fallback: Vec<String>,
    /// 按**脚本**指派字体：键是脚本类名（`latin`/`greek`/`cyrillic`/`cjk`/`emoji`/
    /// `digits`/`punct`），值是该类自己的字体链。
    ///
    /// 与 [`Self::fallback`] 是两种机制，缺一不可：回退链只在「当前字体缺这个字」时触发，
    /// 而绝大多数字体都带 ASCII 字形——蒙古文字体自己「有」英文时，回退链永远不触发，
    /// 想换也换不掉。指派则是无条件的。
    ///
    /// ⚠️ 未列出的类**不是**「用默认字体」而是「不单独切段」——它们与其余文字同属默认链。
    /// 两者结果相同但成本不同：不切段就不会为它们各调一次 `SetFontFamilyName`。
    ///
    /// 用 `BTreeMap` 而非 `HashMap`：序列化顺序要稳定，否则每次写盘键序都在变，
    /// 配置文件的 diff 会无端变脏。
    ///
    /// 注册表里登记为 `Map`（见 `config_schema`）：Map 型键**本身就是叶子**，不下钻，
    /// 于是 `latin`/`cjk` 这些是**数据**而不是配置项；顺带拿到 patch 的逐条合并语义
    /// ——配置片段只加一类指派时不会整表替换掉用户已有的。
    /// ⛔ 同样不得加 `skip_serializing_if`，理由见 [`Self::fallback`]。
    #[serde(default)]
    pub scripts: BTreeMap<String, Vec<String>>,
}

/// 主题配置（[ui.theme]）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiThemeConfig {
    // 字段级缺省保持空：加载用户配置缺字段时回退 theme.txt（旧版迁移），不被强制成 default。
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub style: String,
}

// 手写 Default：仅供 Config::default()（getDefaults/恢复本页）给出有效初值，
// 与字段级 serde 缺省（空）解耦，避免影响加载期 theme.txt 迁移回退。
impl Default for UiThemeConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            style: "system".to_string(),
        }
    }
}

/// 悬停提示配置（[ui.tooltip]）。原 ui.tooltip.{code,pinyin,chaizi,debug}.* 子表拍平为平铺字段
/// （三级上限：ui.tooltip.<字段>）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TooltipConfig {
    /// 提示延迟显示时间（毫秒）。
    #[serde(default = "default_tooltip_delay")]
    pub delay: i32,
    /// 编码提示（原 code.enabled）。默认开。
    #[serde(default = "default_true")]
    pub code_enabled: bool,
    /// 拼音提示（原 pinyin.enabled）。默认开。
    #[serde(default = "default_true")]
    pub pinyin_enabled: bool,
    /// 显示多音字所有读音（原 pinyin.heteronyms）。默认开。
    #[serde(default = "default_true")]
    pub pinyin_heteronyms: bool,
    /// 每字最多显示读音数（原 pinyin.max_readings，0=不限）。
    #[serde(default)]
    pub pinyin_max_readings: usize,
    /// 拆字提示（原 chaizi.enabled）。默认关。
    #[serde(default)]
    pub chaizi_enabled: bool,
    /// 调试提示（原 debug.enabled）。默认关。
    #[serde(default)]
    pub debug_enabled: bool,
}

fn default_tooltip_delay() -> i32 {
    200
}

impl Default for TooltipConfig {
    fn default() -> Self {
        Self {
            delay: default_tooltip_delay(),
            code_enabled: true,
            pinyin_enabled: true,
            pinyin_heteronyms: true,
            pinyin_max_readings: 0,
            chaizi_enabled: false,
            debug_enabled: false,
        }
    }
}

// ───────────────────────── input.phrase（短语前缀列举）─────────────────────────

/// 短语前缀列举配置（[input.phrase]，对齐 Go）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhraseConfig {
    /// 触发前缀导航列举的最小输入长度（原 min_prefix_length）。默认 2。
    #[serde(default = "default_phrase_min_prefix")]
    pub min_prefix: usize,
}

impl Default for PhraseConfig {
    fn default() -> Self {
        Self {
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
    /// 打字速度的修正系数（只作用于**速度展示**，不影响任何字数统计）。
    ///
    /// 采集期已经修掉三条结构性偏差（毫秒分母 / 段首口径对称 / 短码长词封顶，见
    /// `wind_store::stats` 的「速度模型」），剩下这一条修不掉：**输入法无从知道用户
    /// 打错了没有**——打错后退格重打，字符被计两遍而耗时只算一遍，方向恒为正偏差。
    /// 故出厂取 < 1 做经验折价。
    ///
    /// ⚠️ 这不是「速度不好看就调大」的旋钮：把 ≥ 1 的值填进来只会让显示值重新偏离真实
    /// 手速。暂不进设置页（无 GUI 入口），需要时改配置文件。
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

// ───────────────────────── debug ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugConfig {
    /// 日志级别。空字符串等同 `info`（生产默认）。
    /// 注意：`info` 级别日志不得包含用户输入内容、词库词条等隐私数据。
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

// ───────────────────────── 共享 default 助手 ─────────────────────────

fn default_true() -> bool {
    true
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

fn default_smart_punct_list() -> String {
    ".,:".to_string()
}

fn default_enter_behavior() -> String {
    "commit".to_string()
}

fn default_space_behavior() -> String {
    "commit".to_string()
}

/// 空码时按标点**出厂即丢弃废码**（与同族的 `enter_behavior` / `space_on_empty_behavior`
/// 刻意不同，那两项仍是 `commit`——这不是漏改）。
///
/// 判据是代价不对称：判错成 `commit` 会把一串没对应任何字的码插进正文，用户往往发出去才
/// 发现、要退格删好几次；判错成 `clear` 只是少上屏了原码，而回车那条路仍旧给原码，一次
/// 按键就能拿到。留着回车不改，是为了不把「获取原码」这个能力面整个封掉。
fn default_punct_on_empty_behavior() -> String {
    "clear".to_string()
}

fn default_pinyin_separator() -> String {
    "auto".to_string()
}

fn default_shift_behavior() -> String {
    "temp_english".to_string()
}

/// 用户配置目录的就绪探测结果。
///
/// 存在的意义是把「系统尚未就绪」与「用户确实没有配置」分开——两者此前都表现为
/// [`Config::load`] 静默跳过用户层，然后 [`Config::active_schema`] 回退到系统预置方案，
/// 用户看到的就是「设置好的方案重启后变回出厂方案」。
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
    /// 漫游根已就绪、但本用户的 `config.toml` 此刻还看不到，**而本地标记表明它本该存在**
    /// （该用户此前确有用户配置）。这是开机早期漫游 profile 尚未挂载完的竞态，不是
    /// 「用户没配置」——**必须继续等**，别把「没看到」当成「没有」而退回系统五笔。
    ConfigPending { dir: PathBuf },
    /// 漫游根已就绪。此时 `dir_exists`/`file_exists` 是**确定性事实**，
    /// 再等下去也不会变，故不属于需要重试的状态。
    Ready {
        dir: PathBuf,
        dir_exists: bool,
        file_exists: bool,
    },
}

impl UserConfigProbe {
    /// 是否已到达「再等也不会变」的状态。`ConfigPending` **刻意排除**：它正是
    /// 「本该有、暂时没看到」的可变态，要继续轮询等漫游挂载。
    pub fn is_settled(&self) -> bool {
        matches!(
            self,
            Self::Portable(_) | Self::CustomDir(_) | Self::Ready { .. }
        )
    }
}

/// 定制版数据层的目录名，与 `data/` **同级**。
pub const CUSTOM_DATA_DIR_NAME: &str = "data_custom";

/// 定制版清单文件名。它在场与否就是「本机是不是定制版」的唯一判据。
pub const CUSTOM_MANIFEST_NAME: &str = "custom.toml";

/// `data_custom/custom.toml` 的内容。
///
/// 清单只负责**减法**（`hide`）与**身份**：加法与整表替换不需要声明，直接把文件放进
/// `data_custom/` 对应位置即可。
///
/// ⚠️ 刻意**不加** `deny_unknown_fields`：清单是第三方定制者手写、随定制包分发的数据，
/// 未来版本新增段（如 `[dicts] hide`）时，旧程序必须能忽略而不是整层退场。代价是
/// `[schema]`（少写一个 s）这类拼写错误会被静默忽略——那一类由 P3 的
/// `wind_input config check --custom` 负责报出来，而不是靠这里把整个定制版判废。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomManifest {
    /// 定制版身份，见 [`CustomIdentity`]。
    #[serde(default)]
    pub custom: CustomIdentity,
    /// 方案减法清单。
    ///
    /// ⚠️ 与 `[schema].hidden` 是**两个正交的轴，不得合并**：`hidden` = 「不列进方案切换
    /// 列表」（english / 快符仍可用、仍被 mix 引用）；本字段 = 「这个方案在本定制版里
    /// 不存在」。拿 `hidden` 实现减法会让被隐藏的方案继续被 mix / special_modes /
    /// `schema.active` 引用到。
    #[serde(default)]
    pub schemas: CustomHideList,
    /// 主题减法清单。
    #[serde(default)]
    pub themes: CustomHideList,
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
    /// 基于哪个主程序版本定制。
    ///
    /// 本期**只解析和保存，不做强制版本检查**——兼容性判定归 P3，届时由校验 CLI 与
    /// 启动告警消费。现在就卡版本会让定制者每次主程序小版本更新都被迫改清单。
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
/// 层名是 `user` / `custom` / `data` 三个**固定字面量**（`&'static str` 而非 String：
/// 值域封闭，写错就编译不过的那种封闭）。它同时是日志措辞（`覆盖生效[custom][schema]`）
/// 与呈现层判据（主题列表的 builtin = 非 user 层）的来源，两处必须指同一件事。
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

    /// 层内子目录（`schemas` / `themes` / `opencc`），**层名不变**。
    ///
    /// 枚举点要的几乎都是「各层的某个子目录」，写成 `layers.map(|l| l.sub("themes"))`
    /// 才不会在 map 里把层名丢掉——层名一丢，日志与 builtin 判定就只能靠猜路径前缀。
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
/// # 为什么要有它
///
/// 追溯一个键要读 4 个层文件 + 跑一次完整 [`Config::load`]。把「采样」与「取键」分开，
/// 一是让 [`Config::key_origin`] 的实现只剩一行，二是留出**同一次采样查多个键**的形态
/// ——那时几个键的答案来自同一个世界，而逐键各采各的样时，用户正好在这中间改了配置
/// 文件，前后两个键就会给出对不上的答案。
///
/// ⚠️ **不要拿它遍历整张注册表**（几百个键）来给设置页做「每项的来源」：那是把一次
/// 排查用的重查询放进了常态路径。设置页要表达的「此项可被方案覆盖」是**静态**事实，
/// 走 capability 快照，与本快照无关。
///
/// # 一致性边界
///
/// 快照是**采样那一刻**的盘上状态，不随后续文件改动更新。它服务于「现在解释一下」，
/// 不是一个可以长期持有的视图——用完即弃。
pub struct OriginSnapshot {
    /// 各层：层名、配置文件路径、该层**整份**未合并的 TOML。从低到高。
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
        //
        // ★ 值相等这一步不能省。表类型跨层深合并时，最高声明层的值只是并集的一部分
        // （user 写 `{a=1}`、data 写 `{b=2}`，生效值是 `{a=1,b=2}`），此时归属到 user
        // 是错的——用户照着去改 user 层，改不动来自 data 的那一半。`normalize()` 改写过
        // 的值同理。宁可报「指不到单独一层」，也不要报一个会把人带偏的层名。
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

/// 一个配置键在**某一层**的声明情况。见 [`Config::key_origin`]。
#[derive(Debug, Clone, PartialEq)]
pub struct LayerOrigin {
    /// 层名：`default` / `data` / `custom` / `user`。
    ///
    /// 后三个与 [`ResourceLayer::name`] 同名同义（日志 `覆盖生效[custom][…]` 里也是它们）；
    /// `default` 是配置层独有的第四个——资源层没有「代码里的默认文件」这回事。
    pub layer: &'static str,
    /// 该层配置文件的路径。`default` 层是代码里的默认值，没有文件，恒为 `None`。
    ///
    /// 层不存在（非定制版的 `custom`、漫游目录取不到的 `user`）时同样是 `None`，
    /// 与 `value: None` 一起表示「这一层根本不在」。
    pub path: Option<PathBuf>,
    /// 该层**显式声明**的值；`None` = 这一层没写这个键（或该层不存在）。
    ///
    /// ⚠️ 不是「该层生效后的值」——层与层之间是深合并，单层的声明可能只是最终值的一部分。
    pub value: Option<toml::Value>,
}

/// 一个配置键的来源追溯：每层各声明了什么、最终生效的是哪个。见 [`Config::key_origin`]。
#[derive(Debug, Clone, PartialEq)]
pub struct KeyOrigin {
    /// 各层声明，**从低到高排列**（`default` → `data` → `custom` → `user`）。
    ///
    /// 顺序与 [`Config::resource_layers_named`] 相反是有意的：那个 API 服务于「逐层找文件，
    /// 先找到的赢」，倒序最省事；而这里是给人看的，覆盖关系从下往上叠更符合直觉。
    /// 恒含四个元素（层不存在时也占位），呈现端因此不必判断「这一层怎么没了」。
    pub layers: Vec<LayerOrigin>,
    /// 实际生效值，取自一次真实的 [`Config::load`]（含 `normalize` 归一化之后）。
    ///
    /// ★ **不是从 `layers` 推算的**：推算会漏掉 `normalize()` 的改写与段级降级的回落，
    /// 而这两者恰好是「我明明设了值，为什么不是这个」最常见的两个答案。
    pub effective: Option<toml::Value>,
    /// 生效值来自哪一层。`None` 表示**指不到单独一层**，三种成因：
    /// 表类型跨层深合并（生效值是多层的并集）、`normalize()` 改写过、或该键压根不存在。
    pub effective_layer: Option<&'static str>,
    /// 本次加载中，该键是否被段级降级殃及（[`ConfigDegradation::taints`]）。
    ///
    /// 为真时**用户层的值没有进入生效值**——那一段整个回落了出厂默认。这是「设置改了
    /// 不生效」里最难自查的一种：配置文件里白纸黑字写着，程序却在用别的值。
    pub degraded: bool,
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
            // DEBUG 而非 INFO：面向用户的**权威摘要**只有一处——服务启动路径上的
            // `wind_rpc::custom_edition::startup_summary()`（位置确定、含显示名）。这里再打
            // 一行 INFO 会让读日志的人以为定制层被加载了两次，而两行内容还不完全一样。
            //
            // 保留这一行（不删）的理由：它记录的是「解析成功、层已启用」这个事实，且触发点
            // 就是**第一次**问 `custom_manifest()` 的地方——排查「定制层什么时候才生效」
            // 时，这个时间点本身就是线索；而非 service 的宿主（TSF DLL / 移动端 / CLI）
            // 根本走不到那条启动摘要。
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
    /// 合并方式：把各层的 `toml::Value`（默认值序列化得到）深合并（表递归、标量/数组后者覆盖），
    /// 最后一次性反序列化为 `Config`。所有段都会被合并，不再静默丢弃；新增配置字段无需改合并代码。
    ///
    /// L2.5（定制层）只需写**差异键**，不必整份复制 `config.toml`——机制上支持深合并，
    /// 差异越小跨版本存活率越高。
    pub fn load(data_dir: Option<&Path>) -> anyhow::Result<Self> {
        // Layer 1: 代码默认值（序列化为 Value，保证所有字段存在）
        let mut merged = toml::Value::try_from(Self::default())?;

        // Layer 2: 系统预置配置 (data/config.toml)
        if let Some(data_dir) = data_dir {
            let sys_config = data_dir.join("config.toml");
            if let Some(v) = Self::read_toml_value(&sys_config) {
                merge_value(&mut merged, v);
                info!("Loaded system config: {}", sys_config.display());
            }
        }

        // Layer 2.5: 定制版预置配置 (data_custom/config.toml)。
        // 位置固定在 L2 之后、L3 之前：定制者可以覆盖出厂值，但绝不能压过终端用户。
        if let Some(custom_dir) = Self::custom_data_dir() {
            let custom_config = custom_dir.join("config.toml");
            if let Some(v) = Self::read_toml_value(&custom_config) {
                merge_value(&mut merged, v);
                info!("Loaded custom config: {}", custom_config.display());
            }
        }

        // Layer 3: 用户配置 (%APPDATA%/WindInput/config.toml)
        match Self::user_config_dir() {
            Some(user_dir) => {
                let user_config = user_dir.join("config.toml");
                if let Some(v) = Self::read_toml_value(&user_config) {
                    merge_value(&mut merged, v);
                    info!("Loaded user config: {}", user_config.display());
                }
            }
            // 漫游 known folder 解析失败。此前这里静默跳过整个用户层，配置退化为
            // 「默认 ⊕ 系统层」，用户的 schema.active 等设置全部失效且无任何痕迹。
            None => warn!("User config dir unavailable, user layer skipped"),
        }

        Self::migrate_enable_english_value(&mut merged);
        Self::migrate_force_vertical_value(&mut merged);
        Self::migrate_index_labels_value(&mut merged);
        Self::migrate_comment_max_chars_value(&mut merged);
        Self::migrate_empty_code_behavior_value(&mut merged);
        // 位置刻意在**四层合并之后、`try_into` 之前**：定制层 (L2.5) 里的旧值因此与用户层
        // 一样被这一批迁移救到，白捡的——迁移作用在合并结果上，不认值来自哪一层。
        // ⛔ 段级降级**不能**顶替上面这一族 `migrate_*_value`：迁移是「把旧值无损搬到新形态」，
        // 降级是「把这一段整个丢掉换成出厂值」。已发布字段改类型仍必须写迁移，
        // 降级只是最后一道兜底。
        let mut config = Self::deserialize_with_section_fallback(merged);
        config.normalize();
        Ok(config)
    }

    /// 反序列化 `merged`，失败时**只**把有毒的那一段替换成 L1 默认，其余配置原样保留。
    ///
    /// 为什么需要它：整份 `try_into` 是全有全无的——任何一层里一个类型不匹配的键都让
    /// `load()` 返回 `Err`，而调用方几乎都是 `unwrap_or_default()`（`construct.rs`、
    /// `apps/repl`、`wind-mobile`）⇒ 用户的方案/词库/按键/主题**一起**回落出厂值，
    /// 只留一行日志。段的边界恰好是功能边界，用户感知到的应该是「按键设置回默认了」
    /// 而不是「一切归零」。
    ///
    /// ★ 定位有毒段用的是**探针**（把待测的那一段贴到全默认的骨架上试），不是「逐段替换
    /// 直到成功」。后者会误伤：若毒在 `keys`，替换 `input` 后仍然失败，此时无从区分 `input`
    /// 是不是也有毒，只能连它一起降级——那是在丢用户配置。探针法对每段给出独立判定。
    ///
    /// ★ 探两层：顶层段判坏之后，对它的**直接子键**再探一轮，能定位到 `ui.font` 就不要
    /// 降 `ui` 整段。粒度不是锦上添花——`ui` 一段 99 个键、`schema` 88 个，整段回落等于
    /// 候选窗尺寸、字体、主题、工具栏、注释模板一起没，离「一切归零」并不远，而缩小
    /// 爆炸半径就是这套机制的全部意义。**只递归这一层**：再往下收益递减，而每多一层
    /// 就多一份「把配置切碎成半新半旧」的风险。子键探不出结果（该段不是表、或毒在段
    /// 自身的结构上）则退回整段降级。
    ///
    /// ⚠️ 成功路径**不是**零开销：`merged` 会被深 clone 一次。本机实测（259 个叶子键，
    /// dev profile 已开优化）clone ≈ 24µs，`clone + try_into` ≈ 41µs——clone 占约六成。
    ///
    /// 这不是能省掉的：toml 0.8 只有 `impl Deserializer for Value`（**按值**消费），没有
    /// `&Value` 的实现，`try_into` 失败后拿不回所有权，而失败路径必须拿着原值去做探针。
    /// 相对于 `load()` 自身的两次文件 IO，这一次 clone 不在同一量级，故不值得为它改结构。
    ///
    /// 失败路径的探针次数：顶层段数 + 有毒段的子键数，均为个位到几十，只在异常时发生。
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
                    },
                    ..Config::default()
                };
            }
        };

        // (点分路径, **该路径自己的**反序列化错误)。
        // ⚠️ 每条都必须带自己的错误：多段同时有毒时，整份 `try_into` 的 `root_err` 只讲得清
        // 其中一个段，拿它给每一行 WARN 用会把排查的人直接带到无关的段上。
        let mut bad: Vec<(String, String)> = Vec::new();
        if let Some(sections) = merged.as_table() {
            for (section, value) in sections {
                let Some(section_err) = probe_section(&default_v, &[section], value) else {
                    continue;
                };
                let narrowed = narrow_bad_section(&default_v, section, value);
                if narrowed.is_empty() {
                    bad.push((section.clone(), section_err));
                } else {
                    bad.extend(narrowed);
                }
            }
        }
        // 顶层不是表时上面一条都收不到，直接落到下面的整体回落分支。
        //
        // 显式排序而不是依赖 `toml::Table` 的遍历序：后者是否有序取决于 `preserve_order`
        // 特性，而特性可能被任何一个传递依赖打开——那种翻车只在别人的依赖树里复现。
        bad.sort();

        let mut patched = merged;
        for (path, _) in &bad {
            reset_path_to_default(&mut patched, &default_v, path);
        }

        match patched.try_into::<Config>() {
            Ok(mut config) if !bad.is_empty() => {
                for (path, err) in &bad {
                    // WARN 而非 INFO：这是**异常**，不是正常降级。压成 INFO 会掩盖真实的
                    // 迁移缺失——本该写 `migrate_*_value` 的字段类型变更会在这里悄悄"自愈"。
                    warn!(
                        "配置段 [{path}] 解析失败，已回落出厂默认值（该段的用户设置本次不生效）；\
                         原始错误：{err}"
                    );
                }
                config.degradation = ConfigDegradation {
                    sections: bad.into_iter().map(|(path, _)| path).collect(),
                    total_fallback: false,
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
                    },
                    ..Config::default()
                }
            }
        }
    }

    /// 存量迁移（**须在反序列化前**跑，字段已从 [`QuickInputConfig`] 移除、结构体上读不到）：
    /// 废弃键 `schema.quick_input.enable_english = false` → 从内置 quick_mix 的 members 移除
    /// `english`。
    ///
    /// 该键与 members 曾是双真相源；语义合并到 members 后，关掉过英文候选的存量用户
    /// 必须在这里落成成员删除，否则升级后英文候选会自己冒回来。只认 false——true 是默认值，
    /// 无需动作。
    /// 存量迁移（**须在反序列化前**跑）：空码三键（`input.enter_behavior` /
    /// `space_on_empty_behavior` / `punct_on_empty_behavior`）的值域收窄为 `commit | clear`，
    /// 把设置端曾经误列的 `ignore` / `commit_and_input` 归一到 `commit`。
    ///
    /// 那两个值**从未被实现过**：消费点只判 `== "clear"`，其余一律走 commit 分支。所以这条
    /// 迁移是**零行为变更**，纯粹让存量配置落回合法值域——不做的话，设置页下拉「按当前值
    /// 恢复选中项」匹配不到，会静默弹回首项，用户看到的是自己的配置被悄悄改了。
    fn migrate_empty_code_behavior_value(merged: &mut toml::Value) {
        const KEYS: [&str; 3] = [
            "enter_behavior",
            "space_on_empty_behavior",
            "punct_on_empty_behavior",
        ];
        let Some(input) = merged.get_mut("input").and_then(|i| i.as_table_mut()) else {
            return;
        };
        // ★ 合法值域**取自注册表**，不在此处另抄一份。三键的值域并不相同（标点多一个
        // `clear_no_input`），且日后还会加值；抄一份的后果是「新值加了，但迁移把存量配置里
        // 的它抹回 commit」——只在升级过的机器上复现，本地怎么测都是绿的。
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

    /// 存量迁移（**须在反序列化前**跑）：`ui.candidate.index_labels` 由「字符串，每 char
    /// 一槽」改为「字符串数组，每项一槽」，旧值按 char 拆成数组。
    ///
    /// 这条迁移不可省。类型不匹配不是「这一项失效」而是 [`Self::load`] 里
    /// `merged.try_into()?` **整体返回 Err**，而调用方多为 `unwrap_or_default()`
    /// （`construct.rs`、repl、wind-mobile）——配过此项的用户会连方案、词库、按键
    /// 一起静默回落出厂值，只留一行日志。
    ///
    /// 拆分按 `char` 而非字素簇：旧形态本就只能表达单 char 槽位，按 char 拆是对旧值的
    /// **无损**还原；用字素簇反而会把旧配置里被拆散的组合序列错误地粘回一槽，改变行为。
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

    /// 存量迁移（**须在反序列化前**跑）：`ui.candidate.comment_max_chars`（横竖共用一份）
    /// → `comment_max_chars_vertical` / `_horizontal` 各一份。
    ///
    /// 只在新键**缺失**时抄旧值：用户若已写了新键，那是更明确的意图，不该被旧键盖掉。
    ///
    /// ⛔ 旧键**不进** [`RETIRED_KEYS`]（理由见那份清单末尾）：本函数每次 load 都要读它，
    /// 而那份清单是在用户文件上做删除、本函数只改内存 ⇒ 登记进去等于下次启动就迁不到了。
    ///
    /// ★ 这条迁移不可省：它是**非零默认**的键（默认 0=不限，但配过非 0 值的人正是在意
    /// 长度的那批），不迁移的表现是「升级后注释突然不再截断」——而这类回归无人会报 bug，
    /// 用户只会觉得候选栏变宽了。
    fn migrate_comment_max_chars_value(merged: &mut toml::Value) {
        let Some(cand) = merged
            .get_mut("ui")
            .and_then(|u| u.get_mut("candidate"))
            .and_then(|c| c.as_table_mut())
        else {
            return;
        };
        let Some(old) = cand
            .get("comment_max_chars")
            .and_then(toml::Value::as_integer)
        else {
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

    /// 存量迁移（**须在反序列化前**跑，字段已从 [`QuickInputConfig`] 移除）：
    /// 废弃键 `schema.quick_input.force_vertical` → 内置 quick_mix 的
    /// [`MixModeConfig::candidate_layout`]。
    ///
    /// 映射刻意**不对称**：
    /// - `true`  → `"vertical"`（强制竖排）
    /// - `false` → `"follow"`（**不是** `"horizontal"`）——旧布尔的 false 语义是「不强制」，
    ///   即跟随全局；写成 horizontal 会把「没开过这个开关」的用户强行钉在横排上。
    ///
    /// 键不存在则不动，让 [`default_mix_modes`] 的出厂值（Vertical）生效。老版预置文件
    /// 写的是 `force_vertical = true`，与出厂值同义，故未改过配置的用户升级后行为不变。
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
    /// ⊕ `data_custom/config.toml`(L2.5)，**不含用户层(L3)**。
    ///
    /// 供 capability 的 `default` 来源——出厂默认 = L1⊕L2⊕L2.5。系统预置与定制层
    /// 都可合法覆盖 L1（如 `schema.active`）。
    ///
    /// ★ **定制层必须进入这个计算，漏了就出事。** [`preset_for_pruning`] 与
    /// [`materialize_key_actions`] 拿这个值去**删用户层的键**：
    ///
    /// - 定制层不进来 ⇒ 用户在定制版里把开关点到「定制默认」位会被判成「与默认不同」而
    ///   写进用户层永久钉死，此后不再跟随定制层的任何更新；
    /// - 反向算错则**静默删掉用户真实设置**。这颗雷本仓已引爆过一次（真机一份配置
    ///   105 键中 62 键冗余，`schema.mix.auto_commit_block_on_pinyin` 已经中招）。
    ///
    /// ⚠️ **连带效应是正确行为，不要当 bug 修掉**：`capabilities::generate`（wind-rpc）
    /// 也吃这个值，于是定制版设置页显示的「出厂默认」、以及「恢复默认」按钮的落点，
    /// 都会变成**定制默认值**。这正是定制版用户应该看到的语义。
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
            if let Some(v) = Self::read_toml_value(&custom_config) {
                merge_value(&mut merged, v);
            }
        }
        Ok(merged)
    }

    /// 「与默认相同即不落盘」判定所用的出厂默认（L1⊕L2⊕L2.5）。取不到则 `None`。
    ///
    /// ⚠️ **必须确认 `data/config.toml` 在场才返回 `Some`**：[`system_preset_value`] 传 `None`
    /// 会退回纯 L1，而 L2 本就允许合法覆盖 L1（`schema.active` 等出厂值只写在 L2）。
    /// 拿纯 L1 当"默认"去比对，会把用户显式设的值误判成默认而删掉，
    /// `load()` 时再从 L2 回落成**另一个**值 —— 用户的设置被静默改写，比不清理坏得多。
    ///
    /// 这不是假想：`schema.mix.pinyin_only_overflow` 与 `auto_commit_block_on_pinyin` 就曾长期
    /// L1/L2 不一致（已随本次修复对齐），此类漂移只要发生一次，纯 L1 比对就会开始吃用户配置。
    ///
    /// 返回 `None` 时调用方一律退化为「照常写入 / 不清理」，即旧行为。
    ///
    /// ⚠️ **闸门判据仍是「`data/config.toml` 在场」，加了定制层也不改。** `data_custom`
    /// 单独在场而 `data/config.toml` 缺失时必须返回 `None`：L2 才是出厂基线的必要部分，
    /// 定制层只写差异键，拿「L1⊕L2.5」这份残缺 preset 去删用户键正是上面那颗雷。
    /// 定制层的值由 [`system_preset_value`] 自动带进来，无需在此另加分支。
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
    /// **不变量：清理前后 `load()` 的结果逐键完全相同** —— 删掉的每个键，四层合并时都会从
    /// L1⊕L2⊕L2.5 回落到同一个值。故本操作对当前行为零影响，只影响**将来**默认值变更能否到达该用户。
    /// `set_user_value` 的同款收口负责不再产生新的冗余键，本函数负责清掉存量（该收口上线前
    /// 积累的量很可观：真机一份配置 105 键中 62 键冗余）。
    ///
    /// 幂等——跑第二次删 0 个。`data/config.toml` 或用户层缺失时直接返回 0。
    pub fn prune_user_config() -> anyhow::Result<usize> {
        let Some(dir) = Self::user_config_dir() else {
            return Ok(0);
        };
        let file = dir.join("config.toml");
        let Some(mut root) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
        else {
            return Ok(0);
        };

        // 退役键（[`RETIRED_KEYS`]）先清：它们与出厂默认无关，**不能**被 preset 取不到时的
        // 提前返回挡住——否则没装 data/config.toml 的环境永远清不掉。
        let mut removed = prune_retired(&mut root);
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

    /// **一次性物化**：把折算后的引导键绑定写进用户层 `keys.key_actions`，此后停止折算。
    /// 返回物化的绑定条数（0 = 已做过 / 条件不满足）。
    ///
    /// # 为什么必须落盘，而不能继续「每次加载折算一次」
    ///
    /// 五处 `trigger_keys` 的出厂值住在 L2（`data/config.toml`），
    /// [`Self::migrate_trigger_keys_into_key_actions`] 每次 `load()` 都把它们折算进
    /// `keys.key_actions`。而设置页在「五c 全局层收编」之后**只写 `key_actions`、从不写
    /// `trigger_keys`**——于是用户层根本没有一个能压制 L2 出厂值的键：
    ///
    /// 1. 用户在设置页取消勾选 `` ` `` → 用户层 `key_actions` 里那条被删掉（真的删了）；
    /// 2. 下次 `load()`：用户层没写过 `trigger_keys` ⇒ 深合并回落 L2 的 `["backtick"]`
    ///    ⇒ 折算发现 `key_actions` 里没有 `backtick` ⇒ **重新插回去**；
    /// 3. 折算唯一的守卫是 `contains_key`，它区分不了「用户配过别的」与「用户删掉了它」。
    ///
    /// 表现是「取消勾选保存后毫无效果，反引号照样触发，再点应用设置却说『没有变更』」
    /// （最后那句来自设置页保存成功后 `base_config = current_config` 的乐观更新）。
    /// 覆盖安装、备份还原都无法带走「我删过它」——**那个意图从来没有被写进任何地方**。
    ///
    /// 物化之后，`key_actions` 成为唯一真相源，删除就是普通的删除；L2 的 `trigger_keys`
    /// 降级为出厂声明处，只供设置页「恢复默认」按 capability 读取。
    ///
    /// # 对新用户也必须跑
    ///
    /// 只迁移「已有 config.toml 的老用户」会让方案退化成没修：新装机器的用户层里永远没有
    /// 实体条目，删除依然会被 L2 折算复活。故用户层文件不存在时按空表处理，照常物化。
    ///
    /// # 两道安全闸（缺一不可，且都退化为「什么都不做」）
    ///
    /// 1. **用户配置目录不可用**（漫游未挂载）→ 不动。此时用户层「看起来是空的」，
    ///    照做会把出厂绑定物化成用户的全部绑定，抹掉他真实的自定义。同
    ///    [`Self::prune_user_config`] 必须排在 `wait_user_config_ready` 之后的理由。
    /// 2. **L2 `data/config.toml` 不在场** → 不动。折算结果依赖 L2 声明的出厂绑定，
    ///    L2 缺席时折算出的是一张**残缺**的表，物化下去 = 用户永久丢失出厂绑定，
    ///    而且标记一置位就再也不会补回来。这是本函数最危险的一条路径。
    /// 3. **本次 `load()` 的 `keys` 段被降级** → 不动。与闸二同一个失效模式，但入口不同：
    ///    段级降级（[`Self::deserialize_with_section_fallback`]）会把有毒的 `keys` 段换成
    ///    L1 默认，于是 `bindings` 只剩出厂绑定，而 `materialize_into` 是**无条件整表覆盖**，
    ///    还会顺手摘掉 `input.temp_pinyin/temp_english.trigger_keys` 与 `mix_modes[].trigger_keys`
    ///    并打上一次性版本标记 ⇒ 用户的自定义绑定**永久**没了、且再也不会重跑自愈；
    ///    毒若恰在 `key_actions` 里还会被自己覆盖掉，现场都不剩。
    ///
    ///    段级降级上线**之前**这条路不存在：那时 `load()` 直接返回 `Err`，下面那个 `?`
    ///    就是保护，`main.rs` 只 warn 一句、一个字节都不写，用户改掉毒键即可复原。降级把
    ///    `Err` 变成了「成功但内容残缺」，保护随之失效——所以必须在这里补回来。
    ///
    /// 幂等：靠 `keys.key_actions_materialized` 版本号，不靠「看起来像迁移过了」的推断。
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
        // 文件不存在 = 新用户，按空表继续（见上文「对新用户也必须跑」）。
        // 解析失败也按空表：那种情况下用户层本就不生效，物化不会额外丢失什么。
        let mut root = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
            .unwrap_or_else(|| toml::Value::Table(Default::default()));
        if !root.is_table() {
            root = toml::Value::Table(Default::default());
        }

        // 幂等闸：版本号已达标就退出（不必读 L2、不必 load）。
        if already_materialized(&root) {
            return Ok(0);
        }

        // 权威来源 = 一次完整 `load()`：它已跑完 normalize，`key_actions` 就是当前**生效**
        // 的那张表（四层合并 ⊕ 折算 ⊕ 「用户显式配过的不被覆盖」）。这里绝不能自己再抄
        // 一遍折算规则——抄一份就是第二个真相源，本次修的正是这类问题。
        let cfg = Self::load(data_dir.as_deref())?;
        // 闸三：`keys` 段本次被降级过 ⇒ `key_actions` 不是用户真实的绑定，是出厂残表。
        // 判据用 `affects` 而不是精确相等——降级粒度可以细到 `keys.key_actions` 这一层。
        // 判据走共用的 [`ConfigDegradation::blocks_write_back`]，四条写盘闸同一份实现、
        // 同一条日志措辞。判 `keys` 而不是 `keys.key_actions`：本函数还会顺手摘掉
        // `input.*.trigger_keys`，而 `bindings` 的正确性依赖整个 keys 段。
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

    /// 读取 TOML 文件为 Value（不存在/解析失败返回 None 并告警，不中断加载）
    fn read_toml_value(path: &Path) -> Option<toml::Value> {
        if !path.exists() {
            // 「文件不存在」曾是唯一无日志的失败路径：它让「用户没有配置」与
            // 「开机早期读不到配置」在日志上完全同形，只能靠有无 `Loaded user config`
            // 反推。DEBUG 级——`load()` 在热重载/RPC 上高频调用，不能进 INFO 刷屏。
            debug!("Config file absent: {}", path.display());
            return None;
        }
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str::<toml::Value>(&content) {
                Ok(v) => Some(v),
                Err(e) => {
                    info!("Skip invalid config {}: {}", path.display(), e);
                    None
                }
            },
            Err(e) => {
                info!("Cannot read config {}: {}", path.display(), e);
                None
            }
        }
    }

    /// 反序列化后的归一化：修正无效值（如 per_page=0 视为未设置，回退默认）。
    /// 归一化 + 存量迁移。**幂等**，且必须挂在「所有配置生效的必经之路」上，
    /// 而不是只在 `load()` 里调一次。
    ///
    /// ★ 教训：迁移最初只在 `load()` 里跑，于是 `refresh_config_in_memory`
    /// （设置页改配置后的生产路径，直接重建 bundle）拿到的 cfg 没经过折算——
    /// 而消费点已改成只读新表，表现是「在设置页保存一次，引导键就全失效」。
    /// 现由 `ConfigBundle::build` 统一调用，那是配置生效的单一入口。
    pub fn normalize(&mut self) {
        if self.ui.candidate.per_page == 0 {
            self.ui.candidate.per_page = default_per_page();
        }
        self.migrate_quick_mix_pinyin_member();
        self.migrate_quick_input_legacy_member();
        self.migrate_letter_trigger_keys();
        // 须在 migrate_letter_trigger_keys **之后**：那一步先把字母项摘干净，
        // 这里看到的 trigger_keys 已只剩符号键。
        self.migrate_trigger_keys_into_key_actions();
        self.warn_legacy_schema_hotkeys();
        // ⛔ 这里**不再**把四组键组配置折算进 `session_actions`。折算是**消费层的视图**，
        // 不是存储层的改写——见 `KeysConfig::effective_session_actions` 的文档。
        self.warn_legacy_special_modes();
    }

    /// 残留的 `schema.special_modes` 告警（**不迁移**，见 `docs/redesign/overlay-mode-config.md` §5）。
    ///
    /// 该键已废弃：实例集合改由「带 `[overlay]` 段的已安装方案」定义。刻意不做自动迁移——
    /// 呈现字段的新家是 `schema_overrides/{id}.toml`，而本函数所在的 `normalize` 是
    /// **零 IO、幂等、纯内存**的（五c 立的口径：不写盘，回退一版就能工作），写盘型迁移要
    /// 另起一套「一次性副作用 + 迁移状态标记」的机制，代价远超它能省下的那几行手工配置。
    ///
    /// 但也**不能静默丢弃**：用户的 config.toml 里那一段还在，看起来像仍然生效。
    /// 一条 warn 是「让失效可见」的最低成本手段——这正是本仓反复栽过的「配了没反应」那类。
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

    /// 残留的 `keys.schema_hotkeys` 告警（**不迁移**）。
    ///
    /// 该键已废弃：方案直达热键并入 `keys.key_actions` 的 `switch_schema:<方案id>`。
    /// 刻意不做自动折算——保留一条兼容路径的代价是永远要维护两套配置形态的等价性，
    /// 而它能省下的只是用户手改两行。处置与 [`Self::warn_legacy_special_modes`] 一致。
    ///
    /// 但也**不能静默丢弃**：用户的 config.toml 里那一段还在，看起来像仍然生效。
    /// 一条 warn 是「让失效可见」的最低成本手段——「配了没反应且查不到原因」正是本仓
    /// 反复栽过的那类缺陷，而这里恰好是唯一能让它可见的时机。
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
    /// 折算在**内存里**做，不写回配置文件：与本文件其余 `migrate_*` 同策略。用户的
    /// config.toml 保持原样，回退一个版本就能照常工作；旧字段的真正消失发生在用户下次
    /// 用设置页保存时（GUI 按新的工作态全量写回）。
    ///
    /// # 冲突处置：先到先得，顺序必须复现 `try_activate_mode` 的**真实**调用顺序
    ///
    /// ★ 那个顺序是 **临英 > 临拼 > special > mix**（`handle_lifecycle.rs` 里依次是
    /// 临英触发键 → 临拼触发键 → 特殊模式 → mix）。设计文档 §1 曾写成
    /// 「临英 > 快捷输入 > 临拼 > 特殊模式」，**是错的**——照那个顺序迁移会让
    /// 「`;` 同时配给 mix 和临拼」的用户在升级后进错模式，且现象是「一直用的键突然
    /// 变了功能」，极难联想到是迁移干的。以代码为准，不以文档为准。
    ///
    /// 同一个键被多处占用时，未中选的那几处也要清空——留在配置里同样是静默失效，
    /// 而用户会以为它还生效（这正是本次收编要消除的问题之一）。
    ///
    /// 已在 `keys.key_actions` 里显式配过的键**不覆盖**：用户的新配置优先于存量迁移。
    ///
    /// # ⚠️ 它让 `key_actions` 成为**跨段折算**的产物（降级判据要知道这件事）
    ///
    /// 来源里的 `input.temp_english.trigger_keys` 一类住在 **`input` 段**，折算目标在
    /// `keys` 段。于是段级降级把 `input` 换成出厂值时，`keys.key_actions` 里由本函数
    /// 填进去的那几条也一起变成出厂值，而 `degradation.sections` 记的是 `input.*`——
    /// 任何「只问 `keys.*`」的判据都够不着。
    ///
    /// 已知的消费者是 `wind-webdata` 的 `keys_overview`（设置页按键总览），它**刻意
    /// 不把 `input` 段拉进判据**：本函数只对 `key_actions_materialized < VERSION` 的
    /// 存量用户跑一次，是过渡态，而拉进去的代价是「存量用户 `input` 段一坏，按键总览
    /// 整片消失」。这条取舍记在这里，是为了让下一个动本函数的人知道它有这么一个
    /// 「跨段」的性质——**别把新的折算来源加成常驻的**，那会让上面那个取舍不再成立。
    fn migrate_trigger_keys_into_key_actions(&mut self) {
        // ★ 已物化：`key_actions` 是唯一真相源，本折算必须彻底让位。
        //
        // 折算在这里多跑一次的代价不是「白做功」而是「复活用户删掉的绑定」：折算的守卫
        // 只有 `key_actions.contains_key(k)`，它区分不了「用户配过别的」与「用户删掉了它」，
        // 于是每次加载都把出厂绑定重新灌回去（见 `materialize_key_actions` 的文档）。
        //
        // 仍要清空 `trigger_keys`：`normalize()` 之后「trigger_keys 恒为空」是既有的
        // 后置条件，消费端一律读 `key_actions`（handle_temp.rs / handle_mode.rs 等处的
        // 注释均已声明不再读它）。留着非空值会让日后有人误以为它还是活的。
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
        // 特殊模式**不在此折算**：它的实例已不来自 `schema.special_modes`，引导键与直达
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
            // debug 而非 info：本函数是**每次 `Config::load` 都跑的内存内归一化**（折算结果
            // 不写回磁盘），故只要用户配置里还留着 trigger_keys，这两行就会在每一次读配置时
            // 原样重复。按 info 打印会让日志看起来像「同一件事被反复做了 N 遍」——2026-08-12
            // 就因此把「设置保存时 4 次配置全量重载」误判成服务端有冗余（实测每个 RPC 各加载
            // 一次，本就如此）。真正异常的两种情况（键名解析不了 / 多处争用同一键）仍是 warn。
            debug!("配置迁移：引导键 {key:?} → keys.key_actions = {action:?}");
            self.keys.key_actions.insert(key, action);
        }
    }

    /// 清空五处 `trigger_keys`（内存态）。
    ///
    /// 维持 `normalize()` 的后置条件「折算之后 trigger_keys 恒为空」——已物化的用户走的是
    /// 屏蔽分支，不再有 `drain` 把它们取走，须在此显式清掉。**只动内存**：磁盘上 L2 的
    /// `trigger_keys` 是出厂声明处（设置页「恢复默认」按 capability 读它），必须留着。
    fn clear_all_trigger_keys(&mut self) {
        self.input.temp_english.trigger_keys.clear();
        self.input.temp_pinyin.trigger_keys.clear();
        for m in self.schema.mix_modes.iter_mut() {
            m.trigger_keys.clear();
        }
    }

    /// 存量迁移：`trigger_keys` 里的单字母 → 方案级 [`BoundAction`]。
    ///
    /// 引导键曾接受任意 a-z（`key_name_to_vk_with_letters`），字母的特殊能力现已收归
    /// `schema.codetable.z_key_action`。**必须显式迁移**：解析端改成只认符号后，
    /// `filter_map` 会把留在配置里的字母**静默丢弃**——用户的功能无声消失，且配置文件里
    /// 那行还在，从现象完全看不出原因。
    ///
    /// `z` 折算成对应 action，其余字母只能丢弃（本项只管 z）——但要 `warn` 出来，
    /// 让日志里留下痕迹。
    ///
    /// 归属优先级与老的模式激活链一致（临拼 > 特殊模式 > mix）：z 同时配在多处时，
    /// 老实现里也是临拼先匹配。已显式配过 `z_key_action` 的不覆盖——用户的新配置优先。
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
    /// 快捷输入的四个来源（计算/日期/数字/重复）曾是一个不可分的成员，无法单独开关。
    /// 拆分后旧值在原位展开，顺序与展开序一致——存量用户的候选序不变，只是从此可增删。
    /// 对**所有** mix 生效（不限内置 quick_mix）：`"quick_input"` 是保留 id，任何 mix 里
    /// 出现都只可能是这个含义。
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
    /// 背景：`members` 从未开放给用户（无 UI、data/config.toml 无 mix_modes 段），但设置页改
    /// 「快捷输入激活键」时会把整个 mix_modes 数组连同 members 写回用户配置。故存量用户配置里的
    /// 字面 `"pinyin"` 必是旧默认值残留、而非「就要全拼」的用户意图，替换为占位符是安全的。
    /// 只认内置 quick_mix：用户自定义 mix 的字面 id 一律精确解释，不动。
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

    /// 用户配置目录（config.toml / userdata.redb / 词频 / shadow 置顶删词 / 用户词库）。
    /// - 便携模式：`<exe目录>/userdata/`
    /// - 自定义数据目录（安装向导选定，落 `datadir.conf`）：该目录本身
    /// - 正常模式：漫游 `%APPDATA%\WindInput[Dev]`（随用户在多设备间同步）
    ///
    /// 三者优先级即上述顺序。注意自定义目录**只影响本函数**——`local_dir()` 系
    /// （cache / logs / state.toml）不跟随，详见 `variant::custom_userdata_dir`。
    pub fn user_config_dir() -> Option<PathBuf> {
        if crate::variant::is_portable() {
            return crate::variant::portable_userdata_dir();
        }
        if let Some(d) = crate::variant::custom_userdata_dir() {
            return Some(d);
        }
        dirs::config_dir().map(|d| d.join(Self::app_dir_name()))
    }

    /// 探测用户配置目录当前是否可用。纯查询，无副作用、不重试。
    ///
    /// 判据刻意建在**漫游根目录**而非 `config.toml` 上：漫游根一旦可用，
    /// 「我们的目录/文件在不在」就是确定性事实（全新安装本就没有 config.toml，
    /// 它只在用户首次改设置时由 `set_user_value` 创建）。把判据建在文件上会让
    /// 每个全新用户白等一个完整超时。
    pub fn probe_user_config() -> UserConfigProbe {
        if crate::variant::is_portable() {
            return match crate::variant::portable_userdata_dir() {
                Some(d) => UserConfigProbe::Portable(d),
                None => UserConfigProbe::RoamingUnavailable,
            };
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
            // 看不到 config.toml，但本地标记说这用户此前确有配置：开机早期漫游
            // profile 还没挂载完的竞态。继续等，别退回系统预置。
            return UserConfigProbe::ConfigPending { dir };
        }
        UserConfigProbe::Ready {
            dir_exists: dir.is_dir(),
            file_exists,
            dir,
        }
    }

    /// 本地「用户配置曾存在」标记文件路径
    /// （`%LOCALAPPDATA%\WindInput[Dev]\user_config.seen`）。
    ///
    /// 放 `%LOCALAPPDATA%`（非漫游）是关键：它登录即挂载、不受漫游延迟影响
    /// （日志能写出就是证据），故能可靠仲裁那个「可能迟到」的漫游 `config.toml`。
    fn user_config_marker_path() -> Option<PathBuf> {
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
    /// **只应由服务启动路径调用一次**，绝不放进 [`load`](Self::load)：`load()` 在
    /// 热重载/RPC 上高频调用、且被单元测试直接执行，从中写盘会污染真实
    /// `%LOCALAPPDATA%`。写标记是「观察到真实用户配置」后的一次性副作用，
    /// 收敛在服务二进制里。
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
    /// 只应在服务启动早期调用一次，且必须在 logger 初始化之后——否则探测日志全部丢失。
    /// **不要**放进 `load()`：热重载与 RPC 也走 `load()`，在那些线程上阻塞会卡住输入。
    ///
    /// 超时后仍继续启动（降级为系统预置配置），不死等：输入法晚几秒可用尚可接受，
    /// 完全起不来不可接受。
    pub fn wait_user_config_ready(timeout: std::time::Duration) -> bool {
        Self::wait_until_settled(
            Self::probe_user_config,
            timeout,
            std::time::Duration::from_millis(250),
        )
    }

    /// [`Self::wait_user_config_ready`] 的可注入内核：探测源与轮询间隔都是参数。
    ///
    /// 抽出来是为了能测重试路径——真机上漫游根几乎总是就绪，重试分支在开发机
    /// 永远走不到，而它恰恰是这个修复的目的所在，不能靠「上真机重启一次」来验证。
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

    /// 用户覆盖命中时的统一日志打点。
    ///
    /// 「用户目录同名文件整体替代安装目录自带文件」这条能力散落在多个解析函数里
    /// （方案文件 / 词库 / 方案附属资源 / 双拼布局 / 主题 / 数据根文件），各函数的回退
    /// 级数还不一样。排查「同一版程序、这台机器行为和出厂不一致」时，唯一可靠的线索就是
    /// 「当时到底加载了哪个文件」——故所有解析点一律经此打点、共用同一措辞，便于按
    /// `覆盖生效` 一次 grep 出全部生效的覆盖（`wind-theme` 的 `用户覆盖生效[theme]` 也含
    /// 这个子串，故一条 grep 仍能全覆盖）。
    ///
    /// `kind` 是资源类别（`schema` / `dict` / `resource` / `shuangpin` / `theme` / `data`），
    /// `rel` 是方案/数据根下的相对路径。**只在命中用户层时调用**：未覆盖的默认安装
    /// 不产生任何日志，故日志里出现即异常排查线索。
    ///
    /// `shadowed` 区分命中用户层的两种情形，**不可省**：安装目录也有同名文件时才是真的
    /// 「覆盖自带数据」（记 info，排查目标）；安装目录没有时只是第三方方案自带资源走用户
    /// 目录（记 debug）。二者都打 info 的话，一个第三方方案的几十个词库会把真正的覆盖淹掉。
    ///
    /// 这是 [`Self::log_layer_override`] 的**用户层专用**入口，保留给自己实现多级回落
    /// 的解析点（`wind-engine` 的方案/词库解析）。多层解析请直接用带层名的那个。
    pub fn log_user_override(kind: &str, rel: &str, path: &Path, shadowed: bool) {
        Self::log_layer_override("user", kind, rel, path, shadowed);
    }

    /// 覆盖命中时的统一日志打点，`layer` 为命中的层名（`user` / `custom` / `data`）。
    ///
    /// 加层之后「覆盖生效」不再是二值的：同一个文件名可能来自用户层，也可能来自定制版
    /// 自带的 `data_custom/`。排查「这台机器行为和出厂不一致」时，只知道「被覆盖了」而不
    /// 知道**被谁**覆盖，等于把定制版和用户个人设置这两类完全不同的原因混成一团。
    ///
    /// 措辞统一为 `覆盖生效[层][类别]`，可按 `覆盖生效` 一次 grep 出全部生效的覆盖，
    /// 也可按 `覆盖生效[custom]` 只看定制版带来的差异。
    ///
    /// `layer == "data"`（出厂自带、无人覆盖）**不打点**：那是绝大多数文件的常态，
    /// 打了就是刷屏，且日志里"出现即线索"这条性质会随之失效。
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
    /// （方案类资源传 `Some("schemas")`，数据根文件传 `None`）。
    ///
    /// 层序来自 [`Self::resource_layers_with`]：`user > custom > data`。
    /// [`Self::resolve_data_file`] / [`Self::resolve_schema_resource`] 自动继承定制层，
    /// 这也是本计划改动量小的原因——单文件读取绝大部分都经过这里。
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
            // `shadowed` = 「确实盖住了更靠后的某一层」，判据必须看**全部**后续层：
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

    /// 解析**数据根**下的程序自带文件（`system.phrases.toml` / `pinyin_map.txt` 等）：
    /// 用户配置目录同名文件整体替代，回落安装目录 `data_dir/`。两处均无返回 None。
    ///
    /// 与 [`Self::resolve_schema_resource`] 的差别只在根少一层 `schemas/`。这类文件是
    /// **整体替换**语义（不做键级合并）——合并语义只有 `config.toml`（四层）与
    /// `compat.toml`（字段级）两处，它们各有专用加载器，不走本函数。
    pub fn resolve_data_file(data_dir: Option<&Path>, rel: &str) -> Option<PathBuf> {
        Self::resolve_overridable(data_dir, None, rel, "data")
    }

    /// 把单个配置项**部分合并**写入用户层 `config.toml`（%APPDATA%/WindInput/config.toml）。
    ///
    /// 只改 `path` 指定的项、保留用户文件里其它已有项，**不写入未改动的默认/系统段**——
    /// 用户层维持最小 diff，避免覆盖系统层/默认层的后续更新。
    /// 原子写（tmp + rename）。`path` 如 `["ui","candidate","preedit_display"]`。
    ///
    /// ★ **值等于出厂默认（L1⊕L2⊕L2.5）时删除该键，而不是写入**（见
    /// [`preset_for_pruning`](Self::preset_for_pruning)）。这条收口是上面那句「避免覆盖后续更新」
    /// 唯一的兑现方式：此前无论值是什么都照写，用户把开关点回默认位就在用户层留下一个显式值，
    /// 从此**永久钉死、不再跟随 L1/L2 的后续变更**。真机实测一份用户配置 105 个键里 62 个是这种
    /// 冗余键，其中 `schema.mix.auto_commit_block_on_pinyin` 已经引爆：它在默认值还是 `false` 的
    /// 版本被写入，之后默认改回 `true`，该用户却一直停在 `false`，顶码的拼音保护被静默卸掉。
    ///
    /// 语义取舍：加了这条之后，「用户显式选了与默认相同的值」无法与「跟随默认」区分。对配置系统
    /// 而言后者才是正确语义；若将来要支持「锁定某值不随升级变化」（pin），需要另设表达方式，
    /// **不要**靠退回「照原样写入」来实现——那等于把这 62 颗雷再埋回去。
    pub fn set_user_value(path: &[&str], value: toml::Value) -> anyhow::Result<()> {
        if path.is_empty() {
            anyhow::bail!("set_user_value: empty path");
        }
        let dir = Self::user_config_dir().context("no user config dir")?;
        std::fs::create_dir_all(&dir)?;
        let file = dir.join("config.toml");

        // 读现有用户层（partial），不存在/解析失败则空表（不丢已有项时尽量保留）。
        let mut root = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
            .unwrap_or_else(|| toml::Value::Table(Default::default()));
        if !root.is_table() {
            root = toml::Value::Table(Default::default());
        }
        // 供落盘后通知钩子用：下方 set_nested 会 move 掉 value。
        let value_for_hook = value.clone();
        // 出厂默认取不到时 `is_default` 恒 false → 退化为「照常写入」的旧行为（安全降级）。
        // `is_known_key` 与 `prune_redundant` 同一道保险：未登记键（废弃键 / Map 子路径）不收口。
        let is_default = crate::config_schema::is_known_key(&path.join("."))
            && Self::preset_for_pruning()
                .as_ref()
                .and_then(|p| get_nested(p, path))
                .is_some_and(|d| *d == value);
        if let toml::Value::Table(t) = &mut root {
            if is_default {
                remove_nested(t, path);
            } else {
                set_nested(t, path, value);
            }
        }

        let out = toml::to_string_pretty(&root)?;
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, &file)?;
        // 落盘成功后通知订阅方（设置界面等）。放在 rename 之后：失败路径提前 `?`
        // 返回，不会误报。传出的是入参值——键被剪枝删除时用户层虽无此键，生效值
        // 仍是它（见上方剪枝说明），对订阅方语义一致。
        crate::change_hook::notify_changed(path, &value_for_hook);
        Ok(())
    }

    /// [`set_user_value`](Self::set_user_value) 的字符串便捷形式。
    pub fn set_user_string(path: &[&str], value: &str) -> anyhow::Result<()> {
        Self::set_user_value(path, toml::Value::String(value.to_string()))
    }

    /// [`set_user_value`](Self::set_user_value) 的布尔便捷形式。
    pub fn set_user_bool(path: &[&str], value: bool) -> anyhow::Result<()> {
        Self::set_user_value(path, toml::Value::Boolean(value))
    }

    /// 运行时状态目录（state.toml：工具栏位置等本机状态）。
    /// 与 `local_dir()` 相同路径，独立命名便于语义区分。
    pub fn state_dir() -> Option<PathBuf> {
        Self::local_dir()
    }

    /// 本机状态目录（工具栏位置、日志、缓存等机器相关数据）。
    /// - 便携模式：`<exe目录>/userdata/`
    /// - 正常模式：`%LOCALAPPDATA%\WindInput[Dev]`（不随漫游同步）
    pub fn local_dir() -> Option<PathBuf> {
        if crate::variant::is_portable() {
            crate::variant::portable_userdata_dir()
        } else {
            dirs::data_local_dir().map(|d| d.join(Self::app_dir_name()))
        }
    }

    /// 缓存目录（%LOCALAPPDATA%\WindInput\cache）：词库 .wdat 等可重建产物。
    pub fn cache_dir() -> Option<PathBuf> {
        Self::local_dir().map(|d| d.join("cache"))
    }

    /// 日志目录。
    /// - 便携模式：`<exe目录>/userdata/logs`
    /// - 正常模式：`%LOCALAPPDATA%\WindInput[Dev]\logs`
    pub fn log_dir() -> Option<PathBuf> {
        if crate::variant::is_portable() {
            crate::variant::portable_userdata_dir().map(|d| d.join("logs"))
        } else {
            Self::local_dir().map(|d| d.join("logs"))
        }
    }

    /// 获取 data 目录（安装根目录下的 `data/`，正常即可执行文件同目录）。
    ///
    /// 根走 [`crate::variant::install_root`]——那里带一个仅供测试的注入点，见其文档。
    pub fn data_dir() -> Option<PathBuf> {
        crate::variant::install_root().map(|d| d.join("data"))
    }

    /// 定制版清单（`data_custom/custom.toml`）。`None` = 本机不是定制版。
    ///
    /// **判据是清单文件可解析，不是 `data_custom/` 目录存在。** 隐式契约没有编译期约束，
    /// 本仓已有 `datadir.conf` 整段断链（写端完整、读端一行不读，骗了用户半年）的前车之鉴；
    /// 清单同时充当定制版身份（日志/关于页/报障）、减法清单与版本兼容判据。
    ///
    /// ⚠️ **解析失败 ⇒ 整个 custom 层不启用，而不是「半启用」。** 半解析出来的清单可能
    /// 丢掉 `hide` 名单，于是定制版里本该被删掉的方案又冒出来，用户看到的是一堆自己
    /// 从没见过的方案混在里面——诡异、难以归因。而「完全变回原版」这个现象足够明显，
    /// 用户会立刻报障，故宁可整层退场。
    ///
    /// OnceLock 缓存：进程内只读一次盘。⚠️ 同一进程内改不了取值，故相关测试必须各占
    /// 一个测试二进制（`tests/*.rs` 每个文件一个进程，见 `tests/datadir_conf.rs`）。
    pub fn custom_manifest() -> Option<&'static CustomManifest> {
        static MANIFEST: OnceLock<Option<CustomManifest>> = OnceLock::new();
        MANIFEST.get_or_init(load_custom_manifest).as_ref()
    }

    /// 定制版数据层目录（`<安装根>/data_custom`）。**清单在场且可解析时才返回 `Some`**。
    ///
    /// 层序固定为 `data < data_custom < %APPDATA%`：`data_custom` 在安装目录下，
    /// 普通用户无写权限，只承担「随安装包分发的定制」职责；用户个人调整仍走 `%APPDATA%`。
    /// **程序对本目录只读**，退役键清理、配置剪枝一律只作用于用户层。
    pub fn custom_data_dir() -> Option<PathBuf> {
        Self::custom_manifest()?;
        crate::variant::install_root().map(|d| d.join(CUSTOM_DATA_DIR_NAME))
    }

    /// 本定制版是否把方案 `id`「删掉了」（`data_custom/custom.toml` 的 `[schemas] hide`）。
    ///
    /// ★ **与 `[schema].hidden` 是两个正交的轴，不得合并**（见 [`CustomManifest::schemas`]）：
    /// `hidden` = 「不列进方案切换列表」，english / 快符仍可用、仍被 mix 引用、仍能被
    /// `schema.active` 指到；本判据 = 「这个方案在本定制版里**不存在**」。拿 `hidden` 实现
    /// 减法会让被删掉的方案继续被 mix / special_modes / `schema.active` 引用到。
    ///
    /// ★ **hide 是绝对的：被 hide 的 id 在任何层都不存在**，`data/` 与 `data_custom/` 之外，
    /// 也包括用户自己放在 `%APPDATA%\WindInput\schemas\` 里的同名文件。理由是契约 5 的措辞
    /// 「这个方案在本定制版里不存在」——若 hide 只对安装层生效，用户层放一个同名文件就能
    /// 让被删掉的方案复活，定制者的意图落空，而判定还得多带一个「命中的是哪一层」的分支。
    /// **代价（明写在这里，别留白）**：用户无法用被 hide 的 id 给自己的方案命名——他放的
    /// `wubi86.schema.toml` 会连同被删的内置方案一起消失，且现象与「文件没放对」难以区分。
    /// 定制者因此应当只 hide 自己确实想删掉的**内置** id。
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
    /// 这是**唯一**该被枚举点使用的层序来源。目录枚举（方案 / 主题 / 双拼布局 / opencc）
    /// 各自维护一份自己的目录列表，漏接一处的现象是「定制版里那个方案静默不见了」——
    /// 而这类缺陷在没有 `data_custom` 的开发机上永远复现不出来。用法示例：
    ///
    /// ```ignore
    /// // 原来：两个目录写死
    /// let dirs = [user_dir.join("schemas"), data_dir.join("schemas")];
    /// // 改后：层序统一，自动带上 custom 层
    /// let dirs: Vec<PathBuf> = Config::resource_layers_with(Some(data_dir))
    ///     .into_iter()
    ///     .map(|d| d.join("schemas"))
    ///     .collect();
    /// ```
    ///
    /// data 层取 [`Self::data_dir`]；调用方手里已有 data 目录（便携/测试会传自定义路径）
    /// 时用 [`Self::resource_layers_with`]。
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

    /// 同 [`Self::resource_layers`]，但**带层名**（`user` / `custom` / `data`）。
    ///
    /// 层名不是装饰：解析点要用它打 [`Self::log_layer_override`]（「被谁覆盖」是排查
    /// 定制版行为差异的第一个问题），枚举点要用它区分「内置 vs 用户自带」（主题列表的
    /// builtin 标记、方案可删判定）。**没有这个公开形态，每个枚举点都会自己猜层名**
    /// ——猜法一旦分叉，日志里的 `custom` 就不再指同一件事。
    pub fn resource_layers_named() -> Vec<ResourceLayer> {
        Self::resource_layers_named_with(Self::data_dir().as_deref())
    }

    /// 带层名的层序，data 层由调用方指定（`None` = 无 data 层，只剩 user/custom）。
    ///
    /// custom 层刻意**不**由 `data_dir` 推导（如取它的兄弟目录）：清单的 OnceLock 缓存
    /// 是进程级的，若目录随调用方参数变而清单不变，两者会失配——那种不一致只在传了
    /// 自定义 data 目录的场合出现，最难查。这条也是 `None` 的正当用法：调用方手里只有
    /// 「data 层的某个子目录」（如 `wind-engine` 的 `schemas_dir`）时，用 `None` 取
    /// user/custom 两层、再把自己那份 data 层接到末尾，**好过从子目录 `parent()` 反推
    /// 数据根**——反推是把层的兄弟关系埋进一个隐式契约里。
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

    /// 追溯一个配置键的来源：四层各声明了什么、最终生效的是哪一层的值。
    ///
    /// 回答的是**「我改的这个值为什么不生效」**——此前唯一的答案来源是 `config get`
    /// 的最终值，而它恰好不包含「为什么」。四层里任何一层写了同名键都会静默改变结果，
    /// 定制版还多夹一层（`data_custom`），排查时无从下手。
    ///
    /// # 语义要点
    ///
    /// - **层的值是「该层显式写了什么」，不是「该层生效后是什么」。** 表类型逐键深合并，
    ///   单层声明常常只是最终值的一部分——此时 `effective_layer` 为 `None`，因为确实
    ///   指不到某一层。
    /// - **`effective` 取自真实的 [`Self::load`]**，含 `normalize()` 的归一化改写；
    ///   段级降级发生时它是出厂默认，而 `degraded` 会告诉你用户值被丢了。
    /// - 键不存在于任何层（含默认层）时四层全 `None`——调用方应当先用
    ///   `config_schema::is_known_key` 挡掉拼错的键，本函数不认识注册表。
    ///
    /// # 代价
    ///
    /// 每次调用做 4 次文件读 + 一次完整 `load()`。这是**排查用的单次查询**，不要放进
    /// 热路径；需要批量时应当另做一个复用这一次 `load()` 的形态。
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
mod tests {
    use super::*;

    /// ★ 语法模型的默认值必须是「什么都不发生」：`weight = 0` **且** `model` 为空。
    ///
    /// 守的是一个真实发生过的隐患：`model` 的默认值一度是 `zh-hans-bgw.gram`，
    /// 于是用户「只把 weight 调成非 0」就会静默启用它——而该模型在 192 条整句
    /// 评测上实测为 **−4**（设计文档 §8）。默认值把人导向一个质量为负的模型，
    /// 且当事人完全不知情。
    ///
    /// 谁要改回某个具体模型名，先解释清楚为什么默认开启它是安全的。
    #[test]
    fn grammar_defaults_to_disabled_and_no_model() {
        let g = PinyinGrammar::default();
        assert_eq!(g.weight, 0.0, "默认必须关闭");
        assert!(
            g.model.is_empty(),
            "默认模型必须为空，否则只调 weight 就会静默启用它，实得 {:?}",
            g.model
        );
        // 从空配置反序列化（用户没写这一段）也要落到同一处
        let from_empty: PinyinGrammar = toml::from_str("").expect("空表可反序列化");
        assert_eq!(from_empty, g, "缺省反序列化须与 Default 一致");
    }

    /// 注释库的方案限定：`schemas` 留空适用于全部方案，非空则只在列出的方案下加载。
    ///
    /// 「留空=全部」这个方向不能反：反过来的话，用户手写一条却没写 `schemas` 就是
    /// 「配了完全没反应」，而那是本仓反复出现的最难自查的一类故障。
    #[test]
    fn comment_dict_schema_scoping() {
        let global = CommentDictSpec::default();
        assert!(global.applies_to("wubi86"), "留空适用于任意方案");
        assert!(global.applies_to(""), "空方案 id 也算适用");

        let scoped = CommentDictSpec {
            schemas: vec!["english".into(), "pinyin".into()],
            ..Default::default()
        };
        assert!(scoped.applies_to("english"));
        assert!(scoped.applies_to("pinyin"));
        assert!(
            !scoped.applies_to("wubi86"),
            "未列出的方案不得加载——挂大英汉词典在五笔下查是纯浪费，这正是本字段的目的"
        );
        assert!(
            !scoped.applies_to("English"),
            "方案 id 区分大小写，按精确匹配"
        );
    }

    #[test]
    fn z_key_action_parses_value_domain() {
        assert_eq!(BoundAction::parse(""), BoundAction::None);
        assert_eq!(BoundAction::parse("none"), BoundAction::None);
        assert_eq!(BoundAction::parse(" TEMP_PINYIN "), BoundAction::TempPinyin);
        assert_eq!(BoundAction::parse("temp_english"), BoundAction::TempEnglish);
        assert_eq!(BoundAction::parse("AUX_CODE"), BoundAction::AuxCode);
        assert_eq!(
            BoundAction::parse("mix:quick_mix"),
            BoundAction::Mix("quick_mix".into())
        );
        assert_eq!(
            BoundAction::parse("special:rare"),
            BoundAction::Special("rare".into())
        );
        // 未知值绝不静默变成别的功能。
        assert_eq!(BoundAction::parse("enter_temp_pinyin"), BoundAction::None);
        assert_eq!(BoundAction::parse("quick_input"), BoundAction::None);
        // 空 id 无从定位目标，等同不启用（消费端也会被门卫挡下，此处提前收敛）。
        assert_eq!(BoundAction::parse("mix:"), BoundAction::None);
        assert_eq!(BoundAction::parse("special:  "), BoundAction::None);
        // id 大小写敏感（与 special_mode_idx / mix_mode_idx 的精确匹配同口径）。
        assert_eq!(
            BoundAction::parse("mix:Quick_Mix"),
            BoundAction::Mix("Quick_Mix".into())
        );
    }

    /// 出厂默认（`page_keys = [pageupdown, minus_equal]` / `highlight_keys = [arrows, tab]`）
    /// 折算后必须逐键等价于旧 `NavKeys::from_config` 的产物——否则升级即回归。
    #[test]
    fn nav_keys_fold_into_session_actions_preserving_defaults() {
        let c = Config::default();
        let sa = c.keys.effective_session_actions();
        for (key, want) in [
            ("pageup", "page_prev"),
            ("pagedown", "page_next"),
            ("minus", "page_prev"),
            ("equal", "page_next"),
            ("up", "highlight_up"),
            ("down", "highlight_down"),
            ("tab", "highlight_down"),
            ("shift+tab", "highlight_up"),
        ] {
            assert_eq!(
                sa.get(key).map(String::as_str),
                Some(want),
                "默认绑定 {key} 折算后应为 {want}，实际 {:?}",
                sa.get(key)
            );
        }
        // ★★ 存储层必须保持原样：设置页的四个勾选框读的正是它。曾经在 normalize 里
        // clear 掉，后果是出厂默认在界面上全显示为未勾选（每个用户都会遇到）。
        assert_eq!(
            c.keys.page_keys,
            default_page_keys(),
            "折算是消费层的视图，不得改写存储层"
        );
        assert_eq!(c.keys.highlight_keys, default_highlight_keys());
    }

    /// 出厂默认的 `select_key_groups = ["semicolon_quote"]` 折算成两条选词绑定。
    #[test]
    fn default_select_key_group_folds_into_session_actions() {
        let c = Config::default();
        let sa = c.keys.effective_session_actions();
        assert_eq!(
            sa.get("semicolon").map(String::as_str),
            Some("select_candidate:2"),
            "`;` 应折算成次选键"
        );
        assert_eq!(
            sa.get("quote").map(String::as_str),
            Some("select_candidate:3"),
            "`'` 应折算成三选键"
        );
        assert_eq!(
            c.keys.select_key_groups,
            default_select_key_groups(),
            "折算不得改写存储层"
        );
    }

    /// 修饰键选词组（走 keyup 轻敲）折算成四个纯修饰键名。
    ///
    /// ⚠️ 这些键名必须同时被 `hotkey::session_key_to_vk` 与
    /// `wind_keys::keymap::session_key_name_to_vk` 认得——一期漏了后者，三期补上，
    /// 由 wind-coordinator 的跨 crate 测试守。
    #[test]
    fn modifier_select_group_folds_to_modifier_key_names() {
        let mut c = Config::default();
        c.keys.select_key_groups = vec!["lrctrl".into()];
        let sa = c.keys.effective_session_actions();
        assert_eq!(
            sa.get("lctrl").map(String::as_str),
            Some("select_candidate:2")
        );
        assert_eq!(
            sa.get("rctrl").map(String::as_str),
            Some("select_candidate:3")
        );
        // 每个折算出的键名都必须解析得回来，否则绑定静默失效。
        for name in ["lctrl", "rctrl"] {
            assert!(
                crate::hotkey::session_key_to_vk(name).is_some(),
                "{name} 解析不出 VK，绑定会静默失效"
            );
        }
    }

    /// 以词定字组折算，含 `brackets`——它**不在**选词键组的值域里。
    ///
    /// 回归点：两组曾被张冠李戴（用选词键组的解析器解以词定字配置），`brackets` 静默失效。
    #[test]
    fn select_char_group_folds_including_brackets() {
        let mut c = Config::default();
        c.keys.select_char_keys = vec!["brackets".into()];
        let sa = c.keys.effective_session_actions();
        assert_eq!(
            sa.get("lbracket").map(String::as_str),
            Some("select_char:1")
        );
        assert_eq!(
            sa.get("rbracket").map(String::as_str),
            Some("select_char:2")
        );
        assert_eq!(
            c.keys.select_char_keys,
            vec!["brackets".to_string()],
            "折算不得改写存储层"
        );
    }

    /// ★★ 撞键裁决：`comma_period` 同时配给以词定字与选词时，**以词定字赢**。
    ///
    /// 这不是随意选的——主输入路径上 `select_char_index` 的判定在 `select_key_offset`
    /// 之前，折算顺序必须复现那个顺序。搞反了的表现是「一直用的 `,` 突然从取字变成选次选」，
    /// 而用户什么都没改。
    #[test]
    fn select_char_wins_when_group_claimed_by_both() {
        let mut c = Config::default();
        c.keys.select_key_groups = vec!["comma_period".into()];
        c.keys.select_char_keys = vec!["comma_period".into()];
        assert_eq!(
            c.keys
                .effective_session_actions()
                .get("comma")
                .map(String::as_str),
            Some("select_char:1"),
            "撞键时以词定字应赢——它在消费链上更靠前"
        );
    }

    /// 序号越界 / 非数字的载荷落 `None`，不静默收下一个永不生效的绑定。
    #[test]
    fn select_ordinals_are_range_checked() {
        for bad in [
            "select_candidate:0",
            "select_candidate:99",
            "select_candidate:x",
            "select_candidate:",
            "select_char:0",
        ] {
            assert_eq!(
                SessionAction::parse(bad),
                SessionAction::None,
                "{bad} 应被拒绝"
            );
            assert!(
                SessionAction::parse_checked(bad).is_none(),
                "{bad} 应被报成无法识别，而不是静默当成 none"
            );
        }
        assert_eq!(
            SessionAction::parse("select_candidate:2"),
            SessionAction::SelectCandidate(2)
        );
        // Display 与 parse 互为逆运算——写回读不回来是「配置丢了」那类问题的根源。
        for a in [
            SessionAction::SelectCandidate(3),
            SessionAction::SelectChar(1),
        ] {
            assert_eq!(SessionAction::parse(&a.to_string()), a);
        }
    }

    /// ★ page 组优先于 highlight 组——复现 `NavKeys::classify` 的 `.find()` 语义。
    ///
    /// 两组都声明 `tab` 时（`page_keys=[shift_tab]` + 默认 `highlight_keys=[…, tab]`），
    /// 旧实现按「page 全部 push 完再 push highlight」建表，`.find()` 取到 page 那条。
    /// 搞反了的表现是「一直用的 Tab 突然从翻页变成移高亮」，用户什么都没改。
    #[test]
    fn nav_fold_gives_page_group_priority_over_highlight() {
        let mut c = Config::default();
        c.keys.page_keys = vec!["shift_tab".into()];
        c.keys.highlight_keys = vec!["tab".into()];
        let sa = c.keys.effective_session_actions();
        assert_eq!(
            sa.get("tab").map(String::as_str),
            Some("page_next"),
            "tab 两组都配时 page 组应赢"
        );
        assert_eq!(sa.get("shift+tab").map(String::as_str), Some("page_prev"));
    }

    /// 用户**显式**写在 `session_actions` 里的键不被折算覆盖：新配置优先于存量迁移。
    /// 这是「用户要 Tab 清空、但 highlight_keys 默认还带着 tab」时唯一正确的裁决。
    #[test]
    fn explicit_session_action_survives_nav_fold() {
        let mut c = Config::default();
        c.keys
            .session_actions
            .insert("tab".into(), "page_next".into());
        assert_eq!(
            c.keys
                .effective_session_actions()
                .get("tab")
                .map(String::as_str),
            Some("page_next"),
            "显式配置应压过默认 highlight_keys 折算出的 highlight_down"
        );
    }

    /// ★★ 用户把 `page_keys` 清成 `[]` 的**意图不能丢**。
    ///
    /// 这正是「默认值必须留在被折算的那一侧」的理由：若把出厂绑定直接写进
    /// `session_actions`，空数组就与「从没配过」同形，折算跳过而默认绑定仍在——
    /// 用户明明关掉了翻页键，重启后又回来了。五c 折算 `trigger_keys` 时踩过。
    #[test]
    fn cleared_page_keys_yield_no_paging_binds() {
        let mut c = Config::default();
        c.keys.page_keys = vec![];
        c.keys.highlight_keys = vec![];
        // 选词键组默认非空（semicolon_quote），一并清掉才测得出「清空即无绑定」。
        c.keys.select_key_groups = vec![];
        c.keys.select_char_keys = vec![];
        let sa = c.keys.effective_session_actions();
        assert!(sa.is_empty(), "全部清空时不应折算出任何绑定，实际 {sa:?}");
    }

    /// ★★★ `normalize` **不得**碰这两处——折算是消费层的视图，不是存储层的改写。
    ///
    /// 2026-08-11 回归守门：曾在 normalize 里折算并 clear 四个原字段，后果是设置页读到的
    /// 四项恒为空（出厂默认非空，每个用户都遇到）、勾选后重开又变空、在高级表里删掉一条
    /// 折算来的绑定下次又被折算回来。判据见 `effective_session_actions` 的文档。
    #[test]
    fn normalize_does_not_rewrite_key_group_storage() {
        let mut c = Config::default();
        let before_groups = (
            c.keys.page_keys.clone(),
            c.keys.highlight_keys.clone(),
            c.keys.select_key_groups.clone(),
            c.keys.select_char_keys.clone(),
        );
        let before_explicit = c.keys.session_actions.clone();
        c.normalize();
        assert_eq!(
            (
                c.keys.page_keys.clone(),
                c.keys.highlight_keys.clone(),
                c.keys.select_key_groups.clone(),
                c.keys.select_char_keys.clone(),
            ),
            before_groups,
            "四组键组配置是存储层，normalize 不得清空"
        );
        assert_eq!(
            c.keys.session_actions, before_explicit,
            "session_actions 只存用户显式配的，normalize 不得往里塞折算结果——             否则高级表里会冒出一堆用户没配过的绑定，且删了下次又回来"
        );
    }

    /// 有效视图是纯函数：多次调用结果一致，且与调用前后的 normalize 无关。
    #[test]
    fn effective_session_actions_is_pure() {
        let mut c = Config::default();
        let a = c.keys.effective_session_actions();
        c.normalize();
        let b = c.keys.effective_session_actions();
        assert_eq!(a, b);
    }

    /// ⚠️ 组名展开表里的键名是**跨 crate 拼写契约**（消费端在 `wind-keys` 与
    /// `hotkey.rs` 各有一份解析）。这里只能守住「本 crate 自己认得」这一半：
    /// 写错的键名会让绑定解析不出 VK，表现为「升级后翻页键全没了」且无任何报错。
    /// 另一半（两份解析表一致）由 `wind-coordinator` 的跨 crate 测试守。
    #[test]
    fn nav_group_names_resolve() {
        for g in [
            "pageupdown",
            "minus_equal",
            "brackets",
            "comma_period",
            "shift_tab",
        ] {
            assert_eq!(
                page_key_group_binds(g).len(),
                2,
                "page 组 {g} 应展开出两个键"
            );
        }
        for g in ["arrows", "tab"] {
            assert_eq!(
                highlight_key_group_binds(g).len(),
                2,
                "highlight 组 {g} 应展开出两个键"
            );
        }
        assert!(page_key_group_binds("nonexistent").is_empty());
        // 每个折算出的动词都必须解析得回来——写错动词字面量会静默落 None。
        for (_, a) in page_key_group_binds("shift_tab") {
            assert!(SessionAction::parse(&a.to_string()).is_enabled());
        }
    }

    /// `clear` 是 `cancel` 的别名，且**只有一个规范名**回写。
    ///
    /// 两个名字对应两种心智（「清空」vs「取消」），但内核只有一种行为——否则就成了
    /// 「两个名字行为微妙不同」这种最难查的配置陷阱。回写只用 `cancel`，避免同一份配置
    /// 在两次保存后出现两种写法。
    #[test]
    fn clear_parses_as_cancel_alias() {
        assert_eq!(SessionAction::parse("cancel"), SessionAction::Cancel);
        assert_eq!(SessionAction::parse("clear"), SessionAction::Cancel);
        assert_eq!(SessionAction::parse("CLEAR"), SessionAction::Cancel);
        assert_eq!(SessionAction::Cancel.to_string(), "cancel");
        // 别名也要能通过「值认不认识」的校验，否则加载期会误报成拼写错误。
        assert_eq!(
            SessionAction::parse_checked("clear"),
            Some(SessionAction::Cancel)
        );
    }

    /// ★ 「要不要有候选」的判据挂在**动作**上，不在消费点。
    ///
    /// 导航类在没有候选时无事可做；`cancel` 则在「打了码还没出候选」时恰恰必须生效——
    /// 网址模式（原样累积文本、从不产候选）就是这一格。判据写到消费点上就要维护三份
    /// 一致的守卫，那正是本仓栽过四次的形状。
    #[test]
    fn only_navigation_actions_require_candidates() {
        for a in [
            SessionAction::PagePrev,
            SessionAction::PageNext,
            SessionAction::HighlightUp,
            SessionAction::HighlightDown,
        ] {
            assert!(a.requires_candidates(), "{a:?} 是导航类，无候选时无事可做");
        }
        assert!(
            !SessionAction::Cancel.requires_candidates(),
            "cancel 在有编码无候选时必须生效，否则网址模式里按了没反应"
        );
    }

    /// 存量迁移：`trigger_keys` 里的 z → `z_key_action`，其余字母丢弃。
    ///
    /// 不迁移的后果是**静默失效**：解析端只认符号后 `filter_map` 会把字母无声吃掉，
    /// 配置文件里那行还在，用户完全看不出功能为什么没了。
    #[test]
    fn migrate_letter_trigger_keys_moves_z_to_action() {
        let mut c = Config::default();
        c.input.temp_pinyin.trigger_keys = vec!["backtick".into(), "z".into(), "q".into()];
        c.normalize();

        assert_eq!(
            c.schema.codetable.z_key_action, "temp_pinyin",
            "z 应折算成 z_key_action"
        );
        // 字母项被本步摘除；符号项随后由五c 的收编迁移移进 keys.key_actions
        // （`normalize` 里两步依次跑）。断言它**去了哪**而不是「还在原地」——
        // 后者在收编后是个必然失败的观察点，但字母摘除这条契约本身没变。
        assert!(
            c.input.temp_pinyin.trigger_keys.is_empty(),
            "符号项应已被收编迁移取走"
        );
        assert_eq!(
            c.keys.key_actions.get("backtick").map(String::as_str),
            Some("temp_pinyin"),
            "符号引导键应完整折算进 keys.key_actions，不能在两步之间丢失"
        );
        assert!(
            !c.keys.key_actions.contains_key("q"),
            "被摘除的字母不该跟着进新表"
        );
    }

    /// 归属优先级与老的模式激活链一致：临拼 > mix。
    ///
    /// （原先链中的「特殊模式」一环已随 `schema.special_modes` 废弃——z 要进 overlay
    /// 方案改为直接写 `z_key_action = "special:<方案id>"`，不再有可折算的来源。）
    #[test]
    fn migrate_letter_trigger_keys_follows_activation_priority() {
        let mut c = Config::default();
        c.input.temp_pinyin.trigger_keys = vec!["z".into()];
        c.schema.mix_modes = vec![MixModeConfig {
            id: "mx".into(),
            trigger_keys: vec!["z".into()],
            ..Default::default()
        }];
        c.normalize();

        assert_eq!(
            c.schema.codetable.z_key_action, "temp_pinyin",
            "同时配在多处时按激活链取临拼（老实现也是临拼先匹配）"
        );
        assert!(
            c.schema.mix_modes[0].trigger_keys.is_empty(),
            "未中选的字母项同样要摘除，否则留在配置里也是静默失效"
        );
    }

    /// 五c 迁移：三处 `trigger_keys` 折算进 `keys.key_actions`。
    ///
    /// 原为四处——`schema.special_modes[]` 那一处已随该键废弃而消失（实例集合改由带
    /// `[overlay]` 段的方案定义，引导键直接写 `keys.key_actions`）。
    #[test]
    fn migrate_trigger_keys_folds_all_four_sources() {
        let mut c = Config::default();
        c.input.temp_pinyin.trigger_keys = vec!["backtick".into()];
        c.input.temp_english.trigger_keys = vec!["quote".into()];
        c.schema.mix_modes = vec![MixModeConfig {
            id: "quick_mix".into(),
            trigger_keys: vec!["semicolon".into()],
            ..Default::default()
        }];
        c.normalize();

        let ka = &c.keys.key_actions;
        assert_eq!(ka.get("backtick").map(String::as_str), Some("temp_pinyin"));
        assert_eq!(ka.get("quote").map(String::as_str), Some("temp_english"));
        assert_eq!(
            ka.get("semicolon").map(String::as_str),
            Some("mix:quick_mix")
        );

        // 旧字段清空——留着也是静默失效，而用户会以为它还生效。
        assert!(c.input.temp_pinyin.trigger_keys.is_empty());
        assert!(c.input.temp_english.trigger_keys.is_empty());
        assert!(c.schema.mix_modes[0].trigger_keys.is_empty());
    }

    /// ★★ 争用同一个键时，归属必须复现 `try_activate_mode` 的**真实**调用顺序：
    /// 临英 > 临拼 > special > mix。
    ///
    /// 设计文档 §1 曾把顺序写成「临英 > 快捷输入 > 临拼 > 特殊模式」，照那个迁移会让
    /// 「`;` 同时配给 mix 和临拼」的用户升级后进错模式——现象是「一直用的键突然变了
    /// 功能」，极难联想到是迁移干的。以代码为准。
    #[test]
    fn migrate_trigger_keys_follows_real_activation_order() {
        let mut c = Config::default();
        // 同一个 `;` 配给四处，按真实顺序应归临英。
        c.input.temp_english.trigger_keys = vec!["semicolon".into()];
        c.input.temp_pinyin.trigger_keys = vec!["semicolon".into()];
        c.schema.mix_modes = vec![MixModeConfig {
            id: "mx".into(),
            trigger_keys: vec!["semicolon".into()],
            ..Default::default()
        }];
        c.normalize();
        assert_eq!(
            c.keys.key_actions.get("semicolon").map(String::as_str),
            Some("temp_english"),
            "临英在激活链最前，应赢下争用"
        );

        // 单独验临拼 > mix：去掉临英那一处。
        let mut c2 = Config::default();
        c2.input.temp_pinyin.trigger_keys = vec!["grave".into()];
        c2.schema.mix_modes = vec![MixModeConfig {
            id: "mx".into(),
            trigger_keys: vec!["grave".into()],
            ..Default::default()
        }];
        c2.normalize();
        assert_eq!(
            c2.keys.key_actions.get("grave").map(String::as_str),
            Some("temp_pinyin"),
            "临拼排在 mix 之前"
        );
    }

    /// 用户已在 `keys.key_actions` 显式配过的键不被存量迁移覆盖。
    #[test]
    fn migrate_trigger_keys_does_not_override_explicit() {
        let mut c = Config::default();
        c.keys
            .key_actions
            .insert("backtick".into(), "temp_english".into());
        c.input.temp_pinyin.trigger_keys = vec!["backtick".into()];
        c.normalize();
        assert_eq!(
            c.keys.key_actions.get("backtick").map(String::as_str),
            Some("temp_english"),
            "用户的新配置优先于存量迁移"
        );
        // 旧字段仍要清空：它已经不可能生效了。
        assert!(c.input.temp_pinyin.trigger_keys.is_empty());
    }

    /// 空串项与解析不了的键名都不该进新表——前者是历史占位，后者是脏数据。
    #[test]
    fn migrate_trigger_keys_skips_empty_and_unparsable() {
        let mut c = Config::default();
        c.schema.mix_modes = vec![MixModeConfig {
            id: "mx".into(),
            trigger_keys: vec!["".into(), "  ".into(), "根本不是键".into()],
            ..Default::default()
        }];
        c.normalize();
        assert!(
            !c.keys.key_actions.values().any(|v| v == "mix:mx"),
            "空串/无法解析的键名不该进表，实际 {:?}",
            c.keys.key_actions
        );
    }

    /// 残留的 `schema.special_modes` **能被读出来**（供告警），且 `normalize` 后清空。
    ///
    /// ★ 「读得出」这一条是关键：字段若直接删掉，serde 会静默丢弃整段——用户 config.toml
    /// 里那几行还在、看起来仍然生效，实际早已无人消费。本仓反复栽过的「配了没反应」正是
    /// 这个形状，故这里用 `Vec<toml::Value>` 把它接住，只为发一条 warn。
    #[test]
    fn legacy_special_modes_is_read_then_cleared_with_warning() {
        let toml_src = r#"
[schema]
active = "wubi86"

[[schema.special_modes]]
id = "kf"
schema = "kf"
show_all_on_enter = true
"#;
        let mut c: Config = toml::from_str(toml_src).expect("含废弃段的配置仍须解析成功");
        assert_eq!(
            c.schema.legacy_special_modes.len(),
            1,
            "废弃段必须读得出来，否则无法告警"
        );
        c.normalize();
        assert!(
            c.schema.legacy_special_modes.is_empty(),
            "告警后须清空——留着会让后续「有没有配过」的判断读到假信号"
        );
        // 且**不做**任何迁移：不得凭空往 key_actions 里塞绑定（见设计文档 §5）。
        assert!(
            !c.keys
                .key_actions
                .values()
                .any(|v| v.starts_with("special:")),
            "本轮刻意不迁移，实际 {:?}",
            c.keys.key_actions
        );
    }

    /// 废弃字段不再写出：`skip_serializing_if` 保证保存后 config.toml 里不会又冒出来。
    #[test]
    fn legacy_special_modes_is_not_serialized_back() {
        let c = Config::default();
        let out = toml::to_string(&c).expect("序列化");
        assert!(!out.contains("special_modes"), "废弃字段不该写回配置文件");
    }

    /// 已显式配过 `z_key_action` 时，存量迁移不得覆盖用户的新配置。
    #[test]
    fn migrate_letter_trigger_keys_does_not_override_explicit() {
        let mut c = Config::default();
        c.schema.codetable.z_key_action = "temp_english".into();
        c.input.temp_pinyin.trigger_keys = vec!["z".into()];
        c.normalize();

        assert_eq!(
            c.schema.codetable.z_key_action, "temp_english",
            "显式配置优先于存量迁移"
        );
        assert!(
            c.input.temp_pinyin.trigger_keys.is_empty(),
            "旧字母项无论是否中选都要摘除"
        );
    }

    /// 内置「快捷」默认成员用占位符，使快捷输入的拼音跟随主拼音方案（而非恒为全拼）。
    #[test]
    fn quick_mix_default_members_use_primary_pinyin_placeholder() {
        let modes = default_mix_modes();
        let quick = modes
            .iter()
            .find(|m| m.id == QUICK_MIX_ID)
            .expect("应有内置 quick_mix");
        assert!(
            quick
                .members
                .contains(&MIX_MEMBER_PRIMARY_PINYIN.to_string()),
            "默认成员应为占位符，实际 {:?}",
            quick.members
        );
        assert!(
            !quick.members.contains(&DEFAULT_PINYIN_SCHEMA.to_string()),
            "不应再硬编码字面 pinyin，实际 {:?}",
            quick.members
        );
    }

    /// 存量迁移：改过「快捷输入激活键」的用户配置里，members 被整体写回为字面 pinyin，
    /// 加载期须迁成占位符，否则这些用户的快捷输入永远是全拼。
    #[test]
    fn normalize_migrates_quick_mix_literal_pinyin() {
        let mut cfg = Config::default();
        cfg.schema.mix_modes = vec![
            MixModeConfig {
                id: QUICK_MIX_ID.to_string(),
                members: vec![
                    "quick_input".to_string(),
                    "pinyin".to_string(),
                    "english".to_string(),
                ],
                ..Default::default()
            },
            // 用户自定义 mix：字面 id 精确解释，不迁移。
            MixModeConfig {
                id: "my_mix".to_string(),
                members: vec!["pinyin".to_string()],
                ..Default::default()
            },
        ];
        cfg.normalize();
        assert!(
            cfg.schema.mix_modes[0]
                .members
                .contains(&MIX_MEMBER_PRIMARY_PINYIN.to_string()),
            "内置 quick_mix 的字面 pinyin 应迁为占位符，实际 {:?}",
            cfg.schema.mix_modes[0].members
        );
        assert_eq!(
            cfg.schema.mix_modes[1].members,
            vec!["pinyin"],
            "自定义 mix 的字面 pinyin 应原样保留"
        );
    }

    /// 在合并值里塞一个存量用户配置残留的 `schema.quick_input.force_vertical`。
    fn merged_with_force_vertical(v: Option<bool>) -> toml::Value {
        let mut merged = toml::Value::try_from(Config::default()).expect("默认配置应可序列化");
        if let Some(v) = v {
            merged
                .get_mut("schema")
                .and_then(|s| s.get_mut("quick_input"))
                .and_then(|q| q.as_table_mut())
                .expect("schema.quick_input 应存在")
                .insert("force_vertical".to_string(), toml::Value::Boolean(v));
        }
        merged
    }

    fn quick_mix_layout(merged: toml::Value) -> LayoutIntent {
        let cfg: Config = merged.try_into().expect("迁移后应可反序列化");
        cfg.schema
            .mix_modes
            .iter()
            .find(|m| m.id == QUICK_MIX_ID)
            .expect("内置 quick_mix 应存在")
            .candidate_layout
    }

    /// 废弃键 `force_vertical` → `mix_modes[quick_mix].candidate_layout`。
    ///
    /// ★ 映射刻意不对称：`false` 迁成 **Follow 而非 Horizontal**。旧布尔的 false 语义是
    /// 「不强制」（跟随全局），迁成 Horizontal 会把从没开过这个开关、又把全局设成竖排的
    /// 用户强行钉在横排上。
    #[test]
    fn force_vertical_migrates_into_quick_mix_candidate_layout() {
        for (old, want) in [
            (true, LayoutIntent::Vertical),
            (false, LayoutIntent::Follow),
        ] {
            let mut merged = merged_with_force_vertical(Some(old));
            Config::migrate_force_vertical_value(&mut merged);
            assert_eq!(
                quick_mix_layout(merged),
                want,
                "force_vertical={old} 应迁为 {want:?}"
            );
        }
    }

    /// 旧键缺席（全新安装 / 新版预置文件已删该行）时不动，保留出厂竖排。
    /// 守的是「未改过配置的用户升级后行为不变」。
    #[test]
    fn absent_force_vertical_keeps_factory_vertical() {
        let mut merged = merged_with_force_vertical(None);
        Config::migrate_force_vertical_value(&mut merged);
        assert_eq!(
            quick_mix_layout(merged),
            LayoutIntent::Vertical,
            "无旧键时应保留 default_mix_modes() 的出厂竖排"
        );
    }

    /// 存量迁移：合并成员 `quick_input` 就地展开为四个细分来源，其余成员的相对序不变。
    #[test]
    fn normalize_expands_legacy_quick_input_member() {
        let mut cfg = Config::default();
        cfg.schema.mix_modes = vec![MixModeConfig {
            id: QUICK_MIX_ID.to_string(),
            members: vec![
                "quick_input".to_string(),
                MIX_MEMBER_PRIMARY_PINYIN.to_string(),
                "english".to_string(),
            ],
            ..Default::default()
        }];
        cfg.normalize();
        let mut expected: Vec<String> = wind_quick_input::LEGACY_EXPANSION
            .iter()
            .map(|s| s.to_string())
            .collect();
        expected.push(MIX_MEMBER_PRIMARY_PINYIN.to_string());
        expected.push("english".to_string());
        assert_eq!(
            cfg.schema.mix_modes[0].members, expected,
            "旧值应在原位展开，展开序 = 默认成员序"
        );
        // 幂等：再跑一次不重复展开
        let once = cfg.schema.mix_modes[0].members.clone();
        cfg.normalize();
        assert_eq!(cfg.schema.mix_modes[0].members, once, "迁移应幂等");
    }

    /// 展开时跳过用户已单独写出的细分来源，不产生重复成员。
    #[test]
    fn legacy_expansion_skips_already_present_sources() {
        let mut cfg = Config::default();
        cfg.schema.mix_modes = vec![MixModeConfig {
            id: QUICK_MIX_ID.to_string(),
            members: vec![
                wind_quick_input::MEMBER_NUMBER.to_string(),
                "quick_input".to_string(),
            ],
            ..Default::default()
        }];
        cfg.normalize();
        let m = &cfg.schema.mix_modes[0].members;
        assert_eq!(
            m.iter()
                .filter(|s| *s == wind_quick_input::MEMBER_NUMBER)
                .count(),
            1,
            "细分来源不应重复，实际 {:?}",
            m
        );
        assert_eq!(
            m,
            &vec![
                wind_quick_input::MEMBER_NUMBER.to_string(),
                wind_quick_input::MEMBER_CALC.to_string(),
                wind_quick_input::MEMBER_DATE.to_string(),
                wind_quick_input::MEMBER_REPEAT.to_string(),
            ],
            "用户显式写出的来源保持其位置"
        );
    }

    /// 存量迁移：废弃键 `enable_english = false` 落成 members 里的 english 删除。
    /// 该迁移在反序列化前作用于 TOML 值，故直接验证 `migrate_enable_english_value`。
    #[test]
    fn migrates_disabled_enable_english_into_members() {
        let mut v = toml::Value::try_from(Config::default()).unwrap();
        // 模拟存量用户配置：关掉了英文候选
        v.get_mut("schema")
            .unwrap()
            .get_mut("quick_input")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .insert("enable_english".to_string(), toml::Value::Boolean(false));
        Config::migrate_enable_english_value(&mut v);
        let cfg: Config = v.try_into().unwrap();
        assert!(
            !cfg.schema.mix_modes[0]
                .members
                .contains(&"english".to_string()),
            "关过英文候选的存量用户，升级后英文不应冒回来：{:?}",
            cfg.schema.mix_modes[0].members
        );
        assert!(
            cfg.schema.mix_modes[0]
                .members
                .contains(&MIX_MEMBER_PRIMARY_PINYIN.to_string()),
            "只应移除 english，其余成员不动"
        );
    }

    /// 默认值（enable_english 缺省或为 true）不触发迁移。
    #[test]
    fn default_keeps_english_member() {
        let mut v = toml::Value::try_from(Config::default()).unwrap();
        Config::migrate_enable_english_value(&mut v);
        let cfg: Config = v.try_into().unwrap();
        assert!(
            cfg.schema.mix_modes[0]
                .members
                .contains(&"english".to_string()),
            "无废弃键时 english 成员应保留"
        );
    }

    /// input.default 新增项与既有语义：state_scope 默认 global、remember 默认 false。
    #[test]
    fn input_default_state_scope_defaults() {
        let d = InputDefaultConfig::default();
        assert_eq!(d.state_scope, "global");
        assert!(!d.per_app_scope());
        assert!(!d.remember_last_state);
        // 缺字段的旧 config.toml 反序列化与 Default 一致。
        let parsed: InputDefaultConfig = toml::from_str("").unwrap();
        assert_eq!(parsed.state_scope, "global");
        assert!(!parsed.remember_last_state);
        assert!(parsed.chinese_mode);
        // scope 解析大小写不敏感。
        let parsed: InputDefaultConfig = toml::from_str("state_scope = \"App\"").unwrap();
        assert!(parsed.per_app_scope());
    }

    /// `is_settled` 的语义边界：只有「再等也不会变」的两态算就绪。
    /// 尤其 `Ready { file_exists: false }` **必须**算就绪——全新安装本就没有
    /// config.toml（只在用户首次改设置时创建），把它当未就绪会让每个新用户白等整个超时。
    #[test]
    fn probe_settled_semantics() {
        let dir = PathBuf::from("x");
        assert!(UserConfigProbe::Portable(dir.clone()).is_settled());
        // 自定义数据目录是本机固定盘上的普通目录，不存在「等漫游挂载」一说；
        // 漏判会让每次启动都白等一个完整超时，然后退回系统预置方案。
        assert!(UserConfigProbe::CustomDir(dir.clone()).is_settled());
        assert!(
            UserConfigProbe::Ready {
                dir: dir.clone(),
                dir_exists: true,
                file_exists: true,
            }
            .is_settled()
        );
        assert!(
            UserConfigProbe::Ready {
                dir: dir.clone(),
                dir_exists: false,
                file_exists: false,
            }
            .is_settled(),
            "漫游根就绪后，配置在不在是确定性事实，不该继续等待"
        );
        assert!(
            !UserConfigProbe::ConfigPending { dir: dir.clone() }.is_settled(),
            "本地标记说该用户本有 config.toml，但此刻看不到 → 竞态，须继续等而非就绪"
        );
        // 这两态才是「系统尚未就绪」，等待有意义。
        assert!(!UserConfigProbe::RoamingUnavailable.is_settled());
        assert!(!UserConfigProbe::RoamingMissing(dir).is_settled());
    }

    /// 等待的返回值必须与探测结论一致，且未就绪时不得超出 timeout 太多
    /// （防止把服务启动无限期卡住——超时后要降级继续启动，不是死等）。
    #[test]
    fn wait_respects_probe_and_timeout() {
        let settled = Config::probe_user_config().is_settled();
        let start = std::time::Instant::now();
        let ready = Config::wait_user_config_ready(std::time::Duration::from_millis(50));
        let elapsed = start.elapsed();

        assert_eq!(ready, settled, "返回值应与探测结论一致");
        if settled {
            // 开发机/CI 上漫游根通常存在：必须立即返回，一次 sleep 都不能有。
            assert!(
                elapsed < std::time::Duration::from_millis(250),
                "已就绪却等待了 {elapsed:?}"
            );
        } else {
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "超时后应降级返回而非死等，实际 {elapsed:?}"
            );
        }
    }

    /// 重试路径：前几次未就绪，之后转就绪 → 必须等到就绪再返回 true。
    /// 这是本修复的核心分支，开发机上探测恒就绪走不到，只能靠注入。
    #[test]
    fn wait_retries_until_ready() {
        let mut calls = 0u32;
        let ready = Config::wait_until_settled(
            || {
                calls += 1;
                if calls < 3 {
                    UserConfigProbe::RoamingUnavailable
                } else {
                    UserConfigProbe::Ready {
                        dir: PathBuf::from("x"),
                        dir_exists: true,
                        file_exists: true,
                    }
                }
            },
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(1),
        );
        assert!(ready, "转为就绪后应返回 true");
        assert_eq!(calls, 3, "应恰好重试到就绪那次为止");
    }

    /// 始终未就绪 → 必须在 timeout 后降级返回 false，而不是死等把服务卡住。
    #[test]
    fn wait_gives_up_after_timeout() {
        let mut calls = 0u32;
        let start = std::time::Instant::now();
        let ready = Config::wait_until_settled(
            || {
                calls += 1;
                UserConfigProbe::RoamingMissing(PathBuf::from("x"))
            },
            std::time::Duration::from_millis(60),
            std::time::Duration::from_millis(10),
        );
        assert!(!ready, "始终未就绪应返回 false");
        assert!(calls > 1, "应至少重试过，实际 {calls} 次");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "应及时放弃，实际 {:?}",
            start.elapsed()
        );
    }

    fn tv(s: &str) -> toml::Value {
        toml::from_str::<toml::Value>(s).expect("测试 TOML 应可解析")
    }

    /// 出厂默认（L1⊕L2⊕L2.5）样本：覆盖标量 / 数组 / 多级嵌套。
    fn preset_sample() -> toml::Value {
        tv(r#"
[schema]
active = "wubi86"

[schema.mix]
auto_commit_block_on_pinyin = true
pinyin_only_overflow = true
show_source_hint = false

[schema.codetable]
top_code_commit = true

[keys]
page_keys = ["pageupdown", "minus_equal"]
"#)
    }

    #[test]
    fn prune_removes_redundant_keeps_overrides() {
        let preset = preset_sample();
        let mut user = tv(r#"
[schema]
active = "wubi86_pinyin"

[schema.mix]
auto_commit_block_on_pinyin = false
pinyin_only_overflow = true
show_source_hint = false

[keys]
page_keys = ["pageupdown", "minus_equal"]
"#);
        let removed = prune_redundant(&mut user, &preset);
        assert_eq!(
            removed, 3,
            "pinyin_only_overflow / show_source_hint / page_keys 三个与默认相同"
        );
        assert_eq!(
            get_nested(&user, &["schema", "mix", "auto_commit_block_on_pinyin"]),
            Some(&toml::Value::Boolean(false)),
            "真实覆盖（与默认相反）必须保留"
        );
        assert_eq!(
            get_nested(&user, &["schema", "active"]).and_then(|v| v.as_str()),
            Some("wubi86_pinyin"),
            "真实覆盖必须保留"
        );
        assert!(
            get_nested(&user, &["keys"]).is_none(),
            "唯一子键被删后，空的 [keys] 段应一并回收"
        );
    }

    #[test]
    fn prune_preserves_merged_result() {
        // ★ 本轮修复的核心保证：被删的键都会从 L1⊕L2⊕L2.5 回落到同一个值，故清理对**当前**行为
        // 零影响，只影响将来默认值变更能否到达该用户。这条不变量成立，清理才是安全的。
        let preset = preset_sample();
        let user = tv(r#"
[schema]
active = "wubi86_pinyin"

[schema.mix]
auto_commit_block_on_pinyin = false
pinyin_only_overflow = true
show_source_hint = false

[schema.codetable]
top_code_commit = true

[keys]
page_keys = ["pageupdown", "minus_equal"]

[input.punct.custom_mappings]
"'1" = ["1", "＇"]
"#);
        let mut before = preset.clone();
        merge_value(&mut before, user.clone());

        let mut pruned = user.clone();
        let removed = prune_redundant(&mut pruned, &preset);
        assert!(removed > 0, "样本应含冗余键，否则本测试证明不了任何事");
        let mut after = preset.clone();
        merge_value(&mut after, pruned);

        assert_eq!(before, after, "清理前后合并结果必须逐键相同");
    }

    #[test]
    fn prune_is_idempotent() {
        let preset = preset_sample();
        let mut user = tv(r#"
[schema.mix]
pinyin_only_overflow = true
show_source_hint = false
"#);
        assert_eq!(prune_redundant(&mut user, &preset), 2);
        assert_eq!(prune_redundant(&mut user, &preset), 0, "二次清理应无事可做");
    }

    /// ★ 模式级注释模板（三态 `Option`，**刻意不进注册表**）不得被写回清理掉。
    ///
    /// 这类键的出厂值是「键不存在」＝跟随全局，故 preset 里没有它们、注册表也不登记
    /// （见 `config_schema::REGISTRY` 的说明）。若哪天有人为了「让设置页能看见」把它们
    /// 补进注册表，`prune_redundant` 的第一道保险就失效——用户手写的模板会在某次保存后
    /// 被静默删掉，表现为「配了几天突然没了」。本测试是那个改动的拦截点。
    #[test]
    fn prune_keeps_mode_comment_templates() {
        let preset = preset_sample();
        let mut user = tv(r#"
[input.temp_english]
comment_template_vertical = "${dict}"
comment_template_horizontal = ""
"#);
        assert_eq!(prune_redundant(&mut user, &preset), 0, "未登记键一律不碰");
        assert!(
            get_nested(
                &user,
                &["input", "temp_english", "comment_template_vertical"]
            )
            .is_some(),
            "用户手写的模式级模板必须原样保留"
        );
        assert!(
            get_nested(
                &user,
                &["input", "temp_english", "comment_template_horizontal"]
            )
            .is_some(),
            "空串（= 本模式不显示注释）同样是有效配置，不得被当成冗余删除"
        );
    }

    #[test]
    fn prune_keeps_keys_absent_from_preset() {
        // 出厂默认里没有的键一律保留：用户自定义标点映射这类**动态键**（键名由用户输入决定，
        // 不可能出现在 preset 里）若被当成冗余删掉就是丢用户数据。废弃键的清理是另一件事，
        // 必须走显式名单，绝不能靠「preset 里没有」来推断。
        let preset = preset_sample();
        let mut user = tv(r#"
[input.punct.custom_mappings]
"'1" = ["1", "＇"]
"#);
        assert_eq!(prune_redundant(&mut user, &preset), 0);
        assert!(get_nested(&user, &["input", "punct", "custom_mappings", "'1"]).is_some());
    }

    #[test]
    fn prune_keeps_unregistered_keys_even_when_matching_preset() {
        // 注册表未登记的键即使与 preset 完全相同也不得删——此处用真实存在过的废弃键
        // `input.code_commit.*`（已迁到 schema.codetable.*，注册表里查不到）。
        // 废弃键清理是另一件事，必须走显式名单：靠「等于 preset」去推断会把语义搞反。
        assert!(
            !crate::config_schema::is_known_key("input.code_commit.auto_commit_at_full"),
            "前提：该键确未登记，否则本测试证明不了 registry 这道保险"
        );
        let preset = tv(r#"
[input.code_commit]
auto_commit_at_full = false
"#);
        let mut user = preset.clone();
        assert_eq!(prune_redundant(&mut user, &preset), 0);
        assert!(
            get_nested(&user, &["input", "code_commit", "auto_commit_at_full"]).is_some(),
            "未登记键必须原样保留"
        );
    }

    // ── 引导键物化（`Config::materialize_key_actions`）──

    #[test]
    fn materialize_into_writes_bindings_and_drops_legacy_fields() {
        let mut root = tv(r#"
[input.temp_pinyin]
enabled = true
trigger_keys = ["backtick"]

[input.temp_english]
trigger_keys = ["semicolon"]

[[schema.mix_modes]]
id = "quick_mix"
members = ["date"]
trigger_keys = ["backslash"]
"#);
        let bindings = BTreeMap::from([
            ("backtick".to_string(), "temp_pinyin".to_string()),
            ("semicolon".to_string(), "temp_english".to_string()),
        ]);
        let dropped = materialize_into(&mut root, &bindings).unwrap();

        assert_eq!(dropped, 3, "三处 trigger_keys 都应被摘掉");
        assert_eq!(
            get_nested(&root, &["keys", "key_actions", "backtick"]).and_then(toml::Value::as_str),
            Some("temp_pinyin")
        );
        assert!(already_materialized(&root), "版本标记应已写入");
        assert!(
            get_nested(&root, &["input", "temp_pinyin", "trigger_keys"]).is_none(),
            "旧字段必须删除，否则仍是第二真相源"
        );
        assert!(
            get_nested(&root, &["input", "temp_pinyin", "enabled"]).is_some(),
            "同段内的其它键不得受牵连"
        );
        assert!(
            get_nested(&root, &["input", "temp_english"]).is_none(),
            "该段只剩 trigger_keys 时应被 remove_nested 整段回收"
        );
        // mix_modes 只摘 trigger_keys，元素本身（id/members）必须留着。
        let modes = get_nested(&root, &["schema", "mix_modes"])
            .and_then(toml::Value::as_array)
            .expect("mix_modes 应仍在");
        assert_eq!(modes.len(), 1);
        assert!(modes[0].get("id").is_some(), "元素本身不可删");
        assert!(modes[0].get("trigger_keys").is_none());
    }

    #[test]
    fn already_materialized_only_trusts_explicit_version() {
        // 「key_actions 非空」不等于「迁移过」——用户手工配过但从没迁移时会猜错，
        // 一猜错就是出厂绑定永久丢失，故判据只认显式版本号。
        let hand_written = tv("[keys.key_actions]\nbacktick = \"temp_pinyin\"\n");
        assert!(!already_materialized(&hand_written));
        assert!(!already_materialized(&tv("")));
        assert!(already_materialized(&tv(
            "[keys]\nkey_actions_materialized = 1\n"
        )));
    }

    /// ★ 回归闸：物化标记绝不能被 `prune_user_config` 清掉。
    ///
    /// 标记一旦丢失，下次加载就会重新折算，把用户删掉的出厂绑定灌回去——本次修复
    /// 整个失效，且现象与修复前一模一样（"删了又回来"）。
    #[test]
    fn prune_keeps_materialize_marker() {
        let preset = toml::Value::try_from(Config::default()).unwrap();
        assert!(
            get_nested(&preset, &["keys", "key_actions_materialized"]).is_none(),
            "标记不得出现在序列化产物里——否则 registry_covers_every_config_key 会红，\
             且它会被当成用户可配项参与 prune 比对"
        );
        assert!(
            !crate::config_schema::is_known_key("keys.key_actions_materialized"),
            "标记是迁移账本、不是配置项，不该登记进注册表"
        );
        let mut user = tv("[keys]\nkey_actions_materialized = 1\n");
        prune_redundant(&mut user, &preset);
        prune_retired(&mut user);
        assert!(already_materialized(&user), "标记必须在清理后仍然存活");
    }

    /// ★★ 行为闸：已物化时，折算不得复活用户删掉的绑定。
    ///
    /// 这正是报障现场——用户在设置页取消勾选反引号，保存后下次加载又被折算灌回去。
    #[test]
    fn materialized_config_stops_reviving_deleted_binding() {
        let mut c = Config::default();
        // L2 出厂声明仍在（磁盘上那份我们不删），而用户已把绑定删光。
        c.input.temp_pinyin.trigger_keys = vec!["backtick".into()];
        c.keys.key_actions.clear();
        c.keys.key_actions_materialized = KEY_ACTIONS_MATERIALIZE_VERSION;

        c.normalize();

        assert!(
            !c.keys.key_actions.contains_key("backtick"),
            "已物化时 trigger_keys 不得再折算——否则用户的删除永远不生效"
        );
        assert!(
            c.input.temp_pinyin.trigger_keys.is_empty(),
            "normalize 的后置条件：trigger_keys 恒为空，消费端一律读 key_actions"
        );
    }

    /// 反事实对照：未物化时折算照旧（Android 等不跑物化的宿主依赖这条路径）。
    #[test]
    fn unmaterialized_config_still_folds_trigger_keys() {
        let mut c = Config::default();
        c.input.temp_pinyin.trigger_keys = vec!["backtick".into()];
        c.keys.key_actions.clear();
        assert_eq!(c.keys.key_actions_materialized, 0, "前提：未物化");

        c.normalize();

        assert_eq!(
            c.keys.key_actions.get("backtick").map(String::as_str),
            Some("temp_pinyin"),
            "未物化时必须保持原有折算行为，否则 Android 侧引导键当场失效"
        );
    }

    #[test]
    fn prune_keeps_map_subpaths() {
        // `input.punct.custom_mappings` 在注册表里是 Map 类型——**整体**才是一个配置项。
        // collect_leaf_paths 会把它下钻成 `...custom_mappings."'1"` 这种伪键，删单条是错的语义
        // （等于悄悄改写用户的标点映射表）。registry 保险必须拦住。
        assert!(
            crate::config_schema::is_known_key("input.punct.custom_mappings"),
            "前提：Map 整体是登记键"
        );
        assert!(
            !crate::config_schema::is_known_key("input.punct.custom_mappings.'1"),
            "前提：其子路径不是登记键"
        );
        let preset = tv(r#"
[input.punct.custom_mappings]
"'1" = ["1", "＇"]
"#);
        let mut user = preset.clone();
        assert_eq!(
            prune_redundant(&mut user, &preset),
            0,
            "Map 子路径不得被当叶子删除"
        );
        assert!(get_nested(&user, &["input", "punct", "custom_mappings", "'1"]).is_some());
    }

    #[test]
    fn remove_nested_reclaims_empty_parents_only() {
        let mut root = tv(r#"
[a.b]
x = 1
y = 2
"#);
        let toml::Value::Table(t) = &mut root else {
            unreachable!()
        };
        assert!(remove_nested(t, &["a", "b", "x"]));
        assert!(
            get_nested(&root, &["a", "b", "y"]).is_some(),
            "兄弟键还在时不得回收父表"
        );
        let toml::Value::Table(t) = &mut root else {
            unreachable!()
        };
        assert!(remove_nested(t, &["a", "b", "y"]));
        assert!(get_nested(&root, &["a"]).is_none(), "父表变空应逐级回收");
    }

    /// `MixModeConfig` 的两条默认值路径必须一致：serde 缺省（读一份没写该键的配置）与
    /// `Default::default()`（测试夹具 / 代码构造）。
    ///
    /// `free_input_takes_select_keys` 的 serde 缺省是 `true`，而 derive 出来的
    /// `bool::default()` 是 `false`——所以 `Default` 是手写的。本测试就是那条约束的守门：
    /// 日后再加带非零默认值的字段而忘了改手写 `Default`，这里会红。
    #[test]
    fn mix_mode_config_serde_default_matches_default_impl() {
        let from_serde: MixModeConfig =
            toml::from_str("").expect("空表应能反序列化出全默认的 MixModeConfig");
        assert_eq!(
            from_serde,
            MixModeConfig::default(),
            "serde 缺省与 Default::default() 必须逐字段一致"
        );
        assert!(
            MixModeConfig::default().free_input_takes_select_keys,
            "夺取二三候选键默认应为开"
        );
    }

    /// 退役键走显式名单清除，且**三类不得误伤**：同段里还活着的键、名字相似但仍在使用的
    /// 另一个键、以及未登记的 Map 子路径。
    #[test]
    fn prune_retired_removes_dead_keys_only() {
        let mut root = tv(r#"
[schema.quick_input]
enable_english = true
enabled = true
decimal_places = 6

[schema.mix]
enable_english = true

[input.punct.custom_mappings]
"/" = ["、", "／", "、", "/"]
"#);
        assert_eq!(prune_retired(&mut root), 2, "两个退役键都应删除");
        assert!(get_nested(&root, &["schema", "quick_input", "enable_english"]).is_none());
        assert!(get_nested(&root, &["schema", "quick_input", "enabled"]).is_none());
        assert!(
            get_nested(&root, &["schema", "quick_input", "decimal_places"]).is_some(),
            "同段里还活着的键不得误删"
        );
        // ★ `schema.mix.enable_english` 是**另一个仍在使用的键**（混输引擎混入英文词库
        //   候选的开关，manager.rs 实读）。它与退役的 `schema.quick_input.enable_english`
        //   只是叶子名相同，按整条路径匹配才不会误伤。
        assert!(
            get_nested(&root, &["schema", "mix", "enable_english"]).is_some(),
            "schema.mix.enable_english 仍在使用，不得误删"
        );
        // Map 子路径同样不在注册表里，靠「未登记就删」会把用户的自定义标点映射删光。
        assert!(
            get_nested(&root, &["input", "punct", "custom_mappings", "/"]).is_some(),
            "Map 子路径不得误删"
        );
        assert_eq!(prune_retired(&mut root), 0, "幂等：再跑一次删 0 个");
    }

    /// 父表被清空时应整段回收——用户配置里 `[schema.quick_input]` 常常只有这两个退役键。
    #[test]
    fn prune_retired_reclaims_emptied_parent_table() {
        let mut root = tv(r#"
[schema.quick_input]
enable_english = true
enabled = true
"#);
        assert_eq!(prune_retired(&mut root), 2);
        assert!(
            get_nested(&root, &["schema", "quick_input"]).is_none(),
            "两个键都删完后空的 [schema.quick_input] 段应一并回收"
        );
    }

    #[test]
    fn collect_leaf_paths_treats_arrays_as_leaves() {
        // 数组整体是一个配置项：下钻进元素会切出无法用 path 表达、也无法与 preset 比对的伪键。
        let v = tv(r#"
[keys]
page_keys = ["a", "b"]

[schema]
active = "x"
"#);
        let mut out = Vec::new();
        collect_leaf_paths(&v, &mut Vec::new(), &mut out);
        out.sort();
        assert_eq!(
            out,
            vec![
                vec!["keys".to_string(), "page_keys".to_string()],
                vec!["schema".to_string(), "active".to_string()],
            ]
        );
    }

    #[test]
    fn set_nested_creates_overwrites_and_preserves() {
        let mut t = toml::Table::new();
        t.insert("keep".into(), toml::Value::String("x".into()));
        set_nested(
            &mut t,
            &["ui", "candidate", "preedit_display"],
            toml::Value::String("candidate_inline".into()),
        );
        set_nested(
            &mut t,
            &["schema", "active"],
            toml::Value::String("pinyin".into()),
        );
        // 原有项保留
        assert_eq!(t.get("keep").unwrap().as_str(), Some("x"));
        // 嵌套创建
        assert_eq!(
            t.get("ui")
                .unwrap()
                .get("candidate")
                .unwrap()
                .get("preedit_display")
                .unwrap()
                .as_str(),
            Some("candidate_inline")
        );
        assert_eq!(
            t.get("schema").unwrap().get("active").unwrap().as_str(),
            Some("pinyin")
        );
        // 同路径覆盖
        set_nested(
            &mut t,
            &["ui", "candidate", "preedit_display"],
            toml::Value::String("candidate_top".into()),
        );
        assert_eq!(
            t.get("ui")
                .unwrap()
                .get("candidate")
                .unwrap()
                .get("preedit_display")
                .unwrap()
                .as_str(),
            Some("candidate_top")
        );
        // 其它兄弟键不受影响
        assert_eq!(
            t.get("schema").unwrap().get("active").unwrap().as_str(),
            Some("pinyin")
        );
    }

    /// 模拟 load 的合并：默认 Value ← overlay 深合并 → 反序列化 + normalize。
    fn merged_with(overlay_toml: &str) -> Config {
        let mut base = toml::Value::try_from(Config::default()).unwrap();
        let overlay: toml::Value = toml::from_str(overlay_toml).unwrap();
        merge_value(&mut base, overlay);
        let mut cfg: Config = base.try_into().unwrap();
        cfg.normalize();
        cfg
    }

    #[test]
    fn test_default_per_page_is_7() {
        assert_eq!(
            Config::default().ui.candidate.per_page,
            7,
            "候选每页默认应为 7"
        );
    }

    #[test]
    fn test_merge_reads_per_page_from_ui_candidate() {
        let cfg = merged_with("[ui.candidate]\nper_page = 9\n");
        assert_eq!(
            cfg.ui.candidate.per_page, 9,
            "应从 [ui.candidate] 读取 per_page=9"
        );
    }

    #[test]
    fn test_merge_per_page_zero_keeps_default() {
        // per_page=0 视为无效，normalize 回退默认，避免每页只显示 1 个
        let cfg = merged_with("[ui.candidate]\nper_page = 0\n");
        assert_eq!(cfg.ui.candidate.per_page, 7, "per_page=0 应保留默认 7");
    }

    #[test]
    fn test_merge_keeps_input_s2t_debug() {
        // 回归：deep-merge 必须保留各段（features 拆解后 s2t 归 input）
        let cfg = merged_with(
            "[input.s2t]\nenabled = true\nvariant = \"s2tw\"\n\
             [debug]\nlog_level = \"trace\"\n",
        );
        assert!(cfg.input.s2t.enabled, "input.s2t.enabled 应被合并");
        assert_eq!(cfg.input.s2t.variant, "s2tw");
        assert_eq!(cfg.debug.log_level, "trace", "debug 段应被合并");
    }

    #[test]
    fn test_merge_partial_keeps_unspecified_default() {
        // overlay 只覆盖单个字段，同段其它字段保留默认（不被清空）
        let cfg = merged_with("[input]\nenter_behavior = \"clear\"\n");
        assert_eq!(cfg.input.enter_behavior, "clear");
        assert_eq!(cfg.input.filter_mode, "smart", "同段未指定字段应保留默认");
    }

    #[test]
    fn test_merge_input_subtable_fields() {
        // 旧合并漏掉的 input 子表字段（如 auto_pair）现应合并
        let cfg = merged_with("[input.auto_pair]\nchinese = false\nenglish = false\n");
        assert!(!cfg.input.auto_pair.chinese);
        assert!(!cfg.input.auto_pair.english);
    }

    #[test]
    fn test_tooltip_defaults_match_go() {
        let t = Config::default().ui.tooltip;
        assert_eq!(
            t.delay, 200,
            "delay 按 data/config.toml 预置默认 200（偏离 Go 的 100）"
        );
        assert!(t.code_enabled, "code 默认开");
        assert!(
            t.pinyin_enabled && t.pinyin_heteronyms,
            "pinyin 默认开+全读音"
        );
        assert_eq!(t.pinyin_max_readings, 0);
        assert!(!t.chaizi_enabled, "chaizi 默认关");
        assert!(!t.debug_enabled, "debug 默认关");
    }

    #[test]
    fn test_tooltip_merge_override() {
        let cfg = merged_with(
            "[ui.tooltip]\nchaizi_enabled = true\npinyin_heteronyms = false\npinyin_max_readings = 2\n",
        );
        assert!(cfg.ui.tooltip.chaizi_enabled);
        assert!(!cfg.ui.tooltip.pinyin_heteronyms);
        assert_eq!(cfg.ui.tooltip.pinyin_max_readings, 2);
        // 未指定字段保留默认
        assert!(cfg.ui.tooltip.code_enabled, "code 未指定应保留默认开");
        assert_eq!(cfg.ui.tooltip.delay, 200);
    }

    #[test]
    fn test_candidate_tuning_defaults_and_methods() {
        let c = Config::default().ui.candidate;
        assert_eq!(c.font_size, 18.0, "字号默认 18");
        assert!(c.font_size_follow_theme, "默认跟随主题");
        assert_eq!(c.max_chars, 16, "默认最大 16 字");
        assert!(c.index_labels.is_empty() && !c.flip_when_above);
        // 未配置 → 全部让位（主题/默认数字由协调器裁决）
        assert_eq!(c.user_index_label(0), None);
        // truncate：0=不限
        assert_eq!(
            c.truncate_display("这是一个很长的候选"),
            "这是一个很长的候选"
        );
    }

    #[test]
    fn test_candidate_index_labels_and_truncate() {
        let cfg = merged_with(
            "[ui.candidate]\nindex_labels = [\"a\", \"s\", \"d\", \"f\"]\nmax_chars = 4\n",
        );
        let c = cfg.ui.candidate;
        assert_eq!(c.user_index_label(0), Some("a".to_string()));
        assert_eq!(c.user_index_label(2), Some("d".to_string()));
        assert_eq!(
            c.user_index_label(9),
            None,
            "槽位不足→None（让位主题/默认）"
        );
        assert_eq!(
            c.truncate_display("一二三四五六"),
            "一二三四…",
            "截断到 4 字并加省略号"
        );
        assert_eq!(c.truncate_display("一二"), "一二", "不足不截");
    }

    #[test]
    fn test_user_index_label_optional() {
        // 用户显式设置：已配槽位返回 Some，越界返回 None（主题层可接手）。
        let cfg = merged_with("[ui.candidate]\nindex_labels = [\"a\", \"s\", \"d\", \"f\"]\n");
        let c = cfg.ui.candidate;
        assert_eq!(c.user_index_label(0), Some("a".to_string()));
        assert_eq!(c.user_index_label(3), Some("f".to_string()));
        assert_eq!(c.user_index_label(4), None, "越界→None（让位主题/默认）");
        // 未配置：全 None，优先级完全交给主题/默认。
        let d = merged_with("").ui.candidate;
        assert_eq!(d.user_index_label(0), None);
    }

    #[test]
    fn test_index_label_slot_holds_multiple_chars() {
        // 本次重构的核心判据：**一槽一串**，不再按 char 切。
        // 三种旧形态下必被拆散的标签：括号数字、罗马数字、带 ZWJ 的组合 emoji。
        let cfg = merged_with(
            "[ui.candidate]\nindex_labels = [\"(1)\", \"Ⅱ\", \"👨\\u200D👩\\u200D👧\"]\n",
        );
        let c = cfg.ui.candidate;
        assert_eq!(c.user_index_label(0), Some("(1)".to_string()));
        assert_eq!(c.user_index_label(1), Some("Ⅱ".to_string()));
        assert_eq!(
            c.user_index_label(2),
            Some("👨\u{200D}👩\u{200D}👧".to_string()),
            "ZWJ 组合序列整体占一槽，不被拆成 5 个 char"
        );
    }

    #[test]
    fn test_index_label_empty_slot_yields_to_theme() {
        // 第二项恢复的能力：中间空槽 = 该槽让位主题（旧的字符串形态表达不出来）。
        // 判据是 None 而非 Some("")——协调器只在 None 时才去问主题层。
        let cfg = merged_with("[ui.candidate]\nindex_labels = [\"a\", \"\", \"\", \"f\"]\n");
        let c = cfg.ui.candidate;
        assert_eq!(c.user_index_label(0), Some("a".to_string()));
        assert_eq!(c.user_index_label(1), None, "空槽让位，不是 Some(\"\")");
        assert_eq!(c.user_index_label(2), None);
        assert_eq!(
            c.user_index_label(3),
            Some("f".to_string()),
            "空槽之后仍生效"
        );
    }

    #[test]
    fn test_migrate_empty_code_behavior_normalizes_illegal_values() {
        // 设置端 manifest 曾把 keys.overflow 的四个选项抄给了这三个键，而消费点只认
        // commit/clear。归一是零行为变更（那两个值本就走 commit 分支），目的是让存量配置
        // 落回合法值域，设置页下拉才能「按当前值恢复选中项」而不是静默弹回首项。
        let mut v: toml::Value = toml::from_str(
            "[input]\nenter_behavior = \"ignore\"\nspace_on_empty_behavior = \"commit_and_input\"\npunct_on_empty_behavior = \"clear\"\n",
        )
        .unwrap();
        Config::migrate_empty_code_behavior_value(&mut v);
        assert_eq!(v["input"]["enter_behavior"].as_str(), Some("commit"));
        assert_eq!(
            v["input"]["space_on_empty_behavior"].as_str(),
            Some("commit")
        );
        assert_eq!(
            v["input"]["punct_on_empty_behavior"].as_str(),
            Some("clear"),
            "合法值不得被动——否则这条迁移会把用户设的 clear 抹成 commit"
        );
        // ★ 标点独有的第三态必须活着穿过迁移。这条断言的存在意义：迁移一旦把合法值域
        // 内联抄成 ["commit","clear"]（最自然的写法），存量配置里的 clear_no_input 会被
        // 静默抹回 commit——只在升过级的机器上复现，新装机器怎么测都是绿的。
        let mut v3: toml::Value =
            toml::from_str("[input]\npunct_on_empty_behavior = \"clear_no_input\"\n").unwrap();
        Config::migrate_empty_code_behavior_value(&mut v3);
        assert_eq!(
            v3["input"]["punct_on_empty_behavior"].as_str(),
            Some("clear_no_input"),
            "标点第三态被迁移抹掉了——合法值域须取自注册表，不能在迁移里另抄一份"
        );
        // 反向夹逼：同一个值在回车上**不合法**，须归一为 commit。两条一起才拦得住
        // 「图省事让三键共用一份值域」。
        let mut v4: toml::Value =
            toml::from_str("[input]\nenter_behavior = \"clear_no_input\"\n").unwrap();
        Config::migrate_empty_code_behavior_value(&mut v4);
        assert_eq!(
            v4["input"]["enter_behavior"].as_str(),
            Some("commit"),
            "回车没有 clear_no_input 这一态，须被归一"
        );
        // 幂等：每次启动都会跑一遍。
        let before = v.clone();
        Config::migrate_empty_code_behavior_value(&mut v);
        assert_eq!(v, before, "对已归一的值二次迁移须无变化");
    }

    #[test]
    fn test_empty_code_behavior_registry_values_match_impl() {
        // ★ 守门：注册表的值域必须与消费点实际认得的值一致。这三个键此前登记为 Str，
        // 值域不受任何约束，设置端才得以抄进两个从未被实现的选项且无一层拦得住。
        //
        // ⚠️ 三键**不同值域**：标点多一个 `clear_no_input`（丢废码且吞标点），回车/空格的
        // `clear` 本就是吞键态，给它们加同名值只会得到两个行为相同、用户无从分辨的选项。
        // 分开断言正是为了拦「顺手统一成一样」——共用一个期望值的话，把标点的值域抄回
        // 两态、或把新值扩散给另两键，测试都照样绿。
        let values_of = |key: &str| -> &'static [&'static str] {
            let f = crate::config_schema::field(key).expect("键须已登记");
            match f.ty {
                crate::config_schema::FieldType::Enum(vals) => vals,
                other => panic!("{key} 须登记为 Enum 而非 {other:?}"),
            }
        };
        for key in ["input.enter_behavior", "input.space_on_empty_behavior"] {
            assert_eq!(
                values_of(key),
                ["commit", "clear"],
                "{key} 的值域须恰为 commit/clear——它的 clear 已是吞键态，别把标点那个新值扩散过来"
            );
        }
        assert_eq!(
            values_of("input.punct_on_empty_behavior"),
            ["commit", "clear", "clear_no_input"],
            "标点值域须为三态——加值前先确认 Coordinator::punct_empty_code_policy 真的分了那一支"
        );
    }

    #[test]
    fn test_migrate_index_labels_string_to_array() {
        // 存量迁移：旧的字符串形态按 char 拆成数组。不迁移的话 `try_into` 整体失败，
        // 调用方 `unwrap_or_default()` 会把**全部**配置回落出厂值。
        let mut v: toml::Value =
            toml::from_str("[ui.candidate]\nindex_labels = \"asdf\"\n").unwrap();
        Config::migrate_index_labels_value(&mut v);
        assert_eq!(
            v["ui"]["candidate"]["index_labels"],
            toml::Value::try_from(["a", "s", "d", "f"]).unwrap()
        );
        // 幂等：已是数组则原样不动（每次启动都会跑一遍）。
        let before = v.clone();
        Config::migrate_index_labels_value(&mut v);
        assert_eq!(v, before, "对新格式二次迁移须无变化");
        // 迁移后能落进结构体，且旧值语义不变。
        let cfg = merged_with("[ui.candidate]\nindex_labels = [\"a\", \"s\", \"d\", \"f\"]\n");
        assert_eq!(cfg.ui.candidate.user_index_label(1), Some("s".to_string()));
    }

    #[test]
    fn test_load_survives_legacy_string_index_labels() {
        // 迁移的真正把关点在 `Config::load` 的 `merged.try_into()?` 上：
        // 这里复刻那条链路（默认值 ⊕ 旧格式用户层 → 迁移 → 反序列化）。
        // 没有迁移这一步，try_into 会 Err，整个配置无声回落出厂值。
        let mut merged = toml::Value::try_from(Config::default()).unwrap();
        let legacy: toml::Value =
            toml::from_str("[ui.candidate]\nindex_labels = \"asdf\"\n").unwrap();
        merge_value(&mut merged, legacy);
        Config::migrate_index_labels_value(&mut merged);
        let cfg: Config = merged
            .try_into()
            .expect("旧字符串经迁移后须能反序列化，否则全盘配置丢失");
        assert_eq!(
            cfg.ui.candidate.index_labels,
            vec!["a", "s", "d", "f"],
            "旧的单字符标签原样保留，用户无需重配"
        );
    }

    // ─────────────────────── 段级降级（section fallback）───────────────────────

    /// 复刻 [`Config::load`] 的「L1 默认 ⊕ 用户层 → 反序列化」链路。
    ///
    /// ⚠️ **刻意不调 `Config::load`**：它会走 `user_config_dir()` 去读真实
    /// `%APPDATA%\WindInput\config.toml`——那样测试的输入取决于跑测试的这台机器
    /// （本机有配置就测不出、别人机器上又是另一套），本仓也有过测试真写用户配置的前科。
    /// 段级降级的全部判定都在 `deserialize_with_section_fallback` 这个纯函数里，IO 留在
    /// `load()` 外层，测试只喂 `toml::Value`。
    fn merged_user_value(user_toml: &str) -> toml::Value {
        let mut merged = toml::Value::try_from(Config::default()).expect("默认配置应可序列化");
        merge_value(
            &mut merged,
            toml::from_str(user_toml).expect("用例 TOML 应可解析"),
        );
        merged
    }

    /// `ui` 段的毒：`ui.font.scripts` 是 `BTreeMap<String, Vec<String>>`。
    ///
    /// ★ 用 Map 型字段而不是「被删掉的普通字段的残留键」构造用例：全仓无
    /// `deny_unknown_fields` / `flatten` / `untagged`，普通 struct 的未知键被 serde 静默
    /// 丢弃，是**零风险**的，拿它构造用例会得到一个恒绿的假测试。Map 型字段对 serde 而言
    /// 「任何键都是已知的」，旧版残留项会被当真数据反序列化——那才是真实故障路径。
    const POISON_UI: &str = "[ui.font]\nscripts = { latin = 42 }\n";
    /// `keys` 段的毒：`keys.key_actions` 是 `BTreeMap<String, String>`，值给整型。
    const POISON_KEYS: &str = "[keys]\nkey_actions = { F7 = 42 }\n";
    /// 分散在各段、**必须活下来**的用户设置。
    ///
    /// `ui.candidate.per_page` 是关键一条：它与 [`POISON_UI`] **同属 `ui` 段但不同子表**，
    /// 用来钉住「降级只降到 `ui.font`，不牵连 `ui.candidate`」。
    const HEALTHY_USER: &str = "\
[schema]
active = \"section_fallback_probe\"
[input.default]
chinese_mode = false
[ui.candidate]
per_page = 9
[stats]
enabled = false
[debug]
log_level = \"trace\"
";

    /// 坏段回落默认，**其余段的用户值逐键完好**。
    ///
    /// # 反事实验证（两轮，均已实跑，非声称）
    ///
    /// **一、摘掉整个降级逻辑**——在 `deserialize_with_section_fallback` 开头插一行
    /// `return merged.try_into().unwrap_or_default();`（＝本机制上线前的等价行为），
    /// 重跑 `cargo test -p wind-config -j 2 -- section_fallback degradation_affects`：
    /// 9 个用例红 6 个，本用例红在第一条断言上：
    ///
    /// ```text
    /// assertion `left == right` failed: schema 段无毒，用户值必须原样保留
    ///   left: "" / right: "section_fallback_probe"
    /// ```
    ///
    /// **二、只摘掉子表细化、保留段级降级**——让 [`narrow_bad_section`] 恒返回空表。
    /// 9 个用例红 5 个，红的正是粒度那几条：
    ///
    /// ```text
    /// section_fallback_narrows_to_subtable_within_section:
    ///   assertion failed: ui.candidate 不该被牵连   left: 7 / right: 9
    /// section_fallback_is_idempotent:
    ///   left: ["ui"] / right: ["ui.font"]
    /// ```
    ///
    /// 第二轮是「探两层」这项改进单独的守门：它证明这些断言测的是**粒度**，
    /// 而不是搭降级逻辑的便车。
    ///
    /// 两轮里都仍绿、且**应该**绿的：
    /// [`section_fallback_is_transparent_when_healthy`]（断言成功路径与老行为一致，
    /// 两种摘法都没动成功路径）、[`degradation_affects_matches_subpaths`]（纯判据单测）；
    /// 第二轮另有 [`section_fallback_falls_back_to_whole_section_when_not_narrowable`]
    /// 仍绿，它测的正是细化不了时的退路。
    #[test]
    fn section_fallback_keeps_healthy_sections() {
        let merged = merged_user_value(&format!("{HEALTHY_USER}{POISON_UI}"));
        // 前置：这份输入在没有降级的老链路上确实是 Err。若哪天 `scripts` 换了类型、
        // 这条毒不再是毒，这里会先红——避免下面的断言退化成恒绿。
        assert!(
            merged.clone().try_into::<Config>().is_err(),
            "用例前提失效：该输入已不再触发整体反序列化失败，请另造毒键"
        );

        let cfg = Config::deserialize_with_section_fallback(merged);

        // 好段逐键保留。★ 这组断言**故意排在降级记录之前**：它们才是用户实际丢掉的东西，
        // 摘掉降级逻辑时应该由它们先红，指向「用户的方案设置没了」而不是「元信息对不上」。
        assert_eq!(
            cfg.schema.active, "section_fallback_probe",
            "schema 段无毒，用户值必须原样保留"
        );
        assert!(!cfg.input.default.chinese_mode, "input 段的用户值必须保留");
        assert!(!cfg.stats.enabled, "stats 段的用户值必须保留");
        assert_eq!(cfg.debug.log_level, "trace", "debug 段的用户值必须保留");
        assert_eq!(
            cfg.ui.candidate.per_page, 9,
            "毒在 ui.font，ui.candidate 是同段的**另一个子表**，用户值必须保留"
        );

        assert_eq!(cfg.degradation.sections, vec!["ui.font".to_string()]);
        assert!(!cfg.degradation.total_fallback);

        // 坏段回落 L1 默认（不是「保留了半截毒值」，也不是「整份归零」）
        assert!(
            cfg.ui.font.scripts.is_empty(),
            "有毒的 ui.font 子表须回落出厂默认"
        );
    }

    /// ★ 降级粒度收到**子表**这一层：`ui.font.scripts` 有毒时只降 `ui.font`，
    /// `ui` 段其余子表的用户值一个不少。
    ///
    /// 这条是「探两层」这项改进的全部意义所在。`ui` 一段有 99 个键——只降到段一级的话，
    /// 一个坏的字体映射就会把候选窗尺寸、位置、注释模板、工具栏、主题一起打回出厂，
    /// 离本机制想避免的「一切归零」只差一点。没有这条用例，这项改进等于没做。
    #[test]
    fn section_fallback_narrows_to_subtable_within_section() {
        // 同一个 `ui` 段里铺开 4 个子表的用户值，只有 `ui.font` 那个有毒。
        let merged = merged_user_value(
            "\
[ui.candidate]
per_page = 9
max_chars = 33
[ui.theme]
name = \"section_fallback_theme\"
[ui.toolbar]
visible = false
[ui.font]
family = \"SectionFallbackFont\"
scripts = { latin = 42 }
",
        );
        assert!(merged.clone().try_into::<Config>().is_err(), "用例前提");

        let cfg = Config::deserialize_with_section_fallback(merged);

        // 同段的其他子表：完好
        assert_eq!(cfg.ui.candidate.per_page, 9, "ui.candidate 不该被牵连");
        assert_eq!(cfg.ui.candidate.max_chars, 33, "ui.candidate 不该被牵连");
        assert_eq!(
            cfg.ui.theme.name, "section_fallback_theme",
            "ui.theme 不该被牵连"
        );
        assert!(!cfg.ui.toolbar.visible, "ui.toolbar 不该被牵连");

        // 有毒的那个子表：整个回落（`family` 是同子表内的附带损失，粒度只到这一层）
        assert_eq!(cfg.degradation.sections, vec!["ui.font".to_string()]);
        assert!(cfg.ui.font.scripts.is_empty());
        assert_eq!(
            cfg.ui.font.family,
            Config::default().ui.font.family,
            "同一子表内的键一并回落——这是「只递归一层」的已知代价"
        );
    }

    /// 细化不到子键时退回整段降级：段本身就不是表（`[input]` 位置写了个标量）。
    #[test]
    fn section_fallback_falls_back_to_whole_section_when_not_narrowable() {
        let mut merged = merged_user_value(HEALTHY_USER);
        merged
            .as_table_mut()
            .unwrap()
            .insert("input".to_string(), toml::Value::Integer(42));
        assert!(merged.clone().try_into::<Config>().is_err(), "用例前提");

        let cfg = Config::deserialize_with_section_fallback(merged);

        assert_eq!(
            cfg.degradation.sections,
            vec!["input".to_string()],
            "探不出更细的粒度就记整段，不能假装细化成功"
        );
        assert_eq!(
            cfg.schema.active, "section_fallback_probe",
            "其余段照常保留"
        );
        assert_eq!(cfg.ui.candidate.per_page, 9);
        assert!(
            cfg.input.default.chinese_mode,
            "input 整段回落出厂默认（默认为 true）"
        );
    }

    /// ★ 写盘闸的判据必须**两个方向都判**：降级段是待写路径的祖先、待写路径是降级段的祖先。
    ///
    /// 只判前者会漏掉「写大表、坏在小格」——`materialize_key_actions` 要整表写 `keys`，
    /// 而降级粒度可以细到 `keys.key_actions`，漏判的后果是本该拦下的整表覆盖照样发生。
    /// 只判后者会漏掉「坏在大段、写小格」——`input.punct` 整段降级时，
    /// `input.punct.custom_mappings` 的种子同样是出厂值。
    ///
    /// 两个方向各来一条断言，且各配一条**不该命中**的对照（否则「恒为 true」也能全绿）。
    #[test]
    fn taints_judges_both_ancestor_directions() {
        let deg = ConfigDegradation {
            sections: vec!["input.punct".into(), "keys.key_actions".into()],
            total_fallback: false,
        };
        // 相等
        assert!(deg.taints("input.punct"));
        // 降级段是待写路径的祖先（坏在大段、写小格）
        assert!(deg.taints("input.punct.custom_mappings"));
        // 待写路径是降级段的祖先（写大表、坏在小格）
        assert!(
            deg.taints("keys"),
            "整表写 keys 时 keys.key_actions 降级必须拦下"
        );
        // 对照：同段的**兄弟**子路径不受牵连，否则一个坏键会把整段的写盘全堵死
        assert!(!deg.taints("input.symbol"));
        assert!(!deg.taints("keys.session_actions"));
        assert!(!deg.taints("ui"));
        // 前缀相同但不是路径祖先：`keys.key_actions_materialized` 不该被
        // `keys.key_actions` 命中（字符串 starts_with 会误判，故判据必须看 `.`）
        assert!(!deg.taints("keys.key_actions_materialized"));

        // 整份回落 ⇒ 一切不可信
        let total = ConfigDegradation {
            sections: Vec::new(),
            total_fallback: true,
        };
        assert!(total.taints("anything"));
        // 未降级 ⇒ 一律放行（正常路径不能被闸误伤）
        let clean = ConfigDegradation::default();
        assert!(!clean.taints("keys"));
        assert!(!clean.blocks_write_back("keys", "测试"));
    }

    /// `affects` 与 `taints` 对顶层段名必须给出同一答案——两处判据漂移正是本仓
    /// 反复栽的那类，而这两个函数守的是同一批写盘路径。
    #[test]
    fn affects_agrees_with_taints_on_top_level_sections() {
        for deg in [
            ConfigDegradation {
                sections: vec!["keys.key_actions".into()],
                total_fallback: false,
            },
            ConfigDegradation {
                sections: vec!["ui".into()],
                total_fallback: false,
            },
            ConfigDegradation {
                sections: Vec::new(),
                total_fallback: true,
            },
            ConfigDegradation::default(),
        ] {
            for section in ["input", "keys", "schema", "ui", "stats", "debug", "mobile"] {
                assert_eq!(
                    deg.affects(section),
                    deg.taints(section),
                    "{deg:?} 在 [{section}] 上两个判据分叉了"
                );
            }
        }
    }

    /// ★ 每条降级记录必须带**自己那一段**的错误，不能共用最初整份 `try_into` 的错误。
    ///
    /// 多段同时有毒时，整份 `try_into` 的错误只点得到其中一个段。若把它拼进每一行 WARN，
    /// `[keys]` 那行携带的错误文本讲的会是 `ui.font.scripts`——排查的人被直接带到无关的段，
    /// 而这类误导比没有日志更费时间。
    #[test]
    fn section_fallback_reports_per_section_error() {
        let merged = merged_user_value(&format!("{HEALTHY_USER}{POISON_UI}{POISON_KEYS}"));
        let default_v = toml::Value::try_from(Config::default()).unwrap();
        let sections = merged.as_table().unwrap();

        let ui_err = narrow_bad_section(&default_v, "ui", sections.get("ui").unwrap());
        let keys_err = narrow_bad_section(&default_v, "keys", sections.get("keys").unwrap());

        assert_eq!(ui_err.len(), 1);
        assert_eq!(keys_err.len(), 1);
        assert_eq!(ui_err[0].0, "ui.font");
        assert_eq!(keys_err[0].0, "keys.key_actions");

        assert!(
            ui_err[0].1.contains("scripts"),
            "ui 那条要讲自己的字段，实得 {:?}",
            ui_err[0].1
        );
        assert!(
            keys_err[0].1.contains("key_actions"),
            "keys 那条要讲自己的字段，实得 {:?}",
            keys_err[0].1
        );
        assert!(
            !keys_err[0].1.contains("scripts"),
            "keys 那条绝不能讲 ui 的字段，实得 {:?}",
            keys_err[0].1
        );
    }

    /// [`ConfigDegradation::affects`] 的判据必须覆盖子路径。
    ///
    /// 这是 [`Config::materialize_key_actions`] 闸三的判据。写成精确相等会漏判
    /// `keys.key_actions`——而漏判的后果是本该拦下的那次写盘照样发生，把一次可恢复的降级
    /// 变成磁盘上的永久数据丢失。
    #[test]
    fn degradation_affects_matches_subpaths() {
        let d = ConfigDegradation {
            sections: vec!["keys.key_actions".to_string()],
            total_fallback: false,
        };
        assert!(d.affects("keys"), "子路径必须算作该段受影响");
        assert!(!d.affects("ui"), "别的段不能被误判");
        // 前缀相同但不是同一段：`key` 不是 `keys` 的父段，不能靠裸 `starts_with` 匹配上。
        assert!(!d.affects("key"), "只认以 '.' 分隔的真父段");

        let whole = ConfigDegradation {
            sections: vec!["keys".to_string()],
            total_fallback: false,
        };
        assert!(whole.affects("keys"));

        let total = ConfigDegradation {
            sections: Vec::new(),
            total_fallback: true,
        };
        assert!(total.affects("keys"), "整份回落时任何段都受影响");
        assert!(total.affects("ui"));
    }

    /// 多段同时有毒：两段都降级，好段不受牵连。
    ///
    /// 这条守的是「逐段替换直到成功」那种错误实现——毒在 keys 时，替换 input 后仍然失败，
    /// 那种实现分不清 input 是否无辜，会连它一起降级。探针法对每段独立判定。
    #[test]
    fn section_fallback_isolates_multiple_bad_sections() {
        let merged = merged_user_value(&format!("{HEALTHY_USER}{POISON_UI}{POISON_KEYS}"));
        assert!(merged.clone().try_into::<Config>().is_err(), "用例前提");

        let cfg = Config::deserialize_with_section_fallback(merged);

        // 无辜段一个都不能少（同上，先断言用户可见的损失）
        assert_eq!(cfg.schema.active, "section_fallback_probe");
        assert!(!cfg.input.default.chinese_mode);
        assert!(!cfg.stats.enabled);
        assert_eq!(cfg.debug.log_level, "trace");
        assert_eq!(cfg.ui.candidate.per_page, 9);

        assert_eq!(
            cfg.degradation.sections,
            vec!["keys.key_actions".to_string(), "ui.font".to_string()],
            "两处毒各自被定位到子表这一层"
        );
        assert!(cfg.ui.font.scripts.is_empty());
        assert!(cfg.keys.key_actions.is_empty());
    }

    /// 幂等：同一输入连跑两次结果一致；且把降级后的产物再喂回去，不再降级、值也不再变。
    ///
    /// 后半条才是真正的幂等——「加载→写回→再加载」是设置页的实际链路，若第二轮又变一次，
    /// 用户会看到配置在两个状态间来回跳。
    #[test]
    fn section_fallback_is_idempotent() {
        let input = merged_user_value(&format!("{HEALTHY_USER}{POISON_UI}"));

        let first = Config::deserialize_with_section_fallback(input.clone());
        let second = Config::deserialize_with_section_fallback(input);
        // 先钉住「第一轮确实降了级、且好段还在」——否则本用例在「压根没有降级逻辑」的
        // 世界里也是绿的（两轮都得到出厂默认，当然一致），成为一个恒绿的假守门。
        assert_eq!(first.schema.active, "section_fallback_probe");
        assert_eq!(first.degradation.sections, vec!["ui.font".to_string()]);
        assert_eq!(first.degradation, second.degradation);
        assert_eq!(
            toml::Value::try_from(&first).unwrap(),
            toml::Value::try_from(&second).unwrap(),
            "同一输入两次加载须逐键相同"
        );

        // 第二轮：拿第一轮的产物当输入（等价于降级后写回再加载）
        let round_trip = Config::deserialize_with_section_fallback(
            toml::Value::try_from(&first).expect("降级后的配置应可序列化"),
        );
        assert_eq!(
            round_trip.degradation,
            ConfigDegradation::default(),
            "毒已被清掉，第二轮不该再降级"
        );
        assert_eq!(
            toml::Value::try_from(&round_trip).unwrap(),
            toml::Value::try_from(&first).unwrap(),
            "第二轮不得再改动任何键"
        );
    }

    /// 毒不在任何单段（顶层压根不是表）：整份回落 L1 默认，并把这件事标出来。
    /// 段级降级只在「能定位到段」时成立，定位不到时不能假装成功。
    #[test]
    fn section_fallback_reports_total_failure() {
        let cfg = Config::deserialize_with_section_fallback(toml::Value::Integer(42));
        assert!(cfg.degradation.total_fallback, "定位不到有毒段须如实标记");
        assert!(cfg.degradation.sections.is_empty());
        assert_eq!(cfg.schema.active, Config::default().schema.active);
    }

    /// 健康配置零副作用：不降级、不留记录，且与直接 `try_into` 逐键相同。
    #[test]
    fn section_fallback_is_transparent_when_healthy() {
        let merged = merged_user_value(HEALTHY_USER);
        let direct: Config = merged.clone().try_into().expect("健康配置须直接可反序列化");
        let cfg = Config::deserialize_with_section_fallback(merged);
        assert_eq!(cfg.degradation, ConfigDegradation::default());
        assert_eq!(
            toml::Value::try_from(&cfg).unwrap(),
            toml::Value::try_from(&direct).unwrap(),
            "成功路径必须与老行为完全一致"
        );
    }

    /// `degradation` 是 `#[serde(skip)]` 的运行期元信息，不是配置键：既不进注册表覆盖
    /// 检查的叶子集合，也不会被 `config.get` 序列化回去写进用户 config.toml。
    ///
    /// 破坏后的现象很隐蔽：一旦它被序列化，`prune`/`set_user_value` 那套按叶子路径工作的
    /// 逻辑会把它当成用户配置项处理，而 `registry_covers_every_config_key` 会因为多出
    /// 一个未登记键而红——但那时已经有用户的 config.toml 被写进了这个键。
    #[test]
    fn degradation_is_not_a_config_key() {
        let v = toml::Value::try_from(Config::default()).unwrap();
        assert!(
            v.get("degradation").is_none(),
            "降级记录不得出现在配置的序列化产物里"
        );
        assert!(
            !crate::config_schema::config_leaf_keys()
                .iter()
                .any(|k| k.starts_with("degradation")),
            "降级记录不得进入配置叶子路径集合"
        );
    }

    /// ★ `comment_max_chars`（横竖共用）→ 两个方向各一份。配过非 0 值的用户升级后
    /// 注释必须仍按原长度截断，否则表现是「候选栏莫名变宽」，无人会报 bug。
    #[test]
    fn migrate_comment_max_chars_copies_into_both_directions() {
        let mut v: toml::Value =
            toml::from_str("[ui.candidate]\ncomment_max_chars = 12\n").unwrap();
        Config::migrate_comment_max_chars_value(&mut v);
        let cand = v.get("ui").unwrap().get("candidate").unwrap();
        assert_eq!(
            cand.get("comment_max_chars_vertical").unwrap().as_integer(),
            Some(12)
        );
        assert_eq!(
            cand.get("comment_max_chars_horizontal")
                .unwrap()
                .as_integer(),
            Some(12)
        );

        // 幂等：每次启动都跑一遍，第二次不得再改。
        let before = v.clone();
        Config::migrate_comment_max_chars_value(&mut v);
        assert_eq!(v, before, "二次迁移须无变化");
    }

    /// 用户已显式写了新键时，旧键**不得**覆盖它——新键是更明确的意图。
    ///
    /// ⚠️ 同时钉住「只补缺失的那一侧」：只写了竖排的用户，横排仍该从旧键拿到值。
    #[test]
    fn migrate_comment_max_chars_keeps_explicit_new_keys() {
        let mut v: toml::Value = toml::from_str(
            "[ui.candidate]\ncomment_max_chars = 12\ncomment_max_chars_vertical = 30\n",
        )
        .unwrap();
        Config::migrate_comment_max_chars_value(&mut v);
        let cand = v.get("ui").unwrap().get("candidate").unwrap();
        assert_eq!(
            cand.get("comment_max_chars_vertical").unwrap().as_integer(),
            Some(30),
            "显式新键不被旧键盖掉"
        );
        assert_eq!(
            cand.get("comment_max_chars_horizontal")
                .unwrap()
                .as_integer(),
            Some(12),
            "缺失的那一侧仍从旧键补齐"
        );
    }

    /// ⛔ 旧键**不得**进 `RETIRED_KEYS`：它还在被值迁移读取，而那份清单是在**用户文件**上
    /// 做删除、迁移只改内存 ⇒ 登记进去 = 下次启动再也迁不到，用户配的值静默归 0。
    ///
    /// 这条测试钉的是一个「顺手补上去就出事」的改动，故必须显式存在。
    #[test]
    fn retired_keys_excludes_keys_still_read_by_migration() {
        assert!(
            !RETIRED_KEYS.contains(&["ui", "candidate", "comment_max_chars"].as_slice()),
            "comment_max_chars 仍被 migrate_comment_max_chars_value 读取，不能退役"
        );
    }

    #[test]
    fn pinyin_global_config_defaults() {
        let c = Config::default();
        assert!(c.schema.pinyin.show_code_hint);
        assert!(c.schema.pinyin.use_smart_compose);
        assert_eq!(c.schema.pinyin.separator, "auto");
        assert!(!c.schema.pinyin.fuzzy.enabled);
        assert!(!c.schema.pinyin.fuzzy.zh_z);
    }

    /// `auto_learn.max_word_length` 的默认值必须**两条路一致**：代码默认
    /// （`AutoLearnConfig::default()`）与配置默认（serde 缺键）都得是 10。
    ///
    /// 这正是 `AutoLearnConfig` 不能再 `derive(Default)` 的原因——derive 给零值，
    /// 而 `#[serde(default = "…")]` 给 10，同一个语义在两条路上分叉，且分叉只在
    /// 「用户配置里恰好没写这个键」时才显形。
    ///
    /// 拼音造词分支在协调器的 headless 测试里执行不到（`is_pinyin()` 依赖引擎加载，
    /// 那里恒 false），故这条配置接线只能在本 crate 钉住。
    #[test]
    fn auto_learn_max_word_length_default_is_consistent() {
        assert_eq!(
            AutoLearnConfig::default().max_word_length,
            10,
            "代码默认（Default impl）"
        );
        assert_eq!(
            Config::default().schema.pinyin.auto_learn.max_word_length,
            10,
            "经 PinyinGlobal::default() 传导后仍是 10"
        );
        // 配置里写了 [auto_learn] 段但没写本键 → serde default 兜底，仍是 10。
        let c = merged_with("[schema.pinyin.auto_learn]\nenabled = true\n");
        assert_eq!(
            c.schema.pinyin.auto_learn.max_word_length, 10,
            "缺键应走 serde default，而非零值"
        );
        assert!(c.schema.pinyin.auto_learn.enabled, "同段其它键正常覆盖");
        // 显式配置可覆盖；0 = 不限（保留旧行为的逃生舱）。
        let c = merged_with("[schema.pinyin.auto_learn]\nmax_word_length = 0\n");
        assert_eq!(c.schema.pinyin.auto_learn.max_word_length, 0);
    }

    #[test]
    fn pinyin_global_merge_partial() {
        // 仅覆盖 [schema.pinyin.fuzzy] 的 enabled 和 zh_z，其余字段应保留默认值（深合并验证）
        let c = merged_with("[schema.pinyin.fuzzy]\nenabled = true\nzh_z = true\n");
        // 被覆盖字段：变为 true
        assert!(c.schema.pinyin.fuzzy.enabled, "enabled 应被覆盖为 true");
        assert!(c.schema.pinyin.fuzzy.zh_z, "zh_z 应被覆盖为 true");
        // 未覆盖的 fuzzy 字段：保留默认 false
        assert!(!c.schema.pinyin.fuzzy.ch_c, "ch_c 未覆盖，应保留默认 false");
        assert!(!c.schema.pinyin.fuzzy.sh_s, "sh_s 未覆盖，应保留默认 false");
        // 未覆盖的 pinyin 顶层字段：保留默认值
        assert!(
            c.schema.pinyin.show_code_hint,
            "show_code_hint 未覆盖，应保留默认 true"
        );
        assert!(
            c.schema.pinyin.use_smart_compose,
            "use_smart_compose 未覆盖，应保留默认 true"
        );
        assert_eq!(
            c.schema.pinyin.separator, "auto",
            "separator 未覆盖，应保留默认 auto"
        );
    }

    #[test]
    fn test_keys_defaults() {
        // keys 合并 hotkeys + 选择键，默认值需保留（[keys] 整表缺失走 Default）
        let k = Config::default().keys;
        assert_eq!(k.toggle_mode_keys, vec!["lshift", "rshift"]);
        assert_eq!(k.switch_engine, "ctrl+shift+e");
        assert_eq!(k.select_key_groups, vec!["semicolon_quote"]);
        assert_eq!(k.page_keys, vec!["pageupdown", "minus_equal"]);
        assert_eq!(k.overflow.number_key, "ignore");
    }

    #[test]
    fn test_schema_modes_and_input_groups() {
        let c = Config::default();
        // 模式三件套归 schema
        assert_eq!(c.schema.mix_modes.len(), 1, "默认一个快捷 mix");
        // 引导键的**默认值**仍挂在实例上（由 normalize 折算进 keys.key_actions）——
        // 放在被折算的一侧，「用户清空」才折算得出空。理由见 default_mix_modes。
        assert_eq!(c.schema.mix_modes[0].trigger_keys, vec!["semicolon"]);
        assert_eq!(c.schema.quick_input.decimal_places, 6);
        // input 子组
        assert!(
            c.input.punct.smart_after_digit,
            "punct.smart_after_digit 默认开"
        );
        assert_eq!(c.input.symbol.smart_timeout_ms, 500);
        assert!(c.input.temp_english.enabled && c.input.temp_english.show_candidates);
        assert_eq!(c.input.url.prefixes.len(), 5);
        // input.phrase / stats
        assert_eq!(c.input.phrase.min_prefix, 2);
        assert!(c.stats.enabled && c.stats.track_english);
    }

    #[test]
    fn system_preset_without_data_dir_equals_default() {
        let preset = Config::system_preset_value(None).unwrap();
        assert_eq!(preset, toml::Value::try_from(Config::default()).unwrap());
    }

    #[test]
    fn system_preset_applies_config_toml_overrides() {
        let data_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../data"));
        let preset = Config::system_preset_value(Some(data_dir)).unwrap();
        let cfg: Config = preset.try_into().unwrap();
        // config.toml 作为 L2 预置覆盖了空的 code default
        assert_eq!(cfg.schema.active, "wubi86");
    }

    #[test]
    fn test_smart_method_default() {
        let cfg = SymbolConfig::default();
        assert_eq!(cfg.smart_method, SmartMethod::DeleteReplace);
    }

    #[test]
    fn test_smart_method_serde_round_trip() {
        let toml = r#"
smart_mode = true
smart_method = "delete_replace"
"#;
        let cfg: SymbolConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.smart_method, SmartMethod::DeleteReplace);
        assert!(cfg.smart_mode);
    }

    #[test]
    fn test_smart_method_default_when_absent() {
        let toml = r#"smart_mode = true"#;
        let cfg: SymbolConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.smart_method, SmartMethod::DeleteReplace);
    }

    /// 混输简拼**出厂关**。它决定超码长顶码上屏可不可用（简拼候选「消费整串」会让归属恒判给
    /// 拼音，`pinyin_only_overflow` 随即独立拦下顶码），所以这个取值是一项产品决策，不是随手
    /// 挑的默认——没有守门测试的话改回去不会有任何测试变红（实测：只翻这一个值，全量 2754 条
    /// 无一失败）。
    #[test]
    fn mix_pinyin_abbrev_defaults_off() {
        assert!(
            !MixGlobal::default().enable_pinyin_abbrev,
            "简拼出厂应为关；改它前先读 data/config.toml 同名项的注释"
        );
    }

    /// 同源守门：L1（`MixGlobal::default()`）与 L2（`data/config.toml`）必须给出同一个值。
    /// 两者漂移的后果分两层：引擎单测跑在**现实中不存在的配置**下（全绿但保护实际是反的）、
    /// 以及 `preset_for_pruning` 拿 L1⊕L2⊕L2.5 判「用户值是否等于默认」时开始吃用户配置。
    /// 缺 `data/` 时静默跳过（全仓惯例）。
    #[test]
    fn mix_pinyin_abbrev_l1_and_l2_agree() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data");
        let path = dir.join("config.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!(
                "跳过 mix_pinyin_abbrev_l1_and_l2_agree：{} 不存在",
                path.display()
            );
            return;
        };
        let v: toml::Value = toml::from_str(&text).expect("data/config.toml 应能解析");
        let l2 = v
            .get("schema")
            .and_then(|s| s.get("mix"))
            .and_then(|m| m.get("enable_pinyin_abbrev"))
            .and_then(toml::Value::as_bool)
            .expect("data/config.toml 应显式写出 schema.mix.enable_pinyin_abbrev");
        assert_eq!(
            l2,
            MixGlobal::default().enable_pinyin_abbrev,
            "L1 与 L2 的简拼默认值漂移了"
        );
    }

    /// 取值守门：出简让全的**全局**出厂值是关。
    ///
    /// 0.118 曾是 3（全部简码置后）。全局基线是所有码表方案共用的，而「短码首选 = 简码」
    /// 只对五笔这类前缀式简码成立，第三方码表被它静默改了候选顺序。0.119 起改为全局关、
    /// 由方案在 `[engine.codetable]` 里自己声明（见 `wubi86_schema_declares_short_code_yield`）。
    ///
    /// 只翻这个值不会有任何别的测试变红——它是产品决策，本条是它唯一的钉子。
    #[test]
    fn short_code_yield_defaults_off_globally() {
        assert_eq!(
            CodetableGlobal::default().short_code_yield_level,
            0,
            "出简让全的全局出厂应为关；要改先读 default_short_code_yield_level 的注释"
        );
    }

    /// 同源守门：L1（`CodetableGlobal::default()`）与 L2（`data/config.toml`）必须一致。
    /// 漂移的后果同上面那条简拼守门：引擎单测跑在现实中不存在的配置下，且
    /// `preset_for_pruning` 会开始把用户显式设的值误判成默认而删掉。
    /// 缺 `data/` 时静默跳过（全仓惯例）。
    #[test]
    fn short_code_yield_l1_and_l2_agree() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data")
            .join("config.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!(
                "跳过 short_code_yield_l1_and_l2_agree：{} 不存在",
                path.display()
            );
            return;
        };
        let v: toml::Value = toml::from_str(&text).expect("data/config.toml 应能解析");
        let l2 = v
            .get("schema")
            .and_then(|s| s.get("codetable"))
            .and_then(|c| c.get("short_code_yield_level"))
            .and_then(toml::Value::as_integer)
            .expect("data/config.toml 应显式写出 schema.codetable.short_code_yield_level");
        assert_eq!(
            l2 as usize,
            CodetableGlobal::default().short_code_yield_level,
            "出简让全的 L1 与 L2 默认值漂移了"
        );
    }

    /// 出厂方案守门：wubi86 **不**声明出简让全，与第三方码表一样跟随全局。
    ///
    /// 0.119 一度让 wubi86 自带 `short_code_yield_level = 3`，当天撤回。判据是
    /// **用户在全局页做的事必须能作用到出厂方案上**：方案级的 `Some(_)` 恒覆盖全局，
    /// 内置方案给自己配特例，等于把全局页那一项对它变成了摆设——而用户并不知道，
    /// 「方案自带」那个标记藏在方案级码表配置里。
    ///
    /// 完整论证在 `docs/design/codetable-short-code-yields-full.md` §6.3。**方案文件里
    /// 刻意不留这段说明**：它随安装包发布，读者是用户与第三方方案作者，决策档案对他们
    /// 没有用处，只会让人误以为那是一条可以照抄的配置。
    ///
    /// 断言走**真实的反序列化**而不是文本匹配：`CodeTableSpec` 的字段是 `Option` +
    /// `serde(default)`，于是「真的删掉」「注释掉」「键名写错」三者在这里等价，都是
    /// `None`——正是我们想要的等价。用 `is_none()` 而不是比对某个具体值，是为了让
    /// 补回**任何**档位都变红。
    #[test]
    fn wubi86_schema_does_not_declare_short_code_yield() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas")
            .join("wubi86.schema.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!(
                "跳过 wubi86_schema_does_not_declare_short_code_yield：{} 不存在",
                path.display()
            );
            return;
        };
        let schema: crate::schema::Schema =
            toml::from_str(&text).expect("wubi86.schema.toml 应能解析成 Schema");
        assert!(
            schema.engine.codetable.short_code_yield_level.is_none(),
            "出厂方案不该自带出简让全档位（实际 {:?}）——它会让全局页那一项对五笔失效。\
             理由见 docs/design/codetable-short-code-yields-full.md §6.3",
            schema.engine.codetable.short_code_yield_level
        );
    }

    /// 出厂守门：内置拼音方案**不得**自带 `[engine.pinyin].separator`。
    ///
    /// 同 `wubi86_schema_does_not_declare_short_code_yield` 的形状与理由——方案级
    /// `Some(_)` 恒压过全局，一旦出厂方案声明了它，用户在全局页改分隔符对该方案
    /// 完全失效，而「方案自带」这件事藏在方案文件里，从设置页看不出来。
    ///
    /// 这一项**存在的意义恰恰是让用户去覆盖它**（全拼用反引号作分隔符、双拼把反引号
    /// 留给辅助码），所以出厂留空、由用户或第三方方案作者按自己的键位预算填。
    #[test]
    fn builtin_pinyin_schemas_do_not_declare_separator() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas");
        for id in ["pinyin", "shuangpin"] {
            let path = dir.join(format!("{id}.schema.toml"));
            let Ok(text) = std::fs::read_to_string(&path) else {
                eprintln!("跳过 {id}：{} 不存在", path.display());
                continue;
            };
            let schema: crate::schema::Schema =
                toml::from_str(&text).unwrap_or_else(|e| panic!("{id}.schema.toml 应能解析: {e}"));
            assert!(
                schema.engine.pinyin.separator.is_none(),
                "出厂方案 {id} 不该自带 separator（实际 {:?}）——它会让全局页那一项对该方案失效",
                schema.engine.pinyin.separator
            );
        }
    }

    /// 取值守门：空码时按标点**出厂即丢弃废码**，而同族的回车/空格仍是 `commit`。
    ///
    /// 三个值一起断言是刻意的：这组不一致是产品决策（判据见
    /// `default_punct_on_empty_behavior` 的注释），最可能的破坏方式不是有人把 clear 改回
    /// commit，而是有人「顺手把三个统一了」。分开写三条测试拦不住那种改动——它会让被改的
    /// 那条红、另两条绿，看起来像是只动了一个值。
    #[test]
    fn empty_code_behavior_defaults_are_intentionally_asymmetric() {
        let c = InputConfig::default();
        assert_eq!(
            c.punct_on_empty_behavior, "clear",
            "标点出厂应丢弃废码；要改先读 default_punct_on_empty_behavior 的注释"
        );
        assert_eq!(
            c.enter_behavior, "commit",
            "回车须保留上屏原码——它是「我就要这串原码」的唯一出口，别为了统一把它一起改了"
        );
        assert_eq!(
            c.space_on_empty_behavior, "commit",
            "空格暂随回车；改它会静默改写「曾把它设成当时默认值」的用户，须先写 changelog"
        );
    }

    /// 同源守门：L1（`InputConfig::default()`）与 L2（`data/config.toml`）必须给出同一个值。
    /// 漂移的后果有两层：单测跑在**现实中不存在的配置**下（全绿但保护是反的），以及
    /// `preset_for_pruning` 拿 L1⊕L2⊕L2.5 判「用户值是否等于默认」时开始吃用户配置。
    /// 缺 `data/` 时静默跳过（全仓惯例）。
    #[test]
    fn empty_code_behavior_l1_and_l2_agree() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data")
            .join("config.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!(
                "跳过 empty_code_behavior_l1_and_l2_agree：{} 不存在",
                path.display()
            );
            return;
        };
        let v: toml::Value = toml::from_str(&text).expect("data/config.toml 应能解析");
        let l1 = InputConfig::default();
        for (key, expected) in [
            ("enter_behavior", l1.enter_behavior.as_str()),
            (
                "space_on_empty_behavior",
                l1.space_on_empty_behavior.as_str(),
            ),
            (
                "punct_on_empty_behavior",
                l1.punct_on_empty_behavior.as_str(),
            ),
        ] {
            let l2 = v
                .get("input")
                .and_then(|i| i.get(key))
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| panic!("data/config.toml 应显式写出 input.{key}"));
            assert_eq!(l2, expected, "input.{key} 的 L1 与 L2 默认值漂移了");
        }
    }

    #[test]
    fn top_commit_mode_default_is_direct_commit() {
        let c = InputConfig::default();
        assert_eq!(c.top_commit_mode, TopCommitMode::DirectCommit);
    }

    #[test]
    fn top_commit_mode_serde_round_trip() {
        let toml = r#"top_commit_mode = "direct_commit""#;
        let cfg: InputConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.top_commit_mode, TopCommitMode::DirectCommit);
    }

    #[test]
    fn top_commit_mode_absent_defaults_direct_commit() {
        let cfg: InputConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.top_commit_mode, TopCommitMode::DirectCommit);
    }

    /// 工具栏自动隐藏：默认关、超时 5 秒；空表反序列化与 Default 一致。
    #[test]
    fn toolbar_auto_hide_defaults() {
        let tb: ToolbarConfig = toml::from_str("").unwrap();
        assert!(!tb.auto_hide);
        assert_eq!(tb.auto_hide_delay, 5);
        let d = ToolbarConfig::default();
        assert!(!d.auto_hide);
        assert_eq!(d.auto_hide_delay, 5);
    }
}
