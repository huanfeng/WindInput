//! 组内权重重排：把同码候选的次序换回极点上游序，**不改变该码在权重轴上的高度**。
//!
//! ## 为什么不是「上游优先级分带」
//!
//! 词频承担着两件被混在一起的事：**这组候选在全库中有多重要**（跨码，五笔按前缀匹配，
//! 打 `tg` 会召出 `tgyn`/`tgab`… 全部后代，它们必须在同一根轴上比）与**这组内部谁排第一**
//! （同码）。上游的 `weight` 列只答得了后者——它是码内局部序号（值域 5~890、86% 是 10），
//! `tgyn` 的 20 与 `tgab` 的 20 之间没有任何可比性。
//!
//! 所以本模块不碰权重的**数值**，只改它们在组内的**分配**：
//!
//! ```text
//! tgyn 的词频权重集合 = {1497, 1479}
//! 上游序             = [重启, 生词]
//! 重新分配           → 重启=1497, 生词=1479
//! ```
//!
//! 每个码占据的权重区间一字不变 ⇒ 跨码可比性完全不受影响；词频退回它真正擅长的两件事：
//! 决定该码整体的高度、以及并列时的裁决。
//!
//! ## 为什么需要护栏
//!
//! 上游的排序里混着大量作者当年随手排上去的低频词。纯回归会把它们顶回首选——
//! 干跑实测 202 条是 unigram 未命中的（`川崎` / `SD卡` / `滨州学院` / `不信谣，不传谣`），
//! 另有一批词频低一两个数量级的。**「词频把上游序压掉」这件事同时干了两件事**：破坏了
//! 一批上游的合理安排，也拦住了一批上游的不合理安排。只看被破坏的那一半就会过度修正。

use crate::config::Config;
use crate::entry::Entry;
use crate::order_report::Snapshot;
use crate::weight::Unigram;
use std::collections::BTreeMap;

/// 重排结果计数。
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Stats {
    /// 上游首选与当前首选不同、进入护栏判定的码
    pub examined: usize,
    /// 实际回归上游序的码
    pub reordered: usize,
    /// 被「上游首选 unigram 未命中」拦下
    pub held_unseen: usize,
    /// 被倍数护栏拦下
    pub held_ratio: usize,
}

/// 把同码条目的权重按上游序重新分配。
///
/// 只重排**上游也有的条目**：`custom_words` 注入的新词不在上游序里，其权重原样保留，
/// 最终排序时自然落位——否则新词会被挤进一个由上游序决定的位置，而上游对它没有意见。
///
/// 简码带（`shortcode_level > 0`）与受保护码整组跳过：它们的权重是**档位**不是词频，
/// 参与重排等于把档位打散。
pub fn reapply_upstream_order(
    entries: &mut [Entry],
    snapshot: &Snapshot,
    unigram: &Unigram,
    cfg: &Config,
) -> Stats {
    let mut stats = Stats::default();
    if !cfg.upstream_order.enabled {
        return stats;
    }

    // code → 该码下参与重排的条目下标
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        if e.shortcode_level > 0 || cfg.is_protected_code(&e.code) {
            continue;
        }
        groups.entry(&e.code).or_default().push(i);
    }
    // 借用检查：分组阶段借了 entries，改权重前先断开
    let groups: Vec<(String, Vec<usize>)> = groups
        .into_iter()
        .map(|(c, v)| (c.to_string(), v))
        .collect();

    for (code, idxs) in groups {
        if idxs.len() < 2 {
            continue;
        }
        let Some(up) = snapshot.group(&code) else {
            continue;
        };

        // 当前序：权重降序。并列时保持 orig_pos 升序，与写出阶段的稳定排序同口径。
        let mut cur = idxs.clone();
        cur.sort_by(|&a, &b| {
            entries[b]
                .weight
                .cmp(&entries[a].weight)
                .then_with(|| entries[a].orig_pos.cmp(&entries[b].orig_pos))
        });

        // 上游序里仍存活于本组的条目（被过滤的、简码带的都已不在 idxs 中）
        let present: BTreeMap<&str, usize> =
            cur.iter().map(|&i| (entries[i].text.as_str(), i)).collect();
        let up_alive: Vec<usize> = up
            .iter()
            .filter_map(|(t, _)| present.get(t.as_str()).copied())
            .collect();
        if up_alive.len() < 2 {
            continue;
        }

        let up_top = &entries[up_alive[0]].text;
        let cur_top = &entries[cur[0]].text;
        if up_top == cur_top {
            continue; // 首选本就一致，组内次序不值得为它翻动
        }
        stats.examined += 1;

        // ── 护栏 ────────────────────────────────────────
        // ① 上游首选不在 unigram 里 ⇒ 它是生僻词，上游把它排第一多半是随手排的
        let f_up = unigram.get(up_top).copied().unwrap_or(0);
        if f_up == 0 {
            stats.held_unseen += 1;
            continue;
        }
        // ② 当前首选的词频高出太多 ⇒ 词频的意见更可信，不回归
        let f_cur = unigram.get(cur_top).copied().unwrap_or(0);
        if f_cur > f_up.saturating_mul(cfg.upstream_order.max_freq_ratio) {
            stats.held_ratio += 1;
            continue;
        }

        // ── 重排：把这些条目**当前持有的权重值**按上游序重新发一遍 ──
        // 取值集合而非重新计算，是「不改变该码在权重轴上的高度」的实现方式。
        let mut pool: Vec<i64> = up_alive.iter().map(|&i| entries[i].weight).collect();
        pool.sort_unstable_by(|a, b| b.cmp(a));
        for (rank, &i) in up_alive.iter().enumerate() {
            entries[i].weight = pool[rank];
        }
        stats.reordered += 1;
    }
    stats
}

#[cfg(test)]
#[path = "upstream_order_tests.rs"]
mod tests;
