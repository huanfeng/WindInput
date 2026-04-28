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
	c.lastKeyTime = startTime

	// 在变更 inputBuffer 之前抓取"是否为本次 composition 的首字"。若是，本次
	// 按键会让宿主（如 WPS）触发文本 reflow，光标位置会从按键前的位置漂移到
	// reflow 后的位置；此时不能用旧坐标 showUI，否则候选窗会先错位再跳。
	wasComposingEmpty := len(c.inputBuffer) == 0

	// 输入字母时清空配对栈（光标和配对之间插入了内容）
	if c.pairTracker != nil {
		c.pairTracker.Clear()
	}

	// 限制输入缓冲区长度，超长输入没有实际意义且影响性能
	if len(c.inputBuffer)+len(key) > maxInputBufferLen {
		c.logger.Debug("Input buffer length limit reached", "current", len(c.inputBuffer), "max", maxInputBufferLen)
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	// 在光标位置插入字符
	c.inputBuffer = c.inputBuffer[:c.inputCursorPos] + key + c.inputBuffer[c.inputCursorPos:]
	c.inputCursorPos += len(key)
	c.logger.Debug("Input buffer updated", "buffer", c.inputBuffer, "cursor", c.inputCursorPos)

	// 处理顶码（如五笔的五码顶字）
	if c.engineMgr != nil {
		commitText, newInput, shouldCommit := c.engineMgr.HandleTopCode(c.inputBuffer)
		if shouldCommit {
			// 记录输入历史（用于z键重复上屏），需在修改 inputBuffer 之前记录
			if c.inputHistory != nil {
				commitCode := c.inputBuffer[:len(c.inputBuffer)-len(newInput)]
				c.inputHistory.Record(commitText, commitCode, "", 0)
			}
			c.inputBuffer = newInput
			c.inputCursorPos = len(newInput)
			c.logger.Debug("Top code commit", "newInputLen", len(newInput))

			// 顶屏会让 C++ 端结束并重新开启 composition，复位 compositionStart 锁定
			// 与首字符诊断，使候选窗能在新 composition 的真实位置上重新定位。
			c.resetCompositionAnchorAfterCommit()

			// Apply full-width conversion if enabled
			if c.fullWidth {
				commitText = transform.ToFullWidth(commitText)
			}

			// 如果还有剩余输入，继续处理并更新候选
			if len(c.inputBuffer) > 0 {
				c.updateCandidates()
				// 顶屏会让 C++ 结束当前 composition 并立即重建一个新的，
				// 与首字符场景同样存在 reflow 漂移，先推迟 show 等真实坐标。
				c.armPendingFirstShow()

				// InlinePreedit 开启时，让 C++ 端原子地插入文字并开始新组合
				if c.config != nil && c.config.UI.InlinePreedit {
					return &bridge.KeyEventResult{
						Type:           bridge.ResponseTypeInsertText,
						Text:           commitText,
						NewComposition: c.inputBuffer,
					}
				}
				// InlinePreedit 关闭时，不发送 NewComposition（避免应用看到
				// 意外的 composition 文本而终止 composition），改为通过 push
				// pipe 发送空 UpdateComposition 来重建 composition。push pipe
				// 使用 PostMessageW 异步投递，会在 C++ 处理完 CommitText 同步
				// 响应后执行，时序安全。
				if c.bridgeServer != nil {
					c.bridgeServer.PushUpdateCompositionToActiveClient("", 0)
				}
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: commitText,
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
		// 记录输入历史（用于z键重复上屏），需在 clearState 之前记录
		if c.inputHistory != nil {
			c.inputHistory.Record(result.CommitText, c.inputBuffer, "", 0)
		}
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
	// 首字符触发 composition 创建：宿主侧 reflow 会让光标显著漂移，按键前的旧
	// 坐标不可靠。改为推迟，等 HandleCaretUpdate 收到 reflow 后的真实坐标再 show。
	// 非首字符则 inputBuffer 已有内容、composition 已存在，可立即用现有坐标显示。
	if wasComposingEmpty {
		c.armPendingFirstShow()
	} else {
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
	// 优先撤销确认段：当有已确认的分段时（如用户选了"我们"后剩余"de"），
	// 退格应回退上一步确认（恢复"womende"），而非删除缓冲末字符。
	// 这与主流拼音输入法（搜狗、百度、微软拼音）行为一致。
	if len(c.confirmedSegments) > 0 {
		lastSeg := c.confirmedSegments[len(c.confirmedSegments)-1]
		c.confirmedSegments = c.confirmedSegments[:len(c.confirmedSegments)-1]
		c.inputBuffer = lastSeg.ConsumedCode + c.inputBuffer
		c.inputCursorPos = len(c.inputBuffer) // 光标移到末尾
		c.logger.Debug("Backspace: undo confirmed segment",
			"restored", lastSeg.ConsumedCode, "buffer", c.inputBuffer,
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

	if len(c.inputBuffer) > 0 && c.inputCursorPos > 0 {
		// 无确认段时，在光标位置删除前一个字符
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
	// 回车 = 短语终止符，通知造词策略（码表自动造词）
	if c.engineMgr != nil {
		c.engineMgr.OnPhraseTerminated()
	}

	// Commit all confirmed segments + raw input as text
	if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		// 检查回车键行为配置：clear 模式下清空编码
		if c.config != nil && c.config.Input.EnterBehavior == "clear" {
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}

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
		// No candidates (空码), check space_on_empty_behavior config
		if c.config != nil && c.config.Input.SpaceOnEmptyBehavior == "clear" {
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}

		// Default: commit confirmed segments + raw input
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
	// 无编码空闲状态：全角模式下输出全角空格
	if c.fullWidth {
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: string(rune(0x3000)),
		}
	}
	return nil
}

// handleSelectChar 以词定字：从当前高亮候选词中取第 charIndex 个字符上屏（0-based）
func (c *Coordinator) handleSelectChar(charIndex int) *bridge.KeyEventResult {
	if len(c.candidates) == 0 || len(c.inputBuffer) == 0 {
		return nil
	}

	index := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
	if index >= len(c.candidates) {
		return nil
	}

	cand := c.candidates[index]
	runes := []rune(cand.Text)

	// 候选词长度不足时返回 nil，由调用方按 overflow 策略处理
	if charIndex >= len(runes) {
		return nil
	}

	text := string(runes[charIndex])
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 拼接已确认段的文本
	prefix := c.confirmedPrefix()
	if c.fullWidth && prefix != "" {
		prefix = transform.ToFullWidth(prefix)
	}

	// 记录输入历史
	if c.inputHistory != nil {
		c.inputHistory.Record(text, c.inputBuffer, "", 0)
	}

	// 用户词频学习
	if c.engineMgr != nil && !cand.IsCommand {
		selectedCode := c.inputBuffer
		if cand.Code != "" {
			selectedCode = cand.Code
		}
		c.engineMgr.OnCandidateSelected(selectedCode, cand.Text, cand.Source)
	}

	c.logger.Debug("Select char from word", "charIndex", charIndex, "char", text, "word", cand.Text)

	c.clearState()
	c.hideUI()

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: prefix + text,
	}
}

func (c *Coordinator) handleNumberKey(num int) *bridge.KeyEventResult {
	// num is 1-9 or 10 (key '0'), convert to 0-based index within current page
	pageStart := (c.currentPage - 1) * c.candidatesPerPage
	index := pageStart + (num - 1)

	// 计算当前页的有效候选数量
	pageEnd := pageStart + c.candidatesPerPage
	if pageEnd > len(c.candidates) {
		pageEnd = len(c.candidates)
	}
	currentPageCount := pageEnd - pageStart

	if num <= currentPageCount {
		return c.selectCandidate(index)
	}

	// 数字键无效：按 overflow_behavior.number_key 处理
	return c.handleOverflowNumberKey(num)
}

// handleOverflowNumberKey 处理数字键超出当前页候选范围的情况
func (c *Coordinator) handleOverflowNumberKey(num int) *bridge.KeyEventResult {
	if len(c.candidates) == 0 {
		return nil
	}

	behavior := "ignore"
	if c.config != nil && c.config.Input.OverflowBehavior.NumberKey != "" {
		behavior = c.config.Input.OverflowBehavior.NumberKey
	}

	highlightedIndex := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
	if highlightedIndex >= len(c.candidates) {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	switch behavior {
	case "commit":
		// 候选上屏：上屏当前高亮候选
		return c.selectCandidate(highlightedIndex)

	case "commit_and_input":
		// 顶码上屏：上屏当前高亮候选，然后数字字符直接输出
		result := c.selectCandidate(highlightedIndex)
		if result != nil {
			digit := string(rune('0' + num%10))
			if c.fullWidth {
				digit = transform.ToFullWidth(digit)
			}
			result.Text += digit
		}
		return result

	default: // "ignore"
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
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

		// 分级加载：翻到最后 2 页时预加载更多
		if c.hasMoreCandidates && c.currentPage >= c.totalPages-1 {
			c.expandCandidates()
		}

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
			// 使用候选的完整编码（前缀匹配时 cand.Code 与 inputBuffer 不同）
			selectedCode := c.inputBuffer
			if cand.Code != "" {
				selectedCode = cand.Code
			}
			c.engineMgr.OnCandidateSelected(selectedCode, originalText, cand.Source)
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

// handleOverflowSelectKey 处理二三候选键无效时的行为（候选数量不足或无候选）
// triggerKey 是触发按键的字符（如 ";", "'"）
func (c *Coordinator) handleOverflowSelectKey(triggerKey string) *bridge.KeyEventResult {
	if len(c.inputBuffer) == 0 {
		return nil
	}

	behavior := "ignore"
	if c.config != nil && c.config.Input.OverflowBehavior.SelectKey != "" {
		behavior = c.config.Input.OverflowBehavior.SelectKey
	}

	// 无候选时（空码）
	if len(c.candidates) == 0 {
		switch behavior {
		case "commit":
			// 候选上屏：无候选可上屏，清空缓冲区
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		case "commit_and_input":
			// 顶码上屏：清空缓冲区，按键字符按正常流程继续处理
			c.clearState()
			c.hideUI()
			if len(triggerKey) == 1 {
				ch := rune(triggerKey[0])
				if c.isPunctuation(ch) {
					return c.handlePunctuation(ch, false, 0)
				}
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: triggerKey,
				}
			}
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		default: // "ignore"
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
	}

	highlightedIndex := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
	if highlightedIndex >= len(c.candidates) {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	switch behavior {
	case "commit":
		// 候选上屏：上屏当前高亮候选
		return c.selectCandidate(highlightedIndex)
	case "commit_and_input":
		// 顶码上屏：上屏当前高亮候选，然后按键字符按正常流程处理
		result := c.selectCandidate(highlightedIndex)
		if result != nil && len(triggerKey) == 1 {
			ch := rune(triggerKey[0])
			if c.isPunctuation(ch) {
				punctResult := c.handlePunctuation(ch, false, 0)
				if punctResult != nil && punctResult.Text != "" {
					result.Text += punctResult.Text
				}
			} else {
				result.Text += triggerKey
			}
		}
		return result
	default: // "ignore"
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
}

// overflowSelectCharBehavior 返回以词定字键无效时的策略
func (c *Coordinator) overflowSelectCharBehavior() string {
	if c.config != nil && c.config.Input.OverflowBehavior.SelectCharKey != "" {
		return c.config.Input.OverflowBehavior.SelectCharKey
	}
	return "ignore"
}

// handleSelectCharWithOverflow 以词定字的完整处理流程，包含 overflow 策略
// charIndex: 取第几个字符(0-based)，key: 触发按键字符，prevDigitState/prevChar: 标点处理参数
func (c *Coordinator) handleSelectCharWithOverflow(charIndex int, key string, prevDigitState bool, prevChar rune) *bridge.KeyEventResult {
	// 正常以词定字
	if result := c.handleSelectChar(charIndex); result != nil {
		return result
	}

	// handleSelectChar 返回 nil 说明：无候选/候选词长度不足/无 inputBuffer
	if len(c.inputBuffer) == 0 {
		// 无输入缓冲时回退为标点处理
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]), prevDigitState, prevChar)
		}
		return nil
	}

	behavior := c.overflowSelectCharBehavior()

	// 无候选时（空码）
	if len(c.candidates) == 0 {
		switch behavior {
		case "commit":
			// 候选上屏：无候选可上屏，清空缓冲区
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		case "commit_and_input":
			// 顶码上屏：清空缓冲区，按键字符按正常流程继续处理
			c.clearState()
			c.hideUI()
			if len(key) == 1 {
				ch := rune(key[0])
				if c.isPunctuation(ch) {
					return c.handlePunctuation(ch, false, 0)
				}
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: key,
				}
			}
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		default: // "ignore"
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
	}

	highlightedIndex := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
	if highlightedIndex >= len(c.candidates) {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	switch behavior {
	case "commit":
		// 候选上屏：上屏当前高亮候选
		return c.selectCandidate(highlightedIndex)
	case "commit_and_input":
		// 顶码上屏：上屏当前高亮候选，然后按键字符按正常流程处理
		commitResult := c.selectCandidate(highlightedIndex)
		if commitResult != nil && len(key) == 1 {
			ch := rune(key[0])
			if c.isPunctuation(ch) {
				punctResult := c.handlePunctuation(ch, false, 0)
				if punctResult != nil && punctResult.Text != "" {
					commitResult.Text += punctResult.Text
				}
			} else {
				commitResult.Text += key
			}
		}
		return commitResult
	default: // "ignore"
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
}
