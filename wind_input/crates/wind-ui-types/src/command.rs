//! 正向协议：协调器 → 渲染端的 [`UiCommand`]，及全局热键条目、翻页器 tag 常量。

use crate::candidate::CandidateItem;
use crate::diag::InputDiagView;
use crate::menu::{MenuAnchor, MenuItemSpec};
use crate::softkeyboard::SoftKeyCap;
use crate::toast::{ToastKind, ToastPosition};
use crate::toolbar::{ToolbarItem, ToolbarState};

/// UI 命令
#[derive(Debug)]
pub enum UiCommand {
    /// 更新候选列表
    UpdateCandidates {
        preedit: String,
        /// 编码区插入符位置：`preedit` 内的**字节**偏移（恒在字符边界）。自绘 preedit 栏据此
        /// 画竖线；等于 `preedit.len()` 即光标在末尾。
        preedit_caret: usize,
        /// 编码区**由宿主自绘**（`preedit_display = app_inline`：编码嵌在宿主组合区里）。
        ///
        /// ⚠ 它只回答「谁来画」，**不影响 `preedit` 是否下发**——后者恒有值。
        /// 此前是「不画就不发」，把渲染策略焊在了数据通道上：自绘编码栏的宿主
        /// （Android）想拿数据，只能去改一个显示模式配置项。
        preedit_host_owned: bool,
        /// 模式指示文本（拼/双/快/英/符 或全称）；空=不显示。有 preedit 空间时随候选窗持久显示。
        mode_label: String,
        candidates: Vec<CandidateItem>,
        /// 键盘选中项（页内下标），空格上屏目标
        selected: usize,
        /// 鼠标悬停项（页内下标），-1 表示无；与 selected 独立
        hover: i32,
        /// 当前页（1 起）
        page: usize,
        /// 总页数（含动态加载估计）
        total_pages: usize,
        caret_x: i32,
        caret_y: i32,
        /// 光标高度（用于上翻时定位到光标上方）
        caret_height: i32,
        /// 光标坐标是否有效（无效时窗口仅临时显示、不锁定锚点）
        caret_valid: bool,
        /// 固定位置模式（ui.candidate.position_mode=fixed）：忽略光标，用 fixed_x/fixed_y 定位。
        fixed: bool,
        /// 固定位置的**内容左上**屏幕坐标；(0,0) 表示尚未设定，由 UI 侧落到屏幕默认锚点。
        fixed_x: i32,
        fixed_y: i32,
    },
    /// 隐藏候选窗口
    HideCandidates,
    /// 一次性通知 toast（方案切换/词库就绪/错误等）；duration_ms 后自动隐藏。
    ShowToast {
        text: String,
        position: ToastPosition,
        kind: ToastKind,
        duration_ms: u64,
    },
    /// 显示状态提示气泡（中英/标点/全半角/方案切换），约 1 秒后自动隐藏。
    /// (x,y)=光标点(y 为底端)，caret_height 上翻定位用，offset_x/y 用户位置微调。
    ShowStatusTip {
        text: String,
        x: i32,
        y: i32,
        caret_height: i32,
        offset_x: i32,
        offset_y: i32,
        /// 自动隐藏时长（毫秒）；0=常驻不自动隐藏（display_mode=always）。
        duration_ms: u64,
        /// 固定位置模式（position_mode=fixed）：用 fixed_x/fixed_y 作屏幕坐标，忽略光标。
        fixed: bool,
        fixed_x: i32,
        fixed_y: i32,
    },
    /// 隐藏状态提示气泡（常驻模式失焦/切走输入法时）。
    HideStatusTip,
    /// 显示/更新输入诊断 HUD（右键「高级」开）。惰性创建，可拖动，双击复制。
    ShowInputDiag(InputDiagView),
    /// 隐藏输入诊断 HUD。
    HideInputDiag,
    /// 复制输入诊断 HUD 当前显示的文本到剪贴板（右键菜单）。
    /// 走 UI 线程而非协调器直接写剪贴板：文本以**实际渲染的行**为准（含分区隐藏结果），
    /// 那份只有 UI 侧有。
    CopyInputDiagText,
    /// 更新常驻工具栏状态（中英/方案/标点/全半角）
    UpdateToolbar(ToolbarState),
    /// 隐藏工具栏
    HideToolbar,
    /// 设置工具栏位置（启动恢复持久化位置 / 焦点换屏后落到该屏的记忆位置）
    SetToolbarPos { x: i32, y: i32 },
    /// 把工具栏落到指定显示器工作区的右下角——焦点切到一块从未拖过工具栏的屏时下发。
    /// 传边界而非坐标：右下角要减工具栏自身尺寸，那只有 UI 侧知道。
    SetToolbarCorner { work_right: i32, work_bottom: i32 },
    /// 工具栏自动隐藏配置（开关 + 超时毫秒）。来自 ui.toolbar.auto_hide / auto_hide_delay，
    /// 协调器 apply_ui_config（启动 + 配置重载）下发。
    SetToolbarAutoHide { enabled: bool, delay_ms: u64 },
    /// 工具栏纵向排列（true=竖条）。来自 ui.toolbar.vertical，
    /// 协调器 apply_ui_config（启动 + 配置重载）下发。
    SetToolbarVertical(bool),
    /// 工具栏显示哪些格、按什么顺序（顺序即渲染顺序）。来自 ui.toolbar.items，
    /// 协调器 apply_ui_config（启动 + 配置重载）下发，解析与告警都在那侧。
    ///
    /// **不并进 `UpdateToolbar`**：那条是随按键高频推送的动态状态、靠 `PartialEq` 去重，
    /// 把这份配置塞进去等于每次切中英都 clone 一遍列表再深比较——去重反成开销。
    /// 同 `SetToolbarVertical` / `SetToolbarAutoHide` 的分界。
    SetToolbarLayout(Vec<ToolbarItem>),
    /// 应用主题（协调器加载解析后下发）
    SetTheme(Box<wind_theme::Resolved>),
    /// 候选布局方向。来自 ui.candidate.layout（叠加方案级与模式级意图后的结果）。
    ///
    /// 两位刻意分开而不是一个四值枚举：旋转态的 `vertical` 是 **false**（屏幕上候选确是
    /// 并列的），于是渲染端所有按方向分叉的既有判据自动走横排那一支，一处不用改；
    /// 只有「列表怎么构造」这一件事额外判 `rotated`。
    /// `upright` 再往下一层，只改「叶子怎么搭」，排列与 `rotated` 完全一致。
    /// ⚠️ 合法组合只有四个：`vertical && rotated` 不合法（竖排再转 90° 就是横排），
    /// `upright && !rotated` 也不合法（字直立是旋转态内部的取舍）。
    SetCandidateLayout {
        /// 候选纵向堆叠。
        vertical: bool,
        /// 整个候选列表顺时针旋转 90° 呈现（蒙古文等纵向书写脚本）。
        rotated: bool,
        /// 旋转态下每个字逆时针扶正、逐字下行（对联式竖排）。蕴含 `rotated`。
        upright: bool,
    },
    /// 预编辑嵌入模式（true=编码嵌入候选行首，不显示独立 preedit 条）。
    /// 来自 ui.candidate.preedit_display == "candidate_inline"。
    SetPreeditEmbedded(bool),
    /// 候选字号覆盖（0=跟随主题）。来自 ui.candidate.font_size。
    SetCandidateFontSize(f32),
    /// 候选字体：主字体 + 回退链 + 按脚本的字体指派。来自 `[ui.font]`。
    ///
    /// 三项合成一条命令而非各发各的：`fallback` 的语义是「`family` 缺字时找谁」，
    /// 链首就是 `family` 本身 —— 拆成两条命令就产生了**到达顺序依赖**（先收到链、
    /// 后收到主字体时，链首是错的），而命令通道不保证接线方按发送顺序消费每一类。
    /// 同 `SetCandidateMinSize` 把五个值并一条的理由。
    ///
    /// `scripts` 的键是**字符串**而不是枚举：脚本类的定义与它的 Unicode 区间表同住
    /// wind-ui（`text::script`），把枚举拆到本协议 crate 会让「加一个类」变成改两个 crate，
    /// 而区间表才是那个类真正的定义。未知键由渲染端记一条 warn 后忽略——配置是用户手写的。
    SetCandidateFont {
        /// `ui.font.family`，空 = 用渲染端的内置默认字族。
        family: String,
        /// `ui.font.fallback`，主字体缺字时的接续顺序。
        fallback: Vec<String>,
        /// `ui.font.scripts`：脚本类名 → 该类的字体链。
        scripts: Vec<(String, Vec<String>)>,
    },
    /// 候选**文字节点**的字族覆盖（方案级 `[candidate] font_family`）；空 = 不覆盖。
    ///
    /// ⚠️ 与 [`Self::SetCandidateFont`] 的**下发节奏不同**，故是两条命令而不是一个字段：
    /// 前者来自 `ui.font`、随配置重载推一次；本条的归属是**数据方案**
    /// （临英/快符叠加时会切走），随输入语境逐次按键变化，故在 `UpdateCandidates`
    /// 的两个发送点之前重算。合成一条会让配置重载那条路每次按键都重推整份字体方案。
    SetCandidateTextFamily(String),
    /// 候选窗尺寸下限（抗抖动）。来自 ui.candidate.min_window_width_horizontal /
    /// min_window_width_vertical / min_window_height_horizontal /
    /// min_window_height_vertical / min_rows。
    ///
    /// 五值合成一条命令而非各发各的：每条命令都要在两个平台的 manager 各接一条分发臂、
    /// 在启动与热重载两处各发一次，合并后这四个接线点只需记住一次。
    SetCandidateMinSize {
        /// 横排时窗口最小宽度，单位 dp（0=不限）。
        width_horizontal: u32,
        /// 竖排时窗口最小宽度，单位 dp（0=不限）。
        width_vertical: u32,
        /// 横排时窗口最小高度，单位 dp（0=不限）。
        height_horizontal: u32,
        /// 竖排时窗口最小高度，单位 dp（0=不限）。
        height_vertical: u32,
        /// 竖排最小行数，不足补透明占位行（0=不补）。
        rows: u32,
    },
    /// 悬停提示激活延迟（毫秒）。来自 ui.tooltip.delay。
    SetTooltipDelay(i32),
    /// 候选窗在光标上方时反转候选顺序。来自 ui.candidate.flip_when_above。
    SetCandidateFlipWhenAbove(bool),
    /// 候选窗在光标上方时交换编码栏与候选栏位置。来自 ui.candidate.swap_preedit_when_above。
    SetCandidateSwapWhenAbove(bool),
    /// 翻页栏并入编码栏行右对齐显示。来自 ui.candidate.pager_in_preedit。
    SetPagerInPreedit(bool),
    /// 翻页栏显示覆盖（""跟随主题/"hide"/"auto"/"always"）。来自 ui.candidate.pager_bar_display。
    SetPagerDisplay(String),
    /// 页码文字显示覆盖（""跟随主题/"show"/"hide"）。来自 ui.candidate.page_number_display。
    SetPageNumberDisplay(String),
    /// 拆字字根字体（PUA 字根字符渲染）：TTF 文件路径 + DWrite 家族名（取自方案 [engine.chaizi]）。
    SetTooltipChaiziFont { path: String, family: String },
    /// 显示菜单（候选右键菜单 / 功能主菜单；UI 自管导航与子菜单）。
    ShowCandidateMenu {
        items: Vec<MenuItemSpec>,
        anchor: MenuAnchor,
    },
    /// 转发键给打开的菜单（方向键/回车/ESC/空格）；菜单窗无焦点，键由协调器转发
    MenuKey(u32),
    /// 隐藏菜单
    HideMenu,
    /// 写剪贴板（菜单"复制"由协调器驱动 → UI 侧执行）
    CopyToClipboard(String),
    /// 用资源管理器打开路径（菜单"打开配置目录"）
    OpenPath(String),
    /// 启动应用程序并传参（如 wind_setting.exe `--page dict`）。
    OpenApp { path: String, args: String },
    /// 截图所有可见 UI 窗口，保存到 dir 目录（由协调器根据 config 确定）。
    TakeScreenshot { dir: String },
    /// 将候选窗口截图复制到剪贴板（候选不可见则提示）。
    ScreenshotCandidateToClipboard,
    /// 截图状态提示气泡到文件（状态提示右键菜单「截图此窗口」）。
    ScreenshotStatusTip { dir: std::path::PathBuf },
    /// 复制悬停提示（编码反查气泡）文本到剪贴板（其右键菜单「复制内容」）。
    CopyTooltipText,
    /// 截图悬停提示到文件（其右键菜单「截图此窗口」）。
    ScreenshotTooltip { dir: std::path::PathBuf },
    /// 设置悬停提示右键菜单打开状态（开启时抑制其 WM_MOUSELEAVE 自动隐藏）。
    SetTooltipMenuOpen(bool),
    /// 标记状态气泡的右键菜单开/关（打开期间抑制自动隐藏）。
    SetStatusMenuOpen(bool),
    /// 请求上报状态气泡当前位置：UI 侧回 `UiEvent::StatusTipMoved`。
    /// 供「固定位置」开关把当前实际位置落盘，而不是跳到陈旧的 custom_x/custom_y。
    ReportStatusTipPos,
    /// 请求上报候选窗当前位置：UI 侧回 `UiEvent::CandidateWindowMoved`。
    /// 供「定位方式」切到 fixed 时就地固定——窗口正显示着就用它当前的位置，
    /// 不显示则不上报（协调器留空，首显时由 UI 落到屏幕默认锚点）。
    ReportCandidatePos,
    /// 注册全局热键（Win32 RegisterHotKey，线程级）。覆盖式：先反注册旧列表再注册新列表，
    /// 空列表 = 仅清除已注册项。来自 keys.global_hotkeys（协调器构建，启动/配置重载时下发）。
    RegisterGlobalHotkeys(Vec<GlobalHotkeyEntry>),
    /// 软键盘：显示面板，或整块刷新它（切面也走这一条）。
    ///
    /// 只带**当前面**的键位，不带全部 13 面，理由见 [`crate::softkeyboard`]。
    ShowSoftKeyboard {
        /// 各面显示名，用来画标签行。
        pages: Vec<String>,
        /// 当前面在 `pages` 里的下标。
        current: usize,
        /// 当前面的键位表，顺序即键盘上的行列顺序。
        keys: Vec<SoftKeyCap>,
    },
    /// 软键盘：隐藏面板。
    HideSoftKeyboard,
    /// 软键盘：物理按键按下/抬起时同步键帽高亮。
    ///
    /// 与 [`Self::ShowSoftKeyboard`] 分开是因为它每次按键都发，而面板内容不变——
    /// 渲染端据此只重画那一个键帽，不必重排整块面板。
    SoftKeyboardKeyState { slot: String, down: bool },
    /// 软键盘：切层（按住 Shift 显示第二层，松开还原）。
    SoftKeyboardLayer { shift: bool },
    /// 关闭 UI
    Shutdown,
    /// 注入 host-render 管理器（Windows）；协调器 `set_host_render` 后下发，
    /// UI 线程收到后在消息循环中激活 SHM 分流路径。
    #[cfg(windows)]
    SetHostRender(HostRenderArc),
}

