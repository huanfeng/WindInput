// handle_temp_pinyin.go — 临时拼音模式（五笔引擎下通过触发键激活）
// 按键处理、候选更新、UI 显示等核心逻辑委托给 pinyin_mode_shared.go 中的共享实现。
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/schema"
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

	for _, tk := range c.config.Input.TempPinyin.TriggerKeys {
		switch tk {
		case "backtick":
			if key == "`" || uint32(keyCode) == ipc.VK_OEM_3 {
				return "backtick"
			}
		case "semicolon":
			// 仅在输入缓冲区为空且无候选时触发
			// 有候选时 semicolon 仍用于二三候选选择
			if (key == ";" || uint32(keyCode) == ipc.VK_OEM_1) && len(c.candidates) == 0 {
				return "semicolon"
			}
		case "z":
			// z 键触发：仅在无候选时触发，z 同时作为拼音首字母
			if key == "z" && len(c.candidates) == 0 {
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
	for _, tk := range c.config.Input.TempPinyin.TriggerKeys {
		switch tk {
		case "backtick":
			if key == "`" || uint32(keyCode) == ipc.VK_OEM_3 {
				return true
			}
		case "semicolon":
			if key == ";" || uint32(keyCode) == ipc.VK_OEM_1 {
				return true
			}
		case "z":
			if key == "z" {
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

	c.logger.Debug("Entered temp pinyin mode", "triggerKey", triggerKey)

	ops := c.tempPinyinOps()
	c.showPinyinModeUI(ops)

	prefix := c.tempPinyinPrefix()
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     prefix,
		CaretPos: len(prefix),
	}
}

// tempPinyinPrefix 返回临时拼音模式的前缀显示字符
func (c *Coordinator) tempPinyinPrefix() string {
	return "`"
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
	c.preeditDisplay = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	if c.uiManager != nil {
		c.uiManager.SetModeLabel("")
	}
	c.hideUI()

	// 卸载拼音词库层，避免污染五笔引擎的查询结果
	if c.engineMgr != nil {
		c.engineMgr.DeactivateTempPinyin()
	}

	c.logger.Debug("Exited temp pinyin mode", "commit", commit, "textLen", len(text))

	if commit && len(text) > 0 {
		// 记录输入历史，供重复上屏功能使用
		if c.inputHistory != nil {
			c.inputHistory.Record(text, "", "", 0)
		}
		return &bridge.KeyEventResult{
			Type: bridge.ResponseTypeInsertText,
			Text: text,
		}
	}

	return &bridge.KeyEventResult{Type: bridge.ResponseTypeClearComposition}
}

// tempPinyinOps 创建临时拼音模式的操作回调
func (c *Coordinator) tempPinyinOps() *pinyinModeOps {
	return &pinyinModeOps{
		buffer: &c.tempPinyinBuffer,
		prefix: c.tempPinyinPrefix,
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
