//! 系统词库查看（设置端词库窗「系统词库」两级）的引擎侧：库的生效先后、文件条数、头部信息、
//! 只读词条分页与搜索。
//!
//! # 读文件，不碰引擎
//!
//! 全部只读词库**文本**（`.dict.yaml`），不加载 wdat、不持引擎锁：停用的库也要能看，而为看一眼
//! 去建 wdat 正是「停用的库只列不计」（`dict_stats`）要避免的事。调用方在 RPC 线程上调，
//! 不进按键链路。
//!
//! # 60 万条的主库怎么翻页
//!
//! 每个文件按（大小, mtime）缓存一份 [`FileIndex`]：条数、列序，外加**每 [`STRIDE`] 条记一个
//! 行首字节偏移**的稀疏检查点。建索引要整读一遍文件（与计数是同一遍，条数本来就要数）；之后
//! 翻页只 seek 到最近的检查点、往后读不到 `STRIDE + limit` 行，与文件大小无关。检查点每张表
//! 几千个 `u64`（拼音全套约 20 KB），随条数缓存常驻，文件一变即按新戳重建。
//!
//! 搜索必须整扫（编码前缀 / 词文本子串都没有现成索引）：一次扫完把命中行的（表, 行首偏移）
//! 存进**单槽**缓存，同一查询翻页直接按偏移读行。换查询、换库或回到不搜索的浏览即替换 / 清空
//! 这一槽，内存上限是「命中行数 × 16 字节」。

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use serde::Serialize;
use wind_config::schema::{DictSpec, Schema};

use super::{DICT_YAML_SUFFIX, EngineManager};

/// 检查点间隔（条）。翻页最多多读这么多行。
const STRIDE: u64 = 256;

/// 一页最多取多少条（`sysDict.entries` 的 `limit` 上限）。
pub const ENTRIES_PAGE_MAX: usize = 500;

/// 头部前置注释最多取多少行（说明 / 作者 / 来源一类；长篇更新日志截断）。
const NOTE_LINES_MAX: usize = 40;

/// 词条搜索的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryMatch {
    /// 编码前缀（忽略大小写与音节间空格：`nihao` 命中 `ni hao`）。
    Code,
    /// 词文本子串。
    Text,
}

/// 一张表：主文件或 `import_tables` 子表。
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DictTableInfo {
    /// 主文件取文件名（去 `.dict.yaml`），子表取 `import_tables` 里写的名字。
    pub name: String,
    pub path: PathBuf,
    /// 文件不在为 `None`。
    pub count: Option<u64>,
}

/// `sysDict.header` 的引擎侧：头部 YAML 字段 + 文件信息 + 子表清单。
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DictHeader {
    pub name: Option<String>,
    pub version: Option<String>,
    pub sort: Option<String>,
    pub columns: Vec<String>,
    pub import_tables: Vec<String>,
    /// `---` 之前的说明注释（去掉 `# `、去掉 rime 模板的两行套话）；没有为 `None`。
    pub notes: Option<String>,
    pub path: PathBuf,
    pub mtime_ms: Option<u64>,
    /// 口径同 [`EngineManager::dict_file_entries`]：本文件 + 各子表，子表缺失则 `None`。
    pub count: Option<u64>,
    /// 子表（不含主文件本身），按展开顺序。
    pub tables: Vec<DictTableInfo>,
}

