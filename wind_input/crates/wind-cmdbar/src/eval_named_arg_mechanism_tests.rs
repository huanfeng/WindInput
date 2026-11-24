use super::*;
use crate::context::MemoryContext;
use crate::parser::parse;
use crate::registry::{Category, FuncSpec};

/// 探针把收到的实参**编码进返回值**，而不是写进共享状态再断言。
///
/// 这些测试会被 cargo 并行调度，用 `static` 收集会让它们互相覆盖、产生与改动
/// 无关的偶发失败（本仓已因共享测试状态栽过一次）。返回值是每次调用私有的，
/// 天然并行安全，断言的也正是被测函数真实看到的东西。
fn probe_plain(_c: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok("plain".into())
}

fn probe_named(_c: &dyn EvalContext, a: &[String], named: &[(String, String)]) -> Result<String> {
    let named = named
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!("pos[{}] named[{}]", a.join(","), named))
}

/// 注册两个探针：`probe` 收 alpha/beta 两个具名参数，`bare` 一个都不收。
fn probe_registry() -> Registry {
    let mut r = Registry::with_builtins();
    r.register(FuncSpec {
        name: "probe",
        category: Category::Meta,
        min_args: 0,
        max_args: -1,
        pure: true,
        deterministic: false,
        deprecated: false,
        alias_of: "",
        description: "test probe",
        example: "probe()",
        eval: probe_plain,
        named_params: &[("alpha", "甲"), ("beta", "乙")],
        eval_named: Some(probe_named),
    });
    r.register(FuncSpec {
        name: "bare",
        category: Category::Meta,
        min_args: 0,
        max_args: -1,
        pure: true,
        deterministic: false,
        deprecated: false,
        alias_of: "",
        description: "no named params",
        example: "bare()",
        eval: probe_plain,
        named_params: &[],
        eval_named: None,
    });
    r
}

/// 求值一个模板短语（`{expr}`），返回结果或错误文本。
fn run(src: &str) -> std::result::Result<String, String> {
    let reg = probe_registry();
    let ctx = MemoryContext::new().with_input("nihao");
    let p = parse(src).map_err(|e| e.to_string())?;
    evaluate(&p, &ctx, &reg)
        .map(|e| e.display)
        .map_err(|e| e.to_string())
}

/// 有具名参数 → 走 `eval_named`；没有 → 走普通 `eval`。函数据此区分两条入口。
#[test]
fn dispatches_to_named_entry_only_when_named_present() {
    assert_eq!(run("{probe()}").unwrap(), "plain");
    assert_eq!(run("{probe(alpha=\"1\")}").unwrap(), "pos[] named[alpha=1]");
}

/// 位置参数与具名参数分开送达：`args` 不含具名值，`named` 不含位置值。
/// 这是 `proc.run` 这类变参函数能安全加选项的前提。
#[test]
fn positional_and_named_are_delivered_separately() {
    assert_eq!(
        run("{probe(\"p1\", \"p2\", alpha=\"a\")}").unwrap(),
        "pos[p1,p2] named[alpha=a]"
    );
}

/// 多个具名参数按源顺序送达。
#[test]
fn multiple_named_args_keep_source_order() {
    assert_eq!(
        run("{probe(beta=\"B\", alpha=\"A\")}").unwrap(),
        "pos[] named[beta=B,alpha=A]"
    );
}

/// 具名参数的值是完整表达式：插值、嵌套调用都要在送达前求好值。
#[test]
fn named_value_is_evaluated_not_raw() {
    assert_eq!(
        run("{probe(alpha=\"{upper(code)}\")}").unwrap(),
        "pos[] named[alpha=NIHAO]"
    );
    assert_eq!(
        run("{probe(alpha=upper(code))}").unwrap(),
        "pos[] named[alpha=NIHAO]"
    );
}

/// 未登记的名字必须报错并列出支持的名字——静默忽略会让笔误表现为
/// 「写了不生效」，而这条路径没有任何用户可见反馈。
#[test]
fn unregistered_name_errors_and_lists_valid_ones() {
    let err = run("{probe(gamma=\"x\")}").unwrap_err();
    assert!(err.contains("gamma"), "{err}");
    assert!(err.contains("alpha") && err.contains("beta"), "{err}");
}

/// 没登记任何具名参数的函数收到 `k=v` 也要报错，而不是当没看见。
#[test]
fn function_without_named_params_rejects_them() {
    let err = run("{bare(alpha=\"x\")}").unwrap_err();
    assert!(err.contains("alpha"), "{err}");
}

/// arity 只数位置参数：具名参数不占位，否则给变参函数加选项会撞上参数个数上限。
#[test]
fn named_args_do_not_count_toward_arity() {
    let mut r = probe_registry();
    r.register(FuncSpec {
        name: "one_arg",
        category: Category::Meta,
        min_args: 1,
        max_args: 1,
        pure: true,
        deterministic: false,
        deprecated: false,
        alias_of: "",
        description: "exactly one positional",
        example: "one_arg(x)",
        eval: probe_plain,
        named_params: &[("alpha", "甲")],
        eval_named: Some(probe_named),
    });
    let ctx = MemoryContext::new();
    let p = parse("{one_arg(\"p\", alpha=\"a\")}").unwrap();
    assert!(
        evaluate(&p, &ctx, &r).is_ok(),
        "1 个位置参数 + 1 个具名参数不应超出 (1,1) 的 arity"
    );
}

/// display 的纯度检查要穿透具名参数的值，否则副作用可借 `k=v` 绕过守卫，
/// 在候选**显示**阶段就被执行。
#[test]
fn display_purity_check_covers_named_values() {
    let mut r = probe_registry();
    r.register(FuncSpec {
        name: "sideeffect",
        category: Category::Meta,
        min_args: 0,
        max_args: 0,
        pure: false,
        deterministic: false,
        deprecated: false,
        alias_of: "",
        description: "impure",
        example: "sideeffect()",
        eval: probe_plain,
        named_params: &[],
        eval_named: None,
    });
    let ctx = MemoryContext::new();
    let p = parse(r#"$CC(probe(alpha=sideeffect()))"#).unwrap();
    assert!(
        matches!(evaluate(&p, &ctx, &r), Err(CmdbarError::NotPure { .. })),
        "具名参数里的副作用函数必须被 display 纯度守卫拦下"
    );
}
