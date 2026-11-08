//! 异形皮肤的纯算法：多段切片的轴映射、源图按倍率重采样（面积平均）、窗体合成。
//! 设计见 `docs/design/theme-skin-animation.md` §3.2、§5。
//!
//! 流程：源图（未预乘 RGBA）→ [`resample`] 按倍率缩成「图集」（预乘 RGBA）→ 每轴
//! [`map_axis`] 得到目标像素 → 图集坐标的采样表 → [`compose`] 双线性合成目标位图。
//! 固定段在图集里与目标里等长，采样落在像素中心，结果即逐像素拷贝。

use wind_theme::skin::{BandFill, Stretch};

/// 源轴上的一段：`[from, to)`（源像素），`fill = None` 为固定段。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SrcSeg {
    pub from: f32,
    pub to: f32,
    pub fill: Option<BandFill>,
}

/// 把拉伸声明展开成覆盖整轴 `[0, len)` 的段序列（固定段与拉伸带交替）。
pub fn src_segments(stretch: &Stretch, len: f32) -> Vec<SrcSeg> {
    match stretch {
        Stretch::Whole(fill) => vec![SrcSeg {
            from: 0.0,
            to: len,
            fill: Some(*fill),
        }],
        Stretch::Fixed => vec![SrcSeg {
            from: 0.0,
            to: len,
            fill: None,
        }],
        Stretch::Bands(bands) => {
            let mut out = Vec::with_capacity(bands.len() * 2 + 1);
            let mut cur = 0.0f32;
            for b in bands {
                let (from, to) = (b.from.clamp(0.0, len), b.to.clamp(0.0, len));
                if to <= from || from < cur {
                    continue;
                }
                if from > cur {
                    out.push(SrcSeg {
                        from: cur,
                        to: from,
                        fill: None,
                    });
                }
                out.push(SrcSeg {
                    from,
                    to,
                    fill: Some(b.fill),
                });
                cur = to;
            }
            if cur < len {
                out.push(SrcSeg {
                    from: cur,
                    to: len,
                    fill: None,
                });
            }
            out
        }
    }
}

/// 一段在图集与目标两侧的整数区间。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisSeg {
    /// 图集像素 `[a0, a1)`。
    pub a0: u32,
    pub a1: u32,
    /// 目标像素 `[d0, d1)`。
    pub d0: u32,
    pub d1: u32,
    pub fill: Option<BandFill>,
}

/// 源坐标 → 图集坐标（取整规则全模块唯一，切片两侧才能严丝合缝）。
fn to_atlas(x: f32, src_len: f32, atlas_len: u32) -> u32 {
    if src_len <= 0.0 {
        return 0;
    }
    ((x / src_len) * atlas_len as f32)
        .round()
        .clamp(0.0, atlas_len as f32) as u32
}

/// 固定段在图集里的总长 = 该轴的最小目标长度。
pub fn min_len(segs: &[SrcSeg], src_len: f32, atlas_len: u32) -> u32 {
    segs.iter()
        .filter(|s| s.fill.is_none())
        .map(|s| to_atlas(s.to, src_len, atlas_len) - to_atlas(s.from, src_len, atlas_len))
        .sum()
}

