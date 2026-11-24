//! 特殊方案输入模式
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。

use crate::coordinator::{Coordinator, PunctEmptyCodePolicy, State, numpad_char, punct_char};
use crate::pipeline::ModeKind;
use crate::preedit_cursor;
use tracing::debug;
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::Candidate;
use wind_engine::AdmitFn;
use wind_ipc::protocol::MOD_SHIFT;
use wind_keys::keymap;

/// 特殊模式取候选的常规上限，施加在过滤之前。
///
/// 生僻字模式不足一页时由 [`Coordinator::refill_rare_if_short`] 加大重取。别为省掉重取
/// 而调大本值：它会传进拼音引擎的 O(n²) 查重，每次按键都要付。
const SPECIAL_CONVERT_LIMIT: usize = 100;

/// 生僻字模式过滤后不足一页时的重取上限，与拼音引擎的 `MAX_COMPLETION_CANDIDATES`
/// 对齐（再大会被那边 clamp 掉）。
const RARE_REFILL_LIMIT: usize = 1000;

impl Coordinator {
    /// 找出 key_code 绑定的特殊模式下标。
    ///
    /// 数据源是 [`Self::bound_action_for`]：一个键只能绑一个动词，冲突在配置迁移期 warn。
    pub(crate) fn match_special_trigger(&self, key_code: u32) -> Option<u8> {
        match self.bound_action_for(key_code) {
            Some(wind_config::BoundAction::Special(id)) => self.special_mode_idx(&id),
            _ => None,
        }
    }

    /// 按方案 id 在 overlay 注册表中定位下标（与 `match_special_trigger` 的 u8 下标语义
    /// 一致）。供直达热键 `enter_special:<id>` 分发定位；未找到返回 None。
    pub(crate) fn special_mode_idx(&self, id: &str) -> Option<u8> {
        self.engine_mgr.overlay_index_of(id)
    }

    /// 顶屏当前普通输入的半成品（复用 `take_committed` + 高亮候选）并进入特殊模式，
    /// 供直达热键与「缓冲非空/有候选时按引导键」两处共用（对齐 mix/临拼的 commit_and_enter）。
    /// key_code=0 是热键哨兵：`vk_to_prefix_char(0)` 返回 None → `special_prefix` 为空，满足
    /// 「热键进入不写引导符」；引导键进入传真实 VK，组合区写引导符（与空缓冲进入一致）。
    /// 方案须可加载（调用方 `ensure_schema` 保证）。
    pub(crate) fn commit_and_enter_special_mode(
        &self,
        state: &mut State,
        idx: u8,
        key_code: u32,
    ) -> KeyAction {
        // 命令候选顶屏 → 执行命令（与按空格一致），不进模式。
        if let Some(act) = self.top_commit_command_guard(state) {
            return act;
        }
        // 已转换前缀 + 高亮候选一并上屏（含记账与简繁转换）。
        let committed = self.take_committed_with_highlight(state);
        // enter_special_mode 内部清空 input_buffer/candidates、建组合区（key_code=0 → 前缀空）、刷 UI。
        let enter = self.enter_special_mode(state, idx, key_code);
        match committed {
            Some(text) => {
                let new_comp = match &enter {
                    KeyAction::UpdateComposition { text, .. } => text.clone(),
                    _ => state.preedit.clone(),
                };
                self.commit_then_new_composition(text, new_comp)
            }
            None => enter,
        }
    }

    /// 当前特殊模式是否开启「进入即展示候选」（`[overlay] show_all_on_enter`）。
    /// 取进入时的快照，不查注册表——理由见 `State::overlay_spec`。
    fn special_mode_show_all(&self, state: &State) -> bool {
        state
            .overlay_spec
            .as_ref()
            .map(|o| o.show_all_on_enter)
            .unwrap_or(false)
    }

    /// 特殊模式对应的方案 id（= overlay 注册表该下标的方案自身）。
    pub(crate) fn special_schema(&self, idx: u8) -> Option<String> {
        self.engine_mgr
            .overlay_modes()
            .get(idx as usize)
            .map(|e| e.schema_id.clone())
            .filter(|s| !s.is_empty())
    }

