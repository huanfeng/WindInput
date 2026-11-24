use super::*;

fn item(label: &str) -> MenuItemSpec {
    MenuItemSpec::leaf(label, MenuKind::Copy, true, false)
}

fn base() -> LevelRender {
    LevelRender {
        items: vec![item("a"), item("b")],
        selected: 0,
        geom: (100, 200, 80, 40),
    }
}

/// 窗口不可见（初始态 / 刚被 hide）时必须画满并摆位——跳过任何一步都会「菜单不出现」。
#[test]
fn invisible_window_needs_both() {
    assert_eq!(plan_render(None, &base()), (true, true));
}

/// 完全没变 → 一步都不做。
#[test]
fn unchanged_level_does_nothing() {
    let prev = base();
    assert_eq!(plan_render(Some(&prev), &base()), (false, false));
}

/// 只有高亮行变了时，重绘但绝不重新摆位：摆位会把父层顶到子菜单前面一瞬，表现为闪烁。
#[test]
fn selection_change_repaints_without_restacking() {
    let prev = base();
    let mut want = base();
    want.selected = 1;
    assert_eq!(plan_render(Some(&prev), &want), (true, false));
}

/// 内容换了（父项切换导致同一个窗口改画另一份子菜单）→ 重绘，位置没变则不摆位。
#[test]
fn content_change_repaints_without_restacking() {
    let prev = base();
    let mut want = base();
    want.items = vec![item("x")];
    assert_eq!(plan_render(Some(&prev), &want), (true, false));
}

/// 只挪位置（尺寸不变）→ 必须摆位；像素没变则不必重绘。
#[test]
fn move_only_restacks_without_repaint() {
    let prev = base();
    let mut want = base();
    want.geom = (150, 200, 80, 40);
    assert_eq!(plan_render(Some(&prev), &want), (false, true));
}

/// 尺寸变化必须同时重绘：buffer 尺寸变了，旧像素直接失效。
#[test]
fn resize_needs_repaint_too() {
    let prev = base();
    let mut want = base();
    want.geom = (100, 200, 120, 40);
    assert_eq!(plan_render(Some(&prev), &want), (true, true));
}
