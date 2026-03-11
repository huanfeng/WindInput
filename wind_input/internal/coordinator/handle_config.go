// handle_config.go — 配置热更新
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
)

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
		// 设置文本渲染模式
		if uiConfig.TextRenderMode != "" {
			c.uiManager.SetTextRenderMode(uiConfig.TextRenderMode)
		}
		// 设置候选框GDI字体参数
		if uiConfig.GDIFontWeight > 0 || uiConfig.GDIFontScale > 0 {
			c.uiManager.SetGDIFontParams(uiConfig.GDIFontWeight, uiConfig.GDIFontScale)
		}
		// 设置菜单GDI字体参数
		if uiConfig.MenuFontWeight > 0 {
			c.uiManager.SetMenuFontParams(uiConfig.MenuFontWeight, uiConfig.GDIFontScale)
		}
		// 设置菜单字体大小
		if uiConfig.MenuFontSize > 0 {
			c.uiManager.SetMenuFontSize(uiConfig.MenuFontSize)
		}
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
			// 同步词库管理器的活跃引擎
			if dm := c.engineMgr.GetDictManager(); dm != nil {
				dm.SetActiveEngine(string(newType))
			}
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
	c.engineMgr.UpdatePinyinOptions(&engineConfig.Pinyin)

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
