//! 通用小表（设计 §2.4）：带 `columns:` 表头的 Rime 风格 TSV，按列名读写。短语、Shadow 这类
//! 用户逐条操作、规模小的表用它：内存持有解析结果（读零 IO），写时按组补丁、整文件提交
//! （tmp + fsync + 复核指纹 + rename），未改动的行逐字节不变；手改文件经 `sync` 按指纹重新解析。
//! 不走日志 + delta 引擎。

use super::format::ParseError;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
pub struct TableRow {
    /// 该行在「按 `\n` 切开的全部行」里的下标（含头部）。
    pub line: usize,
    /// 按表头列序，已反转义；短行补空串。
    pub fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableDoc {
    pub columns: Vec<String>,
    pub kind: Option<String>,
    pub escape: bool,
    pub rows: Vec<TableRow>,
}

impl TableDoc {
    fn empty(columns: &[&str], kind: &str) -> TableDoc {
        TableDoc {
            columns: columns.iter().map(|c| c.to_string()).collect(),
            kind: Some(kind.to_string()),
            escape: true,
            rows: Vec::new(),
        }
    }

    /// 按列名取一行的字段（文件里没有该列 ⇒ 空串）。
    pub fn get<'a>(&self, row: &'a TableRow, name: &str) -> &'a str {
        col(self, name)
            .and_then(|i| row.fields.get(i))
            .map_or("", String::as_str)
    }
}

pub fn col(doc: &TableDoc, name: &str) -> Option<usize> {
    doc.columns.iter().position(|c| c == name)
}

#[derive(Deserialize, Default)]
struct RawHead {
    #[serde(default)]
    columns: Option<Vec<String>>,
    #[serde(default)]
    wind: Option<RawWind>,
}

#[derive(Deserialize, Default)]
struct RawWind {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    escape: Option<bool>,
}

/// 读：按 escape 设置反转义。命令栏文本例外——它的反斜杠归命令栏 lexer 管，`\n` / `\t` 是字面
/// 序列，这里原样保留（`escape_text_field` 对它只把真实换行 / 制表符写成 `\n` / `\t`，lexer 读到
/// 两者等价；而 `unescape_text_field` 对它不是逆函数，用了就会在第一次保存后改写用户的文本）。
pub(crate) fn unescape(escape: bool, s: &str) -> String {
    if escape && !wind_wdict::is_cmdbar_text(s) {
        wind_wdict::unescape_text_field(s)
    } else {
        s.to_string()
    }
}

fn escape_field(escape: bool, s: &str) -> String {
    if escape {
        wind_wdict::escape_text_field(s)
    } else {
        s.to_string()
    }
}

pub fn parse_table(bytes: &[u8], _path: &Path) -> Result<TableDoc, ParseError> {
    let s = std::str::from_utf8(bytes).map_err(|_| ParseError::NotUtf8)?;
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let (body_offset, comment_cutoff) = wind_dict::rime_body_layout(s).ok_or(ParseError::NoBody)?;
    let yaml = s[..body_offset]
        .trim_start_matches("---")
        .trim_end()
        .trim_end_matches("...");
    let raw: RawHead = if yaml.trim().is_empty() {
        RawHead::default()
    } else {
        serde_yaml::from_str(yaml).map_err(|e| ParseError::BadHeader(e.to_string()))?
    };
    let columns = raw
        .columns
        .ok_or_else(|| ParseError::BadHeader("缺 columns".into()))?;
    let wind = raw.wind.unwrap_or_default();
    let escape = wind.escape.unwrap_or(false);
    let mut rows = Vec::new();
    let mut offset = 0usize;
    for (idx, raw_line) in s.split('\n').enumerate() {
        let start = offset;
        offset += raw_line.len() + 1;
        if start < body_offset {
            continue;
        }
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.trim().is_empty() {
            continue;
        }
        // `#` 开头：`# no comment` 之前一律是注释；之后只有含 tab 的才是数据行（这类表至少两列，
        // 数据行必有 tab）——用户手写的 `# 说明` 不该被读成一行数据。
        let in_comment_zone = comment_cutoff.is_none_or(|c| start <= c);
        if line.starts_with('#') && (in_comment_zone || !line.contains('\t')) {
            continue;
        }
        let mut fields: Vec<String> = line
            .split('\t')
            .take(columns.len())
            .map(|f| unescape(escape, f))
            .collect();
        fields.resize(columns.len(), String::new());
        rows.push(TableRow { line: idx, fields });
    }
    Ok(TableDoc {
        columns,
        kind: wind.kind,
        escape,
        rows,
    })
}

