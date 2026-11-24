//! 英文词组分词输入：分词符切段，每段只打前缀。
//!
//! `envi'deg` → `environmental degradation`，`ip'max` → `iPhone 15 Pro Max`。
//! 价值在共同前缀下精确定位（词组本就能靠前缀召回，只是要翻页）。
//!
//! 查询读 `text` 不读 `code`：词库里有「共同前缀式」编码（`iPhone 15 Pro Max` → `iphone`），
//! code 不含后段词，词边界只在 text 的空格里。代价是不能靠 Trie 剪枝，故索引按首词排序、
//! 查询两次二分切出窗口（[`PhraseSegIndex::first_word_range`]，量级见
//! `bench_window_vs_full_scan`）。设计见 `docs/architecture/engine-candidate-pipeline.md` §6.1。

use wind_candidate::{Candidate, CandidateSource};

/// 词组分词符。
///
/// 不做成可配项，理由见 `EnglishGlobal::phrase_seg` 的文档。
pub const PHRASE_SEPARATOR: char = '\'';

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, RwLock};
use wind_dict::DictManager;

/// 索引里的一条词组：只存偏移，字符串都在 [`PhraseSegIndex`] 的 arena 里（大词库下逐条
/// 独立分配的头部开销远超数据本身）。
///
/// 它是镜像里一条定长记录（[`ENTRY_SIZE`] 字节、七个小端字段按声明序）的解码值，读侧逐字段
/// `from_le_bytes`，不依赖对齐、不需要 `unsafe`，故不加 `#[repr(C)]`。
#[derive(Clone, Copy)]
struct PhraseEntry {
    /// 本条小写词序列在 `lower` 中的起点（第 0 个词的起点）。
    lower_start: u32,
    /// 原文与编码在 `raw` 中的起点：原文在前，编码紧随其后，两者不留分隔。
    raw_start: u32,
    text_len: u32,
    code_len: u32,
    /// 本条各词的结束偏移在 `word_ends` 中的起点。
    words_start: u32,
    /// 词数，恒 ≥ 2（单词条目不进索引）。
    word_count: u32,
    weight: i32,
}

/// 一条 [`PhraseEntry`] 在镜像里的字节数（7 × 4）。
const ENTRY_SIZE: usize = 28;

const MAGIC: [u8; 4] = *b"WPHR";
/// 镜像**布局**版本。只挡布局变化；同布局下的语义变化（收词判据、小写化规则）靠
/// [`wind_dict::cache_fp::ENGLISH_PHRASE_INDEX_TAG`] 换文件名，理由见那里。
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 64;
/// 文件头里指纹（16 个十六进制字符）的位置。进程内建的镜像这一段全零。
const FP_OFF: usize = 40;
const FP_LEN: usize = 16;

/// 落盘文件的扩展名（`EngineManager` 的缓存清理白名单也认它）。
pub const CACHE_EXT: &str = "wphr";

/// 镜像「小到可以直接读进内存」的上限（3 MB），超过就 mmap。
///
/// 取值与理由同 `REVERSE_INDEX_RESIDENT_MAX`（`manager.rs`）：小镜像常驻比建映射快，大镜像
/// 常驻只多付私有内存。两条路同一份字节、同一套查找代码，这个值只影响性能、不影响结果。
pub const PHRASE_INDEX_RESIDENT_MAX: usize = 3 * 1024 * 1024;

/// 同一目录下最多留几份 `.wphr`（按 mtime，命中时会刷新）。
///
/// 指纹进文件名，每种启用集合各一份，旧组合不会自己消失，故设上限。4 份够覆盖
/// 「开着 / 关掉某本 / 再关一本」的来回切换。
const PHRASE_INDEX_CACHE_KEEP: usize = 4;

/// 镜像字节的来源：进程内构建产物，或磁盘文件的映射。对上层完全等价，差别只在这些字节
/// 算不算进程私有内存（同 `wind_dict::reverseidx` 的 `IndexData`）。
enum ImageData {
    Owned(Vec<u8>),
    Mapped(wind_dict::mapping::Mapped),
}

impl ImageData {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v,
            Self::Mapped(m) => m,
        }
    }
}

/// 英文词组分词索引：只收 `text` 含空白的词条。**只读视图**——堆上建的与盘上映射的是
/// 同一种东西：一份字节镜像 + 各段偏移，查询代码只有一套。
///
/// 构建是 O(全表) 的，不能出现在按键链路上，故由 [`LazyPhraseIndex`] 守着 + 后台预热 +
/// 落盘复用。
///
/// # 三块 arena
///
/// | | 存什么 | 谁读 |
/// |---|---|---|
/// | `lower` | 各词小写化后**首尾相接**（不留分隔符） | 匹配 |
/// | `raw` | 原文 + 编码首尾相接 | 产出候选 |
/// | `word_ends` | 每个词在 `lower` 中的结束偏移 | 切词 |
///
/// # 镜像布局（`.wphr`，全部小端，各段起点 4 字节对齐）
///
/// - Header（64 B）：magic `WPHR` + version + entry_count + entries_off + word_ends_off
///   + word_ends_count + lower_off + lower_len + raw_off + raw_len（各 u32）+ 指纹 16 B
///   + 保留 8 B
/// - entries：`entry_count` × [`ENTRY_SIZE`]，**已按首词字节序排好**（二分的前提）
/// - word_ends：`word_ends_count` × u32
/// - lower、raw：两块 arena 原样
///
/// 读侧零拷贝地按偏移取；对齐只为整齐，读法不依赖它。
///
/// 有序是构造保证：镜像只由 [`PhraseSegBuilder`] 排序后产出，本类型不提供收词入口。
pub struct PhraseSegIndex {
    data: ImageData,
    entry_count: usize,
    entries_off: usize,
    word_ends_off: usize,
    word_ends_count: usize,
    lower_off: usize,
    lower_len: usize,
    raw_off: usize,
    raw_len: usize,
}