/// 一行词条（只读展示）。`code` 原样保留音节间空格。
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DictEntryRow {
    pub code: String,
    pub text: String,
    pub weight: i32,
    /// 所在表名（同 [`DictTableInfo::name`]）。
    pub table: String,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DictEntriesPage {
    pub total: u64,
    pub rows: Vec<DictEntryRow>,
}

type Stamp = (u64, Option<SystemTime>);

/// 一个词库文件的索引，见模块文档。偏移都相对**规范化后**的文本（去 BOM；孤立 `\r` 改 `\n`）。
#[derive(Debug)]
struct FileIndex {
    stamp: Stamp,
    count: u64,
    text_col: usize,
    code_col: usize,
    weight_col: Option<usize>,
    /// `# no comment` 指令位置：其后的 `#` 行是数据。
    cutoff: Option<usize>,
    /// 第 `k * STRIDE` 条的行首偏移。
    checkpoints: Vec<u64>,
    /// 文件开头的 BOM 字节数（规范化偏移 + 它 = 文件偏移）。
    bom: u64,
    /// 规范化改动过正文（孤立 `\r`）：偏移对不上文件，翻页要整读再规范化。
    reflowed: bool,
}

static FILE_INDEX: OnceLock<Mutex<HashMap<PathBuf, Arc<FileIndex>>>> = OnceLock::new();

/// 搜索单槽：键 =（各表路径与戳, 查询, 方式），值 = 命中行（表下标, 行首偏移）。
type SearchKey = (Vec<(PathBuf, Stamp)>, String, EntryMatch);
type SearchHits = Arc<Vec<(u32, u64)>>;
static SEARCH_SLOT: OnceLock<Mutex<Option<(SearchKey, SearchHits)>>> = OnceLock::new();

fn stamp_of(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    Some((m.len(), m.modified().ok()))
}

/// 是不是词条行：口径同加载器（`wind_dict::rime_body_layout`）——跳过空行与 `#` 注释，
/// `# no comment` 指令之后的 `#` 行是数据。`start` 是行首在规范化文本里的偏移。
fn is_entry_line(line: &str, start: usize, cutoff: Option<usize>) -> bool {
    let t = line.trim_end();
    !(t.is_empty() || (t.starts_with('#') && cutoff.is_none_or(|c| start <= c)))
}

/// 正文里的词条行：`(行首偏移, 去掉换行的行)`。
fn entry_lines(
    text: &str,
    body: usize,
    cutoff: Option<usize>,
) -> impl Iterator<Item = (usize, &str)> {
    let mut off = body;
    text[body..].split_inclusive('\n').filter_map(move |raw| {
        let start = off;
        off += raw.len();
        is_entry_line(raw, start, cutoff).then(|| (start, strip_eol(raw)))
    })
}

fn strip_eol(raw: &str) -> &str {
    let l = raw.strip_suffix('\n').unwrap_or(raw);
    l.strip_suffix('\r').unwrap_or(l)
}

/// 数 rime 词库正文的词条行（[`is_entry_line`] 口径）。
#[cfg(test)]
pub(super) fn count_dict_entries(raw: &str) -> u64 {
    let text = wind_utils::text::normalize_input(raw);
    match wind_dict::rime_body_layout(&text) {
        Some((body, cutoff)) => entry_lines(&text, body, cutoff).count() as u64,
        None => 0,
    }
}

impl FileIndex {
    fn build(path: &Path, stamp: Stamp) -> Option<FileIndex> {
        let raw = std::fs::read_to_string(path).ok()?;
        let bom = if raw.starts_with('\u{feff}') { 3 } else { 0 };
        let text = wind_utils::text::normalize_input(&raw);
        let reflowed = matches!(text, Cow::Owned(_));
        let mut idx = FileIndex {
            stamp,
            count: 0,
            text_col: 0,
            code_col: 1,
            weight_col: Some(2),
            cutoff: None,
            checkpoints: Vec::new(),
            bom,
            reflowed,
        };
        let Some((body, cutoff)) = wind_dict::rime_body_layout(&text) else {
            return Some(idx);
        };
        idx.cutoff = cutoff;
        // 列序同加载器（`columns:` 声明优先，否则探测）；声明残缺时加载器整库跳过，这里退回
        // rime 默认列序照样能看。
        if let Some(l) = wind_dict::rime_layout(&text, path) {
            (idx.text_col, idx.code_col, idx.weight_col) = (l.text_col, l.code_col, l.weight_col);
        }
        for (start, _) in entry_lines(&text, body, cutoff) {
            if idx.count.is_multiple_of(STRIDE) {
                idx.checkpoints.push(start as u64);
            }
            idx.count += 1;
        }
        Some(idx)
    }

    fn parse_row(&self, line: &str, table: &str) -> DictEntryRow {
        let parts: Vec<&str> = line.split('\t').collect();
        let code = parts.get(self.code_col).map_or("", |s| s.trim_end());
        let text = parts.get(self.text_col).copied().unwrap_or("");
        let weight = self
            .weight_col
            .and_then(|i| parts.get(i))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        DictEntryRow {
            code: code.to_string(),
            text: wind_store::wdict::unescape_text_field(text),
            weight,
            table: table.to_string(),
        }
    }

    /// 只看本行是否命中，不建整行（搜索整扫的热路径）。
    fn row_matches(&self, line: &str, query: &str, mode: EntryMatch) -> bool {
        match mode {
            EntryMatch::Text => {
                // 先拿整行粗筛：不带转义时整行不含就一定不中（多数行到此为止）。
                let escaped = line.contains('\\');
                if !escaped && !line.contains(query) {
                    return false;
                }
                line.split('\t').nth(self.text_col).is_some_and(|t| {
                    if escaped {
                        wind_store::wdict::unescape_text_field(t).contains(query)
                    } else {
                        t.contains(query)
                    }
                })
            }
            EntryMatch::Code => {
                let Some(code) = line.split('\t').nth(self.code_col) else {
                    return false;
                };
                let mut q = query.chars();
                for c in code.chars().filter(|c| *c != ' ') {
                    match q.next() {
                        None => return true,
                        Some(x) if x == c.to_ascii_lowercase() => {}
                        Some(_) => return false,
                    }
                }
                q.next().is_none()
            }
        }
    }
}

/// 取（或重建）一个文件的索引；文件不在 / 读不出为 `None`。不持锁读文件。
fn file_index(path: &Path) -> Option<Arc<FileIndex>> {
    let stamp = stamp_of(path)?;
    let cache = FILE_INDEX.get_or_init(Default::default);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .filter(|i| i.stamp == stamp)
    {
        return Some(hit.clone());
    }
    let idx = Arc::new(FileIndex::build(path, stamp)?);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_path_buf(), idx.clone());
    Some(idx)
}