    /// 进入特殊模式（其方案须可加载，由激活点 ensure_schema 保证）。清空普通输入，初始化空编码缓冲。
    pub(crate) fn enter_special_mode(
        &self,
        state: &mut State,
        idx: u8,
        key_code: u32,
    ) -> KeyAction {
        state.input_buffer.clear();
        state.candidates.clear();
        state.active = Some(ModeKind::Special(idx));
        state.special_id = idx;
        // `[overlay]` 段快照：布局与进入即展示都取它，见 `State::overlay_spec`。
        state.overlay_spec = self
            .engine_mgr
            .overlay_modes()
            .get(idx as usize)
            .map(|e| e.spec.clone());
        state.special_buffer.clear();
        state.special_cursor = 0;
        // 显示态前缀（进入键符号，如 "\"；经 z_key_action 进入时为 "z"）：只显示不消费。
        state.special_prefix = keymap::vk_to_prefix_char_with_letters(key_code)
            .map(|c| c.to_string())
            .unwrap_or_default();
        self.update_special_candidates(state);
        self.notify_ui_update(state);
        let display = state.preedit.clone();
        debug!("Entered special mode idx={}", idx);
        // 直达热键进入时 display 为空（无引导符），此时是否建占位 composition 取坐标由
        // `[input.caret]` 决定；引导键进入时 display 非空，开关不参与。
        self.special_entry_composition(key_code, display)
    }

    /// 进入生僻字模式：用当前活跃方案的编码输入，候选只留生僻字。
    ///
    /// 与 [`Self::enter_special_mode`] 的差别只有不设 `special_id` / `overlay_spec`：本模式
    /// 没有宿主方案，从它们取值处刻意落到默认档（不展示全部、布局与注释模板跟随全局）。
    ///
    /// 缓冲复用 `special_buffer`：按键处理走的是同一个 `handle_special_key`。
    pub(crate) fn enter_rare_char_mode(&self, state: &mut State, key_code: u32) -> KeyAction {
        state.input_buffer.clear();
        state.candidates.clear();
        state.active = Some(ModeKind::RareChar);
        state.special_id = 0;
        state.overlay_spec = None;
        state.special_buffer.clear();
        state.special_cursor = 0;
        state.special_prefix = keymap::vk_to_prefix_char_with_letters(key_code)
            .map(|c| c.to_string())
            .unwrap_or_default();
        self.update_special_candidates(state);
        self.notify_ui_update(state);
        let display = state.preedit.clone();
        debug!("Entered rare-char mode");
        // 同 `enter_special_mode`：只有直达热键（display 为空）那条看开关。
        self.rare_char_entry_composition(key_code, display)
    }

    /// 顶掉当前半成品并进入生僻字模式（引导键 / 直达热键共用）。
    ///
    /// 「顶字重开」与临拼 / 快符 / mix 一致。若将来加「保留编码、原地换成生僻字」的形态，
    /// 改的是这里：准入判据 `wind_candidate::rare_admits` 不依赖进入方式，不必动过滤侧。
    pub(crate) fn commit_and_enter_rare_char_mode(
        &self,
        state: &mut State,
        key_code: u32,
    ) -> KeyAction {
        if let Some(act) = self.top_commit_command_guard(state) {
            return act;
        }
        let committed = self.take_committed_with_highlight(state);
        let enter = self.enter_rare_char_mode(state, key_code);
        match committed {
            Some(text) => {
                let new_comp = match &enter {
                    KeyAction::UpdateComposition { text, .. } => text.clone(),
                    _ => state.preedit.clone(),
                };
                self.commit_then_new_composition(text, new_comp)
            }
            None => enter,
        }
    }

