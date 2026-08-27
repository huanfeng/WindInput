//! 方案级行为覆盖（`[punct]` / `[candidate]` / `[phrases]`）的**代际同步点**。
//!
//! 设计见 `docs/design/schema-scoped-behavior.md` §4。
//!
//! # 规则
//!
//! > 方案意图是**默认值**；用户在该方案期间手动改的值胜出，但只在**当前代际**内有效。
//!
//! 切到英文方案 → 英文标点；用户按 `toggle_punct` → 中文标点（本代际内一直有效）；
//! 切到五笔 → 代际 +1，手动值自动失效；切回英文 → 又是英文标点。
//!
//! # 为什么是代际，不是「切方案时设一次」
//!
//! `finish_user_schema_switch` 自己的注释写明「本函数只覆盖五条切方案路径中的两条」，
//! 加上启动时载入 `schema.active` 一条都不走 ⇒ 命令式的「切方案时设一次」**必然漏接**，
//! 而漏接的表现是「配了没反应」。代际方案不需要枚举切换路径。
//!
//! ⚠️ 反向提醒：`schema_generation` **不能**当 `invalidate_schema` 的失效判据（设置页改
//! `schema_overrides` 不 bump 代际，见 `key_resolver.rs` 头部）。本处要的恰好是
//! 「活跃方案变了」这一个语义，用它是对的。
//!
//! # 为什么标点走「惰性同步写状态」而不是纯声明式
//!
//! 布局是「算出来再下发」的派生值，可以每次重算（见 [`crate::layout`]）；标点不是——
//! `state.chinese_punct` 被语言栏图标、工具栏、`convert_punct`、`active_pairs`、
//! `push_config`、智能符号的 press1 快照等七八处**直接读取**，全改成取值函数是一次大范围
//! 散射，且智能符号那处存的是**状态快照**，语义会歪。
//!
//! ★ 这个形态的关键性质：**漏调一个同步点的后果是「晚一拍」而不是「永不生效」**——
//! 下一次按键必然经过 `handle_key_event`。这正是它优于命令式写法的地方，也是可以接受
//! 「调用点不止一个」的理由。命令式写法漏一条路径就是永久失效。
//!
//! # 标点为什么**不需要**一个 `punct_manual` 字段
//!
//! 因为 `schema_scope_gen` 守卫本身就实现了「手动值在当前代际内胜出」：代际未变时
//! [`Coordinator::sync_schema_scope`] 直接返回，压根不会去覆盖用户刚 toggle 出来的值。
//! 布局则不同——它每次显示都重算，没有「不去覆盖」这一说，故需要显式的
//! [`State::layout_manual`]。

use crate::coordinator::{Coordinator, State};

impl Coordinator {
    /// 活跃方案代际变化时，把方案级意图落到运行时状态。**幂等**：代际未变即刻返回。
    ///
    /// 调用点（都幂等、可重复调，漏一个只是晚一拍）：
    /// - `handle_key_event` 入口——兜底，保证最迟下一次按键必然收敛；
    /// - `push_state_update` / `show_status`——让工具栏与状态泡当场反映新方案的标点态；
    /// - `apply_ui_config`（热重载）。
    pub(crate) fn sync_schema_scope(&self, state: &mut State) {
        let generation = self.engine_mgr.schema_generation();
        if state.schema_scope_gen == generation {
            return;
        }
        state.schema_scope_gen = generation;
        // 布局手动值随代际失效。标点态不需要对应动作，理由见模块文档最后一节。
        state.layout_manual = None;
        // 引号交替态随代际归位。`PunctuationConverter` 是 Coordinator 单例、跨方案共享，而
        // 左右形是**按方案取的**（方案 A 把 `"` 配成 `「」`、方案 B 用默认 `“”`）。不归位的话
        // 切过去第一次按引号可能直接拿到右形。
        //
        // 与「⛔ 进入时保存、退出时回放」那条否决无关：这里是无条件复位到左形，没有需要被记住
        // 的原值。锁序 state → punct 与 `convert_punct` 一致（调用方均已持 state 锁）。
        self.punct.lock().unwrap_or_else(|e| e.into_inner()).reset();
        match self.engine_mgr.active_behavior().punct.resolve() {
            Some(v) => {
                // 首次被方案意图覆盖时记下原值。已经是 `Some` 就不再覆写——从一个有意图的
                // 方案切到另一个有意图的方案时，要还原的仍是**最初那个**全局态，
                // 而不是上一个方案强加的值。
                if state.punct_before_schema.is_none() {
                    state.punct_before_schema = Some(state.chinese_punct);
                }
                state.chinese_punct = v;
            }
            None => {
                // 方案没意见：把被覆盖的值还回去。`take` 同时清掉标记——下次再进有意图的
                // 方案时重新记，故连续切换不会越还越旧。
                //
                // ⚠️ 这里**不能**写成「什么都不做」：那正是 2026-08-23 真机报的
                // 「从五笔切到英文标点变英文，切回五笔还是英文」——`Follow` 的语义是
                // 「回到不受方案影响的那个值」，而不是「保持上一个方案留下的值」。
                if let Some(v) = state.punct_before_schema.take() {
                    state.chinese_punct = v;
                }
            }
        }
    }

    /// 不持 state 锁的调用点用的包装。
    ///
    /// ⚠️ 调用方必须**没有**持有 `self.state` 锁——本仓的 state 锁不可重入。
    pub(crate) fn sync_schema_scope_locked(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.sync_schema_scope(&mut state);
    }

    /// 当前输入语境的**短语加载规格**（`[phrases]` 段）。
    ///
    /// # ★ 归属方案取 `effective_data_schema`，不是 active
    ///
    /// 它已经是词频读端（`apply_freq_rerank_in`）、写端（`record_selection_in`）与右键菜单
    /// （`candidate_op_scope`）三处共用的归属源 ⇒ 临时英文自动按 `english` 方案的
    /// `[phrases]` 走，不会出现「候选归 english 桶、短语归五笔」的错配。
    ///
    /// ⚠️ **绝不能改用 `overlay_engine_schema`**：它在 `show_candidates = false` 时返回
    /// `None`（它回答的是「要不要出候选」，不是「数据算谁的」）。拿它当归属，用户一关候选
    /// 显示，短语作用域就静默换回主方案。这条坑 2026-08-21 已经踩过一次。
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
    /// 返回 `Option` 是刻意的——调用方据此决定「要不要写」，而不是「写什么」。
    /// 若这里给 `Follow` 返回 `Some(true)`，每次切方案都会把用户的英文标点态强行掰回中文。
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
