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
        // 「哪些模式允许」的第二份清单，而是下面 `aux_code_settings_of(来源方案)` 是否
        // 真的给出了码表——落点存在 ⇔ 操作可行（同 `candidate_op_scope` 的收敛方式）。
        if !matches!(state.active, None | Some(ModeKind::TempPinyin)) {
            return None;
        }
        let origin = state.active;
        // 设置按**来源方案**取：临拼在五笔方案下引用的是拼音方案，而辅助码码表配在
        // `[engine.aux_code].files` 里——拿五笔的取，`paths.is_empty()` 恒成立、静默不进。
        let settings = match self.overlay_engine_schema(state) {
            Some(s) => self.engine_mgr.aux_code_settings_of(&s),
            None => self.engine_mgr.aux_code_settings(),
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
            _ => &state.input_buffer,
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
            _ => self.commit_selected(state, &cand, offset),
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
    use wind_bridge::handler::KeyEventData;
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
                 [engine.aux_code]\nfiles = [\"aux_code/flypy_test.txt\"]\n{enabled_line}\
                 [key_actions]\nbacktick = \"aux_code\"\n"
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
        assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_none());
        assert_eq!(st.active, None);
    }

    #[test]
    fn enter_sets_mode_and_preserves_pinyin_preedit() {
        let c = coord_with("enter");
        let mut st = seed_composition(&c);
        let act = c
            .enter_aux_code(&mut st, keymap::VK_BACKTICK)
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
    fn backspace_restores_previous_filter_level() {
        let c = coord_with("prev");
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
            let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
            // Ctrl+M 不被当辅助码字符：有输入态 → ClearComposition 退出。
            let act = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), MOD_CTRL));
            assert_eq!(st.active, None);
            assert!(matches!(act, KeyAction::ClearComposition));
        }
        // 无输入态 → PassThrough。
        let mut st2 = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st2, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["李", "樱"], "旧表：m 命中李/樱");
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_ESCAPE, 0));
        // 模拟方案切换后码表被替换：写新表（李=zz/樱=zy/河=zk，首行换名）。
        std::fs::write(&aux_file, "# name: 新表\n李=zz\n樱=zy\n河=zk\n").unwrap();
        c.invalidate_aux_code_table(); // 切方案钩子置 None，下次进入重挂
        // 再次进入：缓存若没失效会沿用旧表（打 m 仍命中）；失效后按新表（全滤为空）。
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let act = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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

    /// ★★ 触发键在辅助码态里再按一次 = **退出**，绝不重入。
    ///
    /// 真机症状（2026-08-22）：重入时 `preedit_prefix = format!("{旧 preedit}    ")`，
    /// 每按一次组合区就多 4 个空格，看起来像插进了一个越来越宽的制表符。
    /// 判据取「组合区长度不增长」而非只看 `active`——`active` 在重入后**仍是** AuxCode，
    /// 只看它这条测不出任何东西。
    #[test]
    fn trigger_key_exits_instead_of_reentering() {
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

        // 再按一次：退出并还原拼音组合，而不是把前缀又拼一遍。
        let act = c.handle_aux_code_key(&mut st, &tab);
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
        assert_eq!(st.active, None, "触发键再按一次应退出辅助码");
        assert!(st.aux_code.is_none(), "overlay 三件套应整体销毁");
        assert_eq!(st.preedit, "li", "组合区应还原成拼音，不带任何残留空格");
        assert_eq!(st.candidates.len(), 3, "候选应还原");
    }

    /// 再按触发键**不能上屏**。
    ///
    /// 少了「触发键退出」这一支，键会落到兜底臂「其余键：有候选则上屏高亮候选并退出」——
    /// 用户只是想退出，却把首选打了出去，是个破坏性动作。
    #[test]
    fn trigger_key_does_not_commit_on_exit() {
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
        assert_eq!(st.candidates.len(), 3, "退出不得吃掉候选（那意味着上屏了）");
        assert_eq!(st.input_buffer, "li", "退出不得消费编码缓冲");
        assert!(st.committed_text.is_empty(), "不该有待上屏文本");
    }

    /// ★ 辅助码态里**字母恒是码元**，即便它在 `key_actions` 里绑了 `aux_code`。
    ///
    /// 两张表在这一点上必须同规格。会话态那侧是天然的：`z` 在 `session_key_name_to_vk`
    /// 里标着 `printable: true`，而辅助码态取 `include_printable = false` ⇒ 自动排除。
    /// `key_actions` 那侧压根没有 `printable` 这个维度，得显式排。
    ///
    /// 少了这一排的后果：配了 `z = "aux_code"` 的用户在辅助码里再也打不出 `z`——
    /// 而 `z` 是笔画码的「折」、也是形码方案的常用码元。
    ///
    /// ⚠️ 进入侧：码表引擎里 z 绑 `aux_code` 本就无意义（辅助码只服务拼音）；拼音里绑了则
    /// 空缓冲按 z 即进辅助码，那是用户自己的配置。退出侧不论如何都让字母回到编码。
    #[test]
    fn letter_stays_code_input_even_when_bound_as_trigger() {
        let c = coord_with_data_cfg("sess_zletter", data_dir_with_aux("sess_zletter"), |cfg| {
            cfg.keys
                .key_actions
                .insert("z".to_string(), "aux_code".to_string());
        });
        let mut st = seed_composition(&c);
        c.enter_aux_code(&mut st, keymap::VK_BACKTICK)
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

    /// ★ **临拼以外**的 overlay 模式里，触发键不得把辅助码套进来。
    ///
    /// `handle_candidate_nav` 被五个 overlay 共用，全都会把键送回 `apply_session_action`。
    /// 少了 `enter_aux_code` 的守卫，Tab 在特殊模式/临英/URL/mix 里都会夺走那个模式的
    /// 候选、改写 preedit。
    ///
    /// ⚠️ 2026-08-30 起 `TempPinyin` **不在**本集合里——它产出的正是拼音候选，与辅助码
    /// 筛的对象同类，故单独放行（正向用例见 `enters_from_temp_pinyin_*`）。放行集合从
    /// 「只有主输入路」变成「主输入路 + 临拼」时，本用例覆盖的其余模式**一个都没少**，
    /// 反而补齐了 Url / Special / Mix 三个当初漏测的。
    #[test]
    fn enter_is_refused_from_non_pinyin_overlay_modes() {
        let c = coord_with("overlay_guard");
        let mut st = seed_composition(&c);
        for mode in [
            ModeKind::TempEnglish,
            ModeKind::AuxCode,
            ModeKind::Url,
            ModeKind::Special(0),
            ModeKind::Mix(0),
        ] {
            st.active = Some(mode);
            assert!(
                c.enter_aux_code(&mut st, keymap::VK_TAB).is_none(),
                "{mode:?} 态下不得进入辅助码"
            );
            assert_eq!(st.active, Some(mode), "被拒时不得改动 active");
            assert_eq!(st.candidates.len(), 3, "被拒时候选必须原封不动");
            assert_eq!(st.preedit, "li", "被拒时组合区必须原封不动");
        }
    }

    /// 装填「临时拼音组合中」的初始状态：码在 `temp_pinyin_buffer`（**不是** `input_buffer`），
    /// 方案指向 fixture 的 pinyin，引导前缀 `z`。与 `seed_composition` 是同一批候选。
    fn seed_temp_pinyin(
        c: &Arc<Coordinator>,
    ) -> std::sync::MutexGuard<'_, crate::coordinator::State> {
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        st.active = Some(ModeKind::TempPinyin);
        st.temp_pinyin_schema = "pinyin".to_string();
        st.temp_pinyin_buffer = "li".to_string();
        st.temp_pinyin_cursor = 2;
        st.temp_pinyin_prefix = "z".to_string();
        st.candidates = vec![cand("李"), cand("樱"), cand("河")];
        st.selected_index = 0;
        st.current_page = 0;
        st.preedit = "zli".to_string();
        st
    }

    /// 在 `data_dir_with_aux` 之上再放一个**码表**方案，供「活跃方案 ≠ 临拼目标方案」的用例。
    ///
    /// ⚠️ 没有这一步就测不出「按目标方案取绑定」——活跃与目标是同一个方案时，
    /// `bound_action_for`（活跃）与 `bound_action_in_schema`（目标）答案相同，变异不会变红。
    /// 这个方案**刻意不写** `[key_actions]`：绑定只存在于 pinyin 那份里。
    fn data_dir_with_codetable_active(tag: &str) -> std::path::PathBuf {
        let dir = data_dir_with_aux(tag);
        std::fs::write(
            dir.join("schemas").join("wb.schema.toml"),
            "[schema]\nid = \"wb\"\nname = \"wb\"\n[engine]\ntype = \"codetable\"\n",
        )
        .unwrap();
        dir
    }

    /// 临拼里按**目标方案**（拼音/双拼）`[key_actions]` 声明的辅助码触发键即可进入。
    ///
    /// 真实场景：五笔主方案 + z 引导临拼，而 `backtick = "aux_code"` 写在
    /// 出厂 shuangpin.schema.toml 里。活跃方案（五笔）那张表没有这条绑定，
    /// 按活跃方案取则**永远进不去**，且无任何日志。
    ///
    /// ★ 辅助码是「编码类」动词，按产出候选的方案取——判据见
    /// docs/design/key-resolver-unification.md §4.4。整张表并不随目标方案走。
    #[test]
    fn temp_pinyin_enters_aux_by_target_schema_binding() {
        let dir = data_dir_with_codetable_active("tp_aux_bind");
        let c = coord_with_data_cfg("tp_aux_bind", dir, |cfg| {
            cfg.schema.active = "wb".to_string();
            cfg.schema.available = vec!["wb".to_string(), "pinyin".to_string()];
        });
        let mut st = seed_temp_pinyin(&c);
        // 前置条件：活跃方案（wb）不认这条绑定——否则本用例测的是回落而不是目标方案。
        assert_ne!(
            c.bound_action_for(keymap::VK_BACKTICK),
            Some(wind_config::BoundAction::AuxCode),
            "前置：活跃方案不该有 aux_code 绑定"
        );
        let act = c.handle_temp_pinyin_key(&mut st, &key(keymap::VK_BACKTICK, 0));
        assert_eq!(
            st.active,
            Some(ModeKind::AuxCode),
            "应按临拼目标方案的 [key_actions] 进入辅助码，实际: {act:?}"
        );
        assert_eq!(
            st.aux_code.as_ref().and_then(|o| o.origin),
            Some(ModeKind::TempPinyin)
        );
    }

    /// 字母键不参与目标方案的辅助码触发判定——辅助码态里字母恒是码元。
    ///
    /// 与 `aux_code_key_role` 的第一道守卫同源：少了它，配过 `z = "aux_code"` 的用户
    /// 在临拼里打不出 z（`zi` / `zuo` / `zhang` 一个都出不来）。
    #[test]
    fn temp_pinyin_letter_not_taken_as_aux_trigger() {
        let dir = data_dir_with_codetable_active("tp_aux_letter");
        // 把 z 也绑成 aux_code：字母守卫若缺失，下面按 z 就会进辅助码而不是累积拼音。
        std::fs::write(
            dir.join("schemas").join("pinyin.schema.toml"),
            "[schema]\nid = \"pinyin\"\nname = \"pinyin\"\n\
             [engine]\ntype = \"pinyin\"\n\
             [engine.aux_code]\nfiles = [\"aux_code/flypy_test.txt\"]\nenabled = true\n\
             [key_actions]\nz = \"aux_code\"\n",
        )
        .unwrap();
        let c = coord_with_data_cfg("tp_aux_letter", dir, |cfg| {
            cfg.schema.active = "wb".to_string();
            cfg.schema.available = vec!["wb".to_string(), "pinyin".to_string()];
        });
        let mut st = seed_temp_pinyin(&c);
        c.handle_temp_pinyin_key(&mut st, &key(vk_letter('Z'), 0));
        assert_eq!(
            st.active,
            Some(ModeKind::TempPinyin),
            "z 是码元，不得进辅助码"
        );
        assert_eq!(st.temp_pinyin_buffer, "liz", "z 应累积进临拼缓冲");
    }

    /// 临拼态可进辅助码，并**记住来源**；筛选照常作用在临拼候选上。
    ///
    /// 用户诉求原话：「临时拼音的其它功能应该尽量和拼音方案本身一致」。辅助码筛的正是
    /// 拼音候选，而临拼产出的就是拼音候选——此前被 `active.is_some()` 一刀切拒之门外。
    #[test]
    fn enters_from_temp_pinyin_and_filters() {
        let c = coord_with("from_temp");
        let mut st = seed_temp_pinyin(&c);
        c.enter_aux_code(&mut st, keymap::VK_BACKTICK)
            .expect("临拼有候选时应能进入辅助码");
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        assert_eq!(
            st.aux_code.as_ref().and_then(|o| o.origin),
            Some(ModeKind::TempPinyin),
            "来源必须记下来——退出/上屏/切英文全靠它分流"
        );
        // 显示基线取的是临拼的组合区（含引导前缀 z），不是空的 input_buffer。
        assert_eq!(
            st.aux_code.as_ref().map(|o| o.preedit_base.as_str()),
            Some("zli"),
            "显示基线应是临拼组合区"
        );
        // 打 m → 李(mz)/樱(my) 命中，河(sk) 被滤。
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        let texts: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["李", "樱"], "辅助码筛选应作用在临拼候选上");
    }

    /// 退出辅助码回到**临拼**，而不是掉进「什么模式都不在」。
    ///
    /// 少了这条，Esc / 退格 / 触发键复按之后 `active` 会变成 `None`，而组合区里还留着
    /// `zli`、`temp_pinyin_buffer` 也还在——下一个字母会被当成新一轮五笔码。
    #[test]
    fn exits_back_to_temp_pinyin() {
        let c = coord_with("exit_temp");
        let mut st = seed_temp_pinyin(&c);
        c.enter_aux_code(&mut st, keymap::VK_BACKTICK).unwrap();
        c.exit_aux_code(&mut st);
        assert_eq!(st.active, Some(ModeKind::TempPinyin), "应退回临拼");
        assert!(st.aux_code.is_none());
        assert_eq!(st.candidates.len(), 3, "候选应还原为进入前的快照");
        assert_eq!(st.preedit, "zli", "组合区应还原成临拼形态");
        assert_eq!(st.temp_pinyin_buffer, "li", "临拼缓冲全程不该被动过");
    }

    /// 主输入路来源仍退到 `None`——`origin` 的 `None` 分支必须与改动前逐位等价。
    #[test]
    fn exits_to_main_path_when_origin_is_none() {
        let c = coord_with("exit_main");
        let mut st = seed_composition(&c);
        c.enter_aux_code(&mut st, keymap::VK_BACKTICK).unwrap();
        assert_eq!(st.aux_code.as_ref().and_then(|o| o.origin), None);
        c.exit_aux_code(&mut st);
        assert_eq!(st.active, None, "主输入路来源退出后不应停在任何模式里");
        assert_eq!(st.preedit, "li");
    }

    /// 「部分消费 vs 完整消费」的判据必须按**来源缓冲**取。
    ///
    /// 写死 `input_buffer` 的话，从临拼进来的会话恒判「完整消费」——「没时间」这类分步
    /// 组句在选第一个词时就把剩余拼音丢了，而且不会报错。本用例直接锁那个取值函数：
    /// 同一个 `State`（`input_buffer` 空、`temp_pinyin_buffer` 非空）下，两种来源必须
    /// 给出不同答案。
    #[test]
    fn source_buffer_follows_origin() {
        let c = coord_with("src_buf");
        let mut st = seed_temp_pinyin(&c);
        assert!(st.input_buffer.is_empty(), "前置：临拼态主缓冲为空");
        c.enter_aux_code(&mut st, keymap::VK_BACKTICK).unwrap();
        assert_eq!(
            c.aux_code_source_buffer(&st),
            "li",
            "临拼来源应取 temp_pinyin_buffer"
        );
        // 把来源改成主输入路，同一个 State 下答案必须翻转（锁住「不是恒取某一个」）。
        st.aux_code.as_mut().unwrap().origin = None;
        assert_eq!(
            c.aux_code_source_buffer(&st),
            "",
            "主输入路来源应取 input_buffer（此处为空）"
        );
    }

    /// 两张表的动词写法必须逐字一致：同一个功能在两处写法不同的话，用户把配置从一张表
    /// 挪到另一张就会静默失效。
    ///
    /// ⚠️ 共键形态（`aux_code:page_next`）**只有 `session_actions` 有**，这不算写法分叉：
    /// `key_actions` 压根解析不出翻页键，那张表里也就不存在「与翻页共键」这回事。
    #[test]
    fn aux_code_verb_spelling_matches_across_both_tables() {
        use wind_config::{AuxCodeShare, BoundAction, SessionAction};
        assert_eq!(BoundAction::parse("aux_code"), BoundAction::AuxCode);
        assert_eq!(
            SessionAction::parse("aux_code"),
            SessionAction::AuxCode(AuxCodeShare::Solo)
        );
        // 写回也要能读回来（Display 与 parse 互逆）。
        assert_eq!(
            SessionAction::AuxCode(AuxCodeShare::Solo).to_string(),
            "aux_code"
        );
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
        let act = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let act = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
            let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
            let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
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
            .enter_aux_code(&mut st, keymap::VK_BACKTICK)
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
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["李", "樱"], "前置：m 筛出 李/樱");

        let changed = c.try_relax_scope_on_page_end(&mut st);

        assert!(!changed, "辅助码态不放宽（无变化 → 上层不必重绘）");
        assert!(!st.scope_relaxed, "更不得留下一个影响后续按键的放宽态");
        let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(kept, vec!["李", "樱"], "辅助码的筛选结果必须原样保留");
    }

    // ─────────────── 与翻页共键（`aux_code:page_next`） ────────────────

    /// 装填「多页候选」的初始状态：12 条 > 出厂每页 9 条 ⇒ 2 页，翻页才有可观测效果。
    fn seed_multi_page(
        c: &Arc<Coordinator>,
    ) -> std::sync::MutexGuard<'_, crate::coordinator::State> {
        let mut st = seed_composition(c);
        st.candidates = (0..12).map(|i| cand(&format!("候选{i}"))).collect();
        st
    }

    fn coord_with_share_key(tag: &str, enabled: Option<bool>) -> Arc<Coordinator> {
        coord_with_data_cfg(tag, data_dir_with_aux_enabled(tag, enabled), |cfg| {
            cfg.keys
                .session_actions
                .insert("tab".to_string(), "aux_code:page_next".to_string());
        })
    }

    /// ★★ 共键**先试辅助码、不先翻页**：首按 Tab 停在第一页，首选不会被翻走。
    ///
    /// 这正是「顺序即优先级」与「两个都做」的分野。若实现成「先翻页再进入」
    /// （社区 PR #74 的 `page_next_aux_code` 形态），用户按一下键就越过了最常用的那一屏。
    ///
    /// ⚠️ 本用例盯的是**进入后停在第几页**这个契约，不是「有没有调用过 `page_next`」：
    /// 进入必经 `refresh_aux_code_candidates` → `reset_candidate_view`，页码在那里归零，
    /// 所以「先翻页但不保留页码」与「不翻页」在这里本就不可区分（实测过：只加 `page_next`
    /// 探针，本用例照绿）。反过来说，「先翻页」这个形态**必须**再补一道保存/恢复
    /// `current_page` 才成立——而那道补丁一加，本用例立刻变红（也实测过）。
    #[test]
    fn share_key_enters_aux_without_paging() {
        let c = coord_with_share_key("share_enter", Some(true));
        let mut st = seed_multi_page(&c);
        let act = c
            .apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
            .expect("有候选且辅助码可用 ⇒ 进辅助码");
        assert_eq!(st.active, Some(ModeKind::AuxCode));
        assert_eq!(st.current_page, 0, "进入时不得先翻页");
        assert_eq!(st.selected_index, 0, "高亮仍在首选");
        assert_eq!(st.preedit, "li    ", "组合区 = 拼音 + 4 空格");
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    /// 辅助码态内再按共键 = 翻页，**不退出**（专用触发键那条「再按一次退出」不适用于它）。
    ///
    /// 走 `handle_aux_code_key` 而非直接调 `apply_session_action`：要连 `is_aux_code_trigger`
    /// 一起测——它若把共键也认成退出键，用户就永远翻不到第二页。
    #[test]
    fn share_key_pages_inside_aux_mode() {
        let c = coord_with_share_key("share_page", Some(true));
        let mut st = seed_multi_page(&c);
        c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
            .expect("应进入辅助码");
        assert_eq!(st.current_page, 0);

        let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_TAB, 0));

        assert_eq!(st.active, Some(ModeKind::AuxCode), "模式内按共键不得退出");
        assert_eq!(st.current_page, 1, "应翻到第二页");
        assert!(st.aux_code.is_some(), "overlay 三件套仍在");
        assert_eq!(st.candidates.len(), 12, "翻页不得动候选");
        assert!(matches!(act, KeyAction::Consumed));
    }

    /// 辅助码未启用 ⇒ 门卫拒 ⇒ 共键退化成**纯翻页键**（而不是哑键）。
    ///
    /// 出厂 `enabled = false`，所以这是绝大多数用户绑上这个动词后的第一手体验：
    /// 功能没开时它必须仍是个能用的翻页键。
    #[test]
    fn share_key_degrades_to_paging_when_aux_disabled() {
        let c = coord_with_share_key("share_off", Some(false));
        let mut st = seed_multi_page(&c);
        let act = c
            .apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
            .expect("降级为翻页 ⇒ 仍然消费按键");
        assert_eq!(st.active, None, "未启用不得进辅助码");
        assert!(st.aux_code.is_none(), "更不得建 overlay");
        assert_eq!(st.current_page, 1, "应当作纯翻页键");
        assert!(matches!(act, KeyAction::Consumed));
    }

    /// 无候选（空闲）⇒ 两个成员都 `requires_candidates` ⇒ 整个动词放行，Tab 还给宿主。
    ///
    /// 判据取「返回 None」：这是本仓「不吞键」的唯一表达，返回 `Consumed` 就意味着用户
    /// 在空闲时按 Tab 什么也不会发生。
    #[test]
    fn share_key_yields_when_no_candidates() {
        let c = coord_with_share_key("share_idle", Some(true));
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        assert!(
            c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
                .is_none(),
            "无候选时共键必须放行（空闲按 Tab 仍是宿主的制表符）"
        );
        assert_eq!(st.active, None);
    }

    /// 已在**临拼以外**的 overlay 模式里 ⇒ `enter_aux_code` 的守卫拒 ⇒ 只翻页。
    ///
    /// `apply_session_action` 被五个 overlay 共用（`handle_candidate_nav` 转调），共键
    /// 在那些模式里必须是个安分的翻页键，不能把主候选夺过来筛。
    ///
    /// ⚠️ 代表模式从 `TempPinyin` 换成 `TempEnglish`：临拼现在放行，共键在那里的语义
    /// 与主输入路一致（先试进入、进不去才翻页），由 `share_key_enters_aux_from_temp_pinyin`
    /// 单独锁。
    #[test]
    fn share_key_only_pages_in_other_overlay_modes() {
        let c = coord_with_share_key("share_overlay", Some(true));
        let mut st = seed_multi_page(&c);
        st.active = Some(ModeKind::TempEnglish);
        let act = c
            .apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
            .expect("翻页 ⇒ 消费");
        assert_eq!(st.active, Some(ModeKind::TempEnglish), "不得改动所在模式");
        assert!(st.aux_code.is_none());
        assert_eq!(st.current_page, 1);
        assert!(matches!(act, KeyAction::Consumed));
    }

    /// 共键在**临拼**里与主输入路同语义：能进辅助码就进（进不去才翻页）。
    ///
    /// 这是上一条的正面：放行集合改了之后，共键的处置必须跟着改，否则「临拼能进辅助码」
    /// 只对专用触发键成立、对共键不成立——同一个功能两个入口两种结果。
    #[test]
    fn share_key_enters_aux_from_temp_pinyin() {
        let c = coord_with_share_key("share_temp", Some(true));
        let mut st = seed_multi_page(&c);
        st.active = Some(ModeKind::TempPinyin);
        st.temp_pinyin_schema = "pinyin".to_string();
        st.temp_pinyin_buffer = "li".to_string();
        c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
            .expect("进入辅助码 ⇒ 消费");
        assert_eq!(
            st.active,
            Some(ModeKind::AuxCode),
            "共键应把临拼候选接进辅助码"
        );
        assert_eq!(
            st.aux_code.as_ref().and_then(|o| o.origin),
            Some(ModeKind::TempPinyin),
            "来源须记成临拼"
        );
    }

    /// ★★★ 专用触发键**只配在 `session_actions` 的符号键上**时，模式内再按必须正常退出，
    /// 且**绝不上屏**。
    ///
    /// 符号键一律 `printable = true`，而辅助码态下 `handle_candidate_nav` 取
    /// `include_printable = false` ⇒ 用同样取值去查这条绑定会**查不到**，键一路落到兜底臂
    /// 「有候选则上屏高亮候选并退出」——用户只是想退出，却把首选打了出去。
    /// 修法是 `aux_code_key_role` 改取 `include_printable = true`（辅助码的码元只有字母，
    /// 已在那里单独排除）。
    ///
    /// ⚠️ 这是**既有缺陷**，不是共键引入的：此前一直被双拼出厂那份
    /// `[key_actions] backtick = "aux_code"` 兜住，所以本用例刻意换一个 fixture 方案
    /// **没有**在 `key_actions` 里绑过的符号键（`/`），否则测的就是 key_actions 那条路。
    ///
    /// 判据取「候选与缓冲原封不动」而非 `KeyAction` 变体：兜底臂走 `commit_selected`，
    /// 它在部分消费时同样返回 `UpdateComposition`，按变体断言测不出来。
    #[test]
    fn solo_trigger_on_symbol_key_exits_without_commit() {
        let c = coord_with_data_cfg("solo_symbol", data_dir_with_aux("solo_symbol"), |cfg| {
            cfg.keys
                .session_actions
                .insert("slash".to_string(), "aux_code".to_string());
        });
        let mut st = seed_composition(&c);
        let slash = key(keymap::VK_SLASH, 0);
        assert_ne!(
            c.bound_action_for(keymap::VK_SLASH),
            Some(wind_config::BoundAction::AuxCode),
            "前提：这个键在 key_actions 里没有绑定，本用例才测得到会话表那条路"
        );
        c.apply_session_action(&mut st, &slash, true)
            .expect("应进入辅助码");

        let act = c.handle_aux_code_key(&mut st, &slash);

        assert_eq!(st.active, None, "专用触发键再按一次应退出");
        assert_eq!(st.candidates.len(), 3, "退出不得吃掉候选（那意味着上屏了）");
        assert_eq!(st.input_buffer, "li", "退出不得消费编码缓冲");
        assert!(st.committed_text.is_empty(), "不该有待上屏文本");
        assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    }

    /// ★★ 同一个键两张表各有一份、且形态不同时，**退出判定必须跟随分派优先级**（会话表先）。
    ///
    /// 这不是构造出来的边角：**双拼出厂就带着 `[key_actions] backtick = "aux_code"`**
    /// （本用例的 fixture 方案同样带），用户只要在 `session_actions` 里把反引号改配成共键
    /// 就会撞上。若 `is_aux_code_trigger` 写成「任一表命中即算触发键」，进入走会话表
    /// （共键生效）、退出却被 `key_actions` 那份认走 ⇒ 按一次进、再按一次出，第二页永远
    /// 到不了，而两处配置各自看上去都没错。
    #[test]
    fn share_key_wins_over_key_actions_binding_on_same_key() {
        let c = coord_with_data_cfg(
            "share_bothtables",
            data_dir_with_aux("share_bothtables"),
            |cfg| {
                cfg.keys
                    .session_actions
                    .insert("backtick".to_string(), "aux_code:page_next".to_string());
            },
        );
        let mut st = seed_multi_page(&c);
        let bt = key(keymap::VK_BACKTICK, 0);
        // 前提断言：fixture 方案的 [key_actions] 里确实还绑着 backtick = "aux_code"。
        // 少了它本用例就退化成普通共键测试，测不到「两张表打架」这件事。
        assert_eq!(
            c.bound_action_for(keymap::VK_BACKTICK),
            Some(wind_config::BoundAction::AuxCode),
            "前提：key_actions 那份绑定仍在"
        );

        c.apply_session_action(&mut st, &bt, true)
            .expect("共键应进辅助码");
        assert_eq!(st.active, Some(ModeKind::AuxCode));

        let act = c.handle_aux_code_key(&mut st, &bt);

        assert_eq!(
            st.active,
            Some(ModeKind::AuxCode),
            "会话表配的是共键 ⇒ key_actions 那份不得把它认成退出键"
        );
        assert_eq!(st.current_page, 1, "应当翻页");
        assert!(matches!(act, KeyAction::Consumed));
    }

    /// 动词写法：`aux_code` / `aux_code:page_next` 与 `Display` 互逆；写错的参数落 `None`。
    ///
    /// 参数写错**不静默降级成专用触发键**：`aux_code:page_prev` 若退回 `aux_code`，
    /// 用户看到的是「共键没生效」，与「功能坏了」同形；落 `None` 才会被
    /// `parse_checked` 在加载期告警。
    #[test]
    fn share_verb_parse_and_display_roundtrip() {
        use wind_config::{AuxCodeShare, SessionAction};
        assert_eq!(
            SessionAction::parse("aux_code"),
            SessionAction::AuxCode(AuxCodeShare::Solo)
        );
        assert_eq!(
            SessionAction::parse(" AUX_CODE:PAGE_NEXT "),
            SessionAction::AuxCode(AuxCodeShare::PageNext)
        );
        assert_eq!(
            SessionAction::AuxCode(AuxCodeShare::PageNext).to_string(),
            "aux_code:page_next"
        );
        assert_eq!(
            SessionAction::AuxCode(AuxCodeShare::Solo).to_string(),
            "aux_code"
        );
        assert_eq!(
            SessionAction::parse("aux_code:page_prev"),
            SessionAction::None
        );
        assert_eq!(SessionAction::parse_checked("aux_code:page_prev"), None);
    }

    /// 自造码表方案（工=a/aaaa、攻=atyy、公=wcu），供 `schema:<id>` 引用。
    ///
    /// ★ id 由调用方给且每条用例不同（含进程号）：反查索引的磁盘缓存按方案 id 落在进程外的
    /// 共享目录，并行用例若共用一个 id 会互相覆盖。
    fn write_wbx(schemas: &std::path::Path, id: &str) {
        std::fs::create_dir_all(schemas.join(id)).unwrap();
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"测五\"\n[engine]\ntype = \"codetable\"\n\
                 [engine.codetable]\nmax_code_length = 4\n\
                 [[dictionaries]]\nid = \"{id}_main\"\npath = \"{id}/{id}.dict.yaml\"\n\
                 type = \"rime_codetable\"\ndefault = true\n"
            ),
        )
        .unwrap();
        std::fs::write(
            schemas.join(format!("{id}/{id}.dict.yaml")),
            format!(
                "---\nname: {id}\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n\
                 a\t工\t100\naaaa\t工\t90\natyy\t攻\t80\nwcu\t公\t80\n"
            ),
        )
        .unwrap();
    }

    /// 本进程内唯一的码表方案 id（见 [`write_wbx`]）。
    fn wbx_id(tag: &str) -> String {
        format!("zz_ax_{tag}_{}", std::process::id())
    }

    /// 用例结束（含 panic）时清掉夹具目录与码表方案在共享缓存根下的产物（`<cache>/<id>/`）。
    /// id 带进程号，不清的话每跑一次就在真实缓存目录里多留一个 `zz_ax_*`，无上限增长。
    struct Cleanup {
        id: String,
        dir: std::path::PathBuf,
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(cache) = Config::cache_dir() {
                let _ = std::fs::remove_dir_all(cache.join(&self.id));
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// pinyin 方案的 `files` 由调用方给（TOML 数组字面量），并带上 `wbx_id` 方案与 `flypy_test.txt`。
    /// 返回的 [`Cleanup`] 要一直持有到用例结束。
    fn data_dir_with_files(tag: &str, files: &str, wbx_id: &str) -> (std::path::PathBuf, Cleanup) {
        let dir =
            std::env::temp_dir().join(format!("wind_aux_code_src_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = Cleanup {
            id: wbx_id.to_string(),
            dir: dir.clone(),
        };
        let schemas = dir.join("schemas");
        std::fs::create_dir_all(schemas.join("aux_code")).unwrap();
        std::fs::write(
            schemas.join("pinyin.schema.toml"),
            format!(
                "[schema]\nid = \"pinyin\"\nname = \"pinyin\"\n[engine]\ntype = \"pinyin\"\n\
                 [engine.aux_code]\nfiles = {files}\nenabled = true\n\
                 [key_actions]\nbacktick = \"aux_code\"\n"
            ),
        )
        .unwrap();
        std::fs::write(
            schemas.join("aux_code/flypy_test.txt"),
            "李=mz\n樱=my\n河=sk\n",
        )
        .unwrap();
        std::fs::write(schemas.join("aux_code/other.txt"), "李=qq\n樱=qq\n河=qq\n").unwrap();
        write_wbx(&schemas, wbx_id);
        (dir, guard)
    }

    fn seed<'a>(
        c: &'a Arc<Coordinator>,
        texts: &[&str],
    ) -> std::sync::MutexGuard<'a, crate::coordinator::State> {
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        st.input_buffer = "gong".to_string();
        st.candidates = texts.iter().map(|t| cand(t)).collect();
        st.selected_index = 0;
        st.current_page = 0;
        st.preedit = "gong".to_string();
        st
    }

    fn kept(st: &crate::coordinator::State) -> Vec<String> {
        st.candidates.iter().map(|c| c.text.clone()).collect()
    }

    /// `schema:<id>`：用码表方案的编码筛拼音候选；与文件来源混列时两边的码都生效。
    #[test]
    fn schema_source_filters_by_codetable_codes() {
        let id = wbx_id("schema");
        let files = format!(r#"["schema:{id}", "aux_code/flypy_test.txt"]"#);
        let (dir, _g) = data_dir_with_files("schema", &files, &id);
        let c = coord_with_data("schema_src", dir);
        c.engine_mgr.prewarm_text_codes(&id);
        let mut st = seed(&c, &["工", "攻", "公", "河"]);
        assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('A'), 0));
        assert_eq!(kept(&st), vec!["工", "攻"], "方案来源：a 命中工/攻");
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('S'), 0));
        assert_eq!(kept(&st), vec!["河"], "文件来源的码同样生效");
    }

    /// 被引用方案里用户自己加的编码也能筛到（系统 + 用户词库是一个整体）。
    #[test]
    fn schema_source_includes_user_words() {
        let id = wbx_id("user");
        let files = format!(r#"["schema:{id}"]"#);
        let (dir, _g) = data_dir_with_files("user", &files, &id);
        let c = coord_with_data("schema_user", dir);
        c.store
            .as_ref()
            .unwrap()
            .add_user_word(&id, "zzzz", "嗨", 0, 0)
            .unwrap();
        c.engine_mgr.prewarm_text_codes(&id);
        let mut st = seed(&c, &["工", "嗨"]);
        assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
        assert_eq!(kept(&st), vec!["嗨"]);
    }

    /// ★ 系统层没就绪：不进入、不吞键——按键线程绝不现建反查索引。
    #[test]
    fn schema_source_not_ready_does_not_enter() {
        let id = wbx_id("cold");
        let files = format!(r#"["schema:{id}"]"#);
        let (dir, _g) = data_dir_with_files("cold", &files, &id);
        let c = coord_with_data("schema_cold", dir);
        let mut st = seed(&c, &["工", "攻"]);
        assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_none());
        assert_eq!(st.active, None);
        assert_eq!(kept(&st), vec!["工", "攻"], "候选原封不动");
        drop(st);
        // 门卫派出的后台构建建好后，下一次按键即可进入；等它落盘完，Cleanup 才清得干净。
        wait_index_ready(&c, &id);
    }

    /// ★ 改了来源（override 层换 files）不切方案也要生效——此前缓存只在切方案时清。
    #[test]
    fn source_change_takes_effect_without_schema_switch() {
        let id = wbx_id("ovr");
        let (dir, _g) = data_dir_with_files("ovr", r#"["aux_code/flypy_test.txt"]"#, &id);
        let ov = dir.join("overrides");
        std::fs::create_dir_all(&ov).unwrap();
        let path =
            std::env::temp_dir().join(format!("wind_aux_code_ovr_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let mut cfg = Config::default();
        cfg.schema.active = "pinyin".to_string();
        let c =
            Coordinator::new_headless_with_store_override(cfg, Some(&dir), store, Some(ov.clone()));
        let mut st = seed(&c, &["李", "樱", "河"]);
        assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
        assert_eq!(kept(&st), vec!["李", "樱"]);
        let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_ESCAPE, 0));
        std::fs::write(
            ov.join("pinyin.toml"),
            "[engine.aux_code]\nfiles = [\"aux_code/other.txt\"]\n",
        )
        .unwrap();
        assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Q'), 0));
        assert_eq!(kept(&st), vec!["李", "樱", "河"], "新来源 other.txt 生效");
    }

    /// 轮询到该方案的反查索引建好（上限 5 秒）。
    fn wait_index_ready(c: &Coordinator, id: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while c.engine_mgr.reverse_index_if_ready(id).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "反查索引 5 秒内没有建好"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// ★ 会话中途方案来源的反查索引被清掉（改方案设置 / 词库启用集变更都会整表清）：
    /// 本次原样放行、后台重建，而不是只拿用户层去比、把候选滤到几乎全空。
    #[test]
    fn schema_layer_lost_mid_session_passes_through_and_rebuilds() {
        let id = wbx_id("lost");
        let files = format!(r#"["schema:{id}"]"#);
        let (dir, _g) = data_dir_with_files("lost", &files, &id);
        let c = coord_with_data("schema_lost", dir);
        c.engine_mgr.prewarm_text_codes(&id);
        let mut st = seed(&c, &["工", "攻", "公", "河"]);
        assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('A'), 0));
        assert_eq!(kept(&st), vec!["工", "攻"]);
        c.engine_mgr.invalidate_schema(&id);
        assert!(c.engine_mgr.reverse_index_if_ready(&id).is_none());
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('T'), 0));
        assert_eq!(
            kept(&st),
            vec!["工", "攻", "公", "河"],
            "系统层没了：原样放行，不拿用户层单独筛"
        );
        drop(st);
        // 派出的后台重建真的把索引建回来了（也保证 Cleanup 之后不再有线程往缓存目录写）。
        wait_index_ready(&c, &id);
    }

    /// ★ 启动预热覆盖辅助码引用的方案：否则每次启动后第一次按辅助码键都静默不进。
    #[test]
    fn prewarm_indexes_covers_aux_schema_sources() {
        let id = wbx_id("warm");
        let files = format!(r#"["schema:{id}"]"#);
        let (dir, _g) = data_dir_with_files("warm", &files, &id);
        let c = coord_with_data("schema_warm", dir);
        assert!(c.engine_mgr.reverse_index_if_ready(&id).is_none());
        c.prewarm_indexes();
        assert!(c.engine_mgr.reverse_index_if_ready(&id).is_some());
        let mut st = seed(&c, &["工", "攻"]);
        assert!(
            c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some(),
            "预热后第一次按键即可进入"
        );
    }

    /// 相邻的文件来源合成一层（wind-aux-code 的「多表坍缩成单表」），只有方案来源各占一层；
    /// 被方案来源隔开的文件段各自合并，清单顺序即优先级不变。
    #[test]
    fn adjacent_file_sources_collapse_into_one_layer() {
        let id = wbx_id("merge");
        let files = format!(
            r#"["aux_code/flypy_test.txt", "aux_code/other.txt", "schema:{id}", "aux_code/other.txt"]"#
        );
        let (dir, _g) = data_dir_with_files("merge", &files, &id);
        let c = coord_with_data("schema_merge", dir);
        let settings = c.engine_mgr.aux_code_settings();
        assert_eq!(settings.sources.len(), 4);
        let rt = c.ensure_aux_code_runtime(&settings.sources);
        assert_eq!(rt.layer_count(), 3, "文件段 + 方案 + 文件段");
        c.engine_mgr.prewarm_text_codes(&id);
        let mut st = seed(&c, &["李", "樱", "河", "工"]);
        assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Q'), 0));
        assert_eq!(
            kept(&st),
            vec!["李", "樱", "河"],
            "合并层里低优表的码同样生效"
        );
    }

    /// ★ 全拼默认辅助码从 `aux_code/stroke.txt` 改为引用笔画码表方案（`schema:stroke`）后，
    /// 筛选结果必须**逐字一致**——两份数据由 `gen_aux_code` 同一次解析产出，这里用真实产物
    /// 对拍：同一组拼音候选、`hspnz` 上全部 1~3 码的辅助码输入，两种来源留下的候选相同。
    ///
    /// 数据取 `build_dev/data`（gen-data 产物，不入库），缺则跳过（全仓惯例）。方案文件取
    /// 仓库里入库的 `data/schemas/stroke.schema.toml`，只把 id 换成本进程唯一的（反查索引的
    /// 磁盘缓存按 id 落在共享目录，见 [`write_wbx`]）。
    #[test]
    fn stroke_schema_source_filters_like_stroke_txt() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let built = root.join("build_dev/data/schemas");
        let (txt, dict) = (
            built.join("aux_code/stroke.txt"),
            built.join("stroke/stroke.dict.yaml"),
        );
        if !txt.is_file() || !dict.is_file() {
            eprintln!(
                "跳过 stroke_schema_source_filters_like_stroke_txt：缺 {} 或 {}（先跑 gen-data）",
                txt.display(),
                dict.display()
            );
            return;
        }
        let schema_text = std::fs::read_to_string(root.join("data/schemas/stroke.schema.toml"))
            .expect("入库的 stroke.schema.toml");
        let id = wbx_id("stroke");
        let (dir, _g) = data_dir_with_files("stroke", r#"["aux_code/stroke.txt"]"#, &id);
        let schemas = dir.join("schemas");
        std::fs::copy(&txt, schemas.join("aux_code/stroke.txt")).unwrap();
        std::fs::create_dir_all(schemas.join("stroke")).unwrap();
        std::fs::copy(&dict, schemas.join("stroke/stroke.dict.yaml")).unwrap();
        let replaced = schema_text.replace("id = \"stroke\"", &format!("id = \"{id}\""));
        assert_ne!(replaced, schema_text, "方案文件里应有 id = \"stroke\"");
        std::fs::write(schemas.join(format!("{id}.schema.toml")), replaced).unwrap();
        // 第二份 data_dir：同一套文件，只把 pinyin 的来源换成方案引用。
        let dir_schema = dir.with_file_name(format!(
            "{}_schema",
            dir.file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_dir_all(&dir_schema);
        copy_dir(&dir, &dir_schema);
        let _g2 = Cleanup {
            id: id.clone(),
            dir: dir_schema.clone(),
        };
        let pinyin = dir_schema.join("schemas/pinyin.schema.toml");
        let text = std::fs::read_to_string(&pinyin).unwrap();
        let swapped = text.replace(r#"["aux_code/stroke.txt"]"#, &format!(r#"["schema:{id}"]"#));
        assert_ne!(swapped, text, "pinyin 的来源应已换成方案引用");
        std::fs::write(&pinyin, swapped).unwrap();

        let by_file = coord_with_data("stroke_file", dir);
        let by_schema = coord_with_data("stroke_schema", dir_schema);
        by_schema.engine_mgr.prewarm_text_codes(&id);

        // 「li」一页的常见候选 + 几个词组（词组走逐字首码）+ 一个无码字符。
        let texts = [
            "李", "里", "理", "力", "利", "立", "离", "例", "历", "丽", "礼", "黎", "粒", "莉",
            "梨", "璃", "哩", "栗", "历史", "利用", "理解", "力量", "里面", "A",
        ];
        let letters = ['h', 's', 'p', 'n', 'z'];
        let mut inputs: Vec<String> = Vec::new();
        for a in letters {
            for b in letters {
                for c in letters {
                    inputs.push([a, b, c].iter().collect());
                }
            }
        }
        let mut narrowed = 0usize;
        for input in &inputs {
            let run = |c: &Arc<Coordinator>| -> Vec<Vec<String>> {
                let mut st = seed(c, &texts);
                assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
                let steps = input
                    .chars()
                    .map(|ch| {
                        let vk = vk_letter(ch.to_ascii_uppercase());
                        let _ = c.handle_aux_code_key(&mut st, &key(vk, 0));
                        kept(&st)
                    })
                    .collect();
                let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_ESCAPE, 0));
                steps
            };
            let (a, b) = (run(&by_file), run(&by_schema));
            assert_eq!(a, b, "辅助码 {input}：文件来源与方案来源筛出的候选不同");
            narrowed += a
                .iter()
                .filter(|k| !k.is_empty() && k.len() < texts.len())
                .count();
        }
        // 反向保证：确实在筛（不是两边都原样放行而恒等）。
        assert!(
            narrowed > 100,
            "有效筛选步数过少（{narrowed}），对拍多半没测到东西"
        );
        // 抽查一条已知码：「力」的上游笔画码是 zp（折撇）。
        let mut st = seed(&by_schema, &texts);
        assert!(
            by_schema
                .enter_aux_code(&mut st, keymap::VK_BACKTICK)
                .is_some()
        );
        let _ = by_schema.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
        let _ = by_schema.handle_aux_code_key(&mut st, &key(vk_letter('P'), 0));
        assert!(kept(&st).contains(&"力".to_string()), "{:?}", kept(&st));
    }

    /// 递归复制目录（测试夹具用）。
    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let dst = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &dst);
            } else {
                std::fs::copy(e.path(), dst).unwrap();
            }
        }
    }
}
