//! macOS 原生浮动面板（NSPanel），由服务进程自己开窗。
//!
//! macOS 侧候选窗归 `.app`（要跟随 IMKit 给的 caret），软键盘不跟随任何东西，故破例在服务进程
//! 开窗，好让 `soft_keyboard.rs` 的跨平台布局/绘制/交互原样复用。服务进程已由
//! `global_hotkey_macos::run_main_loop` 提升为 UIElement 应用并跑着 Carbon 事件循环，开得了窗。
//!
//! 约定：
//! - 一切调用都在主线程（AppKit 要求），`softkeyboard_host_macos` 负责转运命令；
//!   拿不到 `MainThreadMarker` 就构造不出面板。
//! - `SoftKeyboard` 全程用 Win32 式坐标（设备像素、主屏左上原点），与 AppKit（点、主屏左下原点）
//!   的换算只在本模块发生，漏出去就是拖动跳位、多显示器点不准。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent, NSPanel,
    NSScreen, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize};

use core_graphics::base::{
    kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedFirst, kCGRenderingIntentDefault,
};
use core_graphics::color_space::CGColorSpace;
use core_graphics::data_provider::CGDataProvider;
use core_graphics::image::CGImage;
use foreign_types::ForeignType;

/// 取得（必要时初始化）本进程的 `NSApplication`。必须在主线程调用。
///
/// 服务没走 `NSApplicationMain`，NSApp 要自己建；`run_main_loop` 与 [`MacPanel::create`] 都经这里，
/// 免得激活策略分叉。策略取 `Accessory`（不进 Dock、不占菜单栏），与 `TRANSFORM_TO_UI_ELEMENT` 一致。
pub fn ensure_app() -> Retained<NSApplication> {
    let mtm =
        MainThreadMarker::new().expect("NSApplication 只能在主线程取用；调用方漏了主线程约定");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app
}

/// 鼠标事件的种类。与 Win32 消息的对位见 `soft_keyboard.rs` 的 `mouse_macos`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    /// 光标在面板内移动（未按住）。
    Move,
    /// 左键按下。
    Down,
    /// 左键按住并移动。
    Drag,
    /// 左键抬起。
    Up,
    /// 光标离开面板。移动事件只在视图内才到，缺了它划出面板后最后一格高亮会一直亮着。
    Leave,
    /// 滚轮。
    Wheel,
}

/// 一次鼠标事件。坐标一律是设备像素。
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub kind: MouseKind,
    /// 面板客户区坐标，原点在面板左上（视图 `isFlipped` 保证 y 向下）。
    pub x: f32,
    pub y: f32,
    /// 全局光标坐标，原点在主屏左上。
    pub sx: i32,
    pub sy: i32,
    /// 滚轮格数（正 = 向前滚）。仅 [`MouseKind::Wheel`] 有意义。
    pub wheel: f32,
}

/// 面板当前几何（设备像素，左上原点）。交给鼠标处理器算拖动落点用。
#[derive(Debug, Clone, Copy)]
pub struct PanelGeom {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// 面板鼠标处理器。
///
/// `on_mouse` 返回 `Some((x, y))` 表示「请把面板挪到这里」（设备像素、左上原点）。
/// 返回意图而不自己调 AppKit，拖动算术才能在纯 Rust 里测。
pub trait PanelMouse {
    fn on_mouse(&mut self, ev: MouseEvent, geom: PanelGeom) -> Option<(i32, i32)>;

    /// 面板钳进可见区后实际落到的位置（设备像素、左上原点）。
    ///
    /// 处理器必须记这个而非 `on_mouse` 想去的位置，否则下一帧 `show` 会把面板弹回屏外。
    fn on_moved(&mut self, x: i32, y: i32);
}

/// 视图的 Rust 侧状态。
struct ViewState {
    mouse: RefCell<Option<Rc<RefCell<dyn PanelMouse>>>>,
    /// 本视图所在屏的 backing 倍率。事件坐标要乘它换成设备像素。
    scale: RefCell<f64>,
    /// 已装的跟踪区（换尺寸时要撤旧装新，否则 Move/Leave 的有效范围会停在旧尺寸上）。
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "WindInputSoftKeyboardView"]
    #[ivars = ViewState]
    struct PanelView;

    unsafe impl NSObjectProtocol for PanelView {}

