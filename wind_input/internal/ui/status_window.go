package ui

import (
	"log/slog"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
	"unsafe"

	"github.com/huanfeng/wind_input/pkg/theme"
	"golang.org/x/sys/windows"
)

var (
	procGetForegroundWindow = user32.NewProc("GetForegroundWindow")
	procGetWindowRect       = user32.NewProc("GetWindowRect")
)

// StatusDisplayMode 状态指示器显示模式
type StatusDisplayMode string

const (
	// StatusDisplayModeTemp 临时显示模式（切换时短暂显示后自动隐藏）
	StatusDisplayModeTemp StatusDisplayMode = "temp"
	// StatusDisplayModeAlways 常驻显示模式（始终跟随光标显示）
	StatusDisplayModeAlways StatusDisplayMode = "always"
)

// StatusPositionMode 状态指示器位置模式
type StatusPositionMode string

const (
	// StatusPositionFollowCaret 跟随光标位置
	StatusPositionFollowCaret StatusPositionMode = "follow_caret"
	// StatusPositionCustom 固定自定义位置
	StatusPositionCustom StatusPositionMode = "custom"
)

// StatusState 状态指示器当前状态
type StatusState struct {
	ModeLabel  string // 输入模式标签（如 "中", "英", "拼", "五"）
	PunctLabel string // 标点状态标签（如 "，", ","）
	WidthLabel string // 全半角标签（如 "全", "半"）
}

// StatusWindowConfig 状态指示器窗口运行时配置
type StatusWindowConfig struct {
	Enabled         bool               // 是否启用
	DisplayMode     StatusDisplayMode  // 显示模式
	Duration        int                // 临时显示持续时间（毫秒）
	SchemaNameStyle string             // 方案名称风格
	ShowMode        bool               // 显示输入模式
	ShowPunct       bool               // 显示标点状态
	ShowFullWidth   bool               // 显示全半角状态
	PositionMode    StatusPositionMode // 位置模式
	OffsetX         int                // 跟随光标时的 X 偏移
	OffsetY         int                // 跟随光标时的 Y 偏移
	CustomX         int                // 自定义位置 X
	CustomY         int                // 自定义位置 Y
	FontSize        float64            // 字体大小
	Opacity         float64            // 背景透明度 (0.0-1.0)
	BackgroundColor string             // 自定义背景色 (#RRGGBB)
	TextColor       string             // 自定义文本色 (#RRGGBB)
	BorderRadius    float64            // 圆角半径
}

// StatusMenuAction 状态指示器右键菜单动作
type StatusMenuAction int

const (
	StatusMenuSwitchToAlways StatusMenuAction = iota // 切换到常驻模式
	StatusMenuSwitchToTemp                           // 切换到临时模式
	StatusMenuSettings                               // 打开设置
	StatusMenuHide                                   // 隐藏
)

// 菜单项 ID 常量
const (
	idmStatusSwitchMode = 2001
	idmStatusSettings   = 2002
	idmStatusHide       = 2003
)

// StatusWindow 状态指示器独立窗口
type StatusWindow struct {
	hwnd   windows.HWND
	logger *slog.Logger

	mu      sync.Mutex
	visible bool
	x, y    int
	width   int
	height  int

	state  StatusState
	config StatusWindowConfig

	hideVersion atomic.Uint64 // 用于取消待执行的隐藏定时器

	mouseHovering bool // 鼠标是否悬停在窗口上
	trackingMouse bool // 是否已启用鼠标追踪

	// 拖动状态
	dragging    bool
	dragStartX  int
	dragStartY  int
	dragOffsetX int
	dragOffsetY int

	// 常驻模式下的相对位置偏移（相对于前台窗口左上角）
	relOffsetX   int  // 相对偏移 X
	relOffsetY   int  // 相对偏移 Y
	hasRelOffset bool // 是否有用户自定义的相对偏移

	popupMenu    *PopupMenu
	menuCallback func(action StatusMenuAction)

	renderer *StatusRenderer

	resolvedTheme *theme.ResolvedTheme

	hostRenderFunc func(x, y int) error // 宿主渲染回调（由 Manager 设置）
	hostHideFunc   func()               // 宿主隐藏回调
}

