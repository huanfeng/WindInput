//! AST 求值器
//!
//! 对照 Go `wind_input/internal/cmdbar/eval/eval.go`。把短语对 [`EvalContext`] 求值，
//! 产出 display 文本 + 动作链。`type(arg)` 动作被特例为 [`crate::action::ActionKind::Text`]
//! （宿主走 InsertText 上屏），其余为 [`crate::action::ActionKind::Effect`]。

use crate::action::ResolvedAction;
use crate::ast::{
    ArrayPhrase, CommandPhrase, Expr, ModValue, Modifiers, OnError, Phrase, StringPart, fmt_number,
};
use crate::context::EvalContext;
use crate::error::{CmdbarError, Result};
use crate::registry::Registry;

/// 一条短语求值后的产物。
#[derive(Debug, Clone)]
pub struct Evaluated {
    pub display: String,
    pub actions: Vec<ResolvedAction>,
    /// 动作链失败后的策略（`{on_error: …}`）。非 command 短语没有动作，恒为默认值。
    pub on_error: OnError,
}

/// `$SS` 展开后的一个元素候选。
#[derive(Debug, Clone)]
pub struct ArrayElement {
    pub display: String,
    pub actions: Vec<ResolvedAction>,
    /// 嵌入 `$CC` 的元素级修饰符（组级 prefix 已在解析期禁用）。
    pub modifiers: Modifiers,
}

/// `$SS` 整体展开结果。
#[derive(Debug, Clone)]
pub struct ArrayExpansion {
    pub name: String,
    pub elements: Vec<ArrayElement>,
    pub modifiers: Modifiers,
}

/// 对短语求值。`Array` 须经 [`expand_array`]，此处返回错误。
pub fn evaluate(phrase: &Phrase, ctx: &dyn EvalContext, reg: &Registry) -> Result<Evaluated> {
    match phrase {
        Phrase::Literal(t) => Ok(Evaluated {
            display: t.clone(),
            actions: Vec::new(),
            on_error: OnError::default(),
        }),
        Phrase::Template(expr) => Ok(Evaluated {
            display: eval_expr(expr, ctx, reg)?,
            actions: Vec::new(),
            on_error: OnError::default(),
        }),
        Phrase::Array(_) => Err(CmdbarError::runtime(
            "eval",
            "ArrayPhrase must be expanded via expand_array, not evaluate",
        )),
        Phrase::Command(cp) => eval_command(cp, ctx, reg),
    }
}

fn eval_command(cp: &CommandPhrase, ctx: &dyn EvalContext, reg: &Registry) -> Result<Evaluated> {
    assert_pure_display(&cp.display, reg)?;
    let display = eval_expr(&cp.display, ctx, reg)?;
    let mut actions = Vec::with_capacity(cp.actions.len());
    for act in &cp.actions {
        // type(arg)：拦截为文本上屏（不经 registry 查找）。名单见 `action::EVAL_INTERCEPTED`。
        if let Expr::Call { name, args, named } = act
            && crate::action::is_eval_intercepted(name)
        {
            if !named.is_empty() {
                return Err(CmdbarError::runtime(
                    "type",
                    "type() takes no named arguments",
                ));
            }
            if args.len() != 1 {
                return Err(CmdbarError::runtime(
                    "type",
                    format!("expected 1 arg, got {}", args.len()),
                ));
            }
            actions.push(ResolvedAction::text(args[0].clone()));
            continue;
        }
        actions.push(ResolvedAction::effect(act.clone()));
    }
    Ok(Evaluated {
        display,
        actions,
        on_error: on_error_of(&cp.modifiers)?,
    })
}

/// 读 `{on_error: …}`。
///
/// **值写错必须报错**：静默落回默认，用户会看到「on_error 配了没反应」——而这个
/// 修饰符恰恰是用来防"假成功"的，失灵时的表现就是它要防的那件事。
/// ⚠️ 键名写错（`on_errro`）仍是静默忽略：修饰符袋一贯如此（`prefix`/`async`/`nav`
/// 都没有键名白名单），此处不单独收紧，否则同一个袋里两套规矩更难解释。
fn on_error_of(m: &Modifiers) -> Result<OnError> {
    match m.get("on_error").map(ModValue::as_str_value).as_deref() {
        None | Some("continue") => Ok(OnError::Continue),
        Some("stop") => Ok(OnError::Stop),
        Some(other) => Err(CmdbarError::runtime(
            "$CC",
            format!("on_error 不支持 {other:?}（支持: continue / stop）"),
        )),
    }
}

/// 展开 `$SS`：字面元素→上屏文本候选，嵌入 `$CC`→动作候选。
pub fn expand_array(
    phrase: &ArrayPhrase,
    ctx: &dyn EvalContext,
    reg: &Registry,
) -> Result<ArrayExpansion> {
    let mut out = Vec::with_capacity(phrase.elements.len());
    for (i, elem) in phrase.elements.iter().enumerate() {
        match elem {
            Expr::StringLit(parts) => {
                let display = eval_string_lit(parts, ctx, reg).map_err(|e| wrap_elem(i, e))?;
                out.push(ArrayElement {
                    display,
                    actions: Vec::new(),
                    modifiers: Modifiers::new(),
                });
            }
            Expr::Command(cp) => {
                let ev = eval_command(cp, ctx, reg).map_err(|e| wrap_elem(i, e))?;
                out.push(ArrayElement {
                    display: ev.display,
                    actions: ev.actions,
                    modifiers: cp.modifiers.clone(),
                });
            }
            other => {
                return Err(CmdbarError::runtime(
                    "expand_array",
                    format!("element {} unsupported expr {other:?}", i + 1),
                ));
            }
        }
    }
    Ok(ArrayExpansion {
        name: phrase.name.clone(),
        elements: out,
        modifiers: phrase.modifiers.clone(),
    })
}

