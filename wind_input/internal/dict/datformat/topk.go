package datformat

import (
	"container/heap"
	"sort"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// topKPicker 维护一个容量 K 的 min-heap，用于在流式扫描中保留 top-K 高权重候选。
// 详见 binformat/topk.go 的实现说明（此处与之同源，复制以保持包独立）。
type topKPicker struct {
	limit int
	h     candHeap
}

func newTopKPicker(limit int) *topKPicker {
	return &topKPicker{limit: limit, h: make(candHeap, 0, limit)}
}

func (p *topKPicker) offer(c candidate.Candidate) {
	if len(p.h) < p.limit {
		heap.Push(&p.h, c)
		return
	}
	if candidate.Better(c, p.h[0]) {
		p.h[0] = c
		heap.Fix(&p.h, 0)
	}
}

func (p *topKPicker) sorted() []candidate.Candidate {
	out := make([]candidate.Candidate, len(p.h))
	copy(out, p.h)
	sort.SliceStable(out, func(i, j int) bool {
		return candidate.Better(out[i], out[j])
	})
	return out
}

type candHeap []candidate.Candidate

func (h candHeap) Len() int           { return len(h) }
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
