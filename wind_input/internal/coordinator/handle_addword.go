package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/encoding"
)

const (
	addWordMinLen     = 2    // 最小加词长度
	addWordDefaultLen = 2    // 默认加词长度
	addWordMaxWeight  = 5000 // 手动加词默认权重
)

// addWordCompositionPlaceholder 加词模式使用的占位 composition 文本
// 设置非空的 inputBuffer 让 C++ 侧知道 IME 处于 composition 状态，从而转发所有按键
const addWordCompositionPlaceholder = "\x00"

// enterAddWordMode 进入加词模式
func (c *Coordinator) enterAddWordMode() *bridge.KeyEventResult {
	if c.hasPendingInput() {
		c.clearState()
		c.hideUI()
	}

	chars := c.inputHistory.GetRecentChars(20, 0)

	c.addWordActive = true
	c.addWordChars = chars

	if len(chars) < addWordMinLen {
		c.addWordLen = 0
		c.addWordCode = ""
	} else {
		c.addWordLen = addWordDefaultLen
		if c.addWordLen > len(chars) {
			c.addWordLen = len(chars)
		}
		c.updateAddWordCode()
	}

	// 设置占位 inputBuffer，让 C++ 侧认为 IME 处于 composition 状态
	// 从而转发后续的 ↑↓/Enter/Esc 等按键给 Go 处理
	c.inputBuffer = addWordCompositionPlaceholder
	c.inputCursorPos = 0

	c.showAddWordPreview()

	// 返回 UpdateComposition 激活 C++ 侧的 composition 模式
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     " ",
		CaretPos: 0,
	}
}

// exitAddWordMode 退出加词模式
func (c *Coordinator) exitAddWordMode() {
	c.addWordActive = false
	c.addWordChars = nil
	c.addWordLen = 0
	c.addWordCode = ""
	c.inputBuffer = ""
	c.inputCursorPos = 0
	c.hideUI()
}

