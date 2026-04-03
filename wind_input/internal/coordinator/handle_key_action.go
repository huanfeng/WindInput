// handle_key_action.go — 各按键处理器（字母、退格、光标、回车、空格、翻页、候选选择）
package coordinator

import (
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/transform"
)

// maxInputBufferLen 输入缓冲区最大长度（字节），超过此长度拒绝新输入
const maxInputBufferLen = 60

func (c *Coordinator) handleAlphaKey(key string) *bridge.KeyEventResult {
	startTime := time.Now()

	// 限制输入缓冲区长度，超长输入没有实际意义且影响性能
	if len(c.inputBuffer)+len(key) > maxInputBufferLen {
		c.logger.Debug("Input buffer length limit reached", "current", len(c.inputBuffer), "max", maxInputBufferLen)
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	wasEmpty := len(c.inputBuffer) == 0
	// 在光标位置插入字符
	c.inputBuffer = c.inputBuffer[:c.inputCursorPos] + key + c.inputBuffer[c.inputCursorPos:]
	c.inputCursorPos += len(key)
	c.logger.Debug("Input buffer updated", "buffer", c.inputBuffer, "cursor", c.inputCursorPos)

	// 处理顶码（如五笔的五码顶字）
	if c.engineMgr != nil {
		commitText, newInput, shouldCommit := c.engineMgr.HandleTopCode(c.inputBuffer)
		if shouldCommit {
			c.inputBuffer = newInput
			c.inputCursorPos = len(newInput)
			c.logger.Debug("Top code commit", "newInputLen", len(newInput))

			// Apply full-width conversion if enabled
			if c.fullWidth {
				commitText = transform.ToFullWidth(commitText)
			}

			// 如果还有剩余输入，继续处理并更新候选
			if len(c.inputBuffer) > 0 {
				c.updateCandidates()
				c.showUI()
				// 返回带有新组合的响应，让 C++ 端同时插入文字并开始新组合
				return &bridge.KeyEventResult{
					Type:           bridge.ResponseTypeInsertText,
					Text:           commitText,
					NewComposition: c.inputBuffer,
				}
			} else {
				c.hideUI()
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: commitText,
				}
			}
		}
	}

	// 更新候选词
	updateStart := time.Now()
	result := c.updateCandidatesEx()
	updateElapsed := time.Since(updateStart)

	// 检查自动上屏
	if result != nil && result.ShouldCommit {
		text := result.CommitText
		// Apply full-width conversion if enabled
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}
		// 拼接已确认段的文本
		prefix := c.confirmedPrefix()
		if c.fullWidth && prefix != "" {
			prefix = transform.ToFullWidth(prefix)
		}
		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: prefix + text,
		}
	}

	// 检查空码处理
	if result != nil && result.IsEmpty {
		if result.ShouldClear {
			// 如果有已确认段，先上屏确认段的文字再清空
			if len(c.confirmedSegments) > 0 {
				prefix := c.confirmedPrefix()
				if c.fullWidth {
					prefix = transform.ToFullWidth(prefix)
				}
				c.clearState()
				c.hideUI()
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: prefix,
				}
			}
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}
		if result.ToEnglish {
			// 拼接已确认段 + 剩余编码作为英文上屏
			prefix := c.confirmedPrefix()
			if c.fullWidth && prefix != "" {
				prefix = transform.ToFullWidth(prefix)
			}
			text := c.inputBuffer
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: prefix + text,
			}
		}
	}

	showStart := time.Now()
	// 首字符延迟显示候选窗口。某些应用（EverEdit、微信等）在 composition 创建后
	// 不会立即更新 GetTextExt 返回值，需要等消息循环运行（WM_PAINT）后才会刷新。
	// ITfTextLayoutSink::OnLayoutChange 会在应用重排后触发准确的 caret update。
	// 我们跳过同步调用栈内的 stale 更新（通常 <5ms），等待 OnLayoutChange 的更新
	// （通常 15-30ms），避免候选框先出现在错误位置再跳动。
	if wasEmpty {
		c.pendingFirstShow = true
		c.pendingFirstShowTime = time.Now()
		c.logger.Debug("First character, deferring candidate window for OnLayoutChange")

		// 超时回退：如果 100ms 内 OnLayoutChange 没有触发（应用不支持），强制显示
		go func() {
			time.Sleep(100 * time.Millisecond)
			c.mu.Lock()
			defer c.mu.Unlock()
			if c.pendingFirstShow && len(c.inputBuffer) > 0 && len(c.candidates) > 0 {
				c.pendingFirstShow = false
				c.logger.Debug("pendingFirstShow timeout, force showing")
				c.showUI()
			}
		}()
	} else {
		c.pendingFirstShow = false
		c.showUI()
	}
	showElapsed := time.Since(showStart)

	totalElapsed := time.Since(startTime)
	c.logger.Debug("handleAlphaKey timing", "total", totalElapsed.String(), "updateCandidates", updateElapsed.String(), "showUI", showElapsed.String())

	// Handle Inline Preedit
	if c.config != nil && c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}

	// When InlinePreedit is disabled, we still need to tell TSF that we have an active
	// composition so that subsequent keys (ESC, Backspace, Enter) are intercepted.
	// Return UpdateComposition with empty text - TSF will set _isComposing=TRUE but
	// won't display anything in the application.
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     "",
		CaretPos: 0,
	}
}

