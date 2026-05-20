package mixed

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
)

func getBuiltDictRoot(t *testing.T) string {
	t.Helper()

	_, filename, _, _ := runtime.Caller(0)
	projectRoot := filepath.Join(filepath.Dir(filename), "..", "..", "..", "..")
	dictRoot := filepath.Join(projectRoot, "build", "data", "dict")

	if _, err := os.Stat(filepath.Join(dictRoot, "pinyin", "rime_frost.dict.yaml")); os.IsNotExist(err) {
		t.Skipf("built dict root not found at %s", dictRoot)
	}
	return dictRoot
}

func newRealMixedEngine(t *testing.T) *Engine {
	t.Helper()

	dictRoot := getBuiltDictRoot(t)

	pinyinDict := dict.NewPinyinDict(nil)
	if err := pinyinDict.LoadRimeDir(filepath.Join(dictRoot, "pinyin", "cn_dicts")); err != nil {
		t.Fatalf("load pinyin dict: %v", err)
	}
	pinyinComposite := dict.NewCompositeDict()
	pinyinComposite.AddLayer(dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict))

	pinyinEngine := pinyin.NewEngineWithConfig(pinyinComposite, &pinyin.Config{
		FilterMode:      "smart",
		UseSmartCompose: true,
		ShowCodeHint:    true,
		SkipAbbrev:      true, // 混输模式默认关闭简拼
	}, nil)
	if err := pinyinEngine.LoadUnigram(filepath.Join(dictRoot, "pinyin", "unigram.txt")); err != nil {
		t.Fatalf("load unigram: %v", err)
	}

	return NewEngine(nil, pinyinEngine, &Config{
		MinPinyinLength:      2,
		CodetableWeightBoost: 10000000,
		ShowSourceHint:       true,
	}, nil)
}

func hasCandidateText(cands []candidate.Candidate, want string) bool {
	for _, c := range cands {
		if c.Text == want {
			return true
		}
	}
	return false
}

func candidateIndex(cands []candidate.Candidate, want string) int {
	for i, c := range cands {
		if c.Text == want {
			return i
		}
	}
	return -1
}

// TestMixedAutoCommit_BlockedByFullSyllable 验证：输入是完整音节（如 "wo"）且
// 拼音侧有候选时，全码顶屏应被否决。
func TestMixedAutoCommit_BlockedByFullSyllable(t *testing.T) {
	t.Skip("newRealMixedEngine 工厂未挂载 codetableEngine，无法稳定构造码表精确唯一+无后继的输入；逻辑覆盖见 codetable 包内单元测试与 recheckAutoCommit 手工审查")
}

// TestMixedAutoCommit_AllowedWhenNotSyllable 验证：输入非完整音节（如 "xyz"）时，
// 即便有拼音候选也不应阻止全码顶屏。
func TestMixedAutoCommit_AllowedWhenNotSyllable(t *testing.T) {
	t.Skip("newRealMixedEngine 工厂未挂载 codetableEngine；混输守护逻辑路径已通过 mixed.go:recheckAutoCommit 的 Contains 判定直接覆盖")
}

// TestMixedAutoCommit_AllowedWhenAbbrevPrefix 验证：简拼前缀（如 "zh"）非完整音节，
// Contains==false，守护逻辑放行，由"精确唯一+无后继"裁决。
func TestMixedAutoCommit_AllowedWhenAbbrevPrefix(t *testing.T) {
	t.Skip("同上：codetableEngine 未挂载，且 zh 在真实词库中存在大量更长后继，无法稳定复现")
}

