//! 英文候选的**头部候选**（输入原文 + 大小写变形）——临时英文与英文方案共用；快捷输入的
//! 英文成员读临英那份开关、同样经这里生成（A2-3b，见 `update_mix_candidates`）。
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
use wind_candidate::{Candidate, CandidateSource};
use wind_config::config::RawCandidateMode;

/// [`RawCandidateMode`] 的落点：这一次输入要不要产出「原文」那条头部候选。
///
/// 两侧调用方（主输入路 / 临英）都经本函数，档位语义因此只有一份实现——同本模块
/// 「配置分开、实现共用」的纪律：分歧只允许出现在*要不要生成*，不允许出现在*生成什么*。
///
/// # `InDict` 的判据：`dict` 里有没有与原文**字面**相同的**英文词库**候选
///
/// 判据用字面相等（`==`），**不是** `eq_ignore_ascii_case`、**也不是** `is_exact_code`，
/// 理由见 [`RawCandidateMode`] 的类型文档（`he'll` 的 code 同为 `hell`；`usa` 的字面不在
/// 词库里）。口径与紧邻调用点的 [`merge_head_with_dict`] 一致，两处必须同进同退：那里按字面
/// 让词库同名词占据头部格，判据也按字面认定「同名候选存在」。
///
/// ★ **必须先按 `source` 收到英文词库来源上**，只比 `text` 会被短语误判。主输入路传进来的
/// 是**整张候选表**，走到这里时短语/命令/组候选已经并进去了（`finalize_candidates` 还把
/// `$` 语法展开成了显示文本）；只要有一条短语的文本恰好等于所打原文，`in_dict` 就会在
/// 「这个词根本不在英文词库里」时产出原文候选，而且紧接着的精确去重还会把用户那条短语从
/// 列表里抹掉、换上一条无来源的原文候选。`case_follow_input` 的投影跑在整列上、也会改写
/// 短语文本，误判面比看上去更大。
///
/// ⚠️ 这是**防御性**的：出厂 `english.schema.toml` 写着 `[phrases] enabled = false`
/// （理由见该文件——`date` / `tel` / `em` 这类既是短语码也是英文词，打英文时会莫名跳出
/// 中文内容），故出厂配置下英文方案根本不出短语候选，这条守卫在端到端层面**测不出来**，
/// 单元测试 `in_dict_ignores_non_dictionary_sources` 直接喂判据一张含短语的表来钉它。
/// 定制层或第三方英文方案一旦打开 `[phrases]`，它就是实打实的。
///
/// 收到 `source` 上之后，两个调用点的答案才真正同口径——这正是本模块「产出必须逐字节
/// 相同」那条纪律要求的。
///
/// ⚠️ `dict` 必须已过 shadow 删除与大小写投影：
/// - 用户把某个词隐藏掉了，它就不该再让原文候选冒出来；
/// - 投影会改写候选文本，判据在投影前后求值的结果不同（见调用点注释）。
///
/// ⚠️ **`dict` 为空要区分「问过没命中」与「根本没问」**。临英在
/// `show_candidates = false` 时压根不查词库（`overlay_engine_schema` 返回 `None`），
/// 那种情况下判据无从回答，调用方须退回 [`RawCandidateMode::Always`] 而不是让它恒假
/// ——否则「原文那条在候选关闭时仍是『空格上屏什么』的依据」这条既有语义就断了。
/// 词组分词生效时，[`RawCandidateMode::Always`] 降级成 [`RawCandidateMode::InDict`]。
///
/// # 为什么降级而不是一刀切「不产」
///
/// `Always` 档的立论前提是「英文引擎输入即内容，打词库里没有的词时原文是唯一能上屏的
/// 东西」。含分词符的串**不是内容、是查询语法**，那个前提不成立——打 `ip'pro` 得到字面
/// `ip'pro` 不是任何人想要的，而出厂 `Always` 下它还恒占首位、把真正想要的词组挤到
/// 第二条起（临英更甚：`case_variants` 出厂开着，首三条是 `Ip'pro` / `ip'pro` / `IP'PRO`）。
///
/// 但**一刀切不产会误伤**：`o'clock` / `you're` / `O'Reilly` 这类串既含分词符、原文又
/// 真是词库词，它们本该保住首位。降级成 `InDict` 的「原文字面是不是词库词」判据两边都
/// 照顾到了，而 `InDict` / `Off` 两档行为完全不变——三档天然共存，不需要新旋钮。
///
/// # ⚠️ `phrase_active` 必须由调用方传，不能在这里嗅 `raw.contains('\'')`
///
/// 临英开了 `allow_symbols` 且把 `'` 列进 `symbol_chars` 时，`don't` 是**正常的字面输入**
/// （那是该配置存在的意义），与词组分词无关，不该被本规则管。判据是「本作用域的分词开关
/// 开着**且**这串里确实有分词符」，只有调用方知道前一半。
pub(crate) fn raw_mode_under_phrase_seg(
    mode: RawCandidateMode,
    phrase_active: bool,
) -> RawCandidateMode {
    if phrase_active && mode == RawCandidateMode::Always {
        RawCandidateMode::InDict
    } else {
        mode
    }
}

