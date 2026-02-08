// Package coordinator orchestrates communication between C++ Bridge, Engine, and UI
package coordinator

import (
	"log/slog"
	"runtime"
	"runtime/debug"
	"strings"
	"sync"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/hotkey"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
)

// Restart request channel - main should listen to this
var restartRequestCh = make(chan struct{}, 1)

// RequestRestart signals that a restart is requested
func RequestRestart() {
	select {
	case restartRequestCh <- struct{}{}:
	default:
		// Channel already has a request pending
	}
}

// RestartRequested returns a channel that signals when restart is requested
func RestartRequested() <-chan struct{} {
	return restartRequestCh
}

// Modifier key flags (must match C++ side)
const (
	ModShift = 0x01
	ModCtrl  = 0x02
	ModAlt   = 0x04
)

// EffectiveMode represents the effective input mode considering CapsLock
type EffectiveMode int

const (
	ModeChinese      EffectiveMode = iota // 中文模式
	ModeEnglishLower                      // 英文小写模式
	ModeEnglishUpper                      // 英文大写模式 (CapsLock on)
)

// Coordinator orchestrates between C++ Bridge, Engine, and native UI
type Coordinator struct {
	engineMgr    *engine.Manager
	uiManager    *ui.Manager
	logger       *slog.Logger
	config       *config.Config
	bridgeServer BridgeServer // Interface for broadcasting state to TSF clients

	mu sync.Mutex

	// Input mode state
	chineseMode bool // true = Chinese, false = English
	capsLockOn  bool // CapsLock state (authority source)

	// Full-width and punctuation state
	fullWidth          bool // true = full-width, false = half-width
	chinesePunctuation bool // true = Chinese punctuation, false = English punctuation
	punctFollowMode    bool // true = punctuation follows Chinese/English mode
	toolbarVisible     bool // true = toolbar visible
	imeActivated       bool // true = IME is activated (has focus)

	// Input state
	inputBuffer        string
	inputCursorPos     int    // 光标在 inputBuffer 中的字节位置（0 = 最左，len(inputBuffer) = 最右）
	preeditDisplay     string // 带音节分隔符的显示文本（如 "zhong'guo"），五笔时为空
	syllableBoundaries []int  // 音节边界在 inputBuffer 中的位置（如 [5] 表示位置 5 处有分隔符）
	candidates         []ui.Candidate
	currentPage        int
	totalPages         int
	candidatesPerPage  int

	// 临时英文模式状态
	tempEnglishMode   bool   // 是否处于临时英文模式
	tempEnglishBuffer string // 临时英文缓冲区

	// Caret position (from C++)
	caretX      int
	caretY      int
	caretHeight int
	caretValid  bool // true if we have received valid caret position (coordinates can be negative in multi-monitor)

	// Last known valid window position (for fallback)
	lastValidX int
	lastValidY int

	// Punctuation converter with state (for paired punctuation like quotes)
	punctConverter *transform.PunctuationConverter

	// Hotkey compiler for binary protocol
	hotkeyCompiler *hotkey.Compiler

	// 热键缓存（避免每次焦点变化重新编译）
	cachedKeyDownHotkeys []uint32
	cachedKeyUpHotkeys   []uint32
	hotkeysDirty         bool // 配置变化时置 true
}

// BridgeServer interface for broadcasting state to TSF clients
type BridgeServer interface {
	PushStateToAllClients(status *bridge.StatusUpdateData)
	PushCommitTextToActiveClient(text string) // Only send to active client for security
	RestartService()
}

// SetBridgeServer sets the bridge server for state broadcasting
func (c *Coordinator) SetBridgeServer(server BridgeServer) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.bridgeServer = server
}

// GetEffectiveMode returns the effective input mode considering CapsLock
// - Chinese mode + CapsLock OFF = Chinese
// - Chinese mode + CapsLock ON = English Upper (temporary English for caps)
// - English mode + CapsLock OFF = English Lower
// - English mode + CapsLock ON = English Upper
func (c *Coordinator) GetEffectiveMode() EffectiveMode {
	if c.capsLockOn {
		return ModeEnglishUpper
	}
	if c.chineseMode {
		return ModeChinese
	}
	return ModeEnglishLower
}

// GetEffectiveModeNoLock returns the effective input mode without acquiring lock
// Caller must hold the lock
func (c *Coordinator) getEffectiveModeNoLock() EffectiveMode {
	if c.capsLockOn {
		return ModeEnglishUpper
	}
	if c.chineseMode {
		return ModeChinese
	}
	return ModeEnglishLower
}

// IsCapsLockOn returns the current CapsLock state
func (c *Coordinator) IsCapsLockOn() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.capsLockOn
}

// buildStatusUpdate creates a StatusUpdateData from current state (caller must hold lock)
func (c *Coordinator) buildStatusUpdate() *bridge.StatusUpdateData {
	keyDownHotkeys, keyUpHotkeys := c.getCompiledHotkeys()
	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
}

// broadcastState broadcasts the current state to toolbar and all TSF clients
// This should be called after any state change. Caller must hold the lock.
func (c *Coordinator) broadcastState() {
	// 1. Update Go toolbar
	c.syncToolbarStateNoLock()

	// 2. Push state to all TSF clients
	if c.bridgeServer != nil {
		status := c.buildStatusUpdate()
		// Release lock before network I/O to avoid blocking
		c.mu.Unlock()
		c.bridgeServer.PushStateToAllClients(status)
		c.mu.Lock()
	}
}

// syncToolbarStateNoLock synchronizes the current state to the toolbar (without lock)
func (c *Coordinator) syncToolbarStateNoLock() {
	if c.uiManager == nil {
		return
	}

	// Use effective mode for toolbar display
	effectiveMode := c.getEffectiveModeNoLock()

	c.uiManager.UpdateToolbarState(ui.ToolbarState{
		ChineseMode:   effectiveMode == ModeChinese,
		FullWidth:     c.fullWidth,
		ChinesePunct:  c.chinesePunctuation,
		CapsLock:      c.capsLockOn,
		EffectiveMode: int(effectiveMode),
	})
}

// NewCoordinator creates a new Coordinator
func NewCoordinator(engineMgr *engine.Manager, uiManager *ui.Manager, cfg *config.Config, logger *slog.Logger) *Coordinator {
	candidatesPerPage := 9
	if cfg != nil && cfg.UI.CandidatesPerPage > 0 {
		candidatesPerPage = cfg.UI.CandidatesPerPage
	}

	// 确定初始状态
	startInChineseMode := true
	fullWidth := false
	chinesePunctuation := true
	punctFollowMode := false
	toolbarVisible := false

	if cfg != nil {
		// 检查是否启用"记忆前次状态"
		if cfg.Startup.RememberLastState {
			// 从 RuntimeState 加载前次状态
			state, err := config.LoadRuntimeState()
			if err != nil {
				logger.Warn("Failed to load runtime state, using defaults", "error", err)
				startInChineseMode = cfg.Startup.DefaultChineseMode
				fullWidth = cfg.Startup.DefaultFullWidth
				chinesePunctuation = cfg.Startup.DefaultChinesePunct
			} else {
				startInChineseMode = state.ChineseMode
				fullWidth = state.FullWidth
				chinesePunctuation = state.ChinesePunct
			}
		} else {
			// 使用默认配置
			startInChineseMode = cfg.Startup.DefaultChineseMode
			fullWidth = cfg.Startup.DefaultFullWidth
			chinesePunctuation = cfg.Startup.DefaultChinesePunct
		}

		punctFollowMode = cfg.Input.PunctFollowMode
		toolbarVisible = cfg.Toolbar.Visible
	}

	c := &Coordinator{
		engineMgr:          engineMgr,
		uiManager:          uiManager,
		logger:             logger,
		config:             cfg,
		chineseMode:        startInChineseMode,
		fullWidth:          fullWidth,
		chinesePunctuation: chinesePunctuation,
		punctFollowMode:    punctFollowMode,
		toolbarVisible:     toolbarVisible,
		inputBuffer:        "",
		candidates:         nil,
		currentPage:        1,
		totalPages:         1,
		candidatesPerPage:  candidatesPerPage,
		caretX:             100,
		caretY:             100,
		caretHeight:        20,
		punctConverter:     transform.NewPunctuationConverter(),
		hotkeyCompiler:     hotkey.NewCompiler(cfg),
		hotkeysDirty:       true, // 首次使用时需要编译
	}

	// Set up toolbar callbacks
	c.setupToolbarCallbacks()

	// Set up candidate window callbacks for mouse interaction
	c.setupCandidateCallbacks()

	// Initialize UI config (including debug options)
	if c.uiManager != nil && cfg != nil {
		c.uiManager.UpdateConfig(cfg.UI.FontSize, cfg.UI.FontPath, cfg.UI.HideCandidateWindow)
		// Set candidate layout (horizontal/vertical)
		if cfg.UI.CandidateLayout != "" {
			c.uiManager.SetCandidateLayout(cfg.UI.CandidateLayout)
		}
		// Set hide preedit when inline preedit is enabled
		c.uiManager.SetHidePreedit(cfg.UI.InlinePreedit)
		// Set status indicator config
		c.uiManager.UpdateStatusIndicatorConfig(
			cfg.UI.StatusIndicatorDuration,
			cfg.UI.StatusIndicatorOffsetX,
			cfg.UI.StatusIndicatorOffsetY,
		)
		// 设置编码提示延迟
		c.uiManager.SetTooltipDelay(cfg.UI.TooltipDelay)
		// 加载主题
		if cfg.UI.Theme != "" {
			c.uiManager.LoadTheme(cfg.UI.Theme)
		}
	}

	return c
}

// setupToolbarCallbacks sets up the callbacks for toolbar button clicks
// IMPORTANT: These callbacks are invoked from the UI thread (window procedure).
// We use goroutines to avoid blocking the UI thread with lock acquisition or I/O.
func (c *Coordinator) setupToolbarCallbacks() {
	if c.uiManager == nil {
		return
	}

	c.uiManager.SetToolbarCallbacks(&ui.ToolbarCallback{
		OnToggleMode: func() {
			// Run in goroutine to avoid blocking UI thread
			go c.handleToolbarToggleMode()
		},
		OnToggleWidth: func() {
			go c.handleToolbarToggleWidth()
		},
		OnTogglePunct: func() {
			go c.handleToolbarTogglePunct()
		},
		OnOpenSettings: func() {
			go c.handleToolbarOpenSettings()
		},
		OnPositionChanged: func(x, y int) {
			go c.handleToolbarPositionChanged(x, y)
		},
		OnContextMenu: func(action ui.ToolbarContextMenuAction) {
			go c.handleToolbarContextMenu(action)
		},
	})
}

// setupCandidateCallbacks sets up the callbacks for candidate window mouse interactions
// IMPORTANT: These callbacks are invoked from the UI thread (window procedure).
// We use goroutines to avoid blocking the UI thread with lock acquisition or I/O.
func (c *Coordinator) setupCandidateCallbacks() {
	if c.uiManager == nil {
		return
	}

	c.uiManager.SetCandidateCallbacks(&ui.CandidateCallback{
		OnSelect: func(index int) {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateSelect(index)
		},
		OnHoverChange: func(index, tooltipX, tooltipY int) {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateHoverChange(index, tooltipX, tooltipY)
		},
		OnMoveUp: func(index int) {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateMoveUp(index)
		},
		OnMoveDown: func(index int) {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateMoveDown(index)
		},
		OnMoveTop: func(index int) {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateMoveTop(index)
		},
		OnDelete: func(index int) {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateDelete(index)
		},
		OnOpenSettings: func() {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateOpenSettings()
		},
	})
}

