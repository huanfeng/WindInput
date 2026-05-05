package store

import (
	"log/slog"
	"sync"
	"time"
	"unicode"
)

// StatEvent 单次上屏的统计事件（仅元数据，不含原文）
type StatEvent struct {
	Timestamp    time.Time
	RuneCount    int
	ChineseCount int
	EnglishCount int
	PunctCount   int
	OtherCount   int
	CodeLen      int // 编码长度（0=标点/直接输入）
	CandidatePos int // 候选位置: 0=首选, 1=次选, ... -1=非候选
	SchemaID     string
	Source       CommitSource
}

// 活跃输入判定阈值：两次上屏间隔小于此值视为持续输入
const activeThreshold = 60 * time.Second

// StatCollector 输入统计采集器，内存聚合 + 定期持久化
type StatCollector struct {
	mu     sync.Mutex
	store  *Store
	logger *slog.Logger

	today          *DailyStat // 当日内存聚合
	meta           *StatsMeta // 全局元数据
	dirty          bool       // 是否有未持久化的变更
	lastCommitTime time.Time  // 上一次上屏时间（用于计算活跃时间）

	done chan struct{}
	wg   sync.WaitGroup
}

// NewStatCollector 创建统计采集器并启动后台 flush
func NewStatCollector(store *Store, logger *slog.Logger) *StatCollector {
	sc := &StatCollector{
		store:  store,
		logger: logger,
		done:   make(chan struct{}),
	}

	// 加载当日数据和元数据
	sc.loadToday()
	sc.loadMeta()

	// 启动后台定期 flush
	sc.wg.Add(1)
	go sc.loop()

	return sc
}

// Record 记录一次上屏事件
func (sc *StatCollector) Record(event StatEvent) {
	sc.mu.Lock()
	defer sc.mu.Unlock()

	today := time.Now().Format("2006-01-02")
	// 跨天检测
	if sc.today.Date != today {
		sc.flushLocked()
		sc.today = NewDailyStat(today)
	}

	d := sc.today
	d.TotalChars += event.RuneCount
	d.ChineseChars += event.ChineseCount
	d.EnglishChars += event.EnglishCount
	d.PunctChars += event.PunctCount
	d.OtherChars += event.OtherCount
	d.CommitCount++

	hour := event.Timestamp.Hour()
	d.Hours[hour] += event.RuneCount

	// 码长统计（仅候选词上屏且 CodeLen > 0）
	if event.CodeLen > 0 {
		d.CodeLenSum += event.CodeLen
		d.CodeLenCount++
		idx := min(event.CodeLen-1, 5)
		d.CodeLenDist[idx]++
	}

	// 选重统计（仅候选词上屏）
	if event.CandidatePos >= 0 {
		idx := min(event.CandidatePos, 4)
		d.CandPosDist[idx]++
	}

	// 按方案统计
	if event.SchemaID != "" {
		ss, ok := d.BySchema[event.SchemaID]
		if !ok {
			ss = &SchemaStats{}
			d.BySchema[event.SchemaID] = ss
		}
		ss.TotalChars += event.RuneCount
		ss.CommitCount++
		if event.CodeLen > 0 {
			ss.CodeLenSum += event.CodeLen
			ss.CodeLenCount++
		}
		if event.CandidatePos >= 0 {
			ss.CandPosDist[min(event.CandidatePos, 4)]++
		}
	}

	// 按来源统计
	if int(event.Source) < len(d.BySource) {
		d.BySource[event.Source] += event.RuneCount
	}

	// 活跃时间：与上次上屏间隔 < 60s 则累加
	now := event.Timestamp
	if !sc.lastCommitTime.IsZero() && now.Sub(sc.lastCommitTime) < activeThreshold {
		d.ActiveSeconds += int(now.Sub(sc.lastCommitTime).Seconds())
	}
	sc.lastCommitTime = now

	// 更新元数据
	sc.meta.TotalChars += int64(event.RuneCount)

	sc.dirty = true
}

