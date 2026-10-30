//! 渲染消费形态（RVNode 树）——与 Go 版 `pkg/theme` 的 `RVNode`/`RVImage`/`RVGradient` 对齐。
//!
//! 求值后的最终外观：颜色已解析为 `Rgba`（`None`=无默认、沿用基态/渲染器内置）；几何保持 `Dim`
//! 符号态（paint 期按 scale/host 求值）；背景图/层/渐变转 spec（不解码位图，由 wind-ui 按 ref 缓存）。

use crate::palette::Rgba;
use crate::schema::Dim;

/// 强调条条高占行高的默认比例（主题未配 `accent_bar.height_ratio` 时）。
///
/// ⚠️ `RvViews` derive 的 `Default` 会把 `accent_bar_height_ratio` 置 0.0，那是
/// 「条高算成 0」而非「用默认比例」。凡消费该字段处都须判零回退到本常量，
/// 否则未配比例的主题强调条会塌成 2px 细线。
pub const DEFAULT_ACCENT_BAR_HEIGHT_RATIO: f32 = 0.6;

/// None=0/未设。paint 期 `Dim::resolve(scale, host)` 求值。
#[inline]
fn dim_px(d: Option<Dim>, scale: f32, host: f32) -> f32 {
    d.map(|x| x.resolve(scale, host)).unwrap_or(0.0)
}

/// 四向几何（margin/padding）。None=0。
#[derive(Clone, Debug, Default)]
pub struct RvEdges {
    pub top: Option<Dim>,
    pub right: Option<Dim>,
    pub bottom: Option<Dim>,
    pub left: Option<Dim>,
}

impl RvEdges {
    /// 求值为 [上,右,下,左] 像素（dp×scale）。margin/padding 一般不用百分比，host 传 0。
    pub fn resolve(&self, scale: f32) -> [f32; 4] {
        [
            dim_px(self.top, scale, 0.0),
            dim_px(self.right, scale, 0.0),
            dim_px(self.bottom, scale, 0.0),
            dim_px(self.left, scale, 0.0),
        ]
    }
}

/// 渲染消费形态图片 spec（背景填充图 / layers 覆盖图 / footer 箭头共用）。不含解码位图。
#[derive(Clone, Debug, Default)]
pub struct RvImage {
    /// resources 键或字面 path / data: URI（求值后已坍缩 light/dark）。
    pub reference: String,
    /// nine_slice | stretch | tile | center；空=stretch。
    pub mode: String,
    /// 仅 nine_slice：源图四边切片像素 [上,右,下,左]（纹理空间，不随 DPI 缩放）。
    pub slice: [f32; 4],
    /// 仅 nine_slice：中段沿 [x, y] 轴平铺而非拉伸。
    pub slice_repeat: [bool; 2],
    /// 已解析不透明度（None→1.0）。
    pub opacity: f32,
    /// 仅 layers：内容基准 0，<0 在内容下、>0 在上。
    pub z: i32,
    /// 九宫锚点（覆盖图恒用；背景图配了任一定位字段才用，见 schema::ViewImage::anchor）。
    pub anchor: String,
    /// 定位偏移：dp 或百分比（paint 期相对 host 求值）。
    pub offset_x: Option<Dim>,
    pub offset_y: Option<Dim>,
    /// 定位尺寸（逻辑像素）；0=该维取原图尺寸。
    pub w: i32,
    pub h: i32,
    /// 单色染色（None=图原样）；非 None 时把图当 alpha mask 用此色填充。
    pub tint: Option<Rgba>,
    /// 禁用态染色（仅 footer 箭头）。
    pub disabled_tint: Option<Rgba>,
}

/// 渐变 spec（stop 已解析颜色 + 按 pos 升序）。
#[derive(Clone, Debug)]
pub struct RvGradient {
    /// "linear"（默认）| "radial"。
    pub kind: String,
    /// linear 角度（度）：0=左→右、90=上→下。
    pub angle: f32,
    /// (颜色, pos∈[0,1])，已按 pos 升序。
    pub stops: Vec<(Rgba, f32)>,
}

