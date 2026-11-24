//! 工具栏窗口：常驻状态指示器（中英 / 方案 / 标点 / 全半角 / 简繁 / 软键盘 / 设置 / 自定义按钮）。
//!
//! 横排或纵排的圆角小条，每格一个状态，点击切换、右键弹该格菜单、拖动柄（或格上越过阈值）拖动整条。
//! 位置锚窗口右下角记忆，见 [`Placement`]。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use crate::auto_hide::{AutoHide, AutoHideAction};
use crate::manager::{MenuAnchor, ToolbarAction, UiEvent};
use crate::sys::{
    GetCursorPos, GetWindowRect, HWND, HWND_TOPMOST, IDC_ARROW, IDC_SIZEALL, LPARAM, LRESULT,
    LoadCursorW, POINT, RECT, ReleaseCapture, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetCapture,
    SetCursor, SetWindowPos, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSELEAVE,
    WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_SETCURSOR, WPARAM, clamp_to_work_area,
};
use crate::text::dwrite::TextRenderer;
use crate::view::Rect;
use crate::window::{LayeredWindow, WindowMouse};
use wind_theme::schema::Dim;

/// 状态数据类型已下沉至 wind-ui-types；再导出保持 `wind_ui::toolbar::ToolbarState`
/// 原路径成立。
pub use wind_ui_types::{ToolbarItem, ToolbarState};

/// 一个单元格：文本 + 高亮(激活态，如中文/简繁开) + 淡显(次要状态，如半角/简) + 点击动作
pub(crate) struct Cell {
    text: String,
    highlight: bool,
    dim: bool,
    action: ToolbarAction,
}

// 状态图标：`res/icons/` 下的 SVG 编译期嵌入，只作 alpha 蒙版，颜色由工具栏按主题 tint。
/// 全角（中文）标点 。，
const PUNCT_FULL_SVG: &str = include_str!("../res/icons/punct_full.svg");
/// 半角（英文）标点 .,
const PUNCT_HALF_SVG: &str = include_str!("../res/icons/punct_half.svg");
/// 全角宽度：满月（实心圆）—— 对齐微软五笔全/半角月亮状态。
const WIDTH_FULL_SVG: &str = include_str!("../res/icons/width_full.svg");
/// 半角宽度：月牙（弯月）。
const WIDTH_HALF_SVG: &str = include_str!("../res/icons/width_half.svg");
/// 设置齿轮图标。
const SETTING_SVG: &str = include_str!("../res/icons/setting.svg");
/// 软键盘图标，只有一张：这一格切换的是面板开合，开关态由格底高亮表达。
const SOFT_KEYBOARD_SVG: &str = include_str!("../res/icons/soft_keyboard.svg");

/// 工具栏窗口
pub struct Toolbar {
    window: LayeredWindow,
    renderer: TextRenderer,
    scale: f32,
    visible: bool,
    /// 鼠标处理器（与 window 共享，wnd_proc 经注册表回调）；位置存于其中以便拖动同步
    mouse: Rc<RefCell<ToolbarMouse>>,
    // 外观与几何说明（set_theme 加载主题后覆盖；见 ToolbarSkin / ToolbarDims）
    skin: ToolbarSkin,
    dims: ToolbarDims,
    /// 最近一次状态（供 hover 变化时本地重绘，无需协调器往返）
    last_state: Option<ToolbarState>,
    /// 已渲染的悬停格下标（-1=无）；tick 检测光标位置变化后据此决定是否重绘
    rendered_hover: i32,
    /// 自动隐藏状态机（默认关闭；SetToolbarAutoHide 命令配置）
    auto_hide: AutoHide,
    /// 纵向排列（ui.toolbar.vertical，非主题——见 `bar_layout`）。
    vertical: bool,

    /// 显示哪些格、按什么顺序（`ui.toolbar.items`，经 `SetToolbarLayout` 下发）。
    /// 初值取全集：`UpdateToolbar` 可能先于 `SetToolbarLayout` 到达，初值为空会先闪一帧空条。
    layout: Vec<ToolbarItem>,
    /// 待落的「某显示器右下角」请求：`(工作区右边界, 下边界)`，由 `render` 消费。
    /// 存边界而不算坐标：落点要减去自身尺寸，而尺寸在 `render` 之前不可信（占位 160×40、隐藏期间不重排）。
    /// 跨 `hide` 存活，只由 `render` 消费或被 `set_anchor` 覆盖；边界过时会由协调器 `sync_toolbar_monitor` 重新下发。
    pending_corner: Option<(i32, i32)>,
    /// 最近一帧的拖动柄区域（设备像素，相对窗口左上角）；`render` 从 `bar_layout` 取。
    /// 外部宿主据此在本地拖动，见 [`Self::drag_regions`]。
    drag_rect: Rect,
}

/// 整条工具栏的几何：窗口尺寸 + 每格矩形（设备像素，相对窗口左上角）。
pub(crate) struct BarLayout {
    w: f32,
    h: f32,
    /// 拖动柄占据的区域（横条=左端竖条，纵条=顶端横条）。
    grip: Rect,
    cells: Vec<Rect>,
}

/// 按朝向铺开整条工具栏。纵向恒为横向的转置：`thickness`（主题 `[toolbar] height`）在横条里是条高、
/// 纵条里是条宽；`cell`（`button_width`）同理。同一套主题几何两个朝向都成立。抽成纯函数以便单测。
fn bar_layout(vertical: bool, thickness: f32, grip_len: f32, cell: f32, n: usize) -> BarLayout {
    let long = grip_len + cell * n as f32;
    let (w, h) = if vertical {
        (thickness, long)
    } else {
        (long, thickness)
    };
    let grip = if vertical {
        Rect {
            x: 0.0,
            y: 0.0,
            w: thickness,
            h: grip_len,
        }
    } else {
        Rect {
            x: 0.0,
            y: 0.0,
            w: grip_len,
            h: thickness,
        }
    };
    let cells = (0..n)
        .map(|i| {
            let off = grip_len + cell * i as f32;
            if vertical {
                Rect {
                    x: 0.0,
                    y: off,
                    w: thickness,
                    h: cell,
                }
            } else {
                Rect {
                    x: off,
                    y: 0.0,
                    w: cell,
                    h: thickness,
                }
            }
        })
        .collect();
    BarLayout { w, h, grip, cells }
}

/// 拖动柄矩形 → 整数像素 `[x, y, w, h]`；空矩形（首帧之前）给 `None`。
fn grip_region(r: &Rect) -> Option<[i32; 4]> {
    let (w, h) = (r.w.round() as i32, r.h.round() as i32);
    (w > 0 && h > 0).then(|| [r.x.round() as i32, r.y.round() as i32, w, h])
}

/// 整条外框圆角的兜底半径（主题 `[toolbar] border.radius` 未配时）：短边×0.30。
/// 须取短边：纵条的 `h` 是整条长度，用它算会被下游静默钳成满胶囊。
fn default_border_radius(w: f32, h: f32) -> f32 {
    w.min(h) * 0.30
}

/// 工具栏配色（默认浅色；`set_theme` 加载主题后覆盖）。
/// 与 [`ToolbarDims`] 一起抽成纯值对象，设置端预览不构造窗口也能经 [`paint_toolbar`] 按同一份折算画出。
#[derive(Clone, Copy)]
pub(crate) struct ToolbarSkin {
    pub bg: [u8; 4],
    pub fg: [u8; 4],
    pub hl_bg: [u8; 4],
    pub hl_fg: [u8; 4],
    pub sep: [u8; 4],
    pub grip: [u8; 4],
    pub settings_icon: [u8; 4],
    pub hover_bg: [u8; 4],
}