// RecordTSFEnglish 记录 TSF 英文模式的批量统计（来自 C++ 异步上报）
func (sc *StatCollector) RecordTSFEnglish(chars, digits, puncts, spaces, elapsedMs int) {
	sc.mu.Lock()
	defer sc.mu.Unlock()

	today := time.Now().Format("2006-01-02")
	if sc.today.Date != today {
		sc.flushLocked()
		sc.today = NewDailyStat(today)
	}

	total := chars + digits + puncts + spaces
	d := sc.today
	d.TotalChars += total
	d.EnglishChars += chars
	d.PunctChars += puncts
	d.OtherChars += digits + spaces
	d.CommitCount += total

	now := time.Now()
	hour := now.Hour()
	d.Hours[hour] += total

	if int(SourceTSFDirect) < len(d.BySource) {
		d.BySource[SourceTSFDirect] += total
	}

	if elapsedMs > 0 {
		d.ActiveSeconds += (elapsedMs + 999) / 1000
	} else if !sc.lastCommitTime.IsZero() && now.Sub(sc.lastCommitTime) < activeThreshold {
		d.ActiveSeconds += int(now.Sub(sc.lastCommitTime).Seconds())
	}
	sc.lastCommitTime = now

	sc.meta.TotalChars += int64(total)
	sc.dirty = true
}

// Flush 将内存数据持久化到 BBolt
func (sc *StatCollector) Flush() {
	sc.mu.Lock()
	defer sc.mu.Unlock()
	sc.flushLocked()
}

// Close 关闭采集器，flush 后停止后台协程
func (sc *StatCollector) Close() {
	close(sc.done)
	sc.wg.Wait()
	sc.Flush()
}

// Reset 清空所有内存中的统计数据（配合 Store.ClearStats 使用）
func (sc *StatCollector) Reset() {
	sc.mu.Lock()
	defer sc.mu.Unlock()
	sc.today = NewDailyStat(time.Now().Format("2006-01-02"))
	sc.meta = &StatsMeta{
		FirstDay: time.Now().Format("2006-01-02"),
	}
	sc.dirty = false
}

// Pause 暂停前 flush 所有数据
func (sc *StatCollector) Pause() {
	sc.Flush()
}

// Resume 恢复后重新加载数据
func (sc *StatCollector) Resume() {
	sc.mu.Lock()
	defer sc.mu.Unlock()
	sc.loadTodayLocked()
	sc.loadMetaLocked()
}

// GetTodayStat 返回当日统计的快照（线程安全）
func (sc *StatCollector) GetTodayStat() *DailyStat {
	sc.mu.Lock()
	defer sc.mu.Unlock()

	today := time.Now().Format("2006-01-02")
	if sc.today.Date != today {
		sc.flushLocked()
		sc.today = NewDailyStat(today)
	}

	// 返回拷贝
	snapshot := *sc.today
	if sc.today.BySchema != nil {
		snapshot.BySchema = make(map[string]*SchemaStats, len(sc.today.BySchema))
		for k, v := range sc.today.BySchema {
			cp := *v
			snapshot.BySchema[k] = &cp
		}
	}
	return &snapshot
}

// GetMeta 返回元数据快照（线程安全）
func (sc *StatCollector) GetMeta() *StatsMeta {
	sc.mu.Lock()
	defer sc.mu.Unlock()
	cp := *sc.meta
	return &cp
}

// loop 后台定时 flush
func (sc *StatCollector) loop() {
	defer sc.wg.Done()
	ticker := time.NewTicker(30 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-sc.done:
			return
		case <-ticker.C:
			sc.Flush()
		}
	}
}

