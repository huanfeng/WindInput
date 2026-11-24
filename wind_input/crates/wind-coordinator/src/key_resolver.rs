//! 按键解析层：把分层、分作用域的按键配置解析成消费点能直接用的单一视图。
//!
//! 设计见 `docs/design/key-resolver-unification.md`。本模块是该文档 §7 第 1 步的落点。
//!
//! ## 为什么这层必须在 `wind-coordinator`
//!
//! 动作值域（[`BoundAction`]）在 `wind-config`，键名→VK 的解析（`keymap`）在 `wind-keys`，
//! 而 `wind-config` 不能反向依赖 `wind-keys`（后者经 `wind-cmdbar` 依赖它，加进去成环）。
//! `wind-engine` 也不依赖 `wind-keys`，故方案级表的 VK 预编译同样落不到 `EngineManager`。
//! 本 crate 是唯一同时看得见两边的地方——与 `ConfigBundle::session_keys` 同源的理由。
//!
//! ## 当前范围
//!
//! | 表 | 落点 | 形态 |
//! |---|---|---|
//! | 全局 `keys.key_actions` | [`KeyResolver::global_lead`] | 预编译成 VK 表 |
//! | 方案级 `[session_actions]` | [`schema_session_lookup`] | 按键名逐条比对 |
//! | 方案级 `[key_actions]` | `Coordinator::bound_action_with_source` | 按键名逐条比对 |
//!
//! 两层怎么叠（谁覆盖谁、显式 `none` 如何处置）在 `Coordinator` 侧——
//! `bound_action_with_source` 与 `session_action_for`。它们要按活跃方案取表、需要
//! 方案层不在这里加缓存：`EngineManager` 已有 `key_actions_cache` / `session_actions_cache`
//! （随 `invalidate_schema` 失效），再叠一层 VK 预编译就多一个须同步失效的镜像态。
//! `schema_generation` 不能当失效判据——设置页改 `schema_overrides` 走 `invalidate_schema`、不
//! bump 它，表现为改了不生效。要做方案层预编译，得先加一个在 `invalidate_schema` 里 bump 的代际。

use std::collections::HashMap;

use wind_config::hotkey::{KeyActionRoute, route_of_key_action};
use wind_config::{BoundAction, Config};
use wind_keys::keymap;

/// 分层按键配置的解析器。
///
/// 目标形态是 `lead(schema, vk)` / `session(schema, vk)` / `reachability()` 三个方法同源，
/// 当前只实现了 [`Self::global_lead`]（见模块文档「当前范围」）。
pub(crate) struct KeyResolver {
    /// 全局 `keys.key_actions` 的引导键条目，预编译：键名已解析成 VK、动词已解析成
    /// [`BoundAction`]。
    ///
    /// 值含 [`BoundAction::None`]（用户显式写的 `none`）：查得到就是「全局层表了态」，
    /// 表态为禁用同样不再往下回落到 `z_key_action`。故本表不能用「查不到」表示禁用。
    global_lead: HashMap<u32, BoundAction>,
}

impl KeyResolver {
    /// 从配置预编译。随 `ConfigBundle` 重建（热重载、RPC 改配置、测试构造都经它）。
    pub(crate) fn build(config: &Config) -> Self {
        Self {
            global_lead: compile_global_lead(&config.keys.key_actions),
        }
    }

    /// 全局 `keys.key_actions` 里这个键绑了什么。
    ///
    /// 只含单键条目（引导键 + 纯修饰键）：组合键由热键通路消费，在这里再认一次
    /// 就是同一个键两条路都触发。过滤在预编译期完成。
    pub(crate) fn global_lead(&self, key_code: u32) -> Option<BoundAction> {
        self.global_lead.get(&key_code).cloned()
    }
}

/// 键名 → VK。两张表叠加，缺一不可。
///
/// 修饰键的键名不在 `KEY_TABLE` 里（那是引导键的解析口，走 keydown，修饰键在那条路上
/// 不工作），故必须显式并进来。少了这一层的表现是「转发集里有这个键、TSF 也发了 keyup，
/// 但查表查不到、什么都不发生」——已在测试里复现过一次。
pub(crate) fn key_action_name_to_vk(name: &str) -> Option<u32> {
    keymap::key_name_to_vk_with_letters(name).or_else(|| keymap::modifier_name_to_vk(name))
}

/// 在方案级 `[session_actions]` 表里查一个键。
///
/// 返回 `Some` 即本方案对这个键表了态，包括 [`SessionAction::None`]（显式 `"none"` = 本方案
/// 禁用该键），调用方不得再回落全局——`merge_toml` 无法表达删除，禁用只能靠显式值承载。
///
/// 匹配走 [`keymap::SessionKey::matches`]，与全局表的 `KeyBinds::classify` 同一个谓词。不预编译：
/// 理由见模块文档末段；表通常只有几条。
pub(crate) fn schema_session_lookup(
    table: &std::collections::BTreeMap<String, String>,
    key_code: u32,
    shift: bool,
    include_printable: bool,
) -> Option<wind_config::SessionAction> {
    for (name, verb) in table {
        let Some(k) = keymap::session_key_name_to_vk(name) else {
            continue;
        };
        if k.matches(key_code, shift, include_printable) {
            return Some(wind_config::SessionAction::parse(verb));
        }
    }
    None
}

/// 把 `keys.key_actions` 编译成 VK 表。
///
/// 先到先得：源表是 `BTreeMap`，两个键名解析到同一个 VK 时（大小写写法不一）必须由字典序在前
/// 的那条获胜，故撞键时 `continue` 保留先到者。改成直接 `insert` 会静默反转胜者。
fn compile_global_lead(
    table: &std::collections::BTreeMap<String, String>,
) -> HashMap<u32, BoundAction> {
    let mut out: HashMap<u32, BoundAction> = HashMap::new();
    // 撞键时报出来：分散查表时「谁赢」是遍历顺序的副产物，没有任何一处代码有资格报错；
    // 收敛成单表后构建期天然拿到全集，冲突检测从「要额外实现」变成「顺手就有」。
    // 判据同 `warn_unknown_session_actions`——静默忽略与「这个功能坏了」完全同形。
    let mut winners: HashMap<u32, &str> = HashMap::new();
    for (name, action) in table {
        // 只收单键。组合键走热键通路（`Compiler::compile` 已按形态分流）。
        if !matches!(
            route_of_key_action(name),
            Some(KeyActionRoute::LeadingKey) | Some(KeyActionRoute::ModifierKeyUp)
        ) {
            continue;
        }
        let Some(vk) = key_action_name_to_vk(name) else {
            continue;
        };
        if let Some(prev) = winners.get(&vk) {
            tracing::warn!(
                "key_actions: 键名 {name:?} 与 {prev:?} 解析到同一个键（vk=0x{vk:02X}），\
                 只有 {prev:?} 生效。删掉其中一条以消除歧义。"
            );
            continue;
        }
        winners.insert(vk, name.as_str());
        out.insert(vk, BoundAction::parse(action));
    }
    out
}

#[cfg(test)]
#[path = "key_resolver_tests.rs"]
mod tests;
