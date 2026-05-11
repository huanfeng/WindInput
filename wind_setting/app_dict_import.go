package main

import (
	"bytes"
	"fmt"
	"os"

	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"

	"github.com/huanfeng/wind_input/pkg/dictio"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// DictImportPreview 词库导入预览信息
type DictImportPreview struct {
	SchemaID   string         `json:"schema_id"`
	SchemaName string         `json:"schema_name"`
	Generator  string         `json:"generator"`
	ExportedAt string         `json:"exported_at"`
	Sections   map[string]int `json:"sections"`
	SourceFile string         `json:"source_file"`
}

// TextListPreviewResult 纯词语列表编码预览
type TextListPreviewResult struct {
	Total        int                       `json:"total"`
	SuccessCount int                       `json:"success_count"`
	FailCount    int                       `json:"fail_count"`
	Results      []rpcapi.EncodeResultItem `json:"results"`
}

// ZipImportPreview ZIP 导入预览
type ZipImportPreview struct {
	Schemas     []ZipSchemaPreviewItem `json:"schemas"`
	HasPhrases  bool                   `json:"has_phrases"`
	PhraseCount int                    `json:"phrase_count"`
}

// ZipSchemaPreviewItem ZIP 中单个方案的预览
type ZipSchemaPreviewItem struct {
	SchemaID   string         `json:"schema_id"`
	SchemaName string         `json:"schema_name"`
	Sections   map[string]int `json:"sections"`
}

// SelectImportFile 打开文件选择对话框
func (a *App) SelectImportFile(format string) (string, error) {
	filters := importFileFilters(format)
	path, err := wailsRuntime.OpenFileDialog(a.ctx, wailsRuntime.OpenDialogOptions{
		Title:   "选择导入文件",
		Filters: filters,
	})
	if err != nil {
		return "", fmt.Errorf("打开文件对话框失败: %w", err)
	}
	return path, nil
}

// PreviewImportFile 预览导入文件内容（不写入数据）
func (a *App) PreviewImportFile(format, filePath string) (*DictImportPreview, error) {
	data, err := os.ReadFile(filePath)
	if err != nil {
		return nil, fmt.Errorf("读取文件失败: %w", err)
	}

	switch format {
	case "winddict":
		header, counts, err := dictio.PreviewWindDict(data)
		if err != nil {
			return nil, err
		}
		return &DictImportPreview{
			SchemaID:   header.SchemaID,
			SchemaName: header.SchemaName,
			Generator:  header.Generator,
			ExportedAt: header.ExportedAt,
			Sections:   counts,
			SourceFile: filePath,
		}, nil

	case "tsv":
		result, err := (&dictio.TSVImporter{}).Import(bytes.NewReader(data), dictio.ImportOptions{})
		if err != nil {
			return nil, err
		}
		return &DictImportPreview{
			Sections:   map[string]int{dictio.SectionUserWords: result.Stats.UserWordsCount},
			SourceFile: filePath,
		}, nil

	case "rime":
		result, err := (&dictio.RimeDictImporter{}).Import(bytes.NewReader(data), dictio.ImportOptions{})
		if err != nil {
			return nil, err
		}
		return &DictImportPreview{
			Sections:   map[string]int{dictio.SectionUserWords: result.Stats.UserWordsCount},
			SourceFile: filePath,
		}, nil

	case "phrase_yaml":
		result, err := (&dictio.PhraseYAMLImporter{}).Import(bytes.NewReader(data), dictio.ImportOptions{})
		if err != nil {
			return nil, err
		}
		return &DictImportPreview{
			Sections:   map[string]int{dictio.SectionPhrases: result.Stats.PhraseCount},
			SourceFile: filePath,
		}, nil

	case "textlist":
		result, err := (&dictio.TextListImporter{}).Import(bytes.NewReader(data), dictio.ImportOptions{})
		if err != nil {
			return nil, err
		}
		return &DictImportPreview{
			Sections:   map[string]int{dictio.SectionUserWords: result.Stats.UserWordsCount},
			SourceFile: filePath,
		}, nil

	default:
		return nil, fmt.Errorf("不支持的导入格式: %s", format)
	}
}

// PreviewZipImport 预览 ZIP 备份包内容
func (a *App) PreviewZipImport(filePath string) (*ZipImportPreview, error) {
	f, err := os.Open(filePath)
	if err != nil {
		return nil, fmt.Errorf("打开文件失败: %w", err)
	}
	defer f.Close()

	fi, err := f.Stat()
	if err != nil {
		return nil, err
	}

	manifest, schemaCounts, err := dictio.PreviewZip(f, fi.Size())
	if err != nil {
		return nil, err
	}

	preview := &ZipImportPreview{}

	for _, s := range manifest.Schemas {
		item := ZipSchemaPreviewItem{
			SchemaID:   s.ID,
			SchemaName: s.Name,
			Sections:   schemaCounts[s.ID],
		}
		preview.Schemas = append(preview.Schemas, item)
	}

	if phraseCounts, ok := schemaCounts["_phrases"]; ok {
		preview.HasPhrases = true
		preview.PhraseCount = phraseCounts[dictio.SectionPhrases]
	}

	return preview, nil
}

// PreviewTextList 预览纯词语列表（调用 BatchEncode）
func (a *App) PreviewTextList(filePath, schemaID string) (*TextListPreviewResult, error) {
	data, err := os.ReadFile(filePath)
	if err != nil {
		return nil, fmt.Errorf("读取文件失败: %w", err)
	}

	result, err := (&dictio.TextListImporter{}).Import(bytes.NewReader(data), dictio.ImportOptions{})
	if err != nil {
		return nil, err
	}

	words := make([]string, len(result.UserWords))
	for i, w := range result.UserWords {
		words[i] = w.Text
	}

	reply, err := a.rpcClient.DictBatchEncode(schemaID, words)
	if err != nil {
		return nil, fmt.Errorf("编码失败: %w", err)
	}

	preview := &TextListPreviewResult{
		Total:   len(reply.Results),
		Results: reply.Results,
	}
	for _, r := range reply.Results {
		if r.Status == "ok" {
			preview.SuccessCount++
		} else {
			preview.FailCount++
		}
	}

	return preview, nil
}

// ExecuteImport 执行导入操作
func (a *App) ExecuteImport(filePath, format, schemaID string, sections []string, strategies map[string]string) (*ImportExportResult, error) {
	data, err := os.ReadFile(filePath)
	if err != nil {
		return nil, fmt.Errorf("读取文件失败: %w", err)
	}

	importer := getImporter(format)
	if importer == nil {
		return nil, fmt.Errorf("不支持的导入格式: %s", format)
	}

	opts := dictio.ImportOptions{SchemaID: schemaID, Sections: sections}
	result, err := importer.Import(bytes.NewReader(data), opts)
	if err != nil {
		return nil, fmt.Errorf("解析文件失败: %w", err)
	}

	// 纯词语列表：需要调用 BatchEncode 生成编码
	if format == "textlist" && len(result.UserWords) > 0 {
		words := make([]string, len(result.UserWords))
		for i, w := range result.UserWords {
			words[i] = w.Text
		}
		encReply, err := a.rpcClient.DictBatchEncode(schemaID, words)
		if err != nil {
			return nil, fmt.Errorf("编码生成失败: %w", err)
		}
		// 只保留编码成功的词条
		var encoded []dictio.UserWordEntry
		for _, r := range encReply.Results {
			if r.Status == "ok" && r.Code != "" {
				encoded = append(encoded, dictio.UserWordEntry{
					Code:   r.Code,
					Text:   r.Word,
					Weight: 100, // 默认权重
				})
			}
		}
		result.UserWords = encoded
		result.UpdateStats()
	}

	count, err := a.writeImportResult(schemaID, result, strategies)
	if err != nil {
		return nil, err
	}

	return &ImportExportResult{Count: count}, nil
}

// ExecuteTextListImport 执行纯词语列表导入（编码已确定）
func (a *App) ExecuteTextListImport(schemaID string, words []rpcapi.EncodeResultItem, weight int) (*ImportExportResult, error) {
	var entries []rpcapi.WordEntry
	for _, w := range words {
		if w.Status != "ok" || w.Code == "" {
			continue
		}
		entries = append(entries, rpcapi.WordEntry{
			Code: w.Code, Text: w.Word, Weight: weight,
		})
	}

	if len(entries) == 0 {
		return &ImportExportResult{Count: 0}, nil
	}

	count, err := a.dictBatchAddChunked(schemaID, entries)
	if err != nil {
		return nil, fmt.Errorf("写入失败: %w", err)
	}

	return &ImportExportResult{Count: count}, nil
}

// ExecuteZipImport 执行 ZIP 备份包导入
func (a *App) ExecuteZipImport(filePath string, selectedSchemas []string, includePhrases bool, strategies map[string]string) (*ImportExportResult, error) {
	f, err := os.Open(filePath)
	if err != nil {
		return nil, fmt.Errorf("打开文件失败: %w", err)
	}
	defer f.Close()

	fi, err := f.Stat()
	if err != nil {
		return nil, err
	}

	zipResult, err := dictio.ImportZip(f, fi.Size(), dictio.ImportOptions{})
	if err != nil {
		return nil, err
	}

	totalCount := 0

	// 导入各方案数据
	selectedSet := make(map[string]bool)
	for _, s := range selectedSchemas {
		selectedSet[s] = true
	}

	for sid, result := range zipResult.Schemas {
		if !selectedSet[sid] {
			continue
		}
		count, err := a.writeImportResult(sid, result, strategies)
		if err != nil {
			return nil, fmt.Errorf("导入方案 %s 失败: %w", sid, err)
		}
		totalCount += count
	}

	// 导入短语
	if includePhrases && zipResult.Phrases != nil && len(zipResult.Phrases.Phrases) > 0 {
		count, err := a.writePhrases(zipResult.Phrases.Phrases)
		if err != nil {
			return nil, fmt.Errorf("导入短语失败: %w", err)
		}
		totalCount += count
	}

	return &ImportExportResult{Count: totalCount}, nil
}

const dictBatchChunkSize = 5000

// dictBatchAddChunked 将 entries 分片后逐批发送，避免单次 RPC 超时
func (a *App) dictBatchAddChunked(schemaID string, entries []rpcapi.WordEntry) (int, error) {
	total := 0
	for i := 0; i < len(entries); i += dictBatchChunkSize {
		end := i + dictBatchChunkSize
		if end > len(entries) {
			end = len(entries)
		}
		n, err := a.rpcClient.DictBatchAdd(schemaID, entries[i:end])
		if err != nil {
			return total, err
		}
		total += n
	}
	return total, nil
}

// writeImportResult 将 ImportResult 写入 Store（通过 RPC）
func (a *App) writeImportResult(schemaID string, result *dictio.ImportResult, strategies map[string]string) (int, error) {
	count := 0

	// 写入用户词库
	if len(result.UserWords) > 0 {
		entries := make([]rpcapi.WordEntry, len(result.UserWords))
		for i, w := range result.UserWords {
			entries[i] = rpcapi.WordEntry{
				Code: w.Code, Text: w.Text, Weight: w.Weight,
				Count: w.Count, CreatedAt: w.CreatedAt,
			}
		}
		n, err := a.dictBatchAddChunked(schemaID, entries)
		if err != nil {
			return count, fmt.Errorf("写入用户词库失败: %w", err)
		}
		count += n
	}

	// 写入临时词库（复用 BatchAdd，写入用户词库——导入场景不区分临时/正式）
	if len(result.TempWords) > 0 {
		entries := make([]rpcapi.WordEntry, len(result.TempWords))
		for i, w := range result.TempWords {
			entries[i] = rpcapi.WordEntry{
				Code: w.Code, Text: w.Text, Weight: w.Weight,
				Count: w.Count, CreatedAt: w.CreatedAt,
			}
		}
		n, err := a.dictBatchAddChunked(schemaID, entries)
		if err != nil {
			return count, fmt.Errorf("写入临时词库失败: %w", err)
		}
		count += n
	}

	// 写入词频
	if len(result.FreqData) > 0 {
		entries := make([]rpcapi.FreqPutEntry, len(result.FreqData))
		for i, f := range result.FreqData {
			entries[i] = rpcapi.FreqPutEntry{
				Code: f.Code, Text: f.Text,
				Count: f.Count, LastUsed: f.LastUsed, Streak: f.Streak,
			}
		}
		reply, err := a.rpcClient.FreqBatchPut(schemaID, entries)
		if err != nil {
			return count, fmt.Errorf("写入词频失败: %w", err)
		}
		count += reply.Count
	}

	// 写入 Shadow
	if len(result.ShadowPins) > 0 || len(result.ShadowDels) > 0 {
		pins := make([]rpcapi.ShadowPinItem, len(result.ShadowPins))
		for i, p := range result.ShadowPins {
			pins[i] = rpcapi.ShadowPinItem{Code: p.Code, Word: p.Word, Position: p.Position}
		}
		dels := make([]rpcapi.ShadowDelItem, len(result.ShadowDels))
		for i, d := range result.ShadowDels {
			dels[i] = rpcapi.ShadowDelItem{Code: d.Code, Word: d.Word}
		}
		reply, err := a.rpcClient.ShadowBatchSet(schemaID, pins, dels)
		if err != nil {
			return count, fmt.Errorf("写入 Shadow 失败: %w", err)
		}
		count += reply.PinCount + reply.DelCount
	}

	// 写入短语
	if len(result.Phrases) > 0 {
		n, err := a.writePhrases(result.Phrases)
		if err != nil {
			return count, err
		}
		count += n
	}

	return count, nil
}

// writePhrases 批量写入短语
func (a *App) writePhrases(phrases []dictio.PhraseEntry) (int, error) {
	args := make([]rpcapi.PhraseAddArgs, len(phrases))
	for i, p := range phrases {
		text := p.Text
		texts := ""
		if p.Type == "array" {
			texts = p.Text
			text = ""
		}
		args[i] = rpcapi.PhraseAddArgs{
			Code: p.Code, Text: text, Texts: texts,
			Name: p.Name, Type: p.Type, Position: p.Position,
		}
	}

	reply, err := a.rpcClient.PhraseBatchAdd(args)
	if err != nil {
		return 0, fmt.Errorf("写入短语失败: %w", err)
	}
	return reply.Count, nil
}

// getImporter 根据格式名返回导入器
func getImporter(format string) dictio.Importer {
	switch format {
	case "winddict":
		return &dictio.WindDictImporter{}
	case "tsv":
		return &dictio.TSVImporter{}
	case "rime":
		return &dictio.RimeDictImporter{}
	case "phrase_yaml":
		return &dictio.PhraseYAMLImporter{}
	case "textlist":
		return &dictio.TextListImporter{}
	default:
		return nil
	}
}

// importFileFilters 根据格式返回文件过滤器
func importFileFilters(format string) []wailsRuntime.FileFilter {
	switch format {
	case "winddict":
		return []wailsRuntime.FileFilter{
			{DisplayName: "WindDict 文件 (*.wdict.yaml)", Pattern: "*.wdict.yaml"},
		}
	case "tsv":
		return []wailsRuntime.FileFilter{
			{DisplayName: "文本文件 (*.txt)", Pattern: "*.txt"},
		}
	case "rime":
		return []wailsRuntime.FileFilter{
			{DisplayName: "Rime 词库 (*.dict.yaml)", Pattern: "*.dict.yaml"},
		}
	case "phrase_yaml":
		return []wailsRuntime.FileFilter{
			{DisplayName: "YAML 文件 (*.yaml, *.yml)", Pattern: "*.yaml;*.yml"},
		}
	case "textlist":
		return []wailsRuntime.FileFilter{
			{DisplayName: "文本文件 (*.txt)", Pattern: "*.txt"},
		}
	case "zip":
		return []wailsRuntime.FileFilter{
			{DisplayName: "ZIP 备份 (*.zip)", Pattern: "*.zip"},
		}
	default:
		return []wailsRuntime.FileFilter{
			{DisplayName: "所有文件 (*.*)", Pattern: "*.*"},
		}
	}
}
