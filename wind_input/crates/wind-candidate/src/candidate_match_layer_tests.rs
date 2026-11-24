use super::*;
use std::cmp::Ordering;

fn cand(is_prefix: bool, is_partial: bool, is_promoted: bool) -> Candidate {
    Candidate {
        is_prefix,
        is_partial,
        is_promoted_completion: is_promoted,
        ..Default::default()
    }
}

/// `is_promoted_completion` 让「码更长的补全」在层级比较中等价于非补全（有效前缀层为 false）。
#[test]
fn promoted_completion_ranks_in_exact_layer() {
    let exact = cand(false, false, false); // 精确
    let plain_prefix = cand(true, false, false); // 普通前缀补全（沉底层）
    let promoted = cand(true, false, true); // 提升后的前缀补全

    // 普通补全排在精确之后。
    assert_eq!(cmp_match_layers(&exact, &plain_prefix), Ordering::Less);
    // 提升后的补全与精确同层（层级比较相等，交由后续权重决出）。
    assert_eq!(cmp_match_layers(&exact, &promoted), Ordering::Equal);
    // 提升后的补全排在普通补全之前。
    assert_eq!(cmp_match_layers(&promoted, &plain_prefix), Ordering::Less);
}

/// 提升只影响前缀层，不越过子短语维度：提升补全(is_partial=false)仍优先于子短语(is_partial=true)。
#[test]
fn promoted_completion_still_above_subphrase() {
    let promoted = cand(true, false, true);
    let subphrase = cand(false, true, false);
    assert_eq!(cmp_match_layers(&promoted, &subphrase), Ordering::Less);
}

/// **`is_fuzzy` 不得参与层级比较**（本次改动的核心不变量）。
///
/// 它是「召回来源」而非「匹配质量」：把它做成层级键等价于「惩罚 = ∞」，真实词库下会把
/// 模糊候选压到 200 名开外（`si` 下「是」第 231 位），而生产候选上限仅 50~300 ——
/// 模糊音因此在拼音 / 混输 / 临拼三条路径上全部等价于未实现。惩罚改由引擎在 weight 上
/// 施加（`wind_engine::pinyin::FUZZY_WEIGHT_SCALE`）。
///
/// 谁把 `is_fuzzy` 加回 `cmp_match_layers`，这条就会挂。
#[test]
fn fuzzy_is_not_a_layer() {
    let exact = Candidate::default();
    let fuzzy = Candidate {
        is_fuzzy: true,
        ..Default::default()
    };
    assert_eq!(
        cmp_match_layers(&exact, &fuzzy),
        Ordering::Equal,
        "模糊命中须与精确命中同层，由权重而非层级决出先后"
    );

    // 与其它层级维度组合时，也只由那些维度说了算。
    let fuzzy_exact = Candidate {
        is_fuzzy: true,
        ..cand(false, false, false)
    };
    let plain_prefix = cand(true, false, false);
    assert_eq!(
        cmp_match_layers(&fuzzy_exact, &plain_prefix),
        Ordering::Less,
        "模糊的精确匹配仍应优先于非模糊的前缀补全"
    );
}

/// 简拼整体沉到全拼之后。此前该语义由用户词简拼硬置 `is_fuzzy=true` 借位实现，
/// `is_fuzzy` 退出层级键后改由本字段承接——行为须与从前一致。
#[test]
fn abbrev_ranks_below_full_spelling() {
    let full = Candidate::default();
    let abbrev = Candidate {
        is_abbrev: true,
        ..Default::default()
    };
    assert_eq!(cmp_match_layers(&full, &abbrev), Ordering::Less);
    assert_eq!(cmp_match_layers(&abbrev, &full), Ordering::Greater);
}

/// 层级维度的优先级顺序：简拼 > 有效前缀层 > 子短语。
/// 前者为真即整体沉底，不因后者更优而被拉回。
#[test]
fn abbrev_outranks_other_layer_dimensions() {
    // 简拼但结构上是「精确匹配」；对手是普通前缀补全（更差的结构）。
    let abbrev_exact = Candidate {
        is_abbrev: true,
        ..cand(false, false, false)
    };
    let plain_prefix = cand(true, false, false);
    assert_eq!(
        cmp_match_layers(&abbrev_exact, &plain_prefix),
        Ordering::Greater,
        "简拼是首要键，即便其结构更优也须沉在前缀补全之后"
    );
}

/// 零权重读音的精确单字并入简拼层：与简拼词同层（由权重决出），整体沉在精确之后。
#[test]
fn zero_weight_reading_joins_abbrev_layer() {
    let exact = Candidate::default();
    let zero_weight = Candidate {
        is_zero_weight_reading: true,
        ..Default::default()
    };
    let abbrev = Candidate {
        is_abbrev: true,
        ..Default::default()
    };
    assert_eq!(cmp_match_layers(&zero_weight, &abbrev), Ordering::Equal);
    assert_eq!(cmp_match_layers(&exact, &zero_weight), Ordering::Less);
}
