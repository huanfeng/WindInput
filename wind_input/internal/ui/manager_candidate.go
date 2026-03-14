package ui

// ShowCandidates shows candidates at the given caret position (async, non-blocking)
// The position will be automatically adjusted to stay within screen bounds.
// Parameters:
//   - caretX, caretY: the caret position (where input is happening)
//   - caretHeight: height of the caret/cursor
//   - totalCandidateCount: total number of candidates across all pages
//   - candidatesPerPage: number of candidates per page
func (m *Manager) ShowCandidates(candidates []Candidate, input string, cursorPos, caretX, caretY, caretHeight, page, totalPages, totalCandidateCount, candidatesPerPage int) error {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return nil
	}
	m.candidates = candidates
	m.input = input
	m.cursorPos = cursorPos
	m.page = page
	m.totalPages = totalPages
	m.totalCandidateCount = totalCandidateCount
	m.candidatesPerPage = candidatesPerPage
	m.caretX = caretX
	m.caretY = caretY
	m.caretHeight = caretHeight
	// Capture current input session for this show command
	currentSession := m.inputSession
	m.mu.Unlock()

	m.logger.Debug("Queuing ShowCandidates", "input", input, "cursorPos", cursorPos, "count", len(candidates), "caretX", caretX, "caretY", caretY, "caretHeight", caretHeight, "session", currentSession)

	// Send command to UI thread (non-blocking due to buffered channel)
	select {
	case m.cmdCh <- UICommand{
		Type:                "show",
		Candidates:          candidates,
		Input:               input,
		CursorPos:           cursorPos,
		X:                   caretX,
		Y:                   caretY,
		CaretHeight:         caretHeight,
		Page:                page,
		TotalPages:          totalPages,
		TotalCandidateCount: totalCandidateCount,
		CandidatesPerPage:   candidatesPerPage,
		InputSession:        currentSession,
	}:
		// Signal the event to wake up the message loop
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping show command")
	}

	return nil
}