/// 单个 View 求值后的外观。`Option<Rgba>` 颜色：None=无默认（沿用基态/渲染器内置）。
/// 几何为 `Option<Dim>` 符号态（None=0），paint 期求值。
#[derive(Clone, Debug, Default)]
pub struct RvNode {
    pub margin: RvEdges,
    pub padding: RvEdges,
    pub border_radius: Option<Dim>,
    pub border_width: Option<Dim>,
    pub border_color: Option<Rgba>,
    /// 边框线型："solid"(默认)|"dashed"|"dotted"。None/空=solid。
    /// 数据通路已就绪；wind-ui 虚线/点线光栅绘制待补（当前按 solid 渲染）。
    pub border_style: Option<String>,
    pub bg_color: Option<Rgba>,
    /// 背景形状："circle"|"none"（空=none）。当前仅 index 序号消费（圆形序号底）。
    pub bg_shape: String,
    /// 相对主候选字体的有符号偏移（逻辑 px）；0=同主字体。
    pub font_size: f32,
    /// 0=继承全局。
    pub font_weight: i32,
    /// None/空=继承全局字体族。
    pub font_family: Option<String>,
    pub text_color: Option<Rgba>,
    pub bg_image: Option<RvImage>,
    pub bg_gradient: Option<RvGradient>,
    pub layers: Vec<RvImage>,
    /// 仅 footer_bar：翻页箭头图/字符（同层图优先；继承来的图已在合并层让位，见 schema 同名字段）。
    pub prev_image: Option<RvImage>,
    pub next_image: Option<RvImage>,
    pub prev_char: String,
    pub next_char: String,
    /// 多行/多列布局间距（tooltip/toast 专有）。
    pub line_spacing: Option<Dim>,
    pub col_gap: Option<Dim>,
    pub title_gap: Option<Dim>,
    /// 窗口投影（window / status / tooltip / toast）。
    pub shadow_offset_x: Option<Dim>,
    pub shadow_offset_y: Option<Dim>,
    pub shadow_blur: Option<Dim>,
    pub shadow_spread: Option<Dim>,
    /// 仅模糊扩散层的额外偏移（叠加在 shadow_offset_x/y 之上）。
    pub shadow_spread_offset_x: Option<Dim>,
    pub shadow_spread_offset_y: Option<Dim>,
    pub shadow_color: Option<Rgba>,
    /// 文字角色色（分段着色，`[comment.roles]` / `[tooltip.roles]`）：角色名 → 已解析颜色。
    /// 空 = 没配，片段一律按正文色。只有 `comment`（含状态 patch）与 `tooltip` 消费，
    /// 求色见 [`crate::span::span_color`]。
    pub roles: std::collections::HashMap<String, Rgba>,
    /// 状态 patch（递归）。仅合并色/图/边框/字体/层，不合并几何（state_geometry unsupported）。
    pub selected: Option<Box<RvNode>>,
    pub hover: Option<Box<RvNode>>,
    pub disabled: Option<Box<RvNode>>,
}

