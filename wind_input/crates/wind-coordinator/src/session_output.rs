//! 中立的输出通道：不带任何平台绘制概念的 [`SessionEvent`]。
//!
//! 协调器同时服务桌面和移动端。桌面输出是一串 `UiCommand`（候选窗坐标、首显闸门、
//! 注释渲染、工具栏位置……），移动端只要「现在该显示什么」。构造时给了 `session_tx`
//! （见 `Coordinator::new_session`）就只发本模块的事件，并跳过桌面专属的计算；
//! 没给就走原来的 `UiCommand` 路径，一字节不变。
//!
//! ★ 所有「根据 `session_tx` 选通道」的分支都集中在本文件的 `emit_*` 里，调用点只传参。
//! `tests/output_seam.rs` 守着这条缝：别处直接发 `HideCandidates` / `ShowToast` /
//! `ShowStatusTip` / `UpdateToolbar` / `SkinSignal` 会让该测试失败。
//!
//! ⚠ 事件是在持有 state 锁时发出的（mpsc 通道，不是同步回调）。state 锁不可重入：
//! 持锁路径只能用接受 `&State` 或已算好的值的内部形式，不能调公开的
//! [`Coordinator::is_composing`] / [`Coordinator::latin_input`]（它们自己取锁）。

use std::sync::mpsc;

use wind_ui_types::{
    SkinSignal, StatusTipPlacement, ToastKind, ToastPosition, ToolbarState, UiCommand,
};

use crate::coordinator::{Coordinator, State};

/// 协调器发给宿主的中立事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// 当前组合区与候选页的快照。
    View(CompositionView),
    /// 收起候选与组合区。
    Hide,
    /// 输入状态（模式、全半角、标点……）有变化。
    Status(SessionStatus),
    /// 一次性文字提示（桌面的 toast 与状态气泡在这里合为一条）。
    Notice(String),
}

/// 一帧候选/组合区。候选文本不在帧里，宿主按需经 `candidate_pull` 取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionView {
    /// 组合区显示串（已套音节分隔符）。联想态里它是联想标识，不是用户敲的码。
    pub preedit: String,
    /// 插入符在 `preedit` 内的字节偏移。
    pub preedit_caret: usize,
    /// 是否在「打字中」：`!preedit.is_empty() && !assoc_active && (active.is_some() || !input_buffer.is_empty())`。
    pub composing: bool,
    /// 模式标记（拼/双/快/英/符），仅候选为空时非空。
    pub mode_label: String,
    pub has_candidates: bool,
    /// 页内高亮下标（展开态为平铺下标），与桌面 `UpdateCandidates::selected` 同值。
    pub selected: usize,
    /// 当前页，从 1 起。
    pub page: usize,
    pub total_pages: usize,
}

/// 输入状态快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatus {
    pub chinese_mode: bool,
    /// 键是否按拉丁字母直出：英文方案、中文模式关闭、或不可输入（密码框等）。
    pub latin_input: bool,
    pub icon_label: String,
    pub full_width: bool,
    pub chinese_punct: bool,
    pub s2t_enabled: bool,
    pub s2t_shown: bool,
}

/// 协调器持有的事件发送端（`Option`：桌面构造时为 `None`）。
pub(crate) type SessionSender = mpsc::Sender<SessionEvent>;

impl Coordinator {
    fn emit_session(&self, ev: SessionEvent) {
        if let Some(tx) = &self.session_tx {
            let _ = tx.send(ev);
        }
    }

    /// 收起候选。桌面：`HideCandidates`。
    pub(crate) fn emit_hide(&self) {
        match &self.session_tx {
            Some(_) => self.emit_session(SessionEvent::Hide),
            None => {
                let _ = self.ui_tx.send(UiCommand::HideCandidates);
            }
        }
    }

    /// 一次性提示。桌面：`ShowToast`（位置/类型/时长/强调色原样发）。
    pub(crate) fn emit_toast(
        &self,
        text: String,
        position: ToastPosition,
        kind: ToastKind,
        duration_ms: u64,
        accent: Option<[u8; 4]>,
    ) {
        match &self.session_tx {
            Some(_) => self.emit_session(SessionEvent::Notice(text)),
            None => {
                let _ = self.ui_tx.send(UiCommand::ShowToast {
                    text,
                    position,
                    kind,
                    duration_ms,
                    accent,
                });
            }
        }
    }

    /// 状态气泡。桌面：`ShowStatusTip`；中立通道并入 [`SessionEvent::Notice`]。
    pub(crate) fn emit_status_tip(
        &self,
        text: String,
        x: i32,
        y: i32,
        caret_height: i32,
        duration_ms: u64,
        placement: StatusTipPlacement,
    ) {
        match &self.session_tx {
            Some(_) => self.emit_session(SessionEvent::Notice(text)),
            None => {
                let _ = self.ui_tx.send(UiCommand::ShowStatusTip {
                    text,
                    x,
                    y,
                    caret_height,
                    duration_ms,
                    placement,
                });
            }
        }
    }

