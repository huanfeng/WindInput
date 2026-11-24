//! `.wemj`：mmap 零拷贝的 emoji 扩展表（词 → emoji 列表）
//!
//! 「打出词组后在候选里追加对应 emoji」的查表数据，本模块只负责存储格式与构建，不含候选逻辑。
//! 设计见 `docs/design/emoji-suggestion.md`（§3.4 为什么不进内存表，§4 为什么另建格式）。
//! 骨架取自 [`crate::commentdict`]（排序数组 + 二分，只做精确点查），数据语义独立。
//!
//! # 文件布局
//!
//! - Header (24B)：magic `WEMJ` + version u32 + entry_count u32 + index_off u32
//!   + str_off u32 + reserved u32
//! - Entry[entry_count] (8B 每条，按 text 升序)：off u32（相对 str_off）
//!   + text_len u16 + emoji_len u16
//! - StringPool：每条连续存 `text | emojis`，`emojis` 为空格分隔的 UTF-8
//!
//! 与 `.wcmt` 不同，同一 text 至多一条：重复键在 [`write_emoji_wemj`] 构建时已合并，
//! 查询端不需要组迭代。

use crate::mapping::Mapped;
use std::fs::File;
use std::path::Path;
use tracing::{info, warn};

const MAGIC: [u8; 4] = *b"WEMJ";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 24;
const ENTRY_SIZE: usize = 8;

/// emoji 扩展表 mmap 读取器。
pub struct EmojiReader {
    mmap: Mapped,
    entry_count: u32,
    index_off: u32,
    str_off: u32,
}

/// 一条记录的两段文本（借用自 mmap，零拷贝）。
struct Row<'a> {
    text: &'a str,
    emojis: &'a str,
}

impl EmojiReader {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let mmap = crate::mapping::map_file(&file)?;
        if mmap.len() < HEADER_SIZE {
            anyhow::bail!("emoji wemj too short");
        }
        if mmap[0..4] != MAGIC {
            anyhow::bail!("invalid emoji magic");
        }
        let version = u32::from_le_bytes(mmap[4..8].try_into().unwrap());
        if version != VERSION {
            anyhow::bail!("unsupported emoji version: {}", version);
        }
        let entry_count = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        let index_off = u32::from_le_bytes(mmap[12..16].try_into().unwrap());
        let str_off = u32::from_le_bytes(mmap[16..20].try_into().unwrap());

        let index_end = index_off as usize + entry_count as usize * ENTRY_SIZE;
        if index_end > mmap.len() || str_off as usize > mmap.len() {
            anyhow::bail!("emoji wemj offsets out of range");
        }
        info!(
            "Opened emoji dict: {} ({} entries)",
            path.display(),
            entry_count
        );
        Ok(Self {
            mmap,
            entry_count,
            index_off,
            str_off,
        })
    }

    pub fn entry_count(&self) -> u32 {
        self.entry_count
    }

    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// 读第 `i` 条的两段文本。越界 / UTF-8 损坏 → None（当作该条不存在，不 panic：
    /// 缓存文件可能被外部破坏或半写，功能降级好过崩进程。同 `.wcmt` 的处置）。
    fn row(&self, i: u32) -> Option<Row<'_>> {
        let off = self.index_off as usize + i as usize * ENTRY_SIZE;
        if off + ENTRY_SIZE > self.mmap.len() {
            return None;
        }
        let str_start = u32::from_le_bytes(self.mmap[off..off + 4].try_into().ok()?) as usize;
        let text_len = u16::from_le_bytes(self.mmap[off + 4..off + 6].try_into().ok()?) as usize;
        let emoji_len = u16::from_le_bytes(self.mmap[off + 6..off + 8].try_into().ok()?) as usize;

        let base = self.str_off as usize + str_start;
        let text_end = base + text_len;
        let emoji_end = text_end + emoji_len;
        if emoji_end > self.mmap.len() {
            return None;
        }
        Some(Row {
            text: std::str::from_utf8(&self.mmap[base..text_end]).ok()?,
            emojis: std::str::from_utf8(&self.mmap[text_end..emoji_end]).ok()?,
        })
    }

    /// 首个 `text` 不小于给定值的下标（lower bound）。
    ///
    /// 条目损坏时按「不小于」处理（收缩 hi）：宁可查不到也不要死循环或越界。
    fn lower_bound(&self, text: &str) -> u32 {
        let (mut lo, mut hi) = (0u32, self.entry_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.row(mid) {
                Some(r) if r.text < text => lo = mid + 1,
                _ => hi = mid,
            }
        }
        lo
    }

    /// 查该词的 emoji 串（空格分隔，调用方按需 `split_whitespace`）。
    ///
    /// 返回原始串而非 `Vec<String>`：热路径上只用得到前 `max_per_word` 个，由调用方惰性切分以免分配。
    pub fn lookup(&self, text: &str) -> Option<&str> {
        let r = self.row(self.lower_bound(text))?;
        if r.text == text && !r.emojis.is_empty() {
            Some(r.emojis)
        } else {
            None
        }
    }
}

