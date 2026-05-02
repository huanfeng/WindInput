package backup_test

import (
	"archive/zip"
	"bytes"
	"os"
	"path/filepath"
	"sort"
	"testing"

	"github.com/huanfeng/wind_input/internal/backup"
	"github.com/huanfeng/wind_input/internal/store"
)

func TestManifestRoundTrip(t *testing.T) {
	dir := t.TempDir()
	m := &backup.Manifest{
		Version:     "1.0",
		AppVersion:  "test-0.1",
		CreatedAt:   "2026-05-02T10:00:00+08:00",
		DataDirMode: "standard",
	}
	path := filepath.Join(dir, "manifest.yaml")
	if err := backup.WriteManifest(path, m); err != nil {
		t.Fatalf("WriteManifest: %v", err)
	}
	got, err := backup.ReadManifest(path)
	if err != nil {
		t.Fatalf("ReadManifest: %v", err)
	}
	if got.Version != m.Version || got.AppVersion != m.AppVersion || got.DataDirMode != m.DataDirMode {
		t.Errorf("roundtrip mismatch: got %+v, want %+v", got, m)
	}
}

func openTestStore(t *testing.T, path string) *store.Store {
	t.Helper()
	s, err := store.Open(path)
	if err != nil {
		t.Fatalf("open store %q: %v", path, err)
	}
	return s
}

func TestDBExportImportRoundTrip(t *testing.T) {
	// 创建源 DB
	f1, err := os.CreateTemp("", "src-*.db")
	if err != nil {
		t.Fatal(err)
	}
	f1.Close()
	defer os.Remove(f1.Name())

	s1 := openTestStore(t, f1.Name())
	if err := s1.BulkPutUserWords("wubi86", []store.UserWordBulkEntry{
		{Code: "abcd", Text: "测试词", Weight: 100, Count: 3, CreatedAt: 1000},
	}); err != nil {
		t.Fatalf("BulkPutUserWords: %v", err)
	}
	if err := s1.BulkPutFreq("wubi86", []store.FreqBulkEntry{
		{Code: "abcd", Text: "测试词", Count: 5, LastUsed: 2000, Streak: 1},
	}); err != nil {
		t.Fatalf("BulkPutFreq: %v", err)
	}
	s1.Close()

	// 导出到内存 ZIP
	s2 := openTestStore(t, f1.Name())
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	if err := backup.ExportDBToZip(zw, s2); err != nil {
		t.Fatalf("ExportDBToZip: %v", err)
	}
	zw.Close()
	s2.Close()

	// 导入到新 DB
	f2, err := os.CreateTemp("", "dst-*.db")
	if err != nil {
		t.Fatal(err)
	}
	f2.Close()
	defer os.Remove(f2.Name())

	s3 := openTestStore(t, f2.Name())
	zr, err := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
	if err != nil {
		t.Fatalf("open zip reader: %v", err)
	}
	if err := backup.ImportDBFromZip(zr, s3); err != nil {
		t.Fatalf("ImportDBFromZip: %v", err)
	}

	// 验证用户词
	words, err := s3.AllUserWords("wubi86")
	if err != nil {
		t.Fatalf("AllUserWords after import: %v", err)
	}
	if len(words) != 1 || words[0].Text != "测试词" || words[0].Code != "abcd" {
		t.Errorf("unexpected words after import: %+v", words)
	}
	if words[0].Count != 3 || words[0].CreatedAt != 1000 {
		t.Errorf("fields mismatch: %+v", words[0])
	}

	// 验证词频
	freqs, err := s3.AllFreq("wubi86")
	s3.Close()
	if err != nil {
		t.Fatalf("AllFreq after import: %v", err)
	}
	if len(freqs) != 1 || freqs[0].Count != 5 || freqs[0].Streak != 1 {
		t.Errorf("unexpected freqs after import: %+v", freqs)
	}
}

