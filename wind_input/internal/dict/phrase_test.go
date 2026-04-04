package dict

import (
	"os"
	"path/filepath"
	"testing"
)

func TestPhraseLayerSearchCommandMarksIsCommand(t *testing.T) {
	// 创建临时系统短语文件，包含一个动态短语（含 $uuid 变量）
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

	pl := NewPhraseLayer("phrases", systemFile, "")
	if err := pl.Load(); err != nil {
		t.Fatal(err)
	}

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

	pl := NewPhraseLayer("phrases", "", userFile)
	if err := pl.Load(); err != nil {
		t.Fatal(err)
	}

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

	pl := NewPhraseLayer("phrases", systemFile, "")
	if err := pl.Load(); err != nil {
		t.Fatal(err)
	}

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

	pl := NewPhraseLayer("phrases", systemFile, "")
	if err := pl.Load(); err != nil {
		t.Fatal(err)
	}

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

	pl := NewPhraseLayer("phrases", systemFile, "")
	if err := pl.Load(); err != nil {
		t.Fatal(err)
	}

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
