//! 源码扫描守卫的共用工具（仅测试）。
//!
//! 本 crate 有几条「N 个调用点都要做同一件事」的不变量靠扫描源码守（shadow 取码同源、方案归属
//! 同源、emoji 各路都接）。它们此前用 `src.split("#[cfg(test)]").next()` 取生产代码——遇到
//! 文件中段的第一个 `#[cfg(test)]` 就截断，`coordinator.rs`（两万行，第一个 `#[cfg(test)]` 在
//! 九百多行处）后面的生产代码一直在盲区里。

/// 剔除每个 `#[cfg(test)]` / `#[cfg(all(test, …))]` 修饰的条目，返回剩下的生产代码。
///
/// 按 rustfmt 排版定边界，不配对花括号（字符串、注释、`'{'` 里的花括号会让配对失准，曾把三个
/// 文件的尾部生产代码整段吞掉）：
/// - 属性须独占一行（文档注释里提到它不算）；之后连着的属性行一并跳过；
/// - 条目首行以 `;` / `,` / `}` 结尾 ⇒ 单行条目（`use`、字段、`fn a() {}`）；
/// - 否则跳到首个「与属性同缩进、以 `}` 开头」的行为止（`mod`、`fn`、`impl`、`use {…};`）。
///
/// 真实文件上的覆盖由 `keeps_known_trailing_production_fns` 钉住。
pub(crate) fn production_code(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut lines = src.lines();
    while let Some(line) = lines.next() {
        let attr = line.trim();
        if attr != "#[cfg(test)]" && !attr.starts_with("#[cfg(all(test,") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        let Some(mut first) = lines.next() else { break };
        while first.trim_start().starts_with("#[") {
            let Some(next) = lines.next() else { break };
            first = next;
        }
        let head = first.trim_end();
        if head.ends_with(';') || head.ends_with(',') || head.ends_with('}') {
            continue;
        }
        for l in lines.by_ref() {
            if l.strip_prefix(indent).is_some_and(|r| r.starts_with('}')) {
                break;
            }
        }
    }
    out
}

/// 生产代码里每个 `PassCtx { … }` 字面量的 `field` 取值（去掉首尾空白与前导 `&`）。
/// 字段缺失时给 `None`，由调用方决定算不算违规。
pub(crate) fn pass_ctx_field_values(prod: &str, field: &str) -> Vec<Option<String>> {
    let key = format!("{field}:");
    prod.match_indices("PassCtx {")
        .map(|(off, _)| {
            let body = &prod[off..];
            let body = &body[..body.find('}').unwrap_or(body.len())];
            body.find(&key).map(|f| {
                let val = &body[f + key.len()..];
                val[..val.find(',').unwrap_or(val.len())]
                    .trim()
                    .trim_start_matches('&')
                    .trim()
                    .to_string()
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "source_scan_tests.rs"]
mod tests;
