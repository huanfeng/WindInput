//! 精简 View 盒模型（measure → arrange → paint + 命中矩形提取）
//!
//! 与 Go 版本 `wind_input/internal/ui/viewbox*.go` 的核心子集对齐：
//! row/column 布局、padding/margin、背景/圆角/边框、固定尺寸、交叉轴对齐、
//! 文本叶子。布局分三步——measure 自底向上算尺寸，arrange 自顶向下定坐标，
//! paint 递归绘制；arrange 后每个带 tag 的节点都有绝对矩形，供鼠标命中复用。
//!
//! 九宫格图 / 阴影模糊 / z 分层 / 渐变背景均已支持。

use crate::text::dwrite::{TextRenderer, TextStyle};
use std::cell::RefCell;
use std::collections::HashMap;
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Paint, PathBuilder, Pattern,
    PixmapMut, PixmapPaint, Point, RadialGradient, SpreadMode, Transform,
};
use wind_theme::schema::Dim;

/// 阴影蒙版缓存键：(盒宽, 盒高, 圆角, 模糊, 亚像素相位 x, 亚像素相位 y)。
///
/// 各项量化到 1/4 px：阴影经高斯模糊后，1/4 px 的几何差异不可见，而量化能吸收
/// 浮点噪音，让几何不变的相邻帧稳定命中——否则 `spread`/`radius` 经 DPI 换算后的
/// 末位抖动就足以让缓存永不命中，白付一次哈希。
type ShadowKey = (i32, i32, i32, i32, i32, i32);

/// 蒙版量化因子（1/4 px）。
const SHADOW_Q: f32 = 4.0;

/// 蒙版缓存条数上限。候选窗的阴影几何种类有限（随候选数/码长变化），超限整体清空即可。
const SHADOW_CACHE_CAP: usize = 32;

/// 模糊后的阴影 alpha 蒙版 + 尺寸与四周留边（`pad`）。
struct ShadowMask {
    alpha: Vec<u8>,
    w: i32,
    h: i32,
    pad: i32,
}

thread_local! {
    /// 背景图解码/填充缓存（UI 单线程，跨帧复用，避免每帧解码）。
    static IMAGE_CACHE: RefCell<crate::image_cache::ImageCache> =
        RefCell::new(crate::image_cache::ImageCache::new());

    /// 阴影蒙版缓存（UI 单线程，跨帧复用）。
    ///
    /// 缓存 **alpha 蒙版而非着色后的像素**：蒙版只由几何决定、与阴影颜色无关，于是
    /// 主题明暗切换、阴影调色都不会让它失效，内存也只要 1/4（1 字节/像素 vs BGRA）。
    ///
    /// 这层缓存省掉的是每帧 3 趟可分离方框模糊——400×120 的窗口配 blur=8，临时缓冲
    /// 约 460×180，三轮双向模糊就是 ~50 万像素的 6 遍扫描，而候选窗在一次输入过程中
    /// 尺寸高度重复（同码长、同候选数），几乎帧帧都在重算同一张图。
    static SHADOW_CACHE: RefCell<HashMap<ShadowKey, ShadowMask>> = RefCell::new(HashMap::new());
}

/// 以模糊阴影蒙版调用 `f`（命中缓存则复用，否则构建并入缓存）。
///
/// 传闭包而非返回蒙版，是为了避开一次 Vec 克隆——蒙版有几十上百 KB，克隆的 memcpy
/// 虽比重算模糊便宜，但既然只在闭包内读一次，就没有拷贝的理由。
fn with_shadow_mask<R>(
    bw: f32,
    bh: f32,
    radius: f32,
    blur: f32,
    phase_x: f32,
    phase_y: f32,
    f: impl FnOnce(&ShadowMask) -> R,
) -> Option<R> {
    let q = |v: f32| (v * SHADOW_Q).round() as i32;
    let key = (q(bw), q(bh), q(radius), q(blur), q(phase_x), q(phase_y));
    SHADOW_CACHE.with(|c| {
        if let Some(m) = c.borrow().get(&key) {
            return Some(f(m));
        }
        let mask = build_shadow_mask(bw, bh, radius, blur, phase_x, phase_y)?;
        let mut cache = c.borrow_mut();
        if cache.len() >= SHADOW_CACHE_CAP {
            cache.clear();
        }
        Some(f(cache.entry(key).or_insert(mask)))
    })
}

/// 仅测试可见：当前阴影蒙版缓存条目数。
#[cfg(test)]
fn shadow_cache_len() -> usize {
    SHADOW_CACHE.with(|c| c.borrow().len())
}

/// 仅测试可见：清空阴影蒙版缓存，让用例从确定状态起步。
#[cfg(test)]
fn shadow_cache_clear() {
    SHADOW_CACHE.with(|c| c.borrow_mut().clear());
}

/// 构建模糊阴影蒙版：画 alpha=255 的圆角矩形 → 抽 alpha 通道 → 3 次方框模糊逼近高斯。
///
/// `phase_*` 为亚像素相位（`bx - bx.floor()`）：蒙版按相位构建才能保住边缘 AA，
/// 故相位也是缓存键的一部分。候选窗坐标恒为整数，实际相位恒 0。
fn build_shadow_mask(
    bw: f32,
    bh: f32,
    radius: f32,
    blur: f32,
    phase_x: f32,
    phase_y: f32,
) -> Option<ShadowMask> {
    // 3 次方框模糊级联 sigma ≈ sqrt(blur*(blur+2))，3-sigma 需约 3×sigma px 衰减到透明。
    let sigma = (blur * (blur + 2.0)).max(0.0).sqrt();
    let pad = (3.0 * sigma).ceil() as i32 + 2;
    let tmp_w = bw.ceil() as i32 + 2 * pad;
    let tmp_h = bh.ceil() as i32 + 2 * pad;
    if tmp_w < 1 || tmp_h < 1 {
        return None;
    }
    // 临时盒内阴影左上（保留亚像素偏移维持 AA）
    let local_x = pad as f32 + phase_x;
    let local_y = pad as f32 + phase_y;

    let mut tmp = vec![0u8; (tmp_w * tmp_h * 4) as usize];
    {
        let mut pm = PixmapMut::from_bytes(&mut tmp, tmp_w as u32, tmp_h as u32)?;
        let path = round_rect_path(local_x, local_y, bw, bh, radius.max(0.0))?;
        // 蒙版只取 alpha 通道，填充色任意；用全黑与原实现保持一致。
        let paint = aa_paint([0, 0, 0, 255]);
        pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    // 提取 alpha 通道 → 3× 方框模糊
    let n = (tmp_w * tmp_h) as usize;
    let mut alpha = vec![0u8; n];
    for (i, a) in alpha.iter_mut().enumerate() {
        *a = tmp[i * 4 + 3];
    }
    let r = blur.round() as i32;
    if r > 0 {
        for _ in 0..3 {
            box_blur_alpha(&mut alpha, tmp_w, tmp_h, r);
        }
    }
    Some(ShadowMask {
        alpha,
        w: tmp_w,
        h: tmp_h,
        pad,
    })
}

/// 背景填充图（已解析路径 + 模式）。slice 为源图四边切片像素 [上,右,下,左]。
#[derive(Clone, Debug)]
pub struct ViewImage {
    pub path: String,
    pub mode: String,
    pub slice: [f32; 4],
    pub opacity: f32,
    /// 单色染色（None=图原样）；非 None 时把图当 alpha mask、用此色填充（单色 SVG/图标随主题变色）。
    pub tint: Option<[u8; 4]>,
}

/// z 层级覆盖图（按 anchor 九宫定位 + offset + size 绘于 host 内）。
#[derive(Clone, Debug)]
pub struct ViewLayer {
    pub path: String,
    pub z: i32,
    pub anchor: String,
    /// dp 偏移（已 ×scale，px）。
    pub off_x: f32,
    pub off_y: f32,
    /// 百分比偏移（相对 host 宽/高；paint 期换算）。与 dp 偏移叠加。
    pub off_x_pct: f32,
    pub off_y_pct: f32,
    /// 目标尺寸 px（0=原图尺寸）。
    pub w: f32,
    pub h: f32,
    pub opacity: f32,
}

/// 背景渐变（叠在底色之上、背景图之下，裁到圆角内）。
/// linear 按 angle 方向铺色；radial 以节点中心为圆心。stops 为 (RGBA 直通, pos∈[0,1])。
#[derive(Clone, Debug)]
pub struct ViewGradient {
    pub radial: bool,
    /// 线性角度（度）：0=左→右，顺时针增大。radial 时忽略。
    pub angle: f32,
    pub stops: Vec<([u8; 4], f32)>,
}

/// 四边内/外边距
#[derive(Clone, Copy, Default)]
pub struct Edges {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Edges {
    pub fn all(v: f32) -> Self {
        Self {
            l: v,
            t: v,
            r: v,
            b: v,
        }
    }
    pub fn xy(x: f32, y: f32) -> Self {
        Self {
            l: x,
            t: y,
            r: x,
            b: y,
        }
    }
    fn w(&self) -> f32 {
        self.l + self.r
    }
    fn h(&self) -> f32 {
        self.t + self.b
    }
}

/// 主轴方向
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Row,
    Column,
}

/// 对齐方式（交叉轴 / 文本水平）
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}

/// 绝对矩形（arrange 后填充）
#[derive(Clone, Copy, Default, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// 点是否落在矩形内
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

/// 左侧强调条参数（选中候选的竖条）。**覆盖层语义**：不参与 measure/布局，仅 paint 期
/// 在节点左内边距内绘制。四个字段里三个是 `f32`，故用具名结构体而非元组——位置写反
/// 编译器抓不到。
#[derive(Clone, Copy, Debug)]
pub struct LeftBar {
    pub color: [u8; 4],
    /// 条宽（设备像素，调用方按 DPI 缩放后传入）。
    pub width: f32,
    /// 条高 = 节点高 × 此比例，结果钳到 ≥2px。主题 `accent_bar.height_ratio`，默认 0.6。
    pub height_ratio: f32,
    /// 左缘偏移（设备像素）：正值把条向右推离节点左缘。主题 `accent_bar.offset`，默认 0。
    pub offset: f32,
}

/// 子树旋转方向（见 [`View::rot`]）。
///
/// 做成枚举而不是两个 `bool`：`rotate_cw && rotate_ccw` 是无意义状态，枚举让它压根表达不出来。
/// 只有 ±90° 两向——180° 用不到，加进来反而要求映射函数处理宽高**不**交换的情形。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Rot {
    /// 不旋转（绝大多数节点）。
    #[default]
    None,
    /// 顺时针 90°：局部左上角落到屏幕右上角 ⇒ 一行从左到右的文字转成一列从上到下。
    Cw,
    /// 逆时针 90°：局部左上角落到屏幕左下角。单独用没意义，它的用途是**抵消**外层的
    /// [`Rot::Cw`]，把整项旋转里的单个字扶正（对联式竖排）。
    Ccw,
}

