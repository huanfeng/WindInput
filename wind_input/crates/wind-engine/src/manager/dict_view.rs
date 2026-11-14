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

    const PY: &str = "rime_pinyin";
    const CT: &str = "rime_codetable";

    /// 拼音式主库：本文件 3 条 + 子表 a（2 条，它自己又 import 了 c 与 a）+ 子表 b（2 条）
    /// + 声明了但不存在的 gone。码表式 `ct/main`：本文件 2 条，也写了 import_tables。
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
             version: \"2024-01-02\"\nsort: by_weight\nimport_tables:\n  - sub/a  # 甲表\n  - sub/gone\n  - sub/b\n...\n\
             大\tda\t10\n# 注释\n小\txiao\t5\n\n大小\tda xiao\t3\n",
        );
        write(
            &s.join("py/sub/a.dict.yaml"),
            "---\nname: a\nimport_tables:\n  - c\n  - a\n...\n你好\tni hao\t100\n换\\n行\thuan hang\t1\n",
        );
        write(
            &s.join("py/sub/c.dict.yaml"),
            "---\nname: c\n...\n嵌套\tqian tao\t1\n",
        );
        write(
            &s.join("py/sub/b.dict.yaml"),
            "\u{feff}---\nname: b\ncolumns: [code, text]\n...\nnh\t你会\nzz\t字\n",
        );
        write(
            &s.join("ct/main.dict.yaml"),
            "---\nname: ct\nimport_tables:\n  - ext\n...\naa\t工\t9\nbb\t子\t5\n",
        );
        write(
            &s.join("ct/ext.dict.yaml"),
            "---\nname: ext\n...\ncc\t又\t1\n",
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

    fn eq(q: &str, mode: EntryMatch) -> EntryQuery {
        EntryQuery {
            query: q.into(),
            mode,
            ..Default::default()
        }
    }

    fn sorted(q: &str, sort: EntrySort, desc: bool) -> EntryQuery {
        EntryQuery {
            query: q.into(),
            sort: Some(sort),
            desc,
            ..Default::default()
        }
    }

    const MAIN: &str = "py/main.dict.yaml";

    /// 与加载器同口径：拼音库展开子表一层（不进子表自己的 import_tables、缺的跳过），码表库
    /// 不展开。
    #[test]
    fn count_expands_one_level_for_pinyin_only() {
        let f = fixture("count");
        assert_eq!(
            f.m.dict_file_entries(MAIN, PY),
            Some(7),
            "3 + a 2 + b 2，不含 c"
        );
        assert_eq!(f.m.dict_file_entries("py/sub/b.dict.yaml", PY), Some(2));
        assert_eq!(
            f.m.dict_file_entries("ct/main.dict.yaml", CT),
            Some(2),
            "码表库的 import_tables 加载器不读"
        );
        assert_eq!(f.m.dict_file_entries("py/none.dict.yaml", PY), None);
    }

    #[test]
    fn header_lists_declared_imports_but_only_loaded_tables() {
        let f = fixture("header");
        let h = f.m.dict_header(MAIN, PY).unwrap();
        assert_eq!(h.name.as_deref(), Some("main"));
        assert_eq!(h.version.as_deref(), Some("2024-01-02"));
        assert_eq!(h.sort.as_deref(), Some("by_weight"));
        assert_eq!(
            h.import_tables,
            ["sub/a", "sub/gone", "sub/b"],
            "声明原样列"
        );
        assert!(h.columns.is_empty());
        assert_eq!(h.notes.as_deref(), Some("测试主库\n作者: 某人"));
        assert_eq!(h.count, Some(7));
        assert!(h.path.ends_with("py/main.dict.yaml"));
        assert!(h.mtime_ms.is_some());
        let t: Vec<(&str, Option<u64>)> = h
            .tables
            .iter()
            .map(|t| (t.name.as_str(), t.count))
            .collect();
        assert_eq!(t, [("sub/a", Some(2)), ("sub/b", Some(2))], "缺的不列");
        let c = f.m.dict_header("ct/main.dict.yaml", CT).unwrap();
        assert_eq!(
            (c.import_tables.len(), c.tables.len(), c.count),
            (1, 0, Some(2))
        );
        let b = f.m.dict_header("py/sub/b.dict.yaml", PY).unwrap();
        assert_eq!(b.columns, ["code", "text"]);
        assert_eq!(b.notes, None);
        assert!(f.m.dict_header("py/none.dict.yaml", PY).is_err());
    }

    #[test]
    fn browse_walks_tables_in_order_with_columns_and_unescape() {
        let f = fixture("browse");
        let all =
            f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 0, 100)
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
        assert_eq!(
            (
                all.rows[5].code.as_str(),
                all.rows[5].weight,
                all.rows[5].table.as_str()
            ),
            ("nh", 0, "sub/b"),
            "columns: [code, text]：无权重列给 0，BOM 不影响"
        );
        assert_eq!(all.note, None);
        let p =
            f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 2, 3)
                .unwrap();
        assert_eq!(texts(&p), ["大小", "你好", "换\n行"], "跨表的页");
        let p =
            f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 6, 10)
                .unwrap();
        assert_eq!((p.total, texts(&p)), (7, vec!["字"]));
        let p =
            f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 99, 10)
                .unwrap();
        assert!(p.rows.is_empty());
        let p =
            f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 0, 0)
                .unwrap();
        assert_eq!(p.rows.len(), 1, "limit 0 夹成 1");
        let c =
            f.m.dict_entries("ct/main.dict.yaml", CT, &eq("", EntryMatch::Code), 0, 9)
                .unwrap();
        assert_eq!(texts(&c), ["工", "子"], "码表库不混入 import_tables 的子表");
    }

    #[test]
    fn search_by_code_prefix_and_text_substring() {
        let f = fixture("search");
        let p =
            f.m.dict_entries(MAIN, PY, &eq("DA", EntryMatch::Code), 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["大", "大小"], "前缀、忽略大小写");
        let p =
            f.m.dict_entries(MAIN, PY, &eq("da x", EntryMatch::Code), 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["大小"], "跨音节空格");
        let p =
            f.m.dict_entries(MAIN, PY, &eq("n", EntryMatch::Code), 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["你好", "你会"], "跨表");
        let p =
            f.m.dict_entries(MAIN, PY, &eq("n", EntryMatch::Code), 1, 100)
                .unwrap();
        assert_eq!((p.total, texts(&p)), (2, vec!["你会"]), "命中结果分页");
        let p =
            f.m.dict_entries(MAIN, PY, &eq("大", EntryMatch::Text), 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["大", "大小"]);
        let p =
            f.m.dict_entries(MAIN, PY, &eq("换\n", EntryMatch::Text), 0, 100)
                .unwrap();
        assert_eq!(texts(&p), ["换\n行"], "按反转义后的文本匹配");
        let p =
            f.m.dict_entries(MAIN, PY, &eq("da", EntryMatch::Text), 0, 100)
                .unwrap();
        assert_eq!(p.total, 0, "文本模式不看编码列");
        let p =
            f.m.dict_entries(MAIN, PY, &eq("qian", EntryMatch::Code), 0, 100)
                .unwrap();
        assert_eq!(p.total, 0, "子表自己的 import_tables 不展开");
    }

    /// 不指定方式时编码前缀与词子串取并。
    #[test]
    fn default_mode_matches_code_prefix_or_text_substring() {
        let f = fixture("any");
        assert_eq!(EntryQuery::default().mode, EntryMatch::Any);
        let get = |q: &str| {
            f.m.dict_entries(MAIN, PY, &eq(q, EntryMatch::Any), 0, 100)
                .unwrap()
        };
        assert_eq!(texts(&get("大")), ["大", "大小"], "词子串");
        assert_eq!(
            texts(&get("X")),
            ["小"],
            "编码前缀、忽略大小写：大小 的 da xiao 不以 x 打头"
        );
        assert_eq!(texts(&get("da")), ["大", "大小"], "编码前缀");
        assert_eq!(texts(&get("你")), ["你好", "你会"], "跨表");
        assert_eq!(texts(&get("  ")), texts(&get("")), "只有空白 = 不过滤");
    }

    /// 打字往后加字：在上一组命中里复核，结果与整扫一致。
    #[test]
    fn extending_a_query_narrows_the_previous_hits() {
        let f = fixture("narrow");
        let get = |q: &str| {
            f.m.dict_entries(MAIN, PY, &eq(q, EntryMatch::Any), 0, 100)
                .unwrap()
        };
        assert_eq!(texts(&get("n")), ["你好", "你会"]);
        assert_eq!(texts(&get("ni")), ["你好"]);
        assert_eq!(texts(&get("nih")), ["你好"]);
        assert_eq!(get("nix").total, 0);
        assert_eq!(texts(&get("n")), ["你好", "你会"], "退格回到缓存");
        let s = |q: &str| {
            f.m.dict_entries(MAIN, PY, &sorted(q, EntrySort::Weight, true), 0, 100)
                .unwrap()
        };
        assert_eq!(texts(&s("d")), ["大", "大小"]);
        assert_eq!(texts(&s("da")), ["大", "大小"], "复核保留排好的先后");
        // 缓存是进程级的，并行用例可能把上一组挤掉：直接比对复核与整扫
        let idx = f.m.dict_tables(MAIN, PY).unwrap();
        for (old, new, sort) in [
            ("n", "ni", None),
            ("d", "da x", Some(EntrySort::Text)),
            ("大", "大小", Some(EntrySort::Weight)),
        ] {
            let base =
                scan_hits(&idx, Some(&Needle::new(old, EntryMatch::Any)), sort, false).unwrap();
            let want =
                scan_hits(&idx, Some(&Needle::new(new, EntryMatch::Any)), sort, false).unwrap();
            let got = refine_hits(&idx, &base, Some(&Needle::new(new, EntryMatch::Any))).unwrap();
            assert!(!want.is_empty() && got == want, "{old} → {new}");
        }
    }

    /// 三列各自升降序、分页，以及搜索结果内排序。同键按文件顺序。
    #[test]
    fn sorts_by_each_column_and_pages() {
        let f = fixture("sort");
        let get =
            |q: EntryQuery, off: u64, lim: usize| f.m.dict_entries(MAIN, PY, &q, off, lim).unwrap();
        let w = get(sorted("", EntrySort::Weight, false), 0, 100);
        assert_eq!(w.total, 7);
        assert_eq!(
            texts(&w),
            ["你会", "字", "换\n行", "大小", "小", "大", "你好"],
            "权重升序，你会 / 字 同为 0 按文件顺序"
        );
        assert_eq!(
            texts(&get(sorted("", EntrySort::Weight, true), 0, 3)),
            ["你好", "大", "小"]
        );
        assert_eq!(
            texts(&get(sorted("", EntrySort::Weight, true), 5, 100)),
            ["你会", "字"],
            "降序时同键也按文件顺序"
        );
        assert_eq!(
            texts(&get(sorted("", EntrySort::Weight, false), 2, 3)),
            ["换\n行", "大小", "小"],
            "排序后分页"
        );
        assert_eq!(
            texts(&get(sorted("", EntrySort::Code, false), 0, 100)),
            ["大", "大小", "换\n行", "你会", "你好", "小", "字"],
            "编码按拼平后比：da < daxiao < huanhang < nh < nihao"
        );
        assert_eq!(
            texts(&get(sorted("", EntrySort::Text, false), 0, 100)),
            ["你会", "你好", "大", "大小", "字", "小", "换\n行"]
        );
        assert_eq!(
            texts(&get(sorted("", EntrySort::Text, true), 5, 100)),
            ["你好", "你会"],
            "降序的末页"
        );
        let s = get(sorted("da", EntrySort::Weight, false), 0, 100);
        assert_eq!((s.total, texts(&s)), (2, vec!["大小", "大"]), "命中内排序");
        let p = get(sorted("", EntrySort::Weight, false), 99, 10);
        assert!(p.rows.is_empty() && p.total == 7, "越界");
    }

    /// 排序结果按文件戳缓存：文件一变就重排。
    #[test]
    fn sort_cache_follows_file_changes() {
        let f = fixture("sortchange");
        let rel = "ct/main.dict.yaml";
        let get = || {
            f.m.dict_entries(rel, CT, &sorted("", EntrySort::Weight, true), 0, 10)
                .unwrap()
        };
        assert_eq!(texts(&get()), ["工", "子"]);
        assert_eq!(texts(&get()), ["工", "子"], "第二次走缓存");
        write(
            &f.base.join("schemas").join(rel),
            "---\nname: ct\n...\naa\t工\t1\nbb\t子\t5\ncc\t又\t9\n",
        );
        assert_eq!(texts(&get()), ["又", "子", "工"]);
    }

    fn hk(query: &str) -> HitsKey {
        HitsKey {
            tables: vec![(PathBuf::from("t"), (1, None))],
            lowercase: false,
            query: query.into(),
            mode: EntryMatch::Any,
            sort: None,
            desc: false,
        }
    }

    /// 单组超过行数上限：不缓存，也不挤掉已有的组。
    #[test]
    fn oversized_hits_are_not_cached() {
        let mut c = HitsCache::default();
        let now = Instant::now();
        c.put(hk("a"), Arc::new(vec![0; 10]), now);
        c.put(hk("b"), Arc::new(vec![0; HITS_CACHE_ROWS + 1]), now);
        assert!(c.get(&hk("b"), now).is_none());
        assert!(c.get(&hk("a"), now).is_some(), "已有的组还在");
        for q in ["c", "d", "e", "f"] {
            c.put(hk(q), Arc::new(vec![0; 10]), now);
        }
        assert_eq!(c.entries.len(), HITS_CACHE_SLOTS);
        assert!(c.get(&hk("a"), now).is_none(), "组数超了淘汰最久没用的");
    }

    /// 闲置超过时限，下次取放时整个清空；期间有访问就续期。
    #[test]
    fn hits_cache_clears_after_idle() {
        let mut c = HitsCache::default();
        let t0 = Instant::now();
        c.put(hk("a"), Arc::new(vec![1]), t0);
        let t1 = t0 + HITS_CACHE_IDLE - Duration::from_secs(1);
        assert!(c.get(&hk("a"), t1).is_some());
        let t2 = t1 + HITS_CACHE_IDLE - Duration::from_secs(1);
        assert!(c.get(&hk("a"), t2).is_some(), "访问续期");
        let t3 = t2 + HITS_CACHE_IDLE + Duration::from_secs(1);
        assert!(c.base(&hk("ab"), t3).is_none(), "闲置过期清空");
        assert_eq!(c.rows(), 0);
    }

    /// 英文库编码按小写读、码表按原样读：同一文件两种口径的排序结果各缓存一份，不串。
    #[test]
    fn sort_cache_keeps_lowercase_apart() {
        let f = fixture("sortcase");
        let rel = "ct/case.dict.yaml";
        write(
            &f.base.join("schemas").join(rel),
            "---\nname: case\n...\n乙\tB\t1\n甲\ta\t1\n",
        );
        let get = |ty: &str| {
            f.m.dict_entries(rel, ty, &sorted("", EntrySort::Code, false), 0, 10)
                .unwrap()
        };
        assert_eq!(texts(&get(CT)), ["乙", "甲"], "B < a");
        assert_eq!(texts(&get("english")), ["甲", "乙"], "a < b");
        assert_eq!(texts(&get(CT)), ["乙", "甲"]);
    }

    #[test]
    fn search_follows_file_changes() {
        let f = fixture("change");
        let rel = "py/sub/b.dict.yaml";
        assert_eq!(
            f.m.dict_entries(rel, PY, &eq("z", EntryMatch::Code), 0, 10)
                .unwrap()
                .total,
            1
        );
        write(
            &f.base.join("schemas/py/sub/b.dict.yaml"),
            "---\nname: b\ncolumns: [code, text]\n...\nzz\t字\nzy\t自\nzx\t在\n",
        );
        assert_eq!(
            f.m.dict_entries(rel, PY, &eq("z", EntryMatch::Code), 0, 10)
                .unwrap()
                .total,
            3
        );
        assert_eq!(f.m.dict_file_entries(rel, PY), Some(3));
    }

    /// 加载器丢的行这里也不算：列数不足、编码或词为空；`$SS(` 落在编码列的行纠正列序。
    #[test]
    fn rows_the_loader_drops_are_not_counted() {
        let f = fixture("lines");
        let rel = "ct/lines.dict.yaml";
        write(
            &f.base.join("schemas").join(rel),
            "---\nname: l\ncolumns: [text, code, weight]\n...\n甲\ta\t1\n只有一列\n\tb\t1\n乙\t\t1\n\
             zzgo\t$SS(gogo)\t1\n丙\tc\t2\n",
        );
        assert_eq!(f.m.dict_file_entries(rel, CT), Some(3));
        let p =
            f.m.dict_entries(rel, CT, &eq("", EntryMatch::Code), 0, 10)
                .unwrap();
        assert_eq!(texts(&p), ["甲", "$SS(gogo)", "丙"]);
        assert_eq!(p.rows[1].code, "zzgo", "列序按行纠正");
        let q =
            f.m.dict_entries(rel, CT, &eq("zzg", EntryMatch::Code), 0, 10)
                .unwrap();
        assert_eq!(texts(&q), ["$SS(gogo)"], "纠正列序的行按纠正后的编码搜得到");
    }

    /// `columns:` 声明缺 code 列：加载器整库跳过 ⇒ 条数为空、词条空并给说明。
    #[test]
    fn broken_columns_declaration_is_not_loaded() {
        let f = fixture("cols");
        let rel = "ct/bad.dict.yaml";
        write(
            &f.base.join("schemas").join(rel),
            "---\nname: bad\ncolumns:\n  - text\n  - weight\n...\n甲\t1\n",
        );
        assert_eq!(f.m.dict_file_entries(rel, CT), None);
        let p =
            f.m.dict_entries(rel, CT, &eq("", EntryMatch::Code), 0, 10)
                .unwrap();
        assert_eq!(p.total, 0);
        assert!(
            p.note.as_deref().unwrap().contains("columns"),
            "{:?}",
            p.note
        );
        assert_eq!(f.m.dict_header(rel, CT).unwrap().count, None);
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
        let all =
            f.m.dict_entries(rel, PY, &eq("", EntryMatch::Code), 0, 500)
                .unwrap();
        assert_eq!(all.total, 1000);
        for off in [0u64, 255, 256, 257, 511, 700, 999] {
            let p =
                f.m.dict_entries(rel, PY, &eq("", EntryMatch::Code), off, 7)
                    .unwrap();
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
            f.m.dict_entries("py/cr.dict.yaml", PY, &eq("", EntryMatch::Code), 1, 5)
                .unwrap();
        assert_eq!((p.total, texts(&p)), (3, vec!["乙", "丙"]));
    }

    fn order_of(toml_text: &str) -> Vec<(String, bool)> {
        let schema: Schema = toml::from_str(toml_text).unwrap();
        EngineManager::ordered_system_dicts(&schema)
            .into_iter()
            .map(|(d, m)| (d.id.clone(), m))
            .collect()
    }

    /// 码表：按 base_order 稳定排序，主库也按自己的 base_order 排（可以不在首）。
    #[test]
    fn codetable_order_follows_base_order_even_for_main() {
        let ids = order_of(
            "[engine]\ntype = \"codetable\"\n\
             [[dictionaries]]\nid = \"x2\"\npath = \"x2\"\nbase_order = 2\n\
             [[dictionaries]]\nid = \"m\"\npath = \"m\"\ndefault = true\nbase_order = 5\n\
             [[dictionaries]]\nid = \"x1a\"\npath = \"x1a\"\nbase_order = 1\n\
             [[dictionaries]]\nid = \"nopath\"\n\
             [[dictionaries]]\nid = \"user:我的\"\npath = \"/abs/u.yaml\"\nbase_order = -3\n\
             [[dictionaries]]\nid = \"x1b\"\npath = \"x1b\"\nbase_order = 1\n",
        );
        let ids: Vec<(&str, bool)> = ids.iter().map(|(i, m)| (i.as_str(), *m)).collect();
        assert_eq!(
            ids,
            [("x1a", false), ("x1b", false), ("x2", false), ("m", true)]
        );
    }

    /// 拼音：按方案文件顺序叠层，base_order 不起作用。
    #[test]
    fn pinyin_order_is_file_order_not_base_order() {
        let ids = order_of(
            "[engine]\ntype = \"pinyin\"\n\
             [[dictionaries]]\nid = \"m\"\npath = \"m\"\ntype = \"rime_pinyin\"\ndefault = true\nbase_order = 0\n\
             [[dictionaries]]\nid = \"late\"\npath = \"a\"\ntype = \"rime_pinyin\"\nbase_order = 9\n\
             [[dictionaries]]\nid = \"early\"\npath = \"b\"\ntype = \"rime_pinyin\"\nbase_order = 1\n",
        );
        let ids: Vec<(&str, bool)> = ids.iter().map(|(i, m)| (i.as_str(), *m)).collect();
        assert_eq!(ids, [("m", true), ("late", false), ("early", false)]);
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
        let n = m.dict_file_entries(rel, PY).unwrap();
        eprintln!("首次计数（建索引）{n} 条: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let n2 = m.dict_file_entries(rel, PY).unwrap();
        eprintln!("再次计数（命中缓存）{n2} 条: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let p = m
            .dict_entries(rel, PY, &eq("", EntryMatch::Code), 0, 100)
            .unwrap();
        eprintln!("首页: {:?} ({} 行)", t.elapsed(), p.rows.len());
        for off in [n / 2, n - 100] {
            let t = std::time::Instant::now();
            let p = m
                .dict_entries(rel, PY, &eq("", EntryMatch::Code), off, 100)
                .unwrap();
            eprintln!("第 {off} 条起一页: {:?} ({} 行)", t.elapsed(), p.rows.len());
        }
        for (q, mode) in [("ni", EntryMatch::Code), ("中国", EntryMatch::Text)] {
            let t = std::time::Instant::now();
            let p = m.dict_entries(rel, PY, &eq(q, mode), 0, 100).unwrap();
            eprintln!("搜索 {q:?} 首页: {:?}，命中 {}", t.elapsed(), p.total);
            let t = std::time::Instant::now();
            let _ = m
                .dict_entries(rel, PY, &eq(q, mode), p.total / 2, 100)
                .unwrap();
            eprintln!("搜索 {q:?} 翻到中间: {:?}", t.elapsed());
        }
        for (sort, name) in [
            (EntrySort::Weight, "权重"),
            (EntrySort::Code, "编码"),
            (EntrySort::Text, "词条"),
        ] {
            let t = std::time::Instant::now();
            let p = m
                .dict_entries(rel, PY, &sorted("", sort, true), 0, 100)
                .unwrap();
            eprintln!(
                "整库按{name}排序首页（首次）: {:?}，{} 条",
                t.elapsed(),
                p.total
            );
            let t = std::time::Instant::now();
            let _ = m
                .dict_entries(rel, PY, &sorted("", sort, true), p.total / 2, 100)
                .unwrap();
            eprintln!("整库按{name}排序翻到中间（缓存）: {:?}", t.elapsed());
        }
        eprintln!(
            "命中缓存 {} 行（{} 字节）",
            cached_hit_rows(),
            cached_hit_rows() * 8
        );
        for q in ["z", "zh", "zho", "zhon", "zhong", "中"] {
            let t = std::time::Instant::now();
            let p = m
                .dict_entries(rel, PY, &eq(q, EntryMatch::Any), 0, 100)
                .unwrap();
            eprintln!("逐键搜索 {q:?}: {:?}，命中 {}", t.elapsed(), p.total);
        }
        let cps: usize = m
            .dict_tables(rel, PY)
            .unwrap()
            .iter()
            .map(|(_, _, i)| i.checkpoints.len())
            .sum();
        eprintln!("检查点 {cps} 个（{} 字节）", cps * 8);
        assert!(n > 600_000);
    }
}
