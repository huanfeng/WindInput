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
        flip: wind_theme::Flip::NONE,
        fps: None,
        budget: 0,
        sources: vec![DecorSource {
            path: String::new(),
            k: 1.0,
            animated: false,
            raw_stride: Some(1),
        }],
        base: ClipTl::whole(0, None),
        states: Vec::new(),
        on: Vec::new(),
        cur: (0, 0),
        still: (0, 0),
        pose: (crate::skin_anim::ClipId::Base, 0),
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

/// 输入区退化形（§17.2）：窗体 + block / drag 装饰，穿透装饰与零面积的不进。
#[test]
fn coarse_input_rects_leave_out_pass_decor() {
    let with = |hit, index| DecorGeom {
        hit,
        index,
        ..decor("frame.top_left", Pos9::TOP_LEFT, (0.0, 0.0), 8, 8)
    };
    let g = SkinGeom {
        variant_key: (false, false),
        variant: "horizontal",
        frame_ref: String::new(),
        band_f: 1.0,
        render_scale: 1.0,
        frame_path: String::new(),
        frame_k: 1.0,
        sx: Stretch::Fixed,
        sy: Stretch::Fixed,
        min_w: 0.0,
        min_h: 0.0,
        insets: Edges::default(),
        placement: Default::default(),
        slots: None,
        parts: Vec::new(),
        decor: vec![
            with(DecorHit::Block, 0),
            with(DecorHit::Pass, 1),
            with(DecorHit::Drag, 2),
            with(DecorHit::Block, 3),
        ],
        frame: (24, 24, 60, 40),
        decor_rects: vec![
            (0, 0, 24, 24),
            (84, 0, 24, 24),
            (84, 64, 8, 8),
            (0, 64, 0, 8),
        ],
        margins: (24, 24, 24, 24),
        k: 1.0,
    };
    assert_eq!(
        g.coarse_input_rects(),
        vec![[24, 24, 60, 40], [0, 0, 24, 24], [84, 64, 8, 8]]
    );
}

#[test]
fn raw_stride_maps_kept_frames_back_unless_misaligned() {
    use crate::image_cache::AnimLayout;
    let l = |stride, aligned| AnimLayout {
        durations: vec![100; 3],
        stride,
        raw_frames: 6,
        aligned,
    };
    assert_eq!(raw_stride(None), Some(1), "静态图");
    assert_eq!(
        raw_stride(Some(&l(2, true))),
        Some(2),
        "隔帧丢弃后保留帧 × 2"
    );
    assert_eq!(raw_stride(Some(&l(0, true))), Some(1));
    assert_eq!(raw_stride(Some(&l(2, false))), None, "有帧生成失败：对不上");
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