// handleCandidateSelect handles candidate selection via mouse click
func (c *Coordinator) handleCandidateSelect(index int) {
	c.mu.Lock()

	// Convert page-local index to actual candidate index
	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index

	c.logger.Debug("Candidate selected via mouse", "pageIndex", index, "actualIndex", actualIndex, "currentPage", c.currentPage)

	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		c.logger.Warn("Invalid candidate index", "actualIndex", actualIndex, "candidateCount", len(c.candidates))
		c.mu.Unlock()
		return
	}

	candidate := c.candidates[actualIndex]
	text := candidate.Text

	// Apply full-width conversion if enabled
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	c.logger.Debug("Candidate selected via mouse click", "index", actualIndex)

	// Clear state and hide UI
	c.clearState()
	c.hideUI()

	// Get bridge server reference (release lock before network I/O)
	bridgeServer := c.bridgeServer
	c.mu.Unlock()

	// Send text to TSF via push pipe (only to active client for security)
	if bridgeServer != nil && text != "" {
		bridgeServer.PushCommitTextToActiveClient(text)
	}
}

// handleCandidateHoverChange handles hover state change
func (c *Coordinator) handleCandidateHoverChange(index, tooltipX, tooltipY int) {
	c.logger.Debug("Candidate hover changed", "index", index, "tooltipX", tooltipX, "tooltipY", tooltipY)

	// Refresh the candidate window to show/hide hover highlight
	if c.uiManager != nil {
		c.uiManager.RefreshCandidates()

		// Show/hide tooltip for encoding lookup
		if index >= 0 {
			c.uiManager.ShowTooltipForCandidate(index, tooltipX, tooltipY)
		} else {
			c.uiManager.HideTooltip()
		}
	}
}

// handleCandidateMoveUp handles move up action from context menu
func (c *Coordinator) handleCandidateMoveUp(index int) {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.logger.Debug("Candidate move up requested", "index", index)

	if index <= 0 || index >= len(c.candidates) {
		c.logger.Warn("Invalid candidate index for move up", "index", index)
		return
	}

	c.logger.Debug("Request to move candidate up", "index", index)

	// TODO: Implement candidate priority adjustment
	// This would require:
	// 1. Swap priority/weight with the previous candidate
	// 2. Save to user dictionary or priority file
	// 3. Refresh the candidate list
}

// handleCandidateMoveDown handles move down action from context menu
func (c *Coordinator) handleCandidateMoveDown(index int) {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.logger.Debug("Candidate move down requested", "index", index)

	if index < 0 || index >= len(c.candidates)-1 {
		c.logger.Warn("Invalid candidate index for move down", "index", index)
		return
	}

	c.logger.Debug("Request to move candidate down", "index", index)

	// TODO: Implement candidate priority adjustment
}

// handleCandidateMoveTop handles move to top action from context menu
func (c *Coordinator) handleCandidateMoveTop(index int) {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.logger.Debug("Candidate move to top requested", "index", index)

	if index <= 0 || index >= len(c.candidates) {
		c.logger.Warn("Invalid candidate index for move to top", "index", index)
		return
	}

	c.logger.Debug("Request to move candidate to top", "index", index)

	// TODO: Implement candidate priority adjustment
	// Set this candidate's priority to be highest
}

// handleCandidateDelete handles delete action from context menu
func (c *Coordinator) handleCandidateDelete(index int) {
	c.mu.Lock()

	c.logger.Debug("Candidate delete requested", "index", index)

	if index < 0 || index >= len(c.candidates) {
		c.logger.Warn("Invalid candidate index for delete", "index", index)
		c.mu.Unlock()
		return
	}

	c.mu.Unlock()

	c.logger.Debug("Request to delete user word", "index", index)

	// Show confirmation dialog
	// TODO: Implement confirmation dialog via UI manager
	// For now, just log the request
	// if confirmed {
	//     // Delete from user dictionary
	//     // Refresh candidate list
	// }
}

// handleCandidateOpenSettings handles open settings action from context menu
func (c *Coordinator) handleCandidateOpenSettings() {
	c.logger.Info("Opening settings from candidate context menu")
	if c.uiManager != nil {
		c.uiManager.OpenSettings()
	}
}

// handleToolbarToggleMode handles mode toggle from toolbar click
func (c *Coordinator) handleToolbarToggleMode() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.chineseMode = !c.chineseMode
	c.logger.Info("Mode toggled via toolbar", "chineseMode", c.chineseMode)

	// Clear any pending input when switching modes
	if len(c.inputBuffer) > 0 {
		c.clearState()
		c.hideUI()
	}

	// Sync punctuation with mode if enabled
	if c.punctFollowMode {
		c.chinesePunctuation = c.chineseMode
	}

	// Reset punctuation converter state
	c.punctConverter.Reset()

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

	// Broadcast state to toolbar and all TSF clients
	c.broadcastState()
}

// handleToolbarToggleWidth handles width toggle from toolbar click
func (c *Coordinator) handleToolbarToggleWidth() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.fullWidth = !c.fullWidth
	c.logger.Debug("Full-width toggled via toolbar", "fullWidth", c.fullWidth)

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

	// Broadcast state to toolbar and all TSF clients
	c.broadcastState()
}

// handleToolbarTogglePunct handles punctuation toggle from toolbar click
func (c *Coordinator) handleToolbarTogglePunct() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.chinesePunctuation = !c.chinesePunctuation
	c.logger.Debug("Chinese punctuation toggled via toolbar", "chinesePunctuation", c.chinesePunctuation)

	// Reset punctuation converter state
	c.punctConverter.Reset()

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

	// Broadcast state to toolbar and all TSF clients
	c.broadcastState()
}

// handleToolbarOpenSettings handles settings button click from toolbar
func (c *Coordinator) handleToolbarOpenSettings() {
	c.logger.Info("Opening settings from toolbar")
	if c.uiManager != nil {
		c.uiManager.OpenSettings()
	}
}

// handleToolbarPositionChanged handles toolbar position change (after dragging)
func (c *Coordinator) handleToolbarPositionChanged(x, y int) {
	c.logger.Debug("Toolbar position changed", "x", x, "y", y)
	c.saveToolbarPosition(x, y)
}

// handleToolbarContextMenu handles toolbar right-click context menu action
func (c *Coordinator) handleToolbarContextMenu(action ui.ToolbarContextMenuAction) {
	c.logger.Debug("Toolbar context menu action", "action", action)

	switch action {
	case ui.ToolbarMenuSettings:
		c.logger.Info("Opening settings from toolbar context menu")
		c.handleToolbarOpenSettings()

	case ui.ToolbarMenuRestartService:
		c.logger.Info("Restart service requested from toolbar context menu")
		c.resetAndResync()

	case ui.ToolbarMenuAbout:
		c.logger.Info("Opening about page from toolbar context menu")
		// Open settings with "about" parameter
		if c.uiManager != nil {
			c.uiManager.OpenSettingsWithPage("about")
		}
	}
}

// resetAndResync restarts the Go service process
// It starts a new process and exits the current one
func (c *Coordinator) resetAndResync() {
	c.logger.Info("Restarting Go service process...")

	// Clear current state and hide UI
	c.mu.Lock()
	c.clearState()
	c.hideUI()
	c.mu.Unlock()

	// Request process restart through the restart manager
	RequestRestart()
}

// syncToolbarState synchronizes the current state to the toolbar
// Note: This should be called with lock held, or use broadcastState() instead
func (c *Coordinator) syncToolbarState() {
	c.syncToolbarStateNoLock()
}

// saveToolbarPosition saves the toolbar position to config
func (c *Coordinator) saveToolbarPosition(x, y int) {
	if c.config == nil {
		return
	}

	c.config.Toolbar.PositionX = x
	c.config.Toolbar.PositionY = y

	if err := config.Save(c.config); err != nil {
		c.logger.Error("Failed to save toolbar position", "error", err)
	}
}

