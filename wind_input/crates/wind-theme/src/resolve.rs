//! 求值：typed `Theme` → `Resolved`（palette + RVNode 树 + resources + behavior）。
//!
//! 与 Go 版 `pkg/theme` 的 `ResolveV3`/`ResolveCandidateViews`/`resolveViewNode` 对齐，但按迁移决策精简：
//! - **不做 derive**（主题须显式给全语义色；见 docs/redesign/theme-migration-plan.md 决策 2）；
//! - **LightDark 解析期坍缩**：求值即 `select(is_dark)→单值`，RVNode 只存终值（决策 3）；
//! - **validate 降级为 warn**：未解析的 `${token}`/缺失 ref 记 `tracing::warn`，不 fail（决策 5）；
//!   配合 `Option` 兜底（缺默认=沿用基态/渲染器内置），外部坏主题不黑屏。

use crate::palette::{Rgba, parse_hex, resolve_palette};
use crate::rvnode::{
    DEFAULT_ACCENT_BAR_HEIGHT_RATIO, RvEdges, RvGradient, RvImage, RvNode, RvViews,
};
use crate::schema::{Ld, Theme, ViewGradient, ViewImage, ViewNode, Views};
use std::collections::HashMap;
use std::path::Path;

const TRANSPARENT: Rgba = [0, 0, 0, 0];

/// 解析后行为配置（引擎基线 ⊕ 主题 behavior）。
#[derive(Clone, Debug)]
pub struct ResolvedBehavior {
    pub font_size: i32,
    pub always_show_pager: bool,
    pub show_page_number: bool,
    pub hide_pager: bool,
    /// 竖排候选窗宽度上限，单位 dp。0=不限——渲染层另有一道恒定的屏幕安全钳制兜底，
    /// 不依赖本字段。
    pub vertical_max_width: i32,
    /// 独立翻页栏行水平对齐：left/center/right（默认 center）。
    pub pager_align: String,
}

impl Default for ResolvedBehavior {
    /// 引擎内置基线。
    ///
    /// `vertical_max_width` 出厂值为 0（不限）：固定宽度上限裁切候选文字时不加省略号
    /// （View 引擎不支持折行），改为不限后交给渲染层恒生效的屏幕安全钳制防越界，用户/主题若
    /// 仍要美观类宽度上限可显式配置 `behavior.vertical_max_width`（单位 dp）覆盖。
    fn default() -> Self {
        Self {
            font_size: 18,
            always_show_pager: false,
            show_page_number: true,
            hide_pager: false,
            vertical_max_width: 0,
            pager_align: String::from("center"),
        }
    }
}

/// v3 主题求值后的最终形态——渲染层唯一来源（T3 起 wind-ui 消费）。
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub is_dark: bool,
    /// 全部已解析颜色 token（顶层语义 + selection/hover + toolbar_* 等功能前缀）。
    pub palette: HashMap<String, Rgba>,
    pub views: RvViews,
    pub behavior: ResolvedBehavior,
    /// 图片资源：名→绝对路径 / data: URI（相对路径已按 theme 目录解析，按 is_dark 选变体）。
    pub resources: HashMap<String, String>,
    /// 资产搜索目录（self + base 链）：用于视图节点里**字面** image ref（如 _base 的 chevron.svg，
    /// 不在 resources 注册表）解析为绝对路径——派生主题继承 base 的图标需到 base 目录查找。
    pub asset_dirs: Vec<std::path::PathBuf>,
}

/// 颜色 token 求值（与 Go resolveColorToken 对齐）：
/// 空/None→None（保留默认）；transparent→全透明；`${name}`→palette 查（缺失 warn）；hex→直解。
fn resolve_color(ld: Option<&Ld>, palette: &HashMap<String, Rgba>, is_dark: bool) -> Option<Rgba> {
    let s = ld?.select(is_dark)?.trim();
    if s.is_empty() {
        return None;
    }
    if s == "transparent" {
        return Some(TRANSPARENT);
    }
    if let Some(name) = s.strip_prefix("${").and_then(|x| x.strip_suffix('}')) {
        let c = palette.get(name).copied();
        if c.is_none() {
            tracing::warn!("主题颜色引用未解析: ${{{}}}（token 不在 colors 表）", name);
        }
        return c;
    }
    match parse_hex(s) {
        Some(c) => Some(c),
        None => {
            tracing::warn!("主题颜色字面值非法: {:?}", s);
            None
        }
    }
}

/// schema ViewImage → 渲染消费 RVImage（不解码位图）。
fn to_rv_image(im: &ViewImage, palette: &HashMap<String, Rgba>, is_dark: bool) -> RvImage {
    let slice_px = |d: Option<crate::schema::Dim>| d.map(|x| x.resolve(1.0, 0.0)).unwrap_or(0.0);
    RvImage {
        reference: im.reference.clone(),
        mode: im.mode.clone(),
        // 九宫切片为源图纹理像素，不缩放：scale=1。
        slice: [
            slice_px(im.slice.top),
            slice_px(im.slice.right),
            slice_px(im.slice.bottom),
            slice_px(im.slice.left),
        ],
        slice_repeat: [
            im.slice_repeat.x.as_deref() == Some("repeat"),
            im.slice_repeat.y.as_deref() == Some("repeat"),
        ],
        opacity: im.opacity.unwrap_or(1.0),
        z: im.z,
        anchor: im.anchor.clone(),
        offset_x: im.offset.x,
        offset_y: im.offset.y,
        w: im.size.w,
        h: im.size.h,
        tint: resolve_color(im.tint.as_ref(), palette, is_dark),
        disabled_tint: resolve_color(im.disabled_tint.as_ref(), palette, is_dark),
    }
}

