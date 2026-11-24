//! 生命周期：配置重载、服务重启、独占模式进入/复位。
//!
//! IME 激活/失活、焦点变更、composition 终止是 MessageHandler trait 方法，在
//! coordinator.rs 的 `impl MessageHandler` 块。

use crate::coordinator::{Coordinator, State, punct_char};
use crate::pipeline::ModeKind;
use tracing::{debug, info, warn};
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_config::BoundAction;
use wind_ipc::protocol::{MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;
use wind_ui_types::UiCommand;

/// 方案级按键功能表对某个键的裁决，见 [`Coordinator::bound_key_decision`]。
pub(crate) enum BoundKeyDecision {
    /// 方案表未对该键表态 —— 照常走全局引导键链（未配置者行为不变）。
    NotBound,
    /// 表了态但该键让位（显式 `none` / 活码前缀 / z 的 repeat 身份）：落普通输入，
    /// 且不再落全局引导键链。
    Yield,
    /// 执行这个动作。
    Act(BoundAction),
}

impl Coordinator {
    /// 重启服务进程：隐藏 UI 后向 main 发重启信号（main 释放单例并重拉自身）。
    pub(crate) fn restart_service(&self) {
        info!("Restart service requested from menu");
        // 有活跃 composition 时先清空内部状态并通知 TSF 清除，避免服务退出后留下孤儿组合。
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // `candidates` 这一项不能少：联想态下其余四项全空，宿主里挂的是 `assoc_composition`
        // 占位，漏了就不推 `ClearComposition`，占位空格留在文档里；服务重启后
        // `assoc_placeholder_orphaned` 也没了，`adopt_orphaned_placeholder` 兜不住。
        let has_composition = !state.input_buffer.is_empty()
            || !state.preedit.is_empty()
            || !state.committed_text.is_empty()
            || !state.candidates.is_empty()
            || state.active.is_some();
        if has_composition {
            self.reset_exclusive_modes(&mut state);
        }
        drop(state);
        if has_composition {
            let encoded = wind_ipc::codec::encode_clear_composition();
            self.push_server.push_to_active(&encoded);
        }
        self.notify_ui_hide();
        let _ = self.ui_tx.send(UiCommand::HideToolbar);
        crate::request_restart();
    }

    /// 重载配置（best-effort：重新下发当前主题）。
    pub(crate) fn reload_config(&self) {
        let name = self
            .theme_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let dark = self.resolve_theme_dark();
        self.push_theme(&name, dark);
        // 语言栏图标的呈现参数（[ui.langbar]）也是运行时镜像，不回灌就要重启才生效。
        // 内部自带「无变化则不重发」。
        #[cfg(all(feature = "desktop-ui", windows))]
        self.apply_langbar_config();
        // 速度修正系数同理：不回灌会出现当日速度按新系数、历史最快按旧系数的两套口径。
        if let Some(c) = self.stat_collector.as_ref() {
            c.set_speed_factor(self.rt().config.stats.speed_factor);
        }
        // 不弹「已重载」气泡：热重载统一由 reload_user_config 的 toast 通知。
    }

    /// 该键是否被任一模式配作进入键（临拼/临英的符号触发键、特殊模式引导键、mix 触发键、
    /// 反查模式触发键）。仅用于「智能符号 press2 要不要抢在模式激活之前」的门控。
    ///
    /// z 键功能（`z_key_action`）刻意不算：字母键不产出标点，`punct_char` 那一关就挡住了。
    pub(crate) fn is_any_mode_trigger(&self, key_code: u32) -> bool {
        self.is_temp_pinyin_trigger(key_code)
            || self.is_temp_english_trigger(key_code)
            || self.match_special_trigger(key_code).is_some()
            || self.match_mix_trigger(key_code).is_some()
            || self.is_reverse_trigger(key_code)
    }

    /// 该键在空缓冲时是否已被方案声明为首码，是则符号类模式引导键让位给码表。
    ///
    /// 冲突场景：`;` 既是 `quick_mix` 引导键又在某方案的 `input_chars` 里，模式激活排在码元
    /// 闸门之前，引导键恒赢、`;abc` 打不出。仲裁交给首码集（`leading_chars`，未配置时等于
    /// `input_chars`）：想两者共存，把该符号排除出 `leading_chars` 即可。
    ///
    /// 只管非字母：字母默认全在首码集里，一并让位会废掉 Shift+字母进临英与 `z_key_action`
    /// 的身份裁决（后者自带 `has_code_prefix("z")` 判据）。符号通配键的首位仲裁见
    /// [`Self::wildcard_takes_lead_key`]。
    ///
    /// 只对全局层绑定生效（`bound_key_decision` 按来源分流）：这是跨层仲裁；方案级
    /// `[key_actions]` 与同方案 `leading_chars` 是同层冲突，显式绑定优先，见
    /// docs/design/schema-key-actions.md §4.3。
    pub(crate) fn code_char_takes_lead(&self, key_code: u32) -> bool {
        let Some(ch) = punct_char(key_code, false) else {
            return false;
        };
        if ch.is_ascii_alphabetic() {
            return false;
        }
        self.engine_mgr
            .active_is_leading_char(ch.to_ascii_lowercase())
    }

    /// 空缓冲下这个键是否归符号通配键：是则全局层绑定（出厂反引号 = 临拼触发）让位，
    /// 键落普通输入、由 `try_code_char_gate` 收作通配。方案级显式绑定仍优先
    /// （判据内含，见 `wildcard_symbol_takes_lead`）。
    ///
    /// 只能在空缓冲的调用点问，不能并进 [`Self::code_char_takes_lead`]：后者也被组码中
    /// 「顶字 + 进模式」调用，而满码后通配键按字面不进缓冲，那里再让位键就被白吞。
    pub(crate) fn wildcard_takes_lead_key(&self, key_code: u32) -> bool {
        punct_char(key_code, false).is_some_and(|ch| self.wildcard_symbol_takes_lead(ch))
    }

    /// 空缓冲模式激活的单一入口。命中返回激活 KeyAction，都不命中返回 None（落普通输入）。
    /// URL 前缀夺取是「缓冲扩展夺取」语义，不在此链，单独处理。
    ///
    /// 只有两段：临时英文 Shift+字母（不由 `key_actions` 表达，独立分支），以及
    /// `bound_key_decision`（所有具名键的绑定，方案级 → 全局单键 → `z_key_action`，
    /// 一个键只有一个动作，冲突在配置合并期就定了）。
    ///
    /// 引导键统一不要求候选为空：「空缓冲但有候选」（空码补全等）时按引导键也会进模式。
    pub(crate) fn try_activate_mode(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        // 智能符号 press2 优先于模式激活：模式内二次按进入键时已上屏中文标点并武装
        // （`arm_smart_symbol_after_commit`），时限内再按同键必须替换成英文形而不是又进一次
        // 模式，否则 `;` / `` ` `` / `\` 永远打不出英文形。收窄到：空闲态、被某模式占用的
        // 键、只判 press2 不武装。
        if state.input_buffer.is_empty()
            && state.committed_text.is_empty()
            && state.candidates.is_empty()
            && data.modifiers & MOD_SHORTCUT == 0
            && self.is_any_mode_trigger(data.key_code)
            && let Some(ch) = punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
            && let Some(act) = self.try_smart_symbol_press2_only(state, ch, data.prev_char)
        {
            return Some(act);
        }

        // 临时英文：Shift+字母（空缓冲 + 无候选 + 已启用）。
        //
        // 联想态例外（`assoc_active()`）：缓冲空但摆着联想候选，Shift+字母不在任何选词键的
        // 值域里，不放行就会落进中文码表缓冲。判据不是「候选非空」：空码补全的候选用户真在
        // 看，行为不动。
        //
        // 英文方案下不进临英（也不走 `direct_commit`）：Shift+字母就是「打一个大写字母」，
        // 落主路字母臂，大写记在影子串 `input_buffer_cased`。判据用引擎类型而非方案 id。
        if state.input_buffer.is_empty()
            && (state.candidates.is_empty() || state.assoc_active())
            && self.rt().config.input.temp_english.enabled
            && !self.engine_mgr.active_is_english()
            && data.modifiers & MOD_SHIFT != 0
            && data.modifiers & MOD_SHORTCUT == 0
            && (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code)
        {
            let ch = (b'A' + (data.key_code - 0x41) as u8) as char; // 首字母大写
            // 先收掉联想再进模式：两者共用 `state.candidates`；`exit_assoc` 同时作废未触发的
            // 自动隐藏计时，免得它在打英文时到期把占位组合标成孤儿。
            let was_assoc = self.exit_assoc(state, crate::handle_assoc::AssocExit::ModeActivate);
            // shift_behavior == "direct_commit"：不进临时英文，直接上屏大写字母（对齐 Go）。
            if self.rt().config.input.temp_english.shift_behavior == "direct_commit" {
                let out = if state.full_width {
                    wind_transform::fullwidth::to_full_width(&ch.to_string())
                } else {
                    ch.to_string()
                };
                // 联想窗要自己收：本分支不经 `notify_ui_update`，上屏动作不会关候选窗。
                // 按 `was_assoc` 收窄，免得每个 Shift+字母都多发一条 UI 消息。
                if was_assoc {
                    self.notify_ui_hide();
                }
                return Some(Self::commit_action(out, true));
            }
            state.active = Some(ModeKind::TempEnglish);
            state.temp_english_buffer = ch.to_string();
            // Shift+字母进入时缓冲已含首字母：光标必须落到其后，否则续打会插到首字母之前。
            state.temp_english_cursor = state.temp_english_buffer.len();
            state.temp_english_prefix = String::new();
            self.update_temp_english_candidates(state);
            let disp = state.preedit.clone();
            self.notify_ui_update(state);
            debug!("Entered temp English mode (buffer={})", disp);
            return Some(KeyAction::UpdateComposition {
                text: disp.clone(),
                caret_pos: disp.chars().count() as u32,
            });
        }

        // 已有编码时 Shift+字母（`temp_english.shift_when_composing = "commit_enter"`）：
        // 上屏高亮候选（含已转换前缀），再以该大写字母进临时英文，对齐临拼「顶字进模式」。
        // 出厂 `buffer` 不进本分支。门卫与上面那支同源，区别只在缓冲非空；联想态由上面处理。
        if (!state.input_buffer.is_empty() || !state.committed_text.is_empty())
            && !state.assoc_active()
            && self.rt().config.input.temp_english.enabled
            && self.rt().config.input.temp_english.shift_when_composing == "commit_enter"
            && !self.engine_mgr.active_is_english()
            && data.modifiers & MOD_SHIFT != 0
            && data.modifiers & MOD_SHORTCUT == 0
            && (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code)
        {
            let ch = (b'A' + (data.key_code - 0x41) as u8) as char;
            // 命令候选顶屏 → 执行命令（与按空格一致），不进模式。
            if let Some(act) = self.top_commit_command_guard(state) {
                return Some(act);
            }
            let committed = self.take_committed_with_highlight(state);
            state.input_buffer.clear();
            state.input_buffer_cased.clear();
            state.input_cursor_pos = 0;
            state.candidates.clear();
            state.active = Some(ModeKind::TempEnglish);
            state.temp_english_buffer = ch.to_string();
            state.temp_english_cursor = state.temp_english_buffer.len();
            state.temp_english_prefix = String::new();
            self.update_temp_english_candidates(state);
            let disp = state.preedit.clone();
            // 上屏使宿主光标前移，新组合起点已变：复位首显，让候选窗等 reflow 后的新坐标
            // 再显示并重锁起点，否则留在上屏前的位置（同顶码余码续打，见 handle_candidate）。
            if committed.is_some() {
                self.reset_first_show();
                self.first_show_after_commit
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.notify_ui_update(state);
            debug!("Entered temp English mode after commit (buffer={})", disp);
            return Some(match committed {
                Some(text) => self.commit_then_new_composition(text, disp),
                None => KeyAction::UpdateComposition {
                    caret_pos: disp.chars().count() as u32,
                    text: disp,
                },
            });
        }

        // 快捷输入已退役为内置类方案 mix 成员（quick_input），不再独立激活。

        // 联想态让位：缓冲虽空，但用户按 `;`/`'` 的意图是选第 2/3 条，不是进模式。判据用
        // `assoc_active()` 而非「候选非空」，不动空码补全那条行为。
        //
        // 位置必须在临英 Shift+字母之后：放到函数开头会把临英分支一起挡掉。让位保的是选词键
        // （恒不带 Shift，走下面的 `bound_key_decision`），与 Shift+字母值域不相交。
        if state.assoc_active() {
            return None;
        }

        // 方案级按键功能表（方案文件 / schema_overrides 的 `[key_actions]`）。
        //
        // 位置在英文模式分水岭之后（handle_key_event 里早已 PassThrough 返回）：有字符的键
        // 排在热键路径会让该字符在英文模式下永远打不出来。见 docs/design/schema-key-actions.md §4.1。
        // 命中即执行并跳过全局引导键链；显式 `none` 两边都不走；未声明的键才落全局链。
        if state.input_buffer.is_empty() && data.modifiers & (MOD_SHORTCUT | MOD_SHIFT) == 0 {
            if self.wildcard_takes_lead_key(data.key_code) {
                debug!(
                    "key_action: vk=0x{:02X} 让位 —— 符号通配键作首码",
                    data.key_code
                );
                return None;
            }
            match self.bound_key_decision(data.key_code) {
                BoundKeyDecision::Act(action) => {
                    if let Some(act) = self.enter_bound_action(state, &action, data.key_code) {
                        state.rewind = None; // 首键进入非夺取式，作废任何旧回退登记
                        return Some(act);
                    }
                    // 门卫没过（目标模式不可用）：落普通输入，绝不能返回 Consumed，否则配了个
                    // 不可用的目标就等于把这个键废掉且无从察觉。
                    return None;
                }
                // 让位：该键作正常码，同样不落全局引导键链。
                BoundKeyDecision::Yield => return None,
                BoundKeyDecision::NotBound => {}
            }
        }

        None
    }

    /// 当前方案的 z 键功能（`schema.codetable.z_key_action` 经方案折叠后的生效值）。
    ///
    /// 走 `codetable_settings()` 而非全局配置：不同码表里 z 的地位不同，全局值只是回落基线。
    ///
    /// 与 `[key_actions]` 表的关系见 [`Self::bound_action_for`]：表里显式写了 `z` 就以表为准。
    pub(crate) fn z_key_action(&self) -> BoundAction {
        BoundAction::parse(&self.engine_mgr.codetable_settings().z_key_action)
    }

    /// 方案级按键功能表对某个键的最终裁决，供所有「按引导键进模式」的通路共用。
    ///
    /// 必须单点：进模式有两条通路，空缓冲的 `try_activate_mode` 与有缓冲/候选时「顶字 +
    /// 进模式」的 `decideBufferedTrigger` 链（`coordinator.rs` 的 `_ =>` 臂，空码按键也会走到）。
    /// 只接一条，方案写了 `semicolon = "none"` 空码按 `;` 仍会进快捷输入。
    pub(crate) fn bound_key_decision(&self, key_code: u32) -> BoundKeyDecision {
        self.bound_key_decision_layered(key_code, true)
    }

    /// 同上，可跳过方案级层。语义与适用范围见
    /// [`Self::bound_action_with_source_layered`]（英文半角态用）。
    pub(crate) fn bound_key_decision_layered(
        &self,
        key_code: u32,
        use_schema_layer: bool,
    ) -> BoundKeyDecision {
        let Some((action, from_schema)) =
            self.bound_action_with_source_layered(key_code, use_schema_layer)
        else {
            return BoundKeyDecision::NotBound;
        };
        // 跨层仲裁：全局引导键遇上方案声明的首码要让位；方案级绑定则显式优先。
        // 见 docs/design/schema-key-actions.md §4.3 与 [`Self::bound_action_with_source`]。
        if !from_schema && self.code_char_takes_lead(key_code) {
            debug!("key_action: vk=0x{key_code:02X} 让位 —— 全局绑定遇方案首码（跨层仲裁）");
            return BoundKeyDecision::Yield;
        }
        // 方案级 `switch_schema` 不让位（单向切走、回程由别的键负责是合法配法）。目标方案里
        // 该键走到 `NotBound` 会落到 `is_toggle_mode_keycode`（lshift/rshift 出厂即 toggle_mode），
        // 由 `Coordinator::schema_switch_arrival` 记录 + `handle_bound_modifier_key_up` 的
        // `NotBound` 分支吞键兜底。
        match self.bound_action_yield_reason(key_code, &action) {
            Some(reason) => {
                debug!("key_action: vk=0x{key_code:02X} 让位 —— {reason}");
                BoundKeyDecision::Yield
            }
            None => {
                debug!("key_action: vk=0x{key_code:02X} → {action:?}");
                BoundKeyDecision::Act(action)
            }
        }
    }

    /// 这个键在当前方案里绑了什么动作；未绑定返回 `None`（落全局引导键链）。
    ///
    /// 三个来源，由具体到一般：
    /// 1. 方案文件 / `schema_overrides` 的 `[key_actions]`（任意键）
    /// 2. 全局 `keys.key_actions` 里的单键条目（组合键走热键通路，不在此列）
    /// 3. `schema.codetable.z_key_action`（只管 z）
    ///
    /// 先后由层级决定（方案覆盖全局），不靠调用顺序。键名走 `key_name_to_vk_with_letters`，
    /// 接受字母（字母能否借作功能键取决于它在这张码表里是不是死码，见
    /// [`Self::bound_action_yield_reason`]）；再叠 `modifier_name_to_vk`：修饰键名不在
    /// `KEY_TABLE` 里，漏了这层修饰键的 keyup 会查表落空、什么都不发生。
    pub(crate) fn bound_action_for(&self, key_code: u32) -> Option<BoundAction> {
        self.bound_action_with_source(key_code).map(|(a, _)| a)
    }

    /// 指定方案的 `[key_actions]` 里这个键绑了什么（只查方案层，不回落全局）。
    ///
    /// 与 [`Self::bound_action_for`] 按动词类别分工（docs/design/key-resolver-unification.md §4.4）：
    /// 「从这个输入环境去哪」（`special:*` / `temp_pinyin` / `switch_schema`…）恒走
    /// `bound_action_for`；「解释用户敲的码」（辅助码触发键、码元、分隔符）在 overlay 里按
    /// 产出候选的方案取，走本函数。不回落全局：那份已由 `bound_action_for` 覆盖，再回落就是
    /// 同一条配置查两遍、优先级无从定义。
    pub(crate) fn bound_action_in_schema(
        &self,
        key_code: u32,
        schema_id: &str,
    ) -> Option<BoundAction> {
        for (name, action) in self.engine_mgr.key_actions_of(schema_id).iter() {
            if crate::key_resolver::key_action_name_to_vk(name) == Some(key_code) {
                return Some(BoundAction::parse(action));
            }
        }
        None
    }

    /// 同 [`Self::bound_action_for`]，但一并给出这条绑定来自哪一层
    /// （`true` = 方案级 `[key_actions]`，`false` = 全局 `keys.key_actions` / `z_key_action`）。
    ///
    /// 层级是 `code_char_takes_lead` 仲裁的必需输入：全局引导键 × 方案 `leading_chars` 是
    /// 跨层冲突，让位给码表；方案级绑定 × 同方案 `leading_chars` 是同层冲突，绑定优先。
    /// 丢掉这个区分，全局引导键会把方案自己的码元抢走。见 docs/design/schema-key-actions.md §4.3。
    pub(crate) fn bound_action_with_source(&self, key_code: u32) -> Option<(BoundAction, bool)> {
        self.bound_action_with_source_layered(key_code, true)
    }

    /// 同上，但可跳过方案级层（`use_schema_layer = false` ⇒ 只认全局 `keys.key_actions`）。
    ///
    /// 英文半角态要跳过方案级层：此时当前方案整体不参与输入（`handle_key_event` 在英文
    /// 分水岭直接 `PassThrough`），用户期望的是全局配置。例：英文方案里 `lshift = toggle_mode`、
    /// 全局 `lshift = switch_schema:wubi86`，英文态按左 Shift 应走全局那条切回五笔。
    ///
    /// 实际只作用于修饰键（有字符的键英文态在分水岭前就透传，组合键走全局编译的
    /// `compiled_hotkeys`）。推给 C++ 的转发键集不得跟着这个维度裁剪，必须是并集，否则每切
    /// 一次中英文就要重推（docs/design/key-resolver-unification.md §4.2）。
    ///
    /// 代价：全局配了切方案类动词后，没有方案级同键绑定的方案在中文态下也会命中全局那条，
    /// 用户需要在每个方案里配 `lshift = "toggle_mode"`。这是已接受的取舍，设置页方案级面板有提示。
    fn bound_action_with_source_layered(
        &self,
        key_code: u32,
        use_schema_layer: bool,
    ) -> Option<(BoundAction, bool)> {
        // 宿主临时压制的引导键（移动端触摸 `;`）：按未绑定处理，三层都不查。
        if key_code != 0
            && self
                .trigger_suppressed
                .load(std::sync::atomic::Ordering::Relaxed)
                == key_code
        {
            return None;
        }
        // 键名→VK 走与全局层同一个解析口（`key_action_name_to_vk`），两层各留一份解析规则
        // 就会按维度分裂（见 docs/design/key-resolver-unification.md §2.5）。
        if use_schema_layer {
            for (name, action) in self.engine_mgr.active_key_actions().iter() {
                if crate::key_resolver::key_action_name_to_vk(name) == Some(key_code) {
                    return Some((BoundAction::parse(action), true));
                }
            }
        }
        // 全局 `keys.key_actions` 的单键条目，方案没表态时才落到这里。组合键条目由热键通路
        // 消费，这里再认一次就是两条路都触发。过滤与解析已在 `ConfigBundle::build` 的
        // `KeyResolver` 里做完，本函数在按键热路径上。
        if let Some(action) = self.rt().key_resolver.global_lead(key_code) {
            // 查得到就是全局层表了态，`none` 同样是表态，不再回落 `z_key_action`，
            // 故 `BoundAction::None` 也原样返回。
            return Some((action, false));
        }
        // 表里没写 z 时回落专用字段（自身已含全局→方案的折叠）。跳过方案级层时一并跳过：
        // `z_key_action` 本身是方案级配置（英文态 z 实际到不了这里，判据仍写出来）。
        if use_schema_layer && key_code == keymap::VK_Z {
            let z = self.z_key_action();
            if z.is_enabled() {
                // 按方案级来源计：与 leading_chars 同样是同层冲突。
                return Some((z, true));
            }
        }
        None
    }

    /// 这个键在会话态（有编码或候选）绑了什么动作；未绑定返回 `None`。
    ///
    /// 两层逐键合并（与 `[key_actions]` 一致）：方案文件 / `schema_overrides` 的
    /// `[session_actions]`，再到全局 `keys.session_actions`（已含 `page_keys` 等键组的展开）。
    /// 方案层查得到就是表了态，显式 `"none"` 返回 `None` 且不回落全局（`merge_toml` 只能
    /// 新增/覆盖，删行禁不掉全局绑定）。
    ///
    /// 本方法回答「当前方案下这个键干什么」，不回答「这个键要不要转发」：后者必须取所有方案
    /// 的并集（[`crate::config_bundle::schema_bound_modifier_vks`]），否则切方案会反复装卸钩子。
    pub(crate) fn session_action_for(
        &self,
        key_code: u32,
        shift: bool,
        include_printable: bool,
    ) -> Option<wind_config::SessionAction> {
        if let Some(a) = crate::key_resolver::schema_session_lookup(
            &self.engine_mgr.active_session_actions(),
            key_code,
            shift,
            include_printable,
        ) {
            return a.is_enabled().then_some(a);
        }
        self.rt()
            .session_keys
            .classify(key_code, shift, include_printable)
    }

    /// 绑了动作的键是否让位给正常输入（既不进模式、也不落全局引导键链），返回让位原因
    /// （`None` = 不让位），原因进 debug 日志：「配了不生效」有五个同形成因（没绑上 / 显式
    /// none / 非码表引擎 / repeat / 活码前缀），日志里一眼可辨。
    ///
    /// 字母键的两条判据（只在码表引擎判；拼音/混输绑了就生效，引导键只在空缓冲触发）：
    /// - 活码前缀：本方案码表/短语里有以该字母开头的条目，不让位它就打不出编码。
    /// - z 的 repeat 身份：`z_key_repeat` 开且有上屏历史时 z 归重复输入（z 专有）。
    ///
    /// 符号键不让位、不限引擎：它在码表里不产出编码，按下只可能是为了触发功能。
    fn bound_action_yield_reason(
        &self,
        key_code: u32,
        action: &BoundAction,
    ) -> Option<&'static str> {
        if !action.is_enabled() {
            return Some("显式 none");
        }
        let ch = keymap::vk_to_prefix_char_with_letters(key_code)?;
        // 反查模式只在活跃引擎给得出反查通配键时有意义（码表 / 五笔拼音混输）。放在字母 /
        // 符号分流之前：符号键在拼音方案里也让位、照常产出标点（spec §3.1）。必须在修饰键
        // 早退之后：修饰键 keyup 把 Yield 当显式 none 吞键，排在前面会让绑了反查的 RShift
        // 连中英切换都失效。
        if matches!(action, BoundAction::Reverse) && self.engine_mgr.active_reverse_key().is_none()
        {
            return Some("反查模式仅码表 / 五笔拼音混输方案生效");
        }
        if !ch.is_ascii_alphabetic() {
            return None; // 符号键：不让位，也不限引擎
        }
        // 非码表引擎不替用户判断：绑了字母就当引导键，只在空缓冲生效，组码中的 `lv` 不受影响。
        if !matches!(
            self.engine_mgr.current_engine_type(),
            Some(wind_engine::EngineType::CodeTable)
        ) {
            return None;
        }
        // z 的 repeat 身份只压得住有夺取回路的目标：`temp_pinyin` 有 `try_z_fallback`，让位后
        // `z…` 破了活码前缀仍会被夺取进临拼。special / mix / 临英只支持首键进入，让位一次就
        // 再也进不去（快符的 `show_all_on_enter` 价值全在首键那一下）。
        if key_code == keymap::VK_Z
            && matches!(action, BoundAction::TempPinyin)
            && self.z_key_repeat_text().is_some()
        {
            return Some("z 的 repeat 身份（目标是临拼，有 z-fallback 补救）");
        }
        self.has_code_prefix(&ch.to_ascii_lowercase().to_string())
            .then_some("该字母在本方案是活码前缀")
    }

    /// 执行 z 键功能：按 `action` 进对应模式（空缓冲进入语义，组合区前缀显示 `z`）。
    ///
    /// 各目标模式的可用性门卫都在这里，与引导键进入点同一套判据（临拼的
    /// `temp_pinyin_target`、mix 成员非空、特殊模式的 `ensure_schema`）。门卫没过返回 `None`，
    /// 调用方让键落普通输入：绝不能吞键，否则配了个不可用的目标就等于把这个键废掉。
    pub(crate) fn enter_bound_action(
        &self,
        state: &mut State,
        action: &BoundAction,
        key_code: u32,
    ) -> Option<KeyAction> {
        match action {
            BoundAction::None => None,
            // 反查模式门卫：活跃方案有反查通配键。没过返回 None，触发键落普通输入。
            BoundAction::Reverse => {
                if !self.reverse_mode_available() {
                    return None;
                }
                debug!("key_action: entering reverse mode");
                Some(self.enter_reverse_mode(state, key_code))
            }
            // 软键盘不是「模式」，没有编码缓冲也不进 ModeKind——直接开关面板即可。
            // 状态推送由按键路径顶层的 SoftKeyboardPushOnDrop 兜底。
            BoundAction::SoftKeyboard(page) => Some(self.toggle_softkeyboard(page.as_deref())),
            // 单字输入：与软键盘同类，不进 ModeKind。没有门卫，恒吞键；空缓冲态下状态泡是
            // 唯一的反馈，必须弹。
            BoundAction::SingleChar(a) => {
                if let Some(label) = self.apply_single_char_action(state, *a) {
                    self.show_tip_locked(state, label);
                }
                Some(KeyAction::Consumed)
            }
            // 注释态：持久设置，恒吞键；状态泡是空闲时唯一的反馈。
            BoundAction::CommentState(a) => {
                match self.apply_comment_state_action(state, *a) {
                    Ok(label) => self.show_tip_locked(state, label),
                    Err(e) => warn!("comment_state: {e}"),
                }
                Some(KeyAction::Consumed)
            }
            BoundAction::TempPinyin => {
                let target = self.engine_mgr.temp_pinyin_target()?;
                state.active = Some(ModeKind::TempPinyin);
                state.temp_pinyin_schema = target;
                state.temp_pinyin_buffer.clear();
                state.temp_pinyin_prefix = Self::temp_pinyin_prefix_for(key_code).to_string();
                self.update_temp_pinyin_candidates(state);
                let display = state.preedit.clone();
                self.notify_ui_update(state);
                debug!("key_action: entered temp pinyin");
                Some(KeyAction::UpdateComposition {
                    text: display.clone(),
                    caret_pos: display.chars().count() as u32,
                })
            }
            BoundAction::TempEnglish => {
                if !self.rt().config.input.temp_english.enabled {
                    return None;
                }
                state.active = Some(ModeKind::TempEnglish);
                state.temp_english_buffer.clear();
                state.temp_english_cursor = 0;
                state.temp_english_prefix = keymap::vk_to_prefix_char_with_letters(key_code)
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                self.update_temp_english_candidates(state);
                let display = state.preedit.clone();
                self.notify_ui_update(state);
                debug!("key_action: entered temp English");
                Some(KeyAction::UpdateComposition {
                    text: display.clone(),
                    caret_pos: display.chars().count() as u32,
                })
            }
            BoundAction::Mix(id) => {
                let idx = self.mix_mode_idx(id)?;
                // 与引导键进入点同一门卫：含 quick_input 或至少一个可加载成员方案。
                if !self.mix_has_quick_input(idx) && self.mix_members(idx).is_empty() {
                    return None;
                }
                debug!("key_action: entering mix idx={}", idx);
                Some(self.enter_mix_mode(state, idx, key_code))
            }
            // 辅助码：空缓冲无候选可筛 → 门卫返回 None，触发键落普通标点流程。
            BoundAction::AuxCode => self.enter_aux_code(state, key_code),
            BoundAction::Special(id) => {
                let idx = self.special_mode_idx(id)?;
                let schema = self.special_schema(idx)?;
                if !self.engine_mgr.ensure_schema(&schema) {
                    return None;
                }
                debug!("key_action: entering special idx={}", idx);
                Some(self.enter_special_mode(state, idx, key_code))
            }
            // 生僻字模式用的就是当前活跃方案，必然已加载，无需 `ensure_schema` 门卫。
            BoundAction::RareChar => {
                debug!("key_action: entering rare-char mode");
                Some(self.enter_rare_char_mode(state, key_code))
            }
            // C 类不在这里执行，见 [`Self::run_toggle_schema_action`]：本函数持 `State` 锁，
            // 而 `finish_user_schema_switch` 自己加锁，在这里调就是死锁。且 C 类必须在英文态
            // 也生效，而本函数在分水岭之后的 keydown 路径上，故 C 类只能绑无字符键（修饰键 keyup）。
            BoundAction::ToggleSchema(id) => {
                warn!("key_actions: toggle_schema:{id} 只能绑修饰键（无字符键），此处忽略");
                None
            }
            BoundAction::SwitchSchema(id) => {
                warn!("key_actions: switch_schema:{id} 只能绑修饰键（无字符键），此处忽略");
                None
            }
            // A 类同样不在这里执行：`dispatch_hotkey` 自加锁。keydown 走
            // `bound_lock_free_action_for_keydown`（判定后 drop 锁），keyup 走
            // `handle_bound_modifier_key_up`，两条都在锁外。命令同一分流口。
            BoundAction::Action(_) | BoundAction::Command(_) => None,
        }
    }

    /// 执行 C 类 `toggle_schema`。必须在不持 `State` 锁时调用（内部经
    /// `finish_user_schema_switch` 自行加锁）。目标加载不了时返回 `None` 不吞键。
    fn run_toggle_schema_action(&self, id: &str, trigger_vk: u32) -> Option<KeyAction> {
        if !self.engine_mgr.ensure_schema(id) {
            warn!("key_actions: toggle_schema 目标 {id} 加载失败，不动作");
            return None;
        }
        debug!("key_actions: toggle_schema -> {id}");
        let commit = self.toggle_schema_by_id(id, trigger_vk);
        Some(self.schema_switch_key_action(commit))
    }

    /// 执行 C 类 `switch_schema`（单向）。锁约束同 [`Self::run_toggle_schema_action`]。
    ///
    /// 与往返版的差别是不记来源、不认回程键。方案级绑定也会走到这里（不让位），目标方案里
    /// 该键的误触由 `schema_switch_arrival` 兜底。
    fn run_switch_schema_action(&self, id: &str, trigger_vk: u32) -> Option<KeyAction> {
        if !self.engine_mgr.ensure_schema(id) {
            warn!("key_actions: switch_schema 目标 {id} 加载失败，不动作");
            return None;
        }
        debug!("key_actions: switch_schema -> {id}");
        let commit = self.switch_schema_by_id(id);
        // 记下「这把键刚把用户单向送到了这里」，供目标方案里再按时吞键（见
        // `Coordinator::schema_switch_arrival`），不是回程记录。切换失败（active 未变）时不记，
        // 同 `toggle_schema_by_id` 的判据。全局配法下目标方案里该键仍命中全局表走 `Act`，
        // 到不了消费它的 `NotBound`，故不必区分来源层。
        if trigger_vk != 0 && self.engine_mgr.active_schema_id() == id {
            *self
                .schema_switch_arrival
                .lock()
                .unwrap_or_else(|e| e.into_inner()) =
                Some((trigger_vk, self.engine_mgr.schema_generation()));
        }
        Some(self.schema_switch_key_action(commit))
    }

    /// 这把键是不是刚刚把用户单向送到当前方案的那一把（见
    /// [`Coordinator::schema_switch_arrival`]）。
    ///
    /// 与 [`Self::schema_toggle_key_authorized`] 不可合并：那个回答「能不能执行往返」，本函数
    /// 回答「该不该被安静吃掉」；合并会让单向绑定获得未声明的回程语义。代际不等 = 期间用
    /// 别的方式切过方案，记录作废。
    fn schema_switch_key_arrived(&self, key_code: u32) -> bool {
        if key_code == 0 {
            return false;
        }
        matches!(
            *self
                .schema_switch_arrival
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            Some((vk, arrived_gen))
                if vk == key_code && arrived_gen == self.engine_mgr.schema_generation()
        )
    }

    /// 执行 A 类状态切换，转交 `dispatch_hotkey`。必须在不持 `State` 锁时调用，与 C 类共用
    /// [`Self::run_lock_free_bound_action`] 分流口。分发端不认的动词返回 `None` 不吞键
    /// （说明与 `BoundAction::parse` 白名单不同步，落回正常输入比静默吃掉好查）。
    fn run_dispatch_action(&self, action: &str) -> Option<KeyAction> {
        // `_keyed`：按键路径上，`toggle_mode` / `switch_engine` 的编码要经本次按键应答回给宿主，
        // 不能走 `dispatch_hotkey` 的 push 出口。
        let Some(act) = self.dispatch_hotkey_keyed(action) else {
            warn!("key_actions: 动作 {action} 未被 dispatch_hotkey 接受，不动作");
            return None;
        };
        debug!("key_actions: dispatch {action}");
        Some(act)
    }

    /// 该动作是否属于「锁外执行」那一类（A/C：不建 overlay、只改全局状态，目标函数
    /// `toggle_schema_by_id` / `dispatch_hotkey` 自己加 `State` 锁）。与
    /// [`Self::run_lock_free_bound_action`] 的 match 臂同源；分成两个函数是因为 keyup 路径要
    /// 先判断再决定取不取锁。
    ///
    /// 穷举 `match` 不用 `matches!`：这是组合键分派（[`Self::dispatch_bound_action_hotkey`]）
    /// 锁约束的唯一支点，新变体静默落进 `false` 而执行函数自取锁就会死锁，列全则新增变体编译不过。
    pub(crate) fn is_lock_free_bound(&self, action: &BoundAction) -> bool {
        match action {
            // 目标函数自取 `State` 锁 ⇒ 必须锁外。命令持锁调也不死锁，但它不建 overlay，
            // 归这一类才与分流口同形。
            BoundAction::ToggleSchema(_)
            | BoundAction::SwitchSchema(_)
            | BoundAction::Action(_)
            | BoundAction::Command(_) => true,
            // 建 overlay，要 `&mut State` ⇒ 调用方持锁。
            //
            // `SoftKeyboard` 在这里是 `false` 但也要锁外执行（`softkeyboard_hotkey` 自己取锁）：
            // 它不经 `run_lock_free_bound_action`，由 `dispatch_bound_action_hotkey` 在取锁前
            // 单独分出去。别看到 `false` 就把它塞进持锁分支。
            BoundAction::None
            | BoundAction::TempPinyin
            | BoundAction::TempEnglish
            | BoundAction::AuxCode
            | BoundAction::RareChar
            | BoundAction::Reverse
            | BoundAction::Mix(_)
            | BoundAction::Special(_)
            | BoundAction::SingleChar(_)
            | BoundAction::CommentState(_)
            | BoundAction::SoftKeyboard(_) => false,
        }
    }

    /// A/C 两类动作的统一分流口，调用方须不持 `State` 锁。返回 `None` 表示「不是这两类」，
    /// 调用方继续走原有链路。B 类相反，建 overlay、需要 `&mut State`。
    pub(crate) fn run_lock_free_bound_action(
        &self,
        action: &BoundAction,
        trigger_vk: u32,
    ) -> Option<KeyAction> {
        match action {
            BoundAction::ToggleSchema(id) => self.run_toggle_schema_action(id, trigger_vk),
            BoundAction::SwitchSchema(id) => self.run_switch_schema_action(id, trigger_vk),
            BoundAction::Action(a) => self.run_dispatch_action(a),
            BoundAction::Command(expr) => {
                self.run_bound_command(expr);
                Some(KeyAction::Consumed)
            }
            _ => None,
        }
    }

    /// 执行按键绑定的命令（`command:<表达式>`，`key_actions` 与 `session_actions` 两张表共用）。
    ///
    /// 执行体与工具栏自定义按钮同一个（`spawn_user_command`）。异步执行、不等结果，命令的
    /// 成败由 cmdbar 自己弹 toast。表达式是用户内容（可能含路径 / 网址），只进 debug。
    pub(crate) fn run_bound_command(&self, expr: &str) {
        info!("执行了按键绑定的命令");
        debug!("按键绑定命令: {expr}");
        self.spawn_user_command(expr);
    }

    /// 组合键热键命中后的分派：与单键、修饰键两条通路同一个值域（[`BoundAction`]）。
    /// 返回 `None` = 本函数不接这个动词，调用方继续走原有按键链路（不吞键）。
    ///
    /// 与引导键上下文的差别，都是这条通路专有的：
    /// 1. `key_code` 恒传 0（哨兵）：热键进入不写引导符。
    /// 2. `chinese_mode` 守卫：判据取 [`BoundAction::only_in_chinese_mode`]，与编译期给不给
    ///    `CHINESE_ONLY` 位同一个方法；别的路径转发进来的同一个键会在英文态凭空建组合区。
    ///    只覆盖 B 类，A 类在它之前分流走（理由见 `only_in_chinese_mode`）。
    /// 3. 幂等：已在目标模式时安静吃掉，不重开。
    /// 4. 门卫没过也吃键：与引导键通路刻意相反，见函数末尾。
    ///
    /// 两个动词走独立分支：`SoftKeyboard`（`softkeyboard_hotkey` 自己取锁，只能锁外调，
    /// 也不受 `chinese_mode` 守卫）；`add_word` / `open_add_word_dialog` 不在 `BoundAction`
    /// 值域内（要返回占位 composition 让 C++ 转发全部按键），调用方在本函数之前特判。
    pub(crate) fn dispatch_bound_action_hotkey(&self, action: &str) -> Option<KeyAction> {
        let parsed = BoundAction::parse(action);
        if !parsed.is_enabled() {
            debug!("Unhandled hotkey action: {action}");
            return None;
        }
        // 软键盘：锁外，且不判中英态。
        if let BoundAction::SoftKeyboard(page) = &parsed {
            return Some(self.softkeyboard_hotkey(page.as_deref()));
        }
        // A/C 类：不建 overlay，目标函数自加锁，必须在锁外。trigger_vk 传 0：组合键在所有
        // 方案里都命中，不需要回程键临时授权（见 `schema_toggle_key_authorized`）。
        if self.is_lock_free_bound(&parsed) {
            return Some(
                self.run_lock_free_bound_action(&parsed, 0)
                    .unwrap_or(KeyAction::Consumed),
            );
        }
        // B 类：建 overlay，要 `&mut State`。
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if parsed.only_in_chinese_mode() && !state.chinese_mode {
            return None;
        }
        // 已在目标模式：安静吃掉。放行会让组合键泄漏给宿主，重开会清掉用户已打的编码。
        if let Some(kind) = self.bound_action_mode_kind(&parsed)
            && state.active == Some(kind)
        {
            return Some(KeyAction::Consumed);
        }
        // 门卫没过（目标模式不可加载 / 临拼目标方案不是码表 / 辅助码无候选可筛）时仍然吞键：
        // 引导键落下去是打出一个字符（无害），组合键落下去是把 `Ctrl+;` 交给宿主执行它自己的
        // 加速键。中文态守卫一过这个键就归本函数管。出厂 `input.temp_pinyin.hotkey` 也走这里，
        // 活跃方案是拼音时 `temp_pinyin_target()` 恒为 `None`，属正常配置。
        Some(
            self.commit_and_enter_bound_action(&mut state, &parsed, 0)
                .unwrap_or(KeyAction::Consumed),
        )
    }

    /// 动作对应的独占模式身份，供热键路径做幂等判断；没有对应模式的动作返回 `None`。
    ///
    /// id 解析不出下标时也返回 `None`（那种配置根本进不去）。
    ///
    /// 穷举 `match` 不用 `_` 兜底：漏掉一个建 overlay 的变体就丢幂等，再按一次会清掉用户
    /// 已打进模式里的编码。列全则新增变体编译不过。
    fn bound_action_mode_kind(&self, action: &BoundAction) -> Option<ModeKind> {
        Some(match action {
            BoundAction::TempPinyin => ModeKind::TempPinyin,
            BoundAction::TempEnglish => ModeKind::TempEnglish,
            BoundAction::AuxCode => ModeKind::AuxCode,
            BoundAction::RareChar => ModeKind::RareChar,
            BoundAction::Reverse => ModeKind::Reverse,
            BoundAction::Special(id) => ModeKind::Special(self.special_mode_idx(id)?),
            BoundAction::Mix(id) => ModeKind::Mix(self.mix_mode_idx(id)?),
            // 不建 overlay ⇒ 没有可比对的模式身份。软键盘的幂等由 `toggle_softkeyboard`
            // 自己处理（再按一次就是关掉）。
            BoundAction::None
            | BoundAction::SingleChar(_)
            | BoundAction::CommentState(_)
            | BoundAction::SoftKeyboard(_)
            | BoundAction::ToggleSchema(_)
            | BoundAction::SwitchSchema(_)
            | BoundAction::Action(_)
            | BoundAction::Command(_) => return None,
        })
    }

    /// keydown 路径上的 A 类分派判定：判定在锁内、执行在锁外。
    ///
    /// 调用方拿到 `Some` 后须先 `drop` 掉 `State` guard 再执行（见
    /// [`Self::run_lock_free_bound_action`]）。本函数只读 `state`。三道门：
    /// - 空缓冲：打字打到一半按下绑定键，意图多半是输入；已有编码留给下游的顶字逻辑。
    /// - 无修饰键：`Ctrl+\` 是宿主的快捷键，不该被方案绑定截走。
    /// - 不限修饰键的动作：`toggle_mode` 那类绑在有字符的键上是单程票，
    ///   见 [`BoundAction::requires_modifier_key`]。
    pub(crate) fn bound_lock_free_action_for_keydown(
        &self,
        state: &State,
        data: &KeyEventData,
    ) -> Option<BoundAction> {
        if !state.input_buffer.is_empty() || data.modifiers & (MOD_SHORTCUT | MOD_SHIFT) != 0 {
            return None;
        }
        // 与 `try_activate_mode` 同一道首位仲裁：符号通配键让全局绑定让位。
        if self.wildcard_takes_lead_key(data.key_code) {
            return None;
        }
        let BoundKeyDecision::Act(action) = self.bound_key_decision(data.key_code) else {
            return None;
        };
        if action.requires_modifier_key() {
            // 有字符的键到不了英文态，绑这类动作等于单程票。忽略并 warn，设置页另有行内提示。
            warn!(
                "key_actions: {action:?} 只能绑修饰键（无字符键），键 0x{:02X} 上忽略",
                data.key_code
            );
            return None;
        }
        matches!(action, BoundAction::Action(_) | BoundAction::Command(_)).then_some(action)
    }

    /// 纯修饰键 keyup 上的方案级绑定分派（`rshift = "toggle_schema:english"` 这类）。
    ///
    /// 与 keydown 侧的 `try_activate_mode` 是互补的两半：修饰键没有字符，到不了 keydown 那条链
    /// （TSF 只在干净单击后于 keyup 转发，见 `KeyEventSink.cpp`）。判据是键的形态（有无字符），
    /// 不是动词类别，见 docs/design/schema-key-actions.md §4.1。
    ///
    /// 返回 `None` 表示本函数不接管，调用方继续走 `is_toggle_mode_keycode`。
    pub(crate) fn handle_bound_modifier_key_up(&self, key_code: u32) -> Option<KeyAction> {
        // 英文半角态跳过方案级层，只认全局配置（理由与代价见 `bound_action_with_source_layered`），
        // 本函数是这条规则的唯一落点。
        //
        // 在此读 `chinese_mode` 要求调用点不持 `State` 锁（message_handler 的 keyup 分支直接调，
        // 上游各自 lock 各自释放）。持锁的调用方接进来就得改为传入该值，否则死锁。
        let chinese = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.chinese_mode
        };
        match self.bound_key_decision_layered(key_code, chinese) {
            // 活跃方案没绑这个键，但它可能携带往返语义（刚才正是用它把用户带到当前方案），
            // 见 `schema_toggle_key_authorized`。授权成立后一律转交 `toggle_schema_by_id` 按完整
            // 落点裁决，与全局 `keys.key_actions` 配置走同一条路；别在这里另写一份回程判断。
            BoundKeyDecision::NotBound => {
                if self.schema_toggle_key_authorized(key_code) {
                    let commit =
                        self.toggle_schema_by_id(&self.engine_mgr.active_schema_id(), key_code);
                    return Some(self.schema_switch_key_action(commit));
                }
                // 方案级单向切换刚把用户送到这里：吞键、不动作。绝不能返回 `None` 落回全局链：
                // `lshift`/`rshift` 出厂是 `toggle_mode`，「切方案」会变成「切了中英文」。
                // 与上面那条的区别是吞键 vs 回程：单向没有回程。
                if self.schema_switch_key_arrived(key_code) {
                    debug!("switch_schema: 0x{key_code:02X} 是本方案的单向送达键，吞键不动作");
                    return Some(KeyAction::Consumed);
                }
                None
            }
            // 显式 `none`：屏蔽该键的全局绑定（多半是 `toggle_mode`），必须接管并返回 Consumed。
            BoundKeyDecision::Yield => {
                debug!("bound modifier key_up 0x{key_code:02X} 让位（多为显式 none）");
                Some(KeyAction::Consumed)
            }
            // A/C 类必须在锁外执行（目标函数自己加锁），故先于取锁分流。
            BoundKeyDecision::Act(action) if self.is_lock_free_bound(&action) => {
                self.run_lock_free_bound_action(&action, key_code)
            }
            BoundKeyDecision::Act(action) => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let act = self.enter_bound_action(&mut state, &action, key_code);
                drop(state);
                // 门卫没过时不吞键：返回 None 让全局链接手，与 keydown 侧一致。
                act
            }
        }
    }

    /// 复位三种独占输入模式（临时英文/临时拼音/快捷输入）的状态。仅清空，不负责上屏；
    /// 调用方需在调用前取出待上屏文本（如模式切换时的临时英文缓冲）。
    pub(crate) fn reset_exclusive_modes(&self, state: &mut State) {
        let dirty = state.active.is_some();
        state.active = None;
        state.temp_english_buffer.clear();
        state.temp_english_cursor = 0;
        state.temp_english_prefix.clear();
        state.temp_pinyin_buffer.clear();
        state.temp_pinyin_cursor = 0;
        state.temp_pinyin_prefix.clear();
        state.url_buffer.clear();
        state.url_cursor = 0;
        state.email_buffer.clear();
        state.email_cursor = 0;
        state.unicode_buffer.clear();
        state.unicode_cursor = 0;
        state.rewind = None;
        state.special_buffer.clear();
        state.special_cursor = 0;
        // 翻页扩充 / 末页放宽三位：`scope_relaxed` 的失效点 `expire_scope_override` 只挂在按键
        // 出口，失焦等清缓冲的路径不经过它，不在这里复位就会让新组码继承上一个焦点的放宽态。
        state.has_more = false;
        state.candidate_limit = 0;
        state.scope_relaxed = false;
        // `[overlay]` 段快照随模式一并丢弃，不依赖消费点「先判 active」的实现细节。
        state.overlay_spec = None;
        state.mix_buffer.clear();
        state.mix_cursor = 0;
        state.aux_code = None;
        // 清理可能残留的组合显示（临时拼音/快捷输入会产生候选与 preedit）
        state.input_buffer.clear();
        state.input_buffer_cased.clear();
        state.english_case_variant = crate::english_candidates::CaseVariant::default();
        state.input_cursor_pos = 0;
        state.candidates.clear();
        state.grid_top = None;
        state.grid_tab = wind_ui_types::GridTab::All;
        state.grid_full.clear();
        state.preedit.clear();
        // 拼音逐步转换的已转换前缀一并丢弃（焦点/模式切换不保留半成品组合）。
        state.committed_text.clear();
        state.committed_segs.clear();
        // 焦点/模式切换：解除智能符号待命，避免跨上下文误触发替换。
        self.disarm_smart_symbol();
        // 快捷加词模式遗留：焦点/模式切换时退出。布局无需在此恢复：模式标志已清，下次候选
        // 显示会声明式算回全局基线（见 layout.rs），别改回逐出口「保存/恢复」。
        if state.add_word_active {
            state.add_word_active = false;
            state.add_word_chars.clear();
            state.add_word_clip = None;
            state.add_word_from_clip = false;
            state.add_word_len = 0;
            state.add_word_code.clear();
        }
        if dirty {
            debug!("reset_exclusive_modes: cleared residual exclusive input mode state");
        }
    }
}

#[cfg(test)]
#[path = "handle_lifecycle_restart_tests.rs"]
mod restart_tests;
