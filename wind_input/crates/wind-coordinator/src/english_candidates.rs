//! 英文候选的**头部候选**（输入原文 + 大小写变形）——临时英文与英文方案共用。
//!
//! 设计见 `docs/design/schema-scoped-behavior.md` §5。
//!
//! # 为什么是共用函数
//!
//! 两条路径（`handle_temp.rs` 的临英、`handle_candidate.rs` 的主输入路）的**配置各自独立**
//! （四个键，两侧各一对，默认值还刻意相反），但**产出必须逐字节相同**——否则同一串输入在
//! 两个入口给出的候选不一样，而用户根本不知道自己此刻在哪条路径上。
//!
//! 配置分开、实现共用：分歧只允许出现在「要不要生成」，不允许出现在「生成什么」。
//! 两份实现分叉只是时间问题——`phrase_owns_code` 的注释里已记过一次同型教训。
//!
//! # 为什么头部候选**不带** `source` / `code`
//!
//! 它们没有词库来源。写端 `record_selection_in` 的守卫是 `cand.source != English { return }`，
//! 据此把它们排除在词频之外；否则会写出「读端按候选码永远查不中」的孤儿键
//! （与「短语有文本无码位恒不记词频」同一先例）。
//!
//! # 为什么调用方必须把它们钉在词库段**之前**
//!
//! 「首候选恒是所打原文」是这条能力的全部意义：打词库里没有的词时，原文是唯一能上屏的
//! 东西。词频重排若作用到整个列表，用户按空格就会上屏一个他没打的词。故两处调用方都
//! 只对**词库段**跑 `apply_freq_rerank_in` / `apply_shadow_in`。

use crate::key_convert::en_case_variants;
use wind_candidate::Candidate;

/// 生成头部候选：`[原文] + [大小写变形…]`，内部已去重（变形与原文相同者不产出）。
///
/// `with_raw` / `with_variants` 由调用方按**自己那一侧**的配置传入
/// （临英 `input.temp_english.*`，英文方案 `schema.english.*`）。
///
/// 两者都为 `false`，或 `raw` 为空时返回空表——调用方据此走「无头部候选」的既有路径。
/// ⚠️ 两者都关且词库无命中时最终候选会是空的，那不是缺陷，但上屏出口必须能接住
/// （见 §5.5：临英空格臂判的是「实际候选是否为空」，不是本配置）。
pub(crate) fn english_head_candidates(
    raw: &str,
    with_raw: bool,
    with_variants: bool,
) -> Vec<Candidate> {
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Candidate> = Vec::new();
    if with_raw {
        out.push(Candidate {
            text: raw.to_string(),
            ..Default::default()
        });
    }
    if with_variants {
        for v in en_case_variants(raw) {
            // `en_case_variants` 已剔除与原文相同者；这里再挡一次是为了 `with_raw = false`
            // 时不重复——那种配置下原文不入列，但变形里可能恰好有一条等于原文。
            if v == raw && with_raw {
                continue;
            }
            if out.iter().any(|c| c.text == v) {
                continue;
            }
            out.push(Candidate {
                text: v,
                ..Default::default()
            });
        }
    }
    out
}

/// 英文候选的大小写档位：CapsLock 在英文输入态临时切换的三档（见 `input.english_case_cycle`）。
///
/// `Default` 档才跑逐位投影；另两档整条套形，此时投影不再参与——用户显式要了一种形态，
/// 再按输入形态改写它就是在推翻用户刚下的指令。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CaseVariant {
    #[default]
    Default,
    Upper,
    Lower,
}

impl CaseVariant {
    /// 循环下一档：默认 → 全大写 → 全小写 → 默认。
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Default => Self::Upper,
            Self::Upper => Self::Lower,
            Self::Lower => Self::Default,
        }
    }
}

