//! 菜单协议：工具栏动作、候选词条操作、功能主菜单命令与菜单项规格。
//!
//! `MenuKind::to_menu_id`/`from_menu_id` 是稳定 id 空间的双向映射（macOS `.app`
//! 经 `NSMenuItem.tag` 往返；Android 长按菜单可复用同一套 id）。

/// 工具栏单元格动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    /// 中/英切换（合并方案显示）
    ToggleMode,
    /// 切换输入方案（保留供外部调用，工具栏不单独显示）
    SwitchEngine,
    /// 中/英标点切换
    TogglePunct,
    /// 全/半角切换
    ToggleWidth,
    /// 简/繁转换切换
    ToggleS2t,
    /// 开关软键盘面板
    ToggleSoftKeyboard,
    /// 打开设置
    OpenSettings,
    /// 自定义按钮：执行 `ui.toolbar.buttons[i]` 的 cmdbar 表达式。
    ///
    /// 载荷是**下标**而不是 id 字符串，因为 `ToolbarAction` 必须保持 `Copy`——
    /// `Toolbar` 的命中表 `Vec<(ToolbarAction, Rect)>` 与 `cell_at` / `hover_at`
    /// 全建立在这个前提上，带 `String` 会让整条命中链路改签名。
    ///
    /// 下标失配（配置重载与 UI 侧 spec 之间的一瞬）最坏是执行了相邻按钮的动作，
    /// 非破坏性；协调器侧按下标取不到就忽略。
    Custom(u8),
}

/// 候选词条操作（右键菜单）；复制由 UI 侧直接处理，不在此列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateOp {
    /// 置顶
    MoveTop,
    /// 前移
    MoveUp,
    /// 后移
    MoveDown,
    /// 删除（屏蔽）
    Delete,
    /// 恢复默认
    Reset,
    /// 常用/生僻互切（**全局字级**，不限本方案本码）。
    ///
    /// 与上面五项不是一类：那些落在 shadow（键 = 方案 + 输入码），这个落在常用字覆盖表
    /// （键 = 那个字）。菜单只给一项，文案按当前判定二选一——「设为生僻字」/「设为常用字」，
    /// 故不需要两个变体。
    ToggleCommon,
}

