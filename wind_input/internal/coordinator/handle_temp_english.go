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
	// 首次进入触发 C++ 端 StartComposition，同步标记 pendingFirstShow，
	// 让 Excel/WPS 表格 cell-select→cell-edit 的失焦能命中 replay 路径。
	// 不立即 showUI：等 OnLayoutChange 真实坐标由 HandleCaretUpdate 触发首次显示，
	// 否则会先用按键前的旧坐标显示再跳到正确位置（与 handleAlphaKey 首字符一致）。
	c.armPendingFirstShow()

	return c.tempEnglishCompositionResult()
}

// enterTempEnglishModeWithTrigger 通过触发键进入临时英文模式
func (c *Coordinator) enterTempEnglishModeWithTrigger(triggerKey string) *bridge.KeyEventResult {
	c.tempEnglishMode = true
	c.tempEnglishTriggerKey = triggerKey
	c.tempEnglishBuffer = ""
	c.tempEnglishCursorPos = 0

	if c.config != nil && c.config.Input.ShiftTempEnglish.ShowEnglishCandidates && c.engineMgr != nil {
		c.engineMgr.EnsureEnglishLoaded()
	}

	c.logger.Debug("Entered temp English mode via trigger key", "triggerKey", triggerKey)
	c.armPendingFirstShow()

	return c.tempEnglishCompositionResult()
}

