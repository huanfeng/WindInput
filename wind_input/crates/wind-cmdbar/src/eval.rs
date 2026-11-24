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
    /// 嵌入 `$CC` 元素的**独立命令源**（`$CC(显示名, 动作...)` 完整文本，可单独交给
    /// [`evaluate_phrase`](crate::evaluate_phrase) 重新求值执行）；字面字符串元素为 `None`。
    ///
    /// 宿主把每个成员做成独立候选，选中时按这份源执行——整条 `$SS` 的源不能用，
    /// 那会是一组而不是这一个成员。由 AST 的 `Display` 渲染，往返等价由测试钉住。
    pub src: Option<String>,
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
                    src: None,
                });
            }
            Expr::Command(cp) => {
                let ev = eval_command(cp, ctx, reg).map_err(|e| wrap_elem(i, e))?;
                out.push(ArrayElement {
                    display: ev.display,
                    actions: ev.actions,
                    modifiers: cp.modifiers.clone(),
                    src: Some(cp.to_string()),
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
#[path = "eval_named_arg_mechanism_tests.rs"]
mod named_arg_mechanism_tests;

#[cfg(test)]
#[path = "eval_tests.rs"]
mod tests;
