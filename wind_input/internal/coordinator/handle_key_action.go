// handle_key_action.go — 各按键处理器（字母、退格、光标、回车、空格、翻页、候选选择）
package coordinator

import (
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/transform"
)

func (c *Coordinator) handleAlphaKey(key string) *bridge.KeyEventResult {
	startTime := time.Now()
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
		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	// 检查空码处理
	if result != nil && result.IsEmpty {
		if result.ShouldClear {
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}
		if result.ToEnglish {
			text := c.inputBuffer
			// Apply full-width conversion if enabled
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text,
			}
		}
	}

	showStart := time.Now()
	c.showUI()
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

		if len(c.inputBuffer) == 0 {
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
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

	if len(c.inputBuffer) == 0 {
		// Buffer is already empty - pass through to system
		c.logger.Debug("Backspace with empty buffer, passing through to system")
		return nil
	}

	// Cursor at beginning but buffer not empty - consume the key (don't pass to system)
	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeConsumed,
	}
}

func (c *Coordinator) handleCursorLeft() *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		return nil // 无输入时透传
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
		return nil // 无输入时透传
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

func (c *Coordinator) handleEnter() *bridge.KeyEventResult {
	// Commit raw input as text
	if len(c.inputBuffer) > 0 {
		text := c.inputBuffer

		// Apply full-width conversion if enabled
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}

		c.clearState()
		c.hideUI()

		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
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

	if len(c.inputBuffer) > 0 {
		c.clearState()
		c.hideUI()
	}
	// Always return ClearComposition to ensure C++ side's _isComposing is reset
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

func (c *Coordinator) handleSpace() *bridge.KeyEventResult {
	// Select first candidate of current page
	if len(c.candidates) > 0 {
		// Calculate index of first candidate on current page
		index := (c.currentPage - 1) * c.candidatesPerPage
		if index < len(c.candidates) {
			return c.selectCandidate(index)
		}
	} else if len(c.inputBuffer) > 0 {
		// No candidates, commit raw input
		text := c.inputBuffer

		// Apply full-width conversion if enabled
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}

		c.clearState()
		c.hideUI()
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}
	return nil
}

func (c *Coordinator) handleNumberKey(num int) *bridge.KeyEventResult {
	// num is 1-9, convert to 0-based index within current page
	index := (c.currentPage-1)*c.candidatesPerPage + (num - 1)
	if index < len(c.candidates) {
		return c.selectCandidate(index)
	}
	return nil
}

func (c *Coordinator) handlePageUp() *bridge.KeyEventResult {
	// Pass through only if no candidates
	if len(c.candidates) == 0 {
		return nil
	}

	// Have candidates - always consume the key, even if at first page
	if c.currentPage > 1 {
		c.currentPage--
		c.logger.Debug("Page up", "currentPage", c.currentPage, "totalPages", c.totalPages)
		c.showUI()
	}
	// Return Consumed to indicate key was handled (don't pass to application)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

func (c *Coordinator) handlePageDown() *bridge.KeyEventResult {
	// Pass through only if no candidates
	if len(c.candidates) == 0 {
		return nil
	}

	// Have candidates - always consume the key, even if at last page
	if c.currentPage < c.totalPages {
		c.currentPage++
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

	// 拼音引擎部分上屏：候选消耗的输入长度小于缓冲区长度时，保留剩余部分
	isPinyin := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin
	if isPinyin && cand.ConsumedLength > 0 && cand.ConsumedLength < len(c.inputBuffer) {
		// 用户词频学习：命令候选不进入学习，避免污染用户词典（如 uuid）。
		if !cand.IsCommand {
			consumedCode := c.inputBuffer[:cand.ConsumedLength]
			c.engineMgr.OnCandidateSelected(consumedCode, originalText)
		}

		remaining := c.inputBuffer[cand.ConsumedLength:]
		c.logger.Debug("Partial commit (pinyin)", "index", index, "text", text,
			"consumed", cand.ConsumedLength, "remaining", remaining)

		// 更新缓冲区为剩余部分，光标重置到末尾，重新触发候选更新
		c.inputBuffer = remaining
		c.inputCursorPos = len(remaining)
		c.currentPage = 1
		c.updateCandidates()
		c.showUI()

		return &bridge.KeyEventResult{
			Type:           bridge.ResponseTypeInsertText,
			Text:           text,
			NewComposition: c.compositionText(),
		}
	}

	// 用户词频学习：命令候选不进入学习，避免污染用户词典（如 uuid）。
	if isPinyin && c.engineMgr != nil && !cand.IsCommand {
		c.engineMgr.OnCandidateSelected(c.inputBuffer, originalText)
	}

	c.logger.Debug("Candidate selected", "index", index, "original", originalText, "output", text, "fullWidth", c.fullWidth)

	c.clearState()
	c.hideUI()

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: text,
	}
}
