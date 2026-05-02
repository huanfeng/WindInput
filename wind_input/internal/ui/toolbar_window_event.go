package ui

import (
	"image"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

// createTooltipWindow creates the tooltip window
func (w *ToolbarWindow) createTooltipWindow() {
	tooltipClassName, _ := syscall.UTF16PtrFromString("IMEToolbarTooltip")

	wc := WNDCLASSEXW{
		CbSize:        uint32(unsafe.Sizeof(WNDCLASSEXW{})),
		LpfnWndProc:   syscall.NewCallback(defWndProc),
		LpszClassName: tooltipClassName,
	}

	procRegisterClassExW.Call(uintptr(unsafe.Pointer(&wc)))

	exStyle := uint32(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE)
	style := uint32(WS_POPUP)

	hwnd, _, _ := procCreateWindowExW.Call(
		uintptr(exStyle),
		uintptr(unsafe.Pointer(tooltipClassName)),
		0,
		uintptr(style),
		0, 0, 1, 1,
		0, 0, 0, 0,
	)

	if hwnd != 0 {
		w.tooltipHwnd = windows.HWND(hwnd)
		w.logger.Debug("Tooltip window created", "hwnd", hwnd)
	}
}

// defWndProc is a simple default window procedure
func defWndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	ret, _, _ := procDefWindowProcW.Call(hwnd, uintptr(msg), wParam, lParam)
	return ret
}

// getTooltipText returns the tooltip text for a button
func (w *ToolbarWindow) getTooltipText(button ToolbarHitResult) string {
	switch button {
	case HitGrip:
		return "拖动工具栏"
	case HitModeButton:
		return "切换中文/英文"
	case HitWidthButton:
		return "切换全角/半角"
	case HitPunctButton:
		return "切换中/英标点"
	case HitSettingsButton:
		return "菜单"
	default:
		return ""
	}
}

// handleMouseDown handles WM_LBUTTONDOWN
func (w *ToolbarWindow) handleMouseDown(hwnd uintptr, lParam uintptr) uintptr {
	// Hide context menu first if it's open
	if w.popupMenu != nil && w.popupMenu.IsVisible() {
		w.popupMenu.Hide()
	}

	x := int(int16(lParam & 0xFFFF))
	y := int(int16((lParam >> 16) & 0xFFFF))

	hit := w.renderer.HitTest(x, y, w.width, w.height)

	w.logger.Debug("Toolbar mouse down", "x", x, "y", y, "hit", hit)

	// Hide tooltip and cancel timer when mouse is pressed
	procKillTimer.Call(hwnd, TOOLTIP_TIMER_ID)
	w.hideTooltip()

	w.mu.Lock()
	w.dragStartX = x
	w.dragStartY = y

	if hit == HitGrip {
		// Start dragging immediately for grip
		w.dragging = true
		w.dragOffsetX = x
		w.dragOffsetY = y
		w.mu.Unlock()

		// Capture mouse
		procSetCapture.Call(hwnd)
	} else {
		// For buttons, we track the start position but don't start dragging yet
		w.dragging = false
		w.mu.Unlock()

		// Capture mouse to ensure we get the mouse up event
		procSetCapture.Call(hwnd)
	}

	return 0
}

