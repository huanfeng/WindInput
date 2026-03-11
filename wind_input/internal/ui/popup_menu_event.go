package ui

import (
	"unsafe"
)

// handleMouseMove handles mouse move events
func (m *PopupMenu) handleMouseMove(lParam uintptr) {
	mouseX := int(int16(lParam & 0xFFFF))
	mouseY := int(int16((lParam >> 16) & 0xFFFF))

	m.mu.Lock()
	menuWidth := m.width
	menuHeight := m.height
	menuX := m.x
	menuY := m.y
	sub := m.submenu
	oldHover := m.hoverIndex

	// Check if mouse is in submenu area (for event forwarding)
	insideParent := mouseX >= 0 && mouseX < menuWidth && mouseY >= 0 && mouseY < menuHeight
	m.mu.Unlock()

	// If submenu is open and mouse is in submenu area, forward to submenu
	// This takes priority even when the submenu overlaps with the parent menu
	if sub != nil {
		screenX := menuX + mouseX
		screenY := menuY + mouseY
		if m.forwardMouseMoveToSubmenu(screenX, screenY) {
			// Mouse is in submenu - keep parent hover on submenu item, don't change
			return
		}
	}

	m.mu.Lock()
	// Only show hover if mouse is actually inside the menu bounds
	if insideParent {
		m.hoverIndex = m.hitTest(mouseY)
	} else {
		m.hoverIndex = -1
	}

	newHover := m.hoverIndex

	if newHover != oldHover {
		// Check if the new hover item has children
		hasChildren := false
		if newHover >= 0 && newHover < len(m.items) {
			hasChildren = len(m.items[newHover].Children) > 0
		}
		submenuIdx := m.submenuIndex
		m.mu.Unlock()

		// Kill any pending submenu timer
		procKillTimer.Call(uintptr(m.hwnd), SUBMENU_TIMER_ID)

		if hasChildren {
			// Start submenu delay timer
			procSetTimer.Call(uintptr(m.hwnd), SUBMENU_TIMER_ID, SUBMENU_DELAY_MS, 0)
		} else if submenuIdx >= 0 && newHover != submenuIdx {
			// Before closing submenu, check if mouse is in the submenu tree
			if newHover == -1 {
				// Mouse is outside parent menu - convert to screen coords and check submenu
				screenX := menuX + mouseX
				screenY := menuY + mouseY
				if !m.isPointInMenuTree(screenX, screenY) {
					m.hideSubmenu()
				}
				// else: mouse is in submenu area, keep submenu open
			} else {
				// Moved to a different menu item - close submenu
				m.hideSubmenu()
			}
		}

		// Re-render with new hover state
		m.updateWindow()
		m.trackMouseLeave()
	} else {
		m.mu.Unlock()
	}
}

// forwardMouseMoveToSubmenu forwards a mouse move event to the submenu if the screen
// coordinates are inside it. Returns true if forwarded.
func (m *PopupMenu) forwardMouseMoveToSubmenu(screenX, screenY int) bool {
	m.mu.Lock()
	sub := m.submenu
	m.mu.Unlock()

	if sub == nil {
		return false
	}

	sub.mu.Lock()
	sx, sy, sw, sh := sub.x, sub.y, sub.width, sub.height
	subVisible := sub.visible
	sub.mu.Unlock()

	if !subVisible || screenX < sx || screenX >= sx+sw || screenY < sy || screenY >= sy+sh {
		return false
	}

	// Convert to client coordinates relative to submenu
	clientX := screenX - sx
	clientY := screenY - sy
	newLParam := uintptr(uint16(clientX)) | (uintptr(uint16(clientY)) << 16)
	sub.handleMouseMove(newLParam)
	return true
}

// forwardClickToSubmenu forwards a click event to the submenu if the screen
// coordinates are inside it. Returns true if forwarded.
func (m *PopupMenu) forwardClickToSubmenu(screenX, screenY int) bool {
	m.mu.Lock()
	sub := m.submenu
	m.mu.Unlock()

	if sub == nil {
		return false
	}

	sub.mu.Lock()
	sx, sy, sw, sh := sub.x, sub.y, sub.width, sub.height
	subVisible := sub.visible
	sub.mu.Unlock()

	if !subVisible || screenX < sx || screenX >= sx+sw || screenY < sy || screenY >= sy+sh {
		return false
	}

	// Convert to client coordinates relative to submenu
	clientX := screenX - sx
	clientY := screenY - sy
	newLParam := uintptr(uint16(clientX)) | (uintptr(uint16(clientY)) << 16)
	sub.handleClick(newLParam)
	return true
}