    impl PanelView {
        /// y 轴向下：`layer.contents` 的 CGImage 首行在顶，`convertPoint:fromView:` 的 y 也从顶量起。
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// 面板不激活；不返回 true 的话第一次点击会被系统吃掉用于激活窗口。
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.dispatch(MouseKind::Down, event);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.dispatch(MouseKind::Up, event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.dispatch(MouseKind::Drag, event);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.dispatch(MouseKind::Move, event);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            self.dispatch(MouseKind::Leave, event);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            self.dispatch(MouseKind::Wheel, event);
        }
    }
);

impl PanelView {
    fn new(mtm: MainThreadMarker, frame: NSRect, scale: f64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewState {
            mouse: RefCell::new(None),
            scale: RefCell::new(scale),
            tracking: RefCell::new(None),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        // 用图层承载像素：`layer.contents` 直接吃 CGImage，免得 drawRect 引入与
        // core-graphics 不通用的 objc2-core-graphics 类型。
        this.setWantsLayer(true);
        this.retune_tracking();
        this
    }

    /// 重装跟踪区。带了 `InVisibleRect` 本可不重装；`resize` 仍调一次，保证跟踪范围恒等于面板大小。
    fn retune_tracking(&self) {
        let st = self.ivars();
        if let Some(old) = st.tracking.borrow_mut().take() {
            self.removeTrackingArea(&old);
        }
        let opts = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            // 面板永远不是 key window，非 ActiveAlways 收不到移动事件。
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: owner 传的是本视图自身，其生命周期覆盖跟踪区（换尺寸时先撤旧再装新，
        // 视图析构时 AppKit 一并释放），userInfo 为空。
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                self.bounds(),
                opts,
                Some(self),
                None,
            )
        };
        self.addTrackingArea(&area);
        *st.tracking.borrow_mut() = Some(area);
    }

    fn set_scale(&self, scale: f64) {
        *self.ivars().scale.borrow_mut() = scale;
    }

    fn set_mouse(&self, handler: Rc<RefCell<dyn PanelMouse>>) {
        *self.ivars().mouse.borrow_mut() = Some(handler);
    }

    /// 把一个 AppKit 事件翻成 [`MouseEvent`] 交给处理器，并执行它要求的挪窗。
    ///
    /// 处理器一律 `try_borrow_mut`，借不到就丢掉这次事件：tick 期间处理器已借出，AppKit
    /// 调用可能重入 run loop 把事件送进来，`borrow_mut` 会 panic 掉整个服务。丢一次会被下次纠正。
    fn dispatch(&self, kind: MouseKind, event: &NSEvent) {
        let st = self.ivars();
        let handler = match st.mouse.try_borrow().ok().and_then(|m| m.clone()) {
            Some(h) => h,
            None => return,
        };
        let scale = *st.scale.borrow();
        // 客户区坐标：窗口坐标 → 本视图坐标。视图 isFlipped ⇒ y 已从顶量起。
        let local = self.convertPoint_fromView(event.locationInWindow(), None);
        // 全局光标：AppKit 是主屏左下原点，翻成主屏左上原点。
        let (sx, sy) = global_cursor_px(scale);
        let wheel = if kind == MouseKind::Wheel {
            wheel_lines(event)
        } else {
            0.0
        };
        let ev = MouseEvent {
            kind,
            x: (local.x * scale) as f32,
            y: (local.y * scale) as f32,
            sx,
            sy,
            wheel,
        };
        let geom = match self.window() {
            Some(w) => window_geom_px(&w, scale),
            None => return,
        };
        let Ok(mut h) = handler.try_borrow_mut() else {
            return;
        };
        let want = h.on_mouse(ev, geom);
        drop(h);
        if let Some((x, y)) = want
            && let Some(w) = self.window()
        {
            set_window_origin_px(&w, x, y, scale);
            // 钳制后再量一次，回报真实落点（见 `PanelMouse::on_moved`）。
            let g = window_geom_px(&w, scale);
            if let Ok(mut h) = handler.try_borrow_mut() {
                h.on_moved(g.x, g.y);
            }
        }
    }
}

/// 滚轮增量换算成「格」。
///
/// 触控板（`hasPreciseScrollingDeltas`）一次轻扫来几十个以点计的小 delta，要按行折算。
fn wheel_lines(event: &NSEvent) -> f32 {
    let dy = event.scrollingDeltaY();
    if event.hasPreciseScrollingDeltas() {
        // 精确滚动的单位是点，约定 16 点 ≈ 一格（与 AppKit 默认行高同量级）。
        (dy / 16.0) as f32
    } else {
        dy as f32
    }
}

