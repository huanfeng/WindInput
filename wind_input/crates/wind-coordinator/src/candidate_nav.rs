//! 候选视图导航：分页 / 高亮移动 / 悬停清除 / 末页检索范围临时放宽。
//!
//! （自 coordinator.rs 平移，纯搬运。统一分发入口 `apply_session_action` 仍在
//! coordinator.rs——它同时分发 cancel 等非导航动作。）

use crate::coordinator::{Coordinator, State};
use crate::pipeline::ModeKind;

impl Coordinator {
    /// 每页候选数（来自配置，至少 1）
    pub(crate) fn per_page(&self, active: Option<ModeKind>) -> usize {
        // 走 `eff_candidate`（全局 ⊕ 方案 `[candidate]` 补丁）而不是直接读全局配置：
        // `per_page` 是通用折叠键，绕过它方案里配了就不生效。扩展档不在方案级形态内。
        let cand = self.eff_candidate();
        // overlay 模式(临拼/快捷/短语/临英等,state.active 非空)用扩展档(配置>0 时)。
        //
        // 辅助码除外：它只是筛过一轮的主路径候选，呈现应与主路径一致（同
        // `layout::intent_for` 的 AuxCode 臂），否则按下触发键时每页条数会跳变。
        if active.is_some_and(|m| m != ModeKind::AuxCode) && cand.per_page_extended > 0 {
            cand.per_page_extended.max(1)
        } else {
            cand.per_page.max(1)
        }
    }

    /// 当前候选列表实际使用的每页条数：候选全是同一批 `$SS`/`$AA` 组成员（展开视图）且配了
    /// `input.cmdbar.group_per_page`(>0) 时用它，否则同 [`Self::per_page`]。
    /// 混有非成员候选时一页内不能两种页长，仍按 `per_page`。
    pub(crate) fn page_size(&self, state: &State) -> usize {
        let gp = self.rt().config.input.cmdbar.group_per_page;
        if gp > 0
            && !state.candidates.is_empty()
            && state
                .candidates
                .iter()
                .all(|c| !c.is_group && !c.group_template.is_empty())
        {
            return gp;
        }
        self.per_page(state.active)
    }

    /// 总页数（至少 1）
    pub(crate) fn total_pages(&self, state: &State) -> usize {
        let pp = self.page_size(state);
        state.candidates.len().div_ceil(pp).max(1)
    }

    /// 清除鼠标悬停目标（无需 state 锁，见 [`Coordinator::hover_index`] 的说明）。
    ///
    /// 调用点＝一切「悬停不再对应屏幕上任何东西」的时刻：候选窗隐藏、重新装填、键盘移动
    /// 高亮/翻页。少接一处，悬停高亮与 tooltip 会在下次候选窗出现时凭空复现。
    pub(crate) fn clear_hover(&self) {
        self.hover_index
            .store(-1, std::sync::atomic::Ordering::Relaxed);
    }

