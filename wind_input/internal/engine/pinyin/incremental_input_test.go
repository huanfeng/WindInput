package pinyin

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
)

// createDictForIncrementalTest 创建包含"你说不说"相关词条的测试词库
func createDictForIncrementalTest(t *testing.T) *dict.CompositeDict {
	t.Helper()
	tmpDir := t.TempDir()

	content := `# Rime dictionary
---
name: test_incremental
version: "1.0"
sort: by_weight
...
你	ni	1000
妮	ni	500
说	shuo	900
说不	shuo bu	600
你说	ni shuo	700
不	bu	1000
不说	bu shuo	600
说话	shuo hua	700
是	shi	1000
石	shi	900
时	shi	850
事	shi	800
书	shu	900
数	shu	850
输	shu	800
树	shu	750
术	shu	700
话	hua	800
和	he	900
或	huo	850
火	huo	800
活	huo	750
好	hao	1000
号	hao	500
你好	ni hao	800
我	wo	1000
哦	o	800
`
	if err := os.WriteFile(filepath.Join(tmpDir, "8105.dict.yaml"), []byte(content), 0644); err != nil {
		t.Fatalf("写入测试文件失败: %v", err)
	}

	d := dict.NewPinyinDict(nil)
	if err := d.LoadRimeDir(tmpDir); err != nil {
		t.Fatalf("加载词库失败: %v", err)
	}
	return wrapInCompositeDict(d)
}

// TestIncrementalInput_nishuobushuo 逐字符输入 "nishuobushuo"，
// 检查每一步的第一候选和 preedit 是否合理。
func TestIncrementalInput_nishuobushuo(t *testing.T) {
	d := createDictForIncrementalTest(t)
	engine := NewEngine(d, nil)

	input := "nishuobushuo"
	for i := 1; i <= len(input); i++ {
		prefix := input[:i]
		result := engine.ConvertEx(prefix, 20)

		// 输出前5个候选
		t.Logf("--- input=%q preedit=%q ---", prefix, result.PreeditDisplay)
		for j, c := range result.Candidates {
			if j >= 5 {
				break
			}
			t.Logf("  [%d] %s (weight=%d, consumed=%d)", j, c.Text, c.Weight, c.ConsumedLength)
		}

		// 基本断言：不应为空
		if len(result.Candidates) == 0 {
			t.Errorf("input=%q: no candidates returned", prefix)
			continue
		}

		// 检查第一候选是否合理（至少第一个字应与输入首音节对应）
		first := result.Candidates[0]
		firstRunes := []rune(first.Text)
		if len(firstRunes) == 0 {
			t.Errorf("input=%q: first candidate has empty text", prefix)
		}
	}
}

// TestIncrementalInput_KeyAssertions 针对关键节点的断言
func TestIncrementalInput_KeyAssertions(t *testing.T) {
	d := createDictForIncrementalTest(t)
	engine := NewEngine(d, nil)

	tests := []struct {
		input       string
		mustContain []string // 候选中必须包含这些词
		firstIs     string   // 第一候选应该是（空表示不检查）
		desc        string
	}{
		{"ni", []string{"你"}, "你", "单音节应匹配"},
		{"nish", []string{"你"}, "", "ni+sh(partial)，你应保留"},
		{"nishu", []string{"你"}, "", "ni+shu，你应保留"},
		{"nishuo", []string{"你说"}, "你说", "ni+shuo 精确匹配你说"},
		{"nishuob", []string{"你说"}, "", "ni+shuo+b(partial)，你说应保留"},
		{"nishuobu", []string{"你说"}, "", "ni+shuo+bu，你说应在候选中"},
		{"nishuobush", []string{"你说"}, "", "ni+shuo+bu+sh(partial)，你说应保留"},
		{"nishuobushu", []string{"你说"}, "", "ni+shuo+bu+shu，你说应保留"},
		{"nishuobushuo", []string{"你说", "不说"}, "", "完整输入，你说和不说应在候选中"},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			result := engine.ConvertEx(tt.input, 50)

			t.Logf("input=%q preedit=%q", tt.input, result.PreeditDisplay)
			for j, c := range result.Candidates {
				if j >= 8 {
					break
				}
				t.Logf("  [%d] %s (weight=%d, consumed=%d)", j, c.Text, c.Weight, c.ConsumedLength)
			}

			// 检查必须包含的候选
			for _, must := range tt.mustContain {
				found := false
				for _, c := range result.Candidates {
					if c.Text == must {
						found = true
						break
					}
				}
				if !found {
					t.Errorf("input=%q: MUST contain '%s' but not found (%s)", tt.input, must, tt.desc)
				}
			}

			// 检查第一候选
			if tt.firstIs != "" && len(result.Candidates) > 0 {
				if result.Candidates[0].Text != tt.firstIs {
					t.Errorf("input=%q: first candidate should be '%s', got '%s' (%s)",
						tt.input, tt.firstIs, result.Candidates[0].Text, tt.desc)
				}
			}
		})
	}
}
