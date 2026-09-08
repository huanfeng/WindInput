//! 短语级高层 API（宿主集成入口）。
//!
//! 把「解析 → 求值 / `$SS` 展开」打包成一次调用，并提供 [`is_cmdbar_grammar`] 供宿主
//! 与旧的简单模板路径分流（对应 Go coordinator 的 phrase hook + 双路径策略 design §7.2）。
//!
//! 线程/求值：display 侧只用纯函数（[`Registry::with_builtins`]）即可；命令动作需要
//! 宿主注入 [`Services`](crate::services::Services) 后用 [`Registry::full`]。

use crate::ast::OnError;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::MemoryContext;

    #[test]
    fn grammar_detection() {
        assert!(is_cmdbar_grammar("{date()}"));
        assert!(is_cmdbar_grammar(r#"$CC("x", type("x"))"#));
        assert!(is_cmdbar_grammar(r#"$SS("g", "a")"#));
        // 旧简单模板（无顶层 `{`）不算命令栏语法
        assert!(!is_cmdbar_grammar("$Y年$M月"));
        assert!(!is_cmdbar_grammar("纯文本"));
    }

    #[test]
    fn evaluate_template_phrase() {
        let reg = Registry::with_builtins();
        let ctx = MemoryContext::new().with_input("abc");
        let r = evaluate_phrase("len={len(code)}", &ctx, &reg).unwrap();
        match r {
            PhraseEval::Single {
                display, actions, ..
            } => {
                assert_eq!(display, "len=3");
                assert!(actions.is_empty());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn evaluate_array_phrase() {
        let reg = Registry::full();
        let ctx = MemoryContext::new();
        let r = evaluate_phrase(r#"$SS("符号", "（）", "【】")"#, &ctx, &reg).unwrap();
        match r {
            PhraseEval::Array(a) => {
                assert_eq!(a.name, "符号");
                assert_eq!(a.elements.len(), 2);
                assert_eq!(a.elements[0].display, "（）");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn run_actions_collects_text_and_effects() {
        use crate::services::{KeyInjector, Services};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct Log(Mutex<Vec<String>>);
        impl KeyInjector for Log {
            fn tap(&self, c: &str) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(c.into());
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

        let r =
            evaluate_phrase(r#"$CC("《》", type("《》"), key.tap("Left"))"#, &ctx, &reg).unwrap();
        let actions = match r {
            PhraseEval::Single { actions, .. } => actions,
            _ => panic!(),
        };
        let (insert, err) = run_actions(&actions, &ctx, &reg, OnError::Continue);
        assert!(err.is_none());
        assert_eq!(insert, "《》");
        assert_eq!(log.0.lock().unwrap().as_slice(), &["Left".to_string()]);
    }

    /// 端到端 ime 命令（宿主 $CC 执行通路依赖此流程）：
    /// `$CC("切简繁", ime.toggle("s2t"))` 选中 → 派发到 ImeController.toggle("s2t")，无上屏文本。
    #[test]
    fn run_ime_command_dispatches_to_controller() {
        use crate::services::{ImeController, Services};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct Ime(Mutex<Vec<String>>);
        impl ImeController for Ime {
            fn toggle(&self, target: &str) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(target.into());
                Ok(())
            }
            fn open_setting(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn open_setting_web(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn set_schema(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn theme_cycle(&self, _: &str) -> anyhow::Result<String> {
                Ok(String::new())
            }
        }

        let ime = Arc::new(Ime::default());
        let mut svc = Services::new();
        svc.ime = Some(ime.clone());
        let ctx = MemoryContext::new().with_services(svc);
        let reg = Registry::full();

        let r = evaluate_phrase(r#"$CC("切简繁", ime.toggle("s2t"))"#, &ctx, &reg).unwrap();
        let (insert, err) = match r {
            PhraseEval::Single { actions, .. } => {
                run_actions(&actions, &ctx, &reg, OnError::Continue)
            }
            _ => panic!(),
        };
        assert!(err.is_none());
        assert_eq!(insert, ""); // 纯副作用命令无上屏文本
        assert_eq!(ime.0.lock().unwrap().as_slice(), &["s2t".to_string()]);
    }

    /// 端到端配对命令，锁住 `data/system.phrases.toml` 里 `cojk` 那条的实际写法。
    ///
    /// 系统短语的语法错误**用户零感知**——候选照常出现，选中后什么都不发生。所以这条
    /// 走完整流程（解析 → 求值 → 派发），而不是只测函数本身。
    #[test]
    fn run_pair_command_dispatches_left_right_and_steps() {
        use crate::services::{ImeController, Services};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct Ime(Mutex<Vec<String>>);
        impl ImeController for Ime {
            fn toggle(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn open_setting(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn open_setting_web(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn set_schema(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn theme_cycle(&self, _: &str) -> anyhow::Result<String> {
                Ok(String::new())
            }
            fn pair(&self, left: &str, right: &str, jump_steps: u32) -> anyhow::Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("{left}|{right}|{jump_steps}"));
                Ok(())
            }
        }

        let ime = Arc::new(Ime::default());
        let mut svc = Services::new();
        svc.ime = Some(ime.clone());
        let ctx = MemoryContext::new().with_services(svc);
        let reg = Registry::full();

        // 系统短语 cojk 的原文
        let r = evaluate_phrase(r#"$CC("「」", ime.pair("「", "」"))"#, &ctx, &reg).unwrap();
        let (insert, err) = match r {
            PhraseEval::Single { actions, .. } => {
                run_actions(&actions, &ctx, &reg, OnError::Continue)
            }
            _ => panic!(),
        };
        assert!(err.is_none(), "{err:?}");
        // 文本由 ime.pair 自己推送，不经 $CC 的上屏通道。
        assert_eq!(insert, "");
        assert_eq!(ime.0.lock().unwrap().as_slice(), &["「|」|1".to_string()]);

        // 具名参数形式也要能走通全流程
        let r = evaluate_phrase(
            r#"$CC("注释", ime.pair("<!--", "-->", jump=1))"#,
            &ctx,
            &reg,
        )
        .unwrap();
        let (_, err) = match r {
            PhraseEval::Single { actions, .. } => {
                run_actions(&actions, &ctx, &reg, OnError::Continue)
            }
            _ => panic!(),
        };
        assert!(err.is_none(), "{err:?}");
        assert_eq!(ime.0.lock().unwrap()[1], "<!--|-->|1");
    }

    /// 缺服务时命令优雅降级：动作返回 ServiceUnavailable，run_actions 收集错误但不 panic。
    #[test]
    fn missing_service_degrades_gracefully() {
        let ctx = MemoryContext::new().with_services(crate::services::Services::new());
        let reg = Registry::full();
        let r = evaluate_phrase(r#"$CC("x", open("https://y"))"#, &ctx, &reg).unwrap();
        let (insert, err) = match r {
            PhraseEval::Single { actions, .. } => {
                run_actions(&actions, &ctx, &reg, OnError::Continue)
            }
            _ => panic!(),
        };
        assert_eq!(insert, "");
        assert!(matches!(
            err,
            Some(crate::CmdbarError::ServiceUnavailable { .. })
        ));
    }

    /// `{on_error: "stop"}` 解析出来，且默认（不写）是 Continue。
    ///
    /// 默认值这条断言是**防回归**用的：把默认改成 Stop 会让「前一步失败、后一步照跑」
    /// 的既有词条（如剪贴板占用时仍要移回光标）静默少做一步。
    #[test]
    fn on_error_modifier_parses_and_defaults_to_continue() {
        let reg = Registry::full();
        let ctx = MemoryContext::new().with_services(crate::services::Services::new());

        let got = |src: &str| match evaluate_phrase(src, &ctx, &reg).unwrap() {
            PhraseEval::Single { on_error, .. } => on_error,
            _ => panic!(),
        };
        assert_eq!(got(r#"$CC("x", open("u"))"#), OnError::Continue);
        assert_eq!(
            got(r#"$CC("x", open("u"), {on_error: "continue"})"#),
            OnError::Continue
        );
        assert_eq!(
            got(r#"$CC("x", open("u"), {on_error: "stop"})"#),
            OnError::Stop
        );
        // 值写错必须报错：静默落回默认，表现恰好就是这个修饰符要防的"假成功"。
        let err = evaluate_phrase(r#"$CC("x", open("u"), {on_error: "halt"})"#, &ctx, &reg)
            .expect_err("未知值应报错");
        assert!(err.to_string().contains("stop"), "{err}");
    }

    /// Stop 时首个失败即停，后续动作**不执行**；Continue 时照跑完。
    ///
    /// 判据落在"后续动作有没有留下痕迹"上，而不是返回的错误——两种模式返回的都是
    /// 首个错误，只看 err 分不出它们。
    #[test]
    fn on_error_stop_halts_remaining_actions() {
        use crate::services::{ImeController, Services};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct Ime(Mutex<Vec<String>>);
        impl ImeController for Ime {
            fn toggle(&self, target: &str) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(target.into());
                Ok(())
            }
            fn open_setting(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn open_setting_web(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn set_schema(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn theme_cycle(&self, _: &str) -> anyhow::Result<String> {
                Ok(String::new())
            }
        }

        // open 无服务 ⇒ 第一个动作必失败；ime.toggle 有服务 ⇒ 跑到就会留痕。
        let run = |on_error: OnError| {
            let ime = Arc::new(Ime::default());
            let mut svc = Services::new();
            svc.ime = Some(ime.clone());
            let ctx = MemoryContext::new().with_services(svc);
            let reg = Registry::full();
            let src = r#"$CC("x", open("https://y"), ime.toggle("s2t"))"#;
            let actions = match evaluate_phrase(src, &ctx, &reg).unwrap() {
                PhraseEval::Single { actions, .. } => actions,
                _ => panic!(),
            };
            let (_, err) = run_actions(&actions, &ctx, &reg, on_error);
            (ime.0.lock().unwrap().clone(), err.is_some())
        };

        let (done, failed) = run(OnError::Continue);
        assert!(failed);
        assert_eq!(done, vec!["s2t".to_string()], "Continue 应跑完后续动作");

        let (done, failed) = run(OnError::Stop);
        assert!(failed);
        assert!(done.is_empty(), "Stop 应在首个错误处停住，后续动作不得执行");
    }
}
