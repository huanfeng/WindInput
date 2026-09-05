//! macOS 原生浮动面板（NSPanel）——**服务进程自己的窗口**。
//!
//! # 它在架构里的位置
//!
//! 本仓 macOS 侧此前的规矩是「服务进程只光栅化，窗口一律归 `.app`」：候选窗的像素在
//! 服务进程画好，经 POSIX SHM 推给 `.app` 的 NSPanel 呈现（见 `manager_macos` 模块头）。
//! 软键盘是**第一个例外**，理由是它与那条管线的前提不同：
//!
//! - 候选窗要跟随 caret，而 caret 坐标只有 `.app` 从 IMKit 拿得到 ⇒ 窗口归 `.app` 顺理成章；
//! - 软键盘**不跟随任何东西**，它是用户自己拖到某处的常驻浮层，与 IMKit 毫无关系。
//!
//! 于是让服务进程直接开窗，`soft_keyboard.rs` 那 1900 行布局/绘制/命中/交互（全部是
//! 跨平台的 tiny-skia + View 树）就能原样复用，不必在 Swift 侧再实现第二份。
//!
//! # 为什么服务进程开得了窗
//!
//! 它已经是个有窗口服务器连接的 GUI 进程了，只是至今没开过窗：
//! `global_hotkey_macos::run_main_loop` 里的 `TransformProcessType(TRANSFORM_TO_UI_ELEMENT)`
//! 早就把它提升成了 UIElement 应用（那一步的注释写着「不做它进程不出现在 `lsappinfo list`
//! 里」，且 Carbon 全局热键实测生效 ⇒ 连接是通的），主线程跑的又是
//! `RunApplicationEventLoop()`——Carbon 的**应用**事件循环，本身就在驱动 CFRunLoop。
//!
//! # ⚠️ 主线程约定
//!
//! AppKit 的窗口/视图**只能在主线程**创建与改动。本模块的一切都假定调用方在主线程，
//! 由 `softkeyboard_host_macos` 负责把 forwarder 工作线程的命令转运过来。
//! 类型上也焊死了：`MainThreadOnly` 的 `MainThreadMarker` 拿不到就构造不出面板。
//!
//! # ⚠️ 坐标系换算收口在本模块
//!
//! `SoftKeyboard` 全程用 **Win32 式坐标**：设备像素、原点在主屏**左上**（`default_origin`
//! 给的是它，`hit_at` 吃的是它，`origin` 存的是它）。AppKit 用的是**点**、原点在主屏
//! **左下**。两者的换算只在本模块发生——一旦漏进面板逻辑，就会变成「拖一下跳到别处」
//! 或「多显示器上点不准」这类极难追的错位。

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

/// 取得（必要时初始化）本进程的 `NSApplication`。
///
/// # 为什么要有它，以及为什么只能有一处
///
/// 服务是 LaunchAgent 拉起的裸可执行文件，没走过 `NSApplicationMain`，NSApp 要自己建。
/// 而「建 NSApp + 定激活策略」这件事有两个调用方（主事件循环 `global_hotkey_macos::
/// run_main_loop`、惰性建窗 [`MacPanel::create`]），各写一份迟早在策略上分叉——那是
/// 「服务进程忽然跳进 Dock」这类没人预料得到的症状。
///
/// ⚠️ 激活策略取 `Accessory` 而非 `Regular`：它与 `run_main_loop` 里已有的
/// `TransformProcessType(TRANSFORM_TO_UI_ELEMENT)` 说的是**同一件事**（不进 Dock、
/// 不占菜单栏），保持一致才不会互相翻烧饼。`Regular` 对一个输入法后台服务是明显
/// 错误的可见副作用。
///
/// **必须在主线程调用。**
pub fn ensure_app() -> Retained<NSApplication> {
    let mtm =
        MainThreadMarker::new().expect("NSApplication 只能在主线程取用；调用方漏了主线程约定");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app
}

