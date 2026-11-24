//! 弹出菜单（右键候选菜单 + 功能主菜单）
//!
//! 标准多级级联菜单：父面板常驻，悬停带 ▶ 的项时子菜单作为独立窗口在右侧弹出，可层层展开。
//! 仿 Win32 原生菜单：只在根窗口 SetCapture 一次，捕获后所有鼠标消息以根窗口客户区坐标投递，
//! 再用屏幕坐标对各级面板命中测试。逻辑（结构变更）集中在 MenuState（wnd_proc 侧），
//! 窗口协调（渲染/定位/隐藏多余窗口）在 PopupMenu.tick() 侧。键盘经协调器 MenuKey 转发。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use crate::manager::{MenuAnchor, MenuItemSpec, MenuKind, MenuPlacement, UiEvent};
use crate::sys::{
    GetAsyncKeyState, GetCursorPos, HWND, IDC_ARROW, LPARAM, LRESULT, LoadCursorW, POINT,
    ReleaseCapture, SW_HIDE, SetCapture, SetCursor, ShowWindow, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
    WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_SETCURSOR, WPARAM,
};
use crate::text::dwrite::TextRenderer;
use crate::view::{Align, Edges, Layout, Rect, View};
use crate::window::{LayeredWindow, WindowMouse};
use wind_theme::schema::Dim;
#[cfg(windows)]
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
#[cfg(windows)]
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    SetClipboardData,
};
#[cfg(windows)]
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
#[cfg(windows)]
use windows::Win32::System::Ole::CF_UNICODETEXT;

const FONT_PX: f32 = 14.0;
const BG: [u8; 4] = [250, 250, 250, 252];
const FG: [u8; 4] = [40, 40, 40, 255];
const DISABLED: [u8; 4] = [175, 175, 178, 255];
const BORDER: [u8; 4] = [205, 205, 208, 230];
const SEP: [u8; 4] = [228, 228, 230, 255];
const HL_BG: [u8; 4] = [225, 236, 252, 255];
/// 高亮项文字色默认值（与基态 FG 同——主题可经 menu_hover_text 覆盖）。
const HL_FG: [u8; 4] = FG;

/// 无选中哨兵
const NONE_SEL: usize = usize::MAX;

/// 一层菜单的几何：`(左, 上, 宽, 高, 该层各项的命中矩形 [(项下标, 矩形)])`。
/// 内容几何与窗口几何（含阴影 margin）同型，故共用此别名。
type LayerGeom = (i32, i32, u32, u32, Vec<(usize, Rect)>);

/// 一个打开的菜单层级
struct Level {
    /// 本层菜单项（内容源）
    items: Vec<MenuItemSpec>,
    /// 高亮行（== item 下标；NONE_SEL = 无）
    selected: usize,
    /// 屏幕坐标左上角（由 PopupMenu 渲染后写回）
    origin: (i32, i32),
    /// 窗口尺寸
    size: (u32, u32),
    /// 命中矩形 (item 下标, 窗口局部矩形)
    item_rects: Vec<(usize, Rect)>,
}

impl Level {
    fn new(items: Vec<MenuItemSpec>, selected: usize) -> Self {
        Self {
            items,
            selected,
            origin: (0, 0),
            size: (0, 0),
            item_rects: Vec::new(),
        }
    }
}

/// 已提交到某个窗口的一层渲染态（reconcile 的增量基线）。
///
/// 记录「上一帧这个窗口里画的是什么、摆在哪」，下一帧据此跳过无变化的层。
/// `None` 语义是「该窗口当前不可见」（初始态 / 被 hide 过），必须重新 show。
#[derive(Clone, PartialEq)]
struct LevelRender {
    items: Vec<MenuItemSpec>,
    selected: usize,
    /// 窗口几何 (x, y, w, h)，与 `LayeredWindow::show` 的入参同口径。
    geom: (i32, i32, u32, u32),
}

/// 增量判定：`prev`（上一帧基线，None=窗口不可见）对 `want`（本帧目标）需要做什么。
///
/// 返回 `(需要重绘, 需要重新摆位)`，两者独立：
/// - 重绘（`update`）只推像素，不碰 z 序——高亮变化走这条路；
/// - 摆位（`show`）会 `SetWindowPos(HWND_TOPMOST)`，副作用是把窗口提到 topmost 组最前，
///   所以只在首显或几何真变了时才做。每帧都 `show()` 会让各层 z 序反复交替，
///   子菜单翻到左侧压在父面板上时就会闪一帧遮挡。
fn plan_render(prev: Option<&LevelRender>, want: &LevelRender) -> (bool, bool) {
    match prev {
        // 窗口当前不可见：必须画满并摆位。
        None => (true, true),
        Some(p) => {
            // 尺寸变化同时意味着 buffer 要重画（旧像素尺寸对不上）。
            let repaint = p.items != want.items
                || p.selected != want.selected
                || (p.geom.2, p.geom.3) != (want.geom.2, want.geom.3);
            (repaint, p.geom != want.geom)
        }
    }
}

fn is_separator(it: &MenuItemSpec) -> bool {
    matches!(it.kind, MenuKind::Separator)
}
fn is_submenu(it: &MenuItemSpec) -> bool {
    matches!(it.kind, MenuKind::Submenu)
}
fn selectable(it: &MenuItemSpec) -> bool {
    !is_separator(it) && it.enabled
}

fn first_selectable(items: &[MenuItemSpec]) -> usize {
    items.iter().position(selectable).unwrap_or(NONE_SEL)
}

/// 鼠标键的「按下沿」：仅上一轮未按、本轮按着时为真。
/// 单独成函数是为了让 [`PopupMenu::poll_outside_press`] 里最易写反的那半条语义可测。
fn press_edge(was_down: bool, now_down: bool) -> bool {
    now_down && !was_down
}

/// 左/中/右任一鼠标键当前是否按着。
///
/// 只取 `0x8000`（当前按下）位。最低位 `0x0001`（自上次调用以来按过）是进程级共享状态，
/// 任何调用都会清零它，多处消费会互相偷事件；边沿一律由调用方保存上一轮状态来判定。
fn any_mouse_button_down() -> bool {
    [VK_LBUTTON, VK_MBUTTON, VK_RBUTTON]
        .iter()
        .any(|vk| key_down(vk.0 as i32))
}

/// 某个键当前是否按着（只取 `0x8000` 位，理由见 [`any_mouse_button_down`]）。
fn key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

/// 这次按下是不是逻辑上的右键（只按了右键；左右同按算左键，只关菜单）。
/// `GetAsyncKeyState` 问的是物理键位，左右手互换（`SM_SWAPBUTTON`）后物理左键才是逻辑右键。
fn is_right_press(phys_left: bool, phys_right: bool, swapped: bool) -> bool {
    let (left, right) = if swapped {
        (phys_right, phys_left)
    } else {
        (phys_left, phys_right)
    };
    right && !left
}

/// 系统是否左右手互换了鼠标键。
fn mouse_buttons_swapped() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
        GetSystemMetrics(SM_SWAPBUTTON) != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 一次关掉了菜单的「菜单外按下」：屏幕坐标 + 是否逻辑右键。
///
/// 菜单开着时同线程其它窗口收不到鼠标消息（见 [`PopupMenu::poll_outside_press`]），只有轮询看得见。
/// UI 循环把它转给气泡：在气泡上右键要当作「重新右键」，否则第二次右键只会关掉菜单。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutsidePress {
    pub x: i32,
    pub y: i32,
    pub right: bool,
}

/// 菜单请求的回应时限。UI → 协调器 → UI 来回正常是毫秒级，协调器可能短暂卡在状态锁上，
/// 1.5 秒足够宽；再久多半是请求被丢了（候选已清空时不弹菜单），等待方不该一直挂着。
/// 候选窗的右键压制（`candidate_window::TipHold`）与气泡的「请求在途」共用。
pub(crate) const MENU_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

