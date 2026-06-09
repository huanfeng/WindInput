// mode_trigger.go — 触发键激活模式的统一优先级回落链。
// 设计见 docs/design/mode-trigger-priority-chain.md。
package coordinator

import (
	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/keys"
)

// triggerKeyVK 把规范化按键映射到 OEM 虚拟键码（触发键子集，纯标点键）。
var triggerKeyVK = map[keys.Key]uint32{
	keys.KeyGrave:     ipc.VK_OEM_3,
	keys.KeySemicolon: ipc.VK_OEM_1,
	keys.KeyQuote:     ipc.VK_OEM_7,
	keys.KeyComma:     ipc.VK_OEM_COMMA,
	keys.KeyPeriod:    ipc.VK_OEM_PERIOD,
	keys.KeySlash:     ipc.VK_OEM_2,
	keys.KeyBackslash: ipc.VK_OEM_5,
	keys.KeyLBracket:  ipc.VK_OEM_4,
	keys.KeyRBracket:  ipc.VK_OEM_6,
}

// matchTriggerKeyInList 判断 (key,keyCode) 是否匹配 triggerKeys 列表中的某个键，
// 返回匹配到的配置项字符串（空串=未匹配）。纯键匹配，不含任何状态门禁。
// 同时支持字面值（key 字符串）与 VK 码两种匹配，兼容 key 字段缺失的场景。
func matchTriggerKeyInList(triggerKeys []string, key string, keyCode int) string {
	if len(triggerKeys) == 0 {
		return ""
	}
	parsedKey, _ := keys.ParseKey(key)
	vk := uint32(keyCode)
	for _, tk := range triggerKeys {
		tkKey, _ := keys.ParseKey(tk)
		wantVK, ok := triggerKeyVK[tkKey]
		if !ok {
			continue
		}
		if parsedKey == tkKey || vk == wantVK {
			return tk
		}
	}
	return ""
}

// triggerActionKind 是 buffer 非空时一个触发键的归属裁决。
type triggerActionKind int

const (
	actNone            triggerActionKind = iota // 未处理，调用方继续（标点等）
	actAlphaKey                                 // 双拼韵母键送引擎
	actSelectCandidate                          // 选候选
	actEnterMode                                // 顶码上屏 + 进模式
	actOverflow                                 // 二三候选键候选不足回落
)

// bufferedTriggerDecision 是纯数据裁决结果（无副作用，便于单测）。
type bufferedTriggerDecision struct {
	kind         triggerActionKind
	candidateIdx int    // actSelectCandidate
	modeName     string // actEnterMode
	triggerKey   string // actEnterMode
	commitIdx    int    // actEnterMode：顶码上屏候选索引，-1=空码
	overflowKey  string // actOverflow
}

// triggerModeEntry 描述一个触发键激活的模式（轻量模式表项）。
type triggerModeEntry struct {
	name  string
	match func(key string, keyCode int) string   // 含 enabled；空=不匹配
	setup func(triggerKey string) (string, bool) // 设置模式状态，返回 (prefix, ok)
}

// triggerModes 按优先级返回模式表。
// 顺序：快捷输入 > 临时拼音 > 特殊模式（配置顺序）> 临时英文。
// 详见 docs/design/mode-trigger-priority-chain.md。
func (c *Coordinator) triggerModes() []triggerModeEntry {
	modes := []triggerModeEntry{
		{name: "quick_input", match: c.matchQuickInputTrigger, setup: c.setupQuickInputMode},
		{name: "temp_pinyin", match: c.matchTempPinyinTrigger, setup: c.setupTempPinyinMode},
	}
	// 特殊模式实例（配置顺序）插入临时拼音之后、临时英文之前。
	if c.specialModeReg != nil {
		for _, inst := range c.specialModeReg.instances {
			id := inst.cfg.ID
			modes = append(modes, triggerModeEntry{
				name:  "special:" + id,
				match: func(key string, keyCode int) string { return c.matchSpecialTrigger(id, key, keyCode) },
				setup: func(triggerKey string) (string, bool) { return c.setupSpecialMode(id, triggerKey) },
			})
		}
	}
	modes = append(modes, triggerModeEntry{name: "temp_english", match: c.matchTempEnglishTrigger, setup: c.setupTempEnglishMode})
	return modes
}

