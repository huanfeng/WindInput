package dict

import (
	"path/filepath"
	"testing"
)

func newTestShadowLayer(t *testing.T) *ShadowLayer {
	t.Helper()
	tmpDir := t.TempDir()
	return NewShadowLayer("test_shadow", filepath.Join(tmpDir, "shadow.yaml"))
}

func TestShadowLayerPin(t *testing.T) {
	sl := newTestShadowLayer(t)

	sl.Pin("nihao", "你好", 0)

	if !sl.IsPinned("nihao", "你好") {
		t.Fatal("expected word to be pinned")
	}
	if sl.IsDeleted("nihao", "你好") {
		t.Fatal("pinned word should not be deleted")
	}

	rules := sl.GetShadowRules("nihao")
	if rules == nil || len(rules.Pinned) != 1 {
		t.Fatal("expected 1 pinned rule")
	}
	if rules.Pinned[0].Word != "你好" || rules.Pinned[0].Position != 0 {
		t.Fatalf("unexpected pin: %+v", rules.Pinned[0])
	}

	// Save and reload
	if err := sl.Save(); err != nil {
		t.Fatalf("save failed: %v", err)
	}
	sl2 := NewShadowLayer("test_shadow2", sl.filePath)
	if err := sl2.Load(); err != nil {
		t.Fatalf("load failed: %v", err)
	}
	if !sl2.IsPinned("nihao", "你好") {
		t.Fatal("pinned word should persist after reload")
	}
}

func TestShadowLayerDelete(t *testing.T) {
	sl := newTestShadowLayer(t)

	sl.Delete("zhongguo", "中国")

	if !sl.IsDeleted("zhongguo", "中国") {
		t.Fatal("expected word to be deleted")
	}
	if sl.IsPinned("zhongguo", "中国") {
		t.Fatal("deleted word should not be pinned")
	}
}

func TestShadowLayerRemoveRule(t *testing.T) {
	sl := newTestShadowLayer(t)

	sl.Pin("nihao", "你好", 0)
	if !sl.IsPinned("nihao", "你好") {
		t.Fatal("expected word to be pinned")
	}

	sl.RemoveRule("nihao", "你好")
	if sl.IsPinned("nihao", "你好") {
		t.Fatal("rule should be removed")
	}
}

func TestShadowLayerPinOverwritesDelete(t *testing.T) {
	sl := newTestShadowLayer(t)

	// Delete then pin — pin should remove from deleted
	sl.Delete("nihao", "你好")
	if !sl.IsDeleted("nihao", "你好") {
		t.Fatal("expected deleted")
	}

	sl.Pin("nihao", "你好", 0)
	if sl.IsDeleted("nihao", "你好") {
		t.Fatal("pin should remove deleted status")
	}
	if !sl.IsPinned("nihao", "你好") {
		t.Fatal("should be pinned after pin-over-delete")
	}

	rules := sl.GetShadowRules("nihao")
	if len(rules.Pinned) != 1 || len(rules.Deleted) != 0 {
		t.Fatalf("expected 1 pinned 0 deleted, got pinned=%d deleted=%d", len(rules.Pinned), len(rules.Deleted))
	}
}

func TestShadowLayerDeleteOverwritesPin(t *testing.T) {
	sl := newTestShadowLayer(t)

	sl.Pin("nihao", "你好", 0)
	sl.Delete("nihao", "你好")

	if sl.IsPinned("nihao", "你好") {
		t.Fatal("delete should remove pin status")
	}
	if !sl.IsDeleted("nihao", "你好") {
		t.Fatal("should be deleted")
	}
}

func TestShadowLayerCaseInsensitive(t *testing.T) {
	sl := newTestShadowLayer(t)

	sl.Pin("NiHao", "你好", 0)
	if !sl.IsPinned("nihao", "你好") {
		t.Fatal("code should be case-insensitive")
	}
}

func TestShadowLayerMultipleRules(t *testing.T) {
	sl := newTestShadowLayer(t)

	sl.Pin("nihao", "你好", 0)
	sl.Delete("nihao", "泥号")
	sl.Pin("shijie", "世界", 0)
	sl.Pin("shijie", "时节", 2)

	if sl.GetRuleCount() != 4 {
		t.Fatalf("expected 4 total rules, got %d", sl.GetRuleCount())
	}

	// Save and reload
	if err := sl.Save(); err != nil {
		t.Fatalf("save failed: %v", err)
	}
	sl2 := NewShadowLayer("test_shadow2", sl.filePath)
	if err := sl2.Load(); err != nil {
		t.Fatalf("load failed: %v", err)
	}
	if sl2.GetRuleCount() != 4 {
		t.Fatalf("expected 4 rules after reload, got %d", sl2.GetRuleCount())
	}
}

func TestShadowLayerPinLIFO(t *testing.T) {
	sl := newTestShadowLayer(t)

	// Pin A then B to position 0 — B should be first in array (LIFO)
	sl.Pin("aa", "式", 0)
	sl.Pin("aa", "戒", 0)

	rules := sl.GetShadowRules("aa")
	if len(rules.Pinned) != 2 {
		t.Fatalf("expected 2 pinned, got %d", len(rules.Pinned))
	}
	// 戒 was pinned last → should be first (LIFO)
	if rules.Pinned[0].Word != "戒" {
		t.Errorf("LIFO: last pinned '戒' should be first, got %q", rules.Pinned[0].Word)
	}
	if rules.Pinned[1].Word != "式" {
		t.Errorf("LIFO: first pinned '式' should be second, got %q", rules.Pinned[1].Word)
	}
}

func TestShadowLayerPinUpdatePosition(t *testing.T) {
	sl := newTestShadowLayer(t)

	sl.Pin("aa", "工", 2)
	sl.Pin("aa", "工", 0) // update position and move to front

	rules := sl.GetShadowRules("aa")
	if len(rules.Pinned) != 1 {
		t.Fatalf("expected 1 pinned (no dup), got %d", len(rules.Pinned))
	}
	if rules.Pinned[0].Position != 0 {
		t.Errorf("position should be updated to 0, got %d", rules.Pinned[0].Position)
	}
}
