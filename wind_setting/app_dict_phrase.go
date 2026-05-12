package main

import (
	"fmt"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"
	"gopkg.in/yaml.v3"
)

// ========== 短语管理（通过 RPC）==========

// PhraseItem 短语条目（前端用）
type PhraseItem struct {
	Code     string `json:"code"`
	Text     string `json:"text,omitempty"`
	Texts    string `json:"texts,omitempty"`
	Name     string `json:"name,omitempty"`
	Type     string `json:"type"`
	Position int    `json:"position"`
	Enabled  bool   `json:"enabled"`
	IsSystem bool   `json:"is_system"`
}

// GetPhrases 获取所有短语（通过 RPC）
func (a *App) GetPhrases() ([]PhraseItem, error) {
	reply, err := a.rpcClient.PhraseList()
	if err != nil {
		return nil, fmt.Errorf("获取短语列表失败: %w", err)
	}
	items := make([]PhraseItem, len(reply.Phrases))
	for i, p := range reply.Phrases {
		items[i] = PhraseItem{
			Code: p.Code, Text: p.Text, Texts: p.Texts, Name: p.Name,
			Type: p.Type, Position: p.Position, Enabled: p.Enabled, IsSystem: p.IsSystem,
		}
	}
	return items, nil
}

// AddPhrase 添加短语
func (a *App) AddPhrase(code, text, texts, name, pType string, position int) error {
	return a.rpcClient.PhraseAdd(rpcapi.PhraseAddArgs{
		Code: code, Text: text, Texts: texts, Name: name, Type: pType, Position: position,
	})
}

// UpdatePhrase 更新短语
func (a *App) UpdatePhrase(code, text, name, newCode, newText string, newPosition int, enabled *bool) error {
	return a.rpcClient.PhraseUpdate(rpcapi.PhraseUpdateArgs{
		Code: code, Text: text, Name: name, NewCode: newCode, NewText: newText, NewPosition: newPosition, Enabled: enabled,
	})
}

// RemovePhrase 删除短语
func (a *App) RemovePhrase(code, text, name string) error {
	return a.rpcClient.PhraseRemove(code, text, name)
}

// SetPhraseEnabled 设置短语启用/禁用状态
func (a *App) SetPhraseEnabled(code, text, name string, enabled bool) error {
	return a.rpcClient.PhraseUpdate(rpcapi.PhraseUpdateArgs{
		Code: code, Text: text, Name: name, Enabled: &enabled,
	})
}

// ResetPhrasesToDefault 重置短语为默认值
func (a *App) ResetPhrasesToDefault() error {
	return a.rpcClient.PhraseResetDefaults()
}

// ========== 词频管理 ==========

// FreqItem 词频条目
type FreqItem struct {
	Code     string `json:"code"`
	Text     string `json:"text"`
	Count    int    `json:"count"`
	LastUsed int64  `json:"last_used"`
	Streak   int    `json:"streak"`
	Boost    int    `json:"boost"`
}

// GetFreqList 搜索词频记录
func (a *App) GetFreqList(schemaID, prefix string, limit, offset int) (map[string]interface{}, error) {
	reply, err := a.rpcClient.FreqSearch(schemaID, prefix, limit, offset)
	if err != nil {
		return nil, err
	}
	items := make([]FreqItem, len(reply.Entries))
	for i, e := range reply.Entries {
		items[i] = FreqItem{
			Code: e.Code, Text: e.Text, Count: e.Count,
			LastUsed: e.LastUsed, Streak: e.Streak, Boost: e.Boost,
		}
	}
	return map[string]interface{}{"entries": items, "total": reply.Total}, nil
}

// DeleteFreq 删除单条词频记录
func (a *App) DeleteFreq(schemaID, code, text string) error {
	return a.rpcClient.FreqDelete(schemaID, code, text)
}

// ClearFreq 清空指定方案的所有词频数据
func (a *App) ClearFreq(schemaID string) (int, error) {
	return a.rpcClient.FreqClear(schemaID)
}

