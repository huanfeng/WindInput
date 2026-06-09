package store

import (
	"encoding/json"
	"fmt"
	"time"

	bolt "go.etcd.io/bbolt"
)

var (
	bucketStats     = []byte("Stats")
	bucketStatsDay  = []byte("Daily")
	bucketStatsMeta = []byte("Meta")
)

// CommitSource 上屏来源分类
type CommitSource uint8

const (
	SourceCandidate   CommitSource = iota // 候选词选择 (Space/数字)
	SourceRawInput                        // 原始编码上屏 (Enter)
	SourcePunctuation                     // 标点符号
	SourceTempEnglish                     // 临时英文
	SourceTempPinyin                      // 临时拼音
	SourceQuickInput                      // 快捷输入
	SourceFullWidth                       // 全角转换
	SourceModeSwitch                      // 模式切换时上屏
	SourceTSFDirect                       // TSF 层直接输入 (英文模式)
	SourceSpecialMode                     // 引导键特殊模式（自定义码表）
	commitSourceCount                     // 来源总数（内部使用）
)

// SchemaStats 每个方案的独立统计
type SchemaStats struct {
	TotalChars   int    `json:"tc"`
	CommitCount  int    `json:"cn"`
	CodeLenSum   int    `json:"cls"`
	CodeLenCount int    `json:"clc"`
	CandPosDist  [5]int `json:"cpd"`
}

// DailyStat 每日聚合统计
type DailyStat struct {
	Date string `json:"d"` // "2026-04-24"

	// 字符数统计
	TotalChars   int `json:"tc"` // 总上屏字符数
	ChineseChars int `json:"cc"` // 中文字符数
	EnglishChars int `json:"ec"` // 英文字符数
	PunctChars   int `json:"pc"` // 标点字符数
	OtherChars   int `json:"oc"` // 其他字符数 (数字/符号)

	// 时段分布 (按小时)
	Hours [24]int `json:"h"`

	// 上屏次数
	CommitCount int `json:"cn"`

	// 码长统计 (仅候选词上屏)
	CodeLenSum   int    `json:"cls"` // 码长总和
	CodeLenCount int    `json:"clc"` // 有效码长次数
	CodeLenDist  [6]int `json:"cld"` // 码长分布 [1码,2码,3码,4码,5码,6码+]

	// 选重统计 (仅候选词上屏)
	CandPosDist [5]int `json:"cpd"` // 候选位置分布 [首选,2选,3选,4选,5选+]

	// 速度统计
	ActiveSeconds int `json:"as"` // 活跃输入时间（秒），连续输入间隔 < 60s 视为活跃

	// 按方案分类
	BySchema map[string]*SchemaStats `json:"bs,omitempty"`

	// 按来源分类（数组长度须与 commitSourceCount 保持一致）
	BySource [commitSourceCount]int `json:"src"`
}

// StatsMeta 统计全局元数据
type StatsMeta struct {
	TotalChars    int64  `json:"total_chars"`
	FirstDay      string `json:"first_day"`
	StreakCurrent int    `json:"streak_current"`
	StreakMax     int    `json:"streak_max"`
	StreakLastDay string `json:"streak_last"`
	MaxSpeed      int    `json:"max_speed"` // 历史最快速度（字/分钟，按天计算）
}

// SpeedPerMinute returns chars/minute with a 5-second floor for active time.
func SpeedPerMinute(chars, activeSeconds int) int {
	if chars <= 0 || activeSeconds <= 0 {
		return 0
	}
	if activeSeconds < 5 {
		activeSeconds = 5
	}
	return chars * 60 / activeSeconds
}

// NewDailyStat 创建指定日期的空 DailyStat
func NewDailyStat(date string) *DailyStat {
	return &DailyStat{
		Date:     date,
		BySchema: make(map[string]*SchemaStats),
	}
}

// GetDailyStat 获取指定日期的统计
func (s *Store) GetDailyStat(date string) (*DailyStat, error) {
	var stat *DailyStat
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return nil
		}
		daily := b.Bucket(bucketStatsDay)
		if daily == nil {
			return nil
		}
		v := daily.Get([]byte(date))
		if v == nil {
			return nil
		}
		stat = &DailyStat{}
		return json.Unmarshal(v, stat)
	})
	return stat, err
}

// GetDailyStats 获取日期范围内的统计 (含首尾，key 按字典序)
func (s *Store) GetDailyStats(from, to string) ([]*DailyStat, error) {
	var results []*DailyStat
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return nil
		}
		daily := b.Bucket(bucketStatsDay)
		if daily == nil {
			return nil
		}
		c := daily.Cursor()
		for k, v := c.Seek([]byte(from)); k != nil; k, v = c.Next() {
			if string(k) > to {
				break
			}
			var stat DailyStat
			if err := json.Unmarshal(v, &stat); err != nil {
				continue
			}
			results = append(results, &stat)
		}
		return nil
	})
	return results, err
}