/// 轴映射：固定段等长拷贝；`dst_len` 比最小长度多出的部分按拉伸带的源长度比例分配
/// （最大余数法，总和精确等于 `dst_len`）。没有拉伸带而长度又不等时，整轴等比拉伸。
pub fn map_axis(segs: &[SrcSeg], src_len: f32, atlas_len: u32, dst_len: u32) -> Vec<AxisSeg> {
    let atlas: Vec<(u32, u32)> = segs
        .iter()
        .map(|s| {
            (
                to_atlas(s.from, src_len, atlas_len),
                to_atlas(s.to, src_len, atlas_len),
            )
        })
        .collect();
    let fixed = min_len(segs, src_len, atlas_len);
    let band_total: f32 = segs
        .iter()
        .filter(|s| s.fill.is_some())
        .map(|s| s.to - s.from)
        .sum();
    if band_total <= 0.0 {
        if dst_len == fixed {
            let mut d = 0;
            return segs
                .iter()
                .zip(&atlas)
                .map(|(s, &(a0, a1))| {
                    let seg = AxisSeg {
                        a0,
                        a1,
                        d0: d,
                        d1: d + (a1 - a0),
                        fill: s.fill,
                    };
                    d = seg.d1;
                    seg
                })
                .collect();
        }
        // 不可伸缩的轴被内容撑大：退化为整轴拉伸，宁可变形也不截断内容。
        return vec![AxisSeg {
            a0: 0,
            a1: atlas_len,
            d0: 0,
            d1: dst_len,
            fill: Some(BandFill::Stretch),
        }];
    }
    let extra = dst_len.saturating_sub(fixed);
    // 最大余数法分配 extra。
    let shares: Vec<f32> = segs
        .iter()
        .map(|s| match s.fill {
            Some(_) => extra as f32 * (s.to - s.from) / band_total,
            None => 0.0,
        })
        .collect();
    let mut lens: Vec<u32> = shares.iter().map(|v| v.floor() as u32).collect();
    let mut left = extra - lens.iter().sum::<u32>();
    let mut order: Vec<usize> = (0..segs.len())
        .filter(|&i| segs[i].fill.is_some())
        .collect();
    order.sort_by(|&a, &b| {
        let ra = shares[a] - shares[a].floor();
        let rb = shares[b] - shares[b].floor();
        rb.total_cmp(&ra).then(a.cmp(&b))
    });
    for i in order {
        if left == 0 {
            break;
        }
        lens[i] += 1;
        left -= 1;
    }
    let mut d = 0;
    segs.iter()
        .zip(&atlas)
        .zip(&lens)
        .map(|((s, &(a0, a1)), &band_len)| {
            let len = match s.fill {
                Some(_) => band_len,
                None => a1 - a0,
            };
            let seg = AxisSeg {
                a0,
                a1,
                d0: d,
                d1: d + len,
                fill: s.fill,
            };
            d = seg.d1;
            seg
        })
        .collect()
}

/// 目标轴上每个像素对图集的采样：`(i0, i1, t)`，结果 = lerp(i0, i1, t)。
pub type Tap = (u32, u32, f32);

/// 由轴映射生成采样表。采样恒钳在本段图集区间内，相邻段的像素不会渗进来。
pub fn taps(segs: &[AxisSeg]) -> Vec<Tap> {
    let total = segs.last().map_or(0, |s| s.d1) as usize;
    let mut out = Vec::with_capacity(total);
    for s in segs {
        let alen = s.a1.saturating_sub(s.a0);
        let dlen = s.d1 - s.d0;
        if dlen == 0 {
            continue;
        }
        if alen == 0 {
            // 图集里这段缩没了（极小倍率）：取相邻像素兜底，避免越界。
            let i = s.a0.saturating_sub(1);
            out.extend(std::iter::repeat_n((i, i, 0.0), dlen as usize));
            continue;
        }
        let last = alen - 1;
        let lerp_at = |t: f32| -> Tap {
            let t = t.clamp(0.0, last as f32);
            let i0 = t.floor() as u32;
            let i1 = (i0 + 1).min(last);
            (s.a0 + i0, s.a0 + i1, t - i0 as f32)
        };
        for k in 0..dlen {
            let tap = match s.fill {
                None => {
                    let i = k.min(last);
                    (s.a0 + i, s.a0 + i, 0.0)
                }
                Some(BandFill::Stretch) => {
                    lerp_at((k as f32 + 0.5) * alen as f32 / dlen as f32 - 0.5)
                }
                Some(BandFill::Tile) => {
                    let i = k % alen;
                    (s.a0 + i, s.a0 + i, 0.0)
                }
                Some(BandFill::Round) => {
                    let n = ((dlen as f32 / alen as f32).round() as u32).max(1);
                    let unit = dlen as f32 / n as f32;
                    let u = (k as f32 + 0.5) % unit;
                    lerp_at(u * alen as f32 / unit - 0.5)
                }
            };
            out.push(tap);
        }
    }
    out
}

