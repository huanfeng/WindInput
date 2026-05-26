package ui

import "github.com/huanfeng/wind_input/internal/uicmd"

// SetToolbarVisible shows or hides the toolbar
func (m *Manager) SetToolbarVisible(visible bool) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	if !visible {
		item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdToolbarHide, 0, uicmd.ToolbarHidePayload{})}
		select {
		case m.cmdCh <- item:
			if m.cmdEvent != 0 {
				SetEvent(m.cmdEvent)
			}
		default:
			m.logger.Warn("UI command channel full, dropping toolbar hide command")
		}
	}
	// For showing toolbar, use ShowToolbarWithState instead
}

// ShowToolbarWithState shows the toolbar with position and state in one atomic operation
func (m *Manager) ShowToolbarWithState(x, y int, state ToolbarState) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdToolbarShow, 0, uicmd.ToolbarShowPayload{
		X:     x,
		Y:     y,
		State: toUIToolbarState(state),
	})}
	select {
	case m.cmdCh <- item:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping toolbar show command")
	}
}

// UpdateToolbarState updates the toolbar state
func (m *Manager) UpdateToolbarState(state ToolbarState) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdToolbarUpdate, 0, uicmd.ToolbarUpdatePayload{
		State: toUIToolbarState(state),
	})}
	select {
	case m.cmdCh <- item:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping toolbar update command")
	}
}

// SetToolbarPosition sets the toolbar position
func (m *Manager) SetToolbarPosition(x, y int) {
	if m.toolbar != nil {
		m.toolbar.SetPosition(x, y)
		// Re-render to update layered window with new position
		m.toolbar.Render()
	}
}

// GetToolbarPosition returns the current toolbar position
func (m *Manager) GetToolbarPosition() (int, int) {
	if m.toolbar != nil {
		return m.toolbar.GetPosition()
	}
	return 0, 0
}

// doShowToolbar shows the toolbar with optional position and state (called from UI thread)
func (m *Manager) doShowToolbar(x, y int, state ToolbarState) {
	if m.toolbar == nil {
		m.logger.Warn("doShowToolbar: toolbar is nil")
		return
	}

	m.logger.Debug("doShowToolbar called", "x", x, "y", y)

	// Set position if provided (0,0 视为未指定, 沿用原语义)
	if x != 0 || y != 0 {
		m.toolbar.SetPosition(x, y)
		m.logger.Debug("Toolbar position set", "x", x, "y", y)
	}

	m.logger.Debug("Toolbar state set",
		"chineseMode", state.ChineseMode,
		"fullWidth", state.FullWidth,
		"chinesePunct", state.ChinesePunct,
		"capsLock", state.CapsLock)
	m.toolbar.SetState(state)

	m.toolbar.Show()
	m.logger.Debug("Toolbar shown", "x", x, "y", y)
}

// doHideToolbar hides the toolbar (called from UI thread)
func (m *Manager) doHideToolbar() {
	if m.toolbar != nil {
		m.toolbar.Hide()
		m.logger.Debug("Toolbar hidden")
	} else {
		m.logger.Warn("doHideToolbar: toolbar is nil")
	}
}

// doUpdateToolbar updates the toolbar state (called from UI thread)
func (m *Manager) doUpdateToolbar(state *ToolbarState) {
	if m.toolbar != nil && state != nil {
		m.logger.Debug("doUpdateToolbar",
			"chineseMode", state.ChineseMode,
			"fullWidth", state.FullWidth,
			"chinesePunct", state.ChinesePunct,
			"capsLock", state.CapsLock)
		m.toolbar.SetState(*state)
	} else {
		m.logger.Warn("doUpdateToolbar: toolbar or state is nil",
			"toolbarNil", m.toolbar == nil,
			"stateNil", state == nil)
	}
}

// IsToolbarMenuOpen returns whether the toolbar's context menu is open
func (m *Manager) IsToolbarMenuOpen() bool {
	if m.toolbar != nil {
		return m.toolbar.IsMenuOpen()
	}
	return false
}

// HideToolbarMenu hides the toolbar's context menu if it's open (async, thread-safe)
func (m *Manager) HideToolbarMenu() {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	// Send command to UI thread (don't call HideMenu directly - it has Win32 calls)
	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdToolbarMenuHide, 0, uicmd.ToolbarMenuHidePayload{})}
	select {
	case m.cmdCh <- item:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping hide_toolbar_menu command")
	}
}

// doHideToolbarMenu actually hides the menu (called from UI thread)
func (m *Manager) doHideToolbarMenu() {
	if m.toolbar != nil {
		m.toolbar.HideMenu()
	}
}

// ToolbarMenuContainsPoint checks if the given screen coordinates are within the toolbar menu
func (m *Manager) ToolbarMenuContainsPoint(screenX, screenY int) bool {
	if m.toolbar != nil {
		return m.toolbar.MenuContainsPoint(screenX, screenY)
	}
	return false
}

// ShowUnifiedMenu shows the unified right-click menu at the specified position (async, thread-safe).
//
// 跨进程兼容设计: 投递时 uicmd.MenuShowPayload.Items 暂时留空, Win 端通过 uicmdItem.MenuState
// 旁路字段直接消费 UnifiedMenuState; macOS forwarder 接入时再补转换填充 Items, 让 IMKit 端
// 渲染原生 NSMenu。Callback 通过 SessionID 路由替代 (PR-1 AGENTS.md 已说明)。
func (m *Manager) ShowUnifiedMenu(screenX, screenY, flipRefY int, state UnifiedMenuState, callback func(id int)) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	item := uicmdItem{
		Cmd: uicmd.NewCommand(uicmd.CmdMenuShow, 0, uicmd.MenuShowPayload{
			ScreenX:  screenX,
			ScreenY:  screenY,
			FlipRefY: flipRefY,
			// Items: 留待 macOS forwarder 转换填充
		}),
		MenuState: &state,
		Callback:  callback,
	}
	select {
	case m.cmdCh <- item:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping show_unified_menu command")
	}
}

// doShowUnifiedMenuFromPayload shows the unified menu (called from UI thread).
// 接受平台无关的 payload 与旁路 MenuState/Callback, 走 Win 端 BuildUnifiedMenuItems 路径。
func (m *Manager) doShowUnifiedMenuFromPayload(p uicmd.MenuShowPayload, menuState *UnifiedMenuState, callback func(id int)) {
	if m.unifiedPopupMenu == nil || menuState == nil {
		return
	}

	// Hide any existing toolbar/candidate menus first
	m.doHideCandidateMenu()
	m.doHideToolbarMenu()

	// Set flip reference Y for screen edge handling
	if p.FlipRefY > 0 {
		m.unifiedPopupMenu.SetFlipRefY(p.FlipRefY)
	}

	items := BuildUnifiedMenuItems(*menuState)
	m.unifiedPopupMenu.Show(items, p.ScreenX, p.ScreenY, func(id int) {
		if callback != nil {
			callback(id)
		}
	})
}

// IsUnifiedMenuOpen returns whether the unified popup menu is open
func (m *Manager) IsUnifiedMenuOpen() bool {
	if m.unifiedPopupMenu != nil {
		return m.unifiedPopupMenu.IsVisible()
	}
	return false
}

// HideUnifiedMenu hides the unified popup menu (for use from non-UI threads)
func (m *Manager) HideUnifiedMenu() {
	// The unified menu auto-hides on click-outside, but this can be called to force hide
	if m.unifiedPopupMenu != nil {
		m.unifiedPopupMenu.Hide()
	}
}
