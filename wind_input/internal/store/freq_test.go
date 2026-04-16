package store

import (
	"path/filepath"
	"testing"
	"time"
)

func openFreqTestStore(t *testing.T) *Store {
	t.Helper()
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s
}

func TestFreq_IncrementAndGet(t *testing.T) {
	s := openFreqTestStore(t)
	schemaID := "wubi86"
	code := "tttt"
	text := "的"

	// Initial state: record should be zero.
	rec, err := s.GetFreq(schemaID, code, text)
	if err != nil {
		t.Fatalf("GetFreq (initial): %v", err)
	}
	if rec.Count != 0 {
		t.Errorf("expected initial Count=0, got %d", rec.Count)
	}

	// Increment once.
	before := time.Now().Unix()
	if err := s.IncrementFreq(schemaID, code, text); err != nil {
		t.Fatalf("IncrementFreq (1): %v", err)
	}

	// Increment twice.
	if err := s.IncrementFreq(schemaID, code, text); err != nil {
		t.Fatalf("IncrementFreq (2): %v", err)
	}
	after := time.Now().Unix()

	rec, err = s.GetFreq(schemaID, code, text)
	if err != nil {
		t.Fatalf("GetFreq after increments: %v", err)
	}
	if rec.Count != 2 {
		t.Errorf("expected Count=2, got %d", rec.Count)
	}
	if rec.LastUsed < before || rec.LastUsed > after {
		t.Errorf("LastUsed %d not in [%d, %d]", rec.LastUsed, before, after)
	}
	if rec.Streak != 2 {
		t.Errorf("expected Streak=2, got %d", rec.Streak)
	}
}

func TestFreq_ResetStreak(t *testing.T) {
	s := openFreqTestStore(t)
	schemaID := "wubi86"
	code := "aaaa"
	text := "工"

	// Increment a few times to build streak.
	for i := 0; i < 3; i++ {
		if err := s.IncrementFreq(schemaID, code, text); err != nil {
			t.Fatalf("IncrementFreq: %v", err)
		}
	}

	rec, err := s.GetFreq(schemaID, code, text)
	if err != nil {
		t.Fatalf("GetFreq: %v", err)
	}
	if rec.Streak == 0 {
		t.Fatal("expected Streak>0 before reset")
	}

	// Reset streak.
	if err := s.ResetStreak(schemaID, code, text); err != nil {
		t.Fatalf("ResetStreak: %v", err)
	}

	rec, err = s.GetFreq(schemaID, code, text)
	if err != nil {
		t.Fatalf("GetFreq after reset: %v", err)
	}
	if rec.Streak != 0 {
		t.Errorf("expected Streak=0 after reset, got %d", rec.Streak)
	}
	// Count should be unchanged.
	if rec.Count != 3 {
		t.Errorf("expected Count=3 after reset, got %d", rec.Count)
	}
}

func TestFreq_CalcBoost(t *testing.T) {
	now := time.Now().Unix()

	// 使用默认 profile: BaseScale=50, MaxRecency=100, DecayHalfLife=72h, StreakScale=30, StreakCap=150
	cases := []struct {
		name     string
		rec      FreqRecord
		minBoost int
		maxBoost int
	}{
		{
			name:     "zero count returns 0",
			rec:      FreqRecord{Count: 0, LastUsed: now, Streak: 5},
			minBoost: 0,
			maxBoost: 0,
		},
		{
			name: "count=1 just now no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 60, Streak: 0},
			// base=log2(2)*50=50, recency≈100, streak=0 → ~150
			minBoost: 145,
			maxBoost: 155,
		},
		{
			name: "count=1 2h ago no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 7200, Streak: 0},
			// base=50, recency=100*exp(-ln2/72*2)≈98, streak=0 → ~148
			minBoost: 143,
			maxBoost: 153,
		},
		{
			name: "count=1 2days ago no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 2*86400, Streak: 0},
			// base=50, recency=100*exp(-ln2/72*48)≈63, streak=0 → ~113
			minBoost: 108,
			maxBoost: 118,
		},
		{
			name: "count=1 3days(half-life) ago no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 3*86400, Streak: 0},
			// base=50, recency=100*0.5=50, streak=0 → ~100
			minBoost: 95,
			maxBoost: 105,
		},
		{
			name: "count=1 7days ago no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 7*86400, Streak: 0},
			// base=50, recency=100*exp(-ln2/72*168)≈19, streak=0 → ~69
			minBoost: 64,
			maxBoost: 74,
		},
		{
			name: "count=1 14days ago no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 14*86400, Streak: 0},
			// base=50, recency=100*exp(-ln2/72*336)≈4, streak=0 → ~54
			minBoost: 49,
			maxBoost: 59,
		},
		{
			name: "high count with streak and recent",
			rec:  FreqRecord{Count: 100, LastUsed: now - 30, Streak: 5},
			// base=log2(101)*50≈334, recency≈100, streak=150 → ~584
			minBoost: 575,
			maxBoost: 595,
		},
		{
			name: "streak capped at 150",
			rec:  FreqRecord{Count: 1, LastUsed: now - 14*86400, Streak: 10},
			// base=50, recency≈4, streak=min(300,150)=150 → ~204
			minBoost: 195,
			maxBoost: 210,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			boost := CalcFreqBoost(tc.rec, now)
			if boost < tc.minBoost || boost > tc.maxBoost {
				t.Errorf("CalcFreqBoost=%d, want in [%d, %d]", boost, tc.minBoost, tc.maxBoost)
			}
		})
	}
}

