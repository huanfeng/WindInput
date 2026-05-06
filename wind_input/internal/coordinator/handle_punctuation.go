// handle_punctuation.go — 标点处理、快捷键匹配、选择键、翻页键
package coordinator

import (
	"strings"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/keys"
)

// pairGroupVK 把 PairGroup 映射到一对 (firstVK, secondVK, firstChar, secondChar)。
// firstChar/secondChar 是该按键的"标点字符"形式（用于 key 字符串匹配，例如 ";"），
// 仅对字符可表示的键有意义；修饰键（LShift/RShift/LCtrl/RCtrl）的 char 为空字符串
// —— 此时只通过 VK 比对。
type pairVKEntry struct {
	firstVK    uint32
	secondVK   uint32
	firstChar  string
	secondChar string
}

var pairGroupVK = map[keys.PairGroup]pairVKEntry{
	keys.PairSemicolonQuote: {ipc.VK_OEM_1, ipc.VK_OEM_7, ";", "'"},
	keys.PairCommaPeriod:    {ipc.VK_OEM_COMMA, ipc.VK_OEM_PERIOD, ",", "."},
	keys.PairLRShift:        {ipc.VK_LSHIFT, ipc.VK_RSHIFT, "", ""},
	keys.PairLRCtrl:         {ipc.VK_LCONTROL, ipc.VK_RCONTROL, "", ""},
	keys.PairMinusEqual:     {ipc.VK_OEM_MINUS, ipc.VK_OEM_PLUS, "-", "="},
	keys.PairBrackets:       {ipc.VK_OEM_4, ipc.VK_OEM_6, "[", "]"},
}

// matchPairVK 在配置 groups 列表内查找：当前按键 (key, keyCode) 是否匹配某个 group 的
// 第 idx 个键（idx=0 第一键，idx=1 第二键），且该 group 必须存在于 allowed 集合。
func matchPairVK(groups []keys.PairGroup, allowed map[keys.PairGroup]struct{}, idx int, key string, keyCode int) bool {
	vk := uint32(keyCode)
	for _, g := range groups {
		if _, ok := allowed[g]; !ok {
			continue
		}
		entry, ok := pairGroupVK[g]
		if !ok {
			continue
		}
		var wantVK uint32
		var wantChar string
		if idx == 0 {
			wantVK, wantChar = entry.firstVK, entry.firstChar
		} else {
			wantVK, wantChar = entry.secondVK, entry.secondChar
		}
		if vk == wantVK {
			return true
		}
		if wantChar != "" && key == wantChar {
			return true
		}
	}
	return false
}

// 各 API 接受的 PairGroup 集合 —— 与 pkg/config 一侧保持一致语义。
var (
	selectKeyAllowedGroups = map[keys.PairGroup]struct{}{
		keys.PairSemicolonQuote: {},
		keys.PairCommaPeriod:    {},
		keys.PairLRShift:        {},
		keys.PairLRCtrl:         {},
	}
	selectCharAllowedGroups = map[keys.PairGroup]struct{}{
		keys.PairCommaPeriod: {},
		keys.PairMinusEqual:  {},
		keys.PairBrackets:    {},
	}
)

// isSelectKey2 checks if the key is configured as the 2nd candidate selection key
func (c *Coordinator) isSelectKey2(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}
	return matchPairVK(c.config.Input.SelectKeyGroups, selectKeyAllowedGroups, 0, key, keyCode)
}

// isSelectKey3 checks if the key is configured as the 3rd candidate selection key
func (c *Coordinator) isSelectKey3(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}
	return matchPairVK(c.config.Input.SelectKeyGroups, selectKeyAllowedGroups, 1, key, keyCode)
}

// isSelectCharFirstKey checks if the key is configured as "select first char from word" key
func (c *Coordinator) isSelectCharFirstKey(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}
	return matchPairVK(c.config.Input.SelectCharKeys, selectCharAllowedGroups, 0, key, keyCode)
}

