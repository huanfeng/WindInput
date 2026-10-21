//! **候选的拉取式读取与绝对下标选词**：薄宿主（移动端）的候选接口。
//!
//! # 为什么推送帧不够用
//!
//! 协调器每次按键推送的 `UpdateCandidates` 是**当页**候选（`selected`/`hover` 的注释
//! 明写"页内下标"）。这套形状源自桌面：候选窗只画一页，数字键 1-9 选当页，`-`/`=` 翻页。
//!
//! 移动端的候选栏是一条可滚动的长列表，用户想点第几个点第几个。沿用推送帧的后果是
//! 三个看起来无关的症状，其实同一个根：
//! - 候选栏做了滚动，但一帧只有 6~9 个词，**滑动等于空转**；
//! - 「展开全部候选」的面板里也只有那 6~9 个，右上角却写着 `1/36`；
//! - 点选靠**合成数字键**（页内 1-9），于是永远选不到第 10 个及以后。
//!
//! 分页本身要保留：它决定空格上屏的目标与数字键语义。本模块只是把「读多少」和
//! 「选哪个」从视图分页里解放出来。
//!
//! # 性能：为什么是窗口而不是全量
//!
//! 全量候选可以很长（前缀匹配下拼音常有几百上千条）。每次按键把它整个跨 JNI 送一遍
//! 有三处代价，且**都落在主线程**：
//! 1. UniFFI 逐条序列化字符串；
//! 2. 宿主侧候选栏要为每条 `measureText` 才能算出命中区域；
//! 3. 读取期间持 state 锁，与引擎查询争用。
//!
//! 所以接口是 `(offset, limit)` 窗口，并在核心侧再压一道
//! [`MAX_WINDOW`] 硬上限——宿主传多大都不会真把全量搬过去。宿主按需续取：
//! 起手只要够铺满一两屏，用户滚到尾部再要下一段。

use wind_host::KeyOutcome;

use crate::coordinator::Coordinator;
use crate::edit_ops;

/// 单次拉取的候选条数硬上限。
///
/// 这是**核心侧的护栏**而不是建议值：宿主传 `u32::MAX` 也只会拿到这么多。
/// 取 200 的依据是横向候选栏一屏约 8 条、竖排展开面板一屏约 12 条，200 条足够
/// 连续滚动若干屏而不触发续取，同时序列化开销仍在一帧预算内。
pub const MAX_WINDOW: usize = 200;

/// 候选全量的一个窗口。
#[derive(Debug, Clone, Default)]
pub struct CandidateWindow {
    /// 窗口内的候选文本
    pub items: Vec<String>,
    /// 窗口起点（原样回送，供宿主校验自己拿到的是哪一段）
    pub offset: usize,
    /// **全量**候选总数（宿主据此判断要不要续取，以及画滚动条）
    pub total: usize,
    /// 当前高亮候选的**绝对**下标（空格上屏的目标）。
    ///
    /// 推送帧里的 `selected` 是页内下标，滚动列表用不了——那是"第几页的第几个"，
    /// 而这里要的是"整条列表里的第几个"。
    pub selected: usize,
    /// 与 [`Self::items`] 一一对应的候选注释（候选右侧灰字：码表的剩余编码、拼音候选的
    /// 反查码…，模板即 `ui.candidate.comment_template_*` 横排那一份）。只有
    /// [`Coordinator::candidate_window_with_comments`] 填它，[`Coordinator::candidate_window`]
    /// 恒空——注释要逐条求值（反查、注释库），不要的宿主不该为它付钱。
    pub comments: Vec<String>,
    /// 本次拉取触发了扩容重建（[`Coordinator::candidate_window_growing`]）。
    ///
    /// 重建的池更大，词频重排可能把池外的常用词提到前面——**宿主已取到的前段可能变了**。
    /// 宿主见此须整份重取已显示的范围，不能只把这一段接在旧列表后面，否则按下标选词会选错。
    pub rebuilt: bool,
}

impl Coordinator {
    /// 读取候选全量的一个窗口。**纯读**，不改任何状态（要「不够就扩容」见
    /// [`Self::candidate_window_growing`]）。
    ///
    /// `limit` 会被压到 [`MAX_WINDOW`]；`offset` 越界时返回空 `items`（但 `total`
    /// 仍然有效，宿主据此知道自己越界了而不是"候选没了"）。
    pub fn candidate_window(&self, offset: usize, limit: usize) -> CandidateWindow {
        self.candidate_window_impl(offset, limit, false, false)
    }

    /// 同 [`Self::candidate_window`]，另带每条候选的注释（[`CandidateWindow::comments`]）。
    ///
    /// 注释与桌面候选窗同一份模板与求值上下文（[`crate::coordinator::TooltipEvalCtx`]，
    /// 横排模板、`comment_max_chars(false)` 截断）：同一个候选，电脑上灰字写什么，手机上就写什么。
    pub fn candidate_window_with_comments(&self, offset: usize, limit: usize) -> CandidateWindow {
        self.candidate_window_impl(offset, limit, true, false)
    }

