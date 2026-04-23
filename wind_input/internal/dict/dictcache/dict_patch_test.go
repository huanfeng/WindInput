package dictcache

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"
)

func TestLoadDictPatch_FullNames(t *testing.T) {
	dir := t.TempDir()
	patchFile := filepath.Join(dir, "test.dict.patch.yaml")

	content := `---
entries:
  - code: a
    text: 工
    weight: 30
  - code: abcd
    text: 新词
    weight: 100
delete:
  - code: a
    text: 戈
`
	if err := os.WriteFile(patchFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	patch, err := LoadDictPatch(patchFile)
	if err != nil {
		t.Fatal(err)
	}

	if len(patch.Entries) != 2 {
		t.Errorf("expected 2 entries, got %d", len(patch.Entries))
	}
	if len(patch.Delete) != 1 {
		t.Errorf("expected 1 delete, got %d", len(patch.Delete))
	}

	if patch.Entries[0].Code != "a" || patch.Entries[0].Text != "工" || patch.Entries[0].Weight != 30 {
		t.Errorf("unexpected entry[0]: %+v", patch.Entries[0])
	}
	if patch.Delete[0].Code != "a" || patch.Delete[0].Text != "戈" {
		t.Errorf("unexpected delete[0]: %+v", patch.Delete[0])
	}
}

func TestLoadDictPatch_ShortNames(t *testing.T) {
	dir := t.TempDir()
	patchFile := filepath.Join(dir, "test.dict.patch.yaml")

	content := `---
entries:
  - {c: a, t: 工, w: 30}
  - {c: abcd, t: 新词, w: 100}
  - {c: nihao, t: 你好, w: 500, p: "ni hao"}
delete:
  - {c: a, t: 戈}
`
	if err := os.WriteFile(patchFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	patch, err := LoadDictPatch(patchFile)
	if err != nil {
		t.Fatal(err)
	}

	if len(patch.Entries) != 3 {
		t.Fatalf("expected 3 entries, got %d", len(patch.Entries))
	}
	if patch.Entries[0].Code != "a" || patch.Entries[0].Text != "工" || patch.Entries[0].Weight != 30 {
		t.Errorf("short entry[0] mismatch: %+v", patch.Entries[0])
	}
	if patch.Entries[2].Pinyin != "ni hao" {
		t.Errorf("short entry[2] pinyin mismatch: %q", patch.Entries[2].Pinyin)
	}
	if patch.Delete[0].Code != "a" || patch.Delete[0].Text != "戈" {
		t.Errorf("short delete[0] mismatch: %+v", patch.Delete[0])
	}
}

func TestLoadDictPatch_Empty(t *testing.T) {
	dir := t.TempDir()
	patchFile := filepath.Join(dir, "empty.dict.patch.yaml")

	content := `# 空补丁
---
# entries:
# delete:
`
	if err := os.WriteFile(patchFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	patch, err := LoadDictPatch(patchFile)
	if err != nil {
		t.Fatal(err)
	}
	if !patch.IsEmpty() {
		t.Error("expected empty patch")
	}
}

func TestApplyDictPatch_ModifyWeight(t *testing.T) {
	entries := map[string][]dictEntry{
		"a": {
			{text: "工", weight: 20, naturalOrder: 0},
			{text: "戈", weight: 10, naturalOrder: 1},
		},
	}

	patch := &DictPatch{
		Entries: []DictPatchEntry{
			{Code: "a", Text: "工", Weight: 50},
		},
	}

	logger := slog.Default()
	added, modified, deleted := ApplyDictPatch(entries, nil, patch, logger)

	if added != 0 || modified != 1 || deleted != 0 {
		t.Errorf("expected (0,1,0), got (%d,%d,%d)", added, modified, deleted)
	}
	if entries["a"][0].weight != 50 {
		t.Errorf("expected weight 50, got %d", entries["a"][0].weight)
	}
}

func TestApplyDictPatch_AddEntry(t *testing.T) {
	entries := map[string][]dictEntry{
		"a": {
			{text: "工", weight: 20, naturalOrder: 0},
		},
	}

	patch := &DictPatch{
		Entries: []DictPatchEntry{
			{Code: "abcd", Text: "新词", Weight: 100},
		},
	}

	logger := slog.Default()
	added, modified, deleted := ApplyDictPatch(entries, nil, patch, logger)

	if added != 1 || modified != 0 || deleted != 0 {
		t.Errorf("expected (1,0,0), got (%d,%d,%d)", added, modified, deleted)
	}
	if len(entries["abcd"]) != 1 || entries["abcd"][0].text != "新词" {
		t.Errorf("new entry not found")
	}
}

func TestApplyDictPatch_DeleteEntry(t *testing.T) {
	entries := map[string][]dictEntry{
		"a": {
			{text: "工", weight: 20, naturalOrder: 0},
			{text: "戈", weight: 10, naturalOrder: 1},
		},
	}

	patch := &DictPatch{
		Delete: []DictPatchRef{
			{Code: "a", Text: "戈"},
		},
	}

	logger := slog.Default()
	added, modified, deleted := ApplyDictPatch(entries, nil, patch, logger)

	if added != 0 || modified != 0 || deleted != 1 {
		t.Errorf("expected (0,0,1), got (%d,%d,%d)", added, modified, deleted)
	}
	if len(entries["a"]) != 1 {
		t.Errorf("expected 1 remaining entry, got %d", len(entries["a"]))
	}
	if entries["a"][0].text != "工" {
		t.Errorf("wrong entry remaining: %s", entries["a"][0].text)
	}
}

func TestApplyDictPatch_DeleteLastEntry(t *testing.T) {
	entries := map[string][]dictEntry{
		"xyz": {
			{text: "测试", weight: 10, naturalOrder: 0},
		},
	}

	patch := &DictPatch{
		Delete: []DictPatchRef{
			{Code: "xyz", Text: "测试"},
		},
	}

	logger := slog.Default()
	_, _, deleted := ApplyDictPatch(entries, nil, patch, logger)

	if deleted != 1 {
		t.Errorf("expected 1 deleted, got %d", deleted)
	}
	if _, ok := entries["xyz"]; ok {
		t.Error("expected code 'xyz' to be removed from map")
	}
}

func TestApplyDictPatch_Combined(t *testing.T) {
	entries := map[string][]dictEntry{
		"a": {
			{text: "工", weight: 20, naturalOrder: 0},
			{text: "戈", weight: 10, naturalOrder: 1},
		},
		"bb": {
			{text: "子", weight: 5, naturalOrder: 0},
		},
	}

	patch := &DictPatch{
		Delete: []DictPatchRef{
			{Code: "a", Text: "戈"},
		},
		Entries: []DictPatchEntry{
			{Code: "a", Text: "工", Weight: 50},     // 修改
			{Code: "cc", Text: "新增词", Weight: 100}, // 新增
		},
	}

	logger := slog.Default()
	added, modified, deleted := ApplyDictPatch(entries, nil, patch, logger)

	if added != 1 || modified != 1 || deleted != 1 {
		t.Errorf("expected (1,1,1), got (%d,%d,%d)", added, modified, deleted)
	}

	// 验证修改
	if entries["a"][0].weight != 50 {
		t.Errorf("expected weight 50, got %d", entries["a"][0].weight)
	}
	// 验证删除
	if len(entries["a"]) != 1 {
		t.Errorf("expected 1 entry for 'a', got %d", len(entries["a"]))
	}
	// 验证新增
	if len(entries["cc"]) != 1 || entries["cc"][0].text != "新增词" {
		t.Error("new entry not found")
	}
	// 验证未受影响
	if len(entries["bb"]) != 1 {
		t.Error("unrelated entry was affected")
	}
}

func TestApplyDictPatch_WithAbbrev(t *testing.T) {
	codeEntries := map[string][]dictEntry{
		"nihao": {
			{text: "你好", weight: 1000, naturalOrder: 0},
		},
	}
	abbrevEntries := map[string][]dictEntry{
		"nh": {
			{text: "你好", weight: 1000, naturalOrder: 0},
		},
	}

	patch := &DictPatch{
		Delete: []DictPatchRef{
			{Code: "nihao", Text: "你好"},
		},
		Entries: []DictPatchEntry{
			{Code: "zaijian", Text: "再见", Weight: 500, Pinyin: "zai jian"},
		},
	}

	logger := slog.Default()
	added, _, deleted := ApplyDictPatch(codeEntries, abbrevEntries, patch, logger)

	if added != 1 || deleted != 1 {
		t.Errorf("expected added=1 deleted=1, got added=%d deleted=%d", added, deleted)
	}

	// 验证删除：codeEntries 和 abbrevEntries 都应移除
	if _, ok := codeEntries["nihao"]; ok {
		t.Error("nihao should be deleted from codeEntries")
	}
	if _, ok := abbrevEntries["nh"]; ok {
		t.Error("nh should be deleted from abbrevEntries")
	}

	// 验证新增：codeEntries 和 abbrevEntries 都应有
	if len(codeEntries["zaijian"]) != 1 {
		t.Error("zaijian not added to codeEntries")
	}
	if len(abbrevEntries["zj"]) != 1 || abbrevEntries["zj"][0].text != "再见" {
		t.Error("zj not added to abbrevEntries")
	}
}

func TestFindPatchFiles(t *testing.T) {
	dir := t.TempDir()

	// 创建主词库补丁
	mainPatch := filepath.Join(dir, "main.dict.patch.yaml")
	os.WriteFile(mainPatch, []byte("---\n"), 0644)

	// 创建一个 import 补丁
	importPatch := filepath.Join(dir, "extra.dict.patch.yaml")
	os.WriteFile(importPatch, []byte("---\n"), 0644)

	mainDict := filepath.Join(dir, "main.dict.yaml")
	patches := FindPatchFiles(mainDict, []string{"extra", "nonexist"})

	if len(patches) != 2 {
		t.Errorf("expected 2 patches, got %d: %v", len(patches), patches)
	}
}

func TestFindPatchFiles_NoPatch(t *testing.T) {
	dir := t.TempDir()

	mainDict := filepath.Join(dir, "main.dict.yaml")
	patches := FindPatchFiles(mainDict, []string{"extra"})

	if len(patches) != 0 {
		t.Errorf("expected 0 patches, got %d", len(patches))
	}
}

func TestPatchPath(t *testing.T) {
	tests := []struct {
		input    string
		expected string
	}{
		{"wubi86_jidian.dict.yaml", "wubi86_jidian.dict.patch.yaml"},
		{"foo.txt", "foo.txt.patch.yaml"},
		{"/path/to/base.dict.yaml", "/path/to/base.dict.patch.yaml"},
	}

	for _, tt := range tests {
		got := patchPath(tt.input)
		if got != tt.expected {
			t.Errorf("patchPath(%q) = %q, want %q", tt.input, got, tt.expected)
		}
	}
}

func TestIsEmpty(t *testing.T) {
	var nilPatch *DictPatch
	if !nilPatch.IsEmpty() {
		t.Error("nil patch should be empty")
	}

	emptyPatch := &DictPatch{}
	if !emptyPatch.IsEmpty() {
		t.Error("empty patch should be empty")
	}

	nonEmpty := &DictPatch{
		Entries: []DictPatchEntry{{Code: "a", Text: "b", Weight: 1}},
	}
	if nonEmpty.IsEmpty() {
		t.Error("non-empty patch should not be empty")
	}
}