/// 这一次输入要不要产出原文条（档位语义见上方 [`raw_mode_under_phrase_seg`] 前的说明）。
///
/// # 被「屏蔽这个词」的原文仍算词库词
///
/// 屏蔽滤掉的是词库那条候选，不是用户打原文的权利（§8 第 11 条：输入原文那一条不受屏蔽）。
/// 只看 `dict` 的话，打全一个被屏蔽的词时 `InDict` 判据恒假、原文条跟着消失，空格上屏的
/// 就成了别的词。故 `blocked`（该英文数据方案的屏蔽集，键是词库原文）里的词按它**本来会显示
/// 的样子**再比一次：字面相同，或开着大小写跟随（`follow_case`）时投影后与原文相同
/// （屏蔽 `help` 后打 `Help`）——与没屏蔽时 `dict` 那条投影后的判据同口径。
pub(crate) fn wants_raw_candidate(
    mode: RawCandidateMode,
    raw: &str,
    dict: &[Candidate],
    blocked: Option<&std::collections::HashSet<String>>,
    follow_case: bool,
) -> bool {
    match mode {
        RawCandidateMode::Always => true,
        RawCandidateMode::Off => false,
        RawCandidateMode::InDict => {
            dict.iter()
                .any(|c| c.source == CandidateSource::English && c.text == raw)
                || blocked.is_some_and(|b| {
                    b.contains(raw)
                        || (follow_case
                            && b.iter().any(|w| {
                                // 先按忽略大小写预筛：投影要分配，屏蔽集里绝大多数词与原文无关。
                                w.eq_ignore_ascii_case(raw)
                                    && project_to_input_case(raw, w).as_deref() == Some(raw)
                            }))
                })
        }
    }
}

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

