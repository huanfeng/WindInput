use super::*;

#[test]
fn contains_marker_call_covers_every_marker() {
    for m in MARKER_TABLE {
        assert!(contains_marker_call(&format!("x{m}(1)")), "{m}");
        assert!(!contains_marker_call(m), "{m} 无括号不算调用");
    }
    assert!(!contains_marker_call("{abc}"), "顶层 {{ 不算 marker");
}

fn pp(src: &str) -> Phrase {
    parse(src).unwrap_or_else(|e| panic!("parse {src:?} failed: {e}"))
}

#[test]
fn literal_phrase() {
    assert_eq!(pp("hello"), Phrase::Literal("hello".into()));
}

#[test]
fn template_phrase_roundtrip() {
    let p = pp("你好 {code} 长度 {len(code)}");
    match &p {
        Phrase::Template(Expr::StringLit(parts)) => {
            assert_eq!(parts.len(), 4);
            assert!(matches!(&parts[0], StringPart::Text(t) if t == "你好 "));
            assert!(matches!(&parts[1], StringPart::Interp(_)));
        }
        _ => panic!("expected template, got {p:?}"),
    }
}

/// `${VAR}` 是旧式简单模板语法，不属于命令栏语法——宿主据 `is_cmdbar_grammar` 分流，
/// 误判会让 `${YC}年${MC}月${DC}日` 走 evaluate、因 `YC` 非注册函数而被整条丢弃。
#[test]
fn dollar_brace_is_not_cmdbar_grammar() {
    for src in [
        "${YC}年${MC}月${DC}日",
        "${YC}年${MC}月${DC}日 $HH:$mm:$ss",
        "${Y}-${MM}-${DD}",
    ] {
        assert!(!is_cmdbar_grammar(src), "{src} 不应判为命令栏语法");
        assert!(
            matches!(parse(src), Ok(Phrase::Literal(t)) if t == src),
            "{src} 应解析为 Literal 原文"
        );
    }
}

