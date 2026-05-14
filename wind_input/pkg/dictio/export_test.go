package dictio

import (
	"archive/zip"
	"bytes"
	"strings"
	"testing"
)

func testExportData() *ExportData {
	return &ExportData{
		UserWords: []UserWordEntry{
			{Code: "a", Text: "工", Weight: 100, Count: 5, CreatedAt: 1713234567},
			{Code: "aa", Text: "式", Weight: 200, Count: 0, CreatedAt: 1713234000},
			{Code: "ydjj", Text: "第一行\n第二行", Weight: 50, Count: 1, CreatedAt: 1713230000},
		},
		TempWords: []UserWordEntry{
			{Code: "gkd", Text: "格调", Weight: 50, Count: 3, CreatedAt: 1713234000},
		},
		FreqData: []FreqEntry{
			{Code: "a", Text: "工", Count: 42, LastUsed: 1713234000, Streak: 3},
			{Code: "gg", Text: "中国", Count: 128, LastUsed: 1713234500, Streak: 5},
		},
		Shadow: map[string]ShadowRecord{
			"gg": {
				Pinned:  []ShadowPinEntry{{Code: "gg", Word: "工工", Position: 1}},
				Deleted: []string{"贡贡"},
			},
		},
		Phrases: []PhraseEntry{
			{Code: "sj", Text: "$time_now", Weight: 5000, Position: 1, Enabled: true},
			{Code: "qm", Text: "张三\n手机: 138", Weight: 1000, Position: 1, Enabled: true},
			{Code: "yx", Text: `$AA("邮箱", "a@b.com\nc@d.com")`, Weight: 800, Position: 1, Enabled: true},
		},
	}
}

func TestWindDictExporterBasic(t *testing.T) {
	exporter := &WindDictExporter{}
	data := testExportData()

	var buf bytes.Buffer
	opts := ExportOptions{
		SchemaID:   "wubi86",
		SchemaName: "五笔86",
		Generator:  "TestExporter",
	}

	if err := exporter.Export(&buf, data, opts); err != nil {
		t.Fatalf("Export: %v", err)
	}

	output := buf.String()

	// 验证 YAML 头部
	if !strings.Contains(output, "wind_dict:") {
		t.Error("missing wind_dict header")
	}
	if !strings.Contains(output, "schema_id: wubi86") {
		t.Error("missing schema_id")
	}
	if !strings.Contains(output, "version: 1") {
		t.Error("missing version")
	}

	// 验证 section 分隔符
	if !strings.Contains(output, "--- !user_words") {
		t.Error("missing user_words section")
	}
	if !strings.Contains(output, "--- !temp_words") {
		t.Error("missing temp_words section")
	}
	if !strings.Contains(output, "--- !freq") {
		t.Error("missing freq section")
	}
	if !strings.Contains(output, "--- !shadow") {
		t.Error("missing shadow section")
	}
	if !strings.Contains(output, "--- !phrases") {
		t.Error("missing phrases section")
	}

	// 验证数据行
	if !strings.Contains(output, "a\t工\t100\t5\t1713234567") {
		t.Error("missing user word data line")
	}

	// 验证多行文本转义
	if !strings.Contains(output, `ydjj`+"\t"+`第一行\n第二行`) {
		t.Error("multiline text should be escaped")
	}

	// 验证词频数据
	if !strings.Contains(output, "a\t工\t42\t1713234000\t3") {
		t.Error("missing freq data line")
	}

	// 验证 shadow
	if !strings.Contains(output, "pin\tgg\t工工\t1") {
		t.Error("missing shadow pin line")
	}
	if !strings.Contains(output, "del\tgg\t贡贡") {
		t.Error("missing shadow del line")
	}

	// 验证短语 (新列序: code, text, weight, position, enabled)
	if !strings.Contains(output, "sj\t$time_now\t5000\t1\t1") {
		t.Error("missing phrase line")
	}
	if !strings.Contains(output, `qm`+"\t"+`张三\n手机: 138`) {
		t.Error("multiline phrase should be escaped")
	}
}