/// 候选窗各具名 View + 其它窗口（status/tooltip/toast）+ 列表级几何，求值后形态。
/// toolbar/menu 留 T5（其它窗口）解析。
#[derive(Clone, Debug, Default)]
pub struct RvViews {
    pub window: RvNode,
    pub preedit_bar: RvNode,
    pub candidate_list: RvNode,
    pub item: RvNode,
    pub index: RvNode,
    pub text: RvNode,
    pub comment: RvNode,
    /// 上方注释条（`views.comment_above`）：回退 comment 的字体 / 颜色 / roles 已在 resolve 阶段做完，
    /// 渲染只读它；margin / padding 不继承。
    pub comment_above: RvNode,
    pub accent_bar: RvNode,
    pub footer_bar: RvNode,
    pub mode_label: RvNode,
    pub status: Option<RvNode>,
    pub tooltip: Option<RvNode>,
    pub toast: Option<RvNode>,
    /// 弹出菜单容器（menu.root）：背景色/图/层 + 边框 + shadow。
    /// 颜色语义与候选窗一致——节点显式值优先，未配回退 palette（menu_bg/menu_border）。
    pub menu_root: Option<RvNode>,
    /// 弹出菜单项（menu.item）：几何（padding/hover 圆角/字号偏移）+ 文字色，
    /// 含 hover/disabled 状态 patch（默认色分别取 menu_hover_bg/menu_hover_text、menu_disabled）。
    pub menu_item: Option<RvNode>,
    /// 菜单分隔线（menu.separator）：线色取 background.color，未配回退 menu_separator。
    pub menu_separator: Option<RvNode>,
    /// 菜单最小宽度（None→渲染层兜底 90）。
    pub menu_min_width: Option<Dim>,

    /// 仅 index：主题定义的序号槽位标签，一槽一项（≤10）。空槽/越界回退。
    /// 优先级由协调器裁决：用户配置 index_labels > 本字段 > 默认数字。
    pub index_labels: Vec<String>,

    // 列表级几何（V3-D 属性归位：从 candidate_list / window / accent_bar 节点读取）。
    pub item_spacing: Option<Dim>,
    pub window_gap: Option<Dim>,
    pub row_gap: Option<Dim>,
    pub accent_bar_enabled: bool,
    pub accent_bar_width: Option<Dim>,
    pub accent_bar_offset: Option<Dim>,
    /// 条高 = 行高 × 此比例。0=未经 resolve 填充，消费方须回退
    /// [`DEFAULT_ACCENT_BAR_HEIGHT_RATIO`]（resolve 期已保证落在 (0,1]）。
    pub accent_bar_height_ratio: f32,
    pub shadow_offset_x: Option<Dim>,
    pub shadow_offset_y: Option<Dim>,
    pub shadow_blur: Option<Dim>,
    pub shadow_spread: Option<Dim>,
    /// 仅模糊扩散层的额外偏移（叠加在 shadow_offset_x/y 之上）。
    pub shadow_spread_offset_x: Option<Dim>,
    pub shadow_spread_offset_y: Option<Dim>,
    pub shadow_color: Option<Rgba>,

    /// 候选窗相对光标的位置偏移（window.position_offset）。None=0。
    /// 正值恒为「远离光标」：下方定位向下推、上翻定位向上推。
    /// 仅跟随光标定位消费；固定位置/拖动不叠加。
    pub window_offset_x: Option<Dim>,
    pub window_offset_y: Option<Dim>,

    // 工具栏几何（None→toolbar.rs 内置默认；由主题 [toolbar] 描述，去渲染器硬编码）。
    pub toolbar_height: Option<Dim>,
    pub toolbar_grip_width: Option<Dim>,
    pub toolbar_button_width: Option<Dim>,
    pub toolbar_button_padding: Option<Dim>,
    pub toolbar_button_radius: Option<Dim>,
    /// 工具栏整体背景色 / 边框色（[toolbar] background、border.color）。
    /// ToolbarViews 不是 ViewNode（自带 background/border 字段），故不走 RvNode，
    /// 这里只提取两个色；背景图/渐变待补。None=未配，渲染层回退 toolbar_* token。
    pub toolbar_bg_color: Option<Rgba>,
    pub toolbar_border_color: Option<Rgba>,
    /// 工具栏整条外框圆角 / 线宽（[toolbar] border.radius、border.width）。
    /// None→toolbar.rs 内置默认（圆角 = 条高×0.30，线宽 = 1dp）。
    pub toolbar_border_radius: Option<Dim>,
    pub toolbar_border_width: Option<Dim>,
}