/// 一维重采样权重：目标像素 j ← Σ w·src[start + i]。缩小做面积平均，放大做双线性。
fn weights(src_len: u32, dst_len: u32) -> Vec<(u32, Vec<f32>)> {
    let r = src_len as f64 / dst_len as f64;
    (0..dst_len)
        .map(|j| {
            if r >= 1.0 {
                let (a, b) = (j as f64 * r, (j as f64 + 1.0) * r);
                let start = a.floor() as u32;
                let end = (b.ceil() as u32).min(src_len);
                let w: Vec<f32> = (start..end)
                    .map(|i| {
                        let lo = a.max(i as f64);
                        let hi = b.min(i as f64 + 1.0);
                        ((hi - lo) / r) as f32
                    })
                    .collect();
                (start, w)
            } else {
                let c = ((j as f64 + 0.5) * r - 0.5).clamp(0.0, (src_len - 1) as f64);
                let i0 = c.floor() as u32;
                let i1 = (i0 + 1).min(src_len - 1);
                let t = (c - i0 as f64) as f32;
                if i1 == i0 {
                    (i0, vec![1.0])
                } else {
                    (i0, vec![1.0 - t, t])
                }
            }
        })
        .collect()
}

/// 源图（未预乘 RGBA8）→ `dw×dh` 预乘 RGBA8。先预乘再滤波，半透明边缘不出黑边。
pub fn resample(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (dw, dh) = (dw.max(1), dh.max(1));
    // 预乘成 f32。
    let pre: Vec<[f32; 4]> = src
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [
                p[0] as f32 * a,
                p[1] as f32 * a,
                p[2] as f32 * a,
                p[3] as f32,
            ]
        })
        .collect();
    // 横向。
    let wx = weights(sw, dw);
    let mut mid = vec![[0f32; 4]; (dw * sh) as usize];
    for y in 0..sh as usize {
        let row = &pre[y * sw as usize..(y + 1) * sw as usize];
        for (x, (start, w)) in wx.iter().enumerate() {
            let mut acc = [0f32; 4];
            for (i, wi) in w.iter().enumerate() {
                let p = row[*start as usize + i];
                for c in 0..4 {
                    acc[c] += p[c] * wi;
                }
            }
            mid[y * dw as usize + x] = acc;
        }
    }
    // 纵向。
    let wy = weights(sh, dh);
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for (y, (start, w)) in wy.iter().enumerate() {
        for x in 0..dw as usize {
            let mut acc = [0f32; 4];
            for (i, wi) in w.iter().enumerate() {
                let p = mid[(*start as usize + i) * dw as usize + x];
                for c in 0..4 {
                    acc[c] += p[c] * wi;
                }
            }
            let a = acc[3].round().clamp(0.0, 255.0);
            let o = (y * dw as usize + x) * 4;
            for c in 0..3 {
                out[o + c] = acc[c].round().clamp(0.0, a) as u8;
            }
            out[o + 3] = a as u8;
        }
    }
    out
}

/// 双线性合成：图集（预乘 RGBA8，`aw` 宽）按两轴采样表写出 `xs.len()×ys.len()` 的
/// 预乘 RGBA8。
pub fn compose(atlas: &[u8], aw: u32, xs: &[Tap], ys: &[Tap]) -> Vec<u8> {
    let (w, h) = (xs.len(), ys.len());
    let mut out = vec![0u8; w * h * 4];
    let px = |x: u32, y: u32| -> [f32; 4] {
        let o = ((y * aw + x) * 4) as usize;
        [
            atlas[o] as f32,
            atlas[o + 1] as f32,
            atlas[o + 2] as f32,
            atlas[o + 3] as f32,
        ]
    };
    for (dy, &(y0, y1, ty)) in ys.iter().enumerate() {
        for (dx, &(x0, x1, tx)) in xs.iter().enumerate() {
            let o = (dy * w + dx) * 4;
            if tx == 0.0 && ty == 0.0 {
                let s = ((y0 * aw + x0) * 4) as usize;
                out[o..o + 4].copy_from_slice(&atlas[s..s + 4]);
                continue;
            }
            let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
            let mut v = [0f32; 4];
            for i in 0..4 {
                let top = a[i] + (b[i] - a[i]) * tx;
                let bot = c[i] + (d[i] - c[i]) * tx;
                v[i] = top + (bot - top) * ty;
            }
            let alpha = v[3].round().clamp(0.0, 255.0);
            for i in 0..3 {
                out[o + i] = v[i].round().clamp(0.0, alpha) as u8;
            }
            out[o + 3] = alpha as u8;
        }
    }
    out
}

