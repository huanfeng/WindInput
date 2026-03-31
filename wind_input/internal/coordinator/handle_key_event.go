// handle_key_event.go — 键事件主路由（HandleKeyEvent 函数）
package coordinator

import (
	"strings"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/transform"
)

// shiftedKeyMap maps unshifted key → shifted character (US keyboard layout)
// Used to resolve the actual typed character when Shift is held.
// This table can be extended or replaced by user config for custom keyboard layouts.
var shiftedKeyMap = map[byte]byte{
	'1': '!', '2': '@', '3': '#', '4': '$', '5': '%',
	'6': '^', '7': '&', '8': '*', '9': '(', '0': ')',
	'-': '_', '=': '+',
	'[': '{', ']': '}', '\\': '|',
	';': ':', '\'': '"',
	',': '<', '.': '>', '/': '?',
	'`': '~',
}

// numpadKeyToChar returns the character for a numpad key code, or "" if not a numpad key.
// Numpad keys always output their character directly, bypassing IME processing.
func numpadKeyToChar(keyCode int) string {
	k := uint32(keyCode)
	if k >= ipc.VK_NUMPAD0 && k <= ipc.VK_NUMPAD9 {
		return string(rune('0' + k - ipc.VK_NUMPAD0))
	}
	switch k {
	case ipc.VK_MULTIPLY:
		return "*"
	case ipc.VK_ADD:
		return "+"
	case ipc.VK_SUBTRACT:
		return "-"
	case ipc.VK_DECIMAL:
		return "."
	case ipc.VK_DIVIDE:
		return "/"
	}
	return ""
}

