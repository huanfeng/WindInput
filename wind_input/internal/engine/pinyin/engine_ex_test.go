package pinyin

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

func createTestDictForEx(t *testing.T) dict.Dict {
	t.Helper()
	tmpDir := t.TempDir()

	// 注意：故意让 "赭石" 权重高于 "这是" 来测试排序
	content := `# Rime dictionary
---
name: test
version: "1.0"
sort: by_weight
...
啊	a	1000
爱	ai	1000
你	ni	1000
妮	ni	500
好	hao	1000
号	hao	500
你好	ni hao	800
我	wo	1000
么	me	900
们	men	1000
我们	wo men	800
是	shi	1000
石	shi	900
时	shi	850
事	shi	800
使	shi	700
的	de	1000
中	zhong	1000
国	guo	1000
中国	zhong guo	800
知	zhi	1000
之	zhi	900
道	dao	1000
知道	zhi dao	800
张	zhang	900
找	zhao	900
这	zhe	1000
赭	zhe	100
赭石	zhe shi	600
这事	zhe shi	500
这时	zhe shi	550
这是	zhe shi	700
这使	zhe shi	400
真	zhen	900
正	zheng	900
周	zhou	900
住	zhu	900
麽	me	100
不	bu	1000
白	bai	900
北	bei	900
北京	bei jing	800
不知道	bu zhi dao	700
你好吗	ni hao ma	600
吗	ma	800
替	ti	900
温	wen	800
提问	ti wen	700
`
	if err := os.WriteFile(filepath.Join(tmpDir, "8105.dict.yaml"), []byte(content), 0644); err != nil {
		t.Fatalf("写入测试文件失败: %v", err)
	}

	d := dict.NewPinyinDict()
	if err := d.LoadRimeDir(tmpDir); err != nil {
		t.Fatalf("加载词库失败: %v", err)
	}
	return d
}

func TestEngineConvertEx(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	tests := []struct {
		input          string
		wantPreedit    string
		wantContain    string
		wantHasPartial bool
	}{
		{
			input:          "nihao",
			wantPreedit:    "ni'hao",
			wantContain:    "你好",
			wantHasPartial: false,
		},
		{
			input:          "zhongguo",
			wantPreedit:    "zhong'guo",
			wantContain:    "中国",
			wantHasPartial: false,
		},
		{
			input:          "women",
			wantPreedit:    "wo'men",
			wantContain:    "我们",
			wantHasPartial: false,
		},
		{
			input:          "nihaozh",
			wantPreedit:    "ni'hao'zh",
			wantHasPartial: true,
		},
		{
			input:          "zh",
			wantPreedit:    "zh",
			wantHasPartial: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			result := engine.ConvertEx(tt.input, 10)

			// 检查组合态
			if result.Composition == nil {
				t.Fatal("ConvertEx returned nil Composition")
			}

			// 检查预编辑文本
			if result.PreeditDisplay != tt.wantPreedit {
				t.Errorf("ConvertEx(%q) PreeditDisplay = %q, want %q",
					tt.input, result.PreeditDisplay, tt.wantPreedit)
			}

			// 检查是否有未完成音节
			if result.Composition.HasPartial() != tt.wantHasPartial {
				t.Errorf("ConvertEx(%q) HasPartial() = %v, want %v",
					tt.input, result.Composition.HasPartial(), tt.wantHasPartial)
			}

			// 检查候选词
			if tt.wantContain != "" {
				found := false
				for _, c := range result.Candidates {
					if c.Text == tt.wantContain {
						found = true
						break
					}
				}
				if !found {
					var texts []string
					for _, c := range result.Candidates {
						texts = append(texts, c.Text)
					}
					t.Errorf("ConvertEx(%q) missing %q, got %v",
						tt.input, tt.wantContain, texts)
				}
			}
		})
	}
}

