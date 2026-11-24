//! 精简 View 盒模型（measure → arrange → paint + 命中矩形提取）
//!
//! 与 Go 版本 `wind_input/internal/ui/viewbox*.go` 的核心子集对齐：
//! row/column 布局、padding/margin、背景/圆角/边框、固定尺寸、交叉轴对齐、
//! 文本叶子。布局分三步——measure 自底向上算尺寸，arrange 自顶向下定坐标，
//! paint 递归绘制；arrange 后每个带 tag 的节点都有绝对矩形，供鼠标命中复用。
//!
//! 九宫格图 / 阴影模糊 / z 分层 / 渐变背景均已支持。

use crate::text::{ColorRun, TextBackend, TextStyle};
use std::cell::RefCell;
use std::collections::HashMap;
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, PathBuilder,
    Pattern, PixmapMut, PixmapPaint, Point, RadialGradient, SpreadMode, Transform,
};
use wind_theme::schema::Dim;

/// 阴影蒙版缓存键：(盒宽, 盒高, 圆角, 模糊, 亚像素相位 x, 亚像素相位 y)。
///
/// 各项量化到 1/4 px：模糊后不可见，而量化能吸收 DPI 换算的浮点噪音，让几何不变的
/// 相邻帧稳定命中。
type ShadowKey = (i32, i32, i32, i32, i32, i32);

/// 蒙版量化因子（1/4 px）。
const SHADOW_Q: f32 = 4.0;

/// 蒙版缓存条数上限。候选窗的阴影几何种类有限（随候选数/码长变化），超限整体清空即可。
const SHADOW_CACHE_CAP: usize = 32;

/// 阴影蒙版缓存：蒙版表 + 最后取用时刻（闲置回收与图片缓存同一个
/// [`crate::image_cache::IDLE_EVICT_AFTER`]，设计 `docs/design/memory-footprint.md` §7）。
///
/// 条数上限只挡住「涨到多高」，挡不住「不用了也不落」（候选窗宽度几乎每帧在变、每个宽度
/// 一条），故另有闲置回收。
#[derive(Default)]
struct ShadowCache {
    masks: HashMap<ShadowKey, ShadowMask>,
    /// 最后一次被取用的时刻（命中也算）；None = 空表，没什么可回收。
    last_use: Option<crate::clock::Instant>,
}

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
    /// 缓存 alpha 蒙版而非着色后的像素：蒙版只由几何决定，换色不失效，内存也只要 1/4。
    /// 省掉的是每帧 3 趟可分离方框模糊，而候选窗尺寸在输入过程中高度重复。
    static SHADOW_CACHE: RefCell<ShadowCache> = RefCell::new(ShadowCache::default());
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
        let mut cache = c.borrow_mut();
        if let Some(m) = cache.masks.get(&key) {
            let r = f(m);
            cache.last_use = Some(crate::clock::now());
            return Some(r);
        }
        let mask = build_shadow_mask(bw, bh, radius, blur, phase_x, phase_y)?;
        if cache.masks.len() >= SHADOW_CACHE_CAP {
            cache.masks.clear();
        }
        cache.last_use = Some(crate::clock::now());
        Some(f(cache.masks.entry(key).or_insert(mask)))
    })
}

/// 源图原始尺寸（线程局部图片缓存）。皮肤布局要在绘制前知道它。
pub fn image_src_size(path: &str) -> Option<(u32, u32)> {
    IMAGE_CACHE.with(|c| c.borrow_mut().src_size(path))
}

/// 只读文件头的图片尺寸，见 [`crate::image_cache::ImageCache::header_size`]。
pub fn image_header_size(path: &str) -> Option<(u32, u32)> {
    IMAGE_CACHE.with(|c| c.borrow_mut().header_size(path))
}

/// 皮肤位图的最小尺寸与倍率下自然尺寸，见 [`crate::image_cache::ImageCache::skin_metrics`]。
pub fn skin_metrics(
    path: &str,
    k: f32,
    stretch_x: &wind_theme::skin::Stretch,
    stretch_y: &wind_theme::skin::Stretch,
) -> Option<(u32, u32, u32, u32)> {
    IMAGE_CACHE.with(|c| c.borrow_mut().skin_metrics(path, k, stretch_x, stretch_y))
}

/// 把一块皮肤位图（窗体或装饰）画到 `rect`（设备像素，已取整）。不裁剪——装饰本来就要伸出窗体。
#[allow(clippy::too_many_arguments)]
pub fn paint_skin_image(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    rect: (i32, i32, u32, u32),
    path: &str,
    k: f32,
    stretch_x: &wind_theme::skin::Stretch,
    stretch_y: &wind_theme::skin::Stretch,
    flip: wind_theme::Flip,
    opacity: f32,
) {
    let (x, y, w, h) = rect;
    IMAGE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let Some(fill) = cache.skin_fill(path, k, stretch_x, stretch_y, w, h, flip) else {
            return;
        };
        draw_clipped(
            buf,
            buf_w,
            buf_h,
            fill,
            (x as f32, y as f32, w as f32, h as f32),
            opacity,
            None,
        );
    });
}

/// 丢掉全部动画帧（换主题时），见 [`crate::image_cache::ImageCache::clear_anims`]。
pub fn clear_anim_frames() {
    IMAGE_CACHE.with(|c| c.borrow_mut().clear_anims());
}

/// 源是否动画格式，见 [`crate::image_cache::ImageCache::is_animated`]。
pub fn image_is_animated(path: &str) -> bool {
    IMAGE_CACHE.with(|c| c.borrow_mut().is_animated(path))
}

/// 动画装饰的时间轴，见 [`crate::image_cache::ImageCache::anim_layout`]。
pub fn anim_layout(
    path: &str,
    k: f32,
    w: u32,
    h: u32,
    fps: Option<f32>,
    budget: usize,
    flip: wind_theme::Flip,
) -> Option<crate::image_cache::AnimLayout> {
    IMAGE_CACHE.with(|c| c.borrow_mut().anim_layout(path, k, w, h, fps, budget, flip))
}

/// 把动画装饰的第 `frame` 帧画到 `rect`（设备像素）。帧位图与 [`anim_layout`] 同键缓存。
#[allow(clippy::too_many_arguments)]
pub fn paint_anim_frame(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    rect: (i32, i32, u32, u32),
    path: &str,
    k: f32,
    fps: Option<f32>,
    budget: usize,
    flip: wind_theme::Flip,
    frame: usize,
    opacity: f32,
) {
    let (x, y, w, h) = rect;
    IMAGE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let Some(pm) = cache.anim_frame(path, k, w, h, fps, budget, flip, frame) else {
            return;
        };
        draw_clipped(
            buf,
            buf_w,
            buf_h,
            pm,
            (x as f32, y as f32, w as f32, h as f32),
            opacity,
            None,
        );
    });
}

/// 绘制缓存（背景图 + 阴影蒙版）闲置回收的最早到期时刻，供 UI 消息循环登记进 `next_deadline`。
pub fn paint_cache_next_deadline() -> Option<crate::clock::Instant> {
    let image = IMAGE_CACHE.with(|c| c.borrow().next_deadline());
    let shadow = SHADOW_CACHE.with(|c| {
        c.borrow()
            .last_use
            .map(|t| t + crate::image_cache::IDLE_EVICT_AFTER)
    });
    image.into_iter().chain(shadow).min()
}

/// 闲置够久就把背景图缓存 / 阴影蒙版缓存各自整个丢掉（两者各按自己的最后取用计时），
/// 返回是否真的清了什么（清了才值得记一行日志）。
pub fn paint_cache_evict_if_idle(now: crate::clock::Instant) -> bool {
    let image = IMAGE_CACHE.with(|c| c.borrow_mut().evict_if_idle(now));
    let shadow = SHADOW_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        match c.last_use {
            Some(t) if now.saturating_duration_since(t) >= crate::image_cache::IDLE_EVICT_AFTER => {
                *c = ShadowCache::default();
                true
            }
            _ => false,
        }
    });
    image || shadow
}

