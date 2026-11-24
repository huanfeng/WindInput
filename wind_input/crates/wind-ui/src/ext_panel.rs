//! Linux 外部宿主（Fcitx5 addon）形态的「外部面板」：悬浮工具栏 / 软键盘的 [`LayeredWindow`] 实现。
//!
//! `toolbar.rs` / `soft_keyboard.rs` 的布局、绘制、命中、状态机与 Windows 共用一套，只把窗口壳换掉：
//!
//! - **出**：`update` 把窗口缓冲写进该 kind 自己的 SHM 段（`endpoint::overlay_shm_name`，
//!   `_TB` / `_SKB`），可见时推一帧 `CMD_OVERLAY_FRAME`（落位 `EXACT`，内容盒取整幅位图，帧尾带拖动区）。
//!   `show` / `hide` 同样翻译成帧。
//! - **进**：addon 把窗口内的指针事件报上来（`CMD_PANEL_POINTER`），[`deliver_pointer`] 翻译成
//!   `WM_*` 交给该窗口注册的 [`WindowMouse`]，让现成的鼠标状态机原样跑。
//!
//! 窗口用 `HWND(kind)` 区分；非 Windows 的 `GetCursorPos` / `GetWindowRect` mock 在本形态下查这里
//! 记下的「最后指针屏幕坐标」与面板矩形（见 `sys.rs`）。
//!
//! 状态是线程局部的：窗口、`install` 与指针投递都在 forwarder 线程上。
//!
//! [`LayeredWindow`]: crate::window::LayeredWindow

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use wind_bridge::HostRenderSink;
use wind_bridge::shared_memory_posix::PosixSharedMemory;
use wind_ipc::codec::{OverlayFrameMeta, encode_overlay_frame};
use wind_ipc::protocol::overlay::{
    OVERLAY_KIND_SOFTKEYBOARD, OVERLAY_KIND_TOOLBAR, OVERLAY_PLACE_EXACT,
};
use wind_ipc::protocol::panel_pointer::*;
use wind_ipc::protocol::{MAX_SHARED_RENDER_SIZE, SharedRenderHeader};

use crate::sys::{
    HWND, LPARAM, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSELEAVE,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WPARAM,
};
use crate::window::WindowMouse;

/// 类名 → 浮层 kind。类名与 `toolbar.rs` / `soft_keyboard.rs` 传给 `create` 的一致。
fn kind_for_class(class_name: &str) -> Option<u32> {
    match class_name {
        "WindInputToolbar" => Some(OVERLAY_KIND_TOOLBAR),
        "WindInputSoftKeyboard" => Some(OVERLAY_KIND_SOFTKEYBOARD),
        _ => None,
    }
}

struct Panel {
    /// 绑定这个 kind 的窗口实例号。同 kind 新建窗口会顶替旧的（字段整体赋值是先建新、后 drop
    /// 旧），旧窗口 drop 时据此不误删新的注册。
    owner: u64,
    shm: Option<PosixSharedMemory>,
    /// 最近写进段里那帧的 `(seq, 宽, 高)`；`None` = 还没写过像素，`show` 不推空帧。
    frame: Option<(u32, u32, u32)>,
    x: i32,
    y: i32,
    visible: bool,
    drag: Vec<[i32; 4]>,
    mouse: Option<Rc<RefCell<dyn WindowMouse>>>,
}

#[derive(Default)]
struct Host {
    sink: Option<Arc<dyn HostRenderSink>>,
    suffix: String,
    panels: HashMap<u32, Panel>,
    next_owner: u64,
    /// addon 最近报来的指针屏幕坐标（面板位置 + 面板内坐标），对位 Win32 的 `GetCursorPos`。
    last_pointer: Option<(i32, i32)>,
    /// 指针此刻在哪个面板里：除 LEAVE 外的指针事件置为该 kind，该 kind 的 LEAVE 清掉。
    /// 「最后指针」在 LEAVE 后保持不动（离开坐标没有意义），要知道指针还在不在面板里得看这里。
    pointer_in: Option<u32>,
}

thread_local! {
    static HOST: RefCell<Host> = RefCell::new(Host::default());
}

