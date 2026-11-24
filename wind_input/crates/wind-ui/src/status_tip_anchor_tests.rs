//! 锚点几何（C2-33 / GH#148）：参照矩形 → 气泡左上角，含边距与夹回。
use super::{anchor_origin, place_anchor};
use wind_ui_types::StatusTipAnchor as A;

/// 主屏工作区（任务栏在底部，40px）。
const WORK: (i32, i32, i32, i32) = (0, 0, 2560, 1400);
/// 左侧副屏：整块负坐标。
const LEFT_WORK: (i32, i32, i32, i32) = (-1920, 0, 0, 1040);
const TIP: (u32, u32) = (120, 34);
const M: i32 = 16;

#[test]
fn screen_anchors_on_work_area() {
    assert_eq!(anchor_origin(WORK, TIP, M, A::ScreenCenter), (1220, 683));
    assert_eq!(anchor_origin(WORK, TIP, M, A::ScreenTopLeft), (16, 16));
    assert_eq!(
        anchor_origin(WORK, TIP, M, A::ScreenTopRight),
        (2560 - 16 - 120, 16)
    );
    assert_eq!(
        anchor_origin(WORK, TIP, M, A::ScreenBottomLeft),
        (16, 1400 - 16 - 34)
    );
    assert_eq!(
        anchor_origin(WORK, TIP, M, A::ScreenBottomRight),
        (2560 - 16 - 120, 1400 - 16 - 34)
    );
}

/// 窗口锚点用窗口边框作参照，而不是工作区。
#[test]
fn window_anchors_use_frame() {
    let frame = (400, 300, 1400, 900);
    assert_eq!(
        place_anchor(A::WindowCenter, WORK, Some(frame), TIP, M),
        (900 - 60, 600 - 17)
    );
    assert_eq!(
        place_anchor(A::WindowBottomLeft, WORK, Some(frame), TIP, M),
        (416, 900 - 16 - 34)
    );
}

/// 屏幕锚点不看窗口边框——即便给了也不用。
#[test]
fn screen_anchor_ignores_frame() {
    let frame = (400, 300, 1400, 900);
    assert_eq!(
        place_anchor(A::ScreenTopLeft, WORK, Some(frame), TIP, M),
        (16, 16)
    );
}

/// 边距参与计算：边距为 0 时贴边。
#[test]
fn margin_is_applied() {
    assert_eq!(anchor_origin(WORK, TIP, 0, A::ScreenTopLeft), (0, 0));
    assert_eq!(
        anchor_origin(WORK, TIP, 0, A::ScreenBottomRight),
        (2560 - 120, 1400 - 34)
    );
    assert_ne!(
        anchor_origin(WORK, TIP, 0, A::ScreenBottomRight),
        anchor_origin(WORK, TIP, M, A::ScreenBottomRight)
    );
}

/// 拿不到窗口边框 ⇒ 降级为同位置的屏幕锚点。
#[test]
fn window_anchor_without_frame_degrades_to_screen() {
    assert_eq!(
        place_anchor(A::WindowCenter, WORK, None, TIP, M),
        anchor_origin(WORK, TIP, M, A::ScreenCenter)
    );
    assert_eq!(
        place_anchor(A::WindowBottomLeft, WORK, None, TIP, M),
        anchor_origin(WORK, TIP, M, A::ScreenBottomLeft)
    );
}

/// 窗口一半拖出屏幕：锚点落在屏外的部分夹回工作区。
#[test]
fn window_anchor_outside_work_area_is_clamped() {
    let frame = (-600, 1200, 400, 1800);
    let (x, y) = place_anchor(A::WindowBottomLeft, WORK, Some(frame), TIP, M);
    assert_eq!((x, y), (0, 1400 - 34), "左、下越界都应夹回工作区内");
}

/// 副屏负坐标原样保留，不被拉回主屏（同 place_tests 的同名性质）。
#[test]
fn negative_work_area_survives() {
    assert_eq!(
        place_anchor(A::ScreenTopLeft, LEFT_WORK, None, TIP, M),
        (-1920 + 16, 16)
    );
    assert_eq!(
        place_anchor(A::ScreenBottomRight, LEFT_WORK, None, TIP, M),
        (-16 - 120, 1040 - 16 - 34)
    );
}