/// 一套图片 / 阴影绘制缓存，由调用方持有（见 [`with_paint_caches`]）。
#[derive(Default)]
pub struct PaintCaches {
    image: crate::image_cache::ImageCache,
    shadow: ShadowCache,
}

/// 换上调用方持有的那套缓存跑 `f`，跑完把它（连同 `f` 里新增的条目）收回 `caches`、换回原来
/// 那套。闭包 panic 也会换回。
///
/// 给同线程里多个互不相干的出图方用（同页两个编辑器预览）：各用各的缓存，一方换主题清缓存不
/// 波及另一方，一方的大图也不会挤爆另一方的预算。
pub fn with_paint_caches<R>(caches: &mut PaintCaches, f: impl FnOnce() -> R) -> R {
    struct Swap<'a>(&'a mut PaintCaches);
    impl Drop for Swap<'_> {
        fn drop(&mut self) {
            swap_paint_caches(self.0);
        }
    }
    swap_paint_caches(caches);
    let _back = Swap(caches);
    f()
}

fn swap_paint_caches(c: &mut PaintCaches) {
    IMAGE_CACHE.with(|i| std::mem::swap(&mut *i.borrow_mut(), &mut c.image));
    SHADOW_CACHE.with(|s| std::mem::swap(&mut *s.borrow_mut(), &mut c.shadow));
}

/// 在一套临时的图片 / 阴影缓存里跑 `f`，跑完丢掉、换回原来那套。
///
/// 给一次性出图（设置端预览）用：它画的是别的主题、甚至临时目录里的主题，进了实时窗口
/// 那套线程局部缓存，大图会触发超预算整体清空（连实时窗口的源图与图集一起），临时路径
/// 还会让句柄表越积越多。闭包 panic 也会换回原缓存。
pub fn with_isolated_paint_caches<R>(f: impl FnOnce() -> R) -> R {
    with_paint_caches(&mut PaintCaches::default(), f)
}

/// 仅测试可见：直接操作线程局部的图片缓存（wind-ui 的等待回收测试要在里面布置陈旧驻留）。
#[cfg(any(test, feature = "test-util"))]
pub fn with_image_cache_for_test<R>(f: impl FnOnce(&mut crate::image_cache::ImageCache) -> R) -> R {
    IMAGE_CACHE.with(|c| f(&mut c.borrow_mut()))
}

/// 仅测试可见：当前阴影蒙版缓存条目数。
#[cfg(test)]
fn shadow_cache_len() -> usize {
    SHADOW_CACHE.with(|c| c.borrow().masks.len())
}

/// 仅测试可见：清空阴影蒙版缓存，让用例从确定状态起步。
#[cfg(test)]
fn shadow_cache_clear() {
    SHADOW_CACHE.with(|c| *c.borrow_mut() = ShadowCache::default());
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
    /// 仅 nine_slice：中段沿 [x, y] 轴平铺而非拉伸。
    pub slice_repeat: [bool; 2],
    pub opacity: f32,
    /// 单色染色（None=图原样）；非 None 时把图当 alpha mask、用此色填充（单色 SVG/图标随主题变色）。
    pub tint: Option<[u8; 4]>,
    /// None=铺满节点（mode 作用于整个节点盒）；Some=按定位摆成一块，mode 作用于这块，
    /// 超出节点圆角盒的部分裁掉。
    pub place: Option<ImagePlace>,
    /// DPI 缩放：SVG 的 center / tile 以「文档尺寸 × scale」为单元（位图按纹理像素，不用它）。
    pub scale: f32,
    /// 镜像：按 mode 合成到目标尺寸后再翻；切片与定位按源图写、不翻。
    pub flip: wind_theme::Flip,
}

/// 图片在 host 内的定位：九宫锚点 + 偏移 + 目标尺寸。背景图与覆盖图共用。
#[derive(Clone, Debug, PartialEq)]
pub struct ImagePlace {
    /// top-left | top | … | center | … | bottom-right；空=居中（见 [`anchor_pos`]）。
    pub anchor: String,
    /// dp 偏移（已 ×scale，px）。
    pub off_x: f32,
    pub off_y: f32,
    /// 百分比偏移（相对 host 宽/高；paint 期换算）。与 dp 偏移叠加。
    pub off_x_pct: f32,
    pub off_y_pct: f32,
    /// 目标尺寸 px（已 ×scale）；≤0 = 未给，规则见 [`ImagePlace::rect`]。
    pub w: f32,
    pub h: f32,
    /// DPI 缩放：两维都没给 size 时，原图尺寸要乘它（SVG 为文档尺寸）。
    pub scale: f32,
}

impl Default for ImagePlace {
    fn default() -> Self {
        Self {
            anchor: String::new(),
            off_x: 0.0,
            off_y: 0.0,
            off_x_pct: 0.0,
            off_y_pct: 0.0,
            w: 0.0,
            h: 0.0,
            scale: 1.0,
        }
    }
}

/// 定位图（背景图定位块 / 覆盖图）单边像素上限。
///
/// 尺寸来自主题（不可信）：按原图比例推算可能推出天文数字，而填充位图按这个尺寸整块分配。
/// 定位图最终裁到宿主内，截掉的部分本就看不见。4096 保留 200% 屏上 2048dp 的合法大图，
/// 单张最坏 64 MiB。两维各自截、不保比例（只有病态主题会碰到）。
const MAX_PLACED_EDGE: f32 = 4096.0;

impl ImagePlace {
    /// host 内的目标矩形 `(x, y, w, h)`（已取整）：anchor 九宫基位 + offset（dp px + 百分比
    /// 相对 host 宽/高）。`src` 取原图尺寸（SVG 为文档尺寸），只在有一维没给 size 时才调用
    /// （免得白解码）。尺寸规则（位图与 SVG 同口径）：
    /// - 两维都给：按给的来；
    /// - 只给一维：另一维 = 给定维 × 原图对应宽高比；
    /// - 都不给：原图尺寸 × `scale`（DPI）。
    ///
    /// 结果两维各截到 [`MAX_PLACED_EDGE`]（防病态主题撑爆填充位图）。
    fn rect(
        &self,
        host: Rect,
        src: impl FnOnce() -> Option<(u32, u32)>,
    ) -> Option<(f32, f32, f32, f32)> {
        let (w, h) = if self.w > 0.0 && self.h > 0.0 {
            (self.w, self.h)
        } else {
            let (sw, sh) = src()?;
            let (sw, sh) = (sw.max(1) as f32, sh.max(1) as f32);
            if self.w > 0.0 {
                (self.w, self.w * sh / sw)
            } else if self.h > 0.0 {
                (self.h * sw / sh, self.h)
            } else {
                (sw * self.scale, sh * self.scale)
            }
        };
        let cap = |v: f32| v.round().clamp(1.0, MAX_PLACED_EDGE);
        let (w, h) = (cap(w), cap(h));
        let (ax, ay) = anchor_pos(&self.anchor, host, w, h);
        let x = (ax + self.off_x + self.off_x_pct / 100.0 * host.w).round();
        let y = (ay + self.off_y + self.off_y_pct / 100.0 * host.h).round();
        Some((x, y, w, h))
    }
}

