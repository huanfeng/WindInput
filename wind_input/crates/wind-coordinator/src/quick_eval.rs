//! 快捷输入格式表的**表达式路径**求值（`{amt(unit='圆')}`）。
//!
//! 分发点在协调器而不在 `wind-quick-input`：后者零 `wind-*` 依赖，反向调用 `wind-cmdbar`
//! 会成环。这里同时依赖两者，把「模板 + 本次解析出的量」接起来。
//!
//! 求值用 [`Registry::quick`]（纯函数 + `quick` 取值函数族，**不含动作函数**）：
//! 格式表描述的是候选长什么样，不该能改剪贴板、按键或配置。

use chrono::{DateTime, Local};
use tracing::{debug, warn};
use wind_cmdbar::{EvalContext, PhraseEval, Registry, Services, evaluate_phrase};
use wind_quick_input::{FormatTable, QuickValues};

/// 格式表专用求值上下文。
///
/// 除 `quick_var` 外一律给空/默认：格式表只做「把解析出的量排成一句话」，
/// 剪贴板、选区、前台进程与它无关，暴露出去只会让人写出依赖环境的格式。
struct QuickCtx<'a> {
    values: &'a QuickValues,
    now: DateTime<Local>,
}

impl EvalContext for QuickCtx<'_> {
    fn input(&self) -> String {
        String::new()
    }
    fn last(&self, _n: i64) -> String {
        String::new()
    }
    fn clip(&self, _n: i64) -> String {
        String::new()
    }
    fn sel(&self) -> String {
        String::new()
    }
    fn app(&self) -> String {
        String::new()
    }
    fn title(&self) -> String {
        String::new()
    }
    fn env(&self, _name: &str) -> String {
        String::new()
    }
    fn reverse_lookup(&self, _text: &str, _format: &str, _min_len: usize) -> String {
        // 同 clip/sel/app：格式表只把「本次解析出的量」排成一句话，反查与它无关。
        // 放开只会让人写出依赖词库状态的格式（同一串数字在不同方案下出不同结果）。
        String::new()
    }
    fn now(&self) -> DateTime<Local> {
        self.now
    }
    fn services(&self) -> Option<&Services> {
        None
    }
    /// 与 `$` 变量同一个数据源：`{month()}` 与 `$M` 取到的必然是同一个值。
    fn quick_var(&self, name: &str) -> Option<String> {
        self.values.get(name)
    }
}

fn quick_registry() -> &'static Registry {
    static R: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    R.get_or_init(Registry::quick)
}

/// 求值一条表达式模板。失败返回 `None`（该条候选被丢弃）。
///
/// 这是**热路径**（每次按键刷新候选都会走），故失败只记 `debug`：模板写错是配置态
/// 而非偶发故障，每次按键 warn 一次会把日志刷爆。诊断由 [`precheck`] 在启动时一次性给出。
pub(crate) fn eval_expr(text: &str, values: &QuickValues) -> Option<String> {
    let ctx = QuickCtx {
        values,
        now: Local::now(),
    };
    match evaluate_phrase(text, &ctx, quick_registry()) {
        Ok(PhraseEval::Single {
            display, actions, ..
        }) if actions.is_empty() => Some(display),
        // `$CC` 命令短语：格式表不接受带副作用的条目
        Ok(PhraseEval::Single { .. }) => {
            debug!("快捷输入格式表: 表达式含动作，已忽略: {}", text);
            None
        }
        // `$SS`/`$AA` 多候选：一条格式只产一条候选，语义对不上
        Ok(PhraseEval::Array(_)) => {
            debug!("快捷输入格式表: 表达式产出多候选，已忽略: {}", text);
            None
        }
        Err(e) => {
            debug!("快捷输入格式表: 表达式求值失败 {:?}: {}", text, e);
            None
        }
    }
}

/// 预检用的日期样本。
///
/// ★ 刻意挑一个**平常日子**（2026-06-14，非节日）。这里曾被迫挑节日：`$LF` 当时在
/// 非节日返回 `None`，用平常日子会把一条完全正确的 `{lunar(part='festival')}` 配置
/// 报成错误。`$LF` 改为返回空串后该约束解除，而挑非节日反过来成了一道免费守门——
/// 若 `$LF` 哪天回归成 `None`，出厂配置会当场被预检报错，不必等用户发现。
fn date_sample() -> QuickValues {
    QuickValues::Date {
        y: 2026,
        m: 6,
        d: 14,
    }
}

/// 启动时预检全部表达式模板，把配置错误一次性报到日志里。
///
/// 没有这道预检，写错的表达式在运行期只是「那条候选不出现」——没有任何提示，
/// 用户只会觉得自己的配置没生效。这里用一组样本值做 dry-run，能抓出未知函数、
/// 未登记的参数名、以及 `zheng='nope'` 这类参数值错误。
///
/// 抓不到的是「只在特定输入下才失败」的模板，那类只能靠运行期 `debug` 日志。
pub(crate) fn precheck(table: &FormatTable) {
    let samples = [
        date_sample(),
        QuickValues::YearMonth { y: 2025, m: 12 },
        QuickValues::Number {
            subject: "123.45".to_string(),
        },
        QuickValues::Calc {
            expr: "1+1".to_string(),
            result: "2".to_string(),
            exact: "2".to_string(),
        },
    ];
    for entry in table.entries().iter().filter(|e| e.is_expression()) {
        let Some(values) = samples.iter().find(|v| v.kind() == entry.kind) else {
            continue;
        };
        let ctx = QuickCtx {
            values,
            now: Local::now(),
        };
        if let Err(e) = evaluate_phrase(&entry.text, &ctx, quick_registry()) {
            warn!(
                "快捷输入格式表: 条目 {} 的表达式无法求值，该候选将不会出现 —— {}",
                entry.id, e
            );
        }
    }
}

#[cfg(test)]
#[path = "quick_eval_tests.rs"]
mod tests;
