// handle_ui_callbacks.go — 工具栏与候选窗口 UI 回调
package coordinator

import (
	"context"
	"image"
	"strings"

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
		OnForegroundFullscreenChange: func(enter bool) {
			// Run in goroutine: callback is invoked from UI thread (toolbar
			// WndProc); OnShellFullscreenChange acquires c.mu and may issue
			// UI commands, so keep the WndProc non-blocking.
			go c.OnShellFullscreenChange(enter)
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
	case "take_screenshot":
		if c.uiManager != nil {
			c.uiManager.TakeUIScreenshots()
		}
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
			cfgCopy := c.config.Clone()
			c.cfgMu.Unlock()

			if err := config.Save(cfgCopy); err != nil {
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
		OnHoverChange: func(index, tooltipX, tooltipBelowY, tooltipAboveY int) {
			// Run in goroutine to avoid blocking UI thread
			go c.handleCandidateHoverChange(index, tooltipX, tooltipBelowY, tooltipAboveY)
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
		OnCopyDebugTooltip: func(index int) {
			go c.handleCandidateCopyTooltip(index)
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
		OnDragEnd: func(x, y int) {
			// 回调已在 window_mouse 中通过 goroutine 调用，这里不再嵌套 goroutine
			c.handleCandidateWindowDragEnd(x, y)
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

// handleCandidateHoverChange handles hover state change（异步 tooltip 查询）。
// tooltipBelowY 是候选下沿（首选 tooltip 顶端位置），tooltipAboveY 是候选上沿
// （下方空间不足时 tooltip 底端贴此处，tooltip 子系统在 Show 时根据屏幕工作区自动选择）。
func (c *Coordinator) handleCandidateHoverChange(index, tooltipX, tooltipBelowY, tooltipAboveY int) {
	c.logger.Debug("Candidate hover changed", "index", index, "tooltipX", tooltipX, "belowY", tooltipBelowY, "aboveY", tooltipAboveY)

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

	go c.runTooltipQuery(ctx, index, cand, svc, tooltipX, tooltipBelowY, tooltipAboveY, delay)
}

// handleCandidateMoveUp handles move up action from context menu.
// 五笔/短语: 走 dm.PinWord(targetPosition-1), 短语候选附 ID 走 id 匹配。
// 拼音普通候选: 不支持前移 (没有稳定的目标位置语义)。
func (c *Coordinator) handleCandidateMoveUp(index int) {
	c.handleCandidateMove(index, -1, false)
}

// handleCandidateMoveDown handles move down action from context menu.
func (c *Coordinator) handleCandidateMoveDown(index int) {
	c.handleCandidateMove(index, +1, false)
}

// handleCandidateMoveTop handles move to top action from context menu.
func (c *Coordinator) handleCandidateMoveTop(index int) {
	c.handleCandidateMove(index, 0, true)
}

// handleCandidateMove 统一处理前移/后移/置顶。
//   - top=true: 目标位置 0
//   - top=false: 目标位置 = actualIndex + delta (clamp >= 0)
//
// 普通候选(无 ID): 拼音引擎不支持; 短语候选(有 ID): 所有引擎都支持。
func (c *Coordinator) handleCandidateMove(index, delta int, top bool) {
	c.mu.Lock()

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		c.mu.Unlock()
		return
	}
	if len(c.candidates) <= 1 {
		c.mu.Unlock()
		return
	}
	cand := c.candidates[actualIndex]

	// 字符组 / 字符串组子项: 顺序由 $AA(chars) / $SS(...) marker 定义,
	// 不允许走 Shadow pin 双轨漂移 (UI 菜单也 disable, 这里 defensive)
	if cand.IsGroupMember {
		c.mu.Unlock()
		return
	}

	// 拼音引擎普通候选不支持调位; 短语候选 (cand.ID 非空) 仍允许
	if cand.ID == "" && c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin {
		c.mu.Unlock()
		return
	}

	var targetPosition int
	switch {
	case top:
		if actualIndex == 0 {
			c.mu.Unlock()
			return // 已在首位
		}
		targetPosition = 0
	case delta < 0:
		if actualIndex == 0 {
			c.mu.Unlock()
			return
		}
		targetPosition = actualIndex - 1
	default:
		if actualIndex >= len(c.candidates)-1 {
			c.mu.Unlock()
			return
		}
		targetPosition = actualIndex + 1
	}

	code := c.inputBuffer
	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		dm.PinWord(code, cand.Text, cand.ID, targetPosition)
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
// 单字不允许删除（防止某个字永远打不出来）。短语候选 (cand.ID 非空) 一律允许。
func (c *Coordinator) handleCandidateDelete(index int) {
	c.mu.Lock()

	c.logger.Debug("Candidate delete requested", "index", index)

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		c.mu.Unlock()
		return
	}

	cand := c.candidates[actualIndex]

	// 字符组 / 字符串组子项: 不允许任何 pin/delete (UI 菜单 disable, 这里 defensive)
	if cand.IsGroupMember {
		c.mu.Unlock()
		return
	}

	// 单字不允许删除 (短语 ID 例外, 用户主动挑了具体单字候选)
	if cand.ID == "" && len([]rune(cand.Text)) <= 1 {
		c.logger.Debug("Cannot delete single character")
		c.mu.Unlock()
		return
	}

	// 统一用 inputBuffer 作为 code
	code := c.inputBuffer

	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		// 短语候选 (cand.ID 以 "phrase:" 开头, cand.PhraseTemplate 是 PhraseRecord.Text)
		// 走 PhraseRecord.Enabled = false (软删除), 不写 Shadow。
		// 这样设置 UI 的"启用" Switch 能反映该状态, 用户可恢复。
		// 字符组单字符候选 (IsGroupMember=true) UI 已 disable, 这里不到。
		if strings.HasPrefix(cand.ID, "phrase:") && cand.PhraseTemplate != "" {
			if err := dm.DisablePhrase(code, cand.PhraseTemplate); err != nil {
				c.logger.Error("Failed to disable phrase", "error", err, "code", code)
			}
		} else {
			// 普通候选: 走 DeleteWord。
			// word 优先用 PhraseTemplate (原 marker), 否则用 cand.Text:
			// user/temp dict 存的字面 $AA / $CC marker, applyValueExpansion 或
			// expandAACandidates 把 cand.Text 改写成了展开后显示文本, 直接用
			// cand.Text 无法在源词库 Remove 命中。详见 docs/design/candidate-actions.md §2.1。
			word := cand.Text
			if cand.PhraseTemplate != "" {
				word = cand.PhraseTemplate
			}
			dm.DeleteWord(code, word, cand.ID)
			if err := dm.SaveShadow(); err != nil {
				c.logger.Error("Failed to save shadow layer", "error", err)
			}
		}
	}

	c.mu.Lock()
	c.updateCandidates()
	c.showUI()
	c.mu.Unlock()
}

// handleCandidateResetDefault handles reset to default action from context menu.
// Removes all shadow rules for the candidate (id 优先, 否则 word)。
//
// 语义: 仅恢复位置调整 (Shadow Pinned), 不恢复删除 (DisablePhrase / Shadow Deleted)。
// 删除恢复走设置 UI, 详见 docs/design/candidate-actions.md §2 / §4。
func (c *Coordinator) handleCandidateResetDefault(index int) {
	c.mu.Lock()

	c.logger.Debug("Candidate reset default requested", "index", index)

	actualIndex := (c.currentPage-1)*c.candidatesPerPage + index
	if actualIndex < 0 || actualIndex >= len(c.candidates) {
		c.logger.Warn("Invalid candidate index for reset default", "actualIndex", actualIndex)
		c.mu.Unlock()
		return
	}

	cand := c.candidates[actualIndex]

	// 字符组 / 字符串组子项 (D 类): 不允许任何调整 (defensive 与 UI 菜单同步)。
	// TODO: 未来支持组内成员原地编辑 (允许在 IME 内改 chars 数组顺序)
	if cand.IsGroupMember {
		c.mu.Unlock()
		return
	}

	code := c.inputBuffer

	c.mu.Unlock()

	if c.engineMgr != nil {
		dm := c.engineMgr.GetDictManager()
		dm.RemoveShadowRule(code, cand.Text, cand.ID)
		if err := dm.SaveShadow(); err != nil {
			c.logger.Error("Failed to save shadow layer", "error", err)
		}
	}

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

// 短语位置调整辅助方法 (handlePhraseMoveUp/Down/ToTop/Reset) 已删除,
// 改由 handleCandidateMove* / handleCandidateDelete / handleCandidateResetDefault
// 统一走 Shadow API (按候选 ID 匹配), 见 R2 方案。

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
	state := c.buildUnifiedMenuState()
	capturedProcess := state.ActiveProcessName
	c.uiManager.ShowUnifiedMenu(screenX, screenY, flipRefY, state, func(id int) {
		go c.handleUnifiedMenuAction(id, capturedProcess)
	})
}

// buildUnifiedMenuState 收集统一菜单所需全部状态 (方案/主题/模式/过滤/简繁/高级)。
func (c *Coordinator) buildUnifiedMenuState() ui.UnifiedMenuState {
	// theme/schema 显示信息 (uiManager 调用不持 c.mu)
	themeInfos := c.uiManager.GetAvailableThemeInfos()
	themeMenuItems := make([]ui.ThemeMenuItem, len(themeInfos))
	for i, info := range themeInfos {
		themeMenuItems[i] = ui.ThemeMenuItem{ID: info.ID, DisplayName: info.DisplayName}
	}
	currentThemeID := c.uiManager.GetCurrentThemeID()

	c.mu.Lock()
	defer c.mu.Unlock()

	var schemaMenuItems []ui.SchemaMenuItem
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
	currentThemeStyle := config.ThemeStyleSystem
	if c.config != nil && c.config.UI.ThemeStyle != "" {
		currentThemeStyle = c.config.UI.ThemeStyle
	}
	currentFilterMode := config.FilterSmart
	if c.config != nil && c.config.Input.FilterMode != "" {
		currentFilterMode = c.config.Input.FilterMode
	}
	s2tVariant := config.S2TStandard
	if c.config != nil {
		s2tVariant = c.config.S2T.Variant
	}
	return ui.UnifiedMenuState{
		ChineseMode:          c.chineseMode,
		FullWidth:            c.fullWidth,
		ChinesePunct:         c.chinesePunctuation,
		ToolbarVisible:       c.toolbarVisible,
		Schemas:              schemaMenuItems,
		CurrentSchemaID:      currentSchemaID,
		CurrentFilterMode:    currentFilterMode,
		Themes:               themeMenuItems,
		CurrentThemeID:       currentThemeID,
		CurrentThemeStyle:    currentThemeStyle,
		Version:              c.version,
		ActiveProcessName:    c.activeProcessName,
		SkipCaretPending:     c.activeCompatRule != nil && c.activeCompatRule.SkipCaretPending,
		PinCandidatePosition: c.activeCompatRule != nil && c.activeCompatRule.PinCandidatePosition,
		S2TEnabled:           c.config != nil && c.config.S2T.Enabled,
		S2TVariant:           s2tVariant,
	}
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
	case id == ui.UnifiedMenuPinCandidatePosition:
		go c.handleTogglePinCandidatePosition(capturedProcess)
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
	case id == ui.UnifiedMenuTestToastInfo:
		c.uiManager.ShowToast(ui.ToastOptions{
			Title:    "Info Toast",
			Message:  "这是一条 Info 级通知。",
			Level:    ui.ToastInfo,
			Position: ui.ToastBottomRight,
			Duration: 3500,
		})
	case id == ui.UnifiedMenuTestToastSuccess:
		c.uiManager.ShowToastSuccess("操作成功完成。")
	case id == ui.UnifiedMenuTestToastWarn:
		c.uiManager.ShowToast(ui.ToastOptions{
			Title:    "警告",
			Message:  "这是一条 Warn 级通知，居中显示。",
			Level:    ui.ToastWarn,
			Position: ui.ToastCenter,
			Duration: 4000,
		})
	case id == ui.UnifiedMenuTestToastError:
		c.uiManager.ShowToastError("错误", "这是一条 Error 级通知, 用于测试居中 5s 显示。")
	case id == ui.UnifiedMenuTestToastLongMessage:
		c.uiManager.ShowToast(ui.ToastOptions{
			Title:    "长文本测试",
			Message:  "第一行: 用于验证多行渲染与最大宽度。\n第二行: 中文与 English mixed text 一起看会不会换行不齐。\n第三行: 这一行故意写得比较长, 看看截断和省略号会不会按预期工作。",
			Level:    ui.ToastInfo,
			Position: ui.ToastBottomRight,
			Duration: 6000,
		})
	case id >= ui.UnifiedMenuThemeBase && id < ui.UnifiedMenuThemeBase+100:
		// Theme selection
		themeIndex := id - ui.UnifiedMenuThemeBase
		themeInfos := c.uiManager.GetAvailableThemeInfos()
		if themeIndex >= 0 && themeIndex < len(themeInfos) {
			themeID := themeInfos[themeIndex].ID
			c.logger.Info("Theme selected from unified menu", "theme", themeID)
			c.uiManager.LoadTheme(themeID)
			c.notifyThemeFallbackIfAny()
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
				cfgCopy := c.config.Clone()
				c.cfgMu.Unlock()

				if err := config.Save(cfgCopy); err != nil {
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

// handleToggleSkipCaretPending 切换指定应用的"即时候选"标志，写入用户 compat.toml
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

// handleTogglePinCandidatePosition 切换指定应用的「固定候选位置」标志，写入用户 compat.toml
// 并重新加载兼容性规则、推送 pin 状态到 uiManager，使改动立即生效。
// 关闭时同步清空该应用在 state.toml 中已记忆的所有显示器位置。
// processName 在菜单弹出时捕获，避免菜单关闭期间 FocusLost 清空 activeProcessName 导致操作失效。
func (c *Coordinator) handleTogglePinCandidatePosition(processName string) {
	if processName == "" {
		return
	}

	newValue, err := config.ToggleUserPinCandidatePosition(processName)
	if err != nil {
		c.logger.Error("Failed to toggle pin_candidate_position", "process", processName, "error", err)
		return
	}
	c.logger.Info("Toggled pin_candidate_position", "process", processName, "enabled", newValue)

	newCompat := config.LoadAppCompat()
	procKey := strings.ToLower(processName)
	clearedMemory := false
	c.mu.Lock()
	c.appCompat = newCompat
	c.activeCompatRule = newCompat.GetRule(processName)
	if !newValue {
		// 关闭即清记忆：删掉该进程在 state.toml 中的所有显示器位置
		if _, ok := c.candidatePinPositions[procKey]; ok {
			delete(c.candidatePinPositions, procKey)
			clearedMemory = true
		}
	}
	c.mu.Unlock()

	c.syncCandidatePinStateToUI(processName)
	if clearedMemory {
		c.saveCandidatePinPositions()
	}
}

// handleCandidateWindowDragEnd 是候选窗拖动结束的回调：
// 仅当当前活跃应用启用了「固定候选位置」规则时，把新位置按 caret-所在显示器写入 state 并持久化。
// 未启用规则的应用拖动行为保持现状（仅会话内 dragPinned 有效，Hide 后自动重置）。
func (c *Coordinator) handleCandidateWindowDragEnd(x, y int) {
	c.mu.Lock()
	process := c.activeProcessName
	rule := c.activeCompatRule
	c.mu.Unlock()

	if rule == nil || !rule.PinCandidatePosition || process == "" {
		return
	}

	// 用拖动结束时窗口左上角所在显示器作为 key，与显示路径里的 caret 显示器查表对称
	_, _, workRight, workBottom := ui.GetMonitorWorkAreaFromPoint(x, y)
	monitorKey := ui.MonitorKeyStr(workRight, workBottom)

	procKey := strings.ToLower(process)
	c.mu.Lock()
	if c.candidatePinPositions == nil {
		c.candidatePinPositions = make(map[string]map[string][2]int)
	}
	if c.candidatePinPositions[procKey] == nil {
		c.candidatePinPositions[procKey] = make(map[string][2]int)
	}
	c.candidatePinPositions[procKey][monitorKey] = [2]int{x, y}
	c.mu.Unlock()

	c.syncCandidatePinStateToUI(process)
	c.saveCandidatePinPositions()
	c.logger.Debug("Candidate pin position recorded", "process", procKey, "monitor", monitorKey, "x", x, "y", y)
}

// syncCandidatePinStateToUI 拷贝指定应用的 pin 位置 map（按显示器键）推给 uiManager，
// 作为 doShowCandidates 决定候选窗坐标的依据。enabled 自动跟随当前 activeCompatRule 状态。
// 在焦点切换 / 菜单 toggle / 拖动落盘三处调用。
func (c *Coordinator) syncCandidatePinStateToUI(processName string) {
	if c.uiManager == nil {
		return
	}
	procKey := strings.ToLower(processName)

	c.mu.Lock()
	enabled := c.activeCompatRule != nil && c.activeCompatRule.PinCandidatePosition
	var positions map[string][2]int
	if enabled {
		if existing, ok := c.candidatePinPositions[procKey]; ok && len(existing) > 0 {
			positions = make(map[string][2]int, len(existing))
			for k, v := range existing {
				positions[k] = v
			}
		}
	}
	c.mu.Unlock()

	c.uiManager.SetActiveAppPinState(enabled, positions)
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