impl Default for PhraseSegIndex {
    /// 空索引，所有查询返回空。不走「序列化空 builder 再解析」：默认值必须无条件成功。
    fn default() -> Self {
        Self {
            data: ImageData::Owned(Vec::new()),
            entry_count: 0,
            entries_off: 0,
            word_ends_off: 0,
            word_ends_count: 0,
            lower_off: 0,
            lower_len: 0,
            raw_off: 0,
            raw_len: 0,
        }
    }
}

/// 建表期的可写形态：收词条、排序、摊平成镜像。查询只在 [`PhraseSegIndex`] 上做。
#[derive(Default)]
struct PhraseSegBuilder {
    lower: String,
    raw: String,
    word_ends: Vec<u32>,
    entries: Vec<PhraseEntry>,
}

impl PhraseSegBuilder {
    /// 收一条词条；不足两个词（按 text 的空白切）的原样回滚。先写 arena 再回滚，免得对全表
    /// 每条都多切一次。
    fn push(&mut self, code: &str, text: &str, weight: i32) {
        let lower_start = self.lower.len() as u32;
        let words_start = self.word_ends.len() as u32;
        let mut word_count = 0u32;
        for w in text.split_whitespace() {
            // 与查询侧同源，见 [`lower`]。
            self.lower.push_str(&lower(w));
            self.word_ends.push(self.lower.len() as u32);
            word_count += 1;
        }
        if word_count < 2 {
            self.lower.truncate(lower_start as usize);
            self.word_ends.truncate(words_start as usize);
            return;
        }
        let raw_start = self.raw.len() as u32;
        self.raw.push_str(text);
        self.raw.push_str(code);
        self.entries.push(PhraseEntry {
            lower_start,
            raw_start,
            text_len: text.len() as u32,
            code_len: code.len() as u32,
            words_start,
            word_count,
            weight,
        });
    }

    /// 首词的小写形态（排序键）。词在 arena 里首尾相接，首词从 `lower_start` 起。
    fn first_word(&self, e: &PhraseEntry) -> &str {
        let end = self.word_ends[e.words_start as usize] as usize;
        &self.lower[e.lower_start as usize..end]
    }

    /// 按首词排序，是 [`PhraseSegIndex::first_word_range`] 的前提。测试夹具也须经
    /// [`Self::finish`] 走这里，别自带一份排序/收词判据。
    ///
    /// 稳定排序：同一份词库两次建出的镜像逐字节相同（堆版与盘上那份对拍靠这一点）。
    fn sort(&mut self) {
        // 比较闭包要借 `self`，故先把 `entries` 取出来排完再放回。
        let mut entries = std::mem::take(&mut self.entries);
        entries.sort_by(|a, b| self.first_word(a).cmp(self.first_word(b)));
        self.entries = entries;
        debug_assert!(
            self.entries
                .windows(2)
                .all(|w| self.first_word(&w[0]) <= self.first_word(&w[1])),
            "排序之后 entries 仍不是按首词有序 —— 比较键写错了"
        );
    }

    /// 排序 + 摊平成进程内镜像（指纹段留零）。测试夹具的收尾——与生产（`PhraseSegIndex::scan`
    /// → `into_image` / `write_image`）走同一个 `sort` 与同一份布局代码。
    #[cfg(test)]
    fn finish(mut self) -> Vec<u8> {
        self.sort();
        self.into_image("")
    }

    /// 各段起点与总长：`(entries_off, word_ends_off, lower_off, raw_off, total)`。
    /// 28 × n 恒为 4 的倍数，word_ends 天然对齐；lower 在 u32 数组之后，同样对齐。
    fn layout(&self) -> (usize, usize, usize, usize, usize) {
        let align4 = |n: usize| (n + 3) & !3;
        let entries_off = HEADER_SIZE;
        let word_ends_off = entries_off + self.entries.len() * ENTRY_SIZE;
        let lower_off = word_ends_off + self.word_ends.len() * 4;
        let raw_off = align4(lower_off + self.lower.len());
        let total = align4(raw_off + self.raw.len());
        (entries_off, word_ends_off, lower_off, raw_off, total)
    }

