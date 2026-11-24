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
pub use parser::{contains_marker_call, parse};
pub use phrase::{PhraseEval, evaluate_phrase, is_cmdbar_grammar, plain_type_text, run_actions};
pub use registry::{Category, FuncSpec, Registry, default_registry};
pub use services::{
    CliSpawn, ClipboardService, ConfigService, DEFAULT_CLI_WAIT_MS, DictService, ImeController,
    KeyInjector, NotifyService, ProcSpawn, ProcessRunner, SearchEngine, Services, ToastSpec,
    UrlOpener,
};

#[cfg(test)]
#[path = "lib_integration_tests.rs"]
mod integration_tests;
