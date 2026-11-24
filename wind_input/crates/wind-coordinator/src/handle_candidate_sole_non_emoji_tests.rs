//! 短语自动上屏的唯一性判据：emoji 扩展候选不得计入。失效是静默的（只是不再自动上屏），
//! 用户无从联系到 emoji 功能，故由测试钉住。
use super::sole_non_emoji;
use wind_candidate::Candidate;

fn cand(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        is_phrase: true,
        ..Default::default()
    }
}

fn emoji(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        is_emoji_suggestion: true,
        ..Default::default()
    }
}

#[test]
fn single_real_candidate_is_sole() {
    let v = [cand("好的")];
    assert_eq!(sole_non_emoji(&v).map(|c| c.text.as_str()), Some("好的"));
}

/// 短语 + 追加的 emoji ⇒ 仍算「唯一」，自动上屏不受影响。
#[test]
fn appended_emoji_does_not_break_uniqueness() {
    let v = [cand("好的"), emoji("👌")];
    assert_eq!(sole_non_emoji(&v).map(|c| c.text.as_str()), Some("好的"));
    // 多个 emoji 同样不影响（max_per_word 可配到 3）。
    let v = [cand("好的"), emoji("👌"), emoji("🆗"), emoji("👍")];
    assert_eq!(sole_non_emoji(&v).map(|c| c.text.as_str()), Some("好的"));
}

/// 两条真候选仍判非唯一 —— 排除 emoji 不等于放宽原判据。
#[test]
fn two_real_candidates_are_not_sole() {
    let v = [cand("好的"), cand("好地"), emoji("👌")];
    assert!(sole_non_emoji(&v).is_none());
}

/// 只有 emoji 没有宿主候选 ⇒ 无候选可上屏。实际构造不出来，但判据不该依赖这个前提。
#[test]
fn emoji_only_is_not_sole() {
    let v = [emoji("👌")];
    assert!(sole_non_emoji(&v).is_none());
}

#[test]
fn empty_is_not_sole() {
    assert!(sole_non_emoji(&[]).is_none());
}
