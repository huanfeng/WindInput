package pinyin

import (
	"testing"
)

func TestParserParse(t *testing.T) {
	parser := NewPinyinParser()

	tests := []struct {
		input              string
		wantSyllableCount  int
		wantCompletedTexts []string
		wantPartial        string
		wantHasPartial     bool
	}{
		// 完整音节序列
		{
			input:              "nihao",
			wantSyllableCount:  2,
			wantCompletedTexts: []string{"ni", "hao"},
			wantHasPartial:     false,
		},
		{
			input:              "zhongguo",
			wantSyllableCount:  2,
			wantCompletedTexts: []string{"zhong", "guo"},
			wantHasPartial:     false,
		},
		{
			input:              "women",
			wantSyllableCount:  2,
			wantCompletedTexts: []string{"wo", "men"},
			wantHasPartial:     false,
		},
		// 带有未完成音节
		{
			input:              "nihaozh",
			wantSyllableCount:  3,
			wantCompletedTexts: []string{"ni", "hao"},
			wantPartial:        "zh",
			wantHasPartial:     true,
		},
		{
			input:              "zh",
			wantSyllableCount:  1,
			wantCompletedTexts: []string{},
			wantPartial:        "zh",
			wantHasPartial:     true,
		},
		{
			input:              "shangha",
			wantSyllableCount:  2,
			wantCompletedTexts: []string{"shang", "ha"}, // "ha" 是完整音节
			wantHasPartial:     false,
		},
		{
			input:              "shangh",
			wantSyllableCount:  2,
			wantCompletedTexts: []string{"shang"},
			wantPartial:        "h",
			wantHasPartial:     true,
		},
		// 单音节
		{
			input:              "ni",
			wantSyllableCount:  1,
			wantCompletedTexts: []string{"ni"},
			wantHasPartial:     false,
		},
		// Bug 修复: "zho" 应解析为一个 partial 音节，而非错位为 "o'ho"
		{
			input:              "zho",
			wantSyllableCount:  1,
			wantCompletedTexts: []string{},
			wantPartial:        "zho",
			wantHasPartial:     true,
		},
		// Bug 修复: "dh" 的 "h" 不应被丢弃
		{
			input:              "dh",
			wantSyllableCount:  2,
			wantCompletedTexts: []string{},
			wantPartial:        "h",
			wantHasPartial:     true,
		},
		// Bug 修复: "dddd" 纯声母序列应产生 4 个音节
		{
			input:              "dddd",
			wantSyllableCount:  4,
			wantCompletedTexts: []string{},
			wantPartial:        "d",
			wantHasPartial:     true,
		},
		// 空输入
		{
			input:             "",
			wantSyllableCount: 0,
			wantHasPartial:    false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			result := parser.Parse(tt.input)

			if len(result.Syllables) != tt.wantSyllableCount {
				t.Errorf("Parse(%q) syllable count = %d, want %d", tt.input, len(result.Syllables), tt.wantSyllableCount)
			}

			// 检查完整音节
			completedTexts := result.CompletedSyllables()
			if len(completedTexts) != len(tt.wantCompletedTexts) {
				t.Errorf("Parse(%q) completed count = %d, want %d, got %v", tt.input, len(completedTexts), len(tt.wantCompletedTexts), completedTexts)
			} else {
				for i, want := range tt.wantCompletedTexts {
					if completedTexts[i] != want {
						t.Errorf("Parse(%q) completed[%d] = %q, want %q", tt.input, i, completedTexts[i], want)
					}
				}
			}

			// 检查是否有未完成音节
			if result.HasPartial() != tt.wantHasPartial {
				t.Errorf("Parse(%q) HasPartial() = %v, want %v", tt.input, result.HasPartial(), tt.wantHasPartial)
			}

			// 检查未完成音节内容
			if tt.wantHasPartial && tt.wantPartial != "" {
				partial := result.PartialSyllable()
				if partial != tt.wantPartial {
					t.Errorf("Parse(%q) PartialSyllable() = %q, want %q", tt.input, partial, tt.wantPartial)
				}
			}
		})
	}
}

func TestParserParsePossibleContinues(t *testing.T) {
	parser := NewPinyinParser()

	tests := []struct {
		input             string
		wantPossibleCount int // 期望可能续写数量（> 0 即可）
	}{
		{"zh", 1},      // "zh" 可续写为 "zha", "zhai", "zhan" 等
		{"zho", 1},     // "zho" 可续写为 "zhong", "zhou"
		{"ni", 1},      // "ni" 可续写为 "nian", "niang", "niao" 等
		{"hao", 0},     // "hao" 无续写
		{"zhang", 0},   // "zhang" 无续写
		{"nihao", 0},   // 最后的 "hao" 无续写
		{"nihaozh", 1}, // 最后的 "zh" 可续写
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			result := parser.Parse(tt.input)

			if len(result.Syllables) == 0 {
				if tt.wantPossibleCount > 0 {
					t.Errorf("Parse(%q) no syllables but wanted possible continues", tt.input)
				}
				return
			}

			lastSyllable := result.LastSyllable()
			hasPossible := len(lastSyllable.Possible) > 0

			if tt.wantPossibleCount > 0 && !hasPossible {
				t.Errorf("Parse(%q) last syllable %q has no possible continues, expected some", tt.input, lastSyllable.Text)
			}
			if tt.wantPossibleCount == 0 && hasPossible {
				t.Errorf("Parse(%q) last syllable %q has possible continues %v, expected none", tt.input, lastSyllable.Text, lastSyllable.Possible)
			}
		})
	}
}

func TestParserQuickParse(t *testing.T) {
	parser := NewPinyinParser()

	tests := []struct {
		input string
		want  []string
	}{
		{"nihao", []string{"ni", "hao"}},
		{"zhongguo", []string{"zhong", "guo"}},
		{"nihaozh", []string{"ni", "hao", "zh"}},
		{"zho", []string{"zho"}},
		{"dh", []string{"d", "h"}},
		{"dddd", []string{"d", "d", "d", "d"}},
		{"", nil},
	}

	for _, tt := range tests {
		result := parser.QuickParse(tt.input)

		if len(result) != len(tt.want) {
			t.Errorf("QuickParse(%q) = %v, want %v", tt.input, result, tt.want)
			continue
		}

		for i := range result {
			if result[i] != tt.want[i] {
				t.Errorf("QuickParse(%q)[%d] = %q, want %q", tt.input, i, result[i], tt.want[i])
			}
		}
	}
}

func TestParserParseWithDetail(t *testing.T) {
	parser := NewPinyinParser()

	// 测试有多种切分方案的输入
	// "xian" 可以切分为 "xi'an" 或 "xian"
	result := parser.ParseWithDetail("xian", 5)

	if result.Best == nil {
		t.Fatal("ParseWithDetail(xian) Best is nil")
	}

	// 应该有最佳方案
	t.Logf("Best: %v", result.Best.SyllableTexts())

	// 可能有备选方案
	for i, alt := range result.Alternatives {
		t.Logf("Alt[%d]: %v", i, alt.SyllableTexts())
	}

	// 验证 "xian" 被正确解析
	texts := result.Best.SyllableTexts()
	if len(texts) == 0 {
		t.Error("ParseWithDetail(xian) returned empty syllables")
	}
}

func BenchmarkParserParse(b *testing.B) {
	parser := NewPinyinParser()
	inputs := []string{
		"nihao",
		"zhongguorenminjiefangjun",
		"nihaoshijie",
	}

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		for _, input := range inputs {
			parser.Parse(input)
		}
	}
}
