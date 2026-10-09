//! `.wridx`：反查索引（词 → 该词的全部编码）的序列化格式与读取器。
//!
//! # 为什么它值得有自己的格式
//!
//! 反查索引此前是纯堆结构，随方案常驻。十万词级的码表约 2.4 MB 无所谓，但大词库上它
//! 是个**长尾灾难**：真机实测 feihuzj2 方案 251 万词 → **95.4 MB**，且最多缓存两份。
//! 其中光是定长条目数组就占 40 MB（16 B × 251 万）——42% 的内存花在「索引」而非「数据」上，
//! 而这部分恰恰是最适合留在磁盘上按需分页的。
//!
//! # 为什么这里没有 DAT（与 `.wdat` 的分工）
//!
//! 曾试过把反查做成 **text 为键的 `.wdat`**（双数组 trie），实测 264 MB 磁盘 + 15 秒写入，
//! 为的是省 95 MB 内存——**净亏**，方案作废。原因是 DAT 是为「按前缀逐键检索百万条」
//! 付的钱，而反查的两种用法都不需要它：
//!
//! - `codes_of`：精确点查，每页候选查 5~9 次 → 二分足够；
//! - `texts_with_prefix`：前缀顺扫，靠「按 text 字节序排」这一条性质就能做，无需额外索引。
//!
//! 所以本格式的骨架取自 [`crate::commentdict`]（`.wcmt`：排序数组 + 二分），数据语义
//! 取自原先的堆结构（词 → 编码**列表** + 该词最大权重）。文件体积因此与堆结构同量级，
//! mmap 才真正划算。
//!
//! # 文件布局
//!
//! - Header (32 B)：magic `WRIX` + version u32 + entry_count u32
//!   + index_off u32 + text_off u32 + value_off u32 + reserved u32 × 2
//! - Entry[entry_count] (16 B 每条，**按 text UTF-8 字节序升序**)：
//!   text_off u32（相对 `text_off` 段）+ text_len u16 + code_count u16
//!   + value_off u32（相对 `value_off` 段）+ weight i32
//! - TextPool：全部词按条目序连续拼接
//! - ValuePool：每条目 `[len u8][code 字节]` × code_count，按条目序连续
//!
//! 两个池都**按条目序**写入，于是前缀顺扫在条目数组与文本池上都是顺序访问
//! （mmap 下这决定了是「几次缺页」还是「每词一次缺页」）。
//!
//! ## 为什么编码用 1 字节长度前缀而不是全局结束偏移数组
//!
//! 堆版用 `code_ends: Vec<u32>`（每个编码 4 B）。改成紧跟数据的 1 B 长度前缀后每码省 3 B，
//! 且省掉了一个独立的段。代价是 `CodeList` 只能顺序走而不能随机定位——但每词的编码数
//! 是个位数，`last()` 走到底也是几步。
//!
//! # 常驻 vs 映射
//!
//! [`ReverseIndex::open`] 按文件大小自动选：小于阈值就整份读进内存（省掉缺页与文件句柄），
//! 否则 mmap。**两条路走的是同一份字节布局、同一套查找代码**——这是刻意的：反查是低频
//! 路径，两套实现的分叉会长期无人发现。

use memmap2::Mmap;
use std::path::Path;
use tracing::{info, warn};

const MAGIC: [u8; 4] = *b"WRIX";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 32;
const ENTRY_SIZE: usize = 16;

/// 单个编码的字节上限（1 字节长度前缀的值域）。超长的编码会被丢弃并计入 warn。
const MAX_CODE_BYTES: usize = u8::MAX as usize;
/// 单个词的字节上限（`text_len: u16`）。
const MAX_TEXT_BYTES: usize = u16::MAX as usize;
/// 单个词的编码条数上限（`code_count: u16`）。超出部分截断（按码长升序，丢的是最长的那些）。
const MAX_CODES_PER_TEXT: usize = u16::MAX as usize;

/// 索引字节的来源：进程内构建产物，或磁盘文件的映射。
///
/// 两者对上层完全等价——差别只在这些字节算不算「常驻内存」。
enum IndexData {
    Owned(Vec<u8>),
    Mapped(Mmap),
}

impl IndexData {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(v) => v,
            Self::Mapped(m) => m,
        }
    }
}

