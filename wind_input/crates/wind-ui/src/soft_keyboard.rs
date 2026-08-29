//! 软键盘面板窗口。
//!
//! 一块常驻的符号面板：按物理键盘布局画出当前面的键位，鼠标可点、物理按键会高亮。
//!
//! # 布局零配置
//!
//! **软键盘的布局就是键盘的布局**——键位坐标由键名唯一决定，配置里没有行列。
//! 本文件的 [`ROW_SLOTS`] 是那份坐标表在渲染端的镜像：它只描述「第几行有几个键位」，
//! 键位名本身随 [`SoftKeyCap`] 一起从协调器下发，两边不各存一份名字。
//!
//! # 绘制成本
//!
//! 面板内容几乎恒定，每帧只有一两个键在变（hover / 按下 / 切层），但当前实现是
//! **每次都重建整棵 View 树再整块重绘**——没有做「底板烘焙 + 单键重绘」。
//!
//! 这样够用的前提是文本测量在 `TextRenderer` 里已有缓存：47 个键帽都是
//! `fixed_w`/`fixed_h`，重排只是算矩形，真正贵的字形测量走的是缓存。
//! ⚠️ 若哪天把键帽改成按内容自适应宽度，或去掉测量缓存，这条前提就不成立了，
//! 鼠标划过面板会明显发烫——那时再上单键重绘，别提前优化。
//!
//! # 不抢焦点是承重墙
//!
//! 窗口沿用浮层那组样式（`WS_EX_NOACTIVATE` 等，见 [`LayeredWindow`]）。
//! 「切换焦点自动关闭」这条行为完全依赖它：面板一旦可激活，用户点它上面任何一个键
//! 都是在改变焦点，它会把自己关掉。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use crate::text::dwrite::TextRenderer;
use crate::view::{Align, Edges, Layout, Rect, View};
use crate::window::LayeredWindow;
use wind_ui_types::{
    SOFT_FN_CAPS_INDEX, SOFT_FN_KEYS, SOFT_TAG_CLOSE, SOFT_TAG_FN_BASE, SOFT_TAG_PAGE_BASE,
    SOFT_TAG_PAGE_NEXT, SOFT_TAG_PAGE_PREV, SOFT_TAG_SHIFT, SOFT_TAG_TAB_LEFT, SOFT_TAG_TAB_RIGHT,
    SOFT_TAG_TAB_VIEWPORT, SoftKeyCap, UiEvent, slot_layer,
};

/// 每行的键位个数，与 `wind_softkeyboard::KEY_ROWS` 同构（数字行 / QWERTY / ASDF / ZXCV）。
///
/// 只存**个数**不存键位名：名字随 `SoftKeyCap` 下发，渲染端再存一份就会分叉。
const ROW_SLOTS: [usize; 4] = [13, 13, 11, 10];

/// 键位单元（dp）。设计稿定的 42，下限约 34——再小 CJK 符号就不准了。
const UNIT_DP: f32 = 42.0;
/// 键间距（dp）。
const GAP_DP: f32 = 5.0;
/// 面板内边距（dp）。
const PAD_DP: f32 = 12.0;
/// 键盘区宽度（单位数 + 间隙数）：最宽的是第一行（13 键位 + 2u 退格 = 15u，13 个间隙）。
/// 标签行与底行都按它对齐，面板宽度就只由键盘决定。
const KBD_UNITS: f32 = 15.0;
const KBD_GAPS: f32 = 13.0;
/// 标签行度量（dp）。**必须是模块常量而不是两个函数各写一份**——
/// [`SoftKeyboard::visible_tabs`] 与 [`SoftKeyboard::build_tabs`] 靠它们算出同一个可见集。
const TAB_GAP_DP: f32 = 2.0;
const TAB_PAD_X_DP: f32 = 9.0;
const TAB_FONT_DP: f32 = 12.5;
const TAB_ARROW_W_DP: f32 = 22.0;
const TAB_CLOSE_W_DP: f32 = 27.0;

/// 长按重复：首次延迟与间隔（毫秒）。
///
/// ⚠️ 真实实现应读 `SPI_GETKEYBOARDDELAY` / `SPI_GETKEYBOARDSPEED`——自定常数一定会和
/// 物理键长按的手感对不上。这里取一组接近 Windows 默认的值，读系统设置见 [`repeat_params`]。
const REPEAT_DELAY_MS: u64 = 500;
const REPEAT_RATE_MS: u64 = 33;
/// 物理按键高亮的持续时间（毫秒）。长按时被连发的 keydown 不断续期。
const KEY_FLASH_MS: u64 = 140;
/// 物理 Shift / 大写锁定的跟随节奏（毫秒）。**仅面板可见期间生效**。
const MODIFIER_POLL_MS: u64 = 40;

