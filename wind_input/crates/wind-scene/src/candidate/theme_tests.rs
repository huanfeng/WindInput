use super::*;
use wind_theme::schema::Dim;

/// 竖排变体、方案级字体、阴影开关、字号覆盖一次叠齐；告警用的字族表取在方案级字体之前。
#[test]
fn overrides_compose_onto_source() {
    let mut src = wind_theme::Resolved::default();
    src.views.shadow_blur = Some(Dim::Dp(8.0));
    let mut vv = src.views.clone();
    vv.text.font_family = Some("Vert".into());
    src.views_vertical = Some(vv);
    let o = ThemeOverrides {
        vertical: true,
        scheme_text_family: " Scheme ",
        shadow: false,
        font_size: 18.0,
        ..Default::default()
    };
    let eff = effective_theme(&src, &o, |f| (format!("R:{f}"), 0));
    let v = &eff.theme.views;
    assert_eq!(v.text.font_family.as_deref(), Some("R:Scheme"));
    assert!(v.shadow_blur.is_none(), "关阴影清掉窗口阴影");
    assert!(eff.theme.views_vertical.is_none(), "变体已换入 views");
    assert_eq!(eff.base_font_logical, 18.0);
    let declared: Vec<&str> = eff
        .declared_families
        .iter()
        .map(|(_, f)| f.as_str())
        .collect();
    assert!(declared.contains(&"Vert"), "{declared:?}");
    assert!(
        !declared.iter().any(|f| f.contains("Scheme")),
        "{declared:?}"
    );
    // 原始主题不被改动。
    assert_eq!(src.views.shadow_blur, Some(Dim::Dp(8.0)));
}

#[test]
fn horizontal_keeps_main_views_and_theme_font_size() {
    let mut src = wind_theme::Resolved::default();
    let mut vv = src.views.clone();
    vv.text.font_family = Some("Vert".into());
    src.views_vertical = Some(vv);
    let o = ThemeOverrides {
        shadow: true,
        ..Default::default()
    };
    let eff = effective_theme(&src, &o, |f| (f.to_string(), 0));
    assert_ne!(eff.theme.views.text.font_family.as_deref(), Some("Vert"));
    assert_eq!(eff.base_font_logical, src.behavior.font_size as f32);
}