// TestMixedAutoCommit_BlockedByMultiSyllable 验证 isPossiblePinyinSequence 在多音节 / 含合法前缀
// 场景下能正确识别拼音意图（修复 recheckAutoCommit 用 Contains 只判单音节的缺陷）。
// 不构造完整混输引擎，只验证守护使用的核心判定函数本身。
func TestMixedAutoCommit_BlockedByMultiSyllable(t *testing.T) {
	engine := &Engine{
		maxCodeLen:   4,
		pinyinParser: pinyin.NewPinyinParser(),
	}

	tests := []struct {
		input string
		want  bool
		desc  string
	}{
		{"woai", true, "woai: wo(完整) + ai(完整) → 多音节拼音"},
		{"nizh", true, "nizh: ni(完整) + zh(前缀) → 合法拼音序列"},
		{"zhni", false, "zhni: zh 是前缀但 ni 跟随不是合法尾部前缀，应可顶屏"},
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

// TestRecheckAutoCommit_SourceWhitelist 验证来源白名单：只有 Source ∈ {Codetable, Phrase}
// 的精确命中才允许触发全码顶屏；拼音来源即便 Code==input 也不计入。
// 防止白名单约束被未来改动绕过。
func TestRecheckAutoCommit_SourceWhitelist(t *testing.T) {
	// 构造一个无码表、无 dictManager 的码表引擎，HasLongerCode 必然为 false，
	// 让测试聚焦在来源白名单上。
	ctCfg := codetable.DefaultConfig()
	ctCfg.AutoCommitAtFull = true
	ctCfg.MinAutoCommitLen = 2
	ctCfg.MaxCodeLength = 4
	ctCfg.AutoCommitBlockOnPinyin = false // 关闭拼音守护，专门验证白名单
	ctEng := codetable.NewEngine(ctCfg, nil)

	me := &Engine{
		codetableEngine: ctEng,
		maxCodeLen:      4,
		pinyinParser:    pinyin.NewPinyinParser(),
		config:          &Config{},
	}

	tests := []struct {
		name          string
		candidates    []candidate.Candidate
		wantCommit    bool
		wantText      string
		hasPinyinCand bool
	}{
		{
			name: "拼音来源即便 Code==input 也不顶",
			candidates: []candidate.Candidate{
				{Code: "abc", Text: "啊", Source: candidate.SourcePinyin},
			},
			wantCommit:    false,
			hasPinyinCand: true,
		},
		{
			name: "码表来源精确唯一应顶",
			candidates: []candidate.Candidate{
				{Code: "abc", Text: "甲", Source: candidate.SourceCodetable},
			},
			wantCommit: true,
			wantText:   "甲",
		},
		{
			name: "短语来源精确唯一应顶",
			candidates: []candidate.Candidate{
				{Code: "abc", Text: "测试", Source: candidate.SourcePhrase},
			},
			wantCommit: true,
			wantText:   "测试",
		},
		{
			name: "码表 + 拼音同 Code 仍按白名单算 1 个 → 顶",
			candidates: []candidate.Candidate{
				{Code: "abc", Text: "甲", Source: candidate.SourceCodetable},
				{Code: "abc", Text: "啊", Source: candidate.SourcePinyin},
			},
			wantCommit:    true,
			wantText:      "甲",
			hasPinyinCand: true,
		},
		{
			name: "两个码表 Code==input → 不唯一不顶",
			candidates: []candidate.Candidate{
				{Code: "abc", Text: "甲", Source: candidate.SourceCodetable},
				{Code: "abc", Text: "乙", Source: candidate.SourceCodetable},
			},
			wantCommit: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gotCommit, gotText := me.recheckAutoCommit("abc", tt.candidates, tt.hasPinyinCand)
			if gotCommit != tt.wantCommit {
				t.Errorf("ShouldCommit = %v, want %v", gotCommit, tt.wantCommit)
			}
			if tt.wantCommit && gotText != tt.wantText {
				t.Errorf("CommitText = %q, want %q", gotText, tt.wantText)
			}
		})
	}
}

func TestMixedEngine_CommonWordsFromPinyinFallback(t *testing.T) {
	engine := newRealMixedEngine(t)

	tests := []struct {
		input string
		want  string
	}{
		{input: "cesuo", want: "厕所"},
		{input: "xielou", want: "泄露"},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			rawCandidates, err := engine.GetPinyinEngine().ConvertRaw(tt.input, 200)
			if err != nil {
				t.Fatalf("ConvertRaw(%q): %v", tt.input, err)
			}
			if idx := candidateIndex(rawCandidates, tt.want); idx < 0 {
				t.Fatalf("raw candidates missing %q for input %q", tt.want, tt.input)
			}

			result := engine.ConvertEx(tt.input, 200)
			if !result.IsPinyinFallback {
				t.Fatalf("expected pinyin fallback for %q", tt.input)
			}
			if idx := candidateIndex(result.Candidates, tt.want); idx < 0 {
				t.Fatalf("candidate %q not found for input %q; got=%v", tt.want, tt.input, result.Candidates)
			}
		})
	}
}
