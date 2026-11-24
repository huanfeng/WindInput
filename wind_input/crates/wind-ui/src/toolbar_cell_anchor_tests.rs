use super::*;
use crate::view::Rect;

/// 造一个只填了锚点计算所需字段的鼠标处理器。
fn mouse(vertical: bool, hits: Vec<(ToolbarAction, Rect)>) -> ToolbarMouse {
    let (tx, _rx) = std::sync::mpsc::channel();
    ToolbarMouse {
        hits,
        events: tx,
        hwnd: Default::default(),
        pos: Some((1000, 500)),
        anchor_br: None,
        dragging: false,
        press_cell: false,
        anchor: (0, 0),
        origin: (0, 0),
        hover_idx: -1,
        dirty: false,
        cursor_inside: false,
        size: if vertical { (40, 200) } else { (200, 40) },
        vertical,
    }
}

fn cell(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

/// 拖动阈值是以按下点为中心的全宽矩形：阈值内的手抖仍算点击，出界才转拖动。
#[test]
fn drag_starts_only_past_the_threshold() {
    let a = (100, 100);
    assert!(!drag_started(a, (102, 98), (4, 4)), "2px 内是手抖");
    assert!(drag_started(a, (103, 100), (4, 4)), "横向出界");
    assert!(drag_started(a, (100, 97), (4, 4)), "纵向出界");
}

/// 格上原地按下抬起：照常触发格动作，且不进入拖动。
#[test]
fn click_on_cell_without_moving_still_fires() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut m = mouse(
        false,
        vec![(ToolbarAction::TogglePunct, cell(12.0, 0.0, 30.0, 40.0))],
    );
    m.events = tx;
    let at = LPARAM(20 | (20 << 16));
    m.on_message(HWND::default(), WM_LBUTTONDOWN, WPARAM(MK_LBUTTON), at);
    m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), at);
    assert!(!m.dragging && !m.press_cell);
    assert!(matches!(
        rx.try_recv(),
        Ok(UiEvent::Toolbar(ToolbarAction::TogglePunct))
    ));
}

/// 横条：菜单横向贴到**格的左缘**，纵向仍取整条顶边（向上展开）。
#[test]
fn horizontal_anchor_hugs_the_cell_left_edge() {
    let m = mouse(
        false,
        vec![
            (ToolbarAction::ToggleMode, cell(12.0, 0.0, 30.0, 40.0)),
            (ToolbarAction::TogglePunct, cell(42.0, 0.0, 30.0, 40.0)),
        ],
    );
    let a = m.cell_menu_anchor(50.0, 20.0); // 命中第二格
    assert_eq!(a.x, 1000 + 42, "应贴到格的左缘，而不是整条起点");
    assert_eq!(a.y, 500, "纵向仍取整条顶边");
    assert_eq!(a.bottom, 500 + 40, "翻转回退用整条底边");
}

/// ★ 纵条：`Side` 的纵坐标读的是 **`bottom`**（底边对齐、向上展开），`y` 只作越界
/// 回退。所以两个都得传格的边——只传 `y` 的话菜单仍按整条底边对齐，「贴格」等于没做，
/// 而代码看着是对的。这条测试就是钉住那个差别。
#[test]
fn vertical_anchor_hugs_the_cell_bottom_not_the_bar_bottom() {
    let m = mouse(
        true,
        vec![
            (ToolbarAction::ToggleMode, cell(0.0, 12.0, 40.0, 30.0)),
            (ToolbarAction::TogglePunct, cell(0.0, 42.0, 40.0, 30.0)),
        ],
    );
    let a = m.cell_menu_anchor(20.0, 50.0); // 命中第二格
    assert_eq!(a.bottom, 500 + 42 + 30, "必须是格的底边");
    assert_ne!(a.bottom, 500 + 200, "整条底边就是没贴格");
    assert_eq!(a.y, 500 + 42, "回退用格的上缘");
    assert_eq!(a.right, 1000 + 40, "横向仍贴条身外沿");
}

/// 命中不到格（拖动柄区）时退回整条锚点——右键那里仍该弹完整主菜单。
#[test]
fn miss_falls_back_to_the_whole_bar() {
    let m = mouse(
        false,
        vec![(ToolbarAction::ToggleMode, cell(12.0, 0.0, 30.0, 40.0))],
    );
    let a = m.cell_menu_anchor(5.0, 20.0); // 拖动柄区
    assert_eq!(a.x, 1000, "应退回整条起点");
}
