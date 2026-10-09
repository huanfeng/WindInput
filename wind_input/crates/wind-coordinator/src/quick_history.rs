//! 快捷输入历史（成员 `quick_input.history`，t46）：在快捷输入里上屏过的字面文本，
//! 下次按前缀补全。
//!
//! # 为什么是一个成员，而不是另一套融合规则
//!
//! 快捷输入的融合规则本来就是「成员顺序 = 候选优先级 + 各成员配额」。历史作为成员加入，
//! 它与英文、拼音谁先谁后由用户在成员列表里排，不必为它另立一套排序；成员在列表里即启用，
//! **成员本身就是开关**（出厂不在任何实例里）。
//!
//! # 存储：补全表的第三个分区
//!
//! 形态与网址历史完全相同——「一条文本 + 次数 + 最后一次」，按前缀补全——所以落在
//! `wind_store::completion` 的 [`CompletionKind::QuickHistory`]：全局、不分方案，自带裁剪、
//! 设置端数据页（`quickHistory.*`）与整机备份。**不进 `FREQ` 表**：那张表的 key 必须带方案
//! 与码，历史串没有码。条数上限 `schema.quick_input.history_max`（默认 1000），超出按
//! 「次数少、用得旧」裁掉，不按时间过期。
//!
//! # 匹配：缓冲当字面前缀，不当编码
//!
//! 符号与字母同等对待（`${re` 命中 `${reset_time}`）。**存储区分大小写**（`RESET_TIME` 与
//! `reset_time` 是两条），**匹配不区分**（打 `res` 两条都出，按次数 / 最近排）。
//! 它是唯一一个**跨透镜**的成员：其它成员把缓冲当编码、只在特定透镜下查；历史当字面，
//! 文本 / 数字 / Free / 大写英文透镜都查——否则 `$`、`.` 起手先落进数字透镜，
//! `${reset_time}`、`.txt` 就召不回来。
//!
//! # 排位（与英文等成员的融合）
//!
//! | 透镜 | 历史的位置 |
//! |---|---|
//! | Free / 大写英文 | **首位**，后接英文段；原文的去留见 `mix_free_with_history`（大小写档位键照常作用于英文段） |
//! | 文本 / 数字 | 成员列表里它所在的位置，配额 [`QUICK_HISTORY_QUOTA`] |
//!
//! Free 透镜置首的理由：缓冲已不是任何成员的合法编码，最可能要的就是「之前打过的那串」；
//! 而 Free 透镜没有数字选词键，原文占首位的话历史只能靠方向键挪过去选，置顶就落空了。
//! 原文在组合区里本就看得见，有历史命中时通常不再单列（例外：英文词跟随临英「原文候选」，
//! 见 `mix_free_with_history`）。「打什么上屏什么」由回车兜底——空格上屏高亮、**回车恒上屏
//! 原文**，与网址 / 邮箱模式同一套分工（`mode_completion.rs` 文件头）。没有历史命中时 Free
//! 透镜的候选一字不变。
//!
//! ⚠️ 历史候选**不经过 `finalize_candidates`**：那里会把含 `$` / `{` 的文本当词库特殊语法
//! 展开，`${reset_time}` 会被改写甚至整条丢掉。所以在展开之后才插进去。
//!
//! # 记什么（写端）：按「上屏的是哪个候选」判，不按文本长什么样判
//!
//! - 记：Free / 大写英文 / 词组透镜下的原文上屏（回车、空格兜底、选中原文候选、标点顶屏）
//!   与历史候选本身（次数 +1）；
//! - 不记：文本透镜的原文（纯小写字母本身就是拼音 / 英文编码，见 `learn_mix_literal`）、
//!   成员给出的候选（英文词只记英文词频，t46 楼主说英文不需要；拼音走拼音造词）、
//!   数字透镜产出（日期 / 计算 / 数字 / 金额）、重复上屏候选、已有分步上屏段时的上屏；
//! - 门槛见 [`worth_remembering`]。

use crate::coordinator::{Coordinator, State};
use crate::handle_mode::MixLens;
use wind_candidate::Candidate;
use wind_store::completion::CompletionKind;

/// 历史候选的 `Candidate::id` 标记。上屏出口凭它认出「这是一条历史」。
pub(crate) const QUICK_HISTORY_ID: &str = "quick_input.history";

/// 一次最多给几条历史候选。
///
/// Free 透镜里历史排在原文前面，给多了原文会被挤到第二页；文本 / 数字透镜里它与其它成员
/// 并列，同生僻字成员「最多一页」的理由：别把其余成员整个挤到翻页之外。
pub(crate) const QUICK_HISTORY_QUOTA: usize = 5;

/// 值不值得记进历史：至少 2 个字符、含字母（纯数字、纯标点不记），且不含空白。
///
/// 纯数字与金额重复输入相同值的可能很小，补出来反而可能造成数据错误（t46 10-01 楼）；
/// 单字符补全没有意义。
pub(crate) fn worth_remembering(text: &str) -> bool {
    text.chars().count() >= 2
        && text.chars().any(char::is_alphabetic)
        && !text.chars().any(char::is_whitespace)
}

