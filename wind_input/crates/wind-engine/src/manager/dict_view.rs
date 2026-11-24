//! 系统词库查看（设置端词库窗「系统词库」两级）的引擎侧：库的生效先后、文件条数、头部信息、
//! 只读词条分页与搜索。
//!
//! # 与加载器同口径
//!
//! - **哪些文件**：`rime_pinyin` 库 = 主表 + `import_tables` 子表**一层**（相对主表目录，缺的
//!   跳过），直接用加载器的 `rime_source_paths`；码表 / 英文库只有主表本身，`import_tables`
//!   只在头部卡片里原样列出声明。
//! - **哪些行**：逐行走加载器同一个解析函数（[`wind_dict::RimeRowParser`]）：列数不足、编码或
//!   词为空的行不算，`$CC(` 一类行纠正列序；`columns:` 声明残缺的文件加载器整库跳过，这里
//!   条数为空、词条给说明。
//! - **先后**：见 [`EngineManager::ordered_system_dicts`]。
//!
//! # 读文件，不碰引擎
//!
//! 全部只读词库**文本**（`.dict.yaml`），不加载 wdat、不持引擎锁：停用的库也要能看，而为看一眼
//! 去建 wdat 正是「停用的库只列不计」（`dict_stats`）要避免的事。调用方在 RPC 线程上调，
//! 不进按键链路。
//!
//! # 60 万条的主库怎么翻页
//!
//! 每个文件按（大小, mtime）缓存一份 [`FileIndex`]：条数、列规格，外加**每 [`STRIDE`] 条记一个
//! 行首字节偏移**的稀疏检查点。建索引要整读一遍文件（与计数是同一遍，条数本来就要数）；之后
//! 翻页只 seek 到最近的检查点、往后读不到 `STRIDE + limit` 行，与文件大小无关。检查点每张表
//! 几千个 `u64`（拼音全套约 20 KB），随条数缓存常驻，文件一变即按新戳重建。
//!
//! # 搜索与排序
//!
//! 搜索（编码前缀 / 词文本子串，默认两者取并）与排序（编码 / 词 / 权重）都没有现成索引，要整扫：
//! 一次扫完把命中行按排序键稳定排好（升降序都是同键按文件顺序），存成「表下标 + 行首偏移」
//! 打包的 `u64` 序列（每行 8 字节），同一组条件翻页直接按偏移读行。不搜索只排序 = 全库命中，
//! 60 万条的主库约 5 MB。排序时的键不逐行分配：权重直接带 `i32`，编码 / 词拼进一个大字符串、
//! 每行只记（偏移, 长度）。
//!
//! 结果进一个小 LRU（[`HITS_CACHE_SLOTS`] 组，合计不超过 [`HITS_CACHE_ROWS`] 行，单组超了就
//! 不缓存），键含各表的文件戳，文件一变即失效；闲置 [`HITS_CACHE_IDLE`] 没人查就整个清空（下次
//! 取放时惰性检查，不起线程）。新查询是缓存里某个查询的延长（打字往后加字）时，只在那组命中里
//! 复核，不再整扫：编码前缀与词子串都满足「查询变长、命中只少不多」，排好的先后也原样保留。

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use wind_config::schema::{DictSpec, Schema};
use wind_dict::{RimeRow, RimeRowParser};

use super::{DICT_YAML_SUFFIX, EngineManager};

/// 检查点间隔（条）。翻页最多多读这么多行。
const STRIDE: u64 = 256;

/// 一页最多取多少条（`sysDict.entries` 的 `limit` 上限）。
pub const ENTRIES_PAGE_MAX: usize = 500;

/// 头部前置注释最多取多少行（说明 / 作者 / 来源一类；长篇更新日志截断）。
const NOTE_LINES_MAX: usize = 40;

/// 缓存几组命中（见模块文档）。
const HITS_CACHE_SLOTS: usize = 4;

/// 缓存的命中合计最多多少行（每行 8 字节）：装得下主库整库排序一份，再多就淘汰最久没用的；
/// 单组就超过它的不缓存。
const HITS_CACHE_ROWS: usize = 1_000_000;

/// 命中缓存闲置多久整个清空（词库窗关着时不常驻那几 MB）。
const HITS_CACHE_IDLE: Duration = Duration::from_secs(5 * 60);