/// 功能主菜单命令（对齐 Go 统一菜单）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCmd {
    /// 切到英文模式
    SchemaEnglish,
    /// 选择第 N 个输入方案
    SchemaSelect(usize),
    /// 中/英标点切换
    TogglePunct,
    /// 全/半角切换
    ToggleWidth,
    /// 简繁转换开关
    ToggleS2t,
    /// 检索范围过滤（0 智能/1 常用字/2 全部字符）
    FilterMode(usize),
    /// 选择第 N 个主题
    ThemeSelect(usize),
    /// 主题明暗（0 跟随/1 亮/2 暗）
    ThemeStyle(u8),
    /// 显示/隐藏工具栏
    ToggleToolbar,
    /// 开关软键盘面板
    ToggleSoftKeyboard,
    /// 打开软键盘并切到第 N 面
    SoftKeyboardPage(usize),
    /// 重载配置
    ReloadConfig,
    /// 重启服务进程
    RestartService,
    /// 打开用户数据目录（配置/词库等用户数据所在目录）
    OpenConfigDir,
    /// 打开应用程序目录（exe 所在目录，高级菜单）
    OpenAppDir,
    /// 打开日志文件目录（高级菜单）
    OpenLogDir,
    /// 词库管理（暂兜底为打开配置目录）
    OpenDictionary,
    /// 设置（暂兜底为打开配置目录）
    OpenSettings,
    /// 关于（暂兜底）
    OpenAbout,
    /// 截图所有可见 UI 窗口到文件（高级菜单）
    TakeScreenshot,
    /// 截图候选窗口到剪贴板（高级菜单）
    ScreenshotCandidateToClipboard,
    /// 切换输入诊断 HUD 显隐（高级菜单）
    ToggleInputDiagnostics,
    /// 切换密码框强制英文（高级菜单，临时测试入口）
    TogglePasswordSuppress,
    /// 状态提示气泡：切换常驻显示（display_mode always/temp）
    StatusToggleAlways,
    /// 状态提示气泡：切换「焦点切换时显示」（ui.status.show_on_focus）
    StatusToggleShowOnFocus,
    /// 状态提示气泡：恢复默认位置（position_mode=follow_caret）
    StatusResetPosition,
    /// 状态提示气泡：截图此窗口
    StatusScreenshot,
    /// 输入诊断 HUD：复制全部内容（所见即所得，含分区隐藏后的结果）
    InputDiagCopy,
    /// 输入诊断 HUD：切换分区显示。参数为分区序号，见 [`crate::diag::DiagSections::label`]
    InputDiagToggleSection(u8),
    /// 输入诊断 HUD：停止/恢复刷新（冻结当前快照，便于切走观察时不被新焦点刷掉）
    InputDiagToggleFreeze,
    /// 输入诊断 HUD：切换窗口置顶（关掉可让 HUD 沉到被观察窗口之下）
    InputDiagToggleTopmost,
    /// 悬停提示（编码反查气泡）：复制内容
    TooltipCopy,
    /// 悬停提示（编码反查气泡）：截图此窗口
    TooltipScreenshot,
    /// 状态提示气泡：切换固定位置（position_mode fixed/follow_caret）
    StatusTogglePinned,
    /// 为当前焦点应用设置候选窗首显策略（compat.toml 的 first_show_mode）。
    /// 参数：0=wait 1=fast 2=instant。三档互斥，UI 上呈现为子菜单单选。
    FirstShowMode(u8),
    /// 为当前焦点应用设置初始中英状态（compat.toml 的 initial_mode）。
    /// 参数：0=跟随全局（清除规则）1=英文 2=中文。
    InitialMode(u8),
    /// 为当前焦点应用设置初始中英标点（compat.toml 的 initial_punct）。
    /// 参数同 [`MenuCmd::InitialMode`]。
    InitialPunct(u8),
    /// 为当前焦点应用设置符号自动配对开关（compat.toml 的 auto_pair）。
    /// 参数：0=跟随全局（清除规则）1=启用 2=禁用。
    ///
    /// 「禁用」主要给表格类宿主用：Excel / WPS 表格在「输入态」下把方向键解释成
    /// 「确认单元格并移动」，配对后的光标回退无法实现（TSF 路线已实测失败）。
    AutoPairRule(u8),
    /// 语言栏图标（**Dev 变体专属调试菜单**）：切换标点角标的编码方式。
    /// 参数为 `wind_ui::langbar_icon::BadgeShape::ALL` 的下标。
    ///
    /// 存在的意义：16×16 上哪种编码可辨只能真机看，而每换一种就部署一次要提权 + 重启
    /// 输入法，成本高到根本比不动。渲染搬到服务端后形状本就是运行时参数，把它接到菜单上，
    /// 比选就退化成点几下。**不持久化**——调试项，重启回到默认。
    IconBadgeShape(u8),
    /// 语言栏图标（Dev 调试）：角标彩色 / 与主字同色跟随主题。
    IconToggleColors,
    /// 语言栏图标（Dev 调试）：在各尺寸档位图左上角烧尺寸标记，
    /// 用于真机确认系统实际取用了哪一档、有没有被二次缩放。
    IconToggleSizeMarks,
    /// 语言栏图标：全角状态的右上角标记开关。
    ///
    /// 与标点角标形状是**两个正交的量**，故单列而非并进那个单选组——它不是"第七种形状"。
    IconToggleWidthMark,
    /// 语言栏图标（Dev 调试）：外圈跑马灯演示动画。
    ///
    /// 与上面三项不同，它**不持久化**——那三项是「图标长什么样」的偏好，它是一段持续
    /// 占用 CPU 与 IPC 的演示，重启后自己关掉才是对的默认。
    IconToggleDemoAnim,
}

