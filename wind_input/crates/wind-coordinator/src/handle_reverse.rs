//! 反查模式（`ModeKind::Reverse`，docs/design/codetable-reverse-mode.md §3）。
//!
//! 同生僻字模式先例：缓冲 / 光标 / 退格 / 选词 / 退出复用 special 族
//! （`special_buffer` + `handle_special_key` + `exit_special_mode`），本文件只管
//! 「进入」与「候选怎么来」。候选走 `EngineManager::convert_reverse`（与行内通配同一个
//! `wildcard_query` 内核、不看 `wildcard` 主开关、只查主码表），再过常用字判定与检索范围
//! 过滤；**不**自动上屏、**不**做词频重排、**不**吃候选调整（通配串不是任何
//! 码位，读侧本就查不中）。

use crate::coordinator::{Coordinator, State};
use crate::pipeline::ModeKind;
use tracing::debug;
use wind_bridge::handler::KeyAction;
use wind_dict::WILDCARD_SLOT;
use wind_keys::keymap;

/// 反查缓冲 → 查询模式串：缓冲里**每一个**通配键（含首位）都换成槽位字符。
///
/// 与行内通配（`wildcard_pattern`）的差别正是「含首位、不看让位规则」——想在首位通配
/// 就进反查模式（spec §3.3）。缓冲里没有通配键时原样返回，引擎按字面等长 + 前缀查。
pub(crate) fn reverse_pattern(buffer: &str, key: char) -> String {
    buffer
        .chars()
        .map(|c| if c == key { WILDCARD_SLOT } else { c })
        .collect()
}

impl Coordinator {
    /// 反查模式此刻能不能进：活跃方案有反查通配键（码表 / 混输的主码表才有；拼音等引擎给
    /// `None`）。没有总开关——能不能触发只看有没有绑进入键，与临时拼音 / 临时英文 / 生僻字同。
    ///
    /// 门卫没过时调用方返回 `None` 让触发键落普通输入——绝不吞键（同 `enter_bound_action`）。
    pub(crate) fn reverse_mode_available(&self) -> bool {
        self.engine_mgr.active_reverse_key().is_some()
    }

    /// 反查动词在任一层绑了键：方案 `[key_actions]`、全局 `keys.key_actions`（单键与组合键）、`z_key_action`。
    /// 只给启动体检门控用（没绑就谈不上「模式内冲突」）；按键路径的门卫仍是 [`Self::reverse_mode_available`]。
    pub(crate) fn reverse_bound_anywhere(&self) -> bool {
        use wind_config::BoundAction;
        let any_rev = |m: &std::collections::BTreeMap<String, String>| {
            m.values()
                .any(|v| matches!(BoundAction::parse(v), BoundAction::Reverse))
        };
        any_rev(&self.engine_mgr.active_key_actions())
            || any_rev(&self.rt().config.keys.key_actions)
            || matches!(self.z_key_action(), BoundAction::Reverse)
    }

    /// 这个键是否绑了反查动词（方案 `[key_actions]` → 全局 → `z_key_action` 三层链）。
    ///
    /// 只查绑定，不含可用性（活跃方案有无反查通配键），与
    /// `is_temp_pinyin_trigger` 同口径；可用性门卫在 [`Self::reverse_mode_available`]。
    pub(crate) fn is_reverse_trigger(&self, key_code: u32) -> bool {
        matches!(
            self.bound_action_for(key_code),
            Some(wind_config::BoundAction::Reverse)
        )
    }

    /// 进入反查模式。字段组与 `enter_rare_char_mode` 同一组（本模式也没有宿主方案，
    /// `special_id` / `overlay_spec` 恒为默认值 ⇒ 布局与注释模板跟随全局或本模式配置）。
    pub(crate) fn enter_reverse_mode(&self, state: &mut State, key_code: u32) -> KeyAction {
        state.input_buffer.clear();
        state.candidates.clear();
        state.active = Some(ModeKind::Reverse);
        state.special_id = 0;
        state.overlay_spec = None;
        state.special_buffer.clear();
        state.special_cursor = 0;
        state.special_prefix = keymap::vk_to_prefix_char_with_letters(key_code)
            .map(|c| c.to_string())
            .unwrap_or_default();
        state.scope_relaxed = false;
        self.update_reverse_candidates(state);
        self.notify_ui_update(state);
        let display = state.preedit.clone();
        debug!("Entered reverse mode");
        self.reverse_entry_composition(key_code, display)
    }