fn wrap_elem(i: usize, e: CmdbarError) -> CmdbarError {
    CmdbarError::runtime("$SS element", format!("#{}: {e}", i + 1))
}

/// 检查 display 表达式只引用纯函数（否则副作用会在候选显示阶段触发）。
fn assert_pure_display(expr: &Expr, reg: &Registry) -> Result<()> {
    match expr {
        Expr::StringLit(parts) => {
            for p in parts {
                if let StringPart::Interp(e) = p {
                    assert_pure_display(e, reg)?;
                }
            }
            Ok(())
        }
        Expr::Number { .. } => Ok(()),
        Expr::Ident(name) => check_pure(name, reg),
        Expr::Call { name, args, named } => {
            check_pure(name, reg)?;
            for a in args {
                assert_pure_display(a, reg)?;
            }
            // 具名参数的值同样是表达式，漏查会给副作用留一条绕过 display 纯度检查的路。
            for (_, v) in named {
                assert_pure_display(v, reg)?;
            }
            Ok(())
        }
        other => Err(CmdbarError::runtime(
            "display",
            format!("unsupported expression {other:?}"),
        )),
    }
}

fn check_pure(name: &str, reg: &Registry) -> Result<()> {
    let spec = reg
        .lookup(name)
        .ok_or_else(|| CmdbarError::UnknownFunc { name: name.into() })?;
    if !spec.pure {
        return Err(CmdbarError::NotPure { name: name.into() });
    }
    Ok(())
}

/// 把表达式归约为字符串值。
pub(crate) fn eval_expr(expr: &Expr, ctx: &dyn EvalContext, reg: &Registry) -> Result<String> {
    match expr {
        Expr::StringLit(parts) => eval_string_lit(parts, ctx, reg),
        Expr::Number { value, raw } => {
            if raw.is_empty() {
                Ok(fmt_number(*value))
            } else {
                Ok(raw.clone())
            }
        }
        Expr::Ident(name) => call_func(name, &[], &[], ctx, reg),
        Expr::Call { name, args, named } => {
            let mut argv = Vec::with_capacity(args.len());
            for a in args {
                argv.push(eval_expr(a, ctx, reg)?);
            }
            let mut namedv = Vec::with_capacity(named.len());
            for (k, v) in named {
                namedv.push((k.clone(), eval_expr(v, ctx, reg)?));
            }
            call_func(name, &argv, &namedv, ctx, reg)
        }
        other => Err(CmdbarError::runtime(
            "eval",
            format!("unsupported expression {other:?}"),
        )),
    }
}

fn call_func(
    name: &str,
    args: &[String],
    named: &[(String, String)],
    ctx: &dyn EvalContext,
    reg: &Registry,
) -> Result<String> {
    let spec = reg
        .lookup(name)
        .ok_or_else(|| CmdbarError::UnknownFunc { name: name.into() })?;
    if !spec.accepts(args.len()) {
        return Err(CmdbarError::Arity {
            name: name.into(),
            got: args.len(),
            min: spec.min_args,
            max: spec.max_args,
        });
    }
    if named.is_empty() {
        return (spec.eval)(ctx, args);
    }
    // 未登记的参数名一律报错：静默忽略会让 `cmd="D:/d"` 这类笔误表现为
    // 「写了却没生效」，而这条路径没有任何用户可见反馈。
    for (k, _) in named {
        if !spec.accepts_named(k) {
            return Err(CmdbarError::runtime(
                name,
                if spec.named_params.is_empty() {
                    format!("函数不接受具名参数，收到 {k:?}")
                } else {
                    format!("未知具名参数 {k:?}（支持: {}）", spec.named_help())
                },
            ));
        }
    }
    match spec.eval_named {
        Some(f) => f(ctx, args, named),
        // 白名单非空却没有 named 入口 = 声明与实现脱节，registry 自检测试会拦下。
        None => Err(CmdbarError::runtime(
            name,
            "函数登记了具名参数但未提供求值入口",
        )),
    }
}

fn eval_string_lit(parts: &[StringPart], ctx: &dyn EvalContext, reg: &Registry) -> Result<String> {
    let mut out = String::new();
    for p in parts {
        match p {
            StringPart::Text(t) => out.push_str(t),
            StringPart::Interp(e) => out.push_str(&eval_expr(e, ctx, reg)?),
        }
    }
    Ok(out)
}

/// 具名参数**机制**本身的测试，与任何具体函数解耦。
///
/// 这里用一个只存在于测试的假函数注册进临时 Registry，钉住框架契约：谁能带具名
/// 参数、名字怎么校验、值怎么求值、传到函数手里是什么形状。以后接入 `key.tap`
/// 等其它命令时不必重写这套用例——真正会退化的是这些框架规则，而不是某个函数
/// 自己的业务逻辑。用 proc.run 来测框架则会把服务 mock 的噪音混进来。
#[cfg(test)]
mod named_arg_mechanism_tests {
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

    fn probe_named(
        _c: &dyn EvalContext,
        a: &[String],
        named: &[(String, String)],
    ) -> Result<String> {
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
}

#[cfg(test)]
mod tests {
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
}
