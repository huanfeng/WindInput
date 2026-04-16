package dict

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/store"
)

const testSchema = "test_schema"

func openTestStore(t *testing.T) *store.Store {
	t.Helper()
	s, err := store.Open(filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatalf("store.Open: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s
}

// TestStoreUserLayer_TypeAndName 验证 Type() 和 Name() 返回值。
func TestStoreUserLayer_TypeAndName(t *testing.T) {
	s := openTestStore(t)
	layer := NewStoreUserLayer(s, testSchema)

	if got := layer.Type(); got != LayerTypeUser {
		t.Errorf("Type() = %v, want LayerTypeUser", got)
	}
	if got := layer.Name(); got == "" {
		t.Error("Name() should not be empty")
	}
}

// TestStoreUserLayer_SearchAndAdd 添加词条后验证精确查询和前缀查询及排序。
func TestStoreUserLayer_SearchAndAdd(t *testing.T) {
	s := openTestStore(t)
	layer := NewStoreUserLayer(s, testSchema)

	// 添加若干词条
	if err := layer.Add("abc", "词A", 100); err != nil {
		t.Fatalf("Add: %v", err)
	}
	if err := layer.Add("abc", "词B", 200); err != nil {
		t.Fatalf("Add: %v", err)
	}
	if err := layer.Add("abcd", "词C", 50); err != nil {
		t.Fatalf("Add: %v", err)
	}

	// 精确查询 "abc"
	results := layer.Search("abc", 0)
	if len(results) != 2 {
		t.Fatalf("Search('abc') = %d results, want 2", len(results))
	}
	// 权重高的应排在前面
	if results[0].Text != "词B" {
		t.Errorf("first result = %q, want 词B (higher weight)", results[0].Text)
	}
	if results[1].Text != "词A" {
		t.Errorf("second result = %q, want 词A", results[1].Text)
	}
	// IsCommon 应为 true
	for _, c := range results {
		if !c.IsCommon {
			t.Errorf("candidate %q: IsCommon should be true", c.Text)
		}
	}

	// 前缀查询 "ab" 应返回 3 条
	prefix := layer.SearchPrefix("ab", 0)
	if len(prefix) != 3 {
		t.Errorf("SearchPrefix('ab') = %d results, want 3", len(prefix))
	}

	// limit 验证
	limited := layer.Search("abc", 1)
	if len(limited) != 1 {
		t.Errorf("Search with limit=1 returned %d results, want 1", len(limited))
	}
}

// TestStoreUserLayer_Remove 添加词条后删除，验证已不存在。
func TestStoreUserLayer_Remove(t *testing.T) {
	s := openTestStore(t)
	layer := NewStoreUserLayer(s, testSchema)

	if err := layer.Add("xyz", "词X", 100); err != nil {
		t.Fatalf("Add: %v", err)
	}
	if results := layer.Search("xyz", 0); len(results) != 1 {
		t.Fatalf("before Remove: expected 1 result, got %d", len(results))
	}

	if err := layer.Remove("xyz", "词X"); err != nil {
		t.Fatalf("Remove: %v", err)
	}
	if results := layer.Search("xyz", 0); len(results) != 0 {
		t.Errorf("after Remove: expected 0 results, got %d", len(results))
	}
}

// TestStoreTempLayer_LearnAndPromote 学习词条并验证晋升条件。
func TestStoreTempLayer_LearnAndPromote(t *testing.T) {
	s := openTestStore(t)
	temp := NewStoreTempLayer(s, testSchema)
	temp.SetLimits(100, 3) // 选 3 次即可晋升

	// 学习同一词条 2 次，尚未达到晋升条件
	shouldPromote := temp.LearnWord("ab", "词", 10)
	if shouldPromote {
		t.Error("LearnWord 1st: should not promote yet")
	}
	shouldPromote = temp.LearnWord("ab", "词", 10)
	if shouldPromote {
		t.Error("LearnWord 2nd: should not promote yet")
	}

	// 第 3 次应返回 true（达到晋升条件）
	shouldPromote = temp.LearnWord("ab", "词", 10)
	if !shouldPromote {
		t.Error("LearnWord 3rd: should indicate promote condition met")
	}

	// 词条应可查询到
	results := temp.Search("ab", 0)
	if len(results) == 0 {
		t.Error("Search after LearnWord: expected at least 1 result")
	}

	// 晋升词条
	promoted := temp.PromoteWord("ab", "词")
	if !promoted {
		t.Error("PromoteWord: expected true")
	}

	// 晋升后临时词库中应不再有该词
	afterPromote := temp.Search("ab", 0)
	for _, c := range afterPromote {
		if c.Text == "词" {
			t.Error("after PromoteWord: word still present in temp layer")
		}
	}
}

// TestStoreShadowLayer_PinAndGet 验证固定词规则的写入和读取转换。
func TestStoreShadowLayer_PinAndGet(t *testing.T) {
	s := openTestStore(t)
	shadow := NewStoreShadowLayer(s, testSchema)

	// 无规则时应返回 nil
	if rules := shadow.GetShadowRules("abc"); rules != nil {
		t.Errorf("GetShadowRules on empty: expected nil, got %+v", rules)
	}

	// 固定一个词
	shadow.Pin("abc", "词A", 0)

	rules := shadow.GetShadowRules("abc")
	if rules == nil {
		t.Fatal("GetShadowRules after Pin: expected non-nil")
	}
	if len(rules.Pinned) != 1 {
		t.Fatalf("Pinned count = %d, want 1", len(rules.Pinned))
	}
	if rules.Pinned[0].Word != "词A" {
		t.Errorf("Pinned[0].Word = %q, want 词A", rules.Pinned[0].Word)
	}
	if rules.Pinned[0].Position != 0 {
		t.Errorf("Pinned[0].Position = %d, want 0", rules.Pinned[0].Position)
	}

	// 删除一个词并验证转换
	shadow.Delete("abc", "词B")
	rules = shadow.GetShadowRules("abc")
	if rules == nil {
		t.Fatal("GetShadowRules after Delete: expected non-nil")
	}
	found := false
	for _, d := range rules.Deleted {
		if d == "词B" {
			found = true
		}
	}
	if !found {
		t.Error("Deleted should contain 词B")
	}

	// GetRuleCount 应 >= 1
	if count := shadow.GetRuleCount(); count < 1 {
		t.Errorf("GetRuleCount = %d, want >= 1", count)
	}

	// IsDirty 始终 false
	if shadow.IsDirty() {
		t.Error("IsDirty should always return false")
	}
}

// TestStoreFreqScorer 验证词频加成：未知词返回 0，增加词频后返回 > 0。
func TestStoreFreqScorer(t *testing.T) {
	s := openTestStore(t)
	scorer := NewStoreFreqScorer(s, testSchema, nil)

	// 未知词返回 0
	if boost := scorer.FreqBoost("abc", "词"); boost != 0 {
		t.Errorf("FreqBoost unknown word = %d, want 0", boost)
	}

	// 增加词频
	if err := s.IncrementFreq(testSchema, "abc", "词"); err != nil {
		t.Fatalf("IncrementFreq: %v", err)
	}

	// 增加后应 > 0
	if boost := scorer.FreqBoost("abc", "词"); boost <= 0 {
		t.Errorf("FreqBoost after increment = %d, want > 0", boost)
	}
}