/// 菜单项的动作类型（右键候选菜单 + 功能主菜单共用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    /// 词条操作（置顶/移动/删除/恢复）
    Op(CandidateOp),
    /// 复制候选文本（UI 侧写剪贴板）
    Copy,
    /// 功能主菜单命令
    Command(MenuCmd),
    /// 子菜单父项（点击/回车进入 children）
    Submenu,
    /// 分隔线（不可点击）
    Separator,
    /// 纯展示的文本行（不可点击，不画分隔线）。用于在子菜单顶部显示上下文信息
    /// （如当前焦点进程名），型别本身保证它永远不会触发任何动作——`enabled` 恒
    /// 由构造函数按 false 写死已经能挡住 `selectable()` 那道闸门，这里在
    /// `to_menu_id`/`menu_action` 两处再补一道静态保证，双保险防的是「万一以后
    /// 有人手滑把 enabled 传成 true」。
    Label,
}

impl MenuKind {
    /// 稳定菜单 id：macOS `.app` 把它写进 `NSMenuItem.tag`，选中后经 `CmdMenuAction`
    /// 原样回传，Rust 据此还原动作。构建菜单树（下发）与处理回传（还原）共用此映射，
    /// 二者必须一致。`Submenu`/`Separator`/`Label` 不回传，恒为 0。
    /// id 区间：1 复制｜10-19 词条操作｜100-199 固定命令｜1000+ 方案｜2000+ 主题｜3000+ 过滤｜
    /// 4000+ 明暗｜5000+ 候选窗首显｜6000+ 初始中英｜7000+ 初始标点｜8000+ 诊断 HUD 分区｜
    /// 9000+ 自动配对｜10000+ 语言栏图标角标形状。
    pub fn to_menu_id(self) -> i32 {
        match self {
            MenuKind::Separator | MenuKind::Submenu | MenuKind::Label => 0,
            MenuKind::Copy => 1,
            MenuKind::Op(op) => match op {
                CandidateOp::MoveTop => 10,
                CandidateOp::MoveUp => 11,
                CandidateOp::MoveDown => 12,
                CandidateOp::Delete => 13,
                CandidateOp::Reset => 14,
                CandidateOp::ToggleCommon => 15,
            },
            MenuKind::Command(cmd) => match cmd {
                MenuCmd::SchemaEnglish => 100,
                MenuCmd::TogglePunct => 101,
                MenuCmd::ToggleWidth => 102,
                MenuCmd::ToggleS2t => 103,
                MenuCmd::ToggleToolbar => 104,
                MenuCmd::ReloadConfig => 105,
                MenuCmd::RestartService => 106,
                MenuCmd::OpenConfigDir => 107,
                MenuCmd::OpenDictionary => 108,
                MenuCmd::OpenSettings => 109,
                MenuCmd::OpenAbout => 110,
                MenuCmd::TakeScreenshot => 111,
                MenuCmd::ScreenshotCandidateToClipboard => 112,
                MenuCmd::OpenAppDir => 113,
                MenuCmd::OpenLogDir => 114,
                MenuCmd::StatusToggleAlways => 115,
                MenuCmd::StatusResetPosition => 116,
                MenuCmd::StatusScreenshot => 117,
                MenuCmd::TooltipCopy => 118,
                MenuCmd::TooltipScreenshot => 119,
                MenuCmd::StatusTogglePinned => 122,
                MenuCmd::StatusToggleShowOnFocus => 123,
                MenuCmd::ToggleInputDiagnostics => 120,
                MenuCmd::TogglePasswordSuppress => 121,
                MenuCmd::InputDiagCopy => 124,
                MenuCmd::InputDiagToggleFreeze => 125,
                MenuCmd::InputDiagToggleTopmost => 126,
                MenuCmd::IconToggleColors => 127,
                MenuCmd::IconToggleSizeMarks => 128,
                MenuCmd::IconToggleDemoAnim => 129,
                MenuCmd::IconToggleWidthMark => 130,
                MenuCmd::ToggleSoftKeyboard => 131,
                MenuCmd::IconBadgeShape(i) => 10000 + i as i32,
                MenuCmd::SoftKeyboardPage(i) => 11000 + i as i32,
                MenuCmd::InputDiagToggleSection(i) => 8000 + i as i32,
                MenuCmd::FirstShowMode(m) => 5000 + m as i32,
                MenuCmd::InitialMode(m) => 6000 + m as i32,
                MenuCmd::InitialPunct(m) => 7000 + m as i32,
                MenuCmd::AutoPairRule(m) => 9000 + m as i32,
                MenuCmd::SchemaSelect(i) => 1000 + i as i32,
                MenuCmd::ThemeSelect(i) => 2000 + i as i32,
                MenuCmd::FilterMode(i) => 3000 + i as i32,
                MenuCmd::ThemeStyle(s) => 4000 + s as i32,
            },
        }
    }

