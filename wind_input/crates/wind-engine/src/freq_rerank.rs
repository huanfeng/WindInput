//! 词频重排（排序独立维度，frequency.md §3/§4）。
//!
//! **绝不改 weight**：词频只按 redb 记录 `{count, last_used}` 参与排序，不回写
//! `Candidate::weight`，词库数据不被污染。
//!
//! 拼音侧走位置提升模型：候选按位次前移（`base_pos / 2^count`），不看权重数值，
//! 设计与否决过的权重方案见 `docs/design/freq-rerank-model.md`。
//!
//! 「不改 weight」≠「不改顺序」：`rerank_codetable_usedfirst` 的首要键是 `freq_tier`，
//! 有词频记录时整体压过协调器 `candidate_display_order` 的显示序；`rerank_pinyin_positional`
//! 则复刻了匹配层级，不跨层提拔。
//!
//! 两种语义按引擎类型分流：
//! - 码表/混输（§3）：永久布尔 used-first，用过的档内上浮、不衰减；首选靠 `ProtectPolicy` 保护。
//! - 纯拼音：位置提升，`target_pos = base_pos / 2^count`，衰减乘在次数上。
//!
//! 本模块是 engine 排序层的纯函数（frequency.md §5/§7），由 coordinator 在排序后调用。

use crate::manager::{FreqStrategy, PromotePrefix};
use std::collections::HashMap;
use wind_candidate::Candidate;
use wind_store::freq::{FreqProfile, FreqRecord};

/// 按**输入码长**分级的首选保护策略（见 docs/design/codetable-freq-short-code-protection.md）。
///
/// 按输入码长而非候选码长分级：前缀补全候选的码更长，用候选码长会搅乱分级。
/// 本模块比较链不含 weight，词库靠权重表达的简码钦定地位在此失效，故简码位默认保护首选
/// 1 位；全码位放开，那里才是调频该起作用的地方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtectPolicy {
    /// 索引 0/1/2 = 码长 1/2/3（一简/二简/三简位）。
    pub by_len: [usize; 3],
    /// 码长 ≥ 4（未单列的深码位）。
    pub fallback: usize,
}

impl Default for ProtectPolicy {
    fn default() -> Self {
        Self {
            by_len: [1, 1, 0],
            fallback: 0,
        }
    }
}

impl ProtectPolicy {
    /// 空策略（全不保护）：拼音路径与「关闭保护」用。
    pub const NONE: Self = Self {
        by_len: [0; 3],
        fallback: 0,
    };

    /// 本次输入码长对应的保护位数。空码不保护；码长 ≥ 4 落兜底档。
    pub fn resolve(&self, code: &str) -> usize {
        match code.chars().count() {
            0 => 0,
            n if n <= self.by_len.len() => self.by_len[n - 1],
            _ => self.fallback,
        }
    }
}

/// 候选来源档位，定义在 [`wind_candidate::source_tier`]（跨来源先后的唯一真相源）。
///
/// 它是 `rerank_codetable_usedfirst` 的首要键，开自动调频时整体压过协调器显示序，也会掩盖
/// `is_exact_code` 的效果（验证精确优先须先关自动调频）。不必区分混输：纯拼音不走这里，
/// 纯码表下拼音精确档天然为空。
fn freq_tier(c: &Candidate, input: &str) -> u8 {
    wind_candidate::source_tier(c, input)
}