func TestFreq_CalcBoostWithProfile(t *testing.T) {
	now := time.Now().Unix()

	// 自定义 profile: 短半衰期 24h，小上限
	p := &FreqProfile{
		BoostMax:      1000,
		BaseScale:     50,
		MaxRecency:    200,
		DecayHalfLife: 24,
		StreakScale:   30,
		StreakCap:     150,
	}

	t.Run("custom profile basic", func(t *testing.T) {
		rec := FreqRecord{Count: 1, LastUsed: now - 60, Streak: 0}
		// base=log2(2)*50=50, recency≈200, streak=0 → ~250
		boost := CalcFreqBoostWithProfile(rec, now, p)
		if boost < 240 || boost > 260 {
			t.Errorf("boost=%d, want in [240, 260]", boost)
		}
	})

	t.Run("custom profile half-life decay", func(t *testing.T) {
		rec := FreqRecord{Count: 1, LastUsed: now - 24*3600, Streak: 0}
		// base=50, recency=200*0.5=100, streak=0 → ~150
		boost := CalcFreqBoostWithProfile(rec, now, p)
		if boost < 140 || boost > 160 {
			t.Errorf("boost=%d, want in [140, 160]", boost)
		}
	})

	t.Run("custom profile capped", func(t *testing.T) {
		rec := FreqRecord{Count: 1000, LastUsed: now, Streak: 10}
		boost := CalcFreqBoostWithProfile(rec, now, p)
		if boost > p.BoostMax {
			t.Errorf("boost=%d exceeds BoostMax=%d", boost, p.BoostMax)
		}
	})

	t.Run("nil profile uses default", func(t *testing.T) {
		rec := FreqRecord{Count: 1, LastUsed: now, Streak: 0}
		boost := CalcFreqBoostWithProfile(rec, now, nil)
		boostDefault := CalcFreqBoost(rec, now)
		if boost != boostDefault {
			t.Errorf("nil profile: boost=%d != default=%d", boost, boostDefault)
		}
	})
}

func TestFreq_BoostMax(t *testing.T) {
	now := time.Now().Unix()

	// Extreme values should never exceed FreqBoostMax.
	rec := FreqRecord{
		Count:    ^uint32(0), // max uint32
		LastUsed: now,
		Streak:   255,
	}
	boost := CalcFreqBoost(rec, now)
	if boost > FreqBoostMax {
		t.Errorf("boost %d exceeds FreqBoostMax %d", boost, FreqBoostMax)
	}
	if boost <= 0 {
		t.Errorf("boost should be positive for extreme values, got %d", boost)
	}
}

