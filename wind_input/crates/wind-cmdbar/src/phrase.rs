//! 短语级高层 API（宿主集成入口）。
//!
//! 把「解析 → 求值 / `$SS` 展开」打包成一次调用，并提供 [`is_cmdbar_grammar`] 供宿主
//! 与旧的简单模板路径分流（对应 Go coordinator 的 phrase hook + 双路径策略 design §7.2）。
//!
//! 线程/求值：display 侧只用纯函数（[`Registry::with_builtins`]）即可；命令动作需要
//! 宿主注入 [`Services`](crate::services::Services) 后用 [`Registry::full`]。

use crate::ast::{Expr, OnError, Phrase, StringPart};
use crate::context::EvalContext;
use crate::error::Result;
use crate::eval::{ArrayExpansion, evaluate, expand_array};
use crate::parser::{self, parse};
use crate::registry::Registry;
use crate::{ActionKind, ResolvedAction};

pub use parser::is_cmdbar_grammar;

/// 一条短语求值后的形态：单候选或 `$SS` 多候选。
#[derive(Debug, Clone)]
pub enum PhraseEval {
    /// literal / template / command：单个 display + 动作链（command 才有动作）。
    Single {
        display: String,
        actions: Vec<ResolvedAction>,
        /// 动作链失败后的策略（`{on_error: …}`）；非 command 短语恒为默认值。
        on_error: OnError,
    },
    /// `$SS` 数组：组名 + 多元素。
    Array(ArrayExpansion),
}

impl PhraseEval {
    /// 便捷取首个 display（Array 取组名）。
    pub fn primary_display(&self) -> &str {
        match self {
            PhraseEval::Single { display, .. } => display,
            PhraseEval::Array(a) => &a.name,
        }
    }
}

/// 解析并求值一条短语文本。`$SS` 走 [`expand_array`]，其余走 [`evaluate`]。
pub fn evaluate_phrase(text: &str, ctx: &dyn EvalContext, reg: &Registry) -> Result<PhraseEval> {
    match parse(text)? {
        crate::Phrase::Array(a) => Ok(PhraseEval::Array(expand_array(&a, ctx, reg)?)),
        other => {
            let ev = evaluate(&other, ctx, reg)?;
            Ok(PhraseEval::Single {
                display: ev.display,
                actions: ev.actions,
                on_error: ev.on_error,
            })
        }
    }
}

/// 执行一条已求值短语的动作链（command 选中时调用）：拼接所有 [`ActionKind::Text`]
/// 的上屏文本，并按序触发 [`ActionKind::Effect`] 副作用。返回待上屏文本。
///
/// 动作在此延迟求值（按当前 `ctx`）。`on_error` 决定失败后是否继续跑余下动作；
/// 无论哪种，返回的都是**首个**错误。
///
/// ⚠️ 与宿主的 `run_command_candidate` 是两条执行路径（这里先跑完全部 Effect 再跑
/// Text，那边按源顺序逐个跑）。**新增的链语义必须两边都实现**，否则同一条词条在
/// 测试里与真机上表现不同——本仓「平行实现漂移」的经典入口。
pub fn run_actions(
    actions: &[ResolvedAction],
    ctx: &dyn EvalContext,
    reg: &Registry,
    on_error: OnError,
) -> (String, Option<crate::CmdbarError>) {
    let mut insert = String::new();
    let mut first_err = None;
    // 先 Effect（text 之前）保持与 Go 时序一致：副作用在落字前同步执行。
    for act in actions.iter().filter(|a| a.kind == ActionKind::Effect) {
        if let Err(e) = act.run(ctx, reg)
            && first_err.is_none()
        {
            first_err = Some(e);
        }
        if first_err.is_some() && on_error == OnError::Stop {
            return (insert, first_err);
        }
    }
    for act in actions.iter().filter(|a| a.kind == ActionKind::Text) {
        match act.run(ctx, reg) {
            Ok(s) => insert.push_str(&s),
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        if first_err.is_some() && on_error == OnError::Stop {
            return (insert, first_err);
        }
    }
    (insert, first_err)
}

/// 源是不是「纯文本直通」：`$CC("标签", type("字面量"))`——恰一个动作、动作是 `type`、
/// 实参是不含插值的字符串字面量、无 modifiers（`$CC1` 带 `prefix`，故不算）。
/// 返回上屏文本（即 `type` 的实参，不是标签）；其余一律 `None`。
///
/// 给不能求值、不能执行动作的场合用（码表整句词图）：这类词条上屏什么在解析期就定了，
/// 没有副作用，可以当普通词条用；含插值 / 多动作 / Effect 动作的则不行。
pub fn plain_type_text(src: &str) -> Option<String> {
    let Ok(Phrase::Command(cmd)) = parse(src) else {
        return None;
    };
    if !cmd.modifiers.is_empty() || cmd.actions.len() != 1 {
        return None;
    }
    let Expr::Call { name, args, named } = &cmd.actions[0] else {
        return None;
    };
    if name != "type" || args.len() != 1 || !named.is_empty() {
        return None;
    }
    let Expr::StringLit(parts) = &args[0] else {
        return None;
    };
    let mut out = String::new();
    for part in parts {
        match part {
            StringPart::Text(t) => out.push_str(t),
            StringPart::Interp(_) => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
#[path = "phrase_tests.rs"]
mod tests;
