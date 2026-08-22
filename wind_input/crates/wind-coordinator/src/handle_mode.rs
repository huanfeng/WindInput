//! 模式 / 方案 / 主题切换
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。
//! 简繁、方案切换、主题切换、mix 融合模式、引擎方案叠加。

use crate::coordinator::{Coordinator, State};
use crate::pipeline::ModeKind;
use crate::preedit_cursor;
use crate::theme_style::ThemeStyle;
use tracing::{debug, info, warn};
use wind_bridge::handler::KeyAction;
use wind_config::BoundAction;
use wind_config::Config;
use wind_ui_types::UiCommand;

use crate::coordinator::{numpad_char, printable_char, punct_char};
use wind_bridge::handler::KeyEventData;

/// 兜底主题 id：`config.ui.theme.name` 未设置时的初值，也是 [`Coordinator::push_theme`]
/// 加载失败时的降级目标。两处必须同名，故只此一处定义。
pub(crate) const FALLBACK_THEME: &str = "default";

use wind_candidate::Candidate;
use wind_config::config::FreeInputMode;
use wind_ipc::protocol::{MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;

/// mix 模式的**输入透镜**：同一个融合模式里，按键语义由当前缓冲内容决定。
///
/// - [`Text`](MixLens::Text)：首字符是字母 —— 字母入缓冲、数字选词、`-`/`=` 翻页
///   （拼音 / 英文 / 码表成员）
/// - [`Numeric`](MixLens::Numeric)：首字符是数字或符号 —— 数字与运算符入缓冲、字母选词
///   （计算 / 日期 / 金额成员）
/// - [`Free`](MixLens::Free)：缓冲里出现了**当前透镜接受不了的字符** —— 一切可打印键
///   字面入缓冲，用来打 `GetTestData()` / `test_data` / `<TAB>` 这类内容
///
/// # 为什么是缓冲的纯函数，而不是一个状态位
///
/// `Free` 没有切换键（mix 里挑不出真正空闲的可打印键），因此也就没有可解释的
/// 状态清除时机。由缓冲推导则退格删掉越界字符即自然回到原透镜，所见即所得。
///
/// 旧实现是 `State::mix_numeric: bool` 的粘滞位（只在首字符时写一次）。一个布尔
/// 装不下三种语义，而且它同时被用来决定「候选序号标签用字母还是数字」——一个变量
/// 承担两个语义，加第三态时两边对边缘输入的期望正好相反。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MixLens {
    Text,
    Numeric,
    Free,
}

impl MixLens {
    /// 本透镜能否把 `c` 当作**编码**接受。越界即说明它不可能是编码，只能是字面内容。
    ///
    /// 这是 [`Coordinator::mix_lens`] 与按键分派的**单一真相源**：实际写进缓冲的字符
    /// 必须与本函数一致，否则透镜会在两个取值之间震荡（插入 → 判越界 → 回退 → 再插入）。
    fn accepts(self, c: char) -> bool {
        match self {
            // 文本透镜的缓冲只可能由 ① 的 `VK_A..=VK_Z` 小写字母构成——拼音的手动音节
            // 分隔符走主输入路径，不进 mix 缓冲（`handle_mix_key` 里没有分隔符分支）。
            MixLens::Text => c.is_ascii_lowercase(),
            // 数字透镜 = 求值器的表达式字符集（共用 `wind_quick_input::is_expr_char`，
            // 不另写一份，否则给求值器加运算符时这里会静默落后）。
            MixLens::Numeric => wind_quick_input::is_expr_char(c),
            MixLens::Free => true,
        }
    }

    /// 候选是否**整体上屏**（而非拼音那种分步确认消费前缀）。
    /// 数字透镜的结果与自由输入的原文都没有可分段的编码。
    pub(crate) fn commits_whole(self) -> bool {
        !matches!(self, MixLens::Text)
    }
}

impl Coordinator {
    /// 当前是否处于临时拼音模式（测试/诊断用）。
    pub fn debug_in_temp_pinyin(&self) -> bool {
        matches!(
            self.state.lock().unwrap_or_else(|e| e.into_inner()).active,
            Some(ModeKind::TempPinyin)
        )
    }