// pinyinSharedDictID 拼音共享词库桶 ID（与 wind_input/internal/schema.PinyinSharedDictID 一致）。
// 全拼/双拼以及码表方案的临时拼音、混输方案的拼音辅助均使用此桶存储用户词库，
// 因此即使没有启用任何拼音/双拼方案，该桶仍是有效的存储入口。
const pinyinSharedDictID = "pinyin"

// ========== 方案列表 ==========

// SchemaStatusItem 方案状态信息
type SchemaStatusItem struct {
	SchemaID        string `json:"schema_id"`
	SchemaName      string `json:"schema_name"`
	EngineType      string `json:"engine_type"`                // codetable | pinyin | mixed
	IsMixed         bool   `json:"is_mixed"`                   // 是否为混输方案（用户词库等继承自主方案）
	IsShuangpin     bool   `json:"is_shuangpin"`               // 是否为双拼方案（用户词库的 code 仍以全拼存储）
	ShuangpinLayout string `json:"shuangpin_layout,omitempty"` // 双拼布局 ID
	DataSchemaID    string `json:"data_schema_id,omitempty"`   // 实际存储桶 ID（多个方案可共享同一桶）
	Status          string `json:"status"`
	UserWords       int    `json:"user_words"`
	TempWords       int    `json:"temp_words"`
	ShadowRules     int    `json:"shadow_rules"`
	FreqRecords     int    `json:"freq_records"`
}