thread_local! {
    /// 弹出菜单此刻是否可见（UI 线程内共享）。
    static MENU_VISIBLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 弹出菜单此刻是否可见。候选窗 / 气泡在 wnd_proc 里就要问，故做成线程局部（同在 UI 线程）
/// 而非由 UI 循环每轮传值：传值要到下一轮 tick 才更新，菜单刚关时的鼠标消息会读到过时值。
pub(crate) fn menu_visible() -> bool {
    MENU_VISIBLE.with(|v| v.get())
}

/// 写 [`menu_visible`]。仅 `PopupMenu::show` / `hide` 与测试调用。
pub(crate) fn set_menu_visible(visible: bool) {
    MENU_VISIBLE.with(|v| v.set(visible));
}

/// 菜单交互状态（与 wnd_proc 共享）。只做结构变更，dirty 触发 PopupMenu 协调重绘。
struct MenuState {
    /// 打开的层级链（last = 最深/当前活动层）
    levels: Vec<Level>,
    /// 根窗口（捕获窗口）屏幕原点，用于把客户坐标换算回屏幕坐标
    capture_origin: (i32, i32),
    /// 需要重新协调/重绘
    dirty: bool,
    /// 请求关闭
    closed: bool,
    events: Sender<UiEvent>,
}

impl MenuState {
    fn screen(&self, x: i32, y: i32) -> (i32, i32) {
        (self.capture_origin.0 + x, self.capture_origin.1 + y)
    }

    /// 屏幕坐标命中：从最深层往外找。返回 (层下标, Some(行) | None=面板空白)。
    fn find_hit(&self, sx: i32, sy: i32) -> Option<(usize, Option<usize>)> {
        for k in (0..self.levels.len()).rev() {
            let lv = &self.levels[k];
            let (ox, oy) = lv.origin;
            let (w, h) = lv.size;
            if sx >= ox && sx < ox + w as i32 && sy >= oy && sy < oy + h as i32 {
                let lx = (sx - ox) as f32;
                let ly = (sy - oy) as f32;
                for (row, r) in &lv.item_rects {
                    if r.contains(lx, ly) {
                        return Some((k, Some(*row)));
                    }
                }
                return Some((k, None));
            }
        }
        None
    }

    /// 压入 levels[k].selected 指向的子菜单。focus=true 时聚焦首个可选项（键盘）。
    fn open_child(&mut self, k: usize, focus: bool) {
        let sel = self.levels[k].selected;
        if sel == NONE_SEL {
            return;
        }
        let children = match self.levels[k].items.get(sel) {
            Some(it) if is_submenu(it) => it.children.clone(),
            _ => return,
        };
        let child_sel = if focus {
            first_selectable(&children)
        } else {
            NONE_SEL
        };
        self.levels.push(Level::new(children, child_sel));
        self.dirty = true;
    }

    /// 取消第 k 层高亮，仅当 k 为最深层时生效：更浅的层有子菜单开着，父项须保持高亮指示展开路径。
    /// 只改 `selected`，不动 `levels`——灭高亮从不收起子菜单。`levels` 为空时短路保护了索引。
    fn clear_hover(&mut self, k: usize) {
        if k + 1 == self.levels.len() && self.levels[k].selected != NONE_SEL {
            self.levels[k].selected = NONE_SEL;
            self.dirty = true;
        }
    }

    /// 鼠标悬停到 (层 k, 行 r)：更新高亮、收起更深层、必要时展开子菜单。
    fn hover(&mut self, k: usize, r: usize) {
        let (ok, sub) = match self.levels[k].items.get(r) {
            Some(it) => (selectable(it), is_submenu(it)),
            None => {
                self.clear_hover(k);
                return;
            }
        };
        // 禁用项/分隔符：不高亮，并清掉残留高亮（鼠标不在可选条目即不应高亮）。
        if !ok {
            self.clear_hover(k);
            return;
        }
        if self.levels[k].selected != r {
            self.levels[k].selected = r;
            self.levels.truncate(k + 1);
            self.dirty = true;
        }
        // 子菜单尚未展开则展开（子层 selected=NONE，避免递归自动深入）
        if sub && self.levels.len() == k + 1 {
            self.open_child(k, false);
        }
    }

    /// 鼠标点击 (层 k, 行 r)。
    fn click(&mut self, k: usize, r: usize) {
        let (ok, sub, kind) = match self.levels[k].items.get(r) {
            Some(it) => (selectable(it), is_submenu(it), it.kind),
            None => return,
        };
        if !ok {
            return;
        }
        if self.levels[k].selected != r {
            self.levels[k].selected = r;
            self.levels.truncate(k + 1);
        }
        if sub {
            if self.levels.len() == k + 1 {
                self.open_child(k, false);
            }
        } else {
            let _ = self.events.send(UiEvent::MenuAction(kind));
            self.closed = true;
        }
    }

    /// 在某层移动高亮（跳过分隔/禁用），dir=±1。
    fn move_sel(&mut self, k: usize, dir: i32) {
        let n = self.levels[k].items.len();
        if n == 0 {
            return;
        }
        let cur = self.levels[k].selected;
        let mut i = if cur == NONE_SEL {
            if dir > 0 { -1 } else { 0 }
        } else {
            cur as i32
        };
        for _ in 0..n {
            i = (i + dir).rem_euclid(n as i32);
            if selectable(&self.levels[k].items[i as usize]) {
                self.levels[k].selected = i as usize;
                self.dirty = true;
                return;
            }
        }
    }

    fn deepest(&self) -> usize {
        self.levels.len().saturating_sub(1)
    }

    // —— 键盘动作（都作用于最深活动层）——
    fn key_up(&mut self) {
        let k = self.deepest();
        self.move_sel(k, -1);
    }
    fn key_down(&mut self) {
        let k = self.deepest();
        self.move_sel(k, 1);
    }
    fn key_right(&mut self) {
        let k = self.deepest();
        let sel = self.levels[k].selected;
        if sel != NONE_SEL
            && self.levels[k].items.get(sel).is_some_and(is_submenu)
            && self.levels.len() == k + 1
        {
            self.open_child(k, true);
        }
    }
    fn key_left(&mut self) {
        if self.levels.len() > 1 {
            self.levels.pop();
            self.dirty = true;
        } else {
            self.close();
        }
    }
    fn key_enter(&mut self) {
        let k = self.deepest();
        let sel = self.levels[k].selected;
        if sel == NONE_SEL {
            return;
        }
        let Some(it) = self.levels[k].items.get(sel) else {
            return;
        };
        if is_submenu(it) {
            if self.levels.len() == k + 1 {
                self.open_child(k, true);
            }
        } else if selectable(it) {
            let kind = it.kind;
            let _ = self.events.send(UiEvent::MenuAction(kind));
            self.closed = true;
        }
    }

    fn close(&mut self) {
        self.closed = true;
        let _ = self.events.send(UiEvent::MenuClose);
    }

    // —— 指针（屏幕坐标）：Windows 的 wnd_proc 与 Linux 宿主转来的事件共用 ——

    /// 高亮跟手：指向条目就亮，没指向就灭。只动 `selected`，不会收起已展开的子菜单。
    /// 灭高亮只触发 `update()`、不触发 `show()`（见 `plan_render`），不会重排 z 序。
    fn pointer_move(&mut self, sx: i32, sy: i32) {
        match self.find_hit(sx, sy) {
            Some((k, Some(r))) => self.hover(k, r),
            // 面板内非条目处（root padding / 边框 / 分隔线）→ 灭掉该层高亮。
            Some((k, None)) => self.clear_hover(k),
            // 菜单外 → 同样灭掉最深层高亮。鼠标都移开了还亮着，观感像卡住。
            None => self.clear_hover(self.deepest()),
        }
    }

    fn pointer_left_press(&mut self, sx: i32, sy: i32) {
        match self.find_hit(sx, sy) {
            Some((k, Some(r))) => self.click(k, r),
            Some((_, None)) => {} // 面板空白处：忽略
            None => self.close(), // 菜单外 → 关闭
        }
    }
}

/// 弹出菜单窗口（级联，窗口池按需增长）
pub struct PopupMenu {
    windows: Vec<LayeredWindow>,
    renderer: TextRenderer,
    scale: f32,
    state: Rc<RefCell<MenuState>>,
    visible: bool,
    /// 与 `windows` 同下标的渲染基线：`rendered[k]` = 窗口 k 上一帧画了什么、摆在哪，
    /// `None` = 该窗口当前不可见。reconcile 据此跳过无变化的层，见 [`plan_render`]。
    /// 任何会改变像素但不改变 items/selected 的因素（主题、DPI）必须清空它。
    rendered: Vec<Option<LevelRender>>,
    /// 根面板锚点（已钳制到工作区）
    anchor: (i32, i32),
    bg: [u8; 4],
    fg: [u8; 4],
    disabled: [u8; 4],
    border: [u8; 4],
    sep: [u8; 4],
    hl_bg: [u8; 4],
    /// 高亮项文字色（menu_hover_text）：选中/悬停项文字与基态不同。
    hl_fg: [u8; 4],
    /// 生效字号（逻辑 px）= FONT_PX + menu.item.font_size 偏移。
    /// 存字段而非每次现算：ensure_scale 与 build_view 都要用，且需与 renderer 基准保持一致。
    font_px: f32,
    /// 菜单容器位图背景 + z 层（jidian menu.root 吃九宫格 panel + 角标水印）。
    bg_image: Option<crate::view::ViewImage>,
    bg_gradient: Option<crate::view::ViewGradient>,
    layers: Vec<crate::view::ViewLayer>,
    /// 边框宽 / 圆角（设备像素，从 menu.root 节点 border 读取，px/dp 经 Dim 区分）。
    border_w: f32,
    radius: f32,
    /// 主题配置的软投影（menu.root.shadow）。
    shadow: Option<crate::view::SoftShadow>,
    /// 已应用主题（DPI 变化时按新缩放重解析几何）。
    theme: Option<wind_theme::Resolved>,
    /// 上一轮轮询看到的「有鼠标键按着」状态，供 [`Self::poll_outside_press`] 做边沿检测。
    /// 必须在 `show()` 里按当前真实状态初始化，理由见该处注释。
    mouse_was_down: bool,
    /// 最近一次关掉菜单的菜单外按下，待 UI 循环经 [`Self::take_outside_press`] 取走。
    outside_press: Option<OutsidePress>,
}

impl PopupMenu {
    pub fn new(events: Sender<UiEvent>) -> Result<Self, String> {
        let scale = dpi_scale();
        let renderer = TextRenderer::new("Microsoft YaHei UI", FONT_PX * scale)?;
        let state = Rc::new(RefCell::new(MenuState {
            levels: Vec::new(),
            capture_origin: (0, 0),
            dirty: false,
            closed: false,
            events,
        }));
        let mut menu = Self {
            windows: Vec::new(),
            renderer,
            scale,
            state,
            visible: false,
            rendered: Vec::new(),
            anchor: (0, 0),
            bg: BG,
            fg: FG,
            disabled: DISABLED,
            border: BORDER,
            sep: SEP,
            hl_bg: HL_BG,
            hl_fg: HL_FG,
            font_px: FONT_PX,
            bg_image: None,
            bg_gradient: None,
            layers: Vec::new(),
            border_w: scale, // 默认 1dp（≈1 设备像素，细边清晰）
            radius: 6.0 * scale,
            shadow: None,
            theme: None,
            mouse_was_down: false,
            outside_press: None,
        };
        // 预创建根窗口并绑定鼠标处理器（捕获后只有根窗口收消息）
        menu.ensure_windows(1)?;
        Ok(menu)
    }

    /// 确保窗口池至少有 n 个窗口（新窗口绑定共享 MenuState 处理器）。
    fn ensure_windows(&mut self, n: usize) -> Result<(), String> {
        while self.windows.len() < n {
            let w = LayeredWindow::create(None, 160, 120, "WindInputPopupMenu")?;
            w.register_mouse(self.state.clone());
            self.windows.push(w);
        }
        // 基线与窗口池同长（新窗口默认不可见 → None → 首次必然重绘并摆位）。
        if self.rendered.len() < self.windows.len() {
            self.rendered.resize(self.windows.len(), None);
        }
        Ok(())
    }

    /// 作废全部渲染基线：下一次 reconcile 会全量重绘并重新摆位。
    /// 用于「像素会变但 items/selected 不变」的场合（主题切换、DPI 变化、菜单重新弹出）。
    fn invalidate_rendered(&mut self) {
        self.rendered.fill(None);
    }

    /// DPI 动态化：按显示点所在显示器实时取缩放，变化则更新字号并按新缩放重解析主题几何。
    fn ensure_scale(&mut self, x: i32, y: i32) {
        #[cfg(ext_presenter)]
        let sc = HOST_MENU_SCALE
            .with(|c| c.get())
            .unwrap_or_else(|| crate::dpi::scale_for_point(x, y));
        #[cfg(not(ext_presenter))]
        let sc = crate::dpi::scale_for_point(x, y);
        if (sc - self.scale).abs() > 0.01 {
            self.scale = sc;
            // 用主题生效字号而非常量：主题配了 menu.item.font_size 时，
            // 跨屏切换不能把字号打回 FONT_PX。set_theme 末尾还会再同步一次。
            self.renderer.set_base_size(self.font_px * sc);
            if let Some(t) = self.theme.clone() {
                self.set_theme(&t);
            }
        }
    }

    /// 应用主题（菜单各色）。
    pub fn set_theme(&mut self, theme: &wind_theme::Resolved) {
        self.theme = Some(theme.clone());
        // 先取 palette 兜底，随后被视图节点覆盖。节点色在 resolve 阶段已合成了 palette 默认，
        // 故节点存在就以它为准（与候选窗/状态窗同一优先级），缺席才落回 token。
        self.bg = theme.color("menu_bg", BG);
        self.fg = theme.color("menu_text", FG);
        self.disabled = theme.color("menu_disabled", DISABLED);
        self.border = theme.color("menu_border", BORDER);
        self.sep = theme.color("menu_separator", SEP);
        self.hl_bg = theme.color("menu_hover_bg", HL_BG);
        self.hl_fg = theme.color("menu_hover_text", self.fg);
        // 菜单项：字号偏移 + 文字色 + hover/disabled 状态色。
        self.font_px = FONT_PX;
        if let Some(item) = &theme.views.menu_item {
            // 字号偏移语义与候选窗一致：相对模块基准的有符号增量，钳到 >=6 防止不可读。
            if item.font_size != 0.0 {
                self.font_px = (FONT_PX + item.font_size).max(6.0);
            }
            if let Some(c) = item.text_color {
                self.fg = c;
            }
            if let Some(h) = &item.hover {
                if let Some(c) = h.bg_color {
                    self.hl_bg = c;
                }
                // hover 文字未配时沿用正文色（与旧的 color("menu_hover_text", fg) 兜底一致）。
                self.hl_fg = h.text_color.unwrap_or(self.fg);
            }
            if let Some(d) = &item.disabled
                && let Some(c) = d.text_color
            {
                self.disabled = c;
            }
        }
        // 分隔线：线色取 background.color。
        if let Some(sep) = &theme.views.menu_separator
            && let Some(c) = sep.bg_color
        {
            self.sep = c;
        }
        // 字号可能被主题改了，同步渲染器基准（否则测量仍按旧字号，宽高算错）。
        self.renderer.set_base_size(self.font_px * self.scale);
        let s = self.scale;
        if let Some(node) = &theme.views.menu_root {
            self.bg_image = crate::theme_assets::rv_image(theme, node.bg_image.as_ref(), s);
            self.bg_gradient = crate::theme_assets::rv_gradient(node.bg_gradient.as_ref());
            self.layers = crate::theme_assets::rv_layers(theme, &node.layers, s);
            // 背景色/边框色/宽/圆角从 menu.root 节点读取（权威，px/dp 经 Dim 区分）；
            // bg_color / border_color 默认已带 menu_bg / menu_border token 兜底（resolve build 传入）。
            if let Some(c) = node.bg_color {
                self.bg = c;
            }
            if let Some(c) = node.border_color {
                self.border = c;
            }
            self.border_w = node
                .border_width
                .map(|d| d.resolve(s, 0.0))
                .unwrap_or(s)
                .max(1.0);
            self.radius = node
                .border_radius
                .map(|d| d.resolve(s, 0.0))
                .unwrap_or(6.0 * s);
            self.shadow = crate::view::SoftShadow::build(
                node.shadow_offset_x,
                node.shadow_offset_y,
                node.shadow_blur,
                node.shadow_spread,
                node.shadow_spread_offset_x,
                node.shadow_spread_offset_y,
                node.shadow_color,
                s,
            );
        } else {
            self.bg_image = None;
            self.bg_gradient = None;
            self.layers = Vec::new();
            self.border = theme.color("menu_border", BORDER);
            self.border_w = s;
            self.radius = 6.0 * s;
            self.shadow = None;
        }
        // 颜色/字号/几何变了但 items/selected 没动，增量判据看不见，必须显式作废基线，
        // 否则换主题或跨屏改 DPI 后已显示的层停在旧像素上。
        self.invalidate_rendered();
    }

    /// 显示菜单（顶层 items）于 `anchor` 描述的位置。展开方向见 [`MenuPlacement`]。
    pub fn show(&mut self, items: Vec<MenuItemSpec>, anchor: MenuAnchor) {
        if items.is_empty() {
            return;
        }
        if self.ensure_windows(1).is_err() {
            return;
        }
        let (ax, ay) = if anchor.x == i32::MIN || anchor.y == i32::MIN {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            (p.x, p.y)
        } else {
            (anchor.x, anchor.y)
        };
        // DPI 动态化：先按显示点所在显示器取缩放，再测量/构建（几何依赖 scale）。
        self.ensure_scale(ax, ay);
        // 先测量根面板尺寸以钳制锚点（选中态不影响尺寸，传无选中即可）。
        let (_root, w, h, _hits) = self.build_view(&items, NONE_SEL);
        let (ax, ay) = place_menu(&anchor, ax, ay, w, h, work_area_of(ax, ay), self.scale);
        self.anchor = clamp_to_work_area(ax, ay, w, h);
        {
            let mut st = self.state.borrow_mut();
            // 打开时不预选首项（仿 Win32 原生菜单）：避免首项被高亮“闪一下”；
            // 悬停或方向键导航再点亮（move_sel 从 NONE_SEL 起步落到首个可选项）。
            st.levels = vec![Level::new(items, NONE_SEL)];
            st.dirty = false;
            st.closed = false;
        }
        self.reconcile();
        self.visible = true;
        set_menu_visible(true);
        unsafe {
            // 捕获前先把光标设成箭头：SetCapture 期间不再发 WM_SETCURSOR，光标会冻结在捕获瞬间
            // 的形状（从语言栏图标唤出时可能是 explorer 的忙碌态）。
            // 见 docs/architecture/ui-platform-notes.md 的「弹出菜单」。
            if let Ok(c) = LoadCursorW(None, IDC_ARROW) {
                SetCursor(c);
            }
            SetCapture(self.windows[0].hwnd());
        }
        // 边沿检测初态必须取当前真实按键状态、不能填 false：菜单几乎总是由尚未抬起的点击唤出，
        // 填 false 会把这次按下当成菜单外点击，菜单弹出即消失。
        self.mouse_was_down = any_mouse_button_down();
        self.outside_press = None;
    }

    /// 下一次需要 [`Self::tick`] 的时刻；菜单不可见时 `None`。
    ///
    /// 菜单是 UI 里唯一需要定期轮询的部分：[`Self::poll_outside_press`] 读 `GetAsyncKeyState`
    /// 瞬时状态，没有消息可等。周期 8ms；菜单打开是短暂交互，不影响静态时零唤醒。
    pub fn next_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        if !self.visible {
            return None;
        }
        Some(now + std::time::Duration::from_millis(8))
    }

    pub fn tick(&mut self) {
        if !self.visible {
            return;
        }
        // 先轮询再读 closed：本轮探到的菜单外点击立刻在同一轮生效，不必多等 8ms。
        self.poll_outside_press();
        let (dirty, closed) = {
            let st = self.state.borrow();
            (st.dirty, st.closed)
        };
        if closed {
            self.hide();
            return;
        }
        if dirty {
            self.state.borrow_mut().dirty = false;
            self.reconcile();
        }
    }

    /// 轮询「点在菜单外」并关闭菜单。
    ///
    /// 不能靠 `SetCapture` + `WM_LBUTTONDOWN`：服务进程不是前台进程，捕获退化成「光标在窗口
    /// 可见区内才收得到消息」，点菜单外恰好一条都收不到（见 docs/architecture/ui-platform-notes.md
    /// 的「弹出菜单」）。`GetAsyncKeyState` 直接问物理按键状态，绕开这条规则。
    ///
    /// 这次点击吞不掉（早已投递给目标窗口），只能跟着关菜单——点任务栏则任务栏响应、菜单收起。
    /// 点自家工具栏同样算「外面」，是有意的：关菜单会 `ReleaseCapture`，抬起才能投递到工具栏。
    /// 代价是工具栏设置键（抬起时才请求主菜单）表现为「关闭后立刻重新弹出」而非 toggle，接受。
    fn poll_outside_press(&mut self) {
        let now_down = any_mouse_button_down();
        let was_down = std::mem::replace(&mut self.mouse_was_down, now_down);
        // 仅按下沿触发：否则按住拖拽也会被当成点击；抬起沿更不该关。
        if !press_edge(was_down, now_down) {
            return;
        }
        let mut p = POINT::default();
        unsafe {
            if GetCursorPos(&mut p).is_err() {
                return;
            }
        }
        // 与 `on_message` 共用 `find_hit`：捕获收得到消息的情形必然命中面板，两路互补不重复关闭。
        if self.state.borrow().find_hit(p.x, p.y).is_some() {
            return;
        }
        tracing::debug!("PopupMenu: 检测到菜单外按下 → 关闭");
        self.outside_press = Some(OutsidePress {
            x: p.x,
            y: p.y,
            right: is_right_press(
                key_down(VK_LBUTTON.0 as i32),
                key_down(VK_RBUTTON.0 as i32),
                mouse_buttons_swapped(),
            ),
        });
        // 走 MenuState::close() 而非直接 hide()：它发 UiEvent::MenuClose 让协调器复位 menu_open，
        // 否则菜单没了但键仍被 forward_menu_key 吞掉。
        self.state.borrow_mut().close();
    }

    /// 取走最近一次关掉菜单的菜单外按下（见 [`OutsidePress`]）。
    pub fn take_outside_press(&mut self) -> Option<OutsidePress> {
        self.outside_press.take()
    }

    /// 键盘转发（协调器在组合期拦截方向键/回车/ESC 后下发）。
    pub fn on_key(&mut self, key: u32) {
        if !self.visible {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            match key {
                0x26 => st.key_up(),           // Up
                0x28 => st.key_down(),         // Down
                0x27 => st.key_right(),        // Right → 展开子菜单
                0x25 => st.key_left(),         // Left → 收起/返回
                0x1B => st.close(),            // ESC → 关闭
                0x0D | 0x20 => st.key_enter(), // Enter / Space → 激活
                _ => {}
            }
        }
        self.tick();
    }

    /// 协调窗口：按当前 levels 渲染/定位每一级，隐藏多余窗口，写回几何。
    fn reconcile(&mut self) {
        // 快照内容，避免渲染期间持有 state 借用
        let snap: Vec<(Vec<MenuItemSpec>, usize)> = {
            let st = self.state.borrow();
            st.levels
                .iter()
                .map(|l| (l.items.clone(), l.selected))
                .collect()
        };
        if snap.is_empty() {
            return;
        }
        if self.ensure_windows(snap.len()).is_err() {
            return;
        }

        // 软投影四向扩边（与候选窗一致）：内容布局起点移到 (ml,mt)，窗口左上回移，阴影溢出。
        // geom = 内容几何（屏幕坐标，供 place_child 链式定位）；wgeom = 窗口几何（含 margin，
        // 供命中/写回）。命中 item_rects 为窗口相对（含 ml,mt），与 find_hit 的 screen−origin 一致。
        let (ml, mt, mr, mb) = self
            .shadow
            .as_ref()
            .map(|s| s.margins())
            .unwrap_or((0, 0, 0, 0));
        let mut geom: Vec<LayerGeom> = Vec::with_capacity(snap.len());
        let mut wgeom: Vec<LayerGeom> = Vec::with_capacity(snap.len());
        // 展开方向串联态：一旦某层因右侧无空间翻到左侧，后续更深层级延续左侧展开，
        // 避免各层各自判断出现锯齿重叠（见 place_child 文档注释）。
        let mut prefer_left = false;
        for k in 0..snap.len() {
            let (items, selected) = &snap[k];
            let (mut root, cw, ch, content_hits) = self.build_view(items, *selected);
            // 内容左上（屏幕）：顶层用 anchor；子级贴父内容右缘 + 高亮项纵向对齐。
            let (cox, coy) = if k == 0 {
                self.anchor
            } else {
                let (pox, poy, pw, ph, prects) = &geom[k - 1];
                let psel = snap[k - 1].1;
                let prect = prects.iter().find(|(i, _)| *i == psel).map(|(_, r)| *r);
                let (x, y, went_left) = place_child(
                    (*pox, *poy),
                    (*pw, *ph),
                    prect,
                    (cw, ch),
                    self.scale,
                    prefer_left,
                );
                prefer_left = went_left;
                (x, y)
            };
            let (win_w, win_h) = (cw + ml + mr, ch + mt + mb);
            let (wox, woy) = (cox - ml as i32, coy - mt as i32);
            // 有阴影时把内容重排到 (ml,mt) 并重采窗口相对命中；无阴影时直接用内容命中。
            let whits: Vec<(usize, Rect)> = if ml > 0 || mt > 0 {
                root.layout(ml as f32, mt as f32, &self.renderer);
                let mut raw = Vec::new();
                root.collect_hits(&mut raw);
                raw.iter().map(|(t, r)| (*t as usize, *r)).collect()
            } else {
                content_hits.clone()
            };
            // 增量提交：像素变了只 update（不碰 z 序），首显/几何真变了才 show，见 `plan_render`。
            let want = LevelRender {
                items: items.clone(),
                selected: *selected,
                geom: (wox, woy, win_w, win_h),
            };
            let (repaint, replace) = plan_render(self.rendered[k].as_ref(), &want);
            // 绘制到窗口 k（拆分字段借用：renderer 与 windows 是不同字段，可并存）
            if repaint {
                let r = &self.renderer;
                let win = &mut self.windows[k];
                win.resize(win_w, win_h);
                let buf = win.buffer_mut();
                let n = (win_w * win_h * 4) as usize;
                buf[..n].fill(0);
                if let Some(s) = &self.shadow {
                    s.paint(
                        buf,
                        win_w,
                        win_h,
                        ml as f32,
                        mt as f32,
                        cw as f32,
                        ch as f32,
                        root.corner_radius,
                    );
                }
                root.paint(buf, win_w, win_h, r);
                let _ = win.update();
            }
            if replace {
                self.windows[k].show(wox, woy);
            }
            self.rendered[k] = Some(want);
            geom.push((cox, coy, cw, ch, content_hits));
            wgeom.push((wox, woy, win_w, win_h, whits));
        }

        // 隐藏多余窗口（基线随之作废：再次用到它时必须重绘 + 重新摆位）
        for k in snap.len()..self.windows.len() {
            self.windows[k].hide();
            self.rendered[k] = None;
        }

        // 写回几何（窗口坐标：origin/size 含 margin，item_rects 窗口相对）。
        {
            let mut st = self.state.borrow_mut();
            for (k, (ox, oy, w, h, rects)) in wgeom.into_iter().enumerate() {
                if let Some(l) = st.levels.get_mut(k) {
                    l.origin = (ox, oy);
                    l.size = (w, h);
                    l.item_rects = rects;
                }
            }
            st.capture_origin = st.levels.first().map(|l| l.origin).unwrap_or((0, 0));
        }
    }

    /// 构建一层菜单的视图，返回 (根视图, 宽, 高, 命中矩形)。
    fn build_view(
        &self,
        items: &[MenuItemSpec],
        selected: usize,
    ) -> (View, u32, u32, Vec<(usize, Rect)>) {
        let s = self.scale;
        // 几何读主题（menu.item / menu.root / menu.min_width），None→内置默认（行为不变）。
        let views = self.theme.as_ref().map(|t| &t.views);
        let mi = views.and_then(|v| v.menu_item.as_ref());
        let mroot = views.and_then(|v| v.menu_root.as_ref());
        let resolve_dim =
            |o: Option<Dim>, def: f32| o.map(|x| x.resolve(s, 0.0)).unwrap_or(def * s);
        let item_h = (self.font_px * 1.9 * s).ceil();
        // item 四边内边距独立可配；right/bottom 未写时回退到 left/top（保持既有主题的对称外观）。
        let pad_l = resolve_dim(mi.and_then(|n| n.padding.left), 12.0);
        let pad_t = resolve_dim(mi.and_then(|n| n.padding.top), 4.0);
        let pad = Edges {
            l: pad_l,
            t: pad_t,
            r: mi
                .and_then(|n| n.padding.right)
                .map(|x| x.resolve(s, 0.0))
                .unwrap_or(pad_l),
            b: mi
                .and_then(|n| n.padding.bottom)
                .map(|x| x.resolve(s, 0.0))
                .unwrap_or(pad_t),
        };
        // item 高亮圆角：menu.item.border_radius 优先，默认 4。
        let hover_radius = resolve_dim(mi.and_then(|n| n.border_radius), 4.0);
        // root 四边内边距独立可配，默认 4；未写的边回退到 left（保持既有主题的对称外观）。
        let root_pad_l = resolve_dim(mroot.and_then(|n| n.padding.left), 4.0);
        let root_pad = Edges {
            l: root_pad_l,
            t: mroot
                .and_then(|n| n.padding.top)
                .map(|x| x.resolve(s, 0.0))
                .unwrap_or(root_pad_l),
            r: mroot
                .and_then(|n| n.padding.right)
                .map(|x| x.resolve(s, 0.0))
                .unwrap_or(root_pad_l),
            b: mroot
                .and_then(|n| n.padding.bottom)
                .map(|x| x.resolve(s, 0.0))
                .unwrap_or(root_pad_l),
        };
        // 内容最小宽：menu.min_width 优先，默认 90。
        let min_w = resolve_dim(views.and_then(|v| v.menu_min_width), 90.0);

        // 统一项宽 = 勾选列 + 最长标签 + 内边距；子菜单项额外预留 ▸ 列宽（右对齐固定到末端）。
        let arrow_w = self.renderer.measure_text(SUBMENU_ARROW).width;
        let arrow_gap = 12.0 * s; // 标签与 ▸ 之间的最小留白
        // 任一项可勾选时，所有项预留固定勾选列，保证文字左对齐（✓ 与空白等宽）。
        let has_check = items.iter().any(|it| it.checked);
        let check_col = if has_check {
            self.renderer.measure_text(CHECK_MARK).width + 6.0 * s
        } else {
            0.0
        };
        let mut max_label = 0.0f32;
        for it in items {
            if !is_separator(it) {
                let mut w = check_col + self.renderer.measure_text(&it.label).width;
                if is_submenu(it) {
                    w += arrow_gap + arrow_w;
                }
                max_label = max_label.max(w);
            }
        }
        let item_w = (max_label + pad.l + pad.r).max(min_w);

        let mut root = View::container(Layout::Column)
            .bg(self.bg)
            .border(self.border, self.border_w)
            .radius(self.radius)
            .pad(root_pad);
        // 主题渐变 + 位图背景 + 层（jidian menu.root 吃九宫格 panel + 角标水印）。
        if let Some(g) = &self.bg_gradient {
            root = root.bg_gradient(g.clone());
        }
        if let Some(img) = &self.bg_image {
            root = root.bg_image(img.clone());
        }
        if !self.layers.is_empty() {
            root = root.layers(self.layers.clone());
        }

        for (i, it) in items.iter().enumerate() {
            if is_separator(it) {
                root = root.child(
                    View::container(Layout::Row)
                        .fixed_w(item_w)
                        .fixed_h(1.0_f32.max(s))
                        .margin(Edges::xy(0.0, 3.0 * s))
                        .bg(self.sep),
                );
                continue;
            }
            // 高亮项（选中/悬停且可用）文字用 hl_fg，否则基态 fg / 禁用色。
            let is_hl = i == selected && it.enabled;
            let color = if !it.enabled {
                self.disabled
            } else if is_hl {
                self.hl_fg
            } else {
                self.fg
            };
            let mut item = View::container(Layout::Row)
                .fixed_w(item_w)
                .fixed_h(item_h)
                .pad(pad)
                .radius(hover_radius)
                .cross(Align::Center)
                .tag(i as i32);
            // 勾选列（固定宽）：✓ 仅勾选项显示，但所有项占同宽 → 标签对齐。
            if has_check {
                let mark = if it.checked { CHECK_MARK } else { "" };
                item = item.child(
                    View::leaf(mark, color)
                        .fixed_w(check_col)
                        .text_align(Align::Start),
                );
            }
            item = item.child(View::leaf(it.label.clone(), color));
            // 子菜单 ▸：弹性占位把它推到菜单项右端（与标签解耦，标准菜单观感）。
            if is_submenu(it) {
                item = item
                    .child(View::spacer())
                    .child(View::leaf(SUBMENU_ARROW, color));
            }
            if is_hl {
                item = item.bg(self.hl_bg);
            }
            root = root.child(item);
        }

        root.layout(0.0, 0.0, &self.renderer);
        let (w_f, h_f) = root.measured_size();
        let width = (w_f.ceil() as u32).max(80);
        let height = (h_f.ceil() as u32).max(24);

        let mut hits = Vec::new();
        root.collect_hits(&mut hits);
        let item_rects: Vec<(usize, Rect)> = hits.iter().map(|(t, r)| (*t as usize, *r)).collect();

        (root, width, height, item_rects)
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ReleaseCapture();
            }
            for w in &self.windows {
                unsafe {
                    let _ = ShowWindow(w.hwnd(), SW_HIDE);
                }
            }
            self.visible = false;
            set_menu_visible(false);
            // 窗口已全部隐藏，基线须作废，否则下次弹出时内容相同的层会跳过 show、根本不出现。
            self.invalidate_rendered();
            let mut st = self.state.borrow_mut();
            st.levels.clear();
            st.closed = false;
            st.dirty = false;
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 返回根菜单窗口句柄（截图用；菜单不可见时返回 None）。
    #[cfg(windows)]
    pub fn hwnd(&self) -> Option<windows::Win32::Foundation::HWND> {
        self.windows.first().map(|w| w.hwnd())
    }

    /// 将根菜单当前渲染帧保存为 PNG 文件（截图用）。
    pub fn capture_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        self.windows
            .first()
            .ok_or_else(|| "no menu window".to_string())?
            .capture_to_file(path)
    }
}