/// 构建 `.wemj`：排序 + 同键合并 + 写盘（tmp + rename 原子替换）。
///
/// 同键必须合并（保序去重，先到的排前面），不能覆盖：两张上游表的同键内容各不相同，覆盖会
/// 整组丢数据；繁→简撞键今天恰好无害只是数据巧合。见 `docs/design/emoji-suggestion.md` §3.6。
///
/// 排序必须用稳定的 `sort_by`，否则「先到先得」会随输入规模抖动（小数组恰好稳定，测试抓不到），
/// 与 [`crate::commentdict::write_comment_wcmt`] 同理。
pub fn write_emoji_wemj(
    path: impl AsRef<Path>,
    rows: &[(String, Vec<String>)],
) -> anyhow::Result<()> {
    use std::io::Write;

    let mut sorted: Vec<&(String, Vec<String>)> = rows.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let mut pool: Vec<u8> = Vec::new();
    let mut index: Vec<u8> = Vec::with_capacity(sorted.len() * ENTRY_SIZE);
    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut merged = 0usize;

    let mut i = 0usize;
    while i < sorted.len() {
        // 同 text 组：[i, j)
        let mut j = i;
        while j < sorted.len() && sorted[j].0 == sorted[i].0 {
            j += 1;
        }
        if j - i > 1 {
            merged += 1;
        }
        // 组内合并 emoji：保序去重
        let text = sorted[i].0.as_str();
        let mut emojis: Vec<&str> = Vec::new();
        for (_, list) in sorted[i..j].iter().copied() {
            for e in list {
                let e = e.as_str();
                if !e.is_empty() && !emojis.contains(&e) {
                    emojis.push(e);
                }
            }
        }
        // ⚠️ 必须在下面任何 `continue` 之前推进，否则空条目会让循环卡死。
        i = j;

        if text.is_empty() || emojis.is_empty() {
            continue;
        }
        let joined = emojis.join(" ");
        if text.len() > u16::MAX as usize || joined.len() > u16::MAX as usize {
            skipped += 1;
            continue;
        }
        if pool.len() > u32::MAX as usize {
            anyhow::bail!("emoji dict string pool exceeds 4GB");
        }
        let off = pool.len() as u32;
        pool.extend_from_slice(text.as_bytes());
        pool.extend_from_slice(joined.as_bytes());
        index.extend_from_slice(&off.to_le_bytes());
        index.extend_from_slice(&(text.len() as u16).to_le_bytes());
        index.extend_from_slice(&(joined.len() as u16).to_le_bytes());
        written += 1;
    }
    if skipped > 0 {
        warn!("emoji 表有 {} 条词条/值超长（>64KB），已跳过", skipped);
    }

    let index_off = HEADER_SIZE as u32;
    let str_off = index_off + index.len() as u32;

    let tmp = path.as_ref().with_extension("wemj.tmp");
    if let Some(dir) = tmp.parent() {
        std::fs::create_dir_all(dir)?;
    }
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&MAGIC)?;
        f.write_all(&VERSION.to_le_bytes())?;
        f.write_all(&(written as u32).to_le_bytes())?;
        f.write_all(&index_off.to_le_bytes())?;
        f.write_all(&str_off.to_le_bytes())?;
        f.write_all(&0u32.to_le_bytes())?; // reserved
        f.write_all(&index)?;
        f.write_all(&pool)?;
    }
    // 见 `reader_pool::replacing`：池中可能还有指向替换前数据的 mmap reader。
    let replacing = crate::reader_pool::replacing(path.as_ref());
    let renamed = std::fs::rename(&tmp, path.as_ref());
    drop(replacing);
    renamed?;
    info!(
        "Wrote emoji dict: {} entries ({} merged by key)",
        written, merged
    );
    Ok(())
}