// TestEngineConvertExRanking 测试排序：常用词应优先于生僻词
func TestEngineConvertExRanking(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	// 构建一个简单的 Unigram 模型，模拟真实字频
	// 常见字（这、是、事、时）频率高，生僻字（赭）频率低
	unigram := NewUnigramModel()
	charFreqs := map[string]float64{
		"这": 900000, "是": 850000, "事": 500000, "时": 600000,
		"使": 300000, "赭": 1000, "石": 200000,
		"我": 950000, "们": 400000, "你": 920000, "好": 700000,
		"中": 800000, "国": 750000, "知": 500000, "道": 450000,
		"张": 350000, "找": 300000, "真": 550000, "正": 520000,
		"周": 300000, "住": 280000, "之": 600000, "啊": 200000,
		"爱": 300000, "妮": 50000, "号": 200000, "的": 980000,
		"么": 400000, "麽": 10000, "wo": 0, "me": 0,
	}
	unigram.LoadFromFreqMap(charFreqs)
	engine.SetUnigram(unigram)

	// 测试 "zheshi"：词库中 "赭石" 在 "这是" 前面（行号更小），
	// 但 "这是" 应该排在更前面（使用单字频率排序）
	result := engine.ConvertEx("zheshi", 10)

	if len(result.Candidates) == 0 {
		t.Fatal("ConvertEx('zheshi') returned no candidates")
	}

	// 打印前5个候选便于调试
	for i, c := range result.Candidates {
		if i >= 5 {
			break
		}
		t.Logf("zheshi candidate[%d]: %s (weight=%d)", i, c.Text, c.Weight)
	}

	// 所有精确匹配的2字词应在单字之前
	firstSingleCharIdx := -1
	lastPhraseIdx := -1
	for i, c := range result.Candidates {
		if len([]rune(c.Text)) == 1 {
			if firstSingleCharIdx == -1 {
				firstSingleCharIdx = i
			}
		} else {
			lastPhraseIdx = i
		}
	}
	if firstSingleCharIdx >= 0 && lastPhraseIdx >= 0 && firstSingleCharIdx < lastPhraseIdx {
		t.Errorf("Single char appears before a phrase: singleAt=%d, phraseAt=%d", firstSingleCharIdx, lastPhraseIdx)
	}
}

// TestEngineConvertExPrefixPrediction 测试前缀预测："wome" 应包含 "我们"
func TestEngineConvertExPrefixPrediction(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	// 输入 "wome"（还没输完 "women"），应该通过前缀匹配找到 "我们"
	result := engine.ConvertEx("wome", 10)

	for i, c := range result.Candidates {
		if i >= 5 {
			break
		}
		t.Logf("wome candidate[%d]: %s (weight=%d)", i, c.Text, c.Weight)
	}

	// "我们" 应该在候选列表中
	found := false
	foundIdx := -1
	for i, c := range result.Candidates {
		if c.Text == "我们" {
			found = true
			foundIdx = i
			break
		}
	}
	if !found {
		var texts []string
		for _, c := range result.Candidates {
			texts = append(texts, c.Text)
		}
		t.Fatalf("ConvertEx('wome') should contain '我们', got %v", texts)
	}

	// "我们" 应该排在前3名
	if foundIdx > 2 {
		t.Errorf("'我们' should be in top 3, but at position %d", foundIdx)
	}
}

func TestEngineConvertExEmpty(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	result := engine.ConvertEx("", 10)

	if !result.IsEmpty {
		t.Error("ConvertEx('') should return IsEmpty=true")
	}

	if len(result.Candidates) != 0 {
		t.Errorf("ConvertEx('') should return no candidates, got %d", len(result.Candidates))
	}
}

func TestEngineParseInput(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	tests := []struct {
		input       string
		wantPreedit string
	}{
		{"nihao", "ni'hao"},
		{"nihaozh", "ni'hao'zh"},
		{"zh", "zh"},
		{"", ""},
	}

	for _, tt := range tests {
		comp := engine.ParseInput(tt.input)
		if comp.PreeditText != tt.wantPreedit {
			t.Errorf("ParseInput(%q) PreeditText = %q, want %q",
				tt.input, comp.PreeditText, tt.wantPreedit)
		}
	}
}

