//go:build windows

package ui

import (
	"image"
	"image/color"
	"log/slog"
	"runtime"
	"sync"

	"github.com/huanfeng/wind_input/internal/uicmd"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
	"golang.org/x/sys/windows"
)

// UnifiedMenu* 常量 / ThemeMenuItem / SchemaMenuItem / UnifiedMenuState 已迁至
// types_neutral.go (平台无关), darwin stub 复用同一定义。

// (UICommand 大 struct 已迁移到 internal/uicmd 包的按命令类型拆分 Payload;
//  channel 元素类型见 events.go 的 uicmdItem。)

// Manager manages the candidate window UI
type Manager struct {
	window       *CandidateWindow
	renderer     *Renderer
	logger       *slog.Logger
	themeManager *theme.Manager

	// Toolbar window
	toolbar *ToolbarWindow

	// Tooltip window for encoding lookup
	tooltip *TooltipWindow

	// 独立的状态提示窗口
	status *StatusWindow

	// 独立的 Toast 通知窗口（错误、词库就绪等一次性通知）
	toast *ToastWindow

	mu                  sync.Mutex
	candidates          []Candidate
	input               string
	cursorPos           int
	page                int
	totalPages          int
	totalCandidateCount int
	candidatesPerPage   int
	selectedIndex       int         // 当前页内选中的候选索引
	isPinyinMode        bool        // 是否拼音模式（控制右键菜单前移/后移禁用）
	isQuickInputMode    bool        // 是否快捷输入模式（右键菜单只保留复制）
	modeLabel           string      // 临时模式标签（如"临时拼音"、"快捷输入"），空=不显示
	modeAccentColor     color.Color // 特殊模式内发光边框颜色，nil=不显示
	caretX              int
	caretY              int
	caretHeight         int

	// Sticky position state: once candidate window jumps above caret,
	// it stays above until input is cleared (new input session)
	stickyAbove bool

	// Input session version: incremented on each commit/hide to prevent
	// stale show commands from reappearing the candidate window
	inputSession        uint64
	currentInputSession uint64 // The session being displayed (for UI thread)

	ready   bool
	readyCh chan struct{}

	// Command channel for async UI updates
	cmdCh chan uicmdItem

	// Event channel for async UI events (UI → coordinator/forwarder).
	// 由 SetXxxCallback 内部包装时同时推一份 uicmd.Event, 供 macOS forwarder 订阅。
	// Win 端的 callback 触发链路不变 (双流并行)。
	eventCh chan uicmd.Event

	// Event to wake up the message loop when commands are available
	cmdEvent windows.Handle

	// Toolbar callbacks (set by coordinator)
	toolbarCallbacks *ToolbarCallback

	// Candidate window callbacks (for mouse interaction)
	candidateCallbacks *CandidateCallback

	// 「固定候选位置」规则的运行态：由 coordinator 在焦点切换、菜单 toggle、拖动落盘后推送。
	// appPinEnabled=false 时 doShowCandidates 走常规路径；true 时按 caret 所在显示器从 map 取位置。
	// appPinPositions: key = MonitorKeyStr(workRight, workBottom)，value = [x, y]。
	appPinEnabled   bool
	appPinPositions map[string][2]int

	// Debug: hide candidate window (for performance testing)
	hideCandidateWindow bool

	// 翻页栏/页码显示方式覆盖（空=使用主题配置）
	pagerBarDisplay   config.PagerBarDisplay
	pageNumberDisplay config.PageNumberDisplay

	// Mode indicator version: incremented on each mode indicator show
	// Used to cancel previous hide timers when a new indicator is shown
	modeIndicatorVersion uint64

	// UI config for status indicator
	statusIndicatorDuration int // Duration in milliseconds
	statusIndicatorOffsetX  int // X offset for status indicator
	statusIndicatorOffsetY  int // Y offset for status indicator

	// Tooltip delay config
	tooltipDelay   int    // Delay in milliseconds before showing tooltip (0 = immediate)
	tooltipVersion uint64 // Version counter for cancelling pending tooltip shows

	// Track last rendered content to distinguish content updates from hover refreshes
	lastRenderedInput string
	lastRenderedPage  int

	// Unified popup menu (shared across toolbar/candidate/TSF entries)
	unifiedPopupMenu *PopupMenu

	// Global hotkey state (RegisterHotKey for combination hotkeys)
	globalHotkeys *globalHotkeyState

	// Host render callback: when set, rendered bitmap is sent here instead of local window.
	// Used for Band window proxy rendering in high-Band processes (e.g. Start Menu).
	hostRenderFunc func(img *image.RGBA, x, y int) error
	hostHideFunc   func()

	// maxCandidateChars 候选文本最大显示 rune 数（0 表示不限制）
	maxCandidateChars int

	// snapshot 追踪字段: 用于 setter 在末尾构造 CmdCandidatesConfig 等"全量快照"
	// 命令时读取当前完整状态。Win 端这些命令为 no-op (state 已被 sync setter 应用);
	// macOS forwarder 接入时会消费这些命令做跨进程同步。
	//
	// 注: appPinEnabled / appPinPositions / hideCandidateWindow / maxCandidateChars /
	// pagerDisplayMode / isPinyinMode / isQuickInputMode / modeLabel / modeAccentColor
	// 已在上面定义, 这里只补 renderer 不暴露 getter 的几个状态镜像。
	hidePreedit  bool
	preeditMode  config.PreeditMode
	cmdbarPrefix string
}

