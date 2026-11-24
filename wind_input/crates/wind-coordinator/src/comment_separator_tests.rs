use super::*;
use crate::coordinator::Coordinator;

/// `(文字, 角色, 是分隔符)` 逐段。
fn pieces(t: &StyledText) -> Vec<(&str, Option<&str>, bool)> {
    t.spans()
        .iter()
        .map(|s| {
            (
                &t.as_str()[s.start as usize..s.end as usize],
                s.role,
                s.separator,
            )
        })
        .collect()
}

/// `name` 取 `items` 以参数（缺省 `default`）连接。
fn items<'a>(
    name: &'a str,
    items: &'a [&'a str],
    default: &'a str,
) -> impl Fn(&str, Option<&str>) -> Option<VarValue> + 'a {
    move |n, arg| (n == name).then(|| VarValue::joined(items, arg.unwrap_or(default)))
}

#[test]
fn separator_is_flagged_and_keeps_variable_role() {
    let t = render_styled(
        "${chaizi_all:／}",
        0,
        items("chaizi_all", &["亻尔", "女子"], " "),
    );
    assert_eq!(t.as_str(), "亻尔／女子");
    assert_eq!(
        pieces(&t),
        vec![
            ("亻尔", Some("chaizi_all"), false),
            ("／", Some("chaizi_all"), true),
            ("女子", Some("chaizi_all"), false),
        ]
    );
    // 别名归一后的角色同样带到分隔符上。
    let t = render_styled(
        "${dict_all}",
        0,
        items("dict_all", &["a", "b"], DICT_ALL_SEP),
    );
    assert_eq!(
        pieces(&t),
        vec![
            ("a", Some("dict"), false),
            ("；", Some("dict"), true),
            ("b", Some("dict"), false),
        ]
    );
}

/// 分隔符是空格时只标拼接处，值内部的空格不算——这正是不在渲染时按 `arg` 切开的理由。
#[test]
fn space_separator_does_not_split_inside_items() {
    let t = render_styled(
        "${dict_all: }",
        0,
        items("dict_all", &["a b", "c"], DICT_ALL_SEP),
    );
    assert_eq!(t.as_str(), "a b c");
    assert_eq!(
        pieces(&t),
        vec![
            ("a b", Some("dict"), false),
            (" ", Some("dict"), true),
            ("c", Some("dict"), false),
        ]
    );
}

/// 边界：空分隔符、单项不产分隔符片段；空分项照样占位（同 `join`）；全空 = 变量为空。
#[test]
fn edge_cases() {
    let t = render_styled("${chaizi_all:}", 0, items("chaizi_all", &["a", "b"], " "));
    assert_eq!(pieces(&t), vec![("ab", Some("chaizi_all"), false)]);
    let t = render_styled("${chaizi_all:/}", 0, items("chaizi_all", &["a"], " "));
    assert_eq!(pieces(&t), vec![("a", Some("chaizi_all"), false)]);
    let t = render_styled("${chaizi_all:/}", 0, items("chaizi_all", &["", "b"], " "));
    assert_eq!(
        pieces(&t),
        vec![
            ("/", Some("chaizi_all"), true),
            ("b", Some("chaizi_all"), false)
        ]
    );
    let tpl = "${code_hint}{(${chaizi_all:/})}";
    let e = |n: &str, a: Option<&str>| match n {
        "code_hint" => Some(VarValue::from("kh".to_string())),
        _ => items("chaizi_all", &[""], " ")(n, a),
    };
    assert_eq!(render(tpl, 0, e), "kh", "分项全空 = 变量为空，可选段消失");
    let t = render_styled("${chaizi_all:/}", 0, items("chaizi_all", &[], " "));
    assert!(t.is_empty());
}

/// 内联色里的分隔符：带内联色（内联色优先于一切角色色），仍有分隔符标记。
#[test]
fn separator_inside_inline_color() {
    let t = render_styled(
        "$[accent]{${chaizi_all:/}}",
        0,
        items("chaizi_all", &["a", "b"], " "),
    );
    let sp = &t.spans()[1];
    assert!(sp.separator && sp.color.is_some());
}

/// 纯文本：任意分项 × 分隔符，`VarValue` 与改前的 `join` 逐字相同；整条模板的纯文本出口
/// （`render`）与返回改前字符串的求值闭包渲染结果相同。
#[test]
fn plain_text_matches_join() {
    let cases: &[&[&str]] = &[
        &[],
        &[""],
        &["a"],
        &["a", "b"],
        &["", "b"],
        &["a", ""],
        &["a b", "c d"],
    ];
    for parts in cases {
        for sep in ["", " ", "／", " · ", ":"] {
            let old = parts.join(sep);
            assert_eq!(&*VarValue::joined(*parts, sep), old, "{parts:?} {sep:?}");
            let tpl = format!("x{{(${{chaizi_all:{sep}}})}} ${{chaizi_all:{sep}}}");
            assert_eq!(
                render(&tpl, 0, items("chaizi_all", parts, " ")),
                render(&tpl, 0, |n, _| (n == "chaizi_all").then(|| old.clone())),
                "{tpl:?} {parts:?}"
            );
        }
    }
}

