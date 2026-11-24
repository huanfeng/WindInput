//! 简码分层、冲突分析与降权。
//!
//! 简码（单字 + 码长 ≤ 3）拿固定高权重，占据权重轴顶部，普通词条被 `regular_weight_max`
//! 压在下方——这样词频排序再怎么变都动不了简码的首选地位。
//!
//! 降权解决的是反向问题：一个字既能用简码打出、又占着同前缀 4 码的首选，那个 4 码位就
//! 被浪费了（用户已经有更短的打法）。此时把它让给第二候选，但仅在第二候选够强、且两者
//! 差距不悬殊时才让——否则会把高频字挤到低频词后面。
//!
//! 所有中间结构用 `BTreeMap`：Go 版遍历 `map` 的顺序不确定，虽然当前逻辑对顺序不敏感
//! （每个分组只写自己的条目），但产物是发行词库，不值得把确定性寄托在"恰好不敏感"上。

use crate::config::Config;
use crate::entry::Entry;
use std::collections::{BTreeMap, BTreeSet};

/// 识别简码词条并赋予分层权重。
///
/// 必须在 unigram 赋权**之前**调用：赋权阶段靠 `shortcode_level > 0` 跳过这些条目，
/// 顺序颠倒会让词频覆盖掉简码权重。
pub fn assign_shortcode_weights(entries: &mut [Entry], cfg: &Config) {
    if !cfg.shortcodes.enabled {
        return;
    }
    for e in entries.iter_mut() {
        let code_len = e.code.chars().count();
        if e.is_single_char() && (1..=3).contains(&code_len) {
            e.shortcode_level = code_len;
        }
    }

    // (level, code) → 条目下标
    let mut groups: BTreeMap<(usize, String), Vec<usize>> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        if e.shortcode_level == 0 {
            continue;
        }
        groups
            .entry((e.shortcode_level, e.code.clone()))
            .or_default()
            .push(i);
    }

    for ((level, _code), mut idxs) in groups {
        // 组内按 jidian 原始行序递减赋权，保留原词库的候选排列
        idxs.sort_by_key(|&i| entries[i].orig_pos);
        let base = match level {
            1 => cfg.shortcodes.level1_weight,
            2 => cfg.shortcodes.level2_base_weight,
            3 => cfg.shortcodes.level3_base_weight,
            _ => continue,
        };
        for (rank, idx) in idxs.into_iter().enumerate() {
            entries[idx].weight = base - rank as i64;
        }
    }
}

pub fn count_level(entries: &[Entry], level: usize) -> usize {
    entries
        .iter()
        .filter(|e| e.shortcode_level == level)
        .count()
}

// ── 冲突分析 ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RankedCandidate {
    pub text: String,
    pub weight: i64,
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub kind: String,
    pub char_text: String,
    pub short_code: String,
    pub long_code: String,
    pub candidates_count: usize,
    /// 4 码下按权重排序的候选，最多前 10 条
    pub top_candidates: Vec<RankedCandidate>,
}

/// 简码首选表：code → (text, weight)。同权重时保留先出现的（与 Go 的严格 `>` 一致）。
fn short_top_table(entries: &[Entry]) -> BTreeMap<String, (String, i64)> {
    let mut top: BTreeMap<String, (String, i64)> = BTreeMap::new();
    for e in entries {
        if e.shortcode_level == 0 {
            continue;
        }
        match top.get(&e.code) {
            Some((_, w)) if *w >= e.weight => {}
            _ => {
                top.insert(e.code.clone(), (e.text.clone(), e.weight));
            }
        }
    }
    top
}

/// 4 码候选表，组内按 (权重降序, 文本升序) 排列。
fn full4_table(entries: &[Entry]) -> BTreeMap<String, Vec<(usize, RankedCandidate)>> {
    let mut m: BTreeMap<String, Vec<(usize, RankedCandidate)>> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        if e.code.chars().count() != 4 {
            continue;
        }
        m.entry(e.code.clone()).or_default().push((
            i,
            RankedCandidate {
                text: e.text.clone(),
                weight: e.weight,
            },
        ));
    }
    for list in m.values_mut() {
        list.sort_by(|a, b| {
            b.1.weight
                .cmp(&a.1.weight)
                .then_with(|| a.1.text.cmp(&b.1.text))
        });
    }
    m
}

