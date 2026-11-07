//! 候选窗的异形皮肤接入：几何求值、布局外扩、绘制、对齐点换算。
//! 设计见 `docs/design/theme-skin-animation.md` §3–§5。
//!
//! 接入方式：皮肤生效时根容器不画背景 / 边框 / 阴影，内边距换成皮肤内容框、最小尺寸
//! 取窗体固定段之和；根容器的盒就是窗体。窗体与装饰在根容器绘制前后各画一遍。

use super::{CandidateScene, CandidateSceneInput};
use crate::text::TextBackend;
use crate::view::{Edges, Rect, View};
use wind_theme::skin::{
    Anchor, AnchorBox, BandFill, Clamp, DecorAction, DecorClip, DecorEvent, DecorHit, Pos9,
    RvSkinVariant, RvSlot, RvSlots, Stretch,
};

/// 分槽位布局里的一块。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
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
pub struct AvoidMask {
    /// 第 0 行相对窗体顶边的位置（装饰向上外扩时为负）。
    pub top: i32,
    /// `prefix[i]` = 前 i 行里不透明行的个数，区间查询 O(1)。
    prefix: Vec<u32>,
}

/// 低于它的 alpha 视为透明：阴影、柔边那一圈半透明不该把窗口推开。
const AVOID_ALPHA: u8 = 32;

impl AvoidMask {
    pub fn from_rows(top: i32, opaque: &[bool]) -> Self {
        let mut prefix = Vec::with_capacity(opaque.len() + 1);
        prefix.push(0);
        for &o in opaque {
            prefix.push(prefix.last().copied().unwrap_or(0) + o as u32);
        }
        Self { top, prefix }
    }

    pub fn rows(&self) -> i32 {
        self.prefix.len() as i32 - 1
    }

