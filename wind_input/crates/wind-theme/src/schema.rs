//! 类型化 v3 主题 schema（与 Go 版 `pkg/theme/views.go`/`behavior.go` 对齐）。
//!
//! 这是主题文件经 base 深合并后的 serde 解析目标（未求值的「原始 Theme」）。
//! 约定：`Option<T>` 的 None=未写（回退基线），Some（含 0/默认）=显式值——与 Go 的 `*T` 指针语义一致。
//!
//! 设计取舍（见 docs/redesign/theme-migration-plan.md「已定设计决策」）：
//! - `Dim`：dp/px/% 三态坍缩成单 enum（取代 Go 的 Dimension 双 Marshal 样板）。
//! - `Ld`：light/dark 原语，仅作**解析中间形态**；求值期 `select(is_dark)→单值`，不进 RVNode。
//! - 未知字段一律忽略（不 deny）：对编辑器新增字段前向兼容，配合 unwrap 兜底（warn 而非 fail）。
//! - `colors` 块保留为 `toml::Value`，由 palette 求值层消费（不在此处提前类型化派生/token）。

use serde::Deserialize;
use std::collections::HashMap;

/// 带单位几何尺寸。
/// - 裸数字 / `"Ndp"`：dp（密度无关像素，随 DPI scale 缩放）；
/// - `"Npx"`：设备像素（不随 DPI 缩放，如发丝边框）；
/// - `"N%"`：百分比（相对 host 宽/高，仅覆盖图/定位背景图偏移消费）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dim {
    Dp(f32),
    Px(f32),
    Pct(f32),
}

impl Dim {
    /// 求值为像素：dp×scale；px 直给；pct×host/100。
    pub fn resolve(self, scale: f32, host: f32) -> f32 {
        match self {
            Dim::Dp(v) => v * scale,
            Dim::Px(v) => v,
            Dim::Pct(v) => v / 100.0 * host,
        }
    }
}

fn parse_dim(s: &str) -> Result<Dim, String> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('%') {
        return n
            .trim()
            .parse::<f32>()
            .map(Dim::Pct)
            .map_err(|_| format!("bad percent dimension: {s:?}"));
    }
    if let Some(n) = s.strip_suffix("px") {
        return n
            .trim()
            .parse::<f32>()
            .map(Dim::Px)
            .map_err(|_| format!("bad px dimension: {s:?}"));
    }
    let n = s.strip_suffix("dp").unwrap_or(s).trim();
    n.parse::<f32>()
        .map(Dim::Dp)
        .map_err(|_| format!("bad dimension: {s:?}"))
}

impl<'de> Deserialize<'de> for Dim {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = toml::Value::deserialize(de)?;
        match v {
            toml::Value::Integer(n) => Ok(Dim::Dp(n as f32)),
            toml::Value::Float(n) => Ok(Dim::Dp(n as f32)),
            toml::Value::String(s) => parse_dim(&s).map_err(serde::de::Error::custom),
            _ => Err(serde::de::Error::custom(
                "dimension must be number or string",
            )),
        }
    }
}

/// light/dark 颜色/资源引用原语：标量=明暗共用；`{light, dark}`=分设（缺一侧回退另一侧）。
/// 仅作解析中间形态——求值期 `select(is_dark)` 坍缩为单值。
#[derive(Clone, Debug)]
pub enum Ld {
    Scalar(String),
    Variant {
        light: Option<String>,
        dark: Option<String>,
    },
}

impl Ld {
    /// 按 is_dark 选分支；缺侧回退另一侧；皆空返回 None。
    pub fn select(&self, is_dark: bool) -> Option<&str> {
        match self {
            Ld::Scalar(s) => Some(s.as_str()),
            Ld::Variant { light, dark } => {
                let (primary, fallback) = if is_dark {
                    (dark, light)
                } else {
                    (light, dark)
                };
                primary.as_deref().or(fallback.as_deref())
            }
        }
    }
}

impl<'de> Deserialize<'de> for Ld {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = toml::Value::deserialize(de)?;
        match v {
            toml::Value::String(s) => Ok(Ld::Scalar(s)),
            toml::Value::Table(m) => {
                let get = |k: &str| m.get(k).and_then(|x| x.as_str()).map(|s| s.to_string());
                // 非颜色映射（如 colors.derive {enabled, algorithm}）→ light/dark 皆 None，select 返回 None。
                Ok(Ld::Variant {
                    light: get("light"),
                    dark: get("dark"),
                })
            }
            _ => Ok(Ld::Scalar(String::new())),
        }
    }
}

