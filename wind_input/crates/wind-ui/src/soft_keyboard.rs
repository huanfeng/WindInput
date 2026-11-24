//! 软键盘面板窗口。
//!
//! 一块常驻的符号面板：按物理键盘布局画出当前面的键位，鼠标可点、物理按键会高亮。
//!
//! 设计见 docs/design/soft-keyboard.md。
//!
//! - 布局零配置：键位坐标由键名唯一决定。[`ROW_SLOTS`] 只描述每行几个键位，键位名随
//!   [`SoftKeyCap`] 从协调器下发，两边不各存一份名字。
//! - 每次都重建整棵 View 树再整块重绘，没做单键重绘。够用的前提是键帽都是固定宽高、
//!   字形测量走 `TextRenderer` 缓存；若改成按内容自适应宽度或去掉缓存，再上单键重绘。
//! - 不抢焦点是承重墙：窗口用 `WS_EX_NOACTIVATE` / `NonactivatingPanel`，面板一旦可激活，
//!   点任何键都会改变焦点、触发「切换焦点自动关闭」把自己关掉。
//! - 三平台共用：布局、绘制、命中、状态机都不带 `cfg`。分叉只在文件末尾两处：窗口壳
//!   `PanelWindow`（Windows `LayeredWindow`、macOS [`crate::mac_panel::MacPanel`]、Linux mock），
//!   以及鼠标来源（`mouse_impl` / `mouse_macos`，写同一个 [`SoftMouse`] 状态机）。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use crate::text::dwrite::TextRenderer;
use crate::view::{Align, Edges, Layout, Rect, View};

/// 面板的窗口壳。三个平台的方法集同形，故本文件其余部分无需分叉。
///
/// macOS 上刻意不复用 `window::LayeredWindow`：它在 macOS 上是纯像素缓冲（候选窗靠它光栅化
/// 后写 SHM），改成真窗口会给候选窗凭空开出一个 NSPanel，砸掉 host-render 管线。
#[cfg(target_os = "macos")]
use crate::mac_panel::MacPanel as PanelWindow;
#[cfg(not(target_os = "macos"))]
use crate::window::LayeredWindow as PanelWindow;
use wind_ui_types::{
    SOFT_FN_CAPS_INDEX, SOFT_FN_KEYS, SOFT_TAG_CLOSE, SOFT_TAG_CTRL, SOFT_TAG_ESC,
    SOFT_TAG_FN_BASE, SOFT_TAG_PAGE_BASE, SOFT_TAG_PAGE_NEXT, SOFT_TAG_PAGE_PREV, SOFT_TAG_PIN,
    SOFT_TAG_SHIFT, SOFT_TAG_TAB_LEFT, SOFT_TAG_TAB_RIGHT, SOFT_TAG_TAB_VIEWPORT, SoftKeyCap,
    UiEvent, fn_key_repeats, slot_layer,
};

/// 钉住图标边长（dp）。
const PIN_ICON_DP: f32 = 13.0;

/// 钉住按钮的两张图标，转成 `data:` URI 一次（`ViewImage` 的源是字符串，且填充缓存以它为键）。
fn pin_icon(pinned: bool, tint: [u8; 4], size: f32) -> crate::view::ViewImage {
    use base64::Engine as _;
    use std::sync::OnceLock;
    static URIS: OnceLock<[String; 2]> = OnceLock::new();
    let uris = URIS.get_or_init(|| {
        let enc = |svg: &str| {
            format!(
                "data:image/svg+xml;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(svg)
            )
        };
        [
            enc(include_str!("../res/icons/pin_off.svg")),
            enc(include_str!("../res/icons/pin_on.svg")),
        ]
    });
    crate::view::ViewImage {
        path: uris[usize::from(pinned)].clone(),
        mode: String::new(),
        slice: [0.0; 4],
        slice_repeat: [false; 2],
        opacity: 1.0,
        tint: Some(tint),
        place: Some(crate::view::ImagePlace {
            anchor: "center".into(),
            w: size.round(),
            h: size.round(),
            ..Default::default()
        }),
        scale: 1.0,
        flip: wind_theme::Flip::NONE,
    }
}

/// 每行的键位个数，与 `wind_softkeyboard::KEY_ROWS` 同构（数字行 / QWERTY / ASDF / ZXCV）。
///
/// 只存个数不存键位名：名字随 `SoftKeyCap` 下发，渲染端再存一份就会分叉。
const ROW_SLOTS: [usize; 4] = [13, 13, 11, 10];

/// 键位单元（dp）。设计稿定的 42，下限约 34——再小 CJK 符号就不准了。
const UNIT_DP: f32 = 42.0;
/// 键间距（dp）。
const GAP_DP: f32 = 5.0;
/// 面板内边距（dp）。
const PAD_DP: f32 = 12.0;
/// 键盘区宽度（单位数 + 间隙数）：最宽的是第一行（13 键位 + 2u 退格 = 15u，13 个间隙）。
/// 标签行与底行都按它对齐，面板宽度就只由键盘决定。
const KBD_UNITS: f32 = 16.0;
const KBD_GAPS: f32 = 14.0;
/// 标签行度量（dp）。必须是模块常量：[`SoftKeyboard::visible_tabs`] 与
/// [`SoftKeyboard::build_tabs`] 靠它们算出同一个可见集。
const TAB_GAP_DP: f32 = 2.0;
const TAB_PAD_X_DP: f32 = 9.0;
const TAB_FONT_DP: f32 = 12.5;
const TAB_ARROW_W_DP: f32 = 22.0;
const TAB_CLOSE_W_DP: f32 = 27.0;
/// 顶部拖动条：行高与条本身的尺寸（dp）。
const GRIP_ROW_H_DP: f32 = 9.0;
/// 关闭按钮悬停框的高度（dp）。
///
/// 它比拖动条行高（`GRIP_ROW_H_DP`）大：后者是给横线定的，拿来当亮框基准会又扁又偏上。
/// 亮框靠向上溢出到面板顶部内边距里补足高度。
const CLOSE_H_DP: f32 = 16.0;
const GRIP_W_DP: f32 = 46.0;
const GRIP_H_DP: f32 = 3.5;

/// 长按重复：首次延迟与间隔（毫秒）。
///
/// 只是读不到系统设置时的兜底，取接近 Windows 默认的值；正常路径见 [`repeat_params`]。
const REPEAT_DELAY_MS: u64 = 500;
const REPEAT_RATE_MS: u64 = 33;
/// 物理按键高亮的持续时间（毫秒）。长按时被连发的 keydown 不断续期。
const KEY_FLASH_MS: u64 = 140;
/// 物理 Shift / 大写锁定的跟随节奏（毫秒），仅面板可见期间生效。
const MODIFIER_POLL_MS: u64 = 40;
/// 本平台读得到物理 Shift / 大写锁定（见 [`read_shift_caps`]）。读不到时（Linux 外部宿主）
/// 不登记上面那个节奏，免得白白唤醒线程。
const MODIFIER_POLLS: bool = cfg!(any(windows, target_os = "macos"));

/// 面板上的 Caps 键点了能不能翻转系统大写锁定。
///
/// macOS 上恒为 false，不是漏改：大写锁定状态由 HID 层持有，合成按键从其上的 CGEvent 层注入
/// 够不着，三条路都证伪过（见 docs/design/soft-keyboard.md §11.9）。
///
/// 禁掉的只是「点它去翻转」：物理 CapsLock 状态照常轮询回来并高亮，禁用态下它仍是如实的
/// 指示灯，见 [`SoftKeyboard::caps_key`]。
const CAPS_CLICKABLE: bool = !cfg!(target_os = "macos");

/// 面板配色（全部走主题键，未配则回落到与候选窗同族的中性色）。
#[derive(Clone)]
struct Colors {
    panel: [u8; 4],
    keycap: [u8; 4],
    keycap_fn: [u8; 4],
    keycap_dead: [u8; 4],
    line: [u8; 4],
    ink: [u8; 4],
    /// 角标色。必须比正文更淡，又要在深色底上仍可读——直接沿用 `dim` 会偏亮。
    hint: [u8; 4],
    accent: [u8; 4],
    accent_soft: [u8; 4],
    on_accent: [u8; 4],
    /// 顶部拖动条的颜色。取工具栏那根同款（`toolbar_grip`）——两处是同一件事：
    /// 「这块东西可以拖」。
    grip: [u8; 4],
    /// 悬停底色。必须比按下态轻得多，否则分不清有没有点下去。取主题的 `toolbar_hover`
    /// （很淡的半透明叠加，深浅主题各有其值）。
    hover: [u8; 4],
    /// 功能键（Tab/Enter/Shift/空格…）的文字色。
    ///
    /// 比正文淡一档、但比角标深：功能键是能按的键，用角标那种淡色会让整排看着像禁用。
    fn_ink: [u8; 4],
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            panel: [251, 253, 253, 250],
            keycap: [243, 247, 248, 255],
            keycap_fn: [228, 235, 238, 255],
            keycap_dead: [233, 238, 239, 255],
            line: [213, 223, 226, 255],
            ink: [21, 34, 42, 255],
            hint: [143, 163, 171, 255],
            accent: [44, 110, 141, 255],
            accent_soft: [217, 233, 241, 255],
            on_accent: [255, 255, 255, 255],
            grip: [184, 190, 204, 255],
            // 很淡的一层叠加：悬停只是「鼠标在这」，不该有主色的分量。
            hover: [0, 0, 0, 13],
            fn_ink: [82, 100, 110, 255],
        }
    }
}

impl Colors {
    /// 从主题取色（见 [`SoftKeyboard::set_theme`] 的文档）。抽成纯函数是为了让取色链能被测到：
    /// `SoftKeyboard` 要真窗口才能构造。
    fn from_theme(theme: &wind_theme::Resolved) -> Self {
        let d = Colors::default();
        // 专用覆盖 → 键盘域语义色 → 候选窗同类色 → 硬编码兜底。
        let pick =
            |own: &str, kbd: &str, fallback: [u8; 4]| theme.color(own, theme.color(kbd, fallback));
        Colors {
            panel: pick(
                "softkb_bg",
                "keyboard_bg",
                theme.color("candidate_bg", d.panel),
            ),
            keycap: pick("softkb_key_bg", "key_bg", d.keycap),
            keycap_fn: pick("softkb_fnkey_bg", "key_special_bg", d.keycap_fn),
            keycap_dead: pick("softkb_dead_bg", "surface", d.keycap_dead),
            line: pick("softkb_border", "border", d.line),
            ink: pick(
                "softkb_text",
                "key_text",
                theme.color("candidate_text", d.ink),
            ),
            hint: pick("softkb_hint", "key_hint", d.hint),
            accent: pick(
                "softkb_active_bg",
                "accent",
                theme.color("candidate_selected_bg", d.accent),
            ),
            accent_soft: pick(
                "softkb_hover_bg",
                "key_pressed_bg",
                theme.color("accent_soft", d.accent_soft),
            ),
            // 激活键是「强调色底上的字」：第二级取 on_accent，不取 accent_text——
            // accent_text 是普通底上的强调色文字，常等于 accent，压在 accent 底上会整个消失。
            on_accent: pick(
                "softkb_active_text",
                "on_accent",
                theme.color("candidate_selected_text", d.on_accent),
            ),
            grip: pick("softkb_grip", "toolbar_grip", d.grip),
            hover: pick("softkb_hover_soft", "toolbar_hover", d.hover),
            fn_ink: pick("softkb_fnkey_text", "text_dim", d.fn_ink),
        }
    }
}

