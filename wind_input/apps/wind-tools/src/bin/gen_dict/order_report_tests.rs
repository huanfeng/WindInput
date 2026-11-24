use super::*;

fn cfg() -> Config {
    Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        ..Default::default()
    }
}

fn e(text: &str, code: &str, weight: i64, pos: usize) -> Entry {
    Entry::new(text.into(), code.into(), weight, pos)
}

fn empty() -> BTreeSet<(String, String)> {
    BTreeSet::new()
}

/// ★ 本报告存在的理由：上游用优先级明确表过态，补权把它翻了过来。
/// 现场取自实际产物 `tgyn`（上游 重启 20 > 生词 10，补权后 生词 1497 > 重启 1479）。
#[test]
fn flags_upstream_priority_being_overturned() {
    let upstream = vec![e("重启", "tgyn", 20, 0), e("生词", "tgyn", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let produced = vec![e("生词", "tgyn", 1497, 1), e("重启", "tgyn", 1479, 0)];
    let (changes, sum) = diff(
        &snap,
        &produced,
        &Unigram::new(),
        &cfg(),
        &empty(),
        &empty(),
    );
    assert_eq!(changes.len(), 1);
    assert!(changes[0].top_changed);
    assert!(changes[0].upstream_had_opinion, "20 vs 10 = 上游表过态");
    assert_eq!(changes[0].cause, "词频补权");
    assert_eq!(sum.top_changed_against_upstream, 1);
}

/// 上游整组同为最低档时它没表态，换首选不该被计入「违逆上游」。
/// 这条区分是整份报告的判读基准——混在一起数，1659 这个数字就没有意义。
#[test]
fn same_priority_group_is_not_counted_as_against_upstream() {
    let upstream = vec![e("甲", "abcd", 10, 0), e("乙", "abcd", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let produced = vec![e("乙", "abcd", 900, 1), e("甲", "abcd", 500, 0)];
    let (changes, sum) = diff(
        &snap,
        &produced,
        &Unigram::new(),
        &cfg(),
        &empty(),
        &empty(),
    );
    assert_eq!(sum.top_changed, 1);
    assert_eq!(sum.top_changed_against_upstream, 0);
    assert!(!changes[0].upstream_had_opinion);
}

#[test]
fn unchanged_order_produces_no_row() {
    let upstream = vec![e("甲", "abcd", 30, 0), e("乙", "abcd", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let produced = vec![e("甲", "abcd", 900, 0), e("乙", "abcd", 500, 1)];
    let (changes, sum) = diff(
        &snap,
        &produced,
        &Unigram::new(),
        &cfg(),
        &empty(),
        &empty(),
    );
    assert!(changes.is_empty());
    assert_eq!(sum.comparable, 1);
    assert_eq!(sum.order_changed, 0);
}

/// 成因取自实际动作而非权重形状：降权后权重恰好是「第二候选 -1」，
/// 但词频补权也能凑出同样的差值，靠形状反推会把两者混为一谈。
#[test]
fn cause_comes_from_observed_action_not_weight_shape() {
    let upstream = vec![e("中", "khkg", 30, 0), e("口中", "khkg", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let produced = vec![e("口中", "khkg", 4500, 1), e("中", "khkg", 4499, 0)];

    let (no_hint, _) = diff(
        &snap,
        &produced,
        &Unigram::new(),
        &cfg(),
        &empty(),
        &empty(),
    );
    assert_eq!(no_hint[0].cause, "词频补权", "没有降权记录就不得报成降权");

    let demoted: BTreeSet<(String, String)> = [("khkg".to_string(), "中".to_string())]
        .into_iter()
        .collect();
    let (with_hint, _) = diff(
        &snap,
        &produced,
        &Unigram::new(),
        &cfg(),
        &demoted,
        &empty(),
    );
    assert_eq!(with_hint[0].cause, "简码降权");
}

/// 新增词条顶掉上游首选：它不在上游快照里，优先级记 -1 并单列成因。
#[test]
fn custom_word_taking_top_is_labelled() {
    let upstream = vec![e("甲", "abcd", 30, 0), e("乙", "abcd", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let produced = vec![
        e("新词", "abcd", 5000, 9),
        e("甲", "abcd", 900, 0),
        e("乙", "abcd", 500, 1),
    ];
    let (changes, _) = diff(
        &snap,
        &produced,
        &Unigram::new(),
        &cfg(),
        &empty(),
        &empty(),
    );
    assert_eq!(changes[0].cause, "自定义新增词");
    assert_eq!(changes[0].gen_top_priority, -1);
}

/// 上游首选被过滤掉时首选必然换人，但那不是赋权造成的，要与词频翻转分开。
#[test]
fn filtered_top_is_its_own_cause() {
    let upstream = vec![
        e("弃", "abcd", 30, 0),
        e("甲", "abcd", 10, 1),
        e("乙", "abcd", 10, 2),
    ];
    let snap = Snapshot::capture(&upstream);
    let produced = vec![e("甲", "abcd", 900, 1), e("乙", "abcd", 500, 2)];
    let (changes, _) = diff(
        &snap,
        &produced,
        &Unigram::new(),
        &cfg(),
        &empty(),
        &empty(),
    );
    assert_eq!(changes[0].cause, "上游首选被过滤");
}

/// ★ 保护码换首选未必是缺陷：上游整组同为最低档时，`apply_protected_codes` 本就
/// 用词频拆并列。现场取自实际产物 `qqqq`（上游 狗狗/金 同为 10，产物「金」在前）。
/// 只有上游用不同优先级表过态却仍被翻转，才是保护失效。
#[test]
fn protected_code_tie_break_is_not_an_anomaly() {
    let upstream = vec![e("狗狗", "qqqq", 10, 0), e("金", "qqqq", 10, 1)];
    let snap = Snapshot::capture(&upstream);
    let produced = vec![e("金", "qqqq", 8020, 1), e("狗狗", "qqqq", 8010, 0)];
    let (changes, _) = diff(
        &snap,
        &produced,
        &Unigram::new(),
        &cfg(),
        &empty(),
        &empty(),
    );
    assert_eq!(changes[0].cause, "保护码并列裁决");

    // 上游表过态却被翻 = 保护确实失效，必须叫得出名字
    let upstream2 = vec![e("又", "cccc", 40, 0), e("双双", "cccc", 30, 1)];
    let snap2 = Snapshot::capture(&upstream2);
    let produced2 = vec![e("双双", "cccc", 1319, 1), e("又", "cccc", 1318, 0)];
    let (changes2, _) = diff(
        &snap2,
        &produced2,
        &Unigram::new(),
        &cfg(),
        &empty(),
        &empty(),
    );
    assert_eq!(changes2[0].cause, "保护码异常");
}

/// 单候选码没有「顺序」可言，不该占据报告篇幅。
#[test]
fn single_candidate_code_is_skipped() {
    let upstream = vec![e("甲", "abcd", 10, 0)];
    let snap = Snapshot::capture(&upstream);
    let produced = vec![e("甲", "abcd", 900, 0)];
    let (changes, sum) = diff(
        &snap,
        &produced,
        &Unigram::new(),
        &cfg(),
        &empty(),
        &empty(),
    );
    assert!(changes.is_empty());
    assert_eq!(sum.comparable, 0);
}

/// 快照必须复刻上游 `sort: by_weight` 的显示序：优先级降序，并列时文件序。
#[test]
fn snapshot_reproduces_upstream_display_order() {
    let upstream = vec![
        e("丙", "abcd", 10, 0),
        e("甲", "abcd", 30, 1),
        e("乙", "abcd", 10, 2),
    ];
    let snap = Snapshot::capture(&upstream);
    let order: Vec<&str> = snap.groups["abcd"]
        .iter()
        .map(|(t, _)| t.as_str())
        .collect();
    assert_eq!(order, ["甲", "丙", "乙"], "并列时保持文件序");
}
