//! 用户词文本文件（设计 §2.2 / §2.3）：Rime `.dict.yaml` + 头部 `wind:` 扩展。列规格与注释规则
//! 复用 `wind_dict::rime_layout`（规则只此一份）；匹配键只经 [`normalize_key`]。

use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindHeader {
    pub kind: Option<String>,
    /// `code_spacing: syllable`：码以空格分音节；无空格码 = 边界未知（0），文本为单个汉字时为单音节。
    pub syllable_codes: bool,
    /// `escape: true`：text 列按 `escape_text_field` 规则转义。
    pub escape: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextEntry {
    /// 该行在「按 `\n` 切开的全部行」里的下标（含头部）。补丁按它定位。
    pub line: usize,
    /// 规范化后的扁平码。
    pub code: String,
    pub text: String,
    pub boundary: u64,
    pub weight: i32,
}

#[derive(Clone, Debug)]
pub struct TextDict {
    pub header: WindHeader,
    pub entries: Vec<TextEntry>,
    pub bom: bool,
    pub crlf: bool,
    /// 解析不了而跳过的数据行（列不够、码 / 词空、权重不是数字；§2.5）。空行与注释不算。
    pub bad_lines: usize,
}

#[derive(Debug)]
pub enum ParseError {
    NotUtf8,
    BadHeader(String),
    NoBody,
}

#[derive(Deserialize, Default)]
struct RawHeader {
    #[serde(default)]
    wind: Option<RawWind>,
}

#[derive(Deserialize, Default)]
struct RawWind {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    code_spacing: Option<String>,
    #[serde(default)]
    escape: Option<bool>,
}

/// 匹配键规范化（§2.3，唯一实现）：码去空格得扁平码与音节边界（无空格 ⇒ 0），按方案小写；
/// 文本只做输入归一（行尾）。
pub fn normalize_key(code: &str, text: &str, lowercase: bool) -> (String, u64, String) {
    let (mut flat, boundary) = wind_wdict::split_spaced_code(code.trim());
    if lowercase {
        flat = flat.to_lowercase();
    }
    let text = wind_utils::text::normalize_input(text).into_owned();
    (flat, boundary, text)
}

/// 文本恰为一个 CJK 统一表意文字（含扩展区与兼容区）。
pub(crate) fn is_single_han(text: &str) -> bool {
    let mut it = text.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => matches!(
            c as u32,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F
        ),
        _ => false,
    }
}

pub fn parse(bytes: &[u8], path: &Path, lowercase: bool) -> Result<TextDict, ParseError> {
    let s = std::str::from_utf8(bytes).map_err(|_| ParseError::NotUtf8)?;
    let bom = s.starts_with('\u{feff}');
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let crlf = s
        .find('\n')
        .is_some_and(|i| i > 0 && s.as_bytes()[i - 1] == b'\r');
    let layout = wind_dict::rime_layout(s, path).ok_or(ParseError::NoBody)?;
    let head_text = &s[..layout.body_offset];
    let yaml = head_text
        .trim_start_matches("---")
        .trim_end()
        .trim_end_matches("...");
    let raw: RawHeader = if yaml.trim().is_empty() {
        RawHeader::default()
    } else {
        serde_yaml::from_str(yaml).map_err(|e| ParseError::BadHeader(e.to_string()))?
    };
    let wind = raw.wind.unwrap_or_default();
    let header = WindHeader {
        kind: wind.kind,
        syllable_codes: wind.code_spacing.as_deref() == Some("syllable"),
        escape: wind.escape.unwrap_or(false),
    };
    let mut entries = Vec::new();
    let mut bad_lines = 0usize;
    let mut offset = 0usize;
    for (idx, raw_line) in s.split('\n').enumerate() {
        let start = offset;
        offset += raw_line.len() + 1;
        if start < layout.body_offset {
            continue;
        }
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.trim().is_empty() {
            continue;
        }
        let is_comment_zone = layout.comment_cutoff.is_none_or(|c| start <= c);
        if line.starts_with('#') && is_comment_zone {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        let (Some(text), Some(code)) = (parts.get(layout.text_col), parts.get(layout.code_col))
        else {
            bad_lines += 1;
            continue;
        };
        if text.is_empty() || code.trim().is_empty() {
            bad_lines += 1;
            continue;
        }
        let weight = match layout
            .weight_col
            .and_then(|i| parts.get(i))
            .map(|w| w.trim())
        {
            None | Some("") => 0,
            Some(w) => match w.parse::<i32>() {
                Ok(w) => w,
                Err(_) => {
                    bad_lines += 1;
                    continue;
                }
            },
        };
        let text = if header.escape {
            wind_wdict::unescape_text_field(text)
        } else {
            (*text).to_string()
        };
        let (code, boundary, text) = normalize_key(code, &text, lowercase);
        let boundary = if !header.syllable_codes {
            0
        } else if boundary == 0 && is_single_han(&text) {
            // 空格载体表达不了单音节；一个汉字只对应一个音节
            0b1
        } else {
            boundary
        };
        entries.push(TextEntry {
            line: idx,
            code,
            text,
            boundary,
            weight,
        });
    }
    Ok(TextDict {
        header,
        entries,
        bom,
        crlf,
        bad_lines,
    })
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
