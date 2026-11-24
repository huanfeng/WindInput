use super::*;
use crate::context::MemoryContext;
use crate::parser::parse;

fn eval_str(src: &str) -> String {
    let reg = Registry::with_builtins();
    let ctx = MemoryContext::new().with_input("nihao");
    let p = parse(src).unwrap();
    evaluate(&p, &ctx, &reg).unwrap().display
}

#[test]
fn literal_and_template() {
    assert_eq!(eval_str("hello"), "hello");
    assert_eq!(eval_str("code={code}"), "code=nihao");
    assert_eq!(eval_str("len={len(code)}"), "len=5");
}

#[test]
fn nested_funcs() {
    assert_eq!(eval_str("{upper(sub(code, 1, 2))}"), "NI");
    assert_eq!(eval_str("{concat(code, \"!\")}"), "nihao!");
}

#[test]
fn command_display_must_be_pure() {
    let reg = Registry::full();
    let ctx = MemoryContext::new();
    // open 是副作用函数，不能出现在 display
    let p = parse(r#"$CC(open("u"))"#).unwrap();
    assert!(matches!(
        evaluate(&p, &ctx, &reg),
        Err(CmdbarError::NotPure { .. })
    ));
}

#[test]
fn command_type_action_is_text() {
    let reg = Registry::full();
    let ctx = MemoryContext::new();
    let p = parse(r#"$CC("《》", type("《》"))"#).unwrap();
    let ev = evaluate(&p, &ctx, &reg).unwrap();
    assert_eq!(ev.display, "《》");
    assert_eq!(ev.actions.len(), 1);
    assert_eq!(ev.actions[0].kind, crate::action::ActionKind::Text);
    assert_eq!(ev.actions[0].run(&ctx, &reg).unwrap(), "《》");
}

/// 具名参数的名字必须登记；写错的名字要报错，不能像"没写"一样被吞掉。
#[test]
fn unknown_named_arg_is_rejected() {
    let reg = Registry::full();
    let ctx = MemoryContext::new();
    // proc.run 登记了 cwd，但没有 dir
    let p = parse(r#"$CC("x", proc.run("a.exe", dir="D:/d"))"#).unwrap();
    let err = p_first_action_err(&p, &ctx, &reg);
    assert!(err.contains("dir"), "{err}");
    assert!(err.contains("cwd"), "报错要提示支持的名字: {err}");
}

/// 完全没登记具名参数的函数，收到任何 `k=v` 都应报错。
#[test]
fn named_arg_on_plain_func_is_rejected() {
    let reg = Registry::full();
    let ctx = MemoryContext::new();
    let p = parse(r#"$CC("x", open("https://a", cwd="D:/d"))"#).unwrap();
    let err = p_first_action_err(&p, &ctx, &reg);
    assert!(err.contains("cwd"), "{err}");
}

fn p_first_action_err(p: &Phrase, ctx: &MemoryContext, reg: &Registry) -> String {
    let ev = evaluate(p, ctx, reg).unwrap();
    ev.actions[0]
        .run(ctx, reg)
        .expect_err("expected named-arg rejection")
        .to_string()
}

#[test]
fn array_expansion() {
    let reg = Registry::full();
    let ctx = MemoryContext::new();
    let p = parse(r#"$SS("组", "字面", $CC("动作", type("x")))"#).unwrap();
    let arr = match p {
        Phrase::Array(a) => expand_array(&a, &ctx, &reg).unwrap(),
        _ => panic!(),
    };
    assert_eq!(arr.name, "组");
    assert_eq!(arr.elements.len(), 2);
    assert_eq!(arr.elements[0].display, "字面");
    assert!(arr.elements[0].actions.is_empty());
    assert_eq!(arr.elements[1].display, "动作");
    assert_eq!(arr.elements[1].actions.len(), 1);
}

/// 元素级命令源必须能独立重新解析、求值出同样的显示与动作个数——宿主靠它执行单个成员。
/// 覆盖转义敏感的写法：路径反斜杠、字符串内插花括号、引号、具名参数、`on_error` 修饰符。
#[test]
fn embedded_command_src_round_trips() {
    let reg = Registry::full();
    let ctx = MemoryContext::new().with_input("abc");
    let srcs = [
        r#"$CC("打开", open("D:\\notes\\temp"))"#,
        r#"$CC("长度 {len(code)}", type("{upper(code)}"))"#,
        r#"$CC("引号\"内", type("a\"b"))"#,
        r#"$CC("链", type("x"), type("y"), {on_error: "stop"})"#,
        r#"$CC("具名", open("https://a", cwd="D:/d"))"#,
    ];
    for one in srcs {
        let p = parse(&format!(r#"$SS("组", {one})"#)).unwrap();
        let Phrase::Array(a) = p else { panic!() };
        let arr = expand_array(&a, &ctx, &reg).unwrap();
        let src = arr.elements[0].src.clone().expect("命令元素必须带源");
        let direct = match parse(one).unwrap() {
            p @ Phrase::Command(_) => evaluate(&p, &ctx, &reg).unwrap(),
            _ => panic!(),
        };
        let again = match parse(&src).unwrap() {
            p @ Phrase::Command(_) => evaluate(&p, &ctx, &reg).unwrap(),
            _ => panic!("元素源必须解析回命令短语: {src}"),
        };
        assert_eq!(again.display, direct.display, "{src}");
        assert_eq!(again.actions.len(), direct.actions.len(), "{src}");
        assert_eq!(again.on_error, direct.on_error, "{src}");
    }
}

#[test]
fn literal_elements_have_no_command_src() {
    let reg = Registry::full();
    let ctx = MemoryContext::new();
    let Phrase::Array(a) = parse(r#"$SS("组", "字面", $CC("动作", type("x")))"#).unwrap()
    else {
        panic!()
    };
    let arr = expand_array(&a, &ctx, &reg).unwrap();
    assert!(arr.elements[0].src.is_none());
    assert!(arr.elements[1].src.is_some());
}

#[test]
fn aa_marker_expands_to_char_candidates() {
    // $AA 字符组：每个 rune 成为一个无动作的上屏文本候选。
    let reg = Registry::full();
    let ctx = MemoryContext::new();
    let p = parse(r#"$AA("数字", "①②③")"#).unwrap();
    let arr = match p {
        Phrase::Array(a) => expand_array(&a, &ctx, &reg).unwrap(),
        _ => panic!(),
    };
    assert_eq!(arr.name, "数字");
    assert_eq!(arr.elements.len(), 3);
    let displays: Vec<&str> = arr.elements.iter().map(|e| e.display.as_str()).collect();
    assert_eq!(displays, ["①", "②", "③"]);
    assert!(arr.elements.iter().all(|e| e.actions.is_empty()));
}