/// schema ViewGradient → RVGradient（stop 颜色解析 + 按 pos 升序；无有效 stop 返回 None）。
fn resolve_gradient(
    g: &ViewGradient,
    palette: &HashMap<String, Rgba>,
    is_dark: bool,
) -> Option<RvGradient> {
    let mut stops: Vec<(Rgba, f32)> = g
        .stops
        .iter()
        .filter_map(|s| {
            resolve_color(Some(&s.color), palette, is_dark).map(|c| (c, s.pos.clamp(0.0, 1.0)))
        })
        .collect();
    if stops.is_empty() {
        return None;
    }
    stops.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    Some(RvGradient {
        kind: if g.kind.is_empty() {
            "linear".into()
        } else {
            g.kind.clone()
        },
        angle: g.angle,
        stops,
    })
}

/// 通用 ViewNode → RVNode（窗口无关）。几何直拷（符号 Dim）；颜色 = 默认 ⊕ token 覆盖。
fn resolve_view_node(
    n: &ViewNode,
    palette: &HashMap<String, Rgba>,
    is_dark: bool,
    def_bg: Option<Rgba>,
    def_border: Option<Rgba>,
    def_text: Option<Rgba>,
) -> RvNode {
    let rc = |ld: &Option<Ld>| resolve_color(ld.as_ref(), palette, is_dark);

    let mut out = RvNode {
        margin: RvEdges {
            top: n.margin.top,
            right: n.margin.right,
            bottom: n.margin.bottom,
            left: n.margin.left,
        },
        padding: RvEdges {
            top: n.padding.top,
            right: n.padding.right,
            bottom: n.padding.bottom,
            left: n.padding.left,
        },
        border_radius: n.border.radius,
        border_width: n.border.width,
        border_color: def_border,
        border_style: n.border.style.clone(),
        bg_color: def_bg,
        bg_shape: n.background.shape.clone(),
        font_size: n.font_size.unwrap_or(0) as f32,
        font_weight: n.font_weight.unwrap_or(0),
        font_family: n.font_family.clone(),
        text_color: def_text,
        line_spacing: n.line_spacing,
        col_gap: n.col_gap,
        title_gap: n.title_gap,
        prev_char: n.prev_char.clone().unwrap_or_default(),
        next_char: n.next_char.clone().unwrap_or_default(),
        ..Default::default()
    };

    if let Some(c) = rc(&n.background.color) {
        out.bg_color = Some(c);
    }
    if let Some(c) = rc(&n.border.color) {
        out.border_color = Some(c);
    }
    if let Some(c) = rc(&n.color) {
        out.text_color = Some(c);
    }
    if let Some(img) = &n.background.image {
        out.bg_image = Some(to_rv_image(img, palette, is_dark));
    }
    if let Some(g) = &n.background.gradient {
        // resolve_gradient 对空 stops 返回 None，无需外层判空。
        out.bg_gradient = resolve_gradient(g, palette, is_dark);
    }
    if !n.layers.is_empty() {
        out.layers = n
            .layers
            .iter()
            .map(|l| to_rv_image(l, palette, is_dark))
            .collect();
    }
    if let Some(img) = &n.prev_image {
        out.prev_image = Some(to_rv_image(img, palette, is_dark));
    }
    if let Some(img) = &n.next_image {
        out.next_image = Some(to_rv_image(img, palette, is_dark));
    }
    if let Some(sh) = &n.shadow {
        out.shadow_offset_x = sh.offset_x;
        out.shadow_offset_y = sh.offset_y;
        out.shadow_blur = sh.blur;
        out.shadow_spread = sh.spread;
        out.shadow_spread_offset_x = sh.spread_offset_x;
        out.shadow_spread_offset_y = sh.spread_offset_y;
        out.shadow_color = rc(&sh.color);
    }
    out
}

/// 状态 patch ViewNode → 递归 RVNode（与 Go resolveState 对齐）。
/// nil-gating：仅当显式给了 bg/图/渐变/层/text/border 色/border 宽/字重，或有 palette 默认色，
/// 才算「有覆盖」返回 Some；**不看几何**（状态改几何会致候选框跳动，state_geometry unsupported）。
fn resolve_state(
    node: Option<&ViewNode>,
    palette: &HashMap<String, Rgba>,
    is_dark: bool,
    def_bg: Option<Rgba>,
    def_text: Option<Rgba>,
) -> Option<Box<RvNode>> {
    let rc = |ld: &Option<Ld>| resolve_color(ld.as_ref(), palette, is_dark);
    let mut has = def_bg.is_some() || def_text.is_some();
    if let Some(n) = node {
        let gradient_has = n
            .background
            .gradient
            .as_ref()
            .is_some_and(|g| !g.stops.is_empty());
        if rc(&n.background.color).is_some()
            || n.background.image.is_some()
            || gradient_has
            || !n.layers.is_empty()
            || rc(&n.color).is_some()
            || rc(&n.border.color).is_some()
            || n.border.width.is_some()
            || n.font_weight.is_some()
        {
            has = true;
        }
    }
    if !has {
        return None;
    }
    let default_node;
    let n = match node {
        Some(n) => n,
        None => {
            default_node = ViewNode::default();
            &default_node
        }
    };
    Some(Box::new(resolve_view_node(
        n, palette, is_dark, def_bg, None, def_text,
    )))
}