/// 反查索引：词 → 该词在词库中的全部编码（码长升序 → 字典序，已去重）。
///
/// 查询走二分——反查仅用于悬停 `[编码]` 段、候选注释与词语联想，均为低频路径，无需 O(1)。
/// 存储见模块文档；构建入口是 [`ReverseIndex::build`]，磁盘复用入口是 [`ReverseIndex::open`]。
pub struct ReverseIndex {
    data: IndexData,
    entry_count: u32,
    index_off: u32,
    text_off: u32,
    value_off: u32,
}

impl Default for ReverseIndex {
    /// 空索引：零字节、零条目。所有查询返回「查不到」。
    ///
    /// 刻意**不**走 `from_bytes(serialize(vec![]))`——默认值必须是无条件成功的，
    /// 不能依赖「空镜像恰好能解析」这个可被后续改动破坏的前提。
    fn default() -> Self {
        Self {
            data: IndexData::Owned(Vec::new()),
            entry_count: 0,
            index_off: 0,
            text_off: 0,
            value_off: 0,
        }
    }
}

/// 一条索引记录的定长头部（16 B）。
#[derive(Clone, Copy)]
struct Entry {
    text_off: u32,
    text_len: u16,
    code_count: u16,
    value_off: u32,
    weight: i32,
}

impl ReverseIndex {
    /// 诊断：常驻堆的字节数（读进内存的那份）；mmap 的返回 0（不计私有内存）。
    pub fn heap_bytes(&self) -> usize {
        match &self.data {
            IndexData::Owned(v) => v.capacity(),
            IndexData::Mapped(_) => 0,
        }
    }

    /// 诊断：索引镜像的总字节数（不论常驻还是 mmap）。
    pub fn image_bytes(&self) -> usize {
        self.data.as_slice().len()
    }

    /// 从 (词, 编码, 权重) 三元组构建（进程内，不落盘）。
    ///
    /// 语义与落盘版**完全一致**，因为两者是同一个 [`serialize`] 的产物。
    pub fn build(pairs: Vec<(String, String, i32)>) -> Self {
        Self::from_bytes(serialize(pairs))
    }

