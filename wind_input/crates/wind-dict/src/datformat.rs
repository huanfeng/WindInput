//! DAT 格式 (wdat) — Double-Array Trie 词典
//!
//! 前缀查询为「数组跳转 + 子树 DFS / 分支限界」，适合拼音大词库逐键前缀检索。
//!
//! 文件布局（小端）：
//! ```text
//! [Header 48B]
//! [DAT Base: dat_size*4][DAT Check: dat_size*4][DAT MaxW: dat_size*4]
//! [LeafTable: leaf_count*8]   每条 {entry_off u32, entry_len u16, _ u16}
//! [EntryRecords: entry_count*22]  每条 {text_off u32, text_len u16, weight i32, order u32, boundary u64}
//! [StringPool]
//! [AbbrevSection(可选)]  24B 自描述头 + DAT/MaxW/Leaf/Entry + 简拼 CharMap，共享 StringPool
//! [CharMap 1028B]  {max_code i32, char_map[256] i32}
//! [Meta(可选) 4B len + bytes]
//! ```
//! DAT 查询：`base[s]+c=t`（状态 s 经紧凑码 c 转移到 t），`check[t]==s` 校验；
//! `base[t]<0` 表叶节点，`-base[t]-1` 为 LeafTable 索引；`c=0` 为终止符。
//!
//! MaxW 段（v6）：`maxw[s]` = 以状态 s 为根的子树内所有条目 weight 的最大值，`NO_MAXW`
//! （= i32::MIN）表示子树无条目。它是前缀 Top-K 查询的剪枝上界（见 `search_prefix`）。
//! 段位置紧跟 Check 之后，偏移可直接算出，无需 Header 新字段。

use crate::binformat::DictEntry;
use crate::mapping::Mapped;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::Path;
use tracing::info;

const MAGIC: [u8; 4] = *b"WDAT";
// 版本史（旧缓存一律靠版本/指纹不匹配自动重建，无迁移代码）：
// v3：EntryRecord 加 order u32（全局出现序），等权候选跨编码按出现顺序而非编码字母序。
// v4：EntryRecord 加 boundary u64（音节起始位 bitmask），key 仍为扁平串，查询语义不变。
// v5：AbbrevSection 的条目文本从「词」改为「全拼码」。二进制结构未变但语义变了，旧缓存
//     按新逻辑读会把词当码查主表，故必须 bump。
// v6：新增 MaxW 段，leaf/entry 偏移整体后移。
const VERSION: u32 = 6;
const HEADER_SIZE: usize = 48;
const LEAF_SIZE: usize = 8;
const ENTRY_SIZE: usize = 22;
const CHARMAP_SIZE: usize = 4 + 256 * 4; // 1028

/// `maxw[s]` 的空子树哨兵。刻意不复用 `i32::MIN` 之外的值：weight 为 i32，
/// 任何真实条目的 weight 都 > i32::MIN，故该哨兵不会与真实值混淆。
/// （当前词库最小 weight 为 0，但不依赖该事实。）
const NO_MAXW: i32 = i32::MIN;

/// 原子写临时文件序号（同 binformat，进程内防 tmp 撞名）。
static ATOMIC_WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// ======================= 构建：Double-Array（有序键直接构建，无中间 trie） =======================

/// 构建好的双数组。
struct Dat {
    base: Vec<i32>,
    check: Vec<i32>,
    char_map: [i32; 256],
    max_code: i32,
}

/// 从已按字典序排好、唯一的编码列表直接构建 DAT，不建中间 trie（几百万小节点会把堆撑高、
/// 碎片化）。`data_index` = 编码在列表中的下标（= LeafTable 索引）。
///
/// 原理：一个 trie 节点 ⇔ 一段共享前缀的连续编码区间 `[lo,hi)`（前缀 = codes[lo][..depth]）。
/// BFS 处理 (state, lo, hi, depth)：终止符 = 区间内 len==depth 的那个唯一编码（排序在最前）；
/// 其余编码按第 depth 字节连续分组即各子节点的子区间。
fn build_dat_from_sorted(codes: &[&str]) -> Dat {
    // 1) 字符映射：0 留给终止符，出现过的字节按序得紧凑码 1..=max_code。
    let mut seen = [false; 256];
    for c in codes {
        for &b in c.as_bytes() {
            seen[b as usize] = true;
        }
    }
    let mut char_map = [-1i32; 256];
    char_map[0] = 0;
    let mut max_code = 0i32;
    for b in 1..256 {
        if seen[b] {
            max_code += 1;
            char_map[b] = max_code;
        }
    }

    // 2) base/check 初始化（check=-1 表空闲），root 占位 0；空闲位置(1..)串成链表（见 FreeList）。
    //    按 6× 键数预留：dat_size 实际约 2.3–5.2 × 键数，预留够就不再翻倍扩容（扩容要拷四个
    //    数组，是构建期瞬时峰值的来源）；尾部多余空闲最后裁掉、shrink_to_fit 归还。
    let cap = (codes.len().saturating_mul(6)).max(256);
    let mut base = vec![0i32; cap];
    let mut check = vec![-1i32; cap];
    check[0] = 0;
    let mut free = FreeList::new(cap);
    // 高分支节点的稀疏游标：见 [`find_base_sparse`]。
    let mut sparse_cursor: usize = 1;

    // 3) BFS：队列保层序。
    if !codes.is_empty() {
        let mut queue: std::collections::VecDeque<(i32, usize, usize, usize)> =
            std::collections::VecDeque::new();
        queue.push_back((0i32, 0, codes.len(), 0));
        while let Some((s, lo, hi, depth)) = queue.pop_front() {
            // 收集出边紧凑码 + 子区间分组。
            let mut child_codes: Vec<i32> = Vec::new();
            let mut terminal: Option<usize> = None;
            let mut i = lo;
            // 唯一编码 → 区间内至多一个 len==depth，且必为最短(排序在前)，即 codes[lo]。
            if codes[lo].len() == depth {
                terminal = Some(lo);
                child_codes.push(0);
                i = lo + 1;
            }
            // 余下编码 len>depth，按第 depth 字节连续分组。
            let mut groups: Vec<(u8, usize, usize)> = Vec::new();
            while i < hi {
                let b = codes[i].as_bytes()[depth];
                let glo = i;
                i += 1;
                while i < hi && codes[i].as_bytes()[depth] == b {
                    i += 1;
                }
                groups.push((b, glo, i));
                child_codes.push(char_map[b as usize]);
            }
            if child_codes.is_empty() {
                continue;
            }
            child_codes.sort_unstable();
            let bv = if child_codes.len() >= HIGH_FANOUT {
                find_base_sparse(
                    &child_codes,
                    &mut base,
                    &mut check,
                    &mut free,
                    &mut sparse_cursor,
                )
            } else {
                find_base(&child_codes, &mut base, &mut check, &mut free)
            };
            base[s as usize] = bv;

            // find_base 返回前已确保下面每个目标位置都在 base/check 范围内且空闲，
            // 这里只需占用（写入 check 并从空闲链表摘除），无需再 grow。
            if let Some(leaf) = terminal {
                let t = bv as usize; // bv + 0
                check[t] = s;
                base[t] = -(leaf as i32) - 1;
                free.occupy(t as u32);
            }
            for (b, glo, ghi) in groups {
                let c = char_map[b as usize];
                let t = (bv + c) as usize;
                check[t] = s;
                free.occupy(t as u32);
                queue.push_back((t as i32, glo, ghi, depth + 1));
            }
        }
    }

    // 4) 裁剪尾部空闲。
    let mut size = base.len();
    while size > 1 && check[size - 1] == -1 {
        size -= 1;
    }
    base.truncate(size);
    check.truncate(size);
    // 预留的尾巴裁掉后把容量也还回去：这两个数组要活到落盘，不还的话 6× 预留里没用上的
    // 部分（简拼类词库可能过半）会一直占着。
    base.shrink_to_fit();
    check.shrink_to_fit();

    Dat {
        base,
        check,
        char_map,
        max_code,
    }
}

/// 空闲槽位链表：把 base/check 中未占用的位置（`check[i]==-1`）串成一条按位置升序的
/// 双向链表。位置 0 恒为根状态的占位，永不入链。
///
/// `find_base` 只在当前确实空闲的位置间跳转，代价与空闲位置数成正比。若改回整数游标逐个
/// 探测，大规模小字母表码表的构建会退化为 O(状态数²)。这是 darts-clone / cedar 的标准做法。
struct FreeList {
    /// next[i] = 位置 i 之后第一个空闲位置，0 表示链尾（0 本身不会是空闲位置）。
    next: Vec<u32>,
    /// prev[i] = 位置 i 之前第一个空闲位置，0 表示链头。
    prev: Vec<u32>,
    /// 当前最小空闲位置，0 表示链表已空（正常构建中不会发生：会先触发扩容）。
    head: u32,
    /// 当前最大空闲位置，供扩容时 O(1) 把新增区间接到链尾。
    tail: u32,
}

impl FreeList {
    fn new(cap: usize) -> Self {
        let mut free = Self {
            next: vec![0; cap],
            prev: vec![0; cap],
            head: 0,
            tail: 0,
        };
        free.link_range(1, cap);
        free
    }

    /// 把 `[from, to)` 标记为空闲并接到当前链尾之后（`from` 必须是全新、此前未纳入链表的区间）。
    fn link_range(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        for i in from..to {
            self.next[i] = if i + 1 < to { (i + 1) as u32 } else { 0 };
            self.prev[i] = if i > from { (i - 1) as u32 } else { self.tail };
        }
        if self.tail == 0 {
            self.head = from as u32;
        } else {
            self.next[self.tail as usize] = from as u32;
        }
        self.tail = (to - 1) as u32;
    }

    /// 扩容到 `new_cap`，新增位置全部标记空闲、接到链尾。
    fn grow_to(&mut self, new_cap: usize) {
        let old_cap = self.next.len();
        if new_cap <= old_cap {
            return;
        }
        self.next.resize(new_cap, 0);
        self.prev.resize(new_cap, 0);
        self.link_range(old_cap, new_cap);
    }

