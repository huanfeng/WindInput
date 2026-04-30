// handle_quick_input.go — 快捷输入模式（分号触发，数字输入+字母选择）
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
)

// isQuickInputTriggerKey 仅检查按键是否匹配快捷输入触发键（不检查状态条件）
func (c *Coordinator) isQuickInputTriggerKey(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}
	for _, tk := range c.config.Input.QuickInput.TriggerKeys {
		switch tk {
		case "semicolon":
			if key == ";" || uint32(keyCode) == ipc.VK_OEM_1 {
				return true
			}
		case "backtick":
			if key == "`" || uint32(keyCode) == ipc.VK_OEM_3 {
				return true
			}
		case "quote":
			if key == "'" || uint32(keyCode) == ipc.VK_OEM_7 {
				return true
			}
		case "comma":
			if key == "," || uint32(keyCode) == ipc.VK_OEM_COMMA {
				return true
			}
		case "period":
			if key == "." || uint32(keyCode) == ipc.VK_OEM_PERIOD {
				return true
			}
		case "slash":
			if key == "/" || uint32(keyCode) == ipc.VK_OEM_2 {
				return true
			}
		case "backslash":
			if key == "\\" || uint32(keyCode) == ipc.VK_OEM_5 {
				return true
			}
		case "open_bracket":
			if key == "[" || uint32(keyCode) == ipc.VK_OEM_4 {
				return true
			}
		case "close_bracket":
			if key == "]" || uint32(keyCode) == ipc.VK_OEM_6 {
				return true
			}
		}
	}
	return false
}

