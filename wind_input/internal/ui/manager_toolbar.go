package ui

// SetToolbarVisible shows or hides the toolbar
func (m *Manager) SetToolbarVisible(visible bool) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	if !visible {
		select {
		case m.cmdCh <- UICommand{Type: cmdToolbarHide}:
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

	select {
	case m.cmdCh <- UICommand{
		Type:         cmdToolbarShow,
		ToolbarX:     x,
		ToolbarY:     y,
		ToolbarState: &state,
	}:
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

	select {
	case m.cmdCh <- UICommand{Type: cmdToolbarUpdate, ToolbarState: &state}:
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
func (m *Manager) doShowToolbar(cmd UICommand) {
	if m.toolbar == nil {
		m.logger.Warn("doShowToolbar: toolbar is nil")
		return
	}

	m.logger.Debug("doShowToolbar called",
		"x", cmd.ToolbarX,
		"y", cmd.ToolbarY,
		"hasState", cmd.ToolbarState != nil)

	// Set position if provided
	if cmd.ToolbarX != 0 || cmd.ToolbarY != 0 {
		m.toolbar.SetPosition(cmd.ToolbarX, cmd.ToolbarY)
		m.logger.Debug("Toolbar position set", "x", cmd.ToolbarX, "y", cmd.ToolbarY)
	}

	// Set state if provided (before rendering)
	if cmd.ToolbarState != nil {
		m.logger.Debug("Toolbar state set",
			"chineseMode", cmd.ToolbarState.ChineseMode,
			"fullWidth", cmd.ToolbarState.FullWidth,
			"chinesePunct", cmd.ToolbarState.ChinesePunct,
			"capsLock", cmd.ToolbarState.CapsLock)
		m.toolbar.SetState(*cmd.ToolbarState)
	} else {
		// Just render with current state
		m.toolbar.Render()
	}

	m.toolbar.Show()
	m.logger.Debug("Toolbar shown", "x", cmd.ToolbarX, "y", cmd.ToolbarY)
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
	select {
	case m.cmdCh <- UICommand{Type: cmdHideToolbarMenu}:
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

// ShowUnifiedMenu shows the unified right-click menu at the specified position (async, thread-safe)
func (m *Manager) ShowUnifiedMenu(screenX, screenY, flipRefY int, state UnifiedMenuState, callback func(id int)) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	select {
	case m.cmdCh <- UICommand{
		Type:         cmdShowUnifiedMenu,
		X:            screenX,
		Y:            screenY,
		FlipRefY:     flipRefY,
		MenuState:    &state,
		MenuCallback: callback,
	}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping show_unified_menu command")
	}
}

// doShowUnifiedMenu shows the unified menu (called from UI thread)
func (m *Manager) doShowUnifiedMenu(cmd UICommand) {
	if m.unifiedPopupMenu == nil || cmd.MenuState == nil {
		return
	}

	// Hide any existing toolbar/candidate menus first
	m.doHideCandidateMenu()
	m.doHideToolbarMenu()

	// Set flip reference Y for screen edge handling
	if cmd.FlipRefY > 0 {
		m.unifiedPopupMenu.SetFlipRefY(cmd.FlipRefY)
	}

	items := BuildUnifiedMenuItems(*cmd.MenuState)
	m.unifiedPopupMenu.Show(items, cmd.X, cmd.Y, func(id int) {
		if cmd.MenuCallback != nil {
			cmd.MenuCallback(id)
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