// adjustAddWordLength 调整加词长度
func (c *Coordinator) adjustAddWordLength(delta int) *bridge.KeyEventResult {
	if len(c.addWordChars) < addWordMinLen {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	newLen := c.addWordLen + delta
	if newLen < addWordMinLen {
		newLen = addWordMinLen
	}
	if newLen > len(c.addWordChars) {
		newLen = len(c.addWordChars)
	}

	if newLen != c.addWordLen {
		c.addWordLen = newLen
		c.updateAddWordCode()
		c.showAddWordPreview()
	}

	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

// confirmAddWord 确认加词
func (c *Coordinator) confirmAddWord() *bridge.KeyEventResult {
	if c.addWordLen < addWordMinLen || len(c.addWordChars) < addWordMinLen {
		c.exitAddWordMode()
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
	}

	word := string(c.addWordChars[len(c.addWordChars)-c.addWordLen:])
	code := c.addWordCode

	if code == "" {
		c.logger.Warn("addword: cannot calculate code for word, aborting", "word", word)
		c.exitAddWordMode()
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
	}

	dictMgr := c.engineMgr.GetDictManager()
	if dictMgr == nil {
		c.logger.Warn("addword: dict manager not available")
		c.exitAddWordMode()
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
	}

	userDict := dictMgr.GetUserDict()
	if userDict == nil {
		c.logger.Warn("addword: user dict not available")
		c.exitAddWordMode()
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
	}

	if err := userDict.Add(code, word, addWordMaxWeight); err != nil {
		c.logger.Warn("addword: failed to add word", "word", word, "code", code, "error", err)
	} else {
		c.logger.Info("addword: word added successfully", "word", word, "code", code)
	}

	c.exitAddWordMode()
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// openAddWordDialog 打开加词对话框
func (c *Coordinator) openAddWordDialog() *bridge.KeyEventResult {
	c.exitAddWordMode()
	if c.uiManager != nil {
		c.uiManager.OpenSettingsWithPage("add-word")
	}
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// updateAddWordCode 更新当前加词的编码
func (c *Coordinator) updateAddWordCode() {
	if c.addWordLen < addWordMinLen || len(c.addWordChars) < c.addWordLen {
		c.addWordCode = ""
		return
	}
	word := string(c.addWordChars[len(c.addWordChars)-c.addWordLen:])
	c.addWordCode = c.calcWordCodeForCurrentSchema(word)
}

// calcWordCodeForCurrentSchema 根据当前方案计算词的编码
func (c *Coordinator) calcWordCodeForCurrentSchema(word string) string {
	schemaRules := c.engineMgr.GetEncoderRules()
	if len(schemaRules) == 0 {
		c.logger.Debug("addword: no encoder rules for current schema")
		return ""
	}

	reverseIndex := c.engineMgr.GetReverseIndex()
	if len(reverseIndex) == 0 {
		c.logger.Debug("addword: reverse index is empty")
		return ""
	}

	// 为每个字查找全码（优先取最长的编码，通常是 4 码全码）
	charCodes := make(map[string]string)
	for _, ch := range word {
		charStr := string(ch)
		if _, ok := charCodes[charStr]; ok {
			continue
		}
		codes, found := reverseIndex[charStr]
		if !found || len(codes) == 0 {
			c.logger.Debug("addword: no code found for char", "char", charStr)
			return ""
		}
		// 取最长的编码（全码）
		best := codes[0]
		for _, code := range codes {
			if len(code) > len(best) {
				best = code
			}
		}
		charCodes[charStr] = best
	}

	// 转换 schema.EncoderRule → encoding.Rule
	rules := make([]encoding.Rule, len(schemaRules))
	for i, sr := range schemaRules {
		rules[i] = encoding.Rule{
			LengthEqual: sr.LengthEqual,
			Formula:     sr.Formula,
		}
		if len(sr.LengthInRange) == 2 {
			rules[i].LengthRange = [2]int{sr.LengthInRange[0], sr.LengthInRange[1]}
		}
	}

	code, err := encoding.CalcWordCode(word, charCodes, rules)
	if err != nil {
		c.logger.Debug("addword: CalcWordCode failed", "word", word, "error", err)
		return ""
	}
	return code
}

// showAddWordPreview 显示加词预览候选窗
func (c *Coordinator) showAddWordPreview() {
	if c.uiManager == nil {
		return
	}

	var candidates []ui.Candidate

	if len(c.addWordChars) < addWordMinLen || c.addWordLen < addWordMinLen {
		candidates = []ui.Candidate{
			{Text: "[加词] 无最近输入", Index: -1},
			{Index: -1, Comment: "请先输入文字后再使用  Esc关闭"},
		}
	} else {
		word := string(c.addWordChars[len(c.addWordChars)-c.addWordLen:])
		comment := "无法计算编码"
		if c.addWordCode != "" {
			comment = "编码: " + c.addWordCode
		}
		candidates = []ui.Candidate{
			{Text: "[加词] " + word, Index: -1, Comment: comment},
			{Index: -1, Comment: "↑↓调整长度  Enter添加  Esc取消"},
		}
	}

	candidateCount := len(candidates)
	_ = c.uiManager.ShowCandidates(
		candidates,
		"",
		0,
		c.caretX, c.caretY, c.caretHeight,
		1, 1, candidateCount, candidateCount, 0,
	)
}

// handleAddWordKey 在加词模式下处理按键
func (c *Coordinator) handleAddWordKey(data bridge.KeyEventData) *bridge.KeyEventResult {
	hasCtrl := data.Modifiers&ModCtrl != 0
	vk := uint32(data.KeyCode)

	switch {
	case vk == ipc.VK_ESCAPE:
		c.exitAddWordMode()
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}

	case vk == ipc.VK_UP:
		return c.adjustAddWordLength(1)

	case vk == ipc.VK_DOWN:
		return c.adjustAddWordLength(-1)

	case vk == ipc.VK_RETURN && hasCtrl:
		return c.openAddWordDialog()

	case vk == ipc.VK_RETURN:
		return c.confirmAddWord()

	default:
		// 加词模式下消费所有按键，避免误操作退出
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
}
