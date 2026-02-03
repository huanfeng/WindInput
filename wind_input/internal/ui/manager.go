package ui

import (
	"log/slog"
	"runtime"
	"sync"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

// UICommand represents a command to the UI thread
type UICommand struct {
	Type       string // "show", "hide", "mode", "toolbar_show", "toolbar_hide", "toolbar_update", "settings"
	Candidates []Candidate
	Input      string
	X, Y       int // Caret position (original, not adjusted)
	CaretHeight int // Height of the caret for position adjustment
	Page       int
	TotalPages int
	ModeText   string
	// Toolbar state and position
	ToolbarState *ToolbarState
	ToolbarX     int
	ToolbarY     int
	// Input session version for preventing stale show commands
	InputSession uint64
}

// Manager manages the candidate window UI
type Manager struct {
	window   *CandidateWindow
	renderer *Renderer
	logger   *slog.Logger

	// Toolbar window
	toolbar *ToolbarWindow

	mu         sync.Mutex
	candidates []Candidate
	input      string
	page       int
	totalPages int
	caretX     int
	caretY     int
	caretHeight int

	// Sticky position state: once candidate window jumps above caret,
	// it stays above until input is cleared (new input session)
	stickyAbove bool

	// Input session version: incremented on each commit/hide to prevent
	// stale show commands from reappearing the candidate window
	inputSession        uint64
	currentInputSession uint64 // The session being displayed (for UI thread)

	ready    bool
	readyCh  chan struct{}

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
}

// NewManager creates a new UI manager
func NewManager(logger *slog.Logger) *Manager {
	// Create event for waking up message loop
	event, err := CreateEvent()
	if err != nil {
		logger.Error("Failed to create event", "error", err)
	}

	return &Manager{
		window:                  NewCandidateWindow(logger),
		renderer:                NewRenderer(DefaultRenderConfig()),
		toolbar:                 NewToolbarWindow(logger),
		logger:                  logger,
		readyCh:                 make(chan struct{}),
		cmdCh:                   make(chan UICommand, 100), // Buffered channel to avoid blocking IPC
		cmdEvent:                event,
		statusIndicatorDuration: 800, // Default 800ms
		statusIndicatorOffsetX:  0,
		statusIndicatorOffsetY:  5, // Slightly below cursor
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
		m.doShowCandidates(cmd.Candidates, cmd.Input, cmd.X, cmd.Y, cmd.CaretHeight, cmd.Page, cmd.TotalPages)
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
		m.doOpenSettings()
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

// ShowCandidates shows candidates at the given caret position (async, non-blocking)
// The position will be automatically adjusted to stay within screen bounds.
// Parameters:
//   - caretX, caretY: the caret position (where input is happening)
//   - caretHeight: height of the caret/cursor
func (m *Manager) ShowCandidates(candidates []Candidate, input string, caretX, caretY, caretHeight, page, totalPages int) error {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return nil
	}
	m.candidates = candidates
	m.input = input
	m.page = page
	m.totalPages = totalPages
	m.caretX = caretX
	m.caretY = caretY
	m.caretHeight = caretHeight
	// Capture current input session for this show command
	currentSession := m.inputSession
	m.mu.Unlock()

	m.logger.Debug("Queuing ShowCandidates", "input", input, "count", len(candidates), "caretX", caretX, "caretY", caretY, "caretHeight", caretHeight, "session", currentSession)

	// Send command to UI thread (non-blocking due to buffered channel)
	select {
	case m.cmdCh <- UICommand{
		Type:         "show",
		Candidates:   candidates,
		Input:        input,
		X:            caretX,
		Y:            caretY,
		CaretHeight:  caretHeight,
		Page:         page,
		TotalPages:   totalPages,
		InputSession: currentSession,
	}:
		// Signal the event to wake up the message loop
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping show command")
	}

	return nil
}

// doShowCandidates actually shows candidates (called from UI thread)
// Parameters caretX, caretY, caretHeight are the original caret position info.
func (m *Manager) doShowCandidates(candidates []Candidate, input string, caretX, caretY, caretHeight, page, totalPages int) {
	// Debug: skip rendering if hide_candidate_window is enabled
	if m.hideCandidateWindow {
		m.logger.Debug("doShowCandidates skipped (hide_candidate_window enabled)")
		return
	}

	m.logger.Debug("doShowCandidates start", "input", input, "count", len(candidates), "caretX", caretX, "caretY", caretY, "caretHeight", caretHeight)

	// Check if this is a new input session (input is shorter than before or empty)
	// If so, reset the sticky state and hover index
	m.mu.Lock()
	prevInput := m.input
	if len(input) < len(prevInput) || input == "" {
		m.stickyAbove = false
		m.window.ResetHoverIndex()
		m.logger.Debug("Reset sticky state", "prevInput", prevInput, "newInput", input)
	}
	currentStickyAbove := m.stickyAbove
	// Get current hover index for rendering
	hoverIndex := m.window.GetHoverIndex()
	m.mu.Unlock()

	// Render first to get actual window size (with hover highlight)
	m.logger.Debug("Rendering candidates...", "hoverIndex", hoverIndex)
	img, renderResult := m.renderer.RenderCandidates(candidates, input, page, totalPages, hoverIndex)
	windowWidth := img.Bounds().Dx()
	windowHeight := img.Bounds().Dy()
	m.logger.Debug("Render complete", "width", windowWidth, "height", windowHeight)

	// Update hit test rectangles for mouse interaction
	if renderResult != nil {
		m.window.SetHitRects(renderResult.Rects)
	}

	// Determine position preference based on sticky state
	var preference PositionPreference
	if currentStickyAbove {
		preference = PositionAbove
	} else {
		preference = PositionAuto
	}

	// Adjust position to stay within screen bounds
	// Determine layout from renderer config
	layout := LayoutVertical
	if m.renderer != nil && m.renderer.GetLayout() == "horizontal" {
		layout = LayoutHorizontal
	}
	windowX, windowY, showAbove := AdjustCandidatePosition(caretX, caretY, caretHeight, windowWidth, windowHeight, layout, preference)
	m.logger.Debug("Position adjusted", "windowX", windowX, "windowY", windowY, "showAbove", showAbove, "stickyAbove", currentStickyAbove)

	// Update sticky state if we're now showing above
	if showAbove && !currentStickyAbove {
		m.mu.Lock()
		m.stickyAbove = true
		m.mu.Unlock()
		m.logger.Debug("Set sticky state to above")
	}

	// Update window
	m.logger.Debug("Updating window content...")
	if err := m.window.UpdateContent(img, windowX, windowY); err != nil {
		m.logger.Error("UpdateContent failed", "error", err)
		return
	}
	m.logger.Debug("Window content updated")

	// Show window
	m.logger.Debug("Showing window...")
	m.window.Show()
	m.logger.Debug("doShowCandidates complete")
}

// Hide hides the candidate window (async, non-blocking)
// This also increments the input session to invalidate any pending show commands
func (m *Manager) Hide() {
	// Increment input session FIRST to invalidate any pending show commands
	// This ensures that show commands queued before this hide will be ignored
	m.mu.Lock()
	m.inputSession++
	newSession := m.inputSession
	m.mu.Unlock()

	m.logger.Debug("Hide called, new session", "session", newSession)

	// Send command to UI thread (non-blocking)
	// Note: We always send hide command even if window appears hidden,
	// because the window visibility check is not thread-safe and there might
	// be pending show commands in the channel
	select {
	case m.cmdCh <- UICommand{Type: "hide", InputSession: newSession}:
		// Signal the event to wake up the message loop
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		// Channel full, but hide is not critical - window will be hidden eventually
		m.logger.Debug("UI command channel full, skipping redundant hide")
	}
}

// doHide actually hides the window (called from UI thread)
func (m *Manager) doHide() {
	m.window.Hide()

	// Reset sticky state and hover index when hiding (input session ended)
	m.mu.Lock()
	m.stickyAbove = false
	m.mu.Unlock()
	m.window.ResetHoverIndex()
}

// UpdatePosition updates the window position
func (m *Manager) UpdatePosition(x, y int) {
	m.mu.Lock()
	m.caretX = x
	m.caretY = y
	m.mu.Unlock()

	m.window.SetPosition(x, y)
}

// Destroy destroys the UI manager
func (m *Manager) Destroy() {
	m.window.Destroy()
	if m.toolbar != nil {
		m.toolbar.Destroy()
	}
	if m.cmdEvent != 0 {
		CloseEvent(m.cmdEvent)
		m.cmdEvent = 0
	}
}

// IsVisible returns whether the window is visible
func (m *Manager) IsVisible() bool {
	return m.window.IsVisible()
}

// ShowModeIndicator shows a brief mode indicator (中/En) (async, non-blocking)
func (m *Manager) ShowModeIndicator(mode string, x, y int) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	m.logger.Debug("Queuing ShowModeIndicator", "mode", mode)

	// Send command to UI thread (non-blocking)
	select {
	case m.cmdCh <- UICommand{
		Type:     "mode",
		ModeText: mode,
		X:        x,
		Y:        y,
	}:
		// Signal the event to wake up the message loop
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping mode command")
	}
}

// doShowModeIndicator actually shows the mode indicator (called from UI thread)
func (m *Manager) doShowModeIndicator(mode string, x, y int) {
	// Increment version to cancel any pending hide timers
	m.mu.Lock()
	m.modeIndicatorVersion++
	currentVersion := m.modeIndicatorVersion
	duration := m.statusIndicatorDuration
	offsetX := m.statusIndicatorOffsetX
	offsetY := m.statusIndicatorOffsetY
	m.mu.Unlock()

	// Apply offset to position
	adjustedX := x + offsetX
	adjustedY := y + offsetY

	// Render mode indicator
	img := m.renderer.RenderModeIndicator(mode)

	// Update window
	if err := m.window.UpdateContent(img, adjustedX, adjustedY); err != nil {
		m.logger.Error("UpdateContent failed", "error", err)
		return
	}

	// Show window briefly
	m.window.Show()

	// Hide after delay, but only if version hasn't changed
	// This ensures that rapid state changes reset the timer
	go func() {
		time.Sleep(time.Duration(duration) * time.Millisecond)

		// Check if version is still the same
		m.mu.Lock()
		versionNow := m.modeIndicatorVersion
		m.mu.Unlock()

		if versionNow == currentVersion {
			// Version unchanged, safe to hide
			m.Hide() // Use public method which goes through channel
		}
		// If version changed, another indicator was shown, so don't hide
	}()
}

// UpdateConfig 更新 UI 配置（热更新）
func (m *Manager) UpdateConfig(fontSize float64, fontPath string, hideCandidateWindow bool) {
	// 更新渲染器的字体设置
	if m.renderer != nil {
		m.renderer.UpdateFont(fontSize, fontPath)
	}
	// 更新调试开关
	m.mu.Lock()
	m.hideCandidateWindow = hideCandidateWindow
	m.mu.Unlock()
	m.logger.Info("UI config updated", "fontSize", fontSize, "fontPath", fontPath, "hideCandidateWindow", hideCandidateWindow)
}

// UpdateStatusIndicatorConfig 更新状态提示配置
func (m *Manager) UpdateStatusIndicatorConfig(duration, offsetX, offsetY int) {
	m.mu.Lock()
	if duration > 0 {
		m.statusIndicatorDuration = duration
	}
	m.statusIndicatorOffsetX = offsetX
	m.statusIndicatorOffsetY = offsetY
	m.mu.Unlock()
	m.logger.Info("Status indicator config updated", "duration", duration, "offsetX", offsetX, "offsetY", offsetY)
}

// SetCandidateLayout 设置候选框布局模式
func (m *Manager) SetCandidateLayout(layout string) {
	if m.renderer != nil {
		m.renderer.SetLayout(layout)
		m.logger.Info("Candidate layout updated", "layout", layout)
	}
}

// SetHidePreedit 设置是否隐藏预编辑区域
func (m *Manager) SetHidePreedit(hide bool) {
	if m.renderer != nil {
		m.renderer.SetHidePreedit(hide)
		m.logger.Info("Hide preedit updated", "hide", hide)
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

// RefreshCandidates re-renders the candidate window with current state
// Used to update hover highlight without changing candidate data
func (m *Manager) RefreshCandidates() {
	m.mu.Lock()
	if !m.ready || !m.window.IsVisible() {
		m.mu.Unlock()
		return
	}
	candidates := m.candidates
	input := m.input
	page := m.page
	totalPages := m.totalPages
	caretX := m.caretX
	caretY := m.caretY
	caretHeight := m.caretHeight
	currentSession := m.inputSession
	m.mu.Unlock()

	// Re-queue a show command with current data
	select {
	case m.cmdCh <- UICommand{
		Type:         "show",
		Candidates:   candidates,
		Input:        input,
		X:            caretX,
		Y:            caretY,
		CaretHeight:  caretHeight,
		Page:         page,
		TotalPages:   totalPages,
		InputSession: currentSession,
	}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		// Channel full, skip refresh
	}
}

// SetToolbarVisible shows or hides the toolbar
func (m *Manager) SetToolbarVisible(visible bool) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	if !visible {
		select {
		case m.cmdCh <- UICommand{Type: "toolbar_hide"}:
			if m.cmdEvent != 0 {
				SetEvent(m.cmdEvent)
			}
		default:
			m.logger.Warn("UI command channel full, dropping toolbar hide command")
		}
	}
	// For showing toolbar, use ShowToolbarWithState instead
}

// ShowToolbarWithState shows the toolbar with position and state in one atomic operation
func (m *Manager) ShowToolbarWithState(x, y int, state ToolbarState) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	select {
	case m.cmdCh <- UICommand{
		Type:         "toolbar_show",
		ToolbarX:     x,
		ToolbarY:     y,
		ToolbarState: &state,
	}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping toolbar show command")
	}
}

// UpdateToolbarState updates the toolbar state
func (m *Manager) UpdateToolbarState(state ToolbarState) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	select {
	case m.cmdCh <- UICommand{Type: "toolbar_update", ToolbarState: &state}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping toolbar update command")
	}
}