// HandleKeyEvent handles key events from C++ Bridge
// Returns a result indicating what action to take
func (c *Coordinator) HandleKeyEvent(data bridge.KeyEventData) *bridge.KeyEventResult {
	startTime := time.Now()

	c.mu.Lock()
	lockTime := time.Since(startTime)
	defer c.mu.Unlock()

	// Use Debug for high-frequency key events to reduce log noise
	c.logger.Debug("HandleKeyEvent", "key", data.Key, "keycode", data.KeyCode, "modifiers", data.Modifiers, "chineseMode", c.chineseMode, "lockWait", lockTime.String())

	// Check for Ctrl or Alt modifiers
	hasCtrl := data.Modifiers&ModCtrl != 0
	hasAlt := data.Modifiers&ModAlt != 0
	hasShift := data.Modifiers&ModShift != 0

	// Handle switch engine hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.SwitchEngine, hasCtrl, hasShift, hasAlt, data.KeyCode) {
		return c.handleEngineSwitchKey()
	}

	// Handle full-width toggle hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.ToggleFullWidth, hasCtrl, hasShift, hasAlt, data.KeyCode) {
		return c.handleToggleFullWidth()
	}

	// Handle punctuation toggle hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.TogglePunct, hasCtrl, hasShift, hasAlt, data.KeyCode) {
		return c.handleTogglePunct()
	}

	// Handle mode toggle keys (lshift, rshift, lctrl, rctrl, capslock)
	// IMPORTANT: This must be checked BEFORE the Ctrl/Alt pass-through check,
	// because lctrl/rctrl are toggle mode keys but also set hasCtrl=true
	if toggleKey := c.getToggleModeKey(data.KeyCode); toggleKey != "" {
		c.logger.Debug("Toggle mode key detected", "key", toggleKey, "keyCode", data.KeyCode,
			"isConfigured", c.config != nil && c.config.IsToggleModeKey(toggleKey),
			"configuredKeys", c.config.Hotkeys.ToggleModeKeys)
		if c.config != nil && c.config.IsToggleModeKey(toggleKey) {
			// 检查是否需要在切换前上屏已有内容
			// CommitOnSwitch: 上屏编码（而非候选词），因为用户切换到英文意味着想输入英文
			var commitText string
			if c.config.Hotkeys.CommitOnSwitch && len(c.inputBuffer) > 0 {
				// 只在从中文切换到英文时上屏
				if c.chineseMode {
					commitText = c.inputBuffer
					if c.fullWidth {
						commitText = transform.ToFullWidth(commitText)
					}
				}
			}

			c.chineseMode = !c.chineseMode
			c.logger.Debug("Mode toggled", "key", toggleKey, "chineseMode", c.chineseMode)

			// Clear any pending input when switching modes
			if len(c.inputBuffer) > 0 {
				c.clearState()
				c.hideUI()
			}

			// Sync punctuation with mode if enabled
			if c.punctFollowMode {
				c.chinesePunctuation = c.chineseMode
			}

			// Reset punctuation converter state when switching modes
			c.punctConverter.Reset()

			// Save runtime state if remember_last_state is enabled
			c.saveRuntimeState()

			// Show mode indicator
			c.showModeIndicator()

			// Broadcast state to toolbar and all TSF clients
			c.broadcastState()

			// Return mode_changed with optional commit text
			if commitText != "" {
				return &bridge.KeyEventResult{
					Type:        bridge.ResponseTypeInsertText,
					Text:        commitText,
					ModeChanged: true,
					ChineseMode: c.chineseMode,
				}
			}

			return &bridge.KeyEventResult{
				Type:        bridge.ResponseTypeModeChanged,
				ChineseMode: c.chineseMode,
			}
		} else if toggleKey == "capslock" {
			// CapsLock is not configured as mode toggle key, but we still need to show indicator
			// C++ side sets 0x8000 bit in modifiers to indicate "state notification only"
			// Use the CapsLock state from C++ side (data.Toggles) as it's more accurate
			capsLockOn := data.IsCapsLockOn()
			c.logger.Debug("CapsLock state notification", "on", capsLockOn)

			// Update CapsLock state and broadcast if changed
			if c.capsLockOn != capsLockOn {
				c.capsLockOn = capsLockOn
				c.broadcastState()
			}

			// Show CapsLock indicator (A/a) - use NoLock version since we already hold the lock
			c.handleCapsLockStateNoLock(capsLockOn)

			// Return Consumed to indicate we handled it (no mode change)
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		} else {
			// Toggle key recognized (lshift/rshift/lctrl/rctrl) but not configured
			// Consume the key to avoid passing Shift/Ctrl through to the application
			// This ensures consistent behavior: modifier key releases are always eaten by IME
			c.logger.Debug("Toggle key not configured, consuming", "key", toggleKey)
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
	}

	// Other Ctrl/Alt combinations should be passed to the system
	// (after checking toggle mode keys, since lctrl/rctrl are valid toggle keys)
	if hasCtrl || hasAlt {
		c.logger.Debug("Key has Ctrl/Alt modifier, passing to system")
		return nil
	}

	// Preserve original key for English mode (uppercase letters should stay uppercase)
	key := data.Key

	// English mode: pass through all keys directly to system
	if !c.chineseMode {
		// 纯英文模式：所有按键直接透传给系统，不经过 Go 处理
		return nil
	}

	// Chinese mode with CapsLock: output letters directly (no full-width)
	// CapsLock ON: letters are uppercase, Shift+letter are lowercase
	// This allows users to quickly type English while in Chinese mode
	// Use the CapsLock state from C++ side (data.Toggles) as it's more accurate
	if data.IsCapsLockOn() {
		if len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')) {
			// If there's pending input, commit it first then output the letter
			if len(c.inputBuffer) > 0 && len(c.candidates) > 0 {
				// Commit first candidate
				candidate := c.candidates[0]
				text := candidate.Text
				if c.fullWidth {
					text = transform.ToFullWidth(text)
				}
				c.clearState()
				c.hideUI()

				// Shift+letter = lowercase, letter = uppercase (CapsLock behavior)
				// Note: no full-width conversion for CapsLock English output
				var outputKey string
				if hasShift {
					outputKey = strings.ToLower(key)
				} else {
					outputKey = strings.ToUpper(key)
				}

				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: text + outputKey,
				}
			}

			// No pending input, just output letter
			c.clearState()
			c.hideUI()

			// Shift+letter = lowercase, letter = uppercase (CapsLock behavior)
			// Note: no full-width conversion for CapsLock English output
			var outputKey string
			if hasShift {
				outputKey = strings.ToLower(key)
			} else {
				outputKey = strings.ToUpper(key)
			}

			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: outputKey,
			}
		}
	}

	// 检查是否处于临时英文模式
	if c.tempEnglishMode {
		return c.handleTempEnglishKey(key, &data)
	}

	// 中文模式下，Shift+字母进入临时英文模式（CapsLock OFF 时）
	if c.chineseMode && !data.IsCapsLockOn() && hasShift {
		if c.config != nil && c.config.Input.ShiftTempEnglish.Enabled {
			if len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')) {
				return c.enterTempEnglishMode(key)
			}
		}
	}

	// Chinese mode handling
	switch {
	case data.KeyCode == 37: // Left arrow
		return c.handleCursorLeft()

	case data.KeyCode == 39: // Right arrow
		return c.handleCursorRight()

	case data.KeyCode == 36: // Home
		return c.handleCursorHome()

	case data.KeyCode == 35: // End
		return c.handleCursorEnd()

	case data.KeyCode == 8: // Backspace
		return c.handleBackspace()

	case data.KeyCode == 13: // Enter
		return c.handleEnter()

	case data.KeyCode == 27: // Escape
		return c.handleEscape()

	case data.KeyCode == 32: // Space
		return c.handleSpace()

	case c.isPageUpKey(key, data.KeyCode, uint32(data.Modifiers)):
		return c.handlePageUp()

	case c.isPageDownKey(key, data.KeyCode, uint32(data.Modifiers)):
		return c.handlePageDown()

	case len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')):
		// Chinese mode: convert to lowercase for pinyin
		return c.handleAlphaKey(strings.ToLower(key))

	case len(key) == 1 && key[0] >= '1' && key[0] <= '9':
		return c.handleNumberKey(int(key[0] - '0'))

	case c.isSelectKey2(key, data.KeyCode):
		// Handle 2nd candidate selection key (e.g., semicolon)
		if len(c.candidates) >= 2 && len(c.inputBuffer) > 0 {
			return c.selectCandidate(1) // Select 2nd candidate (index 1)
		}
		// If no candidates, treat as punctuation
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]))
		}
		return nil

	case c.isSelectKey3(key, data.KeyCode):
		// Handle 3rd candidate selection key (e.g., quote)
		if len(c.candidates) >= 3 && len(c.inputBuffer) > 0 {
			return c.selectCandidate(2) // Select 3rd candidate (index 2)
		}
		// If no candidates, treat as punctuation
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]))
		}
		return nil

	case len(key) == 1 && c.isPunctuation(rune(key[0])):
		return c.handlePunctuation(rune(key[0]))

	default:
		c.logger.Debug("Unhandled key", "key", key, "keycode", data.KeyCode)
		return nil
	}
}

func (c *Coordinator) handleAlphaKey(key string) *bridge.KeyEventResult {
	startTime := time.Now()
	// 在光标位置插入字符
	c.inputBuffer = c.inputBuffer[:c.inputCursorPos] + key + c.inputBuffer[c.inputCursorPos:]
	c.inputCursorPos += len(key)
	c.logger.Debug("Input buffer updated", "buffer", c.inputBuffer, "cursor", c.inputCursorPos)

	// 处理顶码（如五笔的五码顶字）
	if c.engineMgr != nil {
		commitText, newInput, shouldCommit := c.engineMgr.HandleTopCode(c.inputBuffer)
		if shouldCommit {
			c.inputBuffer = newInput
			c.inputCursorPos = len(newInput)
			c.logger.Debug("Top code commit", "newInputLen", len(newInput))

			// Apply full-width conversion if enabled
			if c.fullWidth {
				commitText = transform.ToFullWidth(commitText)
			}

			// 如果还有剩余输入，继续处理并更新候选
			if len(c.inputBuffer) > 0 {
				c.updateCandidates()
				c.showUI()
				// 返回带有新组合的响应，让 C++ 端同时插入文字并开始新组合
				return &bridge.KeyEventResult{
					Type:           bridge.ResponseTypeInsertText,
					Text:           commitText,
					NewComposition: c.inputBuffer,
				}
			} else {
				c.hideUI()
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: commitText,
				}
			}
		}
	}

	// 更新候选词
	updateStart := time.Now()
	result := c.updateCandidatesEx()
	updateElapsed := time.Since(updateStart)

	// 检查自动上屏
	if result != nil && result.ShouldCommit {
		text := result.CommitText
		// Apply full-width conversion if enabled
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}
		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	// 检查空码处理
	if result != nil && result.IsEmpty {
		if result.ShouldClear {
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}
		if result.ToEnglish {
			text := c.inputBuffer
			// Apply full-width conversion if enabled
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text,
			}
		}
	}

	showStart := time.Now()
	c.showUI()
	showElapsed := time.Since(showStart)

	totalElapsed := time.Since(startTime)
	c.logger.Debug("handleAlphaKey timing", "total", totalElapsed.String(), "updateCandidates", updateElapsed.String(), "showUI", showElapsed.String())

	// Handle Inline Preedit
	if c.config != nil && c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}

	// When InlinePreedit is disabled, we still need to tell TSF that we have an active
	// composition so that subsequent keys (ESC, Backspace, Enter) are intercepted.
	// Return UpdateComposition with empty text - TSF will set _isComposing=TRUE but
	// won't display anything in the application.
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     "",
		CaretPos: 0,
	}
}

func (c *Coordinator) handleBackspace() *bridge.KeyEventResult {
	if len(c.inputBuffer) > 0 && c.inputCursorPos > 0 {
		// 在光标位置删除前一个字符
		c.inputBuffer = c.inputBuffer[:c.inputCursorPos-1] + c.inputBuffer[c.inputCursorPos:]
		c.inputCursorPos--
		c.logger.Debug("Input buffer after backspace", "buffer", c.inputBuffer, "cursor", c.inputCursorPos)

		if len(c.inputBuffer) == 0 {
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}

		c.updateCandidates()
		c.showUI()

		// Handle Inline Preedit
		if c.config != nil && c.config.UI.InlinePreedit {
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.compositionText(),
				CaretPos: c.displayCursorPos(),
			}
		}

		// When InlinePreedit is disabled, still use UpdateComposition with empty text
		// so that TSF knows there's an active composition
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     "",
			CaretPos: 0,
		}
	}

	if len(c.inputBuffer) == 0 {
		// Buffer is already empty - pass through to system
		c.logger.Debug("Backspace with empty buffer, passing through to system")
		return nil
	}

	// Cursor at beginning but buffer not empty - consume the key (don't pass to system)
	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeConsumed,
	}
}

func (c *Coordinator) handleCursorLeft() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		return nil // 无输入时透传
	}
	if c.inputCursorPos > 0 {
		c.inputCursorPos--
		c.logger.Debug("Cursor left", "cursor", c.inputCursorPos)
		if c.config != nil && c.config.UI.InlinePreedit {
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.compositionText(),
				CaretPos: c.displayCursorPos(),
			}
		}
		// InlinePreedit 关闭时，刷新候选窗口中的光标位置
		c.showUI()
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleCursorRight() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		return nil // 无输入时透传
	}
	if c.inputCursorPos < len(c.inputBuffer) {
		c.inputCursorPos++
		c.logger.Debug("Cursor right", "cursor", c.inputCursorPos)
		if c.config != nil && c.config.UI.InlinePreedit {
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.compositionText(),
				CaretPos: c.displayCursorPos(),
			}
		}
		// InlinePreedit 关闭时，刷新候选窗口中的光标位置
		c.showUI()
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleCursorHome() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		return nil
	}
	c.inputCursorPos = 0
	c.logger.Debug("Cursor home", "cursor", c.inputCursorPos)
	if c.config != nil && c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}
	// InlinePreedit 关闭时，刷新候选窗口中的光标位置
	c.showUI()
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleCursorEnd() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		return nil
	}
	c.inputCursorPos = len(c.inputBuffer)
	c.logger.Debug("Cursor end", "cursor", c.inputCursorPos)
	if c.config != nil && c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}
	// InlinePreedit 关闭时，刷新候选窗口中的光标位置
	c.showUI()
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleEnter() *bridge.KeyEventResult {
	// Commit raw input as text
	if len(c.inputBuffer) > 0 {
		text := c.inputBuffer

		// Apply full-width conversion if enabled
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}

		c.clearState()
		c.hideUI()

		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}
	return nil
}