// handleMouseLeave handles mouse leave events
func (m *PopupMenu) handleMouseLeave() {
	// Use GetCursorPos to check actual cursor position
	// This handles the case where events are forwarded via SetCapture from parent menu:
	// WM_MOUSELEAVE fires because Windows doesn't think the cursor is over this window,
	// but the cursor is actually in our bounds (events are forwarded from parent).
	var pt struct{ X, Y int32 }
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))
	screenX := int(pt.X)
	screenY := int(pt.Y)

	m.mu.Lock()
	x, y, w, h := m.x, m.y, m.width, m.height
	submenuIdx := m.submenuIndex
	m.mu.Unlock()

	// If cursor is still inside this menu, don't clear hover
	if screenX >= x && screenX < x+w && screenY >= y && screenY < y+h {
		return
	}

	// If submenu is open, check if mouse is still in the menu tree
	if submenuIdx >= 0 {
		if m.isPointInMenuTree(screenX, screenY) {
			return // Mouse is in submenu area, don't clear hover
		}
	}

	m.mu.Lock()
	if m.hoverIndex != -1 {
		m.hoverIndex = -1
		m.mu.Unlock()
		m.updateWindow()
	} else {
		m.mu.Unlock()
	}
}

// handleClick handles mouse click events
func (m *PopupMenu) handleClick(lParam uintptr) {
	// Extract mouse position (can be outside window when using SetCapture)
	mouseX := int(int16(lParam & 0xFFFF))
	mouseY := int(int16((lParam >> 16) & 0xFFFF))

	m.mu.Lock()
	menuWidth := m.width
	menuHeight := m.height
	menuX := m.x
	menuY := m.y
	m.mu.Unlock()

	// If submenu is open, check if click is in submenu area first
	// This takes priority even when the submenu overlaps with the parent menu
	screenX := menuX + mouseX
	screenY := menuY + mouseY
	if m.forwardClickToSubmenu(screenX, screenY) {
		return
	}

	// Check if click is outside the menu bounds
	if mouseX < 0 || mouseX >= menuWidth || mouseY < 0 || mouseY >= menuHeight {
		// Click outside menu tree - hide everything
		m.Hide()
		return
	}

	m.mu.Lock()
	index := m.hitTest(mouseY)

	if index >= 0 && index < len(m.items) {
		item := m.items[index]
		if !item.Disabled && !item.Separator {
			// If item has children, show submenu instead of triggering callback
			if len(item.Children) > 0 {
				m.mu.Unlock()
				m.showSubmenu(index)
				return
			}

			callback := m.callback
			id := item.ID
			m.mu.Unlock()

			// Hide menu first
			m.Hide()

			// Then call callback
			if callback != nil {
				callback(id)
			}
			return
		}
	}
	m.mu.Unlock()
}

// hitTest returns the item index at the given Y position
func (m *PopupMenu) hitTest(mouseY int) int {
	scale := GetDPIScale()
	itemH := int(float64(menuItemHeight) * scale)
	sepH := int(float64(menuSeparatorHeight) * scale)
	padY := int(float64(menuPaddingY) * scale)

	y := padY
	for i, item := range m.items {
		var h int
		if item.Separator {
			h = sepH
		} else {
			h = itemH
		}

		if mouseY >= y && mouseY < y+h {
			if item.Separator {
				return -1
			}
			return i
		}
		y += h
	}
	return -1
}