// NewManager creates a new UI manager
func NewManager(logger *slog.Logger) *Manager {
	// Create event for waking up message loop
	event, err := CreateEvent()
	if err != nil {
		logger.Error("Failed to create event", "error", err)
	}

	// Create theme manager
	themeManager := theme.NewManager(logger)

	return &Manager{
		window:        NewCandidateWindow(logger),
		renderer:      NewRenderer(DefaultRenderConfig()),
		toolbar:       NewToolbarWindow(logger),
		tooltip:       NewTooltipWindow(logger),
		status:        NewStatusWindow(logger),
		toast:         NewToastWindow(logger),
		themeManager:  themeManager,
		logger:        logger,
		readyCh:       make(chan struct{}),
		cmdCh:         make(chan uicmdItem, 100), // Buffered channel to avoid blocking IPC
		eventCh:       make(chan uicmd.Event, 100),
		cmdEvent:      event,
		globalHotkeys: &globalHotkeyState{logger: logger},
		// 注意：statusIndicator* 和 tooltipDelay 的默认值统一由 config.DefaultConfig() 提供，
		// 通过 coordinator 初始化时调用对应的 Set/Update 方法设置。
	}
}

// Start starts the UI manager (creates window and runs message loop)
// This should be called from a dedicated goroutine
func (m *Manager) Start() error {
	// Lock this goroutine to its OS thread for Windows GUI operations
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()

	m.logger.Info("Starting UI Manager...")

	// Create candidate window
	if err := m.window.Create(); err != nil {
		return err
	}

	// Set candidate window callbacks if available
	m.mu.Lock()
	if m.candidateCallbacks != nil {
		m.window.SetCallbacks(m.candidateCallbacks)
	}
	m.mu.Unlock()

	// Register DPI change callback to re-render all UI on monitor switch
	m.window.SetOnDPIChanged(func() {
		m.doDPIChanged()
	})

	// Create toolbar window
	if err := m.toolbar.Create(); err != nil {
		m.logger.Error("Failed to create toolbar window", "error", err)
		// Non-fatal, continue without toolbar
	} else {
		// Set toolbar callbacks if available
		m.mu.Lock()
		if m.toolbarCallbacks != nil {
			m.toolbar.SetCallback(m.toolbarCallbacks)
		}
		m.mu.Unlock()
	}

	// Create tooltip window
	if err := m.tooltip.Create(); err != nil {
		m.logger.Error("Failed to create tooltip window", "error", err)
		// Non-fatal, continue without tooltip
	}

	// 创建独立状态提示窗口
	if err := m.status.Create(); err != nil {
		m.logger.Error("Failed to create status window", "error", err)
	}

	// 创建 Toast 通知窗口（非关键，失败不致命）
	if err := m.toast.Create(); err != nil {
		m.logger.Error("Failed to create toast window", "error", err)
	}

	// Create unified popup menu
	m.unifiedPopupMenu = NewPopupMenu()
	if err := m.unifiedPopupMenu.Create(); err != nil {
		m.logger.Error("Failed to create unified popup menu", "error", err)
	}

	// Wire tooltip right-click to the custom popup menu
	m.tooltip.SetOnRightClick(func(text string, x, y int) {
		m.showTooltipContextMenu(text, x, y)
	})

	m.mu.Lock()
	m.ready = true
	m.mu.Unlock()
	close(m.readyCh)

	m.logger.Info("UI Manager ready")

	// Run combined message loop that handles both Windows messages and UI commands
	// This ensures all UI operations happen on the same thread that created the window
	m.runCombinedLoop()

	return nil
}

