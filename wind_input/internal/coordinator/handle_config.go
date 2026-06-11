// handle_config.go — 配置热更新
package coordinator

import (
	"sync"

	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// UpdateUIConfig 更新 UI 配置（热更新）
func (c *Coordinator) UpdateUIConfig(uiConfig *config.UIConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if uiConfig == nil {
		return
	}

	// 更新每页候选数（基础档 + 扩展档），再物化生效值并重算分页
	if uiConfig.Candidate.PerPage > 0 {
		c.candidatesPerPageBase = uiConfig.Candidate.PerPage
	}
	c.candidatesPerPageExtended = uiConfig.Candidate.PerPageExtended
	c.refreshEffectivePerPage()
	// 重新计算总页数
	if len(c.candidates) > 0 {
		c.totalPages = (len(c.candidates) + c.candidatesPerPage - 1) / c.candidatesPerPage
		if c.currentPage > c.totalPages {
			c.currentPage = c.totalPages
		}
	}

	// 更新配置引用
	if c.config != nil {
		c.config.UI = *uiConfig
	}

	// 通知 UI Manager 更新字体等设置
	if c.uiManager != nil {
		cand := &uiConfig.Candidate
		fontSpec := uiConfig.Font.Family
		if fontSpec == "" {
			fontSpec = uiConfig.Font.Path
		}
		c.uiManager.UpdateConfig(cand.FontSize, cand.FontSizeFollowTheme, fontSpec, cand.HideWindow)
		// 主题 behavior 用户覆盖层（哲学Y）：always_show_pager / show_page_number / vertical_max_width
		// 各自的「跟随主题」开关 + 用户值。最终值 = FollowTheme ? 主题 behavior : 用户值。
		c.uiManager.SetBehaviorOverrides(
			cand.AlwaysShowPager, cand.AlwaysShowPagerFollowTheme,
			cand.ShowPageNumber, cand.ShowPageNumberFollowTheme,
			cand.VerticalMaxWidth, cand.VerticalMaxWidthFollowTheme,
		)
		// Update candidate layout
		if cand.Layout != "" {
			c.uiManager.SetCandidateLayout(cand.Layout)
		}
		// Update hide preedit setting
		c.uiManager.SetHidePreedit(cand.InlinePreedit)
		// 用户全局序号标签覆盖
		c.uiManager.SetCandidateIndexLabels(cand.IndexLabels)
		// Update preedit display mode
		c.uiManager.SetPreeditMode(cand.PreeditMode)
		// 候选窗在光标上方时反转 bands 排列
		c.uiManager.SetFlipLayoutWhenAbove(cand.FlipWhenAbove)
		// Update pager display mode override
		c.uiManager.SetPagerBarDisplay(cand.PagerBarDisplay)
		c.uiManager.SetPageNumberDisplay(cand.PageNumberDisplay)
		// 更新完整状态提示配置（v1：旧顶层三字段已删除，迁移由 migrateV0toV1 完成）
		siCfg := uiConfig.StatusIndicator
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
		// 设置编码提示延迟
		c.uiManager.SetTooltipDelay(uiConfig.Tooltip.Delay)
		// 设置文本渲染模式
		if uiConfig.Font.RenderMode != "" {
			c.uiManager.SetTextRenderMode(uiConfig.Font.RenderMode)
		}
		// 设置候选框GDI字体参数
		if uiConfig.Font.GDIWeight > 0 || uiConfig.Font.GDIScale > 0 {
			c.uiManager.SetGDIFontParams(uiConfig.Font.GDIWeight, uiConfig.Font.GDIScale)
		}
		// 设置菜单GDI字体参数
		if uiConfig.Font.MenuWeight > 0 {
			c.uiManager.SetMenuFontParams(uiConfig.Font.MenuWeight, uiConfig.Font.GDIScale)
		}
		// 设置菜单字体大小
		if uiConfig.Font.MenuSize > 0 {
			c.uiManager.SetMenuFontSize(uiConfig.Font.MenuSize)
		}
		// 设置候选文本最大显示字符数
		c.uiManager.SetMaxCandidateChars(cand.MaxChars)
		// 更新主题风格和主题
		c.updateThemeStyle(uiConfig)
	}

	// 重建 tooltip service（配置可能已更新）
	c.rebuildTooltipServiceLocked()

	c.logger.Debug("UI config updated", "candidatesPerPage", c.candidatesPerPage)
}

// UpdateToolbarConfig 更新工具栏配置（热更新）
func (c *Coordinator) UpdateToolbarConfig(toolbarConfig *config.ToolbarConfig) {
	if toolbarConfig == nil {
		return
	}

	c.mu.Lock()
	c.toolbarVisible = toolbarConfig.Visible
	if c.config != nil {
		c.config.UI.Toolbar = *toolbarConfig
	}
	visible := c.toolbarVisible
	hideInFS := toolbarConfig.HideInFullscreen
	reducer := c.toolbarReducer
	c.mu.Unlock()

	// 事件投递必须在 mu 解锁后：sendCritical 最坏阻塞 100ms，若在持锁期间投，
	// 会与 reducer goroutine 在 snapshotToolbarShowParams 中等待 c.mu 形成对峙 ——
	// 不至于死锁（sendCritical 有超时），但每次最坏 100ms 延迟 + 事件 drop。
	if reducer != nil {
		reducer.sendCritical(toolbarEvent{kind: tevUserPreferenceChanged, visible: visible})
		reducer.sendCritical(toolbarEvent{kind: tevConfigChanged, visible: hideInFS})
	}

	c.logger.Debug("Toolbar config updated", "visible", visible)
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

	// 更新配置引用（special_modes 已属 features 节，registry 重建见 UpdateFeaturesConfig）
	if c.config != nil {
		c.config.Input = *inputConfig
	}

	// 更新自动配对配置
	if c.pairTracker != nil {
		c.pairTracker.UpdatePairs(inputConfig.AutoPair.ChinesePairs)
	}
	if c.pairTrackerEn != nil {
		c.pairTrackerEn.UpdatePairs(inputConfig.AutoPair.EnglishPairs)
	}
	// 根据配对表更新引号配对状态
	c.updatePairedQuotes(inputConfig.AutoPair.ChinesePairs)

	// 更新自定义标点映射
	c.punctConverter.SetCustomMappings(inputConfig.PunctCustom.Enabled, inputConfig.PunctCustom.Mappings)

	// 推送英文配对配置到 C++ 侧
	if c.bridgeServer != nil {
		go c.bridgeServer.PushEnglishPairConfigToActiveClient(
			inputConfig.AutoPair.English,
			inputConfig.AutoPair.EnglishPairs,
		)
	}

	c.hotkeysDirty = true // SelectKeyGroups/PageKeys 变化也影响热键
	c.logger.Debug("Input config updated", "punctFollowMode", c.punctFollowMode)
}

// UpdateStatsConfig updates runtime stats config and pushes it to TSF clients.
func (c *Coordinator) UpdateStatsConfig(statsConfig *config.StatsConfig) {
	if statsConfig == nil {
		return
	}

	enabled := statsConfig.Enabled
	trackEnglish := statsConfig.TrackEnglish

	c.mu.Lock()
	if c.config != nil {
		c.config.Features.Stats = *statsConfig
	}
	c.mu.Unlock()

	if c.bridgeServer != nil {
		go c.bridgeServer.PushStatsConfigToActiveClient(enabled, trackEnglish)
	}

	c.logger.Debug("Stats config updated", "enabled", enabled, "trackEnglish", trackEnglish)
}

// UpdateS2TConfig 更新简入繁出配置（来自设置界面）。
func (c *Coordinator) UpdateS2TConfig(s2tConfig *config.S2TConfig) {
	if s2tConfig == nil {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()

	if c.config != nil {
		c.config.Features.S2T = *s2tConfig
	}
	c.reconfigureS2T(*s2tConfig)
	if c.hasPendingInput() {
		c.updateCandidates()
		c.showUI()
	}
	c.logger.Debug("S2T config updated", "enabled", s2tConfig.Enabled, "variant", string(s2tConfig.Variant))
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

	// IME 已激活时重新注册全局快捷键，使新配置立即生效
	if c.imeActivated && c.uiManager != nil {
		c.uiManager.RegisterGlobalHotkeys(c.buildGlobalHotkeyEntries())
	}

	// activate_ime 走 Windows DirectSwitchHotkeys（per-app 切换到本输入法），
	// 同步到注册表使新键即时生效（ctfmon 监听变更）。
	if err := ui.SyncDirectSwitchHotkey(hotkeyConfig.ActivateIME); err != nil {
		c.logger.Warn("Failed to sync activate_ime DirectSwitch hotkey", "error", err)
	}

	c.logger.Debug("Hotkey config updated",
		"toggleModeKeys", hotkeyConfig.ToggleModeKeys,
		"switchEngine", hotkeyConfig.SwitchEngine)
}

// UpdateStartupConfig 更新启动配置
func (c *Coordinator) UpdateStartupConfig(startupConfig *config.GeneralConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if startupConfig == nil {
		return
	}

	// 更新配置引用
	if c.config != nil {
		c.config.General = *startupConfig
	}

	c.logger.Debug("Startup config updated", "rememberLastState", startupConfig.RememberLastState)
}

// UpdateFeaturesConfig 更新 features 节（热更新统一入口）：stats 推送、s2t 重配、
// 特殊模式注册表重建、cmdbar 前缀、quick_input（accent 等由消费方即时读 config）。
// 子方法各自持锁，本方法不嵌套加锁。
func (c *Coordinator) UpdateFeaturesConfig(featuresConfig *config.FeaturesConfig) {
	if featuresConfig == nil {
		return
	}
	c.UpdateStatsConfig(&featuresConfig.Stats)
	c.UpdateS2TConfig(&featuresConfig.S2T)
	c.UpdateCmdbarConfig(&featuresConfig.Cmdbar)

	c.mu.Lock()
	if c.config != nil {
		c.config.Features = *featuresConfig
		// 重建特殊模式注册表，使 special_modes 配置变更热生效（码表懒加载，仅首次激活时加载）。
		c.specialModeReg = newSpecialModeRegistry(c.config.Features.SpecialModes, c.schemasDirs(), c.logger)
	}
	c.mu.Unlock()

	c.logger.Debug("Features config updated", "specialModes", len(featuresConfig.SpecialModes))
}

// UpdateCmdbarConfig 更新命令直通车配置（热更新；v1 起 cmdbar 前缀属 features 节）。
func (c *Coordinator) UpdateCmdbarConfig(cmdbarConfig *config.CmdbarConfig) {
	if cmdbarConfig == nil {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.config != nil {
		c.config.Features.Cmdbar = *cmdbarConfig
	}
	if c.uiManager != nil {
		c.uiManager.SetCmdbarCandidatePrefix(cmdbarConfig.CandidatePrefix)
	}
	c.logger.Debug("Cmdbar config updated")
}

// updateThemeStyle handles theme style and theme name changes
func (c *Coordinator) updateThemeStyle(uiConfig *config.UIConfig) {
	themeStyle := uiConfig.Theme.Style
	if themeStyle == "" {
		themeStyle = config.ThemeStyleSystem
	}

	// Determine dark mode
	isDark := false
	switch themeStyle {
	case config.ThemeStyleDark:
		isDark = true
	case config.ThemeStyleLight:
		isDark = false
	default: // system
		isDark = theme.IsSystemDarkMode()
	}

	// Update dark mode state
	c.uiManager.SetDarkMode(isDark)

	// Load the theme (always reload to pick up new dark mode state)
	if uiConfig.Theme.Name != "" {
		c.uiManager.LoadTheme(uiConfig.Theme.Name)
		c.notifyThemeFallbackIfAny()
	} else {
		c.uiManager.ReapplyTheme()
	}

	// Start/stop watcher based on style
	if themeStyle == config.ThemeStyleSystem {
		c.startDarkModeWatcher()
	} else {
		c.stopDarkModeWatcher()
	}
}

// SetCfgMu 注入与 rpc.Server 共享的配置锁，替换构造时创建的本地锁。
// 必须在启动 Coordinator 事件循环前调用（main.go 注入时机）。
func (c *Coordinator) SetCfgMu(mu *sync.RWMutex) {
	c.cfgMu = mu
}

// ClearInputState 清空输入状态（供外部调用）
func (c *Coordinator) ClearInputState() {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.clearState()
	c.hideUI()
	c.logger.Debug("Input state cleared")
}
