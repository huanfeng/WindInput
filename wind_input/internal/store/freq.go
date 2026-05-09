package store

import (
	"bytes"
	"encoding/json"
	"fmt"
	"math"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
)

const (
	freqFlushSize     = 50
	freqFlushInterval = 30 * time.Second
)

const FreqBoostMax = 2000

var bucketFreq = []byte("Freq")

// FreqRecord holds per-candidate frequency data for a given (code, text) pair.
type FreqRecord struct {
	Count    uint32 `json:"c"`
	LastUsed int64  `json:"t"`
	Streak   uint8  `json:"s,omitempty"`
}

// freqKey returns the composite bucket key for a (code, text) pair.
func freqKey(code, text string) string {
	return code + ":" + text
}

// GetFreq reads the FreqRecord for (code, text) under the given schema.
// Returns a zero FreqRecord (Count==0) if the key does not exist yet.
func (s *Store) GetFreq(schemaID, code, text string) (FreqRecord, error) {
	var rec FreqRecord
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			// Bucket not yet created → treat as empty.
			return nil
		}
		v := b.Get([]byte(freqKey(code, text)))
		if v == nil {
			return nil
		}
		return json.Unmarshal(v, &rec)
	})
	return rec, err
}

// IncrementFreq increments Count by 1, updates LastUsed to now (Unix seconds),
// and increments Streak (capped at 255) for the given (code, text) pair.
func (s *Store) IncrementFreq(schemaID, code, text string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), true)
		if err != nil {
			return fmt.Errorf("IncrementFreq: %w", err)
		}
		key := []byte(freqKey(code, text))
		var rec FreqRecord
		if v := b.Get(key); v != nil {
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("IncrementFreq unmarshal: %w", err)
			}
		}
		rec.Count++
		rec.LastUsed = time.Now().Unix()
		if rec.Streak < 255 {
			rec.Streak++
		}
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("IncrementFreq marshal: %w", err)
		}
		return b.Put(key, data)
	})
}

// ResetStreak sets Streak to 0 for the given (code, text) pair.
// If the record does not exist, this is a no-op.
func (s *Store) ResetStreak(schemaID, code, text string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			// Bucket not yet created → nothing to reset.
			return nil
		}
		key := []byte(freqKey(code, text))
		v := b.Get(key)
		if v == nil {
			return nil
		}
		var rec FreqRecord
		if err := json.Unmarshal(v, &rec); err != nil {
			return fmt.Errorf("ResetStreak unmarshal: %w", err)
		}
		if rec.Streak == 0 {
			return nil
		}
		rec.Streak = 0
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("ResetStreak marshal: %w", err)
		}
		return b.Put(key, data)
	})
}

// FreqEntry holds a frequency record with its parsed code and text.
type FreqEntry struct {
	Code   string
	Text   string
	Record FreqRecord
}

// SearchFreqPrefix returns freq entries whose key starts with the given prefix.
// If prefix is empty, returns all entries. Results are limited by limit (0 = unlimited).
func (s *Store) SearchFreqPrefix(schemaID, prefix string, limit int) ([]FreqEntry, error) {
	var results []FreqEntry
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			return nil
		}
		c := b.Cursor()
		var k, v []byte
		if prefix == "" {
			k, v = c.First()
		} else {
			k, v = c.Seek([]byte(prefix))
		}
		pfx := []byte(prefix)
		for ; k != nil; k, v = c.Next() {
			if prefix != "" && !bytes.HasPrefix(k, pfx) {
				break
			}
			parts := strings.SplitN(string(k), ":", 2)
			if len(parts) != 2 {
				continue
			}
			var rec FreqRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("SearchFreqPrefix unmarshal key %q: %w", k, err)
			}
			results = append(results, FreqEntry{
				Code:   parts[0],
				Text:   parts[1],
				Record: rec,
			})
			if limit > 0 && len(results) >= limit {
				break
			}
		}
		return nil
	})
	return results, err
}

// PutFreq sets a FreqRecord directly for the given (code, text) pair.
func (s *Store) PutFreq(schemaID, code, text string, rec FreqRecord) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), true)
		if err != nil {
			return fmt.Errorf("PutFreq: %w", err)
		}
		data, err := json.Marshal(rec)
		if err != nil {
			return fmt.Errorf("PutFreq marshal: %w", err)
		}
		return b.Put([]byte(freqKey(code, text)), data)
	})
}

