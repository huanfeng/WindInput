package store

import (
	"encoding/json"
	"testing"

	bolt "go.etcd.io/bbolt"
)

func TestPhrase_AddAndGetAll(t *testing.T) {
	s := openTestStore(t)

	phrases := []PhraseRecord{
		{Code: "rq", Text: "日期", Type: "static", Position: 1, Enabled: true},
		{Code: "sj", Text: "{time}", Type: "dynamic", Position: 2, Enabled: true},
		{Code: "dz", Name: "地址", Texts: "北京市|海淀区|中关村", Type: "array", Position: 3, Enabled: true},
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

	// Verify fields are populated correctly.
	for _, rec := range all {
		if rec.Code == "" {
			t.Error("Code should be populated from key")
		}
		if rec.Type == "" {
			t.Error("Type should be set")
		}
	}
}

func TestPhrase_GetByCode(t *testing.T) {
	s := openTestStore(t)

	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期", Type: "static", Enabled: true})
	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期2", Type: "static", Enabled: true})
	_ = s.AddPhrase(PhraseRecord{Code: "sj", Text: "{time}", Type: "dynamic", Enabled: true})

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

	rec := PhraseRecord{Code: "rq", Text: "日期", Type: "static", Position: 1, Enabled: true}
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

	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期", Type: "static", Enabled: true})
	if err := s.RemovePhrase("rq", "日期", ""); err != nil {
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

	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期", Type: "static", Enabled: true})
	if err := s.SetPhraseEnabled("rq", "日期", "", false); err != nil {
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
		{Code: "rq", Text: "日期", Type: "static", Position: 1, Enabled: true, IsSystem: true},
		{Code: "sj", Text: "{time}", Type: "dynamic", Position: 2, Enabled: true, IsSystem: true},
	}
	if err := s.SeedPhrases(seeds); err != nil {
		t.Fatalf("SeedPhrases (first): %v", err)
	}

	// Add a custom phrase.
	_ = s.AddPhrase(PhraseRecord{Code: "custom", Text: "自定义", Type: "static", Enabled: true})

	// Seed again — should be a no-op.
	newSeeds := []PhraseRecord{
		{Code: "xx", Text: "新的", Type: "static", Position: 1, Enabled: true, IsSystem: true},
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

	_ = s.AddPhrase(PhraseRecord{Code: "rq", Text: "日期", Type: "static", Enabled: true})
	_ = s.AddPhrase(PhraseRecord{Code: "sj", Text: "{time}", Type: "dynamic", Enabled: true})

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

func TestPhrase_ArrayKey(t *testing.T) {
	s := openTestStore(t)

	rec := PhraseRecord{
		Code:    "dz",
		Name:    "地址",
		Texts:   "北京市|海淀区|中关村",
		Type:    "array",
		Enabled: true,
	}
	_ = s.AddPhrase(rec)

	results, err := s.GetPhrasesByCode("dz")
	if err != nil {
		t.Fatalf("GetPhrasesByCode: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("expected 1, got %d", len(results))
	}
	if results[0].Name != "地址" {
		t.Errorf("expected Name '地址', got %q", results[0].Name)
	}

	// Remove by name.
	if err := s.RemovePhrase("dz", "", "地址"); err != nil {
		t.Fatalf("RemovePhrase (array): %v", err)
	}
	count, err := s.PhraseCount()
	if err != nil {
		t.Fatalf("PhraseCount: %v", err)
	}
	if count != 0 {
		t.Fatalf("expected 0 after removing array phrase, got %d", count)
	}
}

// 注入 raw key+value, 模拟 legacy/marker 化前的残留记录
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

// 覆盖各种 legacy / 混合 key 形态, 验证 RemovePhrase 都能命中。
func TestPhrase_RemoveLegacyKeyForms(t *testing.T) {
	cases := []struct {
		name    string
		key     []byte
		rec     PhraseRecord
		argText string
		argName string
	}{
		{
			name:    "legacy_array_name_key",
			key:     []byte("dz\x00\x01地址"),
			rec:     PhraseRecord{Name: "地址", Texts: "北京|上海", Type: "array", Enabled: true},
			argText: "",
			argName: "地址",
		},
		{
			name:    "marker_text_key_with_array_arg",
			key:     []byte(`dz` + "\x00" + `$AA("地址", "北京|上海")`),
			rec:     PhraseRecord{Text: `$AA("地址", "北京|上海")`, Type: "array", Enabled: true},
			argText: `$AA("地址", "北京|上海")`,
			argName: "",
		},
		{
			name:    "marker_text_key_remove_by_legacy_name",
			key:     []byte(`dz` + "\x00" + `$AA("地址", "北京|上海")`),
			rec:     PhraseRecord{Text: `$AA("地址", "北京|上海")`, Type: "array", Enabled: true},
			argText: "",
			argName: "地址",
		},
		{
			name:    "legacy_array_key_remove_with_marker_text",
			key:     []byte("dz\x00\x01地址"),
			rec:     PhraseRecord{Name: "地址", Texts: "北京|上海", Type: "array", Enabled: true},
			argText: `$AA("地址", "北京|上海")`,
			argName: "",
		},
		{
			name:    "normal_text_key",
			key:     []byte("rq\x00日期"),
			rec:     PhraseRecord{Text: "日期", Type: "static", Enabled: true},
			argText: "日期",
			argName: "",
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
			if err := s.RemovePhrase(code, tc.argText, tc.argName); err != nil {
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

// 批量删除也走 ForEach 兜底
func TestPhrase_BatchRemoveLegacyMixed(t *testing.T) {
	s := openTestStore(t)
	putRawPhrase(t, s, []byte("dz\x00\x01地址"),
		PhraseRecord{Name: "地址", Texts: "北京|上海", Type: "array", Enabled: true})
	putRawPhrase(t, s, []byte("rq\x00日期"),
		PhraseRecord{Text: "日期", Type: "static", Enabled: true})
	putRawPhrase(t, s, []byte("kp\x00"+`$AA("快捷", "A|B")`),
		PhraseRecord{Text: `$AA("快捷", "A|B")`, Type: "array", Enabled: true})

	items := []PhraseRecord{
		{Code: "dz", Text: "", Name: "地址"},
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
