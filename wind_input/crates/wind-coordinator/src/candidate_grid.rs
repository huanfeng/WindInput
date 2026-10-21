//! 候选展开态（多行网格 / 卷轴）：进入与收起、二维移动、按屏滚动、平铺下标换算。
//!
//! 设计见 `docs/design/candidate-grid-expand.md`。整个功能立在一个等式上：
//! **网格的一行就是一页**——高亮行就是 `current_page`，高亮列就是 `selected_index`。
//!
//! 所以页模型的一切原样适用、一行不改：数字键 / `;'` / Alt、Ctrl+数字作用于高亮行，空格上屏
//! 高亮项，扩容与末页放宽照常从翻页那条路触发。这里新增的只有两样东西：
//! - **滚动位置** `State::grid_top`（可见区第一行对应的页）；
//! - **二维导航**：↑↓ 换行保持列号、←→ 线性移动（越过行尾折到下一行，正是既有的
//!   `move_up` / `move_down`）、翻页键逐行滚（PageUp/PageDown 可翻一整屏）、Esc 收起。
//!
//! # 下标
//!
//! 展开态下发给 UI 的是可见区内逐行平铺的候选，命中矩形的 tag 是平铺下标。宿主（Windows
//! 候选窗、host-render、macOS、Linux）都把它原样回传、不解释，换算回「哪一页、页内第几个」
//! 只在这边做（[`Coordinator::grid_cell`]）。

use crate::coordinator::{Coordinator, State};
use crate::pipeline::ModeKind;
use wind_bridge::handler::KeyEventData;
use wind_candidate::Candidate;
use wind_ipc::protocol::{MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;
use wind_ui_types::{GRID_TAB_TAG_BASE, GridTab};

/// 调位位置（见 [`Coordinator::reorder_positions`]），均为完整候选列表里的下标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReorderPositions {
    /// 本条。
    pub(crate) here: usize,
    /// 前移的目标。
    pub(crate) prev: usize,
    /// 后移的目标。
    pub(crate) next: usize,
    /// 末位。
    pub(crate) last: usize,
}

/// 连续切分类时锚定的位置：用户最初高亮的词与它所在的可见行。
///
/// 只拿当前高亮当锚点会漂：高亮的词组在「单字」里没有，落到第一个单字；再切「词组」锚的就是
/// 这个单字，又落到第一个词组；切回「全部」便停在这个词组上，不是用户原来那个。故切换之间
/// 高亮没被挪过（仍是 `landed`）就沿用最初的锚点。
pub(crate) struct GridTabAnchor {
    text: String,
    row: usize,
    landed: Option<String>,
}

/// 展开态在原地重建前后要保住的位置，见 [`Coordinator::grid_snapshot`]。
pub(crate) struct GridSnapshot {
    page: usize,
    selected: usize,
    tab: GridTab,
}

impl Coordinator {
    /// 展开态的生效可见行数。
    pub(crate) fn grid_rows(&self) -> usize {
        self.rt().config.ui.candidate.grid_rows()
    }

    /// 此刻是否处于展开态：有滚动位置**且有候选**。
    ///
    /// ★ 只看 `grid_top` 不够：上屏 / 回车 / 标点顶屏这一批出口只清候选、不经
    /// `reset_candidate_view`，`grid_top` 会留到下一次装填。展开态专属键要是只认它，组合
    /// 结束后在编辑器里按 ←→ / Esc 都会被吞（Linux / macOS 每个键都进协调器）。下一次装填
    /// 必经 `reset_candidate_view`，残值不会被新候选继承，所以判据收在这里即可。
    pub(crate) fn grid_active(state: &State) -> bool {
        state.grid_top.is_some() && !state.candidates.is_empty()
    }

    /// 当前这批候选能不能展开。
    ///
    /// 只在**进入**时判一次：展开之后条件若变了（比如鼠标点了别的东西），收起交给
    /// `reset_candidate_view` 那条统一出口——候选一重装就收。
    pub(crate) fn grid_eligible(&self, state: &State) -> bool {
        if !self.rt().config.ui.candidate.grid_expand {
            return false;
        }
        if state.candidates.is_empty() || state.assoc_active() {
            return false;
        }
        // 范围见设计 §9：快捷输入的数字透镜与格式表依赖单页、辅助码本身就在缩小候选、
        // 特殊模式各有自己的码表语义，都不展开。
        if !matches!(
            state.active,
            None | Some(ModeKind::TempPinyin) | Some(ModeKind::TempEnglish)
        ) {
            return false;
        }
        // 没有第二页（已加载的或还能加载的）就没什么可展开的。
        if self.total_pages(state) < 2 && !state.has_more {
            return false;
        }
        // 只做横排：竖排 / 旋转态的自然形态是「多列并排」，另立项。
        let o = self.desired_orientation(state);
        if o.vertical || o.text != wind_config::TextOrientation::Normal {
            return false;
        }
        // 宿主自绘候选（TSF UI-less）或候选窗被压住时，本地根本不弹窗，展开无从谈起。
        if self.ui_suppressed_by_host().is_some() || self.candidate_window_hidden() {
            return false;
        }
        true
    }

    /// 收起态按翻页键：**只展开、不翻页**（高亮原地不动），下面几页直接出现在视野里。
    ///
    /// 不顺手翻到第 2 页，是因为展开后第 2 页本来就在眼前；翻过去等于让高亮跳过用户
    /// 正要看的那一行（威注音同样如此）。返回是否展开了。
    pub(crate) fn try_expand_grid(&self, state: &mut State) -> bool {
        if Self::grid_active(state) || !self.grid_eligible(state) {
            return false;
        }
        state.grid_top = Some(state.current_page);
        self.clear_hover();
        self.grid_ensure_loaded(state);
        // 「还能加载」只是可能：扩容、去重、过滤之后仍不足两页，就别展开成一行真候选加一屏
        // 占位行。
        if self.total_pages(state) < 2 {
            state.grid_top = None;
            return false;
        }
        state.grid_keep = true;
        // 在靠后的页上展开（收起态已经翻到了末页附近）：可见区往前让，凑满一屏，
        // 别只露出一行真候选、下面全是占位行。
        let full_top = self.total_pages(state).saturating_sub(self.grid_rows());
        state.grid_top = Some(state.current_page.min(full_top));
        true
    }

    /// 候选原地重建（输入没变：置顶 / 调位 / 删除）之前记下展开态的位置。
    pub(crate) fn grid_snapshot(state: &State) -> Option<GridSnapshot> {
        Self::grid_active(state).then_some(GridSnapshot {
            page: state.current_page,
            selected: state.selected_index,
            tab: state.grid_tab,
        })
    }

    /// 原地重建之后回到重建前的那一行、那一列和那个分类。重建经 `update_candidates` 已按
    /// `grid_keep` 从首行展开回来，这里只把位置挪回去（夹到新列表的范围内）。
    pub(crate) fn grid_restore(&self, state: &mut State, snap: GridSnapshot) {
        if !Self::grid_active(state) {
            return;
        }
        state.grid_tab = snap.tab;
        self.grid_reapply_filter(state);
        state.current_page = snap.page;
        state.selected_index = snap.selected;
        self.clamp_candidate_view(state);
        self.grid_follow(state);
    }

    /// 候选重新装填之后：本次组合展开过就再展开（分步上屏、继续打字都不打回单行）。
    ///
    /// 新候选不够两页时 `try_expand_grid` 不展开，`grid_keep` 留着，下一次够了再展开。
    pub(crate) fn grid_resume(&self, state: &mut State) {
        if state.grid_keep {
            self.try_expand_grid(state);
        }
    }

    /// 收起回单行。返回是否真的收起了（本就没展开 → `false`）。
    pub(crate) fn collapse_grid(&self, state: &mut State) -> bool {
        state.grid_keep = false;
        if state.grid_top.take().is_none() {
            return false;
        }
        // 悬停下标是平铺下标，收起后对应的已经是另一个格子。
        self.clear_hover();
        true
    }

    /// 让高亮行留在可见区内。页码变动之后都要调（换行、滚屏、线性移动折行、夹页）。
    pub(crate) fn grid_follow(&self, state: &mut State) {
        let Some(top) = state.grid_top else {
            return;
        };
        let rows = self.grid_rows();
        let cur = state.current_page;
        let top = if cur < top {
            cur
        } else if cur >= top + rows {
            cur + 1 - rows
        } else {
            top
        };
        state.grid_top = Some(top);
    }

    /// 可见区之后再预留一屏，不够就向引擎扩容。
    ///
    /// 扩容复用 `expand_candidates`（limit 翻倍、整份重建、保持页码与高亮）——与收起态
    /// `page_next` 「接近末页先扩容」是同一个动作，只是展开态一屏能看见好几页，要提前更多。
    fn grid_ensure_loaded(&self, state: &mut State) {
        let Some(top) = state.grid_top else {
            return;
        };
        if state.has_more && top + 2 * self.grid_rows() >= self.total_pages(state) {
            self.expand_candidates(state);
        }
    }

