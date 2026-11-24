//! 英文候选的头部候选（输入原文 + 大小写变形）：临时英文与英文方案共用；快捷输入的英文成员
//! 读临英那份开关，同样经这里生成（见 `update_mix_candidates`）。
//! 设计见 `docs/design/schema-scoped-behavior.md` §5。
//!
//! 两条路径（`handle_temp.rs` 的临英、`handle_candidate.rs` 的主输入路）配置各自独立，但产出
//! 必须逐字节相同：分歧只允许出现在「要不要生成」，不允许出现在「生成什么」。
//!
//! 头部候选不带 `source` / `code`：它们没有词库来源，写端 `record_selection_in` 据此不记词频，
//! 否则会写出读端永远查不中的孤儿键。
//!
//! 调用方必须把它们钉在词库段之前，且只对词库段跑 `apply_freq_rerank_in` / `apply_shadow_in`：
//! 打词库里没有的词时原文是唯一能上屏的东西，重排整列会让空格上屏一个没打的词。

use crate::key_convert::en_case_variants;
use wind_candidate::{Candidate, CandidateSource};
use wind_config::config::RawCandidateMode;

/// 词组分词生效时，[`RawCandidateMode::Always`] 降级成 [`RawCandidateMode::InDict`]。
///
/// 含分词符的串是查询语法而非内容，打 `ip'pro` 得到字面 `ip'pro` 并恒占首位不是任何人想要的；
/// 但 `o'clock` / `you're` 这类原文又真是词库词，故降级而非一刀切不产。
///
/// `phrase_active` 必须由调用方传，不能在这里嗅 `raw.contains('\'')`：临英开了 `allow_symbols`
/// 且列了 `'` 时，`don't` 是正常的字面输入，与分词无关。
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

