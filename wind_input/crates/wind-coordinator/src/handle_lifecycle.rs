//! 生命周期：配置重载、服务重启、独占模式进入/复位。
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。
//! 注：IME 激活/失活、焦点变更、composition 终止是 MessageHandler trait 方法，留在
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
    /// 表了态但该键让位（显式 `none` / 活码前缀 / z 的 repeat 身份）——
    /// 落普通输入，且**不再落全局引导键链**。
    Yield,
    /// 执行这个动作。
    Act(BoundAction),
}

impl Coordinator {
    /// 重启服务进程：隐藏 UI 后向 main 发重启信号（main 释放单例并重拉自身）。
    pub(crate) fn restart_service(&self) {
        info!("Restart service requested from menu");
        // 若有活跃 composition（拼音输入中/独占模式），先清空内部状态并通知 TSF 清除 composition，
        // 避免服务退出后 TSF 持有孤儿 composition 导致残留。
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let has_composition = !state.input_buffer.is_empty()
            || !state.preedit.is_empty()
            || !state.committed_text.is_empty()
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
        // 语言栏图标的呈现参数也在配置里（[ui.langbar]）。少了这一步，改了角标形状/配色
        // 要重启才生效——「改了没反应、重启就好」正是本仓反复出现的那类缺陷（运行时镜像态
        // 没回灌）。内部自带"无变化则不重发"，故白调一次的成本是零。
        #[cfg(all(feature = "desktop-ui", windows))]
        self.apply_langbar_config();
        // 速度修正系数也是运行时镜像态：采集器在 flush 时用它算 `max_speed` 并落库，
        // 不回灌就会出现「改了配置、当日速度变了而历史最快还按旧系数」的两套口径。
        if let Some(c) = self.stat_collector.as_ref() {
            c.set_speed_factor(self.rt().config.stats.speed_factor);
        }
        // 不再弹「已重载」气泡：热重载统一由 reload_user_config 的 toast 通知，避免重复。
    }

    /// 该键是否被**任一模式**配作进入键（临拼/临英的符号触发键、特殊模式引导键、mix 触发键）。
    /// 仅用于「智能符号 press2 要不要抢在模式激活之前」的门控：只有被模式占用的符号键存在
    /// 这个冲突，其余标点照常在标点分支判 press2。
    ///
    /// z 键功能（`z_key_action`）刻意不算：它要过三重身份裁决，且字母键根本不产出标点，
    /// `punct_char` 那一关就已经把它挡在门外。
    fn is_any_mode_trigger(&self, key_code: u32) -> bool {
        self.is_temp_pinyin_trigger(key_code)
            || self.is_temp_english_trigger(key_code)
            || self.match_special_trigger(key_code).is_some()
            || self.match_mix_trigger(key_code).is_some()
    }

    /// 该键在空缓冲时是否已被方案声明为**首码** —— 是则符号类模式引导键让位给码表。
    ///
    /// 冲突现场：`;` 既是 `quick_mix` 的引导键，又被某方案写进了 `input_chars`。模式激活
    /// 排在码元闸门之前且守卫同为「空缓冲」，于是引导键恒赢、方案里配的码元只在组码中生效
    /// （`abc;` 可用而 `;abc` 不可用），且毫无提示。
    ///
    /// 仲裁交给**首码集**：写进 `leading_chars`（未显式配置时等于 `input_chars`）即表示
    /// 「这个字符可以起头」，那它在空缓冲时就该归码表。想两者共存，把该符号排除出
    /// `leading_chars` 即可——它便只在组码中作码元，空缓冲仍进模式。
    ///
    /// ★ **只管非字母**。字母默认全在首码集里，一并让位会直接废掉两个既有功能：
    /// Shift+字母进临时英文、以及 `z_key_action` 的三重身份裁决（后者本就自带
    /// `has_code_prefix("z")` 判据处理码元冲突，不需要也不能被本函数接管）。
    ///
    /// ★★ 五c 之后**只对全局层绑定生效**（`bound_key_decision` 按来源分流）：这是
    /// **跨层**仲裁——全局配置无从知道某个方案把这个符号当码元用了。方案级 `[key_actions]`
    /// 与同方案的 `leading_chars` 是同层冲突，显式绑定优先，见 §4.3。
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