/// 主屏（AppKit 全局坐标系的基准屏）的高度，单位点。
///
/// 取 `screens[0]` 而非 `NSScreen.main`：全局坐标原点恒在 0 号屏左下角，`main` 会随 key window 换屏。
fn primary_height_pt(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm)
        .iter()
        .next()
        .map(|s| s.frame().size.height)
        .unwrap_or(0.0)
}

/// 全局光标位置，换成「设备像素 + 主屏左上原点」。
pub fn global_cursor_px(scale: f64) -> (i32, i32) {
    let Some(mtm) = MainThreadMarker::new() else {
        return (0, 0);
    };
    let p = NSEvent::mouseLocation();
    let top = primary_height_pt(mtm);
    (
        (p.x * scale).round() as i32,
        ((top - p.y) * scale).round() as i32,
    )
}

/// 窗口几何 → 设备像素、左上原点。
fn window_geom_px(win: &NSWindow, scale: f64) -> PanelGeom {
    let Some(mtm) = MainThreadMarker::new() else {
        return PanelGeom {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
    };
    let f = win.frame();
    let top = primary_height_pt(mtm);
    PanelGeom {
        x: (f.origin.x * scale).round() as i32,
        // AppKit 的 origin.y 是窗口底边到主屏底边的距离。
        y: ((top - f.origin.y - f.size.height) * scale).round() as i32,
        w: (f.size.width * scale).round() as u32,
        h: (f.size.height * scale).round() as u32,
    }
}

/// 把窗口挪到「设备像素 + 左上原点」给出的位置，并钳进可见区。
///
/// 可见区（`visibleFrame`，已扣菜单栏与 Dock）只有这里问得到；不钳的话面板拖出屏外就抓不回来。
fn set_window_origin_px(win: &NSWindow, x: i32, y: i32, scale: f64) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let f = win.frame();
    let (w_pt, h_pt) = (f.size.width, f.size.height);
    let (mut x_pt, mut y_top_pt) = (x as f64 / scale, y as f64 / scale);

    // 钳到落点所在屏的可见区，按当前位置钳的话跨屏拖动会被拽回来。
    let top = primary_height_pt(mtm);
    let probe = NSPoint::new(x_pt, top - y_top_pt);
    let screens = NSScreen::screens(mtm);
    let vis = screens
        .iter()
        .find(|s| {
            let fr = s.frame();
            probe.x >= fr.origin.x
                && probe.x < fr.origin.x + fr.size.width
                && probe.y >= fr.origin.y
                && probe.y < fr.origin.y + fr.size.height
        })
        .or_else(|| screens.iter().next())
        .map(|s| s.visibleFrame());
    if let Some(v) = vis {
        let v_top = top - (v.origin.y + v.size.height);
        x_pt = x_pt.clamp(
            v.origin.x,
            (v.origin.x + v.size.width - w_pt).max(v.origin.x),
        );
        y_top_pt = y_top_pt.clamp(v_top, (v_top + v.size.height - h_pt).max(v_top));
    }
    win.setFrameOrigin(NSPoint::new(x_pt, top - y_top_pt - h_pt));
}

/// 主屏可见区（已扣菜单栏与 Dock），设备像素、左上原点：`(x, y, w, h)`。
///
/// 供 `soft_keyboard::default_origin` 决定首次显示的落点。
pub fn work_area_px(scale: f64) -> Option<(i32, i32, u32, u32)> {
    let mtm = MainThreadMarker::new()?;
    let screens = NSScreen::screens(mtm);
    let s = screens.iter().next()?;
    let v = s.visibleFrame();
    let top = primary_height_pt(mtm);
    Some((
        (v.origin.x * scale).round() as i32,
        ((top - (v.origin.y + v.size.height)) * scale).round() as i32,
        (v.size.width * scale).round() as u32,
        (v.size.height * scale).round() as u32,
    ))
}

/// macOS 原生浮动面板。方法集与 `window::LayeredWindow` 同形，`soft_keyboard.rs` 只换类型别名。
pub struct MacPanel {
    panel: Retained<NSPanel>,
    view: Retained<PanelView>,
    mtm: MainThreadMarker,
    width: u32,
    height: u32,
    /// BGRA 预乘像素缓冲区（与 Windows 侧 `LayeredWindow` 同一约定）。
    buffer: Vec<u8>,
    /// 面板当前所在屏的 backing 倍率。`show(&self)` 每次都要重采（跨屏后会变），故用 `Cell`。
    scale: Cell<f64>,
}