/// [`RawCandidateMode`] 的落点：这一次输入要不要产出「原文」那条头部候选。两侧调用方都经
/// 本函数，档位语义只有一份实现。
///
/// `InDict` 的判据：`dict` 里有没有与原文字面相同（`==`，理由见 [`RawCandidateMode`]）的英文
/// 词库候选，口径与 [`merge_head_with_dict`] 一致。必须先按 `source` 收到英文词库来源上：主输入路
/// 传进来的是整张候选表，短语文本恰好等于原文时会误判，随后的精确去重还会抹掉那条短语。出厂英文
/// 方案关了短语，端到端测不出，由 `in_dict_ignores_non_dictionary_sources` 钉住。
///
/// `dict` 必须已过 shadow 删除与大小写投影。`dict` 为空要区分「问过没命中」与「根本没问」：
/// 临英 `show_candidates = false` 时不查词库，调用方须退回 [`RawCandidateMode::Always`]。
///
/// 被屏蔽的词仍算词库词（屏蔽不剥夺打原文的权利）：`blocked`（该英文数据方案的屏蔽集）里的词
/// 按它本来会显示的样子再比一次，即字面相同，或开着 `follow_case` 时投影后与原文相同。
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
/// 两者都为 `false` 或 `raw` 为空时返回空表。两者都关且词库无命中时最终候选为空，上屏出口
/// 必须能接住（设计 §5.5：临英空格臂判的是实际候选是否为空）。
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
            // `en_case_variants` 已剔除与原文相同者，这里是纵深防御。
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
/// 头部某条与英文词库候选字面相同时，由那条词库候选占据头部那一格（保留来源 / 编码 / 释义），
/// 位置语义不变。只认 `source == English`：短语 / 命令等同名项不是这个词本身（命令顶到头部会
/// 把上屏原文变成执行动作），照旧被头部吃掉，口径同 [`wants_raw_candidate`]。
///
/// 占位后按词库来源记词频；没被占位的头部候选经 `english_head_dict_word` 找回词库词记账。
pub(crate) fn merge_head_with_dict(
    head: Vec<Candidate>,
    mut dict: Vec<Candidate>,
) -> Vec<Candidate> {
    if head.is_empty() {
        return dict;
    }
    let mut out = Vec::with_capacity(head.len() + dict.len());
    for h in head {
        // 同名多条时（投影把 `asp` 改成 `ASP`，词库里又有真 `ASP`）先取没被改写的那条。
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
/// `Default` 档才跑逐位投影；另两档整条套形，不再按输入形态改写用户显式要的形态。
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
/// 单向规则：逐位比较公共前缀，输入位是大写则候选该位改大写，小写则原样不动；任一位忽略
/// 大小写不相等则整条放弃。超出输入长度的后缀保留词库原文。
///
/// 小写不投影：`China` / `iPhone` 的大写是词本身的一部分，小写只是默认击键形态，而 Shift 才是
/// 显式意图。不对应整条放弃：对不上的只会是短语 / 命令 / `$` 展开这类无前缀关系的条目，逐位
/// 跳过会产出杂合体。无大写时返回 `None`，让逐键热路径零分配。
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
/// 必须排在词频重排与候选调整之后：两者以 `text` 为键，先改写会让英文词频与置顶静默失效。
/// 被改写的候选把原文留在 [`Candidate::case_source`]，记账走 [`Candidate::freq_text`]。
///
/// 调用方须在之后再去重（投影会让 `hi` 变成 `Hi`，与头部原文撞车）。返回是否改写过任何一条，
/// 没改写时可省掉去重。
pub(crate) fn apply_english_case(cands: &mut [Candidate], raw: &str, variant: CaseVariant) -> bool {
    let mut changed = false;
    for c in cands.iter_mut() {
        // 先还原成词库原文，档位切换才可逆（单向规则下拿 `HILL` 投影回不到默认档），也让本函数幂等。
        if let Some(orig) = &c.case_source
            && c.text != *orig
        {
            // 还原本身也是一次改写。
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
            // 只在首次改写时记原文，保住的是原文而非上一档的形态。
            if c.case_source.is_none() {
                c.case_source = Some(std::mem::replace(&mut c.text, text));
            } else {
                c.text = text;
            }
        }
    }
    changed
}

/// 按精确文本去重，保序保留首条。大小写处理之后必须再跑一次，否则会出现两条一模一样的行。
///
/// 同名几条里有没被改写的（`case_source` 为空）时由它占那一格：词库同时有 `asp` 与 `ASP`、
/// 输入 `ASP` 时，投影出来的那条可能排在前面而吃掉真 `ASP`。占格保留那一格的 `natural_order`。
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

/// 这条候选是不是英文头部候选（所打原文 / 它的大小写变形）：无来源、无码、非短语 /
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

/// 「按一个完整词库码精确查词」的取数上限：头部候选找对应词库词与英文词频召回验证共用。
///
/// 同码条目只是同一个词的几种大小写写法，精确码命中排在前缀补全之前，8 条足够。它只管返回几条，
/// 码表引擎内部按 `max(limit, 50)` 取数再截断，调小它省不了查询本身。
pub(crate) const ENGLISH_EXACT_LOOKUP_LIMIT: usize = 8;

impl crate::coordinator::Coordinator {
    /// 头部候选（所打原文 / 大小写变形）对应的英文词库词，记词频用。
    ///
    /// 没被词库词占位的头部候选（如原文 `Translation`、打 `usa` 对应词库 `USA`）没有词库来源，
    /// 不找回则「打全再上屏」这最常见的用法一条都不记。找回规则见 [`Self::english_exact_dict_word`]；
    /// 返回的 `text` 换成实际上屏的头部文本、词库原文留在 `case_source`（同大小写投影的表示法）。
    /// 临英的上屏历史不经本函数，由 `record_temp_english_selection` 直接记。
    ///
    /// 找不到（不是词库词）返回 `None`，调用方不记词频：记下来是读端永远查不中的孤儿键
    /// （`temp_english_literal_text_is_not_recorded`）。
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
        // 不记词频就不查。判据是写端的 `records()` 而非 `enabled`，否则「仅记录次数」
        // （`record_always`）在打全词按空格时一条都不记。
        if !self.engine_mgr.freq_settings_for(engine_schema).records() {
            return None;
        }
        let mut word = self.english_exact_dict_word(engine_schema, buf, head_text)?;
        if word.text != cand.text {
            word.case_source = Some(std::mem::replace(&mut word.text, cand.text.clone()));
        }
        Some(word)
    }

    /// 按英文词库的键规则找 `head_text` 对应的词库词：码 = 小写化的缓冲，在精确码命中里先找
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
#[path = "english_candidates_tests.rs"]
mod tests;
