use super::*;

#[test]
fn default_matches_go_dictgen() {
    // 与 WindInput-Go tools/dictgen/config.go defaultConfig() 逐项对齐
    let c = Config::default();
    assert_eq!(c.target_median, 1000);
    assert_eq!(c.weight_max, 9999);
    assert_eq!(c.weight_min, 1);
    assert!((c.char_boost_factor - 1.3).abs() < f64::EPSILON);
    assert_eq!(c.fallback.priority_30, 180);
    assert_eq!(c.fallback.priority_20, 150);
    assert_eq!(c.fallback.priority_10, 120);
    assert_eq!(c.regular_weight_max, 8999);
    assert_eq!(c.shortcodes.level1_weight, 9999);
    assert_eq!(c.shortcodes.level2_base_weight, 9950);
    assert_eq!(c.shortcodes.level3_base_weight, 9000);
    assert_eq!(c.demotion.filter_threshold, 200);
    assert_eq!(c.filter.max_code_len, 4);
    assert_eq!(c.filter.max_text_len, 16);
    assert!(!c.filter.drop_pua, "五笔字根字属合法生僻字，默认不过滤");
}

#[test]
fn regular_max_capped_by_shortcodes() {
    let c = Config::default();
    assert_eq!(
        c.regular_max(),
        8999,
        "启用简码时普通词条上限被压到 regular_weight_max"
    );

    let c2 = Config {
        shortcodes: ShortcodeConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(c2.regular_max(), 9999, "关闭简码分层则放开到 weight_max");
}

#[test]
fn rejects_regular_max_overlapping_shortcode_band() {
    let c = Config {
        jidian_path: "a".into(),
        unigram_path: "b".into(),
        output_path: "c".into(),
        regular_weight_max: 9000, // == level3_base_weight
        ..Default::default()
    };
    let err = c.validate().unwrap_err().to_string();
    assert!(
        err.contains("level3_base_weight"),
        "应指出与简码区间重叠: {err}"
    );
}
