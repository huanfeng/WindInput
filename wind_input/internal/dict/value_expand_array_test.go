package dict

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

// TestValueExpander_ExpandToCandidates 验证 ExpandToCandidates 各分支行为。
func TestValueExpander_ExpandToCandidates(t *testing.T) {
	// 构造带 stub Hook 和 stub ArrayHook 的 ValueExpander
	stubHook := CmdbarPhraseHook(func(value string) (string, []cmdbar.ResolvedAction, map[string]any, bool, error) {
		// 只处理 $CC("打开","open(...)")
		return "打开", []cmdbar.ResolvedAction{
			{Kind: cmdbar.ActionEffect, Run: func() (string, error) { return "", nil }},
		}, nil, true, nil
	})

	stubArrayHook := CmdbarArrayHook(func(value string) (string, []CmdbarArrayElement, map[string]any, bool, error) {
		return "数组组", []CmdbarArrayElement{
			{Display: "elem1", Actions: []cmdbar.ResolvedAction{
				{Kind: cmdbar.ActionEffect, Run: func() (string, error) { return "e1", nil }},
			}},
			{Display: "elem2"},
		}, nil, true, nil
	})

	ve := &ValueExpander{
		Hook:           stubHook,
		TemplateEngine: GetTemplateEngine(),
		ArrayHook:      stubArrayHook,
	}

	t.Run("plain text", func(t *testing.T) {
		out := ve.ExpandToCandidates("code1", "→")
		if len(out) != 1 {
			t.Fatalf("expected 1 candidate, got %d", len(out))
		}
		if out[0].Text != "→" {
			t.Errorf("Text = %q, want →", out[0].Text)
		}
	})

	t.Run("$AA expands to runes", func(t *testing.T) {
		out := ve.ExpandToCandidates("arr", `$AA("箭头","←↑→↓")`)
		if len(out) != 4 {
			t.Fatalf("expected 4 candidates, got %d", len(out))
		}
		wantTexts := []string{"←", "↑", "→", "↓"}
		for i, want := range wantTexts {
			if out[i].Text != want {
				t.Errorf("idx %d: Text = %q, want %q", i, out[i].Text, want)
			}
			if out[i].Code != "arr" {
				t.Errorf("idx %d: Code = %q, want arr", i, out[i].Code)
			}
			if out[i].Comment != "箭头" {
				t.Errorf("idx %d: Comment = %q, want 箭头", i, out[i].Comment)
			}
		}
	})

	t.Run("$CC via stub Hook", func(t *testing.T) {
		out := ve.ExpandToCandidates("cmd", `$CC("打开","open(...)")`)
		if len(out) != 1 {
			t.Fatalf("expected 1 candidate, got %d", len(out))
		}
		if len(out[0].Actions) == 0 {
			t.Errorf("expected non-empty Actions for $CC candidate")
		}
	})

	t.Run("$SS via stub ArrayHook", func(t *testing.T) {
		out := ve.ExpandToCandidates("ss", `$SS("数组组","elem1","elem2")`)
		if len(out) != 2 {
			t.Fatalf("expected 2 candidates, got %d", len(out))
		}
		if out[0].Text != "elem1" {
			t.Errorf("idx 0 Text = %q, want elem1", out[0].Text)
		}
		if len(out[0].Actions) == 0 {
			t.Errorf("idx 0 Actions should be non-empty")
		}
		if out[1].Text != "elem2" {
			t.Errorf("idx 1 Text = %q, want elem2", out[1].Text)
		}
		if out[0].Comment != "数组组" {
			t.Errorf("idx 0 Comment = %q, want 数组组", out[0].Comment)
		}
	})

	t.Run("$AA malformed falls back to literal", func(t *testing.T) {
		out := ve.ExpandToCandidates("code2", `$AA(malformed`)
		if len(out) != 1 {
			t.Fatalf("expected 1 fallback candidate, got %d", len(out))
		}
		if out[0].Text != `$AA(malformed` {
			t.Errorf("Text = %q, want literal $AA(malformed", out[0].Text)
		}
	})
}