impl Default for ToolbarSkin {
    // 无主题时的兜底值
    fn default() -> Self {
        Self {
            bg: [255, 255, 255, 245],   // 白色半透明底
            fg: [72, 72, 78, 255],      // 正常文字深灰
            hl_bg: [66, 133, 244, 255], // 高亮蓝（中文模式 / 简繁启用）
            hl_fg: [255, 255, 255, 255],
            sep: [214, 214, 220, 255],           // 浅灰分隔线
            grip: [186, 186, 194, 255],          // 拖动点
            settings_icon: [140, 140, 148, 255], // 设置图标（比普通文字更淡）
            hover_bg: [0, 0, 0, 13],             // 鼠标悬停高亮（极淡，~5% 黑）
        }
    }
}

impl ToolbarSkin {
    /// 按已解析主题折算配色：token 出基础值，`[toolbar]` 节点色覆盖底色 / 边框色
    /// （token 优先于默认、节点再覆盖 token——原 `set_theme` 的顺序）。
    pub(crate) fn from_theme(theme: &wind_theme::Resolved) -> Self {
        let mut s = Self::default();
        s.bg = theme.color("toolbar_background", s.bg);
        s.fg = theme.color("toolbar_full_width_off_text", s.fg);
        s.hl_bg = theme.color("toolbar_mode_chinese_bg", s.hl_bg);
        s.hl_fg = theme.color("toolbar_mode_text", s.hl_fg);
        s.sep = theme.color("toolbar_border", s.sep);
        s.grip = theme.color("toolbar_grip", s.grip);
        s.settings_icon = theme.color("toolbar_settings_icon", s.settings_icon);
        s.hover_bg = theme.color("toolbar_hover", s.hover_bg);
        // [toolbar] 节点色覆盖上面的 token 兜底。
        if let Some(c) = theme.views.toolbar_bg_color {
            s.bg = c;
        }
        if let Some(c) = theme.views.toolbar_border_color {
            s.sep = c;
        }
        s
    }
}

/// 工具栏几何中的**主题可配部分**（`None` → [`Self::resolve`] 内置默认）；单位是
/// 逻辑像素（Dim，渲染时按 DPI 缩放）。
#[derive(Clone, Copy, Default)]
pub(crate) struct ToolbarDims {
    pub height: Option<Dim>,
    pub grip_width: Option<Dim>,
    pub button_width: Option<Dim>,
    pub button_padding: Option<Dim>,
    pub button_radius: Option<Dim>,
    /// 整条外框圆角 / 线宽（[toolbar] border.radius / .width）。
    pub border_radius: Option<Dim>,
    pub border_width: Option<Dim>,
}

impl ToolbarDims {
    /// 几何默认值（逻辑像素，随 DPI 缩放）。主题 [toolbar] 未描述时的兜底；
    /// 与 _base/theme.toml [toolbar] 保持一致，改这里也应同步 _base（反之亦然）。
    const HEIGHT: f32 = 30.0;
    const GRIP_W: f32 = 12.0;
    const BUTTON_W: f32 = 30.0; // 每格（按钮槽）宽度，字面值；配 button_width 覆盖
    const BUTTON_PAD: f32 = 4.0; // 激活/悬停格高亮内缩；配 button_padding 覆盖

    /// 按已解析主题取值（`None`→内置默认，行为不变）。
    pub(crate) fn from_theme(theme: &wind_theme::Resolved) -> Self {
        let v = &theme.views;
        Self {
            height: v.toolbar_height,
            grip_width: v.toolbar_grip_width,
            button_width: v.toolbar_button_width,
            button_padding: v.toolbar_button_padding,
            button_radius: v.toolbar_button_radius,
            border_radius: v.toolbar_border_radius,
            border_width: v.toolbar_border_width,
        }
    }
}

/// 一帧的排布结果（[`bar_layout`] 与缩放值），由 [`toolbar_frame`] 一次算出，画板与成品
/// （窗口 / 预览缓冲区）由此知道尺寸；绘制本体在 [`paint_toolbar`]。
pub(crate) struct ToolbarFrame {
    pub layout: BarLayout,
    pub w: u32,
    pub h: u32,
    pub scale: f32,
    pub radius: u32,
    pub border_w: f32,
    pub inset: u32,
    pub button_radius: u32,
}

/// 按主题几何 + 缩放 + 格数算出一帧的排布与各处圆角 / 线宽。窗口路径与预览共用。
pub(crate) fn toolbar_frame(
    dims: &ToolbarDims,
    scale: f32,
    vertical: bool,
    n: usize,
) -> ToolbarFrame {
    // Dim→设备像素（dp×scale）；None→def_logical×scale（同候选窗 dim 闭包）。
    let dim = |o: Option<Dim>, def_logical: f32| {
        o.map(|x| x.resolve(scale, 0.0))
            .unwrap_or(def_logical * scale)
    };
    // 纵向下这两个值转 90°：thickness 成条宽、grip_len 成顶端拖动区高度。
    let thickness = dim(dims.height, ToolbarDims::HEIGHT).ceil();
    let grip_len = dim(dims.grip_width, ToolbarDims::GRIP_W).ceil();
    // 每格等长（默认 30dp≈方形），状态切换不改变格尺寸，整条长度不抖。
    let cell_len = dim(dims.button_width, ToolbarDims::BUTTON_W);
    let layout = bar_layout(vertical, thickness, grip_len, cell_len, n);
    let w = layout.w.ceil() as u32;
    let h = layout.h.ceil() as u32;
    // 整条圆角：主题 border.radius 优先，未配则见 `default_border_radius`；配 0 即直角。
    let radius = dims
        .border_radius
        .map(|d| d.resolve(scale, 0.0))
        .unwrap_or_else(|| default_border_radius(w as f32, h as f32)) as u32;
    // 细边框线宽：主题 border.width 优先，未配落 1dp（原字面量）。
    let border_w = dims
        .border_width
        .map(|d| d.resolve(scale, 0.0))
        .unwrap_or(1.0 * scale)
        .max(1.0);
    let inset = dim(dims.button_padding, ToolbarDims::BUTTON_PAD) as u32;
    // 高亮格圆角：主题 button_radius 优先，否则 = 高亮块内短边×0.3。
    // 内缩算法须与 `paint_toolbar` 里真正的高亮矩形一致（长轴缩 inset/2、厚度方向缩 inset，纵条转置）。
    let (rw, rh) = match layout.cells.first() {
        Some(r) => (r.w as u32, r.h as u32),
        None => (w, h),
    };
    let (hw, hh) = if vertical {
        (rw.saturating_sub(inset * 2), rh.saturating_sub(inset))
    } else {
        (rw.saturating_sub(inset), rh.saturating_sub(inset * 2))
    };
    let button_radius = dims.button_radius.map_or_else(
        || (hw.min(hh) as f32 * 0.3) as u32,
        |d| (d.resolve(scale, 0.0)) as u32,
    );
    ToolbarFrame {
        layout,
        w,
        h,
        scale,
        radius,
        border_w,
        inset,
        button_radius,
    }
}

/// 工具栏格内字族：实时窗口与预览共用同一份（工具栏不随用户字体配置走，见 `Toolbar::new`）。
pub(crate) const FONT_FAMILY: &str = "Microsoft YaHei UI";