// DeleteFreq removes a single frequency record.
func (s *Store) DeleteFreq(schemaID, code, text string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			return nil
		}
		return b.Delete([]byte(freqKey(code, text)))
	})
}

// ClearAllFreq removes all frequency data for a schema by deleting and
// recreating the Freq sub-bucket. Returns the number of entries removed.
func (s *Store) ClearAllFreq(schemaID string) (int, error) {
	var count int
	err := s.db.Update(func(tx *bolt.Tx) error {
		parent, err := schemaBucket(tx, schemaID, true)
		if err != nil {
			return fmt.Errorf("ClearAllFreq: %w", err)
		}
		fb := parent.Bucket(bucketFreq)
		if fb == nil {
			return nil
		}
		count = fb.Stats().KeyN
		if err := parent.DeleteBucket(bucketFreq); err != nil {
			return fmt.Errorf("ClearAllFreq delete: %w", err)
		}
		if _, err := parent.CreateBucket(bucketFreq); err != nil {
			return fmt.Errorf("ClearAllFreq recreate: %w", err)
		}
		return nil
	})
	return count, err
}

// GetAllFreq returns all FreqRecords for the given schema, keyed by "code:text".
func (s *Store) GetAllFreq(schemaID string) (map[string]FreqRecord, error) {
	result := make(map[string]FreqRecord)
	err := s.db.View(func(tx *bolt.Tx) error {
		b, err := schemaSubBucket(tx, schemaID, string(bucketFreq), false)
		if err != nil {
			// Bucket not yet created → return empty map.
			return nil
		}
		return b.ForEach(func(k, v []byte) error {
			var rec FreqRecord
			if err := json.Unmarshal(v, &rec); err != nil {
				return fmt.Errorf("GetAllFreq unmarshal key %q: %w", k, err)
			}
			result[string(k)] = rec
			return nil
		})
	})
	return result, err
}

// FreqProfile 词频评分参数（每方案可配）
type FreqProfile struct {
	BoostMax      int     `json:"boost_max" yaml:"boost_max"`       // 加成上限（默认 2000）
	BaseScale     float64 `json:"base_scale" yaml:"base_scale"`     // base = log2(count+1) * BaseScale（默认 100）
	MaxRecency    float64 `json:"max_recency" yaml:"max_recency"`   // 时间衰减峰值（默认 300）
	DecayHalfLife float64 `json:"half_life" yaml:"half_life"`       // 半衰期（小时，默认 72）
	StreakScale   float64 `json:"streak_scale" yaml:"streak_scale"` // 连续选择系数（默认 50）
	StreakCap     float64 `json:"streak_cap" yaml:"streak_cap"`     // 连续选择上限（默认 250）
}

// DefaultFreqProfile 返回默认词频评分参数
// 参数设置偏保守，避免少量选择就大幅改变排序
func DefaultFreqProfile() *FreqProfile {
	return &FreqProfile{
		BoostMax:      FreqBoostMax,
		BaseScale:     50,
		MaxRecency:    100,
		DecayHalfLife: 72, // 3 天半衰期
		StreakScale:   30,
		StreakCap:     150,
	}
}

// CalcFreqBoost computes a priority boost score using default profile.
// Kept for backward compatibility.
func CalcFreqBoost(rec FreqRecord, now int64) int {
	return CalcFreqBoostWithProfile(rec, now, nil)
}

