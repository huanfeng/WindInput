use super::*;
use crate::FormatKind;

/// 白名单与取值实现必须一一对应：白名单放行而取值缺失时，模板在渲染期整条静默作废，
/// 加载期校验却照样放行。
#[test]
fn vars_match_whitelist() {
    // 覆盖全部四类可能出现的变量名（并集），逐个比对两侧口径
    let all = [
        "Y", "YYYY", "YY", "YC", "M", "MM", "MC", "D", "DD", "DC", "N", "THOU", "CNL", "CNU",
        "DIG", "AMT", "EXPR", "RESULT", "EXACT", "NOPE", "WC", "LY", "LYN", "LZ", "LM", "LD",
        "LMD", "LF",
    ];
    for name in all {
        let cases = [
            // 样本取平常日子（2026-06-14）：`$LF` 在非节日返回空串而非 None，
            // 用节日当天会放过「$LF 退化成 None」（正面覆盖见 `festival_is_empty_on_ordinary_days`）。
            (FormatKind::Date, date_var(name, 2026, 6, 14).is_some()),
            // 月日与完整日期共用 date_var，白名单也必须同口径——只给 Date 放行
            // 而漏了 MonthDay，`12.25` 的农历/年份条目会在渲染期整条静默作废。
            (FormatKind::MonthDay, date_var(name, 2026, 6, 14).is_some()),
            (
                FormatKind::YearMonth,
                year_month_var(name, 2026, 6).is_some(),
            ),
            (FormatKind::Number, number_var(name, "123").is_some()),
            (FormatKind::Calc, calc_var(name, "1+1", "2", "2").is_some()),
        ];
        for (kind, has_value) in cases {
            assert_eq!(
                kind.supports_var(name),
                has_value,
                "变量 ${} 在 kind={} 上两侧口径不一致（白名单={}，取值={}）",
                name,
                kind.as_str(),
                kind.supports_var(name),
                has_value
            );
        }
    }
}

#[test]
fn year_forms_differ_below_1000() {
    // $Y 原样、$YYYY 补零，不能合并成一个变量
    assert_eq!(date_var("Y", 999, 1, 1).unwrap(), "999");
    assert_eq!(date_var("YYYY", 999, 1, 1).unwrap(), "0999");
    assert_eq!(date_var("YY", 2026, 1, 1).unwrap(), "26");
    assert_eq!(date_var("YY", 2005, 1, 1).unwrap(), "05");
}

#[test]
fn month_day_forms() {
    assert_eq!(date_var("M", 2026, 6, 4).unwrap(), "6");
    assert_eq!(date_var("MM", 2026, 6, 4).unwrap(), "06");
    assert_eq!(date_var("MC", 2026, 12, 4).unwrap(), "十二");
    assert_eq!(date_var("DC", 2026, 12, 25).unwrap(), "二十五");
    assert_eq!(date_var("YC", 2005, 1, 1).unwrap(), "二〇〇五");
}

#[test]
fn number_forms() {
    assert_eq!(number_var("N", "123.45").unwrap(), "123.45");
    assert_eq!(number_var("THOU", "1234567").unwrap(), "1,234,567");
    assert_eq!(number_var("CNL", "123").unwrap(), "一百二十三");
    assert_eq!(number_var("CNU", "123").unwrap(), "壹佰贰拾叁");
    assert_eq!(number_var("DIG", "2026").unwrap(), "二〇二六");
    assert_eq!(number_var("AMT", "123").unwrap(), "壹佰贰拾叁元整");
    assert_eq!(number_var("AMT", "123.45").unwrap(), "壹佰贰拾叁元肆角伍分");
}

#[test]
fn lunar_forms() {
    assert_eq!(date_var("LMD", 2026, 6, 14).unwrap(), "四月廿九");
    assert_eq!(date_var("LM", 2026, 6, 14).unwrap(), "四月");
    assert_eq!(date_var("LD", 2026, 6, 14).unwrap(), "廿九");
    assert_eq!(date_var("LY", 2026, 6, 14).unwrap(), "丙午");
    assert_eq!(date_var("LZ", 2026, 6, 14).unwrap(), "马");
    assert_eq!(date_var("LF", 2026, 6, 19).unwrap(), "端午节");
    // 闰月要带「闰」字
    assert_eq!(date_var("LMD", 2020, 6, 1).unwrap(), "闰四月初十");
    // 干支按农历年，不按公历年：2026-01-01 仍在乙巳年
    assert_eq!(date_var("LY", 2026, 1, 1).unwrap(), "乙巳");
    assert_eq!(date_var("LZ", 2026, 1, 1).unwrap(), "蛇");
    // 农历年数字同理与公历年差 1，两者不可混用
    assert_eq!(date_var("LYN", 2026, 1, 1).unwrap(), "2025");
    assert_eq!(date_var("Y", 2026, 1, 1).unwrap(), "2026");
    assert_eq!(date_var("LYN", 2026, 6, 14).unwrap(), "2026");
}

/// 算不出来 → `None`（整条模板作废）。返回空串的话 `农历$LMD` 会剩下「农历」二字上屏。
/// 两种成因都要覆盖：范围外、公历日非法。
#[test]
fn unconvertible_dates_kill_the_template() {
    // 超出 1900–2100
    assert!(date_var("LMD", 1899, 12, 31).is_none());
    assert!(date_var("LMD", 2101, 1, 1).is_none());
    // 非法公历日期（儒略日会照算成 3/3，必须挡住）
    assert!(date_var("LMD", 2026, 2, 31).is_none());
    // 范围外时所有农历变量一起作废，不能只挡住其中几个
    for name in ["LY", "LYN", "LZ", "LM", "LD", "LMD", "LF"] {
        assert!(
            date_var(name, 2101, 1, 1).is_none(),
            "${name} 应随换算失败作废"
        );
    }
}

/// 算得出来但这一天没有值 → 空串（整条照常出），不是 `None`。只有 `$LF` 属于这一类：
/// `$LY年$LMD$LF` 在平常日子应给出「丙午年四月廿九」。与 `$AMT`「小数超两位返回空串」同一约定。
#[test]
fn festival_is_empty_on_ordinary_days() {
    // 非节日：$LF 有值且为空，同一天其它农历变量照常有值
    assert_eq!(date_var("LF", 2026, 6, 14).unwrap(), "");
    assert_eq!(date_var("LMD", 2026, 6, 14).unwrap(), "四月廿九");
    // 节日当天照常给出节日名
    assert_eq!(date_var("LF", 2026, 6, 19).unwrap(), "端午节");
}

/// 年月类不支持农历：农历月与公历月不一一对应，`2026.12` 推不出唯一农历月。
#[test]
fn year_month_has_no_lunar_vars() {
    for name in ["LY", "LZ", "LM", "LD", "LMD", "LF"] {
        assert!(
            year_month_var(name, 2026, 12).is_none(),
            "year_month 不该支持 ${name}"
        );
        assert!(!FormatKind::YearMonth.supports_var(name));
        assert!(FormatKind::Date.supports_var(name), "date 应支持 ${name}");
    }
}

/// 「不适用」用空串表达，不是 None——渲染层据此丢弃该条候选。
#[test]
fn amount_is_empty_beyond_two_decimals() {
    assert_eq!(number_var("AMT", "1.234").unwrap(), "");
    assert!(
        !number_var("CNL", "1.234").unwrap().is_empty(),
        "中文小数不受两位限制"
    );
}
