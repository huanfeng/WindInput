//! DAT 格式 (wdat) — Double-Array Trie 词典
//!
//! 与 Go 版 `wind_input/internal/dict/datformat/` 对齐（主 DAT；暂不含简拼 AbbrevSection，
//! 与现 wdb 内容对等——全拼/全码）。前缀查询为「数组跳转 + 子树 DFS」，较 wdb 的键索引
//! 二分更省更快，适合拼音大词库逐键前缀检索。
//!
//! 文件布局（小端）：
//! ```text
//! [Header 48B]
//! [DAT Base: dat_size*4][DAT Check: dat_size*4][DAT MaxW: dat_size*4]
//! [LeafTable: leaf_count*8]   每条 {entry_off u32, entry_len u16, _ u16}
//! [EntryRecords: entry_count*22]  每条 {text_off u32, text_len u16, weight i32, order u32, boundary u64}
//! [StringPool]
//! [CharMap 1028B]  {max_code i32, char_map[256] i32}
//! [Meta(可选) 4B len + bytes]
//! ```
//! DAT 查询：`base[s]+c=t`（状态 s 经紧凑码 c 转移到 t），`check[t]==s` 校验；
//! `base[t]<0` 表叶节点，`-base[t]-1` 为 LeafTable 索引；`c=0` 为终止符。
//!
//! **MaxW 段**（v6）：`maxw[s]` = 以状态 s 为根的子树内所有条目 weight 的最大值，
//! `NO_MAXW`（= i32::MIN）表示子树无条目。它是前缀 Top-K 查询的**剪枝上界**，
//! 使查询成本随 K 而非随子树规模 M 增长（见 `search_prefix`）。
//! 段位置紧跟 Check 之后，无需 Header 新字段——`maxw_off = dat_off + dat_size*4*2` 可直接算出，
//! 其后各段偏移（leaf/entry/str）本就在 Header 中显式存储。

use crate::binformat::DictEntry;
use memmap2::Mmap;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::Path;
use tracing::info;

const MAGIC: [u8; 4] = *b"WDAT";
// v3：EntryRecord 增加 order u32（全局自然序，10→14B），跨编码等权时按词库出现顺序排序，
// 不再退化为叶内序号致编码字母序。旧 v2 缓存 mtime/指纹不匹配自动重建。
// v4：EntryRecord 增加 boundary u64（音节起始位 bitmask，14→22B）。源数据 rime `ni hao` 的
// 空格本就是音节真值边界，此前在解析期被 replace(' ',"") 丢弃，迫使查询侧用 DAG 重新猜切分
// （xi'an vs xian 无从分辨）、造词侧靠 410 音节暴力反推。key 仍为扁平串，故 DAT/前缀查询
// 语义完全不变——边界只作为 entry 侧元数据随查询结果返回。旧缓存同样靠指纹不匹配自动重建。
// v5：AbbrevSection 的条目文本从「词」改为「全拼码」——二级索引指向主键而非复制数据。
// 简拼查询因此变成「查索引拿码 → 走主表装配候选」，候选得到真实的 code 与 boundary，
// 词频不再因简拼/全拼分裂成两份计数。**二进制结构完全未变**，变的是那个字符串字段的
// 语义，故必须 bump：旧 v4 缓存若按新逻辑读，会把词当成码拿去查主表、简拼全数落空。
// 旧缓存靠内容指纹不匹配自动重建，无迁移代码。
// v6：新增 MaxW 段（每状态 4B，子树最大 weight），前缀 Top-K 查询改为分支限界。
// 旧 v5 缓存缺该段，且 leaf/entry 偏移整体前移，按新布局读必然错位，故必须 bump。
// 旧缓存靠版本不匹配自动重建，无迁移代码。
const VERSION: u32 = 6;
const HEADER_SIZE: usize = 48;
const LEAF_SIZE: usize = 8;
const ENTRY_SIZE: usize = 22;
const CHARMAP_SIZE: usize = 4 + 256 * 4; // 1028

/// `maxw[s]` 的空子树哨兵。**刻意不复用 `i32::MIN` 之外的值**：weight 为 i32，
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

/// 从**已按字典序排好、唯一**的编码列表**直接构建 DAT**——不建中间 trie，峰值内存仅 base/check。
/// `data_index` = 编码在列表中的下标（= LeafTable 索引）。这是相对「先建几百万小节点 trie 再转双数组」
/// 的关键省内存改造：trie 那几百万个小分配会把堆撑高、碎片化、不还给系统（低配设备 OOM 风险）。
///
/// 原理：一个 trie 节点 ⇔ 一段**共享前缀**的连续编码区间 `[lo,hi)`（前缀 = codes[lo][..depth]）。
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

    // 2) base/check 初始化（check=-1 表空闲），root 占位 0；空闲位置(1..)串成链表，
    //    见 FreeList 顶部说明——这是解决大规模 key 集合构建退化为平方级耗时的关键。
    let mut base = vec![0i32; 256];
    let mut check = vec![-1i32; 256];
    check[0] = 0;
    let mut free = FreeList::new(256);

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
            let bv = find_base(&child_codes, &mut base, &mut check, &mut free);
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
/// **它解决的问题**：旧实现里 `find_base` 从一个整数游标逐个探测候选 base 值，游标只在
/// "已确认占用"的前缀上推进——一次探测中途放弃、试过但没用上的大段位置不会被记住，
/// 下一次调用又要把同一片已知大概率冲突的区间从头探一遍。在大规模（百万级 key）、编码
/// 集中在小字母表（如本项目 a-z）的码表下，这片"重复无效探测"随构建推进线性变长，
/// 总代价退化为 O(状态数²)——真实词库（146 万 key）上实测数分钟不收敛、内存伴随大量
/// "探测失败也触发扩容"的调用被撑到 GB 级。
///
/// 改为只在**当前确实空闲**的位置间跳转，代价与空闲位置数成正比、不受历史已占用区间
/// 影响，是双数组树构建的标准解法（darts-clone / cedar 等实现均采用同类机制）。
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
        // `b` 必须 ≥1（不能只满足 `b+min_code>=1`）：`base[s]<0` 是整个格式用来判定
        // "s 是叶子"的符号约定（见 compute_maxw 与读取侧多处），只有真正的叶子节点会被
        // 显式写入负的 `-(leaf)-1`；任何**非叶状态**（走到这里、正在为其子节点找 base 的
        // 节点）的 base 若意外为负，会被这些判定误当成叶子，其整棵子树从此在自顶向下的
        // 遍历里"消失"（点查询仍能用，因为它按字节走 check[] 链、不看符号——这正是本 bug
        // 曾经的隐蔽之处：exact match 全绿，只有 MaxW/前缀分支限界会跳过整棵子树）。
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

