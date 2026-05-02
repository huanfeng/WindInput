package store_test

import (
	"os"
	"testing"

	"github.com/huanfeng/wind_input/internal/store"
)

func openTempStore(t *testing.T) (*store.Store, func()) {
	t.Helper()
	f, err := os.CreateTemp("", "test-*.db")
	if err != nil {
		t.Fatalf("create temp db: %v", err)
	}
	f.Close()
	s, err := store.Open(f.Name())
	if err != nil {
		os.Remove(f.Name())
		t.Fatalf("open store: %v", err)
	}
	return s, func() {
		s.Close()
		os.Remove(f.Name())
	}
}

func TestAllUserWordsEmpty(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	words, err := s.AllUserWords("wubi86")
	if err != nil {
		t.Fatalf("AllUserWords: %v", err)
	}
	if len(words) != 0 {
		t.Errorf("expected 0 words, got %d", len(words))
	}
}

func TestBulkUserWordsRoundTrip(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	input := []store.UserWordBulkEntry{
		{Code: "abcd", Text: "测试", Weight: 100},
		{Code: "efgh", Text: "词语", Weight: 200},
	}
	if err := s.BulkPutUserWords("wubi86", input); err != nil {
		t.Fatalf("BulkPutUserWords: %v", err)
	}
	got, err := s.AllUserWords("wubi86")
	if err != nil {
		t.Fatalf("AllUserWords: %v", err)
	}
	if len(got) != len(input) {
		t.Fatalf("expected %d words, got %d", len(input), len(got))
	}
	// 验证内容
	textSet := map[string]bool{}
	for _, w := range got {
		textSet[w.Text] = true
	}
	for _, w := range input {
		if !textSet[w.Text] {
			t.Errorf("missing word %q", w.Text)
		}
	}
}

func TestBulkStatsRoundTrip(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	input := []store.DailyStatBulkEntry{
		{Date: "2026-05-01", RawValue: []byte(`{"tc":100}`)},
		{Date: "2026-05-02", RawValue: []byte(`{"tc":200}`)},
	}
	if err := s.BulkPutStats(input); err != nil {
		t.Fatalf("BulkPutStats: %v", err)
	}
	got, err := s.AllStats()
	if err != nil {
		t.Fatalf("AllStats: %v", err)
	}
	if len(got) != 2 {
		t.Fatalf("expected 2 stats, got %d", len(got))
	}
}

func TestBulkFreqRoundTrip(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	input := []store.FreqBulkEntry{
		{Code: "wo", Text: "我", Count: 10, LastUsed: 1000, Streak: 3},
		{Code: "ni", Text: "你", Count: 5, LastUsed: 2000, Streak: 1},
	}
	if err := s.BulkPutFreq("pinyin", input); err != nil {
		t.Fatalf("BulkPutFreq: %v", err)
	}
	got, err := s.AllFreq("pinyin")
	if err != nil {
		t.Fatalf("AllFreq: %v", err)
	}
	if len(got) != len(input) {
		t.Fatalf("expected %d entries, got %d", len(input), len(got))
	}
	byCode := map[string]store.FreqBulkEntry{}
	for _, e := range got {
		byCode[e.Code+":"+e.Text] = e
	}
	for _, want := range input {
		key := want.Code + ":" + want.Text
		got, ok := byCode[key]
		if !ok {
			t.Errorf("missing entry %q", key)
			continue
		}
		if got.Count != want.Count || got.LastUsed != want.LastUsed || got.Streak != want.Streak {
			t.Errorf("entry %q: got %+v, want %+v", key, got, want)
		}
	}
}

func TestBulkShadowRoundTrip(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	input := []store.ShadowBulkEntry{
		{Code: "abc", RawValue: []byte(`{"rule":1}`)},
		{Code: "def", RawValue: []byte(`{"rule":2}`)},
	}
	if err := s.BulkPutShadow("wubi86", input); err != nil {
		t.Fatalf("BulkPutShadow: %v", err)
	}
	got, err := s.AllShadow("wubi86")
	if err != nil {
		t.Fatalf("AllShadow: %v", err)
	}
	if len(got) != len(input) {
		t.Fatalf("expected %d entries, got %d", len(input), len(got))
	}
	byCode := map[string][]byte{}
	for _, e := range got {
		byCode[e.Code] = e.RawValue
	}
	for _, want := range input {
		raw, ok := byCode[want.Code]
		if !ok {
			t.Errorf("missing code %q", want.Code)
			continue
		}
		if string(raw) != string(want.RawValue) {
			t.Errorf("code %q: got %q, want %q", want.Code, raw, want.RawValue)
		}
	}
}

func TestBulkUserWordsFieldsRoundTrip(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	input := []store.UserWordBulkEntry{
		{Code: "abcd", Text: "测试", Weight: 100, Count: 5, CreatedAt: 1234567890},
	}
	if err := s.BulkPutUserWords("wubi86", input); err != nil {
		t.Fatalf("BulkPutUserWords: %v", err)
	}
	got, err := s.AllUserWords("wubi86")
	if err != nil {
		t.Fatalf("AllUserWords: %v", err)
	}
	if len(got) != 1 {
		t.Fatalf("expected 1 word, got %d", len(got))
	}
	w := got[0]
	if w.Code != "abcd" || w.Text != "测试" || w.Weight != 100 || w.Count != 5 || w.CreatedAt != 1234567890 {
		t.Errorf("fields mismatch: got %+v", w)
	}
}