/// Forwarder 启动时调用一次：之后本线程上建的工具栏 / 软键盘窗口经 `sink` 推帧，SHM 段名按 `suffix`。
/// 装之前这两类窗口只记状态、不推帧。
pub fn install(sink: Arc<dyn HostRenderSink>, suffix: &str) {
    HOST.with(|h| {
        let mut h = h.borrow_mut();
        h.sink = Some(sink);
        h.suffix = suffix.to_string();
    });
}

/// addon 本地拖完后同步服务端记下的位置（不推帧：addon 那边窗口已经在那儿了）。
pub fn set_position(kind: u32, x: i32, y: i32) {
    HOST.with(|h| {
        if let Some(p) = h.borrow_mut().panels.get_mut(&kind) {
            (p.x, p.y) = (x, y);
        }
    });
}

/// addon 的 push 连接（重）建立后调用：此刻可见、段里已有像素的面板原样补推一帧可见帧（不重写段，
/// 像素还在原处）。addon 收到 SERVICE_READY 就丢掉手里的面板，同一服务换了连接时靠这一步补回。
pub fn resend_visible() {
    HOST.with(|h| {
        let h = h.borrow();
        let Some(sink) = h.sink.as_ref() else {
            return;
        };
        let mut kinds: Vec<_> = h.panels.keys().copied().collect();
        kinds.sort_unstable();
        for kind in kinds {
            let p = &h.panels[&kind];
            if let (true, Some((seq, w, ht))) = (p.visible, p.frame) {
                sink.push_frame(&encode_overlay_frame(&visible_meta(kind, p, seq, w, ht)));
            }
        }
    });
}

/// `CMD_PANEL_POINTER` 事件 → `(msg, wparam)`。`button` 是 X11 按键号（1 左 / 2 中 / 3 右，
/// 滚轮 4 上 / 5 下）。认不出的组合返回 `None`。
fn pointer_message(event: u32, button: u32) -> Option<(u32, usize)> {
    let msg = match (event, button) {
        (PANEL_POINTER_MOTION, _) => WM_MOUSEMOVE,
        (PANEL_POINTER_PRESS, 1) => WM_LBUTTONDOWN,
        (PANEL_POINTER_PRESS, 2) => WM_MBUTTONDOWN,
        (PANEL_POINTER_PRESS, 3) => WM_RBUTTONDOWN,
        (PANEL_POINTER_RELEASE, 1) => WM_LBUTTONUP,
        (PANEL_POINTER_RELEASE, 2) => WM_MBUTTONUP,
        (PANEL_POINTER_RELEASE, 3) => WM_RBUTTONUP,
        (PANEL_POINTER_LEAVE, _) => WM_MOUSELEAVE,
        // 滚轮量在 wparam 高字（`GET_WHEEL_DELTA_WPARAM`），向上（远离用户）为正。
        (PANEL_POINTER_WHEEL, 4) => return Some((WM_MOUSEWHEEL, (120u16 as usize) << 16)),
        (PANEL_POINTER_WHEEL, 5) => {
            return Some((WM_MOUSEWHEEL, ((-120i16) as u16 as usize) << 16));
        }
        _ => return None,
    };
    Some((msg, 0))
}

/// `MAKELPARAM(x, y)`：低字 x、高字 y，各取低 16 位（负坐标的补码截断，处理器按 `i16` 取回）。
fn make_lparam(x: i32, y: i32) -> LPARAM {
    LPARAM((((y as u16 as u32) << 16) | x as u16 as u32) as isize)
}