func (c *Coordinator) handleBackspace() *bridge.KeyEventResult {
	if len(c.inputBuffer) > 0 && c.inputCursorPos > 0 {
		// 在光标位置删除前一个字符
		c.inputBuffer = c.inputBuffer[:c.inputCursorPos-1] + c.inputBuffer[c.inputCursorPos:]
		c.inputCursorPos--
		c.logger.Debug("Input buffer after backspace", "buffer", c.inputBuffer, "cursor", c.inputCursorPos)

		if len(c.inputBuffer) == 0 && len(c.confirmedSegments) == 0 {
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}

		// inputBuffer 为空但仍有确认段时，回退到上一个确认段
		if len(c.inputBuffer) == 0 && len(c.confirmedSegments) > 0 {
			return c.popConfirmedSegment()
		}

		c.updateCandidates()
		c.showUI()

		// Handle Inline Preedit
		if c.config != nil && c.config.UI.InlinePreedit {
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.compositionText(),
				CaretPos: c.displayCursorPos(),
			}
		}

		// When InlinePreedit is disabled, still use UpdateComposition with empty text
		// so that TSF knows there's an active composition
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     "",
			CaretPos: 0,
		}
	}

	// 光标在编码最左边按退格，且有确认段 → 弹出最后确认段，拼接到当前 buffer 前面
	if len(c.inputBuffer) > 0 && c.inputCursorPos == 0 && len(c.confirmedSegments) > 0 {
		lastSeg := c.confirmedSegments[len(c.confirmedSegments)-1]
		c.confirmedSegments = c.confirmedSegments[:len(c.confirmedSegments)-1]
		c.inputBuffer = lastSeg.ConsumedCode + c.inputBuffer
		c.inputCursorPos = len(lastSeg.ConsumedCode)
		c.logger.Debug("Backspace at cursor 0: pop confirmed segment",
			"restored", lastSeg.ConsumedCode, "buffer", c.inputBuffer)

		c.updateCandidates()
		c.showUI()

		if c.config != nil && c.config.UI.InlinePreedit {
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.compositionText(),
				CaretPos: c.displayCursorPos(),
			}
		}
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     "",
			CaretPos: 0,
		}
	}

	// inputBuffer 为空且有确认段 → 弹出最后确认段恢复编码
	if len(c.inputBuffer) == 0 && len(c.confirmedSegments) > 0 {
		return c.popConfirmedSegment()
	}

	if len(c.inputBuffer) == 0 {
		// Buffer is already empty and no confirmed segments - pass through to system
		c.logger.Debug("Backspace with empty buffer, passing through to system")
		return nil
	}

	// Cursor at beginning but buffer not empty - consume the key (don't pass to system)
	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeConsumed,
	}
}

// handleDelete 处理 Delete 键（前删：删除光标后方的字符）
// 与 Backspace（退删）互补，提供完整的编码编辑体验
func (c *Coordinator) handleDelete() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		// 无输入缓冲但有 pending 状态（如确认段）→ 吃掉，不透传
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil // 完全无输入时透传
	}
	if c.inputCursorPos < len(c.inputBuffer) {
		// 前删：删除光标位置的字符
		c.inputBuffer = c.inputBuffer[:c.inputCursorPos] + c.inputBuffer[c.inputCursorPos+1:]
		c.logger.Debug("Delete key", "buffer", c.inputBuffer, "cursor", c.inputCursorPos)

		if len(c.inputBuffer) == 0 && len(c.confirmedSegments) == 0 {
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}

		// inputBuffer 清空但仍有确认段时，回退到上一个确认段
		if len(c.inputBuffer) == 0 && len(c.confirmedSegments) > 0 {
			return c.popConfirmedSegment()
		}

		c.updateCandidates()
		c.showUI()

		if c.config != nil && c.config.UI.InlinePreedit {
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.compositionText(),
				CaretPos: c.displayCursorPos(),
			}
		}
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     "",
			CaretPos: 0,
		}
	}
	// 光标在末尾 → 吃掉，不透传给系统
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

