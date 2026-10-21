//! 带样式文字 → 绘制用的颜色区间（设计 `docs/design/text-span-colors.md` §6.4）。
//!
//! 候选窗注释、自绘气泡、macOS 气泡三处共用：求色一律走 `wind_theme::span_color`。

use crate::text::dwrite::ColorRun;
use wind_theme::{Resolved, Rgba, RvNode, TextState, span_color};
use wind_ui_types::StyledText;

/// 按当前主题把 `text` 的片段解析成颜色区间，**丢掉颜色等于正文色的区间**。
///
/// 丢弃是「零变化」的构造保证：主题不配角色、模板不含内联色 ⇒ 每个片段都解析到正文色 ⇒
/// 全被丢弃 ⇒ 叶子不带颜色区间，走与分段着色之前**同一条**绘制路径（`draw` 而非
/// `draw_runs`），View 树逐字段相同。零回归因此是结构性质，golden 对拍只是确认。
/// （首发时出厂主题即如此；2026-09-28 起 `_base` 配了角色色，设计 §18。）
pub(crate) fn color_runs(
    theme: &Resolved,
    node: &RvNode,
    is_tooltip: bool,
    state: TextState,
    body_fallback: Rgba,
    text: &StyledText,
) -> Vec<ColorRun> {
    if text.spans().is_empty() {
        return Vec::new();
    }
    let body = wind_theme::body_color(node, state, body_fallback);
    text.spans()
        .iter()
        .filter_map(|s| {
            let rgba = span_color(
                theme,
                node,
                is_tooltip,
                state,
                body_fallback,
                s.role,
                s.in_title,
                s.separator,
                s.color.as_deref(),
            );
            (rgba != body).then_some(ColorRun {
                start: s.start,
                end: s.end,
                rgba,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wind_ui_types::SpanStyle;

    const BODY: Rgba = [150, 150, 150, 255];

    fn styled() -> StyledText {
        let mut t = StyledText::new();
        t.push(
            "kao",
            &SpanStyle {
                role: Some("code_hint"),
                ..Default::default()
            },
        );
        t.push(
            "(",
            &SpanStyle {
                role: Some("literal"),
                ..Default::default()
            },
        );
        t.push(
            "x",
            &SpanStyle {
                role: Some("pinyin"),
                color: Some(std::sync::Arc::new(wind_theme::InlineColor::parse(
                    "#FF0000",
                ))),
                ..Default::default()
            },
        );
        t
    }

    /// 没配角色、只有内联色那段被保留；解析到正文色的全部丢弃。
    #[test]
    fn keeps_only_runs_that_differ_from_body() {
        let theme = Resolved::default();
        let runs = color_runs(
            &theme,
            &RvNode::default(),
            false,
            TextState::Normal,
            BODY,
            &styled(),
        );
        assert_eq!(
            runs,
            vec![ColorRun {
                start: 4,
                end: 5,
                rgba: [255, 0, 0, 255]
            }]
        );
    }

    /// 不配角色的主题 × 无色模板（有角色、无内联色）一段都不留。
    #[test]
    fn unroled_theme_yields_no_runs() {
        let mut t = StyledText::new();
        t.push(
            "kao",
            &SpanStyle {
                role: Some("code_hint"),
                ..Default::default()
            },
        );
        let runs = color_runs(
            &Resolved::default(),
            &RvNode::default(),
            false,
            TextState::Selected,
            BODY,
            &t,
        );
        assert!(runs.is_empty());
    }

    /// 分隔符标记透传到求色：配了 separator 单独着色，没配与变量同色（相邻两段同色仍各占一段）。
    #[test]
    fn separator_flag_reaches_span_color() {
        const RED: Rgba = [255, 0, 0, 255];
        const BLUE: Rgba = [0, 0, 255, 255];
        let var = SpanStyle {
            role: Some("chaizi_all"),
            ..Default::default()
        };
        let sep = SpanStyle {
            separator: true,
            ..var.clone()
        };
        let mut t = StyledText::new();
        t.push("ab", &var);
        t.push("/", &sep);
        t.push("c", &var);
        let runs_with = |roles: &[(&str, Rgba)]| {
            let node = RvNode {
                roles: roles.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                ..Default::default()
            };
            color_runs(
                &Resolved::default(),
                &node,
                false,
                TextState::Normal,
                BODY,
                &t,
            )
            .into_iter()
            .map(|r| (r.start, r.end, r.rgba))
            .collect::<Vec<_>>()
        };
        assert_eq!(
            runs_with(&[("chaizi_all", RED), ("separator", BLUE)]),
            vec![(0, 2, RED), (2, 3, BLUE), (3, 4, RED)]
        );
        assert_eq!(
            runs_with(&[("chaizi_all", RED)]),
            vec![(0, 2, RED), (2, 3, RED), (3, 4, RED)]
        );
    }
}