    /// 占用位置 `pos`（须原为空闲），从链表摘除。
    fn occupy(&mut self, pos: u32) {
        let p = self.prev[pos as usize];
        let n = self.next[pos as usize];
        if p != 0 {
            self.next[p as usize] = n;
        } else {
            self.head = n;
        }
        if n != 0 {
            self.prev[n as usize] = p;
        } else {
            self.tail = p;
        }
    }
}

/// 子节点数达到这个值的节点按「稀疏游标」安放（[`find_base_sparse`]）。
///
/// 首次适配从链表头找位置，对小节点好（填头部空洞），对高分支节点是灾难：头部早被填得
/// 七零八落，几十个子节点同时落空的位置要试到很远。故仿 darts-clone / cedar 分两个游标，
/// 高分支节点去尾部稀疏区。取 4：≥4 个子节点的节点占比不高但耗时占大头。
const HIGH_FANOUT: usize = 4;

/// 高分支节点的 base：从 `cursor`（上一个高分支节点落下的位置）起向后找，不回头。
/// 游标之后的区域几乎全空，候选位置极少冲突，代价与子节点数成正比。
/// 游标位置本身可能已被小节点占掉（它们从头部首次适配，偶尔会走到这里），先线性跳过。
fn find_base_sparse(
    codes: &[i32],
    base: &mut Vec<i32>,
    check: &mut Vec<i32>,
    free: &mut FreeList,
    cursor: &mut usize,
) -> i32 {
    let min_code = codes[0];
    let mut pos = *cursor;
    loop {
        if pos >= check.len() {
            let new_cap = base.len() * 2;
            base.resize(new_cap, 0);
            check.resize(new_cap, -1);
            free.grow_to(new_cap);
        }
        if check[pos] != -1 {
            pos += 1;
            continue;
        }
        let b = pos as i32 - min_code;
        if b < 1 {
            pos += 1;
            continue;
        }
        let mut conflict = false;
        for &c in codes {
            let tu = (b + c) as usize;
            if tu >= check.len() {
                let new_cap = (tu + 1).next_power_of_two().max(base.len() * 2);
                base.resize(new_cap, 0);
                check.resize(new_cap, -1);
                free.grow_to(new_cap);
            }
            if check[tu] != -1 {
                conflict = true;
                break;
            }
        }
        if !conflict {
            *cursor = pos;
            return b;
        }
        // 沿空闲链表前进（pos 当前空闲，next 一定指向更后面的空闲位置）
        let nxt = free.next[pos] as usize;
        pos = if nxt == 0 { check.len() } else { nxt };
    }
}

/// 在空闲链表中找到能安放 `codes`（已升序、非空）全部转移而不冲突的 base 值：
/// 只遍历当前空闲位置，不逐整数探测已占用区间——见 [`FreeList`] 顶部说明。
fn find_base(codes: &[i32], base: &mut Vec<i32>, check: &mut Vec<i32>, free: &mut FreeList) -> i32 {
    let min_code = codes[0];
    let mut pos = free.head;
    loop {
        if pos == 0 {
            // 空闲位置已耗尽：扩容后必有新空闲位置可用。
            let new_cap = base.len() * 2;
            base.resize(new_cap, 0);
            check.resize(new_cap, -1);
            free.grow_to(new_cap);
            pos = free.head;
            continue;
        }
        // 候选 base：令最小编码的目标落在这个空闲位置上。
        //
        // `b` 必须 ≥1（不能只满足 `b+min_code>=1`）：`base[s]<0` 是格式判定叶子的符号约定，
        // 非叶状态的 base 若为负，整棵子树会在 MaxW/前缀分支限界的遍历里消失（点查按 check[]
        // 走、不看符号，所以精确匹配测试测不出来）。
        let b = pos as i32 - min_code;
        if b < 1 {
            pos = free.next[pos as usize];
            continue;
        }
        let mut conflict = false;
        for &c in codes {
            let t = b + c;
            let tu = t as usize;
            if tu >= check.len() {
                let new_cap = (tu + 1).next_power_of_two().max(base.len() * 2);
                base.resize(new_cap, 0);
                check.resize(new_cap, -1);
                free.grow_to(new_cap);
            }
            if check[tu] != -1 {
                conflict = true;
                break;
            }
        }
        if !conflict {
            return b;
        }
        pos = free.next[pos as usize];
    }
}

// ======================= 写入 =======================

/// 空槽标记（放在长度位）。真实长度撞不上它：长度为 `u32::MAX` 的串塞不进 `buf`——
/// 池偏移本身就是 `u32`，写到那一步文件偏移早已溢出。
const POOL_EMPTY: u32 = u32::MAX;
/// 初始槽数，必须是 2 的幂（`slot_of` 用 `& mask` 取模）。
const POOL_INITIAL_SLOTS: usize = 1024;

/// FxHash（rustc 自用的那支）。对中文词条这种 3~12 字节的短串，比 `DefaultHasher`
/// 的 SipHash 快一个量级，而本池每条词条都要算一次。
fn pool_hash(bytes: &[u8]) -> u64 {
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
    let mut h: u64 = 0;
    let (chunks, rest) = bytes.as_chunks::<8>();
    for c in chunks {
        let v = u64::from_le_bytes(*c);
        h = (h.rotate_left(5) ^ v).wrapping_mul(SEED);
    }
    if !rest.is_empty() {
        let mut b = [0u8; 8];
        b[..rest.len()].copy_from_slice(rest);
        h = (h.rotate_left(5) ^ u64::from_le_bytes(b)).wrapping_mul(SEED);
    }
    // 长度混进去：否则 "ab\0" 与 "ab" 在补零后同哈希（相等性仍由 slot_of 逐字节判，
    // 这里只是少一次探测冲突）。
    let h = (h.rotate_left(5) ^ bytes.len() as u64).wrapping_mul(SEED);
    // 终混：把高位转到低位。`slot_of` 取槽用低位（`h & mask`），而乘法积的低位只取决于乘数
    // 低位，不转的话短中文串的落点几乎只由头两个字节决定，探测次数暴涨。rustc-hash 2.x 的
    // `finish` 补的也是这一下（同样是 26）。
    h.rotate_left(26)
}

/// 字符串池（去重）。
///
/// 索引里不存字符串副本：槽位存 `(池内偏移, 字节长)`，判等时回 `buf` 取字节。比
/// `HashMap<String, u32>` 省掉每个唯一文本一份 `String`（大词库上几十 MB）和一次堆分配，
/// 索引恒为 `8 字节 × 槽数`。
struct StringPool {
    buf: Vec<u8>,
    /// 开放寻址（线性探测）表，槽位 `(offset, len)`，`len == POOL_EMPTY` 为空。
    /// 槽数恒为 2 的幂；装填过半即翻倍。
    slots: Vec<(u32, u32)>,
    filled: usize,
}

impl StringPool {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            slots: vec![(0, POOL_EMPTY); POOL_INITIAL_SLOTS],
            filled: 0,
        }
    }

    /// 找 `s` 该落的槽：命中返回其槽位，未命中返回第一个空槽。
    ///
    /// 关联函数而非方法：调用方 `add` 随后要改 `self.buf` / `self.slots`，写成 `&self`
    /// 的方法会让借用跨过那些写。
    #[inline]
    fn slot_of(slots: &[(u32, u32)], buf: &[u8], s: &[u8], h: u64) -> usize {
        let mask = slots.len() - 1;
        let mut i = (h as usize) & mask;
        loop {
            let (off, len) = slots[i];
            if len == POOL_EMPTY {
                return i;
            }
            let (a, b) = (off as usize, off as usize + len as usize);
            if len as usize == s.len() && &buf[a..b] == s {
                return i;
            }
            i = (i + 1) & mask;
        }
    }

    /// 槽数翻倍并重散列。重散列要回 `buf` 取字节重算哈希——不缓存哈希值是刻意的：
    /// 每槽多存 8 字节，在 65 万条上就是 10 MB，而重散列一共只发生 log2(n) 次。
    fn grow(&mut self) {
        let mut next = vec![(0u32, POOL_EMPTY); self.slots.len() * 2];
        let mask = next.len() - 1;
        for &(off, len) in &self.slots {
            if len == POOL_EMPTY {
                continue;
            }
            let s = &self.buf[off as usize..off as usize + len as usize];
            let mut i = (pool_hash(s) as usize) & mask;
            while next[i].1 != POOL_EMPTY {
                i = (i + 1) & mask;
            }
            next[i] = (off, len);
        }
        self.slots = next;
    }

    fn add(&mut self, s: &str) -> u32 {
        let b = s.as_bytes();
        let i = Self::slot_of(&self.slots, &self.buf, b, pool_hash(b));
        if self.slots[i].1 != POOL_EMPTY {
            return self.slots[i].0;
        }
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(b);
        self.slots[i] = (off, b.len() as u32);
        self.filled += 1;
        // 装填因子上限 1/2：线性探测在这个水位上平均探测次数仍接近 1。
        if self.filled * 2 >= self.slots.len() {
            self.grow();
        }
        off
    }
}

/// LeafTable 一行：`(该码的条目区字节偏移, 条目数)`。
type DatLeafRow = (u32, u16);

/// EntryTable 一行：`(文本在共享池的偏移, 文本字节长, weight, natural_order, 音节边界位图)`。
type DatEntryRow = (u32, u16, i32, u32, u64);

/// 当前进程匿名常驻内存（MB，RssAnon），只给构建打点用；非 Linux 返回 0。
fn rss_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        // 取 RssAnon 而非 VmRSS：构建期间 mmap 着的段文件页也算 VmRSS，但那是文件页、
        // 可随时回收，不是本进程的私有内存（与 Windows 看 Private 不看 WS 同理）。
        if let Ok(s) = std::fs::read_to_string("/proc/self/status")
            && let Some(kb) = s
                .lines()
                .find_map(|l| l.strip_prefix("RssAnon:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        {
            return kb / 1024;
        }
    }
    0
}

