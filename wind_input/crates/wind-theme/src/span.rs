//! 分段着色的求色（设计 `docs/design/text-span-colors.md` §4.2、§6）。
//!
//! 片段带的是「角色 / 颜色引用」而不是 RGBA：悬停态只有 UI 知道、明暗切换时 UI 拿手里的候选
//! 直接重绘，所以颜色必须在每次画的时候按当前主题现求。求色是纯函数、只依赖本 crate 的
//! 类型，候选窗、自绘气泡、macOS 气泡的 runs、设置页预览四处共用同一个口径。

use std::sync::Arc;

use crate::palette::{Rgba, parse_hex};
use crate::resolve::Resolved;
use crate::rvnode::RvNode;

/// 内联色 `$[SPEC]{…}` 的颜色部分：常态一份，可选的选中态一份。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InlineColor {
    pub normal: ColorRef,
    pub selected: Option<ColorRef>,
}

/// 一个颜色引用：亮 / 暗各一个原子（单值时两侧相同）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ColorRef {
    pub light: Atom,
    pub dark: Atom,
}

/// 颜色原子。十六进制在解析时就校验（坏值即 [`Atom::Invalid`]）；名字只能在求色时
/// 按当前主题查。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Atom {
    Rgba(Rgba),
    Name(Arc<str>),
    Invalid,
}

impl Atom {
    /// `ATOM := '#' HEX{3|6|8} | NAME`，`NAME = [a-z0-9_]+`。其余一律 [`Atom::Invalid`]。
    fn parse(s: &str) -> Self {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            // 恰好一个 `#`、其后全是十六进制位，才交给 parse_hex。不能直接喂它：它为兼容老主题
            // 会削掉全部前导 `#`、也认不带 `#` 的裸写，内联色要的是写法严格。
            let strict =
                matches!(hex.len(), 3 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit());
            return if strict {
                parse_hex(s).map_or(Atom::Invalid, Atom::Rgba)
            } else {
                Atom::Invalid
            };
        }
        let is_name = !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if is_name {
            Atom::Name(Arc::from(s))
        } else {
            Atom::Invalid
        }
    }
}

impl ColorRef {
    /// `COLOR := ATOM ( '/' ATOM )?`：单值 = 亮暗共用，`a/b` = 亮 / 暗。多于一个 `/` 即非法。
    fn parse(s: &str) -> Self {
        let mut parts = s.split('/');
        let light = Atom::parse(parts.next().unwrap_or(""));
        match (parts.next(), parts.next()) {
            (None, _) => Self {
                dark: light.clone(),
                light,
            },
            (Some(d), None) => Self {
                light,
                dark: Atom::parse(d),
            },
            (Some(_), Some(_)) => Self {
                light: Atom::Invalid,
                dark: Atom::Invalid,
            },
        }
    }
}

impl InlineColor {
    /// `SPEC := COLOR ( ',' KEY '=' COLOR )*`，记号两侧空白忽略。不失败：坏值落
    /// [`Atom::Invalid`]（该段按正文色显示，§4.5）；`KEY` 目前只认 `selected`，其余忽略——
    /// 前向兼容，将来加 `hover=` 时老版本不至于把整段判非法。
    pub fn parse(spec: &str) -> Self {
        let mut items = spec.split(',');
        let normal = ColorRef::parse(items.next().unwrap_or(""));
        let mut selected = None;
        for item in items {
            if let Some((k, v)) = item.split_once('=')
                && k.trim() == "selected"
            {
                selected = Some(ColorRef::parse(v));
            }
        }
        Self { normal, selected }
    }
}

/// 文字所处的状态。选中优先于悬停（与候选窗 `eff_text` 同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextState {
    Normal,
    Selected,
    Hover,
}

fn state_patch(node: &RvNode, state: TextState) -> Option<&RvNode> {
    match state {
        TextState::Normal => None,
        TextState::Selected => node.selected.as_deref(),
        TextState::Hover => node.hover.as_deref(),
    }
}

/// 节点在某状态下的正文色：状态 patch 的文字色，未给则回退基态色（不跨态借色），
/// 基态也没给则用渲染层兜底 `fallback`。候选窗的 `eff_text` 与本模块的求色共用它。
pub fn body_color(node: &RvNode, state: TextState, fallback: Rgba) -> Rgba {
    let base = node.text_color.unwrap_or(fallback);
    state_patch(node, state)
        .and_then(|n| n.text_color)
        .unwrap_or(base)
}

/// 按当前明暗解析一个颜色引用。`None` = 非法或当前主题查不到（调用方回落正文色）。
///
/// 气泡里的名字先查 `tooltip_<名>`、查不到再查同名：为白底调的亮档色放进深色气泡会看不清
/// （§5.4）。`transparent` 及任何 alpha 为 0 的结果都按查不到处理：占着宽度却看不见，只会是
/// 误用（§4.5）。
fn resolve_ref(theme: &Resolved, is_tooltip: bool, r: &ColorRef) -> Option<Rgba> {
    let c = match if theme.is_dark { &r.dark } else { &r.light } {
        Atom::Rgba(c) => Some(*c),
        Atom::Invalid => None,
        Atom::Name(n) if &**n == "transparent" => None,
        Atom::Name(n) => is_tooltip
            .then(|| theme.palette.get(&format!("tooltip_{n}")))
            .flatten()
            .or_else(|| theme.palette.get(&**n))
            .copied(),
    };
    c.filter(|c| c[3] != 0)
}

