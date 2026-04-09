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

// HandleToggleMode toggles the input mode and returns the new state
func (c *Coordinator) HandleToggleMode() (commitText string, chineseMode bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

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

	if on {
		// CapsLock ON: always show "A"
		x, y := c.getIndicatorPosition()
		c.uiManager.ShowModeIndicator("A", x, y)
	} else {
		// CapsLock OFF: show current mode indicator (schema name or "英")
		c.showModeIndicator()
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

	// 保存到用户配置
	go func() {
		if err := config.UpdateSchemaActive(result.NewSchemaID); err != nil {
			c.logger.Error("Failed to save schema to config", "error", err)
		} else {
			c.logger.Debug("Schema saved to config", "schema", result.NewSchemaID)
		}
	}()

	// 显示引擎指示器（如果有跳过的方案，显示带异常提示的指示器）
	if len(result.SkippedSchemas) > 0 {
		c.showEngineIndicatorWithSkipped(result.SkippedSchemas)
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

// showEngineIndicatorWithSkipped 显示带异常方案提示的引擎指示器
// 当有方案因配置错误被跳过时，指示器文本中附加提示信息
func (c *Coordinator) showEngineIndicatorWithSkipped(skipped map[string]string) {
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

	// 收集跳过的方案名称
	var skippedNames []string
	for id := range skipped {
		displayName := id
		if c.engineMgr != nil {
			if sm := c.engineMgr.GetSchemaManager(); sm != nil {
				if s := sm.GetSchema(id); s != nil && s.Schema.Name != "" {
					displayName = s.Schema.Name
				}
			}
		}
		skippedNames = append(skippedNames, displayName)
	}

	// 构建提示文本：「全拼（五笔异常）」
	if len(skippedNames) > 0 {
		modeText += "（"
		for i, name := range skippedNames {
			if i > 0 {
				modeText += "、"
			}
			modeText += name + "异常"
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
