//! 渲染与按行补丁写回（设计 §3.5 文本目标第 1 步）。
//!
//! 补丁只改被操作的行，其余字节（BOM、换行风格、注释、空行、行尾空白、自定义顺序）原样保留；
//! 文件直接以 `---` 开头时最前面补一行说明（[`super::banner::prepend_if_bare`]）。
//! 操作按给出顺序依次作用到「文件行 + 本轮新增行」的内存模型上，最后统一输出：
//! - `Upsert`：已有同键行 ⇒ 第一行的权重取 `max(旧, 新)`（无权重列则不动），拼音文件里该行码
//!   无空格（边界未知）而操作带边界时把码改写成带空格的；没有 ⇒ 追加到末尾；
//! - `Insert`：没有同键行才追加，权重列留空（用户分库：没给权重就按方案默认）；有 ⇒ 空操作；
//! - `Delete`：删所有同键行（含本轮新增的）；
//! - `SetWeight`：只改第一行；早于文件修改时间的视为被手改覆盖，跳过；没有该行 ⇒ 空操作。
//!
//! 匹配键只经 [`normalize_key`]。

use super::format::{ParseError, normalize_key, parse};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum PatchOp {
    Upsert {
        code: String,
        text: String,
        boundary: u64,
        weight: i32,
    },
    Insert {
        code: String,
        text: String,
        boundary: u64,
    },
    Delete {
        code: String,
        text: String,
    },
    SetWeight {
        code: String,
        text: String,
        weight: i32,
        ts_secs: i64,
    },
}

fn header(banner: &str, name: &str, syllable_codes: bool) -> String {
    let mut h = format!(
        "{banner}\n---\nname: {name}\ncolumns: [code, text, weight]\nwind:\n  kind: words\n"
    );
    if syllable_codes {
        h.push_str("  code_spacing: syllable\n");
    }
    // 码表方案的码可能以 `#` 开头，不能被当成注释，所以正文一开头就关掉注释
    h.push_str("  escape: true\n...\n# no comment\n");
    h
}

/// 整文件渲染（迁移 / 清空 / 导入 / 新建）：说明行、LF、无 BOM、`columns: [code, text, weight]`。
pub fn render(name: &str, syllable_codes: bool, rows: &[(String, String, u64, i32)]) -> String {
    render_with(super::banner::USER_DICT, name, syllable_codes, rows)
}

/// 同 [`render`]，说明行由调用方给（用户分库）。
pub fn render_with(
    banner: &str,
    name: &str,
    syllable_codes: bool,
    rows: &[(String, String, u64, i32)],
) -> String {
    let mut s = header(banner, name, syllable_codes);
    for (code, text, boundary, weight) in rows {
        s.push_str(&wind_wdict::join_code_by_boundary(code, *boundary));
        s.push('\t');
        s.push_str(&wind_wdict::escape_text_field(text));
        s.push('\t');
        s.push_str(&weight.to_string());
        s.push('\n');
    }
    s
}

/// 本轮新增的一行（结构化保存，输出时按文件列序拼）。
struct Added {
    key: (String, String),
    code: String,
    boundary: u64,
    /// None：权重列留空（`Insert`）。
    weight: Option<i32>,
    alive: bool,
}

/// 改写一行的第 `col` 列；`\r` 结尾的行保持 `\r`。
fn with_col(line: &str, col: usize, value: &str) -> String {
    let (core, cr) = match line.strip_suffix('\r') {
        Some(c) => (c, "\r"),
        None => (line, ""),
    };
    let mut parts: Vec<&str> = core.split('\t').collect();
    if parts.len() <= col {
        parts.resize(col + 1, "");
    }
    parts[col] = value;
    format!("{}{cr}", parts.join("\t"))
}

fn with_weight(line: &str, weight_col: usize, w: i32) -> String {
    with_col(line, weight_col, &w.to_string())
}