impl Toolbar {
    /// 工具栏格内字号（逻辑像素，随 DPI 缩放）。预览用同一份（见 `candidate_window::preview`）。
    pub(crate) const FONT_PX: f32 = 15.0;

    pub fn new(events: Sender<UiEvent>) -> Result<Self, String> {
        let scale = Self::dpi_scale();
        let window = LayeredWindow::create(None, 160, 40, "WindInputToolbar")?;
        let renderer = TextRenderer::new(FONT_FAMILY, Self::FONT_PX * scale)?;
        let hwnd = window.hwnd();
        let mouse = Rc::new(RefCell::new(ToolbarMouse {
            hits: Vec::new(),
            events,
            hwnd,
            pos: None,
            anchor_br: None,
            dragging: false,
            press_cell: false,
            anchor: (0, 0),
            origin: (0, 0),
            hover_idx: -1,
            dirty: false,
            cursor_inside: false,
            size: (0, 0),
            vertical: false,
        }));
        window.register_mouse(mouse.clone());
        Ok(Self {
            window,
            renderer,
            scale,
            visible: false,
            mouse,
            skin: ToolbarSkin::default(),
            dims: ToolbarDims::default(),
            last_state: None,
            rendered_hover: -1,
            auto_hide: AutoHide::new(),
            vertical: false,
            layout: wind_ui_types::DEFAULT_TOOLBAR_ITEMS.to_vec(),
            pending_corner: None,
            drag_rect: Rect::default(),
        })
    }

    /// 应用主题（工具栏各色，跟随语义）。
    pub fn set_theme(&mut self, theme: &wind_theme::Resolved) {
        // 折算在 [`ToolbarSkin::from_theme`]，预览共用。
        self.skin = ToolbarSkin::from_theme(theme);
        self.dims = ToolbarDims::from_theme(theme);
    }

    /// 配置纵向排列（`ui.toolbar.vertical`，经 SetToolbarVertical 下发）。可见时立即用缓存状态重绘。
    /// 重绘须受 `visible` 门控：`render` 末尾无条件 `show`，对隐藏中的工具栏调用会绕过 `toolbar_gate` 的显示迟滞。
    /// 隐藏期间不必出图，重新显示都经 `render` 重算尺寸。
    pub fn set_vertical(&mut self, vertical: bool) {
        if self.vertical == vertical {
            return;
        }
        self.vertical = vertical;
        if self.visible {
            self.repaint();
        }
    }

    /// 配置自动隐藏（启动/配置重载时经 SetToolbarAutoHide 下发）。
    /// 淡出中关闭开关 → 恢复不透明；开启且当前可见 → 立即起表。
    pub fn set_auto_hide(&mut self, enabled: bool, delay_ms: u64, hover_reveal: bool) {
        // 先配悬停唤回：关掉它时若正处于假隐藏，要先亮回来（下面 enabled 分支会重新计时）。
        let unghosted = self.auto_hide.configure_hover_reveal(hover_reveal);
        if (self.auto_hide.configure(enabled, delay_ms) || unghosted)
            && let Err(e) = self.window.update_with_alpha(255)
        {
            tracing::warn!("Toolbar restore alpha: {}", e);
        }
        if enabled && self.visible {
            // 淡出中重新配置：先恢复不透明再重新计时（configure(true) 不返回 was_fading）。
            if let Err(e) = self.window.update_with_alpha(255) {
                tracing::warn!("Toolbar restore alpha: {}", e);
            }
            self.auto_hide.on_shown(std::time::Instant::now());
        }
    }

    /// 设置工具栏位置（启动恢复持久化位置 / 运行期跟随焦点换屏）。
    /// 只登记原始坐标、不在这里钳制：此刻尺寸不可信（启动时是占位 160×40，跨 DPI 换屏时还是旧屏尺寸），
    /// 而钳制有损不可逆，错尺寸钳出的坐标写回后就回不来了。落点与钳制统一由 `render` 做，数值见 `sys::clamp_rect_tests`。
    /// `repaint` 受 `visible` 门控，理由同 `set_vertical`。
    pub fn set_anchor(&mut self, right: i32, bottom: i32) {
        // 显式位置优先于待落的角落请求，否则 render 会拿 pending 覆盖掉这次设定。
        self.pending_corner = None;
        self.mouse.borrow_mut().anchor_br = Some((right, bottom));
        if self.visible {
            self.repaint();
        }
    }

    /// 移到指定显示器工作区的右下角（焦点切到一块从未拖过工具栏的屏时用）。
    /// 协调器只传工作区边界，落点由这边算：要减去的尺寸只有 UI 侧知道。
    /// 无论可见与否都只登记意图、由 `render` 算：目标屏 DPI 可能不同，只有 `ensure_scale` 之后尺寸才对。
    /// 可见时自己 `repaint`，不依赖协调器紧跟一条 `UpdateToolbar`；`visible` 门控同 `set_vertical`。
    pub fn set_corner(&mut self, work_right: i32, work_bottom: i32) {
        self.pending_corner = Some((work_right, work_bottom));
        if self.visible {
            self.repaint();
        }
    }

    /// 配置显示哪些格、按什么顺序（`ui.toolbar.items`，经 SetToolbarLayout 下发）。
    /// 可见时立即重绘，`visible` 门控同 `set_vertical`。
    pub fn set_layout(&mut self, items: Vec<ToolbarItem>) {
        if self.layout == items {
            return;
        }
        self.layout = items;
        if self.visible {
            self.repaint();
        }
    }

    /// 根据配置的项序列 + 当前状态构建单元格（拖动条之后按 `self.layout` 顺序展开）。
    /// 展开逻辑在自由函数 [`expand_cells`]，便于不构造窗口就能测。
    fn cells(&self, state: &ToolbarState) -> Vec<Cell> {
        expand_cells(&self.layout, state)
    }
}

/// 把配置的项序列 + 当前状态展开成单元格序列；展开为空时回落全集。
/// 协调器只保证项序列非空，保证不了展开后还剩几格，闸门装在产出最终结果的这一环。
/// 当前内置项都无条件产格，理论上到不了空；兜底留着防日后给某项加回运行时条件。
/// `pub(crate)`：设置端工具栏预览（`candidate_window::preview`）用同一份展开逻辑。
pub(crate) fn expand_cells(layout: &[ToolbarItem], state: &ToolbarState) -> Vec<Cell> {
    let cells = expand_cells_raw(layout, state);
    if !cells.is_empty() {
        return cells;
    }
    // 回落仍走 `expand_cells_raw`，状态相关的取值只有一份实现。
    // 全集内置项都无条件产格所以不会再空；若日后给内置项加运行时条件，这里要跟着改。
    expand_cells_raw(&wind_ui_types::DEFAULT_TOOLBAR_ITEMS, state)
}