func (c *Coordinator) handleEscape() *bridge.KeyEventResult {
	// If toolbar context menu is open, close it and consume ESC
	if c.uiManager != nil && c.uiManager.IsToolbarMenuOpen() {
		c.logger.Debug("ESC closes toolbar context menu")
		c.uiManager.HideToolbarMenu()
		// Return Consumed to eat the ESC key (don't pass to app)
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	// If candidate context menu is open, close it and consume ESC
	if c.uiManager != nil && c.uiManager.IsCandidateMenuOpen() {
		c.logger.Debug("ESC closes candidate context menu")
		c.uiManager.HideCandidateMenu()
		// Return Consumed to eat the ESC key (don't pass to app)
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	if len(c.inputBuffer) > 0 {
		c.clearState()
		c.hideUI()
	}
	// Always return ClearComposition to ensure C++ side's _isComposing is reset
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

func (c *Coordinator) handleSpace() *bridge.KeyEventResult {
	// Select first candidate of current page
	if len(c.candidates) > 0 {
		// Calculate index of first candidate on current page
		index := (c.currentPage - 1) * c.candidatesPerPage
		if index < len(c.candidates) {
			return c.selectCandidate(index)
		}
	} else if len(c.inputBuffer) > 0 {
		// No candidates, commit raw input
		text := c.inputBuffer

		// Apply full-width conversion if enabled
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}

		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}
	return nil
}

func (c *Coordinator) handleNumberKey(num int) *bridge.KeyEventResult {
	// num is 1-9, convert to 0-based index within current page
	index := (c.currentPage-1)*c.candidatesPerPage + (num - 1)
	if index < len(c.candidates) {
		return c.selectCandidate(index)
	}
	return nil
}

func (c *Coordinator) handlePageUp() *bridge.KeyEventResult {
	// Pass through only if no candidates
	if len(c.candidates) == 0 {
		return nil
	}

	// Have candidates - always consume the key, even if at first page
	if c.currentPage > 1 {
		c.currentPage--
		c.logger.Debug("Page up", "currentPage", c.currentPage, "totalPages", c.totalPages)
		c.showUI()
	}
	// Return Consumed to indicate key was handled (don't pass to application)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handlePageDown() *bridge.KeyEventResult {
	// Pass through only if no candidates
	if len(c.candidates) == 0 {
		return nil
	}

	// Have candidates - always consume the key, even if at last page
	if c.currentPage < c.totalPages {
		c.currentPage++
		c.logger.Debug("Page down", "currentPage", c.currentPage, "totalPages", c.totalPages)
		c.showUI()
	}
	// Return Consumed to indicate key was handled (don't pass to application)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) selectCandidate(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return nil
	}

	cand := c.candidates[index]
	originalText := cand.Text
	text := originalText

	// Apply full-width conversion if enabled
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 拼音引擎部分上屏：候选消耗的输入长度小于缓冲区长度时，保留剩余部分
	isPinyin := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin
	if isPinyin && cand.ConsumedLength > 0 && cand.ConsumedLength < len(c.inputBuffer) {
		// 用户词频学习：命令候选不进入学习，避免污染用户词典（如 uuid）。
		if !cand.IsCommand {
			consumedCode := c.inputBuffer[:cand.ConsumedLength]
			c.engineMgr.OnCandidateSelected(consumedCode, originalText)
		}

		remaining := c.inputBuffer[cand.ConsumedLength:]
		c.logger.Debug("Partial commit (pinyin)", "index", index, "text", text,
			"consumed", cand.ConsumedLength, "remaining", remaining)

		// 更新缓冲区为剩余部分，光标重置到末尾，重新触发候选更新
		c.inputBuffer = remaining
		c.inputCursorPos = len(remaining)
		c.currentPage = 1
		c.updateCandidates()
		c.showUI()

		return &bridge.KeyEventResult{
			Type:           bridge.ResponseTypeInsertText,
			Text:           text,
			NewComposition: remaining,
		}
	}

	// 用户词频学习：命令候选不进入学习，避免污染用户词典（如 uuid）。
	if isPinyin && c.engineMgr != nil && !cand.IsCommand {
		c.engineMgr.OnCandidateSelected(c.inputBuffer, originalText)
	}

	c.logger.Debug("Candidate selected", "index", index, "original", originalText, "output", text, "fullWidth", c.fullWidth)

	c.clearState()
	c.hideUI()

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: text,
	}
}

// compositionText 返回当前应显示的组合文本。
// 拼音模式返回带音节分隔符的文本（如 "zhong'guo"），五笔或未解析时 fallback 到 inputBuffer。
func (c *Coordinator) compositionText() string {
	if c.preeditDisplay != "" {
		return c.preeditDisplay
	}
	return c.inputBuffer
}

// calcSyllableBoundaries 从已完成音节和部分音节计算边界位置。
// 边界位置是 inputBuffer 中每对相邻音节段之间的字节偏移。
// 例如 ["zhong", "guo"] partial="" → [5]；["ni", "hao"] partial="zh" → [2, 5]
func (c *Coordinator) calcSyllableBoundaries(completedSyllables []string, partialSyllable string) []int {
	segments := make([]string, 0, len(completedSyllables)+1)
	segments = append(segments, completedSyllables...)
	if partialSyllable != "" {
		segments = append(segments, partialSyllable)
	}
	if len(segments) <= 1 {
		return nil
	}
	boundaries := make([]int, 0, len(segments)-1)
	pos := 0
	for i := 0; i < len(segments)-1; i++ {
		pos += len(segments[i])
		boundaries = append(boundaries, pos)
	}
	return boundaries
}

// displayCursorPos 将 inputCursorPos（基于 inputBuffer 的字节位置）映射到 preeditDisplay 中的显示位置。
// 公式：displayPos = inputCursorPos + count(boundary <= inputCursorPos)
func (c *Coordinator) displayCursorPos() int {
	if c.preeditDisplay == "" {
		return c.inputCursorPos
	}
	offset := 0
	for _, b := range c.syllableBoundaries {
		if b <= c.inputCursorPos {
			offset++
		}
	}
	return c.inputCursorPos + offset
}

func (c *Coordinator) updateCandidates() {
	c.updateCandidatesEx()
}

func (c *Coordinator) updateCandidatesEx() *engine.ConvertResult {
	if len(c.inputBuffer) == 0 {
		c.candidates = nil
		return nil
	}

	if c.engineMgr == nil {
		return nil
	}

	// 使用扩展转换获取更多信息
	result := c.engineMgr.ConvertEx(c.inputBuffer, 50)

	// 更新预编辑显示状态
	c.preeditDisplay = result.PreeditDisplay
	// 安全校验：去除分隔符后应与 inputBuffer 一致，否则 fallback
	if c.preeditDisplay != "" {
		stripped := strings.ReplaceAll(c.preeditDisplay, "'", "")
		if stripped != strings.ToLower(c.inputBuffer) {
			c.preeditDisplay = ""
			c.syllableBoundaries = nil
		} else {
			c.syllableBoundaries = c.calcSyllableBoundaries(
				result.CompletedSyllables, result.PartialSyllable)
		}
	} else {
		c.syllableBoundaries = nil
	}

	// Convert to UI candidates
	c.candidates = make([]ui.Candidate, len(result.Candidates))
	for i, ec := range result.Candidates {
		cand := ui.Candidate{
			Text:           ec.Text,
			Index:          i + 1,
			Weight:         ec.Weight,
			IsCommand:      ec.IsCommand,
			ConsumedLength: ec.ConsumedLength,
		}
		// 如果有提示信息（如反查编码），添加到注释
		if ec.Hint != "" {
			cand.Comment = ec.Hint
		}
		c.candidates[i] = cand
	}

	c.logger.Debug("Got candidates", "count", len(c.candidates), "empty", result.IsEmpty)

	// Calculate pagination
	c.totalPages = (len(c.candidates) + c.candidatesPerPage - 1) / c.candidatesPerPage
	if c.totalPages == 0 {
		c.totalPages = 1
	}
	c.currentPage = 1

	return result
}

func (c *Coordinator) showUI() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		c.logger.Warn("UI manager not ready")
		return
	}

	// When InlinePreedit is enabled and there are no candidates,
	// hide the candidate window (only show the inline preedit in the application)
	if c.config != nil && c.config.UI.InlinePreedit && len(c.candidates) == 0 {
		c.hideUI()
		return
	}

	// Get current page candidates
	startIdx := (c.currentPage - 1) * c.candidatesPerPage
	endIdx := startIdx + c.candidatesPerPage
	if endIdx > len(c.candidates) {
		endIdx = len(c.candidates)
	}

	var pageCandidates []ui.Candidate
	if startIdx < len(c.candidates) {
		pageCandidates = c.candidates[startIdx:endIdx]
	}

	// Re-index for display (1-9)
	displayCandidates := make([]ui.Candidate, len(pageCandidates))
	for i, cand := range pageCandidates {
		displayCandidates[i] = ui.Candidate{
			Text:    cand.Text,
			Index:   i + 1,
			Comment: cand.Comment,
			Weight:  cand.Weight,
		}
	}

	// Use caret position for candidate window placement
	// The UI manager will handle boundary detection and position adjustment
	caretX := c.caretX
	caretY := c.caretY
	caretHeight := c.caretHeight

	// Multi-monitor support: coordinates can be negative (monitors to the left/above primary)
	// Only use fallback if we haven't received valid caret info yet (both X and Y are 0)
	// or if coordinates are extremely large (likely garbage values)
	const maxCoord = 32000 // Windows virtual screen limit is typically around 32767
	if (c.caretX == 0 && c.caretY == 0) || caretX > maxCoord || caretX < -maxCoord || caretY > maxCoord || caretY < -maxCoord {
		// Use last known good position or a reasonable default
		if c.lastValidX != 0 || c.lastValidY != 0 {
			caretX = c.lastValidX
			caretY = c.lastValidY
			caretHeight = 20 // Default height for fallback
		} else {
			// Fallback to a safe position on primary monitor
			caretX = 400
			caretY = 300
			caretHeight = 20
		}
		c.logger.Debug("Using fallback position", "caretX", caretX, "caretY", caretY)
	} else {
		// Save valid position for future fallback
		c.lastValidX = caretX
		c.lastValidY = caretY
	}

	c.uiManager.ShowCandidates(
		displayCandidates,
		c.compositionText(),
		c.displayCursorPos(),
		caretX,
		caretY,
		caretHeight,
		c.currentPage,
		c.totalPages,
	)
}

func (c *Coordinator) showModeIndicator() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// Show a brief indicator of the current mode
	modeText := "中"
	if !c.chineseMode {
		modeText = "En"
	}

	// Use caret position directly. The offset is applied in doShowModeIndicator.
	// Note: caretX, caretY from TSF is typically the top-left of the caret.
	// We pass this directly and let the user configure offset to position the indicator.
	x := c.caretX
	y := c.caretY
	const maxCoord = 32000
	if (c.caretX == 0 && c.caretY == 0) || x > maxCoord || x < -maxCoord || y > maxCoord || y < -maxCoord {
		if c.lastValidX != 0 || c.lastValidY != 0 {
			x = c.lastValidX
			y = c.lastValidY
		} else {
			x = 400
			y = 300
		}
	}

	c.uiManager.ShowModeIndicator(modeText, x, y)
}

func (c *Coordinator) hideUI() {
	if c.uiManager != nil {
		c.uiManager.Hide()
		c.uiManager.HideTooltip()
	}
}

func (c *Coordinator) clearState() {
	c.inputBuffer = ""
	c.inputCursorPos = 0
	c.preeditDisplay = ""
	c.syllableBoundaries = nil
	c.tempEnglishMode = false
	c.tempEnglishBuffer = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
}

// HandleCaretUpdate handles caret position updates from C++ Bridge
func (c *Coordinator) HandleCaretUpdate(data bridge.CaretData) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.caretX = data.X
	c.caretY = data.Y
	c.caretHeight = data.Height
	c.caretValid = true // Mark that we have received valid caret position

	c.logger.Debug("Caret position updated", "x", c.caretX, "y", c.caretY, "height", c.caretHeight)
	return nil
}

