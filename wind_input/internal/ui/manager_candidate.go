//go:build windows

package ui

import (
	"image/color"

	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/uicmd"
	"github.com/huanfeng/wind_input/pkg/config"
)

// buildHostHitRects converts a render result's panel-local hit geometry into the
// neutral wire form embedded in host-render shared memory. Page buttons are encoded
// as Index -1 (page up) / -2 (page down), matching the darwin CmdCandidateRects
// convention the DLL hit-tests against. Rect coordinates share the bitmap origin
// (shadow margin included), so they map 1:1 onto the host window's client space.
func buildHostHitRects(rr *RenderResult) []ipc.CandidateHitRect {
	if rr == nil {
		return nil
	}
	rects := make([]ipc.CandidateHitRect, 0, len(rr.Rects)+2)
	for _, r := range rr.Rects {
		rects = append(rects, ipc.CandidateHitRect{
			Index: int32(r.Index),
			X:     int32(r.X),
			Y:     int32(r.Y),
			W:     int32(r.W),
			H:     int32(r.H),
		})
	}
	if rr.PageUpRect != nil {
		rects = append(rects, ipc.CandidateHitRect{
			Index: -1, X: int32(rr.PageUpRect.X), Y: int32(rr.PageUpRect.Y),
			W: int32(rr.PageUpRect.W), H: int32(rr.PageUpRect.H),
		})
	}
	if rr.PageDownRect != nil {
		rects = append(rects, ipc.CandidateHitRect{
			Index: -2, X: int32(rr.PageDownRect.X), Y: int32(rr.PageDownRect.Y),
			W: int32(rr.PageDownRect.W), H: int32(rr.PageDownRect.H),
		})
	}
	return rects
}

// hostRenderedHover encodes which element this frame actually highlights, for the SHM
// header's RenderedHoverIndex. The DLL syncs its hover-dedup baseline to it so re-hovering
// the same candidate after a content change re-highlights. Encoding matches the DLL's hover
// convention: >=0 candidate, -1 none, -2 page-up, -3 page-down (NOT the rect -1/-2 convention).
func hostRenderedHover(hoverIndex int, hoverPageBtn string) int {
	switch hoverPageBtn {
	case "up":
		return -2
	case "down":
		return -3
	}
	if hoverIndex >= 0 {
		return hoverIndex
	}
	return -1
}