/// 解析上游 rime-emoji 词表（`emoji_word.txt` / `emoji_category.txt`），键经 `normalize` 归一。
///
/// 上游行形如 `键<TAB>键 emoji1 emoji2 …`，值的首项恒等于键（OpenCC simplifier 的约定），
/// 故丢弃值的首项。判据必须是位置而不是内容：归一后键已是简体、首项仍是繁体，按「等于键」
/// 过滤会把首项当成 emoji 收进去。
///
/// `normalize` 由调用方注入（繁→简），因为本 crate 不依赖 `wind-transform`；传恒等闭包即不归一。
pub fn parse_upstream(
    content: &str,
    normalize: impl Fn(&str) -> String,
) -> Vec<(String, Vec<String>)> {
    // 行尾规整：孤立 \r 会让整份表算成一行，`#` 开头的表整个被当成注释跳过 → 0 条
    let normalized = wind_utils::text::normalize_input(content);
    let content = normalized.as_ref();
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(tab) = line.find('\t') else {
            continue;
        };
        let key = &line[..tab];
        if key.is_empty() {
            continue;
        }
        // 值区按空格切分，跳过首项（上游契约：首项 == 键）。
        let emojis: Vec<String> = line[tab + 1..]
            .split(' ')
            .filter(|v| !v.is_empty())
            .skip(1)
            .map(str::to_string)
            .collect();
        if emojis.is_empty() {
            continue;
        }
        out.push((normalize(key), emojis));
    }
    out
}

/// 一般 OpenCC 文本表（`键<Tab>值1 值2 …`）：键不归一，值首项等于键时丢弃（rime 生态
/// 「首项重复键」的约定，见 `docs/design/opencc-conversions.md` §2），否则全收。
///
/// 与 [`parse_upstream`] 的分别只在丢首项的判据：那边的键要归一、归一后与首项不再相等，
/// 只能按位置丢；这边键不变，按内容判才能同时兼容不带重复首项的用户表。
pub fn parse_opencc(content: &str) -> Vec<(String, Vec<String>)> {
    let normalized = wind_utils::text::normalize_input(content);
    let mut out = Vec::new();
    for line in normalized.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("---") {
            continue;
        }
        let Some((key, vals)) = line.split_once('\t') else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        let mut it = vals.split(' ').filter(|v| !v.is_empty()).peekable();
        if it.peek() == Some(&key) {
            it.next();
        }
        let vals: Vec<String> = it.map(str::to_string).collect();
        if !vals.is_empty() {
            out.push((key.to_string(), vals));
        }
    }
    out
}

/// 只取 OpenCC 文本表里指定分组的行（rime-frost `others.txt` 那种按分组拼起来的表）。
///
/// 分组标题行形如 `----------偏旁部首\t----------`：OpenCC 文本词典没有注释语法，rime-frost
/// 借「键不可重复」把标题写成一条键、值都是破折号的假词条。标题之后到下一个标题之前的行
/// 归该组；第一个标题之前的行不属于任何组，丢弃。`----------（太短了不启用了）谬\t谬 mu μ`
/// 这种值不是破折号的行是被注释掉的词条，不是标题，也丢弃。
///
/// `sections` 为空时原样返回整份内容（不分组的表）。
pub fn select_sections<'a>(content: &'a str, sections: &[String]) -> std::borrow::Cow<'a, str> {
    // 分组标题行靠整行匹配认出：孤立 \r 不归一的话整份文件是一行，一个分组都认不出。
    let normalized = wind_utils::text::normalize_input(content);
    if sections.is_empty() {
        return normalized;
    }
    let mut out = String::new();
    let mut keep = false;
    for line in normalized.lines() {
        if let Some(rest) = line.strip_prefix("----------") {
            if let Some((name, val)) = rest.split_once('\t')
                && val.trim().chars().all(|c| c == '-')
                && !val.trim().is_empty()
            {
                keep = sections.iter().any(|s| s == name.trim());
            }
            continue;
        }
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    std::borrow::Cow::Owned(out)
}

