package backup

import (
	"archive/zip"
	"encoding/base64"
	"fmt"
	"io"

	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/dictio"
	"gopkg.in/yaml.v3"
)

// ExportDBToZip 将 store 中所有数据导出到 ZIP 的 db/ 路径下
func ExportDBToZip(zw *zip.Writer, s *store.Store) error {
	schemaIDs, err := s.ListSchemaIDs()
	if err != nil {
		return fmt.Errorf("list schema IDs: %w", err)
	}
	for _, id := range schemaIDs {
		pfx := "db/schemas/" + id + "/"
		if err := exportUserDict(zw, s, id, pfx+"userdict.txt", false); err != nil {
			return fmt.Errorf("export userdict %s: %w", id, err)
		}
		if err := exportUserDict(zw, s, id, pfx+"tempdict.txt", true); err != nil {
			return fmt.Errorf("export tempdict %s: %w", id, err)
		}
		if err := exportFreq(zw, s, id, pfx+"freq.yaml"); err != nil {
			return fmt.Errorf("export freq %s: %w", id, err)
		}
		if err := exportShadow(zw, s, id, pfx+"shadow.yaml"); err != nil {
			return fmt.Errorf("export shadow %s: %w", id, err)
		}
	}
	if err := exportGlobalPhrases(zw, s); err != nil {
		return fmt.Errorf("export global phrases: %w", err)
	}
	if err := exportStats(zw, s); err != nil {
		return fmt.Errorf("export stats: %w", err)
	}
	return nil
}

func exportUserDict(zw *zip.Writer, s *store.Store, schemaID, zipPath string, isTemp bool) error {
	var entries []store.UserWordBulkEntry
	var err error
	if isTemp {
		entries, err = s.AllTempWords(schemaID)
	} else {
		entries, err = s.AllUserWords(schemaID)
	}
	if err != nil {
		return err
	}
	if len(entries) == 0 {
		return nil
	}
	data := &dictio.ExportData{}
	for _, e := range entries {
		data.UserWords = append(data.UserWords, dictio.UserWordEntry{
			Code: e.Code, Text: e.Text, Weight: e.Weight,
			Count: e.Count, CreatedAt: e.CreatedAt,
		})
	}
	fw, err := zw.Create(zipPath)
	if err != nil {
		return err
	}
	return (&dictio.WindDictExporter{}).Export(fw, data, dictio.ExportOptions{})
}

// freqYAMLEntry 词频紧凑 YAML 格式
type freqYAMLEntry struct {
	C string `yaml:"c"`
	T string `yaml:"t"`
	N uint32 `yaml:"n"`
	L int64  `yaml:"l,omitempty"`
	S uint8  `yaml:"s,omitempty"`
}

func exportFreq(zw *zip.Writer, s *store.Store, schemaID, zipPath string) error {
	entries, err := s.AllFreq(schemaID)
	if err != nil || len(entries) == 0 {
		return err
	}
	items := make([]freqYAMLEntry, len(entries))
	for i, e := range entries {
		items[i] = freqYAMLEntry{C: e.Code, T: e.Text, N: e.Count, L: e.LastUsed, S: e.Streak}
	}
	fw, err := zw.Create(zipPath)
	if err != nil {
		return err
	}
	enc := yaml.NewEncoder(fw)
	enc.SetIndent(2)
	if err := enc.Encode(items); err != nil {
		return err
	}
	return enc.Close()
}

// shadowYAMLEntry Shadow 规则 YAML 格式（保留原始 JSON 值）
type shadowYAMLEntry struct {
	K string `yaml:"k"`
	V string `yaml:"v"` // JSON 文本
}

func exportShadow(zw *zip.Writer, s *store.Store, schemaID, zipPath string) error {
	entries, err := s.AllShadow(schemaID)
	if err != nil || len(entries) == 0 {
		return err
	}
	items := make([]shadowYAMLEntry, len(entries))
	for i, e := range entries {
		items[i] = shadowYAMLEntry{K: e.Code, V: string(e.RawValue)}
	}
	fw, err := zw.Create(zipPath)
	if err != nil {
		return err
	}
	enc := yaml.NewEncoder(fw)
	enc.SetIndent(2)
	if err := enc.Encode(items); err != nil {
		return err
	}
	return enc.Close()
}

// phraseYAMLEntry 短语 YAML 格式
// RawKey 包含 \x00 字节，必须 base64 编码后存储
type phraseYAMLEntry struct {
	K string `yaml:"k"` // base64(RawKey)
	V string `yaml:"v"` // JSON 文本
}

func exportGlobalPhrases(zw *zip.Writer, s *store.Store) error {
	entries, err := s.AllGlobalPhrases()
	if err != nil || len(entries) == 0 {
		return err
	}
	items := make([]phraseYAMLEntry, len(entries))
	for i, e := range entries {
		items[i] = phraseYAMLEntry{
			K: base64.StdEncoding.EncodeToString(e.RawKey),
			V: string(e.RawValue),
		}
	}
	fw, err := zw.Create("db/phrases.yaml")
	if err != nil {
		return err
	}
	enc := yaml.NewEncoder(fw)
	enc.SetIndent(2)
	if err := enc.Encode(items); err != nil {
		return err
	}
	return enc.Close()
}

// statsYAMLEntry 统计数据 YAML 格式
type statsYAMLEntry struct {
	D string `yaml:"d"`
	V string `yaml:"v"` // JSON 文本
}

func exportStats(zw *zip.Writer, s *store.Store) error {
	entries, err := s.AllStats()
	if err != nil || len(entries) == 0 {
		return err
	}
	items := make([]statsYAMLEntry, len(entries))
	for i, e := range entries {
		items[i] = statsYAMLEntry{D: e.Date, V: string(e.RawValue)}
	}
	fw, err := zw.Create("db/stats.yaml")
	if err != nil {
		return err
	}
	enc := yaml.NewEncoder(fw)
	enc.SetIndent(2)
	if err := enc.Encode(items); err != nil {
		return err
	}
	return enc.Close()
}

