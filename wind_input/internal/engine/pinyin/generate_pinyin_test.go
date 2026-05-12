package pinyin

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/pinyin/shuangpin"
)

// createMultiPronDict 构造一个含多音字的测试词典：
//   - "费"：fei 权重 1000，bi 权重 50（生僻）
//   - "强"：qiang 权重 1000，jiang 权重 80（倔强）
//   - "晓"：xiao 权重 1000（无多读音）
//
// 旧实现按字母序优先（bi < fei、jiang < qiang）会错选生僻读音；
// 新实现按权重择优，应稳定选择 fei / qiang。
func createMultiPronDict(t *testing.T) *dict.CompositeDict {
	t.Helper()
	tmpDir := t.TempDir()
	content := `# multi-pron test
---
name: multi
version: "1.0"
sort: by_weight
...
费	fei	1000
费	bi	50
强	qiang	1000
强	jiang	80
晓	xiao	1000
你	ni	1000
好	hao	1000
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

// TestGenerateWordPinyin_MultiPronByWeight 验证多音字按权重择优。
// 回归覆盖：旧实现下 "费晓强" 会被生成成 "bixiaojiang"（费→bi、强→jiang），
// 因为 allSyllables 按字母序优先扫到 bi、jiang。
func TestGenerateWordPinyin_MultiPronByWeight(t *testing.T) {
	d := createMultiPronDict(t)
	engine := NewEngine(d, nil)

	cases := []struct {
		word string
		want string
	}{
		{"费", "fei"},
		{"强", "qiang"},
		{"费晓强", "feixiaoqiang"},
	}
	for _, c := range cases {
		got := engine.GenerateWordPinyin(c.word)
		if got != c.want {
			t.Errorf("GenerateWordPinyin(%q) = %q, want %q", c.word, got, c.want)
		}
	}
}

// stubLearning 记录学习回调收到的 (code, text)，用于验证 OnCandidateSelected 行为。
type stubLearning struct {
	calls []struct{ code, text string }
}

func (s *stubLearning) OnWordCommitted(code, text string) {
	s.calls = append(s.calls, struct{ code, text string }{code, text})
}

// TestOnCandidateSelected_ShuangpinPrefersConverter 验证双拼路径下
// 优先用 spConverter 切分用户实际按键（与本次输入一致），
// 而不是从文本反查代表读音（旧路径会因多音字而选错读音）。
//
// 场景：词典里 "费晓强" 整词不存在，但 "费"+"晓"+"强" 单字都在 fei/xiao/qiang 下，
// 用户用小鹤双拼输入 "fwxnql"，应学到 code="feixiaoqiang"。
func TestOnCandidateSelected_ShuangpinPrefersConverter(t *testing.T) {
	tmpDir := t.TempDir()
	content := `# sp test
---
name: sp
version: "1.0"
sort: by_weight
...
费	fei	1000
费	bi	50
强	qiang	1000
强	jiang	80
晓	xiao	1000
费晓强	fei xiao qiang	500
`
	if err := os.WriteFile(filepath.Join(tmpDir, "8105.dict.yaml"), []byte(content), 0644); err != nil {
		t.Fatalf("写词典失败: %v", err)
	}
	d := dict.NewPinyinDict(nil)
	if err := d.LoadRimeDir(tmpDir); err != nil {
		t.Fatalf("加载词典失败: %v", err)
	}
	engine := NewEngine(wrapInCompositeDict(d), nil)

	// 装配小鹤双拼转换器
	scheme := shuangpin.Get("xiaohe")
	if scheme == nil {
		t.Fatal("小鹤双拼方案未注册")
	}
	engine.SetShuangpinConverter(shuangpin.NewConverter(scheme))

	stub := &stubLearning{}
	engine.SetLearningStrategy(stub)

	// 模拟用户从候选选中"费晓强"，传入 code 是双拼按键序列
	engine.OnCandidateSelected("fwxnql", "费晓强")

	if len(stub.calls) != 1 {
		t.Fatalf("expected 1 learn call, got %d", len(stub.calls))
	}
	if got := stub.calls[0].code; got != "feixiaoqiang" {
		t.Errorf("learn code = %q, want %q", got, "feixiaoqiang")
	}
}

// TestOnCandidateSelected_NewWordPassesPerCharCheck 验证逐字段校验放行"造新词"。
//
// 场景：用户造"费晓强"——词典里有"费/晓/强"单字，
// 但没有"费晓强"整词。整词反查会失败，但逐字段（fei/费 + xiao/晓 + qiang/强）都能配上，
// 应当通过校验进入学习路径。
//
// 同时拦截：整词不存在且字-音节配错（如 bi/费 不在词典中）。
func TestOnCandidateSelected_NewWordPassesPerCharCheck(t *testing.T) {
	tmpDir := t.TempDir()
	content := `# per-char check
---
name: pc
version: "1.0"
sort: by_weight
...
费	fei	1000
晓	xiao	1000
强	qiang	1000
强	jiang	80
`
	if err := os.WriteFile(filepath.Join(tmpDir, "8105.dict.yaml"), []byte(content), 0644); err != nil {
		t.Fatalf("写词典失败: %v", err)
	}
	d := dict.NewPinyinDict(nil)
	if err := d.LoadRimeDir(tmpDir); err != nil {
		t.Fatalf("加载词典失败: %v", err)
	}
	engine := NewEngine(wrapInCompositeDict(d), nil)
	stub := &stubLearning{}
	engine.SetLearningStrategy(stub)

	// 造新词：整词在词典中不存在，但每个字段都能配上 → 通过
	engine.OnCandidateSelected("feixiaoqiang", "费晓强")
	// 切分错位：bi 下没有"费"字 → 拒绝
	engine.OnCandidateSelected("bixiaojiang", "费晓强")

	if len(stub.calls) != 1 {
		t.Fatalf("expected 1 learn call, got %d: %+v", len(stub.calls), stub.calls)
	}
	if got := stub.calls[0].code; got != "feixiaoqiang" {
		t.Errorf("learned code = %q, want feixiaoqiang", got)
	}
}

// TestOnCandidateSelected_RejectsUnreverseable 验证音节数与字数不匹配时拒绝。
func TestOnCandidateSelected_RejectsUnreverseable(t *testing.T) {
	tmpDir := t.TempDir()
	content := `# reverse-check test
---
name: rc
version: "1.0"
sort: by_weight
...
你	ni	1000
好	hao	1000
你好	ni hao	800
`
	if err := os.WriteFile(filepath.Join(tmpDir, "8105.dict.yaml"), []byte(content), 0644); err != nil {
		t.Fatalf("写词典失败: %v", err)
	}
	d := dict.NewPinyinDict(nil)
	if err := d.LoadRimeDir(tmpDir); err != nil {
		t.Fatalf("加载词典失败: %v", err)
	}
	engine := NewEngine(wrapInCompositeDict(d), nil)
	stub := &stubLearning{}
	engine.SetLearningStrategy(stub)

	// 合法路径：code 能回查到 text，应走通
	engine.OnCandidateSelected("nihao", "你好")
	// 非法路径：xxxx 切不出对应 2 个字的音节序列 → 拒绝
	engine.OnCandidateSelected("xxxx", "你好")

	if len(stub.calls) != 1 {
		t.Fatalf("expected 1 learn call (only valid path), got %d: %+v", len(stub.calls), stub.calls)
	}
	if stub.calls[0].code != "nihao" {
		t.Errorf("unexpected learned code: %q", stub.calls[0].code)
	}
}
