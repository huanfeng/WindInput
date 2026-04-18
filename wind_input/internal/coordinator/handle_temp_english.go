// handle_temp_english.go — 临时英文模式（Shift+字母 / 触发键进入）
package coordinator

import (
	"strings"
	"unicode"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/transform"
)

// ─── 大小写模式 ───

type englishCasePattern int

const (
	caseLower englishCasePattern = iota // 全小写: hello
	caseUpper                           // 全大写: HELLO
	caseTitle                           // 首字母大写: Hello
	caseMixed                           // 混合: hEllo, HeLLO
)

func detectCasePattern(s string) englishCasePattern {
	if s == "" {
		return caseLower
	}
	runes := []rune(s)
	allLower := true
	allUpper := true
	for _, r := range runes {
		if unicode.IsUpper(r) {
			allLower = false
		}
		if unicode.IsLower(r) {
			allUpper = false
		}
	}
	if allLower {
		return caseLower
	}
	if allUpper {
		return caseUpper
	}
	if unicode.IsUpper(runes[0]) {
		lower := true
		for _, r := range runes[1:] {
			if unicode.IsUpper(r) {
				lower = false
				break
			}
		}
		if lower {
			return caseTitle
		}
	}
	return caseMixed
}

// adaptCase 将词库单词适配为用户输入的大小写模式
func adaptCase(word string, pattern englishCasePattern) string {
	switch pattern {
	case caseUpper:
		return strings.ToUpper(word)
	case caseTitle:
		if len(word) == 0 {
			return word
		}
		runes := []rune(strings.ToLower(word))
		runes[0] = unicode.ToUpper(runes[0])
		return string(runes)
	case caseLower:
		return strings.ToLower(word)
	default: // caseMixed: 保留词库原始大小写
		return word
	}
}

// generateCaseVariants 生成用户输入的大小写变体（不含输入本身）
func generateCaseVariants(input string) []string {
	if input == "" {
		return nil
	}
	pattern := detectCasePattern(input)
	lower := strings.ToLower(input)
	upper := strings.ToUpper(input)
	runes := []rune(lower)
	runes[0] = unicode.ToUpper(runes[0])
	title := string(runes)

	var variants []string
	switch pattern {
	case caseLower:
		// 输入全小写 → 首字母大写, 全大写
		variants = append(variants, title, upper)
	case caseTitle:
		// 首字母大写 → 全小写, 全大写
		variants = append(variants, lower, upper)
	case caseUpper:
		// 全大写 → 全小写, 首字母大写
		variants = append(variants, lower, title)
	case caseMixed:
		// 混合大小写 → 全小写, 首字母大写, 全大写
		variants = append(variants, lower, title, upper)
	}

	// 去除与原始输入相同的
	var result []string
	for _, v := range variants {
		if v != input {
			result = append(result, v)
		}
	}
	return result
}

// ─── 进入/退出 ───

// enterTempEnglishMode 进入临时英文模式（Shift+字母触发）
func (c *Coordinator) enterTempEnglishMode(key string) *bridge.KeyEventResult {
	c.tempEnglishMode = true
	c.tempEnglishBuffer = strings.ToUpper(key) // Shift+字母输出大写
	c.tempEnglishCursorPos = len(c.tempEnglishBuffer)

	if c.config != nil && c.config.Input.ShiftTempEnglish.ShowEnglishCandidates && c.engineMgr != nil {
		c.engineMgr.EnsureEnglishLoaded()
	}

	c.logger.Debug("Entered temp English mode", "buffer", c.tempEnglishBuffer)
	c.updateTempEnglishCandidates()
	c.showTempEnglishUI()

	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     c.tempEnglishBuffer,
		CaretPos: c.tempEnglishCursorPos,
	}
}

// enterTempEnglishModeWithTrigger 通过触发键进入临时英文模式
func (c *Coordinator) enterTempEnglishModeWithTrigger(triggerKey string) *bridge.KeyEventResult {
	c.tempEnglishMode = true
	c.tempEnglishBuffer = ""
	c.tempEnglishCursorPos = 0

	if c.config != nil && c.config.Input.ShiftTempEnglish.ShowEnglishCandidates && c.engineMgr != nil {
		c.engineMgr.EnsureEnglishLoaded()
	}

	c.logger.Debug("Entered temp English mode via trigger key", "triggerKey", triggerKey)
	c.showTempEnglishUI()

	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     "",
		CaretPos: 0,
	}
}