/// 构建完成、待落盘的 .wdat 各段（含全部区段偏移）。由 [`WdatWriter::build`] 产出。
///
/// 与 [`WdatWriter`] 分开是为了让条目数据早点释放。可以 [`emit`](Self::emit) 多次：
/// 落盘失败要退到别的目录或换个地方重来时，需要的只是这些字节。
pub struct WdatBlob {
    dat: Dat,
    leaves: Vec<DatLeafRow>,
    entries: Vec<DatEntryRow>,
    maxw: Vec<i32>,
    a_dat: Option<Dat>,
    a_leaves: Vec<DatLeafRow>,
    a_entries: Vec<DatEntryRow>,
    a_maxw: Vec<i32>,
    pool_buf: Vec<u8>,
    meta: Option<Vec<u8>>,
    dat_size: u32,
    dat_off: u32,
    leaf_off: u32,
    entry_off: u32,
    str_off: u32,
    abbrev_off: u32,
    a_dat_off: u32,
    a_leaf_off: u32,
    a_entry_off: u32,
    a_charmap_off: u32,
    char_map_off: u32,
    meta_off: u32,
}

impl WdatBlob {
    /// 原子写一个目标：tmp+pid+seq → rename。`&self`，故可在多个目标上重试。
    pub fn emit(&self, path: &Path) -> anyhow::Result<()> {
        use std::io::Write;
        let t_emit = std::time::Instant::now();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let seq = ATOMIC_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut tmp_os = path.as_os_str().to_os_string();
        tmp_os.push(format!(".tmp.{}.{seq}", std::process::id()));
        let tmp = std::path::PathBuf::from(tmp_os);
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);

        // Header (48B, LE)。
        f.write_all(&MAGIC)?;
        f.write_all(&VERSION.to_le_bytes())?;
        f.write_all(&self.dat_size.to_le_bytes())?;
        f.write_all(&(self.leaves.len() as u32).to_le_bytes())?;
        f.write_all(&self.dat_off.to_le_bytes())?;
        f.write_all(&self.leaf_off.to_le_bytes())?;
        f.write_all(&self.entry_off.to_le_bytes())?;
        f.write_all(&self.str_off.to_le_bytes())?;
        f.write_all(&self.abbrev_off.to_le_bytes())?;
        f.write_all(&self.meta_off.to_le_bytes())?;
        f.write_all(&(self.entries.len() as u32).to_le_bytes())?;
        f.write_all(&self.char_map_off.to_le_bytes())?;

        let write_dat_section = |f: &mut std::io::BufWriter<std::fs::File>,
                                 dat: &Dat,
                                 maxw: &[i32],
                                 leaves: &[DatLeafRow],
                                 entries: &[DatEntryRow]|
         -> std::io::Result<()> {
            for v in &dat.base {
                f.write_all(&v.to_le_bytes())?;
            }
            for v in &dat.check {
                f.write_all(&v.to_le_bytes())?;
            }
            // v6 MaxW：与 base/check 等长，长度不符即为构建 bug（读取侧按 dat_size 定长切片）。
            debug_assert_eq!(maxw.len(), dat.base.len());
            for v in maxw {
                f.write_all(&v.to_le_bytes())?;
            }
            for (eoff, elen) in leaves {
                f.write_all(&eoff.to_le_bytes())?;
                f.write_all(&elen.to_le_bytes())?;
                f.write_all(&0u16.to_le_bytes())?;
            }
            for (toff, tlen, w, order, boundary) in entries {
                f.write_all(&toff.to_le_bytes())?;
                f.write_all(&tlen.to_le_bytes())?;
                f.write_all(&w.to_le_bytes())?;
                f.write_all(&order.to_le_bytes())?;
                f.write_all(&boundary.to_le_bytes())?; // v4：音节边界（22B 中的末 8B）
            }
            Ok(())
        };
        let write_charmap =
            |f: &mut std::io::BufWriter<std::fs::File>, dat: &Dat| -> std::io::Result<()> {
                f.write_all(&dat.max_code.to_le_bytes())?;
                for c in &dat.char_map {
                    f.write_all(&c.to_le_bytes())?;
                }
                Ok(())
            };

        // 主区段 + 共享池。
        write_dat_section(&mut f, &self.dat, &self.maxw, &self.leaves, &self.entries)?;
        f.write_all(&self.pool_buf)?;

        // 简拼区段：自描述头 + DAT/leaf/entry + 简拼 CharMap。
        if let Some(ad) = &self.a_dat {
            f.write_all(&(ad.base.len() as u32).to_le_bytes())?;
            f.write_all(&(self.a_leaves.len() as u32).to_le_bytes())?;
            f.write_all(&self.a_dat_off.to_le_bytes())?;
            f.write_all(&self.a_leaf_off.to_le_bytes())?;
            f.write_all(&self.a_entry_off.to_le_bytes())?;
            f.write_all(&self.a_charmap_off.to_le_bytes())?;
            write_dat_section(&mut f, ad, &self.a_maxw, &self.a_leaves, &self.a_entries)?;
            write_charmap(&mut f, ad)?;
        }

        // 主 CharMap。
        write_charmap(&mut f, &self.dat)?;

        // Meta。
        if let Some(m) = &self.meta
            && !m.is_empty()
        {
            f.write_all(&(m.len() as u32).to_le_bytes())?;
            f.write_all(m)?;
        }

        f.flush()?;
        drop(f);
        // 池里可能还存着指向替换前那份数据的 mmap reader，且 (大小, mtime) 未必看得出
        // 差别（同构词库 + 同一 mtime 刻度）。守卫圈住 rename，见 `reader_pool::replacing`。
        let replacing = crate::reader_pool::replacing(path);
        let renamed = std::fs::rename(&tmp, path);
        drop(replacing);
        if let Err(e) = renamed {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        info!(
            "Wrote wdat: {} keys, {} abbrevs, {} entries, dat_size={}",
            self.leaves.len(),
            self.a_leaves.len(),
            self.entries.len(),
            self.dat_size
        );
        tracing::debug!(
            emit_ms = t_emit.elapsed().as_millis() as u64,
            "wdat emit done"
        );
        Ok(())
    }
}

/// 计算 MaxW 段（v6）：`maxw[s]` = 以状态 s 为根的子树中所有条目 weight 的最大值。
///
/// 不变量：对任意状态 s 与任意 `e ∈ subtree(s)`，`weight(e) <= maxw[s]`。偏大无害、偏小会漏
/// 结果，单元测试须逐状态对拍暴力结果。实现为两趟而非递归（极长编码有栈溢出风险）：前序 DFS
/// 收集访问序，再逆序回填，等价于后序遍历。
fn compute_maxw(dat: &Dat, leaves: &[DatLeafRow], entries: &[DatEntryRow]) -> Vec<i32> {
    let n = dat.base.len();
    let mut maxw = vec![NO_MAXW; n];
    if n == 0 {
        return maxw;
    }
    let in_range = |t: i32| t >= 0 && (t as usize) < n;
    // 状态 s 经终止符（紧凑码 0）指向的叶节点，返回 (叶状态下标, LeafTable 索引)。
    let terminal = |s: i32| -> Option<(usize, usize)> {
        let t = dat.base[s as usize]; // + 0
        if !in_range(t) || dat.check[t as usize] != s {
            return None;
        }
        let bt = dat.base[t as usize];
        if bt >= 0 {
            return None; // 非叶
        }
        Some((t as usize, (-bt - 1) as usize))
    };
    // 某个叶（一码多词）内的最大 weight。
    let leaf_max = |leaf_idx: usize| -> i32 {
        let Some(&(byte_off, len)) = leaves.get(leaf_idx) else {
            return NO_MAXW;
        };
        let start = byte_off as usize / ENTRY_SIZE;
        entries
            .iter()
            .skip(start)
            .take(len as usize)
            .map(|e| e.2)
            .max()
            .unwrap_or(NO_MAXW)
    };

    // 第一趟：前序 DFS，收集正常状态（叶状态经 c=0 到达，不入序列、不展开）。
    let mut order: Vec<i32> = Vec::with_capacity(n);
    let mut stack: Vec<i32> = vec![0];
    while let Some(s) = stack.pop() {
        order.push(s);
        let bs = dat.base[s as usize];
        if bs < 0 {
            continue; // 叶状态无出边（防御，构建侧不应产生）
        }
        for c in 1..=dat.max_code {
            let t = bs + c;
            if in_range(t) && dat.check[t as usize] == s {
                stack.push(t);
            }
        }
    }

    // 第二趟：逆序回填。
    for &s in order.iter().rev() {
        let mut m = NO_MAXW;
        if let Some((leaf_state, leaf_idx)) = terminal(s) {
            let lw = leaf_max(leaf_idx);
            maxw[leaf_state] = lw; // 叶状态自身也填上，保持段语义自洽（查询不读它）
            m = m.max(lw);
        }
        let bs = dat.base[s as usize];
        if bs >= 0 {
            for c in 1..=dat.max_code {
                let t = bs + c;
                if in_range(t) && dat.check[t as usize] == s {
                    m = m.max(maxw[t as usize]);
                }
            }
        }
        maxw[s as usize] = m;
    }
    maxw
}

/// 写入侧的一条候选：`(text, weight, order, boundary)`。
/// boundary 见 [`DictEntry::boundary`]（音节起始位 bitmask，0=无边界信息）。
type WriteEntry = (String, i32, u32, u64);

/// wdat 写入器：`add(code, entries)` 收集全部条目后一次构建。`add_abbrev` 追加简拼表，
/// 写入独立 AbbrevSection（与全拼查询互不污染）。
pub struct WdatWriter {
    keys: Vec<(String, Vec<WriteEntry>)>,
    abbrevs: Vec<(String, Vec<WriteEntry>)>,
    meta: Option<Vec<u8>>,
}

/// 把 `(text, weight)` 列表补上 order：order = 该 code 内的条目序号（0,1,2…）。
/// 复现 v2「叶内序号」语义，供未显式提供全局序的调用方（combined 合并、测试）向后兼容。
/// boundary 置 0（无边界信息）——本入口的调用方均非拼音全量构建路径。
fn with_local_order(entries: Vec<(String, i32)>) -> Vec<WriteEntry> {
    entries
        .into_iter()
        .enumerate()
        .map(|(i, (t, w))| (t, w, i as u32, 0u64))
        .collect()
}

impl WdatWriter {
    pub fn new() -> Self {
        Self {
            keys: Vec::new(),
            abbrevs: Vec::new(),
            meta: None,
        }
    }

