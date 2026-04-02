package pinyin

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
)

func createTestDict(t *testing.T) *dict.CompositeDict {
	t.Helper()
	tmpDir := t.TempDir()

	content := `# Rime dictionary
---
name: test
version: "1.0"
sort: by_weight
...
啊	a	1000
阿	a	900
爱	ai	1000
哀	ai	900
你	ni	1000
泥	ni	900
好	hao	1000
号	hao	900
你好	ni hao	800
我	wo	1000
们	men	1000
我们	wo men	800
是	shi	1000
的	de	1000
了	le	1000
不	bu	1000
知	zhi	1000
道	dao	1000
知道	zhi dao	800
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

func TestEngineConvert(t *testing.T) {
	d := createTestDict(t)
	engine := NewEngine(d, nil)

	tests := []struct {
		input    string
		wantText string
	}{
		{"ni", "你"},
		{"hao", "好"},
		{"nihao", "你好"},
		{"wo", "我"},
		{"shi", "是"},
		{"women", "我们"},
		{"zhidao", "知道"},
	}

	for _, tt := range tests {
		candidates, err := engine.ConvertRaw(tt.input, 10)
		if err != nil {
			t.Errorf("Convert(%q) error: %v", tt.input, err)
			continue
		}
		if len(candidates) == 0 {
			t.Errorf("Convert(%q) 无候选词", tt.input)
			continue
		}
		found := false
		for _, c := range candidates {
			if c.Text == tt.wantText {
				found = true
				break
			}
		}
		if !found {
			texts := make([]string, len(candidates))
			for i, c := range candidates {
				texts[i] = c.Text
			}
			t.Errorf("Convert(%q) 未找到 %q, 得到 %v", tt.input, tt.wantText, texts)
		}
	}
}

func TestEngineConvertEmpty(t *testing.T) {
	d := createTestDict(t)
	engine := NewEngine(d, nil)

	candidates, err := engine.Convert("", 10)
	if err != nil {
		t.Errorf("Convert('') error: %v", err)
	}
	if len(candidates) != 0 {
		t.Errorf("Convert('') 应返回空, 得到 %d 条", len(candidates))
	}
}

func TestEngineConvertWithRealDict(t *testing.T) {
	// 使用 Rime 词库测试
	paths := []string{
		"D:/Develop/workspace/go_dev/WindInput/build/dict/pinyin",
		"../../build/dict/pinyin",
	}

	var dictPath string
	for _, p := range paths {
		if _, err := os.Stat(filepath.Join(p, "8105.dict.yaml")); err == nil {
			dictPath = p
			break
		}
	}

	if dictPath == "" {
		t.Skip("跳过测试：无法找到 Rime 词库目录")
	}

	pinyinDict := dict.NewPinyinDict(nil)
	if err := pinyinDict.LoadRimeDir(dictPath); err != nil {
		t.Fatalf("加载词库失败: %v", err)
	}
	t.Logf("词库加载成功，词条数: %d", pinyinDict.EntryCount())

	// 使用 CompositeDict
	composite := dict.NewCompositeDict()
	layer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
	composite.AddLayer(layer)

	// 创建引擎（关闭 smart 过滤，先测试无过滤情况）
	config := &Config{
		ShowCodeHint:    false,
		FilterMode:      "all",
		UseSmartCompose: false,
	}
	engine := NewEngineWithConfig(composite, config, nil)

	tests := []struct {
		input    string
		wantText string
	}{
		{"women", "我们"},
		{"zhongguo", "中国"},
		{"nihao", "你好"},
	}

	for _, tt := range tests {
		t.Logf("--- 测试输入: %q ---", tt.input)

		// 测试 ConvertRaw（无过滤）
		candidatesRaw, err := engine.ConvertRaw(tt.input, 10)
		if err != nil {
			t.Errorf("ConvertRaw(%q) error: %v", tt.input, err)
			continue
		}
		t.Logf("ConvertRaw: %d 个候选", len(candidatesRaw))
		for i, c := range candidatesRaw {
			if i < 5 {
				t.Logf("  [%d] text=%q weight=%d isCommon=%v", i, c.Text, c.Weight, c.IsCommon)
			}
		}

		foundRaw := false
		for _, c := range candidatesRaw {
			if c.Text == tt.wantText {
				foundRaw = true
				break
			}
		}
		if !foundRaw {
			t.Errorf("ConvertRaw(%q) 未找到 %q", tt.input, tt.wantText)
		}

		// 测试 Convert（有过滤）
		candidates, err := engine.Convert(tt.input, 10)
		if err != nil {
			t.Errorf("Convert(%q) error: %v", tt.input, err)
			continue
		}
		t.Logf("Convert: %d 个候选", len(candidates))
		for i, c := range candidates {
			if i < 5 {
				t.Logf("  [%d] text=%q weight=%d isCommon=%v", i, c.Text, c.Weight, c.IsCommon)
			}
		}

		found := false
		for _, c := range candidates {
			if c.Text == tt.wantText {
				found = true
				break
			}
		}
		if !found {
			t.Errorf("Convert(%q) 未找到 %q", tt.input, tt.wantText)
		}
	}
}

func TestExplicitSeparator_XiAn(t *testing.T) {
	// 测试显式分隔符 xi'an 的候选排序：西安应排在前列
	paths := []string{
		"D:/Develop/workspace/go_dev/WindInput/build/dict/pinyin",
		"../../build/dict/pinyin",
	}

	var dictPath string
	for _, p := range paths {
		if _, err := os.Stat(filepath.Join(p, "8105.dict.yaml")); err == nil {
			dictPath = p
			break
		}
	}
	if dictPath == "" {
		t.Skip("跳过测试：无法找到 Rime 词库目录")
	}

	pinyinDict := dict.NewPinyinDict(nil)
	if err := pinyinDict.LoadRimeDir(dictPath); err != nil {
		t.Fatalf("加载词库失败: %v", err)
	}

	composite := dict.NewCompositeDict()
	layer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
	composite.AddLayer(layer)

	config := &Config{
		FilterMode:      "all",
		UseSmartCompose: false,
	}
	engine := NewEngineWithConfig(composite, config, nil)

	// 测试带显式分隔符的输入
	input := "xi'an"
	result := engine.ConvertEx(input, 50)

	t.Logf("输入: %q, 候选数: %d", input, len(result.Candidates))
	for i, c := range result.Candidates {
		if i < 15 {
			t.Logf("  [%d] text=%q code=%q weight=%d consumed=%d", i, c.Text, c.Code, c.Weight, c.ConsumedLength)
		}
	}

	// 西安必须出现在前 3 个候选中
	found := false
	for i, c := range result.Candidates {
		if c.Text == "西安" {
			found = true
			if i > 2 {
				t.Errorf("西安 排在第 %d 位，期望在前 3 位", i+1)
			} else {
				t.Logf("西安 排在第 %d 位 ✓", i+1)
			}
			break
		}
	}
	if !found {
		t.Errorf("未找到候选 西安")
	}

	// 同时测试不带分隔符的 xian
	input2 := "xian"
	result2 := engine.ConvertEx(input2, 50)
	t.Logf("\n输入: %q, 候选数: %d", input2, len(result2.Candidates))
	for i, c := range result2.Candidates {
		if i < 10 {
			t.Logf("  [%d] text=%q code=%q weight=%d consumed=%d", i, c.Text, c.Code, c.Weight, c.ConsumedLength)
		}
	}
}

func TestLeadingPartialCandidates(t *testing.T) {
	// 测试 sdem/sdeo 等首段 partial 音节输入的候选排序
	paths := []string{
		"../../build/dict/pinyin",
	}

	var dictPath string
	for _, p := range paths {
		if _, err := os.Stat(filepath.Join(p, "8105.dict.yaml")); err == nil {
			dictPath = p
			break
		}
	}
	if dictPath == "" {
		t.Skip("跳过测试：无法找到 Rime 词库目录")
	}

	pinyinDict := dict.NewPinyinDict(nil)
	if err := pinyinDict.LoadRimeDir(dictPath); err != nil {
		t.Fatalf("加载词库失败: %v", err)
	}

	composite := dict.NewCompositeDict()
	layer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
	composite.AddLayer(layer)

	config := &Config{
		FilterMode:      "all",
		UseSmartCompose: false,
	}
	engine := NewEngineWithConfig(composite, config, nil)

	for _, input := range []string{"sdem", "sdeo", "sde", "bdem"} {
		result := engine.ConvertEx(input, 50)
		t.Logf("\n输入: %q, preedit=%q, 候选数: %d", input, result.PreeditDisplay, len(result.Candidates))
		for i, c := range result.Candidates {
			if i < 15 {
				t.Logf("  [%d] text=%q code=%q weight=%d consumed=%d", i, c.Text, c.Code, c.Weight, c.ConsumedLength)
			}
		}

		// 首个候选不应该是 "的" (de)，因为第一个字符不是 d
		if len(result.Candidates) > 0 {
			first := result.Candidates[0]
			if first.Text == "的" {
				t.Errorf("输入 %q: 首候选不应为 '的'，第一字符是 %c", input, input[0])
			}
		}
	}
}

func TestSyllablesPruning(t *testing.T) {
	// 测试长输入不会导致指数级爆炸
	results := ParseSyllables("zhongguorenminjiefangjun")
	if len(results) > maxResults {
		t.Errorf("ParseSyllables 返回 %d 种分割, 超过上限 %d", len(results), maxResults)
	}
}
