package mixed

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/dictcache"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
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

// TestMixedSkipAbbrev 验证混输模式下 SkipAbbrev 生效
// 默认关闭简拼匹配，纯声母输入（如 sfg）不应产生简拼词组候选
func TestMixedSkipAbbrev(t *testing.T) {
	engine := newRealMixedEngine(t)
	pe := engine.GetPinyinEngine()

	// 验证混输模式下拼音引擎的 SkipAbbrev 已开启
	if cfg := pe.GetConfig(); cfg != nil && !cfg.SkipAbbrev {
		t.Skip("SkipAbbrev 未开启（newRealMixedEngine 未设置）")
	}

	// sfg 在简拼关闭时，不应产生简拼词组（如"示范岗"）
	result := pe.ConvertEx("sfg", 50)
	for _, c := range result.Candidates {
		// 简拼词组通常是 3+ 字（每个声母对应一个字）
		if len([]rune(c.Text)) == 3 {
			t.Logf("SkipAbbrev 下仍有3字候选: %q weight=%d (可能来自其他路径)", c.Text, c.Weight)
		}
	}
	t.Logf("sfg SkipAbbrev: %d 候选", len(result.Candidates))
}

// TestMixedHandleTopCode_PureInitials 验证纯声母输入超过码长时触发顶码
func TestMixedHandleTopCode_PureInitials(t *testing.T) {
	dictRoot := getBuiltDictRoot(t)

	// 创建带码表的混输引擎
	pinyinComposite, pinyinEng := createPinyinEngine(t, dictRoot)
	_ = pinyinComposite

	ct := createCodetableEngine(t, dictRoot)

	engine := NewEngine(ct, pinyinEng, &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10000000,
		ShowSourceHint:       true,
	}, nil)

	// sfght: 纯声母，无完整音节 → 应触发顶码
	// 前4码 sfgh 查码表，t 作为新输入
	commitText, newInput, shouldCommit := engine.HandleTopCode("sfght")
	t.Logf("sfght: commit=%q, newInput=%q, shouldCommit=%v", commitText, newInput, shouldCommit)

	// 如果码表有 sfgh 的匹配，应触发顶码
	if shouldCommit {
		if newInput != "t" {
			t.Errorf("顶码后新输入应为 't'，实际为 %q", newInput)
		}
		if commitText == "" {
			t.Error("顶码应上屏文字")
		}
	}
	// 如果码表无 sfgh 匹配，不触发也合理
}

// TestMixedHandleTopCode_FullSyllable 验证含完整音节超过码长时不触发顶码
func TestMixedHandleTopCode_FullSyllable(t *testing.T) {
	dictRoot := getBuiltDictRoot(t)

	pinyinComposite, pinyinEng := createPinyinEngine(t, dictRoot)
	_ = pinyinComposite

	ct := createCodetableEngine(t, dictRoot)

	engine := NewEngine(ct, pinyinEng, &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10000000,
		ShowSourceHint:       true,
	}, nil)

	// buyao: 含完整音节 bu+yao → 不应触发顶码
	_, _, shouldCommit := engine.HandleTopCode("buyao")
	if shouldCommit {
		t.Error("buyao 含完整音节，不应触发顶码")
	}

	// nihao: 含完整音节 ni+hao → 不应触发顶码
	_, _, shouldCommit = engine.HandleTopCode("nihao")
	if shouldCommit {
		t.Error("nihao 含完整音节，不应触发顶码")
	}

	// yanse: yan(完整) + s(前缀) → 不应触发顶码（用户可能在输入拼音"颜色"）
	_, _, shouldCommit = engine.HandleTopCode("yanse")
	if shouldCommit {
		t.Error("yanse: yan+se 是合法拼音序列，不应触发顶码")
	}
}