/// 一个视图节点（容器或文本叶子）
pub struct View {
    pub layout: Layout,
    pub margin: Edges,
    pub padding: Edges,
    pub gap: f32,
    pub cross_align: Align,
    /// 主轴对齐：内容总长小于容器主轴长度时，富余空间落在哪一端（默认 `Start`＝末端留空）。
    ///
    /// 只有存在富余时才有意义，而容器的主轴长度本就由内容决定——富余只可能来自
    /// `fixed_h`/`min_h`（Column）或 `fixed_w`/`min_w`（Row）。候选窗根容器上翻显示时
    /// 用 `End`：窗口最小高度撑出的空白必须落在**顶部**，否则贴光标的底边被空白顶开。
    ///
    /// 与 `grow` 互斥：有 `grow` 子节点时富余已被它们吸收，本项自然失效。
    pub main_align: Align,
    pub fixed_w: Option<f32>,
    pub fixed_h: Option<f32>,
    /// 宽度下限（设备像素）：测得宽度不足时抬到此值，超出则按内容。
    ///
    /// 与 `fixed_w` 的区别是「不得窄于」而非「就是这么宽」——候选窗的根容器用它施加
    /// 窗口最小宽度（`ui.candidate.min_window_width_horizontal` / `_vertical`，dp×scale）。
    ///
    /// ★ 撑出的富余空间**在 `arrange` 阶段**才分配：`fill_cross` 子节点会跟着撑到新的
    /// 内容宽（竖排候选高亮因此自动铺满窗口），非 `fill_cross` 的子节点按 `cross_align`
    /// 落位（默认 `Start`＝左对齐，右侧留空）。故调用方给根容器加下限即可，无需逐层接线。
    pub min_w: Option<f32>,
    /// 高度下限（设备像素）：测得高度不足时抬到此值，超出则按内容。与 `min_w` 对称。
    ///
    /// 候选窗根容器用它施加窗口最小高度（`ui.candidate.min_window_height_*`，dp×scale）。
    ///
    /// ★ 富余高度**默认落在主轴末端**（Column 的底部）：子节点从 `cy0` 依次排下，排完
    /// 即止。要把富余顶到另一端，在首个子节点位置插一个 `grow` 节点（`View::spacer()`）
    /// 吸收——候选窗上翻时正是这么做的，否则贴光标的底边会被空白顶开。
    pub min_h: Option<f32>,
    pub bg: Option<[u8; 4]>,
    pub corner_radius: f32,
    /// 边框 (颜色, 宽度)
    pub border: Option<([u8; 4], f32)>,
    pub text: Option<String>,
    pub text_color: [u8; 4],
    /// 文本字号（设备像素）；None=用渲染器基准字号。序号/注释按相对偏移设具体值。
    pub font_size: Option<f32>,
    /// 文本字重（400/500/700…）；None/0=继承渲染器默认（NORMAL）。
    pub font_weight: Option<i32>,
    /// 文本字体族覆盖；None/空=用渲染器全局字体族。
    pub font_family: Option<String>,
    pub text_align: Align,
    /// 文本内插入符位置（字节偏移，恒在字符边界；`None`=不画）。
    ///
    /// **覆盖层语义**：只在 paint 阶段按偏移画一条竖线，**不参与 measure/布局**——文本始终作为
    /// 整串一次性整形，故宽度与光标位置无关。这是系统 IME 的做法；若改成把文本拆成
    /// 「前半 + 竖线 + 后半」三节点，`measure(a+b) != measure(a)+measure(b)`（字距在拆分边界
    /// 丢失、每段各自亚像素舍入），拆分点随光标移动 → 整串宽度抖动 → 字符位移。
    pub caret_at: Option<usize>,
    /// 插入符竖线宽度（像素；调用方按 DPI 缩放传入）。
    pub caret_w: f32,
    /// 左侧强调条：在节点左缘内绘制竖条（选中候选用）；不占布局空间（落在左内边距内）。
    pub left_bar: Option<LeftBar>,
    /// 圆形背景色：在节点中心画真圆（直径=min(w,h)）；序号圆圈用，替代圆角矩形药丸近似。
    pub circle_bg: Option<[u8; 4]>,
    /// 背景填充图（叠在底色之上，裁到圆角内）。
    pub bg_image: Option<ViewImage>,
    /// 背景渐变（叠在底色之上、背景图之下，裁到圆角内）。
    pub bg_gradient: Option<ViewGradient>,
    /// z 层级覆盖图（z<0 在内容下、z>0 在内容上）。
    pub layers: Vec<ViewLayer>,
    pub children: Vec<View>,
    /// 弹性占位：主轴方向吸收容器剩余空间（用于把后续子节点推到末端，如菜单 ▸ 右对齐）。
    pub grow: bool,
    /// 跨轴填充：在父容器排布时把本节点撑满父内容的跨轴尺寸（Column→宽度），
    /// 供其内部 spacer 实现右对齐（如 preedit 栏让模式标记贴右）。
    pub fill_cross: bool,
    /// 命中标识：>=0 参与命中收集（如候选下标 / 按钮 id），<0 忽略
    pub tag: i32,
    /// 旋转 90° 呈现整棵子树（蒙古文等纵向书写的脚本用）。
    ///
    /// # 形态约束（只支持这一种，越界即 debug_assert）
    ///
    /// 本节点必须是**恰好一个子节点、无自身装饰**的裸包裹层：背景/边框/图层一律挂在
    /// 子节点上（它们会跟着一起转），本节点只负责坐标系变换。这样旋转的数学只有一处、
    /// 且不必回答「装饰是转前画还是转后画」这个没有正确答案的问题。
    ///
    /// # 为什么是「临时缓冲双向旋转」而不是让文本后端转
    ///
    /// ⛔ 走 DirectWrite 的 `SetCurrentTransform` 只能转**文字**，转不了背景/边框/序号圆圈；
    /// 而且差分法回写的包围盒要在旋转空间重算，等于把已经稳定的合成路径再撬一遍。
    /// 临时缓冲是纯像素操作 ⇒ macOS 的 CoreText 后端与 HostRender 的 SHM 帧路径自动跟上，
    /// 不用各写一份。
    ///
    /// ⚠️ 已知代价：子树画进临时缓冲时文本后端的表面尺寸会与窗口尺寸交替，
    /// 每帧多一次 `CreateBitmapRenderTarget`。真机若测出可观占比，再考虑把临时缓冲
    /// 固定成窗口大小、子树画在其一角。
    ///
    /// # 两向都要，且允许嵌套
    ///
    /// 「文字直立的竖排」（对联式）正是**外层顺时针 + 每个字逆时针**：外层把一行拆成一列，
    /// 内层把每个字转回正的。两次旋转都是无损的整像素搬运，且 [`View::paint_rotated`]
    /// 会把底子搬进临时缓冲再搬回，嵌套时内层看到的仍是正确的背景（差分法合成要用它）。
    pub rot: Rot,
    // 计算结果
    mw: f32,
    mh: f32,
    rect: Rect,
}

impl Default for View {
    fn default() -> Self {
        Self {
            layout: Layout::Row,
            margin: Edges::default(),
            padding: Edges::default(),
            gap: 0.0,
            cross_align: Align::Start,
            main_align: Align::Start,
            fixed_w: None,
            fixed_h: None,
            min_w: None,
            min_h: None,
            bg: None,
            caret_at: None,
            caret_w: 1.0,
            corner_radius: 0.0,
            border: None,
            text: None,
            text_color: [0, 0, 0, 255],
            font_size: None,
            font_weight: None,
            font_family: None,
            text_align: Align::Start,
            left_bar: None,
            circle_bg: None,
            bg_image: None,
            bg_gradient: None,
            layers: Vec::new(),
            children: Vec::new(),
            grow: false,
            fill_cross: false,
            tag: -1,
            rot: Rot::None,
            mw: 0.0,
            mh: 0.0,
            rect: Rect::default(),
        }
    }
}

impl View {
    /// 文本叶子
    pub fn leaf(text: impl Into<String>, color: [u8; 4]) -> Self {
        Self {
            text: Some(text.into()),
            text_color: color,
            ..Default::default()
        }
    }

    /// 容器
    pub fn container(layout: Layout) -> Self {
        Self {
            layout,
            ..Default::default()
        }
    }

    /// 弹性占位：主轴吸收剩余空间，把其后的兄弟节点推到容器末端。
    pub fn spacer() -> Self {
        Self {
            grow: true,
            ..Default::default()
        }
    }

    /// 标记本节点为弹性（主轴吸收剩余空间）。
    pub fn grow(mut self) -> Self {
        self.grow = true;
        self
    }

    /// 标记本节点跨轴填充父容器（Column→撑满宽度），供内部 spacer 右对齐。
    pub fn fill_cross(mut self) -> Self {
        self.fill_cross = true;
        self
    }

    // —— 链式构建辅助 ——
    pub fn pad(mut self, e: Edges) -> Self {
        self.padding = e;
        self
    }
    pub fn margin(mut self, e: Edges) -> Self {
        self.margin = e;
        self
    }
    pub fn gap(mut self, g: f32) -> Self {
        self.gap = g;
        self
    }
    pub fn cross(mut self, a: Align) -> Self {
        self.cross_align = a;
        self
    }
    /// 设置主轴对齐（富余空间落在哪一端）。见 [`View::main_align`]。
    pub fn main(mut self, a: Align) -> Self {
        self.main_align = a;
        self
    }
    pub fn bg(mut self, c: [u8; 4]) -> Self {
        self.bg = Some(c);
        self
    }
    pub fn radius(mut self, r: f32) -> Self {
        self.corner_radius = r;
        self
    }
    pub fn border(mut self, c: [u8; 4], w: f32) -> Self {
        self.border = Some((c, w));
        self
    }
    /// 在文本的 `byte_pos` 处画插入符（覆盖层，不影响布局；见 [`View::caret_at`] 字段说明）。
    /// 位置自动夹到字符边界——越界截到末尾、落在字符中间退回前一边界，故 paint 的切片恒安全。
    pub fn caret_at(mut self, byte_pos: usize, width: f32) -> Self {
        self.caret_at = Some(match &self.text {
            Some(t) => {
                let mut p = byte_pos.min(t.len());
                while p > 0 && !t.is_char_boundary(p) {
                    p -= 1;
                }
                p
            }
            None => 0,
        });
        self.caret_w = width;
        self
    }

    pub fn text_align(mut self, a: Align) -> Self {
        self.text_align = a;
        self
    }
    pub fn font_size(mut self, px: f32) -> Self {
        self.font_size = Some(px);
        self
    }
    /// 设字重（>0 生效；0/None 继承默认 NORMAL）。
    pub fn font_weight(mut self, w: i32) -> Self {
        self.font_weight = if w > 0 { Some(w) } else { None };
        self
    }
    /// 设字体族覆盖（非空生效）。
    pub fn font_family(mut self, f: Option<String>) -> Self {
        self.font_family = f.filter(|s| !s.trim().is_empty());
        self
    }
    pub fn bg_gradient(mut self, g: ViewGradient) -> Self {
        self.bg_gradient = Some(g);
        self
    }
    pub fn left_bar(mut self, bar: LeftBar) -> Self {
        self.left_bar = Some(bar);
        self
    }
    pub fn circle_bg(mut self, color: [u8; 4]) -> Self {
        self.circle_bg = Some(color);
        self
    }
    pub fn bg_image(mut self, img: ViewImage) -> Self {
        self.bg_image = Some(img);
        self
    }
    pub fn layers(mut self, layers: Vec<ViewLayer>) -> Self {
        self.layers = layers;
        self
    }
    pub fn tag(mut self, t: i32) -> Self {
        self.tag = t;
        self
    }
    pub fn fixed_h(mut self, h: f32) -> Self {
        self.fixed_h = Some(h);
        self
    }
    /// 设置宽度下限（设备像素）。<=0 视为不限制。
    pub fn min_w(mut self, w: f32) -> Self {
        self.min_w = (w > 0.0).then_some(w);
        self
    }
    /// 设置高度下限（设备像素）。<=0 视为不限制。
    pub fn min_h(mut self, h: f32) -> Self {
        self.min_h = (h > 0.0).then_some(h);
        self
    }
    pub fn fixed_w(mut self, w: f32) -> Self {
        self.fixed_w = Some(w);
        self
    }
    pub fn child(mut self, c: View) -> Self {
        self.children.push(c);
        self
    }

    /// 把 `inner` 包进一个顺时针旋转 90° 的裸包裹层。见 [`View::rot`] 的形态约束。
    pub fn rotated_cw(inner: View) -> Self {
        Self::rotated(Rot::Cw, inner)
    }

    /// 逆时针版。用途只有一个：套在**已被外层顺时针旋转**的子树内部，把单个字扶正。
    pub fn rotated_ccw(inner: View) -> Self {
        Self::rotated(Rot::Ccw, inner)
    }

    fn rotated(rot: Rot, inner: View) -> Self {
        debug_assert_ne!(rot, Rot::None, "旋转包裹层的方向不能是 None");
        Self {
            rot,
            children: vec![inner],
            ..Self::default()
        }
    }

    fn margin_box(&self) -> (f32, f32) {
        (self.mw + self.margin.w(), self.mh + self.margin.h())
    }

    /// 一次完整布局：自底向上测量，再自顶向下定位（根左上角 = (x,y)）。
    pub fn layout(&mut self, x: f32, y: f32, tr: &TextRenderer) {
        self.measure(tr);
        self.arrange(x, y);
    }