/// 一张表的可读源：大多数文件直接按偏移 seek；规范化改过正文的整读进内存。
enum TableReader {
    File(std::io::BufReader<std::fs::File>),
    Mem(std::io::Cursor<Vec<u8>>),
}

impl TableReader {
    fn open(path: &Path, idx: &FileIndex) -> Option<TableReader> {
        if idx.reflowed {
            let raw = std::fs::read_to_string(path).ok()?;
            let text = wind_utils::text::normalize_input(&raw).into_owned();
            return Some(TableReader::Mem(std::io::Cursor::new(text.into_bytes())));
        }
        let f = std::fs::File::open(path).ok()?;
        Some(TableReader::File(std::io::BufReader::new(f)))
    }

    /// 定位到规范化偏移 `pos`。
    fn seek(&mut self, pos: u64, idx: &FileIndex) -> std::io::Result<()> {
        match self {
            TableReader::File(r) => r.seek(SeekFrom::Start(pos + idx.bom)).map(|_| ()),
            TableReader::Mem(c) => {
                c.set_position(pos);
                Ok(())
            }
        }
    }

    fn reader(&mut self) -> &mut dyn BufRead {
        match self {
            TableReader::File(r) => r,
            TableReader::Mem(c) => c,
        }
    }
}

/// 从规范化偏移 `pos`（某个词条行的行首）起，跳过 `skip` 条后取 `take` 条，交给 `f`。
fn read_rows(
    rd: &mut TableReader,
    idx: &FileIndex,
    mut pos: u64,
    mut skip: u64,
    take: usize,
    f: &mut dyn FnMut(&str),
) -> std::io::Result<()> {
    rd.seek(pos, idx)?;
    let mut buf = Vec::new();
    let mut got = 0usize;
    while got < take {
        buf.clear();
        let n = rd.reader().read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        let start = pos as usize;
        pos += n as u64;
        let line = String::from_utf8_lossy(&buf);
        if !is_entry_line(&line, start, idx.cutoff) {
            continue;
        }
        if skip > 0 {
            skip -= 1;
            continue;
        }
        f(strip_eol(&line));
        got += 1;
    }
    Ok(())
}

/// 读单行（偏移处的那一行）。
fn read_line_at(rd: &mut TableReader, idx: &FileIndex, pos: u64) -> Option<String> {
    let mut out = None;
    read_rows(rd, idx, pos, 0, 1, &mut |l| out = Some(l.to_string())).ok()?;
    out
}

/// 头部 YAML 段（`---` 与 `...` 之间）与 `---` 之前的注释行。读到 `...` 即停。
fn read_head_parts(path: &Path) -> (String, Vec<String>) {
    let Ok(f) = std::fs::File::open(path) else {
        return (String::new(), Vec::new());
    };
    let mut yaml = String::new();
    let mut notes = Vec::new();
    let mut in_yaml = false;
    for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
        let Ok(line) = line else { break };
        let line = if i == 0 {
            line.trim_start_matches('\u{feff}').to_string()
        } else {
            line
        };
        let t = line.trim_end();
        if t == "..." || yaml.len() > super::DICT_HEAD_SCAN_LIMIT {
            break;
        }
        if in_yaml {
            yaml.push_str(t);
            yaml.push('\n');
        } else if t == "---" {
            in_yaml = true;
        } else if let Some(c) = t.strip_prefix('#') {
            notes.push(c.strip_prefix(' ').unwrap_or(c).to_string());
        } else if !t.is_empty() {
            // 没有 `---` 的文件：正文前的非注释行就当 YAML 段起点（rime 允许省略 `---`）
            in_yaml = true;
            yaml.push_str(t);
            yaml.push('\n');
        }
    }
    (yaml, notes)
}

