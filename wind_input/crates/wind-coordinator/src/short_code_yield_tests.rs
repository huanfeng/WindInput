use super::*;

fn cand(text: &str, source: CandidateSource) -> Candidate {
    Candidate {
        text: text.to_string(),
        source,
        ..Default::default()
    }
}

fn ct(text: &str) -> Candidate {
    cand(text, CandidateSource::CodeTable)
}

fn tops_of(pairs: &[(usize, &str, &str)]) -> ShortcodeTops {
    let mut t: ShortcodeTops = [const { None }; MAX_LEVEL];
    for &(lv, code, top) in pairs {
        t[lv - 1] = Some((code.to_string(), top.to_string()));
    }
    t
}

fn texts(c: &[Candidate]) -> Vec<&str> {
    c.iter().map(|c| c.text.as_str()).collect()
}

#[test]
fn yields_full_code_top_to_the_first_word() {
    let mut c = vec![ct("路"), ct("路上"), ct("路口")];
    assert!(apply(
        &mut c,
        "khtk",
        &tops_of(&[(3, "kht", "路")]),
        3,
        false
    ));
    assert_eq!(texts(&c), ["路上", "路口", "路"], "字沉到本码所有候选之后");
}

/// **候选调整优先**：与 `yields_full_code_top_to_the_first_word` 逐字同参，只把
/// `user_pinned` 翻成 true —— 两条必须合看，单看任何一条都证明不了让路判据接进来了。
///
/// 顺序必须**纹丝不动**（不只是「首选还是字」）：让位的两步 rotate 会把接位词之后的
/// 候选各前移一位，只断言首选的话，那半个失效抓不到。
#[test]
fn user_pinned_stops_the_yield_entirely() {
    let mut c = vec![ct("路"), ct("路上"), ct("路口")];
    assert!(
        !apply(&mut c, "khtk", &tops_of(&[(3, "kht", "路")]), 3, true),
        "用户调过这个码的顺序，让位须整码停手"
    );
    assert_eq!(texts(&c), ["路", "路上", "路口"], "顺序须纹丝不动");
}

/// 让位的字要沉到**所有**同码候选之后，包括没有简码的全码字——那些字只能在这个
/// 码位打出来，有简码的字不该挡着它们。其余候选相对次序不变。
#[test]
fn the_yielding_char_sinks_below_other_full_code_chars() {
    let mut c = vec![ct("路"), ct("昤"), ct("路上"), ct("路口")];
    assert!(apply(
        &mut c,
        "khtk",
        &tops_of(&[(3, "kht", "路")]),
        3,
        false
    ));
    assert_eq!(texts(&c), ["路上", "昤", "路口", "路"]);
}

/// 只沉底而不提词，首选会变成生僻字——两步缺一不可。
#[test]
fn word_is_promoted_even_when_a_rare_char_sits_between() {
    let mut c = vec![ct("路"), ct("昤"), ct("路上")];
    assert!(apply(
        &mut c,
        "khtk",
        &tops_of(&[(3, "kht", "路")]),
        3,
        false
    ));
    assert_eq!(
        c.first().map(|c| c.text.as_str()),
        Some("路上"),
        "首选须是词，不能是被沉底顺带顶上来的生僻字"
    );
    assert_eq!(texts(&c), ["路上", "昤", "路"]);
}

/// 最隐蔽的一条：二简位已是首选 ⇒ 三简位会让位 ⇒ 三简位首选不再是该字。
/// 判定只看最近一级就会在全码位判成「无简码」，让位链自我拆台。
#[test]
fn scans_all_levels_so_chained_yield_does_not_erase_its_own_premise() {
    // kht 已经让位给词，故三简那级记的是「路上」而不是「路」。
    let tops = tops_of(&[(2, "kh", "路"), (3, "kht", "路上")]);
    let mut c = vec![ct("路"), ct("路口")];
    assert!(
        apply(&mut c, "khtk", &tops, 3, false),
        "二简位的记录仍在，全码位应当照样让位"
    );
    assert_eq!(texts(&c), ["路口", "路"]);
}

#[test]
fn holds_when_no_record_for_this_char() {
    // 走过临时拼音等旁路时该级缺记录 —— 保守不让位。
    let mut c = vec![ct("路"), ct("路上")];
    assert!(!apply(&mut c, "khtk", &tops_of(&[]), 3, false));
    assert_eq!(texts(&c), ["路", "路上"]);
}

/// 记录的码必须是当前输入的前缀。光标中间改码时缓冲长度不变，只有前缀校验抓得住。
#[test]
fn holds_when_record_belongs_to_a_different_code() {
    let mut c = vec![ct("路"), ct("路上")];
    // 记录来自 kht，当前输入却是 kxtk。
    assert!(!apply(
        &mut c,
        "kxtk",
        &tops_of(&[(3, "kht", "路")]),
        3,
        false
    ));
    assert_eq!(texts(&c), ["路", "路上"]);
}

