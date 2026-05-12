package dictio

import (
	"bytes"
	"strings"
	"testing"
)

// ---- WindDict 导入器 ----

func TestWindDictImportExportRoundtrip(t *testing.T) {
	// 导出 → 导入闭环测试
	data := testExportData()

	var buf bytes.Buffer
	exporter := &WindDictExporter{}
	opts := ExportOptions{SchemaID: "wubi86", SchemaName: "五笔86", Generator: "Test"}
	if err := exporter.Export(&buf, data, opts); err != nil {
		t.Fatalf("Export: %v", err)
	}

	importer := &WindDictImporter{}
	result, err := importer.Import(bytes.NewReader(buf.Bytes()), ImportOptions{})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}

	// 验证 user_words
	if len(result.UserWords) != len(data.UserWords) {
		t.Errorf("UserWords count = %d, want %d", len(result.UserWords), len(data.UserWords))
	}
	if len(result.UserWords) > 0 {
		first := result.UserWords[0]
		if first.Code != "a" || first.Text != "工" || first.Weight != 100 {
			t.Errorf("first user word = %+v, want code=a text=工 weight=100", first)
		}
	}

	// 验证多行文本闭环
	for _, uw := range result.UserWords {
		if uw.Code == "ydjj" {
			if uw.Text != "第一行\n第二行" {
				t.Errorf("multiline text = %q, want %q", uw.Text, "第一行\n第二行")
			}
			break
		}
	}

	// 验证 temp_words
	if len(result.TempWords) != len(data.TempWords) {
		t.Errorf("TempWords count = %d, want %d", len(result.TempWords), len(data.TempWords))
	}

	// 验证 freq
	if len(result.FreqData) != len(data.FreqData) {
		t.Errorf("FreqData count = %d, want %d", len(result.FreqData), len(data.FreqData))
	}
	if len(result.FreqData) > 0 {
		f := result.FreqData[0]
		if f.Code != "a" || f.Text != "工" || f.Count != 42 || f.Streak != 3 {
			t.Errorf("first freq = %+v", f)
		}
	}

	// 验证 shadow
	if len(result.ShadowPins) != 1 {
		t.Errorf("ShadowPins count = %d, want 1", len(result.ShadowPins))
	}
	if len(result.ShadowDels) != 1 {
		t.Errorf("ShadowDels count = %d, want 1", len(result.ShadowDels))
	}

	// 验证 phrases
	if len(result.Phrases) != len(data.Phrases) {
		t.Errorf("Phrases count = %d, want %d", len(result.Phrases), len(data.Phrases))
	}
	// 验证短语多行文本
	for _, p := range result.Phrases {
		if p.Code == "qm" {
			if p.Text != "张三\n手机: 138" {
				t.Errorf("phrase multiline = %q, want %q", p.Text, "张三\n手机: 138")
			}
		}
		if p.Code == "yx" {
			if p.Type != "array" || p.Name != "邮箱" {
				t.Errorf("array phrase = type=%q name=%q", p.Type, p.Name)
			}
		}
	}
}

func TestWindDictImportSectionFilter(t *testing.T) {
	data := testExportData()

	var buf bytes.Buffer
	exporter := &WindDictExporter{}
	if err := exporter.Export(&buf, data, ExportOptions{SchemaID: "test", Generator: "Test"}); err != nil {
		t.Fatalf("Export: %v", err)
	}

	// 只导入 freq
	result, err := (&WindDictImporter{}).Import(bytes.NewReader(buf.Bytes()), ImportOptions{
		Sections: []string{SectionFreq},
	})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}

	if len(result.UserWords) != 0 {
		t.Errorf("should not have UserWords, got %d", len(result.UserWords))
	}
	if len(result.FreqData) != 2 {
		t.Errorf("FreqData count = %d, want 2", len(result.FreqData))
	}
}

func TestWindDictImportColumnsReorder(t *testing.T) {
	// 测试列顺序不同于默认
	input := `wind_dict:
  version: 1
  generator: Test
  exported_at: "2026-04-16T10:30:00+08:00"
  schema_id: test
  sections:
    user_words:
      columns: [text, code, weight]

--- !user_words
工	a	100
式	aa	200
`
	result, err := (&WindDictImporter{}).Import(strings.NewReader(input), ImportOptions{})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}

	if len(result.UserWords) != 2 {
		t.Fatalf("UserWords count = %d, want 2", len(result.UserWords))
	}

	first := result.UserWords[0]
	if first.Code != "a" || first.Text != "工" || first.Weight != 100 {
		t.Errorf("reordered first = %+v, want code=a text=工 weight=100", first)
	}
}