    /// 本节点文本的排版样式（字号回退到渲染器基准字号）。
    ///
    /// 三处用到它——measure、paint 里算水平对齐、caret 量前半段——过去各自重复
    /// `(font_size.unwrap_or(base), font_weight.unwrap_or(0), family.as_deref())`
    /// 这串取值。收成一处是因为**它们必须完全一致**：measure 与 paint 的样式一旦分叉，
    /// 布局按一种宽度排、文字按另一种画，表现为文字整体偏移或超出节点框。
    fn text_style(&self, tr: &TextRenderer) -> TextStyle<'_> {
        TextStyle {
            family: self.font_family.as_deref(),
            size: self.font_size.unwrap_or(tr.base_size()),
            weight: self.font_weight.unwrap_or(0),
        }
    }

    fn measure(&mut self, tr: &TextRenderer) {
        if self.rot != Rot::None {
            debug_assert_eq!(
                self.children.len(),
                1,
                "旋转节点必须恰好一个子节点（见 View::rot 的形态约束）"
            );
            // 子树按**未旋转**测量，本节点对外交换宽高——这是整个旋转能力的支点：
            // 排版、宽度预算、文字截断全都在未旋转的局部空间里跑，一行都不用改。
            let (cw, ch) = match self.children.first_mut() {
                Some(c) => {
                    c.measure(tr);
                    c.margin_box()
                }
                None => (0.0, 0.0),
            };
            self.mw = ch;
            self.mh = cw;
            return;
        }
        let (cw, ch) = if let Some(t) = &self.text {
            let m = tr.measure(t, &self.text_style(tr));
            (m.width, m.height)
        } else {
            let mut main = 0.0f32;
            let mut cross = 0.0f32;
            let n = self.children.len();
            for c in &mut self.children {
                c.measure(tr);
                let (mw, mh) = c.margin_box();
                match self.layout {
                    Layout::Row => {
                        main += mw;
                        cross = cross.max(mh);
                    }
                    Layout::Column => {
                        main += mh;
                        cross = cross.max(mw);
                    }
                }
            }
            if n > 1 {
                main += self.gap * (n - 1) as f32;
            }
            match self.layout {
                Layout::Row => (main, cross),
                Layout::Column => (cross, main),
            }
        };
        let mut w = cw + self.padding.w();
        let mut h = ch + self.padding.h();
        if let Some(fw) = self.fixed_w {
            w = fw;
        }
        if let Some(fh) = self.fixed_h {
            h = fh;
        }
        // 下限在 fixed_* 之后施加：二者同时给时 min 仍然是下限（fixed 更窄会被抬起来）。
        if let Some(minw) = self.min_w {
            w = w.max(minw);
        }
        if let Some(minh) = self.min_h {
            h = h.max(minh);
        }
        self.mw = w;
        self.mh = h;
    }

    fn arrange(&mut self, x: f32, y: f32) {
        self.rect = Rect {
            x,
            y,
            w: self.mw,
            h: self.mh,
        };
        if self.children.is_empty() {
            return;
        }
        if self.rot != Rot::None {
            // 子树排在**局部未旋转坐标系**里、原点 (0,0)：paint 时它被画进一张同尺寸的
            // 临时缓冲，那张缓冲的左上角就是这个原点。命中矩形随后由 collect_hits 映射回屏幕。
            if let Some(c) = self.children.first_mut() {
                let (ml, mt) = (c.margin.l, c.margin.t);
                c.arrange(ml, mt);
            }
            return;
        }
        let cx0 = x + self.padding.l;
        let cy0 = y + self.padding.t;
        let content_w = self.mw - self.padding.w();
        let content_h = self.mh - self.padding.h();

        let n = self.children.len();
        let gap_total = if n > 1 {
            self.gap * (n - 1) as f32
        } else {
            0.0
        };
        let growers = self.children.iter().filter(|c| c.grow).count();

        // 主轴富余（内容总长 < 容器主轴长，仅 fixed/min 撑出容器时非零）。grow 子节点会
        // 把它吃干净，故那时对齐偏移恒为 0。
        let main_used = |v: &Self, horizontal: bool| -> f32 {
            v.children
                .iter()
                .map(|c| {
                    let (mw, mh) = c.margin_box();
                    if horizontal { mw } else { mh }
                })
                .sum::<f32>()
                + gap_total
        };
        let main_offset = |slack: f32, align: Align| -> f32 {
            match align {
                Align::Start => 0.0,
                Align::Center => (slack * 0.5).max(0.0),
                Align::End => slack.max(0.0),
            }
        };
        match self.layout {
            Layout::Row => {
                let used = main_used(self, true);
                // 弹性分配：主轴剩余空间均摊给 grow 子节点（撑大其 mw）。
                if growers > 0 {
                    let extra = (content_w - used).max(0.0) / growers as f32;
                    for c in self.children.iter_mut().filter(|c| c.grow) {
                        c.mw += extra;
                    }
                }
                let mut cx = cx0
                    + if growers > 0 {
                        0.0
                    } else {
                        main_offset(content_w - used, self.main_align)
                    };
                for c in &mut self.children {
                    let (cmw, cmh) = c.margin_box();
                    let cy = match self.cross_align {
                        Align::Start => cy0,
                        Align::Center => cy0 + (content_h - cmh) * 0.5,
                        Align::End => cy0 + content_h - cmh,
                    };
                    c.arrange(cx + c.margin.l, cy + c.margin.t);
                    cx += cmw + self.gap;
                }
            }
            Layout::Column => {
                let used = main_used(self, false);
                if growers > 0 {
                    let extra = (content_h - used).max(0.0) / growers as f32;
                    for c in self.children.iter_mut().filter(|c| c.grow) {
                        c.mh += extra;
                    }
                }
                // 跨轴填充：fill_cross 子节点宽度撑满列内容宽（供其内部 spacer 右对齐）。
                for c in self.children.iter_mut().filter(|c| c.fill_cross) {
                    c.mw = (content_w - c.margin.w()).max(c.mw);
                }
                let mut cy = cy0
                    + if growers > 0 {
                        0.0
                    } else {
                        main_offset(content_h - used, self.main_align)
                    };
                for c in &mut self.children {
                    let (cmw, cmh) = c.margin_box();
                    let cx = match self.cross_align {
                        Align::Start => cx0,
                        Align::Center => cx0 + (content_w - cmw) * 0.5,
                        Align::End => cx0 + content_w - cmw,
                    };
                    c.arrange(cx + c.margin.l, cy + c.margin.t);
                    cy += cmh + self.gap;
                }
            }
        }
    }

    /// 测得尺寸（measure 后有效）
    pub fn measured_size(&self) -> (f32, f32) {
        (self.mw, self.mh)
    }

    /// 旋转 90° 绘制子树（方向由 [`Self::rot`] 定）。
    ///
    /// 三步：**把屏幕上那块底子逆向旋转搬进临时缓冲 → 子树照常画进去 → 正向旋转搬回**。
    ///
    /// ★ 第一步不能省、更不能用「填透明」代替：文本后端的差分法合成要拿目标像素当背景做
    /// 抗锯齿混合（`dwrite.rs` 步骤 1），底子不对文字边缘就会带上错误的混色；窗口的圆角、
    /// 渐变、九宫格背景也都在那块底子里。搬进来再搬回去，这些全部原样保留。
    fn paint_rotated(&self, buf: &mut [u8], buf_w: u32, buf_h: u32, tr: &TextRenderer) {
        // 屏幕上的尺寸已是交换后的；局部空间把它交换回来。
        let rw = self.rect.w.round().max(0.0) as u32;
        let rh = self.rect.h.round().max(0.0) as u32;
        let (cw, ch) = (rh, rw); // 局部：宽=屏幕高，高=屏幕宽
        if cw == 0 || ch == 0 {
            return;
        }
        let ox = self.rect.x.round() as i32;
        let oy = self.rect.y.round() as i32;
        let mut tmp = vec![0u8; (cw as usize) * (ch as usize) * 4];
        blit_unrotate(self.rot, buf, buf_w, buf_h, ox, oy, &mut tmp, cw, ch);
        for c in &self.children {
            c.paint(&mut tmp, cw, ch, tr);
        }
        blit_rotate(self.rot, &tmp, cw, ch, buf, buf_w, buf_h, ox, oy);
    }

    /// 收集所有 tag>=0 节点的绝对矩形 → (tag, rect)
    ///
    /// ⚠️ 穿过 [`View::rot`] 节点时必须把子树的矩形从局部空间映射回屏幕空间——
    /// 漏掉这一步的表现是「鼠标悬停/点击到相邻候选」，而画面完全正常，从现象反推不出成因。
    pub fn collect_hits(&self, out: &mut Vec<(i32, Rect)>) {
        if self.tag >= 0 {
            out.push((self.tag, self.rect));
        }
        if self.rot != Rot::None {
            let mut local = Vec::new();
            for c in &self.children {
                c.collect_hits(&mut local);
            }
            out.extend(
                local
                    .into_iter()
                    .map(|(t, r)| (t, rotate_rect(self.rot, r, self.rect))),
            );
            return;
        }
        for c in &self.children {
            c.collect_hits(out);
        }
    }

    /// 递归绘制到 BGRA 缓冲区
    pub fn paint(&self, buf: &mut [u8], buf_w: u32, buf_h: u32, tr: &TextRenderer) {
        if self.rot != Rot::None {
            self.paint_rotated(buf, buf_w, buf_h, tr);
            return;
        }
        let r = self.rect;
        // 背景 + 边框：先铺底色（满圆角矩形），再画 even-odd 描边环覆盖外缘 bw 宽。
        // 边框作为干净描边环绘制（粗细恒为 bw、内外各一条 AA），不再用内/外两次填充
        // （旧法 AA 在边框/底色交界处双重混合致软边、且无法画镂空边框）。
        if let Some(bg) = self.bg {
            fill_rounded(
                buf,
                buf_w,
                buf_h,
                r.x,
                r.y,
                r.w,
                r.h,
                bg,
                self.corner_radius,
            );
        }
        // 背景渐变（叠在底色上、背景图下，裁到圆角内）。
        if let Some(g) = &self.bg_gradient {
            paint_bg_gradient(buf, buf_w, buf_h, r, self.corner_radius, g);
        }
        if let Some((bc, bw)) = self.border {
            fill_ring(
                buf,
                buf_w,
                buf_h,
                r.x,
                r.y,
                r.w,
                r.h,
                bc,
                self.corner_radius,
                bw,
            );
        }
        // 背景填充图（叠在底色上，裁到圆角内）。
        if let Some(img) = &self.bg_image {
            paint_bg_image(buf, buf_w, buf_h, r, self.corner_radius, img);
        }
        // 左侧强调条（选中候选）：在左内边距内画竖条，高 = 内容高 × height_ratio，垂直居中。不占布局。
        if let Some(bar) = self.left_bar {
            let bh = (r.h * bar.height_ratio).max(2.0);
            let by = r.y + (r.h - bh) * 0.5;
            fill_rounded(
                buf,
                buf_w,
                buf_h,
                r.x + bar.offset,
                by,
                bar.width,
                bh,
                bar.color,
                bar.width * 0.5,
            );
        }
        // 圆形背景（序号圆圈）：节点中心真圆，直径 = min(w,h)。
        if let Some(color) = self.circle_bg {
            let cx = r.x + r.w * 0.5;
            let cy = r.y + r.h * 0.5;
            fill_circle(buf, buf_w, buf_h, cx, cy, r.w.min(r.h) * 0.5, color);
        }
        // z<0 覆盖图（在内容下方）。
        for layer in self.layers.iter().filter(|l| l.z < 0) {
            paint_layer(buf, buf_w, buf_h, r, layer);
        }
        // 文本
        if let Some(t) = &self.text {
            let ts = self.text_style(tr);
            let m = tr.measure(t, &ts);
            let cx0 = r.x + self.padding.l;
            let content_w = r.w - self.padding.w();
            let content_h = r.h - self.padding.h();
            let tx = match self.text_align {
                Align::Start => cx0,
                Align::Center => cx0 + (content_w - m.width) * 0.5,
                Align::End => cx0 + content_w - m.width,
            };
            let ty = r.y + self.padding.t + (content_h - m.height) * 0.5;
            let _ = tr.draw(
                buf,
                buf_w,
                buf_h,
                tx.max(r.x),
                ty.max(r.y),
                t,
                &ts,
                self.text_color,
            );
            // 插入符：覆盖在已绘文本之上，按前半段宽度定位。前半段单独整形与其在整串中的
            // 实际推进有极小字距差异，但只偏移竖线自身、不动文本（布局恒用整串 m.width）。
            if let Some(cp) = self.caret_at {
                let hw = if cp == 0 {
                    0.0
                } else {
                    tr.measure(&t[..cp], &ts).width
                };
                fill_rounded(
                    buf,
                    buf_w,
                    buf_h,
                    tx.max(r.x) + hw,
                    ty.max(r.y),
                    self.caret_w,
                    m.height,
                    self.text_color,
                    0.0,
                );
            }
        }
        // 子节点
        for c in &self.children {
            c.paint(buf, buf_w, buf_h, tr);
        }
        // z>=0 覆盖图（在内容上方）。
        for layer in self.layers.iter().filter(|l| l.z >= 0) {
            paint_layer(buf, buf_w, buf_h, r, layer);
        }
    }
}

// ———————————————— 像素绘制工具 ————————————————

/// 贝塞尔逼近圆弧的控制点比例（kappa = 4/3·(√2−1) ≈ 0.552_284_75）。
/// 字面量按 f32 可表示的最近值书写——多写的位数 f32 存不下，只会误导读者。
const KAPPA: f32 = 0.552_284_8;