// 全局状态窗口注册表
var statusWindows = NewWindowRegistry[StatusWindow]()

// statusWndProc 状态窗口的消息处理过程
func statusWndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	switch msg {
	case WM_DESTROY:
		statusWindows.Unregister(windows.HWND(hwnd))
		return 0

	case WM_NCHITTEST:
		return HTCLIENT

	case WM_SETCURSOR:
		w := statusWindows.Get(windows.HWND(hwnd))
		if w != nil && w.isDragging() {
			cursor, _, _ := procLoadCursorW.Call(0, IDC_SIZEALL)
			if cursor != 0 {
				procSetCursor.Call(cursor)
			}
			return 1
		}
		cursor, _, _ := procLoadCursorW.Call(0, IDC_ARROW)
		if cursor != 0 {
			procSetCursor.Call(cursor)
		}
		return 1

	case WM_LBUTTONDOWN:
		w := statusWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.handleDragStart(lParam)
		}
		return 0

	case WM_LBUTTONUP:
		w := statusWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.handleDragEnd()
		}
		return 0

	case WM_MOUSEMOVE:
		w := statusWindows.Get(windows.HWND(hwnd))
		if w != nil {
			if w.isDragging() {
				w.handleDragMove(lParam)
			} else {
				w.handleMouseMove()
			}
		}
		return 0

	case WM_MOUSELEAVE:
		w := statusWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.handleMouseLeave()
		}
		return 0

	case WM_RBUTTONUP:
		w := statusWindows.Get(windows.HWND(hwnd))
		if w != nil {
			w.handleRightClick(lParam)
		}
		return 0

	case WM_DPICHANGED:
		// DPI 变化时无需特殊处理，下次渲染会自动使用新的 DPI 缩放
		return 0
	}

	ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
	return ret
}

// NewStatusWindow 创建状态指示器窗口实例
func NewStatusWindow(logger *slog.Logger) *StatusWindow {
	w := &StatusWindow{
		logger:   logger,
		renderer: NewStatusRenderer(logger),
		config: StatusWindowConfig{
			Enabled:       true,
			DisplayMode:   StatusDisplayModeTemp,
			Duration:      1500,
			ShowMode:      true,
			ShowPunct:     false,
			ShowFullWidth: false,
			PositionMode:  StatusPositionFollowCaret,
			OffsetX:       4,
			OffsetY:       4,
			FontSize:      14.0,
			Opacity:       0.9,
			BorderRadius:  4.0,
		},
	}
	return w
}

// Create 创建状态窗口（必须在 UI 线程调用）
func (w *StatusWindow) Create() error {
	hwnd, err := CreateLayeredWindow(LayeredWindowConfig{
		ClassName: "IMEStatusWindow",
		WndProc:   syscall.NewCallback(statusWndProc),
	})
	if err != nil {
		return err
	}

	w.hwnd = hwnd
	statusWindows.Register(w.hwnd, w)

	// 创建右键菜单
	w.popupMenu = NewPopupMenu()
	if err := w.popupMenu.Create(); err != nil {
		w.logger.Warn("创建状态窗口右键菜单失败", "error", err)
	}

	w.logger.Debug("状态指示器窗口已创建", "hwnd", w.hwnd)
	return nil
}

// getForegroundWindowPos 获取前台窗口的左上角屏幕坐标
func getForegroundWindowPos() (x, y int, ok bool) {
	hwnd, _, _ := procGetForegroundWindow.Call()
	if hwnd == 0 {
		return 0, 0, false
	}
	var rect RECT
	ret, _, _ := procGetWindowRect.Call(hwnd, uintptr(unsafe.Pointer(&rect)))
	if ret == 0 {
		return 0, 0, false
	}
	return int(rect.Left), int(rect.Top), true
}

