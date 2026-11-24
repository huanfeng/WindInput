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
#[path = "english_phrase_tests.rs"]
mod tests;