    /// 换行后把高亮落到第 `col` 列；目标行较短（末页）时落到行尾。
    fn grid_land_column(&self, state: &mut State, col: usize) {
        let (s, e) = self.page_range(state);
        state.selected_index = col.min((e - s).saturating_sub(1));
    }

    /// ↑↓：换行并保持列号。在第一页那一行按 ↑ 收起。返回是否有变化。
    pub(crate) fn grid_move_row(&self, state: &mut State, down: bool) -> bool {
        self.clear_hover();
        let col = state.selected_index;
        if down {
            self.grid_ensure_loaded(state);
            if state.current_page + 1 >= self.total_pages(state) {
                return false;
            }
            Self::turn_page(state, state.current_page + 1);
        } else if state.current_page == 0 {
            return self.collapse_grid(state);
        } else {
            Self::turn_page(state, state.current_page - 1);
        }
        self.grid_land_column(state, col);
        self.grid_follow(state);
        true
    }

    /// 展开态的翻页键：PageUp/PageDown 在 `grid_pageupdown_screen` 开着时翻一整屏，
    /// 其余翻页键逐行滚（同滚轮）。只看物理键，不管它绑在哪个组。
    pub(crate) fn grid_page_key(&self, state: &mut State, down: bool, key_code: u32) -> bool {
        let screen = matches!(key_code, keymap::VK_PRIOR | keymap::VK_NEXT)
            && self.rt().config.ui.candidate.grid_pageupdown_screen;
        if screen {
            self.grid_page(state, down)
        } else {
            self.grid_scroll(state, down)
        }
    }

    /// 一次滚一屏（可见行数那么多行），高亮随之移动并保持列号。
    ///
    /// 已在最后一行再往下翻，交给末页放宽——与收起态「末页按翻页键」是同一条出路，
    /// 放宽出来的候选追加在末尾，照常滚进来。
    fn grid_page(&self, state: &mut State, down: bool) -> bool {
        self.clear_hover();
        let Some(top) = state.grid_top else {
            return false;
        };
        let rows = self.grid_rows();
        let col = state.selected_index;
        let (page, new_top) = if down {
            self.grid_ensure_loaded(state);
            let last = self.total_pages(state) - 1;
            if state.current_page >= last {
                if !self.try_relax_scope_on_page_end(state) {
                    return false;
                }
                self.grid_land_column(state, col);
                self.grid_follow(state);
                return true;
            }
            // 末屏不留空行：最多滚到「最后一页正好落在可见区最后一行」。`.max(top)` 防的是
            // 列表变短后（扩容滤掉了一组生僻字）这个上限比现位置还靠前，那时不往回滚。
            let new_top = (top + rows).min(last.saturating_sub(rows - 1)).max(top);
            ((state.current_page + rows).min(last), new_top)
        } else {
            if state.current_page == 0 {
                return false;
            }
            (
                state.current_page.saturating_sub(rows),
                top.saturating_sub(rows),
            )
        };
        Self::turn_page(state, page);
        state.grid_top = Some(new_top);
        self.grid_land_column(state, col);
        self.grid_follow(state);
        true
    }

    /// 滚一行（滚轮、翻页箭头、逐行的翻页键）。往上到第一行就停住，不像 ↑ 那样收起；
    /// 往下已在最后一行时走末页放宽，与收起态 `page_next` 同一条出路
    /// （同一个 › 不能展开前会放宽、展开后不会）。
    pub(crate) fn grid_scroll(&self, state: &mut State, down: bool) -> bool {
        if !down {
            return state.current_page > 0 && self.grid_move_row(state, false);
        }
        if self.grid_move_row(state, true) {
            return true;
        }
        let col = state.selected_index;
        if !self.try_relax_scope_on_page_end(state) {
            return false;
        }
        self.grid_land_column(state, col);
        self.grid_follow(state);
        true
    }

    /// ←→：线性移动，越过行尾折到下一行行首、越过行首折到上一行行尾。
    ///
    /// 这正是既有 `move_down` / `move_up` 的「页尾回卷到下一页」——一行就是一页，
    /// 所以不用另写。
    fn grid_move_linear(&self, state: &mut State, forward: bool) -> bool {
        let changed = if forward {
            self.move_down(state)
        } else {
            self.move_up(state)
        };
        self.grid_follow(state);
        changed
    }

    /// 展开态专属的按键：←→ 行内移动、Esc 收起。返回 `Some(是否有变化)` 表示键已归网格，
    /// `None` 表示不是这几个键（或没展开），调用方照常分派。
    ///
    /// 挂在 `apply_session_action` 的最前面：主输入与各 overlay 都先经过它，再落到各自的
    /// 光标移动臂与 Esc 臂——于是展开态下 ←→ 不再移动编码区光标、Esc 只收起不取消，
    /// 而且只需接这一处。
    pub(crate) fn grid_key(&self, state: &mut State, data: &KeyEventData) -> Option<bool> {
        if !Self::grid_active(state) {
            return None;
        }
        // Tab / Shift+Tab 切分类（设计 D5）。只在 Tab 没被另作他用时截：没有标签栏、或用户把
        // Tab 绑成了别的动作（`tab = "aux_code:page_next"` / `"cancel"` 这类，或设成整句切换键）
        // 时不截，照旧走它自己的绑定——展开态凭空吞掉用户的绑定是静默失效。
        if data.key_code == keymap::VK_TAB
            && data.modifiers & MOD_SHORTCUT == 0
            && self.tab_free_for_grid(data.modifiers & MOD_SHIFT != 0)
            && self.grid_tabs(state).len() >= 2
        {
            return Some(self.grid_cycle_tab(state, data.modifiers & MOD_SHIFT == 0));
        }
        if data.modifiers & (MOD_SHIFT | MOD_SHORTCUT) != 0 {
            return None;
        }
        match data.key_code {
            keymap::VK_LEFT => Some(self.grid_move_linear(state, false)),
            keymap::VK_RIGHT => Some(self.grid_move_linear(state, true)),
            // Esc 两段式（设计 D4）：先收起、保留组合；收起态再按一次才是取消。
            keymap::VK_ESCAPE => Some(self.collapse_grid(state)),
            _ => None,
        }
    }

    /// Tab（或 Shift+Tab）能不能拿来切分类：没绑任何会话动作，或绑的就是出厂那组高亮上下
    /// （展开态下它们本是换行，让给分类不丢功能——方向键照样换行）；且不是整句切换键。
    fn tab_free_for_grid(&self, shift: bool) -> bool {
        use wind_config::SessionAction as A;
        let bound = self.session_action_for(keymap::VK_TAB, shift, true);
        let default_like = matches!(bound, None | Some(A::HighlightUp) | Some(A::HighlightDown));
        let sentence_key = self
            .rt()
            .sentence_cycle_key
            .is_some_and(|k| k.vk == keymap::VK_TAB && k.shift == shift);
        default_like && !sentence_key
    }

    /// 可见区覆盖的候选绝对下标范围 `[start, end)`；没展开返回 `None`。
    pub(crate) fn grid_visible_range(&self, state: &State) -> Option<(usize, usize)> {
        let top = state.grid_top?;
        let pp = self.per_page(state.active);
        let len = state.candidates.len();
        let start = (top * pp).min(len);
        let end = ((top + self.grid_rows()) * pp).min(len);
        Some((start, end))
    }

    /// 按序号选词 / 按序号操作（数字键、`;'`、Alt / Ctrl+数字）的下标范围 `[start, end)`。
    ///
    /// 收起态就是当页。展开态起点仍是高亮行行首（于是一切 `start + offset` 的写法原样成立），
    /// 终点往后延到带序号的最后一格——序号顺延到下一行时，数字键要跟着选得到那里。
    pub(crate) fn select_range(&self, state: &State) -> (usize, usize) {
        let (start, end) = self.page_range(state);
        match self.grid_visible_range(state) {
            Some((_, vis_end)) if Self::grid_active(state) => (
                start,
                (start + self.grid_index_span(state)).min(vis_end).max(end),
            ),
            _ => (start, end),
        }
    }

    /// 展开态下从高亮行行首起有几格带序号（`ui.candidate.grid_index_fill`），不超出可见区。
    fn grid_index_span(&self, state: &State) -> usize {
        // 数字键 1–9、0 一共十个。
        const DIGITS: usize = 10;
        let pp = self.per_page(state.active);
        let top = state.grid_top.unwrap_or(state.current_page);
        let rows_below = (top + self.grid_rows())
            .saturating_sub(state.current_page)
            .max(1);
        match self.rt().config.ui.candidate.grid_index_fill.as_str() {
            "off" => pp,
            "cells" => DIGITS.max(pp).min(rows_below * pp),
            // "rows" 及未知值：只补整行，避免出现半行带号、半行不带号。
            _ => (DIGITS / pp).max(1).min(rows_below) * pp,
        }
    }

    // ———————————————— 分类筛选（P3） ————————————————

