package ui

import (
	"unsafe"

	"github.com/huanfeng/wind_input/pkg/buildvariant"
)

// handleMouseMove processes mouse move events
func (w *CandidateWindow) handleMouseMove(lParam uintptr) {
	// Extract mouse position from lParam (relative to window client area)
	mouseX := int(int16(lParam & 0xFFFF))
	mouseY := int(int16((lParam >> 16) & 0xFFFF))

	// Enable mouse leave tracking if not already tracking
	w.mu.Lock()
	if !w.trackingMouse {
		tme := TRACKMOUSEEVENT{
			CbSize:    uint32(unsafe.Sizeof(TRACKMOUSEEVENT{})),
			DwFlags:   TME_LEAVE,
			HwndTrack: uintptr(w.hwnd),
		}
		procTrackMouseEvent.Call(uintptr(unsafe.Pointer(&tme)))
		w.trackingMouse = true
	}

	// If dragging, handle drag move and skip hover logic
	if w.dragging {
		w.mu.Unlock()
		w.handleDragMove(lParam)
		return
	}

	// Detect real mouse movement: the first WM_MOUSEMOVE after content update
	// only stores the position; subsequent moves with different coordinates
	// confirm that the user is actually moving the mouse.
	if w.hasLastMousePos {
		if mouseX != w.lastMouseX || mouseY != w.lastMouseY {
			w.mouseHasMoved = true
		}
	}
	w.lastMouseX = mouseX
	w.lastMouseY = mouseY
	w.hasLastMousePos = true

	hitRects := w.hitRects
	pageUpRect := w.pageUpRect
	pageDownRect := w.pageDownRect
	prevHoverIndex := w.hoverIndex
	prevHoverPageBtn := w.hoverPageBtn
	callbacks := w.callbacks
	mouseHasMoved := w.mouseHasMoved
	windowX := w.x
	windowY := w.y
	w.mu.Unlock()

	// Only process hover when the mouse has truly moved,
	// preventing tooltip flicker when the cursor is stationary
	// but candidates change underneath it during typing.
	if !mouseHasMoved {
		return
	}

	mx := float64(mouseX)
	my := float64(mouseY)

	// Hit test against candidate rectangles
	newHoverIndex := -1
	for _, rect := range hitRects {
		if mx >= rect.X && mx <= rect.X+rect.W &&
			my >= rect.Y && my <= rect.Y+rect.H {
			newHoverIndex = rect.Index
			break
		}
	}

	// Hit test against page buttons (only if not hovering a candidate)
	newHoverPageBtn := ""
	if newHoverIndex < 0 {
		if pageUpRect != nil && mx >= pageUpRect.X && mx <= pageUpRect.X+pageUpRect.W &&
			my >= pageUpRect.Y && my <= pageUpRect.Y+pageUpRect.H {
			newHoverPageBtn = "up"
		} else if pageDownRect != nil && mx >= pageDownRect.X && mx <= pageDownRect.X+pageDownRect.W &&
			my >= pageDownRect.Y && my <= pageDownRect.Y+pageDownRect.H {
			newHoverPageBtn = "down"
		}
	}

	// Update hover state if changed
	if newHoverIndex != prevHoverIndex || newHoverPageBtn != prevHoverPageBtn {
		w.mu.Lock()
		w.hoverIndex = newHoverIndex
		w.hoverPageBtn = newHoverPageBtn
		w.mu.Unlock()

		// Calculate tooltip position: centered below the candidate item
		tooltipX := windowX
		tooltipY := windowY
		if newHoverIndex >= 0 {
			for _, rect := range hitRects {
				if rect.Index == newHoverIndex {
					tooltipX = windowX + int(rect.X+rect.W/2)
					tooltipY = windowY + int(rect.Y+rect.H) + 2
					break
				}
			}
		}

		// Notify callback with tooltip position
		if callbacks != nil && callbacks.OnHoverChange != nil {
			callbacks.OnHoverChange(newHoverIndex, tooltipX, tooltipY)
		}
	}
}

