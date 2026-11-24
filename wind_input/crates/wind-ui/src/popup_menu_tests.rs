use super::*;

// 1080p 典型工作区（底部留 40px 任务栏）。
const WA: Option<(i32, i32, i32, i32)> = Some((0, 0, 1920, 1040));
// 菜单典型尺寸。
const MW: u32 = 200;
const MH: u32 = 300;
/// scale=1.0 下的侧向间隙（设备像素），与 `SIDE_GAP_DP` 同源。
const GAP: i32 = SIDE_GAP_DP as i32;

/// 光标处右键：顶边贴锚点，原样落点。
#[test]
fn below_places_menu_at_anchor() {
    let a = MenuAnchor::at_point(500, 400);
    assert_eq!(place_menu(&a, 500, 400, MW, MH, WA, 1.0), (500, 400));
}

/// 横条工具栏（屏幕右下角）：菜单底边贴条顶边向上展开。
#[test]
fn above_expands_upward_from_anchor_top() {
    let a = MenuAnchor::above_rect(1700, 900, 930);
    assert_eq!(place_menu(&a, 1700, 900, MW, MH, WA, 1.0), (1700, 600));
}

/// 横条贴在屏幕顶部：上方装不下 → 翻到条底边之下，而不是钻出屏幕。
#[test]
fn above_flips_below_when_no_room_up() {
    let a = MenuAnchor::above_rect(100, 10, 40);
    assert_eq!(place_menu(&a, 100, 10, MW, MH, WA, 1.0), (100, 40));
}

/// 纵条在屏幕左侧：右边装得下 → 菜单贴右边缘展开，纵坐标对齐条顶。
#[test]
fn side_prefers_right_when_it_fits() {
    // 条占 y[500,700]、右缘 50；菜单高 300 → 底边对齐 700 得顶边 400，左缘 50+GAP。
    let a = MenuAnchor::beside_rect(20, 500, 50, 700);
    assert_eq!(place_menu(&a, 20, 500, MW, MH, WA, 1.0), (50 + GAP, 400));
}

/// 纵条在屏幕右下角（默认落点）：右边放不下 → 翻到左侧，菜单右缘距条左缘一个间隙。
#[test]
fn side_falls_back_to_left_when_right_is_tight() {
    let a = MenuAnchor::beside_rect(1870, 600, 1900, 800);
    assert_eq!(
        place_menu(&a, 1870, 600, MW, MH, WA, 1.0),
        (1870 - GAP - MW as i32, 500)
    );
}

/// 两侧都装不下（窄屏/超宽菜单）：仍取右侧。左侧会算出负坐标，钳回来后正好压在
/// 工具栏上——那恰是侧向弹出要避免的事；贴右边缘至少留着工具栏可见。
#[test]
fn side_keeps_right_when_neither_side_fits() {
    let narrow = Some((0, 0, 150, 1040));
    let a = MenuAnchor::beside_rect(100, 300, 130, 500);
    assert_eq!(
        place_menu(&a, 100, 300, MW, MH, narrow, 1.0),
        (130 + GAP, 200)
    );
}

/// 取不到工作区时按「首选方向装得下」处理：让首选生效、越界交给钳制，
/// 而不是凭空翻转到另一侧。
#[test]
fn missing_work_area_keeps_preferred_direction() {
    let up = MenuAnchor::above_rect(100, 500, 530);
    assert_eq!(place_menu(&up, 100, 500, MW, MH, None, 1.0), (100, 200));
    let side = MenuAnchor::beside_rect(100, 500, 130, 700);
    assert_eq!(
        place_menu(&side, 100, 500, MW, MH, None, 1.0),
        (130 + GAP, 400)
    );
}

/// 侧向按**底边**对齐锚点底边（菜单贴着工具栏往上长，同任务栏菜单）。
///
/// 顶边对齐会栽：纵条只有百来像素高、菜单常有三四百，底边顶出工作区后钳回来，整个菜单
/// 被推到条顶之上。
#[test]
fn side_aligns_bottom_with_anchor_bottom() {
    // 条占 y[896,1028]（右下角典型落点），菜单高 300 → 顶边 728、底边 1028 齐平。
    let a = MenuAnchor::beside_rect(1870, 896, 1900, 1028);
    let (_, y) = place_menu(&a, 1870, 896, MW, MH, WA, 1.0);
    assert_eq!(y, 1028 - MH as i32);
    assert_eq!(y + MH as i32, 1028, "菜单底边与工具栏底边齐平");
    assert!(y < 896, "菜单比条高，理应向上长出条顶");
}

