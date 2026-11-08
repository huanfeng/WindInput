//! 带样式文字 → 绘制用的颜色区间（设计 `docs/design/text-span-colors.md` §6.4）。
//!
//! 候选窗注释、自绘气泡、macOS 气泡三处共用：求色一律走 `wind_theme::span_color`。

use crate::text::ColorRun;
use wind_theme::{Resolved, Rgba, RvNode, RvViews, TextState, span_color};
use wind_ui_types::StyledText;

/// 按当前主题把 `text` 的片段解析成颜色区间，**丢掉颜色等于正文色的区间**。
///
/// 丢弃是「零变化」的构造保证：主题不配角色、模板不含内联色 ⇒ 每个片段都解析到正文色 ⇒
/// 全被丢弃 ⇒ 叶子不带颜色区间，走与分段着色之前**同一条**绘制路径（`draw` 而非
/// `draw_runs`），View 树逐字段相同。零回归因此是结构性质，golden 对拍只是确认。
/// （首发时出厂主题即如此；2026-09-28 起 `_base` 配了角色色，设计 §18。）
pub fn color_runs(
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

/// 候选正文的颜色区间：只有模糊音提示段（`pinyin_hint`）着色，取色走
/// [`wind_theme::pinyin_hint_color`]；其余段按正文色、不建区间。与 [`color_runs`] 同样丢掉
/// 等于正文色的区间（不带提示的候选恒为空 ⇒ 绘制路径与改动前相同）。
pub fn pinyin_hint_runs(
    text_node: &RvNode,
    comment_node: &RvNode,
    state: TextState,
    text_fallback: Rgba,
    comment_fallback: Rgba,
    state_bg: Option<Rgba>,
    text: &StyledText,
) -> Vec<ColorRun> {
    let mut hints = text
        .spans()
        .iter()
        .filter(|s| s.role == Some(wind_theme::PINYIN_HINT_ROLE))
        .peekable();
    if hints.peek().is_none() {
        return Vec::new();
    }
    let body = wind_theme::body_color(text_node, state, text_fallback);
    let rgba = wind_theme::pinyin_hint_color(
        text_node,
        comment_node,
        state,
        text_fallback,
        comment_fallback,
        state_bg,
    );
    if rgba == body {
        return Vec::new();
    }
    hints
        .map(|s| ColorRun {
            start: s.start,
            end: s.end,
            rgba,
        })
        .collect()
}

/// 候选正文在 `state` 下背后的实际底色（逐层按 alpha 合成到窗口底，不透明），供
/// [`wind_theme::pinyin_hint_color`] 判对比度。各层取色与候选窗绘制同一口径：窗口底 →
/// `candidate_list` 底 → 候选项底（选中 / 悬停整体替换基态，未配走出厂兜底）→ 正文叶子底。
///
/// 任一层带背景图 / 渐变 / 覆盖图、或窗口底本身不透明度不足（透到桌面）时取不到可靠值，
/// 返回 `None`（提示回落该态注释色）。
pub fn candidate_backdrop(v: &RvViews, state: TextState) -> Option<Rgba> {
    fn patch(n: &RvNode, state: TextState) -> Option<&RvNode> {
        match state {
            TextState::Selected => n.selected.as_deref(),
            TextState::Hover => n.hover.as_deref(),
            TextState::Normal => None,
        }
    }
    let art = |n: &RvNode| n.bg_image.is_some() || n.bg_gradient.is_some() || !n.layers.is_empty();
    let state_art = |n: &RvNode| patch(n, state).is_some_and(art);
    if art(&v.window)
        || art(&v.candidate_list)
        || art(&v.item)
        || state_art(&v.item)
        || art(&v.text)
    {
        return None;
    }
    let window = v.window.bg_color.unwrap_or(wind_theme::fallback::WINDOW_BG);
    if window[3] != 255 {
        return None;
    }
    let item = match state {
        TextState::Selected => Some(state_bg_or(
            &v.item.selected,
            wind_theme::fallback::SELECTED_BG,
        )),
        TextState::Hover => Some(state_bg_or(&v.item.hover, wind_theme::fallback::HOVER_BG)),
        TextState::Normal => v.item.bg_color,
    };
    let text = patch(&v.text, state)
        .and_then(|n| n.bg_color)
        .or(v.text.bg_color);
    Some(
        [v.candidate_list.bg_color, item, text]
            .into_iter()
            .flatten()
            .fold(window, |under, top| wind_theme::composite_over(top, under)),
    )
}

/// 候选项状态底色：状态 patch 的底色，未配走出厂兜底（同候选窗的 `patch_bg`）。
fn state_bg_or(p: &Option<Box<RvNode>>, d: Rgba) -> Rgba {
    p.as_ref().and_then(|n| n.bg_color).unwrap_or(d)
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

    // ───────────────────── candidate_backdrop ─────────────────────

    const WHITE: Rgba = [255, 255, 255, 255];

    fn bg(c: Rgba) -> RvNode {
        RvNode {
            bg_color: Some(c),
            ..Default::default()
        }
    }

    fn patched(base: RvNode, selected: Option<RvNode>, hover: Option<RvNode>) -> RvNode {
        RvNode {
            selected: selected.map(Box::new),
            hover: hover.map(Box::new),
            ..base
        }
    }

    fn views(window: RvNode, list: RvNode, item: RvNode, text: RvNode) -> RvViews {
        RvViews {
            window,
            candidate_list: list,
            item,
            text,
            ..Default::default()
        }
    }

    /// 半透明各层按 窗口 → 列表 → 候选项 → 正文叶子 的顺序逐层叠上去，结果不透明。
    #[test]
    fn backdrop_composites_layers_in_order() {
        let (list, item, text) = ([0, 0, 255, 128], [255, 0, 0, 64], [0, 255, 0, 32]);
        let v = views(bg(WHITE), bg(list), bg(item), bg(text));
        let co = wind_theme::composite_over;
        let want = co(text, co(item, co(list, WHITE)));
        assert_eq!(candidate_backdrop(&v, TextState::Normal), Some(want));
        assert_eq!(want[3], 255);
        // 顺序真有影响：反着叠得到别的颜色，上式才钉得住层序。
        assert_ne!(want, co(list, co(item, co(text, WHITE))));
    }

    /// 窗口底未配走出厂白；常态候选项不配底色就不参与合成。
    #[test]
    fn backdrop_defaults_to_factory_window_bg() {
        let v = RvViews::default();
        assert_eq!(
            candidate_backdrop(&v, TextState::Normal),
            Some(wind_theme::fallback::WINDOW_BG)
        );
    }

    /// 窗口底半透明（透到桌面）：底色不可知。
    #[test]
    fn backdrop_none_when_window_translucent() {
        let v = views(
            bg([255, 255, 255, 254]),
            RvNode::default(),
            RvNode::default(),
            RvNode::default(),
        );
        assert_eq!(candidate_backdrop(&v, TextState::Normal), None);
        assert_eq!(candidate_backdrop(&v, TextState::Selected), None);
    }

    /// 任一层带渐变 / 背景图 / 覆盖图：底色不可知。
    #[test]
    fn backdrop_none_when_any_layer_has_art() {
        let gradient = || RvNode {
            bg_gradient: Some(wind_theme::RvGradient {
                kind: "linear".into(),
                angle: 0.0,
                stops: vec![(WHITE, 0.0), ([0, 0, 0, 255], 1.0)],
            }),
            ..Default::default()
        };
        let image = || RvNode {
            bg_image: Some(Default::default()),
            ..Default::default()
        };
        let layers = || RvNode {
            layers: vec![Default::default()],
            ..Default::default()
        };
        for (what, art) in [
            ("渐变", gradient()),
            ("背景图", image()),
            ("覆盖图", layers()),
        ] {
            let d = RvNode::default;
            for (at, v) in [
                ("窗口", views(art.clone(), d(), d(), d())),
                ("列表", views(d(), art.clone(), d(), d())),
                ("候选项", views(d(), d(), art.clone(), d())),
                ("正文", views(d(), d(), d(), art.clone())),
            ] {
                assert_eq!(
                    candidate_backdrop(&v, TextState::Normal),
                    None,
                    "{at}{what}"
                );
            }
        }
        // 候选项的状态 patch 带图：只影响该状态。
        let v = views(
            RvNode::default(),
            RvNode::default(),
            patched(RvNode::default(), Some(image()), None),
            RvNode::default(),
        );
        assert_eq!(candidate_backdrop(&v, TextState::Selected), None);
        assert!(candidate_backdrop(&v, TextState::Normal).is_some());
        assert!(candidate_backdrop(&v, TextState::Hover).is_some());
    }

    /// 选中 / 悬停态候选项不配底色时走出厂兜底（悬停 HOVER_BG、选中 SELECTED_BG），配了用配的。
    #[test]
    fn backdrop_state_item_bg_falls_back_to_factory() {
        use wind_theme::fallback::{HOVER_BG, SELECTED_BG};
        let plain = RvViews::default();
        assert_eq!(candidate_backdrop(&plain, TextState::Hover), Some(HOVER_BG));
        assert_eq!(
            candidate_backdrop(&plain, TextState::Selected),
            Some(SELECTED_BG)
        );
        const MINE: Rgba = [10, 20, 30, 255];
        let v = views(
            RvNode::default(),
            RvNode::default(),
            patched(bg([1, 2, 3, 255]), Some(bg(MINE)), Some(bg(MINE))),
            RvNode::default(),
        );
        assert_eq!(candidate_backdrop(&v, TextState::Hover), Some(MINE));
        assert_eq!(candidate_backdrop(&v, TextState::Selected), Some(MINE));
        assert_eq!(
            candidate_backdrop(&v, TextState::Normal),
            Some([1, 2, 3, 255])
        );
    }

    /// 正文叶子的底色叠在最上：状态 patch 的优先，未配回退基态的。
    #[test]
    fn backdrop_text_leaf_bg_on_top() {
        const BASE: Rgba = [0, 128, 0, 255];
        const SEL: Rgba = [128, 0, 0, 255];
        let v = views(
            RvNode::default(),
            RvNode::default(),
            RvNode::default(),
            patched(bg(BASE), Some(bg(SEL)), None),
        );
        assert_eq!(candidate_backdrop(&v, TextState::Normal), Some(BASE));
        assert_eq!(candidate_backdrop(&v, TextState::Selected), Some(SEL));
        assert_eq!(
            candidate_backdrop(&v, TextState::Hover),
            Some(BASE),
            "悬停 patch 没配正文底：沿用基态"
        );
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
