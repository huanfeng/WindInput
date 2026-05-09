package main

import (
	"fmt"
	"strings"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
)

// ========== 按方案操作词库（左右分栏 UI） ==========

// SchemaDictStats 方案词库统计信息
type SchemaDictStats struct {
	SchemaID      string   `json:"schema_id"`
	SchemaName    string   `json:"schema_name"`
	IconLabel     string   `json:"icon_label"`
	EngineType    string   `json:"engine_type"`
	DataSchemaID  string   `json:"data_schema_id"` // 实际存储桶 ID（用于分组合并）
	AliasIDs      []string `json:"alias_ids"`      // 合并前各子方案的原始 ID
	WordCount     int      `json:"word_count"`
	ShadowCount   int      `json:"shadow_count"`
	TempWordCount int      `json:"temp_word_count"`
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
			EngineType: info.EngineType,
		}

		if schemaStats, err := a.rpcClient.DictGetSchemaStats(schemaID); err == nil {
			stats.DataSchemaID = schemaStats.DataSchemaID
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
			EngineType: info.EngineType,
		}
		if schemaStats, err := a.rpcClient.DictGetSchemaStats(refID); err == nil {
			stats.DataSchemaID = schemaStats.DataSchemaID
			stats.WordCount = schemaStats.WordCount
			stats.ShadowCount = schemaStats.ShadowCount
			stats.TempWordCount = schemaStats.TempWordCount
		}
		result = append(result, stats)
	}

	// 将指向同一数据存储桶的方案合并为单条记录（如全拼/双拼共享 "pinyin" 桶）
	result = mergeSharedSchemas(result)

	return result, nil
}

// mergeSharedSchemas 按 DataSchemaID 分组，将共享同一存储桶的方案合并为单条记录
func mergeSharedSchemas(items []SchemaDictStats) []SchemaDictStats {
	type group struct{ items []SchemaDictStats }
	groups := make(map[string]*group)
	order := make([]string, 0, len(items))

	for _, s := range items {
		dataID := s.DataSchemaID
		if dataID == "" {
			dataID = s.SchemaID
		}
		if _, ok := groups[dataID]; !ok {
			groups[dataID] = &group{}
			order = append(order, dataID)
		}
		groups[dataID].items = append(groups[dataID].items, s)
	}

	result := make([]SchemaDictStats, 0, len(order))
	for _, dataID := range order {
		g := groups[dataID]
		if len(g.items) == 1 && g.items[0].SchemaID == dataID {
			result = append(result, g.items[0])
			continue
		}
		// 多个方案或 SchemaID 与数据桶不同：合并为一条，SchemaID 使用数据桶 ID
		merged := g.items[0]
		merged.SchemaID = dataID
		merged.DataSchemaID = dataID
		// 收集所有原始方案 ID 作为别名，供前端匹配 initialSchema
		aliasIDs := make([]string, len(g.items))
		for i, it := range g.items {
			aliasIDs[i] = it.SchemaID
		}
		merged.AliasIDs = aliasIDs
		if len(g.items) > 1 {
			names := make([]string, len(g.items))
			for i, it := range g.items {
				names[i] = it.SchemaName
			}
			merged.SchemaName = strings.Join(names, " / ") + "（共享词库）"
		}
		result = append(result, merged)
	}
	return result
}

// EncodeWordForSchema 使用码表方案的反向编码为词语生成编码（用于加词时自动填充）
func (a *App) EncodeWordForSchema(schemaID, text string) (string, error) {
	if text == "" {
		return "", nil
	}
	reply, err := a.rpcClient.DictBatchEncode(schemaID, []string{text})
	if err != nil {
		return "", fmt.Errorf("编码失败: %w", err)
	}
	if len(reply.Results) > 0 && reply.Results[0].Code != "" {
		return reply.Results[0].Code, nil
	}
	return "", nil
}

// GeneratePinyinCode 为文字生成全拼编码（用于拼音方案加词时自动填充）
func (a *App) GeneratePinyinCode(text string) (string, error) {
	return a.rpcClient.DictGeneratePinyinCode(text)
}

// GetUserDictBySchema 获取指定方案的用户词库
func (a *App) GetUserDictBySchema(schemaID string) ([]UserWordItem, error) {
	reply, err := a.rpcClient.DictSearch(schemaID, "", "", 0, 0)
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

// PagedDictResult 分页查询结果
type PagedDictResult struct {
	Words []UserWordItem `json:"words"`
	Total int            `json:"total"`
}

// GetUserDictBySchemaPage 分页获取指定方案的用户词库
func (a *App) GetUserDictBySchemaPage(schemaID, prefix, textQuery string, limit, offset int) (*PagedDictResult, error) {
	reply, err := a.rpcClient.DictSearch(schemaID, prefix, textQuery, limit, offset)
	if err != nil {
		return nil, fmt.Errorf("获取用户词库失败: %w", err)
	}
	return &PagedDictResult{
		Words: convertWordEntries(reply.Words),
		Total: reply.Total,
	}, nil
}

// ClearUserDictForSchema 清空指定方案的用户词库
func (a *App) ClearUserDictForSchema(schemaID string) (int, error) {
	return a.rpcClient.DictClearUserWords(schemaID)
}

// SearchUserDictBySchema 搜索指定方案的用户词库
func (a *App) SearchUserDictBySchema(schemaID, query string, limit int) ([]UserWordItem, error) {
	reply, err := a.rpcClient.DictSearch(schemaID, query, "", limit, 0)
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
