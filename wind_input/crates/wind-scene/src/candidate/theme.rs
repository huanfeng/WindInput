//! 候选窗的**生效主题**：协调器下发的原始主题叠上外观覆盖（横竖变体、用户字体 / 字重 / 字号、
//! 方案级字体、阴影开关）之后的那份。实时窗口与设置端预览都经 [`effective_theme`] 求，
//! 两边因此拿到同一份主题；字体名怎么解析（查系统字体集）由调用方以闭包注入。

/// 外观覆盖，字段即窗口上的同名设置。
#[derive(Clone, Copy, Debug, Default)]
pub struct ThemeOverrides<'a> {
    /// 竖排或旋转排版：主题写了 `[节点.vertical]` 时整套视图换成竖排变体。
    pub vertical: bool,
    /// 用户外观字体（`ui.font.family`）；空 = 跟随主题节点字族。
    pub user_font_family: &'a str,
    /// 用户全局字重（`ui.font.weight`）；0 = 不指定。
    pub user_font_weight: i32,
    /// 方案级 `[candidate] font_family`；空 = 不覆盖。
    pub scheme_text_family: &'a str,
    /// 是否采用主题阴影（`ui.candidate.shadow`）；false = 清掉窗口阴影。
    pub shadow: bool,
    /// 候选字号覆盖（`ui.candidate.font_size`，逻辑像素）；0 = 跟随主题。
    pub font_size: f32,
}

/// [`effective_theme`] 的结果。
pub struct EffectiveTheme {
    pub theme: wind_theme::Resolved,
    /// 生效的候选基准字号（逻辑像素）。
    pub base_font_logical: f32,
    /// 叠上用户字体之后、方案级字体之前各节点声明的字族（`(节点路径, 字族)`），给调用方做
    /// 存在性告警：被用户字体覆盖掉的主题字族不会被用到，报它缺字体只会误导；方案级覆盖有
    /// 自己的告警（来源名不同）。
    pub declared_families: Vec<(String, String)>,
}

/// 原始主题 + 外观覆盖 → 生效主题与生效字号。
///
/// `resolve_font` 把配置里的字体名换成渲染端认得的 `(family, 名字里带的字重)`（旧 GDI face name
/// → family + 字重；查不到原样返回、字重 0）。
pub fn effective_theme(
    src: &wind_theme::Resolved,
    o: &ThemeOverrides,
    resolve_font: impl Fn(&str) -> (String, i32),
) -> EffectiveTheme {
    let mut t = src.clone();
    // 竖排（含旋转排版）：主题写了 `[节点.vertical]` 时整套视图换成竖排变体。
    if o.vertical
        && let Some(v) = t.views_vertical.take()
    {
        t.views = v;
    }
    apply_user_font(&mut t.views, o.user_font_family);
    if !o.shadow {
        clear_window_shadow(&mut t.views);
    }
    let declared_families = t.views.declared_font_families();
    apply_scheme_text_font(&mut t.views, o.scheme_text_family);
    // 主题节点与方案级字族换成渲染端认得的名字（旧 GDI face name → family + 字重）。
    // 字重优先级：用户 ui.font.weight（非 0）是基准——主题普通字重让位于它、主题强调字重
    // （≥ 600 或比基态重，如选中 700）取 max(主题, 用户)——用户字重低于主题强调值时，
    // 强调仍比普通粗；用户未指定时节点显式字重 > 名字拆出的字重 > 常规。用户指定了字重时也
    // 丢弃名字里的字重，未写字重的节点保持 0 = 继承渲染器默认（即用户字重）。每次都从原始
    // 主题重算，不会污染它。
    let user_weight = o.user_font_weight;
    t.views.apply_user_font_weight(user_weight);
    t.views.resolve_font_families(|f| {
        let (family, weight) = resolve_font(f);
        (family, if user_weight > 0 { 0 } else { weight })
    });
    let base_font_logical = effective_base_font_size(o.font_size, t.behavior.font_size);
    EffectiveTheme {
        theme: t,
        base_font_logical,
        declared_families,
    }
}

/// 外观字体覆盖主题节点字族（R3「外观覆盖主题」）：用户 `ui.font.family` 非空时，主题
/// **全部**节点（含状态 patch）的 `font_family` 一律让位——清空后叶子回落渲染器的全局
/// 字族，即用户字体本身。空 = 不动，主题节点字族照常生效（与改动前一致）。
///
/// 完整优先级（候选文字节点）：方案级 `[candidate] font_family` > 用户 `ui.font.family`
/// > 主题节点 `font_family` > 内置默认 `DEFAULT_FONT_FAMILY`（wind-ui）；其余节点没有方案级那一档。
///
/// `ui.font.fallback` / `ui.font.scripts` **不必**按同一规则处理，主题节点字族本就压不住它们：
/// - scripts：`create_layout` 里按脚本切段的指派在叶子字族**之后**设置，恒胜过节点字族；
/// - fallback：默认链另挂了一条不按 base family 筛选的兜底映射（`ensure_fallback` 第 3 段），
///   base 是主题字族时照样按用户的链接续缺字；CoreText 的 cascade list 是字体级属性，同理。
///
/// 所以只有「主字族」这一格会被主题压住，要改的也只有这一格。
fn apply_user_font(views: &mut wind_theme::RvViews, user_family: &str) {
    if !user_family.trim().is_empty() {
        views.clear_font_families();
    }
}

/// 用户覆盖为无阴影：清掉窗口阴影的全部字段。`shadow_params` 因此得 None、`margins()` 为 0，
/// 窗口与内容等大，`window.show(px - ml, py - mt)` 的扩边补偿自然归零。
/// 主题的 `window_offset_*`（position_offset）刻意保留——它补偿的是发光边等装饰，与阴影无关。
fn clear_window_shadow(views: &mut wind_theme::RvViews) {
    views.shadow_offset_x = None;
    views.shadow_offset_y = None;
    views.shadow_blur = None;
    views.shadow_spread = None;
    views.shadow_spread_offset_x = None;
    views.shadow_spread_offset_y = None;
    views.shadow_color = None;
}

/// 方案级 `[candidate] font_family`（空 = 不覆盖）只作用于候选文字节点，理由见
/// wind-ui `CandidateWindow::text_family_override`。须在 [`apply_user_font`] 之后叠。
fn apply_scheme_text_font(views: &mut wind_theme::RvViews, scheme_family: &str) {
    let f = scheme_family.trim();
    if !f.is_empty() {
        views.text.font_family = Some(f.to_string());
    }
}

/// 生效候选基准字号（逻辑像素）：用户值 > 0 取用户值，否则主题 `behavior.font_size`
/// （`ui.candidate.font_size` 的 0 = 跟随主题）。
pub fn effective_base_font_size(user: f32, theme: i32) -> f32 {
    if user > 0.0 { user } else { theme as f32 }
}

#[cfg(test)]
mod tests {
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
}
