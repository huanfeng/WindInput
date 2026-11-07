//! 候选窗的异形皮肤接入：几何求值、布局外扩、绘制、对齐点换算。
//! 设计见 `docs/design/theme-skin-animation.md` §3–§5。
//!
//! 接入方式：皮肤生效时根容器不画背景 / 边框 / 阴影，内边距换成皮肤内容框、最小尺寸
//! 取窗体固定段之和；根容器的盒就是窗体。窗体与装饰在根容器绘制前后各画一遍。

use super::CandidateWindow;
use crate::view::{Edges, Rect, View};
use wind_theme::skin::{
    Anchor, AnchorBox, BandFill, Clamp, DecorHit, Pos9, RvSkinVariant, RvSlot, RvSlots, Stretch,
};

/// 分槽位布局里的一块。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SlotKind {
    Preedit,
    Candidates,
    Footer,
}

/// 已排版的一块：窗体内位置（相对窗体左上，设备像素）与自然尺寸。
struct Part {
    kind: SlotKind,
    view: View,
    w: f32,
    h: f32,
}

/// 槽位某轴的位置 = `k + a·W`（W 为窗体该轴长度）。`lo`/`hi` 为内容框在该轴两端的内缩，
/// `on_content` 时锚点按内容框算。
fn slot_axis(
    a: f32,
    on_content: bool,
    lo: f32,
    hi: f32,
    off: f32,
    pivot: f32,
    size: f32,
) -> (f32, f32) {
    let base = if on_content { lo - a * (lo + hi) } else { 0.0 };
    (base + off - pivot * size, a)
}

/// 某块在某轴上的约束：位置 = `k + a·W`，自然长度 `size`，须落在 `[lo, W − hi]` 内——锚在内容框
/// 上的块不出内容框（那是文字的安全区），锚在窗体上的块不出窗体（`lo = hi = 0`）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct AxisFit {
    k: f32,
    a: f32,
    size: f32,
    lo: f32,
    hi: f32,
}

/// 让所有块都落在各自的盒内的最小 W（不小于 `min`）。锚在起始边却伸出起始边界（或锚在末端却
/// 伸出末端边界）时，窗体多大都放不进去：跳过该约束并告警，交给作者改偏移。
fn solve_axis(min: f32, parts: &[AxisFit]) -> f32 {
    let mut w = min;
    for p in parts {
        // 起始边：k + aW ≥ lo
        if p.a > 0.0 {
            w = w.max((p.lo - p.k) / p.a);
        } else if p.k < p.lo - 0.5 {
            tracing::warn!("skin: 槽位伸出起始边 {:.0}px，偏移需调整", p.lo - p.k);
        }
        // 末端：k + aW + size ≤ W − hi
        if p.a < 1.0 {
            w = w.max((p.k + p.size + p.hi) / (1.0 - p.a));
        } else if p.k + p.size + p.hi > 0.5 {
            tracing::warn!(
                "skin: 槽位伸出末端 {:.0}px，偏移需调整",
                p.k + p.size + p.hi
            );
        }
    }
    w.ceil()
}

/// 保护带内逐行是否有不透明像素（窗体 + 全部装饰的合成结果）。
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct AvoidMask {
    /// 第 0 行相对窗体顶边的位置（装饰向上外扩时为负）。
    pub(super) top: i32,
    /// `prefix[i]` = 前 i 行里不透明行的个数，区间查询 O(1)。
    prefix: Vec<u32>,
}

/// 低于它的 alpha 视为透明：阴影、柔边那一圈半透明不该把窗口推开。
const AVOID_ALPHA: u8 = 32;

impl AvoidMask {
    pub(super) fn from_rows(top: i32, opaque: &[bool]) -> Self {
        let mut prefix = Vec::with_capacity(opaque.len() + 1);
        prefix.push(0);
        for &o in opaque {
            prefix.push(prefix.last().copied().unwrap_or(0) + o as u32);
        }
        Self { top, prefix }
    }

    pub(super) fn rows(&self) -> i32 {
        self.prefix.len() as i32 - 1
    }

    /// 相对窗体顶边的 `[from, to)` 里有没有不透明行。
    pub(super) fn any(&self, from: i32, to: i32) -> bool {
        let a = (from - self.top).clamp(0, self.rows()) as usize;
        let b = (to - self.top).clamp(0, self.rows()) as usize;
        b > a && self.prefix[b] > self.prefix[a]
    }