/// 去掉 rime 模板的两行套话与首尾空行；剩下的说明截到 [`NOTE_LINES_MAX`] 行。
fn tidy_notes(lines: Vec<String>) -> Option<String> {
    let mut v: Vec<String> = lines
        .into_iter()
        .filter(|l| {
            let t = l.trim();
            !t.eq_ignore_ascii_case("Rime dictionary")
                && !t.to_ascii_lowercase().starts_with("encoding:")
        })
        .collect();
    while v.first().is_some_and(|l| l.trim().is_empty()) {
        v.remove(0);
    }
    while v.last().is_some_and(|l| l.trim().is_empty()) {
        v.pop();
    }
    v.truncate(NOTE_LINES_MAX);
    (!v.is_empty()).then(|| v.join("\n"))
}

fn yaml_scalar(v: Option<&serde_yaml::Value>) -> Option<String> {
    match v? {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn yaml_strs(v: Option<&serde_yaml::Value>) -> Vec<String> {
    v.and_then(|v| v.as_sequence())
        .map(|s| s.iter().filter_map(|x| yaml_scalar(Some(x))).collect())
        .unwrap_or_default()
}

fn mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// 主文件的表名：文件名去 `.dict.yaml`。
fn main_table_name(path: &Path) -> String {
    let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    file.strip_suffix(DICT_YAML_SUFFIX)
        .unwrap_or(file)
        .to_string()
}

impl EngineManager {
    /// 方案的系统词库按**实际叠层先后**排好：主库最先（同 `split_codetable_dicts`：首个
    /// `default`，没有则首个可用库），其余按 `base_order` 稳定排序（同值保持方案文件块序，
    /// 即 `CachedDict::Layered` 同权时靠前的层在先）。不含用户分库与没有 `path` 的项。
    /// 第二个值 = 是不是主库。
    pub fn ordered_system_dicts(schema: &Schema) -> Vec<(&DictSpec, bool)> {
        let Some((main, extras)) = Self::split_codetable_dicts(schema) else {
            return Vec::new();
        };
        let mut rest: Vec<&DictSpec> = extras
            .into_iter()
            .filter(|d| !d.id.starts_with(wind_store::user_dicts::ID_PREFIX))
            .collect();
        rest.sort_by_key(|d| d.base_order);
        std::iter::once((main, true))
            .chain(rest.into_iter().map(|d| (d, false)))
            .collect()
    }

    /// 方案里 `rel` 解析到的主文件 + `import_tables` 递归展开（前序，主文件在首）。子表相对
    /// **所在那张表的目录**、不走分层（同 `rime_source_paths`），`seen` 防环。第三项 = 文件在不在。
    fn dict_tables(&self, rel: &str) -> Option<Vec<(String, PathBuf, bool)>> {
        let schemas_dir = self.data_dir.as_deref()?.join("schemas");
        let main = Self::resolve_dict_file(rel, &schemas_dir);
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // 栈：后入先出，子表逆序压入以保持声明顺序
        let mut stack = vec![(main_table_name(&main), main)];
        while let Some((name, path)) = stack.pop() {
            if !seen.insert(path.clone()) {
                continue;
            }
            let exists = path.is_file();
            if exists {
                let (_, imports) = Self::read_dict_head(&path);
                let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
                for imp in imports.into_iter().rev() {
                    let sub = dir.join(format!("{imp}{DICT_YAML_SUFFIX}"));
                    stack.push((imp, sub));
                }
            }
            out.push((name, path, exists));
        }
        Some(out)
    }

    /// 一张词库的条目数（设置端「系统词库」表用）：按方案里的 `path` 经覆盖解析定位
    /// `.dict.yaml`，按加载器的口径数正文行。停用的库也数得出——只读文本，不加载、不建 wdat。
    /// 按（大小, mtime）缓存，文件没变不重扫。
    ///
    /// 写了 `import_tables` 的库按「本文件 + 各子表（递归）」求和；任一子表缺失、主文件不在
    /// （含只投放了 wdat 的库）或读不出都是 `None`——少算一截比没有数更误导。
    pub fn dict_file_entries(&self, rel: &str) -> Option<u64> {
        let tables = self.dict_tables(rel)?;
        let mut n = 0;
        for (_, path, exists) in tables {
            if !exists {
                return None;
            }
            n += file_index(&path)?.count;
        }
        Some(n)
    }

    /// `sysDict.header` 的引擎侧：`rel` 的头部字段、文件信息与子表清单。主文件不在 → Err。
    pub fn dict_header(&self, rel: &str) -> anyhow::Result<DictHeader> {
        let tables = self
            .dict_tables(rel)
            .ok_or_else(|| anyhow::anyhow!("没有数据目录"))?;
        let (_, main, exists) = &tables[0];
        if !exists {
            anyhow::bail!("词库文件不存在: {}", main.display());
        }
        let (yaml, notes) = read_head_parts(main);
        let head = serde_yaml::from_str::<serde_yaml::Value>(&yaml).unwrap_or_default();
        let count = self.dict_file_entries(rel);
        Ok(DictHeader {
            name: yaml_scalar(head.get("name")),
            version: yaml_scalar(head.get("version")),
            sort: yaml_scalar(head.get("sort")),
            columns: yaml_strs(head.get("columns")),
            // 清单以逐行解析为准（与展开同一份，剥了行内注释）；YAML 段解析失败也拿得到
            import_tables: Self::read_dict_head(main).1,
            notes: tidy_notes(notes),
            path: main.clone(),
            mtime_ms: mtime_ms(main),
            count,
            tables: tables[1..]
                .iter()
                .map(|(name, path, exists)| DictTableInfo {
                    name: name.clone(),
                    path: path.clone(),
                    count: exists.then(|| file_index(path).map(|i| i.count)).flatten(),
                })
                .collect(),
        })
    }

    /// `sysDict.entries` 的引擎侧：按文件顺序分页（带 `import_tables` 的依次展开各子表），
    /// `query` 非空时按 `mode` 过滤。`limit` 夹到 `1..=ENTRIES_PAGE_MAX`。主文件不在 → Err；
    /// 缺失的子表跳过。
    pub fn dict_entries(
        &self,
        rel: &str,
        query: &str,
        mode: EntryMatch,
        offset: u64,
        limit: usize,
    ) -> anyhow::Result<DictEntriesPage> {
        let tables = self
            .dict_tables(rel)
            .ok_or_else(|| anyhow::anyhow!("没有数据目录"))?;
        if !tables[0].2 {
            anyhow::bail!("词库文件不存在: {}", tables[0].1.display());
        }
        let limit = limit.clamp(1, ENTRIES_PAGE_MAX);
        let idx: Vec<(String, PathBuf, Arc<FileIndex>)> = tables
            .into_iter()
            .filter(|t| t.2)
            .filter_map(|(name, path, _)| {
                let i = file_index(&path)?;
                Some((name, path, i))
            })
            .collect();
        let query = match mode {
            EntryMatch::Code => query.replace(' ', "").to_ascii_lowercase(),
            EntryMatch::Text => query.to_string(),
        };
        let slot = SEARCH_SLOT.get_or_init(Default::default);
        if query.is_empty() {
            // 回到浏览：搜索结果用不上了，释放
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
            return Ok(browse_page(&idx, offset, limit));
        }
        let key: SearchKey = (
            idx.iter().map(|(_, p, i)| (p.clone(), i.stamp)).collect(),
            query.clone(),
            mode,
        );
        let cached = slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, h)| h.clone());
        let hits = match cached {
            Some(h) => h,
            None => {
                let h = Arc::new(scan_hits(&idx, &query, mode));
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, h.clone()));
                h
            }
        };
        let mut rows = Vec::new();
        let mut open: Option<(u32, TableReader)> = None;
        for &(ti, pos) in hits.iter().skip(offset as usize).take(limit) {
            let (name, path, fi) = &idx[ti as usize];
            if open.as_ref().is_none_or(|(t, _)| *t != ti) {
                open = TableReader::open(path, fi).map(|r| (ti, r));
            }
            let Some((_, rd)) = open.as_mut() else {
                continue;
            };
            if let Some(line) = read_line_at(rd, fi, pos) {
                rows.push(fi.parse_row(&line, name));
            }
        }
        Ok(DictEntriesPage {
            total: hits.len() as u64,
            rows,
        })
    }
}

