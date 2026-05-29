package pinyin

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
)

// createNueDict 构造覆盖 ü 韵母（v 形式存储）的测试词典。
// 词库统一用 v 表示 ü：虐=nve、略=lve、女=nv、绿=lv；
// 同时含有歧义对照项 努=nu、路=lu，用于验证归一化不会误伤。
func createNueDict(t *testing.T) *dict.CompositeDict {
	t.Helper()
	tmpDir := t.TempDir()
	content := `# nue alias test
---
name: nue
version: "1.0"
sort: by_weight
...
虐	nve	1820
略	lve	5589
女	nv	9990
绿	lv	3469
努	nu	1000
路	lu	1000
待	dai	1000
虐待	nve dai	500
`
	if err := os.WriteFile(filepath.Join(tmpDir, "8105.dict.yaml"), []byte(content), 0644); err != nil {
		t.Fatalf("写测试词典失败: %v", err)
	}
	d := dict.NewPinyinDict(nil)
	if err := d.LoadRimeDir(tmpDir); err != nil {
		t.Fatalf("加载词典失败: %v", err)
	}
	return wrapInCompositeDict(d)
}

func candidateTexts(result *PinyinConvertResult) []string {
	texts := make([]string, 0, len(result.Candidates))
	for _, c := range result.Candidates {
		texts = append(texts, c.Text)
	}
	return texts
}

func candidatesContain(result *PinyinConvertResult, text string) bool {
	for _, c := range result.Candidates {
		if c.Text == text {
			return true
		}
	}
	return false
}

// TestNueLueAlias 验证 nüe/lüe 的两种输入形式（u 形式与 v 形式）均能命中词库。
// 这是本次修复的核心：用户按主流习惯输入 nue/lue 应能打出 虐/略。
func TestNueLueAlias(t *testing.T) {
	d := createNueDict(t)
	engine := NewEngine(d, nil)

	cases := []struct {
		input string
		want  string
	}{
		{"nue", "虐"},     // u 形式（修复目标）
		{"nve", "虐"},     // v 形式（回归保护）
		{"lue", "略"},     // u 形式
		{"lve", "略"},     // v 形式
		{"nuedai", "虐待"}, // 多音节，u 形式
		{"nvedai", "虐待"}, // 多音节，v 形式
	}
	for _, c := range cases {
		result := engine.ConvertEx(c.input, 20)
		if !candidatesContain(result, c.want) {
			t.Errorf("ConvertEx(%q): 期望含 %q, 实际候选=%v", c.input, c.want, candidateTexts(result))
		}
	}
}

// TestNuePreeditShowsOriginal 验证输入 nue 时预编辑区原样显示 "nue"，
// 而非归一化后的 "nve"（归一化只应发生在词库查询，不影响显示）。
func TestNuePreeditShowsOriginal(t *testing.T) {
	d := createNueDict(t)
	engine := NewEngine(d, nil)

	result := engine.ConvertEx("nue", 20)
	if result.Composition.PreeditText != "nue" {
		t.Errorf("preedit 应原样显示 \"nue\", 实际=%q", result.Composition.PreeditText)
	}
}

// TestNuLuNotAliased 验证有歧义的 nu/lu 不被归一化为 nv/lv：
// nu 仍是 努（非 女/虐），lu 仍是 路（非 绿/略）。
// 这是主流输入法的边界：nu/lu 因与 nü/lü 歧义，不当作 ü 处理。
func TestNuLuNotAliased(t *testing.T) {
	d := createNueDict(t)
	engine := NewEngine(d, nil)

	result := engine.ConvertEx("nu", 20)
	if !candidatesContain(result, "努") {
		t.Errorf("ConvertEx(\"nu\"): 期望含 努, 实际候选=%v", candidateTexts(result))
	}
	if candidatesContain(result, "虐") {
		t.Errorf("ConvertEx(\"nu\"): 不应含 虐（nu 不应被归一化为 nve）, 实际候选=%v", candidateTexts(result))
	}

	result = engine.ConvertEx("lu", 20)
	if !candidatesContain(result, "路") {
		t.Errorf("ConvertEx(\"lu\"): 期望含 路, 实际候选=%v", candidateTexts(result))
	}
	if candidatesContain(result, "略") {
		t.Errorf("ConvertEx(\"lu\"): 不应含 略（lu 不应被归一化为 lve）, 实际候选=%v", candidateTexts(result))
	}
}
