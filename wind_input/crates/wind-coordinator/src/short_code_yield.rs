//! 出简让全：有简码的字，在更长的码位上把首选让给词语。
//!
//! 五笔的标准功能——「路」的三简是 `kht`，用户已经能用 3 键打出它，那么 4 码 `khtk` 的
//! 首选位就该给词语（`khtk` 下的「路上」之类），否则等于浪费一个码位。
//!
//! # 为什么在这里做，而不是在词库里
//!
//! 这件事原先由 `gen_dict::apply_demotion` 在**词库生成阶段**做，两个偏差：
//! 判定烘进权重，用户关不掉；且触发条件是「第二候选权重够高且 gap 够小」的有条件降权，
//! 不是标准的出简让全语义。搬到运行时后它成为一个开关，关掉即回到词库原序。
//!
//! # 判据从哪来：记录，而不是查询
//!
//! 「这个字有没有更短的码」看似要 `text → code` 反查，其实不用——五笔简码恒是全码的前缀，
//! 所以问题等价于「输入码的某个前缀上，首选是不是它」。而输入是渐进的：打 `khtk` 必然
//! 逐键经过 `k` → `kh` → `kht`，每一步都跑完整的候选生成。把每一步的首选记下来，到全码
//! 时信息已经齐了，**零查询**。
//!
//! 记的必须是**用户实际看到的**首条，不是词典层的序——后者不含用户调频（`apply_freq_rerank`）、
//! 候选调整（`apply_shadow`）、检索范围过滤（`apply_filter`）与用户词层的效果。用户把某字
//! 调频顶到 `kht` 首位，词典层序毫无变化，而用户确实已经能一键打出它了。
//!
//! # 档位
//!
//! `level` = 参与让位的简码级别上限：0 关闭，2 = 一二级简码置后，3 = 全部简码置后。
//! 判据是「当前码长 > level」而**不是**「当前码长 == 全码长」——后者要知道方案有几码，
//! 换到 5 码方案就错位；前者对任何码长的方案都成立。

use wind_candidate::{Candidate, CandidateSource};

/// 记录表长度：简码最多三级（一简/二简/三简）。
pub(crate) const MAX_LEVEL: usize = 3;

/// 各级简码位的首选：下标 0/1/2 = 码长 1/2/3，值为 `(该级的码, 首选文本)`。
pub(crate) type ShortcodeTops = [Option<(String, String)>; MAX_LEVEL];

/// 淘汰与当前输入码无关的陈旧记录。
///
/// **失效靠拉取而非推送**：`input_buffer.clear()` 在协调器里有十余个散落调用点，逐个接线
/// 迟早漏一处（本仓栽过多次）。改成用时校验前缀关系后，缓冲清空、光标中间编辑、方案切换
/// 全被这一条覆盖——记录的码不再是当前输入的前缀，它就不成立了。
///
/// `kht` 改成 `kxt` 时缓冲长度没变，只有前缀校验抓得住；「退格时按长度截断」抓不住。
pub(crate) fn evict_stale(tops: &mut ShortcodeTops, input: &str) {
    for slot in tops.iter_mut() {
        let stale = slot
            .as_ref()
            .is_none_or(|(code, _)| !input.starts_with(code.as_str()));
        if stale {
            *slot = None;
        }
    }
}

/// 记录本级简码位的首选。码长超出 [`MAX_LEVEL`] 或列表为空时不记。
///
/// 记的是 `candidates[0]`——**不限定来源**。出简让全问的是「用户打这个短码能不能一键得到
/// 这个字」，混输下若首选是拼音候选，那答案就是不能，此时记下拼音那条、后续比对不相等、
/// 于是不让位，正是想要的保守行为。
pub(crate) fn record_top(tops: &mut ShortcodeTops, input: &str, candidates: &[Candidate]) {
    let n = input.chars().count();
    if n == 0 || n > MAX_LEVEL {
        return;
    }
    let Some(first) = candidates.first() else {
        return;
    };
    // 只记单字：让位的对象恒是字，记词没有任何消费者。
    if first.text.chars().count() != 1 {
        tops[n - 1] = None;
        return;
    }
    tops[n - 1] = Some((input.to_string(), first.text.clone()));
}

