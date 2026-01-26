// Package coordinator orchestrates communication between C++ Bridge, Engine, and UI
package coordinator

import (
	"log/slog"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/config"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
)

// Modifier key flags (must match C++ side)
const (
	ModShift = 0x01
	ModCtrl  = 0x02
	ModAlt   = 0x04
)

// Coordinator orchestrates between C++ Bridge, Engine, and native UI
type Coordinator struct {
	engineMgr *engine.Manager
	uiManager *ui.Manager
	logger    *slog.Logger
	config    *config.Config

	mu sync.Mutex

	// Input mode state
	chineseMode bool // true = Chinese, false = English

	// Full-width and punctuation state
	fullWidth          bool // true = full-width, false = half-width
	chinesePunctuation bool // true = Chinese punctuation, false = English punctuation
	punctFollowMode    bool // true = punctuation follows Chinese/English mode
	toolbarVisible     bool // true = toolbar visible
	imeActivated       bool // true = IME is activated (has focus)

	// Input state
	inputBuffer       string
	candidates        []ui.Candidate
	currentPage       int
	totalPages        int
	candidatesPerPage int

	// Caret position (from C++)
	caretX      int
	caretY      int
	caretHeight int

	// Last known valid window position (for fallback)
	lastValidX int
	lastValidY int

	// Punctuation converter with state (for paired punctuation like quotes)
	punctConverter *transform.PunctuationConverter
}

// NewCoordinator creates a new Coordinator
func NewCoordinator(engineMgr *engine.Manager, uiManager *ui.Manager, cfg *config.Config, logger *slog.Logger) *Coordinator {
	candidatesPerPage := 9
	if cfg != nil && cfg.UI.CandidatesPerPage > 0 {
		candidatesPerPage = cfg.UI.CandidatesPerPage
	}

	startInChineseMode := true
	if cfg != nil {
		startInChineseMode = cfg.General.StartInChineseMode
	}

	// Load input settings from config
	fullWidth := false
	chinesePunctuation := true
	punctFollowMode := false
	toolbarVisible := false
	if cfg != nil {
		fullWidth = cfg.Input.FullWidth
		chinesePunctuation = cfg.Input.ChinesePunctuation
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
	}

	// Set up toolbar callbacks
	c.setupToolbarCallbacks()

	return c
}

// setupToolbarCallbacks sets up the callbacks for toolbar button clicks
func (c *Coordinator) setupToolbarCallbacks() {
	if c.uiManager == nil {
		return
	}

	c.uiManager.SetToolbarCallbacks(&ui.ToolbarCallback{
		OnToggleMode: func() {
			c.handleToolbarToggleMode()
		},
		OnToggleWidth: func() {
			c.handleToolbarToggleWidth()
		},
		OnTogglePunct: func() {
			c.handleToolbarTogglePunct()
		},
		OnOpenSettings: func() {
			c.handleToolbarOpenSettings()
		},
		OnPositionChanged: func(x, y int) {
			c.handleToolbarPositionChanged(x, y)
		},
	})
}

// handleToolbarToggleMode handles mode toggle from toolbar click
func (c *Coordinator) handleToolbarToggleMode() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.chineseMode = !c.chineseMode
	c.logger.Debug("Mode toggled via toolbar", "chineseMode", c.chineseMode)

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

	// Update toolbar state
	c.syncToolbarState()
}

// handleToolbarToggleWidth handles width toggle from toolbar click
func (c *Coordinator) handleToolbarToggleWidth() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.fullWidth = !c.fullWidth
	c.logger.Debug("Full-width toggled via toolbar", "fullWidth", c.fullWidth)

	// Update toolbar state
	c.syncToolbarState()

	// Save to config
	c.saveInputConfig()
}

// handleToolbarTogglePunct handles punctuation toggle from toolbar click
func (c *Coordinator) handleToolbarTogglePunct() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.chinesePunctuation = !c.chinesePunctuation
	c.logger.Debug("Chinese punctuation toggled via toolbar", "chinesePunctuation", c.chinesePunctuation)

	// Reset punctuation converter state
	c.punctConverter.Reset()

	// Update toolbar state
	c.syncToolbarState()

	// Save to config
	c.saveInputConfig()
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

