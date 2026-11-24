//! 快捷输入格式表的取值函数族（`system.quick.toml` 的表达式路径）。
//!
//! 取值一律经 [`EvalContext::quick_var`]——本文件**不做解析**，只做呈现加工。
//! 解析（`"2025.12.25"` → 年月日）在 `wind-quick-input` 里，故本 crate 不依赖它。
//!
//! ## 命名参数即「偏离出厂的那一点」
//!
//! 每个函数的**默认参数就是出厂行为**，与对应的 `$` 变量逐字等价：
//! `{month()}` ≡ `$M`，`{month(pad=2)}` ≡ `$MM`，`{month(cn='true')}` ≡ `$MC`。
//! 用户只需要为「与默认不同的那一处」写参数，而不是把整个格式拆成函数调用去拼。
//!
//! ## 不在这里的上下文里，一律空串
//!
//! `quick_var` 默认返回 `None`（见其文档），故这些函数写进短语只得到空串。
//! 这是刻意的：错值比空值难查。

use super::func_specs;
use crate::context::EvalContext;
use crate::error::{CmdbarError, Result};
use crate::registry::FuncSpec;

pub fn specs() -> Vec<FuncSpec> {
    func_specs! {
        "year"  : Value (0, 0) pure => fn_year,   "快捷输入: 年", "year(pad=4)"
            named(fn_year_named, "pad" = "左补零到几位, 默认不补", "cn" = "true 出中文数字 (二〇二五)");
        "month" : Value (0, 0) pure => fn_month,  "快捷输入: 月", "month(pad=2)"
            named(fn_month_named, "pad" = "左补零到几位, 默认不补", "cn" = "true 出中文数字 (十二)");
        "day"   : Value (0, 0) pure => fn_day,    "快捷输入: 日", "day(cn='true')"
            named(fn_day_named, "pad" = "左补零到几位, 默认不补", "cn" = "true 出中文数字 (二十五)");
        "lunar" : Value (0, 0) pure => fn_lunar,  "快捷输入: 农历月日 (四月廿九)", "lunar(part='ganzhi')"
            named(fn_lunar_named, "part" = "md(默认)/month/day/ganzhi/zodiac/year/festival/full");
        // 名字刻意不叫 num —— `num` 已是进制转换函数（calc.rs），同名会在本表里覆盖它，
        // 变成「同一个名字在两处含义不同」。`no_name_clash_with_builtins` 守住这条。
        "raw"   : Value (0, 0) pure => fn_raw,    "快捷输入: 所输数字原样", "raw()";
        "cn"    : Value (0, 0) pure => fn_cn,     "快捷输入: 中文数字 (一百二十三)", "cn(upper='true')"
            named(fn_cn_named, "upper" = "true 出大写 (壹佰贰拾叁)");
        "dig"   : Value (0, 0) pure => fn_dig,    "快捷输入: 逐位中文 (一二三)", "dig()";
        "thou"  : Value (0, 0) pure => fn_thou,   "快捷输入: 千分位 (1,234,567)", "thou(sep=' ')"
            named(fn_thou_named, "sep" = "分隔符, 默认 ','", "group" = "每组位数, 默认 3");
        "amt"   : Value (0, 0) pure => fn_amt,    "快捷输入: 大写金额 (壹佰贰拾叁元整)", "amt(unit='圆')"
            named(fn_amt_named, "unit" = "货币单位, 默认 '元'", "zheng" = "false 去掉末尾的 '整'");
        "expr"  : Value (0, 0) pure => fn_expr,   "快捷输入: 所输算式", "expr()";
        "result": Value (0, 0) pure => fn_result, "快捷输入: 算式结果", "result()";
        "pct"   : Value (0, 0) pure => fn_pct,    "快捷输入: 结果按比例转百分比/千分位等 (1/3→33.33%)", "pct(scale=1000, decimals=1, suffix='‰')"
            named(fn_pct_named, "scale" = "乘的倍数, 默认 100", "decimals" = "小数位数上限(去尾零), 默认 2", "suffix" = "结果后缀, 默认 '%'");
    }
}