    /// 三块 arena + entries 原样摊平写出（布局见 [`PhraseSegIndex`]），`fp` 写进文件头
    /// （空串 = 进程内镜像，指纹段留零）。须已 [`Self::sort`]。
    ///
    /// 落盘走这里直接写文件，不先拼完整镜像，否则建表峰值多出一整张表。
    fn write_image(&self, fp: &str, w: &mut impl std::io::Write) -> std::io::Result<()> {
        let (entries_off, word_ends_off, lower_off, raw_off, total) = self.layout();
        let mut header = [0u8; HEADER_SIZE];
        header[..4].copy_from_slice(&MAGIC);
        for (k, v) in [
            VERSION,
            self.entries.len() as u32,
            entries_off as u32,
            word_ends_off as u32,
            self.word_ends.len() as u32,
            lower_off as u32,
            self.lower.len() as u32,
            raw_off as u32,
            self.raw.len() as u32,
        ]
        .into_iter()
        .enumerate()
        {
            header[4 + k * 4..8 + k * 4].copy_from_slice(&v.to_le_bytes());
        }
        let fp = fp.as_bytes();
        let n = fp.len().min(FP_LEN);
        header[FP_OFF..FP_OFF + n].copy_from_slice(&fp[..n]);
        w.write_all(&header)?;
        for e in &self.entries {
            let mut rec = [0u8; ENTRY_SIZE];
            for (k, v) in [
                e.lower_start,
                e.raw_start,
                e.text_len,
                e.code_len,
                e.words_start,
                e.word_count,
                e.weight as u32,
            ]
            .into_iter()
            .enumerate()
            {
                rec[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            w.write_all(&rec)?;
        }
        for v in &self.word_ends {
            w.write_all(&v.to_le_bytes())?;
        }
        w.write_all(self.lower.as_bytes())?;
        w.write_all(&[0u8; 3][..raw_off - (lower_off + self.lower.len())])?;
        w.write_all(self.raw.as_bytes())?;
        w.write_all(&[0u8; 3][..total - (raw_off + self.raw.len())])
    }

    /// 摊平成内存镜像。`fp` 同 [`Self::write_image`]。
    fn into_image(self, fp: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.layout().4);
        // 写 `Vec` 不会失败。
        let _ = self.write_image(fp, &mut out);
        debug_assert_eq!(out.len(), self.layout().4);
        out
    }
}

/// 对 `[lo, hi)` 做 `slice::partition_point` 同语义的二分：返回第一个使 `pred` 为假的下标
/// （前提是 `pred` 在区间上先真后假）。镜像里的条目是按需解码的，没有现成切片可调。
fn partition_point(mut lo: usize, mut hi: usize, pred: impl Fn(usize) -> bool) -> usize {
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

impl PhraseSegIndex {
    /// 全表扫一次，挑出词组建索引（进程内，不落盘）。
    pub fn build(dm: &DictManager) -> Self {
        Self::from_image(Self::scan(dm).into_image(""))
    }

    /// 全表扫一次，收齐词组并排好序。
    fn scan(dm: &DictManager) -> PhraseSegBuilder {
        let mut b = PhraseSegBuilder::default();
        dm.for_each_entry(&mut |code, text, weight| b.push(code, text, weight));
        b.sort();
        b
    }

    /// 从进程内建好的镜像构造（常驻堆）。镜像出自 [`PhraseSegBuilder::into_image`]，解析不会
    /// 失败；真失败了也只降级成空索引，不 panic（查询在按键链路上）。
    fn from_image(image: Vec<u8>) -> Self {
        match Self::parse(ImageData::Owned(image)) {
            Ok(idx) => idx,
            Err(e) => {
                debug_assert!(false, "自产镜像解析失败：{e}");
                tracing::warn!("英文词组索引镜像无法解析（{e}），本次按空索引处理");
                Self::default()
            }
        }
    }

    /// 打开盘上的 `.wphr`：不超过 `resident_max` 字节就整份读进内存，否则 mmap；
    /// mmap 失败降级为读进内存（文件本身是好的，不必重扫）。
    ///
    /// 文件头指纹必须等于 `expect_fp`；格式 / 版本 / 指纹不符、截断一律 `Err`，由调用方重建。
    fn open(path: &Path, expect_fp: &str, resident_max: usize) -> anyhow::Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len() as usize;
        let data = if len <= resident_max {
            ImageData::Owned(std::fs::read(path)?)
        } else {
            // SAFETY: 映射期间文件内容不得被改写。本模块只经「临时文件 + rename」替换，
            // 从不原地写，被映射的那份 inode 内容不变；外部篡改的风险与 `.wdat`/`.wridx`
            // 的 mmap 相同。
            match wind_dict::mapping::map_file(&file) {
                Ok(m) => ImageData::Mapped(m),
                Err(e) => {
                    tracing::warn!(
                        "英文词组索引 mmap 失败 {}（{e}），退回读进内存",
                        path.display()
                    );
                    ImageData::Owned(std::fs::read(path)?)
                }
            }
        };
        let idx = Self::parse(data)?;
        if idx.bytes()[FP_OFF..FP_OFF + FP_LEN] != *expect_fp.as_bytes() {
            anyhow::bail!("wphr 指纹不符");
        }
        Ok(idx)
    }

    /// 校验头部与各段边界。段越界（文件被截断）、magic / 版本不符一律 `Err`。
    ///
    /// 校验之后 [`Self::entry`] 与两块 arena 的切片不可能越界；段**内**的偏移（词边界、
    /// 原文起点）仍逐次 `get` 兜底——损坏的条目只是查不中，不 panic。
    fn parse(data: ImageData) -> anyhow::Result<Self> {
        let d = data.as_slice();
        if d.len() < HEADER_SIZE {
            anyhow::bail!("wphr 短于文件头（{} B）", d.len());
        }
        if d[0..4] != MAGIC {
            anyhow::bail!("wphr magic 不符");
        }
        let rd = |off: usize| u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]]);
        let version = rd(4);
        if version != VERSION {
            anyhow::bail!("wphr 版本 {version} 不是 {VERSION}");
        }
        let entry_count = rd(8) as usize;
        let entries_off = rd(12) as usize;
        let word_ends_off = rd(16) as usize;
        let word_ends_count = rd(20) as usize;
        let lower_off = rd(24) as usize;
        let lower_len = rd(28) as usize;
        let raw_off = rd(32) as usize;
        let raw_len = rd(36) as usize;
        let fits = |off: usize, len: Option<usize>| {
            len.and_then(|l| off.checked_add(l))
                .is_some_and(|end| off >= HEADER_SIZE && end <= d.len())
        };
        if !fits(entries_off, entry_count.checked_mul(ENTRY_SIZE))
            || !fits(word_ends_off, word_ends_count.checked_mul(4))
            || !fits(lower_off, Some(lower_len))
            || !fits(raw_off, Some(raw_len))
        {
            anyhow::bail!("wphr 段越界（文件被截断？）");
        }
        // 总长须恰好等于写出的长度：只验各段在界内挡不住截在尾部填充里的截断。
        if raw_off.checked_add(raw_len).map(|e| (e + 3) & !3) != Some(d.len()) {
            anyhow::bail!("wphr 总长 {} 与段表不符（文件被截断？）", d.len());
        }
        Ok(Self {
            data,
            entry_count,
            entries_off,
            word_ends_off,
            word_ends_count,
            lower_off,
            lower_len,
            raw_off,
            raw_len,
        })
    }

    fn bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// 第 `i` 条（`i < entry_count`，`parse` 已保证整段在界内）。
    fn entry(&self, i: usize) -> PhraseEntry {
        let o = self.entries_off + i * ENTRY_SIZE;
        let b = &self.bytes()[o..o + ENTRY_SIZE];
        let f = |k: usize| u32::from_le_bytes([b[k], b[k + 1], b[k + 2], b[k + 3]]);
        PhraseEntry {
            lower_start: f(0),
            raw_start: f(4),
            text_len: f(8),
            code_len: f(12),
            words_start: f(16),
            word_count: f(20),
            weight: f(24) as i32,
        }
    }

    /// `word_ends[k]`；越界（镜像损坏）为 `None`。
    fn word_end(&self, k: usize) -> Option<usize> {
        if k >= self.word_ends_count {
            return None;
        }
        let o = self.word_ends_off + k * 4;
        let b = &self.bytes()[o..o + 4];
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    }

    fn lower_arena(&self) -> &[u8] {
        &self.bytes()[self.lower_off..self.lower_off + self.lower_len]
    }

    fn raw_arena(&self) -> &[u8] {
        &self.bytes()[self.raw_off..self.raw_off + self.raw_len]
    }

    /// 第 `j` 个词的小写形态（字节）。`j` 必须 `< e.word_count`；镜像损坏时给空切片。
    ///
    /// 词在 arena 里首尾相接，上一个词的 end 就是这一个的 start。给字节而非 `&str`：
    /// 前缀与字典序比较本就逐字节，省掉 UTF-8 校验（mmap 的字节不能假定合法）。
    fn word(&self, e: &PhraseEntry, j: usize) -> &[u8] {
        let ws = e.words_start as usize;
        let start = if j == 0 {
            Some(e.lower_start as usize)
        } else {
            self.word_end(ws.saturating_add(j - 1))
        };
        match (start, self.word_end(ws.saturating_add(j))) {
            (Some(s), Some(t)) => self.lower_arena().get(s..t).unwrap_or(&[]),
            _ => &[],
        }
    }

    /// `raw` 里从 `start` 起 `len` 字节的串；越界或非 UTF-8（镜像损坏）为 `None`。
    fn raw_str(&self, start: usize, len: usize) -> Option<&str> {
        let b = self.raw_arena().get(start..start.checked_add(len)?)?;
        std::str::from_utf8(b).ok()
    }

    /// 原文（带大小写与空格），上屏用。
    fn text(&self, e: &PhraseEntry) -> Option<&str> {
        self.raw_str(e.raw_start as usize, e.text_len as usize)
    }

    /// 词库里的原始编码。只为填进候选供调试段显示，匹配不读它。
    fn code(&self, e: &PhraseEntry) -> Option<&str> {
        let start = (e.raw_start as usize).checked_add(e.text_len as usize)?;
        self.raw_str(start, e.code_len as usize)
    }

    /// 首词以 `prefix` 开头的那一段（条目下标区间）。`entries` 按首词有序，故这些条目
    /// 必然连续。
    ///
    /// 匹配规则 1 要求首段是第一个词的前缀（见 [`Self::search`]），区间外的条目不可能命中。
    fn first_word_range(&self, prefix: &str) -> Range<usize> {
        let p = prefix.as_bytes();
        let n = self.entry_count;
        let lo = partition_point(0, n, |i| self.word(&self.entry(i), 0) < p);
        // `lo` 起的条目首词都 ≥ prefix；以 prefix 开头的那些排在最前面（字典序下前缀
        // 恒小于任何以它开头的更长串），再一次二分就切出上界。
        let hi = partition_point(lo, n, |i| self.word(&self.entry(i), 0).starts_with(p));
        lo..hi
    }

    pub fn len(&self) -> usize {
        self.entry_count
    }

    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// 本索引占的**进程私有**堆字节：常驻时是镜像 `Vec` 的容量，mmap 时为 0。
    ///
    /// 打进预热日志，是这张表在真机上唯一的内存观测出口（条数说明不了内存）。
    pub fn heap_bytes(&self) -> usize {
        match &self.data {
            ImageData::Owned(v) => v.capacity(),
            ImageData::Mapped(_) => 0,
        }
    }

    /// 映射的文件字节（不计私有内存，可被系统按需换出）；常驻时为 0。
    ///
    /// 与 [`Self::heap_bytes`] 分开报：mmap 下两者差着整张表，混成一个数就看不出私有内存降没降。
    pub fn mapped_bytes(&self) -> usize {
        match &self.data {
            ImageData::Owned(_) => 0,
            ImageData::Mapped(m) => m.len(),
        }
    }

    /// 按分好的段查词组。`segs` 已由调用方切分并小写化，**空段须已剔除**
    /// （见 [`split_segments`]）。
    ///
    /// # 匹配规则
    ///
    /// 1. **首段锚定第一个词**：`segs[0]` 必须是 `words[0]` 的前缀。
    ///    不锚定的话 `pro` 会命中一切含 Pro 的词组 —— 候选爆炸，且与「从头打起」的心智不符。
    /// 2. **其余段保序子序列匹配**，允许跳过中间的词。
    ///    于是 `ip'pro` 能命中 `iPhone 15 Pro`，不必写成 `ip'15'pro`。
    /// 3. 尾部未被任何段覆盖的词**不要求匹配**——那是前缀补全语义，同打 `hel` 出 `hello`。
    ///
    /// 贪心最左匹配：对「是否存在子序列」这个判定，贪心最左与最优解等价（经典结论），
    /// 同时它给出的结束位置是所有可行匹配里最小的，正好就是排序要的紧凑度。
    ///
    /// # 排序
    ///
    /// `weight 降序 → 跨度升序 → 文本序`，见函数体。
    ///
    /// 两次二分定位首词区间后只扫区间内（[`Self::first_word_range`]），查询在按键链路上。
    pub fn search(&self, segs: &[String], limit: usize) -> Vec<Candidate> {
        if segs.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut hits: Vec<(usize, PhraseEntry, &str)> = Vec::new();
        for i in self.first_word_range(&segs[0]) {
            let e = self.entry(i);
            if let Some(span) = self.match_entry(&e, segs)
                && let Some(text) = self.text(&e)
            {
                hits.push((span, e, text));
            }
        }
        // 主键必须是 weight：协调器按 weight 统一重排（AGENTS.md「跨组件硬约定」），跨度当
        // 主键会让英文方案与不排序的快捷输入两处次序不一致。也不能把跨度折进 weight
        // （weight 只承载真实词频）。跨度在同权重时定贴合度；文本序兜底，免得次序随词库
        // 遍历顺序漂移。
        hits.sort_by(|a, b| {
            b.1.weight
                .cmp(&a.1.weight)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.2.cmp(b.2))
        });
        hits.truncate(limit);
        hits.into_iter()
            .enumerate()
            .map(|(i, (_, e, text))| Candidate {
                text: text.to_string(),
                code: self.code(&e).unwrap_or_default().to_string(),
                weight: e.weight,
                natural_order: i as i32,
                source: CandidateSource::English,
                ..Default::default()
            })
            .collect()
    }

    /// 一条词组是否匹配这组段；匹配则返回**跨度** = 最后一段落在第几个词上。
    ///
    /// 跨度就是紧凑度：`ip'pro` 对 `iPhone 15 Pro` 跨度 2、对假想的 `iPhone Pro` 跨度 1，
    /// 后者更贴合所打的两段，该排前面。
    fn match_entry(&self, e: &PhraseEntry, segs: &[String]) -> Option<usize> {
        let n = e.word_count as usize;
        // 段比词还多 ⇒ 无论怎么跳都对不上。提前挡掉，省下后面的逐段扫。
        if segs.len() > n {
            return None;
        }
        // 词表越界 ⇒ 镜像损坏，按不匹配处理。不是摆设：写坏的 `word_count` 可达几十亿，
        // 跳词循环会让按键线程卡死几十秒（`scribbled_entries_never_panic`）。
        if (e.words_start as usize).saturating_add(n) > self.word_ends_count {
            return None;
        }
        // 规则 1：首段锚定第一个词。
        //
        // 经 `search` 进来时恒成立，仍保留：这是本函数自身的契约。
        if !self.word(e, 0).starts_with(segs[0].as_bytes()) {
            return None;
        }
        // 规则 2：其余段在 words[1..] 上保序贪心最左。
        let mut wi = 1usize;
        let mut span = 0usize;
        for seg in &segs[1..] {
            loop {
                if wi >= n {
                    return None;
                }
                let w = self.word(e, wi);
                wi += 1;
                if w.starts_with(seg.as_bytes()) {
                    span = wi - 1;
                    break;
                }
            }
        }
        Some(span)
    }
}

