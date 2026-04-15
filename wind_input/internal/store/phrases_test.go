package store

import "testing"

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
