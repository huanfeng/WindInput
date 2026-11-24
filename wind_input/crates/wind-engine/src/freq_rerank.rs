//! 词频重排（排序独立维度，frequency.md §3/§4）。
//!
//! **绝不改 weight**：词频只按 redb 记录 `{count, last_used}` 参与排序，不回写
//! `Candidate::weight`，词库数据不被污染。
//!
//! 拼音侧走**位置提升**模型（`docs/design/freq-rerank-model.md`）：候选按**位次**前移
//! （`base_pos / 2^count`），完全不看权重数值。此前三版都试图在权重轴上解决问题，
//! 全部失败于同一点——体感由词库的权重分布决定，而那个分布不可控（`de` 下「的」是第二名
//! 的 486 倍，`siyuan` 下「寺院」只是「思源」的 2 倍）。
//!
//! ⚠ 「不改 weight」≠「不改顺序」。`rerank_codetable_usedfirst` 的**首要键是 `freq_tier`**，
//! 与协调器 `candidate_display_order` 的匹配层级是两个正交维度——只要存在词频记录，档位序
//! 就整体压过前一步的排序结果（稳定排序只保住档内相对序）。`rerank_pinyin_positional` 则显式
//! 复刻了层级（调用 `cmp_match_layers`）故不会跨层提拔。改本模块前先想清楚要改的是哪一种。
//!
//! 两种语义按引擎类型分流，**它们的模型已经不同**：
//! - 码表/混输（§3）：**永久布尔 used-first**——用过的（count>0）档内上浮，不衰减。
//!   码表调频默认关闭且有 `ProtectPolicy` 按码长保护首选，实测无越权问题，故维持原样。
//! - 纯拼音：**位置提升**——`target_pos = base_pos / 2^count`，衰减乘在次数上。取代了原先
//!   的「布尔 used-first + 衰减分 + 阈值褪色」，也取代了中途试过的等效权重方案。
//!
//! 设计归属：frequency.md §5/§7 明确把词频重排放在 engine 排序层（持 store freq 只读访问），
//! 而非 dict 查询层或 coordinator。本模块即该排序层的纯函数实现，由 coordinator 在排序后调用。

use crate::manager::{FreqStrategy, PromotePrefix};
use std::collections::HashMap;
use wind_candidate::Candidate;
use wind_store::freq::{FreqProfile, FreqRecord};

/// 按**输入码长**分级的首选保护策略（见 docs/design/codetable-freq-short-code-protection.md）。
///
/// 保护的是「用户当前所在这个码位的钦定首选」，故按**输入码长**分级而非候选码长——
/// 精确档内两者相等，但前缀补全候选的码更长，用候选码长会把分级判据搅乱。
///
/// 五笔一简 25 个码**每个都是二选一**（`a` → 工 9999 / 戈 9998），词库靠权重表达的钦定
/// 地位在本模块完全失效（比较链不含 weight），故简码位默认保护首选 1 位；全码位默认放开，
/// 那里才是调频该起作用的地方。
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

/// 候选来源档位。**定义已搬到 [`wind_candidate::source_tier`]**（跨来源先后的唯一真相源），
/// 本别名只为保留调用点可读性与本模块的文档锚点。
///
/// ⚠️ 本档位是 `rerank_codetable_usedfirst` 的**首要键**：开启自动调频时会整体压过协调器的
/// 显示序，也因此会掩盖 `is_exact_code` 的效果（验证精确匹配优先时须关闭自动调频）。
///
/// ⚠️ 这里**不必**像协调器那样区分「是否混输」：本函数只服务
/// `rerank_codetable_usedfirst`（码表 / 混输），纯拼音走 `rerank_pinyin_positional` 不经过此处；
/// 而纯码表下没有 `Pinyin` 来源候选，拼音精确档天然是空操作。
fn freq_tier(c: &Candidate, input: &str) -> u8 {
    wind_candidate::source_tier(c, input)
}