// Show 显示状态窗口（不激活）
func (w *StatusWindow) Show(x, y int) {
	if w.hwnd == 0 {
		return
	}

	w.mu.Lock()
	if w.config.DisplayMode == StatusDisplayModeAlways {
		// 常驻模式：使用相对于前台窗口的偏移位置
		if fwX, fwY, ok := getForegroundWindowPos(); ok {
			if w.hasRelOffset {
				x = fwX + w.relOffsetX
				y = fwY + w.relOffsetY
			} else {
				// 默认位置：前台窗口左上角 + 小偏移
				x = fwX + 10
				y = fwY + 10
			}
		}
		// 如果获取前台窗口失败，使用传入的 x, y（光标位置）
	}
	w.visible = true
	w.x = x
	w.y = y
	w.mu.Unlock()

	// 渲染状态图像
	img := w.renderer.Render(w.state, w.config)
	if img == nil {
		return
	}

	w.mu.Lock()
	w.width = img.Bounds().Dx()
	w.height = img.Bounds().Dy()
	w.mu.Unlock()

	// 更新分层窗口
	if err := UpdateLayeredWindowFromImage(w.hwnd, img, x, y); err != nil {
		w.logger.Warn("更新状态窗口失败", "error", err)
		return
	}

	procShowWindow.Call(uintptr(w.hwnd), SW_SHOWNA)
}

// Hide 隐藏状态窗口
func (w *StatusWindow) Hide() {
	if w.hwnd == 0 {
		return
	}

	// 同时隐藏弹出菜单
	if w.popupMenu != nil && w.popupMenu.IsVisible() {
		w.popupMenu.Hide()
	}

	procShowWindow.Call(uintptr(w.hwnd), SW_HIDE)

	w.mu.Lock()
	w.visible = false
	w.mu.Unlock()
}

// IsVisible 返回窗口是否可见
func (w *StatusWindow) IsVisible() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.visible
}

// UpdateContent 更新状态内容并重新渲染
func (w *StatusWindow) UpdateContent(state StatusState) {
	w.mu.Lock()
	w.state = state
	visible := w.visible
	x, y := w.x, w.y
	w.mu.Unlock()

	if !visible || w.hwnd == 0 {
		return
	}

	// 重新渲染
	img := w.renderer.Render(state, w.config)
	if img == nil {
		return
	}

	w.mu.Lock()
	w.width = img.Bounds().Dx()
	w.height = img.Bounds().Dy()
	w.mu.Unlock()

	if err := UpdateLayeredWindowFromImage(w.hwnd, img, x, y); err != nil {
		w.logger.Warn("更新状态窗口内容失败", "error", err)
	}
}

// SetConfig 设置运行时配置
func (w *StatusWindow) SetConfig(cfg StatusWindowConfig) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.config = cfg
}

// GetConfig 获取当前配置
func (w *StatusWindow) GetConfig() StatusWindowConfig {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.config
}

// SetTheme 设置主题
func (w *StatusWindow) SetTheme(resolved *theme.ResolvedTheme) {
	w.mu.Lock()
	w.resolvedTheme = resolved
	w.mu.Unlock()

	w.renderer.SetTheme(resolved)
	if w.popupMenu != nil {
		w.popupMenu.SetTheme(resolved)
	}
}

// SetFontPath 设置字体路径
func (w *StatusWindow) SetFontPath(path string) {
	w.renderer.SetFontPath(path)
	if w.popupMenu != nil {
		w.popupMenu.SetFontPath(path)
	}
}

// SetTextRenderMode 设置文本渲染模式
func (w *StatusWindow) SetTextRenderMode(mode TextRenderMode) {
	w.renderer.SetTextRenderMode(mode)
	if w.popupMenu != nil {
		w.popupMenu.SetTextRenderMode(mode)
	}
}

// SetGDIFontParams 设置 GDI 字体参数
func (w *StatusWindow) SetGDIFontParams(weight int, scale float64) {
	w.renderer.SetGDIFontParams(weight, scale)
}

// SetMenuFontParams 设置菜单字体参数
func (w *StatusWindow) SetMenuFontParams(weight int, scale float64) {
	if w.popupMenu != nil {
		w.popupMenu.SetGDIFontParams(weight, scale)
	}
}

// SetMenuFontSize 设置菜单字体大小
func (w *StatusWindow) SetMenuFontSize(size float64) {
	if w.popupMenu != nil {
		w.popupMenu.SetMenuFontSize(size)
	}
}

