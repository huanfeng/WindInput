//! 临时拼音 / 临时英文输入模式
//!
//! 触发键判定、进入/退出、候选刷新、按键处理、选词上屏。

use crate::coordinator::{
    CommittedSeg, Coordinator, ENGINE_MAX_CANDIDATES, PunctEmptyCodePolicy, State,
    TEMP_PINYIN_MAX_CANDIDATES, numpad_char, punct_char,
};
use crate::key_convert::printable_char;
use crate::pipeline::{ModeKind, Rewind, RewindOrigin};
use crate::preedit_cursor;
use tracing::debug;
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::{Candidate, CandidateSource};
use wind_config::config::RawCandidateMode;
use wind_engine::manager::ENGLISH_SCHEMA;
use wind_ipc::protocol::{MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;
use wind_transform::fullwidth::to_full_width;

impl Coordinator {
    /// VK → 组合区前缀字符（统一映射，见 `keymap`；缺省回退反引号）。
    ///
    /// 用带字母的版本：z 进临拼时组合区要显示 `z`；否则会兜底成反引号，回车归还引导字母
    /// 的判据（首字符是否字母）也永远不成立。
    pub(crate) fn temp_pinyin_prefix_for(key_code: u32) -> char {
        keymap::vk_to_prefix_char_with_letters(key_code).unwrap_or('`')
    }

    /// 当前按键是否为临时拼音的引导键。
    ///
    /// 数据源是 [`Self::bound_action_for`]（方案表 → 全局 `keys.key_actions` → `z_key_action`）；
    /// `input.temp_pinyin.trigger_keys` 已由 `normalize` 折算进 `keys.key_actions`。
    /// 「模式内二次按同键退出」「智能符号 press2 门控」「设置页冲突检测」共用本函数，
    /// 方案级绑定的引导键因此对它们同样可见。
    pub(crate) fn is_temp_pinyin_trigger(&self, key_code: u32) -> bool {
        matches!(
            self.bound_action_for(key_code),
            Some(wind_config::BoundAction::TempPinyin)
        )
    }

    /// z 三重身份裁决的「活码前缀」判据（对齐 Go `HasPrefix`）：码表引擎候选（含 BFS 前缀扫描）
    /// 或短语层存在以 `code` 开头的条目 → true。用于区分 z 作正常码字母（有前缀，如自定义 `zhang`）
    /// vs 临时拼音触发（死前缀，如标准五笔 86 的 z）。
    ///
    /// 这是前瞻的存在性判据（前缀查询用 `1`），与 `build_candidates` 的显示门槛
    /// `input.phrase.min_prefix` 刻意不同源：对齐到任一边都会坏——判据降到 2 则 `zz*` 在该方案
    /// 全部消失，产出提到 1 则 `min_prefix` 形同虚设。让位那一帧的反馈由
    /// [`Self::inject_bound_letter_hint`] 提供。见 `docs/design/schema-key-actions.md` §4.2.1。
    pub(crate) fn has_code_prefix(&self, code: &str) -> bool {
        if code.is_empty() {
            return false;
        }
        // 码表 / 用户词（convert 内含前缀扫描）。
        if !self.engine_mgr.convert(code, 1).candidates.is_empty() {
            return true;
        }
        // 短语层：精确或前缀命中。
        let phrases = self.phrases.read().unwrap_or_else(|e| e.into_inner());
        if phrases.is_empty() {
            return false;
        }
        let recent = self.recent_commits_snapshot();
        // 空宿主：本判据只问有没有这个码的短语，不值得在逐键路径上读剪贴板 / 查词库。
        // 代价是依赖瞬时状态的纯模板短语（如 `{dict.rev(clip())}`）在这里判为不存在；
        // 当前调用方的前缀下都还有别的短语撑着，不影响判定。
        let host = wind_phrase::PhraseHost::empty();
        // 短语作用域取活跃方案：本判据只在普通输入态被调用，那时与 `effective_data_schema`
        // 恒等。若日后在 overlay 语境里调用，要改成传 state 走 `phrase_spec_of`。
        let spec = self
            .engine_mgr
            .behavior_for(&self.engine_mgr.active_schema_id())
            .phrases
            .clone();
        let scope = crate::schema_scope::phrase_scope(&spec);
        // 存在性判据用 `1`：只问「有没有以 code 开头的短语」，与显示门槛无关（见函数文档）。
        !phrases.lookup(code, &recent, &host, &scope).is_empty()
            || !phrases.lookup_prefix(code, &recent, 1, &scope).is_empty()
    }

    /// 目标模式能否接住这个残余字符：夺取的前提是残余码在目标里有意义。
    ///
    /// | 目标 | 接受 | why |
    /// |---|---|---|
    /// | `temp_pinyin` | 仅字母 | 拼音里数字/符号没有意义，`z1` 的 1 仍该是选词键 |
    /// | `temp_english` | 字母+数字+符号 | 收英文原文，`z-` → `-` 是合法开头 |
    /// | `mix:<id>` | 字母+数字+符号 | 收自由输入，算式与日期恰恰要数字和运算符 |
    /// | `rare_char` | 仅字母 | 收当前方案的编码，数字仍该是选词键 |
    /// | `reverse` | 仅字母 | 同生僻字 |
    fn z_fallback_accepts(action: &wind_config::BoundAction, ch: char) -> bool {
        use wind_config::BoundAction as BA;
        match action {
            BA::TempPinyin => ch.is_ascii_alphabetic(),
            BA::RareChar => ch.is_ascii_alphabetic(),
            BA::Reverse => ch.is_ascii_alphabetic(),
            BA::TempEnglish | BA::Mix(_) => ch.is_ascii_graphic(),
            // special 不走夺取（见本函数文档），其余动词与夺取无关。
            _ => false,
        }
    }

    /// z-fallback 夺取（对齐 Go `decideEngineDefaultZFallback`）：码表引擎、缓冲以 z 开头、
    /// z 绑了可夺取的目标，且加新键 `ch` 后 `z…` 不再是活码前缀，则抛弃首 z，以
    /// `buffer[1..] + ch` 切入目标模式并武装退格 rewind。返回 `Some` 表示已夺取。
    /// 混输引擎排除（避免 `zhang` 丢首字母）。
    ///
    /// 必须有这条回路：出厂 `zz*` 短语让首键 z 恒被活码判据让位，不夺取的话 z 绑的模式永远
    /// 进不去。快符类刻意排除（残余码在符号表里多半查不到，且它要的是进入即浏览整表）。
    /// 这里刻意不查 `z_key_repeat`：让位只维持一个按键，继续打字母就说明不是要重复上屏；
    /// 改成互斥的话开了 repeat 的方案上 z 一条进入通路都不剩。
    /// 见 `docs/design/schema-key-actions.md` §4.2。
    pub(crate) fn try_z_fallback(&self, state: &mut State, ch: char) -> Option<KeyAction> {
        if !matches!(
            self.engine_mgr.current_engine_type(),
            Some(wind_engine::EngineType::CodeTable)
        ) {
            return None;
        }
        if !state.input_buffer.starts_with('z') {
            return None;
        }
        // 走 `bound_action_for` 而非 `z_key_action()`：方案级绑定必须压过全局，否则首键判定
        // 与夺取对同一个键是两个身份。
        let action = self.bound_action_for(wind_keys::keymap::VK_Z)?;
        // 放在活码判据之前：更便宜，且不通过时该键本就该走原身份（数字选词 / 标点上屏）。
        if !Self::z_fallback_accepts(&action, ch) {
            return None;
        }
        // 缓冲已长过让位帧（如 `zzbd`）且有候选时，非字母键是在对候选下手（选词 / 顶屏），
        // 不是残余码；否则 `zzbd2` 会被夺取，系统短语分组选不了词。让位帧（缓冲恰为 `z`）
        // 不在此列：`z1+2` 必须照样夺取进 mix 算数。字母不受限。
        if state.input_buffer.len() > 1 && !state.candidates.is_empty() && !ch.is_ascii_alphabetic()
        {
            return None;
        }
        let combined = format!("{}{}", state.input_buffer, ch);
        // 加新键后仍是活码前缀（如 zhang 存在时的 "zh"，或系统短语的 "zz"）→ 不夺取。
        if self.has_code_prefix(&combined) {
            return None;
        }
        // residual = 去掉首 z + 新键。
        let residual = format!("{}{}", &state.input_buffer[1..], ch);
        // snapshot = 夺取前的正常码流，不含触发夺取的这一键（与 `Rewind::snapshot` 语义一致）。
        // 不能取 `combined`：按夺取前提它没有候选，且用户从没见过那一帧，退过去像退格没生效。
        let snapshot = state.input_buffer.clone();

        // 按目标模式装载残余码。门卫没过一律返回 None（不夺取，落正常码表），
        // 与首键进入点同策略——绝不能吞键。
        let mode_name = match &action {
            wind_config::BoundAction::TempPinyin => {
                let target = self.engine_mgr.temp_pinyin_target()?;
                state.active = Some(ModeKind::TempPinyin);
                state.temp_pinyin_schema = target;
                state.temp_pinyin_buffer = residual.clone();
                state.temp_pinyin_cursor = state.temp_pinyin_buffer.len();
                state.temp_pinyin_prefix = "z".to_string();
                "temp pinyin"
            }
            wind_config::BoundAction::TempEnglish => {
                if !self.rt().config.input.temp_english.enabled {
                    return None;
                }
                state.active = Some(ModeKind::TempEnglish);
                state.temp_english_buffer = residual.clone();
                state.temp_english_cursor = state.temp_english_buffer.len();
                state.temp_english_prefix = "z".to_string();
                "temp English"
            }
            wind_config::BoundAction::Mix(id) => {
                let idx = self.mix_mode_idx(id)?;
                if !self.mix_has_quick_input(idx) && self.mix_members(idx).is_empty() {
                    return None;
                }
                state.active = Some(ModeKind::Mix(idx));
                state.mix_id = idx;
                state.mix_buffer = residual.clone();
                state.mix_cursor = state.mix_buffer.len();
                state.mix_prefix = "z".to_string();
                "mix"
            }
            // 字段与 `enter_rare_char_mode` 同一组。
            wind_config::BoundAction::RareChar => {
                state.active = Some(ModeKind::RareChar);
                state.special_id = 0;
                state.overlay_spec = None;
                state.special_buffer = residual.clone();
                state.special_cursor = state.special_buffer.len();
                state.special_prefix = "z".to_string();
                "rare char"
            }
            // 字段与 `enter_reverse_mode` 同一组；门卫同首键进入点，没过不夺取、不吞键。
            wind_config::BoundAction::Reverse => {
                if !self.reverse_mode_available() {
                    return None;
                }
                state.active = Some(ModeKind::Reverse);
                state.special_id = 0;
                state.overlay_spec = None;
                state.special_buffer = residual.clone();
                state.special_cursor = state.special_buffer.len();
                state.special_prefix = "z".to_string();
                state.scope_relaxed = false;
                "reverse"
            }
            // 快符类不走夺取（残余码在符号表里查不到，且它要的是「进入即浏览」）；
            // None 表示本方案没给 z 绑任何东西。
            _ => return None,
        };

        state.rewind = Some(Rewind {
            snapshot,
            host_text: residual,
            origin: RewindOrigin::Normal, // z 夺取抢的是正常码表输入流
        });
        state.input_buffer.clear();
        state.candidates.clear();
        match state.active {
            Some(ModeKind::TempPinyin) => self.update_temp_pinyin_candidates(state),
            Some(ModeKind::TempEnglish) => self.update_temp_english_candidates(state),
            Some(ModeKind::Mix(_)) => self.update_mix_candidates(state),
            // 返回值是「全码唯一自动上屏」的请求。残余码 = `buffer[1..] + ch`，让位期间缓冲
            // 可沿 `zz*` 活码一路长到全码（`zzbd` + 字母 = 4 码），故必须接住。
            Some(ModeKind::RareChar) => {
                if let Some(cand) = self.update_special_candidates(state) {
                    return Some(self.special_auto_commit(state, cand));
                }
            }
            // 反查模式不自动上屏（通配查询没有「全码唯一」一说）。
            Some(ModeKind::Reverse) => self.update_reverse_candidates(state),
            None
            | Some(
                ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::Special(_)
                | ModeKind::AuxCode,
            ) => {}
        }
        let display = state.preedit.clone();
        self.notify_ui_update(state);
        debug!("z-fallback: hijacked to {mode_name}");
        Some(KeyAction::UpdateComposition {
            text: display.clone(),
            caret_pos: display.chars().count() as u32,
        })
    }

    /// 当前按键是否为临时英文的引导键。来源与理由同 [`Self::is_temp_pinyin_trigger`]。
    pub(crate) fn is_temp_english_trigger(&self, key_code: u32) -> bool {
        matches!(
            self.bound_action_for(key_code),
            Some(wind_config::BoundAction::TempEnglish)
        )
    }

    /// 临拼：回退最后一个已转换段——把它消费的码并回缓冲前部并重转，光标落码末尾
    /// （理由同主输入的 `pop_committed_seg`）。Backspace（段优先）与 Delete（删空后）共用。
    fn pop_temp_pinyin_seg(&self, state: &mut State) -> KeyAction {
        let Some(popped) = state.committed_segs.pop() else {
            return KeyAction::Consumed;
        };
        let raw_code = popped.raw_code;
        state.committed_text = state
            .committed_segs
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        state.temp_pinyin_buffer = format!("{}{}", raw_code, state.temp_pinyin_buffer);
        state.temp_pinyin_cursor = state.temp_pinyin_buffer.len();
        self.update_temp_pinyin_candidates(state);
        let display = state.preedit.clone();
        let caret_pos = self.overlay_caret(state);
        self.notify_ui_update(state);
        KeyAction::UpdateComposition {
            caret_pos,
            text: display,
        }
    }

    /// 退出临时拼音模式并清空相关状态（含逐步转换的已转换前缀）
    pub(crate) fn exit_temp_pinyin(&self, state: &mut State) {
        state.active = None;
        // 夺取回退登记随模式一起作废，理由同 `exit_special_mode`。
        state.rewind = None;
        state.temp_pinyin_buffer.clear();
        state.temp_pinyin_cursor = 0;
        state.temp_pinyin_schema.clear();
        state.temp_pinyin_prefix.clear();
        state.committed_text.clear();
        state.committed_segs.clear();
        state.candidates.clear();
        state.preedit.clear();
        self.reset_candidate_view(state);
    }

    /// 临拼向引擎取数的上限：目标方案是拼音类才取全量。
    ///
    /// `schema.primary_pinyin` 没有类型校验，手改指向码表方案时取全量会严重劣化（单字母候选
    /// 上千条），故非拼音类退回小批量。用 `loaded_engine_type` 而非 `schema_engine_type`：后者
    /// 每次读文件解析 TOML，本函数在逐键路径上；目标方案此时必然已加载。
    fn temp_pinyin_limit(&self, schema: &str) -> usize {
        match self.engine_mgr.loaded_engine_type(schema) {
            Some(wind_engine::EngineType::Pinyin) => TEMP_PINYIN_MAX_CANDIDATES,
            _ => ENGINE_MAX_CANDIDATES,
        }
    }

    /// 用临时拼音目标方案转换缓冲，刷新候选与组合区（前缀 + 已转换汉字 + 剩余拼音）
    pub(crate) fn update_temp_pinyin_candidates(&self, state: &mut State) {
        self.refill_temp_pinyin_candidates(state);
        self.grid_resume(state);
    }

    fn refill_temp_pinyin_candidates(&self, state: &mut State) {
        state.candidates.clear();
        // 不分批加载：主路残留的 `has_more` 不属于本模式，留着会让翻页 / 展开去走主路扩容。
        state.has_more = false;
        self.reset_candidate_view(state);
        state.overlay_forms = Default::default();
        let prefix = format!("{}{}", state.temp_pinyin_prefix, state.committed_text);
        if state.temp_pinyin_buffer.is_empty() {
            state.preedit = prefix;
            return;
        }
        let Some(schema) = self.overlay_engine_schema(state) else {
            state.preedit = format!("{}{}", prefix, state.temp_pinyin_buffer);
            state.overlay_body = state.temp_pinyin_buffer.clone();
            return;
        };
        let limit = self.temp_pinyin_limit(&schema);
        let result = self
            .engine_mgr
            .convert_with(&schema, &state.temp_pinyin_buffer, limit);
        // 候选调整（shadow）的归一编码，口径同主路径 `shadow_code_of`：双拼给出全拼码，
        // 全拼恒空串则落回击键缓冲。刻意不落 `state.shadow_code`：那是主输入路的生命周期，
        // 共用的话退出临拼后主路径会读到别的码域的残值。
        let shadow_code = if result.shadow_code.is_empty() {
            state.temp_pinyin_buffer.clone()
        } else {
            result.shadow_code.clone()
        };
        // 编码栏跟随高亮用的各形态；候选排好后按高亮（初始即首选）重算，见本函数末尾。
        state.overlay_forms = crate::coordinator::OverlayPreeditForms::of(&result);
        let display = if result.preedit_display.is_empty() {
            state.temp_pinyin_buffer.clone()
        } else {
            result.preedit_display
        };
        state.preedit = format!("{}{}", prefix, display);
        state.overlay_body = display; // 供光标换算（含引擎插入的音节分隔符，与缓冲不同形）

        // 与主路径同一套层级链排序（`candidate_display_order`）；纯 weight 排序会让前缀补全压过
        // 精确匹配。`ignore_weight` 按临拼目标方案取，不能用活跃（码表）方案的 `base_sort`。
        let ignore_weight = self.engine_mgr.base_sort_ignores_weight_of(&schema);
        // 供跨来源档位判「消费整串」与「码 == 输入」。缓冲恒 ASCII，字节长度与 `consumed_length` 同域。
        let input_str = state.temp_pinyin_buffer.clone();
        let mut candidates = result.candidates;
        wind_candidate::drop_corrected_duplicates(&mut candidates);
        // mixed=false：临拼是纯拼音 overlay，不存在码表/拼音跨来源竞争。
        candidates.sort_by(|a, b| {
            crate::handle_candidate::candidate_display_order(a, b, ignore_weight, false, &input_str)
        });
        // 截断值必须跟取数上限同源，否则只改一处会「取了 5000 条又砍回 50」。
        candidates.truncate(limit);
        // 统一展开汇聚点：临时拼音词库候选内 `$` 特殊语法在此展开（见 finalize_candidates）。
        let mut candidates = self.finalize_candidates(candidates, &state.temp_pinyin_buffer);
        // 检索范围过滤，与主路径同序：mark_common → apply_filter。必须在 finalize 之后：
        // 保留条件含 `is_command` / `is_group`，这两个标志是展开 `$CC`/`$AA` 时才置位的。
        self.mark_common(&mut candidates);
        // 用户数据（词频 / 候选调整）归属一律取临拼目标方案：拼音与码表的编码域、码位结构
        // 不同，不能混进主方案的桶。归属经 `data_schema_id` 折叠到 `"pinyin"`，与拼音方案共享。
        // 不能取 active（`apply_freq_rerank` / `apply_shadow` 薄包装即是），写端同理。
        // `overlay_engine_schema` 对临拼安全（判据只是目标方案非空）；临英不可照抄，那边含
        // `show_candidates`，见 `effective_data_schema`。
        let temp_pinyin_owner = Some(schema);
        // 词频重排（used-first）。自动补充的候选（`is_scope_filtered`）排除在重排之外，与主路径
        // 同一纪律：否则误选一次生僻字就会被顶到常用字前面。临拼也有放宽通路。
        let ctx = crate::postprocess::PassCtx {
            owner: temp_pinyin_owner.as_deref(),
            freq_code: Some(&state.temp_pinyin_buffer),
            shadow_code: Some(&shadow_code),
        };
        self.run_passes(
            state,
            &mut candidates,
            crate::postprocess::profile::TEMP_PINYIN_PRE,
            &ctx,
        );
        // 纠错候选不占首选（D6），与主路径同位：重排之后、候选调整之前。
        wind_candidate::protect_top_from_corrected(&mut candidates, input_str.len());
        // 候选调整（置顶 / 隐藏）：与主路径同序，在词频重排之后、简繁展开之前。临拼是主输入路
        // 的平行实现，主路径每加一道加工都得两边各接一次，漏接时失效完全静默。
        self.run_passes(
            state,
            &mut candidates,
            crate::postprocess::profile::TEMP_PINYIN_SHADOW,
            &ctx,
        );
        // Emoji 扩展：钉在所有加工之后（见 `apply_emoji_suggestions`）；用全局 `[input.emoji]`，
        // 与正式拼音方案表现一致。
        self.apply_emoji_suggestions(&mut candidates);
        state.candidates = candidates;
        // 简繁 1对多变体展开（约束见 expand_s2t_variants 文档）。
        self.expand_s2t_variants(state);
        // 编码栏按排序后的首选（之后随高亮）选形态，同主路 `sync_preedit_to_highlight`。
        // 引擎的 `preedit_display` 跟的是引擎序首选，按界面序重排后未必还是它。
        self.sync_preedit_to_highlight(state);
    }

    /// 临时拼音选词 —— 组合区逐步转换（C）。部分匹配并入 committed 前缀留模式内（不上屏）；
    /// 完整匹配整体上屏 committed+候选（前缀触发键不输出）+ 造词，退出。返回最终 KeyAction。
    pub(crate) fn commit_temp_pinyin_selected(
        &self,
        state: &mut State,
        cand: &Candidate,
        candidate_pos: i32,
    ) -> KeyAction {
        self.commit_temp_pinyin_selected_learn(state, cand, candidate_pos, true)
    }

    /// [`Self::commit_temp_pinyin_selected`] 的本体。`learn` = 整体上屏时是否造词并推 6b：
    /// 选词出口恒 `true`；顶屏类出口（标点顶屏）传 [`Self::top_commit_learns`]。
    pub(crate) fn commit_temp_pinyin_selected_learn(
        &self,
        state: &mut State,
        cand: &Candidate,
        candidate_pos: i32,
        learn: bool,
    ) -> KeyAction {
        // $AA/$SS 组折叠候选：补全编码到完整码并重查展开（二级选择，不上屏组名）。
        if cand.is_group {
            state.temp_pinyin_buffer = cand.group_code.clone();
            state.temp_pinyin_cursor = state.temp_pinyin_buffer.len(); // 补全到完整码：光标落末尾
            self.update_temp_pinyin_candidates(state);
            let display = state.preedit.clone();
            let caret_pos = self.overlay_caret(state);
            self.notify_ui_update(state);
            return KeyAction::UpdateComposition {
                caret_pos,
                text: display,
            };
        }
        // $CC 命令候选：执行动作（退出临拼后异步跑），不走文本/分段上屏。
        let cmd_code = state.temp_pinyin_buffer.clone();
        if let Some(act) =
            self.overlay_commit_command(state, cand, &cmd_code, |s, st| s.exit_temp_pinyin(st))
        {
            return act;
        }
        let total = state.temp_pinyin_buffer.len();
        let consumed = cand.consumed_length;
        let code = Self::cand_code(&state.temp_pinyin_buffer, cand);
        let partial =
            consumed > 0 && consumed < total && state.temp_pinyin_buffer.is_char_boundary(consumed);
        // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
        //
        // 归属取临拼目标方案，与读端同源；按 active 会写进码表的桶，开关也取码表那档。
        // 走 `_cand_in`：列表里有 emoji 候选，`is_emoji_suggestion` 守卫要一起带上。
        let temp_pinyin_owner = self.overlay_engine_schema(state);
        self.record_selection_cand_in(
            temp_pinyin_owner.as_deref(),
            &self.freq_code(&state.temp_pinyin_buffer, cand),
            cand,
        );
        // 输入统计：每次临拼选词记一段（来源临时拼音）。
        self.record_commit(
            &cand.text,
            code.len() as u32,
            candidate_pos,
            wind_store::stats::CommitSource::TempPinyin,
        );
        let raw_code = Self::raw_consumed_code(&state.temp_pinyin_buffer, consumed, partial);
        if partial {
            state.committed_segs.push(CommittedSeg {
                raw_code,
                code,
                text: cand.text.clone(),
                source: cand.source,
                boundary: cand.boundary,
                learn: cand.meta.learn_code.clone(),
            });
            state.committed_text.push_str(&cand.text);
            state.temp_pinyin_buffer = state.temp_pinyin_buffer[consumed..].to_string();
            // 分步确认消费掉前缀码：光标落剩余码末尾
            state.temp_pinyin_cursor = state.temp_pinyin_buffer.len();
            self.update_temp_pinyin_candidates(state);
            let display = state.preedit.clone();
            self.notify_ui_update(state);
            KeyAction::UpdateComposition {
                caret_pos: display.chars().count() as u32,
                text: display,
            }
        } else {
            state.committed_segs.push(CommittedSeg {
                raw_code,
                code: code.clone(),
                text: cand.text.clone(),
                source: cand.source,
                boundary: cand.boundary,
                learn: cand.meta.learn_code.clone(),
            });
            let final_simplified = format!("{}{}", state.committed_text, cand.text);
            // 单段整句同样要造词（临拼模式下整句一次上屏亦只 push 一段）。
            // 闸门与归属按临拼目标方案；取不到就不造词，传 `None` 会退回活跃方案语义。
            if learn && let Some(owner) = temp_pinyin_owner.as_deref() {
                let learned_code =
                    self.learn_phrase_on_commit_in(state, cand.is_synthesized, Some(owner));
                // 6b：选中已有临时词推进晋升计数，与主路同一函数；归属同上取临拼目标方案。
                if !cand.is_phrase {
                    self.bump_selected_temp_word(owner, cand, &code, learned_code.as_deref());
                }
            }
            // 变体候选末段用覆盖文本；普通候选整体转换（保留 STPhrases 跨段词级消歧）。
            let out = match &cand.s2t_override {
                Some(t) => format!("{}{}", self.maybe_convert(state, &state.committed_text), t),
                None => self.maybe_convert(state, &final_simplified),
            };
            self.exit_temp_pinyin(state);
            self.notify_ui_hide();
            Self::commit_action(out, true)
        }
    }

    /// 这个键在临拼下是否应作为非字母码元进缓冲；是则返回进缓冲的小写字符。
    ///
    /// 双拼布局把韵母放在符号键上（如 `;` = ing），而主输入路的码元闸门
    /// [`Coordinator::try_code_char_gate`] 在分派到 `handle_temp_pinyin_key` 之后，临拼走不到。
    /// 判据按 overlay 方案取（主输入路问的是活跃引擎）。全拼与纯字母布局的双拼下恒返回 None。
    /// 缓冲空时查首码集，否则查全集，与主输入路一致。字母不走这里：它们有专门的累积臂，
    /// 那条臂不问码元集，免得布局解析失败时整个临拼打不了字。
    fn temp_pinyin_code_char(&self, state: &State, data: &KeyEventData) -> Option<char> {
        let ch = printable_char(data.key_code, data.modifiers & MOD_SHIFT != 0)?;
        if ch.is_ascii_alphabetic() {
            return None;
        }
        let lower = ch.to_ascii_lowercase();
        let schema = self.overlay_engine_schema(state)?;
        let accepted = if state.temp_pinyin_buffer.is_empty() {
            self.engine_mgr.is_leading_char_of(&schema, lower)
        } else {
            self.engine_mgr.is_code_char_of(&schema, lower)
        };
        accepted.then_some(lower)
    }

    /// 临时拼音模式下的按键处理
    pub(crate) fn handle_temp_pinyin_key(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> KeyAction {
        // Ctrl/Alt 组合守卫（见 `overlay_ctrl_alt_guard`）：必须最先，否则组合键会落到
        // 下方各臂被当成普通输入。临拼有逐步转换的已转换前缀，故 committed_text 也算待输入。
        if let Some(act) = self.overlay_ctrl_alt_guard(
            state,
            data,
            !state.temp_pinyin_buffer.is_empty() || !state.committed_text.is_empty(),
            |s, st| s.exit_temp_pinyin(st),
        ) {
            return act;
        }
        if let Some(act) = self.handle_candidate_nav(state, data) {
            return act;
        }
        // 目标方案 `[key_actions]` 里绑的辅助码触发键（如出厂 shuangpin 的 `backtick = "aux_code"`）。
        // 辅助码筛的是这批拼音候选，故按产出候选的方案取（见 docs/design/key-resolver-unification.md
        // §4.4）；整张表不随目标方案走，本处只认 `aux_code`。放在 `handle_candidate_nav` 之后：
        // 全局/主方案的 session_actions 优先。门卫在 `enter_aux_code`，不通过则返回 None 不吞键。
        // 字母不参与：辅助码态里字母恒是码元，否则配过 `z = "aux_code"` 的用户在临拼里打不出 z。
        if data.modifiers & MOD_SHORTCUT == 0
            && !(keymap::VK_A..=keymap::VK_Z).contains(&data.key_code)
            && let Some(schema) = self.overlay_engine_schema(state)
            && matches!(
                self.bound_action_in_schema(data.key_code, &schema),
                Some(wind_config::BoundAction::AuxCode)
            )
            && let Some(act) = self.enter_aux_code(state, data.key_code)
        {
            return act;
        }
        // 编码区光标移动（左右 / Home / End）；置于候选导航之后，导航键优先。
        if let Some(act) = self.overlay_cursor_key(state, data) {
            return act;
        }
        // 进入键二次按下（缓冲空 + 无已转换前缀）：按中英标点配置上屏该符号并退出，并武装智能
        // 符号（时限内再按同键换英文形，press2 拦截在 try_activate_mode 开头）。
        //
        // 上屏字符取当前按键，不取 `temp_pinyin_prefix`：临拼没有实例 id，多个键同绑临拼是常态
        // （如 `` ` `` 与 z），取进入键会让 `` ` `` 进临拼后按 z 上屏 `·`。字母被 `punct_char`
        // 挡在门外，落下方字母臂累积拼音。与 special、mix 的同名分支同构。
        if state.temp_pinyin_buffer.is_empty()
            && state.committed_text.is_empty()
            && self.is_temp_pinyin_trigger(data.key_code)
            && let Some(ch) = punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
        {
            let out = self.convert_punct_char(state, ch);
            self.arm_smart_symbol_after_commit(state, ch, &out);
            self.record_commit(&out, 0, -1, wind_store::stats::CommitSource::Punctuation);
            self.exit_temp_pinyin(state);
            self.notify_ui_hide();
            return Self::commit_action(out, true);
        }
        match data.key_code {
            // Esc：放弃退出。实现收口在 `cancel_session`（按 `state.active` 分派回
            // `exit_temp_pinyin`），与绑了 `cancel` 的自定义键共用同一条路径。
            keymap::VK_ESCAPE => self.cancel_session(state),
            keymap::VK_BACK | keymap::VK_DELETE => {
                // Backspace：段回退优先于光标（有已转换段先退回最后一段，你→ni，码并回缓冲
                // 前部）；否则删光标前一字符。Delete 只删光标后一字符、删空后才回退段——与主输入
                // 同构的刻意不对称（见 coordinator.rs 的 VK_DELETE 臂）。皆空则退出。
                let backward = data.key_code == keymap::VK_BACK;
                if backward && !state.committed_segs.is_empty() {
                    return self.pop_temp_pinyin_seg(state);
                }
                if state.temp_pinyin_buffer.is_empty() {
                    if backward {
                        self.exit_temp_pinyin(state);
                        self.notify_ui_hide();
                        return KeyAction::ClearComposition;
                    }
                    // Delete 且剩余拼音已空（只剩只读前缀）：吃掉，不改变退出语义。
                    return KeyAction::Consumed;
                }
                let removed = {
                    let mut ed = preedit_cursor::BufEdit::new(
                        &mut state.temp_pinyin_buffer,
                        &mut state.temp_pinyin_cursor,
                    );
                    if backward {
                        ed.backspace()
                    } else {
                        ed.delete()
                    }
                };
                if !removed {
                    // 退格时光标已在最左 / Delete 时已在末尾：吃掉不透传。
                    return KeyAction::Consumed;
                }
                if state.temp_pinyin_buffer.is_empty() {
                    if !state.committed_segs.is_empty() {
                        return self.pop_temp_pinyin_seg(state);
                    }
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                self.update_temp_pinyin_candidates(state);
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    caret_pos,
                    text: display,
                }
            }
            keymap::VK_SPACE => {
                // 空格：选当前高亮候选（逐步转换），与 `commit_highlighted` 动词同一出口。
                if let Some(act) = self.commit_highlighted(state) {
                    act
                } else {
                    // 无候选（空码空格）：按 space_on_empty_behavior，与主路同一判据——
                    // "clear" 连已选段一起丢；否则上屏「引导字母 + 已选段 + 剩余原码」，
                    // 引导字母归还同回车（见 `guide_to_return`），快捷输入同一臂同口径。
                    if self.rt().config.input.space_on_empty_behavior == "clear" {
                        self.exit_temp_pinyin(state);
                        self.notify_ui_hide();
                        return KeyAction::ClearComposition;
                    }
                    let guide =
                        Self::guide_to_return(&state.temp_pinyin_prefix, &state.committed_text);
                    // committed 段已在各次选词记过，此处只记本次实际上屏的原码避免重复。
                    let raw = format!("{}{}", guide, state.temp_pinyin_buffer);
                    self.record_commit(
                        &raw,
                        raw.len() as u32,
                        -1,
                        wind_store::stats::CommitSource::TempPinyin,
                    );
                    let raw_text = format!(
                        "{}{}{}",
                        guide, state.committed_text, state.temp_pinyin_buffer
                    );
                    let out = self.maybe_convert(state, &raw_text);
                    // 原码类上屏也进上屏历史（转换前形态，同回车）；原码不记词频。
                    self.push_commit_history(&raw_text);
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    if out.is_empty() {
                        KeyAction::ClearComposition
                    } else {
                        Self::commit_action(out, true)
                    }
                }
            }
            keymap::VK_RETURN => {
                // clear 模式：整段放弃，不上屏任何内容（含已选词的 committed_text）。
                // 须先于下方各分支，否则「打了码再回车」仍会上屏原码。
                if self.enter_clears_composition() {
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // 空缓冲（只按了模式键、无已转换前缀）：commit 模式上屏模式键符号本身
                // （原样不转换，如 `）。
                if state.temp_pinyin_buffer.is_empty() && state.committed_text.is_empty() {
                    if !state.temp_pinyin_prefix.is_empty() {
                        let sym = state.temp_pinyin_prefix.clone();
                        self.record_commit(
                            &sym,
                            0,
                            -1,
                            wind_store::stats::CommitSource::Punctuation,
                        );
                        self.exit_temp_pinyin(state);
                        self.notify_ui_hide();
                        return Self::commit_action(sym, true);
                    }
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // 非空缓冲：上屏「引导字母 + 已转换前缀 + 剩余拼音原码」
                // （符号引导键行为不变，如 `nihao → nihao）。
                //
                // 引导字母要还回去（zhang → zhang，而非 hang），见 `guide_to_return`。
                // z-fallback 的 z 是从 `input_buffer` 抢走的真实击键，还回去恰好复原。
                let guide = Self::guide_to_return(&state.temp_pinyin_prefix, &state.committed_text);
                // committed 段已在选词时记过，此处只记本次实际上屏的原码避免重复。
                let raw = format!("{}{}", guide, state.temp_pinyin_buffer);
                self.record_commit(
                    &raw,
                    raw.len() as u32,
                    -1,
                    wind_store::stats::CommitSource::TempPinyin,
                );
                let raw_text = format!(
                    "{}{}{}",
                    guide, state.committed_text, state.temp_pinyin_buffer
                );
                let out = self.maybe_convert(state, &raw_text);
                // 原码类上屏也进上屏历史（`;` 重复上屏取得到）。记转换前形态（与选词出口
                // 一致）：重复上屏时会再过一次简繁转换。原码不记词频。
                self.push_commit_history(&raw_text);
                self.exit_temp_pinyin(state);
                self.notify_ui_hide();
                if out.is_empty() {
                    KeyAction::ClearComposition
                } else {
                    Self::commit_action(out, true)
                }
            }
            // 展开态下 0 选第 10 格（序号顺延时标「0」的那格）；收起态的 0 不在此臂。
            keymap::VK_0..=keymap::VK_9
                if data.modifiers & MOD_SHIFT == 0
                    && (data.key_code != keymap::VK_0 || Self::grid_active(state)) =>
            {
                // 数字键选当前页第 N 个
                let off = if data.key_code == keymap::VK_0 {
                    9
                } else {
                    (data.key_code - 0x31) as usize
                };
                let (start, end) = self.select_range(state);
                let idx = start + off;
                if idx < end {
                    let cand = state.candidates[idx].clone();
                    self.commit_temp_pinyin_selected(state, &cand, off as i32)
                } else {
                    KeyAction::Consumed
                }
            }
            k if let Some(ch) = numpad_char(k) => {
                // 小键盘 direct 语义（follow_main 时键已在入口归一化为主键盘键，走上面的数字
                // 选词臂）：拼音缓冲是编码，数字不是合法拼音 → 顶屏「已转换前缀 + 当前高亮
                // 候选」再接着输出该字符并退出，已打的码不丢。
                if !state.candidates.is_empty() {
                    let idx = self
                        .highlighted_global_index(state)
                        .min(state.candidates.len() - 1);
                    let cand = state.candidates[idx].clone();
                    let code = state.temp_pinyin_buffer.clone();
                    // 命令候选：执行命令，不追加字符（与主路径 direct 一致）。
                    if let Some(act) = self
                        .overlay_commit_command(state, &cand, &code, |s, st| s.exit_temp_pinyin(st))
                    {
                        return act;
                    }
                    // 记账码见 `freq_code`：临拼缓冲是击键域（双拼下 `siyr`），与候选码不同域。
                    // 归属取临拼目标方案；走 `_cand_in` 带 emoji 守卫。
                    let temp_pinyin_owner = self.overlay_engine_schema(state);
                    self.record_selection_cand_in(
                        temp_pinyin_owner.as_deref(),
                        &self.freq_code(&code, &cand),
                        &cand,
                    );
                    self.record_commit(
                        &cand.text,
                        code.len() as u32,
                        (idx - self.page_range(state).0) as i32,
                        wind_store::stats::CommitSource::TempPinyin,
                    );
                    // 造词 / 6b：归属取临拼目标方案，取不到不学（同 `commit_temp_pinyin_selected`）。
                    if let Some(owner) = temp_pinyin_owner.as_deref() {
                        self.learn_on_top_commit(state, &cand, &code, Some(owner));
                    }
                    state.committed_text.push_str(&cand.text);
                }
                let head = self.maybe_convert(state, &state.committed_text.clone());
                // 小键盘恒半角：不转全角（临拼 direct 档的落点）。
                let tail = if state.full_width && !self.numpad_raw_output(state) {
                    to_full_width(&ch.to_string())
                } else {
                    ch.to_string()
                };
                self.record_commit(&tail, 0, -1, wind_store::stats::CommitSource::Punctuation);
                self.exit_temp_pinyin(state);
                self.notify_ui_hide();
                Self::commit_action(format!("{}{}", head, tail), true)
            }
            // 手动音节分隔符：把 `'` 压入临拼缓冲作硬边界，与主输入路
            // （`message_handler.rs` 的 VK_QUOTE|VK_BACKTICK 臂）同构。
            // 判据必须走 `manual_separator_key_of(.., overlay 方案)`：`manual_separator_key` 问的是
            // 活跃引擎，五笔方案下恒 false，键会落到兜底臂把首选打出去。
            // 排在「引导键二次按下」之后：反引号出厂即临拼引导键，那条要求缓冲为空，本条要求非空。
            keymap::VK_QUOTE | keymap::VK_BACKTICK
                if data.modifiers & MOD_SHIFT == 0
                    && !state.temp_pinyin_buffer.is_empty()
                    && self
                        .overlay_engine_schema(state)
                        .is_some_and(|s| self.manual_separator_key_of(data.key_code, &s)) =>
            {
                preedit_cursor::BufEdit::new(
                    &mut state.temp_pinyin_buffer,
                    &mut state.temp_pinyin_cursor,
                )
                .insert('\'');
                self.update_temp_pinyin_candidates(state);
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    text: display,
                    caret_pos,
                }
            }
            // 非字母码元（双拼布局的符号韵母键，如 `;` = ing），见 [`Self::temp_pinyin_code_char`]。
            // 刻意排在其它具名臂之后、只抢兜底臂的键（否则 `;` 会被当成次选键把首选打出去）；
            // 主输入路的闸门更靠前，这里保守即可，因为双拼码元集与上面那些键不相交。
            _ if data.modifiers & MOD_SHORTCUT == 0
                && let Some(ch) = self.temp_pinyin_code_char(state, data) =>
            {
                preedit_cursor::BufEdit::new(
                    &mut state.temp_pinyin_buffer,
                    &mut state.temp_pinyin_cursor,
                )
                .insert(ch);
                self.update_temp_pinyin_candidates(state);
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    text: display,
                    caret_pos,
                }
            }
            // 守卫在入口 `overlay_ctrl_alt_guard` 之后已恒真，保留作纵深防御。
            keymap::VK_A..=keymap::VK_Z if data.modifiers & MOD_SHORTCUT == 0 => {
                // 字母累积拼音
                let ch = (b'a' + (data.key_code - 0x41) as u8) as char;
                preedit_cursor::BufEdit::new(
                    &mut state.temp_pinyin_buffer,
                    &mut state.temp_pinyin_cursor,
                )
                .insert(ch);
                self.update_temp_pinyin_candidates(state);
                let display = state.preedit.clone();
                let caret_pos = self.overlay_caret(state);
                self.notify_ui_update(state);
                KeyAction::UpdateComposition {
                    text: display,
                    caret_pos,
                }
            }
            _ => {
                // 二三候选键
                if data.modifiers & MOD_SHIFT == 0
                    && let Some(offset) = self.select_key_offset(data.key_code)
                {
                    let (start, end) = self.select_range(state);
                    let idx = start + offset;
                    if idx < end {
                        let cand = state.candidates[idx].clone();
                        return self.commit_temp_pinyin_selected(state, &cand, offset as i32);
                    }
                }
                // 其它键：有候选则上屏高亮候选（分段则保留剩余拼音）；否则退出清空。
                if !state.candidates.is_empty() {
                    let (start, _) = self.page_range(state);
                    let idx = (start + state.selected_index).min(state.candidates.len() - 1);
                    let cand = state.candidates[idx].clone();
                    // 标点键：顶屏高亮候选，标点跟着一起上屏（按中英标点配置转换），对齐快捷输入、
                    // 临英与主输入路 `commit_highlight_then_char`。只接文本候选，组 / 命令候选
                    // 仍走 `commit_temp_pinyin_selected`。高亮停在分步候选时同主路拼音：顶屏
                    // 「已选段 + 该候选」+ 标点，剩余码丢弃并退出（造词只推 6b）。
                    let total = state.temp_pinyin_buffer.len();
                    let partial = cand.consumed_length > 0
                        && cand.consumed_length < total
                        && state
                            .temp_pinyin_buffer
                            .is_char_boundary(cand.consumed_length);
                    if !cand.is_group
                        && !cand.is_command
                        && let Some(ch) = punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
                    {
                        let punct = self.convert_punct_after(state, ch, 0, &cand.text);
                        if partial {
                            let out = self.temp_pinyin_partial_top_commit(
                                state,
                                &cand,
                                (idx - start) as i32,
                            );
                            self.record_commit(
                                &punct,
                                0,
                                -1,
                                wind_store::stats::CommitSource::Punctuation,
                            );
                            return Self::commit_action(format!("{out}{punct}"), true);
                        }
                        // 顶屏类出口：造词 / 6b 跟随 `input.top_commit_learn`。
                        let act = self.commit_temp_pinyin_selected_learn(
                            state,
                            &cand,
                            (idx - start) as i32,
                            self.top_commit_learns(),
                        );
                        return match act {
                            KeyAction::InsertText { text, .. } => {
                                self.record_commit(
                                    &punct,
                                    0,
                                    -1,
                                    wind_store::stats::CommitSource::Punctuation,
                                );
                                Self::commit_action(format!("{text}{punct}"), true)
                            }
                            other => other,
                        };
                    }
                    // 兜底：非标点键（吞键）/ 组 / 命令 / 分步候选。恒造词，不读
                    // `input.top_commit_learn`：键被吞掉了，语义就是选中高亮候选，同空格选词。
                    self.commit_temp_pinyin_selected(state, &cand, (idx - start) as i32)
                } else if data.modifiers & MOD_SHIFT == 0
                    && self.select_key_offset(data.key_code).is_some()
                {
                    // 二三候选键越界（一条候选都没有）：同主路 `handle_overflow_select_key` 的
                    // 无候选分支，按 `keys.overflow.select_key` 处置——`ignore`（出厂）吞键、
                    // 组合留着继续打；`commit` 丢码退出；`commit_and_input` 丢码并出该键字符。
                    let key_char = punct_char(data.key_code, false);
                    match self.rt().config.keys.overflow.select_key.as_str() {
                        "commit" => {
                            self.exit_temp_pinyin(state);
                            self.notify_ui_hide();
                            KeyAction::ClearComposition
                        }
                        "commit_and_input" => {
                            let piece = key_char
                                .map(|c| self.convert_punct(state, c, data.prev_char))
                                .unwrap_or_default();
                            self.exit_temp_pinyin(state);
                            self.notify_ui_hide();
                            if !piece.is_empty() {
                                self.record_commit(
                                    &piece,
                                    0,
                                    -1,
                                    wind_store::stats::CommitSource::Punctuation,
                                );
                            }
                            Self::commit_action(piece, true)
                        }
                        _ => KeyAction::Consumed,
                    }
                } else if let Some(ch) = punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
                {
                    self.temp_pinyin_punct_without_candidate(state, ch)
                } else {
                    self.exit_temp_pinyin(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                }
            }
        }
    }

    /// 临拼高亮停在分步候选时的顶屏（标点臂用）：上屏「已选段 + 该候选」，丢弃剩余码
    /// 并退出临拼——与主路拼音标点顶屏同口径（那里顶屏后清缓冲）。返回要上屏的文本
    /// （不含标点）。
    ///
    /// 记账同 `commit_temp_pinyin_selected`：词频归临拼目标方案、输入统计按临拼来源记一段。
    /// 造词 / 6b 走 `learn_on_top_commit`：分步候选不造词、只推 6b。
    fn temp_pinyin_partial_top_commit(
        &self,
        state: &mut State,
        cand: &Candidate,
        candidate_pos: i32,
    ) -> String {
        let owner = self.overlay_engine_schema(state);
        let code = Self::cand_code(&state.temp_pinyin_buffer, cand);
        self.record_selection_cand_in(
            owner.as_deref(),
            &self.freq_code(&state.temp_pinyin_buffer, cand),
            cand,
        );
        self.record_commit(
            &cand.text,
            code.len() as u32,
            candidate_pos,
            wind_store::stats::CommitSource::TempPinyin,
        );
        if let Some(owner) = owner.as_deref() {
            let buf = state.temp_pinyin_buffer.clone();
            self.learn_on_top_commit(state, cand, &buf, Some(owner));
        }
        // 变体候选末段用覆盖文本；普通候选整体转换（同 `commit_temp_pinyin_selected`）。
        let out = match &cand.s2t_override {
            Some(t) => format!("{}{}", self.maybe_convert(state, &state.committed_text), t),
            None => self.maybe_convert(state, &format!("{}{}", state.committed_text, cand.text)),
        };
        self.exit_temp_pinyin(state);
        self.notify_ui_hide();
        out
    }

    /// 临拼无候选时按标点：与主路空码标点同一套语义（`input.punct_on_empty_behavior`，
    /// 判据 `punct_empty_code_policy_for`）。`commit` 上屏「引导字母 + 已选段 + 剩余原码」
    /// 再接标点（原码记统计与上屏历史，同空格 / 回车臂）；`clear` 全丢只出标点；
    /// `clear_no_input` 连标点也不出。无论哪档都退出临拼。
    fn temp_pinyin_punct_without_candidate(&self, state: &mut State, ch: char) -> KeyAction {
        // 字母引导符（z 进模式）算码：它是用户真实打出的击键，与 `guide_to_return` 同一判据。
        // 不算的话，z 进模式后空缓冲按标点会被判「非空码」照上屏 z，clear 档丢不掉它。
        let guide = Self::guide_to_return(&state.temp_pinyin_prefix, &state.committed_text);
        let code = format!("{guide}{}", state.temp_pinyin_buffer);
        let head = match self.punct_empty_code_policy_for(state, &code) {
            PunctEmptyCodePolicy::ClearNoInput => {
                self.exit_temp_pinyin(state);
                self.notify_ui_hide();
                return KeyAction::ClearComposition;
            }
            PunctEmptyCodePolicy::Clear => String::new(),
            PunctEmptyCodePolicy::Commit => {
                // committed 段已在各次选词记过，此处只记本次实际上屏的原码避免重复。
                let raw = format!("{}{}", guide, state.temp_pinyin_buffer);
                let raw_text = format!(
                    "{}{}{}",
                    guide, state.committed_text, state.temp_pinyin_buffer
                );
                if !raw.is_empty() {
                    self.record_commit(
                        &raw,
                        raw.len() as u32,
                        -1,
                        wind_store::stats::CommitSource::TempPinyin,
                    );
                    // 原码类上屏进上屏历史（转换前形态、不含标点，同主路）；原码不记词频。
                    self.push_commit_history(&raw_text);
                }
                self.maybe_convert(state, &raw_text)
            }
        };
        let punct = self.convert_punct_after(state, ch, 0, &head);
        self.record_commit(&punct, 0, -1, wind_store::stats::CommitSource::Punctuation);
        self.exit_temp_pinyin(state);
        self.notify_ui_hide();
        Self::commit_action(format!("{head}{punct}"), true)
    }

    /// 临英缓冲的光标位插入（字母 / 数字 / 空格 / 符号五个入口共用）。
    fn temp_english_insert(state: &mut State, ch: char) {
        preedit_cursor::BufEdit::new(
            &mut state.temp_english_buffer,
            &mut state.temp_english_cursor,
        )
        .insert(ch);
    }

    /// 退出临时英文模式并清空状态。
    pub(crate) fn exit_temp_english(&self, state: &mut State) {
        state.active = None;
        // 夺取回退登记随模式一起作废，理由同 `exit_special_mode`。
        state.rewind = None;
        state.temp_english_buffer.clear();
        state.temp_english_cursor = 0;
        state.temp_english_prefix.clear();
        // 档位属于这一次组合：退出临英即复位，否则上一个词按出来的全大写会串到下一个。
        state.english_case_variant = crate::english_candidates::CaseVariant::default();
        state.preedit.clear();
        state.candidates.clear();
        state.grid_top = None;
        state.grid_tab = wind_ui_types::GridTab::All;
        state.grid_full.clear();
    }

    /// 刷新临时英文候选：`原文 → 大小写变形 → 英文词库前缀匹配（保持词库原文）`。
    /// 需 `temp_english.show_candidates` 开启才产出变形与词库候选；词库为固定 id "english" 方案。
    ///
    /// 词库候选不按输入形态适配大小写：临英由 Shift+字母进入、首字母恒大写，适配会把整列
    /// 套成 `Hello`。大小写由紧随原文的显式变形候选承载，词库候选顺延其后。去重按精确文本：
    /// 变形候选之间恰是同一小写形态的不同大小写。
    pub(crate) fn update_temp_english_candidates(&self, state: &mut State) {
        self.refill_temp_english_candidates(state);
        self.grid_resume(state);
    }

    fn refill_temp_english_candidates(&self, state: &mut State) {
        state.candidates.clear();
        self.reset_candidate_view(state);
        let buf = state.temp_english_buffer.clone();
        state.preedit = format!("{}{}", state.temp_english_prefix, buf);
        if buf.is_empty() {
            return;
        }
        // 头部候选：原文 + 大小写变形，各有开关（`input.temp_english.*`）。与英文方案配置各自
        // 独立、实现共用（见 crate::english_candidates）：分歧只能在「要不要生成」。
        // 两个开关都关且词库无命中时候选为空，空格臂会正确落到上屏缓冲原文。
        //
        // 变形候选还要求当前真的在出候选：临英的数字键判据是「除原文外还有没有别的候选」，
        // 候选关闭时冒出变形会让 `Ver2b` 里的 `2` 被当成选词键。原文那条不受此限。
        let dict_schema = self.overlay_engine_schema(state);
        let (raw_mode, want_variants) = {
            let te = &self.rt().config.input.temp_english;
            (te.raw_candidate, te.case_variants && dict_schema.is_some())
        };
        // 词库根本没被查询时（`show_candidates = false`），`InDict` 无从回答，退回 `Always`，
        // 否则临英一条候选都不产。
        let raw_mode = if dict_schema.is_none() && raw_mode == RawCandidateMode::InDict {
            RawCandidateMode::Always
        } else {
            raw_mode
        };
        let mut cased = false;
        // 词库段先独立算完，头部候选最后才生成（与主输入路同序）：`InDict` 判据看的是词库段
        // 的最终形态（shadow 删掉的词、大小写投影改写的文本）。
        let mut dict_part: Vec<Candidate> = Vec::new();
        if let Some(schema) = dict_schema {
            let code = buf.to_lowercase();
            // 取数上限按词库方案自己的引擎类型分级，与主输入路同一张表
            // （`initial_candidate_limit_of`），否则排在小上限之外的词等不到词频提上来。
            // 用 `loaded_engine_type`（逐键路径不读文件），故先 `ensure_schema`：英文方案通常
            // 不受预热覆盖，否则启动后首次临英拿到 `None`。已加载时它只是一次 map 查找。
            // 不可照抄 `temp_pinyin_limit`：两处 `_` 兜底一小一大，后果相反。
            self.engine_mgr.ensure_schema(&schema);
            let limit = Self::initial_candidate_limit_of(
                self.engine_mgr.loaded_engine_type(&schema),
                &code,
            );
            let result = self.engine_mgr.convert_with(&schema, &code, limit);
            // 去重按精确文本；入列时补 `natural_order`（= 入列序）。取整条 `Candidate`：
            // `source` / `code` 是词频记账与重排的依据。
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for mut c in result.candidates {
                if !seen.insert(c.text.clone()) {
                    continue;
                }
                c.natural_order = dict_part.len() as i32;
                dict_part.push(c);
            }
            // 词频重排 + 候选调整（置顶/隐藏），归属与写端同源，见 `effective_data_schema`。
            // 只作用于词库段：首候选恒是用户所打原文，是临英的硬承诺。码取小写化缓冲，与英文
            // 方案的 `input_buffer` 同一个键，否则两个入口学不到一块去。
            // 重排前先把用过、但原序在取数上限之外的词补进池尾（调频关时空操作）。
            let recalled = self.english_freq_recall(Some(ENGLISH_SCHEMA), &dict_part, &code);
            dict_part.extend(recalled);
            self.run_passes(
                state,
                &mut dict_part,
                crate::postprocess::profile::TEMP_ENGLISH,
                &crate::postprocess::PassCtx {
                    owner: Some(ENGLISH_SCHEMA),
                    freq_code: Some(&code),
                    shadow_code: Some(&code),
                },
            );
            // 大小写投影排在重排与置顶之后：那两者以 `text` 为键，先改写会让读写两端键不同而
            // 静默失效；原文留在 `case_source`，记账走 `freq_text()`。只投影词库段，否则头部
            // 的大小写变形会塌成一条。
            if self.rt().config.input.temp_english.case_follow_input {
                cased = crate::english_candidates::apply_english_case(
                    &mut dict_part,
                    &buf,
                    crate::english_candidates::CaseVariant::Default,
                );
            }
        }
        // 头部候选：判据依赖上面那段的最终形态。精确去重（不是小写去重）吃掉词库里字面相同
        // 的那条，与 `InDict` 判据同一口径，两处必须同进同退。词组分词生效时 `Always` 降级成
        // `InDict`（见 `raw_mode_under_phrase_seg`）。判据取临英自己那份 `phrase_seg`，且不能只嗅
        // `'`：`symbol_chars` 含 `'` 时 `don't` 是正常字面输入。
        let phrase_active = self.rt().config.input.temp_english.phrase_seg
            && buf.contains(wind_engine::english_phrase::PHRASE_SEPARATOR);
        let raw_mode =
            crate::english_candidates::raw_mode_under_phrase_seg(raw_mode, phrase_active);
        let blocked = self.blocked_words_in(ENGLISH_SCHEMA);
        let want_raw = crate::english_candidates::wants_raw_candidate(
            raw_mode,
            &buf,
            &dict_part,
            blocked.as_deref(),
            self.rt().config.input.temp_english.case_follow_input,
        );
        // 变形跟着原文走（同主输入路）。临英出厂 `case_variants = true`，不管的话
        // `ip'pro` 会先出 `Ip'pro` / `ip'pro` / `IP'PRO` 三条才轮到词组。
        let want_variants = want_variants && (!phrase_active || want_raw);
        let head =
            crate::english_candidates::english_head_candidates(&buf, want_raw, want_variants);
        // 与词库同名的头部格由词库词占据（保留来源 / 编码 / 释义），见 `merge_head_with_dict`。
        // 头部占 `0..h`，其余词库段整体后移 h 位。词库段的号可能有空洞，`natural_order` 只作
        // 相对比较键，别依赖它的绝对值。
        //
        // 已知现象：shadow 先于并头部，「置顶到第 2 位」在原文命中时会落到第 1 位。这与主输入路
        // 一致，不是缺陷。
        let head_len = head.len();
        let mut cands = crate::english_candidates::merge_head_with_dict(head, dict_part);
        for (i, c) in cands.iter_mut().enumerate() {
            if i < head_len {
                c.natural_order = i as i32;
            } else {
                c.natural_order += head_len as i32;
            }
        }
        // 档位（CapsLock 循环）作用于整列，含头部候选：用户按出「全大写」时，列表里
        // 不该还留着小写的变形候选。档位非默认时不再跑投影——用户已显式指定形态。
        if state.english_case_variant != crate::english_candidates::CaseVariant::Default {
            cased |= crate::english_candidates::apply_english_case(
                &mut cands,
                &buf,
                state.english_case_variant,
            );
        }
        // 去重必须在改写之后再跑一次：投影把词库的 `hi` 变成 `Hi`，与头部原文候选撞车；
        // 全大写档更会把三条变形塌成同一条。上面词库段内部那次 `seen` 去重看的是改写前的文本。
        if cased {
            crate::english_candidates::dedup_by_text(&mut cands);
        }
        // 统一展开汇聚点：临时英文词库候选内 `$` 特殊语法在此展开（见 finalize_candidates）。
        state.candidates = self.finalize_candidates(cands, &buf);
    }

    /// 临英文本上屏（可选全角）+ 退出模式。临英所有上屏出口的单一真相源。
    ///
    /// `append_space` 取 `input.temp_english.commit_space`（与 `schema.english.commit_space` 各一份），
    /// 按出口给而非在这里统一判：选词类出口补，回车与标点顶屏不补（后者补了会得到 `hello ,`）。
    ///
    /// 经本函数的选词出口：空格 / 数字键 / 次三选键 / 鼠标点选 / 移动端候选栏
    /// （后两者经 `select_candidate_at` 的临英分支 → `commit_temp_english_selected`）。
    pub(crate) fn commit_temp_english_text(
        &self,
        state: &mut State,
        t: String,
        append_space: bool,
    ) -> KeyAction {
        let text = if state.full_width {
            to_full_width(&t)
        } else {
            t
        };
        // 临时英文上屏（独占模式，无分段 committed）：来源临英，英文无编码故 code_len=0。
        self.record_commit(&text, 0, -1, wind_store::stats::CommitSource::TempEnglish);
        // 补空格排在记账之后，否则统计表里是一条对不上的脏键（词频更早记在
        // `record_temp_english_selection`）。全角态下补全角空格。
        let text = if append_space && !text.is_empty() {
            let sp = if state.full_width {
                to_full_width(" ")
            } else {
                " ".to_string()
            };
            format!("{text}{sp}")
        } else {
            text
        };
        self.exit_temp_english(state);
        self.notify_ui_hide();
        if text.is_empty() {
            KeyAction::ClearComposition
        } else {
            Self::commit_action(text, true)
        }
    }

    /// 临英选中某条候选的上屏出口：命令守卫 → 词频记账 → 补空格 → 上屏退出。
    ///
    /// 键盘侧选词出口（空格 / 回车(`space_as_input`) / 数字键 / 次三选键）与鼠标点选、移动端
    /// 候选栏一律走这里；只拿文本直接上屏会丢掉候选身份，词频就记不上。
    pub(crate) fn commit_temp_english_selected(&self, state: &mut State, gi: usize) -> KeyAction {
        if let Some(act) = self.temp_english_try_command(state, gi) {
            return act;
        }
        let cand = state.candidates[gi].clone();
        self.record_temp_english_selection(state, &cand);
        let append = self.english_space_enabled_in(state);
        self.commit_temp_english_text(state, cand.text, append)
    }

    /// 临英选词的词频记账，与英文方案同一个桶（`ENGLISH_SCHEMA`）。
    ///
    /// 只记词库来的候选：原文与大小写变形没有码，记进去是读端永远查不中的孤儿键。头部候选
    /// 按它对应的词库词记（词库那条已被它按字面去重吃掉），见 `english_head_dict_word`。
    /// 取码前先小写化缓冲，与英文方案同一个键。
    ///
    /// 上屏历史与词频分开，无条件记实际上屏文本：它是 `;` 重复上屏的数据源，要的是用户看见
    /// 的形态（`Translation` 而非 `translation`），不看调频开关与词库来源。
    fn record_temp_english_selection(&self, state: &State, cand: &Candidate) {
        let head_word;
        let freq_cand = if cand.source == CandidateSource::English {
            Some(cand)
        } else {
            // 查词库用临英当下的词库方案；候选显示关着（`None`）时临英本就不查词库，
            // 这里也不为记一次词频去加载它。
            head_word = self.overlay_engine_schema(state).and_then(|dict_schema| {
                self.english_head_dict_word(&dict_schema, &state.temp_english_buffer, cand)
            });
            head_word.as_ref()
        };
        let Some(freq_cand) = freq_cand else {
            self.push_commit_history(&cand.text);
            // 纯原文（不是词库词）：英文自动造词的信号（开关与判据都在 `learn_english_raw` 里）。
            self.learn_temp_english_raw(state, &cand.text);
            return;
        };
        let code = state.temp_english_buffer.to_lowercase();
        // 选中英文临时词（自动造的词）：推进晋升计数，与英文方案主路同一函数。
        // 码取候选自己的全码（`cand_code`），不是缓冲：临时词记在全码下，英文引擎不填
        // `store_code`，拿前缀缓冲去点查永远查不到。
        if freq_cand.meta.is_temp_dict {
            let full = Self::cand_code(&code, freq_cand);
            self.bump_selected_temp_word(ENGLISH_SCHEMA, freq_cand, &full, None);
        }
        // 词频记 `freq_text()` 而非 `text`：读端排在大小写投影之前、看到的是词库原文。
        // 历史记实际上屏的 `cand.text`（先记历史、再看开关，关着也记）。
        self.record_selection_cased_in(
            Some(ENGLISH_SCHEMA),
            &self.freq_code(&code, freq_cand),
            freq_cand.freq_text(),
            &cand.text,
            freq_cand.source,
        );
    }

    /// 临英选中候选（全局下标 `gi`）的命令前置守卫：`$CC` 命令候选 → 执行动作（退出临英后异步跑），
    /// 返回 `Some(action)`；非命令 → `None`，调用方按各自文本上屏语义继续。
    pub(crate) fn temp_english_try_command(
        &self,
        state: &mut State,
        gi: usize,
    ) -> Option<KeyAction> {
        let cand = state.candidates[gi].clone();
        let code = state.temp_english_buffer.clone();
        self.overlay_commit_command(state, &cand, &code, |s, st| s.exit_temp_english(st))
    }

    /// 临英下字符 `ch` 是否被放行「直接入缓冲」——`allow_symbols` 总开关 + `symbol_chars`
    /// 白名单的单一真相源。
    ///
    /// 四个消费点语义各不相同却必须同批读它，漏一个就是「某个字符设了不生效」：
    /// 1. 标点臂（`_ =>`）：放行 → 入缓冲，否则「上屏高亮候选 + 转换后标点 → 退出」。
    /// 2. 数字臂（`VK_1..=VK_9`）：放行 → 入缓冲，否则按页选词。
    /// 3. 选词键 `;`/`'`：放行 → 让位，落标点臂入缓冲；否则选第 2/3 候选。
    /// 4. 导航门控（`handle_candidate_nav`）：放行 → 让位；否则 `-=[],.` 仍翻页。
    ///
    /// 按字符而非按键判：想打 `C++` 不必牺牲全部选词键与翻页键；同一个键的两个 shift 态
    /// （`@` 与 `2`）也各自管辖。
    pub(crate) fn temp_english_char_allowed(&self, ch: char) -> bool {
        let te = &self.rt().config.input.temp_english;
        te.allow_symbols && te.symbol_chars.contains(ch)
    }

    /// 临英缓冲是否已进入「纯文本累积态」——含任何非字母字符（`C++` / `x64` / `e-mail`）。
    ///
    /// 此态下数字键无条件入缓冲，不再当选词键。判据落在缓冲而不是候选数上：首候选恒是原文、
    /// 含符号串照样产出大小写变形，按候选数判会让 `C++11` 的 `1` 直接上屏 `C++`；而缓冲一旦
    /// 含符号，词库必然查不到，选词已无价值。白名单不含数字时也靠它打出 `C++11`。
    pub(crate) fn temp_english_buffer_is_literal(state: &State) -> bool {
        state
            .temp_english_buffer
            .chars()
            .any(|c| !c.is_ascii_alphabetic())
    }

    /// 临时英文模式按键处理：缓冲累积，空格/回车/数字/标点等各出口上屏。
    pub(crate) fn handle_temp_english_key(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> KeyAction {
        // 候选感知刷新后返回组合区动作。
        let refresh = |this: &Self, state: &mut State| -> KeyAction {
            this.update_temp_english_candidates(state);
            let d = state.preedit.clone();
            let caret_pos = this.overlay_caret(state);
            this.notify_ui_update(state);
            KeyAction::UpdateComposition { text: d, caret_pos }
        };
        // 原文类上屏（不经候选：回车、以及 `show_candidates` 关闭时的兜底）。选中候选
        // 一律走 `commit_temp_english_selected`——那条路要记词频，而这里没有候选可记。
        // 补空格按出口给：回车不补（终结性动作），空格兜底补（对应英文方案「空格上屏原码」）。
        let commit_text = |this: &Self, state: &mut State, t: String, sp: bool| -> KeyAction {
            this.commit_temp_english_text(state, t, sp)
        };
        // Ctrl/Alt 组合守卫（见 `overlay_ctrl_alt_guard`）：必须最先，否则组合键会落到下方
        // 各臂当普通输入。临英无分段 committed，待输入内容只看自己的缓冲。
        if let Some(act) = self.overlay_ctrl_alt_guard(
            state,
            data,
            !state.temp_english_buffer.is_empty(),
            |s, st| s.exit_temp_english(st),
        ) {
            return act;
        }
        if let Some(act) = self.handle_candidate_nav(state, data) {
            return act;
        }
        // 编码区光标移动（左右 / Home / End）；置于候选导航之后，导航键优先。
        if let Some(act) = self.overlay_cursor_key(state, data) {
            return act;
        }
        match data.key_code {
            // Esc：放弃退出，实现收口在 `cancel_session`。
            keymap::VK_ESCAPE => self.cancel_session(state),
            keymap::VK_BACK | keymap::VK_DELETE => {
                // 退格删光标前 / Delete 删光标后；缓冲被删空则退出（本就空缓冲时只有退格退出）。
                let backward = data.key_code == keymap::VK_BACK;
                let removed = {
                    let mut ed = preedit_cursor::BufEdit::new(
                        &mut state.temp_english_buffer,
                        &mut state.temp_english_cursor,
                    );
                    if backward {
                        ed.backspace()
                    } else {
                        ed.delete()
                    }
                };
                if state.temp_english_buffer.is_empty() && (removed || backward) {
                    self.exit_temp_english(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                } else if removed {
                    refresh(self, state)
                } else {
                    KeyAction::Consumed
                }
            }
            keymap::VK_SPACE => {
                // space_as_input：空格作为输入字符入缓冲，仅回车上屏（对齐 Go）。
                // 上屏职责随之转给回车，且回车此时取高亮候选而非原文（见下方 VK_RETURN）。
                if self.rt().config.input.temp_english.space_as_input {
                    Self::temp_english_insert(state, ' ');
                    refresh(self, state)
                } else if let Some(act) = self.commit_highlighted(state) {
                    // 空格：上屏当前高亮候选（首候选=原始输入）；命令候选执行动作。
                    // 与 `commit_highlighted` 动词同一出口——那个动词不跟 `space_as_input`
                    // 这个变体，恒为上屏高亮，故开关判在本臂而不在出口里。
                    act
                } else {
                    // 无候选（候选显示关闭 / 原文与变形都不产且词库无命中）：上屏缓冲原文。
                    // 对应英文方案「空格上屏原码」，故同样补空格。
                    let text = state.temp_english_buffer.clone();
                    // 原码类上屏也进上屏历史（转换前形态、不含补的空格，同回车）。
                    self.push_commit_history(&text);
                    self.learn_temp_english_raw(state, &text);
                    let sp = self.english_space_enabled_in(state);
                    commit_text(self, state, text, sp)
                }
            }
            keymap::VK_RETURN => {
                // clear 模式在临英只管空缓冲：缓冲装的是英文原文而非编码，且 `space_as_input`
                // 下上屏职责全压在回车上，clear 再管非空缓冲就一个上屏通路都不剩。
                if self.enter_clears_composition() && state.temp_english_buffer.is_empty() {
                    self.exit_temp_english(state);
                    self.notify_ui_hide();
                    return KeyAction::ClearComposition;
                }
                // space_as_input：空格已被占作输入字符，回车接过「上屏高亮候选」的职责，否则
                // 该配置下可能一个选词键都不剩。未导航时高亮就是首候选（= 原文），与「回车上屏
                // 原文」的直觉一致。
                if self.rt().config.input.temp_english.space_as_input
                    && !state.candidates.is_empty()
                {
                    let idx = self
                        .highlighted_global_index(state)
                        .min(state.candidates.len() - 1);
                    // 此配置下回车接过的是选词职责，故走选中出口（记词频、补空格），
                    // 与空格键在默认配置下的行为对齐——同一个动作换了个键，不该换语义。
                    return self.commit_temp_english_selected(state, idx);
                }
                // 回车：上屏原始输入文本（不取候选）；缓冲空时上屏触发键字符（触发键透传）。
                // 不补空格：回车是终结性动作，与英文方案 `VK_RETURN` 空码分支同口径。
                let text = if state.temp_english_buffer.is_empty() {
                    state.temp_english_prefix.clone()
                } else {
                    // 原码类上屏也进上屏历史，记转换前形态（半角，与临英选词出口一致；
                    // 全角由上屏时再转）。空缓冲上屏的是触发键字符，不记。
                    let raw = state.temp_english_buffer.clone();
                    self.push_commit_history(&raw);
                    self.learn_temp_english_raw(state, &raw);
                    raw
                };
                commit_text(self, state, text, false)
            }
            keymap::VK_A..=keymap::VK_Z => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                let base = data.key_code - 0x41;
                let ch = if shift {
                    (b'A' + base as u8) as char
                } else {
                    (b'a' + base as u8) as char
                };
                Self::temp_english_insert(state, ch);
                refresh(self, state)
            }
            keymap::VK_1..=keymap::VK_9 if data.modifiers & MOD_SHIFT == 0 => {
                let ch = (b'0' + (data.key_code - 0x30) as u8) as char;
                // 数字入缓冲有两条独立通路，缺一不可：
                // 1. 该数字被显式列入白名单（`symbol_chars` 出厂含 `0-9`）——数字是合法英文
                //    内容（hello2 / mp3 / x64），此时选词改走「方向/翻页键导航 + 空格上屏」。
                // 2. 缓冲已是纯文本态（含非字母字符）——见 `temp_english_buffer_is_literal`。
                //    白名单不含数字时靠这条兜底，否则 `C++11` 打不出来。
                let digits_as_input = self.temp_english_char_allowed(ch)
                    || Self::temp_english_buffer_is_literal(state);
                // 数字：有词库候选（>1，即除原文外还有匹配）时按页选词；否则作输入（英文含数字 v2）
                let (start, end) = self.select_range(state);
                let gi = start + (data.key_code - 0x31) as usize;
                if !digits_as_input && state.candidates.len() > 1 && gi < end {
                    self.commit_temp_english_selected(state, gi)
                } else {
                    Self::temp_english_insert(state, ch);
                    refresh(self, state)
                }
            }
            0x30 if data.modifiers & MOD_SHIFT == 0 => {
                // `0` 不参与选词（选词键是 1-9），故不受白名单管辖，否则从列表删掉 0 后
                // 0 键会静默无反应。
                Self::temp_english_insert(state, '0');
                refresh(self, state)
            }
            k if let Some(ch) = numpad_char(k) => {
                // 小键盘 direct 语义（follow_main 时键已在入口归一化成主键盘键，不到达这里）：
                // 临英缓冲是文本不是编码，数字/运算符都是合法内容 → 直接入缓冲。
                Self::temp_english_insert(state, ch);
                refresh(self, state)
            }
            _ => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                // Unicode 模式转交（大写 `U+` 入口）：大写 U 起手时 Shift+字母已被截进临英，
                // `try_prefix_hijack` 看不见那个 U，这里把临英缓冲接上同一张前缀表。必须早于
                // 下方的选词键与白名单判定（`allow_symbols` 出厂关，否则屏幕上出现 `U＋`）。
                // 判据是整串比较而不是写死字母，前缀由用户配置。
                if self.rt().config.input.unicode.enabled
                    && let Some(ch) = printable_char(data.key_code, shift)
                {
                    let probe = format!("{}{}", state.temp_english_buffer, ch);
                    if self.is_unicode_prefix(&probe) {
                        return self.enter_unicode_mode(state, probe, RewindOrigin::TempEnglish);
                    }
                }
                // 邮箱模式转交：五笔用户名常超过四码、早已顶字上屏，主路的 `@` 触发只对短用户名
                // 可用；临英缓冲攒的是整段英文，在这里认 `@` 才能带完整用户名进邮箱模式。
                // 同理必须早于选词键 / 白名单判定。判据与主路闸门一致：本键是 `@` 且缓冲非空。
                if self.rt().config.input.email.enabled
                    && !state.temp_english_buffer.is_empty()
                    && printable_char(data.key_code, shift) == Some(crate::handle_email::EMAIL_AT)
                {
                    let buffer = format!(
                        "{}{}",
                        state.temp_english_buffer,
                        crate::handle_email::EMAIL_AT
                    );
                    return self.enter_email_mode(state, buffer, RewindOrigin::TempEnglish);
                }
                // 词组分词符：必须排在下方选词键判定之前，位置即夺取——`'` 是出厂第三候选键，
                // 同一处判断同时决定「收进缓冲」与「不再作选词键」。缓冲非空是前提。
                if !shift
                    && !state.temp_english_buffer.is_empty()
                    && self.temp_english_phrase_separator_key(data.key_code)
                {
                    Self::temp_english_insert(state, wind_engine::english_phrase::PHRASE_SEPARATOR);
                    return refresh(self, state);
                }
                // 二三候选键（默认 `;` `'`）→ 选候选。与数字臂同构地受白名单抑制：只有该字符
                // 自己列入白名单时才让位入缓冲。越界不在此处理，落下方标点臂。
                if !shift
                    && punct_char(data.key_code, shift)
                        .is_none_or(|ch| !self.temp_english_char_allowed(ch))
                    && let Some(offset) = self.select_key_offset(data.key_code)
                {
                    let (start, end) = self.select_range(state);
                    let gi = start + offset;
                    if gi < end {
                        return self.commit_temp_english_selected(state, gi);
                    }
                }
                // 其它（标点等）：上屏当前高亮候选 + 转换后标点，退出
                if let Some(ch) = punct_char(data.key_code, shift) {
                    // 白名单内的可见符号直接入缓冲累积（如 C++），不上屏退出（对齐 Go）。
                    // 列表外的照旧走下方「上屏高亮候选 + 转换后标点 → 退出」——这条通路是
                    // 「打完英文顺手加句号上屏」的唯一实现，不能因为开了总开关就整体消失。
                    if self.temp_english_char_allowed(ch) {
                        Self::temp_english_insert(state, ch);
                        return refresh(self, state);
                    }
                    let base = if !state.candidates.is_empty() {
                        let idx = self
                            .highlighted_global_index(state)
                            .min(state.candidates.len() - 1);
                        if let Some(act) = self.temp_english_try_command(state, idx) {
                            return act;
                        }
                        let cand = state.candidates[idx].clone();
                        // 顶屏也是一次选中：记词频但不补空格（会得到 `hello ,`），与主输入路
                        // `commit_highlight_then_char` 同口径。本臂不走 `commit_temp_english_text`。
                        self.record_temp_english_selection(state, &cand);
                        cand.text
                    } else {
                        // 无候选：与英文方案主路空码标点同一判据（`punct_on_empty_behavior`）。
                        match self.punct_empty_code_policy_for(state, &state.temp_english_buffer) {
                            PunctEmptyCodePolicy::ClearNoInput => {
                                self.exit_temp_english(state);
                                self.notify_ui_hide();
                                return KeyAction::ClearComposition;
                            }
                            PunctEmptyCodePolicy::Clear => String::new(),
                            PunctEmptyCodePolicy::Commit => {
                                // 顶掉原文：原码类上屏也进上屏历史（转换前形态、不含标点）。
                                self.push_commit_history(&state.temp_english_buffer);
                                self.learn_temp_english_raw(state, &state.temp_english_buffer);
                                state.temp_english_buffer.clone()
                            }
                        }
                    };
                    let base = if state.full_width {
                        to_full_width(&base)
                    } else {
                        base
                    };
                    // 标点紧跟在临英原文后面：「英文后」看它（`Node.` 而不是 `Node。`）。
                    let punct = self.convert_punct_after(state, ch, 0, &base);
                    if !base.is_empty() {
                        self.record_commit(
                            &base,
                            0,
                            -1,
                            wind_store::stats::CommitSource::TempEnglish,
                        );
                    }
                    self.record_commit(&punct, 0, -1, wind_store::stats::CommitSource::Punctuation);
                    self.exit_temp_english(state);
                    self.notify_ui_hide();
                    Self::commit_action(format!("{}{}", base, punct), true)
                } else {
                    KeyAction::Consumed
                }
            }
        }
    }

    /// 顶屏当前高亮候选（若有）并进入临时拼音模式（对齐 Go decideBufferedTrigger 的 actEnterMode）。
    /// 有候选：上屏高亮候选 + 原子开启临时拼音组合；空码：丢弃缓冲后进入。
    pub(crate) fn commit_and_enter_temp_pinyin(
        &self,
        state: &mut State,
        key_code: u32,
        target: String,
    ) -> KeyAction {
        // 命令候选顶屏 → 执行命令（与按空格一致），不进模式、不上屏 display 标签。
        if let Some(act) = self.top_commit_command_guard(state) {
            return act;
        }
        // 已转换前缀 + 高亮候选一并上屏（含记账与简繁转换）。
        let committed = self.take_committed_with_highlight(state);
        state.input_buffer.clear();
        state.candidates.clear();
        // 进入临时拼音
        state.active = Some(ModeKind::TempPinyin);
        state.temp_pinyin_schema = target;
        state.temp_pinyin_buffer.clear();
        // key_code == 0 是直达热键哨兵：不写引导符（temp_pinyin_prefix_for 对未映射键会兜底
        // 反引号，故此处显式取空，对齐 enter_special_mode 的 key_code=0 语义）。
        state.temp_pinyin_prefix = if key_code == 0 {
            String::new()
        } else {
            Self::temp_pinyin_prefix_for(key_code).to_string()
        };
        self.update_temp_pinyin_candidates(state);
        self.notify_ui_update(state);
        let prefix = state.temp_pinyin_prefix.clone();
        match committed {
            // 顶字进入：不看 `[input.caret]` 开关，prefix 为空时这条压根不建 composition。
            // 直达热键多半是打了一半再按、走这一支；开关只管下面那支。改判据时与
            // `temp_pinyin_via_composition` 的文档一起改。
            Some(text) => self.commit_then_new_composition(text, prefix),
            // 直达热键进入时 prefix 为空（`key_code == 0` 不写引导符），此时是否建占位
            // composition 取坐标由 `[input.caret]` 决定；引导键进入时 prefix 非空，开关不参与。
            None => self.temp_pinyin_entry_composition(key_code, prefix),
        }
    }
}
