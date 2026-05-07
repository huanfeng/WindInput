package pinyin

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
)

// getCachedWdatPath 返回运行时缓存的 pinyin.wdat 路径（生产路径）
func getCachedWdatPath(t *testing.T) string {
	t.Helper()
	candidates := []string{
		filepath.Join(os.Getenv("LOCALAPPDATA"), "WindInputDebug", "cache", "pinyin.wdat"),
		filepath.Join(os.Getenv("LOCALAPPDATA"), "WindInput", "cache", "pinyin.wdat"),
	}
	for _, p := range candidates {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	t.Skipf("生产 wdat 缓存不存在，跳过")
	return ""
}

// getRealDictDirForTest 返回真实词库目录路径
func getRealDictDirForTest(t *testing.T) string {
	t.Helper()
	_, filename, _, _ := runtime.Caller(0)
	pinyinDir := filepath.Dir(filename)
	projectRoot := filepath.Join(pinyinDir, "..", "..", "..", "..")

	// 尝试多个可能的路径
	candidates := []string{
		filepath.Join(projectRoot, "build", "data", "schemas", "pinyin", "cn_dicts"),
		filepath.Join(projectRoot, "build_debug", "data", "schemas", "pinyin", "cn_dicts"),
		filepath.Join(projectRoot, "build", "dict", "pinyin"),
	}
	for _, dir := range candidates {
		if _, err := os.Stat(filepath.Join(dir, "8105.dict.yaml")); err == nil {
			return dir
		}
	}
	t.Skipf("Real dictionary not found, skipping")
	return ""
}

// getRealUnigramPath 返回真实 unigram 文件路径
func getRealUnigramPath(t *testing.T) string {
	t.Helper()
	_, filename, _, _ := runtime.Caller(0)
	pinyinDir := filepath.Dir(filename)
	projectRoot := filepath.Join(pinyinDir, "..", "..", "..", "..")

	candidates := []string{
		filepath.Join(projectRoot, "build", "data", "schemas", "pinyin", "unigram.txt"),
		filepath.Join(projectRoot, "build_debug", "data", "schemas", "pinyin", "unigram.txt"),
		filepath.Join(projectRoot, "build", "dict", "pinyin", "unigram.txt"),
	}
	for _, p := range candidates {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	t.Skipf("Unigram not found, skipping")
	return ""
}

// loadRealEngineForTest 加载完整生产引擎
func loadRealEngineForTest(t *testing.T) *Engine {
	t.Helper()
	dictDir := getRealDictDirForTest(t)

	d := dict.NewPinyinDict(nil)
	if err := d.LoadRimeDir(dictDir); err != nil {
		t.Fatalf("加载词库失败: %v", err)
	}

	cd := wrapInCompositeDict(d)
	engine := NewEngineWithConfig(cd, &Config{
		UseSmartCompose: true,
		CandidateOrder:  "smart",
	}, nil)

	unigramPath := getRealUnigramPath(t)
	if err := engine.LoadUnigram(unigramPath); err != nil {
		t.Fatalf("加载 unigram 失败: %v", err)
	}
	t.Logf("Loaded dict from %s, unigram from %s", dictDir, unigramPath)
	return engine
}

func TestViterbiFreq_TianYaShi(t *testing.T) {
	engine := loadRealEngineForTest(t)
	st := engine.syllableTrie

	// 仅测试 Viterbi 在 "tianyashi" 上的分词选择
	input := "tianyashi"
	lattice := BuildLattice(input, st, engine.dict, engine.unigram)

	if lattice.IsEmpty() {
		t.Fatal("BuildLattice 返回空")
	}

	// 打印所有 lattice 节点
	for pos := 0; pos <= len(input); pos++ {
		nodes := lattice.GetNodesEndingAt(pos)
		for _, node := range nodes {
			if len([]rune(node.Word)) > 1 {
				t.Logf("  [LATTICE] pos=%d word=%s logProb=%.4f start=%d end=%d",
					pos, node.Word, node.LogProb, node.Start, node.End)
			}
		}
	}

	result := ViterbiDecode(lattice, nil)
	if result == nil {
		t.Fatal("ViterbiDecode 返回 nil")
	}
	t.Logf("Viterbi(%q) = %v (logProb=%.4f)", input, result.Words, result.LogProb)

	// 期望"天涯"+"是"而非"填鸭式"
	joined := strings.Join(result.Words, "")
	t.Logf("Joined: %s", joined)
	if strings.Contains(joined, "填鸭式") {
		t.Errorf("Viterbi 选择了'填鸭式'，期望'天涯+是'")
	}
}

func TestViterbiFreq_CangMangFull(t *testing.T) {
	engine := loadRealEngineForTest(t)

	tests := []struct {
		input    string
		expected string // 期望 Viterbi 结果中包含的文本
		bad      string // 期望不应包含的文本
	}{
		{
			input:    "cangmangdetianyashiwode",
			expected: "天涯",
			bad:      "填鸭式",
		},
		{
			input:    "cangmangdetianyashiwodeai",
			expected: "天涯",
			bad:      "填鸭式",
		},
		{
			input:    "chongmanxiwangdebashebilequgengnengdaodamudidi",
			expected: "目的地",
			bad:      "弟弟",
		},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			// 额外诊断：对关键音节构建 lattice 查看节点
			if strings.Contains(tt.input, "tianyashi") && !strings.Contains(tt.input, "mudidi") {
				subInput := "tianyashi"
				lattice := BuildLattice(subInput, engine.syllableTrie, engine.dict, engine.unigram)
				t.Logf("=== Lattice for %q ===", subInput)
				for pos := 0; pos <= len(subInput); pos++ {
					nodes := lattice.GetNodesEndingAt(pos)
					for _, node := range nodes {
						if node.Word == "天涯" || node.Word == "填鸭式" || node.Word == "是" {
							t.Logf("  pos=%d word=%q logProb=%.4f start=%d end=%d",
								pos, node.Word, node.LogProb, node.Start, node.End)
						}
					}
				}
				// 单独打印 pos=9（shi结尾）的所有节点
				t.Logf("--- All nodes at pos=9 (shi) ---")
				for _, node := range lattice.GetNodesEndingAt(9) {
					t.Logf("  word=%q logProb=%.4f start=%d", node.Word, node.LogProb, node.Start)
				}
				// 直接查词库确认 shi 的候选
				shiResults := engine.dict.Lookup("shi")
				t.Logf("--- dict.Lookup(\"shi\") count=%d, first few: ---", len(shiResults))
				for i, c := range shiResults {
					if i >= 5 {
						break
					}
					t.Logf("  %q weight=%d", c.Text, c.Weight)
				}
				vResult := ViterbiDecode(lattice, nil)
				if vResult != nil {
					t.Logf("  Viterbi(%q) = %v (logProb=%.4f)", subInput, vResult.Words, vResult.LogProb)
				}
			}
			if strings.Contains(tt.input, "mudidi") {
				subInput := "mudidi"
				// 先查看词库返回的原始 Weight
				results := engine.dict.Lookup("didi")
				for _, c := range results {
					if c.Text == "弟弟" {
						t.Logf("  [DICT] 弟弟: weight=%d, unigram.Contains=%v", c.Weight, engine.unigram.Contains("弟弟"))
					}
				}
				results2 := engine.dict.Lookup("mudidi")
				for _, c := range results2 {
					if c.Text == "目的地" {
						t.Logf("  [DICT] 目的地: weight=%d, unigram.Contains=%v", c.Weight, engine.unigram.Contains("目的地"))
					}
				}

				lattice := BuildLattice(subInput, engine.syllableTrie, engine.dict, engine.unigram)
				t.Logf("=== Lattice for %q ===", subInput)
				for pos := 0; pos <= len(subInput); pos++ {
					nodes := lattice.GetNodesEndingAt(pos)
					for _, node := range nodes {
						if len([]rune(node.Word)) >= 2 {
							t.Logf("  pos=%d word=%s logProb=%.4f start=%d end=%d syllables=%v",
								pos, node.Word, node.LogProb, node.Start, node.End, node.Syllables)
						}
					}
				}
				vResult := ViterbiDecode(lattice, nil)
				if vResult != nil {
					t.Logf("  Viterbi(%q) = %v (logProb=%.4f)", subInput, vResult.Words, vResult.LogProb)
				}
			}

			result := engine.ConvertEx(tt.input, 30)

			t.Logf("=== input=%q ===", tt.input)
			for j, c := range result.Candidates {
				if j >= 10 {
					break
				}
				t.Logf("  [%d] %s (weight=%d, consumed=%d)", j, c.Text, c.Weight, c.ConsumedLength)
			}

			// 检查前5个候选
			found := false
			badFound := false
			for j, c := range result.Candidates {
				if j >= 5 {
					break
				}
				if strings.Contains(c.Text, tt.expected) {
					found = true
				}
				if strings.Contains(c.Text, tt.bad) {
					badFound = true
					t.Logf("  WARNING: '%s' in top-5 candidates", c.Text)
				}
			}
			if !found {
				t.Errorf("前5候选中未找到包含'%s'的结果", tt.expected)
			}
			if badFound {
				// 记录但不失败——这是我们要修复的问题
				t.Logf("已知问题：'%s'出现在前5候选中", tt.bad)
			}

			// 首候选检查
			if len(result.Candidates) > 0 {
				first := result.Candidates[0].Text
				t.Logf("首候选: %s", first)
				if strings.Contains(first, tt.bad) {
					t.Errorf("首候选'%s'包含'%s'", first, tt.bad)
				}
			}
		})
	}
}

