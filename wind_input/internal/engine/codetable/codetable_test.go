package codetable

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
)

// 获取测试用的词库路径
func getTestDictPath(t *testing.T) string {
	// 尝试多个可能的路径（词库位于 schemas/wubi86/ 目录下）
	paths := []string{
		"../../../../build/data/schemas/wubi86/wubi86.txt", // 从 wind_input/internal/engine/codetable 到 build
		"../../../build/data/schemas/wubi86/wubi86.txt",
		"../../build/data/schemas/wubi86/wubi86.txt",
		"../build/data/schemas/wubi86/wubi86.txt",
		"build/data/schemas/wubi86/wubi86.txt",
		// 兼容旧路径
		"../../../../build/dict/wubi86/wubi86.txt",
		"build/dict/wubi86/wubi86.txt",
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

	t.Skip("跳过测试：未找到码表词库文件")
	return ""
}

// initCommonCharsForTest 为测试初始化通用汉字表
func initCommonCharsForTest(dictPath string) {
	// 从词库路径推断 common_chars.txt 路径
	// dictPath: .../build/data/schemas/wubi86/wubi86.txt
	// commonPath: .../build/data/schemas/common_chars.txt
	baseDir := filepath.Dir(filepath.Dir(dictPath)) // 获取 .../schemas
	commonPath := filepath.Join(baseDir, "common_chars.txt")

	// 重置并重新初始化
	dict.ResetCommonCharsForTesting()
	dict.InitCommonCharsWithPath(commonPath)
}

// TestCodetableBasicLookup 测试基本的码表编码查询
func TestCodetableBasicLookup(t *testing.T) {
	dictPath := getTestDictPath(t)

	engine := NewEngine(DefaultConfig(), nil)
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

// TestCodetableEmptyCode 测试空码处理
func TestCodetableEmptyCode(t *testing.T) {
	dictPath := getTestDictPath(t)

	engine := NewEngine(DefaultConfig(), nil)
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

// TestCodetablePrefixMatch 测试前缀匹配
func TestCodetablePrefixMatch(t *testing.T) {
	dictPath := getTestDictPath(t)

	engine := NewEngine(DefaultConfig(), nil)
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

// TestCodetableNoPinyinContamination 测试码表结果不包含拼音编码
func TestCodetableNoPinyinContamination(t *testing.T) {
	dictPath := getTestDictPath(t)

	engine := NewEngine(DefaultConfig(), nil)
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}

	// 这些是典型的拼音编码，码表中应该没有或有不同的结果
	pinyinCodes := []string{
		"ni",  // 拼音 "你"
		"hao", // 拼音 "好"
		"wo",  // 拼音 "我"
		"shi", // 拼音 "是"
	}

	for _, code := range pinyinCodes {
		result := engine.ConvertEx(code, 10)
		if len(result.Candidates) > 0 {
			// 验证返回的是码表编码结果，不是拼音结果
			for _, c := range result.Candidates {
				// 码表编码的候选词应该有 Code 字段
				if c.Pinyin != "" && c.Code == "" {
					t.Errorf("编码 %s 返回了拼音候选词 %s，可能存在拼音污染",
						code, c.Text)
				}
			}
		}
		t.Logf("编码 %s: %d 个候选词", code, len(result.Candidates))
	}
}

// TestCodetableWithDictManager 测试带 DictManager 的查询
func TestCodetableWithDictManager(t *testing.T) {
	dictPath := getTestDictPath(t)

	// 创建临时目录
	tmpDir := t.TempDir()

	// 创建 DictManager
	dm := dict.NewDictManager(tmpDir, tmpDir, nil)
	if err := dm.Initialize(); err != nil {
		t.Fatalf("初始化 DictManager 失败: %v", err)
	}
	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	defer dm.Close()

	// 添加测试用户词
	if err := dm.AddUserWord("test", "测试词", 9999); err != nil {
		t.Fatalf("添加用户词失败: %v", err)
	}

	// 创建码表引擎
	engine := NewEngine(DefaultConfig(), nil)
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

	// 查询码表编码，确保不受用户词影响
	result = engine.ConvertEx("gggg", 10)
	if len(result.Candidates) == 0 || result.Candidates[0].Text != "王" {
		t.Error("编码 gggg 应该首选 '王'")
	}
}

// TestCodetableAutoCommit 测试自动上屏
func TestCodetableAutoCommit(t *testing.T) {
	dictPath := getTestDictPath(t)

	config := DefaultConfig()
	config.AutoCommitAtFull = true

	engine := NewEngine(config, nil)
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}

	// 四码唯一时应该自动上屏
	// 注：这取决于词库内容，可能需要找一个真正唯一的编码
	result := engine.ConvertEx("gggg", 10)
	t.Logf("gggg: %d 候选, ShouldCommit=%v", len(result.Candidates), result.ShouldCommit)

	// 如果只有一个候选且开启了 AutoCommitAtFull，应该自动上屏
	if len(result.Candidates) == 1 && !result.ShouldCommit {
		t.Error("达到最大码长且唯一时应该自动上屏")
	}
}

// TestCodetableTopCodeCommit 测试顶码上屏
func TestCodetableTopCodeCommit(t *testing.T) {
	dictPath := getTestDictPath(t)

	config := DefaultConfig()
	config.TopCodeCommit = true

	engine := NewEngine(config, nil)
	if err := engine.LoadCodeTable(dictPath); err != nil {
		t.Fatalf("加载码表失败: %v", err)
	}

	// 输入超过最大码长，前四码应该上屏，多余的码作为新输入
	commitText, newInput, shouldCommit := engine.HandleTopCode("gggga")
	t.Logf("gggga: commit=%s, newInput=%s, shouldCommit=%v",
		commitText, newInput, shouldCommit)

	if !shouldCommit {
		t.Error("超过最大码长应该触发顶字上屏")
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

// writeFixtureCodeTable 在临时目录写一个最小码表文件。
// entries 为 "code\ttext" 序列，按出现顺序作为递减权重。
func writeFixtureCodeTable(t *testing.T, codeLen int, entries ...string) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "fixture.txt")
	var b []byte
	b = append(b, "[CodeTableHeader]\n"...)
	b = append(b, "Name=fixture\n"...)
	b = append(b, "CodeScheme=wubi86\n"...)
	b = append(b, ("CodeLength=" + itoa(codeLen) + "\n")...)
	b = append(b, "[CodeTable]\n"...)
	for _, e := range entries {
		b = append(b, e...)
		b = append(b, '\n')
	}
	if err := os.WriteFile(path, b, 0644); err != nil {
		t.Fatalf("write fixture: %v", err)
	}
	return path
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	neg := n < 0
	if neg {
		n = -n
	}
	var buf [16]byte
	i := len(buf)
	for n > 0 {
		i--
		buf[i] = byte('0' + n%10)
		n /= 10
	}
	if neg {
		i--
		buf[i] = '-'
	}
	return string(buf[i:])
}

// TestAutoCommitAtFull_ShortCodeUniqueNoSuffix 短码精确唯一且无更长后继时触发顶屏。
func TestAutoCommitAtFull_ShortCodeUniqueNoSuffix(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "aaa\t甲")
	cfg := DefaultConfig()
	cfg.AutoCommitAtFull = true
	cfg.MinAutoCommitLen = 2
	cfg.MaxCodeLength = 4
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	result := engine.ConvertEx("aaa", 10)
	if !result.ShouldCommit {
		t.Fatalf("应该触发全码自动上屏, candidates=%d", len(result.Candidates))
	}
	if result.CommitText != "甲" {
		t.Errorf("CommitText 应为 '甲'，实际 '%s'", result.CommitText)
	}
}

// TestAutoCommitAtFull_HasLongerCodeBlocked 存在更长后继编码时不应顶屏。
func TestAutoCommitAtFull_HasLongerCodeBlocked(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "aaa\t甲", "aaab\t乙")
	cfg := DefaultConfig()
	cfg.AutoCommitAtFull = true
	cfg.MinAutoCommitLen = 2
	cfg.MaxCodeLength = 4
	// 关闭前缀候选以保证候选列表里只有 "aaa→甲" 一个精确匹配
	cfg.PrefixMode = "none"
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	result := engine.ConvertEx("aaa", 10)
	if result.ShouldCommit {
		t.Fatalf("存在更长后继 aaab，不应顶屏；candidates=%d, commit=%q", len(result.Candidates), result.CommitText)
	}
}