// handleMouseClick processes left mouse button click
func (w *CandidateWindow) handleMouseClick(lParam uintptr) {
	// Extract mouse position
	mouseX := int(int16(lParam & 0xFFFF))
	mouseY := int(int16((lParam >> 16) & 0xFFFF))

	w.mu.Lock()
	hitRects := w.hitRects
	pageUpRect := w.pageUpRect
	pageDownRect := w.pageDownRect
	callbacks := w.callbacks
	w.mu.Unlock()

	mx := float64(mouseX)
	my := float64(mouseY)

	// Check page up button first
	if pageUpRect != nil && mx >= pageUpRect.X && mx <= pageUpRect.X+pageUpRect.W &&
		my >= pageUpRect.Y && my <= pageUpRect.Y+pageUpRect.H {
		if callbacks != nil && callbacks.OnPageUp != nil {
			callbacks.OnPageUp()
		}
		return
	}

	// Check page down button
	if pageDownRect != nil && mx >= pageDownRect.X && mx <= pageDownRect.X+pageDownRect.W &&
		my >= pageDownRect.Y && my <= pageDownRect.Y+pageDownRect.H {
		if callbacks != nil && callbacks.OnPageDown != nil {
			callbacks.OnPageDown()
		}
		return
	}

	// Hit test against candidate rectangles
	for _, rect := range hitRects {
		if mx >= rect.X && mx <= rect.X+rect.W &&
			my >= rect.Y && my <= rect.Y+rect.H {
			// Found a hit - notify callback
			if callbacks != nil && callbacks.OnSelect != nil {
				callbacks.OnSelect(rect.Index)
			}
			return
		}
	}

	// No hit on any interactive element — start dragging
	w.handleDragStart(lParam)
}

// isDragging returns whether the window is currently being dragged
func (w *CandidateWindow) isDragging() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.dragging
}

// IsDragPinned returns whether the window position is pinned by user drag
func (w *CandidateWindow) IsDragPinned() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.dragPinned
}

// ResetDragPinned resets the drag pinned state
func (w *CandidateWindow) ResetDragPinned() {
	w.mu.Lock()
	w.dragPinned = false
	w.mu.Unlock()
}

// handleDragStart begins dragging the candidate window
func (w *CandidateWindow) handleDragStart(lParam uintptr) {
	clientX := int(int16(lParam & 0xFFFF))
	clientY := int(int16((lParam >> 16) & 0xFFFF))

	w.mu.Lock()
	w.dragging = true
	w.dragStartX = clientX
	w.dragStartY = clientY
	w.mu.Unlock()

	// Capture mouse to track movement outside the window
	procSetCapture.Call(uintptr(w.hwnd))
}

// handleDragMove moves the window during a drag operation
func (w *CandidateWindow) handleDragMove(lParam uintptr) {
	clientX := int(int16(lParam & 0xFFFF))
	clientY := int(int16((lParam >> 16) & 0xFFFF))

	w.mu.Lock()
	dx := clientX - w.dragStartX
	dy := clientY - w.dragStartY
	newX := w.x + dx
	newY := w.y + dy
	w.x = newX
	w.y = newY
	w.mu.Unlock()

	procSetWindowPos.Call(
		uintptr(w.hwnd),
		HWND_TOPMOST,
		uintptr(newX), uintptr(newY),
		0, 0,
		SWP_NOSIZE|SWP_NOACTIVATE,
	)
}

// handleDragEnd finishes dragging and pins the position
func (w *CandidateWindow) handleDragEnd() {
	w.mu.Lock()
	wasDragging := w.dragging
	w.dragging = false
	w.mu.Unlock()

	if wasDragging {
		procReleaseCapture.Call()

		w.mu.Lock()
		w.dragPinned = true
		w.mu.Unlock()

		w.logger.Debug("候选框拖动完成，位置已锁定")
	}
}

