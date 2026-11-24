//! 出简让全：有简码的字，在更长的码位上把首选让给词语。
//!
//! 「路」的三简是 `kht`，4 码 `khtk` 的首选位就该给词语（「路上」之类）。在运行时做而非烘进
//! 词库权重，才能做成开关、关掉即回到词库原序。
//!
//! # 判据：记录，而不是查询
//!
//! 五笔简码恒是全码的前缀，而输入是渐进的：打 `khtk` 必然逐键经过 `k` → `kh` → `kht`。把每一步
//! 的首选记下来，到全码时信息已齐，零查询。记的必须是用户实际看到的首条（含调频、候选调整、
//! 检索范围过滤、用户词层），不是词典层的序。
//!
//! # 档位
//!
//! `level` = 参与让位的简码级别上限：0 关闭，2 = 一二级简码置后，3 = 全部简码置后。判据是
//! 「当前码长 > level」而非「== 全码长」，对任何码长的方案都成立。

use wind_candidate::{Candidate, CandidateSource};

/// 记录表长度：简码最多三级（一简/二简/三简）。
pub(crate) const MAX_LEVEL: usize = 3;

/// 各级简码位的首选：下标 0/1/2 = 码长 1/2/3，值为 `(该级的码, 首选文本)`。
pub(crate) type ShortcodeTops = [Option<(String, String)>; MAX_LEVEL];

/// 淘汰与当前输入码无关的陈旧记录。
///
/// 失效靠拉取而非推送：缓冲清空点散落十余处，逐个接线迟早漏；用时校验前缀关系一条覆盖清空、
/// 中间编辑、方案切换。`kht` 改成 `kxt` 时长度没变，只有前缀校验抓得住。
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
/// 记的是 `candidates[0]`——不限定来源。出简让全问的是「用户打这个短码能不能一键得到
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
/// 必须在 `apply_freq_rerank` 之后：4 码位的 `ProtectPolicy.fallback` 是 0，先让位会被调频顶回去。
/// 缺记录一律不让位（走过临时模式的输入会缺级）——宁可少让，不可让错。
///
/// `user_pinned` = 本码有置顶规则就位（`apply_shadow` 的返回值），此时不插手。
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
    // 用户右键调过这个码的顺序，让位就整码停手：`ShadowPin.position` 是用户所见（让位之后）
    // 列表里的绝对下标，让位再在 shadow 之后动手会让下标失义；且让位的 rotate 会让后续候选
    // 各前移一位，只赦免首条不够。代价：只是挪一挪某个词也会让首选从词变回字——可预测性优先，
    // 不接受的用户把那条调整恢复默认即可。
    //
    // 只看置顶不看删除（两者不是同一维度）。判据由 `apply_shadow` 一处交出，让位侧不得自己再查。
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
    // 扫全部参与级，不能只看最近一级。「路」若在二简 `kh` 就是首选，`kht` 会因此让位，
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
    // 让位的字沉到本码所有候选之后，不是降一位——用户已经能用简码一键打出它，
    // 它在这个码位上就该彻底靠后，而不是继续占着第二位。同码的其它全码字（`踟` 这类
    // 没有简码的）都排在它前面：那些字只能在这个码位打出来，它不能挡着。
    //
    //   路 | 昤 | 路上 | 路口  →  路上 | 昤 | 路口 | 路
    //
    // 沉底的边界是正常候选段而非整个列表：`is_scope_filtered` 是检索范围临时放宽才
    // 补进来的，「追加在末尾、原有顺序纹丝不动」是它的硬约束，让位的字不能沉到它们后面。
    let normal_len = candidates
        .iter()
        .take_while(|c| !c.is_scope_filtered)
        .count();
    // 两步：接位者转到首位（字随之落到下标 1），再把字从下标 1 沉到正常段末尾。
    //
    // 不能只做沉底那一步——`路 | 昤 | 路上` 直接沉底会得到 `昤 | 路上 | 路`，
    // 首选成了生僻字。「让给词语」和「沉到最后」是两件事，必须都做。
    candidates[..=pos].rotate_right(1);
    candidates[1..normal_len].rotate_left(1);
    true
}

#[cfg(test)]
#[path = "short_code_yield_tests.rs"]
mod tests;
