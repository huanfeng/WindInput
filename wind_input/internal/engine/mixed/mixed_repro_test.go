package mixed

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
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