/// 一级菜单当前画好的一帧（外部宿主形态：像素交宿主贴图）。
#[cfg(ext_presenter)]
pub(crate) struct LevelFrame<'a> {
    /// 窗口几何 `(左, 上, 宽, 高)`，屏幕坐标，含软投影扩边。
    pub geom: (i32, i32, u32, u32),
    /// 本帧内容指纹：与上次推给宿主的相同就不必重推（高亮没变、没挪位）。
    pub key: LevelKey,
    pub buf: &'a [u8],
    pub software_shadow: bool,
}

/// [`LevelFrame::key`]：同 [`plan_render`] 的增量判据——条目、高亮、几何三者都没变即像素没变。
#[cfg(ext_presenter)]
#[derive(Clone, PartialEq)]
pub(crate) struct LevelKey(LevelRender);

/// Linux 外部宿主（Fcitx5 addon）形态：菜单窗口不在本进程，[`LayeredWindow`] 是只持缓冲区的
/// mock，像素由 `menu_host` 取走经 SHM 推给 addon。菜单的状态机、渲染、定位、命中测试
/// 与 Windows 是同一份，只是鼠标事件改由宿主报来（[`Self::on_pointer`]）、工作区改由宿主
/// 报来（[`set_host_work_area`]）。
#[cfg(ext_presenter)]
impl PopupMenu {
    /// 宿主报来的指针事件（屏幕坐标）。语义对齐 Windows：移动与左键同 wnd_proc；右键在哪都只关菜单；
    /// 中键只有落在菜单外才关。
    pub(crate) fn on_pointer(&mut self, event: crate::manager::MenuPointerEvent, x: i32, y: i32) {
        use crate::manager::MenuPointerEvent as E;
        if !self.visible {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            match event {
                E::Move => st.pointer_move(x, y),
                E::LeftPress => st.pointer_left_press(x, y),
                E::RightPress => st.close(),
                E::OtherPress => {
                    if st.find_hit(x, y).is_none() {
                        st.close();
                    }
                }
            }
        }
        self.tick();
    }