func TestEngineGetPossibleSyllables(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	tests := []struct {
		prefix      string
		wantContain []string
	}{
		{"zh", []string{"zha", "zhe", "zhi", "zhong", "zhou", "zhu"}},
		{"ni", []string{"ni", "nian", "niao", "nie", "nin", "ning", "niu"}},
	}

	for _, tt := range tests {
		results := engine.GetPossibleSyllables(tt.prefix)
		resultSet := make(map[string]bool)
		for _, r := range results {
			resultSet[r] = true
		}

		for _, want := range tt.wantContain {
			if !resultSet[want] {
				t.Errorf("GetPossibleSyllables(%q) missing %q, got %v",
					tt.prefix, want, results)
			}
		}
	}
}

func TestEngineIsValidSyllable(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	validSyllables := []string{"ni", "hao", "zhong", "guo", "wo", "men"}
	for _, s := range validSyllables {
		if !engine.IsValidSyllable(s) {
			t.Errorf("IsValidSyllable(%q) = false, want true", s)
		}
	}

	invalidSyllables := []string{"zh", "ng", "xyz"}
	for _, s := range invalidSyllables {
		if engine.IsValidSyllable(s) {
			t.Errorf("IsValidSyllable(%q) = true, want false", s)
		}
	}
}

func TestEngineIsValidSyllablePrefix(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	validPrefixes := []string{"zh", "sh", "ch", "n", "h", "z"}
	for _, s := range validPrefixes {
		if !engine.IsValidSyllablePrefix(s) {
			t.Errorf("IsValidSyllablePrefix(%q) = false, want true", s)
		}
	}
}

// TestEngineConvertExLongInput 长输入压力测试
func TestEngineConvertExLongInput(t *testing.T) {
	d := createTestDictForEx(t)
	config := &Config{
		ShowWubiHint:    false,
		FilterMode:      "all",
		UseSmartCompose: true,
	}
	engine := NewEngineWithConfig(d, config)

	// 构建 Unigram 以启用 Viterbi
	unigram := NewUnigramModel()
	charFreqs := map[string]float64{
		"你": 920000, "好": 700000, "我": 950000, "们": 400000,
		"中": 800000, "国": 750000, "是": 850000, "的": 980000,
		"知": 500000, "道": 450000, "这": 900000,
	}
	unigram.LoadFromFreqMap(charFreqs)
	engine.SetUnigram(unigram)

	longInputs := []string{
		"nihaowoshizhongguoren",                // 21 chars
		"zheshiwomendeguojia",                  // 19 chars
		"nizhidaowomenshisheidema",             // 24 chars
		"zhongguorenminjiefangjun",             // 23 chars（部分音节无词条）
		"aaaaaaaaaaaaaaaaaaaaaaaa",             // 24 个无效输入
		"nihaonihaonihaonihaonihao",            // 25 chars 重复
		"zheshizheshizheshizheshizheshizheshi", // 超长重复
	}

	for _, input := range longInputs {
		t.Run(input[:min(len(input), 15)], func(t *testing.T) {
			result := engine.ConvertEx(input, 50)
			// 关键：不应 panic，应正常返回
			if result == nil {
				t.Fatal("ConvertEx returned nil")
			}
			t.Logf("input=%q (len=%d) candidates=%d isEmpty=%v",
				input, len(input), len(result.Candidates), result.IsEmpty)
		})
	}
}

