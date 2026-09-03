//! 工具栏窗口：常驻状态指示器（中英 / 方案 / 标点 / 全半角）。
//!
//! 与 Go 版本 `wind_input/internal/ui/toolbar_window.go` 对齐（简化版）。
//! 横向圆角小条，每格一个状态；中文模式格高亮。固定显示于工作区右下角。
//! 点击切换暂未实现（后续 UI 统一优化阶段补齐拖动 + 命中），当前为展示用。

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
struct Cell {
    text: String,
    highlight: bool,
    dim: bool,
    action: ToolbarAction,
}

// 标点状态图标：外部 SVG 文件（res/icons/）编译期嵌入（include_str!）。
// SVG 仅作 alpha 蒙版（形状黑色填充即可），颜色由工具栏按主题 tint；位置精确、不受字体基线影响。
// 要调整符号样式，直接编辑这两个 svg 文件即可（无需改 Rust 代码）。
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
/// 软键盘图标。**只有一张**：开关态由格底高亮表达（同 `Mode` 格的做法），而不像
/// 标点 / 全半角那样两态两张图——那两格切换的是**输出形态**，两个状态各有自己的样子；
/// 软键盘格切换的是**面板开合**，图标始终代表同一个东西。
const SOFT_KEYBOARD_SVG: &str = include_str!("../res/icons/soft_keyboard.svg");

/// 工具栏窗口
pub struct Toolbar {
    window: LayeredWindow,
    renderer: TextRenderer,
    scale: f32,
    visible: bool,
    /// 鼠标处理器（与 window 共享，wnd_proc 经注册表回调）；位置存于其中以便拖动同步
    mouse: Rc<RefCell<ToolbarMouse>>,
    // 主题色（默认浅色，set_theme 加载主题后覆盖）
    bg: [u8; 4],
    fg: [u8; 4],
    hl_bg: [u8; 4],
    hl_fg: [u8; 4],
    sep: [u8; 4],
    grip: [u8; 4],
    settings_icon: [u8; 4],
    hover_bg: [u8; 4],
    /// 最近一次状态（供 hover 变化时本地重绘，无需协调器往返）
    last_state: Option<ToolbarState>,
    /// 已渲染的悬停格下标（-1=无）；tick 检测光标位置变化后据此决定是否重绘
    rendered_hover: i32,
    /// 自动隐藏状态机（默认关闭；SetToolbarAutoHide 命令配置）
    auto_hide: AutoHide,
    // 工具栏几何（主题 [toolbar] 描述；None→render 内置默认，见常量 HEIGHT/GRIP_W 等）。
    tb_height: Option<Dim>,
    tb_grip_width: Option<Dim>,
    tb_button_width: Option<Dim>,
    tb_button_padding: Option<Dim>,
    tb_button_radius: Option<Dim>,
    /// 整条外框圆角 / 线宽（[toolbar] border.radius / .width）。None→内置派生值。
    tb_border_radius: Option<Dim>,
    tb_border_width: Option<Dim>,
    /// 纵向排列（ui.toolbar.vertical，非主题——见 `bar_layout`）。
    vertical: bool,
    /// 显示哪些格、按什么顺序（`ui.toolbar.items`，经 `SetToolbarLayout` 下发）。
    ///
    /// 初值取全集而非空：`SetToolbarLayout` 在 `apply_ui_config` 里下发，而工具栏可能在
    /// 那之前就被 `UpdateToolbar` 拉起来渲染一帧——初值为空的话那一帧是条只剩拖动柄的
    /// 空条，随后才"长出"各格，视觉上是一次凭空抖动。
    layout: Vec<ToolbarItem>,
    /// 待落的「某显示器右下角」请求：`(工作区右边界, 下边界)`，由 `render` 消费。
    ///
    /// 存边界而不是直接算坐标，是因为落点要减去工具栏自身尺寸，而**尺寸在 `render` 之前
    /// 不可信**——窗口以 `create(160, 40)` 的占位尺寸起步，`set_vertical` 在隐藏期间又
    /// 不重排（不出图），于是首次渲染前 `window.size()` 既不是横条真值也不是纵条真值。
    ///
    /// **跨 `hide` 存活**：隐藏不该丢弃「下次该落到哪块屏」这个意图，重新显示时由
    /// `render` 消费。清除它的只有 `render` 的 `take()` 与 `set_pos`（后到的显式位置覆盖）。
    /// 边界过时不必担心：这对数值正是协调器 `sync_toolbar_monitor` 的去重 key 本身，
    /// 边界一变 key 必变，下一次 sync 会重新下发覆盖。
    pending_corner: Option<(i32, i32)>,
}

/// 整条工具栏的几何：窗口尺寸 + 每格矩形（设备像素，相对窗口左上角）。
struct BarLayout {
    w: f32,
    h: f32,
    /// 拖动柄占据的区域（横条=左端竖条，纵条=顶端横条）。
    grip: Rect,
    cells: Vec<Rect>,
}

/// 按朝向铺开整条工具栏。**纵向恒为横向的转置**：`thickness`（主题 `[toolbar] height`）
/// 在横条里是条高、在纵条里是条宽；`cell`（`button_width`）在横条里是格宽、纵条里是格高。
/// 于是同一套主题几何在两个朝向下都成立，不必为纵向另配一套尺寸。
///
/// 抽成纯函数是为了可单测：`render` 的其余部分要拿 DirectWrite 测文字、要提交 Layered
/// Window，在非 Windows 上是 mock/空实现，覆盖不到。
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

/// 整条外框圆角的兜底半径（主题 `[toolbar] border.radius` 未配时）：**短边**×0.30。
///
/// 必须取短边而非 `h`：`bar_layout` 把纵条画成横条的转置，`h` 在横条里是厚度、在纵条里
/// 却是整条长度。用 `h` 算出的纵条半径恒远超厚度，被 `push_round_rect` 的
/// `min(w*0.5)` 静默钳成满胶囊——错误被下游钳制吸收，只表现为「纵排圆角明显更大」。
/// 取短边后两个朝向同为厚度×0.30，横条行为与原来逐字节相同。
fn default_border_radius(w: f32, h: f32) -> f32 {
    w.min(h) * 0.30
}

impl Toolbar {
    // 几何默认值（逻辑像素，随 DPI 缩放）。主题 [toolbar] 未描述时的兜底；
    // 与 _base/theme.toml [toolbar] 保持一致，改这里也应同步 _base（反之亦然）。
    const HEIGHT: f32 = 30.0;
    const GRIP_W: f32 = 12.0;
    const BUTTON_W: f32 = 30.0; // 每格（按钮槽）宽度，字面值；配 button_width 覆盖
    const BUTTON_PAD: f32 = 4.0; // 激活/悬停格高亮内缩；配 button_padding 覆盖
    const FONT_PX: f32 = 15.0;

    // 默认浅色配色（主题加载后由 set_theme 覆盖，以下为无主题时的兜底值）
    const BG: [u8; 4] = [255, 255, 255, 245]; // 白色半透明底
    const FG: [u8; 4] = [72, 72, 78, 255]; // 正常文字深灰
    const HL_BG: [u8; 4] = [66, 133, 244, 255]; // 高亮蓝（中文模式 / 简繁启用）
    const HL_FG: [u8; 4] = [255, 255, 255, 255];
    const SEP: [u8; 4] = [214, 214, 220, 255]; // 浅灰分隔线
    const GRIP: [u8; 4] = [186, 186, 194, 255]; // 拖动点
    const SETTINGS_ICON: [u8; 4] = [140, 140, 148, 255]; // 设置图标（比普通文字更淡）
    const HOVER_BG: [u8; 4] = [0, 0, 0, 13]; // 鼠标悬停高亮（极淡，~5% 黑）

