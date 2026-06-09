package coordinator

import (
	"testing"

	"github.com/huanfeng/wind_input/pkg/config"
)

func TestDecideSpecialAutoCommit(t *testing.T) {
	type in struct {
		strategy    string
		fixedLength int
		bufLen      int
		candCount   int
		hasLonger   bool
	}
	cases := []struct {
		name string
		in   in
		want bool
	}{
		{"prefix_free 唯一无后续→上屏", in{config.SpecialAutoCommitPrefixFree, 0, 2, 1, false}, true},
		{"prefix_free 唯一有后续→等", in{config.SpecialAutoCommitPrefixFree, 0, 2, 1, true}, false},
		{"prefix_free 多候选→等", in{config.SpecialAutoCommitPrefixFree, 0, 2, 3, false}, false},
		{"prefix_free 零候选→等", in{config.SpecialAutoCommitPrefixFree, 0, 2, 0, false}, false},
		{"fixed_length 达长且唯一→上屏", in{config.SpecialAutoCommitFixedLength, 4, 4, 1, false}, true},
		{"fixed_length 达长多候选→等", in{config.SpecialAutoCommitFixedLength, 4, 4, 2, false}, false},
		{"fixed_length 未达长→等", in{config.SpecialAutoCommitFixedLength, 4, 3, 1, false}, false},
		{"manual 永远不上屏", in{config.SpecialAutoCommitManual, 0, 9, 1, false}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := decideSpecialAutoCommit(tc.in.strategy, tc.in.fixedLength, tc.in.bufLen, tc.in.candCount, tc.in.hasLonger)
			if got != tc.want {
				t.Fatalf("got %v want %v", got, tc.want)
			}
		})
	}
}
