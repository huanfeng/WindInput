use super::*;
use crate::manager::{MenuCmd, MenuKind};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};

struct Cap(Mutex<Vec<Vec<u8>>>);
impl HostRenderSink for Cap {
    fn push_frame(&self, f: &[u8]) {
        self.0.lock().unwrap().push(f.to_vec());
    }
}
impl Cap {
    fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }
    /// 取出累计的帧，解成 (kind, 可见, x, y, w, h)。
    fn take(&self) -> Vec<(u32, bool, i32, i32, u32, u32)> {
        let u = |f: &[u8], o: usize| u32::from_le_bytes(f[8 + o..12 + o].try_into().unwrap());
        std::mem::take(&mut *self.0.lock().unwrap())
            .iter()
            .map(|f| {
                (
                    u(f, 0),
                    u(f, 16) & SharedRenderHeader::FLAG_VISIBLE != 0,
                    u(f, 24) as i32,
                    u(f, 28) as i32,
                    u(f, 8),
                    u(f, 12),
                )
            })
            .collect()
    }
}

fn leaf(label: &str, cmd: MenuCmd) -> MenuItemSpec {
    MenuItemSpec::leaf(label, MenuKind::Command(cmd), true, false)
}

/// 两项顶层菜单，第二项带一个两项的子菜单。
fn items() -> Vec<MenuItemSpec> {
    vec![
        leaf("全角", MenuCmd::ToggleWidth),
        MenuItemSpec::submenu(
            "主题",
            vec![
                leaf("跟随系统", MenuCmd::ReloadConfig),
                leaf("浅色", MenuCmd::OpenSettings),
            ],
        ),
    ]
}

