//! `.wridx`：反查索引（词 → 该词的全部编码）的序列化格式与读取器。
//!
//! 落盘后按文件大小常驻或 mmap，大词库的反查索引不再占私有内存。不用 text 为键的 `.wdat`：
//! 反查只做精确点查（`codes_of`，二分足够）与前缀顺扫（`texts_with_prefix`，靠按 text 字节序排），
//! 用不上双数组 trie。骨架取自 [`crate::commentdict`]（`.wcmt`：排序数组 + 二分）。
//! 取舍与数据见 `docs/architecture/rime-dict-loading.md` 的反查索引一节。
//!
//! # 文件布局
//!
//! - Header (32 B)：magic `WRIX` + version u32 + entry_count u32
//!   + index_off u32 + text_off u32 + value_off u32 + reserved u32 × 2
//! - Entry[entry_count] (16 B 每条，按 text UTF-8 字节序升序)：
//!   text_off u32（相对 `text_off` 段）+ text_len u16 + code_count u16
//!   + value_off u32（相对 `value_off` 段）+ weight i32
//! - TextPool：全部词按条目序连续拼接
//! - ValuePool：每条目 `[len u8][code 字节]` × code_count，按条目序连续
//!
//! 两个池都按条目序写入，前缀顺扫在 mmap 下是顺序访问（几次缺页而非每词一次）。
//! 编码用 1 B 长度前缀而非偏移数组，每码省 3 B；代价是 `CodeList` 只能顺序走，
//! 但每词编码数是个位数。
//!
//! # 常驻 vs 映射
//!
//! [`ReverseIndex::open`] 按文件大小自动选：小于阈值就整份读进内存（省掉缺页与文件句柄），
//! 否则 mmap。两条路刻意共用同一份字节布局与查找代码：反查是低频路径，分叉会长期无人发现。

use crate::mapping::Mapped;
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
    Mapped(Mapped),
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
            IndexData::Mapped(crate::mapping::map_file(&file)?)
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
    /// 于是二分找到下界后顺序走到第一个不匹配即止，无需额外索引。
    /// 顺扫方向与两个池的写入序一致，mmap 下也是顺序访问。
    ///
    /// 前缀本身要排除：「中」的联想不该包含「中」自己。
    ///
    /// 扫描长度是该前缀下的词数（「中」约数千），微秒级。刻意不提前截断：按权重取 top-N
    /// 必须看完全部候选，否则退化成「字典序前 N 里权重最高的」。
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
/// 条目中的最大值（供 [`ReverseIndex::texts_with_prefix`] 的词语联想按常用度排序）。
/// 取 max 必须在按 (词, 码) 去重之前：同一 (词, 码) 跨库权重不同是常态，先去重会丢掉高权重那条。
///
/// 三条上限（词 64 KB、单码 255 B、每词 65535 码）真实词库触不到，但对畸形输入须有确定行为：
/// 丢弃单个编码或单个词，并各自计数出 warn，免得「某个词反查不到」无从归因。
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
        // 权重取该词全部原始条目的最大值，必须在下面的按码去重与超限过滤之前算（见函数文档）。
        let weight = group.iter().map(|p| p.2).max().unwrap_or(0);

        let value_start = values.len();
        let mut count = 0usize;
        let mut prev_code: Option<&str> = None;
        for (_, code, _) in group {
            // 同 (词, 码) 只留一份，否则同一个码会在候选的 [编码] 段里出现两次。
            // 排序已让同码相邻，比上一条即可；判重放在超限过滤之前，
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
#[path = "reverseidx_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "reverseidx_prefix_tests.rs"]
mod prefix_tests;
