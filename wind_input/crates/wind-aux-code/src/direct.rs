//! 直接辅助码（双拼）的纯逻辑：切分、匹配、并入。设计见 `docs/design/aux-code-direct.md`。
//!
//! 不按引导键，每键按奇偶把输入拆成「前缀 + 末 1～2 位」：前缀由调用方（协调器）单独解码，
//! 取完整覆盖前缀的词与单字，按字形筛出命中项，再并入主候选。**无状态**——每键重算，
//! 不改拼音引擎切分。本模块只放不依赖引擎的部分：
//!
//! - [`split_direct`]：按奇偶切出前缀与辅码；
//! - [`is_direct_source`]：前缀候选能否参与（完整覆盖前缀的词 / 单字，非整句、非补全）；
//! - [`direct_matches`]：字形匹配（单字按码开头；词组按 [`DirectPhraseRule`]）；
//! - [`direct_placement`] + [`merge_direct_hits`]：命中项插在主候选的哪里。
//!
//! 「前缀能否完整切成双拼音节」要问引擎（布局相关），不在这里。
//!
//! ## 与引导键模式的分工
//!
//! 引导键模式（`filter.rs`）是**筛**：只留命中者，词组逐字首码。直接辅助码是**提**：命中者
//! 插到前面、其余原样接后（无命中不动候选），词组规则按 [`DirectPhraseRule`]。两者取码走
//! 同一个 [`AuxCodeLookup`]，「码」一律按**开头**匹配。

use wind_candidate::{Candidate, effective_consumed};

use crate::lookup::AuxCodeLookup;

/// 奇数长度下「引擎首选保留第 1 位」的前缀音节数门槛：前缀 ≥ 此值时首选（通常是整句）不让位。
///
/// 奇数长度的末位对整句党来说其实是下一音节的声母；门槛以下当辅码提升两字词，门槛以上
/// 保住长句首选，防首选长度来回跳（魔然默认值，见设计 §3）。
pub const ODD_KEEP_TOP_MIN_SYLLABLES: usize = 3;

/// 偶数长度下保留在命中项之前的「覆盖全部输入的完整候选」个数上限（全音节解析优先）。
pub const EVEN_KEEP_FULL_COVER: usize = 2;

/// 词组的辅码匹配规则。
///
/// 预留 `First`（手心可选）：加分支即可，筛选入口 [`direct_matches`] 不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirectPhraseRule {
    /// 1 位 `a`：任一字某码以 `a` 开头。
    /// 2 位 `ab`：① 任一字某码以 `ab` 开头；或 ② 存在 i < j，第 i 字某码以 `a` 开头、
    /// 第 j 字某码以 `b` 开头。只做串尾时用（自然码做法）。
    #[default]
    Any,
    /// 末字某码以 `aux` 开头：辅码限定紧挨在它前面的那个字。连续辅助码下串尾一步用，
    /// 与词中辅码同一语义（万象 Pro 同）。
    Last,
}

/// 输入按奇偶切出的前缀与辅码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectSplit<'a> {
    /// 前缀（双拼击键），交给引擎单独解码。
    pub prefix: &'a str,
    /// 末 1 位（奇数长度）或末 2 位（偶数长度），当辅码。
    pub aux: &'a str,
}

/// 按奇偶切分：奇数 ≥ 3 → 前 n−1 键 + 末 1 位；偶数 ≥ 4 → 前 n−2 键 + 末 2 位。
///
/// 返回 `None` 即本键不做直接辅助：不足 3 键、含手动分隔符 `'`（用户已显式声明切分）、
/// 或辅码不全是字母（辅码只收字母，与引导键模式一致；前缀里的非字母键——如微软双拼的 `;`
/// ——照常交给引擎判能否成音节）。
pub fn split_direct(input: &str) -> Option<DirectSplit<'_>> {
    if !input.is_ascii() || input.len() < 3 || input.contains('\'') {
        return None;
    }
    let aux_len = if input.len() % 2 == 1 { 1 } else { 2 };
    let (prefix, aux) = input.split_at(input.len() - aux_len);
    if !aux.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some(DirectSplit { prefix, aux })
}