// syncToolbarState synchronizes the current state to the toolbar
func (c *Coordinator) syncToolbarState() {
	if c.uiManager == nil {
		return
	}

	c.uiManager.UpdateToolbarState(ui.ToolbarState{
		ChineseMode:  c.chineseMode,
		FullWidth:    c.fullWidth,
		ChinesePunct: c.chinesePunctuation,
		CapsLock:     ui.GetCapsLockState(), // Get actual CapsLock state
	})
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
	c.mu.Lock()
	defer c.mu.Unlock()

	// Use Debug for high-frequency key events to reduce log noise
	c.logger.Debug("HandleKeyEvent", "key", data.Key, "keycode", data.KeyCode, "modifiers", data.Modifiers, "chineseMode", c.chineseMode)

	// Check for Ctrl or Alt modifiers
	hasCtrl := data.Modifiers&ModCtrl != 0
	hasAlt := data.Modifiers&ModAlt != 0
	hasShift := data.Modifiers&ModShift != 0

	// Handle Ctrl+` for engine switch (VK_OEM_3 = 192)
	if hasCtrl && data.KeyCode == 192 {
		return c.handleEngineSwitchKey()
	}

	// Handle Shift+Space for full-width toggle
	if hasShift && data.KeyCode == 32 { // VK_SPACE = 32
		toggleKey := ""
		if c.config != nil {
			toggleKey = c.config.Hotkeys.ToggleFullWidth
		}
		if toggleKey == "shift+space" {
			return c.handleToggleFullWidth()
		}
	}

	// Handle Ctrl+. for punctuation toggle (VK_OEM_PERIOD = 190)
	if hasCtrl && data.KeyCode == 190 {
		toggleKey := ""
		if c.config != nil {
			toggleKey = c.config.Hotkeys.TogglePunct
		}
		if toggleKey == "ctrl+." {
			return c.handleTogglePunct()
		}
	}

	// Other Ctrl/Alt combinations should be passed to the system
	if hasCtrl || hasAlt {
		c.logger.Debug("Key has Ctrl/Alt modifier, passing to system")
		return nil
	}

	// Preserve original key for English mode (uppercase letters should stay uppercase)
	key := data.Key

	// Handle Shift key for mode toggle
	if data.KeyCode == 16 { // VK_SHIFT
		c.chineseMode = !c.chineseMode
		c.logger.Debug("Mode toggled by Shift", "chineseMode", c.chineseMode)

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

		// Show mode indicator
		c.showModeIndicator()

		// Update toolbar state
		if c.uiManager != nil && c.toolbarVisible && c.imeActivated {
			c.uiManager.UpdateToolbarState(ui.ToolbarState{
				ChineseMode:  c.chineseMode,
				FullWidth:    c.fullWidth,
				ChinesePunct: c.chinesePunctuation,
				CapsLock:     ui.GetCapsLockState(),
			})
		}

		// Return mode_changed so C++ can update language bar icon
		return &bridge.KeyEventResult{
			Type:        bridge.ResponseTypeModeChanged,
			ChineseMode: c.chineseMode,
		}
	}

	// English mode: pass through all keys
	if !c.chineseMode {
		// In English mode, letters should be passed through directly (both upper and lower case)
		if len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')) {
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: key,
			}
		}
		return nil // Let other keys pass through
	}

	// Chinese mode with CapsLock: output uppercase letters directly
	// This allows users to quickly type uppercase English while in Chinese mode
	if ui.GetCapsLockState() {
		if len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')) {
			// If there's pending input, commit it first then output the uppercase letter
			if len(c.inputBuffer) > 0 && len(c.candidates) > 0 {
				// Commit first candidate
				candidate := c.candidates[0]
				text := candidate.Text
				if c.fullWidth {
					text = transform.ToFullWidth(text)
				}
				c.clearState()
				c.hideUI()

				// Convert letter to uppercase and optionally apply full-width
				upperKey := strings.ToUpper(key)
				if c.fullWidth {
					upperKey = transform.ToFullWidth(upperKey)
				}

				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: text + upperKey,
				}
			}

			// No pending input, just output uppercase letter
			c.clearState()
			c.hideUI()

			upperKey := strings.ToUpper(key)
			if c.fullWidth {
				upperKey = transform.ToFullWidth(upperKey)
			}

			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: upperKey,
			}
		}
	}

	// Chinese mode handling
	switch {
	case data.KeyCode == 8: // Backspace
		return c.handleBackspace()

	case data.KeyCode == 13: // Enter
		return c.handleEnter()

	case data.KeyCode == 27: // Escape
		return c.handleEscape()

	case data.KeyCode == 32: // Space
		return c.handleSpace()

	case key == "page_up" || data.KeyCode == 189: // - key (VK_OEM_MINUS = 0xBD = 189)
		return c.handlePageUp()

	case key == "page_down" || data.KeyCode == 187: // = key (VK_OEM_PLUS = 0xBB = 187)
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
	c.inputBuffer += key
	c.logger.Debug("Input buffer updated", "buffer", c.inputBuffer)

	// 处理顶码（如五笔的五码顶字）
	if c.engineMgr != nil {
		commitText, newInput, shouldCommit := c.engineMgr.HandleTopCode(c.inputBuffer)
		if shouldCommit {
			c.inputBuffer = newInput
			c.logger.Debug("Top code commit", "text", commitText, "newInput", newInput)

			// Apply full-width conversion if enabled
			if c.fullWidth {
				commitText = transform.ToFullWidth(commitText)
			}

			// 如果还有剩余输入，继续处理
			if len(c.inputBuffer) > 0 {
				c.updateCandidates()
				c.showUI()
			} else {
				c.hideUI()
			}

			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: commitText,
			}
		}
	}

	// 更新候选词
	result := c.updateCandidatesEx()

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

	c.showUI()
	return nil // Just show candidates, don't insert anything yet
}

