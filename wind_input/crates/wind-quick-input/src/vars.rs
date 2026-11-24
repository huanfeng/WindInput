//! 格式表变量的取值。
//!
//! 每个函数对应一个 [`crate::FormatKind`]，返回 `None` 表示「本类不支持该变量」——
//! 与 [`FormatKind::supports_var`](crate::FormatKind::supports_var) 的白名单**必须一一对应**：
//! 白名单放行而这里取不到值，模板会在渲染期悄悄整条作废，加载期的校验就白做了。
//! 两处都改的义务由 `vars_match_whitelist` 测试兜底。
//!
//! 日期类变量名与 `system.phrases.toml` 同名同义（`$Y` `$MM` `$YC` …），差别只在数据源：
//! 短语层绑当前时间，这里绑用户打进去的数字。

use crate::{
    FormatKind, decimal_to_amount, decimal_to_chinese_text, digits_to_chinese_chars,
    format_thousands, number_to_amount, small_int_to_chinese, split_decimal, year_to_chinese,
};

/// 一次输入解析出的量。
///
/// 两条模板路径的**共同数据源**：`$` 变量经 [`Self::get`] 取值，`{表达式}` 经宿主的
/// `EvalContext::quick_var` 转发到同一个 [`Self::get`]。同源是刻意的——
/// `{month()}` 与 `$M` 若能取到不同的值，用户就没法在两种写法间迁移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickValues {
    Date {
        y: i32,
        m: u32,
        d: u32,
    },
    /// 只打了月日（`12.25`），`y` 是代码补的当前年。
    ///
    /// 与 [`Self::Date`] 同构且共用 [`date_var`]——差别不在取值，而在**渲染哪一组条目**
    /// （见 [`FormatKind::MonthDay`]）。合成一个变体就没法让两种输入形态各有出厂格式集。
    MonthDay {
        y: i32,
        m: u32,
        d: u32,
    },
    YearMonth {
        y: i32,
        m: u32,
    },
    Number {
        subject: String,
    },
    Calc {
        expr: String,
        result: String,
        /// 结果的原始精度（未按 `decimal_places` 截断/去尾零）。供 `{pct()}` 这类
        /// 需要自行选倍数/小数位/后缀的函数在此基础上二次换算——若只能拿到已按
        /// `decimal_places` 舍入过的 `$RESULT`，`decimal_places=2` 时 1/3 的 `$RESULT`
        /// 已经是 "0.33"，`×100` 只能得到 "33" 而非用户想要的 "33.33"。
        exact: String,
    },
}

impl QuickValues {
    /// 按变量名取值。名字即 `system.quick.toml` 的 `$` 变量名。
    ///
    /// 返回 `None` = 本类不支持该变量（与 [`FormatKind::supports_var`] 一一对应）。
    pub fn get(&self, name: &str) -> Option<String> {
        match self {
            Self::Date { y, m, d } | Self::MonthDay { y, m, d } => date_var(name, *y, *m, *d),
            Self::YearMonth { y, m } => year_month_var(name, *y, *m),
            Self::Number { subject } => number_var(name, subject),
            Self::Calc {
                expr,
                result,
                exact,
            } => calc_var(name, expr, result, exact),
        }
    }

    /// 本次解析对应的格式类别。
    pub fn kind(&self) -> FormatKind {
        match self {
            Self::Date { .. } => FormatKind::Date,
            Self::MonthDay { .. } => FormatKind::MonthDay,
            Self::YearMonth { .. } => FormatKind::YearMonth,
            Self::Number { .. } => FormatKind::Number,
            Self::Calc { .. } => FormatKind::Calc,
        }
    }
}