    /// 可见时强制重排重绘一遍（主题变了：`set_theme` 只作废基线，不会自己重画）。
    pub(crate) fn repaint(&mut self) {
        if self.visible {
            self.reconcile();
        }
    }

    /// 当前各级已画好的帧（下标 = 级）。不可见时为空。
    pub(crate) fn level_frames(&self) -> Vec<LevelFrame<'_>> {
        if !self.visible {
            return Vec::new();
        }
        let n = self.state.borrow().levels.len();
        self.rendered
            .iter()
            .zip(&self.windows)
            .take(n)
            .map_while(|(r, w)| {
                let r = r.as_ref()?;
                Some(LevelFrame {
                    geom: r.geom,
                    key: LevelKey(r.clone()),
                    buf: w.buffer(),
                    software_shadow: self.shadow.is_some(),
                })
            })
            .collect()
    }
}

#[cfg(ext_presenter)]
thread_local! {
    /// 宿主报来的工作区（左, 上, 右, 下）。菜单与它的渲染线程同在 forwarder 线程。
    static HOST_WORK_AREA: std::cell::Cell<Option<(i32, i32, i32, i32)>> =
        const { std::cell::Cell::new(None) };
    /// 本次菜单的渲染缩放。macOS 的宿主坐标是点、菜单几何是像素，`scale_for_point` 拿像素坐标
    /// 去查显示器会查错屏，故由 `menu_host` 按宿主点坐标定好后塞进来；`None` = 照常按点查。
    static HOST_MENU_SCALE: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) };
}