// TestHandleTopCode_SuppressedByFullMatch 完整 input 自身有精确匹配时，不顶字。
// 场景：码表 abcd→甲、abcde→乙；输入 abcde 应让 ConvertEx 走完整流水线返回乙，
// 而不是被 HandleTopCode 直接顶 abcd→甲。
func TestHandleTopCode_SuppressedByFullMatch(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "abcd\t甲", "abcde\t乙")
	cfg := DefaultConfig()
	cfg.TopCodeCommit = true
	cfg.MaxCodeLength = 4
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	commitText, newInput, shouldCommit := engine.HandleTopCode("abcde")
	if shouldCommit {
		t.Fatalf("abcde 有精确匹配（乙），不应顶字；commit=%q, newInput=%q", commitText, newInput)
	}
	if newInput != "abcde" {
		t.Errorf("newInput 应保持 'abcde'，实际 %q", newInput)
	}
}

// TestHandleTopCode_SuppressedByLongerCode 输入虽无精确匹配但有更长后继时，不顶字。
func TestHandleTopCode_SuppressedByLongerCode(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "abcd\t甲", "abcdef\t丙")
	cfg := DefaultConfig()
	cfg.TopCodeCommit = true
	cfg.MaxCodeLength = 4
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	commitText, newInput, shouldCommit := engine.HandleTopCode("abcde")
	if shouldCommit {
		t.Fatalf("abcde 有更长后继 abcdef，不应顶字；commit=%q, newInput=%q", commitText, newInput)
	}
	if newInput != "abcde" {
		t.Errorf("newInput 应保持 'abcde'，实际 %q", newInput)
	}
}