/// 头部候选并入词库段（英文方案主路 / 临英 / 快捷输入英文成员三路共用）：返回
/// `头部段 ++ 剩余词库段`。
///
/// 头部某条与**英文词库**候选字面相同时，由那条词库候选**占据头部那一格**（保留来源 /
/// 编码 / 释义等属性），不再另出一条无来源的头部候选。位置语义不变：头部仍钉在词库段
/// 之前、不受调频（词库段的重排早已算完，这里只是把同名那条挪到头部那一格）。
///
/// 此前是精确去重：词库同名词被头部**吃掉**，那一格只剩一条无来源、无码、无释义的原文。
///
/// 只认 `source == English`：短语 / 命令等同名项不是「这个词」本身（命令的文本是显示标签，
/// 顶到头部会把「上屏原文」变成「执行动作」），照旧被头部吃掉——与 [`wants_raw_candidate`]
/// 的 `InDict` 判据同一口径（只认英文词库来源、按字面）。
///
/// 记账不受影响：占位后它就是一条普通词库候选，按词库来源记词频；没被占位的头部候选
/// 仍经 `english_head_dict_word` 找回词库词记账（dc03ba4c）。
pub(crate) fn merge_head_with_dict(
    head: Vec<Candidate>,
    mut dict: Vec<Candidate>,
) -> Vec<Candidate> {
    if head.is_empty() {
        return dict;
    }
    let mut out = Vec::with_capacity(head.len() + dict.len());
    for h in head {
        // 同名的有多条时（投影把词库的 `asp` 改写成 `ASP`，词库里又有真 `ASP`），先取
        // **没被改写**的那条：它才是字面上就是这个词的词库词，投影出来的那条只是形态撞了名。
        let same = |c: &Candidate| c.source == CandidateSource::English && c.text == h.text;
        match dict
            .iter()
            .position(|c| same(c) && c.case_source.is_none())
            .or_else(|| dict.iter().position(same))
        {
            Some(i) => out.push(dict.remove(i)),
            None => out.push(h),
        }
    }
    // 精确去重（不是小写去重）：`hello` 不该把词库里的 `Hello` 一起抹掉。
    let texts: std::collections::HashSet<String> = out.iter().map(|c| c.text.clone()).collect();
    dict.retain(|c| !texts.contains(&c.text));
    out.append(&mut dict);
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
///
/// 同名的几条里有**没被改写**的（`case_source` 为空）时由它占那一格：词库同时有 `asp` 与
/// `ASP`、输入 `ASP` 时，投影出来的 `asp` 可能因词频更高排在前面，保留首条就把真 `ASP`
/// 吃掉了（论坛 t281 审查）。占格保留那一格的 `natural_order`，位次语义不变。
pub(crate) fn dedup_by_text(cands: &mut Vec<Candidate>) {
    let mut first: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut keep = vec![true; cands.len()];
    for i in 0..cands.len() {
        match first.get(&cands[i].text) {
            None => {
                first.insert(cands[i].text.clone(), i);
            }
            Some(&f) => {
                keep[i] = false;
                if cands[f].case_source.is_some() && cands[i].case_source.is_none() {
                    let order = cands[f].natural_order;
                    cands.swap(f, i);
                    cands[f].natural_order = order;
                }
            }
        }
    }
    let mut it = keep.into_iter();
    cands.retain(|_| it.next().unwrap_or(true));
}

/// 这条候选是不是**英文头部候选**（所打原文 / 它的大小写变形）：无来源、无码、非短语 /
/// 命令 / emoji，且文本是缓冲 `buf` 的某种大小写形态。
///
/// 头部候选由 [`english_head_candidates`] 生成，候选身上没有「我是头部」的标记，只能按
/// 这组特征认；[`crate::coordinator::Coordinator::english_head_dict_word`] 与英文方案主路的
/// 记账分流（`record_selection_cand`）共用本判据，两处必须同一口径。
pub(crate) fn is_english_head_candidate(cand: &Candidate, buf: &str) -> bool {
    cand.source == CandidateSource::None
        && cand.code.is_empty()
        && !cand.is_phrase
        && !cand.is_command
        && !cand.is_emoji_suggestion
        && !buf.is_empty()
        && cand.freq_text().eq_ignore_ascii_case(buf)
}

/// 「按一个完整词库码精确查词」的取数上限——头部候选找对应词库词（A2-39②）与英文词频
/// 召回验证（A2-39③）共用。
///
/// 同码的词库条目只是同一个词的几种大小写写法（`us` / `US`），而引擎把精确码命中排在
/// 前缀补全之前，8 条绰绰有余。⚠️ 它只管**返回**几条：码表引擎内部查词库时按
/// `max(limit, 50)` 取数再截断（`CodeTableEngine::convert`），调小它省不了查询本身。
pub(crate) const ENGLISH_EXACT_LOOKUP_LIMIT: usize = 8;

impl crate::coordinator::Coordinator {
    /// 头部候选（所打原文 / 大小写变形）对应的**英文词库词**——记词频用（A2-39②）。
    ///
    /// 打全 `translation` 时，词库里的 `translation` 被头部候选按字面去重吃掉，用户上屏的
    /// 是头部那条（原文 `Translation` 或变形 `translation`）。头部候选没有词库来源，写端
    /// 若只认来源，这次上屏就一条都不记——而「打全再上屏」恰是最常见的用法。
    ///
    /// 按英文词库的键规则找回它：码 = 小写化的缓冲（词库 code 列恒小写），在**精确码**命中里
    /// 先找字面相同的，再找仅大小写不同的（打 `usa` 上屏，记到词库的 `USA`）。找到就返回
    /// 那条词库候选，但 `text` 换成**实际上屏的头部文本**、词库原文留在 `case_source`——与
    /// 大小写投影同一个表示法：`freq_text()` 是词频键，`text` 仍是用户看见的形态。
    /// 快捷输入与英文方案经 `record_selection_cand_in` 记账，那里历史取 `text`；临英的
    /// 上屏历史不经本函数，由 `record_temp_english_selection` 直接记实际上屏文本。
    ///
    /// 找不到（纯原文，不是词库词）返回 `None`，调用方照旧不记词频：词频是**已有候选的排序
    /// 维度**，不是词条来源——记下来就是读端永远查不中的孤儿键
    /// （`temp_english_literal_text_is_not_recorded` 钉着）。想让自造词参与排序，该走加词。
    ///
    /// `cand` 不是头部候选（有来源 / 有码 / 短语 / emoji，或文本不是缓冲的大小写形态）、
    /// 或 `engine_schema` 不记词频（调频关且未开 `record_always`）时直接 `None`，不查词库。
    pub(crate) fn english_head_dict_word(
        &self,
        engine_schema: &str,
        buf: &str,
        cand: &Candidate,
    ) -> Option<Candidate> {
        if !is_english_head_candidate(cand, buf) {
            return None;
        }
        let head_text = cand.freq_text();
        // 不记词频就不查：结果只用来记词频，而写端本来也会因开关关着而不写。先判开关，
        // 关着时连这次查询与它的副作用（临英的上屏历史）都不发生，行为与改前逐字相同。
        // ⚠️ 判据是写端的 `records()` 而非 `enabled`：这里是**写端**的一环，用 `enabled` 会让
        // 「仅记录次数」（`record_always`）在最常见的「打全词按空格」下一条都不记（论坛 t278）。
        if !self.engine_mgr.freq_settings_for(engine_schema).records() {
            return None;
        }
        let mut word = self.english_exact_dict_word(engine_schema, buf, head_text)?;
        if word.text != cand.text {
            word.case_source = Some(std::mem::replace(&mut word.text, cand.text.clone()));
        }
        Some(word)
    }

    /// 按英文词库的键规则找 `head_text` 对应的词库词：码 = 小写化的缓冲，在**精确码**命中里先找
    /// 字面相同的，再找仅大小写不同的。返回词库原样的那条候选（`text` 即词库原文）。
    /// 记词频（[`Self::english_head_dict_word`]）与右键删除头部原文候选（`delete_target`，它只收
    /// 字面相同的那支）共用。
    /// 被「屏蔽这个词」的词查不到（引擎管理器已滤掉）。
    pub(crate) fn english_exact_dict_word(
        &self,
        engine_schema: &str,
        buf: &str,
        head_text: &str,
    ) -> Option<Candidate> {
        let code = buf.to_lowercase();
        let hits: Vec<Candidate> = self
            .engine_mgr
            .convert_with(engine_schema, &code, ENGLISH_EXACT_LOOKUP_LIMIT)
            .candidates
            .into_iter()
            .filter(|c| c.source == CandidateSource::English && c.code == code)
            .collect();
        hits.iter()
            .find(|c| c.text == head_text)
            .or_else(|| hits.iter().find(|c| c.text.eq_ignore_ascii_case(head_text)))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    fn dict_cand(text: &str) -> Candidate {
        Candidate {
            text: text.to_string(),
            source: CandidateSource::English,
            ..Default::default()
        }
    }

    fn phrase_cand(text: &str) -> Candidate {
        Candidate {
            text: text.to_string(),
            source: CandidateSource::Phrase,
            is_phrase: true,
            ..Default::default()
        }
    }

    /// `InDict` 只认**英文词库来源**：文本相同但来源是短语的那条不得让判据成立。
    ///
    /// 出厂英文方案不加载短语（`english.schema.toml` 的 `[phrases] enabled = false`），
    /// 所以这条守卫在端到端层面测不出来——必须在这里直接喂判据一张含短语的表。
    /// 定制层或第三方方案打开 `[phrases]` 后它才会真的被走到。
    #[test]
    fn in_dict_ignores_non_dictionary_sources() {
        let raw = "zzq";
        assert!(
            !wants_raw_candidate(
                RawCandidateMode::InDict,
                raw,
                &[phrase_cand("zzq")],
                None,
                false
            ),
            "短语文本恰好等于所打原文，不代表这个词在英文词库里"
        );
        assert!(
            wants_raw_candidate(
                RawCandidateMode::InDict,
                raw,
                &[dict_cand("zzq")],
                None,
                false
            ),
            "同样的文本、来源是英文词库时，判据必须成立"
        );
        // 混表：短语在前、词库在后，仍应以词库那条为准。
        assert!(
            wants_raw_candidate(
                RawCandidateMode::InDict,
                raw,
                &[phrase_cand("zzq"), dict_cand("zzq")],
                None,
                false
            ),
            "混表下只要存在词库同名候选，判据就该成立"
        );
    }

    /// 另外两档不看 `dict`，恒定返回。
    #[test]
    fn always_and_off_ignore_the_dictionary() {
        assert!(wants_raw_candidate(
            RawCandidateMode::Always,
            "zzq",
            &[],
            None,
            false
        ));
        assert!(!wants_raw_candidate(
            RawCandidateMode::Off,
            "zzq",
            &[dict_cand("zzq")],
            None,
            false
        ));
    }

    /// 被屏蔽的词仍算词库词：`InDict` 下原文条不因屏蔽消失。按它本来会显示的样子比：字面，或大小写
    /// 跟随开着时投影后的形态；`OK` 不因屏蔽了 `ok` 而算。
    #[test]
    fn in_dict_counts_blocked_word() {
        let blocked: std::collections::HashSet<String> = ["help".to_string()].into();
        let in_dict = |raw: &str, follow: bool| {
            wants_raw_candidate(RawCandidateMode::InDict, raw, &[], Some(&blocked), follow)
        };
        assert!(in_dict("help", false));
        assert!(in_dict("Help", true), "投影后 help → Help");
        assert!(!in_dict("Help", false), "不跟随大小写时 help 不显示成 Help");
        assert!(!in_dict("Hel", true), "前缀不算");
        assert!(!wants_raw_candidate(
            RawCandidateMode::InDict,
            "help",
            &[],
            None,
            false
        ));
    }

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

    /// 头部格由同名**英文词库**候选占据；同名短语不算（照旧被头部吃掉），其余词库段保序。
    #[test]
    fn merge_head_keeps_dict_word_in_head_slot() {
        let mut word = dict_cand("hello");
        word.code = "hello".into();
        word.comment = "释义".into();
        let head = english_head_candidates("hello", true, true);
        let dict = vec![
            dict_cand("help"),
            phrase_cand("HELLO"),
            word,
            dict_cand("Hello"),
        ];
        let merged = merge_head_with_dict(head, dict);
        assert_eq!(texts(&merged), vec!["hello", "Hello", "HELLO", "help"]);
        assert_eq!(
            merged[0].source,
            CandidateSource::English,
            "原文那一格是词库词"
        );
        assert_eq!(merged[0].comment, "释义", "词库词的属性保留");
        assert_eq!(merged[1].source, CandidateSource::English, "变形格同理");
        assert_eq!(
            merged[2].source,
            CandidateSource::None,
            "同名短语不占格：头部变形照旧，短语被吃掉"
        );
    }

    /// 词库同时有 `asp`（词频更高）与 `ASP`，输入 `ASP`：投影后的 `asp` 与真 `ASP` 同名
    /// （论坛 t281 审查）。占据头部格的必须是**没被改写**的真 `ASP`，投影出来的那条被吃掉。
    #[test]
    fn merge_head_prefers_unrewritten_dict_word() {
        let mut dict = vec![dict_cand("asp"), dict_cand("ASP")];
        assert!(apply_english_case(&mut dict, "ASP", CaseVariant::Default));
        assert_eq!(
            texts(&dict),
            vec!["ASP", "ASP"],
            "前提：asp 投影后与真 ASP 同名"
        );
        let merged = merge_head_with_dict(english_head_candidates("ASP", true, false), dict);
        assert_eq!(texts(&merged), vec!["ASP"]);
        assert_eq!(merged[0].source, CandidateSource::English);
        assert_eq!(
            merged[0].case_source, None,
            "头部格是真 ASP，不是投影出来的 asp"
        );
    }

    /// 改写后同名时保留未被改写的那条，且占**首次出现的那一格**（位次不变）。
    #[test]
    fn dedup_prefers_unrewritten_and_keeps_slot() {
        let mut v = vec![dict_cand("asp"), dict_cand("help"), dict_cand("ASP")];
        for (i, c) in v.iter_mut().enumerate() {
            c.natural_order = i as i32;
        }
        assert!(apply_english_case(&mut v, "ASP", CaseVariant::Default));
        dedup_by_text(&mut v);
        assert_eq!(texts(&v), vec!["ASP", "help"]);
        assert_eq!(v[0].case_source, None, "留下的是真 ASP");
        assert_eq!(v[0].natural_order, 0, "占首次出现那一格的位次");
        // 都改写过 / 都没改写：仍保留首条。
        let mut w = vec![dict_cand("hill"), dict_cand("HILL")];
        w[1].case_source = Some("Hill".into());
        w[0].case_source = Some("hill".into());
        w[0].text = "HILL".into();
        dedup_by_text(&mut w);
        assert_eq!(w[0].freq_text(), "hill");
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
