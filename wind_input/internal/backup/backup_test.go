package backup_test

import (
	"archive/zip"
	"bytes"
	"os"
	"path/filepath"
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
