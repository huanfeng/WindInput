// handle_key_event.go — 键事件主路由（HandleKeyEvent 函数）
package coordinator

import (
	"strings"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
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
func (c *Coordinator) HandleKeyEvent(data bridge.KeyEventData) (result *bridge.KeyEventResult) {
	startTime := time.Now()

	c.mu.Lock()
	lockTime := time.Since(startTime)
	// 与 coordinator.muLockTraceWait 同口径的 wait WARN, 直接在原有 lockTime 上加阈值检查,
	// 避免重复测时。命中说明 c.mu 在 KeyEvent 路径上有竞争 (典型来源: 跨 client 的
	// HandleIMEActivated/HandleFocusGained 持锁状态机更新)。
	if lockTime > muWaitThreshold {
		c.logger.Warn("coordinator.mu wait", "caller", "HandleKeyEvent", "duration", lockTime)
	}

	// phaseTimer: 排查 KeyEvent 慢请求时定位耗时 phase。
	// HandleKeyEvent 在多分支中 mark 关键边界, 子函数 (updateCandidates / expandCandidates 等)
	// 通过 c.markKeyPhase 暗道贡献自己的 phase。阈值 20ms 与 bridge slowRequestThreshold 一致。
	c.keyPhaseTimer = newPhaseTimer()

	// 重置统计标记，用于 fallback 采集
	c.statRecorded = false
	defer func() {
		// fallback: 若具体路径未记录统计，在此兜底
		if result != nil && !c.statRecorded &&
			(result.Type == bridge.ResponseTypeInsertText || result.Type == bridge.ResponseTypeInsertTextWithCursor) {
			c.recordCommitFallback(result.Text)
		}
		// dump phase breakdown if slow; rich context lets us correlate with input shape。
		c.markKeyPhase("teardown")
		resultType := ""
		if result != nil {
			resultType = string(result.Type)
		}
		c.keyPhaseTimer.dumpIfSlow(20*time.Millisecond, c.logger, "Slow KeyEvent phases",
			"keyCode", data.KeyCode,
			"modifiers", data.Modifiers,
			"lockWait", lockTime,
			"chineseMode", c.chineseMode,
			"bufferLen", len(c.inputBuffer),
			"candidates", len(c.candidates),
			"resultType", resultType,
		)
		c.keyPhaseTimer = nil
		c.mu.Unlock()
	}()

	// Use Debug for high-frequency key events to reduce log noise
	c.logger.Debug("HandleKeyEvent", "key", data.Key, "keycode", data.KeyCode, "modifiers", data.Modifiers, "chineseMode", c.chineseMode, "lockWait", lockTime.String())

	// 数字后智能标点：保存前一按键的数字状态，然后重置。
	// 仅在数字直通（无候选词选择）时重新设置为 true。
	// 对于 modifier-only 按键（Shift/Ctrl/Alt/CapsLock），保持状态不变，
	// 避免 Shift+标点（如 Shift+; 输入冒号）时丢失数字后状态。
	prevDigitState := c.lastOutputWasDigit
	if !isModifierOnlyKey(uint32(data.KeyCode)) {
		c.lastOutputWasDigit = false
		// 统一记录最近一次按键时间，覆盖所有模式（主输入 / 临时英文 / 临时拼音 / 快捷输入），
		// 让 shouldDeferClearForReplay 在跨焦点 replay 场景下能正确识别"打字驱动焦点切换"。
		c.lastKeyTime = startTime
	}

	// Check for Ctrl or Alt modifiers
	hasCtrl := data.Modifiers&ModCtrl != 0
	hasAlt := data.Modifiers&ModAlt != 0
	hasShift := data.Modifiers&ModShift != 0
	hasWin := data.Modifiers&ModWin != 0 // Command 键（macOS ⌘）

	// Handle switch engine hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.SwitchEngine, hasCtrl, hasShift, hasAlt, hasWin, data.KeyCode) {
		return c.handleEngineSwitchKey()
	}

	// Handle full-width toggle hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.ToggleFullWidth, hasCtrl, hasShift, hasAlt, hasWin, data.KeyCode) {
		return c.handleToggleFullWidth()
	}

	// Handle punctuation toggle hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.TogglePunct, hasCtrl, hasShift, hasAlt, hasWin, data.KeyCode) {
		return c.handleTogglePunct()
	}

	// Handle toggle toolbar hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.ToggleToolbar, hasCtrl, hasShift, hasAlt, hasWin, data.KeyCode) {
		return c.handleToggleToolbarKey()
	}

	// Handle open settings hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.OpenSettings, hasCtrl, hasShift, hasAlt, hasWin, data.KeyCode) {
		return c.handleOpenSettingsKey()
	}

	// Handle simplified->traditional toggle hotkey
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.ToggleS2T, hasCtrl, hasShift, hasAlt, hasWin, data.KeyCode) {
		return c.handleToggleS2T()
	}

	// UI 截图快捷键：仅在输入法激活（正在处理按键）时生效，非全局热键
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.TakeScreenshot, hasCtrl, hasShift, hasAlt, hasWin, data.KeyCode) {
		if c.uiManager != nil {
			c.uiManager.TakeUIScreenshots()
		}
		return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
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

	// Ctrl+Shift+R: 剪切板编码粘贴（调试用），任何状态下可用，仅 Debug 版本
	if buildvariant.IsDebug() && hasCtrl && hasShift && !hasAlt && data.KeyCode == 0x52 {
		return c.handleClipboardPasteCode()
	}

	// 加词模式按键拦截（优先于其他处理）
	if c.addWordActive {
		return c.handleAddWordKey(data)
	}

	// 加词快捷键
	if c.config != nil && c.matchHotkey(c.config.Hotkeys.AddWord, hasCtrl, hasShift, hasAlt, hasWin, data.KeyCode) {
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

			// 返回 StatusUpdate 而非 ModeChanged：bridge 响应自带 iconLabel，
			// C++ 端 KeyEventSink::ProcessResponse 走 StatusUpdate 分支调
			// UpdateFullStatus，直接刷新 _inputTypeLabel + 触发 OnUpdate，
			// 任务栏图标立即更新。不再依赖 CMD_STATE_PUSH 的稳定送达。
			return &bridge.KeyEventResult{
				Type:        bridge.ResponseTypeStatusUpdate,
				ChineseMode: c.chineseMode,
				Status:      c.buildStatusUpdate(),
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

	// 敏感字段（密码/隐私）抑制：把 chineseMode/fullWidth 视为关闭，让输入走英文半角直通，
	// 但**不改变** c.chineseMode（图标仍显示当前模式，与主流输入法一致）。
	// 非敏感字段时局部值与字段一致，行为不变。下方输入分支统一用这两个局部变量判定。
	chineseMode := c.chineseMode
	fullWidth := c.fullWidth
	if c.sensitiveFieldActive {
		chineseMode = false
		fullWidth = false
	}

	// English mode: pass through or full-width convert
	if !chineseMode {
		// IME 英文模式自动配对 (仅 darwin 生效; Windows 英文配对由 C++ TSF 层处理,
		// englishModeAutoPairInGo=false 时 handleEnglishModeAutoPair 直接返回 nil)。
		// 仅 !fullWidth、单字符标点才尝试; 非配对字符返回 nil → 维持下方透传。
		// 放在 fullWidth 分支前: 全角模式标点走全角转换不配对, 故只在非全角时接管。
		//
		// 注意: 英文分支在通用 Shift 符号解析 (下方 ~L400 `shiftedKeyMap`) 之前就返回,
		// 故这里必须自行应用 shiftedKeyMap —— 否则 Shift+9 拿到的是 "9" 而非 "(",
		// 括号/花括号/尖括号等 Shift 类配对永远命中不了 (macOS keyCodeToKeyName 不含 Shift)。
		if !fullWidth {
			pairCh := key
			if hasShift && len(pairCh) == 1 {
				if shifted, ok := shiftedKeyMap[pairCh[0]]; ok {
					pairCh = string(shifted)
				}
			}
			if len(pairCh) == 1 && c.isPunctuation(rune(pairCh[0])) {
				if res := c.handleEnglishModeAutoPair(rune(pairCh[0])); res != nil {
					return res
				}
			}
		}
		if fullWidth {
			// 全角模式下，拦截可打印字符并转为全角输出
			// 空格键特殊处理（data.Key 为 "space" 而非 " "）
			if uint32(data.KeyCode) == ipc.VK_SPACE {
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: string(rune(0x3000)),
				}
			}
			if len(key) == 1 && key[0] >= 0x21 && key[0] <= 0x7E {
				// Shift+符号键映射（如 Shift+1 → !）
				actualKey := key
				capsLock := data.IsCapsLockOn()
				if hasShift {
					if shifted, ok := shiftedKeyMap[key[0]]; ok {
						actualKey = string(shifted)
					} else if key[0] >= 'a' && key[0] <= 'z' {
						// CapsLock ON + Shift → 小写; CapsLock OFF + Shift → 大写
						if !capsLock {
							actualKey = strings.ToUpper(key)
						}
					}
				} else if capsLock && key[0] >= 'a' && key[0] <= 'z' {
					// CapsLock ON 无 Shift → 大写
					actualKey = strings.ToUpper(key)
				}
				text := transform.ToFullWidth(actualKey)
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: text,
				}
			}
		}
		return nil
	}

	// 小键盘按键处理
	if numpadChar := numpadKeyToChar(data.KeyCode); numpadChar != "" {
		// "follow_main" 模式：小键盘数字键、小数点键和运算符键视为主键盘按键，参与 IME 处理
		isNumpadDigit := len(numpadChar) == 1 && numpadChar[0] >= '0' && numpadChar[0] <= '9'
		isNumpadDecimal := uint32(data.KeyCode) == ipc.VK_DECIMAL
		isNumpadOp := numpadChar == "*" || numpadChar == "+" || numpadChar == "-" || numpadChar == "/"
		if (isNumpadDigit || isNumpadDecimal || isNumpadOp) && c.config != nil && c.config.Input.NumpadBehavior == "follow_main" {
			// 将小键盘数字/小数点/运算符转为等效主键盘字符，继续后续 IME 流程
			key = numpadChar
		} else {
			// 默认 "direct" 模式：直接输出字符，不参与候选选择或标点转换
			if c.hasPendingInput() {
				c.clearState()
				c.hideUI()
			}
			if c.pairTracker != nil {
				c.pairTracker.Clear()
			}
			if c.pairTrackerEn != nil {
				c.pairTrackerEn.Clear()
			}
			text := numpadChar
			if c.fullWidth {
				text = transform.ToFullWidth(text)
			}
			// 数字后智能标点：小键盘数字也计入
			if isNumpadDigit {
				c.lastOutputWasDigit = true
			}
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: text,
			}
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

	// Chinese mode with CapsLock: output letters directly, support full-width
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
				var outputKey string
				if hasShift {
					outputKey = strings.ToLower(key)
				} else {
					outputKey = strings.ToUpper(key)
				}
				if c.fullWidth {
					outputKey = transform.ToFullWidth(outputKey)
				}

				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: text + outputKey,
				}
			}

			// No pending input, just output letter
			c.clearState()
			c.hideUI()

			// 计算实际输出字符（CapsLock ON: 无 Shift=大写，有 Shift=小写）
			var outputKey string
			if hasShift {
				outputKey = strings.ToLower(key)
			} else {
				outputKey = strings.ToUpper(key)
			}

			// 非全角：透传给宿主，系统 CapsLock 自然产生大写字母，
			// CAD 等依赖 WM_KEYDOWN 的快捷键可正常触发（与英文模式行为一致）
			if !c.fullWidth {
				// 透传前手动记录统计，因为 nil 返回不会触发 recordCommitFallback
				c.recordCommit(outputKey, 0, -1, store.SourcePunctuation)
				return nil
			}

			// 全角：拦截并转换为全角字符
			return &bridge.KeyEventResult{
				Type: bridge.ResponseTypeInsertText,
				Text: transform.ToFullWidth(outputKey),
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

	// 检查是否处于快捷输入模式
	if c.quickInputMode {
		return c.handleQuickInputKey(key, &data)
	}

	// 检查是否处于特殊模式（自定义码表）
	if c.specialMode {
		return c.handleSpecialModeKey(key, &data)
	}

	// 正在输入（有 buffer / 有候选）时：统一优先级回落链
	// （二三候选 > 模式激活 > overflow > 标点）。详见
	// docs/design/mode-trigger-priority-chain.md。
	if !hasShift && c.chineseMode && (len(c.inputBuffer) > 0 || len(c.candidates) > 0) {
		if r := c.routeBufferedTriggerKey(key, &data); r != nil {
			return r
		}
	}

	// buffer 为空且无候选：保留原三段 getXxxTriggerKey 调用，仅按新优先级
	// 顺序重排（快捷输入 > 临时拼音 > 临时英文）。
	// ★ 保留旧 getXxxTriggerKey：getTempPinyinTriggerKey 内含 z 键首次触发逻辑，
	//   matchTempPinyinTrigger 已排除 z；改用 matchXxx 遍历会丢失 z 首次触发 → 回归。
	if triggerKey := c.getQuickInputTriggerKey(key, data.KeyCode); !hasShift && triggerKey != "" {
		return c.enterQuickInputMode(triggerKey)
	}
	if triggerKey := c.getTempPinyinTriggerKey(key, data.KeyCode); !hasShift && triggerKey != "" {
		return c.enterTempPinyinMode(triggerKey)
	}
	if triggerKey := c.getTempEnglishTriggerKey(key, data.KeyCode); !hasShift && triggerKey != "" {
		return c.enterTempEnglishModeWithTrigger(triggerKey)
	}
	// 特殊模式（自定义码表）：buffer 为空时触发
	if !hasShift && c.specialModeReg != nil {
		if id := c.specialModeReg.match(key, data.KeyCode); id != "" {
			if tk := c.matchSpecialTrigger(id, key, data.KeyCode); tk != "" {
				if prefix, ok := c.setupSpecialMode(id, tk); ok {
					return c.modeCompositionResult(prefix, len(prefix))
				}
			}
		}
	}

	// 中文模式下，Shift+字母处理（CapsLock OFF 时）
	if c.chineseMode && !data.IsCapsLockOn() && hasShift {
		if len(key) == 1 && ((key[0] >= 'a' && key[0] <= 'z') || (key[0] >= 'A' && key[0] <= 'Z')) {
			if len(c.inputBuffer) > 0 {
				// 已有输入缓冲时，将大写字母直接追加到输入缓冲
				return c.handleAlphaKey(strings.ToUpper(key))
			}
			if c.config != nil && c.config.Input.ShiftTempEnglish.Enabled {
				behavior := c.config.Input.ShiftTempEnglish.ShiftBehavior
				if behavior == "direct_commit" {
					// 直接上屏大写字母
					outputKey := strings.ToUpper(key)
					if c.fullWidth {
						outputKey = transform.ToFullWidth(outputKey)
					}
					return &bridge.KeyEventResult{
						Type: bridge.ResponseTypeInsertText,
						Text: outputKey,
					}
				}
				// 默认 "temp_english": 进入临时英文模式
				return c.enterTempEnglishMode(key)
			}
		}
	}

	// Chinese mode handling
	vk := uint32(data.KeyCode)

	// 自动配对：方向键、Enter、Escape 等清空配对栈
	if c.pairTracker != nil {
		switch vk {
		case ipc.VK_LEFT, ipc.VK_RIGHT, ipc.VK_UP, ipc.VK_DOWN,
			ipc.VK_HOME, ipc.VK_END, ipc.VK_RETURN, ipc.VK_ESCAPE:
			c.pairTracker.Clear()
		}
	}
	if c.pairTrackerEn != nil {
		switch vk {
		case ipc.VK_LEFT, ipc.VK_RIGHT, ipc.VK_UP, ipc.VK_DOWN,
			ipc.VK_HOME, ipc.VK_END, ipc.VK_RETURN, ipc.VK_ESCAPE:
			c.pairTrackerEn.Clear()
		}
	}

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

	case !hasShift && c.isSelectCharFirstKey(key, data.KeyCode):
		if result := c.handleSelectCharWithOverflow(0, key, prevDigitState, data.PrevChar); result != nil {
			return result
		}
		return nil

	case !hasShift && c.isSelectCharSecondKey(key, data.KeyCode):
		if result := c.handleSelectCharWithOverflow(1, key, prevDigitState, data.PrevChar); result != nil {
			return result
		}
		return nil

	case c.isPageUpKey(key, data.KeyCode, uint32(data.Modifiers)):
		if result := c.handlePageUp(); result != nil {
			return result
		}
		// No candidates — fall through to punctuation if applicable
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]), prevDigitState, data.PrevChar)
		}
		return nil

	case c.isPageDownKey(key, data.KeyCode, uint32(data.Modifiers)):
		if result := c.handlePageDown(); result != nil {
			return result
		}
		// No candidates — fall through to punctuation if applicable
		if len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]), prevDigitState, data.PrevChar)
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
		lowerKey := strings.ToLower(key)
		if buf, ok := c.zHybridFallback(lowerKey); ok {
			return c.enterTempPinyinFromZBuffer(buf, c.inputBuffer, lowerKey)
		}
		// Chinese mode: convert to lowercase for pinyin
		return c.handleAlphaKey(lowerKey)

	case len(key) == 1 && key[0] >= '1' && key[0] <= '9':
		result := c.handleNumberKey(int(key[0] - '0'))
		if result == nil {
			// 数字直通（无候选词选择），标记用于智能标点
			if c.pairTracker != nil {
				c.pairTracker.Clear()
			}
			if c.pairTrackerEn != nil {
				c.pairTrackerEn.Clear()
			}
			c.lastOutputWasDigit = true
			// 空码状态：有待处理输入但无候选，必须显式清空并上屏数字；
			// 透传（nil）会让应用得到数字但 composition 不会结束，导致状态混乱。
			if c.hasPendingInput() {
				c.clearState()
				c.hideUI()
				digit := key
				if c.fullWidth {
					digit = transform.ToFullWidth(key)
				}
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: digit,
				}
			}
			// 全角模式下输出全角数字
			if c.fullWidth {
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: transform.ToFullWidth(key),
				}
			}
			// 透传路径：result 为 nil，defer fallback 不会触发，需主动记录
			c.recordCommit(key, 0, -1, store.SourcePunctuation)
		}
		return result

	case len(key) == 1 && key[0] == '0':
		result := c.handleNumberKey(10)
		if result == nil {
			if c.pairTracker != nil {
				c.pairTracker.Clear()
			}
			if c.pairTrackerEn != nil {
				c.pairTrackerEn.Clear()
			}
			c.lastOutputWasDigit = true
			// 空码状态：有待处理输入但无候选，必须显式清空并上屏数字；
			// 透传（nil）会让应用得到数字但 composition 不会结束，导致状态混乱。
			if c.hasPendingInput() {
				c.clearState()
				c.hideUI()
				digit := key
				if c.fullWidth {
					digit = transform.ToFullWidth(key)
				}
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: digit,
				}
			}
			// 全角模式下输出全角数字
			if c.fullWidth {
				return &bridge.KeyEventResult{
					Type: bridge.ResponseTypeInsertText,
					Text: transform.ToFullWidth(key),
				}
			}
			// 透传路径：result 为 nil，defer fallback 不会触发，需主动记录
			c.recordCommit(key, 0, -1, store.SourcePunctuation)
		}
		return result

	case !hasShift && c.isSelectKey2(key, data.KeyCode):
		// buffer 非空时的二候选/overflow 已由 routeBufferedTriggerKey 接管，
		// 这里只处理无输入缓冲时的标点回退。
		if len(c.inputBuffer) == 0 && len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]), prevDigitState, data.PrevChar)
		}
		return nil

	case !hasShift && c.isPinyinSeparator(key, data.KeyCode):
		return c.handlePinyinSeparator()

	case !hasShift && c.isSelectKey3(key, data.KeyCode):
		// buffer 非空时的三候选/overflow 已由 routeBufferedTriggerKey 接管，
		// 这里只处理无输入缓冲时的标点回退。
		if len(c.inputBuffer) == 0 && len(key) == 1 && c.isPunctuation(rune(key[0])) {
			return c.handlePunctuation(rune(key[0]), prevDigitState, data.PrevChar)
		}
		return nil

	case len(key) == 1 && c.isPunctuation(rune(key[0])):
		return c.handlePunctuation(rune(key[0]), prevDigitState, data.PrevChar)

	default:
		c.logger.Debug("Unhandled key", "key", key, "keycode", data.KeyCode)
		return nil
	}
}