// SetHostRenderFunc 设置宿主渲染回调
func (w *StatusWindow) SetHostRenderFunc(fn func(x, y int) error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.hostRenderFunc = fn
}

// SetHostHideFunc 设置宿主隐藏回调
func (w *StatusWindow) SetHostHideFunc(fn func()) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.hostHideFunc = fn
}

// SetMenuCallback 设置菜单回调
func (w *StatusWindow) SetMenuCallback(cb func(action StatusMenuAction)) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.menuCallback = cb
}

// Destroy 销毁状态窗口
func (w *StatusWindow) Destroy() {
	if w.popupMenu != nil {
		w.popupMenu.Destroy()
		w.popupMenu = nil
	}
	if w.hwnd != 0 {
		procDestroyWindow.Call(uintptr(w.hwnd))
		w.hwnd = 0
	}
	if w.renderer != nil {
		w.renderer.Close()
		w.renderer = nil
	}
}

// handleMouseMove 处理鼠标移动事件（暂停自动隐藏）
func (w *StatusWindow) handleMouseMove() {
	w.mu.Lock()
	w.mouseHovering = true
	needTrack := !w.trackingMouse
	w.trackingMouse = true
	w.mu.Unlock()

	if needTrack {
		tme := TRACKMOUSEEVENT{
			CbSize:    uint32(unsafe.Sizeof(TRACKMOUSEEVENT{})),
			DwFlags:   TME_LEAVE,
			HwndTrack: uintptr(w.hwnd),
		}
		procTrackMouseEvent.Call(uintptr(unsafe.Pointer(&tme)))
	}
}

// handleMouseLeave 处理鼠标离开事件
func (w *StatusWindow) handleMouseLeave() {
	w.mu.Lock()
	w.mouseHovering = false
	w.trackingMouse = false
	mode := w.config.DisplayMode
	w.mu.Unlock()

	// 临时模式下鼠标离开后重新开始隐藏计时
	if mode == StatusDisplayModeTemp {
		w.scheduleHide()
	}
}

// handleDragStart 开始拖动
func (w *StatusWindow) handleDragStart(lParam uintptr) {
	clientX := int(int16(lParam & 0xFFFF))
	clientY := int(int16((lParam >> 16) & 0xFFFF))

	w.mu.Lock()
	w.dragging = true
	w.dragStartX = clientX
	w.dragStartY = clientY
	w.dragOffsetX = 0
	w.dragOffsetY = 0
	w.mu.Unlock()

	// 捕获鼠标以追踪窗口外的移动
	procSetCapture.Call(uintptr(w.hwnd))
}

// handleDragMove 拖动中
func (w *StatusWindow) handleDragMove(lParam uintptr) {
	clientX := int(int16(lParam & 0xFFFF))
	clientY := int(int16((lParam >> 16) & 0xFFFF))

	w.mu.Lock()
	dx := clientX - w.dragStartX
	dy := clientY - w.dragStartY
	newX := w.x + dx
	newY := w.y + dy
	w.x = newX
	w.y = newY
	w.mu.Unlock()

	// 移动窗口
	procSetWindowPos.Call(
		uintptr(w.hwnd),
		HWND_TOPMOST,
		uintptr(newX), uintptr(newY),
		0, 0,
		SWP_NOSIZE|SWP_NOACTIVATE,
	)
}

// handleDragEnd 结束拖动
func (w *StatusWindow) handleDragEnd() {
	procReleaseCapture.Call()

	w.mu.Lock()
	w.dragging = false
	finalX := w.x
	finalY := w.y
	mode := w.config.DisplayMode
	w.mu.Unlock()

	if mode == StatusDisplayModeAlways {
		// 计算相对于前台窗口的偏移
		if fwX, fwY, ok := getForegroundWindowPos(); ok {
			w.mu.Lock()
			w.relOffsetX = finalX - fwX
			w.relOffsetY = finalY - fwY
			w.hasRelOffset = true
			w.mu.Unlock()
			w.logger.Debug("状态窗口拖动偏移已更新", "relX", finalX-fwX, "relY", finalY-fwY)
		}
	}
}