/// 整文件渲染：按种类的说明行、LF、无 BOM、`escape: true`，正文以 `# no comment` 开头（首列可以 `#` 开头）。
pub fn render_table(columns: &[&str], kind: &str, rows: &[Vec<String>]) -> String {
    let mut s = super::banner::for_kind(kind)
        .map(|b| format!("{b}\n"))
        .unwrap_or_default();
    s.push_str(&format!(
        "---\ncolumns: [{}]\nwind: {{kind: {kind}, escape: true}}\n...\n# no comment\n",
        columns.join(", ")
    ));
    for r in rows {
        let line: Vec<String> = r.iter().map(|f| escape_field(true, f)).collect();
        s.push_str(&line.join("\t"));
        s.push('\n');
    }
    s
}

/// 一组编辑：分组键的取值（按 `group_cols` 序）与该组的新行全集（按 `columns` 列序）。
pub type GroupEdit = (Vec<String>, Vec<Vec<String>>);

/// 按组补丁：原文件里该组的行全部删掉，新行插在原第一行的位置（没有该组 ⇒ 追加到末尾）；其余
/// 字节不变。新行按**文件的**列序、文件的 escape 设置输出（文件里没有的列丢弃）；`original` 为空
/// ⇒ 先渲染表头；直接以 `---` 开头 ⇒ 补上说明行。
pub fn patch_groups(
    original: &[u8],
    path: &Path,
    group_cols: &[&str],
    edits: &[GroupEdit],
    columns: &[&str],
    kind: &str,
) -> Result<Vec<u8>, ParseError> {
    let fresh: std::borrow::Cow<[u8]>;
    let original: &[u8] = if original.is_empty() {
        fresh = render_table(columns, kind, &[]).into_bytes().into();
        &fresh
    } else {
        fresh = super::banner::prepend_if_bare(original, super::banner::for_kind(kind));
        &fresh
    };
    let doc = parse_table(original, path)?;
    let s = std::str::from_utf8(original).map_err(|_| ParseError::NotUtf8)?;
    let (bom, body) = match s.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", s),
    };
    let (_, comment_cutoff) = wind_dict::rime_body_layout(body).ok_or(ParseError::NoBody)?;
    let crlf = body
        .find('\n')
        .is_some_and(|i| i > 0 && body.as_bytes()[i - 1] == b'\r');
    let nl = if crlf { "\r\n" } else { "\n" };
    let gidx: Vec<usize> = group_cols
        .iter()
        .map(|c| col(&doc, c).ok_or_else(|| ParseError::BadHeader(format!("缺列 {c}"))))
        .collect::<Result<_, _>>()?;
    // 新行（按 columns 列序）→ 文件列序的一行文本
    let render_row = |r: &Vec<String>| -> String {
        doc.columns
            .iter()
            .map(|fc| {
                let v = columns
                    .iter()
                    .position(|c| c == fc)
                    .and_then(|i| r.get(i))
                    .map_or("", String::as_str);
                escape_field(doc.escape, v)
            })
            .collect::<Vec<_>>()
            .join("\t")
    };
    let ends_with_nl = body.ends_with('\n');
    let mut lines: Vec<Option<String>> = body.split('\n').map(|l| Some(l.to_string())).collect();
    if ends_with_nl {
        lines.pop();
    }
    let mut appended: Vec<String> = Vec::new();
    for (key, new_rows) in edits {
        let hits: Vec<usize> = doc
            .rows
            .iter()
            .filter(|r| gidx.iter().zip(key).all(|(&i, k)| &r.fields[i] == k))
            .map(|r| r.line)
            .collect();
        let rendered: Vec<String> = new_rows.iter().map(render_row).collect();
        match hits.first() {
            Some(&first) => {
                let cr = if lines[first].as_deref().is_some_and(|l| l.ends_with('\r')) {
                    "\r"
                } else {
                    ""
                };
                for &i in &hits {
                    lines[i] = None;
                }
                if !rendered.is_empty() {
                    let joined: Vec<String> = rendered.iter().map(|l| format!("{l}{cr}")).collect();
                    lines[first] = Some(joined.join("\n"));
                }
            }
            None => appended.extend(rendered),
        }
    }
    let mut out = String::with_capacity(original.len() + appended.len() * 32);
    out.push_str(bom);
    let kept: Vec<&String> = lines.iter().flatten().collect();
    for (i, l) in kept.iter().enumerate() {
        out.push_str(l);
        if i + 1 < kept.len() || ends_with_nl {
            out.push('\n');
        }
    }
    if !appended.is_empty() && !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    if comment_cutoff.is_none() && appended.iter().any(|r| r.starts_with('#')) {
        out.push_str("# no comment");
        out.push_str(nl);
    }
    for r in appended {
        out.push_str(&r);
        out.push_str(nl);
    }
    Ok(out.into_bytes())
}