func TestWindDictExporterSectionFilter(t *testing.T) {
	exporter := &WindDictExporter{}
	data := testExportData()

	var buf bytes.Buffer
	opts := ExportOptions{
		SchemaID:  "wubi86",
		Sections:  []string{SectionUserWords, SectionShadow},
		Generator: "Test",
	}

	if err := exporter.Export(&buf, data, opts); err != nil {
		t.Fatalf("Export: %v", err)
	}

	output := buf.String()

	if !strings.Contains(output, "--- !user_words") {
		t.Error("should include user_words")
	}
	if !strings.Contains(output, "--- !shadow") {
		t.Error("should include shadow")
	}
	if strings.Contains(output, "--- !freq") {
		t.Error("should not include freq")
	}
	if strings.Contains(output, "--- !temp_words") {
		t.Error("should not include temp_words")
	}
	if strings.Contains(output, "--- !phrases") {
		t.Error("should not include phrases")
	}
}

func TestWindDictExporterEmptyData(t *testing.T) {
	exporter := &WindDictExporter{}
	data := &ExportData{}

	var buf bytes.Buffer
	opts := ExportOptions{SchemaID: "test", Generator: "Test"}

	if err := exporter.Export(&buf, data, opts); err != nil {
		t.Fatalf("Export: %v", err)
	}

	output := buf.String()
	if !strings.Contains(output, "wind_dict:") {
		t.Error("should still have header")
	}
	// 空数据不应有任何 section
	if strings.Contains(output, "--- !") {
		t.Error("empty data should not have sections")
	}
}

func TestExportZip(t *testing.T) {
	schemas := []SchemaExportData{
		{
			SchemaID:   "wubi86",
			SchemaName: "五笔86",
			Data: &ExportData{
				UserWords: []UserWordEntry{
					{Code: "a", Text: "工", Weight: 100},
				},
			},
		},
	}
	phrases := []PhraseEntry{
		{Code: "sj", Text: "$time_now", Weight: 1000, Position: 1, Enabled: true},
	}

	var buf bytes.Buffer
	opts := ZipExportOptions{Generator: "TestZip"}
	if err := ExportZip(&buf, schemas, phrases, opts); err != nil {
		t.Fatalf("ExportZip: %v", err)
	}

	// 验证 ZIP 结构
	reader, err := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
	if err != nil {
		t.Fatalf("zip.NewReader: %v", err)
	}

	fileNames := make(map[string]bool)
	for _, f := range reader.File {
		fileNames[f.Name] = true
	}

	if !fileNames["manifest.yaml"] {
		t.Error("missing manifest.yaml")
	}
	if !fileNames["wubi86.wdict.yaml"] {
		t.Error("missing wubi86.wdict.yaml")
	}
	if !fileNames["phrases.wdict.yaml"] {
		t.Error("missing phrases.wdict.yaml")
	}

	// 验证 manifest 内容
	for _, f := range reader.File {
		if f.Name == "manifest.yaml" {
			rc, err := f.Open()
			if err != nil {
				t.Fatalf("open manifest: %v", err)
			}
			var mbuf bytes.Buffer
			mbuf.ReadFrom(rc)
			rc.Close()

			manifest, err := ParseZipManifest(mbuf.Bytes())
			if err != nil {
				t.Fatalf("ParseZipManifest: %v", err)
			}
			if manifest.Version != 1 {
				t.Errorf("manifest version = %d, want 1", manifest.Version)
			}
			if len(manifest.Schemas) != 1 {
				t.Errorf("manifest schemas count = %d, want 1", len(manifest.Schemas))
			}
			if manifest.Schemas[0].ID != "wubi86" {
				t.Errorf("manifest schema id = %q, want wubi86", manifest.Schemas[0].ID)
			}
			if manifest.Phrases == nil {
				t.Error("manifest should have phrases entry")
			}
		}
	}
}
