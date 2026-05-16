package store

import (
	"encoding/json"
	"testing"

	bolt "go.etcd.io/bbolt"
)

func TestPhrase_AddAndGetAll(t *testing.T) {
	s := openTestStore(t)

	phrases := []PhraseRecord{
		{Code: "rq", Text: "日期", Position: 1, Enabled: true},
		{Code: "sj", Text: "{time}", Position: 2, Enabled: true},
		{Code: "dz", Text: `$AA("地址", "北京市|海淀区|中关村")`, Position: 3, Enabled: true},
	}
	for _, p := range phrases {
		if err := s.AddPhrase(p); err != nil {
			t.Fatalf("AddPhrase(%q): %v", p.Code, err)
		}
	}

	all, err := s.GetAllPhrases()
	if err != nil {
		t.Fatalf("GetAllPhrases: %v", err)
	}
	if len(all) != 3 {
		t.Fatalf("expected 3 phrases, got %d", len(all))
	}

	// Verify Code is populated from key, Text is preserved.
	for _, rec := range all {
		if rec.Code == "" {
			t.Error("Code should be populated from key")
		}
		if rec.Text == "" {
			t.Errorf("Text should not be empty for %q", rec.Code)
		}
	}
}

func TestPhrase_GetByCode(t *testing.T) {
	s := openTestStore(t)

	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期", Enabled: true})
	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期2", Enabled: true})
	_ = s.AddPhrase(PhraseRecord{Code: "sj", Text: "{time}", Enabled: true})

	results, err := s.GetPhrasesByCode("rq")
	if err != nil {
		t.Fatalf("GetPhrasesByCode: %v", err)
	}
	if len(results) != 2 {
		t.Fatalf("expected 2 phrases for code 'rq', got %d", len(results))
	}
	for _, r := range results {
		if r.Code != "rq" {
			t.Errorf("expected code 'rq', got %q", r.Code)
		}
	}
}

func TestPhrase_AddDuplicate(t *testing.T) {
	s := openTestStore(t)

	rec := PhraseRecord{Code: "rq", Text: "日期", Position: 1, Enabled: true}
	_ = s.AddPhrase(rec)
	rec.Position = 5
	_ = s.AddPhrase(rec)

	count, err := s.PhraseCount()
	if err != nil {
		t.Fatalf("PhraseCount: %v", err)
	}
	if count != 1 {
		t.Fatalf("expected 1 after duplicate add, got %d", count)
	}
}

func TestPhrase_Remove(t *testing.T) {
	s := openTestStore(t)

	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期", Enabled: true})
	if err := s.RemovePhrase("rq", "日期"); err != nil {
		t.Fatalf("RemovePhrase: %v", err)
	}

	count, err := s.PhraseCount()
	if err != nil {
		t.Fatalf("PhraseCount: %v", err)
	}
	if count != 0 {
		t.Fatalf("expected 0 after remove, got %d", count)
	}
}

func TestPhrase_SetEnabled(t *testing.T) {
	s := openTestStore(t)

	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期", Enabled: true})
	if err := s.SetPhraseEnabled("rq", "日期", false); err != nil {
		t.Fatalf("SetPhraseEnabled: %v", err)
	}

	results, err := s.GetPhrasesByCode("rq")
	if err != nil {
		t.Fatalf("GetPhrasesByCode: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("expected 1, got %d", len(results))
	}
	if results[0].Enabled {
		t.Error("expected Enabled=false after SetPhraseEnabled(false)")
	}
}

func TestPhrase_SeedNoOverwrite(t *testing.T) {
	s := openTestStore(t)

	seeds := []PhraseRecord{
		{Code: "rq", Text: "日期", Position: 1, Enabled: true, IsSystem: true},
		{Code: "sj", Text: "{time}", Position: 2, Enabled: true, IsSystem: true},
	}
	if err := s.SeedPhrases(seeds); err != nil {
		t.Fatalf("SeedPhrases (first): %v", err)
	}

	// Add a custom phrase.
	_ = s.AddPhrase(PhraseRecord{Code: "custom", Text: "自定义", Enabled: true})

	// Seed again — should be a no-op.
	newSeeds := []PhraseRecord{
		{Code: "xx", Text: "新的", Position: 1, Enabled: true, IsSystem: true},
	}
	if err := s.SeedPhrases(newSeeds); err != nil {
		t.Fatalf("SeedPhrases (second): %v", err)
	}

	count, err := s.PhraseCount()
	if err != nil {
		t.Fatalf("PhraseCount: %v", err)
	}
	// 2 seeds + 1 custom = 3; the second seed should NOT have added "xx".
	if count != 3 {
		t.Fatalf("expected 3 phrases, got %d", count)
	}
}

func TestPhrase_ClearAll(t *testing.T) {
	s := openTestStore(t)

	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期", Enabled: true})
	_ = s.AddPhrase(PhraseRecord{Code: "sj", Text: "{time}", Enabled: true})

	if err := s.ClearAllPhrases(); err != nil {
		t.Fatalf("ClearAllPhrases: %v", err)
	}

	count, err := s.PhraseCount()
	if err != nil {
		t.Fatalf("PhraseCount: %v", err)
	}
	if count != 0 {
		t.Fatalf("expected 0 after clear, got %d", count)
	}
}

// TestPhrase_AAMarkerStorage 验证 $AA 字符组短语在新 schema 下用单一 Text
// 字段存储, 不再有 Name/Texts 双字段。
func TestPhrase_AAMarkerStorage(t *testing.T) {
	s := openTestStore(t)

	rec := PhraseRecord{
		Code:    "dz",
		Text:    `$AA("地址", "北京市|海淀区|中关村")`,
		Enabled: true,
	}
	if err := s.AddPhrase(rec); err != nil {
		t.Fatalf("AddPhrase: %v", err)
	}

	results, err := s.GetPhrasesByCode("dz")
	if err != nil {
		t.Fatalf("GetPhrasesByCode: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("expected 1, got %d", len(results))
	}
	if results[0].Text != `$AA("地址", "北京市|海淀区|中关村")` {
		t.Errorf("Text mismatch: %q", results[0].Text)
	}

	// Remove by text.
	if err := s.RemovePhrase("dz", `$AA("地址", "北京市|海淀区|中关村")`); err != nil {
		t.Fatalf("RemovePhrase: %v", err)
	}
	count, err := s.PhraseCount()
	if err != nil {
		t.Fatalf("PhraseCount: %v", err)
	}
	if count != 0 {
		t.Fatalf("expected 0 after removing $AA phrase, got %d", count)
	}
}

// putRawPhrase 注入 raw key+value, 模拟 legacy/marker 化前的残留记录
func putRawPhrase(t *testing.T, s *Store, key []byte, rec PhraseRecord) {
	t.Helper()
	if err := s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketPhrases)
		data, err := json.Marshal(rec)
		if err != nil {
			return err
		}
		return b.Put(key, data)
	}); err != nil {
		t.Fatalf("putRawPhrase: %v", err)
	}
}