/// 字符串池（去重）。
struct StringPool {
    buf: Vec<u8>,
    index: std::collections::HashMap<String, u32>,
}

impl StringPool {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            index: std::collections::HashMap::new(),
        }
    }
    fn add(&mut self, s: &str) -> u32 {
        if let Some(&off) = self.index.get(s) {
            return off;
        }
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(s.as_bytes());
        self.index.insert(s.to_string(), off);
        off
    }
}

/// LeafTable 一行：`(该码的条目区字节偏移, 条目数)`。
type DatLeafRow = (u32, u16);

/// EntryTable 一行：`(文本在共享池的偏移, 文本字节长, weight, natural_order, 音节边界位图)`。
type DatEntryRow = (u32, u16, i32, u32, u64);

/// 从排序后的 (code,entries) 构建一段独立 DAT：返回 (DAT, leaves, entries)，文本入共享池。
/// 主表与简拼表各调一次（共用同一 StringPool 去重）。
fn build_section(
    sorted: &[&(String, Vec<WriteEntry>)],
    pool: &mut StringPool,
) -> (Dat, Vec<DatLeafRow>, Vec<DatEntryRow>) {
    let mut leaves: Vec<DatLeafRow> = Vec::with_capacity(sorted.len());
    let mut entries: Vec<DatEntryRow> = Vec::new();
    let mut codes: Vec<&str> = Vec::with_capacity(sorted.len());
    let mut entry_byte_off = 0u32;
    for kv in sorted {
        let (code, ents) = (&kv.0, &kv.1);
        codes.push(code.as_str());
        leaves.push((entry_byte_off, ents.len() as u16));
        for (text, weight, order, boundary) in ents {
            let text_off = pool.add(text);
            entries.push((text_off, text.len() as u16, *weight, *order, *boundary));
        }
        entry_byte_off += (ents.len() * ENTRY_SIZE) as u32;
    }
    (build_dat_from_sorted(&codes), leaves, entries)
}

/// 计算 MaxW 段（v6）：`maxw[s]` = 以状态 s 为根的子树中所有条目 weight 的最大值。
///
/// 这是前缀 Top-K 查询的剪枝上界，须满足**不变量**：
/// 对任意状态 s 与任意 `e ∈ subtree(s)`，有 `weight(e) <= maxw[s]`。
/// 上界只需保守（偏大无害、偏小会漏结果），故任何"取 max"的实现错误方向都是危险的，
/// 单元测试须逐状态对拍暴力计算的结果。
///
/// **实现为两趟而非递归**：DAT 深度等于最长编码长度，用户词库可能出现极长编码，
/// 递归有栈溢出风险。第一趟前序 DFS 收集访问序（父必先于子出现），第二趟逆序回填
/// （于是子必先于父被算出），等价于后序遍历且无递归。
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

/// wdat 写入器：与 binformat::DictWriter 同样接口（add(code, entries)），输出 DAT 格式。
/// `add_abbrev` 追加简拼（声母缩写）表，写入独立 AbbrevSection（与全拼查询互不污染）。
/// 写入侧的一条候选：`(text, weight, order, boundary)`。
/// boundary 见 [`DictEntry::boundary`]（音节起始位 bitmask，0=无边界信息）。
type WriteEntry = (String, i32, u32, u64);

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
    /// 需要**跨编码全局自然序**（无权重按词库出现顺序）时改用 [`add_with_order`]。
    pub fn add(&mut self, code: String, entries: Vec<(String, i32)>) {
        if !entries.is_empty() {
            self.keys.push((code, with_local_order(entries)));
        }
    }

    /// 追加一个 code 的候选，携带**显式全局 order**（`(text, weight, order)`）。
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

    /// 追加一个 code 的候选，携带 order **与音节边界**（`(text, weight, order, boundary)`）。
    /// 供拼音词典构建路径使用——边界取自 rime 源数据 `ni hao` 的空格（真值，非 DAG 猜测）。
    /// boundary 语义见 [`DictEntry::boundary`]。
    pub fn add_with_boundary(&mut self, code: String, entries: Vec<WriteEntry>) {
        if !entries.is_empty() {
            self.keys.push((code, entries));
        }
    }

    /// 追加简拼条目：`abbrev`=声母序列（`nh`），`entries` 每条 `(全拼码, weight)`
    /// —— **存的是码不是词**（v5，见文件头版本说明）。空条目忽略；order 按叶内序号补全。
    ///
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

    /// 原子写 .wdat（tmp+pid+seq → rename，与 binformat 一致，仅防读到半文件）。
    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        use std::io::Write;
        let path = path.as_ref();

        // 按 code 排序（确定性 + DAT key 唯一）。排序**引用**而非克隆全量数据，省一份大拷贝。
        let mut sorted: Vec<&(String, Vec<WriteEntry>)> = self.keys.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let mut sorted_ab: Vec<&(String, Vec<WriteEntry>)> = self.abbrevs.iter().collect();
        sorted_ab.sort_by(|a, b| a.0.cmp(&b.0));
        let has_abbrev = !sorted_ab.is_empty();

        // 共享字符串池：主表先入（简拼候选多与主表 text 重复 → 去重复用偏移）。
        let mut pool = StringPool::new();
        let (dat, leaves, entries) = build_section(&sorted, &mut pool);
        let (a_dat, a_leaves, a_entries) = if has_abbrev {
            let (d, l, e) = build_section(&sorted_ab, &mut pool);
            (Some(d), l, e)
        } else {
            (None, Vec::new(), Vec::new())
        };

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
        let after_pool = str_off + pool.buf.len() as u32;

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
        let meta_off = match &self.meta {
            Some(m) if !m.is_empty() => char_map_off + CHARMAP_SIZE as u32,
            _ => 0,
        };

        // 原子写。
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
        f.write_all(&dat_size.to_le_bytes())?;
        f.write_all(&(leaves.len() as u32).to_le_bytes())?;
        f.write_all(&dat_off.to_le_bytes())?;
        f.write_all(&leaf_off.to_le_bytes())?;
        f.write_all(&entry_off.to_le_bytes())?;
        f.write_all(&str_off.to_le_bytes())?;
        f.write_all(&abbrev_off.to_le_bytes())?;
        f.write_all(&meta_off.to_le_bytes())?;
        f.write_all(&(entries.len() as u32).to_le_bytes())?;
        f.write_all(&char_map_off.to_le_bytes())?;

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
        write_dat_section(&mut f, &dat, &maxw, &leaves, &entries)?;
        f.write_all(&pool.buf)?;

        // 简拼区段：自描述头 + DAT/leaf/entry + 简拼 CharMap。
        if let Some(ad) = &a_dat {
            f.write_all(&(ad.base.len() as u32).to_le_bytes())?;
            f.write_all(&(a_leaves.len() as u32).to_le_bytes())?;
            f.write_all(&a_dat_off.to_le_bytes())?;
            f.write_all(&a_leaf_off.to_le_bytes())?;
            f.write_all(&a_entry_off.to_le_bytes())?;
            f.write_all(&a_charmap_off.to_le_bytes())?;
            write_dat_section(&mut f, ad, &a_maxw, &a_leaves, &a_entries)?;
            write_charmap(&mut f, ad)?;
        }

        // 主 CharMap。
        write_charmap(&mut f, &dat)?;

        // Meta。
        if let Some(m) = &self.meta
            && !m.is_empty()
        {
            f.write_all(&(m.len() as u32).to_le_bytes())?;
            f.write_all(m)?;
        }

        f.flush()?;
        drop(f);
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        info!(
            "Wrote wdat: {} keys, {} abbrevs, {} entries, dat_size={}",
            leaves.len(),
            a_leaves.len(),
            entries.len(),
            dat_size
        );
        Ok(())
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

