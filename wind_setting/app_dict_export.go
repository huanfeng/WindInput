package main

import (
	"bytes"
	"fmt"
	"os"
	"time"

	wailsRuntime "github.com/wailsapp/wails/v2/pkg/runtime"

	"github.com/huanfeng/wind_input/pkg/dictio"
)

// ExportSchemaData 导出指定方案的数据为 .wdict.yaml 文件
func (a *App) ExportSchemaData(schemaID string, sections []string, schemaName string) (*ImportExportResult, error) {
	defaultFilename := fmt.Sprintf("%s_%s.wdict.yaml", schemaID, time.Now().Format("20060102"))
	path, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "导出词库数据",
		DefaultFilename: defaultFilename,
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "WindDict 文件 (*.wdict.yaml)", Pattern: "*.wdict.yaml"},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开保存对话框失败: %w", err)
	}
	if path == "" {
		return &ImportExportResult{Cancelled: true}, nil
	}

	data, err := a.collectExportData(schemaID, sections)
	if err != nil {
		return nil, err
	}

	f, err := os.Create(path)
	if err != nil {
		return nil, fmt.Errorf("创建文件失败: %w", err)
	}
	defer f.Close()

	exporter := &dictio.WindDictExporter{}
	opts := dictio.ExportOptions{
		SchemaID:   schemaID,
		SchemaName: schemaName,
		Sections:   sections,
		Generator:  "WindInput",
	}
	if err := exporter.Export(f, data, opts); err != nil {
		return nil, fmt.Errorf("导出失败: %w", err)
	}

	total := len(data.UserWords) + len(data.TempWords) + len(data.FreqData) + len(data.Phrases)
	for _, rec := range data.Shadow {
		total += len(rec.Pinned) + len(rec.Deleted)
	}

	return &ImportExportResult{Count: total, Path: path}, nil
}

// ExportPhrasesFile 导出全局短语为独立文件
func (a *App) ExportPhrasesFile(format string) (*ImportExportResult, error) {
	ext := ".wdict.yaml"
	filterName := "WindDict 文件 (*.wdict.yaml)"
	filterPattern := "*.wdict.yaml"
	if format == "yaml" {
		ext = ".yaml"
		filterName = "YAML 文件 (*.yaml)"
		filterPattern = "*.yaml"
	}

	defaultFilename := fmt.Sprintf("phrases_%s%s", time.Now().Format("20060102"), ext)
	path, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "导出短语",
		DefaultFilename: defaultFilename,
		Filters:         []wailsRuntime.FileFilter{{DisplayName: filterName, Pattern: filterPattern}},
	})
	if err != nil {
		return nil, fmt.Errorf("打开保存对话框失败: %w", err)
	}
	if path == "" {
		return &ImportExportResult{Cancelled: true}, nil
	}

	phrases, err := a.collectPhrases()
	if err != nil {
		return nil, err
	}

	if format == "yaml" {
		// 使用旧版 YAML 格式导出（复用现有逻辑）
		return a.exportPhrasesYAML(path, phrases)
	}

	// WindDict 格式
	f, err := os.Create(path)
	if err != nil {
		return nil, fmt.Errorf("创建文件失败: %w", err)
	}
	defer f.Close()

	data := &dictio.ExportData{Phrases: phrases}
	exporter := &dictio.WindDictExporter{}
	opts := dictio.ExportOptions{Sections: []string{dictio.SectionPhrases}, Generator: "WindInput"}
	if err := exporter.Export(f, data, opts); err != nil {
		return nil, fmt.Errorf("导出失败: %w", err)
	}

	return &ImportExportResult{Count: len(phrases), Path: path}, nil
}

// ExportFullBackup 全量备份为 ZIP
func (a *App) ExportFullBackup(schemaIDs []string, schemaNames map[string]string, includePhrases bool) (*ImportExportResult, error) {
	defaultFilename := fmt.Sprintf("wind_backup_%s.zip", time.Now().Format("20060102"))
	path, err := wailsRuntime.SaveFileDialog(a.ctx, wailsRuntime.SaveDialogOptions{
		Title:           "导出完整备份",
		DefaultFilename: defaultFilename,
		Filters: []wailsRuntime.FileFilter{
			{DisplayName: "ZIP 压缩包 (*.zip)", Pattern: "*.zip"},
		},
	})
	if err != nil {
		return nil, fmt.Errorf("打开保存对话框失败: %w", err)
	}
	if path == "" {
		return &ImportExportResult{Cancelled: true}, nil
	}

	var schemas []dictio.SchemaExportData
	for _, sid := range schemaIDs {
		data, err := a.collectExportData(sid, nil)
		if err != nil {
			return nil, fmt.Errorf("收集方案 %s 数据失败: %w", sid, err)
		}
		name := schemaNames[sid]
		if name == "" {
			name = sid
		}
		schemas = append(schemas, dictio.SchemaExportData{
			SchemaID:   sid,
			SchemaName: name,
			Data:       data,
		})
	}

	var phrases []dictio.PhraseEntry
	if includePhrases {
		phrases, err = a.collectPhrases()
		if err != nil {
			return nil, err
		}
	}

	f, err := os.Create(path)
	if err != nil {
		return nil, fmt.Errorf("创建文件失败: %w", err)
	}
	defer f.Close()

	opts := dictio.ZipExportOptions{Generator: "WindInput"}
	if err := dictio.ExportZip(f, schemas, phrases, opts); err != nil {
		return nil, fmt.Errorf("导出 ZIP 失败: %w", err)
	}

	total := 0
	for _, s := range schemas {
		total += len(s.Data.UserWords) + len(s.Data.TempWords) + len(s.Data.FreqData)
	}
	total += len(phrases)

	return &ImportExportResult{Count: total, Path: path}, nil
}

