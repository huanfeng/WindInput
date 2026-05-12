// handle_temp_pinyin.go — 临时拼音模式（五笔引擎下通过触发键激活）
// 按键处理、候选更新、UI 显示等核心逻辑委托给 pinyin_mode_shared.go 中的共享实现。
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/schema"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/keys"
)

// getTempPinyinTriggerKey 检查按键是否应触发临时拼音模式，返回匹配的触发键类型，空串表示不触发
func (c *Coordinator) getTempPinyinTriggerKey(key string, keyCode int) string {
	// 仅码表类型引擎下生效（如五笔）
	if c.engineMgr == nil || !c.engineMgr.IsCurrentEngineType(schema.EngineTypeCodeTable) {
		return ""
	}
	// 检查当前码表方案是否开启了临时拼音
	if !c.engineMgr.IsTempPinyinEnabled() {
		return ""
	}
	// 仅输入缓冲区为空时触发
	if len(c.inputBuffer) > 0 {
		return ""
	}
	if c.config == nil {
		return ""
	}

	parsedKey, _ := keys.ParseKey(key)
	for _, tk := range c.config.Input.TempPinyin.TriggerKeys {
		tkKey, _ := keys.ParseKey(tk)
		switch tkKey {
		case keys.KeyGrave:
			if parsedKey == keys.KeyGrave || uint32(keyCode) == ipc.VK_OEM_3 {
				return tk
			}
		case keys.KeySemicolon:
			// 仅在输入缓冲区为空且无候选时触发
			// 有候选时 semicolon 仍用于二三候选选择
			if (parsedKey == keys.KeySemicolon || uint32(keyCode) == ipc.VK_OEM_1) && len(c.candidates) == 0 {
				return tk
			}
		case keys.KeyQuote:
			if (parsedKey == keys.KeyQuote || uint32(keyCode) == ipc.VK_OEM_7) && len(c.candidates) == 0 {
				return tk
			}
		case keys.KeyComma:
			if parsedKey == keys.KeyComma || uint32(keyCode) == ipc.VK_OEM_COMMA {
				return tk
			}
		case keys.KeyPeriod:
			if parsedKey == keys.KeyPeriod || uint32(keyCode) == ipc.VK_OEM_PERIOD {
				return tk
			}
		case keys.KeySlash:
			if parsedKey == keys.KeySlash || uint32(keyCode) == ipc.VK_OEM_2 {
				return tk
			}
		case keys.KeyBackslash:
			if parsedKey == keys.KeyBackslash || uint32(keyCode) == ipc.VK_OEM_5 {
				return tk
			}
		case keys.KeyLBracket:
			if parsedKey == keys.KeyLBracket || uint32(keyCode) == ipc.VK_OEM_4 {
				return tk
			}
		case keys.KeyRBracket:
			if parsedKey == keys.KeyRBracket || uint32(keyCode) == ipc.VK_OEM_6 {
				return tk
			}
		case keys.KeyZ:
			// z 键触发：仅在无候选时触发，z 同时作为拼音首字母。
			// 渐进决策：只要 z 还可能扩展为码表/短语候选，就先走正常输入流程，
			// 后续字母再依据"新 buffer 是否仍有前缀匹配"决定是否回退到临时拼音。
			// 这里检查 z 前缀是否存在任何码表/短语；以及 z 键重复是否有历史。
			if parsedKey == keys.KeyZ && len(c.candidates) == 0 {
				if c.engineMgr != nil && c.engineMgr.IsZKeyRepeatEnabled() {
					if c.inputHistory != nil {
						records := c.inputHistory.GetRecentRecords(1, 0)
						if len(records) > 0 && records[0].Text != "" {
							return ""
						}
					}
				}
				if c.engineMgr != nil && c.engineMgr.HasPrefix("z") {
					return ""
				}
				return "z"
			}
		}
	}
	return ""
}