/// 前缀查询的排序键。`Ord` 定义为「**越差越大**」，故堆顶恒为当前最差的入选者，
/// 且 `into_sorted_vec()`（升序）直接给出最优→最差的最终顺序。
///
/// 三、四级 tie-break 取 `(leaf, slot)`（LeafTable 索引 + 叶内序号）而**不是遍历序**。
/// 这一点是必须的：`with_local_order` 写入的词库（`export_to_writer` / combined 合并路径）
/// order 是「code 内序号 0,1,2」，等权时会大量打平；若拿遍历序做 tie-break，DFS 与分支限界
/// 两种遍历顺序会给出不同结果——既无法对拍验证，也会让候选顺序随实现改动而抖动。
/// `leaf` 是构建期按 code 字典序分配的稳定编号，任何遍历顺序下答案唯一。
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

/// 路径链节点：`(父节点索引, 该步字节)`。
///
/// **入队时不拼 String**：入队状态数可能远大于最终产出条目数，且绝大多数入队项会被剪枝
/// 丢弃，给它们各分配一个 code 字符串是纯浪费。只在真正要产出条目时才回溯拼接。
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

pub struct WdatReader {
    mmap: Mmap,
    /// 本 reader 映射的文件路径。
    ///
    /// 不是给日志用的：**二级缓存要靠它认出「我是从哪几个文件派生出来的」**
    /// （见 `reverseidx` 的 `.wridx` 指纹）。自己在调用方那边重新推导一遍那些路径，
    /// 就是本仓反复出现的「同一份推导写两处、其中一处悄悄过时」。
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
        let mmap = unsafe { Mmap::map(&file)? };
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

    /// 是否存在**严格长于** `prefix` 的编码（trie 上即：prefix 状态还有非终止后继）。
    ///
    /// 成本 O(max_code)（≤ 出现过的字节种数，码表约 26）次数组读，**不触碰 LeafTable /
    /// StringPool**。这是给「更长后继」这类**存在性**判据用的——此前调用方一律走
    /// `search_prefix(input, 64)` 再 `.any(code 更长)`，等于为一个 bool 遍历整棵子树，
    /// 在 `ok` 拼字这类单前缀 8.8 万条的词库上单次即 20ms 级。
    ///
    /// 注意语义比「search_prefix 后 any」**更严格也更正确**：后者的结果先经权重排序截断
    /// （长码候选权重低时会被挤出而漏判），跨层合并时还会因「同 text 取最短码」抹掉长码；
    /// 本函数直接问 trie，不受二者影响。
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

    /// 前缀查找：按权重降序、order 升序取前 `limit` 条（与 binformat::DictReader 对齐）。
    ///
    /// **v6 起为分支限界（branch and bound）**，成本随 `limit` 而非随子树规模增长。
    /// 借助 v6 的 MaxW 段（`maxw[s]` = 子树内最大 weight）按上界降序展开状态，一旦
    /// 「结果已满 且 当前出队项的上界严格劣于第 `limit` 名」即可终止——此时优先队列中
    /// 剩余各项的上界都不高于它，其子树内条目全部不可能入选。
    ///
    /// 改此实现前请先读 `docs/design/prefix-topk-branch-and-bound.md` 的正确性论证。两处要害：
    ///
    /// 1. **剪枝判据必须是严格小于**。上界只覆盖 RankKey 的首要键 weight；`bound == 第 limit 名
    ///    的 weight` 时，子树内仍可能有同 weight 而 order 更小的条目应当取代它。放宽成 `<=`
    ///    会静默漏结果——没有崩溃、没有日志，只是候选少了几条。
    /// 2. **`NO_MAXW` 子树直接跳过**：空子树无条目可贡献，入队只是浪费。
    ///
    /// 旧的全遍历实现保留为 [`search_prefix_scan`](Self::search_prefix_scan)，仅供对拍验证。
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<DictEntry> {
        self.search_prefix_stats(prefix, limit).0
    }

    /// 前缀查找，只取**音节数不超过 `max_syllables`** 且**在 `completed_len` 处音节边界
    /// 对齐**的条目（拼音短输入严格档，判据见 [`crate::cached::prefix_entry_keep`]）。
    ///
    /// ## 为什么必须下推到这一层，而不是查完再过滤
    ///
    /// 过滤放在调用方时，`limit` 配额会被**注定要丢弃**的条目吃光：实测单字母 `d` 取
    /// 1000 条补全，过闸门的只有 68 条单字，其余 932 条白占名额 —— 且把上限提到 5000
    /// 也没用（`MAX_COMPLETION_CANDIDATES` clamp 在 1000），只是让 `push_unique` 的
    /// O(n²) 查重陪跑。下推之后 top-N 直接就是 N 条合格条目。
    ///
    /// ## 剪枝仍然正确
    ///
    /// 过滤与分支限界的终止判据**正交**：`node.bound` 是子树内最大 weight 的上界，
    /// `bound < worst.key.weight` 意味着该子树内**所有**条目（合格与否）都劣于当前第
    /// `limit` 名，故合格条目也不可能入选，`break` 依然安全。代价在效率而非正确性 ——
    /// 合格条目稀疏时堆更晚填满、剪枝更晚生效，扫描量上升（这正是找齐 N 条所必需的）。
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

