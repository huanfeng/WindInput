use super::*;
use crate::sys::*;
use crate::window::{LayeredWindow, WindowMouse};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use wind_ipc::protocol::overlay::{OVERLAY_KIND_SOFTKEYBOARD, OVERLAY_KIND_TOOLBAR};

struct Cap(Mutex<Vec<Vec<u8>>>);
impl wind_bridge::HostRenderSink for Cap {
    fn push_frame(&self, f: &[u8]) {
        self.0.lock().unwrap().push(f.to_vec());
    }
}

/// 装上记录帧的 sink。每个测试跑在自己的线程上，线程局部状态互不干扰；段名带进程号，
/// 并发跑的测试进程不撞段。
fn cap(tag: &str) -> Arc<Cap> {
    let c = Arc::new(Cap(Mutex::new(Vec::new())));
    install(c.clone(), &format!("_e{tag}{}", std::process::id()));
    c
}

fn frames(c: &Cap) -> Vec<Vec<u8>> {
    c.0.lock().unwrap().clone()
}

/// 帧载荷里第 `o` 字节起的 u32（跳过 8 字节帧头）。
fn u(f: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(f[8 + o..12 + o].try_into().unwrap())
}
fn i(f: &[u8], o: usize) -> i32 {
    u(f, o) as i32
}

#[test]
fn show_pushes_exact_frame_with_drag_tail() {
    let c = cap("s");
    let mut w = LayeredWindow::create(None, 4, 3, "WindInputToolbar").unwrap();
    assert_eq!(w.hwnd(), HWND(OVERLAY_KIND_TOOLBAR as isize));
    w.set_drag_regions(vec![[0, 0, 2, 3], [3, 1, 1, 1]]);
    w.buffer_mut().fill(0xFF);
    w.update().unwrap();
    assert!(frames(&c).is_empty(), "还没显示，只写段不推帧");
    w.show(100, 200);
    let fs = frames(&c);
    assert_eq!(fs.len(), 1);
    let f = &fs[0];
    assert_eq!(u(f, 0), OVERLAY_KIND_TOOLBAR);
    assert_eq!((u(f, 8), u(f, 12)), (4, 3));
    assert_ne!(u(f, 16) & SharedRenderHeader::FLAG_VISIBLE, 0);
    assert_eq!(u(f, 20), OVERLAY_PLACE_EXACT);
    assert_eq!((i(f, 24), i(f, 28)), (100, 200));
    assert_eq!(
        (i(f, 48), i(f, 52), u(f, 56), u(f, 60)),
        (0, 0, 4, 3),
        "内容盒取整幅位图"
    );
    assert_eq!(u(f, 72), 2, "拖动区个数");
    let rects: Vec<i32> = (0..8).map(|k| i(f, 76 + 4 * k)).collect();
    assert_eq!(rects, vec![0, 0, 2, 3, 3, 1, 1, 1]);

    // 可见时再 update：照样推帧，位置不变。
    w.update().unwrap();
    let fs = frames(&c);
    assert_eq!(fs.len(), 2);
    assert_eq!((i(&fs[1], 24), i(&fs[1], 28)), (100, 200));
    assert!(u(&fs[1], 4) > u(&fs[0], 4), "重写段推进 seq");
}

#[test]
fn hide_pushes_hidden_frame() {
    let c = cap("h");
    let w = LayeredWindow::create(None, 2, 2, "WindInputSoftKeyboard").unwrap();
    assert_eq!(w.hwnd(), HWND(OVERLAY_KIND_SOFTKEYBOARD as isize));
    w.hide();
    assert!(frames(&c).is_empty(), "没显示过就不发隐藏帧");
    w.update().unwrap();
    w.show(5, 6);
    w.hide();
    let fs = frames(&c);
    assert_eq!(fs.len(), 2);
    assert_eq!(u(&fs[1], 0), OVERLAY_KIND_SOFTKEYBOARD);
    assert_eq!(u(&fs[1], 16) & SharedRenderHeader::FLAG_VISIBLE, 0);
    assert!(u(&fs[1], 4) > u(&fs[0], 4), "隐藏帧也推进 seq");
    w.hide();
    assert_eq!(frames(&c).len(), 2, "重复隐藏不再发");
}

