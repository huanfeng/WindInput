//! wind-cmdbar: 命令栏系统（表达式解析、求值、内置函数、动作服务）
//!
//! 对照 Go `wind_input/internal/cmdbar/` 完整移植。流水线：
//! `parse(src) -> Phrase` → `evaluate(phrase, ctx, reg) -> (display, actions)`
//! → 宿主选中候选时 `action.run(ctx, reg)` 执行（文本上屏 / 副作用）。
//!
//! `$SS` 字符串组 / `$AA` 字符组（`$SS` 的逐字符简写）数组短语经 [`eval::expand_array`]
//! 展开为多个候选。

pub mod action;
pub mod ast;
pub mod context;
pub mod error;
pub mod eval;
pub mod funcs;
pub mod lint;
pub mod parser;
pub mod phrase;
pub mod registry;
pub mod services;

pub use action::{ActionKind, ResolvedAction};
pub use ast::{ArrayPhrase, CommandPhrase, Expr, ModValue, Modifiers, OnError, Phrase};
pub use context::{EvalContext, History, MemoryContext};
pub use error::{CmdbarError, Result};
pub use eval::{ArrayElement, ArrayExpansion, Evaluated, evaluate, expand_array};
pub use funcs::action::validate_toast_args;
pub use funcs::value::generate_uuid;
pub use lint::{Hint, lint_parsed, lint_phrase};
pub use parser::parse;
pub use phrase::{PhraseEval, evaluate_phrase, is_cmdbar_grammar, run_actions};
pub use registry::{Category, FuncSpec, Registry, default_registry};
pub use services::{
    CliSpawn, ClipboardService, ConfigService, DEFAULT_CLI_WAIT_MS, DictService, ImeController,
    KeyInjector, NotifyService, ProcSpawn, ProcessRunner, SearchEngine, Services, ToastSpec,
    UrlOpener,
};

#[cfg(test)]
mod integration_tests {
    use super::*;

    /// 端到端：解析 → 求值 → 执行动作（mock 服务）。
    #[test]
    fn end_to_end_command_with_effect() {
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct Log(Mutex<Vec<String>>);
        impl KeyInjector for Log {
            fn tap(&self, c: &str) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(format!("tap:{c}"));
                Ok(())
            }
            fn sequence(&self, _: &[String]) -> anyhow::Result<()> {
                Ok(())
            }
            fn hold(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn release(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn type_text(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let log = Arc::new(Log::default());
        let mut svc = Services::new();
        svc.keys = Some(log.clone());
        let ctx = MemoryContext::new().with_services(svc);
        let reg = Registry::full();

        // $CC("《》", type("《》"), key.tap("Left"))：上屏《》并按 Left。
        let phrase = parse(r#"$CC("《》", type("《》"), key.tap("Left"))"#).unwrap();
        let ev = evaluate(&phrase, &ctx, &reg).unwrap();
        assert_eq!(ev.display, "《》");
        assert_eq!(ev.actions.len(), 2);

        // 执行：text 动作返回上屏文本，effect 动作触发副作用。
        let mut inserted = String::new();
        for act in &ev.actions {
            inserted.push_str(&act.run(&ctx, &reg).unwrap());
        }
        assert_eq!(inserted, "《》");
        assert_eq!(log.0.lock().unwrap().as_slice(), &["tap:Left".to_string()]);
    }

    #[test]
    fn template_uses_context() {
        let reg = Registry::with_builtins();
        let ctx = MemoryContext::new().with_input("abc");
        let p = parse("编码 {code} 共 {len(code)} 位").unwrap();
        assert_eq!(
            evaluate(&p, &ctx, &reg).unwrap().display,
            "编码 abc 共 3 位"
        );
    }
}