    /// 候选是否属于某个分类。判据全部取自 `Candidate` 上的现有字段（设计 §8）。
    ///
    /// `rare_known`：常用字表在不在。不在时没法分「常用 / 生僻」，生僻一律不收（`grid_tabs`
    /// 也不会给出这个标签）。
    fn grid_tab_admits(tab: GridTab, c: &Candidate, rare_known: bool) -> bool {
        let han = !c.text.is_empty() && c.text.chars().all(wind_candidate::is_han);
        match tab {
            GridTab::All => true,
            GridTab::Single => han && wind_candidate::semantic_units(&c.text) == 1,
            // 整句不算词组（设计 §8 表）：它是拼出来的一句话，混在词组里只会把真正的词挤下去。
            GridTab::Phrase => {
                han && wind_candidate::semantic_units(&c.text) >= 2 && !c.is_sentence
            }
            GridTab::Rare => han && rare_known && (!c.is_common || c.user_rare),
            // 符号 = 既不含汉字、也不是英文词：拼音里混进来的英文单词另有归处（「全部」里）。
            GridTab::Symbol => {
                !c.text.chars().any(wind_candidate::is_han)
                    && !c.text.chars().any(|ch| ch.is_ascii_alphanumeric())
            }
        }
    }

    /// 常用字表是否已加载（生僻分类的前提）。
    fn rare_known(&self) -> bool {
        !self
            .common_chars
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// 这批候选可用的分类标签。少于两个即不显示标签栏。
    ///
    /// 配置只决定**有哪些**，顺序固定按 [`GridTab`] 枚举：标签栏按枚举顺序画，Tab 循环若按
    /// 配置顺序走，两者就对不上；设置端的多选写回也只保证集合、不保证顺序。
    ///
    /// 「全部」恒在首位（配置里漏写也补上）：切走之后总得有一条回到完整列表的路。生僻要求
    /// 常用字表在；符号只在列表里真有非汉字候选时才给——否则就是一个点开必空的标签。
    pub(crate) fn grid_tabs(&self, state: &State) -> Vec<GridTab> {
        let rt = self.rt();
        let configured = &rt.config.ui.candidate.grid_categories;
        let full: &[Candidate] = if state.grid_tab == GridTab::All {
            &state.candidates
        } else {
            &state.grid_full
        };
        let mut tabs = vec![GridTab::All];
        for tab in GridTab::ALL.into_iter().skip(1) {
            if !configured.iter().any(|k| k.trim() == tab.key()) {
                continue;
            }
            // 每一类都要在完整列表里**已经**有内容才给。只看「还能再取」不够：拼音 `yi` 取到
            // 上限也没有一个词组，临英全是英文词——给出点开必空的标签，Tab 还会因为一路跳过
            // 而被吞掉（原本它在网格里是换行）。生僻另要求常用字表在。
            let rare_known = self.rare_known();
            let usable = (tab != GridTab::Rare || rare_known)
                && full
                    .iter()
                    .any(|c| Self::grid_tab_admits(tab, c, rare_known));
            if usable {
                tabs.push(tab);
            }
        }
        if tabs.len() < 2 { Vec::new() } else { tabs }
    }

    /// Tab / Shift+Tab：切到下一个 / 上一个分类，跳过筛出来是空的（补加载到上限也没有）。
    fn grid_cycle_tab(&self, state: &mut State, forward: bool) -> bool {
        let tabs = self.grid_tabs(state);
        let n = tabs.len();
        let cur = tabs.iter().position(|t| *t == state.grid_tab).unwrap_or(0);
        for step in 1..n {
            let k = if forward {
                (cur + step) % n
            } else {
                (cur + n - step) % n
            };
            if self.grid_switch_tab(state, tabs[k]) {
                return true;
            }
        }
        false
    }

    /// 切到分类 `tab`。筛出来是空的（补加载到上限也没有）就不切，返回 `false`。
    ///
    /// 筛选结果填不满两屏、而引擎还有更多时**先补加载**（单字常常排在首批 300 条之后），
    /// 直到够数或到 `CANDIDATE_LIMIT_CAP`。高亮锚定在原来那个词上（新列表里有的话），
    /// 否则落到第一个。
    pub(crate) fn grid_switch_tab(&self, state: &mut State, tab: GridTab) -> bool {
        if tab == state.grid_tab || !Self::grid_active(state) {
            return false;
        }
        let current = state
            .candidates
            .get(self.highlighted_global_index(state))
            .map(|c| c.text.clone());
        let anchor = match state.grid_tab_anchor.take() {
            Some(a) if a.landed == current => a,
            _ => GridTabAnchor {
                text: current.unwrap_or_default(),
                row: state.current_page - state.grid_top.unwrap_or(state.current_page),
                landed: None,
            },
        };
        let prev = state.grid_tab;
        // 先回到完整列表：补加载（扩容）只认完整列表，筛选也从它出发。
        if prev != GridTab::All {
            state.candidates = std::mem::take(&mut state.grid_full);
            state.grid_tab = GridTab::All;
        }
        let rare_known = self.rare_known();
        if tab != GridTab::All {
            let want = 2 * self.grid_rows() * self.per_page(state.active);
            // 补加载最多两次：每次扩容都是整份重建且在按键线程上（TSF 那边同步等着）。分类稀疏
            // 时一路翻倍到上限要重建五次，而这时多半也凑不满——标签已保证当前列表里至少有一条。
            for _ in 0..2 {
                let have = state
                    .candidates
                    .iter()
                    .filter(|c| Self::grid_tab_admits(tab, c, rare_known))
                    .count();
                let before = state.candidates.len();
                if have >= want || !state.has_more {
                    break;
                }
                self.expand_candidates(state);
                // 扩容后可见条数没涨就停。两种情形都靠这一条收住：临拼 / 辅助码等路径的
                // `expand_candidates` 直接返回、不动 `has_more`（只看它会死循环）；拼音完整音节
                // 下引擎条数随 limit 翻倍、去重后可见条数却纹丝不动（`yi` 实测停在 145），一路扩到
                // 上限只是每次整份重建、白费几十毫秒。
                if state.candidates.len() <= before {
                    break;
                }
            }
            let filtered: Vec<Candidate> = state
                .candidates
                .iter()
                .filter(|c| Self::grid_tab_admits(tab, c, rare_known))
                .cloned()
                .collect();
            if filtered.is_empty() {
                // 这个分类在当前编码下一个都没有：退回原来的分类，不切。补加载可能改过列表
                // （智能档扩容会滤掉先前放行的生僻字），重新锚定在原来那个词上，别留越界页码。
                if prev != GridTab::All {
                    self.grid_apply_tab(state, prev);
                }
                self.grid_land_on(state, anchor);
                return false;
            }
            state.grid_full = std::mem::replace(&mut state.candidates, filtered);
            state.grid_tab = tab;
        }
        self.grid_land_on(state, anchor);
        true
    }

    /// 在完整列表上直接套用分类（不补加载、不锚定）。调用前 `grid_tab` 须为 `All`。
    fn grid_apply_tab(&self, state: &mut State, tab: GridTab) {
        let rare_known = self.rare_known();
        let filtered: Vec<Candidate> = state
            .candidates
            .iter()
            .filter(|c| Self::grid_tab_admits(tab, c, rare_known))
            .cloned()
            .collect();
        state.grid_full = std::mem::replace(&mut state.candidates, filtered);
        state.grid_tab = tab;
    }

    /// 切分类后把高亮落到锚点那个词上（找不到就第一个），尽量留在原来那一可见行，
    /// 并记下落点供下一次切换判断高亮是否被挪过。
    fn grid_land_on(&self, state: &mut State, mut anchor: GridTabAnchor) {
        let idx = state
            .candidates
            .iter()
            .position(|c| c.text == anchor.text)
            .unwrap_or(0);
        let pp = self.per_page(state.active);
        // 直接写页码、不经 `turn_page`：切分类不是翻页，不该改变 `-` 键的翻页身份（同整句切换）。
        state.current_page = idx / pp;
        state.selected_index = idx % pp;
        // 可见区尽量凑满一屏（同在靠后的页上展开）。
        let full_top = self.total_pages(state).saturating_sub(self.grid_rows());
        state.grid_top = Some(state.current_page.saturating_sub(anchor.row).min(full_top));
        self.grid_follow(state);
        self.clear_hover();
        anchor.landed = state.candidates.get(idx).map(|c| c.text.clone());
        state.grid_tab_anchor = Some(anchor);
    }

    /// 候选整份重建（扩容、末页放宽）之后：分类筛选中的话，在新的完整列表上重新筛一遍。
    ///
    /// 重建写的是 `candidates`，于是那一刻它装的是**完整**列表，旧的 `grid_full` 已作废。
    /// 不重筛的后果是：用户在「单字」里往下翻、触发扩容，满屏突然冒出词组。
    pub(crate) fn grid_reapply_filter(&self, state: &mut State) {
        let tab = state.grid_tab;
        if tab == GridTab::All {
            return;
        }
        state.grid_tab = GridTab::All;
        state.grid_full.clear();
        self.grid_apply_tab(state, tab);
        if state.candidates.is_empty() {
            // 极端情况：重建后这一类一个都不剩（检索范围变了）。回到全部，别让候选窗变空。
            state.candidates = std::mem::take(&mut state.grid_full);
            state.grid_tab = GridTab::All;
        }
    }

    /// 调位（置顶 / 前移 / 后移）要用的位置，全部按**完整列表**计——shadow 规则写下的位置，
    /// 读端是按完整候选列表解释的。分类筛选中 `candidates` 只是子集，直接拿子集下标去写，
    /// 「后移」会把一个排在第 40 位的词持久地提到第 4 位。
    ///
    /// 前移 / 后移的目标是子集里**前一个 / 后一个可见词**在完整列表里的位置：用户看到的是
    /// 子集，「往前挪一格」挪的是看得见的那一格。没在筛选时与 `idx ± 1` 逐字一致。
    pub(crate) fn reorder_positions(&self, state: &State, idx: usize) -> ReorderPositions {
        if state.grid_tab == GridTab::All || state.grid_full.is_empty() {
            let last = state.candidates.len().saturating_sub(1);
            return ReorderPositions {
                here: idx,
                prev: idx.saturating_sub(1),
                next: (idx + 1).min(last),
                last,
            };
        }
        let full = &state.grid_full;
        let at = |i: usize| -> Option<usize> {
            let c = state.candidates.get(i)?;
            full.iter().position(|f| {
                f.text == c.text && f.code == c.code && f.id == c.id && f.source == c.source
            })
        };
        let last = full.len().saturating_sub(1);
        let here = at(idx).unwrap_or(idx);
        ReorderPositions {
            here,
            prev: idx
                .checked_sub(1)
                .and_then(at)
                .unwrap_or(here.saturating_sub(1)),
            next: at(idx + 1).unwrap_or((here + 1).min(last)),
            last,
        }
    }

    /// 退出分类筛选、换回完整列表（不动页码与网格位置以外的东西）。没在筛选时什么也不做。
    ///
    /// 给那些要拿「原始候选」做快照的入口用（如辅助码）。
    pub(crate) fn grid_restore_full(&self, state: &mut State) {
        if state.grid_tab == GridTab::All {
            return;
        }
        state.candidates = std::mem::take(&mut state.grid_full);
        state.grid_tab = GridTab::All;
        self.clamp_candidate_view(state);
    }

    /// UI 回传的下标若落在分类标签那一段（[`GRID_TAB_TAG_BASE`] 起），返回对应分类。
    pub(crate) fn grid_tab_of_event(&self, idx: usize) -> Option<GridTab> {
        let k = idx.checked_sub(GRID_TAB_TAG_BASE as usize)?;
        GridTab::from_index(u8::try_from(k).ok()?)
    }

    /// 鼠标点了分类标签：切过去并重绘。没展开、或这个分类筛出来是空的，什么也不做。
    pub(crate) fn grid_click_tab(&self, tab: GridTab) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !self.grid_tabs(&state).contains(&tab) {
            return;
        }
        if self.grid_switch_tab(&mut state, tab) {
            // 高亮换了 ⇒ 混输下组合区按高亮候选重算（键盘那条路在 `after_nav_changed` 里做）。
            // 这里没有按键可回传组合串，嵌入宿主的组合区要等下一键才跟上，与鼠标点格子同形。
            self.sync_preedit_to_highlight(&mut state);
            self.notify_ui_update(&state);
        }
    }

