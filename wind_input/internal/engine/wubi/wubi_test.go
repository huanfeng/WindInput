package wubi

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

// 获取测试用的词库路径
func getTestDictPath(t *testing.T) string {
	// 尝试多个可能的路径
	paths := []string{
		"../../../../build/dict/wubi/wubi86.txt", // 从 wind_input/internal/engine/wubi 到 build
		"../../../build/dict/wubi/wubi86.txt",
		"../../build/dict/wubi/wubi86.txt",
		"../build/dict/wubi/wubi86.txt",
		"build/dict/wubi/wubi86.txt",
		"dict/wubi/wubi86.txt",
	}

	for _, p := range paths {
		absPath, _ := filepath.Abs(p)
		if _, err := os.Stat(absPath); err == nil {
			t.Logf("使用词库: %s", absPath)
			// 初始化通用汉字表（使用相对于词库的路径）
			initCommonCharsForTest(absPath)
			return absPath
		}
	}

	t.Skip("跳过测试：未找到五笔词库文件")
	return ""
}

// initCommonCharsForTest 为测试初始化通用汉字表
func initCommonCharsForTest(dictPath string) {
	// 从词库路径推断 common_chars.txt 路径
	// dictPath: .../build/dict/wubi/wubi86.txt
	// commonPath: .../build/dict/common_chars.txt
	baseDir := filepath.Dir(filepath.Dir(dictPath)) // 获取 .../build/dict
	commonPath := filepath.Join(baseDir, "common_chars.txt")

	// 重置并重新初始化
	dict.ResetCommonCharsForTesting()
	dict.InitCommonCharsWithPath(commonPath)
}

// TestWubiBasicLookup 测试基本的五笔编码查询
func TestWubiBasicLookup(t *testing.T) {
	dictPath := getTestDictPath(t)

	engine := NewEngine(DefaultConfig())
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}

	tests := []struct {
		code       string
		contains   string // 期望包含的字（不要求首选）
		minResults int    // 最少返回数量
		desc       string
	}{
		{"a", "工", 1, "一级简码"},
		{"g", "一", 1, "一级简码"}, // 实际首选可能是 "一"
		{"aa", "式", 1, "二级简码"},
		{"gg", "五", 1, "二级简码"},
		{"gggg", "王", 1, "四码全码"},
		{"aaaa", "工", 1, "四码全码"},
	}

	for _, tt := range tests {
		t.Run(tt.desc+"_"+tt.code, func(t *testing.T) {
			result := engine.ConvertEx(tt.code, 50)
			if len(result.Candidates) < tt.minResults {
				t.Errorf("编码 %s 应该返回至少 %d 个候选词，实际 %d 个",
					tt.code, tt.minResults, len(result.Candidates))
				return
			}
			// 检查是否包含期望的字
			found := false
			for _, c := range result.Candidates {
				if c.Text == tt.contains {
					found = true
					break
				}
			}
			if !found {
				t.Errorf("编码 %s 应该包含 %s，实际候选: %v",
					tt.code, tt.contains, getCandidateTexts(result.Candidates[:min(5, len(result.Candidates))]))
			}
		})
	}
}

func getCandidateTexts(candidates []candidate.Candidate) []string {
	texts := make([]string, len(candidates))
	for i, c := range candidates {
		texts[i] = c.Text
	}
	return texts
}

// TestWubiEmptyCode 测试空码处理
func TestWubiEmptyCode(t *testing.T) {
	dictPath := getTestDictPath(t)

	engine := NewEngine(DefaultConfig())
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}

	tests := []struct {
		code    string
		isEmpty bool
		desc    string
	}{
		{"zzzz", true, "无效四码zzzz"},
		{"qzzz", true, "无效四码qzzz"},
		{"a", false, "有效一码"},
	}

	for _, tt := range tests {
		t.Run(tt.desc, func(t *testing.T) {
			result := engine.ConvertEx(tt.code, 10)
			if result.IsEmpty != tt.isEmpty {
				t.Errorf("编码 %s 的 IsEmpty 应为 %v，实际为 %v",
					tt.code, tt.isEmpty, result.IsEmpty)
			}
		})
	}
}