    /// 设置简繁开关（测试/诊断用）。返回是否生效（数据缺失则 false）。
    pub fn debug_set_s2t(&self, on: bool) -> bool {
        if self.s2t.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
            return false;
        }
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .s2t_enabled = on;
        true
    }

    /// 当前 overlay 模式背后的方案 id —— "模式即方案" 的单一映射（M4）。
    /// 引擎驱动型模式（临拼/特殊/临英）返回 Some(scheme)；无词典模式（快捷/URL）返回 None。
    /// overlay 候选查询统一经此取方案再走 `convert_with`；M5 临时 mix 复用此映射枚举成员方案。
    ///
    /// 说明：激活「触发条件」因各模式高度异构（Shift+字母 / 无修饰触发键 / schema 查找 /
    /// 缓冲扩展夺取）保持 S4d `try_activate_mode` 的显式优先级链，不强塞统一表（避免死抽象）。
    pub(crate) fn overlay_engine_schema(&self, state: &State) -> Option<String> {
        match state.active {
            Some(ModeKind::TempPinyin) => {
                (!state.temp_pinyin_schema.is_empty()).then(|| state.temp_pinyin_schema.clone())
            }
            Some(ModeKind::Special(idx)) => self.special_schema(idx),
            Some(ModeKind::TempEnglish) => self
                .rt()
                .config
                .input
                .temp_english
                .show_candidates
                .then(|| "english".to_string()),
            _ => None,
        }
    }

    /// mix 成员占位符解析：`$primary_pinyin` → `schema.primary_pinyin`（空=全拼）。
    /// 字面方案 id 原样返回——显式写 "pinyin" 即精确要全拼，永不被替换。
    /// 关联函数（入参 primary 而非读 self.rt()）：调用方多在已持 rt() 的闭包内，避免嵌套借用。
    pub(crate) fn resolve_mix_member(member: &str, primary_pinyin: &str) -> String {
        if member != wind_config::config::MIX_MEMBER_PRIMARY_PINYIN {
            return member.to_string();
        }
        if primary_pinyin.is_empty() {
            wind_config::config::DEFAULT_PINYIN_SCHEMA.to_string()
        } else {
            primary_pinyin.to_string()
        }
    }

    /// mix 模式的成员方案 id 列表（占位符已解析，未过滤）。
    fn mix_members_resolved(&self, idx: u8) -> Vec<String> {
        let rt = self.rt();
        let primary = rt.config.schema.primary_pinyin.clone();
        rt.config
            .schema
            .mix_modes
            .get(idx as usize)
            .map(|m| {
                m.members
                    .iter()
                    .map(|s| Self::resolve_mix_member(s, &primary))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// mix 可用的**真实方案**成员（过滤空 / 不可加载 / 快捷输入内置来源）。
    ///
    /// 快捷来源（`quick_input.*`）没有 `.schema.toml`，由协调器直接产候选，故排除在外。
    /// 英文候选的开关**只看 members 有无**——旧的 `quick_input.enable_english` 旁路已废弃
    /// （它与 members 构成双真相源，且这里与 `update_mix_candidates` 各过滤一遍）。
    pub(crate) fn mix_members(&self, idx: u8) -> Vec<String> {
        self.mix_members_resolved(idx)
            .into_iter()
            .filter(|s| {
                !s.is_empty()
                    && !wind_quick_input::is_quick_member(s)
                    && self.engine_mgr.ensure_schema(s)
            })
            .collect()
    }

    /// mix 是否含**任一**快捷输入内置来源（计算/日期/数字/重复）。
    /// 用于「进入条件」与「强制竖排」——只配了重复上屏的 mix 也算快捷输入。
    pub(crate) fn mix_has_quick_input(&self, idx: u8) -> bool {
        self.rt()
            .config
            .schema
            .mix_modes
            .get(idx as usize)
            .map(|m| {
                m.members
                    .iter()
                    .any(|s| wind_quick_input::is_quick_member(s))
            })
            .unwrap_or(false)
    }

    /// mix 是否含**表达式类**来源（计算/日期/数字）——启用数字透镜：
    /// 首字符数字/符号进表达式录入、字母作选词、`-`/`=` 是运算符而非翻页键。
    ///
    /// 刻意与 [`Self::mix_has_quick_input`] 分开：`quick_input.repeat` 不录入表达式，
    /// 只配了它的 mix 若开数字透镜，数字键会变成录不进任何候选的死输入。
    pub(crate) fn mix_has_quick_numeric(&self, idx: u8) -> bool {
        self.rt()
            .config
            .schema
            .mix_modes
            .get(idx as usize)
            .map(|m| {
                m.members
                    .iter()
                    .any(|s| wind_quick_input::QuickSource::from_member(s).is_some())
            })
            .unwrap_or(false)
    }

    /// 本 mix 实例的自由输入设置（实例缺失时按 `Off`——没有实例就没有自由输入可言）。
    pub(crate) fn mix_free_input(&self, idx: u8) -> FreeInputMode {
        self.rt()
            .config
            .schema
            .mix_modes
            .get(idx as usize)
            .map(|m| m.free_input)
            .unwrap_or(FreeInputMode::Off)
    }

    /// mix 的导航分派是否把**可打印键**（`page_keys` 里的 `minus_equal` / `comma_period` /
    /// `brackets` 等键组）算作翻页键。
    ///
    /// 自由输入开启时一律否：那些键必须留给字面输入，否则 `all-in-one` 的 `-` 会被吃成
    /// 翻页。翻页职责转给 PageUp/PageDown。
    ///
    /// **单一真相源**：`handle_mix_key` 的直接调用与 `handle_candidate_nav` 的 Mix 分支
    /// 都问这里。此前两处各写一份且**取值相反**（前者硬编 `true`，后者写
    /// `!mix_has_quick_numeric`），只因后者对 mix 根本不可达才没暴露出来。
    pub(crate) fn mix_nav_include_printable(&self, idx: u8) -> bool {
        self.mix_free_input(idx) == FreeInputMode::Off
    }

    /// 本 mix 实例的自由输入是否夺取二三候选键作字面输入
    /// （`MixModeConfig::free_input_takes_select_keys`，缺省 true）。
    pub(crate) fn mix_takes_select_keys(&self, idx: u8) -> bool {
        self.rt()
            .config
            .schema
            .mix_modes
            .get(idx as usize)
            .map(|m| m.free_input_takes_select_keys)
            .unwrap_or(true)
    }

    /// 二三候选键（`keys.select_key_groups`，默认 `;` `'`）在本实例上**是否仍作选词键**。
    ///
    /// 即「自由输入没开」或「开了但配置为不夺取」。
    ///
    /// # ★ 为什么必须是单一真相源
    ///
    /// 用户报「数字输入模式下 `;` / `'` 不能选候选」，且把 `free_input` 设为 `off` 也无效。
    /// 根因是 `handle_mix_key` 第①步的数字臂 [`Self::mix_numeric_input_char`] 收的是
    /// **一切非字母可打印字符**，口径比 [`MixLens::accepts`]（`is_expr_char`：`0-9 + - * /
    /// ^ . ( )`）宽得多 —— `;` `'` 按 `accepts` 明明是越界字符，却在①被当表达式字符吞进
    /// 缓冲并 `return`，第④步的选词判定**成了不可达代码**。`free_on` 的判定在④，救不了①。
    ///
    /// 于是①的数字臂与④的选词臂必须问**同一个**谓词：①据此让开，④才够得着。两处各写
    /// 一份表达式的话，改了一处没改另一处就会退回原样，而且同样不报错、只是静默失效。
    ///
    /// **文本透镜不受影响**（①的文本臂只收字母，`;` `'` 本就落到④）；**Free 透镜同样不问
    /// 本谓词**——那个透镜下「没有任何选词键」是刻意设计（候选窗连序号标签都不画，见
    /// `coordinator.rs` 的 `hide_index`），字母数字也一并作字面，单把 `;` `'` 挑出来会
    /// 让「Free = 所见即所得」出现一个无法解释的例外。
    pub(crate) fn mix_select_keys_active(&self, idx: u8) -> bool {
        !(self.mix_free_input(idx) != FreeInputMode::Off && self.mix_takes_select_keys(idx))
    }

    /// 当前 mix 缓冲对应的输入透镜 —— **缓冲的纯函数**，见 [`MixLens`]。
    ///
    /// 判据顺序刻意如此：
    /// 1. `Always` 直接给 `Free`（专做字面输入的实例，连基线透镜都不必算）；
    /// 2. 基线透镜按**首字符**二分，与旧 `mix_numeric` 的取值完全一致（零回归）；
    /// 3. `Off` 到此为止 —— 该实例上的一切维持既有行为；
    /// 4. `Auto` 才检查越界：缓冲里只要有一个字符基线透镜接受不了，整个缓冲就只能是字面。
    pub(crate) fn mix_lens(&self, state: &State) -> MixLens {
        let free = self.mix_free_input(state.mix_id);
        if free == FreeInputMode::Always {
            return MixLens::Free;
        }
        // 首字符非字母且本 mix 含表达式类来源 → 数字透镜；其余一律文本透镜。
        // （没有表达式类成员时数字键是选词键，不该开数字透镜——见 mix_has_quick_numeric。）
        let base = match state.mix_buffer.chars().next() {
            Some(c) if !c.is_ascii_alphabetic() && self.mix_has_quick_numeric(state.mix_id) => {
                MixLens::Numeric
            }
            _ => MixLens::Text,
        };
        if free == FreeInputMode::Off {
            return base;
        }
        if state.mix_buffer.chars().any(|c| !base.accepts(c)) {
            MixLens::Free
        } else {
            base
        }
    }

    /// **本键**应按哪个透镜分派。
    ///
    /// 缓冲非空时就是 [`Self::mix_lens`]；**缓冲为空时透镜由这一键自己决定**——非字母的
    /// 可打印键（含小键盘）开数字透镜，字母开文本透镜。
    ///
    /// 这一层不能省：`mix_lens` 是缓冲的纯函数，而空缓冲里没有任何字符可供判断，它只能
    /// 给出文本透镜。若首键直接拿它去分派，`;1+2` 的 `1` 会落到文本透镜的「数字键选词」
    /// 分支被吞掉，缓冲变成 `+2`（改造过程中实测到的回归，7 个既有测试同时红）。
    /// 旧的 `mix_numeric` 正是在首字符处写一次状态位，这里把那次判定原样保留，只是不再存。
    fn mix_lens_for_key(&self, state: &State, data: &KeyEventData, shift: bool) -> MixLens {
        if !state.mix_buffer.is_empty() {
            return self.mix_lens(state);
        }
        if self.mix_free_input(state.mix_id) == FreeInputMode::Always {
            return MixLens::Free;
        }
        let is_letter = (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code);
        // 主键盘可打印字符或小键盘键均可开数字透镜，使小键盘也能录快捷输入表达式。
        if self.mix_has_quick_numeric(state.mix_id)
            && !is_letter
            && (printable_char(data.key_code, shift).is_some()
                || numpad_char(data.key_code).is_some())
        {
            MixLens::Numeric
        } else {
            MixLens::Text
        }
    }

    /// 进入 mix 模式（至少一个成员方案可加载，由激活点保证）。
    pub(crate) fn enter_mix_mode(&self, state: &mut State, idx: u8, key_code: u32) -> KeyAction {
        state.input_buffer.clear();
        state.candidates.clear();
        state.active = Some(ModeKind::Mix(idx));
        state.mix_id = idx;
        state.mix_buffer.clear();
        state.mix_cursor = 0;
        // 透镜不再有状态位：由 `mix_lens(state)` 按缓冲实时推导（清空缓冲即回到基线）。
        // 显示态前缀（进入键符号，如 ";"；经 z_key_action 进入时为 "z"）：只显示不消费，
        // 让用户看到按下的键。
        state.mix_prefix = keymap::vk_to_prefix_char_with_letters(key_code)
            .map(|c| c.to_string())
            .unwrap_or_default();
        self.update_mix_candidates(state);
        // 候选布局（本 mix 的 candidate_layout）由 notify_ui_update → sync_candidate_layout
        // 统一重算，这里不再自己保存/切换布局（见 layout.rs）。
        self.notify_ui_update(state);
        let display = state.preedit.clone();
        debug!("Entered mix mode idx={}", idx);
        KeyAction::UpdateComposition {
            text: display.clone(),
            caret_pos: display.chars().count() as u32,
        }
    }

    /// 全局引导键的「顶字 + 进模式」判定（special > mix > 临拼，对齐空缓冲时的
    /// `try_activate_mode` 顺序）。命中返回已处理的 KeyAction；都不命中返回 None。
    ///
    /// 从 `handle_key_event` 的 `decideBufferedTrigger` 臂原样抽出，供方案级
    /// `[key_actions]` 未表态（`NotBound`）时调用——表了态的键不该再走全局链。
    pub(crate) fn try_global_trigger_commit_enter(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        if let Some(idx) = self.match_special_trigger(data.key_code)
            && let Some(schema) = self.special_schema(idx)
            && self.engine_mgr.ensure_schema(&schema)
        {
            return Some(self.commit_and_enter_special_mode(state, idx, data.key_code));
        }
        // 融合「快捷」（现唯一的快捷输入形态，成员含日期/计算/拼音/英文）——对齐空缓冲
        // 时 handle_lifecycle 的 enter_mix_mode，使有无候选都进同一融合模式。
        if let Some(idx) = self.match_mix_trigger(data.key_code)
            && (self.mix_has_quick_input(idx) || !self.mix_members(idx).is_empty())
        {
            return Some(self.commit_and_enter_mix_mode(state, idx, data.key_code));
        }
        if self.is_temp_pinyin_trigger(data.key_code)
            && let Some(target) = self.engine_mgr.temp_pinyin_target()
        {
            return Some(self.commit_and_enter_temp_pinyin(state, data.key_code, target));
        }
        None
    }

    /// 方案级按键功能的「顶字 + 进模式」版本，与 [`Self::enter_bound_action`] 对应。
    ///
    /// 两者的关系 == `commit_and_enter_mix_mode` 之于 `enter_mix_mode`：一个先把已转换
    /// 前缀和高亮候选上屏再进，一个直接进。门卫完全一致，没过一律返回 None 不吞键。
    pub(crate) fn commit_and_enter_bound_action(
        &self,
        state: &mut State,
        action: &BoundAction,
        key_code: u32,
    ) -> Option<KeyAction> {
        match action {
            BoundAction::None => None,
            BoundAction::TempPinyin => {
                let target = self.engine_mgr.temp_pinyin_target()?;
                Some(self.commit_and_enter_temp_pinyin(state, key_code, target))
            }
            BoundAction::TempEnglish => {
                if !self.rt().config.input.temp_english.enabled {
                    return None;
                }
                // 临英没有专用的「顶字进入」出口，走与 mix / 特殊模式 / 临拼同一个顶屏取文本
                // 入口，再接空缓冲进入语义。
                let committed = self.take_committed_with_highlight(state);
                let enter = self.enter_bound_action(state, action, key_code)?;
                Some(match committed {
                    Some(text) => {
                        let new_comp = match &enter {
                            KeyAction::UpdateComposition { text, .. } => text.clone(),
                            _ => state.preedit.clone(),
                        };
                        self.commit_then_new_composition(text, new_comp)
                    }
                    None => enter,
                })
            }
            BoundAction::Mix(id) => {
                let idx = self.mix_mode_idx(id)?;
                if !self.mix_has_quick_input(idx) && self.mix_members(idx).is_empty() {
                    return None;
                }
                Some(self.commit_and_enter_mix_mode(state, idx, key_code))
            }
            // 辅助码刻意**不顶字**：候选列表保持原状仅筛选，进入后原地过滤。见 `enter_aux_code`。
            BoundAction::Special(id) => {
                let idx = self.special_mode_idx(id)?;
                let schema = self.special_schema(idx)?;
                if !self.engine_mgr.ensure_schema(&schema) {
                    return None;
                }
                Some(self.commit_and_enter_special_mode(state, idx, key_code))
            }
            // A/C 类不建 overlay，「顶字再进」这套对它们没有意义；且目标函数自加锁，
            // 本函数持锁。两类都在锁外的专用分派点执行，见 `enter_bound_action` 的同名分支。
            //
            // 注：走到本函数说明缓冲非空，而 A 类的 keydown 分派要求空缓冲——打字打到
            // 一半按下绑定键，意图多半是输入而非切状态。
            BoundAction::ToggleSchema(_)
            | BoundAction::SwitchSchema(_)
            | BoundAction::Action(_) => None,
        }
    }

    /// 顶屏当前高亮候选（若有）并进入 mix 融合模式。
    /// 用于缓冲非空 / 有候选时按下融合触发键（如 `;`）——对齐 `commit_and_enter_temp_pinyin`：
    /// 先把已转换前缀 + 高亮候选上屏，再进融合模式。
    /// （空缓冲 + 无候选的进入由 handle_lifecycle 的 `enter_mix_mode` 直接处理。）
    pub(crate) fn commit_and_enter_mix_mode(
        &self,
        state: &mut State,
        idx: u8,
        key_code: u32,
    ) -> KeyAction {
        // 命令候选顶屏 → 执行命令（与按空格一致），不进模式、不上屏 display 标签。
        if let Some(act) = self.top_commit_command_guard(state) {
            return act;
        }
        // 已转换前缀 + 高亮候选一并上屏（含记账与简繁转换）。
        let committed = self.take_committed_with_highlight(state);
        // enter_mix_mode 内部清空 input_buffer/candidates、建组合区前缀、刷 UI 并返回 UpdateComposition。
        let enter = self.enter_mix_mode(state, idx, key_code);
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

    /// 退出 mix 模式并清空相关状态（含逐步转换的已转换前缀）。
    /// mix：回退最后一个已转换段——把它消费的码并回缓冲**前部**并重转，光标落码末尾
    /// （理由同主输入的 `pop_committed_seg`）。Backspace（段优先）与 Delete（删空后）共用。
    fn pop_mix_seg(
        &self,
        state: &mut State,
        refresh: &dyn Fn(&Self, &mut State) -> KeyAction,
    ) -> KeyAction {
        let Some((raw_code, _, _, _, _)) = state.committed_segs.pop() else {
            return KeyAction::Consumed;
        };
        state.committed_text = state
            .committed_segs
            .iter()
            .map(|(_, _, t, _, _)| t.as_str())
            .collect();
        state.mix_buffer = format!("{}{}", raw_code, state.mix_buffer);
        state.mix_cursor = state.mix_buffer.len();
        refresh(self, state)
    }

    pub(crate) fn exit_mix_mode(&self, state: &mut State) {
        state.active = None;
        state.mix_buffer.clear();
        state.mix_cursor = 0;
        state.mix_repeat = false;
        state.mix_prefix.clear();
        state.committed_text.clear();
        state.committed_segs.clear();
        state.candidates.clear();
        state.preedit.clear();
        // 布局无需在此恢复：active 已清空，下一次 notify_ui_update 会自动算回全局基线。
    }

    /// 候选的**出口文本**（显示与上屏同源）：1对多变体候选（`s2t_override`）直接用覆盖
    /// 文本，其余按需简繁转换。凡「拿某条候选去显示/上屏」一律走本函数，勿直接
    /// `maybe_s2t(&c.text)`——否则变体候选会退化回默认转换结果（选「齣」出的却是「出」）。
    pub(crate) fn cand_s2t_text(&self, state: &State, c: &Candidate) -> String {
        match &c.s2t_override {
            Some(t) => t.clone(),
            None => self.maybe_s2t(state, &c.text),
        }
    }

    /// 若开启简繁转换，把简体文本转为繁体（数据缺失则原样返回）。
    pub(crate) fn maybe_s2t(&self, state: &State, text: &str) -> String {
        if state.s2t_enabled
            && let Some(conv) = self.s2t.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        {
            return conv.convert(text);
        }
        text.to_string()
    }

    /// 直通命令 `ime.schema("<id>")`：切到指定方案并持久化 `schema.active`。
    ///
    /// 与方案直达热键走**同一条路**（[`Self::switch_schema_by_id`]）——两者都是「指名道姓
    /// 切到这一个」，没有理由分叉出第二套行为。持久化由 `finish_user_schema_switch` 统一
    /// 负责，因而天然**只在切换成功后**才写。
    ///
    /// 这里曾经调一条自带 `is_loaded` 守卫的独立路径，且**无条件**持久化 `schema.active`，
    /// 于是未启用方案上叠了两个故障：切换被守卫拦掉只弹「准备中…」，配置却已改写。
    /// 用户侧表现为「按了没反应」，而下次重启/热重载又莫名切了过去（真机现场见
    /// [`Self::switch_schema_by_id`] 的说明）。
    pub(crate) fn cmd_set_schema(&self, id: &str) {
        self.switch_schema_by_id(id);
    }

    /// 循环切换主题并持久化；dir="prev" 向前，其余向后。返回新主题显示名。
    pub(crate) fn cmd_theme_cycle(&self, dir: &str) -> String {
        let list = self.list_themes(); // Vec<(id, name)>
        if list.is_empty() {
            return String::new();
        }
        let cur = self
            .theme_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let pos = list.iter().position(|(id, _)| *id == cur).unwrap_or(0);
        let n = list.len();
        let next = if dir == "prev" {
            (pos + n - 1) % n
        } else {
            (pos + 1) % n
        };
        self.select_theme(next);
        list[next].1.clone()
    }

    /// 选择第 N 个输入方案（托盘/菜单入口；隐含切到中文模式）。
    ///
    /// 收尾走 [`Self::finish_user_schema_switch`]，与直达热键/循环键同一套——此前这里手写
    /// 了一份，与那边的差异（无条件归位中文 vs 受 CapsLock 开关门控、不取消大写、不记
    /// per-app 模式、不清 preedit）正是「托盘切得动、热键切不动」的由来。
    pub(crate) fn select_schema(&self, index: usize) {
        let list = self.engine_mgr.available_schemas().to_vec();
        if index >= list.len() {
            return;
        }
        let id = list[index].clone();
        // 先判幂等再切：`switch_schema` 对「已是当前方案」和「加载失败」都返回 false，
        // 不分开判就只能二选一。重选当前方案仍要走收尾（用户点它多半就是想归位中文）；
        // 加载失败则必须提示且**不写盘**——否则 `schema.active` 指向一个没生效的方案，
        // 下次重启又莫名切了过去（同 `cmd_set_schema` 栽过的次生撕裂）。
        if self.engine_mgr.active_schema_id() == id || self.engine_mgr.switch_schema(&id) {
            self.finish_user_schema_switch(&id, "Selected schema");
        } else {
            let name = self.engine_mgr.schema_name(&id);
            self.show_tip(&format!(
                "{}加载失败",
                if name.is_empty() { &id } else { &name }
            ));
        }
    }

    /// 可选双拼布局 `(id, 显示名)`，扫描安装目录与用户目录的 `schemas/shuangpin/*.toml`。
    pub fn shuangpin_layouts(&self) -> Vec<(String, String)> {
        self.engine_mgr.shuangpin_layouts()
    }

    /// `shuangpin` 方案当前生效的布局 id（非双拼方案返回空串）。
    pub fn active_shuangpin_layout(&self) -> String {
        self.engine_mgr.shuangpin_layout_of("shuangpin")
    }

    /// 设置双拼布局并落盘。
    ///
    /// 落点是**方案覆盖层** `schema_overrides/shuangpin.toml` 而不是用户 config.toml：
    /// `layout` 属于方案维度（`[engine.pinyin.shuangpin]`），覆盖文件与 `.schema.toml`
    /// 用完全相同的段名，由 `read_schema` 深合并——写法与方案作者的内联写法一致，
    /// 不需要第二套键名。
    ///
    /// **读改写而非整文件覆盖**：这个文件还承载词库启停等其它方案覆盖项，
    /// 整体覆盖会把它们一起抹掉，而症状要等到用户下次发现某个词库自己开回来了才暴露。
    ///
    /// 写完必须重建引擎集：覆盖文件只在引擎构建期读，`reload_user_config` 又按
    /// `cfg.schema` 是否变化决定重不重建——改布局不动 `cfg.schema`，走那条路会
    /// 「写进去了但打字还是老布局」。
    ///
    /// @return 是否成功（布局 id 不在清单里、或落盘失败均为 false）
    pub fn set_shuangpin_layout(&self, layout_id: &str) -> bool {
        if !self
            .engine_mgr
            .shuangpin_layouts()
            .iter()
            .any(|(id, _)| id == layout_id)
        {
            warn!("set_shuangpin_layout: 未知布局 {}", layout_id);
            return false;
        }
        let Some(dir) = Config::user_config_dir().map(|d| d.join("schema_overrides")) else {
            warn!("set_shuangpin_layout: 用户配置目录不可用");
            return false;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("set_shuangpin_layout: 建目录失败: {}", e);
            return false;
        }
        let path = dir.join("shuangpin.toml");
        let mut root: toml::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.parse::<toml::Value>().ok())
            .unwrap_or_else(|| toml::Value::Table(toml::value::Table::new()));
        let mut cursor = &mut root;
        for key in ["engine", "pinyin", "shuangpin"] {
            let table = match cursor {
                toml::Value::Table(t) => t,
                _ => {
                    warn!("set_shuangpin_layout: 覆盖文件结构异常，放弃写入");
                    return false;
                }
            };
            cursor = table
                .entry(key.to_string())
                .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        }
        match cursor {
            toml::Value::Table(t) => {
                t.insert("layout".into(), toml::Value::String(layout_id.to_string()));
            }
            _ => {
                warn!("set_shuangpin_layout: [engine.pinyin.shuangpin] 不是表，放弃写入");
                return false;
            }
        }
        // ⚠ 必须走 `toml::to_string`（文档序列化），不能用 `Value::to_string()`：
        // 后者把根表输出成**内联表** `{ engine = { pinyin = … } }`，那不是合法的
        // TOML 文档，回读时解析失败 → 覆盖被静默忽略，症状是「选了没反应」，
        // 而文件明明写出来了。
        let text = match toml::to_string(&root) {
            Ok(s) => s,
            Err(e) => {
                warn!("set_shuangpin_layout: 序列化失败: {}", e);
                return false;
            }
        };
        if let Err(e) = std::fs::write(&path, text) {
            warn!("set_shuangpin_layout: 写 {} 失败: {}", path.display(), e);
            return false;
        }

        let cfg = self.rt().config.clone();
        self.engine_mgr.reload_from_config(&cfg);
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.input_buffer.clear();
            s.candidates.clear();
            s.preedit.clear();
        }
        self.notify_ui_hide();
        self.push_state_update();
        true
    }

    /// 选择第 N 个主题。
    pub(crate) fn select_theme(&self, index: usize) {
        let list = self.list_themes();
        if index >= list.len() {
            return;
        }
        let (id, name) = list[index].clone();
        *self.theme_name.lock().unwrap_or_else(|e| e.into_inner()) = id.clone();
        let dark = self.resolve_theme_dark();
        self.push_theme(&id, dark);
        self.persist_theme(&id);
        self.show_tip(&format!("主题: {}", name));
    }

    /// 当前该用暗色吗：读运行时明暗设置，`system` 交由实时探测系统明暗。
    pub(crate) fn resolve_theme_dark(&self) -> bool {
        self.theme_style
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resolve_dark()
    }

    /// 设置主题明暗（菜单协议编码：0 跟随/1 亮/2 暗），用当前主题重解析并持久化到
    /// config.ui.theme.style。
    pub(crate) fn set_theme_style(&self, style: u8) {
        let style = ThemeStyle::from_menu_id(style);
        *self.theme_style.lock().unwrap_or_else(|e| e.into_inner()) = style;
        let _ = Config::set_user_string(&["ui", "theme", "style"], style.as_config());
        let name = self
            .theme_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.push_theme(&name, style.resolve_dark());
        self.show_tip(style.label());
    }

    /// 系统「浅色/深色模式」切换的响应（UI 线程截获 WM_SETTINGCHANGE 后回送）。
    ///
    /// 仅 `system` 需要动作——显式选了亮/暗的用户不该被系统设置改写。
    pub(crate) fn on_system_theme_changed(&self) {
        let style = *self.theme_style.lock().unwrap_or_else(|e| e.into_inner());
        if style != ThemeStyle::System {
            return;
        }
        let name = self
            .theme_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let dark = style.resolve_dark();
        tracing::info!("系统明暗切换 → 重解析主题 {} (dark={})", name, dark);
        self.push_theme(&name, dark);
    }

    /// 持久化主题选择。config.ui.theme.name 为单一源（设置页/右键统一，reload 据此应用）。
    pub(crate) fn persist_theme(&self, name: &str) {
        let _ = Config::set_user_string(&["ui", "theme", "name"], name);
    }

    /// 主题搜索目录：用户主题目录（%APPDATA%\WindInput\themes，优先覆盖）+ 安装主题目录。
    /// 用户目录靠前 → 同名主题用户版覆盖内置；base 继承跨目录解析（用户主题可 `base: _base`）。
    pub(crate) fn theme_search_dirs(&self) -> Vec<std::path::PathBuf> {
        let mut dirs = Vec::new();
        if let Some(d) = Config::user_config_dir() {
            dirs.push(d.join("themes"));
        }
        if let Some(d) = &self.themes_dir {
            dirs.push(d.clone());
        }
        dirs
    }

    /// [`Self::push_theme`] 的降级内核：探测源是参数，故降级路径可被单测覆盖。
    ///
    /// 抽出来的理由同 `Config::wait_until_settled`——真机上主题目录几乎总是完好，
    /// 降级分支在开发机永远走不到，而它恰恰是这个修复的目的所在，不能靠「上真机
    /// 删个主题试试」来验证。
    ///
    /// 返回 `(实际生效的主题 id, 主题)`；两级都失败返回 `None`（调用方保留当前）。
    /// 请求的就是 [`FALLBACK_THEME`] 时不重复试第二次。
    fn load_theme_with_fallback<T>(
        mut load: impl FnMut(&str) -> anyhow::Result<T>,
        name: &str,
    ) -> Option<(String, T)> {
        match load(name) {
            Ok(t) => return Some((name.to_string(), t)),
            Err(e) => warn!("Failed to load theme {}: {}", name, e),
        }
        if name == FALLBACK_THEME {
            return None;
        }
        match load(FALLBACK_THEME) {
            Ok(t) => {
                warn!(
                    "主题 {} 不可用，本次降级为 {}（配置未改，下次仍会尝试 {}）",
                    name, FALLBACK_THEME, name
                );
                Some((FALLBACK_THEME.to_string(), t))
            }
            Err(e) => {
                warn!("降级主题 {} 同样加载失败: {}", FALLBACK_THEME, e);
                None
            }
        }
    }

    /// 加载并下发指定主题。跨用户+安装目录解析（含 base 继承）。
    ///
    /// 请求主题加载不了时降级到 [`FALLBACK_THEME`]，**不是**"保留当前"就完事：在**启动**
    /// 路径上"当前"是候选窗构造时的 `Resolved::default()`（编译期派生零值，跟磁盘上那个
    /// 叫 `default` 的主题毫无关系），一次读盘失败就把整个会话钉死在那副外观上，且只留
    /// 一行 warn，用户只看得见"主题不对"。曾见于部署期 `data\` 半截时抢跑起来的服务。
    ///
    /// 降级**不动** `theme_name`、不持久化：用户选的仍是原主题，下次 reload / 切明暗
    /// 会重新尝试。把一次读盘失败固化成配置变更，比显示不对更难查。
    ///
    /// 两级都失败才保留当前不下发——此时盘上多半整个 themes 目录都没了，硬发零值只会
    /// 把**运行中**已经好用的主题也清掉（reload 路径同样走这里），那是退化不是兜底。
    pub(crate) fn push_theme(&self, name: &str, is_dark: bool) {
        let dirs = self.theme_search_dirs();
        if dirs.is_empty() {
            return;
        }
        let loaded = Self::load_theme_with_fallback(
            |n| wind_theme::load_resolved_dirs(&dirs, n, is_dark),
            name,
        );
        let Some((used, theme)) = loaded else {
            warn!("主题目录不可用（安装数据缺失？），保留当前外观未下发");
            return;
        };
        info!("Loaded theme: {} (dark={})", used, is_dark);
        // 记录主题定义的序号槽位，供 index_label 裁决「用户 > 主题 > 默认」。
        *self
            .theme_index_labels
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = theme.views.index_labels.clone();
        let _ = self.ui_tx.send(UiCommand::SetTheme(Box::new(theme)));
    }

    /// 列出可用主题：(id, 显示名)。程序目录主题优先（按 order/id 排序），
    /// 用户目录独有主题排后（忽略 order，按 id 排序）。
    ///
    /// 唯一排序实现：右键菜单与 RPC `theme.list`（[`Self::web_theme_list`]）
    /// 均基于此结果，避免两处各自扫描目录导致顺序不一致。
    pub(crate) fn list_themes(&self) -> Vec<(String, String)> {
        self.list_themes_full()
            .into_iter()
            .map(|(id, name, _builtin)| (id, name))
            .collect()
    }

    /// [`Self::list_themes`] 的完整版本，附带是否内置（程序目录）标记。
    pub(crate) fn list_themes_full(&self) -> Vec<(String, String, bool)> {
        let all_dirs = self.theme_search_dirs();
        let user_dir = Config::user_config_dir().map(|d| d.join("themes"));

        // 扫描程序目录主题，按 (order, id) 排序
        let mut prog_rows: Vec<(String, String, i32)> = Vec::new();
        if let Some(dir) = &self.themes_dir {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.filter_map(|e| e.ok()) {
                    if !e.path().is_dir() {
                        continue;
                    }
                    let Ok(id) = e.file_name().into_string() else {
                        continue;
                    };
                    if id.starts_with('_') || !dir.join(&id).join("theme.toml").exists() {
                        continue;
                    }
                    let meta = wind_theme::read_meta(&all_dirs, &id);
                    let name = meta
                        .as_ref()
                        .map(|m| m.name.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| id.clone());
                    let order = meta.as_ref().map(|m| m.order).unwrap_or(0);
                    prog_rows.push((id, name, order));
                }
            }
            prog_rows.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        }

        let prog_ids: std::collections::HashSet<String> =
            prog_rows.iter().map(|(id, _, _)| id.clone()).collect();

        // 扫描用户目录独有主题（与程序目录不重叠），按 id 排序，忽略 order
        let mut user_rows: Vec<(String, String)> = Vec::new();
        if let Some(udir) = &user_dir {
            if let Ok(rd) = std::fs::read_dir(udir) {
                for e in rd.filter_map(|e| e.ok()) {
                    if !e.path().is_dir() {
                        continue;
                    }
                    let Ok(id) = e.file_name().into_string() else {
                        continue;
                    };
                    if id.starts_with('_') || !udir.join(&id).join("theme.toml").exists() {
                        continue;
                    }
                    if prog_ids.contains(&id) {
                        continue;
                    }
                    let meta = wind_theme::read_meta(&all_dirs, &id);
                    let name = meta
                        .as_ref()
                        .map(|m| m.name.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| id.clone());
                    user_rows.push((id, name));
                }
            }
            user_rows.sort_by(|a, b| a.0.cmp(&b.0));
        }

        let mut result: Vec<(String, String, bool)> = prog_rows
            .into_iter()
            .map(|(id, name, _order)| (id, name, true))
            .collect();
        result.extend(user_rows.into_iter().map(|(id, name)| (id, name, false)));
        result
    }

    /// 方案显示名（友好名优先，未知回退 id）
    pub(crate) fn schema_display_name(id: &str) -> String {
        match id {
            "wubi86" => "五笔".to_string(),
            "pinyin" => "拼音".to_string(),
            "shuangpin" => "双拼".to_string(),
            "wubi86_pinyin" => "五笔拼音".to_string(),
            other => other.to_string(),
        }
    }

    /// 当前激活模式的指示名 (全称, 短称)；None = 无可指示模式（普通输入/网址模式）。
    /// 临时拼音/双拼按目标方案派生（拼/双）；mix/special 取配置 name + short_name。
    pub(crate) fn mode_indicator_names(&self, state: &State) -> Option<(String, String)> {
        match state.active? {
            ModeKind::TempPinyin => {
                let disp = Self::schema_display_name(&state.temp_pinyin_schema);
                let short = disp
                    .chars()
                    .next()
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                Some((format!("临时{}", disp), short))
            }
            ModeKind::TempEnglish => Some(("临时英文".to_string(), "英".to_string())),
            ModeKind::Url => Some(("网址输入".to_string(), "网址".to_string())),
            ModeKind::Mix(i) => {
                let (full, short) = {
                    let rt = self.rt();
                    let m = rt.config.schema.mix_modes.get(i as usize)?;
                    let full = if m.name.is_empty() {
                        "快捷".to_string()
                    } else {
                        m.name.clone()
                    };
                    let short = Self::short_or_first(&m.short_name, &full);
                    (full, short)
                };
                // 自由输入必须在指示上可见：这个透镜下数字键不选词、标点不顶屏，用户若不知道
                // 自己已经切进来，「为什么按 1 没选词」就无从解释。
                if self.mix_lens(state) == MixLens::Free {
                    return Some((format!("{}·自由", full), "字".to_string()));
                }
                Some((full, short))
            }
            ModeKind::Special(i) => {
                // 实例即方案：显示名/短称直接是方案文件的 [schema] name / icon_label。
                // 原先 special_modes 条目里另有一份 name/short_name、缺省时才回落方案文件——
                // 那份重复随数组一并消失，这里不再有「两个来源取其一」的分支。
                let e = self.engine_mgr.overlay_modes().get(i as usize)?.clone();
                let short = if e.icon_label.is_empty() {
                    Self::short_or_first("", &e.name)
                } else {
                    e.icon_label
                };
                Some((e.name, short))
            }
            // 辅助码：显示码表名（如「笔画」）；表未加载或未命名 → 无指示（沿用主路径）。
            ModeKind::AuxCode => {
                let table = self
                    .aux_code_table
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                let name = table.as_ref()?.name.clone();
                if name.is_empty() {
                    None
                } else {
                    let short = Self::short_or_first("", &name);
                    Some((name, short))
                }
            }
        }
    }

    /// 短称：配置非空则用之，否则取全称首字。
    fn short_or_first(short: &str, full: &str) -> String {
        if !short.trim().is_empty() {
            short.trim().to_string()
        } else {
            full.chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_default()
        }
    }

    /// 按 ui.mode_indicator.style 解析出当前应显示的指示文本；None = 不显示。
    pub(crate) fn mode_indicator_text(&self, state: &State) -> Option<String> {
        use wind_config::ModeIndicatorStyle;
        let (full, short) = self.mode_indicator_names(state)?;
        match self.rt().config.ui.mode_indicator.parsed_style() {
            ModeIndicatorStyle::None => None,
            ModeIndicatorStyle::Full => Some(full),
            ModeIndicatorStyle::Short => Some(short),
        }
    }

    pub(crate) fn cycle_schema(&self) {
        // 「无处可去」（`available` 里没有别的方案）时**仍然收尾**，与
        // [`Self::switch_schema_by_id`] 的幂等分支同一判据：用户按的是切换键，意图是
        // 「用这个方案打字」，而英文半角态 / CapsLock 开着时需要发生的恰恰只剩收尾里
        // 那部分（归位中文、取消大写）。什么都不做就是「按了没反应」。
        //
        // 空 id 时跳过：那说明连活跃方案都没有（引擎尚未装配），此时 finish 会把
        // `schema.active` 写成空串——配置从此指向一个不存在的方案。
        let target = self
            .engine_mgr
            .cycle_schema()
            .unwrap_or_else(|| self.engine_mgr.active_schema_id());
        if target.is_empty() {
            return;
        }
        self.finish_user_schema_switch(&target, "Cycled to schema");
    }

    /// 切到指定方案。方案直达热键（`keys.schema_hotkeys`）与直通命令
    /// [`Self::cmd_set_schema`] 共用本函数。
    ///
    /// 与循环键走**同一条收尾**（[`Self::finish_user_schema_switch`]），因而持久化、状态归位、
    /// 工具栏刷新的行为逐项一致——这几个入口表达的是同一个用户意图，只是一个"下一个"、
    /// 一个"这一个"。
    ///
    /// **不要求目标方案已启用或已预热**——`engine_mgr.switch_schema` 内部是懒加载
    /// （`ensure_loaded`），不看 `schema.available`。
    ///
    /// 这里刻意**没有** `is_loaded` 守卫。曾有一条独立的切换路径带着它，防的是「在 IME
    /// 线程同步重熔大词库」的卡顿，但预热只覆盖 available 里的方案，对未启用方案
    /// `is_loaded` 恒为假 —— 于是那条路径的结局只能是弹「准备中…」然后什么都不发生，
    /// 永远。真机现场：用直通命令 `ime.schema` 切一个未启用的方案，**每次重启后的第一次
    /// 必失败**，只弹「XX准备中…」；而该方案的词库缓存其实齐备，加载不过几百毫秒。
    /// 用户按下的是**指名道姓**的热键/命令，同步加载一次（之后就缓存住了）比彻底切不
    /// 过去合理。这也正是「英文方案不必先启用就能用热键切过去」的实现方式。
    ///
    /// 守卫连同那条路径已删除；若日后要再加防卡顿闸门，判据应是
    /// `EngineManager::is_building`（正在建才叫「准备中」），而不是 `is_loaded`——
    /// 「没在建、也没加载」被谎报成「准备中」，正是上面那个现场里用户被误导的原因。
    ///
    /// 先判幂等再切，是为了让失败提示说得准：`engine_mgr.switch_schema` 对「已是当前方案」
    /// 和「方案加载失败」都返回 false，不分开判就只能二选一——要么给正常的重复按键弹一个
    /// 假报错，要么让方案文件损坏时按键毫无反应。
    ///
    /// ★★ 但**幂等 ≠ 什么都不做**：已是目标方案时仍要走收尾。
    ///
    /// 这个键的语义是「我要用这个方案打字」，不只是「把 active 改成它」。英文半角态或
    /// CapsLock 开着时，方案本来就对——需要发生的恰恰只有 `finish_user_schema_switch`
    /// 里那部分：归位中文、取消大写。早退回去等于「在最该生效的场景下按键毫无反应」，
    /// 而这正是 2026-08-21 真机报障的第二次现场（第一次是那段归位被 CapsLock 开关门控，
    /// 见 `finish_user_schema_switch`；修完那处却漏了这条早退，于是同一个症状复发）。
    ///
    /// 判据：**修好一个动作的收尾之后，要回头看它有没有「提前返回」的分支绕过那段收尾。**
    /// 托盘 `select_schema` 当时同批改对了（`active == id || switch_schema(..)`），这里没有，
    /// 于是又成了一处入口漂移——两处现已同构。
    pub(crate) fn switch_schema_by_id(&self, schema_id: &str) {
        if self.engine_mgr.active_schema_id() == schema_id {
            self.restore_state_for_same_schema();
            return;
        }
        if self.engine_mgr.switch_schema(schema_id) {
            self.finish_user_schema_switch(schema_id, "Switched to schema");
        } else {
            let name = self.engine_mgr.schema_name(schema_id);
            self.show_tip(&format!(
                "{}加载失败",
                if name.is_empty() { schema_id } else { &name }
            ));
        }
    }

    /// 方案往返热键（`keys.key_actions` 的 `toggle_schema:<id>`）：切到目标方案，
    /// **再按一次回到来源方案**。
    ///
    /// 与 [`Self::switch_schema_by_id`] 的唯一区别就是这个回程。之所以不把回程直接做进
    /// 那个函数（让所有方案热键都变往返），是因为它同时服务菜单/工具栏/RPC 等入口，
    /// 那些地方「切过去就是切过去」，凭空多出个回程反而不可预期。
    ///
    /// # 回程为什么记来源、而不是配置里写死目标
    ///
    /// 写死目标的话，从别的方案按进来时回程会把用户送到一个他没待过的方案（拼音 → 英文
    /// → 五笔）。记来源则 `拼音→英文→拼音`、`五笔→英文→五笔` 都成立（见
    /// docs/design/schema-key-actions.md §5）。
    ///
    /// `trigger_vk` = 触发本次切换的键（0 = 全局热键等非方案级绑定）。记下它，回程才
    /// **真正**不依赖目标方案的配置——见 [`Self::schema_return_key_action`]。
    pub(crate) fn toggle_schema_by_id(&self, schema_id: &str, trigger_vk: u32) {
        let current = self.engine_mgr.active_schema_id();
        if current == schema_id {
            // 已在目标方案：回来源。take 而非 clone——回程用掉这一次记录，
            // 连按第三次不该又弹回去（那时来源已由下面的切换重新写入）。
            let origin = self
                .schema_toggle_origin
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            match origin {
                // 代际相等 = 自记录以来无人动过活跃方案，这条来源仍代表用户的往返意图。
                Some((origin, generation, _))
                    if generation == self.engine_mgr.schema_generation() =>
                {
                    self.switch_schema_by_id(&origin)
                }
                // 无来源（刚启动）或来源已失效（期间用别的方式切过方案）：**no-op**。
                // 不切走是刻意的——此时没有任何依据说明用户想去哪，随便挑一个（如循环到
                // 下一个方案）会把「往返键」变成「随机跳转键」。
                _ => debug!("toggle_schema: 已在 {schema_id} 且无有效来源，不动作"),
            }
            return;
        }
        self.switch_schema_by_id(schema_id);
        // 切换失败（方案加载不了）时 active 未变，不该记来源——否则下次按会把用户
        // 送去一个他从未离开过的地方。
        if self.engine_mgr.active_schema_id() == schema_id {
            // 代际取**切换之后**的值：这条记录的有效期从现在开始。
            let generation = self.engine_mgr.schema_generation();
            *self
                .schema_toggle_origin
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some((current, generation, trigger_vk));
        }
    }

    /// 该键此刻是否是「回程键」——即刚才正是用它把用户带到当前方案的。
    ///
    /// # 为什么需要它
    ///
    /// 方案级 `[key_actions]` 按**活跃方案**查表。去程后活跃方案已经是目标方案了，若目标
    /// 方案没配同一个键，查表落空 ⇒ 按不动 ⇒ 回不来。于是「五笔按 RShift 进英文方案」
    /// 变成要求英文方案自己也配一遍 RShift——那正是 §5 想消除的对称配置负担，只是从
    /// 「N² 个定向绑定」降到了「每个方案配同一个键」，并没有消除。
    ///
    /// 有了触发键记录，去程后该键在目标方案里**临时**获得回程语义，有效期与来源记录
    /// 同寿（代际一变即失效）。这才兑现「回程不依赖目标方案的配置」。
    ///
    /// `vk == 0` 的记录（全局热键触发）不参与：那类键本来就在所有方案里都生效。
    pub(crate) fn schema_return_key_action(&self, key_code: u32) -> bool {
        if key_code == 0 {
            return false;
        }
        let guard = self
            .schema_toggle_origin
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        matches!(
            guard.as_ref(),
            Some((_, generation, vk))
                if *vk == key_code && *generation == self.engine_mgr.schema_generation()
        )
    }

    /// 执行回程：回到来源方案并用掉记录。调用方须先用
    /// [`Self::schema_return_key_action`] 确认，且**不得持 `State` 锁**。
    pub(crate) fn run_schema_return(&self) -> bool {
        let origin = self
            .schema_toggle_origin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match origin {
            Some((origin, generation, _)) if generation == self.engine_mgr.schema_generation() => {
                debug!("toggle_schema: 回程 -> {origin}");
                self.switch_schema_by_id(&origin);
                true
            }
            _ => false,
        }
    }

    /// 用户主动切换方案后的统一收尾（引擎已切好，此处只处理状态与副作用）。
    ///
    /// 抽出来是因为「切方案」有三个入口，行为却各自漂移过：`switch_schema` 不持久化
    /// `schema.active`、`select_schema` 无条件归位中文而循环键按配置、只有循环键清 preedit
    /// 和取消 CapsLock。新增的直达热键不再添第四份，与循环键共用这里。
    /// 已在目标方案时的**状态归位**：方案不用换，只把输入状态带回「能用这个方案打字」。
    ///
    /// # 为什么不能直接 return
    ///
    /// 「切到某方案」的语义是「我要用这个方案打字」，不只是「把 active 改成它」。英文
    /// 半角态或 CapsLock 开着时，方案本来就对——需要发生的恰恰只剩这里这点归位。早退
    /// 回去就是「在这个键最该生效的场景下按了毫无反应」，真机上报过两次
    /// （见 `schema_switch_entries_do_not_early_return_past_the_finish`）。
    ///
    /// # 为什么不复用 `finish_user_schema_switch`
    ///
    /// 那个函数还会持久化 `schema.active`、重挂拆字库/注释库/辅助码表、刷新工具栏标签
    /// ——方案根本没变，这些全是白做，而重写 `schema.active` 更是实打实的副作用：直通
    /// 命令 `ime.schema` 可被脚本反复调用，旧实现每次都重写一遍配置文件。
    ///
    /// ★ **没有实际变化时完全静默**：本来就是中文态、大写也没开，那就什么都不该发生，
    /// 连状态泡都不该弹。判据是「有没有真的改动状态」，不是「有没有被调用」。
    fn restore_state_for_same_schema(&self) {
        let caps_cancelled = self.force_cancel_caps_lock();
        let follow = self.rt().config.input.punct.follow_mode;
        let to_chinese = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let flip = !state.chinese_mode;
            if flip {
                state.chinese_mode = true;
                if follow {
                    state.chinese_punct = true;
                }
            }
            flip
        };
        if !caps_cancelled && !to_chinese {
            return;
        }
        self.record_app_mode(true);
        self.record_last_state();
        self.push_state_update();
        self.show_status();
        self.notify_toolbar();
    }

    fn finish_user_schema_switch(&self, schema_id: &str, log_verb: &str) {
        // 注：往返键的来源记录**不在这里清**。本函数只覆盖五条切方案路径中的两条，
        // 清在这里等于只清一半；改由 `schema_generation` 代际校验统一失效，
        // 见 `Coordinator::schema_toggle_origin` 的字段说明。
        self.sync_chaizi_assets(); // 拆字库/字根字体随活跃方案切换（变更检测，未变不动）
        self.sync_comment_dicts(); // 方案专属注释库（`schemas` 字段）同理
        self.invalidate_aux_code_table(); // 辅助码表各方案不同，切方案必须重挂（见函数注释）
        // ── 归位到「能用新方案打字」的状态：无条件，不受任何配置门控 ──────────────
        //
        // 切方案的语义前提就是「我要用这个方案打字」，而英文半角与 CapsLock 开启这两种
        // 状态下按键都不进引擎（前者在 handle_key_event 的英文分水岭原样透传，后者被 C++
        // 的 capsLockLetterPassthrough 同步透传，服务端连事件都收不到）。不归位的话，方案
        // 确实换了、`schema.active` 也写了盘，用户却观察不到任何变化——**真机报障原话是
        // 「方案切换热键在英文状态或大写状态不生效」，实为切了但看不见**。
        //
        // ⚠ 这两件事**曾经**共用 `input.capslock.cancel_on_mode_switch`（出厂 false），
        // 于是出厂配置下这段恒不执行。判据教训：**一个动作的语义前提不可配置，可配置的
        // 只能是副作用**。那个开关的正当作用域是切中英模式（用户可能正想打大写英文），
        // 与切方案无关，故这里改调不看开关的 `force_cancel_caps_lock`。
        //
        // 托盘 `select_schema` 一直是无条件归位中文的——「托盘切得动、热键切不动」正是
        // 两条路收尾不一致的直接后果。它现已并入本函数，三个入口至此同一套收尾。
        let caps_cancelled = self.force_cancel_caps_lock();
        let bundle = self.rt();
        let follow = bundle.config.input.punct.follow_mode;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // 只在**实际发生** false→true 翻转时才动标点与记账：chinese_mode 本就是 true 时，
        // chinese_punct 可能是用户用 toggle_punct 单独设的，切方案不该把它重置回去。
        let to_chinese = !state.chinese_mode;
        if to_chinese {
            state.chinese_mode = true;
            if follow {
                state.chinese_punct = true;
            }
        }
        state.input_buffer.clear();
        state.candidates.clear();
        state.preedit.clear();
        drop(state);
        if caps_cancelled || to_chinese {
            self.record_app_mode(true);
            self.record_last_state();
        }
        self.notify_ui_hide();
        self.push_state_update();
        self.show_status();
        self.notify_toolbar();
        info!("{}: {}", log_verb, schema_id);
        if let Err(e) = Config::set_user_string(&["schema", "active"], schema_id) {
            warn!("{}: 持久化 schema.active 失败: {}", log_verb, e);
        }
    }

    /// 判断 key_code 是否为配置的 toggle 模式键（从编译后的 key_up 热键提取 vk 低 16 位）。
    /// TSF 仅在干净单击时于 keyUp 转发这些键，故据此判定即可直接切换。
    ///
    /// ⚠ 必须按 `action` 过滤，不能只看「key_up 里有没有这个 key_code」：修饰键作二三候选键
    /// （`select_key_groups = ["lrctrl"]`）也登记在 key_up 里，只看 key_code 会把「只配了选词
    /// 用的 Ctrl」当成切换键——空闲时轻敲 Ctrl 会莫名切中英文。
    pub(crate) fn is_toggle_mode_keycode(&self, key_code: u32) -> bool {
        self.rt()
            .compiled_hotkeys
            .key_up
            .iter()
            .any(|e| e.action == "toggle_mode" && (e.match_hash & 0xFFFF) == key_code)
    }

    /// 按 id 在 `schema.mix_modes` 配置序中定位下标（与 `match_mix_trigger` 的 u8 下标语义一致，
    /// 最多 256 项）。供 `z_key_action = "mix:<id>"` 分发定位；未找到返回 None。
    pub(crate) fn mix_mode_idx(&self, id: &str) -> Option<u8> {
        self.rt()
            .config
            .schema
            .mix_modes
            .iter()
            .take(u8::MAX as usize + 1)
            .position(|m| m.id == id)
            .map(|i| i as u8)
    }

    /// 找出 key_code 绑定的 mix 模式下标。
    ///
    /// 「按配置顺序先到先得」的歧义已随收编消失：新表是 Map，一个键只能有一个动词。
    pub(crate) fn match_mix_trigger(&self, key_code: u32) -> Option<u8> {
        // 数据源同 `match_special_trigger`：统一走 `bound_action_for`，
        // `mix_modes[].trigger_keys` 已由 `normalize` 折算进 `keys.key_actions`。
        if let Some(wind_config::BoundAction::Mix(id)) = self.bound_action_for(key_code) {
            return self.mix_mode_idx(&id);
        }
        None
    }

    /// 选中当前页第 `page_offset`（0=首选）候选。
    /// 文本透镜（拼音/英文）走组合区逐步转换：部分匹配并入 committed 前缀、裁剪缓冲、重转剩余
    /// （剩余仍由 mix 成员方案出候选，不落五笔），留模式内不上屏；完整匹配整体上屏 + 造词。
    /// 数字透镜（计算）的候选恒整体上屏。
    pub(crate) fn mix_select(&self, state: &mut State, page_offset: usize) -> KeyAction {
        let (start, end) = self.page_range(state);
        let gi = start + page_offset;
        if gi >= end {
            return KeyAction::Consumed;
        }
        let cand = state.candidates[gi].clone();
        // $AA/$SS 组折叠候选：补全编码到完整码并重查展开（二级选择，不上屏组名）。
        if cand.is_group {
            state.mix_buffer = cand.group_code.clone();
            state.mix_cursor = state.mix_buffer.len(); // 补全到完整码：光标落末尾
            self.update_mix_candidates(state);
            let display = state.preedit.clone();
            self.notify_ui_update(state);
            return KeyAction::UpdateComposition {
                caret_pos: display.chars().count() as u32,
                text: display,
            };
        }
        // $CC 命令候选：执行动作（退出混输后异步跑），不走文本/分段上屏。
        let code = state.mix_buffer.clone();
        if let Some(act) =
            self.overlay_commit_command(state, &cand, &code, |s, st| s.exit_mix_mode(st))
        {
            return act;
        }
        // 整体上屏 vs 分步确认的**真正判据**——数字透镜的计算结果与自由输入的原文都没有
        // 可分段消费的编码，只有文本透镜（拼音/英文/码表）才做前缀分步确认。
        let numeric = self.mix_lens(state).commits_whole();
        let total = state.mix_buffer.len();
        let consumed = cand.consumed_length;
        let partial = !numeric
            && consumed > 0
            && consumed < total
            && state.mix_buffer.is_char_boundary(consumed);
        if partial {
            let code = Self::cand_code(&state.mix_buffer, &cand);
            // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
            self.record_selection(
                &self.freq_code(&state.mix_buffer, &cand),
                &cand.text,
                cand.source,
            );
            self.record_commit(
                &cand.text,
                code.len() as u32,
                page_offset as i32,
                wind_store::stats::CommitSource::Mix,
            );
            state.committed_segs.push((
                Self::raw_consumed_code(&state.mix_buffer, consumed, true),
                code,
                cand.text.clone(),
                cand.source,
                cand.boundary,
            ));
            state.committed_text.push_str(&cand.text);
            state.mix_buffer = state.mix_buffer[consumed..].to_string();
            // 分步确认消费掉前缀码：光标落剩余码末尾
            state.mix_cursor = state.mix_buffer.len();
            self.update_mix_candidates(state);
            let display = state.preedit.clone();
            self.notify_ui_update(state);
            KeyAction::UpdateComposition {
                caret_pos: display.chars().count() as u32,
                text: display,
            }
        } else {
            let out = format!("{}{}", state.committed_text, cand.text);
            let code_len = if numeric {
                0
            } else {
                Self::cand_code(&state.mix_buffer, &cand).len() as u32
            };
            if !numeric {
                // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
                let freq_code = self.freq_code(&state.mix_buffer, &cand);
                self.record_selection(&freq_code, &cand.text, cand.source);
                state.committed_segs.push((
                    state.mix_buffer.clone(), // 消费整串：回退码即整个缓冲
                    code,
                    cand.text.clone(),
                    cand.source,
                    cand.boundary,
                ));
                // 单段整句同样要造词（混输下拼音子引擎的整句一次上屏亦只 push 一段）。
                self.learn_phrase_on_commit(state, cand.is_synthesized);
            } else {
                // 数字透镜（计算/日期/金额）无编码可记词频，但同样是一次上屏：
                // 单独记历史，使「算完再按 ; 空格」能重复刚上屏的结果。
                self.push_commit_history(&cand.text);
            }
            // 输入统计：混合模式上屏（计算结果 code_len=0；选词用候选码长）。
            self.record_commit(
                &cand.text,
                code_len,
                page_offset as i32,
                wind_store::stats::CommitSource::Mix,
            );
            // 变体候选末段用覆盖文本；普通候选整体转换（保留 STPhrases 跨段词级消歧）。
            let out = match &cand.s2t_override {
                Some(t) => format!("{}{}", self.maybe_s2t(state, &state.committed_text), t),
                None => self.maybe_s2t(state, &out),
            };
            self.exit_mix_mode(state);
            self.notify_ui_hide();
            Self::commit_action(out, true)
        }
    }

    /// 刷新 mix 候选：按配置成员序逐个查询、合并、按文本去重。
    ///
    /// 成员分三类：快捷输入内置来源（`quick_input.calc/.date/.number`，由
    /// `wind_quick_input` 直接算）、重复上屏（`quick_input.repeat`，取上屏历史，**仅空缓冲时**）、
    /// 真实方案（拼音/英文等，经 `convert_with`）。
    ///
    /// 数字透镜只取内置来源（表达式无拼音/英文意义），文本透镜只取真实方案，避免互相污染。
    /// **成员顺序即候选优先级**——把 `quick_input.calc` 排在最前即得「计算结果作首选」。
    pub(crate) fn update_mix_candidates(&self, state: &mut State) {
        state.candidates.clear();
        self.reset_candidate_view(state);
        state.mix_repeat = false;
        // 组合区 = 显示态前缀 + 已转换前缀（文本透镜逐步转换累积）+ 剩余缓冲。
        state.preedit = format!(
            "{}{}{}",
            state.mix_prefix, state.committed_text, state.mix_buffer
        );
        // 默认主体 = 原始缓冲；文本透镜若给出音节分隔显示，下方会覆盖为该显示串。
        state.overlay_body = state.mix_buffer.clone();
        if state.mix_buffer.is_empty() {
            self.inject_mix_repeat_candidate(state);
            return;
        }
        let lens = self.mix_lens(state);
        if lens == MixLens::Free {
            // 自由输入：缓冲不是任何成员的合法编码，查谁都只会得到噪声。唯一候选＝所打原文，
            // 保证「打什么上屏什么」。**不做全角转换**——与 mix 既有上屏路径保持一致
            // （临英会转，两者是否对齐是独立待定项，不在本次改动范围内）。
            //
            // 刻意**不走 `finalize_candidates`**：那是词库候选里 `$AA`/`$CC` 特殊语法的展开点，
            // 而自由输入的文本是用户逐键打进来的字面内容——打了 `$AA` 就该出 `$AA`。
            state.candidates = vec![Candidate {
                text: state.mix_buffer.clone(),
                ..Default::default()
            }];
            return;
        }
        let numeric = lens == MixLens::Numeric;
        let members = self.mix_members_resolved(state.mix_id);
        let mut cands: Vec<Candidate> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // 文本透镜：取首个**真的给出了分段**的成员方案的 preedit_display（拼音的 `ni'hao`）
        // 作组合区显示。
        //
        // ⚠️ 判据是「与缓冲不同形」，不是「非空」。码表/英文引擎的 `preedit_display` 恒等于
        // 原始输入（见 `CodeTableEngine::convert`），按「非空即采纳」会让**成员顺序**决定编码栏
        // 形态：用户把快符 `kf` 排在 `$primary_pinyin` 之前，拼音的拆分串就永远轮不上，
        // 表现为「快捷输入里完全没有拆分显示」。成员顺序管的是候选优先级，管不到这里。
        let mut text_display: Option<String> = None;
        for member in &members {
            if let Some(src) = wind_quick_input::QuickSource::from_member(member) {
                if !numeric {
                    continue; // 文本模式跳过表达式类来源
                }
                let dp = self.rt().config.schema.quick_input.decimal_places;
                // 表达式模板（`{amt(unit='圆')}`）走 cmdbar 求值；变量模板在 quick-input 内
                // 本地展开。分发在 quick-input 里按条目形态做，这里只提供求值器。
                let eval = |text: &str, values: &wind_quick_input::QuickValues| {
                    crate::quick_eval::eval_expr(text, values)
                };
                // 用户调整（右键调序/停用）叠加到格式表之上。整张镜像传下去，由
                // quick-input 按**实际渲染的类别**取用——`QuickSource::Date` 会产出
                // date 或 year_month 之一，在这里按 src 猜类别会让年月的调序静默失效。
                // 取镜像不查库：本函数在每次按键的候选刷新路径上。
                let adjust = self.quick_adjust_snapshot();
                for r in wind_quick_input::generate_adjusted(
                    src,
                    &state.mix_buffer,
                    dp,
                    &self.quick_formats,
                    &adjust,
                    Some(&eval),
                ) {
                    if !r.text.is_empty() && seen.insert(r.text.clone()) {
                        // 稳定 id：右键要认「哪条格式」，而 text 逐次输入都不同。
                        // 类别由格式表反查（id 在表内唯一）。
                        let kind = self
                            .quick_formats
                            .entries()
                            .iter()
                            .find(|e| e.id == r.id)
                            .map(|e| e.kind);
                        cands.push(Candidate {
                            id: kind
                                .map(|k| crate::handle_quick_format::quick_cand_id(k, &r.id))
                                .unwrap_or_default(),
                            text: r.text,
                            ..Default::default()
                        });
                    }
                }
            } else if wind_quick_input::is_quick_member(member) {
                // quick_input.repeat：仅空缓冲时有候选（上面已 return），此处无动作。
                // 旧值 quick_input 若漏迁移也落这里——不产候选，胜过按未知方案去加载。
                continue;
            } else {
                if numeric {
                    continue; // 数字模式跳过真实方案（表达式无拼音/英文意义）
                }
                if !self.engine_mgr.ensure_schema(member) {
                    continue;
                }
                let result = self.engine_mgr.convert_with(member, &state.mix_buffer, 50);
                // 空串是「这个成员没给出显示串」（方案加载失败等），不是一种形态——漏掉这半个
                // 判据会把组合区整段吞成前缀。
                if text_display.is_none()
                    && !result.preedit_display.is_empty()
                    && result.preedit_display != state.mix_buffer
                {
                    text_display = Some(result.preedit_display.clone());
                }
                for c in result.candidates {
                    if seen.insert(c.text.clone()) {
                        cands.push(c);
                    }
                }
            }
        }
        // 文本透镜用音节分隔显示；数字透镜（计算）保持原始表达式。
        if let Some(disp) = text_display {
            state.preedit = format!("{}{}{}", state.mix_prefix, state.committed_text, disp);
            state.overlay_body = disp; // 供光标换算（含引擎插入的音节分隔符）
        }
        // 统一展开汇聚点：混输成员词库候选内 `$` 特殊语法在此展开（见 finalize_candidates）。
        state.candidates = self.finalize_candidates(cands, &state.mix_buffer);
        // 简繁 1对多变体展开（约束见 expand_s2t_variants 文档）。
        self.expand_s2t_variants(state);
    }

    /// 空缓冲时注入「重复上屏」候选（成员 `quick_input.repeat`）：把上次上屏的内容
    /// 摆成唯一候选，按空格即再上屏一次。
    ///
    /// 这是快捷输入的固有能力（Go 版 `handleQuickInputRepeat`），Rust 重写为 mix 成员时丢失。
    /// 复用 `recent_commits` 上屏历史，与 z 键重复上屏、加词推荐同一事实源。
    ///
    /// 置 `state.mix_repeat` 标记而非在候选上加字段：这条候选与输入缓冲无对应关系
    /// （码为空），选词记录、造词、标点顶屏三条路径都必须绕开它，用一个状态位表达
    /// 「当前候选区是重复候选」比让每条路径各自去嗅探候选特征更难写错。
    fn inject_mix_repeat_candidate(&self, state: &mut State) {
        if !state.committed_text.is_empty() {
            return; // 模式内已逐步上屏过内容：此时的空缓冲不是「刚进来」，不插重复
        }
        let has_repeat = self
            .rt()
            .config
            .schema
            .mix_modes
            .get(state.mix_id as usize)
            .map(|m| {
                m.members
                    .iter()
                    .any(|s| s == wind_quick_input::MEMBER_REPEAT)
            })
            .unwrap_or(false);
        if !has_repeat {
            return;
        }
        let Some(text) = self
            .recent_commits_snapshot()
            .into_iter()
            .find(|t| !t.is_empty())
        else {
            return;
        };
        state.candidates = vec![Candidate {
            text,
            ..Default::default()
        }];
        state.mix_repeat = true;
    }

    /// 数字 lens（计算/表达式）：数字与符号（含 `=`）作输入，字母作选词。
    /// 仅含 quick_input 成员的 mix 在首字符为数字/符号时进入。返回该键应输入的字符。
    pub(crate) fn mix_numeric_input_char(key_code: u32, shift: bool) -> Option<char> {
        if (keymap::VK_A..=keymap::VK_Z).contains(&key_code) {
            None // 字母在数字 lens 作选词，不输入
        } else {
            // 数字 + 任意符号（含 = + - * / . 等）入缓冲；小键盘键回退 numpad_char，
            // 使小键盘数字/运算符与主键盘区在数字透镜里表达式输入一致（问题：快捷输入下
            // 小键盘不生效）。
            printable_char(key_code, shift).or_else(|| numpad_char(key_code))
        }
    }

    /// overlay 模式的 **快捷键组合守卫**（Ctrl/Alt/Cmd，见 `MOD_SHORTCUT`）。
    ///
    /// 走到模式处理器的快捷键组合**必定不是热键**——全局热键与 Ctrl+数字 候选操作
    /// （置顶/删除）都在单点分派之前就匹配完了（见 `handle_key_event` 的顺序）。剩下的
    /// 只可能是宿主自己的快捷键（Ctrl+A / Ctrl+C / macOS 上的 ⌘C ⌘V…）。
    ///
    /// mix 此前没有这道守卫：`Ctrl+E` 会被 ① 的字母臂当成字面 `e` 插进缓冲——用户想按
    /// 宿主快捷键，实得组合区凭空多一个字符。自由输入让 ① 收下的键更多（一切可打印键），
    /// 不补这道守卫的话，`Ctrl+分号`、`Ctrl+减号` 之类也会一并变成字面输入。
    ///
    /// 语义对齐主输入路径：有待输入内容则放弃整段组合并隐藏候选窗，空缓冲则透传给宿主。
    ///
    /// # ⚠️ 想加「候选窗显示时生效的快捷键」，加在上游、不要加在这里
    ///
    /// 本守卫是**兜底**，只处理没人认领的快捷键组合。要新增在候选窗显示期间生效的
    /// 快捷键，正确落点是 `handle_key_event` 里 `handle_candidate_action_hotkey` 那一段
    /// （Ctrl+数字 置顶/删除就在那儿），它在单点分派**之前**，因此天然优先于本守卫，
    /// 且五个模式一次接通。把这类键塞进各模式处理器则要写五遍，还会漏。
    ///
    /// 五个模式处理器（mix / 临拼 / 临英 / 特殊 / URL）**全部**接了本守卫——按「枚举模式
    /// 处理器逐个问它接了吗」的纪律收口。收口前全仓只有**临拼的字母臂**判过一次 Ctrl/Alt
    /// （`handle_temp.rs`），连临拼自己的数字臂与标点臂都没护住。
    pub(crate) fn overlay_ctrl_alt_guard(
        &self,
        state: &mut State,
        data: &KeyEventData,
        has_pending: bool,
        exit: impl Fn(&Self, &mut State),
    ) -> Option<KeyAction> {
        if data.modifiers & MOD_SHORTCUT == 0 {
            return None;
        }
        if has_pending {
            exit(self, state);
            self.notify_ui_hide();
            return Some(KeyAction::ClearComposition);
        }
        Some(KeyAction::PassThrough)
    }

    /// mix 模式按键处理 —— 双透镜统一管线（见架构说明）。
    /// 首字符确定 lens：数字/符号 → 数字 lens（符号输入、字母选词）；字母 → 文本 lens
    /// （字母输入、数字选词、`-`/`=` 翻页）。每键顺序：控制键 → ①输入字符 → ②翻页/高亮
    /// → ③本 lens 选词键 → ④配置二三候选键 → ⑤其它标点顶屏。
    pub(crate) fn handle_mix_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        // Ctrl/Alt 组合守卫：必须最先——否则下方 ① 会把 `Ctrl+E` 的字母当字面输入收走。
        if let Some(act) = self.overlay_ctrl_alt_guard(
            state,
            data,
            !state.mix_buffer.is_empty() || !state.committed_text.is_empty(),
            |s, st| s.exit_mix_mode(st),
        ) {
            return act;
        }
        // 编码区光标移动（左右 / Home / End）。注：数字透镜下 -/= 等是输入字符，但方向键
        // 在两个透镜里都不是输入，故可在分派前统一拦截。
        if let Some(act) = self.overlay_cursor_key(state, data) {
            return act;
        }
        let refresh = |this: &Self, state: &mut State| -> KeyAction {
            this.update_mix_candidates(state);
            let d = state.preedit.clone();
            let caret_pos = this.overlay_caret(state);
            this.notify_ui_update(state);
            KeyAction::UpdateComposition { text: d, caret_pos }
        };
        let commit_text = |this: &Self, state: &mut State, t: String| -> KeyAction {
            this.exit_mix_mode(state);
            this.notify_ui_hide();
            if t.is_empty() {
                KeyAction::ClearComposition
            } else {
                Self::commit_action(t, true)
            }
        };
        // 进入键二次按下（缓冲空 + 无已转换前缀）：按中英标点配置上屏该符号并退出。
        // 必须前置于下方数字透镜——否则 ; 等会被 printable_char 当表达式字符吞进缓冲。
        // 顺带武装智能符号：时限内再按同键即换英文形（`;` → `；` → `;`），否则这个键被模式
        // 占着、英文形没有通路。press2 的拦截在 try_activate_mode 开头，早于模式激活链。
        // `free_input = always` 的实例除外：它的引导键必须能作字面字符打进缓冲，否则
        // 「进模式后第一个想打的就是引导键那个符号」永远只会上屏符号并退出，专做字面
        // 输入的实例连门都进不去。（这类"某开关要否决某条既有通路"的改动，历史上多次
        // 只接了一处就以为完事——此处与 ①⑤ 的分派、②的 include_printable 是同一组。）
        if self.mix_free_input(state.mix_id) != FreeInputMode::Always
            && state.mix_buffer.is_empty()
            && state.committed_text.is_empty()
            && data.modifiers & MOD_SHIFT == 0
            && self.match_mix_trigger(data.key_code) == Some(state.mix_id)
            && let Some(ch) = punct_char(data.key_code, false)
        {
            let out = self.convert_punct_char(state, ch);
            self.arm_smart_symbol_after_commit(state, ch, &out);
            self.record_commit(&out, 0, -1, wind_store::stats::CommitSource::Punctuation);
            self.exit_mix_mode(state);
            self.notify_ui_hide();
            return Self::commit_action(out, true);
        }
        match data.key_code {
            // Esc：放弃退出，实现收口在 `cancel_session`。
            keymap::VK_ESCAPE => self.cancel_session(state),
            keymap::VK_BACK | keymap::VK_DELETE => {
                // Backspace：段回退**优先于光标**（文本透镜有已转换段先退回最后一段，你→ni，
                // 码并回缓冲前部）；否则删光标前一字符。Delete 只删光标后一字符、删空后才回退段
                // ——与主输入同构的刻意不对称。缓冲删空则退出。
                let backward = data.key_code == keymap::VK_BACK;
                if backward && !state.committed_segs.is_empty() {
                    return self.pop_mix_seg(state, &refresh);
                }
                if state.mix_buffer.is_empty() {
                    if backward {
                        self.exit_mix_mode(state);
                        self.notify_ui_hide();
                        return KeyAction::ClearComposition;
                    }
                    return KeyAction::Consumed; // Delete 且缓冲空：只吃键，不改退出语义
                }
                let removed = {
                    let mut ed =
                        preedit_cursor::BufEdit::new(&mut state.mix_buffer, &mut state.mix_cursor);
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
                if state.mix_buffer.is_empty() {
                    if !state.committed_segs.is_empty() {
                        return self.pop_mix_seg(state, &refresh);
                    }
                    self.exit_mix_mode(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                } else {
                    refresh(self, state)
                }
            }
            keymap::VK_SPACE => {
                // 重复上屏：整体上屏上次内容，不记选词/不造词（该候选无对应编码）。
                if state.mix_repeat && !state.candidates.is_empty() {
                    let text = state.candidates[0].text.clone();
                    self.record_commit(&text, 0, 0, wind_store::stats::CommitSource::Mix);
                    // 重复上屏本身也入历史：连按两次仍重复同一内容（而非取到更早的一条）。
                    self.push_commit_history(&text);
                    let out = self.maybe_s2t(state, &text);
                    return commit_text(self, state, out);
                }
                // 空格：选当前高亮候选（文本透镜逐步转换）
                if state.candidates.is_empty() {
                    // 上屏剩余原码：committed 段已在各次选词记过，此处只记 mix_buffer 避免重复。
                    self.record_commit(
                        &state.mix_buffer,
                        state.mix_buffer.len() as u32,
                        -1,
                        wind_store::stats::CommitSource::Mix,
                    );
                    let out = self.maybe_s2t(
                        state,
                        &format!("{}{}", state.committed_text, state.mix_buffer),
                    );
                    commit_text(self, state, out)
                } else {
                    let (start, _) = self.page_range(state);
                    let gi = self
                        .highlighted_global_index(state)
                        .min(state.candidates.len() - 1);
                    self.mix_select(state, gi - start)
                }
            }
            keymap::VK_RETURN => {
                // clear 模式：整段放弃，不上屏任何内容（含已选词的 committed_text）。
                // 须先于下方各分支——此前该判断只写在「空缓冲」分支内，导致「打了码再回车」
                // 仍走非空缓冲路径无条件上屏原码，配置形同虚设（与主输入路径行为不一致）。
                if self.enter_clears_composition() {
                    return commit_text(self, state, String::new());
                }
                // 空缓冲（只按了模式键、无已转换前缀）：commit 模式上屏模式键符号本身
                // （原样不转换，如 ;）。
                if state.mix_buffer.is_empty() && state.committed_text.is_empty() {
                    if !state.mix_prefix.is_empty() {
                        let sym = state.mix_prefix.clone();
                        self.record_commit(
                            &sym,
                            0,
                            -1,
                            wind_store::stats::CommitSource::Punctuation,
                        );
                        return commit_text(self, state, sym);
                    }
                    return commit_text(self, state, String::new());
                }
                // 非空缓冲：上屏「引导字母 + 已转换前缀 + 缓冲原文」。符号引导键行为不变
                // （;nihao → nihao）；字母引导键（z_key_action = "mix:<id>"）归还那个字母，
                // 判据与临拼回车、切中英文共用，见 `guide_to_return`。
                let guide = Self::guide_to_return(&state.mix_prefix, &state.committed_text);
                let raw = format!("{}{}", guide, state.mix_buffer);
                self.record_commit(
                    &raw,
                    raw.len() as u32,
                    -1,
                    wind_store::stats::CommitSource::Mix,
                );
                let out = self.maybe_s2t(
                    state,
                    &format!("{}{}{}", guide, state.committed_text, state.mix_buffer),
                );
                commit_text(self, state, out)
            }
            _ => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                // 透镜按**当前缓冲**（即本键落下之前的内容）推导，本键的归属据此判定；
                // 空缓冲时改由本键自己决定（见 `mix_lens_for_key`）。
                // 插入后由 `refresh` → `update_mix_candidates` 用新缓冲重算。
                let lens = self.mix_lens_for_key(state, data, shift);
                let free_on = self.mix_free_input(state.mix_id) != FreeInputMode::Off;
                let is_letter = (keymap::VK_A..=keymap::VK_Z).contains(&data.key_code);

                // ① 输入字符（按 lens）
                let input = match lens {
                    // 自由输入：一切可打印键字面入缓冲（保留 Shift 形态）；小键盘同样是字面。
                    MixLens::Free => {
                        printable_char(data.key_code, shift).or_else(|| numpad_char(data.key_code))
                    }
                    // 数字透镜：小写字母仍是选词键（否则这个透镜一个选词键都不剩），但
                    // **大写字母任何成员都接受不了** —— 自由输入开启时直接字面，由它把
                    // 透镜带进 Free，于是 `;12.5` 之后按 Shift+G 能续打成 `12.5GB`。
                    MixLens::Numeric if free_on && shift && is_letter => {
                        Some((b'A' + (data.key_code - keymap::VK_A) as u8) as char)
                    }
                    // 二三候选键（`;` `'`）在数字透镜下**必须让开**，交给第④步的选词判定。
                    //
                    // `mix_numeric_input_char` 收的是「一切非字母可打印字符」，比本透镜的
                    // `accepts`（`is_expr_char`）宽 —— 不在此让开的话 `;` `'` 会被当表达式
                    // 字符吞进缓冲并 `return`，④永远够不着，`free_input = off` 也救不回来
                    // （`free_on` 的判定在④）。这正是「数字输入模式下 `;`/`'` 不能选候选」
                    // 的根因，与文本透镜下的夺取是两回事。
                    //
                    // 夺取生效时（`auto`/`always` + `takes`）本臂不让开，行为逐字节不变：
                    // 仍由本函数收下作字面，而不是绕一圈让⑤收——两条路径产出相同，但少一次
                    // 状态穿越。判据与④共用 `mix_select_keys_active`，见那里的 ★。
                    MixLens::Numeric
                        if !shift
                            && self.mix_select_keys_active(state.mix_id)
                            && self.select_key_offset(data.key_code).is_some() =>
                    {
                        None
                    }
                    MixLens::Numeric => Self::mix_numeric_input_char(data.key_code, shift),
                    // 文本透镜：字母入缓冲。自由输入关闭时 Shift 被丢弃（既有行为，恒小写）；
                    // 开启时大写字母即越界字符，字面入缓冲并把透镜带进 Free。
                    MixLens::Text if is_letter => {
                        let base = (data.key_code - keymap::VK_A) as u8;
                        Some(if free_on && shift {
                            (b'A' + base) as char
                        } else {
                            (b'a' + base) as char
                        })
                    }
                    MixLens::Text => None,
                };
                if let Some(ch) = input {
                    preedit_cursor::BufEdit::new(&mut state.mix_buffer, &mut state.mix_cursor)
                        .insert(ch);
                    return refresh(self, state);
                }

                // ② 翻页/高亮（输入字符已消费；数字 lens 的 -/= 已作输入吃掉）
                //
                // `include_printable`：自由输入开启时可打印翻页键（`-`/`=` 等 `page_keys` 里
                // 的键组）必须让位字面输入，否则 `all-in-one` 的 `-` 会被吃成翻页。翻页职责
                // 转给 PageUp/PageDown ——本模式的数字键与二三候选键本就被选词占着，用户在
                // 这里本来就该用功能键翻页。关闭自由输入时维持既有的 `true`。
                if let Some(act) = self.apply_session_action(
                    state,
                    data,
                    self.mix_nav_include_printable(state.mix_id),
                ) {
                    return act;
                }

                // ③④ 选词键。Free 透镜下没有选词键——字母与数字都已在 ① 作字面输入消费，
                // 走到这里的只剩控制键，继续往下落即可。
                if lens != MixLens::Free {
                    // ③ 本 lens 选词键：数字 lens 用字母（a=首选），文本 lens 用数字（1=首选）
                    //
                    // 数字臂的 `!shift` 是**必须的**（④ 早就有、③ 一直漏）：`Shift+1..9` 是
                    // `!@#$%^&*(` 这九个符号，从来不是选词键。漏判的后果是 `;for(` 里的 `(`
                    // （=Shift+9）被当成「选第 9 个候选」吃掉，组合区变成 `;for`——自由输入
                    // 上线后才暴露，因为在此之前这些符号本就走不进缓冲。
                    let sel = if lens == MixLens::Numeric {
                        is_letter.then(|| (data.key_code - keymap::VK_A) as usize)
                    } else {
                        (!shift && (keymap::VK_1..=keymap::VK_9).contains(&data.key_code))
                            .then(|| (data.key_code - keymap::VK_1) as usize)
                    };
                    if let Some(off) = sel {
                        return self.mix_select(state, off);
                    }

                    // ④ 配置二三候选键（默认 `;` `'`）。
                    //
                    // 自由输入夺取时让位字面输入：`rock'n'roll` / `don't` / `for(;;)` 里的
                    // `'` `;` 恰好就是默认选词键，不让位就**走不到 ⑤**——实测 `;rock` 按 `'`
                    // 会选走第 3 候选「日欧」，而它 consumed_length=2 还会触发分步确认，
                    // 把 `ro` 吃掉转成汉字、缓冲只剩 `ck`，整串输入被打散。
                    //
                    // 数字键（③）刻意不在夺取范围：它是文本透镜唯一的选词通路，让位就
                    // 一个选词键都不剩。二三候选键则是数字键 2/3 的冗余别名，让位零能力损失。
                    //
                    // ★ 判据与①的数字臂共用 `mix_select_keys_active`：那里不让开，这里就是
                    // 不可达代码（数字透镜下曾如此，见该函数文档）。
                    if self.mix_select_keys_active(state.mix_id)
                        && !shift
                        && let Some(offset) = self.select_key_offset(data.key_code)
                    {
                        return self.mix_select(state, offset);
                    }
                }

                // ⑤ 越界字面输入：走到这里的可打印键在当前透镜下**既不是编码也不是功能键**，
                // 它不可能是编码，只能是字面内容 → 入缓冲，缓冲随即被判为 Free 透镜。
                // 这一步取代了下方⑥的顶屏语义（`_` `<` `,` `.` 等都归字面），代价是
                // `;nihao,` 不再顶屏出「你好，」——想要标点先按空格上屏再打即可。
                // 关闭自由输入的实例不走这条，⑥ 的既有行为原样保留。
                if free_on
                    && let Some(ch) =
                        printable_char(data.key_code, shift).or_else(|| numpad_char(data.key_code))
                {
                    preedit_cursor::BufEdit::new(&mut state.mix_buffer, &mut state.mix_cursor)
                        .insert(ch);
                    return refresh(self, state);
                }

                // ⑥ 其它标点：顶屏「已转换前缀 + 当前高亮候选」+ 转换后标点，退出。
                // 小键盘键（direct 语义）回退 numpad_char 复用此路——仅**文本透镜**会走到这里，
                // 数字透镜的小键盘早在 ① mix_numeric_input_char 作表达式字符入缓冲。
                // follow_main 时键已在入口归一化为主键盘键。
                if let Some(ch) =
                    punct_char(data.key_code, shift).or_else(|| numpad_char(data.key_code))
                {
                    // 重复上屏候选不参与顶屏：它是「空缓冲时的备选动作」而非本次输入的转换结果，
                    // 顶屏它等于用户没打字却被塞进上次的内容。此时按标点 = 空缓冲按标点。
                    let has_head = !state.mix_repeat && !state.candidates.is_empty();
                    // 高亮候选为组/命令：走统一选中（组→展开重查，命令→执行动作），标点不单独上屏。
                    if has_head {
                        let (start, _) = self.page_range(state);
                        let idx = self
                            .highlighted_global_index(state)
                            .min(state.candidates.len() - 1);
                        if state.candidates[idx].is_group || state.candidates[idx].is_command {
                            return self.mix_select(state, idx - start);
                        }
                    }
                    // 高亮是变体候选时末段用覆盖文本；否则整体转换（保留跨段词级消歧）。
                    let head = if has_head {
                        let idx = self
                            .highlighted_global_index(state)
                            .min(state.candidates.len() - 1);
                        match &state.candidates[idx].s2t_override {
                            Some(t) => {
                                format!("{}{}", self.maybe_s2t(state, &state.committed_text), t)
                            }
                            None => self.maybe_s2t(
                                state,
                                &format!("{}{}", state.committed_text, state.candidates[idx].text),
                            ),
                        }
                    } else {
                        self.maybe_s2t(state, &state.committed_text.clone())
                    };
                    let punct = self.convert_punct_char(state, ch);
                    self.exit_mix_mode(state);
                    self.notify_ui_hide();
                    Self::commit_action(format!("{}{}", head, punct), true)
                } else {
                    KeyAction::Consumed
                }
            }
        }
    }
}