/// `HostRenderManager` 不派生 Debug，包一层使 UiCommand 可 derive Debug。
#[cfg(windows)]
pub struct HostRenderArc(pub std::sync::Arc<wind_bridge::host_render_windows::HostRenderManager>);

#[cfg(windows)]
impl std::fmt::Debug for HostRenderArc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostRenderManager")
    }
}

/// 全局热键条目（协调器按 keys.global_hotkeys 构建，UI 线程经 Win32 RegisterHotKey 注册）。
/// `modifiers` 为 Win32 RegisterHotKey 修饰位（MOD_ALT=0x1/MOD_CONTROL=0x2/MOD_SHIFT=0x4/
/// MOD_WIN=0x8），与 wind-ipc 的 MOD_* 位序不同（ALT/SHIFT 互换），转换在协调器侧完成。
#[derive(Debug, Clone)]
pub struct GlobalHotkeyEntry {
    /// RegisterHotKey 热键 ID（UI 线程内唯一即可）
    pub id: i32,
    /// Win32 修饰位
    pub modifiers: u32,
    /// Windows 虚拟键码
    pub vk: u32,
    /// 触发后回送协调器的热键动作名（与 dispatch_hotkey 的 action 一致）
    pub action: String,
}

/// 翻页器命中/悬停 tag（远高于候选下标，避免冲突）
pub const HOVER_PAGE_PREV: i32 = 100_000;
pub const HOVER_PAGE_NEXT: i32 = 100_001;