// isSelectCharSecondKey checks if the key is configured as "select second char from word" key
func (c *Coordinator) isSelectCharSecondKey(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}
	return matchPairVK(c.config.Input.SelectCharKeys, selectCharAllowedGroups, 1, key, keyCode)
}

// isPunctuation checks if a character is a punctuation/symbol that should be
// handled by the punctuation pipeline. This includes all characters that may
// have Chinese punctuation mappings or may be customized by user in the future.
func (c *Coordinator) isPunctuation(r rune) bool {
	switch r {
	// 基础标点（有中文映射）
	case ',', '.', '?', '!', ':', ';', '\'', '"',
		'(', ')', '[', ']', '{', '}', '<', '>',
		'~', '@', '$', '`', '^', '_', '-', '=':
		return true
	// Shift+数字/符号产生的字符（部分有中文映射，其余预留自定义转换）
	case '#', '%', '&', '*', '+', '|', '/', '\\':
		return true
	}
	return false
}

// handlePunctuation handles punctuation input in Chinese mode
// If no input buffer, directly output punctuation (converted if chinese punctuation is enabled)
// If there's input buffer and punct_commit is enabled, commit current candidate and then output punctuation
// afterDigit: 前一个按键是否为直通数字（Go 端状态追踪，作为回退判断）
// prevChar: 光标前一个字符（来自 C++ ITfTextEditSink，0 表示不可用，作为主要判断）
func (c *Coordinator) handlePunctuation(r rune, afterDigit bool, prevChar rune) *bridge.KeyEventResult {
	c.logger.Debug("handlePunctuation", "char", string(r), "buffer", c.inputBuffer)

	// 标点 = 短语终止符，通知造词策略（码表自动造词）
	if c.engineMgr != nil {
		c.engineMgr.OnPhraseTerminated()
	}

	// Check if punct_commit is enabled
	// 码表/Mixed 受 PunctCommit 开关控制；全拼引擎沿用传统行为，标点恒触发顶字上屏
	punctCommitEnabled := false
	if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		if c.engineMgr != nil {
			if eng := c.engineMgr.GetCurrentEngine(); eng != nil {
				switch e := eng.(type) {
				case *codetable.Engine:
					if cfg := e.GetConfig(); cfg != nil {
						punctCommitEnabled = cfg.PunctCommit
					}
				case *mixed.Engine:
					if we := e.GetCodetableEngine(); we != nil {
						if cfg := we.GetConfig(); cfg != nil {
							punctCommitEnabled = cfg.PunctCommit
						}
					}
				case *pinyin.Engine:
					punctCommitEnabled = true
				}
			}
		}
	}

	// If there's input in buffer (or confirmed segments) and candidates, commit first candidate then output punctuation
	if (len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0) && len(c.candidates) > 0 {
		if punctCommitEnabled {
			// Commit highlighted candidate (with confirmed segments), then output punctuation
			highlightedIndex := (c.currentPage-1)*c.candidatesPerPage + c.selectedIndex
			if highlightedIndex >= len(c.candidates) {
				highlightedIndex = 0
			}
			candidate := c.candidates[highlightedIndex]
			text := candidate.Text

			// Apply full-width conversion if enabled
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}

			// Prepend confirmed segments
			var prefix string
			for _, seg := range c.confirmedSegments {
				t := seg.Text
				if c.fullWidth {
					t = transform.ToFullWidth(t)
				}
				prefix += t
			}

			// Convert punctuation
			punctText := c.convertPunct(r, afterDigit, prevChar)

			commitText := prefix + text

			// 记录输入历史（仅候选文本，不含标点），需在 clearState 之前
			if c.inputHistory != nil {
				c.inputHistory.Record(commitText, "", "", 0)
			}

			// 标点顶字上屏需通知造词策略，否则自动造词的 charBuffer 会漏掉此字
			// OnPhraseTerminated 已在上方 flush 旧序列，此处将候选加入新序列
			if c.engineMgr != nil && !candidate.IsCommand {
				c.engineMgr.OnCandidateSelected(c.inputBuffer, candidate.Text, candidate.Source)
			}

			c.clearState()
			c.hideUI()

			// punct_commit 后的标点也支持自动配对
			if tracker := c.getAutoPairTracker(); tracker != nil {
				punctRunes := []rune(punctText)
				if len(punctRunes) == 1 {
					if right, ok := tracker.GetRight(punctRunes[0]); ok {
						pairPunctText := punctText + string(right)
						tracker.Push(punctRunes[0], right)
						c.pairInsertTime = time.Now()
						c.logger.Debug("Auto-pair: insert pair after punct_commit", "text", pairPunctText)
						return &bridge.KeyEventResult{
							Type:         bridge.ResponseTypeInsertTextWithCursor,
							Text:         commitText + pairPunctText,
							CursorOffset: 1,
						}
					}
				}
			}

			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: commitText + punctText,
			}
		}
	}

	// punct_commit 启用但无候选（空码）：丢弃编码，清空缓冲区，直接输出标点
	if punctCommitEnabled && (len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0) && len(c.candidates) == 0 {
		punctText := c.convertPunct(r, afterDigit, prevChar)
		c.clearState()
		c.hideUI()

		// 空码 punct_commit 后的标点也支持自动配对
		if tracker := c.getAutoPairTracker(); tracker != nil {
			punctRunes := []rune(punctText)
			if len(punctRunes) == 1 {
				if right, ok := tracker.GetRight(punctRunes[0]); ok {
					pairPunctText := punctText + string(right)
					tracker.Push(punctRunes[0], right)
					c.pairInsertTime = time.Now()
					return &bridge.KeyEventResult{
						Type:         bridge.ResponseTypeInsertTextWithCursor,
						Text:         pairPunctText,
						CursorOffset: 1,
					}
				}
			}
		}

		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: punctText,
		}
	}

	// If there's input buffer or confirmed segments but punct_commit is not enabled, just let it pass through
	if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		return nil
	}

	// No input buffer - directly handle punctuation
	punctText := c.convertPunct(r, afterDigit, prevChar)

	// 自动配对：检查转换后的标点是否需要配对
	if tracker := c.getAutoPairTracker(); tracker != nil {
		punctRunes := []rune(punctText)
		if len(punctRunes) == 1 {
			// 智能跳过：输入右标点时，如果栈顶匹配则跳过
			if tracker.IsRight(punctRunes[0]) {
				if entry, ok := tracker.Peek(); ok && entry.Right == punctRunes[0] {
					tracker.Pop()
					c.logger.Debug("Auto-pair: smart skip", "char", punctText)
					return &bridge.KeyEventResult{
						Type: bridge.ResponseTypeMoveCursorRight,
					}
				}
				// 栈顶不匹配，清空栈
				tracker.Clear()
			}

			// 自动配对：输入左标点时，插入配对并回退光标
			if right, ok := tracker.GetRight(punctRunes[0]); ok {
				pairText := punctText + string(right)
				tracker.Push(punctRunes[0], right)
				c.pairInsertTime = time.Now()
				c.logger.Debug("Auto-pair: insert pair", "text", pairText)
				return &bridge.KeyEventResult{
					Type:         bridge.ResponseTypeInsertTextWithCursor,
					Text:         pairText,
					CursorOffset: 1,
				}
			}
		}
	}

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: punctText,
	}
}