/// 码表/混输词频重排（§3）：档位感知的永久 used-first（五笔优先）。
/// 先按来源档位（码表精确 = 精确码短语 < 拼音精确 < 码表前缀补全 < 前缀短语 < 拼音其余），
/// 档内再 used-first + 策略排序；稳定排序保证同档无记录者维持传入的显示序。
///
/// 精确码短语与码表精确同档：若在此分档，会出现「开调频码表恒赢、关调频按权重比」的
/// 开关依赖不一致。见 candidate-sorting-rules.md §7.3「tier 0 为什么把短语并进来」。
///
/// 策略：`Step`（默认/逐次提升）count 降序、last_used 降序 tiebreak（抗误选）；
/// `Top`（一次到顶/MRU）last_used 降序、count 降序 tiebreak（最近选的置该档之首）。
// 八个参数都是彼此正交的重排旋钮（策略/保护/时钟/档位/前缀提升），打包成 struct 只是把
// 同样多的字段换个地方列，调用点反而看不出自己传了什么。
#[allow(clippy::too_many_arguments)]
pub fn rerank_codetable_usedfirst(
    candidates: &mut [Candidate],
    recs: &HashMap<String, FreqRecord>,
    code: &str,
    strategy: FreqStrategy,
    protect: ProtectPolicy,
    now: i64,
    profile: FreqProfile,
    promote_prefix: PromotePrefix,
) {
    // 呈现层保护：记录基础序前 N 位，重排后原序回填（不动 weight，见 frequency.md §8）。
    // 名额只在精确档内取：钦定首选必在精确档，匀给前缀补全会把碰巧排前的补全词钉死。
    // 精确候选不足则少保护，没有精确候选则不保护。
    let protect_n = protect.resolve(code);
    let protected: Vec<String> = candidates
        .iter()
        .filter(|c| c.is_exact_code)
        .take(protect_n)
        .map(|c| c.text.clone())
        .collect();

    use std::cmp::Ordering;
    if strategy == FreqStrategy::Position {
        // 与拼音共用 `rerank_positional`，层级判据换成来源档位：提升只在档内发生，
        // 前缀补全跨不到精确全码之前。
        rerank_positional(
            candidates,
            recs,
            now,
            profile,
            promote_prefix,
            |_| false, // 码表无锚定层：来源档位已承担该职责
            |a, b| freq_tier(a, code).cmp(&freq_tier(b, code)),
        );
    } else {
        candidates.sort_by(|a, b| {
            let ta = freq_tier(a, code);
            let tb = freq_tier(b, code);
            if ta != tb {
                return ta.cmp(&tb);
            }
            match (recs.get(&a.text), recs.get(&b.text)) {
                // 布尔 used-first：用过一次即整体跳到档内最前。`Position` 没有这道台阶。
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal, // 同档均无记录 → 维持引擎权重序（稳定排序）
                (Some(ra), Some(rb)) => match strategy {
                    FreqStrategy::Top => rb
                        .last_used
                        .cmp(&ra.last_used)
                        .then(rb.count.cmp(&ra.count)),
                    FreqStrategy::Step => rb
                        .count
                        .cmp(&ra.count)
                        .then(rb.last_used.cmp(&ra.last_used)),
                    // 上面已分派，此处不可达
                    FreqStrategy::Position => Ordering::Equal,
                },
            }
        });
    }

    for (i, text) in protected.iter().enumerate() {
        if let Some(pos) = candidates.iter().position(|c| &c.text == text)
            && pos > i
        {
            candidates[i..=pos].rotate_right(1);
        }
    }
}

/// 位置提升的**步长底数**：每积累一次有效使用，候选的目标位次除以它。
///
/// 取 2 对齐微软拼音的体感（位次 10→5→2→1）。这是位次的除数而非权重步长，与词库权重
/// 分布无关，见 `docs/design/freq-rerank-model.md` §6。
pub const POSITION_HALVING_BASE: u32 = 2;

/// 「残码上浮的补全」按**预测距离**折抵有效使用次数：每多预测一个用户没输入的音节
/// （[`Candidate::completion_extra_syllables`]），就多要一次实证来兑现。
///
/// 用折扣而非 `extra` 阈值：区分「选过一次的远距离预测」与「选过 30 次的」靠的是次数，
/// 布尔阈值切不开。`extra=1` 的长词选 2 次到首位，`extra=4` 的要 5 次。依据见
/// `docs/design/freq-rerank-model.md` §6.5。
///
/// 折扣对所有候选生效（非补全 `extra` 恒 0）；别改成只对 `is_promoted_completion` 施加，
/// 否则未上浮的补全反而比上浮的更容易被词频顶上来。
fn completion_distance_discount(eff: f64, extra: u8) -> f64 {
    eff - f64::from(extra)
}

/// 衰减殆尽的下限：有效强度低于此值视为「没用过」。
///
/// 必需：位次是整数，`base_pos = 1` 时任何 `divisor > 1` 都会 floor 到 0，没有下限的话
/// 衰减到 0.01 的旧记录也能把第 2 位顶上首位。0.5 即「累计有效使用不足半次」。
pub const MIN_PROMOTION_POWER: f64 = 0.5;

