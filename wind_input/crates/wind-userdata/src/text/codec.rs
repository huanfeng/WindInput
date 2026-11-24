//! 学到的数据的表格文本（设计 §2.4）：temp / draft / freq / assoc 四个文本目标的列规格、解码、
//! 渲染与按键补丁。底层是 [`super::table`] 的 Rime 风格 TSV（`columns:` 表头 + `wind:` 扩展）。
//!
//! 坏行（§2.5）：码 / 上文 / 词为空、数字列不是数字 ⇒ 跳过、计数，补丁时原样保留。

use super::format::{ParseError, is_single_han, normalize_key};
use super::table::{TableDoc, parse_table};
use crate::snapshot::SnapshotEntry;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    /// 编码：按方案规范化；有音节边界就写空格（任何方案），读时空格还原边界。
    Code,
    /// 上文（联想历史）：原样，存进条目的 `code`，边界恒 0。
    Context,
    Text,
    Weight,
    Count,
    CreatedAt,
    LastUsed,
}

#[derive(Debug)]
pub struct TableSpec {
    pub kind: &'static str,
    pub columns: &'static [(&'static str, Field)],
}

pub static TEMP: TableSpec = TableSpec {
    kind: "temp",
    columns: &[
        ("code", Field::Code),
        ("text", Field::Text),
        ("weight", Field::Weight),
        ("count", Field::Count),
        ("created_at", Field::CreatedAt),
    ],
};

pub static DRAFT: TableSpec = TableSpec {
    kind: "draft",
    columns: &[
        ("code", Field::Code),
        ("text", Field::Text),
        ("last_used", Field::LastUsed),
    ],
};

pub static FREQ: TableSpec = TableSpec {
    kind: "freq",
    columns: &[
        ("code", Field::Code),
        ("text", Field::Text),
        ("count", Field::Count),
        ("last_used", Field::LastUsed),
    ],
};

pub static ASSOC: TableSpec = TableSpec {
    kind: "assoc",
    columns: &[
        ("context", Field::Context),
        ("text", Field::Text),
        ("count", Field::Count),
        ("last_used", Field::LastUsed),
    ],
};

pub struct Decoded {
    /// 按 (code, text) 升序、去重（同键多行取文件里第一行）；`order` = 行号。
    pub entries: Vec<SnapshotEntry>,
    pub bad_lines: usize,
}

fn header(spec: &TableSpec, syllable: bool) -> String {
    let cols: Vec<&str> = spec.columns.iter().map(|(n, _)| *n).collect();
    let mut h = super::banner::for_kind(spec.kind)
        .map(|b| format!("{b}\n"))
        .unwrap_or_default();
    h.push_str(&format!(
        "---\ncolumns: [{}]\nwind:\n  kind: {}\n",
        cols.join(", "),
        spec.kind
    ));
    if syllable {
        h.push_str("  code_spacing: syllable\n");
    }
    // 码表方案的码、联想的上文都可能以 `#` 开头：正文一开头就关掉注释
    h.push_str("  escape: true\n...\n# no comment\n");
    h
}

/// 一行（`fields` 按文件列序）→ 条目；坏行 ⇒ None。
fn decode_row(
    fields: &[String],
    idx: &[Option<usize>],
    spec: &TableSpec,
    lowercase: bool,
    syllable: bool,
) -> Option<SnapshotEntry> {
    let mut e = SnapshotEntry {
        code: String::new(),
        text: String::new(),
        weight: 0,
        boundary: 0,
        count: 0,
        created_at: 0,
        last_used: 0,
        flags: 0,
        order: 0,
    };
    let get = |i: usize| -> &str {
        idx[i]
            .and_then(|c| fields.get(c))
            .map_or("", |s| s.as_str())
    };
    let mut raw_code: Option<&str> = None;
    for (i, (_, f)) in spec.columns.iter().enumerate() {
        let v = get(i);
        let num = |v: &str| -> Option<i64> {
            let v = v.trim();
            if v.is_empty() {
                Some(0)
            } else {
                v.parse::<i64>().ok()
            }
        };
        match f {
            Field::Code => raw_code = Some(v),
            Field::Context => {
                if v.is_empty() {
                    return None;
                }
                e.code = v.to_string();
            }
            Field::Text => e.text = v.to_string(),
            Field::Weight => e.weight = i32::try_from(num(v)?).ok()?,
            Field::Count => e.count = u32::try_from(num(v)?).ok()?,
            Field::CreatedAt => e.created_at = num(v)?,
            Field::LastUsed => e.last_used = num(v)?,
        }
    }
    if e.text.is_empty() {
        return None;
    }
    if let Some(code) = raw_code {
        if code.trim().is_empty() {
            return None;
        }
        let (flat, boundary, text) = normalize_key(code, &e.text, lowercase);
        e.code = flat;
        e.text = text;
        // 空格即边界，任何方案都无损往返；单个汉字补单音节只对拼音族（空格载体表达不了 0b1）
        e.boundary = if syllable && boundary == 0 && is_single_han(&e.text) {
            0b1
        } else {
            boundary
        };
    }
    Some(e)
}

