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
    // 横向：逐行预乘成 f32 再滤波（不物化整张 f32 源图：大图上它是源图字节的 4 倍）。
    let wx = weights(sw, dw);
    let mut mid = vec![[0f32; 4]; (dw * sh) as usize];
    let mut row = vec![[0f32; 4]; sw as usize];
    for y in 0..sh as usize {
        let src_row = &src[y * sw as usize * 4..(y + 1) * sw as usize * 4];
        for (d, p) in row.iter_mut().zip(src_row.as_chunks::<4>().0) {
            let a = p[3] as f32 / 255.0;
            *d = [
                p[0] as f32 * a,
                p[1] as f32 * a,
                p[2] as f32 * a,
                p[3] as f32,
            ];
        }
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
#[path = "skin_tests.rs"]
mod tests;