    pub fn new(events: Sender<UiEvent>) -> Result<Self, String> {
        let scale = Self::dpi_scale();
        let window = LayeredWindow::create(None, 160, 40, "WindInputToolbar")?;
        let renderer = TextRenderer::new("Microsoft YaHei UI", Self::FONT_PX * scale)?;
        let hwnd = window.hwnd();
        let mouse = Rc::new(RefCell::new(ToolbarMouse {
            hits: Vec::new(),
            events,
            hwnd,
            pos: None,
            dragging: false,
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
            bg: Self::BG,
            fg: Self::FG,
            hl_bg: Self::HL_BG,
            hl_fg: Self::HL_FG,
            sep: Self::SEP,
            grip: Self::GRIP,
            settings_icon: Self::SETTINGS_ICON,
            hover_bg: Self::HOVER_BG,
            last_state: None,
            rendered_hover: -1,
            auto_hide: AutoHide::new(),
            tb_height: None,
            tb_grip_width: None,
            tb_button_width: None,
            tb_button_padding: None,
            tb_button_radius: None,
            tb_border_radius: None,
            tb_border_width: None,
            vertical: false,
            layout: wind_ui_types::DEFAULT_TOOLBAR_ITEMS.to_vec(),
            pending_corner: None,
        })
    }

    /// 应用主题（工具栏各色，跟随语义）。
    pub fn set_theme(&mut self, theme: &wind_theme::Resolved) {
        // 背景色/边框色：[toolbar] 节点值优先（resolve 已合成 palette 默认），
        // 未配才落回 token —— 与其它窗口一致。
        self.bg = theme.color("toolbar_background", self.bg);
        self.fg = theme.color("toolbar_full_width_off_text", self.fg);
        self.hl_bg = theme.color("toolbar_mode_chinese_bg", self.hl_bg);
        self.hl_fg = theme.color("toolbar_mode_text", self.hl_fg);
        self.sep = theme.color("toolbar_border", self.sep);
        self.grip = theme.color("toolbar_grip", self.grip);
        self.settings_icon = theme.color("toolbar_settings_icon", self.settings_icon);
        self.hover_bg = theme.color("toolbar_hover", self.hover_bg);
        // 几何：从解析后的 [toolbar] 读取（None→render 用内置默认，行为不变）。
        let v = &theme.views;
        self.tb_height = v.toolbar_height;
        self.tb_grip_width = v.toolbar_grip_width;
        self.tb_button_width = v.toolbar_button_width;
        self.tb_button_padding = v.toolbar_button_padding;
        self.tb_button_radius = v.toolbar_button_radius;
        self.tb_border_radius = v.toolbar_border_radius;
        self.tb_border_width = v.toolbar_border_width;
        // [toolbar] 节点色覆盖上面的 token 兜底。
        if let Some(c) = v.toolbar_bg_color {
            self.bg = c;
        }
        if let Some(c) = v.toolbar_border_color {
            self.sep = c;
        }
    }

    /// 配置纵向排列（`ui.toolbar.vertical`，经 SetToolbarVertical 下发）。
    ///
    /// 换向会改窗口尺寸，故可见时立即用缓存状态重绘——否则要等下一次状态推送（切中英等）
    /// 才换向，设置页里改完看着像没生效。
    ///
    /// ⚠️ 重绘必须受 `visible` 门控：`repaint`→`render` 末尾无条件 `show`，对隐藏中的
    /// 工具栏调用会把它显形，绕过 `toolbar_gate` 的显示迟滞（同 `SetTheme` 分支的约束）。
    /// 隐藏期间换向不必出图——朝向已存好，而所有重新显示的路径都经 `update`→`render`
    /// 重算尺寸，不会留下旧朝向的残帧。
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
    pub fn set_auto_hide(&mut self, enabled: bool, delay_ms: u64) {
        if self.auto_hide.configure(enabled, delay_ms)
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
    ///
    /// **只登记原始坐标，一律不在这里钳制**——钳制要拿工具栏尺寸比对工作区边界，而这一刻
    /// 尺寸不可信，且**钳制有损不可逆**：错误尺寸钳出来的坐标一旦写回，正确值就永远回不来。
    /// 两个时机都会踩到：
    ///
    /// - **启动**：窗口以 `create` 的占位尺寸 160×40 起步，位置在 `init_toolbar_pos` 下发、
    ///   朝向要到其后的 `apply_ui_config` 才下发，且 `set_vertical` 隐藏期间不重排。纵条
    ///   于是被按 160 宽钳制，贴右保存的坐标被判越界拉回 `右边界-160`，重启后左移 100+px。
    /// - **跨 DPI 换屏**：`window.size()` 还是**上一块屏** DPI 下的尺寸（`ensure_scale` 要到
    ///   随后的 `render` 才跑）。真机实测：右屏 100% 的纵条 48×212 贴死右下角存 (2512,1156)，
    ///   切到 133% 的左屏再切回来时按 64×282 钳，被拉到 (2496,1086)——左移 16、上移 70，
    ///   且因为结果写回了 `mouse.pos`，之后用正确尺寸重钳也已不越界，位置永久走样。
    ///
    /// 落点与钳制统一由 `render` 做：那里 `ensure_scale` 已按目标屏定好 scale、`resize`
    /// 也已按当前朝向排完版，`w`/`h` 才是真值。数值本体见 `sys::clamp_rect_tests`。
    ///
    /// 不做 alpha 恢复与计时重置：`render` 末尾以 alpha=255 提交并 `on_shown`。
    ///
    /// ⚠️ `repaint` 受 `visible` 门控：`render` 末尾无条件 `show`，对隐藏中的工具栏调用会把
    /// 它显形，绕过 `toolbar_gate` 的显示迟滞（同 `set_vertical` 的约束）。
    pub fn set_pos(&mut self, x: i32, y: i32) {
        // 显式位置优先于待落的角落请求，否则 render 会拿 pending 覆盖掉这次设定。
        self.pending_corner = None;
        self.mouse.borrow_mut().pos = Some((x, y));
        if self.visible {
            self.repaint();
        }
    }

    /// 移到指定显示器工作区的右下角——焦点切到一块**从未拖过工具栏**的屏时用。
    ///
    /// 由协调器传工作区右/下边界、这边算落点，而不是协调器直接算坐标下发：右下角要减去
    /// 工具栏自身的 w/h，而尺寸只有 UI 侧知道（随主题/朝向/DPI 变）。留边同 `corner_position`。
    ///
    /// **无论可见与否都只登记意图**，落点由 `render` 计算。隐藏期间是因为尺寸还是占位值
    /// （见 `set_pos`）；可见期间则是因为目标屏 DPI 可能与当前屏不同——`render` 里
    /// `ensure_scale` 先按目标屏定 scale、再排版出尺寸，只有那一刻两者才同时正确。
    /// 若在这里用当前 `window.size()` 算，跨 DPI 换屏首帧会按旧屏尺寸定位。
    ///
    /// 可见时自己 `repaint` 兜底：协调器当前总是紧跟一条 `UpdateToolbar`（`notify_toolbar`
    /// 里 `sync_toolbar_monitor` 之后必然下发），但那是**调用方的时序契约**，不该由它
    /// 决定本次请求是否生效——多渲染一帧在换屏这种低频事件上无关紧要。
    /// ⚠️ `repaint` 必须受 `visible` 门控：`render` 末尾无条件 `show`，对隐藏中的工具栏
    /// 调用会把它显形，绕过 `toolbar_gate` 的显示迟滞（同 `set_vertical` 的约束）。
    pub fn set_corner(&mut self, work_right: i32, work_bottom: i32) {
        self.pending_corner = Some((work_right, work_bottom));
        if self.visible {
            self.repaint();
        }
    }

    /// 配置显示哪些格、按什么顺序（`ui.toolbar.items`，经 SetToolbarLayout 下发）。
    ///
    /// 格数变化会改窗口尺寸，故可见时立即用缓存状态重绘——否则要等下一次状态推送
    /// （切中英等）才生效，设置页里改完看着像没生效。同 `set_vertical` 的理由。
    ///
    /// ⚠️ 重绘必须受 `visible` 门控：`repaint`→`render` 末尾无条件 `show`，对隐藏中的
    /// 工具栏调用会把它显形，绕过 `toolbar_gate` 的显示迟滞（同 `set_vertical` 的约束）。
    pub fn set_layout(&mut self, items: Vec<ToolbarItem>) {
        if self.layout == items {
            return;
        }
        self.layout = items;
        if self.visible {
            self.repaint();
        }
    }

    /// 根据配置的项序列 + 当前状态构建单元格。
    /// 布局：拖动条 | 各项按 `self.layout` 的顺序展开
    ///
    /// 项序列来自配置（`self.layout`），而本 crate 读不到配置，只消费协调器解析好的结果。
    /// 同 `mode_text` 那处的分工——判据与取值都归协调器。
    ///
    /// 真正的展开逻辑在自由函数 [`expand_cells`]，本方法只是取 `self.layout` 转发：
    /// 构造 `Toolbar` 需要真窗口（非 Windows 上是 mock），挂在它上面的逻辑测不到。
    /// 同 `bar_layout` 被抽成自由函数的理由。
    fn cells(&self, state: &ToolbarState) -> Vec<Cell> {
        expand_cells(&self.layout, state)
    }
}

/// 把配置的项序列 + 当前状态展开成单元格序列。
///
/// # 空结果必须回落，且这道闸门只能装在这里
///
/// 协调器的 `parse_toolbar_items` 已保证**项序列**非空，但那只保证「配置里写了东西」，
/// 保证不了「展开后还剩几格」——决定画几格的是这里。
///
/// ⚠️ 触发这条的路径**变过一次**：原先是 `S2t` 带运行时合取（简繁没开就不产格），
/// 配 `items = ["s2t"]` 而简繁关着即整条空掉。那层合取已删（见 `S2t` 分支的说明），
/// 于是当下所有内置项都无条件产格，只剩「全是 `Custom` 且全被 `enabled = false` 关掉」
/// 这一条路能走到空——协调器侧那种情况已在 `push_custom_item` 里跳过，故理论上到不了。
///
/// 兜底照留：**闸门要装在产出最终结果的那一环**，而不是靠上游当下恰好不会送空进来。
/// 一旦日后又给某个内置项加回运行时条件（那正是上一轮发生过的事），这里不必跟着改。
///
/// 不 panic 也不返回空：`bar_layout` 在 n=0 下数学上安全（无除法），但「安全」不等于
/// 「可接受」——回落全量条至少是个能用的工具栏，且与 `visible=false` 那条真正的
/// 「不要工具栏」路径不冲突。
fn expand_cells(layout: &[ToolbarItem], state: &ToolbarState) -> Vec<Cell> {
    let cells = expand_cells_raw(layout, state);
    if !cells.is_empty() {
        return cells;
    }
    // 回落走 `expand_cells_raw` 而不是直接构造全集的 Cell：状态相关的取值（简繁字面、
    // 高亮）只该有一份实现，绕开它就是把同一套判断抄第二遍。
    //
    // ⚠️ **这一步不可能再空，不必加第二层兜底**：全集里的内置项在 `expand_cells_raw`
    // 里全是无条件 push。若日后给某个内置项加运行时条件，这条论证就失效了——
    // 那时要么改这里，要么保证至少一项恒存。
    expand_cells_raw(&wind_ui_types::DEFAULT_TOOLBAR_ITEMS, state)
}

/// [`expand_cells`] 的无兜底内核。单独一层是为了让兜底自身可测——否则「回落」与
/// 「本来就该有格」两种结果长得一样，测不出兜底有没有生效。
fn expand_cells_raw(layout: &[ToolbarItem], state: &ToolbarState) -> Vec<Cell> {
    {
        // 有效中文：中文模式且大写锁定未开（对齐 Go effectiveChinese = chineseMode && !capsLockOn）。
        // 密码框强制英文时同样不算「有效中文」——此刻键已全部透传给宿主，高亮着中文格
        // 会与实际行为相反。⚠ 这是纯呈现判断，输入闸在 coordinator 的 password_suppress
        // 分支，两者各管各的，勿把本行的结论回灌给任何状态。
        let effective_chinese = state.chinese_mode && !state.caps_lock && !state.input_blocked;
        // 显示标签**完全**由协调器预计算存入 icon_label，此处直接使用、不再有任何例外。
        //
        // 不可输入（密码框等）时曾在这里覆盖成字面量 "英"。标签可配（`[ui.labels]`）之后
        // 那条覆盖必须撤掉：本 crate 读不到配置，留着它的唯一结果是"用户把英文标签改成
        // En，一进密码框又变回英"。判据归协调器、取值也归协调器，这里只负责画。
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
                // 全/半角格：文本留空，渲染时按状态画月亮 SVG（满月=全角 / 弯月=半角，对齐微软五笔）。
                ToolbarItem::FullWidth => cells.push(Cell {
                    text: String::new(),
                    highlight: false,
                    dim: false,
                    action: ToolbarAction::ToggleWidth,
                }),
                // 简繁格：**只由 layout 决定显隐**，与 `state.s2t_enabled` 无关。
                //
                // 曾额外合取一个运行时条件（`s2t_shown`，取值就是「当前是否简入繁出」），
                // 理由是「没开简繁却常驻一个『简』格不是状态指示器该干的事」。真机推翻：
                // 那让开关**自锁**——关着时格子不画，于是工具栏上再也开不回来，而这一格
                // 恰恰是它唯一的鼠标入口。显隐本来就有 `ui.toolbar.items` 这个开关，
                // 运行时那层是第二个开关，方向还与用户意图相反。
                //
                // 与全半角 / 标点两格现在是同一套呈现逻辑：格子恒在，状态只由字与高亮表达，
                // 不淡显——`dim` 曾跟着 `!s2t_enabled` 走，是上一轮从「合取显隐」改成
                // 「恒显示」时漏摘的半成品，导致简体态这一格比 Mode/Punct/FullWidth 三格更透。
                ToolbarItem::S2t => cells.push(Cell {
                    text: if state.s2t_enabled { "繁" } else { "简" }.to_string(),
                    highlight: state.s2t_enabled,
                    dim: false,
                    action: ToolbarAction::ToggleS2t,
                }),
                // 软键盘格：文本留空，渲染时画矢量键盘（同齿轮/月亮，不依赖字体字形）。
                // 开着时高亮——图标只有一张，开合由格底表达。
                ToolbarItem::SoftKeyboard => cells.push(Cell {
                    text: String::new(),
                    highlight: state.soft_keyboard_on,
                    dim: false,
                    action: ToolbarAction::ToggleSoftKeyboard,
                }),
                // 设置格：文本留空，渲染时画矢量齿轮（不依赖字体字形）。
                // 位置随配置，不再固定末尾；隐藏它也不会锁死用户——右键工具栏任意位置
                // 同样弹主菜单（见 `ToolbarMouse::on_message` 的 WM_RBUTTONDOWN）。
                ToolbarItem::Settings => cells.push(Cell {
                    text: String::new(),
                    highlight: false,
                    dim: false,
                    action: ToolbarAction::OpenSettings,
                }),
                // 自定义按钮：label 已由协调器按显示宽度截好（本 crate 读不到配置，
                // 也就不该在这里判断"多宽算宽"）。无状态可言，故不高亮不淡显。
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

impl Toolbar {
    /// DPI 动态化：按工具栏当前位置所在显示器实时取缩放（拖到别的显示器后自动适配）。
    /// 工具栏仅颜色随主题、几何随 scale 现算，故只需更新 scale 与字号。
    ///
    /// 有待落的换屏请求时按**目标屏**取：此刻 `mouse.pos` 还停在上一块屏上，用它会让
    /// 这一帧按旧屏 DPI 排版，而 `render` 末尾又拿这套尺寸去算目标屏的落点——两屏 DPI
    /// 不同则整条大小与位置都偏，要等下一帧才自愈（视觉上是一跳）。
    /// `set_pos` 那条路径无需特判：它已把 `mouse.pos` 更新成目标屏坐标。
    fn ensure_scale(&mut self) {
        let pos = match self.pending_corner {
            // 工作区右/下边界是排他的，退 1px 取屏内点。
            Some((work_right, work_bottom)) => (work_right - 1, work_bottom - 1),
            None => self.mouse.borrow().pos.unwrap_or((0, 0)),
        };
        let sc = crate::dpi::scale_for_point(pos.0, pos.1);
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
        self.ensure_scale();
        let s = self.scale;
        // Dim→设备像素（dp×scale）；None→def_logical×scale（同候选窗 dim 闭包）。
        let dim = |o: Option<Dim>, def_logical: f32| {
            o.map(|x| x.resolve(s, 0.0)).unwrap_or(def_logical * s)
        };
        // 纵向下这两个值转 90°：thickness 成条宽、grip_len 成顶端拖动区高度。
        let thickness = dim(self.tb_height, Self::HEIGHT).ceil();
        let grip_len = dim(self.tb_grip_width, Self::GRIP_W).ceil();

        let cells = self.cells(state);
        // 英文模式下标点固定显示半角，无需看 chinese_punct。
        let effective_chinese = state.chinese_mode && !state.caps_lock;

        // 每格等长（默认 30dp≈方形）：标点/简繁等图标与文字均居中于等长格，
        // 状态切换不改变格尺寸，工具栏整体长度稳定不抖动。主题可配 button_width 覆盖。
        let cell_len = dim(self.tb_button_width, Self::BUTTON_W);
        let layout = bar_layout(self.vertical, thickness, grip_len, cell_len, cells.len());
        let w = layout.w.ceil() as u32;
        let h = layout.h.ceil() as u32;

        self.window.resize(w, h);
        let buf_size = (w * h * 4) as usize;
        {
            let buf = self.window.buffer_mut();
            buf[..buf_size].fill(0);
            // 整条圆角：主题 [toolbar] border.radius 优先，未配则 = 条**短边**×0.30
            // （见 `default_border_radius`）。配 0 即直角——硬边缘风格靠这条实现。
            let radius = self
                .tb_border_radius
                .map(|d| d.resolve(s, 0.0))
                .unwrap_or_else(|| default_border_radius(w as f32, h as f32))
                as u32;
            fill_rounded(buf, w, h, 0, 0, w, h, self.bg, radius);
            // 细边框（与背景同弧度），增强浅色背景下的轮廓（对齐设计稿胶囊外框）。
            // 线宽：主题 border.width 优先，未配落 1dp（原字面量）。
            let border_w = self
                .tb_border_width
                .map(|d| d.resolve(s, 0.0))
                .unwrap_or(1.0 * s)
                .max(1.0);
            crate::view::fill_ring(
                buf,
                w,
                h,
                0.0,
                0.0,
                w as f32,
                h as f32,
                self.sep,
                radius as f32,
                border_w,
            );
            // 拖动柄点阵
            draw_grip(buf, w, h, &layout.grip, self.vertical, self.grip, s);
        }

        // 逐格绘制 + 记录命中矩形
        let font_h = self.renderer.measure_text("中").height;
        let mut hits: Vec<(ToolbarAction, Rect)> = Vec::with_capacity(cells.len());
        for (i, c) in cells.iter().enumerate() {
            let r = layout.cells[i];
            hits.push((c.action, r));
            // 分隔线：仅「拖动柄之后」(首格前) 与「齿轮的边界」绘制（对齐设计稿，状态格之间不画）。
            //
            // 齿轮的边界是**哪一边**取决于它在哪：默认排末尾时画它的起始边；`ui.toolbar.items`
            // 允许把它排到首位后，那条线与首格前的线重合成同一条，齿轮反而没了边界——
            // 于是这种情况改画**下一格**的起始边（= 齿轮的结束边），齿轮自成一区的意图两种
            // 排列下都成立。齿轮既在首位又是唯一一格时不画（没有"下一格"，也无需分区）。
            let is_settings = matches!(c.action, ToolbarAction::OpenSettings);
            let sep_at = if i == 0 && is_settings {
                layout
                    .cells
                    .get(1)
                    .map(|n| if self.vertical { n.y } else { n.x })
            } else if i == 0 || is_settings {
                // 画在格的**起始边**上：横条取左缘 x、纵条取上缘 y。
                Some(if self.vertical { r.y } else { r.x })
            } else {
                None
            };
            if let Some(pos) = sep_at {
                draw_sep(
                    self.window.buffer_mut(),
                    w,
                    h,
                    pos as u32,
                    self.vertical,
                    self.sep,
                    s,
                );
            }
            // 激活格（中文模式）画主题色底 + 高亮文字；悬停格画极淡底。
            // hl_bg 成对配合 hl_fg（如 msime 白字蓝底），缺底色时白字在亮色工具栏上不可见。
            let cell_bg = if c.highlight {
                Some(self.hl_bg)
            } else if (i as i32) == hover_idx {
                Some(self.hover_bg)
            } else {
                None
            };
            if let Some(bgc) = cell_bg {
                let inset = dim(self.tb_button_padding, Self::BUTTON_PAD) as u32;
                // 长轴方向两端各缩 inset/2、厚度方向两端各缩 inset（横条既有比例，纵条转置
                // 施加）——高亮块因此在两个朝向下都是"沿条身更瘦"的胶囊，而非贴边方块。
                let (hx, hy, hw, hh) = if self.vertical {
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
                // 高亮格圆角：主题 button_radius 优先，否则 = 内**短边**×0.3。横条下短边
                // 恒是内高（厚度方向缩得更多），故与原「内高×0.3」等值，纵条则自动转置。
                let hr = self
                    .tb_button_radius
                    .map(|d| d.resolve(s, 0.0))
                    .unwrap_or(hw.min(hh) as f32 * 0.3) as u32;
                fill_rounded(self.window.buffer_mut(), w, h, hx, hy, hw, hh, bgc, hr);
            }
            if is_settings {
                let size = font_h * 0.80;
                let dx = r.x + (r.w - size) * 0.5;
                let dy = r.y + (r.h - size) * 0.5;
                crate::view::draw_svg_icon(
                    self.window.buffer_mut(),
                    w,
                    h,
                    SETTING_SVG,
                    dx,
                    dy,
                    size,
                    self.settings_icon,
                );
            } else if matches!(
                c.action,
                ToolbarAction::TogglePunct
                    | ToolbarAction::ToggleWidth
                    | ToolbarAction::ToggleSoftKeyboard
            ) {
                // 标点 / 全半角 / 软键盘：渲染内联 SVG 图标，主题色 tint，居中于方格。
                // 前两者按状态换图，软键盘只有一张（开合由格底高亮表达）。
                // 英文模式下标点固定半角（不可切换），无论 chinese_punct 状态如何。
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
                // 高亮格（软键盘开着）要用 hl_fg：hl_bg 是主题色实底，用常态 fg 画图标
                // 会与底色撞成一团。文字分支下面本来就按 highlight 选色，这里补齐同一条。
                let tint = if c.highlight { self.hl_fg } else { self.fg };
                crate::view::draw_svg_icon(self.window.buffer_mut(), w, h, svg, dx, dy, size, tint);
            } else {
                // 居中文字
                let m = self.renderer.measure_text(&c.text);
                let tx = r.x + (r.w - m.width) * 0.5;
                let ty = r.y + (r.h - font_h) * 0.5;
                let fg = if c.highlight {
                    self.hl_fg
                } else if c.dim {
                    dim_color(self.fg)
                } else {
                    self.fg
                };
                let _ = self.renderer.draw_text(
                    self.window.buffer_mut(),
                    w,
                    h,
                    tx.max(r.x),
                    ty.max(r.y),
                    &c.text,
                    fg,
                );
            }
        }
        if let Err(e) = self.window.update() {
            tracing::warn!("Toolbar update failed: {}", e);
        }

        // 位置：优先用持久化/拖动后的位置；首次落在工作区右下角（避开任务栏）。
        // 钳制到当前显示器工作区内——避免切换显示器/远程后旧坐标落在屏外不可见。
        //
        // 一切依赖尺寸的落点计算都收口在这里：上面刚按当前朝向/主题/DPI 排完版并 resize，
        // `w`/`h` 此刻才是真值。`set_pos`/`set_corner` 在隐藏期间只登记意图、不算坐标，
        // 就是为了不在 `window.size()` 仍是占位值 160×40 时下判断（见 `set_pos` 文档）。
        let (px, py) = {
            let mut m = self.mouse.borrow_mut();
            m.hits = hits; // 同步命中矩形给鼠标处理器
            // 菜单锚点要用的尺寸/朝向，与命中矩形同源同刻更新——分开更新迟早错位。
            m.size = (w, h);
            m.vertical = self.vertical;
            let raw = match self.pending_corner.take() {
                Some((work_right, work_bottom)) => {
                    Self::corner_in_work_area(work_right, work_bottom, w, h)
                }
                None => m.pos.unwrap_or_else(|| Self::corner_position(w, h)),
            };
            let clamped = clamp_to_work_area(raw.0, raw.1, w, h);
            m.pos = Some(clamped);
            clamped
        };
        self.window.show(px, py);
        self.visible = true;
        self.rendered_hover = hover_idx;
        // 任何显示/状态刷新（render 是所有显示路径的单点）都重置自动隐藏计时。
        // window.update() 以 alpha=255 提交，天然恢复不透明。
        self.auto_hide.on_shown(std::time::Instant::now());
    }

    /// UI 循环每轮调用：消费鼠标处理器的悬停脏标记（由 WM_MOUSEMOVE/WM_MOUSELEAVE 事件置位），
    /// 仅在悬停格变化时本地重绘（无需协调器往返、不轮询光标）。与菜单 dirty→tick 重绘模式一致。
    /// 下一次需要 [`Self::tick`] 的时刻；`None` = 无需为工具栏安排唤醒。
    ///
    /// 只转发自动隐藏的计时。**悬停重绘不在此列**：`dirty` 由 `WM_MOUSEMOVE` /
    /// `WM_MOUSELEAVE` 置位，那两条消息本身就会唤醒消息循环，而 `tick` 排在消息泵之后，
    /// 同一轮即可消费——不需要额外的到期时刻。
    pub fn next_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        if !self.visible {
            return None;
        }
        self.auto_hide.next_deadline(now)
    }

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
        // 自动隐藏推进。快速路径：未启用/无活动计时时 is_active()=false 直接跳过，
        // 不取 Instant::now()、无系统调用（开关关闭时零开销的硬约束）。
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
            }
        }
    }

    // 曾有一个 `show()`（用缓存 pos 直接显示、不重绘），长期无调用者。删除而非保留：
    // 它绕开 `render`，既不消费 `pending_corner`（显示完再被下一次 render 取出算落点，
    // 视觉上凭空跳一次），也不按当前朝向/DPI 重排尺寸。所有显示路径收口于 `render` 是
    // 本模块的既有约定，一个不走 render 的显示入口只会是下一个人的陷阱。
    // 若将来真需要「不改状态地重新显形」，用 `repaint()`（受 `visible` 门控）。

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

    /// 给定工作区右/下边界，算工具栏右下角落点（右/下各留 12px 边距）。
    ///
    /// 纯几何、无系统调用，故可被任意显示器复用——`corner_position` 喂主屏，
    /// `set_corner` 喂焦点所在屏。`max(0)` 的下限只在单屏（工作区从 0 起）时有意义，
    /// 副屏的工作区左/上边界可为负，钳到 0 会把工具栏推回主屏；真正的越界回收交给
    /// `set_pos` 里的 `clamp_to_work_area`（它按落点解析显示器，不预设原点）。
    fn corner_in_work_area(work_right: i32, work_bottom: i32, w: u32, h: u32) -> (i32, i32) {
        const MARGIN: i32 = 12;
        (
            work_right - w as i32 - MARGIN,
            work_bottom - h as i32 - MARGIN,
        )
    }

    /// 主显示器工作区右下角位置（避开任务栏）。
    ///
    /// **仅在协调器那侧 `focus_monitor()` 失败时才轮得到**——它正常总能给出焦点所在屏，
    /// 于是首帧要么走 `pending_corner`（该屏无记忆位置）、要么走 `mouse.pos`（有记忆位置），
    /// 两条路都不经过这里。`SPI_GETWORKAREA` 取的恒是主屏，用它给多屏定位是错的；
    /// 别照着这个函数名去调它。
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
    ///
    /// ⚠️ **首次 `render` 之前可能是未钳制的原始值**：`Toolbar::set_pos` 在隐藏期间不钳制
    /// （那时窗口尺寸还是占位值，钳了反而错——见其文档），要到 `render` 才按真实尺寸钳并
    /// 回写。今天安全，因为本结构体的其余读者（`rect`/`menu_anchor`/拖动 `origin`）全部
    /// 挂在鼠标消息上，而隐藏窗口收不到鼠标消息。**若日后新增不依赖鼠标消息的读路径，
    /// 先确认它是否可能在首帧之前触发。**（`size` 在首帧前同为 `(0,0)`，同一道门挡住。）
    pos: Option<(i32, i32)>,
    dragging: bool,
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
    /// 最近一次渲染的窗口尺寸与朝向，由 `render` 同步（与 `hits` 同一处）。
    /// 菜单锚点据此计算——比现取 `GetWindowRect` 准（渲染刚定的尺寸，无需等窗口生效）
    /// 且无系统调用。`render` 必先于任何鼠标事件发生，故不存在 (0,0) 被用到的时机。
    size: (u32, u32),
    vertical: bool,
}

impl ToolbarMouse {
    /// 工具栏当前占据的屏幕矩形 `(left, top, right, bottom)`。
    fn rect(&self) -> (i32, i32, i32, i32) {
        let (x, y) = self.pos.unwrap_or((0, 0));
        (x, y, x + self.size.0 as i32, y + self.size.1 as i32)
    }