func TestManifestInZipRoundTrip(t *testing.T) {
	m := &backup.Manifest{
		Version:     "1.0",
		AppVersion:  "0.9.9",
		CreatedAt:   "2026-05-02T12:00:00+08:00",
		DataDirMode: "portable",
	}
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	if err := backup.WriteManifestToZip(zw, m); err != nil {
		t.Fatalf("WriteManifestToZip: %v", err)
	}
	zw.Close()

	// 写入临时 zip 文件再用 ReadManifestFromZip 读取
	f, err := os.CreateTemp("", "mtest-*.zip")
	if err != nil {
		t.Fatal(err)
	}
	f.Write(buf.Bytes())
	f.Close()
	defer os.Remove(f.Name())

	got, err := backup.ReadManifestFromZip(f.Name())
	if err != nil {
		t.Fatalf("ReadManifestFromZip: %v", err)
	}
	if got.Version != m.Version || got.AppVersion != m.AppVersion ||
		got.CreatedAt != m.CreatedAt || got.DataDirMode != m.DataDirMode {
		t.Errorf("manifest mismatch: got %+v, want %+v", got, m)
	}
}

func TestReadManifestFromZipNotFound(t *testing.T) {
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	fw, _ := zw.Create("other.txt")
	fw.Write([]byte("data"))
	zw.Close()

	f, err := os.CreateTemp("", "nomanifest-*.zip")
	if err != nil {
		t.Fatal(err)
	}
	f.Write(buf.Bytes())
	f.Close()
	defer os.Remove(f.Name())

	_, err = backup.ReadManifestFromZip(f.Name())
	if err == nil {
		t.Fatal("expected error when manifest.yaml absent, got nil")
	}
}

func TestCopyDirToZipAndExtract(t *testing.T) {
	// 构建源目录
	srcDir := t.TempDir()
	os.WriteFile(filepath.Join(srcDir, "a.txt"), []byte("hello"), 0644)
	os.MkdirAll(filepath.Join(srcDir, "sub"), 0755)
	os.WriteFile(filepath.Join(srcDir, "sub", "b.txt"), []byte("world"), 0644)

	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	if err := backup.CopyDirToZip(zw, srcDir, "files/", nil); err != nil {
		t.Fatalf("CopyDirToZip: %v", err)
	}
	zw.Close()

	zr, err := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
	if err != nil {
		t.Fatalf("NewReader: %v", err)
	}
	destDir := t.TempDir()
	if err := backup.ExtractZipPrefix(zr, "files/", destDir); err != nil {
		t.Fatalf("ExtractZipPrefix: %v", err)
	}

	got, _ := os.ReadFile(filepath.Join(destDir, "a.txt"))
	if string(got) != "hello" {
		t.Errorf("a.txt: got %q", got)
	}
	got, _ = os.ReadFile(filepath.Join(destDir, "sub", "b.txt"))
	if string(got) != "world" {
		t.Errorf("sub/b.txt: got %q", got)
	}
}

func TestCopyDirToZipExclude(t *testing.T) {
	srcDir := t.TempDir()
	os.WriteFile(filepath.Join(srcDir, "keep.txt"), []byte("keep"), 0644)
	os.WriteFile(filepath.Join(srcDir, "skip.db"), []byte("skip"), 0644)
	os.MkdirAll(filepath.Join(srcDir, "skipdir"), 0755)
	os.WriteFile(filepath.Join(srcDir, "skipdir", "inner.txt"), []byte("inner"), 0644)

	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	if err := backup.CopyDirToZip(zw, srcDir, "files/", []string{"skip.db", "skipdir"}); err != nil {
		t.Fatalf("CopyDirToZip: %v", err)
	}
	zw.Close()

	zr, _ := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
	names := make(map[string]bool)
	for _, f := range zr.File {
		names[f.Name] = true
	}
	if !names["files/keep.txt"] {
		t.Error("files/keep.txt should be present")
	}
	if names["files/skip.db"] {
		t.Error("files/skip.db should be excluded")
	}
	if names["files/skipdir/inner.txt"] {
		t.Error("files/skipdir/inner.txt should be excluded")
	}
}

