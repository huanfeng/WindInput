// Package coordinator orchestrates communication between C++ Bridge, Engine, and UI
package coordinator

import (
	"log/slog"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/config"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/hotkey"
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
	caretValid  bool // true if we have received valid caret position (coordinates can be negative in multi-monitor)

	// Last known valid window position (for fallback)
	lastValidX int
	lastValidY int

	// Punctuation converter with state (for paired punctuation like quotes)
	punctConverter *transform.PunctuationConverter

	// Hotkey compiler for binary protocol
	hotkeyCompiler *hotkey.Compiler
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

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

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

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()
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

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()
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

	// Other Ctrl/Alt combinations should be passed to the system
	if hasCtrl || hasAlt {
		c.logger.Debug("Key has Ctrl/Alt modifier, passing to system")
		return nil
	}

	// Preserve original key for English mode (uppercase letters should stay uppercase)
	key := data.Key

	// Handle mode toggle keys (lshift, rshift, lctrl, rctrl, capslock)
	if toggleKey := c.getToggleModeKey(data.KeyCode); toggleKey != "" {
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

			// Update toolbar state
			if c.uiManager != nil && c.toolbarVisible && c.imeActivated {
				c.uiManager.UpdateToolbarState(ui.ToolbarState{
					ChineseMode:  c.chineseMode,
					FullWidth:    c.fullWidth,
					ChinesePunct: c.chinesePunctuation,
					CapsLock:     ui.GetCapsLockState(),
				})
			}

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

	case c.isPageUpKey(key, data.KeyCode):
		return c.handlePageUp()

	case c.isPageDownKey(key, data.KeyCode):
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

	// Handle Inline Preedit
	if c.config != nil && c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.inputBuffer,
			CaretPos: len(c.inputBuffer),
		}
	}

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

		// Handle Inline Preedit
		if c.config != nil && c.config.UI.InlinePreedit {
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.inputBuffer,
				CaretPos: len(c.inputBuffer),
			}
		}
	} else {
		// Buffer is already empty - pass through to system
		// This allows backspace to work normally when not composing
		c.logger.Debug("Backspace with empty buffer, passing through to system")
		return nil
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
		c.inputBuffer,
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
	c.caretValid = true // Mark that we have received valid caret position

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
func (c *Coordinator) getCompiledHotkeys() (keyDownHotkeys, keyUpHotkeys []uint32) {
	if c.hotkeyCompiler == nil {
		return nil, nil
	}
	keyDownHotkeys, keyUpHotkeys = c.hotkeyCompiler.Compile()
	c.logger.Debug("Compiled hotkeys for C++",
		"keyDownCount", len(keyDownHotkeys),
		"keyUpCount", len(keyUpHotkeys))
	return
}

// HandleFocusGained handles focus gained events and returns current status
func (c *Coordinator) HandleFocusGained() *bridge.StatusUpdateData {
	c.logger.Debug("Focus gained")

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
	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           ui.GetCapsLockState(),
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
	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           ui.GetCapsLockState(),
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
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
			c.logger.Debug("CommitOnSwitch: committing input code", "text", commitText)
		}
	}

	c.chineseMode = !c.chineseMode
	c.logger.Debug("Mode toggled via IPC", "chineseMode", c.chineseMode, "commitText", commitText)

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

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeStateNoLock()

	return commitText, c.chineseMode
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

		// Save runtime state
		c.saveRuntimeState()

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

		// Save runtime state
		c.saveRuntimeState()

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

		// Save runtime state
		c.saveRuntimeState()

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

	// Save to config
	c.saveInputConfig()

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

	// Save to config
	c.saveInputConfig()

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
func (c *Coordinator) isPageUpKey(key string, keyCode int) bool {
	if c.config == nil {
		// 默认支持 PageUp 和 - 键
		return key == "page_up" || keyCode == 33 || keyCode == 189
	}

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
			// Shift+Tab 需要在 HandleKeyEvent 中单独处理
			// 这里不处理，因为需要检测 Shift 修饰键
		}
	}
	return false
}

// isPageDownKey checks if the key is configured as a page down key
func (c *Coordinator) isPageDownKey(key string, keyCode int) bool {
	if c.config == nil {
		// 默认支持 PageDown 和 = 键
		return key == "page_down" || keyCode == 34 || keyCode == 187
	}

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
			if keyCode == 9 { // VK_TAB (Tab without Shift = page down)
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
