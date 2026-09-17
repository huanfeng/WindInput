//! 主题图片资产解析（各窗口共享）：把 RvImage 的 ref 解析为绝对路径，转成 View 渲染用的
//! ViewImage / ViewLayer。背景图/层的实际绘制由 view.rs 的 paint（线程局部 image_cache）完成。

use crate::view::{ViewImage, ViewLayer};
use wind_theme::{Resolved, RvImage};

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
pub fn rv_image(theme: &Resolved, im: Option<&RvImage>) -> Option<ViewImage> {
    let im = im?;
    let path = asset_path(theme, &im.reference)?;
    Some(ViewImage {
        path,
        mode: im.mode.clone(),
        slice: im.slice,
        slice_repeat: im.slice_repeat,
        opacity: im.opacity,
        tint: im.tint,
    })
}

/// RvImage[] → ViewLayer[]：解析路径 + 偏移分流（dp×scale / 百分比）+ 尺寸×scale。
pub fn rv_layers(theme: &Resolved, layers: &[RvImage], scale: f32) -> Vec<ViewLayer> {
    use wind_theme::schema::Dim;
    let split = |d: Option<Dim>| match d {
        Some(Dim::Dp(v)) => (v * scale, 0.0),
        Some(Dim::Px(v)) => (v, 0.0),
        Some(Dim::Pct(v)) => (0.0, v),
        None => (0.0, 0.0),
    };
    layers
        .iter()
        .filter_map(|im| {
            let path = asset_path(theme, &im.reference)?;
            let (off_x, off_x_pct) = split(im.offset_x);
            let (off_y, off_y_pct) = split(im.offset_y);
            Some(ViewLayer {
                path,
                z: im.z,
                anchor: im.anchor.clone(),
                off_x,
                off_y,
                off_x_pct,
                off_y_pct,
                w: if im.w > 0 { im.w as f32 * scale } else { 0.0 },
                h: if im.h > 0 { im.h as f32 * scale } else { 0.0 },
                opacity: im.opacity,
            })
        })
        .collect()
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

        let out = rv_image(&theme, Some(&im)).expect("有 ref 就该出图");

        assert_eq!(out.mode, "nine_slice");
        assert_eq!(out.slice, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(out.slice_repeat, [true, false], "中段重复没传到渲染层");
        assert_eq!(out.opacity, 0.5);
    }

    /// 空 ref = 没有图，不能凭空造一个（调用方据此跳过整层绘制）。
    #[test]
    fn rv_image_rejects_empty_reference() {
        assert!(rv_image(&Resolved::default(), Some(&RvImage::default())).is_none());
        assert!(rv_image(&Resolved::default(), None).is_none());
    }
}