#[cfg(test)]
mod theme_fallback_tests {
    use super::FALLBACK_THEME;
    use crate::coordinator::Coordinator;
    use std::cell::RefCell;

    /// 记录每次请求的主题 id，并按 `available` 决定成败——替身返回 `&str` 而非
    /// `Resolved`，降级判定与主题求值无关，不该为了测它去造一份主题数据。
    fn loader<'a>(
        available: &'a [&'a str],
        seen: &'a RefCell<Vec<String>>,
    ) -> impl FnMut(&str) -> anyhow::Result<&'static str> + 'a {
        move |n: &str| {
            seen.borrow_mut().push(n.to_string());
            if available.contains(&n) {
                Ok("theme")
            } else {
                anyhow::bail!("theme '{}' not found", n)
            }
        }
    }

    #[test]
    fn requested_theme_wins_without_touching_fallback() {
        let seen = RefCell::new(Vec::new());
        let got = Coordinator::load_theme_with_fallback(loader(&["violet"], &seen), "violet");
        assert_eq!(got.map(|(id, _)| id), Some("violet".to_string()));
        // 请求主题就绪时不该顺带去读 default——多一次读盘就是多一次半截 data 的暴露面。
        assert_eq!(*seen.borrow(), vec!["violet"]);
    }

    /// 本用例对应的真实故障：部署期 data\ 半截，violet 没复制到而 default 已就位。
    /// 修复前这里会让候选窗停在 `Resolved::default()` 的编译期零值外观直到重启。
    #[test]
    fn missing_theme_falls_back_to_default() {
        let seen = RefCell::new(Vec::new());
        let got = Coordinator::load_theme_with_fallback(loader(&[FALLBACK_THEME], &seen), "violet");
        assert_eq!(
            got.map(|(id, _)| id),
            Some(FALLBACK_THEME.to_string()),
            "请求主题缺失应降级到 {FALLBACK_THEME}"
        );
        assert_eq!(*seen.borrow(), vec!["violet", FALLBACK_THEME]);
    }

    #[test]
    fn missing_fallback_itself_is_not_retried() {
        let seen = RefCell::new(Vec::new());
        let got = Coordinator::load_theme_with_fallback(loader(&[], &seen), FALLBACK_THEME);
        assert!(got.is_none(), "default 自身缺失时无处可降级");
        assert_eq!(
            *seen.borrow(),
            vec![FALLBACK_THEME],
            "请求的就是 default 时不该重复试第二次"
        );
    }

    /// 两级皆失败必须返回 None 让调用方**保留当前**：reload 路径也走 push_theme，
    /// 此时硬发一份零值主题会把运行中已经好用的外观清掉——那是退化不是兜底。
    #[test]
    fn both_missing_yields_none_so_caller_keeps_current() {
        let seen = RefCell::new(Vec::new());
        let got = Coordinator::load_theme_with_fallback(loader(&["jade"], &seen), "violet");
        assert!(got.is_none());
        assert_eq!(*seen.borrow(), vec!["violet", FALLBACK_THEME]);
    }
}