/// [`expand_cells`] 的无兜底内核，单独一层让兜底本身可测。
fn expand_cells_raw(layout: &[ToolbarItem], state: &ToolbarState) -> Vec<Cell> {
    {
        // 有效中文：中文模式、大写锁定未开、且不在密码框强制英文。纯呈现判断，输入闸在 coordinator 的
        // password_suppress 分支，别把这里的结论回灌给状态。
        let effective_chinese = state.chinese_mode && !state.caps_lock && !state.input_blocked;
        // 显示标签完全由协调器预计算（`[ui.labels]` 可配，本 crate 读不到配置），这里不做任何覆盖。
        let mode_text: &str = &state.icon_label;

        let mut cells = Vec::with_capacity(layout.len());
        for item in layout {
            match item {
                ToolbarItem::Mode => cells.push(Cell {
                    text: mode_text.to_string(),
                    highlight: effective_chinese,
                    dim: false,
                    action: ToolbarAction::ToggleMode,
                }),
                // 标点格：文本留空，渲染时按全/半角矢量绘制句号+逗号（不依赖字体字形定位）。
                ToolbarItem::Punct => cells.push(Cell {
                    text: String::new(),
                    highlight: false,
                    dim: false,
                    action: ToolbarAction::TogglePunct,
                }),
                // 全/半角格：文本留空，渲染时按状态画月亮 SVG（满月=全角 / 弯月=半角）。
                ToolbarItem::FullWidth => cells.push(Cell {
                    text: String::new(),
                    highlight: false,
                    dim: false,
                    action: ToolbarAction::ToggleWidth,
                }),
                // 简繁格：显隐只由 layout 决定，与 `state.s2t_enabled` 无关。这一格是简入繁出唯一的鼠标入口，
                // 跟着运行时状态隐藏会自锁（关着就再也开不回来）。状态只由字与高亮表达，不淡显。
                ToolbarItem::S2t => cells.push(Cell {
                    text: if state.s2t_enabled { "繁" } else { "简" }.to_string(),
                    highlight: state.s2t_enabled,
                    dim: false,
                    action: ToolbarAction::ToggleS2t,
                }),
                // 繁简格：恒显「简」，开合只由高亮表达（见 `ToolbarItem::T2s`）。显隐同样只由 layout 决定，避免自锁。
                ToolbarItem::T2s => cells.push(Cell {
                    text: "简".to_string(),
                    highlight: state.t2s_enabled,
                    dim: false,
                    action: ToolbarAction::ToggleT2s,
                }),
                // 软键盘格：文本留空，渲染时画 SVG 图标；开着时高亮。
                ToolbarItem::SoftKeyboard => cells.push(Cell {
                    text: String::new(),
                    highlight: state.soft_keyboard_on,
                    dim: false,
                    action: ToolbarAction::ToggleSoftKeyboard,
                }),
                // 设置格：文本留空，渲染时画齿轮。隐藏它也不会锁死用户：右键任意位置同样弹菜单（见 WM_RBUTTONDOWN）。
                ToolbarItem::Settings => cells.push(Cell {
                    text: String::new(),
                    highlight: false,
                    dim: false,
                    action: ToolbarAction::OpenSettings,
                }),
                // 自定义按钮：label 已由协调器按显示宽度截好。无状态，不高亮不淡显。
                ToolbarItem::Custom { index, label, .. } => cells.push(Cell {
                    text: label.clone(),
                    highlight: false,
                    dim: false,
                    action: ToolbarAction::Custom(*index),
                }),
            }
        }

        cells
    }
}

/// 把整条工具栏画进 `buf`（尺寸 `w`×`h`，预乘 BGRA）。实时窗口（[`Toolbar::render`]）与设置端预览
/// （`candidate_window::preview`）共用的唯一绘制实现，别再抄第二份。
///
/// `state` 只提供像素状态（标点图、软键盘 tint 等）；排布由 `frame` 定好，格数须与 `cells` 一致。
/// 返回命中矩形 `[(动作, 矩形)]`（预览路径丢弃）。
// 参数都是同一帧的独立事实（缓冲区 / 排布 / 格子 / 状态 / 皮肤），强行聚成结构体只是搬家。
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_toolbar(
    buf: &mut [u8],
    w: u32,
    h: u32,
    frame: &ToolbarFrame,
    cells: &[Cell],
    vertical: bool,
    hover_idx: i32,
    skin: &ToolbarSkin,
    state: &ToolbarState,
    renderer: &TextRenderer,
) -> Vec<(ToolbarAction, Rect)> {
    debug_assert_eq!(frame.layout.cells.len(), cells.len());
    let s = frame.scale;
    // 清底：实时窗口的缓冲区是复用的，不清会留残影。
    let buf_size = (w as usize) * (h as usize) * 4;
    let clear_len = buf_size.min(buf.len());
    buf[..clear_len].fill(0);
    // 背景 + 同弧度细边框。
    let layout = &frame.layout;
    fill_rounded(buf, w, h, 0, 0, w, h, skin.bg, frame.radius);
    crate::view::fill_ring(
        buf,
        w,
        h,
        0.0,
        0.0,
        w as f32,
        h as f32,
        skin.sep,
        frame.radius as f32,
        frame.border_w,
    );
    // 拖动柄点阵
    draw_grip(buf, w, h, &layout.grip, vertical, skin.grip, s);

    // 英文模式下标点固定显示半角，无需看 chinese_punct。
    let effective_chinese = state.chinese_mode && !state.caps_lock;
    // 逐格绘制
    let font_h = renderer.measure_text("中").height;
    let mut hits: Vec<(ToolbarAction, Rect)> = Vec::with_capacity(cells.len());
    for (i, c) in cells.iter().enumerate() {
        let r = layout.cells[i];
        hits.push((c.action, r));
        // 分隔线只画在首格前与齿轮的边界。齿轮排在首位时两条线重合，改画下一格的起始边，
        // 让齿轮在两种排列下都自成一区；齿轮是唯一一格时不画。
        let is_settings = matches!(c.action, ToolbarAction::OpenSettings);
        let sep_at = if i == 0 && is_settings {
            layout
                .cells
                .get(1)
                .map(|n| if vertical { n.y } else { n.x })
        } else if i == 0 || is_settings {
            // 画在格的**起始边**上：横条取左缘 x、纵条取上缘 y。
            Some(if vertical { r.y } else { r.x })
        } else {
            None
        };
        if let Some(pos) = sep_at {
            draw_sep(buf, w, h, pos as u32, vertical, skin.sep, s);
        }
        // 激活格（中文模式）画主题色底 + 高亮文字；悬停格画极淡底。
        // hl_bg 成对配合 hl_fg（如 msime 白字蓝底），缺底色时白字在亮色工具栏上不可见。
        let cell_bg = if c.highlight {
            Some(skin.hl_bg)
        } else if (i as i32) == hover_idx {
            Some(skin.hover_bg)
        } else {
            None
        };
        if let Some(bgc) = cell_bg {
            let inset = frame.inset;
            // 长轴两端各缩 inset/2、厚度方向各缩 inset，纵条转置施加。
            let (hx, hy, hw, hh) = if vertical {
                (
                    r.x as u32 + inset,
                    r.y as u32 + inset / 2,
                    (r.w as u32).saturating_sub(inset * 2),
                    (r.h as u32).saturating_sub(inset),
                )
            } else {
                (
                    r.x as u32 + inset / 2,
                    r.y as u32 + inset,
                    (r.w as u32).saturating_sub(inset),
                    (r.h as u32).saturating_sub(inset * 2),
                )
            };
            let hr = frame.button_radius;
            fill_rounded(buf, w, h, hx, hy, hw, hh, bgc, hr);
        }
        if is_settings {
            let size = font_h * 0.80;
            let dx = r.x + (r.w - size) * 0.5;
            let dy = r.y + (r.h - size) * 0.5;
            crate::view::draw_svg_icon(buf, w, h, SETTING_SVG, dx, dy, size, skin.settings_icon);
        } else if matches!(
            c.action,
            ToolbarAction::TogglePunct
                | ToolbarAction::ToggleWidth
                | ToolbarAction::ToggleSoftKeyboard
        ) {
            // 标点 / 全半角 / 软键盘：SVG 图标按主题色 tint，居中于方格。英文模式下标点固定半角。
            let svg = match (
                c.action,
                effective_chinese && state.chinese_punct,
                state.full_width,
            ) {
                (ToolbarAction::TogglePunct, true, _) => PUNCT_FULL_SVG,
                (ToolbarAction::TogglePunct, false, _) => PUNCT_HALF_SVG,
                (ToolbarAction::ToggleSoftKeyboard, _, _) => SOFT_KEYBOARD_SVG,
                (ToolbarAction::ToggleWidth, _, true) => WIDTH_FULL_SVG,
                _ => WIDTH_HALF_SVG,
            };
            let size = font_h * 0.80;
            let dx = r.x + (r.w - size) * 0.5;
            let dy = r.y + (r.h - size) * 0.5;
            // 高亮格要用 hl_fg，常态 fg 会与主题色实底撞在一起。
            let tint = if c.highlight { skin.hl_fg } else { skin.fg };
            crate::view::draw_svg_icon(buf, w, h, svg, dx, dy, size, tint);
        } else {
            // 居中文字
            let m = renderer.measure_text(&c.text);
            let tx = r.x + (r.w - m.width) * 0.5;
            let ty = r.y + (r.h - font_h) * 0.5;
            let fg = if c.highlight {
                skin.hl_fg
            } else if c.dim {
                dim_color(skin.fg)
            } else {
                skin.fg
            };
            let _ = renderer.draw_text(buf, w, h, tx.max(r.x), ty.max(r.y), &c.text, fg);
        }
    }
    hits
}