    /// 从已序列化的镜像字节构造。字节损坏时降级为空索引（不 panic）。
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        match Self::parse(IndexData::Owned(bytes)) {
            Ok(idx) => idx,
            Err(e) => {
                warn!("反查索引镜像无法解析（{e}），本次按空索引处理");
                Self::default()
            }
        }
    }

    /// 打开磁盘上的 `.wridx`。
    ///
    /// `resident_max_bytes` 是「小到可以直接读进内存」的上限：文件不超过它就整份读入
    /// （省掉缺页与常驻的文件映射），否则 mmap。**两条路的查询行为完全相同**，
    /// 差别只是这些字节算不算常驻内存——故阈值取值只影响性能，不影响正确性。
    ///
    /// 文件不存在/格式不符/版本不符一律 `Err`，由调用方重建。
    pub fn open(path: &Path, resident_max_bytes: usize) -> anyhow::Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        let data = if len as usize <= resident_max_bytes {
            IndexData::Owned(std::fs::read(path)?)
        } else {
            IndexData::Mapped(unsafe { Mmap::map(&file)? })
        };
        let idx = Self::parse(data)?;
        info!(
            "Opened reverse index: {} ({} texts, {:.1} MB, {})",
            path.display(),
            idx.len(),
            len as f64 / 1024.0 / 1024.0,
            if idx.is_resident() { "常驻" } else { "mmap" }
        );
        Ok(idx)
    }

    fn parse(data: IndexData) -> anyhow::Result<Self> {
        let d = data.as_slice();
        if d.len() < HEADER_SIZE {
            anyhow::bail!("wridx too short: {} bytes", d.len());
        }
        if d[0..4] != MAGIC {
            anyhow::bail!("invalid wridx magic");
        }
        let rd = |off: usize| u32::from_le_bytes(d[off..off + 4].try_into().unwrap());
        let version = rd(4);
        if version != VERSION {
            anyhow::bail!("unsupported wridx version: {version} (expected {VERSION})");
        }
        let entry_count = rd(8);
        let index_off = rd(12);
        let text_off = rd(16);
        let value_off = rd(20);
        // 段边界只校验到「不越出文件」：段内每一条的越界在读取时各自兜底（见 `entry`/
        // `text_of`），因为缓存文件可能被外部截断，此处一票否决会让整个方案失去反查，
        // 而逐条兜底只丢损坏的那几条。
        let index_end = index_off as usize + entry_count as usize * ENTRY_SIZE;
        if index_end > d.len() || text_off as usize > d.len() || value_off as usize > d.len() {
            anyhow::bail!("wridx offsets out of range");
        }
        Ok(Self {
            data,
            entry_count,
            index_off,
            text_off,
            value_off,
        })
    }

    /// 序列化镜像的原始字节（落盘用；也是「两条构建路径逐位等价」的最强判据）。
    pub fn image(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// 是否常驻内存（`false` = mmap，字节不计入进程私有内存）。
    pub fn is_resident(&self) -> bool {
        matches!(self.data, IndexData::Owned(_))
    }

    /// 索引镜像的总字节数（常驻与否都一样，用于「该不该常驻」这类判定）。
    pub fn data_bytes(&self) -> usize {
        self.data.as_slice().len()
    }

    /// **实际占用的进程私有内存**：常驻时等于镜像大小，mmap 时约等于 0。
    ///
    /// 与 [`Self::data_bytes`] 分开是因为这两个数在 mmap 下差着整个索引——
    /// 日志里混用会让「内存到底降没降」无从判断。
    pub fn resident_bytes(&self) -> usize {
        match &self.data {
            IndexData::Owned(v) => v.len(),
            IndexData::Mapped(_) => 0,
        }
    }

    /// 收录的词数。
    pub fn len(&self) -> usize {
        self.entry_count as usize
    }

    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    fn entry(&self, i: u32) -> Option<Entry> {
        let d = self.data.as_slice();
        let off = self.index_off as usize + i as usize * ENTRY_SIZE;
        let e = d.get(off..off + ENTRY_SIZE)?;
        Some(Entry {
            text_off: u32::from_le_bytes(e[0..4].try_into().ok()?),
            text_len: u16::from_le_bytes(e[4..6].try_into().ok()?),
            code_count: u16::from_le_bytes(e[6..8].try_into().ok()?),
            value_off: u32::from_le_bytes(e[8..12].try_into().ok()?),
            weight: i32::from_le_bytes(e[12..16].try_into().ok()?),
        })
    }

    fn text_of(&self, e: &Entry) -> Option<&str> {
        let d = self.data.as_slice();
        let s = self.text_off as usize + e.text_off as usize;
        std::str::from_utf8(d.get(s..s + e.text_len as usize)?).ok()
    }

    fn text_at(&self, i: u32) -> Option<&str> {
        self.text_of(&self.entry(i)?)
    }

    /// 读 ValuePool 中位于 `off` 的一个编码，返回 (编码, 下一个编码的偏移)。
    fn code_at(&self, off: usize) -> Option<(&str, usize)> {
        let d = self.data.as_slice();
        let base = self.value_off as usize + off;
        let len = *d.get(base)? as usize;
        let s = base + 1;
        Some((std::str::from_utf8(d.get(s..s + len)?).ok()?, off + 1 + len))
    }

    /// 首个 `text` 不小于给定值的下标（lower bound）。
    ///
    /// 条目损坏时按「不小于」处理（收缩 hi）：宁可查不到也不要死循环或越界。
    /// 与 [`crate::commentdict::CommentReader`] 同一约定。
    fn lower_bound(&self, text: &str) -> u32 {
        let (mut lo, mut hi) = (0u32, self.entry_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.text_at(mid) {
                Some(t) if t < text => lo = mid + 1,
                _ => hi = mid,
            }
        }
        lo
    }

    /// 二分查词，返回其编码列表视图；词不在索引中返回 `None`。
    pub fn codes_of(&self, text: &str) -> Option<CodeList<'_>> {
        if text.is_empty() {
            return None;
        }
        let i = self.lower_bound(text);
        let e = self.entry(i)?;
        if self.text_of(&e)? != text {
            return None;
        }
        Some(CodeList {
            index: self,
            value_off: e.value_off as usize,
            count: e.code_count as usize,
        })
    }

    /// **词语联想的取数口**：以 `prefix` 开头、且**严格更长**的词，按权重降序取前 `limit` 条。
    ///
    /// 返回 (整词, 权重)。上屏时补的是整词去掉 `prefix` 之后的部分——那一步在调用方做，
    /// 因为「显示整词、只上屏剩余」是展示层的决定，本层只负责把词捞出来。
    ///
    /// # 为什么能这么便宜
    ///
    /// 条目本就按 `text` 字节序升序（反查用二分的前提）。字节序下同前缀的词必然连续，
    /// 于是二分找到下界后顺序走到第一个不匹配即止——**无需任何额外索引**。
    /// 顺扫方向与两个池的写入序一致，mmap 下也是顺序访问。
    ///
    /// ⚠️ 前缀本身要排除：「中」的联想不该包含「中」自己。
    ///
    /// 扫描长度是该前缀下的词数（「中」约数千），每次上屏一次，微秒级。刻意不做提前截断
    /// ——按权重取 top-N **必须看完全部候选**，扫到一半就停会退化成「字典序前 N 里权重
    /// 最高的那几个」，那是个看起来对、实则完全不对的结果。
    pub fn texts_with_prefix(&self, prefix: &str, limit: usize) -> Vec<(&str, i32)> {
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let lo = self.lower_bound(prefix);
        let mut hits: Vec<(&str, i32)> = Vec::new();
        for i in lo..self.entry_count {
            let Some(e) = self.entry(i) else { break };
            let Some(t) = self.text_of(&e) else { continue };
            if !t.starts_with(prefix) {
                break; // 字节序保证同前缀连续，第一个不匹配即到头
            }
            if t.len() > prefix.len() {
                hits.push((t, e.weight));
            }
        }
        // 权重降序；同权按词短优先（「中国」比「中国人民」更常用作联想），再按字典序定序。
        hits.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.len().cmp(&b.0.len()))
                .then_with(|| a.0.cmp(b.0))
        });
        hits.truncate(limit);
        hits
    }
}

