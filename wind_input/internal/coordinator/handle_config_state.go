// handle_config_state.go — 状态查询、转换与持久化
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/pkg/config"
)

// saveToolbarConfig saves the toolbar configuration to file
func (c *Coordinator) saveToolbarConfig() {
	// Capture value while we hold the lock
	visible := c.toolbarVisible

	go func() {
		cfg, err := config.Load()
		if err != nil {
			cfg = config.DefaultConfig()
		}

		cfg.Toolbar.Visible = visible

		if err := config.Save(cfg); err != nil {
			c.logger.Error("Failed to save toolbar config", "error", err)
		} else {
			c.logger.Debug("Toolbar config saved")
		}
	}()
}

// saveThemeConfig saves the theme name to config
func (c *Coordinator) saveThemeConfig(themeName string) {
	go func() {
		cfg, err := config.Load()
		if err != nil {
			cfg = config.DefaultConfig()
		}

		cfg.UI.Theme = themeName

		if err := config.Save(cfg); err != nil {
			c.logger.Error("Failed to save theme config", "error", err)
		} else {
			c.logger.Debug("Theme config saved", "theme", themeName)
		}
	}()
}

// saveThemeStyleConfig saves the theme style to config
func (c *Coordinator) saveThemeStyleConfig(themeStyle string) {
	go func() {
		cfg, err := config.Load()
		if err != nil {
			cfg = config.DefaultConfig()
		}

		cfg.UI.ThemeStyle = themeStyle

		if err := config.Save(cfg); err != nil {
			c.logger.Error("Failed to save theme style config", "error", err)
		} else {
			c.logger.Debug("Theme style config saved", "themeStyle", themeStyle)
		}
	}()
}

// saveFilterModeConfig saves the filter mode to config
func (c *Coordinator) saveFilterModeConfig(filterMode string) {
	go func() {
		cfg, err := config.Load()
		if err != nil {
			cfg = config.DefaultConfig()
		}

		cfg.Input.FilterMode = filterMode

		if err := config.Save(cfg); err != nil {
			c.logger.Error("Failed to save filter mode config", "error", err)
		} else {
			c.logger.Debug("Filter mode config saved", "filterMode", filterMode)
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