/// `${code_rev_all}`：`word_codes_display` 的 `/` 连接串换分隔符，与改前的 `replace` 逐字相同。
#[test]
fn code_rev_all_matches_pre_change_replace() {
    let old = |codes: &str, arg: Option<&str>| match arg {
        Some(sep) if !codes.is_empty() => codes.replace('/', sep),
        _ => codes.to_string(),
    };
    for codes in ["", "q", "q/trn/trnt"] {
        for arg in [None, Some(""), Some(" "), Some("·")] {
            assert_eq!(
                &*joined_codes(codes, arg),
                old(codes, arg),
                "{codes:?} {arg:?}"
            );
        }
    }
    // 缺省的 `/` 也是分隔符。
    assert_eq!(joined_codes("q/trn", None).seps, vec![(1, 2)]);
}

/// 分隔符的颜色（`None` = 无内联色），逐个分隔符片段。
fn sep_colors(t: &StyledText) -> Vec<(&str, Option<InlineColor>)> {
    t.spans()
        .iter()
        .filter(|s| s.separator)
        .map(|s| {
            (
                &t.as_str()[s.start as usize..s.end as usize],
                s.color.as_deref().cloned(),
            )
        })
        .collect()
}

/// `${name:$[颜色]{分隔符}}`：变量收到的参数是 `分隔符`，颜色只标在分隔符上；文字不带标记。
#[test]
fn inline_color_in_separator_arg() {
    let e = items("chaizi_all", &["亻尔", "女子"], " ");
    let t = render_styled("${chaizi_all:$[#888888]{／}}", 0, &e);
    assert_eq!(t.as_str(), "亻尔／女子");
    assert_eq!(
        sep_colors(&t),
        vec![("／", Some(InlineColor::parse("#888888")))]
    );
    assert!(
        t.spans()
            .iter()
            .filter(|s| !s.separator)
            .all(|s| s.color.is_none()),
        "分项不带颜色"
    );
    assert_eq!(render("${chaizi_all:$[#888888]{／}}", 0, &e), "亻尔／女子");
    // 分隔符带空格、颜色写色名，同模板内联色。
    let t = render_styled("${chaizi_all:$[text_dim]{ · }}", 0, &e);
    assert_eq!(t.as_str(), "亻尔 · 女子");
    assert_eq!(
        sep_colors(&t),
        vec![(" · ", Some(InlineColor::parse("text_dim")))]
    );
    // 空分隔符照样成立：没有分隔符片段。
    let t = render_styled("${chaizi_all:$[x]{}}", 0, &e);
    assert_eq!(t.as_str(), "亻尔女子");
    assert!(sep_colors(&t).is_empty());
}

/// 优先级：分隔符自己的内联色 > 外层 `$[…]{}`（分项仍取外层）；没写时分隔符随外层。
#[test]
fn separator_inline_color_beats_outer_inline() {
    let e = items("chaizi_all", &["a", "b"], " ");
    let t = render_styled("$[accent]{${chaizi_all:$[#888]{/}}}", 0, &e);
    let got: Vec<(bool, InlineColor)> = t
        .spans()
        .iter()
        .map(|s| (s.separator, s.color.as_deref().cloned().unwrap()))
        .collect();
    assert_eq!(
        got,
        vec![
            (false, InlineColor::parse("accent")),
            (true, InlineColor::parse("#888")),
            (false, InlineColor::parse("accent")),
        ]
    );
}

/// 内联色里的 `|`、`:` 是分隔符文字，不切回退链；回退链照常工作。
#[test]
fn separator_body_is_not_split_by_fallback_bar() {
    let e = |n: &str, a: Option<&str>| match n {
        "chaizi_all" => Some(VarValue::default()),
        "dict_all" => Some(VarValue::joined(["x", "y"], a.unwrap_or(DICT_ALL_SEP))),
        _ => None,
    };
    let tpl = "${chaizi_all:$[a]{|}|dict_all:$[b]{:|}}";
    let t = render_styled(tpl, 0, e);
    assert_eq!(t.as_str(), "x:|y");
    assert_eq!(sep_colors(&t), vec![(":|", Some(InlineColor::parse("b")))]);
    let e2 = items("chaizi_all", &["a", "b"], " ");
    assert_eq!(render("${chaizi_all:$[a]{|}|dict_all}", 0, &e2), "a|b");
}