func TestPreviewWindDict(t *testing.T) {
	data := testExportData()

	var buf bytes.Buffer
	exporter := &WindDictExporter{}
	if err := exporter.Export(&buf, data, ExportOptions{SchemaID: "test", Generator: "Test"}); err != nil {
		t.Fatalf("Export: %v", err)
	}

	header, counts, err := PreviewWindDict(buf.Bytes())
	if err != nil {
		t.Fatalf("PreviewWindDict: %v", err)
	}

	if header.SchemaID != "test" {
		t.Errorf("schema_id = %q, want test", header.SchemaID)
	}
	if counts[SectionUserWords] != 3 {
		t.Errorf("user_words count = %d, want 3", counts[SectionUserWords])
	}
	if counts[SectionFreq] != 2 {
		t.Errorf("freq count = %d, want 2", counts[SectionFreq])
	}
}

// ---- TSV 导入器 ----

func TestTSVImporter(t *testing.T) {
	input := "# header\na\t工\t100\naa\t式\t200"
	result, err := (&TSVImporter{}).Import(strings.NewReader(input), ImportOptions{})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}

	if len(result.UserWords) != 2 {
		t.Fatalf("count = %d, want 2", len(result.UserWords))
	}
	if result.UserWords[0].Code != "a" || result.UserWords[0].Weight != 100 {
		t.Errorf("first = %+v", result.UserWords[0])
	}
}

func TestTSVImporterSkipsInvalid(t *testing.T) {
	input := "onlyonefield\na\t工\t100\n"
	result, err := (&TSVImporter{}).Import(strings.NewReader(input), ImportOptions{})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}
	if len(result.UserWords) != 1 {
		t.Errorf("count = %d, want 1", len(result.UserWords))
	}
	if result.Stats.SkippedCount != 1 {
		t.Errorf("skipped = %d, want 1", result.Stats.SkippedCount)
	}
}

func TestTSVImporterValidatesCode(t *testing.T) {
	input := "# 正常行和乱码行混合\na\t工\t100\n中文编码\t乱码\t50\n\t空编码\t50\nbb\t\t100\nabc\t测试\t200\n"
	result, err := (&TSVImporter{}).Import(strings.NewReader(input), ImportOptions{})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}
	// 只有 "a\t工" 和 "abc\t测试" 是合法的
	if len(result.UserWords) != 2 {
		t.Errorf("valid count = %d, want 2", len(result.UserWords))
	}
	if result.Stats.SkippedCount != 3 {
		t.Errorf("skipped = %d, want 3", result.Stats.SkippedCount)
	}
	hasCodeWarning := false
	for _, w := range result.Warnings {
		if strings.Contains(w, "无效字符") {
			hasCodeWarning = true
			break
		}
	}
	if !hasCodeWarning {
		t.Error("should have invalid code warning")
	}
}

// ---- TextList 导入器 ----

func TestTextListImporter(t *testing.T) {
	input := "# 词语列表\n输入法\n候选词\n\n工作\n"
	result, err := (&TextListImporter{}).Import(strings.NewReader(input), ImportOptions{})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}

	if len(result.UserWords) != 3 {
		t.Fatalf("count = %d, want 3", len(result.UserWords))
	}
	if result.UserWords[0].Text != "输入法" || result.UserWords[0].Code != "" {
		t.Errorf("first = %+v, Code should be empty", result.UserWords[0])
	}
}

// ---- PhraseYAML 导入器 ----

func TestPhraseYAMLImporter(t *testing.T) {
	input := `phrases:
  - code: sj
    text: "$time_now"
    position: 1
  - code: yx
    texts: "a@b.com\nc@d.com"
    name: 邮箱
    position: 1
  - code: off
    text: "test"
    disabled: true
`
	result, err := (&PhraseYAMLImporter{}).Import(strings.NewReader(input), ImportOptions{})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}

	if len(result.Phrases) != 3 {
		t.Fatalf("count = %d, want 3", len(result.Phrases))
	}

	if result.Phrases[0].Type != "dynamic" {
		t.Errorf("sj type = %q, want dynamic", result.Phrases[0].Type)
	}
	if result.Phrases[1].Type != "array" || result.Phrases[1].Name != "邮箱" {
		t.Errorf("yx = type=%q name=%q", result.Phrases[1].Type, result.Phrases[1].Name)
	}
	if result.Phrases[2].Enabled {
		t.Error("off phrase should be disabled")
	}
}

