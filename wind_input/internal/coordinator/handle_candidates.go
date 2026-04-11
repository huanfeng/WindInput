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

	// Z 键重复上屏：当输入为 "z" 且方案启用了该功能时，
	// 将上一次上屏的内容作为首选候选插入到候选列表顶部。
	zKeyRepeat := c.inputBuffer == "z" && c.engineMgr.IsZKeyRepeatEnabled()

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
	for i, cand := range result.Candidates {
		cand.Index = i + 1
		// HasShadow 统一用 inputBuffer 查询（Shadow 规则按当前输入编码存储）
		if cand.IsCommand && cand.PhraseTemplate != "" {
			// 命令候选：检查 PhraseLayer 是否有用户覆盖
			if c.engineMgr != nil {
				phraseLayer := c.engineMgr.GetDictManager().GetPhraseLayer()
				if phraseLayer != nil {
					cand.HasShadow = phraseLayer.HasPhraseOverride(c.inputBuffer, cand.PhraseTemplate)
				}
			}
		} else if shadowLayer != nil && !cand.IsCommand {
			cand.HasShadow = shadowLayer.HasRule(c.inputBuffer, cand.Text)
		}
		c.candidates[i] = cand
	}

	// Z 键重复上屏：将上一次上屏的内容作为首选候选插入到列表顶部
	if zKeyRepeat && c.inputHistory != nil {
		records := c.inputHistory.GetRecentRecords(1, 0)
		if len(records) > 0 {
			repeatCand := ui.Candidate{
				Text:   records[0].Text,
				Code:   "z",
				Index:  1,
				Weight: 999999999, // 确保排在最前
			}
			c.candidates = append([]ui.Candidate{repeatCand}, c.candidates...)
			// 重新编号
			for i := range c.candidates {
				c.candidates[i].Index = i + 1
			}
			// 插入重复候选后不再是空码
			result.IsEmpty = false
		}
	}

	c.logger.Debug("Got candidates", "count", len(c.candidates), "empty", result.IsEmpty,
		"input", c.inputBuffer, "preedit", c.preeditDisplay)
	// Debug: log top 3 candidates for ranking investigation (use engine result for NaturalOrder)
	for i := 0; i < len(result.Candidates) && i < 3; i++ {
		ec := result.Candidates[i]
		c.logger.Debug("Candidate", "rank", i+1, "text", ec.Text, "weight", ec.Weight,
			"code", ec.Code, "naturalOrder", ec.NaturalOrder, "consumed", ec.ConsumedLength)
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

	// Re-evaluate host render right before painting. This self-heals cases where
	// focus/lifecycle events temporarily cleared the UI callback after host render
	// had already been set up for the active process.
	c.updateHostRenderState()

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
	copy(displayCandidates, pageCandidates)
	for i := range displayCandidates {
		displayCandidates[i].Index = (i + 1) % 10
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

// updateStatusIndicator 更新状态提示（合并显示输入模式+标点+全半角）
func (c *Coordinator) updateStatusIndicator() {
	if c.uiManager == nil || !c.uiManager.IsReady() {
		return
	}

	// 确保 host render 状态是最新的
	c.updateHostRenderState()

	state := ui.StatusState{
		ModeLabel:  c.getStatusModeLabel(),
		PunctLabel: c.getStatusPunctLabel(),
		WidthLabel: c.getStatusWidthLabel(),
	}

	x, y := c.getIndicatorPosition()
	c.uiManager.ShowStatusIndicator(state, x, y)
}

// getStatusModeLabel 获取模式标签（支持简写/全称，CapsLock 时返回 "A"）
func (c *Coordinator) getStatusModeLabel() string {
	if c.capsLockOn {
		return "A"
	}
	if !c.chineseMode {
		return "英"
	}
	if c.engineMgr != nil {
		name, iconLabel := c.engineMgr.GetSchemaDisplayInfo()
		style := c.config.UI.StatusIndicator.SchemaNameStyle
		if style == "short" && iconLabel != "" {
			return iconLabel
		}
		if name != "" {
			return name
		}
	}
	return "中"
}

// getStatusPunctLabel 获取标点状态标签
func (c *Coordinator) getStatusPunctLabel() string {
	if c.chinesePunctuation {
		return "。"
	}
	return "."
}

// getStatusWidthLabel 获取全半角状态标签
// 全角: ● (实心圆), 半角: ◑ (半实心圆)，始终显示以保持统一
func (c *Coordinator) getStatusWidthLabel() string {
	if c.fullWidth {
		return "●"
	}
	return "◑"
}

// showModeIndicator 向后兼容，转发到 updateStatusIndicator
func (c *Coordinator) showModeIndicator() {
	c.updateStatusIndicator()
}

func (c *Coordinator) hideUI() {
	if c.uiManager != nil {
		c.uiManager.Hide()
		c.uiManager.HideTooltip()
	}
}
