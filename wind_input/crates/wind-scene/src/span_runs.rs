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
#[path = "span_runs_tests.rs"]
mod tests;
