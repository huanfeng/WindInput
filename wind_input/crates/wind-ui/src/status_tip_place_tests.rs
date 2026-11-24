//! 气泡落点的纯几何：**坐标不得假设桌面原点为 (0,0)**。
//!
//! 此前 `clamp_below_or_above` 出口处有一行 `(nx.max(0), ny.max(0))`，把上面按
//! `MonitorFromPoint` + `rcWork` 算对的结果又拍回非负象限。摆在主屏左侧/上方的副屏
//! 坐标整块为负，于是气泡被吸到主屏边缘——多显示器下的「气泡永远在主屏」。
//! 同一病灶本仓在工具栏上踩过一次（`corner_in_work_area` 内明令禁止 `max(0)`）。
use super::{place_below_or_above, place_caret_tip};

/// 主屏（虚拟桌面原点）与摆在它**左侧**的副屏。副屏工作区 x 全为负数——
/// 这正是 `max(0)` 会把气泡整块吸回主屏的那种布局。
const MAIN: (i32, i32, i32, i32) = (0, 0, 2560, 1368);
const LEFT: (i32, i32, i32, i32) = (-1920, 0, 0, 1040);
/// 摆在主屏**上方**的副屏：y 为负。
const TOP: (i32, i32, i32, i32) = (0, -1080, 1920, -40);

const TIP: (u32, u32) = (120, 34);
const GAP: i32 = 4;

/// 光标底 500、高 20（顶 480）；候选窗在光标下方 (300, 504)–(900, 560)。
const CARET_B: i32 = 500;
const CARET_H: i32 = 20;
const CAND_BELOW: (i32, i32, i32, i32) = (300, 504, 900, 560);
const CAND_ABOVE: (i32, i32, i32, i32) = (300, 424, 900, 476);

fn tip(
    above: bool,
    cand: Option<(i32, i32, i32, i32)>,
    bounds: Option<(i32, i32, i32, i32)>,
) -> (i32, i32) {
    place_caret_tip(300, CARET_B, CARET_H, TIP, GAP, 0, above, cand, bounds)
}

const BELOW_Y: i32 = CARET_B + GAP;
const ABOVE_Y: i32 = CARET_B - CARET_H - 34 - GAP;

#[test]
fn caret_side_picks_the_preferred_side_without_candidates() {
    assert_eq!(tip(false, None, Some(MAIN)), (300, BELOW_Y));
    assert_eq!(tip(true, None, Some(MAIN)), (300, ABOVE_Y));
}

/// 候选窗在下方挡住首选的下方：换到光标上方。首选上方本就不挡，照旧。
#[test]
fn candidate_below_pushes_tip_above_the_caret() {
    assert_eq!(tip(false, Some(CAND_BELOW), Some(MAIN)), (300, ABOVE_Y));
    assert_eq!(tip(true, Some(CAND_BELOW), Some(MAIN)), (300, ABOVE_Y));
}

/// 候选窗被翻到光标上方：首选下方不挡，照旧下方。
#[test]
fn candidate_above_leaves_tip_below() {
    assert_eq!(tip(false, Some(CAND_ABOVE), Some(MAIN)), (300, BELOW_Y));
    assert_eq!(tip(true, Some(CAND_ABOVE), Some(MAIN)), (300, BELOW_Y));
}

/// 光标贴着屏幕上沿（上方放不下）、候选窗在下方：贴到候选窗下沿外。
#[test]
fn no_room_above_goes_outside_the_candidate_window() {
    let top_caret = |cand| place_caret_tip(300, 30, 20, TIP, GAP, 0, false, Some(cand), Some(MAIN));
    assert_eq!(top_caret((300, 34, 900, 90)), (300, 90 + GAP));
}

/// 提示泡与候选窗左右不重叠时不算挡住。
#[test]
fn horizontally_disjoint_candidate_does_not_count() {
    assert_eq!(
        tip(false, Some((1000, 504, 1400, 560)), Some(MAIN)),
        (300, BELOW_Y)
    );
}

/// 外部宿主没有屏幕几何：只按候选窗判，照样换到上方。
#[test]
fn without_bounds_only_the_candidate_window_matters() {
    assert_eq!(tip(false, Some(CAND_BELOW), None), (300, ABOVE_Y));
    assert_eq!(tip(false, None, None), (300, BELOW_Y));
}

/// 偏移只加在首选侧：换到另一侧时不叠。
#[test]
fn offset_applies_to_the_preferred_side_only() {
    let p =
        |above, cand| place_caret_tip(300, CARET_B, CARET_H, TIP, GAP, 7, above, cand, Some(MAIN));
    assert_eq!(p(false, None), (300, BELOW_Y + 7));
    assert_eq!(p(true, None), (300, ABOVE_Y + 7));
    assert_eq!(p(false, Some(CAND_BELOW)), (300, ABOVE_Y));
}

/// ★ 左侧副屏：负的 x/y 必须原样保留，绝不能被钳到 0（那会跳回主屏）。
#[test]
fn left_monitor_negative_x_survives() {
    let (x, y) = place_below_or_above(-1200, 500, TIP, 480, 20, GAP, LEFT);
    assert_eq!((x, y), (-1200, 500), "副屏内的合法负坐标不该被改写");
}

/// ★ 上方副屏：整块屏的 y 都是负的，上翻后仍须落在该屏内。
/// caret 必须取该屏内的坐标（-60），下方放不下（-56 + 34 > -40）故上翻。
#[test]
fn top_monitor_negative_y_survives() {
    let caret_y = -60;
    let (x, y) = place_below_or_above(300, caret_y + GAP, TIP, caret_y, 20, GAP, TOP);
    assert_eq!(x, 300);
    assert_eq!(y, caret_y - 20 - 34 - GAP, "应上翻到光标上方，且保持负坐标");
    assert!(
        y >= TOP.1 && y + TIP.1 as i32 <= TOP.3,
        "必须留在上方副屏内"
    );
}

/// 左侧副屏右边界是 0：贴右边缘时结果为负，同样不该被 `max(0)` 吃掉。
#[test]
fn left_monitor_right_edge_clamps_to_negative() {
    let (x, _) = place_below_or_above(-60, 500, TIP, 480, 20, GAP, LEFT);
    assert_eq!(x, -120, "右溢出应回拉到 right - w = -120，而非 0");
}

/// 主屏常规回归：下方放得下就用下方，坐标原样。
#[test]
fn main_monitor_below_unchanged() {
    let (x, y) = place_below_or_above(800, 600, TIP, 580, 20, GAP, MAIN);
    assert_eq!((x, y), (800, 600));
}

/// 主屏底部：下方放不下 → 上翻到光标上方。
#[test]
fn main_monitor_flips_above_near_bottom() {
    let caret_y = 1360;
    let (_, y) = place_below_or_above(800, 1364, TIP, caret_y, 20, GAP, MAIN);
    assert_eq!(y, caret_y - 20 - 34 - GAP);
}

/// 上下都放不下（工作区比气泡还矮）时贴下沿，且左上角保持可见。
#[test]
fn degenerate_short_work_area_pins_to_bottom() {
    let short = (0, 0, 1920, 30);
    let (_, y) = place_below_or_above(100, 20, TIP, 10, 10, GAP, short);
    assert_eq!(y, short.1, "上翻也放不下 → 贴下沿后再被上边界兜回 top");
}