/// 面板配色（全部走主题键，未配则回落到与候选窗同族的中性色）。
#[derive(Clone)]
struct Colors {
    panel: [u8; 4],
    keycap: [u8; 4],
    keycap_fn: [u8; 4],
    keycap_dead: [u8; 4],
    line: [u8; 4],
    ink: [u8; 4],
    /// 角标色。**必须比正文更淡**，又要在深色底上仍可读——直接沿用 `dim` 会偏亮。
    hint: [u8; 4],
    accent: [u8; 4],
    accent_soft: [u8; 4],
    on_accent: [u8; 4],
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
    hwnd: isize,
    /// 拖动中；`anchor` 是按下时的屏幕光标，`origin` 是按下时的窗口左上。
    dragging: bool,
    anchor: (i32, i32),
    origin: (i32, i32),
    /// 拖动落点，供窗口在 tick 里收回去记住位置。
    moved_to: Option<(i32, i32)>,
    /// 未消费的滚轮量（单位：一格）。窗口过程只累加，真正滚动在 `tick` 里做——
    /// 滚动要改 `tab_scroll` 并重绘，而窗口过程拿不到 `&mut SoftKeyboard`。
    wheel: f32,
}

impl SoftMouse {
    /// 还原窗口句柄。存 `isize` 是为了让本结构在非 Windows 上也能构造。
    #[cfg(windows)]
    fn hwnd_handle(&self) -> crate::sys::HWND {
        crate::sys::HWND(self.hwnd as *mut core::ffi::c_void)
    }

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
/// ★ 不自定常数——鼠标长按与物理键长按必须同一个手感，而物理那条走的就是系统设置。
fn repeat_params() -> (u64, u64) {
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
    (REPEAT_DELAY_MS, REPEAT_RATE_MS)
}

/// 软键盘面板窗口。
pub struct SoftKeyboard {
    window: LayeredWindow,
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
    /// 与 `shift_held` 分开存：物理是临时的、点击是锁定的，合成一个布尔就无法表达
    /// 「按住物理 Shift 松开后该回哪一层」。有效层取两者的或。
    shift_locked: bool,
    /// 系统大写锁定当前是否开着（Caps 键据此高亮）。
    caps_on: bool,
    /// 当前面是键盘面：CapsLock 参与字母键的档位显示（见 [`Self::cap_layer`]）。
    send_keys: bool,
    /// 标签行第一个可见标签的下标（面多到一行放不下时才非 0）。
    /// 标签行的水平滚动量（设备像素）。
    ///
    /// 从「按项取舍」改成按像素，是因为项宽不一时按项永远对不齐容器边缘——右侧那个
    /// 箭头会随着当前显示了哪几项而左右跳。有了 [`View::clipped`] 就没必要迁就了。
    tab_scroll: f32,
    /// 上一次已经「滚到可见」的面。
    ///
    /// ★ 只在**面变了**的时候才把当前面拉进视野。无条件每帧拉一次的后果是用户根本
    /// 滚不动标签行——手一松就被拽回当前面那里。滚动是用户的意图，换面才是我们的。
    last_shown_page: usize,
    /// 物理按键按下中的键位名 + 该高亮的到期时刻。
    ///
    /// ★ 高亮**不靠 keyup 配对**：协调器只在 keydown 被调用，物理抬起根本不经过我们。
    /// 改为「按下即高亮、到期自清」——物理长按会连发 keydown 不断续期，视觉上就是
    /// 持续按下，松开后一个周期内自然熄灭。
    down_slot: Option<(String, std::time::Instant)>,

    visible: bool,
    /// 用户拖动后的位置；None = 首次按屏幕默认锚点摆放。
    origin: Option<(i32, i32)>,
}

impl SoftKeyboard {
    const DEFAULT_FONT_PX: f32 = 15.0;