/// 豁免只针对 `${`，真正的 `{expr}` 插值与 marker 一律照旧。
#[test]
fn brace_interpolation_still_detected() {
    for src in [
        "年份{date(\"YYYY\")}",
        "${YC}年，剪贴板：{clip()}", // 混合：`${}` 被跳过，裸 `{` 仍命中
        "$$-{code()}",               // `$$` 是转义的字面 $，其后的 `{` 是真插值
    ] {
        assert!(is_cmdbar_grammar(src), "{src} 应判为命令栏语法");
    }
    assert!(is_cmdbar_grammar(r#"$CC("x", type("x"))"#));
}

#[test]
fn command_phrase_basic() {
    let p = pp(r#"$CC("《》", type("《》"), key.tap("Left"))"#);
    match p {
        Phrase::Command(c) => {
            assert_eq!(c.actions.len(), 2);
            assert!(c.modifiers.is_empty());
        }
        _ => panic!("expected command"),
    }
}

/// 取 `$CC` 第一个动作的调用节点。
fn first_call(src: &str) -> (String, Vec<Expr>, Vec<(String, Expr)>) {
    match pp(src) {
        Phrase::Command(c) => match c.actions[0].clone() {
            Expr::Call { name, args, named } => (name, args, named),
            other => panic!("expected call, got {other:?}"),
        },
        _ => panic!("expected command"),
    }
}

#[test]
fn named_args_parse_after_positional() {
    let (name, args, named) =
        first_call(r#"$CC("查词", proc.run("D:/Dict/d.exe", "x", cwd="D:/Dict"))"#);
    assert_eq!(name, "proc.run");
    assert_eq!(args.len(), 2);
    assert_eq!(named.len(), 1);
    assert_eq!(named[0].0, "cwd");
    assert_eq!(
        named[0].1,
        Expr::StringLit(vec![StringPart::Text("D:/Dict".into())])
    );
}

#[test]
fn named_arg_value_may_interpolate() {
    // 值是完整表达式，插值/嵌套调用都成立（cwd 取自当前输入等场景）。
    let (_, _, named) = first_call(r#"$CC("x", proc.run("a.exe", cwd="{last(1)}"))"#);
    assert!(matches!(&named[0].1, Expr::StringLit(parts)
        if matches!(parts[0], StringPart::Interp(_))));
}

#[test]
fn named_args_reject_bad_forms() {
    // 位置参数不能跟在具名之后
    assert!(parse(r#"$CC("x", proc.run("a.exe", cwd="d", "p"))"#).is_err());
    // 重复 key
    assert!(parse(r#"$CC("x", proc.run("a.exe", cwd="d", cwd="e"))"#).is_err());
    // 缺值
    assert!(parse(r#"$CC("x", proc.run("a.exe", cwd=))"#).is_err());
    // key 必须是裸标识符
    assert!(parse(r#"$CC("x", proc.run("a.exe", "cwd"="d"))"#).is_err());
    // marker 层不收具名参数（那里的选项是末参 options bag）
    assert!(parse(r#"$CC("x", type("y"), prefix=true)"#).is_err());
}

#[test]
fn named_args_round_trip_through_display() {
    // Display 用于调试回显与设置页展示，具名参数必须能原样写回。
    let src = r#"$CC("x", proc.run("a.exe", "p", cwd="D:/d"))"#;
    assert_eq!(pp(src).to_string(), src);
    // 无位置参数时不应留下多余逗号
    let only_named = r#"$CC("x", proc.run(cwd="D:/d"))"#;
    assert_eq!(pp(only_named).to_string(), only_named);
}

#[test]
fn cc1_injects_prefix_modifier() {
    let p = pp(r#"$CC1("x", type("x"))"#);
    match p {
        Phrase::Command(c) => assert_eq!(c.modifiers.get_bool("prefix"), Some(true)),
        _ => panic!(),
    }
}

#[test]
fn explicit_options_override_default() {
    let p = pp(r#"$CC1("x", type("x"), {prefix: false, async: true})"#);
    match p {
        Phrase::Command(c) => {
            assert_eq!(c.modifiers.get_bool("prefix"), Some(false));
            assert_eq!(c.modifiers.get_bool("async"), Some(true));
        }
        _ => panic!(),
    }
}

#[test]
fn options_bag_must_be_last() {
    assert!(parse(r#"$CC("x", {a: 1}, type("y"))"#).is_err());
}

#[test]
fn array_phrase_with_embedded_cc() {
    let p = pp(r#"$SS("操作", "纯文本", $CC("动作", open("https://x")))"#);
    match p {
        Phrase::Array(a) => {
            assert_eq!(a.name, "操作");
            assert_eq!(a.elements.len(), 2);
            assert!(matches!(&a.elements[0], Expr::StringLit(_)));
            assert!(matches!(&a.elements[1], Expr::Command(_)));
            // $SS 默认注入 prefix/expand/nav
            assert_eq!(a.modifiers.get_bool("prefix"), Some(true));
        }
        _ => panic!("expected array"),
    }
}

#[test]
fn array_nested_cc_with_prefix_rejected() {
    assert!(parse(r#"$SS("g", $CC1("x", type("x")))"#).is_err());
}

#[test]
fn aa_marker_explodes_into_per_rune_elements() {
    // $AA 是 $SS 的字符组简写：单字符串参数按 rune 炸开为逐字符元素。
    let p = pp(r#"$AA("标点", "、。·")"#);
    match p {
        Phrase::Array(a) => {
            assert_eq!(a.name, "标点");
            assert_eq!(a.elements.len(), 3);
            assert_eq!(
                a.elements[0],
                Expr::StringLit(vec![StringPart::Text("、".into())])
            );
            assert_eq!(
                a.elements[1],
                Expr::StringLit(vec![StringPart::Text("。".into())])
            );
            assert_eq!(
                a.elements[2],
                Expr::StringLit(vec![StringPart::Text("·".into())])
            );
            // 与 $SS 共享默认修饰符。
            assert_eq!(a.modifiers.get_bool("prefix"), Some(true));
            assert_eq!(a.modifiers.get_bool("nav"), Some(true));
        }
        _ => panic!("expected array"),
    }
}

#[test]
fn aa_marker_multibyte_runes_counted_correctly() {
    // 组合符号/圈号等多字节 rune 必须按 rune（非字节）切分。
    let p = pp(r#"$AA("圆圈", "①②③")"#);
    match p {
        Phrase::Array(a) => assert_eq!(a.elements.len(), 3),
        _ => panic!("expected array"),
    }
}

#[test]
fn aa_marker_arity_and_literal_rules() {
    // 缺 chars 参数（只有组名）→ err。
    assert!(parse(r#"$AA("名")"#).is_err());
    // 多于一个 chars 参数 → err（应合并为单串）。
    assert!(parse(r#"$AA("名", "ab", "cd")"#).is_err());
    // chars 含插值 → err（必须纯字面）。
    assert!(parse(r#"$AA("名", "x{date()}")"#).is_err());
    // 空 chars → err。
    assert!(parse(r#"$AA("名", "")"#).is_err());
}

#[test]
fn namespaced_bare_ident_requires_parens() {
    assert!(parse("{clip.copy}").is_err()); // template 内裸 namespaced
}

#[test]
fn text_before_marker_rejected() {
    assert!(parse(r#"abc$CC("x")"#).is_err());
}

#[test]
fn backslash_before_cjk_does_not_panic() {
    // 回归：反斜杠紧贴中文（Windows 中文路径 `E:\我的文档\`）曾致字节边界 panic
    // （lexer `pos += 2` 落进多字节字符中间）。现按完整字符推进。
    let p = pp(r#"$CC("打开", open("E:\我的文档\x"))"#);
    match p {
        Phrase::Command(c) => match &c.actions[0] {
            Expr::Call { name, args, .. } => {
                assert_eq!(name, "open");
                // 未知转义 `\我` / `\x` 原样保留（含反斜杠）。
                assert_eq!(
                    args[0],
                    Expr::StringLit(vec![StringPart::Text(r"E:\我的文档\x".into())])
                );
            }
            other => panic!("expected open() call, got {other:?}"),
        },
        _ => panic!("expected command"),
    }
}

#[test]
fn escaped_backslash_before_cjk() {
    // `\\` 转义为单反斜杠，随后中文完整保留（不再吞字节 / 乱码）。
    let p = pp(r#"$CC("打开", open("E:\\我的文档"))"#);
    match p {
        Phrase::Command(c) => match &c.actions[0] {
            Expr::Call { args, .. } => assert_eq!(
                args[0],
                Expr::StringLit(vec![StringPart::Text(r"E:\我的文档".into())])
            ),
            other => panic!("expected call, got {other:?}"),
        },
        _ => panic!("expected command"),
    }
}

#[test]
fn cjk_in_ss_array_element_does_not_panic() {
    // $SS 数组走 split_array_args + parse_array_element 的独立字节扫描路径，
    // 同样修复反斜杠贴中文。
    let p = pp(r#"$SS("组", $CC("开", open("D:\目录")))"#);
    match p {
        Phrase::Array(a) => assert_eq!(a.elements.len(), 1),
        _ => panic!("expected array"),
    }
}

/// 期望的 APP_DIR 展开值，独立于被测代码另算一遍（不拿 `wind_config::dir_var_str`
/// 的返回值当期望值，否则只是自证「函数等于它自己」）。
fn want_app_dir() -> String {
    let d = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // 空串会让下面基于 `contains` 的断言恒真（假绿）。
    assert!(!d.is_empty(), "APP_DIR 期望值不该为空");
    d
}

/// 取 `$CC` 第一个动作调用的首个实参。
fn first_action_arg(p: &Phrase) -> &Expr {
    match p {
        Phrase::Command(c) => match &c.actions[0] {
            Expr::Call { args, .. } => &args[0],
            other => panic!("expected call, got {other:?}"),
        },
        other => panic!("expected command, got {other:?}"),
    }
}

#[test]
fn dir_var_expands_in_action_string_arg() {
    // `${APP_DIR}` 在词法期展开成绝对目录字面量，AST 里不得留下 Interp，
    // 否则求值 UnknownFunc，选中候选没反应。
    let p = pp(r#"$CC("[打开安装目录]", open("${APP_DIR}"))"#);
    assert_eq!(
        *first_action_arg(&p),
        Expr::StringLit(vec![StringPart::Text(want_app_dir())])
    );
}

#[test]
fn dir_var_expands_in_type_action() {
    // `type` 由 eval 特例拦截为文本上屏，参数同样是字符串字面量，走同一条词法路径。
    let p = pp(r#"$CC("[输出安装目录]", type("${APP_DIR}"))"#);
    assert_eq!(
        *first_action_arg(&p),
        Expr::StringLit(vec![StringPart::Text(want_app_dir())])
    );
}

#[test]
fn dir_var_concatenates_with_surrounding_literals() {
    // 变量与前后字面量拼成一个 Text 片段（不被切碎成多段）。
    let p = pp(r#"$CC("[日志]", open("前${APP_DIR}\\logs"))"#);
    assert_eq!(
        *first_action_arg(&p),
        Expr::StringLit(vec![StringPart::Text(format!(
            r"前{}\logs",
            want_app_dir()
        ))])
    );
}

#[test]
fn unknown_dollar_brace_stays_literal_and_does_not_drop_phrase() {
    // `${YC}` 是旧式模板变量（归 wind_phrase::expand_template 管），命令栏不认识它。
    // 不能把 `{YC}` 当插值（UnknownFunc 会丢掉整条候选），未知变量原样留字面。
    let p = pp(r#"$CC("[日期]", type("${YC}年"))"#);
    assert_eq!(
        *first_action_arg(&p),
        Expr::StringLit(vec![StringPart::Text("${YC}年".into())])
    );
}

#[test]
fn real_interpolation_still_works_without_dollar() {
    // 不带 `$` 的 `{expr}` 仍是插值。
    let p = pp(r#"$CC("[粘贴]", type("{last()}"))"#);
    match first_action_arg(&p) {
        Expr::StringLit(parts) => assert!(
            matches!(parts.as_slice(), [StringPart::Interp(_)]),
            "应为插值，实际 {parts:?}"
        ),
        other => panic!("expected string lit, got {other:?}"),
    }
}

#[test]
fn escaped_dollar_restores_interpolation_after_it() {
    // `${` 现在是变量语法，想写「字面 $ 紧跟一个真插值」就得转义：`\${expr}`。
    // 这是 `\$` 进转义白名单的唯一理由，丢了它这种写法就没有出路。
    let p = pp(r#"$CC("[价格]", type("\${last()}"))"#);
    match first_action_arg(&p) {
        Expr::StringLit(parts) => assert!(
            matches!(
                parts.as_slice(),
                [StringPart::Text(t), StringPart::Interp(_)] if t == "$"
            ),
            "应为字面 $ + 插值，实际 {parts:?}"
        ),
        other => panic!("expected string lit, got {other:?}"),
    }
}

#[test]
fn dir_var_expands_in_template_phrase_path() {
    // 模板短语（无 marker、含真插值）走 scan_template_parts 那条独立扫描路径。
    // 两条路径对 `${` 的处置必须一致 —— 只改一条正是本功能最初失效的成因。
    let p = pp(r#"{last()} ${APP_DIR}"#);
    match p {
        Phrase::Template(Expr::StringLit(parts)) => {
            let tail = parts
                .iter()
                .filter_map(|p| match p {
                    StringPart::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect::<String>();
            assert!(
                tail.contains(&want_app_dir()),
                "模板路径未展开 ${{APP_DIR}}，实际片段: {parts:?}"
            );
        }
        other => panic!("expected template, got {other:?}"),
    }
}

#[test]
fn unclosed_dollar_brace_falls_back_to_interpolation_error() {
    // `${` 未闭合：不当变量处理，退回原有插值路径并报未闭合错（而非静默吞掉）。
    assert!(parse(r#"$CC("x", type("${APP_DIR"))"#).is_err());
}

#[test]
fn display_roundtrips_via_display_impl() {
    let p = pp(r#"$CC("hi", open("u"))"#);
    let s = format!("{p}");
    let reparsed = pp(&s);
    assert_eq!(p, reparsed);
}