// handleMouseUp handles WM_LBUTTONUP
func (w *ToolbarWindow) handleMouseUp(hwnd uintptr, lParam uintptr) uintptr {
	x := int(int16(lParam & 0xFFFF))
	y := int(int16((lParam >> 16) & 0xFFFF))

	w.mu.Lock()
	wasDragging := w.dragging
	startX := w.dragStartX
	startY := w.dragStartY
	w.dragging = false
	w.mu.Unlock()

	// Release capture
	procReleaseCapture.Call()

	if wasDragging {
		// Save position
		if w.callback != nil && w.callback.OnPositionChanged != nil {
			w.callback.OnPositionChanged(w.x, w.y)
		}
		return 0
	}

	// Handle button click - only if released in the same area as pressed
	hitUp := w.renderer.HitTest(x, y, w.width, w.height)
	hitDown := w.renderer.HitTest(startX, startY, w.width, w.height)

	w.logger.Debug("Toolbar mouse up", "x", x, "y", y, "hitUp", hitUp, "hitDown", hitDown)

	// Only trigger click if pressed and released on the same button
	if hitUp == hitDown && w.callback != nil {
		switch hitUp {
		case HitModeButton:
			w.logger.Info("Mode button clicked")
			if w.callback.OnToggleMode != nil {
				w.callback.OnToggleMode()
			}
		case HitWidthButton:
			w.logger.Info("Width button clicked")
			if w.callback.OnToggleWidth != nil {
				w.callback.OnToggleWidth()
			}
		case HitPunctButton:
			w.logger.Info("Punct button clicked")
			if w.callback.OnTogglePunct != nil {
				w.callback.OnTogglePunct()
			}
		case HitSettingsButton:
			w.logger.Info("Settings button clicked - showing unified menu")
			if w.callback.OnShowMenu != nil {
				// Calculate menu position: below the settings button
				bx, _, bw, _ := w.renderer.GetButtonBounds(HitSettingsButton)
				scale := GetDPIScale()
				gap := int(4 * scale)
				w.mu.Lock()
				menuX := w.x + bx + bw/2
				menuY := w.y + w.height + gap
				flipRefY := w.y - gap // 如果下方放不下，翻转到工具栏上方
				w.mu.Unlock()
				w.callback.OnShowMenu(menuX, menuY, flipRefY)
			}
		}
	}

	return 0
}

// handleRightClick handles WM_RBUTTONUP to show unified context menu
func (w *ToolbarWindow) handleRightClick(hwnd uintptr, lParam uintptr) uintptr {
	w.logger.Debug("Toolbar right click - showing unified menu")

	// Hide tooltip
	w.hideTooltip()

	if w.callback != nil && w.callback.OnShowMenu != nil {
		// Show menu at same position as settings button (below toolbar center, with flip support)
		bx, _, bw, _ := w.renderer.GetButtonBounds(HitSettingsButton)
		scale := GetDPIScale()
		gap := int(4 * scale)
		w.mu.Lock()
		menuX := w.x + bx + bw/2
		menuY := w.y + w.height + gap
		flipRefY := w.y - gap // 如果下方放不下，翻转到工具栏上方
		w.mu.Unlock()
		w.callback.OnShowMenu(menuX, menuY, flipRefY)
	}

	return 0
}

// handleMouseMove handles WM_MOUSEMOVE (legacy, kept for compatibility)
func (w *ToolbarWindow) handleMouseMove(hwnd uintptr, lParam uintptr) uintptr {
	w.mu.Lock()
	if !w.dragging {
		w.mu.Unlock()
		return 0
	}

	// Get current cursor position in screen coordinates
	var pt POINT
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))

	// Calculate new window position
	newX := int(pt.X) - w.dragOffsetX
	newY := int(pt.Y) - w.dragOffsetY

	// Clamp to the work area of the monitor under the cursor, preventing the
	// toolbar from being dragged off-screen or behind the taskbar.
	monLeft, monTop, monRight, monBottom := GetMonitorWorkAreaFromPoint(int(pt.X), int(pt.Y))
	if newX < monLeft {
		newX = monLeft
	}
	if newX+w.width > monRight {
		newX = monRight - w.width
	}
	if newY < monTop {
		newY = monTop
	}
	if newY+w.height > monBottom {
		newY = monBottom - w.height
	}

	w.x = newX
	w.y = newY
	w.mu.Unlock()

	// Move the window
	procSetWindowPos.Call(
		uintptr(w.hwnd),
		HWND_TOPMOST,
		uintptr(newX), uintptr(newY),
		0, 0,
		SWP_NOSIZE|SWP_NOACTIVATE,
	)

	return 0
}