// exitTempEnglishMode 退出临时英文模式
func (c *Coordinator) exitTempEnglishMode(commit bool, text string) *bridge.KeyEventResult {
	c.tempEnglishMode = false
	c.tempEnglishBuffer = ""
	c.tempEnglishCursorPos = 0
	c.tempEnglishCandidates = nil
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	c.selectedIndex = 0
	if c.uiManager != nil {
		c.uiManager.SetModeLabel("")
	}
	c.hideUI()

	c.logger.Debug("Exited temp English mode", "commit", commit, "textLen", len(text))

	if commit && len(text) > 0 {
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// ─── 按键处理 ───

func (c *Coordinator) handleTempEnglishKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	hasShift := data.Modifiers&ModShift != 0
	vk := uint32(data.KeyCode)

	switch {
	case vk == ipc.VK_BACK:
		if c.tempEnglishCursorPos > 0 && len(c.tempEnglishBuffer) > 0 {
			// 在光标位置删除前一个字符
			runes := []rune(c.tempEnglishBuffer)
			pos := c.tempEnglishCursorPos
			if pos > len(runes) {
				pos = len(runes)
			}
			runes = append(runes[:pos-1], runes[pos:]...)
			c.tempEnglishBuffer = string(runes)
			c.tempEnglishCursorPos = pos - 1
			if len(c.tempEnglishBuffer) == 0 {
				return c.exitTempEnglishMode(false, "")
			}
			c.updateTempEnglishCandidates()
			c.showTempEnglishUI()
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.tempEnglishBuffer,
				CaretPos: c.tempEnglishCursorPos,
			}
		}
		if len(c.tempEnglishBuffer) == 0 {
			return c.exitTempEnglishMode(false, "")
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}

	case vk == ipc.VK_DELETE:
		runes := []rune(c.tempEnglishBuffer)
		if c.tempEnglishCursorPos < len(runes) {
			runes = append(runes[:c.tempEnglishCursorPos], runes[c.tempEnglishCursorPos+1:]...)
			c.tempEnglishBuffer = string(runes)
			if len(c.tempEnglishBuffer) == 0 {
				return c.exitTempEnglishMode(false, "")
			}
			c.updateTempEnglishCandidates()
			c.showTempEnglishUI()
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.tempEnglishBuffer,
				CaretPos: c.tempEnglishCursorPos,
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case vk == ipc.VK_ESCAPE:
		return c.exitTempEnglishMode(false, "")

	case vk == ipc.VK_SPACE:
		// 有候选时选择当前高亮候选（首候选=用户输入本身）
		if len(c.candidates) > 0 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			absIdx := pageStart + c.selectedIndex
			if absIdx < len(c.candidates) {
				return c.exitTempEnglishMode(true, c.candidates[absIdx].Text)
			}
		}
		return c.exitTempEnglishMode(true, c.tempEnglishBuffer)

	case vk == ipc.VK_RETURN:
		return c.exitTempEnglishMode(true, c.tempEnglishBuffer)

	// === 左右光标移动 ===
	case vk == ipc.VK_LEFT:
		if c.tempEnglishCursorPos > 0 {
			c.tempEnglishCursorPos--
			c.showTempEnglishUI()
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.tempEnglishBuffer,
				CaretPos: c.tempEnglishCursorPos,
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case vk == ipc.VK_RIGHT:
		if c.tempEnglishCursorPos < len(c.tempEnglishBuffer) {
			c.tempEnglishCursorPos++
			c.showTempEnglishUI()
			return &bridge.KeyEventResult{
				Type:     bridge.ResponseTypeUpdateComposition,
				Text:     c.tempEnglishBuffer,
				CaretPos: c.tempEnglishCursorPos,
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case vk == ipc.VK_HOME:
		c.tempEnglishCursorPos = 0
		c.showTempEnglishUI()
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.tempEnglishBuffer,
			CaretPos: 0,
		}

	case vk == ipc.VK_END:
		c.tempEnglishCursorPos = len(c.tempEnglishBuffer)
		c.showTempEnglishUI()
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.tempEnglishBuffer,
			CaretPos: c.tempEnglishCursorPos,
		}

	// === 翻页（使用与正常模式一致的配置键） ===
	case c.isPageUpKey(key, int(vk), uint32(data.Modifiers)):
		if c.currentPage > 1 {
			c.currentPage--
			c.selectedIndex = 0
			c.showTempEnglishUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case c.isPageDownKey(key, int(vk), uint32(data.Modifiers)):
		if c.currentPage < c.totalPages {
			c.currentPage++
			c.selectedIndex = 0
			c.showTempEnglishUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 高亮移动（使用与正常模式一致的配置键） ===
	case c.isHighlightUpKey(vk, uint32(data.Modifiers)):
		if len(c.candidates) > 0 {
			if c.selectedIndex > 0 {
				c.selectedIndex--
			} else if c.currentPage > 1 {
				c.currentPage--
				startIdx := (c.currentPage - 1) * c.candidatesPerPage
				endIdx := min(startIdx+c.candidatesPerPage, len(c.candidates))
				c.selectedIndex = endIdx - startIdx - 1
			}
			c.showTempEnglishUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case c.isHighlightDownKey(vk, uint32(data.Modifiers)):
		if len(c.candidates) > 0 {
			startIdx := (c.currentPage - 1) * c.candidatesPerPage
			endIdx := min(startIdx+c.candidatesPerPage, len(c.candidates))
			pageCount := endIdx - startIdx
			if c.selectedIndex < pageCount-1 {
				c.selectedIndex++
			} else if c.currentPage < c.totalPages {
				c.currentPage++
				c.selectedIndex = 0
			}
			c.showTempEnglishUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 二候选选择键 ===
	case data.Modifiers&ModShift == 0 && c.isSelectKey2(key, data.KeyCode):
		if len(c.candidates) >= 2 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			idx := pageStart + 1
			if idx < len(c.candidates) {
				return c.exitTempEnglishMode(true, c.candidates[idx].Text)
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 三候选选择键 ===
	case data.Modifiers&ModShift == 0 && c.isSelectKey3(key, data.KeyCode):
		if len(c.candidates) >= 3 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			idx := pageStart + 2
			if idx < len(c.candidates) {
				return c.exitTempEnglishMode(true, c.candidates[idx].Text)
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 字母键 ===
	case len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')):
		var letter string
		if hasShift {
			letter = strings.ToUpper(key)
		} else {
			letter = strings.ToLower(key)
		}
		// 在光标位置插入
		runes := []rune(c.tempEnglishBuffer)
		pos := c.tempEnglishCursorPos
		newRunes := make([]rune, 0, len(runes)+1)
		newRunes = append(newRunes, runes[:pos]...)
		newRunes = append(newRunes, []rune(letter)...)
		newRunes = append(newRunes, runes[pos:]...)
		c.tempEnglishBuffer = string(newRunes)
		c.tempEnglishCursorPos = pos + len([]rune(letter))

		c.updateTempEnglishCandidates()
		c.showTempEnglishUI()
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.tempEnglishBuffer,
			CaretPos: c.tempEnglishCursorPos,
		}

	// === 数字键 1-9：选择当前页候选 ===
	case len(key) == 1 && key[0] >= '1' && key[0] <= '9':
		idx := int(key[0] - '1')
		pageStart := (c.currentPage - 1) * c.candidatesPerPage
		absIdx := pageStart + idx
		if absIdx < len(c.candidates) {
			return c.exitTempEnglishMode(true, c.candidates[absIdx].Text)
		}
		// 无对应候选，上屏缓冲+数字
		if len(c.tempEnglishBuffer) > 0 {
			text := c.tempEnglishBuffer
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}
			c.tempEnglishMode = false
			c.tempEnglishBuffer = ""
			c.tempEnglishCursorPos = 0
			c.tempEnglishCandidates = nil
			c.candidates = nil
			c.hideUI()
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text + key,
			}
		}
		c.exitTempEnglishMode(false, "")
		return nil

	// === 数字键 0 ===
	case len(key) == 1 && key[0] == '0':
		if len(c.tempEnglishBuffer) > 0 {
			text := c.tempEnglishBuffer
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}
			c.tempEnglishMode = false
			c.tempEnglishBuffer = ""
			c.tempEnglishCursorPos = 0
			c.tempEnglishCandidates = nil
			c.candidates = nil
			c.hideUI()
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text + key,
			}
		}
		c.exitTempEnglishMode(false, "")
		return nil
	}

	// 其他按键（如标点）：上屏当前高亮候选+标点
	if len(c.tempEnglishBuffer) > 0 {
		// 取当前高亮候选（与空格上屏逻辑一致）
		text := c.tempEnglishBuffer
		if len(c.candidates) > 0 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			absIdx := pageStart + c.selectedIndex
			if absIdx < len(c.candidates) {
				text = c.candidates[absIdx].Text
			}
		}
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}
		punctText := ""
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			punctResult := c.handlePunctuation(rune(key[0]), false, 0)
			if punctResult != nil {
				punctText = punctResult.Text
			}
		}
		return c.exitTempEnglishMode(true, text+punctText)
	}

	c.exitTempEnglishMode(false, "")
	return nil
}