/// 前缀解码结果里的这条候选能否参与直接辅助：**完整覆盖前缀**的词或单字（含用户词）。
///
/// 排除：没吃满前缀的（子短语 / 半截）、预测了没打的音节的（前缀补全）、引擎新合成的
/// 整句（`is_synthesized`——词库里有的词即便同时是整句解，也照常参与）、简拼与全拼降级
/// （都不是双拼的全音节解读）、草稿层猜测词（恒沉底，辅码不该把它顶上来）、短语 / 命令 / 组，
/// 以及检索范围临时放宽补进来的候选（`is_scope_filtered`：自动补充的恒沉底，是硬约束）。
pub fn is_direct_source(c: &Candidate, prefix_len: usize) -> bool {
    !c.text.is_empty()
        && effective_consumed(c, prefix_len) == prefix_len
        && !c.is_partial
        && !c.is_prefix
        && !c.is_synthesized
        && !c.is_abbrev
        && !c.is_fullpinyin_fallback
        && !c.is_draft
        && !c.is_split_composed
        && !c.is_phrase
        && !c.is_command
        && !c.is_group
        && !c.is_scope_filtered
}

/// 把一条前缀候选标成直接辅助命中项：消费整串（上屏连辅码一起吃掉）、带来源标记；
/// `code` 保留前缀的拼音码（调频记在前缀下）。
///
/// 整句相关的标记一并清掉：它们描述的是**前缀**那次解码（「释读」在 `uidu` 下可能正是整句
/// 最优解、`sentence_rank = 1`），搬到整串输入下就是错的——整句切换键会把它当成本次的整句
/// N-best 去滚动。
pub fn mark_direct_hit(c: &mut Candidate, input_len: usize) {
    c.consumed_length = input_len;
    c.is_direct_aux = true;
    c.is_sentence = false;
    c.is_sentence_demoted = false;
    c.sentence_rank = 0;
}

/// 候选文本是否与辅码 `aux`（1～2 位）字形相符。
///
/// 单字：某码以 `aux` 开头。词组：先受 `max_phrase_len`（0 = 不限）约束，再按 `rule`。
/// 与引导键模式的 passthrough 相反，空辅码 / 空表 / 超过 2 位一律**不命中**——这里「命中」
/// 的后果是被提前，防御态应当是什么都不做。
pub fn direct_matches(
    text: &str,
    lookup: &dyn AuxCodeLookup,
    aux: &str,
    rule: DirectPhraseRule,
    max_phrase_len: usize,
) -> bool {
    let mut aux_chars = aux.chars();
    let (Some(a), second) = (aux_chars.next(), aux_chars.next()) else {
        return false;
    };
    if aux_chars.next().is_some() || lookup.is_empty() {
        return false;
    }
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if chars.next().is_none() {
        return lookup.any_code_starts_with(first, aux);
    }
    if max_phrase_len > 0 && text.chars().count() > max_phrase_len {
        return false;
    }
    match rule {
        DirectPhraseRule::Any => match second {
            None => text
                .chars()
                .any(|ch| lookup.any_code_starts_with_char(ch, a)),
            Some(b) => {
                if text.chars().any(|ch| lookup.any_code_starts_with(ch, aux)) {
                    return true;
                }
                // ② 取最靠前的 a 命中位 i，其后任一字 b 命中即成立（i 越靠前，j 的选择越多）。
                let mut it = text.chars();
                it.by_ref()
                    .any(|ch| lookup.any_code_starts_with_char(ch, a))
                    && it.any(|ch| lookup.any_code_starts_with_char(ch, b))
            }
        },
        DirectPhraseRule::Last => text
            .chars()
            .last()
            .is_some_and(|ch| lookup.any_code_starts_with(ch, aux)),
    }
}

