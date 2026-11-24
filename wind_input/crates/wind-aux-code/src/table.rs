//! 辅助码表：字 → 辅助码列表的紧凑映射
//!
//! 存储布局与 `wind-reverse::PinyinTable`（字→读音列表）完全同构，三段式布局：
//!
//! ```text
//! entries: Vec<Entry{ ch, codes_end }>   按字升序
//!   ├─ 每条对应一个唯一汉字；codes_end 指向该字的所有码在 code_ends 中的
//!   │  结束下标。下标区间 = [前条 codes_end .. 本条 codes_end)
//!   │  （首条起点为 0）
//!
//! code_ends: Vec<u32>                   每码在 arena 中的结束偏移
//!   ├─ 单个辅助码的文本区间 = [code_ends[i-1] .. code_ends[i])
//!   │  （首项起点为 0）
//!   │  【码长任意】笔画/拆字等方案码长不固定，字节区间长度 = 该码的 UTF-8 字节数
//!
//! arena: String                            所有辅助码字符串按序连续拼接
//!   └─ 例：厑 有三码 u、ab、wxyz → arena 存 "uabwxyz"，code_ends = [1, 3, 7]
//! ```
//!
//! ## 单表语义
//! 同字多码严格保留行序（行序即优先级），做 first-seen 稳定去重、不排序。
//!
//! ## 多表挂载
//! 不维护「多张独立表」结构：挂载时经 [`AuxCodeTable::merge`] / [`AuxCodeTable::append`]
//! 一次性 flatten 成一张表，表级优先级、行级优先级与跨表去重全部坍缩到物理序中，
//! 查询零额外开销。挂载顺序即迭代顺序。
//!
//! 文本 / 文件加载（txt 格式 → 表）见 [`crate::loader`]；过滤语义见 [`crate::filter`]。

/// 字级条目：单个汉字在 entries 数组中的登记
#[derive(Debug, Clone, Copy)]
struct CharEntry {
    /// 该条目的汉字（entries 按此字段升序排列，供二分查找定位）
    ch: char,
    /// 该字的码序列在 `code_ends` 中的结束下标（下标区间 = 前条 codes_end .. 本条 codes_end）
    codes_end: u32,
}

/// 辅助码表：字 → 辅助码列表（三段式紧凑存储；单表 / 合并多表通用）
#[derive(Default, Debug)]
pub struct AuxCodeTable {
    /// 辅助码方法名（如「笔画」）；空 = 未命名，调用方回落文件主干名。
    pub name: String,
    /// 按 `ch` 升序排列的字级条目
    entries: Vec<CharEntry>,
    /// 每一个辅助码在 arena 中的结束偏移，按「字序 + 字内行序」连续排列
    code_ends: Vec<u32>,
    /// 所有辅助码字符串的共享拼接区
    arena: String,
}

/// 某字的辅助码列表视图：按需从 arena 切片，取用不额外分配
#[derive(Clone, Copy)]
pub(crate) struct CodeListView<'a> {
    table: &'a AuxCodeTable,
    /// code_ends 下标区间 [start, end)
    start: usize,
    end: usize,
}

impl<'a> CodeListView<'a> {
    /// 最高优先级辅助码（第一个）；仅测试用（`first_code`）。
    #[cfg(test)]
    fn first(&self) -> Option<&'a str> {
        (self.start < self.end).then(|| self.table.code_at(self.start))
    }

    /// 按优先级遍历所有辅助码
    pub(crate) fn iter(self) -> impl Iterator<Item = &'a str> {
        (self.start..self.end).map(move |i| self.table.code_at(i))
    }
}

impl AuxCodeTable {
    // ------------------------------------------------------------------
    // 构造：空表 / 单表 rows / 多表合并 / 增量追加
    // ------------------------------------------------------------------