func TestExtractZipPrefixZipSlip(t *testing.T) {
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	// 构造恶意路径穿越条目
	fw, _ := zw.Create("files/../../evil.txt")
	fw.Write([]byte("evil"))
	zw.Close()

	zr, _ := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
	destDir := t.TempDir()
	err := backup.ExtractZipPrefix(zr, "files/", destDir)
	if err == nil {
		t.Fatal("expected zip-slip error, got nil")
	}
}

func TestExtractSchemaIDsFromZip(t *testing.T) {
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	for _, name := range []string{
		"db/schemas/wubi86/userdict.txt",
		"db/schemas/wubi86/freq.yaml",
		"db/schemas/pinyin/userdict.txt",
		"db/phrases.yaml",
	} {
		fw, _ := zw.Create(name)
		fw.Write([]byte(""))
	}
	zw.Close()

	zr, _ := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
	ids := backup.ExtractSchemaIDsFromZip(zr)
	sort.Strings(ids)
	if len(ids) != 2 || ids[0] != "pinyin" || ids[1] != "wubi86" {
		t.Errorf("unexpected schema IDs: %v", ids)
	}
}

func TestAtomicReplaceDir(t *testing.T) {
	src := t.TempDir()
	dest := t.TempDir()

	// src 有新内容
	os.WriteFile(filepath.Join(src, "a.txt"), []byte("new_a"), 0644)
	os.MkdirAll(filepath.Join(src, "sub"), 0755)
	os.WriteFile(filepath.Join(src, "sub", "b.txt"), []byte("new_b"), 0644)

	// dest 有旧内容（a.txt 旧值，c.txt 应被删除）
	os.WriteFile(filepath.Join(dest, "a.txt"), []byte("old_a"), 0644)
	os.WriteFile(filepath.Join(dest, "c.txt"), []byte("old_c"), 0644)

	if err := backup.AtomicReplaceDir(src, dest); err != nil {
		t.Fatalf("AtomicReplaceDir: %v", err)
	}

	got, _ := os.ReadFile(filepath.Join(dest, "a.txt"))
	if string(got) != "new_a" {
		t.Errorf("a.txt: got %q, want %q", got, "new_a")
	}
	got, _ = os.ReadFile(filepath.Join(dest, "sub", "b.txt"))
	if string(got) != "new_b" {
		t.Errorf("sub/b.txt: got %q, want %q", got, "new_b")
	}
	if _, err := os.Stat(filepath.Join(dest, "c.txt")); !os.IsNotExist(err) {
		t.Error("c.txt should have been removed from dest")
	}
}