// HandleFocusLost handles focus lost events (real focus change, e.g., user clicked another window)
func (c *Coordinator) HandleFocusLost() {
	c.logger.Debug("Focus lost, clearing state and hiding toolbar")

	// 焦点变化后异步释放内存（非阻塞，不影响响应速度）
	defer func() {
		go func() {
			runtime.GC()
			debug.FreeOSMemory()
		}()
	}()

	// Hide toolbar on real focus lost (user switched to another window/app)
	c.SetIMEActivated(false)

	c.mu.Lock()
	defer c.mu.Unlock()
	c.clearState()
}

// HandleCompositionTerminated handles composition unexpectedly terminated events
// This happens when the user clicks within the input field to change cursor position,
// or when the application forcefully terminates the composition.
// Unlike HandleFocusLost, this does NOT hide the toolbar since the user is still
// in the same input field.
func (c *Coordinator) HandleCompositionTerminated() {
	c.logger.Debug("Composition terminated, clearing input state")

	c.mu.Lock()
	defer c.mu.Unlock()

	// Only clear input state and hide candidate window, keep toolbar visible
	c.clearState()
	c.hideUI()
}

// HandleIMEDeactivated handles IME being switched away (user selected another IME)
// This is called from TSF's Deactivate method, before the client disconnects
func (c *Coordinator) HandleIMEDeactivated() {
	c.logger.Info("IME deactivated (user switched to another IME), hiding toolbar")

	c.mu.Lock()
	c.imeActivated = false
	c.clearState()
	c.mu.Unlock()

	// Immediately hide the toolbar
	if c.uiManager != nil {
		c.uiManager.SetToolbarVisible(false)
		c.uiManager.Hide()
	}
}

// HandleClientDisconnected handles TSF client disconnection
// When all clients disconnect (activeClients == 0), hide the toolbar
func (c *Coordinator) HandleClientDisconnected(activeClients int) {
	c.logger.Debug("Client disconnected", "activeClients", activeClients)

	if activeClients == 0 {
		c.logger.Info("All TSF clients disconnected, hiding toolbar")
		c.mu.Lock()
		c.imeActivated = false
		c.mu.Unlock()

		// Hide toolbar and candidate window
		if c.uiManager != nil {
			c.uiManager.SetToolbarVisible(false)
			c.uiManager.Hide()
		}
	}
}

// getCompiledHotkeys returns compiled hotkey hashes for C++ side
// 使用缓存避免每次焦点变化重新编译
func (c *Coordinator) getCompiledHotkeys() (keyDownHotkeys, keyUpHotkeys []uint32) {
	if c.hotkeyCompiler == nil {
		return nil, nil
	}
	if !c.hotkeysDirty && c.cachedKeyDownHotkeys != nil {
		return c.cachedKeyDownHotkeys, c.cachedKeyUpHotkeys
	}
	c.cachedKeyDownHotkeys, c.cachedKeyUpHotkeys = c.hotkeyCompiler.Compile()
	c.hotkeysDirty = false
	c.logger.Debug("Compiled hotkeys for C++",
		"keyDownCount", len(c.cachedKeyDownHotkeys),
		"keyUpCount", len(c.cachedKeyUpHotkeys))
	return c.cachedKeyDownHotkeys, c.cachedKeyUpHotkeys
}

// HandleFocusGained handles focus gained events and returns current status
func (c *Coordinator) HandleFocusGained() *bridge.StatusUpdateData {
	c.logger.Debug("Focus gained")

	// 焦点变化后异步释放内存（非阻塞，不影响响应速度）
	defer func() {
		go func() {
			runtime.GC()
			debug.FreeOSMemory()
		}()
	}()

	// Clear any pending input state when focus changes
	// This ensures composition state is consistent
	c.mu.Lock()
	if len(c.inputBuffer) > 0 {
		c.inputBuffer = ""
		c.candidates = nil
		c.currentPage = 1
		c.totalPages = 1
		c.logger.Debug("Cleared input buffer on focus gained")
	}
	c.mu.Unlock()

	// Hide candidate window (will be shown again when user starts typing)
	c.hideUI()

	// Set IME as activated (this will show toolbar if enabled)
	c.SetIMEActivated(true)

	// Return current status so TSF can sync state (including compiled hotkeys)
	keyDownHotkeys, keyUpHotkeys := c.getCompiledHotkeys()
	c.mu.Lock()
	defer c.mu.Unlock()

	// Sync CapsLock state from system on focus gain
	c.capsLockOn = ui.GetCapsLockState()

	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
}

// HandleIMEActivated handles IME being switched back (user selected this IME again)
// This is called from TSF's Activate method
func (c *Coordinator) HandleIMEActivated() *bridge.StatusUpdateData {
	c.logger.Info("IME activated (user switched back to this IME)")

	// Clear any pending input state when IME is reactivated
	// This ensures composition state is consistent
	c.mu.Lock()
	if len(c.inputBuffer) > 0 {
		c.inputBuffer = ""
		c.candidates = nil
		c.currentPage = 1
		c.totalPages = 1
		c.logger.Debug("Cleared input buffer on IME activated")
	}
	c.mu.Unlock()

	// Hide candidate window (will be shown again when user starts typing)
	c.hideUI()

	// Set IME as activated (this will show toolbar if enabled)
	c.SetIMEActivated(true)

	// Return current status so TSF can sync state (including compiled hotkeys)
	keyDownHotkeys, keyUpHotkeys := c.getCompiledHotkeys()
	c.mu.Lock()
	defer c.mu.Unlock()

	// Sync CapsLock state from system on IME activation
	c.capsLockOn = ui.GetCapsLockState()

	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
}

// HandleCommitRequest handles a commit request from TSF (barrier mechanism)
// This is called when Space/Enter/number key is pressed during composition
func (c *Coordinator) HandleCommitRequest(data bridge.CommitRequestData) *bridge.CommitResultData {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.logger.Debug("Handling commit request",
		"barrierSeq", data.BarrierSeq,
		"triggerKey", data.TriggerKey,
		"inputBuffer", data.InputBuffer)

	var text string
	var newComposition string
	var modeChanged bool

	// Determine action based on trigger key
	switch data.TriggerKey {
	case 0x20: // VK_SPACE
		result := c.handleSpaceInternal()
		if result != nil {
			text = result.Text
			modeChanged = result.ModeChanged
			newComposition = result.NewComposition
		}

	case 0x0D: // VK_RETURN
		result := c.handleEnterInternal()
		if result != nil {
			text = result.Text
		}

	default:
		// Number keys 1-9 (VK codes 0x31-0x39)
		if data.TriggerKey >= 0x31 && data.TriggerKey <= 0x39 {
			num := int(data.TriggerKey - 0x30) // Convert VK code to number 1-9
			result := c.handleNumberKeyInternal(num)
			if result != nil {
				text = result.Text
				newComposition = result.NewComposition
			}
		}
	}

	return &bridge.CommitResultData{
		BarrierSeq:     data.BarrierSeq,
		Text:           text,
		NewComposition: newComposition,
		ModeChanged:    modeChanged,
		ChineseMode:    c.chineseMode,
	}
}

// handleSpaceInternal is the internal implementation of handleSpace (without lock)
func (c *Coordinator) handleSpaceInternal() *bridge.KeyEventResult {
	// Select first candidate of current page
	if len(c.candidates) > 0 {
		// Calculate index of first candidate on current page
		index := (c.currentPage - 1) * c.candidatesPerPage
		if index < len(c.candidates) {
			return c.selectCandidateInternal(index)
		}
	} else if len(c.inputBuffer) > 0 {
		// No candidates, commit raw input
		text := c.inputBuffer

		// Apply full-width conversion if enabled
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}

		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}
	return nil
}

// handleEnterInternal is the internal implementation of handleEnter (without lock)
func (c *Coordinator) handleEnterInternal() *bridge.KeyEventResult {
	if len(c.inputBuffer) > 0 {
		text := c.inputBuffer

		// Apply full-width conversion if enabled
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}

		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}
	return nil
}

// handleNumberKeyInternal is the internal implementation of handleNumberKey (without lock)
func (c *Coordinator) handleNumberKeyInternal(num int) *bridge.KeyEventResult {
	// num is 1-9, convert to 0-based index within current page
	index := (c.currentPage-1)*c.candidatesPerPage + (num - 1)
	if index < len(c.candidates) {
		return c.selectCandidateInternal(index)
	}
	return nil
}

// selectCandidateInternal is the internal implementation of selectCandidate (without lock)
func (c *Coordinator) selectCandidateInternal(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return nil
	}

	cand := c.candidates[index]
	c.logger.Debug("Candidate selected (internal)", "index", index)

	text := cand.Text

	// Apply full-width conversion if enabled
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 拼音引擎部分上屏
	originalText := cand.Text
	isPinyin := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin
	if isPinyin && cand.ConsumedLength > 0 && cand.ConsumedLength < len(c.inputBuffer) {
		// 用户词频学习：命令候选不进入学习，避免污染用户词典（如 uuid）。
		if !cand.IsCommand {
			consumedCode := c.inputBuffer[:cand.ConsumedLength]
			c.engineMgr.OnCandidateSelected(consumedCode, originalText)
		}

		remaining := c.inputBuffer[cand.ConsumedLength:]
		c.logger.Debug("Partial commit internal (pinyin)", "index", index, "text", text,
			"consumed", cand.ConsumedLength, "remaining", remaining)

		c.inputBuffer = remaining
		c.inputCursorPos = len(remaining)
		c.currentPage = 1
		c.updateCandidates()
		c.showUI()

		return &bridge.KeyEventResult{
			Type:           bridge.ResponseTypeInsertText,
			Text:           text,
			NewComposition: remaining,
		}
	}

	// 用户词频学习：命令候选不进入学习，避免污染用户词典（如 uuid）。
	if isPinyin && c.engineMgr != nil && !cand.IsCommand {
		c.engineMgr.OnCandidateSelected(c.inputBuffer, originalText)
	}

	c.clearState()
	c.hideUI()

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: text,
	}
}

// HandleModeNotify handles mode change notification from TSF (local toggle)
// This is called when TSF has already toggled the mode locally and is notifying Go
func (c *Coordinator) HandleModeNotify(data bridge.ModeNotifyData) {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.logger.Info("Mode notify from TSF", "chineseMode", data.ChineseMode, "clearInput", data.ClearInput)

	// Sync mode state from TSF
	c.chineseMode = data.ChineseMode

	// Clear input buffer if requested
	if data.ClearInput {
		c.clearState()
		c.hideUI()
	}

	// Sync punctuation with mode if enabled
	if c.punctFollowMode {
		c.chinesePunctuation = c.chineseMode
		c.punctConverter.Reset()
	}

	// Show mode indicator
	c.showModeIndicator()

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

	// Broadcast state to toolbar and all TSF clients
	c.broadcastState()
}

// HandleToggleMode toggles the input mode and returns the new state
func (c *Coordinator) HandleToggleMode() (commitText string, chineseMode bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

	// Check if CommitOnSwitch is enabled and there's pending input
	// When switching from Chinese to English, commit the raw input code (not the candidate)
	// because the user wants to type English, so we output the original typed characters
	if c.config != nil && c.config.Hotkeys.CommitOnSwitch && len(c.inputBuffer) > 0 {
		// Only commit when switching from Chinese to English
		if c.chineseMode {
			commitText = c.inputBuffer
			if c.fullWidth {
				commitText = transform.ToFullWidth(commitText)
			}
			c.logger.Debug("CommitOnSwitch: committing input code")
		}
	}

	c.chineseMode = !c.chineseMode
	c.logger.Debug("Mode toggled via IPC", "chineseMode", c.chineseMode, "hasCommitText", commitText != "")

	// Clear any pending input when switching modes
	if len(c.inputBuffer) > 0 {
		c.clearState()
		c.hideUI()
	}

	// Sync punctuation with mode if enabled
	if c.punctFollowMode {
		c.chinesePunctuation = c.chineseMode
		c.punctConverter.Reset()
	}

	// Show mode indicator
	c.showModeIndicator()

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeStateNoLock()

	// Broadcast state to toolbar and all TSF clients
	c.broadcastState()

	return commitText, c.chineseMode
}

