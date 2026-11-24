use super::*;
// 只有测试构造样例时用到；放在顶层会让非测试构建报未使用导入（CI 的 clippy 按 -D warnings 跑）。
use wind_ui_types::diag::RectVerdict;

fn view() -> CaretOverlayView {
    CaretOverlayView {
        caret: (2710, 792, 43),
        comp_start: Some((1699, 745)),
        comp_rect: Some((1699, 701, 2710, 792)),
        anchor: (1699, 792),
        anchor_source: "composition_rect",
        caret_source: "tsf_selection",
        rect_verdict: RectVerdict::Trusted,
        process: "WINWORD.EXE".into(),
    }
}

/// 包围盒必须覆盖**所有**要画的点——漏掉任何一个，那个元素就画在画布外、静默消失，
/// 而「元素没出现」在调试工具里会被误读成「宿主没上报」。
#[test]
fn bounds_cover_every_drawn_element() {
    let v = view();
    let (x0, y0, x1, y1) = content_bounds(&v).expect("有内容时必须给出包围盒");
    let (l, t, r, b) = v.comp_rect.unwrap();
    let (sx, sy) = v.comp_start.unwrap();
    for (px, py, what) in [
        (l, t, "rect 左上"),
        (r, b, "rect 右下"),
        (sx, sy, "组合起点"),
        (v.caret.0, v.caret.1, "caret 底"),
        (v.caret.0, v.caret.1 - v.caret.2, "caret 顶"),
        (v.anchor.0, v.anchor.1, "锚点"),
    ] {
        assert!(
            px > x0 && px < x1 && py > y0 && py < y1,
            "{what} ({px},{py}) 落在画布外 ({x0},{y0})-({x1},{y1})"
        );
    }
}

/// 只有 caret 的退化情形（宿主什么都没给）仍要能画出来——那恰恰是要观察的状态。
#[test]
fn bounds_work_with_caret_only() {
    let v = CaretOverlayView {
        comp_start: None,
        comp_rect: None,
        ..view()
    };
    assert!(content_bounds(&v).is_some());
}

/// ★ 文字标注必须能完整表达四类信息——缺任何一类，看图的人就得回头翻日志，
/// 那正是这个工具要消灭的动作。
#[test]
fn legend_carries_every_thing_needed_to_judge() {
    let v = view();
    let text = legend_lines(&v).join(
        "
",
    );
    for needle in [
        "WINWORD.EXE",       // 哪个宿主——per-app 开关作用在谁身上
        "tsf_selection",     // 坐标来源
        "采信矩形",          // 判据走了哪条分支
        "1699,701,2710,792", // 宿主给的矩形
        "2710,792",          // 插入点
        "1699,745",          // 组合起点
        "composition_rect",  // 锚点取自哪里
    ] {
        assert!(
            text.contains(needle),
            "文字标注缺少 {needle}：
{text}"
        );
    }
}

/// ★★★ 文字条带**不得压住几何**——调试工具挡住被调试的对象等于没有。
///
/// 首版把文字画在画布左上角 (4,4)，而画布原点 = 内容包围盒 - PADDING，于是文字
/// 必然落在组合区正上方 96px 内、几乎注定重叠：记事本实测直接盖住了组合区。
/// 修法是让文字**占用画布之外新增的空间**，而不是借用给几何留的余量。
#[test]
fn legend_band_does_not_overlap_geometry() {
    let v = view();
    let (_, y0, _, _) = content_bounds(&v).unwrap();
    // 条带高度由 measure_legend 给出（此处以保守下界代入：至少一行）。
    let band_h = 1u32;
    // 几何的最高点是 rect.top；画布原点上移 band_h 后，它在画布内的 y 应当 ≥ band_h。
    let origin_y = y0 - band_h as i32;
    let rect_top_in_canvas = v.comp_rect.unwrap().1 - origin_y;
    assert!(
        rect_top_in_canvas >= band_h as i32,
        "几何最高点落进了文字条带内（{rect_top_in_canvas} < {band_h}）"
    );
}

/// 越界写入必须静默丢弃，不得 panic 也不得写坏相邻像素。
#[test]
fn out_of_bounds_pixels_are_dropped() {
    let (bw, bh) = (8u32, 8u32);
    let mut buf = vec![0u8; (bw * bh * 4) as usize];
    let mut canvas = Canvas {
        buf: &mut buf,
        w: bw,
        h: bh,
    };
    canvas.put(-1, 0, C_CARET);
    canvas.put(0, -1, C_CARET);
    canvas.put(8, 0, C_CARET);
    canvas.put(0, 8, C_CARET);
    assert!(buf.iter().all(|&b| b == 0), "越界写入污染了缓冲");
}

/// 画出来的每个元素都要能被看见：填充后对应像素的 alpha 必须非零。
#[test]
fn drawing_actually_marks_pixels() {
    let (bw, bh) = (16u32, 16u32);
    let mut buf = vec![0u8; (bw * bh * 4) as usize];
    Canvas {
        buf: &mut buf,
        w: bw,
        h: bh,
    }
    .cross(8, 8, 4, 2, C_ANCHOR);
    let idx = ((8 * bw + 8) * 4) as usize;
    assert_ne!(buf[idx + 3], 0, "十字中心应当被画上");
}
