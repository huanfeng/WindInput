//! 辅助码过滤会话：聚合「候选快照 + 辅助码缓冲 + 重筛」的辅助码专用筛选状态。
//!
//! 会话是辅助码模式的**状态机种子**：进入时由调用方注入原始候选快照（引擎 convert
//! 结果的单槽 memo），此后每次按键（push / pop 缓冲）都从**快照**重筛，而不是在已
//! 筛选的列表上再筛——否则退格还原时 kept 相对顺序会被上一步筛选打乱，候选栏无法
//! 还原到「上一层筛选」。
//!
//! 会话是纯逻辑、不接触文件系统、不依赖协调器状态，可在任意主机编译测试。
//! **不含显示态**：组合区（preedit）拼接、光标定位是协调器的职责（与 `State.preedit`/
//! caret 机制绑在一起），不在这里——避免把 coordinator 的显示约定拖进纯筛选 crate。
//!
//! **被滤候选直接丢弃**：`apply` 只返回命中者（候选窗只显示匹配词，如 `om` 配
//! 「时间」「实践」时实践消失），还原不靠残留标记——快照在手，退出/退格都从快照恢复。

use wind_candidate::{Candidate, CandidateStore};

use crate::filter::{AuxCodeFilterOptions, aux_code_matches};
use crate::lookup::AuxCodeLookup;

/// 辅助码会话：进入时建立、退出/上屏时销毁，随 `ModeKind::AuxCode` 独占存在。
///
/// 承载筛选所需的全部种子——进入时引擎产出的原始候选快照（退格/退出据此还原原样
/// 顺序）与辅助码缓冲。外界只经 `State.aux` 持有它；缓冲 push/pop、从快照重筛等
/// 语义全在本类型内。
pub struct AuxCodeSession {
    /// 辅助码输入缓冲（字形辅助码串，如 "mz"）。
    buffer: String,
    /// 候选快照容器：持有原始候选快照，每次筛选都从它重筛（而非在已筛选的列表上再筛）。
    /// 使用 [`CandidateStore`] 统一管理快照的存储与访问。
    store: CandidateStore,
}

impl AuxCodeSession {
    /// 建立会话：缓冲置空，`original_candidates` 即进入时的原始候选快照。
    pub fn new(original_candidates: Vec<Candidate>) -> Self {
        let mut store = CandidateStore::new();
        store.set_candidates(original_candidates);
        Self {
            buffer: String::new(),
            store,
        }
    }

    /// 当前辅助码缓冲。
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// 辅助码缓冲是否为空。
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// 追加一个辅助码字符。
    pub fn push_char(&mut self, ch: char) {
        self.buffer.push(ch);
    }

    /// 弹出一个辅助码字符（无则 `None`）。
    pub fn pop_char(&mut self) -> Option<char> {
        self.buffer.pop()
    }

    /// 按当前缓冲对**原始候选快照**重筛，返回**通过筛选的候选**（保持快照相对顺序）。
    ///
    /// 被滤候选**不进返回列表**——辅助码是「字形二次筛选」，候选窗只显示匹配词
    /// （如 `om` 配「时间」「实践」时，实践被滤掉、不再可见）。还原不需要把被滤候选
    /// 留在列表里兜底：快照在手，退出用 [`Self::restore_original`]、退格用空缓冲
    /// passthrough（见 [`aux_code_matches`]），都能从快照恢复。空缓冲 / 空表同样由
    /// [`aux_code_matches`] 内部 passthrough（原样放行全部候选）。
    pub fn apply(
        &mut self,
        table: &dyn AuxCodeLookup,
        options: &AuxCodeFilterOptions,
    ) -> Vec<Candidate> {
        self.store
            .set_filter(|c| aux_code_matches(c, table, &self.buffer, options));
        self.store.displayed().to_vec()
    }

    /// 退出还原：清除筛选视图，返回原始候选快照。
    pub fn restore_original(&mut self) -> Vec<Candidate> {
        self.store.clear_filter();
        self.store.displayed().to_vec()
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
