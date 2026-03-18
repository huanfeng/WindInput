package pinyin

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
)

// createDictForNizhibuzhidao 创建覆盖"你知不知道"相关词条的测试词库
func createDictForNizhibuzhidao(t *testing.T) *dict.CompositeDict {
	t.Helper()
	tmpDir := t.TempDir()

	content := `# Rime dictionary
---
name: test_nizhibuzhidao
version: "1.0"
sort: by_weight
...
你	ni	1000
妮	ni	500
知	zhi	1000
之	zhi	900
织	zhi	800
知道	zhi dao	800
知不知道	zhi bu zhi dao	600
不	bu	1000
不知道	bu zhi dao	700
不知	bu zhi	600
道	dao	1000
到	dao	900
的	de	1000
得	de	900
大	da	1000
你知道	ni zhi dao	700
你知	ni zhi	600
你不	ni bu	500
知不知	zhi bu zhi	500
我	wo	1000
好	hao	1000
是	shi	1000
说	shuo	900
`
	if err := os.WriteFile(filepath.Join(tmpDir, "8105.dict.yaml"), []byte(content), 0644); err != nil {
		t.Fatalf("写入测试文件失败: %v", err)
	}

	d := dict.NewPinyinDict()
	if err := d.LoadRimeDir(tmpDir); err != nil {
		t.Fatalf("加载词库失败: %v", err)
	}
	return wrapInCompositeDict(d)
}

// TestIncrementalInput_nizhibuzhidao 逐字符输入 "nizhibuzhidao"，
// 验证每一步的候选排序和 ConsumedLength 合理性。
func TestIncrementalInput_nizhibuzhidao(t *testing.T) {
	d := createDictForNizhibuzhidao(t)
	engine := NewEngine(d)

	input := "nizhibuzhidao"
	for i := 1; i <= len(input); i++ {
		prefix := input[:i]
		result := engine.ConvertEx(prefix, 30)

		t.Logf("--- input=%q preedit=%q ---", prefix, result.PreeditDisplay)
		for j, c := range result.Candidates {
			if j >= 8 {
				break
			}
			t.Logf("  [%d] %s (weight=%d, consumed=%d)", j, c.Text, c.Weight, c.ConsumedLength)
		}

		if len(result.Candidates) == 0 {
			t.Errorf("input=%q: no candidates", prefix)
		}
	}
}

