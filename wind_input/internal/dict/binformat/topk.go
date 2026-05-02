package binformat

import (
	"container/heap"
	"sort"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// topKPicker 维护一个容量 K 的 min-heap，用于在流式扫描中保留 top-K 高权重候选。
//
// 复杂度：N 次 offer 总开销 O(N log K)；sorted 一次性输出 O(K log K)。
// N 是被扫候选总数，K 是 limit。相比"全量收集 + sort.Slice"的 O(N log N)，
// 当 K << N 时显著更快（如拼音 "s" 场景 N=47k, K=200）。
type topKPicker struct {
	limit int
	h     candHeap
}

func newTopKPicker(limit int) *topKPicker {
	return &topKPicker{limit: limit, h: make(candHeap, 0, limit)}
}

// offer 提交一个候选。若堆未满直接入堆；满后仅当新候选优于当前堆顶（min）时替换。
func (p *topKPicker) offer(c candidate.Candidate) {
	if len(p.h) < p.limit {
		heap.Push(&p.h, c)
		return
	}
	// 堆顶是当前 top-K 中最差的；若新候选更优则替换。
	// 注意 candidate.Better(a, b) == true 表示 a 应排在 b 前。
	if candidate.Better(c, p.h[0]) {
		p.h[0] = c
		heap.Fix(&p.h, 0)
	}
}

// sorted 返回按 candidate.Better 降序排列的最终 top-K。
func (p *topKPicker) sorted() []candidate.Candidate {
	out := make([]candidate.Candidate, len(p.h))
	copy(out, p.h)
	sort.SliceStable(out, func(i, j int) bool {
		return candidate.Better(out[i], out[j])
	})
	return out
}

// candHeap 是 candidate.Better 反向意义的 min-heap：堆顶是"最不优"的元素。
type candHeap []candidate.Candidate

func (h candHeap) Len() int { return len(h) }

// Less：堆顶应是"最不优"的，即被 Better 判定排后面的应优先在堆顶。
// candidate.Better(b, a) == true 表示 b 优于 a，即 a 不优于 b → a 在堆顶。
func (h candHeap) Less(i, j int) bool { return candidate.Better(h[j], h[i]) }
func (h candHeap) Swap(i, j int)      { h[i], h[j] = h[j], h[i] }

func (h *candHeap) Push(x any) { *h = append(*h, x.(candidate.Candidate)) }
func (h *candHeap) Pop() any {
	old := *h
	n := len(old)
	x := old[n-1]
	*h = old[:n-1]
	return x
}