// runCombinedLoop runs a combined message loop that handles both Windows messages and UI commands
func (m *Manager) runCombinedLoop() {
	m.logger.Info("Starting combined message loop...")

	var msg MSG
	for {
		// Wait for either a Windows message or the command event
		ret := MsgWaitForMultipleObjects(m.cmdEvent, 50) // 50ms timeout for responsiveness

		switch {
		case ret == WAIT_OBJECT_0:
			// Command event signaled - process pending commands
			ResetEvent(m.cmdEvent)
			m.processPendingCommands()

		case ret == WAIT_OBJECT_0+1:
			// Windows message available - process all pending messages
			for PeekMessage(&msg) {
				if msg.Message == 0x0012 { // WM_QUIT
					m.logger.Info("Received WM_QUIT, exiting loop")
					return
				}
				if msg.Message == wmHotkey {
					// Global hotkey (RegisterHotKey) — dispatch to callback
					m.globalHotkeys.handleWMHotkey(int(msg.WParam))
					continue
				}
				ProcessMessage(&msg)
			}

		case ret == WAIT_TIMEOUT:
			// Timeout - check for any pending commands (in case event was missed)
			m.processPendingCommands()

		default:
			// Error or other return value
			m.logger.Debug("MsgWaitForMultipleObjects returned", "ret", ret)
		}
	}
}

// processPendingCommands processes all pending commands from the channel
func (m *Manager) processPendingCommands() {
	for {
		select {
		case cmd := <-m.cmdCh:
			m.processOneCommand(cmd)
		default:
			return // No more commands
		}
	}
}

