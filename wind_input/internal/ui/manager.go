package ui

import (
	"image"
	"log/slog"
	"runtime"
	"sync"

	"github.com/huanfeng/wind_input/pkg/theme"
	"golang.org/x/sys/windows"
)

// Unified menu ID constants
const (
	UnifiedMenuToggleWidth    = 101
	UnifiedMenuTogglePunct    = 102
	UnifiedMenuToggleToolbar  = 103
	UnifiedMenuSchemaEnglish  = 140 // 英文模式
	UnifiedMenuSchemaBase     = 150 // 方案ID: 150+i
	UnifiedMenuThemeBase      = 200 // 主题ID: 200+i
	UnifiedMenuThemeStyleBase = 250 // 主题风格ID: 250+i (0=system, 1=light, 2=dark)
	UnifiedMenuFilterModeBase = 260 // 检索范围ID: 260+i (0=smart, 1=general, 2=gb18030)
	UnifiedMenuReloadConfig   = 299
	UnifiedMenuRestartService = 303
	UnifiedMenuDictionary     = 300
	UnifiedMenuSettings       = 301
	UnifiedMenuAbout          = 302
)

// ThemeMenuItem holds theme ID and display name for menu rendering
type ThemeMenuItem struct {
	ID          string // Theme ID for loading (e.g., "default")
	DisplayName string // Display name (e.g., "默认主题 1.0")
}

// SchemaMenuItem holds schema ID and display name for menu rendering
type SchemaMenuItem struct {
	ID   string // Schema ID (e.g., "wubi86")
	Name string // Display name (e.g., "五笔86")
}

// UnifiedMenuState holds the current state for building the unified menu
type UnifiedMenuState struct {
	ChineseMode       bool
	FullWidth         bool
	ChinesePunct      bool
	ToolbarVisible    bool
	Schemas           []SchemaMenuItem // Available schemas in order
	CurrentSchemaID   string           // Current active schema ID
	CurrentFilterMode string           // Current filter mode: "smart", "general", "gb18030"
	Themes            []ThemeMenuItem
	CurrentThemeID    string // Current theme ID for checked state
	CurrentThemeStyle string // Current theme style: "system", "light", "dark"
}

// BuildUnifiedMenuItems constructs the unified menu item list
func BuildUnifiedMenuItems(state UnifiedMenuState) []MenuItem {
	// Build schema submenu: 英文 + available schemas
	var schemaChildren []MenuItem
	schemaChildren = append(schemaChildren, MenuItem{
		ID:      UnifiedMenuSchemaEnglish,
		Text:    "英文",
		Checked: !state.ChineseMode,
	})
	if len(state.Schemas) > 0 {
		schemaChildren = append(schemaChildren, MenuItem{Separator: true})
		for i, s := range state.Schemas {
			schemaChildren = append(schemaChildren, MenuItem{
				ID:      UnifiedMenuSchemaBase + i,
				Text:    s.Name,
				Checked: state.ChineseMode && s.ID == state.CurrentSchemaID,
			})
		}
	}

	// Build filter mode submenu
	filterMode := state.CurrentFilterMode
	if filterMode == "" {
		filterMode = "smart"
	}
	filterChildren := []MenuItem{
		{ID: UnifiedMenuFilterModeBase, Text: "智能模式", Checked: filterMode == "smart"},
		{ID: UnifiedMenuFilterModeBase + 1, Text: "常用字", Checked: filterMode == "general"},
		{ID: UnifiedMenuFilterModeBase + 2, Text: "全部字符", Checked: filterMode == "gb18030"},
	}

	items := []MenuItem{
		{Text: "输入方案", Children: schemaChildren},
		{Text: "检索范围", Children: filterChildren},
		{ID: UnifiedMenuToggleWidth, Text: "全角", Checked: state.FullWidth},
		{ID: UnifiedMenuTogglePunct, Text: "中文标点", Checked: state.ChinesePunct},
		{Separator: true},
		{ID: UnifiedMenuToggleToolbar, Text: "显示工具栏", Checked: state.ToolbarVisible},
	}

	// Build theme submenu if there are themes
	if len(state.Themes) > 0 {
		var themeChildren []MenuItem
		for i, t := range state.Themes {
			themeChildren = append(themeChildren, MenuItem{
				ID:      UnifiedMenuThemeBase + i,
				Text:    t.DisplayName,
				Checked: t.ID == state.CurrentThemeID,
			})
		}
		// Add separator and theme style options
		themeStyle := state.CurrentThemeStyle
		if themeStyle == "" {
			themeStyle = "system"
		}
		themeChildren = append(themeChildren, MenuItem{Separator: true})
		themeChildren = append(themeChildren,
			MenuItem{ID: UnifiedMenuThemeStyleBase, Text: "跟随系统", Checked: themeStyle == "system"},
			MenuItem{ID: UnifiedMenuThemeStyleBase + 1, Text: "亮色", Checked: themeStyle == "light"},
			MenuItem{ID: UnifiedMenuThemeStyleBase + 2, Text: "暗色", Checked: themeStyle == "dark"},
		)
		items = append(items, MenuItem{Text: "主题", Children: themeChildren})
	}

	items = append(items,
		MenuItem{Separator: true},
		MenuItem{ID: UnifiedMenuReloadConfig, Text: "重载配置"},
		MenuItem{ID: UnifiedMenuRestartService, Text: "重启服务"},
		MenuItem{Separator: true},
		MenuItem{ID: UnifiedMenuDictionary, Text: "词库管理..."},
		MenuItem{ID: UnifiedMenuSettings, Text: "设置..."},
		MenuItem{Separator: true},
		MenuItem{ID: UnifiedMenuAbout, Text: "关于"},
	)

	return items
}

