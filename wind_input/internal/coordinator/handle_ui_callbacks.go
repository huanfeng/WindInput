// handle_ui_callbacks.go — 工具栏与候选窗口 UI 回调
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
)

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
		OnShowMenu: func(screenX, screenY, flipRefY int) {
			go c.handleShowUnifiedMenu(screenX, screenY, flipRefY)
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
		OnPageUp: func() {
			// Run in goroutine to avoid blocking UI thread
			go func() {
				c.mu.Lock()
				defer c.mu.Unlock()
				c.handlePageUp()
			}()
		},
		OnPageDown: func() {
			// Run in goroutine to avoid blocking UI thread
			go func() {
				c.mu.Lock()
				defer c.mu.Unlock()
				c.handlePageDown()
			}()
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
		OnResetDefault: func(index int) {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateResetDefault(index)
		},
		OnOpenSettings: func() {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateOpenSettings()
		},
		OnAbout: func() {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateAbout()
		},
		OnShowUnifiedMenu: func(screenX, screenY int) {
			go c.handleShowUnifiedMenu(screenX, screenY, 0)
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
	originalText := candidate.Text
	text := originalText

	// Apply full-width conversion if enabled
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 收集学习所需信息（在 clearState 前）
	code := c.inputBuffer
	isCommand := candidate.IsCommand
	candidateSource := candidate.Source
	engineMgr := c.engineMgr

	c.logger.Debug("Candidate selected via mouse click", "index", actualIndex)

	// Clear state and hide UI
	c.clearState()
	c.hideUI()

	// Get bridge server reference (release lock before network I/O)
	bridgeServer := c.bridgeServer
	c.mu.Unlock()

	// 触发选词学习回调
	if engineMgr != nil && !isCommand && originalText != "" {
		engineMgr.OnCandidateSelected(code, originalText, candidateSource)
	}

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

// handleCandidateMoveUp handles move up action from context menu.
// 五笔：所有可见候选（含前缀匹配）都可前移，规则按当前 inputBuffer 存储。
// 拼音：不支持前移。
func (c *Coordinator) handleCandidateMoveUp(index int) {
	c.mu.Lock()

	c.logger.Debug("Candidate move up requested", "index", index)

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	if actualIndex <= 0 || actualIndex >= len(c.candidates) {
		c.mu.Unlock()
		return
	}

	// 单候选或命令候选不可操作
	if len(c.candidates) <= 1 {
		c.mu.Unlock()
		return
	}
	cand := c.candidates[actualIndex]
	if cand.IsCommand {
		c.mu.Unlock()
		return
	}

	// 拼音引擎不支持前移/后移
	if c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin {
		c.mu.Unlock()
		return
	}

	// 前移 = pin(当前位置 - 1)
	code := c.inputBuffer
	targetPosition := actualIndex - 1
	c.mu.Unlock()

	if c.engineMgr != nil {
		shadowLayer := c.engineMgr.GetDictManager().GetShadowLayer()
		if shadowLayer != nil {
			shadowLayer.Pin(code, cand.Text, targetPosition)
			if err := shadowLayer.Save(); err != nil {
				c.logger.Error("Failed to save shadow layer", "error", err)
			}
		}
	}

	c.mu.Lock()
	c.updateCandidates()
	c.showUI()
	c.mu.Unlock()
}

// handleCandidateMoveDown handles move down action from context menu.
// 五笔：所有可见候选都可后移。拼音：不支持。
func (c *Coordinator) handleCandidateMoveDown(index int) {
	c.mu.Lock()

	c.logger.Debug("Candidate move down requested", "index", index)

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	if actualIndex < 0 || actualIndex >= len(c.candidates)-1 {
		c.mu.Unlock()
		return
	}

	if len(c.candidates) <= 1 {
		c.mu.Unlock()
		return
	}
	cand := c.candidates[actualIndex]
	if cand.IsCommand {
		c.mu.Unlock()
		return
	}

	if c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin {
		c.mu.Unlock()
		return
	}

	// 后移 = pin(当前位置 + 1)
	code := c.inputBuffer
	targetPosition := actualIndex + 1
	c.mu.Unlock()

	if c.engineMgr != nil {
		shadowLayer := c.engineMgr.GetDictManager().GetShadowLayer()
		if shadowLayer != nil {
			shadowLayer.Pin(code, cand.Text, targetPosition)
			if err := shadowLayer.Save(); err != nil {
				c.logger.Error("Failed to save shadow layer", "error", err)
			}
		}
	}

	c.mu.Lock()
	c.updateCandidates()
	c.showUI()
	c.mu.Unlock()
}

// handleCandidateMoveTop handles move to top action from context menu
func (c *Coordinator) handleCandidateMoveTop(index int) {
	c.mu.Lock()

	c.logger.Debug("Candidate move to top requested", "index", index)

	// Convert page-local index to actual candidate index
	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index

	if actualIndex <= 0 || actualIndex >= len(c.candidates) {
		c.logger.Warn("Invalid candidate index for move to top", "actualIndex", actualIndex)
		c.mu.Unlock()
		return
	}

	candidate := c.candidates[actualIndex]
	if candidate.IsCommand {
		c.logger.Debug("Cannot move command candidate to top", "text", candidate.Text)
		c.mu.Unlock()
		return
	}

	// 统一用 inputBuffer 作为 code（规则只在当前输入编码下生效）
	code := c.inputBuffer

	c.mu.Unlock()

	if c.engineMgr != nil {
		shadowLayer := c.engineMgr.GetDictManager().GetShadowLayer()
		if shadowLayer != nil {
			shadowLayer.Pin(code, candidate.Text, 0)
			if err := shadowLayer.Save(); err != nil {
				c.logger.Error("Failed to save shadow layer", "error", err)
			}
		}
	}

	c.mu.Lock()
	c.updateCandidates()
	c.showUI()
	c.mu.Unlock()
}

// handleCandidateDelete handles delete action from context menu.
// 单字不允许删除（防止某个字永远打不出来）。
// 所有可见的多字词候选都可删除，规则按当前 inputBuffer 存储。
func (c *Coordinator) handleCandidateDelete(index int) {
	c.mu.Lock()

	c.logger.Debug("Candidate delete requested", "index", index)

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		c.mu.Unlock()
		return
	}

	cand := c.candidates[actualIndex]
	if cand.IsCommand {
		c.mu.Unlock()
		return
	}

	// 单字不允许删除
	if len([]rune(cand.Text)) <= 1 {
		c.logger.Debug("Cannot delete single character", "text", cand.Text)
		c.mu.Unlock()
		return
	}

	// 统一用 inputBuffer 作为 code
	code := c.inputBuffer

	c.mu.Unlock()

	if c.engineMgr != nil {
		shadowLayer := c.engineMgr.GetDictManager().GetShadowLayer()
		if shadowLayer != nil {
			shadowLayer.Delete(code, cand.Text)
			if err := shadowLayer.Save(); err != nil {
				c.logger.Error("Failed to save shadow layer", "error", err)
			}
		}
	}

	c.mu.Lock()
	c.updateCandidates()
	c.showUI()
	c.mu.Unlock()
}

// handleCandidateResetDefault handles reset to default action from context menu
// Removes all shadow rules for the candidate, restoring its original dictionary state
func (c *Coordinator) handleCandidateResetDefault(index int) {
	c.mu.Lock()

	c.logger.Debug("Candidate reset default requested", "index", index)

	// Convert page-local index to actual candidate index
	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index

	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		c.logger.Warn("Invalid candidate index for reset default", "actualIndex", actualIndex)
		c.mu.Unlock()
		return
	}

	candidate := c.candidates[actualIndex]
	if candidate.IsCommand {
		c.logger.Debug("Cannot reset command candidate", "text", candidate.Text)
		c.mu.Unlock()
		return
	}

	// 统一用 inputBuffer 作为 code
	code := c.inputBuffer

	c.mu.Unlock()

	// Remove shadow rule without lock
	if c.engineMgr != nil {
		shadowLayer := c.engineMgr.GetDictManager().GetShadowLayer()
		if shadowLayer != nil {
			shadowLayer.RemoveRule(code, candidate.Text)
			if err := shadowLayer.Save(); err != nil {
				c.logger.Error("Failed to save shadow layer", "error", err)
			}
		}
	}

	// Re-acquire lock to refresh UI
	c.mu.Lock()
	c.updateCandidates()
	c.showUI()
	c.mu.Unlock()
}

// handleCandidateOpenSettings handles open settings action from context menu
func (c *Coordinator) handleCandidateOpenSettings() {
	c.logger.Info("Opening settings from candidate context menu")
	if c.uiManager != nil {
		c.uiManager.OpenSettings()
	}
}

// handleCandidateAbout handles about action from context menu
func (c *Coordinator) handleCandidateAbout() {
	c.logger.Info("Opening about page from candidate context menu")
	if c.uiManager != nil {
		c.uiManager.OpenSettingsWithPage("about")
	}
}

// handleToolbarToggleMode handles mode toggle from toolbar click
func (c *Coordinator) handleToolbarToggleMode() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.chineseMode = !c.chineseMode
	c.logger.Info("Mode toggled via toolbar", "chineseMode", c.chineseMode)

	// Clear any pending input when switching modes
	hadInput := len(c.inputBuffer) > 0
	if hadInput {
		c.clearState()
		c.hideUI()
	}

	// Notify TSF side to clear inline composition if there was active input
	if hadInput && c.bridgeServer != nil {
		go c.bridgeServer.PushClearCompositionToActiveClient()
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
// Toolbar position is temporary and not persisted to config.
// On IME reload, toolbar will return to its default calculated position.
func (c *Coordinator) handleToolbarPositionChanged(x, y int) {
	c.logger.Debug("Toolbar position changed (temporary)", "x", x, "y", y)
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

// handleShowUnifiedMenu shows the unified context menu at the given screen position
func (c *Coordinator) handleShowUnifiedMenu(screenX, screenY, flipRefY int) {
	if c.uiManager == nil {
		return
	}

	// Build theme menu items from theme info
	themeInfos := c.uiManager.GetAvailableThemeInfos()
	themeMenuItems := make([]ui.ThemeMenuItem, len(themeInfos))
	for i, info := range themeInfos {
		themeMenuItems[i] = ui.ThemeMenuItem{ID: info.ID, DisplayName: info.DisplayName}
	}

	c.mu.Lock()
	state := ui.UnifiedMenuState{
		ChineseMode:    c.chineseMode,
		FullWidth:      c.fullWidth,
		ChinesePunct:   c.chinesePunctuation,
		ToolbarVisible: c.toolbarVisible,
		Themes:         themeMenuItems,
		CurrentThemeID: c.uiManager.GetCurrentThemeID(),
	}
	c.mu.Unlock()

	c.uiManager.ShowUnifiedMenu(screenX, screenY, flipRefY, state, func(id int) {
		go c.handleUnifiedMenuAction(id)
	})
}

// handleUnifiedMenuAction handles a menu item selection from the unified menu
func (c *Coordinator) handleUnifiedMenuAction(id int) {
	switch {
	case id == ui.UnifiedMenuToggleMode:
		c.handleToolbarToggleMode()
	case id == ui.UnifiedMenuToggleWidth:
		c.handleToolbarToggleWidth()
	case id == ui.UnifiedMenuTogglePunct:
		c.handleToolbarTogglePunct()
	case id == ui.UnifiedMenuToggleToolbar:
		c.HandleMenuCommand("toggle_toolbar")
	case id == ui.UnifiedMenuReloadConfig:
		c.logger.Info("Reload config from unified menu")
		c.HandleMenuCommand("reload_config")
	case id == ui.UnifiedMenuRestartService:
		c.logger.Info("Restart service requested from unified menu")
		c.resetAndResync()
	case id == ui.UnifiedMenuDictionary:
		if c.uiManager != nil {
			c.uiManager.OpenSettingsWithPage("dictionary")
		}
	case id == ui.UnifiedMenuSettings:
		c.handleToolbarOpenSettings()
	case id == ui.UnifiedMenuAbout:
		if c.uiManager != nil {
			c.uiManager.OpenSettingsWithPage("about")
		}
	case id >= ui.UnifiedMenuThemeBase && id < ui.UnifiedMenuThemeBase+100:
		// Theme selection
		themeIndex := id - ui.UnifiedMenuThemeBase
		themeInfos := c.uiManager.GetAvailableThemeInfos()
		if themeIndex >= 0 && themeIndex < len(themeInfos) {
			themeID := themeInfos[themeIndex].ID
			c.logger.Info("Theme selected from unified menu", "theme", themeID)
			c.uiManager.LoadTheme(themeID)
			// Save to config
			c.saveThemeConfig(themeID)
		}
	}
}

// HandleShowContextMenu handles context menu request from TSF (bridge interface)
func (c *Coordinator) HandleShowContextMenu(screenX, screenY int) {
	c.handleShowUnifiedMenu(screenX, screenY, 0)
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