// handleRightClick processes right mouse button click
func (w *CandidateWindow) handleRightClick(lParam uintptr) {
	// Extract mouse position (relative to window)
	mouseX := int(int16(lParam & 0xFFFF))
	mouseY := int(int16((lParam >> 16) & 0xFFFF))

	w.mu.Lock()
	hitRects := w.hitRects
	windowX := w.x
	windowY := w.y
	popupMenu := w.popupMenu
	pageStartIndex := w.pageStartIndex
	totalCandidateCount := w.totalCandidateCount
	hasShadowFlags := w.hasShadowFlags
	w.mu.Unlock()

	// Hit test against candidate rectangles
	var hitIndex int = -1
	for _, rect := range hitRects {
		if float64(mouseX) >= rect.X && float64(mouseX) <= rect.X+rect.W &&
			float64(mouseY) >= rect.Y && float64(mouseY) <= rect.Y+rect.H {
			hitIndex = rect.Index
			break
		}
	}

	// Check if popup menu is available
	if popupMenu == nil {
		w.logger.Warn("Popup menu not available")
		return
	}

	// Calculate screen position
	screenX := windowX + mouseX
	screenY := windowY + mouseY

	if hitIndex < 0 {
		// Right-clicked on blank area — show unified menu via callback
		w.mu.Lock()
		cb := w.callbacks
		w.mu.Unlock()

		if cb != nil && cb.OnShowUnifiedMenu != nil {
			cb.OnShowUnifiedMenu(screenX, screenY)
		}
		return
	}

	// Determine enable/disable using global candidate position (not page-local)
	globalIndex := pageStartIndex + hitIndex
	isGlobalFirst := globalIndex == 0
	isGlobalLast := totalCandidateCount <= 0 || globalIndex >= totalCandidateCount-1
	isSingleCandidate := totalCandidateCount <= 1

	// Check if this candidate has shadow modifications
	hasShadow := hitIndex >= 0 && hitIndex < len(hasShadowFlags) && hasShadowFlags[hitIndex]

	// Check if candidate is single character (cannot delete)
	isSingleChar := false
	candidateTexts := w.candidateTexts
	if hitIndex >= 0 && hitIndex < len(candidateTexts) {
		isSingleChar = len([]rune(candidateTexts[hitIndex])) <= 1
	}

	// Check if pinyin mode (disable move up/down for non-command candidates)
	isPinyin := w.isPinyinMode

	// 检查是否为命令候选（短语），命令候选在拼音模式下也允许前移/后移
	isCommand := hitIndex >= 0 && hitIndex < len(w.isCommandFlags) && w.isCommandFlags[hitIndex]

	// 菜单状态规则：
	// 前移: 首位禁用 | 单候选禁用 | 拼音非命令禁用
	// 后移: 末位禁用 | 单候选禁用 | 拼音非命令禁用
	// 置顶: 首位禁用
	// 删除: 单字禁用 | 命令候选禁用
	// 恢复默认: 无 Shadow/短语覆盖时禁用
	disableMoveUp := isGlobalFirst || isSingleCandidate || (isPinyin && !isCommand) || w.isQuickInputMode
	disableMoveDown := isGlobalLast || isSingleCandidate || (isPinyin && !isCommand) || w.isQuickInputMode
	disableTop := isGlobalFirst || w.isQuickInputMode
	disableDelete := isSingleChar || isCommand || w.isQuickInputMode

	// Build menu items
	items := []MenuItem{
		{ID: IDM_CANDIDATE_MOVEUP, Text: "前移(U)", Disabled: disableMoveUp},
		{ID: IDM_CANDIDATE_MOVEDOWN, Text: "后移(D)", Disabled: disableMoveDown},
		{ID: IDM_CANDIDATE_MOVETOP, Text: "置顶(T)", Disabled: disableTop},
		{Separator: true},
		{ID: IDM_CANDIDATE_RESET, Text: "恢复默认(R)", Disabled: !hasShadow},
		{Separator: true},
		{ID: IDM_CANDIDATE_DELETE, Text: "删除词条(X)", Disabled: disableDelete},
		{Separator: true},
		{ID: IDM_CANDIDATE_COPY, Text: "复制(C)"},
	}

	// Debug: add batch copy submenu
	if buildvariant.IsDebug() {
		items = append(items, MenuItem{
			Text: "调试复制",
			Children: []MenuItem{
				{ID: IDM_CANDIDATE_COPYALL, Text: "复制所有候选"},
				{ID: IDM_CANDIDATE_COPY1P, Text: "复制前1页候选"},
				{ID: IDM_CANDIDATE_COPY2P, Text: "复制前2页候选"},
				{ID: IDM_CANDIDATE_COPY3P, Text: "复制前3页候选"},
			},
		})
	}

	// Set menu open flag and target index
	w.mu.Lock()
	w.menuOpen = true
	w.menuTargetIndex = hitIndex
	w.mu.Unlock()

	// Show custom popup menu (doesn't steal focus)
	popupMenu.Show(items, screenX, screenY, func(id int) {
		// Handle menu selection in callback
		w.mu.Lock()
		w.menuOpen = false
		targetIndex := w.menuTargetIndex
		cb := w.callbacks
		w.mu.Unlock()

		if cb != nil {
			switch id {
			case IDM_CANDIDATE_MOVEUP:
				if cb.OnMoveUp != nil {
					cb.OnMoveUp(targetIndex)
				}
			case IDM_CANDIDATE_MOVEDOWN:
				if cb.OnMoveDown != nil {
					cb.OnMoveDown(targetIndex)
				}
			case IDM_CANDIDATE_MOVETOP:
				if cb.OnMoveTop != nil {
					cb.OnMoveTop(targetIndex)
				}
			case IDM_CANDIDATE_RESET:
				if cb.OnResetDefault != nil {
					cb.OnResetDefault(targetIndex)
				}
			case IDM_CANDIDATE_DELETE:
				if cb.OnDelete != nil {
					cb.OnDelete(targetIndex)
				}
			case IDM_CANDIDATE_COPY:
				if cb.OnCopy != nil {
					cb.OnCopy(targetIndex)
				}
			case IDM_CANDIDATE_COPYALL:
				if cb.OnCopyDebugBatch != nil {
					cb.OnCopyDebugBatch(0)
				}
			case IDM_CANDIDATE_COPY1P:
				if cb.OnCopyDebugBatch != nil {
					cb.OnCopyDebugBatch(1)
				}
			case IDM_CANDIDATE_COPY2P:
				if cb.OnCopyDebugBatch != nil {
					cb.OnCopyDebugBatch(2)
				}
			case IDM_CANDIDATE_COPY3P:
				if cb.OnCopyDebugBatch != nil {
					cb.OnCopyDebugBatch(3)
				}
			}
		}
	})

	// Note: Unlike TrackPopupMenu, our custom popup doesn't block.
	// The callback will be called when user clicks a menu item.
	// We handle ESC key and click-outside in the coordinator.
}

