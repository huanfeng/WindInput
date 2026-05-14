package dictio

import (
	"bufio"
	"bytes"
	"fmt"
	"io"
	"strings"
)

// WindDictImporter 解析 .wdict.yaml 格式文件。
type WindDictImporter struct{}

func (p *WindDictImporter) Name() string         { return "WindDict" }
func (p *WindDictImporter) Extensions() []string { return []string{".wdict.yaml"} }

// Import 从 reader 中解析 WindDict 格式数据。
func (p *WindDictImporter) Import(r io.Reader, opts ImportOptions) (*ImportResult, error) {
	data, err := io.ReadAll(r)
	if err != nil {
		return nil, fmt.Errorf("读取数据失败: %w", err)
	}

	// 解析头部
	header, err := ParseHeader(data)
	if err != nil {
		return nil, err
	}

	// 分割各 section
	_, sections := SplitSections(data)

	result := &ImportResult{}

	for _, sec := range sections {
		if !opts.ShouldImport(sec.Tag) {
			continue
		}

		// 获取列定义：优先使用头部声明，其次使用默认值
		var columns []string
		if meta, ok := header.Sections[sec.Tag]; ok && len(meta.Columns) > 0 {
			columns = meta.Columns
		} else if def, ok := DefaultColumns[sec.Tag]; ok {
			columns = def
		} else {
			result.Warnings = append(result.Warnings, fmt.Sprintf("未知 section %q，已跳过", sec.Tag))
			continue
		}

		cols := NewColumnDef(columns)

		switch sec.Tag {
		case SectionUserWords:
			entries, warns := parseWordEntries(sec.Body, cols, sec.Tag)
			result.UserWords = entries
			result.Warnings = append(result.Warnings, warns...)
		case SectionTempWords:
			entries, warns := parseWordEntries(sec.Body, cols, sec.Tag)
			result.TempWords = entries
			result.Warnings = append(result.Warnings, warns...)
		case SectionFreq:
			entries, warns := parseFreqEntries(sec.Body, cols)
			result.FreqData = entries
			result.Warnings = append(result.Warnings, warns...)
		case SectionShadow:
			pins, dels, warns := parseShadowEntries(sec.Body, cols)
			result.ShadowPins = pins
			result.ShadowDels = dels
			result.Warnings = append(result.Warnings, warns...)
		case SectionPhrases:
			entries, warns := parsePhraseEntries(sec.Body, cols)
			result.Phrases = entries
			result.Warnings = append(result.Warnings, warns...)
		default:
			result.Warnings = append(result.Warnings, fmt.Sprintf("未知 section %q，已跳过", sec.Tag))
		}
	}

	result.UpdateStats()
	return result, nil
}

// PreviewWindDict 解析文件头部和统计信息（不加载完整数据）。
func PreviewWindDict(data []byte) (*WindDictHeader, map[string]int, error) {
	header, err := ParseHeader(data)
	if err != nil {
		return nil, nil, err
	}

	_, sections := SplitSections(data)
	counts := make(map[string]int)
	for _, sec := range sections {
		count := countDataLines(sec.Body)
		counts[sec.Tag] = count
	}

	return header, counts, nil
}

// countDataLines 统计数据行数（跳过空行和注释）。
func countDataLines(body []byte) int {
	count := 0
	scanner := bufio.NewScanner(bytes.NewReader(body))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		count++
	}
	return count
}