// doShowCandidates actually shows candidates (called from UI thread)
// Parameters caretX, caretY, caretHeight are the original caret position info.
func (m *Manager) doShowCandidates(candidates []Candidate, input string, cursorPos, caretX, caretY, caretHeight, page, totalPages, totalCandidateCount, candidatesPerPage int) {
	// Debug: skip rendering if hide_candidate_window is enabled
	if m.hideCandidateWindow {
		m.logger.Debug("doShowCandidates skipped (hide_candidate_window enabled)")
		return
	}

	m.logger.Debug("doShowCandidates start", "input", input, "count", len(candidates), "caretX", caretX, "caretY", caretY, "caretHeight", caretHeight)

	// Cancel any pending mode indicator hide timer
	// (mode indicator's goroutine checks modeIndicatorVersion before calling Hide)
	m.mu.Lock()
	m.modeIndicatorVersion++
	m.mu.Unlock()

	// Check if this is a new input session (input is shorter than before or empty)
	// If so, reset the sticky state and hover index
	m.mu.Lock()
	prevInput := m.input
	if len(input) < len(prevInput) || input == "" {
		m.stickyAbove = false
		m.window.ResetHoverIndex()
		m.logger.Debug("Reset sticky state", "prevInput", prevInput, "newInput", input)
	}
	// Reset mouse tracking only when candidate content actually changes
	// (not during hover refreshes which have the same input and page)
	if input != m.lastRenderedInput || page != m.lastRenderedPage {
		m.window.ResetMouseTracking()
		m.lastRenderedInput = input
		m.lastRenderedPage = page
	}
	currentStickyAbove := m.stickyAbove
	// Get current hover index and page button hover for rendering
	hoverIndex := m.window.GetHoverIndex()
	hoverPageBtn := m.window.GetHoverPageBtn()
	m.mu.Unlock()

	// Update effective DPI based on caret position before rendering.
	// This ensures correct DPI when the caret is on a different monitor,
	// even before WM_DPICHANGED is received by our windows.
	UpdateEffectiveDPIFromPoint(caretX, caretY)

	// Render first to get actual window size (with hover highlight)
	m.logger.Debug("Rendering candidates...", "hoverIndex", hoverIndex, "hoverPageBtn", hoverPageBtn)
	img, renderResult := m.renderer.RenderCandidates(candidates, input, cursorPos, page, totalPages, hoverIndex, hoverPageBtn)
	windowWidth := img.Bounds().Dx()
	windowHeight := img.Bounds().Dy()
	m.logger.Debug("Render complete", "width", windowWidth, "height", windowHeight)

	// Update hit test rectangles for mouse interaction
	if renderResult != nil {
		m.window.SetHitRects(renderResult.Rects)
		m.window.SetPageRects(renderResult.PageUpRect, renderResult.PageDownRect)
	}

	// 设置分页信息，用于右键菜单的全局位置判断
	pageStartIndex := 0
	if candidatesPerPage > 0 {
		pageStartIndex = (page - 1) * candidatesPerPage
	}
	m.window.SetCandidatePageInfo(pageStartIndex, totalCandidateCount)

	// 设置当前页各候选的 Shadow 修改标记
	hasShadowFlags := make([]bool, len(candidates))
	for i, c := range candidates {
		hasShadowFlags[i] = c.HasShadow
	}
	m.window.SetCandidateHasShadow(hasShadowFlags)

	// Determine position preference based on sticky state
	var preference PositionPreference
	if currentStickyAbove {
		preference = PositionAbove
	} else {
		preference = PositionAuto
	}

	// Adjust position to stay within screen bounds
	// Determine layout from renderer config
	layout := LayoutVertical
	if m.renderer != nil && m.renderer.GetLayout() == "horizontal" {
		layout = LayoutHorizontal
	}
	windowX, windowY, showAbove := AdjustCandidatePosition(caretX, caretY, caretHeight, windowWidth, windowHeight, layout, preference)
	m.logger.Debug("Position adjusted", "windowX", windowX, "windowY", windowY, "showAbove", showAbove, "stickyAbove", currentStickyAbove)

	// Update sticky state if we're now showing above
	if showAbove && !currentStickyAbove {
		m.mu.Lock()
		m.stickyAbove = true
		m.mu.Unlock()
		m.logger.Debug("Set sticky state to above")
	}

	// Update window
	m.logger.Debug("Updating window content...")
	if err := m.window.UpdateContent(img, windowX, windowY); err != nil {
		m.logger.Error("UpdateContent failed", "error", err)
		return
	}
	m.logger.Debug("Window content updated")

	// Show window
	m.logger.Debug("Showing window...")
	m.window.Show()
	m.logger.Debug("doShowCandidates complete")
}

// Hide hides the candidate window (async, non-blocking)
// This also increments the input session to invalidate any pending show commands
func (m *Manager) Hide() {
	// Increment input session FIRST to invalidate any pending show commands
	// This ensures that show commands queued before this hide will be ignored
	m.mu.Lock()
	m.inputSession++
	newSession := m.inputSession
	m.mu.Unlock()

	m.logger.Debug("Hide called, new session", "session", newSession)

	// Send command to UI thread (non-blocking)
	// Note: We always send hide command even if window appears hidden,
	// because the window visibility check is not thread-safe and there might
	// be pending show commands in the channel
	select {
	case m.cmdCh <- UICommand{Type: "hide", InputSession: newSession}:
		// Signal the event to wake up the message loop
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		// Channel full, but hide is not critical - window will be hidden eventually
		m.logger.Debug("UI command channel full, skipping redundant hide")
	}
}

// doHide actually hides the window (called from UI thread)
func (m *Manager) doHide() {
	m.window.Hide()

	// Reset sticky state and hover index when hiding (input session ended)
	m.mu.Lock()
	m.stickyAbove = false
	m.mu.Unlock()
	m.window.ResetHoverIndex()
}

