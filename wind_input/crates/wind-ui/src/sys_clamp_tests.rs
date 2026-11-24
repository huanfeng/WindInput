use super::clamp_content_in_bounds;

/// 1920×1080 主屏，_qingfeng 主题量级的阴影：内容 420×44，四周扩边 (29,29,29,33)，
/// 故窗口为 478×106。
const MARGIN: (i32, i32, i32, i32) = (29, 29, 29, 33);
const CONTENT: (i32, i32) = (420, 44);
const SCREEN: (i32, i32, i32, i32) = (0, 0, 1920, 1080);

/// 内容贴左边缘时，窗口左上必须为负（阴影溢出屏幕），否则内容会被卡在离屏幕边 29px 处。
#[test]
fn content_can_touch_screen_edge_with_shadow_overflowing() {
    // 试图拖到窗口左上为 (-200,-200)（远超边界）
    let (wx, wy) = clamp_content_in_bounds(-200, -200, MARGIN, CONTENT, SCREEN);
    assert_eq!(
        (wx + MARGIN.0, wy + MARGIN.1),
        (0, 0),
        "内容左上应恰好贴屏幕左上角"
    );
    assert_eq!((wx, wy), (-29, -29), "窗口左上为负 = 阴影溢出屏幕");
}

/// 内容贴右/下边缘：内容右边 = 1920、下边 = 1080（含任务栏区域）。
#[test]
fn content_can_touch_bottom_right_including_taskbar_area() {
    let (wx, wy) = clamp_content_in_bounds(9999, 9999, MARGIN, CONTENT, SCREEN);
    let (cx, cy) = (wx + MARGIN.0, wy + MARGIN.1);
    assert_eq!(
        (cx + CONTENT.0, cy + CONTENT.1),
        (1920, 1080),
        "内容右下贴屏幕右下"
    );
}

/// 未越界时原样返回，不得有任何偏移。
#[test]
fn in_bounds_position_is_untouched() {
    assert_eq!(
        clamp_content_in_bounds(500, 400, MARGIN, CONTENT, SCREEN),
        (500, 400)
    );
}

/// 副屏在主屏左侧（负坐标空间）同样成立。
#[test]
fn works_on_a_monitor_at_negative_coordinates() {
    let left_screen = (-1920, 0, 0, 1080);
    let (wx, wy) = clamp_content_in_bounds(-9999, 500, MARGIN, CONTENT, left_screen);
    assert_eq!(wx + MARGIN.0, -1920, "内容左边贴副屏左边缘");
    assert_eq!(wy, 500, "垂直方向未越界，保持不动");
}

/// 内容比可用区域还大时，优先保证左上角可见（否则窗口会被推到区域外）。
/// 取 300×30，使 420×44 的内容在两个方向都放不下。
#[test]
fn oversized_content_keeps_top_left_visible() {
    let tiny = (0, 0, 300, 30);
    let (wx, wy) = clamp_content_in_bounds(100, 100, MARGIN, CONTENT, tiny);
    assert_eq!((wx + MARGIN.0, wy + MARGIN.1), (0, 0));
}

/// 只有一个方向放不下时，另一个方向必须保持不动——不能被"顺手"一起拉到边上。
#[test]
fn only_the_overflowing_axis_is_adjusted() {
    let narrow = (0, 0, 300, 1080); // 宽度放不下，高度绰绰有余
    let (wx, wy) = clamp_content_in_bounds(100, 400, MARGIN, CONTENT, narrow);
    assert_eq!(wx + MARGIN.0, 0, "水平超宽 → 贴左");
    assert_eq!(wy, 400, "垂直未越界 → 原样不动");
}