/// 施加让位。返回是否真的动了顺序。
///
/// 调用位置有硬约束：**必须在 `apply_freq_rerank` 之后**。4 码位的
/// `ProtectPolicy.fallback` 是 0（不保护首选），先让位会被调频原样顶回去。
///
/// 缺记录一律不让位（走过临时英文/临时拼音/特殊模式/URL 模式的输入会缺级）——宁可少让，
/// 不可让错。
///
/// `user_pinned` = 本码有置顶规则就位（`apply_shadow` 的返回值）。**候选调整优先于出简
/// 让全**：用户在这个码上手动排过序，自动让位就不再插手，见下方 `user_pinned` 分支。
pub(crate) fn apply(
    candidates: &mut [Candidate],
    input: &str,
    tops: &ShortcodeTops,
    level: usize,
    user_pinned: bool,
) -> bool {
    let level = level.min(MAX_LEVEL);
    if level == 0 {
        return false;
    }
    // ── 候选调整优先 ──────────────────────────────────────────────────────────
    //
    // 用户右键调过这个码的顺序，让位就整码停手。理由不是「尊重用户」这种软话，而是
    // **`ShadowPin.position` 是绝对下标**：它记的是用户右键当时**所见列表**里的位次，
    // 而用户所见正是让位之后的列表。若让位继续在 shadow 之后动手，用户按下标 N 存进去、
    // 回放时被挪到别处，那个下标就再也表达不了任何东西——position 的语义先垮了。
    //
    // 停的是**整码**而不只是被沉底的那个字：让位的两步 rotate 会让接位词之后的候选各前移
    // 一位，「调到第 4 位」照样会变成第 3 位。只赦免首条治不了这一半。
    //
    // 代价（明确的取舍，不是遗漏）：用户只是把某个词往后挪一挪，也会连带让首选从词变回
    // 字。可预测性优先——「这个码我排过序，就按我排的来」比「有些调整生效有些不生效」
    // 好解释得多。不接受的用户把那条调整「恢复默认」即可。
    //
    // 只看置顶不看删除：删除说的是「这条别出现」，与「谁排第一」不是同一维度。判据由
    // `apply_shadow` 一处交出（见其文档），让位侧不得自己再查规则。
    if user_pinned {
        return false;
    }
    let n = input.chars().count();
    // 简码位自己不让位：只有比参与档位更长的码才谈得上「出简让全」。
    if n <= level {
        return false;
    }
    let Some(first) = candidates.first() else {
        return false;
    };
    // 首选须是码表来源的单字。让拼音/短语候选降位是跨来源排序（`source_tier`）的地盘，
    // 本功能不碰——否则混输下会与那套规则在同一个位置上打架。
    if first.source != CandidateSource::CodeTable
        || first.is_command
        || first.text.chars().count() != 1
    {
        return false;
    }
    // 扫**全部**参与级，不能只看最近一级。「路」若在二简 `kh` 就是首选，`kht` 会因此让位，
    // 于是 `kht` 的首选不再是「路」——只看最近一级的话，`khtk` 就会判成「无简码」而不让位，
    // 判据把自己擦掉了。症状是「有的字让了有的没让」，从现象极难倒推。
    let has_shortcode = tops.iter().take(level).any(|slot| {
        slot.as_ref()
            .is_some_and(|(code, top)| input.starts_with(code.as_str()) && top == &first.text)
    });
    if !has_shortcode {
        return false;
    }
    // 接位者：第一个码表来源的多字词。
    //
    // `is_scope_filtered` 必须排除——那是检索范围临时放宽才放进来的候选，「追加在末尾、
    // 原有顺序纹丝不动」是它的硬约束，提到首位会直接违背。
    let Some(pos) = candidates.iter().position(|c| {
        c.source == CandidateSource::CodeTable
            && !c.is_command
            && !c.is_scope_filtered
            && c.text.chars().count() > 1
    }) else {
        return false;
    };
    // 让位的字**沉到本码所有候选之后**，不是降一位——用户已经能用简码一键打出它，
    // 它在这个码位上就该彻底靠后，而不是继续占着第二位。同码的其它全码字（`踟` 这类
    // 没有简码的）都排在它前面：那些字**只能**在这个码位打出来，它不能挡着。
    //
    //   路 | 昤 | 路上 | 路口  →  路上 | 昤 | 路口 | 路
    //
    // 沉底的边界是**正常候选段**而非整个列表：`is_scope_filtered` 是检索范围临时放宽才
    // 补进来的，「追加在末尾、原有顺序纹丝不动」是它的硬约束，让位的字不能沉到它们后面。
    let normal_len = candidates
        .iter()
        .take_while(|c| !c.is_scope_filtered)
        .count();
    // 两步：接位者转到首位（字随之落到下标 1），再把字从下标 1 沉到正常段末尾。
    //
    // ⚠️ 不能只做沉底那一步——`路 | 昤 | 路上` 直接沉底会得到 `昤 | 路上 | 路`，
    // 首选成了生僻字。「让给词语」和「沉到最后」是两件事，必须都做。
    candidates[..=pos].rotate_right(1);
    candidates[1..normal_len].rotate_left(1);
    true
}

#[cfg(test)]
#[path = "short_code_yield_tests.rs"]
mod tests;