    /// 追加一个 code 的候选（`(text, weight)`）。order 按 code 内序号自动补全（向后兼容旧语义）。
    /// 需要跨编码全局自然序（无权重按词库出现顺序）时改用 [`add_with_order`]。
    pub fn add(&mut self, code: String, entries: Vec<(String, i32)>) {
        if !entries.is_empty() {
            self.keys.push((code, with_local_order(entries)));
        }
    }

    /// 追加一个 code 的候选，携带显式全局 order（`(text, weight, order)`）。
    /// order 为词库文件内的全局出现序（跨编码单调），使等权候选跨编码按出现顺序排列。
    /// order 须 < `composite::PER_LAYER_NO_OFFSET`（1e7），否则会溢出到层序偏移带。
    /// boundary 置 0（无边界信息）；拼音全量构建请用 [`Self::add_with_boundary`]。
    pub fn add_with_order(&mut self, code: String, entries: Vec<(String, i32, u32)>) {
        if !entries.is_empty() {
            self.keys.push((
                code,
                entries
                    .into_iter()
                    .map(|(t, w, o)| (t, w, o, 0u64))
                    .collect(),
            ));
        }
    }

    /// 追加一个 code 的候选，携带 order 与音节边界（`(text, weight, order, boundary)`）。
    /// 供拼音词典构建路径使用——边界取自 rime 源数据 `ni hao` 的空格（真值，非 DAG 猜测）。
    /// boundary 语义见 [`DictEntry::boundary`]。
    pub fn add_with_boundary(&mut self, code: String, entries: Vec<WriteEntry>) {
        if !entries.is_empty() {
            self.keys.push((code, entries));
        }
    }

    /// 追加简拼条目：`abbrev`=声母序列（`nh`），`entries` 每条 `(全拼码, weight)`，
    /// 存的是码不是词（v5）。空条目忽略；order 按叶内序号补全。
    /// `weight` 只决定该简拼下截断时保留哪些码，候选自身的权重来自主表。
    pub fn add_abbrev(&mut self, abbrev: String, entries: Vec<(String, i32)>) {
        if !entries.is_empty() {
            self.abbrevs.push((abbrev, with_local_order(entries)));
        }
    }

    pub fn set_meta(&mut self, meta: Vec<u8>) {
        self.meta = Some(meta);
    }

    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// 构建各段并原子写 .wdat（tmp+pid+seq → rename，防读到半文件）。消费 `self`，让条目数据
    /// 早点释放。要在多个目标之间回退的调用方用 [`Self::build`] 拿到可多次落盘的 [`WdatBlob`]。
    pub fn write(self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        self.build().emit(path.as_ref())
    }

    /// 排序 + 建 DAT + 入池，产出待落盘的 [`WdatBlob`]。消费 `self`，条目数据在这一步结束时
    /// 已释放；想落盘多次就用这个。
    pub fn build(mut self) -> WdatBlob {
        // 按 code 排序（确定性 + DAT key 唯一）后喂给流式写入器，与流式路径共用一套构建，
        // 同一输入逐字节相同（`stream_writer_matches_batch_writer_byte_for_byte`）。排 owned
        // 元组是为了逐 code 喂完立刻释放。
        let t_all = std::time::Instant::now();
        let mut keys = std::mem::take(&mut self.keys);
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        let sort_ms = t_all.elapsed().as_millis() as u64;
        let mut sw = WdatStreamWriter::new();
        sw.sort_ms = sort_ms;
        for (code, ents) in keys {
            sw.push_code(
                &code,
                ents.iter().map(|(t, w, o, b)| (t.as_str(), *w, *o, *b)),
            );
            // `ents` 在此归还：文本已进池，这份 `String` 不必活到写盘。
        }
        let mut abbrevs = std::mem::take(&mut self.abbrevs);
        abbrevs.sort_by(|a, b| a.0.cmp(&b.0));
        for (ab, ents) in abbrevs {
            sw.push_abbrev_sorted(&ab, ents.iter().map(|(t, w, _, _)| (t.as_str(), *w)));
        }
        sw.meta = self.meta.take();
        sw.build()
    }
}

/// 流式写入器：code 按字节序严格递增喂入，text 字节直接进池、不建 `String`，码存进连续
/// 字节缓冲加偏移表。常驻 ≈ 池 + 条目行 + 码缓冲，不像 [`WdatWriter`] 那样收满 owned 元组。
///
/// 喂入顺序就是 DAT 的键序（由段的 [`WdatReader::codes`] 游标或排好序的内存表保证）；
/// 简拼键同样按序喂入，各简拼下的条目顺序照原样写（调用方负责「权重降序、码升序」）。
pub struct WdatStreamWriter {
    pool: StringPool,
    codes_buf: Vec<u8>,
    code_spans: Vec<(u32, u16)>,
    leaves: Vec<DatLeafRow>,
    entries: Vec<DatEntryRow>,
    entry_byte_off: u32,
    abbrevs_buf: Vec<u8>,
    abbrev_spans: Vec<(u32, u16)>,
    a_leaves: Vec<DatLeafRow>,
    a_entries: Vec<DatEntryRow>,
    a_entry_byte_off: u32,
    meta: Option<Vec<u8>>,
    /// 由 [`WdatWriter::build`] 填入，只进 debug 打点。
    sort_ms: u64,
}

impl Default for WdatStreamWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl WdatStreamWriter {
    pub fn new() -> Self {
        Self {
            pool: StringPool::new(),
            codes_buf: Vec::new(),
            code_spans: Vec::new(),
            leaves: Vec::new(),
            entries: Vec::new(),
            entry_byte_off: 0,
            abbrevs_buf: Vec::new(),
            abbrev_spans: Vec::new(),
            a_leaves: Vec::new(),
            a_entries: Vec::new(),
            a_entry_byte_off: 0,
            meta: None,
            sort_ms: 0,
        }
    }

    fn last_span<'b>(buf: &'b [u8], spans: &[(u32, u16)]) -> Option<&'b [u8]> {
        spans
            .last()
            .map(|&(o, l)| &buf[o as usize..o as usize + l as usize])
    }

    /// 喂一个 code 及其候选 `(text, weight, order, boundary)`。code 必须大于上一个（字节序），
    /// 否则 DAT 会建错——这是调用方的不变量，违背即 panic。空候选列表的 code 跳过
    /// （与 [`WdatWriter::add`] 一致）。
    pub fn push_code<'a>(
        &mut self,
        code: &str,
        entries: impl IntoIterator<Item = (&'a str, i32, u32, u64)>,
    ) {
        let mut n = 0usize;
        let start_off = self.entry_byte_off;
        for (text, weight, order, boundary) in entries {
            let text_off = self.pool.add(text);
            self.entries
                .push((text_off, text.len() as u16, weight, order, boundary));
            n += 1;
        }
        if n == 0 {
            return;
        }
        if let Some(prev) = Self::last_span(&self.codes_buf, &self.code_spans) {
            assert!(
                code.as_bytes() > prev,
                "WdatStreamWriter: code 必须按字节序严格递增喂入"
            );
        }
        self.code_spans
            .push((self.codes_buf.len() as u32, code.len() as u16));
        self.codes_buf.extend_from_slice(code.as_bytes());
        // 叶的计数是 u16：超过 65535 条只截断计数，条目区偏移仍按实际条数前进——
        // 与从前 `build_section` 一致，后续叶的偏移才不会错位（release 不查溢出，
        // 用 u16 累加会回绕）。
        self.leaves
            .push((start_off, n.min(u16::MAX as usize) as u16));
        self.entry_byte_off += (n * ENTRY_SIZE) as u32;
    }

    /// 喂一个简拼键及其全拼码列表 `(code, weight)`，键须按字节序严格递增；列表顺序照写
    /// （order 取列表序号，boundary 恒 0——简拼码不构成音节序列）。
    pub fn push_abbrev_sorted<'a>(
        &mut self,
        abbrev: &str,
        codes: impl IntoIterator<Item = (&'a str, i32)>,
    ) {
        let mut n = 0usize;
        let start_off = self.a_entry_byte_off;
        for (i, (code, weight)) in codes.into_iter().enumerate() {
            let off = self.pool.add(code);
            self.a_entries
                .push((off, code.len() as u16, weight, i as u32, 0u64));
            n += 1;
        }
        if n == 0 {
            return;
        }
        if let Some(prev) = Self::last_span(&self.abbrevs_buf, &self.abbrev_spans) {
            assert!(
                abbrev.as_bytes() > prev,
                "WdatStreamWriter: 简拼键必须按字节序严格递增喂入"
            );
        }
        self.abbrev_spans
            .push((self.abbrevs_buf.len() as u32, abbrev.len() as u16));
        self.abbrevs_buf.extend_from_slice(abbrev.as_bytes());
        self.a_leaves
            .push((start_off, n.min(u16::MAX as usize) as u16));
        self.a_entry_byte_off += (n * ENTRY_SIZE) as u32;
    }

    pub fn key_count(&self) -> usize {
        self.code_spans.len()
    }

    pub fn abbrev_count(&self) -> usize {
        self.abbrev_spans.len()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// 第 `idx` 个已喂入的 code（按喂入序）。供归并侧回查简拼对应的全拼码。
    pub fn code_at(&self, idx: usize) -> &str {
        let (o, l) = self.code_spans[idx];
        std::str::from_utf8(&self.codes_buf[o as usize..o as usize + l as usize]).unwrap_or("")
    }

    pub fn set_meta(&mut self, meta: Vec<u8>) {
        self.meta = Some(meta);
    }

    fn spans_as_strs<'b>(buf: &'b [u8], spans: &[(u32, u16)]) -> Vec<&'b str> {
        spans
            .iter()
            .map(|&(o, l)| {
                std::str::from_utf8(&buf[o as usize..o as usize + l as usize]).unwrap_or("")
            })
            .collect()
    }

    /// 建 DAT、算 MaxW、定各段偏移。条目行与池在喂入时已成型，这里只剩两棵 DAT。
    pub fn build(self) -> WdatBlob {
        let t_all = std::time::Instant::now();
        let rss_start = rss_mb();
        tracing::debug!(
            entries = self.entries.len(),
            pool_bytes = self.pool.buf.len(),
            rss_mb = rss_start,
            "wdat section: entries pooled"
        );
        let codes = Self::spans_as_strs(&self.codes_buf, &self.code_spans);
        let t_dat = std::time::Instant::now();
        let dat = build_dat_from_sorted(&codes);
        tracing::debug!(
            codes = codes.len(),
            dat_size = dat.base.len(),
            dat_ms = t_dat.elapsed().as_millis() as u64,
            rss_mb = rss_mb(),
            "wdat section: DAT built"
        );
        drop(codes);
        let Self {
            pool,
            codes_buf,
            code_spans,
            leaves,
            entries,
            abbrevs_buf,
            abbrev_spans,
            a_leaves,
            a_entries,
            meta,
            sort_ms,
            ..
        } = self;
        drop((codes_buf, code_spans));
        let t_main = t_all.elapsed();

        let a_dat = if abbrev_spans.is_empty() {
            None
        } else {
            let abbrevs = Self::spans_as_strs(&abbrevs_buf, &abbrev_spans);
            let t_dat = std::time::Instant::now();
            let d = build_dat_from_sorted(&abbrevs);
            tracing::debug!(
                codes = abbrevs.len(),
                dat_size = d.base.len(),
                dat_ms = t_dat.elapsed().as_millis() as u64,
                rss_mb = rss_mb(),
                "wdat section: abbrev DAT built"
            );
            Some(d)
        };
        drop((abbrevs_buf, abbrev_spans));
        let t_abbrev = t_all.elapsed();

        let blob = assemble_blob(dat, leaves, entries, a_dat, a_leaves, a_entries, pool, meta);
        tracing::debug!(
            keys = blob.leaves.len(),
            entries = blob.entries.len(),
            dat_size = blob.dat_size,
            sort_ms,
            main_section_ms = t_main.as_millis() as u64,
            abbrev_section_ms = (t_abbrev - t_main).as_millis() as u64,
            maxw_ms = (t_all.elapsed() - t_abbrev).as_millis() as u64,
            total_ms = t_all.elapsed().as_millis() as u64 + sort_ms,
            rss_start_mb = rss_start,
            rss_end_mb = rss_mb(),
            "wdat build stages"
        );
        blob
    }
}