// CalcFreqBoostWithProfile computes a priority boost score for the given FreqRecord.
//
// Scoring:
//   - base    = log2(count+1) * BaseScale
//   - recency = MaxRecency * exp(-λ * ageHours), λ = ln(2) / DecayHalfLife
//   - streak  = min(streak * StreakScale, StreakCap)
//   - total capped at BoostMax
//   - returns 0 if Count == 0
func CalcFreqBoostWithProfile(rec FreqRecord, now int64, p *FreqProfile) int {
	if rec.Count == 0 {
		return 0
	}
	if p == nil {
		p = DefaultFreqProfile()
	}

	// base: 对数增长，避免高频词过度主导
	base := math.Log2(float64(rec.Count)+1) * p.BaseScale

	// recency: 连续指数衰减
	ageHours := float64(now-rec.LastUsed) / 3600.0
	if ageHours < 0 {
		ageHours = 0
	}
	lambda := math.Ln2 / p.DecayHalfLife
	recency := p.MaxRecency * math.Exp(-lambda*ageHours)

	// streak: 连续选择加成
	streak := float64(rec.Streak) * p.StreakScale
	if streak > p.StreakCap {
		streak = p.StreakCap
	}

	total := base + recency + streak
	if total > float64(p.BoostMax) {
		total = float64(p.BoostMax)
	}
	return int(total)
}

// BatchIncrementFreq enqueues a freq update via the WriteBuffer.
// The caller supplies the current rec (e.g. from GetFreq) so we can
// pre-compute the next state without a separate read transaction.
func BatchIncrementFreq(wb *WriteBuffer, schemaID, code, text string, rec FreqRecord) {
	rec.Count++
	rec.LastUsed = time.Now().Unix()
	if rec.Streak < 255 {
		rec.Streak++
	}
	data, err := json.Marshal(rec)
	if err != nil {
		return
	}
	wb.Enqueue(WriteOp{
		Bucket: [][]byte{bucketSchemas, []byte(schemaID), bucketFreq},
		Key:    freqKey(code, text),
		Value:  data,
	})
}

// IncrementFreqAsync 异步增加词频计数（内存累积，批量写入）。
// 与 IncrementFreq 的区别：不立即写 BoltDB，大幅减少写锁竞争。
// 崩溃时最多丢失最近一个 flush 周期内的增量，对词频排序可接受。
func (s *Store) IncrementFreqAsync(schemaID, code, text string) {
	key := freqDeltaKey{schemaID: schemaID, code: code, text: text}
	s.freqMu.Lock()
	s.freqDeltas[key]++
	trigger := len(s.freqDeltas) >= freqFlushSize
	s.freqMu.Unlock()
	if trigger {
		select {
		case s.freqFlushCh <- struct{}{}:
		default:
		}
	}
}

// freqFlushLoop 后台定时或按量 flush 词频增量
func (s *Store) freqFlushLoop() {
	defer s.freqWg.Done()
	ticker := time.NewTicker(freqFlushInterval)
	defer ticker.Stop()
	for {
		select {
		case <-s.freqDone:
			_ = s.flushFreqDeltas()
			return
		case <-s.freqFlushCh:
			_ = s.flushFreqDeltas()
		case <-ticker.C:
			_ = s.flushFreqDeltas()
		}
	}
}

// flushFreqDeltas 将内存中的词频增量写入 BoltDB（一次事务）
func (s *Store) flushFreqDeltas() error {
	s.freqMu.Lock()
	if len(s.freqDeltas) == 0 {
		s.freqMu.Unlock()
		return nil
	}
	deltas := s.freqDeltas
	s.freqDeltas = make(map[freqDeltaKey]int)
	s.freqMu.Unlock()

	if s.db == nil {
		// 暂停状态：将增量放回，等 Resume 后下次 flush 再处理
		s.freqMu.Lock()
		for k, v := range deltas {
			s.freqDeltas[k] += v
		}
		s.freqMu.Unlock()
		return nil
	}

	return s.db.Update(func(tx *bolt.Tx) error {
		for key, delta := range deltas {
			b, err := schemaSubBucket(tx, key.schemaID, string(bucketFreq), true)
			if err != nil {
				return err
			}
			dbKey := []byte(freqKey(key.code, key.text))
			var rec FreqRecord
			if v := b.Get(dbKey); v != nil {
				_ = json.Unmarshal(v, &rec)
			}
			rec.Count += uint32(delta)
			rec.LastUsed = time.Now().Unix()
			newStreak := int(rec.Streak) + delta
			if newStreak > 255 {
				newStreak = 255
			}
			rec.Streak = uint8(newStreak)
			data, err := json.Marshal(rec)
			if err != nil {
				return err
			}
			if err := b.Put(dbKey, data); err != nil {
				return err
			}
		}
		return nil
	})
}
