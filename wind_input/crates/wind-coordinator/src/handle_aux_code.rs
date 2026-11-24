//! 辅助码输入模式：拼音候选的字形二次筛选。
//!
//! 触发：`[key_actions]` 绑 `"aux_code"`（出厂默认 `` ` ``），空缓冲时**不进**（无候选可筛，
//! 触发键落普通标点），组码中按下 → 进入并原地筛选候选。
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
use std::sync::Arc;
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
    /// 从哪个模式进来的：`None` = 主输入路（历史唯一来源），`Some(TempPinyin)` = 临拼。
    ///
    /// ★★ **本字段是所有「来源相关」分支的唯一开关，且 `None` 分支必须与改动前逐位等价**
    /// ——辅助码的出口散布在选词（键盘 3 条 + 鼠标 1 条）、退出（Esc / 退格 / 触发键复按）、
    /// 取消、切中英文六处，逐处加判断必然漏。所有落点一律问它，不各自去猜「码在哪个缓冲」。
    ///
    /// 为什么需要它：`coordinator.rs` 那句「辅助码是唯一**不清空** `input_buffer` 的独占
    /// 模式（它只筛候选，拼音码原封不动留在主缓冲里）」曾是准确的——直到辅助码能从临拼
    /// 进入：那时码在 `temp_pinyin_buffer`，`input_buffer` 恒空。判据不跟着走，
    /// 「部分消费」会恒判成完整消费、切英文会把待上屏原码静默丢掉。
    pub(crate) origin: Option<ModeKind>,
}

/// 辅助码态里一个按键相对**辅助码绑定**的角色，见 [`Coordinator::aux_code_key_role`]。
///
/// 做成三态而不是 `bool`：共键（`aux_code:page_next`）与专用触发键在模式内的处置相反
/// （翻页 vs 退出），而两者都必须在 `handle_candidate_nav` **之前**裁决——用 bool 表达
/// 只能表达其中一件事，另一件就会漏到兜底臂上把首选打出去。
enum AuxKeyRole {
    /// 专用触发键（`aux_code`）：再按一次 = 退出辅助码。
    Exit,
    /// 共键（`aux_code:page_next`）：模式内当下翻页，**不退出**。
    PageNext,
    /// 与辅助码绑定无关，交给后面的臂（导航 / 选词 / 码元累积 / 兜底）。
    Other,
}

/// 辅助码会话（快照/缓冲/重筛）已移入 `wind-aux-code::session`，见
/// [`wind_aux_code::AuxCodeSession`]；显示态（组合区 preedit/光标）与筛选会话打包在
/// [`AuxCodeOverlay`]，经 `State.aux_code` 整体持有、整体销毁。
impl Coordinator {
    /// 取辅助码运行时来源：缓存的来源清单与本次一致就复用，否则重建。
    ///
    /// ★ 按来源清单判断而不是只靠「切方案清空」：在设置页改了来源（写 override）不会切方案，
    /// 旧做法会一直用旧表直到下次切方案。
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