    /// 平铺下标 → `(页, 页内下标)`。没展开、或下标落在可见区外返回 `None`。
    pub(crate) fn grid_cell(&self, state: &State, flat: usize) -> Option<(usize, usize)> {
        let (start, end) = self.grid_visible_range(state)?;
        if start + flat >= end {
            return None;
        }
        let pp = self.per_page(state.active);
        let abs = start + flat;
        Some((abs / pp, abs % pp))
    }

    /// UI 反向事件带来的候选下标 → 页内下标。
    ///
    /// 展开态下先把高亮切到那一格（点哪格、右键哪格，操作的就是哪格），再按页内下标走原有
    /// 逻辑——右键菜单、词条操作、点选上屏都不必知道网格的存在。收起态原样返回。
    /// `redraw`：切了高亮要不要立刻重绘（点选上屏马上就要收窗，不必多画一帧）。
    pub(crate) fn resolve_event_index(&self, idx: usize, redraw: bool) -> Option<usize> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !Self::grid_active(&state) {
            return Some(idx);
        }
        let (page, col) = self.grid_cell(&state, idx)?;
        if page != state.current_page || col != state.selected_index {
            Self::turn_page(&mut state, page);
            state.selected_index = col;
            if redraw {
                self.notify_ui_update(&state);
            }
        }
        Some(col)
    }
}

#[cfg(test)]
mod tests {
    //! 展开态的行为测试。合成候选的几组只走导航（不碰引擎），真实词库那组走完整输入链。
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc::Receiver;
    use wind_bridge::handler::{KeyAction, MessageHandler};
    use wind_ui_types::{CandidateGrid, UiCommand, UiEvent};

    /// 出厂每页 7 个、可见 5 行。
    const PP: usize = 7;
    const ROWS: usize = 5;

    fn key(key_code: u32) -> KeyEventData {
        KeyEventData {
            key_code,
            scan_code: 0,
            modifiers: 0,
            event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
        }
    }

    fn coord(grid_on: bool) -> (Arc<Coordinator>, Receiver<UiCommand>) {
        let mut cfg = wind_config::Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.ui.candidate.grid_expand = grid_on;
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
        c.debug_mark_coords_ready();
        (c, rx)
    }

    /// 造 `n` 条候选并挂上一段编码（有编码才有输入会话，Esc 等键才归输入法）。
    fn seed(c: &Coordinator, n: usize) {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "yi".into();
        st.candidates = (0..n)
            .map(|i| wind_candidate::Candidate {
                text: format!("c{i}"),
                ..Default::default()
            })
            .collect();
        c.reset_candidate_view(&mut st);
    }

    /// (当前页, 页内高亮, 网格首行)。
    fn view(c: &Coordinator) -> (usize, usize, Option<usize>) {
        let st = c.state.lock().unwrap();
        (st.current_page, st.selected_index, st.grid_top)
    }

    /// 最后一帧候选：(文字, 选中, 网格版式)。
    fn last_frame(rx: &Receiver<UiCommand>) -> Option<(Vec<String>, usize, Option<CandidateGrid>)> {
        rx.try_iter()
            .filter_map(|cmd| match cmd {
                UiCommand::UpdateCandidates {
                    candidates,
                    selected,
                    grid,
                    ..
                } => Some((
                    candidates.into_iter().map(|i| i.text).collect(),
                    selected,
                    grid,
                )),
                _ => None,
            })
            .last()
    }

    /// 反向对照：开关关着时翻页键就是翻页，网格不出现。
    #[test]
    fn page_key_just_pages_when_grid_is_off() {
        let (c, _rx) = coord(false);
        seed(&c, PP * 3);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        assert_eq!(view(&c), (1, 0, None));
    }

    /// 第一次按翻页键只展开、不翻页（D3）；下发的帧是可见区内逐行平铺的几页。
    #[test]
    fn first_page_key_expands_without_turning_the_page() {
        let (c, rx) = coord(true);
        seed(&c, PP * 8);
        let _ = rx.try_iter().count();
        assert!(matches!(
            c.handle_key_event(&key(keymap::VK_EQUAL)),
            KeyAction::Consumed
        ));
        assert_eq!(view(&c), (0, 0, Some(0)), "高亮不动，只是展开");
        let (texts, selected, grid) = last_frame(&rx).expect("展开要重绘");
        assert_eq!(texts.len(), PP * ROWS, "一屏 = 可见行数 × 每页数");
        assert_eq!(texts[PP], "c7", "第二行就是第二页");
        assert_eq!(selected, 0);
        assert_eq!(
            grid,
            Some(CandidateGrid {
                cols: PP as u16,
                rows: ROWS as u16,
                highlight_row: 0,
                index_start: 0,
                index_count: PP as u16,
                // 合成候选全是非汉字：单字 / 词组 / 生僻都没有内容 ⇒ 不显示标签栏。
                tabs: 0,
                active_tab: 0,
            }),
            "每页 7 个按整行补只编得下高亮行"
        );
    }

    /// 只有一页时没什么可展开：翻页键回到原有语义（翻不动）。
    #[test]
    fn single_page_does_not_expand() {
        let (c, _rx) = coord(true);
        seed(&c, PP);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        assert_eq!(view(&c).2, None);
    }

    /// 「还能加载」但扩容后仍只有一页：不展开，也不记「本次展开过」。
    #[test]
    fn has_more_but_still_one_page_does_not_expand() {
        let (c, _rx) = coord(true);
        seed(&c, PP);
        c.state.lock().unwrap().has_more = true;
        c.handle_key_event(&key(keymap::VK_EQUAL));
        let st = c.state.lock().unwrap();
        assert_eq!(st.grid_top, None);
        assert!(!st.grid_keep);
    }