/// 词条搜索的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EntryMatch {
    /// 编码前缀（忽略大小写与音节间空格：`nihao` 命中 `ni hao`）。
    Code,
    /// 词文本子串。
    Text,
    /// 编码前缀或词文本子串，命中其一即可。
    #[default]
    Any,
}

/// 词条按哪一列排序。同键按文件顺序（稳定排序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntrySort {
    /// 编码（拼平后的字节序，`ni hao` 按 `nihao` 比）。
    Code,
    /// 词（Unicode 码位序）。
    Text,
    Weight,
}

/// `sysDict.entries` 的过滤与排序条件。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryQuery {
    /// 空（或全是空白）= 不过滤。
    pub query: String,
    pub mode: EntryMatch,
    /// `None` = 文件顺序。
    pub sort: Option<EntrySort>,
    pub desc: bool,
}

/// 一张子表（`rime_pinyin` 库 `import_tables` 里真正被加载的那些）。
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DictTableInfo {
    /// 相对主表目录、去 `.dict.yaml` 的名字（即 `import_tables` 里写的那个）。
    pub name: String,
    pub path: PathBuf,
    /// `columns:` 声明残缺（加载器跳过）为 `None`。
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
    /// 头部原样声明的子表（含不存在的、以及码表库里加载器并不展开的）。
    pub import_tables: Vec<String>,
    /// `---` 之前的说明注释（去掉 `# `、去掉 rime 模板的两行套话）；没有为 `None`。
    pub notes: Option<String>,
    pub path: PathBuf,
    pub mtime_ms: Option<u64>,
    /// 口径同 [`EngineManager::dict_file_entries`]。
    pub count: Option<u64>,
    /// 真正加载的子表（不含主文件本身），按声明顺序。
    pub tables: Vec<DictTableInfo>,
}

/// 一行词条（只读展示）。`code` 按音节边界还原空格（`ni hao`）。
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DictEntryRow {
    pub code: String,
    pub text: String,
    pub weight: i32,
    /// 所在表名（主表取文件名去 `.dict.yaml`，子表同 [`DictTableInfo::name`]）。
    pub table: String,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DictEntriesPage {
    pub total: u64,
    pub rows: Vec<DictEntryRow>,
    /// 有表因 `columns:` 声明残缺被加载器整个跳过时的说明（这些表不列）。
    pub note: Option<String>,
}

type Stamp = (u64, Option<SystemTime>);

/// 一个词库文件的索引，见模块文档。偏移都相对**规范化后**的文本（去 BOM；孤立 `\r` 改 `\n`）。
#[derive(Debug)]
struct FileIndex {
    stamp: Stamp,
    /// `None` = `columns:` 声明残缺，加载器整库跳过；此时 `count` 为 0、没有检查点。
    parser: Option<RimeRowParser>,
    count: u64,
    /// `# no comment` 指令位置：其后的 `#` 行是数据。
    cutoff: Option<usize>,
    /// 第 `k * STRIDE` 条的行首偏移。
    checkpoints: Vec<u64>,
    /// 文件开头的 BOM 字节数（规范化偏移 + 它 = 文件偏移）。
    bom: u64,
    /// 规范化改动过正文（孤立 `\r`）：偏移对不上文件，翻页要整读再规范化。
    reflowed: bool,
}

/// 键 =（文件, 编码是否小写化）：英文库加载时编码小写化，同一文件两种口径各一份。
type FileIndexCache = HashMap<(PathBuf, bool), Arc<FileIndex>>;
static FILE_INDEX: OnceLock<Mutex<FileIndexCache>> = OnceLock::new();

/// 命中缓存的键：（各表路径与戳, 编码是否小写化, 查询, 方式, 排序列, 是否降序）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct HitsKey {
    tables: Vec<(PathBuf, Stamp)>,
    /// 同 [`FileIndexCache`] 的键：英文库编码小写化，排序结果与码表口径不同。
    lowercase: bool,
    query: String,
    mode: EntryMatch,
    sort: Option<EntrySort>,
    desc: bool,
}

/// 排好序的命中行，每项 = [`pack`]（表下标, 行首偏移）。
type Hits = Arc<Vec<u64>>;