/// 按分词符切段并小写化，**剔除空段**。
///
/// 空段必须剔除而非让它匹配失败：打到 `ip'` 时末段天然为空，否则候选每按一次分词符就
/// 整片消失一次。中间空段（`ip''pro`）按手滑宽容处理。
pub fn split_segments(input: &str, sep: char) -> Vec<String> {
    input
        .split(sep)
        .filter(|s| !s.is_empty())
        .map(lower)
        .collect()
}

/// 小写化。**索引侧（[`PhraseSegBuilder::push`]）与查询侧必须同源**，两处都只走这个判据。
///
/// 不能逐字符 `char::to_lowercase`：它没有希腊词尾 Σ→ς 的上下文特例，用户打出词尾 ς 时
/// 配不上索引。纯 ASCII 走 `to_ascii_lowercase` 快路径，结果与 `str` 版逐字节相同。
///
/// 语义一变，盘上的 `.wphr` 就全是错的：改动时同步
/// [`wind_dict::cache_fp::ENGLISH_PHRASE_INDEX_TAG`] +1。
fn lower(s: &str) -> String {
    if s.is_ascii() {
        s.to_ascii_lowercase()
    } else {
        s.to_lowercase()
    }
}

/// 懒建的词组索引 + 后台预热 + **落盘复用**。
///
/// 与 `codetable/sentence.rs` 的 [`LazyTables`](crate::codetable::sentence) 同款：懒建只决定
/// 要不要付全表扫描，预热决定在哪条线程上付（不预热就落在第一次按分词符时）。预热没跑完
/// 就打到了，按键线程自己建，最坏与不预热持平。
///
/// 落盘（`docs/design/memory-footprint.md` S5）：给了缓存目录（[`Self::with_cache_dir`]）后，
/// `get` 先按当前启用词库集合的指纹找 `<dir>/phrase-<指纹>.wphr`，命中直接打开（大于
/// [`PHRASE_INDEX_RESIDENT_MAX`] 走 mmap），不命中才全表扫、写盘、再从盘上打开。
///
/// - **指纹** = 各启用层的 [`DictLayer::entries_digest`](wind_dict::DictLayer::entries_digest)
///   （词库 wdat 路径 + 其缓存摘要 + 权重换算参数，按层序）+
///   [`ENGLISH_PHRASE_INDEX_TAG`](wind_dict::cache_fp::ENGLISH_PHRASE_INDEX_TAG)，经
///   `cache_fp::derived_build_key` 哈希——与 `.wridx` / `.wscc` 同一套二级指纹。
/// - **指纹进文件名**，不用 `.fp` sidecar：热摘再启用回来时先前那份还在，直接复用。
///   文件头里另存一份指纹，挡「改名 / 拷错」。
/// - 任一层说不清摘要（内存词库）或没有缓存目录 ⇒ 照旧堆上建、不落盘。
pub struct LazyPhraseIndex {
    /// `RwLock<Option<..>>` 而非 `OnceLock`：索引必须能被作废。关词库走热摘
    /// （`set_dict_enabled` → `unregister_layer`）不重建引擎，没有失效通路就会继续召回已禁用
    /// 词库里的词组。
    ///
    /// 内层用 `Arc` 让读取方取一次 clone 就放锁，不与后台预热的写锁互等。
    index: RwLock<Option<Arc<PhraseSegIndex>>>,
    /// 失效代号。[`Self::invalidate`] 递增，[`Self::get`] 在锁外构建之前记下、写回之前比对：
    /// 构建在锁外（全表扫），热摘若落在构建窗口里，按旧词库建的索引不得写回。启动预热让这个
    /// 窗口每次都真实存在（`tests/english_phrase_index.rs`）。
    ///
    /// 读写都在 `index` 的写锁内，故 `Relaxed` 足够。它只管内存里那份；盘上那份另由
    /// [`Self::load_or_build`] 的指纹复核守。
    generation: AtomicU64,
    /// 落盘目录。`None` = 不落盘（无缓存根、测试里的内存引擎）。
    cache_dir: Option<PathBuf>,
    /// 本实例做过几次全表扫。诊断 + 「命中缓存就不该再扫」的测试判据。
    full_scans: AtomicUsize,
    /// 单飞锁：同一时刻只有一条线程在 [`Self::load_or_build`]，见 [`Self::get`]。
    build_lock: std::sync::Mutex<()>,
    /// 常驻 / mmap 的分界，恒为 [`PHRASE_INDEX_RESIDENT_MAX`]；字段化只为让测试用小夹具
    /// 走到 mmap 那条路。
    resident_max: usize,
}