/// addon 报来的指针事件 → 该 kind 注册的 [`WindowMouse::on_message`]。坐标是位图内坐标，
/// 与 Windows 客户区坐标同义；唯独 `WM_MOUSEWHEEL` 按 Win32 约定给屏幕坐标（窗口位置 +
/// 位图内坐标），Windows 的鼠标处理代码因此原样可用。未绑定的 kind、认不出的事件丢弃。
pub fn deliver_pointer(kind: u32, event: u32, button: u32, x: i32, y: i32) {
    let Some((msg, wparam)) = pointer_message(event, button) else {
        tracing::debug!("外部面板 kind={kind}：丢弃未知指针事件 event={event} button={button}");
        return;
    };
    // 处理器在借用释放后再调：它会回头查 `GetCursorPos` / `GetWindowRect`。
    let Some((handler, (px, py))) = HOST.with(|h| {
        let mut h = h.borrow_mut();
        let p = h.panels.get(&kind)?;
        let (px, py) = (p.x, p.y);
        let handler = p.mouse.clone();
        // 离开事件的坐标没有意义（addon 可能报 0），不拿它挪「最后指针」。
        if event != PANEL_POINTER_LEAVE {
            h.last_pointer = Some((px + x, py + y));
            h.pointer_in = Some(kind);
        } else if h.pointer_in == Some(kind) {
            h.pointer_in = None;
        }
        Some((handler, (px, py)))
    }) else {
        return;
    };
    let lparam = if msg == WM_MOUSEWHEEL {
        make_lparam(px + x, py + y)
    } else {
        make_lparam(x, y)
    };
    if let Some(m) = handler {
        m.borrow_mut()
            .on_message(HWND(kind as isize), msg, WPARAM(wparam), lparam);
    }
}

/// 非 Windows `GetCursorPos` mock 的数据源。
pub(crate) fn cursor_pos() -> Option<(i32, i32)> {
    HOST.with(|h| h.borrow().last_pointer)
}

/// 指针此刻是否在外部面板 `hwnd` 里（见 `Host::pointer_in`）。
pub(crate) fn pointer_inside(hwnd: HWND) -> bool {
    HOST.with(|h| h.borrow().pointer_in.is_some_and(|k| k as isize == hwnd.0))
}

/// 非 Windows `GetWindowRect` mock 的数据源：外部面板 `HWND(kind)` 当前的 `(左, 上, 右, 下)`，
/// 尺寸取最近写进段里的那帧。
pub(crate) fn window_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    let kind = u32::try_from(hwnd.0).ok()?;
    HOST.with(|h| {
        let h = h.borrow();
        let p = h.panels.get(&kind)?;
        let (w, ht) = p.frame.map(|(_, w, ht)| (w, ht)).unwrap_or((0, 0));
        Some((p.x, p.y, p.x + w as i32, p.y + ht as i32))
    })
}

/// 可见帧的元数据：`EXACT` 落在 `(x, y)`，内容盒取整幅位图，带拖动区。
fn visible_meta(kind: u32, p: &Panel, seq: u32, w: u32, h: u32) -> OverlayFrameMeta {
    OverlayFrameMeta {
        kind,
        seq,
        width: w,
        height: h,
        flags: SharedRenderHeader::FLAG_VISIBLE | SharedRenderHeader::FLAG_CONTENT_READY,
        place: OVERLAY_PLACE_EXACT,
        x: p.x,
        y: p.y,
        alt_x: p.x,
        alt_y: p.y,
        content_w: w,
        content_h: h,
        scale: 1.0,
        drag: p.drag.clone(),
        ..Default::default()
    }
}

/// 窗口壳持有的绑定：`LayeredWindow` 按类名建出它，drop 时注销（可见则先推隐藏帧）。
pub(crate) struct Binding {
    kind: u32,
    owner: u64,
}

impl Binding {
    /// 工具栏 / 软键盘的类名绑定对应 kind；其它类名返回 `None`（窗口照旧是 mock）。
    pub(crate) fn for_class(class_name: &str) -> Option<Self> {
        let kind = kind_for_class(class_name)?;
        let owner = HOST.with(|h| {
            let mut h = h.borrow_mut();
            h.next_owner += 1;
            let owner = h.next_owner;
            h.panels.insert(
                kind,
                Panel {
                    owner,
                    shm: None,
                    frame: None,
                    x: 0,
                    y: 0,
                    visible: false,
                    drag: Vec::new(),
                    mouse: None,
                },
            );
            owner
        });
        Some(Self { kind, owner })
    }

    pub(crate) fn hwnd(&self) -> HWND {
        HWND(self.kind as isize)
    }

    /// 在本绑定仍有效时操作面板（被同 kind 的新窗口顶替后静默不做）。
    fn with<R>(&self, f: impl FnOnce(&mut Host) -> Option<R>) -> Option<R> {
        HOST.with(|h| {
            let mut h = h.borrow_mut();
            if h.panels.get(&self.kind)?.owner != self.owner {
                return None;
            }
            f(&mut h)
        })
    }

