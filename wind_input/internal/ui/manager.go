package ui

import (
	"log/slog"
	"runtime"
	"sync"

	"github.com/huanfeng/wind_input/pkg/theme"
	"golang.org/x/sys/windows"
)

// Unified menu ID constants
const (
	UnifiedMenuToggleMode     = 100
	UnifiedMenuToggleWidth    = 101
	UnifiedMenuTogglePunct    = 102
	UnifiedMenuToggleToolbar  = 103
	UnifiedMenuThemeBase      = 200 // 主题ID: 200+i
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

// UnifiedMenuState holds the current state for building the unified menu
type UnifiedMenuState struct {
	ChineseMode    bool
	FullWidth      bool
	ChinesePunct   bool
	ToolbarVisible bool
	Themes         []ThemeMenuItem
	CurrentThemeID string // Current theme ID for checked state
}

// BuildUnifiedMenuItems constructs the unified menu item list
func BuildUnifiedMenuItems(state UnifiedMenuState) []MenuItem {
	items := []MenuItem{
		{ID: UnifiedMenuToggleMode, Text: "中文模式", Checked: state.ChineseMode},
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
	Type                string // "show", "hide", "mode", "toolbar_show", "toolbar_hide", "toolbar_update", "settings", "hide_menu", "show_unified_menu"
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
		window:       NewCandidateWindow(logger),
		renderer:     NewRenderer(DefaultRenderConfig()),
		toolbar:      NewToolbarWindow(logger),
		tooltip:      NewTooltipWindow(logger),
		themeManager: themeManager,
		logger:       logger,
		readyCh:      make(chan struct{}),
		cmdCh:        make(chan UICommand, 100), // Buffered channel to avoid blocking IPC
		cmdEvent:     event,
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
	if m.unifiedPopupMenu != nil {
		m.unifiedPopupMenu.Destroy()
		m.unifiedPopupMenu = nil
	}
	if m.cmdEvent != 0 {
		CloseEvent(m.cmdEvent)
		m.cmdEvent = 0
	}
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

// SetCandidateCallbacks sets the callbacks for candidate window mouse interactions
func (m *Manager) SetCandidateCallbacks(callbacks *CandidateCallback) {
	m.mu.Lock()
	m.candidateCallbacks = callbacks
	if m.window != nil {
		m.window.SetCallbacks(callbacks)
	}
	m.mu.Unlock()
}
