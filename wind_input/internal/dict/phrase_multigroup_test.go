package dict

import (
	"os"
	"path/filepath"
	"testing"
)

// TestPhraseLayer_MultipleAAGroupsSameCode 验证同 code 多个 $AA group:
//   - LoadFromStore 不再覆盖 (phraseGroups[code] 是 slice)
//   - SearchCommand 精确码命中时返回所有 group 的成员候选, 每条带自己 GroupTemplate
//   - 上层 (coordinator) collapse 自动按 GroupTemplate 区分多 nav
//
// 详见 docs/design/candidate-actions.md §5。
func TestPhraseLayer_MultipleAAGroupsSameCode(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzbd"
    text: '$AA("标点A", "，。")'
    position: 1
  - code: "zzbd"
    text: '$AA("标点B", "！？")'
    position: 2
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	// PhraseGroup 切片应该有 2 条 (不被覆盖)
	groups := pl.phraseGroups["zzbd"]
	if len(groups) != 2 {
		t.Fatalf("expected 2 groups under zzbd, got %d", len(groups))
	}

	// SearchCommand 精确码: 返回 2+2=4 个成员候选 (两组各 2 字)
	got := pl.SearchCommand("zzbd", 0)
	if len(got) != 4 {
		t.Fatalf("expected 4 candidates (2 from each group), got %d", len(got))
	}

	// 每个候选的 GroupTemplate 应该对应所属 group 的 RawText
	tplA := `$AA("标点A", "，。")`
	tplB := `$AA("标点B", "！？")`
	countA, countB := 0, 0
	for _, c := range got {
		switch c.GroupTemplate {
		case tplA:
			countA++
			if c.GroupName != "标点A" {
				t.Errorf("expected GroupName=标点A, got %q", c.GroupName)
			}
		case tplB:
			countB++
			if c.GroupName != "标点B" {
				t.Errorf("expected GroupName=标点B, got %q", c.GroupName)
			}
		default:
			t.Errorf("unexpected GroupTemplate %q", c.GroupTemplate)
		}
	}
	if countA != 2 || countB != 2 {
		t.Errorf("expected 2 members per group, got A=%d B=%d", countA, countB)
	}
}

// TestPhraseLayer_MultipleAAGroups_PrefixNav 验证前缀场景: zz → zzbd 多 group
// 应该返回多个 nav 候选 (每个 group 一条), 而不是合并成一个。
func TestPhraseLayer_MultipleAAGroups_PrefixNav(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "zzbd"
    text: '$AA("标点A", "，。")'
    position: 1
  - code: "zzbd"
    text: '$AA("标点B", "！？")'
    position: 2
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")

	got := pl.SearchCommand("zz", 0) // 前缀 nav 路径
	if len(got) != 2 {
		t.Fatalf("expected 2 nav candidates (one per group), got %d: %+v", len(got), got)
	}
	names := map[string]bool{}
	for _, c := range got {
		if !c.IsGroup {
			t.Errorf("expected IsGroup=true, got %+v", c)
		}
		names[c.GroupName] = true
	}
	if !names["标点A"] || !names["标点B"] {
		t.Errorf("expected both group names, got %v", names)
	}
}

// TestPhraseLayer_MixedAAandSSSameCode 验证同 code 混合 $AA + $SS 时, 两个 group 都注册。
func TestPhraseLayer_MixedAAandSSSameCode(t *testing.T) {
	tmpDir := t.TempDir()
	systemFile := filepath.Join(tmpDir, "system.phrases.yaml")
	content := `phrases:
  - code: "mix"
    text: '$AA("字符", "ab")'
    position: 1
  - code: "mix"
    text: '$SS("字符串", "alpha", "beta")'
    position: 2
`
	if err := os.WriteFile(systemFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	pl := loadPhraseLayerFromYAML(t, systemFile, "")
	groups := pl.phraseGroups["mix"]
	if len(groups) != 2 {
		t.Fatalf("expected 2 groups (AA+SS), got %d", len(groups))
	}
	var hasAA, hasSS bool
	for _, g := range groups {
		switch g.Kind {
		case PhraseGroupKindAA:
			hasAA = true
		case PhraseGroupKindSS:
			hasSS = true
		}
	}
	if !hasAA || !hasSS {
		t.Errorf("expected both AA and SS group, got hasAA=%v hasSS=%v", hasAA, hasSS)
	}
}
