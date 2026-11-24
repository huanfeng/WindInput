//! 方案级行为覆盖（`[punct]` / `[candidate]` / `[phrases]`）的代际同步点。
//!
//! 规则：方案意图是默认值；用户在该方案期间手动改的值胜出，但只在当前代际内有效。
//! 设计与取舍见 `docs/design/schema-scoped-behavior.md` §4。
//!
//! 用代际而非「切方案时设一次」：切方案路径有多条（含启动载入），命令式写法漏一条即永久失效；
//! 惰性同步漏调一处只是晚一拍，下一次按键必经 `handle_key_event`。
//! `schema_generation` 不能当 `invalidate_schema` 的失效判据（设置页改 `schema_overrides`
//! 不 bump 代际，见 `key_resolver.rs` 头部），但本处要的正是「活跃方案变了」。
//!
//! 标点走惰性同步写 `state.chinese_punct`，而不是纯声明式：它被语言栏、工具栏、
//! `convert_punct`、智能符号快照等多处直接读取。标点不需要 `punct_manual`：代际未变时
//! [`Coordinator::sync_schema_scope`] 直接返回，本就不会覆盖用户刚 toggle 的值；布局每次显示
//! 都重算，才需要显式的 [`State::layout_manual`]。

use crate::coordinator::{Coordinator, State};

impl Coordinator {
    /// 活跃方案代际变化时，把方案级意图落到运行时状态。幂等：代际未变即刻返回。
    ///
    /// 调用点（漏一个只是晚一拍）：
    /// - `handle_key_event` 入口——兜底，保证最迟下一次按键必然收敛；
    /// - `push_state_update` / `show_status`——让工具栏与状态泡当场反映新方案的标点态；
    /// - `apply_ui_config`（热重载）。
    pub(crate) fn sync_schema_scope(&self, state: &mut State) {
        let generation = self.engine_mgr.schema_generation();
        if state.schema_scope_gen == generation {
            return;
        }
        state.schema_scope_gen = generation;
        // 方案级通用 UI 覆盖（如 per_page）随方案变化，依赖它的下发项要重发。
        self.send_candidate_min_size();
        // 布局手动值随代际失效。标点态不需要对应动作，理由见模块文档最后一节。
        state.layout_manual = None;
        // 单字输入的临时态随代际失效。它本身是 `Option`，清成 `None` 即回落配置层，
        // 不需要 `punct_before_schema` 那种保存原值的配套。
        state.single_char_override = None;
        // 引号交替态随代际归位：`PunctuationConverter` 跨方案共享，而左右形按方案取，
        // 不归位的话切过去第一次按引号可能拿到右形。锁序 state → punct 与 `convert_punct` 一致。
        self.punct.lock().unwrap_or_else(|e| e.into_inner()).reset();
        match self.engine_mgr.active_behavior().punct.resolve() {
            Some(v) => {
                // 首次被覆盖时记原值；已是 `Some` 就不覆写，在有意图的方案间切换时
                // 要还原的仍是最初那个全局态。
                if state.punct_before_schema.is_none() {
                    state.punct_before_schema = Some(state.chinese_punct);
                }
                state.chinese_punct = v;
            }
            None => {
                // `Follow` 是「回到不受方案影响的那个值」，不是保持现状，所以不能什么都不做。
                // `take` 同时清标记，连续切换不会越还越旧。
                if let Some(v) = state.punct_before_schema.take() {
                    state.chinese_punct = v;
                }
            }
        }
    }

    /// 「非用户手动」的入口要改标点态时走这里：活跃方案声明了 `[punct]` 意图就以意图为准
    /// （范围更小的意图优先）。适用于 `follow_mode` 的中英跟随、IME 激活时重置全局默认；
    /// 用户手动 `toggle_punct` 不走这里。
    ///
    /// 先 sync 再写：否则在「代际已变、还没 sync」的窗口里，随后的 sync 会把本入口写的值记成
    /// `punct_before_schema` 基线。有意图时 `value` 记进基线，切回 Follow 方案时还原到它。
    ///
    /// 调用方须已持 `state` 锁。
    pub(crate) fn set_punct_below_schema_intent(&self, state: &mut State, value: bool) {
        self.sync_schema_scope(state);
        match self.engine_mgr.active_behavior().punct.resolve() {
            Some(intent) => {
                state.punct_before_schema = Some(value);
                state.chinese_punct = intent;
            }
            None => state.chinese_punct = value,
        }
    }