    /// 由回传的菜单 id 还原动作；未知 id / 不可点击项返回 None。
    pub fn from_menu_id(id: i32) -> Option<MenuKind> {
        let cmd = match id {
            1 => return Some(MenuKind::Copy),
            10 => return Some(MenuKind::Op(CandidateOp::MoveTop)),
            11 => return Some(MenuKind::Op(CandidateOp::MoveUp)),
            12 => return Some(MenuKind::Op(CandidateOp::MoveDown)),
            13 => return Some(MenuKind::Op(CandidateOp::Delete)),
            14 => return Some(MenuKind::Op(CandidateOp::Reset)),
            15 => return Some(MenuKind::Op(CandidateOp::ToggleCommon)),
            100 => MenuCmd::SchemaEnglish,
            101 => MenuCmd::TogglePunct,
            102 => MenuCmd::ToggleWidth,
            103 => MenuCmd::ToggleS2t,
            104 => MenuCmd::ToggleToolbar,
            105 => MenuCmd::ReloadConfig,
            106 => MenuCmd::RestartService,
            107 => MenuCmd::OpenConfigDir,
            108 => MenuCmd::OpenDictionary,
            109 => MenuCmd::OpenSettings,
            110 => MenuCmd::OpenAbout,
            111 => MenuCmd::TakeScreenshot,
            112 => MenuCmd::ScreenshotCandidateToClipboard,
            113 => MenuCmd::OpenAppDir,
            114 => MenuCmd::OpenLogDir,
            115 => MenuCmd::StatusToggleAlways,
            116 => MenuCmd::StatusResetPosition,
            117 => MenuCmd::StatusScreenshot,
            118 => MenuCmd::TooltipCopy,
            119 => MenuCmd::TooltipScreenshot,
            122 => MenuCmd::StatusTogglePinned,
            123 => MenuCmd::StatusToggleShowOnFocus,

            120 => MenuCmd::ToggleInputDiagnostics,
            121 => MenuCmd::TogglePasswordSuppress,
            124 => MenuCmd::InputDiagCopy,
            125 => MenuCmd::InputDiagToggleFreeze,
            126 => MenuCmd::InputDiagToggleTopmost,
            127 => MenuCmd::IconToggleColors,
            128 => MenuCmd::IconToggleSizeMarks,
            129 => MenuCmd::IconToggleDemoAnim,
            130 => MenuCmd::IconToggleWidthMark,
            131 => MenuCmd::ToggleSoftKeyboard,
            10000..=10099 => MenuCmd::IconBadgeShape((id - 10000) as u8),
            11000..=11999 => MenuCmd::SoftKeyboardPage((id - 11000) as usize),
            8000..=8999 => MenuCmd::InputDiagToggleSection((id - 8000) as u8),
            1000..=1999 => MenuCmd::SchemaSelect((id - 1000) as usize),
            2000..=2999 => MenuCmd::ThemeSelect((id - 2000) as usize),
            3000..=3999 => MenuCmd::FilterMode((id - 3000) as usize),
            4000..=4999 => MenuCmd::ThemeStyle((id - 4000) as u8),
            5000..=5999 => MenuCmd::FirstShowMode((id - 5000) as u8),
            6000..=6999 => MenuCmd::InitialMode((id - 6000) as u8),
            7000..=7999 => MenuCmd::InitialPunct((id - 7000) as u8),
            9000..=9999 => MenuCmd::AutoPairRule((id - 9000) as u8),
            _ => return None,
        };
        Some(MenuKind::Command(cmd))
    }
}