func TestDBExportImportShadowPhraseStats(t *testing.T) {
	f1, err := os.CreateTemp("", "src3-*.db")
	if err != nil {
		t.Fatal(err)
	}
	f1.Close()
	defer os.Remove(f1.Name())

	s1 := openTestStore(t, f1.Name())
	// Shadow
	if err := s1.BulkPutShadow("wubi86", []store.ShadowBulkEntry{
		{Code: "abcd", RawValue: []byte(`{"type":"pin","word":"固顶词","pos":0}`)},
	}); err != nil {
		t.Fatalf("BulkPutShadow: %v", err)
	}
	// 全局短语（RawKey 含 \x00）
	if err := s1.BulkPutGlobalPhrases([]store.PhraseBulkEntry{
		{
			Code:     "nide",
			RawKey:   []byte("nide\x00你的"),
			RawValue: []byte(`{"text":"你的","type":"static","pos":0,"on":true}`),
		},
	}); err != nil {
		t.Fatalf("BulkPutGlobalPhrases: %v", err)
	}
	// 统计
	if err := s1.BulkPutStats([]store.DailyStatBulkEntry{
		{Date: "2026-05-01", RawValue: []byte(`{"tc":888}`)},
	}); err != nil {
		t.Fatalf("BulkPutStats: %v", err)
	}
	s1.Close()

	// 导出
	s2 := openTestStore(t, f1.Name())
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	if err := backup.ExportDBToZip(zw, s2); err != nil {
		t.Fatalf("ExportDBToZip: %v", err)
	}
	zw.Close()
	s2.Close()

	// 导入
	f2, err := os.CreateTemp("", "dst3-*.db")
	if err != nil {
		t.Fatal(err)
	}
	f2.Close()
	defer os.Remove(f2.Name())

	s3 := openTestStore(t, f2.Name())
	zr, err := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
	if err != nil {
		t.Fatalf("zip.NewReader: %v", err)
	}
	if err := backup.ImportDBFromZip(zr, s3); err != nil {
		t.Fatalf("ImportDBFromZip: %v", err)
	}

	// 验证 shadow
	shadows, err := s3.AllShadow("wubi86")
	if err != nil {
		t.Fatalf("AllShadow: %v", err)
	}
	if len(shadows) != 1 || shadows[0].Code != "abcd" {
		t.Errorf("shadow: got %+v", shadows)
	}

	// 验证全局短语（RawKey round-trip）
	phrases, err := s3.AllGlobalPhrases()
	if err != nil {
		t.Fatalf("AllGlobalPhrases: %v", err)
	}
	if len(phrases) != 1 || string(phrases[0].RawKey) != "nide\x00你的" {
		t.Errorf("phrases: got %+v", phrases)
	}

	// 验证统计
	stats, err := s3.AllStats()
	s3.Close()
	if err != nil {
		t.Fatalf("AllStats: %v", err)
	}
	if len(stats) != 1 || stats[0].Date != "2026-05-01" {
		t.Errorf("stats: got %+v", stats)
	}
}

func TestDBExportImportMultiSchema(t *testing.T) {
	f1, err := os.CreateTemp("", "multi-src-*.db")
	if err != nil {
		t.Fatal(err)
	}
	f1.Close()
	defer os.Remove(f1.Name())

	s1 := openTestStore(t, f1.Name())
	for _, schema := range []string{"wubi86", "pinyin"} {
		if err := s1.BulkPutUserWords(schema, []store.UserWordBulkEntry{
			{Code: "aa", Text: schema + "_词"},
		}); err != nil {
			t.Fatalf("BulkPutUserWords %s: %v", schema, err)
		}
		if err := s1.BulkPutFreq(schema, []store.FreqBulkEntry{
			{Code: "aa", Text: schema + "_词", Count: 3},
		}); err != nil {
			t.Fatalf("BulkPutFreq %s: %v", schema, err)
		}
	}
	s1.Close()

	s2 := openTestStore(t, f1.Name())
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	if err := backup.ExportDBToZip(zw, s2); err != nil {
		t.Fatalf("ExportDBToZip: %v", err)
	}
	zw.Close()
	s2.Close()

	f2, err := os.CreateTemp("", "multi-dst-*.db")
	if err != nil {
		t.Fatal(err)
	}
	f2.Close()
	defer os.Remove(f2.Name())

	s3 := openTestStore(t, f2.Name())
	zr, _ := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
	if err := backup.ImportDBFromZip(zr, s3); err != nil {
		t.Fatalf("ImportDBFromZip: %v", err)
	}

	// 验证两个方案的数据相互独立
	for _, schema := range []string{"wubi86", "pinyin"} {
		words, err := s3.AllUserWords(schema)
		if err != nil {
			t.Fatalf("AllUserWords %s: %v", schema, err)
		}
		if len(words) != 1 || words[0].Text != schema+"_词" {
			t.Errorf("schema %s words: got %+v", schema, words)
		}
		freqs, err := s3.AllFreq(schema)
		if err != nil {
			t.Fatalf("AllFreq %s: %v", schema, err)
		}
		if len(freqs) != 1 || freqs[0].Count != 3 {
			t.Errorf("schema %s freqs: got %+v", schema, freqs)
		}
	}
	s3.Close()
}