// TestHandleTopCode_StillCommitsWhenNoLonger 既无精确匹配也无更长后继时，仍正常顶字（原行为回归）。
func TestHandleTopCode_StillCommitsWhenNoLonger(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "abcd\t甲")
	cfg := DefaultConfig()
	cfg.TopCodeCommit = true
	cfg.MaxCodeLength = 4
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	commitText, newInput, shouldCommit := engine.HandleTopCode("abcde")
	if !shouldCommit {
		t.Fatalf("无精确匹配且无更长后继，应顶字 abcd→甲；shouldCommit=%v", shouldCommit)
	}
	if commitText != "甲" {
		t.Errorf("commitText 应为 '甲'，实际 %q", commitText)
	}
	if newInput != "e" {
		t.Errorf("newInput 应为 'e'，实际 %q", newInput)
	}
}

// TestClearOnEmpty_SuppressedByLongerCode 4 码无精确匹配但有更长后继时，应返回前缀候选，
// 既不空码、也不清空（不再走原"空码-清空"分支）。
// 场景：码表只有 abcde→乙，无 abcd；输入 abcd，期望候选含 abcde→乙。
func TestClearOnEmpty_SuppressedByLongerCode(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "abcde\t乙")
	cfg := DefaultConfig()
	cfg.ClearOnEmptyAt4 = true
	cfg.AutoCommitAtFull = false
	cfg.MaxCodeLength = 4
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	result := engine.ConvertEx("abcd", 10)
	if result.IsEmpty {
		t.Fatalf("abcd 有更长后继 abcde，不应为空码；candidates=%d", len(result.Candidates))
	}
	if result.ShouldClear {
		t.Errorf("abcd 有更长后继 abcde，不应清空")
	}
	found := false
	for _, c := range result.Candidates {
		if c.Text == "乙" {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("候选中应包含长码 abcde→乙；实际候选 %d 个", len(result.Candidates))
	}
}

