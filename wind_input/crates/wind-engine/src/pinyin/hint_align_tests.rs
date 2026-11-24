use super::*;
use crate::pinyin::correction::CorrectionRules;
use crate::pinyin::fuzzy::HintSwitch;
use std::sync::Arc;

fn align_hints(
    typed: &str,
    syllables: &[&str],
    cfg: &FuzzyConfig,
    ends_here: bool,
) -> Vec<PinyinHint> {
    align(typed, syllables, cfg, ends_here).hints
}

/// 出厂纠错规则 + `cfg()` 的模糊组。
fn corr_cfg() -> FuzzyConfig {
    let mut c = wind_config::CorrectionConfig::default();
    c.rules.gn_ng = true; // 出厂关；用例是 gognping
    FuzzyConfig {
        correction: Arc::new(CorrectionRules::from_config(&c)),
        ..cfg()
    }
}

#[test]
fn correction_segment_marks_corrected() {
    let a = align("gognping", &["gong", "ping"], &corr_cfg(), true);
    assert!(a.corrected);
    assert_eq!(
        a.hints,
        vec![PinyinHint {
            char_index: 0,
            correct: "gong".into(),
            kind: HintKind::Correction,
        }],
        "纠错段产纠错来源的提示"
    );
}

#[test]
fn correction_off_never_corrected() {
    assert!(!align("gognping", &["gong", "ping"], &cfg(), true).corrected);
}

#[test]
fn exact_not_corrected() {
    assert!(!align("gongping", &["gong", "ping"], &corr_cfg(), true).corrected);
}

#[test]
fn trailing_typing_prefix_not_corrected() {
    // `zhon` 既是 `zhong` 的错拼也是它没敲完的样子：在输入末尾按后者算。
    assert!(!align("zhon", &["zhong"], &corr_cfg(), true).corrected);
    assert!(align("zhonguo", &["zhong", "guo"], &corr_cfg(), true).corrected);
}

#[test]
fn fuzzy_wins_over_correction() {
    // 自定义 `in=ing` 让 `din` 同时是 `ding` 的纠错拼写与 in/ing 模糊变体：归模糊。
    let corr = wind_config::CorrectionConfig {
        custom: vec!["in=ing".into()],
        ..Default::default()
    };
    let c = FuzzyConfig {
        correction: Arc::new(CorrectionRules::from_config(&corr)),
        ..cfg()
    };
    assert!(!c.correction.corrections_of("din").is_empty());
    let a = align("din", &["ding"], &c, false);
    assert!(!a.corrected, "{a:?}");
    assert_eq!(a.hints, vec![h(0, "ding")]);
}

#[test]
fn corrected_computed_without_hint() {
    let mut c = corr_cfg();
    c.hint = HintSwitch::new(false);
    let a = align("gognping", &["gong", "ping"], &c, true);
    assert!(a.corrected && a.hints.is_empty());
}

fn cfg() -> FuzzyConfig {
    FuzzyConfig {
        in_ing: true,
        n_l: true,
        sh_s: true,
        hint: HintSwitch::new(true),
        ..Default::default()
    }
}

fn h(i: u16, s: &str) -> PinyinHint {
    PinyinHint {
        char_index: i,
        correct: s.into(),
        kind: HintKind::Fuzzy,
    }
}

#[test]
fn pinguo_marks_ping() {
    assert_eq!(
        align_hints("pinguo", &["ping", "guo"], &cfg(), true),
        vec![h(0, "ping")]
    );
}

#[test]
fn exact_marks_nothing() {
    assert!(align_hints("pingguo", &["ping", "guo"], &cfg(), true).is_empty());
}

#[test]
fn abbrev_marks_nothing() {
    assert!(align_hints("pg", &["ping", "guo"], &cfg(), true).is_empty());
}

#[test]
fn trailing_prefix_not_marked() {
    assert!(align_hints("pin", &["ping"], &cfg(), true).is_empty());
    assert_eq!(
        align_hints("pin", &["ping"], &cfg(), false),
        vec![h(0, "ping")]
    );
}

#[test]
fn lan_nan_marked() {
    assert_eq!(
        align_hints("lan", &["nan"], &cfg(), true),
        vec![h(0, "nan")]
    );
}

#[test]
fn half_last_syllable() {
    assert!(align_hints("pinggu", &["ping", "guo"], &cfg(), true).is_empty());
}

#[test]
fn sentence_mixed_abbrev() {
    let mut c = cfg();
    c.en_eng = true;
    assert_eq!(
        align_hints(
            "jintiantqzhenhao",
            &["jin", "tian", "tian", "qi", "zheng", "hao"],
            &c,
            true
        ),
        vec![h(4, "zheng")]
    );
}

#[test]
fn shared_key_not_marked() {
    let mut c = cfg();
    c.shared_key.insert("diu".into(), vec!["dui".into()]);
    assert!(align_hints("diu", &["dui"], &c, true).is_empty());
}

#[test]
fn illegal_spelling_marked() {
    let c = FuzzyConfig {
        in_ing: true,
        hint: HintSwitch::new(true),
        ..Default::default()
    };
    assert_eq!(align_hints("din", &["ding"], &c, false), vec![h(0, "ding")]);
}

#[test]
fn hint_off_returns_empty() {
    let mut c = cfg();
    c.hint = HintSwitch::new(false);
    assert!(align_hints("pinguo", &["ping", "guo"], &c, true).is_empty());
}

#[test]
fn fuzzy_prefix_marks_nothing() {
    assert!(align_hints("l", &["nan"], &cfg(), true).is_empty());
}

#[test]
fn zero_initial_final_fuzzy_marked() {
    let c = FuzzyConfig {
        en_eng: true,
        hint: HintSwitch::new(true),
        ..Default::default()
    };
    assert_eq!(align_hints("eng", &["en"], &c, false), vec![h(0, "en")]);
}

#[test]
fn nve_lve_mark_each_other() {
    assert_eq!(
        align_hints("nve", &["lve"], &cfg(), true),
        vec![h(0, "lve")]
    );
    assert_eq!(
        align_hints("lve", &["nve"], &cfg(), true),
        vec![h(0, "nve")]
    );
}

#[test]
fn misaligned_returns_empty() {
    assert!(align_hints("pinguo", &["ping"], &cfg(), true).is_empty());
}
