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
    SOFT_FN_KEYS, SOFT_TAG_CLOSE, SOFT_TAG_FN_BASE, SOFT_TAG_PAGE_BASE, SoftKeyCap, UiEvent,
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

/// 长按重复：首次延迟与间隔（毫秒）。
///
/// ⚠️ 真实实现应读 `SPI_GETKEYBOARDDELAY` / `SPI_GETKEYBOARDSPEED`——自定常数一定会和
/// 物理键长按的手感对不上。这里取一组接近 Windows 默认的值，读系统设置见 [`repeat_params`]。
const REPEAT_DELAY_MS: u64 = 500;
const REPEAT_RATE_MS: u64 = 33;
/// 物理按键高亮的持续时间（毫秒）。长按时被连发的 keydown 不断续期。
const KEY_FLASH_MS: u64 = 140;

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
}

impl SoftMouse {
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
    shift: bool,
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
            shift: false,
            down_slot: None,
            visible: false,
            origin: None,
        })
    }

    pub fn set_theme(&mut self, theme: &wind_theme::Resolved) {
        let d = Colors::default();
        self.colors = Colors {
            panel: theme.color("softkb_bg", theme.color("candidate_bg", d.panel)),
            keycap: theme.color("softkb_key_bg", d.keycap),
            keycap_fn: theme.color("softkb_fnkey_bg", d.keycap_fn),
            keycap_dead: theme.color("softkb_dead_bg", d.keycap_dead),
            line: theme.color("softkb_border", d.line),
            ink: theme.color("softkb_text", theme.color("candidate_text", d.ink)),
            hint: theme.color("softkb_hint", d.hint),
            accent: theme.color(
                "softkb_active_bg",
                theme.color("candidate_selected_bg", d.accent),
            ),
            accent_soft: theme.color("softkb_hover_bg", d.accent_soft),
            on_accent: theme.color(
                "softkb_active_text",
                theme.color("candidate_selected_text", d.on_accent),
            ),
        };
        if self.visible {
            self.render();
        }
    }

    /// 显示面板 / 整块刷新（切面也走这里）。
    pub fn show(&mut self, pages: Vec<String>, current: usize, keys: Vec<SoftKeyCap>) {
        self.pages = pages;
        self.current = current;
        self.keys = keys;
        self.shift = false;
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
        if self.shift != shift {
            self.shift = shift;
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
            if cap.output(self.shift).is_some() {
                let _ = self.events.send(UiEvent::SoftKeyboardKey {
                    slot: cap.slot.clone(),
                    shift: self.shift,
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
                2 => row = row.child(self.page_name_key(1.75, u, gap, s, hover)),
                3 => row = row.child(self.shift_key(2.25, u, gap, s)),
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
                3 => row = row.child(self.shift_key(2.75, u, gap, s)),
                _ => {}
            }
            root = root.child(row);
        }

        // 底行：Ins / Del / 空格 / 翻页 / Esc
        let mut bottom = View::container(Layout::Row).gap(gap);
        bottom = bottom.child(self.fn_key(4, "Ins", 1.5, u, gap, s, hover, pressed));
        bottom = bottom.child(self.fn_key(5, "Del", 1.5, u, gap, s, hover, pressed));
        bottom = bottom.child(self.fn_key(3, "", 8.0, u, gap, s, hover, pressed));
        bottom = bottom.child(self.ctrl_key(
            SOFT_TAG_PAGE_BASE + prev_page(self.current, self.pages.len()),
            "◀",
            1.25,
            u,
            gap,
            s,
            hover,
        ));
        bottom = bottom.child(self.ctrl_key(
            SOFT_TAG_PAGE_BASE + next_page(self.current, self.pages.len()),
            "▶",
            1.25,
            u,
            gap,
            s,
            hover,
        ));
        bottom = bottom.child(self.ctrl_key(SOFT_TAG_CLOSE, "Esc", 1.5, u, gap, s, hover));
        root = root.child(bottom);
        root
    }

    /// 标签行：面名 + 右端关闭按钮。
    fn build_tabs(&self, s: f32, _u: f32, gap: f32, hover: i32) -> View {
        let c = &self.colors;
        let mut tabs = View::container(Layout::Row)
            .gap(2.0 * s)
            .cross(Align::Center);
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
            let label = View::leaf(name, fg)
                .font_size(12.5 * s)
                .text_align(Align::Center);
            tabs = tabs.child(
                View::container(Layout::Row)
                    .bg(bg)
                    .radius(4.0 * s)
                    .pad(Edges::xy(9.0 * s, 5.0 * s))
                    .cross(Align::Center)
                    .tag(tag)
                    .child(label),
            );
        }
        tabs = tabs.child(View::spacer().grow());
        let close_fg = if hover == SOFT_TAG_CLOSE {
            c.ink
        } else {
            c.hint
        };
        tabs = tabs.child(
            View::container(Layout::Row)
                .radius(4.0 * s)
                .bg(if hover == SOFT_TAG_CLOSE {
                    c.keycap
                } else {
                    [0, 0, 0, 0]
                })
                .pad(Edges::xy(7.0 * s, 4.0 * s))
                .tag(SOFT_TAG_CLOSE)
                .child(View::leaf("✕", close_fg).font_size(13.0 * s)),
        );
        let _ = gap;
        tabs
    }

    /// 一个符号键帽：角标（原物理键）+ 主体（当前层的符号）。
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
        let out = cap.output(self.shift);
        let dead = out.is_none();
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
        let sym = out.unwrap_or("·");
        // 多字符 token 收窄字号，避免撑出键帽。
        let sym_px = if sym.chars().count() > 1 {
            12.0 * s
        } else {
            17.0 * s
        };

        let mut cell = View::container(Layout::Column)
            .fixed_w(u)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(border, (1.0 * s).max(1.0))
            .pad(Edges::xy(3.0 * s, 2.0 * s))
            .child(
                View::leaf(label, hint)
                    .font_size(9.0 * s)
                    .text_align(Align::Start)
                    .fill_cross(),
            )
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

    /// 面名键：占着 CapsLock 那个宽键位。
    ///
    /// ⛔ **物理 CapsLock 不接管**——本仓禁止拦截 toggle 键（「翻转再回敲复原」已删除）。
    /// 这里只是把那个显眼的键位利用起来：显示当前面名，鼠标点击切下一面。
    fn page_name_key(&self, units: f32, u: f32, gap: f32, s: f32, hover: i32) -> View {
        let name = self
            .pages
            .get(self.current)
            .map(String::as_str)
            .unwrap_or("");
        let tag = SOFT_TAG_PAGE_BASE + next_page(self.current, self.pages.len());
        let c = &self.colors;
        let (bg, fg) = if hover == tag {
            (c.accent, c.on_accent)
        } else {
            (c.accent_soft, c.accent)
        };
        View::container(Layout::Row)
            .fixed_w(units * u + (units - 1.0) * gap)
            .fixed_h(u)
            .bg(bg)
            .radius(4.0 * s)
            .border(c.accent, (1.0 * s).max(1.0))
            .cross(Align::Center)
            .tag(tag)
            .child(
                View::leaf(name, fg)
                    .font_size(12.5 * s)
                    .font_weight(600)
                    .text_align(Align::Center)
                    .fill_cross()
                    .grow(),
            )
    }

    /// Shift 键：只作层指示，**不可点**。
    ///
    /// 按住物理 Shift 切层是临时的（松开还原），做成可点就变成了 toggle，
    /// 与「临时切层」的语义打架，也与本仓「修饰键只走 keyup 轻敲」的纪律相悖。
    fn shift_key(&self, units: f32, u: f32, gap: f32, s: f32) -> View {
        let c = &self.colors;
        let (bg, fg) = if self.shift {
            (c.accent, c.on_accent)
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
            .child(
                View::leaf("Shift", fg)
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

/// 改变状态的键不重复——按住翻页键会让面飞速乱切。
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
    use super::{SoftMouse, repeat_params, repeats};
    use crate::window::WindowMouse;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE};

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
                    if h >= 0 {
                        self.pressed = h;
                        self.clicked.push(h);
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
                WM_LBUTTONUP => {
                    if self.pressed >= 0 {
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
}