// TestNizhibuzhidao_KeyAssertions 关键节点断言
func TestNizhibuzhidao_KeyAssertions(t *testing.T) {
	d := createDictForNizhibuzhidao(t)
	engine := NewEngine(d)

	tests := []struct {
		input         string
		mustContain   []string
		mustNotFirst  []string       // 这些词不应是第一候选
		consumedCheck map[string]int // text → 期望的最大 ConsumedLength
		desc          string
	}{
		{
			input:       "ni",
			mustContain: []string{"你"},
			desc:        "单音节完整匹配",
		},
		{
			input:       "nizhi",
			mustContain: []string{"你知", "你", "知"},
			desc:        "ni+zhi，你知应在候选中",
		},
		{
			input:       "nizhib",
			mustContain: []string{"你知", "你"},
			desc:        "ni+zhi+b(partial)，你知应保留",
			consumedCheck: map[string]int{
				"你知": 5, // consumed=len("nizhi")=5，不应包含 partial "b"
				"你":  2, // consumed=len("ni")=2
			},
		},
		{
			input:       "nizhibu",
			mustContain: []string{"你知", "你", "不"},
			desc:        "ni+zhi+bu，你知和不应在候选中",
			consumedCheck: map[string]int{
				"你知": 5, // consumed=len("nizhi")=5
				"你":  2,
			},
		},
		{
			input:       "nizhibuzh",
			mustContain: []string{"你知", "你"},
			desc:        "ni+zhi+bu+zh(partial)，你知应保留",
			consumedCheck: map[string]int{
				"你知": 5,
				"你":  2,
			},
		},
		{
			input:       "nizhibuzhi",
			mustContain: []string{"你知", "知不知", "不知"},
			desc:        "ni+zhi+bu+zhi，子词组应丰富",
			consumedCheck: map[string]int{
				"你知": 5,
				"你":  2,
			},
		},
		{
			input:        "nizhibuzhid",
			mustContain:  []string{"你知", "知不知"},
			mustNotFirst: []string{"的", "得", "大"}, // partial "d" 展开的单字不应排第一
			desc:         "ni+zhi+bu+zhi+d(partial)，子词组应保留，partial单字不排首位（注：不知道/知道在dao未完成时无法匹配）",
			consumedCheck: map[string]int{
				"你知": 5,
				"你":  2,
			},
		},
		{
			input:        "nizhibuzhida",
			mustContain:  []string{"你知", "知不知"},
			mustNotFirst: []string{"的", "得", "大"},
			desc:         "ni+zhi+bu+zhi+da，知道在da≠dao时无法匹配",
		},
		{
			input:       "nizhibuzhidao",
			mustContain: []string{"知不知道", "不知道", "知道", "你知"},
			desc:        "完整输入，连续子词组必须全部在候选中（注：你知道跨越bu+zhi不连续，无法通过子词组匹配）",
			consumedCheck: map[string]int{
				"知不知道": 13, // 消耗全部（非首位子词组）
				"不知道":  13, // 消耗全部（非首位子词组）
				"知道":   13, // 消耗全部（非首位子词组）
				"你知":   5,  // 从首位开始的子词组
				"你":    2,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			result := engine.ConvertEx(tt.input, 50)

			t.Logf("input=%q preedit=%q", tt.input, result.PreeditDisplay)
			for j, c := range result.Candidates {
				if j >= 10 {
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
					t.Errorf("MUST contain '%s' but not found (%s)", must, tt.desc)
				}
			}

			// 检查不应排首位的候选
			if len(result.Candidates) > 0 && len(tt.mustNotFirst) > 0 {
				first := result.Candidates[0].Text
				for _, bad := range tt.mustNotFirst {
					if first == bad {
						t.Errorf("'%s' should NOT be first candidate (%s)", bad, tt.desc)
					}
				}
			}

			// 检查 ConsumedLength
			for text, maxConsumed := range tt.consumedCheck {
				for _, c := range result.Candidates {
					if c.Text == text {
						if c.ConsumedLength > maxConsumed {
							t.Errorf("'%s' ConsumedLength=%d, want <= %d (%s)",
								text, c.ConsumedLength, maxConsumed, tt.desc)
						}
						break
					}
				}
			}
		})
	}
}

// TestConsumedLengthNeverExceedsInput 所有候选的 ConsumedLength 不应超过输入长度
func TestConsumedLengthNeverExceedsInput(t *testing.T) {
	d := createDictForNizhibuzhidao(t)
	engine := NewEngine(d)

	inputs := []string{
		"n", "ni", "niz", "nizh", "nizhi",
		"nizhib", "nizhibu", "nizhibuz", "nizhibuzh", "nizhibuzhi",
		"nizhibuzhid", "nizhibuzhida", "nizhibuzhidao",
		"wobuzhidao", "wobuzhidaog",
	}

	for _, input := range inputs {
		result := engine.ConvertEx(input, 50)
		for _, c := range result.Candidates {
			if c.ConsumedLength > len(input) {
				t.Errorf("input=%q: '%s' ConsumedLength=%d exceeds input length %d",
					input, c.Text, c.ConsumedLength, len(input))
			}
			if c.ConsumedLength <= 0 {
				t.Errorf("input=%q: '%s' ConsumedLength=%d should be > 0",
					input, c.Text, c.ConsumedLength)
			}
		}
	}
	fmt.Println("All ConsumedLength values within bounds")
}