impl Toolbar {
    /// DPI 动态化：按这一帧要落的那块屏取缩放，只需更新 scale 与字号。
    /// 取点与 `render` 算落点同出一个 [`Placement`]：有换屏请求时 `mouse.pos` 还在上一块屏，
    /// `set_anchor` 恢复记忆位置时也不更新 `mouse.pos`，用它会按旧屏 DPI 排版。
    fn ensure_scale(&mut self, at: Placement) {
        let (x, y) = at.probe_point();
        let sc = crate::dpi::scale_for_point(x, y);
        if (sc - self.scale).abs() > 0.01 {
            self.scale = sc;
            self.renderer.set_base_size(Self::FONT_PX * sc);
        }
    }

    /// 更新状态并重绘（首次会计算位置并显示）。缓存状态以便 hover 变化时本地重绘。
    pub fn update(&mut self, state: &ToolbarState) {
        self.last_state = Some(state.clone());
        let hover = self.rendered_hover;
        self.render(state, hover);
    }

    /// 用缓存状态原地重绘（主题切换后刷新外观，无需重新传状态）。
    pub fn repaint(&mut self) {
        if let Some(state) = self.last_state.clone() {
            let hover = self.rendered_hover;
            self.render(&state, hover);
        }
    }

    /// 实际渲染（hover_idx=当前悬停格下标，-1 无）。update 与 tick 均经此单点渲染。
    fn render(&mut self, state: &ToolbarState, hover_idx: i32) {
        // 缩放与落点取自同一个 `Placement`。这里只取快照、不消费 `pending_corner`，中途提前返回时请求留到下一帧。
        let at = self.placement();
        self.ensure_scale(at);
        let s = self.scale;
        let cells = self.cells(state);
        let frame = toolbar_frame(&self.dims, s, self.vertical, cells.len());
        let w = frame.w;
        let h = frame.h;
        self.window.resize(w, h);
        self.drag_rect = frame.layout.grip;
        // 拖动由 addon 本地做：拖动柄随帧下发。
        #[cfg(all(target_os = "linux", ext_presenter))]
        self.window.set_drag_regions(self.drag_regions());
        // 背景整幅由 paint_toolbar 重画（含透明底），无需先清。
        let hits = paint_toolbar(
            self.window.buffer_mut(),
            w,
            h,
            &frame,
            &cells,
            self.vertical,
            hover_idx,
            &self.skin,
            state,
            &self.renderer,
        );
        if let Err(e) = self.window.update() {
            tracing::warn!("Toolbar update failed: {}", e);
        }

        // 落点计算都收口在这里：刚按当前朝向/主题/DPI 排完版，`w`/`h` 此刻才是真值。最后钳进当前显示器工作区。
        let (px, py) = {
            let mut m = self.mouse.borrow_mut();
            m.hits = hits; // 同步命中矩形给鼠标处理器
            // 菜单锚点要用的尺寸/朝向，与命中矩形同刻更新。
            m.size = (w, h);
            m.vertical = self.vertical;
            // 优先级见 [`Placement`]。记忆锚点每帧按「右下角 - 当前尺寸」现算：横纵切换、增删格子只改尺寸，
            // 右下角不动；存左上角的话每次尺寸变化都会被顶出工作区再拉回。
            let raw = match at {
                Placement::PendingCorner {
                    work_right,
                    work_bottom,
                } => Self::corner_in_work_area(work_right, work_bottom, w, h),
                Placement::Anchor { right, bottom } => {
                    Self::origin_from_anchor(right, bottom, w, h)
                }
                Placement::LastPos { x, y } => (x, y),
                Placement::PrimaryCorner => Self::corner_position(w, h),
            };
            // 换屏请求用掉之后才清。`at` 不是 `PendingCorner` 说明它本来就是 `None`，无条件清空等价于 `.take()`。
            self.pending_corner = None;
            let clamped = clamp_to_work_area(raw.0, raw.1, w, h);
            m.pos = Some(clamped);
            clamped
        };
        self.window.show(px, py);
        self.visible = true;
        self.rendered_hover = hover_idx;
        // render 是所有显示路径的单点，在此重置自动隐藏计时；window.update() 以 alpha=255 提交，天然恢复不透明。
        self.auto_hide.on_shown(std::time::Instant::now());
    }

