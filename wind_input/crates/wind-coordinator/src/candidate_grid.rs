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
use crate::handle_mode::MixLens;
use crate::pipeline::ModeKind;
use wind_bridge::handler::KeyEventData;
use wind_candidate::Candidate;
use wind_ipc::protocol::{MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;
use wind_ui_types::{GRID_TAB_TAG_BASE, GridTab};

/// 这条候选是不是 shadow **之后**才插进列表的：简繁变体（`expand_s2t_variants`）、emoji
/// 扩展、单字母重复上屏项（`refill_candidates` 插在首位）。读端 `apply_shadow` 看不到它们，
/// 调位写下标时不能把它们算进去。
pub(crate) fn inserted_after_shadow(c: &Candidate) -> bool {
    c.s2t_override.is_some() || c.is_emoji_suggestion || is_leading_repeat(c)
}

/// 单字母引导的重复上屏项（`refill_candidates` 插在首位）。它不是这个码的候选，不给调位。
pub(crate) fn is_leading_repeat(c: &Candidate) -> bool {
    c.natural_order == crate::handle_candidate::LEADING_REPEAT_ORDER
        && c.source == wind_candidate::CandidateSource::None
        && c.code.is_empty()
}

/// 完整列表下标 → 读端位置：`[..=fi]` 里读端看得见的条数减一（后插入项取它前面最近那条）。
fn read_rank(full: &[Candidate], fi: usize) -> usize {
    full[..=fi.min(full.len().saturating_sub(1))]
        .iter()
        .filter(|c| !inserted_after_shadow(c))
        .count()
        .saturating_sub(1)
}

/// 「固定到第 N 位」的落点（见 [`Coordinator::pin_at_slots`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinAtSlots {
    /// `slots[k]` = 落到**界面**第 k+1 位要写的读端 position；`None` = 已在该位，或那一格
    /// 落不到（它后面紧跟的是变体 / emoji 等后插入项，被钉的词插不进它与宿主之间）。
    pub(crate) slots: Vec<Option<usize>>,
    /// 界面候选总条数（完整列表，含后插入项），菜单子项不超过它。
    pub(crate) ui_len: usize,
    /// 列表里有 emoji 扩展候选：此时 `slots` 全为 `None`。emoji 挂在前 `max_hosts` 条之后（或
    /// 常用候选之后的末尾档），词一挪 emoji 就改挂别处，界面名次没法稳定折算，干脆不给。
    pub(crate) has_emoji: bool,
}

impl PinAtSlots {
    /// 界面第 `n` 位（1 起）要写的读端 position。
    pub(crate) fn position_for(&self, n: u8) -> Option<usize> {
        self.slots
            .get((n as usize).checked_sub(1)?)
            .copied()
            .flatten()
    }
}

