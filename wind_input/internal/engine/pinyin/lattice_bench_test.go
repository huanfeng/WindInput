package pinyin

import "testing"

// BenchmarkBuildLattice 量化 BuildLattice 的 alloc/op 与 ns/op。
// H1 优化（seenSetPool + struct key 取代 latticeKey 字符串拼接）后，
// 在小测试词库下 alloc/op 应明显下降；用 -benchmem 观察。
//
// 运行：
//
//	go test ./internal/engine/pinyin/ -run='^$' -bench=BenchmarkBuildLattice -benchmem -count=5
func BenchmarkBuildLattice(b *testing.B) {
	d := createTestDictForViterbi(b)
	unigram := createTestUnigram(b)
	st := NewSyllableTrie()

	cases := []struct {
		name  string
		input string
	}{
		{"short_jintian", "jintian"},
		{"mid_jintiantianqi", "jintiantianqi"},
		{"long_zhongguorenminhenhao", "zhongguorenminhenhao"},
	}

	for _, tc := range cases {
		b.Run(tc.name, func(b *testing.B) {
			b.ReportAllocs()
			b.ResetTimer()
			for i := 0; i < b.N; i++ {
				_ = BuildLattice(tc.input, st, d, unigram)
			}
		})
	}
}
