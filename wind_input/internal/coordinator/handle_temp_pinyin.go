// handle_temp_pinyin.go — 临时拼音模式（五笔引擎下通过触发键激活）
package coordinator

import (
	"strings"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
)

// isTempPinyinTrigger 检查按键是否应触发临时拼音模式
func (c *Coordinator) isTempPinyinTrigger(key string, keyCode int) bool {
	// 仅五笔引擎下生效
	if c.engineMgr == nil || c.engineMgr.GetCurrentType() != engine.EngineTypeWubi {
		return false
	}
	// 仅输入缓冲区为空时触发
	if len(c.inputBuffer) > 0 {
		return false
	}
	if c.config == nil {
		return false
	}

	for _, tk := range c.config.Input.TempPinyin.TriggerKeys {
		switch tk {
		case "backtick":
			if key == "`" || uint32(keyCode) == ipc.VK_OEM_3 {
				return true
			}
		case "semicolon":
			// 仅在输入缓冲区为空且无候选时触发
			// 有候选时 semicolon 仍用于二三候选选择
			if (key == ";" || uint32(keyCode) == ipc.VK_OEM_1) && len(c.candidates) == 0 {
				return true
			}
		}
	}
	return false
}

// isTempPinyinTriggerKeyMatch 仅检查按键是否匹配临时拼音触发键（不检查状态条件）
func (c *Coordinator) isTempPinyinTriggerKeyMatch(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}
	for _, tk := range c.config.Input.TempPinyin.TriggerKeys {
		switch tk {
		case "backtick":
			if key == "`" || uint32(keyCode) == ipc.VK_OEM_3 {
				return true
			}
		case "semicolon":
			if key == ";" || uint32(keyCode) == ipc.VK_OEM_1 {
				return true
			}
		}
	}
	return false
}

// enterTempPinyinMode 进入临时拼音模式
func (c *Coordinator) enterTempPinyinMode() *bridge.KeyEventResult {
	// 确保拼音引擎已加载
	if c.engineMgr != nil {
		if err := c.engineMgr.EnsurePinyinLoaded(); err != nil {
			c.logger.Warn("Failed to load pinyin engine for temp pinyin", "error", err)
			return nil
		}
	}

	c.tempPinyinMode = true
	c.tempPinyinBuffer = ""

	c.logger.Debug("Entered temp pinyin mode")

	// 显示初始 UI（仅前缀标识）
	c.showTempPinyinUI()

	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     "`",
		CaretPos: 1,
	}
}

