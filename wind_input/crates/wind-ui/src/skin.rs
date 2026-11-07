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

/// 某轴图集长度：源长 × 倍率，至少 1。
pub fn atlas_len(src_len: u32, k: f32) -> u32 {
    ((src_len as f32 * k).round() as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