impl Coordinator {
    /// 本融合实例是否启用了历史成员。
    pub(crate) fn mix_has_history(&self, idx: u8) -> bool {
        self.rt()
            .config
            .schema
            .mix_modes
            .get(idx as usize)
            .is_some_and(|m| {
                m.members
                    .iter()
                    .any(|s| s == wind_quick_input::MEMBER_HISTORY)
            })
    }

    /// 按缓冲前缀（不区分大小写）取历史候选，至多 [`QUICK_HISTORY_QUOTA`] 条。
    ///
    /// `skip_exact`：与缓冲逐字相同的那条不出——Free 透镜里原文本就在候选里，同一段文字
    /// 出两次只会占掉一个位置（同网址历史）。
    pub(crate) fn quick_history_candidates(
        &self,
        state: &State,
        skip_exact: bool,
    ) -> Vec<Candidate> {
        let buffer = &state.mix_buffer;
        // 已有分步上屏段时不出：记录端同样不记这种状态（见 `learn_mix_literal`），
        // 出了也只会把「中文段 + 历史」拼成一次上屏，分段里的造词就丢了。
        if buffer.is_empty()
            || !state.committed_text.is_empty()
            || !self.mix_has_history(state.mix_id)
        {
            return Vec::new();
        }
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        // 全量取出再按小写前缀过滤：store 只提供区分大小写的前缀扫描，而条数有
        // `history_max` 封顶（出厂 1000），逐键一次读事务可以接受。
        let rows = match store.list_completions(CompletionKind::QuickHistory, "", 0, 0) {
            Ok((rows, _)) => rows,
            Err(e) => {
                tracing::debug!("快捷输入历史读取失败: {e}");
                return Vec::new();
            }
        };
        let want = buffer.to_lowercase();
        let cand = |text: String| Candidate {
            text,
            id: QUICK_HISTORY_ID.to_string(),
            ..Default::default()
        };
        // 与缓冲逐字相同的那条（用户把一条记过的历史完整打了出来）：`skip_exact` 时不出，
        // 否则**排首位且不占配额**——它就是原文本身，占了名额其余历史只剩 4 条。
        let exact = !skip_exact && rows.iter().any(|(text, _)| text == buffer);
        let mut out: Vec<Candidate> = Vec::with_capacity(QUICK_HISTORY_QUOTA + 1);
        if exact {
            out.push(cand(buffer.clone()));
        }
        out.extend(
            rows.into_iter()
                .filter(|(text, _)| text != buffer)
                .filter(|(text, _)| text.to_lowercase().starts_with(&want))
                .take(QUICK_HISTORY_QUOTA)
                .map(|(text, _)| cand(text)),
        );
        out
    }

    /// Free / 大写英文透镜的候选（`seg` = 原文 + 英文段）并上历史。没有历史命中时原样返回。
    ///
    /// 有命中时原文怎么处理（维护者 2026-10-07 定）：
    /// - **缓冲是英文词**（纯字母、带大写，`RESET` / `Hel`）：原文跟随临英的「原文候选」
    ///   开关（`input.temp_english.raw_candidate`，出厂 Always）——要原文时它**排首位**、
    ///   历史在其后，与临英「首候选是所打原文」一致；不要时去掉原文，历史置首。
    /// - **其余**（带符号，`${re` / `RESET_TIME`）：去掉原文、历史置首。原文在组合区里本就
    ///   看得见，回车恒上屏它；排在历史下面反而怪（实机反馈）。
    ///
    /// `raw_pinned` = `seg` 首格是按开关钉上去的原文（`mix_free_english_segment` 已按临英判据
    /// 判过，这里不再重判一遍——两处各判一次曾在 `InDict` 上分叉：一边按字面、一边忽略大小写）。
    pub(crate) fn mix_free_with_history(
        &self,
        state: &State,
        seg: Vec<Candidate>,
        raw_pinned: bool,
    ) -> Vec<Candidate> {
        use wind_candidate::CandidateSource;
        let hist = self.quick_history_candidates(state, false);
        if hist.is_empty() {
            return seg;
        }
        let raw = state.mix_buffer.as_str();
        // 原文格**按位置认**：钉了原文时 `seg` 首格就是它——`mix_free_english_segment` 先放
        // 头部、去重保留首次出现。按文本认会在两处失手：头部格被同名词库词占据
        // （`merge_head_with_dict`），以及大小写档位把它改写成 `HEL` / `hel`。
        // 逐字相同的历史（若有）排在 `hist` 首位，它就是原文本身，不再另列原文格。
        let exact = hist.first().is_some_and(|h| h.text == raw);
        let raw_first = raw_pinned && !exact;
        let mut out: Vec<Candidate> = Vec::with_capacity(hist.len() + seg.len());
        let mut rest = seg.into_iter();
        if let Some(first) = rest.next() {
            if raw_first {
                out.push(first);
                out.extend(hist);
            } else {
                out.extend(hist);
                // 不单列原文时只去掉**纯原文**（来源为空）；占据头部格的同名词库词照常出
                // ——临英关掉原文候选时词库词同样会出。
                //
                // 首格是不是原文：钉了原文时按位置认（大小写档位可能已把它改写成 `HEL`）；
                // 没钉时首格可能是补位原文，也可能是同样无来源的大小写变形（原文候选关、
                // 变形开）——变形按定义不等于原文，补位原文在档位之后才放、文本恒等于缓冲，
                // 故按文本认。
                let is_raw =
                    first.source == CandidateSource::None && (raw_pinned || first.text == raw);
                if !is_raw && !out.iter().any(|o| o.text == first.text) {
                    out.push(first);
                }
            }
        } else {
            out.extend(hist);
        }
        for c in rest {
            if !out.iter().any(|o| o.text == c.text) {
                out.push(c);
            }
        }
        out
    }