impl Default for LazyPhraseIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl LazyPhraseIndex {
    /// 不落盘的懒建索引。
    pub fn new() -> Self {
        Self::with_cache_dir(None)
    }

    /// 落盘到 `dir`（`None` 同 [`Self::new`]）。
    pub fn with_cache_dir(dir: Option<PathBuf>) -> Self {
        Self {
            index: RwLock::new(None),
            generation: AtomicU64::new(0),
            cache_dir: dir,
            full_scans: AtomicUsize::new(0),
            build_lock: std::sync::Mutex::new(()),
            resident_max: PHRASE_INDEX_RESIDENT_MAX,
        }
    }

    #[cfg(test)]
    fn with_resident_max(mut self, n: usize) -> Self {
        self.resident_max = n;
        self
    }

    /// 取索引，必要时现场构建（或从盘上打开）。
    ///
    /// 两段式取锁（先读后写）而不是全程持写锁：读路径在按键链路上，绝大多数调用都会命中
    /// 已建好的索引、只付一次读锁。
    ///
    /// 未命中时先抢单飞锁（[`Self::build_lock`]）再复查：并发未命中只扫一遍，也不会两边
    /// 同时为同一指纹落盘（Windows 上后到的 rename 会撞上已映射的文件）。单飞锁与 `index`
    /// 锁分开，持有期间读路径照常走。
    pub fn get(&self, dm: &DictManager) -> Arc<PhraseSegIndex> {
        loop {
            if let Some(idx) = self.current() {
                return idx;
            }
            let _flight = self.build_lock.lock().unwrap_or_else(|e| e.into_inner());
            // 排队期间前一位已经建好（且没被作废）就直接用。
            if let Some(idx) = self.current() {
                return idx;
            }
            // 先记代号再构建，顺序不能反，否则照不出「构建期间被失效」。
            let started_at = self.generation.load(Relaxed);
            let built = Arc::new(self.load_or_build(dm, started_at));
            let mut w = self.index.write().unwrap_or_else(|e| e.into_inner());
            // 期间别的线程已经建好就用它的，保证同一时刻只有一份索引在被引用。
            if let Some(existing) = w.as_ref() {
                return Arc::clone(existing);
            }
            if self.generation.load(Relaxed) != started_at {
                // 构建期间词库变过，这份按旧词库建的不许写回，放锁重来。
                drop(w);
                continue;
            }
            *w = Some(Arc::clone(&built));
            return built;
        }
    }