    /// 「辅助码触发键被音节分隔符占用」的诊断告警，**每方案一次**（复位点见
    /// [`Coordinator::invalidate_aux_code_table`]——切方案即换了一套键位环境，值得再报）。
    ///
    /// 不节流的话每按一次键就是一条 warn；而这条的价值全在「第一次就说清为什么没反应」。
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
    /// - 功能未启用（`[schema.pinyin.aux_code].enabled` 折叠方案覆盖后为 false，**出厂即此**）
    /// - 触发键已被拼音音节分隔符占用（见 [`Self::warn_aux_code_key_taken`]）
    /// - 方案未配 `[engine.aux_code].files` 或码表文件全部缺失
    /// - 引用的码表方案（`schema:<id>`）反查索引未就绪（派后台构建，建好后再按即可）
    /// - 当前无候选（没有可筛的东西；空缓冲下触发键落普通标点流程）
    pub(crate) fn enter_aux_code(&self, state: &mut State, key_code: u32) -> Option<KeyAction> {
        // ★★ 只能从**主输入路**进入：本模式筛的是主路候选，而各 overlay 模式有自己的
        // 候选面与生命周期。
        //
        // ⚠️ 这道守卫在 `apply_session_action` 收下 `SessionAction::AuxCode` 之后才成为
        // 必需：`handle_candidate_nav` 被**五个** overlay 共用（辅助码自身、特殊模式、
        // 临拼、临英、URL），它们都会把按键送回 `apply_session_action`，而它现在认得
        // AuxCode ⇒ 触发键在这些模式里会重入。
        //
        // 少了它的实际症状（2026-08-22 真机）：辅助码态再按一次 Tab → 重入 →
        // `preedit_prefix` 每次在旧 preedit 上再拼 4 个空格，组合区里长出一段越来越宽的
        // 空白，看起来像插进了一个宽制表符。
        // 放行集合：主输入路（`None`）与临拼。**其余 overlay 一律拒**——它们各有自己的
        // 候选面与生命周期，而上面那条重入路径对它们依然成立。
        //
        // ★ 临拼放行的依据是「它产出的正是拼音候选」，与辅助码筛的对象同类；判据不是
        // 「哪些模式允许」的第二份清单，而是下面 `aux_code_settings_cached_of(来源方案)` 是否
        // 真的给出了码表——落点存在 ⇔ 操作可行（同 `candidate_op_scope` 的收敛方式）。
        if !matches!(state.active, None | Some(ModeKind::TempPinyin)) {
            return None;
        }
        let origin = state.active;
        // 设置按**来源方案**取：临拼在五笔方案下引用的是拼音方案，而辅助码码表配在
        // `[engine.aux_code].files` 里——拿五笔的取，`paths.is_empty()` 恒成立、静默不进。
        let settings = match self.overlay_engine_schema(state) {
            Some(s) => self.engine_mgr.aux_code_settings_cached_of(&s),
            None => self.engine_mgr.aux_code_settings_cached(),
        };
        if !settings.enabled {
            return None;
        }
        // 键位仲裁：分隔符臂在按键 match 里位于 `key_actions` 裁决**之前**
        // （`message_handler.rs` 的 VK_QUOTE|VK_BACKTICK 臂 vs 兜底臂里的 D0），
        // 故此处即便放行，该键在组码中也早被分隔符吃掉、根本走不到这里。
        // 全拼出厂 `separator = "auto"` + `'` 作选词键 = 反引号恒为分隔符——
        // 若不告警，用户在 schema_overrides 里绑了 `backtick = "aux_code"` 会
        // **完全无反应且无任何日志**，正是本仓反复出现的「配了没反应」型缺陷。
        // 键位仲裁同样按来源方案问：临拼下「这个键是不是分隔符」取决于临拼引用的拼音方案，
        // 不是活跃的五笔方案（后者恒答否，等于这道闸对临拼完全失效）。
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
        // 方案来源的系统层（反查索引）未就绪：不进入、不吞键，派后台构建（建好有提示与重渲染）。
        // 按键线程绝不现建——那是秒级操作，而 TSF→服务是同步 IPC。
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
        // 三件套整体建立：筛选会话（快照原始候选，后续筛选都从它重筛）+ 显示基线
        // （进入前的拼音显示，退出还原）+ 显示前缀（基线 + 分隔符，进入拼一次）。
        // 此后三者同生共死，退出/上屏/复位一律整体销毁，见 `AuxCodeOverlay`。
        //
        // 展开态正按分类筛选时先退回完整列表：快照要的是**原始**候选，拿子集当快照的话，
        // 下面的刷新经视图复位清掉暂存的完整列表，退出辅助码后就只剩那一类了。
        self.grid_restore_full(state);
        let session = wind_aux_code::AuxCodeSession::new(std::mem::take(&mut state.candidates));
        let preedit_base = std::mem::take(&mut state.preedit);
        let preedit_prefix = format!("{}    ", preedit_base);
        // 筛选选项按本次进入时的生效设置固化（期间方案切换不可见）。
        // 词组逐字首码匹配是固定语义，无模式选项（`AuxCodeFilterOptions` 其余取默认）。
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
                    // 会话中途方案来源的反查索引被清掉：只剩用户层可比会把候选滤到几乎全空。
                    // 本次原样放行、派后台重建（建好后下一个码即恢复筛选），与进入门卫同策略。
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
    /// 刻意**不** `ClearComposition`——辅助码只是筛选，退出的语义是「放弃筛选、继续拼音」，
    /// 而非放弃整个组合。调用方据此返回 `UpdateComposition`。
    pub(crate) fn exit_aux_code(&self, state: &mut State) {
        let Some(mut overlay) = state.aux_code.take() else {
            return;
        };
        // 回到**来源模式**而不是无条件 `None`：从临拼进来的要退回临拼继续打拼音，
        // 否则组合区还留着 `zni` 却已不在任何模式里，下一个字母会被当成新一轮五笔码。
        // 主输入路来源时 `origin == None`，与改动前逐字等价。
        state.active = overlay.origin;
        state.candidates = overlay.session.restore_original();
        state.preedit = overlay.preedit_base;
        // 进辅助码前展开着的，退回来也展开着（与分步上屏同一条「本次组合保持」）。
        self.grid_resume(state);
        debug!("aux_code: exited (origin={:?})", overlay.origin);
    }

    /// 辅助码当前正在筛的那个码所在的缓冲（按来源模式取），空串 = 无剩余码。
    ///
    /// ★★ 「部分消费 vs 完整消费」的**唯一判据**。辅助码是唯一不清空来源缓冲的独占模式，
    /// 但「来源缓冲」是哪一个取决于 `origin`：主输入路是 `input_buffer`，临拼是
    /// `temp_pinyin_buffer`。写死 `input_buffer` 的话，从临拼进来的会话恒判「完整消费」，
    /// 「没时间」这类分步组句在选第一个词时就把剩余拼音丢了——而这**不会报错**。
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

    /// 选词上屏后的辅助码收尾：仅清模式态，**不碰 preedit/候选**——上屏路径
    /// （`commit_selected`）已把组合区与候选重算好（分步确认则继续拼音、完整上屏则清空）。
    pub(crate) fn finish_aux_code_after_commit(&self, state: &mut State) {
        state.active = None;
        state.aux_code = None;
    }

    /// 辅助码模式下的按键处理。
    pub(crate) fn handle_aux_code_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        // Ctrl/Alt 组合守卫：必须最先，否则组合键会落到下方各臂被当普通辅助码输入。
        // 「有没有待输入」按**来源缓冲**问（见 `aux_code_source_buffer`）：从临拼进来时
        // `input_buffer` 恒空，写死它会让 Ctrl 组合在有拼音待上屏时也判成「无输入」。
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
        // 辅助码绑定键的角色裁决。**必须排在 `handle_candidate_nav` 之前**：那个函数把键送回
        // `apply_session_action`，而它认得 AuxCode——虽然 `enter_aux_code` 的守卫已挡住重入，
        // 但挡住之后返回 None，键会继续落到下方兜底臂**上屏高亮候选**，那是个破坏性动作。
        match self.aux_code_key_role(data) {
            // 专用触发键再按一次 = 退出。按同一个键返回正是用户对自选触发键的预期。
            AuxKeyRole::Exit => return self.aux_code_exited(state),
            // 共键（`aux_code:page_next`）在模式内 = 下翻页。
            //
            // ⚠️ 这里不能改走下面的 `handle_candidate_nav`：它取 `include_printable = false`，
            // 而符号键（`backtick` 等）一律 `printable = true` ⇒ 配在符号键上的绑定在那儿
            // **查不到**，键会一路落到兜底臂把首选打出去。见 `aux_code_key_role` 的说明。
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

    /// 辅助码态里这个键的角色。**两张表都问**——`aux_code` 在 `keys.key_actions` 与
    /// `keys.session_actions` 里各有一份，用户配在哪张都算数。
    ///
    /// # ★★★ 为什么这里取 `include_printable = true`，与 `handle_candidate_nav` 相反
    ///
    /// 那个取值（false）是为了保护**码元**——文本/码元语境里 `-`/`=`/`z` 这类键要出字符，
    /// 不能被夺为动作。而**辅助码的码元只有字母**（下方累积臂 `VK_A..=VK_Z`，已在本函数
    /// 开头排除），符号键在辅助码态不产出任何东西。
    ///
    /// 若这里沿用 false，所有**符号键**（`session_key_name_to_vk` 里一律 `printable = true`）
    /// 上的 `aux_code` / `aux_code:page_next` 在模式内都查不到，键一路落到兜底臂
    /// 「有候选则上屏高亮候选并退出」——**把首选打了出去**，而用户只是想退出或翻页。
    /// ⚠️ 这是既有缺陷（不是共键引入的）：把 `backtick = "aux_code"` 只写在 `session_actions`
    /// 里就能复现，此前一直被双拼出厂那份 `[key_actions] backtick = "aux_code"` 兜住。
    ///
    /// 📌 同源的缺口仍在：符号键上的 `cancel` 等**其它**会话动词在辅助码态同样不可达
    /// （走 `handle_candidate_nav` 的 false）。要一并解决得让整个辅助码态改用
    /// `include_printable = true`，那就必须先把字母累积臂提到导航之前——是另一件事。
    fn aux_code_key_role(&self, data: &KeyEventData) -> AuxKeyRole {
        // ★ 字母恒是码元，任何绑定都不算数。区间与下方那条累积臂（`VK_A..=VK_Z`）
        // **取同一个**，两处不会漂。少了它，配过 `z = "aux_code"` 的用户在辅助码里再也
        // 打不出 `z`——而 `z` 是笔画码的「折」、也是形码方案的常用码元。
        if (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code) {
            return AuxKeyRole::Other;
        }
        let shift = data.modifiers & MOD_SHIFT != 0;
        // ★★ 两张表都问，但**必须按分派优先级短路**，不能写成「任一表命中即算」：
        // 会话表在按键分派里先裁决（`apply_session_action` 位于 message_handler 的兜底臂 D0
        // 之前），所以它一旦对这个键表了态，`key_actions` 那份就没有发言权了。
        //
        // 少了这个短路的实际后果：**双拼出厂就带着 `[key_actions] backtick = "aux_code"`**，
        // 用户若把反引号在 `session_actions` 里改配成共键，进入走会话表（共键生效）、退出却
        // 被 `key_actions` 那份认成触发键 ⇒ 按一次进、再按一次出，永远翻不到第二页，
        // 而两处配置各自看上去都没错。
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
        // 上屏走**来源模式自己的出口**：临拼的选词要走 `commit_temp_pinyin_selected`
        // ——它负责分段前缀、造词、记账（`CommitSource::TempPinyin`）与引导字母归还，
        // 这些 `commit_selected` 一件也不做。
        //
        // ★ 先把 `active` 还原成来源模式再调：那条出口内部按 `state.active` 判断自己
        // 该不该退出（`exit_temp_pinyin` 会把它置回 `None`），停在 `AuxCode` 上会让它
        // 认不出自己所在的模式。
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
        // 部分消费 → 缓冲还有剩余编码，处于逐步转换态 → 重建会话、留在辅助码模式。
        // 判据取**来源缓冲**（见 `aux_code_source_buffer`）：临拼的剩余码不在 `input_buffer` 里。
        if !self.aux_code_source_buffer(state).is_empty() {
            // 上面为调用出口把 `active` 挪到了来源模式，这里要还给辅助码——否则组合区
            // 是辅助码的形态，模式却停在临拼上，下一个字母会被当成拼音码累积。
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
#[path = "handle_aux_code_tests.rs"]
mod tests;
