package pinyin

import (
	"testing"
)

func TestCompositionBuilder(t *testing.T) {
	builder := NewCompositionBuilder()
	parser := NewPinyinParser()

	tests := []struct {
		input           string
		wantPreedit     string
		wantHasPartial  bool
		wantCursorAtEnd bool
	}{
		{
			input:           "nihao",
			wantPreedit:     "ni hao",
			wantHasPartial:  false,
			wantCursorAtEnd: true,
		},
		{
			input:           "nihaozh",
			wantPreedit:     "ni hao zh",
			wantHasPartial:  true,
			wantCursorAtEnd: true,
		},
		{
			input:           "zh",
			wantPreedit:     "zh",
			wantHasPartial:  true,
			wantCursorAtEnd: true,
		},
		{
			input:           "zhongguo",
			wantPreedit:     "zhong guo",
			wantHasPartial:  false,
			wantCursorAtEnd: true,
		},
		{
			input:           "wo",
			wantPreedit:     "wo",
			wantHasPartial:  false,
			wantCursorAtEnd: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			parsed := parser.Parse(tt.input)
			comp := builder.Build(parsed)

			if comp.PreeditText != tt.wantPreedit {
				t.Errorf("Build(%q) PreeditText = %q, want %q", tt.input, comp.PreeditText, tt.wantPreedit)
			}

			if comp.HasPartial() != tt.wantHasPartial {
				t.Errorf("Build(%q) HasPartial() = %v, want %v", tt.input, comp.HasPartial(), tt.wantHasPartial)
			}

			if tt.wantCursorAtEnd && comp.PreeditCursor != len(comp.PreeditText) {
				t.Errorf("Build(%q) PreeditCursor = %d, want %d (at end)", tt.input, comp.PreeditCursor, len(comp.PreeditText))
			}
		})
	}
}

func TestCompositionStateHighlights(t *testing.T) {
	builder := NewCompositionBuilder()
	parser := NewPinyinParser()

	// 测试 "nihaozh" 的高亮区域
	parsed := parser.Parse("nihaozh")
	comp := builder.Build(parsed)

	// 期望: ni hao zh（自动切分用空格）
	// 高亮区域:
	// [0,2) = "ni" (Completed)
	// [2,3) = " " (Separator)
	// [3,6) = "hao" (Completed)
	// [6,7) = " " (Separator)
	// [7,9) = "zh" (Partial)

	expectedHighlights := []struct {
		start     int
		end       int
		highlight HighlightType
	}{
		{0, 2, HighlightCompleted}, // ni
		{2, 3, HighlightSeparator}, // space
		{3, 6, HighlightCompleted}, // hao
		{6, 7, HighlightSeparator}, // space
		{7, 9, HighlightPartial},   // zh
	}

	if len(comp.Highlights) != len(expectedHighlights) {
		t.Fatalf("Highlights count = %d, want %d", len(comp.Highlights), len(expectedHighlights))
	}

	for i, want := range expectedHighlights {
		got := comp.Highlights[i]
		if got.Start != want.start || got.End != want.end || got.Type != want.highlight {
			t.Errorf("Highlights[%d] = {%d,%d,%v}, want {%d,%d,%v}",
				i, got.Start, got.End, got.Type.String(),
				want.start, want.end, want.highlight.String())
		}
	}
}

func TestCompositionBuildFromSyllables(t *testing.T) {
	builder := NewCompositionBuilder()

	comp := builder.BuildFromSyllables(
		[]string{"ni", "hao"},
		"zh",
		[]string{"a", "ai", "an"},
	)

	if comp.PreeditText != "ni hao zh" {
		t.Errorf("PreeditText = %q, want %q", comp.PreeditText, "ni hao zh")
	}

	if len(comp.CompletedSyllables) != 2 {
		t.Errorf("CompletedSyllables count = %d, want 2", len(comp.CompletedSyllables))
	}

	if comp.PartialSyllable != "zh" {
		t.Errorf("PartialSyllable = %q, want %q", comp.PartialSyllable, "zh")
	}

	if len(comp.PossibleContinues) != 3 {
		t.Errorf("PossibleContinues count = %d, want 3", len(comp.PossibleContinues))
	}
}

func TestCompositionStateAllSyllables(t *testing.T) {
	builder := NewCompositionBuilder()

	comp := builder.BuildFromSyllables(
		[]string{"ni", "hao"},
		"zh",
		nil,
	)

	all := comp.AllSyllables()
	if len(all) != 3 {
		t.Fatalf("AllSyllables() count = %d, want 3", len(all))
	}

	expected := []string{"ni", "hao", "zh"}
	for i, want := range expected {
		if all[i] != want {
			t.Errorf("AllSyllables()[%d] = %q, want %q", i, all[i], want)
		}
	}
}

func TestCompositionStateTotalSyllableCount(t *testing.T) {
	builder := NewCompositionBuilder()

	tests := []struct {
		completed []string
		partial   string
		want      int
	}{
		{[]string{"ni", "hao"}, "zh", 3},
		{[]string{"ni", "hao"}, "", 2},
		{[]string{}, "zh", 1},
		{[]string{}, "", 0},
	}

	for _, tt := range tests {
		comp := builder.BuildFromSyllables(tt.completed, tt.partial, nil)
		if got := comp.TotalSyllableCount(); got != tt.want {
			t.Errorf("TotalSyllableCount() with completed=%v partial=%q = %d, want %d",
				tt.completed, tt.partial, got, tt.want)
		}
	}
}

func TestCompositionStateIsEmpty(t *testing.T) {
	builder := NewCompositionBuilder()

	empty := builder.BuildFromSyllables([]string{}, "", nil)
	if !empty.IsEmpty() {
		t.Error("Empty composition should return IsEmpty() = true")
	}

	notEmpty := builder.BuildFromSyllables([]string{"ni"}, "", nil)
	if notEmpty.IsEmpty() {
		t.Error("Non-empty composition should return IsEmpty() = false")
	}

	partialOnly := builder.BuildFromSyllables([]string{}, "zh", nil)
	if partialOnly.IsEmpty() {
		t.Error("Composition with partial should return IsEmpty() = false")
	}
}

func TestCompositionBuilderSetSeparator(t *testing.T) {
	builder := NewCompositionBuilder().SetSeparator(" ")
	parser := NewPinyinParser()

	parsed := parser.Parse("nihao")
	comp := builder.Build(parsed)

	if comp.PreeditText != "ni hao" {
		t.Errorf("PreeditText with space separator = %q, want %q", comp.PreeditText, "ni hao")
	}
}

func TestCompositionWithExplicitSeparator(t *testing.T) {
	p := NewPinyinParser()
	builder := NewCompositionBuilder()

	tests := []struct {
		input    string
		expected string
	}{
		{"xi'an", "xi'an"},
		{"xi'an'ren", "xi'an'ren"},
		{"chang'an", "chang'an"},
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			result := p.Parse(tt.input)
			comp := builder.Build(result)
			if comp.PreeditText != tt.expected {
				t.Errorf("input=%q: expected preedit %q, got %q", tt.input, tt.expected, comp.PreeditText)
			}
		})
	}
}
