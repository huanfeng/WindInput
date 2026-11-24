//! 悬停防抖：「收到 WM_MOUSEMOVE」不等于「用户动了鼠标」。
//!
//! 该消息的语义是「鼠标与本窗口的相对位置变了」，气泡自己弹到静止的指针下方同样满足。
//! 少了物理坐标门控，气泡一弹在鼠标上就被判成悬停中，`interacting()` 恒真，
//! 主循环每轮把自动隐藏时刻顺延满一份 —— 表现为气泡常显不消失。

use super::StatusTipMouse;

fn mouse_at(baseline: (i32, i32)) -> StatusTipMouse {
    // 接收端就地丢弃：本组测试只验状态迁移，不发任何 UiEvent。
    let (tx, _) = std::sync::mpsc::channel();
    StatusTipMouse {
        hwnd: crate::sys::HWND::default(),
        events: tx,
        dragging: false,
        grab_dx: 0,
        grab_dy: 0,
        margin: (0, 0),
        drag_pin: None,
        mouse_over: false,
        last_cursor: baseline,
        leave_armed: false,
        menu_open: false,
    }
}

/// 气泡出现/移动到静止指针下方：坐标与基线相同 → 不算移动。
#[test]
fn synthetic_move_at_same_cursor_is_rejected() {
    let mut m = mouse_at((640, 480));
    assert!(!m.accept_move((640, 480)));
}

/// 反向对照：真实移动必须放行，并把基线推进到新位置。
#[test]
fn real_move_is_accepted_and_advances_baseline() {
    let mut m = mouse_at((640, 480));
    assert!(m.accept_move((641, 480)));
    assert_eq!(m.last_cursor, (641, 480));
    // 放行后停在原地不动的后续消息又该被挡住。
    assert!(!m.accept_move((641, 480)));
}

/// `reset_hover` 清悬停残留：隐藏时系统未必投 WM_MOUSELEAVE，不清则下次显示直接被判交互中。
#[test]
fn reset_hover_clears_over_and_leave_arm() {
    let mut m = mouse_at((0, 0));
    m.mouse_over = true;
    m.leave_armed = true;
    assert!(m.interacting(), "反向对照：悬停中确实算交互");
    m.reset_hover();
    assert!(!m.mouse_over);
    assert!(
        !m.leave_armed,
        "不清则 arm_leave 永远早退，LEAVE 再也不会来"
    );
    assert!(!m.interacting());
}

/// 拖动与右键菜单不受门控影响，仍各自独立地维持「交互中」。
#[test]
fn drag_and_menu_still_hold_interacting() {
    let mut m = mouse_at((0, 0));
    m.dragging = true;
    assert!(m.interacting());
    m.dragging = false;
    m.menu_open = true;
    assert!(m.interacting());
}