// ───────────────────────── 参数解析 ─────────────────────────

/// 具名参数取值（未给出返回 None）。
fn named<'a>(named_args: &'a [(String, String)], key: &str) -> Option<&'a str> {
    named_args
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// 布尔参数。裸 `true` 会被表达式解析成函数调用，故必须写成字符串或 0/1。
/// 无法识别的值**报错而非当成 false**——静默吞掉写错的开关，用户会以为功能坏了。
fn parse_bool(func: &str, key: &str, v: &str) -> Result<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(CmdbarError::runtime(
            func,
            format!("{} 需要 'true' 或 'false'，得到 {:?}", key, v),
        )),
    }
}

/// 宽度/位数类参数的上限。
///
/// 三个消费者（`pad(pad=)`、`thou(group=)`、`pct(decimals=)`）都是**面向人写的排版常量**，
/// 超过几百就一定是笔误。而它们各自会按这个数分配：`pad_left` 造 N 个字符，
/// `format!("{:.*}", N, x)` 造 N 位小数——不设限时 `pct(decimals=99999999999)` 这一条
/// 模板就能让求值线程卡在分配上。上限本身取多少不敏感（候选窗一行远放不下 512 字符，
/// f64 超过 17 位有效数字之后全是舍入噪声），有界才是要点。
const MAX_WIDTH_ARG: usize = 512;

fn parse_width(func: &str, key: &str, v: &str) -> Result<usize> {
    let n = v
        .trim()
        .parse::<usize>()
        .map_err(|_| CmdbarError::runtime(func, format!("{} 需要非负整数，得到 {:?}", key, v)))?;
    if n > MAX_WIDTH_ARG {
        return Err(CmdbarError::runtime(
            func,
            format!("{key} 过大（{n}，上限 {MAX_WIDTH_ARG}）"),
        ));
    }
    Ok(n)
}

/// 左补零到 `width` 个字符（已够长则原样）。按 char 计数——中文数字模式下补零无意义，
/// 但用户真写了也不该 panic 在字节边界上。
fn pad_left(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        return s.to_string();
    }
    let mut out = String::with_capacity(width);
    for _ in 0..(width - n) {
        out.push('0');
    }
    out.push_str(s);
    out
}

/// 取 `quick_var`，不在快捷输入上下文时给空串。
fn var(ctx: &dyn EvalContext, name: &str) -> String {
    ctx.quick_var(name).unwrap_or_default()
}

// ───────────────────────── 日期三兄弟 ─────────────────────────

/// 年/月/日共用：`cn` 与 `pad` 的取值逻辑一样，只是原子变量名不同。
/// `cn=true` 时 `pad` 被忽略（给「十二」补零没有意义）。
fn date_part(
    ctx: &dyn EvalContext,
    named_args: &[(String, String)],
    func: &str,
    plain: &str,
    cn_name: &str,
) -> Result<String> {
    if let Some(v) = named(named_args, "cn")
        && parse_bool(func, "cn", v)?
    {
        return Ok(var(ctx, cn_name));
    }
    let raw = var(ctx, plain);
    match named(named_args, "pad") {
        Some(v) => Ok(pad_left(&raw, parse_width(func, "pad", v)?)),
        None => Ok(raw),
    }
}

// ───────────────────────── 农历 ─────────────────────────

/// 取农历变量。**换算不出就报错，不给空串**——这是与本文件其它函数相反的一条，
/// 理由是日期超出 1900–2100（或不在日期上下文）时农历压根算不出来，
/// 而模板通常带字面前缀：`农历{lunar()}` 若把空串填进去，会把「农历」二字单独上屏。
/// 报错则整条候选不出现，与 `$` 变量路径的行为一致。
///
/// 注意 `part='festival'`（`$LF`）**不属于**「取不到」：非节日当天它返回空串而非
/// `None`，于是 `{lunar(part='ganzhi')}年{lunar()}{lunar(part='festival')}` 在平常
/// 日子照常出「丙午年四月廿九」。两种「没有值」的分工见 `wind_quick_input::lunar::var`。
fn lunar_var(ctx: &dyn EvalContext, name: &str) -> Result<String> {
    ctx.quick_var(name).ok_or_else(|| {
        CmdbarError::runtime(
            "lunar",
            format!("取不到 ${name}：不在日期上下文，或日期超出 1900-2100"),
        )
    })
}