// TestPhrase_RemoveLegacyKeyForms 验证 RemovePhrase 能命中各种 legacy key
// 形态 (包括迁移前的 code\x00\x01name 残留)。
//
// 2026-05-16 简化后, 删除接口只接受 (code, text), 但内部 ForEach 扫描
// 兜底匹配 rec.Text, 所以历史 key 形式只要 rec.Text 字段对应即可命中。
func TestPhrase_RemoveLegacyKeyForms(t *testing.T) {
	cases := []struct {
		name    string
		key     []byte
		rec     PhraseRecord
		argText string
	}{
		{
			name:    "marker_text_normal_key",
			key:     []byte(`dz` + "\x00" + `$AA("地址", "北京|上海")`),
			rec:     PhraseRecord{Text: `$AA("地址", "北京|上海")`, Enabled: true},
			argText: `$AA("地址", "北京|上海")`,
		},
		{
			name:    "normal_text_key",
			key:     []byte("rq\x00日期"),
			rec:     PhraseRecord{Text: "日期", Enabled: true},
			argText: "日期",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			s := openTestStore(t)
			putRawPhrase(t, s, tc.key, tc.rec)

			// 推断 code: 取 key 的 \x00 前部分
			code := ""
			for i, b := range tc.key {
				if b == 0 {
					code = string(tc.key[:i])
					break
				}
			}
			if err := s.RemovePhrase(code, tc.argText); err != nil {
				t.Fatalf("RemovePhrase: %v", err)
			}
			count, err := s.PhraseCount()
			if err != nil {
				t.Fatalf("PhraseCount: %v", err)
			}
			if count != 0 {
				t.Fatalf("expected 0 after remove (%s), got %d", tc.name, count)
			}
		})
	}
}

// TestPhrase_BatchRemove 验证批量删除接口。
func TestPhrase_BatchRemove(t *testing.T) {
	s := openTestStore(t)
	_ = s.AddPhrase(PhraseRecord{Code: "dz", Text: `$AA("地址", "北京|上海")`, Enabled: true})
	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期", Enabled: true})
	_ = s.AddPhrase(PhraseRecord{Code: "kp", Text: `$AA("快捷", "A|B")`, Enabled: true})

	items := []PhraseRecord{
		{Code: "dz", Text: `$AA("地址", "北京|上海")`},
		{Code: "rq", Text: "日期"},
		{Code: "kp", Text: `$AA("快捷", "A|B")`},
	}
	if err := s.RemovePhrasesBatch(items); err != nil {
		t.Fatalf("RemovePhrasesBatch: %v", err)
	}
	count, err := s.PhraseCount()
	if err != nil {
		t.Fatalf("PhraseCount: %v", err)
	}
	if count != 0 {
		t.Fatalf("expected 0 after batch remove, got %d", count)
	}
}

func TestListSchemaIDs(t *testing.T) {
	s := openTestStore(t)

	// Add user words to two different schemas to create schema buckets.
	if err := s.AddUserWord("wubi86", "a", "工", 100); err != nil {
		t.Fatalf("AddUserWord: %v", err)
	}
	if err := s.AddUserWord("pinyin", "gong", "工", 100); err != nil {
		t.Fatalf("AddUserWord: %v", err)
	}

	ids, err := s.ListSchemaIDs()
	if err != nil {
		t.Fatalf("ListSchemaIDs: %v", err)
	}
	if len(ids) != 2 {
		t.Fatalf("expected 2 schema IDs, got %d", len(ids))
	}
	// Should be sorted.
	if ids[0] != "pinyin" || ids[1] != "wubi86" {
		t.Errorf("expected [pinyin wubi86], got %v", ids)
	}
}
