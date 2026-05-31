package ui

import (
	"image/color"

	"github.com/huanfeng/wind_input/pkg/config"
)

// types_neutral.go 集中 ui 包内"平台无关的纯数据类型与枚举常量"。
//
// 历史上这些定义散落在 status_window.go / toast_renderer.go / toolbar_window.go /
// global_hotkey.go / popup_menu.go / monitor.go / manager.go 等 Win-only 文件中,
// 导致 darwin 构建无法编译。后把它们集中提到此文件 (无 windows / cgo / Win32 引用),
// 让 darwin stub 也能复用同一份类型定义, 避免在 *_darwin.go 中复刻一遍。
//
// 此文件**不能** import windows / unsafe / syscall 等平台相关包。

// ============================================================================
// Status indicator
// ============================================================================

// StatusDisplayMode 状态指示器显示模式
type StatusDisplayMode string

const (
	StatusDisplayModeTemp   StatusDisplayMode = "temp"
	StatusDisplayModeAlways StatusDisplayMode = "always"
)

// StatusPositionMode 状态指示器位置模式
type StatusPositionMode string

const (
	StatusPositionFollowCaret StatusPositionMode = "follow_caret"
	StatusPositionCustom      StatusPositionMode = "custom"
)

// StatusState 状态指示器当前状态
type StatusState struct {
	ModeLabel  string // 输入模式标签（如 "中", "英", "拼", "五"）
	PunctLabel string // 标点状态标签
	WidthLabel string // 全半角标签
}

// StatusWindowConfig 状态指示器窗口运行时配置
type StatusWindowConfig struct {
	Enabled         bool
	DisplayMode     StatusDisplayMode
	Duration        int
	SchemaNameStyle string
	ShowMode        bool
	ShowPunct       bool
	ShowFullWidth   bool
	PositionMode    StatusPositionMode
	OffsetX         int
	OffsetY         int
	CustomX         int
	CustomY         int
	FontSize        float64
	Opacity         float64
	BackgroundColor string
	TextColor       string
	BorderRadius    float64
}

// StatusMenuAction 状态指示器右键菜单动作
type StatusMenuAction int

const (
	StatusMenuSwitchToAlways StatusMenuAction = iota
	StatusMenuSwitchToTemp
	StatusMenuSettings
	StatusMenuHide
)

// ============================================================================
// Toast
// ============================================================================

// ToastLevel toast 强调级别
type ToastLevel int

const (
	ToastInfo    ToastLevel = iota // 蓝色 accent
	ToastSuccess                   // 绿色 accent
	ToastWarn                      // 橙色 accent
	ToastError                     // 红色 accent
)

// ToastPosition toast 落位策略
type ToastPosition int

const (
	ToastCenter      ToastPosition = iota
	ToastBottomRight               // 工作区右下角
	ToastTopRight                  // 预留
	ToastTop                       // 预留
)

// ToastOptions 描述一次 toast 展示请求
type ToastOptions struct {
	Title    string
	Message  string
	Level    ToastLevel
	Position ToastPosition
	Duration int // 自动隐藏毫秒数; 0=默认 5000; <0=不自动隐藏
	MaxWidth int // 内容最大像素宽 (DIP); 0=工作区一半
}

// ToastAccentColor 返回各 Level 对应的边框/标题强调色 (平台无关单一来源)。
// Win 端 toast_renderer.levelAccent 与 darwin forwarder 均复用本函数, 保证两端配色一致。
func ToastAccentColor(level ToastLevel) color.Color {
	switch level {
	case ToastSuccess:
		return color.RGBA{R: 0x4C, G: 0xAF, B: 0x50, A: 0xFF} // 绿
	case ToastWarn:
		return color.RGBA{R: 0xFF, G: 0x98, B: 0x00, A: 0xFF} // 琥珀
	case ToastError:
		return color.RGBA{R: 0xE5, G: 0x39, B: 0x35, A: 0xFF} // 红
	case ToastInfo:
		fallthrough
	default:
		return color.RGBA{R: 0x42, G: 0xA5, B: 0xF5, A: 0xFF} // 蓝
	}
}

// ============================================================================
// Toolbar
// ============================================================================

// ToolbarState 工具栏当前状态
type ToolbarState struct {
	ChineseMode   bool
	CapsLock      bool
	FullWidth     bool
	ChinesePunct  bool
	EffectiveMode int    // 0=Chinese, 1=EnglishLower, 2=EnglishUpper
	ModeLabel     string // Schema icon_label (如 "拼", "五", "双", "混")
}

// ToolbarCallback 工具栏交互回调集合
type ToolbarCallback struct {
	OnToggleMode                 func()
	OnToggleWidth                func()
	OnTogglePunct                func()
	OnOpenSettings               func()
	OnPositionChanged            func(x, y int)
	OnContextMenu                func(action ToolbarContextMenuAction)
	OnShowMenu                   func(screenX, screenY, flipRefY int)
	OnForegroundFullscreenChange func(enter bool)
}

// ToolbarContextMenuAction 工具栏右键菜单动作
type ToolbarContextMenuAction int