/// 规格各列在文件里的下标（文件缺该列 ⇒ None，按空值读）。
fn column_index(doc: &TableDoc, spec: &TableSpec) -> Vec<Option<usize>> {
    spec.columns
        .iter()
        .map(|(n, _)| doc.columns.iter().position(|c| c == n))
        .collect()
}

pub fn decode(
    bytes: &[u8],
    path: &Path,
    spec: &TableSpec,
    lowercase: bool,
    syllable: bool,
) -> Result<Decoded, ParseError> {
    let doc = parse_table(bytes, path)?;
    let idx = column_index(&doc, spec);
    let mut bad_lines = 0;
    let mut entries = Vec::with_capacity(doc.rows.len());
    for r in &doc.rows {
        match decode_row(&r.fields, &idx, spec, lowercase, syllable) {
            Some(mut e) => {
                e.order = r.line as u32;
                entries.push(e);
            }
            None => bad_lines += 1,
        }
    }
    // 稳定排序：同键保留文件里靠前的那一行
    entries.sort_by(|a, b| {
        (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str()))
    });
    entries.dedup_by(|a, b| a.code == b.code && a.text == b.text);
    Ok(Decoded { entries, bad_lines })
}

fn field_value(f: Field, e: &SnapshotEntry) -> String {
    match f {
        Field::Code => wind_wdict::join_code_by_boundary(&e.code, e.boundary),
        Field::Context => e.code.clone(),
        Field::Text => e.text.clone(),
        Field::Weight => e.weight.to_string(),
        Field::Count => e.count.to_string(),
        Field::CreatedAt => e.created_at.to_string(),
        Field::LastUsed => e.last_used.to_string(),
    }
}

/// 按规格列序的一行（未转义）。
pub fn encode_row(spec: &TableSpec, e: &SnapshotEntry) -> Vec<String> {
    spec.columns
        .iter()
        .map(|(_, f)| field_value(*f, e))
        .collect()
}

fn escape(escape: bool, s: &str) -> String {
    if escape {
        wind_wdict::escape_text_field(s)
    } else {
        s.to_string()
    }
}

/// 整文件渲染：说明行、LF、无 BOM、`escape: true`。
pub fn render(spec: &TableSpec, syllable: bool, entries: &[SnapshotEntry]) -> String {
    let mut s = header(spec, syllable);
    for e in entries {
        let line: Vec<String> = encode_row(spec, e)
            .iter()
            .map(|f| escape(true, f))
            .collect();
        s.push_str(&line.join("\t"));
        s.push('\n');
    }
    s
}

/// 按键补丁：`Some(e)` ⇒ 该键第一行换成新值、其余同键行删除，文件里没有则追加末尾；`None` ⇒
/// 同键行全删。坏行、注释、空行与未涉及的行逐字节不变；新行按**文件**的列序与 escape 设置输出。
/// 空文件 ⇒ 先渲染表头；直接以 `---` 开头 ⇒ 补上说明行。单趟线性。
pub fn patch(
    bytes: &[u8],
    path: &Path,
    spec: &TableSpec,
    lowercase: bool,
    syllable: bool,
    edits: &BTreeMap<(String, String), Option<SnapshotEntry>>,
) -> Result<Vec<u8>, ParseError> {
    patch_decoded(bytes, path, spec, lowercase, syllable, edits).map(|(b, _)| b)
}