/// 鼠标交互状态。与窗口共享（`register_mouse` 要 `Rc<RefCell<dyn WindowMouse>>`）。
#[derive(Default)]
struct SoftMouse {
    /// 命中区（tag, 矩形），每次重排后更新。
    hits: Vec<(i32, Rect)>,
    /// 当前悬停 tag，-1 = 无。
    hover: i32,
    /// 按下中的 tag，-1 = 无。
    pressed: i32,
    /// 待处理的点击（tag），由窗口在 tick 里消费。
    clicked: Vec<i32>,
    /// 长按下一次触发时刻。
    repeat_at: Option<std::time::Instant>,
    /// 已进入匀速重复阶段。
    repeating: bool,
    /// hover 变了，需要重画。
    dirty: bool,
    /// 窗口句柄（`isize` 存，避免让本结构在非 Windows 上依赖 HWND 类型）。0 = 未装配。
    ///
    /// 非 Windows 的 lib target 里它无人读，故有 `allow(dead_code)`。别改成 `#[cfg(windows)]`
    /// 字段：本结构刻意保持在非 Windows 上也能构造。
    #[cfg_attr(not(windows), allow(dead_code))]
    hwnd: isize,
    /// 拖动中；`anchor` 是按下时的屏幕光标，`origin` 是按下时的窗口左上。
    /// 两平台同义：Windows 由 `mouse_impl` 写，macOS 由 `mouse_macos` 写。
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    dragging: bool,
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    anchor: (i32, i32),
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    origin: (i32, i32),
    /// 拖动落点（左上角），供窗口在 tick 里收回去。拖动途中每次 `WM_MOUSEMOVE` 都更新。
    moved_to: Option<(i32, i32)>,
    /// 拖动**结束**时的面板右下角，供窗口在 tick 里上报协调器持久化。
    ///
    /// 与 `moved_to` 分开：持久化只能在抬起时发生，否则一次拖动就是上百条 IPC 与 `state.toml` 读改写。
    /// 存右下角而非左上角：记的是锚点，面板尺寸会随切面变。
    #[cfg_attr(not(windows), allow(dead_code))]
    dropped_at: Option<(i32, i32)>,
    /// 未消费的滚轮量（单位：一格）。窗口过程只累加，滚动在 `tick` 里做（要 `&mut SoftKeyboard`）。
    wheel: f32,
    /// 是否已向系统订阅过 `WM_MOUSELEAVE`（一次性，收到后要重订）。
    #[cfg_attr(not(windows), allow(dead_code))]
    leave_armed: bool,
}

impl SoftMouse {
    /// 还原窗口句柄。存 `isize` 是为了让本结构在非 Windows 上也能构造。
    #[cfg(windows)]
    fn hwnd_handle(&self) -> crate::sys::HWND {
        crate::sys::HWND(self.hwnd as *mut core::ffi::c_void)
    }

    /// Linux 外部宿主形态：窗口壳是 `ext_panel`，句柄就是浮层 kind。
    #[cfg(all(target_os = "linux", ext_presenter))]
    fn hwnd_handle(&self) -> crate::sys::HWND {
        crate::sys::HWND(self.hwnd)
    }

    /// Linux 外部宿主形态：addon 每次指针离开都报 `LEAVE`，不需要订阅。
    #[cfg(all(target_os = "linux", ext_presenter))]
    fn arm_leave(&mut self) {}

    /// 订阅一次性 `WM_MOUSELEAVE`（光标移出窗口时收到）。收到后系统即注销，需重订。
    #[cfg(windows)]
    fn arm_leave(&mut self) {
        if self.leave_armed || self.hwnd == 0 {
            return;
        }
        self.leave_armed = true;
        unsafe {
            use windows::Win32::UI::Input::KeyboardAndMouse::{
                TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
            };
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd_handle(),
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut t);
        }
    }

    /// 调用点：各平台的鼠标分支与 `refresh_hover`，以及导出拖动区的 [`drag_regions_of`]。
    fn hit_at(&self, x: f32, y: f32) -> i32 {
        // 逆序找：后画的（层级更高的）优先。键帽之间不重叠，这里只是稳妥。
        self.hits
            .iter()
            .rev()
            .find(|(_, r)| r.contains(x, y))
            .map(|(t, _)| *t)
            .unwrap_or(-1)
    }
}