    fn current(&self) -> Option<Arc<PhraseSegIndex>> {
        self.index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(Arc::clone)
    }

    /// 有缓存就开盘上那份，没有就全表扫、落盘、再从盘上打开。任一环失败都退回「堆上
    /// 建」这个旧行为——只影响内存与启动耗时，不影响结果。`started_at` 是调用方在构建前
    /// 记下的失效代号，落盘前要比对（见 ③）。
    ///
    /// 峰值：不落盘的几条路要在堆上拼完整镜像，期间 arena 与镜像同时在堆上，峰值约为成品
    /// 两倍；落盘那条路从 arena 直接写文件，峰值较低。
    fn load_or_build(&self, dm: &DictManager, started_at: u64) -> PhraseSegIndex {
        let t0 = std::time::Instant::now();
        let Some(dir) = self.cache_dir.as_deref() else {
            return PhraseSegIndex::from_image(self.scan(dm).into_image(""));
        };
        // 有层说不清摘要（内存词库）：没有稳定的磁盘产物可比，落盘只会留下一份无从校验的缓存。
        let Some(digests) = dm.entries_digest() else {
            return PhraseSegIndex::from_image(self.scan(dm).into_image(""));
        };
        let fp = wind_dict::cache_fp::derived_build_key(
            &digests,
            wind_dict::cache_fp::ENGLISH_PHRASE_INDEX_TAG,
        );
        let path = dir.join(format!("phrase-{fp}.{CACHE_EXT}"));

        // ① 复用：启用集合与各词库内容都没变就直接开盘上的那份，连扫描都不发生。
        if path.exists() {
            match PhraseSegIndex::open(&path, &fp, self.resident_max) {
                Ok(idx) => {
                    touch(&path);
                    tracing::info!(
                        phrases = idx.len(),
                        heap_kb = idx.heap_bytes() / 1024,
                        mapped_kb = idx.mapped_bytes() / 1024,
                        ms = t0.elapsed().as_millis(),
                        "英文词组分词：复用落盘索引"
                    );
                    return idx;
                }
                // 截断 / 损坏 / 版本不符：重建并覆盖。留痕，否则「每次启动都慢」无从查起。
                Err(e) => tracing::warn!("英文词组索引缓存 {} 打不开（{e}），重建", path.display()),
            }
        }

        // ② 全表扫。
        let builder = self.scan(dm);
        let scan_ms = t0.elapsed().as_millis();

        // ③ 扫描期间启用集合变了（热摘 / 启停与扫描交错）：这份内容对不上 `fp`，不许落盘。
        //    内存里那份由 `get` 的代号比对处理，这里只管别把它写进以旧集合命名的文件。
        //
        //    两道判据缺一不可：指纹复核挡「扫描前后集合不同」，代号挡「关了又开」（ABA，
        //    前后指纹相同但扫到中间态）；而「换了集合还没 invalidate」的窗口里代号不变。
        if self.generation.load(Relaxed) != started_at
            || dm.entries_digest().as_deref() != Some(digests.as_slice())
        {
            tracing::info!("英文词组分词：扫描期间词库集合变了，本次不落盘");
            return PhraseSegIndex::from_image(builder.into_image(""));
        }

        // ④ 落盘后从盘上重新打开，这一步才真正把字节移出进程私有内存。
        match write_atomically(&path, |w| builder.write_image(&fp, w)) {
            Ok(()) => {
                drop(builder);
                prune_old_caches(dir, &path, PHRASE_INDEX_CACHE_KEEP);
                match PhraseSegIndex::open(&path, &fp, self.resident_max) {
                    Ok(idx) => {
                        tracing::info!(
                            phrases = idx.len(),
                            heap_kb = idx.heap_bytes() / 1024,
                            mapped_kb = idx.mapped_bytes() / 1024,
                            scan_ms,
                            "英文词组分词：全表扫建表并落盘"
                        );
                        idx
                    }
                    // 刚写好就读不回（极罕见）：builder 已放掉，再扫一遍堆上建，不为这条路
                    // 让每次建表都多攥一份镜像。
                    Err(e) => {
                        tracing::warn!(
                            "刚写好的英文词组索引 {} 打不开（{e}），堆上重建",
                            path.display()
                        );
                        PhraseSegIndex::from_image(self.scan(dm).into_image(""))
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "英文词组索引写盘失败 {}（{e}）——本次常驻内存，下次启动仍需全表扫",
                    path.display()
                );
                PhraseSegIndex::from_image(builder.into_image(""))
            }
        }
    }

    /// 全表扫一次（计数）。
    fn scan(&self, dm: &DictManager) -> PhraseSegBuilder {
        self.full_scans.fetch_add(1, Relaxed);
        PhraseSegIndex::scan(dm)
    }

    /// 诊断：本实例做过几次全表扫。命中落盘缓存时不增。
    pub fn full_scans(&self) -> usize {
        self.full_scans.load(Relaxed)
    }

    /// 诊断：已建时返回索引的堆字节（[`PhraseSegIndex::heap_bytes`]，mmap 时接近 0），
    /// 未建返回 `None`。不触发构建。
    pub fn heap_bytes_if_built(&self) -> Option<usize> {
        self.index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|i| i.heap_bytes())
    }