    /// 不持 state 锁的调用点用的包装。
    ///
    /// 调用方必须没有持有 `self.state` 锁（不可重入）。
    pub(crate) fn sync_schema_scope_locked(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.sync_schema_scope(&mut state);
    }

    /// 当前输入语境的短语加载规格（`[phrases]` 段）。
    ///
    /// 归属取 `effective_data_schema`，与词频读写、右键菜单同源，临时英文因此按 `english`
    /// 方案的 `[phrases]` 走。不能改用 `overlay_engine_schema`：它在 `show_candidates = false`
    /// 时返回 `None`（回答的是「要不要出候选」），用户一关候选显示短语作用域就静默换回主方案。
    pub(crate) fn phrase_spec_of(&self, state: &State) -> std::sync::Arc<wind_config::PhrasesSpec> {
        let id = self
            .effective_data_schema(state)
            .unwrap_or_else(|| self.engine_mgr.active_schema_id());
        let behavior = self.engine_mgr.behavior_for(&id);
        // `behavior_for` 返回整段快照的 Arc；这里只要 `[phrases]` 那一段，克隆一次
        // （两个 Vec，通常都是空的）好过让调用方拿着整段。
        std::sync::Arc::new(behavior.phrases.clone())
    }

    /// 当前方案声明的候选布局意图（`[candidate] layout`）。
    pub(crate) fn schema_layout_intent(&self) -> wind_config::LayoutIntent {
        self.engine_mgr.active_behavior().candidate_layout
    }

    /// 当前语境的候选字族（测试/诊断用）。对齐 `debug_desired_vertical` 的既有形态。
    pub fn debug_candidate_font(&self) -> String {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.candidate_font_of(&state)
    }

    /// 当前生效的单字输入状态（测试/诊断用）。三层折叠后的结果，见
    /// [`Coordinator::effective_single_char`]。
    pub fn debug_effective_single_char(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.effective_single_char(&state)
    }

    /// 配置层（不含临时态）里的单字输入（测试/诊断用）。取码表那一份
    /// （`schema.codetable.single_char` ⊕ 方案级覆盖），相关用例都跑在码表方案下。
    ///
    /// 与 [`Self::debug_effective_single_char`] 分开暴露：「热键切换不写配置」只有同时问得到
    /// 生效值与配置值才断言得了。
    pub fn debug_config_single_char(&self) -> bool {
        self.engine_mgr.codetable_settings().single_char
    }

    /// 此刻有没有单字输入的临时态（测试/诊断用）。
    pub fn debug_has_single_char_override(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .single_char_override
            .is_some()
    }

    /// 当前输入语境的候选文字字族覆盖（`[candidate] font_family`）；空 = 不覆盖。
    ///
    /// 与同段的 `layout` 归属判据刻意不同：`layout` 是呈现态，取 `active_behavior`；字体是数据
    /// 属性，取 `effective_data_schema`（与 `[phrases]`、`[punct] custom_mappings` 同源），
    /// 否则临英期间字体仍跟着五笔走。它逐次按键变化，不能挂到代际驱动的 `sync_schema_scope`
    /// 上（临英进出不改代际），下发点见 `Coordinator::sync_candidate_font`。
    pub(crate) fn candidate_font_of(&self, state: &State) -> String {
        let id = self
            .effective_data_schema(state)
            .unwrap_or_else(|| self.engine_mgr.active_schema_id());
        self.engine_mgr
            .behavior_for(&id)
            .candidate_font_family
            .clone()
    }

    /// 当前输入语境的横排时文字排列（`[candidate] text_orientation`）。
    ///
    /// 与候选字体同源，取 [`Coordinator::effective_data_schema`]，临英期间不会按蒙古文方案
    /// 转 90°；与同段 `layout` 判据不同，理由同 [`Self::candidate_font_of`]。逐次按键变化，
    /// 下发点在每次显示都重算的 `desired_orientation` 里。
    pub(crate) fn candidate_text_orientation_of(
        &self,
        state: &State,
    ) -> wind_config::TextOrientation {
        let id = self
            .effective_data_schema(state)
            .unwrap_or_else(|| self.engine_mgr.active_schema_id());
        self.engine_mgr.behavior_for(&id).candidate_text_orientation
    }