// ImportDBFromZip 从 ZIP 的 db/ 目录导入所有数据到 store
func ImportDBFromZip(zr *zip.Reader, s *store.Store) error {
	for _, id := range ExtractSchemaIDsFromZip(zr) {
		pfx := "db/schemas/" + id + "/"
		if err := importUserDict(zr, s, id, pfx+"userdict.txt", false); err != nil {
			return fmt.Errorf("import userdict %s: %w", id, err)
		}
		if err := importUserDict(zr, s, id, pfx+"tempdict.txt", true); err != nil {
			return fmt.Errorf("import tempdict %s: %w", id, err)
		}
		if err := importFreq(zr, s, id, pfx+"freq.yaml"); err != nil {
			return fmt.Errorf("import freq %s: %w", id, err)
		}
		if err := importShadow(zr, s, id, pfx+"shadow.yaml"); err != nil {
			return fmt.Errorf("import shadow %s: %w", id, err)
		}
	}
	if err := importGlobalPhrases(zr, s); err != nil {
		return fmt.Errorf("import global phrases: %w", err)
	}
	if err := importStats(zr, s); err != nil {
		return fmt.Errorf("import stats: %w", err)
	}
	// 重建统计聚合索引（Stats/Meta），否则 GetSummary 读不到总量
	if _, err := s.RecalculateStatsMeta(); err != nil {
		return fmt.Errorf("recalculate stats meta: %w", err)
	}
	return nil
}

func findInZip(zr *zip.Reader, name string) *zip.File {
	for _, f := range zr.File {
		if f.Name == name {
			return f
		}
	}
	return nil
}

func openZipEntry(zr *zip.Reader, name string) (io.ReadCloser, bool, error) {
	f := findInZip(zr, name)
	if f == nil {
		return nil, false, nil
	}
	rc, err := f.Open()
	if err != nil {
		return nil, false, err
	}
	return rc, true, nil
}

func importUserDict(zr *zip.Reader, s *store.Store, schemaID, zipPath string, isTemp bool) error {
	rc, ok, err := openZipEntry(zr, zipPath)
	if !ok || err != nil {
		return err
	}
	defer rc.Close()
	result, err := (&dictio.WindDictImporter{}).Import(rc, dictio.ImportOptions{})
	if err != nil {
		return err
	}
	entries := make([]store.UserWordBulkEntry, len(result.UserWords))
	for i, w := range result.UserWords {
		entries[i] = store.UserWordBulkEntry{
			Code: w.Code, Text: w.Text, Weight: w.Weight,
			Count: w.Count, CreatedAt: w.CreatedAt,
		}
	}
	if isTemp {
		return s.BulkPutTempWords(schemaID, entries)
	}
	return s.BulkPutUserWords(schemaID, entries)
}

func importFreq(zr *zip.Reader, s *store.Store, schemaID, zipPath string) error {
	rc, ok, err := openZipEntry(zr, zipPath)
	if !ok || err != nil {
		return err
	}
	defer rc.Close()
	var items []freqYAMLEntry
	if err := yaml.NewDecoder(rc).Decode(&items); err != nil {
		return err
	}
	entries := make([]store.FreqBulkEntry, len(items))
	for i, item := range items {
		entries[i] = store.FreqBulkEntry{
			Code: item.C, Text: item.T, Count: item.N, LastUsed: item.L, Streak: item.S,
		}
	}
	return s.BulkPutFreq(schemaID, entries)
}

func importShadow(zr *zip.Reader, s *store.Store, schemaID, zipPath string) error {
	rc, ok, err := openZipEntry(zr, zipPath)
	if !ok || err != nil {
		return err
	}
	defer rc.Close()
	var items []shadowYAMLEntry
	if err := yaml.NewDecoder(rc).Decode(&items); err != nil {
		return err
	}
	entries := make([]store.ShadowBulkEntry, len(items))
	for i, item := range items {
		entries[i] = store.ShadowBulkEntry{Code: item.K, RawValue: []byte(item.V)}
	}
	return s.BulkPutShadow(schemaID, entries)
}

func importGlobalPhrases(zr *zip.Reader, s *store.Store) error {
	rc, ok, err := openZipEntry(zr, "db/phrases.yaml")
	if !ok || err != nil {
		return err
	}
	defer rc.Close()
	var items []phraseYAMLEntry
	if err := yaml.NewDecoder(rc).Decode(&items); err != nil {
		return err
	}
	entries := make([]store.PhraseBulkEntry, len(items))
	for i, item := range items {
		rawKey, err := base64.StdEncoding.DecodeString(item.K)
		if err != nil {
			return fmt.Errorf("decode phrase key %q: %w", item.K, err)
		}
		entries[i] = store.PhraseBulkEntry{RawKey: rawKey, RawValue: []byte(item.V)}
	}
	return s.BulkPutGlobalPhrases(entries)
}

func importStats(zr *zip.Reader, s *store.Store) error {
	rc, ok, err := openZipEntry(zr, "db/stats.yaml")
	if !ok || err != nil {
		return err
	}
	defer rc.Close()
	var items []statsYAMLEntry
	if err := yaml.NewDecoder(rc).Decode(&items); err != nil {
		return err
	}
	entries := make([]store.DailyStatBulkEntry, len(items))
	for i, item := range items {
		entries[i] = store.DailyStatBulkEntry{Date: item.D, RawValue: []byte(item.V)}
	}
	return s.BulkPutStats(entries)
}