/// 纵条被拖到屏幕顶部：底边对齐会把菜单顶出工作区上边，改回顶边对齐向下展开。
#[test]
fn side_flips_to_top_align_when_no_room_up() {
    // 条占 y[12,144]，菜单高 300 → 底边对齐得 -156，越界；回退到顶边对齐 12。
    let a = MenuAnchor::beside_rect(20, 12, 50, 144);
    assert_eq!(place_menu(&a, 20, 12, MW, MH, WA, 1.0), (50 + GAP, 12));
}

/// 两侧间隙必须一样宽，否则一边紧贴、一边有缝。
/// 用同一个锚点分别逼出左右两种落点，比较各自到锚点的距离。
#[test]
fn side_gap_is_symmetric_on_both_sides() {
    // 右侧装得下 → 走右侧。
    let roomy = MenuAnchor::beside_rect(400, 500, 430, 700);
    let (rx, _) = place_menu(&roomy, 400, 500, MW, MH, WA, 1.0);
    let right_gap = rx - roomy.right;

    // 同尺寸的条挪到屏幕右缘 → 右侧放不下，走左侧。
    let tight = MenuAnchor::beside_rect(1890, 500, 1920, 700);
    let (lx, _) = place_menu(&tight, 1890, 500, MW, MH, WA, 1.0);
    let left_gap = tight.x - (lx + MW as i32);

    assert_eq!(
        right_gap, left_gap,
        "左右间隙不等：右 {right_gap} / 左 {left_gap}"
    );
    assert_eq!(right_gap, GAP);
}

/// 间隙是 dp，随 DPI 缩放——高分屏上不能缩成一条看不见的线。
#[test]
fn side_gap_scales_with_dpi() {
    let a = MenuAnchor::beside_rect(400, 500, 430, 700);
    let (x1, _) = place_menu(&a, 400, 500, MW, MH, WA, 1.0);
    let (x2, _) = place_menu(&a, 400, 500, MW, MH, WA, 2.0);
    assert_eq!(x1 - a.right, GAP);
    assert_eq!(x2 - a.right, GAP * 2);
}

/// 二级子菜单因右侧无空间翻到左侧——最基础的翻转场景（`prefer_left=false`）。
#[test]
fn place_child_flips_left_when_right_overflows() {
    // 父面板贴在屏幕右侧（右缘 1947），右侧已经没有 200px 空间摆子菜单。
    let (x, _y, went_left) =
        place_child_in_area((1800, 100), (150, 400), None, (MW, MH), 1.0, false, WA);
    assert!(went_left, "右侧放不下应翻到左侧");
    assert!(
        x + MW as i32 <= 1920,
        "翻到左侧后子菜单不应再超出工作区右边界"
    );
}

/// 上一层已经翻到左侧（`prefer_left=true`）：即便本层单独看右侧装得下，也应继续往左展开。
/// 三级贴着已在左侧的二级展开，右侧腾出的空间恰是一级面板所在处，翻回右侧会压在一级上面。
#[test]
fn place_child_continues_left_once_preferred() {
    // 父面板在屏幕中部，单独看右侧明明装得下（1000+150-3+200=1347<1920）。
    let (x, _y, went_left) =
        place_child_in_area((1000, 100), (150, 400), None, (MW, MH), 1.0, true, WA);
    assert!(
        went_left,
        "已翻左的展开链应延续左侧，即便右侧单独看也装得下"
    );
    assert_eq!(x, 1000 - MW as i32 + 3, "应贴父面板左缘展开");
}

/// 延续左侧的偏好只在左侧确实没有空间时才放弃：父面板贴在屏幕左侧，
/// 左侧连子菜单都摆不下，此时该翻回右侧，而不是被钳出屏幕。
#[test]
fn place_child_prefer_left_falls_back_to_right_when_no_room_left() {
    let (x, _y, went_left) =
        place_child_in_area((50, 100), (150, 400), None, (MW, MH), 1.0, true, WA);
    assert!(!went_left, "左侧没有空间时应翻回右侧");
    assert_eq!(x, 50 + 150 - 3, "应贴父面板右缘展开");
}
