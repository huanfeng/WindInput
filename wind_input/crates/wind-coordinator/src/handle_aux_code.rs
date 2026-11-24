//! 辅助码输入模式：拼音候选的字形二次筛选。
//!
//! 触发：`[key_actions]` 绑 `"aux_code"`（出厂默认 `` ` ``）。空缓冲时不进（无候选可筛，触发键
//! 落普通标点），组码中按下则进入并原地筛选候选。
//!
//! - 只筛选不重排：进入时把原始候选快照进 [`wind_aux_code::AuxCodeSession`]，每次从快照重筛，
//!   被保留者保持原序，被滤者直接丢弃；退出 / 退格都从快照恢复。
//! - 独占输入流：Esc 退出并还原拼音组合；Backspace 删空保持空码态，再退格则退出；
//!   Space / Enter / 数字选词后上屏退出。
//! - 连续组句：选中只消费缓冲前缀时不退出，重建会话快照继续筛下一段，直到消费整串。
//! - 组合区 = 显示前缀（拼音基线 + 4 空格，进入时拼一次）+ 辅助码缓冲。
//!
//! 会话筛选状态（快照/缓冲/重筛）在 `wind-aux-code::session`；显示态（preedit/光标）是
//! 协调器职责，与筛选会话一起打包在 [`AuxCodeOverlay`]（`State.aux_code`）。本模块负责
//! 协调器侧的按键路由、模式进出与 UI 更新。

use crate::coordinator::{Coordinator, State};
use crate::pipeline::ModeKind;
use std::sync::Arc;
use tracing::{debug, info};
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::Candidate;
use wind_ipc::protocol::{MOD_ALT, MOD_CTRL, MOD_SHIFT};
use wind_keys::keymap;

/// 辅助码 overlay 的协调器侧状态三件套（筛选会话 + 显示基线 + 显示前缀）。
///
/// 三者同生共死：enter 一次全建，退出 / 上屏 / 复位一律整体 `take`，不存在只清其中一个的路径。
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
    /// 从哪个模式进来的：`None` = 主输入路，`Some(TempPinyin)` = 临拼。
    ///
    /// 所有「来源相关」分支的唯一开关：辅助码的出口散布在选词、退出、取消、切中英文多处，
    /// 一律问它，不各自猜码在哪个缓冲（临拼来源时码在 `temp_pinyin_buffer`，`input_buffer` 恒空）。
    pub(crate) origin: Option<ModeKind>,
}

/// 辅助码态里一个按键相对**辅助码绑定**的角色，见 [`Coordinator::aux_code_key_role`]。
///
/// 三态：共键（`aux_code:page_next`）与专用触发键在模式内的处置相反（翻页 vs 退出），且都须在
/// `handle_candidate_nav` 之前裁决，否则会漏到兜底臂把首选打出去。
enum AuxKeyRole {
    /// 专用触发键（`aux_code`）：再按一次 = 退出辅助码。
    Exit,
    /// 共键（`aux_code:page_next`）：模式内当下翻页，不退出。
    PageNext,
    /// 与辅助码绑定无关，交给后面的臂（导航 / 选词 / 码元累积 / 兜底）。
    Other,
}