    /// 交给引擎的生僻字准入闭包（仅生僻字模式；其余模式返回 `None` = 不筛）。
    ///
    /// 与 [`Self::retain_rare_admitted`] 必须同源（都走 `wind_candidate::rare_admits`）：
    /// 这个在引擎产出时逐条判，那个拿到结果后兜底。两边不一致会白占引擎配额或候选莫名变少。
    ///
    /// 常用字表未加载时返回 `None`（不筛），同 `retain_rare_admitted` 的保护。
    fn rare_admit_fn(&self, state: &State) -> Option<AdmitFn> {
        if !matches!(state.active, Some(ModeKind::RareChar)) {
            return None;
        }
        if self
            .common_chars
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            return None;
        }
        // `ConvertOptions::admit` 要求 `'static`，故克隆 Arc（表数据不复制）。
        let cc = self.common_chars.clone();
        // registry 取调用这一刻的快照。
        let extra = self.engine_mgr.charsets();
        Some(std::sync::Arc::new(move |text: &str| {
            let g = cc.read().unwrap_or_else(|e| e.into_inner());
            wind_candidate::rare_admits(text, &g, &extra)
        }))
    }

    /// 生僻字模式下的取数选项。
    ///
    /// 首次取数（[`Self::update_special_candidates`]）与重取（[`Self::refill_rare_if_short`]）
    /// 必须共用本函数：重取不带准入下推的话，大预算会花在随后被滤掉的常用字上，还白付
    /// 全量查重成本。其余模式 `rare_admit_fn` 返回 `None`，本函数对它们是空选项。
    fn rare_convert_opts(&self, state: &State) -> wind_engine::ConvertOptions {
        wind_engine::ConvertOptions {
            admit: self.rare_admit_fn(state),
            // 简拼保底配额会挤掉尾部候选（正是生僻字所在），补进来的简拼词又会被准入删掉，
            // 净效果是白丢生僻字。见 `ConvertOptions::no_abbrev_quota`。
            no_abbrev_quota: true,
            ..Default::default()
        }
    }

    /// 对给定候选施加生僻字准入（仅生僻字模式；其余模式为空操作）。
    ///
    /// 凡在 `RareChar` 守卫之外的调用点，一律经本函数，不要就地调 `retain_rare_admitted`：
    /// 对快符等 overlay 模式施加生僻准入会把候选（符号、多字条目）整批滤空。
    /// `refill_rare_if_short` 与 mix 的生僻字成员本身已在守卫内，可直接调。
    ///
    /// 默认夹具没装常用字表，过滤整条早退；验这道准入的用例要先照 `set_common` 装上字表与 registry。
    ///
    /// 取 `active` 而非 `&State`：调用方常在可变借出候选表的同时需要这个判据。
    fn apply_rare_admission(
        &self,
        active: Option<ModeKind>,
        candidates: &mut Vec<wind_candidate::Candidate>,
    ) {
        if !matches!(active, Some(ModeKind::RareChar)) {
            return;
        }
        self.retain_rare_admitted(candidates);
    }

    /// 生僻字模式：过滤后不足一页时加大取数上限重取一次。
    ///
    /// 上限施加在过滤之前，而引擎按常用度排序，生僻字恰在被切掉的尾部；同码候选越多
    /// （短码、高频音节）丢得越多，与方案类型无关。不能改成无条件大上限：上限会传进
    /// 拼音引擎的 O(n²) 查重，每键都付。准入下推（`ConvertOptions::admit`）只省查重成本，
    /// 补不回词库层 top-N 截掉的名额，故两者都要。实测数据见
    /// docs/design/single-char-mode.md 的「「截断 → 过滤」在拼音上仍在」。
    fn refill_rare_if_short(&self, state: &mut State, schema: &str) {
        if !matches!(state.active, Some(ModeKind::RareChar)) {
            return;
        }
        // 够一页就不重取，判据用实际生效的每页条数。
        let need = self.per_page(state.active);
        if state.candidates.len() >= need {
            return;
        }
        // 与首次取数共用 `rare_convert_opts`，见那里。
        let opts = self.rare_convert_opts(state);
        let result = self.engine_mgr.convert_with_opts(
            schema,
            &state.special_buffer,
            RARE_REFILL_LIMIT,
            opts,
        );
        // 必须走同一条加工链（finalize → 过滤），否则 `$CC` 等词条在重取路径上不展开。
        let mut refilled = self.finalize_candidates(result.candidates, &state.special_buffer);
        self.retain_rare_admitted(&mut refilled);
        // 理应是超集，仍取多的那份以防引擎不单调。
        if refilled.len() > state.candidates.len() {
            state.candidates = refilled;
        }
    }

    /// 按生僻字准入就地保留候选。
    ///
    /// 常用字表未加载时整条跳过（同 `apply_filter` 的 `common_chars.is_empty()`）：那时
    /// 全体候选都会被判成非常用，过滤没有意义。
    ///
    /// 严格过滤是既定取舍：滤空就空着，不回补、不降级。
    pub(crate) fn retain_rare_admitted(&self, candidates: &mut Vec<wind_candidate::Candidate>) {
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        if cc.is_empty() {
            return;
        }
        // `include_blocks` 点名的类已在装配期打好 `in_rare` 标记。在 `retain` 之外取一次，
        // 免得每键逐候选加锁。
        let extra = self.engine_mgr.charsets();
        candidates.retain(|c| wind_candidate::rare_admits(&c.text, &cc, &extra));
    }

    /// 退出特殊模式并清空相关状态（码表缓存保留供复用）。
    pub(crate) fn exit_special_mode(&self, state: &mut State) {
        state.active = None;
        // 防御性清掉夺取回退登记：留着的话，下次进生僻字打到同样缓冲再退格会误跳回 `z`。
        // `rewind_hijack` 是先 `take` 再调本函数，不受影响。
        state.rewind = None;
        state.overlay_spec = None;
        state.special_buffer.clear();
        state.special_cursor = 0;
        state.special_prefix.clear();
        state.candidates.clear();
        state.preedit.clear();
        // 反查模式会写这三位，不复位的话退出后主路径翻页会误扩充、下次组码直接处于放宽态。
        state.has_more = false;
        state.candidate_limit = 0;
        state.scope_relaxed = false;
    }

    /// [`Self::update_special_candidates`] 请求的全码自动上屏：命令候选走命令路径，
    /// 普通候选记统计、退出模式后上屏。字母键臂与 z 夺取进生僻字（`try_z_fallback`）共用。
    pub(crate) fn special_auto_commit(&self, state: &mut State, cand: Candidate) -> KeyAction {
        // $CC 命令候选自动命中：与手动选中同路（退出模式 + 异步执行动作）。
        let code = state.special_buffer.clone();
        if let Some(act) =
            self.overlay_commit_command(state, &cand, &code, |s, st| s.exit_special_mode(st))
        {
            return act;
        }
        // 自动上屏也是一次选中：记账同选词出口（`record_special_selection`）。
        self.record_special_selection(state, &cand);
        self.record_commit(
            &cand.text,
            state.special_buffer.len() as u32,
            -1,
            wind_store::stats::CommitSource::SpecialMode,
        );
        self.exit_special_mode(state);
        self.notify_ui_hide();
        Self::commit_action(cand.text, true)
    }

    /// 按当前编码缓冲刷新特殊模式候选（经其引用方案的引擎查询，复用方案 CodeTableSpec 全码策略）。
    /// 返回 Some(候选) 表示该方案的全码策略请求自动上屏该候选（`$CC` 命令候选由调用方
    /// 走命令执行路径，普通候选上屏其文本）。
    pub(crate) fn update_special_candidates(&self, state: &mut State) -> Option<Candidate> {
        // 反查模式：缓冲 / 按键处理与 special 族共用，候选另建（convert_reverse + 检索范围
        // 过滤），且从不请求自动上屏（spec reverse-mode §3.2）。
        if matches!(state.active, Some(ModeKind::Reverse)) {
            self.update_reverse_candidates(state);
            return None;
        }
        state.candidates.clear();
        self.reset_candidate_view(state);
        // 组合区 = 显示态前缀 + 编码缓冲（前缀只显示不参与查询）。
        state.preedit = format!("{}{}", state.special_prefix, state.special_buffer);
        if state.special_buffer.is_empty() {
            // 进入即展示：开启 show_all_on_enter 时空码枚举码表首页候选（weight 降序）供浏览；
            // 未开则维持空白，敲码才出候选。
            if self.special_mode_show_all(state)
                && let Some(schema) = self.overlay_engine_schema(state)
            {
                let raw = self.engine_mgr.enumerate_with(&schema, 100);
                state.candidates = self.finalize_candidates(raw, "");
                // 浏览态同样吃候选调整（空码作为 shadow 键位）：一码即上屏的快符方案里，
                // 这批候选往往是用户唯一能右键调整的对象。
                let owner = self.effective_data_schema(state);
                let mut list = std::mem::take(&mut state.candidates);
                self.run_passes(
                    state,
                    &mut list,
                    crate::postprocess::profile::SPECIAL_SHADOW,
                    &crate::postprocess::PassCtx {
                        owner: owner.as_deref(),
                        freq_code: None,
                        shadow_code: Some(""),
                    },
                );
                state.candidates = list;
                // 呈现上限（精确匹配模式只展示一条）必须在 shadow 之后施加，否则用户隐藏掉
                // 那一条后屏幕整个空白。
                if let Some(n) = self.engine_mgr.browse_display_limit_of(&schema) {
                    state.candidates.truncate(n);
                }
            }
            return None;
        }
        let schema = self.overlay_engine_schema(state)?;
        // 生僻字模式把准入下推给引擎（见 `ConvertOptions::admit`），其余模式为空选项。
        let opts = self.rare_convert_opts(state);
        let result = self.engine_mgr.convert_with_opts(
            &schema,
            &state.special_buffer,
            SPECIAL_CONVERT_LIMIT,
            opts,
        );
        // 统一展开汇聚点：快符表内 `$AA/$SS/$CC` 等特殊语法在此炸开/标命令（见 finalize_candidates）。
        state.candidates = self.finalize_candidates(result.candidates, &state.special_buffer);
        // 生僻准入在 finalize 之后（要看展开后的文本）、shadow 之前（同主路径
        // `apply_filter` → `apply_shadow`：先定哪些候选存在，再应用置顶/隐藏）。
        self.apply_rare_admission(state.active, &mut state.candidates);
        self.refill_rare_if_short(state, &schema);
        // 词频重排与候选调整归属特殊方案自身，必须与写端 `record_selection_in` 同取自
        // `effective_data_schema`，否则写一处读另一处，候选顺序永远不动。
        let owner = self.effective_data_schema(state);
        // 生僻字模式不参与词频：读写两端都跳过，否则模式内顺序仍被正常输入的词频牵着走。
        let mut list = std::mem::take(&mut state.candidates);
        let ctx = crate::postprocess::PassCtx {
            owner: owner.as_deref(),
            freq_code: Some(&state.special_buffer),
            shadow_code: Some(&state.special_buffer),
        };
        self.run_passes(
            state,
            &mut list,
            if matches!(state.active, Some(ModeKind::RareChar)) {
                crate::postprocess::profile::RARE_CHAR_PRE
            } else {
                crate::postprocess::profile::SPECIAL_PRE
            },
            &ctx,
        );
        // 纠错候选不占首选（D6）：本路不重排，与主路 / 临拼同一个 helper、同一位置
        // （全部重排之后、shadow 之前——用户置顶仍有最终话语权）。
        wind_candidate::drop_corrected_duplicates(&mut list);
        wind_candidate::protect_top_from_corrected(&mut list, state.special_buffer.len());
        self.run_passes(
            state,
            &mut list,
            crate::postprocess::profile::SPECIAL_SHADOW,
            &ctx,
        );
        state.candidates = list;
        // 空码补全（对齐主路径 `update_candidates` 的补全收口）：精确匹配下本码无候选时，
        // 取引擎备好的 `completion_hints` 一条。判空在过滤之后，否则候选被全部隐藏时不补、空屏。
        // 这条旁路绕过了上面的加工，须同样过 finalize（否则显示 `$CC(...)` 源码）、生僻准入
        // 与 shadow（否则隐藏完当场又被补回来）。
        if state.candidates.is_empty() {
            let mut hint = self.finalize_candidates(result.completion_hints, &state.special_buffer);
            // 走带模式守卫的 `apply_rare_admission`，见其说明。
            self.apply_rare_admission(state.active, &mut hint);
            self.run_passes(
                state,
                &mut hint,
                crate::postprocess::profile::SPECIAL_SHADOW,
                &crate::postprocess::PassCtx {
                    owner: owner.as_deref(),
                    freq_code: None,
                    shadow_code: Some(&state.special_buffer),
                },
            );
            // 仍只采纳一条（`$AA`/`$SS` 在前缀情形折叠为单个组名候选，正常也只有一条）。
            state.candidates.extend(hint.into_iter().next());
        }
        // 自动上屏由方案码表引擎决定，并复核目标仍在候选中。引擎首轮意向判在 shadow 生效
        // 之前：用自定义词条替换内置词条时新旧两条同时在场被判不唯一，故否决时按 shadow 后
        // 的列表复评。复评必须按本模式的方案取引擎（`recheck_auto_commit_for`），不能用主方案的。
        let auto_commit = if result.should_commit && !result.commit_text.is_empty() {
            Some(result.commit_text)
        } else {
            self.engine_mgr.recheck_auto_commit_for(
                &schema,
                &state.special_buffer,
                &state.candidates,
            )
        };
        if let Some(t) = auto_commit {
            // 引擎意向是词条原文：`$CC` 命令比 phrase_template、`$` 模板比 template_source
            // （展开后 text 已变；`{..}` 插值不放行，同主路复核）。
            return state
                .candidates
                .iter()
                .find(|c| {
                    (c.is_command && c.phrase_template == t)
                        || Self::auto_commit_target_matches(c, &t)
                })
                .cloned();
        }
        None
    }

    /// 特殊模式各上屏出口（选词 / 标点顶屏 / 全码自动上屏）共用的记账：词频 + 上屏历史。
    /// 输入统计（`record_commit`）由各出口自己记（码长、位次口径各不同）。
    ///
    /// 词频记账**归属特殊方案自身**（与主方案同层级，只是用特殊按键进入）。
    /// 记账码用输入码：特殊方案是码表语义，`a`/`ab`/`abc` 是三个独立码位，
    /// 与 `freq_code` 对 CodeTable 来源的口径一致。
    ///
    /// 生僻字模式不记词频，正常输入的候选顺序不受影响；但统计与上屏历史照记，否则
    /// 「重复上屏」取不到刚打出的生僻字。
    fn record_special_selection(&self, state: &State, cand: &Candidate) {
        // 反查模式：缓冲是查询串（含通配）而非码位，记账码取候选**全码**（
        // 同主路通配的 `main_freq_code`）；归属 active（`effective_data_schema` 给 None）。
        if matches!(state.active, Some(ModeKind::Reverse)) {
            let code = if cand.code.is_empty() {
                state.special_buffer.clone()
            } else {
                cand.code.clone()
            };
            self.record_selection_cased_in(None, &code, cand.freq_text(), &cand.text, cand.source);
            return;
        }
        if !matches!(state.active, Some(ModeKind::RareChar)) {
            let code = state.special_buffer.clone();
            // 词频记 `freq_text`（模板词为源文本，与 `update_special_candidates` 的重排读端
            // 同键），历史记实际上屏的 `text`。
            self.record_selection_cased_in(
                self.effective_data_schema(state).as_deref(),
                &code,
                cand.freq_text(),
                &cand.text,
                cand.source,
            );
        } else {
            // 上屏历史是 `record_selection_in` 的一部分，跳过词频时须单独补上。
            self.push_commit_history(&cand.text);
        }
    }

    /// 特殊模式选中某候选（全局下标 `gi`）：`$AA`/`$SS` 组折叠候选 → 补全编码到完整码重查展开（二级选择）；
    /// `$CC` 命令候选 → 执行动作（退出后异步跑，触发键码不上屏）；否则文本上屏。
    /// 统一空格 / 数字键 / 二三候选键的选中入口，保证组/命令候选选中行为一致。
    pub(crate) fn commit_special_candidate(&self, state: &mut State, gi: usize) -> KeyAction {
        let cand = state.candidates[gi].clone();
        // $AA/$SS 组折叠候选：补全编码到完整码并重查展开（不上屏组名）。
        if cand.is_group {
            state.special_buffer = cand.group_code.clone();
            state.special_cursor = state.special_buffer.len(); // 补全到完整码：光标落末尾
            self.update_special_candidates(state);
            let display = state.preedit.clone();
            let caret_pos = self.overlay_caret(state);
            self.notify_ui_update(state);
            return KeyAction::UpdateComposition {
                text: display,
                caret_pos,
            };
        }
        let code = state.special_buffer.clone();
        if let Some(act) =
            self.overlay_commit_command(state, &cand, &code, |s, st| s.exit_special_mode(st))
        {
            return act;
        }
        self.record_special_selection(state, &cand);
        self.record_commit(
            &cand.text,
            state.special_buffer.len() as u32,
            -1,
            wind_store::stats::CommitSource::SpecialMode,
        );
        self.exit_special_mode(state);
        self.notify_ui_hide();
        Self::commit_action(cand.text, true)
    }

    /// 特殊模式按键处理：编码累积 + 候选选择 + 三档自动上屏；空格选高亮、回车上屏编码原文。
    pub(crate) fn handle_special_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        // Ctrl/Alt 组合守卫（见 `overlay_ctrl_alt_guard`）：必须最先，否则组合键会落到
        // 下方各臂被当成编码输入。
        if let Some(act) =
            self.overlay_ctrl_alt_guard(state, data, !state.special_buffer.is_empty(), |s, st| {
                s.exit_special_mode(st)
            })
        {
            return act;
        }
        if let Some(act) = self.handle_candidate_nav(state, data) {
            return act;
        }
        // 编码区光标移动（左右 / Home / End）；置于候选导航之后，导航键优先。
        if let Some(act) = self.overlay_cursor_key(state, data) {
            return act;
        }
        // 进入键二次按下（缓冲空）：按中英标点配置上屏该符号并退出。
        // 顺带武装智能符号：时限内再按同键即换英文形，否则这个键被模式占着、英文形没有通路。
        // press2 的拦截在 try_activate_mode 开头，早于模式激活链。
        if state.special_buffer.is_empty()
            && self.match_special_trigger(data.key_code) == Some(state.special_id)
            && let Some(ch) = punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
        {
            let out = self.convert_punct_char(state, ch);
            self.arm_smart_symbol_after_commit(state, ch, &out);
            self.record_commit(&out, 0, -1, wind_store::stats::CommitSource::Punctuation);
            self.exit_special_mode(state);
            self.notify_ui_hide();
            return Self::commit_action(out, true);
        }
        match data.key_code {
            // Esc：放弃退出，实现收口在 `cancel_session`。
            keymap::VK_ESCAPE => self.cancel_session(state),
            keymap::VK_BACK | keymap::VK_DELETE => {
                // 退格删光标前 / Delete 删光标后；缓冲被删空则退出（本就空缓冲时只有退格退出，
                // 保持原语义）。删除时不触发自动上屏。
                let backward = data.key_code == keymap::VK_BACK;
                let removed = {
                    let mut ed = preedit_cursor::BufEdit::new(
                        &mut state.special_buffer,
                        &mut state.special_cursor,
                    );
                    if backward {
                        ed.backspace()
                    } else {
                        ed.delete()
                    }
                };
                if state.special_buffer.is_empty() && (removed || backward) {
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                } else if removed {
                    self.update_special_candidates(state);
                    let display = state.preedit.clone();
                    let caret_pos = self.overlay_caret(state);
                    self.notify_ui_update(state);
                    KeyAction::UpdateComposition {
                        text: display,
                        caret_pos,
                    }
                } else {
                    // 退格时光标已在最左 / Delete 时已在末尾：吃掉不透传。
                    KeyAction::Consumed
                }
            }
            keymap::VK_SPACE => {
                // 空格：有候选选高亮上屏（命令候选执行动作，与 `commit_highlighted` 动词同一
                // 出口）；无候选退出
                if let Some(act) = self.commit_highlighted(state) {
                    act
                } else {
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                }
            }
            keymap::VK_RETURN => {
                // clear 模式：整段放弃。须先于下方各分支，非空缓冲也要遵守（同主输入路径）。
                if self.enter_clears_composition() {
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // 空缓冲（只按了模式键、还没敲编码）：commit 模式上屏模式键符号本身
                // （原样不转换，补输被占用的符号，如 \）。
                if state.special_buffer.is_empty() {
                    if !state.special_prefix.is_empty() {
                        let sym = state.special_prefix.clone();
                        self.record_commit(
                            &sym,
                            0,
                            -1,
                            wind_store::stats::CommitSource::Punctuation,
                        );
                        self.exit_special_mode(state);
                        self.notify_ui_hide();
                        return Self::commit_action(sym, true);
                    }
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // 非空缓冲：上屏编码原文
                let text = state.special_buffer.clone();
                self.record_commit(
                    &text,
                    text.len() as u32,
                    -1,
                    wind_store::stats::CommitSource::SpecialMode,
                );
                self.exit_special_mode(state);
                self.notify_ui_hide();
                Self::commit_action(text, true)
            }
            keymap::VK_1..=keymap::VK_9 => {
                // 数字 1-9 选当前页候选（命令候选执行动作）
                let (start, end) = self.page_range(state);
                let gi = start + (data.key_code - 0x31) as usize;
                if gi < end {
                    self.commit_special_candidate(state, gi)
                } else {
                    KeyAction::Consumed
                }
            }
            keymap::VK_A..=keymap::VK_Z => {
                // 字母：小写归一，在光标处插入
                let ch = (b'a' + (data.key_code - 0x41) as u8) as char;
                preedit_cursor::BufEdit::new(&mut state.special_buffer, &mut state.special_cursor)
                    .insert(ch);
                if let Some(cand) = self.update_special_candidates(state) {
                    return self.special_auto_commit(state, cand);
                }
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    text: display,
                    caret_pos,
                }
            }
            _ => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                // 反查模式的符号通配键（如 `?`）进缓冲作通配，须先于选词键与标点顶屏。
                // 字母通配键走上面的字母臂，不经这里。
                if matches!(state.active, Some(ModeKind::Reverse))
                    && let Some(ch) = punct_char(data.key_code, shift)
                    && self.engine_mgr.active_reverse_key() == Some(ch)
                {
                    preedit_cursor::BufEdit::new(
                        &mut state.special_buffer,
                        &mut state.special_cursor,
                    )
                    .insert(ch);
                    self.update_special_candidates(state);
                    let display = state.preedit.clone();
                    let caret_pos = self.overlay_caret(state);
                    self.notify_ui_update(state);
                    return KeyAction::UpdateComposition {
                        text: display,
                        caret_pos,
                    };
                }
                // 二三候选键 → 选候选（命令候选执行动作）
                if !shift && let Some(offset) = self.select_key_offset(data.key_code) {
                    let (start, end) = self.page_range(state);
                    let gi = start + offset;
                    if gi < end {
                        return self.commit_special_candidate(state, gi);
                    }
                }
                // 其它可打印标点：顶屏当前高亮候选 + 转换后标点，退出。
                // 小键盘键（direct 语义）回退 numpad_char 复用此路：特殊模式缓冲是编码，
                // 数字非法 → 顶屏候选再输出该字符，与主路径 direct 同构。follow_main 时键已在
                // 入口归一化为主键盘键，走上面的数字选词臂。
                if let Some(ch) =
                    punct_char(data.key_code, shift).or_else(|| numpad_char(data.key_code))
                {
                    let hi = if state.candidates.is_empty() {
                        None
                    } else {
                        Some(
                            self.highlighted_global_index(state)
                                .min(state.candidates.len() - 1),
                        )
                    };
                    // 高亮候选为组/命令：走统一选中（组→展开重查，命令→执行动作），
                    // 触发标点不单独上屏（语义同 top_commit_command_guard）。
                    if let Some(idx) = hi
                        && (state.candidates[idx].is_group || state.candidates[idx].is_command)
                    {
                        return self.commit_special_candidate(state, idx);
                    }
                    let committed = match hi {
                        Some(idx) => {
                            // 顶屏也是一次选中：记账同选词出口（`record_special_selection`）。
                            let cand = state.candidates[idx].clone();
                            self.record_special_selection(state, &cand);
                            self.record_commit(
                                &cand.text,
                                state.special_buffer.len() as u32,
                                -1,
                                wind_store::stats::CommitSource::SpecialMode,
                            );
                            cand.text
                        }
                        // 无候选：与主路空码标点同一判据（`punct_on_empty_behavior`）。字母引导符
                        // 算码，理由同临拼（见 `temp_pinyin_punct_without_candidate`）。
                        // 小键盘（direct）维持既有口径（同主路 `commit_highlight_then_char`）。
                        None if punct_char(data.key_code, shift).is_none() => String::new(),
                        None => {
                            let guide = Self::guide_to_return(&state.special_prefix, "");
                            let raw = format!("{guide}{}", state.special_buffer);
                            match self.punct_empty_code_policy_for(state, &raw) {
                                PunctEmptyCodePolicy::ClearNoInput => {
                                    self.exit_special_mode(state);
                                    self.notify_ui_hide();
                                    return KeyAction::ClearComposition;
                                }
                                PunctEmptyCodePolicy::Clear => String::new(),
                                PunctEmptyCodePolicy::Commit => {
                                    if !raw.is_empty() {
                                        self.record_commit(
                                            &raw,
                                            raw.len() as u32,
                                            -1,
                                            wind_store::stats::CommitSource::SpecialMode,
                                        );
                                        // 原码类上屏进上屏历史（不含标点，同主路）；原码不记词频。
                                        self.push_commit_history(&raw);
                                    }
                                    raw
                                }
                            }
                        }
                    };
                    let punct = self.convert_punct_char(state, ch);
                    self.record_commit(&punct, 0, -1, wind_store::stats::CommitSource::Punctuation);
                    self.exit_special_mode(state);
                    self.notify_ui_hide();
                    Self::commit_action(format!("{}{}", committed, punct), true)
                } else {
                    KeyAction::Consumed
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "handle_special_tests.rs"]
mod tests;