impl RvViews {
    /// 主题**显式声明**过的字族名，按节点路径列出（`views.text`、`views.item.selected`、
    /// `views.menu.item` …）。空 / 纯空白的声明不算「声明过」。
    ///
    /// # 为什么要能列出来
    ///
    /// 字族名写错在渲染层**没有任何信号**：DirectWrite 的 `SetFontFamilyName` 对不存在的
    /// 家族返回成功、静默回落默认字体，画面上只表现为「字体不对、字看着小了一圈」。
    /// `ui.font.family` 与方案级 `[candidate] font_family` 都已在设置那一刻查一次存在性并
    /// 记 warn（wind-ui `CandidateWindow::resolve_and_report`），主题节点这条一直漏着
    /// ——而主题恰恰最容易写错：作者自己机器上装着那款字体，换台机器就静默回落。
    ///
    /// 消费方只在**换主题时**走一遍（`CandidateWindow::set_theme`），不在渲染热路径：
    /// 每个字族一次 `FindFamilyName` 是按 COM 调用计费的，逐帧逐节点查等于按帧计费。
    pub fn declared_font_families(&self) -> Vec<(String, String)> {
        // 借 [`Self::font_nodes_mut`] 的节点表（只读场景克隆一份）：节点表只写一处，
        // 「告警查的节点」与「用户字体清掉的节点」才不会各自漂移。只在换主题 / 换字体时走，
        // 不在渲染热路径。
        let mut copy = self.clone();
        let mut out = Vec::new();
        for (path, n) in copy.font_nodes_mut() {
            collect_font_families(path, n, &mut out);
        }
        out
    }

    /// 清掉全部节点（含状态 patch）上的主题字族声明，让它们回落渲染器的全局字族。
    ///
    /// 用于「外观覆盖主题」：用户配了非空 `ui.font.family` 时，主题节点字族一律让位
    /// （优先级见 wind-ui `CandidateWindow::refresh_effective_theme`）。
    pub fn clear_font_families(&mut self) {
        for (_, n) in self.font_nodes_mut() {
            clear_node_font_family(n);
        }
    }

    /// 把全部节点（含状态 patch）声明的字族逐个交给 `resolve` 换成渲染端认得的名字。
    ///
    /// `resolve` 返回 `(family, 字重)`，字重 `0` = 名字里没带字重。存量主题里的旧 GDI face
    /// name（「思源宋体 SemiBold」）靠它拆成 family「思源宋体」+ 600（看板 A2-1）：
    /// 节点**自己没写** `font_weight`（0 = 继承）时才采用名字里的字重，写了的以节点为准。
    /// 用户 `ui.font.weight` 非 0 时名字字重应让位——由调用方让 `resolve` 返回字重 0 实现
    /// （见 wind-ui `CandidateWindow::refresh_effective_theme`）；节点显式字重另由
    /// [`Self::apply_user_font_weight`] 处理。
    pub fn resolve_font_families(&mut self, mut resolve: impl FnMut(&str) -> (String, i32)) {
        for (_, n) in self.font_nodes_mut() {
            resolve_node_font_family(n, &mut resolve);
        }
    }

    /// 以用户 `ui.font.weight`（非 0）为**基准字重**改写主题节点的显式字重（看板 A2-1）。
    ///
    /// - 基态节点、以及不比基态重的状态 patch，直接写成用户字重。不能清成 0：
    ///   wind-ui `eff_weight` 按「节点状态 → item 状态 → 节点基态 → item 基态」级联，
    ///   0 会让它越过本节点掉到别处的强调值上（如 `text.hover` 400 回落 `text` 700）；
    /// - 状态 patch 比基态重（如 `text.selected` 700）才算强调，取 max(主题值, 用户值)；
    ///   用户字重低于它时，选中等强调仍比普通粗。
    ///
    /// 状态 patch 比基态时，基态未写字重按常规 400 比较。`user <= 0`（不指定）时什么都不改。
    pub fn apply_user_font_weight(&mut self, user: i32) {
        if user <= 0 {
            return;
        }
        for (_, n) in self.font_nodes_mut() {
            apply_node_user_weight(n, user, None);
        }
    }

