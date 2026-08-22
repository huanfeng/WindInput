//! 辅助码输入模式：拼音候选的字形二次筛选。
//!
//! 触发：`session_actions` 绑 `"aux_code"`（或 `"page_next_aux_code"` 共键），空缓冲时**不进**
//! （无候选可筛，触发键落普通标点），组码中按下 → 进入并原地筛选候选。
//!
//! 设计要点（对齐 docs 中已拍板的决策）：
//! - **不进即不动**：进入不改候选顺序；辅助码只筛选（通过 `CandidateStore::set_filter`
//!   从快照重筛，被保留候选保持原序）。
//! - 会话自洽：进入时把原始候选快照进 [`wind_aux_code::AuxCodeSession`]（引擎 convert
//!   结果的单槽 memo），每次筛选都从快照重筛；**被滤候选直接丢弃**——辅助码是字形二次
//!   筛选，候选窗只显示命中者（如 `om` 配「时间」「实践」时实践消失），还原不靠残留
//!   标记，快照在手、退出/退格都从快照恢复。
//! - 独占输入流：辅助码输入期间无法同时打拼音；Esc → 退出并还原拼音组合，Backspace 删空
//!   → 保持空码态（候选还原，方便重新输入），空码再退格 → 退出还原拼音；Space/Enter/数字
//!   选词 → 正常上屏后退出。
//! - **连续组句**：选中只消费缓冲前缀的字词（逐步转换态，缓冲还有剩余）时**不退出**
//!   辅助码模式——重建会话快照、清空辅助码缓冲，继续筛下一段（如「没时间」：先 `n` 筛出
//!   没 选中、再 `ss` 筛出 时间 选中，全程留在辅助码模式，直到选中消费整串才退出上屏）。
//! - 组合区 = 显示前缀（进入时拼一次：拼音基线 + 4 空格，右对齐为后续美化项）+ 辅助码缓冲。
//!   显示前缀存 [`AuxCodeOverlay::preedit_prefix`]（与筛选会话、显示基线同生共死），
//!   刷新组合区与 overlay 光标共用，分隔符只写一遍。
//!
//! 会话筛选状态（快照/缓冲/重筛）在 `wind-aux-code::session`；显示态（preedit/光标）是
//! 协调器职责，与筛选会话一起打包在 [`AuxCodeOverlay`]（`State.aux_code`）。本模块负责
//! 协调器侧的按键路由、模式进出与 UI 更新。

use crate::coordinator::{Coordinator, State};
use crate::pipeline::ModeKind;
use tracing::{debug, info};
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::Candidate;
use wind_ipc::protocol::{MOD_ALT, MOD_CTRL, MOD_SHIFT};
use wind_keys::keymap;

/// 辅助码 overlay 的协调器侧状态三件套（筛选会话 + 显示基线 + 显示前缀）。
///
/// 三者**同生共死**：enter 一次全建，退出/上屏/复位一律整体 `take`/`None`——
/// 打包成单个 `Option` 后不存在「只清其中一个」的路径（字段分散在 `State` 时
/// 曾有三处各自 `clear` 的漂移风险）。
///
/// - `session`：纯筛选会话（`wind-aux-code::AuxCodeSession`），不含显示态。
/// - `preedit_base`：进入前的拼音显示基线，退出时还原。
/// - `preedit_prefix`：进入时拼好的显示前缀 = 基线 + 4 空格（分隔符只在此写一遍），
///   刷新组合区与 overlay 光标共用。
pub(crate) struct AuxCodeOverlay {
    pub(crate) session: wind_aux_code::AuxCodeSession,
    pub(crate) preedit_base: String,
    pub(crate) preedit_prefix: String,
    /// 本次会话的筛选选项：进入时按方案 `[engine.aux_code].max_phrase_len` 固化
    /// （`AuxCodeFilterOptions` 其余取默认：逐字首码匹配固定语义），期间方案切换不可见。
    pub(crate) filter_options: wind_aux_code::AuxCodeFilterOptions,
}

/// 辅助码进入来源（决定是否保留翻页位置）。
///
/// 两种路径共用同一门卫和核心逻辑，差异仅「从翻页键进入时需保留当前页码」。
pub(crate) enum AuxCodeTrigger {
    /// 从 `session_actions` 的 `aux_code` 绑定直接触发。
    Direct,
    /// 从共享翻页键触发（`apply_session_action` 已翻页，需保留 `current_page`）。
    FromPage,
}

impl std::fmt::Display for AuxCodeTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuxCodeTrigger::Direct => write!(f, "direct trigger"),
            AuxCodeTrigger::FromPage => write!(f, "from page turn"),
        }
    }
}

/// 辅助码会话（快照/缓冲/重筛）已移入 `wind-aux-code::session`，见
/// [`wind_aux_code::AuxCodeSession`]；显示态（组合区 preedit/光标）与筛选会话打包在
/// [`AuxCodeOverlay`]，经 `State.aux_code` 整体持有、整体销毁。
impl Coordinator {
    /// 首次辅助码输入时懒加载辅助码表：`load_merged` 合并所有已解析路径
    /// （先出现 = 高优），已加载过则 no-op。空表语义由 wind-aux-code 内部处理（passthrough）。
    pub(crate) fn ensure_aux_code_table(&self, paths: &[std::path::PathBuf]) {
        let mut table = self
            .aux_code_table
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if table.is_none() {
            let merged = wind_aux_code::load_merged(paths);
            let n = merged.char_count();
            *table = Some(merged);
            if n > 0 {
                info!("Loaded aux-code table ({} chars)", n);
            }
        }
    }

    /// 进入辅助码模式。门卫没过返回 `None` 不吞键（与各模式进入门卫同策略）：
    /// - 功能未启用（`[schema.pinyin.aux_code].enabled` 折叠方案覆盖后为 false，**出厂即此**）
    /// - 方案未配 `[engine.aux_code].files` 或码表文件全部缺失
    /// - 当前无候选（没有可筛的东西；空缓冲下触发键落普通标点流程）
    ///
    /// `trigger` 区分两种进入路径：
    /// - [`AuxCodeTrigger::Direct`]：从 `session_actions` 的 `aux_code` 绑定直接触发。
    /// - [`AuxCodeTrigger::FromPage`]：从共键（`page_next_aux_code`）触发，`apply_session_action`
    ///   已翻页，
    ///   需保存并恢复 `current_page`，使辅助码模式从翻到的页开始筛选）。
    pub(crate) fn enter_aux_code(
        &self,
        state: &mut State,
        trigger: AuxCodeTrigger,
    ) -> Option<KeyAction> {
        // ★★ 只能从**主输入路**进入：本模式筛的是主路候选，而各 overlay 模式（特殊模式、
        // 临拼、临英、URL、以及辅助码自身）有自己的候选面与生命周期。
        //
        // ⚠️ 这道守卫在 `apply_session_action` 收下 `SessionAction::AuxCode` 之后才成为
        // 必需：`handle_candidate_nav` 被**五个** overlay 共用（辅助码自身、特殊模式、
        // 临拼、临英、URL），它们都会把按键送回 `apply_session_action`，而它现在认得
        // AuxCode ⇒ 触发键在这些模式里会重入、夺走那个模式的候选并改写 preedit。
        //
        // 少了它的实际症状（2026-08-22 真机）：辅助码态再按一次 Tab → 重入 →
        // `preedit_prefix` 每次在旧 preedit 上再拼 4 个空格，组合区里长出一段越来越宽的
        // 空白，看起来像插进了一个宽制表符。
        //
        // 判据是「任一活跃 overlay 都不让进」（`is_some()` 而非只挡 AuxCode）：临时英文 /
        // 临拼等模式下按到辅助码触发键时应保持原模式，而非把主候选夺来筛一遍。
        // 共享键的 FromPage 路径只在主输入路（`active == None`）到达此处，故不受此影响。
        if state.active.is_some() {
            return None;
        }
        let settings = self.engine_mgr.aux_code_settings();
        if !settings.enabled {
            return None;
        }
        let paths = settings.files;
        if paths.is_empty() || state.candidates.is_empty() {
            return None;
        }
        let preserve_page = matches!(trigger, AuxCodeTrigger::FromPage);
        let saved_page = preserve_page.then_some(state.current_page);
        let act = self.init_aux_overlay(state, &paths, settings.max_phrase_len);
        if let Some(page) = saved_page {
            state.current_page = page;
        }
        self.notify_ui_update(state);
        debug!(
            "aux_code: entered ({trigger}), {} candidates",
            state.candidates.len()
        );
        Some(act)
    }

