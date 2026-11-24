//! 求值：typed `Theme` → `Resolved`（palette + RVNode 树 + resources + behavior）。
//!
//! 与 Go 版 `pkg/theme` 的 `ResolveV3`/`ResolveCandidateViews`/`resolveViewNode` 对齐，但按迁移决策精简：
//! - **不做 derive**（主题须显式给全语义色；见 docs/redesign/theme-migration-plan.md 决策 2）；
//! - **LightDark 解析期坍缩**：求值即 `select(is_dark)→单值`，RVNode 只存终值（决策 3）；
//! - **validate 降级为 warn**：未解析的 `${token}`/缺失 ref 记 `tracing::warn`，不 fail（决策 5）；
//!   配合 `Option` 兜底（缺默认=沿用基态/渲染器内置），外部坏主题不黑屏。

use crate::palette::{Rgba, parse_hex, resolve_palette};
use crate::rvnode::{
    DEFAULT_ACCENT_BAR_HEIGHT_RATIO, Flip, RvEdges, RvGradient, RvImage, RvNode, RvViews,
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
    /// 竖排覆盖后的视图：主题有 `[节点.vertical]` 时才有。竖排候选窗用它取代 [`Self::views`]。
    pub views_vertical: Option<RvViews>,
    pub behavior: ResolvedBehavior,
    /// 图片资源：名→绝对路径 / data: URI（相对路径已按 theme 目录解析，按 is_dark 选变体）。
    pub resources: HashMap<String, String>,
    /// 暗色求值时，亮暗两侧路径不同的资源 → 亮色那侧的路径（同 [`Self::resources`] 已解析）。
    /// 亮色求值时为空。皮肤据此校验亮暗两图尺寸一致，不一致改用亮色图。
    pub light_resources: HashMap<String, String>,
    /// 资产搜索目录（self + base 链）：用于视图节点里**字面** image ref（如 _base 的 chevron.svg，
    /// 不在 resources 注册表）解析为绝对路径——派生主题继承 base 的图标需到 base 目录查找。
    pub asset_dirs: Vec<std::path::PathBuf>,
    /// 任务栏语言栏图标主字色（token `langbar_text_cn` / `langbar_text_en`）。
    pub langbar_text: LangbarTextColors,
    /// 异形皮肤；None = 未配（或没有可用变体）。
    pub skin: Option<Box<crate::skin::RvSkin>>,
}

/// 任务栏语言栏图标主字色，四格独立。
///
/// ⚠ 这里的 light/dark 指**任务栏**明暗（服务端每档都渲染两套图标、DLL 按任务栏明暗挑），
/// 与候选窗主题的 `is_dark` 无关——所以 `resolve()` 对 colors 另按 false/true 各解析一次，
/// 不跟随本次求值的 `is_dark`。`None` = 主题没给，由消费端回落内置黑/白。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LangbarTextColors {
    pub cn_light: Option<Rgba>,
    pub cn_dark: Option<Rgba>,
    pub en_light: Option<Rgba>,
    pub en_dark: Option<Rgba>,
}

/// 按任务栏浅/深两档各解析一次 colors，取 `langbar_text_cn` / `langbar_text_en`。
fn resolve_langbar_text(colors: Option<&toml::Value>) -> LangbarTextColors {
    let light = resolve_palette(colors, false);
    let dark = resolve_palette(colors, true);
    LangbarTextColors {
        cn_light: light.get("langbar_text_cn").copied(),
        cn_dark: dark.get("langbar_text_cn").copied(),
        en_light: light.get("langbar_text_en").copied(),
        en_dark: dark.get("langbar_text_en").copied(),
    }
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
        flip: match im.flip.as_deref() {
            None => Flip::NONE,
            Some(s) => Flip::parse(s).unwrap_or_else(|| {
                tracing::warn!("主题图片 flip 取值非法 {s:?}（只认 x / y / xy），按不翻");
                Flip::NONE
            }),
        },
    }
}

