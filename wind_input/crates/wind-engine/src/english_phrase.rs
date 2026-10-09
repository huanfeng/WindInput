//! 英文词组分词输入：分词符切段，每段只打前缀。
//!
//! 论坛 t42。`envi'deg` → `environmental degradation`，`ip'max` → `iPhone 15 Pro Max`。
//!
//! # ★ 查询读 `text`，不读 `code`
//!
//! 出厂词库里词组的编码**有两种并存的方案**（实测 787 条）：
//!
//! | 方案 | 条数 | 例 |
//! |---|---|---|
//! | 拼接式 | 498 | `Buenos Aires` → `buenosaires` |
//! | 共同前缀式 | 289 | `iPhone 15 Pro Max` → `iphone`（code 里没有后段词） |
//!
//! 共同前缀式的 code 压根不含后段词，任何「把 code 拆成各段码」的方案（如 t42 原帖设想的
//! `p1..p10` 分列）对它们直接失效。而**词边界一直明摆在 `text` 的空格里**——按 text 切分
//! 两种编码统一处理，用户自备词库的编码方案也不影响本功能。
//!
//! 代价是索引与 code 无关，不能靠 Trie 前缀剪枝。**但也不是全表线性扫**：匹配规则把首段
//! 钉死在第一个词上，于是索引按首词排序、查询两次二分切出窗口，只扫窗口内
//! （见 [`PhraseSegIndex::first_word_range`]）。出厂 787 条时这无所谓，真机上用户挂了
//! 自备词库是 18 万条，而查询在按键链路上——那时它是约 2 ms 与约 7 µs 的差别（数字与测法见
//! `bench_window_vs_full_scan`）。
//!
//! # 这个功能解决的不是「词组够不着」
//!
//! 词组**本来就能靠前缀召回**（打 `ipho` 出 7 条 iPhone 变体、`macos` 出 9 条 macOS 变体）。
//! 本功能的价值是**在共同前缀下精确定位**：选 `iPhone 15 Pro Max` 原本要翻页，
//! `ip'max` 一步到位。那 289 条共同前缀式条目是主要受益者。

use wind_candidate::{Candidate, CandidateSource};

/// 词组分词符。
///
/// 不做成可配项的理由见 `EnglishGlobal::phrase_seg` 的文档。一句话：真正的备选是 `.`
/// （t153 要拿它作模糊万能键），两者将来要一起定，在那之前多一个旋钮只是多一处要同步的真相。
pub const PHRASE_SEPARATOR: char = '\'';

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, RwLock};
use wind_dict::DictManager;

/// 索引里的一条词组：**只存偏移，不存字符串**。
///
/// 字符串全部躺在 [`PhraseSegIndex`] 的三块 arena 里。这不是微优化——旧结构每条持有
/// `Vec<Box<str>>` + 两个 `Box<str>`，一条词组就是 4~6 次独立堆分配，而真机上这张表有
/// **18 万条**（用户自备英文词库，出厂只有 787 条）：约 90 万次小分配，每次都要付分配器
/// 头部与对齐填充，实测常驻 48 MB，其中真实数据不到三分之一。
///
/// 改成 arena 后整张表只有个位数次分配，字段也从「指针 + 容量」缩到定长偏移。
///
/// 它同时是镜像里一条定长记录（[`ENTRY_SIZE`] 字节、七个小端字段按声明序）的**解码值**。
/// 读侧逐字段 `from_le_bytes` 解出，不做指针强转——与 `.wridx` 同一读法，不依赖对齐、
/// 不需要 `unsafe`，也就不必为它加 `#[repr(C)]`（内存布局与文件布局是两件事）。
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
/// 取值与理由同 `REVERSE_INDEX_RESIDENT_MAX`（`manager.rs`，2026-08-24 实测）：建映射有
/// 近乎恒定的 ~9 ms 开销，2.3 MB 顺序读只要 1 ms，小镜像常驻反而更快；大镜像常驻则要
/// 多付等量私有内存，换来的查询速度完全相同（页缓存热了之后 mmap 就是普通内存读）。
/// 本表的两档实测规模恰好落在它两侧：出厂 787 条约 60 KB（常驻），靶机 18 万条约
/// 13 MB（mmap）。两条路**同一份字节、同一套查找代码**，故这个值只影响性能、不影响结果。
pub const PHRASE_INDEX_RESIDENT_MAX: usize = 3 * 1024 * 1024;

/// 同一目录下最多留几份 `.wphr`（按 mtime，命中时会刷新）。
///
/// 指纹进的是文件名，于是「启用集合」每一种组合各有一份——这正是热摘词库后再启用能
/// 直接复用的原因。代价是旧组合与「词库改过内容之前」的那些不会自己消失，必须有个上限：
/// 18 万条时一份约 13 MB。4 份够覆盖「开着 / 关掉某本 / 再关一本」的来回切换。
const PHRASE_INDEX_CACHE_KEEP: usize = 4;

