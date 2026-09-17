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
    /// 仅覆盖图：九宫锚点。
    pub anchor: String,
    /// 仅覆盖图偏移：dp 或百分比（paint 期相对 host 求值）。
    pub offset_x: Option<Dim>,
    pub offset_y: Option<Dim>,
    /// 仅覆盖图尺寸（逻辑像素）；0=原尺寸。
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
    /// 仅 footer_bar：翻页箭头图/字符。
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