/// 长按重复参数：优先取系统的键盘重复设置。
///
/// 不自定常数：鼠标长按与物理键长按必须同一个手感。
pub(crate) fn repeat_params() -> (u64, u64) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            SPI_GETKEYBOARDDELAY, SPI_GETKEYBOARDSPEED, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
            SystemParametersInfoW,
        };
        let mut delay: u32 = 1;
        let mut speed: u32 = 31;
        let ok_d = SystemParametersInfoW(
            SPI_GETKEYBOARDDELAY,
            0,
            Some(&mut delay as *mut u32 as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok();
        let ok_s = SystemParametersInfoW(
            SPI_GETKEYBOARDSPEED,
            0,
            Some(&mut speed as *mut u32 as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok();
        if ok_d && ok_s {
            // delay 0..3 → 250/500/750/1000ms；speed 0..31 → 约 400ms..33ms（线性插值）。
            let d = 250 + 250 * u64::from(delay.min(3));
            let r = 400 - (400 - 33) * u64::from(speed.min(31)) / 31;
            return (d, r.max(15));
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(p) = crate::mac_panel::key_repeat_params() {
        return p;
    }
    (REPEAT_DELAY_MS, REPEAT_RATE_MS)
}

/// 软键盘面板窗口。
pub struct SoftKeyboard {
    window: PanelWindow,
    renderer: TextRenderer,
    scale: f32,
    colors: Colors,
    events: Sender<UiEvent>,
    mouse: Rc<RefCell<SoftMouse>>,

    pages: Vec<String>,
    current: usize,
    keys: Vec<SoftKeyCap>,
    /// 物理 Shift 按住中（临时切层，松开还原）。
    shift_held: bool,
    /// 面板上的 Shift 键被点亮（锁定切层，再点还原）。
    ///
    /// 与 `shift_held` 分开存：合成一个布尔就无法表达「松开物理 Shift 后该回哪一层」。有效层取两者的或。
    shift_locked: bool,
    /// 系统大写锁定当前是否开着（Caps 键据此高亮）。
    caps_on: bool,
    /// 当前面是键盘面：CapsLock 参与字母键的档位显示（见 [`Self::cap_layer`]）。
    send_keys: bool,
    /// 钉住：只管按钮高亮，真相在协调器（经 `SoftKeyboardPinned` 下发）。隐藏时复位。
    pinned: bool,
    /// 面板上的 Ctrl 处于粘滞状态：下一次点键位合成 `ctrl+键`，然后自动熄灭。
    ///
    /// 它不是层选择器：不改变键帽上画什么，只改变那一次点击发出去的是符号还是组合键。
    ctrl_latched: bool,
    /// 标签行的水平滚动量（设备像素）。按像素而非按项滚：项宽不一时按项对不齐容器边缘。
    tab_scroll: f32,
    /// 上一次已经「滚到可见」的面。
    ///
    /// 只在面变了时才把当前面拉进视野；每帧都拉的话用户滚不动标签行，一松手就被拽回。
    last_shown_page: usize,
    /// 物理按键按下中的键位名 + 该高亮的到期时刻。
    ///
    /// 高亮不靠 keyup 配对（物理抬起不经过协调器），而是按下即高亮、到期自清；
    /// 长按连发的 keydown 不断续期。
    down_slot: Option<(String, std::time::Instant)>,

    visible: bool,
    /// 上一帧的实际落点（左上角）；None = 还没摆过。`ensure_scale` 也用它问所在屏的 DPI。
    origin: Option<(i32, i32)>,
    /// 记忆锚点：面板右下角屏幕坐标；None = 这块屏没记录过，落默认位置。
    ///
    /// 这是意图，`origin` 是本帧落点（意图减当前尺寸再钳进工作区），分工同工具栏。
    /// 切面会改尺寸，每帧现算才能让右下角不动。钳制结果只写回 `origin`。
    anchor_br: Option<(i32, i32)>,
    /// 焦点显示器工作区 `(left, top, right, bottom)`，随 `ShowSoftKeyboard` 下发。
    ///
    /// 默认位置（底部居中）据此算。`None` 时才回退 `SPI_GETWORKAREA`（恒为主屏）。
    work_area: Option<(i32, i32, i32, i32)>,
}

impl SoftKeyboard {
    const DEFAULT_FONT_PX: f32 = 15.0;

    pub fn new(events: Sender<UiEvent>) -> Result<Self, String> {
        let scale = crate::dpi::scale_for_point(0, 0);
        let window = PanelWindow::create(None, 700, 300, "WindInputSoftKeyboard")?;
        let renderer = TextRenderer::new("Microsoft YaHei UI", Self::DEFAULT_FONT_PX * scale)?;
        let mouse = Rc::new(RefCell::new(SoftMouse {
            hover: -1,
            pressed: -1,
            #[cfg(windows)]
            hwnd: window.hwnd().0 as isize,
            #[cfg(all(target_os = "linux", ext_presenter))]
            hwnd: window.hwnd().0,
            #[cfg(not(any(windows, all(target_os = "linux", ext_presenter))))]
            hwnd: 0,
            ..Default::default()
        }));
        window.register_mouse(mouse.clone());
        Ok(Self {
            window,
            renderer,
            scale,
            colors: Colors::default(),
            events,
            mouse,
            pages: Vec::new(),
            current: 0,
            keys: Vec::new(),
            shift_held: false,
            shift_locked: false,
            caps_on: false,
            send_keys: false,
            pinned: false,
            ctrl_latched: false,
            tab_scroll: 0.0,
            last_shown_page: usize::MAX,
            down_slot: None,
            visible: false,
            origin: None,
            anchor_br: None,
            work_area: None,
        })
    }

    /// 协调器下发的摆放依据：焦点屏记过的锚点 + 该屏工作区。随每条 `ShowSoftKeyboard`
    /// 到来，而打开面板和切面刷新都会发。
    ///
    /// 只在面板不可见（真正打开）时采纳锚点，且连 `None` 一起采纳（落该屏默认位置）：
    /// 切面时采纳会让面板跳到新焦点屏；打开时不采纳会开在上次拖过的那块屏上。
    /// 面板开着期间 `anchor_br` 的真相源是 UI 侧自己（拖动结束时更新）。
    pub fn set_placement(
        &mut self,
        anchor: Option<(i32, i32)>,
        work_area: Option<(i32, i32, i32, i32)>,
    ) {
        if !self.visible {
            self.anchor_br = anchor;
            // 光清 `anchor_br` 不够：`render` 落点顺序是 `anchor_br` > `origin` > 默认位置，
            // 上一帧的 `origin` 会顶上来，面板照样开在旧屏上。同屏重开时重算结果与 `origin` 相同。
            if anchor.is_none() {
                self.origin = None;
            }
        }
        // 工作区无条件跟新：只在没有锚点和 `origin` 时参与算默认位置，不会造成跳屏。
        if let Some(w) = work_area {
            self.work_area = Some(w);
        }
    }

    /// 取主题色。
    ///
    /// 复用主题里已有的「键盘域」语义色（`key_bg` / `key_text` / `key_hint` / `key_special_bg` /
    /// `key_pressed_bg` / `keyboard_bg`，原为 Android 软键盘定义），不另立一套：它们以 `${var}`
    /// 引用主色，派生主题只覆盖 primary/accent 就能让键盘跟着变。`softkb_*` 留在链首，
    /// 给只想单独调桌面软键盘的人一个口子。
    pub fn set_theme(&mut self, theme: &wind_theme::Resolved) {
        self.colors = Colors::from_theme(theme);
        if self.visible {
            self.render();
        }
    }

    /// 当前 Shift 档：物理按住 或 面板上锁定，任一即可。
    ///
    /// 不含 CapsLock：它也是键盘面点击回送的 `shift`，Caps 开着再合成 `shift+q` 会出小写，恰好反了。
    /// CapsLock 只影响显示，见 [`Self::cap_layer`]。
    fn layer_shift(&self) -> bool {
        self.shift_held || self.shift_locked
    }

    /// 一个键位**显示**哪一档（两种面都生效，判据见 [`slot_layer`]）。
    fn cap_layer(&self, slot: &str) -> bool {
        slot_layer(slot, self.layer_shift(), self.caps_on)
    }

    /// 点击这个键位时回送给协调器的 `shift`。
    ///
    /// - 符号面查表直接上屏 ⇒ 回送显示档，画着什么就出什么。
    /// - 键盘面合成真实按键 ⇒ 回送物理 Shift，CapsLock 由系统自己应用（理由见 [`Self::layer_shift`]）。
    fn click_shift(&self, slot: &str) -> bool {
        if self.send_keys {
            self.layer_shift()
        } else {
            self.cap_layer(slot)
        }
    }

    /// 显示面板 / 整块刷新（切面也走这里）。
    pub fn show(
        &mut self,
        pages: Vec<String>,
        current: usize,
        keys: Vec<SoftKeyCap>,
        send_keys: bool,
    ) {
        self.pages = pages;
        self.send_keys = send_keys;
        // 面的类型决定点击是「上屏符号」还是「合成按键」，排查按键行为时先看这一行。
        tracing::debug!("软键盘: show page={current} send_keys={send_keys}");
        self.current = current;
        // 标签窗口的校正统一在 `render` 里做（见 [`Self::ensure_current_visible`]）：
        // current 有热键、直通车、点标签、底行翻页四条来路，逐条加一次迟早漏一条。
        self.keys = keys;
        self.shift_held = false;
        self.shift_locked = false;
        self.down_slot = None;
        self.reset_mouse();
        self.visible = true;
        self.render();
    }

    /// 钉住状态变了：只重画按钮。
    pub fn set_pinned(&mut self, pinned: bool) {
        if self.pinned != pinned {
            self.pinned = pinned;
            if self.visible {
                self.render();
            }
        }
    }

    pub fn hide(&mut self) {
        self.pinned = false;
        if self.visible {
            self.visible = false;
            self.reset_mouse();
            self.window.hide();
        }
    }

    /// 清掉悬停/按下残留。
    ///
    /// 只在显示/隐藏边界重置不够：面板常开，鼠标划出去时的最后一格高亮要靠订阅
    /// `WM_MOUSELEAVE` 清（见 `SoftMouse::arm_leave`）。
    fn reset_mouse(&self) {
        let mut m = self.mouse.borrow_mut();
        m.hover = -1;
        m.pressed = -1;
        m.repeat_at = None;
        m.repeating = false;
        m.clicked.clear();
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 物理按键按下/抬起 → 键帽高亮。只改颜色，不重排。
    pub fn set_key_down(&mut self, slot: &str, down: bool) {
        let changed = match (&self.down_slot, down) {
            (Some((s, _)), true) => s != slot,
            (None, true) => true,
            (Some(_), false) => true,
            (None, false) => false,
        };
        self.down_slot = down.then(|| {
            (
                slot.to_string(),
                std::time::Instant::now() + std::time::Duration::from_millis(KEY_FLASH_MS),
            )
        });
        if changed && self.visible {
            self.render();
        }
    }

    /// 切层（按住 Shift）。键帽文字要变，需要重排。
    pub fn set_layer(&mut self, shift: bool) {
        if self.shift_held != shift {
            self.shift_held = shift;
            if self.visible {
                self.render();
            }
        }
    }

    /// 长按重复的下一次触发时刻（供 UI 循环安排 wake，避免空转轮询）。
    pub fn next_deadline(&self) -> Option<std::time::Instant> {
        if !self.visible {
            return None;
        }
        [
            self.mouse.borrow().repeat_at,
            // 物理按键高亮的自动熄灭
            self.down_slot.as_ref().map(|(_, at)| *at),
            // 物理 Shift / 大写锁定的跟随节奏（仅面板可见期间，见 tick 里的说明）
            MODIFIER_POLLS.then(|| {
                std::time::Instant::now() + std::time::Duration::from_millis(MODIFIER_POLL_MS)
            }),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// 消费鼠标产生的点击与长按重复，并在 hover 变化时重画。
    pub fn tick(&mut self) {
        if !self.visible {
            return;
        }
        // 长按：到点就再发一次当前按住的 tag。
        let mut fire: Vec<i32> = Vec::new();
        {
            let mut m = self.mouse.borrow_mut();
            let now = std::time::Instant::now();
            if let Some(at) = m.repeat_at
                && now >= at
                && m.pressed >= 0
            {
                let tag = m.pressed;
                // 改变状态的键不重复——按住会让面飞速乱切。
                if repeats(tag) {
                    fire.push(tag);
                }
                let (_, rate) = repeat_params();
                m.repeating = true;
                m.repeat_at = Some(now + std::time::Duration::from_millis(rate));
            }
            fire.append(&mut m.clicked);
        }
        for tag in fire {
            self.dispatch(tag);
        }
        // 拖动落点：窗口自己记住，本次会话内不再回到默认锚点。
        if let Some(pos) = self.mouse.borrow_mut().moved_to.take() {
            self.origin = Some(pos);
        }
        // 拖动结束：记下锚点并上报协调器落盘（只在抬起时发一次，见 `SoftMouse::dropped_at`）。
        if let Some((right, bottom)) = self.mouse.borrow_mut().dropped_at.take() {
            self.anchor_br = Some((right, bottom));
            let _ = self
                .events
                .send(UiEvent::SoftKeyboardMoved { right, bottom });
        }

        // 滚轮：窗口过程只累加格数，这里换算成像素并重绘。
        let wheel = std::mem::take(&mut self.mouse.borrow_mut().wheel);
        if wheel != 0.0 {
            let step = self.tab_step();
            if self.scroll_tabs(wheel * step * 0.5) {
                self.render();
            }
        }

        // 物理 Shift 与大写锁定的跟随。单独按 Shift 的 keydown 不会转发到协调器，故面板可见期间
        // 以固定节奏轮询键盘状态。这是 UI 事件驱动的有意例外，面板一关 `next_deadline` 就不再登记。
        if let Some((held, caps)) = read_shift_caps() {
            if held != self.shift_held {
                // 松开物理 Shift 时把面板上的锁定一并解除：按下又松开后界面仍停在上档，看起来像卡住。
                if !held {
                    self.shift_locked = false;
                }
                self.shift_held = held;
                self.render();
            }
            if caps != self.caps_on {
                tracing::debug!("软键盘: caps_lock {} -> {caps}", self.caps_on);
                self.caps_on = caps;
                self.render();
            }
        }

        // 物理按键高亮到期熄灭。
        let mut dirty = if let Some((_, at)) = &self.down_slot
            && std::time::Instant::now() >= *at
        {
            self.down_slot = None;
            true
        } else {
            false
        };
        dirty |= {
            let mut m = self.mouse.borrow_mut();
            std::mem::take(&mut m.dirty)
        };
        if dirty {
            self.render();
        }
    }

    /// 把一次命中翻译成协调器事件。
    fn dispatch(&mut self, tag: i32) {
        if tag < 0 {
            return;
        }
        if tag == SOFT_TAG_CLOSE || tag == SOFT_TAG_ESC {
            let _ = self.events.send(UiEvent::SoftKeyboardClose);
            return;
        }
        if tag == SOFT_TAG_PIN {
            let _ = self.events.send(UiEvent::SoftKeyboardPin);
            return;
        }
        if tag == SOFT_TAG_PAGE_PREV || tag == SOFT_TAG_PAGE_NEXT {
            let n = self.pages.len();
            if n > 0 {
                let i = if tag == SOFT_TAG_PAGE_PREV {
                    prev_page(self.current, n)
                } else {
                    next_page(self.current, n)
                } as usize;
                let _ = self.events.send(UiEvent::SoftKeyboardPage(i));
            }
            return;
        }
        if tag == SOFT_TAG_TAB_LEFT {
            // 箭头滚半个视口——一次一项在长短不一的标签上跳得很碎，半屏更好预期。
            let step = self.tab_step();
            if !self.scroll_tabs(-step) {
                return;
            }
            self.render();
            return;
        }
        if tag == SOFT_TAG_TAB_RIGHT {
            let step = self.tab_step();
            if !self.scroll_tabs(step) {
                return;
            }
            self.render();
            return;
        }
        if tag == SOFT_TAG_CTRL {
            self.ctrl_latched = !self.ctrl_latched;
            self.render();
            return;
        }
        if tag == SOFT_TAG_SHIFT {
            // 面板自己的状态：锁定/解锁第二层，不回送协调器。
            self.shift_locked = !self.shift_locked;
            self.render();
            return;
        }
        if tag >= SOFT_TAG_FN_BASE {
            let i = (tag - SOFT_TAG_FN_BASE) as usize;
            match SOFT_FN_KEYS.get(i) {
                // 面板控制键不在 SOFT_FN_KEYS 里，这里恒是要合成的真实按键。
                Some((name, _)) => {
                    let _ = self
                        .events
                        .send(UiEvent::SoftKeyboardFunctionKey((*name).to_string()));
                }
                None => tracing::warn!("软键盘: 未知功能键 tag {tag}"),
            }
            return;
        }
        // 区域标记，不是控件：视口进命中表只为让滚轮知道鼠标在标签行上；点留白是拖动面板
        // （`drags_panel` 在按下时就接走了）。显式返回，别指望它在键位分支里靠下标越界变空操作。
        if tag == SOFT_TAG_TAB_VIEWPORT {
            return;
        }
        // 判据用 `is_page_tag` 而不是 `tag >= SOFT_TAG_PAGE_BASE`：理由见那个函数。
        if is_page_tag(tag) {
            let i = (tag - SOFT_TAG_PAGE_BASE) as usize;
            let _ = self.events.send(UiEvent::SoftKeyboardPage(i));
            return;
        }
        // 键位：tag 即下标。
        if let Some(cap) = self.keys.get(tag as usize) {
            // 空键位不回送（判空用显示档，与键帽画成灰同源）。Ctrl 粘滞时空键位也放行：
            // Ctrl+键是组合键，与这一面在该位置画没画符号无关。
            let ctrl = self.ctrl_latched;
            if ctrl || cap.output(self.cap_layer(&cap.slot)).is_some() {
                let _ = self.events.send(UiEvent::SoftKeyboardKey {
                    slot: cap.slot.clone(),
                    shift: self.click_shift(&cap.slot),
                    ctrl,
                });
                // 粘滞键用完即熄：这是修饰键的通行行为，也免得用户忘了它还亮着。
                if ctrl {
                    self.ctrl_latched = false;
                    self.render();
                }
            }
        }
    }

    /// 这一帧面板该落在哪。三级优先级的唯一定义处，见 [`Placement`]。
    fn placement(&self) -> Placement {
        Placement::pick(self.anchor_br, self.origin, self.work_area)
    }

    fn ensure_scale(&mut self, at: Placement) {
        let (x, y) = at.probe_point();
        let sc = crate::dpi::scale_for_point(x, y);
        if (sc - self.scale).abs() > 0.01 {
            self.scale = sc;
            self.renderer.set_base_size(Self::DEFAULT_FONT_PX * sc);
        }
    }

    /// 命中区随每次重排更新——用上一次的会让「切面后点第一个键出上一面的符号」。
    fn publish_hits(&self, root: &View) {
        let mut hits = Vec::new();
        root.collect_hits(&mut hits);
        self.mouse.borrow_mut().hits = hits;
    }

    /// 按下即「抓住面板拖动」的区域（位图内 `x, y, w, h`），供外部宿主在本地拖动。
    ///
    /// 判据就是鼠标状态机自己那条：`drags_panel(hit_at(..))`，见 [`drag_regions_of`]。
    pub fn drag_regions(&self) -> Vec<[i32; 4]> {
        let (w, h) = self.window.size();
        drag_regions_of(&self.mouse.borrow(), w, h)
    }

    /// 外部宿主本地拖完：落点（左上角）收进来，锚点改成新的右下角并上报协调器落盘——等同
    /// Windows 拖动结束时 `tick` 收 `moved_to` / `dropped_at` 那两步。宿主只报左上角，换算右下角
    /// 要用尺寸，而尺寸只有这里知道。
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub fn on_host_moved(&mut self, x: i32, y: i32) {
        let (w, h) = self.window.size();
        let (right, bottom) = (x + w as i32, y + h as i32);
        self.origin = Some((x, y));
        self.anchor_br = Some((right, bottom));
        let _ = self
            .events
            .send(UiEvent::SoftKeyboardMoved { right, bottom });
    }

    /// 测试用：当前悬停的 tag（-1 = 无）。
    #[cfg(all(test, target_os = "linux", ext_presenter))]
    pub(crate) fn hover(&self) -> i32 {
        self.mouse.borrow().hover
    }

    /// 测试用：某个 tag 当前的命中矩形（forwarder 用例据此找键帽的位图内坐标）。
    #[cfg(all(test, target_os = "linux", ext_presenter))]
    pub(crate) fn hit_rect(&self, tag: i32) -> Option<Rect> {
        self.mouse
            .borrow()
            .hits
            .iter()
            .rev()
            .find(|(t, _)| *t == tag)
            .map(|(_, r)| *r)
    }

    /// 重排 + 重画 + 上屏。
    fn render(&mut self) {
        if !self.visible || self.keys.is_empty() {
            return;
        }
        // 缩放与落点取自同一个 `Placement`：先定落在哪块屏，按那块屏的 DPI 排版，再用同一判据算落点。
        let at = self.placement();
        self.ensure_scale(at);
        let s = self.scale;
        // 只在换面时把当前面拉进视野，见 `last_shown_page`。
        if self.current != self.last_shown_page {
            self.scroll_current_into_view(s);
            self.last_shown_page = self.current;
        }
        let mut root = self.build(s);
        root.layout(0.0, 0.0, &self.renderer);
        self.publish_hits(&root);

        // 命中区更新后、落笔之前按当前光标校正悬停（见 `refresh_hover`）；收在这里是因为
        // 「内容动了」的来路不止一条。
        if self.refresh_hover() {
            root = self.build(s);
            root.layout(0.0, 0.0, &self.renderer);
            self.publish_hits(&root);
        }

        let (w_f, h_f) = root.measured_size();
        let w = (w_f.ceil() as u32).max(64);
        let h = (h_f.ceil() as u32).max(64);
        self.window.resize(w, h);
        // 拖动由 addon 本地做：拖动区随帧下发，与本帧的命中区同刻。
        #[cfg(all(target_os = "linux", ext_presenter))]
        self.window.set_drag_regions(self.drag_regions());
        {
            let buf = self.window.buffer_mut();
            let n = (w * h * 4) as usize;
            buf[..n].fill(0);
            root.paint(buf, w, h, &self.renderer);
        }
        if let Err(e) = self.window.update() {
            tracing::warn!("软键盘: 窗口更新失败: {e}");
            // Linux：位图超出 SHM 段上限（高缩放下的大面板）时整帧送不出去，面板不显示，协调器却
            // 仍在软键盘态接管主键区——打字全变符号。让协调器退出软键盘态，与点关闭按钮同一条路。
            #[cfg(all(target_os = "linux", ext_presenter))]
            let _ = self.events.send(UiEvent::SoftKeyboardClose);
            return;
        }
        // 落点每帧现算：锚右下角减去当前尺寸（切面会改尺寸）。
        let raw = match at {
            Placement::Anchor { right, bottom } => origin_from_anchor(right, bottom, w, h),
            Placement::LastOrigin { x, y } => (x, y),
            Placement::DefaultIn { work } => default_origin(w, h, s, work),
        };
        // 恢复的锚点必须过一次钳制：跨重启期间显示器或面板尺寸可能变了。钳制结果只落到 `origin`。
        let (x, y) = crate::sys::clamp_to_work_area(raw.0, raw.1, w, h);
        self.origin = Some((x, y));
        self.window.show(x, y);
    }

    /// 构建 View 树。
    fn build(&self, s: f32) -> View {
        let u = UNIT_DP * s;
        let gap = GAP_DP * s;
        let c = &self.colors;
        let hover = self.mouse.borrow().hover;
        let pressed = self.mouse.borrow().pressed;

        let mut root = View::container(Layout::Column)
            .bg(c.panel)
            .pad(Edges::all(PAD_DP * s))
            .gap(gap)
            .border(c.line, (1.0 * s).max(1.0))
            .radius(8.0 * s);

        // 顶部拖动条：一根居中的短横。它不带 tag，落进 `hit_at < 0` 的拖动分支，只是给本就能拖的
        // 空白处加了记号。钉住与关闭按钮分居这一行左右两端且等宽，拖动条才精确居中。
        let close_w = TAB_CLOSE_W_DP * s;
        let close_h = CLOSE_H_DP * s;
        let close_inset = PAD_DP * 0.5 * s;
        // 向上溢出量：取「亮框高 − 行高」，于是 margin_box 的高度正好落回行高,
        // `cross(Center)` 不会再叠一次偏移，亮框就稳稳居中在顶栏那条空白里。
        let close_rise = close_h - GRIP_ROW_H_DP * s;
        let close_down = pressed == SOFT_TAG_CLOSE;
        let (close_bg, close_fg) = if close_down {
            (c.accent_soft, c.accent)
        } else if hover == SOFT_TAG_CLOSE {
            (c.hover, c.ink)
        } else {
            ([0, 0, 0, 0], c.hint)
        };
        // 钉住态用强调色底，未钉住与关闭按钮同一套悬停/按下配色。
        let pin_down = pressed == SOFT_TAG_PIN;
        let (pin_bg, pin_fg) = if pin_down || self.pinned {
            (c.accent_soft, c.accent)
        } else if hover == SOFT_TAG_PIN {
            (c.hover, c.ink)
        } else {
            ([0, 0, 0, 0], c.hint)
        };
        root = root.child(
            View::container(Layout::Row)
                .fill_cross()
                .fixed_h(GRIP_ROW_H_DP * s)
                .cross(Align::Center)
                .child(
                    // 钉住按钮：与关闭按钮左右对称，同样向左上溢出吃回内边距（原理见下面
                    // 关闭按钮的注释），margin_box 仍等于 close_w × 行高，拖动条保持精确居中。
                    View::container(Layout::Row)
                        .fixed_w(close_w + close_inset)
                        .fixed_h(close_h)
                        .margin(Edges {
                            t: -close_rise,
                            l: -close_inset,
                            ..Default::default()
                        })
                        .radius(close_h * 0.5)
                        .bg(pin_bg)
                        .border(
                            if pin_down { c.accent } else { [0, 0, 0, 0] },
                            (1.0 * s).max(1.0),
                        )
                        .tag(SOFT_TAG_PIN)
                        // 图标两态各一张 SVG，按文字色 tint；不用 emoji（各平台形状不一、看不出开关）。
                        // 图直接挂在这个有固定宽高的节点上：套一层空子容器高度会塌成 0，只剩一条线。
                        .bg_image(pin_icon(self.pinned, pin_fg, PIN_ICON_DP * s)),
                )
                .child(View::spacer().grow())
                .child(
                    View::container(Layout::Row)
                        .fixed_w(GRIP_W_DP * s)
                        .fixed_h((GRIP_H_DP * s).max(2.0))
                        .radius((GRIP_H_DP * s * 0.5).max(1.0))
                        .bg(c.grip),
                )
                .child(View::spacer().grow())
                .child(
                    // 关闭按钮用负 margin 向右上溢出到面板内边距里，得到宽扁的胶囊热区：
                    // 宽 +inset / margin.r -inset，高 +rise / margin.t -rise ⇒ margin_box 不变，
                    // 不影响同行排布。右侧只吃半个 PAD：顶到边缘会盖掉圆角。
                    View::container(Layout::Row)
                        .fixed_w(close_w + close_inset)
                        .fixed_h(close_h)
                        .margin(Edges {
                            t: -close_rise,
                            r: -close_inset,
                            ..Default::default()
                        })
                        .radius(close_h * 0.5)
                        .bg(close_bg)
                        .border(
                            if close_down { c.accent } else { [0, 0, 0, 0] },
                            (1.0 * s).max(1.0),
                        )
                        .tag(SOFT_TAG_CLOSE)
                        .child(
                            View::leaf("✕", close_fg)
                                .font_size(13.0 * s)
                                .text_align(Align::Center)
                                .fill_cross()
                                .grow(),
                        ),
                ),
        );
        root = root.child(self.build_tabs(s, u, gap, hover, pressed));

        // 键位行：4 行画布 + 各行两端的功能键。行尾功能键一律 `grow` 吃掉本行富余，四行右缘才对齐。
        let mut idx = 0usize;
        for (r, count) in ROW_SLOTS.iter().copied().enumerate() {
            let mut row = View::container(Layout::Row).gap(gap).fill_cross();
            // 行首功能键
            match r {
                1 => row = row.child(self.fn_key(1, "Tab", 1.5, u, gap, s, hover, pressed)),
                2 => row = row.child(self.caps_key(1.75, u, gap, s, hover, pressed)),
                3 => row = row.child(self.shift_key(2.5, u, gap, s, hover)),
                _ => {}
            }
            for i in 0..count {
                if let Some(cap) = self.keys.get(idx + i) {
                    row = row.child(self.key_cap((idx + i) as i32, cap, u, s, hover, pressed));
                }
            }
            idx += count;
            // 行尾功能键。Del 排在 `\` 之后（第二行末尾），照 Windows 触摸键盘的布局。
            match r {
                0 => row = row.child(self.fn_key(0, "⌫", 3.0, u, gap, s, hover, pressed).grow()),
                1 => row = row.child(self.fn_key(4, "Del", 1.5, u, gap, s, hover, pressed).grow()),
                2 => {
                    row = row.child(
                        self.fn_key(2, "Enter", 3.25, u, gap, s, hover, pressed)
                            .grow(),
                    )
                }
                3 => row = row.child(self.shift_key(3.5, u, gap, s, hover).grow()),
                _ => {}
            }
            root = root.child(row);
        }

        // 底行：左组贴左、控制键组贴右（Ctrl / 空格 ┄┄ ◀ ▶ Esc）。`fill_cross` 把本行撑到键盘宽，
        // 中间 spacer 才分得到富余；少了它右组会跟着左组浮动。
        let mut bottom = View::container(Layout::Row).gap(gap).fill_cross();
        bottom = bottom.child(self.ctrl_key_latch(1.75, u, gap, s, hover));
        bottom = bottom.child(self.fn_key(3, "", 8.0, u, gap, s, hover, pressed));
        bottom = bottom.child(View::spacer().grow());
        bottom =
            bottom.child(self.ctrl_key(SOFT_TAG_PAGE_PREV, "◀", 1.5, u, gap, s, hover, pressed));
        bottom =
            bottom.child(self.ctrl_key(SOFT_TAG_PAGE_NEXT, "▶", 1.5, u, gap, s, hover, pressed));
        bottom = bottom.child(self.ctrl_key(SOFT_TAG_ESC, "Esc", 1.5, u, gap, s, hover, pressed));
        root = root.child(bottom);
        root
    }

    /// 各面标签的宽度（不含项间隙）。
    fn tab_widths(&self, s: f32) -> Vec<f32> {
        self.pages
            .iter()
            .map(|n| {
                self.renderer.measure_text_sized(n, TAB_FONT_DP * s).width + TAB_PAD_X_DP * s * 2.0
            })
            .collect()
    }

    /// 标签行的度量：`(各标签宽, 内容总宽, 视口宽, 要不要滚动箭头)`。
    ///
    /// 视口宽先于内容定下：整行宽度对齐键盘区，减去（需要时的）两个箭头即视口。
    /// 箭头位置因此只由键盘宽度决定，与有多少个面无关。
    fn tab_metrics(&self, s: f32, u: f32, gap: f32) -> (Vec<f32>, f32, f32, bool) {
        let widths = self.tab_widths(s);
        let n = widths.len();
        let content_w = widths.iter().sum::<f32>() + TAB_GAP_DP * s * n.saturating_sub(1) as f32;
        let full = KBD_UNITS * u + KBD_GAPS * gap;
        // 关闭按钮已经挪到拖动条那一行，标签行整行都归标签用。
        let need_scroll = content_w > full;
        let arrows = if need_scroll {
            (TAB_ARROW_W_DP * s + TAB_GAP_DP * s) * 2.0
        } else {
            0.0
        };
        let view_w = (full - arrows).max(0.0);
        (widths, content_w, view_w, need_scroll)
    }

    /// 箭头/滚轮一次滚多远：半个视口。
    fn tab_step(&self) -> f32 {
        let s = self.scale;
        let (_, _, view_w, _) = self.tab_metrics(s, UNIT_DP * s, GAP_DP * s);
        (view_w * 0.5).max(40.0 * s)
    }

    /// 当前允许的最大滚动量。
    fn tab_scroll_max(&self, s: f32, u: f32, gap: f32) -> f32 {
        let (_, content_w, view_w, _) = self.tab_metrics(s, u, gap);
        (content_w - view_w).max(0.0)
    }

    /// 滚动标签行（箭头与滚轮共用）。返回是否真的动了。
    fn scroll_tabs(&mut self, dx: f32) -> bool {
        let s = self.scale;
        let max = self.tab_scroll_max(s, UNIT_DP * s, GAP_DP * s);
        let next = (self.tab_scroll + dx).clamp(0.0, max);
        if (next - self.tab_scroll).abs() < 0.5 {
            return false;
        }
        self.tab_scroll = next;
        true
    }

    /// 按当前光标位置重算悬停。内容在鼠标底下动了（滚动、切面）时必须调：
    /// `WM_MOUSEMOVE` 只在光标移动时到达，否则高亮留在原位。
    #[cfg(windows)]
    fn refresh_hover(&self) -> bool {
        use crate::sys::{GetCursorPos, POINT};
        use windows::Win32::Graphics::Gdi::ScreenToClient;
        let mut m = self.mouse.borrow_mut();
        if m.hwnd == 0 {
            return false;
        }
        let hwnd = m.hwnd_handle();
        let mut p = POINT::default();
        unsafe {
            if GetCursorPos(&mut p).is_err() || !ScreenToClient(hwnd, &mut p).as_bool() {
                return false;
            }
        }
        let h = m.hit_at(p.x as f32, p.y as f32);
        if h == m.hover {
            return false;
        }
        m.hover = h;
        true
    }

    /// macOS 版：没有 `ScreenToClient`，光标减面板原点即客户区坐标。
    #[cfg(target_os = "macos")]
    fn refresh_hover(&self) -> bool {
        let (ox, oy) = self.window.origin_px();
        let (cx, cy) = crate::mac_panel::global_cursor_px(f64::from(self.scale));
        let mut m = self.mouse.borrow_mut();
        let h = m.hit_at((cx - ox) as f32, (cy - oy) as f32);
        if h == m.hover {
            return false;
        }
        m.hover = h;
        true
    }

    /// Linux 外部宿主形态：照 macOS 的算法，光标（addon 最近报来的指针）减面板左上角。
    ///
    /// 先问指针还在不在面板里：「最后指针」在 LEAVE 后不动，否则离开后的重绘会把最后停过的键重新点亮。
    #[cfg(all(target_os = "linux", ext_presenter))]
    fn refresh_hover(&self) -> bool {
        use crate::sys::{GetCursorPos, GetWindowRect, POINT, RECT};
        let mut m = self.mouse.borrow_mut();
        let hwnd = m.hwnd_handle();
        if !crate::ext_panel::pointer_inside(hwnd) {
            return false;
        }
        let (mut p, mut r) = (POINT::default(), RECT::default());
        unsafe {
            if GetCursorPos(&mut p).is_err() || GetWindowRect(hwnd, &mut r).is_err() {
                return false;
            }
        }
        let h = m.hit_at((p.x - r.left) as f32, (p.y - r.top) as f32);
        if h == m.hover {
            return false;
        }
        m.hover = h;
        true
    }

    #[cfg(not(any(windows, target_os = "macos", all(target_os = "linux", ext_presenter))))]
    fn refresh_hover(&self) -> bool {
        false
    }

    /// 把当前面滚进视野。只在换面时调用，见 [`Self::last_shown_page`]。
    fn scroll_current_into_view(&mut self, s: f32) {
        if self.pages.is_empty() {
            return;
        }
        let (u, gap) = (UNIT_DP * s, GAP_DP * s);
        let (widths, _, view_w, _) = self.tab_metrics(s, u, gap);
        self.tab_scroll = scroll_to_show(
            &widths,
            TAB_GAP_DP * s,
            self.current,
            self.tab_scroll,
            view_w,
        );
    }

    /// 标签行：一个裁剪视口，需要滚动时两侧加箭头：`‹` │ 视口 │ `›`（关闭按钮在拖动条那一行）。
    ///
    /// 视口用 `fixed_w(0) + grow` 吃掉剩余宽度：measure 时宽为 0，整行宽度与标签内容无关，
    /// 箭头位置恒定。内容用负 margin 左移实现滚动，超出部分由 [`View::clipped`] 裁掉。
    fn build_tabs(&self, s: f32, u: f32, gap: f32, hover: i32, pressed: i32) -> View {
        let c = &self.colors;
        let tab_gap = TAB_GAP_DP * s;
        let pad_x = TAB_PAD_X_DP * s;
        let (_, content_w, view_w, need_scroll) = self.tab_metrics(s, u, gap);
        let max_scroll = (content_w - view_w).max(0.0);
        let scroll = self.tab_scroll.clamp(0.0, max_scroll);

        let mut row = View::container(Layout::Row)
            .gap(tab_gap)
            .cross(Align::Center)
            .fill_cross();
        if need_scroll {
            row = row.child(self.tab_arrow(
                SOFT_TAG_TAB_LEFT,
                "‹",
                TAB_ARROW_W_DP * s,
                s,
                hover,
                pressed,
                scroll > 0.5,
            ));
        }

        // 视口内容：全部标签一字排开，整体左移 scroll。
        let mut inner = View::container(Layout::Row).gap(tab_gap).margin(Edges {
            l: -scroll,
            ..Default::default()
        });
        for (i, name) in self.pages.iter().enumerate() {
            let tag = SOFT_TAG_PAGE_BASE + i as i32;
            let active = i == self.current;
            let (bg, fg) = if active {
                (c.accent_soft, c.accent)
            } else if hover == tag {
                (c.keycap, c.ink)
            } else {
                ([0, 0, 0, 0], c.hint)
            };
            inner = inner.child(
                View::container(Layout::Row)
                    .bg(bg)
                    .radius(4.0 * s)
                    .pad(Edges::xy(pad_x, 5.0 * s))
                    .cross(Align::Center)
                    .tag(tag)
                    .child(
                        View::leaf(name, fg)
                            .font_size(TAB_FONT_DP * s)
                            .text_align(Align::Center),
                    ),
            );
        }
        row = row.child(
            View::container(Layout::Row)
                // 宽度 0 + grow：整行宽度不被内容撑开，富余在 arrange 时才分进来。
                .fixed_w(0.0)
                .grow()
                .clipped()
                // 视口本身也进命中表，让滚轮知道「鼠标在标签行上」。标签的命中区在它
                // 之后收集，会盖在上面，故点标签仍然点得到标签。
                .tag(SOFT_TAG_TAB_VIEWPORT)
                .child(inner),
        );

        if need_scroll {
            row = row.child(self.tab_arrow(
                SOFT_TAG_TAB_RIGHT,
                "›",
                TAB_ARROW_W_DP * s,
                s,
                hover,
                pressed,
                scroll < max_scroll - 0.5,
            ));
        }
        row
    }

    /// 标签行的滚动箭头。`live=false` 时画成淡的且不进命中表——到头了还能点只会让人
    /// 反复试探。
    #[allow(clippy::too_many_arguments)]
    fn tab_arrow(
        &self,
        tag: i32,
        text: &str,
        w: f32,
        s: f32,
        hover: i32,
        pressed: i32,
        live: bool,
    ) -> View {
        let c = &self.colors;
        let down = live && pressed == tag;
        let (bg, fg) = if down {
            (c.accent_soft, c.accent)
        } else if !live {
            // 到头了：淡化正常态文字色，不拿 `line` 当前景——边框色在浅色主题里几乎等于
            // 面板底色，箭头会像整个消失。
            ([0, 0, 0, 0], faded(c.hint, 150))
        } else if hover == tag {
            (c.hover, c.ink)
        } else {
            ([0, 0, 0, 0], c.hint)
        };
        let mut v = View::container(Layout::Row)
            .fixed_w(w)
            .radius(4.0 * s)
            .bg(bg)
            .pad(Edges::xy(2.0 * s, 4.0 * s))
            .cross(Align::Center)
            .child(
                View::leaf(text, fg)
                    .font_size(13.0 * s)
                    .text_align(Align::Center)
                    .fill_cross()
                    .grow(),
            );
        if live {
            v = v.tag(tag);
        }
        v
    }

    /// 一个符号键帽：角标（原物理键）+ 两档符号同时显示。上排小字是另一档，下排大字是当前档，
    /// 切层时两者互换视觉权重。
    fn key_cap(
        &self,
        tag: i32,
        cap: &SoftKeyCap,
        u: f32,
        s: f32,
        hover: i32,
        pressed: i32,
    ) -> View {
        let c = &self.colors;
        // 显示档含 CapsLock（键盘面的字母键），回送档不含——见 cap_layer / layer_shift。
        let shift = self.cap_layer(&cap.slot);
        let cur = cap.output(shift);
        let alt = cap.output(!shift);
        // 「空」只看当前档：当前档没有映射就按不出东西，哪怕另一档有。
        let dead = cur.is_none();
        let phys_down = self.down_slot.as_ref().is_some_and(|(s, _)| s == &cap.slot);
        let down = phys_down || pressed == tag;

        // 三档轻重：普通 → 悬停（底色微亮）→ 按下（柔和主色 + 主色描边 + 主色字）。
        // 按下不用实心 accent 填充（深色方块像坏键）；角标在按下态保持原色。
        let (bg, border, fg, hint) = if down {
            (c.accent_soft, c.accent, c.accent, c.hint)
        } else if dead {
            (c.keycap_dead, c.line, c.hint, c.hint)
        } else if hover == tag {
            (c.hover, c.line, c.ink, c.hint)
        } else {
            (c.keycap, c.line, c.ink, c.hint)
        };

        // 角标用键位名的可读形态（`grave` → `` ` ``），大写显示更像键帽刻字。
        let label = slot_label(&cap.slot);
        let sym = cur.unwrap_or("·");
        // 多字符 token 收窄字号，避免撑出键帽。
        let sym_px = if sym.chars().count() > 1 {
            12.0 * s
        } else {
            17.0 * s
        };

        // 上排：角标 + 另一档（另一档没有映射时留空，不画占位符——那会让人以为它能打）。
        let mut top = View::container(Layout::Row)
            .cross(Align::Start)
            .child(View::leaf(label, hint).font_size(9.0 * s))
            .child(View::spacer().grow());
        if let Some(a) = alt {
            let a_px = if a.chars().count() > 1 {
                8.5 * s
            } else {
                11.0 * s
            };
            top = top.child(View::leaf(a, hint).font_size(a_px));
        }

        let mut cell = View::container(Layout::Column)
            .fixed_w(u)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(border, (1.0 * s).max(1.0))
            .pad(Edges::xy(3.0 * s, 2.0 * s))
            .child(top.fill_cross())
            .child(
                View::leaf(sym, fg)
                    .font_size(sym_px)
                    .text_align(Align::Center)
                    .fill_cross()
                    .grow(),
            );
        // 空键位不进命中表：它点了也不该有反应，连 hover 高亮都不该给。
        if !dead {
            cell = cell.tag(tag);
        }
        cell
    }

    /// 会合成真实按键的功能键（退格 / Tab / 回车 / 空格 / Ins / Del）。
    #[allow(clippy::too_many_arguments)]
    fn fn_key(
        &self,
        fn_idx: usize,
        text: &str,
        units: f32,
        u: f32,
        gap: f32,
        s: f32,
        hover: i32,
        pressed: i32,
    ) -> View {
        let tag = SOFT_TAG_FN_BASE + fn_idx as i32;
        let down = pressed == tag;
        self.flat_key(tag, text, units, u, gap, s, hover, down)
    }

    /// Ctrl：粘滞修饰键。点一下亮起，下一次点键位就合成 `ctrl+键`，然后自动熄灭。
    /// 单独 tap 一次 Ctrl 没有意义，面板又没法「按住」，粘滞是软键盘表达修饰键的通行做法。
    fn ctrl_key_latch(&self, units: f32, u: f32, gap: f32, s: f32, hover: i32) -> View {
        let c = &self.colors;
        let on = self.ctrl_latched;
        let (bg, fg) = if on {
            (c.accent_soft, c.accent)
        } else if hover == SOFT_TAG_CTRL {
            (c.hover, c.ink)
        } else {
            (c.keycap_fn, c.fn_ink)
        };
        View::container(Layout::Row)
            .fixed_w(units * u + (units - 1.0) * gap)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(if on { c.accent } else { c.line }, (1.0 * s).max(1.0))
            .cross(Align::Center)
            .tag(SOFT_TAG_CTRL)
            .child(
                View::leaf("Ctrl", fg)
                    .font_size(11.5 * s)
                    .text_align(Align::Center)
                    .fill_cross()
                    .grow(),
            )
    }

    /// 面板控制键（翻页 / 关闭）：不合成按键，语义各自不同。
    #[allow(clippy::too_many_arguments)]
    fn ctrl_key(
        &self,
        tag: i32,
        text: &str,
        units: f32,
        u: f32,
        gap: f32,
        s: f32,
        hover: i32,
        pressed: i32,
    ) -> View {
        self.flat_key(tag, text, units, u, gap, s, hover, pressed == tag)
    }

    /// Caps 键：显示系统大写锁定，并在够得着的平台上切换它。
    ///
    /// 这不违反「不拦截 CapsLock」：禁的是拦截物理键（「翻转再回敲复原」已删除且不得重来）；
    /// 这里是用户点面板时主动敲一次 `vk:0x14`，物理 CapsLock 仍不接管，状态由轮询读回。
    ///
    /// [`CAPS_CLICKABLE`] 为假时（macOS）画成禁用态：键位留着、尺寸不变（否则两平台布局分家）；
    /// 不进命中表（同空键位）；`caps_on` 的高亮照旧。
    fn caps_key(&self, units: f32, u: f32, gap: f32, s: f32, hover: i32, pressed: i32) -> View {
        let tag = SOFT_TAG_FN_BASE + SOFT_FN_CAPS_INDEX as i32;
        let c = &self.colors;
        // 大写锁定是持续态：与标签行「当前面」同一套（柔和底 + 主色字 + 主色描边）。
        // `caps_on` 排在禁用之前：亮着的锁定态更该被看见。
        let (bg, fg) = if self.caps_on {
            (c.accent_soft, c.accent)
        } else if !CAPS_CLICKABLE {
            // 借空键位那套灰——本面板既有的「这个位置点了没用」的视觉词汇，不另发明一种。
            (c.keycap_dead, c.hint)
        } else if pressed == tag {
            (c.accent_soft, c.ink)
        } else if hover == tag {
            (c.hover, c.ink)
        } else {
            (c.keycap_fn, c.hint)
        };
        let cell = View::container(Layout::Row)
            .fixed_w(units * u + (units - 1.0) * gap)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(
                if self.caps_on { c.accent } else { c.line },
                (1.0 * s).max(1.0),
            )
            .cross(Align::Center)
            .child(
                View::leaf("Caps", fg)
                    .font_size(11.5 * s)
                    .text_align(Align::Center)
                    .fill_cross()
                    .grow(),
            );
        if CAPS_CLICKABLE { cell.tag(tag) } else { cell }
    }

    /// Shift 键：可点（锁定/解锁第二层），同时反映物理按住状态（两来源分开存，见 `shift_locked`）。
    /// 不拦截物理 Shift 的按键事件，面板只是跟随显示。
    fn shift_key(&self, units: f32, u: f32, gap: f32, s: f32, hover: i32) -> View {
        let c = &self.colors;
        let on = self.layer_shift();
        let dot_w = (4.0 * s).max(3.0);
        // 与 Caps 同款：持续态用柔和底 + 主色字，不做实心填充。
        let (bg, fg) = if on {
            (c.accent_soft, c.accent)
        } else if hover == SOFT_TAG_SHIFT {
            (c.hover, c.ink)
        } else {
            (c.keycap_fn, c.hint)
        };
        View::container(Layout::Row)
            .fixed_w(units * u + (units - 1.0) * gap)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(if on { c.accent } else { c.line }, (1.0 * s).max(1.0))
            // 必须给内边距：子节点从边缘开始排，否则最右的标记位压在边框上。
            .pad(Edges::xy(5.0 * s, 0.0))
            .cross(Align::Center)
            .tag(SOFT_TAG_SHIFT)
            // 锁定标记不拼进文字（整串居中会让文字来回跳）：左透明占位 + 居中文字 + 右标记位，
            // 只有标记颜色在变。
            .child(
                View::container(Layout::Row)
                    .fixed_w(dot_w)
                    .fixed_h(dot_w)
                    .radius(dot_w * 0.5),
            )
            .child(
                View::leaf("Shift", fg)
                    .font_size(11.5 * s)
                    .text_align(Align::Center)
                    .fill_cross()
                    .grow(),
            )
            .child(
                View::container(Layout::Row)
                    .fixed_w(dot_w)
                    .fixed_h(dot_w)
                    .radius(dot_w * 0.5)
                    .bg(if self.shift_locked {
                        c.accent
                    } else {
                        [0, 0, 0, 0]
                    }),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn flat_key(
        &self,
        tag: i32,
        text: &str,
        units: f32,
        u: f32,
        gap: f32,
        s: f32,
        hover: i32,
        down: bool,
    ) -> View {
        let c = &self.colors;
        // 与符号键帽同一套三档（见 key_cap），只是底色略深。
        let (bg, border, fg) = if down {
            (c.accent_soft, c.accent, c.accent)
        } else if hover == tag {
            (c.hover, c.line, c.ink)
        } else {
            (c.keycap_fn, c.line, c.fn_ink)
        };
        View::container(Layout::Row)
            .fixed_w(units * u + (units - 1.0) * gap)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(border, (1.0 * s).max(1.0))
            .cross(Align::Center)
            .tag(tag)
            .child(
                View::leaf(text, fg)
                    .font_size(11.5 * s)
                    .text_align(Align::Center)
                    .fill_cross()
                    .grow(),
            )
    }
}

/// 读物理 Shift 是否按住、大写锁定是否开着。无实现的平台返回 `None`，
/// [`SoftKeyboard::tick`] 见 `None` 即整段跳过（面板停在当前层，不会出错档）。
fn read_shift_caps() -> Option<(bool, bool)> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL, VK_SHIFT};
        // 高位 = 按住；低位 = toggle 态（大写锁定用的是这一位）。
        let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
        let caps = (GetKeyState(VK_CAPITAL.0 as i32) as u16 & 0x0001) != 0;
        Some((shift, caps))
    }
    #[cfg(target_os = "macos")]
    {
        crate::mac_panel::shift_caps_state()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// 让第 `cur` 个标签完整露出所需的滚动量（纯算术，宽度由 `scroll_current_into_view` 量好喂进来）。
fn scroll_to_show(widths: &[f32], gap: f32, cur: usize, scroll: f32, view_w: f32) -> f32 {
    if widths.is_empty() {
        return 0.0;
    }
    let cur = cur.min(widths.len() - 1);
    let content_w = widths.iter().sum::<f32>() + gap * (widths.len() - 1) as f32;
    let max = (content_w - view_w).max(0.0);
    let x0: f32 = widths[..cur].iter().sum::<f32>() + gap * cur as f32;
    let x1 = x0 + widths[cur];
    // 在左边就贴左露出，在右边就贴右露出；已经完整可见则一动不动。
    let next = if x0 < scroll {
        x0
    } else if x1 > scroll + view_w {
        x1 - view_w
    } else {
        scroll
    };
    next.clamp(0.0, max)
}

/// 把颜色淡化到指定不透明度（禁用态用）。文字色 alpha 由 `TextRenderer` 回写时混合，
/// 这里只改 alpha 通道，不必知道底色。
fn faded(c: [u8; 4], alpha: u8) -> [u8; 4] {
    [c[0], c[1], c[2], alpha]
}

/// 这个 tag 是不是标签行里的某一个面（`SOFT_TAG_PAGE_BASE + 下标`）。
///
/// 判据必须是闭区间：tag 空间分段分配（键位 / 翻页 / 关闭 / 标签行控件 / 功能键），
/// 写成 `tag >= SOFT_TAG_PAGE_BASE` 会把后面的段（如 `SOFT_TAG_TAB_VIEWPORT`）整段吞进来。
/// 多处共用此判据，别就地展开成 `>=`。
fn is_page_tag(tag: i32) -> bool {
    (SOFT_TAG_PAGE_BASE..SOFT_TAG_CLOSE).contains(&tag)
}

/// 按下这里是「抓住面板拖动」而不是「点了某个控件」。
///
/// 两种情形：`-1`（键位之间的缝）与 `SOFT_TAG_TAB_VIEWPORT`。后者是区域标记而非控件：
/// 标签的命中区盖在它上面，命中到视口就说明点的是标签行留白。
fn drags_panel(tag: i32) -> bool {
    tag < 0 || tag == SOFT_TAG_TAB_VIEWPORT
}

/// `w × h` 面板里按下会拖动的区域，切成互不重叠的矩形（位图内 `x, y, w, h`）。
///
/// 不另写一套几何：按命中区的边把面板切成网格，每格中心问一次 `drags_panel(hit_at(..))`
/// （与按下时的判据同源），再合并相邻格，矩形数压到几十个。
fn drag_regions_of(m: &SoftMouse, w: u32, h: u32) -> Vec<[i32; 4]> {
    let edges = |pick: fn(&Rect) -> (f32, f32), max: u32| {
        let mut v = vec![0, max as i32];
        for (_, r) in &m.hits {
            let (a, len) = pick(r);
            for e in [a, a + len] {
                v.push((e.round() as i32).clamp(0, max as i32));
            }
        }
        v.sort_unstable();
        v.dedup();
        v
    };
    let xs = edges(|r| (r.x, r.w), w);
    let ys = edges(|r| (r.y, r.h), h);
    let mut out: Vec<[i32; 4]> = Vec::new();
    // 上一行带延续下来、还能往下长的块在 `out` 里的下标，按 `(x, 宽)` 认。
    let mut open: Vec<usize> = Vec::new();
    for band in ys.windows(2) {
        let (y0, y1) = (band[0], band[1]);
        let cy = (y0 + y1) as f32 * 0.5;
        let mut runs: Vec<(i32, i32)> = Vec::new();
        for cell in xs.windows(2) {
            let (x0, x1) = (cell[0], cell[1]);
            if !drags_panel(m.hit_at((x0 + x1) as f32 * 0.5, cy)) {
                continue;
            }
            match runs.last_mut() {
                Some((_, end)) if *end == x0 => *end = x1,
                _ => runs.push((x0, x1)),
            }
        }
        let mut next_open = Vec::with_capacity(runs.len());
        for (x0, x1) in runs {
            let rect = [x0, y0, x1 - x0, y1 - y0];
            match open
                .iter()
                .copied()
                .find(|&i| out[i][0] == x0 && out[i][2] == x1 - x0 && out[i][1] + out[i][3] == y0)
            {
                Some(i) => {
                    out[i][3] += y1 - y0;
                    next_open.push(i);
                }
                None => {
                    next_open.push(out.len());
                    out.push(rect);
                }
            }
        }
        open = next_open;
    }
    out
}

// 调用点在 mouse_impl（Windows / Linux 外部宿主）与 mouse_macos；其余形态（开发机不带 linux-host）无人调
/// 这个控件在抬起时才触发，而不是按下就动手。普通键位必须按下即出字（打字跟手、长按重复）。
#[cfg_attr(
    not(any(windows, target_os = "macos", all(target_os = "linux", ext_presenter))),
    allow(dead_code)
)]
fn fires_on_release(tag: i32) -> bool {
    // 关闭：不可撤销，按下即关像「还没点就没了」，中途也来不及反悔。
    if tag == SOFT_TAG_CLOSE || tag == SOFT_TAG_ESC {
        return true;
    }
    // 切面类：按下即切面会重建面板并清掉按下态（`reset_mouse`），按下态闪一下就没了。
    tag == SOFT_TAG_PAGE_PREV || tag == SOFT_TAG_PAGE_NEXT || is_page_tag(tag)
}

/// 改变状态的键不重复——按住翻页键会让面飞速乱切。
fn repeats(tag: i32) -> bool {
    if let Some(idx) = tag.checked_sub(SOFT_TAG_FN_BASE)
        && idx >= 0
    {
        // 功能键里只有大写锁定（toggle）不重复。判据在 types 里，与键表同处。
        return fn_key_repeats(idx as usize);
    }
    (0..SOFT_TAG_PAGE_BASE).contains(&tag)
}

fn next_page(cur: usize, n: usize) -> i32 {
    if n == 0 { 0 } else { ((cur + 1) % n) as i32 }
}

fn prev_page(cur: usize, n: usize) -> i32 {
    if n == 0 {
        0
    } else {
        ((cur + n - 1) % n) as i32
    }
}

/// 键位名 → 键帽角标。符号键用它本来的样子，比 `lbracket` 直观。
fn slot_label(slot: &str) -> String {
    match slot {
        "grave" => "`".into(),
        "minus" => "-".into(),
        "equal" => "=".into(),
        "lbracket" => "[".into(),
        "rbracket" => "]".into(),
        "backslash" => "\\".into(),
        "semicolon" => ";".into(),
        "quote" => "'".into(),
        "comma" => ",".into(),
        "period" => ".".into(),
        "slash" => "/".into(),
        other => other.to_uppercase(),
    }
}

/// 记忆锚点（面板右下角）→ 本帧落点（左上角）。同 `Toolbar::origin_from_anchor`：
/// 锚右下角让尺寸变化朝屏幕内侧展开；必须每帧用当前 `w`/`h` 现算。
fn origin_from_anchor(right: i32, bottom: i32, w: u32, h: u32) -> (i32, i32) {
    (right - w as i32, bottom - h as i32)
}

/// 这一帧面板落在哪——三级优先级只此一处。
///
/// 两个消费者：[`SoftKeyboard::ensure_scale`] 要屏内一点查 DPI，[`SoftKeyboard::render`] 要算落点。
/// 两者不能共用一个函数（落点依赖尺寸，尺寸依赖缩放，缩放依赖在哪块屏），但优先级只写一次；
/// 各写一遍漏掉一级时，副屏首次打开会按主屏 DPI 排版。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// 用户摆过的位置，锚面板右下角（跨重启记忆）。
    Anchor { right: i32, bottom: i32 },
    /// 上一帧的落点（左上角）。同屏重开、hover 重绘走这里。
    LastOrigin { x: i32, y: i32 },
    /// 这块屏还没有记录：按协调器给的焦点屏工作区落默认位置。
    DefaultIn { work: Option<(i32, i32, i32, i32)> },
}

impl Placement {
    /// 三级优先级的唯一实现。拆成自由函数让测试打到真身（`SoftKeyboard` 要真窗口才能构造）。
    fn pick(
        anchor_br: Option<(i32, i32)>,
        origin: Option<(i32, i32)>,
        work_area: Option<(i32, i32, i32, i32)>,
    ) -> Self {
        match (anchor_br, origin) {
            // 锚点是用户的意图，压过 `origin`（那只是上一帧的结果）。
            (Some((right, bottom)), _) => Placement::Anchor { right, bottom },
            (None, Some((x, y))) => Placement::LastOrigin { x, y },
            (None, None) => Placement::DefaultIn { work: work_area },
        }
    }

    /// 取一个落在目标屏内的点，供 DPI 查询。`right`/`bottom` 是排他边界，必须退 1px，
    /// 否则贴边时会查到相邻那块屏的 DPI。
    fn probe_point(self) -> (i32, i32) {
        match self {
            Placement::Anchor { right, bottom } => (right - 1, bottom - 1),
            // 左上角是包含边界，本身就在面板内，不必退。
            Placement::LastOrigin { x, y } => (x, y),
            Placement::DefaultIn {
                work: Some((_, _, right, bottom)),
            } => (right - 1, bottom - 1),
            // 协调器没查到显示器：猜主屏，与 `default_origin` 退回 `SPI_GETWORKAREA` 的兜底一致。
            Placement::DefaultIn { work: None } => (0, 0),
        }
    }
}

/// 首次显示的位置：工作区底部居中，留一点边距。
///
/// `work` 是协调器按焦点显示器给的工作区 `(left, top, right, bottom)`。只有它缺席时才回退
/// `SPI_GETWORKAREA`，那个 API 恒取主屏（同 `Toolbar::corner_position`）。
fn default_origin(w: u32, h: u32, s: f32, work: Option<(i32, i32, i32, i32)>) -> (i32, i32) {
    let margin = (16.0 * s).round() as i32;
    let bottom_centered = |left: i32, top: i32, right: i32, bottom: i32| {
        let x = left + ((right - left) - w as i32) / 2;
        let y = bottom - h as i32 - margin;
        (x.max(left), y.max(top))
    };
    if let Some((left, top, right, bottom)) = work {
        return bottom_centered(left, top, right, bottom);
    }
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
        };
        let mut rc = windows::Win32::Foundation::RECT::default();
        if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rc as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
        {
            return bottom_centered(rc.left, rc.top, rc.right, rc.bottom);
        }
    }
    #[cfg(target_os = "macos")]
    if let Some((ax, ay, aw, ah)) = crate::mac_panel::work_area_px(f64::from(s)) {
        // 与 Windows 分支同一落点：工作区**底部居中**，留一点边距。
        let x = ax + ((aw as i32) - w as i32) / 2;
        let y = ay + ah as i32 - h as i32 - margin;
        return (x.max(ax), y.max(ay));
    }
    let _ = (w, h);
    (margin, margin)
}

// ───────────────────────── 鼠标 ─────────────────────────

/// Win32 消息 → [`SoftMouse`]。Linux 外部宿主形态同样走这一份：`ext_panel` 把 addon 报来的
/// 指针翻译成同一组 `WM_*`，`sys` 的 mock 给出指针与窗口矩形。
#[cfg(any(windows, all(target_os = "linux", ext_presenter)))]
mod mouse_impl {
    use super::{SoftMouse, drags_panel, fires_on_release, is_page_tag, repeat_params, repeats};
    use crate::sys::{
        GetCursorPos, GetWindowRect, HWND_TOPMOST, POINT, RECT, ReleaseCapture, SWP_NOACTIVATE,
        SWP_NOSIZE, SWP_NOZORDER, SetCapture, SetWindowPos, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MOUSELEAVE, WM_MOUSEMOVE, WM_MOUSEWHEEL, clamp_to_work_area,
    };
    #[cfg(not(windows))]
    use crate::sys::{HWND, LPARAM, LRESULT, ScreenToClient, WPARAM};
    use crate::window::WindowMouse;
    use wind_ui_types::{SOFT_TAG_TAB_LEFT, SOFT_TAG_TAB_RIGHT, SOFT_TAG_TAB_VIEWPORT};
    #[cfg(windows)]
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    #[cfg(windows)]
    use windows::Win32::Graphics::Gdi::ScreenToClient;

    fn pos(lparam: LPARAM) -> (f32, f32) {
        let x = (lparam.0 & 0xFFFF) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
        (x, y)
    }

    impl WindowMouse for SoftMouse {
        fn on_message(
            &mut self,
            _hwnd: HWND,
            msg: u32,
            _wparam: WPARAM,
            lparam: LPARAM,
        ) -> Option<LRESULT> {
            match msg {
                WM_MOUSEMOVE if self.dragging => {
                    let mut p = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                    }
                    let nx = self.origin.0 + (p.x - self.anchor.0);
                    let ny = self.origin.1 + (p.y - self.anchor.1);
                    let hwnd = self.hwnd_handle();
                    let (w, h) = unsafe {
                        let mut r = RECT::default();
                        if GetWindowRect(hwnd, &mut r).is_ok() {
                            ((r.right - r.left) as u32, (r.bottom - r.top) as u32)
                        } else {
                            (0, 0)
                        }
                    };
                    // 钳到工作区：面板比候选窗大得多，拖出屏幕就再也抓不回来了。
                    let (cx, cy) = clamp_to_work_area(nx, ny, w, h);
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            cx,
                            cy,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                    }
                    self.moved_to = Some((cx, cy));
                    Some(LRESULT(0))
                }
                WM_MOUSEMOVE => {
                    // 每次进来都补订：LEAVE 是一次性的，收到后系统就注销了。
                    self.arm_leave();
                    let (x, y) = pos(lparam);
                    let h = self.hit_at(x, y);
                    if h != self.hover {
                        self.hover = h;
                        self.dirty = true;
                    }
                    // 按住后移出该键：停止长按重复。后台窗口的 SetCapture 不可靠，靠「移出即停」+ 抬起兜底。
                    if self.pressed >= 0 && h != self.pressed {
                        self.pressed = -1;
                        self.repeat_at = None;
                        self.repeating = false;
                        self.dirty = true;
                    }
                    Some(LRESULT(0))
                }
                WM_LBUTTONDOWN => {
                    let (x, y) = pos(lparam);
                    let h = self.hit_at(x, y);
                    if drags_panel(h) {
                        // 空白处（标签行留白、键位之间的缝）→ 拖动整块面板，判据见 `drags_panel`。
                        // 这里用 SetCapture 是安全的：按下发生在本窗口内；它失效的是侦听窗口外的
                        // 点击（见 docs/architecture/ui-platform-notes.md 的「弹出菜单」），工具栏拖动同理。
                        let mut p = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut p);
                        }
                        let hwnd = self.hwnd_handle();
                        let mut r = RECT::default();
                        let origin = unsafe {
                            if GetWindowRect(hwnd, &mut r).is_ok() {
                                (r.left, r.top)
                            } else {
                                (p.x, p.y)
                            }
                        };
                        self.anchor = (p.x, p.y);
                        self.origin = origin;
                        self.dragging = true;
                        if self.hover != -1 {
                            self.hover = -1;
                            self.dirty = true;
                        }
                        unsafe {
                            let _ = SetCapture(hwnd);
                        }
                        return Some(LRESULT(0));
                    }
                    if h >= 0 {
                        self.pressed = h;
                        // 抬起才触发的控件在这里只记按下态（键帽会亮），动作留到 UP。
                        if !fires_on_release(h) {
                            self.clicked.push(h);
                        }
                        self.dirty = true;
                        if repeats(h) {
                            let (delay, _) = repeat_params();
                            self.repeat_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(delay),
                            );
                        }
                    }
                    Some(LRESULT(0))
                }
                // 光标离开面板：清掉悬停高亮（出界那一下没有 `WM_MOUSEMOVE`）。
                WM_MOUSELEAVE => {
                    self.leave_armed = false;
                    if self.hover != -1 {
                        self.hover = -1;
                        self.dirty = true;
                    }
                    // 按住后划出去再松手，我们收不到 UP，按下态也要一并收掉。
                    if self.pressed >= 0 && !self.dragging {
                        self.pressed = -1;
                        self.repeat_at = None;
                        self.repeating = false;
                        self.dirty = true;
                    }
                    Some(LRESULT(0))
                }
                // 滚轮在标签行上横向滚动。WM_MOUSEWHEEL 的 lparam 是屏幕坐标，须换算。
                // 不抢焦点的面板能收到滚轮全靠系统的「悬停即滚动非活动窗口」，故箭头必须一直留着。
                WM_MOUSEWHEEL => {
                    let mut p = POINT {
                        x: (lparam.0 & 0xFFFF) as i16 as i32,
                        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                    };
                    let hwnd = self.hwnd_handle();
                    unsafe {
                        let _ = ScreenToClient(hwnd, &mut p);
                    }
                    let hit = self.hit_at(p.x as f32, p.y as f32);
                    let on_tabs = hit == SOFT_TAG_TAB_VIEWPORT
                        || hit == SOFT_TAG_TAB_LEFT
                        || hit == SOFT_TAG_TAB_RIGHT
                        || is_page_tag(hit);
                    if on_tabs {
                        let delta = ((_wparam.0 >> 16) & 0xFFFF) as i16;
                        // 向前滚（正 delta）= 向左看，与横向列表的通行方向一致。
                        self.wheel += -delta as f32 / 120.0;
                    }
                    Some(LRESULT(0))
                }
                WM_LBUTTONUP => {
                    if self.dragging {
                        self.dragging = false;
                        unsafe {
                            let _ = ReleaseCapture();
                        }
                        // 抬起才记锚点：取实际窗口矩形的右下角，交给 tick 上报持久化。
                        let hwnd = self.hwnd_handle();
                        let mut r = RECT::default();
                        if unsafe { GetWindowRect(hwnd, &mut r) }.is_ok() {
                            self.dropped_at = Some((r.right, r.bottom));
                        }
                    }
                    if self.pressed >= 0 {
                        // 抬起才触发：必须仍停在按下的那个控件上，挪开再松手是反悔。
                        let (x, y) = pos(lparam);
                        if fires_on_release(self.pressed) && self.hit_at(x, y) == self.pressed {
                            self.clicked.push(self.pressed);
                        }
                        self.pressed = -1;
                        self.dirty = true;
                    }
                    self.repeat_at = None;
                    self.repeating = false;
                    Some(LRESULT(0))
                }
                _ => None,
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod mouse_macos {
    use super::{SoftMouse, drags_panel, fires_on_release, is_page_tag, repeat_params, repeats};
    use crate::mac_panel::{MouseEvent, MouseKind, PanelGeom, PanelMouse};
    use wind_ui_types::{SOFT_TAG_TAB_LEFT, SOFT_TAG_TAB_RIGHT, SOFT_TAG_TAB_VIEWPORT};

    /// AppKit 事件 → 同一个 [`SoftMouse`] 状态机，与 Windows 版（`mouse_impl`）逐条对位：
    ///
    /// | Win32 | macOS | 说明 |
    /// |---|---|---|
    /// | `WM_MOUSEMOVE` | `Move` / 未拖动时的 `Drag` | AppKit 在按住时发的是 `mouseDragged` |
    /// | `WM_LBUTTONDOWN` / `UP` | `Down` / `Up` | — |
    /// | `WM_MOUSELEAVE` + `TrackMouseEvent` 重订 | `Leave` | 跟踪区是常驻的，**不需要每次重订** |
    /// | `SetCapture` / `ReleaseCapture` | 无 | AppKit 按下后隐式把后续事件送回同一视图 |
    /// | `ScreenToClient`（滚轮用） | 无 | 滚轮事件的坐标本来就是客户区的 |
    /// | `SetWindowPos` + `clamp_to_work_area` | 返回落点，由 `mac_panel` 挪窗并钳制 | 可见区只有主线程问得到 |
    impl PanelMouse for SoftMouse {
        fn on_mouse(&mut self, ev: MouseEvent, geom: PanelGeom) -> Option<(i32, i32)> {
            match ev.kind {
                // 拖动中：跟着光标走。落点由 `mac_panel` 钳进可见区，真实位置经 `on_moved` 回来。
                MouseKind::Drag if self.dragging => {
                    let nx = self.origin.0 + (ev.sx - self.anchor.0);
                    let ny = self.origin.1 + (ev.sy - self.anchor.1);
                    Some((nx, ny))
                }
                // 未拖动的移动（含按住时的 Drag）：更新悬停，并处理「按住后挪开」。
                MouseKind::Move | MouseKind::Drag => {
                    let h = self.hit_at(ev.x, ev.y);
                    if h != self.hover {
                        self.hover = h;
                        self.dirty = true;
                    }
                    // 按住后移出该键：停止长按重复（抬起时也不再触发，见 `Up` 的同址判据）。
                    if self.pressed >= 0 && h != self.pressed {
                        self.pressed = -1;
                        self.repeat_at = None;
                        self.repeating = false;
                        self.dirty = true;
                    }
                    None
                }
                MouseKind::Down => {
                    let h = self.hit_at(ev.x, ev.y);
                    if drags_panel(h) {
                        // 空白处（标签行留白、键位之间的缝）→ 拖动整块面板。
                        // 判据是 `drags_panel` 而不是 `h < 0`，理由见那个函数。
                        self.anchor = (ev.sx, ev.sy);
                        self.origin = (geom.x, geom.y);
                        self.dragging = true;
                        if self.hover != -1 {
                            self.hover = -1;
                            self.dirty = true;
                        }
                        return None;
                    }
                    if h >= 0 {
                        self.pressed = h;
                        // 抬起才触发的控件在这里只记按下态（键帽会亮），动作留到 Up。
                        if !fires_on_release(h) {
                            self.clicked.push(h);
                        }
                        self.dirty = true;
                        if repeats(h) {
                            let (delay, _) = repeat_params();
                            self.repeat_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(delay),
                            );
                        }
                    }
                    None
                }
                MouseKind::Up => {
                    self.dragging = false;
                    if self.pressed >= 0 {
                        // 抬起才触发：必须仍停在按下的那个控件上，挪开再松手是反悔。
                        let h = self.hit_at(ev.x, ev.y);
                        if fires_on_release(self.pressed) && h == self.pressed {
                            self.clicked.push(self.pressed);
                        }
                        self.pressed = -1;
                        self.dirty = true;
                    }
                    self.repeat_at = None;
                    self.repeating = false;
                    None
                }
                // 光标离开面板：清掉悬停高亮（出界那一下没有移动事件）。
                MouseKind::Leave => {
                    if self.hover != -1 {
                        self.hover = -1;
                        self.dirty = true;
                    }
                    // 按住后划出去再松手，我们收不到 Up，按下态也要一并收掉。
                    if self.pressed >= 0 && !self.dragging {
                        self.pressed = -1;
                        self.repeat_at = None;
                        self.repeating = false;
                        self.dirty = true;
                    }
                    None
                }
                // 滚轮只在标签行上有意义。这里只累加格数，滚动在 `tick` 里做。
                MouseKind::Wheel => {
                    let h = self.hit_at(ev.x, ev.y);
                    let on_tabs = h == SOFT_TAG_TAB_VIEWPORT
                        || h == SOFT_TAG_TAB_LEFT
                        || h == SOFT_TAG_TAB_RIGHT
                        || is_page_tag(h);
                    if on_tabs {
                        // 向前滚 = 向左看，与横向列表的通行方向一致（同 Windows 分支）。
                        self.wheel += -ev.wheel;
                    }
                    None
                }
            }
        }

        fn on_moved(&mut self, x: i32, y: i32) {
            self.moved_to = Some((x, y));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::view::Rect;
        use wind_ui_types::{SOFT_TAG_CLOSE, SOFT_TAG_PAGE_BASE};

        /// 造一个装好命中区的 `SoftMouse`：tag 0（键位）占 (0,0,10,10)，
        /// tag 1（键位）占 (20,0,10,10)，关闭按钮占 (40,0,10,10)，
        /// 标签行视口占 (0,20,100,10)。其余位置命中 -1（留白）。
        fn mk() -> SoftMouse {
            SoftMouse {
                hits: vec![
                    (
                        0,
                        Rect {
                            x: 0.0,
                            y: 0.0,
                            w: 10.0,
                            h: 10.0,
                        },
                    ),
                    (
                        1,
                        Rect {
                            x: 20.0,
                            y: 0.0,
                            w: 10.0,
                            h: 10.0,
                        },
                    ),
                    (
                        SOFT_TAG_CLOSE,
                        Rect {
                            x: 40.0,
                            y: 0.0,
                            w: 10.0,
                            h: 10.0,
                        },
                    ),
                    (
                        SOFT_TAG_TAB_VIEWPORT,
                        Rect {
                            x: 0.0,
                            y: 20.0,
                            w: 100.0,
                            h: 10.0,
                        },
                    ),
                ],
                hover: -1,
                pressed: -1,
                ..Default::default()
            }
        }

        fn geom() -> PanelGeom {
            PanelGeom {
                x: 100,
                y: 200,
                w: 300,
                h: 150,
            }
        }

        fn ev(kind: MouseKind, x: f32, y: f32) -> MouseEvent {
            MouseEvent {
                kind,
                x,
                y,
                sx: 0,
                sy: 0,
                wheel: 0.0,
            }
        }

        fn ev_screen(kind: MouseKind, sx: i32, sy: i32) -> MouseEvent {
            MouseEvent {
                kind,
                x: 0.0,
                y: 0.0,
                sx,
                sy,
                wheel: 0.0,
            }
        }

        /// 键位按下即触发（`fires_on_release` 为假），与 Windows 侧 `WM_LBUTTONDOWN` 同步。
        #[test]
        fn key_fires_on_press() {
            let mut m = mk();
            assert_eq!(m.on_mouse(ev(MouseKind::Down, 5.0, 5.0), geom()), None);
            assert_eq!(m.pressed, 0);
            assert_eq!(m.clicked, vec![0], "键位按下即入队");
        }

        /// 关闭按钮抬起才触发，且必须仍停在它上面——按下后挪开再松手是「反悔」。
        #[test]
        fn close_fires_on_release_only_when_still_on_it() {
            let mut m = mk();
            m.on_mouse(ev(MouseKind::Down, 45.0, 5.0), geom());
            assert_eq!(m.pressed, SOFT_TAG_CLOSE);
            assert!(m.clicked.is_empty(), "按下不该立刻触发关闭");
            m.on_mouse(ev(MouseKind::Up, 45.0, 5.0), geom());
            assert_eq!(m.clicked, vec![SOFT_TAG_CLOSE]);

            // 反悔：按下后移到别处再松手。
            let mut m = mk();
            m.on_mouse(ev(MouseKind::Down, 45.0, 5.0), geom());
            m.on_mouse(ev(MouseKind::Up, 5.0, 5.0), geom());
            assert!(m.clicked.is_empty(), "挪开再松手不该触发");
        }

        /// 留白与标签行留白都拖整块面板；真控件不拖。判据同 `drags_panel`。
        #[test]
        fn blank_and_tab_viewport_start_a_drag() {
            for (x, y, what) in [(70.0, 5.0, "键位之间的缝"), (50.0, 25.0, "标签行留白")]
            {
                let mut m = mk();
                assert_eq!(m.on_mouse(ev(MouseKind::Down, x, y), geom()), None);
                assert!(m.dragging, "{what} 应当开始拖动");
                assert_eq!(m.origin, (100, 200), "记的是按下时的窗口左上");
            }
            let mut m = mk();
            m.on_mouse(ev(MouseKind::Down, 5.0, 5.0), geom());
            assert!(!m.dragging, "键位不该被当成拖动，否则点了就只会拖窗口");
        }

        /// 拖动落点 = 按下时窗口左上 + 光标位移。
        #[test]
        fn drag_offsets_by_cursor_delta() {
            let mut m = mk();
            let mut down = ev(MouseKind::Down, 70.0, 5.0);
            down.sx = 500;
            down.sy = 600;
            m.on_mouse(down, geom());
            let want = m.on_mouse(ev_screen(MouseKind::Drag, 530, 580), geom());
            assert_eq!(want, Some((130, 180)), "100+30, 200-20");
        }

        /// `moved_to` 记的必须是钳制后的真实落点，否则下一帧 `render` 会把面板送回屏外。
        #[test]
        fn on_moved_records_the_clamped_position() {
            let mut m = mk();
            m.on_mouse(ev(MouseKind::Down, 70.0, 5.0), geom());
            m.on_mouse(ev_screen(MouseKind::Drag, 9999, 9999), geom());
            assert_eq!(m.moved_to, None, "拖动本身不写落点");
            m.on_moved(42, 43);
            assert_eq!(m.moved_to, Some((42, 43)));
        }

        /// 按住后移出该键：停掉按下态与长按重复。
        #[test]
        fn moving_off_a_pressed_key_cancels_it() {
            let mut m = mk();
            m.on_mouse(ev(MouseKind::Down, 5.0, 5.0), geom());
            assert!(m.repeat_at.is_some(), "键位可重复，应已排上首次延迟");
            m.on_mouse(ev(MouseKind::Drag, 25.0, 5.0), geom());
            assert_eq!(m.pressed, -1);
            assert!(m.repeat_at.is_none());
        }

        /// 离开面板：清悬停，并收掉按下态（划出去再松手收不到 Up）。
        #[test]
        fn leave_clears_hover_and_press() {
            let mut m = mk();
            m.on_mouse(ev(MouseKind::Move, 5.0, 5.0), geom());
            assert_eq!(m.hover, 0);
            m.on_mouse(ev(MouseKind::Down, 5.0, 5.0), geom());
            m.on_mouse(ev(MouseKind::Leave, 0.0, 0.0), geom());
            assert_eq!(m.hover, -1, "残留高亮正是这条分支要消灭的");
            assert_eq!(m.pressed, -1);
        }

        /// 滚轮只在标签行上累加；落在键位上不动。方向与横向列表一致（向前滚 = 向左看）。
        #[test]
        fn wheel_only_counts_over_the_tab_row() {
            let mut m = mk();
            let mut w = ev(MouseKind::Wheel, 50.0, 25.0);
            w.wheel = 2.0;
            m.on_mouse(w, geom());
            assert_eq!(m.wheel, -2.0);

            let mut m = mk();
            let mut w = ev(MouseKind::Wheel, 5.0, 5.0);
            w.wheel = 2.0;
            m.on_mouse(w, geom());
            assert_eq!(m.wheel, 0.0, "键位上滚轮不该滚标签行");
        }

        /// 标签（面）也算标签行，滚轮在它上面同样有效。
        #[test]
        fn wheel_counts_over_a_page_tab() {
            let mut m = mk();
            m.hits.push((
                SOFT_TAG_PAGE_BASE,
                Rect {
                    x: 0.0,
                    y: 20.0,
                    w: 30.0,
                    h: 10.0,
                },
            ));
            let mut w = ev(MouseKind::Wheel, 10.0, 25.0);
            w.wheel = 1.0;
            m.on_mouse(w, geom());
            assert_eq!(m.wheel, -1.0);
        }
    }
}

#[cfg(not(any(windows, all(target_os = "linux", ext_presenter))))]
impl crate::window::WindowMouse for SoftMouse {
    // 句柄类型走 `crate::sys::` 而非 `crate::window::`：后者是私有 import，引用会 E0603，
    // 且只在非 Windows 上编译时才现形。
    fn on_message(
        &mut self,
        _hwnd: crate::sys::HWND,
        _msg: u32,
        _wparam: crate::sys::WPARAM,
        _lparam: crate::sys::LPARAM,
    ) -> Option<crate::sys::LRESULT> {
        None
    }
}

#[cfg(test)]
#[path = "soft_keyboard_tests.rs"]
mod tests;

// 激活键文字色：强调色底上的字取 on_accent，不能与底色相同（见 `Colors::from_theme`）。
#[cfg(test)]
mod active_key_color_tests {
    use super::Colors;

    fn theme(name: &str) -> wind_theme::Resolved {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes");
        wind_theme::load_resolved(&dir, name, false).unwrap()
    }

    #[test]
    fn active_key_text_is_on_accent_not_accent() {
        for name in ["_base", "msime", "default"] {
            let t = theme(name);
            let c = Colors::from_theme(&t);
            assert_eq!(
                c.on_accent, t.palette["on_accent"],
                "{name}：激活键文字应取 on_accent"
            );
            assert_ne!(c.on_accent, c.accent, "{name}：激活键字色不能与底色相同");
        }
    }
}