/// MaxW、各段偏移与 [`WdatBlob`] 组装：两条写入路径的共同尾巴。
#[allow(clippy::too_many_arguments)]
fn assemble_blob(
    dat: Dat,
    leaves: Vec<DatLeafRow>,
    entries: Vec<DatEntryRow>,
    a_dat: Option<Dat>,
    a_leaves: Vec<DatLeafRow>,
    a_entries: Vec<DatEntryRow>,
    pool: StringPool,
    meta: Option<Vec<u8>>,
) -> WdatBlob {
    // 池的索引到这里就没用了（去重只发生在构建期），显式解构让那 8B×槽数 立刻归还
    // ——65 万条约 10 MB，而下面还要把整个文件写一遍。
    let StringPool {
        buf: pool_buf,
        slots,
        filled: _,
    } = pool;
    drop(slots);

    // MaxW 段（v6 剪枝上界），与各自的 DAT 同长。
    let maxw = compute_maxw(&dat, &leaves, &entries);
    let a_maxw = a_dat
        .as_ref()
        .map(|ad| compute_maxw(ad, &a_leaves, &a_entries))
        .unwrap_or_default();
    // 主区段偏移。base/check/maxw 三段等长，故 leaf 段起点为 dat_off + dat_size*4*3。
    let dat_size = dat.base.len() as u32;
    let dat_off = HEADER_SIZE as u32;
    let leaf_off = dat_off + dat_size * 4 * 3;
    let entry_off = leaf_off + (leaves.len() * LEAF_SIZE) as u32;
    let str_off = entry_off + (entries.len() * ENTRY_SIZE) as u32;
    let after_pool = str_off + pool_buf.len() as u32;

    // 简拼区段（AbbrevSection）：紧跟共享池之后。自描述头 24B（6×u32）：
    // {dat_size, leaf_count, dat_off, leaf_off, entry_off, char_map_off}。
    const ABBREV_HDR: u32 = 24;
    let (abbrev_off, a_dat_off, a_leaf_off, a_entry_off, a_charmap_off, after_abbrev) =
        if let Some(ad) = &a_dat {
            let abbrev_off = after_pool;
            let a_dat_off = abbrev_off + ABBREV_HDR;
            let a_dat_size = ad.base.len() as u32;
            let a_leaf_off = a_dat_off + a_dat_size * 4 * 3;
            let a_entry_off = a_leaf_off + (a_leaves.len() * LEAF_SIZE) as u32;
            let a_charmap_off = a_entry_off + (a_entries.len() * ENTRY_SIZE) as u32;
            let after = a_charmap_off + CHARMAP_SIZE as u32;
            (
                abbrev_off,
                a_dat_off,
                a_leaf_off,
                a_entry_off,
                a_charmap_off,
                after,
            )
        } else {
            (0, 0, 0, 0, 0, after_pool)
        };

    let char_map_off = after_abbrev;
    let meta_off = match &meta {
        Some(m) if !m.is_empty() => char_map_off + CHARMAP_SIZE as u32,
        _ => 0,
    };

    WdatBlob {
        dat,
        leaves,
        entries,
        maxw,
        a_dat,
        a_leaves,
        a_entries,
        a_maxw,
        pool_buf,
        meta,
        dat_size,
        dat_off,
        leaf_off,
        entry_off,
        str_off,
        abbrev_off,
        a_dat_off,
        a_leaf_off,
        a_entry_off,
        a_charmap_off,
        char_map_off,
        meta_off,
    }
}

impl Default for WdatWriter {
    fn default() -> Self {
        Self::new()
    }
}

// ======================= 读取（mmap 零拷贝） =======================

/// 前缀查询的执行统计（诊断与收益验证用；生产路径可忽略）。
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixSearchStats {
    /// 实际出队展开的状态数（分支限界记号 V）。与子树总状态数之比即剪枝效果。
    pub states_visited: usize,
    /// 实际读取的条目数（分支限界记号 E）。
    pub entries_read: usize,
}

/// 前缀查询的排序键。`Ord` 定义为「越差越大」，故堆顶恒为当前最差的入选者，
/// 且 `into_sorted_vec()`（升序）直接给出最优→最差的最终顺序。
///
/// 三、四级 tie-break 取 `(leaf, slot)` 而不是遍历序：局部 order 的词库等权时大量打平，
/// 拿遍历序会让 DFS 与分支限界给出不同结果，既无法对拍也会随实现改动抖动。
#[derive(PartialEq, Eq)]
struct RankKey {
    weight: i32,
    order: i32,
    leaf: u32,
    slot: u16,
}
impl Ord for RankKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // weight 小 → 差；order 大 → 差；leaf/slot 大 → 差。
        other
            .weight
            .cmp(&self.weight)
            .then(self.order.cmp(&other.order))
            .then(self.leaf.cmp(&other.leaf))
            .then(self.slot.cmp(&other.slot))
    }
}
impl PartialOrd for RankKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
struct Ranked {
    key: RankKey,
    entry: DictEntry,
}
impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl Eq for Ranked {}
impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 分支限界的待展开项，按子树权重上界 `bound` 降序出队。
struct Pending {
    bound: i32,
    state: i32,
    /// `PathArena` 索引；`u32::MAX` 表示「路径就是查询前缀本身」。
    path: u32,
    /// 该状态对应的完整码长（字节）。
    len: u32,
}
impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.bound == other.bound && self.state == other.state
    }
}
impl Eq for Pending {}
impl Ord for Pending {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap 是最大堆：bound 大者先出队。bound 相等时按 state 升序（任意但确定，
        // 保证同一词库上的执行完全可复现）。
        self.bound
            .cmp(&other.bound)
            .then(other.state.cmp(&self.state))
    }
}
impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 路径链节点：`(父节点索引, 该步字节)`。入队时不拼 String（多数入队项会被剪枝），
/// 只在真正产出条目时才回溯拼接。
struct PathNode {
    parent: u32,
    byte: u8,
}

/// 一段 DAT 的视图（主表 / 简拼表各一份），供查询方法复用同一套 walk/DFS 逻辑。
struct DatView {
    dat_off: usize,
    check_off: usize,
    maxw_off: usize, // v6：子树最大 weight（剪枝上界），与 base/check 等长
    dat_size: u32,
    leaf_off: usize,
    entry_off: usize,
    char_map: [i32; 256],
    rev_map: Vec<u8>, // 紧凑码 → 原始字节（1..=max_code）
    max_code: i32,
}

/// [`WdatReader::codes`] 的游标：显式栈 DFS，与 `for_each_leaf` 同一遍历序，但可拉式逐项取。
pub struct CodeCursor<'a> {
    r: &'a WdatReader,
    /// `(状态, 进入该状态时 path 的长度, 下一个要试的紧凑码)`
    stack: Vec<(i32, usize, i32)>,
    path: Vec<u8>,
    root_pending: bool,
}

impl Iterator for CodeCursor<'_> {
    type Item = (String, u32);

    fn next(&mut self) -> Option<Self::Item> {
        let v = &self.r.main;
        if self.root_pending {
            self.root_pending = false;
            if let Some(leaf) = self.r.terminal_leaf(v, 0) {
                return Some((String::new(), leaf));
            }
        }
        while let Some(top) = self.stack.len().checked_sub(1) {
            let (s, plen, _) = self.stack[top];
            self.path.truncate(plen);
            let mut descended = false;
            while self.stack[top].2 <= v.max_code {
                let c = self.stack[top].2;
                self.stack[top].2 += 1;
                let t = self.r.base(v, s) + c;
                if !WdatReader::in_range(v, t) || self.r.check(v, t) != s {
                    continue;
                }
                self.path.push(v.rev_map[c as usize]);
                self.stack.push((t, self.path.len(), 1));
                descended = true;
                if let Some(leaf) = self.r.terminal_leaf(v, t) {
                    let code = std::str::from_utf8(&self.path).unwrap_or("").to_string();
                    return Some((code, leaf));
                }
                break;
            }
            if !descended {
                self.stack.pop();
            }
        }
        None
    }
}

