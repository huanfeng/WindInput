//! 调色板解析：把 colors 段解析为「名称 → RGBA」具体色值。
//!
//! 与 Go 版本 `wind_input/pkg/theme/palette.go` 对齐。
//! 处理 `${var}` 引用（递归 + 环检测）、{light,dark} 变体、#RRGGBB[AA] 十六进制。

use std::collections::{HashMap, HashSet};
use toml::Value;

/// 颜色 [R, G, B, A]，与 UI 缓冲约定一致。
pub type Rgba = [u8; 4];

/// 解析 `#RRGGBB` / `#RRGGBBAA`（`#` 可省），以及 `#RGB`（`#` 必带，扩写为 `#RRGGBB`）。
///
/// 3 位简写必须带 `#`：本函数的调用方不止主题，还有语言栏配置（`[ui.langbar]` 各色，用户手填）
/// 与模板内联色（`$[…]{}`，名字与色值共用一个位置）。3 位也可裸写的话，`bad`、`fed`、`ace`
/// 这类英文单词会被当成颜色。6 / 8 位的裸写保持原样——那是既有行为，手写配置里见过。
pub fn parse_hex(s: &str) -> Option<Rgba> {
    let s = s.trim();
    if let Some(d) = s.strip_prefix('#')
        && d.len() == 3
        && d.is_ascii()
    {
        // 单个十六进制位 v 扩写为 vv，即 v × 17。
        let n = |i: usize| u8::from_str_radix(&d[i..=i], 16).ok().map(|v| v * 17);
        return Some([n(0)?, n(1)?, n(2)?, 255]);
    }
    let s = s.trim_start_matches('#');
    // 逐字符先验十六进制位：下面按两位一组交给 `from_str_radix`，它认前导 `+`
    // （`#+F+F+F` 会被当成白色）。只收紧非法输入，合法写法不受影响。
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // 先挡非 ASCII：下面按字节切片，多字节字符会让切点落在字中间而 panic。
    if !s.is_ascii() {
        return None;
    }
    let h = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    match s.len() {
        6 => Some([h(0)?, h(2)?, h(4)?, 255]),
        8 => Some([h(0)?, h(2)?, h(4)?, h(6)?]),
        _ => None,
    }
}

/// WCAG 2 相对亮度对比度（1.0 – 21.0），只看 RGB、忽略 alpha。
pub fn contrast_ratio(a: Rgba, b: Rgba) -> f64 {
    fn lum(c: Rgba) -> f64 {
        let f = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
    }
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `a` 与 `b` 的对比度达到 `min`（含恰好等于）。
pub fn meets_contrast(a: Rgba, b: Rgba, min: f64) -> bool {
    contrast_ratio(a, b) >= min
}

/// `top` 按自身 alpha 叠在不透明的 `under` 上，结果不透明。
pub fn composite_over(top: Rgba, under: Rgba) -> Rgba {
    let a = f64::from(top[3]) / 255.0;
    let mix = |i: usize| (f64::from(top[i]) * a + f64::from(under[i]) * (1.0 - a)).round() as u8;
    [mix(0), mix(1), mix(2), 255]
}

/// 解析整个调色板：colors 段（Mapping）→ {名称: Rgba}。
/// is_dark 选择 {light,dark} 变体。derive 等非颜色项被忽略。
pub fn resolve_palette(colors: Option<&Value>, is_dark: bool) -> HashMap<String, Rgba> {
    let mut out = HashMap::new();
    let map = match colors.and_then(|c| c.as_table()) {
        Some(m) => m,
        None => return out,
    };
    let mut visiting = HashSet::new();
    let names: Vec<String> = map.keys().cloned().collect();
    for name in names {
        resolve_name(&name, map, is_dark, &mut out, &mut visiting);
    }
    out
}

/// 按名解析（递归 + 记忆 + 环检测）。
fn resolve_name(
    name: &str,
    map: &toml::Table,
    is_dark: bool,
    out: &mut HashMap<String, Rgba>,
    visiting: &mut HashSet<String>,
) -> Option<Rgba> {
    if let Some(c) = out.get(name) {
        return Some(*c);
    }
    if visiting.contains(name) {
        return None; // 环
    }
    visiting.insert(name.to_string());
    let v = map.get(name)?;
    let color = resolve_value(v, map, is_dark, out, visiting);
    visiting.remove(name);
    if let Some(c) = color {
        out.insert(name.to_string(), c);
    }
    color
}

/// 解析一个颜色值（字符串 hex / `${var}` / {light,dark}）。
fn resolve_value(
    v: &Value,
    map: &toml::Table,
    is_dark: bool,
    out: &mut HashMap<String, Rgba>,
    visiting: &mut HashSet<String>,
) -> Option<Rgba> {
    match v {
        Value::String(s) => resolve_str(s, map, is_dark, out, visiting),
        Value::Table(m) => {
            // {light: .., dark: ..} 变体
            let key = if is_dark { "dark" } else { "light" };
            if let Some(inner) = m.get(key) {
                resolve_value(inner, map, is_dark, out, visiting)
            } else {
                None // derive 等非颜色映射
            }
        }
        _ => None,
    }
}

fn resolve_str(
    s: &str,
    map: &toml::Table,
    is_dark: bool,
    out: &mut HashMap<String, Rgba>,
    visiting: &mut HashSet<String>,
) -> Option<Rgba> {
    let s = s.trim();
    if let Some(var) = s.strip_prefix("${").and_then(|x| x.strip_suffix('}')) {
        resolve_name(var, map, is_dark, out, visiting)
    } else {
        parse_hex(s)
    }
}

/// 把 views 中的颜色 token（"${name}" 或 "#hex" 或 {light,dark}）按已解析调色板转为 Rgba。
pub fn color_token(v: &Value, palette: &HashMap<String, Rgba>, is_dark: bool) -> Option<Rgba> {
    match v {
        Value::String(s) => {
            let s = s.trim();
            if let Some(var) = s.strip_prefix("${").and_then(|x| x.strip_suffix('}')) {
                palette.get(var).copied()
            } else {
                parse_hex(s)
            }
        }
        Value::Table(m) => {
            let key = if is_dark { "dark" } else { "light" };
            m.get(key)
                .and_then(|inner| color_token(inner, palette, is_dark))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "palette_tests.rs"]
mod tests;