/// 不搜索的一页：全局序号 → 落在哪张表、该表第几条，从最近的检查点读起。
fn browse_page(
    idx: &[(String, PathBuf, Arc<FileIndex>)],
    offset: u64,
    limit: usize,
) -> DictEntriesPage {
    let total = idx.iter().map(|(_, _, i)| i.count).sum();
    let mut rows = Vec::new();
    let mut base = 0u64;
    for (name, path, fi) in idx {
        if rows.len() >= limit {
            break;
        }
        let local = (offset + rows.len() as u64).saturating_sub(base);
        base += fi.count;
        if local >= fi.count {
            continue;
        }
        let k = (local / STRIDE) as usize;
        let Some(&cp) = fi.checkpoints.get(k) else {
            continue;
        };
        let Some(mut rd) = TableReader::open(path, fi) else {
            continue;
        };
        let want = limit - rows.len();
        let _ = read_rows(&mut rd, fi, cp, local % STRIDE, want, &mut |l| {
            rows.push(fi.parse_row(l, name))
        });
    }
    DictEntriesPage { total, rows }
}

/// 整扫各表，收集命中行的（表下标, 行首偏移）。
fn scan_hits(
    idx: &[(String, PathBuf, Arc<FileIndex>)],
    query: &str,
    mode: EntryMatch,
) -> Vec<(u32, u64)> {
    let mut hits = Vec::new();
    for (ti, (_, path, fi)) in idx.iter().enumerate() {
        let mut raw = String::new();
        let Ok(_) = std::fs::File::open(path).and_then(|mut f| f.read_to_string(&mut raw)) else {
            continue;
        };
        let text = wind_utils::text::normalize_input(&raw);
        let Some((body, cutoff)) = wind_dict::rime_body_layout(&text) else {
            continue;
        };
        for (start, line) in entry_lines(&text, body, cutoff) {
            if fi.row_matches(line, query, mode) {
                hits.push((ti as u32, start as u64));
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use wind_config::Config;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    struct Fx {
        base: PathBuf,
        m: EngineManager,
    }
    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// 拼音式主库：本文件 3 条 + 子表 a（2 条，又 import 了 b 与 a 自己——防环）+ 子表 b（2 条）。
    fn fixture(tag: &str) -> Fx {
        let base =
            std::env::temp_dir().join(format!("wind_dict_view_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let s = base.join("schemas");
        write(
            &s.join("py.schema.toml"),
            "[schema]\nid = \"py\"\n[engine]\ntype = \"pinyin\"\n\
             [[dictionaries]]\nid = \"main\"\npath = \"py/main.dict.yaml\"\ntype = \"rime_pinyin\"\ndefault = true\n",
        );
        write(
            &s.join("py/main.dict.yaml"),
            "# Rime dictionary\n# encoding: utf-8\n#\n# 测试主库\n# 作者: 某人\n\n---\nname: main\n\
             version: \"2024-01-02\"\nsort: by_weight\nimport_tables:\n  - sub/a  # 甲表\n  - sub/b\n...\n\
             大\tda\t10\n# 注释\n小\txiao\t5\n\n大小\tda xiao\t3\n",
        );
        write(
            &s.join("py/sub/a.dict.yaml"),
            "---\nname: a\nimport_tables:\n  - a\n...\n你好\tni hao\t100\n换\\n行\thuan hang\t1\n",
        );
        write(
            &s.join("py/sub/b.dict.yaml"),
            "\u{feff}---\nname: b\ncolumns: [code, text]\n...\nnh\t你会\nzz\t字\n",
        );
        let mut cfg = Config::default();
        cfg.schema.available = vec!["py".into()];
        cfg.schema.active = "py".into();
        let m = EngineManager::new(&cfg, Some(&base));
        Fx { base, m }
    }

    fn texts(p: &DictEntriesPage) -> Vec<&str> {
        p.rows.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn count_sums_imports_recursively_and_guards_cycles() {
        let f = fixture("count");
        assert_eq!(f.m.dict_file_entries("py/main.dict.yaml"), Some(7));
        assert_eq!(f.m.dict_file_entries("py/sub/b.dict.yaml"), Some(2));
    }

    #[test]
    fn missing_sub_table_voids_count_but_header_lists_it() {
        let f = fixture("missing");
        std::fs::remove_file(f.base.join("schemas/py/sub/b.dict.yaml")).unwrap();
        assert_eq!(f.m.dict_file_entries("py/main.dict.yaml"), None);
        let h = f.m.dict_header("py/main.dict.yaml").unwrap();
        assert_eq!(h.count, None);
        let t: Vec<(&str, Option<u64>)> = h
            .tables
            .iter()
            .map(|t| (t.name.as_str(), t.count))
            .collect();
        assert_eq!(t, [("sub/a", Some(2)), ("sub/b", None)]);
        // 词条照样能看，缺的子表跳过
        let p =
            f.m.dict_entries("py/main.dict.yaml", "", EntryMatch::Code, 0, 100)
                .unwrap();
        assert_eq!(p.total, 5);
    }

    #[test]
    fn header_fields_and_notes() {
        let f = fixture("header");
        let h = f.m.dict_header("py/main.dict.yaml").unwrap();
        assert_eq!(h.name.as_deref(), Some("main"));
        assert_eq!(h.version.as_deref(), Some("2024-01-02"));
        assert_eq!(h.sort.as_deref(), Some("by_weight"));
        assert_eq!(h.import_tables, ["sub/a", "sub/b"]);
        assert!(h.columns.is_empty());
        assert_eq!(h.notes.as_deref(), Some("测试主库\n作者: 某人"));
        assert_eq!(h.count, Some(7));
        assert!(h.path.ends_with("py/main.dict.yaml"));
        assert!(h.mtime_ms.is_some());
        let b = f.m.dict_header("py/sub/b.dict.yaml").unwrap();
        assert_eq!(b.columns, ["code", "text"]);
        assert_eq!(b.notes, None);
        assert!(b.tables.is_empty());
        assert!(f.m.dict_header("py/none.dict.yaml").is_err());
    }

    #[test]
    fn browse_walks_tables_in_order_with_columns_and_unescape() {
        let f = fixture("browse");
        let all =
            f.m.dict_entries("py/main.dict.yaml", "", EntryMatch::Code, 0, 100)
                .unwrap();
        assert_eq!(all.total, 7);
        assert_eq!(
            texts(&all),
            ["大", "小", "大小", "你好", "换\n行", "你会", "字"]
        );
        assert_eq!(
            all.rows[2],
            DictEntryRow {
                code: "da xiao".into(),
                text: "大小".into(),
                weight: 3,
                table: "main".into()
            }
        );
        assert_eq!(all.rows[3].table, "sub/a");
        // columns: [code, text]：无权重列给 0，BOM 不影响
        assert_eq!(
            (
                all.rows[5].code.as_str(),
                all.rows[5].weight,
                all.rows[5].table.as_str()
            ),
            ("nh", 0, "sub/b")
        );
        // 跨表的页
        let p =
            f.m.dict_entries("py/main.dict.yaml", "", EntryMatch::Code, 2, 3)
                .unwrap();
        assert_eq!(texts(&p), ["大小", "你好", "换\n行"]);
        // 越界与末页
        let p =
            f.m.dict_entries("py/main.dict.yaml", "", EntryMatch::Code, 6, 10)
                .unwrap();
        assert_eq!((p.total, texts(&p)), (7, vec!["字"]));
        let p =
            f.m.dict_entries("py/main.dict.yaml", "", EntryMatch::Code, 99, 10)
                .unwrap();
        assert!(p.rows.is_empty());
        // limit 0 夹成 1
        let p =
            f.m.dict_entries("py/main.dict.yaml", "", EntryMatch::Code, 0, 0)
                .unwrap();
        assert_eq!(p.rows.len(), 1);
    }

    #[test]
    fn search_by_code_prefix_and_text_substring() {
        let f = fixture("search");
        let rel = "py/main.dict.yaml";
        let p =
            f.m.dict_entries(rel, "DA", EntryMatch::Code, 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["大", "大小"], "前缀、忽略大小写");
        let p =
            f.m.dict_entries(rel, "dax", EntryMatch::Code, 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["大小"], "跨音节空格");
        let p =
            f.m.dict_entries(rel, "n", EntryMatch::Code, 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["你好", "你会"], "跨表");
        let p =
            f.m.dict_entries(rel, "n", EntryMatch::Code, 1, 100)
                .unwrap();
        assert_eq!((p.total, texts(&p)), (2, vec!["你会"]), "命中结果分页");
        let p =
            f.m.dict_entries(rel, "大", EntryMatch::Text, 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["大", "大小"]);
        let p =
            f.m.dict_entries(rel, "换\n", EntryMatch::Text, 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["换\n行"], "按反转义后的文本匹配");
        let p =
            f.m.dict_entries(rel, "da", EntryMatch::Text, 0, 100)
                .unwrap();
        assert_eq!(p.total, 0, "文本模式不看编码列");
    }

    #[test]
    fn search_follows_file_changes() {
        let f = fixture("change");
        let rel = "py/sub/b.dict.yaml";
        assert_eq!(
            f.m.dict_entries(rel, "z", EntryMatch::Code, 0, 10)
                .unwrap()
                .total,
            1
        );
        write(
            &f.base.join("schemas/py/sub/b.dict.yaml"),
            "---\nname: b\ncolumns: [code, text]\n...\nzz\t字\nzy\t自\nzx\t在\n",
        );
        assert_eq!(
            f.m.dict_entries(rel, "z", EntryMatch::Code, 0, 10)
                .unwrap()
                .total,
            3
        );
        assert_eq!(f.m.dict_file_entries(rel), Some(3));
    }

    /// 多于一个检查点：任意偏移的页都与整读切片一致。
    #[test]
    fn pages_across_checkpoints_match_full_read() {
        let f = fixture("stride");
        let mut body = String::from("---\nname: big\n...\n");
        for i in 0..1000 {
            if i % 97 == 0 {
                body.push_str("# 注释\n\n");
            }
            body.push_str(&format!("词{i}\tc{i}\t{i}\r\n"));
        }
        write(&f.base.join("schemas/py/big.dict.yaml"), &body);
        let rel = "py/big.dict.yaml";
        let all = f.m.dict_entries(rel, "", EntryMatch::Code, 0, 500).unwrap();
        assert_eq!(all.total, 1000);
        for off in [0u64, 255, 256, 257, 511, 700, 999] {
            let p = f.m.dict_entries(rel, "", EntryMatch::Code, off, 7).unwrap();
            let want: Vec<String> = (off..(off + 7).min(1000))
                .map(|i| format!("词{i}"))
                .collect();
            assert_eq!(texts(&p), want, "offset {off}");
            assert_eq!(p.rows[0].weight as u64, off);
        }
    }

    /// 孤立 `\r` 换行的文件：偏移按规范化文本算，整读后照样分页。
    #[test]
    fn lone_cr_file_pages() {
        let f = fixture("cr");
        write(
            &f.base.join("schemas/py/cr.dict.yaml"),
            "---\rname: cr\r...\r甲\ta\r乙\tb\r丙\tc\r",
        );
        let p =
            f.m.dict_entries("py/cr.dict.yaml", "", EntryMatch::Code, 1, 5)
                .unwrap();
        assert_eq!((p.total, texts(&p)), (3, vec!["乙", "丙"]));
    }

    #[test]
    fn ordered_system_dicts_main_first_then_base_order_stable() {
        let schema: Schema = toml::from_str(
            "[[dictionaries]]\nid = \"x2\"\npath = \"x2\"\nbase_order = 2\n\
             [[dictionaries]]\nid = \"m\"\npath = \"m\"\ndefault = true\nbase_order = 5\n\
             [[dictionaries]]\nid = \"x1a\"\npath = \"x1a\"\nbase_order = 1\n\
             [[dictionaries]]\nid = \"nopath\"\n\
             [[dictionaries]]\nid = \"user:我的\"\npath = \"/abs/u.yaml\"\nbase_order = -3\n\
             [[dictionaries]]\nid = \"x1b\"\npath = \"x1b\"\nbase_order = 1\n",
        )
        .unwrap();
        let ids: Vec<(&str, bool)> = EngineManager::ordered_system_dicts(&schema)
            .into_iter()
            .map(|(d, m)| (d.id.as_str(), m))
            .collect();
        assert_eq!(
            ids,
            [("m", true), ("x1a", false), ("x1b", false), ("x2", false)]
        );
    }

    /// 真实拼音主库的翻页 / 搜索耗时（要 build_dev/data）：
    /// `cargo test -p wind-engine --release -- --ignored real_pinyin_entries_timing --nocapture`。
    #[test]
    #[ignore = "需 build_dev/data"]
    fn real_pinyin_entries_timing() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        assert!(
            data.join("schemas/pinyin.schema.toml").exists(),
            "缺 build_dev/data"
        );
        let mut cfg = Config::default();
        cfg.schema.available = vec!["pinyin".into()];
        cfg.schema.active = "pinyin".into();
        let m = EngineManager::new(&cfg, Some(&data));
        let rel = "pinyin/rime_frost.dict.yaml";
        let t = std::time::Instant::now();
        let n = m.dict_file_entries(rel).unwrap();
        eprintln!("首次计数（建索引）{n} 条: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let p = m.dict_entries(rel, "", EntryMatch::Code, 0, 100).unwrap();
        eprintln!("首页: {:?} ({} 行)", t.elapsed(), p.rows.len());
        for off in [n / 2, n - 100] {
            let t = std::time::Instant::now();
            let p = m.dict_entries(rel, "", EntryMatch::Code, off, 100).unwrap();
            eprintln!("第 {off} 条起一页: {:?} ({} 行)", t.elapsed(), p.rows.len());
        }
        for (q, mode) in [("ni", EntryMatch::Code), ("中国", EntryMatch::Text)] {
            let t = std::time::Instant::now();
            let p = m.dict_entries(rel, q, mode, 0, 100).unwrap();
            eprintln!("搜索 {q:?} 首页: {:?}，命中 {}", t.elapsed(), p.total);
            let t = std::time::Instant::now();
            let _ = m.dict_entries(rel, q, mode, p.total / 2, 100).unwrap();
            eprintln!("搜索 {q:?} 翻到中间: {:?}", t.elapsed());
        }
        let cps: usize = m
            .dict_tables(rel)
            .unwrap()
            .iter()
            .filter_map(|(_, p, _)| file_index(p))
            .map(|i| i.checkpoints.len())
            .sum();
        eprintln!("检查点 {cps} 个（{} 字节）", cps * 8);
        assert!(n > 600_000);
    }
}
