use super::*;
use wind_theme::{Atom, ColorRef};

fn ev<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str, Option<&str>) -> Option<String> + 'a {
    move |n, _| {
        pairs
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.to_string())
    }
}

/// `(文字, 角色, 颜色名)` 逐段列出；颜色名取常态亮侧原子的名字 / `#` / `!`（非法）。
fn spans(t: &StyledText) -> Vec<(String, Option<String>, Option<String>)> {
    t.spans()
        .iter()
        .map(|s| {
            let color = s.color.as_ref().map(|c| match &c.normal.light {
                Atom::Name(n) => n.to_string(),
                Atom::Rgba(_) => "#".to_string(),
                Atom::Invalid => "!".to_string(),
            });
            (
                t.as_str()[s.start as usize..s.end as usize].to_string(),
                s.role.map(str::to_string),
                color,
            )
        })
        .collect()
}

fn sp(
    text: &str,
    role: Option<&str>,
    color: Option<&str>,
) -> (String, Option<String>, Option<String>) {
    (text.into(), role.map(Into::into), color.map(Into::into))
}

/// 去掉模板里全部 `$[…]{` 与对应的 `}`（仅供「纯加法」断言，用例里的 BODY 不含别的 `}`
/// 以外的结构时足够）：按解析树重建文字，不靠字符串替换。
fn strip_colors(tpl: &str) -> String {
    fn walk(nodes: &[Node], out: &mut String) {
        for n in nodes {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::Var(refs) => {
                    out.push_str("${");
                    let names: Vec<String> = refs
                        .iter()
                        .map(|r| match &r.arg {
                            Some(a) => format!("{}:{a}", r.name),
                            None => r.name.clone(),
                        })
                        .collect();
                    out.push_str(&names.join("|"));
                    out.push('}');
                }
                Node::Group(inner) => {
                    out.push('{');
                    walk(inner, out);
                    out.push('}');
                }
                Node::Color(_, inner) => walk(inner, out),
            }
        }
    }
    let mut out = String::new();
    walk(&parse(tpl), &mut out);
    out
}

/// 「加颜色」是纯加法：包上 `$[…]{}` 前后，文字输出逐字节相同（§4.3）。
#[test]
fn wrapping_in_color_is_purely_additive() {
    let cases = [
        "$[accent]{${a}}",
        "(${a} $[accent]{${b}})",
        "{$[accent]{(${a})}}",
        "$[accent]{{(${a})}}",
        "${a}$[#C00000/#FF8080]{(${b})}",
        "$[text_dim,selected=on_accent]{${a}}：$[warning]{${b}}",
        "${a}{ $[success]{[${b}]}}",
        "$[x]{$[y]{${a}} ${b}}",
        "$[红]{${a}}",
    ];
    let fixtures: [&[(&str, &str)]; 4] = [
        &[("a", "nǐ"), ("b", "亻尔")],
        &[("a", "nǐ"), ("b", "")],
        &[("a", ""), ("b", "亻尔")],
        &[("a", ""), ("b", "")],
    ];
    for tpl in cases {
        let plain = strip_colors(tpl);
        assert!(!plain.contains("$["), "{tpl}: 剥色后仍有 $[");
        for fx in fixtures {
            assert_eq!(
                render(tpl, 0, ev(fx)),
                render(&plain, 0, ev(fx)),
                "{tpl} vs {plain}，取值 {fx:?}"
            );
            let t = Template::parse(tpl).render(&ev(fx), &|_| true);
            let p = Template::parse(&plain).render(&ev(fx), &|_| true);
            assert_eq!(t, p, "Template {tpl} vs {plain}，取值 {fx:?}");
        }
    }
}

/// 内联色只管上色、不是可选段：内部变量全空时它自己不消失（`()` 留着）。
#[test]
fn color_is_not_an_optional_group() {
    let t = render("${b}$[accent]{(${a})}", 0, ev(&[("a", ""), ("b", "x")]));
    assert_eq!(t, "x()");
    // 要「空则消失」就套一层可选段，两种写法等价。
    for tpl in ["${b}{$[accent]{(${a})}}", "${b}$[accent]{{(${a})}}"] {
        assert_eq!(render(tpl, 0, ev(&[("a", ""), ("b", "x")])), "x", "{tpl}");
    }
}

/// 内部变量计入外层「有值」判定：外层可选段因颜色段里的变量而保留。
#[test]
fn color_body_vars_count_for_enclosing_group() {
    let t = render("{[$[accent]{${a}}]}", 0, ev(&[("a", "x")]));
    assert_eq!(t, "[x]");
}

/// 空变量吞空白跨颜色边界照吞：`${chaizi}` 为空时 `)` 前不留空格；
/// 同一模板经 `Template::render_styled`（不 trim）渲染，行尾也不多空格。
#[test]
fn whitespace_swallow_crosses_color_boundary() {
    let fx = [("pinyin", "nǐ"), ("chaizi", "")];
    assert_eq!(
        render("(${pinyin} $[accent]{${chaizi}})", 0, ev(&fx)),
        "(nǐ)"
    );
    let (t, _) =
        Template::parse("${pinyin} $[accent]{${chaizi}}").render_styled(&ev(&fx), &|_| true, false);
    assert_eq!(t.as_str(), "nǐ");
    assert_eq!(spans(&t), vec![sp("nǐ", Some("pinyin"), None)]);
}