// shouldSmartPunct 判断是否应对该标点执行数字后智能转换（保持英文标点）。
// 优先使用 TSF 提供的 prevChar（光标前字符），不可用时回退到 Go 端状态追踪。
func (c *Coordinator) shouldSmartPunct(r rune, afterDigit bool, prevChar rune) bool {
	if c.config == nil || !c.config.Input.SmartPunctAfterDigit {
		return false
	}
	if !c.isSmartPunctChar(r) {
		return false
	}
	// 主判断：TSF 提供的光标前字符
	if prevChar != 0 {
		return prevChar >= '0' && prevChar <= '9'
	}
	// 回退：Go 端按键状态追踪
	return afterDigit
}

// isSmartPunctChar 判断该英文标点是否在数字后智能标点列表中
func (c *Coordinator) isSmartPunctChar(r rune) bool {
	list := c.config.Input.SmartPunctList
	if list == "" {
		// 列表为空时回退到默认行为
		return r == '.' || r == ','
	}
	for _, ch := range list {
		if ch == r {
			return true
		}
	}
	return false
}

// getAutoPairTracker 返回当前应使用的配对追踪器，nil 表示不启用配对
func (c *Coordinator) getAutoPairTracker() *transform.PairTracker {
	if c.config == nil {
		return nil
	}
	// 检查应用黑名单
	if len(c.config.Input.AutoPair.Blacklist) > 0 && c.activeProcessName != "" {
		for _, proc := range c.config.Input.AutoPair.Blacklist {
			if strings.EqualFold(proc, c.activeProcessName) {
				return nil
			}
		}
	}
	if !c.chineseMode {
		return nil // 英文模式由 C++ 处理
	}
	if c.isEffectiveChinesePunct() && c.config.Input.AutoPair.Chinese {
		return c.pairTracker
	}
	if !c.isEffectiveChinesePunct() && c.config.Input.AutoPair.English {
		return c.pairTrackerEn
	}
	return nil
}