/// 记下本次菜单的渲染缩放（外部宿主形态），见 `HOST_MENU_SCALE`。
#[cfg(ext_presenter)]
pub(crate) fn set_host_menu_scale(scale: Option<f32>) {
    HOST_MENU_SCALE.with(|c| c.set(scale));
}

/// 记下宿主报来的工作区，供 [`work_area_of`] 用（Linux 外部宿主形态）。
#[cfg(ext_presenter)]
pub(crate) fn set_host_work_area(area: Option<(i32, i32, i32, i32)>) {
    HOST_WORK_AREA.with(|c| c.set(area));
}

/// 勾选标记（占独立固定宽列，保证标签对齐）。
const CHECK_MARK: &str = "✓";

/// 子菜单指示符（固定到菜单项右端）。
const SUBMENU_ARROW: &str = "▸";

impl WindowMouse for MenuState {
    fn on_message(
        &mut self,
        _hwnd: HWND,
        msg: u32,
        _wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        let v = lparam.0 as u32;
        let x = (v & 0xFFFF) as i16 as i32;
        let y = ((v >> 16) & 0xFFFF) as i16 as i32;
        let (sx, sy) = self.screen(x, y);
        match msg {
            WM_MOUSEMOVE => {
                self.pointer_move(sx, sy);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                self.pointer_left_press(sx, sy);
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN => {
                self.close();
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                unsafe {
                    if let Ok(c) = LoadCursorW(None, IDC_ARROW) {
                        SetCursor(c);
                    }
                }
                Some(LRESULT(1))
            }
            _ => None,
        }
    }
}

/// 写剪贴板（CF_UNICODETEXT）。失败仅记 warn（菜单"复制"等 best-effort 调用方用）；
/// 需要感知失败的调用方（cmdbar clip.copy）用 [`try_set_clipboard_text`]。
#[cfg(windows)]
pub fn set_clipboard_text(text: &str) {
    if let Err(e) = try_set_clipboard_text(text) {
        tracing::warn!("写剪贴板失败: {}", e);
    }
}

/// OpenClipboard 带重试：剪贴板是全局竞争资源，其它进程（剪贴板管理器/Office 等）
/// 短暂持有时会瞬时失败，稍候重试即可成功。
#[cfg(windows)]
fn open_clipboard_retry() -> anyhow::Result<()> {
    const ATTEMPTS: u32 = 5;
    for i in 0..ATTEMPTS {
        if unsafe { OpenClipboard(HWND::default()) }.is_ok() {
            return Ok(());
        }
        if i + 1 < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    anyhow::bail!(
        "OpenClipboard 失败（重试 {} 次后仍被其它进程占用）",
        ATTEMPTS
    )
}

/// 写剪贴板（CF_UNICODETEXT），失败返回错误。空文本为空操作（语义：不清空剪贴板）。
#[cfg(windows)]
pub fn try_set_clipboard_text(text: &str) -> anyhow::Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    open_clipboard_retry()?;
    // 此后所有路径都必须 CloseClipboard 再返回。
    let result = unsafe {
        let _ = EmptyClipboard();
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = wide.len() * std::mem::size_of::<u16>();
        match GlobalAlloc(GMEM_MOVEABLE, bytes) {
            Ok(hmem) => {
                let ptr = GlobalLock(hmem) as *mut u16;
                if ptr.is_null() {
                    let _ = GlobalFree(hmem);
                    Err(anyhow::anyhow!("GlobalLock 失败"))
                } else {
                    std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                    let _ = GlobalUnlock(hmem);
                    match SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(hmem.0)) {
                        // 成功后 hmem 所有权归系统，不能 GlobalFree
                        Ok(_) => Ok(()),
                        Err(e) => {
                            let _ = GlobalFree(hmem);
                            Err(anyhow::anyhow!("SetClipboardData 失败: {}", e))
                        }
                    }
                }
            }
            Err(e) => Err(anyhow::anyhow!("GlobalAlloc 失败: {}", e)),
        }
    };
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

/// 写剪贴板（macOS：经 `pbcopy` 子进程，无需 AppKit/主线程，服务进程即可用；对齐 Go clipboard_darwin）。
#[cfg(target_os = "macos")]
pub fn set_clipboard_text(text: &str) {
    if let Err(e) = try_set_clipboard_text(text) {
        tracing::warn!("写剪贴板失败: {}", e);
    }
}

/// `pbcopy` / `pbpaste` 的命令，强制 UTF-8 locale。
///
/// 两者按 locale 决定文本编码；LaunchAgent 拉起的服务没有 `LANG`，中文读写都会乱码
/// （终端起的 dev 服务继承了 shell 的 `LANG`，开发机上复现不出）。`LC_ALL` 优先级最高，一并设。
#[cfg(target_os = "macos")]
fn pasteboard_command(path: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(path);
    cmd.env("LANG", "en_US.UTF-8").env("LC_ALL", "en_US.UTF-8");
    cmd
}

/// 写剪贴板（macOS pbcopy），失败返回错误。
#[cfg(target_os = "macos")]
pub fn try_set_clipboard_text(text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = pasteboard_command("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("pbcopy 启动失败: {}", e))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| anyhow::anyhow!("pbcopy 写入失败: {}", e))?;
    }
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("pbcopy 退出码非零: {}", status);
    }
    Ok(())
}