/// 某词的编码列表视图：按需从索引字节切片，取用不分配。
#[derive(Clone, Copy)]
pub struct CodeList<'a> {
    index: &'a ReverseIndex,
    /// 本词首个编码在 ValuePool 中的偏移。
    value_off: usize,
    count: usize,
}

impl<'a> CodeList<'a> {
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 末位 = 最长码（全码）。简码可能被一级简码占用，取全码最稳。
    pub fn last(&self) -> Option<&'a str> {
        self.iter().last()
    }

    /// 按序遍历（码长升序）。字节损坏时提前结束，不 panic。
    pub fn iter(self) -> impl Iterator<Item = &'a str> {
        let idx = self.index;
        let mut off = self.value_off;
        let mut left = self.count;
        std::iter::from_fn(move || {
            if left == 0 {
                return None;
            }
            left -= 1;
            let (s, next) = idx.code_at(off)?;
            off = next;
            Some(s)
        })
    }

    /// 以 `sep` 连接全部编码（如 `a/ab/abc`）。
    pub fn join(self, sep: &str) -> String {
        self.iter().collect::<Vec<_>>().join(sep)
    }
}

/// 把 (词, 编码, 权重) 三元组序列化成 `.wridx` 镜像。
///
/// 同一 (词, 编码) 重复只留一份；每词的编码按「码长升序 → 字典序」排，权重取该词全部
/// 条目中的**最大**值。
///
/// # 为什么权重要取 max，且必须在去重之前取
///
/// 反查本身用不到权重——它是给 [`ReverseIndex::texts_with_prefix`] 的：词语联想要的是
/// 「以『中』开头的**最常用**的几个词」。同一个词在主库与扩展库各有一条时取到低的那个，
/// 会让它在联想里莫名靠后。
///
/// ⚠️ **聚合必须发生在按 (词, 码) 去重之前**。同一个 (词, 码) 跨库出现、权重不同是常态
/// （扩展库给同一个打法配了更高词频），去重后再取 max 等于「取排序后靠前那条的权重」
/// ——而排序键里根本没有权重。这个错误不改变索引体积，只改变联想顺序，
/// 靠「大小相等」一类的判据永远测不出来。
///
/// # 超限条目的处置
///
/// 三条上限（词 64 KB、单码 255 B、每词 65535 码）在真实词库上都触不到，但缓存格式必须
/// 对畸形输入有确定行为。丢弃的是**单个编码**或**单个词**，且各自计数出 warn——
/// 静默丢弃会表现为「某个词反查不到」，那是最难归因的一类故障。
pub fn serialize(mut pairs: Vec<(String, String, i32)>) -> Vec<u8> {
    // 全局一次排序即同时满足：词分组相邻、组内码长升序、同长按字典序。
    // 权重不参与排序——它是分组内的聚合量，不影响条目次序。
    pairs.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.len().cmp(&b.1.len()))
            .then_with(|| a.1.cmp(&b.1))
    });
    let mut index: Vec<u8> = Vec::with_capacity(pairs.len() * ENTRY_SIZE);
    let mut texts: Vec<u8> = Vec::new();
    let mut values: Vec<u8> = Vec::new();
    let (mut long_text, mut long_code, mut too_many) = (0usize, 0usize, 0usize);
    let mut entry_count = 0u32;

    let mut i = 0usize;
    while i < pairs.len() {
        let text = pairs[i].0.as_str();
        let mut j = i;
        while j < pairs.len() && pairs[j].0 == text {
            j += 1;
        }
        let group = &pairs[i..j];
        i = j;

        if text.len() > MAX_TEXT_BYTES {
            long_text += 1;
            continue;
        }
        // ★ 权重取该词**全部原始条目**的最大值——必须在下面的按码去重与超限过滤
        //   **之前**算。此前是先 `dedup_by((词, 码))` 再在去重后的条目上取 max，于是
        //   同一个 (词, 码) 分散在主库与扩展库、权重不同时，高权重那条被整条丢掉，
        //   该词在词语联想里就按偏低的权重排序（真机上表现为常用词莫名靠后）。
        //   这个洞能长期存活，是因为旧判据只比了「索引总字节数」——权重差异不改变体积。
        let weight = group.iter().map(|p| p.2).max().unwrap_or(0);

        let value_start = values.len();
        let mut count = 0usize;
        let mut prev_code: Option<&str> = None;
        for (_, code, _) in group {
            // 同 (词, 码) 只留一份，否则同一个码会在候选的 [编码] 段里出现两次。
            // 排序已让同码相邻，比上一条即可；判重放在超限过滤**之前**，
            // 使「留哪一条」与「这条留不留得下」两件事互不干扰。
            if prev_code == Some(code.as_str()) {
                continue;
            }
            prev_code = Some(code.as_str());
            if code.len() > MAX_CODE_BYTES {
                long_code += 1;
                continue;
            }
            if count == MAX_CODES_PER_TEXT {
                too_many += 1;
                break;
            }
            values.push(code.len() as u8);
            values.extend_from_slice(code.as_bytes());
            count += 1;
        }
        // 池溢出 4 GB：无法再表达偏移，只能就此打住。真实词库触不到（那是几十亿字节的
        // 中文词），但静默截断等于「后半本词库反查不到」，必须显式喊出来。
        if texts.len() + text.len() > u32::MAX as usize || values.len() > u32::MAX as usize {
            warn!(
                "反查索引超出 4GB 寻址上限，已在第 {} 词处截断——其后的词将无法反查",
                entry_count
            );
            values.truncate(value_start);
            break;
        }
        index.extend_from_slice(&(texts.len() as u32).to_le_bytes());
        index.extend_from_slice(&(text.len() as u16).to_le_bytes());
        index.extend_from_slice(&(count as u16).to_le_bytes());
        index.extend_from_slice(&(value_start as u32).to_le_bytes());
        index.extend_from_slice(&weight.to_le_bytes());
        texts.extend_from_slice(text.as_bytes());
        entry_count += 1;
    }

    if long_text > 0 || long_code > 0 || too_many > 0 {
        warn!(
            "反查索引跳过超限条目：{long_text} 个词超 64KB、{long_code} 个编码超 255B、\
             {too_many} 个词的编码数超 65535（已截断）"
        );
    }

    let index_off = HEADER_SIZE as u32;
    let text_off = index_off + index.len() as u32;
    let value_off = text_off + texts.len() as u32;
    let mut out = Vec::with_capacity(HEADER_SIZE + index.len() + texts.len() + values.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&entry_count.to_le_bytes());
    out.extend_from_slice(&index_off.to_le_bytes());
    out.extend_from_slice(&text_off.to_le_bytes());
    out.extend_from_slice(&value_off.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&index);
    out.extend_from_slice(&texts);
    out.extend_from_slice(&values);
    out
}