/// 单张位图（解码源、图集、合成目标）的单边上限（像素）。主题可能来自市场，是不可信内容：
/// 不设上限时一个声明巨大尺寸或倍率的主题能让 UI 线程分配几个 GB 直接 abort。
pub const MAX_IMAGE_SIDE: u32 = 8192;
/// 单张位图的总像素上限（约 32 MP，RGBA8 即 128 MiB）。
pub const MAX_IMAGE_PIXELS: u64 = 32 * 1024 * 1024;

/// `w × h` 在 [`MAX_IMAGE_SIDE`] / [`MAX_IMAGE_PIXELS`] 之内。
pub fn fits_image_limits(w: u32, h: u32) -> bool {
    w <= MAX_IMAGE_SIDE && h <= MAX_IMAGE_SIDE && (w as u64) * (h as u64) <= MAX_IMAGE_PIXELS
}

/// 把目标尺寸（设备像素，可为小数）按原比例缩进上限，至少 1×1。
pub fn clamp_to_image_limits(w: f32, h: f32) -> (u32, u32) {
    let (w, h) = (w.max(1.0), h.max(1.0));
    let side = MAX_IMAGE_SIDE as f32;
    let mut f = (side / w).min(side / h).min(1.0);
    let px = (w * f) as f64 * (h * f) as f64;
    if px > MAX_IMAGE_PIXELS as f64 {
        f *= (MAX_IMAGE_PIXELS as f64 / px).sqrt() as f32;
    }
    let c = |v: f32| ((v * f).round() as u32).clamp(1, MAX_IMAGE_SIDE);
    let (cw, ch) = (c(w), c(h));
    // 取整后仍可能略超像素上限，再按长边收一格。
    if fits_image_limits(cw, ch) {
        (cw, ch)
    } else if cw >= ch {
        (cw - 1, ch)
    } else {
        (cw, ch - 1)
    }
}

/// 某轴图集长度：源长 × 倍率，至少 1。
pub fn atlas_len(src_len: u32, k: f32) -> u32 {
    ((src_len as f32 * k).round() as u32).max(1)
}

/// 帧率上限 30：单帧至少 33 ms。
pub const MIN_FRAME_MS: u32 = 33;

/// 单帧时长上限 60 秒。
pub const MAX_FRAME_MS: u32 = 60_000;

/// 一帧的显示时长（毫秒）：给了 `fps` 就一律 1/fps；否则取文件自带的 `numer / denom` 毫秒，
/// 不足 10 ms 按 100 ms（浏览器惯例）。最后钳到 [`MIN_FRAME_MS`, `MAX_FRAME_MS`]。
pub fn frame_ms(numer: u32, denom: u32, fps: Option<f32>) -> u32 {
    let ms = match fps.filter(|f| f.is_finite() && *f > 0.0) {
        Some(f) => (1000.0 / f).round().min(MAX_FRAME_MS as f32) as u32,
        None => match numer.checked_div(denom) {
            Some(ms) if ms >= 10 => ms,
            _ => 100,
        },
    };
    ms.clamp(MIN_FRAME_MS, MAX_FRAME_MS)
}