/// 写剪贴板（Linux 外部宿主形态：wl-clipboard / xclip，见 `linux_host`）。
#[cfg(all(target_os = "linux", ext_presenter))]
pub fn set_clipboard_text(text: &str) {
    if let Err(e) = try_set_clipboard_text(text) {
        tracing::warn!("写剪贴板失败: {}", e);
    }
}

#[cfg(all(target_os = "linux", ext_presenter))]
pub fn try_set_clipboard_text(text: &str) -> anyhow::Result<()> {
    crate::linux_host::set_text(text)
}

/// 写剪贴板（其它非 Windows mock：暂不接入平台剪贴板，空操作）。
#[cfg(not(any(windows, ext_presenter)))]
pub fn set_clipboard_text(_text: &str) {}

/// 写剪贴板（其它非 Windows mock）：明确报不支持。
#[cfg(not(any(windows, ext_presenter)))]
pub fn try_set_clipboard_text(_text: &str) -> anyhow::Result<()> {
    anyhow::bail!("写剪贴板：当前平台暂未支持")
}

/// 剪贴板文本缓存 `(序列号, 文本)`，失效判据见 [`get_clipboard_text`]。
///
/// 缓存持有剪贴板全文副本，刻意不设大小上限：超限不缓存会让超大剪贴板下每次按键都重跑整段转换，
/// 比多占一份内存糟。若内存成问题，应给显示路径缓存截断版、执行路径仍真读，而非超限不缓存。
#[cfg(windows)]
static CLIP_CACHE: std::sync::Mutex<Option<(u32, String)>> = std::sync::Mutex::new(None);

/// 取缓存。`seq == 0` 表示序列号不可用（调用方无窗口站剪贴板访问权限，罕见）——
/// 此时判据失效，一律未命中，绝不能拿旧值冒充当前内容。
#[cfg(windows)]
fn clip_cache_get(seq: u32) -> Option<String> {
    if seq == 0 {
        return None;
    }
    let g = CLIP_CACHE.lock().ok()?;
    let (s, t) = g.as_ref()?;
    (*s == seq).then(|| t.clone())
}

/// 写缓存。`seq == 0` 时不写——没有有效判据的缓存日后无法判断是否过期。
#[cfg(windows)]
fn clip_cache_put(seq: u32, text: &str) {
    if seq == 0 {
        return;
    }
    if let Ok(mut g) = CLIP_CACHE.lock() {
        *g = Some((seq, text.to_string()));
    }
}

#[cfg(windows)]
fn clip_cache_stale() -> String {
    CLIP_CACHE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(_, t)| t.clone()))
        .unwrap_or_default()
}

/// 读剪贴板文本（CF_UNICODETEXT），带序列号缓存。失败/无文本返回空串。
///
/// `GetClipboardSequenceNumber` 只读窗口站计数器，不需要 `OpenClipboard`、不被别的进程挡住；
/// 内容没变时直接复用上次结果。缓存未命中时仍会重试等待，按键线程上的显示路径应改用
/// [`get_clipboard_text_cached`]。
#[cfg(windows)]
pub fn get_clipboard_text() -> String {
    read_clipboard(true)
}

/// 同 [`get_clipboard_text`]，但绝不阻塞：缓存未命中且剪贴板被占用时直接返回上次缓存的文本
/// （从未读到过则空串），不做 sleep 重试。
///
/// 专供只用于显示的场景（候选标签），陈旧一拍无害。执行动作的路径不可用本函数——陈旧内容
/// 会粘错东西，须用 [`get_clipboard_text`]（打不开时返回空串而非旧值）。
#[cfg(windows)]
pub fn get_clipboard_text_cached() -> String {
    read_clipboard(false)
}

/// 剪贴板读取实现。`allow_retry` 区分两种失败语义，见两个公开包装的文档。
#[cfg(windows)]
fn read_clipboard(allow_retry: bool) -> String {
    let seq = unsafe { GetClipboardSequenceNumber() };
    if let Some(hit) = clip_cache_get(seq) {
        return hit;
    }

    let opened = if allow_retry {
        open_clipboard_retry().is_ok()
    } else {
        unsafe { OpenClipboard(HWND::default()) }.is_ok()
    };
    if !opened {
        // 执行路径宁可给空串也不能给陈旧内容（会粘错）；显示路径退回旧标签即可。
        return if allow_retry {
            String::new()
        } else {
            clip_cache_stale()
        };
    }

    let out = unsafe {
        let mut out = String::new();
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
            let hglobal = HGLOBAL(h.0);
            let ptr = GlobalLock(hglobal) as *const u16;
            if !ptr.is_null() {
                // 读到 NUL 结尾；以 GlobalSize 为上界，防生产者未写 NUL 时越界读。
                let max_len = GlobalSize(hglobal) / std::mem::size_of::<u16>();
                let mut len = 0usize;
                while len < max_len && *ptr.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(ptr, len);
                out = String::from_utf16_lossy(slice);
                let _ = GlobalUnlock(hglobal);
            }
        }
        let _ = CloseClipboard();
        out
    };

    // 以读之前的 seq 作键：期间剪贴板变了顶多下次多读一次；用读之后的 seq 会把旧内容盖上新序号。
    clip_cache_put(seq, &out);
    out
}

/// 读剪贴板文本（macOS：经 `pbpaste` 子进程）。失败/无文本返回空串。
#[cfg(target_os = "macos")]
pub fn get_clipboard_text() -> String {
    match pasteboard_command("/usr/bin/pbpaste").output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

// ── macOS 剪贴板缓存读取（Carbon Pasteboard Manager，免子进程） ──────────────
//
// `get_clipboard_text` 走 `pbpaste`（一次 fork+exec）；[`get_clipboard_text_cached`] 在每次按键的
// 候选构建期调用，不能 spawn 进程，故另走 Pasteboard Manager C API 直读并按变更计数缓存。

/// macOS 剪贴板缓存：长生命周期的 `PasteboardRef` + 上次读到的文本。
///
/// `PasteboardRef` 必须常驻：`kPasteboardModified` 的语义是「自该 ref 上次同步以来是否被改过」，
/// 新建的 ref 首次同步恒返回 0，每次新建判据就失效。
///
/// 裸指针跨线程：Pasteboard Manager 的调用全部收在本模块的 Mutex 之内，无并发访问。
#[cfg(target_os = "macos")]
struct ClipCacheMac {
    pb: pasteboard::PasteboardRef,
    text: String,
    /// 是否已真正读过一次。新建 ref 的首次同步不报 modified，故须靠它强制首读。
    primed: bool,
}
#[cfg(target_os = "macos")]
unsafe impl Send for ClipCacheMac {}

#[cfg(target_os = "macos")]
static CLIP_CACHE_MAC: std::sync::Mutex<Option<ClipCacheMac>> = std::sync::Mutex::new(None);

#[cfg(target_os = "macos")]
mod pasteboard {
    use core_foundation_sys::base::CFTypeRef;
    use core_foundation_sys::data::CFDataRef;
    use core_foundation_sys::string::CFStringRef;
    use std::ffi::c_void;

    pub type OSStatus = i32;
    pub type PasteboardRef = *mut c_void;
    pub type PasteboardItemID = *mut c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        pub fn PasteboardCreate(name: CFStringRef, out: *mut PasteboardRef) -> OSStatus;
        /// 与系统剪贴板同步，返回本次同步的标志位；顺带更新变更计数。
        pub fn PasteboardSynchronize(pb: PasteboardRef) -> u32;
        pub fn PasteboardGetItemCount(pb: PasteboardRef, out: *mut u64) -> OSStatus;
        pub fn PasteboardGetItemIdentifier(
            pb: PasteboardRef,
            index: u64,
            out: *mut PasteboardItemID,
        ) -> OSStatus;
        pub fn PasteboardCopyItemFlavorData(
            pb: PasteboardRef,
            item: PasteboardItemID,
            flavor: CFStringRef,
            out: *mut CFDataRef,
        ) -> OSStatus;
        pub fn CFRelease(cf: CFTypeRef);
    }
}