    /// 原地重建（置顶 / 调位 / 删除）后回到原来那一行、那一列，而不是跳回首行。
    #[test]
    fn in_place_rebuild_keeps_the_row_and_column() {
        let (c, _rx) = coord(true);
        seed(&c, PP * 8);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        for _ in 0..3 {
            c.handle_key_event(&key(keymap::VK_DOWN));
        }
        c.handle_key_event(&key(keymap::VK_RIGHT));
        c.handle_key_event(&key(keymap::VK_RIGHT));
        let before = view(&c);
        let mut st = c.state.lock().unwrap();
        let snap = Coordinator::grid_snapshot(&st).expect("展开着");
        // 模拟 `update_candidates`：视图复位，再按「本次展开过」从首行展开回来。
        c.reset_candidate_view(&mut st);
        c.grid_resume(&mut st);
        assert_eq!(st.current_page, 0, "前置：重建会跳回首行");
        c.grid_restore(&mut st, snap);
        drop(st);
        assert_eq!(view(&c), before);
    }

    /// 只剩联想（候选非空、没有编码）时按键：上一次组合已结束，「本次展开过」作废。
    #[test]
    fn association_ends_the_kept_expansion() {
        let (c, _rx) = coord(true);
        {
            let mut st = c.state.lock().unwrap();
            st.candidates = vec![wind_candidate::Candidate {
                text: "们".into(),
                source: wind_candidate::CandidateSource::Assoc,
                ..Default::default()
            }];
            st.grid_keep = true;
        }
        c.handle_key_event(&key(u32::from(b'A')));
        assert!(!c.state.lock().unwrap().grid_keep);
    }