    /// 当前鼠标悬停目标（原始 tag；-1 = 无）。
    pub(crate) fn hover_target(&self) -> i32 {
        self.hover_index.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 候选列表重新装填 / 组合清空后的**视图复位**：翻页归零、键盘高亮归零、鼠标悬停清除。
    ///
    /// 三件事必须一起做，所有装填点都走本函数。漏清悬停在主路径上不可观测（每键都会覆盖），
    /// 只在 overlay 模式露馅。
    pub(crate) fn reset_candidate_view(&self, state: &mut State) {
        state.current_page = 0;
        // 翻页史随候选一起作废：新装填的是另一批候选，上一批翻到过第几页与它无关。
        // 挂在这里而不是各装填点，理由同上面那三件——`current_page` 归零的地方就是它
        // 该归零的地方。生产代码里 `current_page` 的写点全在本文件，别处的都是测试。
        state.paged = false;
        state.selected_index = 0;
        // 展开态随候选一起作废：新装填的是另一批候选，网格从单行重新开始（不记忆展开状态）。
        state.grid_top = None;
        state.grid_tab = wind_ui_types::GridTab::All;
        state.grid_full.clear();
        state.grid_tab_anchor = None;
        self.clear_hover();
    }

    /// 候选列表**原地重建**（不复位视图）后，把页码 / 页内高亮夹回新列表的范围内。
    ///
    /// 扩容（`expand_candidates`）保持当前页/高亮，但重建后的可见列表可能变短（智能档下新一批
    /// 带来同组常用字，整组生僻字被滤走）。不夹的话按页切片会越界。
    ///
    /// 夹页不是翻页：不动 `paged`（页码只降不升，见 `turn_page` 文档里「往下降」那段）。
    /// 真夹了才清悬停——悬停下标指向的那一页已经不是屏幕上这页了。
    pub(crate) fn clamp_candidate_view(&self, state: &mut State) {
        let last_page = self.total_pages(state) - 1;
        let mut changed = false;
        if state.current_page > last_page {
            state.current_page = last_page;
            changed = true;
        }
        let (s, e) = self.page_range(state);
        let max_sel = (e - s).saturating_sub(1);
        if state.selected_index > max_sel {
            state.selected_index = max_sel;
            changed = true;
        }
        if changed {
            self.clear_hover();
        }
        // 页码被夹低了，网格可见区也要跟着收回来，否则高亮行落在可见区外。
        self.grid_follow(state);
    }

    /// 翻到第 `page` 页并记下「这批候选被翻过页」。
    ///
    /// `current_page` 的所有翻页写点都走这里，保证 `paged` 与之同步（漏掉会让 `-` 在翻过页后
    /// 又变回字符）。不含 `try_relax_scope_on_page_end` 里还原 `page_before` 那处：还原不是翻页。
    ///
    /// 往下降的两处（`move_up` 回卷 / `page_prev`）只是重复置位，删掉也不会有用例红；它们照样
    /// 走这里，是为了判据不按写点分叉，不是漏测。
    pub(crate) fn turn_page(state: &mut State, page: usize) {
        state.current_page = page;
        state.paged = true;
    }

    /// 上移高亮（页首回卷到上一页末项）；返回是否变化
    pub(crate) fn move_up(&self, state: &mut State) -> bool {
        self.clear_hover();
        if state.candidates.is_empty() {
            return false;
        }
        if state.selected_index > 0 {
            state.selected_index -= 1;
        } else if state.current_page > 0 {
            Self::turn_page(state, state.current_page - 1);
            let (s, e) = self.page_range(state);
            state.selected_index = e - s - 1;
        } else {
            return false;
        }
        true
    }

    /// 下移高亮（页尾回卷到下一页首项）；返回是否变化
    pub(crate) fn move_down(&self, state: &mut State) -> bool {
        self.clear_hover();
        if state.candidates.is_empty() {
            return false;
        }
        // 接近末页且有更多 → 先动态扩展加载
        if state.has_more && state.current_page + 2 >= self.total_pages(state) {
            self.expand_candidates(state);
        }
        let (s, e) = self.page_range(state);
        let page_count = e - s;
        if state.selected_index + 1 < page_count {
            state.selected_index += 1;
        } else if state.current_page + 1 < self.total_pages(state) {
            Self::turn_page(state, state.current_page + 1);
            state.selected_index = 0;
        } else {
            return false;
        }
        true
    }

    /// 上一页（高亮归零）；返回是否变化
    pub(crate) fn page_prev(&self, state: &mut State) -> bool {
        self.clear_hover();
        if state.current_page > 0 {
            Self::turn_page(state, state.current_page - 1);
            state.selected_index = 0;
            true
        } else {
            false
        }
    }

    /// 下一页（高亮归零）；返回是否变化
    pub(crate) fn page_next(&self, state: &mut State) -> bool {
        self.clear_hover();
        // 接近末页且有更多 → 先动态扩展加载，使新页可达
        if state.has_more && state.current_page + 2 >= self.total_pages(state) {
            self.expand_candidates(state);
        }
        if state.current_page + 1 < self.total_pages(state) {
            Self::turn_page(state, state.current_page + 1);
            state.selected_index = 0;
            true
        } else {
            // 已在末页仍按向后翻页 ⇒「翻到底了还想看更多」＝明确的放宽意图。
            self.try_relax_scope_on_page_end(state)
        }
    }

    /// 组合结束（输入缓冲已空）时让临时放宽失效，恢复配置的检索范围档位。
    ///
    /// 判据取「缓冲是否为空」而非「是否发生了上屏」：上屏、ESC 取消、切焦点清空、模式切换
    /// 都会清空缓冲，一个判据全覆盖，无需逐路径接线。放宽期间敲字母/退格/翻页时缓冲非空，
    /// 状态得以保持——找生僻字常要改几次编码。
    pub(crate) fn expire_scope_override(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s.scope_relaxed {
            return;
        }
        // 判据是「当前模式的」输入缓冲已空：临拼的码在 `temp_pinyin_buffer`、反查在
        // `special_buffer`，此时 `input_buffer` 恒为空，一刀切会让放宽在下一键就被静默清掉。
        let ended = match s.active {
            Some(ModeKind::TempPinyin) => s.temp_pinyin_buffer.is_empty(),
            Some(ModeKind::Reverse) => s.special_buffer.is_empty(),
            None
            | Some(
                ModeKind::TempEnglish
                | ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::Special(_)
                | ModeKind::RareChar
                | ModeKind::Mix(_)
                | ModeKind::AuxCode,
            ) => s.input_buffer.is_empty(),
        };
        if ended {
            s.scope_relaxed = false;
        }
    }

    /// 末页再按向后翻页键 → 临时放宽检索范围为「全部字符」，重建候选并翻到新增的那页。
    ///
    /// 设计见 `docs/design/smart-filter-scope-relax.md` §5。这是三类引擎**通用的主入口**：
    /// 码表候选少、翻两下就到底；拼音候选多，但用户找生僻字本就会一路翻页，翻到底同样是
    /// 明确信号。挂在既有的「翻不动就返回 false」分支上，不占任何键位。
    ///
    /// 返回是否真的发生变化（上层据此决定重绘）。放宽后若没有新增候选则**撤销**，
    /// 避免留下一个什么也没带来、却会影响后续按键的放宽态。
    pub(crate) fn try_relax_scope_on_page_end(&self, state: &mut State) -> bool {
        if !self.rt().config.input.scope_relax.page_end_key {
            return false;
        }
        // 已放宽过就不再重复。放宽是智能档专属的补偿：常用字档要的就是稳定只出常用字，
        // `Gb18030` 本就不过滤。
        if state.scope_relaxed || state.filter_mode != wind_candidate::FilterMode::Smart {
            return false;
        }
        // 辅助码模式：放宽是智能档「常用字」的补偿，辅助码按字形筛、不适用——且放宽会重建
        // 整池候选、绕过辅助码筛选（过滤结果丢失），跳过。
        if matches!(state.active, Some(ModeKind::AuxCode)) {
            return false;
        }
        // 临拼的码在 `temp_pinyin_buffer`，主路径的在 `input_buffer`——须按当前模式取。
        // 用 `input_buffer` 一刀切会让临拼**永远触发不了**（那边恒为空），且没有任何报错。
        // 反查模式同理，码在 `special_buffer`。
        let in_temp = matches!(state.active, Some(ModeKind::TempPinyin));
        let in_reverse = matches!(state.active, Some(ModeKind::Reverse));
        let has_input = if in_temp {
            !state.temp_pinyin_buffer.is_empty()
        } else if in_reverse {
            !state.special_buffer.is_empty()
        } else {
            !state.input_buffer.is_empty()
        };
        if !has_input {
            return false;
        }
        state.scope_relaxed = true;
        // 两条路径的候选重建函数不同：临拼走 overlay 的那套（主路径的 `build_candidates`
        // 读 `input_buffer`，在临拼下会构建出空列表）。
        // `paged` 与 `current_page` 一起存还：`update_temp_pinyin_candidates` 会经
        // `reset_candidate_view` 清掉 `paged`，只还原页码会破坏两者同步的不变式。
        let page_before = state.current_page;
        let paged_before = state.paged;
        // 网格滚动位置同理：重建会经 `reset_candidate_view` 收起网格，而用户此刻正在展开态里
        // 往下翻，收起就等于把他踢回单行。
        let grid_before = state.grid_top;
        // 列号同理：重建会把高亮归零，放宽若没放出东西就原样返回，屏幕上的高亮与空格的上屏
        // 目标从此对不上。
        let selected_before = state.selected_index;
        // 分类筛选同理：临拼那支的重建会经视图复位把分类清掉，主路那支则把完整列表写回
        // `candidates`。两种都在下面重套一次。
        let tab_before = state.grid_tab;
        if in_temp {
            // `update_temp_pinyin_candidates` 会把 current_page/selected_index 归零，
            // 重建后须还原，否则用户翻到的位置丢失。
            self.update_temp_pinyin_candidates(state);
            state.current_page = page_before;
            state.paged = paged_before;
            state.grid_top = grid_before;
            state.selected_index = selected_before;
        } else if in_reverse {
            // 反查的候选另有构建函数（主路的读 `input_buffer`，在本模式恒空）。它本身不动
            // 页码 / 高亮，这里仍同临拼那支还原，免得哪天它内部改成复位视图时静默丢位置。
            let limit = state.candidate_limit;
            self.build_reverse_candidates(state, limit);
            state.current_page = page_before;
            state.paged = paged_before;
            state.grid_top = grid_before;
            state.selected_index = selected_before;
        } else {
            let limit = state.candidate_limit;
            self.build_candidates_at_cursor(state, limit);
        }
        // 判据取「列表里有没有真的出现被滤候选」，而非「总数是否变多」——候选受 limit 截断时
        // 总数可能不变，那样会误判成「没放出东西」而撤销。
        // 要在**完整**列表上判（重套分类筛选之前）：放出来的字不一定落在当前分类里。
        let released = state.candidates.iter().any(|c| c.is_scope_filtered);
        state.grid_tab = tab_before;
        self.grid_reapply_filter(state);
        if !released {
            // 该码位本就没有被滤的字 → 原样撤销，别留一个什么也没带来、却会影响后续按键的放宽态
            state.scope_relaxed = false;
            return false;
        }
        // 放宽出来的候选追加在末尾，照常翻到下一页就能看到；按真实顺序插入会让视口跳回页首。
        if state.current_page + 1 < self.total_pages(state) {
            Self::turn_page(state, state.current_page + 1);
            state.selected_index = 0;
        }
        true
    }
}
