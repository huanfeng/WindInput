use super::*;
use std::sync::mpsc::channel;

fn sub(label: &str, children: Vec<MenuItemSpec>) -> MenuItemSpec {
    let mut it = MenuItemSpec::leaf(label, MenuKind::Submenu, true, false);
    it.children = children;
    it
}

fn leaf(label: &str) -> MenuItemSpec {
    MenuItemSpec::leaf(label, MenuKind::Copy, true, false)
}

/// 造一个两层菜单：一级 origin=(0,0) 100x100，行高 20；二级 origin=(200,0) 100x100。
/// 二级窗口内 y<10 是 root padding（无条目），模拟真实的「窗口内但不在任何 item 上」。
fn two_levels() -> (MenuState, std::sync::mpsc::Receiver<UiEvent>) {
    let (tx, rx) = channel();
    let child_items = vec![leaf("c0"), leaf("c1")];
    let mut lv0 = Level::new(vec![sub("s", child_items.clone()), leaf("other")], 0);
    lv0.origin = (0, 0);
    lv0.size = (100, 100);
    lv0.item_rects = vec![
        (
            0,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 20.0,
            },
        ),
        (
            1,
            Rect {
                x: 0.0,
                y: 20.0,
                w: 100.0,
                h: 20.0,
            },
        ),
    ];
    let mut lv1 = Level::new(child_items, NONE_SEL);
    lv1.origin = (200, 0);
    lv1.size = (100, 100);
    lv1.item_rects = vec![
        (
            0,
            Rect {
                x: 0.0,
                y: 10.0,
                w: 100.0,
                h: 20.0,
            },
        ),
        (
            1,
            Rect {
                x: 0.0,
                y: 30.0,
                w: 100.0,
                h: 20.0,
            },
        ),
    ];
    let st = MenuState {
        levels: vec![lv0, lv1],
        capture_origin: (0, 0),
        dirty: false,
        closed: false,
        events: tx,
    };
    (st, rx)
}

/// 走 `WM_MOUSEMOVE` 的真实分派路径（capture_origin=(0,0)，故客户区坐标==屏幕坐标）。
fn mouse_move(st: &mut MenuState, x: i32, y: i32) {
    let lparam = LPARAM((((y & 0xFFFF) << 16) | (x & 0xFFFF)) as isize);
    st.on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), lparam);
}

/// 高亮必须跟手：鼠标移到子菜单面板内的非条目处（root padding / 边框），该层高亮灭掉。
/// 残留高亮会让用户以为菜单卡住了。
#[test]
fn blank_area_inside_panel_clears_highlight() {
    let (mut st, _rx) = two_levels();
    st.levels[1].selected = 1;
    // (250, 5) 落在二级窗口内，但 y=5 不在任何 item_rect 上。
    assert_eq!(st.find_hit(250, 5), Some((1, None)));
    mouse_move(&mut st, 250, 5);
    assert_eq!(st.levels[1].selected, NONE_SEL, "面板空白处应灭掉本层高亮");
    assert!(st.dirty, "灭高亮要重绘");
}

/// 鼠标移出全部菜单窗口 → 最深层高亮灭掉（原来是一直亮着，观感像卡住）。
#[test]
fn outside_menu_clears_deepest_highlight() {
    let (mut st, _rx) = two_levels();
    st.levels[1].selected = 1;
    assert_eq!(st.find_hit(500, 500), None, "该点不在任何菜单窗口内");
    mouse_move(&mut st, 500, 500);
    assert_eq!(
        st.levels[1].selected, NONE_SEL,
        "移出菜单外应灭掉最深层高亮"
    );
}

/// **灭高亮不得伤及父层**：父项高亮指示的是「子菜单从这里展开」，灭掉它菜单看起来就断链了。
/// 同时子菜单本身必须还在——`clear_hover` 只动 selected，绝不动 levels。
///
/// 关键在 `(50, 50)` 这一下：它落在**父面板**窗口内、不在任何条目上（子菜单仍开着），
/// 于是分派路径会把**非最深层**的 k 交给 `clear_hover`，真正踩到那道「仅最深层」保护。
/// 少了这一下，本测试就只是在测最深层、名不副实（曾经如此，被反向对照抓出）。
#[test]
fn clearing_never_touches_parent_or_closes_submenu() {
    let (mut st, _rx) = two_levels();
    st.levels[1].selected = 1;
    assert_eq!(
        st.find_hit(50, 50),
        Some((0, None)),
        "该点应命中父面板空白处"
    );
    mouse_move(&mut st, 50, 50);
    assert_eq!(st.levels[0].selected, 0, "父项高亮必须保留（子菜单还开着）");
    assert_eq!(st.levels[1].selected, 1, "父面板空白处不该动子菜单的高亮");
    // 子菜单面板内空白、菜单外，另两条路径也不得伤及父层。
    mouse_move(&mut st, 250, 5);
    mouse_move(&mut st, 500, 500);
    assert_eq!(st.levels[0].selected, 0, "父项高亮必须保留（子菜单还开着）");
    assert_eq!(st.levels.len(), 2, "灭高亮不得收起子菜单");
}

/// 直接调 `clear_hover` 指定非最深层同样必须是空操作（键盘/未来调用点的兜底保证）。
#[test]
fn clear_hover_skips_non_deepest_level() {
    let (mut st, _rx) = two_levels();
    st.clear_hover(0);
    assert_eq!(st.levels[0].selected, 0, "父层高亮不该被清（子菜单还开着）");
    assert!(!st.dirty);
}

/// 只剩一层时移出菜单外 → 该层高亮也要灭（此时它就是最深层）。
#[test]
fn single_level_clears_when_pointer_leaves() {
    let (mut st, _rx) = two_levels();
    st.levels.truncate(1);
    st.levels[0].selected = 1;
    mouse_move(&mut st, 500, 500);
    assert_eq!(st.levels[0].selected, NONE_SEL);
}