// exitTempEnglishMode 退出临时英文模式
func (c *Coordinator) exitTempEnglishMode(commit bool, text string) *bridge.KeyEventResult {
	c.tempEnglishMode = false
	c.tempEnglishTriggerKey = ""
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
			return c.tempEnglishCompositionResult()
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
			return c.tempEnglishCompositionResult()
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
		if len(c.tempEnglishBuffer) > 0 {
			return c.exitTempEnglishMode(true, c.tempEnglishBuffer)
		}
		// 缓冲区为空时（触发键进入后直接回车），上屏触发键字符
		return c.exitTempEnglishMode(true, c.tempEnglishTriggerPrefix())

	// === 左右光标移动 ===
	case vk == ipc.VK_LEFT:
		if c.tempEnglishCursorPos > 0 {
			c.tempEnglishCursorPos--
			c.showTempEnglishUI()
			return c.tempEnglishCompositionResult()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case vk == ipc.VK_RIGHT:
		if c.tempEnglishCursorPos < len(c.tempEnglishBuffer) {
			c.tempEnglishCursorPos++
			c.showTempEnglishUI()
			return c.tempEnglishCompositionResult()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case vk == ipc.VK_HOME:
		c.tempEnglishCursorPos = 0
		c.showTempEnglishUI()
		return c.tempEnglishCompositionResultWithCaret(0)

	case vk == ipc.VK_END:
		c.tempEnglishCursorPos = len(c.tempEnglishBuffer)
		c.showTempEnglishUI()
		return c.tempEnglishCompositionResult()

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

	// === 二候选选择键（仅有候选时匹配） ===
	case data.Modifiers&ModShift == 0 && c.isSelectKey2(key, data.KeyCode) && len(c.candidates) > 0:
		if len(c.candidates) >= 2 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			idx := pageStart + 1
			if idx < len(c.candidates) {
				return c.exitTempEnglishMode(true, c.candidates[idx].Text)
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 三候选选择键（仅有候选时匹配） ===
	case data.Modifiers&ModShift == 0 && c.isSelectKey3(key, data.KeyCode) && len(c.candidates) > 0:
		if len(c.candidates) >= 3 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			idx := pageStart + 2
			if idx < len(c.candidates) {
				return c.exitTempEnglishMode(true, c.candidates[idx].Text)
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 触发键二次输入：按当前标点/全半角状态上屏符号 ===
	case c.tempEnglishTriggerKey != "" && c.isTempEnglishTriggerKeyMatch(key, data.KeyCode):
		if len(c.tempEnglishBuffer) == 0 {
			// 缓冲区为空时，直接按标点状态输出触发键字符
			punctText := c.tempEnglishTriggerPrefix()
			if len(punctText) == 1 && c.isEffectiveChinesePunct() {
				if converted, ok := c.punctConverter.ToChinesePunctStr(rune(punctText[0])); ok {
					punctText = converted
				}
			}
			if c.fullWidth {
				punctText = transform.ToFullWidth(punctText)
			}
			return c.exitTempEnglishMode(true, punctText)
		}
		// 有缓冲内容时，上屏当前高亮候选+标点
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
		punctText := key
		if len(key) == 1 && c.isEffectiveChinesePunct() {
			if converted, ok := c.punctConverter.ToChinesePunctStr(rune(key[0])); ok {
				punctText = converted
			}
		}
		if c.fullWidth {
			punctText = transform.ToFullWidth(punctText)
		}
		return c.exitTempEnglishMode(true, text+punctText)

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
		return c.tempEnglishCompositionResult()

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

	// 构建 preedit：触发键进入时显示前缀 + 缓冲区内容
	prefix := c.tempEnglishTriggerPrefix()
	preedit := prefix + c.tempEnglishBuffer
	caretPosUI := len(prefix) + c.tempEnglishCursorPos

	c.uiManager.SetModeLabel("临时英文")
	c.uiManager.ShowCandidates(
		displayCandidates,
		preedit,
		caretPosUI,
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

// tempEnglishTriggerPrefix 返回临时英文触发键对应的字符
func (c *Coordinator) tempEnglishTriggerPrefix() string {
	switch c.tempEnglishTriggerKey {
	case "backtick":
		return "`"
	case "semicolon":
		return ";"
	case "quote":
		return "'"
	case "comma":
		return ","
	case "period":
		return "."
	case "slash":
		return "/"
	case "backslash":
		return "\\"
	case "open_bracket":
		return "["
	case "close_bracket":
		return "]"
	default:
		return ""
	}
}

// tempEnglishCompositionResult 构建临时英文模式的编辑区更新结果（包含前缀）
func (c *Coordinator) tempEnglishCompositionResult() *bridge.KeyEventResult {
	prefix := c.tempEnglishTriggerPrefix()
	preedit := prefix + c.tempEnglishBuffer
	caretPos := len(prefix) + c.tempEnglishCursorPos
	return c.modeCompositionResult(preedit, caretPos)
}

// tempEnglishCompositionResultWithCaret 构建临时英文模式编辑区更新，使用指定光标位置
func (c *Coordinator) tempEnglishCompositionResultWithCaret(cursorPos int) *bridge.KeyEventResult {
	prefix := c.tempEnglishTriggerPrefix()
	preedit := prefix + c.tempEnglishBuffer
	caretPos := len(prefix) + cursorPos
	return c.modeCompositionResult(preedit, caretPos)
}

// isTempEnglishTriggerKeyMatch 仅检查按键是否匹配当前临时英文触发键（不检查状态条件）
func (c *Coordinator) isTempEnglishTriggerKeyMatch(key string, keyCode int) bool {
	switch c.tempEnglishTriggerKey {
	case "backtick":
		return key == "`" || uint32(keyCode) == ipc.VK_OEM_3
	case "semicolon":
		return key == ";" || uint32(keyCode) == ipc.VK_OEM_1
	case "quote":
		return key == "'" || uint32(keyCode) == ipc.VK_OEM_7
	case "comma":
		return key == "," || uint32(keyCode) == ipc.VK_OEM_COMMA
	case "period":
		return key == "." || uint32(keyCode) == ipc.VK_OEM_PERIOD
	case "slash":
		return key == "/" || uint32(keyCode) == ipc.VK_OEM_2
	case "backslash":
		return key == "\\" || uint32(keyCode) == ipc.VK_OEM_5
	case "open_bracket":
		return key == "[" || uint32(keyCode) == ipc.VK_OEM_4
	case "close_bracket":
		return key == "]" || uint32(keyCode) == ipc.VK_OEM_6
	}
	return false
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
			if key == "`" || uint32(keyCode) == ipc.VK_OEM_3 {
				return tk
			}
		case "semicolon":
			if key == ";" || uint32(keyCode) == ipc.VK_OEM_1 {
				return tk
			}
		case "quote":
			if key == "'" || uint32(keyCode) == ipc.VK_OEM_7 {
				return tk
			}
		case "comma":
			if key == "," || uint32(keyCode) == ipc.VK_OEM_COMMA {
				return tk
			}
		case "period":
			if key == "." || uint32(keyCode) == ipc.VK_OEM_PERIOD {
				return tk
			}
		case "slash":
			if key == "/" || uint32(keyCode) == ipc.VK_OEM_2 {
				return tk
			}
		case "backslash":
			if key == "\\" || uint32(keyCode) == ipc.VK_OEM_5 {
				return tk
			}
		case "open_bracket":
			if key == "[" || uint32(keyCode) == ipc.VK_OEM_4 {
				return tk
			}
		case "close_bracket":
			if key == "]" || uint32(keyCode) == ipc.VK_OEM_6 {
				return tk
			}
		}
	}
	return ""
}