/// 一个片段该用的颜色（§6.2）。
///
/// - `node`：`views.comment` 或 `views.tooltip`；`is_tooltip` 决定内联色名字的作用域查找。
/// - `body_fallback`：节点未配正文色时的渲染层兜底（候选窗注释是 `[150,150,150,255]`，
///   气泡是 palette `tooltip_text`）。
/// - `role`：片段角色（归一后的变量名或 `title` / `literal`）；`None` = 无角色（未知变量回显）。
/// - `in_title`：段名里产出的片段，角色未配色时回落 `title` 角色。
/// - `separator`：变量值里的分项分隔符（`${chaizi_all:／}` 的 `／`）。②③ 两步先查 `separator`
///   角色、没配再查 `role`（所在变量）：主题不配 `separator` 时分隔符与变量同色。
///
/// 顺序：① 内联色（选中态有 `selected=` 用它；否则状态改了正文色就回落正文色；否则常态值）
/// → ② 该状态单列的角色色 → 规则 3（状态改了正文色 ⇒ 正文色）→ ③ 常态角色色 →
/// ④ 段名里的变量回落 `title` → ⑤ 正文色。
///
/// 「状态改了正文色」按有效色值比（该状态正文色 ≠ 常态正文色），不按主题写没写
/// `[comment.selected] color`：派生主题常把选中色写回常态色，按「写没写」判会在选中态
/// 无端丢掉全部分色（§6.3）。
#[allow(clippy::too_many_arguments)]
pub fn span_color(
    theme: &Resolved,
    node: &RvNode,
    is_tooltip: bool,
    state: TextState,
    body_fallback: Rgba,
    role: Option<&str>,
    in_title: bool,
    separator: bool,
    inline: Option<&InlineColor>,
) -> Rgba {
    let body = body_color(node, state, body_fallback);
    let state_changed_body =
        state != TextState::Normal && body != body_color(node, TextState::Normal, body_fallback);
    if let Some(ic) = inline {
        if state == TextState::Selected
            && let Some(sel) = &ic.selected
        {
            return resolve_ref(theme, is_tooltip, sel).unwrap_or(body);
        }
        if state_changed_body {
            return body;
        }
        return resolve_ref(theme, is_tooltip, &ic.normal).unwrap_or(body);
    }
    let chain = [separator.then_some("separator"), role];
    let pick = |n: &RvNode| {
        chain
            .iter()
            .flatten()
            .find_map(|r| n.roles.get(*r))
            .copied()
    };
    if let Some(c) = state_patch(node, state).and_then(pick) {
        return c;
    }
    if state_changed_body {
        return body;
    }
    if let Some(c) = pick(node) {
        return c;
    }
    if role.is_some()
        && in_title
        && let Some(c) = node.roles.get("title")
    {
        return *c;
    }
    body
}

/// 正确拼音提示段的角色名（与 wind-ui-types `TEXT_ROLES` 里的 `pinyin_hint` 同名）。
pub const PINYIN_HINT_ROLE: &str = "pinyin_hint";

/// 候选正文里正确拼音提示段（`(ping)`）的颜色，角色名 [`PINYIN_HINT_ROLE`]。
///
/// - `text`：`views.text`（提示段在候选正文里，角色色写在 `[text.roles]`）；`comment`：`views.comment`。
/// - `comment_fallback`：注释节点未配正文色时的渲染层兜底（候选窗的 `COMMENT_FALLBACK`）。
/// - `state_bg`：该状态下候选正文背后的实际底色（已与窗口底合成、不透明）；渐变 / 图片 /
///   透到桌面等取不到可靠值的情况传 `None`。常态不看它。
///
/// 顺序：① 该状态的 `text.<state>.roles` → ② 该状态改了正文色（有效色值比较，同 [`span_color`]
/// 规则 3）时，③ 的常态提示色对 `state_bg` 的对比度 ≥ [`PINYIN_HINT_MIN_CONTRAST`] 才沿用，
/// 否则（对比度不足、底色未知、或 ③ 没配）取该状态下的注释色 → ③ `text.roles`
/// → ④ 注释在该状态下的正文色。即主题不配时提示段与注释同色，含选中 / 悬停态。
/// 正文其余部分无角色，直接用 [`body_color`]`(text, state, 候选正文兜底)`。
pub fn pinyin_hint_color(
    text: &RvNode,
    comment: &RvNode,
    state: TextState,
    text_fallback: Rgba,
    comment_fallback: Rgba,
    state_bg: Option<Rgba>,
) -> Rgba {
    if let Some(c) = state_patch(text, state).and_then(|n| n.roles.get(PINYIN_HINT_ROLE)) {
        return *c;
    }
    let comment_color = body_color(comment, state, comment_fallback);
    let normal = text.roles.get(PINYIN_HINT_ROLE).copied();
    if state != TextState::Normal
        && body_color(text, state, text_fallback)
            != body_color(text, TextState::Normal, text_fallback)
    {
        return match (normal, state_bg) {
            (Some(c), Some(bg))
                if crate::palette::meets_contrast(c, bg, PINYIN_HINT_MIN_CONTRAST) =>
            {
                c
            }
            _ => comment_color,
        };
    }
    normal.unwrap_or(comment_color)
}

/// 高亮态沿用常态提示色所需的最低对比度（WCAG 非正文图形 / 大字口径）。
pub const PINYIN_HINT_MIN_CONTRAST: f64 = 3.0;

#[cfg(test)]
#[path = "span_tests.rs"]
mod tests;