    /// 相对窗体顶边的 `[from, to)` 里有没有不透明行。
    pub fn any(&self, from: i32, to: i32) -> bool {
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
pub struct PlaceAnchor {
    /// 下方时对齐光标的点（相对窗体左上）。
    pub below: (i32, i32),
    /// 上方时对齐光标的点：x 相对窗体左边，y 是**距窗体底边向上**的距离（0 = 底边）。
    pub above: (i32, i32),
    /// 光标行保护带上的不透明行（下方 / 上方各一份：两种方位对齐的横坐标可能不同）；
    /// `avoid_caret = false` 时为 None。
    pub avoid_below: Option<AvoidMask>,
    pub avoid_above: Option<AvoidMask>,
    /// 屏幕钳制额外留出的外扩（左, 上, 右, 下）；`clamp = "frame"` 时全 0。
    pub clamp: (i32, i32, i32, i32),
}

/// 一块装饰的求值结果（设备像素）。
#[derive(Clone, Debug)]
pub struct DecorGeom {
    /// 在变体 `decor` 里的下标：状态机按它对号（求值失败的装饰会被滤掉，下标不连续）。
    index: usize,
    w: u32,
    h: u32,
    anchor: Anchor,
    pivot: Pos9,
    off: (f32, f32),
    z: i32,
    hit: DecorHit,
    opacity: f32,
    /// 动画：fps 覆盖与帧预算份额（各源同一份额）。
    fps: Option<f32>,
    budget: usize,
    /// `[0]` = 装饰文件本身，其后是片段 `{ image = .. }` 引用的文件。
    sources: Vec<DecorSource>,
    /// 各片段的时间轴，见 [`crate::skin_anim::Timelines`]。
    base: ClipTl,
    states: Vec<Option<ClipTl>>,
    on: Vec<Option<ClipTl>>,
    /// 此刻画哪个源的第几帧（保留帧下标）；`still` 是初始状态首帧（关动画、避让遮罩用）。
    cur: (usize, usize),
    still: (usize, usize),
}

/// 装饰用到的一个文件。
#[derive(Clone, Debug)]
struct DecorSource {
    path: String,
    /// 图集倍率：设备像素 / 该图源像素。
    k: f32,
    /// 已按动画解出帧；否则按静态图画（首帧）。
    animated: bool,
}

/// 一段片段：哪个源、从第几个保留帧起、各帧时长。
#[derive(Clone, Debug)]
struct ClipTl {
    source: usize,
    first: usize,
    durations: Vec<u32>,
}

impl ClipTl {
    /// 源的整段；没解出动画（静态图）为一帧。
    fn whole(source: usize, layout: Option<&crate::image_cache::AnimLayout>) -> Self {
        Self {
            source,
            first: 0,
            durations: layout.map_or_else(
                || vec![crate::skin_anim::STILL_CLIP_MS],
                |l| l.durations.clone(),
            ),
        }
    }
}

impl crate::skin_anim::Timelines for DecorGeom {
    fn clip(&self, id: crate::skin_anim::ClipId) -> Option<&[u32]> {
        self.clip_tl(id).map(|c| c.durations.as_slice())
    }
}

impl DecorGeom {
    fn clip_tl(&self, id: crate::skin_anim::ClipId) -> Option<&ClipTl> {
        use crate::skin_anim::ClipId;
        match id {
            ClipId::Base => Some(&self.base),
            ClipId::State(i) => self.states.get(i)?.as_ref(),
            ClipId::On(i) => self.on.get(i)?.as_ref(),
        }
    }

    /// 状态机给的帧 → (源, 保留帧下标)。
    fn locate(&self, pose: crate::skin_anim::Pose) -> (usize, usize) {
        let c = self.clip_tl(pose.clip).unwrap_or(&self.base);
        (c.source, c.first + pose.frame)
    }

    /// 画到 `rect`：动画源画第 `frame` 帧，静态图走窗体同一条整轴拉伸。
    fn paint(
        &self,
        buf: &mut [u8],
        bw: u32,
        bh: u32,
        rect: (i32, i32, u32, u32),
        at: (usize, usize),
    ) {
        let (si, frame) = at;
        let Some(src) = self.sources.get(si) else {
            return;
        };
        if src.animated {
            crate::view::paint_anim_frame(
                buf,
                bw,
                bh,
                rect,
                &src.path,
                src.k,
                self.fps,
                self.budget,
                frame,
                self.opacity,
            );
        } else {
            let whole = Stretch::Whole(BandFill::Stretch);
            crate::view::paint_skin_image(
                buf,
                bw,
                bh,
                rect,
                &src.path,
                src.k,
                &whole,
                &whole,
                self.opacity,
            );
        }
    }
}

/// 皮肤在当前 DPI / 字号下的几何。每帧渲染前由 [`CandidateScene::refresh_skin`] 重算。
pub struct SkinGeom {
    /// 求它时的变体（竖排, 嵌入）。
    variant_key: (bool, bool),
    frame_path: String,
    /// 窗体图集倍率：设备像素 / 所选窗体图源像素。
    frame_k: f32,
    /// 拉伸声明（已换算到所选窗体图的源像素）。
    sx: Stretch,
    sy: Stretch,
    pub min_w: f32,
    pub min_h: f32,
    /// 内容框内缩（设备像素）。
    pub insets: Edges,
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
    pub fn frame_rect(&self) -> Rect {
        let (x, y, w, h) = self.frame;
        Rect {
            x: x as f32,
            y: y as f32,
            w: w as f32,
            h: h as f32,
        }
    }

    /// 不起拖的装饰矩形（窗口缓冲坐标）：`block`，以及穿透窗口建不出来、回落画进主窗口的 `pass`。
    /// `drag` 装饰不在其中——按在它上面与按在窗体空白处一样拖动。
    pub fn no_drag_rects(&self) -> Vec<Rect> {
        self.decor
            .iter()
            .zip(&self.decor_rects)
            .filter(|(d, _)| d.hit != DecorHit::Drag)
            .map(|(_, &(x, y, w, h))| Rect {
                x: x as f32,
                y: y as f32,
                w: w as f32,
                h: h as f32,
            })
            .collect()
    }
    /// 能点的装饰（`drag` / `block`）：(变体下标, 矩形, 在窗体下面)，按 z 升序（后者在上）。
    pub fn click_rects(&self) -> Vec<(usize, Rect, bool)> {
        self.decor
            .iter()
            .zip(&self.decor_rects)
            .filter(|(d, _)| d.hit != DecorHit::Pass)
            .map(|(d, &(x, y, w, h))| {
                let r = Rect {
                    x: x as f32,
                    y: y as f32,
                    w: w as f32,
                    h: h as f32,
                };
                (d.index, r, d.z < 0)
            })
            .collect()
    }

    pub fn slots(&self) -> Option<RvSlots> {
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
    pub fn paint_parts(&self, buf: &mut [u8], bw: u32, bh: u32, tr: &dyn TextBackend) {
        for p in &self.parts {
            p.view.paint(buf, bw, bh, tr);
        }
    }

    pub fn collect_part_hits(&self, out: &mut Vec<(i32, Rect)>) {
        for p in &self.parts {
            p.view.collect_hits(out);
        }
    }

    /// 有没有 `hit = "pass"` 的装饰。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn has_pass(&self) -> bool {
        self.decor.iter().any(|d| d.hit == DecorHit::Pass)
    }

    /// 穿透装饰全在窗体下（z < 0）：穿透窗口放到候选窗下面，否则放上面。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn pass_below(&self) -> bool {
        self.decor
            .iter()
            .filter(|d| d.hit == DecorHit::Pass)
            .all(|d| d.z < 0)
    }

    /// 只画穿透装饰（按 z 序），供独立的穿透窗口用。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn paint_pass(&self, buf: &mut [u8], bw: u32, bh: u32) {
        for (d, &rect) in self.decor.iter().zip(&self.decor_rects) {
            if d.hit == DecorHit::Pass {
                d.paint(buf, bw, bh, rect, d.cur);
            }
        }
    }

