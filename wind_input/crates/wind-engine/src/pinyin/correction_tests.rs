use super::*;
use crate::pinyin::fuzzy::VALID_SYLLABLES;
use wind_config::{CorrectionConfig, CorrectionRuleToggles};

fn rules(f: impl FnOnce(&mut CorrectionConfig)) -> CorrectionRules {
    let mut c = CorrectionConfig::default();
    f(&mut c);
    CorrectionRules::from_config(&c)
}

#[test]
fn gn_ng_fixes_ogn() {
    assert_eq!(
        rules(|c| c.rules.gn_ng = true).corrections_of("gogn"),
        ["gong"]
    );
}
#[test]
fn gn_ng_fixes_ign() {
    assert_eq!(
        rules(|c| c.rules.gn_ng = true).corrections_of("dign"),
        ["ding"]
    );
}
#[test]
fn on_ong_fixes_on() {
    assert_eq!(rules(|_| {}).corrections_of("gon"), ["gong"]);
}
#[test]
fn vowel_swap_fixes_ia_after_h() {
    assert_eq!(rules(|_| {}).corrections_of("hia"), ["hai"]);
}
#[test]
fn legacy_off_by_default() {
    assert!(rules(|_| {}).corrections_of("liou").is_empty());
}
#[test]
fn legacy_on_fixes_liou() {
    assert_eq!(
        rules(|c| c.rules.legacy_spelling = true).corrections_of("liou"),
        ["liu"]
    );
}
#[test]
fn disabled_yields_nothing() {
    assert!(rules(|c| c.enabled = false).is_empty());
}
#[test]
fn never_registers_legal_syllable() {
    let r = rules(|c| {
        c.rules = CorrectionRuleToggles {
            gn_ng: true,
            on_ong: true,
            vowel_swap: true,
            legacy_spelling: true,
            h_misplaced: true,
            mg_ng: true,
            n_before_g: true,
            ai_ia: true,
        }
    });
    for s in r.spellings() {
        assert!(!VALID_SYLLABLES.contains(s), "{s}");
        assert!(!s.starts_with(['a', 'e', 'o']));
    }
}
#[test]
fn custom_rule_applies_alone() {
    let r = rules(|c| {
        c.rules.gn_ng = false;
        c.custom = vec!["ogn=ong".into()];
    });
    assert_eq!(r.corrections_of("gogn"), ["gong"]);
    assert!(r.corrections_of("dign").is_empty());
}
/// 声母限定照 §5.2 表：`ai←ia` 只对 w/g/h/k，`ei←ie` 只对 j/q/x，`ie←ei` 只对 w/f/g/h/k/z。
#[test]
fn vowel_swap_respects_initial_restrictions() {
    let r = rules(|_| {});
    assert_eq!(r.corrections_of("kia"), ["kai"]);
    assert!(
        r.corrections_of("bia").is_empty(),
        "b 不在 ai←ia 的声母表里"
    );
    assert_eq!(r.corrections_of("jei"), ["jie"]);
    assert_eq!(r.corrections_of("zie"), ["zei"]);
    assert!(r.corrections_of("bie").is_empty(), "bie 合法");
    assert_eq!(r.corrections_of("yuo"), ["you"]);
    assert!(r.corrections_of("duo").is_empty(), "duo 合法");
    assert_eq!(r.corrections_of("boa"), ["bao"]);
    assert_eq!(r.corrections_of("xioa"), ["xiao"]);
    assert_eq!(r.corrections_of("xoia"), ["xiao"]);
    assert_eq!(r.corrections_of("guia"), Vec::<String>::new());
    assert_eq!(r.corrections_of("gaui"), ["guai"]);
    assert_eq!(r.corrections_of("giu"), ["gui"]);
    assert_eq!(r.corrections_of("jui"), ["jiu"]);
    assert_eq!(r.wrong_spellings_of("xiao"), ["xioa", "xoia"]);
}

/// 出厂关的规则各自开了才生效。
#[test]
fn off_by_default_rules_need_their_toggle() {
    let base = rules(|_| {});
    for (typed, correct, on) in [
        (
            "hzong",
            "zhong",
            (|c: &mut CorrectionConfig| c.rules.h_misplaced = true) as fn(&mut CorrectionConfig),
        ),
        ("zih", "zhi", |c| c.rules.h_misplaced = true),
        ("bimg", "bing", |c| c.rules.mg_ng = true),
        ("bnag", "bang", |c| c.rules.n_before_g = true),
        ("xai", "xia", |c| c.rules.ai_ia = true),
        ("dnu", "dun", |c| c.rules.ai_ia = true),
        ("duei", "dui", |c| c.rules.legacy_spelling = true),
    ] {
        assert!(base.corrections_of(typed).is_empty(), "{typed}");
        assert_eq!(rules(on).corrections_of(typed), [correct], "{typed}");
    }
}

/// 生成不出可用错误拼写的自定义规则整条跳过，不影响其余规则。
#[test]
fn custom_rule_without_output_is_skipped() {
    let r = rules(|c| {
        c.custom = vec!["xyz=qqq".into(), "dui=diu".into(), "bad".into()];
    });
    assert_eq!(r.spellings().count(), rules(|_| {}).spellings().count());
}

/// 冲突 = 错误写法能切成合法音节或简拼（设计 §2.1）。
#[test]
fn conflict_example_splits_wrong_spelling() {
    assert_eq!(conflict_example("tuen").as_deref(), Some("tuen = tu+en"));
    assert_eq!(conflict_example("xign").as_deref(), Some("xign = xi+g+n"));
    assert_eq!(conflict_example("nage").as_deref(), Some("nage = na+ge"));
    assert_eq!(conflict_example("hzong").as_deref(), Some("hzong = h+zong"));
    assert_eq!(conflict_example("xai").as_deref(), Some("xai = x+ai"));
}

#[test]
fn conflict_example_none_when_unsplittable() {
    assert_eq!(conflict_example("gogn"), None);
    assert_eq!(conflict_example("hia"), None);
    assert_eq!(conflict_example("boa"), None, "单个元音不算一段");
    assert_eq!(conflict_example("tu"), None, "整串就是一个音节，不是冲突");
    assert_eq!(conflict_example(""), None);
}

/// 自定义规则：取它生成的错误拼写里第一个有冲突的；无效规则给原因。
#[test]
fn custom_rule_conflict_reports_first_example() {
    let ex = custom_rule_conflict("uen=un").unwrap().unwrap();
    assert!(ex.ends_with("u+en"), "{ex}");
    assert_eq!(
        custom_rule_conflict("ogn=ong").unwrap().as_deref(),
        Some("logn = lo+g+n")
    );
    assert_eq!(custom_rule_conflict("ioa=iao"), Ok(None));
    assert!(custom_rule_conflict("bad").is_err());
    assert!(custom_rule_conflict("dui=diu").is_err(), "生成不出可用拼写");
}

#[test]
fn custom_rule_invalid_is_rejected() {
    assert!(parse_custom("ogn").is_err());
    assert!(parse_custom("=ong").is_err());
}