    /// 主菜单锚点：横条向上弹（避免压住工具栏），纵条向侧面弹——竖条上仍向上弹会让
    /// 菜单飘到条顶之上老远，与点击位置差出整条的高度。
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

    /// 右键某一格时的菜单锚点：与 [`Self::menu_anchor`] 同一套展开方向，但**贴到那一格**
    /// 而不是整条的起点。
    ///
    /// 分格菜单只有几项，锚在整条起点会让它落在离点击位置半条远的地方——尤其是齿轮
    /// 排末尾的横条上。方向仍由朝向决定，只替换**沿条身**那一维，另一维仍取整条的边
    /// （菜单该贴的是条身的外沿，不是格的）。
    ///
    /// ⚠️ **沿条身那一维是哪个字段，两种朝向不一样**，得照 `place_menu` 实际读哪个来传：
    ///
    /// - 横条（`Above`）：横向位置读 `x` ⇒ 传格的左缘；纵向恒在整条顶边之上。
    /// - 纵条（`Side`）：纵向位置读 **`bottom`**（底边对齐、向上展开），`y` 只在上方
    ///   装不下时作回退 ⇒ **两个都要传格的**，否则菜单仍按整条底边对齐、贴格等于没做。
    ///
    /// 命中不到格（拖动柄区）时退回整条锚点。
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
                if self.cell_at(cx, cy).is_none() {
                    // 非单元格（拖动柄区）→ 开始拖动
                    let mut p = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                    }
                    self.anchor = (p.x, p.y);
                    self.origin = self.pos.unwrap_or((p.x, p.y));
                    self.dragging = true;
                    if self.hover_idx != -1 {
                        self.hover_idx = -1; // 拖动中不显示悬停
                        self.dirty = true;
                    }
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.cursor_inside = true;
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
                if self.dragging {
                    self.dragging = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    // 取实际窗口位置回报，供持久化
                    let mut r = RECT::default();
                    let (x, y) = unsafe {
                        if GetWindowRect(self.hwnd, &mut r).is_ok() {
                            (r.left, r.top)
                        } else {
                            self.pos.unwrap_or((0, 0))
                        }
                    };
                    self.pos = Some((x, y));
                    let _ = self.events.send(UiEvent::ToolbarMoved { x, y });
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
                // 右键工具栏 → 该格的快捷菜单，贴着工具栏弹出（避免遮挡工具栏）。
                // 认不认得这一格由协调器判断（它才读得到方案/软键盘/开关态），
                // 认不得就回落完整主菜单——这里只负责报出点在哪一格上。
                let _ = self.events.send(UiEvent::RequestToolbarMenu {
                    action: self.cell_at(cx, cy),
                    anchor: self.cell_menu_anchor(cx, cy),
                });
                Some(LRESULT(0))
            }
            // 中键点中英格 = 直接切到下一个方案，省掉「右键 → 找到方案 → 点」三步。
            //
            // 只认这一格：其余格中键无动作。中键是**没有视觉提示**的入口，绑在语义
            // 最直白的那一格上还能靠「这格本来就管方案」猜到；散给每一格就成了记忆负担，
            // 且误触代价不一（在简繁格上误触会改动正文的输出形态）。
            //
            // 落在 UP 而非 DOWN，与左键一致：按下再挪开取消，是鼠标交互的通行预期。
            WM_MBUTTONUP => {
                // 拖动中不触发格动作，与左键 UP 同一条守卫：拖着工具栏走时命中矩形照样
                // 命中，没有这道判断就会在挪动过程中顺手切了方案。
                if self.dragging {
                    return Some(LRESULT(0));
                }
                if let Some(ToolbarAction::ToggleMode) = self.cell_at(cx, cy) {
                    // 复用既有的 SwitchEngine（协调器映射到 `switch_engine` → `cycle_schema`），
                    // 不新增动作：这里要的正是它，另起一个只会多一条要各自维护的路径。
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

/// 次要状态文字淡显：alpha 降至 ~65%（半角/未启用简繁等次要态比正常文字更弱）。
fn dim_color(c: [u8; 4]) -> [u8; 4] {
    [c[0], c[1], c[2], (c[3] as f32 * 0.65) as u8]
}

/// 格间分隔线：横条画竖线（`pos` 是 x），纵条画横线（`pos` 是 y）；线两端各内缩 6px。
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

/// 拖动柄点阵，居中于 `grip` 区。横条 2 列×3 行、纵条转置为 3 列×2 行——点阵的长边
/// 始终垂直于条身，看着才像「抓手」而不是顺着条身的一道装饰线。
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
mod tests {
    use super::*;

    // 典型值：主题 [toolbar] 默认（条厚 30dp、拖动柄 12dp、格 30dp）× scale 1.0，
    // 4 格（模式/标点/全半角/设置）。
    const THICK: f32 = 30.0;
    const GRIP: f32 = 12.0;
    const CELL: f32 = 30.0;
    const N: usize = 4;

    /// 横条：回归基线。重构成 `bar_layout` 之前，render 内联算的就是这组值——
    /// 这条测试的作用是「纵向功能没有偷偷改掉横向的既有排布」。
    #[test]
    fn horizontal_layout_matches_legacy_geometry() {
        let l = bar_layout(false, THICK, GRIP, CELL, N);
        assert_eq!((l.w, l.h), (12.0 + 30.0 * 4.0, 30.0));
        assert_eq!(
            (l.grip.x, l.grip.y, l.grip.w, l.grip.h),
            (0.0, 0.0, 12.0, 30.0)
        );
        // 格自拖动柄之后起，沿 x 依次排开，各格占满条厚。
        for (i, c) in l.cells.iter().enumerate() {
            assert_eq!(c.x, 12.0 + 30.0 * i as f32, "第 {i} 格 x");
            assert_eq!((c.y, c.w, c.h), (0.0, 30.0, 30.0), "第 {i} 格");
        }
    }

    /// 右下角落点：从工作区右/下边界各退去工具栏尺寸再留 12px 边距。
    #[test]
    fn corner_backs_off_from_work_area_edges() {
        // 1920×1080 主屏，任务栏 40px：工作区右下 (1920, 1040)；工具栏 132×30。
        let (x, y) = Toolbar::corner_in_work_area(1920, 1040, 132, 30);
        assert_eq!((x, y), (1920 - 132 - 12, 1040 - 30 - 12));
    }

    /// 副屏在主屏**左侧**时工作区坐标为负，落点必须跟着为负。
    ///
    /// 这正是不能在此处 `max(0)` 的理由：钳到 0 会把工具栏推回主屏，表现为「切到左边那块屏
    /// 工具栏没跟过去」。越界回收由 `set_pos` 里的 `clamp_to_work_area` 负责——它按落点
    /// 反查显示器，不预设桌面原点在 (0,0)。
    #[test]
    fn corner_allows_negative_coords_on_left_side_monitor() {
        // 左侧副屏：虚拟桌面 x ∈ [-1920, 0)，工作区右下 (0, 1080)。
        let (x, y) = Toolbar::corner_in_work_area(0, 1080, 132, 30);
        assert_eq!(x, -144, "落点应留在左侧副屏上（负坐标）");
        assert_eq!(y, 1038);
    }

    /// 落点是**尺寸的函数**——同一块屏上横条与纵条的右下角必然落在不同位置。
    ///
    /// 这条测试把「算落点时尺寸必须已是真值」钉死。窗口以 `create(160, 40)` 的占位尺寸
    /// 起步，`set_vertical` 在隐藏期间不重排，故首次 `render` 之前 `window.size()` 两种
    /// 朝向的真值都不是。启动序列恰好在那个窗口里恢复位置，用占位尺寸算/钳的结果就是
    /// 重启后凭空左移——量级见末尾断言。修法是 `set_pos`/`set_corner` 隐藏期间只登记
    /// 意图，落点与钳制统一由 `render` 用刚 `resize` 出的尺寸计算。
    #[test]
    fn corner_depends_on_bar_orientation() {
        // 1920×1080 屏、底部 40px 任务栏 → 工作区右下 (1920, 1040)。
        // 默认几何：横条 132×30，转置后纵条 30×132。
        let horizontal = Toolbar::corner_in_work_area(1920, 1040, 132, 30);
        let vertical = Toolbar::corner_in_work_area(1920, 1040, 30, 132);
        assert_eq!(vertical.0 - horizontal.0, 102, "纵条更窄，落点更靠右");
        assert_eq!(horizontal.1 - vertical.1, 102, "纵条更高，落点更靠上");

        // 占位尺寸算出的落点比纵条真值靠左 130px——「重启后位置左移」的正是这个量。
        // 若哪天改了 create 的占位尺寸，这条会红，提醒回来确认推迟计算仍然成立。
        let placeholder = Toolbar::corner_in_work_area(1920, 1040, 160, 40);
        assert_eq!(vertical.0 - placeholder.0, 130);
    }

    /// 纵条：整条与横条互为转置——宽高对调、格沿 y 排开、各格占满条宽。
    #[test]
    fn vertical_layout_is_transpose_of_horizontal() {
        let h = bar_layout(false, THICK, GRIP, CELL, N);
        let v = bar_layout(true, THICK, GRIP, CELL, N);
        assert_eq!((v.w, v.h), (h.h, h.w), "整条宽高对调");
        assert_eq!((v.grip.w, v.grip.h), (h.grip.h, h.grip.w), "拖动柄区对调");
        assert_eq!(v.cells.len(), h.cells.len());
        for (i, (cv, ch)) in v.cells.iter().zip(h.cells.iter()).enumerate() {
            assert_eq!((cv.x, cv.y), (ch.y, ch.x), "第 {i} 格坐标对调");
            assert_eq!((cv.w, cv.h), (ch.h, ch.w), "第 {i} 格尺寸对调");
        }
    }

    /// 主题几何在两个朝向下同源：条厚恒取 `height`、格长恒取 `button_width`。
    /// 若哪天有人为纵向另引一套尺寸，这条会红——那正是要挡的改动。
    #[test]
    fn vertical_reuses_same_theme_dimensions() {
        let v = bar_layout(true, THICK, GRIP, CELL, N);
        assert_eq!(v.w, THICK, "纵条宽 = 主题 height");
        for c in &v.cells {
            assert_eq!(c.h, CELL, "纵条每格高 = 主题 button_width");
            assert_eq!(c.w, THICK, "纵条每格宽 = 条宽");
        }
        assert_eq!(v.h, GRIP + CELL * N as f32, "纵条总高 = 拖动柄 + 各格");
    }

    /// 兜底外框圆角在两个朝向下必须相等——它描述的是**条的厚度**，与条有多长无关。
    ///
    /// 曾经这里写的是 `h * 0.30`：横条下 `h` 是厚度（30dp→9dp，正确），纵条下 `h` 却成了
    /// 整条长度（192dp→57.6dp），再被 `push_round_rect` 的 `min(w*0.5)` 钳成 15dp 满胶囊。
    /// 症状是「纵排圆角明显比横排大」，且格数越多越必然触顶。同一个转置问题在按钮高亮
    /// 圆角那里（`hw.min(hh)`）修对了、外框这里漏了——故此处按朝向对拍钉死。
    #[test]
    fn border_radius_is_orientation_invariant() {
        let h = bar_layout(false, THICK, GRIP, CELL, N);
        let v = bar_layout(true, THICK, GRIP, CELL, N);
        let rh = default_border_radius(h.w, h.h);
        let rv = default_border_radius(v.w, v.h);
        assert_eq!(rh, rv, "同一套几何转 90° 后圆角不得改变");
        assert_eq!(rh, THICK * 0.30, "圆角派生自条厚，与条长无关");

        // 钳制上限 = 短边一半；兜底值必须留在其下，否则会被静默压成满胶囊。
        for (tag, l) in [("横", &h), ("纵", &v)] {
            let r = default_border_radius(l.w, l.h);
            assert!(
                r < l.w.min(l.h) * 0.5,
                "{tag}条圆角 {r} 触顶（短边半值 {}）",
                l.w.min(l.h) * 0.5
            );
        }

        // 条越长圆角越不该跟着变——纵条加 5 格，半径原地不动。
        let longer = bar_layout(true, THICK, GRIP, CELL, N + 5);
        assert_eq!(default_border_radius(longer.w, longer.h), rv);
    }

    /// 格数随简繁格增减（`cells()` 的既有行为），布局须跟着长短，不能越界。
    #[test]
    fn layout_tracks_cell_count() {
        let four = bar_layout(true, THICK, GRIP, CELL, 4);
        let five = bar_layout(true, THICK, GRIP, CELL, 5);
        assert_eq!(five.h - four.h, CELL, "多一格恰长一格");
        assert_eq!(five.cells.len(), 5);
        // 末格不得超出整条（渲染越界会被静默裁掉，看着像"最后一格没画出来"）。
        let last = five.cells.last().unwrap();
        assert!(
            last.y + last.h <= five.h,
            "末格越界：{} > {}",
            last.y + last.h,
            five.h
        );
    }

    /// n=1（`items` 只留一格）与 n=0 的排布。
    ///
    /// 两者本次才**从不可达变成可达**（`ui.toolbar.items` 之前，格数恒在 4~5）。n=0 已由
    /// `expand_cells` 的兜底挡住，仍钉一条：`bar_layout` 全程无除法，这里断言的是"就算
    /// 哪天兜底被绕过也不会 panic / 不会算出负尺寸"。
    #[test]
    fn layout_handles_one_and_zero_cells() {
        let one = bar_layout(false, THICK, GRIP, CELL, 1);
        assert_eq!(one.w, GRIP + CELL);
        assert_eq!(one.cells.len(), 1);
        assert_eq!(one.cells[0].x, GRIP);

        let zero = bar_layout(false, THICK, GRIP, CELL, 0);
        assert!(zero.cells.is_empty());
        // 只剩拖动柄：尺寸仍为正（负尺寸会让 resize/缓冲区计算炸掉）。
        assert_eq!(zero.w, GRIP);
        assert!(zero.w > 0.0 && zero.h > 0.0);
    }

    /// `s2t_on` = 简入繁出当前是否开着。**它只该影响格里画什么，不该影响有没有这一格**
    /// ——参数名从早先的 `s2t_shown` 改过来，正是因为那层「开着才显示」的合取已删。
    fn tb_state(s2t_on: bool) -> ToolbarState {
        ToolbarState {
            icon_label: "拼".to_string(),
            s2t_enabled: s2t_on,
            // 桌面工具栏已不读这个字段（它留给 macOS / 移动端），这里跟着 s2t_enabled 填，
            // 避免读者误以为两者还有关系。
            s2t_shown: s2t_on,
            ..Default::default()
        }
    }

    fn actions(cells: &[Cell]) -> Vec<ToolbarAction> {
        cells.iter().map(|c| c.action).collect()
    }

    /// 默认项序列展开出全部六格，且**与任何运行时状态无关**。
    #[test]
    fn default_layout_expands_to_every_item() {
        let expected = vec![
            ToolbarAction::ToggleMode,
            ToolbarAction::TogglePunct,
            ToolbarAction::ToggleWidth,
            ToolbarAction::ToggleS2t,
            ToolbarAction::ToggleSoftKeyboard,
            ToolbarAction::OpenSettings,
        ];
        for s2t_on in [true, false] {
            assert_eq!(
                actions(&expand_cells(
                    &wind_ui_types::DEFAULT_TOOLBAR_ITEMS,
                    &tb_state(s2t_on)
                )),
                expected,
                "格的有无不该随运行时状态变（s2t_on={s2t_on}）"
            );
        }
    }

    /// ★ 防回归：简繁格**恒在**，开关态只体现在格内的字与高亮上。
    ///
    /// 这一格曾与「简繁当前开着」合取，于是关掉之后它自己消失，而它正是简繁的唯一
    /// 鼠标入口——开关把自己藏了，工具栏上再也开不回来。显隐归 `ui.toolbar.items`
    /// 一处管，这条钉的就是「运行时不许再插一脚」。
    #[test]
    fn s2t_cell_stays_when_conversion_is_off() {
        let layout = [ToolbarItem::S2t];
        for (s2t_on, text, hl) in [(true, "繁", true), (false, "简", false)] {
            let cells = expand_cells(&layout, &tb_state(s2t_on));
            assert_eq!(actions(&cells), vec![ToolbarAction::ToggleS2t]);
            assert_eq!(cells[0].text, text);
            assert_eq!(cells[0].highlight, hl);
        }
    }

    /// 顺序照配置走，没配的项不出现。
    #[test]
    fn expand_follows_configured_order_and_subset() {
        let layout = [ToolbarItem::Settings, ToolbarItem::Mode];
        assert_eq!(
            actions(&expand_cells(&layout, &tb_state(true))),
            vec![ToolbarAction::OpenSettings, ToolbarAction::ToggleMode]
        );
    }

    /// 空展开必须回落全量条，而不是渲染出一条只剩拖动柄的空窄条。
    ///
    /// ⚠️ **触发这条的路径变过**：原先是「`items` 只留 `s2t` 而简繁没开」——那层合取
    /// 已删，如今内置项全部无条件产格，只有空 layout 还能走到。兜底照留：闸门要装在
    /// 产出最终结果的那一环，而不是靠上游当下恰好不会送空进来。
    #[test]
    fn empty_expansion_falls_back_instead_of_rendering_nothing() {
        let cells = expand_cells(&[], &tb_state(false));
        assert!(!cells.is_empty(), "空展开必须回落，否则工具栏只剩拖动柄");
        assert_eq!(
            actions(&cells),
            actions(&expand_cells_raw(
                &wind_ui_types::DEFAULT_TOOLBAR_ITEMS,
                &tb_state(false)
            )),
            "回落的应当是全量条"
        );
        // 内核（无兜底）确实会给出空——证明上面那条断言测的是兜底本身，不是恒真。
        assert!(expand_cells_raw(&[], &tb_state(false)).is_empty());
    }

    /// 每张工具栏图标都必须**真的能光栅化出可见形状**。
    ///
    /// ★ 这条守的是一个**全程无声**的失效：`rasterize_svg_str_tinted` 解析失败返回
    /// `None`，`draw_svg_icon` 见 `None` 就直接 return——SVG 写错（路径语法、viewBox
    /// 缺失、把形状画到画布外）的表现是**格子空白**，没有日志、没有 panic，编译与其余
    /// 测试全绿。而工具栏格空白与「这一格没配」长得一模一样。
    ///
    /// 按 `FONT_PX * 0.80` 的实际渲染尺寸测（12px），而不是挑一个宽松的大尺寸：细节
    /// 太密的图标在真实尺寸下可能糊成一片，这里顺带把「12px 上还剩多少墨」钉住——
    /// 低于阈值多半是形状太细或跑到画布外了。
    #[test]
    fn every_toolbar_icon_rasterizes_to_visible_ink() {
        let size = (Toolbar::FONT_PX * 0.80).round() as u32;
        for (name, svg) in [
            ("punct_full", PUNCT_FULL_SVG),
            ("punct_half", PUNCT_HALF_SVG),
            ("width_full", WIDTH_FULL_SVG),
            ("width_half", WIDTH_HALF_SVG),
            ("setting", SETTING_SVG),
            ("soft_keyboard", SOFT_KEYBOARD_SVG),
        ] {
            let pm = crate::image_cache::rasterize_svg_str_tinted(svg, size, size, [0, 0, 0, 255])
                .unwrap_or_else(|| panic!("{name}.svg 解析失败——渲染时会静默画不出来"));
            let inked = pm.pixels().iter().filter(|p| p.alpha() > 0).count();
            let total = (size * size) as usize;
            assert!(
                inked * 100 / total >= 5,
                "{name}.svg 在 {size}px 下只有 {inked}/{total} 像素有墨——形状太细或落在画布外"
            );
        }
    }

    /// 缩放只改绝对值、不改结构：dp→设备像素由调用方（render 的 dim 闭包）算好再传入。
    #[test]
    fn layout_scales_uniformly() {
        let one = bar_layout(true, THICK, GRIP, CELL, N);
        let two = bar_layout(true, THICK * 2.0, GRIP * 2.0, CELL * 2.0, N);
        assert_eq!((two.w, two.h), (one.w * 2.0, one.h * 2.0));
    }
}

#[cfg(test)]
mod cell_anchor_tests {
    use super::*;
    use crate::view::Rect;

    /// 造一个只填了锚点计算所需字段的鼠标处理器。
    fn mouse(vertical: bool, hits: Vec<(ToolbarAction, Rect)>) -> ToolbarMouse {
        let (tx, _rx) = std::sync::mpsc::channel();
        ToolbarMouse {
            hits,
            events: tx,
            hwnd: Default::default(),
            pos: Some((1000, 500)),
            dragging: false,
            anchor: (0, 0),
            origin: (0, 0),
            hover_idx: -1,
            dirty: false,
            cursor_inside: false,
            size: if vertical { (40, 200) } else { (200, 40) },
            vertical,
        }
    }

    fn cell(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// 横条：菜单横向贴到**格的左缘**，纵向仍取整条顶边（向上展开）。
    #[test]
    fn horizontal_anchor_hugs_the_cell_left_edge() {
        let m = mouse(
            false,
            vec![
                (ToolbarAction::ToggleMode, cell(12.0, 0.0, 30.0, 40.0)),
                (ToolbarAction::TogglePunct, cell(42.0, 0.0, 30.0, 40.0)),
            ],
        );
        let a = m.cell_menu_anchor(50.0, 20.0); // 命中第二格
        assert_eq!(a.x, 1000 + 42, "应贴到格的左缘，而不是整条起点");
        assert_eq!(a.y, 500, "纵向仍取整条顶边");
        assert_eq!(a.bottom, 500 + 40, "翻转回退用整条底边");
    }

    /// ★ 纵条：`Side` 的纵坐标读的是 **`bottom`**（底边对齐、向上展开），`y` 只作越界
    /// 回退。所以两个都得传格的边——只传 `y` 的话菜单仍按整条底边对齐，「贴格」等于没做，
    /// 而代码看着是对的。这条测试就是钉住那个差别。
    #[test]
    fn vertical_anchor_hugs_the_cell_bottom_not_the_bar_bottom() {
        let m = mouse(
            true,
            vec![
                (ToolbarAction::ToggleMode, cell(0.0, 12.0, 40.0, 30.0)),
                (ToolbarAction::TogglePunct, cell(0.0, 42.0, 40.0, 30.0)),
            ],
        );
        let a = m.cell_menu_anchor(20.0, 50.0); // 命中第二格
        assert_eq!(a.bottom, 500 + 42 + 30, "必须是格的底边");
        assert_ne!(a.bottom, 500 + 200, "整条底边就是没贴格");
        assert_eq!(a.y, 500 + 42, "回退用格的上缘");
        assert_eq!(a.right, 1000 + 40, "横向仍贴条身外沿");
    }

    /// 命中不到格（拖动柄区）时退回整条锚点——右键那里仍该弹完整主菜单。
    #[test]
    fn miss_falls_back_to_the_whole_bar() {
        let m = mouse(
            false,
            vec![(ToolbarAction::ToggleMode, cell(12.0, 0.0, 30.0, 40.0))],
        );
        let a = m.cell_menu_anchor(5.0, 20.0); // 拖动柄区
        assert_eq!(a.x, 1000, "应退回整条起点");
    }
}