// processOneCommand processes a single UI command.
//
// 输入 uicmdItem 由外观方法投递, 内含平台无关的 uicmd.Command + Windows 端的旁路字段
// (完整 candidate 切片、菜单回调函数指针等)。本函数按 uicmd.CommandType 分发到各 do* 方法,
// 用 type assertion 取出对应 payload, 与旁路字段一同传给 do*。
func (m *Manager) processOneCommand(item uicmdItem) {
	cmd := item.Cmd
	// Recover from any panics to keep the loop alive
	defer func() {
		if r := recover(); r != nil {
			m.logger.Error("Panic in UI command processing", "panic", r, "type", cmd.Type)
		}
	}()

	switch cmd.Type {
	case uicmd.CmdCandidatesShow:
		p := cmd.Payload.(uicmd.CandidatesShowPayload)
		// Stale 检测: 若 inputSession 已被推进 (e.g. 用户键入新内容触发 hide),
		// 旧的 show 命令直接丢弃, 避免候选框闪现已经过期的内容。
		m.mu.Lock()
		currentSession := m.inputSession
		m.mu.Unlock()
		if cmd.Session < currentSession {
			m.logger.Debug("Ignoring stale show command", "cmdSession", cmd.Session, "currentSession", currentSession)
			return
		}
		m.currentInputSession = cmd.Session
		m.doShowCandidates(item.Candidates, p.Input, p.CursorPos, p.CaretX, p.CaretY, p.CaretHeight,
			p.Page, p.TotalPages, p.TotalCandidateCount, p.CandidatesPerPage, p.SelectedIndex)
	case uicmd.CmdCandidatesHide:
		m.currentInputSession = cmd.Session
		m.doHide()
	case uicmd.CmdModeShow:
		p := cmd.Payload.(uicmd.ModeShowPayload)
		m.doShowModeIndicator(p.Mode, p.X, p.Y)
	case uicmd.CmdStatusShow:
		p := cmd.Payload.(uicmd.StatusShowPayload)
		m.doShowStatus(StatusState{
			ModeLabel:  p.State.ModeLabel,
			PunctLabel: p.State.PunctLabel,
			WidthLabel: p.State.WidthLabel,
		}, p.X, p.Y)
	case uicmd.CmdStatusHide:
		m.doHideStatus()
	case uicmd.CmdToolbarShow:
		p := cmd.Payload.(uicmd.ToolbarShowPayload)
		m.doShowToolbar(p.X, p.Y, fromUIToolbarState(p.State))
	case uicmd.CmdToolbarHide:
		m.doHideToolbar()
	case uicmd.CmdToolbarUpdate:
		p := cmd.Payload.(uicmd.ToolbarUpdatePayload)
		s := fromUIToolbarState(p.State)
		m.doUpdateToolbar(&s)
	case uicmd.CmdCandidateMenuHide:
		m.doHideCandidateMenu()
	case uicmd.CmdToolbarMenuHide:
		m.doHideToolbarMenu()
	case uicmd.CmdMenuShow:
		p := cmd.Payload.(uicmd.MenuShowPayload)
		m.doShowUnifiedMenuFromPayload(p, item.MenuState, item.Callback)
	case uicmd.CmdHotkeysRegister:
		p := cmd.Payload.(uicmd.HotkeysRegisterPayload)
		m.globalHotkeys.register(fromUIHotkeyEntries(p.Entries))
	case uicmd.CmdHotkeysUnregister:
		m.globalHotkeys.unregister()
	case uicmd.CmdTooltipShow:
		p := cmd.Payload.(uicmd.TooltipShowPayload)
		if m.tooltip != nil {
			m.tooltip.ForceHide()
			m.tooltip.Show(p.Text, p.CenterX, p.BelowY, p.AboveY)
		}
	case uicmd.CmdTooltipHide:
		if m.tooltip != nil {
			m.tooltip.ForceHide()
		}
	case uicmd.CmdCandidatesPosition:
		p := cmd.Payload.(uicmd.CandidatesPositionPayload)
		if m.window != nil {
			m.window.SetPosition(p.X, p.Y)
		}
	case uicmd.CmdToastShow:
		p := cmd.Payload.(uicmd.ToastShowPayload)
		m.doShowToast(ToastOptions{
			Title:    p.Title,
			Message:  p.Message,
			Level:    fromUIToastLevel(p.Level),
			Position: fromUIToastPosition(p.Position),
			Duration: int(p.Duration),
			MaxWidth: int(p.MaxWidth),
		})
	case uicmd.CmdToastHide:
		m.doHideToast()
	case uicmd.CmdScreenshot:
		m.doTakeScreenshot()
	case uicmd.CmdSettingsOpen:
		p := cmd.Payload.(uicmd.SettingsOpenPayload)
		m.doOpenSettings(p.Page, p.WebMode)
	case uicmd.CmdDPIChanged:
		m.doDPIChanged()
	case uicmd.CmdCandidatesConfig,
		uicmd.CmdCandidatesMarkers,
		uicmd.CmdCandidatesPinState,
		uicmd.CmdStatusConfig,
		uicmd.CmdConfigUpdate,
		uicmd.CmdThemeApply:
		// 这些 "snapshot" 命令的 state 已被 sync setter 在调用线程直接应用,
		// Windows 端 processOneCommand 不需要重复处理。
		// 命令仍通过 cmdCh 流转, 是为了 macOS forwarder 能拦截转发到 IMKit。
	default:
		m.logger.Warn("Unknown UI command type", "type", cmd.Type.String())
	}
}

// WaitReady waits until the UI manager is ready
func (m *Manager) WaitReady() {
	<-m.readyCh
}

// IsReady returns whether the UI manager is ready
func (m *Manager) IsReady() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.ready
}

// Destroy destroys the UI manager
func (m *Manager) Destroy() {
	m.window.Destroy()
	if m.renderer != nil {
		m.renderer.Close()
		m.renderer = nil
	}
	if m.toolbar != nil {
		m.toolbar.Destroy()
		m.toolbar = nil
	}
	if m.tooltip != nil {
		m.tooltip.Destroy()
		m.tooltip = nil
	}
	if m.status != nil {
		m.status.Destroy()
		m.status = nil
	}
	if m.toast != nil {
		m.toast.Destroy()
		m.toast = nil
	}
	if m.unifiedPopupMenu != nil {
		m.unifiedPopupMenu.Destroy()
		m.unifiedPopupMenu = nil
	}
	if m.globalHotkeys != nil {
		m.globalHotkeys.unregister()
	}
	if m.cmdEvent != 0 {
		CloseEvent(m.cmdEvent)
		m.cmdEvent = 0
	}
}