// handleMouseLeave processes mouse leave events
func (w *CandidateWindow) handleMouseLeave() {
	w.mu.Lock()
	prevHoverIndex := w.hoverIndex
	prevHoverPageBtn := w.hoverPageBtn
	w.hoverIndex = -1
	w.hoverPageBtn = ""
	w.trackingMouse = false
	w.mouseHasMoved = false
	w.hasLastMousePos = false
	callbacks := w.callbacks
	w.mu.Unlock()

	// Notify callback if hover state changed
	if (prevHoverIndex != -1 || prevHoverPageBtn != "") && callbacks != nil && callbacks.OnHoverChange != nil {
		callbacks.OnHoverChange(-1, 0, 0)
	}
}

// ResetMouseTracking resets mouse movement tracking state.
// Called when candidate content changes (not during hover refreshes)
// so that tooltip won't appear until the mouse has actually moved.
func (w *CandidateWindow) ResetMouseTracking() {
	w.mu.Lock()
	w.mouseHasMoved = false
	w.hasLastMousePos = false
	w.hoverIndex = -1
	w.hoverPageBtn = ""
	w.mu.Unlock()
}

// IsMenuOpen returns whether the context menu is currently open
func (w *CandidateWindow) IsMenuOpen() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.menuOpen
}

// HideMenu hides the popup menu if it's open
func (w *CandidateWindow) HideMenu() {
	w.mu.Lock()
	popupMenu := w.popupMenu
	wasOpen := w.menuOpen
	w.menuOpen = false
	w.mu.Unlock()

	if wasOpen && popupMenu != nil {
		popupMenu.Hide()
	}
}

// MenuContainsPoint checks if the given screen coordinates are within the popup menu
func (w *CandidateWindow) MenuContainsPoint(screenX, screenY int) bool {
	w.mu.Lock()
	popupMenu := w.popupMenu
	menuOpen := w.menuOpen
	w.mu.Unlock()

	if !menuOpen || popupMenu == nil {
		return false
	}
	return popupMenu.ContainsPoint(screenX, screenY)
}
