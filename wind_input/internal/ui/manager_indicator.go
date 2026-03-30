package ui

import (
	"time"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// ShowModeIndicator shows a brief mode indicator (中/En) (async, non-blocking)
func (m *Manager) ShowModeIndicator(mode string, x, y int) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	m.logger.Debug("Queuing ShowModeIndicator", "mode", mode)

	// Send command to UI thread (non-blocking)
	select {
	case m.cmdCh <- UICommand{
		Type:     "mode",
		ModeText: mode,
		X:        x,
		Y:        y,
	}:
		// Signal the event to wake up the message loop
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping mode command")
	}
}

// doShowModeIndicator actually shows the mode indicator (called from UI thread)
func (m *Manager) doShowModeIndicator(mode string, x, y int) {
	// Increment version to cancel any pending hide timers
	m.mu.Lock()
	m.modeIndicatorVersion++
	currentVersion := m.modeIndicatorVersion
	duration := m.statusIndicatorDuration
	offsetX := m.statusIndicatorOffsetX
	offsetY := m.statusIndicatorOffsetY
	m.mu.Unlock()

	// Apply offset to position
	adjustedX := x + offsetX
	adjustedY := y + offsetY

	// Render mode indicator
	img := m.renderer.RenderModeIndicator(mode)

	// Check if host rendering is active (Band window proxy)
	m.mu.Lock()
	hostRender := m.hostRenderFunc
	m.mu.Unlock()

	if hostRender != nil {
		// Send mode indicator bitmap to DLL via shared memory
		if err := hostRender(img, adjustedX, adjustedY); err != nil {
			m.logger.Error("Host render mode indicator failed", "error", err)
		}
		// Hide local window if it was visible
		if m.window.IsVisible() {
			m.window.Hide()
		}
	} else {
		// Clear candidate hit test data to prevent mouse interactions
		// from triggering old candidate window display.
		// The mode indicator and candidate window share the same window,
		// so stale hitRects would cause RefreshCandidates on mouse hover.
		m.window.SetHitRects(nil)
		m.window.SetPageRects(nil, nil)
		m.window.ResetMouseTracking()

		// Update window
		if err := m.window.UpdateContent(img, adjustedX, adjustedY); err != nil {
			m.logger.Error("UpdateContent failed", "error", err)
			return
		}

		// Show window briefly
		m.window.Show()
	}

	// Hide after delay, but only if version hasn't changed
	// This ensures that rapid state changes reset the timer
	go func() {
		time.Sleep(time.Duration(duration) * time.Millisecond)

		// Check if version is still the same
		m.mu.Lock()
		versionNow := m.modeIndicatorVersion
		m.mu.Unlock()

		if versionNow == currentVersion {
			// Version unchanged, safe to hide
			m.Hide() // Use public method which goes through channel
		}
		// If version changed, another indicator was shown, so don't hide
	}()
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