// collectExportData 通过 RPC 收集指定方案的导出数据
func (a *App) collectExportData(schemaID string, sections []string) (*dictio.ExportData, error) {
	opts := dictio.ExportOptions{Sections: sections}
	data := &dictio.ExportData{}

	// 用户词库（limit=-1 表示全部返回，不分页）
	if opts.ShouldExport(dictio.SectionUserWords) {
		reply, err := a.rpcClient.DictSearch(schemaID, "", "", -1, 0)
		if err == nil {
			for _, w := range reply.Words {
				data.UserWords = append(data.UserWords, dictio.UserWordEntry{
					Code: w.Code, Text: w.Text, Weight: w.Weight,
					Count: w.Count, CreatedAt: w.CreatedAt,
				})
			}
		}
	}

	// 临时词库（limit=-1 表示全部返回）
	if opts.ShouldExport(dictio.SectionTempWords) {
		reply, err := a.rpcClient.DictGetTemp(schemaID, "", -1, 0)
		if err == nil {
			for _, w := range reply.Words {
				data.TempWords = append(data.TempWords, dictio.UserWordEntry{
					Code: w.Code, Text: w.Text, Weight: w.Weight, Count: w.Count,
				})
			}
		}
	}

	// 词频
	if opts.ShouldExport(dictio.SectionFreq) {
		reply, err := a.rpcClient.FreqSearch(schemaID, "", 0, 0)
		if err == nil {
			for _, e := range reply.Entries {
				data.FreqData = append(data.FreqData, dictio.FreqEntry{
					Code: e.Code, Text: e.Text,
					Count: uint32(e.Count), LastUsed: e.LastUsed,
					Streak: uint8(e.Streak),
				})
			}
		}
	}

	// Shadow
	if opts.ShouldExport(dictio.SectionShadow) {
		reply, err := a.rpcClient.ShadowGetAllRules(schemaID)
		if err == nil {
			data.Shadow = make(map[string]dictio.ShadowRecord)
			for _, cr := range reply.Rules {
				rec := dictio.ShadowRecord{Deleted: cr.Deleted}
				for _, p := range cr.Pinned {
					rec.Pinned = append(rec.Pinned, dictio.ShadowPinEntry{
						Code: cr.Code, Word: p.Word, Position: p.Position,
					})
				}
				data.Shadow[cr.Code] = rec
			}
		}
	}

	return data, nil
}

// collectPhrases 通过 RPC 收集全局短语
func (a *App) collectPhrases() ([]dictio.PhraseEntry, error) {
	reply, err := a.rpcClient.PhraseList()
	if err != nil {
		return nil, fmt.Errorf("获取短语列表失败: %w", err)
	}

	var phrases []dictio.PhraseEntry
	for _, p := range reply.Phrases {
		text := p.Text
		if p.Texts != "" {
			text = p.Texts
		}
		phrases = append(phrases, dictio.PhraseEntry{
			Code: p.Code, Type: p.Type, Text: text,
			Position: p.Position, Enabled: p.Enabled, Name: p.Name,
		})
	}
	return phrases, nil
}

// exportPhrasesYAML 使用旧版 YAML 格式导出短语
func (a *App) exportPhrasesYAML(path string, phrases []dictio.PhraseEntry) (*ImportExportResult, error) {
	// 转换为旧格式并调用现有导出逻辑
	var buf bytes.Buffer
	buf.WriteString("phrases:\n")
	for _, p := range phrases {
		buf.WriteString(fmt.Sprintf("  - code: %q\n", p.Code))
		if p.Type == "array" {
			buf.WriteString(fmt.Sprintf("    texts: %q\n", p.Text))
			if p.Name != "" {
				buf.WriteString(fmt.Sprintf("    name: %q\n", p.Name))
			}
		} else {
			buf.WriteString(fmt.Sprintf("    text: %q\n", p.Text))
		}
		buf.WriteString(fmt.Sprintf("    position: %d\n", p.Position))
		if !p.Enabled {
			buf.WriteString("    disabled: true\n")
		}
	}

	if err := os.WriteFile(path, buf.Bytes(), 0644); err != nil {
		return nil, fmt.Errorf("写入文件失败: %w", err)
	}

	return &ImportExportResult{Count: len(phrases), Path: path}, nil
}