// UICommand represents a command to the UI thread
type UICommand struct {
	Type                string // "show", "hide", "mode", "status", "status_hide", "toolbar_show", "toolbar_hide", "toolbar_update", "settings", "hide_menu", "show_unified_menu"
	Candidates          []Candidate
	Input               string
	CursorPos           int // Cursor position within Input (display position, for rendering cursor indicator)
	X, Y                int // Caret position (original, not adjusted)
	CaretHeight         int // Height of the caret for position adjustment
	Page                int
	TotalPages          int
	TotalCandidateCount int // 候选总数（所有页）
	CandidatesPerPage   int // 每页候选数
	SelectedIndex       int // 当前页内选中的候选索引（0-based）
	ModeText            string
	// Toolbar state and position
	ToolbarState *ToolbarState
	ToolbarX     int
	ToolbarY     int
	// Input session version for preventing stale show commands
	InputSession uint64
	// Settings page to open (e.g., "about")
	SettingsPage string
	// Unified menu
	MenuState    *UnifiedMenuState
	MenuCallback func(id int)
	FlipRefY     int // 翻转参考Y（下方放不下时翻转到此Y上方，0=禁用）
	// 状态提示
	StatusState *StatusState
	// Global hotkey registration
	HotkeyEntries []GlobalHotkeyEntry
}

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

	mu                  sync.Mutex
	candidates          []Candidate
	input               string
	cursorPos           int
	page                int
	totalPages          int
	totalCandidateCount int
	candidatesPerPage   int
	selectedIndex       int  // 当前页内选中的候选索引
	isPinyinMode        bool // 是否拼音模式（控制右键菜单前移/后移禁用）
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
	cmdCh chan UICommand

	// Event to wake up the message loop when commands are available
	cmdEvent windows.Handle

	// Toolbar callbacks (set by coordinator)
	toolbarCallbacks *ToolbarCallback

	// Candidate window callbacks (for mouse interaction)
	candidateCallbacks *CandidateCallback

	// Debug: hide candidate window (for performance testing)
	hideCandidateWindow bool

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
		themeManager:  themeManager,
		logger:        logger,
		readyCh:       make(chan struct{}),
		cmdCh:         make(chan UICommand, 100), // Buffered channel to avoid blocking IPC
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

	// Create unified popup menu
	m.unifiedPopupMenu = NewPopupMenu()
	if err := m.unifiedPopupMenu.Create(); err != nil {
		m.logger.Error("Failed to create unified popup menu", "error", err)
	}

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