    /// 可能声明 `font_family` 的全部节点（可选节点未写则不列）。
    fn font_nodes_mut(&mut self) -> Vec<(&'static str, &mut RvNode)> {
        let mut nodes: Vec<(&'static str, &mut RvNode)> = vec![
            ("views.window", &mut self.window),
            ("views.preedit_bar", &mut self.preedit_bar),
            ("views.candidate_list", &mut self.candidate_list),
            ("views.item", &mut self.item),
            ("views.index", &mut self.index),
            ("views.text", &mut self.text),
            ("views.comment", &mut self.comment),
            // ⚠️ 必须排在 comment 之后：comment_above 的字族多半是从 comment 继承来的，缺字体告警
            // （wind-ui `warn_missing_theme_families`）按字族名去重、取表里第一个路径，
            // 这样告警指向用户真写了的 `views.comment`，而不是没写过的 `views.comment_above`。
            ("views.comment_above", &mut self.comment_above),
            ("views.accent_bar", &mut self.accent_bar),
            ("views.footer_bar", &mut self.footer_bar),
            ("views.mode_label", &mut self.mode_label),
        ];
        // 可选节点：主题没写就没有对应的键，列出来只会让 warn 指向一个不存在的路径。
        for (path, opt) in [
            ("views.status", &mut self.status),
            ("views.tooltip", &mut self.tooltip),
            ("views.toast", &mut self.toast),
            ("views.menu.root", &mut self.menu_root),
            ("views.menu.item", &mut self.menu_item),
            ("views.menu.separator", &mut self.menu_separator),
        ] {
            if let Some(n) = opt.as_mut() {
                nodes.push((path, n));
            }
        }
        nodes
    }
}

fn resolve_node_font_family(n: &mut RvNode, resolve: &mut dyn FnMut(&str) -> (String, i32)) {
    if let Some(f) = n
        .font_family
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        let (family, weight) = resolve(f);
        if weight > 0 && n.font_weight == 0 {
            n.font_weight = weight;
        }
        n.font_family = Some(family);
    }
    for sub in [&mut n.selected, &mut n.hover, &mut n.disabled]
        .into_iter()
        .flatten()
    {
        resolve_node_font_family(sub, resolve);
    }
}

/// `base` = 状态 patch 所依附基态的字重（基态未写按常规 400）；基态节点自身传 `None`。
///
/// 基态节点一律写成用户字重（主题写了 600 加粗，用户选「细」也得变细）；状态 patch 只有
/// 比基态更重才算强调（如选中 700），取 max(主题, 用户)，其余同样让位于用户字重。
fn apply_node_user_weight(n: &mut RvNode, user: i32, base: Option<i32>) {
    let own = n.font_weight;
    if own != 0 {
        n.font_weight = match base {
            Some(b) if own > b => own.max(user),
            _ => user,
        };
    }
    // 状态 patch 与主题原值比，而非改写后的值：改写后基态是用户字重，会让判定随用户值漂移。
    let sub_base = Some(if own != 0 { own } else { base.unwrap_or(400) });
    for sub in [&mut n.selected, &mut n.hover, &mut n.disabled]
        .into_iter()
        .flatten()
    {
        apply_node_user_weight(sub, user, sub_base);
    }
}

fn clear_node_font_family(n: &mut RvNode) {
    n.font_family = None;
    for sub in [&mut n.selected, &mut n.hover, &mut n.disabled]
        .into_iter()
        .flatten()
    {
        clear_node_font_family(sub);
    }
}

