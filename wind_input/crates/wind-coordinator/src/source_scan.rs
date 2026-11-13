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
mod tests {
    use super::*;

    #[test]
    fn strips_every_test_item_not_just_the_first() {
        let src = "fn a() {}\n#[cfg(test)]\nthread_local! { static X: u8 = 0; }\n\
                   fn b() { x.apply(1) }\n#[cfg(test)]\nuse foo::bar;\nfn c() {}\n\
                   #[cfg(test)]\nmod tests { fn t() { let s = format!(\"{x}\"); } }\nfn d() {}\n";
        let prod = production_code(src);
        for keep in ["fn a()", "fn b()", "x.apply(1)", "fn c()", "fn d()"] {
            assert!(prod.contains(keep), "误删了生产代码 {keep}：{prod}");
        }
        for gone in ["thread_local!", "use foo::bar", "mod tests", "fn t()"] {
            assert!(!prod.contains(gone), "没剔掉测试条目 {gone}：{prod}");
        }
    }

    #[test]
    fn brace_in_strings_comments_and_doc_attr_mentions_do_not_swallow() {
        let src = "/// 断言在 `#[cfg(test)]` 里\nfn doc() {}\n\
                   struct S {\n    #[cfg(test)]\n    probe: usize,\n    kept: u8,\n}\n\
                   #[cfg(test)]\nmod t {\n    // 注释里的 {\n    const C: char = '{';\n    \
                   fn f() { let _ = \"${\"; }\n}\nfn tail() {}\n\
                   #[cfg(all(test, windows))]\nmod w {\n    fn g() {}\n}\nfn last() {}\n";
        let prod = production_code(src);
        for keep in ["fn doc()", "kept: u8", "fn tail()", "fn last()"] {
            assert!(prod.contains(keep), "误删了生产代码 {keep}：{prod}");
        }
        for gone in ["probe", "mod t", "fn f()", "mod w", "fn g()"] {
            assert!(!prod.contains(gone), "没剔掉测试条目 {gone}：{prod}");
        }
    }

    /// 真实文件上的覆盖：三个文件的测试模块之后都还有生产代码，曾因花括号失配被整段吞掉。
    #[test]
    fn keeps_known_trailing_production_fns() {
        for (name, src, tail_fn) in [
            (
                "coordinator.rs",
                include_str!("coordinator.rs"),
                "pub(crate) fn merge_comment_dicts(",
            ),
            (
                "coordinator.rs",
                include_str!("coordinator.rs"),
                "pub(crate) fn pick_index_label(",
            ),
            (
                "handle_candidate.rs",
                include_str!("handle_candidate.rs"),
                "pub(crate) fn tail_after_letters(",
            ),
            (
                "handle_mode.rs",
                include_str!("handle_mode.rs"),
                "pub(crate) fn mix_member_fetch_limit(",
            ),
        ] {
            let prod = production_code(src);
            assert!(
                prod.contains(tail_fn),
                "{name}: production_code 吞掉了 {tail_fn}"
            );
            assert!(
                !prod.contains("#[cfg(test)]\nmod "),
                "{name}: 仍有未剔除的测试模块"
            );
        }
    }

    fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// 兜底臂的模式部分：`_`、绑定名、`Some(<它们>)`，可再并一个 `None`。
    fn is_catch_all(head: &str) -> bool {
        let binding = |t: &str| {
            t == "_"
                || t.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                    && t.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        };
        head.split('|').map(str::trim).any(|alt| {
            let inner = alt
                .strip_prefix("Some(")
                .and_then(|r| r.strip_suffix(')'))
                .map(|r| r.trim().trim_end_matches(',').trim());
            binding(alt) || inner.is_some_and(binding)
        })
    }

    /// 以模式为键的 `match` 不得有兜底臂（`_` / `Some(_)` / 绑定名）：新增 `ModeKind` 变体时要由
    /// 编译器逐处列出接入点，兜底臂会让新模式静默落进「其余」分支（辅助码来源曾因此把临拼当成
    /// 主输入路）。按 rustfmt 排版识别：`match … {` 之后缩进深一级的行是各臂头；臂头（`=>` 之前，
    /// 含折行的 `Some(\n ModeKind::A | …\n)`）里出现 `ModeKind::` 即算以模式为键。
    #[test]
    fn mode_keyed_matches_have_no_wildcard_arm() {
        let mut files = Vec::new();
        rs_files(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let indent = |t: &str| t.len() - t.trim_start().len();
        let mut bad = Vec::new();
        let mut keyed_matches = 0usize;
        for f in &files {
            let prod = production_code(&std::fs::read_to_string(f).unwrap());
            let lines: Vec<&str> = prod.lines().collect();
            for (n, line) in lines.iter().enumerate() {
                if !(line.contains("match ") && line.trim_end().ends_with('{')) {
                    continue;
                }
                let outer = indent(line);
                let Some(arm_ind) = lines[n + 1..]
                    .iter()
                    .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("//"))
                    .map(|l| indent(l))
                    .filter(|&i| i > outer)
                else {
                    continue;
                };
                // 收集各臂头：从臂缩进的行起，到含 `=>` 的行为止（折行模式跨多行）。
                let mut heads: Vec<String> = Vec::new();
                let mut cur: Option<String> = None;
                for l in &lines[n + 1..] {
                    let li = indent(l);
                    if !l.trim().is_empty() && li <= outer {
                        break;
                    }
                    let t = l.trim_start();
                    if li == arm_ind && cur.is_none() && !t.starts_with("//") && !t.starts_with('}')
                    {
                        cur = Some(String::new());
                    }
                    if let Some(h) = cur.as_mut() {
                        let part = l.split("=>").next().unwrap_or("");
                        h.push_str(part.trim());
                        h.push(' ');
                        if l.contains("=>") {
                            heads.push(cur.take().unwrap());
                        }
                    }
                }
                if !heads.iter().any(|h| h.contains("ModeKind::")) {
                    continue;
                }
                keyed_matches += 1;
                for h in &heads {
                    let pat = h.split(" if ").next().unwrap_or("").trim();
                    if is_catch_all(pat) {
                        bad.push(format!("{}:{}: `{pat} =>`", f.display(), n + 1));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "以下以模式为键的 match 有兜底臂，改成显式列出其余 ModeKind 变体：\n  {}",
            bad.join("\n  ")
        );
        // 2026-11-13 实扫 29 个（含 origin 与 `state.active?`）；扫描失效时会掉到个位数。
        assert!(
            keyed_matches >= 29,
            "只扫到 {keyed_matches} 个以模式为键的 match，扫描方式失效了"
        );
    }

    #[test]
    fn catch_all_heads() {
        for yes in [
            "_",
            "Some(_)",
            "None | Some(_)",
            "other",
            "m2",
            "Some(m)",
            "Some( _, )",
        ] {
            assert!(is_catch_all(yes), "{yes}");
        }
        for no in [
            "None",
            "Some(ModeKind::Url)",
            "None | Some(ModeKind::Mix(_))",
        ] {
            assert!(!is_catch_all(no), "{no}");
        }
    }

    #[test]
    fn reads_pass_ctx_fields() {
        let prod = "run(&PassCtx { owner: None, freq_code: Some(&a), shadow_code: None, });\n\
                    run(&PassCtx { owner: x.as_deref(), freq_code: None, })";
        assert_eq!(
            pass_ctx_field_values(prod, "shadow_code"),
            vec![Some("None".to_string()), None]
        );
        assert_eq!(
            pass_ctx_field_values(prod, "owner"),
            vec![Some("None".to_string()), Some("x.as_deref()".to_string())]
        );
    }
}