impl MacPanel {
    /// 建面板。不在主线程直接报错，不静默降级。
    /// 参数表与 `window::LayeredWindow::create` 逐位对齐（含无意义的 `parent`），调用点各平台同一行。
    pub fn create(
        _parent: Option<crate::sys::HWND>,
        width: u32,
        height: u32,
        _class_name: &str,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "软键盘面板只能在主线程创建（当前不在主线程）".to_string())?;

        // 幂等兜底：单测/示例可能在进事件循环前建面板。
        let _ = ensure_app();

        let scale = f64::from(crate::dpi::scale_for_point(0, 0));
        let (w_pt, h_pt) = (f64::from(width) / scale, f64::from(height) / scale);
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w_pt, h_pt));

        // NonactivatingPanel（对位 `WS_EX_NOACTIVATE`）是前提：面板可激活的话点键就改变焦点，
        // 会触发 `close_softkeyboard_on_focus_change` 把自己关掉。
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setHidesOnDeactivate(false);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        // 位图自带圆角，系统阴影沿矩形投影会露出直角。
        panel.setHasShadow(false);
        // 跟随 Space 切换，也能出现在全屏应用之上。
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );
        // 对位 HWND_TOPMOST。别再调 `setFloatingPanel(true)`，它会把 level 冲回 3，
        // 见 docs/architecture/ui-platform-notes.md 的「macOS 面板」。
        panel.setLevel(POP_UP_MENU_WINDOW_LEVEL);

        let view = PanelView::new(mtm, rect, scale);
        panel.setContentView(Some(&view));

        Ok(Self {
            panel,
            view,
            mtm,
            width,
            height,
            buffer: vec![0u8; (width as usize) * (height as usize) * 4],
            scale: Cell::new(scale),
        })
    }

    /// 装鼠标处理器。
    pub fn register_mouse(&self, handler: Rc<RefCell<dyn PanelMouse>>) {
        self.view.set_mouse(handler);
    }

    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    pub fn buffer_mut(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        self.buffer
            .resize((width as usize) * (height as usize) * 4, 0);
        let s = self.scale.get();
        let (w_pt, h_pt) = (f64::from(width) / s, f64::from(height) / s);
        // 保持左上角不动；AppKit 的 origin 是左下角，直接 setContentSize 会让面板向上长。
        let f = self.panel.frame();
        let top_y = f.origin.y + f.size.height;
        self.panel.setFrame_display(
            NSRect::new(
                NSPoint::new(f.origin.x, top_y - h_pt),
                NSSize::new(w_pt, h_pt),
            ),
            false,
        );
        self.view.retune_tracking();
    }

    pub fn clear(&mut self) {
        self.buffer.fill(0);
    }

    /// 把缓冲区贴上屏。对位 Windows 侧的 `UpdateLayeredWindow`。
    pub fn update(&self) -> Result<(), String> {
        let n = (self.width as usize) * (self.height as usize) * 4;
        if self.buffer.len() < n || n == 0 {
            return Err(format!(
                "缓冲区与尺寸不符: {} < {}x{}x4",
                self.buffer.len(),
                self.width,
                self.height
            ));
        }
        let provider = CGDataProvider::from_buffer(std::sync::Arc::new(self.buffer[..n].to_vec()));
        let cs = CGColorSpace::create_device_rgb();
        // BGRA 预乘在 CG 里是「小端 32 位 + Alpha 在前」；须与 `text/coretext.rs` 用同一组常量。
        let bitmap_info = kCGBitmapByteOrder32Little | kCGImageAlphaPremultipliedFirst;
        let img = CGImage::new(
            self.width as usize,
            self.height as usize,
            8,
            32,
            (self.width as usize) * 4,
            &cs,
            bitmap_info,
            &provider,
            false,
            kCGRenderingIntentDefault,
        );
        // 裸 `msg_send` 取图层，免得为两个 setter 引入 objc2-quartz-core。
        let layer: *mut AnyObject = unsafe { msg_send![&*self.view, layer] };
        if layer.is_null() {
            return Err("面板视图没有图层（setWantsLayer 未生效）".into());
        }
        unsafe {
            // contentsScale 必须跟上 backing 倍率，否则 Retina 上位图被拉伸两倍 ⇒ 糊。
            let _: () = msg_send![layer, setContentsScale: self.scale.get()];
            let obj: *const AnyObject = img.as_ptr().cast();
            let _: () = msg_send![layer, setContents: obj];
        }
        Ok(())
    }

    /// 与 `LayeredWindow::update_with_alpha` 同形。软键盘不用整窗透明度，原样转发。
    pub fn update_with_alpha(&self, _alpha: u8) -> Result<(), String> {
        self.update()
    }

    /// 显示到指定位置（设备像素、左上原点）。
    pub fn show(&self, x: i32, y: i32) {
        // 每帧重采倍率，跨屏拖动后下一帧即校正，无需监听换屏。
        self.refresh_scale();
        set_window_origin_px(&self.panel, x, y, self.scale.get());
        // 不用 makeKeyAndOrderFront：会抢焦点。
        self.panel.orderFront(None);
    }

    pub fn hide(&self) {
        self.panel.orderOut(None);
    }

    /// 当前左上角（设备像素、左上原点）。
    pub fn origin_px(&self) -> (i32, i32) {
        let g = window_geom_px(&self.panel, self.scale.get());
        (g.x, g.y)
    }

    /// 重采面板所在屏的 backing 倍率。直接问 `NSWindow.screen`，不走 `dpi::scale_for_point`：
    /// 手上的坐标是用可能过期的倍率换算的，拿它查屏在混合 DPI 下会查错。
    fn refresh_scale(&self) {
        let Some(screen) = self.panel.screen() else {
            return;
        };
        let s = screen.backingScaleFactor();
        if s > 0.0 && (s - self.scale.get()).abs() > 0.01 {
            self.scale.set(s);
            self.view.set_scale(s);
        }
    }

    pub fn capture_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        crate::screenshot::save_bgra_to_png(&self.buffer, self.width, self.height, path)
    }

    pub fn capture_to_clipboard(&self) -> Result<(), String> {
        crate::screenshot::copy_bgra_to_clipboard(&self.buffer, self.width, self.height)
    }

    /// 与 `LayeredWindow::hwnd` 同形的占位。macOS 无 HWND，恒返回默认值——
    /// `SoftMouse::hwnd` 在本平台不参与任何判断（拖动走的是 [`PanelMouse`] 的返回值）。
    pub fn hwnd(&self) -> crate::sys::HWND {
        let _ = self.mtm;
        crate::sys::HWND::default()
    }
}

