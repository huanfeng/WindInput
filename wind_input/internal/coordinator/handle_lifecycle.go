// handle_lifecycle.go — IME 生命周期事件（焦点、激活、停用）与 CommitRequest 处理
package coordinator

import (
	"runtime"
	"runtime/debug"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
)

// HandleCaretUpdate handles caret position updates from C++ Bridge
func (c *Coordinator) HandleCaretUpdate(data bridge.CaretData) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.caretX = data.X
	c.caretY = data.Y
	c.caretHeight = data.Height
	c.caretValid = true // Mark that we have received valid caret position

	// Store composition start position from C++ TSF (via ITfComposition::GetRange)
	if data.CompositionStartX != 0 || data.CompositionStartY != 0 {
		c.compositionStartX = data.CompositionStartX
		c.compositionStartY = data.CompositionStartY
		c.compositionStartValid = true
	}

	c.logger.Debug("Caret position updated", "x", c.caretX, "y", c.caretY, "height", c.caretHeight,
		"compStartX", data.CompositionStartX, "compStartY", data.CompositionStartY)

	// If there's active input, refresh the candidate window position.
	// This handles the case where C++ re-sends caret update after composition
	// start/update, providing the up-to-date position.
	if len(c.inputBuffer) > 0 && len(c.candidates) > 0 && c.uiManager != nil {
		if c.pendingFirstShow {
			c.pendingFirstShow = false
			c.logger.Debug("Pending first show resolved, displaying candidate window")
		}
		c.showUI()
	}

	return nil
}

// HandleFocusLost handles focus lost events (real focus change, e.g., user clicked another window)
func (c *Coordinator) HandleFocusLost() {
	c.logger.Debug("Focus lost, clearing state and hiding toolbar")

	// 焦点变化后异步释放内存（非阻塞，不影响响应速度）
	defer func() {
		go func() {
			runtime.GC()
			debug.FreeOSMemory()
		}()
	}()

	// Hide toolbar on real focus lost (user switched to another window/app)
	c.SetIMEActivated(false)

	c.mu.Lock()
	defer c.mu.Unlock()
	c.clearState()
}

// HandleCompositionTerminated handles composition unexpectedly terminated events
// This happens when the user clicks within the input field to change cursor position,
// or when the application forcefully terminates the composition.
// Unlike HandleFocusLost, this does NOT hide the toolbar since the user is still
// in the same input field.
func (c *Coordinator) HandleCompositionTerminated() {
	c.logger.Debug("Composition terminated, clearing input state")

	c.mu.Lock()
	defer c.mu.Unlock()

	// Only clear input state and hide candidate window, keep toolbar visible
	c.clearState()
	c.hideUI()
}

// HandleIMEDeactivated handles IME being switched away (user selected another IME)
// This is called from TSF's Deactivate method, before the client disconnects
func (c *Coordinator) HandleIMEDeactivated() {
	c.logger.Info("IME deactivated (user switched to another IME), hiding toolbar")

	c.mu.Lock()
	c.imeActivated = false
	c.clearState()
	c.mu.Unlock()

	// Immediately hide the toolbar
	if c.uiManager != nil {
		c.uiManager.SetToolbarVisible(false)
		c.uiManager.Hide()
	}
}

// HandleClientDisconnected handles TSF client disconnection
// When all clients disconnect (activeClients == 0), hide the toolbar
func (c *Coordinator) HandleClientDisconnected(activeClients int) {
	c.logger.Debug("Client disconnected", "activeClients", activeClients)

	if activeClients == 0 {
		c.logger.Info("All TSF clients disconnected, hiding toolbar")
		c.mu.Lock()
		c.imeActivated = false
		c.mu.Unlock()

		// Hide toolbar and candidate window
		if c.uiManager != nil {
			c.uiManager.SetToolbarVisible(false)
			c.uiManager.Hide()
		}
	}
}

// getCompiledHotkeys returns compiled hotkey hashes for C++ side
// 使用缓存避免每次焦点变化重新编译
func (c *Coordinator) getCompiledHotkeys() (keyDownHotkeys, keyUpHotkeys []uint32) {
	if c.hotkeyCompiler == nil {
		return nil, nil
	}
	if !c.hotkeysDirty && c.cachedKeyDownHotkeys != nil {
		return c.cachedKeyDownHotkeys, c.cachedKeyUpHotkeys
	}
	c.cachedKeyDownHotkeys, c.cachedKeyUpHotkeys = c.hotkeyCompiler.Compile()
	c.hotkeysDirty = false
	c.logger.Debug("Compiled hotkeys for C++",
		"keyDownCount", len(c.cachedKeyDownHotkeys),
		"keyUpCount", len(c.cachedKeyUpHotkeys))
	return c.cachedKeyDownHotkeys, c.cachedKeyUpHotkeys
}