/// 抗锯齿纯色画笔。`color` 为直通 [R,G,B,A]。
///
/// **R/B 在此交换**：BGRA 缓冲被当作 tiny-skia 的 RGBA Pixmap 直接渲染（零拷贝），
/// 故传色时取 [B,G,R,A]，输出即合法的预乘 BGRA。这个约定原先在五处填充函数里各抄一遍，
/// 抄漏一处就是红蓝互换的显示 bug，收敛到此处只留一个出错点。
///
/// `#[inline]`：调用点在逐帧绘制路径上，语义上仍是「就地构造一个 Paint」，不该因为抽函数
/// 而多一次调用。
#[inline]
fn aa_paint(color: [u8; 4]) -> Paint<'static> {
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color(Color::from_rgba8(color[2], color[1], color[0], color[3]));
    paint
}

/// 在缓冲区子区域填充圆角矩形：tiny-skia 抗锯齿填充 + 源覆盖混合。
/// `color` 约定为直通 [R,G,B,A]；缓冲区按预乘 BGRA 维护（供 UpdateLayeredWindow）。
///
/// 关键技巧：把 BGRA 缓冲当作 tiny-skia 的"RGBA" Pixmap 直接渲染（零拷贝），
/// 传色时交换 R/B（Color 取 [B,G,R,A]）。预乘 alpha 合成逐通道对称，故输出即合法 BGRA。
/// 绘制背景填充图：从线程局部缓存取目标尺寸填充位图（BGRA 预乘），以 Pattern 填到圆角路径内。
/// 绘制背景渐变：以 tiny-skia 线性/径向着色器填到圆角路径内（叠在底色之上、背景图之下）。
///
/// 本文件的绘图基元一律豁免 `too_many_arguments`：参数是「缓冲 + 缓冲尺寸 + 几何 + 颜色」
/// 这组固定形状，且都在逐帧绘制路径上——包成参数对象只是把同一批标量搬一遍。
#[allow(clippy::too_many_arguments)]
fn paint_bg_gradient(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    r: Rect,
    radius: f32,
    g: &ViewGradient,
) {
    if g.stops.is_empty() {
        return;
    }
    let x = r.x.round();
    let y = r.y.round();
    let rw = r.w.round().max(1.0);
    let rh = r.h.round().max(1.0);
    // 单色/退化 → 纯色填充（tiny-skia 渐变需 ≥2 停靠点）。
    if g.stops.len() < 2 {
        fill_rounded(buf, buf_w, buf_h, r.x, r.y, r.w, r.h, g.stops[0].0, radius);
        return;
    }
    let Some(path) = round_rect_path(x, y, rw, rh, radius.round().max(0.0)) else {
        return;
    };
    // (B,G,R,A) 交换：缓冲按 BGRA 维护，tiny-skia 当 RGBA 渲染（同 fill_rounded 约定）。
    let to_color = |c: &[u8; 4]| Color::from_rgba8(c[2], c[1], c[0], c[3]);
    // pos 规整：钳到 [0,1] 且单调不减（tiny-skia 要求递增停靠点）。
    let mut last = 0.0f32;
    let stops: Vec<GradientStop> = g
        .stops
        .iter()
        .map(|(c, p)| {
            let pos = p.clamp(0.0, 1.0).max(last);
            last = pos;
            GradientStop::new(pos, to_color(c))
        })
        .collect();
    let cx = x + rw * 0.5;
    let cy = y + rh * 0.5;
    let shader = if g.radial {
        let radius_len = 0.5 * (rw * rw + rh * rh).sqrt();
        RadialGradient::new(
            Point::from_xy(cx, cy),
            Point::from_xy(cx, cy),
            radius_len.max(1.0),
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        )
    } else {
        // angle: 0=左→右，顺时针；端点沿方向投影覆盖整盒。
        let rad = g.angle.to_radians();
        let dx = rad.cos();
        let dy = rad.sin();
        let hl = (rw * dx).abs() * 0.5 + (rh * dy).abs() * 0.5;
        let p0 = Point::from_xy(cx - dx * hl, cy - dy * hl);
        let p1 = Point::from_xy(cx + dx * hl, cy + dy * hl);
        LinearGradient::new(p0, p1, stops, SpreadMode::Pad, Transform::identity())
    };
    let Some(shader) = shader else {
        return;
    };
    let Some(mut pm) = PixmapMut::from_bytes(buf, buf_w, buf_h) else {
        return;
    };
    let paint = Paint {
        shader,
        anti_alias: true,
        ..Default::default()
    };
    pm.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn paint_bg_image(buf: &mut [u8], buf_w: u32, buf_h: u32, r: Rect, radius: f32, img: &ViewImage) {
    let x = r.x.round();
    let y = r.y.round();
    let rw = r.w.round().max(1.0);
    let rh = r.h.round().max(1.0);
    let slice = [
        img.slice[0].round().max(0.0) as u32,
        img.slice[1].round().max(0.0) as u32,
        img.slice[2].round().max(0.0) as u32,
        img.slice[3].round().max(0.0) as u32,
    ];
    let mode = crate::image_cache::mode_code(&img.mode);
    let tint = img.tint.unwrap_or([0, 0, 0, 0]);
    let Some(path) = round_rect_path(x, y, rw, rh, radius.round().max(0.0)) else {
        return;
    };
    IMAGE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let Some(fill) = cache.fill(&img.path, mode, slice, rw as u32, rh as u32, tint) else {
            return;
        };
        let Some(mut pm) = PixmapMut::from_bytes(buf, buf_w, buf_h) else {
            return;
        };
        // 填充位图已是目标尺寸（mode 缩放完成），Pattern 仅平移到 rect → 无需再缩放（Nearest 即可）。
        let shader = Pattern::new(
            fill.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Nearest,
            img.opacity.clamp(0.0, 1.0),
            Transform::from_translate(x, y),
        );
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Default::default()
        };
        pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    });
}

/// 绘制 z 层覆盖图：按 anchor 九宫定位 + offset（dp + 百分比）置于 host 内，stretch 到目标尺寸 + opacity。
fn paint_layer(buf: &mut [u8], buf_w: u32, buf_h: u32, host: Rect, layer: &ViewLayer) {
    IMAGE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        // 目标尺寸：指定则用之，否则用原图尺寸。
        let (lw, lh) = if layer.w > 0.0 && layer.h > 0.0 {
            (layer.w.round().max(1.0), layer.h.round().max(1.0))
        } else {
            let Some((sw, sh)) = cache.src_size(&layer.path) else {
                return;
            };
            (sw as f32, sh as f32)
        };
        // anchor 九宫基位（host 内）+ offset（dp px + 百分比相对 host 宽/高）。
        let (ax, ay) = anchor_pos(&layer.anchor, host, lw, lh);
        let lx = (ax + layer.off_x + layer.off_x_pct / 100.0 * host.w).round();
        let ly = (ay + layer.off_y + layer.off_y_pct / 100.0 * host.h).round();
        let Some(fill) = cache.fill(
            &layer.path,
            crate::image_cache::mode_code("stretch"),
            [0; 4],
            lw as u32,
            lh as u32,
            [0, 0, 0, 0],
        ) else {
            return;
        };
        let Some(path) = round_rect_path(lx, ly, lw, lh, 0.0) else {
            return;
        };
        let Some(mut pm) = PixmapMut::from_bytes(buf, buf_w, buf_h) else {
            return;
        };
        let shader = Pattern::new(
            fill.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Nearest,
            layer.opacity.clamp(0.0, 1.0),
            Transform::from_translate(lx, ly),
        );
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Default::default()
        };
        pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    });
}

/// anchor 九宫定位：返回覆盖图左上角在 host 内的基准坐标（未含 offset）。
fn anchor_pos(anchor: &str, host: Rect, lw: f32, lh: f32) -> (f32, f32) {
    let ax = if anchor.contains("left") {
        host.x
    } else if anchor.contains("right") {
        host.x + host.w - lw
    } else {
        host.x + (host.w - lw) * 0.5
    };
    let ay = if anchor.contains("top") {
        host.y
    } else if anchor.contains("bottom") {
        host.y + host.h - lh
    } else {
        host.y + (host.h - lh) * 0.5
    };
    (ax, ay)
}

// ─────────────────── 90° 旋转：几何与像素搬运（纯函数）───────────────────
//
// 唯一的映射约定写在这里，三处（矩形映射、两次像素搬运）共用，**不得各写一遍**：
//
//   局部缓冲宽 `cw`、高 `ch`；屏幕矩形宽 `rw = ch`、高 `rh = cw`（两向都交换宽高）。
//   屏幕内偏移 (sx, sy) ← 局部 (lx, ly)：
//     顺时针  `lx = sy`、`ly = ch - 1 - sx`
//     逆时针  `lx = cw - 1 - sy`、`ly = sx`
//
// 直觉校验：局部左上角 (0,0) 顺时针落到屏幕**右上角**、逆时针落到屏幕**左下角**。
// 于是一行从左到右的文字，顺时针转完是一列从上到下 —— 蒙古文要的就是这个。

/// 局部矩形 → 屏幕矩形。`host` 是旋转节点在屏幕上的矩形（宽高已交换）。
///
/// ★ 与 [`for_each_mapped`] 必须同源：这里给的是**连续**坐标（矩形四边），那里是
/// **离散**像素中心，故此处不出现 `-1`。两者若各推一遍，命中区会整体偏一像素——
/// 画面完全正常，只有点击落点不对，从现象反推不出成因。
fn rotate_rect(rot: Rot, local: Rect, host: Rect) -> Rect {
    match rot {
        Rot::None => local,
        Rot::Cw => Rect {
            x: host.x + host.w - local.y - local.h,
            y: host.y + local.x,
            w: local.h,
            h: local.w,
        },
        Rot::Ccw => Rect {
            x: host.x + local.y,
            y: host.y + host.h - local.x - local.w,
            w: local.h,
            h: local.w,
        },
    }
}

/// 把屏幕上 `(ox, oy)` 起、`ch × cw` 大小的那块**逆向**搬进局部缓冲（旋转的逆变换）。
/// 越界像素留零：调用方随后会把同一批坐标搬回去，越界的部分两头都不参与。
#[allow(clippy::too_many_arguments)]
fn blit_unrotate(
    rot: Rot,
    buf: &[u8],
    buf_w: u32,
    buf_h: u32,
    ox: i32,
    oy: i32,
    tmp: &mut [u8],
    cw: u32,
    ch: u32,
) {
    for_each_mapped(rot, buf_w, buf_h, ox, oy, cw, ch, |src_i, dst_i| {
        tmp[dst_i..dst_i + 4].copy_from_slice(&buf[src_i..src_i + 4]);
    });
}

/// 把局部缓冲**正向**旋转 90° 写回屏幕 `(ox, oy)` 处。
#[allow(clippy::too_many_arguments)]
fn blit_rotate(
    rot: Rot,
    tmp: &[u8],
    cw: u32,
    ch: u32,
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    ox: i32,
    oy: i32,
) {
    for_each_mapped(rot, buf_w, buf_h, ox, oy, cw, ch, |dst_i, src_i| {
        buf[dst_i..dst_i + 4].copy_from_slice(&tmp[src_i..src_i + 4]);
    });
}