// parseWordEntries 解析词库数据行。
func parseWordEntries(body []byte, cols *ColumnDef, tag string) ([]UserWordEntry, []string) {
	var entries []UserWordEntry
	var warnings []string
	lineNum := 0

	scanner := bufio.NewScanner(bytes.NewReader(body))
	for scanner.Scan() {
		lineNum++
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}

		fields := strings.Split(line, "\t")
		code := cols.GetUnescaped(fields, "code")
		text := cols.GetUnescaped(fields, "text")

		if code == "" || text == "" {
			warnings = append(warnings, fmt.Sprintf("[%s] 第 %d 行: 缺少编码或文本，已跳过", tag, lineNum))
			continue
		}

		if !isValidCode(code) {
			warnings = append(warnings, fmt.Sprintf("[%s] 第 %d 行: 编码含无效字符 %q，已跳过", tag, lineNum, code))
			continue
		}

		entry := UserWordEntry{
			Code:      code,
			Text:      text,
			Weight:    cols.GetInt(fields, "weight", 0),
			Count:     cols.GetInt(fields, "count", 0),
			CreatedAt: cols.GetInt64(fields, "created_at", 0),
		}
		entries = append(entries, entry)
	}

	return entries, warnings
}

// parseFreqEntries 解析词频数据行。
func parseFreqEntries(body []byte, cols *ColumnDef) ([]FreqEntry, []string) {
	var entries []FreqEntry
	var warnings []string
	lineNum := 0

	scanner := bufio.NewScanner(bytes.NewReader(body))
	for scanner.Scan() {
		lineNum++
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}

		fields := strings.Split(line, "\t")
		code := cols.GetUnescaped(fields, "code")
		text := cols.GetUnescaped(fields, "text")

		if code == "" || text == "" {
			warnings = append(warnings, fmt.Sprintf("[freq] 第 %d 行: 缺少编码或文本，已跳过", lineNum))
			continue
		}

		entry := FreqEntry{
			Code:     code,
			Text:     text,
			Count:    cols.GetUint32(fields, "count", 0),
			LastUsed: cols.GetInt64(fields, "last_used", 0),
			Streak:   cols.GetUint8(fields, "streak", 0),
		}
		entries = append(entries, entry)
	}

	return entries, warnings
}

// parseShadowEntries 解析 Shadow 数据行。
func parseShadowEntries(body []byte, cols *ColumnDef) ([]ShadowPinEntry, []ShadowDelEntry, []string) {
	var pins []ShadowPinEntry
	var dels []ShadowDelEntry
	var warnings []string
	lineNum := 0

	scanner := bufio.NewScanner(bytes.NewReader(body))
	for scanner.Scan() {
		lineNum++
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}

		fields := strings.Split(line, "\t")
		action := cols.Get(fields, "action")
		code := cols.GetUnescaped(fields, "code")
		word := cols.GetUnescaped(fields, "word")

		if code == "" || word == "" {
			warnings = append(warnings, fmt.Sprintf("[shadow] 第 %d 行: 缺少编码或词条，已跳过", lineNum))
			continue
		}

		switch action {
		case "pin":
			position := cols.GetInt(fields, "position", 0)
			pins = append(pins, ShadowPinEntry{Code: code, Word: word, Position: position})
		case "del":
			dels = append(dels, ShadowDelEntry{Code: code, Word: word})
		default:
			warnings = append(warnings, fmt.Sprintf("[shadow] 第 %d 行: 未知操作 %q，已跳过", lineNum, action))
		}
	}

	return pins, dels, warnings
}

// parsePhraseEntries 解析短语数据行。
func parsePhraseEntries(body []byte, cols *ColumnDef) ([]PhraseEntry, []string) {
	var entries []PhraseEntry
	var warnings []string
	lineNum := 0

	scanner := bufio.NewScanner(bytes.NewReader(body))
	for scanner.Scan() {
		lineNum++
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}

		fields := strings.Split(line, "\t")
		code := cols.GetUnescaped(fields, "code")
		text := cols.GetUnescaped(fields, "text")

		if code == "" {
			warnings = append(warnings, fmt.Sprintf("[phrases] 第 %d 行: 缺少编码，已跳过", lineNum))
			continue
		}

		entry := PhraseEntry{
			Code:     code,
			Text:     text,
			Weight:   cols.GetInt(fields, "weight", 0),
			Position: cols.GetInt(fields, "position", 1),
			Enabled:  cols.GetBool(fields, "enabled", true),
		}
		entries = append(entries, entry)
	}

	return entries, warnings
}
