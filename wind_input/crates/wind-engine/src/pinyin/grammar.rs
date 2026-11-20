//! 上下文语言模型（n-gram）接口。
//!
//! 形态对齐 librime 的 `Grammar`（`ref/weasel/librime/src/rime/gear/grammar.h`）：
//! 整句路径分 = **各节点自身的对数权重之和 + 各转移的上下文对数分之和**，纯加法，
//! 因为两者同在对数域。节点权重由 [`super::lattice::score_node`] 算出，
//! 上下文分由本 trait 提供。
//!
//! 详见 `docs/design/language-model-integration.md`。本模块是该文档 §7 的 **P1**：
//! 只搭骨架、不接真模型，验收判据是「`pinyin_eval` 指标与接入前逐位相同」。

/// 上下文打分器。
///
/// 实现须是 `Send + Sync`：[`super::viterbi::ViterbiDecoder`] 被 `PinyinEngine` 持有，
/// 而引擎会跨线程使用。
pub trait Grammar: Send + Sync {
    /// 给「在 `context` 之后接上 `word`」这一步转移打分，返回**对数域**的加分。
    ///
    /// - `context`：已经拼出的前文。对齐 librime `Line::context()`（`poet.cc:52-58`）
    ///   的语义——**只回看两个词**，再由具体实现按需截取（octagram 取尾部 ≤3 个字）。
    ///   句首为空串。
    /// - `word`：本次转移要接上的词。
    /// - `is_rear`：本次转移是否落在整句末尾。octagram 会据此额外查一次
    ///   `word + "$"` 的句末搭配（`octagram.cc:149-158`）。
    ///
    /// 返回值直接加进路径分，**正值是奖励、负值是惩罚**。
    fn query(&self, context: &str, word: &str, is_rear: bool) -> f64;

    /// 打分的尺度系数（octagram 的 `weight`：转移分 = weight × (…)）。连续辅助码的闸门裕度
    /// 随它缩放；`None` = 实现不按这个系数打分（按 0.5 处理）。
    fn weight(&self) -> Option<f64> {
        None
    }
}

/// 恒返回 `0.0` 的空实现。
///
/// ## ★ 为什么是 0.0 而不是 librime 的 `kPenalty`（`log(1e-8) = -18.42`）
///
/// librime 的 `Grammar::Evaluate` 在无模型时返回那个常数，是因为**它那一侧没有别的
/// 「每词固定罚」**——`kPenalty` 就是词数惩罚本身。我们不同：
/// [`super::lattice::WORD_PENALTY`]（= 3.0）早就在扮演这个角色，并且它的取值是连同
/// `DICT_TOTAL` 一起被 `pinyin_eval` 标定出来的（见 `lattice.rs` 里那两处长注释）。
///
/// 于是**任何非零常数都等价于偷偷改了 `WORD_PENALTY`**：整句分里会多出 `n × C`
/// 这一项（`n` = 词数），直接改变「词数不同的两条路径」之间的比较，指标必然漂移。
/// P1 的验收判据是「逐位不变」，所以这里只能是 0.0。
///
/// ## 它存在的意义
///
/// [`super::viterbi::ViterbiDecoder`] 默认是 `None`（连 context 串都不构造，零开销）。
/// `NullGrammar` 用来在测试里**把 query 这条路径真正走一遍**——否则 P1 只证明了
/// 「没接模型时没坏」，没证明「接上模型的通路是通的」。两件事都要验。
pub struct NullGrammar;

impl Grammar for NullGrammar {
    fn query(&self, _context: &str, _word: &str, _is_rear: bool) -> f64 {
        0.0
    }
}