/// 命中 LRU（最近用的在末尾）与最近一次取放的时刻。
#[derive(Default)]
struct HitsCache {
    entries: Vec<(HitsKey, Hits)>,
    last: Option<Instant>,
}

impl HitsCache {
    /// 每次取放先调：闲置超过 [`HITS_CACHE_IDLE`] 就清空，再记下这次的时刻。
    fn touch(&mut self, now: Instant) {
        if self
            .last
            .is_some_and(|t| now.saturating_duration_since(t) > HITS_CACHE_IDLE)
        {
            self.entries.clear();
        }
        self.last = Some(now);
    }

    /// 取缓存；命中就挪到末尾（最近用）。
    fn get(&mut self, key: &HitsKey, now: Instant) -> Option<Hits> {
        self.touch(now);
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        let e = self.entries.remove(i);
        let h = e.1.clone();
        self.entries.push(e);
        Some(h)
    }

    /// 新查询能在哪组缓存命中里复核：除查询外键都相同，旧查询非空且是新查询的前缀；
    /// 有多组取最长的（命中最少）。
    fn base(&mut self, key: &HitsKey, now: Instant) -> Option<Hits> {
        self.touch(now);
        self.entries
            .iter()
            .filter(|(k, _)| {
                k.tables == key.tables
                    && k.lowercase == key.lowercase
                    && k.mode == key.mode
                    && k.sort == key.sort
                    && k.desc == key.desc
                    && !k.query.is_empty()
                    && key.query.starts_with(&k.query)
            })
            .max_by_key(|(k, _)| k.query.len())
            .map(|(_, h)| h.clone())
    }

    /// 放入并按组数 / 总行数淘汰最久没用的；单组超过 [`HITS_CACHE_ROWS`] 不放（也不挤掉别的）。
    fn put(&mut self, key: HitsKey, hits: Hits, now: Instant) {
        self.touch(now);
        if hits.len() > HITS_CACHE_ROWS {
            return;
        }
        self.entries.retain(|(k, _)| *k != key);
        self.entries.push((key, hits));
        let mut rows: usize = self.entries.iter().map(|(_, h)| h.len()).sum();
        while self.entries.len() > HITS_CACHE_SLOTS || rows > HITS_CACHE_ROWS {
            rows -= self.entries.remove(0).1.len();
        }
    }

    #[cfg(test)]
    fn rows(&self) -> usize {
        self.entries.iter().map(|(_, h)| h.len()).sum()
    }
}

static HITS_CACHE: OnceLock<Mutex<HitsCache>> = OnceLock::new();

/// 偏移占低 48 位（256 TB），表下标占高 16 位。
const POS_BITS: u32 = 48;

fn pack(table: usize, pos: usize) -> u64 {
    ((table as u64) << POS_BITS) | pos as u64
}

fn unpack(v: u64) -> (usize, u64) {
    ((v >> POS_BITS) as usize, v & ((1 << POS_BITS) - 1))
}

fn hits_cache() -> std::sync::MutexGuard<'static, HitsCache> {
    HITS_CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// 命中缓存的当前行数（耗时记录用）。
#[cfg(test)]
fn cached_hit_rows() -> usize {
    hits_cache().rows()
}

/// 一次查询的归一形态：编码按小写、去空格比前缀，词按原文比子串。
struct Needle<'a> {
    code: String,
    text: &'a str,
    mode: EntryMatch,
}

impl<'a> Needle<'a> {
    fn new(query: &'a str, mode: EntryMatch) -> Self {
        Self {
            code: query.replace(' ', "").to_ascii_lowercase(),
            text: query,
            mode,
        }
    }

    fn by_code(&self) -> bool {
        self.mode != EntryMatch::Text && !self.code.is_empty()
    }

    fn by_text(&self) -> bool {
        self.mode != EntryMatch::Code
    }
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    Some((m.len(), m.modified().ok()))
}

fn strip_eol(raw: &str) -> &str {
    let l = raw.strip_suffix('\n').unwrap_or(raw);
    l.strip_suffix('\r').unwrap_or(l)
}