// handleTempPinyinKey 处理临时拼音模式下的按键
func (c *Coordinator) handleTempPinyinKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	switch {
	case len(key) == 1 && key[0] >= 'a' && key[0] <= 'z':
		// 追加字母到拼音缓冲区
		c.tempPinyinBuffer += key
		c.updateTempPinyinCandidates()
		c.showTempPinyinUI()
		preedit := "`" + c.preeditDisplay
		if c.preeditDisplay == "" {
			preedit = "`" + c.tempPinyinBuffer
		}
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     preedit,
			CaretPos: len(preedit),
		}

	case len(key) == 1 && key[0] >= 'A' && key[0] <= 'Z':
		// 大写字母也转小写处理
		c.tempPinyinBuffer += strings.ToLower(key)
		c.updateTempPinyinCandidates()
		c.showTempPinyinUI()
		preedit := "`" + c.preeditDisplay
		if c.preeditDisplay == "" {
			preedit = "`" + c.tempPinyinBuffer
		}
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     preedit,
			CaretPos: len(preedit),
		}

	case len(key) == 1 && key[0] >= '1' && key[0] <= '9':
		// 数字选择候选
		idx := int(key[0]-'0') - 1
		pageStart := (c.currentPage - 1) * c.candidatesPerPage
		globalIdx := pageStart + idx
		if globalIdx < len(c.candidates) {
			return c.selectTempPinyinCandidate(globalIdx)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case uint32(data.KeyCode) == ipc.VK_SPACE:
		// 选择第一个候选
		if len(c.candidates) > 0 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			return c.selectTempPinyinCandidate(pageStart)
		}
		// 无候选时退出
		return c.exitTempPinyinMode(false, "")

	case uint32(data.KeyCode) == ipc.VK_RETURN:
		// 上屏拼音原文字母
		if len(c.tempPinyinBuffer) > 0 {
			return c.exitTempPinyinMode(true, c.tempPinyinBuffer)
		}
		return c.exitTempPinyinMode(false, "")

	case uint32(data.KeyCode) == ipc.VK_BACK:
		if len(c.tempPinyinBuffer) > 0 {
			c.tempPinyinBuffer = c.tempPinyinBuffer[:len(c.tempPinyinBuffer)-1]
			if len(c.tempPinyinBuffer) == 0 {
				return c.exitTempPinyinMode(false, "")
			}
			c.currentPage = 1
			c.updateTempPinyinCandidates()
			c.showTempPinyinUI()
			preedit := "`" + c.preeditDisplay
			if c.preeditDisplay == "" {
				preedit = "`" + c.tempPinyinBuffer
			}
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     preedit,
				CaretPos: len(preedit),
			}
		}
		return c.exitTempPinyinMode(false, "")

	case uint32(data.KeyCode) == ipc.VK_ESCAPE:
		return c.exitTempPinyinMode(false, "")

	case c.isPageUpKey(key, data.KeyCode, uint32(data.Modifiers)):
		if c.currentPage > 1 {
			c.currentPage--
			c.showTempPinyinUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case c.isPageDownKey(key, data.KeyCode, uint32(data.Modifiers)):
		if c.currentPage < c.totalPages {
			c.currentPage++
			c.showTempPinyinUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case c.isTempPinyinTriggerKeyMatch(key, data.KeyCode):
		// 再次按下触发键：缓冲区为空时输出对应的标点符号
		if len(c.tempPinyinBuffer) == 0 {
			triggerChar := key
			if len(triggerChar) == 1 {
				punctText := triggerChar
				if c.chinesePunctuation {
					if converted, ok := c.punctConverter.ToChinesePunctStr(rune(triggerChar[0])); ok {
						punctText = converted
					}
				}
				if c.fullWidth {
					punctText = transform.ToFullWidth(punctText)
				}
				return c.exitTempPinyinMode(true, punctText)
			}
		}
		// 缓冲区有内容时，按标点处理（同 default 逻辑）
		if len(c.candidates) > 0 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			cand := c.candidates[pageStart]
			text := cand.Text
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}

			c.tempPinyinMode = false
			c.tempPinyinBuffer = ""
			c.preeditDisplay = ""
			c.candidates = nil
			c.currentPage = 1
			c.totalPages = 1
			c.hideUI()

			punctText := key
			if len(key) == 1 && c.chinesePunctuation {
				if converted, ok := c.punctConverter.ToChinesePunctStr(rune(key[0])); ok {
					punctText = converted
				}
			}
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text + punctText,
			}
		}
		return c.exitTempPinyinMode(false, "")

	default:
		// 其他按键（如标点）
		if len(c.candidates) > 0 {
			// 有候选时：选第一个候选后处理标点
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			cand := c.candidates[pageStart]
			text := cand.Text
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}

			c.tempPinyinMode = false
			c.tempPinyinBuffer = ""
			c.preeditDisplay = ""
			c.candidates = nil
			c.currentPage = 1
			c.totalPages = 1
			c.hideUI()

			// 处理标点
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

		// 无候选时退出
		return c.exitTempPinyinMode(false, "")
	}
}

