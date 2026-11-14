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
    /// 之前：补充候选沉底是硬约束。这个标志只由过滤类步骤（`apply_filter`、出简让全、直接
    /// 辅助码）置上；不接 `Filter` 的 profile 上切片恒为整表。
    FreqRerank,
    /// 用户的删除 / 置顶 / 移动（`apply_shadow_in`），返回值汇入 [`PassOutcome::user_pinned`]。
    Shadow,
}

/// 一次执行的上下文：码域与用户数据归属。各路径口径不同，必须显式给，不回落到活跃方案。
///
/// 两个码是 `Option`：`None` = 本 profile 不含要用它的那一步。profile 含 `FreqRerank` /
/// `Shadow` 却没给对应的码是编程错误（开发构建断言、发布构建记错误日志并跳过该步）——
/// 不用空串占位，因为空串是特殊模式浏览态的**合法** shadow 码（`Some("")`）。
pub(crate) struct PassCtx<'a> {
    /// 用户数据归属方案；`None` = 活跃方案（即 `apply_*_in` 的 `schema_override`）。
    pub(crate) owner: Option<&'a str>,
    /// 调频取频用的码。
    pub(crate) freq_code: Option<&'a str>,
    /// shadow 规则取码（见 `Coordinator::shadow_code_of`；与 `freq_code` 刻意不同域）。
    pub(crate) shadow_code: Option<&'a str>,
}

/// 执行结果。
#[derive(Default)]
pub(crate) struct PassOutcome {
    /// 用户显式置顶 / 移动过（shadow 让路判据，见 `build_candidates` 的出简让全）。
    pub(crate) user_pinned: bool,
}

/// 各路径接哪几步。新增一条候选池时在这里加常量，并登记进测试模块的 `ALL`
/// （漏登记 = 不受顺序单测保护）。
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

    /// 临时拼音：调频之前。⚠️ **现状不接单字**（`apply_single_char` 从未接到临拼上，是否该接
    /// 未定，见 feature-placement-guide.md §4.2）。shadow 要等纠错护顶之后，单列。
    pub(crate) const TEMP_PINYIN_PRE: &[Pass] = &[Filter, FreqRerank];
    /// 临时拼音收尾的 shadow。
    pub(crate) const TEMP_PINYIN_SHADOW: &[Pass] = &[Shadow];
    /// 临时英文的词库候选段：英文不做检索范围与单字（对英文无作用）。
    pub(crate) const TEMP_ENGLISH: &[Pass] = &[FreqRerank, Shadow];
    /// mix 自由英文段：同临时英文。
    pub(crate) const MIX_ENGLISH_SEGMENT: &[Pass] = &[FreqRerank, Shadow];
    /// 特殊模式：调频之前（不接过滤：特殊模式自带码表，候选即用户要的；生僻字准入另有一步）。
    pub(crate) const SPECIAL_PRE: &[Pass] = &[FreqRerank];
    /// 生僻字模式**完全不参与词频**（用户拍板）：读写两端一起跳。
    pub(crate) const RARE_CHAR_PRE: &[Pass] = &[];
    /// 特殊模式的 shadow（主列表、补全候选、浏览态三处共用）。
    pub(crate) const SPECIAL_SHADOW: &[Pass] = &[Shadow];
    /// 候选反查（通配查字）：只过检索范围。
    pub(crate) const REVERSE: &[Pass] = &[Filter];
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
                    let Some(freq_code) = ctx.freq_code else {
                        missing_code(*pass);
                        continue;
                    };
                    // 用 take_while 而非 partition_point：不预设列表整体有序，只截到首个补充候选。
                    let n = list.iter().take_while(|c| !c.is_scope_filtered).count();
                    self.apply_freq_rerank_in(ctx.owner, &mut list[..n], freq_code);
                }
                Pass::Shadow => {
                    let Some(shadow_code) = ctx.shadow_code else {
                        missing_code(*pass);
                        continue;
                    };
                    out.user_pinned |= self.apply_shadow_in(ctx.owner, list, shadow_code);
                }
            }
        }
        out
    }
}

/// profile 含某步却没给它要的码：编程错误。开发构建当场断言；发布构建不让输入法崩，记错误后跳过。
fn missing_code(pass: Pass) {
    debug_assert!(false, "PassCtx 缺 {pass:?} 要用的码");
    tracing::error!("候选后处理：PassCtx 缺 {pass:?} 要用的码，跳过这一步");
}

#[cfg(test)]
mod tests {
    use super::Pass::*;
    use super::profile;

    /// 全部 profile（供顺序单测遍历）。
    const ALL: &[(&str, &[super::Pass])] = &[
        ("MAIN_PRE", profile::MAIN_PRE),
        ("MAIN_SHADOW", profile::MAIN_SHADOW),
        ("SENTENCE_POOL", profile::SENTENCE_POOL),
        ("COMPLETION_POOL", profile::COMPLETION_POOL),
        ("DIRECT_AUX_PREFIX", profile::DIRECT_AUX_PREFIX),
        ("DIRECT_AUX_MERGE", profile::DIRECT_AUX_MERGE),
        ("TEMP_PINYIN_PRE", profile::TEMP_PINYIN_PRE),
        ("TEMP_PINYIN_SHADOW", profile::TEMP_PINYIN_SHADOW),
        ("TEMP_ENGLISH", profile::TEMP_ENGLISH),
        ("MIX_ENGLISH_SEGMENT", profile::MIX_ENGLISH_SEGMENT),
        ("SPECIAL_PRE", profile::SPECIAL_PRE),
        ("RARE_CHAR_PRE", profile::RARE_CHAR_PRE),
        ("SPECIAL_SHADOW", profile::SPECIAL_SHADOW),
        ("REVERSE", profile::REVERSE),
    ];

    /// 每个 profile 都必须是规范顺序（`Pass` 声明序）的严格递增子序列：既不乱序，也不重复。
    #[test]
    fn every_profile_follows_canonical_order() {
        for (name, p) in ALL {
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
        // 锚点带尾逗号：只有 `run_passes` 的实参位置才有，注释里提到 profile 名不会被误认。
        let pre = body
            .find("crate::postprocess::profile::MAIN_PRE,")
            .expect("主链应经 MAIN_PRE");
        let shadow = body
            .find("crate::postprocess::profile::MAIN_SHADOW,")
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

    /// 调用点确实用的是对应 profile：只锁常量内容的话，调用点误传成别的 profile 照样绿。
    #[test]
    fn call_sites_use_their_profile() {
        let prod = crate::source_scan::production_code(include_str!("handle_candidate.rs"));
        for (pool, want) in [
            ("&mut completion_pool,", "COMPLETION_POOL"),
            ("&mut pool,", "SENTENCE_POOL"),
        ] {
            let at = prod
                .find(pool)
                .unwrap_or_else(|| panic!("找不到 {pool} 的 run_passes"));
            let tail = &prod[at..];
            let got = tail
                .find("profile::")
                .map(|i| &tail[i + "profile::".len()..])
                .and_then(|t| {
                    t.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .next()
                })
                .unwrap_or("");
            assert_eq!(got, want, "{pool} 那次 run_passes 用的 profile 不对");
        }
    }
}
