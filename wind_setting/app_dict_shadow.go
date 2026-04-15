package main

import "fmt"

// ========== Shadow 管理 ==========

// ShadowRuleItem Shadow 规则项（用于前端）
type ShadowRuleItem struct {
	Code     string `json:"code"`
	Word     string `json:"word"`
	Type     string `json:"type"`     // "pin" 或 "delete"
	Position int    `json:"position"` // 仅 pin 有效
}

// GetShadowRules 获取所有 Shadow 规则
func (a *App) GetShadowRules() ([]ShadowRuleItem, error) {
	reply, err := a.rpcClient.ShadowGetAllRules("")
	if err != nil {
		return nil, fmt.Errorf("获取 Shadow 规则失败: %w", err)
	}

	var items []ShadowRuleItem
	for _, cr := range reply.Rules {
		for _, p := range cr.Pinned {
			items = append(items, ShadowRuleItem{
				Code:     cr.Code,
				Word:     p.Word,
				Type:     "pin",
				Position: p.Position,
			})
		}
		for _, d := range cr.Deleted {
			items = append(items, ShadowRuleItem{
				Code: cr.Code,
				Word: d,
				Type: "delete",
			})
		}
	}

	return items, nil
}

// PinShadowWord 固定词到指定位置
func (a *App) PinShadowWord(code, word string, position int) error {
	return a.rpcClient.ShadowPin("", code, word, position)
}

// DeleteShadowWord 隐藏词条
func (a *App) DeleteShadowWord(code, word string) error {
	return a.rpcClient.ShadowDelete("", code, word)
}

// RemoveShadowRule 删除 Shadow 规则
func (a *App) RemoveShadowRule(code, word string) error {
	return a.rpcClient.ShadowRemoveRule("", code, word)
}
