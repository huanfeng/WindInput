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
	"github.com/huanfeng/wind_input/pkg/config"
)

// pinyinModeOps 封装拼音模式中各实现的差异化行为
type pinyinModeOps struct {
	buffer               *string                                   // 指向缓冲区字段的指针
	cursorPos            *int                                      // 指向光标位置的指针（在 buffer 中的字节偏移）
	committed            *string                                   // 指向累积已提交文本的指针（部分上屏时累积）
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
		c.pinyinModeInsertChar(ops, key)
		c.currentPage = 1
		c.selectedIndex = 0
		c.updatePinyinModeCandidates(ops)
		c.showPinyinModeUI(ops)
		return c.pinyinModeCompositionResult(ops)

	// === 大写字母转小写 ===
	case len(key) == 1 && key[0] >= 'A' && key[0] <= 'Z':
		c.pinyinModeInsertChar(ops, strings.ToLower(key))
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

	// === 空格：选当前高亮候选 ===
	case vk == ipc.VK_SPACE:
		if len(c.candidates) > 0 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			return c.selectPinyinModeCandidate(ops, pageStart+c.selectedIndex)
		}
		if ops.consumeSpaceEmpty {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return ops.exitMode(false, "")

	// === 回车：上屏编码（缓冲区为空时上屏触发键字符） ===
	case vk == ipc.VK_RETURN:
		if len(*ops.buffer) > 0 {
			// 如有部分上屏累积的真实文本（committed），单独记入历史；buffer 是拼音码本身，不计为候选
			if c.inputHistory != nil && ops.committed != nil && *ops.committed != "" {
				c.inputHistory.Record(*ops.committed, "", "", 0)
			}
			return ops.exitMode(true, *ops.buffer)
		}
		return ops.exitMode(true, ops.prefix())

	// === 退格 ===
	case vk == ipc.VK_BACK:
		if len(*ops.buffer) > 0 {
			if ops.cursorPos != nil && *ops.cursorPos > 0 {
				// 在光标位置删除
				*ops.buffer = (*ops.buffer)[:*ops.cursorPos-1] + (*ops.buffer)[*ops.cursorPos:]
				*ops.cursorPos--
			} else if ops.cursorPos == nil {
				// 无光标时从末尾删除（兼容）
				*ops.buffer = (*ops.buffer)[:len(*ops.buffer)-1]
			} else {
				// 光标在开头，不能再删
				return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
			}
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

	// === 左右方向键：移动光标 ===
	case vk == ipc.VK_LEFT:
		if ops.cursorPos != nil && *ops.cursorPos > 0 {
			*ops.cursorPos--
			c.showPinyinModeUI(ops)
			return c.pinyinModeCompositionResult(ops)
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	case vk == ipc.VK_RIGHT:
		if ops.cursorPos != nil && *ops.cursorPos < len(*ops.buffer) {
			*ops.cursorPos++
			c.showPinyinModeUI(ops)
			return c.pinyinModeCompositionResult(ops)
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

	// === 二候选选择键（仅有候选时匹配，无候选时让触发键等后续 case 处理） ===
	case data.Modifiers&ModShift == 0 && c.isSelectKey2(key, data.KeyCode) && len(c.candidates) > 0:
		if len(c.candidates) >= 2 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			idx := pageStart + 1
			if idx < len(c.candidates) {
				return c.selectPinyinModeCandidate(ops, idx)
			}
		}
		return c.handlePinyinModeOverflowSelectKey(ops, key)

	// === 拼音分隔符 ===
	case data.Modifiers&ModShift == 0 && ops.separator(key, data.KeyCode):
		if len(*ops.buffer) > 0 {
			// 检查光标位置前一个字符是否已是分隔符
			insertPos := len(*ops.buffer)
			if ops.cursorPos != nil {
				insertPos = *ops.cursorPos
			}
			if insertPos > 0 && (*ops.buffer)[insertPos-1] != '\'' {
				c.pinyinModeInsertChar(ops, "'")
				c.updatePinyinModeCandidates(ops)
				c.showPinyinModeUI(ops)
				return c.pinyinModeCompositionResult(ops)
			}
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}

	// === 三候选选择键（仅有候选时匹配，无候选时让触发键等后续 case 处理） ===
	case data.Modifiers&ModShift == 0 && c.isSelectKey3(key, data.KeyCode) && len(c.candidates) > 0:
		if len(c.candidates) >= 3 {
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			idx := pageStart + 2
			if idx < len(c.candidates) {
				return c.selectPinyinModeCandidate(ops, idx)
			}
		}
		return c.handlePinyinModeOverflowSelectKey(ops, key)

	// === 触发键 ===
	case ops.triggerKey != nil && ops.triggerKey(key, data.KeyCode):
		// 缓冲区为空时，输出触发键字符的标点形式
		if len(*ops.buffer) == 0 {
			punctText := key
			if len(key) == 1 && c.isEffectiveChinesePunct() {
				if converted, ok := c.punctConverter.ToChinesePunctStr(rune(key[0])); ok {
					punctText = converted
				}
			}
			if c.fullWidth {
				punctText = transform.ToFullWidth(punctText)
			}
			return ops.exitMode(true, punctText)
		}
		// 有候选时：先检查是否为以词定字键
		if len(c.candidates) > 0 {
			if data.Modifiers&ModShift == 0 && c.isSelectCharFirstKey(key, data.KeyCode) {
				return c.selectPinyinModeChar(ops, 0)
			}
			if data.Modifiers&ModShift == 0 && c.isSelectCharSecondKey(key, data.KeyCode) {
				return c.selectPinyinModeChar(ops, 1)
			}
			return c.selectPinyinModeWithPunct(ops, 0, key)
		}
		return ops.exitMode(false, "")

	default:
		// 其他按键（标点等）：有候选时先检查以词定字键，否则选当前高亮候选+标点
		if len(c.candidates) > 0 {
			if data.Modifiers&ModShift == 0 && c.isSelectCharFirstKey(key, data.KeyCode) {
				return c.selectPinyinModeChar(ops, 0)
			}
			if data.Modifiers&ModShift == 0 && c.isSelectCharSecondKey(key, data.KeyCode) {
				return c.selectPinyinModeChar(ops, 1)
			}
			pageStart := (c.currentPage - 1) * c.candidatesPerPage
			absIdx := pageStart + c.selectedIndex
			if absIdx >= len(c.candidates) {
				absIdx = pageStart
			}
			cand := c.candidates[absIdx]
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
			c.recordPinyinModeHistory(ops, text)
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

	// 拼音拆分选择：已选字保留在合成区（不立即上屏），等全部选完再统一提交
	if cand.ConsumedLength > 0 && cand.ConsumedLength < len(*ops.buffer) {
		if ops.committed != nil {
			*ops.committed += text
		}
		*ops.buffer = (*ops.buffer)[cand.ConsumedLength:]
		if ops.cursorPos != nil {
			*ops.cursorPos = len(*ops.buffer)
		}
		c.currentPage = 1
		c.updatePinyinModeCandidates(ops)
		c.showPinyinModeUI(ops)
		// 返回合成区更新（非 InsertText），已选字作为 committed 前缀留在 preedit 中
		return c.pinyinModeCompositionResult(ops)
	}

	// 完整匹配：将累积的 committed + 当前候选一次性提交
	c.recordPinyinModeHistory(ops, text)
	return ops.exitMode(true, pinyinModeFullText(ops, text))
}

// pinyinModeFullText 返回 committed 前缀与当前候选文本拼接后的完整提交文本。
// 所有退出路径（全量选词、以词定字、标点附加）均通过此函数确保 committed 部分不被遗漏。
func pinyinModeFullText(ops *pinyinModeOps, text string) string {
	if ops.committed != nil && *ops.committed != "" {
		return *ops.committed + text
	}
	return text
}

// selectPinyinModeChar 以词定字：从当前高亮候选词中取第 charIndex 个字符上屏（拼音模式专用）
func (c *Coordinator) selectPinyinModeChar(ops *pinyinModeOps, charIndex int) *bridge.KeyEventResult {
	index := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
	if index >= len(c.candidates) {
		index = (c.currentPage - 1) * c.candidatesPerPage
	}
	if index >= len(c.candidates) {
		return ops.exitMode(false, "")
	}
	cand := c.candidates[index]
	runes := []rune(cand.Text)
	if charIndex >= len(runes) {
		// 候选词长度不足，按 overflow 策略：忽略（消费按键）
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
	text := string(runes[charIndex])
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}
	c.recordPinyinModeHistory(ops, text)
	return ops.exitMode(true, pinyinModeFullText(ops, text))
}

// recordPinyinModeHistory 记录拼音模式上屏文本到输入历史（仅候选文本，不含标点）
// 拼接 ops.committed（部分上屏累积）与本次 text，形成完整候选历史
func (c *Coordinator) recordPinyinModeHistory(ops *pinyinModeOps, text string) {
	if c.inputHistory == nil {
		return
	}
	full := text
	if ops.committed != nil {
		full = *ops.committed + text
	}
	if full == "" {
		return
	}
	c.inputHistory.Record(full, "", "", 0)
}

// handlePinyinModeOverflowSelectKey 处理拼音模式下二/三选键候选不足时的行为
// 复用 overflow_behavior.select_key 配置，与码表主路径保持一致：
//   - "ignore"（默认）: 仅消费按键
//   - "commit": 上屏当前高亮候选，不输出触发键
//   - "commit_and_input": 上屏当前高亮候选并附加触发键（即原拼音模式行为）
func (c *Coordinator) handlePinyinModeOverflowSelectKey(ops *pinyinModeOps, key string) *bridge.KeyEventResult {
	behavior := config.OverflowIgnore
	if c.config != nil && c.config.Input.OverflowBehavior.SelectKey != "" {
		behavior = c.config.Input.OverflowBehavior.SelectKey
	}

	pageStart := (c.currentPage - 1) * c.candidatesPerPage
	highlightedIdx := pageStart + c.selectedIndex
	if highlightedIdx >= len(c.candidates) {
		highlightedIdx = pageStart
	}

	switch behavior {
	case config.OverflowCommit:
		return c.selectPinyinModeCandidate(ops, highlightedIdx)
	case config.OverflowCommitAndInput:
		return c.selectPinyinModeWithPunct(ops, c.selectedIndex, key)
	default: // OverflowIgnore
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
}

// selectPinyinModeWithPunct 选择首候选并附加标点后退出
// 输入历史只记录候选部分（不含标点），上屏内容包含 committed 前缀和标点
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
	if len(key) == 1 && c.isEffectiveChinesePunct() {
		if converted, ok := c.punctConverter.ToChinesePunctStr(rune(key[0])); ok {
			punctText = converted
		}
	}
	c.recordPinyinModeHistory(ops, text)
	return ops.exitMode(true, pinyinModeFullText(ops, text)+punctText)
}

// mapBufferPosToDisplayPos 将 buffer 中的光标位置映射到 preeditDisplay 中的位置
// preeditDisplay 可能包含额外的分隔符（如 ' 或空格），需要跳过这些字符
func mapBufferPosToDisplayPos(buffer, display string, bufPos int) int {
	bi := 0 // buffer 索引
	di := 0 // display 索引
	for bi < bufPos && di < len(display) {
		if bi < len(buffer) && display[di] == buffer[bi] {
			bi++
			di++
		} else {
			// display 中的额外字符（分隔符），仅推进 display 索引
			di++
		}
	}
	return di
}

// pinyinModeInsertChar 在光标位置插入字符
func (c *Coordinator) pinyinModeInsertChar(ops *pinyinModeOps, ch string) {
	if ops.cursorPos != nil {
		pos := *ops.cursorPos
		*ops.buffer = (*ops.buffer)[:pos] + ch + (*ops.buffer)[pos:]
		*ops.cursorPos = pos + len(ch)
	} else {
		*ops.buffer += ch
	}
}

// pinyinModeCompositionResult 构建拼音模式的编辑区更新结果
// preedit = prefix + committed + preeditDisplay（或 buffer）
func (c *Coordinator) pinyinModeCompositionResult(ops *pinyinModeOps) *bridge.KeyEventResult {
	prefix := ops.prefix()
	committed := ""
	if ops.committed != nil {
		committed = *ops.committed
	}
	preedit := prefix + committed + c.preeditDisplay
	if c.preeditDisplay == "" {
		preedit = prefix + committed + *ops.buffer
	}
	// 计算光标位置（考虑 committed 偏移和 preeditDisplay 中的分隔符偏移）
	prefixCommittedLen := len(prefix) + len(committed)
	caretPos := len(preedit)
	if ops.cursorPos != nil {
		if c.preeditDisplay != "" {
			caretPos = prefixCommittedLen + mapBufferPosToDisplayPos(*ops.buffer, c.preeditDisplay, *ops.cursorPos)
		} else {
			caretPos = prefixCommittedLen + *ops.cursorPos
		}
	}
	return c.modeCompositionResult(preedit, caretPos)
}

// modeCompositionResult 是 compositionUpdateResultWith 的薄封装，保留旧调用点的语义。
// 后续可逐步替换调用方为 compositionUpdateResultWith。
func (c *Coordinator) modeCompositionResult(text string, caretPos int) *bridge.KeyEventResult {
	return c.compositionUpdateResultWith(text, caretPos)
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
	committed := ""
	if ops.committed != nil {
		committed = *ops.committed
	}
	preedit := prefix + committed + c.preeditDisplay
	if c.preeditDisplay == "" && len(*ops.buffer) > 0 {
		preedit = prefix + committed + *ops.buffer
	} else if len(*ops.buffer) == 0 {
		preedit = prefix + committed
	}

	// 计算候选窗中的光标位置（考虑 committed 偏移和 preeditDisplay 中的分隔符偏移）
	prefixCommittedLen := len(prefix) + len(committed)
	preeditCaret := len(preedit)
	if ops.cursorPos != nil {
		if c.preeditDisplay != "" {
			preeditCaret = prefixCommittedLen + mapBufferPosToDisplayPos(*ops.buffer, c.preeditDisplay, *ops.cursorPos)
		} else {
			preeditCaret = prefixCommittedLen + *ops.cursorPos
		}
	}

	modeLabel := "临时拼音"
	if c.engineMgr != nil {
		modeLabel = c.engineMgr.GetTempPinyinModeLabel()
	}
	c.uiManager.SetModeLabel(modeLabel)
	c.uiManager.SetModeAccentColor(c.modeAccentColor("temp_pinyin"))
	c.uiManager.ShowCandidates(
		displayCandidates,
		preedit,
		preeditCaret,
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

	separatorMode := config.PinyinSeparatorAuto
	if c.config != nil && c.config.Input.PinyinSeparator != "" {
		separatorMode = c.config.Input.PinyinSeparator
	}

	switch separatorMode {
	case config.PinyinSeparatorNone:
		return false
	case config.PinyinSeparatorQuote:
		return key == "'" || uint32(keyCode) == ipc.VK_OEM_7
	case config.PinyinSeparatorBacktick:
		return key == "`" || uint32(keyCode) == ipc.VK_OEM_3
	case config.PinyinSeparatorAuto:
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