    /// 码表空码：候选为空但组合还在，「本次展开过」不能被当成组合结束清掉。
    #[test]
    fn empty_code_keeps_the_kept_expansion() {
        let (c, _rx) = coord(true);
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "zzzz".into();
            st.grid_keep = true;
        }
        c.handle_key_event(&key(keymap::VK_LEFT));
        assert!(c.state.lock().unwrap().grid_keep);
    }

    /// 顶码：首选上屏、余码另起一次组合，不继承展开。
    #[test]
    fn top_code_commit_starts_collapsed() {
        let (c, _rx) = coord(true);
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "aaaah".into();
        st.input_buffer_cased = "aaaah".into();
        st.grid_keep = true;
        c.commit_top_text(
            &mut st,
            "aaaa",
            "工".into(),
            None,
            None,
            "h",
            wind_candidate::CandidateSource::default(),
            None,
        );
        assert!(!st.grid_keep);
        assert_eq!(st.grid_top, None);
    }

    /// ↓ 换行并保持列号；← → 线性移动、越过行尾折到下一行。
    #[test]
    fn arrows_move_in_two_dimensions() {
        let (c, rx) = coord(true);
        seed(&c, PP * 8);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        c.handle_key_event(&key(keymap::VK_RIGHT));
        c.handle_key_event(&key(keymap::VK_RIGHT));
        assert_eq!(view(&c), (0, 2, Some(0)));
        c.handle_key_event(&key(keymap::VK_DOWN));
        assert_eq!(view(&c), (1, 2, Some(0)), "换行保持列号");
        let (_, selected, grid) = last_frame(&rx).unwrap();
        assert_eq!(selected, PP + 2, "选中项是平铺下标");
        assert_eq!(grid.unwrap().highlight_row, 1);
        for _ in 0..(PP - 2) {
            c.handle_key_event(&key(keymap::VK_RIGHT));
        }
        assert_eq!(view(&c), (2, 0, Some(0)), "越过行尾折到下一行行首");
        c.handle_key_event(&key(keymap::VK_LEFT));
        assert_eq!(view(&c), (1, PP - 1, Some(0)), "越过行首折回上一行行尾");
    }

    /// 展开态下 ←→ 归网格，不再移动编码区光标。
    #[test]
    fn arrows_do_not_move_the_preedit_caret_when_expanded() {
        let (c, _rx) = coord(true);
        seed(&c, PP * 3);
        c.state.lock().unwrap().input_cursor_pos = 2;
        c.handle_key_event(&key(keymap::VK_EQUAL));
        c.handle_key_event(&key(keymap::VK_LEFT));
        assert_eq!(c.state.lock().unwrap().input_cursor_pos, 2);
    }

    /// `-` `=` 逐行移动（同滚轮）：高亮到可见区末行再按，可见区滚一行；首行按 `-` 停住不收起。
    #[test]
    fn page_key_moves_a_row_when_expanded() {
        let (c, _rx) = coord(true);
        seed(&c, PP * 8);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        c.handle_key_event(&key(keymap::VK_RIGHT));
        c.handle_key_event(&key(keymap::VK_EQUAL));
        assert_eq!(view(&c), (1, 1, Some(0)), "下移一行并保持列号");
        for _ in 0..3 {
            c.handle_key_event(&key(keymap::VK_EQUAL));
        }
        assert_eq!(view(&c), (4, 1, Some(0)), "到可见区末行");
        c.handle_key_event(&key(keymap::VK_EQUAL));
        assert_eq!(view(&c), (5, 1, Some(1)), "末行再按，可见区滚一行");
        for _ in 0..5 {
            c.handle_key_event(&key(keymap::VK_MINUS));
        }
        assert_eq!(view(&c), (0, 1, Some(0)));
        assert!(matches!(
            c.handle_key_event(&key(keymap::VK_MINUS)),
            KeyAction::Consumed
        ));
        assert_eq!(view(&c), (0, 1, Some(0)), "首行按 - 停住，不收起");
    }

    /// PageUp/PageDown 一次滚一屏，高亮随之下移一屏并保持列号；末屏不留空行。
    #[test]
    fn page_up_down_scrolls_a_screen_when_expanded() {
        let (c, _rx) = coord(true);
        seed(&c, PP * 8); // 8 页
        c.handle_key_event(&key(keymap::VK_NEXT));
        c.handle_key_event(&key(keymap::VK_RIGHT));
        c.handle_key_event(&key(keymap::VK_NEXT));
        // 末页是第 7 页：可见区最多滚到 3..=7，高亮从 0 下移 5 行到第 5 页。
        assert_eq!(view(&c), (5, 1, Some(3)));
        c.handle_key_event(&key(keymap::VK_NEXT));
        assert_eq!(view(&c), (7, 1, Some(3)), "到底后高亮落到末行");
        c.handle_key_event(&key(keymap::VK_PRIOR));
        assert_eq!(view(&c), (2, 1, Some(0)));
        c.handle_key_event(&key(keymap::VK_PRIOR));
        assert_eq!(view(&c), (0, 1, Some(0)), "首屏再往上翻回到首行，不收起");
        assert!(matches!(
            c.handle_key_event(&key(keymap::VK_PRIOR)),
            KeyAction::Consumed
        ));
        assert_eq!(view(&c).2, Some(0));
    }

    /// 关掉 `grid_pageupdown_screen` 后 PageUp/PageDown 也逐行。
    #[test]
    fn page_up_down_moves_a_row_when_screen_off() {
        let mut cfg = wind_config::Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.ui.candidate.grid_expand = true;
        cfg.ui.candidate.grid_pageupdown_screen = false;
        let (c, _rx) = Coordinator::new_headless_with_ui(cfg, None);
        c.debug_mark_coords_ready();
        seed(&c, PP * 8);
        c.handle_key_event(&key(keymap::VK_NEXT));
        c.handle_key_event(&key(keymap::VK_NEXT));
        assert_eq!(view(&c), (1, 0, Some(0)));
        c.handle_key_event(&key(keymap::VK_PRIOR));
        c.handle_key_event(&key(keymap::VK_PRIOR));
        assert_eq!(view(&c), (0, 0, Some(0)), "首行停住，不收起");
    }

    /// 高亮行滚出可见区时可见区跟随。
    #[test]
    fn visible_rows_follow_the_highlight() {
        let (c, rx) = coord(true);
        seed(&c, PP * 8);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        for _ in 0..ROWS {
            c.handle_key_event(&key(keymap::VK_DOWN));
        }
        assert_eq!(view(&c), (5, 0, Some(1)));
        let (texts, selected, grid) = last_frame(&rx).unwrap();
        assert_eq!(texts[0], "c7", "可见区第一行是第 1 页");
        assert_eq!(grid.unwrap().highlight_row as usize, ROWS - 1);
        assert_eq!(selected, (ROWS - 1) * PP);
    }

    /// 末屏行数不足一屏：照常下发实有的那些，补行由渲染端负责。
    #[test]
    fn short_last_screen_sends_only_real_rows() {
        let (c, rx) = coord(true);
        seed(&c, PP * 2 + 3);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        let (texts, _, grid) = last_frame(&rx).unwrap();
        assert_eq!(texts.len(), PP * 2 + 3);
        assert_eq!(grid.unwrap().rows as usize, ROWS);
    }

    /// ↓ 到末行后停住；↑ 在第一行收起。
    #[test]
    fn up_on_first_row_collapses() {
        let (c, rx) = coord(true);
        seed(&c, PP * 3);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        c.handle_key_event(&key(keymap::VK_DOWN));
        c.handle_key_event(&key(keymap::VK_DOWN));
        c.handle_key_event(&key(keymap::VK_DOWN));
        assert_eq!(view(&c).0, 2, "末行再按 ↓ 停住");
        c.handle_key_event(&key(keymap::VK_UP));
        c.handle_key_event(&key(keymap::VK_UP));
        assert_eq!(view(&c), (0, 0, Some(0)));
        c.handle_key_event(&key(keymap::VK_UP));
        assert_eq!(view(&c).2, None, "第一行按 ↑ 收起");
        let (texts, _, grid) = last_frame(&rx).unwrap();
        assert_eq!((texts.len(), grid), (PP, None), "收起后回到单行");
    }

    /// Esc 两段式（D4）：先收起、保留编码；再按才取消。
    #[test]
    fn escape_collapses_first_then_cancels() {
        let (c, _rx) = coord(true);
        seed(&c, PP * 3);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        c.handle_key_event(&key(keymap::VK_DOWN));
        assert!(matches!(
            c.handle_key_event(&key(keymap::VK_ESCAPE)),
            KeyAction::Consumed
        ));
        {
            let st = c.state.lock().unwrap();
            assert_eq!(st.grid_top, None);
            assert_eq!(st.input_buffer, "yi", "收起不动编码");
            assert_eq!(st.current_page, 1, "收起停在高亮所在的那一页");
        }
        assert!(matches!(
            c.handle_key_event(&key(keymap::VK_ESCAPE)),
            KeyAction::ClearComposition
        ));
    }

    /// 回车上屏这类出口只清候选、不经视图复位：组合结束后 ←→ / Esc 必须交还宿主，
    /// 不能被残留的展开态吞掉（Linux / macOS 每个键都会进协调器）。
    #[test]
    fn keys_go_back_to_the_host_after_the_composition_ends() {
        let (c, _rx) = coord(true);
        seed(&c, PP * 3);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        assert!(view(&c).2.is_some());
        c.handle_key_event(&key(keymap::VK_RETURN));
        assert!(
            c.state.lock().unwrap().candidates.is_empty(),
            "回车上屏了编码"
        );
        for vk in [keymap::VK_LEFT, keymap::VK_RIGHT, keymap::VK_ESCAPE] {
            assert!(
                matches!(c.handle_key_event(&key(vk)), KeyAction::PassThrough),
                "组合已结束，键 {vk:#x} 应交还宿主"
            );
        }
    }

    /// 在靠后的页上展开：可见区往前让、凑满一屏，而不是只露出一行真候选。
    #[test]
    fn expanding_near_the_end_fills_the_screen() {
        let (c, rx) = coord(true);
        seed(&c, PP * 6);
        for _ in 0..(PP * 4) {
            c.handle_key_event(&key(keymap::VK_DOWN)); // 收起态逐个移动，回卷到第 4 页
        }
        assert_eq!(view(&c), (4, 0, None));
        c.handle_key_event(&key(keymap::VK_EQUAL));
        assert_eq!(view(&c), (4, 0, Some(1)), "6 页可见 5 行 ⇒ 首行是第 1 页");
        let (texts, selected, grid) = last_frame(&rx).unwrap();
        assert_eq!(texts.len(), PP * ROWS);
        assert_eq!(grid.unwrap().highlight_row, 3);
        assert_eq!(selected, 3 * PP);
    }

    /// UI 回传平铺下标：右键第 3 行第 2 格，高亮先切过去，菜单目标是那一格。
    #[test]
    fn flat_index_from_ui_lands_on_that_cell() {
        let (c, _rx) = coord(true);
        seed(&c, PP * 8);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        assert_eq!(c.resolve_event_index(2 * PP + 1, true), Some(1));
        assert_eq!(view(&c), (2, 1, Some(0)));
        assert_eq!(c.resolve_event_index(PP * ROWS, true), None, "可见区外");
        // 收起态原样返回（快捷键等调用方传的就是页内下标）。
        c.collapse_grid(&mut c.state.lock().unwrap());
        assert_eq!(c.resolve_event_index(3, true), Some(3));
    }

    /// 悬停校验按整个可见区，不按单页：第 2 行的格子也是合法悬停目标。
    #[test]
    fn hover_accepts_cells_beyond_the_first_row() {
        let (c, _rx) = coord(true);
        seed(&c, PP * 3);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        c.handle_ui_event(UiEvent::Hover((PP + 1) as i32));
        assert_eq!(c.hover_target(), (PP + 1) as i32);
    }

    /// 滚轮 / 翻页箭头在展开态滚一行；往上滚到顶不收起。收起态往下滚先展开。
    #[test]
    fn mouse_page_scrolls_rows() {
        let (c, _rx) = coord(true);
        seed(&c, PP * 3);
        c.handle_ui_event(UiEvent::Page(1));
        assert_eq!(view(&c), (0, 0, Some(0)), "收起态往下滚 = 展开");
        c.handle_ui_event(UiEvent::Page(1));
        assert_eq!(view(&c).0, 1);
        c.handle_ui_event(UiEvent::Page(-1));
        c.handle_ui_event(UiEvent::Page(-1));
        assert_eq!(view(&c), (0, 0, Some(0)), "滚到顶停住");
    }

    /// 只有横排展开：竖排下翻页键照旧翻页。
    #[test]
    fn vertical_layout_does_not_expand() {
        let mut cfg = wind_config::Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.ui.candidate.grid_expand = true;
        cfg.ui.candidate.layout = "vertical".into();
        let (c, _rx) = Coordinator::new_headless_with_ui(cfg, None);
        seed(&c, PP * 3);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        assert_eq!(view(&c), (1, 0, None));
    }

    /// 自定每页数与序号顺延档位的协调器。
    fn coord_with(per_page: usize, fill: &str) -> (Arc<Coordinator>, Receiver<UiCommand>) {
        let mut cfg = wind_config::Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.ui.candidate.grid_expand = true;
        cfg.ui.candidate.per_page = per_page;
        cfg.ui.candidate.grid_index_fill = fill.into();
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
        c.debug_mark_coords_ready();
        (c, rx)
    }

    /// 最后一帧里带序号的格子：(平铺下标起点, 格数, 各格序号)。
    fn index_frame(rx: &Receiver<UiCommand>) -> (u16, u16, Vec<String>) {
        rx.try_iter()
            .filter_map(|cmd| match cmd {
                UiCommand::UpdateCandidates {
                    candidates,
                    grid: Some(g),
                    ..
                } => Some((
                    g.index_start,
                    g.index_count,
                    candidates.into_iter().map(|i| i.label).collect::<Vec<_>>(),
                )),
                _ => None,
            })
            .last()
            .expect("应下发展开态的帧")
    }

    /// 只补整行（出厂）：每页 5 个时高亮行 1–5、下一行 6–9、0（第 10 个由数字键 0 选，序号沿用
    /// 普通候选窗的写法），数字键的选择范围跟着延到下一行。
    #[test]
    fn indices_continue_onto_whole_rows() {
        let (c, rx) = coord_with(5, "rows");
        seed(&c, 5 * 8);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        c.handle_key_event(&key(keymap::VK_DOWN));
        let (from, count, labels) = index_frame(&rx);
        assert_eq!((from, count), (5, 10), "高亮在第 2 行：第 2、3 行带号");
        assert_eq!(
            &labels[5..15],
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"]
        );
        let st = c.state.lock().unwrap();
        assert_eq!(c.select_range(&st), (5, 15), "数字键的范围跟着序号走");
    }

    /// 高亮在可见区最后一行：下面没有可见行可补，只编高亮行（不给看不见的格子编号）。
    #[test]
    fn indices_stay_in_the_visible_rows() {
        let (c, rx) = coord_with(5, "rows");
        seed(&c, 5 * 8);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        for _ in 0..(ROWS - 1) {
            c.handle_key_event(&key(keymap::VK_DOWN));
        }
        let (from, count, _) = index_frame(&rx);
        assert_eq!((from, count), ((ROWS as u16 - 1) * 5, 5));
    }

    /// 逐格档：每页 7 个时编满 10 个，下一行前 3 格带 8、9、0；off 档只编高亮行。
    #[test]
    fn index_fill_cells_and_off() {
        let (c, rx) = coord_with(7, "cells");
        seed(&c, 7 * 4);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        let (from, count, labels) = index_frame(&rx);
        assert_eq!((from, count), (0, 10));
        assert_eq!(&labels[7..10], &["8", "9", "0"]);
        let (c, rx) = coord_with(5, "off");
        seed(&c, 5 * 4);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        assert_eq!(index_frame(&rx).1, 5);
    }

    /// 菜单开着时在另一格上右键：高亮与悬停都跟到新的这一格（悬停若停在旧格，旧格的悬停
    /// 底色会留在原处，看上去像高亮没跟过来）。
    #[test]
    fn second_right_click_moves_highlight_and_hover() {
        let (c, rx) = coord(true);
        seed(&c, PP * 3);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        for idx in [PP + 2, 2 * PP + 4] {
            c.handle_ui_event(UiEvent::RequestCandidateMenu {
                page_local: idx,
                x: 0,
                y: 0,
            });
        }
        assert_eq!(c.hover_target(), (2 * PP + 4) as i32);
        let (_, selected, _) = last_frame(&rx).unwrap();
        assert_eq!(selected, 2 * PP + 4);
    }

    fn data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
    }

    /// 真实词库：打 `yi`、展开、↓ 到第二行、按 2 ⇒ 上屏的是第 2 页第 2 个。
    /// 数字键语义没动（作用于高亮行 = 当前页），这条把它钉在完整输入链上。
    #[test]
    fn number_key_selects_in_the_highlighted_row() {
        if !data_dir().join("schemas/pinyin.schema.toml").exists() {
            return;
        }
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec!["pinyin".into()];
        cfg.schema.active = "pinyin".into();
        cfg.input.default.chinese_mode = true;
        cfg.ui.candidate.grid_expand = true;
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
        c.debug_mark_coords_ready();
        for ch in "YI".chars() {
            c.handle_key_event(&key(ch as u32));
        }
        c.handle_key_event(&key(keymap::VK_EQUAL));
        c.handle_key_event(&key(keymap::VK_DOWN));
        let (texts, selected, _) = last_frame(&rx).expect("展开态应下发候选");
        assert_eq!(selected, PP);
        let want = c.state.lock().unwrap().candidates[PP + 1].text.clone();
        assert_eq!(texts[PP + 1], want);
        match c.handle_key_event(&key(u32::from(b'2'))) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, want),
            other => panic!("按 2 应上屏第 2 页第 2 个，实得 {other:?}"),
        }
        assert_eq!(c.state.lock().unwrap().grid_top, None, "上屏后收起");
    }

    /// 打出 `code`（大写键码）并展开（真实词库）。词库缺失返回 `None`。
    ///
    /// 分类用例用 `shijian`：它同时有单字与词组（实测 100 / 15）；`yi` 这类完整单音节一个
    /// 词组都没有，测不到词组那一支。
    fn typed_expanded(code: &str) -> Option<(Arc<Coordinator>, Receiver<UiCommand>)> {
        if !data_dir().join("schemas/pinyin.schema.toml").exists() {
            return None;
        }
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec!["pinyin".into()];
        cfg.schema.active = "pinyin".into();
        cfg.input.default.chinese_mode = true;
        cfg.ui.candidate.grid_expand = true;
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
        c.debug_mark_coords_ready();
        for ch in code.chars() {
            c.handle_key_event(&key(ch as u32));
        }
        c.handle_key_event(&key(keymap::VK_EQUAL));
        Some((c, rx))
    }

    fn shift_tab() -> KeyEventData {
        KeyEventData {
            modifiers: MOD_SHIFT,
            ..key(keymap::VK_TAB)
        }
    }

    fn texts(c: &Coordinator) -> Vec<String> {
        c.state
            .lock()
            .unwrap()
            .candidates
            .iter()
            .map(|c| c.text.clone())
            .collect()
    }

    fn tab_of(c: &Coordinator) -> GridTab {
        c.state.lock().unwrap().grid_tab
    }

    /// 把高亮移到平铺下标 `idx` 上（展开态 ↓ 换行、→ 换列）。
    fn highlight_flat(c: &Coordinator, idx: usize) {
        for _ in 0..idx / PP {
            c.handle_key_event(&key(keymap::VK_DOWN));
        }
        for _ in 0..idx % PP {
            c.handle_key_event(&key(keymap::VK_RIGHT));
        }
        let st = c.state.lock().unwrap();
        assert_eq!(st.current_page * PP + st.selected_index, idx);
    }

    /// 分步上屏：选一个只吃掉前一段的单字，余下那段的候选仍是展开的；继续打字也不收。
    #[test]
    fn step_commit_keeps_the_grid_expanded() {
        let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
            return;
        };
        let idx = texts(&c)
            .iter()
            .position(|t| t.chars().count() == 1)
            .expect("shijian 应有单字候选");
        highlight_flat(&c, idx);
        c.handle_key_event(&key(keymap::VK_SPACE));
        {
            let st = c.state.lock().unwrap();
            assert!(!st.committed_text.is_empty(), "单字应留在组合区");
            assert_eq!(st.input_buffer, "jian");
            assert_eq!(st.grid_top, Some(0), "余下那段从首行展开");
        }
        c.handle_key_event(&key(u32::from(b'G')));
        assert_eq!(view(&c).2, Some(0), "继续打字也保持展开");
    }

    /// Esc 收起之后，本次组合不再自动展开。
    #[test]
    fn escape_collapse_stops_re_expanding() {
        let Some((c, _rx)) = typed_expanded("SHI") else {
            return;
        };
        c.handle_key_event(&key(keymap::VK_ESCAPE));
        assert_eq!(view(&c).2, None);
        c.handle_key_event(&key(u32::from(b'J')));
        assert_eq!(view(&c).2, None, "收起后再打字不再展开");
    }

    /// 上屏结束组合：下一次组合从单行开始。
    #[test]
    fn next_composition_starts_collapsed() {
        let Some((c, _rx)) = typed_expanded("YI") else {
            return;
        };
        assert!(view(&c).2.is_some());
        c.handle_key_event(&key(keymap::VK_SPACE));
        assert!(c.state.lock().unwrap().input_buffer.is_empty(), "空格上屏");
        for ch in "YI".chars() {
            c.handle_key_event(&key(ch as u32));
        }
        assert_eq!(view(&c).2, None, "新组合不继承上一次的展开");
    }

    /// Tab 依次切到单字、词组：列表里只剩该类；切回全部，列表与切走之前逐条一致。
    #[test]
    fn tab_key_filters_by_category_and_back() {
        let Some((c, rx)) = typed_expanded("SHIJIAN") else {
            return;
        };
        let all = texts(&c);
        let units = |t: &str| wind_candidate::semantic_units(t);
        let han = |t: &str| t.chars().all(wind_candidate::is_han);
        c.handle_key_event(&key(keymap::VK_TAB));
        assert_eq!(tab_of(&c), GridTab::Single);
        let single = texts(&c);
        assert!(!single.is_empty());
        assert!(
            single.iter().all(|t| han(t) && units(t) == 1),
            "单字：{single:?}"
        );
        let (_, _, grid) = last_frame(&rx).unwrap();
        assert_eq!(grid.unwrap().active_tab, GridTab::Single as u8);
        c.handle_key_event(&key(keymap::VK_TAB));
        assert_eq!(tab_of(&c), GridTab::Phrase);
        assert!(texts(&c).iter().all(|t| han(t) && units(t) >= 2));
        // Shift+Tab 反向：词组 → 单字 → 全部。
        c.handle_key_event(&shift_tab());
        c.handle_key_event(&shift_tab());
        assert_eq!(tab_of(&c), GridTab::All);
        let back = texts(&c);
        assert_eq!(
            &back[..all.len().min(back.len())],
            &all[..all.len().min(back.len())]
        );
        assert!(back.len() >= all.len(), "补加载过只会更多，不会丢");
    }

    /// 切分类时高亮锚定在原来那个词上。
    #[test]
    fn switching_tab_keeps_the_highlighted_word() {
        let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
            return;
        };
        // 在「全部」里走到第一个单字上，记下它。
        let first_single = {
            let st = c.state.lock().unwrap();
            st.candidates
                .iter()
                .position(|x| wind_candidate::semantic_units(&x.text) == 1)
                .unwrap()
        };
        for _ in 0..first_single {
            c.handle_key_event(&key(keymap::VK_RIGHT));
        }
        let word = {
            let st = c.state.lock().unwrap();
            st.candidates[c.highlighted_global_index(&st)].text.clone()
        };
        c.handle_key_event(&key(keymap::VK_TAB));
        let st = c.state.lock().unwrap();
        assert_eq!(st.candidates[c.highlighted_global_index(&st)].text, word);
    }

    /// Tab 转一圈回到「全部」：高亮回到原来那个词、原来那一可见行。中途某个分类里没有它
    /// （落到第一个）也不改锚点，只要切换之间没挪过高亮。
    #[test]
    fn tab_round_trip_returns_to_the_same_cell() {
        let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
            return;
        };
        // 第二页以后的第一个单字：它在「词组」里没有。
        let idx = {
            let st = c.state.lock().unwrap();
            (PP..st.candidates.len())
                .find(|&i| wind_candidate::semantic_units(&st.candidates[i].text) == 1)
                .expect("shijian 第二页以后应有单字")
        };
        highlight_flat(&c, idx);
        let before = {
            let st = c.state.lock().unwrap();
            (st.current_page, st.selected_index, st.grid_top)
        };
        assert!(before.2.unwrap() < before.0, "前提：高亮不在可见区首行");
        let n = c.grid_tabs(&c.state.lock().unwrap()).len();
        assert!(n >= 3, "前提：至少全部 / 单字 / 词组");
        for _ in 0..n {
            c.handle_key_event(&key(keymap::VK_TAB));
        }
        assert_eq!(tab_of(&c), GridTab::All);
        let st = c.state.lock().unwrap();
        assert_eq!((st.current_page, st.selected_index, st.grid_top), before);
    }

    /// 在「单字」里触发扩容：扩容重建的是完整列表（带词组），筛选要重新套上。
    #[test]
    fn expansion_keeps_the_filter() {
        let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
            return;
        };
        c.handle_key_event(&key(keymap::VK_TAB));
        let mut st = c.state.lock().unwrap();
        assert_eq!(st.grid_tab, GridTab::Single);
        // 白盒触发一次扩容：这个码首批就取完了，翻页触发不到。
        st.has_more = true;
        let limit = st.candidate_limit;
        c.expand_candidates(&mut st);
        assert!(st.candidate_limit > limit, "扩容确实重建过");
        assert_eq!(st.grid_tab, GridTab::Single);
        assert!(
            st.candidates
                .iter()
                .all(|x| wind_candidate::semantic_units(&x.text) == 1),
            "扩容后仍只有单字"
        );
        assert!(
            st.grid_full
                .iter()
                .any(|x| wind_candidate::semantic_units(&x.text) >= 2),
            "完整列表里有词组（重建出来的是完整列表）"
        );
    }

    /// 鼠标点标签（tag 段 `GRID_TAB_TAG_BASE`）切分类；数字键在筛过的列表里选词；
    /// 上屏后再打字，分类随视图一起复位。
    #[test]
    fn tab_click_then_select_then_reset() {
        let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
            return;
        };
        c.handle_ui_event(UiEvent::CandidateSelect(
            GRID_TAB_TAG_BASE as usize + GridTab::Phrase as usize,
        ));
        assert_eq!(tab_of(&c), GridTab::Phrase);
        let want = texts(&c)[1].clone();
        match c.handle_key_event(&key(u32::from(b'2'))) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, want),
            other => panic!("按 2 应上屏词组里的第 2 个，实得 {other:?}"),
        }
        for ch in "SHI".chars() {
            c.handle_key_event(&key(ch as u32));
        }
        let st = c.state.lock().unwrap();
        assert_eq!((st.grid_tab, st.grid_top), (GridTab::All, None));
        assert!(st.grid_full.is_empty());
    }

    /// 当前候选里一条都没有的分类不给：合成候选全是非汉字，单字 / 词组都不出现，标签栏消失，
    /// Tab 回到它原本的绑定（出厂高亮下移 ⇒ 展开态下换行），而不是被一路跳过的循环吞掉。
    #[test]
    fn empty_categories_are_not_offered() {
        let (c, rx) = coord(true);
        seed(&c, PP * 3);
        c.handle_key_event(&key(keymap::VK_EQUAL));
        let (_, _, grid) = last_frame(&rx).unwrap();
        assert_eq!(grid.unwrap().tabs, 0, "没有可用分类 ⇒ 不显示标签栏");
        c.handle_key_event(&key(keymap::VK_TAB));
        let st = c.state.lock().unwrap();
        assert_eq!(st.grid_tab, GridTab::All);
        assert_eq!(st.candidates.len(), PP * 3);
        assert_eq!(st.current_page, 1, "Tab 照旧是高亮下移（展开态即换行）");
    }

    fn cand(text: &str) -> Candidate {
        Candidate {
            text: text.into(),
            ..Default::default()
        }
    }

    /// 分类判据：词组不收整句，符号不收英文词。
    #[test]
    fn category_rules() {
        let admits = |t, c: &Candidate| Coordinator::grid_tab_admits(t, c, true);
        let mut sentence = cand("时间不早了");
        sentence.is_sentence = true;
        assert!(admits(GridTab::Phrase, &cand("时间")));
        assert!(!admits(GridTab::Phrase, &sentence), "整句不算词组");
        assert!(!admits(GridTab::Phrase, &cand("时")));
        assert!(admits(GridTab::Single, &cand("时")));
        assert!(admits(GridTab::Symbol, &cand("😀")));
        assert!(admits(GridTab::Symbol, &cand("※")));
        assert!(!admits(GridTab::Symbol, &cand("shijian")), "英文词不算符号");
        assert!(!admits(GridTab::Symbol, &cand("时")));
    }

    /// 调位位置按完整列表计：子集 [b, d] 对应完整列表 [A, b, C, d]。
    #[test]
    fn reorder_positions_map_back_to_the_full_list() {
        let (c, _rx) = coord(true);
        let mut st = c.state.lock().unwrap();
        st.candidates = vec![cand("b"), cand("d")];
        st.grid_full = vec![cand("A"), cand("b"), cand("C"), cand("d")];
        st.grid_tab = GridTab::Single;
        let p0 = c.reorder_positions(&st, 0);
        assert_eq!(
            (p0.here, p0.prev, p0.next, p0.last),
            (1, 0, 3, 3),
            "后移落到下一个可见词 d 的位置"
        );
        let p1 = c.reorder_positions(&st, 1);
        assert_eq!(
            (p1.here, p1.prev, p1.next),
            (3, 1, 3),
            "前移落到上一个可见词 b 的位置"
        );
        // 没在筛选：与 idx ± 1 逐字一致。
        st.grid_tab = GridTab::All;
        st.grid_full.clear();
        let p = c.reorder_positions(&st, 1);
        assert_eq!((p.here, p.prev, p.next, p.last), (1, 0, 1, 1));
    }

    /// 退回完整列表（辅助码等要拿原始候选做快照的入口用）。
    #[test]
    fn restore_full_list() {
        let (c, _rx) = coord(true);
        let mut st = c.state.lock().unwrap();
        st.candidates = vec![cand("b")];
        st.grid_full = vec![cand("A"), cand("b")];
        st.grid_tab = GridTab::Single;
        c.grid_restore_full(&mut st);
        assert_eq!(st.grid_tab, GridTab::All);
        assert!(st.grid_full.is_empty());
        assert_eq!(st.candidates.len(), 2);
    }

    /// 用户把 Tab 另作他用（这里是整句切换键）时，展开态不截 Tab。
    #[test]
    fn tab_left_alone_when_bound_elsewhere() {
        let (c, _rx) = coord(true);
        assert!(
            c.tab_free_for_grid(false),
            "出厂 Tab = 高亮下移，可以让给分类"
        );
        let mut cfg = wind_config::Config::default();
        cfg.ui.candidate.grid_expand = true;
        cfg.schema.pinyin.sentence_cycle_key = "tab".into();
        let (c, _rx) = Coordinator::new_headless_with_ui(cfg, None);
        assert!(!c.tab_free_for_grid(false), "Tab 是整句切换键 ⇒ 不截");
    }

    /// 真实词库：常用字表在时给「生僻」，切过去只剩非常用（或手动标生僻）的汉字。
    ///
    /// 无头构造不会从数据目录装常用字表，这里注入一张小表：名单里的算常用，其余汉字生僻。
    /// 检索范围取「全部字符」：出厂智能档下同码有常用字就滤掉生僻字，完整列表里压根没有
    /// 生僻字，标签也就（正确地）不给。
    #[test]
    fn rare_category_keeps_only_uncommon_han() {
        if !data_dir().join("schemas/pinyin.schema.toml").exists() {
            return;
        }
        let mut cfg = wind_config::Config::default();
        cfg.input.filter_mode = "gb18030".into();
        cfg.schema.available = vec!["pinyin".into()];
        cfg.schema.active = "pinyin".into();
        cfg.input.default.chinese_mode = true;
        cfg.ui.candidate.grid_expand = true;
        let (c, _rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
        c.debug_mark_coords_ready();
        crate::charset_test_support::set_common_chars(&c, "以一亿已亦易矣乙译意依".chars());
        for ch in "YI".chars() {
            c.handle_key_event(&key(ch as u32));
        }
        c.handle_key_event(&key(keymap::VK_EQUAL));
        let mut st = c.state.lock().unwrap();
        assert!(
            c.grid_tabs(&st).contains(&GridTab::Rare),
            "常用字表在 ⇒ 给生僻"
        );
        assert!(c.grid_switch_tab(&mut st, GridTab::Rare));
        assert!(!st.candidates.is_empty());
        assert!(
            st.candidates
                .iter()
                .all(|x| !"以一亿已亦易矣乙译意依".contains(x.text.as_str())),
            "名单里的常用字不该出现在生僻里"
        );
    }

    /// 真实词库、每页 5 个：展开后按 6 上屏的是第二行行首（序号顺延到下一行，数字键跟着走）。
    #[test]
    fn number_key_follows_indices_onto_the_next_row() {
        if !data_dir().join("schemas/pinyin.schema.toml").exists() {
            return;
        }
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec!["pinyin".into()];
        cfg.schema.active = "pinyin".into();
        cfg.input.default.chinese_mode = true;
        cfg.ui.candidate.grid_expand = true;
        cfg.ui.candidate.per_page = 5;
        let (c, _rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
        c.debug_mark_coords_ready();
        for ch in "YI".chars() {
            c.handle_key_event(&key(ch as u32));
        }
        c.handle_key_event(&key(keymap::VK_EQUAL));
        let want = c.state.lock().unwrap().candidates[5].text.clone();
        match c.handle_key_event(&key(u32::from(b'6'))) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, want),
            other => panic!("按 6 应上屏第二行行首，实得 {other:?}"),
        }
    }
}