// processOneCommand processes a single UI command
func (m *Manager) processOneCommand(cmd UICommand) {
	// Recover from any panics to keep the loop alive
	defer func() {
		if r := recover(); r != nil {
			m.logger.Error("Panic in UI command processing", "panic", r, "type", cmd.Type)
		}
	}()

	switch cmd.Type {
	case "show":
		// Check if this show command is from the current input session
		// If the session has been incremented (by a hide command), ignore stale show commands
		m.mu.Lock()
		currentSession := m.inputSession
		m.mu.Unlock()

		if cmd.InputSession < currentSession {
			m.logger.Debug("Ignoring stale show command", "cmdSession", cmd.InputSession, "currentSession", currentSession)
			return
		}
		m.currentInputSession = cmd.InputSession
		m.doShowCandidates(cmd.Candidates, cmd.Input, cmd.CursorPos, cmd.X, cmd.Y, cmd.CaretHeight, cmd.Page, cmd.TotalPages, cmd.TotalCandidateCount, cmd.CandidatesPerPage, cmd.SelectedIndex)
	case "hide":
		// Update current session to the hide command's session
		m.currentInputSession = cmd.InputSession
		m.doHide()
	case "mode":
		m.doShowModeIndicator(cmd.ModeText, cmd.X, cmd.Y)
	case "status":
		if cmd.StatusState != nil {
			m.doShowStatus(*cmd.StatusState, cmd.X, cmd.Y)
		}
	case "status_hide":
		m.doHideStatus()
	case "toolbar_show":
		m.doShowToolbar(cmd)
	case "toolbar_hide":
		m.doHideToolbar()
	case "toolbar_update":
		m.doUpdateToolbar(cmd.ToolbarState)
	case "settings":
		m.doOpenSettings(cmd.SettingsPage)
	case "hide_menu":
		m.doHideCandidateMenu()
	case "hide_toolbar_menu":
		m.doHideToolbarMenu()
	case "show_unified_menu":
		m.doShowUnifiedMenu(cmd)
	case "dpi_changed":
		m.doDPIChanged()
	case "register_hotkeys":
		m.globalHotkeys.register(cmd.HotkeyEntries)
	case "unregister_hotkeys":
		m.globalHotkeys.unregister()
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

// SetGlobalHotkeyCallback sets the callback for global hotkey events
func (m *Manager) SetGlobalHotkeyCallback(cb func(command string)) {
	m.globalHotkeys.callback = cb
}

// RegisterGlobalHotkeys registers combination hotkeys via Windows RegisterHotKey API.
// Must be called from coordinator; actual registration happens on the UI thread.
func (m *Manager) RegisterGlobalHotkeys(entries []GlobalHotkeyEntry) {
	select {
	case m.cmdCh <- UICommand{Type: "register_hotkeys", HotkeyEntries: entries}:
		SetEvent(m.cmdEvent)
	default:
		m.logger.Warn("Command channel full, dropping register_hotkeys")
	}
}

// UnregisterGlobalHotkeys unregisters all previously registered global hotkeys.
func (m *Manager) UnregisterGlobalHotkeys() {
	select {
	case m.cmdCh <- UICommand{Type: "unregister_hotkeys"}:
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

// SetToolbarCallbacks sets the callbacks for toolbar actions
func (m *Manager) SetToolbarCallbacks(callbacks *ToolbarCallback) {
	m.mu.Lock()
	m.toolbarCallbacks = callbacks
	if m.toolbar != nil {
		m.toolbar.SetCallback(callbacks)
	}
	m.mu.Unlock()
}

// GetStatusWindow 返回状态窗口实例（供外部设置回调）
func (m *Manager) GetStatusWindow() *StatusWindow {
	return m.status
}

// SetCandidateCallbacks sets the callbacks for candidate window mouse interactions
func (m *Manager) SetCandidateCallbacks(callbacks *CandidateCallback) {
	m.mu.Lock()
	m.candidateCallbacks = callbacks
	if m.window != nil {
		m.window.SetCallbacks(callbacks)
	}
	m.mu.Unlock()
}
