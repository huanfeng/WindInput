package pinyin

import (
	"testing"
)

func TestScorerExactBeatsPartial(t *testing.T) {
	scorer := NewScorer(nil, nil)

	exact := CandidateFeatures{MatchType: MatchExact, SyllableMatch: true, CharCount: 2, LMScore: -8.0}
	partial := CandidateFeatures{MatchType: MatchPartial, SyllableMatch: false, CharCount: 3, LMScore: -7.0}

	scoreExact := scorer.Score(exact)
	scorePartial := scorer.Score(partial)

	if scoreExact <= scorePartial {
		t.Errorf("exact match (%.2f) should beat partial match (%.2f)", scoreExact, scorePartial)
	}
}

func TestScorerUserWordBoost(t *testing.T) {
	scorer := NewScorer(nil, nil)

	system := CandidateFeatures{MatchType: MatchExact, SyllableMatch: true, LMScore: -8.0}
	user := CandidateFeatures{MatchType: MatchExact, SyllableMatch: true, LMScore: -8.0, IsUserWord: true}

	scoreSystem := scorer.Score(system)
	scoreUser := scorer.Score(user)

	if scoreUser <= scoreSystem {
		t.Errorf("user word (%.2f) should beat system word (%.2f)", scoreUser, scoreSystem)
	}
}

func TestScorerViterbiHighPriority(t *testing.T) {
	scorer := NewScorer(nil, nil)

	viterbi := CandidateFeatures{IsViterbi: true, LMScore: -10.0, CharCount: 4}
	exact := CandidateFeatures{MatchType: MatchExact, SyllableMatch: true, LMScore: -6.0, CharCount: 2}

	scoreV := scorer.Score(viterbi)
	scoreE := scorer.Score(exact)

	if scoreV <= scoreE {
		t.Errorf("viterbi (%.2f) should beat exact single match (%.2f)", scoreV, scoreE)
	}
}

func TestScorerCommandHighest(t *testing.T) {
	scorer := NewScorer(nil, nil)

	command := CandidateFeatures{IsCommand: true, CharCount: 1}
	viterbi := CandidateFeatures{IsViterbi: true, LMScore: -5.0, CharCount: 4}

	scoreCmd := scorer.Score(command)
	scoreV := scorer.Score(viterbi)

	if scoreCmd <= scoreV {
		t.Errorf("command (%.2f) should beat viterbi (%.2f)", scoreCmd, scoreV)
	}
}

func TestScorerFuzzyPenalty(t *testing.T) {
	scorer := NewScorer(nil, nil)

	normal := CandidateFeatures{MatchType: MatchExact, SyllableMatch: true, CharCount: 2, LMScore: -8.0}
	fuzzy := CandidateFeatures{MatchType: MatchExact, SyllableMatch: true, CharCount: 2, LMScore: -8.0, IsFuzzy: true}

	scoreNormal := scorer.Score(normal)
	scoreFuzzy := scorer.Score(fuzzy)

	if scoreFuzzy >= scoreNormal {
		t.Errorf("fuzzy (%.2f) should be penalized vs normal (%.2f)", scoreFuzzy, scoreNormal)
	}
}

func TestScorerSegmentRankPenalty(t *testing.T) {
	scorer := NewScorer(nil, nil)

	main := CandidateFeatures{MatchType: MatchExact, SyllableMatch: true, CharCount: 2, LMScore: -8.0, SegmentRank: 0}
	alt := CandidateFeatures{MatchType: MatchExact, SyllableMatch: true, CharCount: 2, LMScore: -8.0, SegmentRank: 1}

	scoreMain := scorer.Score(main)
	scoreAlt := scorer.Score(alt)

	if scoreAlt >= scoreMain {
		t.Errorf("alt segment (%.2f) should be lower than main (%.2f)", scoreAlt, scoreMain)
	}
}