/// 命中项插在主候选的什么位置（设计 §6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectPlacement {
    /// 命中项排最前（奇数长度、前缀 < [`ODD_KEEP_TOP_MIN_SYLLABLES`] 音节）。
    HitsFirst,
    /// 主候选首位（通常是整句）保留第 1 位，命中项从第 2 位起（奇数长度、长前缀）。
    AfterTop,
    /// 主候选开头覆盖全部输入的完整候选最多保留这么多个，命中项接其后（偶数长度）。
    AfterFullCover(usize),
}

/// 按辅码位数与前缀音节数定插入方式。
pub fn direct_placement(aux_len: usize, prefix_syllables: usize) -> DirectPlacement {
    if aux_len >= 2 {
        DirectPlacement::AfterFullCover(EVEN_KEEP_FULL_COVER)
    } else if prefix_syllables >= ODD_KEEP_TOP_MIN_SYLLABLES {
        DirectPlacement::AfterTop
    } else {
        DirectPlacement::HitsFirst
    }
}

/// 主候选里这一条是否「覆盖全部输入的完整候选」（词或整句）：吃满整串、不是子短语 / 补全，
/// 也不是简拼 / 全拼降级（那不是双拼的全音节解读）。
fn covers_whole_input(c: &Candidate, input_len: usize) -> bool {
    effective_consumed(c, input_len) == input_len
        && !c.is_partial
        && !c.is_prefix
        && !c.is_abbrev
        && !c.is_fullpinyin_fallback
}