// UpdatePosition updates the window position
func (m *Manager) UpdatePosition(x, y int) {
	m.mu.Lock()
	m.caretX = x
	m.caretY = y
	m.mu.Unlock()

	m.window.SetPosition(x, y)
}

// IsVisible returns whether the window is visible
func (m *Manager) IsVisible() bool {
	return m.window.IsVisible()
}

// RefreshCandidates re-renders the candidate window with current state
// Used to update hover highlight without changing candidate data
func (m *Manager) RefreshCandidates() {
	m.mu.Lock()
	if !m.ready || !m.window.IsVisible() {
		m.mu.Unlock()
		return
	}
	candidates := m.candidates
	input := m.input
	cursorPos := m.cursorPos
	page := m.page
	totalPages := m.totalPages
	totalCandidateCount := m.totalCandidateCount
	candidatesPerPage := m.candidatesPerPage
	caretX := m.caretX
	caretY := m.caretY
	caretHeight := m.caretHeight
	currentSession := m.inputSession
	m.mu.Unlock()

	// Re-queue a show command with current data
	select {
	case m.cmdCh <- UICommand{
		Type:                "show",
		Candidates:          candidates,
		Input:               input,
		CursorPos:           cursorPos,
		X:                   caretX,
		Y:                   caretY,
		CaretHeight:         caretHeight,
		Page:                page,
		TotalPages:          totalPages,
		TotalCandidateCount: totalCandidateCount,
		CandidatesPerPage:   candidatesPerPage,
		InputSession:        currentSession,
	}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		// Channel full, skip refresh
	}
}

// NotifyDPIChanged notifies the manager that DPI has changed (async, thread-safe).
// This triggers re-rendering of all visible windows with the new DPI scale.
func (m *Manager) NotifyDPIChanged() {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	select {
	case m.cmdCh <- UICommand{Type: "dpi_changed"}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping dpi_changed command")
	}
}

// doDPIChanged handles DPI change: re-renders visible candidate window and toolbar (called from UI thread).
func (m *Manager) doDPIChanged() {
	m.logger.Info("DPI changed, re-rendering UI")

	// Re-render toolbar (resize + re-render)
	if m.toolbar != nil {
		m.toolbar.handleDPIChanged()
	}

	// Recalculate renderer's DPI-dependent config (font size, padding, etc.)
	if m.renderer != nil {
		m.renderer.RefreshDPIScale()
	}

	// Re-render candidate window if visible
	if m.window != nil && m.window.IsVisible() {
		m.mu.Lock()
		candidates := m.candidates
		input := m.input
		cursorPos := m.cursorPos
		page := m.page
		totalPages := m.totalPages
		totalCandidateCount := m.totalCandidateCount
		candidatesPerPage := m.candidatesPerPage
		caretX := m.caretX
		caretY := m.caretY
		caretHeight := m.caretHeight
		m.mu.Unlock()

		m.doShowCandidates(candidates, input, cursorPos, caretX, caretY, caretHeight, page, totalPages, totalCandidateCount, candidatesPerPage)
	}
}

// IsCandidateMenuOpen returns whether the candidate window's context menu is open
func (m *Manager) IsCandidateMenuOpen() bool {
	if m.window != nil {
		return m.window.IsMenuOpen()
	}
	return false
}

// HideCandidateMenu hides the candidate window's context menu if it's open (async, thread-safe)
func (m *Manager) HideCandidateMenu() {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	// Send command to UI thread (don't call HideMenu directly - it has Win32 calls)
	select {
	case m.cmdCh <- UICommand{Type: "hide_menu"}:
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping hide_menu command")
	}
}

// doHideCandidateMenu actually hides the menu (called from UI thread)
func (m *Manager) doHideCandidateMenu() {
	if m.window != nil {
		m.window.HideMenu()
	}
}

// CandidateMenuContainsPoint checks if the given screen coordinates are within the candidate menu
func (m *Manager) CandidateMenuContainsPoint(screenX, screenY int) bool {
	if m.window != nil {
		return m.window.MenuContainsPoint(screenX, screenY)
	}
	return false
}
