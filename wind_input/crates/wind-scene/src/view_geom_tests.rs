use super::*;

#[test]
fn rect_contains_includes_edges() {
    let r = Rect {
        x: 10.0,
        y: 10.0,
        w: 20.0,
        h: 20.0,
    };
    assert!(r.contains(10.0, 10.0)); // 左上角
    assert!(r.contains(30.0, 30.0)); // 右下角（x+w, y+h 含边界）
    assert!(r.contains(20.0, 20.0)); // 内部
    assert!(!r.contains(9.9, 20.0)); // 左外
    assert!(!r.contains(30.1, 20.0)); // 右外
    assert!(!r.contains(20.0, 9.9)); // 上外
}

#[test]
fn edges_helpers() {
    let a = Edges::all(5.0);
    assert_eq!((a.l, a.t, a.r, a.b), (5.0, 5.0, 5.0, 5.0));
    assert_eq!(a.w(), 10.0);
    assert_eq!(a.h(), 10.0);
    let xy = Edges::xy(3.0, 4.0);
    assert_eq!((xy.l, xy.t, xy.r, xy.b), (3.0, 4.0, 3.0, 4.0));
    assert_eq!(xy.w(), 6.0);
    assert_eq!(xy.h(), 8.0);
}

#[test]
fn fill_rounded_writes_alpha() {
    let mut buf = vec![0u8; 10 * 10 * 4];
    fill_rounded(
        &mut buf,
        10,
        10,
        0.0,
        0.0,
        10.0,
        10.0,
        [255, 0, 0, 255],
        0.0,
    );
    let center = (5 * 10 + 5) * 4;
    assert!(buf[center + 3] > 0, "中心像素 alpha 应被写入");
}

// ─────────────────── 像素对齐：相邻盒子不留缝也不重叠 ───────────────────

/// 行间距为 0 时相邻候选行之间不得有缝。非整数倍 DPI 下行几何全带小数，「位置 round、
/// 尺寸也 round」会让同一条公共边界两边算出不同整数，时而空 1px、时而叠 1px。
/// 这里画三行真实数值，逐行扫 alpha，从第一行顶到最后一行底不允许有空行。
#[test]
fn adjacent_rows_leave_no_gap_at_fractional_scale() {
    // scale = 1.5 下候选窗实测的行几何（见 candidate_window 的行距探针）
    let rows = [(9.0f32, 53.4f32), (62.4, 53.4), (115.8, 53.4)];
    let (w, h) = (24u32, 200u32);
    let mut buf = vec![0u8; (w * h) as usize * 4];
    for (y, rh) in rows {
        fill_rounded(&mut buf, w, h, 0.0, y, w as f32, rh, [255, 0, 0, 255], 0.0);
    }
    let alpha_at = |row: u32| buf[((row * w + w / 2) * 4 + 3) as usize];
    let top = rows[0].0.round() as u32;
    let bottom = (rows[2].0 + rows[2].1).round() as u32;
    let empty: Vec<u32> = (top..bottom).filter(|r| alpha_at(*r) == 0).collect();
    assert!(
        empty.is_empty(),
        "第 {top}~{bottom} 行之间不该有空行，实际空在 {empty:?}"
    );
    // 前置：确实画了东西，且范围之外没有越界涂抹（否则上面的「无空行」可能是整片糊了）
    assert!(alpha_at(top) > 0, "前置：首行须被填上");
    assert_eq!(alpha_at(top.saturating_sub(2)), 0, "首行之上不该被涂到");
    assert_eq!(alpha_at(bottom + 2), 0, "末行之下不该被涂到");
}

/// 对照：整数倍缩放下行为不变。
#[test]
fn integer_scale_rows_are_unchanged() {
    let (w, h) = (24u32, 200u32);
    let mut buf = vec![0u8; (w * h) as usize * 4];
    for (y, rh) in [(6.0f32, 36.0f32), (42.0, 36.0), (78.0, 36.0)] {
        fill_rounded(&mut buf, w, h, 0.0, y, w as f32, rh, [255, 0, 0, 255], 0.0);
    }
    let alpha_at = |row: u32| buf[((row * w + w / 2) * 4 + 3) as usize];
    assert!((6..114).all(|r| alpha_at(r) > 0), "6~114 行须连续填满");
    assert_eq!(alpha_at(5), 0, "盒子上沿之外不该被涂到");
    assert_eq!(alpha_at(114), 0, "盒子下沿之外不该被涂到");
}

