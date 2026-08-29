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
    /// 工具栏被拖动到新位置（屏幕坐标），供协调器持久化
    ToolbarMoved { x: i32, y: i32 },
    /// 候选词条操作（页内下标 + 动作）
    CandidateOp { op: CandidateOp, page_local: usize },
    /// 右键候选请求弹出菜单（页内下标 + 屏幕坐标）；协调器据此构建菜单项回送
    RequestCandidateMenu { page_local: usize, x: i32, y: i32 },
    /// 请求功能主菜单；来自候选窗空白/工具栏右键或设置键。
    RequestMainMenu(MenuAnchor),
    /// 菜单项激活（携带动作）：UI 自管导航/子菜单，仅把最终动作回送协调器
    MenuAction(MenuKind),
    /// 关闭菜单（点击菜单外 / ESC / 右键）
    MenuClose,
    /// 全局热键触发（线程级 RegisterHotKey 的 WM_HOTKEY），携带热键动作名
    GlobalHotkey(String),
    /// 点击软键盘键帽：键位名 + 当时是否在第二层。
    ///
    /// 回送键位名而不是下标：下标依赖「面板与协调器对键位顺序的理解一致」，
    /// 而键位名是两侧共同的那套拼写，错了会当场报警而不是静默错位。
    /// 点了一个符号键帽。
    ///
    /// `ctrl` = 面板上的 Ctrl 处于粘滞状态。此时**一律走合成按键**，不管当前是哪种面：
    /// Ctrl+C 要的是复制，跟这一面画着什么符号没有关系。
    SoftKeyboardKey {
        slot: String,
        shift: bool,
        ctrl: bool,
    },
    /// 点击软键盘标签行切面（面下标）。
    SoftKeyboardPage(usize),
    /// 点击软键盘的关闭按钮。
    SoftKeyboardClose,
    /// 点击软键盘上的**特殊键**（退格 / Tab / 回车 / 空格 / Ins / Del）。
    ///
    /// 键名取 `wind_keys::key_inject` 认得的那套（`backspace` / `tab` / `enter` …），
    /// 协调器收到后合成一次真实按键。物理键不走这里——它们本就透传给宿主，
    /// 连我们都不经过。
    SoftKeyboardFunctionKey(String),
    /// 状态提示气泡被拖动到新位置（内容左上屏幕坐标），供协调器持久化
    StatusTipMoved { x: i32, y: i32 },
    /// 候选窗被拖动到新位置（内容左上屏幕坐标）。协调器仅在 fixed 模式下持久化；
    /// follow_caret 模式的拖动是"本次组合内临时挪开"，不落盘。
    CandidateWindowMoved { x: i32, y: i32 },
    /// 右键状态提示气泡请求弹出菜单（屏幕坐标）
    RequestStatusMenu { x: i32, y: i32 },
    /// 右键悬停提示（编码反查气泡）请求弹出菜单（屏幕坐标）
    RequestTooltipMenu { x: i32, y: i32 },
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