fn weight_in(line: &str, weight_col: Option<usize>) -> i32 {
    weight_col
        .and_then(|i| line.trim_end_matches('\r').split('\t').nth(i))
        .and_then(|w| w.trim().parse().ok())
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
pub fn patch(
    original: &[u8],
    path: &Path,
    lowercase: bool,
    ops: &[PatchOp],
    file_mtime_secs: i64,
    new_file_name: &str,
    syllable_codes: bool,
) -> Result<Vec<u8>, ParseError> {
    patch_with_banner(
        original,
        path,
        lowercase,
        ops,
        file_mtime_secs,
        new_file_name,
        syllable_codes,
        Some(super::banner::USER_DICT),
    )
}

/// 同 [`patch`]；`banner` 为 None 时不给直接以 `---` 开头的文件补说明行（用户分库：用户自己
/// 放进来的文件不动）。
#[allow(clippy::too_many_arguments)]
pub fn patch_with_banner(
    original: &[u8],
    path: &Path,
    lowercase: bool,
    ops: &[PatchOp],
    file_mtime_secs: i64,
    new_file_name: &str,
    syllable_codes: bool,
    banner: Option<&str>,
) -> Result<Vec<u8>, ParseError> {
    let fresh: std::borrow::Cow<[u8]>;
    let original: &[u8] = if original.is_empty() {
        let b = banner.unwrap_or(super::banner::USER_DICT);
        fresh = header(b, new_file_name, syllable_codes).into_bytes().into();
        &fresh
    } else {
        fresh = super::banner::prepend_if_bare(original, banner);
        &fresh
    };
    let dict = parse(original, path, lowercase)?;
    let s = std::str::from_utf8(original).map_err(|_| ParseError::NotUtf8)?;
    let (bom, body) = match s.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", s),
    };
    let layout = wind_dict::rime_layout(body, path).ok_or(ParseError::NoBody)?;
    let ends_with_nl = body.ends_with('\n');
    let mut lines: Vec<Option<String>> = body.split('\n').map(|l| Some(l.to_string())).collect();
    if ends_with_nl {
        lines.pop();
    }
    let mut by_key: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for e in &dict.entries {
        by_key
            .entry((e.code.clone(), e.text.clone()))
            .or_default()
            .push(e.line);
    }
    let mut added: Vec<Added> = Vec::new();
    // 同键在一轮内至多一条活的新增行（Upsert 命中就合并），按键索引即可，免得大批追加成平方级
    let mut added_at: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for op in ops {
        let (code, text) = match op {
            PatchOp::Upsert { code, text, .. }
            | PatchOp::Insert { code, text, .. }
            | PatchOp::Delete { code, text }
            | PatchOp::SetWeight { code, text, .. } => (code, text),
        };
        let (flat, _, norm_text) = normalize_key(code, text, lowercase);
        let key = (flat.clone(), norm_text.clone());
        let first_line = by_key
            .get(&key)
            .and_then(|v| v.iter().copied().find(|i| lines[*i].is_some()));
        let first_added = added_at.get(&key).copied().filter(|&i| added[i].alive);
        match op {
            PatchOp::Upsert {
                boundary, weight, ..
            } => {
                if let Some(i) = first_line {
                    let mut line = lines[i].take().unwrap();
                    if let Some(wc) = layout.weight_col
                        && *weight > weight_in(&line, Some(wc))
                    {
                        line = with_weight(&line, wc, *weight);
                    }
                    let unknown_boundary = line
                        .trim_end_matches('\r')
                        .split('\t')
                        .nth(layout.code_col)
                        .is_some_and(|c| !c.trim().contains(' '));
                    if dict.header.syllable_codes && *boundary != 0 && unknown_boundary {
                        let spaced = wind_wdict::join_code_by_boundary(&flat, *boundary);
                        line = with_col(&line, layout.code_col, &spaced);
                    }
                    lines[i] = Some(line);
                } else if let Some(a) = first_added {
                    added[a].weight = Some(added[a].weight.map_or(*weight, |w| w.max(*weight)));
                    if added[a].boundary == 0 {
                        added[a].boundary = *boundary;
                    }
                } else {
                    added_at.insert(key.clone(), added.len());
                    added.push(Added {
                        key,
                        code: flat,
                        boundary: *boundary,
                        weight: Some(*weight),
                        alive: true,
                    });
                }
            }
            PatchOp::Insert { boundary, .. } => {
                if first_line.is_none() && first_added.is_none() {
                    added_at.insert(key.clone(), added.len());
                    added.push(Added {
                        key,
                        code: flat,
                        boundary: *boundary,
                        weight: None,
                        alive: true,
                    });
                }
            }
            PatchOp::Delete { .. } => {
                if let Some(v) = by_key.get(&key) {
                    for &i in v {
                        lines[i] = None;
                    }
                }
                if let Some(&i) = added_at.get(&key) {
                    added[i].alive = false;
                }
            }
            PatchOp::SetWeight {
                weight, ts_secs, ..
            } => {
                if *ts_secs < file_mtime_secs {
                    continue;
                }
                if let Some(i) = first_line {
                    if let Some(wc) = layout.weight_col {
                        let line = lines[i].as_ref().unwrap();
                        lines[i] = Some(with_weight(line, wc, *weight));
                    }
                } else if let Some(a) = first_added {
                    added[a].weight = Some(*weight);
                }
            }
        }
    }
    let nl = if dict.crlf { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(original.len() + added.len() * 32);
    out.push_str(bom);
    let kept: Vec<&String> = lines.iter().flatten().collect();
    for (i, l) in kept.iter().enumerate() {
        out.push_str(l);
        if i + 1 < kept.len() || ends_with_nl {
            out.push('\n');
        }
    }
    let alive: Vec<&Added> = added.iter().filter(|a| a.alive).collect();
    if !alive.is_empty() && !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    let ncols = layout
        .text_col
        .max(layout.code_col)
        .max(layout.weight_col.unwrap_or(0))
        + 1;
    let mut rows = Vec::with_capacity(alive.len());
    for a in alive {
        let mut cols = vec![String::new(); ncols];
        cols[layout.text_col] = if dict.header.escape {
            wind_wdict::escape_text_field(&a.key.1)
        } else {
            a.key.1.clone()
        };
        cols[layout.code_col] = wind_wdict::join_code_by_boundary(&a.code, a.boundary);
        if let (Some(wc), Some(w)) = (layout.weight_col, a.weight) {
            cols[wc] = w.to_string();
        }
        // 权重留空且在末列：不留行尾的空列
        let keep = layout.text_col.max(layout.code_col) + 1;
        while cols.len() > keep && cols.last().is_some_and(String::is_empty) {
            cols.pop();
        }
        rows.push(cols.join("\t"));
    }
    // 没关注释的旧文件：追加的行以 `#` 开头时先补一行 `# no comment`，否则它会被读成注释
    if layout.comment_cutoff.is_none() && rows.iter().any(|r| r.starts_with('#')) {
        out.push_str("# no comment");
        out.push_str(nl);
    }
    for r in rows {
        out.push_str(&r);
        out.push_str(nl);
    }
    Ok(out.into_bytes())
}

#[cfg(test)]
#[path = "patch_tests.rs"]
mod tests;