fn fn_lunar(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    lunar_var(ctx, "LMD")
}

fn fn_lunar_named(ctx: &dyn EvalContext, _a: &[String], n: &[(String, String)]) -> Result<String> {
    let name = match named(n, "part").unwrap_or("md").trim() {
        "md" => "LMD",
        "month" => "LM",
        "day" => "LD",
        "ganzhi" => "LY",
        "zodiac" => "LZ",
        "year" => "LYN",
        "festival" => "LF",
        // 干支年 + 月日。两段分别取，任一缺失都由 `?` 整条作废，不拼出「年四月廿九」
        "full" => {
            let gz = lunar_var(ctx, "LY")?;
            let md = lunar_var(ctx, "LMD")?;
            return Ok(format!("{gz}年{md}"));
        }
        other => {
            return Err(CmdbarError::runtime(
                "lunar",
                format!(
                    "part 未知取值 {:?}，可用: md/month/day/ganzhi/zodiac/year/festival/full",
                    other
                ),
            ));
        }
    };
    lunar_var(ctx, name)
}

fn fn_year(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "Y"))
}
fn fn_year_named(ctx: &dyn EvalContext, _a: &[String], n: &[(String, String)]) -> Result<String> {
    date_part(ctx, n, "year", "Y", "YC")
}

fn fn_month(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "M"))
}
fn fn_month_named(ctx: &dyn EvalContext, _a: &[String], n: &[(String, String)]) -> Result<String> {
    date_part(ctx, n, "month", "M", "MC")
}

fn fn_day(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "D"))
}
fn fn_day_named(ctx: &dyn EvalContext, _a: &[String], n: &[(String, String)]) -> Result<String> {
    date_part(ctx, n, "day", "D", "DC")
}

// ───────────────────────── 数字 ─────────────────────────

fn fn_raw(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "N"))
}

fn fn_cn(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "CNL"))
}
fn fn_cn_named(ctx: &dyn EvalContext, _a: &[String], n: &[(String, String)]) -> Result<String> {
    let upper = match named(n, "upper") {
        Some(v) => parse_bool("cn", "upper", v)?,
        None => false,
    };
    Ok(var(ctx, if upper { "CNU" } else { "CNL" }))
}

fn fn_dig(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "DIG"))
}

fn fn_thou(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "THOU"))
}

/// 千分位。`group` 非 3 时不能在成品串上改——分组位置本身变了，必须从原数重切。
fn fn_thou_named(ctx: &dyn EvalContext, _a: &[String], n: &[(String, String)]) -> Result<String> {
    let sep = named(n, "sep").unwrap_or(",");
    let group = match named(n, "group") {
        Some(v) => parse_width("thou", "group", v)?.max(1),
        None => 3,
    };
    if group == 3 {
        // 出厂形态就是 3 位一组，只需换分隔符
        return Ok(var(ctx, "THOU").replace(',', sep));
    }
    let raw = var(ctx, "N");
    let (int_part, dec_part) = match raw.split_once('.') {
        Some((i, d)) => (i, Some(d)),
        None => (raw.as_str(), None),
    };
    let digits: Vec<char> = int_part.chars().collect();
    let mut grouped = String::new();
    for (i, c) in digits.iter().enumerate() {
        // 从右往左每 group 位插一个分隔符：左侧余数段之后即第一个插入点
        if i > 0 && (digits.len() - i).is_multiple_of(group) {
            grouped.push_str(sep);
        }
        grouped.push(*c);
    }
    Ok(match dec_part {
        Some(d) => format!("{}.{}", grouped, d),
        None => grouped,
    })
}

fn fn_amt(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "AMT"))
}