/// 边界取整本身：公共边界在两边必须解析成同一个整数。
///
/// 直接测 `snap_to_pixels` 而不只测像素，是因为像素那条测不出「重叠」——
/// 多画一层 alpha 仍是非 0，扫描看不出来，而重叠在半透明背景下是看得见的深一道。
#[test]
fn snap_shares_the_boundary_between_neighbours() {
    for (y0, h0, y1) in [
        (9.0f32, 53.4f32, 62.4f32),
        (7.5, 44.5, 52.0),
        (0.3, 10.4, 10.7),
    ] {
        let (_, ya, _, ha) = snap_to_pixels(0.0, y0, 1.0, h0);
        let (_, yb, _, _) = snap_to_pixels(0.0, y1, 1.0, h0);
        assert_eq!(
            ya + ha,
            yb,
            "上一个盒子的下边界须正好落在下一个的上边界（y0={y0} h0={h0} y1={y1}）"
        );
    }
}

/// 边界取整的另一半：整数尺寸必须与落点无关，插入符、分隔线等「恒 n 像素粗」的元素靠它。
/// 非整数尺寸会随落点变（相邻平铺无缝的代价），故这类尺寸必须在源头取整（[`View::caret_at`]）。
#[test]
fn snap_keeps_integer_sizes_at_any_subpixel_offset() {
    for n in [1.0f32, 2.0, 3.0] {
        for x in [0.0f32, 0.1, 0.4, 0.5, 0.6, 0.9, 10.3, 10.7] {
            let (_, _, w, _) = snap_to_pixels(x, 0.0, n, 1.0);
            assert_eq!(w, n, "整数尺寸 {n} 在 x={x} 处不该变成 {w}");
        }
    }
    // 反面对照：非整数尺寸随落点变 —— 正是插入符必须在源头取整的理由。
    let (_, _, w_lo, _) = snap_to_pixels(10.0, 0.0, 1.5, 1.0);
    let (_, _, w_hi, _) = snap_to_pixels(10.6, 0.0, 1.5, 1.0);
    assert_ne!(
        w_lo, w_hi,
        "1.5 宽在不同落点该给出不同结果，否则本测试的前提不成立"
    );
}

/// 插入符宽度在 `caret_at` 就取整，绘制端拿不到半像素。
///
/// 1.5 是 1.5 倍缩放下 `scale.max(1.0)` 的真实取值；不取整的话竖线会一边打字
/// 一边在 1px / 2px 之间闪（插入符的 x 随前半段文本宽度逐字移动）。
#[test]
fn caret_width_snaps_to_whole_pixels() {
    for (given, want) in [(1.0f32, 1.0f32), (1.25, 1.0), (1.5, 2.0), (2.0, 2.0)] {
        let v = View::leaf("ab".to_string(), [0, 0, 0, 255]).caret_at(1, given);
        assert_eq!(v.caret_w, want, "caret_w({given}) 应取整到 {want}");
    }
    // 缩放小于 1 时仍须看得见。
    let v = View::leaf("ab".to_string(), [0, 0, 0, 255]).caret_at(1, 0.4);
    assert_eq!(v.caret_w, 1.0, "亚像素缩放下插入符不该消失");
}

#[test]
fn fill_rounded_transparent_color_is_noop() {
    let mut buf = vec![0u8; 4 * 4 * 4];
    fill_rounded(&mut buf, 4, 4, 0.0, 0.0, 4.0, 4.0, [255, 0, 0, 0], 0.0);
    assert!(buf.iter().all(|&b| b == 0), "alpha=0 不应写入任何像素");
}

#[test]
fn fill_circle_writes_center_not_corner() {
    let mut buf = vec![0u8; 20 * 20 * 4];
    fill_circle(&mut buf, 20, 20, 10.0, 10.0, 8.0, [0, 255, 0, 255]);
    assert!(px_alpha(&buf, 20, 10, 10) > 0, "圆心应被填充");
    assert_eq!(px_alpha(&buf, 20, 0, 0), 0, "角落在圆外，不应被填充");
}

#[test]
fn gradient_linear_writes_pixels() {
    let mut buf = vec![0u8; 16 * 16 * 4];
    let g = ViewGradient {
        radial: false,
        angle: 0.0,
        stops: vec![([255, 0, 0, 255], 0.0), ([0, 0, 255, 255], 1.0)],
    };
    let r = Rect {
        x: 0.0,
        y: 0.0,
        w: 16.0,
        h: 16.0,
    };
    paint_bg_gradient(&mut buf, 16, 16, r, 0.0, &g);
    assert!(px_alpha(&buf, 16, 8, 8) > 0, "渐变中心 alpha 应被写入");
}