/// z 层级覆盖图（按 anchor 九宫定位 + offset + size 绘于 host 内）。
#[derive(Clone, Debug)]
pub struct ViewLayer {
    pub path: String,
    pub z: i32,
    pub place: ImagePlace,
    pub opacity: f32,
    /// 单色染色，语义同背景图 [`ViewImage::tint`]（alpha 蒙版）；`[0,0,0,0]`=原样。
    /// 图层不支持 disabled_tint。
    pub tint: [u8; 4],
    /// 镜像，语义同背景图 [`ViewImage::flip`]。
    pub flip: wind_theme::Flip,
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
#[derive(Clone, Copy, Default, Debug)]
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    Row,
    Column,
}

/// 对齐方式（交叉轴 / 文本水平）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

    /// 与另一矩形的交集；不相交时给 `None`（不返回零尺寸矩形——那会让调用方
    /// 把「完全裁掉」当成「有个宽 0 的可点区域」）。
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        (x1 > x0 && y1 > y0).then_some(Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    /// 包住两者的最小矩形。
    fn union(&self, o: &Rect) -> Rect {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
}

/// 左侧强调条参数（选中候选的竖条）。覆盖层语义：不参与 measure/布局，仅 paint 期
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
/// 只有 ±90° 两向——180° 用不到，加进来反而要求映射函数处理宽高不交换的情形。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Rot {
    /// 不旋转（绝大多数节点）。
    #[default]
    None,
    /// 顺时针 90°：局部左上角落到屏幕右上角 ⇒ 一行从左到右的文字转成一列从上到下。
    Cw,
    /// 逆时针 90°：局部左上角落到屏幕左下角。单独用没意义，它的用途是抵消外层的
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
    /// 富余只可能来自 `fixed_h`/`min_h`（Column）或 `fixed_w`/`min_w`（Row）。候选窗根容器
    /// 上翻时用 `End`：空白落在顶部，贴光标的底边才不会被顶开。
    ///
    /// 与 `grow` 互斥：有 `grow` 子节点时富余已被它们吸收，本项自然失效。
    pub main_align: Align,
    pub fixed_w: Option<f32>,
    pub fixed_h: Option<f32>,
    /// 宽度下限（设备像素）：测得宽度不足时抬到此值，超出则按内容。
    ///
    /// 与 `fixed_w` 的区别是「不得窄于」而非「就是这么宽」，候选窗根容器用它施加窗口最小宽度。
    ///
    /// 撑出的富余在 `arrange` 阶段分配：`fill_cross` 子节点跟着撑满，其余按 `cross_align`
    /// 落位，故只需给根容器加下限。
    pub min_w: Option<f32>,
    /// 高度下限（设备像素）：测得高度不足时抬到此值，超出则按内容。与 `min_w` 对称。
    ///
    /// 富余高度默认落在主轴末端，要落到另一端用 [`Self::main_align`] 或 `grow` 节点。
    pub min_h: Option<f32>,
    pub bg: Option<[u8; 4]>,
    pub corner_radius: f32,
    /// 边框 (颜色, 宽度)
    pub border: Option<([u8; 4], f32)>,
    pub text: Option<String>,
    pub text_color: [u8; 4],
    /// 分段颜色：按字节区间覆盖 `text_color`。空 = 整段 `text_color`（出厂路径）。
    ///
    /// 只在 paint 消费，不参与 measure，理由同 [`View::caret_at`]：拆成多个节点各自测量
    /// 会让宽度随颜色边界抖动。见 [`TextBackend::draw_runs`]。
    pub color_runs: Vec<ColorRun>,
    /// 文本字号（设备像素）；None=用渲染器基准字号。序号/注释按相对偏移设具体值。
    pub font_size: Option<f32>,
    /// 文本字重（400/500/700…）；None/0=继承渲染器默认（NORMAL）。
    pub font_weight: Option<i32>,
    /// 文本字体族覆盖；None/空=用渲染器全局字体族。
    pub font_family: Option<String>,
    pub text_align: Align,
    /// 文本内插入符位置（字节偏移，恒在字符边界；`None`=不画）。
    ///
    /// 覆盖层语义：只在 paint 阶段按偏移画一条竖线，不参与 measure。文本整串一次整形，宽度
    /// 与光标位置无关；拆成「前半 + 竖线 + 后半」会因字距与舍入让宽度随光标抖动。
    pub caret_at: Option<usize>,
    /// 插入符竖线宽度（像素；调用方按 DPI 缩放传入，[`View::caret_at`] 取整到整像素）。
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
    /// 把子树裁到本节点的矩形内：溢出的部分既不绘制，也不参与命中（滚动视口的地基）。
    ///
    /// 实现是「画完把界外像素还原」，只裁子节点，本节点自己的背景/边框不受裁剪。
    pub clip: bool,
    /// 把子树裁到本节点的圆角轮廓内（只处理四个圆角处）：子节点配直角（或圆角比本节点小）
    /// 时，贴边的高亮不会在圆角外露出方角——候选窗据此做出「首尾随窗口大圆弧、内部直角填充」
    /// 的胶囊形态。与 [`Self::clip`] 的矩形裁剪互补，裁的是矩形里被圆角削掉的那四小块。
    ///
    /// 本节点的边框因此推迟到子节点之后绘制（否则贴边的子节点会盖住描边环）。
    pub clip_corners: bool,
    /// 命中标识：>=0 参与命中收集（如候选下标 / 按钮 id），<0 忽略
    pub tag: i32,
    /// 预览节点身份（0 = 无）：只供主题编辑器预览按节点收集矩形（[`View::collect_nodes`]），
    /// 不参与布局、绘制与命中。取值由场景自定（候选窗见 `candidate::NodeKey`）。
    pub node: u8,
    /// 旋转 90° 呈现整棵子树（蒙古文等纵向书写的脚本用）。
    ///
    /// # 形态约束（只支持这一种，越界即 debug_assert）
    ///
    /// 本节点必须是恰好一个子节点、无自身装饰的裸包裹层（越界即 debug_assert）：装饰挂在
    /// 子节点上一起转，本节点只负责坐标系变换。
    ///
    /// 用临时缓冲双向旋转而不让文本后端转：后端变换只能转文字、转不了背景/边框；纯像素操作
    /// 让各平台后端自动跟上。已知代价：每帧多一次后端表面重建。
    ///
    /// 两向都要且允许嵌套：直立竖排是外层顺时针 + 每个字逆时针。[`View::paint_rotated`] 会把
    /// 底子搬进临时缓冲再搬回，嵌套时内层看到的仍是正确的背景。
    /// 设计见 `docs/design/mongolian-vertical-candidates.md` 的「渲染：临时缓冲双向旋转」。
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
            color_runs: Vec::new(),
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
            clip: false,
            clip_corners: false,
            tag: -1,
            node: 0,
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

    /// 把子树裁到本节点矩形内（滚动视口用）。见 [`Self::clip`] 字段说明。
    pub fn clipped(mut self) -> Self {
        self.clip = true;
        self
    }

    /// 把子树裁到本节点的圆角轮廓内。见 [`Self::clip_corners`]。
    pub fn clip_to_corners(mut self) -> Self {
        self.clip_corners = true;
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
    /// 设置分段颜色（字节区间，见 [`View::color_runs`]）。
    pub fn color_runs(mut self, runs: Vec<ColorRun>) -> Self {
        self.color_runs = runs;
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
        // 取整到整设备像素必须在这里：`snap_to_pixels` 只对整数尺寸保尺寸，传 1.5 会让竖线
        // 粗细随插入符移动在 1px 与 2px 之间闪。
        self.caret_w = width.round().max(1.0);
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
    pub fn node(mut self, id: u8) -> Self {
        self.node = id;
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

    /// 逆时针版。用途只有一个：套在已被外层顺时针旋转的子树内部，把单个字扶正。
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
    pub fn layout(&mut self, x: f32, y: f32, tr: &dyn TextBackend) {
        self.measure(tr);
        self.arrange(x, y);
    }

    /// 本节点文本的排版样式（字号回退到渲染器基准字号）。
    ///
    /// measure、paint 的水平对齐、caret 量前半段三处共用，必须完全一致：样式一分叉，
    /// 文字会整体偏移或超出节点框。
    fn text_style(&self, tr: &dyn TextBackend) -> TextStyle<'_> {
        TextStyle {
            family: self.font_family.as_deref(),
            size: self.font_size.unwrap_or(tr.base_size()),
            weight: self.font_weight.unwrap_or(0),
        }
    }

    fn measure(&mut self, tr: &dyn TextBackend) {
        if self.rot != Rot::None {
            debug_assert_eq!(
                self.children.len(),
                1,
                "旋转节点必须恰好一个子节点（见 View::rot 的形态约束）"
            );
            // 子树按未旋转测量，本节点对外交换宽高——这是整个旋转能力的支点：
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
            // 子树排在局部未旋转坐标系里、原点 (0,0)：paint 时它被画进一张同尺寸的
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

    /// 子树（含自身）绘制可能触及的包围盒。`clip` 用它决定要备份哪一块。
    fn subtree_bbox(&self) -> Rect {
        let mut r = self.rect;
        for c in &self.children {
            r = r.union(&c.subtree_bbox());
        }
        r
    }

    /// 测得尺寸（measure 后有效）
    pub fn measured_size(&self) -> (f32, f32) {
        (self.mw, self.mh)
    }

    /// 整棵树逐节点的文本转储（每节点一行，按深度缩进），供渲染 golden 对拍。
    ///
    /// 每行只列与默认值不同的字段（布局结果 `m`/`rect` 恒列）。手写而不 `#[derive(Debug)]`：
    /// 派生输出会随新增字段整体变形，而 golden 要守的恰恰是「加了新能力、出厂路径的树逐字节
    /// 不变」——新字段取默认值时不出现，出厂路径的转储就与加字段前相同。
    #[cfg(any(test, feature = "test-util"))]
    pub fn debug_dump(&self) -> String {
        let mut out = String::new();
        self.dump_into(&mut out, 0, &View::default());
        out
    }

    #[cfg(any(test, feature = "test-util"))]
    fn dump_into(&self, out: &mut String, depth: usize, d: &View) {
        let mut line = format!("{:indent$}{:?}", "", self.layout, indent = depth * 2);
        let mut field = |name: &str, v: String, dv: String| {
            if v != dv {
                line.push_str(&format!(" {name}={v}"));
            }
        };
        macro_rules! f {
            ($name:literal, $($field:tt)+) => {
                field($name, format!("{:?}", self.$($field)+), format!("{:?}", d.$($field)+))
            };
        }
        f!("margin", margin);
        f!("padding", padding);
        f!("gap", gap);
        f!("cross", cross_align);
        f!("main", main_align);
        f!("fixed_w", fixed_w);
        f!("fixed_h", fixed_h);
        f!("min_w", min_w);
        f!("min_h", min_h);
        f!("bg", bg);
        f!("radius", corner_radius);
        f!("border", border);
        f!("text", text);
        f!("color", text_color);
        f!("runs", color_runs);
        f!("size", font_size);
        f!("weight", font_weight);
        f!("family", font_family);
        f!("align", text_align);
        f!("caret", caret_at);
        f!("caret_w", caret_w);
        f!("bar", left_bar);
        f!("circle", circle_bg);
        f!("image", bg_image);
        f!("gradient", bg_gradient);
        f!("layers", layers);
        f!("grow", grow);
        f!("fill", fill_cross);
        f!("clip", clip);
        f!("tag", tag);
        f!("rot", rot);
        line.push_str(&format!(
            " m=({:?},{:?}) rect={:?}\n",
            self.mw, self.mh, self.rect
        ));
        out.push_str(&line);
        for c in &self.children {
            c.dump_into(out, depth + 1, d);
        }
    }

    /// 排布后的绝对矩形（layout 后有效）。仅供测试断言「某节点实际占多宽」——
    /// `measured_size` 是撑开前的量，被 `fill_cross` 拉宽的部分看不到。
    #[cfg(any(test, feature = "test-util"))]
    pub fn laid_rect(&self) -> Rect {
        self.rect
    }

    /// 旋转 90° 绘制子树（方向由 [`Self::rot`] 定）。
    ///
    /// 三步：把屏幕上那块底子逆向旋转搬进临时缓冲 → 子树照常画进去 → 正向旋转搬回。
    ///
    /// 第一步不能省、也不能用「填透明」代替：文本后端的差分法合成要拿目标像素当背景做
    /// 抗锯齿混合，窗口的圆角、渐变、九宫格背景也都在那块底子里。
    fn paint_rotated(&self, buf: &mut [u8], buf_w: u32, buf_h: u32, tr: &dyn TextBackend) {
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
    /// 穿过 [`View::rot`] 节点时必须把子树的矩形从局部空间映射回屏幕空间，否则悬停/点击
    /// 会落到相邻候选上。
    pub fn collect_hits(&self, out: &mut Vec<(i32, Rect)>) {
        self.collect_hits_clipped(out, None);
    }

    /// `clip` 是祖先施加的裁剪框（多层嵌套取交集）。
    ///
    /// 命中区必须与看得见的部分一致：被裁掉一半的标签只有露出来那半能点，
    /// 否则鼠标会在容器外面的空白处「点到」一个不可见的控件。
    fn collect_hits_clipped(&self, out: &mut Vec<(i32, Rect)>, clip: Option<Rect>) {
        let visible = match clip {
            Some(c) => self.rect.intersect(&c),
            None => Some(self.rect),
        };
        if self.tag >= 0
            && let Some(r) = visible
        {
            out.push((self.tag, r));
        }
        // 本节点完全在裁剪框外 ⇒ 不再往下走。
        //
        // 前提是「子节点画在父节点的矩形之内」：负 margin 把子节点拉回框内时会漏掉其命中区。
        // 按 `subtree_bbox` 判断要 O(n²)，而这种组合在 clip 容器内没有出现过；真要用，先改这条。
        if clip.is_some() && visible.is_none() {
            return;
        }
        let child_clip = if self.clip { visible } else { clip };

        // 旋转节点：子树在未旋转的局部空间里收集，映射回屏幕空间之后才谈得上裁剪。
        //
        // 裁剪框在屏幕空间，故顺序只能是「先 rotate_rect 回屏幕空间，再按 child_clip 裁」。
        if self.rot != Rot::None {
            let mut local = Vec::new();
            for c in &self.children {
                c.collect_hits(&mut local);
            }
            out.extend(local.into_iter().filter_map(|(t, r)| {
                let screen = rotate_rect(self.rot, r, self.rect);
                match child_clip {
                    Some(c) => screen.intersect(&c).map(|x| (t, x)),
                    None => Some((t, screen)),
                }
            }));
            return;
        }

        for c in &self.children {
            c.collect_hits_clipped(out, child_clip);
        }
    }

    /// 先序收集打了 [`View::node`] 的节点矩形（预览点选用，实时窗口不调）。不按 `clip` 裁剪；
    /// 旋转子树映射回屏幕空间。
    pub fn collect_nodes(&self, out: &mut Vec<(u8, Rect)>) {
        if self.node != 0 {
            out.push((self.node, self.rect));
        }
        if self.rot != Rot::None {
            let mut local = Vec::new();
            for c in &self.children {
                c.collect_nodes(&mut local);
            }
            out.extend(
                local
                    .into_iter()
                    .map(|(k, r)| (k, rotate_rect(self.rot, r, self.rect))),
            );
            return;
        }
        for c in &self.children {
            c.collect_nodes(out);
        }
    }

    /// 递归绘制到 BGRA 缓冲区
    pub fn paint(&self, buf: &mut [u8], buf_w: u32, buf_h: u32, tr: &dyn TextBackend) {
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
        // 圆角裁剪：此后画的（z<0 层、子节点、z>=0 层）凡落在圆角外的，最后还原成此刻的像素
        // （见 `clip_corners`）。备份取在 z<0 层之前，否则底层图会从圆角处露出去。
        let round_clip = self.clip_corners && self.corner_radius > 0.0;
        let corner_saved = if round_clip {
            CornerSave::take(buf, buf_w, buf_h, r, self.corner_radius)
        } else {
            None
        };
        // z<0 覆盖图（在内容下方）。
        for layer in self.layers.iter().filter(|l| l.z < 0) {
            paint_layer(buf, buf_w, buf_h, r, layer);
        }
        // 边框压在背景图与 z<0 层之上（与主题编辑器预览同序），否则会被整窗背景图盖掉。
        if let Some((bc, bw)) = self.border.filter(|_| !round_clip) {
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
            // 没有分段颜色就走原来的 `draw`，出厂路径一个调用都不变（golden 对拍守着）。
            let _ = if self.color_runs.is_empty() {
                tr.draw(
                    buf,
                    buf_w,
                    buf_h,
                    tx.max(r.x),
                    ty.max(r.y),
                    t,
                    &ts,
                    self.text_color,
                )
            } else {
                tr.draw_runs(
                    buf,
                    buf_w,
                    buf_h,
                    tx.max(r.x),
                    ty.max(r.y),
                    t,
                    &ts,
                    self.text_color,
                    &self.color_runs,
                )
            };
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
        // 子节点。
        //
        // 裁剪用「画完还原界外像素」而不是给每个绘制函数加 clip 参数：对所有绘制方式（含以后
        // 新加的）一视同仁。代价是那块像素画两遍，只在开了 clip 的少数节点上发生。
        if self.clip {
            let bbox = self.subtree_bbox();
            // 溢出区域为空 ⇒ 无需备份，直接画（绝大多数帧走这里：内容没超出）。
            match snapshot(buf, buf_w, buf_h, bbox) {
                Some(saved) if !contains_rect(&self.rect, &bbox) => {
                    for c in &self.children {
                        c.paint(buf, buf_w, buf_h, tr);
                    }
                    restore_outside(buf, buf_w, buf_h, bbox, self.rect, &saved);
                }
                _ => {
                    for c in &self.children {
                        c.paint(buf, buf_w, buf_h, tr);
                    }
                }
            }
        } else {
            for c in &self.children {
                c.paint(buf, buf_w, buf_h, tr);
            }
        }
        // z>=0 覆盖图（在内容上方）。先于圆角还原画：覆盖图本身只裁到矩形，要靠还原才进圆角。
        for layer in self.layers.iter().filter(|l| l.z >= 0) {
            paint_layer(buf, buf_w, buf_h, r, layer);
        }
        if let Some(saved) = corner_saved {
            saved.restore(buf, buf_w, buf_h);
        }
        // 推迟的边框：压在子节点与圆角裁剪之上。
        if round_clip && let Some((bc, bw)) = self.border {
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
    }
}

/// 圆角裁剪要还原的像素：窗口矩形四个角上的小块（直边部分不会被圆角削到，不必备份）。
///
/// 备份取在子节点绘制之前；子节点画完后 [`Self::restore`] 把块内圆角轮廓之外的像素按抗锯齿
/// 覆盖率混回备份，等价于「子节点以圆角轮廓为蒙版绘制」。基于像素而不是改每个绘制函数，
/// 所以渐变、阴影、文字、图片等任何绘制方式都一视同仁。
struct CornerSave {
    /// 窗口矩形（已对齐像素网格）与圆角半径，掩码据此重画。
    sx: f32,
    sy: f32,
    sw: f32,
    sh: f32,
    radius: f32,
    /// 掩码方块边长（见 `take`）。
    k: u32,
    /// 还原块：互不重叠，已夹到缓冲内。
    blocks: Vec<CornerBlock>,
}

struct CornerBlock {
    /// 该块所属的角（决定掩码方块贴在窗口哪个角）。
    right: bool,
    bottom: bool,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    saved: Vec<u8>,
}

impl CornerSave {
    fn take(buf: &[u8], buf_w: u32, buf_h: u32, rect: Rect, radius: f32) -> Option<Self> {
        let (sx, sy, sw, sh) = snap_to_pixels(rect.x, rect.y, rect.w, rect.h);
        if sw < 1.0 || sh < 1.0 {
            return None;
        }
        let (mw, mh) = (sw as u32, sh as u32);
        // 圆弧最多占到 k 个像素（+1 容纳抗锯齿的半像素）；与 push_round_rect 的半径钳制同源。
        let k = (radius.round().max(0.0) as u32)
            .min(mw / 2)
            .min(mh / 2)
            .saturating_add(1);
        // 左右、上下各一条带；右/下带从 max(k, 总长-k) 起，奇数尺寸下不与左/上带重叠，
        // 否则同一像素会被混合两次。
        let cols = [(0, k.min(mw)), (k.max(mw.saturating_sub(k)), mw)];
        let rows = [(0, k.min(mh)), (k.max(mh.saturating_sub(k)), mh)];
        let mut blocks = Vec::with_capacity(4);
        for (ri, &(ya, yb)) in rows.iter().enumerate() {
            for (ci, &(xa, xb)) in cols.iter().enumerate() {
                if xb <= xa || yb <= ya {
                    continue;
                }
                let block = Rect {
                    x: sx + xa as f32,
                    y: sy + ya as f32,
                    w: (xb - xa) as f32,
                    h: (yb - ya) as f32,
                };
                let Some((x0, y0, x1, y1)) = clamp_px(block, buf_w, buf_h) else {
                    continue;
                };
                let rect = Rect {
                    x: x0 as f32,
                    y: y0 as f32,
                    w: (x1 - x0) as f32,
                    h: (y1 - y0) as f32,
                };
                if let Some(saved) = snapshot(buf, buf_w, buf_h, rect) {
                    blocks.push(CornerBlock {
                        right: ci == 1,
                        bottom: ri == 1,
                        x0,
                        y0,
                        x1,
                        y1,
                        saved,
                    });
                }
            }
        }
        (!blocks.is_empty()).then_some(Self {
            sx,
            sy,
            sw,
            sh,
            radius,
            k,
            blocks,
        })
    }

    /// 圆角轮廓之外的像素还原成备份，边缘按覆盖率混合。
    ///
    /// 轮廓掩码用 `fill_rounded` 画同一个窗口矩形（整数平移，覆盖率与背景的弧线一致），
    /// 但只画该角的 k×k 方块，不分配整窗大小的缓冲。掩码方块始终包住整段圆弧：若画布边缘
    /// 从弧线中间切过，tiny-skia 会走裁剪光栅化，覆盖率可差到 6%（胶囊形实测）。
    fn restore(&self, buf: &mut [u8], buf_w: u32, _buf_h: u32) {
        let (mw, mh) = (self.sw as u32, self.sh as u32);
        let k = self.k;
        for b in &self.blocks {
            // 掩码方块在窗口里的左上角。
            let mx = self.sx
                + if b.right {
                    (mw - k.min(mw)) as f32
                } else {
                    0.0
                };
            let my = self.sy
                + if b.bottom {
                    (mh - k.min(mh)) as f32
                } else {
                    0.0
                };
            let (mkw, mkh) = (k.min(mw), k.min(mh));
            let mut mask = vec![0u8; (mkw * mkh * 4) as usize];
            fill_rounded(
                &mut mask,
                mkw,
                mkh,
                self.sx - mx,
                self.sy - my,
                self.sw,
                self.sh,
                [255, 255, 255, 255],
                self.radius,
            );
            let (bw, bh) = (b.x1 - b.x0, b.y1 - b.y0);
            for y in 0..bh {
                for x in 0..bw {
                    let (ax, ay) = ((b.x0 + x) as f32 - mx, (b.y0 + y) as f32 - my);
                    // 还原块必在掩码方块内（见 `take` 的分带），保险起见越界按圆角外处理。
                    let cov = if ax < 0.0 || ay < 0.0 || ax >= mkw as f32 || ay >= mkh as f32 {
                        0
                    } else {
                        mask[((ay as u32 * mkw + ax as u32) * 4 + 3) as usize] as i32
                    };
                    if cov == 255 {
                        continue;
                    }
                    let dst = (((b.y0 + y) * buf_w + b.x0 + x) * 4) as usize;
                    let src = ((y * bw + x) * 4) as usize;
                    for c in 0..4 {
                        let (cur, old) = (buf[dst + c] as i32, b.saved[src + c] as i32);
                        buf[dst + c] = (old + ((cur - old) * cov + 127) / 255) as u8;
                    }
                }
            }
        }
    }
}

/// `outer` 是否完全包住 `inner`（含边界）。
fn contains_rect(outer: &Rect, inner: &Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

/// 把矩形按缓冲边界夹取成整数像素范围 `(x0, y0, x1, y1)`；空则 `None`。
fn clamp_px(r: Rect, buf_w: u32, buf_h: u32) -> Option<(u32, u32, u32, u32)> {
    let x0 = r.x.floor().max(0.0) as u32;
    let y0 = r.y.floor().max(0.0) as u32;
    let x1 = (r.x + r.w).ceil().max(0.0).min(buf_w as f32) as u32;
    let y1 = (r.y + r.h).ceil().max(0.0).min(buf_h as f32) as u32;
    (x1 > x0 && y1 > y0 && x0 < buf_w && y0 < buf_h).then_some((
        x0,
        y0,
        x1.min(buf_w),
        y1.min(buf_h),
    ))
}

/// 备份一块矩形区域的像素（BGRA，逐行拷贝）。
fn snapshot(buf: &[u8], buf_w: u32, buf_h: u32, r: Rect) -> Option<Vec<u8>> {
    let (x0, y0, x1, y1) = clamp_px(r, buf_w, buf_h)?;
    let row = ((x1 - x0) * 4) as usize;
    let mut out = Vec::with_capacity(row * (y1 - y0) as usize);
    for y in y0..y1 {
        let off = ((y * buf_w + x0) * 4) as usize;
        out.extend_from_slice(&buf[off..off + row]);
    }
    Some(out)
}

/// 把 `bbox` 内、`keep` 外的像素还原成备份——即「裁剪」。
fn restore_outside(buf: &mut [u8], buf_w: u32, buf_h: u32, bbox: Rect, keep: Rect, saved: &[u8]) {
    let Some((x0, y0, x1, y1)) = clamp_px(bbox, buf_w, buf_h) else {
        return;
    };
    let row = ((x1 - x0) * 4) as usize;
    // keep 也夹到缓冲内；完全落在缓冲外时整块还原。
    let keep_px = clamp_px(keep, buf_w, buf_h);
    for y in y0..y1 {
        let src_row = (y - y0) as usize * row;
        for x in x0..x1 {
            let inside = keep_px
                .is_some_and(|(kx0, ky0, kx1, ky1)| x >= kx0 && x < kx1 && y >= ky0 && y < ky1);
            if inside {
                continue;
            }
            let dst = ((y * buf_w + x) * 4) as usize;
            let src = src_row + ((x - x0) * 4) as usize;
            buf[dst..dst + 4].copy_from_slice(&saved[src..src + 4]);
        }
    }
}

// ———————————————— 像素绘制工具 ————————————————

/// 贝塞尔逼近圆弧的控制点比例（kappa = 4/3·(√2−1) ≈ 0.552_284_75）。
/// 字面量按 f32 可表示的最近值书写——多写的位数 f32 存不下，只会误导读者。
const KAPPA: f32 = 0.552_284_8;

/// 抗锯齿纯色画笔。`color` 为直通 [R,G,B,A]。
///
/// R/B 在此交换：BGRA 缓冲被当作 tiny-skia 的 RGBA Pixmap 直接渲染（零拷贝），故传色时取
/// [B,G,R,A]，输出即合法的预乘 BGRA。各填充函数都经这里取画笔，别各自交换。
#[inline]
fn aa_paint(color: [u8; 4]) -> Paint<'static> {
    let mut paint = Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color(Color::from_rgba8(color[2], color[1], color[0], color[3]));
    paint
}

/// 绘制背景渐变：以 tiny-skia 线性/径向着色器填到圆角路径内（叠在底色之上、背景图之下）。
///
/// 本文件的绘图基元一律豁免 `too_many_arguments`：参数是「缓冲 + 缓冲尺寸 + 几何 + 颜色」
/// 这组固定形状，包成参数对象只是把同一批标量搬一遍。
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
    let (x, y, rw, rh) = snap_to_pixels(r.x, r.y, r.w, r.h);
    let (rw, rh) = (rw.max(1.0), rh.max(1.0));
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

/// 绘制背景填充图：从线程局部缓存取目标尺寸填充位图（BGRA 预乘），以 Pattern 填到圆角路径内。
fn paint_bg_image(buf: &mut [u8], buf_w: u32, buf_h: u32, r: Rect, radius: f32, img: &ViewImage) {
    if let Some(place) = &img.place {
        paint_placed_bg_image(buf, buf_w, buf_h, r, radius, img, place);
        return;
    }
    let (x, y, rw, rh) = snap_to_pixels(r.x, r.y, r.w, r.h);
    let (rw, rh) = (rw.max(1.0), rh.max(1.0));
    let spec = fill_spec_of(img);
    let tint = img.tint.unwrap_or([0, 0, 0, 0]);
    let Some(path) = round_rect_path(x, y, rw, rh, radius.round().max(0.0)) else {
        return;
    };
    IMAGE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let Some(fill) = cache.fill(&img.path, spec, rw as u32, rh as u32, tint) else {
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

/// 定位背景图：按 place 摆成一块、mode 作用于这块，超出节点圆角盒的部分裁掉——
/// 与主题编辑器预览「定位 + 裁到边框盒」同一画法。填充缓存按这块的尺寸取，
/// 与铺满同一张图的缓存各占各的键。
fn paint_placed_bg_image(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    r: Rect,
    radius: f32,
    img: &ViewImage,
    place: &ImagePlace,
) {
    let (x, y, rw, rh) = snap_to_pixels(r.x, r.y, r.w, r.h);
    let Some(node_path) = round_rect_path(x, y, rw.max(1.0), rh.max(1.0), radius.round().max(0.0))
    else {
        return;
    };
    let spec = fill_spec_of(img);
    let tint = img.tint.unwrap_or([0, 0, 0, 0]);
    IMAGE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let Some((lx, ly, lw, lh)) = place.rect(r, || cache.src_size(&img.path)) else {
            return;
        };
        let Some(fill) = cache.fill(&img.path, spec, lw as u32, lh as u32, tint) else {
            return;
        };
        draw_clipped(
            buf,
            buf_w,
            buf_h,
            fill,
            (lx, ly, lw, lh),
            img.opacity,
            Some(&node_path),
        );
    });
}

/// 把已按目标尺寸备好的填充位图贴到 `(x, y, w, h)`，只留 `clip` 路径内的部分（None=不裁）。
/// 定位背景图（裁到节点圆角盒）与覆盖图（裁到节点矩形）共用。
fn draw_clipped(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    fill: &tiny_skia::Pixmap,
    (x, y, w, h): (f32, f32, f32, f32),
    opacity: f32,
    clip: Option<&tiny_skia::Path>,
) {
    let Some(img_path) = round_rect_path(x, y, w, h, 0.0) else {
        return;
    };
    let mask = match clip {
        Some(clip) => {
            let Some(mut mask) = Mask::new(buf_w, buf_h) else {
                return;
            };
            mask.fill_path(clip, FillRule::Winding, true, Transform::identity());
            Some(mask)
        }
        None => None,
    };
    let Some(mut pm) = PixmapMut::from_bytes(buf, buf_w, buf_h) else {
        return;
    };
    let shader = Pattern::new(
        fill.as_ref(),
        SpreadMode::Pad,
        FilterQuality::Nearest,
        opacity.clamp(0.0, 1.0),
        Transform::from_translate(x, y),
    );
    let paint = Paint {
        shader,
        anti_alias: true,
        ..Default::default()
    };
    pm.fill_path(
        &img_path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        mask.as_ref(),
    );
}

/// `ViewImage` → 填充规格。
///
/// 单拎出来是为了能直接测：这是主题数据走到光栅化前的最后一环，而绘制本身在单测里
/// 验不了。漏传一个字段的表现是「主题里配了却没效果」，上游每一层的测试都照样绿。
fn fill_spec_of(img: &ViewImage) -> crate::image_cache::FillSpec {
    let px = |v: f32| v.round().max(0.0) as u32;
    crate::image_cache::FillSpec {
        mode: crate::image_cache::mode_code(&img.mode),
        slice: [
            px(img.slice[0]),
            px(img.slice[1]),
            px(img.slice[2]),
            px(img.slice[3]),
        ],
        repeat: img.slice_repeat,
        scale_milli: (img.scale * 1000.0).round().max(1.0) as u32,
        flip: img.flip,
    }
}

/// 绘制 z 层覆盖图：按 place 定位（见 [`ImagePlace::rect`]）置于 host 内，stretch 到目标尺寸 + tint + opacity；
/// 超出 host 矩形（不带圆角）的部分裁掉，内容上下两层同一口径——对齐编辑器预览 `paintLayers`。
fn paint_layer(buf: &mut [u8], buf_w: u32, buf_h: u32, host: Rect, layer: &ViewLayer) {
    let (x, y, rw, rh) = snap_to_pixels(host.x, host.y, host.w, host.h);
    let (rw, rh) = (rw.max(1.0), rh.max(1.0));
    IMAGE_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let Some((lx, ly, lw, lh)) = layer.place.rect(host, || cache.src_size(&layer.path)) else {
            return;
        };
        let Some(fill) = cache.fill(
            &layer.path,
            crate::image_cache::FillSpec {
                flip: layer.flip,
                ..crate::image_cache::FillSpec::stretch()
            },
            lw as u32,
            lh as u32,
            layer.tint,
        ) else {
            return;
        };
        // 整块落在 host 内就不必建蒙版（逐帧每层一张整缓冲大小的蒙版不便宜）。
        let inside = lx >= x && ly >= y && lx + lw <= x + rw && ly + lh <= y + rh;
        let host_path = if inside {
            None
        } else {
            round_rect_path(x, y, rw, rh, 0.0)
        };
        if !inside && host_path.is_none() {
            return;
        }
        draw_clipped(
            buf,
            buf_w,
            buf_h,
            fill,
            (lx, ly, lw, lh),
            layer.opacity,
            host_path.as_ref(),
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
// 唯一的映射约定写在这里，三处（矩形映射、两次像素搬运）共用，不得各写一遍：
//
//   局部缓冲宽 `cw`、高 `ch`；屏幕矩形宽 `rw = ch`、高 `rh = cw`（两向都交换宽高）。
//   屏幕内偏移 (sx, sy) ← 局部 (lx, ly)：
//     顺时针  `lx = sy`、`ly = ch - 1 - sx`
//     逆时针  `lx = cw - 1 - sy`、`ly = sx`
//
// 直觉校验：局部左上角 (0,0) 顺时针落到屏幕右上角、逆时针落到屏幕左下角。
// 于是一行从左到右的文字，顺时针转完是一列从上到下 —— 蒙古文要的就是这个。

/// 局部矩形 → 屏幕矩形。`host` 是旋转节点在屏幕上的矩形（宽高已交换）。
///
/// 与 [`for_each_mapped`] 必须同源：这里是连续坐标（矩形四边），那里是离散像素中心，故此处
/// 不出现 `-1`。各推一遍会让命中区整体偏一像素。
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

/// 把屏幕上 `(ox, oy)` 起、`ch × cw` 大小的那块逆向搬进局部缓冲（旋转的逆变换）。
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

/// 把局部缓冲正向旋转 90° 写回屏幕 `(ox, oy)` 处。
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
/// 两次搬运共用它：坐标映射必须逐像素一致，否则底子与结果错开一像素，文字边缘发虚。
///
/// `Rot::None` 不产出任何像素：走到这儿说明上游漏判，什么都不画比画错位置更容易发现。
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

/// 把浮点矩形对齐到像素网格：四条边界各自取整，而不是「位置 round、尺寸也 round」。
///
/// 相邻两个盒子的公共边界在两边取整到同一个整数，才既不留缝也不重叠；分别 round 在非整数
/// 倍 DPI 下会时而重叠、时而空出 1px。
///
/// 不在这里兜 `max(1.0)`：各调用点对「退化成 0 尺寸」的处理不同。
///
/// 只对整数尺寸保尺寸：非整数尺寸的结果随 `frac(x)` 在 `⌊n⌋`/`⌈n⌉` 间变。语义定宽的元素
/// （插入符、分隔线）必须在源头取整成整设备像素：插入符收口在 [`View::caret_at`]，编码栏
/// 分隔线收口在它的 `fixed_h` 调用处。
fn snap_to_pixels(x: f32, y: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
    let x0 = x.round();
    let y0 = y.round();
    (x0, y0, (x + w).round() - x0, (y + h).round() - y0)
}

/// 在缓冲区子区域填充圆角矩形：tiny-skia 抗锯齿填充 + 源覆盖混合。
/// `color` 约定为直通 [R,G,B,A]；缓冲区按预乘 BGRA 维护（供 UpdateLayeredWindow），
/// 传色时交换 R/B（见 [`aa_paint`]）。
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
    // 位置/尺寸对齐像素网格（按边界取整，见 snap_to_pixels），半径保留浮点供 AA。
    let (x, y, w, h) = snap_to_pixels(x, y, w, h);
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
    let (x, y, w, h) = snap_to_pixels(x, y, w, h);
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
// 盒模型布局与形状绘制（纯 Rust tiny-skia）跨平台一致；唯一差异是文本测量（Windows 用
// DirectWrite，其余用 mock）。故断言具体文本尺寸的用例 gate 到 `not(windows)`。

/// 几何与形状绘制测试（跨平台真实：tiny-skia 纯 Rust 光栅化，不依赖文本后端）。
#[cfg(test)]
mod fill_spec_tests {
    use super::*;

    /// 主题数据到光栅化的最后一环。这里漏一个字段，`wind-theme` 那侧测得再全也白搭——
    /// 画出来还是旧行为，而且一声不响。
    #[test]
    fn fill_spec_carries_every_field() {
        let img = ViewImage {
            path: "p.png".to_string(),
            mode: "nine_slice".to_string(),
            slice: [1.4, 2.5, 3.0, -1.0],
            slice_repeat: [true, false],
            opacity: 1.0,
            tint: None,
            place: None,
            scale: 1.5,
            flip: wind_theme::Flip { x: true, y: true },
        };

        let spec = fill_spec_of(&img);
        assert_eq!(
            spec.scale_milli, 1500,
            "DPI 没传到光栅化（SVG center/tile 单元）"
        );

        assert_eq!(spec.mode, crate::image_cache::mode_code("nine_slice"));
        // 切片取整到源图纹理像素；负值夹到 0（主题写反了不该变成天文数字）。
        assert_eq!(spec.slice, [1, 3, 3, 0]);
        assert_eq!(spec.repeat, [true, false], "中段重复没传到光栅化");
        assert_eq!(spec.flip, img.flip, "镜像没传到光栅化");
    }

    /// 不写 mode/repeat 的图 = 整体拉伸，与既有主题一致。
    #[test]
    fn fill_spec_defaults_to_stretch() {
        let img = ViewImage {
            path: "p.png".to_string(),
            mode: String::new(),
            slice: [0.0; 4],
            slice_repeat: [false; 2],
            opacity: 1.0,
            tint: None,
            place: None,
            scale: 1.0,
            flip: wind_theme::Flip::NONE,
        };
        assert_eq!(fill_spec_of(&img), crate::image_cache::FillSpec::stretch());
    }
}

#[cfg(test)]
#[path = "view_geom_tests.rs"]
mod geom_tests;

/// 阴影蒙版缓存：缓存不得改变渲染结果（跨平台真实——纯 tiny-skia 光栅化，不涉文本）。
///
/// 这些用例的重心是反向对照：只断言"同参数两次结果一致"是抓不到缓存 bug 的——
/// 键漏了某一项时，两次调用都会取到同一张错蒙版，结果照样一致、测试照样绿。真正能
/// 抓住漏键的是"只改一项，输出必须变"。
#[cfg(test)]
#[path = "view_shadow_cache_tests.rs"]
mod shadow_cache_tests;

/// 顺时针 90° 旋转：几何映射、像素搬运、测量与命中。
///
/// 不 gate 平台：全部断言只用 `fixed_w`/`fixed_h` 与裸缓冲，不碰文本测量，
/// 故三个平台答案相同（`layout_tests` 之所以 gate 是因为它断言文本尺寸）。
#[cfg(test)]
#[path = "view_rotate_tests.rs"]
mod rotate_tests;

/// 盒模型布局测试。断言含文本尺寸，依赖 mock 文本测量
/// （`measure_text_sized` = 字符数 × 字号 × 0.6，行高 = 字号 × 1.2）。
/// macOS 文本后端为真 CoreText（mock 失效），故同 Windows 一并 gate 出。
#[cfg(test)]
#[path = "view_layout_tests.rs"]
mod layout_tests;

/// 绘制层序：边框必须压在背景图之上。
///
/// 不 gate 平台：只用定宽定高与纯色位图，不碰文本测量。
#[cfg(test)]
mod paint_order_tests {
    use super::*;
    use crate::text::MockText as TextRenderer;
    use base64::Engine as _;

    fn solid_png_uri(rgba: [u8; 4]) -> String {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba(rgba));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("PNG 编码");
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.into_inner())
        )
    }

    /// 窗口整体铺一张不透明背景图、再配一圈边框：边框处必须是边框色。
    /// 背景图先于边框画的话这里是图的绿色——预览里有边框、实机被图盖掉。
    #[test]
    fn border_is_painted_over_bg_image() {
        let tr = TextRenderer::new("test", 20.0).unwrap();
        let mut v = View::container(Layout::Row)
            .fixed_w(20.0)
            .fixed_h(20.0)
            .border([255, 0, 0, 255], 3.0)
            .bg_image(ViewImage {
                path: solid_png_uri([0, 255, 0, 255]),
                mode: String::new(),
                slice: [0.0; 4],
                slice_repeat: [false; 2],
                opacity: 1.0,
                tint: None,
                place: None,
                scale: 1.0,
                flip: wind_theme::Flip::NONE,
            });
        v.layout(0.0, 0.0, &tr);
        let (w, h) = (20usize, 20usize);
        let mut buf = vec![0u8; w * h * 4];
        v.paint(&mut buf, w as u32, h as u32, &tr);

        // BGRA；取左缘中点（边框环内）与正中（只有图）。
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [buf[i + 2], buf[i + 1], buf[i]]
        };
        assert_eq!(at(1, 10), [255, 0, 0], "边框被背景图盖住了");
        assert_eq!(at(10, 10), [0, 255, 0], "背景图本身没画出来");
    }
}

/// 背景图定位：配了 anchor/offset/size 的背景图摆成一块、裁到节点圆角盒内；
/// 不配则铺满节点，与改前一致。覆盖图的尺寸口径（只给一维）一并钉在这里。
///
/// 不 gate 平台：只用定宽定高与纯色位图，不碰文本测量。
#[cfg(test)]
#[path = "view_bg_place_tests.rs"]
mod bg_place_tests;

// 分段颜色的接线：有区间走 `draw_runs`、没有走原来的 `draw`，且区间不影响布局。
// 只有 mock 后端记绘制调用。
#[cfg(test)]
mod color_runs_paint_tests {
    use super::*;
    use crate::text::MockText as TextRenderer;

    fn paint_log(v: &mut View) -> Vec<String> {
        let tr = TextRenderer::new("test", 20.0).unwrap();
        v.layout(0.0, 0.0, &tr);
        let mut buf = vec![0u8; 200 * 50 * 4];
        v.paint(&mut buf, 200, 50, &tr);
        tr.take_draw_log()
    }

    #[test]
    fn runs_route_to_draw_runs_and_plain_stays_on_draw() {
        let run = ColorRun {
            start: 0,
            end: 1,
            rgba: [255, 0, 0, 255],
        };
        let mut colored = View::leaf("ab", [0, 0, 0, 255]).color_runs(vec![run]);
        let mut plain = View::leaf("ab", [0, 0, 0, 255]);
        let c = paint_log(&mut colored);
        let p = paint_log(&mut plain);
        assert_eq!(c.len(), 1);
        assert!(c[0].starts_with("draw_runs ") && c[0].contains("runs=[ColorRun"));
        assert_eq!(p.len(), 1);
        assert!(p[0].starts_with("draw "), "无区间必须仍走 draw：{}", p[0]);
        assert_eq!(
            colored.measured_size(),
            plain.measured_size(),
            "颜色不参与布局"
        );
    }
}

#[cfg(test)]
mod isolated_cache_tests {
    use super::*;

    #[test]
    fn isolated_paint_caches_leave_live_caches_untouched() {
        let live = crate::image_cache::anim_fixtures::data_uri(
            "image/gif",
            &crate::image_cache::anim_fixtures::gif(2, 2, &[([1, 2, 3, 255], 50)]),
        );
        let _ = image_src_size(&live);
        let before = with_image_cache_for_test(|c| c.ids_len_for_test());
        assert!(before >= 1);
        let inner = with_isolated_paint_caches(|| {
            assert_eq!(
                with_image_cache_for_test(|c| c.ids_len_for_test()),
                0,
                "临时缓存是空的"
            );
            let _ = image_src_size("data:image/png;base64,AAAA");
            with_image_cache_for_test(|c| c.ids_len_for_test())
        });
        assert_eq!(inner, 1);
        assert_eq!(
            with_image_cache_for_test(|c| c.ids_len_for_test()),
            before,
            "换回原缓存"
        );
    }

    /// 调用方持有的缓存跨次保留自己的条目，且与线程当前那套互不串。
    #[test]
    fn owned_paint_caches_keep_their_entries() {
        let ids = || with_image_cache_for_test(|c| c.ids_len_for_test());
        let before = ids();
        let mut a = PaintCaches::default();
        with_paint_caches(&mut a, || {
            let _ = image_src_size("data:image/png;base64,AAAA");
        });
        assert_eq!(ids(), before, "线程当前那套不变");
        let n = with_paint_caches(&mut a, ids);
        assert_eq!(n, 1, "第二次换上时条目还在");
        let mut b = PaintCaches::default();
        assert_eq!(with_paint_caches(&mut b, ids), 0, "另一套是空的");
    }
}
