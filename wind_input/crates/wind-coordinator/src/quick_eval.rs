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
    fn reverse_lookup(&self, _text: &str, _format: &str) -> String {
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
mod tests {
    use super::*;

    fn date() -> QuickValues {
        QuickValues::Date {
            y: 2025,
            m: 6,
            d: 5,
        }
    }

    /// ★ 端到端回归：打 `1/3` 出厂候选须含百分比档，且不是从已截断的 `$RESULT`
    /// 二次舍入出来的——`decimal_places=2` 时 `$RESULT` 是 "0.33"，若 `pct()` 从它
    /// 再算，`×100` 只会得到 "33%" 而非 "33.33%"。走真实出厂表 + 真实 eval，
    /// 而不是像 `funcs::quick` 单测那样手填 `$EXACT`，才盖得住整条接线。
    #[test]
    fn calc_percent_uses_exact_not_rounded_result() {
        let got = wind_quick_input::generate_with_eval(
            wind_quick_input::QuickSource::Calc,
            "1/3",
            2,
            &wind_quick_input::FormatTable::builtin(),
            Some(&eval_expr),
        );
        assert_eq!(got, vec!["0.33", "1/3=0.33", "33.33%"]);
    }

    /// ★ 表达式路径与变量路径必须同源：无参函数 == 对应的 `$` 变量。
    #[test]
    fn bare_calls_match_variable_path() {
        let v = date();
        assert_eq!(eval_expr("{year()}", &v).unwrap(), v.get("Y").unwrap());
        assert_eq!(eval_expr("{month()}", &v).unwrap(), v.get("M").unwrap());
        assert_eq!(eval_expr("{day()}", &v).unwrap(), v.get("D").unwrap());
        let n = QuickValues::Number {
            subject: "1234567".into(),
        };
        assert_eq!(eval_expr("{amt()}", &n).unwrap(), n.get("AMT").unwrap());
        assert_eq!(eval_expr("{thou()}", &n).unwrap(), n.get("THOU").unwrap());
        assert_eq!(eval_expr("{cn()}", &n).unwrap(), n.get("CNL").unwrap());
    }

    /// 命名参数是这条路径存在的理由：变量表达不了的偏离，参数能表达。
    #[test]
    fn named_params_reach_beyond_variables() {
        let v = date();
        assert_eq!(
            eval_expr("{year()}年{month(pad=2)}月{day(pad=2)}日", &v).unwrap(),
            "2025年06月05日"
        );
        assert_eq!(
            eval_expr("{month(cn='true')}月{day(cn='true')}日", &v).unwrap(),
            "六月五日"
        );
        let n = QuickValues::Number {
            subject: "1234567".into(),
        };
        assert_eq!(eval_expr("{thou(sep=' ')}", &n).unwrap(), "1 234 567");
        assert_eq!(eval_expr("{thou(group=4)}", &n).unwrap(), "123,4567");
        assert_eq!(
            eval_expr("{amt(unit='圆', zheng='false')}", &n).unwrap(),
            "壹佰贰拾叁万肆仟伍佰陆拾柒圆"
        );
    }

    /// 字面量与函数可以混排（这是模板的常态）。
    #[test]
    fn literals_mix_with_calls() {
        let n = QuickValues::Number {
            subject: "123.45".into(),
        };
        assert_eq!(eval_expr("¥{thou()}", &n).unwrap(), "¥123.45");
    }

    /// 内置的通用文本函数照常可用，可与 quick 取值函数复合。
    ///
    /// ⚠️ `s2t` / `pinyin` 目前是 **stub（原样返回）**，繁体金额、数字读音这类需求
    /// 要等 cmdbar 补上实现，别在文档里承诺。
    #[test]
    fn builtin_text_funcs_compose_with_quick_funcs() {
        let n = QuickValues::Number {
            subject: "123".into(),
        };
        assert_eq!(eval_expr("{concat('¥', thou())}", &n).unwrap(), "¥123");
        assert_eq!(
            eval_expr("{replace(amt(), '整', '')}", &n).unwrap(),
            "壹佰贰拾叁元"
        );
        // 空值兜底：本次输入无金额写法时给个替代文本，而不是让候选消失
        let d = QuickValues::Number {
            subject: "1.234".into(),
        };
        assert_eq!(eval_expr("{default(amt(), '—')}", &d).unwrap(), "—");

        // 现状记录：stub 函数原样返回（不是本改动引入的，改动时会红）
        assert_eq!(eval_expr("{s2t(amt())}", &n).unwrap(), "壹佰贰拾叁元整");
    }

    /// 错误一律吞成 None（候选不出现），不 panic、不上屏半成品。
    #[test]
    fn errors_yield_none() {
        let v = date();
        assert!(eval_expr("{no_such_func()}", &v).is_none());
        assert!(eval_expr("{month(nope=1)}", &v).is_none(), "未登记的参数名");
        assert!(eval_expr("{month(cn='maybe')}", &v).is_none(), "参数值非法");
        assert!(eval_expr("{month(", &v).is_none(), "语法错误");
    }

    /// 跨类调用取不到值 → 空串 → 该条候选被丢弃，而不是给出一个假值。
    #[test]
    fn cross_kind_call_is_empty_not_wrong() {
        let n = QuickValues::Number {
            subject: "123.45".into(),
        };
        assert_eq!(eval_expr("{year()}", &n).unwrap(), "");
    }

    /// 动作函数不在 quick 注册表里——格式表不该能产生副作用。
    #[test]
    fn action_funcs_are_not_reachable() {
        let v = date();
        assert!(eval_expr("{clip.set('x')}", &v).is_none());
    }

    #[test]
    fn precheck_accepts_factory_table() {
        precheck(&FormatTable::builtin()); // 不 panic 即可（出厂表无表达式条目）
    }

    /// 农历表达式：八个 part 都能求值，且与 `$` 变量同源。
    #[test]
    fn lunar_expression_parts() {
        // 2026-06-19 端午
        let v = QuickValues::Date {
            y: 2026,
            m: 6,
            d: 19,
        };
        assert_eq!(eval_expr("{lunar()}", &v).unwrap(), v.get("LMD").unwrap());
        assert_eq!(
            eval_expr("{lunar(part='ganzhi')}", &v).unwrap(),
            v.get("LY").unwrap()
        );
        assert_eq!(eval_expr("{lunar(part='zodiac')}", &v).unwrap(), "马");
        assert_eq!(eval_expr("{lunar(part='year')}", &v).unwrap(), "2026");
        assert_eq!(eval_expr("{lunar(part='festival')}", &v).unwrap(), "端午节");
        assert_eq!(
            eval_expr("{lunar(part='full')}", &v).unwrap(),
            "丙午年五月初五"
        );
        // 与字面量混排
        assert_eq!(eval_expr("农历{lunar()}", &v).unwrap(), "农历五月初五");
        assert!(eval_expr("{lunar(part='nope')}", &v).is_none());
    }

    /// ★ 农历取不到值时整条候选消失，而**不是**上屏半截的「农历」。
    ///
    /// 这是 `lunar()` 取不到值时报错（而非给空串）的理由：表达式路径是整条插值，
    /// 字面前缀不会因为函数返回空串而消失。
    #[test]
    fn lunar_out_of_range_kills_whole_template() {
        let out = QuickValues::Date {
            y: 1899,
            m: 12,
            d: 31,
        };
        assert!(eval_expr("农历{lunar()}", &out).is_none());
        assert!(eval_expr("{lunar(part='full')}", &out).is_none());
        // 同一条输入下公历函数照常——不能因为农历算不出就把整类候选拖死
        assert_eq!(eval_expr("{year()}年", &out).unwrap(), "1899年");
    }

    /// ★ 非节日当天 `{lunar(part='festival')}` 求值成**空串**，整条照常出。
    ///
    /// 与 `$LF` 变量路径同语义（见 `wind_quick_input::lunar::var`）：表达式路径和
    /// 变量路径对同一天必须给出同一个答案，否则用户在两种写法间迁移会撞见行为差异。
    #[test]
    fn lunar_festival_is_empty_on_ordinary_days() {
        let plain = QuickValues::Date {
            y: 2026,
            m: 6,
            d: 14,
        };
        assert_eq!(
            eval_expr("今天是{lunar(part='festival')}", &plain).unwrap(),
            "今天是"
        );
        // 追加式写法：平常日子只少节日名，日期部分照常
        assert_eq!(
            eval_expr(
                "{lunar(part='ganzhi')}年{lunar()}{lunar(part='festival')}",
                &plain
            )
            .unwrap(),
            "丙午年四月廿九"
        );
        assert_eq!(eval_expr("农历{lunar()}", &plain).unwrap(), "农历四月廿九");

        // 节日当天照常追加节日名
        let duanwu = QuickValues::Date {
            y: 2026,
            m: 6,
            d: 19,
        };
        assert_eq!(
            eval_expr(
                "{lunar(part='ganzhi')}年{lunar()}{lunar(part='festival')}",
                &duanwu
            )
            .unwrap(),
            "丙午年五月初五端午节"
        );
    }

    /// ★ 预检样本取**平常日子**，全部 part 仍须取到值。
    ///
    /// `$LF` 若回归成「非节日返回 None」，这里会立刻红——出厂配置被预检误报成
    /// 配置错误的回归，不该等用户发现。
    #[test]
    fn precheck_sample_covers_all_lunar_parts() {
        let v = date_sample();
        for p in [
            "md", "month", "day", "ganzhi", "zodiac", "year", "festival", "full",
        ] {
            assert!(
                eval_expr(&format!("{{lunar(part='{p}')}}"), &v).is_some(),
                "预检样本取不到 part={p}，会把正确的配置报成错误"
            );
        }
    }
}
