//! §3.2 文本处理函数（对照 Go funcs/text.go）。全部 `pure=true` 且 `deterministic=true`。
//!
//! `t2s/s2t/pinyin` 当前为占位（原样返回），待接 OpenCC / 拼音转换表（与 Go 同为 stub）。

use super::func_specs;
use super::util::{parse_arg_int, resolve_1based, rune_len};
use crate::context::EvalContext;
use crate::error::{CmdbarError, Result};
use crate::registry::FuncSpec;

pub fn specs() -> Vec<FuncSpec> {
    func_specs! {
        "len"    : Text (1, 1) det => fn_len,     "字符串字符数 (按 rune)", "len(last())";
        "upper"  : Text (1, 1) det => fn_upper,   "转大写", "upper(\"abc\")";
        "lower"  : Text (1, 1) det => fn_lower,   "转小写", "lower(\"ABC\")";
        "trim"   : Text (1, 2) det => fn_trim,    "去首尾空白; trim(s, chars) 去指定字符", "trim(last())";
        "sub"    : Text (2, 3) det => fn_sub,     "切片, 索引 1 起, 支持负数; sub(s, start, end) 双闭区间", "sub(code, 2)";
        "replace": Text (3, 3) det => fn_replace, "字面替换", "replace(last(), \"a\", \"b\")";
        "regex"  : Text (3, 3) det => fn_regex,   "正则替换 (Rust regex 语法)", "regex(last(), \"\\\\d+\", \"N\")";
        "split"  : Text (3, 3) det => fn_split,   "按 sep 拆分, 取第 n 段 (1 起, 支持负数)", "split(last(), \",\", 1)";
        "concat" : Text (0, -1) det => fn_concat, "字符串拼接", "concat(last(), \" \", clip())";
        "reverse": Text (1, 1) det => fn_reverse, "反转字符串 (按 rune)", "reverse(\"abc\")";
        "t2s"    : Text (1, 1) det => fn_passthrough, "(stub) 繁→简; 暂占位原样返回", "t2s(last())";
        "s2t"    : Text (1, 1) det => fn_passthrough, "(stub) 简→繁; 暂占位原样返回", "s2t(last())";
        "pinyin" : Text (1, 1) det => fn_passthrough, "(stub) 汉字转拼音; 暂占位原样返回", "pinyin(last())";
        "url"    : Text (1, 1) det => fn_url,      "URL 编码 (component)", "url(last())";
        "html"   : Text (1, 1) det => fn_html,     "HTML 实体编码", "html(last())";
        "json"   : Text (1, 1) det => fn_json,     "JSON 字符串字面量化 (含外层引号)", "json(last())";
        "base64" : Text (1, 1) det => fn_base64,   "Base64 编码", "base64(last())";
        "default": Text (2, 2) det => fn_default,  "s 为空时返回 fallback", "default(last(), \"(empty)\")";
    }
}

fn fn_len(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    Ok(rune_len(&args[0]).to_string())
}
fn fn_upper(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    Ok(args[0].to_uppercase())
}
fn fn_lower(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    Ok(args[0].to_lowercase())
}

fn fn_trim(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    if args.len() == 1 {
        Ok(args[0].trim().to_string())
    } else {
        let chars: Vec<char> = args[1].chars().collect();
        Ok(args[0].trim_matches(|c| chars.contains(&c)).to_string())
    }
}

fn fn_sub(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    let rs: Vec<char> = args[0].chars().collect();
    let n = rs.len();
    let start = parse_arg_int("sub", &args[1])?;
    let s = match resolve_1based(start, n) {
        Some(s) => s,
        None => return Ok(String::new()),
    };
    if args.len() == 2 {
        return Ok(rs[s..].iter().collect());
    }
    let end = parse_arg_int("sub", &args[2])?;
    let e = match resolve_1based(end, n) {
        Some(e) => e + 1, // 双闭 → 排他上界
        None => return Ok(String::new()),
    };
    if e <= s {
        return Ok(String::new());
    }
    Ok(rs[s..e].iter().collect())
}

fn fn_replace(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    Ok(args[0].replace(&args[1], &args[2]))
}

fn fn_regex(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    let re =
        regex::Regex::new(&args[1]).map_err(|e| CmdbarError::runtime("regex", e.to_string()))?;
    Ok(re.replace_all(&args[0], args[2].as_str()).into_owned())
}

fn fn_split(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    let n = parse_arg_int("split", &args[2])?;
    let parts: Vec<&str> = args[0].split(args[1].as_str()).collect();
    match resolve_1based(n, parts.len()) {
        Some(idx) => Ok(parts[idx].to_string()),
        None => Ok(String::new()),
    }
}

fn fn_concat(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    Ok(args.concat())
}

fn fn_reverse(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    Ok(args[0].chars().rev().collect())
}

fn fn_passthrough(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    Ok(args[0].clone())
}

fn fn_url(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    Ok(query_escape(&args[0]))
}

fn fn_html(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    let mut out = String::with_capacity(args[0].len());
    for ch in args[0].chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    Ok(out)
}

fn fn_json(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    Ok(json_quote(&args[0]))
}

fn fn_base64(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(args[0].as_bytes()))
}

fn fn_default(_: &dyn EvalContext, args: &[String]) -> Result<String> {
    if args[0].is_empty() {
        Ok(args[1].clone())
    } else {
        Ok(args[0].clone())
    }
}

/// URL query 编码（对齐 Go url.QueryEscape：空格→`+`，保留 `A-Za-z0-9-_.~`，余下 `%XX`）。
pub(crate) fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(hex_upper(b >> 4));
                out.push(hex_upper(b & 0xf));
            }
        }
    }
    out
}

fn hex_upper(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

/// 把字符串编码为 JSON 字符串字面量（含外层引号）。对齐 Go json.Marshal：
/// 转义 `"` `\\` 控制字符，并把 `<` `>` `&` 转 `\u00XX`（HTML 安全）。
fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
