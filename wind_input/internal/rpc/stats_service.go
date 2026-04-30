package rpc

import (
	"fmt"
	"log/slog"
	"time"

	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// StatsService 输入统计 RPC 服务
type StatsService struct {
	store         *store.Store
	logger        *slog.Logger
	statCollector *store.StatCollector
	server        *Server
}

// GetSummary 获取统计概览
func (s *StatsService) GetSummary(args *rpcapi.Empty, reply *rpcapi.StatsSummaryReply) error {
	if s.statCollector == nil {
		return nil
	}

	// 当日数据来自内存（始终是最新、完整的）
	today := s.statCollector.GetTodayStat()
	meta := s.statCollector.GetMeta()

	reply.TodayChars = today.TotalChars
	reply.TodayChinese = today.ChineseChars
	reply.TodayEnglish = today.EnglishChars
	// meta.TotalChars 已包含当日数据（Record 时实时递增）
	reply.TotalChars = meta.TotalChars
	reply.StreakCurrent = meta.StreakCurrent
	reply.StreakMax = meta.StreakMax

	// 计算活跃天数：DB 中的天数（定期 flush 后已含今天）
	// 如果今天从未 flush 过但有数据，额外 +1
	days, _ := s.store.CountStatsDays()
	now := time.Now()
	todayStr := now.Format("2006-01-02")
	if today.TotalChars > 0 {
		todayInDB, _ := s.store.GetDailyStat(todayStr)
		if todayInDB == nil {
			days++ // 今天还未 flush 到 DB
		}
	}
	reply.ActiveDays = days
	if days > 0 {
		reply.DailyAvg = int(reply.TotalChars / int64(days))
	}

	// 本周和本月统计：DB 数据 + 用内存中的今天替换 DB 中可能过期的今天
	weekStart := now.AddDate(0, 0, -int(now.Weekday()))
	monthStart := time.Date(now.Year(), now.Month(), 1, 0, 0, 0, 0, now.Location())
	weekFrom := weekStart.Format("2006-01-02")
	monthFrom := monthStart.Format("2006-01-02")

	weekDays, _ := s.store.GetDailyStats(weekFrom, todayStr)
	for _, d := range weekDays {
		if d.Date == todayStr {
			reply.WeekChars += today.TotalChars // 用内存中的最新数据
		} else {
			reply.WeekChars += d.TotalChars
		}
	}
	// 如果今天在范围内但 DB 中没有（未 flush），追加内存数据
	if today.TotalChars > 0 && todayStr >= weekFrom {
		hasToday := false
		for _, d := range weekDays {
			if d.Date == todayStr {
				hasToday = true
				break
			}
		}
		if !hasToday {
			reply.WeekChars += today.TotalChars
		}
	}

	monthDays, _ := s.store.GetDailyStats(monthFrom, todayStr)
	for _, d := range monthDays {
		if d.Date == todayStr {
			reply.MonthChars += today.TotalChars
		} else {
			reply.MonthChars += d.TotalChars
		}
	}
	if today.TotalChars > 0 && todayStr >= monthFrom {
		hasToday := false
		for _, d := range monthDays {
			if d.Date == todayStr {
				hasToday = true
				break
			}
		}
		if !hasToday {
			reply.MonthChars += today.TotalChars
		}
	}

	// 最高日和码长/选重率（从最近90天计算）
	recentFrom := now.AddDate(0, -3, 0).Format("2006-01-02")
	recentDays, _ := s.store.GetDailyStats(recentFrom, todayStr)

	var totalCodeLen, totalCodeCount int
	var totalFirstSelect, totalCandSelect int

	for _, d := range recentDays {
		chars := d.TotalChars
		codeLenSum := d.CodeLenSum
		codeLenCount := d.CodeLenCount
		candPosDist := d.CandPosDist
		// 用内存中的最新今天数据替换
		if d.Date == todayStr {
			chars = today.TotalChars
			codeLenSum = today.CodeLenSum
			codeLenCount = today.CodeLenCount
			candPosDist = today.CandPosDist
		}
		if chars > reply.MaxDayChars {
			reply.MaxDayChars = chars
			reply.MaxDayDate = d.Date
		}
		totalCodeLen += codeLenSum
		totalCodeCount += codeLenCount
		totalFirstSelect += candPosDist[0]
		for _, v := range candPosDist {
			totalCandSelect += v
		}
	}
	// 如果今天不在 DB 中，单独计算
	if today.TotalChars > 0 {
		hasToday := false
		for _, d := range recentDays {
			if d.Date == todayStr {
				hasToday = true
				break
			}
		}
		if !hasToday {
			if today.TotalChars > reply.MaxDayChars {
				reply.MaxDayChars = today.TotalChars
				reply.MaxDayDate = today.Date
			}
			totalCodeLen += today.CodeLenSum
			totalCodeCount += today.CodeLenCount
			totalFirstSelect += today.CandPosDist[0]
			for _, v := range today.CandPosDist {
				totalCandSelect += v
			}
		}
	}

	if totalCodeCount > 0 {
		reply.AvgCodeLen = float64(totalCodeLen) / float64(totalCodeCount)
	}
	if totalCandSelect > 0 {
		reply.FirstSelectRate = float64(totalFirstSelect) / float64(totalCandSelect)
	}

	// 速度统计
	// 今日速度
	reply.TodaySpeed = store.SpeedPerMinute(today.TotalChars, today.ActiveSeconds)
	// 统计区间平均速度（近90天）
	var totalActiveSec int
	var totalCharsForSpeed int
	for _, d := range recentDays {
		if d.Date == todayStr {
			totalActiveSec += today.ActiveSeconds
			totalCharsForSpeed += today.TotalChars
		} else {
			totalActiveSec += d.ActiveSeconds
			totalCharsForSpeed += d.TotalChars
		}
	}
	// 如果今天不在 DB 中
	if today.TotalChars > 0 {
		hasToday := false
		for _, d := range recentDays {
			if d.Date == todayStr {
				hasToday = true
				break
			}
		}
		if !hasToday {
			totalActiveSec += today.ActiveSeconds
			totalCharsForSpeed += today.TotalChars
		}
	}
	reply.OverallSpeed = store.SpeedPerMinute(totalCharsForSpeed, totalActiveSec)
	// 历史最快速度
	reply.MaxSpeed = meta.MaxSpeed

	return nil
}

// GetDaily 获取日期范围内的每日统计
func (s *StatsService) GetDaily(args *rpcapi.StatsGetDailyArgs, reply *rpcapi.StatsGetDailyReply) error {
	days, err := s.store.GetDailyStats(args.From, args.To)
	if err != nil {
		return err
	}

	reply.Days = make([]rpcapi.StatsDailyItem, 0, len(days))
	for _, d := range days {
		item := rpcapi.StatsDailyItem{
			Date:          d.Date,
			TotalChars:    d.TotalChars,
			ChineseChars:  d.ChineseChars,
			EnglishChars:  d.EnglishChars,
			PunctChars:    d.PunctChars,
			OtherChars:    d.OtherChars,
			Hours:         d.Hours,
			CommitCount:   d.CommitCount,
			CodeLenSum:    d.CodeLenSum,
			CodeLenCount:  d.CodeLenCount,
			CodeLenDist:   d.CodeLenDist,
			CandPosDist:   d.CandPosDist,
			ActiveSeconds: d.ActiveSeconds,
			BySource:      d.BySource,
		}
		if len(d.BySchema) > 0 {
			item.BySchema = make(map[string]*rpcapi.SchemaStatsItem, len(d.BySchema))
			for k, v := range d.BySchema {
				item.BySchema[k] = &rpcapi.SchemaStatsItem{
					TotalChars:   v.TotalChars,
					CommitCount:  v.CommitCount,
					CodeLenSum:   v.CodeLenSum,
					CodeLenCount: v.CodeLenCount,
					CandPosDist:  v.CandPosDist,
				}
			}
		}
		reply.Days = append(reply.Days, item)
	}

	// 用内存中的最新今天数据替换 DB 中可能过期的今天数据
	if s.statCollector != nil {
		todayStr := time.Now().Format("2006-01-02")
		if args.From <= todayStr && todayStr <= args.To {
			today := s.statCollector.GetTodayStat()
			if today.TotalChars > 0 {
				todayItem := rpcapi.StatsDailyItem{
					Date:          today.Date,
					TotalChars:    today.TotalChars,
					ChineseChars:  today.ChineseChars,
					EnglishChars:  today.EnglishChars,
					PunctChars:    today.PunctChars,
					OtherChars:    today.OtherChars,
					Hours:         today.Hours,
					CommitCount:   today.CommitCount,
					CodeLenSum:    today.CodeLenSum,
					CodeLenCount:  today.CodeLenCount,
					CodeLenDist:   today.CodeLenDist,
					CandPosDist:   today.CandPosDist,
					ActiveSeconds: today.ActiveSeconds,
					BySource:      today.BySource,
				}
				// 替换或追加（不合并，因为内存数据已是完整的）
				replaced := false
				for i := range reply.Days {
					if reply.Days[i].Date == todayStr {
						reply.Days[i] = todayItem
						replaced = true
						break
					}
				}
				if !replaced {
					reply.Days = append(reply.Days, todayItem)
				}
			}
		}
	}

	return nil
}

// GetConfig 获取统计配置
func (s *StatsService) GetConfig(args *rpcapi.Empty, reply *rpcapi.StatsConfigReply) error {
	if s.server == nil || s.server.cfg == nil {
		// 返回默认值
		reply.Enabled = true
		reply.RetainDays = 0
		reply.TrackEnglish = true
		return nil
	}
	s.server.cfgMu.RLock()
	stats := s.server.cfg.Stats
	s.server.cfgMu.RUnlock()
	reply.Enabled = stats.IsEnabled()
	reply.RetainDays = stats.RetainDays
	reply.TrackEnglish = stats.IsTrackEnglish()
	return nil
}

// UpdateConfig 更新统计配置（写锁内完成 snapshot+持久化+ApplyConfigUpdate 增量热更新）
func (s *StatsService) UpdateConfig(args *rpcapi.StatsConfigUpdateArgs, reply *rpcapi.Empty) error {
	if s.server == nil || s.server.cfg == nil {
		return fmt.Errorf("config not available")
	}
	s.server.cfgMu.Lock()
	defer s.server.cfgMu.Unlock()

	oldCfg := *s.server.cfg
	newCfg := deepCopyConfig(s.server.cfg)
	newCfg.Stats.Enabled = &args.Enabled
	newCfg.Stats.RetainDays = args.RetainDays
	newCfg.Stats.TrackEnglish = &args.TrackEnglish
	config.ApplyConfigFallbacks(newCfg)

	if err := config.Save(newCfg); err != nil {
		return err
	}

	if s.server.configReloader != nil {
		// 调用方已持有 cfgMu 写锁，符合 ApplyConfigUpdate 的契约
		if _, err := s.server.configReloader.ApplyConfigUpdate(&oldCfg, newCfg, map[string]bool{"stats": true}); err != nil {
			s.logger.Error("ApplyConfigUpdate failed", "error", err)
		}
	}
	return nil
}

// Clear 清空统计数据
func (s *StatsService) Clear(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	if err := s.store.ClearStats(); err != nil {
		return err
	}
	// 同时重置内存中的统计数据，避免 flush 时写回旧数据
	if s.statCollector != nil {
		s.statCollector.Reset()
	}
	return nil
}

// Prune 清理指定天数之前的统计数据
func (s *StatsService) Prune(args *rpcapi.StatsPruneArgs, reply *rpcapi.StatsPruneReply) error {
	if args == nil || args.Days <= 0 {
		return fmt.Errorf("days must be greater than 0")
	}

	if s.statCollector != nil {
		s.statCollector.Flush()
	}

	before := time.Now().AddDate(0, 0, -args.Days).Format("2006-01-02")
	count, err := s.store.PruneStats(before)
	if err != nil {
		return err
	}
	if _, err := s.store.RecalculateStatsMeta(); err != nil {
		return err
	}
	if s.statCollector != nil {
		s.statCollector.Resume()
	}

	reply.Count = count
	reply.Before = before
	return nil
}
