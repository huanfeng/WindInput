//! 主题图片资产解析（各窗口共享）：把 RvImage 的 ref 解析为绝对路径，转成 View 渲染用的
//! ViewImage / ViewLayer；RvGradient 同在此转成 ViewGradient。背景图/层的实际绘制由 view.rs 的 paint（线程局部 image_cache）完成。

use crate::view::{ImagePlace, ViewGradient, ViewImage, ViewLayer};
use wind_theme::{Resolved, RvGradient, RvImage};

/// 把 image ref 解析为可读绝对路径：resources 注册表 → data:/绝对 → 内存资源表（[`crate::assets`]）
/// → 在 asset_dirs（base 链目录）搜字面文件（如 _base 的 chevron.svg）。resources 解析出的相对
/// 路径同样先查内存资源表。
pub fn asset_path(theme: &Resolved, reference: &str) -> Option<String> {
    if reference.is_empty() {
        return None;
    }
    if let Some(p) = theme.resources.get(reference) {
        return Some(crate::assets::source_key(p).unwrap_or_else(|| p.clone()));
    }
    if reference.starts_with("data:") || std::path::Path::new(reference).is_absolute() {
        return Some(reference.to_string());
    }
    if let Some(k) = crate::assets::source_key(reference) {
        return Some(k);
    }
    for d in &theme.asset_dirs {
        let p = d.join(reference);
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    Some(reference.to_string())
}

/// 暗色求值时，资源键亮暗两侧不是同一张图：返回亮色那张的源串（同 [`asset_path`] 的形态）；
/// 亮色求值、不是资源键、两侧同图时 None。
pub fn light_asset_path(theme: &Resolved, reference: &str) -> Option<String> {
    let p = theme.light_resources.get(reference)?;
    Some(crate::assets::source_key(p).unwrap_or_else(|| p.clone()))
}

/// RvImage → 渲染用 ViewImage（reference 解析为绝对路径）。
///
/// 配了定位字段（见 [`positioned`]）才带 `place`，否则铺满节点——与改前逐像素一致。
pub fn rv_image(theme: &Resolved, im: Option<&RvImage>, scale: f32) -> Option<ViewImage> {
    let im = im?;
    let path = asset_path(theme, &im.reference)?;
    Some(ViewImage {
        path,
        mode: im.mode.clone(),
        slice: im.slice,
        slice_repeat: im.slice_repeat,
        opacity: im.opacity,
        tint: im.tint,
        place: positioned(im).then(|| image_place(im, scale)),
        scale,
        flip: im.flip,
    })
}

/// RvImage[] → ViewLayer[]：解析路径 + 定位（见 [`image_place`]）。
pub fn rv_layers(theme: &Resolved, layers: &[RvImage], scale: f32) -> Vec<ViewLayer> {
    layers
        .iter()
        .filter_map(|im| {
            let path = asset_path(theme, &im.reference)?;
            Some(ViewLayer {
                path,
                z: im.z,
                place: image_place(im, scale),
                opacity: im.opacity,
                // 口径同 rv_image：未配 / transparent 都落成 alpha=0 = 不染色。
                tint: im.tint.unwrap_or([0, 0, 0, 0]),
                flip: im.flip,
            })
        })
        .collect()
}

/// RvGradient → 渲染用 ViewGradient（stop 颜色直通 [R,G,B,A]）；无 stop 返回 None。
/// 候选窗各节点与弹窗类（status / tooltip / toast / menu.root）共用，绘制统一走
/// `View::bg_gradient`（叠在底色之上、背景图之下、裁到圆角）。
pub fn rv_gradient(g: Option<&RvGradient>) -> Option<ViewGradient> {
    let g = g?;
    if g.stops.is_empty() {
        return None;
    }
    Some(ViewGradient {
        radial: g.kind == "radial",
        angle: g.angle,
        stops: g.stops.clone(),
    })
}

/// 定位字段 → 渲染形态：偏移分流（dp×scale / 百分比）+ 尺寸×scale。背景图与覆盖图共用。
fn image_place(im: &RvImage, scale: f32) -> ImagePlace {
    use wind_theme::schema::Dim;
    let split = |d: Option<Dim>| match d {
        Some(Dim::Dp(v)) => (v * scale, 0.0),
        Some(Dim::Px(v)) => (v, 0.0),
        Some(Dim::Pct(v)) => (0.0, v),
        None => (0.0, 0.0),
    };
    let (off_x, off_x_pct) = split(im.offset_x);
    let (off_y, off_y_pct) = split(im.offset_y);
    ImagePlace {
        anchor: im.anchor.clone(),
        off_x,
        off_y,
        off_x_pct,
        off_y_pct,
        w: if im.w > 0 { im.w as f32 * scale } else { 0.0 },
        h: if im.h > 0 { im.h as f32 * scale } else { 0.0 },
        scale,
    }
}

/// 背景图是否按定位绘制：配了 anchor、任一非零偏移或非零尺寸。
///
/// 逐条对齐主题编辑器预览的 `bgPositioned()`（candidateBox.ts）——两边判据一旦分叉，
/// 就是「预览摆在角上、实机铺满整窗」（t238 / A2-52 正是 Rust 端缺了这一判）。
fn positioned(im: &RvImage) -> bool {
    use wind_theme::schema::Dim;
    let nonzero = |d: Option<Dim>| match d {
        Some(Dim::Dp(v) | Dim::Px(v) | Dim::Pct(v)) => v != 0.0,
        None => false,
    };
    !im.anchor.is_empty() || nonzero(im.offset_x) || nonzero(im.offset_y) || im.w != 0 || im.h != 0
}

/// 弹窗类（status / tooltip / toast / menu.root）渐变接线测试的共用夹具：蓝底 + 纯红线性渐变。
/// 渐变叠在底色之上，接上了取样点就是红，没接上就是蓝。
#[cfg(any(test, feature = "test-util"))]
pub mod gradient_fixture {
    pub const BG: [u8; 4] = [0, 0, 255, 255];
    pub const RED: [u8; 3] = [255, 0, 0];
    pub const BLUE: [u8; 3] = [0, 0, 255];

    pub fn node() -> wind_theme::RvNode {
        wind_theme::RvNode {
            bg_color: Some(BG),
            bg_gradient: Some(wind_theme::RvGradient {
                kind: "linear".into(),
                angle: 0.0,
                stops: vec![([255, 0, 0, 255], 0.0), ([255, 0, 0, 255], 1.0)],
            }),
            ..Default::default()
        }
    }

    /// BGRA 缓冲 → 某点 RGB。
    pub fn rgb(buf: &[u8], buf_w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * buf_w + x) * 4) as usize;
        [buf[i + 2], buf[i + 1], buf[i]]
    }
}

#[cfg(test)]
#[path = "theme_assets_tests.rs"]
mod tests;
