use super::*;

#[test]
fn splits_the_gdi_face_names_from_the_report() {
    assert_eq!(
        split_weight_suffix("思源宋体 SemiBold"),
        Some(("思源宋体", 600))
    );
    assert_eq!(
        split_weight_suffix("霞鹜文楷 Medium"),
        Some(("霞鹜文楷", 500))
    );
    assert_eq!(
        split_weight_suffix("Source Han Serif SC ExtraLight"),
        Some(("Source Han Serif SC", 200))
    );
}

#[test]
fn two_word_weights_and_separators_are_recognised() {
    assert_eq!(split_weight_suffix("Foo Semi Bold"), Some(("Foo", 600)));
    assert_eq!(split_weight_suffix("Foo Extra-Light"), Some(("Foo", 200)));
    assert_eq!(split_weight_suffix("  Foo   light  "), Some(("Foo", 300)));
    assert_eq!(
        split_weight_suffix("方正书宋 粗体"),
        Some(("方正书宋", 700))
    );
}

#[test]
fn names_without_a_weight_suffix_are_left_alone() {
    assert_eq!(split_weight_suffix("霞鹜文楷"), None);
    assert_eq!(split_weight_suffix("Microsoft YaHei UI"), None);
    assert_eq!(split_weight_suffix("Bold"), None, "单个字重词不是字体名");
    assert_eq!(split_weight_suffix(""), None);
    // 字重词在中间不算后缀。
    assert_eq!(split_weight_suffix("Foo Bold Serif"), None);
    // 粘连的不拆：「SemiBoldX」不是字重词。
    assert_eq!(split_weight_suffix("Foo SemiBoldX"), None);
}

#[test]
fn localized_name_prefers_zh_cn_then_en_us() {
    let n = |v: &[(&str, &str)]| -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    };
    assert_eq!(
        pick_localized_name(&n(&[
            ("en-us", "Source Han Serif SC"),
            ("zh-CN", "思源宋体")
        ])),
        Some("思源宋体".into())
    );
    assert_eq!(
        pick_localized_name(&n(&[("ja-jp", "源ノ明朝"), ("en-us", "Source Han Serif")])),
        Some("Source Han Serif".into())
    );
    assert_eq!(
        pick_localized_name(&n(&[("ja-jp", "源ノ明朝")])),
        Some("源ノ明朝".into())
    );
    assert_eq!(pick_localized_name(&[]), None);
}

#[test]
fn name_comparison_ignores_ascii_case_and_padding() {
    assert!(font_name_eq(" Consolas ", "consolas"));
    assert!(!font_name_eq("Consolas", "Consola"));
}
