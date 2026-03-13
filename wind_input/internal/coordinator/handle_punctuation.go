// handle_punctuation.go — 标点处理、快捷键匹配、选择键、翻页键
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/engine"
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
		'~', '@', '$', '`', '^', '_':
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
func (c *Coordinator) handlePunctuation(r rune) *bridge.KeyEventResult {
	c.logger.Debug("handlePunctuation", "char", string(r), "buffer", c.inputBuffer)

	// If there's input in buffer, check if we should commit first (punct_commit)
	if len(c.inputBuffer) > 0 && len(c.candidates) > 0 {
		// Check if punct_commit is enabled in wubi config
		punctCommit := false
		if c.config != nil && c.config.Engine.Type == "wubi" {
			punctCommit = c.config.Engine.Wubi.PunctCommit
		}

		if punctCommit {
			// Commit first candidate, then output punctuation
			candidate := c.candidates[0]
			text := candidate.Text

			// Apply full-width conversion if enabled
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}

			// Convert punctuation
			punctText := string(r)
			if c.chinesePunctuation {
				var converted bool
				punctText, converted = c.punctConverter.ToChinesePunctStr(r)
				if !converted {
					punctText = string(r)
				}
			}

			// Apply full-width conversion to punctuation if enabled
			if c.fullWidth {
				punctText = transform.ToFullWidth(punctText)
			}

			c.clearState()
			c.hideUI()

			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text + punctText,
			}
		}
	}

	// If there's input buffer but punct_commit is not enabled, just let it pass through
	if len(c.inputBuffer) > 0 {
		return nil
	}

	// No input buffer - directly handle punctuation
	punctText := string(r)
	if c.chinesePunctuation {
		var converted bool
		punctText, converted = c.punctConverter.ToChinesePunctStr(r)
		if !converted {
			punctText = string(r)
		}
	}

	// Apply full-width conversion if enabled
	if c.fullWidth {
		punctText = transform.ToFullWidth(punctText)
	}

	return &bridge.KeyEventResult{
		Type: bridge.ResponseTypeInsertText,
		Text: punctText,
	}
}

// handleToggleFullWidth handles the full-width toggle hotkey (e.g., Shift+Space)
func (c *Coordinator) handleToggleFullWidth() *bridge.KeyEventResult {
	c.fullWidth = !c.fullWidth
	c.logger.Debug("Full-width toggled via hotkey", "fullWidth", c.fullWidth)

	// Show indicator
	indicator := "半"
	if c.fullWidth {
		indicator = "全"
	}
	c.showIndicator(indicator)

	// Update toolbar state
	c.syncToolbarState()

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

	// Consume the key (don't let it pass through)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
}

// handleTogglePunct handles the punctuation toggle hotkey (e.g., Ctrl+.)
func (c *Coordinator) handleTogglePunct() *bridge.KeyEventResult {
	c.chinesePunctuation = !c.chinesePunctuation
	c.logger.Debug("Chinese punctuation toggled via hotkey", "chinesePunctuation", c.chinesePunctuation)

	// Reset punctuation converter state
	c.punctConverter.Reset()

	// Show indicator
	indicator := "英."
	if c.chinesePunctuation {
		indicator = "中，"
	}
	c.showIndicator(indicator)

	// Update toolbar state
	c.syncToolbarState()

	// Save runtime state if remember_last_state is enabled
	c.saveRuntimeState()

	// Consume the key (don't let it pass through)
	return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
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
// 条件：1) 当前是拼音模式 2) 有未提交的输入 3) 按键是 '
func (c *Coordinator) isPinyinSeparator(key string, keyCode int) bool {
	if c.engineMgr == nil || c.engineMgr.GetCurrentType() != engine.EngineTypePinyin {
		return false
	}
	if len(c.inputBuffer) == 0 {
		return false
	}
	if key == "'" || uint32(keyCode) == ipc.VK_OEM_7 {
		return true
	}
	return false
}

// handlePinyinSeparator 将分隔符插入输入缓冲区并刷新候选
func (c *Coordinator) handlePinyinSeparator() *bridge.KeyEventResult {
	c.inputBuffer = c.inputBuffer[:c.inputCursorPos] + "'" + c.inputBuffer[c.inputCursorPos:]
	c.inputCursorPos += 1
	c.logger.Debug("Pinyin separator inserted", "buffer", c.inputBuffer, "cursor", c.inputCursorPos)

	c.updateCandidates()
	c.showUI()

	return &bridge.KeyEventResult{
		Type:           bridge.ResponseTypeUpdateComposition,
		NewComposition: c.inputBuffer,
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
		// Unknown hotkey format
		c.logger.Debug("Unknown hotkey format", "hotkey", hotkeyStr)
		return false
	}

	// Check if all modifiers match
	if needCtrl != hasCtrl || needShift != hasShift || needAlt != hasAlt {
		return false
	}

	// Check if the key matches
	return keyCode == targetKeyCode
}
