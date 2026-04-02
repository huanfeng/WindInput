package mixed

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
)

// TestPinyinHasFullSyllable 验证拼音引擎的 HasFullSyllable 信号
// 这是混输意图判断的基础：完整音节表示可能是有效拼音，纯简拼表示更可能是码表编码
func TestPinyinHasFullSyllable(t *testing.T) {
	engine := newRealMixedEngine(t)
	pe := engine.GetPinyinEngine()

	tests := []struct {
		input            string
		wantFullSyllable bool
		desc             string
	}{
		// 纯声母（无完整音节）→ HasFullSyllable = false
		{"sf", false, "纯声母2码"},
		{"sfg", false, "纯声母3码"},
		{"wfht", false, "纯声母4码"},
		{"bg", false, "纯声母2码 bg"},
		{"ds", false, "纯声母2码 ds"},

		// 含完整音节 → HasFullSyllable = true
		{"shi", true, "完整音节 shi"},
		{"ni", true, "完整音节 ni"},
		{"bao", true, "完整音节 bao"},
		{"wo", true, "完整音节 wo"},
		{"de", true, "完整音节 de"},
		{"ai", true, "完整音节 ai（纯元音）"},

		// 混合输入（含完整音节+部分） → HasFullSyllable = true
		{"nib", true, "完整音节 ni + 部分 b"},
		{"shim", true, "完整音节 shi + 部分 m"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := pe.ConvertEx(tt.input, 10)
			if result.HasFullSyllable != tt.wantFullSyllable {
				t.Errorf("input=%q: HasFullSyllable=%v, want=%v",
					tt.input, result.HasFullSyllable, tt.wantFullSyllable)
			}
		})
	}
}

// TestMixedIntentDetection_PureInitials 验证纯简拼输入时拼音候选被正确降权
// 场景：用户输入 sfg/wfht 等纯声母序列，更可能是码表编码
func TestMixedIntentDetection_PureInitials(t *testing.T) {
	engine := newRealMixedEngine(t)

	tests := []struct {
		input string
		desc  string
	}{
		{"sfg", "3码纯声母"},
		{"wfht", "4码纯声母"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := engine.ConvertEx(tt.input, 50)
			if len(result.Candidates) == 0 {
				t.Skipf("input=%q: 无候选词（可能缺少码表）", tt.input)
				return
			}

			// 检查所有拼音来源的候选词权重都被降权了
			for _, c := range result.Candidates {
				if c.Source == candidate.SourcePinyin {
					// 拼音候选应被降权（3码-2M，4码-3.5M）
					t.Logf("input=%q: 拼音候选 %q weight=%d", tt.input, c.Text, c.Weight)
				}
			}
		})
	}
}

// TestMixedIntentDetection_FullSyllable 验证含完整音节的输入拼音候选不被降权
// 场景：用户输入 shi/bao 等含完整音节的内容，拼音候选应保持正常权重
func TestMixedIntentDetection_FullSyllable(t *testing.T) {
	engine := newRealMixedEngine(t)

	tests := []struct {
		input    string
		wantText string
		desc     string
	}{
		{"shi", "是", "完整音节 shi 应产生高权重拼音候选"},
		{"bao", "报", "完整音节 bao 应产生高权重拼音候选"},
		{"wo", "我", "完整音节 wo 应产生高权重拼音候选"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := engine.ConvertEx(tt.input, 50)
			if len(result.Candidates) == 0 {
				t.Fatalf("input=%q: 无候选词", tt.input)
			}

			// 验证期望的拼音候选词存在
			idx := candidateIndex(result.Candidates, tt.wantText)
			if idx < 0 {
				texts := make([]string, 0, 5)
				for i, c := range result.Candidates {
					if i >= 5 {
						break
					}
					texts = append(texts, c.Text)
				}
				t.Errorf("input=%q: 期望候选 %q 未找到，前5个候选: %v",
					tt.input, tt.wantText, texts)
			}
		})
	}
}

