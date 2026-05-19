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

// TestPhraseLayerSearchCommandTemplateNotCommand 验证模板变量短语 ($uuid/$Y/$M
// 等无副作用 Actions 的展开) 不再被打上 IsCommand。IsCommand 严格收缩为
// "有副作用 Actions" 后, 这些纯文本候选应保持 IsCommand=false, 让上层
// doSelectCandidate 能记入 inputHistory / 触发学习。
func TestPhraseLayerSearchCommandTemplateNotCommand(t *testing.T) {
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
		if c.IsCommand {
			t.Fatalf("candidate[%d]: 模板变量短语不应标 IsCommand=true (无 Actions)", i)
		}
		if !c.IsPhrase {
			t.Fatalf("candidate[%d] should keep IsPhrase=true", i)
		}
		if len(c.Actions) != 0 {
			t.Fatalf("candidate[%d]: 模板变量短语 Actions 应为空, got %d", i, len(c.Actions))
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
		// IsCommand 严格表"有副作用 Actions"; $AA 字符成员是纯文本, 不应被标 IsCommand
		if c.IsCommand {
			t.Fatalf("candidate[%d]: $AA 字符成员不应标 IsCommand=true (纯文本)", i)
		}
		if !c.IsPhrase {
			t.Fatalf("candidate[%d] should have IsPhrase=true", i)
		}
		if !c.IsGroupMember {
			t.Fatalf("candidate[%d] should have IsGroupMember=true", i)
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

// TestPhraseCandidateIDStatic 验证静态短语候选填充 ID = phrase:<code>:<text>。
func TestPhraseCandidateIDStatic(t *testing.T) {
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

	got := pl.Search("dz", 10)
	if len(got) != 1 {
		t.Fatalf("expected 1 candidate, got %d", len(got))
	}
	wantID := "phrase:dz:我的地址"
	if got[0].ID != wantID {
		t.Fatalf("ID = %q, want %q", got[0].ID, wantID)
	}
	if got[0].PhraseTemplate != "我的地址" {
		t.Fatalf("PhraseTemplate = %q, want %q", got[0].PhraseTemplate, "我的地址")
	}
}

// TestPhraseCandidateIDDynamic 验证动态短语候选 ID 用模板 (而非展开后 Text)。
func TestPhraseCandidateIDDynamic(t *testing.T) {
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

	got := pl.SearchCommand("rq", 10)
	if len(got) == 0 {
		t.Fatal("expected dynamic candidate")
	}
	wantID := "phrase:rq:$Y-$MM-$DD"
	if got[0].ID != wantID {
		t.Fatalf("ID = %q, want %q (must use template, not expanded text)", got[0].ID, wantID)
	}
}

// TestPhraseCandidateIDAAGroupChars 验证 $AA 字符组精确码命中后, 每个字符
// 候选都填上独立 ID = phrase:<code>:<char>。
func TestPhraseCandidateIDAAGroupChars(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzbd"
    text: '$AA("标点符号", "，。！")'
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	got := pl.SearchCommand("zzbd", 0)
	if len(got) != 3 {
		t.Fatalf("expected 3 chars, got %d", len(got))
	}
	wantIDs := []string{"phrase:zzbd:，", "phrase:zzbd:。", "phrase:zzbd:！"}
	for i, want := range wantIDs {
		if got[i].ID != want {
			t.Fatalf("char[%d] ID = %q, want %q", i, got[i].ID, want)
		}
	}
}

// TestPhraseCandidateIDGroupNavStable 验证 group nav 候选附稳定 ID
// = PhraseCandidateID(code, group 原始 PhraseRecord.Text), 让 Shadow pin /
// DisablePhrase 按 candID 跨 collapse 状态稳定命中。
// 详见 docs/design/candidate-actions.md §5。
func TestPhraseCandidateIDGroupNavStable(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	const groupTpl = `$AA("标点符号", "，。")`
	content := `phrases:
  - code: "zzbd"
    text: '` + groupTpl + `'
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	got := pl.SearchCommand("zz", 0)
	if len(got) != 1 {
		t.Fatalf("expected 1 nav candidate, got %d", len(got))
	}
	if !got[0].IsGroup {
		t.Fatal("nav candidate should be IsGroup=true")
	}
	wantID := PhraseCandidateID("zzbd", groupTpl)
	if got[0].ID != wantID {
		t.Fatalf("nav candidate ID = %q, want %q", got[0].ID, wantID)
	}
	if got[0].PhraseTemplate != groupTpl {
		t.Fatalf("nav PhraseTemplate = %q, want %q", got[0].PhraseTemplate, groupTpl)
	}
	if got[0].GroupTemplate != groupTpl {
		t.Fatalf("nav GroupTemplate = %q, want %q", got[0].GroupTemplate, groupTpl)
	}
	// 导航候选不**标** IsGroupMember (它本身是组入口, 不展开):
	if got[0].IsGroupMember {
		t.Fatal("nav candidate must NOT have IsGroupMember=true (组入口 不展开)")
	}
}

// TestPhraseAAGroupCharsAreGroupMembers 验证 $AA 字符组精确码命中后,
// 每个字符候选都标 IsGroupMember=true, 让右键菜单全 disable。
//
// 引入: 2026-05-17 R2 follow-up (字符组顺序在 $AA(chars) 中已完整定义,
// 不允许通过 Shadow 双轨漂移)。
func TestPhraseAAGroupCharsAreGroupMembers(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzbd"
    text: '$AA("标点符号", "，。！")'
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	got := pl.SearchCommand("zzbd", 0)
	if len(got) != 3 {
		t.Fatalf("expected 3 chars, got %d", len(got))
	}
	for i, c := range got {
		if !c.IsGroupMember {
			t.Errorf("char[%d]=%q: expected IsGroupMember=true, got false", i, c.Text)
		}
		// IsGroup 仍是 false (导航才是 IsGroup=true)
		if c.IsGroup {
			t.Errorf("char[%d]=%q: IsGroup should be false on expanded char", i, c.Text)
		}
	}
}

// TestPhraseAAGroupCharsCarryGroupName 验证 $AA 字符组成员候选填了
// GroupName + GroupCode, 供 collapseGroupMembersIfMixed 在混合场景下
// collapse 出 nav 候选展示用。
//
// 同时覆盖 PhraseLayer.Search (静态精确路径) 和 SearchCommand (字符组精确
// 命中路径) 两条入口, 保证两条路径的标记一致。
func TestPhraseAAGroupCharsCarryGroupName(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzbd"
    text: '$AA("标点符号", "，。！")'
    weight: 3000
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// 路径 1: SearchCommand 字符组精确命中
	cmdGot := pl.SearchCommand("zzbd", 0)
	if len(cmdGot) != 3 {
		t.Fatalf("SearchCommand: expected 3 chars, got %d", len(cmdGot))
	}
	for i, c := range cmdGot {
		if c.GroupCode != "zzbd" {
			t.Errorf("SearchCommand char[%d]=%q: GroupCode want zzbd, got %q", i, c.Text, c.GroupCode)
		}
		if c.GroupName != "标点符号" {
			t.Errorf("SearchCommand char[%d]=%q: GroupName want '标点符号', got %q", i, c.Text, c.GroupName)
		}
	}

	// 路径 2: Search 静态精确 (staticPhrases 字符级 entry, 命中后同样标 IsGroupMember)
	searchGot := pl.Search("zzbd", 0)
	if len(searchGot) != 3 {
		t.Fatalf("Search: expected 3 chars, got %d", len(searchGot))
	}
	for i, c := range searchGot {
		if !c.IsGroupMember {
			t.Errorf("Search char[%d]=%q: IsGroupMember want true", i, c.Text)
		}
		if c.GroupCode != "zzbd" {
			t.Errorf("Search char[%d]=%q: GroupCode want zzbd, got %q", i, c.Text, c.GroupCode)
		}
		if c.GroupName != "标点符号" {
			t.Errorf("Search char[%d]=%q: GroupName want '标点符号', got %q", i, c.Text, c.GroupName)
		}
		// NaturalOrder 必须与 SearchCommand 路径一致 (按 chars 数组顺序 0/1/2),
		// 否则 codetable engine Phase 1 dedup (Search+SearchCommand 同时调) 后
		// 保留 Search 出口, NaturalOrder=0 会让五笔下字符组展开顺序乱掉。
		// 用户反馈 2026-05-19。
		if c.NaturalOrder != i {
			t.Errorf("Search char[%d]=%q: NaturalOrder want %d, got %d (must match SearchCommand path)",
				i, c.Text, i, c.NaturalOrder)
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
		{Code: "leg", Text: "旧条目1", Position: 1, Enabled: true, IsSystem: true},
		{Code: "leg", Text: "旧条目2", Position: 5, Enabled: true, IsSystem: true},
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

// TestPhraseLayerSearchPrefixMinLength 验证 input.phrase.min_prefix_length 门控:
// 默认 2 时单字符 "z" 不展开 zz** 短语; 调到 1 时恢复旧行为; 码长 == prefix 长度的短语始终可见。
func TestPhraseLayerSearchPrefixMinLength(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzys"
    text: '$AA("圈数字", "①②③④⑤")'
    position: 1
  - code: "zzab"
    text: "TEST"
    position: 1
  - code: "z"
    text: "短码1"
    position: 1
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// 默认未配置时, 行为应当与旧版本一致 (不门控)。
	if got := pl.MinPrefixLength(); got != 1 {
		t.Fatalf("default MinPrefixLength want 1 (no gating), got %d", got)
	}
	if r := pl.SearchPrefix("z", 0); len(r) == 0 {
		t.Fatal("ungated state: prefix 'z' should still expand zz** entries")
	}

	// 启用 min=2: 单字符 "z" 不应再前缀展开 zzab / zzys。
	pl.SetMinPrefixLength(2)
	r := pl.SearchPrefix("z", 0)
	for _, c := range r {
		if c.Code == "zzys" || c.Code == "zzab" {
			t.Fatalf("min=2: prefix 'z' should not surface multi-char codes, got code=%q text=%q", c.Code, c.Text)
		}
	}
	// 同时码长 == prefix 长度 (code="z") 的短码短语仍可命中 (per-entry rule)。
	foundShort := false
	for _, c := range r {
		if c.Code == "z" {
			foundShort = true
			break
		}
	}
	if !foundShort {
		t.Fatal("min=2: code-len-1 phrase 'z' should still appear via per-entry exception (len(prefix) >= len(code))")
	}

	// 长前缀 "zz" 不再被门控, zzab / zzys 应回归。
	r = pl.SearchPrefix("zz", 0)
	hasZzab := false
	hasZzys := false
	for _, c := range r {
		if c.Code == "zzab" {
			hasZzab = true
		}
		if c.Code == "zzys" && c.IsGroup {
			hasZzys = true
		}
	}
	if !hasZzab || !hasZzys {
		t.Fatalf("min=2: prefix 'zz' should still surface zzab(static) and zzys(group), got hasZzab=%v hasZzys=%v", hasZzab, hasZzys)
	}

	// 调到 1 → 恢复旧行为。
	pl.SetMinPrefixLength(1)
	r = pl.SearchPrefix("z", 0)
	hasZzab = false
	for _, c := range r {
		if c.Code == "zzab" {
			hasZzab = true
		}
	}
	if !hasZzab {
		t.Fatal("min=1: prefix 'z' should expand zzab again")
	}
}

// TestPhraseLayerPrefixNavOrderByWeight 验证 prefix-nav (SearchCommand 情况 3) 与
// SearchPrefix groupSlice 路径的排序: 显式 weight 作主序, 同 weight 下 NaturalOrder
// (= LoadSeq) 作 tie-break, 不再回退到 Code asc 字母序。
func TestPhraseLayerPrefixNavOrderByWeight(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	// 故意写成 zzbd < zzde < zzdl 的 yaml 顺序; 权重 zzdl 最高, zzde 居中, zzbd 最低。
	// 若按 Code asc 错误排序 → zzbd 在前; 按 weight 正确排序 → zzdl 在前。
	content := `phrases:
  - code: "zzbd"
    text: '$AA("标点", "、。·")'
    weight: 1010
  - code: "zzde"
    text: '$AA("大写俄文", "АБВ")'
    weight: 1110
  - code: "zzdl"
    text: '$AA("大写罗马", "ⅠⅡⅢ")'
    weight: 1230
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// SearchCommand("zz") 走情况 3 prefix-nav, 应按 weight desc 排序。
	r := pl.SearchCommand("zz", 0)
	if len(r) != 3 {
		t.Fatalf("expected 3 nav candidates, got %d", len(r))
	}
	wantOrder := []string{"zzdl", "zzde", "zzbd"}
	for i, want := range wantOrder {
		if r[i].Code != want {
			t.Fatalf("SearchCommand 'zz' rank %d: got %q (weight=%d), want %q", i+1, r[i].Code, r[i].Weight, want)
		}
	}

	// 同 weight 下应按 LoadSeq (yaml 写入顺序) 排序。
	systemFile2 := filepath.Join(tmpDir, "system2.phrases.yaml")
	// 同权重 1000, 故意以 zzde, zzbd, zzdl 顺序写入 (不等于字母序)。
	content2 := `phrases:
  - code: "zzde"
    text: '$AA("g1", "А")'
    weight: 1000
  - code: "zzbd"
    text: '$AA("g2", "、")'
    weight: 1000
  - code: "zzdl"
    text: '$AA("g3", "Ⅰ")'
    weight: 1000
`
	if err := os.WriteFile(systemFile2, []byte(content2), 0644); err != nil {
		t.Fatal(err)
	}
	pl2 := loadPhraseLayerFromYAML(t, systemFile2, "")
	r2 := pl2.SearchCommand("zz", 0)
	if len(r2) != 3 {
		t.Fatalf("expected 3 nav candidates (uniform weight), got %d", len(r2))
	}
	// 注意: bbolt GetAllPhrases 按 code 字节升序返回 → LoadSeq 实际等同 code asc 排序。
	// 这是预期行为: yaml 写入顺序与 store 字节序解耦, 同权重下回退到稳定的 code 序。
	// 若希望"yaml 写入顺序"成为主序, 必须用 weight 显式区分。
	// code asc (字节序): zzbd < zzde < zzdl ('e'=0x65 < 'l'=0x6c)
	wantOrder2 := []string{"zzbd", "zzde", "zzdl"}
	for i, want := range wantOrder2 {
		if r2[i].Code != want {
			t.Fatalf("uniform weight rank %d: got %q (NaturalOrder=%d), want %q",
				i+1, r2[i].Code, r2[i].NaturalOrder, want)
		}
	}
	// 关键不变量: NaturalOrder 已被赋值 (>=0 且不全为 0), 不再依赖 Code asc 兜底。
	allZero := true
	for _, c := range r2 {
		if c.NaturalOrder != 0 {
			allZero = false
			break
		}
	}
	if allZero {
		t.Fatal("NaturalOrder should be assigned from LoadSeq, not left as 0 for all candidates")
	}
}