/// 加载 emoji 扩展表：优先 mmap `.wemj` 缓存，不新鲜则从上游 txt 重建。
///
/// - `tables`：要解析的上游文本表，按顺序合并，顺序即优先级（同键时精确词表的 emoji 在前）。
///   不存在的文件跳过（分类表可选）。
/// - `fp_extra`：只参与指纹、不参与解析的源。归一表（`TSCharactersDerived.octrie`）必须放这里，
///   漏传会让 OpenCC 数据升级后旧缓存被永久复用（见 [`cache_fp::EMOJI_TAG`]）。
/// - `sections`：只取这些分组（见 [`select_sections`]），空 = 整表。分组决定解析结果却不在
///   源文件字节里，故并入指纹 tag，同一份 `others.txt` 拆出的各模块缓存互不冒认。
/// - `normalize`：`Some` = rime-emoji 上游表（键是繁体），按位置丢值首项再归一键，见
///   [`parse_upstream`]；`None` = 一般 OpenCC 表，键不归一，值首项等于键时才丢，见 [`parse_opencc`]。
///
/// 缓存建不了（目录只读、磁盘满）时不降级成内存表，而是让功能不可用并告警：这是可选功能，
/// 不该常驻内存（见 `docs/design/emoji-suggestion.md` §3.4）。
pub fn load_or_build(
    tables: &[std::path::PathBuf],
    fp_extra: &[std::path::PathBuf],
    cache_file: &Path,
    sections: &[String],
    normalize: Option<&dyn Fn(&str) -> String>,
) -> Option<std::sync::Arc<EmojiReader>> {
    use crate::{cache_fp, reader_pool};
    // 解析规则（rime-emoji / OpenCC）同样决定产出，并入 tag。
    let rule = if normalize.is_some() { "" } else { "~opencc" };
    let tag = if sections.is_empty() {
        format!("{}{rule}", cache_fp::EMOJI_TAG)
    } else {
        format!("{}{rule}#{}", cache_fp::EMOJI_TAG, sections.join("|"))
    };
    let tag = tag.as_str();

    // 指纹覆盖「解析的源」+「影响解析结果的源」两部分，缺一都会让缓存该重建时不重建。
    let fp_sources: Vec<&Path> = tables
        .iter()
        .chain(fp_extra.iter())
        .map(std::path::PathBuf::as_path)
        .collect();

    // single-flight：同一缓存文件的构建区间互斥（同 `load_comment_source`）。
    let lock = reader_pool::file_lock(cache_file);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

    if cache_fp::cache_is_fresh(cache_file, &fp_sources, tag)
        && let Ok(r) = reader_pool::open_emoji(cache_file)
    {
        return Some(r);
    }

    let mut rows: Vec<(String, Vec<String>)> = Vec::new();
    for t in tables {
        if !t.exists() {
            continue; // 分类表可选，缺了不算失败
        }
        match std::fs::read_to_string(t) {
            Ok(s) => {
                let body = select_sections(&s, sections);
                rows.extend(match normalize {
                    Some(n) => parse_upstream(&body, n),
                    None => parse_opencc(&body),
                })
            }
            Err(e) => warn!("读取 emoji 表失败 {}: {}", t.display(), e),
        }
    }
    if rows.is_empty() {
        warn!("emoji 表为空或全部读取失败，扩展功能不可用");
        return None;
    }

    if let Err(e) = write_emoji_wemj(cache_file, &rows) {
        warn!("emoji 缓存构建失败 {}: {}", cache_file.display(), e);
        return None;
    }
    cache_fp::write_cache_fp(cache_file, &fp_sources, tag);
    match reader_pool::open_emoji(cache_file) {
        Ok(r) => Some(r),
        Err(e) => {
            warn!("emoji 缓存写成但打不开 {}: {}", cache_file.display(), e);
            None
        }
    }
}

#[cfg(test)]
#[path = "emojidict_tests.rs"]
mod tests;