    /// 这个键是不是**专用**辅助码触发键（只绑 `session_actions.aux_code`、不带翻页）。
    ///
    /// 与按键分派**同一真相源**（复用 `session_action_for`），不另写平行逻辑。
    ///
    /// - 字母（`A`..`Z`）恒是辅助码码元，绝不当触发键。
    /// - 共键情形（`page_next_aux_code`）在【进入侧】由 `apply_session_action` 一并处理，
    ///   不在此判定；此处只服务于辅助码态内的「专用触发键静默消费」。
    pub(crate) fn is_dedicated_aux_trigger(&self, key_code: u32, shift: bool) -> bool {
        // 字母恒是码元，绝不当触发键。区间与 `handle_aux_code_key` 里那条累积臂
        // （`VK_A..=VK_Z`）**取同一个**，两处不会漂。
        if (keymap::VK_A..=keymap::VK_Z).contains(&key_code) {
            return false;
        }
        self.session_action_for(key_code, shift, false) == Some(wind_config::SessionAction::AuxCode)
    }

    /// 创建辅助码 overlay 并激活辅助码模式（`enter_aux_code` 的公共逻辑）。
    ///
    /// **不发送 UI 更新**：调用方负责在状态完全就绪后统一调用 `notify_ui_update`。
    /// 这样共享键路径可以在翻页后保存/恢复 `current_page`，只发一次通知，避免闪烁。
    fn init_aux_overlay(
        &self,
        state: &mut State,
        paths: &[std::path::PathBuf],
        max_phrase_len: usize,
    ) -> KeyAction {
        self.ensure_aux_code_table(paths);
        // 三件套整体建立：筛选会话（快照原始候选，后续筛选都从它重筛）+ 显示基线
        // （进入前的拼音显示，退出还原）+ 显示前缀（基线 + 分隔符，进入拼一次）。
        // 此后三者同生共死，退出/上屏/复位一律整体销毁，见 `AuxCodeOverlay`。
        let session = wind_aux_code::AuxCodeSession::new(std::mem::take(&mut state.candidates));
        let preedit_base = std::mem::take(&mut state.preedit);
        let preedit_prefix = format!("{}    ", preedit_base);
        // 筛选选项按本次进入时的生效设置固化（期间方案切换不可见）。
        // 词组逐字首码匹配是固定语义，无模式选项（`AuxCodeFilterOptions` 其余取默认）。
        let filter_options = wind_aux_code::AuxCodeFilterOptions { max_phrase_len };
        state.aux_code = Some(AuxCodeOverlay {
            session,
            preedit_base,
            preedit_prefix,
            filter_options,
        });
        state.active = Some(ModeKind::AuxCode);
        self.refresh_aux_code_candidates(state);
        let display = state.preedit.clone();
        let caret_pos = self.overlay_caret(state);
        // ★ 不在此处 notify_ui_update —— 由调用方在状态完全就绪后统一发送。
        KeyAction::UpdateComposition {
            text: display,
            caret_pos,
        }
    }

    /// 辅助码模式下的翻页/导航处理，含共享键自动退出逻辑。
    ///
    /// - PagePrev 翻页成功（从非首页回到首页） + 辅助码缓冲为空 → 自动退出辅助码模式。
    /// - PagePrev 在首页无效果（只有一 / 从未翻过页） → 不退出，留在辅助码模式。
    /// - PageNext / 高亮 → 正常导航，不触发退出。
    ///
    /// 不改 `apply_session_action` 签名，不影响其他调用点。
    pub(crate) fn handle_candidate_nav_or_auto_exit(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        let shift = data.modifiers & MOD_SHIFT != 0;
        let action = self.rt().session_keys.classify(data.key_code, shift, true);
        match action {
            Some(wind_config::SessionAction::PagePrev) => {
                let at_first_page_before = state.current_page == 0;
                if !self.page_prev(state) {
                    // page_prev 在首页无效果（只有一/从未翻过页）→ 不退出，留在辅助码模式
                    return Some(KeyAction::Consumed);
                }
                self.notify_ui_update(state);
                // 翻页成功且刚回到首页 + 辅助码缓冲为空 → 自动退出
                if state.current_page == 0
                    && !at_first_page_before
                    && state
                        .aux_code
                        .as_ref()
                        .is_some_and(|o| o.session.is_empty())
                {
                    self.exit_aux_code(state);
                    return Some(KeyAction::UpdateComposition {
                        text: state.preedit.clone(),
                        caret_pos: self.overlay_caret(state),
                    });
                }
                Some(KeyAction::Consumed)
            }
            Some(
                wind_config::SessionAction::PageNext | wind_config::SessionAction::PageNextAuxCode,
            ) => {
                if self.page_next(state) {
                    self.notify_ui_update(state);
                }
                Some(KeyAction::Consumed)
            }
            Some(
                wind_config::SessionAction::HighlightUp | wind_config::SessionAction::HighlightDown,
            ) => {
                // 高亮键（`include_printable=true`）— 委托给 handle_candidate_nav
                self.handle_candidate_nav(state, data)
            }
            // 取消键：辅助码态下等同 Esc，整体放弃组合（ClearComposition）。
            // 此前 `_ => None` 把取消键漏给了兜底臂，会**上屏高亮候选**而非取消，是回退引入的缺陷。
            // 辅助码态恒有输入会话（active 非空），`cancel_session` 必走取消分支。
            Some(wind_config::SessionAction::Cancel) => Some(self.cancel_session(state)),
            _ => None,
        }
    }

    /// 刷新辅助码候选：按会话内辅助码缓冲对**原始候选快照**重筛，只保留命中者
    /// （被滤候选直接丢弃，候选窗只显示匹配词）。空缓冲 / 空表由 wind-aux-code 内部
    /// passthrough。同步重拼组合区 = 显示前缀 + 辅助码缓冲。
    pub(crate) fn refresh_aux_code_candidates(&self, state: &mut State) {
        if state.aux_code.is_none() {
            return;
        }
        // 候选重筛 = 列表重新装填：翻页/高亮/悬停复位到页首（对齐 `reset_candidate_view`
        // 契约，否则筛选后高亮会停在原地、可能指向已沉底的被滤候选）。
        // 先 reset（取 `&mut state`）再取 overlay 借用：`is_none` 守卫已保证非 None。
        self.reset_candidate_view(state);
        let overlay = state.aux_code.as_mut().expect("辅助码模式必持 overlay");
        let table = self
            .aux_code_table
            .read()
            .unwrap_or_else(|e| e.into_inner());
        // 空表 = 未加载（防御语义：不过滤，还原快照，见 wind-aux-code）。
        state.candidates = match table.as_ref() {
            Some(t) => overlay.session.apply(t, &overlay.filter_options),
            None => overlay.session.restore_original(),
        };
        // 组合区 = 显示前缀（进入时拼好：基线 + 4 空格）+ 辅助码缓冲。
        state.preedit = format!("{}{}", overlay.preedit_prefix, overlay.session.buffer());
    }

    /// 退出辅助码：还原拼音组合（候选恢复会话快照原样、preedit 回到拼音显示）。
    /// 刻意**不** `ClearComposition`——辅助码只是筛选，退出的语义是「放弃筛选、继续拼音」，
    /// 而非放弃整个组合。调用方据此返回 `UpdateComposition`。
    pub(crate) fn exit_aux_code(&self, state: &mut State) {
        let Some(mut overlay) = state.aux_code.take() else {
            return;
        };
        state.active = None;
        state.candidates = overlay.session.restore_original();
        state.preedit = overlay.preedit_base;
        debug!("aux_code: exited");
    }

    /// 选词上屏后的辅助码收尾：仅清模式态，**不碰 preedit/候选**——上屏路径
    /// （`commit_selected`）已把组合区与候选重算好（分步确认则继续拼音、完整上屏则清空）。
    pub(crate) fn finish_aux_code_after_commit(&self, state: &mut State) {
        state.active = None;
        state.aux_code = None;
    }