    /// 换坐标系（ext_presenter 路径：设备像素 → 逻辑点，`f = 1 / scale`）：逻辑行覆盖的设备行里
    /// 有一行不透明就算不透明。
    fn scaled(&self, f: f32) -> Self {
        if (f - 1.0).abs() < 1e-6 {
            return self.clone();
        }
        let top = (self.top as f32 * f).floor() as i32;
        let end = ((self.top + self.rows()) as f32 * f).ceil() as i32;
        let rows: Vec<bool> = (top..end)
            .map(|r| {
                let from = (r as f32 / f).floor() as i32;
                let to = ((r + 1) as f32 / f).ceil() as i32;
                self.any(from, to)
            })
            .collect();
        Self::from_rows(top, &rows)
    }
}

/// `place_window` 的皮肤参数（调用方坐标系，相对窗体左上）。
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct PlaceAnchor {
    /// 下方时对齐光标的点（相对窗体左上）。
    pub(super) below: (i32, i32),
    /// 上方时对齐光标的点：x 相对窗体左边，y 是**距窗体底边向上**的距离（0 = 底边）。
    pub(super) above: (i32, i32),
    /// 光标行保护带上的不透明行（下方 / 上方各一份：两种方位对齐的横坐标可能不同）；
    /// `avoid_caret = false` 时为 None。
    pub(super) avoid_below: Option<AvoidMask>,
    pub(super) avoid_above: Option<AvoidMask>,
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
    /// 分槽位布局；None = stack。
    slots: Option<RvSlots>,
    /// 分槽位布局下本帧排好的各块。
    parts: Vec<Part>,
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
    pub(super) fn slots(&self) -> Option<RvSlots> {
        self.slots
    }

    fn slot_of(&self, kind: SlotKind) -> Option<RvSlot> {
        let s = self.slots?;
        match kind {
            SlotKind::Preedit => s.preedit,
            SlotKind::Candidates => Some(s.candidates),
            SlotKind::Footer => s.footer,
        }
    }

    /// 槽位两轴的 `(k, a)`：位置 = k + a·窗体长度。
    fn slot_axes(&self, slot: RvSlot, w: f32, h: f32) -> ((f32, f32), (f32, f32)) {
        let on_content = slot.anchor.on == AnchorBox::Content;
        let i = self.insets;
        let x = slot_axis(
            slot.anchor.pos.h as f32 / 2.0,
            on_content,
            i.l,
            i.r,
            slot.offset[0] * self.k,
            slot.pivot.h as f32 / 2.0,
            w,
        );
        let y = slot_axis(
            slot.anchor.pos.v as f32 / 2.0,
            on_content,
            i.t,
            i.b,
            slot.offset[1] * self.k,
            slot.pivot.v as f32 / 2.0,
            h,
        );
        (x, y)
    }

    /// 窗体为 `fw×fh` 时各块的左上（相对窗体左上）。
    fn part_positions(&self, fw: f32, fh: f32) -> Vec<(f32, f32)> {
        self.parts
            .iter()
            .map(|p| match self.slot_of(p.kind) {
                Some(slot) => {
                    let ((kx, ax), (ky, ay)) = self.slot_axes(slot, p.w, p.h);
                    ((kx + ax * fw).round(), (ky + ay * fh).round())
                }
                None => (self.insets.l, self.insets.t),
            })
            .collect()
    }

    /// 分槽位布局：画各块（根容器之后、盖在内容上的装饰之前）。
    pub(super) fn paint_parts(
        &self,
        buf: &mut [u8],
        bw: u32,
        bh: u32,
        tr: &crate::text::dwrite::TextRenderer,
    ) {
        for p in &self.parts {
            p.view.paint(buf, bw, bh, tr);
        }
    }

    pub(super) fn collect_part_hits(&self, out: &mut Vec<(i32, Rect)>) {
        for p in &self.parts {
            p.view.collect_hits(out);
        }
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
            slots: v.slots,
            parts: Vec::new(),
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
        let free = self.skin_is_free();
        if free {
            self.skin_measure_parts(root);
        }
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
        if free {
            self.skin_place_parts();
        }
        Some((ml as u32, mt as u32, mr as u32, mb as u32))
    }

    /// 本帧是否分槽位布局。
    pub(super) fn skin_is_free(&self) -> bool {
        self.skin_geom.as_ref().is_some_and(|g| g.slots.is_some())
    }

    /// 分槽位：取走 `build_tree` 交出的各块，按自然尺寸排一遍，解出能装下所有槽位的最小窗体，
    /// 写进根容器（空盒）的最小尺寸。
    fn skin_measure_parts(&mut self, root: &mut View) {
        let taken = std::mem::take(&mut *self.free_parts.borrow_mut());
        let mut parts: Vec<Part> = taken
            .into_iter()
            .map(|(kind, mut view)| {
                view.layout(0.0, 0.0, &self.text_renderer);
                let (w, h) = view.measured_size();
                Part {
                    kind,
                    view,
                    w: w.ceil(),
                    h: h.ceil(),
                }
            })
            .collect();
        let Some(g) = self.skin_geom.as_mut() else {
            return;
        };
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for p in &parts {
            if let Some(slot) = g.slot_of(p.kind) {
                let ((kx, ax), (ky, ay)) = g.slot_axes(slot, p.w, p.h);
                let i = g.insets;
                let (bx, by) = if slot.anchor.on == AnchorBox::Content {
                    ((i.l, i.r), (i.t, i.b))
                } else {
                    ((0.0, 0.0), (0.0, 0.0))
                };
                xs.push(AxisFit {
                    k: kx,
                    a: ax,
                    size: p.w,
                    lo: bx.0,
                    hi: bx.1,
                });
                ys.push(AxisFit {
                    k: ky,
                    a: ay,
                    size: p.h,
                    lo: by.0,
                    hi: by.1,
                });
            }
        }
        let fw = solve_axis(root.min_w.unwrap_or(0.0), &xs);
        let fh = solve_axis(root.min_h.unwrap_or(0.0), &ys);
        root.min_w = Some(fw);
        root.min_h = Some(fh);
        g.parts = std::mem::take(&mut parts);
    }

    /// 分槽位：窗体尺寸与外扩已定，把各块排到最终位置（窗口缓冲坐标）。
    fn skin_place_parts(&mut self) {
        let Some(g) = self.skin_geom.as_mut() else {
            return;
        };
        let (ml, mt, fw, fh) = g.frame;
        let pos = g.part_positions(fw as f32, fh as f32);
        for (p, (x, y)) in g.parts.iter_mut().zip(pos) {
            p.view
                .layout(ml as f32 + x, mt as f32 + y, &self.text_renderer);
        }
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
        if self.skin_is_free() {
            self.skin_place_parts();
        }
    }

    /// 合成窗体 + 全部装饰，取光标保护带（对齐点横坐标 `caret_x` 向左 `before`、向右 `after`，dp）
    /// 内逐行的不透明情况。行坐标相对窗体顶边（设备像素）。
    fn skin_avoid_mask(
        &self,
        g: &SkinGeom,
        caret_x: f32,
        strip: wind_theme::skin::AvoidStrip,
    ) -> AvoidMask {
        let (ml, mt, mr, mb) = g.margins;
        let (_, _, w, h) = g.frame;
        let (bw, bh) = (w + ml + mr, h + mt + mb);
        let mut buf = vec![0u8; (bw as usize) * (bh as usize) * 4];
        g.paint(&mut buf, bw, bh, false, false);
        g.paint(&mut buf, bw, bh, true, false);
        let cx = ml as f32 + caret_x;
        let x0 = (cx - strip.before * self.scale).floor().max(0.0) as usize;
        let x1 = ((cx + strip.after * self.scale).ceil().max(0.0) as usize).min(bw as usize);
        let rows: Vec<bool> = (0..bh as usize)
            .map(|y| {
                let row = &buf[y * bw as usize * 4..(y + 1) * bw as usize * 4];
                (x0..x1).any(|x| row[x * 4 + 3] >= AVOID_ALPHA)
            })
            .collect();
        AvoidMask::from_rows(-(mt as i32), &rows)
    }

    /// `place_window` 的对齐与避让参数。`f` 把设备像素换到调用方坐标系（ext_presenter 路径是逻辑点，
    /// 传 `1 / scale`；其余传 1）。无皮肤时为默认值（窗体左上 / 左下对齐、无避让、无外扩），与改前一致。
    pub(super) fn skin_place_anchor(&self, f: f32) -> PlaceAnchor {
        let Some(g) = &self.skin_geom else {
            return PlaceAnchor::default();
        };
        let r = |v: f32| (v * f).round() as i32;
        let (_, _, w, h) = g.frame;
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
        let (avoid_below, avoid_above) = match g.placement.avoid_caret {
            Some(strip) => {
                let mask = |caret_x: f32| self.skin_avoid_mask(g, caret_x, strip).scaled(f);
                (Some(mask(bx)), Some(mask(ax)))
            }
            None => (None, None),
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
            avoid_below,
            avoid_above,
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
    fn avoid_pushes_opaque_rows_off_the_caret_line() {
        // 光标行 180..200（底端 200，行高 20），gap 2 → 保护带 178..202。
        // 窗体顶上方 30 行：上 20 行透明、下 10 行（-10..0）不透明。
        let mut rows = vec![false; 20];
        rows.extend([true; 10]);
        rows.extend([true; 20]); // 窗体本身 0..20
        let m = AvoidMask::from_rows(-30, &rows);
        // 下方 y=202：不透明的 -10..0 落在 192..202，压线 → 推到 212。透明的那 20 行越过光标行不算。
        assert_eq!(
            CandidateWindow::avoid_caret_line(202, false, 200, 20, Some(&m)),
            212
        );
        // 不透明行本来就在保护带以下：不动。
        let low = AvoidMask::from_rows(0, &[false, false, true]);
        assert_eq!(
            CandidateWindow::avoid_caret_line(200, false, 200, 20, Some(&low)),
            200
        );
        // 上方：窗体 0..20 在 y=160 → 160..180，压到 178 → 上推到底边 ≤ 178。
        let frame = AvoidMask::from_rows(0, &[true; 20]);
        assert_eq!(
            CandidateWindow::avoid_caret_line(160, true, 200, 20, Some(&frame)),
            158
        );
        assert_eq!(
            CandidateWindow::avoid_caret_line(5, false, 200, 20, None),
            5
        );
    }

    #[test]
    fn mask_scales_to_logical_points() {
        // 设备行 -4..4，只有 2..4 不透明；缩到 1/2 → 逻辑行 -2..2，只有第 1 行不透明。
        let mut rows = vec![false; 6];
        rows.extend([true; 2]);
        let m = AvoidMask::from_rows(-4, &rows).scaled(0.5);
        assert_eq!((m.top, m.rows()), (-2, 4));
        assert!(m.any(1, 2));
        assert!(!m.any(-2, 1));
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
            v.placement.avoid_caret = None;
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
        assert!(
            a.avoid_below.is_none() && a.avoid_above.is_none(),
            "关掉 avoid_caret 不该带保护带"
        );
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
    fn default_avoid_sees_decor_pixels_in_strip() {
        let mut w = fixture_window("avoid");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.build_tree(false);
        w.skin_layout(&mut root).unwrap();
        let a = w.skin_place_anchor(1.0);
        assert_eq!(a.clamp, (0, 0, 0, 0), "缺省只钳窗体");
        let m = a.avoid_below.as_ref().expect("缺省开避让");
        assert_eq!(m.top, -6, "装饰向上外扩 6px，保护带从那里算起");
        assert!(m.any(-6, 0), "缺省保护带够宽，右上角装饰落在带内");
        // 装饰伸到窗体上方 6px，下方放时压住光标行 → 整窗下推 6。
        let (_, y, _) = CandidateWindow::place_window(100, 200, 20, 30, 20, false, (0, 0), &a);
        assert_eq!(y, 208);
    }

    #[test]
    fn narrow_strip_ignores_decor_far_from_caret() {
        use wind_theme::skin::AvoidStrip;
        let mut w = fixture_window("strip");
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.placement.avoid_caret = Some(AvoidStrip {
                before: 0.0,
                after: 10.0,
            });
            w.set_theme(theme);
        }
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.build_tree(false);
        w.skin_layout(&mut root).unwrap();
        let a = w.skin_place_anchor(1.0);
        assert!(
            !a.avoid_below.as_ref().unwrap().any(-6, 0),
            "装饰在保护带右边"
        );
        // 光标左侧对齐窗体左缘、带宽 10px：右上角的装饰压不到正在输入的字，窗口不动。
        let (_, y, _) = CandidateWindow::place_window(100, 200, 20, 30, 20, false, (0, 0), &a);
        assert_eq!(y, 202);
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

    fn fit(k: f32, a: f32, size: f32) -> AxisFit {
        AxisFit {
            k,
            a,
            size,
            lo: 0.0,
            hi: 0.0,
        }
    }

    #[test]
    fn solve_axis_fits_slots_anchored_on_both_ends() {
        // 左锚：从 10 起、宽 50 → W ≥ 60。右锚（a=1）：k=-50，宽 20 → W ≥ 50。
        assert_eq!(
            solve_axis(0.0, &[fit(10.0, 0.0, 50.0), fit(-50.0, 1.0, 20.0)]),
            60.0
        );
        assert_eq!(
            solve_axis(80.0, &[fit(10.0, 0.0, 50.0)]),
            80.0,
            "不小于最小尺寸"
        );
        // 居中（a=0.5，pivot 0.5）：k = -w/2 → W ≥ w。
        let (k, a) = slot_axis(0.5, false, 0.0, 0.0, 0.0, 0.5, 40.0);
        assert_eq!(solve_axis(0.0, &[fit(k, a, 40.0)]), 40.0);
        // 内容框锚点：左内缩 4、右内缩 5，锚在内容框右端 → k = 4 - (4+5) - 10 = -15。
        assert_eq!(slot_axis(1.0, true, 4.0, 5.0, 0.0, 1.0, 10.0), (-15.0, 1.0));
        // 锚在内容框左上的块不出内容框：W ≥ 4 + 50 + 5。
        let (k, a) = slot_axis(0.0, true, 4.0, 5.0, 0.0, 0.0, 50.0);
        let content = AxisFit {
            k,
            a,
            size: 50.0,
            lo: 4.0,
            hi: 5.0,
        };
        assert_eq!(solve_axis(0.0, &[content]), 59.0);
    }

    fn cand(text: &str) -> wind_ui_types::CandidateItem {
        wind_ui_types::CandidateItem {
            text: text.to_string(),
            text_styled: Default::default(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: Default::default(),
            comment_above: Default::default(),
            no_index: false,
        }
    }

    #[test]
    fn free_slots_place_candidates_and_footer_independently() {
        use wind_theme::skin::{RvSlot, RvSlots};
        let mut w = fixture_window("free");
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.slots = Some(RvSlots {
                preedit: None,
                candidates: RvSlot {
                    anchor: Anchor::parse("content.top_left").unwrap(),
                    pivot: Pos9::TOP_LEFT,
                    offset: [0.0, 0.0],
                },
                footer: Some(RvSlot {
                    anchor: Anchor::parse("frame.bottom_right").unwrap(),
                    pivot: Pos9 { h: 2, v: 2 },
                    offset: [-1.0, -1.0],
                }),
            });
            w.set_theme(theme);
        }
        w.update("", 0, "", vec![cand("甲"), cand("乙")], 0, -1, 1, 3);
        w.refresh_skin_geom();
        assert!(w.skin_is_free());
        let mut root = w.build_tree(false);
        let (ml, mt, _, _) = w.skin_layout(&mut root).unwrap();
        let g = w.skin_geom.as_ref().unwrap();
        let kinds: Vec<SlotKind> = g.parts.iter().map(|p| p.kind).collect();
        assert_eq!(kinds, vec![SlotKind::Candidates, SlotKind::Footer]);
        let (fx, fy, fw, fh) = g.frame;
        assert_eq!((fx, fy), (ml as i32, mt as i32));
        let pos = g.part_positions(fw as f32, fh as f32);
        // 候选槽贴内容框左上（内缩 4, 3）。
        assert_eq!(pos[0], (4.0, 3.0));
        // 翻页槽右下角贴窗体右下、内缩 1。
        let footer = &g.parts[1];
        assert_eq!(
            (pos[1].0 + footer.w, pos[1].1 + footer.h),
            (fw as f32 - 1.0, fh as f32 - 1.0)
        );
        // 窗体装得下两块：候选在内容框里，翻页不压出窗体。
        assert!(fw as f32 >= 4.0 + g.parts[0].w + 5.0);
        assert!(pos[1].0 >= 0.0 && pos[1].1 >= 0.0);
        // 命中区来自各块，候选可点。
        let mut hits = Vec::new();
        g.collect_part_hits(&mut hits);
        assert!(hits.iter().any(|(tag, _)| *tag == 0), "候选 0 的命中区");
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
