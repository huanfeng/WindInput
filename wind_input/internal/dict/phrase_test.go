package dict

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/internal/store"
)

// loadPhraseLayerFromYAML 测试辅助：将 YAML 短语文件种子到 Store 后加载 PhraseLayer
func loadPhraseLayerFromYAML(t *testing.T, systemFile, userFile string) *PhraseLayer {
	t.Helper()

	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "test.db")
	s, err := store.Open(dbPath)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close() })

	// 种子短语到 Store
	var records []store.PhraseRecord
	for _, file := range []struct {
		path     string
		isSystem bool
	}{
		{systemFile, true},
		{userFile, false},
	} {
		if file.path == "" {
			continue
		}
		entries, err := ParsePhraseYAMLFile(file.path)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if e.Code == "" || (e.Text == "" && e.Texts == "") {
				continue
			}
			rec := store.PhraseRecord{
				Code:     strings.ToLower(e.Code),
				Text:     e.Text,
				Texts:    e.Texts,
				Name:     e.Name,
				Type:     detectPhraseType(e),
				Position: e.Position,
				Enabled:  !e.Disabled,
				IsSystem: file.isSystem,
			}
			if rec.Position <= 0 {
				rec.Position = 1
			}
			records = append(records, rec)
		}
	}
	if len(records) > 0 {
		if err := s.SeedPhrases(records); err != nil {
			t.Fatal(err)
		}
	}

	pl := NewPhraseLayerEx("phrases", systemFile, "", s)
	if err := pl.LoadFromStore(s); err != nil {
		t.Fatal(err)
	}
	return pl
}

func TestPhraseLayerSearchCommandMarksIsCommand(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "uuid"
    text: "$uuid"
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	results := pl.SearchCommand("uuid", 10)
	if len(results) == 0 {
		t.Fatal("SearchCommand(uuid) should return candidates")
	}

	for i, c := range results {
		if !c.IsCommand {
			t.Fatalf("candidate[%d] should be marked IsCommand=true", i)
		}
	}
}

