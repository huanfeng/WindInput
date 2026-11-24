//! 快捷输入历史（成员 `quick_input.history`）：在快捷输入里上屏过的字面文本，下次按前缀补全。
//!
//! 作为成员而非另一套融合规则：与英文、拼音谁先谁后由用户在成员列表里排，成员在列表里即启用
//! （出厂不在任何实例里）。
//!
//! # 存储
//!
//! 落在 `wind_store::completion` 的 [`CompletionKind::QuickHistory`]（与网址历史同形：文本 +
//! 次数 + 最后一次）：全局、不分方案，自带裁剪、设置端数据页（`quickHistory.*`）与整机备份。
//! 不进 `FREQ` 表：那张表的 key 必须带方案与码。上限 `schema.quick_input.history_max`，超出按
//! 「次数少、用得旧」裁掉。
//!
//! # 匹配：缓冲当字面前缀，不当编码
//!
//! 符号与字母同等对待（`${re` 命中 `${reset_time}`）。存储区分大小写，匹配不区分。它是唯一跨
//! 透镜的成员：文本 / 数字 / Free / 大写英文透镜都查，否则 `$`、`.` 起手落进数字透镜就召不回来。
//!
//! # 排位
//!
//! | 透镜 | 历史的位置 |
//! |---|---|
//! | Free / 大写英文 | 首位，后接英文段；原文的去留见 `mix_free_with_history` |
//! | 文本 / 数字 | 成员列表里它所在的位置，配额 [`QUICK_HISTORY_QUOTA`] |
//!
//! Free 透镜置首：缓冲已不是任何成员的合法编码，最可能要的就是之前打过的那串，且该透镜没有
//! 数字选词键。「打什么上屏什么」由回车兜底（与网址 / 邮箱同一套分工，见 `mode_completion.rs`）。
//! 没有历史命中时 Free 透镜的候选一字不变。
//!
//! 历史候选不经过 `finalize_candidates`：那里会把含 `$` / `{` 的文本当词库特殊语法展开，
//! `${reset_time}` 会被改写甚至丢掉，所以在展开之后才插进去。
//!
//! # 记什么：按「上屏的是哪个候选」判，不按文本长什么样判
//!
//! - 记：Free / 大写英文 / 词组 / 文本透镜下的原文上屏（回车、空格兜底、选中原文候选、标点顶屏）
//!   与历史候选本身（次数 +1）；
//! - 不记：成员给出的候选、数字透镜产出、重复上屏候选、已有分步上屏段时的上屏；
//! - 门槛见 [`worth_remembering`]。

use crate::coordinator::{Coordinator, State};
use crate::handle_mode::MixLens;
use wind_candidate::{Candidate, CandidateSource};
use wind_store::completion::CompletionKind;
use wind_store::freq::FreqRecord;

/// 历史候选的 `Candidate::id` 标记。上屏出口凭它认出「这是一条历史」。
pub(crate) const QUICK_HISTORY_ID: &str = "quick_input.history";

/// 一次最多给几条历史候选。
///
/// Free 透镜里历史排在原文前面，给多了原文会被挤到第二页；文本 / 数字透镜里它与其它成员
/// 并列，同生僻字成员「最多一页」的理由：别把其余成员整个挤到翻页之外。
pub(crate) const QUICK_HISTORY_QUOTA: usize = 5;