func (c *Coordinator) handleBackspace() *bridge.KeyEventResult {
	if len(c.inputBuffer) > 0 {
		c.inputBuffer = c.inputBuffer[:len(c.inputBuffer)-1]
		c.logger.Debug("Input buffer after backspace", "buffer", c.inputBuffer)

		if len(c.inputBuffer) == 0 {
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}

		c.updateCandidates()
		c.showUI()
	} else {
		// Buffer is already empty - this shouldn't happen normally
		// Return ClearComposition to reset C++ side's _isComposing state
		c.logger.Debug("Backspace with empty buffer, clearing composition state")
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
	}
	return nil
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
	// Only handle if we have candidates and are not on the first page
	if len(c.candidates) == 0 || c.currentPage <= 1 {
		return nil
	}

	c.currentPage--
	c.logger.Debug("Page up", "currentPage", c.currentPage, "totalPages", c.totalPages)
	c.showUI()
	return nil
}

func (c *Coordinator) handlePageDown() *bridge.KeyEventResult {
	// Only handle if we have candidates and are not on the last page
	if len(c.candidates) == 0 || c.currentPage >= c.totalPages {
		return nil
	}

	c.currentPage++
	c.logger.Debug("Page down", "currentPage", c.currentPage, "totalPages", c.totalPages)
	c.showUI()
	return nil
}

