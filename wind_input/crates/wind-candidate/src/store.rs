//! 通用候选词筛选容器：封装「原始候选 + 可选筛选视图」。
//!
//! - 原始数据始终保留在 `original` 字段中，筛选构建独立的显示列表
//! - 清除筛选即复制原始数据恢复全量显示
//!
//! 使用场景：
//! - 辅助码：`set_candidates`（快照）+ `set_filter`（重筛）+ `clear_filter`（还原）
//!
//! # ⛔ 曾有一个 `filtered_out()`（被滤集迭代器），已删除
//!
//! 它写来预备「翻页放宽」用，但那条路一直没接过来——而辅助码是**明确不做**翻页放宽的
//! （见 `aux_code_does_not_relax_scope_on_page_end`），所以既没有消费者也没有在途的
//! 消费者。留着的害处不是占地方，是它**按 `text` 建 HashSet 做差集**：同一段文本在
//! `original` 里出现两次而只有一条通过筛选时，两条都会被当成「已显示」，被滤的那条
//! 就此漏出结果。这个错在没有消费者时完全看不出来，等哪天有人把它接上翻页放宽，
//! 收到的是一个静默漏项的列表。
//!
//! 真要重做：按**下标**记录哪些进了 `displayed`（`set_filter` 时顺手存一份
//! `Vec<usize>`），别再按文本比对。同理删掉的还有 `is_filtered` / `original` /
//! `into_original`——都是没有消费者的推测式接口。

use crate::Candidate;

/// 候选词筛选容器。
///
/// 持有原始候选列表（拥有所有权）和一个可选的筛选视图。筛选不修改原始数据，
/// 只构建一组新的候选列表（通过筛选的候选的克隆）。
///
/// # 示例
///
/// ```ignore
/// let mut store = CandidateStore::new();
/// store.set_candidates(vec![cand_a, cand_b, cand_c]);
/// store.set_filter(|c| c.is_common);         // 只保留常用词
/// assert_eq!(store.displayed().len(), 1);     // 只有 cand_a
/// store.clear_filter();
/// assert_eq!(store.displayed().len(), 3);     // 恢复全量
/// ```
pub struct CandidateStore {
    /// 原始候选（拥有所有权，筛选期间不修改）。
    original: Vec<Candidate>,
    /// 当前显示的候选（筛选后的子集或原始的克隆）。
    displayed: Vec<Candidate>,
}

impl Default for CandidateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CandidateStore {
    /// 创建空容器。
    pub fn new() -> Self {
        Self {
            original: Vec::new(),
            displayed: Vec::new(),
        }
    }

    /// 替换原始候选列表，同时重置显示为全量。
    ///
    /// 语义：新数据到来（引擎 convert 结果），旧快照失效。
    pub fn set_candidates(&mut self, candidates: Vec<Candidate>) {
        self.original = candidates;
        self.displayed = self.original.clone();
    }

    /// 应用筛选谓词，构建筛选视图。
    ///
    /// 从 `original` 中选出所有使 `predicate` 返回 `true` 的候选，按原序组成显示列表。
    /// 重复调用会覆盖前一次的筛选（等效于先 `clear_filter` 再 `set_filter`）。
    pub fn set_filter(&mut self, mut predicate: impl FnMut(&Candidate) -> bool) {
        self.displayed = self
            .original
            .iter()
            .filter(|c| predicate(c))
            .cloned()
            .collect();
    }

    /// 清除筛选视图，恢复全量显示。
    pub fn clear_filter(&mut self) {
        self.displayed = self.original.clone();
    }

    /// 当前显示的候选（筛选后的子集或全量）。
    pub fn displayed(&self) -> &[Candidate] {
        &self.displayed
    }

    /// 当前显示的候选数量。
    pub fn len(&self) -> usize {
        self.displayed.len()
    }

    /// 当前是否为空。
    pub fn is_empty(&self) -> bool {
        self.displayed.is_empty()
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