/// 历史候选并进英文段时的起算位次（0 起）。位置提升按「位次 / 2^有效次数」前移，标定同
/// 拼音（10→5→2→1）：用 1 次到第 4 位、3 次到首位。
pub(crate) const HISTORY_BASE_POS: usize = 8;

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
        self.quick_history_candidates_with_recs(state, skip_exact).0
    }

    /// 同 [`Self::quick_history_candidates`]，另返回各候选文本的热度记录（供词频重排）。
    pub(crate) fn quick_history_candidates_with_recs(
        &self,
        state: &State,
        skip_exact: bool,
    ) -> (
        Vec<Candidate>,
        std::collections::HashMap<String, FreqRecord>,
    ) {
        let none = (Vec::new(), std::collections::HashMap::new());
        let buffer = &state.mix_buffer;
        // 已有分步上屏段时不出：记录端同样不记这种状态（见 `learn_mix_literal`），
        // 出了也只会把「中文段 + 历史」拼成一次上屏，分段里的造词就丢了。
        if buffer.is_empty()
            || !state.committed_text.is_empty()
            || !self.mix_has_history(state.mix_id)
        {
            return none;
        }
        let Some(store) = self.store.as_ref() else {
            return none;
        };
        // 全量取出再按小写前缀过滤：store 只提供区分大小写的前缀扫描，而条数有
        // `history_max` 封顶（出厂 1000），逐键一次读事务可以接受。
        let rows = match store.list_completions(CompletionKind::QuickHistory, "", 0, 0) {
            Ok((rows, _)) => rows,
            Err(e) => {
                tracing::debug!("快捷输入历史读取失败: {e}");
                return none;
            }
        };
        let want = buffer.to_lowercase();
        let cand = |text: String| Candidate {
            text,
            id: QUICK_HISTORY_ID.to_string(),
            ..Default::default()
        };
        // 与缓冲逐字相同的那条（用户把一条记过的历史完整打了出来）：`skip_exact` 时不出，
        // 否则排首位且不占配额——它就是原文本身，占了名额其余历史只剩 4 条。
        let exact = !skip_exact && rows.iter().any(|(text, _)| text == buffer);
        let mut out: Vec<Candidate> = Vec::with_capacity(QUICK_HISTORY_QUOTA + 1);
        if exact {
            out.push(cand(buffer.clone()));
        }
        let mut recs = std::collections::HashMap::new();
        for (text, rec) in rows
            .into_iter()
            .filter(|(text, _)| text != buffer)
            .filter(|(text, _)| text.to_lowercase().starts_with(&want))
            .take(QUICK_HISTORY_QUOTA)
        {
            recs.insert(
                text.clone(),
                FreqRecord {
                    count: rec.count,
                    last_used: rec.last_used,
                },
            );
            out.push(cand(text));
        }
        (out, recs)
    }

    /// 文本透镜下并进英文段的历史候选，连同各自的热度记录（次数 / 最近一次）。
    ///
    /// 按成员顺序排在英文之后，`font-size` 这类历史永远落后；整体提前又会压住常打的 `font`。
    /// 故并进英文段、与英文词一起过词频重排，从 [`HISTORY_BASE_POS`] 位起算。与词库同名的不并入。
    pub(crate) fn quick_history_english_pool(&self, state: &State) -> Vec<(Candidate, FreqRecord)> {
        let buffer = &state.mix_buffer;
        if buffer.is_empty()
            || !state.committed_text.is_empty()
            || !self.mix_has_history(state.mix_id)
        {
            return Vec::new();
        }
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        let Ok((rows, _)) = store.list_completions(CompletionKind::QuickHistory, "", 0, 0) else {
            return Vec::new();
        };
        let want = buffer.to_lowercase();
        rows.into_iter()
            .filter(|(text, _)| text != buffer && text.to_lowercase().starts_with(&want))
            .take(QUICK_HISTORY_QUOTA)
            .map(|(text, rec)| {
                let cand = Candidate {
                    text,
                    id: QUICK_HISTORY_ID.to_string(),
                    source: CandidateSource::English,
                    ..Default::default()
                };
                let rec = FreqRecord {
                    count: rec.count,
                    last_used: rec.last_used,
                };
                (cand, rec)
            })
            .collect()
    }

    /// Free / 大写英文透镜的候选（`seg` = 原文 + 英文段）并上历史。没有历史命中时原样返回。
    ///
    /// 有命中时：缓冲是英文词（纯字母带大写，`RESET`）则原文跟随临英「原文候选」开关
    /// （`input.temp_english.raw_candidate`），要原文时它排首位、历史在后；其余（带符号）去掉
    /// 原文、历史置首（原文在组合区看得见，回车恒上屏它）。
    ///
    /// `raw_pinned` = `seg` 首格是按开关钉上去的原文，由 `mix_free_english_segment` 判过，这里
    /// 不再重判（两处各判会在大小写上分叉）。
    pub(crate) fn mix_free_with_history(
        &self,
        state: &State,
        seg: Vec<Candidate>,
        raw_pinned: bool,
    ) -> Vec<Candidate> {
        use wind_candidate::CandidateSource;
        let (mut hist, recs) = self.quick_history_candidates_with_recs(state, false);
        if hist.is_empty() {
            return seg;
        }
        let raw = state.mix_buffer.as_str();
        // 历史段与文本透镜同一把尺：按英文调频（一次到顶 / 位置提升…）重排。
        // 逐字相同的那条是原文本身，恒首位不参与。
        let skip = usize::from(hist.first().is_some_and(|h| h.text == raw));
        self.apply_freq_rerank_with_extra(Some("english"), &mut hist[skip..], raw, recs);
        // 原文格按位置认：钉了原文时 `seg` 首格就是它——`mix_free_english_segment` 先放
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
                // 不单列原文时只去掉纯原文（来源为空）；占据头部格的同名词库词照常出。
                // 钉了原文时按位置认（档位可能已把它改写成 `HEL`）；没钉时首格可能是大小写变形，
                // 而补位原文文本恒等于缓冲，故按文本认。
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
        self.quick_history_of(state, idx)
    }

    /// 同 [`Self::quick_history_at`]，但按全量绝对下标判（移动端候选长按菜单）。
    pub(crate) fn quick_history_of(&self, state: &State, idx: usize) -> Option<String> {
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
        if !self.mix_has_history(state.mix_id)
            || !worth_remembering(text)
            || self.learning_suspended()
        {
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

    /// 原文上屏（回车、空格兜底、选中原文候选）时记历史。只记整段都是字面输入的那次：
    /// 已有分步上屏段时不记（剩余码不是用户想留的串）；数字透镜不记；文本透镜（纯小写）也记，
    /// 词库词由 `mix_cand_is_literal` 挡掉，是否压住拼音由成员顺序决定。
    ///
    /// 必须在 `exit_mix_mode` 清缓冲之前调用。
    pub(crate) fn learn_mix_literal(&self, state: &State) {
        if !state.committed_text.is_empty() || self.mix_lens(state) == MixLens::Numeric {
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
