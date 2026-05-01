// Package perf 提供按键链路性能采样的内存环形缓冲，可主动导出为 JSONL 文件。
//
// 用途：定位输入流畅性瓶颈（首键冷启动 / 引擎查询 / UI 渲染等阶段时延），
// 在不开 trace 工具的情况下让用户/开发者获取最近 N 次按键的细分耗时。
//
// 隐私：Sample.Input 含用户输入编码，仅在 DEBUG 调试场景使用；
// 默认仅在显式调用 Export* 时落盘，且文件由调用方控制。
package perf

import (
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"sync"
	"time"
)

const defaultCapacity = 512

// EngineTiming 引擎层各阶段细分耗时（来自 codetable.ConvertEx 等引擎实现）。
// 字段为 0 表示该引擎/路径未参与或未埋点。
type EngineTiming struct {
	Convert time.Duration `json:"convert,omitempty"` // 引擎 ConvertEx 总耗时
	Exact   time.Duration `json:"exact,omitempty"`   // Phase1 精确匹配查询
	Prefix  time.Duration `json:"prefix,omitempty"`  // Phase2 前缀匹配查询
	Weight  time.Duration `json:"weight,omitempty"`  // Phase3 权重处理（含 Phase3.5 补全）
	Sort    time.Duration `json:"sort,omitempty"`    // Phase5 排序 + ProtectTopN
	Shadow  time.Duration `json:"shadow,omitempty"`  // Phase6 Shadow 拦截
	Filter  time.Duration `json:"filter,omitempty"`  // 过滤 + 截断
}

// Sample 单次按键链路的性能样本。
type Sample struct {
	Time       time.Time     `json:"time"`
	Input      string        `json:"input"`     // 当时的 inputBuffer（DEBUG 用途）
	InputLen   int           `json:"input_len"` // 字节长度（避免依赖 input 解析隐私）
	FirstKey   bool          `json:"first_key"` // composition 首键（commit/Hide 后第一次按）
	EngineType string        `json:"engine_type,omitempty"`
	CandCount  int           `json:"cand_count"`
	Total      time.Duration `json:"total"`
	Update     time.Duration `json:"update"`            // updateCandidatesEx 总耗时
	ShowUI     time.Duration `json:"show_ui,omitempty"` // armPendingFirstShow / showUI 耗时
	Engine     EngineTiming  `json:"engine"`
}

type recorder struct {
	mu   sync.Mutex
	buf  []Sample
	head int
	full bool
	cap  int
}

var global = &recorder{cap: defaultCapacity, buf: make([]Sample, 0, defaultCapacity)}

// SetCapacity 调整环形缓冲容量。<=0 时忽略；新容量小于已有样本数时丢弃最旧样本。
func SetCapacity(n int) {
	if n <= 0 {
		return
	}
	global.mu.Lock()
	defer global.mu.Unlock()
	old := snapshotLocked(global)
	global.cap = n
	global.buf = make([]Sample, 0, n)
	global.head = 0
	global.full = false
	start := 0
	if len(old) > n {
		start = len(old) - n
	}
	for _, s := range old[start:] {
		appendLocked(global, s)
	}
}

// Capacity 返回当前容量。
func Capacity() int {
	global.mu.Lock()
	defer global.mu.Unlock()
	return global.cap
}

// Record 记录一次样本。线程安全；O(1)。
func Record(s Sample) {
	global.mu.Lock()
	defer global.mu.Unlock()
	if global.cap == 0 {
		global.cap = defaultCapacity
	}
	appendLocked(global, s)
}

func appendLocked(r *recorder, s Sample) {
	if len(r.buf) < r.cap {
		r.buf = append(r.buf, s)
		return
	}
	r.buf[r.head] = s
	r.head = (r.head + 1) % r.cap
	r.full = true
}