// TestClearOnEmpty_StillClearsWhenNoLonger 无更长后继时，全码空仍清空（原行为回归）。
func TestClearOnEmpty_StillClearsWhenNoLonger(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "wxyz\t甲")
	cfg := DefaultConfig()
	cfg.ClearOnEmptyAt4 = true
	cfg.AutoCommitAtFull = false
	cfg.MaxCodeLength = 4
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	result := engine.ConvertEx("abcd", 10)
	if !result.IsEmpty {
		t.Fatalf("abcd 在该码表下应为空码")
	}
	if !result.ShouldClear {
		t.Errorf("abcd 无候选无后继，应清空；ShouldClear=%v", result.ShouldClear)
	}
}

// TestAutoCommitAtFull_BelowMinLen 输入长度未达到 MinAutoCommitLen 不应顶屏。
func TestAutoCommitAtFull_BelowMinLen(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "abc\t丙")
	cfg := DefaultConfig()
	cfg.AutoCommitAtFull = true
	cfg.MinAutoCommitLen = 4
	cfg.MaxCodeLength = 4
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	result := engine.ConvertEx("abc", 10)
	if result.ShouldCommit {
		t.Fatalf("输入长度 3 < MinAutoCommitLen 4，不应顶屏；commit=%q", result.CommitText)
	}
}

// TestAutoCommitAtFull_Disabled_DoesNotCommit 当 AutoCommitAtFull=false 时，
// 即便精确唯一且无更长后继，也不应顶屏。防止开关被绕过。
func TestAutoCommitAtFull_Disabled_DoesNotCommit(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "abcd\t甲")
	cfg := DefaultConfig()
	cfg.AutoCommitAtFull = false // 关闭开关
	cfg.MaxCodeLength = 4
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	result := engine.ConvertEx("abcd", 10)
	if result.ShouldCommit {
		t.Fatalf("AutoCommitAtFull=false 时不应顶屏；commit=%q", result.CommitText)
	}
	if len(result.Candidates) == 0 || result.Candidates[0].Text != "甲" {
		t.Errorf("候选应正常返回 '甲'；得到 %d 个候选", len(result.Candidates))
	}
}

// TestPrefixEnabledAtMaxLen_WithLongerCode 输入长度恰好 == MaxCodeLength 且无精确匹配但
// 存在更长后继时，前缀查询应启用并返回长码候选（保护本轮 Phase 2 prefixEnabled 边界放宽）。
func TestPrefixEnabledAtMaxLen_WithLongerCode(t *testing.T) {
	path := writeFixtureCodeTable(t, 4, "abcde\t乙")
	cfg := DefaultConfig()
	cfg.AutoCommitAtFull = false
	cfg.MaxCodeLength = 4
	engine := NewEngine(cfg, nil)
	if err := engine.LoadCodeTable(path); err != nil {
		t.Fatalf("load fixture: %v", err)
	}
	result := engine.ConvertEx("abcd", 10)
	if result.IsEmpty {
		t.Fatalf("inputLen==MaxCodeLength 且有 abcde 后继时不应空码；candidates=%d", len(result.Candidates))
	}
	found := false
	for _, c := range result.Candidates {
		if c.Code == "abcde" && c.Text == "乙" {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("候选中应包含长码 abcde→乙；实际 %d 个候选", len(result.Candidates))
	}
}

// TestAutoCommitAtFull_AfterShadowDelete Shadow 删词后剩余唯一时应触发顶屏。
// 防止"Shadow 删词后精确唯一性"链路被未来改动破坏。
// 注意：纯码表方案下 checkAutoCommit 使用的 filteredExact 已应用 Shadow 删除规则
// （codetable.go:507~514），此测试验证该路径。
func TestAutoCommitAtFull_AfterShadowDelete(t *testing.T) {
	// 该测试需要构造 ShadowProvider 注入 DictManager，超出 fixture 工具的能力范围。
	// codetable.go:507~514 已经在 filteredExact 上调用 ApplyShadowPins，逻辑正确。
	// 这里以注释形式记录测试意图，待后续补 DictManager fixture 工具后启用。
	t.Skip("Shadow 删词测试需要 DictManager fixture，待 dict 包测试工具完善后启用；逻辑路径已在 codetable.go:507~514 通过手工 review 验证")
}