// HandleCapsLockState shows Caps Lock indicator (A/a) and updates toolbar
func (c *Coordinator) HandleCapsLockState(on bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

	// Update capsLockOn state and broadcast if changed
	if c.capsLockOn != on {
		c.capsLockOn = on
		c.broadcastState()
	}

	c.handleCapsLockStateNoLock(on)
}

// handleCapsLockStateNoLock is the internal version without locking (caller must hold the lock)
func (c *Coordinator) handleCapsLockStateNoLock(on bool) {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// Show A for Caps Lock ON, a for OFF
	indicator := "a"
	if on {
		indicator = "A"
	}

	// Use valid position or fallback (multi-monitor: coordinates can be negative)
	x := c.caretX
	y := c.caretY + c.caretHeight + 5
	const maxCoord = 32000
	if (c.caretX == 0 && c.caretY == 0) || x > maxCoord || x < -maxCoord || y > maxCoord || y < -maxCoord {
		if c.lastValidX != 0 || c.lastValidY != 0 {
			x = c.lastValidX
			y = c.lastValidY
		} else {
			x = 400
			y = 300
		}
	}

	c.uiManager.ShowModeIndicator(indicator, x, y)
	// Note: Toolbar state is already updated by broadcastState() which is called
	// before handleCapsLockStateNoLock() in the CapsLock handling path.
	// We don't need to update it again here.
}

// handleEngineSwitchKey 处理引擎切换快捷键 (Ctrl+`)
func (c *Coordinator) handleEngineSwitchKey() *bridge.KeyEventResult {
	if c.engineMgr == nil {
		return nil
	}

	// 检查是否有输入需要清除
	hadInput := len(c.inputBuffer) > 0

	// 清除当前输入状态
	c.clearState()
	c.hideUI()

	// 切换引擎
	newType, err := c.engineMgr.ToggleEngine()
	if err != nil {
		c.logger.Error("Failed to switch engine", "error", err)
		return nil
	}

	c.logger.Info("Engine switched", "newType", newType)

	// 保存到用户配置
	go func() {
		if err := config.UpdateEngineType(string(newType)); err != nil {
			c.logger.Error("Failed to save engine type to config", "error", err)
		} else {
			c.logger.Debug("Engine type saved to config", "type", newType)
		}
	}()

	// 显示引擎指示器
	c.showEngineIndicator()

	// 返回 ClearComposition 让 C++ 端清除 _isComposing 状态
	if hadInput {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
	}
	return nil
}

// showEngineIndicator 显示引擎切换指示器
func (c *Coordinator) showEngineIndicator() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// 获取引擎显示名称
	engineName := c.engineMgr.GetEngineDisplayName()

	// Use valid position or fallback
	x := c.caretX
	y := c.caretY + c.caretHeight + 5
	const maxCoord = 32000
	if (c.caretX == 0 && c.caretY == 0) || x > maxCoord || x < -maxCoord || y > maxCoord || y < -maxCoord {
		if c.lastValidX != 0 || c.lastValidY != 0 {
			x = c.lastValidX
			y = c.lastValidY
		} else {
			x = 400
			y = 300
		}
	}

	c.uiManager.ShowModeIndicator(engineName, x, y)
}

// GetCurrentEngineName 获取当前引擎名称
func (c *Coordinator) GetCurrentEngineName() string {
	if c.engineMgr == nil {
		return "unknown"
	}
	return string(c.engineMgr.GetCurrentType())
}

// getCurrentEngineNameNoLock gets engine name without acquiring lock (caller must hold lock or ensure thread safety)
func (c *Coordinator) getCurrentEngineNameNoLock() string {
	if c.engineMgr == nil {
		return "unknown"
	}
	return string(c.engineMgr.GetCurrentType())
}

// UpdateUIConfig 更新 UI 配置（热更新）
func (c *Coordinator) UpdateUIConfig(uiConfig *config.UIConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if uiConfig == nil {
		return
	}

	// 更新每页候选数
	if uiConfig.CandidatesPerPage > 0 {
		c.candidatesPerPage = uiConfig.CandidatesPerPage
		// 重新计算总页数
		if len(c.candidates) > 0 {
			c.totalPages = (len(c.candidates) + c.candidatesPerPage - 1) / c.candidatesPerPage
			if c.currentPage > c.totalPages {
				c.currentPage = c.totalPages
			}
		}
	}

	// 更新配置引用
	if c.config != nil {
		c.config.UI = *uiConfig
	}

	// 通知 UI Manager 更新字体等设置
	if c.uiManager != nil {
		c.uiManager.UpdateConfig(uiConfig.FontSize, uiConfig.FontPath, uiConfig.HideCandidateWindow)
		// Update candidate layout
		if uiConfig.CandidateLayout != "" {
			c.uiManager.SetCandidateLayout(uiConfig.CandidateLayout)
		}
		// Update hide preedit setting
		c.uiManager.SetHidePreedit(uiConfig.InlinePreedit)
		// Update status indicator config
		c.uiManager.UpdateStatusIndicatorConfig(
			uiConfig.StatusIndicatorDuration,
			uiConfig.StatusIndicatorOffsetX,
			uiConfig.StatusIndicatorOffsetY,
		)
		// 设置编码提示延迟
		c.uiManager.SetTooltipDelay(uiConfig.TooltipDelay)
		// 更新主题
		if uiConfig.Theme != "" {
			c.uiManager.LoadTheme(uiConfig.Theme)
		}
	}

	c.logger.Debug("UI config updated", "candidatesPerPage", c.candidatesPerPage)
}

// UpdateToolbarConfig 更新工具栏配置（热更新）
func (c *Coordinator) UpdateToolbarConfig(toolbarConfig *config.ToolbarConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if toolbarConfig == nil {
		return
	}

	c.toolbarVisible = toolbarConfig.Visible

	// 更新配置引用
	if c.config != nil {
		c.config.Toolbar = *toolbarConfig
	}

	// 通知 UI Manager 更新工具栏状态
	if c.uiManager != nil {
		if c.toolbarVisible && c.imeActivated {
			c.uiManager.ShowToolbarWithState(toolbarConfig.PositionX, toolbarConfig.PositionY, ui.ToolbarState{
				ChineseMode:   c.chineseMode,
				FullWidth:     c.fullWidth,
				ChinesePunct:  c.chinesePunctuation,
				CapsLock:      c.capsLockOn,
				EffectiveMode: int(c.getEffectiveModeNoLock()),
			})
		} else {
			c.uiManager.SetToolbarVisible(false)
		}
	}

	c.logger.Debug("Toolbar config updated", "visible", c.toolbarVisible)
}

// UpdateInputConfig 更新输入配置（热更新）
// 注意：fullWidth 和 chinesePunctuation 是运行时状态，不从配置更新
func (c *Coordinator) UpdateInputConfig(inputConfig *config.InputConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if inputConfig == nil {
		return
	}

	// 只更新配置项，不更新运行时状态（fullWidth, chinesePunctuation）
	c.punctFollowMode = inputConfig.PunctFollowMode

	// 更新配置引用
	if c.config != nil {
		c.config.Input = *inputConfig
	}

	c.hotkeysDirty = true // SelectKeyGroups/PageKeys 变化也影响热键
	c.logger.Debug("Input config updated", "punctFollowMode", c.punctFollowMode)
}

// UpdateEngineConfig 更新引擎配置
func (c *Coordinator) UpdateEngineConfig(engineConfig *config.EngineConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if engineConfig == nil || c.engineMgr == nil {
		return
	}

	// 检查引擎类型是否改变
	currentType := c.engineMgr.GetCurrentType()
	newType := engine.EngineType(engineConfig.Type)

	if currentType != newType {
		// 清除当前输入状态
		c.clearState()
		c.hideUI()

		// 切换引擎
		if err := c.engineMgr.SwitchEngine(newType); err != nil {
			c.logger.Error("Failed to switch engine", "error", err, "targetType", newType)
		} else {
			c.logger.Info("Engine switched via config reload", "from", currentType, "to", newType)
		}
	}

	// 更新引擎选项
	c.engineMgr.UpdateFilterMode(engineConfig.FilterMode)
	c.engineMgr.UpdateWubiOptions(
		engineConfig.Wubi.AutoCommitAt4,
		engineConfig.Wubi.ClearOnEmptyAt4,
		engineConfig.Wubi.TopCodeCommit,
		engineConfig.Wubi.PunctCommit,
		engineConfig.Wubi.ShowCodeHint,
		engineConfig.Wubi.SingleCodeInput,
	)
	c.engineMgr.UpdatePinyinOptions(engineConfig.Pinyin.ShowWubiHint)

	// 更新配置引用
	if c.config != nil {
		c.config.Engine = *engineConfig
	}

	c.logger.Debug("Engine config updated", "type", engineConfig.Type, "filterMode", engineConfig.FilterMode)
}

// UpdateHotkeyConfig 更新快捷键配置
func (c *Coordinator) UpdateHotkeyConfig(hotkeyConfig *config.HotkeyConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if hotkeyConfig == nil {
		return
	}

	// 更新配置引用
	if c.config != nil {
		c.config.Hotkeys = *hotkeyConfig
	}

	// 重新编译快捷键（如果有编译器的话）
	if c.hotkeyCompiler != nil {
		c.hotkeyCompiler.UpdateConfig(c.config)
	}
	c.hotkeysDirty = true // 标记缓存失效，下次获取时重新编译

	c.logger.Debug("Hotkey config updated",
		"toggleModeKeys", hotkeyConfig.ToggleModeKeys,
		"switchEngine", hotkeyConfig.SwitchEngine)
}

// UpdateStartupConfig 更新启动配置
func (c *Coordinator) UpdateStartupConfig(startupConfig *config.StartupConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if startupConfig == nil {
		return
	}

	// 更新配置引用
	if c.config != nil {
		c.config.Startup = *startupConfig
	}

	c.logger.Debug("Startup config updated", "rememberLastState", startupConfig.RememberLastState)
}

// ClearInputState 清空输入状态（供外部调用）
func (c *Coordinator) ClearInputState() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.clearState()
	c.hideUI()
	c.logger.Debug("Input state cleared")
}