/// 年份三态 + 中文年。`date` / `month_day` / `year_month` 共用
/// （`month_day` 的年是代码补的当前年，取值路径与另两类无差别）。
fn year_var(name: &str, y: i32) -> Option<String> {
    Some(match name {
        // 原样：改造前 `format!("{}年", year)` 的等价物
        "Y" => y.to_string(),
        // 补零到四位：改造前 ISO/斜杠形态用的是 `{:04}`，与 `$Y` 在 y<1000 时不同
        "YYYY" => format!("{:04}", y),
        // 后两位（`25-12-25` 这类写法）
        "YY" => format!("{:02}", y.rem_euclid(100)),
        "YC" => year_to_chinese(y),
        _ => return None,
    })
}

/// `kind = "date"` 与 `kind = "month_day"`：年 + 月 + 日（含农历）。
///
/// 两类共用一份实现——它们的差别在渲染哪组条目，不在取值。
pub(crate) fn date_var(name: &str, y: i32, m: u32, d: u32) -> Option<String> {
    if let Some(v) = year_var(name, y) {
        return Some(v);
    }
    if crate::lunar::is_var(name) {
        // ★ **换算不出**（超出 1900–2100，或 2 月 31 日这种非法公历日）时返回 `None`
        // 而**不是**空串：空串只在整条模板恰好只有这一个变量时才会让候选消失，
        // 而 `农历$LMD` 会剩下「农历」二字上屏。`None` 让整条模板作废，
        // 公历那几条候选不受影响。
        //
        // 这与 `$LF` 在**非节日**返回空串不冲突，两者是不同的问题：那里日期换算成功了、
        // 只是这一天没有节日名，`$LY年$LMD$LF` 理应给出「丙午年四月廿九」而非整条消失。
        // 详见 [`crate::lunar::var`] 的「`None` 与空串的分工」。
        return crate::lunar::solar_to_lunar(y, m, d).and_then(|l| crate::lunar::var(name, &l));
    }
    Some(match name {
        "M" => m.to_string(),
        "MM" => format!("{:02}", m),
        "MC" => small_int_to_chinese(m),
        "D" => d.to_string(),
        "DD" => format!("{:02}", d),
        "DC" => small_int_to_chinese(d),
        _ => return None,
    })
}

/// `kind = "year_month"`：年 + 月（无日）。
pub(crate) fn year_month_var(name: &str, y: i32, m: u32) -> Option<String> {
    if let Some(v) = year_var(name, y) {
        return Some(v);
    }
    Some(match name {
        "M" => m.to_string(),
        "MM" => format!("{:02}", m),
        "MC" => small_int_to_chinese(m),
        _ => return None,
    })
}

/// `kind = "number"`：`subject` 是 `number_subject` 的产出（纯数字串，可含一个小数点）。
///
/// `$AMT` 在「小数超两位」时返回**空串**而非 `None`：那不是「不支持该变量」，
/// 而是这条候选在本次输入下无意义（无角分写法），空串会让渲染层丢弃该条。
pub(crate) fn number_var(name: &str, subject: &str) -> Option<String> {
    let (int_raw, dec_part) = split_decimal(subject);
    let int_part = if int_raw.is_empty() { "0" } else { int_raw };
    Some(match name {
        "N" => subject.to_string(),
        "THOU" => format_thousands(int_part, dec_part),
        "CNL" => decimal_to_chinese_text(int_part, dec_part, false),
        "CNU" => decimal_to_chinese_text(int_part, dec_part, true),
        "DIG" => digits_to_chinese_chars(subject),
        "AMT" => {
            if dec_part.is_empty() {
                number_to_amount(int_part)
            } else {
                decimal_to_amount(int_part, dec_part)
            }
        }
        _ => return None,
    })
}

/// `kind = "calc"`：`expr` 为裁剪后的算式（不含 `=` 及其右侧），`result` 为格式化后的结果，
/// `exact` 为未截断的原始精度结果。
pub(crate) fn calc_var(name: &str, expr: &str, result: &str, exact: &str) -> Option<String> {
    Some(match name {
        "EXPR" => expr.to_string(),
        "RESULT" => result.to_string(),
        "EXACT" => exact.to_string(),
        _ => return None,
    })
}

#[cfg(test)]
#[path = "vars_tests.rs"]
mod tests;
