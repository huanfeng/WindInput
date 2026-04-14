package store

import (
	"testing"
)

func TestUserWords_AddAndGet(t *testing.T) {
	s := openTestStore(t)

	if err := s.AddUserWord("wubi", "aaaa", "工", 100); err != nil {
		t.Fatalf("AddUserWord: %v", err)
	}

	words, err := s.GetUserWords("wubi", "aaaa")
	if err != nil {
		t.Fatalf("GetUserWords: %v", err)
	}
	if len(words) != 1 {
		t.Fatalf("expected 1 word, got %d", len(words))
	}
	w := words[0]
	if w.Text != "工" {
		t.Errorf("Text: got %q, want %q", w.Text, "工")
	}
	if w.Weight != 100 {
		t.Errorf("Weight: got %d, want 100", w.Weight)
	}
	if w.CreatedAt == 0 {
		t.Error("CreatedAt should be non-zero")
	}
}

func TestUserWords_AddDuplicate(t *testing.T) {
	s := openTestStore(t)

	if err := s.AddUserWord("wubi", "aaaa", "工", 100); err != nil {
		t.Fatalf("first AddUserWord: %v", err)
	}
	// Add same code+text with higher weight — weight should be updated.
	if err := s.AddUserWord("wubi", "aaaa", "工", 200); err != nil {
		t.Fatalf("second AddUserWord: %v", err)
	}

	words, err := s.GetUserWords("wubi", "aaaa")
	if err != nil {
		t.Fatalf("GetUserWords: %v", err)
	}
	if len(words) != 1 {
		t.Fatalf("expected 1 word after dedup, got %d", len(words))
	}
	if words[0].Weight != 200 {
		t.Errorf("Weight: got %d, want 200", words[0].Weight)
	}

	// Add same code+text with lower weight — weight should remain 200.
	if err := s.AddUserWord("wubi", "aaaa", "工", 50); err != nil {
		t.Fatalf("third AddUserWord: %v", err)
	}
	words, err = s.GetUserWords("wubi", "aaaa")
	if err != nil {
		t.Fatalf("GetUserWords after lower weight: %v", err)
	}
	if words[0].Weight != 200 {
		t.Errorf("Weight should remain 200, got %d", words[0].Weight)
	}
}

func TestUserWords_Remove(t *testing.T) {
	s := openTestStore(t)

	if err := s.AddUserWord("wubi", "aaaa", "工", 100); err != nil {
		t.Fatalf("AddUserWord 工: %v", err)
	}
	if err := s.AddUserWord("wubi", "aaaa", "王", 90); err != nil {
		t.Fatalf("AddUserWord 王: %v", err)
	}

	if err := s.RemoveUserWord("wubi", "aaaa", "工"); err != nil {
		t.Fatalf("RemoveUserWord: %v", err)
	}

	words, err := s.GetUserWords("wubi", "aaaa")
	if err != nil {
		t.Fatalf("GetUserWords: %v", err)
	}
	if len(words) != 1 {
		t.Fatalf("expected 1 word after remove, got %d", len(words))
	}
	if words[0].Text != "王" {
		t.Errorf("remaining word: got %q, want %q", words[0].Text, "王")
	}
}

func TestUserWords_PrefixSearch(t *testing.T) {
	s := openTestStore(t)

	entries := []struct{ code, text string }{
		{"abc", "甲"},
		{"abcd", "乙"},
		{"abce", "丙"},
		{"abd", "丁"},
		{"xyz", "戊"},
	}
	for _, e := range entries {
		if err := s.AddUserWord("wubi", e.code, e.text, 100); err != nil {
			t.Fatalf("AddUserWord %v: %v", e, err)
		}
	}

	// Prefix "abc" should match keys starting with "abc\x00": "abc\x00甲", "abcd\x00乙", "abce\x00丙" — 3 entries.
	results, err := s.SearchUserWordsPrefix("wubi", "abc", 0)
	if err != nil {
		t.Fatalf("SearchUserWordsPrefix abc: %v", err)
	}
	if len(results) != 3 {
		t.Errorf("expected 3 results for prefix 'abc', got %d", len(results))
	}

	// Prefix "ab" should match all four non-xyz entries.
	results, err = s.SearchUserWordsPrefix("wubi", "ab", 0)
	if err != nil {
		t.Fatalf("SearchUserWordsPrefix ab: %v", err)
	}
	if len(results) != 4 {
		t.Errorf("expected 4 results for prefix 'ab', got %d", len(results))
	}

	// Limit should be respected.
	results, err = s.SearchUserWordsPrefix("wubi", "ab", 2)
	if err != nil {
		t.Fatalf("SearchUserWordsPrefix ab limit: %v", err)
	}
	if len(results) != 2 {
		t.Errorf("expected 2 results (limit), got %d", len(results))
	}

	// Prefix "xyz" should match exactly 1.
	results, err = s.SearchUserWordsPrefix("wubi", "xyz", 0)
	if err != nil {
		t.Fatalf("SearchUserWordsPrefix xyz: %v", err)
	}
	if len(results) != 1 {
		t.Errorf("expected 1 result for prefix 'xyz', got %d", len(results))
	}
}

func TestUserWords_MultipleSchemas(t *testing.T) {
	s := openTestStore(t)

	if err := s.AddUserWord("wubi", "aaaa", "工", 100); err != nil {
		t.Fatalf("AddUserWord wubi: %v", err)
	}
	if err := s.AddUserWord("pinyin", "gong", "工", 80); err != nil {
		t.Fatalf("AddUserWord pinyin: %v", err)
	}

	wubiWords, err := s.GetUserWords("wubi", "aaaa")
	if err != nil {
		t.Fatalf("GetUserWords wubi: %v", err)
	}
	if len(wubiWords) != 1 {
		t.Errorf("wubi: expected 1 word, got %d", len(wubiWords))
	}

	// pinyin schema should not see wubi's code.
	pinyinWords, err := s.GetUserWords("pinyin", "aaaa")
	if err != nil {
		t.Fatalf("GetUserWords pinyin/aaaa: %v", err)
	}
	if len(pinyinWords) != 0 {
		t.Errorf("pinyin should have no words under 'aaaa', got %d", len(pinyinWords))
	}

	pinyinWords, err = s.GetUserWords("pinyin", "gong")
	if err != nil {
		t.Fatalf("GetUserWords pinyin/gong: %v", err)
	}
	if len(pinyinWords) != 1 {
		t.Errorf("pinyin: expected 1 word under 'gong', got %d", len(pinyinWords))
	}
}

func TestUserWords_EntryCount(t *testing.T) {
	s := openTestStore(t)

	count, err := s.UserWordCount("wubi")
	if err != nil {
		t.Fatalf("UserWordCount (empty): %v", err)
	}
	if count != 0 {
		t.Errorf("expected 0, got %d", count)
	}

	words := []struct{ code, text string }{
		{"aaaa", "工"},
		{"aaaa", "王"},
		{"aaa", "三"},
	}
	for _, w := range words {
		if err := s.AddUserWord("wubi", w.code, w.text, 100); err != nil {
			t.Fatalf("AddUserWord: %v", err)
		}
	}

	count, err = s.UserWordCount("wubi")
	if err != nil {
		t.Fatalf("UserWordCount: %v", err)
	}
	if count != 3 {
		t.Errorf("expected 3, got %d", count)
	}
}
