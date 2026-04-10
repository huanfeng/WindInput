// handle_punctuation.go — 标点处理、快捷键匹配、选择键、翻页键
package coordinator

import (
	"strings"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/transform"
)

// isSelectKey2 checks if the key is configured as the 2nd candidate selection key
func (c *Coordinator) isSelectKey2(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}

	// 根据选择键组配置检查
	for _, group := range c.config.Input.SelectKeyGroups {
		switch group {
		case "semicolon_quote":
			if key == ";" || uint32(keyCode) == ipc.VK_OEM_1 {
				return true
			}
		case "comma_period":
			if key == "," || uint32(keyCode) == ipc.VK_OEM_COMMA {
				return true
			}
		case "lrshift":
			if uint32(keyCode) == ipc.VK_LSHIFT {
				return true
			}
		case "lrctrl":
			if uint32(keyCode) == ipc.VK_LCONTROL {
				return true
			}
		}
	}
	return false
}

// isSelectKey3 checks if the key is configured as the 3rd candidate selection key
func (c *Coordinator) isSelectKey3(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}

	// 根据选择键组配置检查
	for _, group := range c.config.Input.SelectKeyGroups {
		switch group {
		case "semicolon_quote":
			if key == "'" || uint32(keyCode) == ipc.VK_OEM_7 {
				return true
			}
		case "comma_period":
			if key == "." || uint32(keyCode) == ipc.VK_OEM_PERIOD {
				return true
			}
		case "lrshift":
			if uint32(keyCode) == ipc.VK_RSHIFT {
				return true
			}
		case "lrctrl":
			if uint32(keyCode) == ipc.VK_RCONTROL {
				return true
			}
		}
	}
	return false
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

	// If there's input in buffer (or confirmed segments), check if we should commit first (punct_commit)
	if (len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0) && len(c.candidates) > 0 {
		// Check if punct_commit is enabled in wubi config
		punctCommit := false
		if c.engineMgr != nil {
			if eng := c.engineMgr.GetCurrentEngine(); eng != nil {
				switch e := eng.(type) {
				case *codetable.Engine:
					if cfg := e.GetConfig(); cfg != nil {
						punctCommit = cfg.PunctCommit
					}
				case *mixed.Engine:
					if we := e.GetCodetableEngine(); we != nil {
						if cfg := we.GetConfig(); cfg != nil {
							punctCommit = cfg.PunctCommit
						}
					}
				}
			}
		}

		if punctCommit {
			// Commit first candidate (with confirmed segments), then output punctuation
			candidate := c.candidates[0]
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
			punctText := string(r)
			if c.chinesePunctuation {
				// 数字后智能标点：句号→点号、逗号→英文逗号
				if c.shouldSmartPunct(r, afterDigit, prevChar) {
					punctText = string(r)
				} else {
					var converted bool
					punctText, converted = c.punctConverter.ToChinesePunctStr(r)
					if !converted {
						punctText = string(r)
					}
				}
			}

			// Apply full-width conversion to punctuation if enabled
			if c.fullWidth {
				punctText = transform.ToFullWidth(punctText)
			}

			c.clearState()
			c.hideUI()

			commitText := prefix + text

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

	// If there's input buffer or confirmed segments but punct_commit is not enabled, just let it pass through
	if len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 {
		return nil
	}

	// No input buffer - directly handle punctuation
	punctText := string(r)
	if c.chinesePunctuation {
		// 数字后智能标点：句号→点号、逗号→英文逗号
		if c.shouldSmartPunct(r, afterDigit, prevChar) {
			punctText = string(r)
		} else {
			var converted bool
			punctText, converted = c.punctConverter.ToChinesePunctStr(r)
			if !converted {
				punctText = string(r)
			}
		}
	}

	// Apply full-width conversion if enabled
	if c.fullWidth {
		punctText = transform.ToFullWidth(punctText)
	}

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
	if c.chinesePunctuation && c.config.Input.AutoPair.Chinese {
		return c.pairTracker
	}
	if !c.chinesePunctuation && c.config.Input.AutoPair.English {
		return c.pairTrackerEn
	}
	return nil
}

// applyToggleFullWidth 执行全角切换的核心逻辑（需持锁调用）
func (c *Coordinator) applyToggleFullWidth() {
	c.fullWidth = !c.fullWidth
	indicator := "半"
	if c.fullWidth {
		indicator = "全"
	}
	c.showIndicator(indicator)
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
	indicator := "英."
	if c.chinesePunctuation {
		indicator = "中。"
	}
	c.showIndicator(indicator)
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

// isHighlightUpKey checks if the key is configured as a highlight up key
func (c *Coordinator) isHighlightUpKey(keyCode uint32, modifiers uint32) bool {
	if c.config == nil {
		return false
	}
	for _, hk := range c.config.Input.HighlightKeys {
		switch hk {
		case "arrows":
			if keyCode == ipc.VK_UP {
				return true
			}
		case "tab":
			if keyCode == ipc.VK_TAB && modifiers&ModShift != 0 {
				return true
			}
		}
	}
	return false
}

// isHighlightDownKey checks if the key is configured as a highlight down key
func (c *Coordinator) isHighlightDownKey(keyCode uint32, modifiers uint32) bool {
	if c.config == nil {
		return false
	}
	for _, hk := range c.config.Input.HighlightKeys {
		switch hk {
		case "arrows":
			if keyCode == ipc.VK_DOWN {
				return true
			}
		case "tab":
			if keyCode == ipc.VK_TAB && modifiers&ModShift == 0 {
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
		return key == "page_up" || uint32(keyCode) == ipc.VK_PRIOR || (uint32(keyCode) == ipc.VK_OEM_MINUS && modifiers&ModShift == 0)
	}

	hasShift := modifiers&ModShift != 0

	for _, pk := range c.config.Input.PageKeys {
		switch pk {
		case "pageupdown":
			if key == "page_up" || uint32(keyCode) == ipc.VK_PRIOR {
				return true
			}
		case "minus_equal":
			// Shift+- 应输出 _ 而非翻页
			if !hasShift && uint32(keyCode) == ipc.VK_OEM_MINUS {
				return true
			}
		case "brackets":
			// Shift+[ 应输出 { 而非翻页
			if !hasShift && uint32(keyCode) == ipc.VK_OEM_4 {
				return true
			}
		case "shift_tab":
			// Shift+Tab = page up
			if uint32(keyCode) == ipc.VK_TAB && hasShift {
				return true
			}
		}
	}
	return false
}

// isPageDownKey checks if the key is configured as a page down key
func (c *Coordinator) isPageDownKey(key string, keyCode int, modifiers uint32) bool {
	if c.config == nil {
		// 默认支持 PageDown 和 = 键
		return key == "page_down" || uint32(keyCode) == ipc.VK_NEXT || (uint32(keyCode) == ipc.VK_OEM_PLUS && modifiers&ModShift == 0)
	}

	hasShift := modifiers&ModShift != 0

	for _, pk := range c.config.Input.PageKeys {
		switch pk {
		case "pageupdown":
			if key == "page_down" || uint32(keyCode) == ipc.VK_NEXT {
				return true
			}
		case "minus_equal":
			// Shift+= 应输出 + 而非翻页
			if !hasShift && uint32(keyCode) == ipc.VK_OEM_PLUS {
				return true
			}
		case "brackets":
			// Shift+] 应输出 } 而非翻页
			if !hasShift && uint32(keyCode) == ipc.VK_OEM_6 {
				return true
			}
		case "shift_tab":
			// Tab without Shift = page down
			if uint32(keyCode) == ipc.VK_TAB && !hasShift {
				return true
			}
		}
	}
	return false
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

	// Handle Inline Preedit (与 handleAlphaKey 保持一致)
	if c.config != nil && c.config.UI.InlinePreedit {
		return &bridge.KeyEventResult{
			Type:     bridge.ResponseTypeUpdateComposition,
			Text:     c.compositionText(),
			CaretPos: c.displayCursorPos(),
		}
	}

	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     "",
		CaretPos: 0,
	}
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
			switch part {
			case "ctrl":
				needCtrl = true
			case "shift":
				needShift = true
			case "alt":
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
func resolveVKFromKeyName(name string) int {
	// Single letter a-z → 0x41-0x5A
	if len(name) == 1 {
		ch := name[0]
		if ch >= 'a' && ch <= 'z' {
			return int(ch-'a') + 0x41
		}
		// Digit 0-9 → 0x30-0x39
		if ch >= '0' && ch <= '9' {
			return int(ch-'0') + 0x30
		}
	}

	// F1-F12 → 0x70-0x7B
	if len(name) >= 2 && name[0] == 'f' {
		rest := name[1:]
		num := 0
		valid := true
		for _, c := range rest {
			if c < '0' || c > '9' {
				valid = false
				break
			}
			num = num*10 + int(c-'0')
		}
		if valid && num >= 1 && num <= 12 {
			return 0x70 + num - 1
		}
	}

	// Special keys
	switch name {
	case "`":
		return int(ipc.VK_OEM_3)
	case "space":
		return int(ipc.VK_SPACE)
	case ".":
		return int(ipc.VK_OEM_PERIOD)
	case ",":
		return int(ipc.VK_OEM_COMMA)
	case ";":
		return int(ipc.VK_OEM_1)
	case "'":
		return int(ipc.VK_OEM_7)
	case "/":
		return int(ipc.VK_OEM_2)
	case "\\":
		return int(ipc.VK_OEM_5)
	case "[":
		return int(ipc.VK_OEM_4)
	case "]":
		return int(ipc.VK_OEM_6)
	case "-":
		return int(ipc.VK_OEM_MINUS)
	case "=", "equal":
		return int(ipc.VK_OEM_PLUS)
	case "tab":
		return 0x09
	case "escape", "esc":
		return 0x1B
	}
	return 0
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