/// 镜像字节的来源：进程内构建产物，或磁盘文件的映射。对上层完全等价，差别只在这些字节
/// 算不算进程私有内存（同 `wind_dict::reverseidx` 的 `IndexData`）。
enum ImageData {
    Owned(Vec<u8>),
    Mapped(memmap2::Mmap),
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
/// ⚠️ 构建是 O(全表) 的（`DictManager::for_each_entry` 自己的注释就写着「绝不能出现在
/// 按键链路上」），故由 [`LazyPhraseIndex`] 守着 + 后台预热 + 落盘复用。
///
/// # 三块 arena
///
/// | | 存什么 | 谁读 |
/// |---|---|---|
/// | `lower` | 各词小写化后**首尾相接**（不留分隔符） | 匹配 |
/// | `raw` | 原文 + 编码首尾相接 | 产出候选 |
/// | `word_ends` | 每个词在 `lower` 中的结束偏移 | 切词 |
///
/// 词与词之间不留分隔符，是因为边界已由 `word_ends` 给出——再塞一个空格等于为 18 万条
/// 各付一个字节去表达一件已经表达过的事。
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
/// 就是建表时那三块 arena + entries 原样摊平，读侧零拷贝地从切片里按偏移取。对齐只为
/// 整齐，读法（逐字段 `from_le_bytes`）不依赖它。
///
/// # 有序是构造保证，不是运行期检查
///
/// 能产出镜像的只有 [`PhraseSegBuilder::finish`]（排序 + 末尾验一次），盘上的文件也是它
/// 写的；`PhraseSegIndex` 自身不提供任何收词入口。旧版靠一个 debug 构建里的 `finished`
/// 标志在 `search` 里兜「建完忘了 finish」，现在这种状态根本构造不出来。
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
    /// 空索引：零字节、零条目，所有查询返回空。不走「序列化一个空 builder 再解析」——
    /// 默认值必须无条件成功（同 `ReverseIndex::default` 的理由）。
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
    /// 收一条词条。**非词组（不足两个词）原样回滚**，不留痕迹。
    ///
    /// 判据是「text 里有空白」而不是「code 里有什么」：词边界只在 text 上（见模块文档
    /// 「查询读 text，不读 code」那一节）。
    ///
    /// 先写 arena 再回滚，而不是先数词数——数词数要先切一遍，切完还得再走一遍才能写进
    /// arena，等于对**全表**每条都多切一次。回滚只对被丢弃的那些条目付代价，而那是少数。
    fn push(&mut self, code: &str, text: &str, weight: i32) {
        let lower_start = self.lower.len() as u32;
        let words_start = self.word_ends.len() as u32;
        let mut word_count = 0u32;
        for w in text.split_whitespace() {
            // ★ 与查询侧同源，见 [`lower`]。纯 ASCII 的词（英文词库里的绝大多数）在那里
            // 走不查 Unicode 表的快路径；临时 `String` 只对非 ASCII 词产生，随即被 push
            // 进 arena 并丢弃——那是构建期的瞬时分配，不进常驻。
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

    /// **按首词排序**。
    ///
    /// 排序是 [`PhraseSegIndex::first_word_range`] 的前提，也就是 `search` 的前提。测试夹具
    /// 走的也是这一条（经 [`Self::finish`]）——夹具自己补一句 `sort` 就又是一份会漂移的
    /// 复制品（本模块的 `idx()` 夹具上一次就栽在这里：它自带一份「≥2 个词才进索引」的
    /// 判据，与 `push` 并存）。
    ///
    /// 稳定排序：同首词的条目保持枚举序，于是同一份词库两次建出的镜像逐字节相同
    /// （堆版与盘上那份对拍时靠的就是这一点）。
    fn sort(&mut self) {
        // 比较闭包要读 `self.lower` / `self.word_ends`，而 `self.entries` 同时被可变借出。
        // 取出来排完再放回是最直白的解法：`first_word()` 不碰 `entries`，语义完全等价。
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
    /// 落盘走这里**直接写文件**，不先拼一份完整镜像：那样建表时 arena 与镜像同时在堆上，
    /// 18 万条下峰值多出整整一张表（约 17 MB）——而这一步恰恰是为了省内存才做的。
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
    /// **mmap 失败降级为读进内存**（同 `.wcmt`「mmap 为主、失败降级到内存」）——文件本身
    /// 是好的，没理由为此再全表扫一遍。
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
            match unsafe { memmap2::Mmap::map(&file) } {
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
        // raw 是最后一段，其后只有对齐填充：总长必须恰好等于 `into_image` 写出的长度。
        // 只验「各段在界内」挡不住截在尾部填充里的那几种（实测：截掉最后 1 字节照样能开），
        // 而截断就是截断，没理由当它完好。
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
    /// 第 0 个词从 `lower_start` 起，其余从前一个词的结束偏移起——词在 arena 里首尾相接，
    /// 所以「上一个的 end」就是「这一个的 start」，不必另存起点。
    ///
    /// 给字节而非 `&str`：匹配只做前缀与字典序比较，而 `str` 的这两种比较本来就是逐字节的，
    /// 结果完全相同——省掉的是每次比较前的 UTF-8 校验（mmap 的字节不能假定合法）。
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
    /// 匹配规则 1 要求首段是第一个词的前缀（见 [`Self::search`]），于是**区间之外的条目
    /// 一条都不可能命中**，不必看。真机 18 万条词组下这是从「全表逐条 `starts_with`」
    /// 降到「两次二分 + 扫命中段」。
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
    /// ★ 打进预热日志，是这张表在真机上**唯一**的观测出口。
    /// 2026-09-22 查一次「服务占 100 MB」花了六轮 A/B 才定位到这里——当时日志只报条数
    /// （`phrases=180998`），而条数说明不了内存，恰恰是条数看着正常时内存最吓人。
    pub fn heap_bytes(&self) -> usize {
        match &self.data {
            ImageData::Owned(v) => v.capacity(),
            ImageData::Mapped(_) => 0,
        }
    }

    /// 映射的文件字节（不计私有内存，可被系统按需换出）；常驻时为 0。
    ///
    /// 与 [`Self::heap_bytes`] 分开报：两者在 mmap 下差着整张表，日志里混成一个数就无从
    /// 判断「内存到底降没降」（同 `ReverseIndex::resident_bytes` / `data_bytes` 的分工）。
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
    /// 2. **其余段保序子序列匹配**，允许跳过中间的词（2026-09-18 拍板：优先保效果）。
    ///    于是 `ip'pro` 能命中 `iPhone 15 Pro`，不必写成 `ip'15'pro`。
    /// 3. 尾部未被任何段覆盖的词**不要求匹配**——那是前缀补全语义，同打 `hel` 出 `hello`。
    ///
    /// 贪心最左匹配：对「是否存在子序列」这个判定，贪心最左与最优解等价（经典结论），
    /// 同时它给出的结束位置是所有可行匹配里最小的，正好就是排序要的紧凑度。
    ///
    /// # 排序
    ///
    /// `weight 降序 → 跨度升序 → 文本序`。主键是 weight 而非跨度，理由见函数体里的长注释
    /// （一句话：协调器会按 weight 统一重排，引擎内不改 weight 的排序会被冲掉）。
    ///
    /// # 量级
    ///
    /// **两次二分定位首词区间，然后只扫区间内**（见 [`Self::first_word_range`]）。规则 1
    /// 把首段钉死在第一个词上，区间外的条目连看都不用看。
    ///
    /// 这一步不是为出厂那 787 条做的——那点量怎么扫都行。真机上用户挂自备英文词库后
    /// 这张表是 **18 万条**，而查询在按键链路上：每按一个字母全表扫一遍，`ip` 这样的
    /// 短前缀尤其吃亏。二分之后扫描量只剩首词真正匹配的那几百条。
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
        // ★ **weight 降序 → 跨度升序 → 文本序**。跨度是次级键，不是主键。
        //
        // 主键必须是 weight，这是 AGENTS.md「跨组件硬约定」里的一条：**候选排序必须落到
        // weight，引擎内部只调顺序、不改 weight 的排序会被协调器的统一重排冲掉**。
        // `EnglishEngine` 没有覆写 `base_sort_ignores_weight()`（默认 false），于是英文
        // 方案那条路上 `candidate_display_order` 的键序是
        // `cmp_exact_first → by_weight → base_order → natural_order` —— 跨度一个都不在里面。
        //
        // 本模块**曾把跨度当主键**，实测的后果是两个作用域顺序不一致：`ip'pro` 在引擎侧
        // 首位是跨度 1 的 `iPad Pro`，到了英文方案首页却被 weight 序挤出去了；而快捷输入
        // 那条路（`update_mix_candidates`）完全不排序、原样透传，跨度序在那边还活着。
        // 同一串输入两处不同序，且单测测的是用户看不到的中间态。
        //
        // 另外两条路都不可行：`base_sort_ignores_weight() -> true` 会对**全部**英文候选
        // 生效（普通英文输入的词频排序一起变）；把跨度折进 `weight` 与
        // `mixed/engine.rs` 的「`weight` 只承载真实词频」相冲。
        //
        // 于是承认 weight 优先就是最终口径。跨度仍然有用——同权重时它决定谁更贴合所打的
        // 那几段，而词库里同权重的条目成片存在（出厂词组大量 weight 相同）。
        // 文本序兜底是为了定序：同分时次序不能随词库遍历顺序漂移，否则候选位置会在重建
        // 索引后莫名换位。
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
        // 词表越界 ⇒ 镜像损坏，按不匹配处理。这一条**不是**防御性摆设：被写坏的 `word_count`
        // 可以是几十亿，下面的跳词循环会逐个空词走到底——按键线程卡死几十秒
        // （`scribbled_entries_never_panic` 实测 48 s）。
        if (e.words_start as usize).saturating_add(n) > self.word_ends_count {
            return None;
        }
        // 规则 1：首段锚定第一个词。
        //
        // 走 `search` 进来时这条恒成立（`first_word_range` 已按它切过区间），**仍然保留**：
        // 它是本函数自身的契约，删掉的话函数就只在「调用方恰好先筛过」时才正确。
        // 代价是区间内每条多一次短前缀比较，与省下的 18 万次不在一个量级。
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
/// 空段必须剔除而不是让它匹配失败：用户打到 `ip'` 时最后一段天然是空的，若让它参与匹配，
/// 候选会在每次按下分词符的那一刻整片消失，再打一个字母又回来——打字过程中闪烁。
/// 中间的空段（`ip''pro`，多按一下）同理，按「手滑」宽容处理。
pub fn split_segments(input: &str, sep: char) -> Vec<String> {
    input
        .split(sep)
        .filter(|s| !s.is_empty())
        .map(lower)
        .collect()
}

/// 小写化。**索引侧（[`PhraseSegBuilder::push`]）与查询侧必须同源**，两处都只走这个判据。
///
/// # 为什么不能图省事逐字符
///
/// `char::to_lowercase` 与 `str::to_lowercase` **不等价**：后者带希腊词尾 Σ 的上下文特例
/// （`"ΟΔΟΣ"` → `"οδος"`，词尾 ς），前者恒给 σ（`"οδοσ"`）。逐字符版有两个毛病：
/// 两侧只要有一侧用它就静默漏召回；**即便两侧都用它**，用户自己打出词尾 ς 时也配不上
/// 索引里的 σ。改动前索引侧走的就是 `str` 版，逐字符是 arena 改造时为省临时分配换的
/// ——省下的那点分配不值一个语义回归。
///
/// ⚠️ 本函数的语义一变，盘上的 `.wphr` 就全是错的：改动时同步
/// [`wind_dict::cache_fp::ENGLISH_PHRASE_INDEX_TAG`] +1。
///
/// # ASCII 快路径
///
/// 英文词库里绝大多数词是纯 ASCII，而 ASCII 小写化没有任何上下文特例，`to_ascii_lowercase`
/// 与 `to_lowercase` 逐字节相同。走它避开 Unicode 表查找；非 ASCII 才落到 `str` 版。
/// 两条路对同一输入必然给出同一结果，所以这个分流不引入新的分叉面。
fn lower(s: &str) -> String {
    if s.is_ascii() {
        s.to_ascii_lowercase()
    } else {
        s.to_lowercase()
    }
}

/// 懒建的词组索引 + 后台预热 + **落盘复用**。
///
/// 与 `codetable/sentence.rs` 的 [`LazyTables`](crate::codetable::sentence) 同款：
/// 「懒」只解决**要不要付**这笔全表扫描，不解决**在哪条线程上付**——不预热的话它会恰好
/// 落在用户第一次按下分词符的那一刻。预热**不改变任何取值**：`get` 的两段式取锁保证
/// 同一时刻只有一份索引被引用（竞态下两条线程可能各建一次，但只有一份会被留下，
/// 见 [`LazyPhraseIndex::get`] 自己的说明——**不是** `OnceLock::get_or_init` 那种
/// 「保证只建一次」，这里刻意不用 `OnceLock`，因为索引必须能被作废）。
/// 预热没跑完就打到了，按键线程自己建一份，那是与「不预热」持平的最坏情况，不会更差。
///
/// # 落盘（2026-10-09，`docs/design/memory-footprint.md` S5）
///
/// 靶机实测 18.1 万条词组常驻约 13 MB，且每次启动后台全表扫 127–401 ms。给了缓存目录
/// （[`Self::with_cache_dir`]）后，`get` 先按「当前启用词库集合」的指纹找
/// `<dir>/phrase-<指纹>.wphr`：命中就直接打开（大于 [`PHRASE_INDEX_RESIDENT_MAX`] 走 mmap，
/// 常驻堆接近 0），不命中才全表扫、写盘、再从盘上打开。
///
/// - **指纹** = 各启用层的 [`DictLayer::entries_digest`](wind_dict::DictLayer::entries_digest)
///   （词库 wdat 路径 + 其缓存摘要 + 权重换算参数，按层序）+
///   [`ENGLISH_PHRASE_INDEX_TAG`](wind_dict::cache_fp::ENGLISH_PHRASE_INDEX_TAG)，经
///   `cache_fp::derived_build_key` 哈希——与 `.wridx` / `.wscc` 同一套二级指纹。
/// - **指纹进文件名**，不用 `.fp` sidecar：热摘一本词库后启用集合变了，要取的是**另一份**
///   文件；再启用回来时先前那份还在，直接复用。sidecar 模式一个路径只容一份，来回切就是
///   来回重建。文件头里另存一份指纹，挡「改名 / 拷错」。
/// - 任一层说不清摘要（内存词库）或没有缓存目录 ⇒ 照旧堆上建、不落盘。
pub struct LazyPhraseIndex {
    /// `RwLock<Option<..>>` 而非 `OnceLock`：**索引必须能被作废**。
    ///
    /// 关闭某本英文词库走的是**热摘**（`CodeTableEngine::set_dict_enabled` →
    /// `DictManager::unregister_layer`，返回 true = 目标已达成 ⇒ 引擎不重建）。索引若只建
    /// 一次且没有失效通路，就会继续召回已禁用词库里的词组——出厂 `en_ext` 一本就带 779/787
    /// 条，而同一串输入走原路径已经查不到它们了。症状是本仓反复记着的那种
    /// 「关了没反应，顺手改别的设置又好了」（改别的设置会触发 `reload_from_config` →
    /// `engines.clear()` → 引擎连同索引一起重建）。
    ///
    /// 用 `Arc` 包内层是为了让读取方**不必持锁**：查询在按键路径上，持读锁跑完整个线性扫
    /// 会和后台预热的写锁互相等。取一次 `Arc::clone` 就放锁。
    index: RwLock<Option<Arc<PhraseSegIndex>>>,
    /// 失效代号。[`Self::invalidate`] 递增，[`Self::get`] 在**锁外构建之前**记下它、
    /// 写回之前比对。
    ///
    /// # 没有它会怎样（2026-09-22 实测的真实竞态）
    ///
    /// `get` 刻意在锁外构建（那是秒级的全表扫，持锁会把按键线路一起堵住），于是
    /// 「开始构建」与「写回」之间有一段长窗口。热摘词库（`set_dict_enabled` →
    /// `unregister_layer` → `invalidate`）若落在这段窗口里，那份**按旧词库建好的**索引
    /// 随后会被原样写回 —— 用户看到的就是本仓反复记着的那句「关了词库没反应」。
    ///
    /// 引擎构造时会 `prewarm` 一条后台线程去建索引，所以这个窗口在**每次启动**都真实存在，
    /// 不是理论竞态：`tests/english_phrase_index.rs` 在机器负载高时就会红，而它测的正是
    /// 「关掉 en_ext 之后不得再召回它里面的词组」。
    ///
    /// 代号的读写都在 `index` 的写锁内完成，故 `Relaxed` 足够——锁本身提供了同步。
    ///
    /// ⚠️ 它只管**内存里那一份**。盘上那份另有守卫：扫描前后各取一次指纹，不等就不落盘
    /// （见 [`Self::load_or_build`]）——否则按新集合扫出的内容会躺进以旧集合命名的文件，
    /// 而文件是跨进程、跨重启复用的。
    generation: AtomicU64,
    /// 落盘目录。`None` = 不落盘（无缓存根、测试里的内存引擎）。
    cache_dir: Option<PathBuf>,
    /// 本实例做过几次全表扫。诊断 + 「命中缓存就不该再扫」的测试判据。
    full_scans: AtomicUsize,
    /// 单飞锁：同一时刻只有一条线程在 [`Self::load_or_build`]，见 [`Self::get`]。
    build_lock: std::sync::Mutex<()>,
    /// 常驻 / mmap 的分界，恒为 [`PHRASE_INDEX_RESIDENT_MAX`]；字段化只为让测试用小夹具
    /// 走到 mmap 那条路（3 MB 的夹具要四万多条词组）。
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
    /// 未命中时先抢**单飞锁**（[`Self::build_lock`]）再复查：预热线程与按键线程同时撞上
    /// 空索引时只扫一遍，后到者等着拿现成的。等的时长与「自己再扫一遍」相当，但不重复付
    /// CPU，也不会两边同时为同一指纹落盘（Windows 上后到的 rename 会撞上先到者刚映射的
    /// 文件，打一条误导性的 warn）。单飞锁与 `index` 锁分开：持它期间按键线程的读路径
    /// 照常走，只有同样未命中的才排队。
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
            // ★ 先记代号**再**构建。顺序不能反：反了就照不出「构建期间被失效」。
            let started_at = self.generation.load(Relaxed);
            let built = Arc::new(self.load_or_build(dm, started_at));
            let mut w = self.index.write().unwrap_or_else(|e| e.into_inner());
            // 期间别的线程已经建好就用它的，保证同一时刻只有一份索引在被引用。
            if let Some(existing) = w.as_ref() {
                return Arc::clone(existing);
            }
            if self.generation.load(Relaxed) != started_at {
                // 构建期间词库变过（热摘了某本），这份是按旧词库建的，**不许写回**。
                // 放锁重来——下一轮按新词库重建。见 `generation` 字段的文档。
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
    /// ⚠️ 峰值：不落盘的几条路（无缓存目录、内存词库、扫描期间集合变了、写盘失败）要在
    /// 堆上拼一份完整镜像，期间 arena 与镜像**同时在堆上**，峰值约为成品的两倍（合成
    /// 18 万条实测 42.9 MB，成品 16.9 MB）。落盘那条路从 arena 直接写文件，峰值 30.8 MB。
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
                // 截断 / 损坏 / 版本不符：落到下面重建并覆盖。留痕——静默重建会让「每次
                // 启动都慢」这类故障失去唯一的外部线索（同 `.wridx` 那条）。
                Err(e) => tracing::warn!("英文词组索引缓存 {} 打不开（{e}），重建", path.display()),
            }
        }

        // ② 全表扫。
        let builder = self.scan(dm);
        let scan_ms = t0.elapsed().as_millis();

        // ③ 扫描期间启用集合变了（热摘 / 启停与扫描交错）：这份内容对不上 `fp`，不许落盘。
        //    内存里那份由 `get` 的代号比对处理，这里只管别把它写进以旧集合命名的文件。
        //
        //    两道判据缺一不可：指纹复核挡「扫描前后集合不同」，代号挡「关了又开」（ABA）——
        //    后者前后指纹相同，扫到的却是中间态；而每次启停都伴随 `invalidate`，代号必变。
        //    反过来代号也替代不了指纹：「换了集合但还没来得及 invalidate」的窗口里代号不变。
        if self.generation.load(Relaxed) != started_at
            || dm.entries_digest().as_deref() != Some(digests.as_slice())
        {
            tracing::info!("英文词组分词：扫描期间词库集合变了，本次不落盘");
            return PhraseSegIndex::from_image(builder.into_image(""));
        }

        // ④ 落盘后**从盘上重新打开**——这一步才真正把字节移出进程私有内存（同 `.wridx`）。
        //    从 arena 直接写文件，不先拼内存镜像（理由见 `PhraseSegBuilder::write_image`）。
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
                    // 极罕见（刚写好就读不回）。builder 已放掉，再扫一遍堆上建——宁可慢这一次，
                    // 也不为这条路让每次建表都多攥一份镜像。
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
    /// `a_disabled_feature_never_builds_the_index`）。真机上这张表 12.7 MB，
    /// 而它是否存在只取决于两个开关的**或**，判据必须能被断言，不能只写在注释里。
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
        // 代号与清空在**同一把写锁内**完成，`get` 的比对也在这把锁内 —— 两者互斥，
        // 不存在「代号已加、索引还没清」的中间态被看到。
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
                    // 条数说明不了内存：出厂 787 条与用户自备词库的 18 万条差两个数量级，
                    // 而后者曾在真机上常驻 48 MB。把字节数一并报出来；mmap 时堆接近 0，
                    // 映射字节另报（不计私有内存）。
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
/// 临时文件名带进程号与序号：同一进程里两个引擎实例（测试里常见）或两个进程同时为同一
/// 指纹落盘时，各写各的临时文件，rename 谁后到谁赢——内容由指纹决定，赢谁都一样。
/// 共用一个 `.tmp` 的话两边会交错写同一个文件，rename 出去的可能是半截。
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
/// 不能见 `.tmp` 就删：同目录可能正有另一个实例（另一进程、测试里的另一引擎）在写它，
/// 删掉的后果是对方 rename 失败、退回堆上建。一次落盘是百毫秒级，1 小时绰绰有余。
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
mod tests {
    use super::*;

    /// ⚠️ 夹具走**生产同一条** `push` + `finish`，不再自己复制一份「≥2 个词才进索引」的
    /// 判据、也不自己补排序——那种复制品曾与 `build` 并存，是典型的漂移隐患
    /// （改了一处另一处静默过期）。
    fn idx(pairs: &[(&str, &str, i32)]) -> PhraseSegIndex {
        let mut b = PhraseSegBuilder::default();
        for (text, code, w) in pairs {
            b.push(code, text, *w);
        }
        PhraseSegIndex::from_image(b.finish())
    }

    impl PhraseSegIndex {
        /// 全部条目（按镜像序）。只给测试 / 基准里的「朴素全表扫」对照用。
        fn entries(&self) -> impl Iterator<Item = PhraseEntry> + '_ {
            (0..self.entry_count).map(|i| self.entry(i))
        }
    }

    fn texts(i: &PhraseSegIndex, input: &str) -> Vec<String> {
        i.search(&split_segments(input, '\''), 20)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    /// 真机量级（18 万条）下二分窗口与全表扫的耗时对比。手动跑：
    /// `cargo test -p wind-engine --release --lib -- --ignored --nocapture bench_`
    ///
    /// 实测（release、单线程、同一查询 200 次取均值，合成 18 万条、窗口 100 条）：
    /// - 2026-09-22，arena 结构体版：窗口 5.1 µs / 全表 1.48 ms；
    /// - 2026-10-09，字节镜像版（落盘改造后，条目按需小端解码；AMD EPYC 7402 开发机，
    ///   3 轮）：窗口 7.2–8.3 µs / 全表 1.88–1.99 ms，约 250 倍。
    ///
    /// 无法确认两次是否同机同负载，不可直接对比出「解码慢了多少」；能确定的是窗口仍在
    /// 个位微秒。
    /// 全表扫的毫秒级落在按键链路上，每多打一个字母就再付一次——这才是做二分的理由，
    /// 不是「显得快一点」。
    ///
    /// `#[ignore]` 是因为它是**基准不是判据**：机器一换数字就变，拿它当回归门会变成
    /// 随机红。正确性由 `the_binary_search_window_returns_exactly_what_a_full_scan_would`
    /// 守，「有没有真的少扫」由 `the_first_word_window_is_exactly_the_prefix_block` 守。
    #[test]
    #[ignore = "基准，不参与常规回归"]
    fn bench_window_vs_full_scan() {
        let mut b = PhraseSegBuilder::default();
        for i in 0..180_000u32 {
            let text = format!("word{i:06} beta gamma{i:04}");
            b.push(&format!("w{i}"), &text, (i % 1000) as i32);
        }
        let me = PhraseSegIndex::from_image(b.finish());
        let segs = split_segments("word0123'gam", PHRASE_SEPARATOR);
        let t0 = std::time::Instant::now();
        for _ in 0..200 {
            std::hint::black_box(me.search(&segs, 20));
        }
        let windowed = t0.elapsed() / 200;
        let t1 = std::time::Instant::now();
        for _ in 0..200 {
            let mut hits = 0usize;
            for e in me.entries() {
                if me.match_entry(&e, &segs).is_some() {
                    hits += 1;
                }
            }
            std::hint::black_box(hits);
        }
        let full = t1.elapsed() / 200;
        println!(
            "窗口 {windowed:?} / 全表 {full:?}  窗口条目数={}",
            me.first_word_range(&segs[0]).len()
        );
    }

    /// ★ 构建期间被 `invalidate` 的那份结果，**不许写回**。
    ///
    /// `get` 刻意在锁外构建（全表扫，持锁会堵住按键线路），于是「开始构建」到「写回」
    /// 之间有一段长窗口。热摘词库落在窗口里时，那份按**旧**词库建好的索引若被原样写回，
    /// 用户看到的就是「关了词库没反应」。
    ///
    /// 这不是理论竞态：引擎构造时 `prewarm` 一条后台线程去建索引，窗口每次启动都存在。
    /// 2026-09-22 `tests/english_phrase_index.rs` 在 `cargo test --workspace` 的负载下
    /// 稳定复现（机器闲时反而绿，所以单独跑那条照不出来）。
    ///
    /// # 时序是**摆明**的，不是靠撞
    ///
    /// 自定义一个会在遍历中途停下来等信号的层，于是「构建已经读过旧词库、但还没写回」
    /// 这个瞬间被固定住。第一版用 `channel` 在**调 `get` 之前**同步，那是错的——构建
    /// 压根没跨过失效点，删掉代号比对照样绿（实测）。
    ///
    /// 层的内容用一个标志翻转来模拟热摘，而不是真去 `unregister_layer`：`for_each_entry`
    /// 正持着 composite 的锁停在那里，主线程这时摘层会直接死锁。
    ///
    /// 反向验证（变异）：删掉 `get` 里 `generation != started_at` 那段比对即红。
    #[test]
    fn a_build_that_started_before_invalidate_must_not_win() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        use std::sync::{Mutex, mpsc};
        use wind_dict::{DictLayer, DictManager, LayerType};

        /// 测试层的共享状态：测试线程与层各持一份 `Arc`。
        struct Gate {
            entered: mpsc::Sender<()>,
            resume: Mutex<mpsc::Receiver<()>>,
            /// 只让**第一趟**遍历停下来；重建那趟要直接走完。
            stopped_once: AtomicBool,
            /// 还吐不吐 `zzz zzz`。主线程在放行前翻成 false = 那本词库被热摘了。
            yields_zzz: AtomicBool,
        }

        struct FlakyLayer(std::sync::Arc<Gate>);

        impl DictLayer for FlakyLayer {
            fn name(&self) -> &str {
                "flaky"
            }
            fn layer_type(&self) -> LayerType {
                LayerType::System
            }
            fn search(&self, _code: &str, _limit: usize) -> Vec<Candidate> {
                Vec::new()
            }
            fn search_prefix(&self, _p: &str, _limit: usize) -> Vec<Candidate> {
                Vec::new()
            }
            fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
                // ⚠️ 先吐词条**再**停。反过来的话，放行后读到的已经是翻转后的标志，
                // 构建结果本来就不含 zzz —— 两种实现都绿，什么也证不出来（第一版的错）。
                // 要照出缺陷，构建线程手上必须是**旧**视图。
                if self.0.yields_zzz.load(Relaxed) {
                    f("zzz", "zzz zzz", 100);
                }
                f("aaa", "aaa bbb", 100);
                if !self.0.stopped_once.swap(true, Relaxed) {
                    // 第一趟：告诉主线程「旧视图我已经读完了」，然后等它失效完再返回。
                    self.0.entered.send(()).unwrap();
                    self.0.resume.lock().unwrap().recv().unwrap();
                }
            }
        }

        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let gate = std::sync::Arc::new(Gate {
            entered: entered_tx,
            resume: Mutex::new(resume_rx),
            stopped_once: AtomicBool::new(false),
            yields_zzz: AtomicBool::new(true),
        });

        let dm = std::sync::Arc::new(DictManager::new());
        dm.register_layer(Box::new(FlakyLayer(std::sync::Arc::clone(&gate))));

        let lazy = std::sync::Arc::new(LazyPhraseIndex::new());
        let builder = {
            let (lazy, dm) = (std::sync::Arc::clone(&lazy), std::sync::Arc::clone(&dm));
            std::thread::spawn(move || lazy.get(&dm))
        };

        // 构建线程此刻停在 `for_each_entry` 的末尾，手上已是含 zzz 的旧视图。
        entered_rx.recv().expect("构建应已读完旧视图");
        gate.yields_zzz.store(false, Relaxed); // = 热摘掉那本词库
        lazy.invalidate();
        resume_tx.send(()).unwrap();

        builder.join().expect("构建线程");

        let zzz = |i: &PhraseSegIndex| -> Vec<String> {
            i.search(&split_segments("zzz", PHRASE_SEPARATOR), 10)
                .into_iter()
                .map(|c| c.text)
                .collect()
        };
        // 要紧的是**别人**读到的那份：缓存里不许躺着按旧词库建的索引。
        let after = zzz(&lazy.get(&dm));
        assert!(after.is_empty(), "缓存里躺着按旧词库建的索引：{after:?}");
        // 反面：主库那条必须还在，证明作废的是过时的那份，不是把功能连坐关掉了。
        assert_eq!(
            lazy.get(&dm)
                .search(&split_segments("aaa", PHRASE_SEPARATOR), 10)
                .len(),
            1,
            "主库词组不该受牵连"
        );
    }

    /// ★ 索引侧与查询侧的小写化必须是**同一个**函数。
    ///
    /// 判据非用希腊词尾 Σ 不可：`str::to_lowercase` 有上下文特例（`"ΟΔΟΣ"` → `"οδος"`），
    /// 逐字符 `char::to_lowercase` 恒给 σ（`"οδοσ"`）。Café / Über 这类常见多字节词两种
    /// 写法结果相同——既有的 `multibyte_words_slice_on_char_boundaries` 正是因此照不出
    /// 分叉，那条恒绿。
    ///
    /// 反向验证（变异）：把 `split_segments` 的 `.map(lower)` 换回 `.map(str::to_lowercase)`，
    /// 本用例立刻红——首段 `odos` 配不上索引里的 `οδοσ`，二分窗口为空。
    #[test]
    fn both_sides_lowercase_the_same_way() {
        // 首词与非首词各放一个词尾 Σ，两条匹配规则都要覆盖。
        let i = idx(&[("ΟΔΟΣ ΑΘΗΝΑΣ", "odos", 100)]);
        assert_eq!(
            texts(&i, "οδο'αθη"),
            vec!["ΟΔΟΣ ΑΘΗΝΑΣ"],
            "首段与非首段都得能配上"
        );
        // 用户打的是大写，查询侧也得归一到同一形态。
        assert_eq!(texts(&i, "ΟΔΟ'ΑΘΗ"), vec!["ΟΔΟΣ ΑΘΗΝΑΣ"]);
        // 整词（词尾 Σ 就在段末）——这是两种写法真正分叉的位置。
        assert_eq!(texts(&i, "οδος'αθη"), vec!["ΟΔΟΣ ΑΘΗΝΑΣ"]);
    }

    /// 首词有公共前缀的一族 —— 二分边界最容易错的地方（`ip` 的区间必须刚好收住
    /// `ipad`/`iphone`/`ipod`，既不漏 `ipod` 也不吃进 `internet`）。
    fn prefix_family() -> PhraseSegIndex {
        idx(&[
            ("iPad Pro", "ipadpro", 100),
            ("iPhone 15 Pro Max", "iphone", 100),
            ("iPhone 15 Pro", "iphone", 90),
            ("iPod Touch", "ipod", 80),
            ("Internet Explorer", "ie", 70),
            ("Buenos Aires", "buenosaires", 60),
            ("Mac OS X", "macosx", 50),
            ("Zulu Time", "zulu", 40),
            ("北京 大学", "bjdx", 30),
        ])
    }

    /// 朴素全表扫，只回答「命中哪些」。
    ///
    /// 刻意**不复制排序逻辑**：顺序自有 `weight_outranks_span` 那几条用例守着，这里再抄
    /// 一份三级比较器只会多一个会漂移的副本。用集合比对，测的是「二分有没有漏/多」。
    fn linear_hits(i: &PhraseSegIndex, input: &str) -> std::collections::BTreeSet<String> {
        let segs = split_segments(input, PHRASE_SEPARATOR);
        if segs.is_empty() {
            return Default::default();
        }
        i.entries()
            .filter(|e| i.match_entry(e, &segs).is_some())
            .map(|e| i.text(&e).unwrap().to_string())
            .collect()
    }

    fn search_hits(i: &PhraseSegIndex, input: &str) -> std::collections::BTreeSet<String> {
        i.search(&split_segments(input, PHRASE_SEPARATOR), 100)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    /// ★ 二分窗口必须与全表扫召回同一批条目。
    ///
    /// 反向验证（变异）：删掉 `finish()` 里那句 `sort_by`（并去掉它末尾的有序断言，模拟
    /// 「只是漏了排序」），本用例红在 **`ip'pro`** 上——2026-09-22 实跑的结果。
    ///
    /// ⚠️ 举例必须实跑，不能推。列表里第一个输入 `i'pro` **照不出来**：未排序时它的窗口
    /// 恰好仍然正确。所以这条用例的有效性来自「覆盖了一整族前缀」，不是某一个输入；
    /// 删掉别的输入只留 `i'pro`，它就变成假护栏了。
    #[test]
    fn the_binary_search_window_returns_exactly_what_a_full_scan_would() {
        let i = prefix_family();
        for input in [
            "i'pro",     // 区间跨 ipad/iphone/ipod 三族
            "ip'pro",    //
            "ipa'pro",   // 只剩 iPad
            "iph'max",   // 只剩一条
            "ipo'touch", // 区间**右端**那条，最容易被上界切掉
            "int'exp",   // 区间左邻，不得被吃进来
            "b'air",     // 全表最前
            "z'time",    // 全表最后（ASCII 段）
            "北'大",     // 多字节首词，排在全部 ASCII 之后
            "zz'x",      // 首词无人匹配 ⇒ 空区间
            "'",         // 空段全被剔除 ⇒ 空结果
        ] {
            assert_eq!(
                search_hits(&i, input),
                linear_hits(&i, input),
                "输入 {input:?} 上二分窗口与全表扫不一致"
            );
        }
    }

    /// ★ 窗口本身的边界：`ip` 收住三族、不吃 `internet`。
    ///
    /// 与上一条的分工：那条测「结果对不对」，这条测「少看了多少」——窗口若退化成全表，
    /// 结果照样正确，而本功能（把按键路径上的 18 万条扫描降下来）就白做了。
    #[test]
    fn the_first_word_window_is_exactly_the_prefix_block() {
        let i = prefix_family();
        let win: std::collections::BTreeSet<String> = i
            .first_word_range("ip")
            .map(|k| i.text(&i.entry(k)).unwrap().to_string())
            .collect();
        assert_eq!(
            win,
            [
                "iPad Pro",
                "iPhone 15 Pro",
                "iPhone 15 Pro Max",
                "iPod Touch"
            ]
            .into_iter()
            .map(String::from)
            .collect::<std::collections::BTreeSet<_>>()
        );
        assert!(
            i.first_word_range("zz").is_empty(),
            "无人匹配的首词该给出空窗口"
        );
        // 空前缀 = 全表。⚠️ 这**不是**一条按键路径：`split_segments` 已经
        // `filter(|s| !s.is_empty())`，`segs[0]` 永不为空（用户刚按下分词符时是
        // `ip'` ⇒ `segs = ["ip"]`）。留这条是钉住 `partition_point` 在空前缀上的
        // 边界行为，免得将来有人「优化」成空前缀直接返回空切片。
        assert_eq!(
            i.first_word_range("").len(),
            i.len(),
            "空前缀该给出全表窗口"
        );
    }

    /// 两种编码方案都靠 text 命中——这是整个设计的立足点。
    #[test]
    fn matches_both_encoding_schemes() {
        let i = idx(&[
            ("Buenos Aires", "buenosaires", 100), // 拼接式
            ("iPhone 15 Pro", "iphone", 100),     // 共同前缀式：code 里没有 15/Pro
        ]);
        assert_eq!(texts(&i, "bue'air"), vec!["Buenos Aires"]);
        assert_eq!(texts(&i, "ip'15"), vec!["iPhone 15 Pro"]);
    }

    /// 跳词：`ip'pro` 越过 `15` 命中 `Pro`。这是 2026-09-18 拍板的「优先保效果」。
    #[test]
    fn skips_intervening_words() {
        let i = idx(&[("iPhone 15 Pro", "iphone", 100)]);
        assert_eq!(texts(&i, "ip'pro"), vec!["iPhone 15 Pro"]);
    }

    /// 跳词不等于乱序：段序必须与词序一致。
    #[test]
    fn skipping_still_requires_order() {
        let i = idx(&[("Mac OS X Snow Leopard", "macosx", 100)]);
        assert_eq!(texts(&i, "mac'snow"), vec!["Mac OS X Snow Leopard"]);
        // `leopard` 在 `snow` 之后，反过来打就不该命中。
        assert!(
            texts(&i, "mac'leo'snow").is_empty(),
            "段序与词序相反时不得命中"
        );
    }

    /// 首段锚定第一个词：中段词不能当入口。
    #[test]
    fn first_segment_must_anchor_the_first_word() {
        let i = idx(&[("iPhone 15 Pro", "iphone", 100)]);
        assert!(
            texts(&i, "pro'").is_empty(),
            "`pro` 不是首词前缀，不得从中段进入"
        );
    }

    /// 跨度是**次级**键：同权重时跳得少的排前面。
    ///
    /// ⚠️ 两条 weight 必须**相等**，本用例才测得到跨度。weight 不等的话主键就分出了胜负，
    /// 「有没有跨度这一级」根本看不出来。
    #[test]
    fn tighter_span_breaks_ties_within_the_same_weight() {
        let i = idx(&[
            ("iPhone 15 Pro Max", "iphone", 100),
            ("iPhone Pro", "iphone", 100),
        ]);
        assert_eq!(
            texts(&i, "ip'pro"),
            vec!["iPhone Pro", "iPhone 15 Pro Max"],
            "同权重下跳 0 个词的应排在跳 1 个词的前面"
        );
    }

    /// ★ 而 weight 是**主键**：权重更高的排前面，哪怕它跨度更大。
    ///
    /// 这条钉的是 AGENTS.md 那条硬约定的落地——协调器会按 weight 统一重排，引擎内序
    /// 若以跨度为主键，到了用户眼前就是另一个顺序（两个作用域还会各不相同）。
    /// 与上一条构成对照的两半：缺了它，「跨度优先」的旧实现同样能过上一条。
    #[test]
    fn weight_outranks_span() {
        let i = idx(&[
            ("iPhone 15 Pro Max", "iphone", 900),
            ("iPhone Pro", "iphone", 10),
        ]);
        assert_eq!(
            texts(&i, "ip'pro"),
            vec!["iPhone 15 Pro Max", "iPhone Pro"],
            "weight 是主键：高权重的排前面，跨度只在同权重时才说话"
        );
    }

    /// weight 主键在同跨度的条目之间同样生效（与 `weight_outranks_span` 互补：那条跨度
    /// 不同、这条跨度相同）。
    #[test]
    fn weight_orders_entries_of_equal_span() {
        let i = idx(&[
            ("iPhone 15 Pro", "iphone", 10),
            ("iPhone 16 Pro", "iphone", 900),
        ]);
        assert_eq!(
            texts(&i, "ip'pro"),
            vec!["iPhone 16 Pro", "iPhone 15 Pro"],
            "同跨度下按 weight 降序"
        );
    }

    /// 末尾空段被剔除：打到 `ip'` 的那一刻候选不该整片消失。
    #[test]
    fn trailing_empty_segment_is_dropped() {
        let i = idx(&[("iPhone 15 Pro", "iphone", 100)]);
        assert_eq!(split_segments("ip'", '\''), vec!["ip".to_string()]);
        assert_eq!(texts(&i, "ip'"), vec!["iPhone 15 Pro"]);
    }

    /// 单词条目不进索引——它们走原本的 Trie 前缀匹配，不该在这里被重复召回。
    #[test]
    fn single_word_entries_are_not_indexed() {
        let i = idx(&[
            ("hello", "hello", 100),
            ("Buenos Aires", "buenosaires", 100),
        ]);
        assert_eq!(i.len(), 1);
    }

    /// ★ **每条词组的堆开销上界**。这是本模块唯一的内存护栏。
    ///
    /// 缘起：真机上这张表有 18 万条（用户自备英文词库，出厂只有 787 条），旧结构每条
    /// 持有 `Vec<Box<str>>` + 两个 `Box<str>`，约 90 万次小分配，实测常驻 **48 MB**，
    /// 而它被建了两份（english 方案 + 混输的 english 子引擎）⇒ 96 MB。
    ///
    /// 上界取 120 字节/条：arena 版实测约 78（entry 28 + word_ends 10 + lower 15 + raw 25），
    /// 留出的余量够容纳词长分布的波动，但**挡得住退回 `Box<str>`**——那一版光
    /// `Vec` + 两个 `Box` 的头部就已经是 56 字节，加上每词一次分配的分配器开销必然超线。
    ///
    /// ⚠️ 样本必须**足够多且带多词条目**：条数太少时 arena 的翻倍扩容尾巴会摊到分母上，
    /// 测出来的是扩容策略而不是结构本身。
    #[test]
    fn heap_cost_per_phrase_stays_within_budget() {
        let pairs: Vec<(String, String, i32)> = (0..2000)
            .map(|i| {
                (
                    format!("iPhone {i} Pro Max Ultra"),
                    format!("iphone{i}"),
                    100,
                )
            })
            .collect();
        let mut b = PhraseSegBuilder::default();
        for (text, code, w) in &pairs {
            b.push(code, text, *w);
        }
        // ⚠️ 走生产同一条收尾（`finish` → 镜像），别手抄——抄一份的话，将来改布局时这里会
        // 静默地测一个生产里不存在的形态。这正是 `idx()` 夹具注释点名批评的漂移模式。
        let me = PhraseSegIndex::from_image(b.finish());

        // ★ 这一条堵的是上面那条护栏的**漏网口**：`heap_bytes` 只统计镜像那一块，
        // 谁要是往 `PhraseEntry` 里加回一个 `Box<str>`（16 B）或 `Vec<Box<str>>`（24 B），
        // 那份堆内存**不会被 `heap_bytes` 统计到**，上面的预算断言照样绿。
        // 用 `size_of` 直接钉住「条目里不许出现指针」，这是编译期事实，绕不过去。
        // 它也与镜像里一条记录的定长一致：解码值比记录还大，说明多了不该有的字段。
        assert!(
            size_of::<PhraseEntry>() <= ENTRY_SIZE,
            "PhraseEntry 涨到 {} 字节——加了指针字段？条目必须只存定长偏移",
            size_of::<PhraseEntry>()
        );
        assert_eq!(me.len(), 2000, "全部应进索引（每条 5 个词）");
        let per = me.heap_bytes() / me.len();
        // ⚠️ 预算按**本夹具**定，不是按生产表定：这里每条 5 个词（`iPhone {i} Pro Max
        // Ultra`），实测 per≈102；生产表平均 2~3 词、靶机 18 万条实测 per≈74。拿 120 卡
        // 5 词的夹具只剩 15% 余量，夹具字符串一改长就会在结构毫无退化时假红，故留到 140。
        // 真正钉住「不许退回 per-entry 堆分配」的是下面那条 `size_of` 断言，它是编译期
        // 事实、绕不过去；本条只是粗筛。
        assert!(
            per < 140,
            "每条词组堆开销 {per} 字节，超出预算 140——退回 per-entry 堆分配了？\n             （总计 {} KB / {} 条，本夹具每条 5 个词）",
            me.heap_bytes() / 1024,
            me.len()
        );
    }

    /// 多字节字符不能把偏移算错：arena 存的是**字节**偏移，切片边界必须落在字符边界上。
    /// 旧结构各词独立成串，天然不会切错；arena 把它们首尾相接之后这就成了真实风险。
    #[test]
    fn multibyte_words_slice_on_char_boundaries() {
        let i = idx(&[("Café Noir Über", "cafe", 100)]);
        assert_eq!(texts(&i, "caf'noir"), vec!["Café Noir Über"]);
        // 小写化后 Ü → ü，段用小写打
        assert_eq!(texts(&i, "caf'üb"), vec!["Café Noir Über"]);
    }

    /// 段比词多时不命中——别让 `a'b'c` 匹配上只有两个词的条目。
    #[test]
    fn more_segments_than_words_never_matches() {
        let i = idx(&[("Buenos Aires", "buenosaires", 100)]);
        assert!(texts(&i, "bue'air'x").is_empty());
    }

    // ───────────────────────── 落盘 / mmap ─────────────────────────

    /// 每个用例一个干净的临时目录（`TMPDIR` 下，进程号 + 用例名区分，可并行）。
    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wind_wphr_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 一次查询的完整可观测结果：顺序、text、code、weight、natural_order 全在里面。
    /// 对拍比的是这个，不是集合——堆版与 mmap 版连**顺序**都必须一致。
    fn full(i: &PhraseSegIndex, input: &str) -> Vec<(String, String, i32, i32)> {
        i.search(&split_segments(input, PHRASE_SEPARATOR), 50)
            .into_iter()
            .map(|c| (c.text, c.code, c.weight, c.natural_order))
            .collect()
    }

    /// 对拍用的查询：共同前缀式与拼接式、多段、跳词、大小写、多字节、空段、边界。
    const PROBES: &[&str] = &[
        "i'pro",
        "ip'pro",
        "IP'PRO",
        "ipa'pro",
        "iph'max",
        "iph'15'max",
        "ipo'touch",
        "int'exp",
        "b'air",
        "bue'air",
        "z'time",
        "北'大",
        "οδο'αθη",
        "οδος'αθη",
        "caf'üb",
        "zz'x",
        "'",
        "ip'",
        "ip''pro",
        "mac'snow",
        "mac'leo'snow",
        "a'b'c'd'e'f",
        "word0123'gam",
        "word1'b'g",
    ];

    /// 对拍夹具：`prefix_family` 那一族 + 多字节 / 希腊词尾 Σ / 跳词。
    fn probe_pairs() -> Vec<(String, String, i32)> {
        let mut v: Vec<(String, String, i32)> = [
            ("iPad Pro", "ipadpro", 100),
            ("iPhone 15 Pro Max", "iphone", 100),
            ("iPhone 15 Pro", "iphone", 90),
            ("iPhone Pro", "iphone", 90),
            ("iPod Touch", "ipod", 80),
            ("Internet Explorer", "ie", 70),
            ("Buenos Aires", "buenosaires", 60),
            ("Mac OS X Snow Leopard", "macosx", 50),
            ("Zulu Time", "zulu", 40),
            ("北京 大学", "bjdx", 30),
            ("ΟΔΟΣ ΑΘΗΝΑΣ", "odos", 20),
            ("Café Noir Über", "cafe", 10),
            ("hello", "hello", 100), // 单词，不进索引
        ]
        .iter()
        .map(|(t, c, w)| (t.to_string(), c.to_string(), *w))
        .collect();
        for i in 0..300u32 {
            v.push((
                format!("Word{i:04} Beta Gamma{i:03}"),
                format!("w{i}"),
                (i % 7) as i32,
            ));
        }
        v
    }

    /// `fp` 为空 = 进程内镜像；非空 = 落盘形态（头里带指纹）。
    fn build_image(pairs: &[(String, String, i32)], fp: &str) -> Vec<u8> {
        let mut b = PhraseSegBuilder::default();
        for (t, c, w) in pairs {
            b.push(c, t, *w);
        }
        b.sort();
        b.into_image(fp)
    }

    const FP: &str = "0123456789abcdef";

    /// 镜像写盘，再以「常驻读入」「mmap」两种方式打开。
    /// 走生产的落盘写法（`write_image` 流式写文件），而不是把内存镜像原样倒进去——
    /// 后者测不到「流式写出的字节与内存镜像一致」这一条。
    fn open_both(pairs: &[(String, String, i32)], dir: &Path) -> (PhraseSegIndex, PhraseSegIndex) {
        let mut b = PhraseSegBuilder::default();
        for (t, c, w) in pairs {
            b.push(c, t, *w);
        }
        b.sort();
        let p = dir.join("t.wphr");
        write_atomically(&p, |w| b.write_image(FP, w)).unwrap();
        let resident = PhraseSegIndex::open(&p, FP, usize::MAX).unwrap();
        let mapped = PhraseSegIndex::open(&p, FP, 0).unwrap();
        (resident, mapped)
    }

    /// ★ 堆版、常驻读入版、mmap 版对同一批查询**逐条**相同（含顺序、code、weight）。
    ///
    /// 三者共用同一套查找代码，这条测的是「落盘—读回」这段链路没有走样（盖指纹盖错位置、
    /// 写盘写坏、读回读错）。
    ///
    /// ⚠️ 它**照不出对称的解码错误**：`entry()` 里把 `text_len` 读成 `code_len`，三个版本
    /// 错得一模一样、对拍照样绿（实跑如此）。那一类由 `matches_both_encoding_schemes` 等
    /// 语义用例兜（同一变异下它们红）。
    ///
    /// 反向验证（变异）：让 `write_atomically` 落盘前把首条记录的 `raw_start` 翻一位，
    /// 本条红在 `b'air` 上（2026-10-09 实跑）。
    #[test]
    fn heap_resident_and_mapped_views_agree_on_every_probe() {
        let dir = tmp_dir("agree");
        let image = build_image(&probe_pairs(), "");
        let heap = PhraseSegIndex::from_image(image.clone());
        let (resident, mapped) = open_both(&probe_pairs(), &dir);

        assert!(
            mapped.mapped_bytes() > 0 && mapped.heap_bytes() == 0,
            "mmap 不该计入堆"
        );
        assert!(resident.mapped_bytes() == 0 && resident.heap_bytes() >= image.len());
        assert_eq!(heap.len(), 312, "300 条合成 + 12 条词组，单词 hello 不进");
        let mut nonempty = 0;
        for q in PROBES {
            let want = full(&heap, q);
            nonempty += usize::from(!want.is_empty());
            assert_eq!(
                full(&resident, q),
                want,
                "常驻读入版在 {q:?} 上与堆版不一致"
            );
            assert_eq!(full(&mapped, q), want, "mmap 版在 {q:?} 上与堆版不一致");
        }
        // 防假绿：探针得真的命中东西，全空的对拍什么也证明不了。
        assert!(nonempty >= 15, "探针命中太少（{nonempty}），对拍失去意义");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 真机量级（18 万条）下堆版与 mmap 版对拍。
    ///
    /// 合成词库模仿靶机那份的两个特征：两种编码方案并存（拼接式 / 共同前缀式），以及
    /// 首词大量共享前缀（二分窗口里有成百上千条，跨度与 weight 的排序真正起作用）。
    #[test]
    fn heap_and_mapped_agree_on_a_180k_synthetic_dictionary() {
        let dir = tmp_dir("agree180k");
        let pairs = synthetic_pairs(180_000);
        let image = build_image(&pairs, "");
        let heap = PhraseSegIndex::from_image(image.clone());
        let (_, mapped) = open_both(&pairs, &dir);
        assert_eq!(heap.len(), 180_000);
        assert!(mapped.heap_bytes() == 0 && mapped.mapped_bytes() == image.len());
        for q in [
            "alpha'be",
            "alp'b'g",
            "Alpha0'Be",
            "be'ga",
            "gam'del",
            "zeta'e",
            "alpha1234'beta",
            "omega9'x",
            "alpha'zz",
            "q'q",
            "delta'",
        ] {
            assert_eq!(full(&mapped, q), full(&heap, q), "18 万条下 {q:?} 不一致");
        }
        assert!(
            !full(&heap, "alp'b'g").is_empty(),
            "防假绿：主力探针必须有命中"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 18 万条合成词组（探针与 `wind-engine` 的内存探针同一个生成器思路）。
    fn synthetic_pairs(n: u32) -> Vec<(String, String, i32)> {
        const HEADS: [&str; 8] = [
            "Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Omega", "Sigma",
        ];
        (0..n)
            .map(|i| {
                let h = HEADS[(i % 8) as usize];
                let text = match i % 3 {
                    0 => format!("{h}{} Beta Gamma{}", i % 5000, i % 97),
                    1 => format!("{h}{} Beta", i % 5000),
                    _ => format!("{h}{} Delta Epsilon Zeta{}", i % 5000, i),
                };
                // 偶数条拼接式、奇数条共同前缀式（code 只有首词）。
                let code = if i % 2 == 0 {
                    text.to_ascii_lowercase().replace(' ', "")
                } else {
                    text.split(' ').next().unwrap().to_ascii_lowercase()
                };
                (text, code, (i % 1000) as i32)
            })
            .collect()
    }

    /// 截断 / magic / 版本 / 指纹不符：`open` 一律 `Err`（调用方据此重建），不 panic。
    #[test]
    fn open_rejects_truncated_or_foreign_files() {
        let dir = tmp_dir("reject");
        let image = build_image(&probe_pairs(), FP);
        let p = dir.join("x.wphr");
        let try_open = |bytes: &[u8]| {
            std::fs::write(&p, bytes).unwrap();
            PhraseSegIndex::open(&p, FP, 0).map(|_| ())
        };
        assert!(try_open(&image).is_ok(), "前提：完好的文件能打开");
        for cut in [
            0,
            10,
            HEADER_SIZE - 1,
            HEADER_SIZE,
            image.len() / 2,
            image.len() - 1,
        ] {
            assert!(try_open(&image[..cut]).is_err(), "截断到 {cut} 字节应被拒");
        }
        let mut bad = image.clone();
        bad[0] = b'X';
        assert!(try_open(&bad).is_err(), "magic 不符应被拒");
        let mut bad = image.clone();
        bad[4] = 99;
        assert!(try_open(&bad).is_err(), "版本不符应被拒");
        std::fs::write(&p, &image).unwrap();
        assert!(
            PhraseSegIndex::open(&p, "fedcba9876543210", 0).is_err(),
            "指纹不符应被拒（改名 / 拷错的文件不能当命中）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 段**内**被写坏（头部完好、偏移乱指）：查询不 panic，最多查不中。
    ///
    /// 头部校验挡不住这一类——它只验各段整体在界内。按键链路上的 `convert` 不许 panic。
    #[test]
    fn scribbled_entries_never_panic() {
        let image = build_image(&probe_pairs(), "");
        // 确定性「随机」：线性同余，每 7 字节翻一次，覆盖 entries / word_ends / arena 全段。
        let mut seed = 0x2545_f491_u32;
        for round in 0..20 {
            let mut bad = image.clone();
            for b in bad[HEADER_SIZE..].iter_mut().step_by(7 + round) {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                *b ^= (seed >> 16) as u8;
            }
            let i = PhraseSegIndex::parse(ImageData::Owned(bad)).expect("头部没动，应能解析");
            for q in PROBES {
                let _ = full(&i, q);
            }
        }

        // 写坏的 `word_count` 不许把跳词循环拖成几十亿步（首版实测：上面 20 轮跑了 48 s）。
        // 这里造一条确定的：首条词数改成 u32::MAX，段数 2 ⇒ 无守卫时要空转 ~4e9 次。
        // 反向验证（变异）：删掉 `match_entry` 里「词表越界」那道守卫，本条红在耗时上。
        let mut bad = image.clone();
        bad[HEADER_SIZE + 20..HEADER_SIZE + 24].copy_from_slice(&u32::MAX.to_le_bytes());
        let i = PhraseSegIndex::parse(ImageData::Owned(bad)).unwrap();
        let t0 = std::time::Instant::now();
        let first = i.entry(0);
        assert!(
            i.match_entry(&first, &split_segments("b'zzz", '\''))
                .is_none()
        );
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(2),
            "损坏的词数让匹配空转了 {:?}",
            t0.elapsed()
        );
    }

    // ── LazyPhraseIndex 的落盘接线 ──────────────────────────────────

    /// 测试层：内容、摘要、启用标志都可控；`for_each_entry` 计次。
    struct DigestLayer {
        name: String,
        entries: Vec<(String, String, i32)>,
        enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// `None` = 说不清摘要（模拟内存词库）。
        has_digest: bool,
    }

    impl DigestLayer {
        fn new(name: &str, entries: &[(&str, &str, i32)]) -> Self {
            Self {
                name: name.into(),
                entries: entries
                    .iter()
                    .map(|(t, c, w)| (t.to_string(), c.to_string(), *w))
                    .collect(),
                enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                has_digest: true,
            }
        }
    }

    impl wind_dict::DictLayer for DigestLayer {
        fn name(&self) -> &str {
            &self.name
        }
        fn layer_type(&self) -> wind_dict::LayerType {
            wind_dict::LayerType::System
        }
        fn search(&self, _code: &str, _limit: usize) -> Vec<Candidate> {
            Vec::new()
        }
        fn search_prefix(&self, _p: &str, _limit: usize) -> Vec<Candidate> {
            Vec::new()
        }
        fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
            for (t, c, w) in &self.entries {
                f(c, t, *w);
            }
        }
        fn entries_digest(&self) -> Option<String> {
            self.has_digest.then(|| format!("{}|v1", self.name))
        }
        fn enabled(&self) -> bool {
            self.enabled.load(Relaxed)
        }
        fn set_enabled(&self, on: bool) {
            self.enabled.store(on, Relaxed);
        }
    }

    const MAIN: &[(&str, &str, i32)] = &[
        ("Buenos Aires", "buenosaires", 60),
        ("Zulu Time", "zulu", 40),
    ];
    const EXT: &[(&str, &str, i32)] = &[
        ("iPhone 15 Pro Max", "iphone", 100),
        ("iPod Touch", "ipod", 80),
    ];

    fn two_layer_dm() -> DictManager {
        let dm = DictManager::new();
        dm.register_layer(Box::new(DigestLayer::new("main", MAIN)));
        dm.register_layer(Box::new(DigestLayer::new("ext", EXT)));
        dm
    }

    fn lazy_texts(lazy: &LazyPhraseIndex, dm: &DictManager, input: &str) -> Vec<String> {
        lazy.get(dm)
            .search(&split_segments(input, PHRASE_SEPARATOR), 20)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    fn wphr_files(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        v.retain(|p| p.extension().is_some_and(|x| x == CACHE_EXT));
        v.sort();
        v
    }

    /// ★ 第二个实例（= 下次启动）命中盘上那份，**一次全表扫都不做**，结果与首建相同。
    ///
    /// 反向验证（变异）：把 `load_or_build` 的 ① 整段删掉（每次都重扫），本条红在
    /// `full_scans() == 0` 上。
    #[test]
    fn a_second_instance_reuses_the_cache_without_scanning() {
        let dir = tmp_dir("hit");
        let dm = two_layer_dm();
        let first = LazyPhraseIndex::with_cache_dir(Some(dir.clone())).with_resident_max(0);
        let want = lazy_texts(&first, &dm, "ip'max");
        assert_eq!(want, vec!["iPhone 15 Pro Max"]);
        assert_eq!(first.full_scans(), 1);
        assert_eq!(wphr_files(&dir).len(), 1, "首建应落盘一份");
        // 首建之后就是从盘上打开的那份：mmap，堆为 0。
        assert_eq!(first.heap_bytes_if_built(), Some(0));
        assert!(first.mapped_bytes_if_built().unwrap() > 0);

        let second = LazyPhraseIndex::with_cache_dir(Some(dir.clone())).with_resident_max(0);
        assert_eq!(lazy_texts(&second, &dm, "ip'max"), want);
        assert_eq!(second.full_scans(), 0, "命中缓存不该再全表扫");
        assert_eq!(lazy_texts(&second, &dm, "bue'air"), vec!["Buenos Aires"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 小镜像（不超过阈值）读进内存、不 mmap——与 `.wridx` 的 3 MB 分界同一取舍。
    #[test]
    fn a_small_cache_is_read_into_memory() {
        let dir = tmp_dir("small");
        let dm = two_layer_dm();
        let _ = lazy_texts(
            &LazyPhraseIndex::with_cache_dir(Some(dir.clone())),
            &dm,
            "ip",
        );
        let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
        assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
        assert_eq!(lazy.full_scans(), 0);
        assert_eq!(
            lazy.mapped_bytes_if_built(),
            Some(0),
            "几百字节的镜像不该 mmap"
        );
        assert!(lazy.heap_bytes_if_built().unwrap() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 热摘词库：按**新的启用集合**取另一份，不读回旧的；再启用回来直接复用先前那份。
    ///
    /// 反向验证（变异）：把 `CompositeDict::entries_digest` 的 `.filter(|l| l.enabled())`
    /// 删掉（摘要不随启停变），本条红在「关掉 ext 后不该再召回 iPhone」上——读回了旧文件。
    #[test]
    fn toggling_a_dictionary_switches_caches_and_switching_back_reuses() {
        let dir = tmp_dir("toggle");
        let dm = two_layer_dm();
        let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
        assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
        assert_eq!(lazy.full_scans(), 1);

        assert!(dm.set_layer_enabled("ext", false));
        lazy.invalidate();
        assert!(
            lazy_texts(&lazy, &dm, "ip'max").is_empty(),
            "关掉 ext 后不该再召回它的词组（读回了旧缓存？）"
        );
        assert_eq!(lazy_texts(&lazy, &dm, "bue'air"), vec!["Buenos Aires"]);
        assert_eq!(lazy.full_scans(), 2, "新集合第一次要扫一遍");
        assert_eq!(wphr_files(&dir).len(), 2, "两个集合各一份");

        assert!(dm.set_layer_enabled("ext", true));
        lazy.invalidate();
        assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
        assert_eq!(lazy.full_scans(), 2, "切回原集合应复用先前那份，不再扫");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 词库内容变了（摘要变）⇒ 换一份重建，不复用旧内容。
    ///
    /// 真实词库的摘要来自 wdat 的 `.fp`（内容指纹）或 `大小:mtime`，见
    /// `SystemDictLayer::entries_digest`（wind-dict 另有用例钉住它随文件变）；这里钉的是
    /// 「摘要一变，这一层就不会再读回旧文件」。
    #[test]
    fn a_changed_dictionary_is_rebuilt() {
        let dir = tmp_dir("changed");
        let dm = DictManager::new();
        let layer = DigestLayer::new("main", MAIN);
        let version = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1));
        // 版本号放在共享原子里，层注册进去之后仍能从外面改。
        struct Shared(DigestLayer, std::sync::Arc<std::sync::atomic::AtomicU32>);
        impl wind_dict::DictLayer for Shared {
            fn name(&self) -> &str {
                self.0.name()
            }
            fn layer_type(&self) -> wind_dict::LayerType {
                self.0.layer_type()
            }
            fn search(&self, c: &str, l: usize) -> Vec<Candidate> {
                self.0.search(c, l)
            }
            fn search_prefix(&self, p: &str, l: usize) -> Vec<Candidate> {
                self.0.search_prefix(p, l)
            }
            fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
                self.0.for_each_entry(f);
                if self.1.load(Relaxed) >= 2 {
                    f("newp", "New Phrase", 1);
                }
            }
            fn entries_digest(&self) -> Option<String> {
                Some(format!("main|v{}", self.1.load(Relaxed)))
            }
        }
        dm.register_layer(Box::new(Shared(layer, std::sync::Arc::clone(&version))));

        let a = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
        assert!(lazy_texts(&a, &dm, "new'p").is_empty());

        version.store(2, Relaxed); // 词库被改过
        let b = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
        assert_eq!(lazy_texts(&b, &dm, "new'p"), vec!["New Phrase"]);
        assert_eq!(b.full_scans(), 1, "内容变了必须重建");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 盘上那份被截断 / 写坏 ⇒ 重建并覆盖，不 panic；覆盖后下一次又能命中。
    #[test]
    fn a_corrupt_cache_file_is_rebuilt_and_overwritten() {
        let dir = tmp_dir("corrupt");
        let dm = two_layer_dm();
        let _ = lazy_texts(
            &LazyPhraseIndex::with_cache_dir(Some(dir.clone())),
            &dm,
            "ip",
        );
        let files = wphr_files(&dir);
        assert_eq!(files.len(), 1);
        for garbage in [
            &b"WPHR\x01\x00"[..],
            &b"not a phrase index at all, just junk......"[..],
        ] {
            std::fs::write(&files[0], garbage).unwrap();
            let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
            assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
            assert_eq!(lazy.full_scans(), 1, "坏文件应触发重建");
            let again = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
            assert_eq!(lazy_texts(&again, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
            assert_eq!(again.full_scans(), 0, "重建应已覆盖坏文件");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 写盘失败（缓存目录不可建）⇒ 退回堆上建，结果照常。
    #[test]
    fn a_write_failure_falls_back_to_the_heap() {
        let dir = tmp_dir("nowrite");
        let blocker = dir.join("i_am_a_file");
        std::fs::write(&blocker, b"x").unwrap();
        let dm = two_layer_dm();
        // 目录路径实际是个普通文件 ⇒ create_dir_all 失败。
        let lazy = LazyPhraseIndex::with_cache_dir(Some(blocker.join("sub")));
        assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
        assert!(lazy.heap_bytes_if_built().unwrap() > 0, "应退回常驻堆");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 有层说不清摘要（内存词库）⇒ 不落盘：没有稳定产物可比，写下来的缓存无从校验。
    #[test]
    fn an_undigestable_layer_disables_persistence() {
        let dir = tmp_dir("nodigest");
        let dm = DictManager::new();
        let mut l = DigestLayer::new("mem", MAIN);
        l.has_digest = false;
        dm.register_layer(Box::new(l));
        let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
        assert_eq!(lazy_texts(&lazy, &dm, "bue'air"), vec!["Buenos Aires"]);
        assert!(wphr_files(&dir).is_empty(), "说不清摘要时不该落盘");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 扫描与启停交错：扫到的内容对不上扫描前取的指纹 ⇒ **不落盘**。
    ///
    /// 不设这道闸的话，按新集合扫出的内容会躺进以旧集合命名的文件；文件跨重启复用，
    /// 下次以旧集合启动时就读到了错的词组——比「这次关了没反应」更难查。
    ///
    /// 时序是摆明的：被扫的层在吐完自己的词条之后亲手把另一层停掉，于是「扫描前」与
    /// 「扫描后」的启用集合必然不同。
    ///
    /// 反向验证（变异）：删掉 `load_or_build` ③ 那段指纹复核，本条红（落了一份文件）。
    #[test]
    fn a_scan_that_races_a_toggle_is_not_persisted() {
        struct Toggler {
            inner: DigestLayer,
            other: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }
        impl wind_dict::DictLayer for Toggler {
            fn name(&self) -> &str {
                self.inner.name()
            }
            fn layer_type(&self) -> wind_dict::LayerType {
                self.inner.layer_type()
            }
            fn search(&self, c: &str, l: usize) -> Vec<Candidate> {
                self.inner.search(c, l)
            }
            fn search_prefix(&self, p: &str, l: usize) -> Vec<Candidate> {
                self.inner.search_prefix(p, l)
            }
            fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
                self.inner.for_each_entry(f);
                self.other.store(false, Relaxed); // 扫描进行中，另一本被停用
            }
            fn entries_digest(&self) -> Option<String> {
                self.inner.entries_digest()
            }
        }
        let dir = tmp_dir("race");
        let dm = DictManager::new();
        let ext = DigestLayer::new("ext", EXT);
        let ext_flag = std::sync::Arc::clone(&ext.enabled);
        dm.register_layer(Box::new(Toggler {
            inner: DigestLayer::new("main", MAIN),
            other: ext_flag,
        }));
        dm.register_layer(Box::new(ext));
        let lazy = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
        let _ = lazy.get(&dm);
        assert!(
            wphr_files(&dir).is_empty(),
            "扫描期间启用集合变了，这份内容不能以扫描前的指纹落盘"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同目录只留最近 [`PHRASE_INDEX_CACHE_KEEP`] 份 `phrase-*.wphr`，别的文件不碰。
    #[test]
    fn old_caches_are_pruned_but_foreign_files_are_left_alone() {
        let dir = tmp_dir("prune");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for k in 0..6 {
            let p = dir.join(format!("phrase-old{k}.{CACHE_EXT}"));
            std::fs::write(&p, b"x").unwrap();
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(old + std::time::Duration::from_secs(k))
                .unwrap();
        }
        std::fs::write(dir.join("english.wdat"), b"keep me").unwrap();
        let dm = two_layer_dm();
        let _ = lazy_texts(
            &LazyPhraseIndex::with_cache_dir(Some(dir.clone())),
            &dm,
            "ip",
        );
        let left = wphr_files(&dir);
        assert_eq!(
            left.len(),
            PHRASE_INDEX_CACHE_KEEP,
            "应只留 {PHRASE_INDEX_CACHE_KEEP} 份：{left:?}"
        );
        // 留下的是新建那份 + 最近的三份旧的（old5/old4/old3）。
        assert!(
            left.iter()
                .any(|p| p.ends_with(format!("phrase-old5.{CACHE_EXT}")))
        );
        assert!(
            !left
                .iter()
                .any(|p| p.ends_with(format!("phrase-old0.{CACHE_EXT}")))
        );
        assert!(dir.join("english.wdat").exists(), "别的缓存产物不许碰");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ wdat 换了内容、`.fp` 却没跟上（写 `.fp` 失败的现场）：词组索引必须重建，
    /// 不得召回新 wdat 里已没有的词组。
    ///
    /// 旧摘要只看 `.fp`，这种现场下它与换内容之前逐字相同。现在 `SystemDictLayer` 给
    /// 词组索引的摘要另混入 wdat 自身的 `大小:mtime`（只混进这一处，不动 `.wridx` 共用的
    /// `cache_digest`——那会让全体用户的反查索引白重建一次）。
    ///
    /// 反向验证（变异）：把 `entries_digest` 里那段 `大小:mtime` 去掉，本条红在 `ip'max`
    /// 仍召回 iPhone 上。
    #[test]
    fn a_rewritten_wdat_with_a_stale_fp_is_rebuilt() {
        use wind_dict::SystemDictLayer;
        use wind_dict::cached::CachedDict;
        let dir = tmp_dir("stalefp");
        let (yaml, wdat, cache) = (dir.join("en.dict.yaml"), dir.join("en.wdat"), dir.join("c"));
        let mut fp = wdat.clone().into_os_string();
        fp.push(".fp");
        std::fs::write(
            &yaml,
            "---\nname: en\n...\nBuenos Aires\tbuenosaires\t60\niPhone 15 Pro Max\tiphone\t100\n",
        )
        .unwrap();
        {
            let dm = DictManager::new();
            dm.register_layer(Box::new(SystemDictLayer::new(
                CachedDict::load_at_with(&yaml, &wdat, true).unwrap(),
                "en",
            )));
            let lazy = LazyPhraseIndex::with_cache_dir(Some(cache.clone()));
            assert_eq!(lazy_texts(&lazy, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
        }
        // wdat 被换成不含 iPhone 的版本，`.fp` 仍是旧的。
        let stale = std::fs::read(&fp).unwrap();
        let mut d = wind_dict::codetable::CodetableDict::empty();
        d.merge_single("buenosaires".into(), "Buenos Aires".into(), 60, 0);
        let mut w = wind_dict::datformat::WdatWriter::new();
        d.export_to_wdat(&mut w);
        w.write(&wdat).unwrap();
        std::fs::write(&fp, &stale).unwrap();

        let dm = DictManager::new();
        dm.register_layer(Box::new(SystemDictLayer::new(
            CachedDict::Mmap(wind_dict::reader_pool::open_wdat(&wdat).unwrap()),
            "en",
        )));
        let lazy = LazyPhraseIndex::with_cache_dir(Some(cache.clone()));
        assert!(
            lazy_texts(&lazy, &dm, "ip'max").is_empty(),
            "新 wdat 里已没有 iPhone，却从旧索引召回了它"
        );
        assert_eq!(lazy.full_scans(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 崩溃留下的 `phrase-*.tmp`：超过 1 小时的清掉，新鲜的（可能正被另一实例写）不碰，
    /// 别人的 `.tmp` 不碰。
    #[test]
    fn stale_temp_files_from_a_crash_are_cleaned() {
        let dir = tmp_dir("tmpclean");
        let set_age = |p: &Path, secs: u64| {
            std::fs::write(p, b"x").unwrap();
            let f = std::fs::File::options().write(true).open(p).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs))
                .unwrap();
        };
        let old = dir.join(format!("phrase-aaaa.{CACHE_EXT}.999-0.tmp"));
        let fresh = dir.join(format!("phrase-bbbb.{CACHE_EXT}.999-1.tmp"));
        let foreign = dir.join("other.tmp");
        set_age(&old, 2 * 3600);
        set_age(&fresh, 10);
        set_age(&foreign, 2 * 3600);
        let _ = lazy_texts(
            &LazyPhraseIndex::with_cache_dir(Some(dir.clone())),
            &two_layer_dm(),
            "ip",
        );
        assert!(!old.exists(), "超过 1 小时的崩溃残留应被清掉");
        assert!(fresh.exists(), "新鲜的临时文件可能正被别的实例写，不许碰");
        assert!(foreign.exists(), "别人的 .tmp 不许碰");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 扫描期间启用集合「关了又开」（ABA）：扫描前后指纹相同，但扫到的是中间态。
    /// 这份内容不许落盘——否则它躺在以完整集合命名的文件里，下次（以及本次重试）
    /// 都会命中它，被关过一瞬的那本词库的词组从此消失。
    ///
    /// 判据靠失效代号：扫描期间每次启停都伴随 `invalidate`，代号一变就不落盘。
    ///
    /// 反向验证（变异）：删掉 `load_or_build` 里落盘前的代号比对，本条红（重试命中了
    /// 那份残缺的文件，`ip'max` 召回为空）。
    #[test]
    fn a_scan_that_sees_an_off_then_on_toggle_is_not_persisted() {
        use std::sync::atomic::AtomicBool;
        struct Hook {
            inner: DigestLayer,
            run: Box<dyn Fn() + Send + Sync>,
        }
        impl wind_dict::DictLayer for Hook {
            fn name(&self) -> &str {
                self.inner.name()
            }
            fn layer_type(&self) -> wind_dict::LayerType {
                self.inner.layer_type()
            }
            fn search(&self, c: &str, l: usize) -> Vec<Candidate> {
                self.inner.search(c, l)
            }
            fn search_prefix(&self, p: &str, l: usize) -> Vec<Candidate> {
                self.inner.search_prefix(p, l)
            }
            fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
                self.inner.for_each_entry(f);
                (self.run)();
            }
            fn entries_digest(&self) -> Option<String> {
                self.inner.entries_digest()
            }
        }
        let dir = tmp_dir("aba");
        let lazy = Arc::new(LazyPhraseIndex::with_cache_dir(Some(dir.clone())));
        let ext = DigestLayer::new("ext", EXT);
        let flag = Arc::clone(&ext.enabled);
        let fired = Arc::new(AtomicBool::new(false));
        let dm = DictManager::new();
        // 层序：off（main 之后关 ext）→ ext（被跳过）→ on（再开回来）。只在第一趟扫描动手。
        let (l1, f1, fl1) = (Arc::clone(&lazy), Arc::clone(&fired), Arc::clone(&flag));
        dm.register_layer(Box::new(Hook {
            inner: DigestLayer::new("main", MAIN),
            run: Box::new(move || {
                if !f1.load(Relaxed) {
                    fl1.store(false, Relaxed);
                    l1.invalidate();
                }
            }),
        }));
        dm.register_layer(Box::new(ext));
        let (l2, f2, fl2) = (Arc::clone(&lazy), Arc::clone(&fired), Arc::clone(&flag));
        dm.register_layer(Box::new(Hook {
            inner: DigestLayer::new("zz", &[]),
            run: Box::new(move || {
                if !f2.swap(true, Relaxed) {
                    fl2.store(true, Relaxed);
                    l2.invalidate();
                }
            }),
        }));
        assert_eq!(
            lazy_texts(&lazy, &dm, "ip'max"),
            vec!["iPhone 15 Pro Max"],
            "重试读回了扫描中间态落的盘"
        );
        let again = LazyPhraseIndex::with_cache_dir(Some(dir.clone()));
        assert_eq!(lazy_texts(&again, &dm, "ip'max"), vec!["iPhone 15 Pro Max"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 单飞：预热线程与按键线程同时 `get`，全表扫只发生一次。
    ///
    /// 两边各扫一遍不只是白费几百毫秒：两份同指纹的落盘在 Windows 上会让后到的 rename
    /// 撞上先到者刚映射的文件，打一条误导性的 warn。
    ///
    /// 时序：第一趟扫描停在层里，放第二条线程进 `get`、等 200 ms 再放行。没有单飞锁时
    /// 第二条线程这期间必然自己开扫（索引还没写回）。
    ///
    /// 反向验证（变异）：去掉 `get` 里的单飞锁，本条红在扫描次数 2 上。
    #[test]
    fn concurrent_gets_scan_only_once() {
        use std::sync::mpsc;
        use std::sync::{Mutex, atomic::AtomicU32};
        struct Gate {
            calls: AtomicU32,
            entered: Mutex<Option<mpsc::Sender<()>>>,
            resume: Mutex<mpsc::Receiver<()>>,
        }
        struct Slow(Arc<Gate>);
        impl wind_dict::DictLayer for Slow {
            fn name(&self) -> &str {
                "slow"
            }
            fn layer_type(&self) -> wind_dict::LayerType {
                wind_dict::LayerType::System
            }
            fn search(&self, _c: &str, _l: usize) -> Vec<Candidate> {
                Vec::new()
            }
            fn search_prefix(&self, _p: &str, _l: usize) -> Vec<Candidate> {
                Vec::new()
            }
            fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
                f("aaa", "aaa bbb", 1);
                if self.0.calls.fetch_add(1, Relaxed) == 0 {
                    if let Some(tx) = self.0.entered.lock().unwrap().take() {
                        tx.send(()).unwrap();
                    }
                    self.0.resume.lock().unwrap().recv().unwrap();
                }
            }
            fn entries_digest(&self) -> Option<String> {
                Some("slow|v1".into())
            }
        }
        let (etx, erx) = mpsc::channel();
        let (rtx, rrx) = mpsc::channel();
        let gate = Arc::new(Gate {
            calls: AtomicU32::new(0),
            entered: Mutex::new(Some(etx)),
            resume: Mutex::new(rrx),
        });
        let dm = Arc::new(DictManager::new());
        dm.register_layer(Box::new(Slow(Arc::clone(&gate))));
        let dir = tmp_dir("single");
        let lazy = Arc::new(LazyPhraseIndex::with_cache_dir(Some(dir.clone())));
        let spawn = || {
            let (lazy, dm) = (Arc::clone(&lazy), Arc::clone(&dm));
            std::thread::spawn(move || lazy.get(&dm).len())
        };
        let a = spawn();
        erx.recv().unwrap();
        let b = spawn();
        std::thread::sleep(std::time::Duration::from_millis(200));
        rtx.send(()).unwrap();
        assert_eq!((a.join().unwrap(), b.join().unwrap()), (1, 1));
        assert_eq!(gate.calls.load(Relaxed), 1, "两条线程各扫了一遍全表");
        assert_eq!(lazy.full_scans(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