/// 解析图片路径：data: URI / 绝对路径原样；相对路径拼到 theme 目录。
fn resolve_image_path(p: &str, base_dir: &Path) -> String {
    if p.starts_with("data:") || Path::new(p).is_absolute() {
        return p.to_string();
    }
    base_dir.join(p).to_string_lossy().into_owned()
}

/// 便捷：按名加载主题（base 深合并 + 类型化）并求值为 `Resolved`。
pub fn load_resolved(themes_dir: &Path, name: &str, is_dark: bool) -> anyhow::Result<Resolved> {
    load_resolved_dirs(&[themes_dir.to_path_buf()], name, is_dark)
}

/// 多目录版：在 dirs 中定位主题并求值（base 跨目录、资产按 base 链目录查找）。
pub fn load_resolved_dirs(
    dirs: &[std::path::PathBuf],
    name: &str,
    is_dark: bool,
) -> anyhow::Result<Resolved> {
    let t = crate::theme::load_typed_dirs(dirs, name)?;
    let chain = crate::theme::theme_chain_dirs(dirs, name);
    if chain.is_empty() {
        anyhow::bail!("theme '{}' not found", name);
    }
    Ok(resolve(&t, is_dark, &chain))
}

/// 求值入口：typed `Theme` + is_dark + 资产目录链（self 在前，base 在后）→ `Resolved`。
/// `asset_dirs[0]` = self 主题目录（resources 相对路径基准）；全部用于字面 image ref 查找。
pub fn resolve(theme: &Theme, is_dark: bool, asset_dirs: &[std::path::PathBuf]) -> Resolved {
    let self_dir = asset_dirs
        .first()
        .cloned()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let theme_dir = self_dir.as_path();
    // 1. palette（不 derive；resolve_palette 已忽略 derive 块）。
    let palette = resolve_palette(theme.colors.as_ref(), is_dark);
    // 2. views（无 views 块 → 全默认 RvViews，仅 palette 取色，零回归）。
    let views = match &theme.views {
        Some(v) => resolve_views(v, &palette, is_dark),
        None => RvViews::default(),
    };
    // 3. behavior（基线 ⊕ 主题）。
    let behavior = merge_behavior(theme);
    // 4. resources（按 is_dark 选变体 + 相对路径解析）。
    let resources = theme
        .resources
        .iter()
        .filter_map(|(name, ref_ld)| {
            ref_ld
                .select(is_dark)
                .map(|p| (name.clone(), resolve_image_path(p, theme_dir)))
        })
        .collect();

    Resolved {
        is_dark,
        palette,
        views,
        behavior,
        resources,
        asset_dirs: asset_dirs.to_vec(),
    }
}

fn merge_behavior(theme: &Theme) -> ResolvedBehavior {
    let mut b = ResolvedBehavior::default();
    if let Some(ov) = &theme.behavior {
        if let Some(v) = ov.font_size {
            b.font_size = v;
        }
        if let Some(v) = ov.always_show_pager {
            b.always_show_pager = v;
        }
        if let Some(v) = ov.show_page_number {
            b.show_page_number = v;
        }
        if let Some(v) = ov.hide_pager {
            b.hide_pager = v;
        }
        if let Some(v) = ov.vertical_max_width {
            b.vertical_max_width = v;
        }
        if let Some(v) = &ov.pager_align {
            b.pager_align = v.clone();
        }
    }
    b
}

