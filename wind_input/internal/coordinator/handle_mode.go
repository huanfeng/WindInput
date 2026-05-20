// handle_mode.go — 模式切换、CapsLock 状态、引擎切换
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/pkg/config"
)

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

// HandleSystemModeSwitch handles system-initiated mode switch (e.g., Ctrl+Space).
// Unlike HandleToggleMode, the target mode is decided by the system — Go must follow.
// Returns commitText if CommitOnSwitch is enabled and there's pending input.
func (c *Coordinator) HandleSystemModeSwitch(chineseMode bool) (commitText string) {
	c.mu.Lock()
	defer c.mu.Unlock()

	// If mode is already the same, nothing to do
	if c.chineseMode == chineseMode {
		c.logger.Debug("System mode switch: already in target mode", "chineseMode", chineseMode)
		return ""
	}

	// 切换模式 = 短语终止符，通知造词策略（码表自动造词）
	if c.chineseMode && c.engineMgr != nil {
		c.engineMgr.OnPhraseTerminated()
	}

	// Check CommitOnSwitch: when switching FROM Chinese, commit pending input code
	if c.config != nil && c.config.Hotkeys.CommitOnSwitch && c.chineseMode {
		commitText = c.getPendingBufferText()
		if commitText != "" {
			c.logger.Debug("SystemModeSwitch CommitOnSwitch: committing input code")
		}
	}

	// Force set to target mode (not toggle)
	c.chineseMode = chineseMode
	c.logger.Debug("Mode switched via system", "chineseMode", c.chineseMode, "hasCommitText", commitText != "")

	// Clear any pending input when switching modes
	if c.hasPendingInput() {
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

	return commitText
}

// HandleToggleMode toggles the input mode and returns the new state
func (c *Coordinator) HandleToggleMode() (commitText string, chineseMode bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

	// 切换模式 = 短语终止符，通知造词策略（码表自动造词）
	if c.chineseMode && c.engineMgr != nil {
		c.engineMgr.OnPhraseTerminated()
	}

	// Check if CommitOnSwitch is enabled and there's pending input
	// When switching from Chinese to English, commit the raw input code (not the candidate)
	// because the user wants to type English, so we output the original typed characters
	if c.config != nil && c.config.Hotkeys.CommitOnSwitch && c.chineseMode {
		commitText = c.getPendingBufferText()
		if commitText != "" {
			c.logger.Debug("CommitOnSwitch: committing input code")
		}
	}

	c.chineseMode = !c.chineseMode
	c.logger.Debug("Mode toggled via IPC", "chineseMode", c.chineseMode, "hasCommitText", commitText != "")

	// Clear any pending input when switching modes
	if c.hasPendingInput() {
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
	// CapsLock 状态变化时统一走合并状态显示
	// getStatusModeLabel() 内部会检查 c.capsLockOn 返回 "A"
	c.updateStatusIndicator()
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
	// Only hide UI if there was active input, to avoid hide→show flicker
	if hadInput {
		c.hideUI()
	}

	// 按配置中的 Available 列表切换方案
	var available []string
	if c.config != nil {
		available = c.config.Schema.Available
	}
	result, err := c.engineMgr.ToggleSchema(available)
	if err != nil {
		c.logger.Error("Failed to switch schema", "error", err)
		return nil
	}

	c.logger.Info("Schema switched", "newSchema", result.NewSchemaID)

	// 记录跳过的异常方案
	for id, errMsg := range result.SkippedSchemas {
		c.logger.Warn("Schema skipped due to error", "schemaID", id, "error", errMsg)
	}

	// 保存到用户配置 + 同步 RPC 层内存配置 + 通知设置端订阅者
	// 用 goroutine 是因为当前持有 c.mu，而锁顺序要求 cfgMu → c.mu，必须放到锁外完成
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

	// 显示引擎指示器（带异常 / 准备中状态后缀）
	if len(result.SkippedSchemas) > 0 || len(result.PendingSchemas) > 0 {
		c.showEngineIndicatorWithStatus(result.SkippedSchemas, result.PendingSchemas)
	} else {
		c.showEngineIndicator()
	}

	// 广播状态更新到工具栏和所有 TSF 客户端（更新 iconLabel 等）
	c.broadcastState()

	// 返回 ClearComposition 让 C++ 端清除 _isComposing 状态
	if hadInput {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

// showEngineIndicator 显示引擎切换指示器（使用方案名称）
func (c *Coordinator) showEngineIndicator() {
	// Reuse showModeIndicator which now uses schema name
	c.showModeIndicator()
}

// showEngineIndicatorWithStatus 显示带状态后缀的引擎指示器。
// skipped: 真正加载失败的方案（提示"X 异常"，请用户排查）
// pending: 资源后台生成中的方案（提示"X 准备中"，不需要用户介入）
// 两个 map 都为空时与 showEngineIndicator 等价。
func (c *Coordinator) showEngineIndicatorWithStatus(skipped, pending map[string]string) {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// 获取当前方案名称作为主文本
	var modeText string
	if c.engineMgr != nil {
		name, _ := c.engineMgr.GetSchemaDisplayInfo()
		if name != "" {
			modeText = name
		} else {
			modeText = "中"
		}
	}

	resolveName := func(id string) string {
		if c.engineMgr != nil {
			if sm := c.engineMgr.GetSchemaManager(); sm != nil {
				if s := sm.GetSchema(id); s != nil && s.Schema.Name != "" {
					return s.Schema.Name
				}
			}
		}
		return id
	}

	// 拼装片段："五笔异常"、"全拼准备中"
	var parts []string
	for id := range pending {
		parts = append(parts, resolveName(id)+"准备中")
	}
	for id := range skipped {
		parts = append(parts, resolveName(id)+"异常")
	}

	if len(parts) > 0 {
		modeText += "（"
		for i, p := range parts {
			if i > 0 {
				modeText += "、"
			}
			modeText += p
		}
		modeText += "）"
	}

	x, y := c.getIndicatorPosition()
	c.uiManager.ShowModeIndicator(modeText, x, y)
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