/// 逐帧收集动画，合计字节超出预算就隔帧丢弃，被丢帧的时长并入前一帧（总时长不变）。
/// 流式进行：驻留不超过预算加一帧，注定被丢的帧也不必缩放。
pub struct FrameBudget<T> {
    frames: Vec<(T, u32)>,
    frame_bytes: usize,
    budget: usize,
    /// 每隔几帧留一帧（每丢一轮翻倍）。
    stride: usize,
    seen: usize,
    /// 有该留的帧没生成出来：之后的保留帧前移，原始帧号映射不再可靠。
    lost: bool,
}

impl<T> FrameBudget<T> {
    pub fn new(frame_bytes: usize, budget: usize) -> Self {
        Self {
            frames: Vec::new(),
            frame_bytes,
            budget,
            stride: 1,
            seen: 0,
            lost: false,
        }
    }

    /// 送入一帧（时长 `ms`）。只在这帧会被留下时才调 `make` 生成像素；`make` 返回 None 时按丢帧处理。
    pub fn push_with(&mut self, ms: u32, make: impl FnOnce() -> Option<T>) {
        let keep = self.seen.is_multiple_of(self.stride);
        self.seen += 1;
        let made = keep.then(make).flatten();
        self.lost |= keep && made.is_none();
        match (made, self.frames.last_mut()) {
            (Some(t), _) => self.frames.push((t, ms)),
            (None, Some(last)) => last.1 = last.1.saturating_add(ms),
            // 首帧就没生成出来：没有前一帧可并。
            (None, None) => {}
        }
        while self.frames.len() > 1 && self.frames.len() * self.frame_bytes > self.budget {
            self.halve();
        }
    }

    /// 隔帧丢一轮：奇数位的帧并入前一帧。
    fn halve(&mut self) {
        let mut out: Vec<(T, u32)> = Vec::with_capacity(self.frames.len().div_ceil(2));
        for (i, (t, ms)) in std::mem::take(&mut self.frames).into_iter().enumerate() {
            match out.last_mut() {
                Some(last) if i % 2 == 1 => last.1 = last.1.saturating_add(ms),
                _ => out.push((t, ms)),
            }
        }
        self.frames = out;
        self.stride *= 2;
    }

    /// 是否因预算丢过帧。
    pub fn dropped(&self) -> bool {
        self.stride > 1
    }

    /// 每隔几帧留一帧：原始第 `i` 帧落在保留下来的第 `i / stride` 帧里。
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// 送入的原始帧数。
    pub fn seen(&self) -> usize {
        self.seen
    }

    /// 保留帧与原始帧号是否还按 `i / stride` 对得上（有帧生成失败时对不上）。
    pub fn aligned(&self) -> bool {
        !self.lost
    }

    pub fn finish(self) -> Vec<(T, u32)> {
        self.frames
    }
}