/// 造一个 CFString（+1，调用方负责释放）。
#[cfg(target_os = "macos")]
unsafe fn cfstr(s: &str) -> core_foundation_sys::string::CFStringRef {
    use core_foundation_sys::base::kCFAllocatorDefault;
    use core_foundation_sys::string::{CFStringCreateWithBytes, kCFStringEncodingUTF8};
    unsafe {
        CFStringCreateWithBytes(
            kCFAllocatorDefault,
            s.as_ptr(),
            s.len() as isize,
            kCFStringEncodingUTF8,
            false as u8,
        )
    }
}

/// 同 [`get_clipboard_text`]，但不 spawn 子进程：经 Pasteboard Manager 直读并按变更计数缓存。
/// 专供只用于显示的场景（候选标签）；执行动作的路径请用 [`get_clipboard_text`]。
#[cfg(target_os = "macos")]
pub fn get_clipboard_text_cached() -> String {
    use pasteboard::*;
    /// `kPasteboardModified`：自本 ref 上次同步以来剪贴板被改过。
    const K_PASTEBOARD_MODIFIED: u32 = 1;
    /// `kPasteboardClipboard` 在头文件里是 `CFSTR(...)` 宏、不是导出符号，故自造。
    const CLIPBOARD_NAME: &str = "com.apple.pasteboard.clipboard";
    const UTF8_FLAVOR: &str = "public.utf8-plain-text";

    let mut guard = CLIP_CACHE_MAC.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        if guard.is_none() {
            let name = cfstr(CLIPBOARD_NAME);
            if name.is_null() {
                return String::new();
            }
            let mut pb: PasteboardRef = std::ptr::null_mut();
            let st = PasteboardCreate(name, &mut pb);
            CFRelease(name as _);
            if st != 0 || pb.is_null() {
                return String::new();
            }
            *guard = Some(ClipCacheMac {
                pb,
                text: String::new(),
                primed: false,
            });
        }
        let cache = guard.as_mut().expect("刚建过");

        let flags = PasteboardSynchronize(cache.pb);
        if cache.primed && flags & K_PASTEBOARD_MODIFIED == 0 {
            return cache.text.clone(); // 未变更：纯内存命中，不碰系统
        }

        let mut text = String::new();
        // 这一轮是否读到了确定的答案，须与「读到了空」区分：剪贴板是图片时空串是正确答案；
        // API 失败时的空串若连同 primed=true 提交，下次同步报「未变更」，标签就恒为空直到重新复制。
        let mut read_ok = false;
        let flavor = cfstr(UTF8_FLAVOR);
        let mut n: u64 = 0;
        if !flavor.is_null() && PasteboardGetItemCount(cache.pb, &mut n) == 0 {
            read_ok = true;
            // 项索引从 1 起（Pasteboard Manager 的约定，不是 0）。
            for i in 1..=n {
                let mut id: PasteboardItemID = std::ptr::null_mut();
                if PasteboardGetItemIdentifier(cache.pb, i, &mut id) != 0 {
                    continue;
                }
                let mut data: core_foundation_sys::data::CFDataRef = std::ptr::null();
                if PasteboardCopyItemFlavorData(cache.pb, id, flavor, &mut data) == 0
                    && !data.is_null()
                {
                    use core_foundation_sys::data::{CFDataGetBytePtr, CFDataGetLength};
                    let len = CFDataGetLength(data) as usize;
                    let ptr = CFDataGetBytePtr(data);
                    if !ptr.is_null() && len > 0 {
                        text = String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len))
                            .into_owned();
                    }
                    CFRelease(data as _);
                    break; // 只取第一项文本，与 pbpaste 行为一致
                }
            }
        }
        if !flavor.is_null() {
            CFRelease(flavor as _);
        }
        if !read_ok {
            // 没读成：不动缓存也不置 primed，下次照常重试；返回上次已知值。
            tracing::debug!("剪贴板读取失败，沿用上次缓存值");
            return cache.text.clone();
        }
        cache.text = text.clone();
        cache.primed = true;
        text
    }
}

/// 读剪贴板文本（Linux 外部宿主形态）。
#[cfg(all(target_os = "linux", ext_presenter))]
pub fn get_clipboard_text() -> String {
    crate::linux_host::get_text()
}

/// Linux 外部宿主形态：返回空串（同 Windows / macOS 以外的「拿不到就空」）。
///
/// 调用点在每次按键的候选构建期，读一次就要起 `wl-paste` / `xclip` 子进程，持有方挂住时按键线程跟着挂；
/// Linux 又没有廉价的变更计数可做缓存。代价是这类候选的显示标签里剪贴板那段为空；
/// 执行动作走 [`get_clipboard_text`]，不受影响。
#[cfg(all(target_os = "linux", ext_presenter))]
pub fn get_clipboard_text_cached() -> String {
    String::new()
}

/// 读剪贴板文本（其它非 Windows mock：返回空串）。
#[cfg(not(any(windows, ext_presenter)))]
pub fn get_clipboard_text() -> String {
    String::new()
}

/// 其它非 Windows mock：无剪贴板可读。
#[cfg(not(any(windows, ext_presenter)))]
pub fn get_clipboard_text_cached() -> String {
    String::new()
}

fn dpi_scale() -> f32 {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGPIXELSY, ReleaseDC};
        unsafe {
            let hdc = GetDC(HWND::default());
            let dpi = GetDeviceCaps(hdc, LOGPIXELSY);
            ReleaseDC(HWND::default(), hdc);
            if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 }
        }
    }
    #[cfg(not(windows))]
    {
        1.0
    }
}

/// 侧向弹出时菜单与锚点之间的间隙（dp，随 DPI 缩放）。
///
/// 左右两侧取同一个值——观感统一是这里的唯一目的，别按方向调偏。
const SIDE_GAP_DP: f32 = 6.0;

/// 按 `placement` 把菜单摆到锚点旁，返回菜单左上角（**尚未钳制**到工作区）。
///
/// `ax`/`ay` 是解析过 `i32::MIN`（取光标位）之后的锚点左上角；`wa` 为该点所在显示器的工作区，
/// 取不到时按「首选方向装得下」处理，越界交给调用方的 `clamp_to_work_area` 兜底。
fn place_menu(
    anchor: &MenuAnchor,
    ax: i32,
    ay: i32,
    w: u32,
    h: u32,
    wa: Option<(i32, i32, i32, i32)>,
    scale: f32,
) -> (i32, i32) {
    match anchor.placement {
        MenuPlacement::Below => (ax, ay),
        MenuPlacement::Above => {
            // 底边贴锚点顶边向上展开；顶出工作区则翻到锚点底边之下。
            let up = ay - h as i32;
            let fits_up = wa.map(|(_, top, _, _)| up >= top).unwrap_or(true);
            (ax, if fits_up { up } else { anchor.bottom })
        }
        MenuPlacement::Side => {
            // 两侧留同样宽的间隙：投影带方向性偏移，紧贴时会显得一侧紧贴、一侧有缝。
            let gap = (SIDE_GAP_DP * scale).round() as i32;
            // 右侧装得下走右侧，否则走左侧；间隙一并计入判据。两侧都装不下仍取右侧：
            // 左侧钳回来后会正好压在工具栏上。
            let fits_right = wa
                .map(|(_, _, right, _)| anchor.right + gap + w as i32 <= right)
                .unwrap_or(true);
            let fits_left = wa
                .map(|(left, _, _, _)| ax - gap - w as i32 >= left)
                .unwrap_or(true);
            let x = if fits_right || !fits_left {
                anchor.right + gap
            } else {
                ax - gap - w as i32
            };
            // 纵坐标与锚点底边对齐（贴着工具栏往上长，同任务栏菜单）。顶边对齐会让高菜单底边
            // 顶出工作区、钳回后整个推到条顶之上。底边对齐顶出上边时改回顶边对齐向下展开。
            let up = anchor.bottom - h as i32;
            let fits_up = wa.map(|(_, top, _, _)| up >= top).unwrap_or(true);
            (x, if fits_up { up } else { ay })
        }
    }
}

/// 取某屏幕点所在显示器的工作区矩形 (left, top, right, bottom)。
fn work_area_of(x: i32, y: i32) -> Option<(i32, i32, i32, i32)> {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        unsafe {
            let pt = POINT { x, y };
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let wa = mi.rcWork;
                return Some((wa.left, wa.top, wa.right, wa.bottom));
            }
        }
    }
    // Linux：屏幕几何只有 addon 拿得到，随「打开菜单」请求报上来（整块根窗口，不分显示器）。
    #[cfg(ext_presenter)]
    if let Some(wa) = HOST_WORK_AREA.with(|c| c.get()) {
        return Some(wa);
    }
    let _ = (x, y);
    None
}

