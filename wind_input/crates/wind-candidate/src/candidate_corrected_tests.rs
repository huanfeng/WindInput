use super::*;

fn c(text: &str, corrected: bool) -> Candidate {
    Candidate {
        text: text.into(),
        is_corrected: corrected,
        ..Default::default()
    }
}

fn texts(cs: &[Candidate]) -> Vec<&str> {
    cs.iter().map(|c| c.text.as_str()).collect()
}

#[test]
fn duplicate_keeps_uncorrected() {
    let mut cs = vec![c("中", true), c("种", true), c("中", false), c("众", false)];
    drop_corrected_duplicates(&mut cs);
    assert_eq!(texts(&cs), ["种", "中", "众"]);
    assert!(!cs[1].is_corrected);
}

#[test]
fn complete_uncorrected_takes_top() {
    let mut abbrev = c("干呕", false);
    abbrev.is_abbrev = true;
    let mut partial = c("与", false);
    partial.is_partial = true;
    partial.consumed_length = 2;
    let mut cs = vec![
        c("有", true),
        abbrev,
        partial,
        c("又", true),
        c("与哦", false),
    ];
    protect_top_from_corrected(&mut cs, 3);
    assert_eq!(texts(&cs), ["与哦", "有", "干呕", "与", "又"]);
}

/// 误读召回同为低置信：首条是它时让给完整普通候选；完整候选里也不算它。
#[test]
fn misread_recall_does_not_take_top() {
    let recall = |t: &str| Candidate {
        text: t.into(),
        is_misread_recall: true,
        ..Default::default()
    };
    let mut cs = vec![recall("莘莘"), recall("堤防"), c("欣欣", false)];
    protect_top_from_corrected(&mut cs, 6);
    assert_eq!(texts(&cs), ["欣欣", "莘莘", "堤防"]);
}

#[test]
fn no_complete_keeps_corrected_top() {
    let mut abbrev = c("干呕功能", false);
    abbrev.is_abbrev = true;
    let mut prefix = c("公平", false);
    prefix.is_prefix = true;
    let mut short = c("公", false);
    short.consumed_length = 2;
    let mut cs = vec![c("工", true), abbrev, prefix, short];
    protect_top_from_corrected(&mut cs, 4);
    assert_eq!(texts(&cs), ["工", "干呕功能", "公平", "公"]);
}

#[test]
fn uncorrected_top_untouched() {
    let mut cs = vec![c("公平", false), c("工", true)];
    protect_top_from_corrected(&mut cs, 8);
    assert_eq!(texts(&cs), ["公平", "工"]);
}
