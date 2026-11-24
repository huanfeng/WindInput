//! [`steals_coda`] 的三种形态：只有**简拼段**抢前一音节的韵尾才算，全拼零声母节点不算。

use super::*;

fn node(start: usize, syllables: &[&str]) -> LatticeNode {
    let mut mask = 0u64;
    let mut off = 0;
    for s in syllables {
        mask |= 1 << off;
        off += s.len();
    }
    LatticeNode {
        start,
        end: start + off,
        word: String::new(),
        syllables: syllables.iter().map(|s| s.to_string()).collect(),
        syl_mask: mask,
        log_prob: 0.0,
        canon: None,
    }
}

#[test]
fn consonant_abbrev_segment_steals() {
    // `ningbr` 读成 ni + n|g：`nin` 是音节。
    assert!(steals_coda(
        "ningbr",
        &SyllableTrie::new(),
        &node(2, &["n", "g"])
    ));
}

#[test]
fn vowel_abbrev_segment_steals() {
    // `yuedzh` 读成 yu + 耳朵(e|d)：`yue` 是音节。
    assert!(steals_coda(
        "yuedzh",
        &SyllableTrie::new(),
        &node(2, &["e", "d"])
    ));
}

#[test]
fn full_pinyin_zero_initial_node_does_not_steal() {
    // `ji` + 阿姨(a|yi)：`jia` 虽是音节，但「阿」是零声母全拼音节，不是简拼声母。
    assert!(!steals_coda(
        "jiayi",
        &SyllableTrie::new(),
        &node(2, &["a", "yi"])
    ));
}