fn host() -> (MenuHost, Receiver<UiEvent>, Cap) {
    let (tx, rx) = channel();
    // SHM 名带进程号与线程号：并发测试不撞段。
    let suffix = format!(
        "_tm{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    )
    .replace(['(', ')', ' '], "")
    .replace("ThreadId", "");
    let mut h = MenuHost::new(tx, suffix);
    // 钉死 1x：`set_work_area` 会按本机真实屏幕取缩放，Retina 上跑会得到 2x，
    // 而下面的用例都按「宿主单位 = 像素」断言。
    h.set_work_area_scaled((0, 0, 1280, 800), 1.0, 1.0);
    set_host_menu_scale(Some(1.0));
    (h, rx, Cap::new())
}

fn events(rx: &Receiver<UiEvent>) -> Vec<String> {
    rx.try_iter().map(|e| format!("{e:?}")).collect()
}

/// 宿主坐标是点（macOS）：锚点 / 工作区 / 指针乘 scale 进菜单，落位除回来，位图尺寸仍是像素。
#[test]
fn point_host_converts_anchor_work_area_and_pointer() {
    let (mut h, _rx, cap) = host();
    h.set_work_area_scaled((0, 0, 640, 400), 2.0, 2.0);
    h.show(&cap, items(), MenuAnchor::at_point(50, 100));
    let f = cap.take();
    assert_eq!(f.len(), 1);
    let (_, visible, x, y, w, _h) = f[0];
    assert!(visible);
    assert_eq!((x, y), (50, 100), "落位折回宿主坐标：锚点原样");
    let (host_w, _) = (w, 0);
    // 同一菜单在 1x 宿主下的位图宽 = 2x 宿主下的位图宽 / 2（字号随 scale 放大）。
    let (mut h1, _rx1, cap1) = host();
    h1.set_work_area_scaled((0, 0, 640, 400), 1.0, 1.0);
    set_host_menu_scale(Some(1.0));
    h1.show(&cap1, items(), MenuAnchor::at_point(50, 100));
    let w1 = cap1.take()[0].4;
    assert!(host_w > w1, "2x 的位图应更宽：{host_w} vs {w1}");
    set_host_menu_scale(None);
}

#[test]
fn show_pushes_one_exact_frame_at_anchor_and_hide_pushes_hidden() {
    let (mut h, _rx, cap) = host();
    h.show(&cap, items(), MenuAnchor::at_point(100, 200));
    let f = cap.take();
    assert_eq!(f.len(), 1, "只有顶层一级：{f:?}");
    let (kind, vis, x, y, w, hh) = f[0];
    assert_eq!(kind, OVERLAY_KIND_MENU);
    assert!(vis && w > 0 && hh > 0);
    assert_eq!((x, y), (100, 200), "无投影时窗口左上即锚点");
    h.hide(&cap);
    assert_eq!(
        events(&_rx),
        vec!["MenuClose".to_string()],
        "收可见菜单回送 MenuClose"
    );
    let f = cap.take();
    assert_eq!(f.len(), 1);
    assert_eq!((f[0].0, f[0].1), (OVERLAY_KIND_MENU, false));
    h.hide(&cap);
    assert!(cap.take().is_empty(), "已隐藏的级不重复推隐藏帧");
}

#[test]
fn anchor_near_bottom_right_is_clamped_into_host_work_area() {
    let (mut h, _rx, cap) = host();
    h.show(&cap, items(), MenuAnchor::at_point(1270, 790));
    let (_, _, x, y, w, hh) = cap.take()[0];
    assert!(
        x + w as i32 <= 1280 && y + hh as i32 <= 800,
        "@({x},{y}) {w}x{hh}"
    );
}

/// 悬停：鼠标移到第一项 → 重推一帧（高亮）；移到带子菜单的项 → 第 1 级出现在右侧；
/// 移进子菜单的项 → 子菜单重推（高亮）；再移到菜单外 → 只灭最深层（子菜单）的高亮，
/// 子菜单保留、顶层不动。
#[test]
fn hover_repaints_and_opens_submenu_to_the_right() {
    let (mut h, _rx, cap) = host();
    h.show(&cap, items(), MenuAnchor::at_point(100, 100));
    let (_, _, x, y, w, hh) = cap.take()[0];
    let row = |y: i32, hh: u32, i: i32| y + 4 + (hh as i32 - 8) * (2 * i + 1) / 4;
    h.pointer(&cap, MenuPointerEvent::Move, x + 20, row(y, hh, 0));
    let f = cap.take();
    assert_eq!(f.len(), 1, "高亮只重推顶层：{f:?}");
    h.pointer(&cap, MenuPointerEvent::Move, x + 20, row(y, hh, 0));
    assert!(cap.take().is_empty(), "同一项上移动不重推");
    h.pointer(&cap, MenuPointerEvent::Move, x + 20, row(y, hh, 1));
    let f = cap.take();
    let sub = f.iter().find(|t| t.0 == OVERLAY_KIND_MENU + 1).copied();
    let (_, vis, sx, sy, _, sh) = sub.unwrap_or_else(|| panic!("子菜单没出来：{f:?}"));
    assert!(vis && sx >= x + w as i32 - 10, "子菜单在右侧：x={sx}");
    h.pointer(&cap, MenuPointerEvent::Move, sx + 20, row(sy, sh, 0));
    let f = cap.take();
    assert_eq!(
        f.iter().map(|t| (t.0, t.1)).collect::<Vec<_>>(),
        vec![(OVERLAY_KIND_MENU + 1, true)],
        "子菜单项高亮只重推子菜单：{f:?}"
    );
    h.pointer(&cap, MenuPointerEvent::Move, 5, 5);
    let f = cap.take();
    assert_eq!(
        f.iter().map(|t| (t.0, t.1)).collect::<Vec<_>>(),
        vec![(OVERLAY_KIND_MENU + 1, true)],
        "移出菜单只灭子菜单的高亮、子菜单不收、顶层不动：{f:?}"
    );
}

#[test]
fn click_on_leaf_reports_action_and_closes() {
    let (mut h, rx, cap) = host();
    h.show(&cap, items(), MenuAnchor::at_point(100, 100));
    let (_, _, x, y, _, hh) = cap.take()[0];
    h.pointer(&cap, MenuPointerEvent::LeftPress, x + 20, y + hh as i32 / 4);
    let ev = events(&rx);
    assert!(
        ev.iter()
            .any(|e| e.contains("MenuAction") && e.contains("ToggleWidth")),
        "{ev:?}"
    );
    let f = cap.take();
    assert!(f.iter().any(|t| !t.1), "点选后菜单收起：{f:?}");
}

#[test]
fn press_outside_or_right_press_closes_with_menu_close_event() {
    for ev in [
        MenuPointerEvent::LeftPress,
        MenuPointerEvent::RightPress,
        MenuPointerEvent::OtherPress,
    ] {
        let (mut h, rx, cap) = host();
        h.show(&cap, items(), MenuAnchor::at_point(100, 100));
        cap.take();
        h.pointer(&cap, ev, 900, 700);
        assert!(events(&rx).iter().any(|e| e == "MenuClose"), "{ev:?}");
        assert!(cap.take().iter().any(|t| !t.1), "{ev:?} 后菜单收起");
    }
}

#[test]
fn keyboard_navigates_and_escape_closes() {
    let (mut h, rx, cap) = host();
    h.show(&cap, items(), MenuAnchor::at_point(100, 100));
    cap.take();
    h.key(&cap, 0x28); // Down → 第一项高亮
    assert_eq!(cap.take().len(), 1);
    h.key(&cap, 0x28); // Down → 「主题」
    h.key(&cap, 0x27); // Right → 展开子菜单
    assert!(
        cap.take()
            .iter()
            .any(|t| t.0 == OVERLAY_KIND_MENU + 1 && t.1),
        "→ 展开子菜单"
    );
    h.key(&cap, 0x1B);
    assert!(events(&rx).iter().any(|e| e == "MenuClose"));
    let f = cap.take();
    assert_eq!(
        f.iter().filter(|t| !t.1).count(),
        2,
        "两级都推隐藏帧：{f:?}"
    );
}

/// 两端对「菜单开着」的认识错开时自愈：菜单不在屏上还收到键 / 指针，回 MenuClose。
#[test]
fn input_for_an_invisible_menu_reports_close() {
    let (mut h, rx, cap) = host();
    h.key(&cap, 0x28);
    assert_eq!(events(&rx), vec!["MenuClose".to_string()]);
    h.pointer(&cap, MenuPointerEvent::Move, 1, 1);
    assert_eq!(events(&rx), vec!["MenuClose".to_string()]);
    assert!(cap.take().is_empty());
}

/// SHM 段建不起来：顶层推不出帧，屏上没有菜单。必须当场回送 `MenuClose`、菜单不留在
/// 「可见」态——否则协调器的 `menu_open` 一直为真，下一个方向键 / 回车被吞。
#[test]
fn top_level_shm_failure_closes_menu_and_reports_close() {
    let (mut h, rx, cap) = host();
    h.create_shm = |_, _| Err(std::io::Error::other("注入的失败"));
    h.show(&cap, items(), MenuAnchor::at_point(100, 100));
    assert_eq!(events(&rx), vec!["MenuClose".to_string()]);
    assert!(cap.take().is_empty(), "没推出过帧，也就没有隐藏帧可补");
    assert!(!h.menu.as_ref().unwrap().is_visible(), "菜单已收起");
    // 之后再来的菜单键走自愈路径（不可见 → 再回一条 MenuClose），不会被当成导航。
    h.key(&cap, 0x28);
    assert_eq!(events(&rx), vec!["MenuClose".to_string()]);
}

/// 顶层推过之后才推不出：收菜单时把此前推过的那一帧补推成隐藏帧，宿主那边不留残影。
#[test]
fn top_level_push_failure_after_success_hides_the_pushed_frame() {
    let (mut h, rx, cap) = host();
    h.show(&cap, items(), MenuAnchor::at_point(100, 100));
    assert_eq!(cap.take().len(), 1);
    // 丢掉已建好的段，下次推帧要重建——让它失败。
    h.shms[0] = None;
    h.create_shm = |_, _| Err(std::io::Error::other("注入的失败"));
    h.key(&cap, 0x28); // 高亮变化 → 顶层要重推
    assert_eq!(events(&rx), vec!["MenuClose".to_string()]);
    let f = cap.take();
    assert_eq!(
        f.iter().map(|t| (t.0, t.1)).collect::<Vec<_>>(),
        vec![(OVERLAY_KIND_MENU, false)],
        "补推一帧隐藏：{f:?}"
    );
}

/// 协调器把宿主报来的坐标钳到 ±2^24（`message_handler` 的 `HOST_COORD_LIMIT`）：在这个
/// 范围的极端处弹菜单、展开子菜单、移动指针，定位与命中里的加法都不溢出（测试构建开着
/// 溢出检查；不钳制时锚点取 `i32::MAX` 会在定位里 panic）。
#[test]
fn coordinates_at_the_clamp_limit_do_not_overflow() {
    const L: i32 = 1 << 24;
    for (x, y) in [(L, L), (-L, -L), (L, -L), (-L, L)] {
        for (wl, wt, wr, wb) in [(-L, -L, L, L), (0, 0, 1280, 800), (0, 0, 0, 0)] {
            let (mut h, _rx, cap) = host();
            h.set_work_area(wl, wt, wr, wb);
            h.show(&cap, items(), MenuAnchor::at_point(x, y));
            h.key(&cap, 0x28);
            h.key(&cap, 0x28);
            h.key(&cap, 0x27); // 展开子菜单
            for (px, py) in [(x, y), (L, L), (-L, -L)] {
                h.pointer(&cap, MenuPointerEvent::Move, px, py);
            }
            h.pointer(&cap, MenuPointerEvent::OtherPress, L, L);
            assert!(
                !cap.take().is_empty(),
                "@({x},{y}) 工作区 {wl},{wt},{wr},{wb}"
            );
        }
    }
}

#[test]
fn more_item_anchor_sentinel_uses_last_pointer() {
    let (mut h, _rx, cap) = host();
    h.show(&cap, items(), MenuAnchor::at_point(100, 100));
    h.pointer(&cap, MenuPointerEvent::Move, 300, 400);
    h.hide(&cap);
    cap.take();
    h.show(&cap, items(), MenuAnchor::at_point(i32::MIN, i32::MIN));
    let (_, _, x, y, _, _) = cap.take()[0];
    assert_eq!((x, y), (300, 400));
}
