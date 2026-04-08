// handle_lifecycle.go — IME 生命周期事件（焦点、激活、停用）与 CommitRequest 处理
package coordinator

import (
	"runtime"
	"runtime/debug"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
)

// HandleCaretUpdate handles caret position updates from C++ Bridge
func (c *Coordinator) HandleCaretUpdate(data bridge.CaretData) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	// 应用兼容性规则：caret_use_top 将 Y 从 rect.bottom 转换为 rect.top。
	// 微信等 WebView 应用的 GetTextExt 返回的 height 不稳定（h=1 或 h=20），
	// 导致 rect.bottom 在不同时刻差异达 20px，但 rect.top 始终稳定（差异 ≤1px）。
	if c.activeCompatRule != nil && c.activeCompatRule.CaretUseTop && data.Height > 0 {
		rawH := data.Height
		data.Y = data.Y - rawH // bottom → top
		data.Height = 1        // 最小高度，确保候选框紧贴文字下方
		if data.CompositionStartY != 0 {
			data.CompositionStartY = data.CompositionStartY - rawH
		}
	}

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

	// If there's active input, refresh the candidate window position.
	// This handles the case where C++ re-sends caret update after composition
	// start/update, providing the up-to-date position.
	hasInput := len(c.inputBuffer) > 0
	hasCandidates := len(c.candidates) > 0
	hasUI := c.uiManager != nil
	if hasInput && hasCandidates && hasUI {
		if c.pendingFirstShow {
			// 首字符延迟显示：跳过同步调用栈内的 stale 更新。
			// OnKeyDown → SendCaretPositionUpdate 和 UpdateComposition → SendCaretPositionUpdate
			// 都在同一调用栈中完成，此时 GetTextExt 可能返回旧坐标。
			// OnLayoutChange 在应用消息循环运行后触发（通常 10-30ms），坐标才可靠。
			// 超过 80ms 仍未收到新更新时强制显示，避免候选窗口不出现。
			elapsed := time.Since(c.pendingFirstShowTime)
			c.diagCaretUpdateCount++

			// 计算当前位置与 Position A 的差异
			dx := data.X - c.diagPreKeyCaretX
			dy := data.Y - c.diagPreKeyCaretY
			if dx < 0 {
				dx = -dx
			}
			if dy < 0 {
				dy = -dy
			}

			// 对已知不可靠的进程，使用更长的等待阈值（20ms）以跳过两段式重排的中间值。
			// 微信等应用在 composition 创建后会经历两段重排：
			//   第一段 ~10ms：位置近似正确但非最终值（height=20）
			//   第二段 ~15-20ms：位置最终确定（height 可能变为 1）
			// 普通应用（记事本等）在 10ms 时位置已稳定，使用默认 10ms 阈值。
			minWait := 10 * time.Millisecond
			pendingProfile := c.caretProfiles[c.activeProcessID]
			if pendingProfile != nil && !pendingProfile.posAReliable {
				minWait = 20 * time.Millisecond
			}

			if elapsed < minWait {
				// 同步调用栈或中间值更新，跳过（不逐条输出日志，仅计数）
				return nil
			}
			// OnLayoutChange 或超时后的更新，可信赖
			c.pendingFirstShow = false
			reliable := dx <= 4 && dy <= 4
			c.updateCaretProfile(reliable)
			// 一条汇总日志：posA=预按键位置, posC=最终接受位置, skipped=跳过的中间更新数
			c.logger.Debug("caret.diag first=resolved",
				"posA", [2]int{c.diagPreKeyCaretX, c.diagPreKeyCaretY},
				"posC", [2]int{data.X, data.Y}, "hC", data.Height,
				"dX", dx, "dY", dy,
				"elapsed", elapsed.String(),
				"skipped", c.diagCaretUpdateCount-1,
				"reliable", reliable,
				"pid", c.activeProcessID)
		} else if !c.lastKeyTime.IsZero() {
			sinceKey := time.Since(c.lastKeyTime)
			profile := c.caretProfiles[c.activeProcessID]

			if profile != nil && !profile.posAReliable {
				// 已知不可靠进程：跳过按键后 25ms 内的 caret 更新。
				// 微信等应用两段式重排在 ~20ms 内才最终稳定，25ms 覆盖完整周期。
				if sinceKey < 25*time.Millisecond {
					return nil
				}
			} else if sinceKey >= 10*time.Millisecond && c.diagPreKeyCaretValid {
				// 自校验：检测 OnLayoutChange 与 Position A 的偏差，必要时降级 profile。
				dx := data.X - c.diagPreKeyCaretX
				dy := data.Y - c.diagPreKeyCaretY
				if dx < 0 {
					dx = -dx
				}
				if dy < 0 {
					dy = -dy
				}
				if dx > 4 || dy > 4 {
					c.updateCaretProfile(false)
				}
			}
		}
		c.showUI()
	} else if c.pendingFirstShow && hasInput && hasUI && !hasCandidates {
		// 首字符但还没有候选（引擎还在处理），保持 pending
	}

	return nil
}