// TestWubiPrefixMatch 测试前缀匹配
func TestWubiPrefixMatch(t *testing.T) {
	dictPath := getTestDictPath(t)

	engine := NewEngine(DefaultConfig())
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}

	// 输入 "gg" 应该匹配 "gg" 开头的所有编码
	result := engine.ConvertEx("gg", 50)
	if len(result.Candidates) == 0 {
		t.Error("前缀 gg 应该返回候选词")
		return
	}

	// 验证返回的候选词
	t.Logf("前缀 gg 返回 %d 个候选词", len(result.Candidates))
	for i, c := range result.Candidates[:min(5, len(result.Candidates))] {
		t.Logf("  %d: %s (code=%s, weight=%d)", i+1, c.Text, c.Code, c.Weight)
	}
}

// TestWubiNoPinyinContamination 测试五笔结果不包含拼音编码
func TestWubiNoPinyinContamination(t *testing.T) {
	dictPath := getTestDictPath(t)

	engine := NewEngine(DefaultConfig())
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}

	// 这些是典型的拼音编码，五笔中应该没有或有不同的结果
	pinyinCodes := []string{
		"ni",  // 拼音 "你"
		"hao", // 拼音 "好"
		"wo",  // 拼音 "我"
		"shi", // 拼音 "是"
	}

	for _, code := range pinyinCodes {
		result := engine.ConvertEx(code, 10)
		if len(result.Candidates) > 0 {
			// 验证返回的是五笔编码结果，不是拼音结果
			for _, c := range result.Candidates {
				// 五笔编码的候选词应该有 Code 字段
				if c.Pinyin != "" && c.Code == "" {
					t.Errorf("编码 %s 返回了拼音候选词 %s，可能存在拼音污染",
						code, c.Text)
				}
			}
		}
		t.Logf("编码 %s: %d 个候选词", code, len(result.Candidates))
	}
}

// TestWubiWithDictManager 测试带 DictManager 的查询
func TestWubiWithDictManager(t *testing.T) {
	dictPath := getTestDictPath(t)

	// 创建临时目录
	tmpDir := t.TempDir()

	// 创建 DictManager
	dm := dict.NewDictManager(tmpDir)
	if err := dm.Initialize(); err != nil {
		t.Fatalf("初始化 DictManager 失败: %v", err)
	}
	defer dm.Close()

	// 添加测试用户词
	if err := dm.AddUserWord("test", "测试词", 9999); err != nil {
		t.Fatalf("添加用户词失败: %v", err)
	}

	// 创建五笔引擎
	engine := NewEngine(DefaultConfig())
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}
	engine.SetDictManager(dm)

	// 查询用户词
	result := engine.ConvertEx("test", 10)
	if len(result.Candidates) == 0 {
		t.Error("应该能查到用户词 'test'")
		return
	}

	found := false
	for _, c := range result.Candidates {
		if c.Text == "测试词" {
			found = true
			break
		}
	}
	if !found {
		t.Error("用户词 '测试词' 应该在候选列表中")
	}

	// 查询五笔编码，确保不受用户词影响
	result = engine.ConvertEx("gggg", 10)
	if len(result.Candidates) == 0 || result.Candidates[0].Text != "王" {
		t.Error("五笔编码 gggg 应该首选 '王'")
	}
}

// TestWubiAutoCommit 测试自动上屏
func TestWubiAutoCommit(t *testing.T) {
	dictPath := getTestDictPath(t)

	config := DefaultConfig()
	config.AutoCommitAt4 = true

	engine := NewEngine(config)
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}

	// 四码唯一时应该自动上屏
	// 注：这取决于词库内容，可能需要找一个真正唯一的编码
	result := engine.ConvertEx("gggg", 10)
	t.Logf("gggg: %d 候选, ShouldCommit=%v", len(result.Candidates), result.ShouldCommit)

	// 如果只有一个候选且开启了 AutoCommitAt4，应该自动上屏
	if len(result.Candidates) == 1 && !result.ShouldCommit {
		t.Error("四码唯一时应该自动上屏")
	}
}

// TestWubiTopCodeCommit 测试五码顶字
func TestWubiTopCodeCommit(t *testing.T) {
	dictPath := getTestDictPath(t)

	config := DefaultConfig()
	config.TopCodeCommit = true

	engine := NewEngine(config)
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}

	// 输入五码，前四码应该上屏，第五码作为新输入
	commitText, newInput, shouldCommit := engine.HandleTopCode("gggga")
	t.Logf("gggga: commit=%s, newInput=%s, shouldCommit=%v",
		commitText, newInput, shouldCommit)

	if !shouldCommit {
		t.Error("五码应该触发顶字上屏")
	}
	if newInput != "a" {
		t.Errorf("新输入应该是 'a'，实际是 '%s'", newInput)
	}
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}
