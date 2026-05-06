package pinyin

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
)

func createTestDictForViterbi(t testing.TB) *dict.CompositeDict {
	t.Helper()
	tmpDir := t.TempDir()

	content := `# Rime dictionary
---
name: test
version: "1.0"
sort: by_weight
...
今	jin	1000
金	jin	900
进	jin	800
天	tian	1000
田	tian	900
今天	jin tian	800
天气	tian qi	800
很	hen	1000
好	hao	1000
很好	hen hao	800
我	wo	1000
爱	ai	1000
学	xue	1000
习	xi	1000
学习	xue xi	800
中	zhong	1000
国	guo	1000
中国	zhong guo	800
人	ren	1000
民	min	1000
人民	ren min	800
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

func createTestUnigram(t testing.TB) *UnigramModel {
	t.Helper()
	m := NewUnigramModel()
	freqs := map[string]float64{
		"今天": 500,
		"天气": 400,
		"很好": 300,
		"学习": 250,
		"中国": 600,
		"人民": 350,
		"我":  800,
		"爱":  200,
		"今":  100,
		"天":  100,
		"金":  80,
		"田":  60,
		"很":  100,
		"好":  200,
		"学":  100,
		"习":  50,
		"中":  80,
		"国":  100,
		"人":  100,
		"民":  50,
		"进":  70,
	}
	m.LoadFromFreqMap(freqs)
	return m
}

func TestViterbiDecode(t *testing.T) {
	d := createTestDictForViterbi(t)
	unigram := createTestUnigram(t)
	st := NewSyllableTrie()

	tests := []struct {
		input    string
		wantWord string // 期望结果中包含的词
	}{
		{"jintian", "今天"},
		{"henhao", "很好"},
		{"zhongguo", "中国"},
	}

	for _, tt := range tests {
		lattice := BuildLattice(tt.input, st, d, unigram)
		if lattice.IsEmpty() {
			t.Errorf("BuildLattice(%q) 返回空网格", tt.input)
			continue
		}

		result := ViterbiDecode(lattice, nil)
		if result == nil {
			t.Errorf("ViterbiDecode(%q) 返回 nil", tt.input)
			continue
		}

		sentence := result.String()
		if sentence != tt.wantWord {
			t.Errorf("ViterbiDecode(%q) = %q, want %q", tt.input, sentence, tt.wantWord)
		}
	}
}

func TestViterbiLongInput(t *testing.T) {
	d := createTestDictForViterbi(t)
	unigram := createTestUnigram(t)
	st := NewSyllableTrie()

	// 测试"今天天气很好"
	input := "jintiantianqihenhao"
	lattice := BuildLattice(input, st, d, unigram)

	result := ViterbiDecode(lattice, nil)
	if result == nil {
		t.Fatal("ViterbiDecode 返回 nil")
	}

	sentence := result.String()
	t.Logf("输入: %s -> 输出: %s (words: %v, logprob: %.4f)", input, sentence, result.Words, result.LogProb)

	// 期望至少包含"今天"
	found := false
	for _, word := range result.Words {
		if word == "今天" {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("结果 %v 中未找到 '今天'", result.Words)
	}
}

func TestViterbiEmpty(t *testing.T) {
	st := NewSyllableTrie()
	d := dict.NewPinyinDict(nil)
	lattice := BuildLattice("xyz", st, wrapInCompositeDict(d), nil)

	result := ViterbiDecode(lattice, nil)
	if result != nil {
		t.Errorf("ViterbiDecode(空网格) 应返回 nil, 得到 %v", result)
	}
}

func TestViterbiResult(t *testing.T) {
	r := &ViterbiResult{
		Words:   []string{"今天", "天气", "很好"},
		LogProb: -10.5,
	}

	if r.String() != "今天天气很好" {
		t.Errorf("String() = %q, want '今天天气很好'", r.String())
	}
}
