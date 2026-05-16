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
			if e.Code == "" || e.Text == "" {
				continue
			}
			rec := store.PhraseRecord{
				Code:     strings.ToLower(e.Code),
				Text:     e.Text,
				Type:     detectPhraseType(e),
				Weight:   resolveWeightFromFileEntry(e),
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
    text: '$AA("圈数字", "①②③④⑤")'
    position: 1
  - code: "zzjt"
    text: '$AA("箭头符号", "→↑←↓")'
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
    text: '$AA("特殊符号", "℃°‰")'
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
    text: '$AA("标点符号", "，。！？")'
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
    text: '$AA("标点符号", "，。！")'
    position: 1
  - code: "zzsz"
    text: '$AA("数字符号", "①②③")'
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

// TestResolvePhraseWeight 覆盖 resolvePhraseWeight 的优先级与边界
// (2026-05-16 后: position 不再参与 weight 计算, 仅作为同 code tie-break):
//   - weight > 0 → 直接用 (clamp 到 10000)
//   - weight <= 0 → 默认 1000 (短语 tier 中位)
func TestResolvePhraseWeight(t *testing.T) {
	cases := []struct {
		name   string
		weight int
		want   int
	}{
		{"explicit weight 3000", 3000, 3000},
		{"explicit weight 5000", 5000, 5000},
		{"explicit weight clamps to max", 99999, 10000},
		{"zero → default 1000", 0, 1000},
		{"negative weight → default 1000", -10, 1000},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got := resolvePhraseWeight(c.weight)
			if got != c.want {
				t.Fatalf("resolvePhraseWeight(%d) = %d, want %d", c.weight, got, c.want)
			}
		})
	}
}

// TestResolveWeightFromFileEntry 验证 *int Weight 字段的"未设置"与"显式 0" 区分
// (2026-05-16 后: position 不再换算为 weight, 仅在 sort 阶段做 tie-break):
//   - Weight=nil + position 任意 → 1000 (默认中位)
//   - Weight=*0 → 0 (显式禁用排序权重)
//   - Weight=*2000 → 2000 (显式)
//   - 空 entry → 1000
func TestResolveWeightFromFileEntry(t *testing.T) {
	zero := 0
	w2000 := 2000

	if got := resolveWeightFromFileEntry(PhraseFileEntry{Position: 1}); got != 1000 {
		t.Fatalf("Weight=nil + position=1 want 1000 (默认, position 不再影响 weight), got %d", got)
	}
	if got := resolveWeightFromFileEntry(PhraseFileEntry{Weight: &zero, Position: 1}); got != 0 {
		t.Fatalf("explicit Weight=0 should yield 0 regardless of position, got %d", got)
	}
	if got := resolveWeightFromFileEntry(PhraseFileEntry{Weight: &w2000, Position: 1}); got != 2000 {
		t.Fatalf("Weight=2000 should ignore position, got %d", got)
	}
	if got := resolveWeightFromFileEntry(PhraseFileEntry{}); got != 1000 {
		t.Fatalf("empty entry should default to 1000, got %d", got)
	}
}

// TestPhraseLayerWeightFieldPriority 验证 yaml 中 weight 字段显式生效:
// 同编码下 weight=8000 > weight=3000 > 默认 (1000)。
// (2026-05-16 后: position 仅做 tie-break, 不再被映射为 9999)。
func TestPhraseLayerWeightFieldPriority(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "ww"
    text: "中等优先级"
    weight: 3000
  - code: "ww"
    text: "高优先级"
    weight: 8000
  - code: "ww"
    text: "默认 (无 weight)"
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	pl := loadPhraseLayerFromYAML(t, systemFile, "")
	results := pl.Search("ww", 10)
	if len(results) != 3 {
		t.Fatalf("expected 3 candidates, got %d", len(results))
	}
	// 8000 > 3000 > 1000(默认)
	wantOrder := []string{"高优先级", "中等优先级", "默认 (无 weight)"}
	for i, w := range wantOrder {
		if results[i].Text != w {
			t.Fatalf("idx %d: want %q, got %q (weight=%d)", i, w, results[i].Text, results[i].Weight)
		}
	}
	if results[0].Weight != 8000 {
		t.Fatalf("高优先级 weight want 8000, got %d", results[0].Weight)
	}
	if results[1].Weight != 3000 {
		t.Fatalf("中等优先级 weight want 3000, got %d", results[1].Weight)
	}
	if results[2].Weight != 1000 {
		t.Fatalf("默认 weight want 1000, got %d", results[2].Weight)
	}
}

// TestPhraseLayerArrayGroupNaturalOrder 验证字符组展开:
// $AA("test", "abc") + weight=3000 → 三个字符候选 weight 都是 3000,
// NaturalOrder = 0/1/2, 排序按数组顺序。
func TestPhraseLayerArrayGroupNaturalOrder(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "tt"
    text: '$AA("test", "abc")'
    weight: 3000
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	pl := loadPhraseLayerFromYAML(t, systemFile, "")
	results := pl.SearchCommand("tt", 10)
	if len(results) != 3 {
		t.Fatalf("expected 3 candidates, got %d", len(results))
	}
	wantTexts := []string{"a", "b", "c"}
	for i, w := range wantTexts {
		if results[i].Text != w {
			t.Fatalf("idx %d: want %q, got %q", i, w, results[i].Text)
		}
		if results[i].Weight != 3000 {
			t.Fatalf("idx %d: weight want 3000, got %d", i, results[i].Weight)
		}
		if results[i].NaturalOrder != i {
			t.Fatalf("idx %d: NaturalOrder want %d, got %d", i, i, results[i].NaturalOrder)
		}
	}
}

// TestPhraseLayerLegacyPositionStillWorks 验证旧 db 兼容性:
// PhraseRecord 中 Weight=0 + Position 旧字段下 — 2026-05-16 后两条都映射为
// weight=1000 (默认), position 作为同 code tie-break (升序), 旧条目1 (position=1)
// 仍排在 旧条目2 (position=5) 之前。
func TestPhraseLayerLegacyPositionStillWorks(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "legacy.db")
	s, err := store.Open(dbPath)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close() })

	// 直接种子旧记录: Weight 字段缺失 (零值), 仅有 Position
	if err := s.SeedPhrases([]store.PhraseRecord{
		{Code: "leg", Text: "旧条目1", Type: "static", Position: 1, Enabled: true, IsSystem: true},
		{Code: "leg", Text: "旧条目2", Type: "static", Position: 5, Enabled: true, IsSystem: true},
	}); err != nil {
		t.Fatal(err)
	}

	pl := NewPhraseLayerEx("phrases", "", "", s)
	if err := pl.LoadFromStore(s); err != nil {
		t.Fatal(err)
	}

	results := pl.Search("leg", 10)
	if len(results) != 2 {
		t.Fatalf("expected 2 results, got %d", len(results))
	}
	// weight 都是 1000 (默认); position tie-break: 1 < 5, 旧条目1 在前。
	if results[0].Text != "旧条目1" {
		t.Fatalf("expected 旧条目1 first (position=1), got %q", results[0].Text)
	}
	if results[0].Weight != 1000 {
		t.Fatalf("旧条目1 weight want 1000 (默认), got %d", results[0].Weight)
	}
	if results[1].Weight != 1000 {
		t.Fatalf("旧条目2 weight want 1000 (默认), got %d", results[1].Weight)
	}
}