// HandleFocusGained handles focus gained events and returns current status
func (c *Coordinator) HandleFocusGained() *bridge.StatusUpdateData {
	c.logger.Debug("Focus gained")

	// 焦点变化后异步释放内存（非阻塞，不影响响应速度）
	defer func() {
		go func() {
			runtime.GC()
			debug.FreeOSMemory()
		}()
	}()

	// Clear any pending input state when focus changes
	// This ensures composition state is consistent
	c.mu.Lock()
	if len(c.inputBuffer) > 0 {
		c.inputBuffer = ""
		c.inputCursorPos = 0
		c.candidates = nil
		c.currentPage = 1
		c.totalPages = 1
		c.logger.Debug("Cleared input buffer on focus gained")
	}
	c.mu.Unlock()

	// Hide candidate window (will be shown again when user starts typing)
	c.hideUI()

	// Set IME as activated (this will show toolbar if enabled)
	c.SetIMEActivated(true)

	// Return current status so TSF can sync state (including compiled hotkeys)
	keyDownHotkeys, keyUpHotkeys := c.getCompiledHotkeys()
	c.mu.Lock()
	defer c.mu.Unlock()

	// Sync CapsLock state from system on focus gain
	c.capsLockOn = ui.GetCapsLockState()

	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		IconLabel:          c.getIconLabelNoLock(),
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
}

// HandleIMEActivated handles IME being switched back (user selected this IME again)
// This is called from TSF's Activate method
func (c *Coordinator) HandleIMEActivated() *bridge.StatusUpdateData {
	c.logger.Info("IME activated (user switched back to this IME)")

	// Clear any pending input state when IME is reactivated
	// This ensures composition state is consistent
	c.mu.Lock()
	if len(c.inputBuffer) > 0 {
		c.inputBuffer = ""
		c.inputCursorPos = 0
		c.candidates = nil
		c.currentPage = 1
		c.totalPages = 1
		c.logger.Debug("Cleared input buffer on IME activated")
	}
	c.mu.Unlock()

	// Hide candidate window (will be shown again when user starts typing)
	c.hideUI()

	// Set IME as activated (this will show toolbar if enabled)
	c.SetIMEActivated(true)

	// Return current status so TSF can sync state (including compiled hotkeys)
	keyDownHotkeys, keyUpHotkeys := c.getCompiledHotkeys()
	c.mu.Lock()
	defer c.mu.Unlock()

	// Sync CapsLock state from system on IME activation
	c.capsLockOn = ui.GetCapsLockState()

	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		IconLabel:          c.getIconLabelNoLock(),
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
}

// HandleCommitRequest handles a commit request from TSF (barrier mechanism)
// This is called when Space/Enter/number key is pressed during composition
func (c *Coordinator) HandleCommitRequest(data bridge.CommitRequestData) *bridge.CommitResultData {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.logger.Debug("Handling commit request",
		"barrierSeq", data.BarrierSeq,
		"triggerKey", data.TriggerKey,
		"inputBuffer", data.InputBuffer)

	var text string
	var newComposition string
	var modeChanged bool

	// Determine action based on trigger key
	switch data.TriggerKey {
	case 0x20: // VK_SPACE
		result := c.handleSpaceInternal()
		if result != nil {
			text = result.Text
			modeChanged = result.ModeChanged
			newComposition = result.NewComposition
		}

	case 0x0D: // VK_RETURN
		result := c.handleEnterInternal()
		if result != nil {
			text = result.Text
		}

	default:
		// Number keys 1-9 (VK codes 0x31-0x39)
		if data.TriggerKey >= 0x31 && data.TriggerKey <= 0x39 {
			num := int(data.TriggerKey - 0x30) // Convert VK code to number 1-9
			result := c.handleNumberKeyInternal(num)
			if result != nil {
				text = result.Text
				newComposition = result.NewComposition
			}
		} else if data.TriggerKey == 0x30 {
			// Number key 0 selects 10th candidate
			result := c.handleNumberKeyInternal(10)
			if result != nil {
				text = result.Text
				newComposition = result.NewComposition
			}
		}
	}

	return &bridge.CommitResultData{
		BarrierSeq:     data.BarrierSeq,
		Text:           text,
		NewComposition: newComposition,
		ModeChanged:    modeChanged,
		ChineseMode:    c.chineseMode,
	}
}