    /// 同上，但要的范围超出已加载的候选、且引擎还有更多（`has_more`）时，先扩容重建
    /// （[`Coordinator::expand_candidates`]，与桌面翻页到末页附近同一条路：上限翻倍、
    /// 保持高亮）——移动端候选栏「滑到尾部再取」靠它。触发了重建则 [`CandidateWindow::rebuilt`]。
    ///
    /// 这是本模块唯一会改状态的读口：首批只取够首屏（拼音单字母移动端 60 条，见
    /// `main_path_candidate_limit`），后面的要在用户真往后滑时才付钱。
    pub fn candidate_window_growing(
        &self,
        offset: usize,
        limit: usize,
        with_comments: bool,
    ) -> CandidateWindow {
        self.candidate_window_impl(offset, limit, with_comments, true)
    }

    fn candidate_window_impl(
        &self,
        offset: usize,
        limit: usize,
        with_comments: bool,
        grow: bool,
    ) -> CandidateWindow {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut rebuilt = false;
        // 只在「紧接着已加载的部分往后续取」时扩容：起点越过已加载末尾的请求是越界查询
        // （宿主据此判断自己越界），不能被悄悄扩容成有内容——宿主从来是连续往后取的
        if grow && offset <= state.candidates.len() {
            let want_end = offset.saturating_add(limit.min(MAX_WINDOW));
            // 每轮上限翻倍；expand 在引擎没给出新东西时会清 has_more，循环必然结束
            while state.has_more && state.candidates.len() < want_end {
                let before = (state.candidate_limit, state.candidates.len());
                self.expand_candidates(&mut state);
                if (state.candidate_limit, state.candidates.len()) == before {
                    break;
                }
                rebuilt = true;
            }
        }
        let total = state.candidates.len();
        let selected = self.highlighted_global_index(&state);
        let end = offset.saturating_add(limit.min(MAX_WINDOW)).min(total);
        let items = if offset >= total {
            Vec::new()
        } else {
            state.candidates[offset..end]
                .iter()
                .map(|c| self.cand_display_text(&state, c))
                .collect()
        };
        let comments = if with_comments && offset < total {
            self.comments_for(&state, offset, end)
        } else {
            Vec::new()
        };
        CandidateWindow {
            items,
            offset,
            total,
            selected,
            comments,
            rebuilt,
        }
    }

    /// `state.candidates[start..end]` 的注释（纯文本）。上下文与桌面候选页组装同源。
    fn comments_for(
        &self,
        state: &crate::coordinator::State,
        start: usize,
        end: usize,
    ) -> Vec<String> {
        let rt = self.rt();
        let behavior = self.engine_mgr.active_behavior();
        let tpl = crate::comment::Template::parse(
            self.comment_template_for(&rt.config, state, &behavior, false),
        );
        let max = rt.config.ui.candidate.comment_max_chars(false);
        let ctx = self.tooltip_eval_ctx(state, &rt.tooltip);
        let reverse = self.reverse.read().unwrap_or_else(|e| e.into_inner());
        state.candidates[start..end]
            .iter()
            .map(|c| {
                let dict_schema = self.comment_dict_scope(
                    state,
                    c,
                    ctx.mix_comment_scope,
                    &ctx.comment_dict_schema,
                );
                self.comment_for(c, &tpl, max, &reverse, ctx.hint_source, &dict_schema)
                    .as_str()
                    .to_string()
            })
            .collect()
    }

    /// **按绝对下标选词**，返回编辑指令流。
    ///
    /// 与 [`Coordinator::should_handle_key`] + `edit_ops::to_outcome` 是同一种形状：
    /// 宿主拿到 ops 就执行，不需要 push 通道。此前移动端点选候选靠合成数字键走按键路，
    /// 正是因为桌面的鼠标点选把上屏结果发进 push 管道，而 headless 那头没有消费端。
    ///
    /// 主输入路与**所有 overlay 模式**（临英 / 临拼 / 快捷输入 / 快符 / 生僻字走各自的键盘
    /// 选词出口；网址 / 邮箱 / Unicode 整串上屏）都返回编辑指令，宿主**须**按指令执行
    /// （分步时是改组合区，整串时是上屏）；内部那次 push 在 headless 下没有消费端，不会重复。
    ///
    /// 只有越界、`$CC` 命令候选（已异步执行）、组折叠候选（已就地展开）返回 `passthrough`
    /// （`consumed=false`、无 ops）。
    pub fn select_candidate(&self, index: usize) -> KeyOutcome {
        match self.select_candidate_at(index) {
            Some(action) => edit_ops::to_outcome(action),
            None => KeyOutcome::passthrough(),
        }
    }
}