/// 菜单相对锚点的展开方向。
///
/// 取代早先的 `above: bool`——加入侧向后就是三态，布尔位表达不了，硬塞会变成
/// `above`/`side` 两个互斥布尔而类型不作担保。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuPlacement {
    /// 顶边贴锚点顶边向下展开（光标处右键：候选/状态泡/Tooltip/诊断 HUD）。
    Below,
    /// 底边贴锚点顶边向上展开；上方装不下则翻到锚点底边之下。
    /// 横向工具栏用，避免菜单压住工具栏本身。
    Above,
    /// 贴锚点侧边展开：右侧装得下走右侧，否则走左侧。
    /// 纵向工具栏用——竖条上仍向上弹会让菜单飘到条顶之上老远。
    Side,
}

/// 菜单锚点：屏幕坐标矩形 + 展开方向。
///
/// 聚合成一个类型而非散落的 `x/y/right/bottom/placement` 五个参数：**哪些边参与定位是
/// 由 `placement` 决定的**（`Below` 只看左上、`Above` 还要下边、`Side` 还要右边），
/// 这份知识只有收在一处才不会在某个调用点被漏填成 0 而静默错位。
#[derive(Debug, Clone, Copy)]
pub struct MenuAnchor {
    /// 锚点左边（`i32::MIN` = 由 UI 取当前光标位，此时其余边同样退化为该点）。
    pub x: i32,
    /// 锚点上边。
    pub y: i32,
    /// 锚点右边（仅 `Side` 使用）。
    pub right: i32,
    /// 锚点下边（仅 `Above` 的翻转回退使用）。
    pub bottom: i32,
    pub placement: MenuPlacement,
}

impl MenuAnchor {
    /// 点状锚点，向下展开。`i32::MIN` 表示取光标位。
    pub fn at_point(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            right: x,
            bottom: y,
            placement: MenuPlacement::Below,
        }
    }

    /// 矩形锚点，向上展开（横向工具栏）。
    pub fn above_rect(x: i32, y: i32, bottom: i32) -> Self {
        Self {
            x,
            y,
            right: x,
            bottom,
            placement: MenuPlacement::Above,
        }
    }

    /// 矩形锚点，侧向展开（纵向工具栏）。
    pub fn beside_rect(x: i32, y: i32, right: i32, bottom: i32) -> Self {
        Self {
            x,
            y,
            right,
            bottom,
            placement: MenuPlacement::Side,
        }
    }
}

/// 菜单项规格（由协调器构建）。支持勾选态与子菜单。
///
/// `PartialEq` 供弹出菜单的增量重绘用：`popup_menu::reconcile`（wind-ui）靠它判断
/// 某一层的内容是否真的变了，没变就不重绘、更不重排 z 序。
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItemSpec {
    pub label: String,
    pub kind: MenuKind,
    pub enabled: bool,
    /// 勾选标记（当前方案/主题/开关态）
    pub checked: bool,
    /// 子菜单项（kind=Submenu 时有效）
    pub children: Vec<MenuItemSpec>,
}

impl MenuItemSpec {
    pub fn leaf(label: impl Into<String>, kind: MenuKind, enabled: bool, checked: bool) -> Self {
        Self {
            label: label.into(),
            kind,
            enabled,
            checked,
            children: Vec::new(),
        }
    }
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            kind: MenuKind::Separator,
            enabled: false,
            checked: false,
            children: Vec::new(),
        }
    }
    /// 纯展示的文本行，用作子菜单顶部的上下文标题（如「当前应用：xxx.exe」）。
    pub fn label(text: impl Into<String>) -> Self {
        Self {
            label: text.into(),
            kind: MenuKind::Label,
            enabled: false,
            checked: false,
            children: Vec::new(),
        }
    }
    pub fn submenu(label: impl Into<String>, children: Vec<MenuItemSpec>) -> Self {
        Self {
            label: label.into(),
            kind: MenuKind::Submenu,
            enabled: true,
            checked: false,
            children,
        }
    }
}
