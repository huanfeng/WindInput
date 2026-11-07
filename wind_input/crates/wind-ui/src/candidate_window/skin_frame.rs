//! 候选窗的异形皮肤接入：几何求值、布局外扩、绘制、对齐点换算。
//! 设计见 `docs/design/theme-skin-animation.md` §3–§5。
//!
//! 接入方式：皮肤生效时根容器不画背景 / 边框 / 阴影，内边距换成皮肤内容框、最小尺寸
//! 取窗体固定段之和；根容器的盒就是窗体。窗体与装饰在根容器绘制前后各画一遍。

use super::CandidateWindow;
use crate::view::{Edges, Rect, View};
use wind_theme::skin::{
    Anchor, AnchorBox, BandFill, Clamp, DecorHit, Pos9, RvSkinVariant, Stretch,
};

/// `place_window` 的皮肤参数（调用方坐标系，相对窗体左上）。
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct PlaceAnchor {
    /// 下方时对齐光标的点（相对窗体左上）。
    pub(super) below: (i32, i32),
    /// 上方时对齐光标的点：x 相对窗体左边，y 是**距窗体底边向上**的距离（0 = 底边）。
    pub(super) above: (i32, i32),
    /// 需避让光标行的装饰矩形（相对窗体左上）；`avoid_caret = false` 时为空。
    pub(super) avoid: Vec<(i32, i32, i32, i32)>,
    /// 屏幕钳制额外留出的外扩（左, 上, 右, 下）；`clamp = "frame"` 时全 0。
    pub(super) clamp: (i32, i32, i32, i32),
}

/// 一块装饰的求值结果（设备像素）。
#[derive(Clone, Debug)]
pub(super) struct DecorGeom {
    path: String,
    /// 图集倍率：设备像素 / 该图源像素。
    k: f32,
    w: u32,
    h: u32,
    anchor: Anchor,
    pivot: Pos9,
    off: (f32, f32),
    z: i32,
    hit: DecorHit,
    opacity: f32,
}

/// 皮肤在当前 DPI / 字号下的几何。每帧渲染前由 [`CandidateWindow::refresh_skin_geom`] 重算。
#[derive(Clone, Debug)]
pub(super) struct SkinGeom {
    frame_path: String,
    /// 窗体图集倍率：设备像素 / 所选窗体图源像素。
    frame_k: f32,
    /// 拉伸声明（已换算到所选窗体图的源像素）。
    sx: Stretch,
    sy: Stretch,
    pub(super) min_w: f32,
    pub(super) min_h: f32,
    /// 内容框内缩（设备像素）。
    pub(super) insets: Edges,
    placement: wind_theme::skin::RvPlacement,
    decor: Vec<DecorGeom>,
    /// 本帧布局结果：窗体与各装饰在窗口缓冲内的矩形。
    frame: (i32, i32, u32, u32),
    decor_rects: Vec<(i32, i32, u32, u32)>,
    /// 本帧外扩（左, 上, 右, 下）。
    margins: (u32, u32, u32, u32),
    /// 主坐标源像素 → 设备像素。
    k: f32,
}

/// z 分档：<0 窗体下；0..100 窗体上、内容下；>=100 内容上。
const Z_OVER_CONTENT: i32 = 100;