/// 大写金额。空串表示本次输入无金额写法（负数/超两位小数），此时任何参数都不该
/// 把它变成非空——否则会凭空造出一条无意义候选。
fn fn_amt_named(ctx: &dyn EvalContext, _a: &[String], n: &[(String, String)]) -> Result<String> {
    let mut s = var(ctx, "AMT");
    if s.is_empty() {
        return Ok(s);
    }
    if let Some(v) = named(n, "zheng")
        && !parse_bool("amt", "zheng", v)?
    {
        s = s.trim_end_matches('整').to_string();
    }
    if let Some(unit) = named(n, "unit") {
        // 「元」在大写金额里只出现一次（整数部分之后），replace 不会误伤
        s = s.replace('元', unit);
    }
    Ok(s)
}

// ───────────────────────── 计算 ─────────────────────────

fn fn_expr(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "EXPR"))
}
fn fn_result(ctx: &dyn EvalContext, _a: &[String]) -> Result<String> {
    Ok(var(ctx, "RESULT"))
}

/// 按比例换算结果并加后缀：默认 `scale=100, decimals=2, suffix='%'`，即百分比。
/// 换 `scale=1000, suffix='‰'` 是千分比，`scale=10000, suffix='bp'` 是基点——
/// 一个函数覆盖这一族「结果 × 倍数 + 单位」的写法，不必逐个再开新函数。
///
/// 换算基于 `$EXACT`（原始未截断精度）而非 `$RESULT`：若从已按 `decimal_places`
/// 舍入过的 `$RESULT` 再乘 `scale`，`decimal_places=2` 时 1/3 的 `$RESULT` 已是
/// `"0.33"`，`×100` 只能得到 `"33"` 而非 `"33.33"`——二次舍入会丢精度。
fn fn_pct(ctx: &dyn EvalContext, a: &[String]) -> Result<String> {
    fn_pct_named(ctx, a, &[])
}

fn fn_pct_named(ctx: &dyn EvalContext, _a: &[String], n: &[(String, String)]) -> Result<String> {
    let scale = match named(n, "scale") {
        Some(v) => parse_scale("pct", v)?,
        None => 100.0,
    };
    let decimals = match named(n, "decimals") {
        Some(v) => parse_width("pct", "decimals", v)?,
        None => 2,
    };
    let suffix = named(n, "suffix").unwrap_or("%");
    // 跨类调用（不在计算上下文）：与 fn_amt/fn_raw 等既有函数同一约定，给空串
    // 而非报错——报错会让「有的模板碰巧含 pct()」在非计算类别整条候选消失。
    let raw = var(ctx, "EXACT");
    if raw.is_empty() {
        return Ok(String::new());
    }
    let v: f64 = raw
        .parse()
        .map_err(|_| CmdbarError::runtime("pct", format!("内部值解析失败: {raw:?}")))?;
    // 溢出成 ±inf / NaN 时不能照 format 出去：`{:.2}` 对它们给的是字面 "inf"/"NaN"，
    // 拼上后缀就成了 "inf%" 这么一条谁也用不上的候选。$EXACT 本身恒有限
    // （`render_calc` 用 `is_finite` 过过一道），所以非有限只可能来自 scale 写太大。
    let scaled = v * scale;
    if !scaled.is_finite() {
        return Err(CmdbarError::runtime(
            "pct",
            format!("换算结果超出可表示范围（{v} × {scale}）"),
        ));
    }
    let mut s = format!("{:.*}", decimals, scaled);
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    Ok(format!("{s}{suffix}"))
}

/// `scale` 只查「是不是数字」，不设上下限：它不按值分配内存（宽度由 `decimals` 定），
/// 而负数倍率、0 倍率都有正当用法。真正的越界只剩「乘出来不是有限数」，那在上面拦。
fn parse_scale(func: &str, v: &str) -> Result<f64> {
    v.trim()
        .parse::<f64>()
        .map_err(|_| CmdbarError::runtime(func, format!("scale 需要数字，得到 {v:?}")))
}

#[cfg(test)]
#[path = "quick_tests.rs"]
mod tests;