    /// 辅助码模式下的按键处理。
    pub(crate) fn handle_aux_code_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        // Ctrl/Alt 组合守卫：必须最先，否则组合键会落到下方各臂被当普通辅助码输入。
        let has_input = state
            .aux_code
            .as_ref()
            .is_some_and(|o| !o.session.is_empty())
            || !state.input_buffer.is_empty()
            || !state.committed_text.is_empty();
        if let Some(act) =
            self.overlay_ctrl_alt_guard(state, data, has_input, |s, st| s.exit_aux_code(st))
        {
            return act;
        }
        // 专用触发键（只绑 `aux_code`、不带翻页）在辅助码态内 → 静默消费（Consumed），**不退出**。
        // 共键键（`page_next_aux_code`）不在此列，它落到下方导航分支正常翻页。
        // 退出辅助码模式**只**有三条路：Esc、空码退格、翻回首页（自动退出）。本判断必须排在
        // `handle_candidate_nav` / 兜底臂之前：否则触发键会落到下方兜底臂**上屏高亮候选**，
        // 那是破坏性动作。
        if self.is_dedicated_aux_trigger(data.key_code, data.modifiers & MOD_SHIFT != 0) {
            return KeyAction::Consumed;
        }
        // 候选导航（翻页 / 高亮）：辅助码只收字母，`-`/`=`/`[`/`]` 等按普通导航处理。
        // 共享键在第一页边界时自动退出辅助码模式（见 `handle_candidate_nav_or_auto_exit`）。
        if let Some(act) = self.handle_candidate_nav_or_auto_exit(state, data) {
            return act;
        }
        match data.key_code {
            // Esc：放弃辅助码、还原拼音组合（不走 cancel_session——那会 ClearComposition
            // 把整个拼音组合也放弃，与「辅助码只是筛选」的语义不符）。
            keymap::VK_ESCAPE => self.aux_code_exited(state),
            keymap::VK_BACK => {
                // Backspace：删一个辅助码字符。删到空（错误码清空后）**保持空码态**便于
                // 重输；缓冲已空时再退格 → 退出还原拼音组合。
                let popped = state
                    .aux_code
                    .as_mut()
                    .is_some_and(|o| o.session.pop_char().is_some());
                if popped {
                    self.refresh_aux_code_candidates(state);
                    self.aux_code_updated(state)
                } else {
                    self.aux_code_exited(state)
                }
            }
            keymap::VK_SPACE | keymap::VK_RETURN => {
                // 空格/回车：选当前高亮候选（正常拼音上屏路径），然后退出辅助码。
                if let Some((cand, offset)) = self.highlighted_candidate(state) {
                    self.aux_code_committed(state, cand, offset)
                } else {
                    self.aux_code_exited(state)
                }
            }
            keymap::VK_1..=keymap::VK_9 if data.modifiers & MOD_SHIFT == 0 => {
                // 数字选当前页第 N 个。
                let (start, end) = self.page_range(state);
                let idx = start + (data.key_code - keymap::VK_1) as usize;
                if idx < end {
                    let cand = state.candidates[idx].clone();
                    self.aux_code_committed(state, cand, (data.key_code - keymap::VK_1) as i32)
                } else {
                    KeyAction::Consumed
                }
            }
            // 字母累积辅助码（小写化；与拼音/临拼同，Shift 不影响字符形态）。
            keymap::VK_A..=keymap::VK_Z if data.modifiers & (MOD_CTRL | MOD_ALT) == 0 => {
                let ch = (b'a' + (data.key_code - keymap::VK_A) as u8) as char;
                if let Some(o) = &mut state.aux_code {
                    o.session.push_char(ch);
                }
                self.refresh_aux_code_candidates(state);
                self.aux_code_updated(state)
            }
            _ => {
                // 二三候选键（`;`/`,` 等可打印选词键组，keydown 消费）。
                if data.modifiers & MOD_SHIFT == 0
                    && let Some(offset) = self.select_key_offset(data.key_code)
                {
                    let (start, end) = self.page_range(state);
                    let idx = start + offset;
                    if idx < end {
                        let cand = state.candidates[idx].clone();
                        return self.aux_code_committed(state, cand, offset as i32);
                    }
                }
                // 其余键：有候选则上屏高亮候选并退出；否则退出还原拼音。
                if let Some((cand, offset)) = self.highlighted_candidate(state) {
                    self.aux_code_committed(state, cand, offset)
                } else {
                    self.aux_code_exited(state)
                }
            }
        }
    }

    /// 组合区随辅助码更新：通知 UI 并回组合更新（光标在辅助码串尾）。
    fn aux_code_updated(&self, state: &mut State) -> KeyAction {
        let display = state.preedit.clone();
        let caret_pos = self.overlay_caret(state);
        self.notify_ui_update(state);
        KeyAction::UpdateComposition {
            text: display,
            caret_pos,
        }
    }

    /// 已退出辅助码、还原拼音组合区：通知 UI 并回组合更新（光标回到拼音串尾）。
    fn aux_code_exited(&self, state: &mut State) -> KeyAction {
        self.exit_aux_code(state);
        let display = state.preedit.clone();
        let caret_pos = self.composition_caret(state);
        self.notify_ui_update(state);
        KeyAction::UpdateComposition {
            text: display,
            caret_pos,
        }
    }

    /// 上屏选中候选并结束辅助码会话（commit 路径已重算组合区/候选）。
    ///
    /// **连续组句**：候选只消费缓冲前缀（`commit_selected` 走逐步转换分支、缓冲还有剩余）
    /// 时**不退出**辅助码模式——重建会话快照继续筛选下一段（如「没时间」：先 `n` 筛出 没
    /// 选中，再 `ss` 筛出 时间 选中，全程留在辅助码模式）；完整消费（整串上屏）才退出。
    ///
    /// ★ **辅助码的三条选词路径必须全部走这里**：键盘（数字/空格/回车/二三候选键，见
    /// [`Self::handle_aux_code_key`]）、鼠标点选（`handle_candidate_click`）、修饰键作
    /// 二三候选键的 keyup（`select_page_candidate`）。少接一条的表现不是报错，而是
    /// 「按 `2` 能继续组句、轻敲 Shift 选同一个候选却退出了模式」——同一个动作换个键
    /// 换套语义，是最难被复现出来的那类缺陷。三条都收口在此，就不存在「只改对两条」。
    pub(crate) fn aux_code_committed(
        &self,
        state: &mut State,
        cand: Candidate,
        offset: i32,
    ) -> KeyAction {
        let act = self.commit_selected(state, &cand, offset);
        // 部分消费 → 缓冲还有剩余编码，处于逐步转换态 → 重建会话、留在辅助码模式。
        if !state.input_buffer.is_empty() {
            self.rearm_aux_code_session(state);
            let display = state.preedit.clone();
            let caret_pos = self.overlay_caret(state);
            self.notify_ui_update(state);
            return KeyAction::UpdateComposition {
                text: display,
                caret_pos,
            };
        }
        self.finish_aux_code_after_commit(state);
        act
    }

    /// 辅助码连续组句：部分消费后重建会话，继续在辅助码模式下筛选下一段。
    ///
    /// `commit_selected` 的逐步转换分支已把 `committed_text` 并入前缀、缓冲裁剪为剩余
    /// 编码，并调用 `update_candidates` 用引擎重转了剩余编码的候选。本函数据此：
    /// - 显示基线更新为「已转换前缀 + 剩余拼音」（与进入辅助码时的显示形态一致）；
    /// - 以引擎重转出的**新候选**为快照重建会话（辅助码缓冲清空，空码 = passthrough 全显），
    ///   下一段辅助码直接作用于这些候选。
    pub(crate) fn rearm_aux_code_session(&self, state: &mut State) {
        let overlay = state.aux_code.as_mut().expect("辅助码模式必持 overlay");
        let preedit_base = std::mem::take(&mut state.preedit);
        overlay.preedit_base = preedit_base.clone();
        overlay.preedit_prefix = format!("{}    ", preedit_base);
        overlay.session = wind_aux_code::AuxCodeSession::new(std::mem::take(&mut state.candidates));
        self.refresh_aux_code_candidates(state);
    }

    /// 当前高亮候选及其页内偏移（无候选 → `None`）。
    fn highlighted_candidate(&self, state: &State) -> Option<(Candidate, i32)> {
        if state.candidates.is_empty() {
            return None;
        }
        let (start, _) = self.page_range(state);
        let idx = (start + state.selected_index).min(state.candidates.len() - 1);
        Some((state.candidates[idx].clone(), (idx - start) as i32))
    }
}

#[cfg(test)]
mod tests {
    //! 辅助码流程的无头集成测试：方案 data_dir（含 `[engine.aux_code]` + `[key_actions]`）+
    //! 临时 store。headless 无引擎，故候选由测试直接装填，覆盖的是辅助码自身的进出/筛选/
    //! 还原/上屏语义（`aux_code_settings` 经真实方案文件解析，证明配置接线的正确性）。
    //!
    //! ⚠️ 多数用例的 fixture 方案写了 `enabled = true`——**出厂是关的**。开关本身的
    //! 三态行为（默认关 / 方案覆盖开 / 方案覆盖关）由 `aux_code_disabled_*` 那组用例
    //! 单独覆盖，别让「fixture 开着」把默认值回归掩盖掉。
    use super::*;
    use crate::coordinator::Coordinator;
    use std::sync::Arc;
    use wind_bridge::handler::{KeyEventData, MessageHandler};
    use wind_candidate::{Candidate, CandidateSource};
    use wind_config::Config;
    use wind_ipc::protocol::{MOD_ALT, MOD_CTRL};
    use wind_store::Store;

    /// 造一个含 pinyin 方案的 data_dir：`[engine.aux_code]` 指到测试小码表，backtick 绑辅助码。
    /// 方案段显式 `enabled = true`（出厂全局是 false，见模块头注释）。
    fn data_dir_with_aux(tag: &str) -> std::path::PathBuf {
        data_dir_with_aux_enabled(tag, Some(true))
    }