    /// 空缓冲模式激活的单一入口。命中返回激活 KeyAction，都不命中返回 None（落普通输入）。
    /// URL 前缀夺取是「缓冲扩展夺取」语义，不在此链，单独处理。
    ///
    /// # 五c 之后：硬编码优先级链已消失
    ///
    /// 曾经这里按固定顺序依次问「是不是临英触发键 / 临拼触发键 / 特殊模式 / mix」，
    /// 那套顺序**不是数据、无从配置**，且两个实例配同一个键时后者静默失效。
    /// 现在只剩两段：
    ///
    /// 1. **临时英文 Shift+字母** —— 它不由 `key_actions` 表达（键是 Shift+任意字母，
    ///    不是某个具体键），故仍是独立分支。
    /// 2. **`bound_key_decision`** —— 所有具名键的绑定，方案级 → 全局单键 → `z_key_action`
    ///    由具体到一般。一个键只有一个动作，冲突在配置合并期就定了。
    ///
    /// ⚠️ 行为统一：老实现里临英/special/mix 三段要求 `candidates.is_empty()`、临拼那段
    /// 刻意不要求（其注释明说「不要求候选空」）。单点裁决无法按动作分条件而不增复杂度，
    /// 统一取**不要求**——跟随临拼的既有行为。影响面是「空缓冲但有候选」（空码补全等）
    /// 时按引导键：原先 special/mix 不进、现在进。这是本次收编唯一有意的行为变化。
    pub(crate) fn try_activate_mode(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        // ★ 联想态让位：此刻缓冲虽空，但屏幕上摆着一批候选，用户按 `;`/`'` 的意图是
        // **选第 2/3 条**，不是进快捷输入。
        //
        // 上面那条「统一取不要求候选空」的裁决是针对空码补全那类候选定的——那时用户
        // 确实没在选词。联想是另一回事：不让位的话，`;` 会把刚出来的联想窗顶掉换成
        // 模式引导符，而二三候选键在联想态**永远按不出来**。
        //
        // 判据用 `assoc_active()` 而非「候选非空」，正是为了不动空码补全那条既有行为。
        if state.assoc_active() {
            return None;
        }
        // 智能符号 press2 **优先于模式激活**：模式内二次按进入键时已上屏中文标点并武装
        // （见 `arm_smart_symbol_after_commit`），时限内再按同键必须替换成英文形，而不是又进
        // 一次模式——否则被模式占用的符号键（`;` / `` ` `` / `\`）永远打不出英文形，武装白武装。
        //
        // 三重收窄，确保不惊扰既有路径：① 仅空闲态（无缓冲/无已转换前缀/无候选，缓冲非空时的
        // 模式触发另有 `decideBufferedTrigger` 那条链，不归此处管）；② 仅被某模式占用的键
        // （普通标点仍按原路径在标点分支判 press2，路径与风险都不扩散）；③ 仅判 press2，不武装。
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

        // 临时英文：Shift+字母（空缓冲 + 无候选 + 已启用）
        if state.input_buffer.is_empty()
            && state.candidates.is_empty()
            && self.rt().config.input.temp_english.enabled
            && data.modifiers & MOD_SHIFT != 0
            && data.modifiers & MOD_SHORTCUT == 0
            && (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code)
        {
            let ch = (b'A' + (data.key_code - 0x41) as u8) as char; // 首字母大写
            // shift_behavior == "direct_commit"：不进临时英文，直接上屏大写字母（对齐 Go）。
            if self.rt().config.input.temp_english.shift_behavior == "direct_commit" {
                let out = if state.full_width {
                    wind_transform::fullwidth::to_full_width(&ch.to_string())
                } else {
                    ch.to_string()
                };
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

        // 快捷输入已退役为内置类方案 mix 成员（quick_input），不再独立激活：
        // 想要纯快捷输入，配一个 members=["quick_input"] 的 mix 即可。; 默认走「快捷」融合 mix。

        // 方案级按键功能表（方案文件 / schema_overrides 的 `[key_actions]`）。
        //
        // 位置：**英文模式分水岭之后**（那在 handle_key_event 里，早已 PassThrough 返回）。
        // 有字符的键必须排在这里而不是热键路径，否则该字符在英文模式下永远打不出来。
        // 详见 docs/design/schema-key-actions.md §4.1。
        //
        // 命中即执行并跳过下方全局引导键链；显式 `none` 则两边都不走（return None 落普通
        // 输入）；未声明的键才落全局链——这是「未配置者行为逐字节不变」的保证。
        if state.input_buffer.is_empty() && data.modifiers & (MOD_SHORTCUT | MOD_SHIFT) == 0 {
            match self.bound_key_decision(data.key_code) {
                BoundKeyDecision::Act(action) => {
                    if let Some(act) = self.enter_bound_action(state, &action, data.key_code) {
                        state.rewind = None; // 首键进入非夺取式，作废任何旧回退登记
                        return Some(act);
                    }
                    // 门卫没过（目标模式不可用）：不吞键，落普通输入。绝不能返回 Consumed——
                    // 配了个不可用的目标就等于把这个键废掉，且用户完全看不出原因。
                    return None;
                }
                // 让位（显式 none / 活码前缀 / z 的 repeat 身份）：该键作正常码，同样不落
                // 全局引导键链——方案既然给这个键表了态，就不该再被全局 trigger_keys 抢走。
                BoundKeyDecision::Yield => return None,
                BoundKeyDecision::NotBound => {}
            }
        }

        None
    }

    /// 当前方案的 z 键功能（`schema.codetable.z_key_action` 经方案折叠后的生效值）。
    ///
    /// 走 `codetable_settings()` 而非直接读全局配置：这是**方案级**配置，不同码表里 z 的
    /// 地位不同（五笔 86 是死码，别的码表未必），全局值只是没有方案覆盖时的回落基线。
    ///
    /// 与 `[key_actions]` 表的关系见 [`Self::bound_action_for`]：表里显式写了 `z` 就以表为准。
    pub(crate) fn z_key_action(&self) -> BoundAction {
        BoundAction::parse(&self.engine_mgr.codetable_settings().z_key_action)
    }

    /// 方案级按键功能表对某个键的**最终裁决**，供所有「按引导键进模式」的通路共用。
    ///
    /// ★ 必须单点：进同一个模式有**两条**通路——空缓冲的 `try_activate_mode`，以及有缓冲/
    /// 候选时「顶字 + 进模式」的 `decideBufferedTrigger` 链（`coordinator.rs` 的 `_ =>` 臂）。
    /// 后者的模式触发判定**不要求缓冲非空**，空码按键同样会走到。只接一条的后果是：方案里
    /// 写了 `semicolon = "none"`，空码按 `;` 仍然进快捷输入——第一条放行、第二条接管。
    ///
    /// 同源教训见 `project_mixed_overflow_vs_topcode`（混输上屏三条通路，否决开关必须三处
    /// 都接）。盘查的判据是「进这个模式有几个入口」，不是「我改的函数里有几个分支」。
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
        // 跨层仲裁：**全局**引导键遇上方案声明的首码要让位——全局配置无从知道某个方案
        // 把这个符号当码元用了。方案级绑定则相反（同层冲突，显式绑定优先于字符集推导）。
        // 见 docs/design/schema-key-actions.md §4.3 与 [`Self::bound_action_with_source`]。
        if !from_schema && self.code_char_takes_lead(key_code) {
            debug!("key_action: vk=0x{key_code:02X} 让位 —— 全局绑定遇方案首码（跨层仲裁）");
            return BoundKeyDecision::Yield;
        }
        // 注：方案级 `switch_schema` 曾在此整条让位并 warn。**2026-08-30 放开**——
        // 当时的理由是「单向切走后目标方案没有这条绑定，这个键就再也按不动了」，但那描述的
        // 是**这把键**按不动，而回程本就可以由别的键负责（用户的实际配法正是「右 Shift 单向
        // 去英文方案、左 Shift 管中英文态」）。禁令把一个「可能的困扰」升成了「绝对禁止」，
        // 挡掉了合法配法。
        //
        // ★ 但禁令担心的**后果**是真的，只是换了个地方兜：目标方案里该键走到 `NotBound`，
        // 若就此返回 None 会落到 `is_toggle_mode_keycode`，而 lshift/rshift 出厂就是
        // `toggle_mode` 键 ⇒「配的是切方案却切了中英文」。现由
        // `Coordinator::schema_switch_arrival` 记录 + `handle_bound_modifier_key_up` 的
        // `NotBound` 分支吞键兜底，见那两处。
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
    /// 三个来源，**由具体到一般**：
    /// 1. 方案文件 / `schema_overrides` 的 `[key_actions]`（任意键）
    /// 2. 全局 `keys.key_actions` 里的**单键**条目（组合键走热键通路，不在此列）
    /// 3. `schema.codetable.z_key_action`（只管 z，早于本表存在的专用字段）
    ///
    /// 第 2 层是五c「全局层收编」的落点：五处 `trigger_keys` 折算到这里之后，
    /// 「谁先谁后」由**层级**决定（方案覆盖全局），不再由 `try_activate_mode` 里的
    /// 硬编码调用顺序决定——那套顺序不是数据、无从配置，且两个实例配同一个键时
    /// 后者静默失效。
    ///
    /// 键名走 `key_name_to_vk_with_letters`——本表**接受字母**，与只认符号的全局
    /// `trigger_keys` 相反：字母能否借作功能键取决于「这张码表里它是不是死码」，那正是
    /// 方案级配置才能表达的判断（见 [`Self::bound_action_key_yields`]）。
    ///
    /// 再叠一层 `modifier_name_to_vk`：修饰键的键名**不在** `KEY_TABLE` 里（那是引导键的
    /// 解析口，走 keydown，修饰键在那条路上不工作），故必须显式并进来。少了这一层的表现是
    /// 「转发集里有这个键、TSF 也发了 keyup，但查表查不到、什么都不发生」——已在测试里
    /// 复现过一次。
    pub(crate) fn bound_action_for(&self, key_code: u32) -> Option<BoundAction> {
        self.bound_action_with_source(key_code).map(|(a, _)| a)
    }

    /// **指定方案**的 `[key_actions]` 里这个键绑了什么（只查方案层，不回落全局）。
    ///
    /// ⚠️ 与 [`Self::bound_action_for`] 的分工是**动词类别**，不是「另一种取表方式」，
    /// 见 docs/design/key-resolver-unification.md §4.4：
    ///
    /// - 「从这个输入环境去哪」（`special:*` / `temp_pinyin` / `switch_schema`…）恒走
    ///   `bound_action_for`（主方案 → 全局 → `z_key_action` 三层链）；
    /// - 「解释用户敲的码」（辅助码触发键、码元、分隔符）在 overlay 里按**产出候选的
    ///   方案**取，走本函数。
    ///
    /// **刻意不回落全局**：全局层那份已经由 `bound_action_for` 那条链覆盖了，在这里再回落
    /// 一次等于同一条配置在同一个按键上被查两遍，且两遍的优先级无从定义。本函数只回答
    /// 「目标方案**自己**声明了什么」。
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

    /// 同 [`Self::bound_action_for`]，但一并给出**这条绑定来自哪一层**
    /// （`true` = 方案级 `[key_actions]`，`false` = 全局 `keys.key_actions` / `z_key_action`）。
    ///
    /// 层级信息是 `code_char_takes_lead` 仲裁的必需输入，不是锦上添花：
    ///
    /// - **全局**引导键 × 方案的 `leading_chars` 是**跨层**冲突 ⇒ 让位给码表。全局配置
    ///   无从知道某个方案把这个符号当码元用了。
    /// - **方案级**绑定 × 同方案的 `leading_chars` 是**同层**冲突 ⇒ 绑定优先。两条声明
    ///   都出自这个方案，显式绑定比从字符集隐式推导更具体。
    ///
    /// 见 docs/design/schema-key-actions.md §4.3。合并两层来源时若丢掉这个区分，
    /// 全局引导键就会变成「绑定优先」，把方案自己的码元抢走。
    pub(crate) fn bound_action_with_source(&self, key_code: u32) -> Option<(BoundAction, bool)> {
        self.bound_action_with_source_layered(key_code, true)
    }

    /// 同上，但可**跳过方案级层**（`use_schema_layer = false` ⇒ 只认全局 `keys.key_actions`）。
    ///
    /// # ★★ 为什么英文半角态要跳过方案级层
    ///
    /// 英文半角态下**当前方案整体已经不参与输入行为**——码元集、标点、候选、引擎全都不
    /// 工作（`handle_key_event` 在英文分水岭处直接 `PassThrough`）。那么「这个方案的按键
    /// 表」也就没有理由继续生效：用户此刻不在任何方案的输入语境里，他期望的是**全局配置**。
    ///
    /// 现场（2026-08-30 用户报障）：英文方案里方案级配了 `lshift = toggle_mode`，全局配了
    /// `lshift = switch_schema:wubi86`。进系统英文后再按左 Shift，用户期望走全局那条切回
    /// 五笔，实际却仍命中英文方案那条、只把 `chinese_mode` 翻回来 ⇒ 看起来像「切回了英文
    /// 方案」（方案其实从未变过，见 project_shift_schema_mode_switch）。
    ///
    /// ⚠️ **这不是新增一张表**，是三层链在英文态少查一层——配置面零增长。
    ///
    /// # ⚠️ 只有修饰键会走到这里
    ///
    /// 英文态下有字符的键在分水岭前就 `PassThrough`，组合键走的 `compiled_hotkeys` 本就是
    /// 全局编译、不分方案。故本参数实际只作用于 `lshift`/`rshift`/`lctrl`/`rctrl`。
    ///
    /// # ⚠️ 可达性并集**不得**跟着这个维度走
    ///
    /// 推给 C++ 的转发键集必须是所有维度所有取值的并集（`reachability()` /
    /// `all_key_action_keys()` 照常枚举两层）。按当前态裁剪的话，每切一次中英文就要重推
    /// 一次，漏一次就是「切完中英文这个键不灵」——与按活跃方案裁剪同型，见
    /// docs/design/key-resolver-unification.md §4.2。
    ///
    /// # ⚠️ 代价：对称配置负担
    ///
    /// 全局一旦配了切方案类动词，**没有方案级同键绑定的方案**在中文态下按这个键就会命中
    /// 全局那条（`handle_bound_modifier_key_up` 先于 `is_toggle_mode_keycode`）。用户需要
    /// 在每个方案里都配一遍 `lshift = "toggle_mode"`。这是本设计已知且已被接受的代价
    /// （2026-08-30 用户拍板），设置页在方案级面板给提示。
    fn bound_action_with_source_layered(
        &self,
        key_code: u32,
        use_schema_layer: bool,
    ) -> Option<(BoundAction, bool)> {
        // 键名→VK 走与全局层**同一个**解析口（`key_action_name_to_vk`）：两层各留一份解析
        // 规则，就是「同一张表按维度分裂」。同类**现存**缺陷有一个未修的样本：
        // `hotkey_action_entry` 的动词白名单只认 3 个，而单键那条路的 `BoundAction` 值域
        // 更大 ⇒ `ctrl+alt+e = "temp_english"` 静默失效、`z = "temp_english"` 正常。
        // 见 docs/design/key-resolver-unification.md §2.5。
        if use_schema_layer {
            for (name, action) in self.engine_mgr.active_key_actions().iter() {
                if crate::key_resolver::key_action_name_to_vk(name) == Some(key_code) {
                    return Some((BoundAction::parse(action), true));
                }
            }
        }
        // 全局 `keys.key_actions` 的单键条目。方案没表态时才落到这里——方案覆盖全局是
        // 本设计的基本层级，与码表行为、注释模板等其它方案级配置同构。
        //
        // 只认单键：组合键条目由热键通路消费（`Compiler::compile` 已按形态分流），
        // 在这里再认一次就是同一个键两条路都触发。该过滤连同键名→VK、动词→`BoundAction`
        // 的解析，都已在 `ConfigBundle::build` 的 `KeyResolver` 里做完——**本函数在按键
        // 热路径上**，原先每键都要线性遍历整张表并逐条做字符串解析。
        if let Some(action) = self.rt().key_resolver.global_lead(key_code) {
            // 查得到就是全局层**表了态**，`none` 同样是表态：语义与方案级同，
            // 不再往下回落到 `z_key_action`。故 `BoundAction::None` 也原样返回，
            // 不能在这里过滤掉（`parse` 把空 id 与未知动词一并归为 `None`，
            // 与折算前 `is_enabled()` 分支的结果逐值相同）。
            return Some((action, false));
        }
        // 表里没写 z 时，回落到专用字段（其自身已含全局→方案的折叠）。
        // 跳过方案级层时一并跳过：`z_key_action` 本身就是方案级配置。实际到不了——z 是
        // 字母键、英文态下在分水岭就 `PassThrough` 了——判据仍写出来，免得日后有人把
        // 本函数用在别的态上时，这一层成为唯一漏网的方案级来源。
        if use_schema_layer && key_code == keymap::VK_Z {
            let z = self.z_key_action();
            if z.is_enabled() {
                // z_key_action 本身是方案级配置（经 codetable_settings 折叠），
                // 故按方案级来源计——它与 leading_chars 同样是同层冲突。
                return Some((z, true));
            }
        }
        None
    }

    /// 这个键在**会话态**（有编码或候选）绑了什么动作；未绑定返回 `None`。
    ///
    /// 两层，**逐键合并**（与 `[key_actions]` 一致）：
    /// 1. 方案文件 / `schema_overrides` 的 `[session_actions]`
    /// 2. 全局 `keys.session_actions`（已含 `page_keys` 等四组键组配置的展开结果）
    ///
    /// ★ 方案层查得到就是**表了态**，显式 `"none"` 同样是表态 ⇒ 返回 `None` 且
    /// **不再回落全局**。靠「从 override 里删掉那一行」是禁不掉全局绑定的：`merge_toml`
    /// 只能新增/覆盖。语义与 [`Self::bound_action_with_source`] 的全局 `none` 逐条对应。
    ///
    /// ⚠️ **本方法是「当前方案下这个键干什么」，不是「这个键要不要转发」**。后者必须取
    /// 所有方案的并集（[`crate::config_bundle::schema_bound_modifier_vks`] 那条理由），
    /// 且并集的消费者另有其人——`capslock_bound` 决定装不装全局钩子，按活跃方案取值会让
    /// 切方案反复装卸钩子。别拿本方法去回答可达性问题。
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

    /// 绑了动作的键是否**让位**给正常输入（此时既不进模式、也不落全局引导键链）。
    ///
    /// 两条判据，都只对**字母键**成立——符号键在码表里不产出编码，按下只可能是为了触发功能：
    ///
    /// - **活码前缀**：本方案的码表/短语里有以该字母开头的条目（如自定义 `zhang`）。不让位的话
    ///   那个字母在这个方案里就彻底打不出编码了，且毫无提示。这条原是 z 专有的裁决
    ///   （对齐 Go `judgeZFirstTrigger`），随 `[key_actions]` 泛化到任意字母。
    /// - **z 的 repeat 身份**：`z_key_repeat` 开且有上屏历史时 z 归重复输入。这条**仍是 z 专有**
    ///   ——repeat 功能本身就绑死在 z 上，不是通用概念。
    ///
    /// 字母键额外限定码表引擎：拼音/混输里字母全是有效输入，借作功能键会丢首字母
    /// （与 `try_z_fallback` 的门禁同源）。符号键不限引擎——拼音方案里用 `\` 进快符同样合理。
    /// 同上，但返回**让位原因**（`None` = 不让位）供日志说明。
    ///
    /// 「配了不生效」是这套机制最常见的求助形态，而它有五个成因（没绑上 / 显式 none /
    /// 非码表引擎 / repeat / 活码前缀），单看现象完全同形。原因字符串直接进 debug 日志，
    /// 排查时一眼可辨，不必再逐个假设去试。
    fn bound_action_yield_reason(
        &self,
        key_code: u32,
        action: &BoundAction,
    ) -> Option<&'static str> {
        if !action.is_enabled() {
            return Some("显式 none");
        }
        let ch = keymap::vk_to_prefix_char_with_letters(key_code)?;
        if !ch.is_ascii_alphabetic() {
            return None; // 符号键：不让位，也不限引擎
        }
        if !matches!(
            self.engine_mgr.current_engine_type(),
            Some(wind_engine::EngineType::CodeTable)
        ) {
            return Some("字母键仅码表引擎生效（拼音/混输里字母全是有效输入）");
        }
        // z 的 repeat 身份**只压得住有夺取回路的目标**。
        //
        // `temp_pinyin` 有 `try_z_fallback`：首键让位给 repeat 之后，用户继续打字母、`z…`
        // 破了活码前缀时仍会被夺取进临拼——两个功能真正共存，让位只维持一个按键。
        //
        // 其余目标（special / mix / 临英）**只支持首键进入**，没有夺取回路。让位一次就是
        // 这个方案里再也进不去，尤其快符那种 `show_all_on_enter` 的模式——它的全部价值
        // 就在首键那一下「进入即列出符号表」，被 repeat 抢掉等于功能不存在。
        //
        // 判据落在「目标模式有没有补救通路」，不是「谁更重要」：前者可验证，后者会随人而变。
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
    /// **各目标模式的可用性门卫都在这里**，与引导键进入点用的是同一套判据（临拼的
    /// `temp_pinyin_target`、mix 的成员非空、特殊模式的 `ensure_schema`）。门卫没过返回
    /// `None`，调用方让 z 落普通输入作正常码——绝不能吞键，否则配了个不可用的目标就等于
    /// 把 z 这个编码键废掉，且用户完全看不出原因。
    pub(crate) fn enter_bound_action(
        &self,
        state: &mut State,
        action: &BoundAction,
        key_code: u32,
    ) -> Option<KeyAction> {
        match action {
            BoundAction::None => None,
            // 软键盘不是「模式」，没有编码缓冲也不进 ModeKind——直接开关面板即可。
            // 状态推送由按键路径顶层的 SoftKeyboardPushOnDrop 兜底。
            BoundAction::SoftKeyboard(page) => Some(self.toggle_softkeyboard(page.as_deref())),
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
            // 生僻字模式：用的就是当前活跃方案，无需 `ensure_schema` 门卫——那个方案此刻
            // 正在被用来打字，必然已加载。
            BoundAction::RareChar => {
                debug!("key_action: entering rare-char mode");
                Some(self.enter_rare_char_mode(state, key_code))
            }
            // C 类**不在这里执行**，见 [`Self::run_toggle_schema_action`]。
            //
            // 本函数的契约是「调用方持 `State` 锁」，而 `toggle_schema_by_id` 要走
            // `finish_user_schema_switch`，那里自己 `self.state.lock()` —— 在这里调就是死锁。
            //
            // 这条锁约束与 §4.1 的插入点判据**独立地指向同一结论**：C 类必须在英文模式下
            // 也生效（否则切到英文方案就回不来），而本函数在分水岭之后的 keydown 路径上，
            // 英文态根本走不到。故 C 类只在无字符键（修饰键 keyup）上可用。
            BoundAction::ToggleSchema(id) => {
                warn!("key_actions: toggle_schema:{id} 只能绑修饰键（无字符键），此处忽略");
                None
            }
            BoundAction::SwitchSchema(id) => {
                warn!("key_actions: switch_schema:{id} 只能绑修饰键（无字符键），此处忽略");
                None
            }
            // A 类同样不在这里执行：`dispatch_hotkey` 自加锁，本函数持锁。
            // keydown 走 `bound_lock_free_action_for_keydown`（判定后 drop 锁），
            // keyup 走 `handle_bound_modifier_key_up`，两条都在锁外。
            BoundAction::Action(_) => None,
        }
    }

    /// 执行 C 类 `toggle_schema`。**必须在不持 `State` 锁时调用**——内部经
    /// `finish_user_schema_switch` 自行加锁，见 [`Self::enter_bound_action`] 里的说明。
    ///
    /// 目标加载不了时返回 `None` 不吞键：与各模式门卫同策略，配了个不可用的目标不该
    /// 把这个键废掉，且用户看不出原因。
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
    /// 与往返版的唯一差别是不记来源、不认回程键。**方案级 `[key_actions]` 里出现本动词
    /// 时不会走到这里**——`bound_action_yield_reason` 已让位并 warn，理由见
    /// [`BoundAction::SwitchSchema`]：方案级按活跃方案查表，单向切走后目标方案没有这条
    /// 绑定，键就再也按不动了。
    fn run_switch_schema_action(&self, id: &str, trigger_vk: u32) -> Option<KeyAction> {
        if !self.engine_mgr.ensure_schema(id) {
            warn!("key_actions: switch_schema 目标 {id} 加载失败，不动作");
            return None;
        }
        debug!("key_actions: switch_schema -> {id}");
        let commit = self.switch_schema_by_id(id);
        // 记下「这把键刚把用户单向送到了这里」，供目标方案里再按时吞键（见
        // `Coordinator::schema_switch_arrival`）。**不是**回程记录——单向没有回程。
        //
        // 切换失败时不记：`switch_schema_by_id` 的加载失败分支会让 active 保持原样，
        // 记了等于宣称「这把键把你送到了当前方案」，而它其实哪也没去（与
        // `toggle_schema_by_id` 只在 active 确实变成目标后才 `record_schema_toggle_origin`
        // 同一条判据）。
        //
        // 全局配法下这条记录**查不到**也无害：目标方案里该键仍命中全局表走 `Act` 分支，
        // 根本到不了消费它的 `NotBound`。故此处不必区分绑定来自哪一层。
        if trigger_vk != 0 && self.engine_mgr.active_schema_id() == id {
            *self
                .schema_switch_arrival
                .lock()
                .unwrap_or_else(|e| e.into_inner()) =
                Some((trigger_vk, self.engine_mgr.schema_generation()));
        }
        Some(self.schema_switch_key_action(commit))
    }

    /// 这把键是不是**刚刚把用户单向送到当前方案**的那一把（见
    /// [`Coordinator::schema_switch_arrival`]）。
    ///
    /// 与 [`Self::schema_toggle_key_authorized`] 是**两件事**，不可合并：那个回答「这把键
    /// 能不能执行往返」，本函数回答「这把键该不该被安静吃掉」。合并的话，单向绑定会获得
    /// 它从未声明过的回程语义——那正是用户明确不想要的「切过去还会弹回来」。
    ///
    /// 代际不等 = 期间用别的方式切过方案，记录作废，该键恢复原本语义。
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

    /// 执行 A 类状态切换，转交 `dispatch_hotkey`（这批动作的既有单点）。
    ///
    /// **必须在不持 `State` 锁时调用**——`dispatch_hotkey` 的每个分支都自己 `state.lock()`。
    /// 与 C 类同一约束，故两者共用 [`Self::run_lock_free_bound_action`] 这个分流口。
    ///
    /// 分发端不认的动词返回 `None` 不吞键：白名单已在 `BoundAction::parse` 拦过一道，
    /// 走到这里还失败说明两处不同步，此时让键落回正常输入比静默吃掉好查。
    fn run_dispatch_action(&self, action: &str) -> Option<KeyAction> {
        // `_keyed`：本函数在按键路径上，`toggle_mode` / `switch_engine` 的编码要经
        // 本次按键应答回给宿主，不能走 `dispatch_hotkey` 的 push 出口（见其文档）。
        let Some(act) = self.dispatch_hotkey_keyed(action) else {
            warn!("key_actions: 动作 {action} 未被 dispatch_hotkey 接受，不动作");
            return None;
        };
        debug!("key_actions: dispatch {action}");
        Some(act)
    }

    /// A/C 两类「不建 overlay、只改全局状态」的动作的统一分流口。
    ///
    /// 它们的共同点不是语义而是**调用约束**：目标函数（`toggle_schema_by_id` /
    /// `dispatch_hotkey`）都自己加 `State` 锁，故一律要在锁外执行。B 类相反——它建
    /// overlay，需要 `&mut State`。这条线就是 keydown 路径上两个插入点的分界。
    ///
    /// 返回 `None` 表示「不是这两类」，调用方继续走原有链路。
    /// 该动作是否属于「锁外执行」那一类（A/C）。与
    /// [`Self::run_lock_free_bound_action`] 的 match 臂同源——分成两个函数是因为
    /// keyup 路径要**先判断再决定取不取锁**，而不是拿到结果才知道。
    pub(crate) fn is_lock_free_bound(&self, action: &BoundAction) -> bool {
        matches!(
            action,
            BoundAction::ToggleSchema(_) | BoundAction::SwitchSchema(_) | BoundAction::Action(_)
        )
    }

    pub(crate) fn run_lock_free_bound_action(
        &self,
        action: &BoundAction,
        trigger_vk: u32,
    ) -> Option<KeyAction> {
        match action {
            BoundAction::ToggleSchema(id) => self.run_toggle_schema_action(id, trigger_vk),
            BoundAction::SwitchSchema(id) => self.run_switch_schema_action(id, trigger_vk),
            BoundAction::Action(a) => self.run_dispatch_action(a),
            _ => None,
        }
    }

    /// keydown 路径上的 A 类分派判定：**判定在锁内、执行在锁外**。
    ///
    /// 调用方拿到 `Some` 后须先 `drop` 掉 `State` guard 再执行（见
    /// [`Self::run_lock_free_bound_action`] 的锁约束）。本函数只读 `state`，不改。
    ///
    /// 三道门：
    /// - **空缓冲**：打字打到一半按下绑定键，意图多半是输入而非切状态；且 A 类不吞
    ///   已有编码，留给下游的顶字逻辑更合理。
    /// - **无修饰键**：`Ctrl+\` 是宿主的快捷键，不该被方案绑定截走。
    /// - **不限修饰键的动作**：`toggle_mode` 那类绑在有字符的键上是单程票，
    ///   见 [`BoundAction::requires_modifier_key`]。
    pub(crate) fn bound_lock_free_action_for_keydown(
        &self,
        state: &State,
        data: &KeyEventData,
    ) -> Option<BoundAction> {
        if !state.input_buffer.is_empty() || data.modifiers & (MOD_SHORTCUT | MOD_SHIFT) != 0 {
            return None;
        }
        let BoundKeyDecision::Act(action) = self.bound_key_decision(data.key_code) else {
            return None;
        };
        if action.requires_modifier_key() {
            // 有字符的键到不了英文态，绑这类动作等于单程票。core 侧忽略并 warn，
            // 设置页对同一组合给行内提示。
            warn!(
                "key_actions: {action:?} 只能绑修饰键（无字符键），键 0x{:02X} 上忽略",
                data.key_code
            );
            return None;
        }
        matches!(action, BoundAction::Action(_)).then_some(action)
    }

    /// 纯修饰键 keyup 上的方案级绑定分派（`rshift = "toggle_schema:english"` 这类）。
    ///
    /// 与 keydown 侧的 `try_activate_mode` 是**互补的两半**，不是重复：修饰键没有字符，
    /// 到不了 keydown 那条链（TSF 只在干净单击后于 keyup 转发，见 `KeyEventSink.cpp`）。
    /// 判据是键的形态（有无字符），不是动词类别——见 schema-key-actions.md §4.1。
    ///
    /// 返回 `None` 表示本函数不接管，调用方继续走 `is_toggle_mode_keycode`。
    pub(crate) fn handle_bound_modifier_key_up(&self, key_code: u32) -> Option<KeyAction> {
        // ★★ 英文半角态跳过方案级层，只认全局配置——理由与代价见
        // `bound_action_with_source_layered`。**本函数是这条规则的唯一落点**：英文态下
        // 有字符的键到不了分派（分水岭 `PassThrough`），组合键走全局编译的 hotkeys，
        // 故只有修饰键这条路需要按态分层。
        //
        // ⚠️ 在此读 `chinese_mode` 是安全的：本函数由 message_handler 的 keyup 分支直接
        // 调用，**调用点不持 `State` 锁**（上游 `handle_select_key_up` /
        // `handle_session_action_key_up` 都是各自 lock 各自释放）。若日后有持锁的调用方
        // 接进来，必须改为由调用方传入该值，否则死锁。
        let chinese = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.chinese_mode
        };
        match self.bound_key_decision_layered(key_code, chinese) {
            // 活跃方案没绑这个键，但它可能**携带往返语义**——刚才正是用它把用户带到当前
            // 方案的。少了这一条，「五笔按 RShift 去英文方案」就要求英文方案自己也配一遍
            // 才回得来。见 `schema_toggle_key_authorized`。
            //
            // ★ 授权成立后一律转交 `toggle_schema_by_id`，不在这里自己判「回程还是去程」：
            //   授权只保证代际未变（⇒ 必然仍在目标方案），至于该回来源还是该重新落地，由
            //   它按完整落点裁决——**与全局 `keys.key_actions` 配置走的是同一条路**。曾经
            //   这里有一份只看代际的独立回程实现（`run_schema_return`），判据一分叉，
            //   同一个 bug 就只在其中一种配置下复现，报障时表现为「换个配法就不灵」。
            BoundKeyDecision::NotBound => {
                if self.schema_toggle_key_authorized(key_code) {
                    let commit =
                        self.toggle_schema_by_id(&self.engine_mgr.active_schema_id(), key_code);
                    return Some(self.schema_switch_key_action(commit));
                }
                // 方案级**单向**切换刚把用户送到这里：吞键、不动作。
                //
                // ★ 绝不能返回 `None` 落回全局链——`lshift`/`rshift` 出厂就是 `toggle_mode`
                // 键，那样「配的是切方案」会变成「切了中英文」，比没反应难查得多。方案级
                // 单向曾因此被整条禁掉（见 `bound_key_decision` 的注释），现由本分支兜底。
                //
                // ★ 与上面那条的区别是**吞键 vs 回程**：单向没有回程，用户要的就是「切过去
                // 就完事」。回程由他自己安排的另一把键负责。
                if self.schema_switch_key_arrived(key_code) {
                    debug!("switch_schema: 0x{key_code:02X} 是本方案的单向送达键，吞键不动作");
                    return Some(KeyAction::Consumed);
                }
                None
            }
            // 显式 `none`：屏蔽该键的全局绑定（多半是 `toggle_mode`）。必须**接管**并
            // 返回 Consumed，落到下面就等于 `none` 没生效——这正是 `;` 那次漏接的形态。
            BoundKeyDecision::Yield => {
                debug!("bound modifier key_up 0x{key_code:02X} 让位（多为显式 none）");
                Some(KeyAction::Consumed)
            }
            // A/C 类必须在**锁外**执行（目标函数自己加锁），故先于取锁分流。
            BoundKeyDecision::Act(action) if self.is_lock_free_bound(&action) => {
                self.run_lock_free_bound_action(&action, key_code)
            }
            BoundKeyDecision::Act(action) => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let act = self.enter_bound_action(&mut state, &action, key_code);
                drop(state);
                // 门卫没过（目标模式不可用）时**不吞键**：返回 None 让全局链接手，
                // 与 keydown 侧 `enter_bound_action` 返回 None 的处置一致。
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
        state.rewind = None;
        state.special_buffer.clear();
        state.special_cursor = 0;
        // `[overlay]` 段快照随模式一并丢弃。消费点都先判 `active == Special`，残留本不会
        // 被读到——但那条「先判 active」是消费点的实现细节，不是这里可以依赖的契约。
        state.overlay_spec = None;
        state.mix_buffer.clear();
        state.mix_cursor = 0;
        state.aux_code = None;
        // 清理可能残留的组合显示（临时拼音/快捷输入会产生候选与 preedit）
        state.input_buffer.clear();
        state.input_buffer_cased.clear();
        state.input_cursor_pos = 0;
        state.candidates.clear();
        state.preedit.clear();
        // 拼音逐步转换的已转换前缀一并丢弃（焦点/模式切换不保留半成品组合）。
        state.committed_text.clear();
        state.committed_segs.clear();
        // 焦点/模式切换：解除智能符号待命，避免跨上下文误触发替换。
        self.disarm_smart_symbol();
        // 快捷加词模式遗留：焦点/模式切换时退出。
        // 布局无需在此恢复——模式标志已清，下一次候选显示会自动算回全局基线（见 layout.rs）。
        // 这正是声明式重算相对「保存/恢复」的价值：这条路径当年就是补丁式加上的第 3、第 4 个
        // 恢复出口，再加四个模式就会有十几处，漏一处即候选窗卡在竖排且无日志。
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