// isTempPinyinTriggerKeyMatch 仅检查按键是否匹配临时拼音触发键（不检查状态条件）
func (c *Coordinator) isTempPinyinTriggerKeyMatch(key string, keyCode int) bool {
	if c.config == nil {
		return false
	}
	parsedKey, _ := keys.ParseKey(key)
	for _, tk := range c.config.Input.TempPinyin.TriggerKeys {
		tkKey, _ := keys.ParseKey(tk)
		switch tkKey {
		case keys.KeyGrave:
			if parsedKey == keys.KeyGrave || uint32(keyCode) == ipc.VK_OEM_3 {
				return true
			}
		case keys.KeySemicolon:
			if parsedKey == keys.KeySemicolon || uint32(keyCode) == ipc.VK_OEM_1 {
				return true
			}
		case keys.KeyQuote:
			if parsedKey == keys.KeyQuote || uint32(keyCode) == ipc.VK_OEM_7 {
				return true
			}
		case keys.KeyComma:
			if parsedKey == keys.KeyComma || uint32(keyCode) == ipc.VK_OEM_COMMA {
				return true
			}
		case keys.KeyPeriod:
			if parsedKey == keys.KeyPeriod || uint32(keyCode) == ipc.VK_OEM_PERIOD {
				return true
			}
		case keys.KeySlash:
			if parsedKey == keys.KeySlash || uint32(keyCode) == ipc.VK_OEM_2 {
				return true
			}
		case keys.KeyBackslash:
			if parsedKey == keys.KeyBackslash || uint32(keyCode) == ipc.VK_OEM_5 {
				return true
			}
		case keys.KeyLBracket:
			if parsedKey == keys.KeyLBracket || uint32(keyCode) == ipc.VK_OEM_4 {
				return true
			}
		case keys.KeyRBracket:
			if parsedKey == keys.KeyRBracket || uint32(keyCode) == ipc.VK_OEM_6 {
				return true
			}
		case keys.KeyZ:
			if parsedKey == keys.KeyZ {
				return true
			}
		}
	}
	return false
}

// enterTempPinyinMode 进入临时拼音模式
// triggerKey 标识触发键类型（"backtick"/"semicolon"/"z"）
func (c *Coordinator) enterTempPinyinMode(triggerKey string) *bridge.KeyEventResult {
	// 确保拼音引擎已加载
	if c.engineMgr != nil {
		if err := c.engineMgr.EnsurePinyinLoaded(); err != nil {
			c.logger.Warn("Failed to load pinyin engine for temp pinyin", "error", err)
			return nil
		}
		// 激活拼音词库层（进入时注册，退出时卸载，避免污染五笔查询）
		c.engineMgr.ActivateTempPinyin()
	}

	c.tempPinyinMode = true
	c.tempPinyinTriggerKey = triggerKey
	c.tempPinyinBuffer = ""
	c.tempPinyinCursorPos = 0
	c.tempPinyinCommitted = ""

	c.logger.Debug("Entered temp pinyin mode", "triggerKey", triggerKey)

	// 首次进入触发 C++ 端 StartComposition，同步标记 pendingFirstShow，
	// 让 Excel/WPS 表格 cell-select→cell-edit 的失焦能命中 replay 路径。
	// 不立即 showUI：等 OnLayoutChange 真实坐标由 HandleCaretUpdate 触发首次显示，
	// 否则会先用按键前的旧坐标显示再跳到正确位置（与 handleAlphaKey 首字符一致）。
	c.armPendingFirstShow()

	prefix := c.tempPinyinPrefix()
	return c.modeCompositionResult(prefix, len(prefix))
}

// tempPinyinPrefix 返回临时拼音模式的前缀显示字符（使用实际触发键字符）
func (c *Coordinator) tempPinyinPrefix() string {
	parsed, _ := keys.ParseKey(c.tempPinyinTriggerKey)
	switch parsed {
	case keys.KeyGrave:
		return "`"
	case keys.KeySemicolon:
		return ";"
	case keys.KeyQuote:
		return "'"
	case keys.KeyComma:
		return ","
	case keys.KeyPeriod:
		return "."
	case keys.KeySlash:
		return "/"
	case keys.KeyBackslash:
		return "\\"
	case keys.KeyLBracket:
		return "["
	case keys.KeyRBracket:
		return "]"
	case keys.KeyZ:
		return "z"
	default:
		return "`"
	}
}