/// 鼠标事件的种类。刻意**不照搬 Win32 消息名**——本模块是 AppKit 侧，照搬只会让读者
/// 以为这里在模拟消息泵。语义对位见 `soft_keyboard.rs` 的 `mouse_macos`。
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
    /// 光标离开面板。**必须有这一条**：没有它，鼠标快速划出面板时最后那一格高亮会一直
    /// 亮着——移动事件只在光标还在视图内时到达，出界那一下没有任何事件。
    Leave,
    /// 滚轮。
    Wheel,
}

/// 一次鼠标事件。坐标一律是**设备像素**。
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub kind: MouseKind,
    /// 面板客户区坐标，原点在面板**左上**（视图 `isFlipped` 已保证 y 向下）。
    pub x: f32,
    pub y: f32,
    /// 全局光标坐标，原点在主屏**左上**（已由本模块从 AppKit 的左下原点翻好）。
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
/// 返回 `Some((x, y))` 表示「请把面板挪到这里」（设备像素、左上原点）——拖动就靠它。
/// 让处理器**返回意图**而不是自己调 AppKit，是为了把 `soft_keyboard.rs` 那半边留在
/// 纯 Rust 里可测：拖动的算术（锚点差 + 钳制）不该需要一个真窗口才能验证。
pub trait PanelMouse {
    fn on_mouse(&mut self, ev: MouseEvent, geom: PanelGeom) -> Option<(i32, i32)>;

    /// 面板**实际**落到了哪里（设备像素、左上原点）。
    ///
    /// ★ 与 `on_mouse` 的返回值分开是必须的：那个返回值是「想去哪」，而
    /// [`set_window_origin_px`] 还会把它钳进可见区。处理器要记住的是**钳制后**的落点——
    /// 记成想去的那个，下一帧 `render` 拿它调 `show` 就会把面板弹回屏外，
    /// 表现为「往边上一拖，松手后自己跳走了」。
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
        /// y 轴向下。
        ///
        /// ★ 这一行同时买到两件事，缺了任何一件都要在别处补一次翻转：
        ///   1. `layer.contents` 贴上去的 CGImage 方向与我们的缓冲区一致（首行在顶）；
        ///   2. `convertPoint:fromView:` 出来的鼠标 y 从**顶**量起，正好是 `hit_at` 要的。
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// 面板不抢焦点，点它时窗口不会被激活；不返回 true 的话**第一次点击会被系统吃掉**
        /// 用于激活窗口，用户看到的是「第一下没反应，第二下才出字」。
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
        // 用图层承载像素：`layer.contents` 直接吃 CGImage，省掉 drawRect 与
        // NSGraphicsContext 那一层（后者的 CGContext 来自 objc2-core-graphics，与本仓
        // 既有的 core-graphics 0.25 是两套不通用的类型，混用只会徒增指针转换）。
        this.setWantsLayer(true);
        this.retune_tracking();
        this
    }

    /// 重装跟踪区。
    ///
    /// 已带 `InVisibleRect`（跟踪范围由 AppKit 自动跟随 `visibleRect`），故换尺寸后
    /// 严格说不必重装。仍在 `resize` 里调一次是**把不变量写成代码**：跟踪范围必须等于
    /// 面板当前大小，一旦哪天去掉 `InVisibleRect`，缺了这一步的症状是「面板变宽后
    /// 右半边不高亮」——那种错很难从现象反推到跟踪区上。
    fn retune_tracking(&self) {
        let st = self.ivars();
        if let Some(old) = st.tracking.borrow_mut().take() {
            self.removeTrackingArea(&old);
        }
        let opts = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            // ActiveAlways：面板永远不是 key window（NOACTIVATE），若用
            // ActiveInKeyWindow 之类，移动事件一次都不会来。
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
    /// ⚠️ 对处理器一律用 `try_borrow_mut`，借不到就**丢掉这一次事件**。
    ///
    /// AppKit 事件与面板的 tick 定时器同在主线程，而 tick 期间 `SoftMouse` 是借出的；
    /// 某些 AppKit 调用会重入 run loop，于是事件有可能落在那个窗口里。用 `borrow_mut`
    /// 的话那一下就是 panic —— 输入法服务整个进程没了，代价与「少响应一次鼠标移动」
    /// 完全不成比例。丢事件是安全的：悬停/按下都会被下一个事件或下一次 tick 纠正。
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
            // 钳制之后再量一次，把**真实**落点回报给处理器，理由见 `PanelMouse::on_moved`。
            let g = window_geom_px(&w, scale);
            if let Ok(mut h) = handler.try_borrow_mut() {
                h.on_moved(g.x, g.y);
            }
        }
    }
}

