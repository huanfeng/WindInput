// pinyin_mode_shared.go — 拼音模式共享逻辑
// 临时拼音模式（handle_temp_pinyin.go）和快捷输入拼音子模式（handle_quick_input_pinyin.go）
// 共用此文件中的按键处理、候选更新、候选选择、UI 显示等核心逻辑。
// 各模式通过 pinyinModeOps 结构体注入差异化行为。
package coordinator

import (
	"strings"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/ui"
)

// pinyinModeOps 封装拼音模式中各实现的差异化行为
type pinyinModeOps struct {
	buffer               *string                                   // 指向缓冲区字段的指针
	prefix               func() string                             // 获取前缀显示字符
	exitMode             func(bool, string) *bridge.KeyEventResult // 完全退出模式
	exitOnBackspaceEmpty func() *bridge.KeyEventResult             // 退格删空缓冲区时的行为
	separator            func(string, int) bool                    // 分隔符判断
	triggerKey           func(string, int) bool                    // 触发键判断
	consumeSpaceEmpty    bool                                      // 无候选时空格是否仅消费（true）或退出（false）
}

// handlePinyinModeKey 拼音模式通用按键处理
func (c *Coordinator) handlePinyinModeKey(ops *pinyinModeOps, key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	vk := uint32(data.KeyCode)

	switch {
	// === 字母 a-z ===
	case len(key) == 1 && key[0] >= 'a' && key[0] <= 'z':
		*ops.buffer += key
		c.currentPage = 1
		c.selectedIndex = 0
		c.updatePinyinModeCandidates(ops)
		c.showPinyinModeUI(ops)
		return c.pinyinModeCompositionResult(ops)

	// === 大写字母转小写 ===
	case len(key) == 1 && key[0] >= 'A' && key[0] <= 'Z':
		*ops.buffer += strings.ToLower(key)
		c.currentPage = 1
		c.selectedIndex = 0
		c.updatePinyinModeCandidates(ops)
		c.showPinyinModeUI(ops)
		return c.pinyinModeCompositionResult(ops)

	// === 数字 1-9 选候选 ===
	case len(key) == 1 && key[0] >= '1' && key[0] <= '9':
		idx := int(key[0]-'0') - 1
		pageStart := (c.currentPage - 1) * c.candidatesPerPage
		globalIdx := pageStart + idx
		if globalIdx < len(c.candidates) {
			return c.selectPinyinModeCandidate(ops, globalIdx)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 数字 0 选第10个 ===
	case len(key) == 1 && key[0] == '0':
		pageStart := (c.currentPage - 1) * c.candidatesPerPage
		globalIdx := pageStart + 9
		if globalIdx < len(c.candidates) {
			return c.selectPinyinModeCandidate(ops, globalIdx)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 空格：选首候选 ===
	case vk == ipc.VK_SPACE:
		if len(c.candidates) > 0 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			return c.selectPinyinModeCandidate(ops, pageStart)
		}
		if ops.consumeSpaceEmpty {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return ops.exitMode(false, "")

	// === 回车：上屏原文字母 ===
	case vk == ipc.VK_RETURN:
		if len(*ops.buffer) > 0 {
			return ops.exitMode(true, *ops.buffer)
		}
		return ops.exitMode(false, "")

	// === 退格 ===
	case vk == ipc.VK_BACK:
		if len(*ops.buffer) > 0 {
			*ops.buffer = (*ops.buffer)[:len(*ops.buffer)-1]
			if len(*ops.buffer) == 0 {
				return ops.exitOnBackspaceEmpty()
			}
			c.currentPage = 1
			c.updatePinyinModeCandidates(ops)
			c.showPinyinModeUI(ops)
			return c.pinyinModeCompositionResult(ops)
		}
		return ops.exitOnBackspaceEmpty()

	// === ESC ===
	case vk == ipc.VK_ESCAPE:
		return ops.exitMode(false, "")

	// === 翻页 ===
	case c.isPageUpKey(key, data.KeyCode, uint32(data.Modifiers)):
		if c.currentPage > 1 {
			c.currentPage--
			c.selectedIndex = 0
			c.showPinyinModeUI(ops)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case c.isPageDownKey(key, data.KeyCode, uint32(data.Modifiers)):
		if c.currentPage < c.totalPages {
			c.currentPage++
			c.selectedIndex = 0
			c.showPinyinModeUI(ops)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 高亮上移 ===
	case c.isHighlightUpKey(vk, uint32(data.Modifiers)):
		if len(c.candidates) > 0 {
			if c.selectedIndex > 0 {
				c.selectedIndex--
				c.showPinyinModeUI(ops)
			} else if c.currentPage > 1 {
				c.currentPage--
				startIdx := (c.currentPage - 1) * c.candidatesPerPage
				endIdx := startIdx + c.candidatesPerPage
				if endIdx > len(c.candidates) {
					endIdx = len(c.candidates)
				}
				c.selectedIndex = endIdx - startIdx - 1
				c.showPinyinModeUI(ops)
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 高亮下移 ===
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
				c.showPinyinModeUI(ops)
			} else if c.currentPage < c.totalPages {
				c.currentPage++
				c.selectedIndex = 0
				c.showPinyinModeUI(ops)
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 二候选选择键 ===
	case data.Modifiers&ModShift == 0 && c.isSelectKey2(key, data.KeyCode):
		if len(c.candidates) >= 2 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			idx := pageStart + 1
			if idx < len(c.candidates) {
				return c.selectPinyinModeCandidate(ops, idx)
			}
		}
		if len(c.candidates) > 0 {
			return c.selectPinyinModeWithPunct(ops, 0, key)
		}
		return ops.exitMode(false, "")

	// === 拼音分隔符 ===
	case data.Modifiers&ModShift == 0 && ops.separator(key, data.KeyCode):
		if len(*ops.buffer) > 0 &&
			(*ops.buffer)[len(*ops.buffer)-1] != '\'' {
			*ops.buffer += "'"
			c.updatePinyinModeCandidates(ops)
			c.showPinyinModeUI(ops)
			return c.pinyinModeCompositionResult(ops)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 三候选选择键 ===
	case data.Modifiers&ModShift == 0 && c.isSelectKey3(key, data.KeyCode):
		if len(c.candidates) >= 3 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			idx := pageStart + 2
			if idx < len(c.candidates) {
				return c.selectPinyinModeCandidate(ops, idx)
			}
		}
		if len(c.candidates) > 0 {
			return c.selectPinyinModeWithPunct(ops, 0, key)
		}
		return ops.exitMode(false, "")

	// === 触发键 ===
	case ops.triggerKey != nil && ops.triggerKey(key, data.KeyCode):
		// 缓冲区为空时，输出触发键字符的标点形式
		if len(*ops.buffer) == 0 {
			punctText := key
			if len(key) == 1 && c.chinesePunctuation {
				if converted, ok := c.punctConverter.ToChinesePunctStr(rune(key[0])); ok {
					punctText = converted
				}
			}
			if c.fullWidth {
				punctText = transform.ToFullWidth(punctText)
			}
			return ops.exitMode(true, punctText)
		}
		// 有候选时上屏首候选+标点
		if len(c.candidates) > 0 {
			return c.selectPinyinModeWithPunct(ops, 0, key)
		}
		return ops.exitMode(false, "")

	default:
		// 其他按键（标点等）：有候选时选首候选+标点
		if len(c.candidates) > 0 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			cand := c.candidates[pageStart]
			text := cand.Text
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
			return ops.exitMode(true, text+punctText)
		}
		return ops.exitMode(false, "")
	}
}

// updatePinyinModeCandidates 更新拼音候选
func (c *Coordinator) updatePinyinModeCandidates(ops *pinyinModeOps) {
	if c.engineMgr == nil || len(*ops.buffer) == 0 {
		c.candidates = nil
		c.preeditDisplay = ""
		c.totalPages = 1
		return
	}

	maxCandidates := 100
	result := c.engineMgr.ConvertWithPinyin(*ops.buffer, maxCandidates)

	for i := range result.Candidates {
		result.Candidates[i].Index = i + 1
	}

	c.candidates = result.Candidates
	c.preeditDisplay = result.PreeditDisplay

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

// selectPinyinModeCandidate 选择候选（支持部分上屏）
func (c *Coordinator) selectPinyinModeCandidate(ops *pinyinModeOps, index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	cand := c.candidates[index]
	text := cand.Text
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	// 支持拼音部分上屏
	if cand.ConsumedLength > 0 && cand.ConsumedLength < len(*ops.buffer) {
		*ops.buffer = (*ops.buffer)[cand.ConsumedLength:]
		c.currentPage = 1
		c.updatePinyinModeCandidates(ops)
		c.showPinyinModeUI(ops)

		prefix := ops.prefix()
		preedit := prefix + c.preeditDisplay
		if c.preeditDisplay == "" {
			preedit = prefix + *ops.buffer
		}
		return &bridge.KeyEventResult{
			Type:           bridge.ResponseTypeInsertText,
			Text:           text,
			NewComposition: preedit,
		}
	}

	return ops.exitMode(true, text)
}

// selectPinyinModeWithPunct 选择首候选并附加标点后退出
func (c *Coordinator) selectPinyinModeWithPunct(ops *pinyinModeOps, pageOffset int, key string) *bridge.KeyEventResult {
	pageStart := (c.currentPage - 1) * c.candidatesPerPage
	idx := pageStart + pageOffset
	if idx >= len(c.candidates) {
		return ops.exitMode(false, "")
	}
	cand := c.candidates[idx]
	text := cand.Text
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}
	punctText := key
	if len(key) == 1 && c.chinesePunctuation {
		if converted, ok := c.punctConverter.ToChinesePunctStr(rune(key[0])); ok {
			punctText = converted
		}
	}
	return ops.exitMode(true, text+punctText)
}

// pinyinModeCompositionResult 构建拼音模式的编辑区更新结果
func (c *Coordinator) pinyinModeCompositionResult(ops *pinyinModeOps) *bridge.KeyEventResult {
	prefix := ops.prefix()
	preedit := prefix + c.preeditDisplay
	if c.preeditDisplay == "" {
		preedit = prefix + *ops.buffer
	}
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     preedit,
		CaretPos: len(preedit),
	}
}

// showPinyinModeUI 显示拼音模式 UI
func (c *Coordinator) showPinyinModeUI(ops *pinyinModeOps) {
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

	startIdx := (c.currentPage - 1) * c.candidatesPerPage
	endIdx := startIdx + c.candidatesPerPage
	if endIdx > len(c.candidates) {
		endIdx = len(c.candidates)
	}

	var pageCandidates []ui.Candidate
	if startIdx < len(c.candidates) {
		pageCandidates = c.candidates[startIdx:endIdx]
	}

	// 数字编号（1-9, 0 for 10th）
	displayCandidates := make([]ui.Candidate, len(pageCandidates))
	copy(displayCandidates, pageCandidates)
	for i := range displayCandidates {
		displayCandidates[i].Index = (i + 1) % 10
	}

	prefix := ops.prefix()
	preedit := prefix + c.preeditDisplay
	if c.preeditDisplay == "" && len(*ops.buffer) > 0 {
		preedit = prefix + *ops.buffer
	} else if len(*ops.buffer) == 0 {
		preedit = prefix
	}

	c.uiManager.SetModeLabel("临时拼音")
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

// isPinyinSeparatorForBuffer 通用拼音分隔符判断
func (c *Coordinator) isPinyinSeparatorForBuffer(buffer string, key string, keyCode int) bool {
	if len(buffer) == 0 {
		return false
	}

	separatorMode := "auto"
	if c.config != nil && c.config.Input.PinyinSeparator != "" {
		separatorMode = c.config.Input.PinyinSeparator
	}

	switch separatorMode {
	case "none":
		return false
	case "quote":
		return key == "'" || uint32(keyCode) == ipc.VK_OEM_7
	case "backtick":
		return key == "`" || uint32(keyCode) == ipc.VK_OEM_3
	case "auto":
		isQuote := key == "'" || uint32(keyCode) == ipc.VK_OEM_7
		isBacktick := key == "`" || uint32(keyCode) == ipc.VK_OEM_3
		if isQuote {
			if c.isSelectKey3(key, keyCode) {
				return false
			}
			return true
		}
		if isBacktick {
			quoteIsSelectKey := c.isSelectKey3("'", int(ipc.VK_OEM_7))
			return quoteIsSelectKey
		}
		return false
	default:
		return false
	}
}
