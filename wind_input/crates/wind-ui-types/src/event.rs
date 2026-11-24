//! 反向协议：渲染端 → 协调器的 [`UiEvent`]（鼠标交互 / 系统通知）。

use crate::menu::{CandidateOp, MenuAnchor, MenuKind, ToolbarAction};

/// UI → 协调器的反向事件（鼠标交互）
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// 点击选中当前页内第 N 个候选（0 起）
    CandidateSelect(usize),
    /// 滚轮翻页：>0 下一页，<0 上一页
    Page(i32),
    /// 悬停到页内候选下标（-1 表示离开）
    Hover(i32),
    /// 工具栏单元格点击
    Toolbar(ToolbarAction),
    /// 工具栏被拖动到新位置，供协调器持久化。
    ///
    /// 报的是窗口右下角屏幕坐标（`GetWindowRect` 的 `right`/`bottom`）：持久化的是锚点，
    /// 工具栏尺寸会随朝向/格数变（见 `UiCommand::SetToolbarAnchor`）。只在拖动结束
    /// （`WM_LBUTTONUP`）时发一次。
    ToolbarMoved { right: i32, bottom: i32 },
    /// 软键盘面板被拖动到新位置，供协调器持久化（右下角屏幕坐标）。
    ///
    /// 与 [`Self::ToolbarMoved`] 同构，也是抬起才发一次；别在 `WM_MOUSEMOVE` 里发，
    /// 那会每帧一次 IPC + state.toml 读改写。
    SoftKeyboardMoved { right: i32, bottom: i32 },
    /// 候选词条操作（页内下标 + 动作）
    CandidateOp { op: CandidateOp, page_local: usize },
    /// 右键候选请求弹出菜单（页内下标 + 屏幕坐标）；协调器据此构建菜单项回送
    RequestCandidateMenu { page_local: usize, x: i32, y: i32 },
    /// 请求功能主菜单；来自候选窗空白或工具栏的设置键。
    RequestMainMenu(MenuAnchor),
    /// 右键工具栏：请求该格的快捷菜单。
    ///
    /// `action` 是命中格的动作（拖动柄区为 `None`），协调器据它给精简菜单。没有定制的格
    /// 一律回落完整主菜单，不可省：隐藏了齿轮时这是主菜单仅剩的鼠标入口。UI 侧只报点在
    /// 哪一格，菜单内容由协调器决定。
    RequestToolbarMenu {
        action: Option<ToolbarAction>,
        anchor: MenuAnchor,
    },
    /// 菜单项激活（携带动作）：UI 自管导航/子菜单，仅把最终动作回送协调器
    MenuAction(MenuKind),
    /// 关闭菜单（点击菜单外 / ESC / 右键）
    MenuClose,
    /// 全局热键触发（线程级 RegisterHotKey 的 WM_HOTKEY），携带热键动作名
    GlobalHotkey(String),
    /// 点击软键盘键帽：键位名 + 当时是否在第二层（`shift`）。
    ///
    /// 回送键位名而不是下标：键位名是两侧共同的拼写，错了会当场报警而不是静默错位。
    /// `ctrl` = 面板上的 Ctrl 处于粘滞状态，此时不论哪种面一律走合成按键（Ctrl+C 要的是复制）。
    SoftKeyboardKey {
        slot: String,
        shift: bool,
        ctrl: bool,
    },
    /// 点击软键盘标签行切面（面下标）。
    SoftKeyboardPage(usize),
    /// 点击软键盘的关闭按钮。
    SoftKeyboardClose,
    /// 点击软键盘左上角的钉住按钮（切换钉住状态，真相在协调器）。
    SoftKeyboardPin,
    /// 点击软键盘上的特殊键（退格 / Tab / 回车 / 空格 / Ins / Del）。
    ///
    /// 键名取 `wind_keys::key_inject` 认得的那套，协调器收到后合成一次真实按键。
    SoftKeyboardFunctionKey(String),
    /// 状态提示气泡被拖动到新位置（内容左上屏幕坐标），供协调器持久化
    StatusTipMoved { x: i32, y: i32 },
    /// 候选窗被拖动到新位置（内容左上屏幕坐标）。协调器仅在 fixed 模式下持久化；
    /// follow_caret 模式的拖动是"本次组合内临时挪开"，不落盘。
    CandidateWindowMoved { x: i32, y: i32 },
    /// 候选窗空白处（编码栏 / 内边距）被双击。协调器按 `ui.candidate.double_click_screenshot`
    /// 决定是否截图——UI 侧读不到配置，只报事实。候选项与翻页键不产生本事件（按下即动作）。
    CandidateDoubleClick,
    /// 右键状态提示气泡请求弹出菜单（屏幕坐标）
    RequestStatusMenu { x: i32, y: i32 },
    /// 右键悬停提示请求弹出菜单。
    ///
    /// - `x`/`y`：屏幕坐标（菜单锚点）；
    /// - `candidate`：气泡属于当前页的第几个候选（页内下标）。右键时鼠标已在气泡上、
    ///   不在候选上，协调器的悬停目标此刻未必还指着它，故由显示气泡的一方带上；
    /// - `hit`：点中的段 / 原始行（见 `TooltipDoc::hit_at_line`）；点在内边距为 `None`；
    /// - `doc_fingerprint`：UI 当前所画气泡的 `TooltipDoc::fingerprint`。`hit` 是按 UI 所见
    ///   换算的，协调器缓存若已换成另一份结构，段下标就对不上——两边指纹不等时不给按段操作。
    RequestTooltipMenu {
        x: i32,
        y: i32,
        candidate: i32,
        hit: Option<crate::TooltipHit>,
        doc_fingerprint: u64,
    },
    /// 输入诊断 HUD 上右键：请求其上下文菜单（复制 / 显示分类 / 停止刷新 / 置顶）。
    RequestInputDiagMenu { x: i32, y: i32 },
    /// 系统「浅色/深色模式」已切换（Win32 `WM_SETTINGCHANGE`/`ImmersiveColorSet`）。
    /// 协调器仅在 `ui.theme.style = "system"` 时据此重解析主题，其余明暗为用户显式指定。
    SystemThemeChanged,
    /// 候选项排列当前是否被反转（`flip_when_above` 真正生效，见 `CandidateWindow::above_layout`）。
    /// 仅在取值变化时发送。协调器据此把 `highlight_up` / `highlight_down` 的走向翻过来 ——
    /// 判据只有 UI 侧算得出（要窗口尺寸 + 屏幕工作区才知道有没有上翻），协调器不能自行推导。
    CandidateFlipped(bool),
}