    /// 下一次需要 [`Self::tick`] 的时刻；`None` = 无需为工具栏安排唤醒。
    /// 只转发自动隐藏的计时：悬停重绘由 `WM_MOUSEMOVE` / `WM_MOUSELEAVE` 唤醒消息循环，同一轮 `tick` 即可消费。
    pub fn next_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        if !self.visible {
            return None;
        }
        self.auto_hide.next_deadline(now)
    }

    /// UI 循环每轮调用：消费悬停脏标记（仅悬停格变化时本地重绘），并推进自动隐藏。
    pub fn tick(&mut self) {
        if !self.visible {
            return;
        }
        let (dirty, hov) = {
            let m = self.mouse.borrow();
            (m.dirty, m.hover_idx)
        };
        if dirty {
            self.mouse.borrow_mut().dirty = false;
            if hov != self.rendered_hover {
                if let Some(state) = self.last_state.clone() {
                    self.render(&state, hov);
                } else {
                    self.rendered_hover = hov;
                }
            }
        }
        // 自动隐藏推进。未启用时 is_active()=false 直接跳过，不取时间（开关关闭时零开销）。
        if self.auto_hide.is_active() {
            let (inside, dragging) = {
                let m = self.mouse.borrow();
                (m.cursor_inside, m.dragging)
            };
            let now = std::time::Instant::now();
            match self.auto_hide.tick_at(now, inside, dragging) {
                AutoHideAction::None => {}
                AutoHideAction::Fade(a) => {
                    if let Err(e) = self.window.update_with_alpha(a) {
                        tracing::warn!("Toolbar fade: {}", e);
                    }
                }
                AutoHideAction::Restore => {
                    if let Err(e) = self.window.update_with_alpha(255) {
                        tracing::warn!("Toolbar fade restore: {}", e);
                    }
                }
                AutoHideAction::Hide => self.hide(),
                // 假隐藏：窗口留在原位、`visible` 仍为 true，只把整窗 alpha 压到几乎不可见；光标移入时由 Restore 亮回来。
                AutoHideAction::Ghost => {
                    if let Err(e) = self.window.update_with_alpha(crate::auto_hide::GHOST_ALPHA) {
                        tracing::warn!("Toolbar ghost: {}", e);
                    }
                }
            }
        }
    }

    // 所有显示路径收口于 `render`，不提供绕开它的 `show()`：那样既不消费 `pending_corner`，也不按当前朝向/DPI
    // 重排尺寸。要「不改状态地重新显形」用 `repaint()`（受 `visible` 门控）。

    /// 按下即拖动整条工具栏的区域（位图内 `x, y, w, h`）：拖动柄（格子铺满了其余部分）。首帧之前为空。
    pub fn drag_regions(&self) -> Vec<[i32; 4]> {
        grip_region(&self.drag_rect).into_iter().collect()
    }

    /// 外部宿主本地拖完：等同 Windows 拖动结束时 `WM_LBUTTONUP` 那一步——记下落点与右下角锚点并上报落盘。
    /// 宿主只报左上角，换算右下角要用只有这里知道的尺寸。
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub fn on_host_moved(&mut self, x: i32, y: i32) {
        let mut m = self.mouse.borrow_mut();
        let (w, h) = m.size;
        let (right, bottom) = (x + w as i32, y + h as i32);
        m.pos = Some((x, y));
        m.anchor_br = Some((right, bottom));
        let _ = m.events.send(UiEvent::ToolbarMoved { right, bottom });
    }

    /// 用户此刻看得见它：可见且不在自动隐藏的假隐藏里（假隐藏时 `visible` 仍为 true）。
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub fn is_seen(&self) -> bool {
        self.visible && !self.auto_hide.is_ghosted()
    }

    /// 当前是否可见（`render` 置 true，`hide` 置 false）。
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 将当前渲染帧保存为 PNG 文件（截图用）。
    pub fn capture_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        self.window.capture_to_file(path)
    }

    /// 返回工具栏窗口句柄（截图用）。
    #[cfg(windows)]
    pub fn hwnd(&self) -> windows::Win32::Foundation::HWND {
        self.window.hwnd()
    }

    pub fn hide(&mut self) {
        self.window.hide();
        self.visible = false;
        self.rendered_hover = -1; // 重新显示时按光标位置重算悬停
        self.auto_hide.on_hidden();
    }

    /// 这一帧工具栏该落在哪，见 [`Placement`]。纯读，不消费 `pending_corner`。
    fn placement(&self) -> Placement {
        let m = self.mouse.borrow();
        Placement::pick(self.pending_corner, m.anchor_br, m.pos)
    }

    /// 记忆锚点（窗口右下角）→ 本帧落点（左上角）。必须每帧用当前 `w`/`h` 现算，不能存结果复用。
    fn origin_from_anchor(right: i32, bottom: i32, w: u32, h: u32) -> (i32, i32) {
        (right - w as i32, bottom - h as i32)
    }

    /// 给定工作区右/下边界，算工具栏右下角落点（右/下各留 12px 边距）。
    /// 纯几何，可用于任意显示器；不在这里钳到 0（副屏边界可为负），越界回收交给 `clamp_to_work_area`。
    fn corner_in_work_area(work_right: i32, work_bottom: i32, w: u32, h: u32) -> (i32, i32) {
        const MARGIN: i32 = 12;
        (
            work_right - w as i32 - MARGIN,
            work_bottom - h as i32 - MARGIN,
        )
    }

    /// 主显示器工作区右下角位置（避开任务栏）。
    /// 仅在协调器 `focus_monitor()` 失败时才轮得到；`SPI_GETWORKAREA` 恒取主屏，别拿它做多屏定位。
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn corner_position(w: u32, h: u32) -> (i32, i32) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::RECT;
            use windows::Win32::UI::WindowsAndMessaging::{
                SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
            };
            unsafe {
                let mut rect = RECT::default();
                let ok = SystemParametersInfoW(
                    SPI_GETWORKAREA,
                    0,
                    Some(&mut rect as *mut _ as *mut std::ffi::c_void),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                );
                if ok.is_ok() && rect.right > rect.left {
                    let (x, y) = Self::corner_in_work_area(rect.right, rect.bottom, w, h);
                    return (x.max(0), y.max(0));
                }
            }
        }
        (200, 200)
    }

    fn dpi_scale() -> f32 {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::Graphics::Gdi::*;
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
}

/// 工具栏鼠标处理器：点击单元格切换；非单元格区（拖动柄）按下拖动整条工具栏。
pub struct ToolbarMouse {
    hits: Vec<(ToolbarAction, Rect)>,
    events: Sender<UiEvent>,
    hwnd: HWND,
    /// 当前位置（屏幕坐标）；None = 尚未定位。
    /// 首次 `render` 之前可能是未钳制的原始值（`size` 同为 `(0,0)`）。现有读者都挂在鼠标消息上，隐藏窗口收不到，
    /// 故安全；新增不依赖鼠标消息的读路径前先确认这一点。
    pos: Option<(i32, i32)>,
    /// 记忆锚点：窗口右下角屏幕坐标；None = 该屏没记录过。
    /// 它是意图，`pos` 是本帧落点（意图减当前尺寸再钳进工作区）。`render` 的钳制结果只写回 `pos`、不回写这里，
    /// 否则临时接小屏的一次妥协会永久顶掉用户摆的位置。
    anchor_br: Option<(i32, i32)>,
    dragging: bool,
    /// 在格上按下、尚未移过拖动阈值：再移过阈值就转成拖动，原地抬起仍是点击。
    press_cell: bool,
    /// 拖动起点：光标屏幕坐标
    anchor: (i32, i32),
    /// 拖动起点：窗口屏幕坐标
    origin: (i32, i32),
    /// 当前悬停格下标（-1=无）；由 WM_MOUSEMOVE/WM_MOUSELEAVE 事件更新
    hover_idx: i32,
    /// 悬停态有变更、待 Toolbar::tick 重绘
    dirty: bool,
    /// 光标是否在工具栏窗口内（WM_MOUSEMOVE 置 true / WM_MOUSELEAVE 置 false）；
    /// 自动隐藏顺延判据——不能用 hover_idx（拖动柄区为 -1 但光标仍在窗内）。
    cursor_inside: bool,
    /// 最近一次渲染的窗口尺寸与朝向，由 `render` 与 `hits` 同处同步，菜单锚点据此计算（无系统调用）。
    size: (u32, u32),
    vertical: bool,
}

