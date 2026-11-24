//! 整句标记是验证 S2（用户词进整句词图）的主要抓手：整句只出一条、赢者通吃，
//! 光看候选文本分不清「用户词赢了」与「系统词本来就长这样」。

use super::*;

fn cand(f: impl FnOnce(&mut Candidate)) -> Candidate {
    let mut c = Candidate {
        text: "有盖伦吗".into(),
        ..Default::default()
    };
    f(&mut c);
    c
}

#[test]
fn non_sentence_has_no_tag() {
    assert_eq!(sentence_debug_tag(&cand(|_| {})), None);
    // 只有 is_synthesized 而没有 is_sentence 不构成整句（理论上不该出现，
    // 但标记判据必须以 is_sentence 为准，不能靠另两个反推）。
    assert_eq!(sentence_debug_tag(&cand(|c| c.is_synthesized = true)), None);
}

#[test]
fn synthesized_and_dictionary_sentences_are_distinguishable() {
    // 引擎新拼出来的：词库里没有「有盖伦吗」这个词条。
    assert_eq!(
        sentence_debug_tag(&cand(|c| {
            c.is_sentence = true;
            c.is_synthesized = true;
        }))
        .as_deref(),
        Some("整句(合成)")
    );
    // 同文合并：词库本就有这个词，被补标了整句身份。
    assert_eq!(
        sentence_debug_tag(&cand(|c| c.is_sentence = true)).as_deref(),
        Some("整句(词典)")
    );
}

#[test]
fn demotion_is_visible() {
    assert_eq!(
        sentence_debug_tag(&cand(|c| {
            c.is_sentence = true;
            c.is_synthesized = true;
            c.is_sentence_demoted = true;
        }))
        .as_deref(),
        Some("整句(合成·已降位)"),
        "降位是排序决策、不清 is_sentence，调试时必须看得见"
    );
}

#[test]
fn nbest_rank_shows_only_for_alternatives() {
    let tag = |rank: u8| {
        sentence_debug_tag(&cand(|c| {
            c.is_sentence = true;
            c.is_synthesized = true;
            c.sentence_rank = rank;
        }))
    };
    // 名次 1 = 最优解，与出厂单条整句同一标记。
    assert_eq!(tag(1).as_deref(), Some("整句(合成)"));
    assert_eq!(tag(3).as_deref(), Some("整句#3(合成)"));
}