/// 正文各行：`(行首偏移, 去掉换行的行, 本行注释是否生效)`。
fn body_lines(
    text: &str,
    body: usize,
    cutoff: Option<usize>,
) -> impl Iterator<Item = (usize, &str, bool)> {
    let mut off = body;
    text[body..].split_inclusive('\n').map(move |raw| {
        let start = off;
        off += raw.len();
        (start, strip_eol(raw), cutoff.is_none_or(|c| start <= c))
    })
}

/// 数 rime 词库正文里加载器会收的行。
#[cfg(test)]
pub(super) fn count_dict_entries(raw: &str) -> u64 {
    let text = wind_utils::text::normalize_input(raw);
    let Some((body, cutoff)) = wind_dict::rime_body_layout(&text) else {
        return 0;
    };
    let Some(p) = RimeRowParser::new(&text, Path::new("test"), false) else {
        return 0;
    };
    body_lines(&text, body, cutoff)
        .filter(|(_, l, on)| p.parse(l, *on).is_some())
        .count() as u64
}

/// 整读一个文件并规范化：`(规范化文本, BOM 字节数, 是否改动过正文)`。
fn read_normalized(path: &Path) -> Option<(String, u64, bool)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let bom = if raw.starts_with('\u{feff}') { 3 } else { 0 };
    let (text, reflowed) = match wind_utils::text::normalize_input(&raw) {
        Cow::Borrowed(t) => (t.to_string(), false),
        Cow::Owned(t) => (t, true),
    };
    Some((text, bom, reflowed))
}

impl FileIndex {
    /// 读戳 → 整读 → 再读戳，两次不同（读的过程中文件被改）就重来一次。
    fn build(path: &Path, lowercase: bool) -> Option<FileIndex> {
        for _ in 0..2 {
            let before = stamp_of(path)?;
            let (text, bom, reflowed) = read_normalized(path)?;
            if stamp_of(path)? != before {
                continue;
            }
            return Some(Self::from_text(
                &text, path, before, bom, reflowed, lowercase,
            ));
        }
        None
    }

    fn from_text(
        text: &str,
        path: &Path,
        stamp: Stamp,
        bom: u64,
        reflowed: bool,
        lowercase: bool,
    ) -> FileIndex {
        let mut idx = FileIndex {
            stamp,
            parser: None,
            count: 0,
            cutoff: None,
            checkpoints: Vec::new(),
            bom,
            reflowed,
        };
        let Some((body, cutoff)) = wind_dict::rime_body_layout(text) else {
            // 没有正文分隔行：加载器按零条目处理（不是「不加载」）
            idx.parser = RimeRowParser::new("---\n...\n", path, lowercase);
            return idx;
        };
        idx.cutoff = cutoff;
        idx.parser = RimeRowParser::new(text, path, lowercase);
        let Some(p) = idx.parser else {
            return idx;
        };
        for (start, line, on) in body_lines(text, body, cutoff) {
            if p.parse(line, on).is_none() {
                continue;
            }
            if idx.count.is_multiple_of(STRIDE) {
                idx.checkpoints.push(start as u64);
            }
            idx.count += 1;
        }
        idx
    }

    fn row(&self, r: RimeRow, table: &str) -> DictEntryRow {
        DictEntryRow {
            code: wind_store::wdict::join_code_by_boundary(&r.code, r.boundary),
            text: r.text,
            weight: r.weight,
            table: table.to_string(),
        }
    }

    /// 本行（加载器收它的话）且命中 `q` 时给出解析好的行；`q` 为 `None` 不过滤。整扫的热路径。
    fn match_row(p: &RimeRowParser, line: &str, on: bool, q: Option<&Needle>) -> Option<RimeRow> {
        let Some(q) = q else {
            return p.parse(line, on);
        };
        // 粗筛（必要条件，多数行到此为止）：文本——不带转义时整行不含就一定不中；编码——
        // 编码列跳过空格后不以查询开头就不中（`$` 行列序可能被纠正，交给完整解析）
        let text_pass = q.by_text() && (line.contains('\\') || line.contains(q.text));
        let code_pass = !text_pass
            && q.by_code()
            && (line.contains('$')
                || line.split('\t').nth(p.code_col()).is_some_and(|f| {
                    let mut raw = f.bytes().filter(|&b| b != b' ');
                    q.code
                        .bytes()
                        .all(|c| raw.next().is_some_and(|b| b.eq_ignore_ascii_case(&c)))
                }));
        if !text_pass && !code_pass {
            return None;
        }
        let r = p.parse(line, on)?;
        let hit = (q.by_text() && r.text.contains(q.text))
            || (q.by_code()
                && r.code.len() >= q.code.len()
                && r.code.as_bytes()[..q.code.len()].eq_ignore_ascii_case(q.code.as_bytes()));
        hit.then_some(r)
    }
}

