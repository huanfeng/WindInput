//! 候选视图导航：分页 / 高亮移动 / 悬停清除 / 末页检索范围临时放宽。
//!
//! （自 coordinator.rs 平移，纯搬运。统一分发入口 `apply_session_action` 仍在
//! coordinator.rs——它同时分发 cancel 等非导航动作。）

use crate::coordinator::{Coordinator, State};
use crate::pipeline::ModeKind;

impl Coordinator {
    /// 每页候选数（来自配置，至少 1）
    pub(crate) fn per_page(&self, active: Option<ModeKind>) -> usize {
        let bundle = self.rt();
        let cand = &bundle.config.ui.candidate;
        // overlay 模式(临拼/快捷/短语/临英等,state.active 非空)用扩展档(配置>0 时)。
        //
        // **辅助码除外**：它的候选就是主路径候选被筛过一轮，呈现形态应与主路径一致
        // （`layout::intent_for` 的 AuxCode 臂返回 `None` 表达的正是这个意图）。
        // 走扩展档会让候选窗在按下触发键的瞬间从 per_page 跳到 per_page_extended，
        // 「只是筛选」的视觉承诺当场破功。出厂 `per_page_extended = 0` 时不可见，
        // 配了扩展档的用户才会撞上——同一意图两处判据不一致，迟早漂移。
        if active.is_some_and(|m| m != ModeKind::AuxCode) && cand.per_page_extended > 0 {
            cand.per_page_extended.max(1)
        } else {
            cand.per_page.max(1)
        }
    }

    /// 总页数（至少 1）
    pub(crate) fn total_pages(&self, state: &State) -> usize {
        let pp = self.per_page(state.active);
        state.candidates.len().div_ceil(pp).max(1)
    }

    /// 清除鼠标悬停目标（无需 state 锁，见 [`Coordinator::hover_index`] 的说明）。
    ///
    /// 调用点＝一切「悬停不再对应屏幕上任何东西」的时刻：候选窗隐藏、候选列表重新装填、
    /// 键盘移动高亮/翻页。少接一处的后果是**静默的**——悬停高亮与 tooltip 会在下一次候选窗
    /// 出现时凭空复现，且鼠标从未移动过。
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
    /// # ★★ 三件事必须一起做
    ///
    /// 此前只有主路径 `update_candidates` 三件齐全，特殊模式 / 临拼 / 混输 / 快捷输入的
    /// 8 个装填点都只做了前两件——漏掉的第三件让悬停高亮与 tooltip 跨按键、跨组合、跨模式
    /// 存活（2026-08-12 用户反馈）。而普通输入每敲一键都重走主路径把残留覆盖掉，
    /// **该缺陷在主路径上物理不可观测**，只有 overlay 模式才露馅。
    ///
    /// 收进一处后，新增候选来源时能漏的只剩「忘了调用本函数」——比在三行里少写一行显眼得多。
    pub(crate) fn reset_candidate_view(&self, state: &mut State) {
        state.current_page = 0;
        // 翻页史随候选一起作废：新装填的是另一批候选，上一批翻到过第几页与它无关。
        // 挂在这里而不是各装填点，理由同上面那三件——`current_page` 归零的地方就是它
        // 该归零的地方。生产代码里 `current_page` 的写点全在本文件，别处的都是测试。
        state.paged = false;
        state.selected_index = 0;
        self.clear_hover();
    }