/// 遍历两个缓冲之间的对应像素，回调收到 `(屏幕字节下标, 局部字节下标)`。
///
/// ★ 两次搬运共用它而不是各写一个双重循环：两处的坐标映射必须逐像素一致，否则
/// 「搬进来的底子」与「搬回去的结果」错开一个像素——表现是文字边缘发虚且整体偏移一格，
/// 而两段代码各自看都对。同 `dwrite.rs` 那条「测量与绘制向同一个 API 传不同参数」的教训。
///
/// `Rot::None` 不产出任何像素：调用点都在 `rot != None` 的分支里，真走到这儿说明
/// 上游漏判了，什么都不画比画错位置更容易发现。
#[allow(clippy::too_many_arguments)]
fn for_each_mapped(
    rot: Rot,
    buf_w: u32,
    buf_h: u32,
    ox: i32,
    oy: i32,
    cw: u32,
    ch: u32,
    mut f: impl FnMut(usize, usize),
) {
    if rot == Rot::None {
        debug_assert!(false, "for_each_mapped 不该收到 Rot::None");
        return;
    }
    let (rw, rh) = (ch, cw); // 屏幕尺寸 = 局部尺寸交换
    for sy in 0..rh {
        let py = oy + sy as i32;
        if py < 0 || py >= buf_h as i32 {
            continue;
        }
        for sx in 0..rw {
            let px = ox + sx as i32;
            if px < 0 || px >= buf_w as i32 {
                continue;
            }
            let (lx, ly) = match rot {
                Rot::Cw => (sy, ch - 1 - sx),
                _ => (cw - 1 - sy, sx),
            };
            let screen_i = ((py as u32 * buf_w + px as u32) * 4) as usize;
            let local_i = ((ly * cw + lx) * 4) as usize;
            f(screen_i, local_i);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn fill_rounded(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [u8; 4],
    radius: f32,
) {
    if color[3] == 0 {
        return;
    }
    // 位置/尺寸对齐像素网格（这些盒子本就像素对齐），半径保留浮点供 AA。
    let x = x.round();
    let y = y.round();
    let w = w.round();
    let h = h.round();
    if w <= 0.0 || h <= 0.0 || buf_w == 0 || buf_h == 0 {
        return;
    }
    let Some(path) = round_rect_path(x, y, w, h, radius.round().max(0.0)) else {
        return;
    };
    let Some(mut pm) = PixmapMut::from_bytes(buf, buf_w, buf_h) else {
        return;
    };
    let paint = aa_paint(color);
    pm.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// 填充实心圆（tiny-skia 抗锯齿）。`color` 为 [R,G,B,A]，缓冲预乘 BGRA（同 fill_rounded 换 R/B）。
#[allow(clippy::too_many_arguments)]
pub fn fill_circle(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    cx: f32,
    cy: f32,
    r: f32,
    color: [u8; 4],
) {
    if color[3] == 0 || r <= 0.0 || buf_w == 0 || buf_h == 0 {
        return;
    }
    let mut pb = PathBuilder::new();
    pb.push_circle(cx, cy, r);
    let Some(path) = pb.finish() else {
        return;
    };
    let Some(mut pm) = PixmapMut::from_bytes(buf, buf_w, buf_h) else {
        return;
    };
    let paint = aa_paint(color);
    pm.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// 绘制齿轮图标（工具栏设置按钮）：8 齿平顶 cog 多边形（`color` 齿轮体）+ 中心孔（`hole` 色）。
/// 纯矢量绘制，不依赖字体度量 → 在单元格内精确居中、与文字格对齐。
/// `(cx,cy)` 中心，`r` 外径（齿尖半径）。color/hole 为 [R,G,B,A]，缓冲预乘 BGRA（同 fill_rounded 换 R/B）。
#[allow(clippy::too_many_arguments)]
pub fn fill_gear(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    cx: f32,
    cy: f32,
    r: f32,
    color: [u8; 4],
    hole: [u8; 4],
) {
    if color[3] == 0 || r <= 0.0 || buf_w == 0 || buf_h == 0 {
        return;
    }
    let teeth = 8usize;
    let r_in = r * 0.72; // 齿根半径
    let seg = std::f32::consts::TAU / teeth as f32;
    let tip = seg * 0.28; // 齿尖半角（平顶齿）
    let mut pb = PathBuilder::new();
    for i in 0..teeth {
        let c = i as f32 * seg;
        // 每齿 4 顶点：根-起 → 尖-起 → 尖-止 → 根-止，形成平顶梯形齿。
        let verts = [
            (r_in, c - seg * 0.5),
            (r, c - tip),
            (r, c + tip),
            (r_in, c + seg * 0.5),
        ];
        for (j, (rad, ang)) in verts.iter().enumerate() {
            let x = cx + rad * ang.cos();
            let y = cy + rad * ang.sin();
            if i == 0 && j == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
    }
    pb.close();
    if let Some(path) = pb.finish()
        && let Some(mut pm) = PixmapMut::from_bytes(buf, buf_w, buf_h)
    {
        let paint = aa_paint(color);
        pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    // 中心孔：在齿轮体上叠画一个小圆（hole 色，通常取工具栏底色 → 视觉镂空）。
    fill_circle(buf, buf_w, buf_h, cx, cy, r * 0.34, hole);
}

/// 绘制内联 SVG 图标到 BGRA 缓冲（工具栏图标用）：按 `color` 单色 tint（SVG 仅作 alpha 蒙版），
/// 栅格化到 `size`×`size` 后以 src-over 合成到 `(dx,dy)`（左上角）。不依赖字体度量 → 位置精确、形状灵活。
#[allow(clippy::too_many_arguments)]
pub fn draw_svg_icon(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    svg: &str,
    dx: f32,
    dy: f32,
    size: f32,
    color: [u8; 4],
) {
    if color[3] == 0 {
        return;
    }
    let s = size.round().max(1.0) as u32;
    let Some(icon) = crate::image_cache::rasterize_svg_str_tinted(svg, s, s, color) else {
        return;
    };
    let Some(mut pm) = PixmapMut::from_bytes(buf, buf_w, buf_h) else {
        return;
    };
    pm.draw_pixmap(
        dx.round() as i32,
        dy.round() as i32,
        icon.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

/// 高斯软投影（对齐 Go paintBlurredShadow）：在临时缓冲画 spread 扩张的圆角矩形，
/// alpha 通道做 3 次方框模糊逼近高斯，着色后预乘 src-over 合成到主 BGRA 缓冲。
/// (box_x, box_y, box_w, box_h) 为内容盒在主缓冲中的几何（不含 spread/offset）；
/// off_x/off_y 为阴影总偏移（基础 + 扩散额外偏移之和）。
#[allow(clippy::too_many_arguments)]
pub fn paint_blur_shadow(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    box_x: f32,
    box_y: f32,
    box_w: f32,
    box_h: f32,
    radius: f32,
    blur: f32,
    spread: f32,
    off_x: f32,
    off_y: f32,
    color: [u8; 4],
) {
    if color[3] == 0 || buf_w == 0 || buf_h == 0 {
        return;
    }
    // 扩散后阴影盒（内容盒 ±spread，再加总偏移）
    let bw = box_w + 2.0 * spread;
    let bh = box_h + 2.0 * spread;
    let bx = box_x + off_x - spread;
    let by = box_y + off_y - spread;
    if bw <= 0.0 || bh <= 0.0 {
        return;
    }
    // 亚像素相位：蒙版按相位构建才能保住边缘 AA，故它也是缓存键的一部分。
    // 候选窗的阴影盒坐标恒为整数，实际相位恒 0 → 同几何必命中。
    let phase_x = bx - bx.floor();
    let phase_y = by - by.floor();

    // 着色 + 预乘 src-over 合成到主缓冲（主缓冲为 BGRA：0=B,1=G,2=R,3=A；color 为 [R,G,B,A]）
    let (cr, cg, cb, ca) = (
        color[0] as u32,
        color[1] as u32,
        color[2] as u32,
        color[3] as u32,
    );
    with_shadow_mask(bw, bh, radius, blur, phase_x, phase_y, |mask| {
        let dst_x0 = bx.floor() as i32 - mask.pad;
        let dst_y0 = by.floor() as i32 - mask.pad;
        for ty in 0..mask.h {
            for tx in 0..mask.w {
                let ma = mask.alpha[(ty * mask.w + tx) as usize] as u32;
                if ma == 0 {
                    continue;
                }
                let fa = ma * ca / 255; // 最终 alpha
                if fa == 0 {
                    continue;
                }
                let dx = dst_x0 + tx;
                let dy = dst_y0 + ty;
                if dx < 0 || dx >= buf_w as i32 || dy < 0 || dy >= buf_h as i32 {
                    continue;
                }
                let off = ((dy * buf_w as i32 + dx) * 4) as usize;
                let inv = 255 - fa;
                let sb = cb * fa / 255;
                let sg = cg * fa / 255;
                let sr = cr * fa / 255;
                buf[off] = ((sb * 255 + buf[off] as u32 * inv) / 255) as u8;
                buf[off + 1] = ((sg * 255 + buf[off + 1] as u32 * inv) / 255) as u8;
                buf[off + 2] = ((sr * 255 + buf[off + 2] as u32 * inv) / 255) as u8;
                buf[off + 3] = ((fa * 255 + buf[off + 3] as u32 * inv) / 255) as u8;
            }
        }
    });
}

/// 窗口软投影参数（设备像素，已 ×scale）。模糊扩散层总偏移 = 基础 offset + 扩散额外偏移。
/// 候选窗与其它窗口（status/tooltip/toast）共享：四向扩边 + 高斯软影绘制一处实现。
pub struct SoftShadow {
    pub ox: f32,
    pub oy: f32,
    pub blur: f32,
    pub spread: f32,
    pub sox: f32,
    pub soy: f32,
    pub color: [u8; 4],
}

impl SoftShadow {
    /// 从节点 shadow_* 字段（Option<Dim> + 颜色）构建并 ×scale。
    /// 无色/全透明/零模糊零扩散零偏移 → None（不画投影）。
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        offset_x: Option<Dim>,
        offset_y: Option<Dim>,
        blur: Option<Dim>,
        spread: Option<Dim>,
        spread_off_x: Option<Dim>,
        spread_off_y: Option<Dim>,
        color: Option<[u8; 4]>,
        scale: f32,
    ) -> Option<SoftShadow> {
        let color = color?;
        if color[3] == 0 {
            return None;
        }
        let signed = |d: Option<Dim>| d.map(|x| x.resolve(scale, 0.0)).unwrap_or(0.0);
        let nonneg = |d: Option<Dim>| signed(d).max(0.0);
        let sh = SoftShadow {
            ox: signed(offset_x),
            oy: signed(offset_y),
            blur: nonneg(blur),
            spread: nonneg(spread),
            sox: signed(spread_off_x),
            soy: signed(spread_off_y),
            color,
        };
        if sh.blur <= 0.0 && sh.spread <= 0.0 && sh.off_x() == 0.0 && sh.off_y() == 0.0 {
            return None;
        }
        Some(sh)
    }

    /// 模糊扩散层 X 方向总偏移（基础 + 扩散额外）。
    pub fn off_x(&self) -> f32 {
        self.ox + self.sox
    }
    /// 模糊扩散层 Y 方向总偏移。
    pub fn off_y(&self) -> f32 {
        self.oy + self.soy
    }

    /// 四向缓冲扩边 (left, top, right, bottom)（与 Go shadowMargins 对齐）。
    pub fn margins(&self) -> (u32, u32, u32, u32) {
        let sigma = (self.blur * (self.blur + 2.0)).max(0.0).sqrt();
        let base = (3.0 * sigma).ceil() + 2.0 + self.spread;
        let (ox, oy) = (self.off_x(), self.off_y());
        (
            (base + (-ox).max(0.0)).ceil() as u32,
            (base + (-oy).max(0.0)).ceil() as u32,
            (base + ox.max(0.0)).ceil() as u32,
            (base + oy.max(0.0)).ceil() as u32,
        )
    }

    /// 在主缓冲画软影。(bx,by) 为内容盒左上（不含 offset/spread），(bw,bh) 内容盒尺寸，radius 圆角。
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &self,
        buf: &mut [u8],
        buf_w: u32,
        buf_h: u32,
        bx: f32,
        by: f32,
        bw: f32,
        bh: f32,
        radius: f32,
    ) {
        paint_blur_shadow(
            buf,
            buf_w,
            buf_h,
            bx,
            by,
            bw,
            bh,
            radius,
            self.blur,
            self.spread,
            self.off_x(),
            self.off_y(),
            self.color,
        );
    }
}

/// 对 alpha 缓冲做一次可分离方框模糊（水平 + 垂直），边界取延伸（clamp）。三次调用逼近高斯。
fn box_blur_alpha(a: &mut [u8], w: i32, h: i32, r: i32) {
    if r <= 0 || w <= 0 || h <= 0 {
        return;
    }
    let win = (2 * r + 1) as u32;
    let mut tmp = vec![0u8; a.len()];
    // 水平
    for y in 0..h {
        let row = (y * w) as usize;
        let mut sum: u32 = 0;
        for k in -r..=r {
            let xi = k.clamp(0, w - 1) as usize;
            sum += a[row + xi] as u32;
        }
        for x in 0..w {
            tmp[row + x as usize] = (sum / win) as u8;
            let x_in = (x + r + 1).clamp(0, w - 1) as usize;
            let x_out = (x - r).clamp(0, w - 1) as usize;
            sum += a[row + x_in] as u32;
            sum -= a[row + x_out] as u32;
        }
    }
    // 垂直
    for x in 0..w {
        let xi = x as usize;
        let mut sum: u32 = 0;
        for k in -r..=r {
            let yi = k.clamp(0, h - 1);
            sum += tmp[(yi * w) as usize + xi] as u32;
        }
        for y in 0..h {
            a[(y * w) as usize + xi] = (sum / win) as u8;
            let y_in = (y + r + 1).clamp(0, h - 1);
            let y_out = (y - r).clamp(0, h - 1);
            sum += tmp[(y_in * w) as usize + xi] as u32;
            sum -= tmp[(y_out * w) as usize + xi] as u32;
        }
    }
}

/// 向 PathBuilder 追加一个圆角矩形子路径（radius 自动钳制到 min(w,h)/2；为 0 时退化为直角矩形）。
fn push_round_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, radius: f32) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let r = radius.min(w * 0.5).min(h * 0.5).max(0.0);
    if r <= 0.0 {
        if let Some(rect) = tiny_skia::Rect::from_xywh(x, y, w, h) {
            pb.push_rect(rect);
        }
        return;
    }
    let (l, t, rt, b) = (x, y, x + w, y + h);
    let k = r * KAPPA;
    pb.move_to(l + r, t);
    pb.line_to(rt - r, t);
    pb.cubic_to(rt - r + k, t, rt, t + r - k, rt, t + r);
    pb.line_to(rt, b - r);
    pb.cubic_to(rt, b - r + k, rt - r + k, b, rt - r, b);
    pb.line_to(l + r, b);
    pb.cubic_to(l + r - k, b, l, b - r + k, l, b - r);
    pb.line_to(l, t + r);
    pb.cubic_to(l, t + r - k, l + r - k, t, l + r, t);
    pb.close();
}

/// 构造圆角矩形路径（radius 自动钳制到 min(w,h)/2；为 0 时退化为直角矩形）。
fn round_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    push_round_rect(&mut pb, x, y, w, h, radius);
    pb.finish()
}

/// 圆角矩形描边环（外圈 − 内圈，even-odd 单次填充）：粗细恒为 bw、内外各一条干净 AA，
/// 对齐 Go 的边框画法（避免中心描边 AA 渗色致粗细不均）。透明内部也适用。
/// color 为 [R,G,B,A]，缓冲预乘 BGRA（同 fill_rounded 换 R/B）。
#[allow(clippy::too_many_arguments)]
pub fn fill_ring(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [u8; 4],
    radius: f32,
    bw: f32,
) {
    if color[3] == 0 || bw <= 0.0 || buf_w == 0 || buf_h == 0 {
        return;
    }
    let x = x.round();
    let y = y.round();
    let w = w.round();
    let h = h.round();
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let radius = radius.round().max(0.0);
    let mut pb = PathBuilder::new();
    push_round_rect(&mut pb, x, y, w, h, radius); // 外圈
    push_round_rect(
        &mut pb,
        x + bw,
        y + bw,
        w - 2.0 * bw,
        h - 2.0 * bw,
        (radius - bw).max(0.0),
    ); // 内圈（even-odd 挖空）
    let Some(path) = pb.finish() else {
        return;
    };
    let Some(mut pm) = PixmapMut::from_bytes(buf, buf_w, buf_h) else {
        return;
    };
    let paint = aa_paint(color);
    pm.fill_path(
        &path,
        &paint,
        FillRule::EvenOdd,
        Transform::identity(),
        None,
    );
}

// ———————————————— 测试 ————————————————
//
// 测试边界说明：本模块的盒模型布局（measure/arrange/collect_hits）与形状绘制
// （fill_rounded/fill_circle/fill_ring，基于纯 Rust 的 tiny-skia）在 **所有平台
// 行为一致**，Linux 上的测试结果对 Windows 同样有效。唯一的跨平台差异是**文本
// 测量**：Windows 用 DirectWrite，非 Windows 用 mock 近似（字符数 × 字号 × 0.6）。
// 因此凡断言**具体文本尺寸/含文本布局数值**的用例都 gate 到 `not(windows)`，
// 以 mock 的确定尺寸做精确断言；纯几何与形状用例则跨平台运行。

/// 几何与形状绘制测试（跨平台真实：tiny-skia 纯 Rust 光栅化，不依赖文本后端）。
#[cfg(test)]
mod geom_tests {
    use super::*;

    #[test]
    fn rect_contains_includes_edges() {
        let r = Rect {
            x: 10.0,
            y: 10.0,
            w: 20.0,
            h: 20.0,
        };
        assert!(r.contains(10.0, 10.0)); // 左上角
        assert!(r.contains(30.0, 30.0)); // 右下角（x+w, y+h 含边界）
        assert!(r.contains(20.0, 20.0)); // 内部
        assert!(!r.contains(9.9, 20.0)); // 左外
        assert!(!r.contains(30.1, 20.0)); // 右外
        assert!(!r.contains(20.0, 9.9)); // 上外
    }

    #[test]
    fn edges_helpers() {
        let a = Edges::all(5.0);
        assert_eq!((a.l, a.t, a.r, a.b), (5.0, 5.0, 5.0, 5.0));
        assert_eq!(a.w(), 10.0);
        assert_eq!(a.h(), 10.0);
        let xy = Edges::xy(3.0, 4.0);
        assert_eq!((xy.l, xy.t, xy.r, xy.b), (3.0, 4.0, 3.0, 4.0));
        assert_eq!(xy.w(), 6.0);
        assert_eq!(xy.h(), 8.0);
    }

    #[test]
    fn fill_rounded_writes_alpha() {
        let mut buf = vec![0u8; 10 * 10 * 4];
        fill_rounded(
            &mut buf,
            10,
            10,
            0.0,
            0.0,
            10.0,
            10.0,
            [255, 0, 0, 255],
            0.0,
        );
        let center = (5 * 10 + 5) * 4;
        assert!(buf[center + 3] > 0, "中心像素 alpha 应被写入");
    }

    #[test]
    fn fill_rounded_transparent_color_is_noop() {
        let mut buf = vec![0u8; 4 * 4 * 4];
        fill_rounded(&mut buf, 4, 4, 0.0, 0.0, 4.0, 4.0, [255, 0, 0, 0], 0.0);
        assert!(buf.iter().all(|&b| b == 0), "alpha=0 不应写入任何像素");
    }

    #[test]
    fn fill_circle_writes_center_not_corner() {
        let mut buf = vec![0u8; 20 * 20 * 4];
        fill_circle(&mut buf, 20, 20, 10.0, 10.0, 8.0, [0, 255, 0, 255]);
        assert!(px_alpha(&buf, 20, 10, 10) > 0, "圆心应被填充");
        assert_eq!(px_alpha(&buf, 20, 0, 0), 0, "角落在圆外，不应被填充");
    }

    #[test]
    fn gradient_linear_writes_pixels() {
        let mut buf = vec![0u8; 16 * 16 * 4];
        let g = ViewGradient {
            radial: false,
            angle: 0.0,
            stops: vec![([255, 0, 0, 255], 0.0), ([0, 0, 255, 255], 1.0)],
        };
        let r = Rect {
            x: 0.0,
            y: 0.0,
            w: 16.0,
            h: 16.0,
        };
        paint_bg_gradient(&mut buf, 16, 16, r, 0.0, &g);
        assert!(px_alpha(&buf, 16, 8, 8) > 0, "渐变中心 alpha 应被写入");
    }

    #[test]
    fn gradient_single_stop_falls_back_to_solid() {
        let mut buf = vec![0u8; 8 * 8 * 4];
        let g = ViewGradient {
            radial: false,
            angle: 0.0,
            stops: vec![([0, 255, 0, 255], 0.5)],
        };
        let r = Rect {
            x: 0.0,
            y: 0.0,
            w: 8.0,
            h: 8.0,
        };
        paint_bg_gradient(&mut buf, 8, 8, r, 0.0, &g);
        assert!(px_alpha(&buf, 8, 4, 4) > 0, "单停靠点退化为纯色填充");
    }

    #[test]
    fn fill_ring_hollow_center() {
        let mut buf = vec![0u8; 20 * 20 * 4];
        fill_ring(
            &mut buf,
            20,
            20,
            0.0,
            0.0,
            20.0,
            20.0,
            [0, 0, 255, 255],
            0.0,
            2.0,
        );
        // 边框环：靠边像素被描边，正中心（挖空）保持透明
        assert!(px_alpha(&buf, 20, 10, 0) > 0, "上边框应被描边");
        assert_eq!(px_alpha(&buf, 20, 10, 10), 0, "环中心应镂空透明");
    }

    // ── 左侧强调条：height_ratio / offset ──────────────────────────────────
    // 用无文本的固定尺寸容器，几何跨平台确定（不依赖 DirectWrite / mock 测量差异）。

    /// 在 40×40 缓冲里画一个 40×40 的容器，带指定强调条参数；返回 BGRA 缓冲。
    fn paint_left_bar(height_ratio: f32, offset: f32, width: f32) -> Vec<u8> {
        let tr = crate::text::dwrite::TextRenderer::new("test", 20.0).unwrap();
        let mut v = View::container(Layout::Row)
            .fixed_w(40.0)
            .fixed_h(40.0)
            .left_bar(LeftBar {
                color: [255, 0, 0, 255],
                width,
                height_ratio,
                offset,
            });
        v.layout(0.0, 0.0, &tr);
        let mut buf = vec![0u8; 40 * 40 * 4];
        v.paint(&mut buf, 40, 40, &tr);
        buf
    }

    /// BGRA 缓冲里 (x, y) 像素的 alpha 分量。`stride` 为缓冲宽度（像素）。
    /// 写成函数而非手写 `(y * w + x) * 4 + 3`：坐标语义更清楚，也避开 clippy
    /// 对 `0 * 20 + 0` 这类「保留坐标形状」写法的 erasing_op/identity_op 报错。
    fn px_alpha(buf: &[u8], stride: usize, x: usize, y: usize) -> u8 {
        buf[(y * stride + x) * 4 + 3]
    }

    /// 40×40 缓冲专用的 [`px_alpha`]。
    fn alpha_at(buf: &[u8], x: usize, y: usize) -> u8 {
        px_alpha(buf, 40, x, y)
    }

    #[test]
    fn left_bar_height_ratio_controls_bar_height() {
        // ratio=1.0：条高 = 行高，顶部与中部都被填充。
        let full = paint_left_bar(1.0, 0.0, 4.0);
        assert!(alpha_at(&full, 1, 1) > 0, "ratio=1.0 顶部应被填充");
        assert!(alpha_at(&full, 1, 20) > 0, "ratio=1.0 中部应被填充");

        // ratio=0.5：条高 = 行高一半、垂直居中 → 中部填充、顶部留白。
        let half = paint_left_bar(0.5, 0.0, 4.0);
        assert!(alpha_at(&half, 1, 20) > 0, "ratio=0.5 中部应被填充");
        assert_eq!(alpha_at(&half, 1, 1), 0, "ratio=0.5 顶部应留白");
    }

    #[test]
    fn left_bar_offset_shifts_bar_right() {
        // offset=0：条贴左缘。
        let flush = paint_left_bar(1.0, 0.0, 4.0);
        assert!(alpha_at(&flush, 1, 20) > 0, "offset=0 左缘应被填充");

        // offset=10：条右移 10px → 左缘留白，10px 之后才是条。
        let shifted = paint_left_bar(1.0, 10.0, 4.0);
        assert_eq!(alpha_at(&shifted, 1, 20), 0, "offset=10 左缘应留白");
        assert!(alpha_at(&shifted, 11, 20) > 0, "offset=10 处应被填充");
    }

    /// 极小 ratio 仍保底 2px——否则主题写个 0.01 会让条彻底消失。
    #[test]
    fn left_bar_tiny_ratio_clamped_to_min_height() {
        let tiny = paint_left_bar(0.001, 0.0, 4.0);
        assert!(alpha_at(&tiny, 1, 20) > 0, "极小比例仍应保底 2px 可见");
    }
}

/// 阴影蒙版缓存：缓存不得改变渲染结果（跨平台真实——纯 tiny-skia 光栅化，不涉文本）。
///
/// 这些用例的重心是**反向对照**：只断言"同参数两次结果一致"是抓不到缓存 bug 的——
/// 键漏了某一项时，两次调用都会取到同一张错蒙版，结果照样一致、测试照样绿。真正能
/// 抓住漏键的是"只改一项，输出必须变"。
#[cfg(test)]
mod shadow_cache_tests {
    use super::*;

    /// 在 64×64 透明缓冲上画一次阴影，返回缓冲。内容盒固定落在 (16,16)。
    fn shadow_buf(bw: f32, bh: f32, radius: f32, blur: f32, color: [u8; 4]) -> Vec<u8> {
        const W: u32 = 64;
        const H: u32 = 64;
        let mut buf = vec![0u8; (W * H * 4) as usize];
        paint_blur_shadow(
            &mut buf, W, H, 16.0, 16.0, bw, bh, radius, blur, 0.0, 0.0, 0.0, color,
        );
        buf
    }

    /// 基准形状：够小以完整落在 64×64 内，blur 够大以真正触发模糊路径。
    fn base(color: [u8; 4]) -> Vec<u8> {
        shadow_buf(20.0, 20.0, 4.0, 3.0, color)
    }

    const BLACK: [u8; 4] = [0, 0, 0, 200];

    /// 缓存**确实在工作**：同几何重绘不新增条目，不同几何才新增。
    ///
    /// 这条是本模块其余用例的地基。没有它，那些"结果一致 / 结果不同"的断言在缓存
    /// 完全失效（每帧照旧重算模糊）时也会全绿——而消除"每帧重算"正是这次改动的
    /// 全部目的，测不出来就等于没测。
    ///
    /// 换色不新增条目这一条同时正面验证了键的设计：蒙版与颜色无关。
    #[test]
    fn cache_stores_and_reuses_masks() {
        shadow_cache_clear();
        assert_eq!(shadow_cache_len(), 0, "起手应为空");
        let _ = base(BLACK);
        assert_eq!(shadow_cache_len(), 1, "首次绘制应入缓存一条");
        let _ = base(BLACK);
        assert_eq!(shadow_cache_len(), 1, "同几何重绘应命中，不得新增");
        let _ = base([255, 0, 0, 200]);
        assert_eq!(shadow_cache_len(), 1, "仅换色应命中（蒙版与颜色无关）");
        let _ = shadow_buf(28.0, 20.0, 4.0, 3.0, BLACK);
        assert_eq!(shadow_cache_len(), 2, "不同几何应新增条目");
    }

    /// 同参数重复绘制结果逐字节一致——缓存命中路径与首次构建路径必须等价。
    #[test]
    fn cache_hit_reproduces_first_render() {
        let first = base(BLACK);
        let second = base(BLACK);
        assert_eq!(first, second, "缓存命中不得改变渲染结果");
    }

    /// 阴影颜色**不在缓存键里**（缓存的是 alpha 蒙版），输出却必须随颜色变。
    ///
    /// 这是"缓存蒙版而非着色像素"这一设计的判据：一旦有人把着色也塞进缓存，
    /// 换了颜色仍会取到上一色的像素——而因为几何没变，缓存必命中，错误 100% 复现。
    #[test]
    fn color_is_not_cached_though_absent_from_key() {
        let black = base(BLACK);
        let red = base([255, 0, 0, 200]);
        assert_ne!(black, red, "换色必须改变输出（着色不得被缓存）");
    }

    /// 主题明暗切换只改颜色、不改几何——此时蒙版应复用而输出仍正确。
    /// 与上一用例同源，但特意走"先浅后深"的顺序，覆盖缓存已被填充后再换色的路径。
    #[test]
    fn alpha_only_change_still_affects_output() {
        let opaque = base([0, 0, 0, 255]);
        let faint = base([0, 0, 0, 64]);
        assert_ne!(opaque, faint, "仅改阴影透明度也必须改变输出");
    }

    /// 盒尺寸进键：只改宽度，输出必须变。
    #[test]
    fn box_size_changes_output() {
        let a = shadow_buf(20.0, 20.0, 4.0, 3.0, BLACK);
        let b = shadow_buf(28.0, 20.0, 4.0, 3.0, BLACK);
        assert_ne!(a, b, "盒宽变化必须改变输出");
    }

    /// 圆角进键：直角与圆角的蒙版不同。
    #[test]
    fn radius_changes_output() {
        let sharp = shadow_buf(20.0, 20.0, 0.0, 3.0, BLACK);
        let round = shadow_buf(20.0, 20.0, 9.0, 3.0, BLACK);
        assert_ne!(sharp, round, "圆角变化必须改变输出");
    }

    /// 模糊半径进键：它同时决定蒙版尺寸（pad）与衰减，漏掉它错得最明显。
    #[test]
    fn blur_changes_output() {
        let tight = shadow_buf(20.0, 20.0, 4.0, 1.0, BLACK);
        let wide = shadow_buf(20.0, 20.0, 4.0, 6.0, BLACK);
        assert_ne!(tight, wide, "模糊半径变化必须改变输出");
    }

    /// 缓存超限后整体清空，不得影响正确性——清空前后同参数结果须一致。
    /// 用 33 组不同几何（> `SHADOW_CACHE_CAP`）挤掉基准条目，再重画基准比对。
    #[test]
    fn eviction_preserves_correctness() {
        let before = base(BLACK);
        for i in 0..=SHADOW_CACHE_CAP {
            let _ = shadow_buf(10.0 + i as f32, 12.0, 2.0, 2.0, BLACK);
        }
        let after = base(BLACK);
        assert_eq!(before, after, "缓存清空后重建的蒙版须与首次一致");
    }
}

/// 顺时针 90° 旋转：几何映射、像素搬运、测量与命中。
///
/// **不 gate 平台**：全部断言只用 `fixed_w`/`fixed_h` 与裸缓冲，不碰文本测量，
/// 故三个平台答案相同（`layout_tests` 之所以 gate 是因为它断言文本尺寸）。
#[cfg(test)]
mod rotate_tests {
    use super::*;
    use crate::text::dwrite::TextRenderer;

    fn tr() -> TextRenderer {
        TextRenderer::new("test", 20.0).unwrap()
    }

    fn fixed(w: f32, h: f32, tag: i32) -> View {
        View::container(Layout::Row).fixed_w(w).fixed_h(h).tag(tag)
    }

    fn hit(root: &View, tag: i32) -> Rect {
        let mut out = Vec::new();
        root.collect_hits(&mut out);
        out.into_iter()
            .find(|(t, _)| *t == tag)
            .unwrap_or_else(|| panic!("tag {tag} 未命中"))
            .1
    }

    /// 旋转节点对外交换宽高——整个旋转能力的支点：子树仍在未旋转空间里排版。
    #[test]
    fn rotation_swaps_the_measured_size() {
        let mut root = View::rotated_cw(fixed(40.0, 10.0, 0));
        root.layout(0.0, 0.0, &tr());
        assert_eq!(root.measured_size(), (10.0, 40.0));
    }

    /// ★ 方向判据：局部左上角必须落到屏幕**右上角**。落到左下角就是逆时针，
    /// 蒙古文会变成自下而上读。
    #[test]
    fn local_top_left_maps_to_screen_top_right() {
        let host = Rect {
            x: 100.0,
            y: 200.0,
            w: 10.0,
            h: 40.0,
        };
        // 局部 (0,0) 处 2×3 的小块。
        let got = rotate_rect(
            Rot::Cw,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 2.0,
                h: 3.0,
            },
            host,
        );
        assert_eq!(got.x, 100.0 + 10.0 - 3.0, "没贴到右缘 ⇒ 转反了");
        assert_eq!(got.y, 200.0);
        assert_eq!((got.w, got.h), (3.0, 2.0), "宽高没交换");
    }

    /// 命中矩形必须跟着转。漏掉映射时画面完全正常、只有鼠标点错候选，从现象反推不出成因。
    ///
    /// ★ 反向对照同样必要：不带旋转时矩形**不得**被映射。只测「转了」的话，
    /// 一个无条件映射的实现会让普通候选窗的命中全错，而那条路径没人测。
    #[test]
    fn hit_rects_follow_the_rotation() {
        let inner = View::container(Layout::Column)
            .child(fixed(40.0, 10.0, 1))
            .child(fixed(40.0, 10.0, 2));
        let mut root = View::rotated_cw(inner);
        root.layout(0.0, 0.0, &tr());
        // 局部：1 在上 (y=0)、2 在下 (y=10)；屏幕宽 20（=局部高）。
        // 顺时针后：1 贴右（x=10）、2 在左（x=0）。
        assert_eq!(hit(&root, 1).x, 10.0);
        assert_eq!(hit(&root, 2).x, 0.0);
        assert_eq!(hit(&root, 1).w, 10.0, "屏幕宽应为局部高");
        assert_eq!(hit(&root, 1).h, 40.0, "屏幕高应为局部宽");

        let mut plain = View::container(Layout::Column)
            .child(fixed(40.0, 10.0, 1))
            .child(fixed(40.0, 10.0, 2));
        plain.layout(0.0, 0.0, &tr());
        assert_eq!(hit(&plain, 1).y, 0.0, "无旋转时不得映射");
        assert_eq!(hit(&plain, 2).y, 10.0);
    }

    /// 像素方向：局部缓冲左上角那一点，搬到屏幕后必须在**右上角**。
    /// 这条与 `local_top_left_maps_to_screen_top_right` 是同一约定的两个层面
    /// （矩形 / 像素），必须一起钉——两处各写一遍映射正是最容易错开一格的地方。
    #[test]
    fn pixel_blit_puts_local_origin_at_screen_top_right() {
        let (cw, ch) = (4u32, 3u32); // 局部 4×3 ⇒ 屏幕 3×4
        let mut tmp = vec![0u8; (cw * ch * 4) as usize];
        tmp[0..4].copy_from_slice(&[1, 2, 3, 4]); // 局部 (0,0)
        let (bw, bh) = (3u32, 4u32);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        blit_rotate(Rot::Cw, &tmp, cw, ch, &mut buf, bw, bh, 0, 0);
        let top_right = ((bw - 1) * 4) as usize; // 第 0 行、最后一列
        assert_eq!(&buf[top_right..top_right + 4], &[1, 2, 3, 4]);
        assert_eq!(&buf[0..4], &[0, 0, 0, 0], "左上角不该有东西");
    }

    /// 搬进来再搬回去必须是恒等——底子（窗口圆角/渐变/背景图）要原样保留，
    /// 且文本后端的差分法合成拿它当抗锯齿的背景基准，错一位文字边缘就带错误混色。
    #[test]
    fn blit_round_trip_is_identity() {
        let (cw, ch) = (5u32, 7u32);
        let (bw, bh) = (ch, cw);
        let orig: Vec<u8> = (0..(bw * bh * 4)).map(|i| (i % 251) as u8).collect();
        let mut buf = orig.clone();
        let mut tmp = vec![0u8; (cw * ch * 4) as usize];
        blit_unrotate(Rot::Cw, &buf, bw, bh, 0, 0, &mut tmp, cw, ch);
        blit_rotate(Rot::Cw, &tmp, cw, ch, &mut buf, bw, bh, 0, 0);
        assert_eq!(buf, orig, "往返不是恒等：底子会被搬歪");
    }

    /// 逆时针的方向判据：局部左上角落到屏幕**左下角**。
    ///
    /// ★ 必须与顺时针那条一起看：两条只测「宽高交换了」的话，一个把 Ccw 也实现成
    /// 顺时针的版本能全绿——而那样对联式竖排的每个字会是倒的（转了 180°）。
    #[test]
    fn ccw_puts_local_origin_at_screen_bottom_left() {
        let host = Rect {
            x: 100.0,
            y: 200.0,
            w: 10.0,
            h: 40.0,
        };
        let got = rotate_rect(
            Rot::Ccw,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 2.0,
                h: 3.0,
            },
            host,
        );
        assert_eq!(got.x, 100.0, "没贴到左缘 ⇒ 转反了");
        assert_eq!(got.y, 200.0 + 40.0 - 2.0, "没贴到下缘 ⇒ 转反了");
        assert_eq!((got.w, got.h), (3.0, 2.0), "宽高没交换");

        // 像素层同一约定（连续/离散两处最容易错开一格）。
        let (cw, ch) = (4u32, 3u32); // 局部 4×3 ⇒ 屏幕 3×4
        let mut tmp = vec![0u8; (cw * ch * 4) as usize];
        tmp[0..4].copy_from_slice(&[1, 2, 3, 4]); // 局部 (0,0)
        let (bw, bh) = (3u32, 4u32);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        blit_rotate(Rot::Ccw, &tmp, cw, ch, &mut buf, bw, bh, 0, 0);
        let bottom_left = ((bh - 1) * bw * 4) as usize;
        assert_eq!(&buf[bottom_left..bottom_left + 4], &[1, 2, 3, 4]);
        assert_eq!(&buf[0..4], &[0, 0, 0, 0], "左上角不该有东西");
    }

    /// ★★ 对联式竖排的**全部数学**就是这一条：外层顺时针套内层逆时针 ≡ 什么都没做。
    ///
    /// 它同时钉住三件事——两向的映射互为逆、嵌套时内层拿到的底子是对的、宽高交换两次
    /// 回到原值。任何一处方向写反，这里的像素就与直画不同（而单看某一向的测试都能全绿）。
    ///
    /// 用左右异色的两块而不是单色：单色块转 180° 也一样，测不出方向。
    #[test]
    fn cw_around_ccw_is_the_identity() {
        const RED: [u8; 4] = [0, 0, 255, 255];
        const BLUE: [u8; 4] = [255, 0, 0, 255];
        let leaf = || {
            View::container(Layout::Row)
                .child(
                    View::container(Layout::Row)
                        .fixed_w(2.0)
                        .fixed_h(2.0)
                        .bg(RED),
                )
                .child(
                    View::container(Layout::Row)
                        .fixed_w(2.0)
                        .fixed_h(2.0)
                        .bg(BLUE),
                )
        };
        let (bw, bh) = (4u32, 2u32);

        let mut plain = leaf();
        plain.layout(0.0, 0.0, &tr());
        assert_eq!(plain.measured_size(), (4.0, 2.0));
        let mut direct = vec![0u8; (bw * bh * 4) as usize];
        plain.paint(&mut direct, bw, bh, &tr());

        let mut nested = View::rotated_cw(View::rotated_ccw(leaf()));
        nested.layout(0.0, 0.0, &tr());
        assert_eq!(nested.measured_size(), (4.0, 2.0), "两次交换应回到原尺寸");
        let mut got = vec![0u8; (bw * bh * 4) as usize];
        nested.paint(&mut got, bw, bh, &tr());

        assert_eq!(got, direct, "cw∘ccw 不是恒等 ⇒ 竖排的字会歪或倒");
    }

    /// 嵌套下的命中矩形也必须还原——画面对而点击错是这类 bug 的典型形态。
    #[test]
    fn cw_around_ccw_restores_hit_rects() {
        let build = || {
            View::container(Layout::Row)
                .child(fixed(4.0, 2.0, 1))
                .child(fixed(6.0, 2.0, 2))
        };
        let mut plain = build();
        plain.layout(0.0, 0.0, &tr());
        let mut nested = View::rotated_cw(View::rotated_ccw(build()));
        nested.layout(0.0, 0.0, &tr());
        for tag in [1, 2] {
            let (n, p) = (hit(&nested, tag), hit(&plain, tag));
            assert_eq!(
                (n.x, n.y, n.w, n.h),
                (p.x, p.y, p.w, p.h),
                "tag {tag} 的命中区没还原"
            );
        }
    }

    /// 旋转块部分落在缓冲外时不得 panic，也不得写坏界内像素。
    #[test]
    fn out_of_bounds_blit_is_clipped_not_panicking() {
        let (cw, ch) = (4u32, 4u32);
        let tmp = vec![9u8; (cw * ch * 4) as usize];
        let (bw, bh) = (4u32, 4u32);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        // 原点在负象限：只有右下角一部分落在界内。
        blit_rotate(Rot::Cw, &tmp, cw, ch, &mut buf, bw, bh, -2, -2);
        assert_eq!(&buf[0..4], &[9, 9, 9, 9], "界内部分应被写到");
        // 完全在界外：一个字节都不该动。
        let mut buf2 = vec![0u8; (bw * bh * 4) as usize];
        blit_rotate(Rot::Cw, &tmp, cw, ch, &mut buf2, bw, bh, 100, 100);
        assert!(buf2.iter().all(|&b| b == 0));
    }
}

/// 盒模型布局测试。断言含文本尺寸，依赖 mock 文本测量
/// （`measure_text_sized` = 字符数 × 字号 × 0.6，行高 = 字号 × 1.2）。
/// macOS 文本后端为真 CoreText（mock 失效），故同 Windows 一并 gate 出。
#[cfg(all(test, not(windows), not(target_os = "macos")))]
mod layout_tests {
    use super::*;
    use crate::text::dwrite::TextRenderer;

    fn tr() -> TextRenderer {
        TextRenderer::new("test", 20.0).unwrap()
    }

    /// 构造一个固定尺寸的叶容器（无文本，尺寸跨平台确定）。
    fn fixed(w: f32, h: f32, tag: i32) -> View {
        View::container(Layout::Row).fixed_w(w).fixed_h(h).tag(tag)
    }

    /// 取某 tag 节点 arrange 后的绝对矩形。
    fn hit(root: &View, tag: i32) -> Rect {
        let mut out = Vec::new();
        root.collect_hits(&mut out);
        out.into_iter()
            .find(|(t, _)| *t == tag)
            .unwrap_or_else(|| panic!("tag {tag} 未命中"))
            .1
    }

    #[test]
    fn measure_row_sums_main_axis_with_gap() {
        let mut v = View::container(Layout::Row)
            .gap(10.0)
            .child(fixed(50.0, 20.0, -1))
            .child(fixed(50.0, 20.0, -1));
        v.layout(0.0, 0.0, &tr());
        assert_eq!(v.measured_size(), (110.0, 20.0)); // 50+50+gap10, 交叉轴取 max
    }

    #[test]
    fn measure_column_sums_main_axis_with_gap() {
        let mut v = View::container(Layout::Column)
            .gap(10.0)
            .child(fixed(50.0, 20.0, -1))
            .child(fixed(50.0, 20.0, -1));
        v.layout(0.0, 0.0, &tr());
        assert_eq!(v.measured_size(), (50.0, 50.0)); // 宽取 max50, 高 20+20+gap10
    }

    #[test]
    fn measure_adds_padding() {
        let mut v = View::container(Layout::Row)
            .pad(Edges::all(8.0))
            .child(fixed(50.0, 20.0, -1));
        v.layout(0.0, 0.0, &tr());
        assert_eq!(v.measured_size(), (66.0, 36.0)); // +16 两侧 padding
    }

    #[test]
    fn fixed_size_overrides_content() {
        let mut v = View::container(Layout::Row)
            .fixed_w(200.0)
            .fixed_h(30.0)
            .child(fixed(999.0, 999.0, -1));
        v.layout(0.0, 0.0, &tr());
        assert_eq!(v.measured_size(), (200.0, 30.0));
    }

    /// 宽度下限：内容不足时抬到下限。
    #[test]
    fn min_w_raises_narrow_content() {
        let mut v = View::container(Layout::Row)
            .min_w(200.0)
            .child(fixed(50.0, 20.0, -1));
        v.layout(0.0, 0.0, &tr());
        assert_eq!(v.measured_size(), (200.0, 20.0));
    }

    /// 反向对照：内容够宽时下限不得改变尺寸，否则 `min_w` 就退化成了 `fixed_w`。
    #[test]
    fn min_w_leaves_wide_content_untouched() {
        let mut v = View::container(Layout::Row)
            .min_w(200.0)
            .child(fixed(300.0, 20.0, -1));
        v.layout(0.0, 0.0, &tr());
        assert_eq!(v.measured_size(), (300.0, 20.0));
    }

    /// 候选窗最小宽度的核心机制：下限设在叶子上，宽度逐层冒到祖先，父级 padding 照常累加
    /// —— 调用方不必自己换算序号列宽与内边距。
    #[test]
    fn min_w_propagates_from_leaf_to_ancestor() {
        let mut v = View::container(Layout::Column).pad(Edges::all(8.0)).child(
            View::container(Layout::Row)
                .child(fixed(10.0, 20.0, -1))
                // fixed_w=30 且 min_w=120：下限在 fixed 之后施加，故取 120。
                .child(fixed(30.0, 20.0, -1).min_w(120.0)),
        );
        v.layout(0.0, 0.0, &tr());
        // 内层 Row = 10 + 120 = 130；外层再加两侧 padding 16。
        assert_eq!(v.measured_size().0, 146.0);
    }

    #[test]
    fn leaf_text_measured_via_mock() {
        let mut v = View::leaf("abc", [0, 0, 0, 255]).font_size(20.0);
        v.layout(0.0, 0.0, &tr());
        // mock: 3 字符 × 20 × 0.6 = 36 宽；20 × 1.2 = 24 高
        assert_eq!(v.measured_size(), (36.0, 24.0));
    }

    #[test]
    fn arrange_cross_center_offsets_child() {
        let mut root = View::container(Layout::Row)
            .cross(Align::Center)
            .fixed_h(40.0)
            .child(fixed(50.0, 20.0, 0));
        root.layout(0.0, 0.0, &tr());
        let r = hit(&root, 0);
        assert_eq!(r.x, 0.0);
        assert_eq!(r.y, 10.0); // (40-20)/2 居中
    }

    #[test]
    fn arrange_grow_spacer_absorbs_remaining() {
        let mut root = View::container(Layout::Row)
            .fixed_w(200.0)
            .child(fixed(50.0, 10.0, 0))
            .child(View::spacer())
            .child(fixed(50.0, 10.0, 2));
        root.layout(0.0, 0.0, &tr());
        assert_eq!(hit(&root, 0).x, 0.0);
        assert_eq!(hit(&root, 2).x, 150.0); // spacer 吸收 200-100=100，把末项推到 150
    }

    /// 「左固定 + 中间自由 + 右固定」——**不需要新的布局原语**，现有两件就够：
    /// 窄行加 `fill_cross` 撑到列的内容宽（= 最宽那行的宽度），内部 `spacer().grow()`
    /// 吃掉富余，末项就恒贴右缘。
    ///
    /// ★ 少了 `fill_cross`，行宽只等于自身内容宽，spacer 分不到一个像素，右组会跟着
    /// 左组浮动——软键盘的关闭按钮与翻页键都栽在这里，而且**面板越宽错得越明显**，
    /// 窄面板上看起来还挺对。
    #[test]
    fn fill_cross_row_lets_spacer_pin_the_last_child_right() {
        let mut root = View::container(Layout::Column)
            .child(fixed(200.0, 10.0, 9)) // 最宽的行，决定列宽
            .child(
                View::container(Layout::Row)
                    .fill_cross()
                    .child(fixed(20.0, 10.0, 0))
                    .child(View::spacer().grow())
                    .child(fixed(30.0, 10.0, 2)),
            );
        root.layout(0.0, 0.0, &tr());
        assert_eq!(hit(&root, 0).x, 0.0, "左组贴左缘");
        assert_eq!(hit(&root, 2).x, 170.0, "右组贴右缘 200-30");
    }

    #[test]
    fn arrange_fill_cross_stretches_width() {
        let mut root = View::container(Layout::Column).fixed_w(100.0).child(
            View::container(Layout::Column)
                .fixed_h(20.0)
                .fill_cross()
                .tag(0),
        );
        root.layout(0.0, 0.0, &tr());
        assert_eq!(hit(&root, 0).w, 100.0); // 跨轴撑满列内容宽
    }

    #[test]
    fn arrange_applies_child_margin() {
        let mut root = View::container(Layout::Row).child(fixed(10.0, 10.0, 0).margin(Edges {
            l: 5.0,
            t: 3.0,
            r: 0.0,
            b: 0.0,
        }));
        root.layout(0.0, 0.0, &tr());
        let r = hit(&root, 0);
        assert_eq!((r.x, r.y), (5.0, 3.0)); // margin 偏移子节点原点
    }

    /// 插入符是**覆盖层**：改变 caret 位置不得改变文本的布局尺寸。
    /// 这是回归测试——曾把 preedit 拆成「前半+竖线+后半」三节点，因
    /// `measure(a+b) != measure(a)+measure(b)` 且拆分点随光标而变，导致移动光标时编码位移。
    #[test]
    fn caret_never_affects_measured_size() {
        let tr = tr();
        let mut base = View::leaf("nihao", [0, 0, 0, 255]).font_size(20.0);
        base.layout(0.0, 0.0, &tr);
        let want = base.measured_size();
        for caret in 0..=5 {
            let mut v = View::leaf("nihao", [0, 0, 0, 255])
                .font_size(20.0)
                .caret_at(caret, 1.0);
            v.layout(0.0, 0.0, &tr);
            assert_eq!(
                v.measured_size(),
                want,
                "caret={caret} 不应改变文本布局尺寸"
            );
        }
    }

    /// caret_at 自夹到字符边界——paint 里 `&t[..cp]` 落在字符中间会 panic。
    #[test]
    fn caret_at_clamps_to_char_boundary() {
        // 「你」占 3 字节：1/2 退回 0，3 是边界；越界截到末尾（"你hao" = 6 字节）
        assert_eq!(
            View::leaf("你hao", [0; 4]).caret_at(1, 1.0).caret_at,
            Some(0)
        );
        assert_eq!(
            View::leaf("你hao", [0; 4]).caret_at(2, 1.0).caret_at,
            Some(0)
        );
        assert_eq!(
            View::leaf("你hao", [0; 4]).caret_at(3, 1.0).caret_at,
            Some(3)
        );
        assert_eq!(
            View::leaf("你hao", [0; 4]).caret_at(99, 1.0).caret_at,
            Some(6)
        );
    }

    #[test]
    fn collect_hits_depth_first_skips_untagged() {
        let mut root = View::container(Layout::Column)
            .child(fixed(10.0, 10.0, 0))
            .child(fixed(10.0, 10.0, 1).child(fixed(5.0, 5.0, 2)));
        root.layout(0.0, 0.0, &tr());
        let mut out = Vec::new();
        root.collect_hits(&mut out);
        let tags: Vec<i32> = out.iter().map(|(t, _)| *t).collect();
        assert_eq!(tags, vec![0, 1, 2]); // 先序遍历；root 默认 tag=-1 不收集
    }
}
