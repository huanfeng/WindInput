package ui

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

func TestCandidateDisplayText_NewlineGlyph(t *testing.T) {
	withAction := Candidate{Text: "打开"}
	withAction.Actions = make([]cmdbar.ResolvedAction, 1)

	cases := []struct {
		name string
		cand Candidate
		want string
	}{
		{"plain no newline", Candidate{Text: "你好"}, "你好"},
		{"lf replaced", Candidate{Text: "行1\n行2"}, "行1" + CandidateNewlineGlyph + "行2"},
		{"cr replaced", Candidate{Text: "行1\r行2"}, "行1" + CandidateNewlineGlyph + "行2"},
		{"crlf folds to one glyph", Candidate{Text: "行1\r\n行2"}, "行1" + CandidateNewlineGlyph + "行2"},
		{"command prefix kept", withAction, CmdbarCandidatePrefix + "打开"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := candidateDisplayText(tc.cand); got != tc.want {
				t.Fatalf("candidateDisplayText = %q, want %q", got, tc.want)
			}
		})
	}
}