    /// 同上，但方案段的 `enabled` 可控：`None` = 不写这一行（回落全局基线）。
    fn data_dir_with_aux_enabled(tag: &str, enabled: Option<bool>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wind_aux_code_data_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let schemas = dir.join("schemas");
        let aux_code_dir = schemas.join("aux_code");
        std::fs::create_dir_all(&aux_code_dir).unwrap();
        let enabled_line = match enabled {
            Some(v) => format!("enabled = {v}\n"),
            None => String::new(),
        };
        std::fs::write(
            schemas.join("pinyin.schema.toml"),
            format!(
                "[schema]\nid = \"pinyin\"\nname = \"pinyin\"\n\
                 [engine]\ntype = \"pinyin\"\n\
                 [engine.aux_code]\nfiles = [\"aux_code/flypy_test.txt\"]\n{enabled_line}"
            ),
        )
        .unwrap();
        std::fs::write(aux_code_dir.join("flypy_test.txt"), "李=mz\n樱=my\n河=sk\n").unwrap();
        dir
    }

    fn coord_with(tag: &str) -> Arc<Coordinator> {
        coord_with_data(tag, data_dir_with_aux(tag))
    }

    fn coord_with_data(tag: &str, data_dir: std::path::PathBuf) -> Arc<Coordinator> {
        coord_with_data_cfg(tag, data_dir, |_| {})
    }

    /// 同上，但可改配置。出厂值恰好让某些差异不可见（如 `per_page_extended = 0` 时
    /// 两档相同），不显式配上就测不出档位选错。
    fn coord_with_data_cfg(
        tag: &str,
        data_dir: std::path::PathBuf,
        tweak: impl FnOnce(&mut Config),
    ) -> Arc<Coordinator> {
        let path = std::env::temp_dir().join(format!("wind_aux_code_{tag}.redb"));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let mut cfg = Config::default();
        cfg.schema.active = "pinyin".to_string();
        tweak(&mut cfg);
        Coordinator::new_headless_with_store(cfg, Some(&data_dir), store)
    }

    fn cand(text: &str) -> Candidate {
        Candidate {
            text: text.into(),
            source: CandidateSource::Pinyin,
            ..Default::default()
        }
    }

    /// 字母 VK（VK_A..=VK_Z 是区间端点常量，逐字命名的仅 A/Z）。
    fn vk_letter(c: char) -> u32 {
        keymap::VK_A + (c as u32 - 'A' as u32)
    }