/// 四向距离。None=未写（回退基线）；Some（含 0）=显式值。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct ViewEdges {
    pub top: Option<Dim>,
    pub right: Option<Dim>,
    pub bottom: Option<Dim>,
    pub left: Option<Dim>,
}

/// 九宫格中段的填充方式，分轴。值：`"stretch"`（默认，拉伸）| `"repeat"`（平铺）。
///
/// 分轴是必须的而不是讲究：横轴随候选窗宽度变化，纵轴一般固定。拿一个值管两轴的话，
/// 一张 985×255 的背景在 40px 高的编码条里会从「压扁到 40px」变成「只取顶部 40 行」，
/// 图的内容完全不同。
///
/// 人写形态支持简写，由 `normalize` 展开：`slice_repeat = "repeat"`（两轴同值）、
/// `slice_repeat = ["repeat", "stretch"]`（x, y）。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct SliceRepeat {
    pub x: Option<String>,
    pub y: Option<String>,
}

/// 边框：width 常用 `"1px"` 发丝线（不随 DPI 加粗）。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct ViewBorder {
    pub width: Option<Dim>,
    pub color: Option<Ld>,
    pub radius: Option<Dim>,
    /// 线型：solid（默认）| dashed | dotted。None/空=solid。
    pub style: Option<String>,
}

/// 覆盖图/定位背景图偏移；x/y 各支持 dp 或百分比（"N%"）。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct ViewPoint {
    pub x: Option<Dim>,
    pub y: Option<Dim>,
}

/// 覆盖图尺寸（逻辑像素）；0=原图尺寸。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct ViewSize {
    #[serde(default)]
    pub w: i32,
    #[serde(default)]
    pub h: i32,
}

/// 通用图片对象：背景填充图与 layers[] 覆盖图共用。
/// `ref` 优先查顶层 resources，否则按字面 path / data: URI 解析。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct ViewImage {
    #[serde(default, rename = "ref")]
    pub reference: String,
    /// nine_slice | stretch | tile | center；空=stretch。
    #[serde(default)]
    pub mode: String,
    /// 仅 nine_slice：源图四边切片像素。
    #[serde(default)]
    pub slice: ViewEdges,
    /// 仅 nine_slice：中段沿该轴**平铺**而非拉伸。
    #[serde(default)]
    pub slice_repeat: SliceRepeat,
    pub opacity: Option<f32>,
    /// 仅 layers[]：内容基准 0，<0 在内容下、>0 在上。
    #[serde(default)]
    pub z: i32,
    /// 仅覆盖图：top-left | top | … | center | … | bottom-right。
    #[serde(default)]
    pub anchor: String,
    #[serde(default)]
    pub offset: ViewPoint,
    #[serde(default)]
    pub size: ViewSize,
    /// 单色染色：非空=把图当 alpha mask、用此色填充（单色图标随主题变色；SVG 现场栅格化）。
    pub tint: Option<Ld>,
    /// 禁用态染色（仅 footer 翻页箭头消费）。
    pub disabled_tint: Option<Ld>,
}

/// 渐变色停。
#[derive(Deserialize, Debug, Clone)]
pub struct ViewGradientStop {
    pub color: Ld,
    #[serde(default)]
    pub pos: f32,
}

/// 渐变填充：linear（默认，按 angle 方向）| radial（圆心=矩形中心）。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct ViewGradient {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub angle: f32,
    #[serde(default)]
    pub stops: Vec<ViewGradientStop>,
}

/// 背景填充：底色 + 可选渐变 + 可选图（依次叠加，裁到圆角内）。优先级：底色 < 渐变 < 图。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct ViewFill {
    pub color: Option<Ld>,
    /// 背景形状："circle" | "none"（空=none）。当前仅 views.index 消费。
    #[serde(default)]
    pub shape: String,
    pub image: Option<ViewImage>,
    pub gradient: Option<ViewGradient>,
}