pub struct WdatReader {
    mmap: Mapped,
    /// 本 reader 映射的文件路径。二级缓存（`.wridx` 指纹）靠它认出派生源，不要在调用方
    /// 重新推导一遍。
    path: std::path::PathBuf,
    leaf_count: u32,
    str_off: usize, // 共享字符串池（主表与简拼共用）
    main: DatView,
    abbrev: Option<DatView>, // 简拼区段（声母缩写，独立 DAT，不污染全拼前缀查询）
}

impl WdatReader {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path)?;
        let mmap = crate::mapping::map_file(&file)?;
        if mmap.len() < HEADER_SIZE {
            anyhow::bail!("wdat too short");
        }
        if mmap[0..4] != MAGIC {
            anyhow::bail!("invalid wdat magic: {:?}", &mmap[0..4]);
        }
        let file_len = mmap.len();
        let rd = |off: usize| u32::from_le_bytes(mmap[off..off + 4].try_into().unwrap());
        // 版本必须匹配：v2→v3 EntryRecord 由 10→14B，旧缓存若按新步长解析会读到错乱候选。
        // 校验失败即 bail，调用方（cached/load_merged_dicts）捕获 Err 后回退重建，自动升级缓存。
        let version = rd(4);
        if version != VERSION {
            anyhow::bail!("wdat version mismatch: file={version}, expected={VERSION} (需重建缓存)");
        }
        let dat_size = rd(8);
        let leaf_count = rd(12);
        let dat_off = rd(16) as usize;
        let leaf_off = rd(20) as usize;
        let entry_off = rd(24) as usize;
        let str_off = rd(28) as usize;
        let abbrev_off = rd(32) as usize;
        let char_map_off = rd(44) as usize;

        // 从 char_map_off 读 CharMap → (char_map, rev_map, max_code)。
        let read_charmap = |off: usize| -> ([i32; 256], Vec<u8>, i32) {
            let max_code = i32::from_le_bytes(mmap[off..off + 4].try_into().unwrap());
            let mut cm = [-1i32; 256];
            for (b, slot) in cm.iter_mut().enumerate() {
                let o = off + 4 + b * 4;
                *slot = i32::from_le_bytes(mmap[o..o + 4].try_into().unwrap());
            }
            let mut rm = vec![0u8; (max_code.max(0) as usize) + 1];
            for (b, &c) in cm.iter().enumerate() {
                if c > 0 && (c as usize) < rm.len() {
                    rm[c as usize] = b as u8;
                }
            }
            (cm, rm, max_code)
        };

        // 主区段越界校验。v6 起 base/check/maxw 三段等长依次排列。
        let check_off = dat_off + dat_size as usize * 4;
        let maxw_off = check_off + dat_size as usize * 4;
        if maxw_off + dat_size as usize * 4 > file_len
            || char_map_off + CHARMAP_SIZE > file_len
            || leaf_off > file_len
            || str_off > file_len
        {
            anyhow::bail!("wdat offsets out of range");
        }
        let (char_map, rev_map, max_code) = read_charmap(char_map_off);
        let main = DatView {
            dat_off,
            check_off,
            maxw_off,
            dat_size,
            leaf_off,
            entry_off,
            char_map,
            rev_map,
            max_code,
        };

        // 简拼区段（自描述头 24B：dat_size, leaf_count, dat_off, leaf_off, entry_off, char_map_off）。
        let abbrev = if abbrev_off != 0 && abbrev_off + 24 <= file_len {
            let a_dat_size = rd(abbrev_off);
            let a_dat_off = rd(abbrev_off + 8) as usize;
            let a_leaf_off = rd(abbrev_off + 12) as usize;
            let a_entry_off = rd(abbrev_off + 16) as usize;
            let a_charmap_off = rd(abbrev_off + 20) as usize;
            if a_charmap_off + CHARMAP_SIZE <= file_len
                && a_dat_off + a_dat_size as usize * 12 <= file_len
            {
                let (cm, rm, mc) = read_charmap(a_charmap_off);
                Some(DatView {
                    dat_off: a_dat_off,
                    check_off: a_dat_off + a_dat_size as usize * 4,
                    maxw_off: a_dat_off + a_dat_size as usize * 8,
                    dat_size: a_dat_size,
                    leaf_off: a_leaf_off,
                    entry_off: a_entry_off,
                    char_map: cm,
                    rev_map: rm,
                    max_code: mc,
                })
            } else {
                None
            }
        } else {
            None
        };

        info!(
            "Opened wdat: {} ({} keys, dat_size={}, abbrev={})",
            path.display(),
            leaf_count,
            dat_size,
            abbrev.is_some()
        );
        Ok(Self {
            mmap,
            path: path.to_path_buf(),
            leaf_count,
            str_off,
            main,
            abbrev,
        })
    }

    pub fn key_count(&self) -> u32 {
        self.leaf_count
    }

    /// 本 reader 映射的 wdat 文件路径（见字段注释：供二级缓存认源）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[inline]
    fn base(&self, v: &DatView, i: i32) -> i32 {
        let o = v.dat_off + (i as usize) * 4;
        i32::from_le_bytes(self.mmap[o..o + 4].try_into().unwrap())
    }
    #[inline]
    fn check(&self, v: &DatView, i: i32) -> i32 {
        let o = v.check_off + (i as usize) * 4;
        i32::from_le_bytes(self.mmap[o..o + 4].try_into().unwrap())
    }
    /// v6 剪枝上界：以状态 i 为根的子树内最大 weight（`NO_MAXW` = 子树无条目）。
    #[inline]
    fn maxw(&self, v: &DatView, i: i32) -> i32 {
        let o = v.maxw_off + (i as usize) * 4;
        i32::from_le_bytes(self.mmap[o..o + 4].try_into().unwrap())
    }
    #[inline]
    fn in_range(v: &DatView, t: i32) -> bool {
        t >= 0 && (t as u32) < v.dat_size
    }

    /// 沿 code 走到状态（不含终止符）。失败返回 None。
    fn walk(&self, v: &DatView, code: &str) -> Option<i32> {
        let mut s = 0i32;
        for &b in code.as_bytes() {
            let c = v.char_map[b as usize];
            if c < 0 {
                return None;
            }
            let t = self.base(v, s) + c;
            if !Self::in_range(v, t) || self.check(v, t) != s {
                return None;
            }
            s = t;
        }
        Some(s)
    }

    /// 状态 s 的终止符叶（若有）→ LeafTable 索引。
    fn terminal_leaf(&self, v: &DatView, s: i32) -> Option<u32> {
        let t = self.base(v, s); // + 0
        if !Self::in_range(v, t) || self.check(v, t) != s {
            return None;
        }
        let bt = self.base(v, t);
        if bt >= 0 {
            return None; // 非叶
        }
        Some((-bt - 1) as u32)
    }

    fn read_leaf(&self, v: &DatView, leaf_idx: u32) -> DatLeafRow {
        let o = v.leaf_off + leaf_idx as usize * LEAF_SIZE;
        let eoff = u32::from_le_bytes(self.mmap[o..o + 4].try_into().unwrap());
        let elen = u16::from_le_bytes(self.mmap[o + 4..o + 6].try_into().unwrap());
        (eoff, elen)
    }

    fn read_string(&self, off: u32, len: u16) -> &str {
        let start = self.str_off + off as usize;
        let end = start + len as usize;
        if end > self.mmap.len() {
            return "";
        }
        std::str::from_utf8(&self.mmap[start..end]).unwrap_or("")
    }

    /// 流式读某叶的所有候选：逐条回调 f(text, weight, order, boundary)。order=写入时携带的
    /// 全局自然序（v3；无权重时跨编码按词库出现顺序排列）；boundary=音节起始位 bitmask
    /// （v4，见 [`DictEntry::boundary`]，0=无边界信息）。不分配中间 Vec，供全量遍历流式使用。
    fn read_leaf_entries(
        &self,
        v: &DatView,
        leaf_idx: u32,
        f: &mut dyn FnMut(&str, i32, i32, u64),
    ) {
        let (eoff, elen) = self.read_leaf(v, leaf_idx);
        let base = v.entry_off + eoff as usize;
        for i in 0..elen as usize {
            let o = base + i * ENTRY_SIZE;
            if o + ENTRY_SIZE > self.mmap.len() {
                break;
            }
            let text_off = u32::from_le_bytes(self.mmap[o..o + 4].try_into().unwrap());
            let text_len = u16::from_le_bytes(self.mmap[o + 4..o + 6].try_into().unwrap());
            let weight = i32::from_le_bytes(self.mmap[o + 6..o + 10].try_into().unwrap());
            let order = u32::from_le_bytes(self.mmap[o + 10..o + 14].try_into().unwrap());
            let boundary = u64::from_le_bytes(self.mmap[o + 14..o + 22].try_into().unwrap());
            f(
                self.read_string(text_off, text_len),
                weight,
                order as i32,
                boundary,
            );
        }
    }

    /// 读某叶候选到 out（精确/前缀查找用）。
    fn read_entries(&self, v: &DatView, leaf_idx: u32, code: &str, out: &mut Vec<DictEntry>) {
        self.read_leaf_entries(v, leaf_idx, &mut |text, weight, order, boundary| {
            out.push(DictEntry {
                code: code.to_string(),
                text: text.to_string(),
                weight,
                order,
                boundary,
            });
        });
    }

    fn exact(&self, v: &DatView, code: &str) -> Vec<DictEntry> {
        if v.dat_size == 0 {
            return Vec::new();
        }
        let Some(s) = self.walk(v, code) else {
            return Vec::new();
        };
        let Some(leaf) = self.terminal_leaf(v, s) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        self.read_entries(v, leaf, code, &mut out);
        out
    }

    /// 精确查找（全拼/全码）。
    pub fn search(&self, code: &str) -> Vec<DictEntry> {
        self.exact(&self.main, code)
    }

    /// 简拼查找（声母缩写，如 "nh"→你好）：查独立简拼 DAT，按权重降序、截断。
    /// 无简拼区段或未命中返回空。
    pub fn search_abbrev(&self, code: &str, limit: usize) -> Vec<DictEntry> {
        let Some(v) = &self.abbrev else {
            return Vec::new();
        };
        let mut out = self.exact(v, code);
        out.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.order.cmp(&b.order)));
        if limit > 0 {
            out.truncate(limit);
        }
        out
    }

    pub fn has_abbrev(&self) -> bool {
        self.abbrev.is_some()
    }

    /// 是否存在严格长于 `prefix` 的编码（trie 上即：prefix 状态还有非终止后继）。
    ///
    /// 成本 O(max_code) 次数组读，不触碰 LeafTable / StringPool，供「更长后继」这类存在性判据用。
    /// 别改回「`search_prefix` 后 `.any(更长)`」：那要遍历整棵子树，且结果经权重截断与跨层去重
    /// 后会漏判长码。
    pub fn has_longer_code(&self, prefix: &str) -> bool {
        let v = &self.main;
        if v.dat_size == 0 {
            return false;
        }
        let Some(s) = self.walk(v, prefix) else {
            return false;
        };
        // c=0 是终止符（prefix 自身成词），不算「更长」；c>=1 的任一有效转移都意味着
        // 该子树下必有更长编码（trie 每条路径终归通向叶）。
        let b = self.base(v, s);
        (1..=v.max_code).any(|c| {
            let t = b + c;
            Self::in_range(v, t) && self.check(v, t) == s
        })
    }

    /// 沿 `PathNode` 链回溯拼出完整 code。只在真正产出条目时调用（见 `PathNode` 说明）。
    fn build_code(prefix: &str, arena: &[PathNode], mut idx: u32) -> String {
        if idx == u32::MAX {
            return prefix.to_string();
        }
        let mut suffix: Vec<u8> = Vec::new();
        while idx != u32::MAX {
            let n = &arena[idx as usize];
            suffix.push(n.byte);
            idx = n.parent;
        }
        suffix.reverse();
        let mut s = String::with_capacity(prefix.len() + suffix.len());
        s.push_str(prefix);
        s.push_str(std::str::from_utf8(&suffix).unwrap_or(""));
        s
    }

    /// 前缀查找：按权重降序、order 升序取前 `limit` 条。
    ///
    /// 分支限界：借 MaxW 段按上界降序展开状态，「结果已满且出队项上界严格劣于第 `limit` 名」
    /// 即终止，成本随 `limit` 而非子树规模增长。改前先读
    /// `docs/design/prefix-topk-branch-and-bound.md`。两处要害：剪枝判据必须是严格小于
    /// （上界只覆盖 weight，同 weight 的条目仍可能按 order 胜出，放宽成 `<=` 会静默漏结果）；
    /// `NO_MAXW` 子树直接跳过。全遍历参考实现见 [`search_prefix_scan`](Self::search_prefix_scan)。
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<DictEntry> {
        self.search_prefix_stats(prefix, limit).0
    }

    /// 前缀查找，只取音节数不超过 `max_syllables` 且在 `completed_len` 处音节边界对齐的条目
    /// （拼音短输入严格档，判据见 [`crate::cached::prefix_entry_keep`]）。
    ///
    /// 过滤必须下推到这一层：放在调用方时 `limit` 配额会被注定丢弃的条目吃光。过滤与分支限界
    /// 的终止判据正交（上界覆盖子树内全部条目），剪枝依然正确，代价只在合格条目稀疏时扫描量
    /// 上升。另按码长上限剪掉过深子树，见 [`crate::cached::prefix_code_len_cap`]。
    pub fn search_prefix_syllable_capped(
        &self,
        prefix: &str,
        limit: usize,
        max_syllables: u32,
        completed_len: usize,
    ) -> Vec<DictEntry> {
        self.search_prefix_inner(prefix, limit, Some((max_syllables, completed_len)))
            .0
    }

    /// 同 [`search_prefix`](Self::search_prefix)，另返回执行统计（剪枝效果验证用）。
    pub fn search_prefix_stats(
        &self,
        prefix: &str,
        limit: usize,
    ) -> (Vec<DictEntry>, PrefixSearchStats) {
        self.search_prefix_inner(prefix, limit, None)
    }

    /// `search_prefix` 系列的公共实现。`filter = Some((max_syllables, completed_len))` 时按
    /// [`crate::cached::prefix_entry_keep`] 过滤条目（不影响剪枝判据，见
    /// [`search_prefix_syllable_capped`](Self::search_prefix_syllable_capped) 的论证）。
    fn search_prefix_inner(
        &self,
        prefix: &str,
        limit: usize,
        filter: Option<(u32, usize)>,
    ) -> (Vec<DictEntry>, PrefixSearchStats) {
        let mut stats = PrefixSearchStats::default();
        let v = &self.main;
        if v.dat_size == 0 || limit == 0 {
            return (Vec::new(), stats);
        }
        let Some(start) = self.walk(v, prefix) else {
            return (Vec::new(), stats);
        };
        let root_bound = self.maxw(v, start);
        if root_bound == NO_MAXW {
            return (Vec::new(), stats); // 子树无任何条目
        }

        let mut pq: BinaryHeap<Pending> = BinaryHeap::new();
        pq.push(Pending {
            bound: root_bound,
            state: start,
            path: u32::MAX,
            len: prefix.len() as u32,
        });
        let mut arena: Vec<PathNode> = Vec::new();
        let out = self.bnb_collect(v, pq, &mut arena, prefix, limit, filter, &mut stats);
        (out, stats)
    }

    /// 分支限界主循环：从 `pq` 里的起点（可以不止一个）出发按上界降序展开，取前 `limit` 条。
    ///
    /// 各起点的完整 code = `prefix` + 其 `path` 在 `arena` 上回溯出的字节。单起点
    /// （`search_prefix`）传查询前缀、`path = u32::MAX`；多起点（`search_pattern`）传
    /// `prefix = ""`，并把各起点自身的码预先铺进 `arena`——`build_code` 的「单前缀」假设
    /// 由此解除，而无需改它的签名。剪枝判据与正确性论证不变，见 `search_prefix` 文档。
    #[allow(clippy::too_many_arguments)]
    fn bnb_collect(
        &self,
        v: &DatView,
        mut pq: BinaryHeap<Pending>,
        arena: &mut Vec<PathNode>,
        prefix: &str,
        limit: usize,
        filter: Option<(u32, usize)>,
        stats: &mut PrefixSearchStats,
    ) -> Vec<DictEntry> {
        let mut heap: BinaryHeap<Ranked> = BinaryHeap::with_capacity(limit + 1);
        let len_cap = filter.map_or(usize::MAX, |(m, _)| crate::cached::prefix_code_len_cap(m));
        while let Some(node) = pq.pop() {
            // 剪枝（见函数文档要害 1）：严格小于才终止，且是 break 而非 continue——
            // 出队顺序保证剩余各项上界都 <= 本项。
            if heap.len() >= limit
                && let Some(worst) = heap.peek()
                && node.bound < worst.key.weight
            {
                break;
            }
            stats.states_visited += 1;

            // 本状态自身若成词，收集其条目（一码多词）。
            if let Some(leaf) = self.terminal_leaf(v, node.state) {
                let code = Self::build_code(prefix, &arena[..], node.path);
                let mut slot: u16 = 0;
                let entries_read = &mut stats.entries_read;
                self.read_leaf_entries(v, leaf, &mut |text, weight, order, boundary| {
                    let key = RankKey {
                        weight,
                        order,
                        leaf,
                        slot,
                    };
                    slot += 1;
                    *entries_read += 1;
                    // 音节数 + 边界对齐过滤（严格档）：`slot` 已自增、`entries_read` 已计数，
                    // 二者描述的是「读到了第几条」，与是否入选无关，故在此之后才丢弃。
                    if let Some((m, completed_len)) = filter
                        && !crate::cached::prefix_capped_keep(
                            boundary,
                            code.len(),
                            m,
                            completed_len,
                        )
                    {
                        return;
                    }
                    if heap.len() >= limit {
                        // 堆顶是最差的入选者：不严格优于它就丢弃——先比较后分配，
                        // 绝大多数条目走到这里即返回，不触碰 code/text 的 to_string()。
                        match heap.peek() {
                            Some(worst) if key >= worst.key => return,
                            _ => {}
                        }
                        heap.pop();
                    }
                    heap.push(Ranked {
                        key,
                        entry: DictEntry {
                            code: code.clone(),
                            text: text.to_string(),
                            weight,
                            order,
                            boundary,
                        },
                    });
                });
            }

            // 展开子状态（紧凑码 0 是终止符，已由上面的 terminal_leaf 处理）。
            let bs = self.base(v, node.state);
            if bs < 0 {
                continue; // 叶状态无出边
            }
            // 子状态的码都比上限长，整棵子树都过不了过滤（剪枝理由见 `prefix_code_len_cap`）。
            if node.len as usize >= len_cap {
                continue;
            }
            for c in 1..=v.max_code {
                let t = bs + c;
                if !Self::in_range(v, t) || self.check(v, t) != node.state {
                    continue;
                }
                let bound = self.maxw(v, t);
                if bound == NO_MAXW {
                    continue; // 见函数文档要害 2
                }
                let path = arena.len() as u32;
                arena.push(PathNode {
                    parent: node.path,
                    byte: v.rev_map[c as usize],
                });
                pq.push(Pending {
                    bound,
                    state: t,
                    path,
                    len: node.len + 1,
                });
            }
        }
        heap.into_sorted_vec()
            .into_iter()
            .map(|r| r.entry)
            .collect()
    }

    /// 通配查询（`docs/design/codetable-wildcard.md` §5.1）：`pattern` 中等于 `wildcard`
    /// 的字节位匹配恰好一个任意码元，其余位字面匹配。
    ///
    /// 1. 逐位推进前沿：通配位遍历该状态全部有效转移（`1..=max_code`，稀疏，`NO_MAXW`
    ///    子树直接跳过），字面位走一次转移；
    /// 2. 等长档：前沿各状态的终止叶按 `RankKey` 取前 `limit`；
    /// 3. 更长档（`with_prefix`）：余额 `limit - 等长条数` 交给 [`Self::bnb_collect`]，
    ///    以全部前沿状态的子节点为多起点入队。
    ///
    /// 两档分开取额的理由见 [`crate::layer::DictLayer::search_pattern`]。首位即通配时
    /// 前沿从根展开，靠 `limit` 与分支限界兜底。`pattern` / `wildcard` 须为 ASCII，否则返回空。
    pub fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        limit: usize,
        with_prefix: bool,
    ) -> Vec<DictEntry> {
        let v = &self.main;
        if v.dat_size == 0
            || limit == 0
            || pattern.is_empty()
            || !pattern.is_ascii()
            || !wildcard.is_ascii()
        {
            return Vec::new();
        }
        let wb = wildcard as u8;
        let mut arena: Vec<PathNode> = Vec::new();
        // 前沿：(状态, 该状态完整码在 arena 上的末节点)。根的码为空，记 u32::MAX。
        let mut frontier: Vec<(i32, u32)> = vec![(0, u32::MAX)];
        for &pb in pattern.as_bytes() {
            let mut next: Vec<(i32, u32)> = Vec::new();
            for &(s, path) in &frontier {
                let bs = self.base(v, s);
                if bs < 0 {
                    continue; // 叶状态无出边
                }
                if pb == wb {
                    for c in 1..=v.max_code {
                        let t = bs + c;
                        if !Self::in_range(v, t)
                            || self.check(v, t) != s
                            || self.maxw(v, t) == NO_MAXW
                        {
                            continue;
                        }
                        arena.push(PathNode {
                            parent: path,
                            byte: v.rev_map[c as usize],
                        });
                        next.push((t, arena.len() as u32 - 1));
                    }
                } else {
                    let c = v.char_map[pb as usize];
                    if c < 0 {
                        continue; // 该字节不在本词库的码元里（同 `walk`）
                    }
                    let t = bs + c;
                    if !Self::in_range(v, t) || self.check(v, t) != s {
                        continue;
                    }
                    arena.push(PathNode {
                        parent: path,
                        byte: pb,
                    });
                    next.push((t, arena.len() as u32 - 1));
                }
            }
            if next.is_empty() {
                return Vec::new();
            }
            frontier = next;
        }

        // 等长档。
        let mut exact: BinaryHeap<Ranked> = BinaryHeap::with_capacity(limit + 1);
        for &(s, path) in &frontier {
            let Some(leaf) = self.terminal_leaf(v, s) else {
                continue;
            };
            let code = Self::build_code("", &arena[..], path);
            let mut slot: u16 = 0;
            self.read_leaf_entries(v, leaf, &mut |text, weight, order, boundary| {
                let key = RankKey {
                    weight,
                    order,
                    leaf,
                    slot,
                };
                slot += 1;
                if exact.len() >= limit {
                    match exact.peek() {
                        Some(worst) if key >= worst.key => return,
                        _ => {}
                    }
                    exact.pop();
                }
                exact.push(Ranked {
                    key,
                    entry: DictEntry {
                        code: code.clone(),
                        text: text.to_string(),
                        weight,
                        order,
                        boundary,
                    },
                });
            });
        }
        let mut out: Vec<DictEntry> = exact
            .into_sorted_vec()
            .into_iter()
            .map(|r| r.entry)
            .collect();
        let rest = limit - out.len();
        if !with_prefix || rest == 0 {
            return out;
        }

        // 更长档：前沿各状态的子节点（紧凑码 ≥1）作多起点。
        let mut pq: BinaryHeap<Pending> = BinaryHeap::new();
        for &(s, path) in &frontier {
            let bs = self.base(v, s);
            if bs < 0 {
                continue;
            }
            for c in 1..=v.max_code {
                let t = bs + c;
                if !Self::in_range(v, t) || self.check(v, t) != s {
                    continue;
                }
                let bound = self.maxw(v, t);
                if bound == NO_MAXW {
                    continue;
                }
                arena.push(PathNode {
                    parent: path,
                    byte: v.rev_map[c as usize],
                });
                pq.push(Pending {
                    bound,
                    state: t,
                    path: arena.len() as u32 - 1,
                    len: pattern.len() as u32 + 1,
                });
            }
        }
        let mut stats = PrefixSearchStats::default();
        out.extend(self.bnb_collect(v, pq, &mut arena, "", rest, None, &mut stats));
        out
    }

    /// 前缀查找的全遍历参考实现：DFS 整棵子树 + top-N 堆。
    ///
    /// 只为对拍：[`search_prefix`](Self::search_prefix) 的结果必须与它逐条一致
    /// （`bnb_matches_full_scan`）。生产路径不应调用，成本随子树规模增长。
    pub fn search_prefix_scan(&self, prefix: &str, limit: usize) -> Vec<DictEntry> {
        let v = &self.main;
        if v.dat_size == 0 || limit == 0 {
            return Vec::new();
        }
        let Some(start) = self.walk(v, prefix) else {
            return Vec::new();
        };
        let mut heap: BinaryHeap<Ranked> = BinaryHeap::with_capacity(limit + 1);
        let mut path: Vec<u8> = prefix.as_bytes().to_vec();
        self.for_each_leaf(v, start, &mut path, &mut |code, leaf| {
            let mut slot: u16 = 0;
            self.read_leaf_entries(v, leaf, &mut |text, weight, order, boundary| {
                let key = RankKey {
                    weight,
                    order,
                    leaf,
                    slot,
                };
                slot += 1;
                if heap.len() >= limit {
                    match heap.peek() {
                        Some(worst) if key >= worst.key => return,
                        _ => {}
                    }
                    heap.pop();
                }
                heap.push(Ranked {
                    key,
                    entry: DictEntry {
                        code: code.to_string(),
                        text: text.to_string(),
                        weight,
                        order,
                        boundary,
                    },
                });
            });
        });
        heap.into_sorted_vec()
            .into_iter()
            .map(|r| r.entry)
            .collect()
    }

    /// 主表全部 code 的有序游标（字节序递增，短码在长码前），每项 `(code, leaf)`；
    /// 配合 [`Self::leaf_entries`] 读候选。供多段 k 路归并用：拉式迭代，不像
    /// [`Self::for_each_entry`] 那样要把整段推完。
    pub fn codes(&self) -> CodeCursor<'_> {
        CodeCursor {
            r: self,
            stack: if self.main.dat_size == 0 {
                Vec::new()
            } else {
                vec![(0, 0, 1)]
            },
            path: Vec::new(),
            root_pending: self.main.dat_size != 0,
        }
    }

    /// 主表里 `code` 的叶号（[`Self::codes`] 给出的同一种）；码不在表里给 `None`。
    pub fn leaf_of(&self, code: &str) -> Option<u32> {
        let v = &self.main;
        if v.dat_size == 0 {
            return None;
        }
        self.terminal_leaf(v, self.walk(v, code)?)
    }

    /// 某叶首条在主表条目区里的序号。条目区按喂入顺序连续排列（[`WdatStreamWriter::push_code`]），
    /// 所以这就是该叶首条的全局序号，调用方不必自己另建「码 → 序号」表。
    pub fn leaf_first_entry(&self, leaf: u32) -> u32 {
        self.read_leaf(&self.main, leaf).0 / ENTRY_SIZE as u32
    }

    /// 某叶的全部候选 `(text, weight, order, boundary)`，text 直接借 mmap。
    pub fn leaf_entries(&self, leaf: u32) -> Vec<(&str, i32, u32, u64)> {
        let mut out = Vec::new();
        self.for_each_leaf_entry(leaf, &mut |t, w, o, b| out.push((t, w, o, b)));
        out
    }

    /// [`Self::leaf_entries`] 的回调版：全表扫描时逐叶分配一个 Vec 的开销可观。
    pub fn for_each_leaf_entry<'a>(&'a self, leaf: u32, f: &mut dyn FnMut(&'a str, i32, u32, u64)) {
        let v = &self.main;
        let (eoff, elen) = self.read_leaf(v, leaf);
        let base = v.entry_off + eoff as usize;
        for i in 0..elen as usize {
            let o = base + i * ENTRY_SIZE;
            if o + ENTRY_SIZE > self.mmap.len() {
                break;
            }
            let text_off = u32::from_le_bytes(self.mmap[o..o + 4].try_into().unwrap());
            let text_len = u16::from_le_bytes(self.mmap[o + 4..o + 6].try_into().unwrap());
            let weight = i32::from_le_bytes(self.mmap[o + 6..o + 10].try_into().unwrap());
            let order = u32::from_le_bytes(self.mmap[o + 10..o + 14].try_into().unwrap());
            let boundary = u64::from_le_bytes(self.mmap[o + 14..o + 22].try_into().unwrap());
            f(
                self.read_string(text_off, text_len),
                weight,
                order,
                boundary,
            );
        }
    }

    /// 遍历全部条目（供反查索引构建）：DFS 全树流式回调 (code,text,weight)，不累积全量 Vec。
    pub fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        if self.main.dat_size == 0 {
            return;
        }
        let v = &self.main;
        let mut path: Vec<u8> = Vec::new();
        self.for_each_leaf(v, 0, &mut path, &mut |code, leaf| {
            self.read_leaf_entries(v, leaf, &mut |text, weight, _order, _boundary| {
                f(code, text, weight);
            });
        });
    }

    /// DFS 子树：对每个叶流式调用 on_leaf(完整code, leaf_idx)，不累积候选。
    /// 用显式栈避免深递归；path 随进出栈增删。
    fn for_each_leaf(
        &self,
        v: &DatView,
        start: i32,
        path: &mut Vec<u8>,
        on_leaf: &mut dyn FnMut(&str, u32),
    ) {
        let mut stack: Vec<(i32, usize, i32)> = vec![(start, path.len(), 1)];
        if let Some(leaf) = self.terminal_leaf(v, start) {
            on_leaf(std::str::from_utf8(path).unwrap_or(""), leaf);
        }
        while let Some(&mut (s, plen, ref mut next_c)) = stack.last_mut() {
            path.truncate(plen);
            let mut descended = false;
            while *next_c <= v.max_code {
                let c = *next_c;
                *next_c += 1;
                let t = self.base(v, s) + c;
                if !Self::in_range(v, t) || self.check(v, t) != s {
                    continue;
                }
                path.push(v.rev_map[c as usize]);
                if let Some(leaf) = self.terminal_leaf(v, t) {
                    on_leaf(std::str::from_utf8(path).unwrap_or(""), leaf);
                }
                stack.push((t, path.len(), 1));
                descended = true;
                break;
            }
            if !descended {
                stack.pop();
            }
        }
    }
}

#[cfg(test)]
#[path = "datformat_tests.rs"]
mod tests;