// SetToolbarPosition sets the toolbar position
func (m *Manager) SetToolbarPosition(x, y int) {
	if m.toolbar != nil {
		m.toolbar.SetPosition(x, y)
		// Re-render to update layered window with new position
		m.toolbar.Render()
	}
}

// GetToolbarPosition returns the current toolbar position
func (m *Manager) GetToolbarPosition() (int, int) {
	if m.toolbar != nil {
		return m.toolbar.GetPosition()
	}
	return 0, 0
}

// OpenSettings opens the settings window
func (m *Manager) OpenSettings() {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	select {
	case m.cmdCh <- UICommand{Type: "settings"}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping settings command")
	}
}

// doShowToolbar shows the toolbar with optional position and state (called from UI thread)
func (m *Manager) doShowToolbar(cmd UICommand) {
	if m.toolbar == nil {
		m.logger.Warn("doShowToolbar: toolbar is nil")
		return
	}

	m.logger.Debug("doShowToolbar called",
		"x", cmd.ToolbarX,
		"y", cmd.ToolbarY,
		"hasState", cmd.ToolbarState != nil)

	// Set position if provided
	if cmd.ToolbarX != 0 || cmd.ToolbarY != 0 {
		m.toolbar.SetPosition(cmd.ToolbarX, cmd.ToolbarY)
		m.logger.Debug("Toolbar position set", "x", cmd.ToolbarX, "y", cmd.ToolbarY)
	}

	// Set state if provided (before rendering)
	if cmd.ToolbarState != nil {
		m.logger.Debug("Toolbar state set",
			"chineseMode", cmd.ToolbarState.ChineseMode,
			"fullWidth", cmd.ToolbarState.FullWidth,
			"chinesePunct", cmd.ToolbarState.ChinesePunct,
			"capsLock", cmd.ToolbarState.CapsLock)
		m.toolbar.SetState(*cmd.ToolbarState)
	} else {
		// Just render with current state
		m.toolbar.Render()
	}

	m.toolbar.Show()
	m.logger.Info("Toolbar shown", "x", cmd.ToolbarX, "y", cmd.ToolbarY)
}