// PutDailyStat 写入一条每日统计
func (s *Store) PutDailyStat(stat *DailyStat) error {
	data, err := json.Marshal(stat)
	if err != nil {
		return fmt.Errorf("marshal DailyStat: %w", err)
	}
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return fmt.Errorf("Stats bucket not found")
		}
		daily := b.Bucket(bucketStatsDay)
		if daily == nil {
			return fmt.Errorf("Stats/Daily bucket not found")
		}
		return daily.Put([]byte(stat.Date), data)
	})
}

// GetStatsMeta 获取全局元数据
func (s *Store) GetStatsMeta() (*StatsMeta, error) {
	var meta StatsMeta
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return nil
		}
		mb := b.Bucket(bucketStatsMeta)
		if mb == nil {
			return nil
		}
		v := mb.Get([]byte("meta"))
		if v == nil {
			return nil
		}
		return json.Unmarshal(v, &meta)
	})
	return &meta, err
}

// PutStatsMeta 写入全局元数据
func (s *Store) PutStatsMeta(meta *StatsMeta) error {
	data, err := json.Marshal(meta)
	if err != nil {
		return fmt.Errorf("marshal StatsMeta: %w", err)
	}
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return fmt.Errorf("Stats bucket not found")
		}
		mb := b.Bucket(bucketStatsMeta)
		if mb == nil {
			return fmt.Errorf("Stats/Meta bucket not found")
		}
		return mb.Put([]byte("meta"), data)
	})
}

// PruneStats 清理指定日期之前的统计数据，返回删除条数
func (s *Store) PruneStats(before string) (int, error) {
	var count int
	err := s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return nil
		}
		daily := b.Bucket(bucketStatsDay)
		if daily == nil {
			return nil
		}
		c := daily.Cursor()
		for k, _ := c.First(); k != nil; k, _ = c.Next() {
			if string(k) >= before {
				break
			}
			if err := daily.Delete(k); err != nil {
				return fmt.Errorf("delete %s: %w", k, err)
			}
			count++
		}
		return nil
	})
	return count, err
}

// RecalculateStatsMeta rebuilds global stats metadata from remaining daily rows.
func (s *Store) RecalculateStatsMeta() (*StatsMeta, error) {
	meta := &StatsMeta{}
	var dates []string
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return nil
		}
		daily := b.Bucket(bucketStatsDay)
		if daily == nil {
			return nil
		}
		c := daily.Cursor()
		for k, v := c.First(); k != nil; k, v = c.Next() {
			var stat DailyStat
			if err := json.Unmarshal(v, &stat); err != nil {
				continue
			}
			date := string(k)
			dates = append(dates, date)
			if meta.FirstDay == "" {
				meta.FirstDay = date
			}
			meta.TotalChars += int64(stat.TotalChars)
			if speed := SpeedPerMinute(stat.TotalChars, stat.ActiveSeconds); speed > meta.MaxSpeed {
				meta.MaxSpeed = speed
			}
		}
		return nil
	})
	if err != nil {
		return nil, err
	}

	meta.StreakCurrent, meta.StreakMax, meta.StreakLastDay = calculateStreaks(dates)
	if err := s.PutStatsMeta(meta); err != nil {
		return nil, err
	}
	return meta, nil
}

func calculateStreaks(dates []string) (current, max int, lastDay string) {
	var prev time.Time
	for _, date := range dates {
		day, err := time.Parse("2006-01-02", date)
		if err != nil {
			continue
		}
		if lastDay == "" {
			current = 1
			max = 1
		} else if day.Sub(prev).Hours()/24 <= 1.5 {
			current++
		} else {
			current = 1
		}
		if current > max {
			max = current
		}
		prev = day
		lastDay = date
	}
	return current, max, lastDay
}

// ClearStats 清空所有统计数据
func (s *Store) ClearStats() error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return nil
		}
		// 删除并重建 Daily
		if b.Bucket(bucketStatsDay) != nil {
			if err := b.DeleteBucket(bucketStatsDay); err != nil {
				return err
			}
		}
		if _, err := b.CreateBucket(bucketStatsDay); err != nil {
			return err
		}
		// 删除并重建 Meta
		if b.Bucket(bucketStatsMeta) != nil {
			if err := b.DeleteBucket(bucketStatsMeta); err != nil {
				return err
			}
		}
		if _, err := b.CreateBucket(bucketStatsMeta); err != nil {
			return err
		}
		return nil
	})
}

// CountStatsDays 返回统计数据的天数
func (s *Store) CountStatsDays() (int, error) {
	var count int
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketStats)
		if b == nil {
			return nil
		}
		daily := b.Bucket(bucketStatsDay)
		if daily == nil {
			return nil
		}
		count = daily.Stats().KeyN
		return nil
	})
	return count, err
}