func (c *Coordinator) selectCandidate(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return nil
	}

	candidate := c.candidates[index]
	c.logger.Debug("Candidate selected", "index", index, "text", candidate.Text)

	text := candidate.Text

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

	// Convert to UI candidates
	c.candidates = make([]ui.Candidate, len(result.Candidates))
	for i, ec := range result.Candidates {
		cand := ui.Candidate{
			Text:   ec.Text,
			Index:  i + 1,
			Weight: ec.Weight,
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

	// Calculate window position (below caret)
	windowX := c.caretX
	windowY := c.caretY + c.caretHeight + 5

	// Multi-monitor support: coordinates can be negative (monitors to the left/above primary)
	// Only use fallback if we haven't received valid caret info yet (both X and Y are 0)
	// or if coordinates are extremely large (likely garbage values)
	const maxCoord = 32000 // Windows virtual screen limit is typically around 32767
	if (c.caretX == 0 && c.caretY == 0) || windowX > maxCoord || windowX < -maxCoord || windowY > maxCoord || windowY < -maxCoord {
		// Use last known good position or a reasonable default
		if c.lastValidX != 0 || c.lastValidY != 0 {
			windowX = c.lastValidX
			windowY = c.lastValidY
		} else {
			// Fallback to a safe position on primary monitor
			windowX = 400
			windowY = 300
		}
		c.logger.Debug("Using fallback position", "x", windowX, "y", windowY, "caretX", c.caretX, "caretY", c.caretY)
	} else {
		// Save valid position for future fallback
		c.lastValidX = windowX
		c.lastValidY = windowY
	}

	c.uiManager.ShowCandidates(
		displayCandidates,
		c.inputBuffer,
		windowX,
		windowY,
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

	c.uiManager.ShowModeIndicator(modeText, x, y)
}

func (c *Coordinator) hideUI() {
	if c.uiManager != nil {
		c.uiManager.Hide()
	}
}

func (c *Coordinator) clearState() {
	c.inputBuffer = ""
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

	c.logger.Debug("Caret position updated", "x", c.caretX, "y", c.caretY, "height", c.caretHeight)
	return nil
}

// HandleFocusLost handles focus lost events
func (c *Coordinator) HandleFocusLost() {
	c.logger.Debug("Focus lost, clearing state")

	// Set IME as deactivated (this will hide toolbar)
	c.SetIMEActivated(false)

	c.mu.Lock()
	defer c.mu.Unlock()
	c.clearState()
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

// HandleFocusGained handles focus gained events and returns current status
func (c *Coordinator) HandleFocusGained() *bridge.StatusUpdateData {
	c.logger.Debug("Focus gained")

	// Set IME as activated (this will show toolbar if enabled)
	c.SetIMEActivated(true)

	// Return current status so TSF can sync state
	c.mu.Lock()
	defer c.mu.Unlock()
	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           ui.GetCapsLockState(),
	}
}

// HandleToggleMode toggles the input mode and returns the new state
func (c *Coordinator) HandleToggleMode() bool {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.chineseMode = !c.chineseMode
	c.logger.Debug("Mode toggled via IPC", "chineseMode", c.chineseMode)

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

	// Update toolbar state to sync Chinese/English mode
	if c.uiManager != nil && c.toolbarVisible && c.imeActivated {
		c.uiManager.UpdateToolbarState(ui.ToolbarState{
			ChineseMode:  c.chineseMode,
			FullWidth:    c.fullWidth,
			ChinesePunct: c.chinesePunctuation,
			CapsLock:     ui.GetCapsLockState(),
		})
	}

	return c.chineseMode
}

// HandleCapsLockState shows Caps Lock indicator (A/a) and updates toolbar
func (c *Coordinator) HandleCapsLockState(on bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

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

	// Update toolbar state to sync CapsLock indicator
	if c.toolbarVisible && c.imeActivated {
		c.uiManager.UpdateToolbarState(ui.ToolbarState{
			ChineseMode:  c.chineseMode,
			FullWidth:    c.fullWidth,
			ChinesePunct: c.chinesePunctuation,
			CapsLock:     on,
		})
	}
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
		c.uiManager.UpdateConfig(uiConfig.FontSize, uiConfig.FontPath)
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
				ChineseMode:  c.chineseMode,
				FullWidth:    c.fullWidth,
				ChinesePunct: c.chinesePunctuation,
				CapsLock:     ui.GetCapsLockState(),
			})
		} else {
			c.uiManager.SetToolbarVisible(false)
		}
	}

	c.logger.Debug("Toolbar config updated", "visible", c.toolbarVisible)
}