// HandleSelectionChanged handles selection/caret change events from ITfTextEditSink::OnEndEdit.
// This is called when the cursor moves outside of composition (e.g., mouse click).
func (c *Coordinator) HandleSelectionChanged(prevChar rune) {
	c.mu.Lock()
	defer c.mu.Unlock()

	// 选区变化时清空配对栈（自动配对插入后 200ms 内的 SelectionChanged 事件除外，
	// 这些事件是 CommitText 和光标移动引发的，不是用户操作）
	if c.pairTracker != nil {
		if c.pairInsertTime.IsZero() || time.Since(c.pairInsertTime) > 200*time.Millisecond {
			c.pairTracker.Clear()
		}
	}
	c.lastOutputWasDigit = false
	c.logger.Debug("Selection changed, reset smart punct state", "prevChar", string(prevChar))
}

// HandleHostRenderReady is called when host render shared memory is set up for the active client.
// This triggers updating the UI manager's render callbacks immediately, without waiting for next focus change.
func (c *Coordinator) HandleHostRenderReady() {
	c.updateHostRenderState()
}

// updateHostRenderState checks if the active process has host rendering and updates
// the UI manager's render callbacks accordingly.
func (c *Coordinator) updateHostRenderState() {
	if c.bridgeServer == nil || c.uiManager == nil {
		return
	}

	writeFrame, hideFunc := c.bridgeServer.GetActiveHostRender()
	if writeFrame != nil {
		c.logger.Info("Enabling host render for active process", "alreadyEnabled", c.uiManager.IsHostRendering())
		c.uiManager.SetHostRenderFunc(writeFrame, hideFunc)
	} else {
		if c.uiManager.IsHostRendering() {
			c.logger.Info("Disabling host render for active process")
		}
		c.uiManager.SetHostRenderFunc(nil, nil)
	}
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

	// 注意：不在此处清除 hostRenderFunc。HostRender 绑定到进程级别（共享内存按 PID
	// 建立），不应因进程内焦点变化而清除。showUI() 在每次绑定前调用
	// updateHostRenderState() 自动根据 activeProcessID 重新评估，切换到非 HostRender
	// 进程时会自然清除。若在此清除，开始菜单等受限环境中频繁的焦点抖动会导致
	// doShowCandidates 执行时 hostRenderFunc 为 nil，候选框回退到不可见的本地窗口。

	// Hide toolbar on real focus lost (user switched to another window/app)
	c.SetIMEActivated(false)

	c.mu.Lock()
	defer c.mu.Unlock()
	c.lastOutputWasDigit = false
	c.clearState()
}

