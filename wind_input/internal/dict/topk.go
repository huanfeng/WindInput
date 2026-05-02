package dict

import (
	"container/heap"
	"sort"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// memTopKPicker 维护一个容量 K 的 min-heap，用于在流式扫描中保留 top-K 高权重候选。
//
// 用于 dict 包内的内存模式查询（CodeTable.entries 遍历、SimpleDict 遍历等），
// 避免对全量候选做 O(N log N) 排序。binformat / datformat 包各有同源副本，
// 此处独立以保持 dict 包不依赖底层存储格式包。
type memTopKPicker struct {
	limit int
	h     memCandHeap
}

func newMemTopKPicker(limit int) *memTopKPicker {
	return &memTopKPicker{limit: limit, h: make(memCandHeap, 0, limit)}
}

func (p *memTopKPicker) offer(c candidate.Candidate) {
	if len(p.h) < p.limit {
		heap.Push(&p.h, c)
		return
	}
	if candidate.Better(c, p.h[0]) {
		p.h[0] = c
		heap.Fix(&p.h, 0)
	}
}

func (p *memTopKPicker) sorted() []candidate.Candidate {
	out := make([]candidate.Candidate, len(p.h))
	copy(out, p.h)
	sort.SliceStable(out, func(i, j int) bool {
		return candidate.Better(out[i], out[j])
	})
	return out
}

// memCandHeap 是按 candidate.Better 反向排序的 min-heap：堆顶 = "最不优" 元素。
type memCandHeap []candidate.Candidate

func (h memCandHeap) Len() int           { return len(h) }
func (h memCandHeap) Less(i, j int) bool { return candidate.Better(h[j], h[i]) }
func (h memCandHeap) Swap(i, j int)      { h[i], h[j] = h[j], h[i] }

func (h *memCandHeap) Push(x any) { *h = append(*h, x.(candidate.Candidate)) }
func (h *memCandHeap) Pop() any {
	old := *h
	n := len(old)
	x := old[n-1]
	*h = old[:n-1]
	return x
}