// applyToggleFullWidth 执行全角切换的核心逻辑（需持锁调用）
func (c *Coordinator) applyToggleFullWidth() {
	c.fullWidth = !c.fullWidth
	c.updateStatusIndicator()
	c.saveRuntimeState()
}

// handleToggleFullWidth handles the full-width toggle hotkey (e.g., Shift+Space)
func (c *Coordinator) handleToggleFullWidth() *bridge.KeyEventResult {
	c.applyToggleFullWidth()
	c.logger.Debug("Full-width toggled via hotkey", "fullWidth", c.fullWidth)
	c.syncToolbarState()
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

// applyTogglePunct 执行标点切换的核心逻辑（需持锁调用）
func (c *Coordinator) applyTogglePunct() {
	c.chinesePunctuation = !c.chinesePunctuation
	c.punctConverter.Reset()
	if c.pairTracker != nil {
		c.pairTracker.Clear()
	}
	c.updateStatusIndicator()
	c.saveRuntimeState()
}

// handleTogglePunct handles the punctuation toggle hotkey (e.g., Ctrl+.)
func (c *Coordinator) handleTogglePunct() *bridge.KeyEventResult {
	// Don't toggle punctuation in English mode
	if !c.chineseMode {
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}

	c.applyTogglePunct()
	c.logger.Debug("Chinese punctuation toggled via hotkey", "chinesePunctuation", c.chinesePunctuation)
	c.syncToolbarState()
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

// isModifierOnlyKey 判断是否为 modifier-only 按键（不产生字符输出）
func isModifierOnlyKey(vk uint32) bool {
	switch vk {
	case ipc.VK_SHIFT, ipc.VK_LSHIFT, ipc.VK_RSHIFT,
		ipc.VK_CONTROL, ipc.VK_LCONTROL, ipc.VK_RCONTROL,
		ipc.VK_MENU, ipc.VK_CAPITAL:
		return true
	}
	return false
}

// getToggleModeKey maps keycode to toggle mode key name
func (c *Coordinator) getToggleModeKey(keyCode int) string {
	switch uint32(keyCode) {
	case ipc.VK_LSHIFT:
		return "lshift"
	case ipc.VK_RSHIFT:
		return "rshift"
	case ipc.VK_SHIFT:
		return "lshift" // 默认作为左Shift处理
	case ipc.VK_LCONTROL:
		return "lctrl"
	case ipc.VK_RCONTROL:
		return "rctrl"
	case ipc.VK_CAPITAL:
		return "capslock"
	}
	return ""
}

// highlightKeyMatch 判断 (keyCode, modifiers) 是否匹配某个 HighlightKeys group 的方向。
// idx=0 上移高亮，idx=1 下移高亮。
//
// arrows 分支语义：上方向键=上移，下方向键=下移，与 Shift 无关。
// tab 分支语义：Shift+Tab=上移，Tab=下移（与翻页 shift_tab 类似但用于高亮场景）。
func highlightKeyMatch(groups []keys.PairGroup, idx int, keyCode uint32, hasShift bool) bool {
	for _, hk := range groups {
		switch hk {
		case keys.PairArrows:
			if idx == 0 && keyCode == ipc.VK_UP {
				return true
			}
			if idx == 1 && keyCode == ipc.VK_DOWN {
				return true
			}
		case keys.PairTab:
			if keyCode != ipc.VK_TAB {
				continue
			}
			if idx == 0 && hasShift {
				return true
			}
			if idx == 1 && !hasShift {
				return true
			}
		}
	}
	return false
}

// isHighlightUpKey checks if the key is configured as a highlight up key
func (c *Coordinator) isHighlightUpKey(keyCode uint32, modifiers uint32) bool {
	if c.config == nil {
		return false
	}
	return highlightKeyMatch(c.config.Input.HighlightKeys, 0, keyCode, modifiers&ModShift != 0)
}

// isHighlightDownKey checks if the key is configured as a highlight down key
func (c *Coordinator) isHighlightDownKey(keyCode uint32, modifiers uint32) bool {
	if c.config == nil {
		return false
	}
	return highlightKeyMatch(c.config.Input.HighlightKeys, 1, keyCode, modifiers&ModShift != 0)
}

// isQuickInputPageUpKey 快捷输入模式专用翻页上键判断。
// 排除 minus_equal（-/=）和 brackets（[/]）这些字符键，因为快捷输入模式下它们是有效输入字符。
func (c *Coordinator) isQuickInputPageUpKey(key string, keyCode int, modifiers uint32) bool {
	vk := uint32(keyCode)
	if vk == ipc.VK_OEM_MINUS || vk == ipc.VK_OEM_PLUS || vk == ipc.VK_OEM_4 || vk == ipc.VK_OEM_6 {
		return false
	}
	return c.isPageUpKey(key, keyCode, modifiers)
}

// isQuickInputPageDownKey 快捷输入模式专用翻页下键判断。
// 排除 minus_equal（-/=）和 brackets（[/]）这些字符键，因为快捷输入模式下它们是有效输入字符。
func (c *Coordinator) isQuickInputPageDownKey(key string, keyCode int, modifiers uint32) bool {
	vk := uint32(keyCode)
	if vk == ipc.VK_OEM_MINUS || vk == ipc.VK_OEM_PLUS || vk == ipc.VK_OEM_4 || vk == ipc.VK_OEM_6 {
		return false
	}
	return c.isPageDownKey(key, keyCode, modifiers)
}

// pageKeyMatch 判断 (key, keyCode, modifiers) 是否匹配某个翻页 group 的指定方向。
// idx=0 表示"上一页"键，idx=1 表示"下一页"键。
//
// 注意翻页键有 Shift 门控的特殊语义：
//   - minus_equal/brackets 在 Shift 按下时不触发翻页（因为 Shift+- = _, Shift+[ = { 等）
//   - shift_tab 中"上一页"恰好是 Shift+Tab（Shift 必须按下），"下一页"是 Tab（Shift 必须未按下）
//   - pageupdown 不依赖 Shift
func pageKeyMatch(groups []keys.PairGroup, idx int, key string, keyCode int, hasShift bool) bool {
	vk := uint32(keyCode)
	for _, pk := range groups {
		switch pk {
		case keys.PairPageUpDown:
			parsedKey, _ := keys.ParseKey(key)
			if idx == 0 && (parsedKey == keys.KeyPageUp || vk == ipc.VK_PRIOR) {
				return true
			}
			if idx == 1 && (parsedKey == keys.KeyPageDown || vk == ipc.VK_NEXT) {
				return true
			}
		case keys.PairMinusEqual:
			if hasShift {
				continue
			}
			if idx == 0 && vk == ipc.VK_OEM_MINUS {
				return true
			}
			if idx == 1 && vk == ipc.VK_OEM_PLUS {
				return true
			}
		case keys.PairBrackets:
			if hasShift {
				continue
			}
			if idx == 0 && vk == ipc.VK_OEM_4 {
				return true
			}
			if idx == 1 && vk == ipc.VK_OEM_6 {
				return true
			}
		case keys.PairShiftTab:
			if vk != ipc.VK_TAB {
				continue
			}
			if idx == 0 && hasShift {
				return true
			}
			if idx == 1 && !hasShift {
				return true
			}
		}
	}
	return false
}

// isPageUpKey checks if the key is configured as a page up key
func (c *Coordinator) isPageUpKey(key string, keyCode int, modifiers uint32) bool {
	if c.config == nil {
		// 默认支持 PageUp 和 - 键（Shift+- 应输出 _ 而非翻页）
		parsedKey, _ := keys.ParseKey(key)
		return parsedKey == keys.KeyPageUp || uint32(keyCode) == ipc.VK_PRIOR || (uint32(keyCode) == ipc.VK_OEM_MINUS && modifiers&ModShift == 0)
	}
	return pageKeyMatch(c.config.Input.PageKeys, 0, key, keyCode, modifiers&ModShift != 0)
}

// isPageDownKey checks if the key is configured as a page down key
func (c *Coordinator) isPageDownKey(key string, keyCode int, modifiers uint32) bool {
	if c.config == nil {
		// 默认支持 PageDown 和 = 键
		parsedKey, _ := keys.ParseKey(key)
		return parsedKey == keys.KeyPageDown || uint32(keyCode) == ipc.VK_NEXT || (uint32(keyCode) == ipc.VK_OEM_PLUS && modifiers&ModShift == 0)
	}
	return pageKeyMatch(c.config.Input.PageKeys, 1, key, keyCode, modifiers&ModShift != 0)
}

// isPinyinSeparator 判断按键是否应作为拼音分隔符处理
// 根据配置 pinyin_separator 决定分隔符按键：
//   - "auto": ' 未被配置为选择键时用 '，否则用 `
//   - "quote": 强制用 '
//   - "backtick": 强制用 `
//   - "none" / "": 禁用分隔符
func (c *Coordinator) isPinyinSeparator(key string, keyCode int) bool {
	if c.engineMgr == nil || c.engineMgr.GetCurrentType() != engine.EngineTypePinyin {
		return false
	}
	if len(c.inputBuffer) == 0 {
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
		// ' 未被配置为选择键时用 '，否则回退到 `
		isQuote := key == "'" || uint32(keyCode) == ipc.VK_OEM_7
		isBacktick := key == "`" || uint32(keyCode) == ipc.VK_OEM_3
		if isQuote {
			// ' 同时是选择键时不作为分隔符
			if c.isSelectKey3(key, keyCode) {
				return false
			}
			return true
		}
		if isBacktick {
			// 只有当 ' 被选择键占用时，` 才作为分隔符
			quoteIsSelectKey := c.isSelectKey3("'", int(ipc.VK_OEM_7))
			return quoteIsSelectKey
		}
		return false
	default:
		return false
	}
}

// handlePinyinSeparator 将分隔符插入输入缓冲区并刷新候选
// 无论物理按键是 ' 还是 `，都统一插入 ' 作为拼音分隔符（引擎层只认 '）
func (c *Coordinator) handlePinyinSeparator() *bridge.KeyEventResult {
	// 防止连续分隔符：如果光标前已经是 '，则忽略本次输入
	if c.inputCursorPos > 0 && c.inputBuffer[c.inputCursorPos-1] == '\'' {
		c.logger.Debug("Ignoring consecutive pinyin separator")
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
	}
	c.inputBuffer = c.inputBuffer[:c.inputCursorPos] + "'" + c.inputBuffer[c.inputCursorPos:]
	c.inputCursorPos++
	c.logger.Debug("Pinyin separator inserted", "buffer", c.inputBuffer, "cursor", c.inputCursorPos)

	c.updateCandidates()
	c.showUI()

	// 与 handleAlphaKey 保持一致：通过统一入口构建 UpdateComposition 响应
	return c.compositionUpdateResult()
}

// matchHotkey checks if the current key event matches the configured hotkey string
// Supported formats: "ctrl+`", "shift+space", "ctrl+.", "ctrl+shift+e", "none", ""
func (c *Coordinator) matchHotkey(hotkeyStr string, hasCtrl, hasShift, hasAlt bool, keyCode int) bool {
	if hotkeyStr == "" || hotkeyStr == "none" {
		return false
	}

	// Parse the hotkey string
	needCtrl := false
	needShift := false
	needAlt := false
	var targetKeyCode int

	// Parse modifiers and key
	switch hotkeyStr {
	case "ctrl+`":
		needCtrl = true
		targetKeyCode = int(ipc.VK_OEM_3)
	case "ctrl+shift+e":
		needCtrl = true
		needShift = true
		targetKeyCode = 69 // VK_E (0x45)
	case "shift+space":
		needShift = true
		targetKeyCode = int(ipc.VK_SPACE)
	case "ctrl+shift+space":
		needCtrl = true
		needShift = true
		targetKeyCode = int(ipc.VK_SPACE)
	case "ctrl+.":
		needCtrl = true
		targetKeyCode = int(ipc.VK_OEM_PERIOD)
	case "ctrl+,":
		needCtrl = true
		targetKeyCode = int(ipc.VK_OEM_COMMA)
	default:
		// Generic parser: split by "+" and resolve modifiers + key
		parts := strings.Split(strings.ToLower(hotkeyStr), "+")
		for i, part := range parts {
			switch keys.Modifier(part) {
			case keys.ModCtrl:
				needCtrl = true
			case keys.ModShift:
				needShift = true
			case keys.ModAlt:
				needAlt = true
			default:
				// Last non-modifier part is the key name
				// Only treat the last part as the key (or any unrecognized part)
				if i == len(parts)-1 {
					targetKeyCode = resolveVKFromKeyName(part)
				}
			}
		}
		if targetKeyCode == 0 {
			c.logger.Debug("Unknown hotkey format", "hotkey", hotkeyStr)
			return false
		}
	}

	// Check if all modifiers match
	if needCtrl != hasCtrl || needShift != hasShift || needAlt != hasAlt {
		return false
	}

	// Check if the key matches
	return keyCode == targetKeyCode
}

// resolveVKFromKeyName converts a lowercase key name string to a Windows virtual key code.
// Returns 0 if the name is not recognized.
// vkByKeyHP 把规范化 keys.Key 映射到 Windows 虚拟键码（handle_punctuation 子集）。
var vkByKeyHP = map[keys.Key]int{
	keys.KeyGrave:     int(ipc.VK_OEM_3),
	keys.KeySpace:     int(ipc.VK_SPACE),
	keys.KeyPeriod:    int(ipc.VK_OEM_PERIOD),
	keys.KeyComma:     int(ipc.VK_OEM_COMMA),
	keys.KeySemicolon: int(ipc.VK_OEM_1),
	keys.KeyQuote:     int(ipc.VK_OEM_7),
	keys.KeySlash:     int(ipc.VK_OEM_2),
	keys.KeyBackslash: int(ipc.VK_OEM_5),
	keys.KeyLBracket:  int(ipc.VK_OEM_4),
	keys.KeyRBracket:  int(ipc.VK_OEM_6),
	keys.KeyMinus:     int(ipc.VK_OEM_MINUS),
	keys.KeyEqual:     int(ipc.VK_OEM_PLUS),
	keys.KeyTab:       0x09,
	keys.KeyEscape:    0x1B,
}

func init() {
	// 字母 a-z -> 0x41-0x5A
	for c := byte('a'); c <= 'z'; c++ {
		vkByKeyHP[keys.Key(string(c))] = int(c-'a') + 0x41
	}
	// 数字 0-9 -> 0x30-0x39
	for c := byte('0'); c <= '9'; c++ {
		vkByKeyHP[keys.Key(string(c))] = int(c-'0') + 0x30
	}
	// F1-F12 -> 0x70-0x7B
	fNames := []keys.Key{
		keys.KeyF1, keys.KeyF2, keys.KeyF3, keys.KeyF4, keys.KeyF5, keys.KeyF6,
		keys.KeyF7, keys.KeyF8, keys.KeyF9, keys.KeyF10, keys.KeyF11, keys.KeyF12,
	}
	for i, k := range fNames {
		vkByKeyHP[k] = 0x70 + i
	}
}

// resolveVKFromKeyName 把任意按键名（含别名/大小写）解析为 Windows 虚拟键码。
// 入口先经 keys.ParseKey 规范化，再查 vkByKeyHP 表；未识别返回 0。
func resolveVKFromKeyName(name string) int {
	k, ok := keys.ParseKey(name)
	if !ok {
		return 0
	}
	return vkByKeyHP[k]
}

// updatePairedQuotes 根据中文配对表更新 PunctuationConverter 的引号配对状态
// 当引号在配对表中时，跳过交替逻辑，始终输出左引号由配对追踪器补全右引号
func (c *Coordinator) updatePairedQuotes(chinesePairs []string) {
	var singlePaired, doublePaired bool
	for _, s := range chinesePairs {
		runes := []rune(s)
		if len(runes) != 2 {
			continue
		}
		if runes[0] == '\u2018' && runes[1] == '\u2019' {
			singlePaired = true
		}
		if runes[0] == '\u201C' && runes[1] == '\u201D' {
			doublePaired = true
		}
	}
	c.punctConverter.SetPairedQuotes(singlePaired, doublePaired)
}

// convertPunct 统一标点转换逻辑：自定义映射 > 中文标点转换 > 全角转换
// 返回最终输出的标点文本
func (c *Coordinator) convertPunct(r rune, afterDigit bool, prevChar rune) string {
	effectiveChPunct := c.isEffectiveChinesePunct()
	smartPunct := effectiveChPunct && c.shouldSmartPunct(r, afterDigit, prevChar)
	isChinesePunct := effectiveChPunct && !smartPunct

	// 自定义标点映射优先
	if c.config != nil && c.config.Input.PunctCustom.Enabled {
		colIdx := -1
		if isChinesePunct && c.fullWidth {
			colIdx = 2 // 中文全角
		} else if isChinesePunct {
			colIdx = 0 // 中文半角
		} else if c.fullWidth {
			colIdx = 1 // 英文全角
		}
		if colIdx >= 0 {
			if text, ok := c.punctConverter.LookupCustom(r, colIdx); ok {
				return text
			}
		}
	}

	// 默认转换逻辑
	punctText := string(r)
	if isChinesePunct {
		if converted, ok := c.punctConverter.ToChinesePunctStr(r); ok {
			punctText = converted
		}
	}
	if c.fullWidth {
		punctText = transform.ToFullWidth(punctText)
	}
	return punctText
}