/// 候选窗各节点 + 其它窗口 + 列表级几何求值（与 Go ResolveCandidateViews + other_views 对齐）。
fn resolve_views(v: &Views, palette: &HashMap<String, Rgba>, is_dark: bool) -> RvViews {
    let tk = |name: &str| palette.get(name).copied();
    let build = |n: &ViewNode, bg: Option<Rgba>, border: Option<Rgba>, text: Option<Rgba>| {
        resolve_view_node(n, palette, is_dark, bg, border, text)
    };

    let mut rv = RvViews {
        window: build(&v.window, tk("bg"), tk("border"), None),
        preedit_bar: build(&v.preedit_bar, tk("surface"), None, tk("text_dim")),
        candidate_list: build(&v.candidate_list, None, None, None),
        item: build(&v.item, None, None, None),
        index: build(&v.index, tk("accent"), None, tk("on_accent")),
        text: build(&v.text, None, None, tk("text")),
        comment: build(&v.comment, None, None, tk("text_hint")),
        accent_bar: build(&v.accent_bar, tk("accent"), None, None),
        footer_bar: build(&v.footer_bar, None, None, None),
        mode_label: build(&v.mode_label, None, None, tk("text_hint")),
        shadow_color: tk("shadow"),
        ..Default::default()
    };

    // item/text/index/comment 状态 patch。
    rv.item.selected = resolve_state(
        v.item.selected.as_deref(),
        palette,
        is_dark,
        tk("selection"),
        tk("selection_text"),
    );
    rv.item.hover = resolve_state(v.item.hover.as_deref(), palette, is_dark, tk("hover"), None);
    rv.item.disabled = resolve_state(v.item.disabled.as_deref(), palette, is_dark, None, None);
    rv.text.selected = resolve_state(
        v.text.selected.as_deref(),
        palette,
        is_dark,
        None,
        tk("selection_text"),
    );
    rv.text.hover = resolve_state(v.text.hover.as_deref(), palette, is_dark, None, None);
    rv.index.selected = resolve_state(v.index.selected.as_deref(), palette, is_dark, None, None);
    rv.index.hover = resolve_state(v.index.hover.as_deref(), palette, is_dark, None, None);
    rv.comment.selected =
        resolve_state(v.comment.selected.as_deref(), palette, is_dark, None, None);
    rv.comment.hover = resolve_state(v.comment.hover.as_deref(), palette, is_dark, None, None);

    // 序号槽位字符（index 节点）：透传到 Resolved，协调器裁决优先级（用户 > 主题 > 默认）。
    rv.index_labels = v.index.labels.clone();
    // 列表级几何（candidate_list 节点）。
    rv.item_spacing = v.candidate_list.gap;
    rv.window_gap = v.candidate_list.band_gap;
    rv.row_gap = v.candidate_list.row_gap;
    // window 投影 → 顶层 shadow 字段。
    if let Some(sh) = &v.window.shadow {
        rv.shadow_offset_x = sh.offset_x;
        rv.shadow_offset_y = sh.offset_y;
        rv.shadow_blur = sh.blur;
        rv.shadow_spread = sh.spread;
        rv.shadow_spread_offset_x = sh.spread_offset_x;
        rv.shadow_spread_offset_y = sh.spread_offset_y;
        if let Some(c) = resolve_color(sh.color.as_ref(), palette, is_dark) {
            rv.shadow_color = Some(c);
        }
    }
    // 候选窗位置偏移（window 节点 → 顶层，与 shadow_* 同一「属性归位」模式）。
    if let Some(po) = &v.window.position_offset {
        rv.window_offset_x = po.x;
        rv.window_offset_y = po.y;
    }
    // accent_bar 启用 + 几何。
    rv.accent_bar_enabled = v.accent_bar.enabled.unwrap_or(false);
    rv.accent_bar_width = v.accent_bar.width;
    rv.accent_bar_offset = v.accent_bar.offset;
    // 条高比：值域 (0, 1]，越界或未配一律落 DEFAULT_ACCENT_BAR_HEIGHT_RATIO。
    // ⚠️ 这里必须写出默认值而不是留 `f32` 的零值——`RvViews::default()` 的 0.0 会让
    // 强调条高度算成 0（钳到 2px 的细线），未配主题的强调条形同消失。
    rv.accent_bar_height_ratio = match v.accent_bar.height_ratio {
        Some(r) if r > 0.0 && r <= 1.0 => r,
        Some(r) => {
            tracing::warn!(
                ratio = r,
                "accent_bar.height_ratio 超出值域 (0,1]，回退默认 {}",
                DEFAULT_ACCENT_BAR_HEIGHT_RATIO
            );
            DEFAULT_ACCENT_BAR_HEIGHT_RATIO
        }
        None => DEFAULT_ACCENT_BAR_HEIGHT_RATIO,
    };

    // 其它窗口（status/tooltip/toast）：各注入自己 palette 默认色（T5 再补 toolbar/menu）。
    rv.status = v
        .status
        .as_ref()
        .map(|n| build(n, tk("status_bg"), None, tk("status_text")));
    rv.tooltip = v
        .tooltip
        .as_ref()
        .map(|n| build(n, tk("tooltip_bg"), None, tk("tooltip_text")));
    rv.toast = v
        .toast
        .as_ref()
        .map(|n| build(n, tk("toast_bg"), None, tk("toast_text")));
    // 菜单容器（menu.root）：背景色/图/层来源；颜色默认 menu_bg/menu_border/menu_text。
    rv.menu_root = v
        .menu
        .as_ref()
        .map(|m| build(&m.root, tk("menu_bg"), tk("menu_border"), tk("menu_text")));
    // 菜单项（menu.item）：几何（padding/hover 圆角/字号偏移）+ 文字色。
    rv.menu_item = v
        .menu
        .as_ref()
        .map(|m| build(&m.item, None, None, tk("menu_text")));
    // 菜单项状态 patch：与候选窗 item 同构——节点显式值优先，未配落到 palette 默认色。
    // 之前只建了基态，hover/disabled 全靠 popup_menu.rs 直读 token，导致主题里写的
    // menu.item.hover / .disabled 不生效。
    if let Some(m) = v.menu.as_ref()
        && let Some(item) = rv.menu_item.as_mut()
    {
        item.hover = resolve_state(
            m.item.hover.as_deref(),
            palette,
            is_dark,
            tk("menu_hover_bg"),
            tk("menu_hover_text"),
        );
        item.disabled = resolve_state(
            m.item.disabled.as_deref(),
            palette,
            is_dark,
            None,
            tk("menu_disabled"),
        );
    }
    // 菜单分隔线（menu.separator）：线色走 background.color，默认 menu_separator。
    rv.menu_separator = v
        .menu
        .as_ref()
        .map(|m| build(&m.separator, tk("menu_separator"), None, None));
    rv.menu_min_width = v.menu.as_ref().and_then(|m| m.min_width);

    // 工具栏几何：从 [toolbar] 直读 Dim（None→toolbar.rs 内置默认），供渲染器去硬编码。
    if let Some(tb) = &v.toolbar {
        rv.toolbar_height = tb.height;
        rv.toolbar_grip_width = tb.grip_width;
        rv.toolbar_button_width = tb.button_width;
        rv.toolbar_button_padding = tb.button_padding;
        rv.toolbar_button_radius = tb.button_radius;
        // 整体背景色/边框色：主题显式值优先，未配落 toolbar_background / toolbar_border token
        // （与其它窗口同一套「节点 ⊕ palette」语义）。
        rv.toolbar_bg_color = resolve_color(tb.background.color.as_ref(), palette, is_dark)
            .or_else(|| tk("toolbar_background"));
        rv.toolbar_border_color = resolve_color(tb.border.color.as_ref(), palette, is_dark)
            .or_else(|| tk("toolbar_border"));
        // 整条外框圆角/线宽：此前只提取了色，radius/width 解析到 schema 就断了，
        // 主题写了不生效（渲染层硬编码 h×0.30 与 1dp）。
        rv.toolbar_border_radius = tb.border.radius;
        rv.toolbar_border_width = tb.border.width;
    }

    rv
}