// decideBufferedTrigger 裁决 buffer 非空 / 有候选时一个 !hasShift 键的归属。
// 纯函数：只读状态，无副作用。优先级 A~F 见设计文档。
func (c *Coordinator) decideBufferedTrigger(key string, keyCode int) bufferedTriggerDecision {
	pageStart := (c.currentPage - 1) * c.candidatesPerPage

	// A. 双拼韵母键优先送引擎
	if len(c.inputBuffer) > 0 && c.isShuangpinFinalKey(key) {
		return bufferedTriggerDecision{kind: actAlphaKey}
	}

	isSel2 := c.isSelectKey2(key, keyCode)
	isSel3 := c.isSelectKey3(key, keyCode)

	// B. 二候选键 + 候选 ≥ 2
	if isSel2 {
		idx := pageStart + 1
		if idx < len(c.candidates) && idx-pageStart < c.candidatesPerPage {
			return bufferedTriggerDecision{kind: actSelectCandidate, candidateIdx: idx}
		}
	}
	// C. 三候选键 + 候选 ≥ 3
	if isSel3 {
		idx := pageStart + 2
		if idx < len(c.candidates) && idx-pageStart < c.candidatesPerPage {
			return bufferedTriggerDecision{kind: actSelectCandidate, candidateIdx: idx}
		}
	}

	// D. 模式激活键（按优先级遍历）
	for _, m := range c.triggerModes() {
		tk := m.match(key, keyCode)
		if tk == "" {
			continue
		}
		commitIdx := -1
		if len(c.candidates) > 0 {
			hi := pageStart + c.selectedIndex
			if hi >= len(c.candidates) {
				hi = 0
			}
			cnd := c.candidates[hi]
			if cnd.IsGroup || len(cnd.Actions) > 0 {
				// 高亮是组/命令候选：不进模式，回落 E/F（与改动前一致）
				break
			}
			commitIdx = hi
		}
		return bufferedTriggerDecision{
			kind: actEnterMode, modeName: m.name, triggerKey: tk, commitIdx: commitIdx,
		}
	}

	// E. 二三候选键候选不足回落
	if isSel2 || isSel3 {
		return bufferedTriggerDecision{kind: actOverflow, overflowKey: key}
	}

	// F. 透传
	return bufferedTriggerDecision{kind: actNone}
}

// findTriggerMode 按 name 取模式表项。
func (c *Coordinator) findTriggerMode(name string) (triggerModeEntry, bool) {
	for _, m := range c.triggerModes() {
		if m.name == name {
			return m, true
		}
	}
	return triggerModeEntry{}, false
}

// enterModeCommitting 顶码上屏当前高亮候选（commitIdx>=0）或丢弃空码（-1），随后进入模式。
// 用 InsertText{HasNewComposition} 把"上屏文本"与"开启模式 preedit"合并为一个原子结果。
// 返回 nil 表示放弃（候选非普通文本 / 模式 setup 失败），调用方应回落后续处理。
func (c *Coordinator) enterModeCommitting(name, triggerKey string, commitIdx int) *bridge.KeyEventResult {
	entry, ok := c.findTriggerMode(name)
	if !ok {
		return nil
	}

	var finalText string
	if commitIdx >= 0 {
		res := c.doSelectCandidate(commitIdx)
		// 仅"完全上屏"(InsertText) 才继续进模式；组候选二级展开/cmdbar 等非 InsertText → 放弃
		if res == nil || res.Type != bridge.ResponseTypeInsertText {
			return nil
		}
		finalText = res.Text
	} else {
		c.clearState()
	}

	prefix, setupOK := entry.setup(triggerKey)
	if !setupOK {
		return nil
	}

	if finalText != "" {
		c.resetCompositionAnchorAfterCommit()
		newComp := ""
		if c.isInlinePreedit() {
			newComp = prefix
		}
		return &bridge.KeyEventResult{
			Type:              bridge.ResponseTypeInsertText,
			Text:              finalText,
			HasNewComposition: true,
			NewComposition:    newComp,
		}
	}
	return c.modeCompositionResult(prefix, len(prefix))
}

// routeBufferedTriggerKey 在 buffer 非空 / 有候选时按优先级回落链处理一个 !hasShift 键。
// 返回 nil 表示本链未处理，调用方继续后续 switch（标点等）。
func (c *Coordinator) routeBufferedTriggerKey(key string, data *bridge.KeyEventData) *bridge.KeyEventResult {
	d := c.decideBufferedTrigger(key, data.KeyCode)
	switch d.kind {
	case actAlphaKey:
		return c.handleAlphaKey(key)
	case actSelectCandidate:
		return c.selectCandidate(d.candidateIdx)
	case actEnterMode:
		return c.enterModeCommitting(d.modeName, d.triggerKey, d.commitIdx)
	case actOverflow:
		return c.handleOverflowSelectKey(d.overflowKey)
	default:
		return nil
	}
}
