//! 候选窗的异形皮肤接入：几何求值、布局外扩、绘制、对齐点换算。
//! 设计见 `docs/design/theme-skin-animation.md` §3–§5。
//!
//! 接入方式：皮肤生效时根容器不画背景 / 边框 / 阴影，内边距换成皮肤内容框、最小尺寸
//! 取窗体固定段之和；根容器的盒就是窗体。窗体与装饰在根容器绘制前后各画一遍。

use super::scene::{CandidateScene, CandidateSceneInput};
use super::{CandidateWindow, TextRenderer};
use crate::view::{Edges, Rect, View};
use wind_theme::skin::{
    Anchor, AnchorBox, BandFill, Clamp, DecorAction, DecorClip, DecorEvent, DecorHit, Pos9,
    RvSkinVariant, RvSlot, RvSlots, Stretch,
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
pub(super) struct SkinGeom {
    /// 求它时的变体（竖排, 嵌入）。
    variant_key: (bool, bool),
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

    /// 不起拖的装饰矩形（窗口缓冲坐标）：`block`，以及穿透窗口建不出来、回落画进主窗口的 `pass`。
    /// `drag` 装饰不在其中——按在它上面与按在窗体空白处一样拖动。
    pub(super) fn no_drag_rects(&self) -> Vec<Rect> {
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
    pub(super) fn click_rects(&self) -> Vec<(usize, Rect, bool)> {
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
        for (d, &rect) in self.decor.iter().zip(&self.decor_rects) {
            if d.hit == DecorHit::Pass {
                d.paint(buf, bw, bh, rect, d.cur);
            }
        }
    }

    /// 画皮肤层：`over = false` 画窗体及其上下的装饰（根容器之前调用），`true` 画盖在内容上的装饰。
    /// `skip_pass`：穿透装饰另由穿透窗口画，这里跳过。
    pub(super) fn paint(&self, buf: &mut [u8], bw: u32, bh: u32, over: bool, skip_pass: bool) {
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

#[cfg(all(test, mock_text))]
impl SkinGeom {
    /// 渲染金样用：几何逐字段转储，每块装饰附「此刻的帧 + 单独画出的像素摘要」。
    pub(super) fn golden_dump(&self, digest: fn(&[u8]) -> u64) -> String {
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
pub(super) struct AnimClock {
    /// 距动画时间轴起点（窗口由隐藏变可见那一刻）的时长。
    pub(super) t: std::time::Duration,
    /// 窗口此刻还不在屏上。刷新时即「这一帧由隐藏变可见」：各装饰状态机从头播。
    pub(super) showing: bool,
    /// 播不播动画：`ui.theme.animation` 且系统没关动画效果。关着时装饰停在初始状态首帧。
    pub(super) allowed: bool,
}

impl CandidateScene {
    /// 按当前主题、横竖排、嵌入形态、DPI、字号求皮肤几何；不启用皮肤时置 None。
    /// 由隐藏变可见时各装饰状态机从头播；再按状态机给各装饰选帧，并记下次重算时刻。
    pub(super) fn refresh_skin(&mut self, inp: &CandidateSceneInput, clock: AnimClock) {
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
    pub(super) fn next_anim_at(&self) -> Option<std::time::Duration> {
        self.anim_next
    }

    /// 同一条皮肤告警只报一次（几何每帧重求）。
    fn warn_skin_once(&self, msg: String) {
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
    pub(super) fn decor_event(
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
                let (w, h) = (w.round().max(1.0) as u32, h.round().max(1.0) as u32);
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
    pub(super) fn skin_layout(
        &mut self,
        root: &mut View,
        text: &TextRenderer,
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
    pub(super) fn skin_is_free(&self) -> bool {
        self.skin_geom.as_ref().is_some_and(|g| g.slots.is_some())
    }

    /// 分槽位：取走 `build_tree` 交出的各块，按自然尺寸排一遍，解出能装下所有槽位的最小窗体，
    /// 写进根容器（空盒）的最小尺寸。
    fn skin_measure_parts(&mut self, root: &mut View, text: &TextRenderer) {
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
    fn skin_place_parts(&mut self, text: &TextRenderer) {
        let Some(g) = self.skin_geom.as_mut() else {
            return;
        };
        let (ml, mt, fw, fh) = g.frame;
        let pos = g.part_positions(fw as f32, fh as f32);
        for (p, (x, y)) in g.parts.iter_mut().zip(pos) {
            p.view.layout(ml as f32 + x, mt as f32 + y, text);
        }
    }

    /// 渲染入口对内容尺寸做完钳制（竖排宽度上限、屏幕安全上限、最小 40×24）后调用：窗体按
    /// 钳后的尺寸画，装饰随之重新定位。外扩量沿用布局时的值——它已经按窗体自然尺寸量过，
    /// 窗体变窄只会让右侧多出一点透明余量，不会让装饰出缓冲。
    pub(super) fn skin_fit_frame(&mut self, content_w: u32, content_h: u32, text: &TextRenderer) {
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
    pub(super) fn skin_place_anchor(&self, f: f32, scale: f32) -> PlaceAnchor {
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

impl CandidateWindow {
    /// 刷新本帧皮肤几何与装饰动画（见 [`CandidateScene::refresh_skin`]）：由隐藏变可见时
    /// 重读动画开关、记动画起点。
    #[cfg_attr(not(any(windows, ext_presenter)), allow(dead_code))]
    pub(super) fn refresh_skin_geom(&mut self) {
        self.refresh_skin_geom_at(std::time::Instant::now());
    }

    /// 同 [`Self::refresh_skin_geom`]，时刻由调用方给（金样注入时间用）。
    pub(super) fn refresh_skin_geom_at(&mut self, now: std::time::Instant) {
        if !self.visible {
            self.refresh_anim_allowed();
            self.anim_start = now;
        }
        // 这一帧就是按此刻重画的，之前登记的「待重绘」作废。
        self.anim_due = false;
        let clock = self.anim_clock(now);
        self.with_scene(|scene, inp| scene.refresh_skin(inp, clock));
    }

    /// 此刻的动画时钟：时间轴起点、可见性与动画开关都在窗口侧。
    fn anim_clock(&self, now: std::time::Instant) -> AnimClock {
        AnimClock {
            t: now.saturating_duration_since(self.anim_start),
            showing: !self.visible,
            allowed: self.anim_allowed,
        }
    }

    /// 重读「播不播动画」：`ui.theme.animation` 且系统没关动画效果。变了就让状态机重来。
    pub(super) fn refresh_anim_allowed(&mut self) {
        let system = (self.motion_allowed)();
        let allowed = self.theme_animation && system;
        // 冻结原因变了记一条：远程桌面等场景系统关了动画效果，看不到动画不是 bug。
        let why = (self.theme_animation, system);
        if self.theme.skin.is_some() && self.anim_reported != Some(why) {
            self.anim_reported = Some(why);
            tracing::info!(
                "skin: 主题动画{}（ui.theme.animation={}，系统动画效果={}）",
                if allowed { "播放" } else { "冻结" },
                why.0,
                if why.1 { "开" } else { "关" }
            );
        }
        if allowed != self.anim_allowed {
            self.anim_allowed = allowed;
            self.scene.decor_anims_key = None;
        }
    }

    /// 喂一个装饰事件；`only` 只给那一块装饰（`click`）。不播动画时不切换。
    /// 显示中画面可能变了就登记一次换帧重绘。
    pub(super) fn skin_event(&mut self, ev: DecorEvent, only: Option<usize>) {
        self.skin_event_at(ev, only, std::time::Instant::now());
    }

    /// 同 [`Self::skin_event`]，时刻由调用方给（金样注入时间用）。
    pub(super) fn skin_event_at(
        &mut self,
        ev: DecorEvent,
        only: Option<usize>,
        now: std::time::Instant,
    ) {
        let clock = self.anim_clock(now);
        let changed = self.with_scene(|scene, inp| scene.decor_event(inp, ev, only, clock));
        if changed && self.visible {
            self.anim_due = true;
        }
    }

    /// 候选更新：比较相邻两次的编码串与页码，喂 `typing` / `backspace` / `page` / `empty`
    /// （`preedit` 取协调器下发的原串，编码由宿主自绘时也照样比）。
    ///
    /// 只有 `composing`（打字中）的帧才算编码：上屏后进联想时编码串变成空串或联想标识，
    /// 那不是退格也不是打字。不在打字中的帧之后再打字，按从空串起打算 `typing`。
    pub fn note_skin_input(
        &mut self,
        preedit: &str,
        composing: bool,
        page: usize,
        no_candidates: bool,
    ) {
        if self.theme.skin.is_none() {
            return;
        }
        let code = if composing { preedit } else { "" };
        let empty = composing && no_candidates;
        let prev = self
            .skin_input_prev
            .replace((code.to_string(), page, empty));
        let (old, old_page, old_empty) = match &prev {
            Some((p, n, e)) => (p.as_str(), Some(*n), *e),
            None => ("", None, false),
        };
        if !composing {
            // 联想 / 上屏后的帧：不报编码事件，只记下来作下一帧的基线。
        } else if code != old {
            let ev = if code.len() < old.len() && old.starts_with(code) {
                DecorEvent::Backspace
            } else {
                DecorEvent::Typing
            };
            self.skin_event(ev, None);
        } else if old_page.is_some_and(|n| n != page) {
            self.skin_event(DecorEvent::Page, None);
        }
        if empty && !old_empty {
            self.skin_event(DecorEvent::Empty, None);
        }
    }

    /// 工具栏状态：中英变了喂 `mode_cn` / `mode_en`（第一次只记下，不算切换）。
    pub fn note_skin_mode(&mut self, chinese: bool) {
        if self.theme.skin.is_none() {
            return;
        }
        if self
            .skin_mode_prev
            .replace(chinese)
            .is_some_and(|c| c != chinese)
        {
            let ev = if chinese {
                DecorEvent::ModeCn
            } else {
                DecorEvent::ModeEn
            };
            self.skin_event(ev, None);
        }
    }

    /// 协调器发来的装饰事件。
    pub fn skin_signal(&mut self, sig: wind_ui_types::SkinSignal) {
        match sig {
            wind_ui_types::SkinSignal::Commit => self.skin_event(DecorEvent::Commit, None),
        }
    }

    /// `ui.theme.animation`。
    pub fn set_theme_animation(&mut self, on: bool) {
        if self.theme_animation != on {
            self.theme_animation = on;
            self.refresh_anim_allowed();
            // 显示中就地换成播放 / 首帧。
            self.anim_due = self.visible;
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

    /// 夹具：30×20 窗体图（左上像素红、其余白；x 10..20 / y 8..12 可拉伸）+ 6×6 蓝色装饰，
    /// 装饰挂在窗体右上角外侧（底边右端贴锚点）。
    fn fixture_window(tag: &str) -> CandidateWindow {
        fixture_window_hit(tag, None)
    }

    fn fixture_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wind-skin-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture_window_hit(tag: &str, hit: Option<&str>) -> CandidateWindow {
        let dir = fixture_dir(tag);
        let decor = dir.join("decor.png");
        image::RgbaImage::from_pixel(6, 6, image::Rgba([0, 0, 255, 255]))
            .save(&decor)
            .unwrap();
        fixture_window_decor(&dir, &decor, hit)
    }

    /// 夹具窗体 + 给定的装饰文件（6×6）。
    fn fixture_window_decor(
        dir: &std::path::Path,
        decor: &std::path::Path,
        hit: Option<&str>,
    ) -> CandidateWindow {
        use wind_theme::skin::{BandSpec, Skin, SkinDecor, SkinFrame, SkinInsets, SkinVariant};
        let frame = dir.join("frame.png");
        let mut img = image::RgbaImage::from_pixel(30, 20, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(&frame).unwrap();
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
                    hit: hit.map(Into::into),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let theme = wind_theme::Resolved {
            skin: wind_theme::skin::resolve_skin(&skin).map(Box::new),
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
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let m = w
            .scene
            .skin_layout(&mut root, &w.text_renderer)
            .expect("皮肤生效");
        assert_eq!(m, (0, 6, 0, 0), "装饰伸出窗体上方 6px，只在上边外扩");
        let g = w.scene.skin_geom.as_ref().unwrap();
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
        assert!(
            w.scene.shadow_params(&w.scene_input(false)).is_none(),
            "皮肤不叠软件阴影"
        );
    }

    #[test]
    fn skin_paints_frame_corner_and_decor() {
        let mut w = fixture_window("paint");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let (ml, mt, mr, mb) = w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
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

    /// 按 `hit` 配一个挂在窗体右上外侧的装饰，返回「窗体外装饰上的一点是否不起拖」等三个判定。
    fn decor_no_drag(tag: &str, hit: Option<&str>) -> [bool; 3] {
        let mut w = fixture_window_hit(tag, hit);
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
        let mut m = w.mouse.borrow_mut();
        m.skin_frame = Some(g.frame_rect());
        m.skin_decor = g.no_drag_rects();
        let (_, _, fw, _) = g.frame;
        let x = fw as f32 - 3.0;
        [
            m.on_decor_only(x, 2.0),
            m.on_decor_only(x, 10.0),
            m.on_decor_only(1.0, 2.0),
        ]
    }

    #[test]
    fn block_decor_outside_frame_does_not_start_drag() {
        let [decor, in_frame, gap] = decor_no_drag("mouse-block", Some("block"));
        assert!(decor, "窗体外的装饰");
        assert!(!in_frame, "窗体内");
        assert!(!gap, "外扩区里装饰以外的透明处");
    }

    #[test]
    fn decor_drags_window_by_default() {
        assert_eq!(decor_no_drag("mouse-drag", None), [false; 3]);
    }

    #[test]
    fn clamped_content_size_moves_frame_and_decor() {
        let mut w = fixture_window("fit");
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let (_, _, fw, fh) = w.scene.skin_geom.as_ref().unwrap().frame;
        // 渲染入口把宽度钳窄 4px：窗体按钳后宽度画，右上角装饰跟着左移。
        w.scene.skin_fit_frame(fw - 4, fh, &w.text_renderer);
        let g = w.scene.skin_geom.as_ref().unwrap();
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
            w.scene.skin_geom.is_none(),
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
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let a = w.scene.skin_place_anchor(1.0, w.scale);
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
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let a = w.scene.skin_place_anchor(1.0, w.scale);
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
        let mut root = w.scene.build_tree(&w.scene_input(false));
        w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let a = w.scene.skin_place_anchor(1.0, w.scale);
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
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let (ml, mt, mr, mb) = w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
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
        assert!(w.scene.skin_is_free());
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let (ml, mt, _, _) = w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
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
        assert!(w.scene.skin_geom.is_none(), "竖排没配皮肤：不硬套横排图");
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
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    /// 装饰是两帧 GIF（红 100 ms、绿 200 ms）的夹具窗；`motion` 为系统动画开关的桩。
    fn anim_window(tag: &str, motion: fn() -> bool) -> CandidateWindow {
        let dir = fixture_dir(tag);
        let gif = dir.join("decor.gif");
        let bytes = crate::image_cache::anim_fixtures::gif(6, 6, &[(RED, 100), (GREEN, 200)]);
        std::fs::write(&gif, bytes).unwrap();
        let mut w = fixture_window_decor(&dir, &gif, None);
        w.motion_allowed = motion;
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w
    }

    /// 装饰右上角的像素（BGRA）：装饰挂在窗体右上外侧，缓冲右上角就是它。
    fn decor_px(w: &CandidateWindow) -> [u8; 4] {
        let (bw, _) = w.window.size();
        let o = ((bw - 1) * 4) as usize;
        let b = w.window.buffer();
        [b[o], b[o + 1], b[o + 2], b[o + 3]]
    }

    /// 期限在 `now` 之后 `max` 之内。
    fn due_within(d: Option<std::time::Instant>, max: std::time::Duration) -> bool {
        let now = std::time::Instant::now();
        d.is_some_and(|t| t > now && t <= now + max)
    }

    #[test]
    fn visible_animated_decor_schedules_next_frame() {
        let mut w = anim_window("anim-visible", || true);
        w.show();
        assert!(w.is_visible());
        assert!(
            due_within(w.next_deadline(), std::time::Duration::from_millis(100)),
            "首帧 100 ms 后换帧"
        );
        assert_eq!(decor_px(&w), [0, 0, 255, 255], "首帧是红");
    }

    #[test]
    fn hidden_window_does_not_wake_for_animation() {
        let mut w = anim_window("anim-hidden", || true);
        w.show();
        w.hide();
        assert!(
            !due_within(w.next_deadline(), std::time::Duration::from_secs(1)),
            "隐藏后只剩释放像素缓冲那类长期限"
        );
        w.tick();
        assert!(!w.take_anim_due());
    }

    #[test]
    fn static_decor_does_not_schedule_frames() {
        let mut w = fixture_window("anim-static");
        w.motion_allowed = || true;
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.show();
        assert_eq!(w.next_deadline(), None);
    }

    #[test]
    fn reduced_motion_keeps_first_frame_and_never_wakes() {
        let mut w = anim_window("anim-reduced", || false);
        w.show();
        assert_eq!(w.next_deadline(), None);
        assert_eq!(decor_px(&w), [0, 0, 255, 255], "停在首帧");
    }

    #[test]
    fn due_frame_repaints_without_resetting_timeline_or_hover() {
        let mut w = anim_window("anim-repaint", || true);
        w.show();
        // 等到换帧期限，tick 置标记。
        let due = w.next_deadline().expect("动画登记了期限");
        std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
        w.tick();
        assert!(w.take_anim_due());
        assert!(!w.take_anim_due(), "标记取走即清");
        let start = w.anim_start;
        w.mouse.borrow_mut().last_hover = 0;
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(w.anim_start, start, "重绘不重置时间轴");
        assert_eq!(w.mouse.borrow().last_hover, 0, "换帧重绘不动悬停去重");
        assert_eq!(decor_px(&w), [0, 255, 0, 255], "第二帧是绿");
        assert!(
            due_within(w.next_deadline(), std::time::Duration::from_millis(200)),
            "第二帧 200 ms 后回到首帧"
        );
        // 普通重绘（内容更新）照旧重置悬停去重。
        w.show();
        assert_eq!(w.mouse.borrow().last_hover, -1);
    }

    #[test]
    fn showing_again_restarts_the_timeline() {
        let mut w = anim_window("anim-restart", || true);
        w.show();
        w.anim_start -= std::time::Duration::from_millis(150);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), [0, 255, 0, 255], "经过 150 ms 在第二帧");
        w.hide();
        w.show();
        assert_eq!(decor_px(&w), [0, 0, 255, 255], "重新显示从首帧开始");
    }

    #[test]
    fn pass_decor_layer_follows_animation_frames() {
        let mut w = anim_window("anim-pass", || true);
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].hit = DecorHit::Pass;
            w.set_theme(theme);
        }
        w.show();
        w.anim_start -= std::time::Duration::from_millis(150);
        w.refresh_skin_geom();
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let (ml, mt, mr, mb) = w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
        let g = w.scene.skin_geom.as_ref().unwrap();
        let (_, _, fw, fh) = g.frame;
        let (bw, bh) = (fw + ml + mr, fh + mt + mb);
        let mut pass = vec![0u8; (bw * bh * 4) as usize];
        g.paint_pass(&mut pass, bw, bh);
        let o = ((bw - 1) * 4) as usize;
        assert_eq!(&pass[o..o + 4], &[0, 255, 0, 255], "穿透层画当前帧");
    }
    #[test]
    fn avoid_mask_ignores_current_animation_frame() {
        // 首帧不透明、第二帧全透明：保护带只按首帧算，换到第二帧结果不变。
        let dir = fixture_dir("anim-avoid");
        let gif = dir.join("decor.gif");
        let clear = [0, 0, 0, 0];
        let bytes = crate::image_cache::anim_fixtures::gif(6, 6, &[(RED, 100), (clear, 200)]);
        std::fs::write(&gif, bytes).unwrap();
        let mut w = fixture_window_decor(&dir, &gif, None);
        w.motion_allowed = || true;
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w.show();
        let mask = |w: &mut CandidateWindow| {
            w.refresh_skin_geom();
            let mut root = w.scene.build_tree(&w.scene_input(false));
            w.scene.skin_layout(&mut root, &w.text_renderer).unwrap();
            w.scene.skin_place_anchor(1.0, w.scale).avoid_below
        };
        let first = mask(&mut w);
        w.anim_start -= std::time::Duration::from_millis(150);
        let second = mask(&mut w);
        assert_eq!(
            w.scene.skin_geom.as_ref().unwrap().decor[0].cur.1,
            1,
            "已换到第二帧"
        );
        assert!(first.as_ref().unwrap().any(-6, 0), "首帧装饰压在保护带里");
        assert_eq!(first, second);
    }
    #[test]
    fn single_frame_gif_takes_no_budget_share() {
        let mut w = anim_window("anim-share", || true);
        let one = fixture_dir("anim-share").join("one.gif");
        std::fs::write(
            &one,
            crate::image_cache::anim_fixtures::gif(6, 6, &[(RED, 100)]),
        )
        .unwrap();
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            let mut d = v.decor[0].clone();
            d.image = one.to_string_lossy().into_owned();
            v.decor.push(d);
            w.set_theme(theme);
        }
        w.show();
        let g = w.scene.skin_geom.as_ref().unwrap();
        assert!(g.decor[0].sources[0].animated);
        assert!(!g.decor[1].sources[0].animated, "单帧 GIF 按静态图画");
        assert_eq!(
            g.decor[0].budget,
            crate::image_cache::ANIM_BUDGET_BYTES,
            "唯一的动画独占预算"
        );
    }

    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    /// 各色在缓冲里的 BGRA。
    const PX_RED: [u8; 4] = [0, 0, 255, 255];
    const PX_GREEN: [u8; 4] = [0, 255, 0, 255];
    const PX_BLUE: [u8; 4] = [255, 0, 0, 255];
    const PX_WHITE: [u8; 4] = [255, 255, 255, 255];

    use std::time::{Duration, Instant};
    use wind_theme::skin::RvDecorAnim;

    /// 装饰是四帧 GIF（红 / 绿 / 蓝 / 白，各 100 ms），状态 `a` = 0-1、`b` = 2-3（初始 a）。
    fn state_window(tag: &str, on: Vec<(DecorEvent, DecorAction)>) -> CandidateWindow {
        state_window_with(tag, Some(0), on)
    }

    fn state_window_with(
        tag: &str,
        initial: Option<usize>,
        on: Vec<(DecorEvent, DecorAction)>,
    ) -> CandidateWindow {
        let dir = fixture_dir(tag);
        let gif = dir.join("decor.gif");
        let frames = [(RED, 100), (GREEN, 100), (BLUE, 100), (WHITE, 100)];
        std::fs::write(&gif, crate::image_cache::anim_fixtures::gif(6, 6, &frames)).unwrap();
        let mut w = fixture_window_decor(&dir, &gif, None);
        w.motion_allowed = || true;
        let mut theme = w.theme_source.clone();
        let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
        v.decor[0].anim = RvDecorAnim {
            states: vec![
                ("a".into(), DecorClip::Frames { first: 0, last: 1 }),
                ("b".into(), DecorClip::Frames { first: 2, last: 3 }),
            ],
            initial,
            on,
        };
        w.set_theme(theme);
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        w
    }

    /// 处理一轮 UI 循环里的换帧重绘（同 manager）。
    fn pump(w: &mut CandidateWindow) {
        w.tick();
        if w.take_anim_due() {
            w.repaint_anim_frame(|w| w.show());
        }
    }

    fn play(first: u32, last: u32, then: Option<usize>) -> DecorAction {
        DecorAction::Play {
            clip: DecorClip::Frames { first, last },
            then,
        }
    }

    #[test]
    fn typing_switches_state_and_repaints() {
        let mut w = state_window(
            "sm-typing",
            vec![(DecorEvent::Typing, DecorAction::State(1))],
        );
        w.show();
        assert_eq!(decor_px(&w), PX_RED, "初始状态 a 的首帧");
        w.note_skin_input("a", true, 1, false);
        assert!(
            w.next_deadline().is_some_and(|d| d <= Instant::now()),
            "切了状态要立即醒来重绘"
        );
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE, "状态 b 从第 2 帧起");
        w.anim_start -= Duration::from_millis(150);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_WHITE, "b 在 2-3 之间循环");
        w.anim_start -= Duration::from_millis(100);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_BLUE);
    }

    #[test]
    fn input_diff_maps_to_typing_backspace_page_and_empty() {
        use DecorEvent::*;
        let on = vec![
            (Typing, play(1, 1, None)),
            (Backspace, play(2, 2, None)),
            (Page, play(3, 3, None)),
            (Empty, DecorAction::State(1)),
        ];
        let mut w = state_window("sm-diff", on);
        w.show();
        let step = |w: &mut CandidateWindow, pre: &str, page: usize, none: bool| {
            w.note_skin_input(pre, true, page, none);
            pump(w);
            decor_px(w)
        };
        assert_eq!(step(&mut w, "ab", 1, false), PX_GREEN, "typing");
        assert_eq!(step(&mut w, "a", 1, false), PX_BLUE, "前缀变短 = backspace");
        assert_eq!(step(&mut w, "a", 2, false), PX_WHITE, "同编码换页 = page");
        assert_eq!(
            step(&mut w, "ac", 2, false),
            PX_GREEN,
            "改了一个字也是 typing"
        );
        assert_eq!(
            step(&mut w, "acx", 1, true),
            PX_BLUE,
            "空码：typing 之后紧跟 empty，切到 b"
        );
    }

    #[test]
    fn commit_plays_clip_then_lands_on_state() {
        let on = vec![(DecorEvent::Commit, play(3, 3, Some(1)))];
        let mut w = state_window("sm-commit", on);
        w.show();
        w.skin_signal(wind_ui_types::SkinSignal::Commit);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_WHITE, "一次性片段");
        assert!(
            due_within(w.next_deadline(), Duration::from_millis(100)),
            "片段播完的时刻"
        );
        w.anim_start -= Duration::from_millis(100);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_BLUE, "播完转 then = b");
    }

    #[test]
    fn hidden_commit_lands_on_then_for_next_show() {
        let on = vec![
            (DecorEvent::Typing, DecorAction::State(1)),
            (DecorEvent::Commit, play(3, 3, Some(0))),
        ];
        let mut w = state_window("sm-hidden", on);
        w.show();
        w.note_skin_input("a", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE);
        w.hide();
        w.skin_signal(wind_ui_types::SkinSignal::Commit);
        assert!(!w.take_anim_due(), "隐藏时不重绘");
        w.show();
        assert_eq!(decor_px(&w), PX_RED, "隐藏时片段丢弃、直接落到 then = a");
    }

    #[test]
    fn mode_switch_ignores_first_toolbar_state() {
        let on = vec![
            (DecorEvent::ModeEn, DecorAction::State(1)),
            (DecorEvent::ModeCn, DecorAction::State(0)),
        ];
        let mut w = state_window("sm-mode", on);
        w.show();
        w.note_skin_mode(false);
        assert!(!w.take_anim_due(), "首次只记下");
        assert_eq!(decor_px(&w), PX_RED);
        w.note_skin_mode(false);
        assert!(!w.take_anim_due(), "没变");
        w.note_skin_mode(true);
        w.note_skin_mode(false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE, "切到英文");
    }

    #[test]
    fn idle_seconds_after_last_event() {
        let on = vec![
            (DecorEvent::Typing, DecorAction::State(1)),
            (DecorEvent::Idle(2), DecorAction::State(0)),
        ];
        let mut w = state_window("sm-idle", on);
        w.show();
        w.note_skin_input("a", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE);
        w.anim_start -= Duration::from_millis(2050);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_RED, "静默 2 秒回 a");
    }

    #[test]
    fn idle_is_registered_as_wakeup() {
        // 只有 idle 会动：初始状态是单帧。
        let mut w = state_window_with(
            "sm-idle-wake",
            Some(0),
            vec![(DecorEvent::Idle(1), DecorAction::State(1))],
        );
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].anim.states[0].1 = DecorClip::Frames { first: 0, last: 0 };
            w.set_theme(theme);
        }
        w.show();
        let d = w.next_deadline().expect("登记了 idle");
        let left = d.saturating_duration_since(Instant::now());
        assert!(left > Duration::from_millis(900) && left <= Duration::from_secs(1));
    }

    #[test]
    fn click_on_decor_within_drag_threshold() {
        let on = vec![(DecorEvent::Click, DecorAction::State(1))];
        let mut w = state_window("sm-click", on);
        w.show();
        let (bw, _) = w.window.size();
        let (x, y) = (bw as f32 - 2.0, 2.0);
        {
            let mut m = w.mouse.borrow_mut();
            m.press_decor(x, y, (100, 100));
            m.release_decor(x, y, (110, 100));
            assert_eq!(m.skin_click, None, "挪过拖动阈值是拖动");
            m.press_decor(x, y, (100, 100));
            m.release_decor(x, y, (103, 97));
            assert_eq!(m.skin_click, Some(0));
        }
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_BLUE);
        let mut m = w.mouse.borrow_mut();
        m.press_decor(5.0, 30.0, (0, 0));
        assert_eq!(m.decor_press, None, "窗体上按下不算装饰");
    }

    #[test]
    fn pass_decor_is_not_clickable() {
        let mut w = state_window("sm-click-pass", vec![]);
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].hit = DecorHit::Pass;
            w.set_theme(theme);
        }
        w.show();
        assert!(w.mouse.borrow().skin_click_rects.is_empty());
    }

    #[test]
    fn animation_off_freezes_initial_state_first_frame() {
        let on = vec![(DecorEvent::Typing, DecorAction::State(0))];
        let mut w = state_window_with("sm-off", Some(1), on);
        w.set_theme_animation(false);
        w.show();
        assert_eq!(decor_px(&w), PX_BLUE, "初始状态 b 的首帧（第 2 帧）");
        assert_eq!(w.next_deadline(), None, "不登记换帧");
        w.note_skin_input("a", true, 1, false);
        assert!(!w.take_anim_due(), "事件不切换");
        w.show();
        assert_eq!(decor_px(&w), PX_BLUE);
        w.set_theme_animation(true);
        assert!(w.take_anim_due(), "打开后就地重绘");
        w.repaint_anim_frame(|w| w.show());
        assert!(
            due_within(w.next_deadline(), Duration::from_millis(100)),
            "开始播放"
        );
    }

    #[test]
    fn system_reduced_motion_also_freezes_states() {
        let on = vec![(DecorEvent::Typing, DecorAction::State(0))];
        let mut w = state_window_with("sm-reduced", Some(1), on);
        w.motion_allowed = || false;
        w.show();
        assert_eq!(decor_px(&w), PX_BLUE);
        w.note_skin_input("a", true, 1, false);
        assert!(!w.take_anim_due());
    }

    #[test]
    fn out_of_range_state_is_dropped() {
        let on = vec![(DecorEvent::Typing, DecorAction::State(1))];
        let mut w = state_window("sm-range", on);
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            v.decor[0].anim.states[1].1 = DecorClip::Frames { first: 2, last: 9 };
            w.set_theme(theme);
        }
        w.show();
        w.note_skin_input("a", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_RED, "越界的状态当作没写，仍在 a");
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

    #[test]
    fn missing_image_clip_is_dropped() {
        for (tag, name) in [
            ("sm-miss-rel", "no_such_clip.gif".to_string()),
            (
                "sm-miss-abs",
                fixture_dir("sm-miss-abs")
                    .join("gone.gif")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ] {
            let mut w = state_window(tag, vec![]);
            {
                let mut theme = w.theme_source.clone();
                let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
                let clip = DecorClip::Image(name.clone());
                v.decor[0].anim.on =
                    vec![(DecorEvent::Commit, DecorAction::Play { clip, then: None })];
                w.set_theme(theme);
            }
            w.show();
            {
                let g = w.scene.skin_geom.as_ref().unwrap();
                assert_eq!(g.decor[0].sources.len(), 1, "{name}: 缺失文件不进源");
                assert!(g.decor[0].on[0].is_none(), "{name}: 片段不可用");
            }
            w.skin_signal(wind_ui_types::SkinSignal::Commit);
            assert!(!w.take_anim_due(), "{name}: 引用它的动作当作没写");
            assert_eq!(decor_px(&w), PX_RED);
        }
    }

    #[test]
    fn leaving_composition_is_not_backspace() {
        use DecorEvent::*;
        let on = vec![(Typing, play(1, 1, None)), (Backspace, play(2, 2, None))];
        let mut w = state_window("sm-assoc", on);
        w.show();
        w.note_skin_input("ab", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_GREEN, "typing");
        w.anim_start -= Duration::from_millis(150);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_RED, "片段播完");
        w.note_skin_input("", false, 1, false);
        assert!(!w.take_anim_due(), "上屏后编码清空不是退格");
        w.note_skin_input("联", false, 1, false);
        assert!(!w.take_anim_due(), "联想标识不是打字");
        w.note_skin_input("x", true, 1, false);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_GREEN, "联想之后再打字照常 typing");
    }

    #[test]
    fn image_clip_plays_another_file() {
        let mut w = state_window("sm-image", vec![]);
        let other = fixture_dir("sm-image").join("other.gif");
        let frames = [(GREEN, 100), (WHITE, 100)];
        std::fs::write(
            &other,
            crate::image_cache::anim_fixtures::gif(6, 6, &frames),
        )
        .unwrap();
        {
            let mut theme = w.theme_source.clone();
            let v = theme.skin.as_mut().unwrap().horizontal.as_mut().unwrap();
            let clip = DecorClip::Image(other.to_string_lossy().into_owned());
            v.decor[0].anim.on = vec![(DecorEvent::Commit, DecorAction::Play { clip, then: None })];
            w.set_theme(theme);
        }
        w.show();
        w.skin_signal(wind_ui_types::SkinSignal::Commit);
        pump(&mut w);
        assert_eq!(decor_px(&w), PX_GREEN, "另一个文件的首帧");
        let g = w.scene.skin_geom.as_ref().unwrap();
        assert_eq!(g.decor[0].sources.len(), 2);
        assert_eq!(
            g.decor[0].budget,
            crate::image_cache::ANIM_BUDGET_BYTES / 2,
            "两个动图文件均分预算"
        );
        w.anim_start -= Duration::from_millis(250);
        w.repaint_anim_frame(|w| w.show());
        assert_eq!(decor_px(&w), PX_RED, "播完回到 a");
    }
}