/// 将菜单钳制在光标所在显示器工作区内（右/下溢出贴边）。
fn clamp_to_work_area(x: i32, y: i32, w: u32, h: u32) -> (i32, i32) {
    if let Some((left, top, right, bottom)) = work_area_of(x, y) {
        let (wi, hi) = (w as i32, h as i32);
        let mut nx = x;
        let mut ny = y;
        if nx + wi > right {
            nx = right - wi;
        }
        if ny + hi > bottom {
            ny = (y - hi).max(top);
        }
        if nx < left {
            nx = left;
        }
        if ny < top {
            ny = top;
        }
        return (nx, ny);
    }
    (x, y)
}

/// 子菜单定位：贴父面板右缘展开，纵向对齐父高亮项；某侧放不下则翻到另一侧；最后钳制工作区。
///
/// `prefer_left`：本次展开链此前是否已翻到左侧。一旦翻过，后续层级延续左侧（左侧也放不下才翻回），
/// 否则更深层会翻回右侧压在上一层面板上，形成锯齿重叠。
/// 返回值第三项是本层实际选定的方向，供调用方串成下一层的 `prefer_left`（见 `reconcile`）。
fn place_child(
    parent_origin: (i32, i32),
    parent_size: (u32, u32),
    parent_item: Option<Rect>,
    child_size: (u32, u32),
    scale: f32,
    prefer_left: bool,
) -> (i32, i32, bool) {
    let (pox, poy) = parent_origin;
    place_child_in_area(
        parent_origin,
        parent_size,
        parent_item,
        child_size,
        scale,
        prefer_left,
        work_area_of(pox, poy),
    )
}

/// [`place_child`] 的纯逻辑部分：工作区矩形由调用方传入，而非在函数内部现查。
///
/// 拆出来让测试能注入工作区：`work_area_of` 在非 Windows 上恒为 `None`，测不到翻转分支。
fn place_child_in_area(
    parent_origin: (i32, i32),
    parent_size: (u32, u32),
    parent_item: Option<Rect>,
    child_size: (u32, u32),
    scale: f32,
    prefer_left: bool,
    work_area: Option<(i32, i32, i32, i32)>,
) -> (i32, i32, bool) {
    let (pox, poy) = parent_origin;
    let (pw, _ph) = parent_size;
    let (cw, ch) = (child_size.0 as i32, child_size.1 as i32);
    let overlap = (3.0 * scale) as i32;
    let pad_top = (4.0 * scale) as i32;

    let item_top = parent_item.map(|r| r.y as i32).unwrap_or(0);
    let x_right = pox + pw as i32 - overlap;
    let x_left = pox - cw + overlap;
    let mut y = poy + item_top - pad_top;

    let (x, went_left) = match work_area {
        Some((left, top, right, bottom)) => {
            let fits_right = x_right + cw <= right;
            let fits_left = x_left >= left;
            let (x, went_left) = if prefer_left {
                // 延续左侧展开；左侧也放不下时才翻回右侧。
                if fits_left {
                    (x_left, true)
                } else {
                    (x_right, false)
                }
            } else if fits_right {
                (x_right, false)
            } else {
                (x_left, true)
            };
            if y + ch > bottom {
                y = bottom - ch;
            }
            if y < top {
                y = top;
            }
            (x.max(left), went_left)
        }
        // 取不到工作区：无从判断是否装得下，按当前偏好方向摆（与 place_menu 同一原则）。
        None => (if prefer_left { x_left } else { x_right }, prefer_left),
    };
    (x, y, went_left)
}

/// 「点菜单外面就关」的判据（见 [`PopupMenu::poll_outside_press`]）。
///
/// 纯轮询驱动，错了只会表现为「菜单一弹就没」或「点外面关不掉」这类难复现的时序问题。
#[cfg(test)]
#[path = "popup_menu_outside_press_tests.rs"]
mod outside_press_tests;

#[cfg(test)]
#[path = "popup_menu_render_plan_tests.rs"]
mod render_plan_tests;

/// 悬停语义测试：不依赖窗口，直接喂几何给 `MenuState`。
#[cfg(test)]
#[path = "popup_menu_hover_tests.rs"]
mod hover_tests;

#[cfg(all(test, windows))]
mod clipboard_tests {
    use super::*;

    /// 缓存必须随剪贴板内容变更而失效，否则 `clip()` 会一直吐旧内容、粘错东西。
    ///
    /// 标 `#[ignore]`：它真写系统剪贴板，会覆盖跑测试者当下的内容。
    /// 改动 `read_clipboard` / 缓存判据后，请在 Windows 本机手动跑一次：
    /// `cargo test -p wind-ui --lib clipboard_cache -- --ignored --nocapture`
    #[test]
    #[ignore = "真写系统剪贴板，会覆盖使用者当前内容；须在 Windows 本机手动跑"]
    fn clipboard_cache_invalidates_on_change() {
        const A: &str = "WindInput-clip-cache-A";
        const B: &str = "WindInput-clip-cache-B";
        let original = get_clipboard_text();

        set_clipboard_text(A);
        assert_eq!(get_clipboard_text(), A, "写入后首读");
        assert_eq!(get_clipboard_text(), A, "二读（应命中缓存）");
        assert_eq!(
            get_clipboard_text_cached(),
            A,
            "非阻塞版与阻塞版共用同一缓存"
        );

        // 内容变更 → GetClipboardSequenceNumber 递增 → 缓存必须失效。
        set_clipboard_text(B);
        assert_eq!(
            get_clipboard_text(),
            B,
            "缓存未随剪贴板变更失效（会粘错内容）"
        );
        assert_eq!(get_clipboard_text_cached(), B, "非阻塞版同样须失效");

        // 尽力恢复（set_clipboard_text 对空串是 no-op，故原本为空时无法还原）。
        if !original.is_empty() {
            set_clipboard_text(&original);
        }
    }
}

#[cfg(test)]
#[path = "popup_menu_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "macos"))]
mod clipboard_tests_macos {
    use super::*;

    /// pbcopy/pbpaste 必须带 UTF-8 locale（见 `pasteboard_command`）。终端里跑测试有 `LANG`，
    /// 往返测试抓不到，故直接看命令。
    #[test]
    fn pasteboard_command_forces_utf8_locale() {
        let cmd = pasteboard_command("/usr/bin/pbpaste");
        let envs: Vec<_> = cmd.get_envs().collect();
        for key in ["LANG", "LC_ALL"] {
            assert!(
                envs.iter().any(|(k, v)| *k == key
                    && v.and_then(|v| v.to_str())
                        .is_some_and(|v| v.ends_with("UTF-8"))),
                "{key} 未设成 UTF-8：{envs:?}"
            );
        }
    }

    /// macOS 版的同一条不变量：缓存必须随剪贴板变更失效。
    ///
    /// 标 `#[ignore]`：真写系统剪贴板。改动 `get_clipboard_text_cached` 的 macOS 实现后手动跑：
    /// `env -u LANG -u LC_ALL -u LC_CTYPE cargo test -p wind-ui --lib clipboard_cache -- --ignored --nocapture`
    #[test]
    #[ignore = "真写系统剪贴板，会覆盖使用者当前内容；须在 macOS 本机手动跑"]
    fn clipboard_cache_invalidates_on_change_macos() {
        const A: &str = "WindInput-clip-cache-A";
        const B: &str = "WindInput-clip-cache-B";
        let original = get_clipboard_text();

        set_clipboard_text(A);
        assert_eq!(get_clipboard_text_cached(), A, "写入后首读");
        assert_eq!(get_clipboard_text_cached(), A, "二读（应命中缓存）");

        // 内容变更 → PasteboardSynchronize 置 kPasteboardModified → 缓存必须失效。
        // 判据失灵的后果是候选标签一直显示上一次的剪贴板内容。
        set_clipboard_text(B);
        assert_eq!(
            get_clipboard_text_cached(),
            B,
            "缓存未随剪贴板变更失效（候选标签会显示旧内容）"
        );

        // 中文往返：在没有 LANG 的环境里跑才有意义（模拟 LaunchAgent 下的服务）：
        // `env -u LANG -u LC_ALL -u LC_CTYPE cargo test ... -- --ignored`
        const ZH: &str = "清风输入法剪贴板中文";
        set_clipboard_text(ZH);
        assert_eq!(
            get_clipboard_text(),
            ZH,
            "中文往返走样（pbcopy/pbpaste 编码）"
        );

        if !original.is_empty() {
            set_clipboard_text(&original);
        }
    }
}

#[cfg(test)]
mod gradient_tests {
    use super::*;
    use crate::theme_assets::gradient_fixture as fx;

    /// menu.root 的 `background.gradient` 叠在底色上（口径同候选窗节点）。
    /// 取样点在 root 上内边距里（避开 1px 边框）。
    #[test]
    fn menu_root_paints_bg_gradient() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut m = PopupMenu::new(tx).expect("mock 菜单");
        let mut theme = wind_theme::Resolved::default();
        theme.views.menu_root = Some(fx::node());
        m.set_theme(&theme);
        let items = vec![MenuItemSpec::leaf("x", MenuKind::Copy, true, false)];
        let (root, w, h, _) = m.build_view(&items, NONE_SEL);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        root.paint(&mut buf, w, h, &m.renderer);
        assert_eq!(fx::rgb(&buf, w, w / 2, 2), fx::RED, "menu.root 渐变没画");

        // 换到同节点、但没配渐变的主题：渐变必须清掉，露出底色。
        let mut plain = wind_theme::Resolved::default();
        plain.views.menu_root = Some(wind_theme::RvNode {
            bg_gradient: None,
            ..fx::node()
        });
        m.set_theme(&plain);
        let (root, w, h, _) = m.build_view(&items, NONE_SEL);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        root.paint(&mut buf, w, h, &m.renderer);
        assert_eq!(fx::rgb(&buf, w, w / 2, 2), fx::BLUE, "旧主题的渐变残留");
    }
}
