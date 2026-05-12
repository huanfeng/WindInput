package pinyin

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
)

// getRealDictDir 返回真实词库目录路径。
// 从项目根目录 build/dict/pinyin/ 加载完整词库。
// 若词库不存在则跳过测试（CI 环境可能没有词库）。
func getRealDictDir(t *testing.T) string {
	t.Helper()

	// 从测试文件位置向上推导项目根目录
	_, filename, _, _ := runtime.Caller(0)
	// filename = .../wind_input/internal/engine/pinyin/realdict_test.go
	pinyinDir := filepath.Dir(filename)
	projectRoot := filepath.Join(pinyinDir, "..", "..", "..", "..")

	// 候选目录：优先 build 产物，回退到 .cache（开发环境）
	candidates := []string{
		filepath.Join(projectRoot, "build", "dict", "pinyin"),
		filepath.Join(projectRoot, ".cache", "rime-frost", "cn_dicts"),
	}
	for _, dir := range candidates {
		if _, err := os.Stat(filepath.Join(dir, "8105.dict.yaml")); err == nil {
			return dir
		}
	}
	t.Skipf("Real dictionary not found in any of %v, skipping", candidates)
	return ""
}

// loadRealDict 加载完整生产词库
func loadRealDict(t *testing.T) *dict.CompositeDict {
	t.Helper()
	dictDir := getRealDictDir(t)

	d := dict.NewPinyinDict(nil)
	if err := d.LoadRimeDir(dictDir); err != nil {
		t.Fatalf("加载真实词库失败: %v", err)
	}
	t.Logf("Loaded real dict from %s", dictDir)
	return wrapInCompositeDict(d)
}

// loadRealEngine 加载完整生产词库 + unigram LM，匹配真实运行环境
func loadRealEngine(t *testing.T) *Engine {
	t.Helper()
	d := loadRealDict(t)
	engine := NewEngine(d, nil)

	dictDir := getRealDictDir(t)
	unigramPath := filepath.Join(dictDir, "unigram.txt")
	if _, err := os.Stat(unigramPath); err == nil {
		if err := engine.LoadUnigram(unigramPath); err != nil {
			t.Logf("Warning: failed to load unigram: %v", err)
		} else {
			t.Logf("Loaded unigram from %s", unigramPath)
		}
	}
	return engine
}

// TestRealDict_IncrementalInput_nizhibuzhidao 使用真实词库逐字符输入
func TestRealDict_IncrementalInput_nizhibuzhidao(t *testing.T) {
	d := loadRealDict(t)
	engine := NewEngine(d, nil)

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
	}
}

// TestRealDict_IncrementalInput_wobuzhidao 使用真实词库逐字符输入
func TestRealDict_IncrementalInput_wobuzhidao(t *testing.T) {
	d := loadRealDict(t)
	engine := NewEngine(d, nil)

	input := "wobuzhidao"
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
	}
}

// TestRealDict_KeyAssertions 使用真实词库的关键断言
func TestRealDict_KeyAssertions(t *testing.T) {
	d := loadRealDict(t)
	engine := NewEngine(d, nil)

	tests := []struct {
		input        string
		mustContain  []string // 候选中必须包含
		mustNotFirst []string // 不应为第一候选
		firstOneOf   []string // 第一候选应是其中之一（空表示不检查）
		desc         string
	}{
		// --- wobuzhidao 序列 ---
		{
			input:       "wo",
			mustContain: []string{"我"},
			firstOneOf:  []string{"我"},
			desc:        "wo → 我",
		},
		{
			input:       "wobu",
			mustContain: []string{"我", "我不"},
			desc:        "wo+bu",
		},
		{
			input:        "wobuzhi",
			mustContain:  []string{"我"},
			mustNotFirst: []string{"在", "这", "的"},
			desc:         "wo+bu+zhi，不相关单字不应排首位",
		},
		{
			input:        "wobuzhid",
			mustContain:  []string{"我"},
			mustNotFirst: []string{"的", "得", "大"},
			desc:         "wo+bu+zhi+d(partial)，partial展开单字不应排首位",
		},
		{
			input:       "wobuzhidao",
			mustContain: []string{"我不知道"},
			firstOneOf:  []string{"我不知道"},
			desc:        "完整输入，我不知道应是首选",
		},

		// --- nizhibuzhidao 序列 ---
		{
			input:       "nizhi",
			mustContain: []string{"你知"},
			desc:        "ni+zhi",
		},
		{
			input:        "nizhibuz",
			mustContain:  []string{"你"},
			mustNotFirst: []string{"在", "字", "自"},
			desc:         "partial z 展开的单字不应排首位",
		},
		{
			input:        "nizhibuzh",
			mustContain:  []string{"你"},
			mustNotFirst: []string{"这", "知", "之"},
			desc:         "partial zh 展开的单字不应排首位",
		},
		{
			input:        "nizhibuzhi",
			mustContain:  []string{"你"},
			mustNotFirst: []string{"值不值", "不知", "布置", "知不知"}, // 非首位子词组不应排首位
			desc:         "ni+zhi+bu+zhi，非首位子词组（跳过ni）不应排首位",
		},
		{
			input:        "nizhibuzhid",
			mustContain:  []string{"你"},
			mustNotFirst: []string{"的", "得", "大", "地", "值不值"}, // partial单字和非首位子词组都不应排首位
			desc:         "partial d 展开的单字和非首位子词组不应排首位",
		},
		{
			input:       "nizhibuzhidao",
			mustContain: []string{"你知不知道"},
			firstOneOf:  []string{"你知不知道"},
			desc:        "完整输入，你知不知道应是首选",
		},

		// --- dazhongwu 序列 ---
		{
			input:       "dazhongwu",
			mustContain: []string{"大众", "大"},
			desc:        "完整拼音子词组应在候选中",
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

			// 检查必须包含
			for _, must := range tt.mustContain {
				found := false
				for _, c := range result.Candidates {
					if c.Text == must {
						found = true
						break
					}
				}
				if !found {
					t.Errorf("MUST contain '%s' (%s)", must, tt.desc)
				}
			}

			// 检查首候选
			if len(tt.firstOneOf) > 0 && len(result.Candidates) > 0 {
				first := result.Candidates[0].Text
				matched := false
				for _, f := range tt.firstOneOf {
					if first == f {
						matched = true
						break
					}
				}
				if !matched {
					t.Errorf("First candidate '%s' not in expected %v (%s)", first, tt.firstOneOf, tt.desc)
				}
			}

			// 检查不应排首位
			if len(tt.mustNotFirst) > 0 && len(result.Candidates) > 0 {
				first := result.Candidates[0].Text
				for _, bad := range tt.mustNotFirst {
					if first == bad {
						t.Errorf("'%s' should NOT be first (%s)", bad, tt.desc)
					}
				}
			}

			// ConsumedLength 边界检查
			for _, c := range result.Candidates {
				if c.ConsumedLength > len(tt.input) {
					t.Errorf("'%s' ConsumedLength=%d exceeds input length %d",
						c.Text, c.ConsumedLength, len(tt.input))
				}
			}
		})
	}
}
