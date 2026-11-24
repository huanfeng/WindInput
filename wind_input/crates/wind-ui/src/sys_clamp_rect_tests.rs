use super::clamp_rect_in_bounds;

/// 两块屏：右屏 100%、左屏约 133%，工具栏纵向。
/// 右屏工作区右下 (2560, 1368)，纵条 48×212，用户拖到贴死右下角 → 存 (2512, 1156)。
/// 左屏工作区右下 (0, 1704)，同一条在该屏 DPI 下是 64×282。
const RIGHT_SCREEN: (i32, i32, i32, i32) = (0, 0, 2560, 1368);
const FLUSH_POS: (i32, i32) = (2512, 1156);
const SIZE_ON_RIGHT: (u32, u32) = (48, 212);
const SIZE_ON_LEFT: (u32, u32) = (64, 282);

/// 用目标屏的尺寸钳贴边位置：恰好不越界，原样保留。
/// 边界是闭合的 —— `x + w == right` 不算越界，否则贴边永远保不住。
#[test]
fn flush_corner_survives_clamp_with_correct_size() {
    let got = clamp_rect_in_bounds(
        FLUSH_POS.0,
        FLUSH_POS.1,
        SIZE_ON_RIGHT.0,
        SIZE_ON_RIGHT.1,
        RIGHT_SCREEN,
    );
    assert_eq!(got, FLUSH_POS, "贴边位置不该被钳走");
}

/// 用上一块屏的尺寸钳，会把贴边位置永久拉离边缘：`2512+64 > 2560` → 2496，
/// `1156+282 > 1368` → 1086。结果写回记忆位置后用正确尺寸重钳也回不来，故调用方必须
/// 保证尺寸属于目标屏。
#[test]
fn stale_size_destroys_a_flush_corner_position() {
    let damaged = clamp_rect_in_bounds(
        FLUSH_POS.0,
        FLUSH_POS.1,
        SIZE_ON_LEFT.0,
        SIZE_ON_LEFT.1,
        RIGHT_SCREEN,
    );
    assert_eq!(damaged, (2496, 1086));

    // 不可逆：拿正确尺寸再钳一次也救不回来。
    let rechecked = clamp_rect_in_bounds(
        damaged.0,
        damaged.1,
        SIZE_ON_RIGHT.0,
        SIZE_ON_RIGHT.1,
        RIGHT_SCREEN,
    );
    assert_eq!(rechecked, damaged, "钳制有损，第二次钳不会复原");
    assert_ne!(rechecked, FLUSH_POS);
}

/// 左屏工作区右边界为 0（屏在主屏左侧，坐标为负）：贴边位置同样必须保住。
#[test]
fn flush_corner_survives_on_negative_coordinate_monitor() {
    let left_screen = (-2048, 0, 0, 1704);
    let got = clamp_rect_in_bounds(-64, 1422, SIZE_ON_LEFT.0, SIZE_ON_LEFT.1, left_screen);
    assert_eq!(got, (-64, 1422));
}
