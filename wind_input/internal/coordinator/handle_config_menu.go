// handle_config_menu.go — 菜单命令处理与 IME 激活状态
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
)

// inputStateSnapshot 保存重载配置前的输入状态快照
type inputStateSnapshot struct {
	inputBuffer           string
	inputCursorPos        int
	preeditDisplay        string
	syllableBoundaries    []int
	candidates            []ui.Candidate
	currentPage           int
	totalPages            int
	selectedIndex         int
	tempEnglishMode       bool
	tempEnglishBuffer     string
	tempPinyinMode        bool
	tempPinyinBuffer      string
	compositionStartX     int
	compositionStartY     int
	compositionStartValid bool
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
		// IME activated - register global hotkeys for combination keys
		c.uiManager.RegisterGlobalHotkeys(c.buildGlobalHotkeyEntries())

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
			effMode := c.getEffectiveModeNoLock()
			c.uiManager.ShowToolbarWithState(posX, posY, ui.ToolbarState{
				ChineseMode:   effMode == ModeChinese,
				FullWidth:     c.fullWidth,
				ChinesePunct:  c.chinesePunctuation && effMode == ModeChinese,
				CapsLock:      c.capsLockOn,
				EffectiveMode: int(effMode),
			})
		}
	} else {
		// IME deactivated - unregister global hotkeys
		c.uiManager.UnregisterGlobalHotkeys()

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
		if c.uiManager != nil {
			c.uiManager.OpenSettingsWithPage("dictionary")
		}

	case "add_word":
		c.logger.Info("Quick add word requested from menu")
		c.enterAddWordMode()

	case "show_about":
		c.logger.Info("Showing about dialog requested")
		if c.uiManager != nil {
			c.uiManager.OpenSettingsWithPage("about")
		}

	case "reload_config":
		c.logger.Info("Reload config requested from menu")
		// 保存完整输入状态（reload 过程中可能被 CompositionTerminated/FocusLost 事件清空）
		hadActiveInput := len(c.inputBuffer) > 0 || c.tempPinyinMode || c.tempEnglishMode
		snapshot := inputStateSnapshot{
			inputBuffer:           c.inputBuffer,
			inputCursorPos:        c.inputCursorPos,
			preeditDisplay:        c.preeditDisplay,
			syllableBoundaries:    c.syllableBoundaries,
			candidates:            c.candidates,
			currentPage:           c.currentPage,
			totalPages:            c.totalPages,
			selectedIndex:         c.selectedIndex,
			tempEnglishMode:       c.tempEnglishMode,
			tempEnglishBuffer:     c.tempEnglishBuffer,
			tempPinyinMode:        c.tempPinyinMode,
			tempPinyinBuffer:      c.tempPinyinBuffer,
			compositionStartX:     c.compositionStartX,
			compositionStartY:     c.compositionStartY,
			compositionStartValid: c.compositionStartValid,
		}
		go func() {
			newCfg, err := config.Load()
			if err != nil {
				c.logger.Error("Failed to load config for reload", "error", err)
				return
			}
			c.UpdateHotkeyConfig(&newCfg.Hotkeys)
			c.UpdateStartupConfig(&newCfg.Startup)
			c.UpdateUIConfig(&newCfg.UI)
			c.UpdateToolbarConfig(&newCfg.Toolbar)
			c.UpdateInputConfig(&newCfg.Input)
			c.logger.Info("Config reloaded successfully from menu")

			// 刷新候选窗口
			if hadActiveInput {
				c.mu.Lock()
				stateCleared := len(c.inputBuffer) == 0 && !c.tempPinyinMode && !c.tempEnglishMode
				if stateCleared {
					// 输入状态被外部事件清空，恢复快照
					c.inputBuffer = snapshot.inputBuffer
					c.inputCursorPos = snapshot.inputCursorPos
					c.preeditDisplay = snapshot.preeditDisplay
					c.syllableBoundaries = snapshot.syllableBoundaries
					c.candidates = snapshot.candidates
					c.currentPage = snapshot.currentPage
					c.totalPages = snapshot.totalPages
					c.selectedIndex = snapshot.selectedIndex
					c.tempEnglishMode = snapshot.tempEnglishMode
					c.tempEnglishBuffer = snapshot.tempEnglishBuffer
					c.tempPinyinMode = snapshot.tempPinyinMode
					c.tempPinyinBuffer = snapshot.tempPinyinBuffer
				}
				// 始终恢复组合起始位置（确保候选窗口位置不偏移）
				c.compositionStartX = snapshot.compositionStartX
				c.compositionStartY = snapshot.compositionStartY
				c.compositionStartValid = snapshot.compositionStartValid
				if len(c.candidates) > 0 {
					c.showUI()
				}
				c.mu.Unlock()
			}
		}()

	case "exit":
		c.logger.Info("Exit requested from menu")
		RequestExit()
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
		IconLabel:          c.getIconLabelNoLock(),
	}
}

// buildGlobalHotkeyEntries builds the list of global hotkey entries from config.
// Caller must hold c.mu.
func (c *Coordinator) buildGlobalHotkeyEntries() []ui.GlobalHotkeyEntry {
	if c.config == nil {
		return nil
	}
	var entries []ui.GlobalHotkeyEntry
	id := 1
	if entry, ok := ui.ParseHotkeyString(c.config.Hotkeys.SwitchEngine, id, "switch_engine"); ok {
		entries = append(entries, entry)
		id++
	}
	if entry, ok := ui.ParseHotkeyString(c.config.Hotkeys.ToggleFullWidth, id, "toggle_full_width"); ok {
		entries = append(entries, entry)
		id++
	}
	if entry, ok := ui.ParseHotkeyString(c.config.Hotkeys.TogglePunct, id, "toggle_punct"); ok {
		entries = append(entries, entry)
		id++
	}
	if entry, ok := ui.ParseHotkeyString(c.config.Hotkeys.ToggleToolbar, id, "toggle_toolbar"); ok {
		entries = append(entries, entry)
		id++
	}
	if entry, ok := ui.ParseHotkeyString(c.config.Hotkeys.OpenSettings, id, "open_settings"); ok {
		entries = append(entries, entry)
	}
	return entries
}

// showIndicator shows a brief indicator text
func (c *Coordinator) showIndicator(text string) {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	x, y := c.getIndicatorPosition()
	c.uiManager.ShowModeIndicator(text, x, y)
}
