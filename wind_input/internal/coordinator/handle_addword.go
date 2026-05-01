package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/encoding"
)

const (
	addWordMinLen     = 1    // 最小加词长度
	addWordDefaultLen = 2    // 默认加词长度
	addWordMaxWeight  = 1200 // 手动加词默认权重（略高于系统词库归一化中位 1000）
)

// addWordCompositionPlaceholder 加词模式使用的占位 composition 文本
// 设置非空的 inputBuffer 让 C++ 侧知道 IME 处于 composition 状态，从而转发所有按键
const addWordCompositionPlaceholder = "\x00"

// getAddWordMaxLen 获取加词最大字数（受 encoder 配置限制）
func (c *Coordinator) getAddWordMaxLen() int {
	maxLen := 20 // 默认上限
	if c.engineMgr != nil {
		if encoderMax := c.engineMgr.GetEncoderMaxWordLength(); encoderMax > 0 {
			maxLen = encoderMax
		}
	}
	return maxLen
}

// enterAddWordMode 进入加词模式
func (c *Coordinator) enterAddWordMode() *bridge.KeyEventResult {
	if c.hasPendingInput() {
		c.clearState()
		c.hideUI()
	}

	maxLen := c.getAddWordMaxLen()
	chars := c.inputHistory.GetRecentChars(maxLen, 0)

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

	maxLen := c.getAddWordMaxLen()
	newLen := c.addWordLen + delta
	if newLen < addWordMinLen {
		newLen = addWordMinLen
	}
	if newLen > len(c.addWordChars) {
		newLen = len(c.addWordChars)
	}
	if newLen > maxLen {
		newLen = maxLen
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

	if err := dictMgr.AddUserWord(code, word, addWordMaxWeight); err != nil {
		c.logger.Warn("addword: failed to add word", "error", err)
	} else {
		c.logger.Info("addword: word added successfully", "wordLen", len([]rune(word)), "codeLen", len(code))
		// 旁路 RPC 直接写库后需手动广播 dict-event，否则设置端用户词库视图不刷新
		if c.eventNotifier != nil {
			schemaID := ""
			if c.engineMgr != nil {
				schemaID = c.engineMgr.GetCurrentSchemaID()
			}
			c.eventNotifier.NotifyUserDictAdd(schemaID)
		}
	}

	c.exitAddWordMode()
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// openAddWordDialog 打开加词对话框
func (c *Coordinator) openAddWordDialog() *bridge.KeyEventResult {
	word := ""
	code := ""
	schemaID := ""
	if c.addWordLen >= addWordMinLen && len(c.addWordChars) >= addWordMinLen {
		word = string(c.addWordChars[len(c.addWordChars)-c.addWordLen:])
		code = c.addWordCode
	}
	if c.engineMgr != nil {
		schemaID = c.engineMgr.GetCurrentSchemaID()
	}

	c.exitAddWordMode()

	if c.uiManager != nil {
		// 构造参数：--page=add-word --text=xxx --code=xxx --schema=xxx
		// ShellExecute 会自动按空格拆分为独立的命令行参数
		page := "add-word"
		if word != "" {
			page += " --text=" + word
		}
		if code != "" {
			page += " --code=" + code
		}
		if schemaID != "" {
			page += " --schema=" + schemaID
		}
		c.uiManager.OpenSettingsWithPage(page)
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

	// 转换 schema.EncoderRule → encoding.SchemaEncoderRule → encoding.Rule
	encRules := make([]encoding.SchemaEncoderRule, len(schemaRules))
	for i, sr := range schemaRules {
		encRules[i] = encoding.SchemaEncoderRule{
			LengthEqual:   sr.LengthEqual,
			LengthInRange: sr.LengthInRange,
			Formula:       sr.Formula,
		}
	}
	rules := encoding.ConvertSchemaRules(encRules)

	encoder := encoding.NewReverseEncoder(reverseIndex, rules)
	code, err := encoder.Encode(word)
	if err != nil {
		c.logger.Debug("addword: encode failed", "word", word, "error", err)
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
			{Text: "[快捷加词] 无最近输入", Index: -1},
			{Index: -1, Comment: "请先输入文字后再使用  Esc关闭"},
		}
	} else {
		word := string(c.addWordChars[len(c.addWordChars)-c.addWordLen:])
		comment := "无法计算编码"
		if c.addWordCode != "" {
			comment = "编码: " + c.addWordCode
		}
		candidates = []ui.Candidate{
			{Text: "[快捷加词] " + word, Index: -1, Comment: comment},
			{Index: -1, Comment: "↑↓调整长度  Enter添加  Ctrl+Enter编辑  Esc取消"},
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