#[cfg(test)]
mod mix_numpad_tests {
    use crate::coordinator::Coordinator;

    #[test]
    fn numpad_keys_feed_numeric_lens() {
        // 小键盘数字 / 运算符 → 表达式字符（此前只认主键盘区，快捷输入下小键盘被吞）。
        assert_eq!(Coordinator::mix_numeric_input_char(0x60, false), Some('0')); // Numpad0
        assert_eq!(Coordinator::mix_numeric_input_char(0x69, false), Some('9')); // Numpad9
        assert_eq!(Coordinator::mix_numeric_input_char(0x6B, false), Some('+')); // Numpad +
        assert_eq!(Coordinator::mix_numeric_input_char(0x6D, false), Some('-')); // Numpad -
        assert_eq!(Coordinator::mix_numeric_input_char(0x6A, false), Some('*')); // Numpad *
        assert_eq!(Coordinator::mix_numeric_input_char(0x6F, false), Some('/')); // Numpad /
        assert_eq!(Coordinator::mix_numeric_input_char(0x6E, false), Some('.')); // Numpad .
        // 主键盘区数字仍正常（回归保护）。
        assert_eq!(Coordinator::mix_numeric_input_char(0x31, false), Some('1')); // VK_1
        // 字母在数字透镜里作选词，不作输入。
        assert_eq!(Coordinator::mix_numeric_input_char(0x41, false), None); // 'A'
    }