// isDragging 返回是否正在拖动
func (w *StatusWindow) isDragging() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.dragging
}

// ResetDragPosition 重置拖动位置（临时模式下每次显示时调用）
func (w *StatusWindow) ResetDragPosition() {
	w.mu.Lock()
	w.hasRelOffset = false
	w.relOffsetX = 0
	w.relOffsetY = 0
	w.mu.Unlock()
}

// GetDraggedPosition 获取拖动后的相对偏移（常驻模式下使用）
func (w *StatusWindow) GetDraggedPosition() (x, y int, hasDragged bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.relOffsetX, w.relOffsetY, w.hasRelOffset
}

// handleRightClick 处理右键点击事件
func (w *StatusWindow) handleRightClick(lParam uintptr) {
	if w.popupMenu == nil {
		return
	}

	screenX, screenY := w.getScreenPos(lParam)

	w.mu.Lock()
	mode := w.config.DisplayMode
	cb := w.menuCallback
	w.mu.Unlock()

	// 构建菜单项
	var items []MenuItem
	if mode == StatusDisplayModeTemp {
		items = append(items, MenuItem{ID: idmStatusSwitchMode, Text: "常驻显示 (beta)"})
	} else {
		items = append(items, MenuItem{ID: idmStatusSwitchMode, Text: "临时显示"})
	}
	items = append(items, MenuItem{Separator: true})
	items = append(items, MenuItem{ID: idmStatusSettings, Text: "设置..."})
	items = append(items, MenuItem{ID: idmStatusHide, Text: "隐藏"})

	w.popupMenu.Show(items, screenX, screenY, func(id int) {
		// 菜单关闭后，临时模式下重新启动隐藏倒计时
		if mode == StatusDisplayModeTemp {
			w.scheduleHide()
		}

		if cb == nil || id == 0 {
			return
		}
		switch id {
		case idmStatusSwitchMode:
			if mode == StatusDisplayModeTemp {
				cb(StatusMenuSwitchToAlways)
			} else {
				cb(StatusMenuSwitchToTemp)
			}
		case idmStatusSettings:
			cb(StatusMenuSettings)
		case idmStatusHide:
			cb(StatusMenuHide)
		}
	})
}

// CancelPendingHide 取消所有待执行的隐藏定时器（切换到常驻模式时调用）
func (w *StatusWindow) CancelPendingHide() {
	w.hideVersion.Add(1)
}

// scheduleHide 在配置的延迟后隐藏窗口（可被新的调用取消）
func (w *StatusWindow) scheduleHide() {
	version := w.hideVersion.Add(1)

	w.mu.Lock()
	duration := w.config.Duration
	w.mu.Unlock()

	if duration <= 0 {
		duration = 1500
	}

	go func() {
		time.Sleep(time.Duration(duration) * time.Millisecond)

		// 检查版本号是否匹配（未被新的调用取消）
		if w.hideVersion.Load() != version {
			return
		}

		// 检查鼠标是否悬停或右键菜单是否显示
		w.mu.Lock()
		hovering := w.mouseHovering
		w.mu.Unlock()
		if hovering {
			return
		}
		// 右键菜单打开时不自动隐藏，避免破坏 SetCapture/键盘钩子全局状态
		if w.popupMenu != nil && w.popupMenu.IsVisible() {
			return
		}

		// 调用宿主隐藏函数或直接隐藏
		w.mu.Lock()
		hideFn := w.hostHideFunc
		w.mu.Unlock()

		if hideFn != nil {
			hideFn()
		} else {
			w.Hide()
		}
	}()
}

// getScreenPos 从 lParam 提取客户区坐标并转换为屏幕坐标
func (w *StatusWindow) getScreenPos(lParam uintptr) (int, int) {
	x := int(int16(lParam & 0xFFFF))
	y := int(int16((lParam >> 16) & 0xFFFF))
	pt := POINT{X: int32(x), Y: int32(y)}
	procClientToScreen.Call(uintptr(w.hwnd), uintptr(unsafe.Pointer(&pt)))
	return int(pt.X), int(pt.Y)
}