const (
	ToolbarMenuSettings ToolbarContextMenuAction = iota
	ToolbarMenuRestartService
	ToolbarMenuAbout
)

// ============================================================================
// Global hotkey
// ============================================================================

// GlobalHotkeyEntry 一项全局快捷键定义
type GlobalHotkeyEntry struct {
	ID        int    // Unique ID (1-based)
	Modifiers uint32 // hotkeyModControl / hotkeyModShift 等位掩码 (Win 平台 RegisterHotKey 语义)
	VK        uint32 // Virtual key code
	Command   string // Command name for callback dispatch
}

// ============================================================================
// Popup menu (ui-internal 菜单数据; 与 uicmd.MenuItem 不同, 这里是 Win 渲染端结构)
// ============================================================================

// MenuItem 弹出菜单的菜单项 (ui 内部渲染数据, 非 wire 形态)
type MenuItem struct {
	ID        int
	Text      string
	Disabled  bool
	Separator bool
	Checked   bool       // 勾选状态 (显示 ✓)
	Children  []MenuItem // 子菜单 (非空时显示 ▸, hover 展开)
}

// PopupMenuCallback 菜单项选中回调
type PopupMenuCallback func(id int)

// ============================================================================
// Candidate window layout / position
// ============================================================================

// CandidateLayout 候选窗布局方向
type CandidateLayout int

const (
	LayoutVertical   CandidateLayout = iota // 垂直
	LayoutHorizontal                        // 水平
)

// PositionPreference 候选窗位置偏好
type PositionPreference int

const (
	PositionAuto  PositionPreference = iota // 自动 (按屏幕边界)
	PositionAbove                           // 强制在 caret 上方
	PositionBelow                           // 强制在 caret 下方
)

// ============================================================================
// Unified menu (Manager.ShowUnifiedMenu 的状态与项目 ID)
// ============================================================================

// UnifiedMenu* 是统一右键菜单各项的项 ID。
//
// 字段含义见 manager.ShowUnifiedMenu 中各分支; 这些常量由 coordinator 直接引用
// 作为 BuildUnifiedMenuItems 的输入与回调 dispatch 的 case 标识, 故必须平台无关。
const (
	UnifiedMenuToggleWidth          = 101
	UnifiedMenuTogglePunct          = 102
	UnifiedMenuToggleToolbar        = 103
	UnifiedMenuToggleS2T            = 104 // 简入繁出 总开关
	UnifiedMenuSchemaEnglish        = 140 // 英文模式
	UnifiedMenuSchemaBase           = 150 // 方案ID: 150+i
	UnifiedMenuThemeBase            = 200 // 主题ID: 200+i
	UnifiedMenuThemeStyleBase       = 250 // 主题风格ID: 250+i (0=system, 1=light, 2=dark)
	UnifiedMenuFilterModeBase       = 260 // 检索范围ID: 260+i (0=smart, 1=general, 2=gb18030)
	UnifiedMenuS2TVariantBase       = 270 // 简入繁出 变体ID: 270+i (0=s2t, 1=s2tw, 2=s2twp, 3=s2hk)
	UnifiedMenuTestBase             = 280 // 三级菜单测试ID: 280+i
	UnifiedMenuTestToastInfo        = 290
	UnifiedMenuTestToastSuccess     = 291
	UnifiedMenuTestToastWarn        = 292
	UnifiedMenuTestToastError       = 293
	UnifiedMenuTestToastLongMessage = 294
	UnifiedMenuReloadConfig         = 299
	UnifiedMenuRestartService       = 303
	UnifiedMenuDictionary           = 300
	UnifiedMenuSettings             = 301
	UnifiedMenuAbout                = 302
	UnifiedMenuSkipCaretPending     = 304 // 为当前应用启用即时候选
	UnifiedMenuPinCandidatePosition = 305 // 为当前应用启用固定候选位置
)

// ThemeMenuItem 统一菜单中主题项
type ThemeMenuItem struct {
	ID          string // Theme ID (如 "default")
	DisplayName string // 显示名 (如 "默认主题 1.0")
}

// SchemaMenuItem 统一菜单中方案项
type SchemaMenuItem struct {
	ID   string // Schema ID
	Name string // 显示名
}

// UnifiedMenuState 构建统一菜单需要的全部状态
type UnifiedMenuState struct {
	ChineseMode          bool
	FullWidth            bool
	ChinesePunct         bool
	ToolbarVisible       bool
	Schemas              []SchemaMenuItem
	CurrentSchemaID      string
	CurrentFilterMode    config.FilterMode
	Themes               []ThemeMenuItem
	CurrentThemeID       string
	CurrentThemeStyle    config.ThemeStyle
	Version              string
	ActiveProcessName    string
	SkipCaretPending     bool
	PinCandidatePosition bool
	S2TEnabled           bool
	S2TVariant           config.S2TVariant

	// 平台裁剪 (darwin 置 true): macOS 无浮动工具栏概念, 也不需要 Win 的
	// caret-pending / pin-position 兼容性高级项。
	OmitToolbarToggle bool
	OmitAdvanced      bool
}