// SetIMEActivated sets the IME activation state
// When activated, show toolbar if enabled; when deactivated, hide toolbar
func (c *Coordinator) SetIMEActivated(activated bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

	wasActivated := c.imeActivated
	c.imeActivated = activated
	c.logger.Debug("IME activation", "activated", activated, "wasActivated", wasActivated)

	if c.uiManager == nil {
		return
	}

	if activated {
		// IME activated - show toolbar if enabled
		if c.toolbarVisible {
			// Always recalculate toolbar position based on current caret/focus position
			// This ensures toolbar follows the active screen when switching between apps
			toolbarWidth, toolbarHeight := 140, 30 // Base size, will be scaled by DPI
			var posX, posY int

			// Use caret position to determine which monitor to show toolbar on
			// Note: coordinates can be negative in multi-monitor setups, use caretValid flag
			if c.caretValid {
				posX, posY = ui.GetToolbarPositionForCaret(
					c.caretX, c.caretY,
					ui.ScaleIntForDPI(toolbarWidth),
					ui.ScaleIntForDPI(toolbarHeight),
				)
			} else {
				// No valid caret position yet, use mouse position as fallback
				posX, posY = ui.GetDefaultToolbarPosition(
					ui.ScaleIntForDPI(toolbarWidth),
					ui.ScaleIntForDPI(toolbarHeight),
				)
			}
			c.logger.Debug("Toolbar position calculated", "x", posX, "y", posY, "caretX", c.caretX, "caretY", c.caretY)

			// Show toolbar with position and state in one atomic operation
			c.uiManager.ShowToolbarWithState(posX, posY, ui.ToolbarState{
				ChineseMode:   c.chineseMode,
				FullWidth:     c.fullWidth,
				ChinesePunct:  c.chinesePunctuation,
				CapsLock:      c.capsLockOn,
				EffectiveMode: int(c.getEffectiveModeNoLock()),
			})
		}
	} else {
		// IME deactivated - always hide toolbar
		c.uiManager.SetToolbarVisible(false)
		// Also hide candidate window
		c.hideUI()
	}
}

// HandleMenuCommand handles menu commands from C++ (toggle_mode, toggle_width, toggle_punct, open_settings, toggle_toolbar)
func (c *Coordinator) HandleMenuCommand(command string) *bridge.StatusUpdateData {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.logger.Info("HandleMenuCommand", "command", command)

	needBroadcast := false

	switch command {
	case "toggle_mode":
		c.chineseMode = !c.chineseMode
		c.logger.Debug("Mode toggled via menu", "chineseMode", c.chineseMode)

		// Clear any pending input when switching modes
		if len(c.inputBuffer) > 0 {
			c.clearState()
			c.hideUI()
		}

		// Sync punctuation with mode if enabled
		if c.punctFollowMode {
			c.chinesePunctuation = c.chineseMode
		}

		// Reset punctuation converter state when switching modes
		c.punctConverter.Reset()

		// Save runtime state
		c.saveRuntimeState()

		// Show mode indicator
		c.showModeIndicator()

		needBroadcast = true

	case "toggle_width":
		c.fullWidth = !c.fullWidth
		c.logger.Debug("Full-width toggled via menu", "fullWidth", c.fullWidth)

		// Show indicator
		indicator := "半"
		if c.fullWidth {
			indicator = "全"
		}
		c.showIndicator(indicator)

		// Save runtime state
		c.saveRuntimeState()

		needBroadcast = true

	case "toggle_punct":
		c.chinesePunctuation = !c.chinesePunctuation
		c.logger.Debug("Chinese punctuation toggled via menu", "chinesePunctuation", c.chinesePunctuation)

		// Reset punctuation converter state
		c.punctConverter.Reset()

		// Show indicator
		indicator := "英."
		if c.chinesePunctuation {
			indicator = "中，"
		}
		c.showIndicator(indicator)

		// Save runtime state
		c.saveRuntimeState()

		needBroadcast = true

	case "toggle_toolbar":
		c.toolbarVisible = !c.toolbarVisible
		c.logger.Debug("Toolbar visibility toggled via menu", "toolbarVisible", c.toolbarVisible)

		// Update UI
		if c.uiManager != nil {
			if c.toolbarVisible && c.imeActivated {
				// Calculate position based on current caret
				// Note: coordinates can be negative in multi-monitor setups, use caretValid flag
				toolbarWidth, toolbarHeight := 140, 30
				var posX, posY int
				if c.caretValid {
					posX, posY = ui.GetToolbarPositionForCaret(
						c.caretX, c.caretY,
						ui.ScaleIntForDPI(toolbarWidth),
						ui.ScaleIntForDPI(toolbarHeight),
					)
				} else {
					posX, posY = ui.GetDefaultToolbarPosition(
						ui.ScaleIntForDPI(toolbarWidth),
						ui.ScaleIntForDPI(toolbarHeight),
					)
				}
				c.uiManager.ShowToolbarWithState(posX, posY, ui.ToolbarState{
					ChineseMode:   c.chineseMode,
					FullWidth:     c.fullWidth,
					ChinesePunct:  c.chinesePunctuation,
					CapsLock:      c.capsLockOn,
					EffectiveMode: int(c.getEffectiveModeNoLock()),
				})
			} else {
				c.uiManager.SetToolbarVisible(false)
			}
		}

		// Save to config
		c.saveToolbarConfig()

		needBroadcast = true

	case "open_settings":
		c.logger.Info("Opening settings requested")
		// Open settings window (will be implemented in UI)
		if c.uiManager != nil {
			c.uiManager.OpenSettings()
		}

	case "open_dictionary":
		c.logger.Info("Opening dictionary manager requested")
		// TODO: Open dictionary manager dialog
		// if c.uiManager != nil {
		//     c.uiManager.OpenDictionaryManager()
		// }

	case "show_about":
		c.logger.Info("Showing about dialog requested")
		// TODO: Show about dialog
		// if c.uiManager != nil {
		//     c.uiManager.ShowAboutDialog()
		// }

	case "exit":
		c.logger.Info("Exit requested from menu")
		// TODO: Signal application exit
		// For now, just log the request
	}

	// Broadcast state to all clients if needed
	if needBroadcast {
		c.broadcastState()
	}

	// Return current status
	return c.buildStatusUpdate()
}

// getStatusUpdate returns the current status (caller must hold lock)
func (c *Coordinator) getStatusUpdate() *bridge.StatusUpdateData {
	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
	}
}

// showIndicator shows a brief indicator text
func (c *Coordinator) showIndicator(text string) {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// Use valid position or fallback
	x := c.caretX
	y := c.caretY + c.caretHeight + 5
	const maxCoord = 32000
	if (c.caretX == 0 && c.caretY == 0) || x > maxCoord || x < -maxCoord || y > maxCoord || y < -maxCoord {
		if c.lastValidX != 0 || c.lastValidY != 0 {
			x = c.lastValidX
			y = c.lastValidY
		} else {
			x = 400
			y = 300
		}
	}

	c.uiManager.ShowModeIndicator(text, x, y)
}

// saveToolbarConfig saves the toolbar configuration to file
func (c *Coordinator) saveToolbarConfig() {
	go func() {
		cfg, err := config.Load()
		if err != nil {
			cfg = config.DefaultConfig()
		}

		cfg.Toolbar.Visible = c.toolbarVisible

		if err := config.Save(cfg); err != nil {
			c.logger.Error("Failed to save toolbar config", "error", err)
		} else {
			c.logger.Debug("Toolbar config saved")
		}
	}()
}

// GetFullWidth returns the current full-width mode state
func (c *Coordinator) GetFullWidth() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.fullWidth
}

// GetChinesePunctuation returns the current Chinese punctuation mode state
func (c *Coordinator) GetChinesePunctuation() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.chinesePunctuation
}

// GetToolbarVisible returns the current toolbar visibility state
func (c *Coordinator) GetToolbarVisible() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.toolbarVisible
}

// GetChineseMode returns the current Chinese mode state
func (c *Coordinator) GetChineseMode() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.chineseMode
}

// TransformOutput applies full-width and punctuation transformations to output text
func (c *Coordinator) TransformOutput(text string) string {
	c.mu.Lock()
	defer c.mu.Unlock()

	result := text

	// Apply full-width conversion if enabled
	if c.fullWidth {
		result = transform.ToFullWidth(result)
	}

	return result
}

// TransformPunctuation transforms a punctuation character based on current settings
func (c *Coordinator) TransformPunctuation(r rune) (string, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if !c.chinesePunctuation {
		return string(r), false
	}

	// Use punctuation converter which handles paired punctuation (quotes)
	return c.punctConverter.ToChinesePunctStr(r)
}

// isSelectKey2 checks if the key is configured as the 2nd candidate selection key
func (c *Coordinator) isSelectKey2(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}

	// 根据选择键组配置检查
	for _, group := range c.config.Input.SelectKeyGroups {
		switch group {
		case "semicolon_quote":
			if key == ";" || keyCode == 186 { // VK_OEM_1
				return true
			}
		case "comma_period":
			if key == "," || keyCode == 188 { // VK_OEM_COMMA
				return true
			}
		case "lrshift":
			if keyCode == 160 { // VK_LSHIFT
				return true
			}
		case "lrctrl":
			if keyCode == 162 { // VK_LCONTROL
				return true
			}
		}
	}
	return false
}

// isSelectKey3 checks if the key is configured as the 3rd candidate selection key
func (c *Coordinator) isSelectKey3(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}

	// 根据选择键组配置检查
	for _, group := range c.config.Input.SelectKeyGroups {
		switch group {
		case "semicolon_quote":
			if key == "'" || keyCode == 222 { // VK_OEM_7
				return true
			}
		case "comma_period":
			if key == "." || keyCode == 190 { // VK_OEM_PERIOD
				return true
			}
		case "lrshift":
			if keyCode == 161 { // VK_RSHIFT
				return true
			}
		case "lrctrl":
			if keyCode == 163 { // VK_RCONTROL
				return true
			}
		}
	}
	return false
}

// isPunctuation checks if a character is a punctuation mark that can be converted
func (c *Coordinator) isPunctuation(r rune) bool {
	// Check common punctuation marks
	switch r {
	case ',', '.', '?', '!', ':', ';', '\'', '"',
		'(', ')', '[', ']', '{', '}', '<', '>',
		'~', '@', '$', '`', '^', '_':
		return true
	}
	return false
}

// handlePunctuation handles punctuation input in Chinese mode
// If no input buffer, directly output punctuation (converted if chinese punctuation is enabled)
// If there's input buffer and punct_commit is enabled, commit current candidate and then output punctuation
func (c *Coordinator) handlePunctuation(r rune) *bridge.KeyEventResult {
	c.logger.Debug("handlePunctuation", "char", string(r), "buffer", c.inputBuffer)

	// If there's input in buffer, check if we should commit first (punct_commit)
	if len(c.inputBuffer) > 0 && len(c.candidates) > 0 {
		// Check if punct_commit is enabled in wubi config
		punctCommit := false
		if c.config != nil && c.config.Engine.Type == "wubi" {
			punctCommit = c.config.Engine.Wubi.PunctCommit
		}

		if punctCommit {
			// Commit first candidate, then output punctuation
			candidate := c.candidates[0]
			text := candidate.Text

			// Apply full-width conversion if enabled
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}

			// Convert punctuation
			punctText := string(r)
			if c.chinesePunctuation {
				var converted bool
				punctText, converted = c.punctConverter.ToChinesePunctStr(r)
				if !converted {
					punctText = string(r)
				}
			}

			// Apply full-width conversion to punctuation if enabled
			if c.fullWidth {
				punctText = transform.ToFullWidth(punctText)
			}

			c.clearState()
			c.hideUI()

			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text + punctText,
			}
		}
	}

	// If there's input buffer but punct_commit is not enabled, just let it pass through
	if len(c.inputBuffer) > 0 {
		return nil
	}

	// No input buffer - directly handle punctuation
	punctText := string(r)
	if c.chinesePunctuation {
		var converted bool
		punctText, converted = c.punctConverter.ToChinesePunctStr(r)
		if !converted {
			punctText = string(r)
		}
	}

	// Apply full-width conversion if enabled
	if c.fullWidth {
		punctText = transform.ToFullWidth(punctText)
	}

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: punctText,
	}
}