// updateTempPinyinCandidates 更新临时拼音候选
func (c *Coordinator) updateTempPinyinCandidates() {
	if c.engineMgr == nil || len(c.tempPinyinBuffer) == 0 {
		c.candidates = nil
		c.preeditDisplay = ""
		c.totalPages = 1
		return
	}

	maxCandidates := 100
	result := c.engineMgr.ConvertWithPinyin(c.tempPinyinBuffer, maxCandidates)

	// 转换为 ui.Candidate 列表
	uiCandidates := make([]ui.Candidate, len(result.Candidates))
	for i, cand := range result.Candidates {
		uiCandidates[i] = ui.Candidate{
			Text:           cand.Text,
			Index:          i + 1,
			Comment:        cand.Hint, // 五笔编码作为 Comment 显示
			Weight:         cand.Weight,
			ConsumedLength: cand.ConsumedLength,
		}
	}

	c.candidates = uiCandidates
	c.preeditDisplay = result.PreeditDisplay

	// 计算分页
	if c.candidatesPerPage <= 0 {
		c.candidatesPerPage = 7
	}
	total := len(c.candidates)
	c.totalPages = (total + c.candidatesPerPage - 1) / c.candidatesPerPage
	if c.totalPages < 1 {
		c.totalPages = 1
	}
	if c.currentPage > c.totalPages {
		c.currentPage = c.totalPages
	}
	if c.currentPage < 1 {
		c.currentPage = 1
	}
}

// selectTempPinyinCandidate 选择临时拼音候选
func (c *Coordinator) selectTempPinyinCandidate(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	cand := c.candidates[index]
	text := cand.Text
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 支持拼音部分上屏
	if cand.ConsumedLength > 0 && cand.ConsumedLength < len(c.tempPinyinBuffer) {
		remaining := c.tempPinyinBuffer[cand.ConsumedLength:]
		c.tempPinyinBuffer = remaining
		c.currentPage = 1
		c.updateTempPinyinCandidates()
		c.showTempPinyinUI()

		preedit := "`" + c.preeditDisplay
		if c.preeditDisplay == "" {
			preedit = "`" + c.tempPinyinBuffer
		}
		return &bridge.KeyEventResult{
			Type:           bridge.ResponseTypeInsertText,
			Text:           text,
			NewComposition: preedit,
		}
	}

	// 全部上屏，退出临时拼音模式
	return c.exitTempPinyinMode(true, text)
}

// exitTempPinyinMode 退出临时拼音模式
func (c *Coordinator) exitTempPinyinMode(commit bool, text string) *bridge.KeyEventResult {
	c.tempPinyinMode = false
	c.tempPinyinBuffer = ""
	c.preeditDisplay = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	c.hideUI()

	c.logger.Debug("Exited temp pinyin mode", "commit", commit, "textLen", len(text))

	if commit && len(text) > 0 {
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// showTempPinyinUI 显示临时拼音模式 UI
func (c *Coordinator) showTempPinyinUI() {
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

	// 获取当前页候选
	startIdx := (c.currentPage - 1) * c.candidatesPerPage
	endIdx := startIdx + c.candidatesPerPage
	if endIdx > len(c.candidates) {
		endIdx = len(c.candidates)
	}

	var pageCandidates []ui.Candidate
	if startIdx < len(c.candidates) {
		pageCandidates = c.candidates[startIdx:endIdx]
	}

	// 重新编号显示（1-9）
	displayCandidates := make([]ui.Candidate, len(pageCandidates))
	for i, cand := range pageCandidates {
		displayCandidates[i] = ui.Candidate{
			Text:    cand.Text,
			Index:   i + 1,
			Comment: cand.Comment,
			Weight:  cand.Weight,
		}
	}

	// 构建预编辑文本
	preedit := "`" + c.preeditDisplay
	if c.preeditDisplay == "" && c.tempPinyinBuffer != "" {
		preedit = "`" + c.tempPinyinBuffer
	} else if c.tempPinyinBuffer == "" {
		preedit = "`"
	}

	c.uiManager.ShowCandidates(
		displayCandidates,
		preedit,
		len(preedit),
		caretX,
		caretY,
		caretHeight,
		c.currentPage,
		c.totalPages,
	)
}