// UpdateInputConfig 更新输入配置（热更新）
func (c *Coordinator) UpdateInputConfig(inputConfig *config.InputConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if inputConfig == nil {
		return
	}

	c.fullWidth = inputConfig.FullWidth
	c.chinesePunctuation = inputConfig.ChinesePunctuation
	c.punctFollowMode = inputConfig.PunctFollowMode

	// 更新配置引用
	if c.config != nil {
		c.config.Input = *inputConfig
	}

	// Reset punctuation converter state when config changes
	c.punctConverter.Reset()

	// 更新工具栏状态
	if c.uiManager != nil && c.toolbarVisible {
		c.uiManager.UpdateToolbarState(ui.ToolbarState{
			ChineseMode:   c.chineseMode,
			FullWidth:     c.fullWidth,
			ChinesePunct:  c.chinesePunctuation,
			CapsLock:      ui.GetCapsLockState(),
		})
	}

	c.logger.Debug("Input config updated", "fullWidth", c.fullWidth, "chinesePunctuation", c.chinesePunctuation, "punctFollowMode", c.punctFollowMode)
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
			if c.caretX > 0 || c.caretY > 0 {
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
				ChineseMode:  c.chineseMode,
				FullWidth:    c.fullWidth,
				ChinesePunct: c.chinesePunctuation,
				CapsLock:     ui.GetCapsLockState(),
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

		// Show mode indicator
		c.showModeIndicator()

	case "toggle_width":
		c.fullWidth = !c.fullWidth
		c.logger.Debug("Full-width toggled via menu", "fullWidth", c.fullWidth)

		// Show indicator
		indicator := "半"
		if c.fullWidth {
			indicator = "全"
		}
		c.showIndicator(indicator)

		// Save to config
		c.saveInputConfig()

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

		// Save to config
		c.saveInputConfig()

	case "toggle_toolbar":
		c.toolbarVisible = !c.toolbarVisible
		c.logger.Debug("Toolbar visibility toggled via menu", "toolbarVisible", c.toolbarVisible)

		// Update UI
		if c.uiManager != nil {
			if c.toolbarVisible && c.imeActivated {
				// Calculate position based on current caret
				toolbarWidth, toolbarHeight := 140, 30
				var posX, posY int
				if c.caretX > 0 || c.caretY > 0 {
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
					ChineseMode:  c.chineseMode,
					FullWidth:    c.fullWidth,
					ChinesePunct: c.chinesePunctuation,
					CapsLock:     ui.GetCapsLockState(),
				})
			} else {
				c.uiManager.SetToolbarVisible(false)
			}
		}

		// Save to config
		c.saveToolbarConfig()

	case "open_settings":
		c.logger.Info("Opening settings requested")
		// Open settings window (will be implemented in UI)
		if c.uiManager != nil {
			c.uiManager.OpenSettings()
		}
	}

	// Return current status
	return c.getStatusUpdate()
}

// getStatusUpdate returns the current status
func (c *Coordinator) getStatusUpdate() *bridge.StatusUpdateData {
	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           ui.GetCapsLockState(),
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

// saveInputConfig saves the input configuration to file
func (c *Coordinator) saveInputConfig() {
	go func() {
		cfg, err := config.Load()
		if err != nil {
			cfg = config.DefaultConfig()
		}

		cfg.Input.FullWidth = c.fullWidth
		cfg.Input.ChinesePunctuation = c.chinesePunctuation

		if err := config.Save(cfg); err != nil {
			c.logger.Error("Failed to save input config", "error", err)
		} else {
			c.logger.Debug("Input config saved")
		}
	}()
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
	switch c.config.Input.SelectKey2 {
	case "semicolon":
		return key == ";" || keyCode == 186 // VK_OEM_1
	case "comma":
		return key == "," || keyCode == 188 // VK_OEM_COMMA
	case "lshift":
		return keyCode == 160 // VK_LSHIFT
	case "lctrl":
		return keyCode == 162 // VK_LCONTROL
	case "none", "":
		return false
	}
	return false
}

// isSelectKey3 checks if the key is configured as the 3rd candidate selection key
func (c *Coordinator) isSelectKey3(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}
	switch c.config.Input.SelectKey3 {
	case "quote":
		return key == "'" || keyCode == 222 // VK_OEM_7
	case "period":
		return key == "." || keyCode == 190 // VK_OEM_PERIOD
	case "rshift":
		return keyCode == 161 // VK_RSHIFT
	case "rctrl":
		return keyCode == 163 // VK_RCONTROL
	case "none", "":
		return false
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

	// Save to config
	c.saveInputConfig()

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

	// Save to config
	c.saveInputConfig()

	// Consume the key (don't let it pass through)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}