// handleToggleFullWidth handles the full-width toggle hotkey (e.g., Shift+Space)
func (c *Coordinator) handleToggleFullWidth() *bridge.KeyEventResult {
	c.fullWidth = !c.fullWidth
	c.logger.Debug("Full-width toggled via hotkey", "fullWidth", c.fullWidth)

	// Show indicator
	indicator := "半"
	if c.fullWidth {
		indicator = "全"
	}
	c.showIndicator(indicator)

	// Update toolbar state
	c.syncToolbarState()

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

	// Consume the key (don't let it pass through)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

// handleTogglePunct handles the punctuation toggle hotkey (e.g., Ctrl+.)
func (c *Coordinator) handleTogglePunct() *bridge.KeyEventResult {
	c.chinesePunctuation = !c.chinesePunctuation
	c.logger.Debug("Chinese punctuation toggled via hotkey", "chinesePunctuation", c.chinesePunctuation)

	// Reset punctuation converter state
	c.punctConverter.Reset()

	// Show indicator
	indicator := "英."
	if c.chinesePunctuation {
		indicator = "中，"
	}
	c.showIndicator(indicator)

	// Update toolbar state
	c.syncToolbarState()

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

	// Consume the key (don't let it pass through)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

// getToggleModeKey maps keycode to toggle mode key name
func (c *Coordinator) getToggleModeKey(keyCode int) string {
	switch keyCode {
	case 160: // VK_LSHIFT
		return "lshift"
	case 161: // VK_RSHIFT
		return "rshift"
	case 16: // VK_SHIFT (generic shift)
		return "lshift" // 默认作为左Shift处理
	case 162: // VK_LCONTROL
		return "lctrl"
	case 163: // VK_RCONTROL
		return "rctrl"
	case 20: // VK_CAPITAL (Caps Lock)
		return "capslock"
	}
	return ""
}

// saveRuntimeState saves the current state if remember_last_state is enabled
func (c *Coordinator) saveRuntimeState() {
	if c.config == nil || !c.config.Startup.RememberLastState {
		return
	}

	go func() {
		state := &config.RuntimeState{
			ChineseMode:  c.chineseMode,
			FullWidth:    c.fullWidth,
			ChinesePunct: c.chinesePunctuation,
			EngineType:   c.GetCurrentEngineName(),
		}
		if err := config.SaveRuntimeState(state); err != nil {
			c.logger.Error("Failed to save runtime state", "error", err)
		} else {
			c.logger.Debug("Runtime state saved", "chineseMode", state.ChineseMode)
		}
	}()
}

// saveRuntimeStateNoLock saves runtime state without acquiring lock (caller must hold lock)
func (c *Coordinator) saveRuntimeStateNoLock() {
	if c.config == nil || !c.config.Startup.RememberLastState {
		return
	}

	// Capture values while we hold the lock
	state := &config.RuntimeState{
		ChineseMode:  c.chineseMode,
		FullWidth:    c.fullWidth,
		ChinesePunct: c.chinesePunctuation,
		EngineType:   c.getCurrentEngineNameNoLock(),
	}

	go func() {
		if err := config.SaveRuntimeState(state); err != nil {
			c.logger.Error("Failed to save runtime state", "error", err)
		} else {
			c.logger.Debug("Runtime state saved", "chineseMode", state.ChineseMode)
		}
	}()
}

// isPageUpKey checks if the key is configured as a page up key
func (c *Coordinator) isPageUpKey(key string, keyCode int, modifiers uint32) bool {
	if c.config == nil {
		// 默认支持 PageUp 和 - 键
		return key == "page_up" || keyCode == 33 || keyCode == 189
	}

	hasShift := modifiers&ModShift != 0

	for _, pk := range c.config.Input.PageKeys {
		switch pk {
		case "pageupdown":
			if key == "page_up" || keyCode == 33 { // VK_PRIOR
				return true
			}
		case "minus_equal":
			if keyCode == 189 { // VK_OEM_MINUS
				return true
			}
		case "brackets":
			if keyCode == 219 { // VK_OEM_4 ([)
				return true
			}
		case "shift_tab":
			// Shift+Tab = page up
			if keyCode == 9 && hasShift { // VK_TAB with Shift
				return true
			}
		}
	}
	return false
}

// isPageDownKey checks if the key is configured as a page down key
func (c *Coordinator) isPageDownKey(key string, keyCode int, modifiers uint32) bool {
	if c.config == nil {
		// 默认支持 PageDown 和 = 键
		return key == "page_down" || keyCode == 34 || keyCode == 187
	}

	hasShift := modifiers&ModShift != 0

	for _, pk := range c.config.Input.PageKeys {
		switch pk {
		case "pageupdown":
			if key == "page_down" || keyCode == 34 { // VK_NEXT
				return true
			}
		case "minus_equal":
			if keyCode == 187 { // VK_OEM_PLUS (=)
				return true
			}
		case "brackets":
			if keyCode == 221 { // VK_OEM_6 (])
				return true
			}
		case "shift_tab":
			// Tab without Shift = page down
			if keyCode == 9 && !hasShift { // VK_TAB without Shift
				return true
			}
		}
	}
	return false
}

// matchHotkey checks if the current key event matches the configured hotkey string
// Supported formats: "ctrl+`", "shift+space", "ctrl+.", "ctrl+shift+e", "none", ""
func (c *Coordinator) matchHotkey(hotkeyStr string, hasCtrl, hasShift, hasAlt bool, keyCode int) bool {
	if hotkeyStr == "" || hotkeyStr == "none" {
		return false
	}

	// Parse the hotkey string
	needCtrl := false
	needShift := false
	needAlt := false
	var targetKeyCode int

	// Parse modifiers and key
	switch hotkeyStr {
	case "ctrl+`":
		needCtrl = true
		targetKeyCode = 192 // VK_OEM_3
	case "ctrl+shift+e":
		needCtrl = true
		needShift = true
		targetKeyCode = 69 // VK_E
	case "shift+space":
		needShift = true
		targetKeyCode = 32 // VK_SPACE
	case "ctrl+shift+space":
		needCtrl = true
		needShift = true
		targetKeyCode = 32 // VK_SPACE
	case "ctrl+.":
		needCtrl = true
		targetKeyCode = 190 // VK_OEM_PERIOD
	case "ctrl+,":
		needCtrl = true
		targetKeyCode = 188 // VK_OEM_COMMA
	default:
		// Unknown hotkey format
		c.logger.Debug("Unknown hotkey format", "hotkey", hotkeyStr)
		return false
	}

	// Check if all modifiers match
	if needCtrl != hasCtrl || needShift != hasShift || needAlt != hasAlt {
		return false
	}

	// Check if the key matches
	return keyCode == targetKeyCode
}

// enterTempEnglishMode 进入临时英文模式
func (c *Coordinator) enterTempEnglishMode(key string) *bridge.KeyEventResult {
	c.tempEnglishMode = true
	c.tempEnglishBuffer = strings.ToUpper(key) // Shift+字母输出大写

	c.logger.Debug("Entered temp English mode", "buffer", c.tempEnglishBuffer)

	// 显示临时英文模式 UI
	c.showTempEnglishUI()

	// 返回 UpdateComposition 让 C++ 端知道进入了 composing 状态
	// 这样后续的 Backspace/Enter 才会被发送到 Go 端处理
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     c.tempEnglishBuffer,
		CaretPos: len(c.tempEnglishBuffer),
	}
}

// handleTempEnglishKey 处理临时英文模式下的按键
func (c *Coordinator) handleTempEnglishKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	hasShift := data.Modifiers&ModShift != 0

	switch {
	case data.KeyCode == 8: // Backspace
		if len(c.tempEnglishBuffer) > 0 {
			c.tempEnglishBuffer = c.tempEnglishBuffer[:len(c.tempEnglishBuffer)-1]
			if len(c.tempEnglishBuffer) == 0 {
				return c.exitTempEnglishMode(false, "")
			}
			c.showTempEnglishUI()
			// 返回 UpdateComposition 保持 composing 状态
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.tempEnglishBuffer,
				CaretPos: len(c.tempEnglishBuffer),
			}
		}
		// 缓冲区已空，返回 ClearComposition
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}

	case data.KeyCode == 27: // Escape
		return c.exitTempEnglishMode(false, "")

	case data.KeyCode == 32: // Space
		// 上屏缓冲内容
		text := c.tempEnglishBuffer
		return c.exitTempEnglishMode(true, text)

	case data.KeyCode == 13: // Enter
		// Enter 也上屏缓冲内容
		return c.exitTempEnglishMode(true, c.tempEnglishBuffer)

	case len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')):
		// 追加字母：Shift+字母=大写，字母=小写
		var letter string
		if hasShift {
			letter = strings.ToUpper(key)
		} else {
			letter = strings.ToLower(key)
		}
		c.tempEnglishBuffer += letter
		c.logger.Debug("Temp English buffer updated", "buffer", c.tempEnglishBuffer)
		c.showTempEnglishUI()
		// 返回 UpdateComposition 保持 composing 状态
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.tempEnglishBuffer,
			CaretPos: len(c.tempEnglishBuffer),
		}

	case len(key) == 1 && key[0] >= '0' && key[0] <= '9':
		// 数字：当前没有英文候选，上屏缓冲内容并输出数字
		// （如果将来有英文词库/候选，数字应该用于选择候选）
		if len(c.tempEnglishBuffer) > 0 {
			text := c.tempEnglishBuffer
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}
			c.tempEnglishMode = false
			c.tempEnglishBuffer = ""
			c.hideUI()
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text + key,
			}
		}
		// 缓冲区为空，退出并透传数字
		c.exitTempEnglishMode(false, "")
		return nil
	}

	// 其他按键（如标点）：上屏缓冲内容，然后处理按键
	if len(c.tempEnglishBuffer) > 0 {
		text := c.tempEnglishBuffer
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}
		c.tempEnglishMode = false
		c.tempEnglishBuffer = ""
		c.hideUI()
		// 如果是标点，处理标点
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			punctResult := c.handlePunctuation(rune(key[0]))
			if punctResult != nil {
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: text + punctResult.Text,
				}
			}
		}
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	// 缓冲区为空，退出临时英文模式并透传
	c.exitTempEnglishMode(false, "")
	return nil
}

// exitTempEnglishMode 退出临时英文模式
func (c *Coordinator) exitTempEnglishMode(commit bool, text string) *bridge.KeyEventResult {
	c.tempEnglishMode = false
	c.tempEnglishBuffer = ""
	c.candidates = nil
	c.currentPage = 0
	c.totalPages = 0
	c.hideUI()

	c.logger.Debug("Exited temp English mode", "commit", commit, "textLen", len(text))

	if commit && len(text) > 0 {
		// 应用全角转换（如果启用）
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	// 取消时返回 ClearComposition 让 C++ 端清除 composing 状态
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// showTempEnglishUI 显示临时英文模式的 UI
func (c *Coordinator) showTempEnglishUI() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// 使用光标位置
	caretX := c.caretX
	caretY := c.caretY
	caretHeight := c.caretHeight

	const maxCoord = 32000
	if (c.caretX == 0 && c.caretY == 0) || caretX > maxCoord || caretX < -maxCoord || caretY > maxCoord || caretY < -maxCoord {
		if c.lastValidX != 0 || c.lastValidY != 0 {
			caretX = c.lastValidX
			caretY = c.lastValidY
			caretHeight = 20
		} else {
			caretX = 400
			caretY = 300
			caretHeight = 20
		}
	}

	// 显示临时英文缓冲区内容（无候选词）
	c.uiManager.ShowCandidates(
		nil, // 无候选词
		c.tempEnglishBuffer,
		len(c.tempEnglishBuffer), // 光标在末尾
		caretX,
		caretY,
		caretHeight,
		1, // currentPage
		1, // totalPages
	)
}