// doHideToolbar hides the toolbar (called from UI thread)
func (m *Manager) doHideToolbar() {
	if m.toolbar != nil {
		m.toolbar.Hide()
		m.logger.Info("Toolbar hidden")
	} else {
		m.logger.Warn("doHideToolbar: toolbar is nil")
	}
}

// doUpdateToolbar updates the toolbar state (called from UI thread)
func (m *Manager) doUpdateToolbar(state *ToolbarState) {
	if m.toolbar != nil && state != nil {
		m.logger.Debug("doUpdateToolbar",
			"chineseMode", state.ChineseMode,
			"fullWidth", state.FullWidth,
			"chinesePunct", state.ChinesePunct,
			"capsLock", state.CapsLock)
		m.toolbar.SetState(*state)
	} else {
		m.logger.Warn("doUpdateToolbar: toolbar or state is nil",
			"toolbarNil", m.toolbar == nil,
			"stateNil", state == nil)
	}
}

// doOpenSettings opens the settings window (called from UI thread)
func (m *Manager) doOpenSettings() {
	m.logger.Info("Opening settings application")

	// Try to launch wind_setting.exe
	// First try the install directory, then fall back to current directory
	shell32 := windows.NewLazySystemDLL("shell32.dll")
	procShellExecuteW := shell32.NewProc("ShellExecuteW")

	// Try paths in order of preference
	paths := []string{
		"C:\\Program Files\\WindInput\\wind_setting.exe",
		"wind_setting.exe", // Current directory or PATH
	}

	openPtr, _ := windows.UTF16PtrFromString("open")
	var lastRet uintptr
	var lastErr error

	for _, path := range paths {
		pathPtr, _ := windows.UTF16PtrFromString(path)

		ret, _, err := procShellExecuteW.Call(
			0,                                // hwnd
			uintptr(unsafe.Pointer(openPtr)), // lpOperation ("open")
			uintptr(unsafe.Pointer(pathPtr)), // lpFile (path to exe)
			0,                                // lpParameters
			0,                                // lpDirectory
			1,                                // nShowCmd (SW_SHOWNORMAL)
		)

		lastRet = ret
		lastErr = err

		// ShellExecuteW returns >32 on success
		if ret > 32 {
			m.logger.Info("Settings application launched successfully", "path", path)
			return
		}
	}

	// All paths failed, fall back to opening the web URL
	m.logger.Warn("Failed to launch wind_setting.exe, falling back to web URL", "ret", lastRet, "error", lastErr)

	url := "http://127.0.0.1:18923"
	urlPtr, _ := windows.UTF16PtrFromString(url)

	ret, _, err := procShellExecuteW.Call(
		0,                                // hwnd
		uintptr(unsafe.Pointer(openPtr)), // lpOperation ("open")
		uintptr(unsafe.Pointer(urlPtr)),  // lpFile (URL)
		0,                                // lpParameters
		0,                                // lpDirectory
		1,                                // nShowCmd (SW_SHOWNORMAL)
	)

	if ret <= 32 {
		m.logger.Error("Failed to open settings URL", "error", err, "ret", ret)
	} else {
		m.logger.Info("Settings URL opened successfully (fallback)")
	}
}
