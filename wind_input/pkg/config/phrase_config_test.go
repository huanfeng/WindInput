package config

import "testing"

// TestPhraseConfigDefault 验证 input.phrase.min_prefix_length 的默认值与兜底语义:
//   - DefaultConfig() 默认 2
//   - ApplyConfigFallbacks 在 0 / 负值时回退到 2
//   - 用户显式 1 不被覆盖 (per-entry "码长 == 输入长度" 短码场景)
func TestPhraseConfigDefault(t *testing.T) {
	if got := DefaultConfig().Input.Phrase.MinPrefixLength; got != 2 {
		t.Fatalf("DefaultConfig().Input.Phrase.MinPrefixLength = %d, want 2", got)
	}

	cases := []struct {
		name string
		in   int
		want int
	}{
		{"zero falls back to 2", 0, 2},
		{"negative falls back to 2", -1, 2},
		{"explicit 1 preserved", 1, 1},
		{"explicit 3 preserved", 3, 3},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			cfg := DefaultConfig()
			cfg.Input.Phrase.MinPrefixLength = tc.in
			ApplyConfigFallbacks(cfg)
			if got := cfg.Input.Phrase.MinPrefixLength; got != tc.want {
				t.Fatalf("after fallback: got %d, want %d", got, tc.want)
			}
		})
	}
}