/// 取（或重建）一个文件的索引；文件不在 / 读不出为 `None`。不持锁读文件。
fn file_index(path: &Path, lowercase: bool) -> Option<Arc<FileIndex>> {
    let stamp = stamp_of(path)?;
    let key = (path.to_path_buf(), lowercase);
    let cache = FILE_INDEX.get_or_init(Default::default);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .filter(|i| i.stamp == stamp)
    {
        return Some(hit.clone());
    }
    let idx = Arc::new(FileIndex::build(path, lowercase)?);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, idx.clone());
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
            let (text, _, _) = read_normalized(path)?;
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
    f: &mut dyn FnMut(RimeRow),
) -> std::io::Result<()> {
    let Some(p) = idx.parser else {
        return Ok(());
    };
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
        let on = idx.cutoff.is_none_or(|c| start <= c);
        let Some(r) = p.parse(strip_eol(&line), on) else {
            continue;
        };
        if skip > 0 {
            skip -= 1;
            continue;
        }
        f(r);
        got += 1;
    }
    Ok(())
}

/// 读单条（偏移处的那一行）。
fn read_row_at(rd: &mut TableReader, idx: &FileIndex, pos: u64) -> Option<RimeRow> {
    let mut out = None;
    read_rows(rd, idx, pos, 0, 1, &mut |r| out = Some(r)).ok()?;
    out
}