/// 候选的**有效提升强度**：`count` 经半衰期衰减，**保留小数**。
///
/// 不能取整：`decay` 一过瞬间就略小于 1，截断会让 `count = 1` 的记录立刻失效
/// （见 freq-rerank-model.md §6.3）。
///
/// 上限 64：`2^64` 已远超任何候选列表长度，同时防止 `powf` 溢出成 `inf`。
fn promotion_power(
    c: &Candidate,
    recs: &HashMap<String, FreqRecord>,
    now: i64,
    profile: FreqProfile,
    promote_prefix: PromotePrefix,
) -> f64 {
    // 前缀补全按 `promote_prefix` 收窄：短输入下把长词靠词频顶到高频单字前与直觉相悖。
    // 判据用有效前缀层（与 `cmp_match_layers` 同口径），引擎主动上浮的候选不受影响。
    // 预测距离不在这里判，交给 `completion_distance_discount` 连续折抵。
    if c.is_prefix && !c.is_promoted_completion && !promote_prefix.allows(&c.text) {
        return 0.0;
    }
    recs.get(&c.text).map_or(0.0, |r| {
        let eff = r.count as f64 * profile.decay_factor(r, now);
        // 折扣须在 `MIN_PROMOTION_POWER` 判定之前施加，否则远距离预测选过一次仍拿到
        // 正 power，`base_pos` 小时照样 floor 到 0 抢走首位。
        let eff = completion_distance_discount(eff, c.completion_extra_syllables);
        if eff < MIN_PROMOTION_POWER {
            return 0.0;
        }
        eff.min(64.0)
    })
}

/// 按有效强度算目标位次：`base_pos / BASE^power`。
fn target_position(base_pos: usize, power: f64) -> usize {
    if power <= 0.0 {
        return base_pos;
    }
    let divisor = (POSITION_HALVING_BASE as f64).powf(power);
    (base_pos as f64 / divisor).floor() as usize
}

/// 拼音词频重排：**位置提升**模型（`docs/design/freq-rerank-model.md`）。
///
/// ```text
/// target_pos = base_pos / (HALVING_BASE ^ effective_count)
/// ```
///
/// 排序键：`(锚定, 匹配层级, target_pos, 是否被提升, base_pos)`。
///
/// 用位次而非权重，是为了让体感与词库权重分布解耦，见 `docs/design/freq-rerank-model.md`。
///
/// 契约：入参必须已按 `candidate_display_order` 排好（`base_pos` 取入参下标）。
///
/// 三层分工：① 锚定（精确码短语）恒占顶部、不参与提升；整句不锚定，靠同量纲 weight 挣
/// 位置后同样接受词频挑战。② `cmp_match_layers`：词频不得跨层提拔。③ 目标位次升序，
/// 同位次按有效强度降序。
pub fn rerank_pinyin_positional(
    candidates: &mut [Candidate],
    recs: &HashMap<String, FreqRecord>,
    now: i64,
    profile: FreqProfile,
    promote_prefix: PromotePrefix,
    input_len: usize,
) {
    // 零权重读音让位（`Candidate::is_zero_weight_reading`）对用户选过的字不成立：直接退回
    // 精确层再竞争。不靠位置提升捞，因为它在简拼层末尾，减半模型要选很多次才到顶。
    for c in candidates.iter_mut() {
        if c.is_zero_weight_reading && promotion_power(c, recs, now, profile, promote_prefix) > 0.0
        {
            c.is_zero_weight_reading = false;
        }
    }
    rerank_positional(
        candidates,
        recs,
        now,
        profile,
        promote_prefix,
        // 只锚定精确码短语。整句不锚定：其 weight 已与词库同量纲（`pinyin::sentence_weight`），
        // 锚定会让模糊同码候选选再多次也翻不过它，见 `pinyin_fuzzy_peer_can_overtake_sentence`。
        |c| c.is_phrase && c.is_exact_code,
        // 消费长度 → 匹配层级，须与显示序成对：本函数只在有词频记录时跑，两处键不一致会让
        // 候选次序随「上过屏与否」切换。见 `wind_candidate::cmp_by_consumed`。
        //
        // 不要在这里加 `cmp_completion_extra`：显示序已施加并经 `base_pos` 传入，再写进比较器
        // 就成了词频也翻不过的硬约束（见 `pinyin_sentence_flag`）。音节数对齐是先验，词频是
        // 实证，实证应能推翻先验。
        |a, b| {
            wind_candidate::cmp_by_consumed(a, b, input_len)
                .then_with(|| wind_candidate::cmp_match_layers(a, b))
        },
    );
}