// ShowCandidates shows candidates at the given caret position (async, non-blocking)
// The position will be automatically adjusted to stay within screen bounds.
// Parameters:
//   - caretX, caretY: the caret position (where input is happening)
//   - caretHeight: height of the caret/cursor
//   - totalCandidateCount: total number of candidates across all pages
//   - candidatesPerPage: number of candidates per page
func (m *Manager) ShowCandidates(candidates []Candidate, input string, cursorPos, caretX, caretY, caretHeight, page, totalPages, totalCandidateCount, candidatesPerPage, selectedIndex int) error {
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
	m.selectedIndex = selectedIndex
	m.caretX = caretX
	m.caretY = caretY
	m.caretHeight = caretHeight
	// Capture current input session for this show command
	currentSession := m.inputSession
	m.mu.Unlock()

	m.logger.Debug("Queuing ShowCandidates", "input", input, "cursorPos", cursorPos, "count", len(candidates), "caretX", caretX, "caretY", caretY, "caretHeight", caretHeight, "selectedIndex", selectedIndex, "session", currentSession)

	// Send command to UI thread (non-blocking due to buffered channel)
	item := uicmdItem{
		Cmd: uicmd.NewCommand(uicmd.CmdCandidatesShow, currentSession, uicmd.CandidatesShowPayload{
			Candidates:          toUICandidates(candidates),
			Input:               input,
			CursorPos:           cursorPos,
			CaretX:              caretX,
			CaretY:              caretY,
			CaretHeight:         caretHeight,
			Page:                page,
			TotalPages:          totalPages,
			TotalCandidateCount: totalCandidateCount,
			CandidatesPerPage:   candidatesPerPage,
			SelectedIndex:       selectedIndex,
		}),
		Candidates: candidates,
	}
	select {
	case m.cmdCh <- item:
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
func (m *Manager) doShowCandidates(candidates []Candidate, input string, cursorPos, caretX, caretY, caretHeight, page, totalPages, totalCandidateCount, candidatesPerPage, selectedIndex int) {
	// Debug: skip rendering if hide_candidate_window is enabled
	if m.hideCandidateWindow {
		m.logger.Debug("doShowCandidates skipped (hide_candidate_window enabled)")
		return
	}

	m.logger.Debug("doShowCandidates start", "input", input, "count", len(candidates), "caretX", caretX, "caretY", caretY, "caretHeight", caretHeight)

	// 候选文本 rune 数截断：超过 maxCandidateChars 时追加"…"
	m.mu.Lock()
	maxChars := m.maxCandidateChars
	m.mu.Unlock()
	if maxChars > 0 {
		for i := range candidates {
			runes := []rune(candidates[i].Text)
			if len(runes) > maxChars {
				candidates[i].Text = string(runes[:maxChars]) + "…"
			}
		}
	}

	// Cancel any pending mode indicator hide timer
	// (mode indicator's goroutine checks modeIndicatorVersion before calling Hide)
	m.mu.Lock()
	m.modeIndicatorVersion++
	m.mu.Unlock()

	// Check if this is a new input session (input is shorter than before or empty)
	// If so, reset the sticky state, drag pinned state, and hover index
	m.mu.Lock()
	prevInput := m.input
	if len(input) < len(prevInput) || input == "" {
		m.stickyAbove = false
		m.window.ResetDragPinned()
		m.window.ResetHoverIndex()
		m.logger.Debug("Reset sticky state", "prevInput", prevInput, "newInput", input)
	}
	// Reset mouse tracking only when candidate content actually changes
	// (not during hover refreshes which have the same input and page)
	if input != m.lastRenderedInput || page != m.lastRenderedPage {
		m.window.ResetMouseTracking()
		// host render 模式下同步清除 host 悬停态：内容变化后新帧不应带上一次的高亮
		// （镜像本地 ResetMouseTracking 清 hoverIndex）。DLL 侧也会在几何变化时吸收
		// 内容更新引发的合成鼠标移动，二者共同消除"打字时光标恰在候选上"的高亮/tooltip 抖动。
		m.hostHoverIndex = -1
		m.hostHoverPageBtn = ""
		m.lastRenderedInput = input
		m.lastRenderedPage = page
	}
	currentStickyAbove := m.stickyAbove
	modeLabel := m.modeLabel
	modeAccentColor := m.modeAccentColor
	// Get current hover index and page button hover for rendering
	hoverIndex := m.window.GetHoverIndex()
	hoverPageBtn := m.window.GetHoverPageBtn()
	// host render 模式下鼠标在 DLL 处理，本地 window 的 hoverIndex 恒为 -1；改用
	// coordinator 经 SetHostHoverIndex 同步过来的 host 悬停索引来渲染高亮。
	// 注意：此处 m.mu 已由本函数上方持有（紧接着的 Unlock 释放），直接读字段，
	// 切勿再次 m.mu.Lock()（非重入互斥锁会自我死锁，卡死整个 UI 线程）。
	if m.hostRenderFunc != nil {
		hoverIndex = m.hostHoverIndex
		hoverPageBtn = m.hostHoverPageBtn
	}
	m.mu.Unlock()

	// Set mode label and accent color on renderer before rendering
	if m.renderer != nil {
		m.renderer.SetModeLabel(modeLabel)
		m.renderer.SetModeAccentColor(modeAccentColor)
	}

	// Update effective DPI based on caret position before rendering.
	// This ensures correct DPI when the caret is on a different monitor,
	// even before WM_DPICHANGED is received by our windows.
	UpdateEffectiveDPIFromPoint(caretX, caretY)

	// 注入当前「是否在光标上方」状态，供 build 函数在 flip 模式下反转 bands 排列。
	// 使用 currentStickyAbove（本帧渲染前捕获）：已在上方的帧直接以正确方向渲染；
	// 刚切换到上方的首帧在下方位置计算后再做一次二次渲染（见下方 showAbove 逻辑）。
	if m.renderer != nil {
		m.renderer.SetIsAbove(currentStickyAbove)
	}

	// Render first to get actual window size (with hover highlight)
	m.logger.Debug("Rendering candidates...", "hoverIndex", hoverIndex, "hoverPageBtn", hoverPageBtn, "selectedIndex", selectedIndex)
	img, renderResult := m.renderer.RenderCandidates(candidates, input, cursorPos, page, totalPages, hoverIndex, hoverPageBtn, selectedIndex)
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

	// 设置当前页各候选的 Shadow 修改标记和命令标记
	hasShadowFlags := make([]bool, len(candidates))
	candidateTexts := make([]string, len(candidates))
	isCommandFlags := make([]bool, len(candidates))
	isGroupMemberFlags := make([]bool, len(candidates))
	isPhraseFlags := make([]bool, len(candidates))
	isUserDictFlags := make([]bool, len(candidates))
	isTempDictFlags := make([]bool, len(candidates))
	for i, c := range candidates {
		hasShadowFlags[i] = c.HasShadow
		candidateTexts[i] = c.Text
		isCommandFlags[i] = c.IsCommand
		isGroupMemberFlags[i] = c.IsGroupMember
		isPhraseFlags[i] = c.IsPhrase
		isUserDictFlags[i] = c.Meta.IsUserDict
		isTempDictFlags[i] = c.Meta.IsTempDict
	}
	m.window.SetCandidateHasShadow(hasShadowFlags)
	m.window.SetCandidateMenuState(candidateTexts, m.isPinyinMode, isCommandFlags, isGroupMemberFlags, isPhraseFlags, isUserDictFlags, isTempDictFlags)
	m.window.SetQuickInputMode(m.isQuickInputMode)

	// 位置决策优先级：
	//   1. 「固定候选位置」规则（per-app 持久化，按 caret 所在显示器查表，clamp 到工作区）
	//   2. 会话内 drag pin（用户当前会话拖动后）
	//   3. caret 自动定位
	var windowX, windowY int
	if pinX, pinY, ok := m.resolveAppPinnedPosition(caretX, caretY, windowWidth, windowHeight); ok {
		windowX, windowY = pinX, pinY
		m.logger.Debug("Position pinned by app rule", "windowX", windowX, "windowY", windowY)
	} else if m.window.IsDragPinned() {
		windowX, windowY = m.window.GetPosition()
		m.logger.Debug("Position pinned by drag", "windowX", windowX, "windowY", windowY)
	} else {
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
		if m.renderer != nil && m.renderer.GetLayout() == config.LayoutHorizontal {
			layout = LayoutHorizontal
		}
		// 阴影画布四向扩展了 shadowMargin 像素，定位应以内容尺寸（不含阴影）为基准，
		// 再把画布整体左移/上移 margin，使内容区域精确对准光标。
		sml, smt, smr, smb := 0, 0, 0, 0
		if renderResult != nil {
			sml = renderResult.ShadowMarginLeft
			smt = renderResult.ShadowMarginTop
			smr = renderResult.ShadowMarginRight
			smb = renderResult.ShadowMarginBottom
		}
		contentW := windowWidth - sml - smr
		contentH := windowHeight - smt - smb
		if contentW < 1 {
			contentW = 1
		}
		if contentH < 1 {
			contentH = 1
		}
		var showAbove bool
		windowX, windowY, showAbove = AdjustCandidatePosition(caretX, caretY, caretHeight, contentW, contentH, layout, preference)
		windowX -= sml
		windowY -= smt
		m.logger.Debug("Position adjusted", "windowX", windowX, "windowY", windowY, "showAbove", showAbove, "stickyAbove", currentStickyAbove)

		// Update sticky state if we're now showing above
		if showAbove && !currentStickyAbove {
			m.mu.Lock()
			m.stickyAbove = true
			m.mu.Unlock()
			m.logger.Debug("Set sticky state to above")

			// 首次切换到上方：若 flip 模式开启，以正确方向做一次二次渲染。
			// 尺寸不变（bands 内容相同，仅顺序不同），位置无需重算，仅更新 img 和 hitRects。
			if m.renderer != nil && m.renderer.config.FlipWhenAbove {
				m.renderer.SetIsAbove(true)
				img, renderResult = m.renderer.RenderCandidates(candidates, input, cursorPos, page, totalPages, hoverIndex, hoverPageBtn, selectedIndex)
				m.logger.Debug("Re-rendered with flipped layout (above)")
				// 同步更新 hit rects（候选项位置已随 flip 变化）
				if renderResult != nil {
					m.window.SetHitRects(renderResult.Rects)
					m.window.SetPageRects(renderResult.PageUpRect, renderResult.PageDownRect)
				}
			}
		}
	}

	// Check if host rendering is active (Band window proxy)
	m.mu.Lock()
	hostRender := m.hostRenderFunc
	m.mu.Unlock()

	if hostRender != nil {
		// Send bitmap to DLL via shared memory for host window rendering, embedding
		// the panel-local hit rects so the host window can route mouse clicks/hover
		// back to Go. Rects share the bitmap's coordinate origin (shadow margin
		// included), matching the local-window hit-test basis 1:1.
		hostRects := buildHostHitRects(renderResult)
		// Tell the DLL which element this frame highlights so it can keep its hover-dedup
		// baseline in sync (hoverIndex/hoverPageBtn are the values used for this render).
		renderedHover := hostRenderedHover(hoverIndex, hoverPageBtn)
		m.logger.Debug("Host rendering: sending bitmap to shared memory...", "rects", len(hostRects), "renderedHover", renderedHover)
		if err := hostRender(img, windowX, windowY, hostRects, renderedHover); err != nil {
			// Host render failed (e.g., shared memory closed after process restart).
			// Clear the stale function so subsequent calls don't keep failing,
			// and fall through to the local window path as a fallback.
			m.logger.Error("Host render failed, clearing stale func", "error", err)
			m.mu.Lock()
			m.hostRenderFunc = nil
			m.hostHideFunc = nil
			m.hostVisible = false
			m.mu.Unlock()
			// Fall through to local window rendering below
		} else {
			// Hide local window if it was visible (mode switch from local to host)
			if m.window.IsVisible() {
				m.window.Hide()
			}
			// Mark the host candidate as visible. This mirrors the local window's
			// IsVisible() (which stays false in host mode) so RefreshCandidates and
			// other "is a candidate currently shown?" checks behave identically to
			// non-host mode — critical so a stray hover(-1) after Hide() doesn't
			// re-show the just-hidden candidates.
			m.mu.Lock()
			m.hostVisible = true
			m.mu.Unlock()
			m.logger.Debug("doShowCandidates complete (host render)")
			return
		}
	}

	// Position stability: suppress micro-shifts (< 4px) when window is already visible.
	// Some apps (EverEdit) report slightly different caret height on the first vs
	// subsequent GetTextExt calls, causing a 1-2px vertical jump.
	actualX, actualY := windowX, windowY
	if m.window.IsVisible() {
		prevX, prevY := m.window.GetPosition()
		dx := actualX - prevX
		dy := actualY - prevY
		if dx < 0 {
			dx = -dx
		}
		if dy < 0 {
			dy = -dy
		}
		if dx < 4 && dy < 4 {
			actualX = prevX
			actualY = prevY
		}
	}

	// Update window
	m.logger.Debug("Updating window content...")
	if err := m.window.UpdateContent(img, actualX, actualY); err != nil {
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
	// 同步置 host 候选不可见（doHide 真正 WriteHide 是异步的）：一旦决定隐藏，逻辑上
	// 候选即不可见，避免随后到来的 hover(-1)→RefreshCandidates 在 doHide 执行前读到
	// 旧的 hostVisible=true 而把刚隐藏的候选又重显。镜像 Hide() 同步自增 session 的语义。
	m.hostVisible = false
	m.mu.Unlock()

	m.logger.Debug("Hide called, new session", "session", newSession)

	// Send command to UI thread (non-blocking)
	// Note: We always send hide command even if window appears hidden,
	// because the window visibility check is not thread-safe and there might
	// be pending show commands in the channel
	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdCandidatesHide, newSession, uicmd.CandidatesHidePayload{})}
	select {
	case m.cmdCh <- item:
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

	// Also hide host window if active
	m.mu.Lock()
	hostHide := m.hostHideFunc
	m.stickyAbove = false
	m.mu.Unlock()
	if hostHide != nil {
		hostHide()
	}
	m.mu.Lock()
	m.hostHoverIndex = -1   // 候选隐藏时清空 host 悬停，避免下次显示残留高亮
	m.hostHoverPageBtn = "" // 同上：清空翻页按钮悬停态
	m.hostVisible = false   // host 候选已隐藏，镜像本地 window 的 Hide
	m.mu.Unlock()
	m.window.ResetHoverIndex()
	m.window.ResetDragPinned()
	// 重置鼠标移动追踪：候选窗再次出现时，必须等用户真正挪动鼠标后才能触发 tooltip，
	// 防止"光标恰好停在新候选区域上 → 不动也立刻弹 tooltip"。
	m.window.ResetMouseTracking()
}

// UpdatePosition 投递 CmdCandidatesPosition 命令到 UI 线程更新候选框位置。
// 历史上为 sync 直接 m.window.SetPosition; 后改为 async 投递, 集中线程与跨进程兼容。
func (m *Manager) UpdatePosition(x, y int) {
	m.mu.Lock()
	m.caretX = x
	m.caretY = y
	m.mu.Unlock()

	m.postCmd(uicmd.NewCommand(uicmd.CmdCandidatesPosition, 0, uicmd.CandidatesPositionPayload{X: x, Y: y}))
}

// IsVisible returns whether the window is visible
func (m *Manager) IsVisible() bool {
	return m.window.IsVisible()
}

// RefreshCandidates re-renders the candidate window with current state
// Used to update hover highlight without changing candidate data
func (m *Manager) RefreshCandidates() {
	m.mu.Lock()
	// host render 模式下本地 window 始终隐藏（候选经 SHM 在宿主进程渲染），用 hostVisible
	// 镜像可见性：仅当本地窗口可见、或 host 渲染中且 host 候选正在显示时才刷新。否则隐藏
	// 后到来的 hover(-1) 会经 RefreshCandidates 把刚隐藏的候选又重新显示出来（ESC/选词后）。
	hostShown := m.hostRenderFunc != nil && m.hostVisible
	if !m.ready || (!m.window.IsVisible() && !hostShown) {
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
	selectedIndex := m.selectedIndex
	caretX := m.caretX
	caretY := m.caretY
	caretHeight := m.caretHeight
	currentSession := m.inputSession
	m.mu.Unlock()

	// Re-queue a show command with current data
	item := uicmdItem{
		Cmd: uicmd.NewCommand(uicmd.CmdCandidatesShow, currentSession, uicmd.CandidatesShowPayload{
			Candidates:          toUICandidates(candidates),
			Input:               input,
			CursorPos:           cursorPos,
			CaretX:              caretX,
			CaretY:              caretY,
			CaretHeight:         caretHeight,
			Page:                page,
			TotalPages:          totalPages,
			TotalCandidateCount: totalCandidateCount,
			CandidatesPerPage:   candidatesPerPage,
			SelectedIndex:       selectedIndex,
		}),
		Candidates: candidates,
	}
	select {
	case m.cmdCh <- item:
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

	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdDPIChanged, 0, uicmd.DPIChangedPayload{})}
	select {
	case m.cmdCh <- item:
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
		selectedIndex := m.selectedIndex
		caretX := m.caretX
		caretY := m.caretY
		caretHeight := m.caretHeight
		m.mu.Unlock()

		m.doShowCandidates(candidates, input, cursorPos, caretX, caretY, caretHeight, page, totalPages, totalCandidateCount, candidatesPerPage, selectedIndex)
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
	item := uicmdItem{Cmd: uicmd.NewCommand(uicmd.CmdCandidateMenuHide, 0, uicmd.CandidateMenuHidePayload{})}
	select {
	case m.cmdCh <- item:
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

// SetPinyinMode 设置是否为拼音模式（影响右键菜单前移/后移启用状态）。
// 末尾投递 CmdCandidatesMarkers 全量快照, 供跨进程同步。
func (m *Manager) SetPinyinMode(isPinyin bool) {
	m.mu.Lock()
	m.isPinyinMode = isPinyin
	m.mu.Unlock()
	m.postCmd(m.snapshotCandidatesMarkers())
}

// SetQuickInputMode 设置是否为快捷输入模式（右键菜单只保留复制）。
func (m *Manager) SetQuickInputMode(isQuickInput bool) {
	m.mu.Lock()
	m.isQuickInputMode = isQuickInput
	m.mu.Unlock()
	m.postCmd(m.snapshotCandidatesMarkers())
}

// SetModeLabel 设置临时模式标签（如"临时拼音"、"快捷输入"），空字符串表示不显示。
func (m *Manager) SetModeLabel(label string) {
	m.mu.Lock()
	m.modeLabel = label
	m.mu.Unlock()
	m.postCmd(m.snapshotCandidatesMarkers())
}

// SetModeAccentColor 设置特殊模式内发光边框颜色，nil 表示不显示。
func (m *Manager) SetModeAccentColor(c color.Color) {
	m.mu.Lock()
	m.modeAccentColor = c
	m.mu.Unlock()
	m.postCmd(m.snapshotCandidatesMarkers())
}

// resolveAppPinnedPosition 返回「固定候选位置」规则对应的候选窗左上角坐标。
// 决策三档（与用户在设计阶段确认的语义保持一致）：
//  1. 规则未启用 / 无任何记忆 → ok=false 走常规自动定位；
//  2. caret 所在显示器有记录 → 使用该记录，并 clamp 到该显示器工作区（处理分辨率变化）；
//  3. caret 所在显示器无记录：
//     a) 若任一记录仍落在某有效显示器工作区内（用户多屏轮换中、只是当前在另一屏）
//     → ok=false 走常规自动定位，避免拿别屏坐标贴到当前屏；
//     b) 否则所有记录都已"孤儿化"（保存的显示器已拔/分辨率已变）
//     → 任选一条 clamp 到 caret 所在显示器工作区，保证 pin 行为不"失效"。
func (m *Manager) resolveAppPinnedPosition(caretX, caretY, windowWidth, windowHeight int) (int, int, bool) {
	m.mu.Lock()
	enabled := m.appPinEnabled
	positions := m.appPinPositions
	m.mu.Unlock()

	if !enabled || len(positions) == 0 {
		return 0, 0, false
	}

	workLeft, workTop, workRight, workBottom := GetMonitorWorkAreaFromPoint(caretX, caretY)
	caretMonitorKey := MonitorKeyStr(workRight, workBottom)

	var x, y int
	var found bool
	if pos, ok := positions[caretMonitorKey]; ok {
		x, y, found = pos[0], pos[1], true
	} else {
		// 当前显示器无记录：检查是否存在任何"仍在有效显示器内"的旧记录
		anyOnValidMonitor := false
		var orphanX, orphanY int
		haveOrphan := false
		for _, pos := range positions {
			ml, mt, mr, mb := GetMonitorWorkAreaFromPoint(pos[0], pos[1])
			if pos[0] >= ml && pos[0] < mr && pos[1] >= mt && pos[1] < mb {
				anyOnValidMonitor = true
				break
			}
			if !haveOrphan {
				orphanX, orphanY = pos[0], pos[1]
				haveOrphan = true
			}
		}
		if anyOnValidMonitor {
			// 多屏轮换：另一屏的记录有效，不应用到当前屏，让其走常规自动定位
			return 0, 0, false
		}
		if haveOrphan {
			x, y, found = orphanX, orphanY, true
		}
	}
	if !found {
		return 0, 0, false
	}

	// Clamp 到 caret 所在显示器工作区（不回写 map，避免 clamp 后的临时安全位置污染用户原意）
	if x+windowWidth > workRight {
		x = workRight - windowWidth
	}
	if x < workLeft {
		x = workLeft
	}
	if y+windowHeight > workBottom {
		y = workBottom - windowHeight
	}
	if y < workTop {
		y = workTop
	}
	return x, y, true
}