/// schema ViewGradient → RVGradient（stop 颜色解析 + 按 pos 升序；无有效 stop 返回 None）。
fn resolve_gradient(
    g: &ViewGradient,
    palette: &HashMap<String, Rgba>,
    is_dark: bool,
) -> Option<RvGradient> {
    // 缺 pos 的均分按原序号算（颜色解析失败的 stop 也占位），与编辑器 resolveGradientV3 同口径。
    let n = g.stops.len();
    let mut stops: Vec<(Rgba, f32)> = g
        .stops
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let even = if n > 1 {
                i as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let pos = s.pos.unwrap_or(even).clamp(0.0, 1.0);
            resolve_color(Some(&s.color), palette, is_dark).map(|c| (c, pos))
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
    out.roles = resolve_roles(&n.roles, palette, is_dark);
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

/// 角色色表求值：`""`（未设置）与未解析的 token 不入表（后者沿用 `resolve_color` 的 warn）；
/// `transparent` 与 alpha 为 0 的颜色拒收并 warn——文字占着宽度却看不见，只会是误用，与内联色
/// 同一口径（§4.5、§5.1）。
fn resolve_roles(
    roles: &HashMap<String, Ld>,
    palette: &HashMap<String, Rgba>,
    is_dark: bool,
) -> HashMap<String, Rgba> {
    roles
        .iter()
        .filter_map(|(role, ld)| {
            let c = resolve_color(Some(ld), palette, is_dark)?;
            if c[3] == 0 {
                tracing::warn!("主题角色色 {role} 是全透明色，按未设置处理");
                return None;
            }
            Some((role.clone(), c))
        })
        .collect()
}

/// 状态 patch ViewNode → 递归 RVNode（与 Go resolveState 对齐）。
/// nil-gating：仅当显式给了 bg/图/渐变/层/text/border 色/border 宽/字重/角色色，或有 palette 默认色，
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
    let default_node;
    let n = match node {
        Some(n) => n,
        None => {
            default_node = ViewNode::default();
            &default_node
        }
    };
    // 先求值再判：角色色表只解析一次（它的 warn 不该因门控多打一遍）；只写了
    // `[comment.selected.roles]` 的 patch 也算「有覆盖」，否则被整体丢弃。
    let out = resolve_view_node(n, palette, is_dark, def_bg, None, def_text);
    (has || !out.roles.is_empty()).then(|| Box::new(out))
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
    // 2a. 竖排变体（同一份 palette）。
    let views_vertical = theme
        .vertical
        .as_ref()
        .and_then(|t| t.views.as_ref())
        .map(|v| resolve_views(v, &palette, is_dark));
    // 2b. 补主题没写的标准色契约名。排在 views 之后：节点的 palette 默认色（`tk("text")` 之类）
    // 仍只看主题自己写的，兜底只服务于按名求色（内联色）与按名取色的渲染层。
    let mut palette = palette;
    crate::contract::fill_missing(theme.colors.as_ref(), &mut palette, is_dark);
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
    let light_resources = theme
        .resources
        .iter()
        .filter(|_| is_dark)
        .filter_map(|(name, ref_ld)| {
            let light = ref_ld.select(false)?;
            (Some(light) != ref_ld.select(true))
                .then(|| (name.clone(), resolve_image_path(light, theme_dir)))
        })
        .collect();

    Resolved {
        is_dark,
        palette,
        views,
        views_vertical,
        behavior,
        resources,
        light_resources,
        asset_dirs: asset_dirs.to_vec(),
        langbar_text: resolve_langbar_text(theme.colors.as_ref()),
        skin: theme
            .skin
            .as_deref()
            .and_then(crate::skin::resolve_skin)
            .map(Box::new),
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

    // 上方注释条：字体 / 颜色 / roles / 状态 patch 缺省回退 comment，margin / padding 不继承
    // （设计 candidate-comment-above-line §3.4）。回退在 build 之前做，渲染层只读解析后的值。
    let mut above = v.comment_above.clone();
    above.inherit_from(&v.comment);

    let mut rv = RvViews {
        window: build(&v.window, tk("bg"), tk("border"), None),
        preedit_bar: build(&v.preedit_bar, tk("surface"), None, tk("text_dim")),
        candidate_list: build(&v.candidate_list, None, None, None),
        item: build(&v.item, None, None, None),
        index: build(&v.index, tk("accent"), None, tk("on_accent")),
        text: build(&v.text, None, None, tk("text")),
        comment: build(&v.comment, None, None, tk("text_hint")),
        comment_above: build(&above, None, None, tk("text_hint")),
        accent_bar: build(&v.accent_bar, tk("accent"), None, None),
        footer_bar: build(&v.footer_bar, None, None, None),
        mode_label: build(&v.mode_label, None, None, tk("text_hint")),
        divider: build(&v.divider, tk("separator"), None, None),
        grid_row: build(&v.grid_row, None, None, None),
        grid_divider: build(&v.grid_divider, None, None, None),
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
    rv.comment_above.selected =
        resolve_state(above.selected.as_deref(), palette, is_dark, None, None);
    rv.comment_above.hover = resolve_state(above.hover.as_deref(), palette, is_dark, None, None);
    // footer_bar 的禁用态（首页的「上一页」/末页的「下一页」）：主题编辑器「禁用态」面板
    // 配的就是它，此前这里不建、渲染层硬用全局 text_hint，于是面板上配了没反应、预览又
    // 照配置画 —— 预览与实机因此对不上。
    rv.footer_bar.disabled = resolve_state(
        v.footer_bar.disabled.as_deref(),
        palette,
        is_dark,
        None,
        None,
    );

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
    rv.divider_enabled = v.divider.enabled;
    rv.divider_height = v.divider.height;
    rv.grid_divider_enabled = v.grid_divider.enabled;
    rv.grid_divider_height = v.grid_divider.height;
    rv.grid_row_align = v.grid_row.align.clone();
    // 整行状态：不设默认底色（缺省透明），否则会叠在格子自己的选中底色上。
    rv.grid_row.selected =
        resolve_state(v.grid_row.selected.as_deref(), palette, is_dark, None, None);
    rv.grid_row.hover = resolve_state(v.grid_row.hover.as_deref(), palette, is_dark, None, None);
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
#[path = "resolve_tests.rs"]
mod tests;
