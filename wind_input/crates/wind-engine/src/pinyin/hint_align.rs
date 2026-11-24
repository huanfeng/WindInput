//! 正确拼音提示与纠错判定：把用户敲的拼音串与候选词库读音逐音节对齐，找出「读错的音节」
//! 与「经换位纠错的音节」。
//!
//! 对齐用 DP 把 `typed` 切成与词库音节同样多的段，每段按 Equal / Fuzzy / Corrected /
//! Prefix / FuzzyPrefix 判类（先命中先得：同一段既能按模糊又能按纠错解释时归模糊），
//! 取总代价最小的切法。`Fuzzy` 段产出模糊来源的提示，`Corrected` 段产出纠错来源的提示并使
//! 整条记为经纠错。

use super::fuzzy::{FuzzyConfig, FuzzyMatcher};
use wind_candidate::{HintKind, PinyinHint};

/// 最长音节的字节数（`zhuang`、`chuang`、`shuang`）。
const MAX_SYLLABLE_LEN: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Equal,
    Fuzzy,
    Corrected,
    Prefix,
    FuzzyPrefix,
}

impl Kind {
    fn cost(self) -> u32 {
        match self {
            Kind::Equal | Kind::Prefix => 0,
            Kind::Fuzzy | Kind::FuzzyPrefix => 1,
            // 纠错比模糊重：两种切法都对得上时取模糊那种。
            Kind::Corrected => 2,
        }
    }
}

/// 判一段用户输入 `u` 相对词库音节 `c` 的类别；`variants` 是 `c` 的拼写级模糊变体。
fn classify(
    u: &str,
    c: &str,
    variants: &[String],
    cfg: &FuzzyConfig,
    last_open: bool,
) -> Option<Kind> {
    if u == c {
        return Some(Kind::Equal);
    }
    let is_prefix = u.len() < c.len() && c.starts_with(u);
    // D10：输入末尾的半截音节（`pin` 对 `ping`、`zhon` 对 `zhong`）可能只是还没敲完，
    // 不当作读错，也不当作纠错。
    let typing = last_open && is_prefix;
    if variants.iter().any(|v| v == u) {
        return Some(if typing { Kind::Prefix } else { Kind::Fuzzy });
    }
    if !typing && cfg.correction.corrections_of(u).iter().any(|s| s == c) {
        return Some(Kind::Corrected);
    }
    if is_prefix {
        return Some(Kind::Prefix);
    }
    if variants
        .iter()
        .any(|v| u.len() < v.len() && v.starts_with(u))
    {
        return Some(Kind::FuzzyPrefix);
    }
    None
}

/// 对齐结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Alignment {
    /// 带提示的音节（`char_index` 升序，模糊与纠错两种来源）；只在 `cfg.hint` 开时填。
    pub hints: Vec<PinyinHint>,
    /// 有音节只能按换位纠错对上；`cfg.correction` 非空时恒算。
    pub corrected: bool,
}

/// 对齐 `typed` 与 `dict_syllables`。
///
/// `typed` 须是规范化全拼：已做 ü 归一化（`lue` → `lve`，见 [`super::spelling`]）、不含 `'`
/// 分隔符，否则整条对不上、静默漏标。`cfg` 原样传入即可：拼写变体只看用户模糊组、不读
/// 双拼同键音节（见 [`FuzzyMatcher::fuzzy_spelling_variants`]）。
///
/// `input_ends_here`：`typed` 的末尾就是用户当前输入的末尾。对齐失败返回空结果。
pub(crate) fn align(
    typed: &str,
    dict_syllables: &[&str],
    cfg: &FuzzyConfig,
    input_ends_here: bool,
) -> Alignment {
    let want_hints = cfg.hint.get();
    if (!want_hints && cfg.correction.is_empty())
        || dict_syllables.is_empty()
        || typed.is_empty()
        || !typed.is_ascii()
    {
        return Alignment::default();
    }
    let n = dict_syllables.len();
    let m = typed.len();
    let variants: Vec<Vec<String>> = dict_syllables
        .iter()
        .map(|c| FuzzyMatcher::fuzzy_spelling_variants(c, cfg))
        .collect();

    // best[i][p]：前 i 个音节消费了 typed[..p] 的最优 (代价, Fuzzy 段数)。
    type Score = (u32, u32);
    let mut best: Vec<Vec<Option<Score>>> = vec![vec![None; m + 1]; n + 1];
    // 回溯：(上一位置, 本段类别)
    let mut from: Vec<Vec<Option<(usize, Kind)>>> = vec![vec![None; m + 1]; n + 1];
    best[0][0] = Some((0, 0));
    for i in 0..n {
        for p in 0..m {
            let Some(base) = best[i][p] else { continue };
            // 一段至多一个音节，最长音节 6 字节（`zhuang`）。
            for q in (p + 1)..=(p + MAX_SYLLABLE_LEN).min(m) {
                let last_open = input_ends_here && i + 1 == n && q == m;
                let Some(kind) = classify(
                    &typed[p..q],
                    dict_syllables[i],
                    &variants[i],
                    cfg,
                    last_open,
                ) else {
                    continue;
                };
                let score = (
                    base.0 + kind.cost(),
                    base.1 + u32::from(kind == Kind::Fuzzy),
                );
                if best[i + 1][q].is_none_or(|b| score < b) {
                    best[i + 1][q] = Some(score);
                    from[i + 1][q] = Some((p, kind));
                }
            }
        }
    }
    if best[n][m].is_none() {
        return Alignment::default();
    }
    let mut out = Alignment::default();
    let mut p = m;
    for i in (1..=n).rev() {
        let (prev, kind) = from[i][p].expect("best 非空则回溯链完整");
        out.corrected |= kind == Kind::Corrected;
        let hint_kind = match kind {
            Kind::Fuzzy => Some(HintKind::Fuzzy),
            Kind::Corrected => Some(HintKind::Correction),
            _ => None,
        };
        if let Some(hint_kind) = hint_kind.filter(|_| want_hints) {
            out.hints.push(PinyinHint {
                char_index: (i - 1) as u16,
                correct: dict_syllables[i - 1].to_string(),
                kind: hint_kind,
            });
        }
        p = prev;
    }
    out.hints.reverse();
    out
}

#[cfg(test)]
#[path = "hint_align_tests.rs"]
mod tests;