#[test]
fn gradient_single_stop_falls_back_to_solid() {
    let mut buf = vec![0u8; 8 * 8 * 4];
    let g = ViewGradient {
        radial: false,
        angle: 0.0,
        stops: vec![([0, 255, 0, 255], 0.5)],
    };
    let r = Rect {
        x: 0.0,
        y: 0.0,
        w: 8.0,
        h: 8.0,
    };
    paint_bg_gradient(&mut buf, 8, 8, r, 0.0, &g);
    assert!(px_alpha(&buf, 8, 4, 4) > 0, "单停靠点退化为纯色填充");
}

#[test]
fn fill_ring_hollow_center() {
    let mut buf = vec![0u8; 20 * 20 * 4];
    fill_ring(
        &mut buf,
        20,
        20,
        0.0,
        0.0,
        20.0,
        20.0,
        [0, 0, 255, 255],
        0.0,
        2.0,
    );
    // 边框环：靠边像素被描边，正中心（挖空）保持透明
    assert!(px_alpha(&buf, 20, 10, 0) > 0, "上边框应被描边");
    assert_eq!(px_alpha(&buf, 20, 10, 10), 0, "环中心应镂空透明");
}

// ── 左侧强调条：height_ratio / offset ──────────────────────────────────
// 用无文本的固定尺寸容器，几何跨平台确定（不依赖 DirectWrite / mock 测量差异）。

/// 在 40×40 缓冲里画一个 40×40 的容器，带指定强调条参数；返回 BGRA 缓冲。
fn paint_left_bar(height_ratio: f32, offset: f32, width: f32) -> Vec<u8> {
    let tr = crate::text::MockText::new("test", 20.0).unwrap();
    let mut v = View::container(Layout::Row)
        .fixed_w(40.0)
        .fixed_h(40.0)
        .left_bar(LeftBar {
            color: [255, 0, 0, 255],
            width,
            height_ratio,
            offset,
        });
    v.layout(0.0, 0.0, &tr);
    let mut buf = vec![0u8; 40 * 40 * 4];
    v.paint(&mut buf, 40, 40, &tr);
    buf
}

/// BGRA 缓冲里 (x, y) 像素的 alpha 分量。`stride` 为缓冲宽度（像素）。
/// 写成函数而非手写 `(y * w + x) * 4 + 3`：坐标语义更清楚，也避开 clippy
/// 对 `0 * 20 + 0` 这类「保留坐标形状」写法的 erasing_op/identity_op 报错。
fn px_alpha(buf: &[u8], stride: usize, x: usize, y: usize) -> u8 {
    buf[(y * stride + x) * 4 + 3]
}

/// 40×40 缓冲专用的 [`px_alpha`]。
fn alpha_at(buf: &[u8], x: usize, y: usize) -> u8 {
    px_alpha(buf, 40, x, y)
}

#[test]
fn left_bar_height_ratio_controls_bar_height() {
    // ratio=1.0：条高 = 行高，顶部与中部都被填充。
    let full = paint_left_bar(1.0, 0.0, 4.0);
    assert!(alpha_at(&full, 1, 1) > 0, "ratio=1.0 顶部应被填充");
    assert!(alpha_at(&full, 1, 20) > 0, "ratio=1.0 中部应被填充");

    // ratio=0.5：条高 = 行高一半、垂直居中 → 中部填充、顶部留白。
    let half = paint_left_bar(0.5, 0.0, 4.0);
    assert!(alpha_at(&half, 1, 20) > 0, "ratio=0.5 中部应被填充");
    assert_eq!(alpha_at(&half, 1, 1), 0, "ratio=0.5 顶部应留白");
}

#[test]
fn left_bar_offset_shifts_bar_right() {
    // offset=0：条贴左缘。
    let flush = paint_left_bar(1.0, 0.0, 4.0);
    assert!(alpha_at(&flush, 1, 20) > 0, "offset=0 左缘应被填充");

    // offset=10：条右移 10px → 左缘留白，10px 之后才是条。
    let shifted = paint_left_bar(1.0, 10.0, 4.0);
    assert_eq!(alpha_at(&shifted, 1, 20), 0, "offset=10 左缘应留白");
    assert!(alpha_at(&shifted, 11, 20) > 0, "offset=10 处应被填充");
}

/// 极小 ratio 仍保底 2px——否则主题写个 0.01 会让条彻底消失。
#[test]
fn left_bar_tiny_ratio_clamped_to_min_height() {
    let tiny = paint_left_bar(0.001, 0.0, 4.0);
    assert!(alpha_at(&tiny, 1, 20) > 0, "极小比例仍应保底 2px 可见");
}