impl Resolved {
    /// 便捷：window 背景色（测试/调用方常用）。
    pub fn window_bg(&self) -> Option<Rgba> {
        self.views.window.bg_color
    }
    /// 按名取 palette 色（供工具栏/菜单等扁平取色，与旧 ResolvedTheme.color 同义）。
    pub fn color(&self, name: &str, default: Rgba) -> Rgba {
        self.palette.get(name).copied().unwrap_or(default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Dim;

    fn data_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes")
    }

    fn testdata_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/themes")
    }

    fn load(name: &str, is_dark: bool) -> Resolved {
        load_resolved_dirs(&[data_dir()], name, is_dark).unwrap()
    }

    fn load_jidian(is_dark: bool) -> Resolved {
        load_resolved_dirs(&[testdata_dir(), data_dir()], "jidian-classic", is_dark).unwrap()
    }

    #[test]
    fn test_index_labels_survive_resolve() {
        // 主题 [index] labels 经 normalize→typed→resolve 全链路存活到 Resolved.views.index_labels。
        // 协调器据此裁决「用户 > 主题 > 默认」；此测只验证主题层承载不丢。
        let text = "[index]\nlabels = [\"①\", \"②\", \"③\"]\n";
        let value: toml::Value = toml::from_str(text).unwrap();
        let normalized = crate::normalize::normalize_theme(value);
        let theme: Theme = normalized.try_into().unwrap();
        let r = resolve(&theme, false, &[data_dir()]);
        assert_eq!(
            r.views.index_labels,
            vec!["①".to_string(), "②".to_string(), "③".to_string()],
        );
    }

    /// 菜单颜色语义：节点显式值优先，未配回退 palette token（与候选窗一致）。
    /// 历史上 popup_menu.rs 只读 token、无视 [menu.*] 节点色，主题里写的颜色全不生效。
    #[test]
    fn test_menu_node_colors_override_palette() {
        let text = "\
[colors]
menu_bg = \"#111111\"
menu_text = \"#222222\"
menu_hover_bg = \"#333333\"
menu_hover_text = \"#444444\"
menu_disabled = \"#555555\"
menu_separator = \"#666666\"

[menu.root]
background = \"#AA0000\"

[menu.item]
color = \"#BB0000\"
font_size = 2

[menu.item.hover]
background = \"#CC0000\"
color = \"#DD0000\"

[menu.item.disabled]
color = \"#EE0000\"

[menu.separator]
background = \"#FF0000\"
";
        let value: toml::Value = toml::from_str(text).unwrap();
        let normalized = crate::normalize::normalize_theme(value);
        let theme: Theme = normalized.try_into().unwrap();
        let r = resolve(&theme, false, &[data_dir()]);

        let root = r.views.menu_root.as_ref().expect("menu_root");
        assert_eq!(
            root.bg_color,
            Some([0xAA, 0, 0, 255]),
            "容器背景色应取节点值"
        );

        let item = r.views.menu_item.as_ref().expect("menu_item");
        assert_eq!(
            item.text_color,
            Some([0xBB, 0, 0, 255]),
            "项文字色应取节点值"
        );
        assert_eq!(item.font_size, 2.0, "字号偏移应透传");

        let hover = item.hover.as_ref().expect("menu.item.hover 应被构建");
        assert_eq!(hover.bg_color, Some([0xCC, 0, 0, 255]));
        assert_eq!(hover.text_color, Some([0xDD, 0, 0, 255]));

        let disabled = item.disabled.as_ref().expect("menu.item.disabled 应被构建");
        assert_eq!(disabled.text_color, Some([0xEE, 0, 0, 255]));

        let sep = r.views.menu_separator.as_ref().expect("menu_separator");
        assert_eq!(sep.bg_color, Some([0xFF, 0, 0, 255]), "分隔线色应取节点值");
    }

    /// 未写 [menu.*] 节点色时回退 palette token —— 保证既有主题行为不变。
    #[test]
    fn test_menu_falls_back_to_palette_tokens() {
        let text = "\
[colors]
menu_bg = \"#111111\"
menu_text = \"#222222\"
menu_hover_bg = \"#333333\"
menu_hover_text = \"#444444\"
menu_disabled = \"#555555\"
menu_separator = \"#666666\"

[menu.root]
radius = 6
";
        let value: toml::Value = toml::from_str(text).unwrap();
        let normalized = crate::normalize::normalize_theme(value);
        let theme: Theme = normalized.try_into().unwrap();
        let r = resolve(&theme, false, &[data_dir()]);

        let root = r.views.menu_root.as_ref().expect("menu_root");
        assert_eq!(root.bg_color, Some([0x11, 0x11, 0x11, 255]));
        let item = r.views.menu_item.as_ref().expect("menu_item");
        assert_eq!(item.text_color, Some([0x22, 0x22, 0x22, 255]));
        let hover = item.hover.as_ref().expect("hover 由 palette 默认色撑起");
        assert_eq!(hover.bg_color, Some([0x33, 0x33, 0x33, 255]));
        assert_eq!(hover.text_color, Some([0x44, 0x44, 0x44, 255]));
        let disabled = item
            .disabled
            .as_ref()
            .expect("disabled 由 palette 默认色撑起");
        assert_eq!(disabled.text_color, Some([0x55, 0x55, 0x55, 255]));
        let sep = r.views.menu_separator.as_ref().expect("menu_separator");
        assert_eq!(sep.bg_color, Some([0x66, 0x66, 0x66, 255]));
    }