#[test]
fn unbound_class_pushes_nothing() {
    let c = cap("u");
    let w = LayeredWindow::create(None, 2, 2, "WindInputCandidate").unwrap();
    assert_eq!(w.hwnd(), HWND::default());
    w.set_drag_regions(vec![[0, 0, 1, 1]]);
    w.update().unwrap();
    w.show(1, 2);
    w.update_with_alpha(128).unwrap();
    w.hide();
    assert!(frames(&c).is_empty());
}

#[test]
fn dropping_visible_panel_hides_it() {
    let c = cap("d");
    let w = LayeredWindow::create(None, 2, 2, "WindInputToolbar").unwrap();
    w.update().unwrap();
    w.show(0, 0);
    drop(w);
    let fs = frames(&c);
    assert_eq!(fs.len(), 2);
    assert_eq!(u(&fs[1], 16) & SharedRenderHeader::FLAG_VISIBLE, 0);
}

#[test]
fn replacing_window_of_same_kind_keeps_new_binding() {
    let c = cap("r");
    let old = LayeredWindow::create(None, 2, 2, "WindInputToolbar").unwrap();
    // 字段整体赋值的顺序：先建新的，再 drop 旧的。旧的不能把新的注册顶掉。
    let new = LayeredWindow::create(None, 2, 2, "WindInputToolbar").unwrap();
    drop(old);
    new.update().unwrap();
    new.show(7, 8);
    assert_eq!(frames(&c).len(), 1);
}

struct Rec(Vec<(u32, usize, isize)>);
impl WindowMouse for Rec {
    fn on_message(
        &mut self,
        _hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        self.0.push((msg, wparam.0, lparam.0));
        Some(LRESULT(0))
    }
}

#[test]
fn pointer_maps_to_wm_messages() {
    let _c = cap("p");
    let w = LayeredWindow::create(None, 50, 20, "WindInputToolbar").unwrap();
    w.update().unwrap();
    w.show(100, 200);
    let rec = Rc::new(RefCell::new(Rec(Vec::new())));
    w.register_mouse(rec.clone());
    let k = OVERLAY_KIND_TOOLBAR;
    // 位图内 (5, 6)；滚轮按 Win32 约定给屏幕坐标 (105, 206)，其余给客户区坐标。
    let lp = (6 << 16 | 5) as isize;
    let screen_lp = (206 << 16 | 105) as isize;
    let cases: [(u32, u32, u32, usize); 10] = [
        (PANEL_POINTER_MOTION, 0, WM_MOUSEMOVE, 0),
        (PANEL_POINTER_PRESS, 1, WM_LBUTTONDOWN, 0),
        (PANEL_POINTER_RELEASE, 1, WM_LBUTTONUP, 0),
        (PANEL_POINTER_PRESS, 2, WM_MBUTTONDOWN, 0),
        (PANEL_POINTER_RELEASE, 2, WM_MBUTTONUP, 0),
        (PANEL_POINTER_PRESS, 3, WM_RBUTTONDOWN, 0),
        (PANEL_POINTER_RELEASE, 3, WM_RBUTTONUP, 0),
        (PANEL_POINTER_LEAVE, 0, WM_MOUSELEAVE, 0),
        (
            PANEL_POINTER_WHEEL,
            4,
            WM_MOUSEWHEEL,
            (120u16 as usize) << 16,
        ),
        (
            PANEL_POINTER_WHEEL,
            5,
            WM_MOUSEWHEEL,
            ((-120i16) as u16 as usize) << 16,
        ),
    ];
    for (ev, btn, _, _) in cases {
        deliver_pointer(k, ev, btn, 5, 6);
    }
    let want: Vec<(u32, usize, isize)> = cases
        .iter()
        .map(|&(_, _, m, wp)| (m, wp, if m == WM_MOUSEWHEEL { screen_lp } else { lp }))
        .collect();
    assert_eq!(rec.borrow().0, want);
    let wheels: Vec<(i16, i16, i16)> = rec.borrow().0[8..]
        .iter()
        .map(|&(_, wp, l)| {
            (
                (wp >> 16) as u16 as i16,
                l as u16 as i16,
                (l >> 16) as u16 as i16,
            )
        })
        .collect();
    assert_eq!(wheels, vec![(120, 105, 206), (-120, 105, 206)]);

    // 负坐标（拖出窗口外）按 MAKELPARAM 截成低 16 位。
    rec.borrow_mut().0.clear();
    deliver_pointer(k, PANEL_POINTER_MOTION, 0, -1, -2);
    assert_eq!(rec.borrow().0, vec![(WM_MOUSEMOVE, 0, 0xFFFE_FFFF)]);

    // 未知事件 / 未知按键 / 未绑定的 kind：丢弃。
    rec.borrow_mut().0.clear();
    deliver_pointer(k, 99, 0, 0, 0);
    deliver_pointer(k, PANEL_POINTER_PRESS, 8, 0, 0);
    deliver_pointer(OVERLAY_KIND_SOFTKEYBOARD, PANEL_POINTER_MOTION, 0, 0, 0);
    assert!(rec.borrow().0.is_empty());
}