    /// 诊断：已建时返回映射字节（[`PhraseSegIndex::mapped_bytes`]，常驻时为 0），
    /// 未建返回 `None`。不触发构建。
    pub fn mapped_bytes_if_built(&self) -> Option<usize> {
        self.index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|i| i.mapped_bytes())
    }

    /// 索引是否已经建出来了。
    ///
    /// 供「关着词组分词就不该付这笔内存」的守门测试用（`english.rs` 的
    /// `a_disabled_feature_never_builds_the_index`）。
    pub fn is_built(&self) -> bool {
        self.index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// 作废索引，下次查询时重建。
    ///
    /// 调用点＝词库启用状态变更（`EnglishEngine::set_dict_enabled`）。词库热摘不重建引擎，
    /// 这是索引跟上词库的唯一通路。落盘时下一次 `get` 按新集合的指纹取另一份文件。
    pub fn invalidate(&self) {
        // 代号与清空在同一把写锁内，`get` 的比对也在这把锁内，看不到中间态。
        let mut w = self.index.write().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Relaxed);
        *w = None;
    }

    /// 把索引构建推给后台线程。由引擎构建完成时调用。
    pub fn prewarm(self: &Arc<Self>, dm: Arc<DictManager>) {
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("english-phrase-warm".into())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let idx = me.get(&dm);
                tracing::info!(
                    ms = t0.elapsed().as_millis(),
                    phrases = idx.len(),
                    // 条数说明不了内存，字节数一并报；mmap 字节另报（不计私有内存）。
                    heap_kb = idx.heap_bytes() / 1024,
                    mapped_kb = idx.mapped_bytes() / 1024,
                    full_scans = me.full_scans(),
                    "英文词组分词：后台预热完成"
                );
            });
        if let Err(e) = spawned {
            // 不致命：索引仍会在首次分词查询时现场构建，只是那一下会卡。
            tracing::warn!("英文词组分词：预热线程启动失败（{e}），退回首次查询时现场构建");
        }
    }
}