func TestPhraseLayerStaticPhrase(t *testing.T) {
	tmpDir := t.TempDir()
	userFile := filepath.Join(tmpDir, "user.phrases.yaml")
	content := `phrases:
  - code: "dz"
    text: "我的地址"
    position: 1
`
	if err := os.WriteFile(userFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	pl := loadPhraseLayerFromYAML(t, "", userFile)

	results := pl.Search("dz", 10)
	if len(results) != 1 {
		t.Fatalf("expected 1 result, got %d", len(results))
	}
	if results[0].Text != "我的地址" {
		t.Fatalf("expected '我的地址', got %q", results[0].Text)
	}
}

func TestPhraseLayerDynamicExpansion(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "rq"
    text: "$Y-$MM-$DD"
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// 动态短语不应出现在 Search 中
	results := pl.Search("rq", 10)
	if len(results) != 0 {
		t.Fatalf("dynamic phrase should not appear in Search, got %d", len(results))
	}

	// 应出现在 SearchCommand 中，且已展开
	cmdResults := pl.SearchCommand("rq", 10)
	if len(cmdResults) == 0 {
		t.Fatal("dynamic phrase should appear in SearchCommand")
	}
	// 展开后不应包含 $
	if cmdResults[0].Text == "$Y-$MM-$DD" {
		t.Fatal("dynamic phrase text should be expanded, not raw template")
	}
}

func TestPhraseLayerGroupSearch(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzys"
    name: "圈数字"
    texts: "①②③④⑤"
    position: 1
  - code: "zzjt"
    name: "箭头符号"
    texts: "→↑←↓"
    position: 2
  - code: "zzrq"
    text: "$Y-$MM-$DD"
    position: 1
  - code: "abc"
    text: "普通短语"
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// 1. SearchPrefix("zz") 应返回组名候选，而非展开字符
	prefixResults := pl.SearchPrefix("zz", 0)
	groupCount := 0
	for _, c := range prefixResults {
		if c.IsGroup {
			groupCount++
		}
	}
	if groupCount != 2 {
		t.Fatalf("expected 2 group candidates for prefix 'zz', got %d (total %d)", groupCount, len(prefixResults))
	}

	// 2. 验证组名和编码
	found := map[string]bool{}
	for _, c := range prefixResults {
		if c.IsGroup {
			found[c.GroupCode] = true
			if c.GroupCode == "zzys" && c.Text != "圈数字" {
				t.Fatalf("expected group name '圈数字', got %q", c.Text)
			}
			if c.GroupCode == "zzjt" && c.Text != "箭头符号" {
				t.Fatalf("expected group name '箭头符号', got %q", c.Text)
			}
		}
	}
	if !found["zzys"] || !found["zzjt"] {
		t.Fatal("missing expected groups in prefix search")
	}

	// 3. Search("zzys") 精确匹配应返回展开的字符
	exactResults := pl.Search("zzys", 0)
	if len(exactResults) != 5 {
		t.Fatalf("expected 5 chars for exact 'zzys', got %d", len(exactResults))
	}
	if exactResults[0].Text != "①" {
		t.Fatalf("expected first char '①', got %q", exactResults[0].Text)
	}

	// 4. SearchPrefix("zz") 不应包含展开的字符候选
	for _, c := range prefixResults {
		if !c.IsGroup && (c.Code == "zzys" || c.Code == "zzjt") {
			t.Fatalf("prefix search should not return expanded chars for group code %q", c.Code)
		}
	}

	// 5. 动态短语（zzrq）仍应出现在 SearchPrefix 但不是组
	// zzrq 是动态短语，不在 staticPhrases 中，SearchPrefix 不返回它
	for _, c := range prefixResults {
		if c.Code == "zzrq" {
			t.Fatal("dynamic phrase zzrq should not appear in SearchPrefix")
		}
	}

	// 6. 普通静态短语前缀搜索仍正常
	abcResults := pl.SearchPrefix("ab", 0)
	if len(abcResults) != 1 || abcResults[0].Text != "普通短语" {
		t.Fatalf("expected normal prefix search to work, got %d results", len(abcResults))
	}
}

func TestPhraseLayerGroupDisabled(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzts"
    name: "特殊符号"
    texts: "℃°‰"
    position: 1
    disabled: true
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// 禁用的组不应出现在前缀搜索中
	results := pl.SearchPrefix("zz", 0)
	for _, c := range results {
		if c.GroupCode == "zzts" {
			t.Fatal("disabled group should not appear in SearchPrefix")
		}
	}

	// 禁用的组也不应有精确匹配结果
	exact := pl.Search("zzts", 0)
	if len(exact) != 0 {
		t.Fatalf("disabled group should not have exact matches, got %d", len(exact))
	}
}

func TestSearchCommandGroupExactMatch(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzbd"
    name: "标点符号"
    texts: "，。！？"
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	results := pl.SearchCommand("zzbd", 0)
	if len(results) != 4 {
		t.Fatalf("expected 4 chars for SearchCommand('zzbd'), got %d", len(results))
	}
	for i, c := range results {
		if !c.IsCommand {
			t.Fatalf("candidate[%d] should have IsCommand=true", i)
		}
		if !c.IsPhrase {
			t.Fatalf("candidate[%d] should have IsPhrase=true", i)
		}
		if c.IsGroup {
			t.Fatalf("candidate[%d] should NOT have IsGroup=true (exact match returns chars)", i)
		}
	}
	// 第一个字符应为 "，"
	if results[0].Text != "，" {
		t.Fatalf("expected first char '，', got %q", results[0].Text)
	}
}

func TestSearchCommandGroupPrefixNavigation(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzbd"
    name: "标点符号"
    texts: "，。！"
    position: 1
  - code: "zzsz"
    name: "数字符号"
    texts: "①②③"
    position: 2
  - code: "abc"
    text: "普通短语"
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// "zz" 前缀应返回两个导航候选
	results := pl.SearchCommand("zz", 0)
	if len(results) != 2 {
		t.Fatalf("expected 2 nav candidates for SearchCommand('zz'), got %d", len(results))
	}
	for i, c := range results {
		if !c.IsGroup {
			t.Fatalf("candidate[%d] should have IsGroup=true", i)
		}
		if c.GroupCode == "" {
			t.Fatalf("candidate[%d] GroupCode should not be empty", i)
		}
	}
	// 确认两个组都出现了
	codes := map[string]bool{}
	for _, c := range results {
		codes[c.GroupCode] = true
	}
	if !codes["zzbd"] || !codes["zzsz"] {
		t.Fatalf("expected groups zzbd and zzsz, got %v", codes)
	}

	// "abc" 前缀不应触发导航（非组）
	abcResults := pl.SearchCommand("ab", 0)
	for _, c := range abcResults {
		if c.IsGroup {
			t.Fatal("non-group prefix should not return IsGroup candidates")
		}
	}
}