/// 处理器里回头查 `GetCursorPos` / `GetWindowRect`（工具栏拖动就这么做）不能撞上借用。
struct Reentrant(Vec<(i32, i32, RECT)>);
impl WindowMouse for Reentrant {
    fn on_message(&mut self, hwnd: HWND, _: u32, _: WPARAM, _: LPARAM) -> Option<LRESULT> {
        let mut p = POINT::default();
        let mut r = RECT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = GetWindowRect(hwnd, &mut r);
        }
        self.0.push((p.x, p.y, r));
        Some(LRESULT(0))
    }
}

#[test]
fn cursor_pos_tracks_last_pointer() {
    let _c = cap("c");
    let w = LayeredWindow::create(None, 30, 10, "WindInputSoftKeyboard").unwrap();
    w.update().unwrap();
    w.show(100, 200);
    let rec = Rc::new(RefCell::new(Reentrant(Vec::new())));
    w.register_mouse(rec.clone());
    deliver_pointer(OVERLAY_KIND_SOFTKEYBOARD, PANEL_POINTER_MOTION, 0, 5, 6);
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.unwrap();
    assert_eq!((p.x, p.y), (105, 206));
    let rect = RECT {
        left: 100,
        top: 200,
        right: 130,
        bottom: 210,
    };
    assert_eq!(
        rec.borrow().0,
        vec![(105, 206, rect)],
        "处理器里查到的是本次指针"
    );

    // 离开不挪「最后指针」。
    deliver_pointer(OVERLAY_KIND_SOFTKEYBOARD, PANEL_POINTER_LEAVE, 0, 0, 0);
    unsafe { GetCursorPos(&mut p) }.unwrap();
    assert_eq!((p.x, p.y), (105, 206));

    // addon 本地拖完同步位置：不推帧，之后的矩形与指针换算都按新位置。
    let n = frames(&_c).len();
    set_position(OVERLAY_KIND_SOFTKEYBOARD, 300, 400);
    assert_eq!(frames(&_c).len(), n, "set_position 不推帧");
    let mut r = RECT::default();
    unsafe { GetWindowRect(w.hwnd(), &mut r) }.unwrap();
    assert_eq!((r.left, r.top, r.right, r.bottom), (300, 400, 330, 410));
    deliver_pointer(OVERLAY_KIND_SOFTKEYBOARD, PANEL_POINTER_MOTION, 0, 1, 1);
    unsafe { GetCursorPos(&mut p) }.unwrap();
    assert_eq!((p.x, p.y), (301, 401));
}

#[test]
fn faded_update_scales_premultiplied_pixels() {
    let c = cap("a");
    let mut w = LayeredWindow::create(None, 1, 1, "WindInputToolbar").unwrap();
    w.buffer_mut().copy_from_slice(&[200, 100, 50, 255]);
    w.show(0, 0);
    assert!(frames(&c).is_empty(), "段里还没像素，不推空帧");
    w.update_with_alpha(128).unwrap();
    assert_eq!(frames(&c).len(), 1);
    assert_eq!(w.buffer(), &[200, 100, 50, 255], "窗口自己的缓冲不动");
    let shm_px = shm_pixels(OVERLAY_KIND_TOOLBAR).unwrap();
    assert_eq!(&shm_px[..4], &[100, 50, 25, 128]);
}