    /// mix 成员占位符解析：$primary_pinyin 跟随主拼音方案，字面 id 精确解释。
    #[test]
    fn resolve_mix_member_placeholder_vs_literal() {
        use wind_config::config::MIX_MEMBER_PRIMARY_PINYIN as PH;
        assert_eq!(
            Coordinator::resolve_mix_member(PH, "shoudao"),
            "shoudao",
            "占位符应解析为主拼音方案"
        );
        assert_eq!(
            Coordinator::resolve_mix_member(PH, ""),
            "pinyin",
            "主拼音方案为空时占位符回退全拼"
        );
        // 字面 id 一律原样——"pinyin" 表示「就要全拼」，不被主拼音方案替换。
        assert_eq!(
            Coordinator::resolve_mix_member("pinyin", "shoudao"),
            "pinyin",
            "字面 pinyin 不应被替换"
        );
        assert_eq!(
            Coordinator::resolve_mix_member("quick_input", "shoudao"),
            "quick_input"
        );
        assert_eq!(
            Coordinator::resolve_mix_member("english", "shoudao"),
            "english"
        );
    }
}

/// 切方案收尾的**编译期**守卫。
///
/// 为什么是源码扫描而不是跑一遍 `finish_user_schema_switch`：那个函数末尾会
/// `Config::set_user_string(["schema","active"])` **真的写用户 config.toml**（测试进程里
/// `user_config_dir()` 就是真实的 `%APPDATA%`，没有隔离钩子）。跑一次就把开发者自己的
/// 活跃方案改掉——甚至可能因为 `set_user_value` 的「等于默认即删键」而把他手配的
/// `schema.active` 整条抹掉。守卫要防的东西不值这个代价。
#[cfg(test)]
mod schema_switch_finish_guard {
    const SRC: &str = include_str!("handle_mode.rs");

