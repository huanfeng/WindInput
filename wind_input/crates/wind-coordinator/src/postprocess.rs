//! 候选后处理步骤表（`docs/architecture/feature-placement-guide.md` §5.1）。
//!
//! 「过滤 → 单字 → 调频 → shadow」这几道加工被主链与若干候选池各接一份。此前每条路径手写
//! 自己那几行 `apply_*`，先后顺序与「接了哪几步」只记在注释里。这里把它们收成数据：
//!
//! - [`Pass`] 的声明顺序就是**规范顺序**；
//! - 每条路径用 [`profile`] 里的一个常量声明自己接哪几步，不接的理由写在常量旁；
//! - [`Coordinator::run_passes`] 按 profile 依次执行。
//!
//! 单测断言每个 profile 都是规范顺序的子序列：顺序约束从注释变成会红的测试。
//!
//! 主链独有、只有一个消费者的步骤（英文定位、整句块、纠错护顶、直接辅助码、出简让全）不进表，
//! 仍在 `build_candidates` 里原地调用。

use wind_candidate::Candidate;

use crate::coordinator::{Coordinator, State};

/// 一道后处理。**声明顺序 = 规范顺序**（派生的 `Ord` 即依据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Pass {
    /// 检索范围过滤（`apply_filter`）。依赖 `is_common` 已填好（`mark_common`，不在表内）。
    Filter,
    /// 单字输入（`apply_single_char`），与 `Filter` 正交。
    SingleChar,
    /// 用户词频重排（`apply_freq_rerank_in`）。只排到首个自动补充候选（`is_scope_filtered`）
    /// 之前：补充候选沉底是硬约束。
    FreqRerank,
    /// 用户的删除 / 置顶 / 移动（`apply_shadow_in`），返回值汇入 [`PassOutcome::user_pinned`]。
    Shadow,
}

/// 一次执行的上下文：码域与用户数据归属。各路径口径不同，必须显式给，不回落到活跃方案。
pub(crate) struct PassCtx<'a> {
    /// 用户数据归属方案；`None` = 活跃方案（即 `apply_*_in` 的 `schema_override`）。
    pub(crate) owner: Option<&'a str>,
    /// 调频取频用的码。
    pub(crate) freq_code: &'a str,
    /// shadow 规则取码（见 `Coordinator::shadow_code_of`；与 `freq_code` 刻意不同域）。
    pub(crate) shadow_code: &'a str,
}

/// 执行结果。
#[derive(Default)]
pub(crate) struct PassOutcome {
    /// 用户显式置顶 / 移动过（shadow 让路判据，见 `build_candidates` 的出简让全）。
    pub(crate) user_pinned: bool,
}

/// 各路径接哪几步。新增一条候选池时在这里加常量，并在 `ALL` 里登记。
pub(crate) mod profile {
    use super::Pass::{self, *};

    /// 主链调频之前的三步。shadow 要等英文定位、整句块、纠错护顶、直接辅助码之后（见
    /// `build_candidates`），故单列 [`MAIN_SHADOW`]。
    pub(crate) const MAIN_PRE: &[Pass] = &[Filter, SingleChar, FreqRerank];
    /// 主链收尾的 shadow。
    pub(crate) const MAIN_SHADOW: &[Pass] = &[Shadow];
    /// 整句池：直接拷自引擎结果，不过滤的话切换键能把用户隐藏的、被检索范围滤掉的整句换回来。
    /// 不调频：池子的顺序就是整句名次。
    pub(crate) const SENTENCE_POOL: &[Pass] = &[Filter, SingleChar, Shadow];
    /// 空码补全池：补进来的候选同样显示在当前码下，用户隐藏的往往正是它。只取一条，不调频。
    pub(crate) const COMPLETION_POOL: &[Pass] = &[Filter, SingleChar, Shadow];
    /// 直接辅助码对前缀单独解码的候选池（`decode_direct_prefix`），口径同主链调频之前。
    pub(crate) const DIRECT_AUX_PREFIX: &[Pass] = &[Filter, SingleChar, FreqRerank];
    /// 直接辅助码合并命中项前对前缀池做的 shadow（取前缀的 shadow 码）。
    pub(crate) const DIRECT_AUX_MERGE: &[Pass] = &[Shadow];

