package ui

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

func TestCandidateDisplayText_NewlineGlyph(t *testing.T) {
	// ActionEffect 候选 → 应该被加前缀
	withEffect := Candidate{Text: "打开"}
	withEffect.Actions = []cmdbar.ResolvedAction{{Kind: cmdbar.ActionEffect}}

	// 仅 ActionText 候选 (例如 $CC("hello", type("hello"))) → 视觉上与
	// 普通文本候选无差, 不加前缀
	withTextOnly := Candidate{Text: "hello"}
	withTextOnly.Actions = []cmdbar.ResolvedAction{{Kind: cmdbar.ActionText}}

	// 混合 ActionText + ActionEffect → 加前缀
	withMixed := Candidate{Text: "【】"}
	withMixed.Actions = []cmdbar.ResolvedAction{
		{Kind: cmdbar.ActionText},
		{Kind: cmdbar.ActionEffect},
	}

	const customPrefix = "▶"

	cases := []struct {
		name   string
		cand   Candidate
		prefix string
		want   string
	}{
		{"plain no newline", Candidate{Text: "你好"}, DefaultCmdbarCandidatePrefix, "你好"},
		{"lf replaced", Candidate{Text: "行1\n行2"}, DefaultCmdbarCandidatePrefix, "行1" + CandidateNewlineGlyph + "行2"},
		{"cr replaced", Candidate{Text: "行1\r行2"}, DefaultCmdbarCandidatePrefix, "行1" + CandidateNewlineGlyph + "行2"},
		{"crlf folds to one glyph", Candidate{Text: "行1\r\n行2"}, DefaultCmdbarCandidatePrefix, "行1" + CandidateNewlineGlyph + "行2"},
		{"side effect gets default prefix", withEffect, DefaultCmdbarCandidatePrefix, DefaultCmdbarCandidatePrefix + "打开"},
		{"action text only no prefix", withTextOnly, DefaultCmdbarCandidatePrefix, "hello"},
		{"mixed action keeps prefix", withMixed, DefaultCmdbarCandidatePrefix, DefaultCmdbarCandidatePrefix + "【】"},
		{"empty prefix disables marker", withEffect, "", "打开"},
		{"custom prefix used", withEffect, customPrefix, customPrefix + "打开"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := candidateDisplayText(tc.cand, tc.prefix); got != tc.want {
				t.Fatalf("candidateDisplayText = %q, want %q", got, tc.want)
			}
		})
	}
}