    pub fn new(events: Sender<UiEvent>) -> Result<Self, String> {
        let scale = crate::dpi::scale_for_point(0, 0);
        let window = LayeredWindow::create(None, 700, 300, "WindInputSoftKeyboard")?;
        let renderer = TextRenderer::new("Microsoft YaHei UI", Self::DEFAULT_FONT_PX * scale)?;
        let mouse = Rc::new(RefCell::new(SoftMouse {
            hover: -1,
            pressed: -1,
            #[cfg(windows)]
            hwnd: window.hwnd().0 as isize,
            #[cfg(not(windows))]
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
            tab_scroll: 0.0,
            last_shown_page: usize::MAX,
            down_slot: None,
            visible: false,
            origin: None,
        })
    }

    /// 取主题色。
    ///
    /// ★ **复用主题里已有的「键盘域」语义色**（`key_bg` / `key_text` / `key_hint` /
    /// `key_special_bg` / `key_pressed_bg` / `keyboard_bg`），不另立一套。那几个键本是
    /// 给 Android 软键盘定的，注释还写着「桌面没有这些控件」——现在桌面有了，而它们
    /// 描述的是同一件东西：一块键盘长什么样。
    ///
    /// 这一步不是洁癖：键盘域的值一律以 `${var}` 引用主色，派生主题（amber/jade/violet）
    /// 只覆盖 primary/accent 系就能让键盘跟着变。自立门户的 `softkb_*` 在任何主题文件里
    /// 都没有定义，于是**永远落到硬编码兜底**——换成橙色主题，软键盘还是蓝的。
    ///
    /// `softkb_*` 仍留在链首，给「只想单独调桌面软键盘」的人一个口子。
    pub fn set_theme(&mut self, theme: &wind_theme::Resolved) {
        let d = Colors::default();
        // 专用覆盖 → 键盘域语义色 → 候选窗同类色 → 硬编码兜底。
        let pick =
            |own: &str, kbd: &str, fallback: [u8; 4]| theme.color(own, theme.color(kbd, fallback));
        self.colors = Colors {
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
            on_accent: pick(
                "softkb_active_text",
                "accent_text",
                theme.color("candidate_selected_text", d.on_accent),
            ),
        };
        if self.visible {
            self.render();
        }
    }

    /// 当前 Shift 档：物理按住 或 面板上锁定，任一即可。
    ///
    /// ★ **不含 CapsLock**。它同时是点击回送给协调器的 `shift`，而那边会据此合成
    /// `shift+q`——Caps 开着再加 Shift 在真实键盘上出的是**小写**，把 caps 混进来
    /// 会让「Caps 开时点字母」恰好出反。CapsLock 只影响显示，见 [`Self::cap_layer`]。
    fn layer_shift(&self) -> bool {
        self.shift_held || self.shift_locked
    }

    /// 一个键位**显示**哪一档（两种面都生效，判据见 [`slot_layer`]）。
    fn cap_layer(&self, slot: &str) -> bool {
        slot_layer(slot, self.layer_shift(), self.caps_on)
    }

    /// 点击这个键位时回送给协调器的 `shift`。
    ///
    /// ★ 两种面语义不同，而这正是「显示与输出不分叉」的落点：
    /// - **符号面**查表直接上屏 ⇒ 回送**显示档**，画着什么就出什么。
    /// - **键盘面**合成真实按键 ⇒ 回送**物理 Shift**，CapsLock 由系统自己应用；
    ///   把 caps 混进来，Caps 开时合成的 `shift+q` 恰好出小写，正好是反的。
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
        // 面的类型直接决定点击是「上屏符号」还是「合成按键」，值得留一行——
        // 「CapsLock 不生效」那次排查，正是这一行一眼指出用户当时在符号面。
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

    pub fn hide(&mut self) {
        if self.visible {
            self.visible = false;
            self.reset_mouse();
            self.window.hide();
        }
    }

    /// 清掉悬停/按下残留。
    ///
    /// 面板不追 `WM_MOUSELEAVE`（那要先 `TrackMouseEvent` 订阅，为一格高亮残留多接一条
    /// 消息不划算），改为在显示/隐藏这两个边界上重置——残留最多活到下次打开前，看不见。
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
            Some(std::time::Instant::now() + std::time::Duration::from_millis(MODIFIER_POLL_MS)),
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
        // 拖动落点：窗口自己记住，下次显示不再回到默认锚点。
        if let Some(pos) = self.mouse.borrow_mut().moved_to.take() {
            self.origin = Some(pos);
        }

        // 滚轮：窗口过程只累加格数，这里换算成像素并重绘。
        let wheel = std::mem::take(&mut self.mouse.borrow_mut().wheel);
        if wheel != 0.0 {
            let step = self.tab_step();
            if self.scroll_tabs(wheel * step * 0.5) {
                self.render();
            }
        }

        // 物理 Shift 与大写锁定的跟随。
        //
        // ★ 为什么要轮询：单独按 Shift 不出字，那个 keydown 根本不会转发到协调器——
        // 我们只在「Shift+某键」时才从修饰位里知道它按住了。而用户要的是「一按 Shift
        // 面板立刻切显上档」。故面板可见期间以固定节奏查一次键盘状态。
        //
        // ⚠️ 这是本仓「UI 已改事件驱动、不做空转轮询」的一处**有意例外**，边界写死在
        // 「面板可见时」：面板一关，`next_deadline` 立即不再登记这个节奏，线程回到全静默。
        if let Some((held, caps)) = read_shift_caps() {
            if held != self.shift_held {
                // 松开物理 Shift 时把面板上的锁定一并解除。
                //
                // 两个来源本可以各管各的，但那样会出现「点了面板 Shift 锁在上档，又按了
                // 一下物理 Shift，松开后仍停在上档」——用户刚做完一个「按下又松开」的
                // 完整动作，界面却没回来，看起来就是卡住了。以物理动作为准更符合直觉。
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
        if tag == SOFT_TAG_CLOSE {
            let _ = self.events.send(UiEvent::SoftKeyboardClose);
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
        if tag >= SOFT_TAG_PAGE_BASE {
            let i = (tag - SOFT_TAG_PAGE_BASE) as usize;
            let _ = self.events.send(UiEvent::SoftKeyboardPage(i));
            return;
        }
        // 键位：tag 即下标。
        if let Some(cap) = self.keys.get(tag as usize) {
            // 空键位不回送：面板上它是灰的，点了什么都不该发生。
            // 判空用**显示档**——与键帽画成灰的那条判据同源。
            if cap.output(self.cap_layer(&cap.slot)).is_some() {
                let _ = self.events.send(UiEvent::SoftKeyboardKey {
                    slot: cap.slot.clone(),
                    shift: self.click_shift(&cap.slot),
                });
            }
        }
    }

    fn ensure_scale(&mut self) {
        let (x, y) = self.origin.unwrap_or((0, 0));
        let sc = crate::dpi::scale_for_point(x, y);
        if (sc - self.scale).abs() > 0.01 {
            self.scale = sc;
            self.renderer.set_base_size(Self::DEFAULT_FONT_PX * sc);
        }
    }

    /// 重排 + 重画 + 上屏。
    fn render(&mut self) {
        if !self.visible || self.keys.is_empty() {
            return;
        }
        self.ensure_scale();
        let s = self.scale;
        // ★ 只在**换面**时把当前面拉进视野。每帧无条件拉的后果是用户滚不动标签行——
        // 手一松就被拽回当前面。滚动是用户的意图，换面才是我们的。
        if self.current != self.last_shown_page {
            self.scroll_current_into_view(s);
            self.last_shown_page = self.current;
        }
        let mut root = self.build(s);
        root.layout(0.0, 0.0, &self.renderer);
        let (w_f, h_f) = root.measured_size();
        let w = (w_f.ceil() as u32).max(64);
        let h = (h_f.ceil() as u32).max(64);

        self.window.resize(w, h);
        {
            let buf = self.window.buffer_mut();
            let n = (w * h * 4) as usize;
            buf[..n].fill(0);
            root.paint(buf, w, h, &self.renderer);
        }
        // 命中区随每次重排更新——用上一次的会让「切面后点第一个键出上一面的符号」。
        {
            let mut hits = Vec::new();
            root.collect_hits(&mut hits);
            self.mouse.borrow_mut().hits = hits;
        }
        if let Err(e) = self.window.update() {
            tracing::warn!("软键盘: 窗口更新失败: {e}");
            return;
        }
        let (x, y) = self.origin.unwrap_or_else(|| default_origin(w, h, s));
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

        root = root.child(self.build_tabs(s, u, gap, hover));

        // 键位行：4 行画布 + 各行两端的功能键。
        let mut idx = 0usize;
        for (r, count) in ROW_SLOTS.iter().copied().enumerate() {
            let mut row = View::container(Layout::Row).gap(gap);
            // 行首功能键
            match r {
                1 => row = row.child(self.fn_key(1, "Tab", 1.5, u, gap, s, hover, pressed)),
                2 => row = row.child(self.caps_key(1.75, u, gap, s, hover, pressed)),
                3 => row = row.child(self.shift_key(2.25, u, gap, s, hover)),
                _ => {}
            }
            for i in 0..count {
                if let Some(cap) = self.keys.get(idx + i) {
                    row = row.child(self.key_cap((idx + i) as i32, cap, u, s, hover, pressed));
                }
            }
            idx += count;
            // 行尾功能键
            match r {
                0 => row = row.child(self.fn_key(0, "⌫", 2.0, u, gap, s, hover, pressed)),
                2 => row = row.child(self.fn_key(2, "Enter", 2.25, u, gap, s, hover, pressed)),
                3 => row = row.child(self.shift_key(2.75, u, gap, s, hover)),
                _ => {}
            }
            root = root.child(row);
        }

        // 底行：**左组贴左、控制键组贴右**（Ins / Del / 空格 ┄┄ ◀ ▶ Esc）。
        //
        // 「左固定 + 中间自由 + 右固定」不需要新的布局原语：`fill_cross` 把本行撑到列的
        // 内容宽（= 最宽那行键盘的宽度），中间的 `spacer().grow()` 吃掉富余，右组就恒
        // 贴右缘。少了 `fill_cross`，行宽只等于自身内容宽，spacer 分不到一个像素，右组
        // 就跟着左组浮动——面板越宽错得越明显。
        let mut bottom = View::container(Layout::Row).gap(gap).fill_cross();
        bottom = bottom.child(self.fn_key(4, "Ins", 1.5, u, gap, s, hover, pressed));
        bottom = bottom.child(self.fn_key(5, "Del", 1.5, u, gap, s, hover, pressed));
        bottom = bottom.child(self.fn_key(3, "", 7.0, u, gap, s, hover, pressed));
        bottom = bottom.child(View::spacer().grow());
        bottom = bottom.child(self.ctrl_key(SOFT_TAG_PAGE_PREV, "◀", 1.5, u, gap, s, hover));
        bottom = bottom.child(self.ctrl_key(SOFT_TAG_PAGE_NEXT, "▶", 1.5, u, gap, s, hover));
        bottom = bottom.child(self.ctrl_key(SOFT_TAG_CLOSE, "Esc", 1.5, u, gap, s, hover));
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
    /// ★ 视口宽是**先于内容**定下来的：整行宽度对齐键盘区，减去关闭按钮与（需要时的）
    /// 两个箭头，剩下的就是视口。所以右侧那些控件的位置只由键盘宽度决定，**与有多少个
    /// 面、当前显示了哪几个都无关**——上一版它们跟着内容宽度漂，正是因为没有这一步。
    fn tab_metrics(&self, s: f32, u: f32, gap: f32) -> (Vec<f32>, f32, f32, bool) {
        let widths = self.tab_widths(s);
        let n = widths.len();
        let content_w = widths.iter().sum::<f32>() + TAB_GAP_DP * s * n.saturating_sub(1) as f32;
        let full = KBD_UNITS * u + KBD_GAPS * gap;
        let close = TAB_CLOSE_W_DP * s + TAB_GAP_DP * s;
        let need_scroll = content_w > full - close;
        let arrows = if need_scroll {
            (TAB_ARROW_W_DP * s + TAB_GAP_DP * s) * 2.0
        } else {
            0.0
        };
        let view_w = (full - close - arrows).max(0.0);
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

    /// 把当前面滚进视野。**只在换面时调用**，见 [`Self::last_shown_page`]。
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

    /// 标签行：**一个裁剪视口 + 固定在右侧的控件**。
    ///
    /// 结构（从左到右）：`‹` │ 视口（clip，内容按像素左移） │ `›` │ `✕`
    ///
    /// 视口用 `fixed_w(0) + grow` 吃掉中间全部剩余宽度：measure 时它宽度为 0，所以
    /// **整行的宽度与标签内容完全无关**；arrange 时才把富余分给它。右侧三个控件因此
    /// 恒定贴在同一个位置——上一版它们随内容漂，就是因为行宽被内容撑着走。
    ///
    /// 内容用负 margin 左移实现滚动，超出视口的部分由 [`View::clipped`] 裁掉。
    fn build_tabs(&self, s: f32, u: f32, gap: f32, hover: i32) -> View {
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
                // ★ 宽度 0 + grow：整行宽度不被内容撑开，富余在 arrange 时才分进来。
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
                scroll < max_scroll - 0.5,
            ));
        }
        let close_fg = if hover == SOFT_TAG_CLOSE {
            c.ink
        } else {
            c.hint
        };
        row = row.child(
            View::container(Layout::Row)
                .fixed_w(TAB_CLOSE_W_DP * s)
                .radius(4.0 * s)
                .bg(if hover == SOFT_TAG_CLOSE {
                    c.keycap
                } else {
                    [0, 0, 0, 0]
                })
                .pad(Edges::xy(0.0, 4.0 * s))
                .tag(SOFT_TAG_CLOSE)
                .child(
                    View::leaf("✕", close_fg)
                        .font_size(13.0 * s)
                        .text_align(Align::Center)
                        .fill_cross()
                        .grow(),
                ),
        );
        row
    }

    /// 标签行的滚动箭头。`live=false` 时画成淡的且不进命中表——到头了还能点只会让人
    /// 反复试探。
    fn tab_arrow(&self, tag: i32, text: &str, w: f32, s: f32, hover: i32, live: bool) -> View {
        let c = &self.colors;
        let fg = if !live {
            c.line
        } else if hover == tag {
            c.ink
        } else {
            c.hint
        };
        let mut v = View::container(Layout::Row)
            .fixed_w(w)
            .radius(4.0 * s)
            .bg(if live && hover == tag {
                c.keycap
            } else {
                [0, 0, 0, 0]
            })
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

    /// 一个符号键帽：角标（原物理键）+ **两档符号同时显示**。
    ///
    /// 上排小字是另一档，下排大字是当前档，切层时两者互换视觉权重——这样用户不按 Shift
    /// 也知道上档是什么，而当前能打出的是哪一个仍然一眼可辨（靠字号与颜色，不是靠记）。
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

        let (bg, border, fg, hint) = if down {
            (c.accent, c.accent, c.on_accent, c.on_accent)
        } else if dead {
            (c.keycap_dead, c.line, c.hint, c.hint)
        } else if hover == tag {
            (c.accent_soft, c.accent, c.ink, c.hint)
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
    ) -> View {
        self.flat_key(tag, text, units, u, gap, s, hover, false)
    }

    /// Caps 键：显示并切换系统大写锁定。
    ///
    /// ⛔ 这不违反「不拦截 CapsLock」那条禁令。禁的是**拦截物理键**——toggle 键的
    /// keydown/keyup 处理有坑，「翻转再回敲复原」已被删除且不得重来。这里是用户点面板时
    /// 我们**主动敲一次** `vk:0x14`，与用户自己按下没有区别；物理 CapsLock 仍然完全
    /// 不接管，它的状态由下面的轮询如实读回来。
    fn caps_key(&self, units: f32, u: f32, gap: f32, s: f32, hover: i32, pressed: i32) -> View {
        let tag = SOFT_TAG_FN_BASE + SOFT_FN_CAPS_INDEX as i32;
        let c = &self.colors;
        let (bg, fg) = if self.caps_on {
            (c.accent, c.on_accent)
        } else if pressed == tag || hover == tag {
            (c.accent_soft, c.ink)
        } else {
            (c.keycap_fn, c.hint)
        };
        View::container(Layout::Row)
            .fixed_w(units * u + (units - 1.0) * gap)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(
                if self.caps_on { c.accent } else { c.line },
                (1.0 * s).max(1.0),
            )
            .cross(Align::Center)
            .tag(tag)
            .child(
                View::leaf("Caps", fg)
                    .font_size(11.5 * s)
                    .text_align(Align::Center)
                    .fill_cross()
                    .grow(),
            )
    }

    /// Shift 键：**可点**（锁定/解锁第二层），同时反映物理按住状态。
    ///
    /// 两种来源分开存（`shift_held` / `shift_locked`）：物理按住是临时的、松开还原，
    /// 点击是锁定的、再点才还原。合成一个布尔就无法回答「松开物理 Shift 后该回哪一层」。
    ///
    /// ⛔ 仍然**不拦截物理 Shift 的按键事件**——面板只是跟随显示，切层判据取自每次按键
    /// 携带的修饰位与一次轻量的键盘状态查询，不去接管这个键本身。
    fn shift_key(&self, units: f32, u: f32, gap: f32, s: f32, hover: i32) -> View {
        let c = &self.colors;
        let on = self.layer_shift();
        let (bg, fg) = if on {
            (c.accent, c.on_accent)
        } else if hover == SOFT_TAG_SHIFT {
            (c.accent_soft, c.ink)
        } else {
            (c.keycap_fn, c.hint)
        };
        View::container(Layout::Row)
            .fixed_w(units * u + (units - 1.0) * gap)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(if on { c.accent } else { c.line }, (1.0 * s).max(1.0))
            .cross(Align::Center)
            .tag(SOFT_TAG_SHIFT)
            .child(
                View::leaf(
                    if self.shift_locked {
                        "Shift ●"
                    } else {
                        "Shift"
                    },
                    fg,
                )
                .font_size(11.5 * s)
                .text_align(Align::Center)
                .fill_cross()
                .grow(),
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
        let (bg, fg) = if down {
            (c.accent, c.on_accent)
        } else if hover == tag {
            (c.accent_soft, c.ink)
        } else {
            (c.keycap_fn, c.hint)
        };
        View::container(Layout::Row)
            .fixed_w(units * u + (units - 1.0) * gap)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(c.line, (1.0 * s).max(1.0))
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

/// 读物理 Shift 是否按住、大写锁定是否开着。非 Windows 返回 `None`（面板本就只有
/// Windows 实现）。
fn read_shift_caps() -> Option<(bool, bool)> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL, VK_SHIFT};
        // 高位 = 按住；低位 = toggle 态（大写锁定用的是这一位）。
        let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
        let caps = (GetKeyState(VK_CAPITAL.0 as i32) as u16 & 0x0001) != 0;
        Some((shift, caps))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// 改变状态的键不重复——按住翻页键会让面飞速乱切。
/// 让第 `cur` 个标签完整露出所需的滚动量。
///
/// 纯函数——宽度已量好，这里只做算术。抽出来是为了能单测，`scroll_current_into_view`
/// 只负责把渲染器测出的宽度喂进来。
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

/// 这个控件在**抬起**时才触发，而不是按下就动手。
///
/// 只给「关掉整块面板」这一类不可撤销的动作用。普通键位必须按下即出字——打字要跟手，
/// 长按重复也建立在按下就开始之上。而关闭按钮按下即关，手感上像是「还没点就没了」，
/// 且中途反悔（按住挪开）也来不及。
fn fires_on_release(tag: i32) -> bool {
    tag == SOFT_TAG_CLOSE
}

fn repeats(tag: i32) -> bool {
    (0..SOFT_TAG_PAGE_BASE).contains(&tag) || (SOFT_TAG_FN_BASE..).contains(&tag)
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

/// 首次显示的位置：工作区底部居中，留一点边距。
fn default_origin(w: u32, h: u32, s: f32) -> (i32, i32) {
    let margin = (16.0 * s).round() as i32;
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
            let x = rc.left + ((rc.right - rc.left) - w as i32) / 2;
            let y = rc.bottom - h as i32 - margin;
            return (x.max(rc.left), y.max(rc.top));
        }
    }
    let _ = (w, h);
    (margin, margin)
}

// ───────────────────────── 鼠标 ─────────────────────────

#[cfg(windows)]
mod mouse_impl {
    use super::{SoftMouse, fires_on_release, repeat_params, repeats};
    use crate::sys::{
        GetCursorPos, GetWindowRect, HWND_TOPMOST, POINT, RECT, ReleaseCapture, SWP_NOACTIVATE,
        SWP_NOSIZE, SWP_NOZORDER, SetCapture, SetWindowPos, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, clamp_to_work_area,
    };
    use crate::window::WindowMouse;
    use wind_ui_types::{
        SOFT_TAG_CLOSE, SOFT_TAG_PAGE_BASE, SOFT_TAG_TAB_LEFT, SOFT_TAG_TAB_RIGHT,
        SOFT_TAG_TAB_VIEWPORT,
    };
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
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
                    let (x, y) = pos(lparam);
                    let h = self.hit_at(x, y);
                    if h != self.hover {
                        self.hover = h;
                        self.dirty = true;
                    }
                    // 按住后移出该键：停止长按重复。SetCapture 在后台窗口按线程失效
                    // （本仓已有记录），所以这里靠「移出即停」+ 抬起兜底，不用 capture。
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
                    if h < 0 {
                        // 空白处（标签行留白、键位之间的缝）→ 拖动整块面板。
                        //
                        // 这里用 SetCapture 是安全的：按下发生在本窗口内，捕获的是本线程的
                        // 鼠标。本仓记过「后台窗口 SetCapture 按线程失效」，那说的是拿它去
                        // **侦听窗口外的点击**（菜单关闭），与拖动不是一回事——工具栏的拖动
                        // 一直就是这么做的。
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
                // 滚轮在标签行上横向滚动。
                //
                // ⚠️ WM_MOUSEWHEEL 的 lparam 是**屏幕坐标**（其它鼠标消息是客户区坐标），
                // 不换算会永远命中不到。
                //
                // 面板是 WS_EX_NOACTIVATE、永远拿不到焦点，而滚轮消息按规矩发给焦点窗口——
                // 我们能收到它，靠的是 Win10 起默认开启的「悬停即滚动非活动窗口」。
                // 用户若关掉那个设置就滚不动，故箭头必须一直留着，滚轮只是快捷方式。
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
                        || (SOFT_TAG_PAGE_BASE..SOFT_TAG_CLOSE).contains(&hit);
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
                    }
                    if self.pressed >= 0 {
                        // 抬起才触发：必须仍停在按下的那个控件上——按下后挪开再松手
                        // 是「反悔」，不该执行。
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

#[cfg(not(windows))]
impl crate::window::WindowMouse for SoftMouse {
    fn on_message(
        &mut self,
        _hwnd: crate::window::HWND,
        _msg: u32,
        _wparam: crate::window::WPARAM,
        _lparam: crate::window::LPARAM,
    ) -> Option<crate::window::LRESULT> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_slots_match_the_ansi_main_block() {
        // 13+13+11+10 = 47。与 wind-softkeyboard 的 KEY_ROWS 对不上时，键帽会整体错位，
        // 而错位在肉眼看来只是「某些符号跑到了别的键上」。
        assert_eq!(ROW_SLOTS.iter().sum::<usize>(), 47);
    }

    #[test]
    fn page_cycling_wraps() {
        assert_eq!(next_page(0, 3), 1);
        assert_eq!(next_page(2, 3), 0);
        assert_eq!(prev_page(0, 3), 2);
        assert_eq!(prev_page(2, 3), 1);
        // 空表不该 panic（除零 / 下溢）
        assert_eq!(next_page(0, 0), 0);
        assert_eq!(prev_page(0, 0), 0);
    }

    #[test]
    fn only_output_keys_repeat() {
        assert!(repeats(0), "符号键位重复");
        assert!(repeats(SOFT_TAG_FN_BASE), "退格等功能键重复");
        assert!(!repeats(SOFT_TAG_PAGE_BASE), "翻页不重复——按住会飞速乱切");
        assert!(!repeats(SOFT_TAG_CLOSE), "关闭不重复");
    }

    #[test]
    fn slot_labels_use_the_printed_symbol() {
        assert_eq!(slot_label("grave"), "`");
        assert_eq!(slot_label("lbracket"), "[");
        assert_eq!(slot_label("q"), "Q");
        assert_eq!(slot_label("1"), "1");
    }

    /// 标签滚动的算术：贴左露出、贴右露出、已可见则不动。
    #[test]
    fn scroll_only_moves_when_the_tab_is_off_screen() {
        let w = [80.0f32; 8]; // 每项 80，间隙 0 ⇒ 内容 640
        let view = 200.0;
        // 已完整可见 ⇒ 一动不动。★ 这条是「用户滚不动标签行」那个 bug 的守门：
        //   若这里返回了别的值，每帧都会把滚动量拽回去。
        assert_eq!(scroll_to_show(&w, 0.0, 1, 0.0, view), 0.0);
        // 在右边界之外 ⇒ 贴右露出：第 3 项右缘 4*80=320，减去视口 200
        assert_eq!(scroll_to_show(&w, 0.0, 3, 0.0, view), 120.0);
        // 在左边界之外 ⇒ 贴左露出
        assert_eq!(scroll_to_show(&w, 0.0, 1, 300.0, view), 80.0);
        // 不会滚过头：末项也不超过 content-view
        assert_eq!(scroll_to_show(&w, 0.0, 7, 0.0, view), 640.0 - 200.0);
        // 内容装得下 ⇒ 恒为 0
        assert_eq!(scroll_to_show(&[50.0, 50.0], 0.0, 1, 0.0, view), 0.0);
        // 空表不 panic
        assert_eq!(scroll_to_show(&[], 0.0, 3, 0.0, view), 0.0);
    }
}
