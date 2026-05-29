//go:build windows

package ui

import (
	"github.com/huanfeng/wind_input/internal/clipboard"
	"github.com/huanfeng/wind_input/internal/uicmd"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// ShowModeIndicator 向后兼容：单模式文本显示，内部转发到 ShowStatusIndicator
func (m *Manager) ShowModeIndicator(mode string, x, y int) {
	m.ShowStatusIndicator(StatusState{ModeLabel: mode}, x, y, 0)
}

// ShowStatusIndicator 显示合并状态提示（异步，非阻塞）。height 为 caret 高度，
// 供 darwin 端把气泡锚到 caret 底部；Win 端按原有 caret 顶部定位，忽略此参数。
func (m *Manager) ShowStatusIndicator(state StatusState, x, y, _ int) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdStatusShow, 0, uicmd.StatusShowPayload{
		State: toUIStatusState(state),
		X:     x,
		Y:     y,
	})}
	select {
	case m.cmdCh <- item:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping status command")
	}
}

// HideStatusIndicator 隐藏状态提示窗口（异步）
func (m *Manager) HideStatusIndicator() {
	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdStatusHide, 0, uicmd.StatusHidePayload{})}
	select {
	case m.cmdCh <- item:
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

// HideTooltip hides the tooltip and cancels any pending delayed show
func (m *Manager) HideTooltip() {
	m.mu.Lock()
	m.tooltipVersion++
	m.mu.Unlock()
	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdTooltipHide, 0, uicmd.TooltipHidePayload{})}
	select {
	case m.cmdCh <- item:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
	}
}

// ShowTooltipText 投递 CmdTooltipShow 命令到 UI 线程显示 tooltip 文本（无延迟）。
// belowY 为候选项下沿（首选位置），aboveY 为候选项上沿（下方放不下时使用）。
//
// 历史上为 sync 直接 m.tooltip.ForceHide+Show, 后改为 async 投递, 1 个 UI tick
// 的延迟肉眼无感, 但带来两个好处:
//   - 跨进程兼容: macOS forwarder 可消费该命令转发到 IMKit
//   - 线程隔离: tooltip.ForceHide/Show 都集中在 UI 线程执行
func (m *Manager) ShowTooltipText(text string, centerX, belowY, aboveY int) {
	if text == "" {
		return
	}
	m.mu.Lock()
	if !m.ready || m.tooltip == nil {
		m.mu.Unlock()
		return
	}
	// 取消任何待显示的延迟 tooltip（通过递增版本号）
	m.tooltipVersion++
	m.mu.Unlock()

	m.postCmd(uicmd.NewCommand(uicmd.CmdTooltipShow, 0, uicmd.TooltipShowPayload{
		Text:    text,
		CenterX: centerX,
		BelowY:  belowY,
		AboveY:  aboveY,
	}))
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
		m.applyPagerOverride()
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

	// 应用到 Toast 通知窗口
	if m.toast != nil {
		m.toast.SetTheme(resolved)
	}
}

// ShowToast 发送一次 toast 通知（异步，非阻塞）。会绕过 UI 配置（不可被禁用）。
func (m *Manager) ShowToast(opts ToastOptions) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdToastShow, 0, uicmd.ToastShowPayload{
		Title:    opts.Title,
		Message:  opts.Message,
		Level:    toUIToastLevel(opts.Level),
		Position: toUIToastPosition(opts.Position),
		Duration: int32(opts.Duration),
		MaxWidth: int32(opts.MaxWidth),
	})}
	select {
	case m.cmdCh <- item:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping toast command")
	}
}

// HideToast 立即隐藏当前 toast（异步）。
func (m *Manager) HideToast() {
	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdToastHide, 0, uicmd.ToastHidePayload{})}
	select {
	case m.cmdCh <- item:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
	}
}

// ShowToastError 快捷封装：屏幕居中 / 错误配色 / 5s 自动隐藏。
// 用于"设置打不开""引擎初始化失败"等需要立刻引起注意的场景。
func (m *Manager) ShowToastError(title, message string) {
	m.ShowToast(ToastOptions{
		Title:    title,
		Message:  message,
		Level:    ToastError,
		Position: ToastCenter,
		Duration: 5000,
	})
}

// ShowToastSuccess 快捷封装：右下角 / 成功配色 / 3.5s 自动隐藏。
// 用于"词库加载完成"等正向反馈，不打扰用户当前焦点。
func (m *Manager) ShowToastSuccess(message string) {
	m.ShowToast(ToastOptions{
		Message:  message,
		Level:    ToastSuccess,
		Position: ToastBottomRight,
		Duration: 3500,
	})
}

// doShowToast 在 UI 线程实际执行 Show（由命令分发调用）。
func (m *Manager) doShowToast(opts ToastOptions) {
	if m.toast == nil {
		return
	}
	m.toast.Show(opts)
}

// doHideToast 在 UI 线程实际执行 Hide（由命令分发调用）。
func (m *Manager) doHideToast() {
	if m.toast == nil {
		return
	}
	m.toast.Hide()
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

// showTooltipContextMenu 在指定屏幕坐标显示 tooltip 右键自定义菜单。
// 必须从 UI 线程调用（在 tooltip 的 WM_RBUTTONUP 回调中触发）。
func (m *Manager) showTooltipContextMenu(text string, x, y int) {
	if m.unifiedPopupMenu == nil {
		return
	}
	items := []MenuItem{
		{ID: 1, Text: "复制提示内容"},
	}
	// 菜单关闭后解除对 WM_MOUSELEAVE 的抑制，使 tooltip 可以正常隐藏
	tt := m.tooltip
	m.unifiedPopupMenu.SetOnHide(func() {
		if tt != nil {
			tt.SuppressLeave(false)
		}
	})
	m.unifiedPopupMenu.Show(items, x, y, func(id int) {
		if id == 1 {
			_ = clipboard.SetText(text)
		}
	})
}

// SetTooltipChaiziFont 配置 tooltip 窗口的拆字字体（用于渲染 PUA 字根字符）。
// dwFamilyName 非空时使用 DirectWrite 系统字体 fallback；否则以 FreeType 加载 fontPath 文件。
// 该方法可在任意 goroutine 调用；tooltip 未就绪时静默忽略。
func (m *Manager) SetTooltipChaiziFont(fontPath, dwFamilyName string) {
	if fontPath == "" && dwFamilyName == "" {
		return
	}
	m.mu.Lock()
	tt := m.tooltip
	m.mu.Unlock()
	if tt == nil {
		return
	}
	tt.SetChaiziFont(fontPath, dwFamilyName)
}