func snapshotLocked(r *recorder) []Sample {
	if !r.full {
		out := make([]Sample, len(r.buf))
		copy(out, r.buf)
		return out
	}
	out := make([]Sample, 0, len(r.buf))
	out = append(out, r.buf[r.head:]...)
	out = append(out, r.buf[:r.head]...)
	return out
}

// Snapshot 返回当前环形缓冲的有序拷贝（最旧 → 最新）。
func Snapshot() []Sample {
	global.mu.Lock()
	defer global.mu.Unlock()
	return snapshotLocked(global)
}

// Clear 清空缓冲。
func Clear() {
	global.mu.Lock()
	defer global.mu.Unlock()
	global.buf = global.buf[:0]
	global.head = 0
	global.full = false
}

// StatsRow 一组样本的耗时分位数（基于 Total）。
type StatsRow struct {
	Count int           `json:"count"`
	P50   time.Duration `json:"p50"`
	P95   time.Duration `json:"p95"`
	P99   time.Duration `json:"p99"`
	Max   time.Duration `json:"max"`
	Avg   time.Duration `json:"avg"`
}

// Stats 全量统计：分别给出首键/续键/全部 三组分位数。
type Stats struct {
	Count        int      `json:"count"`
	First        StatsRow `json:"first_key"`
	Continuation StatsRow `json:"continuation"`
	All          StatsRow `json:"all"`
}

// ComputeStats 基于 Total 字段计算分位数统计。
func ComputeStats() Stats {
	samples := Snapshot()
	var first, cont, all []time.Duration
	for _, s := range samples {
		all = append(all, s.Total)
		if s.FirstKey {
			first = append(first, s.Total)
		} else {
			cont = append(cont, s.Total)
		}
	}
	return Stats{
		Count:        len(samples),
		First:        durRow(first),
		Continuation: durRow(cont),
		All:          durRow(all),
	}
}

func durRow(ds []time.Duration) StatsRow {
	if len(ds) == 0 {
		return StatsRow{}
	}
	sort.Slice(ds, func(i, j int) bool { return ds[i] < ds[j] })
	var sum time.Duration
	for _, d := range ds {
		sum += d
	}
	pick := func(p int) time.Duration {
		idx := len(ds) * p / 100
		if idx >= len(ds) {
			idx = len(ds) - 1
		}
		return ds[idx]
	}
	return StatsRow{
		Count: len(ds),
		P50:   pick(50),
		P95:   pick(95),
		P99:   pick(99),
		Max:   ds[len(ds)-1],
		Avg:   sum / time.Duration(len(ds)),
	}
}

// FormatStats 返回单行可读统计摘要。
func FormatStats(s Stats) string {
	row := func(name string, r StatsRow) string {
		if r.Count == 0 {
			return fmt.Sprintf("%s: n=0", name)
		}
		return fmt.Sprintf("%s n=%d avg=%v p50=%v p95=%v p99=%v max=%v",
			name, r.Count, r.Avg, r.P50, r.P95, r.P99, r.Max)
	}
	return fmt.Sprintf("%s | %s | %s",
		row("first", s.First),
		row("cont", s.Continuation),
		row("all", s.All),
	)
}

// ExportJSONL 把当前环形缓冲写到 path（JSON Lines；首行为 header 含统计）。
// 返回实际写入的样本数。
func ExportJSONL(path string) (int, error) {
	samples := Snapshot()
	stats := ComputeStats()

	f, err := os.Create(path)
	if err != nil {
		return 0, err
	}
	defer f.Close()
	enc := json.NewEncoder(f)

	header := map[string]any{
		"kind":     "perf-header",
		"time":     time.Now(),
		"capacity": Capacity(),
		"count":    len(samples),
		"stats":    stats,
	}
	if err := enc.Encode(header); err != nil {
		return 0, err
	}
	for _, s := range samples {
		if err := enc.Encode(s); err != nil {
			return 0, err
		}
	}
	return len(samples), nil
}