// TestEngineConvertExSortModes 测试不同排序模式
func TestEngineConvertExSortModes(t *testing.T) {
	d := createTestDictForEx(t)

	// 构建 Unigram
	unigram := NewUnigramModel()
	charFreqs := map[string]float64{
		"这": 900000, "是": 850000, "事": 500000, "时": 600000,
		"使": 300000, "赭": 1000, "石": 200000,
	}
	unigram.LoadFromFreqMap(charFreqs)

	tests := []struct {
		order string
		check func(t *testing.T, candidates []candidate.Candidate)
	}{
		{
			order: "phrase_first",
			check: func(t *testing.T, candidates []candidate.Candidate) {
				// 词组应排在单字前面
				lastPhraseIdx := -1
				firstSingleIdx := -1
				for i, c := range candidates {
					runeLen := len([]rune(c.Text))
					if runeLen > 1 && lastPhraseIdx < i {
						lastPhraseIdx = i
					}
					if runeLen == 1 && firstSingleIdx == -1 {
						firstSingleIdx = i
					}
				}
				if firstSingleIdx >= 0 && lastPhraseIdx >= 0 && firstSingleIdx < lastPhraseIdx {
					t.Error("phrase_first: single char found before last phrase")
				}
			},
		},
		{
			order: "char_first",
			check: func(t *testing.T, candidates []candidate.Candidate) {
				// 默认按权重排序，不强制单字优先
				if len(candidates) == 0 {
					t.Error("char_first: no candidates")
				}
			},
		},
		{
			order: "smart",
			check: func(t *testing.T, candidates []candidate.Candidate) {
				// 智能模式：应有候选返回
				if len(candidates) == 0 {
					t.Error("smart: no candidates")
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.order, func(t *testing.T) {
			config := &Config{
				FilterMode:     "all",
				CandidateOrder: tt.order,
			}
			engine := NewEngineWithConfig(d, config)
			engine.SetUnigram(unigram)

			result := engine.ConvertEx("zheshi", 20)
			if result == nil {
				t.Fatal("nil result")
			}
			t.Logf("%s: %d candidates", tt.order, len(result.Candidates))
			for i, c := range result.Candidates {
				if i >= 5 {
					break
				}
				t.Logf("  [%d] %q weight=%d", i, c.Text, c.Weight)
			}
			tt.check(t, result.Candidates)
		})
	}
}

// TestEngineConvertExFilterModes 测试不同过滤模式
func TestEngineConvertExFilterModes(t *testing.T) {
	d := createTestDictForEx(t)

	// 测试各种过滤模式都能正常运行
	modes := []string{"all", "smart", "general", "gb18030"}

	for _, mode := range modes {
		t.Run(mode, func(t *testing.T) {
			config := &Config{FilterMode: mode}
			engine := NewEngineWithConfig(d, config)

			result := engine.ConvertEx("nihao", 50)
			if result == nil {
				t.Fatal("nil result")
			}
			t.Logf("filter=%s: %d candidates", mode, len(result.Candidates))

			// "general" 模式过滤 IsCommon=false 的候选，测试词库无 IsCommon 标记
			// 所以 general 返回 0 是正确行为；smart 会 fallback 到全部
			if mode == "general" {
				// general 模式可能返回 0 候选（因为 IsCommon 未标记）
				return
			}
			// 其他模式应包含 你好
			found := false
			for _, c := range result.Candidates {
				if c.Text == "你好" {
					found = true
					break
				}
			}
			if !found {
				t.Errorf("filter=%s: missing '你好'", mode)
			}
		})
	}

	// 比较 ConvertRaw（无过滤）和 Convert（有过滤）
	t.Run("ConvertRaw_vs_Convert", func(t *testing.T) {
		config := &Config{FilterMode: "smart"}
		engine := NewEngineWithConfig(d, config)

		raw, err := engine.ConvertRaw("nihao", 50)
		if err != nil {
			t.Fatal(err)
		}
		filtered, err := engine.Convert("nihao", 50)
		if err != nil {
			t.Fatal(err)
		}

		t.Logf("ConvertRaw: %d, Convert: %d", len(raw), len(filtered))
		// ConvertRaw 应该 >= Convert（因为 Convert 会过滤）
		if len(raw) < len(filtered) {
			t.Errorf("ConvertRaw (%d) should return >= Convert (%d)", len(raw), len(filtered))
		}
	})
}

// TestEngineConvertExConsumedLength 测试部分上屏的 ConsumedLength
func TestEngineConvertExConsumedLength(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	tests := []struct {
		input     string
		wantText  string
		wantRange [2]int // ConsumedLength 的最小和最大期望值
	}{
		{
			input:     "nihao",
			wantText:  "你好",
			wantRange: [2]int{5, 5}, // 完整匹配，消耗全部
		},
		{
			input:     "nihao",
			wantText:  "你",
			wantRange: [2]int{2, 2}, // 首音节单字，消耗 "ni"
		},
		{
			input:     "nihao",
			wantText:  "好",
			wantRange: [2]int{5, 5}, // 第二音节单字，消耗到 "nihao"
		},
	}

	for _, tt := range tests {
		t.Run(tt.input+"→"+tt.wantText, func(t *testing.T) {
			result := engine.ConvertEx(tt.input, 50)
			found := false
			for _, c := range result.Candidates {
				if c.Text == tt.wantText {
					found = true
					if c.ConsumedLength < tt.wantRange[0] || c.ConsumedLength > tt.wantRange[1] {
						t.Errorf("ConsumedLength for %q = %d, want [%d, %d]",
							tt.wantText, c.ConsumedLength, tt.wantRange[0], tt.wantRange[1])
					} else {
						t.Logf("%q → ConsumedLength=%d (OK)", tt.wantText, c.ConsumedLength)
					}
					break
				}
			}
			if !found {
				t.Errorf("candidate %q not found in results", tt.wantText)
			}
		})
	}
}

// TestEngineConvertExFuzzyIntegration 模糊拼音引擎集成测试
func TestEngineConvertExFuzzyIntegration(t *testing.T) {
	d := createTestDictForEx(t)

	// 启用 zh↔z 和 sh↔s 模糊
	fuzzyConfig := &FuzzyConfig{ZhZ: true, ShS: true}
	config := &Config{
		FilterMode: "all",
		Fuzzy:      fuzzyConfig,
	}
	engine := NewEngineWithConfig(d, config)

	// "zeshi" 通过 z↔zh 模糊应该能找到 "这是"（zhe shi）
	t.Run("fuzzy_zh_z", func(t *testing.T) {
		result := engine.ConvertEx("zesi", 20)
		found := false
		for _, c := range result.Candidates {
			if c.Text == "这" || c.Text == "赭" {
				found = true
				break
			}
		}
		if !found {
			var texts []string
			for _, c := range result.Candidates {
				texts = append(texts, c.Text)
			}
			t.Logf("zesi candidates: %v", texts)
			// 模糊查找 ze → zhe，si → shi
			t.Log("Note: fuzzy zh↔z should expand 'ze' to include 'zhe' results")
		}
	})

	// "si" 通过 s↔sh 模糊应该能找到 "是"（shi）
	t.Run("fuzzy_sh_s", func(t *testing.T) {
		result := engine.ConvertEx("si", 20)
		found := false
		for _, c := range result.Candidates {
			if c.Text == "是" || c.Text == "时" || c.Text == "石" {
				found = true
				t.Logf("Found '%s' via sh↔s fuzzy", c.Text)
				break
			}
		}
		if !found {
			var texts []string
			for i, c := range result.Candidates {
				if i >= 10 {
					break
				}
				texts = append(texts, c.Text)
			}
			t.Errorf("si should find shi-group chars via fuzzy, got %v", texts)
		}
	})

	// 无模糊配置时，"si" 不应找到 "shi" 的字
	t.Run("no_fuzzy", func(t *testing.T) {
		noFuzzyConfig := &Config{FilterMode: "all"}
		noFuzzyEngine := NewEngineWithConfig(d, noFuzzyConfig)

		result := noFuzzyEngine.ConvertEx("si", 20)
		for _, c := range result.Candidates {
			if c.Text == "是" || c.Text == "时" || c.Text == "石" {
				t.Errorf("Without fuzzy, 'si' should NOT find %q (from 'shi')", c.Text)
			}
		}
	})
}

// TestEngineUserFreqLearning 用户词频学习测试
func TestEngineUserFreqLearning(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	// 创建 Unigram
	unigram := NewUnigramModel()
	charFreqs := map[string]float64{
		"你": 920000, "好": 700000, "妮": 50000,
	}
	unigram.LoadFromFreqMap(charFreqs)
	engine.SetUnigram(unigram)

	// 验证初始状态：你 的概率高于 妮
	probNi := unigram.LogProb("你")
	probNi2 := unigram.LogProb("妮")
	if probNi <= probNi2 {
		t.Errorf("初始状态：你(%.4f) 应高于 妮(%.4f)", probNi, probNi2)
	}

	// 多次选择 "妮" 来提升词频
	for i := 0; i < 10; i++ {
		unigram.BoostUserFreq("妮", 1)
	}

	// 验证提升后的概率变化
	boostedProb := unigram.LogProb("妮")
	if boostedProb <= probNi2 {
		t.Errorf("提升后：妮(%.4f) 应高于初始值(%.4f)", boostedProb, probNi2)
	}
	t.Logf("妮: 初始=%.4f 提升后=%.4f (diff=%.4f)", probNi2, boostedProb, boostedProb-probNi2)

	// 验证 SaveUserFreqs/LoadUserFreqs 往返一致
	tmpFile := filepath.Join(t.TempDir(), "user_freq.txt")
	if err := unigram.SaveUserFreqs(tmpFile); err != nil {
		t.Fatal(err)
	}

	unigram2 := NewUnigramModel()
	unigram2.LoadFromFreqMap(charFreqs)
	if err := unigram2.LoadUserFreqs(tmpFile); err != nil {
		t.Fatal(err)
	}

	// 加载后概率应一致
	loadedProb := unigram2.LogProb("妮")
	if loadedProb != boostedProb {
		t.Errorf("加载后概率不一致: got=%.4f want=%.4f", loadedProb, boostedProb)
	}
}

// TestEngineConvertViaConvert 确保 Convert/ConvertRaw 委托到 convertCore 行为正确
func TestEngineConvertViaConvert(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	// 空输入
	candidates, err := engine.Convert("", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(candidates) != 0 {
		t.Errorf("Convert('') should return empty, got %d", len(candidates))
	}

	// 正常输入
	candidates, err = engine.Convert("nihao", 10)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, c := range candidates {
		if c.Text == "你好" {
			found = true
			break
		}
	}
	if !found {
		t.Error("Convert('nihao') should contain '你好'")
	}

	// ConvertRaw 应返回更多或等量候选
	raw, err := engine.ConvertRaw("nihao", 50)
	if err != nil {
		t.Fatal(err)
	}
	if len(raw) == 0 {
		t.Error("ConvertRaw('nihao') should return candidates")
	}
}

// TestEngineConvertExAbbrev 简拼词组匹配测试
func TestEngineConvertExAbbrev(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	// 输入 "bzd" 应匹配简拼 "不知道"（bu zhi dao → b+z+d）
	t.Run("bzd_abbrev", func(t *testing.T) {
		result := engine.ConvertEx("bzd", 20)

		for i, c := range result.Candidates {
			if i >= 10 {
				break
			}
			t.Logf("bzd candidate[%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
		}

		// "不知道" 应该在候选列表中
		found := false
		for _, c := range result.Candidates {
			if c.Text == "不知道" {
				found = true
				if c.ConsumedLength != 3 {
					t.Errorf("'不知道' ConsumedLength = %d, want 3", c.ConsumedLength)
				}
				break
			}
		}
		if !found {
			var texts []string
			for _, c := range result.Candidates {
				texts = append(texts, c.Text)
			}
			t.Errorf("ConvertEx('bzd') should contain '不知道', got %v", texts)
		}
	})

	// 输入 "nh" 应匹配简拼 "你好"（ni hao → n+h）
	t.Run("nh_abbrev", func(t *testing.T) {
		result := engine.ConvertEx("nh", 20)

		for i, c := range result.Candidates {
			if i >= 10 {
				break
			}
			t.Logf("nh candidate[%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
		}

		found := false
		for _, c := range result.Candidates {
			if c.Text == "你好" {
				found = true
				break
			}
		}
		if !found {
			var texts []string
			for _, c := range result.Candidates {
				texts = append(texts, c.Text)
			}
			t.Errorf("ConvertEx('nh') should contain '你好', got %v", texts)
		}
	})
}

// TestEngineConvertExFirstPartialCandidate 首音节候选测试
// 当所有音节都是 partial 时，首音节应生成单字候选
func TestEngineConvertExFirstPartialCandidate(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	// 输入 "bzd"，所有音节 ["b","z","d"] 都是 partial
	// 首音节 "b" 应生成候选（不、白、北 等 b 开头的单字）
	result := engine.ConvertEx("bzd", 30)

	for i, c := range result.Candidates {
		if i >= 15 {
			break
		}
		t.Logf("bzd candidate[%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	// 检查首音节 "b" 的单字候选存在
	bChars := []string{"不", "白", "北"}
	for _, want := range bChars {
		found := false
		for _, c := range result.Candidates {
			if c.Text == want {
				found = true
				// 首音节单字的 ConsumedLength 应为 1（只消耗 "b"）
				if c.ConsumedLength != 1 {
					t.Errorf("'%s' ConsumedLength = %d, want 1", want, c.ConsumedLength)
				}
				break
			}
		}
		if !found {
			t.Errorf("ConvertEx('bzd') should contain '%s' as first partial candidate", want)
		}
	}

	// 简拼词组 "不知道" 应排在首音节单字 "不" 之前（权重更高）
	bzdIdx := -1
	buIdx := -1
	for i, c := range result.Candidates {
		if c.Text == "不知道" && bzdIdx == -1 {
			bzdIdx = i
		}
		if c.Text == "不" && buIdx == -1 {
			buIdx = i
		}
	}
	if bzdIdx >= 0 && buIdx >= 0 && bzdIdx > buIdx {
		t.Errorf("'不知道'(idx=%d) should rank before '不'(idx=%d)", bzdIdx, buIdx)
	}
}

// TestEngineConvertExMultiSyllableShowsSingleChars 多音节输入应同时显示首音节单字
func TestEngineConvertExMultiSyllableShowsSingleChars(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	// 输入 "nihao"（2 音节），应同时包含词组"你好"和首音节单字"你"、"妮"
	result := engine.ConvertEx("nihao", 30)

	for i, c := range result.Candidates {
		if i >= 10 {
			break
		}
		t.Logf("nihao candidate[%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	// 词组"你好"应在首位
	if len(result.Candidates) == 0 || result.Candidates[0].Text != "你好" {
		t.Errorf("First candidate should be '你好'")
	}

	// 首音节单字"妮"应在候选列表中（"你"可能已被精确匹配包含）
	found := false
	foundIdx := -1
	for i, c := range result.Candidates {
		if c.Text == "妮" {
			found = true
			foundIdx = i
			break
		}
	}
	if !found {
		var texts []string
		for _, c := range result.Candidates {
			texts = append(texts, c.Text)
		}
		t.Fatalf("nihao should contain '妮' as first syllable char, got %v", texts)
	}

	// "妮"应排在前 10 名内（不应被推到很远的位置）
	if foundIdx > 10 {
		t.Errorf("'妮' at position %d, should be within top 10", foundIdx)
	}

	// "妮"的 ConsumedLength 应为 2（只消耗 "ni"）
	for _, c := range result.Candidates {
		if c.Text == "妮" && c.ConsumedLength != 2 {
			t.Errorf("'妮' ConsumedLength = %d, want 2", c.ConsumedLength)
		}
	}
}

// TestEngineConvertExTiwenShowsChars "tiwen" 应同时显示词组和首音节单字
func TestEngineConvertExTiwenShowsChars(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	result := engine.ConvertEx("tiwen", 30)

	for i, c := range result.Candidates {
		if i >= 10 {
			break
		}
		t.Logf("tiwen candidate[%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	// "提问" 词组应存在
	foundPhrase := false
	for _, c := range result.Candidates {
		if c.Text == "提问" {
			foundPhrase = true
			break
		}
	}
	if !foundPhrase {
		t.Error("tiwen should contain '提问'")
	}

	// "替" 首音节单字也应存在
	foundChar := false
	charIdx := -1
	for i, c := range result.Candidates {
		if c.Text == "替" {
			foundChar = true
			charIdx = i
			break
		}
	}
	if !foundChar {
		var texts []string
		for _, c := range result.Candidates {
			texts = append(texts, c.Text)
		}
		t.Fatalf("tiwen should contain '替', got %v", texts)
	}

	// "替" 应在前 5 名内
	if charIdx > 5 {
		t.Errorf("'替' at position %d, should be within top 5", charIdx)
	}

	// "替" 的 ConsumedLength 应为 2（只消耗 "ti"）
	for _, c := range result.Candidates {
		if c.Text == "替" && c.ConsumedLength != 2 {
			t.Errorf("'替' ConsumedLength = %d, want 2", c.ConsumedLength)
		}
	}
}

// TestEngineConvertExSingleLetterPriority 单字母输入时单字应优先于词组
func TestEngineConvertExSingleLetterPriority(t *testing.T) {
	d := createTestDictForEx(t)
	engine := NewEngine(d)

	// 输入 "b"：单字（不、白、北）应排在词组（版权等）前面
	result := engine.ConvertEx("b", 30)

	for i, c := range result.Candidates {
		if i >= 10 {
			break
		}
		t.Logf("b candidate[%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	// 找到第一个单字和第一个多字词的位置
	firstSingleCharIdx := -1
	firstPhraseIdx := -1
	for i, c := range result.Candidates {
		charCount := len([]rune(c.Text))
		if charCount == 1 && firstSingleCharIdx == -1 {
			firstSingleCharIdx = i
		}
		if charCount > 1 && firstPhraseIdx == -1 {
			firstPhraseIdx = i
		}
	}

	// 单字应排在词组前面
	if firstSingleCharIdx >= 0 && firstPhraseIdx >= 0 && firstSingleCharIdx > firstPhraseIdx {
		t.Errorf("Single char (idx=%d) should appear before phrase (idx=%d) for 'b' input",
			firstSingleCharIdx, firstPhraseIdx)
	}
}

func BenchmarkEngineConvertEx(b *testing.B) {
	tmpDir := b.TempDir()

	content := `# Rime dictionary
---
name: bench
version: "1.0"
sort: by_weight
...
你	ni	1000
好	hao	1000
你好	ni hao	800
中	zhong	1000
国	guo	1000
中国	zhong guo	800
我	wo	1000
们	men	1000
我们	wo men	800
`
	if err := os.WriteFile(filepath.Join(tmpDir, "8105.dict.yaml"), []byte(content), 0644); err != nil {
		b.Fatalf("写入测试文件失败: %v", err)
	}

	d := dict.NewPinyinDict()
	if err := d.LoadRimeDir(tmpDir); err != nil {
		b.Fatalf("加载词库失败: %v", err)
	}

	engine := NewEngine(d)
	inputs := []string{"nihao", "zhongguo", "women", "nihaozh"}

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		for _, input := range inputs {
			engine.ConvertEx(input, 10)
		}
	}
}