/// 调位位置（见 [`Coordinator::reorder_positions`]），均为读端（shadow 应用时）候选列表里的下标。
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
    /// 只看 `grid_top` 不够：上屏等出口只清候选、不经 `reset_candidate_view`，`grid_top`
    /// 会残留，组合结束后 ←→ / Esc 会被吞。下一次装填必经 `reset_candidate_view`，故判据
    /// 收在这里即可。
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
        // 范围见设计 §9：快捷输入只开文本 / 词组透镜（数字透镜与格式表依赖单页，自由透镜
        // 只有原文与历史）；辅助码本身就在缩小候选、特殊模式各有自己的码表语义，都不展开。
        let mode_ok = match state.active {
            None | Some(ModeKind::TempPinyin) | Some(ModeKind::TempEnglish) => true,
            Some(ModeKind::Mix(_)) => {
                matches!(self.mix_lens(state), MixLens::Text | MixLens::Phrase)
            }
            Some(
                ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::Special(_)
                | ModeKind::RareChar
                | ModeKind::Reverse
                | ModeKind::AuxCode,
            ) => false,
        };
        if !mode_ok {
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

    /// 展开态的翻页键：绑了「翻屏」动词（`page_screen_*`）的键恒翻一整屏；PageUp/PageDown 在
    /// `grid_pageupdown_screen` 开着时也翻一整屏；其余翻页键逐行滚（同滚轮）。
    /// 物理键的判断不管它绑在哪个组。
    pub(crate) fn grid_page_key(
        &self,
        state: &mut State,
        down: bool,
        key_code: u32,
        screen_verb: bool,
    ) -> bool {
        let screen = screen_verb
            || (matches!(key_code, keymap::VK_PRIOR | keymap::VK_NEXT)
                && self.rt().config.ui.candidate.grid_pageupdown_screen);
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
    /// 而且只需接这一处。快捷输入例外：它的光标与 Esc 臂早于该函数，见 `handle_mix_key`。
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
            .active_sentence_cycle_key()
            .is_some_and(|k| k.vk == keymap::VK_TAB && k.shift == shift);
        default_like && !sentence_key
    }

    /// 可见区覆盖的候选绝对下标范围 `[start, end)`；没展开返回 `None`。
    pub(crate) fn grid_visible_range(&self, state: &State) -> Option<(usize, usize)> {
        let top = state.grid_top?;
        let pp = self.page_size(state);
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
        let pp = self.page_size(state);
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
            let want = 2 * self.grid_rows() * self.page_size(state);
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
        let pp = self.page_size(state);
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
    ///
    /// 完整列表里还要**跳过 shadow 之后才插入的项**（[`inserted_after_shadow`]）：读端
    /// `apply_shadow` 解释 position 用的是插入之前的列表，按 `candidates` 的下标写，开了简繁
    /// 时单字之后的候选每过一条变体就偏大一格。这类项自身的位置取它前面最近的那一条（变体即
    /// 它的原字）。
    pub(crate) fn reorder_positions(&self, state: &State, idx: usize) -> ReorderPositions {
        let full = Self::reorder_full_list(state);
        let at = |i| Self::full_index_of(state, i);
        let last = full
            .iter()
            .filter(|c| !inserted_after_shadow(c))
            .count()
            .saturating_sub(1);
        let here = at(idx)
            .map(|fi| read_rank(full, fi))
            .unwrap_or(idx.min(last));
        // 前一个 / 后一个可见、且读端位置不同的那一条（跳过同一原字的变体）。
        let prev = (0..idx)
            .rev()
            .filter_map(|i| at(i).map(|fi| read_rank(full, fi)))
            .find(|&r| r < here)
            .unwrap_or(here.saturating_sub(1));
        let next = (idx + 1..state.candidates.len())
            .filter_map(|i| at(i).map(|fi| read_rank(full, fi)))
            .find(|&r| r > here)
            .unwrap_or((here + 1).min(last));
        ReorderPositions {
            here,
            prev,
            next,
            last,
        }
    }

    /// 调位口径下的完整列表：分类筛选中是 `grid_full`，否则就是 `candidates`。
    fn reorder_full_list(state: &State) -> &[Candidate] {
        if state.grid_tab != GridTab::All && !state.grid_full.is_empty() {
            &state.grid_full
        } else {
            &state.candidates
        }
    }

    /// 可见下标 → [`Self::reorder_full_list`] 里的下标。
    fn full_index_of(state: &State, i: usize) -> Option<usize> {
        let full = Self::reorder_full_list(state);
        if std::ptr::eq(full, state.candidates.as_slice()) {
            return (i < full.len()).then_some(i);
        }
        let c = state.candidates.get(i)?;
        full.iter().position(|f| {
            f.text == c.text && f.code == c.code && f.id == c.id && f.source == c.source
        })
    }

    /// 读端第 `rank` 位的那条候选（前移 / 后移查邻居有没有自己的固定规则用）。
    pub(crate) fn candidate_at_read_rank(&self, state: &State, rank: usize) -> Option<Candidate> {
        Self::reorder_full_list(state)
            .iter()
            .filter(|c| !inserted_after_shadow(c))
            .nth(rank)
            .cloned()
    }

    /// 「固定到第 N 位」的 N 指**界面名次**（完整列表里的第 N 格，含简繁变体等后插入项），
    /// 写下去的是读端 position。折算：把被钉的词（连同紧随其后的简繁变体）从完整列表摘掉得
    /// `L`，落到界面第 k+1 格 = 在 `L[k]` 之前，读端 position = `L[..k]` 里读端看得见的条数。
    /// `L[k]` 是后插入项时落不到（它们紧跟宿主），该格给 `None`；当前所在那格也给 `None`。
    ///
    /// 点的是变体时按它的原字算（写端规则键本就是原字）。菜单与写端同用本函数。
    pub(crate) fn pin_at_slots(&self, state: &State, idx: usize) -> PinAtSlots {
        let full = Self::reorder_full_list(state);
        let mut fi = Self::full_index_of(state, idx).unwrap_or(idx.min(full.len()));
        while fi > 0 && fi < full.len() && full[fi].s2t_override.is_some() {
            fi -= 1;
        }
        let mut end = (fi + 1).min(full.len());
        while end < full.len() && full[end].s2t_override.is_some() {
            end += 1;
        }
        let has_emoji = full.iter().any(|c| c.is_emoji_suggestion);
        let rest: Vec<&Candidate> = full[..fi].iter().chain(&full[end..]).collect();
        let slots = (0..=rest.len())
            .map(|k| {
                if has_emoji || k == fi || rest.get(k).is_some_and(|c| inserted_after_shadow(c)) {
                    return None;
                }
                Some(
                    rest[..k]
                        .iter()
                        .filter(|c| !inserted_after_shadow(c))
                        .count(),
                )
            })
            .collect();
        PinAtSlots {
            slots,
            ui_len: full.len(),
            has_emoji,
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
        let pp = self.page_size(state);
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
#[path = "candidate_grid_tests.rs"]
mod tests;