    fn key(vk: u32, modifiers: u32) -> KeyEventData {
        KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers,
            event_type: 0,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
        }
    }

    /// 装填「拼音组合中」的初始状态：缓冲 + 候选（李/樱/河），高亮首选。
    fn seed_composition(
        c: &Arc<Coordinator>,
    ) -> std::sync::MutexGuard<'_, crate::coordinator::State> {
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        st.input_buffer = "li".to_string();
        st.candidates = vec![cand("李"), cand("樱"), cand("河")];
        st.selected_index = 0;
        st.current_page = 0;
        st.preedit = "li".to_string();
        st
    }

    #[test]
    fn enter_requires_candidates() {
        let c = coord_with("guard");
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        // 空候选（空缓冲场景）：门卫拦下，触发键不吞（返回 None → 落普通标点）。
        assert!(
            c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct)
                .is_none()
        );
        assert_eq!(st.active, None);
    }

    #[test]
    fn enter_sets_mode_and_preserves_pinyin_preedit() {
        let c = coord_with("enter");
        let mut st = seed_composition(&c);
        let act = c
            .enter_aux_code(&mut st, super::AuxCodeTrigger::Direct)
            .expect("有候选应进入");
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        let overlay = st.aux_code.as_ref().expect("辅助码 overlay 已建立");
        assert!(overlay.session.is_empty());
        assert_eq!(overlay.preedit_base, "li");
        // 组合区 = 拼音 + 4 空格 + 辅助码（空）。
        assert_eq!(st.preedit, "li    ");
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    #[test]
    fn aux_letters_filter_and_drop_tail() {
        let c = coord_with("filter");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        // 打 m：李(mz)/樱(my) 命中，河(sk) 被滤 → 直接丢弃，不在候选列表里。
        let act = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
        let texts: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["李", "樱"],
            "筛选后 = 命中子序列，被滤候选不再出现在候选窗"
        );
        assert_eq!(st.preedit, "li    m");
    }

    #[test]
    fn deeper_aux_code_narrows_again() {
        let c = coord_with("narrow");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        // 再打 y：只剩 樱(my)。
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Y'), 0));
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["樱"]);
        assert_eq!(st.preedit, "li    my");
    }

    #[test]
    fn esc_exits_and_restores_pinyin_composition() {
        let c = coord_with("esc");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_ESCAPE, 0));
        assert_eq!(st.active, None, "Esc 退出辅助码");
        assert_eq!(st.preedit, "li", "退出还原拼音组合区");
        assert!(
            st.aux_code.is_none(),
            "退出销毁整个 overlay（会话/基线/前缀）"
        );
        assert_eq!(st.candidates.len(), 3);
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    /// ★★ 自定义取消键（`session_actions` 里的 `cancel`）在辅助码态必须**连主组合一起
    /// 放弃**，不能只退筛选。
    ///
    /// `cancel_session` 末尾无条件 `notify_ui_hide` + `ClearComposition`，而
    /// `exit_aux_code` 是本仓唯一一个「退出后主组合仍存活」的退出函数（它按设计还原
    /// 拼音候选与 preedit）。两者直接拼在一起就自相矛盾：宿主收到「清掉组合」，协调器
    /// 这边 `input_buffer` 还是 `li`、候选还有三条——下一次敲 `a` 会让屏幕上凭空冒出
    /// `lia`。
    ///
    /// 判据取「协调器状态与 ClearComposition 相符」，**不是**看返回的 KeyAction：
    /// 那个变体修不修都是 ClearComposition，按它断言测不出任何东西。
    ///
    /// 与上面 `esc_exits_and_restores_pinyin_composition` 恰成对照：Esc 走
    /// `aux_code_exited`（还原拼音、返回 UpdateComposition），取消键走这里（整体放弃、
    /// 返回 ClearComposition）。两个动作语义不同，各走各的路。
    #[test]
    fn cancel_session_in_aux_mode_clears_whole_composition() {
        let c = coord_with("cancel_whole");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        assert_eq!(st.active, Some(ModeKind::AuxCode));

        let act = c.cancel_session(&mut st);
        assert!(matches!(act, KeyAction::ClearComposition));
        assert_eq!(st.active, None, "取消键应退出辅助码");
        assert!(st.aux_code.is_none(), "overlay 三件套应整体销毁");
        assert!(
            st.input_buffer.is_empty(),
            "编码缓冲必须清空——残留会让下一次按键在屏幕上补出旧内容"
        );
        assert!(
            st.preedit.is_empty(),
            "组合区必须清空，与 ClearComposition 相符"
        );
        assert!(
            st.candidates.is_empty(),
            "候选必须清空，UI 已被 notify_ui_hide 隐藏"
        );
    }

    #[test]
    fn backspace_to_empty_stays_for_reinput() {
        let c = coord_with("back");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
        // 删空后保持在辅助码态（空码、候选还原），方便重新输入。
        assert_eq!(st.active, Some(ModeKind::AuxCode), "删空后不退出辅助码");
        assert!(st.aux_code.as_ref().unwrap().session.is_empty());
        assert_eq!(st.candidates.len(), 3);
        assert_eq!(st.preedit, "li    ");
        // 直接再输辅助码即可重新筛选。
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["李", "樱"]);
        // 空码再退格 → 退出辅助码、还原拼音组合。
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
        assert_eq!(st.active, Some(ModeKind::AuxCode), "删空这一下保持空码态");
        let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
        assert_eq!(st.active, None, "空码再退格退出辅助码");
        assert_eq!(st.preedit, "li");
        assert!(
            st.aux_code.is_none(),
            "退格退出销毁整个 overlay（会话/基线/前缀）"
        );
        assert_eq!(st.candidates.len(), 3);
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    #[test]
    fn backspace_without_input_exits_aux_mode() {
        let c = coord_with("back_no_input");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        // 未输入任何辅助码，直接退格 → 退出辅助码模式。
        let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
        assert_eq!(st.active, None, "无输入退格退出辅助码");
        assert!(st.aux_code.is_none());
        assert_eq!(st.preedit, "li");
        assert_eq!(st.candidates.len(), 3);
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    #[test]
    fn backspace_restores_previous_filter_level() {
        let c = coord_with("prev");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        // my → 只剩 樱。
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Y'), 0));
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["樱"]);
        // 退格删 y → 回到 m 的筛选层（还原到"之前的状态"）。
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["李", "樱"], "退格还原到上一层筛选结果");
        assert_eq!(st.preedit, "li    m");
        // 再退格删空 → 保持辅助码态，候选全部还原，组合区回空码显示。
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
        assert_eq!(st.active, Some(ModeKind::AuxCode), "删空后不退出");
        assert_eq!(st.candidates.len(), 3);
        assert_eq!(st.preedit, "li    ");
    }

    #[test]
    fn wrong_aux_code_backspace_restores_all() {
        let c = coord_with("wrong");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        // 错误辅助码 zz：一个都匹配不上，候选列表清空（被滤候选直接丢弃）。
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
        assert!(
            st.candidates.is_empty(),
            "错误码下候选窗为空（全部被滤丢弃）"
        );
        // 删一个 → 仍在辅助码态（zz → z，照样全滤）。
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        assert!(
            st.candidates.is_empty(),
            "z 仍匹配不上任何候选，列表保持为空"
        );
        // 删空 → 保持辅助码态，候选栏还原到没筛选的原列表（顺序一致），便于重输。
        let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
        assert_eq!(st.active, Some(ModeKind::AuxCode), "删空后不退出辅助码");
        let texts: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["李", "樱", "河"]);
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    #[test]
    fn space_commits_highlighted_and_exits() {
        let c = coord_with("space");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        // 首选仍是「李」（选中下标 0 未移动，排序保持子序列）。
        let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_SPACE, 0));
        assert_eq!(st.active, None, "上屏后退出辅助码");
        assert!(
            st.aux_code.is_none(),
            "上屏后销毁整个 overlay（会话/基线/前缀）"
        );
        assert!(matches!(act, KeyAction::InsertText { .. }));
    }

    #[test]
    fn ctrl_alt_guard_does_not_consume_as_code() {
        let c = coord_with("ctrl");
        {
            let mut st = seed_composition(&c);
            let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
            // Ctrl+M 不被当辅助码字符：有输入态 → ClearComposition 退出。
            let act = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), MOD_CTRL));
            assert_eq!(st.active, None);
            assert!(matches!(act, KeyAction::ClearComposition));
        }
        // 无输入态 → PassThrough。
        let mut st2 = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st2, super::AuxCodeTrigger::Direct);
        st2.input_buffer.clear();
        st2.committed_text.clear();
        let act = c.handle_aux_code_key(&mut st2, &key(vk_letter('M'), MOD_ALT));
        assert!(matches!(act, KeyAction::PassThrough));
    }

    /// 方案切换会失效辅助码表缓存：切方案后再次进入辅助码必须按新表重筛。
    /// 码表缓存是**全局一份**、不区分方案，而各方案码表不同（拼音笔画表 vs 双拼小鹤
    /// 全码表）——切方案若不清缓存，双拼会一直用拼音那份表。本测试用「重写码表文件 +
    /// `invalidate_aux_code_table`」模拟切方案钩子的行为，验证重挂生效。
    #[test]
    fn schema_switch_invalidates_aux_code_table() {
        let c = coord_with("switch");
        let aux_file = std::env::temp_dir()
            .join("wind_aux_code_data_switch")
            .join("schemas/aux_code/flypy_test.txt");
        let mut st = seed_composition(&c);
        // 首次进入：载入旧表（李=mz/樱=my/河=sk），打 m 命中李+樱。
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["李", "樱"], "旧表：m 命中李/樱");
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_ESCAPE, 0));
        // 模拟方案切换后码表被替换：写新表（李=zz/樱=zy/河=zk，首行换名）。
        std::fs::write(&aux_file, "# name: 新表\n李=zz\n樱=zy\n河=zk\n").unwrap();
        c.invalidate_aux_code_table(); // 切方案钩子置 None，下次进入重挂
        // 再次进入：缓存若没失效会沿用旧表（打 m 仍命中）；失效后按新表（全滤为空）。
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        assert!(
            st.candidates.is_empty(),
            "切方案后必须重挂新表：m 不再命中任何候选"
        );
    }

    /// 连续组句：选中只消费缓冲前缀的字词（部分消费）→ **不退出**辅助码模式，重建会话
    /// 继续筛下一段。用「李」consumed_length=1 < 缓冲 "li"(2) 制造逐步转换态；headless
    /// 无引擎，重转剩余编码得空候选，但模式态 / 前缀推进 / 会话重建必须正确。
    #[test]
    fn partial_commit_stays_in_aux_mode_for_continuous_filter() {
        let c = coord_with("continuous");
        let mut st = seed_composition(&c);
        st.candidates[0].consumed_length = 1; // 李 只消费 li 的前 1 码
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0)); // 筛 m → 李/樱
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        // 空格选 李：部分消费 → 逐步转换 + 重建会话，留在辅助码模式。
        let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_SPACE, 0));
        assert_eq!(
            st.active,
            Some(ModeKind::AuxCode),
            "部分消费后不退出辅助码，继续筛下一段"
        );
        assert!(st.aux_code.is_some(), "overlay 仍在（重建的会话）");
        assert_eq!(st.committed_text, "李", "逐步转换：已转换前缀并入");
        assert_eq!(st.input_buffer, "i", "缓冲裁剪为剩余编码");
        assert!(
            st.aux_code.as_ref().unwrap().session.is_empty(),
            "重建会话辅助码缓冲清空，可继续输入下一段辅助码"
        );
        assert!(
            st.preedit.starts_with("李i"),
            "组合区 = 已转换前缀 + 剩余拼音 + 分隔符前缀"
        );
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    /// ★★ 第三条选词路径（`select_page_candidate`）也要留在模式内。
    ///
    /// 它是「修饰键作二三候选键」的 keyup 落点（`handle_select_key_up` →
    /// `select_page_candidate`）。此前那一臂是 `commit_selected` + **无条件**
    /// `finish_aux_code_after_commit`，于是配了 `select_key_groups = lrshift` 的用户
    /// 打「没时间」时：按 `2` 能继续组句，轻敲 Shift 选同一个候选却直接退出辅助码——
    /// 同一个动作换个键换套语义。
    ///
    /// 判据取「`active` 仍是 AuxCode 且缓冲已裁剪」，不是看返回的 `KeyAction` 变体：
    /// 留在模式内与退出模式都返回 `UpdateComposition`，按变体断言测不出任何东西。
    #[test]
    fn select_page_candidate_partial_commit_stays_in_aux_mode() {
        let c = coord_with("continuous_selectkey");
        let mut st = seed_composition(&c);
        st.candidates[0].consumed_length = 1; // 李 只消费 li 的前 1 码
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0)); // 筛 m → 李/樱
        assert_eq!(st.active, Some(ModeKind::AuxCode));

        // offset 0 = 首选（李）。走的是与键盘数字键**不同**的那条路径。
        let act = c
            .select_page_candidate(&mut st, 0)
            .expect("页内有候选，不应越界");
        assert_eq!(
            st.active,
            Some(ModeKind::AuxCode),
            "部分消费后不得退出辅助码——否则与按 `2` 选同一个候选结果不同"
        );
        assert!(st.aux_code.is_some(), "overlay 三件套应已重建");
        assert_eq!(st.committed_text, "李");
        assert_eq!(st.input_buffer, "i", "缓冲应裁剪为剩余编码");
        assert!(
            st.aux_code.as_ref().unwrap().session.is_empty(),
            "重建会话的辅助码缓冲应清空"
        );
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    /// 同一条路径的反向对照：完整消费仍须退出，别把「不退出」写成无条件的。
    #[test]
    fn select_page_candidate_full_commit_exits_aux_mode() {
        let c = coord_with("continuous_selectkey_exit");
        let mut st = seed_composition(&c);
        // consumed_length 全 0 = 整串消费。
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.select_page_candidate(&mut st, 0).expect("页内有候选");
        assert_eq!(st.active, None, "整串消费仍退出辅助码");
        assert!(st.aux_code.is_none(), "overlay 三件套应整体销毁");
    }

    /// 完整消费（候选消费整串）→ 照常退出辅助码：连续组句只在逐步转换态续命，
    /// 最后一段选中即整体上屏并退出（对齐 `space_commits_highlighted_and_exits`）。
    #[test]
    fn full_commit_still_exits_aux_mode() {
        let c = coord_with("continuous_exit");
        let mut st = seed_composition(&c);
        // 全部候选 consumed_length=0（未标注 = 整串消费）→ 空格选中即完整上屏退出。
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_SPACE, 0));
        assert_eq!(st.active, None, "整串消费仍退出辅助码");
        assert!(st.aux_code.is_none());
        assert!(matches!(act, KeyAction::InsertText { .. }));
    }

    /// 回归：辅助码模式下按 ↓ 触发的「翻页放宽」（`expand_candidates`）不得把未过滤的
    /// 整池候选塞回来——过滤结果必须保持（`has_more`/`candidate_input` 满足扩展条件时，
    /// 无守卫会重建整池、丢失筛选）。
    #[test]
    fn arrow_navigation_does_not_lose_filter() {
        let c = coord_with("nav");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        // 筛 m → [李, 樱]，并制造「池未穷尽、可翻页放宽」的条件。
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        st.has_more = true;
        st.candidate_input = "li".to_string();
        st.candidate_limit = 10;
        // 向下箭头：若无守卫会触发 expand_candidates → build_candidates 重建（headless 得空），
        // 过滤结果被清空。
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_DOWN, 0));
        let texts: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["李", "樱"], "↓ 不得丢失辅助码过滤结果");
        assert_eq!(st.active, Some(ModeKind::AuxCode));
    }

    // ───────────────────────── 开关三态（出厂关 / 方案覆盖两个方向）─────────────────────────

    /// ★ 出厂默认**关闭**：方案配了 `files`、也绑了触发键，但没人写过 `enabled`
    /// —— 全局基线 `[schema.pinyin.aux_code].enabled = false` 说了算，门卫拒绝进入。
    ///
    /// 「方案里配了码表」只表示「这个方案推荐这张表」，不构成开启意图。若哪天这条
    /// 判据被改回「files 非空即开」，本用例会立刻变红。
    #[test]
    fn aux_code_disabled_by_default_does_not_enter() {
        let dir = data_dir_with_aux_enabled("default_off", None);
        let c = coord_with_data("default_off", dir);
        let mut st = seed_composition(&c);
        let act = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        assert!(
            act.is_none(),
            "出厂未启用时门卫必须不吞键（落普通标点流程）"
        );
        assert_eq!(st.active, None, "不得进入辅助码模式");
        assert!(st.aux_code.is_none());
        assert_eq!(st.candidates.len(), 3, "候选原封不动（未被 take 走）");
        assert_eq!(st.preedit, "li", "组合区原封不动");
    }

    /// ★ Tab 这类非符号键靠 `[session_actions]` 进辅助码。
    ///
    /// `key_actions` 那张表**解析不出 `tab`**（`key_action_name_to_vk` 只认符号键、字母 z
    /// 与四个修饰键），写进去会被静默丢弃——这正是把 `aux_code` 也收进 `SessionAction`
    /// 的理由。两条路通向同一个 `enter_aux_code`，差别只在哪些键名解析得出来。
    #[test]
    fn session_action_tab_enters_aux_code() {
        let c = coord_with_data_cfg("sess_tab", data_dir_with_aux("sess_tab"), |cfg| {
            cfg.keys
                .session_actions
                .insert("tab".to_string(), "aux_code".to_string());
        });
        let mut st = seed_composition(&c);
        let act = c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true);
        assert!(act.is_some(), "有候选时 Tab 应进辅助码");
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        assert!(st.aux_code.is_some(), "overlay 三件套应已建立");
    }

    /// ★★ 无候选时**必须放行**，否则空闲按 Tab 就再也打不出制表符了。
    ///
    /// 守的是 `requires_candidates()` 那道通用闸门：`AuxCode` 落在
    /// `!matches!(self, None | Cancel)` 的 true 侧是自动的，一旦有人给它加特例
    /// （比如照 `Cancel` 的样子放宽到「有会话即可」），这条就红。
    #[test]
    fn session_action_tab_passes_through_without_candidates() {
        let c = coord_with_data_cfg("sess_tab_idle", data_dir_with_aux("sess_tab_idle"), |cfg| {
            cfg.keys
                .session_actions
                .insert("tab".to_string(), "aux_code".to_string());
        });
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        // 空闲态：无缓冲、无候选。
        let act = c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true);
        assert!(act.is_none(), "无候选时不得吞键——Tab 要还给宿主");
        assert_eq!(st.active, None);
    }

    /// ★★ 专用触发键（只绑 `aux_code`）在辅助码态里再按一次 = **静默消费**，既不退出也不重入。
    ///
    /// 设计约束（2026-08-23）：辅助码模式的退出**只**允许 Esc / 空码退格 / 翻回首页三条路，
    /// 任何触发键在辅助码态内都不能「按一次退出」。真机症状背景（2026-08-22）：早期版本按触发键
    /// 会重入，重入时 `preedit_prefix = format!("{旧 preedit}    ")`，每按一次组合区多 4 个空格。
    /// 这里断言：再按专用触发键后，模式仍在、组合区不再增长、候选不变——既没退出也没重入。
    #[test]
    fn dedicated_trigger_is_consumed_not_exit_in_aux() {
        let c = coord_with_data_cfg("sess_reenter", data_dir_with_aux("sess_reenter"), |cfg| {
            cfg.keys
                .session_actions
                .insert("tab".to_string(), "aux_code".to_string());
        });
        let mut st = seed_composition(&c);
        let tab = key(keymap::VK_TAB, 0);
        c.apply_session_action(&mut st, &tab, true).expect("应进入");
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        let after_enter = st.preedit.clone();
        assert_eq!(after_enter, "li    ", "进入后 = 拼音 + 4 空格");

        // 再按一次：静默消费——仍在辅助码内，组合区不增长、候选不变。
        let act = c.handle_aux_code_key(&mut st, &tab);
        assert!(
            matches!(act, KeyAction::Consumed),
            "专用触发键在辅助码态内应被静默消费"
        );
        assert_eq!(
            st.active,
            Some(ModeKind::AuxCode),
            "触发键再按一次不得退出辅助码"
        );
        assert!(st.aux_code.is_some(), "overlay 三件套应保留");
        assert_eq!(st.preedit, "li    ", "组合区不应再被拼一遍（不重入）");
        assert_eq!(st.candidates.len(), 3, "候选不变");
    }

    /// 按专用触发键**不能上屏**。
    ///
    /// 触发键在辅助码态内被静默消费（见 `dedicated_trigger_is_consumed_not_exit_in_aux`），
    /// 不会落到兜底臂「其余键：有候选则上屏高亮候选并退出」——那会把首选打了出去，是破坏性动作。
    #[test]
    fn trigger_key_does_not_commit_in_aux() {
        let c = coord_with_data_cfg("sess_nocommit", data_dir_with_aux("sess_nocommit"), |cfg| {
            cfg.keys
                .session_actions
                .insert("tab".to_string(), "aux_code".to_string());
        });
        let mut st = seed_composition(&c);
        let tab = key(keymap::VK_TAB, 0);
        c.apply_session_action(&mut st, &tab, true).expect("应进入");
        let _ = c.handle_aux_code_key(&mut st, &tab);
        // 判据取「候选还在、缓冲还在」而不是看返回的 KeyAction 变体：兜底臂走
        // `commit_selected`，它在部分消费时同样返回 `UpdateComposition`，按变体断言测不出来。
        assert_eq!(
            st.candidates.len(),
            3,
            "触发键不得吃掉候选（那意味着上屏了）"
        );
        assert_eq!(st.input_buffer, "li", "触发键不得消费编码缓冲");
        assert!(st.committed_text.is_empty(), "不该有待上屏文本");
    }

    /// ★ 辅助码态里**字母恒是码元**，即便把它在 `session_actions` 里绑成 `aux_code`。
    ///
    /// `is_dedicated_aux_trigger` 显式排除字母（`VK_A..=VK_Z` 一律不当触发键），所以
    /// `z = "aux_code"` 这种绑定在辅助码态内不会把 `z` 当触发键吞掉，而是照常累积为码元。
    /// 会话态那侧同理：`z` 在 `session_key_name_to_vk` 里标着 `printable: true`，辅助码态
    /// 取 `include_printable = false` ⇒ 自动排除。
    ///
    /// 少了这一排的后果：用户若把 `z` 绑成辅助码触发键，在辅助码里就再也打不出 `z`——
    /// 而 `z` 是笔画码的「折」、也是形码方案的常用码元。
    ///
    /// ⚠️ 这条绑定对**进入**本就无效（`bound_action_yield_reason`：字母键仅码表引擎
    /// 生效，而辅助码只服务拼音）。退出侧比进入侧更宽松，正是不对称的形态。
    #[test]
    fn letter_stays_code_input_even_when_bound_as_trigger() {
        let c = coord_with_data_cfg("sess_zletter", data_dir_with_aux("sess_zletter"), |cfg| {
            cfg.keys
                .session_actions
                .insert("z".to_string(), "aux_code".to_string());
        });
        let mut st = seed_composition(&c);
        c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct)
            .expect("反引号应进得去");
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
        assert_eq!(
            st.active,
            Some(ModeKind::AuxCode),
            "z 是码元，不得当成退出键"
        );
        assert_eq!(
            st.aux_code.as_ref().map(|o| o.session.buffer()),
            Some("z"),
            "z 应累积进辅助码缓冲"
        );
    }

    /// ★ 别的 overlay 模式里，触发键也不得把辅助码套进来。
    ///
    /// `handle_candidate_nav` 被五个 overlay 共用，全都会把键送回 `apply_session_action`。
    /// 少了 `enter_aux_code` 的 `state.active.is_some()` 守卫，Tab 在特殊模式/临拼/临英/URL
    /// 里都会夺走那个模式的候选、改写 preedit。
    #[test]
    fn enter_is_refused_from_any_overlay_mode() {
        let c = coord_with("overlay_guard");
        let mut st = seed_composition(&c);
        for mode in [
            ModeKind::TempPinyin,
            ModeKind::TempEnglish,
            ModeKind::AuxCode,
        ] {
            st.active = Some(mode);
            assert!(
                c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct)
                    .is_none(),
                "{mode:?} 态下不得进入辅助码"
            );
            assert_eq!(st.active, Some(mode), "被拒时不得改动 active");
            assert_eq!(st.candidates.len(), 3, "被拒时候选必须原封不动");
            assert_eq!(st.preedit, "li", "被拒时组合区必须原封不动");
        }
    }

    /// `aux_code` 只住 `session_actions`：`key_actions` 不收此动词（写法一致性的旧约束已
    /// 不再适用，因为两张表不再共享这个动词）。
    #[test]
    fn aux_code_verb_only_in_session_actions() {
        use wind_config::{BoundAction, SessionAction};
        assert_eq!(BoundAction::parse("aux_code"), BoundAction::None);
        assert_eq!(SessionAction::parse("aux_code"), SessionAction::AuxCode);
        // 写回也要能读回来（Display 与 parse 互逆）。
        assert_eq!(SessionAction::AuxCode.to_string(), "aux_code");
    }

    /// tri-state 覆盖方向一：全局关 + 方案显式开 → 进得去。
    /// 这正是「只在双拼开、全拼不动」的落地形态。
    #[test]
    fn aux_code_schema_override_enables_over_global_off() {
        let c = coord_with("schema_on"); // fixture 方案写了 enabled = true
        assert!(
            !Config::default().schema.pinyin.aux_code.enabled,
            "前提：全局基线出厂为关，本用例才证明得了方案覆盖生效"
        );
        let mut st = seed_composition(&c);
        let act = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        assert!(act.is_some(), "方案 enabled = true 应覆盖全局的关");
        assert_eq!(st.active, Some(ModeKind::AuxCode));
    }

    /// tri-state 覆盖方向二：全局开 + 方案显式关 → 仍进不去。
    ///
    /// 只测方向一会漏掉「覆盖只实现了 `Some(true)` 分支」这种半截实现——那种写法
    /// 在方向一全绿、方向二静默失效。
    #[test]
    fn aux_code_schema_override_disables_over_global_on() {
        let dir = data_dir_with_aux_enabled("schema_off", Some(false));
        let path = std::env::temp_dir().join("wind_aux_code_schema_off.redb");
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let mut cfg = Config::default();
        cfg.schema.active = "pinyin".to_string();
        cfg.schema.pinyin.aux_code.enabled = true; // 全局开
        let c = Coordinator::new_headless_with_store(cfg, Some(&dir), store);
        let mut st = seed_composition(&c);
        let act = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        assert!(act.is_none(), "方案 enabled = false 应覆盖全局的开");
        assert_eq!(st.active, None);
    }

    // ───────────────────────── 缺陷回归 ─────────────────────────

    /// 回归：**鼠标点击选词**必须与键盘选词走同一条 `commit_selected` 路径。
    ///
    /// `select_candidate_at` 原以 `state.active.is_none()` 区分主输入路，辅助码
    /// （`active == Some(AuxCode)`）于是落进 overlay 分支走 `commit_candidate`——那条会
    /// 直接 `input_buffer.clear()`，于是分步组句在鼠标点第一个字时把剩余拼音一并丢掉，
    /// 而键盘选同一个候选却能继续组句；且 `state.aux_code` 不被清理，overlay 连同候选
    /// 快照残留（三件套「同生共死」的约定被打破）。
    #[test]
    fn mouse_click_commit_keeps_stepwise_conversion() {
        let c = coord_with("mouse_partial");
        {
            let mut st = seed_composition(&c);
            st.candidates[0].consumed_length = 1; // 李 只消费 li 的前 1 码
            let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
            let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0)); // 筛 m → 李/樱
            assert_eq!(st.active, Some(ModeKind::AuxCode));
        } // select_candidate_at 自己取锁，必须先放
        let _ = c.select_candidate_at(0); // 鼠标点第一个候选（李）
        let st = c.state.lock().unwrap();
        assert_eq!(
            st.active,
            Some(ModeKind::AuxCode),
            "部分消费：鼠标点选也应留在辅助码模式继续筛下一段（同键盘）"
        );
        assert_eq!(st.committed_text, "李", "逐步转换：已转换前缀并入");
        assert_eq!(st.input_buffer, "i", "剩余编码必须保留，不得被 clear 掉");
        assert!(st.aux_code.is_some(), "overlay 已按新候选重建，不是残留");
        assert!(
            st.aux_code.as_ref().unwrap().session.is_empty(),
            "重建会话的辅助码缓冲应清空，可继续输入下一段"
        );
    }

    /// 回归：鼠标点选**完整消费**的候选 → 退出辅助码且 overlay 必须清干净。
    #[test]
    fn mouse_click_full_commit_clears_overlay() {
        let c = coord_with("mouse_full");
        {
            let mut st = seed_composition(&c); // consumed_length 全 0 = 整串消费
            let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        }
        let _ = c.select_candidate_at(0);
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, None, "整串消费后退出辅助码");
        assert!(
            st.aux_code.is_none(),
            "overlay 必须随模式一起销毁——否则候选快照会一直挂着"
        );
    }

    /// 回归：辅助码态下切英文，`commit_on_switch` 开启时必须上屏拼音原码。
    ///
    /// ★ 辅助码是唯一**不清空 `input_buffer`** 的独占模式（它只筛候选，拼音码原封不动
    /// 留在主缓冲）。`take_input_on_mode_switch` 的独占分支原本假定「独占模式下
    /// input_buffer 必为空」，对辅助码不成立——匹配不到任何一臂就返回空串，用户待上屏的
    /// 拼音码被静默丢弃，而同样的操作在普通拼音态下会正常上屏。
    #[test]
    fn mode_switch_to_english_commits_pinyin_code() {
        let c = coord_with("switch_en");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        // 切英文（commit_on_switch 出厂即开，见 keys.commit_on_switch）。
        let text = c.take_input_on_mode_switch(&mut st, false);
        assert_eq!(text, "li", "待上屏的拼音原码不得因为在辅助码模式里而丢失");
        assert_eq!(st.active, None, "独占模式一并复位");
        assert!(st.aux_code.is_none());
    }

    /// 回归：进辅助码不得改变候选窗的分页档位。
    ///
    /// ★ `per_page` 原以 `active.is_some()` 决定是否走 `per_page_extended`——辅助码一进
    /// 就切档，候选窗在按下触发键的瞬间从 per_page 跳到扩展档。而 `layout::intent_for`
    /// 的 AuxCode 臂特意返回 `None`（＝沿用主路径呈现），同一意图两处判据相反。
    ///
    /// ⚠️ 出厂 `per_page_extended = 0` 时两档取值相同，这个缺陷**不可见**——fixture 必须
    /// 显式配上扩展档，否则测试恒绿。
    #[test]
    fn aux_code_keeps_main_path_per_page() {
        let c = coord_with_data_cfg("per_page", data_dir_with_aux("per_page"), |cfg| {
            cfg.ui.candidate.per_page = 5;
            cfg.ui.candidate.per_page_extended = 9;
        });
        assert_eq!(c.per_page(None), 5, "主输入路用 per_page");
        assert_eq!(
            c.per_page(Some(ModeKind::TempPinyin)),
            9,
            "真正的 overlay 模式（临拼另起一套候选）才用扩展档"
        );
        assert_eq!(
            c.per_page(Some(ModeKind::AuxCode)),
            5,
            "辅助码只是把主路径候选筛了一轮，分页档位须与主输入路一致"
        );

        // 端到端：同一批候选，进模式前后总页数不得跳变（12 条 → 5 档 3 页 / 9 档 2 页）。
        let mut st = seed_composition(&c);
        st.candidates = (0..12).map(|i| cand(&format!("字{i}"))).collect();
        let pages_before = c.total_pages(&st);
        assert_eq!(pages_before, 3, "前置：主路径 12 条 / 每页 5 = 3 页");
        let _ = c
            .enter_aux_code(&mut st, super::AuxCodeTrigger::Direct)
            .expect("有候选应进入");
        assert_eq!(
            c.total_pages(&st),
            pages_before,
            "进辅助码的瞬间总页数不得跳变"
        );
    }

    /// 回归：辅助码态下末页翻页不得触发「检索范围临时放宽」。
    ///
    /// ★ 放宽是智能档「同码位有常用字就滤掉生僻字」的补偿，辅助码按字形筛、不适用；
    /// 更要命的是放宽会走 `build_candidates` 重建整池候选，把辅助码筛出来的结果整个冲掉。
    ///
    /// ⚠️ 判据只能取候选列表本身：放宽后若没有被滤候选会自行撤销，于是**两条路径的返回值
    /// 都是 false**——只断言返回值和 `scope_relaxed` 会假绿，看不出候选已被冲掉。
    #[test]
    fn aux_code_does_not_relax_scope_on_page_end() {
        let c = coord_with("relax");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["李", "樱"], "前置：m 筛出 李/樱");

        let changed = c.try_relax_scope_on_page_end(&mut st);

        assert!(!changed, "辅助码态不放宽（无变化 → 上层不必重绘）");
        assert!(!st.scope_relaxed, "更不得留下一个影响后续按键的放宽态");
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["李", "樱"], "辅助码的筛选结果必须原样保留");
    }

    // ────────────────────── 辅助码自动退出 ───────────────────────

    /// 翻回第一页（从非首页） + 无辅助码输入 → 自动退出辅助码模式。
    #[test]
    fn auto_exit_on_first_page_no_aux_input() {
        let c = coord_with("auto_exit_first");
        {
            let mut st = seed_composition(&c);
            st.chinese_mode = true;
            // 12 个候选，per_page=9 → 2 页
            st.candidates = (0..12).map(|i| cand(&format!("候选{i}"))).collect();
            let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
            assert_eq!(st.active, Some(ModeKind::AuxCode));
        } // 释放锁
        // 翻到第二页
        let _ = c.handle_key_event(&key(keymap::VK_NEXT, 0));
        {
            let st = c.state.lock().unwrap();
            assert_eq!(st.current_page, 1, "应翻到第二页");
        }
        // 翻回第一页（page_prev）+ 空缓冲 → 自动退出
        let act = c.handle_key_event(&key(keymap::VK_PRIOR, 0));
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, None, "从非首页翻回+空缓冲应自动退出");
        assert!(st.aux_code.is_none());
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    /// 只有一页候选 + 空辅助码缓冲 → 不退出（留在辅助码模式）。
    #[test]
    fn no_auto_exit_when_single_page() {
        let c = coord_with("single_page");
        let mut st = seed_composition(&c);
        st.chinese_mode = true;
        // 直接进入辅助码模式。
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        drop(st);
        // 只有一页 → 按 page_prev 不退出
        let _act = c.handle_key_event(&key(keymap::VK_PRIOR, 0));
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, Some(ModeKind::AuxCode), "只有一页不应自动退出");
    }

    /// 已输入辅助码字母后翻回首页 → 不自动退出。
    #[test]
    fn no_auto_exit_when_aux_input_exists() {
        let c = coord_with("no_exit");
        let mut st = seed_composition(&c);
        st.chinese_mode = true;
        st.candidates = (0..12).map(|i| cand(&format!("候选{i}"))).collect();
        let _ = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        drop(st);
        // 输入辅助码 'm'（session 非空；输码本身会把页码重置回首页）
        let _ = c.handle_key_event(&key(vk_letter('M'), 0));
        {
            let mut st = c.state.lock().unwrap();
            assert!(!st.aux_code.as_ref().unwrap().session.is_empty());
            // 强行置于第二页，模拟「从深页翻回」
            st.current_page = 1;
        }
        // 从第二页翻回第一页 + 有辅助码输入 → 不自动退出
        let act = c.handle_key_event(&key(keymap::VK_PRIOR, 0));
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, Some(ModeKind::AuxCode), "有辅助码输入不退出");
        assert_eq!(st.current_page, 0);
        assert!(matches!(
            act,
            KeyAction::Consumed | KeyAction::UpdateComposition { .. }
        ));
    }

    /// 只绑翻页没绑辅助码 → 正常翻页，无辅助码逻辑。
    #[test]
    fn page_key_only_no_aux_trigger() {
        let c = coord_with("page_only");
        let mut st = seed_composition(&c);
        // 未绑辅助码触发键：直接 enter_aux_code 走原路径。
        let act = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        assert!(act.is_some(), "非辅助码触发键正常进入辅助码");
        assert_eq!(st.active, Some(ModeKind::AuxCode));
    }

    /// 非共享键的原有触发 → 行为不变（直接 enter_aux_code 走原路径）。
    #[test]
    fn existing_trigger_key_still_works() {
        let c = coord_with("existing_trigger");
        let mut st = seed_composition(&c);
        // 直接调用 enter_aux_code 走原有路径（session_actions.aux_code 的进入逻辑）。
        let act = c.enter_aux_code(&mut st, super::AuxCodeTrigger::Direct);
        assert!(act.is_some(), "原有触发键应正常进入辅助码");
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        assert!(st.aux_code.is_some(), "应创建 overlay");
    }

    /// `page_next_aux_code`：单一 `session_actions` 动词即可翻页 + 进辅助码，无需跨表。
    #[test]
    fn page_next_aux_code_enters_and_pages() {
        let c = coord_with_data_cfg("pna_enter", data_dir_with_aux("pna_enter"), |cfg| {
            cfg.schema.active = "pinyin".to_string();
            cfg.keys
                .session_actions
                .insert("tab".to_string(), "page_next_aux_code".to_string());
        });
        {
            let mut st = seed_composition(&c);
            st.chinese_mode = true;
            // 12 个候选，per_page=9 → 2 页
            st.candidates = (0..12).map(|i| cand(&format!("候选{i}"))).collect();
        }
        let act = c.handle_key_event(&key(keymap::VK_TAB, 0));
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, Some(ModeKind::AuxCode), "应进入辅助码模式");
        assert!(st.aux_code.is_some(), "应建立 overlay");
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
        // 翻页后的页码必须保留（FromPage 保留刚翻到的页码）
        assert_eq!(st.current_page, 1, "翻到第二页后页码应保留");
    }

    /// 空缓冲时按 `page_next_aux_code` → 不进辅助码，键正常放行。
    #[test]
    fn page_next_aux_code_no_candidates_passes_through() {
        let c = coord_with_data_cfg("pna_empty", data_dir_with_aux("pna_empty"), |cfg| {
            cfg.schema.active = "pinyin".to_string();
            cfg.keys
                .session_actions
                .insert("tab".to_string(), "page_next_aux_code".to_string());
        });
        {
            let mut st = c.state.lock().unwrap();
            st.chinese_mode = true;
            // 无候选
        }
        let _act = c.handle_key_event(&key(keymap::VK_TAB, 0));
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, None, "空缓冲不进辅助码");
    }

    /// 辅助码模式内按 `page_next_aux_code` → 只翻页、不退出、不重复进入。
    #[test]
    fn page_next_aux_code_continues_paging_in_aux() {
        let c = coord_with_data_cfg("pna_paging", data_dir_with_aux("pna_paging"), |cfg| {
            cfg.schema.active = "pinyin".to_string();
            cfg.keys
                .session_actions
                .insert("tab".to_string(), "page_next_aux_code".to_string());
        });
        {
            let mut st = seed_composition(&c);
            st.chinese_mode = true;
            st.candidates = (0..12).map(|i| cand(&format!("候选{i}"))).collect();
        }
        // 进入辅助码模式（page_next_aux_code：先翻页再进入）
        let _ = c.handle_key_event(&key(keymap::VK_TAB, 0));
        {
            let st = c.state.lock().unwrap();
            assert_eq!(st.active, Some(ModeKind::AuxCode));
            assert_eq!(st.current_page, 1, "已进入且翻到第二页");
        }
        // 输入辅助码 'a'，使 session 非空（防止翻回首页时自动退出）
        let _ = c.handle_key_event(&key(vk_letter('A'), 0));
        // 模式内再按 → 只翻页（已是末页，不翻动）、不退出
        let act = c.handle_key_event(&key(keymap::VK_TAB, 0));
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, Some(ModeKind::AuxCode), "模式内不应退出");
        assert!(matches!(
            act,
            KeyAction::Consumed | KeyAction::UpdateComposition { .. }
        ));
    }

    /// 辅助码未启用时按 `page_next_aux_code` → 只翻页不进辅助码。
    #[test]
    fn page_next_aux_code_disabled_pure_page() {
        let c = coord_with_data_cfg(
            "pna_disabled",
            data_dir_with_aux_enabled("pna_disabled", Some(false)),
            |cfg| {
                cfg.schema.active = "pinyin".to_string();
                cfg.keys
                    .session_actions
                    .insert("tab".to_string(), "page_next_aux_code".to_string());
            },
        );
        {
            let mut st = seed_composition(&c);
            st.chinese_mode = true;
            st.candidates = (0..12).map(|i| cand(&format!("候选{i}"))).collect();
        }
        let _act = c.handle_key_event(&key(keymap::VK_TAB, 0));
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, None, "辅助码未启用不应进入辅助码模式");
        assert!(st.aux_code.is_none(), "不应创建 overlay");
    }
}
