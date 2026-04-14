package store

import (
	"testing"
)

func TestTempWords_LearnAndPromote(t *testing.T) {
	s := openTestStore(t)
	schema := "wubi86"
	code := "abc"
	text := "测试"

	// Learn once.
	if err := s.LearnTempWord(schema, code, text, 5); err != nil {
		t.Fatalf("LearnTempWord #1: %v", err)
	}
	// Learn again — weight should accumulate, count should be 2.
	if err := s.LearnTempWord(schema, code, text, 3); err != nil {
		t.Fatalf("LearnTempWord #2: %v", err)
	}

	words, err := s.GetTempWords(schema, code)
	if err != nil {
		t.Fatalf("GetTempWords: %v", err)
	}
	if len(words) != 1 {
		t.Fatalf("expected 1 temp word, got %d", len(words))
	}
	if words[0].Weight != 8 {
		t.Errorf("expected weight=8, got %d", words[0].Weight)
	}
	if words[0].Count != 2 {
		t.Errorf("expected count=2, got %d", words[0].Count)
	}

	// Promote to user words.
	if err := s.PromoteTempWord(schema, code, text); err != nil {
		t.Fatalf("PromoteTempWord: %v", err)
	}

	// Temp bucket should now be empty for this code.
	after, err := s.GetTempWords(schema, code)
	if err != nil {
		t.Fatalf("GetTempWords after promote: %v", err)
	}
	if len(after) != 0 {
		t.Errorf("expected 0 temp words after promote, got %d", len(after))
	}

	// User words bucket should contain the promoted entry.
	userWords, err := s.GetUserWords(schema, code)
	if err != nil {
		t.Fatalf("GetUserWords: %v", err)
	}
	if len(userWords) != 1 {
		t.Fatalf("expected 1 user word, got %d", len(userWords))
	}
	if userWords[0].Weight != 8 {
		t.Errorf("expected user word weight=8, got %d", userWords[0].Weight)
	}
}

func TestTempWords_Evict(t *testing.T) {
	s := openTestStore(t)
	schema := "wubi86"

	words := []struct {
		code   string
		text   string
		weight int
	}{
		{"a", "w1", 10},
		{"b", "w2", 50},
		{"c", "w3", 30},
		{"d", "w4", 70},
		{"e", "w5", 20},
	}
	for _, w := range words {
		if err := s.LearnTempWord(schema, w.code, w.text, w.weight); err != nil {
			t.Fatalf("LearnTempWord(%q): %v", w.text, err)
		}
	}

	count, err := s.TempWordCount(schema)
	if err != nil {
		t.Fatalf("TempWordCount: %v", err)
	}
	if count != 5 {
		t.Fatalf("expected 5 words before evict, got %d", count)
	}

	deleted, err := s.EvictTempWords(schema, 3)
	if err != nil {
		t.Fatalf("EvictTempWords: %v", err)
	}
	if deleted != 2 {
		t.Errorf("expected 2 deleted, got %d", deleted)
	}

	count, err = s.TempWordCount(schema)
	if err != nil {
		t.Fatalf("TempWordCount after evict: %v", err)
	}
	if count != 3 {
		t.Errorf("expected 3 words after evict, got %d", count)
	}

	// The 3 highest-weight words should remain: w2(50), w3(30), w4(70).
	remaining := make(map[string]bool)
	for _, code := range []string{"b", "c", "d"} {
		ws, err := s.GetTempWords(schema, code)
		if err != nil {
			t.Fatalf("GetTempWords(%q): %v", code, err)
		}
		for _, w := range ws {
			remaining[w.Text] = true
		}
	}
	for _, expect := range []string{"w2", "w3", "w4"} {
		if !remaining[expect] {
			t.Errorf("expected %q to remain after evict", expect)
		}
	}

	// Evicted words (w1, w5) should be gone.
	for _, code := range []string{"a", "e"} {
		ws, err := s.GetTempWords(schema, code)
		if err != nil {
			t.Fatalf("GetTempWords(%q): %v", code, err)
		}
		if len(ws) != 0 {
			t.Errorf("expected %q to be evicted", code)
		}
	}
}

func TestTempWords_ClearAll(t *testing.T) {
	s := openTestStore(t)
	schema := "wubi86"

	for i, pair := range [][2]string{{"a", "w1"}, {"b", "w2"}, {"c", "w3"}} {
		if err := s.LearnTempWord(schema, pair[0], pair[1], (i+1)*10); err != nil {
			t.Fatalf("LearnTempWord: %v", err)
		}
	}

	count, err := s.TempWordCount(schema)
	if err != nil {
		t.Fatalf("TempWordCount: %v", err)
	}
	if count != 3 {
		t.Fatalf("expected 3 words, got %d", count)
	}

	cleared, err := s.ClearTempWords(schema)
	if err != nil {
		t.Fatalf("ClearTempWords: %v", err)
	}
	if cleared != 3 {
		t.Errorf("expected cleared=3, got %d", cleared)
	}

	count, err = s.TempWordCount(schema)
	if err != nil {
		t.Fatalf("TempWordCount after clear: %v", err)
	}
	if count != 0 {
		t.Errorf("expected 0 words after clear, got %d", count)
	}
}