/// 经过 `elapsed` 时该显示第几帧，以及距下一次换帧还有多久（单帧或空为 None）。
/// 按总时长取模，不逐帧累加，长时间播放也不漂移。
pub fn frame_at(
    durations: &[u32],
    elapsed: std::time::Duration,
) -> (usize, Option<std::time::Duration>) {
    let total: u128 = durations.iter().map(|&d| d as u128 * 1000).sum();
    if durations.len() < 2 || total == 0 {
        return (0, None);
    }
    let t = elapsed.as_micros() % total;
    let mut end = 0u128;
    for (i, &d) in durations.iter().enumerate() {
        end += d as u128 * 1000;
        if t < end {
            return (i, Some(std::time::Duration::from_micros((end - t) as u64)));
        }
    }
    (0, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_to_image_limits_keeps_aspect_within_bounds() {
        assert_eq!(clamp_to_image_limits(300.0, 40.0), (300, 40), "上限内原样");
        assert_eq!(clamp_to_image_limits(0.0, -5.0), (1, 1));
        for (w, h) in [
            (1e9, 10.0),
            (20000.0, 20000.0),
            (8192.0, 8192.0),
            (1e6, 1e6),
        ] {
            let (cw, ch) = clamp_to_image_limits(w, h);
            assert!(fits_image_limits(cw, ch), "{w}x{h} → {cw}x{ch}");
        }
        let (cw, ch) = clamp_to_image_limits(16384.0, 8192.0);
        assert!(
            (cw as f32 / ch as f32 - 2.0).abs() < 0.01,
            "{cw}x{ch} 保持 2:1"
        );
    }
    use wind_theme::skin::Band;

    fn bands(list: &[(f32, f32, BandFill)]) -> Stretch {
        Stretch::Bands(
            list.iter()
                .map(|&(from, to, fill)| Band { from, to, fill })
                .collect(),
        )
    }

    #[test]
    fn segments_alternate_and_cover_axis() {
        let s = src_segments(
            &bands(&[
                (10.0, 20.0, BandFill::Stretch),
                (50.0, 60.0, BandFill::Round),
            ]),
            100.0,
        );
        let spans: Vec<(f32, f32, bool)> =
            s.iter().map(|x| (x.from, x.to, x.fill.is_some())).collect();
        assert_eq!(
            spans,
            vec![
                (0.0, 10.0, false),
                (10.0, 20.0, true),
                (20.0, 50.0, false),
                (50.0, 60.0, true),
                (60.0, 100.0, false),
            ]
        );
    }

    #[test]
    fn min_len_is_fixed_sum_at_scale() {
        // 1800 宽，只有 300..500 可拉伸 → 最小宽 = 1600。
        let s = src_segments(&bands(&[(300.0, 500.0, BandFill::Stretch)]), 1800.0);
        assert_eq!(min_len(&s, 1800.0, 1800), 1600);
        // 1/3 倍率下按图集取整。
        assert_eq!(min_len(&s, 1800.0, atlas_len(1800, 1.0 / 3.0)), 533);
    }

    #[test]
    fn extra_split_by_band_length_and_sums_exactly() {
        let s = src_segments(
            &bands(&[
                (10.0, 20.0, BandFill::Stretch),
                (50.0, 80.0, BandFill::Stretch),
            ]),
            100.0,
        );
        let m = map_axis(&s, 100.0, 100, 160);
        assert_eq!(m.last().unwrap().d1, 160);
        // 固定段等长。
        for seg in m.iter().filter(|x| x.fill.is_none()) {
            assert_eq!(seg.d1 - seg.d0, seg.a1 - seg.a0);
        }
        // 固定 60，extra 100 按 10:30 分。
        let band_lens: Vec<u32> = m
            .iter()
            .filter(|x| x.fill.is_some())
            .map(|x| x.d1 - x.d0)
            .collect();
        assert_eq!(band_lens, vec![25, 75]);
    }

    #[test]
    fn below_min_bands_collapse_to_zero() {
        let s = src_segments(&bands(&[(10.0, 20.0, BandFill::Stretch)]), 100.0);
        let m = map_axis(&s, 100.0, 100, 50);
        assert_eq!(m[1].d1 - m[1].d0, 0, "拉伸带可缩到 0");
        assert_eq!(m.last().unwrap().d1, 90, "不低于固定段之和");
    }

    #[test]
    fn fixed_axis_grows_by_whole_stretch() {
        let s = src_segments(&Stretch::Fixed, 40.0);
        assert_eq!(map_axis(&s, 40.0, 40, 40).len(), 1);
        let m = map_axis(&s, 40.0, 40, 60);
        assert_eq!(
            (m[0].d0, m[0].d1, m[0].fill),
            (0, 60, Some(BandFill::Stretch))
        );
    }

    #[test]
    fn fixed_segments_copy_pixels_exactly() {
        let s = src_segments(&bands(&[(4.0, 6.0, BandFill::Stretch)]), 10.0);
        let t = taps(&map_axis(&s, 10.0, 10, 20));
        assert_eq!(t.len(), 20);
        for (k, tap) in t.iter().take(4).enumerate() {
            assert_eq!(*tap, (k as u32, k as u32, 0.0));
        }
        // 尾部固定段：目标 16..20 ← 图集 6..10。
        for k in 0..4 {
            assert_eq!(t[16 + k], (6 + k as u32, 6 + k as u32, 0.0));
        }
    }

    #[test]
    fn round_repeats_whole_units() {
        // 单元 10，目标 34 → 3 次（34/10 = 3.4 取整），每次单元 ≈ 11.33。
        let s = src_segments(&bands(&[(0.0, 10.0, BandFill::Round)]), 10.0);
        let t = taps(&map_axis(&s, 10.0, 10, 34));
        assert_eq!(t.len(), 34);
        // 每个单元的首像素都回到图集起点附近（不会从半个图案开始）。
        let starts: Vec<u32> = [0usize, 12, 23].iter().map(|&k| t[k].0).collect();
        assert!(starts.iter().all(|&i| i == 0), "单元起点 {starts:?}");
    }

    #[test]
    fn tile_crops_last_unit() {
        let s = src_segments(&bands(&[(0.0, 4.0, BandFill::Tile)]), 4.0);
        let t = taps(&map_axis(&s, 4.0, 4, 10));
        let idx: Vec<u32> = t.iter().map(|x| x.0).collect();
        assert_eq!(idx, vec![0, 1, 2, 3, 0, 1, 2, 3, 0, 1]);
    }

    #[test]
    fn taps_never_leave_their_segment() {
        let s = src_segments(&bands(&[(3.0, 5.0, BandFill::Stretch)]), 10.0);
        for (a0, a1) in [(3u32, 5u32)] {
            let t = taps(&map_axis(&s, 10.0, 10, 30));
            for &(i0, i1, _) in &t[3..23] {
                assert!((a0..a1).contains(&i0) && (a0..a1).contains(&i1));
            }
        }
    }

    #[test]
    fn downscale_is_area_average_premultiplied() {
        // 2×1：不透明红 + 全透明（其颜色是绿，不应渗进结果）→ 1×1 半透明红。
        let src = [255, 0, 0, 255, 0, 255, 0, 0];
        let out = resample(&src, 2, 1, 1, 1);
        assert_eq!(out, vec![128, 0, 0, 128]);
    }

    #[test]
    fn identity_resample_premultiplies_only() {
        let src = [200, 100, 50, 128];
        assert_eq!(resample(&src, 1, 1, 1, 1), vec![100, 50, 25, 128]);
    }

    #[test]
    fn compose_keeps_premultiplied_invariant() {
        let atlas = [255, 0, 0, 255, 0, 0, 0, 0];
        let xs = [(0, 1, 0.5)];
        let ys = [(0, 0, 0.0)];
        let out = compose(&atlas, 2, &xs, &ys);
        assert!(out[0] <= out[3]);
        assert_eq!(out[3], 128);
    }
    #[test]
    fn frame_ms_normalizes_short_and_missing_delays() {
        assert_eq!(frame_ms(80, 1, None), 80);
        assert_eq!(frame_ms(250, 2, None), 125, "比值换算成毫秒");
        assert_eq!(frame_ms(0, 1, None), 100, "0 ms 按 100 ms");
        assert_eq!(frame_ms(9, 1, None), 100, "不足 10 ms 按 100 ms");
        assert_eq!(frame_ms(10, 1, None), 33, "10 ms 不再改写，但受帧率上限");
        assert_eq!(frame_ms(5, 0, None), 100, "分母为 0 视作缺失");
    }

    #[test]
    fn frame_ms_fps_override_and_cap() {
        assert_eq!(frame_ms(500, 1, Some(10.0)), 100, "fps 覆盖文件时长");
        assert_eq!(frame_ms(0, 1, Some(24.0)), 42);
        assert_eq!(frame_ms(500, 1, Some(60.0)), MIN_FRAME_MS, "帧率上限 30");
        assert_eq!(frame_ms(20, 1, None), MIN_FRAME_MS);
        assert_eq!(frame_ms(70, 1, Some(0.0)), 70, "非正 fps 忽略");
    }

    fn budget_run(n: u32, frame_bytes: usize, budget: usize) -> (Vec<(u32, u32)>, bool, Vec<u32>) {
        let mut made = Vec::new();
        let mut b = FrameBudget::new(frame_bytes, budget);
        for i in 0..n {
            b.push_with(10 + i, || {
                made.push(i);
                Some(i)
            });
        }
        let dropped = b.dropped();
        (b.finish(), dropped, made)
    }

    #[test]
    fn frame_budget_keeps_everything_within_budget() {
        let (frames, dropped, _) = budget_run(4, 10, 40);
        assert!(!dropped);
        assert_eq!(frames, vec![(0, 10), (1, 11), (2, 12), (3, 13)]);
    }

    #[test]
    fn frame_budget_drops_every_other_frame_and_conserves_time() {
        // 7 帧、每帧 10 字节、预算 30：隔帧丢两轮，留 0、4 两帧。
        let (frames, dropped, made) = budget_run(7, 10, 30);
        assert!(dropped);
        assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![0, 4]);
        let total: u32 = frames.iter().map(|f| f.1).sum();
        assert_eq!(total, (0..7).map(|i| 10 + i).sum::<u32>(), "总时长守恒");
        assert_eq!(frames[0].1, 10 + 11 + 12 + 13, "被丢帧的时长并入前一帧");
        assert!(frames.len() * 10 <= 30);
        assert!(!made.contains(&5), "注定被丢的帧不生成像素");
    }

    #[test]
    fn frame_budget_keeps_one_frame_even_if_it_alone_exceeds() {
        let (frames, _, _) = budget_run(3, 100, 10);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], (0, 10 + 11 + 12));
    }

    #[test]
    fn frame_at_wraps_and_reports_time_to_next_frame() {
        use std::time::Duration;
        let d = [100, 50, 200];
        let ms = Duration::from_millis;
        assert_eq!(frame_at(&d, ms(0)), (0, Some(ms(100))));
        assert_eq!(frame_at(&d, ms(99)), (0, Some(ms(1))));
        assert_eq!(frame_at(&d, ms(100)), (1, Some(ms(50))), "边界属于下一帧");
        assert_eq!(frame_at(&d, ms(349)), (2, Some(ms(1))));
        assert_eq!(
            frame_at(&d, ms(350)),
            (0, Some(ms(100))),
            "总时长处回到首帧"
        );
        assert_eq!(
            frame_at(&d, ms(350 * 1000 + 120)),
            (1, Some(ms(30))),
            "按总时长取模"
        );
    }

    #[test]
    fn frame_at_single_frame_never_schedules() {
        use std::time::Duration;
        assert_eq!(frame_at(&[100], Duration::from_secs(5)), (0, None));
        assert_eq!(frame_at(&[], Duration::ZERO), (0, None));
    }
    #[test]
    fn frame_ms_caps_very_long_frames() {
        assert_eq!(frame_ms(0, 1, Some(0.001)), MAX_FRAME_MS, "极小 fps");
        assert_eq!(frame_ms(u32::MAX, 1, None), MAX_FRAME_MS, "单帧超长");
        assert_eq!(frame_ms(0, 1, Some(f32::MIN_POSITIVE)), MAX_FRAME_MS);
    }

    #[test]
    fn frame_budget_flags_lost_frames_as_misaligned() {
        let mut b = FrameBudget::new(10, 100);
        b.push_with(10, || Some(0));
        assert!(b.aligned());
        b.push_with(10, || None);
        b.push_with(10, || Some(2));
        assert!(!b.aligned(), "第 1 帧没生成，第 2 帧前移到保留下标 1");
    }

    #[test]
    fn frame_budget_merged_durations_saturate() {
        let mut b = FrameBudget::new(10, 10);
        b.push_with(u32::MAX - 5, || Some(0));
        b.push_with(100, || Some(1));
        b.push_with(100, || Some(2));
        assert_eq!(b.finish(), vec![(0, u32::MAX)]);
    }
}