    /// 画皮肤层：`over = false` 画窗体及其上下的装饰（根容器之前调用），`true` 画盖在内容上的装饰。
    /// `skip_pass`：穿透装饰另由穿透窗口画，这里跳过。
    pub fn paint(&self, buf: &mut [u8], bw: u32, bh: u32, over: bool, skip_pass: bool) {
        self.paint_at(buf, bw, bh, over, skip_pass, false);
    }

    /// 同 [`Self::paint`]；`first_frame` 时装饰一律画初始状态首帧（避让遮罩用，不随帧变）。
    fn paint_at(
        &self,
        buf: &mut [u8],
        bw: u32,
        bh: u32,
        over: bool,
        skip_pass: bool,
        first_frame: bool,
    ) {
        let paint_decor = |buf: &mut [u8], keep: &dyn Fn(i32) -> bool| {
            for (d, &rect) in self.decor.iter().zip(&self.decor_rects) {
                if skip_pass && d.hit == DecorHit::Pass {
                    continue;
                }
                if keep(d.z) {
                    d.paint(buf, bw, bh, rect, if first_frame { d.still } else { d.cur });
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

#[cfg(any(test, feature = "test-util"))]
impl SkinGeom {
    /// 本帧窗体在缓冲里的矩形 `(x, y, w, h)`。
    pub fn frame_for_test(&self) -> (i32, i32, u32, u32) {
        self.frame
    }

    /// 本帧各装饰在缓冲里的矩形。
    pub fn decor_rects_for_test(&self) -> &[(i32, i32, u32, u32)] {
        &self.decor_rects
    }

    pub fn decor_for_test(&self) -> &[DecorGeom] {
        &self.decor
    }

    /// 分槽位各块：`(槽位, 自然宽, 自然高)`。
    pub fn parts_for_test(&self) -> Vec<(SlotKind, f32, f32)> {
        self.parts.iter().map(|p| (p.kind, p.w, p.h)).collect()
    }

    pub fn part_positions_for_test(&self, fw: f32, fh: f32) -> Vec<(f32, f32)> {
        self.part_positions(fw, fh)
    }
    /// 渲染金样用：几何逐字段转储，每块装饰附「此刻的帧 + 单独画出的像素摘要」。
    pub fn golden_dump(&self, digest: fn(&[u8]) -> u64) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "skin variant={:?} frame_path={} frame_k={:?} k={:?}\n  sx={:?}\n  sy={:?}\n  min=({:?},{:?}) insets={:?}\n  frame={:?} margins={:?}\n  placement={:?}\n  slots={:?}",
            self.variant_key,
            self.frame_path,
            self.frame_k,
            self.k,
            self.sx,
            self.sy,
            self.min_w,
            self.min_h,
            self.insets,
            self.frame,
            self.margins,
            self.placement,
            self.slots,
        );
        for (d, &rect) in self.decor.iter().zip(&self.decor_rects) {
            let mut buf = vec![0u8; (d.w * d.h * 4) as usize];
            d.paint(&mut buf, d.w, d.h, (0, 0, d.w, d.h), d.cur);
            let srcs: Vec<_> = d
                .sources
                .iter()
                .map(|s| format!("({} k={:?} animated={})", s.path, s.k, s.animated))
                .collect();
            let _ = writeln!(
                out,
                "  decor[{}] rect={rect:?} z={} hit={:?} opacity={:?} fps={:?} cur={:?} still={:?} 摘要 {:016x}\n    sources={}",
                d.index,
                d.z,
                d.hit,
                d.opacity,
                d.fps,
                d.cur,
                d.still,
                digest(&buf),
                srcs.join(" "),
            );
        }
        for p in &self.parts {
            let _ = writeln!(out, "  part {:?} {:?}x{:?}", p.kind, p.w, p.h);
            out.push_str(&p.view.debug_dump());
        }
        out
    }
}

#[cfg(any(test, feature = "test-util"))]
impl DecorGeom {
    /// 此刻画的 `(源, 保留帧下标)`。
    pub fn cur_for_test(&self) -> (usize, usize) {
        self.cur
    }

    /// 各源是否按动画解出了帧（`[0]` 是装饰文件本身）。
    pub fn sources_animated_for_test(&self) -> Vec<bool> {
        self.sources.iter().map(|s| s.animated).collect()
    }

    pub fn budget_for_test(&self) -> usize {
        self.budget
    }

    /// 各 `on` 事件片段可不可用。
    pub fn on_for_test(&self) -> Vec<bool> {
        self.on.iter().map(Option::is_some).collect()
    }
}

/// 装饰文件的帧区间 `[first, last]`（原始帧号）→ 片段。`raw` = 原始帧数，None 为不知道
/// （关着动画没解帧：只用得到首帧）；越界为 None。降帧后原始第 `i` 帧落在保留的第 `i / stride` 帧，
/// 区间取落在其中的保留帧（起点向上取整），一个都没有时取终点所在那帧。
fn frames_clip(
    first: u32,
    last: u32,
    layout: Option<&crate::image_cache::AnimLayout>,
    raw: Option<usize>,
) -> Option<ClipTl> {
    let still = || ClipTl::whole(0, None);
    match (raw, layout) {
        (None, _) => Some(still()),
        (Some(n), _) if last as usize >= n => None,
        (Some(_), None) => Some(still()),
        // 有帧生成失败、保留帧前移：映射对不上，按整段播（调用方 warn）。
        (Some(_), Some(l)) if !l.aligned => Some(ClipTl::whole(0, Some(l))),
        (Some(_), Some(l)) => {
            let stride = l.stride.max(1);
            let len = l.durations.len();
            // 首帧向上取整：区间起点被丢掉时取它之后的第一个保留帧，别画到区间外的帧。
            let b = (last as usize / stride).min(len - 1);
            let a = (first as usize).div_ceil(stride).min(b);
            Some(ClipTl {
                source: 0,
                first: a,
                durations: l.durations[a..=b].to_vec(),
            })
        }
    }
}

/// 装饰动画的时刻，由窗口注入（场景不取时间）。
#[derive(Clone, Copy, Debug)]
pub struct AnimClock {
    /// 距动画时间轴起点（窗口由隐藏变可见那一刻）的时长。
    pub t: std::time::Duration,
    /// 窗口此刻还不在屏上。刷新时即「这一帧由隐藏变可见」：各装饰状态机从头播。
    pub showing: bool,
    /// 播不播动画：`ui.theme.animation` 且系统没关动画效果。关着时装饰停在初始状态首帧。
    pub allowed: bool,
}

impl CandidateScene {
    /// 按当前主题、横竖排、嵌入形态、DPI、字号求皮肤几何；不启用皮肤时置 None。
    /// 由隐藏变可见时各装饰状态机从头播；再按状态机给各装饰选帧，并记下次重算时刻。
    pub fn refresh_skin(&mut self, inp: &CandidateSceneInput, clock: AnimClock) {
        use crate::skin_anim::Machine;
        self.skin_geom = self.compute_skin_geom(inp, clock.allowed);
        self.ensure_decor_anims(inp, clock);
        let t = clock.t;
        let variant = inp
            .theme
            .skin
            .as_ref()
            .and_then(|s| s.variant(inp.vertical, inp.preedit_embedded));
        let mut next = None;
        if let (Some(g), Some(v)) = (self.skin_geom.as_mut(), variant) {
            for d in &mut g.decor {
                let Some(spec) = v.decor.get(d.index).map(|x| &x.anim) else {
                    continue;
                };
                let pose = match self.decor_anims.get_mut(d.index) {
                    Some(m) if clock.allowed => {
                        if clock.showing {
                            m.show(spec, t, &*d);
                        }
                        m.seek(spec, t, &*d)
                    }
                    _ => Machine::still(spec, &*d),
                };
                d.cur = d.locate(pose);
                next = [next, pose.next].into_iter().flatten().min();
            }
        }
        self.anim_next = next;
    }

    /// 下一次换帧时刻（距动画时间轴起点）；None = 本帧没有在播的动画。
    pub fn next_anim_at(&self) -> Option<std::time::Duration> {
        self.anim_next
    }

    /// 同一条皮肤告警只报一次（几何每帧重求）。
    pub(super) fn warn_skin_once(&self, msg: String) {
        if self.skin_warned.borrow_mut().insert(msg.clone()) {
            tracing::warn!("{msg}");
        }
    }

    /// 状态机按当前变体的装饰一块一台；换了变体（横竖 / 嵌入）或被作废时重建，回到初始状态。
    fn ensure_decor_anims(&mut self, inp: &CandidateSceneInput, clock: AnimClock) {
        let key = (inp.vertical, inp.preedit_embedded);
        if self.decor_anims_key == Some(key) {
            return;
        }
        let visible = !clock.showing;
        self.decor_anims = inp
            .theme
            .skin
            .as_ref()
            .and_then(|s| s.variant(key.0, key.1))
            .map_or_else(Vec::new, |v| {
                v.decor
                    .iter()
                    .map(|d| crate::skin_anim::Machine::new(&d.anim, clock.t, visible))
                    .collect()
            });
        self.decor_anims_key = Some(key);
    }

    /// 喂一个装饰事件；`only` 只给那一块装饰（`click`）。不播动画时不切换。
    /// 返回画面是否可能变了（显示中由窗口登记换帧重绘）。
    pub fn decor_event(
        &mut self,
        inp: &CandidateSceneInput,
        ev: DecorEvent,
        only: Option<usize>,
        clock: AnimClock,
    ) -> bool {
        use crate::skin_anim::{AnyClip, Timelines};
        if !clock.allowed || inp.theme.skin.is_none() {
            return false;
        }
        self.ensure_decor_anims(inp, clock);
        let key = (inp.vertical, inp.preedit_embedded);
        let t = clock.t;
        let Some(v) = inp
            .theme
            .skin
            .as_ref()
            .and_then(|s| s.variant(inp.vertical, inp.preedit_embedded))
        else {
            return false;
        };
        let mut changed = false;
        for (i, m) in self.decor_anims.iter_mut().enumerate() {
            if only.is_some_and(|o| o != i) {
                continue;
            }
            let Some(spec) = v.decor.get(i).map(|d| &d.anim) else {
                continue;
            };
            // 还没求过几何（从未显示），或几何是另一个变体的（隐藏期间换了横竖 / 嵌入）：
            // 不知道片段时长，只会记常驻状态。
            let geom = self
                .skin_geom
                .as_ref()
                .filter(|g| g.variant_key == key)
                .and_then(|g| g.decor.iter().find(|d| d.index == i));
            let tl: &dyn Timelines = match geom {
                Some(d) => d,
                None => &AnyClip,
            };
            changed |= match ev {
                DecorEvent::Hide => m.hide(spec, t, tl),
                ev => m.fire(spec, ev, t, tl),
            };
        }
        changed
    }

    fn compute_skin_geom(&self, inp: &CandidateSceneInput, anim_allowed: bool) -> Option<SkinGeom> {
        let skin = inp.theme.skin.as_ref()?;
        let v = skin.variant(inp.vertical, inp.preedit_embedded)?;
        // 主坐标（skin.density 源像素）→ 设备像素。
        let k = skin.factor(inp.scale, inp.base_font_logical);
        // 渲染缩放：设备像素 / dp。
        let render_scale = k * skin.density;
        let (src_ref, src_density) = pick_source(v, render_scale);
        let frame_path = crate::theme_assets::asset_path(inp.theme, src_ref)?;
        if crate::image_cache::is_svg_source(&frame_path) {
            tracing::warn!("skin: 窗体图须为位图（SVG 不支持多段切片），回退普通窗口");
            return None;
        }
        // 拉伸带按 frame.image 的像素写，换算到所选源图。
        let band_f = src_density / v.frame.density;
        let sx = scale_stretch(&v.frame.stretch_x, band_f);
        let sy = scale_stretch(&v.frame.stretch_y, band_f);
        let frame_k = render_scale / src_density;
        // 倍率来自主题（density）：窗体图集超出位图上限就整套回退，不去分配它。
        if let Some((sw, sh)) = crate::view::image_src_size(&frame_path)
            && !crate::skin::fits_image_limits(
                crate::skin::atlas_len(sw, frame_k),
                crate::skin::atlas_len(sh, frame_k),
            )
        {
            self.warn_skin_once(format!(
                "skin: 窗体图按倍率 {frame_k} 缩放后超出上限，回退普通窗口"
            ));
            return None;
        }
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
        // 每块装饰用到的文件：[0] 是装饰本身，其后是片段 `{ image = .. }` 引用的（去重；
        // 找不到的记 None，引用它的片段不可用）。
        let plans: Vec<_> = v
            .decor
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                let main = crate::theme_assets::asset_path(inp.theme, &d.image)?;
                let mut names: Vec<&str> = Vec::new();
                let clips =
                    d.anim
                        .states
                        .iter()
                        .map(|(_, c)| c)
                        .chain(d.anim.on.iter().filter_map(|(_, a)| match a {
                            DecorAction::Play { clip, .. } => Some(clip),
                            DecorAction::State(_) => None,
                        }));
                for c in clips {
                    if let DecorClip::Image(n) = c
                        && !names.contains(&n.as_str())
                    {
                        names.push(n);
                    }
                }
                let extra: Vec<(&str, Option<String>)> = names
                    .into_iter()
                    .map(|n| (n, crate::theme_assets::asset_path(inp.theme, n)))
                    .collect();
                // 关着动画也要解帧的唯一情形：初始状态从装饰文件的非 0 帧起。
                let still_needs_frames = d.anim.initial.is_some_and(
                    |s| matches!(d.anim.states[s].1, DecorClip::Frames { first, .. } if first > 0),
                );
                let decode_main =
                    crate::view::image_is_animated(&main) && (anim_allowed || still_needs_frames);
                let decode_extra: Vec<bool> = extra
                    .iter()
                    .map(|(_, p)| {
                        anim_allowed && p.as_deref().is_some_and(crate::view::image_is_animated)
                    })
                    .collect();
                Some((i, d, main, decode_main, extra, decode_extra))
            })
            .collect();
        // 动画帧预算按本变体要解帧的文件均分，合计不超 24 MiB。
        let n_anim: usize = plans
            .iter()
            .map(|(_, _, _, m, _, x)| *m as usize + x.iter().filter(|b| **b).count())
            .sum();
        let budget = crate::image_cache::ANIM_BUDGET_BYTES / n_anim.max(1);
        let decor = plans
            .into_iter()
            .filter_map(|(index, d, main, decode_main, extra, decode_extra)| {
                let (sw, sh) = crate::view::image_src_size(&main)?;
                let (w, h) = match d.size {
                    Some([w, h]) => (w * k, h * k),
                    None => {
                        let f = render_scale / d.density;
                        (sw as f32 * f, sh as f32 * f)
                    }
                };
                // 不可信主题可声明任意大的装饰：按原比例缩进位图上限，别让分配直接 abort。
                let (w, h) = crate::skin::clamp_to_image_limits(w.round(), h.round());
                // 每个源：路径、倍率、解帧结果。片段文件按装饰同一目标尺寸铺。
                let source = |path: &str, decode: bool| {
                    let sw = crate::view::image_src_size(path).map_or(sw, |(w, _)| w);
                    let k = w as f32 / sw.max(1) as f32;
                    let layout = decode
                        .then(|| crate::view::anim_layout(path, k, w, h, d.fps, budget))
                        .flatten();
                    (k, layout)
                };
                let (main_k, main_layout) = source(&main, decode_main);
                // 装饰文件的原始帧数：没解帧又是动图时不知道（只在关动画时出现，此时只用首帧）。
                let main_raw = match &main_layout {
                    Some(l) => Some(l.raw_frames),
                    None if decode_main || !crate::view::image_is_animated(&main) => Some(1),
                    None => None,
                };
                let mut sources = vec![DecorSource {
                    path: main.clone(),
                    k: main_k,
                    animated: main_layout.is_some(),
                }];
                let mut layouts = vec![main_layout];
                let mut extra_idx: Vec<(&str, Option<usize>)> = Vec::new();
                for ((name, path), decode) in extra.into_iter().zip(decode_extra) {
                    // `asset_path` 找不到时原样返回引用名，读不出尺寸才算真缺失（结果缓存，不每帧读盘）。
                    let Some(path) = path.filter(|p| crate::view::image_src_size(p).is_some())
                    else {
                        extra_idx.push((name, None));
                        continue;
                    };
                    let (k, layout) = source(&path, decode);
                    sources.push(DecorSource {
                        path,
                        k,
                        animated: layout.is_some(),
                    });
                    layouts.push(layout);
                    extra_idx.push((name, Some(sources.len() - 1)));
                }
                let clip_of = |c: &DecorClip, what: &dyn Fn() -> String| -> Option<ClipTl> {
                    let tl = match c {
                        DecorClip::Frames { first, last } => {
                            if layouts[0].as_ref().is_some_and(|l| !l.aligned) {
                                self.warn_skin_once(format!(
                                    "skin: 装饰 {} 有帧解码失败，帧区间片段按整段播",
                                    d.image
                                ));
                            }
                            frames_clip(*first, *last, layouts[0].as_ref(), main_raw)
                        }
                        DecorClip::Image(n) => extra_idx
                            .iter()
                            .find(|(x, _)| x == n)
                            .and_then(|(_, si)| *si)
                            .map(|si| ClipTl::whole(si, layouts[si].as_ref())),
                    };
                    if tl.is_none() {
                        self.warn_skin_once(format!(
                            "skin: 装饰 {} 的 {} 越界或文件缺失，忽略",
                            d.image,
                            what()
                        ));
                    }
                    tl
                };
                let states = d
                    .anim
                    .states
                    .iter()
                    .map(|(n, c)| clip_of(c, &|| format!("states.{n}")))
                    .collect();
                let on = d
                    .anim
                    .on
                    .iter()
                    .map(|(ev, a)| match a {
                        DecorAction::Play { clip, .. } => clip_of(clip, &|| format!("on.{ev:?}")),
                        DecorAction::State(_) => None,
                    })
                    .collect();
                let mut g = DecorGeom {
                    index,
                    w,
                    h,
                    anchor: d.anchor,
                    pivot: d.pivot,
                    off: (d.offset[0] * k, d.offset[1] * k),
                    z: d.z,
                    hit: d.hit,
                    opacity: d.opacity,
                    fps: d.fps,
                    budget,
                    sources,
                    base: ClipTl::whole(0, layouts[0].as_ref()),
                    states,
                    on,
                    cur: (0, 0),
                    still: (0, 0),
                };
                g.still = g.locate(crate::skin_anim::Machine::still(&d.anim, &g));
                g.cur = g.still;
                Some(g)
            })
            .collect();
        Some(SkinGeom {
            variant_key: (inp.vertical, inp.preedit_embedded),
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
    pub fn skin_layout(
        &mut self,
        root: &mut View,
        text: &dyn TextBackend,
    ) -> Option<(u32, u32, u32, u32)> {
        self.skin_geom.as_ref()?;
        let free = self.skin_is_free();
        if free {
            self.skin_measure_parts(root, text);
        }
        root.layout(0.0, 0.0, text);
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
        root.layout(ml as f32, mt as f32, text);
        if free {
            self.skin_place_parts(text);
        }
        Some((ml as u32, mt as u32, mr as u32, mb as u32))
    }

    /// 本帧是否分槽位布局。
    pub fn skin_is_free(&self) -> bool {
        self.skin_geom.as_ref().is_some_and(|g| g.slots.is_some())
    }

    /// 分槽位：取走 `build_tree` 交出的各块，按自然尺寸排一遍，解出能装下所有槽位的最小窗体，
    /// 写进根容器（空盒）的最小尺寸。
    fn skin_measure_parts(&mut self, root: &mut View, text: &dyn TextBackend) {
        let taken = std::mem::take(&mut *self.free_parts.borrow_mut());
        let mut parts: Vec<Part> = taken
            .into_iter()
            .map(|(kind, mut view)| {
                view.layout(0.0, 0.0, text);
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
    fn skin_place_parts(&mut self, text: &dyn TextBackend) {
        let Some(g) = self.skin_geom.as_mut() else {
            return;
        };
        let (ml, mt, fw, fh) = g.frame;
        let pos = g.part_positions(fw as f32, fh as f32);
        for (p, (x, y)) in g.parts.iter_mut().zip(pos) {
            p.view.layout(ml as f32 + x, mt as f32 + y, text);
        }
    }

    /// [`CandidateScene::layout`] 对内容尺寸做完钳制（竖排宽度上限、屏幕安全上限、最小 40×24）
    /// 后调用：窗体按钳后的尺寸画，装饰随之重新定位。外扩量沿用布局时的值——它已经按窗体自然尺寸量过，
    /// 窗体变窄只会让右侧多出一点透明余量，不会让装饰出缓冲。
    pub fn skin_fit_frame(&mut self, content_w: u32, content_h: u32, text: &dyn TextBackend) {
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
            self.skin_place_parts(text);
        }
    }

    /// 合成窗体 + 全部装饰，取光标保护带（对齐点横坐标 `caret_x` 向左 `before`、向右 `after`，dp）
    /// 内逐行的不透明情况。行坐标相对窗体顶边（设备像素）。
    fn skin_avoid_mask(
        &self,
        g: &SkinGeom,
        caret_x: f32,
        strip: wind_theme::skin::AvoidStrip,
        scale: f32,
    ) -> AvoidMask {
        let (ml, mt, mr, mb) = g.margins;
        let (_, _, w, h) = g.frame;
        let (bw, bh) = (w + ml + mr, h + mt + mb);
        let mut buf = vec![0u8; (bw as usize) * (bh as usize) * 4];
        // 固定按首帧算：随帧变的话打字时窗口会在按键间跳。
        g.paint_at(&mut buf, bw, bh, false, false, true);
        g.paint_at(&mut buf, bw, bh, true, false, true);
        let cx = ml as f32 + caret_x;
        let x0 = (cx - strip.before * scale).floor().max(0.0) as usize;
        let x1 = ((cx + strip.after * scale).ceil().max(0.0) as usize).min(bw as usize);
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
    pub fn skin_place_anchor(&self, f: f32, scale: f32) -> PlaceAnchor {
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
                let mask = |caret_x: f32| self.skin_avoid_mask(g, caret_x, strip, scale).scaled(f);
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
            index: 0,
            w,
            h,
            anchor: Anchor::parse(anchor).unwrap(),
            pivot,
            off,
            z: 1,
            hit: DecorHit::Block,
            opacity: 1.0,
            fps: None,
            budget: 0,
            sources: vec![DecorSource {
                path: String::new(),
                k: 1.0,
                animated: false,
            }],
            base: ClipTl::whole(0, None),
            states: Vec::new(),
            on: Vec::new(),
            cur: (0, 0),
            still: (0, 0),
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

    #[test]
    fn frames_map_onto_kept_frames_after_budget_drop() {
        use crate::image_cache::AnimLayout;
        // 原始 6 帧、隔帧丢成 3 帧：保留的是原始 0 / 2 / 4。
        let l = AnimLayout {
            durations: vec![200, 200, 200],
            stride: 2,
            raw_frames: 6,
            aligned: true,
        };
        let c = |a, b| frames_clip(a, b, Some(&l), Some(6)).map(|c| (c.first, c.durations));
        assert_eq!(c(2, 3), Some((1, vec![200])), "2-3 都落在保留的第 1 帧");
        assert_eq!(
            c(1, 4),
            Some((1, vec![200, 200])),
            "起点 1 被丢：从它之后的保留帧（原始 2）起，不画区间外的原始 0"
        );
        assert_eq!(c(0, 4), Some((0, vec![200, 200, 200])));
        assert_eq!(
            c(3, 3),
            Some((1, vec![200])),
            "区间里没有保留帧：取终点所在那帧"
        );
        assert_eq!(c(5, 5), Some((2, vec![200])));
        assert_eq!(c(4, 6), None, "越界");
        let lost = AnimLayout {
            aligned: false,
            ..l.clone()
        };
        let whole = frames_clip(2, 3, Some(&lost), Some(6)).unwrap();
        assert_eq!(
            (whole.first, whole.durations.len()),
            (0, 3),
            "有帧生成失败：映射不可靠，按整段播"
        );
        assert!(
            frames_clip(0, 0, None, Some(1)).is_some(),
            "静态图只有第 0 帧"
        );
        assert!(frames_clip(1, 1, None, Some(1)).is_none());
        assert!(
            frames_clip(3, 5, None, None).is_some(),
            "没解帧（关着动画）时不判越界"
        );
    }
}