// handleMouseMoveWithTooltip handles WM_MOUSEMOVE with tooltip support
func (w *ToolbarWindow) handleMouseMoveWithTooltip(hwnd uintptr, lParam uintptr) uintptr {
	x := int(int16(lParam & 0xFFFF))
	y := int(int16((lParam >> 16) & 0xFFFF))

	// Check if dragging
	w.mu.Lock()
	if w.dragging {
		w.mu.Unlock()
		return w.handleMouseMove(hwnd, lParam)
	}

	// Enable mouse leave tracking if not already enabled
	if !w.trackingMouse {
		tme := TRACKMOUSEEVENT{
			CbSize:    uint32(unsafe.Sizeof(TRACKMOUSEEVENT{})),
			DwFlags:   TME_LEAVE,
			HwndTrack: hwnd,
		}
		procTrackMouseEvent.Call(uintptr(unsafe.Pointer(&tme)))
		w.trackingMouse = true
	}

	// Check which button is hovered
	newHover := w.renderer.HitTest(x, y, w.width, w.height)
	oldHover := w.hoverButton

	if newHover != oldHover {
		w.hoverButton = newHover
		w.mu.Unlock()

		// Cancel any pending tooltip timer
		procKillTimer.Call(hwnd, TOOLTIP_TIMER_ID)

		// Hide current tooltip if shown
		if w.tooltipVisible {
			w.hideTooltip()
		}

		// Start new tooltip timer if hovering a button
		if newHover != HitNone {
			procSetTimer.Call(hwnd, TOOLTIP_TIMER_ID, TOOLTIP_DELAY_MS, 0)
		}
	} else {
		w.mu.Unlock()
	}

	return 0
}

// handleMouseLeave handles WM_MOUSELEAVE
func (w *ToolbarWindow) handleMouseLeave(hwnd uintptr) uintptr {
	w.mu.Lock()
	w.trackingMouse = false
	w.hoverButton = HitNone
	w.mu.Unlock()

	// Cancel tooltip timer and hide tooltip
	procKillTimer.Call(uintptr(hwnd), TOOLTIP_TIMER_ID)
	w.hideTooltip()

	return 0
}

// handleTimer handles WM_TIMER for tooltip
func (w *ToolbarWindow) handleTimer(hwnd uintptr, wParam uintptr) uintptr {
	if wParam == TOOLTIP_TIMER_ID {
		procKillTimer.Call(hwnd, TOOLTIP_TIMER_ID)

		w.mu.Lock()
		button := w.hoverButton
		w.mu.Unlock()

		if button != HitNone {
			w.showTooltip(button)
		}
	}
	return 0
}

// showTooltip shows the tooltip for a button
func (w *ToolbarWindow) showTooltip(button ToolbarHitResult) {
	if w.tooltipHwnd == 0 {
		return
	}

	text := w.getTooltipText(button)
	if text == "" {
		return
	}

	// Render tooltip
	img := w.renderer.RenderTooltip(text)
	if img == nil {
		return
	}

	// Get button bounds in screen coordinates
	bx, _, bw, _ := w.renderer.GetButtonBounds(button)

	w.mu.Lock()
	screenX := w.x + bx + bw/2 - img.Bounds().Dx()/2
	screenY := w.y + w.height + 4 // Below toolbar
	w.mu.Unlock()

	// Update layered window
	w.updateTooltipLayeredWindow(img, screenX, screenY)

	// Show tooltip
	procShowWindow.Call(uintptr(w.tooltipHwnd), SW_SHOW)
	w.tooltipVisible = true
}

// hideTooltip hides the tooltip
func (w *ToolbarWindow) hideTooltip() {
	if w.tooltipHwnd != 0 && w.tooltipVisible {
		procShowWindow.Call(uintptr(w.tooltipHwnd), SW_HIDE)
		w.tooltipVisible = false
	}
}