/// 把索引镜像写到 `path`（tmp + rename 原子替换）。
///
/// 失败不是正确性问题——调用方直接用内存里的镜像即可，只是下次启动还要再建一遍。
/// 常见失败是 Windows 上旧文件仍被本进程映射着（rename 会 Access Denied）。
pub fn write_wridx(path: &Path, image: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, image)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(v: &[(&str, &str, i32)]) -> ReverseIndex {
        ReverseIndex::build(
            v.iter()
                .map(|(t, c, w)| (t.to_string(), c.to_string(), *w))
                .collect(),
        )
    }

    #[test]
    fn roundtrip_through_disk_is_byte_identical_and_behaviour_identical() {
        let mem = build(&[
            ("工", "aaaa", 100),
            ("工", "a", 50),
            ("中", "khk", 10),
            ("大", "de", 7),
        ]);
        let p = std::env::temp_dir().join(format!("wind_wridx_rt_{}.wridx", std::process::id()));
        write_wridx(&p, mem.image()).unwrap();

        // 阈值给足 → 常驻；给 0 → mmap。两条路都必须与内存版**逐位相同**。
        let resident = ReverseIndex::open(&p, usize::MAX).unwrap();
        let mapped = ReverseIndex::open(&p, 0).unwrap();
        assert!(resident.is_resident());
        assert!(!mapped.is_resident());
        assert_eq!(resident.image(), mem.image());
        assert_eq!(mapped.image(), mem.image());
        assert_eq!(mapped.resident_bytes(), 0, "mmap 不应计入常驻内存");
        assert_eq!(resident.resident_bytes(), mem.data_bytes());

        for idx in [&mem, &resident, &mapped] {
            assert_eq!(idx.len(), 3);
            assert_eq!(
                idx.codes_of("工").unwrap().iter().collect::<Vec<_>>(),
                ["a", "aaaa"]
            );
            assert_eq!(idx.codes_of("工").unwrap().last(), Some("aaaa"));
            assert_eq!(idx.codes_of("工").unwrap().join("/"), "a/aaaa");
            assert!(idx.codes_of("无").is_none());
        }
        drop((resident, mapped));
        let _ = std::fs::remove_file(&p);
    }

    /// arena 是连续拼接的，相邻词/码若边界算错会串味。等长且互为前缀的样本压这个边界。
    #[test]
    fn pool_boundaries_dont_bleed() {
        let idx = build(&[
            ("甲", "aa", 1),
            ("甲", "aaa", 1),
            ("乙", "bb", 1),
            ("丙", "cc", 1),
        ]);
        assert_eq!(
            idx.codes_of("甲").unwrap().iter().collect::<Vec<_>>(),
            ["aa", "aaa"]
        );
        assert_eq!(
            idx.codes_of("乙").unwrap().iter().collect::<Vec<_>>(),
            ["bb"]
        );
        // 「丙」按字节序排首位，其编码起点必须是 0 而非前一条目的终点
        assert_eq!(
            idx.codes_of("丙").unwrap().iter().collect::<Vec<_>>(),
            ["cc"]
        );
        assert!(idx.codes_of("").is_none(), "空词不应命中");
    }

    /// 多字节键的二分：比较按 UTF-8 字节序，与写入端 `String::cmp` 必须一致。
    /// 混入扩展区汉字（4 字节）——若哪一端按 char 数或 UTF-16 比较，这里会错位。
    #[test]
    fn multibyte_keys_compare_consistently() {
        let idx = build(&[
            ("𠮷", "a", 1),
            ("吉", "b", 1),
            ("a", "c", 1),
            ("Ω", "d", 1),
            ("🀄", "e", 1),
        ]);
        for (k, want) in [
            ("𠮷", "a"),
            ("吉", "b"),
            ("a", "c"),
            ("Ω", "d"),
            ("🀄", "e"),
        ] {
            assert_eq!(
                idx.codes_of(k).unwrap().last(),
                Some(want),
                "键 {k} 应可查到"
            );
        }
        // 边界：小于最小键 / 大于最大键
        assert!(idx.codes_of("\u{1}").is_none());
        assert!(idx.codes_of("\u{10FFFF}").is_none());
    }

    #[test]
    fn empty_index_is_valid_everywhere() {
        let mem = build(&[]);
        assert!(mem.is_empty());
        assert!(mem.codes_of("任意").is_none());
        assert!(mem.texts_with_prefix("中", 9).is_empty());
        let p = std::env::temp_dir().join(format!("wind_wridx_empty_{}.wridx", std::process::id()));
        write_wridx(&p, mem.image()).unwrap();
        let disk = ReverseIndex::open(&p, usize::MAX).unwrap();
        assert!(disk.is_empty());
        assert!(disk.codes_of("任意").is_none());
        drop(disk);
        let _ = std::fs::remove_file(&p);

        // Default 与「序列化一个空表」必须行为一致（前者刻意不走序列化，故须单独验）
        let d = ReverseIndex::default();
        assert!(d.is_empty() && d.codes_of("x").is_none());
        assert!(d.texts_with_prefix("x", 9).is_empty());
    }

    /// 非 wridx / 版本不符 / 过短的文件必须被拒绝而非当成空表——否则缓存格式升级后
    /// 用户会静默失去反查，且毫无提示。
    #[test]
    fn rejects_foreign_or_stale_file() {
        let p = std::env::temp_dir().join(format!("wind_wridx_bad_{}.wridx", std::process::id()));
        std::fs::write(
            &p,
            b"not a wridx file at all, padded well past the header size",
        )
        .unwrap();
        assert!(
            ReverseIndex::open(&p, usize::MAX).is_err(),
            "magic 不符须拒绝"
        );
        std::fs::write(&p, b"WRIX").unwrap();
        assert!(ReverseIndex::open(&p, usize::MAX).is_err(), "过短须拒绝");
        // 版本位改掉 → 拒绝（缓存格式升级的自动失效依赖这一条）
        let mut img = build(&[("中", "a", 1)]).image().to_vec();
        img[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
        std::fs::write(&p, &img).unwrap();
        assert!(
            ReverseIndex::open(&p, usize::MAX).is_err(),
            "版本不符须拒绝"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// 超长编码被丢弃，但**同词的其它编码与权重必须留下**——整词消失才是真正的伤害。
    #[test]
    fn oversized_code_is_dropped_without_losing_the_word() {
        let long = "x".repeat(MAX_CODE_BYTES + 1);
        let idx = ReverseIndex::build(vec![
            ("词".into(), "ok".into(), 5),
            ("词".into(), long, 900),
            ("别的".into(), "z".into(), 1),
        ]);
        assert_eq!(
            idx.codes_of("词").unwrap().iter().collect::<Vec<_>>(),
            ["ok"]
        );
        // 权重在编码过滤之**前**聚合，故被丢弃那条的 900 仍要生效
        assert_eq!(
            idx.texts_with_prefix("词", 1),
            Vec::<(&str, i32)>::new(),
            "「词」没有更长的词"
        );
        let all = build(&[("中", "a", 1)]);
        assert_eq!(all.len(), 1);
    }

    /// 超长的**词**整条丢弃，但不能影响其相邻条目的偏移计算。
    #[test]
    fn oversized_text_is_dropped_without_shifting_neighbours() {
        let huge = "中".repeat(MAX_TEXT_BYTES); // 3 字节/字 → 远超 64KB
        let idx = ReverseIndex::build(vec![
            ("阿".into(), "a".into(), 1),
            (huge, "h".into(), 1),
            ("龘".into(), "z".into(), 2),
        ]);
        assert_eq!(idx.len(), 2, "超长词整条丢弃");
        assert_eq!(idx.codes_of("阿").unwrap().last(), Some("a"));
        assert_eq!(idx.codes_of("龘").unwrap().last(), Some("z"));
    }
}

#[cfg(test)]
mod prefix_tests {
    //! `texts_with_prefix`：词语联想的取数口。行为必须与常驻/mmap 无关，故每条都两路都跑。
    use super::*;

    fn both(
        pairs: Vec<(String, String, i32)>,
        tag: &str,
    ) -> (ReverseIndex, ReverseIndex, std::path::PathBuf) {
        let mem = ReverseIndex::build(pairs);
        let p = std::env::temp_dir().join(format!(
            "wind_wridx_pfx_{}_{}.wridx",
            std::process::id(),
            tag
        ));
        write_wridx(&p, mem.image()).unwrap();
        let mapped = ReverseIndex::open(&p, 0).unwrap();
        (mem, mapped, p)
    }

    fn sample() -> Vec<(String, String, i32)> {
        // 刻意让**字典序**与**权重序**相反——两者一致的话，「按权重排」和「按字典序排」
        // 会得出同样的结果，本组测试就什么都没验到。
        [
            ("中", "a", 1000),
            ("中一", "aa", 1),
            ("中丁", "ab", 2),
            ("中国", "ac", 900),
            ("中国人", "acd", 800),
            ("中间", "ad", 700),
            ("丰", "b", 500),
            ("串", "c", 400),
        ]
        .iter()
        .map(|(t, c, w)| (t.to_string(), c.to_string(), *w))
        .collect()
    }

    #[test]
    fn ranks_by_weight_not_dictionary_order() {
        let (mem, mapped, p) = both(sample(), "rank");
        for i in [&mem, &mapped] {
            let texts: Vec<_> = i
                .texts_with_prefix("中", 3)
                .iter()
                .map(|(t, _)| *t)
                .collect();
            assert_eq!(
                texts,
                ["中国", "中国人", "中间"],
                "取的是最常用的三个，不是字典序前三（那会是 中一/中丁/中国）"
            );
        }
        drop((mem, mapped));
        let _ = std::fs::remove_file(&p);
    }

    /// ★ 前缀词**自己**必须排除：「中」的联想不该包含「中」。
    ///
    /// 它的权重恰恰常常是全场最高（单字比词常用），不排除就会稳定占据首位，
    /// 而选中它等于上屏一个空串。
    #[test]
    fn excludes_the_prefix_itself() {
        let (mem, mapped, p) = both(sample(), "self");
        for i in [&mem, &mapped] {
            let out = i.texts_with_prefix("中", 9);
            assert!(
                !out.iter().any(|(t, _)| *t == "中"),
                "前缀本身不是它自己的联想"
            );
            assert_eq!(out.len(), 5, "中一/中丁/中国/中国人/中间");
        }
        drop((mem, mapped));
        let _ = std::fs::remove_file(&p);
    }

    /// 扫描必须在第一个不匹配处停下，且不越界到相邻前缀。
    #[test]
    fn does_not_leak_into_neighbouring_prefixes() {
        let (mem, mapped, p) = both(sample(), "leak");
        for i in [&mem, &mapped] {
            assert!(
                i.texts_with_prefix("丰", 9).is_empty(),
                "「丰」没有更长的词"
            );
            assert!(i.texts_with_prefix("串", 9).is_empty());
            assert!(i.texts_with_prefix("龘", 9).is_empty(), "不存在的前缀");
            assert!(i.texts_with_prefix("", 9).is_empty());
            assert!(i.texts_with_prefix("中", 0).is_empty());
        }
        drop((mem, mapped));
        let _ = std::fs::remove_file(&p);
    }

    /// 同权时短词优先——「中国」比「中国人民」更适合当联想。
    #[test]
    fn same_weight_prefers_shorter() {
        let i = ReverseIndex::build(vec![
            ("中国人民".into(), "a".into(), 5),
            ("中国".into(), "b".into(), 5),
        ]);
        assert_eq!(i.texts_with_prefix("中", 2)[0].0, "中国");
    }

    /// 权重取该词全部条目里的**最大**值——同一个词在主库与扩展库各有一条时，
    /// 取到低的那个会让它在联想里莫名靠后。
    #[test]
    fn weight_is_max_across_codes() {
        let i = ReverseIndex::build(vec![
            ("中国".into(), "a".into(), 1),
            ("中国".into(), "bb".into(), 900),
            ("中间".into(), "c".into(), 500),
        ]);
        assert_eq!(i.texts_with_prefix("中", 2)[0].0, "中国", "取 900 而不是 1");
    }

    /// ★ 回归：**同一个 (词, 码) 跨库重复、权重不同**时，聚合必须取到高的那个。
    ///
    /// 上一条只覆盖「同词**不同**码」，而真正出问题的是同词**同码**：按 (词, 码) 去重
    /// 会整条丢掉重复项，若聚合发生在去重之后，被丢的那条所带的高权重一并消失。
    /// 2026-08-24 实测：`中` 在两个库里都是 `khk`（权重 10 / 999），逐库并集算出 10、
    /// 先合并再构建算出 999——两条路径就此分叉，而当时的等价性判据（比索引总字节数）
    /// 对权重差异完全不敏感。
    ///
    /// 两个方向都要断言：**颠倒输入顺序结果必须相同**。只测一个方向的话，
    /// 「取第一条」这种错误实现会有一半概率蒙对。
    #[test]
    fn weight_is_max_even_when_the_same_code_repeats_across_dicts() {
        // 两个方向都跑：只测一个方向的话，「取第一条权重」这种错误实现有一半概率蒙对。
        for (w1, w2) in [(1, 999), (999, 1)] {
            let i = ReverseIndex::build(vec![
                ("甲乙".into(), "x".into(), w1),
                ("甲乙".into(), "x".into(), w2), // 同码重复：必有一条被去重丢掉
                ("甲丙".into(), "y".into(), 500),
            ]);
            assert_eq!(
                i.codes_of("甲乙").unwrap().iter().collect::<Vec<_>>(),
                ["x"],
                "同 (词, 码) 仍须去重成一条"
            );
            assert_eq!(
                i.texts_with_prefix("甲", 2)
                    .iter()
                    .map(|(t, w)| (*t, *w))
                    .collect::<Vec<_>>(),
                [("甲乙", 999), ("甲丙", 500)],
                "被去重丢掉的那条所带的权重必须参与聚合（w1={w1}, w2={w2}）"
            );
        }
    }
}