/// 结构化窗口投影。blur 经高斯软影渲染；spread 扩张阴影盒。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct ViewShadow {
    pub offset_x: Option<Dim>,
    pub offset_y: Option<Dim>,
    pub blur: Option<Dim>,
    pub spread: Option<Dim>,
    /// 仅作用于模糊扩散层的额外偏移（叠加在 offset_x/offset_y 之上）。
    pub spread_offset_x: Option<Dim>,
    pub spread_offset_y: Option<Dim>,
    pub color: Option<Ld>,
}

/// 一个具名 View 的外观属性（盒模型 + 文本 + 仅特定节点字段）。
/// 状态 patch（selected/hover/disabled）递归本类型——但渲染消费只合并色/图/边框/字体/层，
/// **不合并几何**（padding/margin/font_size），避免候选框跳动（state_geometry unsupported）。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct ViewNode {
    #[serde(default)]
    pub margin: ViewEdges,
    #[serde(default)]
    pub padding: ViewEdges,
    #[serde(default)]
    pub background: ViewFill,
    #[serde(default)]
    pub border: ViewBorder,
    pub font_family: Option<String>,
    /// 相对主候选字体的有符号偏移（逻辑 px）：-4=base-4；None/0=同主字体。
    pub font_size: Option<i32>,
    pub font_weight: Option<i32>,
    pub color: Option<Ld>,
    /// 仅 index：序号槽位标签，一槽一项（≤10）。**每项是字符串不是字符**——
    /// `(1)`、罗马数字、带 ZWJ 的组合 emoji 都是多 char 的合法标签。
    #[serde(default)]
    pub labels: Vec<String>,
    /// z 层级覆盖图。
    #[serde(default)]
    pub layers: Vec<ViewImage>,

    // 仅特定节点字段（无关节点为 None，不消费）：
    /// 窗口投影（window / status / tooltip / toast）。
    pub shadow: Option<ViewShadow>,
    /// 仅 window：候选窗相对光标的位置偏移（dp，随 DPI 缩放）。
    /// 正值把窗口推离光标——y 在下方定位时向下、上翻定位时向上，语义恒为「远离」。
    /// 供边缘带装饰（发光边/外描边/九宫格留白）的主题拉开与光标的观感距离。
    /// **仅作用于跟随光标定位**；固定位置与用户拖动是显式意图，不叠加。
    pub position_offset: Option<ViewPoint>,
    /// 仅 candidate_list：横排候选项横向间距基数。
    pub gap: Option<Dim>,
    /// 仅 candidate_list：band 间距。
    pub band_gap: Option<Dim>,
    /// 仅 candidate_list：竖排行间距（None/0=紧贴）。
    pub row_gap: Option<Dim>,
    /// 仅 accent_bar：是否绘制选中候选左侧强调条。
    pub enabled: Option<bool>,
    /// 仅 accent_bar：条宽。
    pub width: Option<Dim>,
    /// 仅 accent_bar：左缘偏移。
    pub offset: Option<Dim>,
    /// 仅 accent_bar：条高 = ItemHeight × 此比例。
    pub height_ratio: Option<f32>,
    /// 仅 footer_bar：上/下翻页箭头图（可 SVG + tint 随主题变色）。
    pub prev_image: Option<ViewImage>,
    pub next_image: Option<ViewImage>,
    /// 仅 footer_bar：上/下翻页字符（None=内置 ❮/❯）；未配图时生效。
    pub prev_char: Option<String>,
    pub next_char: Option<String>,
    /// 多行/多列布局间距（tooltip/toast 专有；None=渲染层兜底）。
    pub line_spacing: Option<Dim>,
    pub col_gap: Option<Dim>,
    pub title_gap: Option<Dim>,

    // 状态态 patch（递归）。
    pub selected: Option<Box<ViewNode>>,
    pub hover: Option<Box<ViewNode>>,
    pub disabled: Option<Box<ViewNode>>,
}

/// 工具栏 schema：button base + mode 状态覆盖 + settings 齿轮色 + 几何（None=内置默认）。
#[derive(Deserialize, Debug, Default)]
pub struct ToolbarViews {
    #[serde(default)]
    pub background: ViewFill,
    #[serde(default)]
    pub border: ViewBorder,
    pub height: Option<Dim>,
    pub grip_width: Option<Dim>,
    pub button_width: Option<Dim>,
    pub button_padding: Option<Dim>,
    pub button_radius: Option<Dim>,
    #[serde(default)]
    pub grip: ViewNode,
    #[serde(default)]
    pub button: ToolbarButton,
    #[serde(default)]
    pub settings: ToolbarSettings,
}

