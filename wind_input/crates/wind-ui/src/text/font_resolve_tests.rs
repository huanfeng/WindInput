use super::*;

fn sys(families: &'static [&'static str]) -> impl Fn(&str) -> Option<bool> {
    move |n: &str| Some(families.iter().any(|f| f.eq_ignore_ascii_case(n)))
}

fn no_face(_: &str) -> Option<(String, i32)> {
    None
}

#[test]
fn an_existing_family_is_never_rewritten() {
    // 「Arial Black」本身是 family：不能被拆成 Arial + 900。
    let r = resolve_font_name("Arial Black", sys(&["Arial", "Arial Black"]), no_face);
    assert_eq!(r.family, "Arial Black");
    assert_eq!(r.weight, 0);
    assert_eq!(r.source, FontNameSource::Exact);
}

#[test]
fn legacy_gdi_face_name_becomes_family_plus_weight() {
    let r = resolve_font_name("思源宋体 SemiBold", sys(&["思源宋体"]), no_face);
    assert_eq!(
        r,
        ResolvedFont {
            family: "思源宋体".into(),
            weight: 600,
            source: FontNameSource::WeightSuffix
        }
    );
}

#[test]
fn stripped_family_must_exist_or_face_lookup_takes_over() {
    // 剥出来的「文渊宋体」不在系统里 → 不许采用，交给 face 全名查找。
    let r = resolve_font_name("文渊宋体 Bold", sys(&["WenYuan Serif SC"]), |n| {
        (n == "文渊宋体 Bold").then(|| ("WenYuan Serif SC".to_string(), 700))
    });
    assert_eq!(r.family, "WenYuan Serif SC");
    assert_eq!(r.weight, 700);
    assert_eq!(r.source, FontNameSource::FaceName);
}

#[test]
fn a_name_nobody_knows_stays_missing_and_untouched() {
    let r = resolve_font_name(" 不存在 Medium ", sys(&["不存在的别的"]), no_face);
    assert_eq!(r.family, "不存在 Medium");
    assert_eq!(r.source, FontNameSource::Missing);
}

#[test]
fn without_a_font_collection_nothing_is_guessed() {
    let r = resolve_font_name(
        "思源宋体 SemiBold",
        |_| None,
        |_| panic!("查不了时不该去扫字体集"),
    );
    assert_eq!(r.family, "思源宋体 SemiBold");
    assert_eq!(r.source, FontNameSource::Unknown);
}