    /// 构造空码表（未挂载任何辅助码 → 过滤时原样放行，见 [`crate::filter::filter_by_aux_code`]）
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 (字, 辅助码) 行构建**单张**码表。
    ///
    /// 语义：
    /// - 同字多行**按序保留 first-seen 去重**：同字的同一码只留首次出现的行（节省空间），
    ///   不同码按原序依次入列——文件中出现的先后即行级优先级
    /// - 空辅助码的行跳过（空码 = 没码，占一条记录毫无意义）
    /// - 字去重只发生在 entries 层：多个码归并到同一个字的 code_ends 区间内
    pub fn from_rows(rows: Vec<(char, &str)>) -> Self {
        // 1. 过滤空码 + 稳定排序（保留行内顺序，同字条连续出现，code_ends 自然按序入列）
        let mut rows = rows
            .into_iter()
            .filter(|(_, code)| !code.is_empty())
            .collect::<Vec<_>>();
        rows.sort_by_key(|(ch, _)| *ch);

        let mut entries = Vec::with_capacity(rows.len());
        let mut code_ends = Vec::with_capacity(rows.len());
        let mut arena = String::with_capacity(rows.iter().map(|(_, c)| c.len()).sum());

        // 2. 相邻同字的多行归并到同一个 entry，code_ends 逐条推入
        //    同字范围内对新码做 first-seen 线性比对：相同码已存在则跳过
        let mut i = 0;
        while i < rows.len() {
            let (cur_ch, _) = rows[i];
            let codes_start_for_char = code_ends.len();
            let mut j = i;
            while j < rows.len() && rows[j].0 == cur_ch {
                let (_, code) = rows[j];
                // First-seen 去重：linear scan 该字已生成的码范围（通常 1~3 个，代价可忽略）
                let mut seen = false;
                for k in codes_start_for_char..code_ends.len() {
                    if Self::code_at_from_parts(&arena, &code_ends, k) == code {
                        seen = true;
                        break;
                    }
                }
                if !seen {
                    arena.push_str(code);
                    code_ends.push(arena.len() as u32);
                }
                j += 1;
            }
            entries.push(CharEntry {
                ch: cur_ch,
                codes_end: code_ends.len() as u32,
            });
            i = j;
        }

        entries.shrink_to_fit();
        code_ends.shrink_to_fit();
        arena.shrink_to_fit();

        tracing::debug!(
            "辅助码表构建完成：{} 字，{} 码条目，arena {} 字节",
            entries.len(),
            code_ends.len(),
            arena.len()
        );

        Self {
            name: String::new(),
            entries,
            code_ends,
            arena,
        }
    }

    /// 设置名称（元数据；空表加载时由 loader 回落文件主干名）。
    pub fn with_name(mut self, name: String) -> Self {
        self.name = name;
        self
    }

    /// **合并多张码表**：按迭代顺序 = 挂载优先级序（先出现 = 高优）。
    ///
    /// 高优表的 (字,码) 先入列，交给 `from_rows` 稳定排序 + first-seen 去重：同字时保留
    /// 入参先后，低优表的重复码丢弃；空表自动跳过。
    pub fn merge<I>(tables: I) -> Self
    where
        I: IntoIterator<Item = AuxCodeTable>,
    {
        // Per-table codes 是 &str（指向各自表的 arena），必须先 owned 化，
        // 才能把 &'_ str 统一交给同一个 from_rows。这是低频的设置路径，String 分配可接受。
        let mut owned: Vec<(char, String)> = Vec::new();
        let mut name = String::new();
        for t in tables {
            if name.is_empty() {
                name = t.name.clone();
            }
            for (ch, code) in t.all_entries_codes() {
                owned.push((ch, code.to_string()));
            }
        }
        let borrowed: Vec<(char, &str)> = owned
            .iter()
            .map(|(ch, code)| (*ch, code.as_str()))
            .collect();
        Self::from_rows(borrowed).with_name(name)
    }

    /// 增量追加：把 `other` 表作为**最低优先级**挂到现有表的末尾。
    ///
    /// 实现上是 self + other 一并交给 `merge` 重建；批量挂载应直接用 `merge`。
    pub fn append(&mut self, other: AuxCodeTable) {
        if other.is_empty() {
            tracing::debug!("跳过空的辅助码表（未收录任何字）");
            return;
        }
        let prev = std::mem::take(self);
        *self = Self::merge(vec![prev, other]);
    }

    // ------------------------------------------------------------------
    // 内部工具：区间切片取码
    // ------------------------------------------------------------------

    /// 按 code_ends 的全局下标取单个辅助码文本
    fn code_at(&self, idx: usize) -> &str {
        Self::code_at_from_parts(&self.arena, &self.code_ends, idx)
    }