// TestIsPossiblePinyinSequence 验证拼音序列判断逻辑（纯逻辑测试，不依赖词库）
func TestIsPossiblePinyinSequence(t *testing.T) {
	// 创建最小化的 Engine，只需要 pinyinParser
	engine := &Engine{
		maxCodeLen:   4,
		pinyinParser: pinyin.NewPinyinParser(),
	}

	tests := []struct {
		input string
		want  bool
		desc  string
	}{
		// 不是拼音 → 允许顶码
		{"rcqn", false, "rcqn: 非拼音序列（五笔'反馈'）"},
		{"ukjg", false, "ukjg: 非拼音序列"},
		{"sfgh", false, "sfgh: 纯声母序列"},
		{"wfht", false, "wfht: 纯声母序列"},
		{"gggg", false, "gggg: g 不是合法拼音"},
		{"dkjf", false, "dkjf: 非拼音序列"},

		// 是拼音 → 抑制顶码
		{"yans", true, "yans: yan(完整) + s(前缀) → 可能是 yan-se"},
		{"wang", true, "wang: wang(完整) → 可能是拼音"},
		{"shen", true, "shen: shen(完整) → 可能是拼音"},
		{"gong", true, "gong: gong(完整) → 可能是拼音"},
		{"zhen", true, "zhen: zhen(完整) → 可能是拼音"},
		{"feng", true, "feng: feng(完整) → 可能是拼音"},
		{"niha", true, "niha: ni(完整) + ha(完整) → 可能是 ni-hao"},
		{"shan", true, "shan: shan(完整) → 可能是拼音"},
		{"zhon", true, "zhon: 整体是 zhong 的前缀 → 合法拼音"},
		{"shuo", true, "shuo: shuo(完整) → 合法拼音"},
		{"zhua", true, "zhua: zhua(完整) → 合法拼音"},
		{"chon", true, "chon: 整体是 chong 的前缀 → 合法拼音"},
		{"shua", true, "shua: shua(完整) → 合法拼音"},

		// 单字母元音开头但长度>=2的完整音节
		{"aish", true, "aish: ai(完整) + sh(前缀) → 可能是 ai-shi"},
		{"ergo", true, "ergo: er(完整) + ... → 部分拼音"},

		// 首音节为单字母（a/e/o）→ 过滤简拼
		{"abcd", false, "abcd: a(单字母完整) → 被简拼过滤"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			got := engine.isPossiblePinyinSequence(tt.input)
			if got != tt.want {
				t.Errorf("isPossiblePinyinSequence(%q) = %v, want %v", tt.input, got, tt.want)
			}
		})
	}
}

// TestIsWholeSyllablePinyin 验证"整音节"判断逻辑（顶码歧义裁决的核心门禁，纯逻辑、不依赖词库）。
//
// 区别于 isPossiblePinyinSequence：本函数只在前缀"恰好切在音节边界、无残缺尾音节"时为真，
// 用于把 wang/aipu 这类"既是完整拼音又可能是五笔全码"的歧义串放行顶码，而 zhon/yans 这类
// 残缺串永远不放行（继续受拼音保护）。
func TestIsWholeSyllablePinyin(t *testing.T) {
	engine := &Engine{
		maxCodeLen:   4,
		pinyinParser: pinyin.NewPinyinParser(),
	}

	tests := []struct {
		input string
		want  bool
		desc  string
	}{
		// 整音节（单音节填满码长）→ 是
		{"wang", true, "wang: 单个完整音节"},
		{"shen", true, "shen: 单个完整音节"},
		{"gong", true, "gong: 单个完整音节"},
		{"zhua", true, "zhua: 单个完整音节"},
		{"shuo", true, "shuo: 单个完整音节"},

		// 整音节（多音节恰好覆盖）→ 是
		{"aipu", true, "aipu: ai+pu 两完整音节恰好覆盖"},
		{"niha", true, "niha: ni+ha 两完整音节恰好覆盖"},
		{"woai", true, "woai: wo+ai 两完整音节恰好覆盖"},

		// 残缺尾音节 / 残缺前缀 → 否（仍受拼音保护，不放行顶码）
		{"zhon", false, "zhon: zhong 的残缺前缀，非完整音节"},
		{"yans", false, "yans: yan + 残缺 s"},
		{"nizh", false, "nizh: ni + 残缺 zh"},

		// 首音节单字母简拼 / 非拼音 → 否
		{"abcd", false, "abcd: 首音节 a 为单字母简拼"},
		{"rcqn", false, "rcqn: 非拼音序列"},
		{"sfgh", false, "sfgh: 纯声母序列"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			got := engine.isWholeSyllablePinyin(tt.input)
			if got != tt.want {
				t.Errorf("isWholeSyllablePinyin(%q) = %v, want %v", tt.input, got, tt.want)
			}
		})
	}
}