// GetAllSchemaStatuses 获取所有方案状态
// 排序：启用方案(按配置顺序) → 未启用但有数据 → 残留(orphaned)
func (a *App) GetAllSchemaStatuses() ([]SchemaStatusItem, error) {
	reply, err := a.rpcClient.SystemListSchemas()
	if err != nil {
		return nil, err
	}

	// 从 GetAvailableSchemas 构建完整 nameMap、engineTypeMap、双拼信息映射
	nameMap := make(map[string]string)
	engineTypeMap := make(map[string]string)
	shuangpinMap := make(map[string]bool)
	shuangpinLayoutMap := make(map[string]string)
	if schemas, err := a.GetAvailableSchemas(); err == nil {
		for _, s := range schemas {
			nameMap[s.ID] = s.Name
			engineTypeMap[s.ID] = s.EngineType
			shuangpinMap[s.ID] = s.IsShuangpin
			shuangpinLayoutMap[s.ID] = s.ShuangpinLayout
		}
	}

	// 获取引用关系，判断混输方案
	mixedSet := make(map[string]bool)
	if refs, err := a.GetSchemaReferences(); err == nil {
		for id, ref := range refs {
			if ref.PrimarySchema != "" || ref.SecondarySchema != "" {
				mixedSet[id] = true
			}
		}
	}

	// 获取配置中的启用方案顺序
	cfg, _ := config.Load()
	enabledOrder := make(map[string]int)
	if cfg != nil {
		for i, id := range cfg.Schema.Available {
			enabledOrder[id] = i
		}
	}

	items := make([]SchemaStatusItem, len(reply.Schemas))
	for i, s := range reply.Schemas {
		name := nameMap[s.SchemaID]
		if name == "" {
			name = s.SchemaID
		}
		// 查询数据存储桶 ID（多个方案可能共享同一桶，如全拼/双拼共享 "pinyin" 桶）
		var dataSchemaID string
		if stats, err := a.rpcClient.DictGetSchemaStats(s.SchemaID); err == nil {
			dataSchemaID = stats.DataSchemaID
		}
		items[i] = SchemaStatusItem{
			SchemaID: s.SchemaID, SchemaName: name,
			EngineType:      engineTypeMap[s.SchemaID],
			IsMixed:         mixedSet[s.SchemaID],
			IsShuangpin:     shuangpinMap[s.SchemaID],
			ShuangpinLayout: shuangpinLayoutMap[s.SchemaID],
			DataSchemaID:    dataSchemaID,
			Status:          s.Status,
			UserWords:       s.UserWords, TempWords: s.TempWords,
			ShadowRules: s.ShadowRules, FreqRecords: s.FreqRecords,
		}
	}

	// 处理"被启用方案依赖的主方案"：
	// 双拼/混输/临时拼音都依赖全拼方案存储用户词库。用户可能未启用全拼方案
	// （如只用五笔+双拼）。此时全拼方案对应的数据桶（"pinyin"）实际上**仍被用着**，
	// 不应被当作"残留"。
	//
	// 处理两种状态：
	//   1) 主方案已在 reply.Schemas 中（因为桶有数据），但 status="orphaned"
	//      → 提升为 "enabled" 以允许在 UI 中编辑（不显示残留警告）
	//   2) 主方案完全不在 reply.Schemas 中（桶尚未创建）
	//      → 补一个虚拟条目，让用户能进入管理
	//
	// 识别"被依赖"的标准：任何 enabled 方案的 data_schema_id 指向它
	dependedIDs := make(map[string]bool)
	for _, it := range items {
		if it.Status != "enabled" || it.DataSchemaID == "" || it.DataSchemaID == it.SchemaID {
			continue
		}
		dependedIDs[it.DataSchemaID] = true
	}
	// "pinyin" 是拼音共享桶（PinyinSharedDictID），即使当前没有任何启用的
	// 拼音/双拼方案，五笔的临时拼音、混输的辅助拼音也可能使用其中的词库数据。
	// 因此始终视为"被依赖"，不显示为残留、始终允许编辑。
	dependedIDs[pinyinSharedDictID] = true

	existingIDs := make(map[string]bool, len(items))
	for i := range items {
		existingIDs[items[i].SchemaID] = true
		// 状态提升：被启用方案依赖的方案不应被视为残留
		if dependedIDs[items[i].SchemaID] && items[i].Status == "orphaned" {
			items[i].Status = "enabled"
		}
	}
	for depID := range dependedIDs {
		if existingIDs[depID] {
			continue
		}
		// 主方案完全不存在：补虚拟条目
		name := nameMap[depID]
		if name == "" {
			name = depID
		}
		stats, _ := a.rpcClient.DictGetSchemaStats(depID)
		entry := SchemaStatusItem{
			SchemaID:     depID,
			SchemaName:   name,
			EngineType:   engineTypeMap[depID],
			DataSchemaID: depID,
			Status:       "enabled",
		}
		if stats != nil {
			entry.UserWords = stats.WordCount
			entry.TempWords = stats.TempWordCount
			entry.ShadowRules = stats.ShadowCount
		}
		items = append(items, entry)
		existingIDs[depID] = true
	}

	// 排序：enabled(按配置顺序) → disabled → orphaned
	// enabled 组内：用户明确启用的按 enabledOrder 排，未在启用列表里的
	// 隐式方案（如未启用但被依赖的 "pinyin"）排到该组末尾
	sort.SliceStable(items, func(i, j int) bool {
		si, sj := items[i], items[j]
		ri := statusRank(si.Status)
		rj := statusRank(sj.Status)
		if ri != rj {
			return ri < rj
		}
		if si.Status == "enabled" {
			oi, oki := enabledOrder[si.SchemaID]
			oj, okj := enabledOrder[sj.SchemaID]
			// 一个在启用列表里、另一个不在 → 在的优先
			if oki != okj {
				return oki
			}
			if oki && okj {
				return oi < oj
			}
			// 都不在启用列表（如隐式 "pinyin" 加上其它边界 case）
			return si.SchemaID < sj.SchemaID
		}
		return si.SchemaID < sj.SchemaID
	})

	return items, nil
}

// statusRank 返回方案状态的排序权重
func statusRank(status string) int {
	switch status {
	case "enabled":
		return 0
	case "disabled":
		return 1
	default: // orphaned
		return 2
	}
}

// ========== 短语导入导出 ==========