/// 把命中项并入主候选：`head`（按 `placement` 保留在前的那段）+ 命中项 + 其余。
///
/// - 无命中：主候选原样返回（无命中不动候选）。
/// - 按文本去重：保留段里已有的，命中项不再重复；命中项里有的，其余里那条丢掉。
/// - `AfterFullCover` 只取主候选**开头连续**的完整候选，不把后面的完整候选挪上来——
///   主候选自身的相对顺序始终不变，只是中间插进了命中项。
pub fn merge_direct_hits(
    main: Vec<Candidate>,
    hits: Vec<Candidate>,
    input_len: usize,
    placement: DirectPlacement,
) -> Vec<Candidate> {
    if hits.is_empty() {
        return main;
    }
    let keep = match placement {
        DirectPlacement::HitsFirst => 0,
        DirectPlacement::AfterTop => main.len().min(1),
        DirectPlacement::AfterFullCover(n) => main
            .iter()
            .take(n)
            .take_while(|c| covers_whole_input(c, input_len))
            .count(),
    };
    let mut out: Vec<Candidate> = Vec::with_capacity(main.len() + hits.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut main = main.into_iter();
    for c in main.by_ref().take(keep) {
        seen.insert(c.text.clone());
        out.push(c);
    }
    for c in hits {
        if seen.insert(c.text.clone()) {
            out.push(c);
        }
    }
    out.extend(main.filter(|c| !seen.contains(&c.text)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::AuxCodeTable;

    /// 小鹤形码（取自 flypy_full.txt）：释 pl、湿 dy、适 zk、十 al、试 yg、读 yd、度 gy。
    fn table() -> AuxCodeTable {
        AuxCodeTable::from_rows(vec![
            ('释', "pl"),
            ('湿', "dy"),
            ('适', "zk"),
            ('十', "al"),
            ('试', "yg"),
            ('读', "yd"),
            ('度', "gy"),
            ('国', "ky"),
            ('庆', "gd"),
            ('情', "xo"),
        ])
    }

    fn cand(text: &str) -> Candidate {
        Candidate {
            text: text.into(),
            ..Default::default()
        }
    }

    fn texts(v: &[Candidate]) -> Vec<&str> {
        v.iter().map(|c| c.text.as_str()).collect()
    }

    fn m(text: &str, aux: &str) -> bool {
        direct_matches(text, &table(), aux, DirectPhraseRule::Any, 0)
    }

    // ── 切分 ──

    #[test]
    fn split_odd_takes_last_one() {
        assert_eq!(
            split_direct("uiduc"),
            Some(DirectSplit {
                prefix: "uidu",
                aux: "c"
            })
        );
        assert_eq!(
            split_direct("xlr"),
            Some(DirectSplit {
                prefix: "xl",
                aux: "r"
            })
        );
    }

    #[test]
    fn split_even_takes_last_two() {
        assert_eq!(
            split_direct("uidupl"),
            Some(DirectSplit {
                prefix: "uidu",
                aux: "pl"
            })
        );
        assert_eq!(
            split_direct("xlrn"),
            Some(DirectSplit {
                prefix: "xl",
                aux: "rn"
            })
        );
    }

    #[test]
    fn split_needs_three_keys() {
        assert_eq!(split_direct(""), None);
        assert_eq!(split_direct("u"), None);
        assert_eq!(split_direct("ui"), None);
    }

    #[test]
    fn split_skips_manual_separator() {
        assert_eq!(split_direct("ui'dup"), None);
        assert_eq!(split_direct("uidu'p"), None);
    }

    #[test]
    fn split_skips_non_letter_aux() {
        // 微软双拼 `;` = ing：落在辅码位上不是辅码。前缀里的 `;` 照常放行（由引擎判音节）。
        assert_eq!(split_direct("uid;"), None);
        assert_eq!(
            split_direct("u;dup"),
            Some(DirectSplit {
                prefix: "u;du",
                aux: "p"
            })
        );
    }

    // ── 匹配：§5 表逐格 ──

    #[test]
    fn single_char_one_letter() {
        assert!(m("释", "p"));
        assert!(!m("湿", "p"));
    }

    #[test]
    fn single_char_two_letters() {
        assert!(m("释", "pl"));
        assert!(!m("释", "pk"), "按码开头匹配：pl 不以 pk 开头");
        assert!(!m("十", "pl"));
    }

    #[test]
    fn phrase_one_letter_any_char() {
        assert!(m("释读", "p"), "首字 释=pl");
        assert!(m("释读", "y"), "次字 读=yd，任意字规则");
        assert!(!m("湿度", "p"), "湿 dy / 度 gy 都不以 p 开头");
    }

    #[test]
    fn phrase_two_letters_whole_code_of_any_char() {
        // ① 任一字的某码以 ab 开头
        assert!(m("释读", "yd"), "读=yd");
        assert!(m("释读", "pl"), "释=pl");
        assert!(!m("湿度", "pl"));
    }

    #[test]
    fn phrase_two_letters_ordered_pair() {
        // ② i < j：第 i 字 a 开头、第 j 字 b 开头
        assert!(m("释读", "py"), "释 p + 读 y");
        assert!(!m("释读", "yp"), "顺序反了：读在释之后，不成立");
        assert!(m("国庆", "kg"), "国 ky + 庆 gd");
        assert!(!m("国情", "kg"), "情=xo 不以 g 开头");
        assert!(m("湿度", "dg"));
    }

    /// 末字规则：只看最后一个字（单字即其本身），1 / 2 位都按码开头。
    #[test]
    fn last_rule_checks_final_char_only() {
        let last =
            |text: &str, aux: &str| direct_matches(text, &table(), aux, DirectPhraseRule::Last, 0);
        assert!(last("释读", "y"), "读=yd");
        assert!(last("释读", "yd"));
        assert!(!last("释读", "p"), "释是首字");
        assert!(!last("释读", "py"), "不做跨字配对");
        assert!(last("国庆", "gd"));
        assert!(!last("国情", "k"));
        assert!(last("释", "pl"), "单字同 Any");
        assert!(!last("释读", ""));
    }

    #[test]
    fn unknown_char_and_empty_lookup_do_not_match() {
        assert!(!m("李", "m"), "表里无码");
        let empty = AuxCodeTable::new();
        assert!(!direct_matches("释", &empty, "p", DirectPhraseRule::Any, 0));
        assert!(!m("释", ""), "空辅码不命中");
        assert!(!m("释", "plx"), "超过 2 位不命中");
    }

    /// 2 位辅码的命中集 ⊆ 其首字母 1 位的命中集。协调器据此按首字母给引擎下推准入、
    /// 并让奇数键取的前缀池在紧随其后的偶数键复用——这条不成立，复用就会漏命中。
    #[test]
    fn two_letter_hits_are_subset_of_first_letter_hits() {
        let t = table();
        let texts = [
            "释",
            "湿",
            "适",
            "十",
            "试",
            "读",
            "度",
            "国",
            "庆",
            "情",
            "释读",
            "湿度",
            "国庆",
            "国情",
            "读释",
            "释读度",
            "李",
        ];
        let letters = ['p', 'l', 'd', 'y', 'g', 'k', 'x', 'o', 'a', 'z'];
        let mut two_letter_hits = 0;
        for text in texts {
            for a in letters {
                for b in letters {
                    let ab: String = [a, b].iter().collect();
                    for (max, r) in [0, 2].into_iter().flat_map(|m| {
                        [DirectPhraseRule::Any, DirectPhraseRule::Last].map(|r| (m, r))
                    }) {
                        if direct_matches(text, &t, &ab, r, max) {
                            two_letter_hits += 1;
                            assert!(
                                direct_matches(text, &t, &a.to_string(), r, max),
                                "{text} 中 {ab} 却不中 {a}"
                            );
                        }
                    }
                }
            }
        }
        assert!(two_letter_hits > 20, "样本太少：{two_letter_hits}");
    }

    #[test]
    fn max_phrase_len_excludes_long_phrases_only() {
        let t = table();
        let r = DirectPhraseRule::Any;
        assert!(!direct_matches("释读度", &t, "p", r, 2), "3 字词 > 上限 2");
        assert!(direct_matches("释读", &t, "p", r, 2));
        assert!(direct_matches("释", &t, "p", r, 1), "单字恒参与");
        assert!(direct_matches("释读度", &t, "p", r, 0), "0 = 不限");
    }

    // ── 来源资格 ──

    #[test]
    fn source_requires_full_prefix_cover_non_sentence() {
        let full = Candidate {
            consumed_length: 4,
            ..cand("释读")
        };
        assert!(is_direct_source(&full, 4));
        assert!(is_direct_source(&cand("释读"), 4), "0 = 整串");
        let lexicon_sentence = Candidate {
            is_sentence: true,
            ..cand("释读")
        };
        assert!(
            is_direct_source(&lexicon_sentence, 4),
            "词库词同时是整句解：照常参与"
        );
        for (why, c) in [
            (
                "没吃满前缀",
                Candidate {
                    consumed_length: 2,
                    ..cand("释")
                },
            ),
            (
                "子短语",
                Candidate {
                    is_partial: true,
                    ..cand("释")
                },
            ),
            (
                "前缀补全",
                Candidate {
                    is_prefix: true,
                    ..cand("释读者")
                },
            ),
            (
                "引擎合成的整句",
                Candidate {
                    is_synthesized: true,
                    is_sentence: true,
                    ..cand("是读")
                },
            ),
            (
                "简拼",
                Candidate {
                    is_abbrev: true,
                    ..cand("释读")
                },
            ),
            (
                "检索范围放宽补进来的",
                Candidate {
                    is_scope_filtered: true,
                    ..cand("释读")
                },
            ),
            (
                "草稿",
                Candidate {
                    is_draft: true,
                    ..cand("释读")
                },
            ),
        ] {
            assert!(!is_direct_source(&c, 4), "{why}");
        }
    }

    #[test]
    fn mark_direct_hit_drops_prefix_sentence_identity() {
        let mut c = Candidate {
            is_sentence: true,
            is_sentence_demoted: true,
            sentence_rank: 1,
            consumed_length: 4,
            code: "shidu".into(),
            ..cand("释读")
        };
        mark_direct_hit(&mut c, 5);
        assert_eq!(c.consumed_length, 5);
        assert!(c.is_direct_aux);
        assert!(!c.is_sentence && !c.is_sentence_demoted);
        assert_eq!(c.sentence_rank, 0, "不能冒充本次整句 N-best");
        assert_eq!(c.code, "shidu", "码仍是前缀的拼音码");
    }

    // ── 排序：§6 三种情况与无命中 ──

    fn hit(text: &str) -> Candidate {
        Candidate {
            is_direct_aux: true,
            consumed_length: 5,
            ..cand(text)
        }
    }

    #[test]
    fn placement_by_parity_and_syllables() {
        assert_eq!(direct_placement(1, 2), DirectPlacement::HitsFirst);
        assert_eq!(direct_placement(1, 3), DirectPlacement::AfterTop);
        assert_eq!(
            direct_placement(2, 1),
            DirectPlacement::AfterFullCover(EVEN_KEEP_FULL_COVER)
        );
        assert_eq!(EVEN_KEEP_FULL_COVER, 2);
        assert_eq!(ODD_KEEP_TOP_MIN_SYLLABLES, 3);
    }

    #[test]
    fn no_hits_keeps_main_untouched() {
        let main = vec![cand("湿度"), cand("适度")];
        let out = merge_direct_hits(main, vec![], 5, DirectPlacement::HitsFirst);
        assert_eq!(texts(&out), vec!["湿度", "适度"]);
    }

    #[test]
    fn odd_short_prefix_hits_first_rest_deduped() {
        let main = vec![cand("湿度"), cand("释读"), cand("适度")];
        let out = merge_direct_hits(main, vec![hit("释读")], 5, DirectPlacement::HitsFirst);
        assert_eq!(texts(&out), vec!["释读", "湿度", "适度"]);
        assert!(out[0].is_direct_aux, "去重时留命中项那条（消费整串）");
    }

    #[test]
    fn odd_long_prefix_keeps_engine_top() {
        let main = vec![cand("整句"), cand("甲"), cand("乙")];
        let out = merge_direct_hits(main, vec![hit("命中")], 7, DirectPlacement::AfterTop);
        assert_eq!(texts(&out), vec!["整句", "命中", "甲", "乙"]);
    }

    #[test]
    fn even_keeps_up_to_two_leading_full_cover() {
        let full = |t: &str| Candidate {
            consumed_length: 6,
            ..cand(t)
        };
        let short = |t: &str| Candidate {
            consumed_length: 4,
            ..cand(t)
        };
        let keep2 = DirectPlacement::AfterFullCover(2);
        // 三条完整候选：只留前 2 条在命中项之前。
        let main = vec![full("甲"), full("乙"), full("丙"), short("丁")];
        let out = merge_direct_hits(main, vec![hit("命中")], 6, keep2);
        assert_eq!(texts(&out), vec!["甲", "乙", "命中", "丙", "丁"]);
        // 开头不是完整候选（`pl` 不成音节）：无可保留，命中项直接排最前。
        let main = vec![short("湿度"), full("乙")];
        let out = merge_direct_hits(main, vec![hit("释读")], 6, keep2);
        assert_eq!(
            texts(&out),
            vec!["释读", "湿度", "乙"],
            "只取开头连续的完整候选"
        );
        // 子短语 / 补全 / 简拼虽吃满整串也不算完整候选。
        let abbrev = Candidate {
            is_abbrev: true,
            ..full("简拼")
        };
        let out = merge_direct_hits(vec![abbrev], vec![hit("命中")], 6, keep2);
        assert_eq!(texts(&out), vec!["命中", "简拼"]);
    }

    #[test]
    fn kept_head_wins_over_duplicate_hit() {
        let full = Candidate {
            consumed_length: 6,
            ..cand("释读")
        };
        let out = merge_direct_hits(
            vec![full],
            vec![hit("释读"), hit("试读")],
            6,
            DirectPlacement::AfterFullCover(2),
        );
        assert_eq!(texts(&out), vec!["释读", "试读"]);
        assert!(!out[0].is_direct_aux, "保留段里已有的，命中项不再重复");
    }
}