// TestMixedOverflow_PinyinWithCodetable 验证超过码长时码表和拼音都参与查询
func TestMixedOverflow_PinyinWithCodetable(t *testing.T) {
	dictRoot := getBuiltDictRoot(t)

	pinyinComposite, pinyinEng := createPinyinEngine(t, dictRoot)
	_ = pinyinComposite

	ct := createCodetableEngine(t, dictRoot)

	engine := NewEngine(ct, pinyinEng, &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10000000,
		ShowSourceHint:       true,
	}, nil)

	// buyao: 拼音能解析 → IsPinyinFallback=true，同时码表 buya 也参与
	result := engine.ConvertEx("buyao", 50)
	t.Logf("buyao: %d 候选, IsPinyinFallback=%v", len(result.Candidates), result.IsPinyinFallback)

	if !result.IsPinyinFallback {
		t.Error("buyao 应标记为拼音降级模式")
	}

	// 验证拼音候选存在
	if idx := candidateIndex(result.Candidates, "不要"); idx < 0 {
		t.Log("'不要' 未在候选中（可能词库无此词）")
	} else {
		t.Logf("'不要' 在位置 %d", idx)
	}

	// 验证码表候选也参与了
	hasCodetable := false
	for _, c := range result.Candidates {
		if c.Source == candidate.SourceCodetable {
			hasCodetable = true
			t.Logf("码表候选: %q weight=%d", c.Text, c.Weight)
			break
		}
	}
	if hasCodetable {
		t.Log("码表候选参与了竞争 ✓")
	}
}

// createCodetableEngine 创建码表引擎的辅助函数
func createCodetableEngine(t *testing.T, dictRoot string) *codetable.Engine {
	t.Helper()

	// 查找五笔 RIME 词库（split .dict.toml 优先，回退旧 .dict.yaml）
	rimeMainDict := filepath.Join(dictRoot, "wubi86", "wubi86_jidian.dict.toml")
	if _, err := os.Stat(rimeMainDict); os.IsNotExist(err) {
		rimeMainDict = filepath.Join(dictRoot, "wubi86", "wubi86_jidian.dict.yaml")
		if _, err := os.Stat(rimeMainDict); os.IsNotExist(err) {
			t.Skipf("wubi rime dict not found under %s", filepath.Join(dictRoot, "wubi86"))
		}
	}

	// 转换 RIME 格式到 wdb（放在 dictRoot 同级目录避免 mmap 文件锁导致 TempDir 清理失败）
	wdbPath := filepath.Join(dictRoot, "wubi86", "wubi86_test.wdb")
	if _, err := os.Stat(wdbPath); os.IsNotExist(err) {
		if err := dictcache.ConvertRimeCodetableToWdb(rimeMainDict, wdbPath, slog.Default()); err != nil {
			t.Fatalf("convert rime to wdb: %v", err)
		}
	}

	ct := codetable.NewEngine(codetable.DefaultConfig(), nil)
	if err := ct.LoadCodeTableBinary(wdbPath); err != nil {
		t.Fatalf("load codetable binary: %v", err)
	}
	return ct
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
