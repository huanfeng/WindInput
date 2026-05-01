// handle_config_state.go — 状态查询、转换与持久化
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
)

// saveToolbarConfig saves the toolbar configuration to file
func (c *Coordinator) saveToolbarConfig() {
	// 调用者持有锁，安全读取
	visible := c.toolbarVisible
	notifier := c.eventNotifier

	go func() {
		c.cfgMu.Lock()
		c.config.Toolbar.Visible = visible
		cfgCopy := *c.config
		c.cfgMu.Unlock()

		if err := config.Save(&cfgCopy); err != nil {
			c.logger.Error("Failed to save toolbar config", "error", err)
		} else {
			c.logger.Debug("Toolbar config saved")
		}
		if notifier != nil {
			notifier.NotifyConfigUpdate()
		}
	}()
}

// saveThemeConfig saves the theme name to config
func (c *Coordinator) saveThemeConfig(themeName string) {
	notifier := c.eventNotifier
	go func() {
		c.cfgMu.Lock()
		c.config.UI.Theme = themeName
		cfgCopy := *c.config
		c.cfgMu.Unlock()

		if err := config.Save(&cfgCopy); err != nil {
			c.logger.Error("Failed to save theme config", "error", err)
		} else {
			c.logger.Debug("Theme config saved", "theme", themeName)
		}
		if notifier != nil {
			notifier.NotifyConfigUpdate()
		}
	}()
}

// saveThemeStyleConfig saves the theme style to config
func (c *Coordinator) saveThemeStyleConfig(themeStyle config.ThemeStyle) {
	notifier := c.eventNotifier
	go func() {
		c.cfgMu.Lock()
		c.config.UI.ThemeStyle = themeStyle
		cfgCopy := *c.config
		c.cfgMu.Unlock()

		if err := config.Save(&cfgCopy); err != nil {
			c.logger.Error("Failed to save theme style config", "error", err)
		} else {
			c.logger.Debug("Theme style config saved", "themeStyle", themeStyle)
		}
		if notifier != nil {
			notifier.NotifyConfigUpdate()
		}
	}()
}

// saveFilterModeConfig saves the filter mode to config
func (c *Coordinator) saveFilterModeConfig(filterMode config.FilterMode) {
	notifier := c.eventNotifier
	go func() {
		c.cfgMu.Lock()
		c.config.Input.FilterMode = filterMode
		cfgCopy := *c.config
		c.cfgMu.Unlock()

		if err := config.Save(&cfgCopy); err != nil {
			c.logger.Error("Failed to save filter mode config", "error", err)
		} else {
			c.logger.Debug("Filter mode config saved", "filterMode", filterMode)
		}
		if notifier != nil {
			notifier.NotifyConfigUpdate()
		}
	}()
}

// handleStatusMenuAction 处理状态窗口右键菜单动作
func (c *Coordinator) handleStatusMenuAction(action ui.StatusMenuAction) {
	// cfgMu 先于 c.mu 获取，与 ApplyConfigUpdate → Update*Config 路径的锁顺序一致
	c.cfgMu.Lock()
	defer c.cfgMu.Unlock()
	c.mu.Lock()
	defer c.mu.Unlock()

	switch action {
	case ui.StatusMenuSwitchToAlways:
		c.config.UI.StatusIndicator.DisplayMode = "always"
		c.applyStatusIndicatorConfig()
		c.saveStatusIndicatorConfig()
		// 取消临时模式的待执行隐藏，然后立即显示
		if sw := c.uiManager.GetStatusWindow(); sw != nil {
			sw.CancelPendingHide()
		}
		c.updateStatusIndicator()
	case ui.StatusMenuSwitchToTemp:
		c.config.UI.StatusIndicator.DisplayMode = "temp"
		c.applyStatusIndicatorConfig()
		c.saveStatusIndicatorConfig()
		// 立即显示一次（触发临时模式的自动隐藏倒计时）
		c.updateStatusIndicator()
	case ui.StatusMenuSettings:
		if c.uiManager != nil {
			c.uiManager.OpenSettingsWithPage("appearance")
		}
	case ui.StatusMenuHide:
		c.config.UI.StatusIndicator.Enabled = false
		c.applyStatusIndicatorConfig()
		c.saveStatusIndicatorConfig()
		if c.uiManager != nil {
			c.uiManager.HideStatusIndicator()
		}
	}
}

// applyStatusIndicatorConfig 将当前配置应用到状态窗口
func (c *Coordinator) applyStatusIndicatorConfig() {
	if c.uiManager == nil {
		return
	}
	siCfg := c.config.UI.StatusIndicator
	c.uiManager.UpdateStatusIndicatorFullConfig(ui.StatusWindowConfig{
		Enabled:         siCfg.Enabled,
		DisplayMode:     ui.StatusDisplayMode(siCfg.DisplayMode),
		Duration:        siCfg.Duration,
		SchemaNameStyle: siCfg.SchemaNameStyle,
		ShowMode:        siCfg.ShowMode,
		ShowPunct:       siCfg.ShowPunct,
		ShowFullWidth:   siCfg.ShowFullWidth,
		PositionMode:    ui.StatusPositionMode(siCfg.PositionMode),
		OffsetX:         siCfg.OffsetX,
		OffsetY:         siCfg.OffsetY,
		CustomX:         siCfg.CustomX,
		CustomY:         siCfg.CustomY,
		FontSize:        siCfg.FontSize,
		Opacity:         siCfg.Opacity,
		BackgroundColor: siCfg.BackgroundColor,
		TextColor:       siCfg.TextColor,
		BorderRadius:    siCfg.BorderRadius,
	})
}

// saveStatusIndicatorConfig 异步保存状态提示配置到文件
func (c *Coordinator) saveStatusIndicatorConfig() {
	// 调用者持有 cfgMu + c.mu，安全读取
	siCfg := c.config.UI.StatusIndicator
	notifier := c.eventNotifier

	go func() {
		c.cfgMu.Lock()
		c.config.UI.StatusIndicator = siCfg
		cfgCopy := *c.config
		c.cfgMu.Unlock()

		if err := config.Save(&cfgCopy); err != nil {
			c.logger.Error("Failed to save status indicator config", "error", err)
		} else {
			c.logger.Debug("Status indicator config saved")
		}
		if notifier != nil {
			notifier.NotifyConfigUpdate()
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

// saveRuntimeState saves the current state if remember_last_state is enabled
// 调用者必须持有 c.mu 锁
func (c *Coordinator) saveRuntimeState() {
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
