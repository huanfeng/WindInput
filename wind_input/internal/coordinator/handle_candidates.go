// handle_candidates.go — 候选词管理、分页、组合文本与 UI 显示
package coordinator

import (
	"strings"
	"unicode/utf8"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/internal/transform"
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

// calcShuangpinBoundaries 从 preedit 文本反推音节边界位置。
// 按空格分割 preedit 得到各段长度，累加得到 inputBuffer 中的边界偏移。
// 这样边界与 preedit 显示始终同步，无论段是 1 键（简拼）还是 2 键（有效键对）。
func (c *Coordinator) calcShuangpinBoundaries() []int {
	segments := strings.Split(c.preeditDisplay, " ")
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

// displayCursorPos 将 inputCursorPos 映射为 TSF/UTF-16 wstring 偏移（rune 计数）。
// 确认段中文前缀按 rune 数计算（BMP 汉字 = 1 UTF-16 code unit = 1 rune），
// 拼音编码是纯 ASCII（字节数 == rune 数），两者累加即为 wstring 偏移。
func (c *Coordinator) displayCursorPos() int {
	prefixRuneLen := 0
	for _, seg := range c.confirmedSegments {
		prefixRuneLen += utf8.RuneCountInString(seg.Text)
	}
	if c.preeditDisplay == "" {
		return prefixRuneLen + c.inputCursorPos
	}
	return prefixRuneLen + mapBufferPosToDisplayPos(c.inputBuffer, c.preeditDisplay, c.inputCursorPos)
}

// uiCursorPos 将 inputCursorPos 映射为候选窗预编辑文本的 UTF-8 字节偏移。
// 确认段中文前缀按 UTF-8 字节数计算（"中" = 3 字节），
// 使候选窗光标落在正确位置而非偏前。
func (c *Coordinator) uiCursorPos() int {
	prefixByteLen := 0
	for _, seg := range c.confirmedSegments {
		prefixByteLen += len(seg.Text) // UTF-8 字节数，非 rune 数
	}
	if c.preeditDisplay == "" {
		return prefixByteLen + c.inputCursorPos
	}
	return prefixByteLen + mapBufferPosToDisplayPos(c.inputBuffer, c.preeditDisplay, c.inputCursorPos)
}

func (c *Coordinator) updateCandidates() {
	c.updateCandidatesEx()
}

func (c *Coordinator) updateCandidatesEx() *engine.ConvertResult {
	if len(c.inputBuffer) == 0 {
		c.candidates = nil
		c.candidateLimit = 0
		c.candidateInput = ""
		c.hasMoreCandidates = false
		return nil
	}

	if c.engineMgr == nil {
		return nil
	}

	// Z 键重复上屏：当输入为 "z" 且方案启用了该功能时，
	// 将上一次上屏的内容作为首选候选插入到候选列表顶部。
	zKeyRepeat := c.inputBuffer == "z" && c.engineMgr.IsZKeyRepeatEnabled()

	// 分级加载：拼音/混输引擎首次加载 300 条；码表引擎短前缀（1字符→100条，2字符→300条）也限制初始量
	// 首键（composition 开始后的第一个按键）使用更小的限制以降低前缀查找延迟
	initialLimit := 0
	firstKey := c.pendingFirstKey
	c.pendingFirstKey = false
	switch c.engineMgr.GetCurrentType() {
	case engine.EngineTypePinyin, engine.EngineTypeMixed:
		if firstKey {
			initialLimit = 50
		} else {
			initialLimit = 300
		}
	case engine.EngineTypeCodetable:
		inputLen := len(c.inputBuffer)
		if inputLen <= 1 {
			if firstKey {
				initialLimit = 50
			} else {
				initialLimit = 100
			}
		} else if inputLen == 2 {
			initialLimit = 300
		}
	}
	c.candidateLimit = initialLimit
	c.candidateInput = c.inputBuffer

	// 使用扩展转换获取更多信息
	result := c.engineMgr.ConvertEx(c.inputBuffer, initialLimit)

	// 分级加载：判断是否还有更多候选未加载
	c.hasMoreCandidates = initialLimit > 0 && len(result.Candidates) >= initialLimit

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
		} else if result.FullPinyinInput != "" {
			// 双拼模式：每个音节固定 2 键，按键对计算边界
			c.syllableBoundaries = c.calcShuangpinBoundaries()
		} else {
			c.syllableBoundaries = c.calcSyllableBoundaries(
				result.CompletedSyllables, result.PartialSyllable)
		}
	} else {
		c.syllableBoundaries = nil
	}

	// Convert to UI candidates
	// Check shadow layer for HasShadow flags
	var dictMgr *dict.DictManager
	if c.engineMgr != nil {
		dictMgr = c.engineMgr.GetDictManager()
	}

	c.candidates = make([]ui.Candidate, len(result.Candidates))
	for i, cand := range result.Candidates {
		cand.Index = i + 1
		// HasShadow 统一用 inputBuffer 查询（Shadow 规则按当前输入编码存储）
		if cand.IsCommand && cand.PhraseTemplate != "" {
			// 命令候选：检查 PhraseLayer 是否有用户覆盖
			if dictMgr != nil {
				phraseLayer := dictMgr.GetPhraseLayer()
				if phraseLayer != nil {
					cand.HasShadow = phraseLayer.HasPhraseOverride(c.inputBuffer, cand.PhraseTemplate)
				}
			}
		} else if dictMgr != nil && !cand.IsCommand {
			cand.HasShadow = dictMgr.HasShadowRule(c.inputBuffer, cand.Text)
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
			// Z键混合模式（重复+临时拼音同时启用）：只显示重复候选，
			// 后续字母键切入临时拼音，与快捷输入模式行为一致
			if c.isZKeyHybridMode() {
				c.candidates = []ui.Candidate{repeatCand}
			} else {
				c.candidates = append([]ui.Candidate{repeatCand}, c.candidates...)
			}
			// 重新编号
			for i := range c.candidates {
				c.candidates[i].Index = i + 1
			}
			// 插入重复候选后不再是空码
			result.IsEmpty = false
		}
	}

	// 简入繁出：候选词文本转换（仅 S2T 启用时生效）
	c.applyS2TToCandidates()

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
	c.uiManager.SetModeLabel("")        // 正常模式不显示模式标签
	c.uiManager.SetModeAccentColor(nil) // 正常模式无光效

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

	// 分级加载：负值 totalPages 表示还有更多候选未加载
	displayTotalPages := c.totalPages
	if c.hasMoreCandidates {
		displayTotalPages = -c.totalPages
	}

	c.uiManager.ShowCandidates(
		displayCandidates,
		c.compositionText(),
		c.uiCursorPos(),
		caretX,
		caretY,
		caretHeight,
		c.currentPage,
		displayTotalPages,
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
	if c.isEffectiveChinesePunct() {
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

// expandCandidates 扩展候选列表（翻页到边界时调用）
func (c *Coordinator) expandCandidates() {
	if !c.hasMoreCandidates || c.candidateInput != c.inputBuffer {
		return
	}

	// 每次扩展翻倍，上限 5000
	newLimit := c.candidateLimit * 2
	if newLimit > 5000 {
		newLimit = 5000
	}
	if newLimit <= c.candidateLimit {
		c.hasMoreCandidates = false
		return
	}

	result := c.engineMgr.ConvertEx(c.inputBuffer, newLimit)
	if result == nil || len(result.Candidates) <= len(c.candidates) {
		c.hasMoreCandidates = false
		return
	}

	c.candidateLimit = newLimit
	c.hasMoreCandidates = len(result.Candidates) >= newLimit

	// 重建 UI 候选列表
	var dictMgr *dict.DictManager
	if c.engineMgr != nil {
		dictMgr = c.engineMgr.GetDictManager()
	}

	c.candidates = make([]ui.Candidate, len(result.Candidates))
	for i, cand := range result.Candidates {
		cand.Index = i + 1
		if cand.IsCommand && cand.PhraseTemplate != "" {
			if dictMgr != nil {
				phraseLayer := dictMgr.GetPhraseLayer()
				if phraseLayer != nil {
					cand.HasShadow = phraseLayer.HasPhraseOverride(c.inputBuffer, cand.PhraseTemplate)
				}
			}
		} else if dictMgr != nil && !cand.IsCommand {
			cand.HasShadow = dictMgr.HasShadowRule(c.inputBuffer, cand.Text)
		}
		c.candidates[i] = cand
	}

	// 重新计算分页（保持当前页不变）
	c.totalPages = (len(c.candidates) + c.candidatesPerPage - 1) / c.candidatesPerPage
	if c.totalPages == 0 {
		c.totalPages = 1
	}

	c.logger.Debug("Expanded candidates", "count", len(c.candidates),
		"limit", newLimit, "hasMore", c.hasMoreCandidates)
}

// isInlinePreedit 返回是否启用嵌入编码（编码文本嵌入到宿主应用光标处）。
// 这是 InlinePreedit 配置的唯一判定入口；config 为 nil 时按默认值（true）处理，
// 与 config.go 默认值保持一致，避免散落判断中 nil 方向不一致的问题。
func (c *Coordinator) isInlinePreedit() bool {
	if c.config == nil {
		return true
	}
	return c.config.UI.InlinePreedit
}

// compositionUpdateResult 构建主输入流程的 UpdateComposition 响应。
// 等价于 compositionUpdateResultWith(c.compositionText(), c.displayCursorPos())。
func (c *Coordinator) compositionUpdateResult() *bridge.KeyEventResult {
	return c.compositionUpdateResultWith(c.compositionText(), c.displayCursorPos())
}

// compositionUpdateResultWith 用给定的 preedit 文本和光标位置构建 UpdateComposition 响应，
// 遵循 InlinePreedit 配置：关闭时发送空文本（避免编码嵌入应用），但仍发送 UpdateComposition
// 以保持 TSF 端 _isComposing/_hasCandidates 激活，确保后续按键能被拦截。
// 用于临时英文/临时拼音/快捷输入等需要自定义 preedit 文本（含触发键 prefix）的模式。
func (c *Coordinator) compositionUpdateResultWith(text string, caretPos int) *bridge.KeyEventResult {
	if !c.isInlinePreedit() {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeUpdateComposition}
	}
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     text,
		CaretPos: caretPos,
	}
}

func (c *Coordinator) hideUI() {
	if c.uiManager != nil {
		c.uiManager.Hide()
		c.uiManager.HideTooltip()
	}
}

// doSelectCandidate 是候选词选择的统一核心实现（调用方须持锁）。
// 处理组候选展开、拼音分步确认、完整上屏三种情形，
// 包含学习回调、输入历史记录和统计上报，返回需交付给 TSF 的结果。
func (c *Coordinator) doSelectCandidate(index int) *bridge.KeyEventResult {
	if index < 0 || index >= len(c.candidates) {
		return nil
	}
	cand := c.candidates[index]
	c.logger.Debug("Candidate selected", "index", index)

	// ── 组候选：替换 inputBuffer 为组的完整编码，触发二级展开 ──────────────
	if cand.IsGroup && cand.GroupCode != "" {
		c.inputBuffer = cand.GroupCode
		c.inputCursorPos = len(c.inputBuffer)
		c.currentPage = 1
		c.selectedIndex = 0
		c.updateCandidates()
		c.showUI()
		return c.compositionUpdateResult()
	}

	originalText := cand.Text
	text := originalText
	if c.fullWidth {
		text = transform.ToFullWidth(text)
	}

	isPinyin := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypePinyin
	isMixed := c.engineMgr != nil && c.engineMgr.GetCurrentType() == engine.EngineTypeMixed

	// ── 拼音分步确认：候选消耗长度 < 缓冲区长度，暂存已确认段 ──────────────
	if (isPinyin || (isMixed && cand.ConsumedLength > 0)) &&
		cand.ConsumedLength > 0 && cand.ConsumedLength < len(c.inputBuffer) {

		consumedCode := c.inputBuffer[:cand.ConsumedLength]
		if !cand.IsCommand {
			c.engineMgr.OnCandidateSelected(consumedCode, originalText, cand.Source)
		}

		remaining := c.inputBuffer[cand.ConsumedLength:]
		c.logger.Debug("Partial confirm (pinyin)", "index", index, "text", text,
			"consumed", cand.ConsumedLength, "remaining", remaining,
			"confirmedCount", len(c.confirmedSegments)+1)

		c.confirmedSegments = append(c.confirmedSegments, ConfirmedSegment{
			Text:         originalText,
			ConsumedCode: consumedCode,
		})
		c.inputBuffer = remaining
		c.inputCursorPos = len(remaining)
		c.currentPage = 1
		c.updateCandidates()
		c.showUI()
		return c.compositionUpdateResult()
	}

	// 预计算分步确认场景下的合并编码和文本，供学习回调和历史记录共用
	// （学习和历史记录基于原始文本，不受 fullWidth 显示变换影响）
	var segCode, segText string
	if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
		var codeBuilder, textBuilder strings.Builder
		for _, seg := range c.confirmedSegments {
			codeBuilder.WriteString(seg.ConsumedCode)
			textBuilder.WriteString(seg.Text)
		}
		segCode = codeBuilder.String()
		segText = textBuilder.String()
	}

	// ── 完全消费：学习回调 ────────────────────────────────────────────────
	if c.engineMgr != nil && !cand.IsCommand {
		if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
			c.engineMgr.OnCandidateSelected(segCode+c.inputBuffer, segText+originalText, cand.Source)
		} else {
			selectedCode := c.inputBuffer
			if cand.Code != "" {
				selectedCode = cand.Code
			}
			c.engineMgr.OnCandidateSelected(selectedCode, originalText, cand.Source)
		}
	}

	// ── 输入历史记录（用于加词推荐）────────────────────────────────────────
	if c.inputHistory != nil && !cand.IsCommand {
		histText := originalText
		histCode := c.inputBuffer
		if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
			histText = segText + originalText
			histCode = segCode + c.inputBuffer
		}
		c.inputHistory.Record(histText, histCode, "", 0)
	}

	// ── 拼接已确认段 + 当前候选，构建最终上屏文本 ──────────────────────────
	finalText := text
	if (isPinyin || isMixed) && len(c.confirmedSegments) > 0 {
		var sb strings.Builder
		for _, seg := range c.confirmedSegments {
			t := seg.Text
			if c.fullWidth {
				t = transform.ToFullWidth(t)
			}
			sb.WriteString(t)
		}
		finalText = sb.String() + text
	}

	c.logger.Debug("Candidate selected (full commit)", "index", index,
		"original", originalText, "output", finalText,
		"fullWidth", c.fullWidth, "confirmedSegments", len(c.confirmedSegments))

	c.recordCommit(finalText, len(c.inputBuffer), index%c.candidatesPerPage, store.SourceCandidate)
	c.clearState()
	c.hideUI()

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: finalText,
	}
}