// getQuickInputTriggerKey 检查按键是否应触发快捷输入模式，返回匹配的触发键类型，空串表示不触发
func (c *Coordinator) getQuickInputTriggerKey(key string, keyCode int) string {
	if c.config == nil || len(c.config.Input.QuickInput.TriggerKeys) == 0 {
		return ""
	}
	// 仅输入缓冲区为空且无候选时触发
	if len(c.inputBuffer) > 0 || len(c.candidates) > 0 {
		return ""
	}
	for _, tk := range c.config.Input.QuickInput.TriggerKeys {
		switch tk {
		case "semicolon":
			if key == ";" || uint32(keyCode) == ipc.VK_OEM_1 {
				return tk
			}
		case "backtick":
			if key == "`" || uint32(keyCode) == ipc.VK_OEM_3 {
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

// quickInputPrefix 返回当前触发键对应的字符
func (c *Coordinator) quickInputPrefix() string {
	switch c.quickInputTriggerKey {
	case "backtick":
		return "`"
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
		return ";"
	}
}

// enterQuickInputMode 进入快捷输入模式，triggerKey 标识触发键类型
func (c *Coordinator) enterQuickInputMode(triggerKey string) *bridge.KeyEventResult {
	c.quickInputMode = true
	c.quickInputTriggerKey = triggerKey
	c.quickInputBuffer = ""

	// 强制竖排：保存当前布局并切换
	if c.config != nil && c.config.Input.QuickInput.ForceVertical {
		c.savedLayout = c.config.UI.CandidateLayout
		if c.uiManager != nil {
			c.uiManager.SetCandidateLayout("vertical")
		}
	}

	c.logger.Debug("Entered quick input mode")

	// 更新候选（缓冲区为空时显示重复上屏候选）
	c.updateQuickInputCandidates()

	// 首次进入触发 C++ 端 StartComposition，同步标记 pendingFirstShow，
	// 让 Excel/WPS 表格 cell-select→cell-edit 的失焦能命中 replay 路径。
	// 不立即 showUI：等 OnLayoutChange 真实坐标由 HandleCaretUpdate 触发首次显示，
	// 否则会先用按键前的旧坐标显示再跳到正确位置（与 handleAlphaKey 首字符一致）。
	c.armPendingFirstShow()

	preedit := c.quickInputPrefix()
	return c.modeCompositionResult(preedit, len(preedit))
}

// handleQuickInputKey 处理快捷输入模式下的按键
// maxQuickInputBufferLen 快捷输入缓冲区最大长度
const maxQuickInputBufferLen = 20

func (c *Coordinator) handleQuickInputKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	// 如果处于临时拼音子模式，委托给拼音按键处理
	if c.quickInputPinyinMode {
		return c.handleQuickInputPinyinKey(key, data)
	}

	vk := uint32(data.KeyCode)

	switch {
	// === 控制键（按 VK 码识别，优先处理） ===

	// 空格：缓冲区为空时重复上屏，有候选时选当前高亮
	case vk == ipc.VK_SPACE:
		if len(c.quickInputBuffer) == 0 {
			return c.handleQuickInputRepeat()
		}
		if len(c.candidates) > 0 {
			index := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
			return c.selectQuickInputCandidate(index)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// 回车：上屏缓冲区原文；缓冲区为空时上屏触发键字符
	case vk == ipc.VK_RETURN:
		if len(c.quickInputBuffer) > 0 {
			return c.exitQuickInputMode(true, c.quickInputBuffer)
		}
		return c.exitQuickInputMode(true, c.quickInputPrefix())

	// 退格：删缓冲区末字符
	case vk == ipc.VK_BACK:
		if len(c.quickInputBuffer) > 0 {
			c.quickInputBuffer = c.quickInputBuffer[:len(c.quickInputBuffer)-1]
			if len(c.quickInputBuffer) == 0 {
				c.updateQuickInputCandidates()
				c.showQuickInputUI()
				prefix := c.quickInputPrefix()
				return c.modeCompositionResult(prefix, len(prefix))
			}
			c.currentPage = 1
			c.selectedIndex = 0
			c.updateQuickInputCandidates()
			c.showQuickInputUI()
			preedit := c.quickInputPrefix() + c.quickInputBuffer
			return c.modeCompositionResult(preedit, len(preedit))
		}
		return c.exitQuickInputMode(false, "")

	// ESC：退出
	case vk == ipc.VK_ESCAPE:
		return c.exitQuickInputMode(false, "")

	// === 导航键（使用与正常模式一致的配置键） ===

	case c.isQuickInputPageUpKey(key, int(vk), uint32(data.Modifiers)):
		if c.currentPage > 1 {
			c.currentPage--
			c.selectedIndex = 0
			c.showQuickInputUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case c.isQuickInputPageDownKey(key, int(vk), uint32(data.Modifiers)):
		if c.currentPage < c.totalPages {
			c.currentPage++
			c.selectedIndex = 0
			c.showQuickInputUI()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case c.isHighlightUpKey(vk, uint32(data.Modifiers)):
		if len(c.candidates) > 0 {
			if c.selectedIndex > 0 {
				c.selectedIndex--
				c.showQuickInputUI()
			} else if c.currentPage > 1 {
				c.currentPage--
				startIdx := (c.currentPage - 1) * c.candidatesPerPage
				endIdx := startIdx + c.candidatesPerPage
				if endIdx > len(c.candidates) {
					endIdx = len(c.candidates)
				}
				c.selectedIndex = endIdx - startIdx - 1
				c.showQuickInputUI()
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case c.isHighlightDownKey(vk, uint32(data.Modifiers)):
		if len(c.candidates) > 0 {
			startIdx := (c.currentPage - 1) * c.candidatesPerPage
			endIdx := startIdx + c.candidatesPerPage
			if endIdx > len(c.candidates) {
				endIdx = len(c.candidates)
			}
			pageCount := endIdx - startIdx
			if c.selectedIndex < pageCount-1 {
				c.selectedIndex++
				c.showQuickInputUI()
			} else if c.currentPage < c.totalPages {
				c.currentPage++
				c.selectedIndex = 0
				c.showQuickInputUI()
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 再次按触发键且缓冲区为空：上屏触发键字符 ===

	case c.isQuickInputTriggerKey(key, data.KeyCode) && len(c.quickInputBuffer) == 0:
		prefix := c.quickInputPrefix()
		punctText := prefix
		if c.chinesePunctuation {
			if converted, ok := c.punctConverter.ToChinesePunctStr(rune(prefix[0])); ok {
				punctText = converted
			}
		}
		if c.fullWidth {
			punctText = transform.ToFullWidth(punctText)
		}
		return c.exitQuickInputMode(true, punctText)

	// === 字母键 a-z/A-Z：候选选择（仅缓冲区非空时） ===

	case len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')):
		lower := key[0]
		if lower >= 'A' && lower <= 'Z' {
			lower = lower - 'A' + 'a'
		}
		// 缓冲区为空时：进入临时拼音子模式（字母作为拼音首字母）
		// z 键也进入拼音模式（重复上屏功能通过空格实现）
		if len(c.quickInputBuffer) == 0 {
			return c.enterQuickInputPinyinMode(string(lower))
		}
		idx := int(lower - 'a')
		pageStart := (c.currentPage - 1) * c.candidatesPerPage
		globalIdx := pageStart + idx
		if globalIdx < len(c.candidates) {
			return c.selectQuickInputCandidate(globalIdx)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 所有其他可打印字符：追加到缓冲区 ===
	// 包括数字 0-9、运算符 +-*/、点号、分号、括号、等号等
	// 在快捷输入模式下，这些符号不再作为翻页键或选择键
	case len(key) == 1 && key[0] >= '!' && key[0] <= '~':
		if len(c.quickInputBuffer) >= maxQuickInputBufferLen {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		c.quickInputBuffer += key
		c.updateQuickInputCandidates()
		c.showQuickInputUI()
		preedit := c.quickInputPrefix() + c.quickInputBuffer
		return c.modeCompositionResult(preedit, len(preedit))

	default:
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
}

// handleQuickInputRepeat 重复上屏：从 inputHistory 取最近一条记录
func (c *Coordinator) handleQuickInputRepeat() *bridge.KeyEventResult {
	if c.inputHistory == nil {
		return c.exitQuickInputMode(false, "")
	}
	records := c.inputHistory.GetRecentRecords(1, 0)
	if len(records) == 0 {
		return c.exitQuickInputMode(false, "")
	}
	text := records[0].Text
	if text == "" {
		return c.exitQuickInputMode(false, "")
	}
	return c.exitQuickInputMode(true, text)
}

// dedup 去重并保持顺序
func dedup(items []string) []string {
	seen := make(map[string]struct{}, len(items))
	result := make([]string, 0, len(items))
	for _, item := range items {
		if _, ok := seen[item]; !ok {
			seen[item] = struct{}{}
			result = append(result, item)
		}
	}
	return result
}

// updateQuickInputCandidates 更新快捷输入候选（合并多模块候选并去重）
func (c *Coordinator) updateQuickInputCandidates() {
	buf := c.quickInputBuffer
	if len(buf) == 0 {
		// 缓冲区为空：显示上次上屏内容作为重复候选
		if c.inputHistory != nil {
			records := c.inputHistory.GetRecentRecords(1, 0)
			if len(records) > 0 && records[0].Text != "" {
				c.candidates = []ui.Candidate{
					{
						Text:  records[0].Text,
						Index: -1, // 不显示序号，只能用空格上屏
					},
				}
				c.totalPages = 1
				c.currentPage = 1
				c.selectedIndex = 0
				return
			}
		}
		c.candidates = nil
		c.totalPages = 1
		return
	}

	var allTexts []string

	// 1. 年月日日期（三段）
	if isDateExpression(buf) {
		allTexts = append(allTexts, generateDateCandidates(buf)...)
	}

	// 2. 年月日期（两段，首段>31）
	if isYearMonthExpression(buf) {
		allTexts = append(allTexts, generateYearMonthCandidates(buf)...)
	}

	// 3. 计算表达式（必须有真实运算符）
	if isCalcExpression(buf) {
		decimalPlaces := 6
		if c.config != nil {
			decimalPlaces = c.config.Input.QuickInput.DecimalPlaces
		}
		if calcs := generateCalcCandidates(buf, decimalPlaces); len(calcs) > 0 {
			allTexts = append(allTexts, calcs...)
		}
	}

	// 4. 数字/小数（整数或小数）
	if isDecimalNumber(buf) {
		allTexts = append(allTexts, generateNumberCandidates(buf)...)
	}

	// 去重
	texts := dedup(allTexts)

	// 转换为 ui.Candidate，设置 IndexLabel 为 a/b/c...
	candidates := make([]ui.Candidate, 0, len(texts))
	for i, t := range texts {
		label := ""
		if i < 26 {
			label = string(rune('a' + i))
		}
		candidates = append(candidates, ui.Candidate{
			Text:       t,
			Index:      i + 1,
			IndexLabel: label,
		})
	}

	c.candidates = candidates

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

// selectQuickInputCandidate 选择快捷输入候选后退出
func (c *Coordinator) selectQuickInputCandidate(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	cand := c.candidates[index]
	text := cand.Text
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 记录输入历史，供重复上屏功能使用
	if c.inputHistory != nil {
		c.inputHistory.Record(text, "", "", 0)
	}

	return c.exitQuickInputMode(true, text)
}

// exitQuickInputMode 退出快捷输入模式
func (c *Coordinator) exitQuickInputMode(commit bool, text string) *bridge.KeyEventResult {
	// 清理临时拼音子模式状态（防御性：正常路径由 exitQuickInputPinyinMode 提前清理）
	if c.quickInputPinyinDictSwapped && c.engineMgr != nil {
		c.engineMgr.DeactivateTempPinyin()
	}
	c.quickInputPinyinMode = false
	c.quickInputPinyinBuffer = ""
	c.quickInputPinyinDictSwapped = false

	// 恢复布局（如果之前保存了）
	if c.savedLayout != "" && c.uiManager != nil {
		c.uiManager.SetCandidateLayout(c.savedLayout)
		c.savedLayout = ""
	}

	// 重置快捷输入模式标志和模式标签
	if c.uiManager != nil {
		c.uiManager.SetQuickInputMode(false)
		c.uiManager.SetModeLabel("")
	}

	c.quickInputMode = false
	c.quickInputTriggerKey = ""
	c.quickInputBuffer = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	c.selectedIndex = 0
	c.hideUI()

	c.logger.Debug("Exited quick input mode", "commit", commit, "textLen", len(text))

	if commit && len(text) > 0 {
		c.recordCommit(text, 0, -1, store.SourceQuickInput)
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// showQuickInputUI 显示快捷输入模式 UI
func (c *Coordinator) showQuickInputUI() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// 使用光标位置（与 showTempPinyinUI 一致）
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

	// 复制候选并重新设置 IndexLabel
	displayCandidates := make([]ui.Candidate, len(pageCandidates))
	copy(displayCandidates, pageCandidates)
	for i := range displayCandidates {
		if i < 26 {
			displayCandidates[i].IndexLabel = string(rune('a' + i))
		}
	}

	// 构建预编辑文本
	preedit := c.quickInputPrefix() + c.quickInputBuffer

	c.uiManager.SetQuickInputMode(true)
	c.uiManager.SetModeLabel("快捷输入")
	c.uiManager.ShowCandidates(
		displayCandidates,
		preedit,
		len(preedit),
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
