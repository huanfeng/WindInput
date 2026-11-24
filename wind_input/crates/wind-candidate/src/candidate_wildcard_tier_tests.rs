use super::*;

fn ct(code: &str, exact: bool, wildcard: bool) -> Candidate {
    Candidate {
        text: "蒸".into(),
        code: code.into(),
        is_exact_code: exact,
        is_wildcard: wildcard,
        is_common: true,
        source: CandidateSource::CodeTable,
        ..Default::default()
    }
}

/// spec §10：通配等长结果与码表精确同档。`freq_tier` 以本函数为首要键，
/// 档 2 的话开调频时拼音精确档（1）会整体压过它。
/// 对照：非通配候选只认 `code == input`，`is_exact_code` 不单独提档。
#[test]
fn wildcard_equal_length_shares_tier_with_codetable_exact() {
    assert_eq!(
        source_tier(&ct("abi", true, true), "azi"),
        0,
        "通配等长 = 档 0"
    );
    assert_eq!(
        source_tier(&ct("abic", false, true), "azi"),
        2,
        "通配更长 = 码表前缀档"
    );
    assert_eq!(
        source_tier(&ct("azi", true, false), "azi"),
        0,
        "对照：字面精确"
    );
    assert_eq!(
        source_tier(&ct("abi", true, false), "azi"),
        2,
        "对照：非通配候选不因 is_exact_code 提档"
    );
}

/// 锁（实现前即绿）：混输显示序「通配等长 → 拼音精确 → 通配更长」。
/// 等长靠 `cmp_exact_first` 领先，更长补全靠 `source_tier` 落在拼音精确之后。
#[test]
fn mixed_display_order_wildcard_equal_then_pinyin_exact_then_longer() {
    let equal = Candidate {
        weight: 1,
        ..ct("abi", true, true)
    };
    let longer = Candidate {
        text: "蒸笼".into(),
        weight: 9000,
        ..ct("abic", false, true)
    };
    let py = Candidate {
        text: "阿紫".into(),
        code: "azi".into(),
        weight: 169,
        is_common: true,
        consumed_length: 3,
        source: CandidateSource::Pinyin,
        ..Default::default()
    };
    let mut v = [longer, py, equal];
    v.sort_by(|a, b| candidate_display_order(a, b, false, true, "azi"));
    let order: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(order, ["蒸", "阿紫", "蒸笼"]);
}

fn dd(text: &str, exact: bool, weight: i32, disabled: bool) -> Candidate {
    Candidate {
        text: text.into(),
        code: "uuia".into(),
        source: CandidateSource::CodeTable,
        is_exact_code: exact,
        is_wildcard: true,
        weight,
        from_disabled_dict: disabled,
        ..Default::default()
    }
}

/// reverse-mode spec §4.2：同一精确档内已启用的排前，未启用库的排后——
/// 不论权重；但不跨档（未启用的等长仍先于已启用的更长补全）。
#[test]
fn disabled_dict_sorts_after_enabled_within_exact_tier() {
    let mut v = [
        dd("影等长", true, 9999, true),
        dd("启更长", false, 9999, false),
        dd("启等长", true, 1, false),
    ];
    v.sort_by(|a, b| candidate_display_order(a, b, false, false, "uuiz"));
    let got: Vec<&str> = v.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(got, ["启等长", "影等长", "启更长"]);
}

/// 两条都不是未启用库候选时该键恒 Equal ⇒ 既有次序逐条不变。
#[test]
fn disabled_flag_never_reorders_enabled_candidates() {
    let a = dd("甲", true, 10, false);
    let b = dd("乙", true, 20, false);
    assert_eq!(
        a.from_disabled_dict.cmp(&b.from_disabled_dict),
        std::cmp::Ordering::Equal
    );
    let mut v = [a, b];
    v.sort_by(|x, y| candidate_display_order(x, y, false, false, "uuiz"));
    assert_eq!(v[0].text, "乙", "仍按权重");
}