    /// 强调条条高比：显式值透传；未配 / 越界一律落默认 0.6。
    ///
    /// ⚠️ 这条锁的是「未配 ≠ 0.0」。`RvViews` derive 的 Default 会给 0.0，若 resolve
    /// 不显式写默认值，所有未配该项的主题（含全部内置主题）强调条会塌成 2px 细线。
    #[test]
    fn test_accent_bar_height_ratio_defaults_and_clamps() {
        let build = |accent: &str| {
            let text = format!("[colors]\naccent = \"#3B82F6\"\n\n[accent_bar]\n{accent}");
            let value: toml::Value = toml::from_str(&text).unwrap();
            let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
            resolve(&theme, false, &[data_dir()])
                .views
                .accent_bar_height_ratio
        };

        // 未配 → 默认
        assert_eq!(build("enabled = true\n"), DEFAULT_ACCENT_BAR_HEIGHT_RATIO);
        // 显式值透传
        assert_eq!(build("enabled = true\nheight_ratio = 1.0\n"), 1.0);
        assert_eq!(build("enabled = true\nheight_ratio = 0.35\n"), 0.35);
        // 越界（≤0 / >1）→ 回退默认，不 fail
        assert_eq!(
            build("enabled = true\nheight_ratio = 0.0\n"),
            DEFAULT_ACCENT_BAR_HEIGHT_RATIO
        );
        assert_eq!(
            build("enabled = true\nheight_ratio = -0.5\n"),
            DEFAULT_ACCENT_BAR_HEIGHT_RATIO
        );
        assert_eq!(
            build("enabled = true\nheight_ratio = 1.5\n"),
            DEFAULT_ACCENT_BAR_HEIGHT_RATIO
        );
    }

    /// 内置主题真实加载出的条高比必须**可用**（0 < r ≤ 1），不会塌成 2px 细线。
    ///
    /// 这条原先写作「内置主题都没写 height_ratio，故都应等于 0.6」——把「守住默认值」
    /// 借「主题恰好没配」当载体。msime 后来显式配了条高比，载体作废、测试误红，可
    /// resolve 的默认值逻辑一行没动。故此处只锁「值可用」这个与主题选值无关的性质；
    /// 「未配→0.6」由上面的 `..._defaults_and_clamps` 用受控文本正面覆盖，且下面单独
    /// 用 default（确未配）再钉一次，两者互不牵连。
    #[test]
    fn test_builtin_themes_get_usable_accent_ratio() {
        for name in ["default", "msime"] {
            let r = load(name, false).views.accent_bar_height_ratio;
            assert!(
                r > 0.0 && r <= 1.0,
                "{name} 条高比 {r} 不可用（0 会被渲染钳成 2px 细线）"
            );
        }
        assert_eq!(
            load("default", false).views.accent_bar_height_ratio,
            DEFAULT_ACCENT_BAR_HEIGHT_RATIO,
            "default 未配 height_ratio，应落默认值而非 0"
        );
    }