// TestViterbiFreq_WdatLongSentence 使用生产 wdat 文件验证长句 Viterbi 分词
// 用于排查单测（YAML dict）与生产（wdat）行为不一致的问题
func TestViterbiFreq_WdatLongSentence(t *testing.T) {
	wdatPath := getCachedWdatPath(t)
	unigramPath := getRealUnigramPath(t)

	d := dict.NewPinyinDict(nil)
	if err := d.LoadDAT(wdatPath); err != nil {
		t.Fatalf("加载 wdat 失败: %v", err)
	}
	t.Logf("wdat: %s, entries=%d", wdatPath, d.EntryCount())

	// 验证关键词在 wdat 中是否存在
	for _, kv := range []struct{ code, word string }{
		{"bashe", "跋涉"},
		{"daoda", "到达"},
		{"mudidi", "目的地"},
		{"tianyashi", "天涯"},
	} {
		results := d.Lookup(kv.code)
		found := false
		for _, c := range results {
			if c.Text == kv.word {
				found = true
				t.Logf("  wdat Lookup(%q) → %q weight=%d", kv.code, c.Text, c.Weight)
				break
			}
		}
		if !found {
			t.Logf("  WARNING: wdat Lookup(%q) 未找到 %q（共 %d 个结果）", kv.code, kv.word, len(results))
		}
	}

	cd := wrapInCompositeDict(d)
	engine := NewEngineWithConfig(cd, &Config{
		UseSmartCompose: true,
		CandidateOrder:  "smart",
	}, nil)
	if err := engine.LoadUnigram(unigramPath); err != nil {
		t.Fatalf("加载 unigram 失败: %v", err)
	}

	tests := []struct {
		input    string
		expected string
		bad      string
	}{
		{"cangmangdetianyashiwode", "天涯", "填鸭式"},
		{"chongmanxiwangdebashebilequgengnengdaodamudidi", "目的地", "弟弟"},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			result := engine.ConvertEx(tt.input, 10)
			if len(result.Candidates) > 0 {
				t.Logf("首候选: %s", result.Candidates[0].Text)
			}
			for i, c := range result.Candidates {
				if i >= 5 {
					break
				}
				t.Logf("  [%d] %s (consumed=%d)", i, c.Text, c.ConsumedLength)
			}
			if len(result.Candidates) > 0 {
				first := result.Candidates[0].Text
				if strings.Contains(first, tt.bad) {
					t.Errorf("wdat首候选 %q 包含 %q，期望包含 %q", first, tt.bad, tt.expected)
				}
			}
		})
	}
}