#[test]
fn holds_when_nobody_can_take_over() {
    // 同码只有单字，没有词可以接位。
    let mut c = vec![ct("路"), ct("昤")];
    assert!(!apply(
        &mut c,
        "khtk",
        &tops_of(&[(3, "kht", "路")]),
        3,
        false
    ));
    assert_eq!(texts(&c), ["路", "昤"]);
}

/// 档位边界：level=2 时三简字不参与让位。
#[test]
fn level_two_ignores_third_level_shortcodes() {
    let tops = tops_of(&[(3, "kht", "路")]);
    let mut c = vec![ct("路"), ct("路上")];
    assert!(
        !apply(&mut c, "khtk", &tops, 2, false),
        "三简记录在档位 2 下不算数"
    );
    // 同一份候选，档位 3 就让。
    assert!(apply(&mut c, "khtk", &tops, 3, false));
}

#[test]
fn level_two_still_yields_for_a_second_level_shortcode() {
    let mut c = vec![ct("大"), ct("大厦")];
    assert!(apply(
        &mut c,
        "dddd",
        &tops_of(&[(2, "dd", "大")]),
        2,
        false
    ));
    assert_eq!(texts(&c), ["大厦", "大"]);
}

#[test]
fn level_zero_is_a_full_stop() {
    let mut c = vec![ct("路"), ct("路上")];
    assert!(!apply(
        &mut c,
        "khtk",
        &tops_of(&[(3, "kht", "路")]),
        0,
        false
    ));
    assert_eq!(texts(&c), ["路", "路上"]);
}

/// 简码位自身不让位——`kht` 在档位 3 下是简码位，不是「更长的码」。
#[test]
fn shortcode_position_itself_never_yields() {
    let mut c = vec![ct("路"), ct("路上")];
    assert!(!apply(
        &mut c,
        "kht",
        &tops_of(&[(2, "kh", "路")]),
        3,
        false
    ));
    assert_eq!(texts(&c), ["路", "路上"]);
}

/// 让位只在码表来源内做：拼音候选的次序归 `source_tier` 管。
#[test]
fn does_not_touch_non_codetable_candidates() {
    let mut c = vec![cand("路", CandidateSource::Pinyin), ct("路上")];
    assert!(
        !apply(&mut c, "khtk", &tops_of(&[(3, "kht", "路")]), 3, false),
        "首选不是码表来源时不让位"
    );
    // 反过来，接位者也必须是码表来源。
    let mut c2 = vec![ct("路"), cand("路上", CandidateSource::Pinyin)];
    assert!(!apply(
        &mut c2,
        "khtk",
        &tops_of(&[(3, "kht", "路")]),
        3,
        false
    ));
    assert_eq!(texts(&c2), ["路", "路上"]);
}

/// 检索范围临时放宽补进来的候选沉底是硬约束，不能被提到首位。
#[test]
fn scope_relaxed_candidates_never_take_over() {
    let mut relaxed = ct("路上");
    relaxed.is_scope_filtered = true;
    let mut c = vec![ct("路"), relaxed];
    assert!(!apply(
        &mut c,
        "khtk",
        &tops_of(&[(3, "kht", "路")]),
        3,
        false
    ));
    assert_eq!(texts(&c), ["路", "路上"]);
}

/// 沉底的边界是**正常候选段**：让位的字沉到放宽候选之前，不能沉到整个列表最后。
/// 否则「放宽候选恒在末尾」这条硬约束就被这个功能破掉了。
#[test]
fn the_yielding_char_sinks_above_scope_relaxed_candidates() {
    let mut relaxed = ct("踟");
    relaxed.is_scope_filtered = true;
    let mut c = vec![ct("路"), ct("路上"), relaxed];
    assert!(apply(
        &mut c,
        "khtk",
        &tops_of(&[(3, "kht", "路")]),
        3,
        false
    ));
    assert_eq!(texts(&c), ["路上", "路", "踟"]);
}

#[test]
fn records_only_single_char_tops_within_shortcode_range() {
    let mut t: ShortcodeTops = [const { None }; MAX_LEVEL];
    record_top(&mut t, "kht", &[ct("路"), ct("路上")]);
    assert_eq!(t[2], Some(("kht".into(), "路".into())));
    // 首选是词 ⇒ 该级作废（不是保留旧值）。
    record_top(&mut t, "kht", &[ct("路上"), ct("路")]);
    assert_eq!(t[2], None);
    // 超出简码级别不记。
    record_top(&mut t, "khtk", &[ct("路")]);
    assert_eq!(t, [None, None, None]);
}

#[test]
fn evicts_records_that_are_no_longer_a_prefix() {
    let mut t = tops_of(&[(1, "k", "口"), (2, "kh", "路"), (3, "kht", "路")]);
    evict_stale(&mut t, "kxt");
    assert_eq!(t[0], Some(("k".into(), "口".into())), "k 仍是 kxt 的前缀");
    assert_eq!(t[1], None, "kh 不是 kxt 的前缀");
    assert_eq!(t[2], None);
    // 缓冲清空后重新输入，旧记录全部作废——不需要在任何 clear() 点接线。
    evict_stale(&mut t, "a");
    assert_eq!(t, [None, None, None]);
}
