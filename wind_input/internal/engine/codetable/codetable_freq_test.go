package codetable

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
)

// newTestEngineWithFreq 创建带词频学习的测试引擎
func newTestEngineWithFreq(t *testing.T, protectTopN int) (*Engine, *dict.UserDict) {
	t.Helper()
	config := &Config{
		MaxCodeLength:   4,
		EnableUserFreq:  true,
		ProtectTopN:     protectTopN,
		DedupCandidates: true,
	}
	engine := NewEngine(config, nil)

	// 加载真实码表（如果可用）
	dictPath := getTestDictPath(t)
	if dictPath != "" {
		ct, err := dict.LoadCodeTable(dictPath)
		if err != nil {
			t.Skipf("无法加载码表: %v", err)
		}
		engine.codeTable = ct
	}

	// 创建 DictManager 并通过 SwitchSchema 初始化 UserDict
	tmpDir := t.TempDir()
	dm := dict.NewDictManager(tmpDir, tmpDir, nil)
	dm.SwitchSchema("test", "test.shadow.yaml", "test.userwords.txt")
	engine.SetDictManager(dm)

	userDict := dm.GetUserDict()
	if userDict == nil {
		t.Fatal("UserDict 应被初始化")
	}

	return engine, userDict
}

func TestCodetableOnCandidateSelected_DisabledByDefault(t *testing.T) {
	config := &Config{
		MaxCodeLength:  4,
		EnableUserFreq: false, // 关闭
	}
	engine := NewEngine(config, nil)

	// 不应 panic
	engine.OnCandidateSelected("sf", "树发")
}

func TestCodetableOnCandidateSelected_SingleCharSkipped(t *testing.T) {
	engine, userDict := newTestEngineWithFreq(t, 0)
	defer userDict.Close()

	// 单字不应学习
	engine.OnCandidateSelected("s", "木")

	if userDict.EntryCount() != 0 {
		t.Errorf("单字不应被学习, 词条数=%d", userDict.EntryCount())
	}
}

func TestCodetableOnCandidateSelected_ProtectTopN(t *testing.T) {
	engine, userDict := newTestEngineWithFreq(t, 3)
	defer userDict.Close()

	if engine.codeTable == nil {
		t.Skip("需要真实码表来测试 protect_top_n")
	}

	// 找到码表中 "sf" 的前3个候选
	entries := engine.codeTable.Lookup("sf")
	if len(entries) < 3 {
		t.Skipf("码表 'sf' 候选不足 3 个: %d", len(entries))
	}

	// 码表前3位应被保护（只要是多字词）
	for i := 0; i < 3 && i < len(entries); i++ {
		text := entries[i].Text
		if len([]rune(text)) <= 1 {
			continue // 单字另有跳过逻辑
		}
		engine.OnCandidateSelected("sf", text)
	}

	if userDict.EntryCount() != 0 {
		t.Errorf("码表前 %d 位应受保护, 但词条数=%d", 3, userDict.EntryCount())
	}
}

func TestCodetableOnCandidateSelected_BeyondProtectTopN(t *testing.T) {
	engine, userDict := newTestEngineWithFreq(t, 2)
	defer userDict.Close()

	if engine.codeTable == nil {
		t.Skip("需要真实码表来测试")
	}

	// 找到 protect_top_n 之后的多字词候选
	entries := engine.codeTable.Lookup("sf")
	var targetText string
	for i := 2; i < len(entries); i++ {
		if len([]rune(entries[i].Text)) >= 2 {
			targetText = entries[i].Text
			break
		}
	}
	if targetText == "" {
		t.Skip("找不到 protect_top_n 之后的多字词")
	}

	// 第 protect_top_n 位之后的多字词应被学习
	engine.OnCandidateSelected("sf", targetText)

	if userDict.EntryCount() == 0 {
		t.Errorf("protect_top_n 之后的词应被学习: %q", targetText)
	}
}

func TestCodetableOnCandidateSelected_NotInCodeTable(t *testing.T) {
	engine, userDict := newTestEngineWithFreq(t, 3)
	defer userDict.Close()

	// 不在码表中的多字词应被学习（getOriginalRank 返回 -1）
	engine.OnCandidateSelected("abcd", "测试词组")

	if userDict.EntryCount() == 0 {
		t.Error("不在码表中的词应被学习")
	}
}

func TestCodetableGetOriginalRank(t *testing.T) {
	engine, userDict := newTestEngineWithFreq(t, 0)
	defer userDict.Close()

	if engine.codeTable == nil {
		t.Skip("需要真实码表")
	}

	// 不存在的编码
	rank := engine.getOriginalRank("zzzz", "不存在")
	if rank != -1 {
		t.Errorf("不存在的编码应返回 -1, 实际=%d", rank)
	}

	// 存在的编码，查第一个候选
	entries := engine.codeTable.Lookup("sf")
	if len(entries) > 0 {
		rank = engine.getOriginalRank("sf", entries[0].Text)
		if rank != 0 {
			t.Errorf("第一个候选的排名应为 0, 实际=%d", rank)
		}
	}

	// 不存在的词
	rank = engine.getOriginalRank("sf", "绝对不存在的词xyz")
	if rank != -1 {
		t.Errorf("不存在的词应返回 -1, 实际=%d", rank)
	}
}