        let mut heap: BinaryHeap<Ranked> = BinaryHeap::with_capacity(limit + 1);
        let mut pq: BinaryHeap<Pending> = BinaryHeap::new();
        let mut arena: Vec<PathNode> = Vec::new();
        pq.push(Pending {
            bound: root_bound,
            state: start,
            path: u32::MAX,
        });

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
                let code = Self::build_code(prefix, &arena, node.path);
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
                        && !crate::cached::prefix_entry_keep(boundary, code.len(), m, completed_len)
                    {
                        return;
                    }
                    if heap.len() >= limit {
                        // 堆顶是最差的入选者：不严格优于它就丢弃——**先比较后分配**，
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
                });
            }
        }
        let out = heap
            .into_sorted_vec()
            .into_iter()
            .map(|r| r.entry)
            .collect();
        (out, stats)
    }

    /// 前缀查找的**全遍历参考实现**（v6 之前的行为）：DFS 整棵子树 + top-N 堆。
    ///
    /// 保留它只为验证：[`search_prefix`](Self::search_prefix) 的分支限界结果必须与本函数
    /// **逐条一致**（测试 `bnb_matches_full_scan`）。剪枝一旦写错就是静默漏候选，
    /// 唯有对拍能可靠地发现。
    ///
    /// 生产路径不应调用——它的成本随子树规模而非 `limit` 增长。
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

    /// 遍历全部条目（供反查索引构建）：DFS 全树**流式**回调 (code,text,weight)，
    /// 不累积全量 Vec——避免在大词库反查时堆起数十万 DictEntry（私有堆峰值/碎片）。
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

    /// DFS 子树：对每个叶**流式**调用 on_leaf(完整code, leaf_idx)，不累积候选。
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
mod tests {
    use super::*;

    fn build(tmp_name: &str, data: &[(&str, &[(&str, i32)])]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(tmp_name);
        let mut w = WdatWriter::new();
        for (code, ents) in data {
            let v: Vec<(String, i32)> = ents.iter().map(|(t, wt)| (t.to_string(), *wt)).collect();
            w.add(code.to_string(), v);
        }
        w.write(&p).expect("write wdat");
        p
    }

    /// 按 (code, entries) 构建 wdat（owned 版，供批量生成用例）。走 `WdatWriter::add`
    /// → `with_local_order`，即 order = code 内序号——与 `export_to_writer` / combined
    /// 合并路径同语义，单条 code 时 order 恒 0（等权即全打平，最坏 tie-break 场景）。
    fn build_owned(tmp_name: &str, data: &[(String, Vec<(String, i32)>)]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(tmp_name);
        let mut w = WdatWriter::new();
        for (code, ents) in data {
            w.add(code.clone(), ents.clone());
        }
        w.write(&p).expect("write wdat");
        p
    }