// showSubmenu creates and shows a submenu for the item at the given index
func (m *PopupMenu) showSubmenu(index int) {
	m.mu.Lock()
	if index < 0 || index >= len(m.items) || len(m.items[index].Children) == 0 {
		m.mu.Unlock()
		return
	}
	// Already showing this submenu
	if m.submenuIndex == index && m.submenu != nil {
		m.mu.Unlock()
		return
	}
	children := m.items[index].Children
	resolvedTheme := m.resolvedTheme
	callback := m.callback
	menuX := m.x
	menuWidth := m.width
	m.mu.Unlock()

	// Hide existing submenu if any
	m.hideSubmenu()

	// Calculate submenu position (right side of parent item)
	scale := GetDPIScale()
	itemH := int(float64(menuItemHeight) * scale)
	sepH := int(float64(menuSeparatorHeight) * scale)
	padY := int(float64(menuPaddingY) * scale)

	itemY := padY
	m.mu.Lock()
	for i, item := range m.items {
		if i == index {
			break
		}
		if item.Separator {
			itemY += sepH
		} else {
			itemY += itemH
		}
	}
	menuY := m.y
	m.mu.Unlock()

	subX := menuX + menuWidth - 2 // Slight overlap
	subY := menuY + itemY

	// Create submenu sharing parent's rendering resources
	sub := newPopupMenuShared(m)
	sub.parentMenu = m
	if resolvedTheme != nil {
		sub.resolvedTheme = resolvedTheme
	}
	if err := sub.Create(); err != nil {
		return
	}

	m.mu.Lock()
	m.submenu = sub
	m.submenuIndex = index
	m.mu.Unlock()

	// Show submenu - callback proxies to parent's callback
	sub.Show(children, subX, subY, func(id int) {
		// Propagate to root callback and hide root menu
		if callback != nil {
			callback(id)
		}
	})

	// Re-render parent to show highlight on submenu item
	m.updateWindow()
}

// hideSubmenu hides and cleans up the current submenu
func (m *PopupMenu) hideSubmenu() {
	m.mu.Lock()
	sub := m.submenu
	m.submenu = nil
	m.submenuIndex = -1
	m.mu.Unlock()

	if sub != nil {
		sub.Hide()
		sub.Destroy()
	}
}

// isPointInSubmenu checks if coordinates (relative to parent menu window) are inside the submenu
func (m *PopupMenu) isPointInSubmenu(clientX, clientY int) bool {
	m.mu.Lock()
	sub := m.submenu
	menuX := m.x
	menuY := m.y
	m.mu.Unlock()

	if sub == nil {
		return false
	}

	// Convert to screen coordinates
	screenX := menuX + clientX
	screenY := menuY + clientY

	return sub.isPointInMenuTree(screenX, screenY)
}

// isPointInMenuTree checks if screen coordinates are in this menu or any of its submenus
func (m *PopupMenu) isPointInMenuTree(screenX, screenY int) bool {
	m.mu.Lock()
	x, y, w, h := m.x, m.y, m.width, m.height
	visible := m.visible
	sub := m.submenu
	m.mu.Unlock()

	if !visible {
		return false
	}

	if screenX >= x && screenX < x+w && screenY >= y && screenY < y+h {
		return true
	}

	if sub != nil {
		return sub.isPointInMenuTree(screenX, screenY)
	}

	return false
}

// ContainsPoint checks if the given screen coordinates are within the menu tree
func (m *PopupMenu) ContainsPoint(screenX, screenY int) bool {
	return m.isPointInMenuTree(screenX, screenY)
}

// GetBounds returns the menu bounds (x, y, width, height)
func (m *PopupMenu) GetBounds() (int, int, int, int) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.x, m.y, m.width, m.height
}

// checkMouseState checks if mouse button is pressed outside the menu tree
// This is a backup mechanism for cross-process click detection where SetCapture doesn't work
func (m *PopupMenu) checkMouseState() {
	if !m.IsVisible() {
		return
	}

	// Check if left mouse button is pressed
	state, _, _ := procGetAsyncKeyState.Call(VK_LBUTTON)
	// GetAsyncKeyState returns: high-order bit set if key is down
	if state&0x8000 == 0 {
		return // Mouse button not pressed
	}

	// Get current cursor position (screen coordinates)
	var pt struct{ X, Y int32 }
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))

	// Check if cursor is inside the menu tree (including submenus)
	if !m.isPointInMenuTree(int(pt.X), int(pt.Y)) {
		// Mouse pressed outside menu tree - close it
		m.Hide()
	}
}
