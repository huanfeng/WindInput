// handle_candidates.go — 候选词管理、分页、组合文本与 UI 显示
package coordinator

import (
	"strings"
	"unicode/utf8"

	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/ui"
)

// confirmedPrefix 返回所有已确认段的汉字拼接文本。
func (c *Coordinator) confirmedPrefix() string {
	if len(c.confirmedSegments) == 0 {
		return ""
	}
	var b strings.Builder
	for _, seg := range c.confirmedSegments {
		b.WriteString(seg.Text)
	}
	return b.String()
}

// compositionText 返回当前应显示的组合文本。
// 拼音分步确认时，前缀为已确认的汉字，后跟活动编码的拼音显示。
// 拼音模式返回带音节分隔符的文本（如 "zhong guo"），五笔或未解析时 fallback 到 inputBuffer。
func (c *Coordinator) compositionText() string {
	prefix := c.confirmedPrefix()
	if c.preeditDisplay != "" {
		display := c.preeditDisplay
		// 如果 inputBuffer 以 ' 结尾但 preeditDisplay 没有，补上尾部的 '
		// （用户刚输入分隔符但还没有后续字符，引擎的 preedit 不含尾部分隔符）
		if strings.HasSuffix(c.inputBuffer, "'") && !strings.HasSuffix(display, "'") {
			display += "'"
		}
		return prefix + display
	}
	return prefix + c.inputBuffer
}

// calcSyllableBoundaries 从已完成音节和部分音节计算边界位置。
// 边界位置是 inputBuffer 中每对相邻音节段之间的字节偏移。
// 例如 ["zhong", "guo"] partial="" → [5]；["ni", "hao"] partial="zh" → [2, 5]
func (c *Coordinator) calcSyllableBoundaries(completedSyllables []string, partialSyllable string) []int {
	segments := make([]string, 0, len(completedSyllables)+1)
	segments = append(segments, completedSyllables...)
	if partialSyllable != "" {
		segments = append(segments, partialSyllable)
	}
	if len(segments) <= 1 {
		return nil
	}
	boundaries := make([]int, 0, len(segments)-1)
	pos := 0
	for i := 0; i < len(segments)-1; i++ {
		pos += len(segments[i])
		boundaries = append(boundaries, pos)
	}
	return boundaries
}

// displayCursorPos 将 inputCursorPos（基于 inputBuffer 的字节位置）映射到组合显示文本中的
// 光标位置。返回值是 rune 计数（即 UTF-16 code unit 计数，对 BMP 字符），
// 与 C++ TSF 侧的 wstring 偏移一致。
//
// 确认文本前缀是中文（每个汉字 UTF-8 3字节 = 1 rune），
// 拼音编码部分是纯 ASCII（每字节 = 1 rune），两者用不同方式计算偏移。
func (c *Coordinator) displayCursorPos() int {
	// 确认段前缀用 rune 计数（中文字符在 UTF-16 中也是 1 code unit）
	prefixRuneLen := 0
	for _, seg := range c.confirmedSegments {
		prefixRuneLen += utf8.RuneCountInString(seg.Text)
	}
	// inputBuffer 是纯 ASCII (a-z, ')，字节数 == rune 数
	if c.preeditDisplay == "" {
		return prefixRuneLen + c.inputCursorPos
	}
	offset := 0
	for _, b := range c.syllableBoundaries {
		if b <= c.inputCursorPos {
			offset++
		}
	}
	return prefixRuneLen + c.inputCursorPos + offset
}

func (c *Coordinator) updateCandidates() {
	c.updateCandidatesEx()
}

func (c *Coordinator) updateCandidatesEx() *engine.ConvertResult {
	if len(c.inputBuffer) == 0 {
		c.candidates = nil
		return nil
	}

	if c.engineMgr == nil {
		return nil
	}

	// 使用扩展转换获取更多信息
	result := c.engineMgr.ConvertEx(c.inputBuffer, 0)

	// 更新预编辑显示状态
	c.preeditDisplay = result.PreeditDisplay
	// 安全校验：去除分隔符后应与 inputBuffer（同样去掉分隔符）一致，否则 fallback
	// preeditDisplay 中自动切分用空格、用户分隔符用 '，inputBuffer 中用户分隔符用 '
	// 两边都需要去掉 ' 和空格后再比较
	if c.preeditDisplay != "" {
		stripped := strings.ReplaceAll(strings.ReplaceAll(c.preeditDisplay, "'", ""), " ", "")
		inputStripped := strings.ReplaceAll(strings.ToLower(c.inputBuffer), "'", "")
		if stripped != inputStripped {
			c.preeditDisplay = ""
			c.syllableBoundaries = nil
		} else {
			c.syllableBoundaries = c.calcSyllableBoundaries(
				result.CompletedSyllables, result.PartialSyllable)
		}
	} else {
		c.syllableBoundaries = nil
	}

	// Convert to UI candidates
	// Check shadow layer for HasShadow flags
	var shadowLayer interface {
		HasRule(code string, word string) bool
	}
	if c.engineMgr != nil {
		shadowLayer = c.engineMgr.GetDictManager().GetShadowLayer()
	}

	c.candidates = make([]ui.Candidate, len(result.Candidates))
	for i, ec := range result.Candidates {
		code := ec.Code
		if code == "" {
			code = c.inputBuffer
		}
		cand := ui.Candidate{
			Text:           ec.Text,
			Code:           ec.Code,
			Index:          i + 1,
			Weight:         ec.Weight,
			IsCommand:      ec.IsCommand,
			ConsumedLength: ec.ConsumedLength,
			Source:         string(ec.Source),
		}
		// HasShadow 统一用 inputBuffer 查询（Shadow 规则按当前输入编码存储）
		if shadowLayer != nil && !ec.IsCommand {
			cand.HasShadow = shadowLayer.HasRule(c.inputBuffer, ec.Text)
		}
		// 如果有提示信息（如反查编码），添加到注释
		if ec.Hint != "" {
			cand.Comment = ec.Hint
		}
		c.candidates[i] = cand
	}

	c.logger.Debug("Got candidates", "count", len(c.candidates), "empty", result.IsEmpty,
		"input", c.inputBuffer, "preedit", c.preeditDisplay)
	// Debug: log top 3 candidates for ranking investigation
	for i := 0; i < len(c.candidates) && i < 3; i++ {
		cand := c.candidates[i]
		c.logger.Debug("Candidate", "rank", i+1, "text", cand.Text, "weight", cand.Weight,
			"code", cand.Code, "consumed", cand.ConsumedLength)
	}

	// Calculate pagination
	c.totalPages = (len(c.candidates) + c.candidatesPerPage - 1) / c.candidatesPerPage
	if c.totalPages == 0 {
		c.totalPages = 1
	}
	c.currentPage = 1
	c.selectedIndex = 0

	return result
}

