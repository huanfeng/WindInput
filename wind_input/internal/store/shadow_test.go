package store

import (
	"testing"
)

const testSchema = "test_schema"

func TestShadow_PinAndGet(t *testing.T) {
	s := openTestStore(t)

	if err := s.PinShadow(testSchema, "ni", "你", 0); err != nil {
		t.Fatalf("PinShadow: %v", err)
	}

	rec, err := s.GetShadowRules(testSchema, "ni")
	if err != nil {
		t.Fatalf("GetShadowRules: %v", err)
	}
	if len(rec.Pinned) != 1 {
		t.Fatalf("expected 1 pin, got %d", len(rec.Pinned))
	}
	if rec.Pinned[0].Word != "你" || rec.Pinned[0].Position != 0 {
		t.Errorf("unexpected pin: %+v", rec.Pinned[0])
	}
	if len(rec.Deleted) != 0 {
		t.Errorf("expected no deleted entries, got %v", rec.Deleted)
	}
}

func TestShadow_DeleteAndGet(t *testing.T) {
	s := openTestStore(t)

	if err := s.DeleteShadow(testSchema, "ni", "你"); err != nil {
		t.Fatalf("DeleteShadow: %v", err)
	}

	rec, err := s.GetShadowRules(testSchema, "ni")
	if err != nil {
		t.Fatalf("GetShadowRules: %v", err)
	}
	if len(rec.Deleted) != 1 || rec.Deleted[0] != "你" {
		t.Errorf("expected deleted=[你], got %v", rec.Deleted)
	}
	if len(rec.Pinned) != 0 {
		t.Errorf("expected no pins, got %v", rec.Pinned)
	}
}

func TestShadow_PinOverridesDelete(t *testing.T) {
	s := openTestStore(t)

	// First delete the word.
	if err := s.DeleteShadow(testSchema, "ni", "你"); err != nil {
		t.Fatalf("DeleteShadow: %v", err)
	}

	// Now pin the same word — it must be removed from Deleted.
	if err := s.PinShadow(testSchema, "ni", "你", 1); err != nil {
		t.Fatalf("PinShadow: %v", err)
	}

	rec, err := s.GetShadowRules(testSchema, "ni")
	if err != nil {
		t.Fatalf("GetShadowRules: %v", err)
	}
	if len(rec.Pinned) != 1 || rec.Pinned[0].Word != "你" {
		t.Errorf("expected 1 pin for 你, got %+v", rec.Pinned)
	}
	if len(rec.Deleted) != 0 {
		t.Errorf("expected deleted to be empty after pin, got %v", rec.Deleted)
	}
}

func TestShadow_RemoveRule(t *testing.T) {
	s := openTestStore(t)

	// Pin two words and delete a third under the same code.
	if err := s.PinShadow(testSchema, "ni", "你", 0); err != nil {
		t.Fatalf("PinShadow 你: %v", err)
	}
	if err := s.PinShadow(testSchema, "ni", "妮", 1); err != nil {
		t.Fatalf("PinShadow 妮: %v", err)
	}
	if err := s.DeleteShadow(testSchema, "ni", "拟"); err != nil {
		t.Fatalf("DeleteShadow 拟: %v", err)
	}

	// Remove only 你's rules.
	if err := s.RemoveShadowRule(testSchema, "ni", "你"); err != nil {
		t.Fatalf("RemoveShadowRule: %v", err)
	}

	rec, err := s.GetShadowRules(testSchema, "ni")
	if err != nil {
		t.Fatalf("GetShadowRules: %v", err)
	}
	// 妮 pin should remain.
	if len(rec.Pinned) != 1 || rec.Pinned[0].Word != "妮" {
		t.Errorf("expected pin [妮], got %+v", rec.Pinned)
	}
	// 拟 delete should remain.
	if len(rec.Deleted) != 1 || rec.Deleted[0] != "拟" {
		t.Errorf("expected deleted [拟], got %v", rec.Deleted)
	}
}

func TestShadow_GetRuleCount(t *testing.T) {
	s := openTestStore(t)

	if err := s.PinShadow(testSchema, "ni", "你", 0); err != nil {
		t.Fatalf("PinShadow ni: %v", err)
	}
	if err := s.DeleteShadow(testSchema, "wo", "我"); err != nil {
		t.Fatalf("DeleteShadow wo: %v", err)
	}

	count, err := s.ShadowRuleCount(testSchema)
	if err != nil {
		t.Fatalf("ShadowRuleCount: %v", err)
	}
	if count != 2 {
		t.Errorf("expected count=2, got %d", count)
	}
}