/// 递归收集一个节点及其状态 patch（selected / hover / disabled）声明的字族。
///
/// 状态 patch 不能漏：`[item.selected] font_family` 是「选中项换个字体」的正经写法，
/// 而它与基态用的是两个不同的名字——只查基态的话，选中那一行的字体照样静默回落。
fn collect_font_families(path: &str, n: &RvNode, out: &mut Vec<(String, String)>) {
    if let Some(f) = n.font_family.as_deref() {
        let f = f.trim();
        if !f.is_empty() {
            out.push((path.to_string(), f.to_string()));
        }
    }
    for (state, sub) in [
        ("selected", &n.selected),
        ("hover", &n.hover),
        ("disabled", &n.disabled),
    ] {
        if let Some(sub) = sub {
            collect_font_families(&format!("{path}.{state}"), sub, out);
        }
    }
}

#[cfg(test)]
mod font_family_tests {
    use super::*;

    fn node(family: Option<&str>) -> RvNode {
        RvNode {
            font_family: family.map(str::to_string),
            ..Default::default()
        }
    }

    /// 收集要覆盖三件事：必选节点与可选节点都算、状态 patch 递归进去、空声明不算。
    #[test]
    fn declared_font_families_covers_states_and_optional_nodes() {
        let mut v = RvViews {
            text: node(Some("思源宋体")),
            index: node(Some("  ")), // 纯空白 = 没声明
            comment: node(None),
            ..Default::default()
        };
        v.item = node(Some("Consolas"));
        v.item.selected = Some(Box::new(node(Some("Consolas Bold"))));
        v.item.hover = Some(Box::new(node(None)));
        v.menu_item = Some(node(Some("Segoe UI")));
        v.toast = Some(node(None));

        let got = v.declared_font_families();
        assert_eq!(
            got,
            vec![
                ("views.item".to_string(), "Consolas".to_string()),
                (
                    "views.item.selected".to_string(),
                    "Consolas Bold".to_string()
                ),
                ("views.text".to_string(), "思源宋体".to_string()),
                ("views.menu.item".to_string(), "Segoe UI".to_string()),
            ],
            "顺序按节点表 + 状态 patch 紧跟其基态；空白与 None 不该出现"
        );
    }

    #[test]
    fn clear_font_families_strips_every_declared_node_and_state() {
        let mut v = RvViews {
            text: node(Some("思源宋体")),
            footer_bar: node(Some("Consolas")),
            ..Default::default()
        };
        v.item.selected = Some(Box::new(node(Some("Consolas Bold"))));
        v.menu_item = Some(node(Some("Segoe UI")));
        assert!(!v.declared_font_families().is_empty());
        v.clear_font_families();
        assert!(
            v.declared_font_families().is_empty(),
            "清完仍有声明：节点表与告警表不同源"
        );
    }

    /// 旧 GDI face name 拆出的字重只在节点自己没写字重时采用；状态 patch 同样过一遍。
    #[test]
    fn resolve_font_families_rewrites_names_and_fills_only_unset_weights() {
        let mut v = RvViews {
            text: node(Some("思源宋体 SemiBold")),
            ..Default::default()
        };
        let mut sel = node(Some("思源宋体 SemiBold"));
        sel.font_weight = 800;
        v.item.selected = Some(Box::new(sel));
        v.resolve_font_families(|f| match f {
            "思源宋体 SemiBold" => ("思源宋体".to_string(), 600),
            other => (other.to_string(), 0),
        });
        assert_eq!(v.text.font_family.as_deref(), Some("思源宋体"));
        assert_eq!(v.text.font_weight, 600);
        let sel = v.item.selected.as_ref().unwrap();
        assert_eq!(sel.font_family.as_deref(), Some("思源宋体"));
        assert_eq!(sel.font_weight, 800, "节点显式写的字重不被名字里的字重覆盖");
        assert_eq!(v.comment.font_family, None, "没声明的节点不凭空长出字族");
    }