/// 这一帧工具栏落在哪，四级优先级只在 [`Placement::pick`] 定义一次。
/// [`Toolbar::ensure_scale`]（要一个屏内点查 DPI）与 [`Toolbar::render`]（要算左上角落点）都消费它；
/// 两者不能合成一个函数（落点要尺寸，尺寸要缩放，缩放要屏），但优先级只写一次，漏一级就是 match 不穷尽。
/// 比 `soft_keyboard::Placement` 多出第一级：协调器换屏时下发的右下角请求必须压过记忆锚点，
/// 否则跟随焦点换屏会被上一块屏的记忆位置顶回去。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// 待落的换屏请求：协调器给的**目标屏**工作区右下角。压过一切记忆位置。
    PendingCorner { work_right: i32, work_bottom: i32 },
    /// 用户摆过的位置，锚窗口右下角（跨重启记忆）。
    Anchor { right: i32, bottom: i32 },
    /// 上一帧的落点（左上角）。
    LastPos { x: i32, y: i32 },
    /// 什么记录都没有（首次启动）：主屏右下角。
    PrimaryCorner,
}

impl Placement {
    /// 四级优先级的唯一实现。拆成自由函数让测试打到真身（`Toolbar` 要真窗口才能构造）。
    fn pick(
        pending_corner: Option<(i32, i32)>,
        anchor_br: Option<(i32, i32)>,
        pos: Option<(i32, i32)>,
    ) -> Self {
        match (pending_corner, anchor_br, pos) {
            (Some((work_right, work_bottom)), _, _) => Placement::PendingCorner {
                work_right,
                work_bottom,
            },
            (None, Some((right, bottom)), _) => Placement::Anchor { right, bottom },
            (None, None, Some((x, y))) => Placement::LastPos { x, y },
            (None, None, None) => Placement::PrimaryCorner,
        }
    }

    /// 取一个落在目标屏内的点，供 DPI 查询。
    /// `right`/`bottom` 是排他边界（Win32 `RECT` 语义），须退 1px，否则贴边时 `MonitorFromPoint` 会查到相邻屏。
    fn probe_point(self) -> (i32, i32) {
        match self {
            Placement::PendingCorner {
                work_right,
                work_bottom,
            } => (work_right - 1, work_bottom - 1),
            Placement::Anchor { right, bottom } => (right - 1, bottom - 1),
            // 左上角是包含边界，本身就在窗口内，不必退。
            Placement::LastPos { x, y } => (x, y),
            // 兜底主屏，与 `corner_position` 的 `SPI_GETWORKAREA` 一致。
            Placement::PrimaryCorner => (0, 0),
        }
    }
}

impl ToolbarMouse {
    /// 工具栏当前占据的屏幕矩形 `(left, top, right, bottom)`。
    fn rect(&self) -> (i32, i32, i32, i32) {
        let (x, y) = self.pos.unwrap_or((0, 0));
        (x, y, x + self.size.0 as i32, y + self.size.1 as i32)
    }

    /// 主菜单锚点：横条向上弹（避免压住工具栏），纵条向侧面弹。
    fn menu_anchor(&self) -> MenuAnchor {
        let (l, t, r, b) = self.rect();
        if self.vertical {
            MenuAnchor::beside_rect(l, t, r, b)
        } else {
            MenuAnchor::above_rect(l, t, b)
        }
    }

    fn cell_at(&self, x: f32, y: f32) -> Option<ToolbarAction> {
        self.hits
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(a, _)| *a)
    }

    /// 右键某一格时的菜单锚点：展开方向同 [`Self::menu_anchor`]，但沿条身那一维贴到那一格。
    /// 沿条身是哪个字段取决于 `place_menu` 读哪个：横条读 `x`；纵条读 `bottom`（向上展开）、
    /// 上方装不下时回退读 `y`，故两个都要传格的。命中不到格（拖动柄区）时退回整条锚点。
    fn cell_menu_anchor(&self, x: f32, y: f32) -> MenuAnchor {
        let (l, t, r, b) = self.rect();
        let Some((_, cell)) = self.hits.iter().find(|(_, cr)| cr.contains(x, y)) else {
            return self.menu_anchor();
        };
        if self.vertical {
            MenuAnchor::beside_rect(l, t + cell.y as i32, r, t + (cell.y + cell.h) as i32)
        } else {
            MenuAnchor::above_rect(l + cell.x as i32, t, b)
        }
    }

    /// 命中格下标（-1=无）。用于悬停高亮。
    fn hover_at(&self, x: f32, y: f32) -> i32 {
        self.hits
            .iter()
            .position(|(_, r)| r.contains(x, y))
            .map(|i| i as i32)
            .unwrap_or(-1)
    }

    /// 注册一次性 WM_MOUSELEAVE 通知（光标移出窗口时收到），以便清除悬停。
    fn arm_leave(&self) {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::UI::Input::KeyboardAndMouse::{
                TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
            };
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut t);
        }
    }
}

/// 在格上按下也能拖动（移过阈值才算拖，原地抬起仍是点击）。仅 Windows：Linux 宿主由
/// addon 按拖动柄区域本地拖，这里再起一次拖动会和它打架。
const CELL_DRAG: bool = cfg!(windows);

/// `WM_MOUSEMOVE` 的 wParam：左键按住。
const MK_LBUTTON: usize = 0x0001;

/// 系统拖动阈值（全宽，跟随用户设置）。
fn drag_threshold() -> (i32, i32) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDRAG, SM_CYDRAG};
        (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG))
    }
    #[cfg(not(windows))]
    {
        (4, 4)
    }
}

/// 光标从按下点移出以按下点为中心、`thr` 为全宽的矩形即算开始拖动（同 `DragDetect`）。
fn drag_started(anchor: (i32, i32), p: (i32, i32), thr: (i32, i32)) -> bool {
    (p.0 - anchor.0).abs() > thr.0 / 2 || (p.1 - anchor.1).abs() > thr.1 / 2
}

impl ToolbarMouse {
    /// 进入拖动态：拖动中不显示悬停。
    fn start_drag(&mut self) {
        self.dragging = true;
        if self.hover_idx != -1 {
            self.hover_idx = -1;
            self.dirty = true;
        }
    }
}