    /// 顶掉当前半成品并进入反查模式（同 `commit_and_enter_rare_char_mode`）。
    pub(crate) fn commit_and_enter_reverse_mode(
        &self,
        state: &mut State,
        key_code: u32,
    ) -> KeyAction {
        if let Some(act) = self.top_commit_command_guard(state) {
            return act;
        }
        let committed = self.take_committed_with_highlight(state);
        let enter = self.enter_reverse_mode(state, key_code);
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

    /// 缓冲变了之后的候选刷新：视图复位 + 首批（`WILDCARD_INITIAL_LIMIT`，同行内通配 §11）。
    pub(crate) fn update_reverse_candidates(&self, state: &mut State) {
        self.reset_candidate_view(state);
        self.build_reverse_candidates(state, crate::wildcard::WILDCARD_INITIAL_LIMIT);
    }

    /// 按 `limit` 重建反查候选，**不动页码 / 高亮**（翻页扩充复用）；返回引擎条数。
    ///
    /// `has_more` 看引擎条数而非可见条数（spec codetable-wildcard §11 契约 3）：新增的一批
    /// 被检索范围整批滤掉时，可见条数不变而引擎后面仍可能有常用字。
    pub(crate) fn build_reverse_candidates(&self, state: &mut State, limit: usize) -> usize {
        state.candidates.clear();
        state.preedit = format!("{}{}", state.special_prefix, state.special_buffer);
        state.candidate_limit = limit;
        let key = match self.engine_mgr.active_reverse_key() {
            Some(k) if !state.special_buffer.is_empty() => k,
            _ => {
                state.has_more = false;
                return 0;
            }
        };
        let buffer = state.special_buffer.clone();
        let pattern = reverse_pattern(&buffer, key);
        let result = self
            .engine_mgr
            .convert_reverse(&buffer, &pattern, limit)
            .unwrap_or_default();
        let engine_count = result.candidates.len();
        // 词条内特殊语法（`$SS` / `$CC` / `{..}`）统一展开，同主路通配（`build_candidates`）。
        let mut cands = self.finalize_candidates(result.candidates, &buffer);
        self.mark_common(&mut cands);
        // 通配结果带 `is_wildcard`，§11 的整组常用字过滤在 `filter_candidates` 里自动成立。
        self.run_passes(
            state,
            &mut cands,
            crate::postprocess::profile::REVERSE,
            &crate::postprocess::PassCtx {
                owner: None,
                freq_code: None,
                shadow_code: None,
            },
        );
        state.candidates = cands;
        state.has_more = engine_count >= limit;
        engine_count
    }

    /// 反查模式的翻页扩充（`expand_candidates` 开头按 `active` 分流到这里）：判据与主路
    /// 逐条相同——`limit` ×2 封顶 `CANDIDATE_LIMIT_CAP`、重建后夹回页码 / 高亮、到底看
    /// **引擎条数**是否超过上一批（spec codetable-wildcard §11 契约 3）。
    ///
    /// 与主路的差别只在候选来源：主路 `build_candidates` 读 `input_buffer`，在本模式恒空，
    /// 走它会把反查候选整份清掉。组合区由 `build_reverse_candidates` 按缓冲原样重写
    /// （本模式组合区不随高亮变），故不调 `sync_preedit_to_highlight`；也不展开繁简变体
    /// （首批构建本就没展开，扩充须与之一致）。
    pub(crate) fn expand_reverse_candidates(&self, state: &mut State) {
        if !state.has_more {
            return;
        }
        let new_limit =
            (state.candidate_limit.saturating_mul(2)).min(wind_engine::engine::CANDIDATE_LIMIT_CAP);
        if new_limit <= state.candidate_limit {
            state.has_more = false;
            return;
        }
        let prev_limit = state.candidate_limit;
        let engine_count = self.build_reverse_candidates(state, new_limit);
        self.clamp_candidate_view(state);
        if engine_count <= prev_limit {
            // 引擎没给出新东西：到底。`build_reverse_candidates` 已把 limit 写成 new_limit，还原。
            state.has_more = false;
            state.candidate_limit = prev_limit;
        }
    }
}

#[cfg(test)]
#[path = "handle_reverse_tests.rs"]
mod tests;
