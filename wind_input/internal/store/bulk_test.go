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

func TestBulkTempWordsRoundTrip(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	input := []store.UserWordBulkEntry{
		{Code: "tmp1", Text: "临时词", Weight: 50, Count: 2, CreatedAt: 9999},
		{Code: "tmp2", Text: "暂存词", Weight: 10, Count: 1, CreatedAt: 8888},
	}
	if err := s.BulkPutTempWords("wubi86", input); err != nil {
		t.Fatalf("BulkPutTempWords: %v", err)
	}
	got, err := s.AllTempWords("wubi86")
	if err != nil {
		t.Fatalf("AllTempWords: %v", err)
	}
	if len(got) != len(input) {
		t.Fatalf("expected %d temp words, got %d", len(input), len(got))
	}
	byCode := map[string]store.UserWordBulkEntry{}
	for _, e := range got {
		byCode[e.Code] = e
	}
	for _, want := range input {
		e, ok := byCode[want.Code]
		if !ok {
			t.Errorf("missing code %q", want.Code)
			continue
		}
		if e.Text != want.Text || e.Weight != want.Weight || e.Count != want.Count || e.CreatedAt != want.CreatedAt {
			t.Errorf("code %q: got %+v, want %+v", want.Code, e, want)
		}
	}
}

func TestBulkTempWordsIsolatedFromUserWords(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	if err := s.BulkPutUserWords("wubi86", []store.UserWordBulkEntry{
		{Code: "abcd", Text: "用户词"},
	}); err != nil {
		t.Fatalf("BulkPutUserWords: %v", err)
	}
	if err := s.BulkPutTempWords("wubi86", []store.UserWordBulkEntry{
		{Code: "abcd", Text: "临时词"},
	}); err != nil {
		t.Fatalf("BulkPutTempWords: %v", err)
	}

	userWords, _ := s.AllUserWords("wubi86")
	tempWords, _ := s.AllTempWords("wubi86")
	if len(userWords) != 1 || userWords[0].Text != "用户词" {
		t.Errorf("UserWords corrupted: %+v", userWords)
	}
	if len(tempWords) != 1 || tempWords[0].Text != "临时词" {
		t.Errorf("TempWords corrupted: %+v", tempWords)
	}
}

func TestBulkGlobalPhrasesRoundTrip(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	// RawKey 含 \x00 分隔符（phrase key 格式：code\x00text）
	input := []store.PhraseBulkEntry{
		{
			Code:     "nide",
			RawKey:   []byte("nide\x00你的"),
			RawValue: []byte(`{"text":"你的","type":"static","pos":0,"on":true}`),
		},
		{
			Code:     "wode",
			RawKey:   []byte("wode\x00我的"),
			RawValue: []byte(`{"text":"我的","type":"static","pos":1,"on":true}`),
		},
	}
	if err := s.BulkPutGlobalPhrases(input); err != nil {
		t.Fatalf("BulkPutGlobalPhrases: %v", err)
	}
	got, err := s.AllGlobalPhrases()
	if err != nil {
		t.Fatalf("AllGlobalPhrases: %v", err)
	}
	if len(got) != len(input) {
		t.Fatalf("expected %d phrases, got %d", len(input), len(got))
	}
	byCode := map[string]store.PhraseBulkEntry{}
	for _, e := range got {
		byCode[e.Code] = e
	}
	for _, want := range input {
		e, ok := byCode[want.Code]
		if !ok {
			t.Errorf("missing code %q", want.Code)
			continue
		}
		if string(e.RawKey) != string(want.RawKey) {
			t.Errorf("code %q RawKey: got %q, want %q", want.Code, e.RawKey, want.RawKey)
		}
		if string(e.RawValue) != string(want.RawValue) {
			t.Errorf("code %q RawValue: got %q, want %q", want.Code, e.RawValue, want.RawValue)
		}
	}
}

func TestBulkMultiSchemaIsolation(t *testing.T) {
	s, cleanup := openTempStore(t)
	defer cleanup()

	if err := s.BulkPutUserWords("schema_a", []store.UserWordBulkEntry{
		{Code: "aa", Text: "词A"},
	}); err != nil {
		t.Fatalf("schema_a put: %v", err)
	}
	if err := s.BulkPutUserWords("schema_b", []store.UserWordBulkEntry{
		{Code: "bb", Text: "词B"},
		{Code: "bc", Text: "词C"},
	}); err != nil {
		t.Fatalf("schema_b put: %v", err)
	}

	wordsA, _ := s.AllUserWords("schema_a")
	wordsB, _ := s.AllUserWords("schema_b")
	if len(wordsA) != 1 {
		t.Errorf("schema_a: expected 1 word, got %d", len(wordsA))
	}
	if len(wordsB) != 2 {
		t.Errorf("schema_b: expected 2 words, got %d", len(wordsB))
	}
	wordsC, _ := s.AllUserWords("schema_c")
	if len(wordsC) != 0 {
		t.Errorf("schema_c: expected 0 words, got %d", len(wordsC))
	}
}