fn scale_stretch(s: &Stretch, f: f32) -> Stretch {
    match s {
        Stretch::Bands(b) => Stretch::Bands(
            b.iter()
                .map(|band| wind_theme::skin::Band {
                    from: band.from * f,
                    to: band.to * f,
                    fill: band.fill,
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// 选窗体源图：取 density ≥ 所需中最小的一份，都不够取最大的一份。
fn pick_source(v: &RvSkinVariant, need: f32) -> (&str, f32) {
    let mut all: Vec<(&str, f32)> = std::iter::once((v.frame.image.as_str(), v.frame.density))
        .chain(v.frame.srcset.iter().map(|(p, d)| (p.as_str(), *d)))
        .collect();
    all.sort_by(|a, b| a.1.total_cmp(&b.1));
    all.iter()
        .copied()
        .find(|(_, d)| *d >= need - 1e-3)
        .or_else(|| all.last().copied())
        .unwrap_or((v.frame.image.as_str(), v.frame.density))
}

/// 盒上锚点的坐标。
fn anchor_point(a: Anchor, frame: Rect, content: Rect) -> (f32, f32) {
    let b = match a.on {
        AnchorBox::Frame => frame,
        AnchorBox::Content => content,
    };
    a.pos.point(b.x, b.y, b.w, b.h)
}

/// 装饰矩形（相对窗体左上为原点）。
fn decor_rect(d: &DecorGeom, frame: Rect, content: Rect) -> (i32, i32, u32, u32) {
    let (ax, ay) = anchor_point(d.anchor, frame, content);
    let (px, py) = d.pivot.point(0.0, 0.0, d.w as f32, d.h as f32);
    (
        (ax - px + d.off.0).round() as i32,
        (ay - py + d.off.1).round() as i32,
        d.w,
        d.h,
    )
}

impl SkinGeom {
    fn content_rect(&self, frame: Rect) -> Rect {
        Rect {
            x: frame.x + self.insets.l,
            y: frame.y + self.insets.t,
            w: (frame.w - self.insets.l - self.insets.r).max(0.0),
            h: (frame.h - self.insets.t - self.insets.b).max(0.0),
        }
    }

    /// 窗体矩形（窗口缓冲坐标）。
    pub(super) fn frame_rect(&self) -> Rect {
        let (x, y, w, h) = self.frame;
        Rect {
            x: x as f32,
            y: y as f32,
            w: w as f32,
            h: h as f32,
        }
    }

    /// 装饰矩形（窗口缓冲坐标）：鼠标层据此判「装饰区不拖动」。
    pub(super) fn decor_rects(&self) -> Vec<Rect> {
        self.decor_rects
            .iter()
            .map(|&(x, y, w, h)| Rect {
                x: x as f32,
                y: y as f32,
                w: w as f32,
                h: h as f32,
            })
            .collect()
    }
    /// 有没有 `hit = "pass"` 的装饰。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn has_pass(&self) -> bool {
        self.decor.iter().any(|d| d.hit == DecorHit::Pass)
    }

    /// 穿透装饰全在窗体下（z < 0）：穿透窗口放到候选窗下面，否则放上面。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn pass_below(&self) -> bool {
        self.decor
            .iter()
            .filter(|d| d.hit == DecorHit::Pass)
            .all(|d| d.z < 0)
    }

    /// 只画穿透装饰（按 z 序），供独立的穿透窗口用。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn paint_pass(&self, buf: &mut [u8], bw: u32, bh: u32) {
        let whole = Stretch::Whole(BandFill::Stretch);
        for (d, &rect) in self.decor.iter().zip(&self.decor_rects) {
            if d.hit == DecorHit::Pass {
                crate::view::paint_skin_image(
                    buf, bw, bh, rect, &d.path, d.k, &whole, &whole, d.opacity,
                );
            }
        }
    }

    /// 画皮肤层：`over = false` 画窗体及其上下的装饰（根容器之前调用），`true` 画盖在内容上的装饰。
    /// `skip_pass`：穿透装饰另由穿透窗口画，这里跳过。
    pub(super) fn paint(&self, buf: &mut [u8], bw: u32, bh: u32, over: bool, skip_pass: bool) {
        let whole = Stretch::Whole(BandFill::Stretch);
        let paint_decor = |buf: &mut [u8], keep: &dyn Fn(i32) -> bool| {
            for (d, &rect) in self.decor.iter().zip(&self.decor_rects) {
                if skip_pass && d.hit == DecorHit::Pass {
                    continue;
                }
                if keep(d.z) {
                    crate::view::paint_skin_image(
                        buf, bw, bh, rect, &d.path, d.k, &whole, &whole, d.opacity,
                    );
                }
            }
        };
        if over {
            paint_decor(buf, &|z| z >= Z_OVER_CONTENT);
            return;
        }
        paint_decor(buf, &|z| z < 0);
        crate::view::paint_skin_image(
            buf,
            bw,
            bh,
            self.frame,
            &self.frame_path,
            self.frame_k,
            &self.sx,
            &self.sy,
            1.0,
        );
        paint_decor(buf, &|z| (0..Z_OVER_CONTENT).contains(&z));
    }
}

impl CandidateWindow {
    /// 按当前主题、横竖排、嵌入形态、DPI、字号求皮肤几何；不启用皮肤时置 None。
    pub(super) fn refresh_skin_geom(&mut self) {
        self.skin_geom = self.compute_skin_geom();
    }

    fn compute_skin_geom(&self) -> Option<SkinGeom> {
        let skin = self.theme.skin.as_ref()?;
        let v = skin.variant(self.vertical, self.preedit_embedded)?;
        // 主坐标（skin.density 源像素）→ 设备像素。
        let k = skin.factor(self.scale, self.base_font_logical);
        // 渲染缩放：设备像素 / dp。
        let render_scale = k * skin.density;
        let (src_ref, src_density) = pick_source(v, render_scale);
        let frame_path = crate::theme_assets::asset_path(&self.theme, src_ref)?;
        if crate::image_cache::is_svg_source(&frame_path) {
            tracing::warn!("skin: 窗体图须为位图（SVG 不支持多段切片），回退普通窗口");
            return None;
        }
        // 拉伸带按 frame.image 的像素写，换算到所选源图。
        let band_f = src_density / v.frame.density;
        let sx = scale_stretch(&v.frame.stretch_x, band_f);
        let sy = scale_stretch(&v.frame.stretch_y, band_f);
        let frame_k = render_scale / src_density;
        let Some((min_w, min_h, _, _)) = crate::view::skin_metrics(&frame_path, frame_k, &sx, &sy)
        else {
            tracing::warn!("skin: 窗体图读不出，回退普通窗口");
            return None;
        };
        let c = v.content;
        let insets = Edges {
            t: c.top * k,
            r: c.right * k,
            b: c.bottom * k,
            l: c.left * k,
        };
        let decor = v
            .decor
            .iter()
            .filter_map(|d| {
                let path = crate::theme_assets::asset_path(&self.theme, &d.image)?;
                let (sw, sh) = crate::view::image_src_size(&path)?;
                let (w, h) = match d.size {
                    Some([w, h]) => (w * k, h * k),
                    None => {
                        let f = render_scale / d.density;
                        (sw as f32 * f, sh as f32 * f)
                    }
                };
                let (w, h) = (w.round().max(1.0) as u32, h.round().max(1.0) as u32);
                Some(DecorGeom {
                    path,
                    k: w as f32 / sw.max(1) as f32,
                    w,
                    h,
                    anchor: d.anchor,
                    pivot: d.pivot,
                    off: (d.offset[0] * k, d.offset[1] * k),
                    z: d.z,
                    hit: d.hit,
                    opacity: d.opacity,
                })
            })
            .collect();
        Some(SkinGeom {
            frame_path,
            frame_k,
            sx,
            sy,
            min_w: min_w as f32,
            min_h: min_h as f32,
            insets,
            placement: v.placement,
            decor,
            frame: (0, 0, 0, 0),
            decor_rects: Vec::new(),
            margins: (0, 0, 0, 0),
            k,
        })
    }

    /// 皮肤布局：先在原点量出窗体（根容器盒）尺寸，按装饰伸出窗体的部分求外扩量，再把
    /// 根容器挪到外扩后的位置。返回外扩 `(左, 上, 右, 下)`；不启用皮肤返回 None（调用方走阴影那条）。
    pub(super) fn skin_layout(&mut self, root: &mut View) -> Option<(u32, u32, u32, u32)> {
        self.skin_geom.as_ref()?;
        root.layout(0.0, 0.0, &self.text_renderer);
        let (w, h) = root.measured_size();
        let g = self.skin_geom.as_mut()?;
        let frame = Rect {
            x: 0.0,
            y: 0.0,
            w: w.ceil(),
            h: h.ceil(),
        };
        let content = g.content_rect(frame);
        let rects: Vec<_> = g
            .decor
            .iter()
            .map(|d| decor_rect(d, frame, content))
            .collect();
        let (mut ml, mut mt, mut mr, mut mb) = (0i32, 0i32, 0i32, 0i32);
        for &(x, y, dw, dh) in &rects {
            ml = ml.max(-x);
            mt = mt.max(-y);
            mr = mr.max(x + dw as i32 - frame.w as i32);
            mb = mb.max(y + dh as i32 - frame.h as i32);
        }
        g.frame = (ml, mt, frame.w as u32, frame.h as u32);
        g.margins = (ml as u32, mt as u32, mr as u32, mb as u32);
        g.decor_rects = rects
            .into_iter()
            .map(|(x, y, dw, dh)| (x + ml, y + mt, dw, dh))
            .collect();
        root.layout(ml as f32, mt as f32, &self.text_renderer);
        Some((ml as u32, mt as u32, mr as u32, mb as u32))
    }

    /// 渲染入口对内容尺寸做完钳制（竖排宽度上限、屏幕安全上限、最小 40×24）后调用：窗体按
    /// 钳后的尺寸画，装饰随之重新定位。外扩量沿用布局时的值——它已经按窗体自然尺寸量过，
    /// 窗体变窄只会让右侧多出一点透明余量，不会让装饰出缓冲。
    pub(super) fn skin_fit_frame(&mut self, content_w: u32, content_h: u32) {
        let Some(g) = self.skin_geom.as_mut() else {
            return;
        };
        let (ml, mt, w, h) = g.frame;
        if (w, h) == (content_w, content_h) {
            return;
        }
        g.frame = (ml, mt, content_w, content_h);
        let frame = Rect {
            x: 0.0,
            y: 0.0,
            w: content_w as f32,
            h: content_h as f32,
        };
        let content = g.content_rect(frame);
        g.decor_rects = g
            .decor
            .iter()
            .map(|d| {
                let (x, y, dw, dh) = decor_rect(d, frame, content);
                (x + ml, y + mt, dw, dh)
            })
            .collect();
    }

    /// `place_window` 的对齐与避让参数。`f` 把设备像素换到调用方坐标系（ext_presenter 路径是逻辑点，
    /// 传 `1 / scale`；其余传 1）。无皮肤时为默认值（窗体左上 / 左下对齐、无避让、无外扩），与改前一致。
    pub(super) fn skin_place_anchor(&self, f: f32) -> PlaceAnchor {
        let Some(g) = &self.skin_geom else {
            return PlaceAnchor::default();
        };
        let r = |v: f32| (v * f).round() as i32;
        let (ml, mt, w, h) = g.frame;
        let frame = Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        };
        let content = g.content_rect(frame);
        let point = |c: wind_theme::skin::CaretPoint| {
            let (x, y) = anchor_point(c.anchor, frame, content);
            (x + c.offset[0] * g.k, y + c.offset[1] * g.k)
        };
        let (bx, by) = point(g.placement.caret);
        let (ax, ay) = point(g.placement.caret_above);
        let avoid = if g.placement.avoid_caret {
            g.decor_rects
                .iter()
                .map(|&(x, y, dw, dh)| {
                    (
                        r((x - ml) as f32),
                        r((y - mt) as f32),
                        r(dw as f32),
                        r(dh as f32),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let clamp = match g.placement.clamp {
            Clamp::Frame => (0, 0, 0, 0),
            Clamp::All => {
                let (l, t, rr, b) = g.margins;
                (r(l as f32), r(t as f32), r(rr as f32), r(b as f32))
            }
        };
        PlaceAnchor {
            below: (r(bx), r(by)),
            above: (r(ax), r(frame.h - ay)),
            avoid,
            clamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decor(anchor: &str, pivot: Pos9, off: (f32, f32), w: u32, h: u32) -> DecorGeom {
        DecorGeom {
            path: String::new(),
            k: 1.0,
            w,
            h,
            anchor: Anchor::parse(anchor).unwrap(),
            pivot,
            off,
            z: 1,
            hit: DecorHit::Block,
            opacity: 1.0,
        }
    }

    #[test]
    fn decor_rect_uses_anchor_pivot_and_offset() {
        let frame = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 80.0,
        };
        let content = Rect {
            x: 10.0,
            y: 20.0,
            w: 150.0,
            h: 40.0,
        };
        // 右上角，装饰底边右端贴锚点，再左移 10 → 伸出窗体上方 30。
        let d = decor("frame.top_right", Pos9 { h: 2, v: 2 }, (-10.0, 0.0), 40, 30);
        assert_eq!(decor_rect(&d, frame, content), (150, -30, 40, 30));
        // 内容框左下角。
        let d = decor("content.bottom_left", Pos9::TOP_LEFT, (0.0, 0.0), 5, 5);
        assert_eq!(decor_rect(&d, frame, content), (10, 60, 5, 5));
    }

    /// 夹具：30×20 窗体图（左上像素红、其余白；x 10..20 / y 8..12 可拉伸）+ 6×6 蓝色装饰，
    /// 装饰挂在窗体右上角外侧（底边右端贴锚点）。
    fn fixture_window(tag: &str) -> CandidateWindow {
        use wind_theme::skin::{BandSpec, Skin, SkinDecor, SkinFrame, SkinInsets, SkinVariant};
        let dir = std::env::temp_dir().join(format!("wind-skin-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let frame = dir.join("frame.png");
        let mut img = image::RgbaImage::from_pixel(30, 20, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(&frame).unwrap();
        let decor = dir.join("decor.png");
        image::RgbaImage::from_pixel(6, 6, image::Rgba([0, 0, 255, 255]))
            .save(&decor)
            .unwrap();
        let skin = Skin {
            horizontal: Some(SkinVariant {
                frame: Some(SkinFrame {
                    image: frame.to_string_lossy().into_owned(),
                    stretch_x: Some(vec![BandSpec::Pair([10.0, 20.0])]),
                    stretch_y: Some(vec![BandSpec::Pair([8.0, 12.0])]),
                    ..Default::default()
                }),
                content: SkinInsets {
                    left: 4.0,
                    top: 3.0,
                    right: 5.0,
                    bottom: 2.0,
                },
                decor: vec![SkinDecor {
                    image: decor.to_string_lossy().into_owned(),
                    anchor: "frame.top_right".into(),
                    pivot: Some("bottom_right".into()),
                    z: 1,
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let theme = wind_theme::Resolved {
            skin: wind_theme::skin::resolve_skin(&skin),
            ..Default::default()
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w =
            CandidateWindow::new(super::super::CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_theme(theme);
        w.set_orientation(false, false, false);
        w
    }

    #[test]
    fn skin_sets_min_size_insets_and_decor_margin() {
        let mut w = fixture_window("layout");
        // 只有模式标记：内容远小于窗体最小尺寸。
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.build_tree(false);
        let m = w.skin_layout(&mut root).expect("皮肤生效");
        assert_eq!(m, (0, 6, 0, 0), "装饰伸出窗体上方 6px，只在上边外扩");
        let g = w.skin_geom.as_ref().unwrap();
        // 最小尺寸 = 固定段之和：30-10、20-4。
        let (fx, fy, fw, fh) = g.frame;
        assert_eq!((fx, fy), (0, 6));
        assert!(fw >= 20 && fh >= 16, "窗体 {fw}x{fh} 不应小于固定段之和");
        assert_eq!(g.decor_rects, vec![(fw as i32 - 6, 0, 6, 6)]);
        // 根容器的盒就是窗体：内边距 = 内容框内缩。
        assert_eq!(
            (g.insets.l, g.insets.t, g.insets.r, g.insets.b),
            (4.0, 3.0, 5.0, 2.0)
        );
        assert!(w.shadow_params().is_none(), "皮肤不叠软件阴影");
    }

    #[test]
    fn skin_paints_frame_corner_and_decor() {
        let mut w = fixture_window("paint");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.build_tree(false);
        let (ml, mt, mr, mb) = w.skin_layout(&mut root).unwrap();
        let g = w.skin_geom.as_ref().unwrap();
        let (_, _, fw, fh) = g.frame;
        let (bw, bh) = (fw + ml + mr, fh + mt + mb);
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        g.paint(&mut buf, bw, bh, false, false);
        let px = |x: u32, y: u32| {
            let o = ((y * bw + x) * 4) as usize;
            [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
        };
        // BGRA：窗体左上（固定段，逐像素拷贝）是红；装饰是蓝；装饰左侧的外扩区透明。
        assert_eq!(px(ml, mt), [0, 0, 255, 255]);
        assert_eq!(px(bw - 1, 0), [255, 0, 0, 255]);
        assert_eq!(px(0, 0)[3], 0);
    }

    #[test]
    fn decor_outside_frame_does_not_start_drag() {
        let mut w = fixture_window("mouse");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.build_tree(false);
        w.skin_layout(&mut root).unwrap();
        let g = w.skin_geom.as_ref().unwrap();
        let mut m = w.mouse.borrow_mut();
        m.skin_frame = Some(g.frame_rect());
        m.skin_decor = g.decor_rects();
        let (_, _, fw, _) = g.frame;
        let x = fw as f32 - 3.0;
        assert!(m.on_decor_only(x, 2.0), "窗体外的装饰");
        assert!(!m.on_decor_only(x, 10.0), "窗体内");
        assert!(!m.on_decor_only(1.0, 2.0), "外扩区里装饰以外的透明处");
    }

    #[test]
    fn clamped_content_size_moves_frame_and_decor() {
        let mut w = fixture_window("fit");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.build_tree(false);
        w.skin_layout(&mut root).unwrap();
        let (_, _, fw, fh) = w.skin_geom.as_ref().unwrap().frame;
        // 渲染入口把宽度钳窄 4px：窗体按钳后宽度画，右上角装饰跟着左移。
        w.skin_fit_frame(fw - 4, fh);
        let g = w.skin_geom.as_ref().unwrap();
        assert_eq!(g.frame, (0, 6, fw - 4, fh));
        assert_eq!(g.decor_rects, vec![(fw as i32 - 10, 0, 6, 6)]);
    }

    #[test]
    fn svg_frame_falls_back_to_plain_window() {
        let mut w = fixture_window("svg");
        let mut theme = w.theme_source.clone();
        let skin = theme.skin.as_mut().unwrap();
        skin.horizontal.as_mut().unwrap().frame.image = "data:image/svg+xml;base64,PHN2Zy8+".into();
        w.set_theme(theme);
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        assert!(
            w.skin_geom.is_none(),
            "SVG 窗体画不出来，不能当皮肤生效（否则窗口全透明）"
        );
    }

    #[test]
    fn avoid_pushes_decor_off_the_caret_line() {
        // 光标行 180..200（底端 200，行高 20），gap 2 → 需避开 178..202。
        // 下方：窗口 y=202，装饰在窗体上方 -30..0 → 盖住光标行，推到装饰顶 = 202。
        let y = CandidateWindow::avoid_caret_line(202, false, 200, 20, &[(0, -30, 10, 30)]);
        assert_eq!(y, 232);
        // 装饰本来就不碰光标行：不动。
        let y = CandidateWindow::avoid_caret_line(202, false, 200, 20, &[(0, 5, 10, 5)]);
        assert_eq!(y, 202);
        // 上方：窗口底边在 178，装饰向下伸出 10 → 上推 10。
        let y = CandidateWindow::avoid_caret_line(100, true, 200, 20, &[(0, 70, 10, 18)]);
        assert_eq!(y, 90);
    }

    #[test]
    fn caret_point_with_offset_moves_window() {
        use wind_theme::skin::{Anchor, CaretPoint};
        let mut w = fixture_window("caret");
        {
            let theme = w.theme_source.clone();
            let mut theme = theme;
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.placement.caret = CaretPoint {
                anchor: Anchor::parse("content.top_left").unwrap(),
                offset: [0.0, -1.0],
            };
            v.placement.avoid_caret = false;
            w.set_theme(theme);
        }
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.build_tree(false);
        w.skin_layout(&mut root).unwrap();
        let a = w.skin_place_anchor(1.0);
        // 内容框左上 (4, 3) 再上移 1 → (4, 2)；上方缺省窗体左下 → 距底 0。
        assert_eq!(a.below, (4, 2));
        assert_eq!(a.above, (0, 0));
        assert!(a.avoid.is_empty(), "关掉 avoid_caret 不该带矩形");
        // 非 Windows 下 place_window 恒在下方：对齐点落在光标底端 + 2。
        let (x, y, above) = CandidateWindow::place_window(100, 200, 20, 30, 20, false, (0, 0), &a);
        assert_eq!((x, y, above), (96, 200, false));
        // 无皮肤的默认参数：与改前一致（窗体左上贴光标底端 + 2）。
        let d = PlaceAnchor::default();
        assert_eq!(
            CandidateWindow::place_window(100, 200, 20, 30, 20, false, (0, 0), &d),
            (100, 202, false)
        );
    }

    #[test]
    fn default_avoid_collects_decor_relative_to_frame() {
        let mut w = fixture_window("avoid");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.build_tree(false);
        w.skin_layout(&mut root).unwrap();
        let (_, _, fw, _) = w.skin_geom.as_ref().unwrap().frame;
        let a = w.skin_place_anchor(1.0);
        assert_eq!(
            a.avoid,
            vec![(fw as i32 - 6, -6, 6, 6)],
            "窗体坐标：装饰在窗体上方 6px"
        );
        assert_eq!(a.clamp, (0, 0, 0, 0), "缺省只钳窗体");
        // 装饰伸到窗体上方 6px，下方放时会压住光标行 → 整窗下推 6。
        let (_, y, _) = CandidateWindow::place_window(100, 200, 20, 30, 20, false, (0, 0), &a);
        assert_eq!(y, 208);
    }

    #[test]
    fn pass_decor_is_split_out_of_main_buffer() {
        let mut w = fixture_window("pass");
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].hit = DecorHit::Pass;
            w.set_theme(theme);
        }
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.build_tree(false);
        let (ml, mt, mr, mb) = w.skin_layout(&mut root).unwrap();
        let g = w.skin_geom.as_ref().unwrap();
        assert!(g.has_pass());
        assert!(!g.pass_below(), "z = 1 的穿透装饰要放在主窗口上面");
        let (_, _, fw, fh) = g.frame;
        let (bw, bh) = (fw + ml + mr, fh + mt + mb);
        let blank = vec![0u8; (bw * bh * 4) as usize];
        let (mut main, mut pass) = (blank.clone(), blank);
        g.paint(&mut main, bw, bh, false, true);
        g.paint_pass(&mut pass, bw, bh);
        let at = |b: &[u8], x: u32, y: u32| b[((y * bw + x) * 4 + 3) as usize];
        // 装饰在右上角外扩区：主缓冲里是透明（点击会穿过主窗口），穿透层里有像素。
        assert_eq!(at(&main, bw - 1, 0), 0);
        assert_eq!(at(&pass, bw - 1, 0), 255);
        // 窗体只在主缓冲。
        assert_eq!(at(&main, ml, mt), 255);
        assert_eq!(at(&pass, ml, mt), 0);
    }

    #[test]
    fn vertical_without_skin_variant_keeps_plain_window() {
        let mut w = fixture_window("vertical");
        w.set_orientation(true, false, false);
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        assert!(w.skin_geom.is_none(), "竖排没配皮肤：不硬套横排图");
    }

    #[test]
    fn source_pick_prefers_smallest_sufficient_density() {
        use wind_theme::skin::{Skin, SkinFrame, SkinVariant, SrcSetEntry};
        let entry = |image: &str, density| SrcSetEntry {
            image: image.into(),
            density,
        };
        let v = Skin {
            horizontal: Some(SkinVariant {
                frame: Some(SkinFrame {
                    image: "a3".into(),
                    density: Some(3.0),
                    srcset: vec![entry("a1", 1.0), entry("a2", 2.0)],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let s = wind_theme::skin::resolve_skin(&v).unwrap();
        let v = s.horizontal.as_ref().unwrap();
        assert_eq!(pick_source(v, 1.0).0, "a1");
        assert_eq!(pick_source(v, 1.5).0, "a2");
        assert_eq!(pick_source(v, 2.0).0, "a2");
        assert_eq!(pick_source(v, 4.0).0, "a3", "都不够取最大");
    }
}