    /// 输入状态。桌面：`UpdateToolbar`。`latin_input` 由调用方在**取 state 锁之前**算好。
    pub(crate) fn emit_status(&self, tb: ToolbarState, latin_input: bool) {
        match &self.session_tx {
            Some(_) => self.emit_session(SessionEvent::Status(SessionStatus {
                chinese_mode: tb.chinese_mode,
                latin_input,
                icon_label: tb.icon_label,
                full_width: tb.full_width,
                chinese_punct: tb.chinese_punct,
                s2t_enabled: tb.s2t_enabled,
                s2t_shown: tb.s2t_shown,
            })),
            None => {
                let _ = self.ui_tx.send(UiCommand::UpdateToolbar(tb));
            }
        }
    }

    /// 真有文本上屏：喂皮肤装饰的 `commit` 事件。只有桌面画皮肤，中立通道不发。
    pub(crate) fn emit_skin_commit(&self) {
        if self.session_tx.is_none() {
            let _ = self.ui_tx.send(UiCommand::SkinSignal(SkinSignal::Commit));
        }
    }

    /// 组合区/候选页快照（仅中立通道；桌面的 `UpdateCandidates` 在 `notify_ui_update` 里）。
    ///
    /// 页码、选中项与桌面同源：`page_range` / `grid_visible_range` 定页，`frame_selected`
    /// 定高亮，不另写一份。
    pub(crate) fn emit_view(&self, state: &State, mode_label: String) {
        let grid_range = self.grid_visible_range(state);
        let (start, end) = grid_range.unwrap_or_else(|| self.page_range(state));
        let n_items = end - start;
        let per_page = self.per_page(state.active);
        let grid_index = self.grid_index_of(state, grid_range);
        let highlight_row = state
            .grid_top
            .zip(grid_index)
            .map(|(top, _)| state.current_page.saturating_sub(top));
        let (preedit, preedit_caret) = self.display_preedit(state);
        self.emit_session(SessionEvent::View(CompositionView {
            composing: composing_of(state),
            preedit,
            preedit_caret,
            mode_label,
            has_candidates: n_items > 0,
            selected: frame_selected(state, per_page, highlight_row, n_items),
            page: state.current_page + 1,
            total_pages: self.total_pages(state),
        }));
    }

    /// 直接发一帧（加词面板这类不由 `State` 推出的画面）。
    pub(crate) fn emit_view_frame(&self, view: CompositionView) {
        self.emit_session(SessionEvent::View(view));
    }

    /// 组合区显示串与插入符字节偏移：套上音节分隔符的显示替换
    /// （`ui.candidate.syllable_separator`，GH#267）。光标是字节偏移，换算时按前缀整体
    /// 替换后的长度重算。桌面 `UpdateCandidates` 与中立通道共用。
    pub(crate) fn display_preedit(&self, state: &State) -> (String, usize) {
        let mut preedit = state.preedit.clone();
        let mut preedit_caret = self.ui_caret_bytes(state).min(preedit.len());
        if let Some(sep) = self.syllable_separator_for(state.active.is_none())
            && preedit.contains('\'')
        {
            let head =
                wind_bridge::handler::apply_syllable_separator(&preedit[..preedit_caret], &sep);
            preedit = wind_bridge::handler::apply_syllable_separator(&preedit, &sep);
            preedit_caret = head.len().min(preedit.len());
        }
        (preedit, preedit_caret)
    }

    /// 当前是否在「打字中」。**自己取 state 锁**，持锁路径别调。
    pub fn is_composing(&self) -> bool {
        composing_of(&self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// 键是否按拉丁字母直出。**自己取 state 锁**（`effective_input_block` 也读 state，
    /// 所以先后取、不嵌套），持锁路径别调。
    pub fn latin_input(&self) -> bool {
        let input_blocked = self.effective_input_block().shows_english();
        let chinese_mode = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .chinese_mode;
        self.latin_input_with(input_blocked, chinese_mode)
    }

    /// [`Self::latin_input`] 的纯计算部分：不取 state 锁，调用方给齐两个已读好的值。
    ///
    /// 不看 `icon_label`：它用户可配（`[ui.labels]`），把「英」改成「A」判据就失效。
    pub(crate) fn latin_input_with(&self, input_blocked: bool, chinese_mode: bool) -> bool {
        input_blocked || !chinese_mode || self.engine_mgr.active_is_english()
    }
}

/// 打字中：有组合区内容、不是联想标识，且确有输入在进行。
///
/// 第三项是为回车等上屏出口补的：它们只清 `input_buffer` 和候选，**不清 `preedit`**
/// （`message_handler.rs` 的 Enter/Space 上屏分支），残留的 `preedit` 不能让状态卡在 true。
/// 特殊模式（`active` 有值，临时拼音/临英等）下 `input_buffer` 可能为空而编码栏只有引导符，
/// 那仍是打字中，所以是 `active.is_some() || 缓冲非空`，而不是单看 `input_buffer`。
pub(crate) fn composing_of(state: &State) -> bool {
    !state.preedit.is_empty()
        && !state.assoc_active()
        && (state.active.is_some() || !state.input_buffer.is_empty())
}

/// 帧里的高亮下标。展开态的选中项是平铺下标：高亮行之前的整行都算进去。
/// 桌面 `UpdateCandidates` 与 [`SessionEvent::View`] 共用，两边必须同值。
pub(crate) fn frame_selected(
    state: &State,
    per_page: usize,
    highlight_row: Option<usize>,
    n_items: usize,
) -> usize {
    let selected_flat = match highlight_row {
        Some(row) => row * per_page + state.selected_index,
        None => state.selected_index,
    };
    selected_flat.min(n_items.saturating_sub(1))
}
