//! `show_as = "tail"` 的落点：常用候选之后、生僻与放宽补充之前。
use super::emoji_tail_insert_at;
use wind_candidate::Candidate;

fn common(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        is_common: true,
        ..Default::default()
    }
}
fn rare(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        ..Default::default()
    }
}
fn relaxed(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        is_scope_filtered: true,
        ..Default::default()
    }
}

/// 智能档下孤儿码位的生僻字留在尾部，emoji 必须插在它们前面。
#[test]
fn lands_after_last_common_before_rare() {
    let v = [common("开心"), common("开新"), rare("闓"), rare("鐦")];
    assert_eq!(emoji_tail_insert_at(&v), 2);
}

/// 放宽补充段永远在 emoji 之后。
#[test]
fn never_crosses_relaxed_block() {
    let v = [common("开心"), relaxed("闓")];
    assert_eq!(emoji_tail_insert_at(&v), 1);
    // 补充段里即便混着常用字（放宽时整批并回），也不算数。
    let mut c = relaxed("开新");
    c.is_common = true;
    let v = [common("开心"), rare("闓"), c];
    assert_eq!(emoji_tail_insert_at(&v), 1);
}

/// 没有任何常用候选：落到末尾，不抢首位。
#[test]
fn all_rare_appends_at_end_not_front() {
    let v = [rare("闓"), rare("鐦")];
    assert_eq!(emoji_tail_insert_at(&v), 2);
    let v = [rare("闓"), relaxed("鐦")];
    assert_eq!(emoji_tail_insert_at(&v), 1);
}

/// 短语 / 命令与常用字同级（与过滤器的「常用词类」判据一致）。
#[test]
fn phrase_counts_as_common() {
    let mut p = rare("今天");
    p.is_phrase = true;
    let v = [common("开心"), p, rare("闓")];
    assert_eq!(emoji_tail_insert_at(&v), 2);
}

#[test]
fn empty_list() {
    assert_eq!(emoji_tail_insert_at(&[]), 0);
}
