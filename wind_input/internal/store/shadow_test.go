package store

import (
	"testing"

	bolt "go.etcd.io/bbolt"
)

const testSchema = "test_schema"

func TestShadow_PinAndGet(t *testing.T) {
	s := openTestStore(t)

	if err := s.PinShadow(testSchema, "ni", "你", "", 0); err != nil {
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

	if err := s.DeleteShadow(testSchema, "ni", "你", ""); err != nil {
		t.Fatalf("DeleteShadow: %v", err)
	}

	rec, err := s.GetShadowRules(testSchema, "ni")
	if err != nil {
		t.Fatalf("GetShadowRules: %v", err)
	}
	if len(rec.Deleted) != 1 || rec.Deleted[0].Word != "你" {
		t.Errorf("expected deleted=[你], got %v", rec.Deleted)
	}
	if len(rec.Pinned) != 0 {
		t.Errorf("expected no pins, got %v", rec.Pinned)
	}
}

func TestShadow_PinOverridesDelete(t *testing.T) {
	s := openTestStore(t)

	// First delete the word.
	if err := s.DeleteShadow(testSchema, "ni", "你", ""); err != nil {
		t.Fatalf("DeleteShadow: %v", err)
	}

	// Now pin the same word — it must be removed from Deleted.
	if err := s.PinShadow(testSchema, "ni", "你", "", 1); err != nil {
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

func TestShadow_DeleteOverridesPin(t *testing.T) {
	s := openTestStore(t)

	// Pin first
	if err := s.PinShadow(testSchema, "ni", "你", "", 0); err != nil {
		t.Fatalf("PinShadow: %v", err)
	}
	// Then delete: pin should be removed (互斥)
	if err := s.DeleteShadow(testSchema, "ni", "你", ""); err != nil {
		t.Fatalf("DeleteShadow: %v", err)
	}

	rec, _ := s.GetShadowRules(testSchema, "ni")
	if len(rec.Pinned) != 0 {
		t.Errorf("expected pin cleared by delete, got %+v", rec.Pinned)
	}
	if len(rec.Deleted) != 1 || rec.Deleted[0].Word != "你" {
		t.Errorf("expected 1 delete, got %+v", rec.Deleted)
	}
}

func TestShadow_RemoveRule(t *testing.T) {
	s := openTestStore(t)

	// Pin two words and delete a third under the same code.
	if err := s.PinShadow(testSchema, "ni", "你", "", 0); err != nil {
		t.Fatalf("PinShadow 你: %v", err)
	}
	if err := s.PinShadow(testSchema, "ni", "妮", "", 1); err != nil {
		t.Fatalf("PinShadow 妮: %v", err)
	}
	if err := s.DeleteShadow(testSchema, "ni", "拟", ""); err != nil {
		t.Fatalf("DeleteShadow 拟: %v", err)
	}

	// Remove only 你's rules.
	if err := s.RemoveShadowRule(testSchema, "ni", "你", ""); err != nil {
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
	if len(rec.Deleted) != 1 || rec.Deleted[0].Word != "拟" {
		t.Errorf("expected deleted [拟], got %v", rec.Deleted)
	}
}

func TestShadow_GetRuleCount(t *testing.T) {
	s := openTestStore(t)

	if err := s.PinShadow(testSchema, "ni", "你", "", 0); err != nil {
		t.Fatalf("PinShadow ni: %v", err)
	}
	if err := s.DeleteShadow(testSchema, "wo", "我", ""); err != nil {
		t.Fatalf("DeleteShadow wo: %v", err)
	}

	count, err := s.ShadowRuleCount(testSchema)
	if err != nil {
		t.Fatalf("ShadowRuleCount: %v", err)
	}
	if count != 2 {
		t.Errorf("expected count=2 (1 pin + 1 delete), got %d", count)
	}
}

// TestShadow_PinByCandID 验证 CandID 优先匹配: 同 (code, word) 不同 candID
// 视为不同规则; CandID 非空时按 id 替换。
func TestShadow_PinByCandID(t *testing.T) {
	s := openTestStore(t)

	// 两条不同 candID 的 pin (word 相同 — 模拟动态短语展开后撞 text)
	if err := s.PinShadow(testSchema, "rq", "2026-05-17", "phrase:rq:$Y-$MM-$DD", 0); err != nil {
		t.Fatalf("PinShadow id-1: %v", err)
	}
	if err := s.PinShadow(testSchema, "rq", "2026-05-17", "phrase:rq:$Y年$M月$D日", 1); err != nil {
		t.Fatalf("PinShadow id-2: %v", err)
	}
	rec, err := s.GetShadowRules(testSchema, "rq")
	if err != nil {
		t.Fatalf("GetShadowRules: %v", err)
	}
	if len(rec.Pinned) != 2 {
		t.Fatalf("expected 2 pins (distinct by candID), got %d", len(rec.Pinned))
	}

	// 用第一个 candID 重新 pin: 替换该 id 规则, 另一个保留
	if err := s.PinShadow(testSchema, "rq", "today", "phrase:rq:$Y-$MM-$DD", 2); err != nil {
		t.Fatalf("PinShadow id-1 replace: %v", err)
	}
	rec, _ = s.GetShadowRules(testSchema, "rq")
	if len(rec.Pinned) != 2 {
		t.Fatalf("expected 2 pins after replace, got %d", len(rec.Pinned))
	}
	// 找到 candID = phrase:rq:$Y-$MM-$DD 的位置应是 2
	for _, p := range rec.Pinned {
		if p.CandID == "phrase:rq:$Y-$MM-$DD" && p.Position != 2 {
			t.Errorf("expected replaced pin position 2, got %d", p.Position)
		}
	}
}

// TestShadow_RemoveByCandID 验证按 id 删规则不波及同 word 的其它 id 规则。
func TestShadow_RemoveByCandID(t *testing.T) {
	s := openTestStore(t)

	if err := s.PinShadow(testSchema, "rq", "2026-05-17", "phrase:rq:A", 0); err != nil {
		t.Fatalf("PinShadow A: %v", err)
	}
	if err := s.PinShadow(testSchema, "rq", "2026-05-17", "phrase:rq:B", 1); err != nil {
		t.Fatalf("PinShadow B: %v", err)
	}
	if err := s.RemoveShadowRule(testSchema, "rq", "2026-05-17", "phrase:rq:A"); err != nil {
		t.Fatalf("RemoveShadowRule A: %v", err)
	}
	rec, _ := s.GetShadowRules(testSchema, "rq")
	if len(rec.Pinned) != 1 {
		t.Fatalf("expected 1 pin after remove, got %d", len(rec.Pinned))
	}
	if rec.Pinned[0].CandID != "phrase:rq:B" {
		t.Errorf("expected remaining candID=phrase:rq:B, got %q", rec.Pinned[0].CandID)
	}
}

// TestShadow_LegacyDeletedFormat 验证 ShadowDelete.UnmarshalJSON 兼容
// 旧版 db 写入的 `"d":["词A","词B"]` 纯字符串格式 (2026-05-17 之前)。
// 旧格式的 delete 写入了方案桶, 当前方案桶设计直接复用兼容路径。
func TestShadow_LegacyDeletedFormat(t *testing.T) {
	s := openTestStore(t)

	// 直接以旧格式 (Deleted 为 []string) 注入方案桶
	legacy := []byte(`{"p":[{"w":"词A","pos":0}],"d":["旧词1","旧词2"]}`)
	if err := s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, testSchema, string(bucketShadow), true)
		if err != nil {
			return err
		}
		return b.Put([]byte("zz"), legacy)
	}); err != nil {
		t.Fatalf("seed legacy: %v", err)
	}

	rec, err := s.GetShadowRules(testSchema, "zz")
	if err != nil {
		t.Fatalf("GetShadowRules: %v", err)
	}
	if len(rec.Pinned) != 1 {
		t.Errorf("expected 1 pin from legacy data, got %d", len(rec.Pinned))
	}
	if len(rec.Deleted) != 2 {
		t.Fatalf("expected 2 deleted entries from legacy [\"旧词1\",\"旧词2\"], got %d", len(rec.Deleted))
	}
	if rec.Deleted[0].Word != "旧词1" || rec.Deleted[0].CandID != "" {
		t.Errorf("legacy[0] mismatch: %+v", rec.Deleted[0])
	}
	if rec.Deleted[1].Word != "旧词2" || rec.Deleted[1].CandID != "" {
		t.Errorf("legacy[1] mismatch: %+v", rec.Deleted[1])
	}
}

// TestShadow_DeleteSameSchema 验证 delete 写方案桶后, 同方案 Get 能读到,
// 不同方案 Get 读不到 (回归方案桶设计)。
func TestShadow_DeleteSameSchema(t *testing.T) {
	s := openTestStore(t)

	const schemaA = "schemaA"
	const schemaB = "schemaB"

	// schemaA 下 delete
	if err := s.DeleteShadow(schemaA, "n", "拟", "phrase:n:T"); err != nil {
		t.Fatalf("DeleteShadow A: %v", err)
	}

	// schemaA 下 Get 应能读到
	recA, err := s.GetShadowRules(schemaA, "n")
	if err != nil {
		t.Fatalf("GetShadowRules A: %v", err)
	}
	if len(recA.Deleted) != 1 || recA.Deleted[0].Word != "拟" || recA.Deleted[0].CandID != "phrase:n:T" {
		t.Errorf("expected delete in schemaA, got %v", recA.Deleted)
	}

	// schemaB 下 Get 不应读到 (方案桶隔离)
	recB, err := s.GetShadowRules(schemaB, "n")
	if err != nil {
		t.Fatalf("GetShadowRules B: %v", err)
	}
	if len(recB.Deleted) != 0 {
		t.Errorf("expected no delete in schemaB (按方案隔离), got %v", recB.Deleted)
	}
}