func (c *Coordinator) showUI() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		c.logger.Warn("UI manager not ready")
		return
	}

	// 设置拼音模式标记（影响右键菜单前移/后移启用状态）
	isPinyin := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin
	c.uiManager.SetPinyinMode(isPinyin)

	// When InlinePreedit is enabled and there are no candidates,
	// hide the candidate window (only show the inline preedit in the application)
	if c.config != nil && c.config.UI.InlinePreedit && len(c.candidates) == 0 {
		c.hideUI()
		return
	}

	// Get current page candidates
	startIdx := (c.currentPage - 1) * c.candidatesPerPage
	endIdx := startIdx + c.candidatesPerPage
	if endIdx > len(c.candidates) {
		endIdx = len(c.candidates)
	}

	var pageCandidates []ui.Candidate
	if startIdx < len(c.candidates) {
		pageCandidates = c.candidates[startIdx:endIdx]
	}

	// Re-index for display (1-9, 0 for 10th)
	displayCandidates := make([]ui.Candidate, len(pageCandidates))
	for i, cand := range pageCandidates {
		displayCandidates[i] = ui.Candidate{
			Text:      cand.Text,
			Code:      cand.Code,
			Index:     (i + 1) % 10,
			Comment:   cand.Comment,
			Weight:    cand.Weight,
			HasShadow: cand.HasShadow,
		}
	}

	// Use caret position for candidate window placement
	// The UI manager will handle boundary detection and position adjustment
	// When inline preedit is enabled, anchor the window at the composition start position
	// instead of following the current caret (which moves as the user types)
	caretX := c.caretX
	caretY := c.caretY
	caretHeight := c.caretHeight
	if c.config != nil && c.config.UI.InlinePreedit && c.compositionStartValid {
		caretX = c.compositionStartX
		caretY = c.compositionStartY
	}

	// Multi-monitor support: coordinates can be negative (monitors to the left/above primary)
	// Only use fallback if we haven't received valid caret info yet (both X and Y are 0)
	// or if coordinates are extremely large (likely garbage values)
	const maxCoord = 32000 // Windows virtual screen limit is typically around 32767
	if (c.caretX == 0 && c.caretY == 0) || caretX > maxCoord || caretX < -maxCoord || caretY > maxCoord || caretY < -maxCoord {
		// Use last known good position or a reasonable default
		if c.lastValidX != 0 || c.lastValidY != 0 {
			caretX = c.lastValidX
			caretY = c.lastValidY
			caretHeight = 20 // Default height for fallback
		} else {
			// Fallback to a safe position on primary monitor
			caretX = 400
			caretY = 300
			caretHeight = 20
		}
		c.logger.Debug("Using fallback position", "caretX", caretX, "caretY", caretY)
	} else {
		// Save valid position for future fallback
		c.lastValidX = caretX
		c.lastValidY = caretY
	}

	c.uiManager.ShowCandidates(
		displayCandidates,
		c.compositionText(),
		c.displayCursorPos(),
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

// getIndicatorPosition returns the unified position for all status indicators.
// Falls back to lastValid or default position if current caret position is invalid.
func (c *Coordinator) getIndicatorPosition() (x, y int) {
	x = c.caretX
	y = c.caretY
	const maxCoord = 32000
	if (c.caretX == 0 && c.caretY == 0) || x > maxCoord || x < -maxCoord || y > maxCoord || y < -maxCoord {
		if c.lastValidX != 0 || c.lastValidY != 0 {
			x = c.lastValidX
			y = c.lastValidY
		} else {
			x = 400
			y = 300
		}
	}
	return x, y
}

func (c *Coordinator) showModeIndicator() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// Build composite mode text: Chinese mode shows "中·五笔" or "中·拼音", English mode shows "En"
	var modeText string
	if !c.chineseMode {
		modeText = "英"
	} else if c.engineMgr != nil {
		switch c.engineMgr.GetCurrentType() {
		case engine.EngineTypeWubi:
			modeText = "中·五笔"
		case engine.EngineTypePinyin:
			modeText = "中·拼音"
		case engine.EngineTypeMixed:
			modeText = "中·混输"
		default:
			modeText = "中"
		}
	} else {
		modeText = "中"
	}

	x, y := c.getIndicatorPosition()
	c.uiManager.ShowModeIndicator(modeText, x, y)
}

func (c *Coordinator) hideUI() {
	if c.uiManager != nil {
		c.uiManager.Hide()
		c.uiManager.HideTooltip()
	}
}
