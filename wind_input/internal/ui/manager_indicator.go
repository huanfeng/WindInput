package ui

import (
	"time"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// ShowModeIndicator 向后兼容：单模式文本显示，内部转发到 ShowStatusIndicator
func (m *Manager) ShowModeIndicator(mode string, x, y int) {
	m.ShowStatusIndicator(StatusState{ModeLabel: mode}, x, y)
}

// ShowStatusIndicator 显示合并状态提示（异步，非阻塞）
func (m *Manager) ShowStatusIndicator(state StatusState, x, y int) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	select {
	case m.cmdCh <- UICommand{
		Type:        "status",
		StatusState: &state,
		X:           x,
		Y:           y,
	}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping status command")
	}
}

// HideStatusIndicator 隐藏状态提示窗口（异步）
func (m *Manager) HideStatusIndicator() {
	select {
	case m.cmdCh <- UICommand{Type: "status_hide"}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
	}
}

// doShowModeIndicator 向后兼容：转发到 doShowStatus
func (m *Manager) doShowModeIndicator(mode string, x, y int) {
	m.doShowStatus(StatusState{ModeLabel: mode}, x, y)
}

// doShowStatus 在 UI 线程中显示状态提示
func (m *Manager) doShowStatus(state StatusState, x, y int) {
	if m.status == nil {
		return
	}

	cfg := m.status.GetConfig()
	if !cfg.Enabled {
		return
	}

	// 计算位置
	var finalX, finalY int
	if cfg.PositionMode == StatusPositionCustom {
		finalX = cfg.CustomX
		finalY = cfg.CustomY
	} else {
		finalX = x + cfg.OffsetX
		finalY = y + cfg.OffsetY
	}

	// 临时模式下重置拖动位置
	if cfg.DisplayMode == StatusDisplayModeTemp {
		m.status.ResetDragPosition()
	}

	// Host render 路径
	m.status.mu.Lock()
	hostRender := m.status.hostRenderFunc
	m.status.mu.Unlock()

	if hostRender != nil {
		// 先更新状态以便宿主渲染
		m.status.mu.Lock()
		m.status.state = state
		m.status.mu.Unlock()

		if err := hostRender(finalX, finalY); err != nil {
			m.logger.Error("Host render status indicator failed", "error", err)
		}
		if m.status.IsVisible() {
			m.status.Hide()
		}
	} else {
		// 更新内部状态并显示
		m.status.mu.Lock()
		m.status.state = state
		m.status.mu.Unlock()

		m.status.Show(finalX, finalY)
	}

	// 临时模式下启动自动隐藏
	if cfg.DisplayMode == StatusDisplayModeTemp {
		m.status.scheduleHide()
	}
}

// doHideStatus 在 UI 线程中隐藏状态提示
func (m *Manager) doHideStatus() {
	if m.status == nil {
		return
	}
	m.status.mu.Lock()
	hostHide := m.status.hostHideFunc
	m.status.mu.Unlock()
	if hostHide != nil {
		hostHide()
	}
	m.status.Hide()
}

// ShowTooltipForCandidate shows a tooltip for the candidate at the given page-local index
// TODO: 反查功能待实现 - 需要以下数据支持：
//  1. 拼音反查：根据汉字查询拼音（需要拼音字典数据）
//  2. 五笔编码反查：显示汉字的完整五笔编码（需要编码反查表）
//  3. 可选：五笔拆字方法展示
//
// 目前 candidate.Comment 字段为空，因为引擎未返回 Hint 信息
func (m *Manager) ShowTooltipForCandidate(pageIndex int, tooltipX, tooltipY int) {
	m.mu.Lock()
	if !m.ready || m.tooltip == nil {
		m.mu.Unlock()
		return
	}

	// Get the candidate at the page-local index
	if pageIndex < 0 || pageIndex >= len(m.candidates) {
		m.mu.Unlock()
		m.HideTooltip()
		return
	}

	candidate := m.candidates[pageIndex]
	comment := candidate.Comment
	delay := m.tooltipDelay

	// Increment version to cancel any pending tooltip show
	m.tooltipVersion++
	version := m.tooltipVersion
	m.mu.Unlock()

	// Hide any currently visible tooltip immediately when switching candidates
	m.tooltip.Hide()

	// Only show tooltip if there's a comment (encoding info)
	if comment == "" {
		return
	}

	// Show tooltip with delay
	if delay <= 0 {
		m.tooltip.Show(comment, tooltipX, tooltipY)
		return
	}
	go func() {
		time.Sleep(time.Duration(delay) * time.Millisecond)
		m.mu.Lock()
		if m.tooltipVersion != version {
			m.mu.Unlock()
			return // Cancelled: hover changed before delay elapsed
		}
		m.mu.Unlock()
		m.tooltip.Show(comment, tooltipX, tooltipY)
	}()
}