/// 滚轮增量换算成「格」。
///
/// 触控板一次轻扫会来几十个极小 delta（`hasPreciseScrollingDeltas`），直接当格数用会
/// 让标签行飞过好几屏。这里按行折算，与 `.app` 侧候选窗滚轮「攒够一格再发」同源。
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
/// ⚠️ 取的是 `NSScreen.screens[0]` 而**不是** `NSScreen.main`：AppKit 全局坐标的原点
/// 恒在第 0 号屏的左下角，而 `main` 指的是「当前有 key window 的那块屏」，会随用户
/// 换屏而变。拿 `main` 去做翻转，在多显示器上会整块错位。
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
        // AppKit 的 origin.y 是窗口**底**边到主屏底边的距离；左上原点要的是顶边到主屏顶边。
        y: ((top - f.origin.y - f.size.height) * scale).round() as i32,
        w: (f.size.width * scale).round() as u32,
        h: (f.size.height * scale).round() as u32,
    }
}

/// 把窗口挪到「设备像素 + 左上原点」给出的位置，并钳进可见区。
///
/// 钳制在这里做而不是在鼠标处理器里：可见区（`visibleFrame`，已扣掉菜单栏与 Dock）
/// 只有 AppKit 主线程问得到，而面板比候选窗大得多，拖出屏幕就再也抓不回来了。
fn set_window_origin_px(win: &NSWindow, x: i32, y: i32, scale: f64) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let f = win.frame();
    let (w_pt, h_pt) = (f.size.width, f.size.height);
    let (mut x_pt, mut y_top_pt) = (x as f64 / scale, y as f64 / scale);

    // 钳到**落点所在**那块屏的可见区。用落点而不是当前位置，跨屏拖动才不会被拽回来。
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

/// macOS 原生浮动面板。方法集刻意与 `window::LayeredWindow` **同形**，于是
/// `soft_keyboard.rs` 只需换一个类型别名，1900 行绘制/交互代码一个字都不用改。
pub struct MacPanel {
    panel: Retained<NSPanel>,
    view: Retained<PanelView>,
    mtm: MainThreadMarker,
    width: u32,
    height: u32,
    /// BGRA 预乘像素缓冲区（与 Windows 侧 `LayeredWindow` 同一约定）。
    buffer: Vec<u8>,
    /// 面板**当前所在屏**的 backing 倍率。
    ///
    /// `Cell` 而非裸 `f64`：`show` 的签名要与 `LayeredWindow::show` 同形（`&self`），
    /// 而每次显示都要重采一次倍率——面板被拖到另一块屏后不重采，px↔pt 的换算就整体
    /// 错位（Retina ↔ 非 Retina 之间差一倍）。
    scale: Cell<f64>,
}

impl MacPanel {
    /// 建面板。**必须在主线程**——拿不到 `MainThreadMarker` 直接报错，不做静默降级：
    /// 静默降级的后果是「面板永远不出现，日志里什么都没有」。
    /// 参数表与 `window::LayeredWindow::create` **逐位对齐**（含 macOS 上无意义的
    /// `parent`），这样 `soft_keyboard.rs` 的调用点在三个平台上是同一行。
    pub fn create(
        _parent: Option<crate::sys::HWND>,
        width: u32,
        height: u32,
        _class_name: &str,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "软键盘面板只能在主线程创建（当前不在主线程）".to_string())?;

        // AppKit 要有 NSApp 才谈得上开窗（理由与策略选择见 [`ensure_app`]）。
        // 正常路径上主事件循环已经建过了，这里是幂等兜底——面板也可能被单测/示例
        // 在没进事件循环时先建出来。
        let _ = ensure_app();