#[derive(Deserialize, Debug, Default)]
pub struct ToolbarButton {
    #[serde(default)]
    pub background: ViewFill,
    pub color: Option<Ld>,
    #[serde(default)]
    pub border: ViewBorder,
    pub mode: Option<ToolbarModeStates>,
}

#[derive(Deserialize, Debug, Default)]
pub struct ToolbarModeStates {
    #[serde(default)]
    pub chinese: ViewNode,
    #[serde(default)]
    pub english: ViewNode,
}

#[derive(Deserialize, Debug, Default)]
pub struct ToolbarSettings {
    #[serde(default)]
    pub background: ViewFill,
    #[serde(default)]
    pub icon: ViewFill,
    #[serde(default)]
    pub hole: ViewFill,
}

/// 弹出菜单 schema：root 容器 + item（含 hover/disabled patch）+ separator 线色。
#[derive(Deserialize, Debug, Default)]
pub struct MenuViews {
    #[serde(default)]
    pub root: ViewNode,
    #[serde(default)]
    pub item: ViewNode,
    #[serde(default)]
    pub separator: ViewNode,
    /// 菜单最小宽度（dp；None→渲染层兜底 90）。
    pub min_width: Option<Dim>,
}

/// 具名 View 集合（固定骨架）。候选窗各节点 + 其它窗口（status/tooltip/toast/toolbar/menu）。
#[derive(Deserialize, Debug, Default)]
pub struct Views {
    #[serde(default)]
    pub window: ViewNode,
    #[serde(default)]
    pub preedit_bar: ViewNode,
    #[serde(default)]
    pub candidate_list: ViewNode,
    #[serde(default)]
    pub item: ViewNode,
    #[serde(default)]
    pub index: ViewNode,
    #[serde(default)]
    pub text: ViewNode,
    #[serde(default)]
    pub comment: ViewNode,
    #[serde(default)]
    pub accent_bar: ViewNode,
    #[serde(default)]
    pub footer_bar: ViewNode,
    #[serde(default)]
    pub mode_label: ViewNode,
    pub status: Option<ViewNode>,
    pub tooltip: Option<ViewNode>,
    pub toast: Option<ViewNode>,
    pub toolbar: Option<ToolbarViews>,
    pub menu: Option<MenuViews>,
}

/// 主题元信息。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct Meta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub order: i32,
}

/// 行为配置（用户可覆盖白名单）。None=未指定，走引擎基线。
#[derive(Deserialize, Debug, Default, Clone)]
pub struct Behavior {
    pub font_size: Option<i32>,
    pub always_show_pager: Option<bool>,
    pub show_page_number: Option<bool>,
    pub hide_pager: Option<bool>,
    /// 竖排候选窗内容宽度上限，单位 **dp**（跟 `min_window_width_*` 等字段一致，渲染时按当前
    /// 显示器 DPI 换算成设备 px）。0 或未配 = 不限。仅竖排布局生效；跟屏幕边界无关的另一层
    /// 安全钳制（不越出显示器）由渲染层恒定施加，不受本字段控制。
    pub vertical_max_width: Option<i32>,
    /// 独立翻页栏行的水平对齐：left/center/right（默认 center）。仅竖排独立翻页栏行生效；
    /// 翻页栏并入编码栏时强制右对齐，不受此项影响。
    pub pager_align: Option<String>,
}

/// 经 base 深合并后的「原始 Theme」（未求值）。
/// `colors` 保留为 `toml::Value`，由 palette 求值层消费；`resources` 名→ref（{light,dark} 或标量）。
#[derive(Deserialize, Debug, Default)]
pub struct Theme {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub colors: Option<toml::Value>,
    #[serde(default)]
    pub views: Option<Views>,
    #[serde(default)]
    pub behavior: Option<Behavior>,
    #[serde(default)]
    pub resources: HashMap<String, Ld>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{load_typed, load_typed_dirs};
    use std::path::Path;

