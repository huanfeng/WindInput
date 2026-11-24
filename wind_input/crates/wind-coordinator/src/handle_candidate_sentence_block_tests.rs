//! 整句 N-best 块的摆位：`sentence_count > 1` 时按名次连续摆到最前，其余不动。
use super::*;

fn c(text: &str, rank: u8) -> Candidate {
    Candidate {
        text: text.into(),
        sentence_rank: rank,
        is_sentence: rank > 0,
        ..Default::default()
    }
}

fn texts(v: &[Candidate]) -> Vec<&str> {
    v.iter().map(|x| x.text.as_str()).collect()
}

/// 词频重排把整句块打散之后，摆位把它们按名次收拢到最前。
#[test]
fn scattered_sentences_are_gathered_to_the_front_in_rank_order() {
    let mut v = vec![
        c("有", 0),
        c("有概论吗", 2),
        c("游", 0),
        c("有该论吗", 1),
        c("又", 0),
        c("有盖伦吗", 3),
    ];
    place_sentence_block(&mut v, 3);
    assert_eq!(
        texts(&v),
        vec!["有该论吗", "有概论吗", "有盖伦吗", "有", "游", "又"]
    );
}

/// 非整句候选的相对次序必须原样保留，摆位只搬整句那几条。
#[test]
fn non_sentence_order_is_untouched() {
    let mut v = vec![c("乙", 0), c("整2", 2), c("甲", 0), c("整1", 1), c("丙", 0)];
    place_sentence_block(&mut v, 2);
    assert_eq!(&texts(&v)[2..], &["乙", "甲", "丙"]);
}

/// 出厂 `show == 1`：什么都不动。整句靠 weight 挣位置那一套逐位不变。
#[test]
fn show_one_is_a_no_op() {
    let mut v = vec![c("有", 0), c("有该论吗", 1), c("有概论吗", 2)];
    let before: Vec<String> = texts(&v).iter().map(|s| s.to_string()).collect();
    place_sentence_block(&mut v, 1);
    assert_eq!(texts(&v), before);
}

/// 块里只有一条带名次的整句 ⇒ 不摆：混输把拼音子引擎钉成 1/1，没有这道会把拼音整句拉到
/// 码表精确候选前面。
#[test]
fn a_single_ranked_sentence_is_not_a_block() {
    let mut v = vec![c("工作", 0), c("啊啊我", 1)];
    place_sentence_block(&mut v, 3);
    assert_eq!(texts(&v), vec!["工作", "啊啊我"], "单条不该越过码表候选");
}
