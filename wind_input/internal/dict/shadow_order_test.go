package dict

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
)

func TestApplyShadowPins_PinToTop(t *testing.T) {
	candidates := []candidate.Candidate{
		{Text: "工", Code: "aa", Weight: 100},
		{Text: "式", Code: "aa", Weight: 90},
		{Text: "戒", Code: "aa", Weight: 80},
	}

	rules := &ShadowRules{
		Pinned: []PinnedWord{{Word: "戒", Position: 0}},
	}

	result := ApplyShadowPins(candidates, rules)
	if len(result) != 3 {
		t.Fatalf("expected 3 results, got %d", len(result))
	}
	if result[0].Text != "戒" {
		t.Errorf("pinned '戒' should be first, got %q", result[0].Text)
	}
	if result[1].Text != "工" {
		t.Errorf("'工' should be second, got %q", result[1].Text)
	}
	if result[2].Text != "式" {
		t.Errorf("'式' should be third, got %q", result[2].Text)
	}
}

func TestApplyShadowPins_MultiplePinsLIFO(t *testing.T) {
	candidates := []candidate.Candidate{
		{Text: "工", Code: "aa", Weight: 100},
		{Text: "式", Code: "aa", Weight: 90},
		{Text: "戒", Code: "aa", Weight: 80},
	}

	// 戒 is first in array (LIFO, pinned last), both at position 0
	rules := &ShadowRules{
		Pinned: []PinnedWord{
			{Word: "戒", Position: 0},
			{Word: "式", Position: 0},
		},
	}

	result := ApplyShadowPins(candidates, rules)
	// 戒 (higher LIFO priority) gets pos 0, 式 gets bumped to pos 1
	if result[0].Text != "戒" {
		t.Errorf("LIFO: '戒' should be first, got %q", result[0].Text)
	}
	if result[1].Text != "式" {
		t.Errorf("LIFO: '式' should be second, got %q", result[1].Text)
	}
	if result[2].Text != "工" {
		t.Errorf("'工' should be third, got %q", result[2].Text)
	}
}

func TestApplyShadowPins_PinToMiddle(t *testing.T) {
	candidates := []candidate.Candidate{
		{Text: "工", Code: "aa", Weight: 100},
		{Text: "式", Code: "aa", Weight: 90},
		{Text: "戒", Code: "aa", Weight: 80},
	}

	// Pin 戒 to position 1 (middle)
	rules := &ShadowRules{
		Pinned: []PinnedWord{{Word: "戒", Position: 1}},
	}

	result := ApplyShadowPins(candidates, rules)
	if result[0].Text != "工" {
		t.Errorf("'工' should stay first, got %q", result[0].Text)
	}
	if result[1].Text != "戒" {
		t.Errorf("pinned '戒' should be at position 1, got %q", result[1].Text)
	}
	if result[2].Text != "式" {
		t.Errorf("'式' should be third, got %q", result[2].Text)
	}
}

func TestApplyShadowPins_Delete(t *testing.T) {
	candidates := []candidate.Candidate{
		{Text: "工", Code: "aa", Weight: 100},
		{Text: "恭喜发财", Code: "aa", Weight: 90},
		{Text: "式", Code: "aa", Weight: 80},
	}

	rules := &ShadowRules{
		Deleted: []string{"恭喜发财"},
	}

	result := ApplyShadowPins(candidates, rules)
	if len(result) != 2 {
		t.Fatalf("expected 2 results after delete, got %d", len(result))
	}
	for _, c := range result {
		if c.Text == "恭喜发财" {
			t.Error("deleted word should not appear")
		}
	}
}

func TestApplyShadowPins_DeleteSingleCharBlocked(t *testing.T) {
	candidates := []candidate.Candidate{
		{Text: "工", Code: "aa", Weight: 100},
		{Text: "式", Code: "aa", Weight: 90},
	}

	// 尝试删除单字——应被忽略
	rules := &ShadowRules{
		Deleted: []string{"工"},
	}

	result := ApplyShadowPins(candidates, rules)
	if len(result) != 2 {
		t.Fatalf("single char delete should be blocked, got %d results", len(result))
	}
}

func TestApplyShadowPins_NilRules(t *testing.T) {
	candidates := []candidate.Candidate{
		{Text: "工", Code: "aa", Weight: 100},
	}

	result := ApplyShadowPins(candidates, nil)
	if len(result) != 1 || result[0].Text != "工" {
		t.Error("nil rules should return unchanged candidates")
	}
}

func TestApplyShadowPins_PinMissingWord(t *testing.T) {
	candidates := []candidate.Candidate{
		{Text: "工", Code: "aa", Weight: 100},
		{Text: "式", Code: "aa", Weight: 90},
	}

	// Pin a word that doesn't exist in candidates — should be skipped
	rules := &ShadowRules{
		Pinned: []PinnedWord{{Word: "不存在", Position: 0}},
	}

	result := ApplyShadowPins(candidates, rules)
	if len(result) != 2 {
		t.Fatalf("expected 2 results, got %d", len(result))
	}
	if result[0].Text != "工" {
		t.Errorf("original order should be preserved, got %q first", result[0].Text)
	}
}