    /// 用户字重作基准：基态节点（含主题写的加粗）与不比基态重的状态 patch 直接写成用户字重；
    /// 比基态重的状态 patch 才算强调、取 max(主题, 用户)；可选节点与状态 patch 同样过。
    #[test]
    fn apply_user_font_weight_replaces_plain_and_raises_emphasis() {
        let w = |fw: i32| RvNode {
            font_weight: fw,
            ..Default::default()
        };
        let mut v = RvViews {
            text: w(500),
            index: w(700),
            comment: w(300),
            ..Default::default()
        };
        v.text.selected = Some(Box::new(w(550)));
        v.item.selected = Some(Box::new(w(500)));
        v.status = Some(w(400));
        v.apply_user_font_weight(450);
        assert_eq!(v.text.font_weight, 450, "普通字重让位于用户字重");
        assert_eq!(v.comment.font_weight, 450, "比用户细的普通字重同样让位");
        assert_eq!(v.index.font_weight, 450, "基态 700 也让位于用户字重");
        let ts = v.text.selected.as_ref().unwrap().font_weight;
        assert_eq!(ts, 550, "比基态 500 重即强调，取 max");
        let is = v.item.selected.as_ref().unwrap().font_weight;
        assert_eq!(is, 500, "基态未写（常规 400）时 500 也算强调");
        assert_eq!(v.status.as_ref().unwrap().font_weight, 450, "可选节点也过");
        assert_eq!(v.item.font_weight, 0, "未写的节点保持继承");

        let mut v = RvViews {
            index: w(700),
            ..Default::default()
        };
        v.apply_user_font_weight(900);
        assert_eq!(v.index.font_weight, 900, "用户更重时强调跟到用户字重");
        v.apply_user_font_weight(0);
        assert_eq!(v.index.font_weight, 900, "0 = 不指定，原样不动");
    }

    /// 普通节点必须写成用户字重而非清 0：清 0 会让 wind-ui `eff_weight` 的级联
    /// （节点状态 → item 状态 → 节点基态 → item 基态）越过它掉到强调值上。
    /// 以下三例按 own 判定：`text.hover` 400 不比基态 700 重、`text.selected` 400 不比
    /// 基态（未写按 400）重、`text` 400 < 600，都是普通，均应得用户字重 300。
    #[test]
    fn plain_weight_is_written_as_user_so_cascade_does_not_fall_to_emphasis() {
        let w = |fw: i32| RvNode {
            font_weight: fw,
            ..Default::default()
        };
        // text=700 + text.hover=400 ⇒ 悬停 300（hover patch 不再为 0 而回落基态 700）
        let mut v = RvViews {
            text: w(700),
            ..Default::default()
        };
        v.text.hover = Some(Box::new(w(400)));
        v.apply_user_font_weight(300);
        assert_eq!(v.text.font_weight, 300, "基态加粗也让位于用户字重");
        assert_eq!(v.text.hover.as_ref().unwrap().font_weight, 300);

        // item.selected=700 + text.selected=400 ⇒ 选中文字 300（不回落到 item.selected 700）
        let mut v = RvViews::default();
        v.item.selected = Some(Box::new(w(700)));
        v.text.selected = Some(Box::new(w(400)));
        v.apply_user_font_weight(300);
        assert_eq!(v.item.selected.as_ref().unwrap().font_weight, 700);
        assert_eq!(v.text.selected.as_ref().unwrap().font_weight, 300);

        // item=600 + text=400 ⇒ 候选文字 300（不回落到 item 基态 600）
        let mut v = RvViews {
            item: w(600),
            text: w(400),
            ..Default::default()
        };
        v.apply_user_font_weight(300);
        assert_eq!(v.item.font_weight, 300);
        assert_eq!(v.text.font_weight, 300);
    }

    /// 没配过字体的主题一条都不该报——否则每次换主题都白查一轮 COM。
    #[test]
    fn a_theme_without_font_family_declares_nothing() {
        assert!(RvViews::default().declared_font_families().is_empty());
    }
}