/// 出错口径同模板内联色：语法不成立（`]` 后不是 `{`、`SPEC` 过长或含换行、只占参数一部分）
/// 参数按字面；`BODY` 未闭合即变量未闭合，其后全按字面；颜色写错仍是内联色（求色时回落正文色）。
#[test]
fn separator_inline_color_errors_degrade_like_inline_color() {
    let e = items("chaizi_all", &["a", "b"], " ");
    let plain = |tpl: &str| render(tpl, 0, &e);
    assert_eq!(plain("${chaizi_all:$[x]/}"), "a$[x]/b", "]后不是 {{");
    assert_eq!(
        plain("${chaizi_all:-$[x]{/}}"),
        "a-$[x]{/}b",
        "只占一部分：整个按字面"
    );
    assert!(
        sep_colors(&render_styled("${chaizi_all:-$[x]{/}}", 0, &e))[0]
            .1
            .is_none()
    );
    let long = "x".repeat(COLOR_SPEC_MAX + 1);
    assert_eq!(
        plain(&format!("${{chaizi_all:$[{long}]{{/}}}}")),
        format!("a$[{long}]{{/b}}"),
        "SPEC 过长：同改前，变量收在第一个 `}}`"
    );
    assert_eq!(
        plain("${chaizi_all} ${chaizi_all:$[x]{/ {${chaizi_all}"),
        "a b ${chaizi_all:$[x]{/ {${chaizi_all}",
        "BODY 未闭合 = 变量未闭合，其后全按字面"
    );
    // 颜色值非法：语法成立，分隔符带一个非法颜色（求色回落正文色，同 `$[红]{…}`）。
    let t = render_styled("${chaizi_all:$[红]{/}}", 0, &e);
    assert_eq!(t.as_str(), "a/b");
    assert_eq!(sep_colors(&t), vec![("/", Some(InlineColor::parse("红")))]);
    assert_eq!(
        InlineColor::parse("红").normal,
        wind_theme::ColorRef {
            light: wind_theme::Atom::Invalid,
            dark: wind_theme::Atom::Invalid,
        }
    );
}

/// 夹具拆字表：好 = 女子 [vbg]、人 = 人 [w]、丂 = （空） [gnv]（有编码无字根）。
fn chaizi() -> wind_reverse::ReverseLookup {
    let dir = std::env::temp_dir().join(format!("wind_sep_chaizi_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("chaizi.txt");
    std::fs::write(&p, "好\t女子\tvbg\n人\t人\tw\n丂\t\tgnv\n").unwrap();
    let rl = wind_reverse::ReverseLookup::load(None, Some(&p));
    let _ = std::fs::remove_dir_all(&dir);
    rl
}

/// `${chaizi_all}` / `${chaizi_code_all}` 两个入口（裸文本、候选）的纯文本与改前的
/// `radicals_of` / `codes_of` 逐字相同，分隔符带标记。
#[test]
fn chaizi_all_variables_match_pre_change() {
    let rl = chaizi();
    let co = Coordinator::new_headless(Config::default(), None);
    for text in ["好人", "好", "丂人", "好X人", "XY"] {
        let cand = Candidate {
            text: text.into(),
            ..Default::default()
        };
        for arg in [None, Some(""), Some("／"), Some(" · ")] {
            let sep = arg.unwrap_or(" ");
            let old = [
                ("chaizi_all", rl.radicals_of(text, sep)),
                ("chaizi_code_all", rl.codes_of(text, sep)),
            ];
            for (name, old) in old {
                let bare = reverse_text_var(name, arg, text, &rl).unwrap();
                assert_eq!(&*bare, old, "{name} {text:?} {arg:?}");
                let via = co.eval_var(name, arg, &cand, &rl, "").unwrap();
                assert_eq!(via, bare, "{name} {text:?} {arg:?}");
            }
        }
    }
    let tpl = Template::parse("${chaizi_all:／} ${chaizi_code_all:+}");
    let cand = Candidate {
        text: "好人".into(),
        ..Default::default()
    };
    let t = co.comment_for(&cand, &tpl, 0, &rl, "");
    assert_eq!(t.as_str(), "女子／人 vbg+w");
    let seps: Vec<(&str, Option<&str>)> = pieces(&t)
        .into_iter()
        .filter(|p| p.2)
        .map(|p| (p.0, p.1))
        .collect();
    assert_eq!(
        seps,
        vec![("／", Some("chaizi_all")), ("+", Some("chaizi_code_all"))]
    );
}