    /// 静态版：在 `from_rows` 构建期间（只有 arena + code_ends 切片时）取指定码文本，
    /// 避免在去重循环里重复写区间计算逻辑
    fn code_at_from_parts<'a>(arena: &'a str, code_ends: &'a [u32], idx: usize) -> &'a str {
        let start = if idx == 0 {
            0
        } else {
            code_ends[idx - 1] as usize
        };
        &arena[start..code_ends[idx] as usize]
    }

    // ------------------------------------------------------------------
    // 查询接口：单字码列表 / 首选码 / 前缀匹配判据
    // ------------------------------------------------------------------

    /// 二分查找某字的辅助码列表视图；未收录返回 None
    pub(crate) fn view_codes(&self, ch: char) -> Option<CodeListView<'_>> {
        let i = self.entries.binary_search_by_key(&ch, |e| e.ch).ok()?;
        let start = if i == 0 {
            0
        } else {
            self.entries[i - 1].codes_end as usize
        };
        Some(CodeListView {
            table: self,
            start,
            end: self.entries[i].codes_end as usize,
        })
    }

    /// 某字的所有辅助码迭代器（按「行级优先级 / 合并时的全局优先级」排序）；
    /// 未收录返回空迭代。同字同码已 first-seen 去重，跨表同码也已去重。
    #[cfg(test)]
    pub fn codes_of(&self, ch: char) -> impl Iterator<Item = &str> {
        self.view_codes(ch).into_iter().flat_map(|v| v.iter())
    }

    /// 某字的**最高优先级辅助码**（合并后全局第一个）；未收录返回 None
    #[cfg(test)]
    pub fn first_code(&self, ch: char) -> Option<&str> {
        self.view_codes(ch).and_then(|v| v.first())
    }

    /// 过滤核心判据：任意辅助码以 `prefix` 为前缀即返回 true。
    ///
    /// **early-return**：找到第一个匹配的码立即停止扫描剩余码。
    pub fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
        match self.view_codes(ch) {
            Some(v) => v.iter().any(|c| c.starts_with(prefix)),
            None => false,
        }
    }

    /// 过滤判据（词组逐字匹配用）：任意辅助码以单字符 `c` 开头。
    ///
    /// 与 [`Self::any_code_starts_with`] 的区别：后者按**字符串前缀**匹配（码可长可短，
    /// 需构造前缀串），本方法只按**单个字符**判定首码——词组「每字第一个辅助码」逐位
    /// 对齐时无需临时分配字符串，是按键热路径上的零分配版本。
    pub fn any_code_starts_with_char(&self, ch: char, c: char) -> bool {
        match self.view_codes(ch) {
            Some(v) => v.iter().any(|code| code.starts_with(c)),
            None => false,
        }
    }

    // ------------------------------------------------------------------
    // 状态查询
    // ------------------------------------------------------------------

    /// 码表是否为空（无任何字 → 过滤时全滤）
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 收录的不同汉字数
    pub fn char_count(&self) -> usize {
        self.entries.len()
    }

    /// 收录的总码条数（>= char_count，同字多码会累加；合并表已去重后的值）
    #[cfg(test)]
    pub fn code_count(&self) -> usize {
        self.code_ends.len()
    }

    // ------------------------------------------------------------------
    // 合并辅助：把整张表按「字升序 × 字内优先级」展开成 (字, 码) 对序列
    // ------------------------------------------------------------------

    /// 遍历整张表的所有 (字, 辅助码) 对，按「字升序 × 字内行级优先级」展开。
    ///
    /// 主要供 [`AuxCodeTable::merge`] 内部串联多张表使用：
    /// 按挂载顺序把各表迭代结果首尾相接 → 交给 `from_rows`，
    /// 即可一次性完成跨表优先级排序 + 跨表 first-seen 去重。
    pub fn all_entries_codes(&self) -> impl Iterator<Item = (char, &str)> {
        self.entries.iter().enumerate().flat_map(move |(i, e)| {
            let start = if i == 0 {
                0
            } else {
                self.entries[i - 1].codes_end as usize
            };
            let end = e.codes_end as usize;
            (start..end).map(move |k| (e.ch, self.code_at(k)))
        })
    }
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