func TestPhraseYAMLRejectsWindDict(t *testing.T) {
	input := `wind_dict:
  version: 1
`
	_, err := (&PhraseYAMLImporter{}).Import(strings.NewReader(input), ImportOptions{})
	if err == nil {
		t.Error("should reject WindDict format")
	}
}

// ---- Rime 导入器 ----

func TestRimeDictImporter(t *testing.T) {
	input := `---
name: test
version: "1.0"
columns:
  - text
  - code
  - weight
...

工	a	100
式	aa	200
# 注释行
中国	gg	500
`
	result, err := (&RimeDictImporter{}).Import(strings.NewReader(input), ImportOptions{})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}

	if len(result.UserWords) != 3 {
		t.Fatalf("count = %d, want 3", len(result.UserWords))
	}

	first := result.UserWords[0]
	if first.Text != "工" || first.Code != "a" || first.Weight != 100 {
		t.Errorf("first = %+v", first)
	}

	last := result.UserWords[2]
	if last.Text != "中国" || last.Code != "gg" {
		t.Errorf("last = %+v", last)
	}
}

func TestRimeDictImporterDefaultColumns(t *testing.T) {
	// 无 columns 声明，使用默认 [text, code, weight]
	input := `---
name: test
version: "1.0"
...

工	a	100
`
	result, err := (&RimeDictImporter{}).Import(strings.NewReader(input), ImportOptions{})
	if err != nil {
		t.Fatalf("Import: %v", err)
	}

	if len(result.UserWords) != 1 {
		t.Fatalf("count = %d, want 1", len(result.UserWords))
	}
	if result.UserWords[0].Text != "工" || result.UserWords[0].Code != "a" {
		t.Errorf("entry = %+v", result.UserWords[0])
	}
}

// ---- ZIP 导入导出闭环 ----

func TestZipImportExportRoundtrip(t *testing.T) {
	schemas := []SchemaExportData{
		{
			SchemaID:   "wubi86",
			SchemaName: "五笔86",
			Data: &ExportData{
				UserWords: []UserWordEntry{
					{Code: "a", Text: "工", Weight: 100},
					{Code: "aa", Text: "式", Weight: 200},
				},
				FreqData: []FreqEntry{
					{Code: "a", Text: "工", Count: 42, LastUsed: 1713234000, Streak: 3},
				},
			},
		},
	}
	phrases := []PhraseEntry{
		{Code: "sj", Type: "static", Text: "$time_now", Position: 1, Enabled: true},
	}

	// 导出
	var buf bytes.Buffer
	if err := ExportZip(&buf, schemas, phrases, ZipExportOptions{Generator: "Test"}); err != nil {
		t.Fatalf("ExportZip: %v", err)
	}

	// 导入
	zipData := buf.Bytes()
	result, err := ImportZip(bytes.NewReader(zipData), int64(len(zipData)), ImportOptions{})
	if err != nil {
		t.Fatalf("ImportZip: %v", err)
	}

	// 验证 manifest
	if result.Manifest.Version != 1 {
		t.Errorf("manifest version = %d", result.Manifest.Version)
	}

	// 验证方案数据
	wubi, ok := result.Schemas["wubi86"]
	if !ok {
		t.Fatal("missing wubi86 schema")
	}
	if len(wubi.UserWords) != 2 {
		t.Errorf("wubi86 UserWords = %d, want 2", len(wubi.UserWords))
	}
	if len(wubi.FreqData) != 1 {
		t.Errorf("wubi86 FreqData = %d, want 1", len(wubi.FreqData))
	}

	// 验证短语
	if result.Phrases == nil {
		t.Fatal("missing phrases")
	}
	if len(result.Phrases.Phrases) != 1 {
		t.Errorf("phrases count = %d, want 1", len(result.Phrases.Phrases))
	}
}