    /// 当前页第 `page_local` 格若是历史候选，返回它的文本（右键菜单与菜单动作共用这一判据）。
    pub(crate) fn quick_history_at(&self, state: &State, page_local: usize) -> Option<String> {
        let (start, end) = self.page_range(state);
        let idx = start + page_local;
        if idx >= end {
            return None;
        }
        state
            .candidates
            .get(idx)
            .filter(|c| c.id == QUICK_HISTORY_ID)
            .map(|c| c.text.clone())
    }

    /// 右键「删除此历史」：从补全表里删掉这一条，并立即重算候选（不刷新的话用户得退出重进
    /// 才看得到它消失）。走 mix 路径——历史只出现在快捷输入里。
    pub(crate) fn delete_quick_history(&self, text: &str) {
        if let Some(store) = self.store.as_ref()
            && let Err(e) = store.remove_completion(CompletionKind::QuickHistory, text)
        {
            tracing::warn!("快捷输入历史删除失败: {e}");
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(state.active, Some(crate::pipeline::ModeKind::Mix(_))) {
            self.update_mix_candidates(&mut state);
            self.notify_ui_update(&state);
        }
    }

    /// 记一条快捷输入历史并裁剪到上限。未启用历史成员、或文本不值得记时什么也不做。
    pub(crate) fn learn_quick_history(&self, state: &State, text: &str) {
        if !self.mix_has_history(state.mix_id) || !worth_remembering(text) {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if let Err(e) = store.record_completion(CompletionKind::QuickHistory, text) {
            tracing::debug!("快捷输入历史写入失败: {e}");
            return;
        }
        let max = self.rt().config.schema.quick_input.history_max as usize;
        // 传 `keep` 保护刚写入的这条——否则表一满就再也学不进新的（见 `prune_completions`）。
        if let Err(e) = store.prune_completions(CompletionKind::QuickHistory, max, Some(text)) {
            tracing::debug!("快捷输入历史裁剪失败: {e}");
        }
    }

    /// 选中的候选是不是「原文」：文本与缓冲逐字相同，且不是词库词（英文头部候选能找回
    /// 对应词库词的，算词库词——英文词只记英文词频，不进历史）。
    pub(crate) fn mix_cand_is_literal(&self, state: &State, cand: &Candidate) -> bool {
        use wind_candidate::CandidateSource;
        // 先判成员：没启用历史时不该为这条判据去查英文词库（`mix_freq_candidate`）。
        self.mix_has_history(state.mix_id)
            && cand.source == CandidateSource::None
            && cand.text == state.mix_buffer
            && !matches!(
                self.mix_freq_candidate(state, cand),
                Some(c) if c.source != CandidateSource::None
            )
    }

    /// 原文上屏（回车、空格兜底、选中原文候选）时记历史。
    ///
    /// 只记**整段都是字面输入**的那次，且只在 Free / 大写英文 / 词组透镜下记：
    /// - 已有分步上屏段（`committed_text` 非空）时上屏的是「中文段 + 剩余码」，剩余码不是
    ///   用户想留的那串；
    /// - 数字透镜的缓冲是算式或数字；
    /// - **文本透镜不记**：缓冲是纯小写字母，本身就是拼音 / 英文的合法编码。回车上屏的
    ///   `nihao`、`test` 记进来，下次打 `;te` 首选就成了 `test` 而不是「特」。t46 要的是
    ///   带符号、带大写的标识符，它们都落在 Free / 大写英文透镜。
    ///
    /// 必须在 `exit_mix_mode` 清缓冲之前调用。
    pub(crate) fn learn_mix_literal(&self, state: &State) {
        if !state.committed_text.is_empty()
            || matches!(self.mix_lens(state), MixLens::Numeric | MixLens::Text)
        {
            return;
        }
        let text = state.mix_buffer.clone();
        self.learn_quick_history(state, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::worth_remembering;

    #[test]
    fn threshold() {
        assert!(worth_remembering("${reset_time}"));
        assert!(worth_remembering("RESET_TIME"));
        assert!(worth_remembering("ab"));
        assert!(!worth_remembering("a"), "单字符不记");
        assert!(!worth_remembering("123456"), "纯数字不记");
        assert!(!worth_remembering("..."), "纯标点不记");
        assert!(!worth_remembering("a b"), "含空白不记");
    }
}