/// 码表/混输词频重排（§3）：档位感知的**永久** used-first（五笔优先）。
/// 先按来源档位（**码表精确 = 精确码短语** < 拼音精确 < 码表前缀补全 < 前缀短语 < 拼音其余），
/// 档内再 used-first + 策略排序。前缀短语排在码表前缀补全**之后**＝短语不因 is_phrase 抬到
/// 补全之上。（二者曾同档、由 weight 分先后，但那一步从未真正裁决过——混输给码表前缀补全
/// +500K 而前缀短语拿原始权重，码表恒赢；加成拆除后已就地拆成两档，见 `source_tier`。）
/// 稳定排序保证同档无记录者维持引擎权重序，绝不把拼音浮到五笔精确全码之上。
///
/// ⚠️ 精确码短语与码表精确**同档**（都是 0），故本函数不再让码表精确恒赢短语——它们在档内
/// 按 used-first/权重竞争。这条合并的动因正在本函数：`source_tier` 是这里的**首要键**、
/// 整体压过协调器显示序，短语与码表精确若在此分档，就会出现「开调频码表恒赢、关调频按权重比」
/// 的开关依赖不一致（`PHRASE_WEIGHT_BASE` 删除后暴露）。见 `source_tier` 函数文档。
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
    // 保护位数按输入码长分级——简码位（一简/二简）的钦定首选不该被一次误选永久改写，
    // 而全码位正是调频该起作用的地方。
    //
    // 名额**只在精确档内取**（`is_exact_code`）：钦定首选一定是精确档，把名额匀给前缀补全
    // 没有语义依据——那会把一个碰巧排在前面的补全词钉死，还挡住用户真正常用的那条。
    // 精确候选不足名额数就少保护；该码位没有精确候选（打了词库里没有的码）则不保护。
    let protect_n = protect.resolve(code);
    let protected: Vec<String> = candidates
        .iter()
        .filter(|c| c.is_exact_code)
        .take(protect_n)
        .map(|c| c.text.clone())
        .collect();

    use std::cmp::Ordering;
    if strategy == FreqStrategy::Position {
        // 位次减半：与拼音共用 `rerank_positional`，只把层级判据换成来源档位。
        // 档位仍是硬约束——前缀补全（tier 2）再怎么提升也跨不到精确全码（tier 0）之前，
        // 「五笔优先」不受影响；提升只在**档内**发生，这正是前缀匹配为主的方案要的。
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
/// 取 2（每次减半）是拿实测的微软拼音行为标定的——用户观察「中间档的字 1 次提前一半、
/// 2 次进第 2 位、再 1 次到首位」，位次 10→5→2→1 与减半逐条吻合。
///
/// **这是位次的除数，不是权重的步长**，两者性质完全不同：权重步长的体感取决于词库的
/// 权重分布（`de` 下「的」是第二名的 486 倍、`siyuan` 下「寺院」只是「思源」的 2 倍，
/// 同一个步长在两处天差地别），位次除数则与分布无关——第 2 位就是第 2 位。
pub const POSITION_HALVING_BASE: u32 = 2;

/// 「残码上浮的补全」按**预测距离**折抵有效使用次数：每多预测一个用户没输入的音节
/// （[`Candidate::completion_extra_syllables`]），就多要一次实证来兑现。
///
/// ## 为什么是折扣而不是阈值
///
/// 前一版写作硬阈值「`extra <= 1` 才豁免 `promote_prefix`」，挡住了 `extra=4` 的
/// `bingdongsanchif`，却在 `extra=1` 上翻车：真机报障 `bkdssjiifwyiriv`（双拼，
/// bing dong san chi fei yi ri + 残码 zh，started=8）下 9 音节的
/// 「冰冻三尺非一日之寒」`extra` 恰好是 1，卡在阈值**里侧**拿到豁免，选过**一次**
/// 就靠 `power=1` 把 `base_pos=1` 减半到 0、抢走首位。
///
/// 而同为 `extra=1` 的 `jisuanjik`→「计算机科学」必须能靠 **30 次**词频反超残码整句
/// （`pinyin_sentence_flag` 的既有断言）—— 两个案例的真正差异**不是 `extra`，是次数**。
/// 用一个布尔量去切一个连续量上的差异，必然在阈值两侧同时误伤与漏放；阈值定在 0 会
/// 打断计算机科学，定在 1 会放过冰冻三尺，无解。
///
/// 折扣把两者一次性摆平（`eff` 为衰减后的有效次数）：
///
/// | 场景 | extra | 次数 | `eff - extra` | 结果 |
/// |---|---|---|---|---|
/// | `bkdssjiifwyiriv` 长词 | 1 | 1 | 0 | 不提升 ✅ |
/// | `jisuanjik` 计算机科学 | 1 | 30 | 29 | 仍提升 ✅ |
/// | `bingdongsanchif` 长词 | 4 | 1 | −3 | 不提升 ✅ |
///
/// 体感：`extra=1` 的长词选 2 次到首位，`extra=4` 的要 5 次。既守住「音节数对齐优先」
/// 这个先验，又没堵死「实证推翻先验」——只是让预测得越远的词，兑现门槛越高。这与
/// librime/fcitx5 把 `overLengthCost` 和用户历史放在同一根轴上相加是同一个思路。
///
/// ⚠️ 折扣对**所有**候选生效（非补全候选 `extra` 恒 0，减 0 无影响），不要改成只对
/// `is_promoted_completion` 施加——那会让「未上浮但 extra > 0」的补全反而比上浮的更
/// 容易被词频顶上来，次序颠倒。
fn completion_distance_discount(eff: f64, extra: u8) -> f64 {
    eff - f64::from(extra)
}

/// 衰减殆尽的下限：有效强度低于此值视为「没用过」。
///
/// **必需，不是保险丝**。位次是整数，`base_pos = 1`（第 2 位）时任何 `divisor > 1` 都会让
/// `1 / divisor < 1` 而 floor 到 0——即一年前用过一次、衰减到 0.01 的记录仍能把第 2 位顶上
/// 首位。取 0.5 的语义是「累计有效使用不足半次」。
///
/// 这相当于把旧模型的「阈值褪色」以更合理的形式带回：作用在**归一化的有效次数**上，
/// 而不是在权重分上（后者的阈值随词库分布漂移，正是被推翻的那套）。
pub const MIN_PROMOTION_POWER: f64 = 0.5;

/// 候选的**有效提升强度**：`count` 经半衰期衰减，**保留小数**。
///
/// ⚠️ **不能取整**。曾写作 `(count as f64 * decay) as u32`，而 `as` 是向零截断：
/// `count = 1` 的记录只要过了一瞬间，`decay` 就略小于 1（选完 1 分钟后 ≈ 0.99984），
/// 截断即得 0——**用一次的记录几乎立刻失效**，真机表现为「选了词，再打还是老样子」。
/// 而单测全部用 `now == last_used`（decay 恰为 1.0）喂入，正好落在唯一能通过的点上，
/// 全绿却掩盖了它。
///
/// 保留小数后除数是连续的（`2^0.99984 ≈ 1.9998`），衰减平滑无跳变。
///
/// 上限 64：`2^64` 已远超任何候选列表长度，同时防止 `powf` 溢出成 `inf`。
fn promotion_power(
    c: &Candidate,
    recs: &HashMap<String, FreqRecord>,
    now: i64,
    profile: FreqProfile,
    promote_prefix: PromotePrefix,
) -> f64 {
    // 前缀补全候选按 `promote_prefix` 收窄：短输入下用户给出的信息量很少，把长词组靠词频
    // 顶到高频单字前面与直觉相悖（实测 `d` 下「东西」六次即爬到首位，压过「的」「得」）。
    //
    // 判据用**有效前缀层**，与 `cmp_match_layers` 同口径——被引擎主动提升进完整匹配层的
    // 候选（残码上浮 / 用户长词上浮）是结构决策，不该被本项误伤。
    //
    // `Single` 档按**语义单元数**而非字符数：英文所有候选都是前缀匹配（打 `hel` 出
    // `hello`），按字符数会把它们全挡死。
    //
    // 「预测得多远」这件事**不在本闸门里判**，交给
    // [`completion_distance_discount`] 在有效次数上折抵——它是连续量，用布尔闸门切会在
    // 阈值两侧同时误伤与漏放（那一版的完整事故记录见该函数的文档）。
    if c.is_prefix && !c.is_promoted_completion && !promote_prefix.allows(&c.text) {
        return 0.0;
    }
    recs.get(&c.text).map_or(0.0, |r| {
        let eff = r.count as f64 * profile.decay_factor(r, now);
        // 折扣必须在 `MIN_PROMOTION_POWER` 判定**之前**施加：折后不足半次即视为没用过，
        // 否则「远距离预测选过一次」仍会拿到一个大于 0 的 power，`base_pos` 小的时候
        // 照样能 floor 到 0 抢走首位——那正是本次要修的现象。
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
/// # 为什么是位次而不是权重
///
/// 前三版都试图在**权重轴**上解决问题（布尔闸门 → 等效权重 → 局部插值），全部失败于同一点：
/// **体感由词库的权重分布决定，而那个分布不可控**。`de` 下 rime-ice 把「的」抬到第二名的
/// 486 倍，`siyuan` 下「寺院」只是「思源」的 2 倍——同一套参数在前者慢得离谱、在后者快得
/// 离谱，调参只是在两个坏结果之间挪，换本词库还要重调。
///
/// 位次把体感与分布**彻底解耦**：第 2 位用一次就能到首位，第 40 位要六次，与它们的绝对
/// 权重无关。这也是实测的微软拼音行为，且与搜狗/fcitx5 的框架自洽——它们都是「加权混合」，
/// 而混合的前提是**先归一化**；用位次归一化，加权混合即退化为本模型。
///
/// # 契约：入参必须已按显示序排好
///
/// `base_pos` 取的就是入参下标，所以调用方必须先跑 `candidate_display_order`。协调器
/// `handle_candidate.rs` 的顺序（display_order → filter → **本函数** → shadow）满足这一点，
/// 且本函数是最后一道整体排序，其结果不会被后续按权重重排推翻。
///
/// # 三层的分工
///
/// ① **锚定**（精确码短语）恒占顶部、互相维持原序，**不参与位置提升**。整句**不在此列**
///    ——它的 weight 已与词库同量纲，靠 `candidate_display_order` 挣位置，然后与其余候选
///    一样接受词频挑战。
/// ② **匹配层级** `cmp_match_layers`：词频不得跨层提拔（模糊「是」不能压过精确「四」）。
/// ③ **目标位次**升序；同位次时**被提升者在前**——否则提升到 0 的候选会与原本就在 0 位的
///    并列，再按 `base_pos` 排又回到原位，提升等于没做。
pub fn rerank_pinyin_positional(
    candidates: &mut [Candidate],
    recs: &HashMap<String, FreqRecord>,
    now: i64,
    profile: FreqProfile,
    promote_prefix: PromotePrefix,
    input_len: usize,
) {
    // 零权重读音让位（`Candidate::is_zero_weight_reading`）对**用户选过的字**不再成立：
    // 有了词频就是用户用过这个读音，退回精确层，再按位置提升与同层竞争。
    //
    // 不靠位置提升把它从简拼层里捞回来：它在简拼层的**末尾**（小鹤 `cw` 下 𤭢 在第 140 位），
    // 减半模型要选 8 次才到顶，而用户第一次就是翻十几页找到它的。
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
        // 精确码短语锚定顶部，不参与位置提升。
        //
        // **整句不再锚定**：整句 weight 已与词库同量纲（`pinyin::sentence_weight`），它靠
        // weight 在 `candidate_display_order` 里挣到自己的 base_pos，然后与其余候选一样
        // 参与位置提升竞争。锚定原本是硬闸门（命中即维持原序、衰减分连算都不算），只要
        // 它还在，「整句同量纲」就只在无词频记录时成立。
        //
        // 那些摘锚定布尔（`is_sentence_contested` / `is_sentence_unanchored`）都是为了在
        // 硬闸门上凿洞，已随锚定一并回收。`is_sentence_demoted` 留着 —— 它另有本职：
        // step 6.5/6.5b 靠它记录「已让位」，并真的把 weight 压到 `max-1`。
        //
        // 实测移除的影响面极小 —— 锚定此前已被两侧夹到名存实亡：
        // - 同码同层的竞争者存在 ⇒ step 6.6 已摘掉锚定（该步骤现已随本改动删除）；
        // - 不同层的候选（前缀补全「今天天气」、子短语）⇒ `cmp_match_layers` 在排序键里
        //   位于 target_pos 之前，位置提升推不动它们跨层，锚定与否都一样。
        //
        // 唯一真正变化的是**模糊同码候选**：6.6 的过滤器带 `!o.is_fuzzy`，不把它算作
        // 竞争者，于是整句保持锚定、用户选中再多次也翻不过。守门测试
        // `pinyin_fuzzy_peer_can_overtake_sentence`。
        |c| c.is_phrase && c.is_exact_code,
        // 消费长度 → 匹配层级（简拼 / 前缀补全 / 子短语）。词频不得跨层提拔。
        //
        // ## ⚠️ `cmp_by_consumed` 必须在前，且不能省
        //
        // 本函数是**最后一道整体排序**，会推翻 `candidate_display_order` 的结果；而它只在
        // 本次输入**有词频记录**时才被调用（`recs.is_empty()` 直接 return）。两处排序键不
        // 一致 ⇒ 同一批候选存在两种次序，且切换开关是「这个词有没有被上过屏」——用户看到的
        // 是候选忽隐忽现。实测：「冰冻三尺非一日之寒」打 `bingdongsanchi`（整音节边界，
        // 拿不到残码上浮 ⇒ 停在前缀补全层）首次在第 1 位，上屏一次后掉到第 24 位，
        // 从词频表删除又恢复。判据与完整事故记录见 `wind_candidate::cmp_by_consumed`。
        //
        // 词频**只在同一消费长度组内**起作用，这也正是「上过屏与否，召回体验应当一致」的
        // 形式化表述。
        // ⚠️ **不要在这里加 `cmp_completion_extra`**（音节数档位）。它已经由协调器的
        // `candidate_display_order` 施加过，并经 `base_pos`（入参下标）原样传进本函数 ——
        // 无词频记录的候选 `target_pos == base_pos`，档位序自然保持。
        //
        // 把它写进本比较器等于**把默认序升级成词频也翻不过的硬约束**，代价实测：
        // `jisuanjik` 下选过 30 次的「计算机科学」(5 音节，extra=1) 再也压不过残码整句
        // 「计算机看」(4 音节，extra=0)，`pinyin_sentence_flag` 当场抓到。
        //
        // ★ 判据：音节数对齐是**先验**（用户还没告诉我们他要哪个时的合理猜测），
        // 而词频是**实证**（他已经选过 30 次了）。实证该能推翻先验 —— 这也是
        // librime/fcitx5 把 `overLengthCost` 与用户历史放在同一根轴上相加的道理。
        // 真正不容词频跨越的是 `cmp_match_layers` 那种结构性质量差异（模糊 vs 精确）。
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
/// 抽出共用而非各写一份，是因为「位次减半 + 强度 tie-break + 衰减」这套逻辑踩过的坑
/// （`as u32` 截断、`base_pos=0` 恒判未提升、整数位次下的 `MIN_PROMOTION_POWER`）
/// 每一处都够隐蔽，落成两份必然漂移。
///
/// # ⚠️ 契约：入参必须已按显示序排好
///
/// `base_pos` 取的就是入参下标。协调器的顺序（`candidate_display_order` → `apply_filter`
/// → **本函数** → `apply_shadow`）满足这一点，且本函数是最后一道整体排序，其结果不会被
/// 后续按权重重排推翻。
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
    // (锚定, 目标位次, 有效强度)
    //
    // ⚠️ 第三项曾是 `target < pos`（「是否真的前移」），那是个 bug：`base_pos = 0` 的候选
    // 无处可移、`target` 也是 0，于是恒判为「未提升」，被所有提升者挤到后面——真机上表现为
    // **本来排第一、用户也常用的词反而掉位**（实测 `siyuan` 下「寺院」p=8.0 却被挤到第 3）。
    // 判据应该是「有没有词频」，不是「有没有移动」。
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
            // 同位次时按**有效强度**降序：多个候选都提升到同一位次（尤其是都到 0）时，
            // 用得多的在前。无词频者 power = 0，自然落在有词频者之后。
            .then_with(|| {
                meta[b]
                    .2
                    .partial_cmp(&meta[a].2)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then(a.cmp(&b)) // 其余维持原序（稳定）
    });

    // 诊断：本模块此前**零日志**——真机上「选了词、再打还是老样子」时无从判断是没记录、
    // 没调用、还是提升被算成 0（那次的根因是 `as u32` 截断）。这里补一条。
    //
    // 用 `trace!` 而非更高级别：候选文本属用户输入内容，INFO 及以上不得记录。
    // 先判 `enabled!` 再拼串，避免热路径上白白格式化。
    if tracing::enabled!(tracing::Level::TRACE) {
        // 只记**有词频**的候选：其余候选的位移全是被前者插队挤出来的连锁顺延，
        // 记下来会让单行日志涨到数千字符而毫无信息量（实测一次输入刷出整页候选）。
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
