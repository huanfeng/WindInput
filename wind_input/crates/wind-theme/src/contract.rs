//! 标准色契约的引擎兜底（设计 `docs/design/text-span-colors.md` §5.4「引擎兜底」）。
//!
//! 契约名（候选注释 / 悬停提示模板里 `$[名]{…}` 写的那些名字）由 `_base` 定义，继承 `_base`
//! 的主题天然都有。没写 `base` 的第三方主题一个也没有，`$[error]{…}` 就静默回落正文色。本模块
//! 在调色板解析后把**主题没写的**契约名补上：主题写了的一个不动，契约外的名字一个不加
//! （不是隐式继承 `_base`——那会连带改掉第三方主题的布局与其它颜色）。
//!
//! 补值的唯一来源是编译期嵌入的 `_base/theme.toml` 本身，而不是一张手抄的常量表：两处写同一组
//! 值，迟早只改一处，而这里漂移了没有任何症状（只有无 base 主题看得到）。嵌入而不是运行时从
//! 主题目录读：兜底不该依赖搜索链里恰好有 `_base`（设置页预览、移动端拉调色板各走各的目录）。
//! 代价是本 crate 依赖仓内 `data/` 布局：单独 `cargo package` / vendor 本 crate 会编译失败
//! （失败是显式的；各端构建都在完整仓内，改 `_base` 会经 dep-info 触发重编）。
//!
//! 引用的解析口径：
//! - 契约名之间的引用按**本主题**解析——`selection_text = "${text}"` 取本主题的 text，
//!   `accent_text = "${accent}"` 取本主题的强调色；本主题也没有，再取兜底值。
//! - 契约外的名字（`accent = "${primary}"` 里的 `primary`）只取 `_base` 自己的值：它不在契约
//!   里，第三方主题的同名色未必是同一个意思。
//! - 例外：主题没写 `tooltip_bg` 时，`tooltip_text` 取渲染层常量
//!   [`fallback::TOOLTIP_TEXT`](crate::fallback::TOOLTIP_TEXT)，理由见 [`fill_missing`]。

use std::collections::HashMap;
use std::sync::OnceLock;

use toml::Value;

use crate::fallback::TOOLTIP_TEXT;
use crate::palette::{Rgba, resolve_palette};

/// 候选窗用的契约名；每个在气泡里另有 `tooltip_<名>`（§5.4）。
pub const NAMES: [&str; 11] = [
    "text",
    "text_dim",
    "text_hint",
    "accent",
    "on_accent",
    "selection_text",
    "accent_text",
    "success",
    "warning",
    "error",
    "info",
];

/// 全部契约名：[`NAMES`] 及其 `tooltip_*`。
pub fn all_names() -> impl Iterator<Item = String> {
    NAMES
        .iter()
        .flat_map(|n| [n.to_string(), format!("tooltip_{n}")])
}

const BASE_THEME: &str = include_str!("../../../../data/themes/_base/theme.toml");

/// `_base` 的 `[colors]` 原表（未求值）。
fn base_colors() -> &'static toml::Table {
    static COLORS: OnceLock<toml::Table> = OnceLock::new();
    COLORS.get_or_init(|| {
        let v: Value = BASE_THEME
            .parse()
            .expect("嵌入的 _base/theme.toml 解析失败");
        v.get("colors")
            .and_then(Value::as_table)
            .cloned()
            .expect("嵌入的 _base/theme.toml 没有 [colors]")
    })
}

/// 把主题 `colors` 段里**没写**的契约名补进已解析的 `palette`。
///
/// 「写没写」看 `colors` 的键而不是解析结果：主题写了却解析不出（坏值、`transparent`、断链）
/// 是作者自己的选择，兜底不替它改成 `_base` 的色。
pub fn fill_missing(colors: Option<&Value>, palette: &mut HashMap<String, Rgba>, is_dark: bool) {
    let own = colors.and_then(Value::as_table);
    let written = |n: &str| own.is_some_and(|t| t.contains_key(n));
    // `_base` 原表垫底，主题写了的契约名换成本主题的终值（解析不出的删掉，引用它的也跟着
    // 解析不出，与主题里写 `${它}` 的结果一致）；契约外的名字保持 `_base` 的原样。
    let hex = |c: &Rgba| format!("#{:02X}{:02X}{:02X}{:02X}", c[0], c[1], c[2], c[3]);
    let mut table = base_colors().clone();
    // 主题没有可用的气泡底色（没写，或写了解析不出）：气泡底是渲染层常量（深灰），`tooltip_text`
    // 兜底取渲染层原来的常量，外观与没有兜底时完全一致。`_base` 的暗档 `${text}` 是配它自己的
    // 深色气泡底的，借本主题 text 在这里会出问题：只调了浅色、text 写成单个深色值的主题，暗色下
    // 成了深字压深底（2026-09-28 确认）。引用它的 tooltip_on_accent 等随之取同一个值。主题自己写了
    // tooltip_text 的，下面的循环照常换回本主题的值。
    // 判「可用」看解析结果而不是写没写：渲染层取底色也是按解析结果（`theme.color("tooltip_bg")`）。
    if !palette.contains_key("tooltip_bg") {
        table.insert("tooltip_text".into(), Value::String(hex(&TOOLTIP_TEXT)));
    }
    for n in all_names().filter(|n| written(n)) {
        match palette.get(&n) {
            Some(c) => {
                table.insert(n, Value::String(hex(c)));
            }
            None => {
                table.remove(&n);
            }
        }
    }
    let fallback = resolve_palette(Some(&Value::Table(table)), is_dark);
    for n in all_names().filter(|n| !written(n)) {
        if let Some(c) = fallback.get(&n) {
            palette.insert(n, *c);
        }
    }
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod tests;