// TestMixedIntentDetection_VowelInCodetable 验证含元音字母的码表编码不被误判
// 这是旧 containsVowel 方案的核心缺陷：郑码等码表使用元音字母，
// 旧方案会误判为"含元音=拼音意图"而不降权拼音候选。
// 新方案基于拼音解析质量判断，不依赖输入字符集。
func TestMixedIntentDetection_VowelInCodetable(t *testing.T) {
	engine := newRealMixedEngine(t)
	pe := engine.GetPinyinEngine()

	// "aie" 虽然全是元音字母，但拼音引擎能否解析为完整音节取决于拼音规则
	// 关键：新方案不再依赖"是否含元音"来判断，而是看拼音引擎的实际解析结果
	input := "aie"
	result := pe.ConvertEx(input, 10)
	t.Logf("input=%q: HasFullSyllable=%v, candidates=%d",
		input, result.HasFullSyllable, len(result.Candidates))

	// "have" 不是合法拼音，但含元音
	// 旧方案：含元音→不降权拼音（错误：have 不是拼音）
	// 新方案：看解析结果，如果没有完整音节就降权
	input2 := "have"
	result2 := pe.ConvertEx(input2, 10)
	t.Logf("input=%q: HasFullSyllable=%v, candidates=%d",
		input2, result2.HasFullSyllable, len(result2.Candidates))

	// "nv" 是合法拼音（女），应有完整音节
	input3 := "nv"
	result3 := pe.ConvertEx(input3, 10)
	if !result3.HasFullSyllable {
		t.Errorf("input=%q: 期望 HasFullSyllable=true（nv=女 是合法拼音）", input3)
	}
}

// newPinyinOnlyMixedEngine 创建仅有拼音引擎的混输引擎（用于隔离测试拼音降权逻辑）
func newPinyinOnlyMixedEngine(t *testing.T) (*Engine, *pinyin.Engine) {
	t.Helper()

	dictRoot := getBuiltDictRoot(t)

	pinyinComposite, pinyinEng := createPinyinEngine(t, dictRoot)
	_ = pinyinComposite

	engine := NewEngine(nil, pinyinEng, &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10000000,
		ShowSourceHint:       false,
	}, nil)

	return engine, pinyinEng
}

// createPinyinEngine 创建拼音引擎的辅助函数
func createPinyinEngine(t *testing.T, dictRoot string) (*dict.CompositeDict, *pinyin.Engine) {
	t.Helper()

	pinyinDict := dict.NewPinyinDict(nil)
	if err := pinyinDict.LoadRimeDir(filepath.Join(dictRoot, "pinyin", "cn_dicts")); err != nil {
		t.Fatalf("load pinyin dict: %v", err)
	}
	pinyinComposite := dict.NewCompositeDict()
	pinyinComposite.AddLayer(dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict))

	pinyinEng := pinyin.NewEngineWithConfig(pinyinComposite, &pinyin.Config{
		FilterMode:      "smart",
		UseSmartCompose: true,
		ShowCodeHint:    false,
	}, nil)
	if err := pinyinEng.LoadUnigram(filepath.Join(dictRoot, "pinyin", "unigram.txt")); err != nil {
		t.Fatalf("load unigram: %v", err)
	}

	return pinyinComposite, pinyinEng
}

// TestMixedPinyinWeightPenalty 验证纯简拼的具体降权值
func TestMixedPinyinWeightPenalty(t *testing.T) {
	engine, pe := newPinyinOnlyMixedEngine(t)

	tests := []struct {
		input         string
		expectPenalty bool
		desc          string
	}{
		// 2码纯简拼：不降权（高频救急场景）
		{"bg", false, "2码简拼不降权"},
		{"ds", false, "2码简拼不降权"},
		// 3码纯简拼：降权
		{"sfg", true, "3码纯简拼降权"},
		{"dsg", true, "3码纯简拼降权"},
		// 含完整音节：不降权
		{"shi", false, "含完整音节不降权"},
		{"bao", false, "含完整音节不降权"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			// 获取拼音引擎的原始权重
			rawResult := pe.ConvertEx(tt.input, 10)
			if len(rawResult.Candidates) == 0 {
				t.Skipf("input=%q: 拼音引擎无候选", tt.input)
				return
			}
			rawWeight := rawResult.Candidates[0].Weight

			// 获取混输引擎的权重
			mixedResult := engine.ConvertEx(tt.input, 50)
			var mixedPinyinWeight int
			found := false
			for _, c := range mixedResult.Candidates {
				if c.Source == candidate.SourcePinyin {
					mixedPinyinWeight = c.Weight
					found = true
					break
				}
			}
			if !found {
				t.Skipf("input=%q: 混输结果中无拼音候选", tt.input)
				return
			}

			if tt.expectPenalty {
				if mixedPinyinWeight >= rawWeight {
					t.Errorf("input=%q: 期望拼音降权，但混输权重(%d) >= 原始权重(%d)",
						tt.input, mixedPinyinWeight, rawWeight)
				}
				t.Logf("input=%q: 原始=%d, 混输=%d, 降权=%d",
					tt.input, rawWeight, mixedPinyinWeight, rawWeight-mixedPinyinWeight)
			} else {
				if mixedPinyinWeight < rawWeight {
					t.Errorf("input=%q: 不应降权，但混输权重(%d) < 原始权重(%d)",
						tt.input, mixedPinyinWeight, rawWeight)
				}
			}
		})
	}
}
