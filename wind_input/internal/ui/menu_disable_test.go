package ui

import (
	"testing"
)

// computeMenuDisableState mirrors the logic in handleRightClick for testability.
// Returns (isGlobalFirst, isGlobalLast).
func computeMenuDisableState(pageStartIndex, hitIndex, totalCandidateCount int) (bool, bool) {
	globalIndex := pageStartIndex + hitIndex
	isGlobalFirst := globalIndex == 0
	isGlobalLast := totalCandidateCount <= 0 || globalIndex >= totalCandidateCount-1
	return isGlobalFirst, isGlobalLast
}

func TestMenuDisable_SinglePage(t *testing.T) {
	// 7 candidates on a single page (page 1, candidatesPerPage=7)
	total := 7
	pageStart := 0

	tests := []struct {
		name      string
		hitIndex  int
		wantFirst bool
		wantLast  bool
	}{
		{"first item", 0, true, false},
		{"middle item", 3, false, false},
		{"last item", 6, false, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			isFirst, isLast := computeMenuDisableState(pageStart, tt.hitIndex, total)
			if isFirst != tt.wantFirst {
				t.Errorf("isGlobalFirst = %v, want %v", isFirst, tt.wantFirst)
			}
			if isLast != tt.wantLast {
				t.Errorf("isGlobalLast = %v, want %v", isLast, tt.wantLast)
			}
		})
	}
}

func TestMenuDisable_MultiPage(t *testing.T) {
	// 20 candidates, 7 per page → 3 pages
	total := 20
	perPage := 7

	tests := []struct {
		name      string
		page      int // 1-based
		hitIndex  int // 0-based within page
		wantFirst bool
		wantLast  bool
	}{
		// Page 1: globalIndex 0-6
		{"page1 first", 1, 0, true, false},
		{"page1 middle", 1, 3, false, false},
		{"page1 last", 1, 6, false, false}, // NOT global last

		// Page 2: globalIndex 7-13
		{"page2 first", 2, 0, false, false}, // NOT global first
		{"page2 middle", 2, 3, false, false},
		{"page2 last", 2, 6, false, false}, // NOT global last

		// Page 3: globalIndex 14-19 (6 candidates on last page)
		{"page3 first", 3, 0, false, false},
		{"page3 last", 3, 5, false, true}, // global last (index 19)
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pageStart := (tt.page - 1) * perPage
			isFirst, isLast := computeMenuDisableState(pageStart, tt.hitIndex, total)
			if isFirst != tt.wantFirst {
				t.Errorf("isGlobalFirst = %v, want %v (globalIndex=%d)", isFirst, tt.wantFirst, pageStart+tt.hitIndex)
			}
			if isLast != tt.wantLast {
				t.Errorf("isGlobalLast = %v, want %v (globalIndex=%d, total=%d)", isLast, tt.wantLast, pageStart+tt.hitIndex, total)
			}
		})
	}
}

func TestMenuDisable_ZeroTotal(t *testing.T) {
	// Edge case: totalCandidateCount == 0 (uninitialized)
	// All items should have both move-up and move-down disabled
	isFirst, isLast := computeMenuDisableState(0, 0, 0)
	if !isFirst {
		t.Error("expected isGlobalFirst=true when total=0")
	}
	if !isLast {
		t.Error("expected isGlobalLast=true when total=0")
	}
}

func TestMenuDisable_SingleCandidate(t *testing.T) {
	// Only 1 candidate total
	isFirst, isLast := computeMenuDisableState(0, 0, 1)
	if !isFirst {
		t.Error("expected isGlobalFirst=true for single candidate")
	}
	if !isLast {
		t.Error("expected isGlobalLast=true for single candidate")
	}
}