    /// 参考实现 = 改造前的「DFS 全量收集 → 稳定排序 → 截断」。作为差分对比的黄金标准：
    /// top-N 堆的输出必须与它**逐条相同**（含顺序），否则即为正确性回归。
    fn reference_prefix(r: &WdatReader, prefix: &str, limit: usize) -> Vec<DictEntry> {
        let v = &r.main;
        let Some(start) = r.walk(v, prefix) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut path: Vec<u8> = prefix.as_bytes().to_vec();
        r.for_each_leaf(v, start, &mut path, &mut |code, leaf| {
            r.read_leaf_entries(v, leaf, &mut |text, weight, order, boundary| {
                out.push(DictEntry {
                    code: code.to_string(),
                    text: text.to_string(),
                    weight,
                    order,
                    boundary,
                });
            });
        });
        out.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.order.cmp(&b.order)));
        out.truncate(limit);
        out
    }

    fn assert_same(actual: &[DictEntry], expect: &[DictEntry], ctx: &str) {
        let a: Vec<_> = actual
            .iter()
            .map(|e| (&e.code, &e.text, e.weight))
            .collect();
        let b: Vec<_> = expect
            .iter()
            .map(|e| (&e.code, &e.text, e.weight))
            .collect();
        assert_eq!(a, b, "{ctx}");
    }

    /// top-N 堆 == 全量排序取前 N，在**权重与 order 全部打平**时也成立。
    ///
    /// 这是 `ok` 拼字词库的真实形状：88020 条无 weight 列（全取 default_weight），
    /// 每 code 单条（order 恒 0）。此时两级排序键完全失效，顺序只由稳定排序的
    /// 「保持 DFS 序」兜底——正是 top-N 堆最容易与全排序分歧之处（堆本身不稳定，
    /// 靠 `RankKey::seq` 复现）。
    #[test]
    fn topn_prefix_matches_full_sort_when_all_keys_tie() {
        let data: Vec<(String, Vec<(String, i32)>)> = (0..500u32)
            .map(|i| {
                let c1 = (b'a' + (i / 26) as u8) as char;
                let c2 = (b'a' + (i % 26) as u8) as char;
                (format!("ok{c1}{c2}"), vec![(format!("字{i}"), 0)])
            })
            .collect();
        let p = build_owned("wdat_topn_all_tie.wdat", &data);
        let r = WdatReader::open(&p).unwrap();

        for limit in [1usize, 5, 90, 499, 500, 501, 1000] {
            assert_same(
                &r.search_prefix("ok", limit),
                &reference_prefix(&r, "ok", limit),
                &format!("全打平 limit={limit}"),
            );
        }
        let _ = std::fs::remove_file(&p);
    }

    /// top-N 堆 == 全量排序取前 N，混合权重 + 同 code 多条（order 0,1,2）时也成立。
    #[test]
    fn topn_prefix_matches_full_sort_mixed_weights() {
        let data: Vec<(String, Vec<(String, i32)>)> = (0..200u32)
            .map(|i| {
                let c1 = (b'a' + (i / 26) as u8) as char;
                let c2 = (b'a' + (i % 26) as u8) as char;
                // 权重故意大量重复（i % 7），并让部分 code 带 2 条（order 0/1）。
                let mut ents = vec![(format!("甲{i}"), (i % 7) as i32 * 100)];
                if i % 3 == 0 {
                    ents.push((format!("乙{i}"), (i % 7) as i32 * 100));
                }
                (format!("ok{c1}{c2}"), ents)
            })
            .collect();
        let p = build_owned("wdat_topn_mixed.wdat", &data);
        let r = WdatReader::open(&p).unwrap();

        for limit in [1usize, 3, 50, 200, 999] {
            assert_same(
                &r.search_prefix("ok", limit),
                &reference_prefix(&r, "ok", limit),
                &format!("混合权重 limit={limit}"),
            );
        }
        let _ = std::fs::remove_file(&p);
    }

    /// **剪枝确实发生**：权重有区分度时，小 limit 的访问状态数须远低于全遍历。
    ///
    /// ⚠️ 上面两个对拍测试证明不了这件事。`..._all_keys_tie` 里 weight 全为 0，剪枝判据
    /// `bound < worst.weight` 即 `0 < 0` 恒假，分支限界**退化成全遍历**，结果自然一致——
    /// 剪枝逻辑若被误删或写成恒不触发，那个测试照样全绿。本测试直接钉 `states_visited`，
    /// 是「剪枝真的执行了」唯一的证据。
    #[test]
    fn bnb_actually_prunes_when_weights_differ() {
        let data: Vec<(String, Vec<(String, i32)>)> = (0..676u32)
            .map(|i| {
                let c1 = (b'a' + (i / 26) as u8) as char;
                let c2 = (b'a' + (i % 26) as u8) as char;
                // 伪随机但确定的权重，让高权重分散到不同子树（逼近真实词库形状）。
                let w = ((i * 7919) % 10000) as i32;
                (format!("ok{c1}{c2}"), vec![(format!("字{i}"), w)])
            })
            .collect();
        let p = build_owned("wdat_bnb_prune.wdat", &data);
        let r = WdatReader::open(&p).unwrap();

        let (_, full) = r.search_prefix_stats("ok", 100_000);
        let (_, few) = r.search_prefix_stats("ok", 5);
        assert!(
            few.states_visited * 4 < full.states_visited,
            "limit=5 应因剪枝远少于全遍历：few={} full={}",
            few.states_visited,
            full.states_visited
        );
        // 剪枝不得损害正确性。
        for limit in [1usize, 5, 40, 676, 999] {
            assert_same(
                &r.search_prefix("ok", limit),
                &r.search_prefix_scan("ok", limit),
                &format!("剪枝对拍 limit={limit}"),
            );
        }
        let _ = std::fs::remove_file(&p);
    }

    /// **剪枝判据必须是严格小于**，放宽成 `<=` 即静默漏候选。
    ///
    /// 上界只覆盖 RankKey 的首要键 weight。`bound == 第 limit 名的 weight` 时，该子树内
    /// 仍可能有同 weight 但 order/leaf 更小的条目应当取代第 limit 名。
    ///
    /// 构造刻意让**出队顺序完全确定**（否则 `<` 与 `<=` 的差异会随 tie 顺序时隐时现，
    /// 测试变成抛硬币）：`za` 一个叶子内含两条（300/100）**一次性填满 limit=2**，
    /// 于是 worst 的 weight=100 由它独自决定；此时唯一待展开的 `a` 子树 maxw 恰为 100。
    /// `甲`(order=0, leaf=0) 优于 `丙`(order=1, leaf=1)，故必须能取代它。
    #[test]
    fn pruning_bound_must_be_strictly_less() {
        let p = build(
            "wdat_bnb_strict.wdat",
            &[
                ("ab", &[("甲", 100)][..]),
                ("za", &[("乙", 300), ("丙", 100)][..]),
            ],
        );
        let r = WdatReader::open(&p).unwrap();
        let got: Vec<String> = r
            .search_prefix("", 2)
            .iter()
            .map(|e| e.text.clone())
            .collect();
        assert_eq!(
            got,
            vec!["乙".to_string(), "甲".to_string()],
            "同 weight 时 order 更小的「甲」应取代「丙」；若剪枝误用 <=，「甲」所在子树被整棵跳过"
        );
        assert_same(
            &r.search_prefix("", 2),
            &r.search_prefix_scan("", 2),
            "严格小于对拍",
        );
        let _ = std::fs::remove_file(&p);
    }

    /// `maxw` 必须是**真实上界**：偏大只是剪枝变弱（结果仍正确），偏小则静默漏候选。
    /// 逐前缀与暴力遍历子树求得的最大 weight 对拍。
    #[test]
    fn maxw_equals_brute_force_subtree_max() {
        let p = build(
            "wdat_maxw_bound.wdat",
            &[
                ("ab", &[("甲", 100)][..]),
                ("abc", &[("乙", 700)][..]),
                ("abd", &[("丙", 300)][..]),
                ("az", &[("丁", 50)][..]),
                ("z", &[("戊", 900)][..]),
            ],
        );
        let r = WdatReader::open(&p).unwrap();
        let v = &r.main;
        for prefix in ["", "a", "ab", "abc", "abd", "az", "z"] {
            let Some(s) = r.walk(v, prefix) else {
                panic!("前缀 {prefix:?} 应可达");
            };
            let mut brute = NO_MAXW;
            let mut path: Vec<u8> = prefix.as_bytes().to_vec();
            r.for_each_leaf(v, s, &mut path, &mut |_code, leaf| {
                r.read_leaf_entries(v, leaf, &mut |_t, w, _o, _b| {
                    brute = brute.max(w);
                });
            });
            assert_eq!(
                r.maxw(v, s),
                brute,
                "前缀 {prefix:?} 的子树权重上界与暴力结果不符"
            );
        }
        let _ = std::fs::remove_file(&p);
    }

    /// **护栏**：DFS 必须遍历完整棵子树，不得早停。
    ///
    /// 最高权重的词放在字典序最末。若把本函数「优化」成 `codetable.rs` / `binformat.rs`
    /// 那种「攒够 limit(*2) 就 break」，DFS 按字典序推进，这条永远进不了结果集——
    /// 那正是本次修复**刻意没有采用**早停方案的原因（8.8 万条的库上偏差极显著）。
    #[test]
    fn topn_prefix_keeps_high_weight_entry_at_lexical_tail() {
        let mut data: Vec<(String, Vec<(String, i32)>)> = (0..400u32)
            .map(|i| {
                let c1 = (b'a' + (i / 26) as u8) as char;
                let c2 = (b'a' + (i % 26) as u8) as char;
                (format!("ok{c1}{c2}"), vec![(format!("庸{i}"), 10)])
            })
            .collect();
        data.push(("okzz".to_string(), vec![("压轴".to_string(), 9999)]));
        let p = build_owned("wdat_topn_tail.wdat", &data);
        let r = WdatReader::open(&p).unwrap();

        let out = r.search_prefix("ok", 5);
        assert_eq!(out.len(), 5);
        assert_eq!(
            out[0].text, "压轴",
            "字典序最末的高权重词被漏掉 ⇒ DFS 提前中止了"
        );
        assert_eq!(out[0].code, "okzz");
        let _ = std::fs::remove_file(&p);
    }

    /// `has_longer_code` 的基本语义：只认**严格更长**的后继，前缀自身成词不算。
    #[test]
    fn has_longer_code_semantics() {
        let p = build(
            "wdat_has_longer.wdat",
            &[
                ("ok", &[("好", 10)]),     // 自身成词，且下面还有更长的
                ("okz", &[("仄", 10)]),    // 中间节点，自身也成词
                ("okzz", &[("最长", 10)]), // 该支最长
                ("om", &[("某", 10)]),     // 自身成词，无更长后继
            ],
        );
        let r = WdatReader::open(&p).unwrap();

        assert!(r.has_longer_code(""), "空前缀：词库非空即有更长码");
        assert!(r.has_longer_code("o"), "o 下有 ok/om");
        assert!(r.has_longer_code("ok"), "ok 自身成词，但下面还有 okz/okzz");
        assert!(r.has_longer_code("okz"), "okz 自身成词，但下面还有 okzz");
        assert!(!r.has_longer_code("okzz"), "okzz 是该支最长，无更长后继");
        assert!(!r.has_longer_code("om"), "om 自身成词且无后继");
        assert!(!r.has_longer_code("xyz"), "前缀不存在");
        assert!(!r.has_longer_code("okzzz"), "越过最长码");
        let _ = std::fs::remove_file(&p);
    }

    /// `has_longer_code` 与旧判据（`search_prefix(_, 64)` 再 `any(code 更长)`）在
    /// **未触发截断**时逐一致——覆盖「前缀自身成词 / 中间节点 / 叶子 / 不存在」四类位置。
    ///
    /// 二者并非全等：旧判据经权重排序截断，长码候选权重偏低被挤出前 64 名时会漏判成
    /// false。新实现直接问 trie，故 **新 ⊇ 旧**（只会更保守，不会更激进）——这正是
    /// 「更长后继」作为上屏安全阀所需的方向。本测试锁住未截断场景下二者不得分歧。
    #[test]
    fn has_longer_code_agrees_with_legacy_predicate_when_untruncated() {
        let data: Vec<(String, Vec<(String, i32)>)> = (0..300u32)
            .map(|i| {
                let c1 = (b'a' + (i / 26) as u8) as char;
                let c2 = (b'a' + (i % 26) as u8) as char;
                // 一半是 3 码（叶），一半再挂 4 码后继 → 覆盖有/无更长后继两类。
                if i % 2 == 0 {
                    (format!("z{c1}{c2}"), vec![(format!("甲{i}"), 10)])
                } else {
                    (format!("z{c1}{c2}q"), vec![(format!("乙{i}"), 10)])
                }
            })
            .collect();
        let p = build_owned("wdat_has_longer_agree.wdat", &data);
        let r = WdatReader::open(&p).unwrap();

        let legacy = |prefix: &str| -> bool {
            let n = prefix.chars().count();
            r.search_prefix(prefix, 64)
                .iter()
                .any(|e| e.code.chars().count() > n)
        };
        // 枚举全部编码的全部前缀 + 空串 + 若干不存在的码。
        let mut prefixes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        prefixes.insert(String::new());
        for extra in ["zzzz", "q", "z"] {
            prefixes.insert(extra.to_string());
        }
        r.for_each_entry(&mut |code, _t, _w| {
            for i in 1..=code.len() {
                prefixes.insert(code[..i].to_string());
            }
        });
        for pre in &prefixes {
            assert_eq!(
                r.has_longer_code(pre),
                legacy(pre),
                "前缀 {pre:?} 上新旧判据分歧"
            );
        }
        let _ = std::fs::remove_file(&p);
    }

    /// limit=0 → 空（与改造前 `truncate(0)` 语义一致，且不做无谓的 DFS）。
    #[test]
    fn topn_prefix_limit_zero_is_empty() {
        let data = vec![("oka".to_string(), vec![("甲".to_string(), 10)])];
        let p = build_owned("wdat_topn_zero.wdat", &data);
        let r = WdatReader::open(&p).unwrap();
        assert!(r.search_prefix("ok", 0).is_empty());
        assert_eq!(r.search_prefix("ok", 1).len(), 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn exact_match_multi_key() {
        let p = build(
            "wdat_exact_test.wdat",
            &[
                ("a", &[("工", 9999), ("戈", 100)]),
                ("ni", &[("你", 800), ("尼", 50)]),
                ("nihao", &[("你好", 1200)]),
                ("zhongguo", &[("中国", 2000)]),
            ],
        );
        let r = WdatReader::open(&p).unwrap();
        assert_eq!(r.key_count(), 4);

        let a = r.search("a");
        assert_eq!(a.len(), 2);
        assert!(a.iter().any(|e| e.text == "工" && e.weight == 9999));
        assert!(a.iter().any(|e| e.text == "戈"));

        let nihao = r.search("nihao");
        assert_eq!(nihao.len(), 1);
        assert_eq!(nihao[0].text, "你好");
        assert_eq!(nihao[0].weight, 1200);
        assert_eq!(nihao[0].code, "nihao");

        let zg = r.search("zhongguo");
        assert_eq!(zg.len(), 1);
        assert_eq!(zg[0].text, "中国");

        // 不存在的 code。
        assert!(r.search("xyz").is_empty());
        assert!(r.search("n").is_empty()); // "n" 非终止（是 ni/nihao 前缀但无 n 自身词）
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn prefix_collects_subtree_with_codes() {
        let p = build(
            "wdat_prefix_test.wdat",
            &[
                ("ni", &[("你", 800)]),
                ("nihao", &[("你好", 1200)]),
                ("nihaoma", &[("你好吗", 300)]),
                ("nin", &[("您", 600)]),
                ("zhong", &[("中", 500)]),
            ],
        );
        let r = WdatReader::open(&p).unwrap();
        // 前缀 "ni" → ni/nihao/nihaoma/nin（不含 zhong）。
        let res = r.search_prefix("ni", 10);
        let texts: Vec<&str> = res.iter().map(|e| e.text.as_str()).collect();
        assert!(texts.contains(&"你"));
        assert!(texts.contains(&"你好"));
        assert!(texts.contains(&"你好吗"));
        assert!(texts.contains(&"您"));
        assert!(!texts.contains(&"中"), "前缀 ni 不应含 zhong: {texts:?}");
        // 重建的 code 正确。
        let nihao = res.iter().find(|e| e.text == "你好").unwrap();
        assert_eq!(nihao.code, "nihao");
        let nihaoma = res.iter().find(|e| e.text == "你好吗").unwrap();
        assert_eq!(nihaoma.code, "nihaoma");
        // 按权重降序。
        assert_eq!(res[0].text, "你好", "最高权重 1200 应排首: {texts:?}");
        // limit 截断。
        assert_eq!(r.search_prefix("ni", 2).len(), 2);
        let _ = std::fs::remove_file(&p);
    }

    /// 回归（wdat v3）：无权重（全同权）时，跨编码前缀查询应按**全局 order（词库出现顺序）**
    /// 排列，而非退化为编码字母序。构造 order 与字母序相反以区分两者。
    #[test]
    fn prefix_no_weight_sorts_by_global_order_not_code() {
        let p = std::env::temp_dir().join("wdat_global_order_test.wdat");
        let mut w = WdatWriter::new();
        // 出现顺序 za → ma → aa（order 0,1,2），编码字母序则相反 aa < ma < za；权重全 0。
        w.add_with_order("za".to_string(), vec![("Z".to_string(), 0, 0)]);
        w.add_with_order("ma".to_string(), vec![("M".to_string(), 0, 1)]);
        w.add_with_order("aa".to_string(), vec![("A".to_string(), 0, 2)]);
        w.write(&p).expect("write wdat");
        let r = WdatReader::open(&p).unwrap();
        let res = r.search_prefix("", 10);
        let texts: Vec<&str> = res.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["Z", "M", "A"],
            "无权重应按全局 order（出现序）排序而非编码字母序，实际: {texts:?}"
        );
        // 单编码多条同权：前缀查询（会按 weight desc、order asc 排序）亦按 order 升序恢复出现序。
        // （精确 search 不排序、返回叶内存储序——生产路径写入前已预排序，故存储序即展示序。）
        let mut w2 = WdatWriter::new();
        w2.add_with_order(
            "aa".to_string(),
            vec![
                ("三".to_string(), 0, 2),
                ("一".to_string(), 0, 0),
                ("二".to_string(), 0, 1),
            ],
        );
        let p2 = std::env::temp_dir().join("wdat_global_order_test2.wdat");
        w2.write(&p2).expect("write wdat");
        let r2 = WdatReader::open(&p2).unwrap();
        let t2: Vec<String> = r2
            .search_prefix("aa", 10)
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert_eq!(
            t2,
            vec!["一", "二", "三"],
            "同码同权前缀查询应按 order 升序"
        );
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&p2);
    }

    #[test]
    fn for_each_enumerates_all() {
        let p = build(
            "wdat_foreach_test.wdat",
            &[("a", &[("工", 9999), ("戈", 100)]), ("aaaa", &[("叕", 50)])],
        );
        let r = WdatReader::open(&p).unwrap();
        let mut got: Vec<(String, String, i32)> = Vec::new();
        r.for_each_entry(&mut |c, t, w| got.push((c.to_string(), t.to_string(), w)));
        assert_eq!(got.len(), 3, "应枚举 3 条: {got:?}");
        assert!(got.contains(&("a".to_string(), "工".to_string(), 9999)));
        assert!(got.contains(&("a".to_string(), "戈".to_string(), 100)));
        assert!(got.contains(&("aaaa".to_string(), "叕".to_string(), 50)));
        let _ = std::fs::remove_file(&p);
    }

    /// **对拍**：同一份数据分别建 wdb(binformat) 与 wdat，比对精确/前缀/全枚举查询结果一致
    /// （按 (code,text,weight) 集合比较，忽略 natural_order 这一格式差异）。这是 wdb→wdat
    /// 迁移的核心正确性保证。
    #[test]
    fn parity_with_wdb() {
        use crate::binformat::{DictReader, DictWriter};
        let data: Vec<(&str, Vec<(&str, i32)>)> = vec![
            ("a", vec![("工", 9999), ("戈", 100)]),
            ("ni", vec![("你", 800), ("尼", 50)]),
            ("nihao", vec![("你好", 1200)]),
            ("nihaoma", vec![("你好吗", 300)]),
            ("nin", vec![("您", 600)]),
            ("zhong", vec![("中", 500)]),
            ("zhongguo", vec![("中国", 2000)]),
            ("zhi", vec![("之", 700), ("知", 690)]),
        ];
        let to_owned = |e: &Vec<(&str, i32)>| -> Vec<(String, i32)> {
            e.iter().map(|(t, w)| (t.to_string(), *w)).collect()
        };

        let wdb_path = std::env::temp_dir().join("wdat_parity.wdb");
        let mut dw = DictWriter::new();
        for (c, e) in &data {
            dw.add(c.to_string(), to_owned(e));
        }
        dw.write(&wdb_path).unwrap();
        let wdb = DictReader::open(&wdb_path).unwrap();

        let wdat_path = std::env::temp_dir().join("wdat_parity.wdat");
        let mut ww = WdatWriter::new();
        for (c, e) in &data {
            ww.add(c.to_string(), to_owned(e));
        }
        ww.write(&wdat_path).unwrap();
        let wdat = WdatReader::open(&wdat_path).unwrap();

        assert_eq!(wdb.key_count(), wdat.key_count(), "key_count 应一致");

        // 精确：每个 code 的 (text,weight) 集合一致。
        for (c, _) in &data {
            let mut a: Vec<(String, i32)> = wdb
                .search(c)
                .into_iter()
                .map(|e| (e.text, e.weight))
                .collect();
            let mut b: Vec<(String, i32)> = wdat
                .search(c)
                .into_iter()
                .map(|e| (e.text, e.weight))
                .collect();
            a.sort();
            b.sort();
            assert_eq!(a, b, "精确查询 '{c}' 不一致");
        }
        // 不存在 code 两者都空。
        assert!(wdb.search("xyz").is_empty() && wdat.search("xyz").is_empty());

        // 前缀：(code,text,weight) 集合一致（含空前缀=全量、单字母、整码）。
        for pre in ["ni", "n", "nih", "zhong", "z", "a", "zh", ""] {
            let mut a: Vec<(String, String, i32)> = wdb
                .search_prefix(pre, 100000)
                .into_iter()
                .map(|e| (e.code, e.text, e.weight))
                .collect();
            let mut b: Vec<(String, String, i32)> = wdat
                .search_prefix(pre, 100000)
                .into_iter()
                .map(|e| (e.code, e.text, e.weight))
                .collect();
            a.sort();
            b.sort();
            assert_eq!(a, b, "前缀查询 '{pre}' 不一致");
        }

        // for_each_entry：全量 (code,text,weight) 集合一致。
        let mut ea = Vec::new();
        wdb.for_each_entry(&mut |c, t, w| ea.push((c.to_string(), t.to_string(), w)));
        let mut eb = Vec::new();
        wdat.for_each_entry(&mut |c, t, w| eb.push((c.to_string(), t.to_string(), w)));
        ea.sort();
        eb.sort();
        assert_eq!(ea, eb, "全枚举不一致");

        let _ = std::fs::remove_file(&wdb_path);
        let _ = std::fs::remove_file(&wdat_path);
    }

    /// 简拼 AbbrevSection 往返：简拼查得到、按权重排序，且**不污染全拼**精确/前缀查询。
    ///
    /// ⚠️ **条目内容已随 v5 从「词」改为「全拼码」**（二级索引指向主键）。取出的
    /// `DictEntry::text` 现在装的是码，调用方拿它去主表装配候选。
    #[test]
    fn abbrev_section_roundtrip() {
        let p = std::env::temp_dir().join("wdat_abbrev_test.wdat");
        let mut w = WdatWriter::new();
        w.add("nihao".into(), vec![("你好".into(), 1200)]);
        w.add("beijing".into(), vec![("北京".into(), 2000)]);
        // 存码不存词；同一简拼下多个码按权重降序（权重只用于截断时取舍）。
        w.add_abbrev(
            "nh".into(),
            vec![("nihao".into(), 1200), ("nihuo".into(), 5)],
        );
        w.add_abbrev("bj".into(), vec![("beijing".into(), 2000)]);
        w.add_abbrev("nhm".into(), vec![("nihaoma".into(), 300)]);
        w.write(&p).unwrap();

        let r = WdatReader::open(&p).unwrap();
        assert!(r.has_abbrev());
        // 简拼命中 + 权重降序；取出的是**全拼码**。
        let nh = r.search_abbrev("nh", 10);
        assert_eq!(nh.len(), 2);
        assert_eq!(nh[0].text, "nihao", "按权重降序，且存的是码: {nh:?}");
        assert_eq!(nh[0].code, "nh");
        assert_eq!(r.search_abbrev("bj", 10)[0].text, "beijing");
        assert_eq!(r.search_abbrev("nhm", 10)[0].text, "nihaoma");
        assert!(r.search_abbrev("zzz", 10).is_empty());
        // **不污染全拼**：全拼 search/search_prefix 不应命中简拼码。
        assert!(r.search("nh").is_empty(), "全拼精确不应命中简拼码 nh");
        assert_eq!(r.search("nihao")[0].text, "你好");
        let pre = r.search_prefix("n", 100);
        assert!(pre.iter().any(|e| e.text == "你好"));
        assert!(
            !pre.iter().any(|e| e.code == "nh" || e.code == "nhm"),
            "前缀查询不应含简拼码: {:?}",
            pre.iter().map(|e| &e.code).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_file(&p);
    }

    /// 无简拼区段时 search_abbrev 返回空、has_abbrev=false。
    #[test]
    fn no_abbrev_section() {
        let p = build("wdat_noabbrev_test.wdat", &[("a", &[("工", 1)])]);
        let r = WdatReader::open(&p).unwrap();
        assert!(!r.has_abbrev());
        assert!(r.search_abbrev("nh", 10).is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn shared_prefix_and_branching() {
        // 强分支：同前缀大量分叉，验证 base/check 冲突解决正确。
        let data: Vec<(String, Vec<(String, i32)>)> = (0..200)
            .map(|i| (format!("code{i:03}"), vec![(format!("词{i}"), i)]))
            .collect();
        let p = std::env::temp_dir().join("wdat_branch_test.wdat");
        let mut w = WdatWriter::new();
        for (c, e) in &data {
            w.add(c.clone(), e.clone());
        }
        w.write(&p).unwrap();
        let r = WdatReader::open(&p).unwrap();
        assert_eq!(r.key_count(), 200);
        for i in 0..200 {
            let res = r.search(&format!("code{i:03}"));
            assert_eq!(res.len(), 1, "code{i:03} 应命中");
            assert_eq!(res[0].text, format!("词{i}"));
            assert_eq!(res[0].weight, i);
        }
        // 前缀 code0 → code000..code099（100 条）。
        let pre = r.search_prefix("code0", 1000);
        assert_eq!(pre.len(), 100, "code0 前缀应 100 条");
        let _ = std::fs::remove_file(&p);
    }

    /// 回归测试：复刻真实故障现场的 key 形态——上百万条、编码几乎全落在 6/8 位、
    /// 字母表仅 a-z（真实样本见 feihuzj2 方案的 `feihuzj2_extra_jichu_pro.dict.yaml`，
    /// 146 万 key）。旧的线性探测 `find_base` 在这种"深、密、共享前缀"的输入下会
    /// 退化为 O(状态数²)，实测数分钟不收敛、内存被撑到 GB 级（见 [`FreeList`] 顶部
    /// 说明）。这里用 1/5 规模（30 万 key）验证新实现在合理时间内完成且结果正确。
    #[test]
    fn large_scale_build_stays_fast_and_correct() {
        fn gen_code(mut n: u64, len: usize) -> String {
            let mut buf = vec![b'a'; len];
            for i in (0..len).rev() {
                buf[i] = b'a' + (n % 26) as u8;
                n /= 26;
            }
            String::from_utf8(buf).unwrap()
        }
        let total = 300_000usize;
        let mut data: Vec<(String, Vec<(String, i32)>)> = Vec::with_capacity(total);
        let (mut c4, mut c6, mut c8) = (0u64, 0u64, 0u64);
        for i in 0..total {
            // 大致复刻真实文件里 4/6/8 位码的比例（约 1:4:4）。
            let code = match i % 9 {
                0 => {
                    let c = gen_code(c4, 4);
                    c4 += 1;
                    c
                }
                1..=4 => {
                    let c = gen_code(c6, 6);
                    c6 += 1;
                    c
                }
                _ => {
                    let c = gen_code(c8, 8);
                    c8 += 1;
                    c
                }
            };
            data.push((code, vec![(format!("词{i}"), i as i32)]));
        }

        let p = std::env::temp_dir().join("wdat_large_scale_test.wdat");
        let start = std::time::Instant::now();
        let mut w = WdatWriter::new();
        for (c, e) in &data {
            w.add(c.clone(), e.clone());
        }
        w.write(&p).unwrap();
        let elapsed = start.elapsed();
        eprintln!("built {total} keys in {elapsed:?}");
        assert!(
            elapsed.as_secs() < 30,
            "{total} 个 key 构建耗时 {elapsed:?}——若退化回平方级复杂度，这里会是\
             几分钟甚至更久（历史故障见 FreeList 顶部说明）",
        );

        let r = WdatReader::open(&p).unwrap();
        assert_eq!(r.key_count() as usize, total);
        // 抽样点查验证正确性（量太大，不做全量校验）。
        for i in (0..total).step_by(997) {
            let (code, _) = &data[i];
            let res = r.search(code);
            assert_eq!(res.len(), 1, "{code} 应命中");
            assert_eq!(res[0].text, format!("词{i}"));
        }
        let _ = std::fs::remove_file(&p);
    }
}