/// 同 [`patch`]，并顺带给出补丁结果的解码（与对结果再 [`decode`] 一遍逐项相同，含行号与坏行数），
/// 省掉折回时的第二遍全文解析。
pub fn patch_decoded(
    bytes: &[u8],
    path: &Path,
    spec: &TableSpec,
    lowercase: bool,
    syllable: bool,
    edits: &BTreeMap<(String, String), Option<SnapshotEntry>>,
) -> Result<(Vec<u8>, Decoded), ParseError> {
    let fresh: std::borrow::Cow<[u8]>;
    let bytes: &[u8] = if bytes.is_empty() {
        fresh = header(spec, syllable).into_bytes().into();
        &fresh
    } else {
        fresh = super::banner::prepend_if_bare(bytes, super::banner::for_kind(spec.kind));
        &fresh
    };
    let doc = parse_table(bytes, path)?;
    let s = std::str::from_utf8(bytes).map_err(|_| ParseError::NotUtf8)?;
    let (bom, body) = match s.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", s),
    };
    let crlf = body
        .find('\n')
        .is_some_and(|i| i > 0 && body.as_bytes()[i - 1] == b'\r');
    let nl = if crlf { "\r\n" } else { "\n" };
    let idx = column_index(&doc, spec);
    // 新行：按文件列序的已转义字段
    let render_fields = |e: &SnapshotEntry| -> Vec<String> {
        doc.columns
            .iter()
            .map(|fc| {
                let v = spec
                    .columns
                    .iter()
                    .find(|(n, _)| n == fc)
                    .map(|(_, f)| field_value(*f, e))
                    .unwrap_or_default();
                escape(doc.escape, &v)
            })
            .collect()
    };
    // 新行读回来是什么（与 parse_table + decode_row 同一规则）
    let reread = |fields: &[String]| -> Option<SnapshotEntry> {
        let un: Vec<String> = fields
            .iter()
            .map(|f| super::table::unescape(doc.escape, f))
            .collect();
        decode_row(&un, &idx, spec, lowercase, syllable)
    };
    let ends_with_nl = body.ends_with('\n');
    let mut lines: Vec<Option<String>> = body.split('\n').map(|l| Some(l.to_string())).collect();
    if ends_with_nl {
        lines.pop();
    }
    // 每行读出的条目（None = 非数据行或坏行）
    let mut line_entry: Vec<Option<SnapshotEntry>> = vec![None; lines.len()];
    let mut bad_lines = 0usize;
    for r in &doc.rows {
        match decode_row(&r.fields, &idx, spec, lowercase, syllable) {
            Some(e) => line_entry[r.line] = Some(e),
            None => bad_lines += 1,
        }
    }
    let (_, comment_cutoff) = wind_dict::rime_body_layout(body).ok_or(ParseError::NoBody)?;
    // `# no comment` 所在行号（之前的行都在注释区）
    let cutoff_line = comment_cutoff.map(|c| body[..c].matches('\n').count());
    let mut hits: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, e) in line_entry.iter().enumerate() {
        if let Some(e) = e
            && edits.contains_key(&(e.code.clone(), e.text.clone()))
        {
            hits.entry((e.code.clone(), e.text.clone()))
                .or_default()
                .push(i);
        }
    }
    let mut appended: Vec<(String, Option<SnapshotEntry>)> = Vec::new();
    for (key, new) in edits {
        match hits.get(key) {
            Some(ls) => {
                let first = ls[0];
                let cr = if lines[first].as_deref().is_some_and(|l| l.ends_with('\r')) {
                    "\r"
                } else {
                    ""
                };
                for &i in ls {
                    lines[i] = None;
                    line_entry[i] = None;
                }
                if let Some(e) = new {
                    let f = render_fields(e);
                    let text = f.join("\t");
                    // 换上的行落在注释区里且以 # 开头 ⇒ 读回来是注释
                    let comment = text.starts_with('#') && cutoff_line.is_none_or(|c| first <= c);
                    line_entry[first] = if comment { None } else { reread(&f) };
                    lines[first] = Some(format!("{text}{cr}"));
                }
            }
            None => {
                if let Some(e) = new {
                    let f = render_fields(e);
                    let entry = reread(&f);
                    appended.push((f.join("\t"), entry));
                }
            }
        }
    }
    let mut out = String::with_capacity(bytes.len() + appended.len() * 32);
    out.push_str(bom);
    let mut entries: Vec<SnapshotEntry> = Vec::new();
    let mut n = 0usize;
    let kept_total = lines.iter().flatten().count();
    for (l, e) in lines.iter().zip(line_entry) {
        let Some(l) = l else { continue };
        out.push_str(l);
        if n + 1 < kept_total || ends_with_nl {
            out.push('\n');
        }
        if let Some(mut e) = e {
            e.order = n as u32;
            entries.push(e);
        }
        n += 1;
    }
    if !appended.is_empty() && !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    if comment_cutoff.is_none() && appended.iter().any(|(r, _)| r.starts_with('#')) {
        out.push_str("# no comment");
        out.push_str(nl);
        n += 1;
    }
    for (r, e) in appended {
        out.push_str(&r);
        out.push_str(nl);
        if let Some(mut e) = e {
            e.order = n as u32;
            entries.push(e);
        }
        n += 1;
    }
    entries.sort_by(|a, b| {
        (a.code.as_str(), a.text.as_str()).cmp(&(b.code.as_str(), b.text.as_str()))
    });
    entries.dedup_by(|a, b| a.code == b.code && a.text == b.text);
    Ok((out.into_bytes(), Decoded { entries, bad_lines }))
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
