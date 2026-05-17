package main

import "fmt"

// ========== Shadow 管理 ==========

// ShadowRuleItem Shadow 规则项（用于前端）
//
// 2026-05-17 R2: 新增 CandID 字段, 短语候选规则用 id 匹配。
type ShadowRuleItem struct {
	Code     string `json:"code"`
	Word     string `json:"word"`
	CandID   string `json:"cand_id,omitempty"` // 候选稳定 id (短语场景)
	Type     string `json:"type"`              // "pin" 或 "delete"
	Position int    `json:"position"`          // 仅 pin 有效
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
				CandID:   p.CandID,
				Type:     "pin",
				Position: p.Position,
			})
		}
		for _, d := range cr.Deleted {
			items = append(items, ShadowRuleItem{
				Code:   cr.Code,
				Word:   d.Word,
				CandID: d.CandID,
				Type:   "delete",
			})
		}
	}

	return items, nil
}

// PinShadowWord 固定词到指定位置 (候选 id 可选)
func (a *App) PinShadowWord(code, word, candID string, position int) error {
	return a.rpcClient.ShadowPin("", code, word, candID, position)
}

// DeleteShadowWord 隐藏词条/候选
func (a *App) DeleteShadowWord(code, word, candID string) error {
	return a.rpcClient.ShadowDelete("", code, word, candID)
}

// RemoveShadowRule 删除 Shadow 规则
func (a *App) RemoveShadowRule(code, word, candID string) error {
	return a.rpcClient.ShadowRemoveRule("", code, word, candID)
}
