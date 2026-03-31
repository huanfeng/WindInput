package encoding

import (
	"testing"
)

func TestParseFormula(t *testing.T) {
	steps, err := ParseFormula("AaAbBaBb")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(steps) != 4 {
		t.Fatalf("expected 4 steps, got %d", len(steps))
	}
	expected := []FormulaStep{
		{CharIndex: 0, CodeIndex: 0},
		{CharIndex: 0, CodeIndex: 1},
		{CharIndex: 1, CodeIndex: 0},
		{CharIndex: 1, CodeIndex: 1},
	}
	for i, s := range steps {
		if s != expected[i] {
			t.Errorf("step[%d]: got %+v, want %+v", i, s, expected[i])
		}
	}
}

func TestParseFormula_LastChar(t *testing.T) {
	steps, err := ParseFormula("AaBaCaZa")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(steps) != 4 {
		t.Fatalf("expected 4 steps, got %d", len(steps))
	}
	if steps[3].CharIndex != -1 || steps[3].CodeIndex != 0 {
		t.Errorf("step[3]: got %+v, want {CharIndex:-1, CodeIndex:0}", steps[3])
	}
}

func TestParseFormula_InvalidLength(t *testing.T) {
	_, err := ParseFormula("AaB")
	if err == nil {
		t.Fatal("expected error for odd-length formula, got nil")
	}
}

func TestCalcWordCode_TwoChars(t *testing.T) {
	charCodes := map[string]string{
		"中": "khkg",
		"国": "lgyi",
	}
	rules := []Rule{
		{LengthEqual: 2, Formula: "AaAbBaBb"},
	}
	code, err := CalcWordCode("中国", charCodes, rules)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if code != "khlg" {
		t.Errorf("expected 'khlg', got '%s'", code)
	}
}

func TestCalcWordCode_ThreeChars(t *testing.T) {
	charCodes := map[string]string{
		"中": "khkg",
		"华": "wxfj",
		"人": "wwww",
	}
	rules := []Rule{
		{LengthEqual: 3, Formula: "AaBaCaCb"},
	}
	code, err := CalcWordCode("中华人", charCodes, rules)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if code != "kwww" {
		t.Errorf("expected 'kwww', got '%s'", code)
	}
}

func TestCalcWordCode_FourChars(t *testing.T) {
	charCodes := map[string]string{
		"中": "khkg",
		"华": "wxfj",
		"人": "wwww",
		"民": "naen",
	}
	rules := []Rule{
		{LengthRange: [2]int{4, 10}, Formula: "AaBaCaZa"},
	}
	code, err := CalcWordCode("中华人民", charCodes, rules)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if code != "kwwn" {
		t.Errorf("expected 'kwwn', got '%s'", code)
	}
}

func TestCalcWordCode_FiveChars(t *testing.T) {
	charCodes := map[string]string{
		"中": "khkg",
		"华": "wxfj",
		"人": "wwww",
		"民": "naen",
		"国": "lgyi",
	}
	rules := []Rule{
		{LengthRange: [2]int{4, 10}, Formula: "AaBaCaZa"},
	}
	code, err := CalcWordCode("中华人民国", charCodes, rules)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if code != "kwwl" {
		t.Errorf("expected 'kwwl', got '%s'", code)
	}
}

func TestCalcWordCode_NoMatchingRule(t *testing.T) {
	charCodes := map[string]string{
		"中": "khkg",
		"国": "lgyi",
	}
	rules := []Rule{
		{LengthEqual: 3, Formula: "AaBaCaCb"},
	}
	_, err := CalcWordCode("中国", charCodes, rules)
	if err == nil {
		t.Fatal("expected error for no matching rule, got nil")
	}
}

func TestCalcWordCode_MissingCharCode(t *testing.T) {
	charCodes := map[string]string{
		"中": "khkg",
		// "国" is missing
	}
	rules := []Rule{
		{LengthEqual: 2, Formula: "AaAbBaBb"},
	}
	_, err := CalcWordCode("中国", charCodes, rules)
	if err == nil {
		t.Fatal("expected error for missing char code, got nil")
	}
}

func TestCalcWordCode_ShortCharCode(t *testing.T) {
	charCodes := map[string]string{
		"中": "khkg",
		"国": "l", // only 1 char, but need index 1
	}
	rules := []Rule{
		{LengthEqual: 2, Formula: "AaAbBaBb"},
	}
	_, err := CalcWordCode("中国", charCodes, rules)
	if err == nil {
		t.Fatal("expected error for short char code, got nil")
	}
}

func TestMatchRule(t *testing.T) {
	rules := []Rule{
		{LengthEqual: 2, Formula: "AaAbBaBb"},
		{LengthEqual: 3, Formula: "AaBaCaCb"},
		{LengthRange: [2]int{4, 10}, Formula: "AaBaCaZa"},
	}

	// 精确匹配 2
	r := MatchRule(rules, 2)
	if r == nil || r.Formula != "AaAbBaBb" {
		t.Errorf("expected match for length 2")
	}

	// 精确匹配 3
	r = MatchRule(rules, 3)
	if r == nil || r.Formula != "AaBaCaCb" {
		t.Errorf("expected match for length 3")
	}

	// 范围匹配 5
	r = MatchRule(rules, 5)
	if r == nil || r.Formula != "AaBaCaZa" {
		t.Errorf("expected range match for length 5")
	}

	// 无匹配 1
	r = MatchRule(rules, 1)
	if r != nil {
		t.Errorf("expected no match for length 1, got %+v", r)
	}

	// 无匹配 11
	r = MatchRule(rules, 11)
	if r != nil {
		t.Errorf("expected no match for length 11, got %+v", r)
	}
}