// HideTooltip hides the tooltip and cancels any pending delayed show
func (m *Manager) HideTooltip() {
	m.mu.Lock()
	m.tooltipVersion++
	m.mu.Unlock()
	if m.tooltip != nil {
		m.tooltip.Hide()
	}
}

// LoadTheme loads a theme by name and applies it to all renderers
func (m *Manager) LoadTheme(themeName string) error {
	if m.themeManager == nil {
		return nil
	}

	// Load the theme
	if err := m.themeManager.LoadTheme(themeName); err != nil {
		m.logger.Warn("Failed to load theme, using default", "theme", themeName, "error", err)
	}

	// Apply theme to all renderers
	resolved := m.themeManager.GetResolvedTheme()
	m.applyTheme(resolved)

	// Refresh candidate window if it's currently visible
	if m.window != nil && m.window.IsVisible() {
		m.RefreshCandidates()
	}

	m.logger.Info("Theme loaded", "theme", themeName)
	return nil
}

// applyTheme applies the resolved theme to all UI components
func (m *Manager) applyTheme(resolved *theme.ResolvedTheme) {
	if resolved == nil {
		return
	}

	// Apply to candidate window renderer
	if m.renderer != nil {
		m.renderer.SetTheme(resolved)
	}

	// Apply to toolbar (this also handles popup menu in toolbar)
	if m.toolbar != nil {
		m.toolbar.SetTheme(resolved)
	}

	// Apply to popup menus via candidate window
	if m.window != nil {
		m.window.SetTheme(resolved)
	}

	// Apply to tooltip
	if m.tooltip != nil {
		m.tooltip.SetTheme(resolved)
	}

	// Apply to unified popup menu
	if m.unifiedPopupMenu != nil {
		m.unifiedPopupMenu.SetTheme(resolved)
	}

	// 应用到状态窗口
	if m.status != nil {
		m.status.SetTheme(resolved)
	}
}

// SetDarkMode sets the dark mode state on the theme manager
func (m *Manager) SetDarkMode(isDark bool) {
	if m.themeManager != nil {
		m.themeManager.SetDarkMode(isDark)
	}
}

// ReapplyTheme re-resolves and applies the current theme (e.g., after dark mode change)
func (m *Manager) ReapplyTheme() {
	if m.themeManager == nil {
		return
	}

	resolved := m.themeManager.GetResolvedTheme()
	m.applyTheme(resolved)

	// Refresh candidate window if it's currently visible
	if m.window != nil && m.window.IsVisible() {
		m.RefreshCandidates()
	}
}

// GetAvailableThemes returns a list of available theme names
func (m *Manager) GetAvailableThemes() []string {
	if m.themeManager == nil {
		return []string{"default"}
	}
	return m.themeManager.ListAvailableThemes()
}

// GetCurrentThemeName returns the name of the currently loaded theme
func (m *Manager) GetCurrentThemeName() string {
	if m.themeManager == nil {
		return "default"
	}
	t := m.themeManager.GetCurrentTheme()
	if t != nil {
		return t.Meta.Name
	}
	return "default"
}

// GetCurrentThemeID returns the ID of the currently loaded theme (e.g., "default", "msime")
func (m *Manager) GetCurrentThemeID() string {
	if m.themeManager == nil {
		return "default"
	}
	return m.themeManager.GetCurrentThemeID()
}

// GetAvailableThemeInfos returns theme display info (ID + display name) for all available themes
func (m *Manager) GetAvailableThemeInfos() []theme.ThemeDisplayInfo {
	if m.themeManager == nil {
		return []theme.ThemeDisplayInfo{
			{ID: "default", DisplayName: "默认主题"},
		}
	}
	return m.themeManager.ListAvailableThemeInfos()
}