// SetGlobalHotkeyCallback sets the callback for global hotkey events.
// 内部包装 callback, 触发时同时推一份 EvtHotkeyTriggered 到 Manager.Events()。
func (m *Manager) SetGlobalHotkeyCallback(cb func(command string)) {
	m.globalHotkeys.callback = m.wrapHotkeyCallback(cb)
}

// RegisterGlobalHotkeys registers combination hotkeys via Windows RegisterHotKey API.
// Must be called from coordinator; actual registration happens on the UI thread.
func (m *Manager) RegisterGlobalHotkeys(entries []GlobalHotkeyEntry) {
	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdHotkeysRegister, 0,
		uicmd.HotkeysRegisterPayload{Entries: toUIHotkeyEntries(entries)})}
	select {
	case m.cmdCh <- item:
		SetEvent(m.cmdEvent)
	default:
		m.logger.Warn("Command channel full, dropping register_hotkeys")
	}
}

// TakeUIScreenshots 触发所有当前可见 UI 窗口的截图，保存到用户数据目录的 screenshots/ 子目录。
// 可从任意 goroutine 调用，实际截图在 UI 线程执行。
func (m *Manager) TakeUIScreenshots() {
	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdScreenshot, 0, nil)}
	select {
	case m.cmdCh <- item:
		SetEvent(m.cmdEvent)
	default:
		m.logger.Warn("Command channel full, dropping screenshot command")
	}
}

// UnregisterGlobalHotkeys unregisters all previously registered global hotkeys.
func (m *Manager) UnregisterGlobalHotkeys() {
	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdHotkeysUnregister, 0,
		uicmd.HotkeysUnregisterPayload{})}
	select {
	case m.cmdCh <- item:
		SetEvent(m.cmdEvent)
	default:
		m.logger.Warn("Command channel full, dropping unregister_hotkeys")
	}
}

// SetHostRenderFunc sets the host render callback.
// When set, rendered bitmaps are sent to this function instead of the local window.
// Pass nil to disable host rendering and resume local window rendering.
func (m *Manager) SetHostRenderFunc(renderFunc func(img *image.RGBA, x, y int) error, hideFunc func()) {
	m.mu.Lock()
	m.hostRenderFunc = renderFunc
	m.hostHideFunc = hideFunc
	m.mu.Unlock()
}

// IsHostRendering returns whether host rendering is currently active.
func (m *Manager) IsHostRendering() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.hostRenderFunc != nil
}

// SetToolbarCallbacks sets the callbacks for toolbar actions.
// 用 wrapToolbarCallbacks 包装传入 callbacks, 每次触发同时推一份 EvtToolbarClick。
func (m *Manager) SetToolbarCallbacks(callbacks *ToolbarCallback) {
	wrapped := m.wrapToolbarCallbacks(callbacks)
	m.mu.Lock()
	m.toolbarCallbacks = wrapped
	if m.toolbar != nil {
		m.toolbar.SetCallback(wrapped)
	}
	m.mu.Unlock()
}

// GetStatusWindow 返回状态窗口实例（供外部设置回调）
func (m *Manager) GetStatusWindow() *StatusWindow {
	return m.status
}

// SetCandidateCallbacks sets the callbacks for candidate window mouse interactions.
// 用 wrapCandidateCallbacks 包装, 每次触发同时推 EvtCandidateXxx 到 Events()。
func (m *Manager) SetCandidateCallbacks(callbacks *CandidateCallback) {
	wrapped := m.wrapCandidateCallbacks(callbacks)
	m.mu.Lock()
	m.candidateCallbacks = wrapped
	if m.window != nil {
		m.window.SetCallbacks(wrapped)
	}
	m.mu.Unlock()
}

// SetActiveAppPinState 由 coordinator 在焦点切换 / 菜单 toggle / 拖动落盘后推送：
// enabled=false 时 doShowCandidates 走常规自动定位 + 会话内 drag pin；
// enabled=true 且 positionsByMonitor 含 caret 所在显示器 key 时使用其坐标（显示前再 clamp 到工作区）。
// positionsByMonitor 由调用方拷贝传入，本方法不再共享其底层数组。
func (m *Manager) SetActiveAppPinState(enabled bool, positionsByMonitor map[string][2]int) {
	m.mu.Lock()
	m.appPinEnabled = enabled
	if enabled {
		m.appPinPositions = positionsByMonitor
	} else {
		m.appPinPositions = nil
	}
	m.mu.Unlock()
	m.postCmd(m.snapshotCandidatesPinState())
}