// HandleCompositionTerminated handles composition unexpectedly terminated events
// This happens when the user clicks within the input field to change cursor position,
// or when the application forcefully terminates the composition.
// Unlike HandleFocusLost, this does NOT hide the toolbar since the user is still
// in the same input field.
func (c *Coordinator) HandleCompositionTerminated() {
	// HostRender 模式下（开始菜单等受限环境），SearchHost 的搜索框不支持 TSF
	// composition，DLL 每次设置 composition 文本后搜索框会立即终止它。但在
	// HostRender 模式下候选框通过 Band 窗口独立渲染，不依赖 TSF composition，
	// 因此忽略 composition 终止事件，保持输入状态和候选窗口不变。
	if c.uiManager != nil && c.uiManager.IsHostRendering() {
		c.logger.Debug("Composition terminated in host render mode, ignoring")
		return
	}

	c.logger.Debug("Composition terminated, clearing input state")

	c.mu.Lock()
	defer c.mu.Unlock()

	// 光标位置可能已变化（用户点击了输入框内其他位置），重置数字后智能标点状态
	c.lastOutputWasDigit = false
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
	c.lastOutputWasDigit = false
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
func (c *Coordinator) HandleFocusGained(processID uint32) *bridge.StatusUpdateData {
	if processID != 0 {
		c.activeProcessID = processID
		c.activeProcessName = bridge.GetProcessName(processID)
		c.activeCompatRule = c.appCompat.GetRule(c.activeProcessName)
		if c.activeCompatRule != nil {
			c.logger.Debug("Compat rule matched", "process", c.activeProcessName, "caretUseTop", c.activeCompatRule.CaretUseTop)
		}
	}
	c.logger.Debug("Focus gained", "processID", processID, "process", c.activeProcessName)

	// 焦点变化后异步释放内存（非阻塞，不影响响应速度）
	defer func() {
		go func() {
			runtime.GC()
			debug.FreeOSMemory()
		}()
	}()

	// Update host render state for the new active process
	c.updateHostRenderState()

	// Clear any pending input state when focus changes
	// This ensures composition state is consistent
	c.mu.Lock()
	c.lastOutputWasDigit = false
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
func (c *Coordinator) HandleIMEActivated(processID uint32) *bridge.StatusUpdateData {
	if processID != 0 {
		c.activeProcessID = processID
		c.activeProcessName = bridge.GetProcessName(processID)
		c.activeCompatRule = c.appCompat.GetRule(c.activeProcessName)
	}
	c.logger.Info("IME activated (user switched back to this IME)", "processID", processID)

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

	// 组候选：替换 inputBuffer 为组的完整编码，触发二级展开
	if cand.IsGroup && cand.GroupCode != "" {
		c.inputBuffer = cand.GroupCode
		c.inputCursorPos = len(c.inputBuffer)
		c.currentPage = 1
		c.selectedIndex = 0
		c.updateCandidates()
		c.showUI()
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}

	originalText := cand.Text
	text := originalText

	// Apply full-width conversion if enabled
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 拼音引擎分步确认：候选消耗的输入长度小于缓冲区长度时，
	// 将已确认的文字暂存到 confirmedSegments 而非直接上屏。
	isPinyin := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin
	isMixed := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypeMixed
	if (isPinyin || (isMixed && cand.ConsumedLength > 0)) && cand.ConsumedLength > 0 && cand.ConsumedLength < len(c.inputBuffer) {
		consumedCode := c.inputBuffer[:cand.ConsumedLength]
		if !cand.IsCommand {
			c.engineMgr.OnCandidateSelected(consumedCode, originalText, cand.Source)
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

	// 完全消费：触发学习回调（拼音和五笔统一）
	if c.engineMgr != nil && !cand.IsCommand {
		if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
			// 分段输入：合并所有段的编码和文本，作为完整词组学习
			var fullCode, fullText string
			for _, seg := range c.confirmedSegments {
				fullCode += seg.ConsumedCode
				fullText += seg.Text
			}
			fullCode += c.inputBuffer
			fullText += originalText
			c.engineMgr.OnCandidateSelected(fullCode, fullText, cand.Source)
		} else {
			c.engineMgr.OnCandidateSelected(c.inputBuffer, originalText, cand.Source)
		}
	}

	// 拼接所有已确认段的文本 + 当前选中的候选
	finalText := text
	if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
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
