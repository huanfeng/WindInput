// handle_quick_input_pinyin.go — 快捷输入模式下的临时拼音子模式
// 在快捷输入模式中，字母键（a-z）在缓冲区为空时进入临时拼音子模式，
// 补足混输模式下没有独立临时拼音入口的能力。
// 按键处理、候选更新、UI 显示等核心逻辑委托给 pinyin_mode_shared.go 中的共享实现。
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/schema"
)

// enterQuickInputPinyinMode 在快捷输入模式下进入临时拼音子模式
func (c *Coordinator) enterQuickInputPinyinMode(firstKey string) *bridge.KeyEventResult {
	// 加载拼音引擎
	if c.engineMgr != nil {
		if err := c.engineMgr.EnsurePinyinLoaded(); err != nil {
			c.logger.Warn("Failed to load pinyin engine for quick input pinyin", "error", err)
			return &bridge.KeyEventResult{Type: bridge.ResponseTypeConsumed}
		}
		// 码表引擎下需要交换词库层，避免码表候选污染拼音查询
		// 混输引擎已包含拼音层，无需交换（否则退出时会错误移除混输所需的拼音层）
		if c.engineMgr.IsCurrentEngineType(schema.EngineTypeCodeTable) {
			c.engineMgr.ActivateTempPinyin()
			c.quickInputPinyinDictSwapped = true
		}
	}

	c.quickInputPinyinMode = true
	c.quickInputPinyinBuffer = firstKey
	c.currentPage = 1
	c.selectedIndex = 0

	c.logger.Debug("Entered quick input pinyin mode", "firstKey", firstKey)

	ops := c.quickInputPinyinOps()
	c.updatePinyinModeCandidates(ops)
	c.showPinyinModeUI(ops)

	return c.pinyinModeCompositionResult(ops)
}

// handleQuickInputPinyinKey 处理快捷输入临时拼音子模式下的按键（委托给共享处理器）
func (c *Coordinator) handleQuickInputPinyinKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	return c.handlePinyinModeKey(c.quickInputPinyinOps(), key, data)
}

// exitQuickInputPinyinToBase 退出拼音子模式，返回快捷输入基础模式
func (c *Coordinator) exitQuickInputPinyinToBase() *bridge.KeyEventResult {
	// 仅在实际交换过词库层时才恢复（码表引擎下）
	if c.quickInputPinyinDictSwapped && c.engineMgr != nil {
		c.engineMgr.DeactivateTempPinyin()
	}

	c.quickInputPinyinMode = false
	c.quickInputPinyinBuffer = ""
	c.quickInputPinyinDictSwapped = false
	c.preeditDisplay = ""
	c.currentPage = 1
	c.selectedIndex = 0

	c.logger.Debug("Exited quick input pinyin to base mode")

	// 返回快捷输入基础状态
	c.updateQuickInputCandidates()
	c.showQuickInputUI()

	preedit := c.quickInputPrefix()
	return &bridge.KeyEventResult{
		Type:     bridge.ResponseTypeUpdateComposition,
		Text:     preedit,
		CaretPos: len(preedit),
	}
}

// exitQuickInputPinyinMode 退出拼音子模式并退出快捷输入
func (c *Coordinator) exitQuickInputPinyinMode(commit bool, text string) *bridge.KeyEventResult {
	// 仅在实际交换过词库层时才恢复
	if c.quickInputPinyinDictSwapped && c.engineMgr != nil {
		c.engineMgr.DeactivateTempPinyin()
	}

	c.quickInputPinyinMode = false
	c.quickInputPinyinBuffer = ""
	c.quickInputPinyinDictSwapped = false
	c.preeditDisplay = ""

	// 记录输入历史，供重复上屏功能使用
	if commit && len(text) > 0 && c.inputHistory != nil {
		c.inputHistory.Record(text, "", "", 0)
	}

	return c.exitQuickInputMode(commit, text)
}

// quickInputPinyinOps 创建快捷输入拼音子模式的操作回调
func (c *Coordinator) quickInputPinyinOps() *pinyinModeOps {
	return &pinyinModeOps{
		buffer: &c.quickInputPinyinBuffer,
		prefix: c.quickInputPrefix,
		exitMode: func(commit bool, text string) *bridge.KeyEventResult {
			return c.exitQuickInputPinyinMode(commit, text)
		},
		exitOnBackspaceEmpty: func() *bridge.KeyEventResult {
			return c.exitQuickInputPinyinToBase()
		},
		separator: func(key string, keyCode int) bool {
			return c.isPinyinSeparatorForBuffer(c.quickInputPinyinBuffer, key, keyCode)
		},
		triggerKey: func(key string, keyCode int) bool {
			return c.isQuickInputTriggerKey(key, keyCode)
		},
		consumeSpaceEmpty: true,
	}
}