// handleTempPinyinKey 处理临时拼音模式下的按键（委托给共享处理器）
func (c *Coordinator) handleTempPinyinKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	return c.handlePinyinModeKey(c.tempPinyinOps(), key, data)
}

// exitTempPinyinMode 退出临时拼音模式
func (c *Coordinator) exitTempPinyinMode(commit bool, text string) *bridge.KeyEventResult {
	c.tempPinyinMode = false
	c.tempPinyinBuffer = ""
	c.tempPinyinTriggerKey = ""
	c.tempPinyinRewindBuffer = ""
	c.tempPinyinRewindKey = ""
	c.preeditDisplay = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	if c.uiManager != nil {
		c.uiManager.SetModeLabel("")
		c.uiManager.SetModeAccentColor(nil)
	}
	c.hideUI()

	// 卸载拼音词库层，避免污染五笔引擎的查询结果
	if c.engineMgr != nil {
		c.engineMgr.DeactivateTempPinyin()
	}

	c.logger.Debug("Exited temp pinyin mode", "commit", commit, "textLen", len(text))

	if commit && len(text) > 0 {
		// 输入历史在候选最终化点（selectPinyinModeXxx / handlePunctuation）统一记录,
		// 此处不再记录, 以避免把拼音码、触发键、标点等非候选文本误记
		c.tempPinyinCommitted = ""
		c.recordCommit(text, 0, -1, store.SourceTempPinyin)
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}
	c.tempPinyinCommitted = ""

	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// isTempPinyinZTrigger 检查 z 是否配置为临时拼音触发键
func (c *Coordinator) isTempPinyinZTrigger() bool {
	if c.engineMgr == nil || !c.engineMgr.IsTempPinyinEnabled() {
		return false
	}
	if c.config == nil {
		return false
	}
	for _, tk := range c.config.Input.TempPinyin.TriggerKeys {
		if tk == "z" {
			return true
		}
	}
	return false
}

// isZKeyHybridMode 检查是否处于 Z 键混合模式（重复上屏 + 临时拼音同时启用）
func (c *Coordinator) isZKeyHybridMode() bool {
	if c.engineMgr == nil || !c.engineMgr.IsZKeyRepeatEnabled() {
		return false
	}
	if c.config == nil {
		return false
	}
	for _, tk := range c.config.Input.TempPinyin.TriggerKeys {
		if tk == "z" {
			return true
		}
	}
	return false
}

// zHybridFallback 判定 z 键混合模式下当前按键是否应该回退到临时拼音.
// 返回 ok=true 时 pinyinBuffer 是切入临时拼音时的初始 buffer
// (即 inputBuffer 去掉首 z 后再追加新键).
//
// 触发条件 (全部满足):
//   - inputBuffer 非空且以 'z' 开头
//   - z 键被配置为临时拼音触发键或处于 Z 键混合模式
//   - engineMgr 非空
//   - inputBuffer + 新键 在码表/短语层中已无前缀匹配
//
// 抽出独立方法是为了让 z 决策核心可单测, 不必构造完整的 HandleKeyEvent 链路.
func (c *Coordinator) zHybridFallback(lowerKey string) (pinyinBuffer string, ok bool) {
	if len(c.inputBuffer) == 0 || c.inputBuffer[0] != 'z' {
		return "", false
	}
	if !c.isZKeyHybridMode() && !c.isTempPinyinZTrigger() {
		return "", false
	}
	if c.engineMgr == nil {
		return "", false
	}
	if c.engineMgr.HasPrefix(c.inputBuffer + lowerKey) {
		return "", false
	}
	return c.inputBuffer[1:] + lowerKey, true
}

// enterTempPinyinFromZBuffer 从 z 键正常输入路径回退到临时拼音模式。
// initialBuffer 为剩余作为拼音 buffer 的字符（即 inputBuffer 去掉首 z 后再追加新键）。
// rewindBuffer / rewindKey 是切入瞬间被抛弃的 inputBuffer 和触发键, 用于让用户
// 在切入后未做任何编辑时通过 backspace 一键回退到正常输入流.
func (c *Coordinator) enterTempPinyinFromZBuffer(initialBuffer, rewindBuffer, rewindKey string) *bridge.KeyEventResult {
	// 清除当前 z 前缀的输入状态
	c.clearState()
	c.hideUI()

	// 进入临时拼音模式
	if c.engineMgr != nil {
		if err := c.engineMgr.EnsurePinyinLoaded(); err != nil {
			c.logger.Warn("Failed to load pinyin engine for z fallback", "error", err)
			return nil
		}
		c.engineMgr.ActivateTempPinyin()
	}

	c.tempPinyinMode = true
	c.tempPinyinTriggerKey = "z"
	c.tempPinyinBuffer = initialBuffer
	c.tempPinyinCursorPos = len(initialBuffer)
	c.tempPinyinCommitted = ""
	// 记录回退缓存; 任何新字符插入都会在 pinyin_mode_shared.go 里清掉
	c.tempPinyinRewindBuffer = rewindBuffer
	c.tempPinyinRewindKey = rewindKey

	c.logger.Debug("Entered temp pinyin from z fallback", "bufferLen", len(initialBuffer))

	// 更新拼音候选并显示 UI
	ops := c.tempPinyinOps()
	c.updatePinyinModeCandidates(ops)
	c.showPinyinModeUI(ops)

	prefix := c.tempPinyinPrefix()
	preedit := prefix + initialBuffer
	return c.modeCompositionResult(preedit, len(preedit))
}

// rewindTempPinyinToNormal 在 z 键混合切入后第一次 backspace 时被调用,
// 把状态精确还原到切入前的"正常输入路径", 让用户感觉切入与回退是对称的.
//
// 调用前提: c.tempPinyinMode==true 且 c.tempPinyinRewindBuffer 非空且
// c.tempPinyinBuffer 仍是切入瞬间的样子 (即 == rewindBuffer[1:] + rewindKey).
// 调用方 (pinyin_mode_shared.go) 已校验, 此处不再判断.
func (c *Coordinator) rewindTempPinyinToNormal() *bridge.KeyEventResult {
	pre := c.tempPinyinRewindBuffer

	// 拆掉临时拼音状态, 不上屏任何内容
	c.tempPinyinMode = false
	c.tempPinyinBuffer = ""
	c.tempPinyinCursorPos = 0
	c.tempPinyinCommitted = ""
	c.tempPinyinTriggerKey = ""
	c.tempPinyinRewindBuffer = ""
	c.tempPinyinRewindKey = ""
	if c.engineMgr != nil {
		c.engineMgr.DeactivateTempPinyin()
	}
	if c.uiManager != nil {
		c.uiManager.SetModeLabel("")
		c.uiManager.SetModeAccentColor(nil)
	}

	// 还原正常输入路径: inputBuffer = 切入前的内容, 刷新候选 + UI
	c.inputBuffer = pre
	c.inputCursorPos = len(pre)
	c.preeditDisplay = ""
	c.updateCandidates()
	c.showUI()

	c.logger.Debug("Rewound from temp pinyin back to normal input", "buffer", pre)

	return c.compositionUpdateResult()
}

// tempPinyinOps 创建临时拼音模式的操作回调
func (c *Coordinator) tempPinyinOps() *pinyinModeOps {
	return &pinyinModeOps{
		buffer:    &c.tempPinyinBuffer,
		cursorPos: &c.tempPinyinCursorPos,
		committed: &c.tempPinyinCommitted,
		prefix:    c.tempPinyinPrefix,
		exitMode: func(commit bool, text string) *bridge.KeyEventResult {
			return c.exitTempPinyinMode(commit, text)
		},
		exitOnBackspaceEmpty: func() *bridge.KeyEventResult {
			return c.exitTempPinyinMode(false, "")
		},
		separator: func(key string, keyCode int) bool {
			return c.isPinyinSeparatorForBuffer(c.tempPinyinBuffer, key, keyCode)
		},
		triggerKey: func(key string, keyCode int) bool {
			return c.isTempPinyinTriggerKeyMatch(key, keyCode)
		},
		consumeSpaceEmpty: false,
	}
}
