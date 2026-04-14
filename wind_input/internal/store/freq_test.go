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
			name: "count=1 recent no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 60, Streak: 0},
			// base=log2(2)*100=100, recency=200(<1h), streak=0 → 300
			minBoost: 290,
			maxBoost: 310,
		},
		{
			name: "count=1 yesterday no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 7200, Streak: 0},
			// base=100, recency=100(<1day), streak=0 → 200
			minBoost: 190,
			maxBoost: 210,
		},
		{
			name: "count=1 last week no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 2*86400, Streak: 0},
			// base=100, recency=50(<1week), streak=0 → 150
			minBoost: 140,
			maxBoost: 160,
		},
		{
			name: "count=1 old no streak",
			rec:  FreqRecord{Count: 1, LastUsed: now - 8*86400, Streak: 0},
			// base=100, recency=0, streak=0 → 100
			minBoost: 90,
			maxBoost: 110,
		},
		{
			name: "high count with streak and recent",
			rec:  FreqRecord{Count: 100, LastUsed: now - 30, Streak: 5},
			// base=log2(101)*100≈669, recency=200, streak=250 → capped at 2000
			minBoost: 1000,
			maxBoost: FreqBoostMax,
		},
		{
			name: "streak capped at 250",
			rec:  FreqRecord{Count: 1, LastUsed: now - 8*86400, Streak: 10},
			// base=100, recency=0, streak=min(500,250)=250 → 350
			minBoost: 340,
			maxBoost: 360,
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
