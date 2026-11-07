//! 主题图片资产解析（各窗口共享）：把 RvImage 的 ref 解析为绝对路径，转成 View 渲染用的
//! ViewImage / ViewLayer；RvGradient 同在此转成 ViewGradient。背景图/层的实际绘制由 view.rs 的 paint（线程局部 image_cache）完成。

use crate::view::{ImagePlace, ViewGradient, ViewImage, ViewLayer};
use wind_theme::{Resolved, RvGradient, RvImage};

/// 把 image ref 解析为可读绝对路径：resources 注册表 → data:/绝对 → 在 asset_dirs（base 链目录）
/// 搜字面文件（如 _base 的 chevron.svg）。
pub fn asset_path(theme: &Resolved, reference: &str) -> Option<String> {
    if reference.is_empty() {
        return None;
    }
    if let Some(p) = theme.resources.get(reference) {
        return Some(p.clone());
    }
    if reference.starts_with("data:") || std::path::Path::new(reference).is_absolute() {
        return Some(reference.to_string());
    }
    for d in &theme.asset_dirs {
        let p = d.join(reference);
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    Some(reference.to_string())
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
mod tests {
    use super::*;
    use wind_theme::Resolved;

    /// 本模块是主题求值形态到渲染形态的**最后一环**：这里漏传一个字段，上游求值
    /// 测得再全也没用——画出来仍是旧行为，而且一声不响。
    ///
    /// `slice_repeat` 是第一个用这条护栏钉住的字段：漏传它的后果是九宫中段永远拉伸，
    /// 也就是候选窗变宽时纹理跟着「呼吸」，正是它要修的那个毛病。
    #[test]
    fn rv_image_carries_fill_shape_through() {
        let theme = Resolved::default();
        let im = RvImage {
            reference: "panel.png".to_string(),
            mode: "nine_slice".to_string(),
            slice: [1.0, 2.0, 3.0, 4.0],
            slice_repeat: [true, false],
            opacity: 0.5,
            ..Default::default()
        };

        let out = rv_image(&theme, Some(&im), 1.0).expect("有 ref 就该出图");

        assert_eq!(out.mode, "nine_slice");
        assert_eq!(out.slice, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(out.slice_repeat, [true, false], "中段重复没传到渲染层");
        assert_eq!(out.opacity, 0.5);
    }

    /// 背景图何时走定位，逐条对齐编辑器 `bgPositioned()`：anchor / 任一非零偏移 / 非零尺寸。
    /// 什么都没配（或偏移写成 0）必须仍是铺满——既有主题一个像素都不能变。
    #[test]
    fn rv_image_places_only_when_positioning_is_configured() {
        use wind_theme::schema::Dim;
        let theme = Resolved::default();
        let base = RvImage {
            reference: "girl.png".to_string(),
            ..Default::default()
        };
        let placed = |im: RvImage| rv_image(&theme, Some(&im), 2.0).unwrap().place;

        assert_eq!(placed(base.clone()), None, "没配定位却不再铺满");
        let zero_off = RvImage {
            offset_x: Some(Dim::Dp(0.0)),
            offset_y: Some(Dim::Pct(0.0)),
            ..base.clone()
        };
        assert_eq!(placed(zero_off), None, "零偏移不算配了定位");
        for im in [
            RvImage {
                anchor: "right".to_string(),
                ..base.clone()
            },
            RvImage {
                offset_y: Some(Dim::Pct(-3.0)),
                ..base.clone()
            },
            RvImage {
                h: 5,
                ..base.clone()
            },
        ] {
            assert!(placed(im).is_some(), "配了定位字段却仍铺满");
        }

        // 换算与覆盖图同一套：dp×scale、百分比原样、尺寸×scale。
        let full = RvImage {
            anchor: "bottom-right".to_string(),
            offset_x: Some(Dim::Dp(-4.0)),
            offset_y: Some(Dim::Pct(10.0)),
            w: 24,
            ..base.clone()
        };
        let p = placed(full.clone()).unwrap();
        assert_eq!(
            p,
            ImagePlace {
                anchor: "bottom-right".to_string(),
                off_x: -8.0,
                off_y: 0.0,
                off_x_pct: 0.0,
                off_y_pct: 10.0,
                w: 48.0,
                h: 0.0,
                scale: 2.0,
            }
        );
        assert_eq!(rv_layers(&theme, &[full], 2.0)[0].place, p);
    }

    /// 图层 tint 透传：口径同背景图 `rv_image`（未配 = 不染色 `[0,0,0,0]`）。
    /// 曾经 schema 有、resolve 有、到这一环被丢——主题配了图层 tint 毫无变化。
    #[test]
    fn rv_layers_carries_tint() {
        let theme = Resolved::default();
        let tinted = RvImage {
            reference: "mark.svg".to_string(),
            tint: Some([10, 20, 30, 200]),
            ..Default::default()
        };
        let plain = RvImage {
            reference: "mark.svg".to_string(),
            ..Default::default()
        };
        let out = rv_layers(&theme, &[tinted.clone(), plain], 1.0);
        assert_eq!(out[0].tint, [10, 20, 30, 200], "图层 tint 没传到渲染层");
        assert_eq!(out[1].tint, [0, 0, 0, 0], "未配 tint 应原样出图");
        // 与背景图同口径。
        let bg = rv_image(&theme, Some(&tinted), 1.0).unwrap();
        assert_eq!(bg.tint, Some(out[0].tint));
    }

    /// 空 ref = 没有图，不能凭空造一个（调用方据此跳过整层绘制）。
    #[test]
    fn rv_image_rejects_empty_reference() {
        assert!(rv_image(&Resolved::default(), Some(&RvImage::default()), 1.0).is_none());
        assert!(rv_image(&Resolved::default(), None, 1.0).is_none());
    }
}
