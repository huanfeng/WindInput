// handle_temp_english.go — 临时英文模式（Shift+字母触发）
package coordinator

import (
	"strings"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/transform"
)

// enterTempEnglishMode 进入临时英文模式
func (c *Coordinator) enterTempEnglishMode(key string) *bridge.KeyEventResult {
	c.tempEnglishMode = true
	c.tempEnglishBuffer = strings.ToUpper(key) // Shift+字母输出大写

	c.logger.Debug("Entered temp English mode", "buffer", c.tempEnglishBuffer)

	// 显示临时英文模式 UI
	c.showTempEnglishUI()

	// 返回 UpdateComposition 让 C++ 端知道进入了 composing 状态
	// 这样后续的 Backspace/Enter 才会被发送到 Go 端处理
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     c.tempEnglishBuffer,
		CaretPos: len(c.tempEnglishBuffer),
	}
}

// handleTempEnglishKey 处理临时英文模式下的按键
func (c *Coordinator) handleTempEnglishKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	hasShift := data.Modifiers&ModShift != 0

	switch {
	case uint32(data.KeyCode) == ipc.VK_BACK:
		if len(c.tempEnglishBuffer) > 0 {
			c.tempEnglishBuffer = c.tempEnglishBuffer[:len(c.tempEnglishBuffer)-1]
			if len(c.tempEnglishBuffer) == 0 {
				return c.exitTempEnglishMode(false, "")
			}
			c.showTempEnglishUI()
			// 返回 UpdateComposition 保持 composing 状态
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.tempEnglishBuffer,
				CaretPos: len(c.tempEnglishBuffer),
			}
		}
		// 缓冲区已空，返回 ClearComposition
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}

	case uint32(data.KeyCode) == ipc.VK_ESCAPE:
		return c.exitTempEnglishMode(false, "")

	case uint32(data.KeyCode) == ipc.VK_SPACE:
		// 上屏缓冲内容
		text := c.tempEnglishBuffer
		return c.exitTempEnglishMode(true, text)

	case uint32(data.KeyCode) == ipc.VK_RETURN:
		// Enter 也上屏缓冲内容
		return c.exitTempEnglishMode(true, c.tempEnglishBuffer)

	case len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')):
		// 追加字母：Shift+字母=大写，字母=小写
		var letter string
		if hasShift {
			letter = strings.ToUpper(key)
		} else {
			letter = strings.ToLower(key)
		}
		c.tempEnglishBuffer += letter
		c.logger.Debug("Temp English buffer updated", "buffer", c.tempEnglishBuffer)
		c.showTempEnglishUI()
		// 返回 UpdateComposition 保持 composing 状态
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.tempEnglishBuffer,
			CaretPos: len(c.tempEnglishBuffer),
		}

	case len(key) == 1 && key[0] >= '0' && key[0] <= '9':
		// 数字：当前没有英文候选，上屏缓冲内容并输出数字
		// （如果将来有英文词库/候选，数字应该用于选择候选）
		if len(c.tempEnglishBuffer) > 0 {
			text := c.tempEnglishBuffer
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}
			c.tempEnglishMode = false
			c.tempEnglishBuffer = ""
			c.hideUI()
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text + key,
			}
		}
		// 缓冲区为空，退出并透传数字
		c.exitTempEnglishMode(false, "")
		return nil
	}

	// 其他按键（如标点）：上屏缓冲内容，然后处理按键
	if len(c.tempEnglishBuffer) > 0 {
		text := c.tempEnglishBuffer
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}
		c.tempEnglishMode = false
		c.tempEnglishBuffer = ""
		c.hideUI()
		// 如果是标点，处理标点
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			punctResult := c.handlePunctuation(rune(key[0]))
			if punctResult != nil {
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: text + punctResult.Text,
				}
			}
		}
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	// 缓冲区为空，退出临时英文模式并透传
	c.exitTempEnglishMode(false, "")
	return nil
}

// exitTempEnglishMode 退出临时英文模式
func (c *Coordinator) exitTempEnglishMode(commit bool, text string) *bridge.KeyEventResult {
	c.tempEnglishMode = false
	c.tempEnglishBuffer = ""
	c.candidates = nil
	c.currentPage = 0
	c.totalPages = 0
	c.hideUI()

	c.logger.Debug("Exited temp English mode", "commit", commit, "textLen", len(text))

	if commit && len(text) > 0 {
		// 应用全角转换（如果启用）
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	// 取消时返回 ClearComposition 让 C++ 端清除 composing 状态
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// showTempEnglishUI 显示临时英文模式的 UI
func (c *Coordinator) showTempEnglishUI() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// 使用光标位置
	caretX := c.caretX
	caretY := c.caretY
	caretHeight := c.caretHeight

	const maxCoord = 32000
	if (c.caretX == 0 && c.caretY == 0) || caretX > maxCoord || caretX < -maxCoord || caretY > maxCoord || caretY < -maxCoord {
		if c.lastValidX != 0 || c.lastValidY != 0 {
			caretX = c.lastValidX
			caretY = c.lastValidY
			caretHeight = 20
		} else {
			caretX = 400
			caretY = 300
			caretHeight = 20
		}
	}

	// 显示临时英文缓冲区内容（无候选词）
	c.uiManager.ShowCandidates(
		nil, // 无候选词
		c.tempEnglishBuffer,
		len(c.tempEnglishBuffer), // 光标在末尾
		caretX,
		caretY,
		caretHeight,
		1, // currentPage
		1, // totalPages
		0, // totalCandidateCount
		0, // candidatesPerPage
	)
}