    /// 截取 `fn <name>` 之后括号配平的函数体，**并剥掉整行注释**。
    ///
    /// 剥注释不是洁癖：本文件的注释里就成段解释着「为什么不能读那个开关」，纯文本扫描
    /// 会把这段解释本身判成违规（初版守卫正是这样红的）。**判据要落在代码上，不能落在
    /// 讲述判据的文字上**——否则维护者只能靠删注释来让测试变绿。
    fn body_of(name: &str) -> String {
        let at = SRC
            .find(&format!("fn {name}"))
            .unwrap_or_else(|| panic!("源码里找不到 fn {name}（改名了？守卫需同步更新）"));
        let open = SRC[at..]
            .find('{')
            .unwrap_or_else(|| panic!("fn {name} 没有函数体"))
            + at;
        let mut depth = 0usize;
        for (i, ch) in SRC[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return SRC[open..open + i + 1]
                            .lines()
                            .filter(|l| !l.trim_start().starts_with("//"))
                            .collect::<Vec<_>>()
                            .join(
                                "
",
                            );
                    }
                }
                _ => {}
            }
        }
        panic!("fn {name} 的花括号不配平");
    }

    /// ★★ 幂等分支**不得**绕过收尾。
    ///
    /// 「切到某方案」的语义是「我要用这个方案打字」，不只是「把 active 改成它」。已是
    /// 目标方案时，需要发生的恰恰只剩收尾里那部分（归位中文、取消大写）——早退回去就是
    /// 「英文半角态/大写态下按方案热键毫无反应」，而那正是这个键最该生效的场景。
    ///
    /// 真机上这个症状出现过**两次**：第一次是收尾里的归位被 CapsLock 开关门控（见
    /// `finish_user_schema_switch_is_not_gated_by_capslock_option`），修好之后**同一个
    /// 症状复发**，因为 `switch_schema_by_id` 还有一条 `if active == target { return; }`
    /// 的早退绕过了刚修好的收尾。
    ///
    /// ⇒ 可复用判据：**修好一个动作的收尾之后，回头找它有没有提前返回的分支绕过那段收尾。**
    #[test]
    fn schema_switch_entries_do_not_early_return_past_the_finish() {
        for name in ["switch_schema_by_id", "select_schema"] {
            let body = body_of(name);
            assert!(
                body.contains("finish_user_schema_switch"),
                "{name} 必须走统一收尾"
            );
            // 幂等分支**不能是裸 return**：要么与切换尝试共用一个条件（都落到 finish），
            // 要么单独做状态归位。两者都行，唯独「什么都不做」不行。
            let idempotent_handled = body.contains("restore_state_for_same_schema")
                || body.contains("active_schema_id() == id || self.engine_mgr.switch_schema(&id)");
            assert!(
                idempotent_handled,
                "{name}: 「已是该方案」时既没归位也没走收尾 ⇒ 英文态/大写态下按方案热键\
                 毫无反应。要么调 restore_state_for_same_schema，要么与切换尝试共用条件"
            );
        }
    }

    /// ★ 本次真机故障的直接守卫：切方案的状态归位**不得**受 CapsLock 那个开关门控。
    ///
    /// 病理：`input.capslock.cancel_on_mode_switch` 出厂 `false`，而归位中文 + 取消大写
    /// 曾整个裹在 `if 该开关 {}` 里，于是出厂配置下英文半角态/大写态按方案直达热键，
    /// 方案真的换了、`schema.active` 也写了盘，用户却观察不到任何变化——那两种状态下
    /// 按键根本不进引擎（英文半角在 `handle_key_event` 的分水岭原样透传，大写被 C++ 的
    /// `capsLockLetterPassthrough` 同步透传）。用户报障原话即「热键在英文状态或大写状态
    /// 不生效」。
    ///
    /// 判据：**一个动作的语义前提不可配置，可配置的只能是副作用。**「我要用这个方案打字」
    /// 是切方案的语义前提；那个开关的正当作用域只有切中英模式（那里用户可能正想打大写英文）。
    #[test]
    fn finish_user_schema_switch_is_not_gated_by_capslock_option() {
        let body = body_of("finish_user_schema_switch");
        assert!(
            !body.contains("cancel_on_mode_switch"),
            "切方案的归位不得读 input.capslock.cancel_on_mode_switch——它出厂关闭，\
             一读就等于「英文态/大写态下方案切换毫无反应」"
        );
        assert!(
            !body.contains("cancel_caps_on_switch"),
            "切方案取消大写要走无条件的 force_cancel_caps_lock；cancel_caps_on_switch 带开关判定"
        );
        assert!(
            body.contains("force_cancel_caps_lock"),
            "大写开着时不取消，切完方案照样打大写英文——用户看到的仍是「切换不生效」"
        );
        assert!(
            body.contains("state.chinese_mode = true"),
            "英文半角态不归位中文，按键全程原样透传，新方案根本不参与出字"
        );
    }

    /// 三个方案切换入口（托盘 / 直达热键 / 循环键）必须共用同一条收尾。
    ///
    /// 这三处历史上各自漂移过（持久化、归位中文、取消大写、清 preedit 四件事的组合各不
    /// 相同），本次故障的可见形态正是「托盘切得动、热键切不动」——托盘那份手写收尾是
    /// **无条件**归位中文的，热键那份受开关门控。合并之后同一个 bug 不会再只修一半。
    #[test]
    fn all_schema_switch_entries_share_one_finish() {
        let body = body_of("select_schema");
        assert!(
            body.contains("finish_user_schema_switch"),
            "托盘选方案必须走统一收尾，不得再手写一份"
        );
        assert!(
            !body.contains("chinese_mode = true"),
            "手写归位＝又一份会漂移的收尾；归位属于 finish_user_schema_switch"
        );
        for name in ["switch_schema_by_id", "cycle_schema"] {
            assert!(
                body_of(name).contains("finish_user_schema_switch"),
                "{name} 也必须走统一收尾"
            );
        }
    }
}