// flushLocked 持久化内存数据（必须持有锁）
// sc.today 始终保持完整的当日数据，flush 时直接覆盖写入 DB。
// 不重置 sc.today，这样查询时总能获得完整的当日统计。
func (sc *StatCollector) flushLocked() {
	if !sc.dirty || sc.store == nil || sc.store.IsPaused() {
		return
	}

	// 直接覆盖写入当日数据（sc.today 已包含完整累计）
	if err := sc.store.PutDailyStat(sc.today); err != nil {
		sc.logger.Error("Failed to flush DailyStat", "date", sc.today.Date, "error", err)
		return
	}

	// 更新连续天数
	sc.updateStreak()

	// 更新历史最快速度（按天计算）
	if speed := SpeedPerMinute(sc.today.TotalChars, sc.today.ActiveSeconds); speed > sc.meta.MaxSpeed {
		sc.meta.MaxSpeed = speed
	}

	// 持久化元数据
	if err := sc.store.PutStatsMeta(sc.meta); err != nil {
		sc.logger.Error("Failed to flush StatsMeta", "error", err)
		return
	}

	sc.dirty = false
}

// updateStreak 更新连续输入天数
func (sc *StatCollector) updateStreak() {
	today := sc.today.Date
	if sc.meta.StreakLastDay == "" {
		sc.meta.StreakCurrent = 1
		sc.meta.StreakLastDay = today
		if sc.meta.StreakMax < 1 {
			sc.meta.StreakMax = 1
		}
		return
	}
	if sc.meta.StreakLastDay == today {
		return // 同一天，不重复计算
	}

	lastDay, err := time.Parse("2006-01-02", sc.meta.StreakLastDay)
	if err != nil {
		sc.meta.StreakCurrent = 1
		sc.meta.StreakLastDay = today
		return
	}
	todayTime, err := time.Parse("2006-01-02", today)
	if err != nil {
		return
	}

	diff := todayTime.Sub(lastDay).Hours() / 24
	if diff <= 1.5 { // 容忍时区偏差
		sc.meta.StreakCurrent++
	} else {
		sc.meta.StreakCurrent = 1
	}
	sc.meta.StreakLastDay = today
	if sc.meta.StreakCurrent > sc.meta.StreakMax {
		sc.meta.StreakMax = sc.meta.StreakCurrent
	}
}

// loadToday 从 BBolt 加载当日数据
func (sc *StatCollector) loadToday() {
	sc.mu.Lock()
	defer sc.mu.Unlock()
	sc.loadTodayLocked()
}

func (sc *StatCollector) loadTodayLocked() {
	today := time.Now().Format("2006-01-02")
	stat, err := sc.store.GetDailyStat(today)
	if err != nil {
		sc.logger.Error("Failed to load today's stats", "error", err)
	}
	if stat != nil {
		sc.today = stat
		if sc.today.BySchema == nil {
			sc.today.BySchema = make(map[string]*SchemaStats)
		}
	} else {
		sc.today = NewDailyStat(today)
	}
}

// loadMeta 从 BBolt 加载元数据
func (sc *StatCollector) loadMeta() {
	sc.mu.Lock()
	defer sc.mu.Unlock()
	sc.loadMetaLocked()
}

func (sc *StatCollector) loadMetaLocked() {
	meta, err := sc.store.GetStatsMeta()
	if err != nil {
		sc.logger.Error("Failed to load stats meta", "error", err)
	}
	if meta != nil {
		sc.meta = meta
	} else {
		sc.meta = &StatsMeta{}
	}
	if sc.meta.FirstDay == "" {
		sc.meta.FirstDay = time.Now().Format("2006-01-02")
	}
}

// ClassifyChars 按 rune 分类字符（纯计数，不存储原文）
func ClassifyChars(text string) (chinese, english, punct, other int) {
	for _, r := range text {
		switch {
		case unicode.Is(unicode.Han, r):
			chinese++
		case (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z'):
			english++
		case unicode.IsPunct(r) || unicode.IsSymbol(r):
			punct++
		default:
			other++
		}
	}
	return
}
