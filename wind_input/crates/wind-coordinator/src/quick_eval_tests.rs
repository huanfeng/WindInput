use super::*;

fn date() -> QuickValues {
    QuickValues::Date {
        y: 2025,
        m: 6,
        d: 5,
    }
}

/// 端到端：打 `1/3` 出厂候选须含百分比档，且不是从已截断的 `$RESULT` 二次舍入出来的
/// （`decimal_places=2` 时 `$RESULT` 是 "0.33"，从它再算只得 "33%" 而非 "33.33%"）。
/// 走真实出厂表 + 真实 eval，才盖得住整条接线。
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

/// 表达式路径与变量路径必须同源：无参函数 == 对应的 `$` 变量。
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
/// `s2t` / `pinyin` 目前是 stub（原样返回），繁体金额、数字读音要等 cmdbar 补上实现，
/// 别在文档里承诺。
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

/// 农历取不到值时整条候选消失，而不是上屏半截的「农历」。
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

/// 非节日当天 `{lunar(part='festival')}` 求值成空串，整条照常出。
///
/// 与 `$LF` 变量路径同语义（见 `wind_quick_input::lunar::var`）：两种写法对同一天
/// 必须给出同一个答案。
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

/// 预检样本取平常日子，全部 part 仍须取到值；`$LF` 回归成「非节日返回 None」时，
/// 出厂配置会被预检误报成配置错误。
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