/// 把候选投影成用户所打的大小写形态；返回 `None` = 不投影（调用方保留原文）。
///
/// # 规则（**单向**：只投影大写，不投影小写）
///
/// 逐位比较输入 `raw` 与候选 `cand` 的公共前缀：输入位是大写字母且与候选位忽略大小写
/// 相等 → 候选该位改大写；输入位是小写 → 候选该位**原样不动**；任一位忽略大小写都不相等
/// → **整条放弃投影**。超出输入长度的后缀一律保留词库原文。
///
/// ## 为什么小写不投影
///
/// 词库里 `China` / `iPhone` / `I` 这类条目的大写是词本身的一部分。双向投影会让打 `chi`
/// 的用户再也打不出 `China`——而他并没有表达「我要小写」的意思，小写只是默认击键形态。
/// 大写则相反：按下 Shift 是一个显式动作，那才是用户的意图表达。
///
/// ## 为什么「不对应」要整条放弃，而不是跳过该位
///
/// 英文引擎是大小写不敏感的**前缀**匹配，正常词库候选必然逐位对应；对不上的只会是短语 /
/// 命令 / `$` 展开这类没有前缀关系的条目。对它们逐位跳过会产出既非词库形态、也非用户所打
/// 的杂合体（`WoWzy` 一类），而整条放弃恰好把它们原样留下。
///
/// ## 为什么无大写时返回 `None` 而不是原串
///
/// 绝大多数击键都是全小写，这是逐键热路径——`None` 让调用方零分配地走原路。
pub(crate) fn project_to_input_case(raw: &str, cand: &str) -> Option<String> {
    if !raw.chars().any(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let mut out = String::with_capacity(cand.len());
    let mut raw_it = raw.chars();
    for c in cand.chars() {
        match raw_it.next() {
            // 输入已用尽：后缀保留词库原文。
            None => out.push(c),
            Some(r) => {
                if !r.eq_ignore_ascii_case(&c) {
                    return None; // 字母不对应 ⇒ 整条放弃
                }
                out.push(if r.is_ascii_uppercase() {
                    c.to_ascii_uppercase()
                } else {
                    c
                });
            }
        }
    }
    (out != cand).then_some(out)
}

/// 对整列候选应用大小写处理：`Default` 档跑逐位投影，另两档整条套形。
///
/// ★ **必须排在词频重排与候选调整之后**：两者都以候选 `text` 为键，先改写文本会让读写两端
/// 用不同的键，英文词频与置顶**静默失效**。被改写的候选一律把原文留在
/// [`Candidate::case_source`] 里，记账走 [`Candidate::freq_text`]。
///
/// ★ 调用方须在本函数**之后**再去重：投影会让词库的 `hi` 变成 `Hi`，与头部原文候选撞车。
/// 返回**是否改写过任何一条**——调用方据此决定要不要再跑一次去重（逐键热路径，
/// 全小写击键占绝大多数，那时一条也不会改写，去重的 HashSet 分配可以整个省掉）。
pub(crate) fn apply_english_case(cands: &mut [Candidate], raw: &str, variant: CaseVariant) -> bool {
    let mut changed = false;
    for c in cands.iter_mut() {
        // 先还原成词库原文：档位切换必须**可逆**。从「全大写」回「默认」时，若拿 `HILL`
        // 去投影，`Hi` 的小写位不改大写位（单向规则）⇒ 结果还是 `HILL`，档位再也切不回来。
        // 逐键路径上候选是新建的（`case_source` 恒 None），这一步是纵深防御 + 让本函数幂等。
        if let Some(orig) = &c.case_source
            && c.text != *orig
        {
            // 还原本身就是一次改写（`HILL` → `hill`），调用方据 `changed` 决定要不要重去重。
            changed = true;
            c.text = orig.clone();
        }
        let projected = match variant {
            CaseVariant::Default => project_to_input_case(raw, &c.text),
            CaseVariant::Upper => {
                let up = c.text.to_uppercase();
                (up != c.text).then_some(up)
            }
            CaseVariant::Lower => {
                let low = c.text.to_lowercase();
                (low != c.text).then_some(low)
            }
        };
        if let Some(text) = projected {
            changed = true;
            // 只在首次改写时记原文：档位切换会连续改写同一批候选（每次按键重建候选表时
            // case_source 恒为 None，这里是纵深防御——保住的是「原文」而非「上一档的形态」。
            if c.case_source.is_none() {
                c.case_source = Some(std::mem::replace(&mut c.text, text));
            } else {
                c.text = text;
            }
        }
    }
    changed
}

/// 按**精确文本**去重，保序保留首条。
///
/// 大小写处理之后必须再跑一次：投影会把词库的 `hi` 改写成 `Hi`，与头部原文候选撞车；
/// 全大写/全小写档更会把三条大小写变形塌成同一条。去重若仍停在改写之前，候选窗里就会
/// 出现两条一模一样的行。
pub(crate) fn dedup_by_text(cands: &mut Vec<Candidate>) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    cands.retain(|c| seen.insert(c.text.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(v: &[Candidate]) -> Vec<&str> {
        v.iter().map(|c| c.text.as_str()).collect()
    }

    /// 四种开关组合的产出，逐格钉住。
    #[test]
    fn switch_matrix() {
        assert_eq!(
            texts(&english_head_candidates("Hel", true, true)),
            vec!["Hel", "hel", "HEL"],
            "原文在最前，变形跟随（首字母大写形态等于原文，已被剔除）"
        );
        assert_eq!(
            texts(&english_head_candidates("Hel", true, false)),
            vec!["Hel"]
        );
        assert_eq!(
            texts(&english_head_candidates("Hel", false, true)),
            vec!["hel", "HEL"],
            "不要原文时只剩变形"
        );
        assert!(
            english_head_candidates("Hel", false, false).is_empty(),
            "两者皆关 = 无头部候选，调用方走既有路径"
        );
    }

    /// 空输入恒空——别让它产出一条空文本候选（那会在候选窗里显示成一个空行）。
    #[test]
    fn empty_input_yields_nothing() {
        assert!(english_head_candidates("", true, true).is_empty());
    }

    /// 头部候选**不带** `source` / `code`：写端据此把它们排除在词频之外。
    ///
    /// 这条不是形式检查——带上 source 的后果是往词频表里写一批读端永远查不中的孤儿键，
    /// 且完全静默。
    #[test]
    fn head_candidates_carry_no_source_or_code() {
        for c in english_head_candidates("Hel", true, true) {
            assert_eq!(c.source, wind_candidate::CandidateSource::default());
            assert!(c.code.is_empty(), "头部候选不得带码：{}", c.text);
        }
    }

    // ── 大小写投影 ─────────────────────────────────────────────────────

    /// 用户给的两个例子，逐格钉住。
    #[test]
    fn projects_user_examples() {
        assert_eq!(project_to_input_case("Hi", "hill").as_deref(), Some("Hill"));
        assert_eq!(project_to_input_case("WoW", "wow").as_deref(), Some("WoW"));
        assert_eq!(
            project_to_input_case("WoW", "wowed").as_deref(),
            Some("WoWed"),
            "超出输入长度的后缀保留词库原文"
        );
    }

    /// 全小写输入不投影——绝大多数击键走这条零分配快路。
    #[test]
    fn all_lowercase_input_projects_nothing() {
        assert_eq!(project_to_input_case("hi", "hill"), None);
        assert_eq!(project_to_input_case("", "hill"), None);
    }

    /// ★ 单向：小写位不把词库自带的大写压下去，否则 `China` / `iPhone` 再也打不出来。
    #[test]
    fn lowercase_never_overwrites_dict_case() {
        assert_eq!(
            project_to_input_case("chi", "China"),
            None,
            "无大写输入 ⇒ 不投影"
        );
        assert_eq!(
            project_to_input_case("Iph", "iPhone").as_deref(),
            Some("IPhone"),
            "用户按了 Shift 才覆盖，且只覆盖他按了的那两位"
        );
    }

    /// ★ 字母不对应 ⇒ 整条放弃，不产出杂合体。
    #[test]
    fn mismatched_letters_abandon_whole_candidate() {
        assert_eq!(project_to_input_case("Wo", "xyz"), None);
        assert_eq!(
            project_to_input_case("Wo", "w-o"),
            None,
            "非字母位同样按不对应处理（短语/命令类候选原样留下）"
        );
    }

    /// 投影结果与原文相同时返回 `None`，避免调用方白记一次 `case_source`。
    #[test]
    fn no_change_yields_none() {
        assert_eq!(
            project_to_input_case("Hi", "Hill"),
            None,
            "词库本就是这个形态"
        );
    }

    /// 三档循环闭合。
    #[test]
    fn variant_cycles_back_to_default() {
        let mut v = CaseVariant::Default;
        v = v.next();
        assert_eq!(v, CaseVariant::Upper);
        v = v.next();
        assert_eq!(v, CaseVariant::Lower);
        assert_eq!(v.next(), CaseVariant::Default);
    }

    /// ★★ 记账文本恒是投影**前**的词库原文——两端不同源就是英文词频静默失效。
    #[test]
    fn projection_keeps_original_text_for_freq() {
        let mut cands = vec![Candidate {
            text: "hill".into(),
            ..Default::default()
        }];
        apply_english_case(&mut cands, "Hi", CaseVariant::Default);
        assert_eq!(cands[0].text, "Hill", "显示与上屏用投影后");
        assert_eq!(cands[0].freq_text(), "hill", "记账用投影前");
    }

    /// 档位切换连续改写同一条候选时，`case_source` 仍是词库原文而非上一档的形态。
    #[test]
    fn variant_switch_keeps_original_not_previous_form() {
        let mut cands = vec![Candidate {
            text: "hill".into(),
            ..Default::default()
        }];
        apply_english_case(&mut cands, "Hi", CaseVariant::Default);
        apply_english_case(&mut cands, "Hi", CaseVariant::Upper);
        assert_eq!(cands[0].text, "HILL");
        assert_eq!(cands[0].freq_text(), "hill");
    }

    /// 全大写/全小写档整条套形，与输入形态无关。
    #[test]
    fn upper_and_lower_variants_ignore_input_shape() {
        let mk = || {
            vec![Candidate {
                text: "hill".into(),
                ..Default::default()
            }]
        };
        let mut up = mk();
        apply_english_case(&mut up, "hi", CaseVariant::Upper);
        assert_eq!(up[0].text, "HILL", "输入全小写也照样全大写");
        let mut low = mk();
        low[0].text = "China".into();
        apply_english_case(&mut low, "Chi", CaseVariant::Lower);
        assert_eq!(low[0].text, "china");
    }

    /// 去重保序保留首条。
    #[test]
    fn dedup_keeps_first_occurrence() {
        let mut v: Vec<Candidate> = ["Hi", "hi", "Hi", "HI"]
            .iter()
            .map(|t| Candidate {
                text: (*t).into(),
                ..Default::default()
            })
            .collect();
        dedup_by_text(&mut v);
        assert_eq!(texts(&v), vec!["Hi", "hi", "HI"]);
    }

    /// 无字母的输入三形态相同，变形为空——只剩原文。
    #[test]
    fn non_alpha_input_has_no_variants() {
        assert_eq!(
            texts(&english_head_candidates("123", true, true)),
            vec!["123"]
        );
    }
}
