package coordinator

import (
	"log/slog"
	"time"
)

// phaseTimer 在 HandleKeyEvent 的单次调用范围内累积"按命名 phase 划分的耗时"。
// 用法见 HandleKeyEvent: 入口构造 → 关键边界 mark → defer dumpIfSlow。
//
// 排查目的: KeyEvent 慢请求 (>20ms) 的耗时归因。每个 mark 记录的是「自上次
// mark/构造以来这段路径的耗时」, 而非"接下来要做的事"。慢请求触发时一次性
// 输出 total + 各 phase 的 delta, 比在十几个地方各打一条难以关联的日志要清晰。
//
// 不并发安全: 同一时刻仅供单个 HandleKeyEvent goroutine 使用 (调用方持有 c.mu 保护),
// 因此 slice append 不需要锁。
type phaseTimer struct {
	start  time.Time
	last   time.Time
	phases []phaseRecord
}

type phaseRecord struct {
	name string
	dur  time.Duration
}

// newPhaseTimer 创建一个新的 phase timer, start/last 均设为当前时间。
func newPhaseTimer() *phaseTimer {
	now := time.Now()
	return &phaseTimer{start: now, last: now}
}

// mark 把「上次 mark/newPhaseTimer 到现在」这段时间归入名为 name 的 phase。
// nil receiver 安全 (诊断未启用 / 已清空时调用静默忽略)。
func (pt *phaseTimer) mark(name string) {
	if pt == nil {
		return
	}
	now := time.Now()
	pt.phases = append(pt.phases, phaseRecord{name: name, dur: now.Sub(pt.last)})
	pt.last = now
}

// addDuration 把已知 duration 的 phase 直接记入 (不基于上次 mark 的 elapsed)。
// 用于「子系统返回了自己的 timing 统计, 我们想暴露到顶层 phase 列表」的场景。
// 不更新 last, 因此后续 mark 仍以「上次 mark 时间」为锚, 子 phase 数据是
// "补充字段", 不破坏主时间线。
func (pt *phaseTimer) addDuration(name string, d time.Duration) {
	if pt == nil {
		return
	}
	pt.phases = append(pt.phases, phaseRecord{name: name, dur: d})
}

// total 返回 newPhaseTimer 到现在的总耗时。nil receiver 返回 0。
func (pt *phaseTimer) total() time.Duration {
	if pt == nil {
		return 0
	}
	return time.Since(pt.start)
}

// dumpIfSlow 仅当 total >= threshold 时, 用 Warn 级别一次性输出 total + 各
// phase delta + 调用方追加的 extra 结构化字段 (alternating key/value)。
// 不命中阈值时不产生任何日志。
func (pt *phaseTimer) dumpIfSlow(threshold time.Duration, logger *slog.Logger, msg string, extra ...any) {
	if pt == nil {
		return
	}
	total := pt.total()
	if total < threshold {
		return
	}
	attrs := make([]any, 0, 2+len(extra)+len(pt.phases)*2)
	attrs = append(attrs, "total", total)
	attrs = append(attrs, extra...)
	for _, p := range pt.phases {
		attrs = append(attrs, "p_"+p.name, p.dur)
	}
	logger.Warn(msg, attrs...)
}

// markKeyPhase 给子函数 (updateCandidates / expandCandidates 等) 用的 phase 标记入口。
// 通过 c.keyPhaseTimer 暗道传递, 避免改大量函数签名。
// nil 安全: HandleKeyEvent 之外的路径 keyPhaseTimer 为 nil, 调用静默忽略。
//
// 调用约定: 必须在持有 c.mu 期间使用 (避免并发写 phases slice)。
func (c *Coordinator) markKeyPhase(name string) {
	c.keyPhaseTimer.mark(name)
}

// markKeyPhaseDuration 把子系统 (engine.ConvertEx 等) 自带的 timing 字段挂到 KeyEvent
// phase 列表里。用于把 engine 内部的 Exact/Prefix/Sort/Filter 等子阶段暴露到慢请求
// breakdown, 让定位粒度从「convert 整段慢」细化到「convert 内部哪一段慢」。
func (c *Coordinator) markKeyPhaseDuration(name string, d time.Duration) {
	c.keyPhaseTimer.addDuration(name, d)
}