// handleSpaceInternal is the internal implementation of handleSpace (without lock)
func (c *Coordinator) handleSpaceInternal() *bridge.KeyEventResult {
	// Select first candidate of current page
	if len(c.candidates) > 0 {
		// Calculate index of first candidate on current page
		index := (c.currentPage - 1) * c.candidatesPerPage
		if index < len(c.candidates) {
			return c.selectCandidateInternal(index)
		}
	} else if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		// No candidates, commit confirmed segments + raw input
		var finalText string
		for _, seg := range c.confirmedSegments {
			t := seg.Text
			if c.fullWidth {
				t = transform.ToFullWidth(t)
			}
			finalText += t
		}
		if len(c.inputBuffer) > 0 {
			raw := c.inputBuffer
			if c.fullWidth {
				raw = transform.ToFullWidth(raw)
			}
			finalText += raw
		}

		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: finalText,
		}
	}
	return nil
}

// handleEnterInternal is the internal implementation of handleEnter (without lock)
func (c *Coordinator) handleEnterInternal() *bridge.KeyEventResult {
	if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		var finalText string
		for _, seg := range c.confirmedSegments {
			t := seg.Text
			if c.fullWidth {
				t = transform.ToFullWidth(t)
			}
			finalText += t
		}
		if len(c.inputBuffer) > 0 {
			raw := c.inputBuffer
			if c.fullWidth {
				raw = transform.ToFullWidth(raw)
			}
			finalText += raw
		}

		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: finalText,
		}
	}
	return nil
}

// handleNumberKeyInternal is the internal implementation of handleNumberKey (without lock)
func (c *Coordinator) handleNumberKeyInternal(num int) *bridge.KeyEventResult {
	// num is 1-9 or 10 (key '0'), convert to 0-based index within current page
	index := (c.currentPage-1)*c.candidatesPerPage + (num - 1)
	if index < len(c.candidates) {
		return c.selectCandidateInternal(index)
	}
	return nil
}

// selectCandidateInternal is the internal implementation of selectCandidate (without lock)
func (c *Coordinator) selectCandidateInternal(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return nil
	}

	cand := c.candidates[index]
	c.logger.Debug("Candidate selected (internal)", "index", index)

	originalText := cand.Text
	text := originalText

	// Apply full-width conversion if enabled
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 拼音引擎分步确认：候选消耗的输入长度小于缓冲区长度时，
	// 将已确认的文字暂存到 confirmedSegments 而非直接上屏。
	isPinyin := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin
	if isPinyin && cand.ConsumedLength > 0 && cand.ConsumedLength < len(c.inputBuffer) {
		consumedCode := c.inputBuffer[:cand.ConsumedLength]
		if !cand.IsCommand {
			c.engineMgr.OnCandidateSelected(consumedCode, originalText)
		}

		remaining := c.inputBuffer[cand.ConsumedLength:]
		c.logger.Debug("Partial confirm internal (pinyin)", "index", index, "text", text,
			"consumed", cand.ConsumedLength, "remaining", remaining,
			"confirmedCount", len(c.confirmedSegments)+1)

		// 推入确认栈，不上屏
		c.confirmedSegments = append(c.confirmedSegments, ConfirmedSegment{
			Text:         originalText,
			ConsumedCode: consumedCode,
		})

		c.inputBuffer = remaining
		c.inputCursorPos = len(remaining)
		c.currentPage = 1
		c.updateCandidates()
		c.showUI()

		// 返回 UpdateComposition 而非 InsertText
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}

	// 完全消费或非拼音：连同所有确认段一次性上屏
	if isPinyin && c.engineMgr != nil && !cand.IsCommand {
		c.engineMgr.OnCandidateSelected(c.inputBuffer, originalText)
	}

	// 拼接所有已确认段的文本 + 当前选中的候选
	finalText := text
	if isPinyin && len(c.confirmedSegments) > 0 {
		var allText string
		for _, seg := range c.confirmedSegments {
			t := seg.Text
			if c.fullWidth {
				t = transform.ToFullWidth(t)
			}
			allText += t
		}
		finalText = allText + text
	}

	c.clearState()
	c.hideUI()

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: finalText,
	}
}
