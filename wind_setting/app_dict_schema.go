package main

import (
	"fmt"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
)

// ========== 按方案操作词库（左右分栏 UI） ==========

// SchemaDictStats 方案词库统计信息
type SchemaDictStats struct {
	SchemaID      string `json:"schema_id"`
	SchemaName    string `json:"schema_name"`
	IconLabel     string `json:"icon_label"`
	WordCount     int    `json:"word_count"`
	ShadowCount   int    `json:"shadow_count"`
	TempWordCount int    `json:"temp_word_count"`
}

// TempWordItem 临时词条（前端展示用）
type TempWordItem struct {
	Code   string `json:"code"`
	Text   string `json:"text"`
	Weight int    `json:"weight"`
	Count  int    `json:"count"` // 选择次数
}

// GetEnabledSchemasWithDictStats 获取所有已启用方案及其词库统计
func (a *App) GetEnabledSchemasWithDictStats() ([]SchemaDictStats, error) {
	cfg, err := config.Load()
	if err != nil {
		return nil, fmt.Errorf("加载配置失败: %w", err)
	}

	schemas, err := a.GetAvailableSchemas()
	if err != nil {
		return nil, err
	}

	// 建立 ID→SchemaInfo 映射
	schemaMap := make(map[string]SchemaInfo)
	for _, s := range schemas {
		schemaMap[s.ID] = s
	}

	// 获取引用关系，用于过滤引用式混输方案（其用户数据继承自主方案）
	refs, _ := a.GetSchemaReferences()

	var result []SchemaDictStats
	for _, schemaID := range cfg.Schema.Available {
		info, ok := schemaMap[schemaID]
		if !ok {
			continue
		}

		// 引用式混输方案的用户数据与主方案共享，不在词库管理中重复显示
		if ref, ok := refs[schemaID]; ok && (ref.PrimarySchema != "" || ref.SecondarySchema != "") {
			continue
		}

		stats := SchemaDictStats{
			SchemaID:   schemaID,
			SchemaName: info.Name,
			IconLabel:  info.IconLabel,
		}

		if schemaStats, err := a.rpcClient.DictGetSchemaStats(schemaID); err == nil {
			stats.WordCount = schemaStats.WordCount
			stats.ShadowCount = schemaStats.ShadowCount
			stats.TempWordCount = schemaStats.TempWordCount
		}

		result = append(result, stats)
	}

	// 添加被引用但未启用的方案（如混输引用的 pinyin 方案有独立的 userfreq）
	addedIDs := make(map[string]bool)
	for _, s := range result {
		addedIDs[s.SchemaID] = true
	}
	refIDs, _ := a.GetReferencedSchemaIDs()
	for _, refID := range refIDs {
		if addedIDs[refID] {
			continue
		}
		info, ok := schemaMap[refID]
		if !ok {
			continue
		}
		stats := SchemaDictStats{
			SchemaID:   refID,
			SchemaName: info.Name,
			IconLabel:  info.IconLabel,
		}
		if schemaStats, err := a.rpcClient.DictGetSchemaStats(refID); err == nil {
			stats.WordCount = schemaStats.WordCount
			stats.ShadowCount = schemaStats.ShadowCount
			stats.TempWordCount = schemaStats.TempWordCount
		}
		result = append(result, stats)
	}

	return result, nil
}

// GetUserDictBySchema 获取指定方案的用户词库
func (a *App) GetUserDictBySchema(schemaID string) ([]UserWordItem, error) {
	reply, err := a.rpcClient.DictSearch(schemaID, "", 0, 0)
	if err != nil {
		return nil, fmt.Errorf("获取用户词库失败: %w", err)
	}
	return convertWordEntries(reply.Words), nil
}

// AddUserWordForSchema 向指定方案添加用户词条
func (a *App) AddUserWordForSchema(schemaID, code, text string, weight int) error {
	return a.rpcClient.DictAdd(schemaID, code, text, weight)
}

// RemoveUserWordForSchema 从指定方案删除用户词条
func (a *App) RemoveUserWordForSchema(schemaID, code, text string) error {
	return a.rpcClient.DictRemove(schemaID, code, text)
}

// SearchUserDictBySchema 搜索指定方案的用户词库
func (a *App) SearchUserDictBySchema(schemaID, query string, limit int) ([]UserWordItem, error) {
	reply, err := a.rpcClient.DictSearch(schemaID, query, limit, 0)
	if err != nil {
		return nil, fmt.Errorf("搜索用户词库失败: %w", err)
	}
	return convertWordEntries(reply.Words), nil
}

// GetShadowBySchema 获取指定方案的 Shadow 规则
func (a *App) GetShadowBySchema(schemaID string) ([]ShadowRuleItem, error) {
	reply, err := a.rpcClient.ShadowGetAllRules(schemaID)
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

// PinShadowWordForSchema 在指定方案中固定词到指定位置
func (a *App) PinShadowWordForSchema(schemaID, code, word string, position int) error {
	return a.rpcClient.ShadowPin(schemaID, code, word, position)
}

// DeleteShadowWordForSchema 在指定方案中隐藏词条
func (a *App) DeleteShadowWordForSchema(schemaID, code, word string) error {
	return a.rpcClient.ShadowDelete(schemaID, code, word)
}

// RemoveShadowRuleForSchema 在指定方案中删除规则
func (a *App) RemoveShadowRuleForSchema(schemaID, code, word string) error {
	return a.rpcClient.ShadowRemoveRule(schemaID, code, word)
}

// GetTempDictBySchema 获取指定方案的临时词库
func (a *App) GetTempDictBySchema(schemaID string) ([]TempWordItem, error) {
	reply, err := a.rpcClient.DictGetTemp(schemaID, "", 0, 0)
	if err != nil {
		return nil, fmt.Errorf("获取临时词库失败: %w", err)
	}

	items := make([]TempWordItem, len(reply.Words))
	for i, w := range reply.Words {
		items[i] = TempWordItem{
			Code:   w.Code,
			Text:   w.Text,
			Weight: w.Weight,
			Count:  w.Count,
		}
	}
	return items, nil
}

// ClearTempDictForSchema 清空指定方案的临时词库
func (a *App) ClearTempDictForSchema(schemaID string) (int, error) {
	return a.rpcClient.DictClearTemp(schemaID)
}

// PromoteTempWordForSchema 将临时词条晋升到用户词库
func (a *App) PromoteTempWordForSchema(schemaID, code, text string) error {
	return a.rpcClient.DictPromoteTemp(schemaID, code, text)
}

// PromoteAllTempWordsForSchema 将所有临时词条晋升到用户词库
func (a *App) PromoteAllTempWordsForSchema(schemaID string) (int, error) {
	return a.rpcClient.DictPromoteAllTemp(schemaID)
}

// RemoveTempWordForSchema 从临时词库删除词条
func (a *App) RemoveTempWordForSchema(schemaID, code, text string) error {
	return a.rpcClient.DictRemoveTemp(schemaID, code, text)
}

// ========== 辅助：时间戳转换 ==========

// formatCreatedAt 将 unix 时间戳转为 RFC3339 字符串
func formatCreatedAt(ts int64) string {
	if ts == 0 {
		return ""
	}
	return time.Unix(ts, 0).Format(time.RFC3339)
}
