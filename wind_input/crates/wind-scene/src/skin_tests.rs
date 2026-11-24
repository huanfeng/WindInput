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
    let spans: Vec<(f32, f32, bool)> = s.iter().map(|x| (x.from, x.to, x.fill.is_some())).collect();
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