// phraseYAMLEntry 简化 YAML 格式的短语条目
type phraseYAMLEntry struct {
	Code     string `yaml:"code"`
	Text     string `yaml:"text,omitempty"`
	Texts    string `yaml:"texts,omitempty"`
	Name     string `yaml:"name,omitempty"`
	Position int    `yaml:"position,omitempty"`
	Disabled bool   `yaml:"disabled,omitempty"`
}

type phraseYAMLFile struct {
	Phrases []phraseYAMLEntry `yaml:"phrases"`
}

// ImportPhrases 导入短语（简化 YAML 格式）
func (a *App) ImportPhrases() (*ImportExportResult, error) {
	path, err := wailsRuntime.OpenFileDialog(a.ctx, wailsRuntime.OpenDialogOptions{
		Title: "导入短语",
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "短语文件 (*.yaml, *.yml)", Pattern: "*.yaml;*.yml"},
			{DisplayName: "所有文件 (*.*)", Pattern: "*.*"},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开文件对话框失败: %w", err)
	}
	if path == "" {
		return &ImportExportResult{Cancelled: true}, nil
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("读取文件失败: %w", err)
	}

	var file phraseYAMLFile
	if err := yaml.Unmarshal(data, &file); err != nil {
		return nil, fmt.Errorf("解析 YAML 失败: %w", err)
	}

	count := 0
	for _, e := range file.Phrases {
		if e.Code == "" || (e.Text == "" && e.Texts == "") {
			continue
		}
		pType := "static"
		if e.Texts != "" {
			pType = "array"
		} else if strings.Contains(e.Text, "$") {
			pType = "dynamic"
		}
		pos := e.Position
		if pos <= 0 {
			pos = 1
		}
		if err := a.rpcClient.PhraseAdd(rpcapi.PhraseAddArgs{
			Code: e.Code, Text: e.Text, Texts: e.Texts, Name: e.Name,
			Type: pType, Position: pos,
		}); err == nil {
			count++
		}
	}

	return &ImportExportResult{Count: count, Total: len(file.Phrases)}, nil
}

// ExportPhrases 导出短语（简化 YAML 格式）
func (a *App) ExportPhrases() (*ImportExportResult, error) {
	defaultFilename := fmt.Sprintf("phrases_%s.yaml", time.Now().Format("20060102"))
	path, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "导出短语",
		DefaultFilename: defaultFilename,
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "短语文件 (*.yaml)", Pattern: "*.yaml"},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开保存对话框失败: %w", err)
	}
	if path == "" {
		return &ImportExportResult{Cancelled: true}, nil
	}

	reply, err := a.rpcClient.PhraseList()
	if err != nil {
		return nil, fmt.Errorf("获取短语列表失败: %w", err)
	}

	entries := make([]phraseYAMLEntry, 0, len(reply.Phrases))
	for _, p := range reply.Phrases {
		e := phraseYAMLEntry{
			Code:     p.Code,
			Text:     p.Text,
			Texts:    p.Texts,
			Name:     p.Name,
			Position: p.Position,
			Disabled: !p.Enabled,
		}
		entries = append(entries, e)
	}

	data, err := yaml.Marshal(phraseYAMLFile{Phrases: entries})
	if err != nil {
		return nil, fmt.Errorf("序列化失败: %w", err)
	}

	if err := os.WriteFile(path, data, 0644); err != nil {
		return nil, fmt.Errorf("写入文件失败: %w", err)
	}

	return &ImportExportResult{Count: len(entries), Path: path}, nil
}

// ========== 短语文件变化检测（已迁移到 RPC，保留空实现兼容前端）==========

// CheckPhrasesModified 检查短语是否被外部修改（RPC 模式下不再适用）
func (a *App) CheckPhrasesModified() (bool, error) {
	return false, nil
}

// ReloadPhrases 重新加载短语（RPC 模式下由服务端管理）
func (a *App) ReloadPhrases() error {
	return nil
}
