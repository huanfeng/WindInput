package dict

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/store"
)

// setupTestManager 创建基于 Store 的测试 DictManager
func setupTestManager(t *testing.T) *DictManager {
	t.Helper()
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "test.db")

	dm := NewDictManager(tmpDir, tmpDir, nil)
	if err := dm.OpenStore(dbPath); err != nil {
		t.Fatalf("OpenStore failed: %v", err)
	}
	if err := dm.Initialize(); err != nil {
		t.Fatalf("Initialize failed: %v", err)
	}
	t.Cleanup(func() { dm.Close() })
	return dm
}

func TestDictManager_SwitchSchema(t *testing.T) {
	dm := setupTestManager(t)

	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)

	if dm.GetActiveSchemaID() != "wubi86" {
		t.Errorf("expected wubi86, got %s", dm.GetActiveSchemaID())
	}
	if dm.GetStoreShadowLayer() == nil {
		t.Error("StoreShadowLayer should not be nil")
	}
	if dm.GetStoreUserLayer() == nil {
		t.Error("StoreUserLayer should not be nil")
	}

	// 添加用户词
	if err := dm.AddUserWord("test", "测试", 100); err != nil {
		t.Fatalf("AddUserWord failed: %v", err)
	}

	// 切换到 pinyin
	dm.SwitchSchemaFull("pinyin", "pinyin", 5000, 5)
	if dm.GetActiveSchemaID() != "pinyin" {
		t.Errorf("expected pinyin, got %s", dm.GetActiveSchemaID())
	}
	if dm.GetStoreUserLayer().EntryCount() != 0 {
		t.Errorf("pinyin should have 0 entries, got %d", dm.GetStoreUserLayer().EntryCount())
	}

	// 切换回 wubi86，数据应保留
	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	if dm.GetStoreUserLayer().EntryCount() != 1 {
		t.Errorf("wubi86 should have 1 entry, got %d", dm.GetStoreUserLayer().EntryCount())
	}
}

func TestDictManager_ShadowIsolation(t *testing.T) {
	dm := setupTestManager(t)

	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	dm.PinWord("abc", "测试", 0)

	rules := dm.GetStoreShadowLayer().GetShadowRules("abc")
	if rules == nil || len(rules.Pinned) != 1 {
		t.Fatal("wubi86 should have 1 pin rule")
	}

	// 切换到 pinyin，shadow 应独立
	dm.SwitchSchemaFull("pinyin", "pinyin", 5000, 5)
	pinyinRules := dm.GetStoreShadowLayer().GetShadowRules("abc")
	if pinyinRules != nil && (len(pinyinRules.Pinned) > 0 || len(pinyinRules.Deleted) > 0) {
		t.Error("pinyin should have no shadow rules")
	}

	// 切换回 wubi86，规则应保留
	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	rules2 := dm.GetStoreShadowLayer().GetShadowRules("abc")
	if rules2 == nil || len(rules2.Pinned) != 1 {
		t.Error("wubi86 shadow rules should persist")
	}
}

func TestDictManager_SameSchemaNoOp(t *testing.T) {
	dm := setupTestManager(t)

	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	dm.AddUserWord("a", "甲", 100)

	// 再次切换到相同方案应该是 no-op
	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	if dm.GetStoreUserLayer().EntryCount() != 1 {
		t.Error("same-schema switch should not lose data")
	}
}

func TestDictManager_MixedSchemaSharesData(t *testing.T) {
	dm := setupTestManager(t)

	// 混输方案 wubi86_pinyin 应与主方案 wubi86 共享用户数据
	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	dm.AddUserWord("test", "测试", 100)

	// 混输方案使用相同的 dataSchemaID
	dm.SwitchSchemaFull("wubi86_pinyin", "wubi86", 5000, 5)
	if dm.GetStoreUserLayer().EntryCount() != 1 {
		t.Errorf("mixed schema should share data with primary, got %d entries", dm.GetStoreUserLayer().EntryCount())
	}
}

func TestDictManager_StoreFreqScorer(t *testing.T) {
	dm := setupTestManager(t)

	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)

	// 通过 Store 记录词频
	s := dm.GetStore()
	if s == nil {
		t.Fatal("Store should not be nil")
	}

	s.IncrementFreq("wubi86", "abc", "测试")
	s.IncrementFreq("wubi86", "abc", "测试")

	rec, err := s.GetFreq("wubi86", "abc", "测试")
	if err != nil {
		t.Fatal(err)
	}
	if rec.Count != 2 {
		t.Errorf("expected freq count 2, got %d", rec.Count)
	}
}

func TestDictManager_DeleteUserWord_NoShadow(t *testing.T) {
	dm := setupTestManager(t)
	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)

	// 添加用户词后删除：应直接删除，不产生 Shadow
	dm.AddUserWord("abc", "测试", 100)
	if dm.GetStoreUserLayer().EntryCount() != 1 {
		t.Fatal("should have 1 user word")
	}

	dm.DeleteWord("abc", "测试")
	if dm.GetStoreUserLayer().EntryCount() != 0 {
		t.Error("user word should be deleted")
	}
	if dm.HasShadowRule("abc", "测试") {
		t.Error("should NOT have shadow rule for non-system word")
	}
}

func TestDictManager_ShadowPinAndRemove(t *testing.T) {
	dm := setupTestManager(t)
	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)

	// Pin + RemoveRule 流程仍正常
	dm.PinWord("abc", "测试", 0)
	if !dm.HasShadowRule("abc", "测试") {
		t.Error("should have shadow rule after PinWord")
	}

	dm.RemoveShadowRule("abc", "测试")
	if dm.HasShadowRule("abc", "测试") {
		t.Error("should not have shadow rule after RemoveShadowRule")
	}
}

// 确保 store 包被使用（setupTestManager 间接使用）
var _ = store.FreqRecord{}
