//! 学习文件（core 的 TSV 表）按原始字段读写，并在文件头带同步令牌。
//! 字段保持转义后的原文，不反转义；头部除令牌行外原样保留。

use super::format::FormatError;
use super::{Key, LearnKind, NormKey, norm_key};
use std::collections::BTreeMap;

const NO_COMMENT: &str = "# no comment";

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LearnedFile {
    /// 数据行之前的全部原文（banner、`---` 头、`...`、`# no comment`）；文件不存在时为空。
    pub head: String,
    pub columns: Vec<String>,
    /// 键 → (原样键, count, 按 `field_cols` 顺序的字段)。
    pub rows: BTreeMap<NormKey, (Key, u64, Vec<String>)>,
    /// 跳过的坏行数（列数不足、count 非整数）。
    pub bad_lines: usize,
}

fn perr<T>(msg: impl Into<String>) -> Result<T, FormatError> {
    Err(FormatError::Parse(msg.into()))
}

fn default_columns(kind: LearnKind) -> Vec<String> {
    let cols: &[&str] = match kind {
        LearnKind::Temp => &["code", "text", "weight", "count", "created_at"],
        LearnKind::Freq => &["code", "text", "count", "last_used"],
        LearnKind::Assoc => &["context", "text", "count", "last_used"],
        LearnKind::Completion => &["kind", "text", "count", "last_used"],
    };
    cols.iter().map(|s| s.to_string()).collect()
}

fn trim_eol(line: &str) -> &str {
    let l = line.strip_suffix('\n').unwrap_or(line);
    l.strip_suffix('\r').unwrap_or(l)
}

/// 头部结束位置（字节偏移，含 `# no comment` 行或 `...` 行）与是否有 `# no comment`。
fn head_end(s: &str) -> Result<(usize, bool), FormatError> {
    let mut off = 0;
    let mut in_yaml = false;
    let mut dots_end = None;
    for line in s.split_inclusive('\n') {
        let t = trim_eol(line);
        off += line.len();
        match dots_end {
            None => {
                if !in_yaml {
                    if t == "---" {
                        in_yaml = true;
                    } else if !t.starts_with('#') && !t.trim().is_empty() {
                        return perr("缺少 YAML 头");
                    }
                } else if t == "..." {
                    dots_end = Some(off);
                }
            }
            Some(_) => {
                if t.trim().is_empty() {
                    continue;
                }
                return Ok(if t == NO_COMMENT {
                    (off, true)
                } else {
                    (dots_end.unwrap(), false)
                });
            }
        }
    }
    match dots_end {
        Some(d) => Ok((d, false)),
        None => perr("缺少 YAML 头"),
    }
}

fn parse_columns(head: &str) -> Result<Vec<String>, FormatError> {
    for line in head.lines() {
        if let Some(rest) = line.strip_prefix("columns:") {
            let inner = rest
                .trim()
                .strip_prefix('[')
                .and_then(|r| r.strip_suffix(']'))
                .map_or_else(|| perr::<&str>("columns 不是流序列"), Ok)?;
            return Ok(inner
                .split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect());
        }
    }
    perr("缺 columns")
}

