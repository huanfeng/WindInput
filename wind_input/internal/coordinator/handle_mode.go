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

	// Show A for Caps Lock ON, 英 for OFF
	indicator := "英"
	if on {
		indicator = "A"
	}

	x, y := c.getIndicatorPosition()
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
	// Only hide UI if there was active input, to avoid hide→show flicker
	if hadInput {
		c.hideUI()
	}

	// 切换方案
	newSchemaID, err := c.engineMgr.ToggleSchema()
	if err != nil {
		c.logger.Error("Failed to switch schema", "error", err)
		return nil
	}

	c.logger.Info("Schema switched", "newSchema", newSchemaID)

	// 保存到用户配置
	go func() {
		if err := config.UpdateSchemaActive(newSchemaID); err != nil {
			c.logger.Error("Failed to save schema to config", "error", err)
		} else {
			c.logger.Debug("Schema saved to config", "schema", newSchemaID)
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

// showEngineIndicator 显示引擎切换指示器（复合显示引擎名+当前模式）
func (c *Coordinator) showEngineIndicator() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	name, _ := c.engineMgr.GetSchemaDisplayInfo()
	text := "中·" + name

	x, y := c.getIndicatorPosition()
	c.uiManager.ShowModeIndicator(text, x, y)
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
