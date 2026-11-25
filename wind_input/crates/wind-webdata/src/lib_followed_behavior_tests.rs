use super::*;

#[test]
fn followed_behavior_reads_global_values() {
    let mut cfg = wind_config::Config::default();
    cfg.ui.candidate.layout = "vertical".into();
    cfg.ui.font.family = "霞鹜文楷".into();
    cfg.ui.candidate.per_page = 9;
    cfg.schema.pinyin.aux_code.enabled = true;
    cfg.schema.pinyin.aux_code.max_phrase_len = 3;
    cfg.schema.pinyin.aux_code.direct = true;
    cfg.schema.pinyin.aux_code.continuous = false;
    let v = followed_behavior_of(&cfg, Some("主题字体".into()));
    assert_eq!(
        v,
        json!({
            "layout": "vertical",
            "fontFamily": "霞鹜文楷",
            "perPage": 9,
            "auxEnabled": true,
            "auxMaxPhraseLen": 3,
            "auxDirect": true,
            "auxContinuous": false,
        })
    );
}

/// 全局字体空 = 跟随主题 ⇒ 给主题的字体；主题也没给 ⇒ 空串（不是「默认」之类的占位词）。
#[test]
fn followed_behavior_font_falls_back_to_theme_then_empty() {
    let mut cfg = wind_config::Config::default();
    cfg.ui.font.family = String::new();
    let v = followed_behavior_of(&cfg, Some("主题字体".into()));
    assert_eq!(v["fontFamily"], "主题字体");
    let v = followed_behavior_of(&cfg, None);
    assert_eq!(v["fontFamily"], "");
}

/// 全局 layout 只认横竖两档：空串/未知值按运行时同一裁决落到横排。
#[test]
fn followed_behavior_layout_is_horizontal_or_vertical() {
    let mut cfg = wind_config::Config::default();
    cfg.ui.candidate.layout = String::new();
    assert_eq!(followed_behavior_of(&cfg, None)["layout"], "horizontal");
}

#[test]
fn followed_behavior_is_a_readonly_sidecar() {
    assert!(
        READONLY_SIDECAR_FIELDS.contains(&"followedBehavior"),
        "旁路字段漏登记：{READONLY_SIDECAR_FIELDS:?}"
    );
}