/// 筛选会话见 [`wind_aux_code::AuxCodeSession`]；显示态与它打包在 [`AuxCodeOverlay`]
/// （`State.aux_code`）。
impl Coordinator {
    /// 取辅助码运行时来源：缓存的来源清单与本次一致就复用，否则重建。
    ///
    /// 按来源清单判断而不只靠切方案清空：设置页改来源（写 override）不会切方案。
    pub(crate) fn ensure_aux_code_runtime(
        &self,
        sources: &[wind_engine::AuxSource],
    ) -> Arc<crate::aux_code_source::AuxCodeRuntime> {
        if let Some(rt) = self
            .aux_code_runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|rt| rt.matches(sources))
        {
            return rt.clone();
        }
        let rt = Arc::new(crate::aux_code_source::AuxCodeRuntime::build(
            sources,
            &self.engine_mgr,
        ));
        info!("Loaded aux-code sources ({} layers)", sources.len());
        *self
            .aux_code_runtime
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(rt.clone());
        rt
    }

    /// 「辅助码触发键被音节分隔符占用」的诊断告警，每方案一次（复位点见
    /// [`Coordinator::invalidate_aux_code_table`]），避免每按一次键一条 warn。
    fn warn_aux_code_key_taken(&self, key_code: u32) {
        use std::sync::atomic::Ordering;
        if self.aux_code_key_warned.swap(true, Ordering::Relaxed) {
            return;
        }
        tracing::warn!(
            "辅助码触发键 0x{key_code:02X} 已被拼音音节分隔符占用\
             （schema.pinyin.separator = {:?}），本次不进入辅助码。\
             全拼下 separator = \"auto\" 且 `'` 作选词键时，反引号即分隔符——\
             请为辅助码改绑其它键，或把 separator 显式设为 \"quote\"/\"none\"。",
            self.engine_mgr.pinyin_separator_mode()
        );
    }

    /// 进入辅助码模式。门卫没过返回 `None` 不吞键（与各模式进入门卫同策略）：
    /// - 功能未启用（`[schema.pinyin.aux_code].enabled` 折叠方案覆盖后为 false，出厂即此）
    /// - 触发键已被拼音音节分隔符占用（见 [`Self::warn_aux_code_key_taken`]）
    /// - 方案未配 `[engine.aux_code].files` 或码表文件全部缺失
    /// - 引用的码表方案（`schema:<id>`）反查索引未就绪（派后台构建，建好后再按即可）
    /// - 当前无候选（没有可筛的东西；空缓冲下触发键落普通标点流程）
    pub(crate) fn enter_aux_code(&self, state: &mut State, key_code: u32) -> Option<KeyAction> {
        // 只从主输入路（`None`）与临拼进入：各 overlay 共用的 `handle_candidate_nav` 会把按键送回
        // `apply_session_action`，没有这道守卫触发键会在其中（含辅助码自身）重入，组合区越拼越宽。
        // 临拼放行是因为它产出的正是拼音候选；能否真正进入仍由下面按来源方案取到的码表决定。
        if !matches!(state.active, None | Some(ModeKind::TempPinyin)) {
            return None;
        }
        let origin = state.active;
        // 设置按来源方案取：临拼在五笔方案下引用的是拼音方案，拿五笔的取会静默不进。
        let settings = match self.overlay_engine_schema(state) {
            Some(s) => self.engine_mgr.aux_code_settings_cached_of(&s),
            None => self.engine_mgr.aux_code_settings_cached(),
        };
        if !settings.enabled {
            return None;
        }
        // 键位仲裁：分隔符臂在按键分派里位于 `key_actions` 裁决之前，被占用的键在组码中早被
        // 分隔符吃掉。全拼出厂 `separator = "auto"` + `'` 作选词键时反引号恒为分隔符，不告警则
        // 用户绑了 `backtick = "aux_code"` 完全无反应。同样按来源方案问。
        let separator_taken = match self.overlay_engine_schema(state) {
            Some(s) => self.manual_separator_key_of(key_code, &s),
            None => self.manual_separator_key(key_code),
        };
        if separator_taken {
            self.warn_aux_code_key_taken(key_code);
            return None;
        }
        if settings.sources.is_empty() || state.candidates.is_empty() {
            return None;
        }
        let rt = self.ensure_aux_code_runtime(&settings.sources);
        // 方案来源的反查索引未就绪：不进入、不吞键，派后台构建。按键线程绝不现建（秒级操作）。
        let pending: Vec<String> = rt
            .schema_ids()
            .filter(|id| self.engine_mgr.reverse_index_if_ready(id).is_none())
            .map(str::to_string)
            .collect();
        if !pending.is_empty() {
            for id in &pending {
                self.spawn_index_warm(id, false);
            }
            debug!("aux_code: 方案来源索引未就绪，本次不进入");
            return None;
        }
        // 展开态正按分类筛选时先退回完整列表：快照要原始候选，拿子集当快照的话，退出辅助码后
        // 就只剩那一类了。
        self.grid_restore_full(state);
        let session = wind_aux_code::AuxCodeSession::new(std::mem::take(&mut state.candidates));
        let preedit_base = std::mem::take(&mut state.preedit);
        let preedit_prefix = format!("{}    ", preedit_base);
        // 筛选选项按进入时的生效设置固化。
        let filter_options = wind_aux_code::AuxCodeFilterOptions {
            max_phrase_len: settings.max_phrase_len,
        };
        state.aux_code = Some(AuxCodeOverlay {
            session,
            preedit_base,
            preedit_prefix,
            filter_options,
            origin,
        });
        state.active = Some(ModeKind::AuxCode);
        self.refresh_aux_code_candidates(state);
        let display = state.preedit.clone();
        let caret_pos = self.overlay_caret(state);
        self.notify_ui_update(state);
        debug!(
            "aux_code: entered (key 0x{key_code:02X}, {} candidates)",
            state.candidates.len()
        );
        Some(KeyAction::UpdateComposition {
            text: display,
            caret_pos,
        })
    }

    /// 刷新辅助码候选：按会话内辅助码缓冲对原始候选快照重筛，只保留命中者
    /// （被滤候选直接丢弃，候选窗只显示匹配词）。空缓冲 / 空表由 wind-aux-code 内部
    /// passthrough。同步重拼组合区 = 显示前缀 + 辅助码缓冲。
    pub(crate) fn refresh_aux_code_candidates(&self, state: &mut State) {
        if state.aux_code.is_none() {
            return;
        }
        // 重筛 = 列表重新装填，翻页 / 高亮复位到页首（`reset_candidate_view` 契约）。
        self.reset_candidate_view(state);
        let rt = self
            .aux_code_runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let overlay = state.aux_code.as_mut().expect("辅助码模式必持 overlay");
        // 未建 = 防御语义：不过滤，还原快照（见 wind-aux-code）。
        state.candidates = match rt {
            Some(rt) => {
                let lookup = rt.lookup(&self.engine_mgr);
                let unready = lookup.unready_schemas();
                if unready.is_empty() {
                    overlay.session.apply(&lookup, &overlay.filter_options)
                } else {
                    // 会话中途反查索引被清掉：本次原样放行、派后台重建，否则候选会被滤到几乎全空。
                    for id in unready {
                        self.spawn_index_warm(id, false);
                    }
                    debug!("aux_code: 方案来源索引会话中失效，本次不筛选");
                    overlay.session.restore_original()
                }
            }
            None => overlay.session.restore_original(),
        };
        // 组合区 = 显示前缀（进入时拼好：基线 + 4 空格）+ 辅助码缓冲。
        state.preedit = format!("{}{}", overlay.preedit_prefix, overlay.session.buffer());
    }

    /// 退出辅助码：还原拼音组合（候选恢复会话快照原样、preedit 回到拼音显示）。
    /// 刻意不 `ClearComposition`：退出的语义是「放弃筛选、继续拼音」。调用方返回 `UpdateComposition`。
    pub(crate) fn exit_aux_code(&self, state: &mut State) {
        let Some(mut overlay) = state.aux_code.take() else {
            return;
        };
        // 回到来源模式：从临拼进来的要退回临拼，否则下一个字母会被当成新一轮五笔码。
        state.active = overlay.origin;
        state.candidates = overlay.session.restore_original();
        state.preedit = overlay.preedit_base;
        // 进辅助码前展开着的，退回来也展开着（与分步上屏同一条「本次组合保持」）。
        self.grid_resume(state);
        debug!("aux_code: exited (origin={:?})", overlay.origin);
    }

    /// 辅助码当前正在筛的那个码所在的缓冲（按来源模式取），空串 = 无剩余码。
    ///
    /// 「部分消费 vs 完整消费」的唯一判据。辅助码是唯一不清空来源缓冲的独占模式，来源缓冲随
    /// `origin` 而定；写死 `input_buffer` 会让临拼来源恒判完整消费、静默丢掉剩余拼音。
    pub(crate) fn aux_code_source_buffer<'a>(&self, state: &'a State) -> &'a str {
        match state.aux_code.as_ref().and_then(|o| o.origin) {
            Some(ModeKind::TempPinyin) => &state.temp_pinyin_buffer,
            None
            | Some(
                ModeKind::TempEnglish
                | ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::Special(_)
                | ModeKind::RareChar
                | ModeKind::Reverse
                | ModeKind::Mix(_)
                | ModeKind::AuxCode,
            ) => &state.input_buffer,
        }
    }

    /// 选词上屏后的辅助码收尾：仅清模式态，不碰 preedit / 候选，上屏路径
    /// （`commit_selected`）已把组合区与候选重算好（分步确认则继续拼音、完整上屏则清空）。
    pub(crate) fn finish_aux_code_after_commit(&self, state: &mut State) {
        state.active = None;
        state.aux_code = None;
    }

    /// 辅助码模式下的按键处理。
    pub(crate) fn handle_aux_code_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        // Ctrl/Alt 组合守卫必须最先。「有没有待输入」按来源缓冲问（见 `aux_code_source_buffer`）。
        let has_input = state
            .aux_code
            .as_ref()
            .is_some_and(|o| !o.session.is_empty())
            || !self.aux_code_source_buffer(state).is_empty()
            || !state.committed_text.is_empty();
        if let Some(act) =
            self.overlay_ctrl_alt_guard(state, data, has_input, |s, st| s.exit_aux_code(st))
        {
            return act;
        }
        // 辅助码绑定键的角色裁决须排在 `handle_candidate_nav` 之前：重入被挡住后返回 None，
        // 键会落到兜底臂上屏高亮候选。
        match self.aux_code_key_role(data) {
            // 专用触发键再按一次 = 退出。
            AuxKeyRole::Exit => return self.aux_code_exited(state),
            // 共键在模式内 = 下翻页。不能改走 `handle_candidate_nav`，理由见 `aux_code_key_role`。
            AuxKeyRole::PageNext => {
                if self.page_next(state) {
                    self.notify_ui_update(state);
                }
                return KeyAction::Consumed;
            }
            AuxKeyRole::Other => {}
        }
        // 候选导航（翻页 / 高亮）：辅助码只收字母，`-`/`=`/`[`/`]` 等按普通导航处理。
        if let Some(act) = self.handle_candidate_nav(state, data) {
            return act;
        }
        match data.key_code {
            // Esc：放弃辅助码、还原拼音组合（不走 cancel_session，那会放弃整个组合）。
            keymap::VK_ESCAPE => self.aux_code_exited(state),
            keymap::VK_BACK => {
                // 删一个辅助码字符；删到空保持空码态，已空再退格则退出。
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
                // 与 `commit_highlighted` 动词同一出口。
                self.commit_highlighted(state)
                    .unwrap_or_else(|| self.aux_code_exited(state))
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

    /// 辅助码态里这个键的角色。`keys.key_actions` 与 `keys.session_actions` 两张表都问。
    ///
    /// 这里取 `include_printable = true`，与 `handle_candidate_nav` 相反：那边的 false 保护码元，
    /// 而辅助码的码元只有字母（开头已排除）。沿用 false 会让符号键（一律 `printable = true`）上的
    /// 绑定查不到，键落到兜底臂把首选打出去。
    ///
    /// 已知缺口：符号键上的 `cancel` 等其它会话动词在辅助码态仍不可达（走导航的 false）；要解决
    /// 得先把字母累积臂提到导航之前。
    fn aux_code_key_role(&self, data: &KeyEventData) -> AuxKeyRole {
        // 字母恒是码元，任何绑定都不算数（区间与累积臂相同）。
        if (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code) {
            return AuxKeyRole::Other;
        }
        let shift = data.modifiers & MOD_SHIFT != 0;
        // 必须按分派优先级短路，不能「任一表命中即算」：会话表先裁决，它表了态 `key_actions` 就
        // 没有发言权。否则双拼出厂的 `[key_actions] backtick = "aux_code"` 会把会话表里改配的
        // 共键认成触发键，按一次进、再按一次出。
        if let Some(wind_config::SessionAction::AuxCode(share)) =
            self.session_action_for(data.key_code, shift, true)
        {
            return match share {
                wind_config::AuxCodeShare::Solo => AuxKeyRole::Exit,
                wind_config::AuxCodeShare::PageNext => AuxKeyRole::PageNext,
            };
        }
        // `key_actions` 那张表没有共键形态（它解析不出翻页键，见 `SessionAction::AuxCode`
        // 的文档），命中即专用触发键。
        if matches!(
            self.bound_action_for(data.key_code),
            Some(wind_config::BoundAction::AuxCode)
        ) {
            return AuxKeyRole::Exit;
        }
        AuxKeyRole::Other
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
    /// 连续组句：候选只消费缓冲前缀时不退出，重建会话快照继续筛下一段；完整消费才退出。
    ///
    /// 辅助码的三条选词路径必须全部走这里：键盘（[`Self::handle_aux_code_key`]）、鼠标点选
    /// （`handle_candidate_click`）、修饰键作二三候选键的 keyup（`select_page_candidate`），
    /// 否则同一个选择换个键就换了语义。
    pub(crate) fn aux_code_committed(
        &self,
        state: &mut State,
        cand: Candidate,
        offset: i32,
    ) -> KeyAction {
        // 上屏走来源模式自己的出口：临拼的 `commit_temp_pinyin_selected` 负责分段前缀、造词、
        // 记账与引导字母归还。先把 `active` 还原成来源模式再调，那条出口按它判断自己该不该退出。
        let act = match state.aux_code.as_ref().and_then(|o| o.origin) {
            Some(ModeKind::TempPinyin) => {
                state.active = Some(ModeKind::TempPinyin);
                self.commit_temp_pinyin_selected(state, &cand, offset)
            }
            None
            | Some(
                ModeKind::TempEnglish
                | ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::Special(_)
                | ModeKind::RareChar
                | ModeKind::Reverse
                | ModeKind::Mix(_)
                | ModeKind::AuxCode,
            ) => self.commit_selected(state, &cand, offset),
        };
        // 部分消费（来源缓冲还有剩余）→ 重建会话、留在辅助码模式。
        if !self.aux_code_source_buffer(state).is_empty() {
            // 把 `active` 还给辅助码，否则下一个字母会被当成拼音码累积。
            state.active = Some(ModeKind::AuxCode);
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
    /// - 以引擎重转出的新候选为快照重建会话（辅助码缓冲清空，空码 = passthrough 全显），
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
#[path = "handle_aux_code_tests.rs"]
mod tests;