/// 角色：变量值 = 归一后的变量名（别名、回退链取到的那个）；字面 = literal；
/// 未知变量回显无角色（不建区间）。
#[test]
fn roles_follow_the_variable_that_produced_the_text() {
    let fx = [("code_hint", ""), ("code", "wq"), ("pinyin", "nǐ")];
    let t = render_styled("${code_hint|code}(${pinyin}) ${pinyn}", 0, ev(&fx));
    assert_eq!(t.as_str(), "wq(nǐ) ${pinyn}");
    assert_eq!(
        spans(&t),
        vec![
            sp("wq", Some("code_rev"), None),
            sp("(", Some("literal"), None),
            sp("nǐ", Some("pinyin"), None),
            sp(") ", Some("literal"), None),
        ]
    );
    assert_eq!(role_of("code_all"), "code_rev_all");
}

/// 颜色栈：内联色标在其内的全部片段上（嵌套取内层），字面文字也带色。
#[test]
fn inline_color_marks_everything_inside() {
    let t = render_styled(
        "$[accent]{(${a}$[#F80]{${b}})}",
        0,
        ev(&[("a", "x"), ("b", "y")]),
    );
    assert_eq!(t.as_str(), "(xy)");
    assert_eq!(
        spans(&t),
        vec![
            sp("(", Some("literal"), Some("accent")),
            // `a` / `b` 不在契约清单里：没有角色，但仍带内联色。
            sp("x", None, Some("accent")),
            sp("y", None, Some("#")),
            sp(")", Some("literal"), Some("accent")),
        ]
    );
}

/// 语法成立与否只看结构，不看颜色写得对不对（§4.4）。
#[test]
fn syntax_validity_is_structural() {
    let fx = [("a", "x")];
    // 颜色非法：语法成立，只显示 BODY。
    for tpl in ["$[红]{${a}}", "$[]{${a}}", "$[#GG]{${a}}"] {
        let t = render_styled(tpl, 0, ev(&fx));
        assert_eq!(t.as_str(), "x", "{tpl}");
        let c = t.spans()[0].color.as_ref().expect("仍是内联色");
        assert_eq!(
            c.normal,
            ColorRef {
                light: Atom::Invalid,
                dark: Atom::Invalid
            }
        );
    }
    // 结构不成立：`$` 按字面、从 `[` 继续扫描——与引入内联色之前的引擎逐字节相同。
    let long = format!("$[{}]{{${{a}}}}", "a".repeat(65));
    for tpl in [
        "$[accent${a}",
        "$[accent]${a}",
        "$[acc\nent]{${a}}",
        "$[a{b]{${a}}",
        "$[a]{${a}",
        long.as_str(),
    ] {
        let got = render(tpl, 0, ev(&fx));
        assert!(got.starts_with("$["), "{tpl:?} 不成立，应留字面：{got}");
        assert_eq!(got, super::legacy_ref::render(tpl, 0, ev(&fx)), "{tpl:?}");
    }
    let ok64 = format!("$[{}]{{${{a}}}}", "a".repeat(64));
    assert_eq!(render(&ok64, 0, ev(&fx)), "x", "恰 64 字节成立");
}

/// 截断标记 `…` 继承被截处前一个字的样式。
#[test]
fn truncation_mark_inherits_style() {
    let t = render_styled("$[accent]{${a}}", 2, ev(&[("a", "abcd")]));
    assert_eq!(t.as_str(), "ab…");
    assert_eq!(spans(&t), vec![sp("ab…", None, Some("accent"))]);
}

/// 段名模板：字面文字角色 title、变量片段 in_title。
#[test]
fn title_context_marks_literals_and_vars() {
    let (t, _) = Template::parse("编码{(${code_source})}").render_styled(
        &ev(&[("code_source", "五笔")]),
        &|_| true,
        true,
    );
    assert_eq!(t.as_str(), "编码(五笔)");
    assert_eq!(
        spans(&t),
        vec![
            sp("编码(", Some("title"), None),
            sp("五笔", Some("code_source"), None),
            sp(")", Some("title"), None),
        ]
    );
    assert!(t.spans().iter().all(|s| s.in_title));
}

/// 可选段在内联色里（`$[x]{{…}}`）：段内片段照样带外层的内联色——`Group` 的子构建器
/// 必须继承颜色栈，否则套一层可选段颜色就丢了。
#[test]
fn group_inside_color_keeps_the_color() {
    let t = render_styled("$[accent]{{(${code_rev})}}", 0, ev(&[("code_rev", "wq")]));
    assert_eq!(t.as_str(), "(wq)");
    assert_eq!(
        spans(&t),
        vec![
            sp("(", Some("literal"), Some("accent")),
            sp("wq", Some("code_rev"), Some("accent")),
            sp(")", Some("literal"), Some("accent")),
        ]
    );
}

/// 可选段 → 内联色 → 可选段 的嵌套：最内层取最近的内联色，段外的字面不带色。
#[test]
fn nested_group_color_group() {
    let t = render_styled("[{<$[warning]{{${pinyin}}!}>}]", 0, ev(&[("pinyin", "nǐ")]));
    assert_eq!(t.as_str(), "[<nǐ!>]");
    assert_eq!(
        spans(&t),
        vec![
            sp("[<", Some("literal"), None),
            sp("nǐ", Some("pinyin"), Some("warning")),
            sp("!", Some("literal"), Some("warning")),
            sp(">]", Some("literal"), None),
        ]
    );
}

/// 两个 walker 必须下钻 Color 节点（§8.1 ⚠️）。
#[test]
fn walkers_descend_into_color() {
    let t = Template::parse("$[x]{${debug}}");
    assert!(
        t.references("debug"),
        "漏下钻 ⇒ 调试上下文不准备、段静默为空"
    );
    let t = Template::parse("$[x]{a\tb}");
    assert!(t.has_literal('\t'), "漏下钻 ⇒ 分列段被折行");
}