    /// 全部 profile（供顺序单测遍历）。
    #[cfg(test)]
    pub(crate) const ALL: &[(&str, &[Pass])] = &[
        ("MAIN_PRE", MAIN_PRE),
        ("MAIN_SHADOW", MAIN_SHADOW),
        ("SENTENCE_POOL", SENTENCE_POOL),
        ("COMPLETION_POOL", COMPLETION_POOL),
        ("DIRECT_AUX_PREFIX", DIRECT_AUX_PREFIX),
        ("DIRECT_AUX_MERGE", DIRECT_AUX_MERGE),
    ];
}

impl Coordinator {
    /// 按 `profile` 依次对 `list` 执行后处理。
    pub(crate) fn run_passes(
        &self,
        state: &State,
        list: &mut Vec<Candidate>,
        profile: &[Pass],
        ctx: &PassCtx,
    ) -> PassOutcome {
        let mut out = PassOutcome::default();
        for pass in profile {
            match pass {
                Pass::Filter => self.apply_filter(state, list),
                Pass::SingleChar => self.apply_single_char(state, list),
                Pass::FreqRerank => {
                    // 用 take_while 而非 partition_point：不预设列表整体有序，只截到首个补充候选。
                    let n = list.iter().take_while(|c| !c.is_scope_filtered).count();
                    self.apply_freq_rerank_in(ctx.owner, &mut list[..n], ctx.freq_code);
                }
                Pass::Shadow => {
                    out.user_pinned |= self.apply_shadow_in(ctx.owner, list, ctx.shadow_code);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Pass::*;
    use super::profile;

    /// 每个 profile 都必须是规范顺序（`Pass` 声明序）的严格递增子序列：既不乱序，也不重复。
    #[test]
    fn every_profile_follows_canonical_order() {
        for (name, p) in profile::ALL {
            assert!(
                p.windows(2).all(|w| w[0] < w[1]),
                "{name} 违反规范顺序或重复：{p:?}"
            );
        }
    }

    /// ★ 单字过滤必须排在 shadow **之前**：`ShadowPin.position` 是绝对下标，记的是用户右键当时
    /// 所见的列表，滤在 shadow 之后位次就对不上（出简让全踩过同一个坑）。端到端造 shadow 规则
    /// 要真实 store，行为断言写不出来，故钉结构：主链的单字在 `MAIN_PRE` 里，且 `MAIN_PRE`
    /// 在 `build_candidates` 里先于 `MAIN_SHADOW` 执行。
    ///
    /// ⚠️ 后半句钉的是**文本位置**：两处锚点都在 `build_candidates` 同一个函数体内，文本序 =
    /// 执行序；把调用挪进别的辅助函数时请重新核对。
    #[test]
    fn main_chain_single_char_runs_before_shadow() {
        assert!(profile::MAIN_PRE.contains(&SingleChar));
        assert!(!profile::MAIN_PRE.contains(&Shadow));
        const SRC: &str = include_str!("handle_candidate.rs");
        let body = &SRC[SRC
            .find("fn build_candidates(")
            .expect("找不到 build_candidates")..];
        let pre = body.find("profile::MAIN_PRE").expect("主链应经 MAIN_PRE");
        let shadow = body
            .find("profile::MAIN_SHADOW")
            .expect("主链应经 MAIN_SHADOW");
        assert!(pre < shadow, "MAIN_PRE 必须先于 MAIN_SHADOW");
    }

    /// 补全池与整句池走**同一条**过滤链：主列表被滤空后从补全池取一条，不过单字就会
    /// 「开了单字模式还是冒出一个词」；不过 shadow 就会「隐藏完当场又被补回来」。
    #[test]
    fn side_pools_go_through_filter_single_char_and_shadow() {
        for (name, p) in [
            ("COMPLETION_POOL", profile::COMPLETION_POOL),
            ("SENTENCE_POOL", profile::SENTENCE_POOL),
        ] {
            for pass in [Filter, SingleChar, Shadow] {
                assert!(p.contains(&pass), "{name} 缺 {pass:?}");
            }
        }
    }
}
