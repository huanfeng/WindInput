package dictio

import (
	"bufio"
	"fmt"
	"io"
	"strconv"
	"time"
)

// WindDictExporter 将数据导出为 .wdict.yaml 格式。
type WindDictExporter struct{}

func (e *WindDictExporter) Name() string      { return "WindDict" }
func (e *WindDictExporter) Extension() string { return ".wdict.yaml" }

// Export 将 ExportData 写入 writer。
func (e *WindDictExporter) Export(w io.Writer, data *ExportData, opts ExportOptions) error {
	bw := bufio.NewWriter(w)

	// 1. 构建 header
	header := e.buildHeader(data, opts)

	// 2. 写入 YAML 头部
	if err := WriteHeader(bw, header); err != nil {
		return err
	}

	// 3. 按 section 顺序写入数据段
	if meta, ok := header.Sections[SectionUserWords]; ok {
		if err := e.writeUserWords(bw, SectionUserWords, data.UserWords, NewColumnDef(meta.Columns)); err != nil {
			return err
		}
	}

	if meta, ok := header.Sections[SectionTempWords]; ok {
		if err := e.writeUserWords(bw, SectionTempWords, data.TempWords, NewColumnDef(meta.Columns)); err != nil {
			return err
		}
	}

	if meta, ok := header.Sections[SectionFreq]; ok {
		if err := e.writeFreq(bw, data.FreqData, NewColumnDef(meta.Columns)); err != nil {
			return err
		}
	}

	if meta, ok := header.Sections[SectionShadow]; ok {
		if err := e.writeShadow(bw, data.Shadow, NewColumnDef(meta.Columns)); err != nil {
			return err
		}
	}

	if meta, ok := header.Sections[SectionPhrases]; ok {
		if err := e.writePhrases(bw, data.Phrases, NewColumnDef(meta.Columns)); err != nil {
			return err
		}
	}

	return bw.Flush()
}

// buildHeader 根据导出数据和选项构建 YAML 头部。
func (e *WindDictExporter) buildHeader(data *ExportData, opts ExportOptions) *WindDictHeader {
	generator := opts.Generator
	if generator == "" {
		generator = "WindInput"
	}

	header := &WindDictHeader{
		Version:    FormatVersion,
		Generator:  generator,
		ExportedAt: time.Now().Format(time.RFC3339),
		SchemaID:   opts.SchemaID,
		SchemaName: opts.SchemaName,
		Sections:   make(map[string]SectionMeta),
	}

	// 只为有数据且在导出范围内的 section 添加列定义
	if len(data.UserWords) > 0 && opts.ShouldExport(SectionUserWords) {
		header.Sections[SectionUserWords] = SectionMeta{Columns: DefaultColumns[SectionUserWords]}
	}
	if len(data.TempWords) > 0 && opts.ShouldExport(SectionTempWords) {
		header.Sections[SectionTempWords] = SectionMeta{Columns: DefaultColumns[SectionTempWords]}
	}
	if len(data.FreqData) > 0 && opts.ShouldExport(SectionFreq) {
		header.Sections[SectionFreq] = SectionMeta{Columns: DefaultColumns[SectionFreq]}
	}
	if len(data.Shadow) > 0 && opts.ShouldExport(SectionShadow) {
		header.Sections[SectionShadow] = SectionMeta{Columns: DefaultColumns[SectionShadow]}
	}
	if len(data.Phrases) > 0 && opts.ShouldExport(SectionPhrases) {
		header.Sections[SectionPhrases] = SectionMeta{Columns: DefaultColumns[SectionPhrases]}
	}

	return header
}

// writeSectionHeader 写入 section 分隔符。
func writeSectionHeader(w *bufio.Writer, tag string) error {
	_, err := fmt.Fprintf(w, "\n--- !%s\n", tag)
	return err
}

// writeUserWords 写入用户词库或临时词库段。
func (e *WindDictExporter) writeUserWords(w *bufio.Writer, tag string, words []UserWordEntry, cols *ColumnDef) error {
	if err := writeSectionHeader(w, tag); err != nil {
		return err
	}
	for _, entry := range words {
		if err := writeWordLine(w, entry, cols); err != nil {
			return err
		}
	}
	return nil
}

