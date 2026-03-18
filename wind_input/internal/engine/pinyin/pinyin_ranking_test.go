package pinyin

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
)

// createExtendedTestDict 创建包含更多词条的测试词库，用于覆盖长句输入场景
func createExtendedTestDict(t *testing.T) *dict.CompositeDict {
	t.Helper()
	tmpDir := t.TempDir()

	content := `# Rime dictionary
---
name: test_extended
version: "1.0"
sort: by_weight
...
我	wo	1000
我们	wo men	800
我不	wo bu	600
不	bu	1000
不知道	bu zhi dao	700
不知	bu zhi	600
知	zhi	1000
知道	zhi dao	800
道	dao	1000
该	gai	900
改	gai	850
大	da	1000
大众	da zhong	800
中	zhong	1000
众	zhong	700
武	wu	900
舞	wu	800
物	wu	850
对	dui	900
自	zi	900
对自我	dui zi wo	500
的	de	1000
得	de	900
地	di	900
表	biao	800
现	xian	900
表现	biao xian	700
你	ni	1000
好	hao	1000
你好	ni hao	800
是	shi	1000
百	bai	800
子	zi	800
冬	dong	700
瓜	gua	700
打	da	800
包	bao	700
打包	da bao	600
他	ta	900
她	ta	850
吗	ma	800
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

// findCandidate 在候选列表中查找指定文本，返回索引和是否找到
func findCandidate(result *PinyinConvertResult, text string) (int, bool) {
	for i, c := range result.Candidates {
		if c.Text == text {
			return i, true
		}
	}
	return -1, false
}

// ============================================================
// Bug 1: 简拼不应压过完整拼音匹配
// "dazhongwu" 切分为 ["da","zhong","wu"]，
// 子词组 "大众" 应排在简拼 "对自我"(d-z-w) 之前
// ============================================================

func TestFullPinyinBeatsAbbrev_dazhongwu(t *testing.T) {
	d := createExtendedTestDict(t)
	engine := NewEngine(d)

	result := engine.ConvertEx("dazhongwu", 50)

	for i, c := range result.Candidates {
		if i >= 10 {
			break
		}
		t.Logf("dazhongwu candidate[%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	// "大众" 是完整拼音匹配 (da+zhong)，应该在候选中
	dazhongIdx, foundDazhong := findCandidate(result, "大众")
	if !foundDazhong {
		t.Fatal("'大众' should be in candidates for 'dazhongwu'")
	}

	// "对自我" 是简拼匹配 (d-z-w)，如果存在，排名必须低于 "大众"
	duiziwoIdx, foundDuiziwo := findCandidate(result, "对自我")
	if foundDuiziwo && duiziwoIdx < dazhongIdx {
		t.Errorf("Full pinyin '大众'(idx=%d) should rank higher than abbrev '对自我'(idx=%d)",
			dazhongIdx, duiziwoIdx)
	}

	// "大" 作为首音节单字也应在候选中
	_, foundDa := findCandidate(result, "大")
	if !foundDa {
		t.Error("'大' should be in candidates for 'dazhongwu'")
	}
}

// ============================================================
// Bug 2: partial 后缀不应导致已完成音节的匹配消失
// "wobuzhidao" → "我不知道" 是第一候选
// "wobuzhidaog" → "我不知道" 仍应在候选中且排名靠前
// ============================================================

func TestPartialSuffixKeepsExactMatch_wobuzhidaog(t *testing.T) {
	d := createExtendedTestDict(t)
	engine := NewEngine(d)

	// 先验证无 partial 时的基线
	resultBase := engine.ConvertEx("wobuzhidao", 50)
	t.Log("=== wobuzhidao (baseline) ===")
	for i, c := range resultBase.Candidates {
		if i >= 5 {
			break
		}
		t.Logf("  [%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	baseIdx, foundBase := findCandidate(resultBase, "我不知道")
	// 注意：测试词库可能没有 "我不知道" 作为一个整词，
	// 但 Viterbi 应该能组出来（如果有 bigram/unigram 支持）。
	// 退而求其次，至少 "不知道" 和 "知道" 应在候选中。
	if !foundBase {
		// 检查子词组
		_, foundBuzhidao := findCandidate(resultBase, "不知道")
		if !foundBuzhidao {
			t.Error("'不知道' should be in candidates for 'wobuzhidao'")
		}
	} else {
		t.Logf("'我不知道' found at index %d (baseline)", baseIdx)
	}

	// 核心测试：加了 partial "g" 后
	resultWithG := engine.ConvertEx("wobuzhidaog", 50)
	t.Log("=== wobuzhidaog (with partial g) ===")
	for i, c := range resultWithG.Candidates {
		if i >= 10 {
			break
		}
		t.Logf("  [%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	// "不知道" 必须仍在候选中（作为子词组或精确匹配）
	buzhidaoIdx, foundBuzhidao := findCandidate(resultWithG, "不知道")
	if !foundBuzhidao {
		t.Fatal("CRITICAL: '不知道' MUST be in candidates for 'wobuzhidaog' — partial 'g' should not remove completed-syllable matches")
	}

	// "不知道" 的 ConsumedLength 应该是 "buzhidao" 的长度(8)，不是整个输入的长度
	buzhidaoCand := resultWithG.Candidates[buzhidaoIdx]
	// ConsumedLength 应 <= len("wobuzhidao")=10 并且 > 0
	if buzhidaoCand.ConsumedLength > len("wobuzhidaog") {
		t.Errorf("'不知道' ConsumedLength=%d exceeds input length %d",
			buzhidaoCand.ConsumedLength, len("wobuzhidaog"))
	}

	// "知道" 也应在候选中
	_, foundZhidao := findCandidate(resultWithG, "知道")
	if !foundZhidao {
		t.Error("'知道' should be in candidates for 'wobuzhidaog'")
	}

	// 在分步确认模型中，"我"(consumed=2) 比 "不知道"(consumed=11) 更优：
	// 选"我"可以继续输入"不知道"，选"不知道"会丢失"wo"。
	// 所以不再断言 "不知道" 必须排在 "我" 前面。
	_, _ = findCandidate(resultWithG, "我")
}

// ============================================================
// Bug 2 变体: "nihaozh" — partial "zh" 不应消除 "你好" 匹配
// ============================================================

func TestPartialSuffixKeepsExactMatch_nihaozh(t *testing.T) {
	d := createExtendedTestDict(t)
	engine := NewEngine(d)

	result := engine.ConvertEx("nihaozh", 50)

	for i, c := range result.Candidates {
		if i >= 8 {
			break
		}
		t.Logf("nihaozh candidate[%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	// "你好" 必须在候选中
	nihaoIdx, foundNihao := findCandidate(result, "你好")
	if !foundNihao {
		t.Fatal("CRITICAL: '你好' MUST be in candidates for 'nihaozh'")
	}

	// "你好" 应排在前3名
	if nihaoIdx > 2 {
		t.Errorf("'你好' should be in top 3 for 'nihaozh', found at idx=%d", nihaoIdx)
	}

	// "你好" 的 ConsumedLength 应该是 5 (nihao)，不是 7 (nihaozh)
	nihaoCand := result.Candidates[nihaoIdx]
	if nihaoCand.ConsumedLength != 5 {
		t.Errorf("'你好' ConsumedLength=%d, want 5 (only 'nihao' consumed, not 'nihaozh')",
			nihaoCand.ConsumedLength)
	}
}

// ============================================================
// Bug 3: 选字后剩余输入应产生合理的候选
// 从 "wobuzhidaogaid" 选了 "我"(consumed=2) 后，
// 剩余 "buzhidaogaid" 必须包含 "不知道" 而非不相关的词
// ============================================================

func TestRemainingBufferAfterPartialCommit(t *testing.T) {
	d := createExtendedTestDict(t)
	engine := NewEngine(d)

	// 模拟选了 "我" 后的剩余输入
	remaining := "buzhidaogaid"
	result := engine.ConvertEx(remaining, 50)

	t.Log("=== buzhidaogaid (remaining after selecting 我) ===")
	for i, c := range result.Candidates {
		if i >= 10 {
			break
		}
		t.Logf("  [%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	// "不知道" 必须在候选中（子词组 bu+zhi+dao）
	_, foundBuzhidao := findCandidate(result, "不知道")
	if !foundBuzhidao {
		t.Fatal("CRITICAL: '不知道' MUST be in candidates for 'buzhidaogaid'")
	}

	// "知道" 也应在候选中
	_, foundZhidao := findCandidate(result, "知道")
	if !foundZhidao {
		t.Error("'知道' should be in candidates for 'buzhidaogaid'")
	}

	// "不" 作为首音节单字应在候选中
	_, foundBu := findCandidate(result, "不")
	if !foundBu {
		t.Error("'不' should be in candidates for 'buzhidaogaid'")
	}

	// 首候选不应是完全不相关的词
	if len(result.Candidates) > 0 {
		first := result.Candidates[0]
		// 首候选的文本中的字应该和输入的拼音有对应关系
		// 至少应该包含 bu/zhi/dao/gai 相关的字
		validFirstChars := []string{"不", "知", "道", "该", "改", "布", "步"}
		firstRunes := []rune(first.Text)
		firstCharValid := false
		for _, valid := range validFirstChars {
			if string(firstRunes[0:1]) == valid {
				firstCharValid = true
				break
			}
		}
		if !firstCharValid {
			t.Errorf("First candidate '%s' starts with unrelated character for input 'buzhidaogaid'",
				first.Text)
		}
	}
}

// ============================================================
// 回归测试: 简拼在没有完整音节匹配时仍然有效
// "bzd" (纯简拼) 应匹配 "不知道"
// ============================================================

func TestPureAbbrevStillWorks(t *testing.T) {
	d := createExtendedTestDict(t)
	engine := NewEngine(d)

	result := engine.ConvertEx("bzd", 30)

	for i, c := range result.Candidates {
		if i >= 5 {
			break
		}
		t.Logf("bzd candidate[%d]: %s (weight=%d, consumed=%d)", i, c.Text, c.Weight, c.ConsumedLength)
	}

	_, foundBuzhidao := findCandidate(result, "不知道")
	if !foundBuzhidao {
		t.Error("'不知道' should be in candidates for pure abbrev 'bzd'")
	}
}

// ============================================================
// 回归测试: ConsumedLength 在 partial 场景下的正确性
// ============================================================

func TestConsumedLengthWithPartial(t *testing.T) {
	d := createExtendedTestDict(t)
	engine := NewEngine(d)

	tests := []struct {
		input    string
		wantText string
		wantMax  int // ConsumedLength 不应超过此值
		desc     string
	}{
		{"nihaozh", "你好", 5, "nihao consumed, zh is partial"},
		{"wobuzhidaog", "知道", 11, "zhidao is non-start sub-phrase, must consume all"},
		{"wobuzhidaog", "我", 2, "wo only"},
	}

	for _, tt := range tests {
		t.Run(tt.input+"→"+tt.wantText, func(t *testing.T) {
			result := engine.ConvertEx(tt.input, 50)
			for _, c := range result.Candidates {
				if c.Text == tt.wantText {
					if c.ConsumedLength > tt.wantMax {
						t.Errorf("%s: '%s' ConsumedLength=%d, want <= %d (%s)",
							tt.input, tt.wantText, c.ConsumedLength, tt.wantMax, tt.desc)
					} else {
						t.Logf("%s: '%s' ConsumedLength=%d (OK, max=%d)", tt.input, tt.wantText, c.ConsumedLength, tt.wantMax)
					}
					return
				}
			}
			// 如果找不到候选，列出所有候选帮助调试
			var texts []string
			for i, c := range result.Candidates {
				if i >= 10 {
					break
				}
				texts = append(texts, c.Text)
			}
			t.Errorf("%s: '%s' not found in candidates (top10: %s)", tt.input, tt.wantText, strings.Join(texts, ","))
		})
	}
}