/// `NSPopUpMenuWindowLevel`（101），与 .app 侧候选窗同层。浮动层(3)会被 Spotlight（23）盖住，
/// 见 docs/architecture/ui-platform-notes.md 的「macOS 面板」。
const POP_UP_MENU_WINDOW_LEVEL: isize = 101;

/// 物理 Shift 是否按住、大写锁定是否开着。
///
/// 对位 Windows 侧 `read_shift_caps` 的 `GetKeyState`。用类方法 `NSEvent::modifierFlags`
/// 查当前状态，无需额外授权；调用点 `tick` 本在主线程。
pub fn shift_caps_state() -> Option<(bool, bool)> {
    use objc2_app_kit::NSEventModifierFlags;
    MainThreadMarker::new()?;
    let f = NSEvent::modifierFlags_class();
    Some((
        f.contains(NSEventModifierFlags::Shift),
        f.contains(NSEventModifierFlags::CapsLock),
    ))
}

/// 系统键盘重复设置 →（首次延迟 ms, 重复间隔 ms）。
///
/// 读 `NSGlobalDomain` 的 `InitialKeyRepeat` / `KeyRepeat`（单位 1/60 秒）。
/// 未设置时读回 0，返回 `None` 让调用方兜底，0 当真值会变成按下即无限连发。
pub fn key_repeat_params() -> Option<(u64, u64)> {
    use objc2_foundation::{NSString, NSUserDefaults};
    let d = NSUserDefaults::standardUserDefaults();
    let initial = d.doubleForKey(&NSString::from_str("InitialKeyRepeat"));
    let repeat = d.doubleForKey(&NSString::from_str("KeyRepeat"));
    if initial <= 0.0 || repeat <= 0.0 {
        return None;
    }
    let ms = |ticks: f64| (ticks * 1000.0 / 60.0).round().max(1.0) as u64;
    Some((ms(initial), ms(repeat).max(15)))
}