func TestFreq_SearchPrefix(t *testing.T) {
	s := openFreqTestStore(t)
	schemaID := "wubi86"

	// Insert 3 entries with different code prefixes.
	entries := [][2]string{
		{"abc", "你"},
		{"abd", "好"},
		{"xyz", "世"},
	}
	for _, e := range entries {
		if err := s.IncrementFreq(schemaID, e[0], e[1]); err != nil {
			t.Fatalf("IncrementFreq %v: %v", e, err)
		}
	}

	// Search with prefix "ab" → should return 2 entries.
	results, err := s.SearchFreqPrefix(schemaID, "ab", 0)
	if err != nil {
		t.Fatalf("SearchFreqPrefix(ab, 0): %v", err)
	}
	if len(results) != 2 {
		t.Errorf("expected 2 results for prefix 'ab', got %d", len(results))
	}

	// Search with empty prefix → should return all 3.
	results, err = s.SearchFreqPrefix(schemaID, "", 0)
	if err != nil {
		t.Fatalf("SearchFreqPrefix('', 0): %v", err)
	}
	if len(results) != 3 {
		t.Errorf("expected 3 results for empty prefix, got %d", len(results))
	}

	// Search with prefix "ab" and limit 1 → should return 1.
	results, err = s.SearchFreqPrefix(schemaID, "ab", 1)
	if err != nil {
		t.Fatalf("SearchFreqPrefix(ab, 1): %v", err)
	}
	if len(results) != 1 {
		t.Errorf("expected 1 result for prefix 'ab' with limit 1, got %d", len(results))
	}

	// Verify FreqEntry fields are correctly parsed.
	for _, r := range results {
		if r.Code == "" || r.Text == "" {
			t.Errorf("FreqEntry has empty Code or Text: %+v", r)
		}
		if r.Record.Count != 1 {
			t.Errorf("expected Count=1, got %d", r.Record.Count)
		}
	}

	// Search non-existent schema → should return nil.
	results, err = s.SearchFreqPrefix("nonexistent", "ab", 0)
	if err != nil {
		t.Fatalf("SearchFreqPrefix nonexistent schema: %v", err)
	}
	if results != nil {
		t.Errorf("expected nil for nonexistent schema, got %v", results)
	}
}

func TestFreq_Delete(t *testing.T) {
	s := openFreqTestStore(t)
	schemaID := "wubi86"
	code := "tttt"
	text := "的"

	// Increment then delete.
	if err := s.IncrementFreq(schemaID, code, text); err != nil {
		t.Fatalf("IncrementFreq: %v", err)
	}

	if err := s.DeleteFreq(schemaID, code, text); err != nil {
		t.Fatalf("DeleteFreq: %v", err)
	}

	// Verify record is gone (zero record).
	rec, err := s.GetFreq(schemaID, code, text)
	if err != nil {
		t.Fatalf("GetFreq after delete: %v", err)
	}
	if rec.Count != 0 {
		t.Errorf("expected Count=0 after delete, got %d", rec.Count)
	}

	// Delete on nonexistent schema should be a no-op.
	if err := s.DeleteFreq("nonexistent", code, text); err != nil {
		t.Errorf("DeleteFreq nonexistent schema should be no-op, got %v", err)
	}
}

func TestFreq_ClearAll(t *testing.T) {
	s := openFreqTestStore(t)
	schemaID := "wubi86"

	pairs := [][2]string{
		{"tttt", "的"},
		{"aaaa", "工"},
		{"ssss", "木"},
	}
	for _, p := range pairs {
		if err := s.IncrementFreq(schemaID, p[0], p[1]); err != nil {
			t.Fatalf("IncrementFreq %v: %v", p, err)
		}
	}

	count, err := s.ClearAllFreq(schemaID)
	if err != nil {
		t.Fatalf("ClearAllFreq: %v", err)
	}
	if count != len(pairs) {
		t.Errorf("expected count=%d, got %d", len(pairs), count)
	}

	// Verify all entries are gone.
	all, err := s.GetAllFreq(schemaID)
	if err != nil {
		t.Fatalf("GetAllFreq after clear: %v", err)
	}
	if len(all) != 0 {
		t.Errorf("expected empty map after ClearAllFreq, got %d entries", len(all))
	}
}

func TestFreq_GetAllFreq(t *testing.T) {
	s := openFreqTestStore(t)
	schemaID := "wubi86"

	pairs := [][2]string{
		{"tttt", "的"},
		{"aaaa", "工"},
		{"ssss", "木"},
	}

	for _, p := range pairs {
		if err := s.IncrementFreq(schemaID, p[0], p[1]); err != nil {
			t.Fatalf("IncrementFreq %v: %v", p, err)
		}
	}

	all, err := s.GetAllFreq(schemaID)
	if err != nil {
		t.Fatalf("GetAllFreq: %v", err)
	}
	if len(all) != len(pairs) {
		t.Errorf("expected %d records, got %d", len(pairs), len(all))
	}
	for _, p := range pairs {
		key := freqKey(p[0], p[1])
		rec, ok := all[key]
		if !ok {
			t.Errorf("missing key %q in GetAllFreq result", key)
			continue
		}
		if rec.Count != 1 {
			t.Errorf("key %q: expected Count=1, got %d", key, rec.Count)
		}
	}
}