// ─── 候选更新 ───

// updateTempEnglishCandidates 更新临时英文模式的候选列表
// 逻辑：
//  1. 首候选始终是用户输入的原文（方便空格直接上屏）
//  2. 词库前缀匹配的候选（适配用户的大小写模式）
//  3. 无词库匹配时，生成大小写变体作为候选
func (c *Coordinator) updateTempEnglishCandidates() {
	buf := c.tempEnglishBuffer
	if buf == "" {
		c.tempEnglishCandidates = nil
		c.candidates = nil
		c.currentPage = 1
		c.totalPages = 1
		c.selectedIndex = 0
		return
	}

	showCandidates := c.config != nil && c.config.Input.ShiftTempEnglish.ShowEnglishCandidates
	casePattern := detectCasePattern(buf)
	bufLower := strings.ToLower(buf)

	var allCandidates []candidate.Candidate

	// 1. 首候选：用户输入的原文
	allCandidates = append(allCandidates, candidate.Candidate{
		Text: buf,
		Code: bufLower,
	})

	// 2. 词库候选
	seen := map[string]bool{bufLower: true} // 首候选已占用
	if showCandidates && c.engineMgr != nil {
		results := c.engineMgr.SearchEnglish(bufLower, c.candidatesPerPage*5)
		for _, cand := range results {
			lower := strings.ToLower(cand.Text)
			if seen[lower] {
				continue
			}
			seen[lower] = true
			// 大小写适配：仅对词库中全小写的词进行适配（hello→Hello）
			// 已有大写的专有词（DHCP、iPhone、Aaron）保持原样
			displayText := cand.Text
			if casePattern != caseLower && displayText == lower {
				displayText = adaptCase(displayText, casePattern)
			}
			allCandidates = append(allCandidates, candidate.Candidate{
				Text:   displayText,
				Code:   lower,
				Weight: cand.Weight,
			})
		}
	}

	// 3. 大小写变体（当词库候选较少时补充）
	// 注意：变体与用户输入的小写形式相同，不能用 seen（按小写去重）过滤
	// 改用 seenText（按原始文本去重）
	if len(allCandidates) <= 1 {
		variants := generateCaseVariants(buf)
		for _, v := range variants {
			allCandidates = append(allCandidates, candidate.Candidate{
				Text: v,
				Code: bufLower,
			})
		}
	}

	c.tempEnglishCandidates = allCandidates
	c.candidates = allCandidates
	c.currentPage = 1
	c.selectedIndex = 0
	if len(allCandidates) > 0 {
		c.totalPages = (len(allCandidates) + c.candidatesPerPage - 1) / c.candidatesPerPage
	} else {
		c.totalPages = 1
	}
}