/// 临时文件 + rename 原子替换（同 `wind_dict::reverseidx::write_wridx`），内容由 `fill` 流式写入。
///
/// 临时文件名带进程号与序号：多个实例同时为同一指纹落盘时各写各的，rename 谁赢都一样；
/// 共用一个 `.tmp` 会交错写出半截文件。
fn write_atomically(
    path: &Path,
    fill: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write;
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!(
        "{CACHE_EXT}.{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Relaxed)
    ));
    let res = (|| {
        let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        fill(&mut w)?;
        w.flush()?;
        drop(w);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// 命中时刷新 mtime，让 [`prune_old_caches`] 的「最近用过」按使用而非按写入算。
/// 失败无害（最坏是一份常用的被当成旧的清掉，下次重建）。
fn touch(path: &Path) {
    let _ = std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(std::time::SystemTime::now()));
}

/// 崩溃残留的临时文件多旧才清（见 [`prune_old_caches`]）。
///
/// 不能见 `.tmp` 就删：可能正有另一个实例在写它。一次落盘百毫秒级，1 小时绰绰有余。
const STALE_TMP_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// 本模块产出的文件：`phrase-<指纹>.wphr` 与其写盘临时文件 `phrase-<指纹>.wphr.<进程>-<序号>.tmp`。
///
/// 缓存清理（`EngineManager::rebuild_all_caches` 的白名单）也认这个判据——只认扩展名的话
/// `.tmp` 太宽，会删到别人的临时文件。
pub fn is_phrase_cache_file(name: &str) -> bool {
    name.starts_with("phrase-")
        && (name.ends_with(&format!(".{CACHE_EXT}"))
            || (name.ends_with(".tmp") && name.contains(&format!(".{CACHE_EXT}."))))
}

/// 同目录的 `phrase-*.wphr` 只留最近 `keep` 份（`current` 恒保留），并清掉超过
/// [`STALE_TMP_AGE`] 的写盘临时文件（写到一半崩溃的残留，`write_atomically` 自己清不到）。
/// best-effort：删不掉（Windows 上仍被别的实例映射着）就留到下次。
///
/// 只认本模块自己的文件名形态——缓存目录里还躺着同方案的 `.wdat` 等产物。
fn prune_old_caches(dir: &Path, current: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?;
            if !is_phrase_cache_file(name) || p == current {
                return None;
            }
            let mtime = e.metadata().ok()?.modified().ok()?;
            if name.ends_with(".tmp") {
                if now
                    .duration_since(mtime)
                    .is_ok_and(|age| age > STALE_TMP_AGE)
                {
                    let _ = std::fs::remove_file(&p);
                }
                return None;
            }
            Some((mtime, p))
        })
        .collect();
    // 新的在前；`current` 已占一个名额。
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, p) in files.into_iter().skip(keep.saturating_sub(1)) {
        if let Err(e) = std::fs::remove_file(&p) {
            tracing::debug!("清理旧英文词组索引 {} 失败（{e}），留到下次", p.display());
        }
    }
}

#[cfg(test)]
#[path = "english_phrase_tests.rs"]
mod tests;