    /// 当前输入语境是否只出单字。取值的唯一入口。
    ///
    /// | 层 | 来源 |
    /// |---|---|
    /// | ⓪ 引擎不变量 | 英文/未知引擎恒关，压过临时态 |
    /// | ① 临时态 | `State::single_char_override`（热键切的），压过配置层 |
    /// | ② 全局 + 方案级 | 按当前引擎分流，见下表 |
    ///
    /// | 引擎 | 取值 | 方案级覆盖 |
    /// |---|---|---|
    /// | 码表 | `schema.codetable.single_char` | `[engine.codetable] single_char` |
    /// | 混输 | 同上（`codetable_settings` 自己解析到 `primary_schema`） | 继承主码表方案 |
    /// | 拼音 | `schema.pinyin.single_char` | 无（刻意） |
    /// | 英文等 | 恒关 | — |
    ///
    /// 码表与拼音分两份：码表开单字是为了定长盲打，拼音开了只是「不出词」，分开后「五笔只出
    /// 单字、拼音照常出词」在全局层就能表达。
    ///
    /// 英文引擎恒关不是遗漏：开了会把单词候选全滤光。它是不变量，必须问在临时态之前，否则英文
    /// 方案下按一次 `single_char:on` 词库候选整批消失。
    ///
    /// 只有主输入路（`update_candidates`）问这个函数；临拼 / 临英 / mix / 特殊模式各自装配
    /// 候选，不受本项管辖。逐次按键重算，不能挂到代际驱动的 `sync_schema_scope` 上。
    pub(crate) fn effective_single_char(&self, state: &State) -> bool {
        use wind_engine::engine::EngineType;
        // 英文/未知引擎恒关，先于临时态判。
        let engine = self.engine_mgr.current_engine_type();
        if !matches!(
            engine,
            Some(EngineType::CodeTable) | Some(EngineType::Mixed) | Some(EngineType::Pinyin)
        ) {
            return false;
        }
        if let Some(on) = state.single_char_override {
            return on;
        }
        match engine {
            // 码表与混输共用码表那一份。`codetable_settings()` 已经把「全局 ⊕ 方案级」
            // 折叠好，也已经替混输解析到 `primary_schema` ——两件事都不必在此重做。
            Some(EngineType::CodeTable) | Some(EngineType::Mixed) => {
                self.engine_mgr.codetable_settings().single_char
            }
            Some(EngineType::Pinyin) => self.rt().config.schema.pinyin.single_char,
            // 上面那道 `matches!` 已把其余引擎挡掉，这里只是穷尽性所需。
            _ => false,
        }
    }
}

/// 把 `[phrases]` 规格折成一次查询的作用域。
///
/// 单独一个自由函数而不是方法：`PhraseScope` 借用 spec 里的两个 `Vec`，调用点必须先把
/// spec 绑成局部变量再取 scope，方法形态反而藏不住这一步。
pub(crate) fn phrase_scope(spec: &wind_config::PhrasesSpec) -> wind_phrase::PhraseScope<'_> {
    wind_phrase::PhraseScope {
        enabled: spec.enabled,
        categories: &spec.categories,
        exclude: &spec.exclude_categories,
    }
}

#[cfg(test)]
mod tests {
    use wind_config::{LayoutIntent, PunctIntent};

    /// `PunctIntent::resolve` 的三态：`Follow` 必须是 `None`（不干预）而不是某个默认值。
    ///
    /// 若 `Follow` 返回 `Some(true)`，每次切方案都会把用户的英文标点态掰回中文。
    #[test]
    fn punct_intent_follow_means_no_write() {
        assert_eq!(PunctIntent::Follow.resolve(), None);
        assert_eq!(PunctIntent::Chinese.resolve(), Some(true));
        assert_eq!(PunctIntent::English.resolve(), Some(false));
    }

    /// 两个 `Follow` 是同一个概念的两处表达，取值词汇必须对得上。
    #[test]
    fn default_intents_are_follow() {
        assert_eq!(PunctIntent::default(), PunctIntent::Follow);
        assert_eq!(LayoutIntent::default(), LayoutIntent::Follow);
    }
}