// updateTooltipLayeredWindow updates the tooltip layered window
func (w *ToolbarWindow) updateTooltipLayeredWindow(img *image.RGBA, x, y int) {
	bounds := img.Bounds()
	width := bounds.Dx()
	height := bounds.Dy()

	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen == 0 {
		return
	}
	defer procReleaseDC.Call(0, hdcScreen)

	hdcMem, _, _ := procCreateCompatibleDC.Call(hdcScreen)
	if hdcMem == 0 {
		return
	}
	defer procDeleteDC.Call(hdcMem)

	bi := BITMAPINFO{
		BmiHeader: BITMAPINFOHEADER{
			BiSize:        uint32(unsafe.Sizeof(BITMAPINFOHEADER{})),
			BiWidth:       int32(width),
			BiHeight:      -int32(height),
			BiPlanes:      1,
			BiBitCount:    32,
			BiCompression: BI_RGB,
		},
	}

	var bits unsafe.Pointer
	hBitmap, _, _ := procCreateDIBSection.Call(
		hdcMem,
		uintptr(unsafe.Pointer(&bi)),
		DIB_RGB_COLORS,
		uintptr(unsafe.Pointer(&bits)),
		0, 0,
	)
	if hBitmap == 0 {
		return
	}
	defer procDeleteObject.Call(hBitmap)

	procSelectObject.Call(hdcMem, hBitmap)

	// Copy image data (RGBA to BGRA channel swap).
	// image.RGBA is already premultiplied alpha, matching UpdateLayeredWindow's expectation.
	pixelCount := width * height
	dstSlice := unsafe.Slice((*byte)(bits), pixelCount*4)

	for i := 0; i < pixelCount; i++ {
		srcIdx := i * 4
		dstIdx := i * 4

		dstSlice[dstIdx+0] = img.Pix[srcIdx+2] // B
		dstSlice[dstIdx+1] = img.Pix[srcIdx+1] // G
		dstSlice[dstIdx+2] = img.Pix[srcIdx+0] // R
		dstSlice[dstIdx+3] = img.Pix[srcIdx+3] // A
	}

	ptSrc := POINT{X: 0, Y: 0}
	ptDst := POINT{X: int32(x), Y: int32(y)}
	size := SIZE{Cx: int32(width), Cy: int32(height)}
	blend := BLENDFUNCTION{
		BlendOp:             AC_SRC_OVER,
		BlendFlags:          0,
		SourceConstantAlpha: 255,
		AlphaFormat:         AC_SRC_ALPHA,
	}

	procUpdateLayeredWindow.Call(
		uintptr(w.tooltipHwnd),
		hdcScreen,
		uintptr(unsafe.Pointer(&ptDst)),
		uintptr(unsafe.Pointer(&size)),
		hdcMem,
		uintptr(unsafe.Pointer(&ptSrc)),
		0,
		uintptr(unsafe.Pointer(&blend)),
		ULW_ALPHA,
	)
}

// handleSetCursor handles WM_SETCURSOR - sets appropriate cursor based on mouse position
func (w *ToolbarWindow) handleSetCursor(hwnd uintptr, lParam uintptr) uintptr {
	// Get current cursor position
	var pt POINT
	procGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))

	// Convert to client coordinates
	procScreenToClient.Call(hwnd, uintptr(unsafe.Pointer(&pt)))

	x := int(pt.X)
	y := int(pt.Y)

	// Determine which cursor to show based on hit test
	hit := w.renderer.HitTest(x, y, w.width, w.height)

	var cursorID uintptr
	switch hit {
	case HitGrip:
		// Grip area - show move cursor
		cursorID = IDC_SIZEALL
	case HitModeButton, HitWidthButton, HitPunctButton, HitSettingsButton:
		// Button area - show hand cursor
		cursorID = IDC_HAND
	default:
		// Default - show arrow cursor
		cursorID = IDC_ARROW
	}

	// Load and set cursor
	cursor, _, _ := procLoadCursorW.Call(0, cursorID)
	if cursor != 0 {
		procSetCursor.Call(cursor)
	}

	return 1 // Return TRUE to indicate we handled the message
}
