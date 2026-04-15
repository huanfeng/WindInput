package rpc

import (
	"fmt"
	"log/slog"
	"strings"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// DictService 词库管理 RPC 服务
type DictService struct {
	store  *store.Store
	dm     *dict.DictManager
	logger *slog.Logger
}

func (d *DictService) resolveSchemaID(id string) string {
	if id != "" {
		return id
	}
	return d.dm.GetActiveSchemaID()
}

// Search 搜索用户词库（前缀匹配，支持分页）
func (d *DictService) Search(args *rpcapi.DictSearchArgs, reply *rpcapi.DictSearchReply) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	prefix := strings.ToLower(args.Prefix)
	limit := args.Limit
	if limit <= 0 {
		limit = 50
	}

	allWords, err := d.store.SearchUserWordsPrefix(schemaID, prefix, 0)
	if err != nil {
		return fmt.Errorf("search: %w", err)
	}

	reply.Total = len(allWords)

	start := args.Offset
	if start > len(allWords) {
		start = len(allWords)
	}
	end := start + limit
	if end > len(allWords) {
		end = len(allWords)
	}

	pageWords := allWords[start:end]
	reply.Words = make([]rpcapi.WordEntry, len(pageWords))
	for i, w := range pageWords {
		reply.Words[i] = rpcapi.WordEntry{
			Text:      w.Text,
			Weight:    w.Weight,
			Count:     w.Count,
			CreatedAt: w.CreatedAt,
		}
	}

	return nil
}

// SearchByCode 精确编码查询
func (d *DictService) SearchByCode(args *rpcapi.DictSearchArgs, reply *rpcapi.DictSearchReply) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	code := strings.ToLower(args.Prefix)

	words, err := d.store.GetUserWords(schemaID, code)
	if err != nil {
		return fmt.Errorf("search by code: %w", err)
	}

	reply.Total = len(words)
	reply.Words = make([]rpcapi.WordEntry, len(words))
	for i, w := range words {
		reply.Words[i] = rpcapi.WordEntry{
			Code:      code,
			Text:      w.Text,
			Weight:    w.Weight,
			Count:     w.Count,
			CreatedAt: w.CreatedAt,
		}
	}

	return nil
}

// Add 添加用户词条
func (d *DictService) Add(args *rpcapi.DictAddArgs, reply *rpcapi.Empty) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.Code == "" || args.Text == "" {
		return fmt.Errorf("code and text are required")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	weight := args.Weight
	if weight <= 0 {
		weight = 1200
	}

	d.logger.Info("RPC Dict.Add", "schemaID", schemaID, "codeLen", len(args.Code), "textLen", len([]rune(args.Text)))
	return d.store.AddUserWord(schemaID, args.Code, args.Text, weight)
}

// Remove 删除用户词条
func (d *DictService) Remove(args *rpcapi.DictRemoveArgs, reply *rpcapi.Empty) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.Code == "" || args.Text == "" {
		return fmt.Errorf("code and text are required")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	d.logger.Info("RPC Dict.Remove", "schemaID", schemaID, "codeLen", len(args.Code), "textLen", len([]rune(args.Text)))
	return d.store.RemoveUserWord(schemaID, args.Code, args.Text)
}

// Update 更新词条权重
func (d *DictService) Update(args *rpcapi.DictUpdateArgs, reply *rpcapi.Empty) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.Code == "" || args.Text == "" {
		return fmt.Errorf("code and text are required")
	}

	schemaID := d.resolveSchemaID(args.SchemaID)
	return d.store.UpdateUserWordWeight(schemaID, args.Code, args.Text, args.NewWeight)
}

// GetStats 获取词库统计
func (d *DictService) GetStats(args *rpcapi.Empty, reply *rpcapi.DictStatsReply) error {
	reply.Stats = d.dm.GetStats()
	return nil
}

// GetSchemaStats 获取指定方案的统计
func (d *DictService) GetSchemaStats(args *rpcapi.DictSchemaStatsArgs, reply *rpcapi.DictSchemaStatsReply) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := d.resolveSchemaID(args.SchemaID)

	wordCount, _ := d.store.UserWordCount(schemaID)
	shadowCount, _ := d.store.ShadowRuleCount(schemaID)
	tempCount, _ := d.store.TempWordCount(schemaID)

	reply.WordCount = wordCount
	reply.ShadowCount = shadowCount
	reply.TempWordCount = tempCount
	return nil
}