// ─── UI 显示 ───

// showTempEnglishUI 显示临时英文模式的 UI
// 分页逻辑与 showPinyinModeUI 一致
func (c *Coordinator) showTempEnglishUI() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	caretX := c.caretX
	caretY := c.caretY
	caretHeight := c.caretHeight
	if c.config != nil && c.config.UI.InlinePreedit && c.compositionStartValid {
		caretX = c.compositionStartX
		caretY = c.compositionStartY
	}

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

	// 分页计算
	startIdx := (c.currentPage - 1) * c.candidatesPerPage
	endIdx := min(startIdx+c.candidatesPerPage, len(c.candidates))

	var pageCandidates []candidate.Candidate
	if startIdx < len(c.candidates) {
		pageCandidates = c.candidates[startIdx:endIdx]
	}

	// 设置数字编号
	displayCandidates := make([]candidate.Candidate, len(pageCandidates))
	copy(displayCandidates, pageCandidates)
	for i := range displayCandidates {
		displayCandidates[i].Index = (i + 1) % 10
	}

	c.uiManager.SetModeLabel("临时英文")
	c.uiManager.ShowCandidates(
		displayCandidates,
		c.tempEnglishBuffer,
		c.tempEnglishCursorPos,
		caretX,
		caretY,
		caretHeight,
		c.currentPage,
		c.totalPages,
		len(c.candidates),
		c.candidatesPerPage,
		c.selectedIndex,
	)
}

// ─── 触发键 ───

// getTempEnglishTriggerKey 检查按键是否应触发临时英文模式
func (c *Coordinator) getTempEnglishTriggerKey(key string, keyCode int) string {
	if c.config == nil || !c.config.Input.ShiftTempEnglish.Enabled {
		return ""
	}
	if len(c.inputBuffer) > 0 || len(c.candidates) > 0 {
		return ""
	}

	triggerKeys := c.config.Input.ShiftTempEnglish.TriggerKeys
	if len(triggerKeys) == 0 {
		return ""
	}

	for _, tk := range triggerKeys {
		switch tk {
		case "backtick":
			if key == "`" {
				return tk
			}
		case "semicolon":
			if key == ";" {
				return tk
			}
		case "slash":
			if key == "/" {
				return tk
			}
		case "backslash":
			if key == "\\" {
				return tk
			}
		}
	}
	return ""
}
