use super::*;
use std::sync::mpsc::channel;

/// 菜单几乎总是由尚未抬起的点击唤出；把这枚旧按下当新点击，菜单会弹出即消失。
#[test]
fn already_held_button_is_not_a_new_press() {
    assert!(!press_edge(true, true));
}

/// 抬起沿不关：否则「按住拖过菜单外再松手」会在松手时误关。
#[test]
fn release_is_not_a_press() {
    assert!(!press_edge(true, false));
}

/// 真正该关的唯一形态：上一轮空手、这一轮按下。
#[test]
fn fresh_press_is_detected() {
    assert!(press_edge(false, true));
}

#[test]
fn idle_is_not_a_press() {
    assert!(!press_edge(false, false));
}

/// `menu_visible()` 随菜单显隐：hide 后不复位，候选窗 / 气泡的右键会一直被当成「由轮询接手」而丢掉。
#[test]
fn menu_visible_tracks_show_and_hide() {
    let (tx, _rx) = channel();
    let mut m = PopupMenu::new(tx).expect("mock 菜单");
    set_menu_visible(false);
    m.show(
        vec![MenuItemSpec::leaf("x", MenuKind::Copy, true, false)],
        MenuAnchor::at_point(10, 10),
    );
    assert!(menu_visible());
    m.hide();
    assert!(!menu_visible());
}

/// 在气泡上右键 = 重新右键；左键 / 左右同按只关菜单。左右手互换时物理左键才是右键。
#[test]
fn right_press_follows_logical_buttons() {
    assert!(is_right_press(false, true, false));
    assert!(!is_right_press(true, false, false));
    assert!(!is_right_press(true, true, false), "左右同按算左键");
    assert!(!is_right_press(false, false, false), "中键");
    assert!(
        is_right_press(true, false, true),
        "互换后物理左键是逻辑右键"
    );
    assert!(!is_right_press(false, true, true));
}

fn state_with_panel_at(ox: i32, oy: i32, w: u32, h: u32) -> MenuState {
    let (tx, _rx) = channel();
    let mut lv = Level::new(
        vec![MenuItemSpec::leaf("a", MenuKind::Copy, true, false)],
        0,
    );
    lv.origin = (ox, oy);
    lv.size = (w, h);
    MenuState {
        levels: vec![lv],
        capture_origin: (777, 888), // 故意非零，见下面两条测试的说明
        dirty: false,
        closed: false,
        events: tx,
    }
}

/// 轮询传给 `find_hit` 的必须是屏幕坐标：`GetCursorPos` 本来就是屏幕坐标，不能再像
/// `on_message` 那样经 `screen()` 平移。`capture_origin` 特意非零，补上那次转换这条就红。
#[test]
fn cursor_inside_panel_hits_in_screen_coords() {
    let st = state_with_panel_at(100, 100, 80, 40);
    assert!(st.find_hit(120, 110).is_some());
}

/// 面板外 → 无命中 → 该关。任务栏、别的应用、原文本框都落在这一支。
#[test]
fn cursor_outside_panel_misses() {
    let st = state_with_panel_at(100, 100, 80, 40);
    assert!(st.find_hit(500, 500).is_none());
    // 右/下边界是开区间（`sx < ox + w`），紧贴右下角外一像素也算外面。
    assert!(st.find_hit(180, 140).is_none());
}