// HandleKeyEvent handles key events from C++ Bridge
// Returns a result indicating what action to take
func (c *Coordinator) HandleKeyEvent(data bridge.KeyEventData) *bridge.KeyEventResult {
	startTime := time.Now()

	c.mu.Lock()
	lockTime := time.Since(startTime)
	defer c.mu.Unlock()

	// Use Debug for high-frequency key events to reduce log noise
	c.logger.Debug("HandleKeyEvent", "key", data.Key, "keycode", data.KeyCode, "modifiers", data.Modifiers, "chineseMode", c.chineseMode, "lockWait", lockTime.String())

	// Check for Ctrl or Alt modifiers
	hasCtrl := data.Modifiers&ModCtrl != 0
	hasAlt := data.Modifiers&ModAlt != 0
	hasShift := data.Modifiers&ModShift != 0

	// Handle switch engine hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.SwitchEngine, hasCtrl, hasShift, hasAlt, data.KeyCode) {
		return c.handleEngineSwitchKey()
	}

	// Handle full-width toggle hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.ToggleFullWidth, hasCtrl, hasShift, hasAlt, data.KeyCode) {
		return c.handleToggleFullWidth()
	}

	// Handle punctuation toggle hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.TogglePunct, hasCtrl, hasShift, hasAlt, data.KeyCode) {
		return c.handleTogglePunct()
	}

	// 候选词操作快捷键（仅在输入态且有候选时生效）
	if c.config != nil && hasCtrl && len(c.candidates) > 0 && len(c.inputBuffer) > 0 {
		if num := c.matchCandidateActionKey(c.config.Hotkeys.DeleteCandidate, hasCtrl, hasShift, data.KeyCode); num > 0 {
			return c.handleDeleteCandidateByKey(num)
		}
		if num := c.matchCandidateActionKey(c.config.Hotkeys.PinCandidate, hasCtrl, hasShift, data.KeyCode); num > 0 {
			return c.handlePinCandidateByKey(num)
		}
	}

	// 加词模式按键拦截（优先于其他处理）
	if c.addWordActive {
		return c.handleAddWordKey(data)
	}

	// 加词快捷键
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.AddWord, hasCtrl, hasShift, hasAlt, data.KeyCode) {
		return c.enterAddWordMode()
	}

	// Handle mode toggle keys (lshift, rshift, lctrl, rctrl, capslock)
	// IMPORTANT: This must be checked BEFORE the Ctrl/Alt pass-through check,
	// because lctrl/rctrl are toggle mode keys but also set hasCtrl=true
	if toggleKey := c.getToggleModeKey(data.KeyCode); toggleKey != "" {
		c.logger.Debug("Toggle mode key detected", "key", toggleKey, "keyCode", data.KeyCode,
			"isConfigured", c.config != nil && c.config.IsToggleModeKey(toggleKey),
			"configuredKeys", c.config.Hotkeys.ToggleModeKeys)
		if c.config != nil && c.config.IsToggleModeKey(toggleKey) {
			// 检查是否需要在切换前上屏已有内容
			// CommitOnSwitch: 上屏编码（而非候选词），因为用户切换到英文意味着想输入英文
			var commitText string
			if c.config.Hotkeys.CommitOnSwitch && c.chineseMode {
				commitText = c.getPendingBufferText()
			}

			c.chineseMode = !c.chineseMode
			c.logger.Debug("Mode toggled", "key", toggleKey, "chineseMode", c.chineseMode)

			// Clear any pending input when switching modes
			if c.hasPendingInput() {
				c.clearState()
				c.hideUI()
			}

			// Sync punctuation with mode if enabled
			if c.punctFollowMode {
				c.chinesePunctuation = c.chineseMode
			}

			// Reset punctuation converter state when switching modes
			c.punctConverter.Reset()

			// Save runtime state if remember_last_state is enabled
			c.saveRuntimeState()

			// Show mode indicator
			c.showModeIndicator()

			// Broadcast state to toolbar and all TSF clients
			c.broadcastState()

			// Return mode_changed with optional commit text
			if commitText != "" {
				return &bridge.KeyEventResult{
					Type:        bridge.ResponseTypeInsertText,
					Text:        commitText,
					ModeChanged: true,
					ChineseMode: c.chineseMode,
				}
			}

			return &bridge.KeyEventResult{
				Type:        bridge.ResponseTypeModeChanged,
				ChineseMode: c.chineseMode,
			}
		} else if toggleKey == "capslock" {
			// CapsLock is not configured as mode toggle key, but we still need to show indicator
			// C++ side sets 0x8000 bit in modifiers to indicate "state notification only"
			// Use the CapsLock state from C++ side (data.Toggles) as it's more accurate
			capsLockOn := data.IsCapsLockOn()
			c.logger.Debug("CapsLock state notification", "on", capsLockOn)

			// CapsLock 切换时，清理所有待处理的输入缓冲
			// 避免残留状态导致后续数字、标点等按键行为异常
			var capsCommitText string
			hasPending := c.hasPendingInput()
			if hasPending {
				if c.config != nil && c.config.Hotkeys.CommitOnSwitch {
					capsCommitText = c.getPendingBufferText()
				}
				c.clearState()
				c.hideUI()
			}

			// Update CapsLock state and broadcast if changed
			if c.capsLockOn != capsLockOn {
				c.capsLockOn = capsLockOn
				c.broadcastState()
			}

			// Show CapsLock indicator (A/a) - use NoLock version since we already hold the lock
			c.handleCapsLockStateNoLock(capsLockOn)

			if capsCommitText != "" {
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: capsCommitText,
				}
			}
			if hasPending {
				return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
			}
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		} else {
			// Toggle key recognized (lshift/rshift/lctrl/rctrl) but not configured
			// Consume the key to avoid passing Shift/Ctrl through to the application
			// This ensures consistent behavior: modifier key releases are always eaten by IME
			c.logger.Debug("Toggle key not configured, consuming", "key", toggleKey)
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
	}

	// Other Ctrl/Alt combinations should be passed to the system
	// (after checking toggle mode keys, since lctrl/rctrl are valid toggle keys)
	if hasCtrl || hasAlt {
		if c.hasPendingInput() {
			// 输入态下 Ctrl/Alt 组合键（非已注册热键）：取消输入，让 C++ 端透传按键给宿主程序
			// 例如 Ctrl+S 保存、Ctrl+C 复制等，用户意图是执行快捷键而非继续打字
			c.logger.Debug("Ctrl/Alt combo during composing, clearing state for pass-through",
				"ctrl", hasCtrl, "alt", hasAlt, "keyCode", data.KeyCode)
			c.clearState()
			c.hideUI()
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
		}
		c.logger.Debug("Key has Ctrl/Alt modifier, passing to system")
		return nil
	}

	// Preserve original key for English mode (uppercase letters should stay uppercase)
	key := data.Key

	// English mode: pass through all keys directly to system
	if !c.chineseMode {
		return nil
	}

	// 小键盘按键：始终直接输出字符，不参与候选选择或标点转换
	if numpadChar := numpadKeyToChar(data.KeyCode); numpadChar != "" {
		if c.hasPendingInput() {
			c.clearState()
			c.hideUI()
		}
		text := numpadChar
		if c.fullWidth {
			text = transform.ToFullWidth(text)
		}
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	// Shift+符号/数字键解析：将物理键映射为实际输入字符
	// 例如 Shift+1 → "!", Shift+, → "<", Shift+; → ":"
	// 字母键不在此映射中，由后续 Shift+字母逻辑单独处理
	if hasShift && len(key) == 1 {
		if shifted, ok := shiftedKeyMap[key[0]]; ok {
			key = string(shifted)
		}
	}

	// Chinese mode with CapsLock: output letters directly (no full-width)
	// CapsLock ON: letters are uppercase, Shift+letter are lowercase
	// This allows users to quickly type English while in Chinese mode
	// Use the CapsLock state from C++ side (data.Toggles) as it's more accurate
	if data.IsCapsLockOn() {
		if len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')) {
			// If there's pending input, commit it first then output the letter
			if len(c.inputBuffer) > 0 && len(c.candidates) > 0 {
				// Commit first candidate
				candidate := c.candidates[0]
				text := candidate.Text
				if c.fullWidth {
					text = transform.ToFullWidth(text)
				}
				c.clearState()
				c.hideUI()

				// Shift+letter = lowercase, letter = uppercase (CapsLock behavior)
				// Note: no full-width conversion for CapsLock English output
				var outputKey string
				if hasShift {
					outputKey = strings.ToLower(key)
				} else {
					outputKey = strings.ToUpper(key)
				}

				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: text + outputKey,
				}
			}

			// No pending input, just output letter
			c.clearState()
			c.hideUI()

			// Shift+letter = lowercase, letter = uppercase (CapsLock behavior)
			// Note: no full-width conversion for CapsLock English output
			var outputKey string
			if hasShift {
				outputKey = strings.ToLower(key)
			} else {
				outputKey = strings.ToUpper(key)
			}

			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: outputKey,
			}
		}
	}

	// 检查是否处于临时英文模式
	if c.tempEnglishMode {
		return c.handleTempEnglishKey(key, &data)
	}

	// 检查是否处于临时拼音模式
	if c.tempPinyinMode {
		return c.handleTempPinyinKey(key, &data)
	}

	// 检查是否应触发临时拼音模式（Shift 时不触发，Shift+` 应输出 ~）
	if !hasShift && c.isTempPinyinTrigger(key, data.KeyCode) {
		return c.enterTempPinyinMode()
	}

	// 中文模式下，Shift+字母处理（CapsLock OFF 时）
	if c.chineseMode && !data.IsCapsLockOn() && hasShift {
		if len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')) {
			if len(c.inputBuffer) > 0 {
				// 已有输入缓冲时，将大写字母直接追加到输入缓冲
				return c.handleAlphaKey(strings.ToUpper(key))
			}
			// 无输入缓冲时，进入临时英文模式
			if c.config != nil && c.config.Input.ShiftTempEnglish.Enabled {
				return c.enterTempEnglishMode(key)
			}
		}
	}

	// Chinese mode handling
	vk := uint32(data.KeyCode)
	switch {
	case c.isHighlightUpKey(vk, uint32(data.Modifiers)):
		return c.handleArrowUp()

	case c.isHighlightDownKey(vk, uint32(data.Modifiers)):
		return c.handleArrowDown()

	case vk == ipc.VK_LEFT:
		return c.handleCursorLeft()

	case vk == ipc.VK_RIGHT:
		return c.handleCursorRight()

	case vk == ipc.VK_HOME:
		return c.handleCursorHome()

	case vk == ipc.VK_END:
		return c.handleCursorEnd()

	case vk == ipc.VK_BACK:
		return c.handleBackspace()

	case vk == ipc.VK_DELETE:
		return c.handleDelete()

	case vk == ipc.VK_RETURN:
		return c.handleEnter()

	case vk == ipc.VK_ESCAPE:
		return c.handleEscape()

	case vk == ipc.VK_SPACE:
		return c.handleSpace()

	case c.isPageUpKey(key, data.KeyCode, uint32(data.Modifiers)):
		if result := c.handlePageUp(); result != nil {
			return result
		}
		// No candidates — fall through to punctuation if applicable
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]))
		}
		return nil

	case c.isPageDownKey(key, data.KeyCode, uint32(data.Modifiers)):
		if result := c.handlePageDown(); result != nil {
			return result
		}
		// No candidates — fall through to punctuation if applicable
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]))
		}
		return nil

	case vk == ipc.VK_TAB:
		// Tab 安全网：输入态下始终消费，防止透传给宿主程序导致焦点跳转
		// 如果 Tab 已被 isHighlightDownKey/UpKey 匹配则不会到达此处
		if c.hasPendingInput() {
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		return nil

	case len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')):
		// Chinese mode: convert to lowercase for pinyin
		return c.handleAlphaKey(strings.ToLower(key))

	case len(key) == 1 && key[0] >= '1' && key[0] <= '9':
		return c.handleNumberKey(int(key[0] - '0'))

	case len(key) == 1 && key[0] == '0':
		return c.handleNumberKey(10)

	case !hasShift && c.isSelectKey2(key, data.KeyCode):
		// Handle 2nd candidate selection key (e.g., semicolon)
		// Shift 时不触发选择（Shift+; 应输出 : 而非选候选）
		if len(c.candidates) >= 2 && len(c.inputBuffer) > 0 {
			return c.selectCandidate(1) // Select 2nd candidate (index 1)
		}
		// If no candidates, treat as punctuation
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]))
		}
		return nil

	case !hasShift && c.isPinyinSeparator(key, data.KeyCode):
		return c.handlePinyinSeparator()

	case !hasShift && c.isSelectKey3(key, data.KeyCode):
		// Handle 3rd candidate selection key (e.g., quote)
		// Shift 时不触发选择（Shift+' 应输出 " 而非选候选）
		if len(c.candidates) >= 3 && len(c.inputBuffer) > 0 {
			return c.selectCandidate(2) // Select 3rd candidate (index 2)
		}
		// If no candidates, treat as punctuation
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]))
		}
		return nil

	case len(key) == 1 && c.isPunctuation(rune(key[0])):
		return c.handlePunctuation(rune(key[0]))

	default:
		c.logger.Debug("Unhandled key", "key", key, "keycode", data.KeyCode)
		return nil
	}
}
