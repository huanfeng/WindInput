use super::*;

fn codes() -> CharCodes {
    // 取码规则验证用的最小表
    let mut m = CharCodes::new();
    m.insert('人', "w".into()); // 一级简码，仅 1 码
    m.insert('工', "aaa".into());
    m.insert('智', "tdkj".into());
    m.insert('能', "cexx".into());
    m.insert('中', "khk".into());
    m
}

#[test]
fn two_char_takes_first_two_of_each() {
    let c = codes();
    assert_eq!(encode_phrase("智能", &c), Some("tdce".into()));
}

#[test]
fn three_char_is_one_one_two() {
    let c = codes();
    assert_eq!(encode_phrase("工智能", &c), Some("atce".into()));
}

#[test]
fn four_plus_uses_last_char_not_fourth() {
    let c = codes();
    // 5 字词：前三字首码 + **末字**首码（不是第 4 字）
    assert_eq!(encode_phrase("人工智能中", &c), Some("watk".into()));
}

#[test]
fn short_code_is_padded_with_l() {
    let c = codes();
    // 「人」只有 1 码 w，2 字规则要 2 码 → 补 l
    assert_eq!(encode_phrase("人人", &c), Some("wlwl".into()));
}

#[test]
fn missing_char_yields_none_rather_than_wrong_code() {
    let c = codes();
    assert_eq!(
        encode_phrase("智鑫", &c),
        None,
        "缺字须整体失败，不得产出错码"
    );
}

#[test]
fn single_char_returns_its_own_code() {
    assert_eq!(encode_phrase("中", &codes()), Some("khk".into()));
    assert_eq!(encode_phrase("", &codes()), None);
}

#[test]
fn char_map_prefers_highest_weight_code() {
    let entries = vec![
        Entry::new("中".into(), "khk".into(), 10, 0),
        Entry::new("中".into(), "k".into(), 30, 1), // 权重更高 → 首选
        Entry::new("中文".into(), "khyy".into(), 30, 2), // 词组不入表
    ];
    let m = build_char_code_map(&entries);
    assert_eq!(m.get(&'中'), Some(&"k".to_string()));
    assert_eq!(m.len(), 1, "只收单字");
}