impl WindowMouse for ToolbarMouse {
    fn on_message(
        &mut self,
        _hwnd: HWND,
        msg: u32,
        _wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        let v = lparam.0 as u32;
        let cx = (v & 0xFFFF) as i16 as f32;
        let cy = ((v >> 16) & 0xFFFF) as i16 as f32;
        match msg {
            WM_LBUTTONDOWN => {
                let on_cell = self.cell_at(cx, cy).is_some();
                if !on_cell || CELL_DRAG {
                    let mut p = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                    }
                    self.anchor = (p.x, p.y);
                    self.origin = self.pos.unwrap_or((p.x, p.y));
                    if on_cell {
                        // 格上按下：先不拖，移过阈值再转拖动（见 WM_MOUSEMOVE）。
                        self.press_cell = true;
                    } else {
                        // 拖动柄区 → 直接开始拖动
                        self.start_drag();
                    }
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.cursor_inside = true;
                if self.press_cell {
                    if _wparam.0 & MK_LBUTTON == 0 {
                        // 抬起没送到（捕获被抢走）：放弃这次按下，别让悬停移动变成拖动。
                        self.press_cell = false;
                        unsafe {
                            let _ = ReleaseCapture();
                        }
                    } else {
                        let mut p = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut p);
                        }
                        if drag_started(self.anchor, (p.x, p.y), drag_threshold()) {
                            self.press_cell = false;
                            self.start_drag();
                        }
                    }
                }
                if self.dragging {
                    let mut p = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                    }
                    let nx = self.origin.0 + (p.x - self.anchor.0);
                    let ny = self.origin.1 + (p.y - self.anchor.1);
                    // 钳制到（最近显示器的）工作区，防止拖出桌面/拖入任务栏。
                    // 多显示器下 MonitorFromPoint(NEAREST) 会随光标过界切到目标显示器。
                    let (w, h) = unsafe {
                        let mut r = RECT::default();
                        if GetWindowRect(self.hwnd, &mut r).is_ok() {
                            ((r.right - r.left) as u32, (r.bottom - r.top) as u32)
                        } else {
                            (0, 0)
                        }
                    };
                    let (cx, cy) = clamp_to_work_area(nx, ny, w, h);
                    self.pos = Some((cx, cy));
                    unsafe {
                        let _ = SetWindowPos(
                            self.hwnd,
                            HWND_TOPMOST,
                            cx,
                            cy,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                    }
                } else {
                    // 非拖动：更新悬停格（变化置脏，由 Toolbar::tick 重绘）+ 注册移出通知。
                    let hov = self.hover_at(cx, cy);
                    if hov != self.hover_idx {
                        self.hover_idx = hov;
                        self.dirty = true;
                    }
                    self.arm_leave();
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                // 光标移出工具栏 → 清除悬停高亮。
                self.cursor_inside = false;
                if self.hover_idx != -1 {
                    self.hover_idx = -1;
                    self.dirty = true;
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if self.press_cell {
                    self.press_cell = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                if self.dragging {
                    self.dragging = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    // 取实际窗口矩形回报右下角锚点供持久化（见 `ToolbarMouse::anchor_br`）。
                    let mut r = RECT::default();
                    let (l, t, right, bottom) = unsafe {
                        if GetWindowRect(self.hwnd, &mut r).is_ok() {
                            (r.left, r.top, r.right, r.bottom)
                        } else {
                            // 失败兜底：用上一帧落点 + 尺寸补出四边，免得把 (0,0) 存成用户位置。
                            let (x, y) = self.pos.unwrap_or((0, 0));
                            (x, y, x + self.size.0 as i32, y + self.size.1 as i32)
                        }
                    };
                    self.pos = Some((l, t));
                    self.anchor_br = Some((right, bottom));
                    let _ = self.events.send(UiEvent::ToolbarMoved { right, bottom });
                } else if let Some(action) = self.cell_at(cx, cy) {
                    if matches!(action, ToolbarAction::OpenSettings) {
                        // 设置键 = 弹出功能主菜单（贴着工具栏，避免遮挡它）。
                        let _ = self
                            .events
                            .send(UiEvent::RequestMainMenu(self.menu_anchor()));
                    } else {
                        // 其它单元格：按下未拖动 → 抬起时触发切换
                        let _ = self.events.send(UiEvent::Toolbar(action));
                    }
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN => {
                // 右键 → 该格的快捷菜单。认不认得这一格由协调器判断，认不得就回落主菜单；这里只报点在哪一格。
                let _ = self.events.send(UiEvent::RequestToolbarMenu {
                    action: self.cell_at(cx, cy),
                    anchor: self.cell_menu_anchor(cx, cy),
                });
                Some(LRESULT(0))
            }
            // 中键点中英格 = 直接切到下一个方案。只认这一格：中键没有视觉提示，绑在管方案的格上才猜得到，
            // 散给每一格误触代价不一。落在 UP 与左键一致。
            WM_MBUTTONUP => {
                // 拖动中不触发格动作，与左键 UP 同一条守卫。
                if self.dragging {
                    return Some(LRESULT(0));
                }
                if let Some(ToolbarAction::ToggleMode) = self.cell_at(cx, cy) {
                    // 复用既有的 SwitchEngine（协调器映射到 `cycle_schema`）。
                    let _ = self
                        .events
                        .send(UiEvent::Toolbar(ToolbarAction::SwitchEngine));
                }
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                unsafe {
                    let cur = if self.dragging {
                        IDC_SIZEALL
                    } else {
                        IDC_ARROW
                    };
                    if let Ok(c) = LoadCursorW(None, cur) {
                        SetCursor(c);
                    }
                }
                Some(LRESULT(1))
            }
            _ => None,
        }
    }
}

/// 圆角填充：复用 view 的抗锯齿 + 预乘混合实现，保持各窗口圆角一致。
/// 参数形状与 `view::fill_rounded` 一一对应（转发用），故同样豁免参数数量检查。
#[allow(clippy::too_many_arguments)]
fn fill_rounded(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: [u8; 4],
    radius: u32,
) {
    crate::view::fill_rounded(
        buf,
        buf_w,
        buf_h,
        x as f32,
        y as f32,
        w as f32,
        h as f32,
        color,
        radius as f32,
    );
}

/// 次要状态文字淡显：alpha 降至 ~65%。
fn dim_color(c: [u8; 4]) -> [u8; 4] {
    [c[0], c[1], c[2], (c[3] as f32 * 0.65) as u8]
}

/// 格间分隔线：横条画竖线（`pos` 是 x），纵条画横线（`pos` 是 y）；线两端各内缩 6 逻辑像素。
fn draw_sep(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    pos: u32,
    vertical: bool,
    color: [u8; 4],
    scale: f32,
) {
    let inset = (6.0 * scale) as u32;
    // across = 线要横跨的那条边（横条跨条高、纵条跨条宽）。
    let across = if vertical { buf_h } else { buf_w };
    let span = if vertical { buf_w } else { buf_h };
    let a0 = inset;
    let a1 = span.saturating_sub(inset);
    if pos >= across || a1 <= a0 {
        return;
    }
    let (x, y, w, h) = if vertical {
        (a0 as f32, pos as f32, (a1 - a0) as f32, 1.0)
    } else {
        (pos as f32, a0 as f32, 1.0, (a1 - a0) as f32)
    };
    // 1px 线 = 直角矩形（tiny-skia），与其它形状统一
    crate::view::fill_rounded(buf, buf_w, buf_h, x, y, w, h, color, 0.0);
}

/// 拖动柄点阵，居中于 `grip` 区。横条 2 列×3 行、纵条 3 列×2 行，长边始终垂直于条身。
fn draw_grip(
    buf: &mut [u8],
    buf_w: u32,
    buf_h: u32,
    grip: &Rect,
    vertical: bool,
    color: [u8; 4],
    scale: f32,
) {
    let dot = (2.0 * scale).max(1.0);
    let gap = 4.0 * scale;
    let cx = grip.x + grip.w / 2.0;
    let cy = grip.y + grip.h / 2.0;
    let (cols, rows) = if vertical { (3, 2) } else { (2, 3) };
    let x0 = cx - (cols - 1) as f32 * gap / 2.0;
    let y0 = cy - (rows - 1) as f32 * gap / 2.0;
    for row in 0..rows {
        for col in 0..cols {
            let x = x0 + col as f32 * gap;
            let y = y0 + row as f32 * gap;
            fill_dot(buf, buf_w, buf_h, x, y, dot / 2.0, color);
        }
    }
}

fn fill_dot(buf: &mut [u8], buf_w: u32, buf_h: u32, cx: f32, cy: f32, r: f32, color: [u8; 4]) {
    // 抗锯齿圆点（tiny-skia），与其它形状统一
    crate::view::fill_circle(buf, buf_w, buf_h, cx, cy, r, color);
}

#[cfg(test)]
#[path = "toolbar_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "toolbar_cell_anchor_tests.rs"]
mod cell_anchor_tests;