pub fn parse(kind: LearnKind, bytes: Option<&[u8]>) -> Result<LearnedFile, FormatError> {
    let bytes = bytes.unwrap_or(&[]);
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(LearnedFile {
            columns: default_columns(kind),
            ..Default::default()
        });
    }
    let s = std::str::from_utf8(bytes).map_err(|_| FormatError::Parse("不是 UTF-8".into()))?;
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let (end, has_marker) = head_end(s)?;
    let head = &s[..end];
    let columns = parse_columns(head)?;
    let pos = |name: &str| columns.iter().position(|c| c == name);
    let [k0, k1] = kind.key_cols();
    let (Some(i0), Some(i1)) = (pos(k0), pos(k1)) else {
        return perr(format!("缺键列 {k0}/{k1}"));
    };
    let Some(ic) = pos("count") else {
        return perr("缺 count 列");
    };
    let fidx: Vec<Option<usize>> = kind.field_cols().iter().map(|n| pos(n)).collect();

    let mut f = LearnedFile {
        head: head.to_string(),
        columns: columns.clone(),
        ..Default::default()
    };
    for raw in s[end..].split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('#') && (!has_marker || !line.contains('\t')) {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let need = [i0, i1, ic].into_iter().max().unwrap();
        if cols.len() <= need {
            f.bad_lines += 1;
            continue;
        }
        let c = cols[ic].trim();
        let count = if c.is_empty() {
            Some(0)
        } else {
            c.parse::<i64>().ok().and_then(|n| u32::try_from(n).ok())
        };
        let Some(count) = count.map(u64::from) else {
            f.bad_lines += 1;
            continue;
        };
        let key = Key(cols[i0].to_string(), cols[i1].to_string());
        let fields = fidx
            .iter()
            .map(|i| {
                i.and_then(|i| cols.get(i))
                    .copied()
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        f.rows
            .entry(norm_key(kind, &key))
            .or_insert((key, count, fields));
    }
    Ok(f)
}

fn new_head(kind: LearnKind, token: Option<&str>, syllable: bool) -> String {
    let cols = default_columns(kind).join(", ");
    let sync = token.map(|t| format!("sync: {t}"));
    if kind == LearnKind::Completion {
        let mut m = format!("kind: {}, escape: true", kind.as_str());
        if let Some(s) = sync {
            m.push_str(&format!(", {s}"));
        }
        return format!("---\ncolumns: [{cols}]\nwind: {{{m}}}\n...\n{NO_COMMENT}\n");
    }
    let mut h = format!("---\ncolumns: [{cols}]\nwind:\n  kind: {}\n", kind.as_str());
    if syllable {
        h.push_str("  code_spacing: syllable\n");
    }
    h.push_str("  escape: true\n");
    if let Some(s) = sync {
        h.push_str(&format!("  {s}\n"));
    }
    h.push_str(&format!("...\n{NO_COMMENT}\n"));
    h
}

/// 在已有头里写入/替换令牌。
fn set_token(head: &str, token: &str) -> String {
    let mut out = String::with_capacity(head.len() + 24);
    let lines: Vec<&str> = head.split_inclusive('\n').collect();
    let mut i = 0;
    let mut done = false;
    while i < lines.len() {
        let line = lines[i];
        let t = trim_eol(line);
        let eol = &line[t.len()..];
        if !done && t.starts_with("wind:") {
            let rest = t["wind:".len()..].trim();
            if rest.starts_with('{') {
                out.push_str(&set_flow_token(t, token));
                out.push_str(eol);
                done = true;
                i += 1;
                continue;
            }
            if rest.is_empty() {
                // 块映射：收集缩进行
                let mut j = i + 1;
                while j < lines.len() {
                    let u = trim_eol(lines[j]);
                    if u.starts_with(' ') || u.starts_with('\t') {
                        j += 1;
                    } else {
                        break;
                    }
                }
                let indent: String = lines
                    .get(i + 1)
                    .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').collect())
                    .unwrap_or_default();
                let indent = if indent.is_empty() {
                    "  ".to_string()
                } else {
                    indent
                };
                let mut replaced = false;
                out.push_str(line);
                for l in &lines[i + 1..j] {
                    if trim_eol(l).trim_start().starts_with("sync:") {
                        let e = &l[trim_eol(l).len()..];
                        out.push_str(&format!("{indent}sync: {token}{e}"));
                        replaced = true;
                    } else {
                        out.push_str(l);
                    }
                }
                if !replaced {
                    let nl = if eol.is_empty() { "\n" } else { eol };
                    if j == i + 1 && eol.is_empty() {
                        out.push_str(nl);
                    }
                    out.push_str(&format!("{indent}sync: {token}{nl}"));
                }
                done = true;
                i = j;
                continue;
            }
        }
        out.push_str(line);
        i += 1;
    }
    out
}

fn set_flow_token(line: &str, token: &str) -> String {
    let open = line.find('{').unwrap();
    if let Some(p) = line[open..].find("sync:") {
        let vs = open + p + "sync:".len();
        let ve = line[vs..].find([',', '}']).map_or(line.len(), |x| vs + x);
        format!("{} {token}{}", &line[..vs], &line[ve..])
    } else {
        let close = line.rfind('}').unwrap_or(line.len());
        let sep = if line[open + 1..close].trim().is_empty() {
            ""
        } else {
            ", "
        };
        format!(
            "{}{sep}sync: {token}{}",
            line[..close].trim_end(),
            &line[close..]
        )
    }
}

pub fn render(kind: LearnKind, f: &LearnedFile, token: Option<&str>, syllable: bool) -> Vec<u8> {
    let (head, columns) = if f.head.is_empty() {
        (new_head(kind, token, syllable), default_columns(kind))
    } else {
        let h = match token {
            Some(t) => set_token(&f.head, t),
            None => f.head.clone(),
        };
        (h, f.columns.clone())
    };
    let [k0, k1] = kind.key_cols();
    let fcols = kind.field_cols();
    let mut s = head;
    for (key, count, fields) in f.rows.values() {
        let line: Vec<String> = columns
            .iter()
            .map(|c| {
                if c == k0 {
                    key.0.clone()
                } else if c == k1 {
                    key.1.clone()
                } else if c == "count" {
                    count.to_string()
                } else {
                    fcols
                        .iter()
                        .position(|n| n == c)
                        .and_then(|i| fields.get(i).cloned())
                        .unwrap_or_default()
                }
            })
            .collect();
        s.push_str(&line.join("\t"));
        s.push('\n');
    }
    s.into_bytes()
}

/// 读头部 `wind:` 映射里的 sync 值（块/流映射均可）。
pub fn token_of(bytes: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(bytes).ok()?;
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let (end, _) = head_end(s).ok()?;
    let mut in_wind = false;
    for line in s[..end].lines() {
        if let Some(rest) = line.strip_prefix("wind:") {
            let rest = rest.trim();
            if let Some(inner) = rest.strip_prefix('{') {
                let inner = inner.split('}').next().unwrap_or("");
                return inner.split(',').find_map(|kv| {
                    kv.trim()
                        .strip_prefix("sync:")
                        .map(|v| v.trim().to_string())
                });
            }
            in_wind = rest.is_empty();
        } else if in_wind {
            if line.starts_with(' ') || line.starts_with('\t') {
                if let Some(v) = line.trim_start().strip_prefix("sync:") {
                    return Some(v.trim().to_string());
                }
            } else {
                in_wind = false;
            }
        }
    }
    None
}