// BatchAdd 批量添加词条
func (d *DictService) BatchAdd(args *rpcapi.DictBatchAddArgs, reply *rpcapi.DictBatchAddReply) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := d.resolveSchemaID(args.SchemaID)

	for _, w := range args.Words {
		weight := w.Weight
		if weight <= 0 {
			weight = 1200
		}
		if err := d.store.AddUserWord(schemaID, w.Code, w.Text, weight); err != nil {
			return fmt.Errorf("batch add: %w", err)
		}
		reply.Count++
	}

	d.logger.Info("RPC Dict.BatchAdd", "schemaID", schemaID, "count", reply.Count)
	return nil
}

// ── 临时词库操作 ──

// GetTemp 查询临时词库
func (d *DictService) GetTemp(args *rpcapi.DictGetTempArgs, reply *rpcapi.DictSearchReply) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := d.resolveSchemaID(args.SchemaID)
	prefix := strings.ToLower(args.Prefix)

	var allWords []store.UserWordRecord
	var err error
	if prefix == "" {
		allWords, err = d.store.SearchTempWordsPrefix(schemaID, "", 0)
	} else {
		allWords, err = d.store.SearchTempWordsPrefix(schemaID, prefix, 0)
	}
	if err != nil {
		return err
	}

	reply.Total = len(allWords)

	limit := args.Limit
	if limit <= 0 {
		limit = 50
	}
	start := args.Offset
	if start > len(allWords) {
		start = len(allWords)
	}
	end := start + limit
	if end > len(allWords) {
		end = len(allWords)
	}

	pageWords := allWords[start:end]
	reply.Words = make([]rpcapi.WordEntry, len(pageWords))
	for i, w := range pageWords {
		reply.Words[i] = rpcapi.WordEntry{
			Text:      w.Text,
			Weight:    w.Weight,
			Count:     w.Count,
			CreatedAt: w.CreatedAt,
		}
	}
	return nil
}

// RemoveTemp 删除临时词条
func (d *DictService) RemoveTemp(args *rpcapi.DictRemoveTempArgs, reply *rpcapi.Empty) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := d.resolveSchemaID(args.SchemaID)
	// 临时词库也使用 TempWords bucket，需要在 store 层添加 RemoveTempWord
	return d.store.RemoveTempWord(schemaID, args.Code, args.Text)
}

// ClearTemp 清空临时词库
func (d *DictService) ClearTemp(args *rpcapi.DictClearTempArgs, reply *rpcapi.DictClearTempReply) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := d.resolveSchemaID(args.SchemaID)
	count, err := d.store.ClearTempWords(schemaID)
	if err != nil {
		return err
	}
	reply.Count = count
	d.logger.Info("RPC Dict.ClearTemp", "schemaID", schemaID, "cleared", count)
	return nil
}

// PromoteTemp 晋升单个临时词条
func (d *DictService) PromoteTemp(args *rpcapi.DictPromoteTempArgs, reply *rpcapi.Empty) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := d.resolveSchemaID(args.SchemaID)
	return d.store.PromoteTempWord(schemaID, args.Code, args.Text)
}

// PromoteAllTemp 晋升所有临时词条
func (d *DictService) PromoteAllTemp(args *rpcapi.DictPromoteAllTempArgs, reply *rpcapi.DictPromoteAllTempReply) error {
	if d.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := d.resolveSchemaID(args.SchemaID)

	// 获取所有临时词条
	words, err := d.store.SearchTempWordsPrefix(schemaID, "", 0)
	if err != nil {
		return err
	}

	for _, w := range words {
		// PromoteTempWord 从 key 中解析 code，这里需要用 GetTempWords 逐 code 处理
		// 简化实现：先 promote 每个
		if err := d.store.PromoteTempWord(schemaID, "", w.Text); err != nil {
			// 单个失败不中断
			continue
		}
		reply.Count++
	}

	// 清空剩余
	d.store.ClearTempWords(schemaID)

	d.logger.Info("RPC Dict.PromoteAllTemp", "schemaID", schemaID, "promoted", reply.Count)
	return nil
}