    /// 翻到第 `page` 页并记下「这批候选被翻过页」。
    ///
    /// 存在的理由只有一个：`current_page` 有五个写点（页内回卷两个、翻页键两个、
    /// 末页放宽一个），而 `paged` 必须与它们**每一个**同步。分散成五行 `paged = true`
    /// 是在等着下一个新写点漏掉——漏掉的表现是 `-` 在用户明明翻过页之后又变回字符，
    /// 只在某一条特定翻页路径上复现。
    ///
    /// ⚠️ 不含 `try_relax_scope_on_page_end` 里那处**还原** `page_before`：还原不是翻页，
    /// 那一帧若置位会把「临拼下重建候选前本来就在第 0 页」误记成翻过页。
    ///
    /// # 五个写点里只有三个有独立效果，另两个是刻意的冗余
    ///
    /// 往**上**抬页码的三处（`move_down` 回卷 / `page_next` / 末页放宽）各自都能单独把
    /// `paged` 从 false 翻成 true，删掉任一处都有用例变红。往**下**降的两处
    /// （`move_up` 回卷 / `page_prev`）则永远只是重复置位——页码降得下去，就说明它先被
    /// 抬上去过，那一帧已经置过了。
    ///
    /// 它们照样走这个函数，是因为**判据不该按写点分叉**：留两处裸赋值，下一个人照着最近
    /// 的先例加第六个写点时，抄到的可能正是那两处。所以「删掉它们没有用例会红」是设计的
    /// 一部分，不是漏测——别为此去补一条只能靠白盒读 `paged` 才写得出的用例。
    fn turn_page(state: &mut State, page: usize) {
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
        // ⚠️ 判据是「**当前模式的**输入缓冲已空」。临拼的码在 `temp_pinyin_buffer`，
        // 而它的 `input_buffer` 恒为空——用 `input_buffer` 一刀切会让临拼刚放宽就在下一次
        // 按键被清掉，且**静默**（用户只看到「按了没用」）。退出临拼后 `active` 已变回
        // 非 TempPinyin，走 `input_buffer` 分支照常失效。
        let ended = if matches!(s.active, Some(ModeKind::TempPinyin)) {
            s.temp_pinyin_buffer.is_empty()
        } else {
            s.input_buffer.is_empty()
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
        // 已放宽过就不再重复。放宽是**智能档专属**的补偿：只有智能档会按「同码位有常用字」
        // 滤掉生僻字，也只有它需要一条把被滤掉的放回来的出路（见上方引用的设计文档，全篇
        // 以 `filter_mode = "smart"` 为前提）。常用字档若也能放宽，它与智能档的差异就被
        // 抹平了——用户选「常用字」要的正是一个稳定只出常用字的列表；`Gb18030` 本就不过滤，
        // 更无可放宽。
        if state.scope_relaxed || state.filter_mode != wind_candidate::FilterMode::Smart {
            return false;
        }
        // 辅助码模式：放宽是智能档「常用字」的补偿，辅助码按字形筛、不适用——且放宽会重建
        // 整池候选、绕过辅助码筛选（过滤结果丢失），跳过。
        if matches!(state.active, Some(ModeKind::AuxCode)) {
            return false;
        }
        // ⚠️ 临拼的码在 `temp_pinyin_buffer`，主路径的在 `input_buffer`——须按当前模式取。
        // 用 `input_buffer` 一刀切会让临拼**永远触发不了**（那边恒为空），且没有任何报错。
        let in_temp = matches!(state.active, Some(ModeKind::TempPinyin));
        let has_input = if in_temp {
            !state.temp_pinyin_buffer.is_empty()
        } else {
            !state.input_buffer.is_empty()
        };
        if !has_input {
            return false;
        }
        state.scope_relaxed = true;
        // 两条路径的候选重建函数不同：临拼走 overlay 的那套（主路径的 `build_candidates`
        // 读 `input_buffer`，在临拼下会构建出空列表）。
        // `paged` 与 `current_page` 一起存还：下面 `update_temp_pinyin_candidates` 内部会经
        // `reset_candidate_view` 把 `paged` 清成 false，只还原页码的话就留下
        // 「`current_page > 0` 而 `paged == false`」这个错位，`turn_page` 文档里承诺的
        // 「同生命周期」当场破功。今天无害（本状态位只有普通模式的符号闸门在读，而
        // overlay 在按键分派处就 return 了），但不变式破一个口子就不再是不变式。
        let page_before = state.current_page;
        let paged_before = state.paged;
        if in_temp {
            // ⚠️ `update_temp_pinyin_candidates` 会把 current_page/selected_index 归零，
            // 重建后须还原，否则用户翻到的位置丢失。
            self.update_temp_pinyin_candidates(state);
            state.current_page = page_before;
            state.paged = paged_before;
        } else {
            let limit = state.candidate_limit;
            self.build_candidates(state, limit);
        }
        // 判据取「列表里有没有真的出现被滤候选」，而非「总数是否变多」——候选受 limit 截断时
        // 总数可能不变，那样会误判成「没放出东西」而撤销。
        if !state.candidates.iter().any(|c| c.is_scope_filtered) {
            // 该码位本就没有被滤的字 → 原样撤销，别留一个什么也没带来、却会影响后续按键的放宽态
            state.scope_relaxed = false;
            return false;
        }
        // 放宽出来的候选**追加在末尾**，所以照常翻到下一页就能看到，与「继续往后翻」的动作
        // 语义完全一致。⚠️ 曾让放宽后的候选按真实顺序插入，结果 `dwi` 的新字（权重 8999 占
        // 三简位）落到第 1 页第 2 位，视口只能跳回页首——翻页翻着翻着跳回开头，很突兀。
        if state.current_page + 1 < self.total_pages(state) {
            Self::turn_page(state, state.current_page + 1);
            state.selected_index = 0;
        }
        true
    }
}