// writeWordLine 按列定义写入单条词库记录。
func writeWordLine(w *bufio.Writer, entry UserWordEntry, cols *ColumnDef) error {
	for i, name := range cols.Names {
		if i > 0 {
			_ = w.WriteByte('\t')
		}
		switch name {
		case "code":
			_, _ = w.WriteString(EscapeField(entry.Code))
		case "text":
			_, _ = w.WriteString(EscapeField(entry.Text))
		case "weight":
			_, _ = w.WriteString(strconv.Itoa(entry.Weight))
		case "count":
			_, _ = w.WriteString(strconv.Itoa(entry.Count))
		case "created_at":
			_, _ = w.WriteString(strconv.FormatInt(entry.CreatedAt, 10))
		default:
			// 未知列写空
		}
	}
	return w.WriteByte('\n')
}

// writeFreq 写入词频数据段。
func (e *WindDictExporter) writeFreq(w *bufio.Writer, entries []FreqEntry, cols *ColumnDef) error {
	if err := writeSectionHeader(w, SectionFreq); err != nil {
		return err
	}
	for _, entry := range entries {
		for i, name := range cols.Names {
			if i > 0 {
				_ = w.WriteByte('\t')
			}
			switch name {
			case "code":
				_, _ = w.WriteString(EscapeField(entry.Code))
			case "text":
				_, _ = w.WriteString(EscapeField(entry.Text))
			case "count":
				_, _ = w.WriteString(strconv.FormatUint(uint64(entry.Count), 10))
			case "last_used":
				_, _ = w.WriteString(strconv.FormatInt(entry.LastUsed, 10))
			case "streak":
				_, _ = w.WriteString(strconv.FormatUint(uint64(entry.Streak), 10))
			default:
			}
		}
		_ = w.WriteByte('\n')
	}
	return nil
}

// writeShadow 写入候选调整段。
func (e *WindDictExporter) writeShadow(w *bufio.Writer, shadow map[string]ShadowRecord, cols *ColumnDef) error {
	if err := writeSectionHeader(w, SectionShadow); err != nil {
		return err
	}
	for code, rec := range shadow {
		for _, pin := range rec.Pinned {
			if err := writeShadowLine(w, "pin", code, pin.Word, pin.Position, cols); err != nil {
				return err
			}
		}
		for _, word := range rec.Deleted {
			if err := writeShadowLine(w, "del", code, word, 0, cols); err != nil {
				return err
			}
		}
	}
	return nil
}

// writeShadowLine 按列定义写入单条 Shadow 规则。
func writeShadowLine(w *bufio.Writer, action, code, word string, position int, cols *ColumnDef) error {
	for i, name := range cols.Names {
		if i > 0 {
			_ = w.WriteByte('\t')
		}
		switch name {
		case "action":
			_, _ = w.WriteString(action)
		case "code":
			_, _ = w.WriteString(EscapeField(code))
		case "word":
			_, _ = w.WriteString(EscapeField(word))
		case "position":
			if action == "pin" {
				_, _ = w.WriteString(strconv.Itoa(position))
			}
			// del 类型不写 position
		default:
		}
	}
	return w.WriteByte('\n')
}

// writePhrases 写入短语段。
func (e *WindDictExporter) writePhrases(w *bufio.Writer, phrases []PhraseEntry, cols *ColumnDef) error {
	if err := writeSectionHeader(w, SectionPhrases); err != nil {
		return err
	}
	for _, entry := range phrases {
		for i, name := range cols.Names {
			if i > 0 {
				_ = w.WriteByte('\t')
			}
			switch name {
			case "code":
				_, _ = w.WriteString(EscapeField(entry.Code))
			case "text":
				_, _ = w.WriteString(EscapeField(entry.Text))
			case "weight":
				_, _ = w.WriteString(strconv.Itoa(entry.Weight))
			case "position":
				_, _ = w.WriteString(strconv.Itoa(entry.Position))
			case "enabled":
				_, _ = w.WriteString(FormatBool(entry.Enabled))
			default:
			}
		}
		_ = w.WriteByte('\n')
	}
	return nil
}