struct TableState {
    fp: String,
    doc: Arc<TableDoc>,
    isolated: bool,
}

/// 一个小表文件的持有者：内存里是最近一次成功解析的内容，读零 IO。
pub struct TextTable {
    path: PathBuf,
    columns: &'static [&'static str],
    kind: &'static str,
    state: Mutex<TableState>,
}

impl TextTable {
    /// 读文件并解析：不存在 ⇒ 空表；解析失败 ⇒ 空表 + 隔离（不往里写）+ WARN。
    pub fn open(path: PathBuf, columns: &'static [&'static str], kind: &'static str) -> TextTable {
        let t = TextTable {
            path,
            columns,
            kind,
            state: Mutex::new(TableState {
                fp: String::new(),
                doc: Arc::new(TableDoc::empty(columns, kind)),
                isolated: false,
            }),
        };
        t.sync();
        t
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn doc(&self) -> Arc<TableDoc> {
        self.lock().doc.clone()
    }

    pub fn isolated(&self) -> bool {
        self.lock().isolated
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TableState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 按文件现状更新内存（文件锁外的读取；`st` 由调用方持有）。返回是否换了内容。
    fn reload_locked(&self, st: &mut TableState) -> anyhow::Result<bool> {
        let cur = super::fold::read_text(&self.path)?;
        let fp = super::fold::fingerprint(cur.as_ref().map(|(b, _)| b.as_slice()));
        if fp == st.fp {
            return Ok(false);
        }
        st.fp = fp;
        match cur {
            None => {
                st.doc = Arc::new(TableDoc::empty(self.columns, self.kind));
                st.isolated = false;
                Ok(true)
            }
            Some((bytes, _)) => match parse_table(&bytes, &self.path) {
                Ok(doc) => {
                    st.doc = Arc::new(doc);
                    st.isolated = false;
                    Ok(true)
                }
                Err(e) => {
                    tracing::warn!(
                        "{} 隔离（{e:?}）：不往里写，按上次成功解析的内容提供",
                        self.path.display()
                    );
                    st.isolated = true;
                    Ok(false)
                }
            },
        }
    }

    /// 文件变了（指纹不同）就重新解析；隔离时保留上次成功的内容。返回是否换了内容。
    pub fn sync(&self) -> bool {
        let mut st = self.lock();
        match self.reload_locked(&mut st) {
            Ok(changed) => changed,
            Err(e) => {
                tracing::warn!("读不了 {}：{e:#}", self.path.display());
                false
            }
        }
    }

    /// 按组编辑并提交。先按文件现状重新解析（手改优先）；文件隔离 ⇒ Err、不写。
    pub fn edit(&self, edits: &[GroupEdit], group_cols: &[&str]) -> anyhow::Result<()> {
        let mut st = self.lock();
        self.reload_locked(&mut st)?;
        if st.isolated {
            anyhow::bail!("{} 无法解析，请先修复或删除它", self.path.display());
        }
        let original = match super::fold::read_text(&self.path)? {
            Some((b, _)) => b,
            None => Vec::new(),
        };
        let out = patch_groups(
            &original,
            &self.path,
            group_cols,
            edits,
            self.columns,
            self.kind,
        )
        .map_err(|e| anyhow::anyhow!("{} 补丁失败：{e:?}", self.path.display()))?;
        super::fold::commit_text(&self.path, &out, Some(&st.fp))?;
        self.install(&mut st, &out)
    }

    /// 整文件替换（清空 / 还原 / 迁移）：不复核指纹，隔离的文件也照样替换。
    pub fn replace_all(&self, rows: &[Vec<String>]) -> anyhow::Result<()> {
        let mut st = self.lock();
        let out = render_table(self.columns, self.kind, rows);
        super::fold::commit_text(&self.path, out.as_bytes(), None)?;
        self.install(&mut st, out.as_bytes())
    }

    fn install(&self, st: &mut TableState, bytes: &[u8]) -> anyhow::Result<()> {
        let doc = parse_table(bytes, &self.path)
            .map_err(|e| anyhow::anyhow!("写出的内容解析失败：{e:?}"))?;
        st.fp = super::fold::fingerprint(Some(bytes));
        st.doc = Arc::new(doc);
        st.isolated = false;
        Ok(())
    }
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