// popConfirmedSegment 弹出最后一个确认段，将其编码恢复到 inputBuffer 中。
func (c *Coordinator) popConfirmedSegment() *bridge.KeyEventResult {
	lastSeg := c.confirmedSegments[len(c.confirmedSegments)-1]
	c.confirmedSegments = c.confirmedSegments[:len(c.confirmedSegments)-1]
	c.inputBuffer = lastSeg.ConsumedCode
	c.inputCursorPos = len(lastSeg.ConsumedCode)
	c.logger.Debug("Pop confirmed segment", "restored", lastSeg.ConsumedCode,
		"remainingSegments", len(c.confirmedSegments))

	c.updateCandidates()
	c.showUI()

	if c.config != nil && c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     "",
		CaretPos: 0,
	}
}

func (c *Coordinator) handleCursorLeft() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil // 完全无输入时透传
	}
	if c.inputCursorPos > 0 {
		c.inputCursorPos--
		c.logger.Debug("Cursor left", "cursor", c.inputCursorPos)
		if c.config != nil && c.config.UI.InlinePreedit {
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.compositionText(),
				CaretPos: c.displayCursorPos(),
			}
		}
		// InlinePreedit 关闭时，刷新候选窗口中的光标位置
		c.showUI()
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleCursorRight() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil // 完全无输入时透传
	}
	if c.inputCursorPos < len(c.inputBuffer) {
		c.inputCursorPos++
		c.logger.Debug("Cursor right", "cursor", c.inputCursorPos)
		if c.config != nil && c.config.UI.InlinePreedit {
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.compositionText(),
				CaretPos: c.displayCursorPos(),
			}
		}
		// InlinePreedit 关闭时，刷新候选窗口中的光标位置
		c.showUI()
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleCursorHome() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil
	}
	c.inputCursorPos = 0
	c.logger.Debug("Cursor home", "cursor", c.inputCursorPos)
	if c.config != nil && c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}
	// InlinePreedit 关闭时，刷新候选窗口中的光标位置
	c.showUI()
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleCursorEnd() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil
	}
	c.inputCursorPos = len(c.inputBuffer)
	c.logger.Debug("Cursor end", "cursor", c.inputCursorPos)
	if c.config != nil && c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}
	// InlinePreedit 关闭时，刷新候选窗口中的光标位置
	c.showUI()
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleArrowUp() *bridge.KeyEventResult {
	if len(c.candidates) == 0 {
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil // 完全无输入时透传
	}
	if c.selectedIndex > 0 {
		c.selectedIndex--
		c.logger.Debug("Arrow up", "selectedIndex", c.selectedIndex)
		c.showUI()
	} else if c.currentPage > 1 {
		// 当前页第一个，跳转到上一页最后一个
		c.currentPage--
		startIdx := (c.currentPage - 1) * c.candidatesPerPage
		endIdx := startIdx + c.candidatesPerPage
		if endIdx > len(c.candidates) {
			endIdx = len(c.candidates)
		}
		c.selectedIndex = endIdx - startIdx - 1
		c.logger.Debug("Arrow up to previous page", "currentPage", c.currentPage, "selectedIndex", c.selectedIndex)
		c.showUI()
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleArrowDown() *bridge.KeyEventResult {
	if len(c.candidates) == 0 {
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil // 完全无输入时透传
	}
	// 计算当前页候选数量
	startIdx := (c.currentPage - 1) * c.candidatesPerPage
	endIdx := startIdx + c.candidatesPerPage
	if endIdx > len(c.candidates) {
		endIdx = len(c.candidates)
	}
	pageCount := endIdx - startIdx
	if c.selectedIndex < pageCount-1 {
		c.selectedIndex++
		c.logger.Debug("Arrow down", "selectedIndex", c.selectedIndex)
		c.showUI()
	} else if c.currentPage < c.totalPages {
		// 当前页最后一个，跳转到下一页第一个
		c.currentPage++
		c.selectedIndex = 0
		c.logger.Debug("Arrow down to next page", "currentPage", c.currentPage, "selectedIndex", c.selectedIndex)
		c.showUI()
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handleEnter() *bridge.KeyEventResult {
	// Commit all confirmed segments + raw input as text
	if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		var finalText string
		// 拼接已确认段的汉字
		for _, seg := range c.confirmedSegments {
			t := seg.Text
			if c.fullWidth {
				t = transform.ToFullWidth(t)
			}
			finalText += t
		}
		// 追加剩余原始编码
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

func (c *Coordinator) handleEscape() *bridge.KeyEventResult {
	// If toolbar context menu is open, close it and consume ESC
	if c.uiManager != nil && c.uiManager.IsToolbarMenuOpen() {
		c.logger.Debug("ESC closes toolbar context menu")
		c.uiManager.HideToolbarMenu()
		// Return Consumed to eat the ESC key (don't pass to app)
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	// If candidate context menu is open, close it and consume ESC
	if c.uiManager != nil && c.uiManager.IsCandidateMenuOpen() {
		c.logger.Debug("ESC closes candidate context menu")
		c.uiManager.HideCandidateMenu()
		// Return Consumed to eat the ESC key (don't pass to app)
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		c.clearState()
		c.hideUI()
	}
	// Always return ClearComposition to ensure C++ side's _isComposing is reset
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

func (c *Coordinator) handleSpace() *bridge.KeyEventResult {
	// Select the currently highlighted candidate (controlled by up/down arrows)
	if len(c.candidates) > 0 {
		index := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
		if index < len(c.candidates) {
			return c.selectCandidate(index)
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

func (c *Coordinator) handleNumberKey(num int) *bridge.KeyEventResult {
	// num is 1-9 or 10 (key '0'), convert to 0-based index within current page
	index := (c.currentPage-1)*c.candidatesPerPage + (num - 1)
	if index < len(c.candidates) {
		return c.selectCandidate(index)
	}
	return nil
}

func (c *Coordinator) handlePageUp() *bridge.KeyEventResult {
	// Pass through only if no candidates and no pending input
	if len(c.candidates) == 0 {
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil
	}

	// Have candidates - always consume the key, even if at first page
	if c.currentPage > 1 {
		c.currentPage--
		c.selectedIndex = 0
		c.logger.Debug("Page up", "currentPage", c.currentPage, "totalPages", c.totalPages)
		c.showUI()
	}
	// Return Consumed to indicate key was handled (don't pass to application)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handlePageDown() *bridge.KeyEventResult {
	// Pass through only if no candidates and no pending input
	if len(c.candidates) == 0 {
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil
	}

	// Have candidates - always consume the key, even if at last page
	if c.currentPage < c.totalPages {
		c.currentPage++
		c.selectedIndex = 0
		c.logger.Debug("Page down", "currentPage", c.currentPage, "totalPages", c.totalPages)
		c.showUI()
	}
	// Return Consumed to indicate key was handled (don't pass to application)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) selectCandidate(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return nil
	}

	cand := c.candidates[index]
	originalText := cand.Text
	text := originalText

	// Apply full-width conversion if enabled
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 拼音引擎分步确认：候选消耗的输入长度小于缓冲区长度时，
	// 将已确认的文字暂存到 confirmedSegments 而非直接上屏，
	// 用户可以通过退格键回退重新选词。
	isPinyin := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin
	isMixed := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypeMixed
	if (isPinyin || (isMixed && cand.ConsumedLength > 0)) && cand.ConsumedLength > 0 && cand.ConsumedLength < len(c.inputBuffer) {
		// 用户词频学习：命令候选不进入学习，避免污染用户词典（如 uuid）。
		consumedCode := c.inputBuffer[:cand.ConsumedLength]
		if !cand.IsCommand {
			c.engineMgr.OnCandidateSelected(consumedCode, originalText, cand.Source)
		}

		remaining := c.inputBuffer[cand.ConsumedLength:]
		c.logger.Debug("Partial confirm (pinyin)", "index", index, "text", text,
			"consumed", cand.ConsumedLength, "remaining", remaining,
			"confirmedCount", len(c.confirmedSegments)+1)

		// 推入确认栈，不上屏
		c.confirmedSegments = append(c.confirmedSegments, ConfirmedSegment{
			Text:         originalText,
			ConsumedCode: consumedCode,
		})

		// 更新缓冲区为剩余部分，光标重置到末尾，重新触发候选更新
		c.inputBuffer = remaining
		c.inputCursorPos = len(remaining)
		c.currentPage = 1
		c.updateCandidates()
		c.showUI()

		// 返回 UpdateComposition 而非 InsertText，文字留在组合态中
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

	// 记录输入历史（用于加词推荐）
	if c.inputHistory != nil && !cand.IsCommand {
		histText := originalText
		histCode := c.inputBuffer
		if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
			var fCode, fText string
			for _, seg := range c.confirmedSegments {
				fCode += seg.ConsumedCode
				fText += seg.Text
			}
			histText = fText + originalText
			histCode = fCode + c.inputBuffer
		}
		c.inputHistory.Record(histText, histCode, "", 0)
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

	c.logger.Debug("Candidate selected (full commit)", "index", index, "original", originalText,
		"output", finalText, "fullWidth", c.fullWidth, "confirmedSegments", len(c.confirmedSegments))

	c.clearState()
	c.hideUI()

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: finalText,
	}
}