/// 找出同一字在有前缀关系的编码中都占首选的情况。
///
/// 两类：简码层级之间（1↔2、2↔3、1↔3），以及 2/3 简码与同前缀 4 码之间。
pub fn analyze_conflicts(entries: &[Entry]) -> Vec<Conflict> {
    let top_by_code = short_top_table(entries);
    let full4 = full4_table(entries);
    let mut conflicts = Vec::new();

    // 简码层级间：短码与长码的首选是同一个字
    for (code, (text, _)) in &top_by_code {
        let clen = code.chars().count();
        if clen < 2 {
            continue;
        }
        for l in 1..clen {
            let prefix: String = code.chars().take(l).collect();
            if let Some((shorter_text, _)) = top_by_code.get(&prefix)
                && shorter_text == text
            {
                conflicts.push(Conflict {
                    kind: format!("level{l}_level{clen}"),
                    char_text: text.clone(),
                    short_code: prefix,
                    long_code: code.clone(),
                    candidates_count: 0,
                    top_candidates: Vec::new(),
                });
            }
        }
    }

    // 2/3 简码 vs 同前缀 4 码首选
    for (code4, cands) in &full4 {
        let Some((_, top)) = cands.first() else {
            continue;
        };

        let prefix2: String = code4.chars().take(2).collect();
        if let Some((t, _)) = top_by_code.get(&prefix2) {
            // code4 自身也是简码时不算冲突（它就是那条简码）
            if *t == top.text && !top_by_code.contains_key(code4) {
                conflicts.push(build_full4_conflict(
                    "level2_full4",
                    &top.text,
                    &prefix2,
                    code4,
                    cands,
                ));
            }
        }

        let prefix3: String = code4.chars().take(3).collect();
        if let Some((t, _)) = top_by_code.get(&prefix3)
            && *t == top.text
        {
            conflicts.push(build_full4_conflict(
                "level3_full4",
                &top.text,
                &prefix3,
                code4,
                cands,
            ));
        }
    }

    conflicts.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.long_code.cmp(&b.long_code))
    });
    conflicts
}

fn build_full4_conflict(
    kind: &str,
    char_text: &str,
    short_code: &str,
    long_code: &str,
    cands: &[(usize, RankedCandidate)],
) -> Conflict {
    Conflict {
        kind: kind.to_string(),
        char_text: char_text.to_string(),
        short_code: short_code.to_string(),
        long_code: long_code.to_string(),
        candidates_count: cands.len(),
        top_candidates: cands.iter().take(10).map(|(_, c)| c.clone()).collect(),
    }
}

// ── 降权 ──────────────────────────────────────────────

/// 对同时占据简码和 4 码首选的字降权，让位给第二候选。
///
/// 返回**实际被降权**的 `(code, text)`——顺序变化报告靠它认定成因。降权后的权重是
/// 「第二候选 -1」，但词频补权也能凑出同样的差值，从权重形状反推会把两者混为一谈。
///
/// 触发条件（两个都要满足，否则保留原样）：
/// - 第二候选权重 ≥ promote 阈值（单字/词组各一档）——太弱的候选不值得让位
/// - gap 比例 ≤ max_gap_ratio——首字优势太大时让位会明显劣化体验
pub fn apply_demotion(entries: &mut [Entry], cfg: &Config) -> BTreeSet<(String, String)> {
    let mut demoted = BTreeSet::new();
    if !cfg.demotion.enabled {
        return demoted;
    }
    let dc = &cfg.demotion;
    // 基于降权前的状态计算，循环中不再更新（与 Go 一致）
    let short_top = short_top_table(entries);
    let full4 = full4_table(entries);

    for (code4, cands) in &full4 {
        if cands.len() < 2 {
            continue;
        }
        // 受保护码不降权：本函数的规则是「有简码能打出的字，让出 4 码首选给词组」，
        // 对普通编码成立，但四叠码这类**键位约定**的首选是上游钦定的，让位就是改掉约定。
        // 这是 `cccc` 首选从「又」变成「双双」的直接原因（又 3010 → 双双 1319 - 1 = 1318）。
        if cfg.is_protected_code(code4) {
            continue;
        }
        let (top_idx, top) = &cands[0];

        // 首字是否已能用任一前缀简码打出
        let has_short = (1..=3).filter(|l| *l < code4.chars().count()).any(|l| {
            let prefix: String = code4.chars().take(l).collect();
            short_top.get(&prefix).is_some_and(|(t, _)| *t == top.text)
        });
        if !has_short {
            continue;
        }

        // 第一个越过过滤阈值的候选才算"竞争者"
        let Some((_, second)) = cands[1..]
            .iter()
            .find(|(_, c)| c.weight >= dc.filter_threshold)
        else {
            continue;
        };

        let is_char = second.text.chars().take(2).count() == 1;
        let gap_ratio = (top.weight - second.weight) as f64 / top.weight as f64;
        let (promote_wt, max_gap) = if is_char {
            (dc.single_char_promote_wt, dc.max_gap_ratio_single)
        } else {
            (dc.word_promote_wt, dc.max_gap_ratio_word)
        };
        if second.weight < promote_wt || gap_ratio > max_gap {
            continue;
        }

        // 只动这一条 4 码条目；简码条目本身不变
        entries[*top_idx].weight = second.weight - 1;
        demoted.insert((code4.clone(), top.text.clone()));
    }
    demoted
}

#[cfg(test)]
#[path = "shortcode_tests.rs"]
mod tests;