    /// 工具栏整体背景色/边框色：节点优先，未配回退 toolbar_* token。
    #[test]
    fn test_toolbar_node_colors_override_palette() {
        let text = "\
[colors]
toolbar_background = \"#111111\"
toolbar_border = \"#222222\"

[toolbar]
background = \"#AA0000\"
border = { color = \"#BB0000\" }
";
        let value: toml::Value = toml::from_str(text).unwrap();
        let normalized = crate::normalize::normalize_theme(value);
        let theme: Theme = normalized.try_into().unwrap();
        let r = resolve(&theme, false, &[data_dir()]);
        assert_eq!(r.views.toolbar_bg_color, Some([0xAA, 0, 0, 255]));
        assert_eq!(r.views.toolbar_border_color, Some([0xBB, 0, 0, 255]));

        // 未配节点色 → 回退 token
        let text2 = "\
[colors]
toolbar_background = \"#111111\"
toolbar_border = \"#222222\"

[toolbar]
height = 30
";
        let v2: toml::Value = toml::from_str(text2).unwrap();
        let t2: Theme = crate::normalize::normalize_theme(v2).try_into().unwrap();
        let r2 = resolve(&t2, false, &[data_dir()]);
        assert_eq!(r2.views.toolbar_bg_color, Some([0x11, 0x11, 0x11, 255]));
        assert_eq!(r2.views.toolbar_border_color, Some([0x22, 0x22, 0x22, 255]));
    }

    /// 候选窗位置偏移：内联表与 `[x, y]` 简写等价，未配为 None（=0，旧行为）。
    #[test]
    fn test_window_position_offset() {
        let load_offset = |body: &str| {
            let value: toml::Value = toml::from_str(body).unwrap();
            let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
            let v = resolve(&theme, false, &[data_dir()]).views;
            (v.window_offset_x, v.window_offset_y)
        };

        let inline = load_offset("[window]\nposition_offset = { x = 3, y = 4 }\n");
        assert_eq!(inline.0, Some(crate::schema::Dim::Dp(3.0)));
        assert_eq!(inline.1, Some(crate::schema::Dim::Dp(4.0)));
        // 数组简写（与 shadow.offset 一致的书写体验）→ 归一化成同一形态
        assert_eq!(load_offset("[window]\nposition_offset = [3, 4]\n"), inline);
        // px 单位不随 DPI 缩放
        assert_eq!(
            load_offset("[window]\nposition_offset = { y = \"2px\" }\n").1,
            Some(crate::schema::Dim::Px(2.0))
        );
        // 未配 → None，place_window 拿到 0，定位与旧版逐像素一致
        assert_eq!(load_offset("[window]\nradius = 8\n"), (None, None));
    }

    /// `slice_repeat` 要一路走到渲染消费形态：字符串在求值层坍缩成两轴布尔。
    ///
    /// 这条守的是数据通路——渲染层再怎么实现平铺，这里断了就永远是拉伸。
    #[test]
    fn slice_repeat_reaches_rv_image_per_axis() {
        let text = "\
[preedit_bar]
background = { image = { ref = \"p.png\", mode = \"nine_slice\", slice_repeat = [\"repeat\", \"stretch\"] } }

[item]
background = { image = { ref = \"q.png\", mode = \"nine_slice\" } }
";
        let value: toml::Value = toml::from_str(text).unwrap();
        let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
        let v = resolve(&theme, false, &[data_dir()]).views;

        let bar = v.preedit_bar.bg_image.expect("preedit_bar 背景图");
        assert_eq!(bar.slice_repeat, [true, false], "两轴分别求值，不能串");

        // 不写的主题保持原样：全部拉伸，既有观感不被新字段改掉。
        let item = v.item.bg_image.expect("item 背景图");
        assert_eq!(item.slice_repeat, [false, false]);
    }

    /// 四个「此前渲染层不读自身盒模型」的节点：背景与边框须完整进 RvNode。
    ///
    /// preedit_bar / mode_label 的圆角原先借用 item.border_radius（调候选项圆角会连带
    /// 改预编辑栏）；candidate_list / footer_bar 连背景边框都没接线。渲染侧已补齐，
    /// 这里锁数据通路——防止日后在 resolve 里加节点白名单时又把它们漏掉。
    #[test]
    fn test_container_nodes_carry_own_box_model() {
        let text = "\
[colors]
accent = \"#3B82F6\"

[preedit_bar]
background = \"#111111\"
radius = 7
border = { color = \"#222222\", width = 2 }

[mode_label]
background = \"#333333\"
radius = 9
border = { color = \"#444444\" }

[candidate_list]
background = \"#555555\"
radius = 11
border = { color = \"#666666\" }

[footer_bar]
background = \"#777777\"
radius = 13
border = { color = \"#888888\" }
";
        let value: toml::Value = toml::from_str(text).unwrap();
        let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
        let v = resolve(&theme, false, &[data_dir()]).views;

        let cases: [(&str, &RvNode, u8, u8, f32); 4] = [
            ("preedit_bar", &v.preedit_bar, 0x11, 0x22, 7.0),
            ("mode_label", &v.mode_label, 0x33, 0x44, 9.0),
            ("candidate_list", &v.candidate_list, 0x55, 0x66, 11.0),
            ("footer_bar", &v.footer_bar, 0x77, 0x88, 13.0),
        ];
        for (name, node, bg, border, radius) in cases {
            assert_eq!(node.bg_color, Some([bg, bg, bg, 255]), "{name} 背景色");
            assert_eq!(
                node.border_color,
                Some([border, border, border, 255]),
                "{name} 边框色"
            );
            assert_eq!(
                node.border_radius,
                Some(crate::schema::Dim::Dp(radius)),
                "{name} 圆角（自身的，不是借 item 的）"
            );
        }
        // preedit_bar 显式边框宽也要透传（渲染层 `dim(border_width, 0).max(1)`）。
        assert_eq!(
            v.preedit_bar.border_width,
            Some(crate::schema::Dim::Dp(2.0))
        );
        // 未配圆角的节点保持 None —— 渲染层据此回退（preedit_bar/mode_label 回退 item）。
        let bare: toml::Value = toml::from_str("[colors]\naccent = \"#3B82F6\"\n").unwrap();
        let t2: Theme = crate::normalize::normalize_theme(bare).try_into().unwrap();
        let v2 = resolve(&t2, false, &[data_dir()]).views;
        assert_eq!(v2.preedit_bar.border_radius, None);
        assert_eq!(v2.mode_label.border_radius, None);
    }

    /// 工具栏整条外框圆角/线宽：此前只提取了 border.color，radius/width 解析到
    /// schema 就断了 —— 编辑器把两个控件都开放着，实机却毫无变化。
    #[test]
    fn test_toolbar_border_radius_and_width_extracted() {
        let text = "\
[toolbar]
border = { color = \"#BB0000\", radius = 0, width = \"2px\" }
";
        let value: toml::Value = toml::from_str(text).unwrap();
        let theme: Theme = crate::normalize::normalize_theme(value).try_into().unwrap();
        let r = resolve(&theme, false, &[data_dir()]);
        // radius=0 必须是「显式直角」而非「未配」——硬边缘风格靠它。
        assert_eq!(
            r.views.toolbar_border_radius,
            Some(crate::schema::Dim::Dp(0.0))
        );
        // px 单位不随 DPI 加粗（发丝线）。
        assert_eq!(
            r.views.toolbar_border_width,
            Some(crate::schema::Dim::Px(2.0))
        );

        // 未配 → None，渲染层落内置派生值（条高×0.30 / 1dp），零回归。
        let bare: toml::Value = toml::from_str("[toolbar]\nheight = 30\n").unwrap();
        let t2: Theme = crate::normalize::normalize_theme(bare).try_into().unwrap();
        let r2 = resolve(&t2, false, &[data_dir()]);
        assert_eq!(r2.views.toolbar_border_radius, None);
        assert_eq!(r2.views.toolbar_border_width, None);
    }

    #[test]
    fn test_default_theme_resolve() {
        let r = load("default", false);
        // window bg = bg light = 白
        assert_eq!(r.window_bg(), Some([255, 255, 255, 255]));
        // v4 设计基线：序号为淡色纯数字。shape="none" → 不画圆圈（bg_color 兜底 accent 但不渲染）。
        assert_eq!(r.views.index.bg_shape, "none");
        assert_eq!(r.views.index.bg_color, Some([0x3b, 0x82, 0xf6, 255])); // accent 兜底(因 shape=none 不绘制)
        assert_eq!(r.views.index.text_color, Some([0x9A, 0xA0, 0xAE, 255])); // text_hint light
        // 选中候选文字用 accent 高亮（[text.selected].color = accent_text light）。
        let sel = r.views.text.selected.as_ref().expect("text.selected patch");
        assert_eq!(sel.text_color, Some([0x25, 0x63, 0xeb, 255]));
        // 行为：default 单页不显翻页区；清风设计默认不显页码数字（保持整洁）。
        assert!(!r.behavior.always_show_pager);
        assert!(!r.behavior.show_page_number);
        assert_eq!(r.behavior.font_size, 18);
        // 暗色 bg = 深蓝灰（设计稿暗色面板基调 #121826）
        let d = load("default", true);
        assert_eq!(d.window_bg(), Some([0x12, 0x18, 0x26, 255]));
    }

    #[test]
    fn test_jidian_classic_rvnode() {
        let r = load_jidian(false);
        // window 九宫格背景图
        let bg = r.views.window.bg_image.as_ref().expect("window bg image");
        assert_eq!(bg.reference, "panel");
        assert_eq!(bg.mode, "nine_slice");
        assert_eq!(bg.slice, [8.0, 8.0, 8.0, 8.0]);
        // window 投影 blur 进顶层 shadow
        assert_eq!(r.views.shadow_blur, Some(Dim::Dp(8.0)));
        // layers 水印 z=1 + 透明度
        assert_eq!(r.views.window.layers.len(), 1);
        let mark = &r.views.window.layers[0];
        assert_eq!(mark.reference, "mark");
        assert_eq!(mark.z, 1);
        assert_eq!(mark.opacity, 0.9);
        assert_eq!(mark.offset_x, Some(Dim::Dp(-8.0)));
        // item 选中态 patch：背景图 + 字重（几何不进）
        let sel = r.views.item.selected.as_ref().expect("item.selected patch");
        assert_eq!(sel.font_weight, 600);
        assert!(sel.bg_image.is_some());
        // accent_bar 几何
        assert_eq!(r.views.accent_bar_width, Some(Dim::Dp(3.0)));
        // resources 解析为绝对路径（按 is_dark 选 light 变体 panel.png）
        let panel = r.resources.get("panel").expect("panel resource");
        assert!(panel.ends_with("panel.png"), "got {panel}");
        assert!(Path::new(panel).is_absolute(), "应为绝对路径: {panel}");
        // 暗色选 dark 变体
        let d = load_jidian(true);
        assert!(
            d.resources
                .get("panel")
                .unwrap()
                .ends_with("panel-dark.png")
        );
        // 其它窗口也吃九宫格
        let status = r.views.status.as_ref().expect("status rvnode");
        assert!(status.bg_image.is_some());
        assert_eq!(status.layers.len(), 1);
    }

    #[test]
    fn test_read_meta_and_multidir_base() {
        let dirs = vec![testdata_dir(), data_dir()];
        // 显示名 / order 取自主题自身 meta（不 base 合并）。
        let m = crate::theme::read_meta(&dirs, "jidian-classic").expect("jidian meta");
        assert_eq!(m.name, "极点经典(位图)");
        assert_eq!(m.order, 2);
        let dm = crate::theme::read_meta(&[data_dir()], "default").expect("default meta");
        assert_eq!(dm.name, "清风·蓝");
        // 多目录加载：jidian `base: _base` 跨目录解析正常（resources/views 出来）。
        let r = load_resolved_dirs(&dirs, "jidian-classic", false).expect("resolve jidian");
        assert!(
            r.views.window.bg_image.is_some(),
            "应解析出 _base+jidian 合并后的 window 背景图"
        );
        assert!(r.resources.contains_key("panel"));

        // 资产链：default 经 _qingfeng → _base，链中应含 _base 目录（_base 的 chevron.svg 可被继承解析）。
        let d = load_resolved_dirs(&[data_dir()], "default", false).expect("resolve default");
        assert!(
            d.asset_dirs.iter().any(|p| p.ends_with("_base")),
            "default 资产链应含 _base 目录"
        );
        // 清风设计把 _base 的 chevron 覆盖为空 ref（回退细小文字 ‹ › 翻页箭头）。
        let prev = d
            .views
            .footer_bar
            .prev_image
            .as_ref()
            .expect("footer prev_image");
        assert_eq!(prev.reference, "", "清风主题应覆盖 chevron 为空 → 文字箭头");
    }

    #[test]
    fn test_state_gating_drops_geometry_only_patch() {
        // hover 只改 padding（几何）应被视为空 patch 丢弃；改色则保留。
        let palette = HashMap::new();
        let geo_only = ViewNode {
            padding: crate::schema::ViewEdges {
                top: Some(Dim::Dp(99.0)),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(
            resolve_state(Some(&geo_only), &palette, false, None, None).is_none(),
            "纯几何 patch 应丢弃"
        );
        let color_patch = ViewNode {
            color: Some(Ld::Scalar("#FF0000".into())),
            ..Default::default()
        };
        assert!(
            resolve_state(Some(&color_patch), &palette, false, None, None).is_some(),
            "改色 patch 应保留"
        );
    }
}