    pub(crate) fn register_mouse(&self, handler: Rc<RefCell<dyn WindowMouse>>) {
        self.with(|h| {
            h.panels.get_mut(&self.kind)?.mouse = Some(handler);
            Some(())
        });
    }

    pub(crate) fn set_drag_regions(&self, rects: Vec<[i32; 4]>) {
        self.with(|h| {
            h.panels.get_mut(&self.kind)?.drag = rects;
            Some(())
        });
    }

    /// 写像素进段；可见时推帧。`alpha` 是整窗常数透明度（淡出用），按 `UpdateLayeredWindow`
    /// 的语义与预乘像素叠乘——四个通道同乘，窗口自己的缓冲不动。
    pub(crate) fn update(&self, buf: &[u8], w: u32, ht: u32, alpha: u8) -> Result<(), String> {
        let n = w as usize * ht as usize * 4;
        if n > MAX_SHARED_RENDER_SIZE - SharedRenderHeader::SIZE || buf.len() < n {
            return Err(format!("外部面板 kind={} 位图尺寸不对 {w}x{ht}", self.kind));
        }
        let faded: Vec<u8>;
        let px = if alpha == 255 {
            &buf[..n]
        } else {
            let a = alpha as u32;
            faded = buf[..n]
                .iter()
                .map(|&c| ((c as u32 * a + 127) / 255) as u8)
                .collect();
            &faded
        };
        let kind = self.kind;
        self.with(|h| {
            let sink = h.sink.clone()?;
            let suffix = h.suffix.clone();
            let p = h.panels.get_mut(&kind)?;
            if p.shm.is_none() {
                p.shm = Some(crate::overlay_host::create_layer_shm(&suffix, kind)?);
            }
            let seq = p.shm.as_mut()?.write_frame(0, 0, w, ht, px);
            p.frame = Some((seq, w, ht));
            if p.visible {
                sink.push_frame(&encode_overlay_frame(&visible_meta(kind, p, seq, w, ht)));
            }
            Some(())
        });
        Ok(())
    }

    /// 记下位置并显示。段里还没像素时只记状态，下一次 `update` 带出可见帧。
    pub(crate) fn show(&self, x: i32, y: i32) {
        let kind = self.kind;
        self.with(|h| {
            let sink = h.sink.clone();
            let p = h.panels.get_mut(&kind)?;
            (p.x, p.y, p.visible) = (x, y, true);
            let (seq, w, ht) = p.frame?;
            sink?.push_frame(&encode_overlay_frame(&visible_meta(kind, p, seq, w, ht)));
            Some(())
        });
    }

    /// 推隐藏帧。本就没显示则什么都不发（同 `overlay_host`）。
    pub(crate) fn hide(&self) {
        self.with(|h| hide_panel(h, self.kind));
    }
}

fn hide_panel(h: &mut Host, kind: u32) -> Option<()> {
    let sink = h.sink.clone();
    let p = h.panels.get_mut(&kind)?;
    if !p.visible {
        return None;
    }
    p.visible = false;
    let seq = p.shm.as_mut().map(|s| s.write_hidden()).unwrap_or(0);
    sink?.push_frame(&encode_overlay_frame(&OverlayFrameMeta {
        kind,
        seq,
        ..Default::default()
    }));
    Some(())
}

impl Drop for Binding {
    fn drop(&mut self) {
        // 线程退出时 thread_local 可能已析构，`try_with` 不 panic。
        let _ = HOST.try_with(|h| {
            let mut h = h.borrow_mut();
            if h.panels.get(&self.kind).map(|p| p.owner) != Some(self.owner) {
                return;
            }
            hide_panel(&mut h, self.kind);
            h.panels.remove(&self.kind);
        });
    }
}

/// 测试用：该 kind 段里当前的像素。
#[cfg(test)]
fn shm_pixels(kind: u32) -> Option<Vec<u8>> {
    HOST.with(|h| {
        Some(
            h.borrow()
                .panels
                .get(&kind)?
                .shm
                .as_ref()?
                .pixels()
                .to_vec(),
        )
    })
}

#[cfg(test)]
#[path = "ext_panel_tests.rs"]
mod tests;
