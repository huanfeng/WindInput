// handle_ui_callbacks.go — 工具栏与候选窗口 UI 回调
package coordinator

import (
	"context"
	"image"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
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

// setupStatusWindowCallbacks 设置状态窗口右键菜单回调
func (c *Coordinator) setupStatusWindowCallbacks() {
	if c.uiManager == nil {
		return
	}
	if sw := c.uiManager.GetStatusWindow(); sw != nil {
		sw.SetMenuCallback(func(action ui.StatusMenuAction) {
			go c.handleStatusMenuAction(action)
		})
	}
}

// setupGlobalHotkeyCallbacks sets up the callback for global hotkey events (RegisterHotKey)
func (c *Coordinator) setupGlobalHotkeyCallbacks() {
	if c.uiManager == nil {
		return
	}
	c.uiManager.SetGlobalHotkeyCallback(func(command string) {
		c.handleGlobalHotkeyCommand(command)
	})
}

// handleGlobalHotkeyCommand handles a global hotkey event dispatched from the UI thread
func (c *Coordinator) handleGlobalHotkeyCommand(command string) {
	c.logger.Debug("Global hotkey command", "command", command)
	switch command {
	case "switch_engine":
		c.handleGlobalSwitchEngine()
	case "toggle_full_width":
		c.handleToolbarToggleWidth()
	case "toggle_punct":
		c.handleToolbarTogglePunct()
	case "toggle_toolbar":
		c.HandleMenuCommand("toggle_toolbar")
	case "open_settings":
		c.HandleMenuCommand("open_settings")
	}
}

// handleGlobalSwitchEngine handles schema switch triggered by RegisterHotKey
func (c *Coordinator) handleGlobalSwitchEngine() {
	c.mu.Lock()
	if c.engineMgr == nil {
		c.mu.Unlock()
		return
	}
	hadInput := len(c.inputBuffer) > 0
	c.clearState()
	if hadInput {
		c.hideUI()
	}
	var available []string
	if c.config != nil {
		available = c.config.Schema.Available
	}
	c.mu.Unlock()

	result, err := c.engineMgr.ToggleSchema(available)
	if err != nil {
		c.logger.Error("Failed to switch schema via global hotkey", "error", err)
		return
	}
	c.logger.Info("Schema switched via global hotkey", "newSchema", result.NewSchemaID)

	// 记录跳过的异常方案
	for id, errMsg := range result.SkippedSchemas {
		c.logger.Warn("Schema skipped due to error", "schemaID", id, "error", errMsg)
	}

	// 保存到用户配置 + 同步 RPC 层内存配置 + 通知设置端订阅者
	notifier := c.eventNotifier
	go func() {
		if c.cfgMu != nil && c.config != nil {
			c.cfgMu.Lock()
			c.config.Schema.Active = result.NewSchemaID
			cfgCopy := *c.config
			c.cfgMu.Unlock()

			if err := config.Save(&cfgCopy); err != nil {
				c.logger.Error("Failed to save schema to config", "error", err)
			} else {
				c.logger.Debug("Schema saved to config", "schema", result.NewSchemaID)
			}
		}
		if notifier != nil {
			notifier.NotifyConfigUpdate()
		}
	}()

	c.mu.Lock()
	if len(result.SkippedSchemas) > 0 || len(result.PendingSchemas) > 0 {
		c.showEngineIndicatorWithStatus(result.SkippedSchemas, result.PendingSchemas)
	} else {
		c.showEngineIndicator()
	}
	c.broadcastState()
	c.mu.Unlock()

	if hadInput && c.bridgeServer != nil {
		server := c.bridgeServer
		go server.PushClearCompositionToActiveClient()
	}
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
		OnCopy: func(index int) {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateCopy(index)
		},
		OnCopyDebugBatch: func(maxPages int) {
			go c.handleCandidateCopyBatch(maxPages)
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

// handleCandidateSelect 处理鼠标点击选词（在独立 goroutine 中调用，通过 push 管道交付结果）
func (c *Coordinator) handleCandidateSelect(index int) {
	c.mu.Lock()

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	c.logger.Debug("Candidate selected via mouse", "pageIndex", index, "actualIndex", actualIndex)

	result := c.doSelectCandidate(actualIndex)
	bridgeServer := c.bridgeServer
	c.mu.Unlock()

	pushKeyEventResult(bridgeServer, result)
}

// handleCandidateHoverChange handles hover state change（异步 tooltip 查询）
func (c *Coordinator) handleCandidateHoverChange(index, tooltipX, tooltipY int) {
	c.logger.Debug("Candidate hover changed", "index", index, "tooltipX", tooltipX, "tooltipY", tooltipY)

	// 取消上一次查询并立即更新 hoverIdx，防止旧 goroutine 通过中间检查
	c.cancelTooltipQuery()
	c.tooltipMu.Lock()
	c.tooltipHoverIdx = index
	c.tooltipMu.Unlock()

	if c.uiManager != nil {
		c.uiManager.RefreshCandidates()
	}

	if index < 0 {
		if c.uiManager != nil {
			c.uiManager.HideTooltip()
		}
		return
	}

	// 在持有锁时获取候选数据
	c.mu.Lock()
	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		c.mu.Unlock()
		return
	}
	cand := c.candidates[actualIndex]
	delay := 100
	if c.config != nil && c.config.UI.TooltipDelay >= 0 {
		delay = c.config.UI.TooltipDelay
	}
	c.mu.Unlock()

	// 获取 tooltip service 引用
	c.tooltipMu.Lock()
	svc := c.tooltipService
	c.tooltipMu.Unlock()

	hasProviders := svc != nil && svc.HasEnabledProviders()
	if !hasProviders && cand.Code == "" {
		return
	}

	// 启动异步查询 goroutine
	ctx, cancel := context.WithCancel(context.Background())
	c.tooltipMu.Lock()
	c.tooltipCancel = cancel
	c.tooltipMu.Unlock()

	go c.runTooltipQuery(ctx, index, cand, svc, tooltipX, tooltipY, delay)
}

// handleCandidateMoveUp handles move up action from context menu.
// 五笔：所有可见候选（含前缀匹配）都可前移，规则按当前 inputBuffer 存储。
// 拼音普通候选：不支持前移。命令候选（短语）：通过 PhraseLayer 调整 position。
func (c *Coordinator) handleCandidateMoveUp(index int) {
	c.mu.Lock()

	c.logger.Debug("Candidate move up requested", "index", index)

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	if actualIndex <= 0 || actualIndex >= len(c.candidates) {
		c.mu.Unlock()
		return
	}

	if len(c.candidates) <= 1 {
		c.mu.Unlock()
		return
	}
	cand := c.candidates[actualIndex]

	// 命令候选（短语）：通过 PhraseLayer 调整位置
	if cand.IsCommand && cand.PhraseTemplate != "" {
		code := c.inputBuffer
		c.mu.Unlock()
		c.handlePhraseMoveUp(code, cand.PhraseTemplate)
		c.mu.Lock()
		c.updateCandidates()
		c.showUI()
		c.mu.Unlock()
		return
	}

	// 拼音引擎普通候选不支持前移/后移
	if c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin {
		c.mu.Unlock()
		return
	}

	// 前移 = pin(当前位置 - 1)
	code := c.inputBuffer
	targetPosition := actualIndex - 1
	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		dm.PinWord(code, cand.Text, targetPosition)
		if err := dm.SaveShadow(); err != nil {
			c.logger.Error("Failed to save shadow layer", "error", err)
		}
	}

	c.mu.Lock()
	c.updateCandidates()
	c.showUI()
	c.mu.Unlock()
}

// handleCandidateMoveDown handles move down action from context menu.
// 五笔：所有可见候选都可后移。拼音普通候选：不支持。命令候选（短语）：通过 PhraseLayer 调整。
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

	// 命令候选（短语）：通过 PhraseLayer 调整位置
	if cand.IsCommand && cand.PhraseTemplate != "" {
		code := c.inputBuffer
		c.mu.Unlock()
		c.handlePhraseMoveDown(code, cand.PhraseTemplate)
		c.mu.Lock()
		c.updateCandidates()
		c.showUI()
		c.mu.Unlock()
		return
	}

	// 拼音引擎普通候选不支持后移
	if c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin {
		c.mu.Unlock()
		return
	}

	// 后移 = pin(当前位置 + 1)
	code := c.inputBuffer
	targetPosition := actualIndex + 1
	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		dm.PinWord(code, cand.Text, targetPosition)
		if err := dm.SaveShadow(); err != nil {
			c.logger.Error("Failed to save shadow layer", "error", err)
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

	cand := c.candidates[actualIndex]

	// 命令候选（短语）：通过 PhraseLayer 置顶
	if cand.IsCommand && cand.PhraseTemplate != "" {
		code := c.inputBuffer
		c.mu.Unlock()
		c.handlePhraseMoveToTop(code, cand.PhraseTemplate)
		c.mu.Lock()
		c.updateCandidates()
		c.showUI()
		c.mu.Unlock()
		return
	}

	// 统一用 inputBuffer 作为 code（规则只在当前输入编码下生效）
	code := c.inputBuffer

	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		dm.PinWord(code, cand.Text, 0)
		if err := dm.SaveShadow(); err != nil {
			c.logger.Error("Failed to save shadow layer", "error", err)
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
		dm := c.engineMgr.GetDictManager()
		dm.DeleteWord(code, cand.Text)
		if err := dm.SaveShadow(); err != nil {
			c.logger.Error("Failed to save shadow layer", "error", err)
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

	cand := c.candidates[actualIndex]

	// 命令候选（短语）：移除用户位置覆盖，恢复系统默认
	if cand.IsCommand && cand.PhraseTemplate != "" {
		code := c.inputBuffer
		c.mu.Unlock()
		c.handlePhraseReset(code, cand.PhraseTemplate)
		c.mu.Lock()
		c.updateCandidates()
		c.showUI()
		c.mu.Unlock()
		return
	}

	// 统一用 inputBuffer 作为 code
	code := c.inputBuffer

	c.mu.Unlock()

	// Remove shadow rule without lock
	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		dm.RemoveShadowRule(code, cand.Text)
		if err := dm.SaveShadow(); err != nil {
			c.logger.Error("Failed to save shadow layer", "error", err)
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

// ===== 短语位置调整辅助方法 =====

// handlePhraseMoveUp 通过 PhraseLayer 将短语前移一位
func (c *Coordinator) handlePhraseMoveUp(code, templateText string) {
	if c.engineMgr == nil {
		return
	}
	phraseLayer := c.engineMgr.GetDictManager().GetPhraseLayer()
	if phraseLayer == nil {
		return
	}
	if err := phraseLayer.MovePhraseUp(code, templateText); err != nil {
		c.logger.Error("Failed to move phrase up", "error", err, "code", code)
	}
}

// handlePhraseMoveDown 通过 PhraseLayer 将短语后移一位
func (c *Coordinator) handlePhraseMoveDown(code, templateText string) {
	if c.engineMgr == nil {
		return
	}
	phraseLayer := c.engineMgr.GetDictManager().GetPhraseLayer()
	if phraseLayer == nil {
		return
	}
	if err := phraseLayer.MovePhraseDown(code, templateText); err != nil {
		c.logger.Error("Failed to move phrase down", "error", err, "code", code)
	}
}

// handlePhraseMoveToTop 通过 PhraseLayer 将短语置顶
func (c *Coordinator) handlePhraseMoveToTop(code, templateText string) {
	if c.engineMgr == nil {
		return
	}
	phraseLayer := c.engineMgr.GetDictManager().GetPhraseLayer()
	if phraseLayer == nil {
		return
	}
	if err := phraseLayer.MovePhraseToTop(code, templateText); err != nil {
		c.logger.Error("Failed to move phrase to top", "error", err, "code", code)
	}
}

// handlePhraseReset 移除短语的用户位置覆盖，恢复系统默认
func (c *Coordinator) handlePhraseReset(code, templateText string) {
	if c.engineMgr == nil {
		return
	}
	phraseLayer := c.engineMgr.GetDictManager().GetPhraseLayer()
	if phraseLayer == nil {
		return
	}
	if err := phraseLayer.ResetPhraseOverride(code, templateText); err != nil {
		c.logger.Error("Failed to reset phrase override", "error", err, "code", code)
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
		server := c.bridgeServer
		go server.PushClearCompositionToActiveClient()
	}

	// Sync punctuation with mode if enabled
	if c.punctFollowMode {
		c.chinesePunctuation = c.chineseMode
	}

	// Reset punctuation converter state
	c.punctConverter.Reset()

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

	// 更新状态提示窗口
	c.updateStatusIndicator()

	// Broadcast state to toolbar and all TSF clients
	c.broadcastState()
}

// handleToolbarToggleWidth handles width toggle from toolbar click
func (c *Coordinator) handleToolbarToggleWidth() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.applyToggleFullWidth()
	c.logger.Debug("Full-width toggled via toolbar", "fullWidth", c.fullWidth)
	c.broadcastState()
}

// handleToolbarTogglePunct handles punctuation toggle from toolbar click
func (c *Coordinator) handleToolbarTogglePunct() {
	c.mu.Lock()
	defer c.mu.Unlock()

	// Don't toggle punctuation in English mode
	if !c.chineseMode {
		return
	}

	c.applyTogglePunct()
	c.logger.Debug("Chinese punctuation toggled via toolbar", "chinesePunctuation", c.chinesePunctuation)
	c.broadcastState()
}

// handleToolbarOpenSettings handles settings button click from toolbar
func (c *Coordinator) handleToolbarOpenSettings() {
	c.logger.Info("Opening settings from toolbar")
	if c.uiManager != nil {
		c.uiManager.OpenSettings()
	}
}

// handleToolbarPositionChanged handles toolbar position change (after dragging).
// Saves the position per monitor so it can be restored on next focus gain.
func (c *Coordinator) handleToolbarPositionChanged(x, y int) {
	// Identify the monitor by its work-area right/bottom edges.
	// This call is safe outside the coordinator lock (pure Win32 query).
	_, _, monRight, monBottom := ui.GetMonitorWorkAreaFromPoint(x, y)
	key := ui.MonitorKeyStr(monRight, monBottom)

	c.mu.Lock()
	if c.toolbarUserPos == nil {
		c.toolbarUserPos = make(map[string]image.Point)
	}
	c.toolbarUserPos[key] = image.Point{X: x, Y: y}
	c.mu.Unlock()

	c.logger.Debug("Toolbar user position saved", "monitorKey", key, "x", x, "y", y)
	c.saveToolbarPositions()
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

	// Build schema menu items from config available list
	var schemaMenuItems []ui.SchemaMenuItem
	c.mu.Lock()
	if c.config != nil && c.engineMgr != nil {
		for _, schemaID := range c.config.Schema.Available {
			name := c.engineMgr.GetSchemaNameByID(schemaID)
			schemaMenuItems = append(schemaMenuItems, ui.SchemaMenuItem{ID: schemaID, Name: name})
		}
	}
	currentSchemaID := ""
	if c.engineMgr != nil {
		currentSchemaID = c.engineMgr.GetCurrentSchemaID()
	}

	// Get current theme style from config
	currentThemeStyle := config.ThemeStyleSystem
	if c.config != nil && c.config.UI.ThemeStyle != "" {
		currentThemeStyle = c.config.UI.ThemeStyle
	}
	currentFilterMode := config.FilterSmart
	if c.config != nil && c.config.Input.FilterMode != "" {
		currentFilterMode = c.config.Input.FilterMode
	}
	activeProcessName := c.activeProcessName
	skipCaretPending := c.activeCompatRule != nil && c.activeCompatRule.SkipCaretPending
	state := ui.UnifiedMenuState{
		ChineseMode:       c.chineseMode,
		FullWidth:         c.fullWidth,
		ChinesePunct:      c.chinesePunctuation,
		ToolbarVisible:    c.toolbarVisible,
		Schemas:           schemaMenuItems,
		CurrentSchemaID:   currentSchemaID,
		CurrentFilterMode: currentFilterMode,
		Themes:            themeMenuItems,
		CurrentThemeID:    c.uiManager.GetCurrentThemeID(),
		CurrentThemeStyle: currentThemeStyle,
		Version:           c.version,
		ActiveProcessName: activeProcessName,
		SkipCaretPending:  skipCaretPending,
		S2TEnabled:        c.config != nil && c.config.S2T.Enabled,
		S2TVariant: func() config.S2TVariant {
			if c.config == nil {
				return config.S2TStandard
			}
			return c.config.S2T.Variant
		}(),
	}
	c.mu.Unlock()

	capturedProcess := activeProcessName
	c.uiManager.ShowUnifiedMenu(screenX, screenY, flipRefY, state, func(id int) {
		go c.handleUnifiedMenuAction(id, capturedProcess)
	})
}

// handleUnifiedMenuAction handles a menu item selection from the unified menu.
// capturedProcess 是菜单弹出时记录的进程名，避免菜单关闭期间 FocusLost 清空 activeProcessName。
func (c *Coordinator) handleUnifiedMenuAction(id int, capturedProcess string) {
	switch {
	case id == ui.UnifiedMenuSchemaEnglish:
		c.handleSwitchToEnglish()
	case id >= ui.UnifiedMenuSchemaBase && id < ui.UnifiedMenuSchemaBase+50:
		c.handleSchemaMenuSelection(id - ui.UnifiedMenuSchemaBase)
	case id == ui.UnifiedMenuToggleWidth:
		c.handleToolbarToggleWidth()
	case id == ui.UnifiedMenuTogglePunct:
		c.handleToolbarTogglePunct()
	case id == ui.UnifiedMenuToggleToolbar:
		c.HandleMenuCommand("toggle_toolbar")
	case id == ui.UnifiedMenuToggleS2T:
		c.mu.Lock()
		c.handleToggleS2T()
		c.mu.Unlock()
	case id >= ui.UnifiedMenuS2TVariantBase && id < ui.UnifiedMenuS2TVariantBase+10:
		variantIndex := id - ui.UnifiedMenuS2TVariantBase
		variants := []config.S2TVariant{
			config.S2TStandard,
			config.S2TTaiwan,
			config.S2TTaiwanPhrase,
			config.S2THongKong,
		}
		if variantIndex >= 0 && variantIndex < len(variants) {
			c.mu.Lock()
			c.handleSetS2TVariant(variants[variantIndex])
			c.mu.Unlock()
		}
	case id == ui.UnifiedMenuReloadConfig:
		c.logger.Info("Reload config from unified menu")
		c.HandleMenuCommand("reload_config")
	case id == ui.UnifiedMenuRestartService:
		c.logger.Info("Restart service requested from unified menu")
		c.resetAndResync()
	case id == ui.UnifiedMenuSkipCaretPending:
		go c.handleToggleSkipCaretPending(capturedProcess)
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
	case id >= ui.UnifiedMenuThemeStyleBase && id < ui.UnifiedMenuThemeStyleBase+10:
		// Theme style selection (system/light/dark)
		styleIndex := id - ui.UnifiedMenuThemeStyleBase
		styles := []config.ThemeStyle{config.ThemeStyleSystem, config.ThemeStyleLight, config.ThemeStyleDark}
		if styleIndex >= 0 && styleIndex < len(styles) {
			newStyle := styles[styleIndex]
			c.logger.Info("Theme style selected from unified menu", "style", newStyle)
			c.mu.Lock()
			if c.config != nil {
				c.config.UI.ThemeStyle = newStyle
			}
			// Apply the style change
			if c.uiManager != nil && c.config != nil {
				c.updateThemeStyle(&c.config.UI)
			}
			c.mu.Unlock()
			c.saveThemeStyleConfig(newStyle)
		}
	case id >= ui.UnifiedMenuFilterModeBase && id < ui.UnifiedMenuFilterModeBase+10:
		// Filter mode selection
		modeIndex := id - ui.UnifiedMenuFilterModeBase
		modes := []config.FilterMode{config.FilterSmart, config.FilterGeneral, config.FilterGB18030}
		if modeIndex >= 0 && modeIndex < len(modes) {
			newMode := modes[modeIndex]
			c.logger.Info("Filter mode selected from unified menu", "mode", newMode)
			c.mu.Lock()
			if c.config != nil {
				c.config.Input.FilterMode = newMode
			}
			if c.engineMgr != nil {
				c.engineMgr.UpdateFilterMode(newMode)
			}
			c.mu.Unlock()
			c.saveFilterModeConfig(newMode)
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

// handleSwitchToEnglish switches to English mode from the schema submenu
func (c *Coordinator) handleSwitchToEnglish() {
	c.mu.Lock()
	defer c.mu.Unlock()

	if !c.chineseMode {
		return // already English
	}

	c.chineseMode = false

	// Clear any pending input
	hadInput := len(c.inputBuffer) > 0
	if hadInput {
		c.clearState()
		c.hideUI()
	}

	// Notify TSF to clear composition
	if hadInput && c.bridgeServer != nil {
		server := c.bridgeServer
		go server.PushClearCompositionToActiveClient()
	}

	// Sync punctuation with mode if enabled
	if c.punctFollowMode {
		c.chinesePunctuation = false
	}
	c.punctConverter.Reset()

	// Save runtime state
	c.saveRuntimeState()

	// Show mode indicator
	c.showModeIndicator()

	// Broadcast state
	c.broadcastState()
}

// handleSchemaMenuSelection handles schema selection from the schema submenu
func (c *Coordinator) handleSchemaMenuSelection(index int) {
	c.mu.Lock()

	if c.config == nil || c.engineMgr == nil {
		c.mu.Unlock()
		return
	}

	available := c.config.Schema.Available
	if index < 0 || index >= len(available) {
		c.mu.Unlock()
		return
	}

	targetSchemaID := available[index]
	currentSchemaID := c.engineMgr.GetCurrentSchemaID()

	// Switch to Chinese mode if needed
	if !c.chineseMode {
		c.chineseMode = true
		if c.punctFollowMode {
			c.chinesePunctuation = true
		}
		c.punctConverter.Reset()
	}

	// Clear any pending input
	hadInput := len(c.inputBuffer) > 0
	if hadInput {
		c.clearState()
		c.hideUI()
	}

	needSchemaSwitch := targetSchemaID != currentSchemaID
	c.mu.Unlock()

	// Switch schema (without coordinator lock, engine manager has its own lock)
	if needSchemaSwitch {
		if err := c.engineMgr.SwitchToSchemaByID(targetSchemaID); err != nil {
			c.logger.Error("Failed to switch schema from menu", "error", err)
		} else {
			// 同步 RPC 层内存配置 + 写盘 + 通知设置端订阅者
			if c.cfgMu != nil && c.config != nil {
				c.cfgMu.Lock()
				c.config.Schema.Active = targetSchemaID
				cfgCopy := *c.config
				c.cfgMu.Unlock()

				if err := config.Save(&cfgCopy); err != nil {
					c.logger.Error("Failed to save schema to config", "error", err)
				}
			}
			if c.eventNotifier != nil {
				c.eventNotifier.NotifyConfigUpdate()
			}
		}
	}

	c.mu.Lock()
	c.saveRuntimeState()
	c.showEngineIndicator()
	c.broadcastState()
	c.mu.Unlock()

	// Notify TSF to clear composition if there was active input
	if hadInput && c.bridgeServer != nil {
		server := c.bridgeServer
		go server.PushClearCompositionToActiveClient()
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
	bridgeServer := c.bridgeServer
	c.mu.Unlock()

	// Notify active TSF client to clear inline composition before restart
	// This prevents dangling composition state in applications
	if bridgeServer != nil {
		bridgeServer.PushClearCompositionToActiveClient()
	}

	// Unregister global hotkeys before exit
	if c.uiManager != nil {
		c.uiManager.UnregisterGlobalHotkeys()
	}

	// Request process restart through the restart manager
	RequestRestart()
}

// syncToolbarState synchronizes the current state to the toolbar
// Note: This should be called with lock held, or use broadcastState() instead
func (c *Coordinator) syncToolbarState() {
	c.syncToolbarStateNoLock()
}

// handleToggleSkipCaretPending 切换指定应用的"即时候选"标志，写入用户 compat.yaml
// 并重新加载兼容性规则，使改动立即生效。
// processName 在菜单弹出时捕获，避免菜单关闭期间 FocusLost 清空 activeProcessName 导致操作失效。
func (c *Coordinator) handleToggleSkipCaretPending(processName string) {
	if processName == "" {
		return
	}

	newValue, err := config.ToggleUserSkipCaretPending(processName)
	if err != nil {
		c.logger.Error("Failed to toggle skip_caret_pending", "process", processName, "error", err)
		return
	}
	c.logger.Info("Toggled skip_caret_pending", "process", processName, "enabled", newValue)

	// 重新加载 compat 规则，使本次改动对当前会话立即生效
	newCompat := config.LoadAppCompat()
	c.mu.Lock()
	c.appCompat = newCompat
	// 用捕获的进程名更新规则，不依赖可能已被 FocusLost 清空的 activeProcessName
	c.activeCompatRule = newCompat.GetRule(processName)
	c.mu.Unlock()
}

// pushKeyEventResult 将 KeyEventResult 通过 bridge push 管道发送给活跃 TSF 客户端。
// 用于鼠标等异步路径（无法通过 return 交付结果）。
func pushKeyEventResult(srv BridgeServer, result *bridge.KeyEventResult) {
	if result == nil || srv == nil {
		return
	}
	switch result.Type {
	case bridge.ResponseTypeInsertText:
		srv.PushCommitTextToActiveClient(result.Text)
	case bridge.ResponseTypeUpdateComposition:
		srv.PushUpdateCompositionToActiveClient(result.Text, result.CaretPos)
	case bridge.ResponseTypeClearComposition:
		srv.PushClearCompositionToActiveClient()
	}
}