/// 头部 YAML 段（`---` 与 `...` 之间）与 `---` 之前的注释行。读到 `...` 即停。
fn read_head_parts(path: &Path) -> (String, Vec<String>) {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else {
        return (String::new(), Vec::new());
    };
    // 只读头部一段（注释 + YAML 段远小于两倍上限），先统一换行再逐行看：孤立 `\r` 不归一的话
    // 整个头部会算成一行。
    let mut buf = Vec::new();
    let limit = (super::DICT_HEAD_SCAN_LIMIT * 2) as u64;
    if f.take(limit).read_to_end(&mut buf).is_err() {
        return (String::new(), Vec::new());
    }
    let raw = String::from_utf8_lossy(&buf);
    let text = wind_utils::text::normalize_input(&raw);
    let mut yaml = String::new();
    let mut notes = Vec::new();
    let mut in_yaml = false;
    for (i, line) in text.lines().enumerate() {
        let line = if i == 0 {
            line.trim_start_matches('\u{feff}')
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

/// 一本库的各表（主表在首），带索引。
type Tables = Vec<(String, PathBuf, Arc<FileIndex>)>;

impl EngineManager {
    /// 方案的系统词库按**实际叠层先后**排好，第二个值 = 是不是主库（同 `split_codetable_dicts`：
    /// 首个 `default`，没有则首个可用库）。不含用户分库与没有 `path` 的项。
    ///
    /// - 拼音（`Schema::is_pinyin`）：`load_dictionary` 按方案文件顺序把启用的库叠成
    ///   `CachedDict::Layered`，`base_order` 不起作用 ⇒ 按文件顺序。
    /// - 码表 / 英文：同权时按 `base_order` 升序 ⇒ 按它稳定排序；主库也按自己的 `base_order`
    ///   排，不一定在首。`base_order` 相同的库之间没有库级先后（同权时按词条行序），这里按方案
    ///   文件顺序列出。
    pub fn ordered_system_dicts(schema: &Schema) -> Vec<(&DictSpec, bool)> {
        let Some((main, _)) = Self::split_codetable_dicts(schema) else {
            return Vec::new();
        };
        let mut v: Vec<&DictSpec> = schema
            .dictionaries
            .iter()
            .filter(|d| !d.path.is_empty() && !d.id.starts_with(wind_store::user_dicts::ID_PREFIX))
            .collect();
        if !schema.is_pinyin() {
            v.sort_by_key(|d| d.base_order);
        }
        v.into_iter().map(|d| (d, std::ptr::eq(d, main))).collect()
    }

    /// 一本库加载器实际读的文件：主表在首；`rime_pinyin` 另加 `import_tables` 子表一层
    /// （[`Self::rime_source_paths`]，缺的跳过）。第一项是主表（可能不存在）。
    fn dict_files(&self, rel: &str, dict_type: &str) -> Option<Vec<(String, PathBuf)>> {
        let schemas_dir = self.data_dir.as_deref()?.join("schemas");
        let main = Self::resolve_dict_file(rel, &schemas_dir);
        let mut out = vec![(main_table_name(&main), main.clone())];
        if dict_type == "rime_pinyin" && main.is_file() {
            let dir = main.parent().map(Path::to_path_buf).unwrap_or_default();
            for sub in Self::rime_source_paths(&main, dict_type)
                .into_iter()
                .skip(1)
            {
                let rel_sub = sub
                    .strip_prefix(&dir)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| sub.to_string_lossy().into_owned());
                let name = rel_sub
                    .strip_suffix(DICT_YAML_SUFFIX)
                    .unwrap_or(&rel_sub)
                    .to_string();
                out.push((name, sub));
            }
        }
        Some(out)
    }

    /// 带索引的各表；主表不在 → Err。读不出的子表跳过（加载器同样跳过）。
    fn dict_tables(&self, rel: &str, dict_type: &str) -> anyhow::Result<Tables> {
        let files = self
            .dict_files(rel, dict_type)
            .ok_or_else(|| anyhow::anyhow!("没有数据目录"))?;
        let lc = dict_type == "english";
        let mut out = Vec::new();
        for (i, (name, path)) in files.into_iter().enumerate() {
            match file_index(&path, lc) {
                Some(fi) => out.push((name, path, fi)),
                None if i == 0 => anyhow::bail!("词库文件不存在: {}", path.display()),
                None => {}
            }
        }
        Ok(out)
    }

    /// 一张词库的条目数（设置端「系统词库」表用）：按方案里的 `path` 经覆盖解析定位，口径同
    /// 加载器（见模块文档）。停用的库也数得出——只读文本，不加载、不建 wdat。按（大小, mtime）
    /// 缓存，文件没变不重扫。主表不在（含只投放了 wdat 的库）或有表 `columns:` 声明残缺为 `None`。
    pub fn dict_file_entries(&self, rel: &str, dict_type: &str) -> Option<u64> {
        let tables = self.dict_tables(rel, dict_type).ok()?;
        if tables.iter().any(|(_, _, fi)| fi.parser.is_none()) {
            return None;
        }
        Some(tables.iter().map(|(_, _, fi)| fi.count).sum())
    }

    /// `sysDict.header` 的引擎侧：`rel` 的头部字段、文件信息与子表清单。主文件不在 → Err。
    pub fn dict_header(&self, rel: &str, dict_type: &str) -> anyhow::Result<DictHeader> {
        let tables = self.dict_tables(rel, dict_type)?;
        let main = tables[0].1.clone();
        let (yaml, notes) = read_head_parts(&main);
        let head = serde_yaml::from_str::<serde_yaml::Value>(&yaml).unwrap_or_default();
        Ok(DictHeader {
            name: yaml_scalar(head.get("name")),
            version: yaml_scalar(head.get("version")),
            sort: yaml_scalar(head.get("sort")),
            columns: yaml_strs(head.get("columns")),
            // 清单以逐行解析为准（剥了行内注释）；YAML 段解析失败也拿得到
            import_tables: Self::read_dict_head(&main).1,
            notes: tidy_notes(notes),
            mtime_ms: mtime_ms(&main),
            count: self.dict_file_entries(rel, dict_type),
            tables: tables[1..]
                .iter()
                .map(|(name, path, fi)| DictTableInfo {
                    name: name.clone(),
                    path: path.clone(),
                    count: fi.parser.map(|_| fi.count),
                })
                .collect(),
            path: main,
        })
    }

    /// `sysDict.entries` 的引擎侧：按 `q` 过滤、排序后分页（不过滤不排序时按文件顺序，
    /// `rime_pinyin` 库依次展开子表）。`limit` 夹到 `1..=ENTRIES_PAGE_MAX`。主文件不在 → Err。
    pub fn dict_entries(
        &self,
        rel: &str,
        dict_type: &str,
        q: &EntryQuery,
        offset: u64,
        limit: usize,
    ) -> anyhow::Result<DictEntriesPage> {
        let limit = limit.clamp(1, ENTRIES_PAGE_MAX);
        let mut idx = self.dict_tables(rel, dict_type)?;
        let skipped: Vec<&str> = idx
            .iter()
            .filter(|(_, _, fi)| fi.parser.is_none())
            .map(|(n, _, _)| n.as_str())
            .collect();
        let note = (!skipped.is_empty()).then(|| {
            format!(
                "{} 的 columns: 声明缺 text 或 code 列，输入法不加载，词条不列",
                skipped.join("、")
            )
        });
        let query = q.query.trim();
        if query.is_empty() && q.sort.is_none() {
            let mut page = browse_page(&idx, offset, limit);
            page.note = note;
            return Ok(page);
        }
        let needle = (!query.is_empty()).then(|| Needle::new(query, q.mode));
        let mut hits = None;
        // 扫描时发现文件在索引之后变过：重建索引再扫一次
        for _ in 0..2 {
            let key = HitsKey {
                tables: idx.iter().map(|(_, p, i)| (p.clone(), i.stamp)).collect(),
                lowercase: dict_type == "english",
                query: query.to_string(),
                mode: q.mode,
                sort: q.sort,
                desc: q.desc,
            };
            if let Some(h) = hits_cache().get(&key, Instant::now()) {
                hits = Some(h);
                break;
            }
            let base = hits_cache().base(&key, Instant::now());
            let found = match base {
                Some(base) => refine_hits(&idx, &base, needle.as_ref()),
                None => scan_hits(&idx, needle.as_ref(), q.sort, q.desc),
            };
            match found {
                Some(h) => {
                    let h = Arc::new(h);
                    hits_cache().put(key, h.clone(), Instant::now());
                    hits = Some(h);
                    break;
                }
                None => idx = self.dict_tables(rel, dict_type)?,
            }
        }
        let hits = hits.ok_or_else(|| anyhow::anyhow!("词库文件正在变化，请稍后再试"))?;
        let n = hits.len();
        let picked = (offset as usize).min(n)..(offset as usize).saturating_add(limit).min(n);
        let mut readers: Vec<Option<TableReader>> = idx.iter().map(|_| None).collect();
        let mut rows = Vec::with_capacity(picked.len());
        for i in picked {
            let (ti, pos) = unpack(hits[i]);
            let Some((name, path, fi)) = idx.get(ti) else {
                continue;
            };
            if readers[ti].is_none() {
                readers[ti] = TableReader::open(path, fi);
            }
            let Some(rd) = readers[ti].as_mut() else {
                continue;
            };
            if let Some(r) = read_row_at(rd, fi, pos) {
                rows.push(fi.row(r, name));
            }
        }
        Ok(DictEntriesPage {
            total: n as u64,
            rows,
            note,
        })
    }
}

/// 不搜索的一页：全局序号 → 落在哪张表、该表第几条，从最近的检查点读起。
fn browse_page(idx: &Tables, offset: u64, limit: usize) -> DictEntriesPage {
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
        let Some(&cp) = fi.checkpoints.get((local / STRIDE) as usize) else {
            continue;
        };
        let Some(mut rd) = TableReader::open(path, fi) else {
            continue;
        };
        let want = limit - rows.len();
        let _ = read_rows(&mut rd, fi, cp, local % STRIDE, want, &mut |r| {
            rows.push(fi.row(r, name))
        });
    }
    DictEntriesPage {
        total,
        rows,
        note: None,
    }
}

/// 读一张表的规范化全文，并确认它仍是索引时的那一份（戳变了 → `None`，偏移已对不上）。
fn table_text(path: &Path, fi: &FileIndex) -> Option<Option<String>> {
    let Some((text, _, _)) = read_normalized(path) else {
        return Some(None);
    };
    if stamp_of(path) != Some(fi.stamp) {
        return None;
    }
    Some(Some(text))
}

/// 整扫各表，收集命中 `q`（`None` = 全部）的行，按 `sort` 稳定排序（`None` = 文件顺序）；
/// `desc` 降序，同键仍按文件顺序。读完某张表发现它的戳与索引不符 → `None`，调用方重建索引重扫。
fn scan_hits(
    idx: &Tables,
    q: Option<&Needle>,
    sort: Option<EntrySort>,
    desc: bool,
) -> Option<Vec<u64>> {
    let mut hits = Vec::new();
    // 排序键不逐行分配：权重带 i32；编码 / 词拼进 `keys`，行上只记（偏移, 长度）。不过滤时
    // 行数已知，一次留够，免得倍增扩容的峰值。
    let all = if q.is_none() {
        idx.iter().map(|(_, _, fi)| fi.count as usize).sum()
    } else {
        0
    };
    let mut by_num: Vec<(i32, u64)> = Vec::new();
    let mut keys = String::new();
    let mut by_str: Vec<(u32, u32, u64)> = Vec::new();
    match sort {
        None => hits.reserve_exact(all),
        Some(EntrySort::Weight) => by_num.reserve_exact(all),
        Some(_) => by_str.reserve_exact(all),
    }
    for (ti, (_, path, fi)) in idx.iter().enumerate() {
        let Some(p) = fi.parser else {
            continue;
        };
        let Some(text) = table_text(path, fi)? else {
            continue;
        };
        let Some((body, cutoff)) = wind_dict::rime_body_layout(&text) else {
            continue;
        };
        for (start, line, on) in body_lines(&text, body, cutoff) {
            let Some(r) = FileIndex::match_row(&p, line, on, q) else {
                continue;
            };
            let h = pack(ti, start);
            let key = match sort {
                None => {
                    hits.push(h);
                    continue;
                }
                Some(EntrySort::Weight) => {
                    by_num.push((r.weight, h));
                    continue;
                }
                Some(EntrySort::Code) => &r.code,
                Some(EntrySort::Text) => &r.text,
            };
            by_str.push((keys.len() as u32, key.len() as u32, h));
            keys.push_str(key);
        }
    }
    // 稳定排序：降序只反比较、不倒序列，同键留在文件顺序
    let ord = |o: std::cmp::Ordering| if desc { o.reverse() } else { o };
    match sort {
        None if desc => hits.reverse(),
        None => {}
        // 结果另起一份恰好大小的（`collect` 会就地复用排序用的大缓冲，进缓存后白占两三倍）
        Some(EntrySort::Weight) => {
            by_num.sort_by(|a, b| ord(a.0.cmp(&b.0)));
            hits = Vec::with_capacity(by_num.len());
            hits.extend(by_num.iter().map(|&(_, h)| h));
        }
        Some(_) => {
            let key = |&(o, l, _): &(u32, u32, u64)| &keys[o as usize..(o + l) as usize];
            by_str.sort_by(|a, b| ord(key(a).cmp(key(b))));
            hits = Vec::with_capacity(by_str.len());
            hits.extend(by_str.iter().map(|&(_, _, h)| h));
        }
    }
    hits.shrink_to_fit();
    Some(hits)
}

/// 在一组已排好的命中里复核新查询（新查询是旧查询的延长，命中只少不多），先后不变。
/// 戳不符同 [`scan_hits`]。
fn refine_hits(idx: &Tables, base: &[u64], q: Option<&Needle>) -> Option<Vec<u64>> {
    let mut texts: Vec<Option<Option<String>>> = idx.iter().map(|_| None).collect();
    let mut out = Vec::new();
    for &h in base {
        let (ti, pos) = unpack(h);
        let Some((_, path, fi)) = idx.get(ti) else {
            continue;
        };
        let Some(p) = fi.parser else {
            continue;
        };
        if texts[ti].is_none() {
            texts[ti] = Some(table_text(path, fi)?);
        }
        let Some(Some(text)) = &texts[ti] else {
            continue;
        };
        let Some(raw) = text.get(pos as usize..) else {
            continue;
        };
        let line = strip_eol(raw.split_inclusive('\n').next().unwrap_or(""));
        let on = fi.cutoff.is_none_or(|c| pos as usize <= c);
        if FileIndex::match_row(&p, line, on, q).is_some() {
            out.push(h);
        }
    }
    out.shrink_to_fit();
    Some(out)
}

#[cfg(test)]
#[path = "dict_view_tests.rs"]
mod tests;