/// 位置提升的**通用实现**，拼音与码表共用（`docs/design/freq-rerank-model.md`）。
///
/// ```text
/// target_pos = base_pos / (HALVING_BASE ^ effective_count)
/// ```
///
/// 排序键：`(锚定, 层级, target_pos, 有效强度降序, base_pos)`。
///
/// 两个调用方只在**层级判据**上不同，位置提升逻辑完全一致：
///
/// | 调用方 | `anchored` | `layer_cmp` |
/// |---|---|---|
/// | 拼音 | 精确码短语 | [`wind_candidate::cmp_match_layers`] |
/// | 码表 / 混输 / 英文 | 无（恒 `false`） | `freq_tier` 来源档位 |
///
/// 契约：入参必须已按显示序排好（`base_pos` 取入参下标）。协调器的顺序
/// （`candidate_display_order` → `apply_filter` → 本函数 → `apply_shadow`）满足这一点。
pub fn rerank_positional<A, L>(
    candidates: &mut [Candidate],
    recs: &HashMap<String, FreqRecord>,
    now: i64,
    profile: FreqProfile,
    promote_prefix: PromotePrefix,
    anchored: A,
    layer_cmp: L,
) where
    A: Fn(&Candidate) -> bool,
    L: Fn(&Candidate, &Candidate) -> std::cmp::Ordering,
{
    let n = candidates.len();
    if n < 2 {
        return;
    }
    // (锚定, 目标位次, 有效强度)。第三项必须是强度而非「是否前移」：`base_pos = 0` 的候选
    // 无处可移，按是否前移会被所有提升者挤到后面（freq-rerank-model.md §6.2）。
    let meta: Vec<(bool, usize, f64)> = candidates
        .iter()
        .enumerate()
        .map(|(pos, c)| {
            let power = promotion_power(c, recs, now, profile, promote_prefix);
            (anchored(c), target_position(pos, power), power)
        })
        .collect();

    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        let (aa, ab) = (meta[a].0, meta[b].0);
        if aa != ab {
            return ab.cmp(&aa); // 锚定者在前
        }
        if aa {
            return a.cmp(&b); // 均锚定 → 维持原序，不参与提升
        }
        layer_cmp(&candidates[a], &candidates[b])
            .then_with(|| meta[a].1.cmp(&meta[b].1)) // 目标位次升序
            // 同位次按有效强度降序：用得多的在前，无词频者 power = 0 自然靠后。
            .then_with(|| {
                meta[b]
                    .2
                    .partial_cmp(&meta[a].2)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then(a.cmp(&b)) // 其余维持原序（稳定）
    });

    // 诊断日志用 `trace!`：候选文本属用户输入内容，INFO 及以上不得记录。
    // 先判 `enabled!` 再拼串，避免热路径上白白格式化。
    if tracing::enabled!(tracing::Level::TRACE) {
        // 只记有词频的候选：其余位移都是连锁顺延，记下来只会让单行日志膨胀。
        let moved: Vec<String> = idx
            .iter()
            .enumerate()
            .filter(|&(_, &old_pos)| meta[old_pos].2 > 0.0)
            .map(|(new_pos, &old_pos)| {
                format!(
                    "{}:{}->{}(p={:.2})",
                    candidates[old_pos].text, old_pos, new_pos, meta[old_pos].2
                )
            })
            .collect();
        if moved.is_empty() {
            tracing::trace!(records = recs.len(), "词频重排：无候选移位");
        } else {
            tracing::trace!(records = recs.len(), moved = %moved.join(" "), "词频重排");
        }
    }

    // 应用置换：move 而非 clone（候选含 String，上千条时 clone 不可忽略）
    let mut reordered: Vec<Candidate> = Vec::with_capacity(n);
    for &i in &idx {
        reordered.push(std::mem::take(&mut candidates[i]));
    }
    for (slot, c) in candidates.iter_mut().zip(reordered) {
        *slot = c;
    }
}

#[cfg(test)]
#[path = "freq_rerank_tests.rs"]
mod tests;