    fn data_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes")
    }

    fn testdata_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/themes")
    }

    #[test]
    fn test_dim_parse() {
        assert_eq!(parse_dim("8"), Ok(Dim::Dp(8.0)));
        assert_eq!(parse_dim("8dp"), Ok(Dim::Dp(8.0)));
        assert_eq!(parse_dim("1px"), Ok(Dim::Px(1.0)));
        assert_eq!(parse_dim("50%"), Ok(Dim::Pct(50.0)));
        assert!(parse_dim("nope").is_err());
        // 求值语义
        assert_eq!(Dim::Dp(8.0).resolve(2.0, 100.0), 16.0); // dp×scale
        assert_eq!(Dim::Px(1.0).resolve(2.0, 100.0), 1.0); // px 直给
        assert_eq!(Dim::Pct(50.0).resolve(2.0, 100.0), 50.0); // pct×host
    }

    #[test]
    fn test_ld_select() {
        let s = Ld::Scalar("#FFF".into());
        assert_eq!(s.select(false), Some("#FFF"));
        assert_eq!(s.select(true), Some("#FFF"));
        let v = Ld::Variant {
            light: Some("#FFF".into()),
            dark: Some("#000".into()),
        };
        assert_eq!(v.select(false), Some("#FFF"));
        assert_eq!(v.select(true), Some("#000"));
        // 缺侧回退另一侧
        let only_light = Ld::Variant {
            light: Some("#FFF".into()),
            dark: None,
        };
        assert_eq!(only_light.select(true), Some("#FFF"));
        // derive {enabled, algorithm} 形态 → 皆 None
        let derive: Ld = toml::from_str("enabled = true\nalgorithm = \"hsl-shift\"").unwrap();
        assert_eq!(derive.select(false), None);
    }

    #[test]
    fn test_all_builtin_themes_typecheck() {
        let dir = data_dir();
        for name in ["_base", "default", "msime"] {
            let t = load_typed(&dir, name).unwrap_or_else(|e| panic!("load {name}: {e}"));
            assert!(t.views.is_some(), "{name} 应有 views");
            assert!(t.colors.is_some(), "{name} 应有 colors");
        }
    }

    #[test]
    fn test_jidian_classic_rich_features() {
        let t = load_typed_dirs(&[testdata_dir(), data_dir()], "jidian-classic")
            .expect("load jidian-classic");
        // base 继承 _base → colors 合并进来
        assert!(t.colors.is_some(), "应从 _base 继承 colors");
        // resources：panel/sel 双变体 + mark 标量
        let panel = t.resources.get("panel").expect("panel resource");
        assert_eq!(panel.select(false), Some("panel.png"));
        assert_eq!(panel.select(true), Some("panel-dark.png"));
        assert_eq!(
            t.resources.get("mark").and_then(|m| m.select(false)),
            Some("mark.png")
        );

        let views = t.views.as_ref().expect("views");
        // window 九宫格背景图
        let bg = views
            .window
            .background
            .image
            .as_ref()
            .expect("window bg image");
        assert_eq!(bg.reference, "panel");
        assert_eq!(bg.mode, "nine_slice");
        assert_eq!(bg.slice.top, Some(Dim::Dp(8.0)));
        // window 阴影 blur
        let sh = views.window.shadow.as_ref().expect("window shadow");
        assert_eq!(sh.blur, Some(Dim::Dp(8.0)));
        // window layers：右下角水印 z=1
        assert_eq!(views.window.layers.len(), 1);
        let mark = &views.window.layers[0];
        assert_eq!(mark.reference, "mark");
        assert_eq!(mark.z, 1);
        assert_eq!(mark.anchor, "bottom-right");
        assert_eq!(mark.offset.x, Some(Dim::Dp(-8.0))); // 负偏移内缩
        assert_eq!(mark.opacity, Some(0.9));
        // item 选中态：背景图 + 字重（状态 patch）
        let sel = views.item.selected.as_ref().expect("item.selected");
        assert_eq!(sel.font_weight, Some(600));
        assert_eq!(
            sel.background.image.as_ref().map(|i| i.reference.as_str()),
            Some("sel")
        );
        // accent_bar 启用 + 宽度
        assert_eq!(views.accent_bar.enabled, Some(true));
        assert_eq!(views.accent_bar.width, Some(Dim::Dp(3.0)));
        // 其它窗口也吃九宫格 + 水印
        let status = views.status.as_ref().expect("status view");
        assert!(status.background.image.is_some());
        assert_eq!(status.layers.len(), 1);
        assert!(views.menu.is_some());
        assert!(views.toast.is_some());
    }
}