        let scale = f64::from(crate::dpi::scale_for_point(0, 0));
        let (w_pt, h_pt) = (f64::from(width) / scale, f64::from(height) / scale);
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w_pt, h_pt));

        // NonactivatingPanel 是本面板的**行为前提**，不是装饰：
        // `close_softkeyboard_on_focus_change` 那条「切走就关面板」的逻辑，完全依赖面板
        // 自己不抢焦点。面板一旦可激活，用户点它上面任何一个键都是在改变焦点，它会把
        // 自己关掉——正是设计文档 §「焦点切换时关闭」里写明的那个陷阱。
        // 对位 Windows 侧的 `WS_EX_NOACTIVATE`。
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setFloatingPanel(true);
        panel.setHidesOnDeactivate(false);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        // 面板画的是自带圆角的整块位图，系统阴影会沿**矩形**边缘投影，露出四个直角。
        panel.setHasShadow(false);
        // 跟着用户走：切 Space 不留在原地，全屏应用之上也要能出现（输入法浮层的通行行为）。
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );
        // 浮在普通窗口之上。对位 Windows 侧的 HWND_TOPMOST。
        panel.setLevel(FLOATING_WINDOW_LEVEL);

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
        // 改尺寸要保持**左上角**不动。AppKit 的 origin 记的是左下角，直接 setContentSize
        // 会让面板在变高时向上长——用户看到的是「切到键位多的那一面，面板往上跳了一截」。
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
        // BGRA 预乘（与 Windows 侧 UpdateLayeredWindow 同一约定）在 CG 的说法里是
        // 「小端 32 位 + Alpha 在前」：内存序 B,G,R,A 读成小端 u32 就是 ARGB。
        // ★ 与 `text/coretext.rs` 给 CGContext 用的是**同一组常量**——两处描述的是同一
        // 块缓冲区的同一种排布，各写各的迟早分叉成「文字对了底色反了」。
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
        // 取图层走裸 `msg_send` 而不是 `NSView::layer()`：后者的返回类型是 `CALayer`，
        // 要为它把整个 objc2-quartz-core 拉进依赖树，而我们只用得上两个 setter。
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
        // 每次显示都重采一次倍率：`render` 每帧都走到这里，于是跨屏拖动后的第一帧就
        // 把换算校正过来了，不需要另设一条「屏幕变了」的通知路径。
        self.refresh_scale();
        set_window_origin_px(&self.panel, x, y, self.scale.get());
        // orderFront 而**不是** makeKeyAndOrderFront：后者会抢焦点，见 create 里
        // NonactivatingPanel 那段的理由。
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

    /// 重新采一次**面板所在屏**的 backing 倍率。
    ///
    /// ⚠️ 直接问 `NSWindow.screen.backingScaleFactor`，**不走** `dpi::scale_for_point`：
    /// 后者吃的是坐标，而我们手上的坐标本身就是用（可能已经过期的）倍率换算出来的
    /// ——拿它去查屏幕是循环依赖，混合 DPI 下会查到错的那块屏。窗口自己知道它在哪，
    /// 问它最直接。
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

/// `NSFloatingWindowLevel`。AppKit 的窗口层级是 `CGWindowLevelForKey` 的映射值，
/// 浮动层恒为 3（`kCGFloatingWindowLevelKey`）。
const FLOATING_WINDOW_LEVEL: isize = 3;

/// 物理 Shift 是否按住、大写锁定是否开着。
///
/// 对位 Windows 侧 `soft_keyboard::read_shift_caps` 里的 `GetKeyState`。走
/// `NSEvent::modifierFlags`（**类**方法，查的是当前状态而非某个事件）而不是
/// `CGEventSourceFlagsState`：前者已在依赖里、不需要额外授权，且本函数的唯一调用点
/// （面板的 `tick`）本来就在主线程。
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
/// 对位 Windows 侧的 `SPI_GETKEYBOARDDELAY` / `SPI_GETKEYBOARDSPEED`。macOS 把这两个值
/// 存在 `NSGlobalDomain` 的 `InitialKeyRepeat` / `KeyRepeat`，单位是 **1/60 秒**。
/// 键没设过时读回 0，此时返回 `None` 让调用方用自己的兜底常量——把 0 当真值会得到
/// 「按下即无限连发」。
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
